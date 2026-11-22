use std::collections::HashMap;
use std::sync::LazyLock;

use serde_json::json;

use crate::clear_special::read_scalar_with;
use crate::errors::{MAX_ENTRIES, Result, capacity_error};
use crate::js::{self, Caps, JsString, Lit, Re};
use crate::protect::blocks::CheckBudget;
use crate::protect::placeholders::{CLOSE, CLOSE_UNIT, OPEN, OPEN_UNIT, PLACEHOLDER_TOKEN};
use crate::protect::tag_spans::{COMMENT_OPEN, Span, inside_tag, named_at, tag_spans};
use crate::rules::quote_state::{QuoteTags, could_open, digit_before_quote};
use crate::shared::block_tags::{BLOCK_TAG_NAMES, DEFAULT_SAFE_TAGS, VOID_BLOCK_TAG_NAMES, VOID_TAG_NAMES};
use crate::shared::boundaries::{
    dash_quote_punctuation, inside_quote_word, is_quote_dash, opening_quote_neighbours, opens_glued_cyrillic,
};
use crate::shared::quote_pairs::{LDQUO_U, PairSlot, QUOT_U, RAQUO_U, RDQUO_U, matching_quote, pair_authored_quotes};
use crate::shared::url_email_regex::{
    EMAIL_REGEX, IPV6_REGEX, URL_REGEX, URL_START_REGEX, URL_UNBRACKETED_START_REGEX, UUID_REGEX,
};
use crate::{cat, lit};

fn sequence_hint(text: &[u16]) -> bool {
    text.iter().any(|&c| matches!(c, 0x2E | 0x3A | 0x40 | 0x2D))
}

static BLOCK_TOKEN_RE: LazyLock<Re> = LazyLock::new(|| Re::new(&PLACEHOLDER_TOKEN, "gu"));
static ADDRESS_PLACEHOLDER_RE: LazyLock<Re> =
    LazyLock::new(|| Re::new(&format!(r"(?:http:\/\/)?{URL_CORE_SOURCE}"), "g"));
static URL_CORE_HEAD: Lit = lit!("simonenko-xyz/A0SAFE");

static URL_RESTORE_RE: LazyLock<Re> =
    LazyLock::new(|| Re::new(r"http:\/\/simonenko-xyz\/A0SAFE([0-9A-F]+)NUM(\d+)ID", "gms"));
static URL_RESTORE_NOPROTO_RE: LazyLock<Re> =
    LazyLock::new(|| Re::new(r"simonenko-xyz\/A0SAFE([0-9A-F]+)NUM(\d+)ID", "gms"));
static EMAIL_RESTORE_RE: LazyLock<Re> = LazyLock::new(|| Re::new(r"A1SAFE([0-9A-F]+)NUM(\d+)ID@simonenko\.xyz", "gms"));
static TOKEN_RESTORE_RE: LazyLock<Re> = LazyLock::new(|| Re::new(r"AzSAFE([0-9A-F]+)TOK(\d+)TK", "gms"));
static SCHEME_RE: LazyLock<Re> = LazyLock::new(|| Re::new(r"^[^:]+:\/\/", ""));
static HREF_SCHEME_RE: LazyLock<Re> = LazyLock::new(|| Re::new(r"^(?:https?|ftps?|mailto):", "i"));
static BARE_SCHEME_RE: LazyLock<Re> = LazyLock::new(|| Re::new(r"^(?:https?|ftps?|mailto):\/*$", "i"));
static REFERENCE_RE: LazyLock<Re> =
    LazyLock::new(|| Re::new(r"&(?:(amp|lt|gt|quot|apos)|#([0-9]{1,7})|#[xX]([0-9A-Fa-f]{1,6}));", "g"));
static WHOLE_RE: LazyLock<Re> = LazyLock::new(|| {
    Re::new(
        &format!(
            r"(http:\/\/|<{OPEN}A\d+{CLOSE}>)?simonenko-xyz\/A0SAFE([0-9A-F]+)NUM\d+ID|A1SAFE([0-9A-F]+)NUM\d+ID@simonenko\.xyz|AzSAFE([0-9A-F]+)TOK\d+TK"
        ),
        "g",
    )
});
static SEALED_RE: LazyLock<Re> = LazyLock::new(|| Re::new(&format!(r"{OPEN}S(\d+){CLOSE}"), "g"));

pub const URL_CORE_SOURCE: &str = r"simonenko-xyz\/A0SAFE[0-9A-F]+NUM\d+ID";

pub static LINKABLE_SOURCE: LazyLock<String> =
    LazyLock::new(|| format!(r"http:\/\/{URL_CORE_SOURCE}|A1SAFE[0-9A-F]+NUM\d+ID@simonenko\.xyz"));

