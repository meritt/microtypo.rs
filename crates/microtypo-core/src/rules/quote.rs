use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::sync::{Arc, LazyLock};

use serde_json::Value;

use crate::cat;
use crate::errors::Result;
use crate::js::{self, Caps, JsString, Re};
use crate::protect::placeholders::{CLOSE, CLOSE_UNIT, OPEN, OPEN_UNIT, REFERENCE_HOLE, TRANSPARENT_TAG_KINDS};
use crate::protect::sequences::LINKABLE_SOURCE;
use crate::rule_runner::{Ctx, Replacement, Rule, RuleGroupDef};
use crate::shared::boundaries::{
    NOT_AFTER_LONE_QUOTE, OPEN_CYRILLIC_SOURCE, OPEN_LEFT, QUOTE_PUNCTUATION, QUOTE_WORD, WEAK_LEFT, WEAK_STOP,
    opening_quote_neighbours,
};
use crate::shared::character_reference::{
    authored_reference_glyph, character_reference_ending_at, kept_glyph_at, kept_reference_glyph, quote_boundary_glyph,
    read_character_reference_at,
};
use crate::shared::glyphs::{BDQUO, COPY, HELLIP, LAQUO, LDQUO, NBSP, PRIME, RAQUO, unit};
use crate::shared::inline_boundaries::{
    inline_end_tag_start, inline_text_end, native_tag_end, native_tag_start, native_void_tag,
};
use crate::shared::nowrap_context::{SameText, held_before_nowrap_end};
use crate::shared::quote_pairs::{
    PairSlot, QuoteToken, RDQUO_U, Visit, matching_quote, pair_quote_tokens, resolve_authored_quote_pairs,
};
use crate::shared::rule_features::F;
use crate::shared::strings::count;

use super::number::follows_feet;
use super::quote_state::{
    Kept, OpenedBehind, QUOTATION_MARK_RE, QuoteOptions, QuoteTags, SEPARATOR, SPACED_CLOSER_LEFT, TITLE_CONTENT_CHAR,
    TITLE_WORD_CHAR, authored_opening_at, block_end_tag_start, can_start_quote, canonical_quote_roles, could_open,
    dash_quote_at, digit_before_quote, is_digit, is_quote_mark, is_separator, opening_content_at,
    opening_content_start, process_quotes, quote_content_at, quote_follower, quote_follower_glyph, quote_left_dash,
    quote_paragraphs, scalar_at, token_start,
};

const FIRS_OPEN: u16 = unit(LAQUO);
const FIRS_CLOSE: u16 = unit(RAQUO);
const QUOT: u16 = 0x22;
const GT: u16 = 0x3E;
const BDQUO_U: u16 = unit(BDQUO);
const LDQUO_U: u16 = unit(LDQUO);
const NBSP_U: u16 = unit(NBSP);
const PRIME_U: u16 = unit(PRIME);
const SEMICOLON: u16 = 0x3B;