static ADDRESS_LINK_RE: LazyLock<Re> =
    LazyLock::new(|| Re::new(r#"<a href="((?:https?|ftps?|mailto):\/\/)([^\s<>"']{4,})">([^\s<>"']{4,})<\/a>"#, "giu"));

static HTTP: Lit = lit!("http://");
const MARKER: &str = "SAFE";

type Census = [usize; 4];
type MembersMayOpen<'a> = dyn Fn(&[AddressQuote], usize, Option<u16>) -> bool + 'a;

pub struct Sealed {
    pub text: JsString,
    values: Vec<JsString>,
    found: Census,
    pub edits: Option<Vec<crate::shared::provenance::MapEdit>>,
}

fn holds(found: &Census, now: &Census) -> bool {
    now[0] >= found[0] && now[1] <= found[1] && now[2] >= found[2] && now[3] >= found[3]
}

fn scan_while(text: &[u16], mut at: usize, accepts: impl Fn(u16) -> bool) -> Option<usize> {
    while at < text.len() {
        let end = (at + (1 << 16)).min(text.len());
        let (blocks, tail) = text[at..end].as_chunks::<16>();
        for (i, block) in blocks.iter().enumerate() {
            let stopped = block.iter().enumerate().fold(0u16, |bits, (k, &u)| bits | (u16::from(!accepts(u)) << k));
            if stopped != 0 {
                return Some(at + i * 16 + stopped.trailing_zeros() as usize);
            }
        }
        if let Some(offset) = tail.iter().position(|&u| !accepts(u)) {
            return Some(end - tail.len() + offset);
        }
        if end - at == 1 << 16 && js::regex::scan_out_of_budget() {
            return None;
        }
        at = end;
    }
    Some(at)
}

fn census(text: &[u16], nonce: &[u8]) -> Census {
    static URL_HEAD: Lit = lit!("simonenko-xyz/A0");
    static EMAIL_HEAD: Lit = lit!("A1");
    static TOKEN_HEAD: Lit = lit!("Az");
    static EMAIL_END: Lit = lit!("ID@simonenko.xyz");
    const SAFE: &[u16] = &[0x53, 0x41, 0x46, 0x45];
    const NUM: &[u16] = &[0x4E, 0x55, 0x4D];
    const TOK: &[u16] = &[0x54, 0x4F, 0x4B];
    const ID: &[u16] = &[0x49, 0x44];
    const TK: &[u16] = &[0x54, 0x4B];
    const LINK: &[u16] = &[0x3C, OPEN_UNIT, 0x41];
    let mut counts = [0; 4];
    if nonce.is_empty() {
        return counts;
    }
    let digit = |u: u16| (0x30..=0x39).contains(&u);
    let mut at = 0;
    let mut next_check = 1 << 16;
    while let Some(safe) = scan_while(text, at, |u| u != 0x53) {
        if safe == text.len() {
            break;
        }
        if safe >= next_check {
            if js::regex::scan_out_of_budget() {
                break;
            }
            next_check = safe + (1 << 16);
        }
        at = safe + 1;
        if !text[safe..].starts_with(SAFE) {
            continue;
        }
        let (kind, start, number, end) = if text[..safe].ends_with(&URL_HEAD) {
            (0, safe - URL_HEAD.len(), NUM, ID)
        } else if text[..safe].ends_with(&EMAIL_HEAD) {
            (2, safe - EMAIL_HEAD.len(), NUM, EMAIL_END.as_slice())
        } else if text[..safe].ends_with(&TOKEN_HEAD) {
            (3, safe - TOKEN_HEAD.len(), TOK, TK)
        } else {
            continue;
        };
        let tail = &text[safe + SAFE.len()..];
        if tail.len() < nonce.len() || !tail.iter().zip(nonce).all(|(&u, &b)| u == u16::from(b)) {
            continue;
        }
        let tail = &tail[nonce.len()..];
        if !tail.starts_with(number) {
            continue;
        }
        let tail = &tail[number.len()..];
        let Some(digits) = scan_while(tail, 0, digit) else { break };
        if digits == 0 || !tail[digits..].starts_with(end) {
            continue;
        }
        counts[kind] += 1;
        if kind == 0 {
            let before = &text[..start];
            let mut linked = before.ends_with(&HTTP);
            if !linked && before.ends_with(&[CLOSE_UNIT, 0x3E]) {
                let shut = before.len() - 2;
                let mut open = shut;
                while open > 0 && digit(before[open - 1]) {
                    open -= 1;
                    if (shut - open) & 0xFFFF == 0 && js::regex::scan_out_of_budget() {
                        return counts;
                    }
                }
                linked = open < shut && before[..open].ends_with(LINK);
            }
            counts[1] += usize::from(!linked);
        }
        at = text.len() - tail.len() + digits + end.len();
    }
    counts
}

fn decode_references(address: &[u16]) -> JsString {
    REFERENCE_RE.replace_with(address, |c| {
        if c.has(1) {
            let named = match js::lossy(c.s(1)).as_str() {
                "amp" => '&',
                "lt" => '<',
                "gt" => '>',
                "quot" => '"',
                _ => '\'',
            };
            return vec![named as u16];
        }
        let code = if c.has(2) {
            js::lossy(c.s(2)).parse::<u32>().unwrap_or(0)
        } else {
            u32::from_str_radix(&js::lossy(c.s(3)), 16).unwrap_or(0)
        };
        if code == 0 || code > 0x10_FFFF || (0xD800..=0xDFFF).contains(&code) {
            vec![0xFFFD]
        } else {
            js::from_code_point(code)
        }
    })
}

pub static RESTORE_REGEXES: LazyLock<Vec<Re>> = LazyLock::new(|| {
    vec![URL_RESTORE_RE.clone(), URL_RESTORE_NOPROTO_RE.clone(), EMAIL_RESTORE_RE.clone(), TOKEN_RESTORE_RE.clone()]
});

#[derive(Clone, Copy)]
enum Kind {
    Url,
    Email,
    Token,
}

impl Kind {
    fn letter(self) -> u16 {
        match self {
            Kind::Url => 0x75,
            Kind::Email => 0x65,
            Kind::Token => 0x74,
        }
    }

    fn name(self) -> (&'static str, &'static str) {
        match self {
            Kind::Url => ("URLs", "url"),
            Kind::Email => ("emails", "email"),
            Kind::Token => ("tokens", "token"),
        }
    }
}

struct EntropyPool {
    fill: fn(&mut [u8]),
    bytes: [u8; 256],
    used: usize,
}

impl EntropyPool {
    fn new(fill: fn(&mut [u8])) -> Self {
        EntropyPool { fill, bytes: [0; 256], used: 256 }
    }

    fn nonce(&mut self) -> String {
        const HEX: &[u8; 16] = b"0123456789ABCDEF";
        if self.used == self.bytes.len() {
            (self.fill)(&mut self.bytes);
            self.used = 0;
        }
        let drawn = &mut self.bytes[self.used..self.used + 8];
        self.used += 8;
        let nonce =
            drawn.iter().flat_map(|b| [HEX[usize::from(b >> 4)] as char, HEX[usize::from(b & 15)] as char]).collect();
        drawn.fill(0);
        nonce
    }
}

impl std::fmt::Debug for EntropyPool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EntropyPool").field("used", &self.used).finish_non_exhaustive()
    }
}

fn trim_url_parens(url: &[u16]) -> &[u16] {
    let open = url.iter().filter(|&&c| c == 0x28).count();
    let mut close = url.iter().filter(|&&c| c == 0x29).count();
    let mut end = url.len();
    while end > 0 && url[end - 1] == 0x29 && close > open {
        close -= 1;
        end -= 1;
    }
    &url[..end]
}

fn url_in(m: &[u16]) -> &[u16] {
    let url = trim_url_parens(m);
    if url.len() == m.len() {
        return url;
    }
    let mut end = URL_UNBRACKETED_START_REGEX.exec_at(url, 0).map_or(0, |c| c.whole().len());
    let mut open = 0isize;
    for (i, &c) in url.iter().enumerate() {
        if c == 0x28 {
            open += 1;
        } else if c == 0x29 {
            open -= 1;
            if open >= 0 && i >= end {
                end = i + 1;
            }
        }
    }
    &url[..end]
}

const MDASH_U: u16 = 0x2014;

static QUOTE_CONTEXT: LazyLock<Re> =
    LazyLock::new(|| Re::new("[\"«»„“”&<\u{2029}]|(?:\\r\\n|\\r(?!\\n)|\\n)[ \\t]*[\\r\\n]", "gu"));
static BLOCK_TAG: LazyLock<Re> = LazyLock::new(|| {
    let names: Vec<&str> = BLOCK_TAG_NAMES.iter().chain(VOID_BLOCK_TAG_NAMES).copied().collect();
    Re::new(&format!(r"^<\/?(?:p|{})(?=[\s/>])", names.join("|")), "iu")
});
static NON_PROSE_TAG: LazyLock<Re> = LazyLock::new(|| {
    let names: Vec<&str> = BLOCK_TAG_NAMES
        .iter()
        .chain(VOID_TAG_NAMES)
        .chain(DEFAULT_SAFE_TAGS.iter())
        .chain(&["xmp", "noembed", "title"])
        .copied()
        .collect();
    Re::new(&format!(r"^</?(?:p|{})(?=[\s/>])", names.join("|")), "iu")
});
static TAG_START: LazyLock<Re> = LazyLock::new(|| Re::new(r"^<\/?[a-z]", "iu"));

fn prose_inline_tag(source: &[u16], span: &Span) -> bool {
    let raw = js::slice(source, span.start, span.end + 1);
    raw.starts_with(COMMENT_OPEN) || (TAG_START.test(raw) && !NON_PROSE_TAG.test(raw))
}

#[derive(Debug, Default)]
struct AddressQuote {
    at: usize,
    end: usize,
    glyph: Option<u16>,
    after_dash: bool,
    after_dash_boundary: bool,
    internal: bool,
    closer: Option<u16>,
    partner: Option<usize>,
    in_address: bool,
    run: Option<usize>,
    remaining: usize,
    before_address: bool,
    measurement: bool,
    measured_value: bool,
    close_only: bool,
    later_closers: usize,
    alternative: Option<usize>,
    outside: Option<Vec<usize>>,
    address_end: bool,
    speech_end: bool,
    speech_start: bool,
    shared_end: Option<usize>,
}

#[derive(Debug)]
struct Run {
    end: usize,
    length: usize,
}

fn address_quote_tokens(source: &[u16], tags: &[Span]) -> Vec<AddressQuote> {
    let mut tokens = Vec::new();
    let mut tag = 0;
    let mut dash_end = None;
    let mut follow = 0;
    let mut next_inline = |at: usize| -> Option<usize> {
        inside_tag(source, tags, &mut follow, at);
        let Some(span) = tags.get(follow).filter(|s| s.start == at) else { return Some(at) };
        if named_at(source, at + 1, "br") || BLOCK_TAG.test(js::slice(source, at, span.end + 1)) {
            return None;
        }
        Some(if prose_inline_tag(source, span) { span.end + 1 } else { at })
    };
    for m in QUOTE_CONTEXT.find_all(source) {
        let at = m.index();
        if inside_tag(source, tags, &mut tag, at) {
            continue;
        }
        let whole = m.whole();
        if whole == [0x3C] {
            let Some(span) = tags.get(tag).filter(|s| s.start == at) else { continue };
            if !BLOCK_TAG.test(js::slice(source, at, span.end + 1)) {
                continue;
            }
        }
        if whole == [0x3C] || whole == [0x2029] || whole.len() > 1 {
            tokens.push(AddressQuote { at, end: at + whole.len(), ..AddressQuote::default() });
            continue;
        }
        let (glyph, end) = read_scalar_with(source, at, true);
        let glyph = (glyph.len() == 1).then(|| glyph[0]);
        if is_quote_dash(glyph) {
            dash_end = Some(end);
        } else if let Some(g) = glyph
            && (matching_quote(g).is_some() || g == QUOT_U || g == RAQUO_U || g == RDQUO_U)
        {
            let mut before = at;
            let mut j = tag;
            while j > 0 && tags[j - 1].end + 1 == before && prose_inline_tag(source, &tags[j - 1]) {
                before = tags[j - 1].start;
                j -= 1;
            }
            let after_dash = dash_end == Some(before) || is_quote_dash(before.checked_sub(1).map(|b| source[b]));
            let after_dash_boundary = after_dash
                && (source.get(end).is_none_or(|&c| js::is_js_space(c))
                    || dash_quote_punctuation(source, end, Some(MDASH_U), Some(&mut next_inline)));
            tokens.push(AddressQuote { at, end, glyph, after_dash, after_dash_boundary, ..AddressQuote::default() });
        }
    }
    tokens
}