static QUOTES_OUTSIDE_A_RE: LazyLock<Re> =
    LazyLock::new(|| Re::new(&format!(r#"(<{OPEN}A\d+{CLOSE}>)"([\s\S]+?)"(<\/{OPEN}A\d+{CLOSE}>)"#), "g"));

static WORD_END_RE: LazyLock<Re> = LazyLock::new(|| Re::new(&format!("[a-zа-яё0-9{HELLIP}]$"), "iu"));
static LONE_SPACE_RE: LazyLock<Re> = LazyLock::new(|| Re::new(r"^\s$", "u"));
static CLOSER_LEFT: LazyLock<String> = LazyLock::new(|| {
    format!(r"[a-zа-яё0-9]|\.|{HELLIP}|!|\?|>|\)|\]|:|\+|%|@|#|\$|\*|`(?<={OPEN}B\d{{1,9}}{CLOSE}`{{1,16}})")
});
static CLOSING_NEIGHBOUR_RE: LazyLock<Re> =
    LazyLock::new(|| Re::new(r"(?<=[\p{L}\p{M}\p{N}\p{S}\p{Pe}\p{Pf}\p{Terminal_Punctuation}]|\S\p{P})", "uy"));
static INDENT_END_RE: LazyLock<Re> = LazyLock::new(|| Re::new(&format!(r"{OPEN}W\d{{1,9}}{CLOSE}$"), "u"));
const INDENT_MAX: usize = 12;

fn vt(v: Visit<'_>) -> Result<()> {
    v.map_or(Ok(()), |f| f())
}

fn unit_at(text: &[u16], at: Option<usize>, default: u16) -> u16 {
    at.and_then(|i| text.get(i).copied()).unwrap_or(default)
}

fn quotation_mark(c: Option<u16>) -> bool {
    c.is_some_and(|c| QUOTATION_MARK_RE.test(&[c]))
}

fn indent_before(source: &[u16], at: usize) -> usize {
    if at == 0 || source[at - 1] != CLOSE_UNIT {
        return 0;
    }
    INDENT_END_RE.exec(&source[at.saturating_sub(INDENT_MAX)..at]).map_or(0, |m| m.whole().len())
}

fn ends_before(cx: &QuoteTags<'_>, source: &[u16], raw: &[u16], from: usize, to: usize) -> bool {
    let name_len = raw.iter().position(|&c| matches!(c, 0x2F | 0x3E) || js::is_js_space(c)).unwrap_or(raw.len());
    if name_len == 0 {
        return false;
    }
    let name = &raw[..name_len];
    let window = &source[..source.len().min(to + 1)];
    let mut at = from;
    while let Some(found) = js::index_of(window, &[0x3C, 0x2F], at) {
        if let Some(tag_end) = native_tag_end(source, found)
            && cx.raw(js::slice(source, found + 2, tag_end - 1)) == Some(name)
        {
            return true;
        }
        at = found + 2;
    }
    false
}

fn node_between(cx: &QuoteTags<'_>, source: &[u16], from: usize, to: usize) -> bool {
    let window = &source[..source.len().min(to)];
    let mut at = from;
    while let Some(found) = js::index_of_unit(window, 0x3C, at) {
        if let Some(tag_end) = native_tag_end(source, found)
            && cx
                .raw(js::slice(source, found + 1, tag_end - 1))
                .is_some_and(|raw| matches!(raw.first(), Some(0x21 | 0x3F)))
        {
            return true;
        }
        at = found + 1;
    }
    false
}

fn close_at_boundary(m: &Caps<'_>, cx: &mut QuoteTags<'_>, replacement: JsString) -> Result<JsString> {
    let source = m.input;
    let end = m.index() + 1;
    if m.whole().first() != Some(&GT) || native_tag_start(source, end).is_none() {
        return Ok(replacement);
    }
    let glued = m.whole().contains(&FIRS_OPEN);
    let mut before = end;
    loop {
        let previous = before;
        before = inline_text_end(cx.names(), source, before);
        while let Some(start) = inline_end_tag_start(source, before).or_else(|| block_end_tag_start(source, before, cx))
        {
            before = start;
        }
        let comment = native_tag_start(source, before)
            .filter(|&tag_start| cx.raw_comment(js::slice(source, tag_start + 1, before - 1)));
        if let Some(tag_start) = comment {
            before = tag_start;
        }
        let unspaced = before;
        while before > 0 && (source[before - 1] == 0x20 || source[before - 1] == 0x09) {
            if before & 1023 == 0 {
                cx.check("quote:left")?;
            }
            before -= 1;
        }
        before -= indent_before(source, before);
        if glued && before != unspaced && comment.is_none() {
            return Ok(m.whole().to_vec());
        }
        if before == previous {
            break;
        }
    }
    let width = if before >= 2 && js::code_point_at(source, before - 2).is_some_and(|c| c > 0xffff) { 2 } else { 1 };
    let previous = js::slice(source, before.saturating_sub(width), before);
    let tag = if previous == [GT] { native_tag_start(source, before) } else { None };
    if let Some(tag) = tag {
        let raw = cx.raw(js::slice(source, tag + 1, before - 1)).unwrap_or_default().to_vec();
        let whole = native_void_tag(cx.names(), source, tag, before)
            || matches!(raw.first(), Some(0x21 | 0x3F))
            || raw.last() == Some(&0x2F)
            || ends_before(cx, source, &raw, before, end);
        return Ok(if whole { replacement } else { m.whole().to_vec() });
    }
    let content = !previous.is_empty() && quote_content_at(source, before - width, cx);
    Ok(if content { replacement } else { m.whole().to_vec() })
}

fn repeat_char(s: &[u16], ch: u16) -> JsString {
    vec![ch; count(s, &[QUOT])]
}

fn closes_before_paren(p1: &[u16], next: &[u16], offset: usize, source: &[u16]) -> bool {
    next.first() == Some(&0x28) && LONE_SPACE_RE.test(p1) && WORD_END_RE.test(js::slice(source, 0, offset))
}

static OPEN_AFTER_STRONG_RE: LazyLock<Re> =
    LazyLock::new(|| Re::new(&format!(r#"(^|[{}])((?:"|\\")+)(?=([^ \t]))(?:(?!{LAQUO})[^ \t])?"#, *OPEN_LEFT), "giu"));

fn dash_keeps(p2: &[u16], p3: &[u16], source: &[u16], at: usize, cx: &mut QuoteTags<'_>) -> bool {
    if p2 != [QUOT] {
        return false;
    }
    let dash = quote_left_dash(source, at, cx);
    dash.is_some() && (p3 == [NBSP_U] || dash_quote_at(source, at + 1, dash, cx))
}

fn open_after_strong(m: &Caps<'_>, ctx: &mut Ctx<'_>) -> Result<JsString> {
    let (full, p1, p2, p3) = (m.whole(), m.s(1), m.s(2), m.s(3));
    let (offset, source) = (m.index(), m.input);
    {
        let (mut cx, _) = ctx.quote_cx();
        if dash_keeps(p2, p3, source, offset + p1.len(), &mut cx) {
            return Ok(full.to_vec());
        }
    }
    if shut_behind_nowrap_end(p1, p2, p3, offset, source, ctx)? {
        return Ok(full.to_vec());
    }
    let (quote, follower) = full.split_at(p1.len() + p2.len());
    let (mut cx, qs) = ctx.quote_cx();
    let mut out = open_before_word(quote, p1, p2, p3, offset, source, &mut cx, qs)?;
    out.extend_from_slice(follower);
    Ok(out)
}

static OPEN_AFTER_WEAK_RE: LazyLock<Re> = LazyLock::new(|| {
    Re::new(
        &format!(
            r#"([{WEAK_LEFT}{CLOSE}])((?:"|\\")+)(?=([^{}]|(?<=[{WEAK_LEFT}{CLOSE}]"")[.,;:!?){HELLIP}]))"#,
            *WEAK_STOP
        ),
        "giu",
    )
});

static OPEN_CYRILLIC_RE: LazyLock<Re> = LazyLock::new(|| Re::new(OPEN_CYRILLIC_SOURCE, "giu"));

static OPENS_BEHIND: LazyLock<[Re; 3]> = LazyLock::new(|| {
    [&*OPEN_AFTER_STRONG_RE, &*OPEN_AFTER_WEAK_RE, &*OPEN_CYRILLIC_RE].map(|re| Re::new(re.source(), "iu"))
});

fn shut_behind_nowrap_end(
    p1: &[u16],
    p2: &[u16],
    p3: &[u16],
    offset: usize,
    source: &[u16],
    ctx: &mut Ctx<'_>,
) -> Result<bool> {
    let held = if p1 == [GT] { held_before_nowrap_end(ctx, source, offset)? } else { None };
    Ok(held.is_some_and(|held| {
        let probe = cat!(held, p2, p3);
        !OPENS_BEHIND.iter().any(|re| re.test(&probe))
    }))
}

static OPEN_BEFORE_SPACE_RE: LazyLock<Re> = LazyLock::new(|| {
    Re::new(
        &format!(
            r#"(^|[{}{CLOSE}]|(?<={});)("|\\")(?:([ \t]+)([^{SEPARATOR}]+)|(?=[<{QUOTE_PUNCTUATION}])|({NBSP}+)(?=[^{SEPARATOR}]))"#,
            *OPEN_LEFT, *REFERENCE_HOLE
        ),
        "giu",
    )
});

static EMPTY_PAIR_FOLLOWER_RE: LazyLock<Re> = LazyLock::new(|| Re::new("[.,;:!?)…]", "u"));
static LATIN_LETTER_RE: LazyLock<Re> = LazyLock::new(|| Re::new(r"\p{Script=Latin}", "u"));

fn closes_spaced_empty_pair(
    p1: &[u16],
    p2: &[u16],
    p3: &[u16],
    offset: usize,
    source: &[u16],
    cx: &mut QuoteTags<'_>,
) -> bool {
    if p2 != [QUOT] || (p1 != [0x20] && p1 != [0x09]) || !EMPTY_PAIR_FOLLOWER_RE.test(p3) {
        return false;
    }
    let mut before = offset.checked_sub(1);
    while let Some(b) = before
        && (source[b] == 0x20 || source[b] == 0x09)
    {
        before = b.checked_sub(1);
    }
    before.is_some_and(|b| source[b] == QUOT && could_open(source, b, cx, b + 1, None))
}

fn opening_quote_padding(source: &[u16], at: usize, cx: &QuoteTags<'_>) -> &'static [u16] {
    if at > 0 && source[at - 1] == CLOSE_UNIT && cx.label_address_before(source, at) { &[0x20] } else { &[] }
}

#[allow(clippy::too_many_arguments)]
fn open_before_word(
    full: &[u16],
    p1: &[u16],
    p2: &[u16],
    p3: &[u16],
    offset: usize,
    source: &[u16],
    cx: &mut QuoteTags<'_>,
    qs: &mut QuoteSession,
) -> Result<JsString> {
    let at = offset + p1.len();
    if p1 == [SEMICOLON] && ends_authored_quote_pair(source, offset, cx, qs)? {
        return Ok(full.to_vec());
    }
    if !opening_content_at(source, offset + p1.len() + p2.len(), cx, false)? {
        let last = if p2.ends_with(&[0x5C, QUOT]) { 2 } else { 1 };
        if p2.len() <= last {
            return Ok(full.to_vec());
        }
        let cut = p2.len() - last;
        let inner =
            open_before_word(&full[..full.len() - last], p1, &p2[..cut], &p2[cut..=cut], offset, source, cx, qs)?;
        return Ok(cat!(inner, &p2[cut..]));
    }
    let empty_pair = p2 == [QUOT, QUOT] && EMPTY_PAIR_FOLLOWER_RE.test(p3);
    let right = if empty_pair { QUOT } else { quote_follower(source, at + p2.len(), cx) };
    let left = (p1 == [FIRS_OPEN]).then_some(Some(FIRS_OPEN));
    if !can_start_quote(source, at, right, cx, left)
        || closes_before_paren(p1, p3, offset, source)
        || closes_spaced_empty_pair(p1, p2, p3, offset, source, cx)
    {
        return Ok(full.to_vec());
    }
    if empty_pair {
        return Ok(cat!(p1, FIRS_OPEN, FIRS_CLOSE));
    }
    Ok(cat!(p1, opening_quote_padding(source, at, cx), repeat_char(p2, FIRS_OPEN)))
}

const OPENS: i32 = 1;
const UNDECIDED: i32 = 0;
const CLOSES: i32 = -1;

fn quote_role(text: &[u16], at: usize, cx: &mut QuoteTags<'_>) -> i32 {
    let c = text[at];
    if c == FIRS_OPEN {
        return OPENS;
    }
    if c == FIRS_CLOSE {
        return CLOSES;
    }
    if could_open(text, at, cx, at + 1, None) { UNDECIDED } else { CLOSES }
}

fn empty_pair_at(text: &[u16], at: &[usize], j: usize, end: usize, immutable: Option<&HashSet<usize>>) -> bool {
    if immutable.is_some_and(|set| set.contains(&at[j]) || at.get(j + 1).is_some_and(|next| set.contains(next))) {
        return false;
    }
    if j + 1 >= end || text[at[j]] != QUOT || text[at[j + 1]] != QUOT {
        return false;
    }
    let from = at[j] + 1;
    let to = token_start(text, at[j + 1]);
    if to <= from {
        return false;
    }
    text[from..to].iter().all(|&c| c == 0x20 || c == 0x09)
}

#[derive(Default)]
struct Assigned {
    at: Vec<usize>,
    roles: Vec<i32>,
    depths: Vec<i64>,
    pairs: Vec<bool>,
}

fn assign_paragraph_roles(
    text: &[u16],
    (from, stop): (usize, usize),
    a: &mut Assigned,
    cx: &mut QuoteTags<'_>,
    immutable: Option<&HashSet<usize>>,
) {
    let base = a.at.len();
    let mut opens: i64 = 0;
    let mut closes: i64 = 0;
    let mut undecided: i64 = 0;
    let mut standing: i64 = 0;
    for i in from..stop {
        let c = text[i];
        if c != FIRS_OPEN && c != FIRS_CLOSE && c != QUOT {
            continue;
        }
        if c == QUOT && immutable.is_some_and(|set| set.contains(&i)) {
            continue;
        }
        let role = quote_role(text, i, cx);
        if role == CLOSES && standing == 0 && undecided == 0 && c == QUOT {
            continue;
        }
        a.at.push(i);
        a.roles.push(role);
        if role == OPENS {
            opens += 1;
            standing += 1;
        } else if role == CLOSES {
            closes += 1;
            standing = (standing - 1).max(0);
        } else {
            undecided += 1;
        }
    }

    let mut closings = undecided - ((closes + undecided - opens).div_euclid(2)).clamp(0, undecided);
    let end = a.at.len();
    let mut depth: i64 = 0;

    let mut need = vec![0i64; end - base + 1];
    for j in (base..end).rev() {
        let k = j - base;
        need[k] = if a.roles[j] == CLOSES { need[k + 1] + 1 } else { (need[k + 1] - 1).max(0) };
    }

    let mut later_undecided = undecided;
    let mut j = base;
    while j < end {
        if a.roles[j] == UNDECIDED {
            later_undecided -= 1;
            if empty_pair_at(text, &a.at, j, end, immutable) {
                later_undecided -= i64::from(a.roles[j + 1] == UNDECIDED);
                closings -= i64::from(closings > 0);
                a.roles[j] = OPENS;
                a.roles[j + 1] = CLOSES;
                a.depths.push(depth);
                a.depths.push(depth + 1);
                a.pairs.push(true);
                a.pairs.push(false);
                j += 2;
                continue;
            }
            let starves = depth - 1 < need[j - base + 1] && closings <= later_undecided;
            let shuts = depth > 0 && closings > 0 && !starves;
            a.roles[j] = if shuts { CLOSES } else { OPENS };
            closings -= i64::from(shuts);
        }
        a.depths.push(depth);
        a.pairs.push(false);
        depth = (depth + i64::from(a.roles[j])).max(0);
        j += 1;
    }
}

struct RoleSource {
    text: JsString,
    offsets: Vec<usize>,
    immutable: HashSet<usize>,
}

fn quote_role_source(text: &[u16], cx: &mut QuoteTags<'_>) -> Result<Option<RoleSource>> {
    if !js::includes(text, &[OPEN_UNIT, 0x4E]) {
        return Ok(None);
    }
    let mut references = Vec::new();
    let mut seen = 0u32;
    let mut at = js::index_of_unit(text, 0x26, 0);
    while let Some(start) = at {
        if seen & 1023 == 0 {
            cx.check("quote:references")?;
        }
        seen = seen.wrapping_add(1);
        let mut next = start + 1;
        if let Some(reference) = read_character_reference_at(text, start) {
            if let Some(glyph) = kept_reference_glyph(Some(&reference))
                && let &[mark] = glyph.as_slice()
                && is_quote_mark(mark)
            {
                references.push((start, reference.end, mark));
            }
            next = reference.end;
        }
        at = js::index_of_unit(text, 0x26, next);
    }
    if references.is_empty() {
        return Ok(None);
    }

    let mut pieces = Vec::with_capacity(text.len());
    let mut offsets = Vec::with_capacity(text.len());
    let mut immutable = HashSet::new();
    let mut cursor = 0;
    for &(start, end, glyph) in &references {
        pieces.extend_from_slice(&text[cursor..start]);
        pieces.push(glyph);
        for i in cursor..start {
            if i & 1023 == 0 {
                cx.check("quote:references")?;
            }
            offsets.push(i);
        }
        immutable.insert(offsets.len());
        offsets.push(start);
        cursor = end;
    }
    pieces.extend_from_slice(&text[cursor..]);
    for i in cursor..text.len() {
        if i & 1023 == 0 {
            cx.check("quote:references")?;
        }
        offsets.push(i);
    }
    let budget = cx.budget();
    let text = canonical_quote_roles(pieces, cx, budget, false)?;
    Ok(Some(RoleSource { text, offsets, immutable }))
}

fn assign_quote_roles(text: &[u16], cx: &mut QuoteTags<'_>) -> Result<Assigned> {
    let source = quote_role_source(text, cx)?;
    let read = source.as_ref().map_or(text, |source| source.text.as_slice());
    let immutable = source.as_ref().map(|source| &source.immutable);
    let budget = cx.budget();
    let mut a = Assigned::default();
    for range in quote_paragraphs(read, cx, budget)? {
        assign_paragraph_roles(read, range, &mut a, cx, immutable);
    }
    if let Some(source) = &source {
        for at in &mut a.at {
            *at = source.offsets[*at];
        }
    }
    Ok(a)
}

fn close_after_copyright(m: &Caps<'_>, cx: &mut QuoteTags<'_>, qs: &mut QuoteSession) -> Result<JsString> {
    let (before, quotes) = (m.s(1), m.s(2));
    let text = m.input;
    if !qs.spaced_closers.as_mut().is_some_and(|known| known.holds(text)) {
        let a = assign_quote_roles(text, cx)?;
        let closing = (0..a.at.len()).filter(|&i| a.roles[i] == CLOSES && a.depths[i] > 0).map(|i| a.at[i]).collect();
        qs.spaced_closers = Some(KnownClosers::new(text.to_vec(), closing));
    }
    let at = m.index() + before.len() + usize::from(quotes.first() == Some(&0x5C));
    let closes = qs.spaced_closers.as_ref().is_some_and(|known| known.closing.contains(&at));
    Ok(if closes { cat!(before, repeat_char(quotes, FIRS_CLOSE)) } else { m.whole().to_vec() })
}

#[derive(Default)]
struct Plan {
    assigned: Option<Assigned>,
    seen: usize,
}

impl Plan {
    fn quote_at(&mut self, text: &[u16], cx: &mut QuoteTags<'_>, at: usize) -> Result<(i32, i64, Option<usize>)> {
        if self.assigned.is_none() {
            self.assigned = Some(assign_quote_roles(text, cx)?);
        }
        let Some(a) = &self.assigned else { return Ok((UNDECIDED, 0, None)) };
        while self.seen < a.at.len() && a.at[self.seen] < at {
            self.seen += 1;
        }
        let seen = self.seen;
        if seen >= a.at.len() || a.at[seen] != at {
            return Ok((UNDECIDED, 0, None));
        }
        Ok((a.roles[seen], a.depths[seen], if a.pairs[seen] { Some(a.at[seen + 1] + 1) } else { None }))
    }
}

fn open_spaced_quotes(text: &[u16], cx: &mut QuoteTags<'_>) -> Result<JsString> {
    if !text.contains(&QUOT) {
        return Ok(text.to_vec());
    }
    let re = &*OPEN_BEFORE_SPACE_RE;
    let mut parts: JsString = Vec::with_capacity(text.len());
    let mut plan = Plan::default();
    let mut cursor = 0usize;
    let mut last_index = 0usize;
    while let Some(m) = re.exec_at(text, last_index) {
        let (full, p1, p2, p4) = (m.whole(), m.s(1), m.s(2), m.s(4));
        let after_left = m.index() + p1.len();
        last_index = m.index() + full.len();
        if !m.has(3) {
            let after = after_left + p2.len();
            let dash = quote_left_dash(text, after_left, cx);
            if dash.is_some() && (m.has(5) || dash_quote_at(text, after, dash, cx)) {
                let (role, depth, _) = plan.quote_at(text, cx, after - 1)?;
                if depth == 0 || role == OPENS {
                    parts.extend_from_slice(js::slice(text, cursor, after_left));
                    parts.push(FIRS_OPEN);
                    cursor = after + m.s(5).len();
                }
            }
            continue;
        }
        let (role, depth, pair_end) = plan.quote_at(text, cx, after_left + p2.len() - 1)?;

        if let Some(pair_end) = pair_end {
            parts.extend_from_slice(js::slice(text, cursor, m.index()));
            parts.extend_from_slice(p1);
            parts.push(FIRS_OPEN);
            parts.push(FIRS_CLOSE);
            cursor = pair_end;
            last_index = cursor;
            continue;
        }

        let opens_nested = depth > 0 && p1 != [FIRS_OPEN] && role == OPENS;

        let quote = after_left + p2.len() - 1;
        if !could_open(text, quote, cx, quote + 1, None)
            || (!opens_nested && (depth > 0 || closes_before_paren(p1, p4, m.index(), text)))
        {
            let after_quote = after_left + p2.len();
            parts.extend_from_slice(js::slice(text, cursor, after_quote));
            cursor = after_quote;
            last_index = after_quote;
            continue;
        }

        parts.extend_from_slice(js::slice(text, cursor, m.index()));
        parts.extend_from_slice(p1);
        parts.push(FIRS_OPEN);
        parts.extend_from_slice(p4);
        cursor = m.index() + full.len();
        last_index = cursor;
    }
    parts.extend_from_slice(js::from(text, cursor));
    Ok(parts)
}

static ADDRESS_THEN_RE: LazyLock<Re> =
    LazyLock::new(|| Re::new(&format!(r"({})(?:<\/{OPEN}A\d+{CLOSE}>)?(.?)", *LINKABLE_SOURCE), "gsu"));

fn before_link_end(buf: &[u16], at: usize) -> usize {
    let at_i = at as isize;
    let get = |i: isize| js::at(buf, i);
    if get(at_i - 1) != Some(GT) || get(at_i - 2) != Some(CLOSE_UNIT) {
        return at;
    }
    let mut i = at_i - 3;
    while is_digit(get(i)) {
        i -= 1;
    }
    let is_link_end =
        get(i) == Some(0x41) && get(i - 1) == Some(OPEN_UNIT) && get(i - 2) == Some(0x2F) && get(i - 3) == Some(0x3C);
    if is_link_end { (i - 3) as usize } else { at }
}

static END_TAG_THEN_QUOTE_RE: LazyLock<Re> = LazyLock::new(|| {
    Re::new(&format!(r#"(<\/{OPEN}[{TRANSPARENT_TAG_KINDS}]\d+{CLOSE}>)(["{LAQUO}{RAQUO}{BDQUO}{LDQUO}])"#), "gu")
});

#[derive(Default)]
pub struct AuthoredClosers {
    addresses: HashSet<JsString>,
    end_tags: HashSet<JsString>,
}

fn kept_where_always_closed(text: &[u16], re: &Re) -> HashSet<JsString> {
    let mut closed = HashSet::new();
    let mut other = HashSet::new();
    for m in re.find_all(text) {
        let name = m.s(1).to_vec();
        if m.s(2) == [FIRS_CLOSE] {
            closed.insert(name);
        } else {
            other.insert(name);
        }
    }
    for name in &other {
        closed.remove(name);
    }
    closed
}

fn authored_closers_in(text: &[u16]) -> AuthoredClosers {
    if !text.contains(&FIRS_CLOSE) {
        return AuthoredClosers::default();
    }
    AuthoredClosers {
        addresses: kept_where_always_closed(text, &ADDRESS_THEN_RE),
        end_tags: kept_where_always_closed(text, &END_TAG_THEN_QUOTE_RE),
    }
}

fn kept_closer(closers: &AuthoredClosers, buf: &[u16], at: usize) -> bool {
    if let Some(tag) = inline_end_tag_start(buf, at)
        && closers.end_tags.contains(&buf[tag..at])
    {
        return true;
    }
    let longest = closers.addresses.iter().map(Vec::len).max().unwrap_or(0);
    let end = before_link_end(buf, at);
    let before = &buf[end.saturating_sub(longest)..end];
    closers.addresses.iter().any(|address| before.ends_with(address))
}

static MAY_STRIP_PADDING_RE: LazyLock<Re> = LazyLock::new(|| Re::new(&NOT_AFTER_LONE_QUOTE, "uy"));

fn preserved_quote(glyph: Option<u16>) -> bool {
    glyph.is_some_and(|g| matching_quote(g).is_some() || g == FIRS_CLOSE || g == RDQUO_U || g == QUOT)
}

fn single_glyph(glyph: Option<JsString>) -> Option<u16> {
    glyph.filter(|g| g.len() == 1).map(|g| g[0])
}

#[derive(Default)]
pub struct QuoteSession {
    pub padding_strip: bool,
    pub open: bool,
    pub close: bool,
    pub read_foot: bool,
    reference_pairs: Option<KnownClosers>,
    spaced_closers: Option<KnownClosers>,
}

impl QuoteSession {
    pub fn forget_text_identity(&mut self) {
        for known in [&mut self.reference_pairs, &mut self.spaced_closers].into_iter().flatten() {
            known.same.forget();
        }
    }
}

struct KnownClosers {
    text: JsString,
    same: SameText,
    closing: HashSet<usize>,
}

impl KnownClosers {
    fn new(text: JsString, closing: HashSet<usize>) -> Self {
        KnownClosers { text, same: SameText::default(), closing }
    }

    fn holds(&mut self, text: &[u16]) -> bool {
        self.same.holds(&self.text, text)
    }
}

const UNSET: u16 = u16::MAX;
const AMP: u16 = 0x26;

#[derive(Clone)]
struct Tok {
    at: usize,
    end: usize,
    glyph: u16,
    preserved: bool,
    closer: Option<u16>,
    partner: Option<usize>,
    outer: Option<usize>,
    mixed: Option<usize>,
    next: Option<usize>,
    previous: Option<usize>,
    following: Option<usize>,
    enclosing: Option<usize>,
    after: Option<usize>,
    follower: u16,
    run_end: Option<usize>,
    content_after_run: bool,
    empty: bool,
    after_titles: Option<Option<usize>>,
    measure_digit: Option<usize>,
    measure_at: Option<usize>,
    follows_feet: Option<bool>,
}

impl Tok {
    fn new(at: usize, end: usize, glyph: u16, preserved: bool) -> Self {
        Tok {
            at,
            end,
            glyph,
            preserved,
            closer: Some(UNSET),
            partner: None,
            outer: None,
            mixed: None,
            next: None,
            previous: None,
            following: None,
            enclosing: None,
            after: None,
            follower: 0x20,
            run_end: None,
            content_after_run: false,
            empty: false,
            after_titles: None,
            measure_digit: None,
            measure_at: None,
            follows_feet: None,
        }
    }
}

trait Oracle {
    fn may_open(&mut self, t: &[Tok], i: usize, after_low: bool) -> Result<bool>;
    fn opens_within(&mut self, t: &[Tok], i: usize) -> Result<bool>;
}

fn pair_list(t: &mut [Tok], list: &[usize], visible_roles: bool, oracle: &mut dyn Oracle, v: Visit<'_>) -> Result<()> {
    let roles = if visible_roles {
        None
    } else {
        let mut roles = Vec::with_capacity(list.len());
        for &k in list {
            vt(v)?;
            roles.push(t[k].glyph);
        }
        let view: &[Tok] = t;
        resolve_authored_quote_pairs(&mut roles, &mut |_, i, after_low| oracle.may_open(view, i, after_low), true, v)?;
        Some(roles)
    };
    let mut open: Vec<usize> = Vec::new();
    for (i, &k) in list.iter().enumerate() {
        vt(v)?;
        let glyph = t[k].glyph;
        let top = open.last().copied();
        let straight = glyph == QUOT;
        let contextual = straight || (roles.is_none() && glyph == LDQUO_U);
        let closes = if contextual {
            top.is_some_and(|top| t[top].closer == Some(glyph)) && !oracle.opens_within(t, i)?
        } else {
            roles.as_ref().and_then(|r| r.get(i)) == Some(&FIRS_CLOSE)
        };
        t[k].closer = matching_quote(glyph);
        if closes {
            t[k].closer = None;
        } else if straight && oracle.may_open(t, i, false)? {
            t[k].closer = Some(glyph);
        }
        t[k].partner = None;
        t[k].outer = if t[k].closer.is_none() { None } else { top };
        if t[k].closer.is_some() {
            open.push(k);
        } else if let Some(top) = top
            && t[top].closer == Some(glyph)
        {
            t[k].partner = Some(top);
            t[top].partner = Some(k);
            open.pop();
        }
    }
    Ok(())
}

fn has_opening_content_beyond_quotes(
    text: &[u16],
    from: usize,
    cx: &mut QuoteTags<'_>,
    content: &mut HashMap<usize, bool>,
    v: Visit<'_>,
) -> Result<bool> {
    let mut at = from;
    let mut run = Vec::new();
    while !content.contains_key(&at) {
        vt(v)?;
        run.push(at);
        at = cx.text_start(text, at);
        if text.get(at) == Some(&QUOT) {
            at += 1;
            continue;
        }
        if text.get(at) == Some(&AMP)
            && let Some(reference) = read_character_reference_at(text, at)
            && preserved_quote(single_glyph(authored_reference_glyph(Some(&reference), true)))
        {
            at = reference.end;
            continue;
        }
        let opens = opening_content_at(text, at, cx, false)?
            && opening_quote_neighbours(Some(SEMICOLON), unit_at(text, Some(at), 0x20));
        content.insert(at, opens);
    }
    let result = content[&at];
    for offset in run {
        vt(v)?;
        content.insert(offset, result);
    }
    Ok(result)
}

fn closes_content(text: &[u16], t: &[Tok], k: usize, cx: &mut QuoteTags<'_>) -> bool {
    if t[k].glyph != QUOT || t[k].previous.is_some_and(|p| t[p].closer.is_none()) {
        return true;
    }
    let before = cx.text_end(text, t[k].at);
    before != 0 && CLOSING_NEIGHBOUR_RE.exec_at(text, before).is_some()
}

#[derive(Clone, Copy)]
struct Inch {
    reads: bool,
    padded: bool,
}

impl Inch {
    fn at(self, text: &[u16], t: &[Tok], k: usize) -> bool {
        self.reads && digit_before_quote(text, t[k].at, self.padded).is_some()
    }
}

#[allow(clippy::too_many_arguments)]
fn index_mixed_quote_pairs(
    text: &[u16],
    cx: &mut QuoteTags<'_>,
    t: &mut [Tok],
    ordered: &[usize],
    reads_inches: bool,
    padded: bool,
    inch: Inch,
    v: Visit<'_>,
) -> Result<()> {
    let mut frame: Option<usize> = None;
    let mut directed: Vec<usize> = Vec::new();
    for &k in ordered {
        vt(v)?;
        if t[k].next.is_some_and(|n| t[n].glyph == QUOT) {
            t[k].enclosing = frame;
        }
        let finishes = !opening_quote_neighbours(Some(SEMICOLON), t[k].follower)
            || t[k].next.is_some_and(|n| t[n].closer.is_some());
        let mut turned = !t[k].preserved
            && t[k].glyph == LDQUO_U
            && t[k].partner.is_none()
            && if frame.is_some_and(|f| t[f].glyph == QUOT) {
                finishes
            } else {
                (frame.is_some_and(|f| t[f].closer == Some(LDQUO_U)) && finishes)
                    || (!t[t[k].run_end.unwrap_or(k)].content_after_run
                        && t[k].next.is_none_or(|n| t[n].glyph != QUOT && t[n].glyph != RDQUO_U))
            };
        if turned
            && let Some(n) = t[k].next
            && !t[n].preserved
            && touching_closers(text, cx, t, k, n, inch)?.is_some()
        {
            turned = false;
        }
        if turned {
            t[k].closer = None;
        }
        let mut owner = None;
        if !t[k].preserved && t[k].glyph != QUOT {
            if t[k].closer.is_some() {
                directed.push(k);
            } else if directed.last().is_some_and(|&d| t[d].closer == Some(t[k].glyph)) {
                owner = directed.pop();
            }
        }
        if t[k].closer.is_some() {
            t[k].outer = frame;
            frame = Some(k);
        } else if let Some(p) = t[k].partner {
            frame = t[p].outer;
        } else if let Some(o) = owner
            && t[o].partner.is_none()
        {
            if let Some(m) = t[o].mixed {
                t[m].mixed = None;
            }
            t[o].mixed = Some(k);
            t[k].mixed = Some(o);
            frame = t[o].outer;
        } else if let Some(f) = frame
            && owner.is_none()
            && t[f].mixed.is_none_or(|m| matching_quote(t[f].glyph) != Some(t[m].glyph))
            && !t[k].preserved
            && !t[f].preserved
            && if t[k].glyph == QUOT {
                t[f].partner.is_none()
                    && t[f].glyph != QUOT
                    && (!reads_inches || digit_before_quote(text, t[k].at, padded).is_none())
            } else {
                t[f].glyph == QUOT || (t[f].partner.is_none() && (turned || t[f].closer == Some(t[k].glyph)))
            }
        {
            t[f].mixed = Some(k);
            t[k].mixed = Some(f);
            frame = t[f].outer;
        } else {
            frame = None;
        }
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn pair_empty_at_boundary(
    text: &[u16],
    cx: &mut QuoteTags<'_>,
    t: &mut [Tok],
    first: Option<usize>,
    second: Option<usize>,
    boundary: usize,
    reads_inches: bool,
    padded: bool,
) -> Result<bool> {
    let partner = t[boundary].partner.or(t[boundary].mixed);
    let (Some(first), Some(second), Some(partner)) = (first, second, partner) else {
        return Ok(false);
    };
    let (f, s) = (&t[first], &t[second]);
    if (f.glyph != QUOT && (s.glyph != QUOT || f.closer.is_none()))
        || (s.glyph != QUOT && s.closer.is_some() && s.glyph != LDQUO_U)
        || f.preserved
        || s.preserved
        || f.end != s.at
        || f.partner.is_some_and(|p| p != second)
        || s.partner.is_some_and(|p| p != first)
        || f.mixed.is_some_and(|m| m != second)
        || s.mixed.is_some_and(|m| m != first)
    {
        return Ok(false);
    }
    if (t[boundary].previous != Some(second)
        && opening_content_start(text, t[second].end, cx, false)? != Some(t[boundary].at))
        || node_between(cx, text, t[second].end, t[boundary].at)
    {
        return Ok(false);
    }
    if t[boundary].closer.is_none() {
        if t[partner].at >= t[first].at
            || matching_quote(t[partner].glyph) != Some(t[boundary].glyph)
            || (reads_inches && digit_before_quote(text, t[first].at, padded).is_some())
        {
            return Ok(false);
        }
    } else if t[first].closer.is_none()
        || (t[boundary].glyph == QUOT && t[partner].glyph == QUOT && t[boundary].next == Some(partner))
    {
        return Ok(false);
    }
    t[boundary].partner = Some(partner);
    t[partner].partner = Some(boundary);
    mark_empty_value(t, first, second);
    Ok(true)
}

fn mark_empty_value(t: &mut [Tok], first: usize, second: usize) {
    t[first].partner = Some(second);
    t[second].partner = Some(first);
    t[first].closer = Some(matching_quote(t[first].glyph).unwrap_or(t[first].glyph));
    t[second].closer = None;
    t[first].empty = true;
    t[second].empty = true;
}

fn starts_with_empty_quotation(t: &[Tok], k: usize) -> bool {
    let Some(open) = t[k].next else { return false };
    let Some(close) = t[open].next else { return false };
    !t[open].preserved && !t[close].preserved && matching_quote(t[open].glyph) == Some(t[close].glyph)
}

fn after_completed_titles(t: &mut [Tok], first: Option<usize>, v: Visit<'_>) -> Result<Option<usize>> {
    let mut seen = Vec::new();
    let mut token = first;
    while let Some(k) = token
        && let Some(partner) = t[k].partner
        && t[k].closer.is_some()
        && !t[k].preserved
    {
        vt(v)?;
        if let Some(after) = t[k].after_titles {
            token = after;
            break;
        }
        seen.push(k);
        token = t[partner].following;
    }
    for title in seen {
        vt(v)?;
        t[title].after_titles = Some(token);
    }
    Ok(token)
}

fn free_inside_frame(t: &[Tok], owner: Option<usize>, end: usize) -> bool {
    owner.is_none_or(|o| t[o].at > t[end].at && t[o].glyph == QUOT && !t[o].preserved)
}

fn keep_enclosed_empty_values(
    text: &[u16],
    t: &mut [Tok],
    k: usize,
    pending: &mut Vec<Option<usize>>,
    v: Visit<'_>,
) -> Result<()> {
    let Some(title) = t[k].partner else { return Ok(()) };
    let Some(outer) = t[title].outer else { return Ok(()) };
    let Some(mut end) = t[outer].partner.or(t[outer].mixed) else { return Ok(()) };
    if t[outer].preserved
        || t[end].preserved
        || t[title].preserved
        || t[k].preserved
        || t[title].next == Some(k)
        || matching_quote(t[outer].glyph).unwrap_or(t[outer].glyph) != t[end].glyph
        || t[end].at <= t[k].at
    {
        return Ok(());
    }
    let mut displaced = None;
    let sibling = t[k].run_end.unwrap_or(k);
    if t[end].at < t[sibling].at && !t[sibling].preserved && t[sibling].closer.is_some() && t[sibling].partner.is_some()
    {
        let later = after_completed_titles(t, Some(sibling), v)?;
        if let Some(later) = later
            && !t[later].preserved
            && t[later].partner.is_none()
            && t[later].mixed.is_none()
            && t[later].glyph == t[end].glyph
            && !(t[later].closer.is_some() && t[later].run_end.is_some_and(|r| t[r].content_after_run))
        {
            displaced = Some(end);
            end = later;
        }
    }
    let mut free = 0usize;
    let mut last = k;
    let mut next = t[last].next;
    while let Some(n) = next
        && t[last].end == t[n].at
        && n != end
        && !t[n].preserved
        && (Some(n) == displaced || (free_inside_frame(t, t[n].partner, end) && free_inside_frame(t, t[n].mixed, end)))
    {
        vt(v)?;
        free += 1;
        last = n;
        next = t[last].next;
    }
    let blocked = match next {
        Some(n) => {
            n != end && (t[n].preserved || t[n].closer.is_none() || t[n].partner.is_none_or(|p| t[p].at >= t[end].at))
        }
        None => opening_quote_neighbours(Some(SEMICOLON), unit_at(text, Some(t[last].end), 0x20)),
    };
    if free < 2 || !free.is_multiple_of(2) || blocked {
        return Ok(());
    }
    if let Some(d) = displaced {
        t[d].mixed = None;
        t[outer].mixed = None;
    }
    t[outer].partner = Some(end);
    t[end].partner = Some(outer);
    t[end].closer = None;
    pending.push(Some(outer));
    pending.push(Some(k));
    let mut left = t[k].next;
    while left != next {
        vt(v)?;
        let Some(l) = left else { break };
        let Some(ln) = t[l].next else { break };
        release_later_partners(t, l, end);
        release_later_partners(t, ln, end);
        t[l].mixed = None;
        t[ln].mixed = None;
        mark_empty_value(t, l, ln);
        pending.push(Some(l));
        left = t[ln].next;
    }
    Ok(())
}

static VISIBLE_CONTENT_END_RE: LazyLock<Re> =
    LazyLock::new(|| Re::new(&format!("(?<={})", TITLE_CONTENT_CHAR.source()), "uy"));

fn has_title_content(
    text: &[u16],
    cx: &mut QuoteTags<'_>,
    t: &[Tok],
    open: usize,
    end: usize,
    v: Visit<'_>,
) -> Result<bool> {
    let mut at = opening_content_start(text, t[open].end, cx, false)?;
    let mut outer_end = end;
    let mut child = t[open].following;
    while let Some(c) = child
        && Some(t[c].at) == at
        && c != outer_end
    {
        vt(v)?;
        let Some(partner) = t[c].partner else { return Ok(false) };
        if t[c].closer.is_none() || t[partner].at >= t[outer_end].at {
            return Ok(false);
        }
        outer_end = partner;
        at = opening_content_start(text, t[c].end, cx, false)?;
        child = t[c].following;
    }
    let Some(at) = at else { return Ok(false) };
    if at >= t[outer_end].at
        || (quotation_mark(text.get(at).copied())
            && (at + 1 >= t[outer_end].at || !opening_content_at(text, at + 1, cx, false)?))
    {
        return Ok(false);
    }
    let before = cx.text_end(text, t[end].at);
    Ok(VISIBLE_CONTENT_END_RE.exec_at(text, before).is_some())
}

fn belongs_within(t: &[Tok], k: usize, first: usize, second: usize, end: usize) -> bool {
    let (partner, mixed) = (t[k].partner, t[k].mixed);
    (partner == Some(first) || partner == Some(second) || free_inside_frame(t, partner, end))
        && (mixed == Some(first) || mixed == Some(second) || free_inside_frame(t, mixed, end))
}

fn release_later_partners(t: &mut [Tok], k: usize, end: usize) {
    if let Some(p) = t[k].partner
        && t[p].at > t[end].at
        && t[p].partner == Some(k)
    {
        t[p].partner = None;
    }
    if let Some(m) = t[k].mixed
        && t[m].at > t[end].at
        && t[m].mixed == Some(k)
    {
        t[m].mixed = None;
    }
}

fn glued_to_word(text: &[u16], t: &[Tok], k: usize) -> bool {
    if t[k].glyph != QUOT {
        return false;
    }
    let after = t[k].after.unwrap_or(t[k].end);
    if text.get(after) == Some(&AMP) {
        TITLE_WORD_CHAR.test(&[t[k].follower])
    } else {
        TITLE_WORD_CHAR.test(scalar_at(text, after))
    }
}

fn can_close(glyph: u16) -> bool {
    glyph != FIRS_OPEN && glyph != BDQUO_U
}

fn punctuation_gap(gap: &[u16]) -> bool {
    !gap.is_empty() && gap.iter().all(|&c| matches!(c, 0x2E | 0x2C | 0x3B | 0x3A | 0x21 | 0x3F) || c == unit(HELLIP))
}

fn closes_last(text: &[u16], t: &[Tok], outer: usize, closer: usize) -> bool {
    let own = t[outer].partner.or(t[outer].mixed);
    if own.is_some_and(|o| o != closer)
        || t[closer].following.is_some()
        || !matches!(text.get(t[closer].end), Some(0x20 | 0x09))
    {
        return false;
    }
    let start = t[closer].at;
    let mut at = start;
    while at > 0 && matches!(text[at - 1], 0x20 | 0x09) {
        at -= 1;
    }
    at < start && at > 0 && WORD_END_RE.test(&text[at - 1..at])
}

fn nest_touching_closers(
    text: &[u16],
    cx: &mut QuoteTags<'_>,
    t: &mut [Tok],
    outer: usize,
    inner: usize,
    inch: Inch,
) -> Result<bool> {
    let closers = touching_closers(text, cx, t, outer, inner, inch)?;
    if let Some((first, second)) = closers {
        nest(t, outer, inner, first, second);
    }
    Ok(closers.is_some())
}

fn touching_closers(
    text: &[u16],
    cx: &mut QuoteTags<'_>,
    t: &[Tok],
    outer: usize,
    inner: usize,
    inch: Inch,
) -> Result<Option<(usize, usize)>> {
    if !opening_content_at(text, t[inner].after.unwrap_or(t[inner].end), cx, false)? {
        return Ok(None);
    }
    let own = t[inner].partner.or(t[inner].mixed);
    let enclosing = t[outer].partner.or(t[outer].mixed);
    let run = t[inner].following;
    let own_glyph = own.map(|o| t[o].glyph);
    let skipped = enclosing.or(
        if matching_quote(t[inner].glyph) == own_glyph
            || (t[inner].glyph == QUOT && own_glyph == Some(QUOT) && !own.is_some_and(|o| inch.at(text, t, o)))
        {
            run
        } else {
            None
        },
    );
    let passed = match (skipped, own) {
        (Some(s), Some(o)) if t[o].at > t[s].at => (Some(s), Some(o)),
        _ => (None, None),
    };
    let after =
        run.and_then(|r| t[r].next.or(t[r].following.filter(|&f| punctuation_gap(js::slice(text, t[r].end, t[f].at)))));
    let left = if enclosing.is_some() || t[outer].glyph == LDQUO_U {
        (None, None)
    } else {
        (own, own.and_then(|o| t[o].following))
    };
    for (first, second) in [(own, enclosing), passed, (run, after), left] {
        let (Some(first), Some(second)) = (first, second) else { continue };
        if t[first].at < t[second].at
            && t[inner].next != Some(first)
            && !t[first].preserved
            && !t[second].preserved
            && t[second].closer.is_none()
            && !glued_to_word(text, t, first)
            && !glued_to_word(text, t, second)
            && closes_content(text, t, first, cx)
            && (closes_content(text, t, second, cx) || closes_last(text, t, outer, second))
            && (t[first].closer.is_none()
                || (Some(first) == run && t[first].partner.or(t[first].mixed) == Some(second)))
            && can_close(t[first].glyph)
        {
            return Ok(Some((first, second)));
        }
    }
    Ok(None)
}

fn nest(t: &mut [Tok], outer: usize, inner: usize, first: usize, second: usize) {
    for k in [outer, inner, first, second] {
        if let Some(p) = t[k].partner
            && t[p].partner == Some(k)
        {
            t[p].partner = None;
        }
    }
    t[outer].partner = Some(second);
    t[second].partner = Some(outer);
    t[inner].partner = Some(first);
    t[first].partner = Some(inner);
    t[first].closer = None;
}

fn connect_titles(
    t: &mut [Tok],
    first: usize,
    previous: usize,
    k: usize,
    last: usize,
    pending: &mut Vec<Option<usize>>,
) {
    if let Some(p) = t[first].partner
        && p != previous
    {
        t[p].partner = None;
    }
    t[previous].partner = Some(first);
    t[first].partner = Some(previous);
    t[k].partner = Some(last);
    t[last].partner = Some(k);
    pending.push(Some(k));
    pending.push(Some(previous));
}

fn pair_content_titles(
    text: &[u16],
    cx: &mut QuoteTags<'_>,
    t: &mut [Tok],
    right: usize,
    padded: bool,
    pending: &mut Vec<Option<usize>>,
    v: Visit<'_>,
) -> Result<bool> {
    let Some(left) = t[right].previous else { return Ok(false) };
    let first = match t[left].partner {
        Some(p) if t[p].at < t[left].at => Some(p),
        _ => t[left].enclosing,
    };
    let Some(first) = first else { return Ok(false) };
    if t[first].preserved
        || t[left].preserved
        || t[first].at >= t[left].at
        || (t[left].glyph != QUOT && t[left].glyph != LDQUO_U)
        || t[left].end != t[right].at
        || (t[left].glyph == QUOT && digit_before_quote(text, t[left].at, padded).is_some())
    {
        return Ok(false);
    }
    let following = t[right].following;
    let Some(end) = after_completed_titles(t, following, v)? else { return Ok(false) };
    if t[end].preserved
        || t[left].partner.is_some_and(|p| t[p].at > t[left].at && t[p].at <= t[end].at)
        || matching_quote(t[first].glyph) != Some(t[end].glyph)
        || TITLE_WORD_CHAR.test(&quote_follower_glyph(text, t[end].end, cx))
        || !belongs_within(t, first, left, end, end)
        || !belongs_within(t, left, first, right, end)
        || !belongs_within(t, right, left, end, end)
        || !belongs_within(t, end, right, first, end)
        || !has_title_content(text, cx, t, first, left, v)?
        || !has_title_content(text, cx, t, right, end, v)?
    {
        return Ok(false);
    }
    for k in [first, left, right, end] {
        release_later_partners(t, k, end);
    }
    t[first].mixed = None;
    t[left].mixed = None;
    t[left].closer = None;
    t[right].mixed = None;
    t[right].closer = Some(t[right].glyph);
    t[end].mixed = None;
    t[end].closer = None;
    connect_titles(t, first, left, right, end, pending);
    Ok(true)
}

fn has_authored_partner(t: &[Tok], k: usize) -> bool {
    let Some(peer) = t[k].partner else { return false };
    if t[peer].partner != Some(k) {
        return false;
    }
    let (first, last) = if t[k].at < t[peer].at { (k, peer) } else { (peer, k) };
    let closer = if t[first].glyph == QUOT { Some(QUOT) } else { matching_quote(t[first].glyph) };
    t[first].closer.is_some() && t[last].closer.is_none() && closer == Some(t[last].glyph)
}

fn measured_feet(
    text: &[u16],
    cx: &mut QuoteTags<'_>,
    read_foot: bool,
    t: &mut [Tok],
    k: usize,
    v: Visit<'_>,
) -> Result<bool> {
    if let Some(feet) = t[k].follows_feet {
        return Ok(feet);
    }
    let feet = follows_feet(text, t[k].measure_digit.unwrap_or(0), cx, v, read_foot)?;
    t[k].follows_feet = Some(feet);
    Ok(feet)
}

#[derive(Clone, Default)]
struct Speech {
    slot: PairSlot,
    left: usize,
    right: usize,
    depth: i64,
}

impl QuoteToken for Speech {
    fn slot(&self) -> &PairSlot {
        &self.slot
    }
    fn slot_mut(&mut self) -> &mut PairSlot {
        &mut self.slot
    }
}

type Snapshot = Rc<Vec<(u16, Option<usize>, Option<u16>)>>;

#[derive(Clone)]
struct SpeechRange {
    end: usize,
    pairs: Snapshot,
}

#[allow(clippy::too_many_arguments)]
fn directed_may_open(
    text: &[u16],
    cx: &mut QuoteTags<'_>,
    t: &[Tok],
    tokens: &[usize],
    roles: &[u16],
    index: usize,
    after_low: bool,
) -> Result<bool> {
    let k = tokens[index];
    let left = (index > 0 && t[tokens[index - 1]].end == t[k].at).then(|| Some(roles[index - 1]));
    Ok(opening_content_at(text, t[k].end, cx, false)? && authored_opening_at(text, t[k].at, after_low, cx, left)?)
}

struct Speeches<'s> {
    ranges: &'s mut [Option<SpeechRange>],
    complete: &'s mut [isize],
}

#[allow(clippy::too_many_arguments)]
fn index_directed_speech(
    text: &[u16],
    cx: &mut QuoteTags<'_>,
    read_foot: bool,
    t: &mut [Tok],
    tokens: &[usize],
    found: &mut Speeches<'_>,
    require_complete_pairs: bool,
    measures: Option<&HashSet<usize>>,
    nested_neutral: bool,
    v: Visit<'_>,
) -> Result<()> {
    let n = tokens.len();
    let mut roles = Vec::with_capacity(n);
    let mut pairs: Vec<Speech> = Vec::with_capacity(n);
    let mut positions = HashMap::new();
    for (i, &k) in tokens.iter().enumerate() {
        vt(v)?;
        if measures.is_some() {
            positions.insert(k, i);
        }
        let glyph = if measures.is_some_and(|m| m.contains(&k)) { PRIME_U } else { t[k].glyph };
        roles.push(glyph);
        pairs.push(Speech { slot: PairSlot { glyph, ..PairSlot::default() }, ..Speech::default() });
    }
    {
        let view: &[Tok] = t;
        let cell = RefCell::new(&mut *cx);
        resolve_authored_quote_pairs(
            &mut roles,
            &mut |buf, i, after_low| directed_may_open(text, &mut cell.borrow_mut(), view, tokens, buf, i, after_low),
            require_complete_pairs,
            v,
        )?;
        let roles: &[u16] = &roles;
        pair_quote_tokens(
            &mut pairs,
            Some(roles),
            &mut |_, i, after_low| directed_may_open(text, &mut cell.borrow_mut(), view, tokens, roles, i, after_low),
            Some(&mut |p: &[Speech], i| {
                let k = tokens[i];
                let ready = match view[k].previous {
                    None => true,
                    Some(previous) => {
                        nested_neutral
                            && view[previous].glyph == QUOT
                            && p[i - 1].slot.closer.is_some()
                            && TITLE_WORD_CHAR.test(&[view[k].follower])
                    }
                };
                Ok(ready && directed_may_open(text, &mut cell.borrow_mut(), view, tokens, roles, i, false)?)
            }),
            v,
        )?;
    }
    let mut last_unresolved: isize = -1;
    let mut last_german: isize = -1;
    let mut last_english: isize = -1;
    let mut last_measure: isize = -1;
    let mut last_directed: isize = -1;
    let mut last_neutral_cluster: isize = -1;
    let mut last_unpaired_source: isize = -1;
    let mut last_quoted_title: isize = -1;
    let mut last_numeric_pair: isize = -1;
    let mut last_reassigned: isize = -1;
    let mut frame: Option<usize> = None;
    let mut snapshot: Option<Snapshot> = None;
    for i in 0..n {
        vt(v)?;
        let at = i as isize;
        let k = tokens[i];
        if t[k].glyph == QUOT
            && t[k].previous.is_some_and(|p| t[p].glyph == QUOT && t[p].previous.is_some_and(|pp| t[pp].glyph == QUOT))
        {
            last_neutral_cluster = at;
        }
        let owner = t[k].partner;
        let authored = measures.is_some() && has_authored_partner(t, k);
        if measures.is_some() && !authored {
            last_unpaired_source = at;
        }
        let endpoint = match owner {
            Some(o)
                if measures.is_some()
                    || matching_quote(t[k].glyph) == Some(t[o].glyph)
                    || matching_quote(t[o].glyph) == Some(t[k].glyph) =>
            {
                t[o].at
            }
            _ => t[k].at,
        };
        if pairs[i].slot.glyph == PRIME_U {
            last_measure = at;
            if authored
                && let Some(o) = owner
                && t[o].at > t[k].at
                && t[k].next != Some(o)
            {
                if measures.is_some_and(|m| m.contains(&o)) {
                    if !measured_feet(text, cx, read_foot, t, k, v)? && !measured_feet(text, cx, read_foot, t, o, v)? {
                        last_numeric_pair = at;
                    }
                } else if opening_content_at(text, t[k].end, cx, false)? {
                    last_quoted_title = at;
                }
            }
            if let Some(f) = frame {
                pairs[f].left = pairs[f].left.min(endpoint);
                pairs[f].right = pairs[f].right.max(endpoint);
                if let Some(alternate) = t[k].mixed
                    && t[alternate].mixed == Some(k)
                    && let Some(alt_outer) = t[alternate].outer
                    && (t[alt_outer].partner.is_none()
                        || positions.get(&alt_outer).is_none_or(|&p| pairs[p].slot.partner.is_none()))
                {
                    pairs[f].left = pairs[f].left.min(t[alt_outer].at);
                }
                if authored
                    && let Some(o) = owner
                    && t[o].at < t[k].at
                    && let Some(owner_outer) = t[o].outer
                    && t[owner_outer].partner.is_none()
                {
                    pairs[f].left = pairs[f].left.min(t[owner_outer].at);
                }
            }
            continue;
        }
        let pair_partner = pairs[i].slot.partner;
        let partner_token = pair_partner.map(|p| tokens[p]);
        if measures.is_some() && (owner.is_none() || owner != partner_token) {
            last_reassigned = at;
        }
        let unresolved = match pair_partner {
            None => true,
            Some(_) => {
                ((t[k].preserved || owner.is_some_and(|o| t[o].preserved)) && owner != partner_token)
                    || (pairs[i].slot.closer.is_some()
                        && (!opening_content_at(text, t[k].end, cx, false)?
                            || (t[k].previous.is_none() && !directed_may_open(text, cx, t, tokens, &roles, i, false)?)))
            }
        };
        if unresolved {
            last_unresolved = at;
        }
        if pairs[i].slot.glyph == BDQUO_U {
            last_german = at;
        }
        if pairs[i].slot.glyph == RDQUO_U {
            last_english = at;
        }
        if pairs[i].slot.closer.is_some() {
            frame = Some(i);
            pairs[i].left = endpoint;
            pairs[i].right = endpoint;
            pairs[i].depth = 1;
            continue;
        }
        let Some(opening) = pair_partner else { continue };
        frame = pairs[opening].slot.outer;
        let left = pairs[opening].left.min(endpoint);
        let right = pairs[opening].right.max(endpoint);
        if let Some(o) = pairs[opening].slot.outer {
            pairs[o].left = pairs[o].left.min(left);
            pairs[o].right = pairs[o].right.max(right);
            pairs[o].depth = pairs[o].depth.max(pairs[opening].depth + 1);
        }
        let start = opening as isize;
        if pairs[opening].slot.glyph != QUOT {
            last_directed = last_directed.max(start);
        }
        if start <= last_unresolved || t[tokens[opening]].at > left || right > t[k].at {
            continue;
        }
        if measures.is_none() {
            found.complete[opening] = found.complete[opening].max(at);
        } else {
            let parent = t[tokens[opening]].outer;
            let retains_title = start <= last_quoted_title || (start <= last_numeric_pair && start > last_reassigned);
            if start > last_measure
                || (retains_title && start > last_unpaired_source && parent.is_none_or(|p| t[p].partner.is_some()))
            {
                continue;
            }
        }
        let mixed = start <= last_german && start <= last_english;
        let measured = measures.is_some()
            && t[tokens[opening]].closer.is_some()
            && start <= last_directed
            && start > last_neutral_cluster
            && pairs[opening].depth > 1
            && found.complete[opening] < at;
        if (mixed || measured) && found.ranges[opening].as_ref().is_none_or(|r| r.end < i) {
            let pairs_now = snapshot
                .get_or_insert_with(|| {
                    Rc::new(pairs.iter().map(|p| (p.slot.glyph, p.slot.partner, p.slot.closer)).collect())
                })
                .clone();
            found.ranges[opening] = Some(SpeechRange { end: i, pairs: pairs_now });
        }
    }
    Ok(())
}

struct Measures {
    spare: HashSet<usize>,
    possible: HashSet<usize>,
}

fn speech_measurements(
    text: &[u16],
    cx: &mut QuoteTags<'_>,
    t: &mut [Tok],
    tokens: &[usize],
    padded: bool,
    v: Visit<'_>,
) -> Result<Measures> {
    let mut spare = HashSet::new();
    let mut possible = HashSet::new();
    for (i, &k) in tokens.iter().enumerate() {
        vt(v)?;
        if t[k].preserved || t[k].glyph == FIRS_OPEN || t[k].glyph == BDQUO_U {
            continue;
        }
        let Some(digit) = digit_before_quote(text, t[k].at, padded) else { continue };
        if LATIN_LETTER_RE.test(&[t[k].follower]) {
            continue;
        }
        t[k].measure_digit = Some(digit);
        let prior = i.checked_sub(1).map(|j| tokens[j]);
        let closer = match prior {
            Some(p) if t[p].glyph == QUOT => Some(QUOT),
            Some(p) => matching_quote(t[p].glyph),
            None => None,
        };
        let Some(p) = prior.filter(|_| closer == Some(t[k].glyph)) else {
            spare.insert(k);
            possible.insert(k);
            continue;
        };
        if t[k].partner != Some(p) && !authored_opening_at(text, t[p].at, false, cx, None)? {
            spare.insert(k);
            possible.insert(k);
            continue;
        }
        if let Some(&after) = tokens.get(i + 1)
            && Some(t[after].glyph) == closer
            && digit_before_quote(text, t[after].at, padded).is_none()
            && (t[after].previous == Some(k) || !authored_opening_at(text, t[after].at, false, cx, None)?)
        {
            possible.insert(k);
        }
    }
    Ok(Measures { spare, possible })
}

#[allow(clippy::too_many_arguments)]
fn complete_directed_speech(
    text: &[u16],
    cx: &mut QuoteTags<'_>,
    read_foot: bool,
    t: &mut [Tok],
    tokens: &[usize],
    pending: &mut Vec<Option<usize>>,
    reads_inches: bool,
    padded: bool,
    v: Visit<'_>,
) -> Result<()> {
    let mut english = false;
    let mut german = false;
    let mut nested_straight = false;
    for &k in tokens {
        vt(v)?;
        english = english || t[k].glyph == RDQUO_U;
        german = german || t[k].glyph == BDQUO_U;
        nested_straight = nested_straight
            || (t[k].glyph == QUOT
                && t[k].previous.is_some_and(|p| t[p].glyph == QUOT)
                && TITLE_WORD_CHAR.test(&[t[k].follower]));
        if english && german && nested_straight {
            break;
        }
    }
    let mixed = english && german;
    let measures = if reads_inches { Some(speech_measurements(text, cx, t, tokens, padded, v)?) } else { None };
    if !mixed && measures.as_ref().is_none_or(|m| m.possible.is_empty()) {
        return Ok(());
    }
    let n = tokens.len();
    let mut ranges: Vec<Option<SpeechRange>> = vec![None; n];
    let mut complete = vec![-1isize; n];
    let mut index = |cx: &mut QuoteTags<'_>, t: &mut [Tok], units: Option<&HashSet<usize>>, nested: bool| {
        let mut found = Speeches { ranges: &mut ranges, complete: &mut complete };
        index_directed_speech(text, cx, read_foot, t, tokens, &mut found, false, units, nested, v)?;
        index_directed_speech(text, cx, read_foot, t, tokens, &mut found, true, units, nested, v)
    };
    index(cx, t, None, false)?;
    if let Some(m) = &measures
        && !m.possible.is_empty()
    {
        if !m.spare.is_empty() && m.spare.len() < m.possible.len() {
            index(cx, t, Some(&m.spare), false)?;
        }
        index(cx, t, Some(&m.possible), false)?;
        if !m.spare.is_empty() && nested_straight {
            index(cx, t, Some(&m.spare), true)?;
        }
    }
    let mut i = 0;
    while i < n {
        vt(v)?;
        let Some(range) = ranges[i].clone() else {
            i += 1;
            continue;
        };
        for &k in &tokens[i..=range.end] {
            vt(v)?;
            if let Some(p) = t[k].partner
                && t[p].partner == Some(k)
            {
                t[p].partner = None;
            }
            if let Some(m) = t[k].mixed
                && t[m].mixed == Some(k)
            {
                t[m].mixed = None;
            }
        }
        for j in i..=range.end {
            vt(v)?;
            let k = tokens[j];
            let (glyph, partner, closer) = range.pairs[j];
            if glyph == PRIME_U {
                t[k].partner = None;
                t[k].mixed = None;
                t[k].measure_at = t[k].measure_digit.map(|d| d + 1);
                continue;
            }
            t[k].partner = partner.map(|p| tokens[p]);
            t[k].closer = closer;
            t[k].mixed = None;
            t[k].empty = t[k].partner.is_some() && (t[k].next == t[k].partner || t[k].previous == t[k].partner);
            pending.push(Some(k));
        }
        i = range.end + 1;
    }
    Ok(())
}

struct PreservedOracle<'o, 'c> {
    text: &'o [u16],
    cx: &'o mut QuoteTags<'c>,
    tokens: &'o [usize],
    content: &'o mut HashMap<usize, bool>,
    v: Visit<'o>,
}

impl Oracle for PreservedOracle<'_, '_> {
    fn may_open(&mut self, t: &[Tok], i: usize, _: bool) -> Result<bool> {
        let text = self.text;
        let k = self.tokens[i];
        let before = self.cx.text_end(text, t[k].at);
        let after = self.cx.text_start(text, t[k].end);
        if i > 0 && t[self.tokens[i - 1]].end == before {
            return has_opening_content_beyond_quotes(text, after, self.cx, self.content, self.v);
        }
        let previous = before.checked_sub(1).map(|b| text[b]);
        let right = unit_at(text, Some(after), 0x20);
        if preserved_quote(previous) {
            return Ok(opening_quote_neighbours(Some(SEMICOLON), right));
        }
        Ok(opening_quote_neighbours(previous, 0x20)
            || (opening_quote_neighbours(previous, right)
                && has_opening_content_beyond_quotes(text, after, self.cx, self.content, self.v)?))
    }

    fn opens_within(&mut self, t: &[Tok], i: usize) -> Result<bool> {
        Ok(i + 1 < self.tokens.len() && self.may_open(t, i, false)?)
    }
}

struct EditableOracle<'o, 'c> {
    text: &'o [u16],
    cx: &'o mut QuoteTags<'c>,
    editable: &'o [usize],
    reads_inches: bool,
    padded: bool,
    unpaired: Option<&'o HashSet<usize>>,
}

impl EditableOracle<'_, '_> {
    fn opens_editable(&mut self, t: &[Tok], i: usize) -> Result<bool> {
        let text = self.text;
        let k = self.editable[i];
        let before = self.cx.text_end(text, t[k].at);
        let left = if t[k].previous.is_some() { Some(SEMICOLON) } else { before.checked_sub(1).map(|b| text[b]) };
        let after = t[k].after.unwrap_or(t[k].end);
        if t[k].glyph == QUOT
            && t[k].previous.is_some_and(|p| t[p].closer.is_none())
            && t[k].next.is_none()
            && quotation_mark(text.get(after).copied())
            && !opening_content_at(text, after + 1, self.cx, false)?
        {
            return Ok(false);
        }
        Ok(opening_quote_neighbours(left, t[k].follower) && opening_content_at(text, after, self.cx, false)?)
    }
}

impl Oracle for EditableOracle<'_, '_> {
    fn may_open(&mut self, t: &[Tok], i: usize, _: bool) -> Result<bool> {
        self.opens_editable(t, i)
    }

    fn opens_within(&mut self, t: &[Tok], i: usize) -> Result<bool> {
        let text = self.text;
        let k = self.editable[i];
        if self.unpaired.is_some_and(|u| u.contains(&k)) {
            return Ok(false);
        }
        if t[k].glyph == LDQUO_U
            && t[k].next.is_some_and(|n| t[n].glyph == RDQUO_U)
            && let Some(p) = t[k].previous
            && t[p].glyph == QUOT
            && t[p].closer.is_none()
            && t[p].partner.is_none()
            && closes_content(text, t, p, self.cx)
            && (!self.reads_inches || digit_before_quote(text, t[p].at, self.padded).is_none())
        {
            return Ok(true);
        }
        let previous = t[k].previous;
        let run_end = t[k].run_end.unwrap_or(k);
        let split = previous.is_some_and(|p| t[p].preserved || (t[p].closer.is_none() && t[p].partner.is_some()))
            && t[k].next.is_some_and(|n| !t[n].preserved);
        let held = t[run_end].next.is_some_and(|n| t[n].preserved) && previous.is_some_and(|p| t[p].closer.is_none());
        Ok(!split
            && !held
            && (t[run_end].content_after_run || (t[k].glyph == LDQUO_U && starts_with_empty_quotation(t, k)))
            && self.opens_editable(t, i)?)
    }
}

#[allow(clippy::too_many_arguments)]
fn connected_editable_pairs(
    text: &[u16],
    cx: &mut QuoteTags<'_>,
    qs: &QuoteSession,
    t: &mut [Tok],
    editable: &[usize],
    ordered: &[usize],
    preserved: &[usize],
    reads_inches: bool,
    writes_neutral_closers: bool,
    v: Visit<'_>,
) -> Result<HashSet<usize>> {
    for pair in ordered.windows(2) {
        vt(v)?;
        let (left, right) = (pair[0], pair[1]);
        t[left].following = Some(right);
        if t[left].end == cx.text_end(text, t[right].at) {
            t[left].next = Some(right);
            t[right].previous = Some(left);
        }
    }
    for &k in editable.iter().rev() {
        vt(v)?;
        let after = cx.text_start(text, t[k].end);
        t[k].after = Some(after);
        t[k].follower = kept_glyph_at(text, after).0.first().copied().unwrap_or(0x20);
        if let Some(n) = t[k].next
            && !t[n].preserved
        {
            t[k].run_end = t[n].run_end;
            continue;
        }
        t[k].run_end = Some(k);
        t[k].content_after_run = match t[k].next {
            Some(n) => t[n].partner.is_some() && t[n].closer.is_some(),
            None => {
                opening_quote_neighbours(Some(SEMICOLON), t[k].follower) && opening_content_at(text, after, cx, false)?
            }
        };
    }
    let padded = reads_inches && qs.padding_strip;
    let inch = Inch { reads: reads_inches, padded };
    pair_list(
        t,
        editable,
        true,
        &mut EditableOracle { text, cx: &mut *cx, editable, reads_inches, padded, unpaired: None },
        v,
    )?;
    let mut unpaired = HashSet::new();
    for &k in editable {
        vt(v)?;
        if t[k].glyph != LDQUO_U || t[k].closer != Some(RDQUO_U) {
            continue;
        }
        let german = t[k].previous;
        if t[k].partner.is_none()
            || german.is_some_and(|g| {
                t[g].glyph == BDQUO_U
                    && t[g].partner.is_none()
                    && t[g].outer.is_some_and(|o| t[o].closer == Some(RDQUO_U) && t[o].partner.is_none())
            })
        {
            unpaired.insert(k);
        }
    }
    if !unpaired.is_empty() {
        pair_list(
            t,
            editable,
            true,
            &mut EditableOracle { text, cx: &mut *cx, editable, reads_inches, padded, unpaired: Some(&unpaired) },
            v,
        )?;
    }
    if reads_inches {
        for &k in editable {
            vt(v)?;
            let previous = t[k].previous;
            let skip = t[k].glyph != QUOT
                || t[k].partner.is_some()
                || t[k].next.is_none_or(|n| t[n].partner.is_none() || t[n].closer.is_none())
                || previous.is_none_or(|p| {
                    t[p].glyph != QUOT
                        || t[p].closer.is_some()
                        || t[p].partner.is_none()
                        || digit_before_quote(text, t[p].at, padded).is_none()
                });
            if skip {
                continue;
            }
            let Some(p) = previous else { continue };
            let Some(outer) = t[p].partner else { continue };
            t[k].closer = None;
            t[k].partner = Some(outer);
            t[outer].partner = Some(k);
            t[p].partner = None;
        }
    }
    index_mixed_quote_pairs(text, cx, t, ordered, reads_inches, padded, inch, v)?;
    let mut pending: Vec<Option<usize>> = Vec::new();
    for &k in preserved {
        vt(v)?;
        if t[k].partner.is_some() {
            pending.push(Some(k));
        }
    }
    let mut preceding_title: Option<usize> = None;
    for (i, &k) in ordered.iter().enumerate() {
        vt(v)?;
        if reads_inches
            && t[k].glyph == QUOT
            && !t[k].preserved
            && t[k].previous.is_some()
            && pair_content_titles(text, cx, t, k, padded, &mut pending, v)?
        {
            continue;
        }
        if reads_inches && t[k].closer.is_none() && t[k].next.is_some_and(|n| t[n].next.is_some()) {
            keep_enclosed_empty_values(text, t, k, &mut pending, v)?;
        }
        let first = i.checked_sub(2).map(|j| ordered[j]);
        let second = i.checked_sub(1).map(|j| ordered[j]);
        if pair_empty_at_boundary(text, cx, t, first, second, k, reads_inches, padded)? {
            pending.push(first);
            pending.push(Some(k));
        }
        let nearest = t[k].previous;
        if nearest.is_none_or(|n| t[n].partner.is_some() || t[n].mixed.is_some()) {
            preceding_title = nearest.filter(|&n| t[n].closer.is_none());
        }
        if t[k].preserved {
            continue;
        }
        let own = t[k].partner.or(t[k].mixed);
        let opens_title = own.is_some() && t[k].closer.is_some() && t[k].next != own;
        let previous = if reads_inches && opens_title { preceding_title.or(nearest) } else { nearest };
        let Some(previous) = previous else { continue };
        if t[k].closer.is_none() {
            continue;
        }
        if let Some(n) = nearest
            && n != previous
            && t[n].closer.is_some()
            && !t[n].preserved
            && nest_touching_closers(text, cx, t, n, k, inch)?
        {
            pending.push(Some(k));
            pending.push(Some(n));
        }
        if t[previous].closer.is_some() {
            if nest_touching_closers(text, cx, t, previous, k, inch)?
                || (t[previous].partner.is_some()
                    && t[k].partner.is_some()
                    && t[k].next != t[k].partner
                    && (t[previous].glyph != QUOT || t[k].glyph != QUOT))
            {
                pending.push(Some(k));
                pending.push(Some(previous));
            }
            continue;
        }
        let (Some(first), Some(last)) = (t[previous].partner.or(t[previous].mixed), t[k].partner.or(t[k].mixed)) else {
            continue;
        };
        if !closes_content(text, t, previous, cx) {
            continue;
        }
        if t[k].next == Some(last) {
            if reads_inches
                && t[k].glyph == QUOT
                && t[last].glyph == QUOT
                && t[k].outer.is_none()
                && digit_before_quote(text, t[previous].at, padded).is_none()
            {
                continue;
            }
            if (t[first].next == Some(previous) && (t[k].glyph == QUOT || t[last].glyph == QUOT))
                || (t[previous].glyph == QUOT
                    && reads_inches
                    && digit_before_quote(text, t[previous].at, padded).is_some())
                || (t[k].glyph == QUOT && t[k].outer.is_some_and(|o| t[o].partner.is_none()))
            {
                continue;
            }
            t[k].empty = true;
            t[last].empty = true;
        }
        connect_titles(t, first, previous, k, last, &mut pending);
    }
    if writes_neutral_closers {
        complete_directed_speech(text, cx, qs.read_foot, t, ordered, &mut pending, reads_inches, padded, v)?;
    }
    let mut connected = HashSet::new();
    while let Some(token) = pending.pop() {
        vt(v)?;
        let Some(k) = token else { continue };
        if connected.contains(&k) || t[k].partner.is_none() {
            continue;
        }
        connected.insert(k);
        pending.push(t[k].previous);
        pending.push(t[k].next);
        pending.push(t[k].partner);
    }
    Ok(connected)
}

struct Authored {
    text: Option<JsString>,
    closing: HashSet<usize>,
}

fn index_authored_quotes(
    text: &[u16],
    cx: &mut QuoteTags<'_>,
    qs: &QuoteSession,
    pieces: bool,
    writes_neutral_closers: bool,
    reads_inches: bool,
) -> Result<Authored> {
    let mut closing = HashSet::new();
    let mut content = HashMap::new();
    let mut out: JsString = Vec::new();
    let mut pushed = false;
    let mut cursor = 0usize;
    let mut added: isize = 0;
    let budget = cx.budget();
    let visits = Cell::new(0u32);
    let tick = || -> Result<()> {
        let n = visits.get().wrapping_add(1);
        visits.set(n);
        if n & 1023 == 0
            && let Some(check) = budget
        {
            check("quote:authoredIndex")?;
        }
        Ok(())
    };
    let v: Visit<'_> = if budget.is_some() { Some(&tick) } else { None };
    let saved = cx.set_paragraphs(true);
    let paragraphs = quote_paragraphs(text, cx, budget);
    cx.set_paragraphs(saved);
    for (from, stop) in paragraphs? {
        let paragraph = &text[from..stop];
        let writable = pieces
            && ((from == 0 && stop == text.len())
                || needs_authored_quotes(paragraph, js::includes(paragraph, &[OPEN_UNIT, 0x4E])));
        if pieces && !writable && !paragraph.contains(&AMP) {
            continue;
        }
        let mut t: Vec<Tok> = Vec::new();
        let mut tokens = Vec::new();
        let mut editable = Vec::new();
        let mut ordered = Vec::new();
        let mut at = from;
        while at < stop {
            if at & 1023 == 0 {
                cx.check("quote:references")?;
            }
            let c = text[at];
            if writable && (is_quote_mark(c) || c == QUOT || c == RDQUO_U) {
                editable.push(t.len());
                ordered.push(t.len());
                t.push(Tok::new(at, at + 1, c, false));
            }
            if c != AMP {
                at += 1;
                continue;
            }
            let Some(reference) = read_character_reference_at(text, at) else {
                at += 1;
                continue;
            };
            let glyph = single_glyph(authored_reference_glyph(Some(&reference), true));
            if let Some(glyph) = glyph.filter(|&g| preserved_quote(Some(g))) {
                tokens.push(t.len());
                if writable {
                    ordered.push(t.len());
                }
                t.push(Tok::new(at, reference.end, glyph, true));
            }
            at = reference.end;
        }
        pair_list(
            &mut t,
            &tokens,
            false,
            &mut PreservedOracle { text, cx: &mut *cx, tokens: &tokens, content: &mut content, v },
            v,
        )?;
        let connected = if editable.is_empty() {
            HashSet::new()
        } else {
            connected_editable_pairs(
                text,
                cx,
                qs,
                &mut t,
                &editable,
                &ordered,
                &tokens,
                reads_inches,
                writes_neutral_closers,
                v,
            )?
        };
        for &k in if writable { &ordered } else { &tokens } {
            vt(v)?;
            let token = &t[k];
            if let Some(measure_at) = token.measure_at {
                out.extend_from_slice(&text[cursor..measure_at]);
                out.push(PRIME_U);
                pushed = true;
                added -= (token.at - measure_at) as isize;
                cursor = token.end;
                continue;
            }
            if token.preserved {
                if token.closer.is_none() && token.partner.is_some() {
                    closing.insert(((token.end - 1) as isize + added) as usize);
                }
                continue;
            }
            let adjacent = token.partner.is_some_and(|p| t[p].at == token.end || t[p].end == token.at);
            if !connected.contains(&k)
                || (token.glyph == QUOT
                    && (((token.empty || token.closer.is_none()) && !writes_neutral_closers)
                        || (!token.empty && adjacent)))
            {
                continue;
            }
            let role = if token.closer.is_none() { FIRS_CLOSE } else { FIRS_OPEN };
            if token.glyph != role {
                let padding = if role == FIRS_OPEN && token.glyph == QUOT {
                    opening_quote_padding(text, token.at, cx)
                } else {
                    &[]
                };
                added += padding.len() as isize;
                out.extend_from_slice(&text[cursor..token.at]);
                out.extend_from_slice(padding);
                out.push(role);
                pushed = true;
                cursor = token.end;
            }
        }
    }
    if pushed {
        out.extend_from_slice(&text[cursor..]);
    }
    Ok(Authored { text: pushed.then_some(out), closing })
}

static QUOTE_GLYPHS: LazyLock<String> = LazyLock::new(|| format!("\"{LAQUO}{RAQUO}{BDQUO}{LDQUO}\u{201D}"));
static EDITABLE_QUOTE_RE: LazyLock<Re> = LazyLock::new(|| Re::new(&format!("[{}]", *QUOTE_GLYPHS), "u"));
static TOUCHING_QUOTE_RE: LazyLock<Re> = LazyLock::new(|| Re::new(&format!("[{0}](?=[{0}<])", *QUOTE_GLYPHS), "u"));

fn needs_authored_quotes(text: &[u16], has_references: bool) -> bool {
    if has_references { EDITABLE_QUOTE_RE.test(text) } else { TOUCHING_QUOTE_RE.test(text) }
}

pub fn prepare_authored_quotes(
    text: JsString,
    cx: &mut QuoteTags<'_>,
    qs: &mut QuoteSession,
    writes_neutral_closers: bool,
    reads_inches: bool,
) -> Result<JsString> {
    let has_references = js::includes(&text, &[OPEN_UNIT, 0x4E]);
    if !needs_authored_quotes(&text, has_references) {
        return Ok(text);
    }
    let authored =
        index_authored_quotes(&text, cx, qs, true, writes_neutral_closers, writes_neutral_closers && reads_inches)?;
    let text = authored.text.unwrap_or(text);
    if has_references {
        qs.reference_pairs = Some(KnownClosers::new(text.clone(), authored.closing));
    }
    Ok(text)
}

fn ends_authored_quote_pair(text: &[u16], at: usize, cx: &mut QuoteTags<'_>, qs: &mut QuoteSession) -> Result<bool> {
    let reference = character_reference_ending_at(text, at, None)?;
    let glyph = single_glyph(authored_reference_glyph(reference.as_ref(), true));
    if !matches!(glyph, Some(QUOT | LDQUO_U | RDQUO_U | FIRS_CLOSE)) {
        return Ok(false);
    }
    if !qs.reference_pairs.as_mut().is_some_and(|known| known.holds(text)) {
        let authored = index_authored_quotes(text, cx, qs, false, false, false)?;
        qs.reference_pairs = Some(KnownClosers::new(text.to_vec(), authored.closing));
    }
    Ok(qs.reference_pairs.as_ref().is_some_and(|known| known.closing.contains(&at)))
}

fn close_behind_closer(m: &Caps<'_>, cx: &mut QuoteTags<'_>, qs: &mut QuoteSession) -> Result<JsString> {
    let (full, inner, outer) = (m.whole(), m.s(1), m.s(2));
    let (at, source) = (m.index(), m.input);
    let closed = || cat!(inner, repeat_char(outer, FIRS_CLOSE));
    if inner == [SEMICOLON] {
        let reference = character_reference_ending_at(source, at, None)?;
        let ends = single_glyph(quote_boundary_glyph(reference.as_ref())) == Some(FIRS_CLOSE)
            || ends_authored_quote_pair(source, at, cx, qs)?;
        return Ok(if ends { closed() } else { full.to_vec() });
    }
    let mut left = at;
    if qs.padding_strip {
        while left > 0 && matches!(source[left - 1], 0x20 | 0x09 | NBSP_U) {
            left -= 1;
        }
        if left < at && MAY_STRIP_PADDING_RE.exec_at(source, left).is_none() {
            left = at;
        }
    }
    let opens = (left == 0 || source[left - 1] != FIRS_OPEN) && can_start_quote(source, left, QUOT, cx, None);
    Ok(if opens { full.to_vec() } else { closed() })
}

fn opened_behind(source: &[u16], at: usize, cx: &mut QuoteTags<'_>) -> Result<Option<JsString>> {
    let mut end = at + 1;
    while source.get(end) == Some(&QUOT) {
        end += 1;
    }
    let quotes = &source[at + 1..end];
    let Some(&follower) = source.get(end) else { return Ok(None) };
    if quotes.is_empty() || is_separator(follower) {
        return Ok(None);
    }
    let full = cat!(FIRS_OPEN, quotes);
    if dash_keeps(quotes, &[follower], source, at + 1, cx) {
        return Ok(None);
    }
    let written =
        open_before_word(&full, &[FIRS_OPEN], quotes, &[follower], at, source, cx, &mut QuoteSession::default())?;
    Ok(if written == full { None } else { Some(written[1..].to_vec()) })
}

fn apply_inner_quotes(ctx: &mut Ctx<'_>) -> Result<Option<JsString>> {
    let settings = ctx.group.as_ref().map(|g| &g.settings);
    let allow_nested = setting_on(settings, "allowNested");
    let convert_inches = setting_on(settings, "convertInches");
    let closers = ctx.authored_closers.take().unwrap_or_default();
    let kept = |buf: &[u16], at: usize| kept_closer(&closers, buf, at);
    let check_budget = ctx.check_budget;
    let (mut cx, qs, text) = ctx.quote_parts();
    let opens = qs.open;
    let options = QuoteOptions {
        allow_nested,
        convert_inches,
        strips_padding: qs.padding_strip,
        pair_remainder: opens && qs.close,
        check_budget: Some(check_budget),
        kept_closer: (!closers.addresses.is_empty() || !closers.end_tags.is_empty()).then_some(&kept as Kept<'_>),
        opened_behind: opens.then_some(&opened_behind as OpenedBehind<'_>),
    };
    let text = process_quotes(text, &options, &mut cx)?;
    ctx.authored_closers = Some(closers);
    Ok(Some(text))
}

fn setting_on(settings: Option<&serde_json::Map<String, Value>>, key: &str) -> bool {
    settings.and_then(|s| s.get(key)) != Some(&Value::Bool(false))
}

type Bounded = fn(&Caps<'_>) -> JsString;

fn bounded(f: Bounded) -> Replacement {
    Replacement::func(move |m, ctx| {
        let (mut cx, _) = ctx.quote_cx();
        close_at_boundary(m, &mut cx, f(m))
    })
}

fn after_tag(mark: &'static str) -> Replacement {
    Replacement::func(move |m, ctx| {
        let (mut cx, _) = ctx.quote_cx();
        close_at_boundary(m, &mut cx, cat!(">", FIRS_CLOSE, mark, m.s(1)))
    })
}

fn close_count(s: &[u16]) -> JsString {
    vec![FIRS_CLOSE; count(s, &[QUOT]) + count(s, &[FIRS_OPEN])]
}

pub fn quote_group() -> RuleGroupDef {
    let closer_left = &*CLOSER_LEFT;
    let mut group = super::group(
        "Кавычки",
        None,
        vec![
            Rule::pattern("around_link", QUOTES_OUTSIDE_A_RE.clone(), Replacement::tpl(r#""$1$2$3""#))
                .describe("Кавычки вынесены за тег <a>")
                .requires(F::QUOTE),
            Rule::pattern(
                "fix_orphan_open",
                Re::new(r"([a-zа-яё0-9])«(?=\s|$)", "giu"),
                Replacement::Template(cat!("$1", FIRS_CLOSE)),
            )
            .describe("Открывающая «, приклеенная к концу слова перед пробелом/концом → закрывающая »")
            .requires(F::QUOTE),
            Rule::handler("open", |ctx| {
                if !ctx.text.contains(&QUOT) {
                    return Ok(None);
                }
                let text = ctx.text.clone();
                let stepped = OPEN_AFTER_STRONG_RE.try_replace_with(&text, |m| open_after_strong(m, ctx))?;
                let (mut cx, qs) = ctx.quote_cx();
                let stepped = OPEN_AFTER_WEAK_RE.try_replace_with(&stepped, |m| {
                    open_before_word(m.whole(), m.s(1), m.s(2), m.s(3), m.index(), m.input, &mut cx, qs)
                })?;
                Ok(Some(open_spaced_quotes(&stepped, &mut cx)?))
            })
            .describe("Открывающая кавычка")
            .requires(F::QUOTE),
            Rule::pattern(
                "open_cyrillic",
                OPEN_CYRILLIC_RE.clone(),
                Replacement::Template(cat!("$1 ", FIRS_OPEN, "$2")),
            )
            .describe("Приклеенная кавычка у кириллицы → « с пробелом")
            .requires(F::QUOTE),
            Rule::patterns(
                "close",
                vec![
                    (
                        Re::new(r#"(?<=\\[\w:]*)\\"(?=$|\s|[.,;:!?)]|<)"#, "u"),
                        Replacement::Template(cat!("\\", FIRS_CLOSE)),
                    ),
                    (
                        Re::new(&format!(r#"({closer_left})((?:"|\\")+)(?!{QUOTE_WORD})"#), "giu"),
                        bounded(|m| cat!(m.s(1), repeat_char(m.s(2), FIRS_CLOSE))),
                    ),
                    (
                        Re::new(&format!(r#"([{RAQUO};])((?:"|\\")+)(?!["\p{{L}}\p{{N}}])"#), "gu"),
                        Replacement::func(|m, ctx| {
                            let (mut cx, qs) = ctx.quote_cx();
                            close_behind_closer(m, &mut cx, qs)
                        }),
                    ),
                    (
                        Re::new(
                            &format!(
                                r#"({closer_left})((?:"|\\"|{LAQUO})+)(<[^>]+>)(\.|{HELLIP}|;|:|\?|!|,|\)|<\/|$| )"#
                            ),
                            "giu",
                        ),
                        bounded(|m| cat!(m.s(1), close_count(m.s(2)), m.s(3), m.s(4))),
                    ),
                    (
                        Re::new(
                            &format!(r#"({closer_left})(\s+)((?:"|\\")+)(\s+)(\.|{HELLIP}|;|:|\?|!|,|\)|\(|<\/|$| )"#),
                            "giu",
                        ),
                        bounded(|m| cat!(m.s(1), m.s(2), close_count(m.s(3)), m.s(4), m.s(5))),
                    ),
                    (
                        Re::new(&format!(r#"({})(\s+)((?:"|\\")+)(?=\s)"#, *SPACED_CLOSER_LEFT), "giu"),
                        bounded(|m| cat!(m.s(1), repeat_char(m.s(3), FIRS_CLOSE))),
                    ),
                    (
                        Re::new(&format!(r#"({COPY}[ \t]+)((?:"|\\")+)(?=\s)"#), "gu"),
                        Replacement::func(|m, ctx| {
                            let (mut cx, qs) = ctx.quote_cx();
                            close_after_copyright(m, &mut cx, qs)
                        }),
                    ),
                    (
                        Re::new(&format!(r#"({LAQUO})([ \t]+)((?:"|\\")+)(?=\s)"#), "gu"),
                        bounded(|m| cat!(m.s(1), repeat_char(m.s(3), FIRS_CLOSE))),
                    ),
                    (Re::new(r">«\.($|\s|<)", "gu"), after_tag(".")),
                    (Re::new(r">«,($|\s|<|\S)", "gu"), after_tag(",")),
                    (Re::new(r">«:($|\s|<|\S)", "gu"), after_tag(":")),
                    (Re::new(r">«;($|\s|<|\S)", "gu"), after_tag(";")),
                    (Re::new(r">«\)($|\s|<|\S)", "gu"), after_tag(")")),
                    (
                        Re::new(r#"((?:"|\\")+)(?=[ \t]*$)"#, "giu"),
                        Replacement::func(|m, _| Ok(vec![FIRS_CLOSE; count(m.s(1), &[QUOT])])),
                    ),
                    (
                        Re::new(
                            &format!(
                                r#"([^{SEPARATOR}])((?:"|\\")+)(?=\.|{HELLIP}|;|:|\?|!|,|\s|\p{{Pe}}|<\/|<|{RAQUO}|&|$)"#
                            ),
                            "giu",
                        ),
                        Replacement::func(|m, ctx| {
                            let (mut cx, _) = ctx.quote_cx();
                            let after = m.index() + m.whole().len();
                            if m.input.get(after) == Some(&AMP) && quote_follower(m.input, after, &mut cx) != FIRS_CLOSE
                            {
                                return Ok(m.whole().to_vec());
                            }
                            close_at_boundary(m, &mut cx, cat!(m.s(1), vec![FIRS_CLOSE; count(m.s(2), &[QUOT])]))
                        }),
                    ),
                ],
            )
            .describe("Закрывающая кавычка")
            .requires(F::QUOTE)
            .requires_pattern(Re::new(&format!(r#""|{LAQUO}<|>{LAQUO}[.,:;)]"#), "u")),
            Rule::handler("nested", apply_inner_quotes).describe("Внутренние кавычки-лапки и дюймы").requires(F::QUOTE),
        ],
    );
    group.pre_parse = Some(Arc::new(|ctx: &mut Ctx<'_>| {
        ctx.authored_closers = Some(authored_closers_in(&ctx.text));
        Ok(())
    }));
    group
}

#[cfg(test)]
mod tests {
    use serde_json::{Map, Value};

    use crate::js::{lossy, u};
    use crate::protect::blocks::SafeBlocks;
    use crate::protect::sequences::SafeSequences;
    use crate::protect::tags::{DEFAULT_LAYOUT, SafeTags};
    use crate::rule_runner::{Ctx, compile_rule_group, run_rule_group};

    use super::quote_group;

    const GROUP_CASES: &[(&str, bool, bool, &str)] = &[
        ("Он сказал \"привет\" и ушёл", true, true, "Он сказал «привет» и ушёл"),
        ("\"Мама \"мыла\" раму\"", true, true, "«Мама „мыла“ раму»"),
        ("\"Книга \"Война и \"мир\"\" прочитана\"", true, true, "«Книга „Война и „мир““ прочитана»"),
        ("Экран 15\" и 17\"", true, true, "Экран 15″ и 17″"),
        ("Доска 2\"x4\" длиной", true, true, "Доска 2\"x4″ длиной"),
        ("It's \"quoted\" text, isn't it?", true, true, "It's «quoted» text, isn't it?"),
        ("\" Новый Амбер\" вышел", true, true, "«Новый Амбер» вышел"),
        ("Слово \" отдельно \" стоит", true, true, "Слово «отдельно» стоит"),
        ("Пустые \" \" кавычки", true, true, "Пустые «» кавычки"),
        ("**\"Амбер\"** и \"это *важно*\".", true, true, "**«Амбер»** и «это *важно*»."),
        (
            "Ссылка <\u{e000}A1\u{e001}>\"текст\"</\u{e000}A1\u{e001}> тут",
            true,
            true,
            "Ссылка «<\u{e000}A1\u{e001}>текст</\u{e000}A1\u{e001}>» тут",
        ),
        ("слово\"кириллица", true, true, "слово «кириллица"),
        ("\"Мама \"мыла\" раму\"", false, true, "«Мама «мыла» раму»"),
        ("Экран 15\" и \"фильм\"", true, false, "Экран 15» и «фильм»"),
        ("путь C:\\dir\\\"", true, true, "путь C:\\dir\\\""),
        ("Он сказал: \"Корвин\" (принц)", true, true, "Он сказал: «Корвин» (принц)"),
        ("\"Первый\"\n\n\"Второй \"вложенный\"\"", true, true, "«Первый»\n\n«Второй „вложенный“»"),
        ("«ёлка» и «а «б» в»", true, true, "«ёлка» и «а „б“ в»"),
        ("цитата \"a\", \"b\"; \"c\": \"d\"!", true, true, "цитата «a», «b»; «c»: «d»!"),
        ("😀 \"эмодзи 😀\" 12\" 😀", true, true, "😀 «эмодзи 😀» 12″ 😀"),
        ("\"раз\" \"два\" \"три", true, true, "«раз» «два» «три"),
        ("слово «, дальше", true, true, "слово «, дальше"),
        ("Он сказал \"Кот\" (2020) и", true, true, "Он сказал «Кот» (2020) и"),
    ];

    fn run(text: &str, allow_nested: bool, convert_inches: bool) -> String {
        let mut group = compile_rule_group(&quote_group(), "quote").unwrap();
        if !allow_nested {
            group.set_setting("allowNested", Value::Bool(false));
        }
        if !convert_inches {
            group.set_setting("convertInches", Value::Bool(false));
        }
        let settings = Map::new();
        let mut tags = SafeTags::new(false);
        let blocks = SafeBlocks::default();
        let mut sequences = SafeSequences::new(|bytes| bytes.fill(0));
        let mut ctx = Ctx {
            text: Vec::new(),
            group: None,
            html: true,
            layout: DEFAULT_LAYOUT,
            engine_settings: &settings,
            check_budget: &|_| Ok(()),
            on_cycle_limit: None,
            safe_tags: &mut tags,
            safe_blocks: &blocks,
            safe_sequences: &mut sequences,
            prefix: "",
            budget: None,
            markdown: None,
            authored_closers: None,
            inline: None,
            features: None,
            trace: None,
            trace_projection: None,
            trace_escaped: None,
            bound: crate::rule_runner::Bindings::default(),
        };
        lossy(&run_rule_group(&group, u(text), &mut ctx).unwrap())
    }

    #[test]
    fn matches_reference() {
        for &(input, allow_nested, convert_inches, expected) in GROUP_CASES {
            assert_eq!(run(input, allow_nested, convert_inches), expected, "input: {input:?}");
        }
    }
}