fn pair_members(
    tokens: &mut [AddressQuote],
    members: &[usize],
    may_open: &MembersMayOpen<'_>,
    within_pair: bool,
    visible_roles: bool,
) {
    let mut view: Vec<PairSlot> = members
        .iter()
        .map(|&k| PairSlot {
            glyph: tokens[k].glyph.unwrap_or_default(),
            close_only: tokens[k].close_only,
            closer: tokens[k].closer,
            partner: None,
            outer: None,
        })
        .collect();
    {
        let tokens: &[AddressQuote] = tokens;
        let opens =
            |v: &[PairSlot], index: usize| may_open(tokens, index, index.checked_sub(1).and_then(|p| v[p].closer));
        let mut open = |v: &[PairSlot], index: usize, _: bool| Ok(opens(v, index));
        let mut inner = |v: &[PairSlot], index: usize| Ok(!tokens[members[index]].in_address && opens(v, index));
        pair_authored_quotes(&mut view, &mut open, within_pair.then_some(&mut inner), visible_roles, None)
            .expect("unbudgeted pairing");
    }
    for (slot, &k) in view.iter().zip(members) {
        tokens[k].closer = slot.closer;
        tokens[k].partner = slot.partner.map(|p| members[p]);
    }
}

fn closes_speech(
    tokens: &[AddressQuote],
    runs: &[Run],
    opener: Option<usize>,
    k: usize,
    unpaired: usize,
    open_count: usize,
) -> bool {
    let Some(opener) = opener.map(|o| &tokens[o]) else { return false };
    let token = &tokens[k];
    let token_partner = token.partner.map(|p| &tokens[p]);
    if token.internal && token.run.is_some() && token.run == token_partner.and_then(|p| p.run) {
        return token.glyph == Some(QUOT_U)
            && token.remaining.cast_signed() <= open_count.cast_signed() - token.later_closers.cast_signed();
    }
    let partner = opener.partner.map(|p| &tokens[p]);
    let numeric_alternative =
        !token.internal && token.run.is_some() && partner.is_some_and(|p| p.measured_value && p.alternative.is_none());
    if token.in_address
        && !numeric_alternative
        && partner.is_some_and(|p| p.at > token.at)
        && (unpaired == 0
            || (token.run.is_none() && partner.and_then(|p| p.run).is_some_and(|r| runs[r].length > unpaired)))
    {
        return false;
    }
    if opener.closer == token.glyph {
        return true;
    }
    (opener.glyph == Some(QUOT_U) || token.glyph == Some(QUOT_U))
        && !token.internal
        && (token.closer.is_none() || token.glyph == Some(QUOT_U))
        && token.run.is_some()
        && (numeric_alternative || partner.is_none() || (unpaired > 0 && partner.and_then(|p| p.run) == token.run))
}

fn reserve_prose_closers(tokens: &mut [AddressQuote], speech: &[usize]) {
    let mut available: Vec<usize> = Vec::new();
    let mut later_closers = 0usize;
    for &k in speech.iter().rev() {
        if tokens[k].glyph.is_none() {
            available.clear();
            later_closers = 0;
            continue;
        }
        if tokens[k].in_address {
            tokens[k].later_closers = later_closers;
        } else if tokens[k].closer.is_none() {
            later_closers += 1;
        } else {
            later_closers = later_closers.saturating_sub(1);
        }
        if tokens[k].internal || (tokens[k].in_address && tokens[k].run.is_none()) {
            continue;
        }
        if tokens[k].closer.is_some() {
            available = tokens[k].outside.clone().unwrap_or_default();
        } else if let Some(opener) = tokens[k].partner {
            let later = available.last().and_then(|&a| tokens[a].glyph);
            let token = &tokens[k];
            if (token.measurement && !available.is_empty())
                || (token.in_address
                    && (later == token.glyph || (tokens[opener].glyph == Some(QUOT_U) && later == Some(RAQUO_U))))
            {
                tokens[k].alternative = available.pop();
            }
            tokens[opener].outside = Some(std::mem::take(&mut available));
        } else if tokens[k].in_address {
            available.clear();
        } else if !tokens[k].measurement {
            available.push(k);
        }
    }
}

struct QuotedAddresses {
    source: JsString,
    tags: Vec<Span>,
    tokens: Vec<AddressQuote>,
    i: usize,
    tag: usize,
}

impl QuotedAddresses {
    fn new(source: JsString, reads_inches: bool) -> Self {
        let tags = if source.contains(&0x3C) { tag_spans(&source, None) } else { Vec::new() };
        let mut tokens = address_quote_tokens(&source, &tags);
        let mut runs: Vec<Run> = Vec::new();
        let mut tag = 0;
        let mut i = 0;
        let mut previous_address_end = 0;
        for m in URL_REGEX.find_all(&source) {
            if inside_tag(&source, &tags, &mut tag, m.index()) {
                continue;
            }
            while i < tokens.len() && tokens[i].at < m.index() {
                i += 1;
            }
            if i > 0 {
                let before = &tokens[i - 1];
                if before.glyph == Some(QUOT_U) && before.end == m.index() && !before.in_address {
                    let from = previous_address_end.max(if i >= 2 { tokens[i - 2].end } else { 0 });
                    let left = js::slice(&source, from, before.at);
                    let mut email_end = None;
                    if left.contains(&0x40) {
                        for mail in EMAIL_REGEX.find_all(left) {
                            email_end = Some(mail.end());
                        }
                    }
                    tokens[i - 1].before_address = email_end != Some(left.len());
                }
            }
            previous_address_end = m.end();
            let from = i;
            let mut limit = m.end();
            while i < tokens.len() && tokens[i].at <= limit && tokens[i].glyph.is_some() {
                limit = limit.max(tokens[i].end);
                i += 1;
            }
            if i > from {
                let region: Vec<usize> = (from..i).collect();
                pair_members(&mut tokens, &region, &|_, _, _| true, false, false);
                let mut tail = limit;
                let run = runs.len();
                runs.push(Run { end: limit, length: 0 });
                for token in tokens[from..i].iter_mut().rev() {
                    token.in_address = true;
                    token.internal = token.partner.is_some();
                    if token.end == tail {
                        token.run = Some(run);
                        runs[run].length += 1;
                        token.remaining = runs[run].length;
                        tail = token.at;
                    }
                }
            }
        }
        let digit_before = |at: usize| digit_before_quote(&source, at, false).is_some();
        let mut speech = Vec::new();
        for (k, token) in tokens.iter_mut().enumerate() {
            if token.in_address || token.before_address || token.glyph != Some(QUOT_U) {
                speech.push(k);
                continue;
            }
            let (at, end) = (token.at, token.end);
            if reads_inches && digit_before(at) {
                let mut start = at - 1;
                while start > 0 && digit_before(start) {
                    start -= 1;
                }
                let before = start.checked_sub(1).map(|b| source[b]);
                token.measurement = !inside_quote_word(before, source.get(start).copied());
                token.measured_value = matches!(before, Some(0x2F | 0x27 | 0x2032)) && digit_before(start - 1);
            }
            let left = at.checked_sub(1).map(|b| source[b]);
            let right = source.get(end).copied();
            if !inside_quote_word(left, right) || opens_glued_cyrillic(left, right) {
                speech.push(k);
            }
        }
        let may_open = |tokens: &[AddressQuote], members: &[usize], index: usize, previous_closer: Option<u16>| {
            let token = &tokens[members[index]];
            let (at, end) = (token.at, token.end);
            let left = if token.after_dash { Some(MDASH_U) } else { at.checked_sub(1).map(|b| source[b]) };
            let next = source.get(end).copied().unwrap_or(0x20);
            let previous = index.checked_sub(1).map(|p| &tokens[members[p]]);
            let touches = previous.is_some_and(|p| p.end == at);
            (!token.in_address
                && token.glyph == Some(QUOT_U)
                && (could_open(&source, at, &mut QuoteTags::default(), end, token.after_dash.then_some(MDASH_U))
                    || opens_glued_cyrillic(at.checked_sub(1).map(|b| source[b]), Some(next))))
                || (!js::is_js_space(next)
                    && ((touches
                        && !token.in_address
                        && previous.is_some_and(|p| !p.in_address)
                        && previous_closer.is_some())
                        || opening_quote_neighbours(if touches { previous.and_then(|p| p.glyph) } else { left }, next)))
        };
        let claim_speech = |tokens: &mut Vec<AddressQuote>, paragraph: &[usize], openers: &mut Vec<usize>| {
            let mut open = Vec::new();
            let mut pending: Vec<usize> = Vec::new();
            let mut next = 0;
            for &k in paragraph {
                while next < openers.len() && tokens[openers[next]].at < tokens[k].at {
                    if open.is_empty() {
                        pending.push(openers[next]);
                    }
                    next += 1;
                }
                let token = &tokens[k];
                if token.closer.is_some() {
                    open.push(k);
                } else if token.partner.is_some() {
                    open.pop();
                } else if !token.in_address
                    && open.is_empty()
                    && pending.last().and_then(|&p| tokens[p].glyph.and_then(matching_quote)) == token.glyph
                    && token.glyph.is_some()
                    && let Some(p) = pending.pop()
                {
                    tokens[p].speech_start = true;
                }
            }
            openers.clear();
        };
        let finish = |tokens: &mut Vec<AddressQuote>, paragraph: &mut Vec<usize>, openers: &mut Vec<usize>| {
            pair_members(tokens, paragraph, &|t, index, closer| may_open(t, paragraph, index, closer), true, true);
            for index in 0..paragraph.len() {
                let k = paragraph[index];
                let previous_closer = index.checked_sub(1).and_then(|p| tokens[paragraph[p]].closer);
                let token = &tokens[k];
                if !token.in_address
                    && token.glyph == Some(QUOT_U)
                    && token.partner.is_none()
                    && token.after_dash_boundary
                {
                    tokens[k].closer = None;
                }
                let token = &tokens[k];
                if !token.in_address
                    && token.glyph == Some(LDQUO_U)
                    && token.partner.is_none()
                    && !may_open(tokens, paragraph, index, previous_closer)
                {
                    tokens[k].closer = None;
                }
            }
            claim_speech(tokens, paragraph, openers);
            paragraph.clear();
        };
        let mut paragraph = Vec::new();
        let mut openers = Vec::new();
        for &k in &speech {
            let token = &tokens[k];
            if token.glyph.is_none() {
                finish(&mut tokens, &mut paragraph, &mut openers);
            } else if !token.internal && (!token.in_address || token.run.is_some()) {
                tokens[k].close_only = tokens[k].in_address;
                paragraph.push(k);
            } else if token.in_address && !token.internal && token.closer.is_some() && token.glyph != Some(QUOT_U) {
                openers.push(k);
            }
        }
        finish(&mut tokens, &mut paragraph, &mut openers);
        reserve_prose_closers(&mut tokens, &speech);
        let mut enclosing: Vec<usize> = Vec::new();
        let mut unpaired = 0usize;
        for &k in &speech {
            let opener = enclosing.last().copied();
            if let Some(alternative) = tokens[k].alternative
                && tokens[k].partner == opener
                && unpaired == 0
                && let Some(o) = opener
            {
                tokens[o].partner = Some(alternative);
                tokens[alternative].partner = Some(o);
                tokens[k].partner = None;
                if tokens[k].measurement {
                    continue;
                }
            }
            let opener_partner = opener.and_then(|o| tokens[o].partner).map(|p| &tokens[p]);
            if tokens[k].in_address
                && tokens[k].run.is_some()
                && unpaired == 0
                && opener_partner.is_some_and(|p| p.at > tokens[k].at && !p.in_address)
            {
                tokens[k].address_end = true;
            }
            if tokens[k].glyph.is_none() {
                enclosing.clear();
                unpaired = 0;
            } else if !tokens[k].in_address && tokens[k].closer.is_some() {
                enclosing.push(k);
                if tokens[k].partner.is_none() {
                    unpaired += 1;
                }
            } else if closes_speech(&tokens, &runs, opener, k, unpaired, enclosing.len()) {
                let o = opener.expect("closing an open level");
                let token = &tokens[k];
                let partner_run = token.partner.and_then(|p| tokens[p].run);
                let shared = token.internal && (token.run.is_none() || partner_run != token.run);
                if !shared || (token.run.is_some_and(|r| runs[r].end == token.end) && token.closer.is_none()) {
                    if let Some(shared_end) = tokens[o].shared_end {
                        tokens[shared_end].speech_end = false;
                    }
                    tokens[k].speech_end = true;
                    if !shared {
                        enclosing.pop();
                        if tokens[o].partner.is_none() {
                            unpaired -= 1;
                        }
                        continue;
                    }
                    tokens[o].shared_end = Some(k);
                }
            }
        }
        QuotedAddresses { source, tags, tokens, i: 0, tag: 0 }
    }

    fn address(&mut self, original: &[u16], address: &[u16], at: usize) -> JsString {
        let (source, tokens) = (&self.source, &self.tokens);
        while self.i < tokens.len() && tokens[self.i].at < at {
            self.i += 1;
        }
        if source.get(at) != original.get(at) || inside_tag(source, &self.tags, &mut self.tag, at) {
            return address.to_vec();
        }
        let limit = at + address.len();
        let mut paired_end = at;
        while self.i < tokens.len() && tokens[self.i].at <= limit.max(paired_end) {
            let token = &tokens[self.i];
            if token.glyph.is_none() || source[token.at] == QUOT_U {
                break;
            }
            if token.speech_end || token.speech_start {
                if token.at >= limit {
                    break;
                }
                let usual = URL_START_REGEX.exec_at(js::slice(source, at, token.at), 0).map_or(0, |c| c.whole().len());
                return js::slice(original, at, (at + usual).max(paired_end)).to_vec();
            }
            if token.address_end || (token.internal && token.closer.is_none()) {
                paired_end = token.end;
            }
            if token.at >= limit && !token.internal && !token.address_end {
                break;
            }
            self.i += 1;
        }
        js::slice(original, at, limit.max(paired_end)).to_vec()
    }
}

pub struct AddressLayout<'a> {
    pub cuts: Vec<usize>,
    pub prose: Prose<'a>,
}

pub type Prose<'a> = Box<dyn FnOnce(CheckBudget<'_>) -> Result<JsString> + 'a>;

pub type AddressSyntax<'a> = &'a dyn Fn(&[u16], CheckBudget<'_>) -> Result<AddressLayout<'a>>;

struct Context<'a> {
    source: JsString,
    prose: Option<Prose<'a>>,
    check: CheckBudget<'a>,
    reads_inches: bool,
    blocks_vaulted: bool,
    quoted: Option<QuotedAddresses>,
}

#[derive(Debug)]
pub struct SafeSequences {
    urls: Vec<JsString>,
    emails: Vec<JsString>,
    tokens: Vec<JsString>,
    ids: HashMap<JsString, usize>,
    entropy: EntropyPool,
    nonce: String,
    marker: JsString,
    counted: Option<(JsString, Census)>,
}

impl SafeSequences {
    pub fn new(fill_random: fn(&mut [u8])) -> Self {
        SafeSequences {
            urls: Vec::new(),
            emails: Vec::new(),
            tokens: Vec::new(),
            ids: HashMap::new(),
            entropy: EntropyPool::new(fill_random),
            nonce: String::new(),
            marker: js::u(MARKER),
            counted: None,
        }
    }

    pub fn reset(&mut self) {
        self.nonce = self.entropy.nonce();
        self.marker = js::u(&format!("{MARKER}{}", self.nonce));
        self.reset_frame();
    }

    fn nonce_units(&self) -> &[u16] {
        &self.marker[MARKER.len()..]
    }

    pub fn reset_frame(&mut self) {
        self.urls.clear();
        self.emails.clear();
        self.tokens.clear();
        self.ids.clear();
        self.counted = None;
    }

    fn id_of(&mut self, kind: Kind, content: &[u16]) -> Result<usize> {
        let entries = match kind {
            Kind::Url => &mut self.urls,
            Kind::Email => &mut self.emails,
            Kind::Token => &mut self.tokens,
        };
        if entries.len() >= MAX_ENTRIES {
            let (name, reason) = kind.name();
            return Err(capacity_error(
                format!("SafeSequences: too many {name} in input"),
                json!({ "kind": reason, "max": MAX_ENTRIES }),
            ));
        }
        let key = cat!(kind.letter(), content);
        if let Some(&known) = self.ids.get(&key) {
            return Ok(known);
        }
        let id = entries.len();
        entries.push(content.to_vec());
        self.ids.insert(key, id);
        Ok(id)
    }

    fn replace_fallible(
        &mut self,
        re: &Re,
        text: &[u16],
        mut f: impl FnMut(&mut Self, &Caps<'_>) -> Result<JsString>,
    ) -> Result<JsString> {
        let mut error = None;
        let out = re.replace_with(text, |c| {
            if error.is_some() {
                return Vec::new();
            }
            match f(self, c) {
                Ok(s) => s,
                Err(e) => {
                    error = Some(e);
                    Vec::new()
                }
            }
        });
        match error {
            Some(e) => Err(e),
            None => Ok(out),
        }
    }

    pub fn left_in(&self, text: &[u16]) -> bool {
        js::includes(text, &self.marker)
    }

    pub fn protect_address_links(&mut self, text: &[u16]) -> Result<JsString> {
        if js::includes(text, &js::u("<a href=\"")) { self.protect_links(text) } else { Ok(text.to_vec()) }
    }

    pub fn protect(
        &mut self,
        text: &[u16],
        syntax: Option<AddressSyntax<'_>>,
        check: CheckBudget<'_>,
        reads_inches: bool,
        code_cuts: Option<&[usize]>,
    ) -> Result<JsString> {
        if !sequence_hint(text) {
            return Ok(text.to_vec());
        }
        let source = if code_cuts.is_some() { text.to_vec() } else { self.protect_address_links(text)? };
        let layout = syntax.map(|syntax| syntax(&source, check)).transpose()?;
        let (cuts, prose) = match layout {
            Some(layout) => (layout.cuts, Some(layout.prose)),
            None => (Vec::new(), None),
        };
        let mut context =
            Context { source, prose, check, reads_inches, blocks_vaulted: code_cuts.is_some(), quoted: None };
        let codes = code_cuts.unwrap_or_default();
        let source = context.source.clone();
        if cuts.is_empty() && codes.is_empty() {
            return self.protect_part(&source, &mut context, 0);
        }
        let mut parts = Vec::with_capacity(source.len());
        let mut from = 0;
        let (mut link, mut code) = (0, 0);
        while link < cuts.len() || code < codes.len() {
            let at = if cuts.get(link).copied().unwrap_or(usize::MAX) < codes.get(code).copied().unwrap_or(usize::MAX) {
                link += 1;
                cuts[link - 1]
            } else {
                code += 1;
                codes[code - 1]
            };
            if at == from {
                continue;
            }
            parts.extend(self.protect_part(js::slice(&source, from, at), &mut context, from)?);
            from = at;
        }
        parts.extend(self.protect_part(js::slice(&source, from, source.len()), &mut context, from)?);
        Ok(parts)
    }

    fn protect_part(&mut self, text: &[u16], context: &mut Context<'_>, offset: usize) -> Result<JsString> {
        if !sequence_hint(text) {
            return Ok(text.to_vec());
        }
        if context.blocks_vaulted && text.contains(&OPEN_UNIT) {
            let mut parts = Vec::with_capacity(text.len());
            let mut from = 0;
            for token in BLOCK_TOKEN_RE.find_all(text) {
                parts.extend(self.protect_addresses(js::slice(text, from, token.index()), context, offset + from)?);
                parts.extend_from_slice(token.whole());
                from = token.end();
            }
            parts.extend(self.protect_addresses(js::slice(text, from, text.len()), context, offset + from)?);
            return Ok(parts);
        }
        self.protect_addresses(text, context, offset)
    }

    fn protect_addresses(&mut self, text: &[u16], context: &mut Context<'_>, offset: usize) -> Result<JsString> {
        let nonce = self.nonce.clone();
        let mut result = text.to_vec();
        let mut parts: Vec<u16> = Vec::new();
        let mut replaced = false;
        let mut cursor = 0;
        for c in URL_REGEX.find_all(text) {
            let (m, at) = (c.whole(), c.index());
            if context.quoted.is_none()
                && (m.iter().any(|&u| matches!(u, 0xAB | 0xBB | 0x201E | 0x201C | 0x201D | 0x26))
                    || text.get(at + m.len()).is_some_and(|&u| matches!(u, 0xBB | 0x201C | 0x201D | 0x26)))
            {
                let prose = match context.prose.take() {
                    Some(prose) => prose(context.check)?,
                    None => context.source.clone(),
                };
                context.quoted = Some(QuotedAddresses::new(prose, context.reads_inches));
            }
            let candidate = match context.quoted.as_mut() {
                None => m.to_vec(),
                Some(quoted) => quoted.address(&context.source, m, offset + at),
            };
            let url = url_in(&candidate);
            if url.is_empty() || BARE_SCHEME_RE.test(url) {
                continue;
            }
            let id = self.id_of(Kind::Url, url)?;
            parts.extend_from_slice(js::slice(text, cursor, at));
            parts.extend(cat!("http://simonenko-xyz/A0SAFE", nonce.as_str(), "NUM", js::num(id), "ID"));
            cursor = at + url.len();
            replaced = true;
        }
        if replaced {
            parts.extend_from_slice(js::slice(text, cursor, text.len()));
            result = parts;
        }
        if result.contains(&0x40) {
            result = self.protect_emails(&result)?;
        }
        if result.contains(&0x2D) {
            result = self.replace_fallible(&UUID_REGEX, &result, |this, c| this.store_token(c.whole()))?;
        }
        if result.contains(&0x3A) {
            result = self.replace_fallible(&IPV6_REGEX, &result, |this, c| this.store_token(c.whole()))?;
        }
        Ok(result)
    }

    fn read_emails(&mut self, stretch: &[u16]) -> Result<JsString> {
        if !stretch.contains(&0x40) {
            return Ok(stretch.to_vec());
        }
        let nonce = self.nonce.clone();
        self.replace_fallible(&EMAIL_REGEX, stretch, |this, c| {
            let id = this.id_of(Kind::Email, c.whole())?;
            Ok(cat!("A1SAFE", nonce.as_str(), "NUM", js::num(id), "ID@simonenko.xyz"))
        })
    }

    fn protect_emails(&mut self, text: &[u16]) -> Result<JsString> {
        if !js::includes(text, &URL_CORE_HEAD) {
            return self.read_emails(text);
        }
        let mut parts = Vec::with_capacity(text.len());
        let mut cursor = 0;
        for m in ADDRESS_PLACEHOLDER_RE.find_all(text) {
            parts.extend(self.read_emails(js::slice(text, cursor, m.index()))?);
            parts.extend_from_slice(m.whole());
            cursor = m.end();
        }
        parts.extend(self.read_emails(js::slice(text, cursor, text.len()))?);
        Ok(parts)
    }

    fn protect_links(&mut self, text: &[u16]) -> Result<JsString> {
        self.replace_fallible(&ADDRESS_LINK_RE, text, |this, c| {
            if decode_references(c.s(2)) != decode_references(c.s(3)) {
                return Ok(c.whole().to_vec());
            }
            let id = this.id_of(Kind::Url, &cat!(c.s(1), c.s(3)))?;
            Ok(cat!(
                "<a href=\"",
                c.s(1),
                c.s(2),
                "\">simonenko-xyz/A0SAFE",
                this.nonce.as_str(),
                "NUM",
                js::num(id),
                "ID</a>"
            ))
        })
    }

    fn store_token(&mut self, content: &[u16]) -> Result<JsString> {
        let id = self.id_of(Kind::Token, content)?;
        Ok(cat!("AzSAFE", self.nonce.as_str(), "TOK", js::num(id), "TK"))
    }

    pub fn protect_tokens(&mut self, text: &[u16], pattern: &Re, accepts: &dyn Fn(&[u16]) -> bool) -> Result<JsString> {
        pattern.try_replace_with(text, |c| {
            if accepts(c.whole()) { self.store_token(c.whole()) } else { Ok(c.whole().to_vec()) }
        })
    }

    pub fn restore(&self, text: JsString) -> JsString {
        self.restore_into(text, false, None)
    }

    pub fn restore_href(&self, text: JsString) -> JsString {
        self.restore_into(text, true, None)
    }

    pub fn restore_with(&self, text: JsString, mapping: &mut crate::shared::provenance::RestoreMap) -> JsString {
        self.restore_into(text, false, Some(mapping))
    }

    fn restore_into(
        &self,
        text: JsString,
        href: bool,
        mut mapping: Option<&mut crate::shared::provenance::RestoreMap>,
    ) -> JsString {
        let mut result = text;
        if self.urls.is_empty() && self.emails.is_empty() && self.tokens.is_empty() {
            return result;
        }
        let nonce = self.nonce_units();
        let lookup = |entries: &Vec<JsString>, c: &Caps<'_>| -> Option<JsString> {
            if c.s(1) != nonce {
                return None;
            }
            let id = js::parse_int(c.s(2)).and_then(|n| usize::try_from(n).ok())?;
            entries.get(id).cloned()
        };
        if !self.urls.is_empty() {
            result = crate::shared::provenance::replace_recorded(
                result,
                &URL_RESTORE_RE,
                |c| match lookup(&self.urls, c) {
                    None => c.whole().to_vec(),
                    Some(url) if !href => url,
                    Some(url) if HREF_SCHEME_RE.test(&url) => decode_references(&url),
                    Some(url) => cat!("http://", decode_references(&url)),
                },
                mapping.as_deref_mut(),
            );
            result = crate::shared::provenance::replace_recorded(
                result,
                &URL_RESTORE_NOPROTO_RE,
                |c| match lookup(&self.urls, c) {
                    Some(u) => SCHEME_RE.replace_with(&u, |_| Vec::new()),
                    None => c.whole().to_vec(),
                },
                mapping.as_deref_mut(),
            );
        }
        if !self.emails.is_empty() {
            result = crate::shared::provenance::replace_recorded(
                result,
                &EMAIL_RESTORE_RE,
                |c| match lookup(&self.emails, c) {
                    None => c.whole().to_vec(),
                    Some(email) if !href => email,
                    Some(email) => decode_references(&email),
                },
                mapping.as_deref_mut(),
            );
        }
        if !self.tokens.is_empty() {
            result = crate::shared::provenance::replace_recorded(
                result,
                &TOKEN_RESTORE_RE,
                |c| lookup(&self.tokens, c).unwrap_or_else(|| c.whole().to_vec()),
                mapping,
            );
        }
        result
    }

    pub fn has_entries(&self) -> bool {
        !(self.urls.is_empty() && self.emails.is_empty() && self.tokens.is_empty())
    }

    pub fn settle(&mut self, before: &[u16], after: JsString) -> std::result::Result<JsString, Sealed> {
        self.settle_with(before, after, false)
    }

    pub fn settle_with(
        &mut self,
        before: &[u16],
        after: JsString,
        record: bool,
    ) -> std::result::Result<JsString, Sealed> {
        if !self.has_entries() || after == before {
            return Ok(after);
        }
        let found = match &self.counted {
            Some((text, counts)) if text == before => *counts,
            _ => self.census(before),
        };
        let now = self.census(&after);
        if holds(&found, &now) {
            return Ok(self.remember(after, now));
        }
        let nonce = self.nonce_units();
        let mut sealed: Vec<JsString> = Vec::new();
        let mut edits = record.then(Vec::new);
        let text = WHOLE_RE.replace_with(before, |c| {
            let id = (2..=4).find(|&i| c.has(i)).map_or(&[][..], |i| c.s(i));
            if id != nonce {
                return c.whole().to_vec();
            }
            let link = if c.has(1) && c.s(1) != HTTP.as_slice() { c.s(1) } else { &[] };
            sealed.push(c.whole()[link.len()..].to_vec());
            let text = cat!(link, [OPEN_UNIT, 0x53].as_slice(), js::num(sealed.len() - 1), [CLOSE_UNIT].as_slice());
            if let Some(edits) = edits.as_mut() {
                edits.push(crate::shared::provenance::MapEdit::from(crate::Edit {
                    start: c.index(),
                    end: c.end(),
                    text: text.clone(),
                }));
            }
            text
        });
        Err(Sealed { text, values: sealed, found, edits })
    }

    pub fn unseal(&mut self, before: &[u16], sealed: Sealed, again: JsString) -> JsString {
        self.unseal_with(before, sealed, again, None).0
    }

    pub fn unseal_with(
        &mut self,
        before: &[u16],
        sealed: Sealed,
        again: JsString,
        mapping: Option<&mut crate::shared::provenance::RestoreMap>,
    ) -> (JsString, bool) {
        let again = crate::shared::provenance::replace_recorded(
            again,
            &SEALED_RE,
            |c| {
                js::parse_int(c.s(1))
                    .and_then(|n| usize::try_from(n).ok())
                    .and_then(|n| sealed.values.get(n).cloned())
                    .unwrap_or_else(|| c.whole().to_vec())
            },
            mapping,
        );
        let kept = self.census(&again);
        if holds(&sealed.found, &kept) {
            (self.remember(again, kept), true)
        } else {
            (self.remember(before.to_vec(), sealed.found), false)
        }
    }

    fn remember(&mut self, text: JsString, counts: Census) -> JsString {
        match &mut self.counted {
            Some((cached, cached_counts)) => {
                cached.clear();
                cached.extend_from_slice(&text);
                *cached_counts = counts;
            }
            None => self.counted = Some((text.clone(), counts)),
        }
        text
    }

    fn census(&self, text: &[u16]) -> Census {
        census(text, self.nonce.as_bytes())
    }

    pub fn restore_regexes() -> &'static [Re] {
        &RESTORE_REGEXES
    }
}

#[cfg(test)]
#[path = "sequences_tests.rs"]
mod tests;
