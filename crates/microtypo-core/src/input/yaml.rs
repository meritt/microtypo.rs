use std::sync::LazyLock;

use serde_json::json;

use crate::errors::{Error, Result};
use crate::input::span_path::{Chain, LocatedSpan, Position, chain_append};
use crate::js::{self, JsString, Re};
use crate::protect::blocks::CheckBudget;
use crate::shared::escape_decoder::decode_escapes;
use crate::shared::strings::{is_flow_space, line_view, skip_spaces};

const LF: u16 = 0x0A;
const CR: u16 = 0x0D;
const SP: u16 = 0x20;
const TAB: u16 = 0x09;
const DQ: u16 = 0x22;
const SQ: u16 = 0x27;
const HASH: u16 = 0x23;
const AMP: u16 = 0x26;
const BANG: u16 = 0x21;
const COLON: u16 = 0x3A;
const QM: u16 = 0x3F;
const DASH: u16 = 0x2D;
const LT: u16 = 0x3C;
const GT: u16 = 0x3E;
const BACKSLASH: u16 = 0x5C;
const LBRACKET: u16 = 0x5B;
const RBRACKET: u16 = 0x5D;
const LBRACE: u16 = 0x7B;
const RBRACE: u16 = 0x7D;
const COMMA: u16 = 0x2C;

static PLAIN_KEYWORD: LazyLock<Re> =
    LazyLock::new(|| Re::new(r"^(?:~|null|true|false|yes|no|on|off|y|n|\.inf|\.nan|=|<<|---|\.\.\.)$", "i"));
static LETTER: LazyLock<Re> = LazyLock::new(|| Re::new(r"\p{L}", "u"));

static PLAIN_BREAKS: LazyLock<Re> = LazyLock::new(|| Re::new(r":(?:\s|$)|\s#|[\r\n]", ""));
static CANONICAL_INT: LazyLock<Re> = LazyLock::new(|| Re::new(r"^(?:0|-?[1-9][0-9]{0,14})$", ""));
static CANONICAL_BOOL: LazyLock<Re> = LazyLock::new(|| Re::new(r"^(?:true|false)$", ""));

fn resolves_to_string(value: &[u16]) -> bool {
    if value.is_empty() || PLAIN_KEYWORD.test(value) {
        return false;
    }

    if !LETTER.test(value) {
        return false;
    }

    value.contains(&SP) || !matches!(value.first(), Some(0x2B | 0x2D | 0x2E | 0x30..=0x39))
}

fn is_plain_indicator(c: u16) -> bool {
    matches!(
        c,
        0x2D | 0x3F
            | 0x3A
            | 0x2C
            | 0x5B
            | 0x5D
            | 0x7B
            | 0x7D
            | 0x23
            | 0x26
            | 0x2A
            | 0x21
            | 0x7C
            | 0x3E
            | 0x27
            | 0x22
            | 0x25
            | 0x40
            | 0x60
    )
}

fn plain_indicator_test(s: &[u16]) -> bool {
    s.first().is_some_and(|&c| is_plain_indicator(c))
}

fn is_flow_break(c: u16) -> bool {
    matches!(c, COMMA | LBRACKET | RBRACKET | LBRACE | RBRACE)
}

pub fn plain_scalar_safe(typed: &[u16]) -> bool {
    !typed.is_empty()
        && js::trim(typed) == typed
        && !PLAIN_BREAKS.test(typed)
        && !plain_indicator_test(typed)
        && !PLAIN_KEYWORD.test(typed)
}

pub fn flow_scalar_safe(typed: &[u16]) -> bool {
    plain_scalar_safe(typed) && !typed.iter().any(|&c| is_flow_break(c))
}

fn is_plain_after_indicator(ch: Option<u16>) -> bool {
    !is_flow_space(ch) && !ch.is_some_and(is_flow_break)
}

fn folds_onto_next_line(src: &[u16], line_end: usize, n: usize, indent: usize) -> bool {
    let mut line_start = line_end + 1;

    while line_start < n {
        let p = skip_spaces(src, line_start, n);

        if p >= n {
            return false;
        }

        if src[p] != LF && src[p] != CR {
            return src[p] != HASH && p - line_start > indent;
        }

        line_start = js::index_of_unit(src, LF, p).map_or(n, |b| b + 1);
    }

    false
}

fn plain_scalar_end(src: &[u16], pos: usize, line_end: usize) -> usize {
    let mut end = line_end;

    for i in pos + 1..line_end {
        if src[i] == HASH && (src[i - 1] == SP || src[i - 1] == TAB) {
            end = i;
            break;
        }
    }

    while end > pos && (src[end - 1] == SP || src[end - 1] == TAB) {
        end -= 1;
    }

    end
}

const YAML_ESCAPE: &[(u16, &str)] = &[
    (b'0' as u16, "\0"),
    (b'a' as u16, "\x07"),
    (b'b' as u16, "\x08"),
    (b't' as u16, "\t"),
    (b'\t' as u16, "\t"),
    (b'n' as u16, "\n"),
    (b'v' as u16, "\x0B"),
    (b'f' as u16, "\x0C"),
    (b'r' as u16, "\r"),
    (b'e' as u16, "\x1B"),
    (b' ' as u16, " "),
    (b'"' as u16, "\""),
    (b'/' as u16, "/"),
    (b'\\' as u16, "\\"),
    (b'N' as u16, "\u{85}"),
    (b'_' as u16, "\u{A0}"),
    (b'L' as u16, "\u{2028}"),
    (b'P' as u16, "\u{2029}"),
];

const YAML_HEX: &[(u16, usize)] = &[(b'x' as u16, 2), (b'u' as u16, 4), (b'U' as u16, 8)];

fn decode_quoted_key(raw: &[u16], quote: u16) -> JsString {
    if quote == SQ { js::replace_all_lit(raw, &[SQ, SQ], &[SQ]) } else { decode_escapes(raw, YAML_ESCAPE, YAML_HEX) }
}

fn scan_quoted_line(line: &[u16], escapes: bool) -> (&[u16], bool) {
    let mut content_end = 0;
    let mut i = 0;

    while i < line.len() {
        if escapes && line[i] == BACKSLASH {
            if i + 1 == line.len() {
                return (&line[..i], true);
            }

            i += 2;
            content_end = i;
            continue;
        }

        let ch = line[i];

        i += 1;

        if ch != SP && ch != TAB {
            content_end = i;
        }
    }

    (&line[..content_end], false)
}

fn fold_breaks(count: isize, first_escaped: bool) -> JsString {
    let n = if first_escaped { count - 1 } else { count };

    if n <= 0 {
        return Vec::new();
    }

    if n == 1 { vec![SP] } else { vec![LF; (n - 1) as usize] }
}

fn fold_quoted_scalar(raw: &[u16], quote: u16) -> JsString {
    if !raw.contains(&LF) {
        return raw.to_vec();
    }

    let escapes = quote == DQ;
    let lines = js::split(raw, &[LF]);
    let last_index = lines.len() - 1;
    let mut out: JsString = Vec::new();
    let mut breaks: isize = -1;
    let mut first_escaped = false;

    for (i, &line) in lines.iter().enumerate() {
        let last = i == last_index;
        let body = if i == 0 {
            line
        } else {
            let lead = line.iter().take_while(|&&c| c == SP || c == TAB).count();
            &line[lead..]
        };
        let (content, escapes_break) = if last {
            (body, false)
        } else {
            let stripped = if body.last() == Some(&CR) { &body[..body.len() - 1] } else { body };
            scan_quoted_line(stripped, escapes)
        };

        if !last && content.is_empty() && !escapes_break && breaks >= 0 {
            breaks += 1;
            continue;
        }

        out = if breaks < 0 { content.to_vec() } else { crate::cat!(out, fold_breaks(breaks, first_escaped), content) };
        breaks = 1;
        first_escaped = escapes_break;
    }

    out
}

fn quoted_key_name(raw: &[u16], quote: u16) -> JsString {
    decode_quoted_key(&fold_quoted_scalar(raw, quote), quote)
}

#[derive(Clone, Copy)]
struct Quoted {
    end: usize,
    eligible: bool,
    multiline: bool,
}

fn scan_quoted(src: &[u16], i: usize, line_end: usize) -> Option<Quoted> {
    let q = src[i];
    let mut j = i + 1;
    let mut has_escape = false;
    let mut multiline = false;

    while j < line_end {
        let c = src[j];
        multiline = multiline || c == LF;

        if q == DQ {
            if c == BACKSLASH {
                has_escape = true;
                multiline = multiline || src.get(j + 1) == Some(&LF);
                j += 2;
                continue;
            }

            if c == DQ {
                return Some(Quoted { end: j + 1, eligible: !has_escape, multiline });
            }
        } else if c == SQ {
            if src.get(j + 1) == Some(&SQ) {
                has_escape = true;
                j += 2;
                continue;
            }

            return Some(Quoted { end: j + 1, eligible: !has_escape, multiline });
        }

        j += 1;
    }

    None
}

fn has_trailing_line_tokens(src: &[u16], from: usize, line_end: usize) -> bool {
    let p = skip_spaces(src, from, line_end);

    p < line_end && src[p] != HASH
}

fn assert_no_trailing_line_tokens(src: &[u16], from: usize, line_end: usize) -> Result<()> {
    if has_trailing_line_tokens(src, from, line_end) {
        let p = skip_spaces(src, from, line_end);

        return Err(Error::input(
            format!("Unexpected trailing content after YAML scalar at offset {p}"),
            json!({ "offset": p }),
        ));
    }

    Ok(())
}

fn is_flow_structural(c: u16) -> bool {
    matches!(c, SP | TAB | LF | CR | LBRACKET | RBRACKET | LBRACE | RBRACE | COMMA | COLON | DQ | SQ)
}

fn skip_flow_space(src: &[u16], i: usize, n: usize) -> usize {
    let mut j = i;

    loop {
        while j < n && is_flow_space(src.get(j).copied()) {
            j += 1;
        }

        if j >= n || src[j] != HASH || (j > 0 && !is_flow_space(Some(src[j - 1]))) {
            return j;
        }

        match js::index_of_unit(src, LF, j) {
            None => return n,
            Some(line) => j = line,
        }
    }
}

fn end_of_node_properties(src: &[u16], i: usize, n: usize) -> usize {
    let mut j = i;

    while j < n && (src[j] == AMP || src[j] == BANG) {
        if src[j] == BANG && src.get(j + 1) == Some(&LT) {
            j = js::index_of_unit(src, GT, j + 2).map_or(n, |close| close + 1);
        } else {
            j += 1;

            while j < n && is_plain_after_indicator(Some(src[j])) {
                j += 1;
            }
        }

        j = skip_flow_space(src, j, n);
    }

    j
}

fn property_tag(src: &[u16], from: usize, to: usize) -> Option<JsString> {
    let mut tag = None;
    let mut p = from;

    while p < to {
        let ch = src[p];

        if ch == HASH {
            p = js::index_of_unit(src, LF, p).unwrap_or(to);
        } else if ch == BANG && src.get(p + 1) == Some(&LT) {
            let end = match js::index_of_unit(src, GT, p + 2) {
                Some(close) if close < to => close + 1,
                _ => to,
            };

            tag = Some(src[p..end].to_vec());
            p = end;
        } else if ch == BANG || ch == AMP {
            let start = p;

            p += 1;

            while p < to && is_plain_after_indicator(Some(src[p])) {
                p += 1;
            }

            if ch == BANG {
                tag = Some(src[start..p].to_vec());
            }
        } else {
            p += 1;
        }
    }

    tag
}

#[derive(Debug, Clone, PartialEq)]
enum KeyName {
    Named(JsString),
    Unnamed,
}

const UNNAMED_SEGMENT: &str = "Symbol(unnamed key)";

fn key_position(key: &KeyName) -> Position {
    match key {
        KeyName::Named(s) => Position::Key(s.clone()),
        KeyName::Unnamed => Position::Key(js::u(UNNAMED_SEGMENT)),
    }
}

fn opt_key_position(key: &Option<KeyName>) -> Position {
    match key {
        Some(k) => key_position(k),
        None => Position::Key(js::u("null")),
    }
}

fn is_lit(s: &[u16], lit: &str) -> bool {
    s.iter().copied().eq(lit.encode_utf16())
}

fn typed_key_name(tag: Option<&[u16]>, text: JsString) -> KeyName {
    let Some(tag) = tag else { return KeyName::Named(text) };

    if is_lit(tag, "!") || is_lit(tag, "!!str") || is_lit(tag, "!<tag:yaml.org,2002:str>") {
        return KeyName::Named(text);
    }

    let self_named: Option<&Re> = if is_lit(tag, "!!int") || is_lit(tag, "!<tag:yaml.org,2002:int>") {
        Some(&CANONICAL_INT)
    } else if is_lit(tag, "!!bool") || is_lit(tag, "!<tag:yaml.org,2002:bool>") {
        Some(&CANONICAL_BOOL)
    } else {
        None
    };

    if self_named.is_some_and(|re| re.test(&text)) { KeyName::Named(text) } else { KeyName::Unnamed }
}

struct FlowFrame {
    seq: bool,
    index: usize,
    key: Option<KeyName>,
    explicit: bool,
    start: usize,
    opaque: bool,
    spans_at: usize,
    unnamed_at: Option<usize>,
    covered: bool,
    base: Chain,
}

fn flow_element_chain(frame: Option<&FlowFrame>, base: &Chain) -> Chain {
    let Some(frame) = frame else { return base.clone() };

    if !frame.seq {
        return chain_append(&frame.base, [opt_key_position(&frame.key)]);
    }

    let element = chain_append(&frame.base, [Position::Index(frame.index)]);

    match &frame.key {
        None => element,
        Some(k) => chain_append(&element, [key_position(k)]),
    }
}

fn awaits_key(frame: Option<&FlowFrame>) -> bool {
    frame.is_some_and(|f| !f.seq && f.key.is_none())
}

fn in_key_position(frame: Option<&FlowFrame>) -> bool {
    frame.is_some_and(|f| f.opaque || f.explicit) || awaits_key(frame)
}

fn mark_unaddressed(spans: &mut [LocatedSpan], from: usize) {
    for span in spans.iter_mut().skip(from) {
        span.unaddressed = true;
    }
}

fn set_flow_key(frame: &mut FlowFrame, key: KeyName, spans_len: usize) {
    let unnamed = key == KeyName::Unnamed;

    frame.key = Some(key);

    if unnamed && !frame.covered {
        frame.unnamed_at = Some(spans_len);
    }
}

fn name_flow_key(frame: &mut FlowFrame, tag: Option<&[u16]>, text: JsString, spans_len: usize) {
    set_flow_key(frame, typed_key_name(tag, text), spans_len);
}

fn end_flow_entry(frame: &mut FlowFrame, spans: &mut [LocatedSpan]) {
    if let Some(at) = frame.unnamed_at.take() {
        mark_unaddressed(spans, at);
    }
}

#[allow(clippy::too_many_arguments)]
fn record_flow_scalar(
    src: &[u16],
    n: usize,
    start: usize,
    quoted: &Quoted,
    frame: Option<&mut FlowFrame>,
    base: &Chain,
    spans: &mut Vec<LocatedSpan>,
    tagged: bool,
    tag: Option<&[u16]>,
) {
    let is_key = in_key_position(frame.as_deref()) || src.get(skip_flow_space(src, quoted.end, n)) == Some(&COLON);
    let chain = match frame.as_deref() {
        Some(f) if is_key && !f.seq => f.base.clone(),
        f => flow_element_chain(f, base),
    };
    let eligible = quoted.eligible && !quoted.multiline && !tagged;

    spans.push(LocatedSpan::with(start, quoted.end, is_key, chain, eligible, vec![src[start]], None));

    if is_key
        && let Some(f) = frame
        && !f.opaque
    {
        let name = quoted_key_name(&src[start + 1..quoted.end - 1], src[start]);
        name_flow_key(f, tag, name, spans.len());
    }
}

fn trim_separators(text: &[u16]) -> &[u16] {
    let mut end = text.len();

    while end > 0 && is_flow_space(Some(text[end - 1])) {
        end -= 1;
    }

    &text[..end]
}

fn plain_key_text(src: &[u16], from: usize, to: usize) -> JsString {
    for i in from..to {
        if src[i] == HASH && (i == from || is_flow_space(Some(src[i - 1]))) {
            return trim_separators(js::slice(src, from, i)).to_vec();
        }
    }

    trim_separators(js::slice(src, from, to)).to_vec()
}

fn flow_colon_separates(src: &[u16], i: usize) -> bool {
    i + 1 >= src.len() || is_flow_space(Some(src[i + 1])) || is_flow_break(src[i + 1])
}

fn flow_key_colon(src: &[u16], from: usize, end: usize) -> Option<usize> {
    (from..end).find(|&i| src[i] == COLON && flow_colon_separates(src, i))
}

const FLOW_PLAIN_END: [u16; 3] = [COMMA, RBRACKET, RBRACE];
const LINE_BREAK: [u16; 2] = [LF, CR];

struct Cursors {
    plain_ends: [Option<usize>; 3],
    line_breaks: [Option<usize>; 2],
}

fn new_delimiter_cursors(src: &[u16]) -> Cursors {
    Cursors {
        plain_ends: FLOW_PLAIN_END.map(|ch| js::index_of_unit(src, ch, 0)),
        line_breaks: LINE_BREAK.map(|ch| js::index_of_unit(src, ch, 0)),
    }
}

fn nearest_of(src: &[u16], from: usize, chars: &[u16], found: &mut [Option<usize>]) -> Option<usize> {
    let mut best: Option<usize> = None;

    for k in 0..chars.len() {
        if let Some(f) = found[k]
            && f < from
        {
            found[k] = js::index_of_unit(src, chars[k], from);
        }

        if let Some(f) = found[k]
            && best.is_none_or(|b| f < b)
        {
            best = Some(f);
        }
    }

    best
}

#[allow(clippy::too_many_arguments)]
fn scan_flow_collection(
    src: &[u16],
    start: usize,
    n: usize,
    base: &Chain,
    spans: &mut Vec<LocatedSpan>,
    check_budget: CheckBudget<'_>,
    cursors: &mut Cursors,
) -> Result<Option<usize>> {
    let mut stack: Vec<FlowFrame> = Vec::new();
    let mut j = start;
    let mut can_open_quote = false;
    let mut steps: u64 = 0;
    let mut folded = false;
    let mut key_start: Option<usize> = None;
    let mut tagged = false;
    let mut tag: Option<JsString> = None;
    let mut after_json_node = false;

    while j < n {
        if steps & 0x3fff == 0 {
            check_budget("yaml-flow-scan")?;
        }
        steps += 1;

        let c = src[j];

        if is_flow_space(Some(c)) {
            j += 1;
            continue;
        }

        if c == HASH && (j == start || is_flow_space(Some(src[j - 1]))) {
            j = nearest_of(src, j, &LINE_BREAK, &mut cursors.line_breaks).unwrap_or(n);
            continue;
        }

        let follows_json_node = after_json_node;
        after_json_node = false;

        if (c == AMP || c == BANG) && can_open_quote {
            let end = end_of_node_properties(src, j, n);

            tag = property_tag(src, j, end);
            j = end;
            tagged = true;
            continue;
        }

        if c == QM && can_open_quote && (j + 1 >= n || !is_plain_after_indicator(Some(src[j + 1]))) {
            if let Some(frame) = stack.last_mut() {
                frame.explicit = true;
            }

            j += 1;
            continue;
        }

        if (c == DQ || c == SQ) && can_open_quote {
            let Some(q) = scan_quoted(src, j, n) else { return Ok(None) };
            let tag_arg = if tagged { tag.clone() } else { None };

            record_flow_scalar(src, n, j, &q, stack.last_mut(), base, spans, tagged, tag_arg.as_deref());
            j = q.end;
            can_open_quote = false;
            tagged = false;
            after_json_node = true;
            continue;
        }

        if c == LBRACKET || c == LBRACE {
            let frame = stack.last();
            let opaque = tagged || in_key_position(frame);
            let covered = frame.is_some_and(|f| f.unnamed_at.is_some() || f.covered);
            let frame_base = flow_element_chain(frame, base);

            stack.push(FlowFrame {
                seq: c == LBRACKET,
                index: 0,
                key: None,
                explicit: false,
                start: j,
                opaque,
                spans_at: spans.len(),
                unnamed_at: None,
                covered,
                base: frame_base,
            });
            key_start = None;
            tagged = false;
            j += 1;
            can_open_quote = true;
            continue;
        }

        if c == RBRACKET || c == RBRACE {
            if stack.last().is_none_or(|f| f.seq != (c == RBRACKET)) {
                return Ok(None);
            }

            let mut frame = stack.pop().expect("checked above");
            end_flow_entry(&mut frame, spans);
            j += 1;
            can_open_quote = false;
            folded = false;
            tagged = false;
            after_json_node = true;

            if in_key_position(stack.last()) || src.get(skip_flow_space(src, j, n)) == Some(&COLON) {
                spans.truncate(frame.spans_at);

                if let Some(outer) = stack.last_mut()
                    && !outer.opaque
                {
                    let key =
                        if outer.explicit { KeyName::Unnamed } else { KeyName::Named(src[frame.start..j].to_vec()) };
                    set_flow_key(outer, key, spans.len());
                }
            }

            if stack.is_empty() {
                return Ok(Some(j));
            }

            continue;
        }

        if c == COMMA {
            if let Some(frame) = stack.last_mut() {
                if frame.seq {
                    frame.index += 1;
                }

                end_flow_entry(frame, spans);
                frame.key = None;
                frame.explicit = false;
            }

            folded = false;
            tagged = false;
            key_start = None;
            j += 1;
            can_open_quote = true;
            continue;
        }

        if c == COLON && (follows_json_node || flow_colon_separates(src, j) || !in_key_position(stack.last())) {
            let tag_arg = if tagged { tag.clone() } else { None };
            let frame = stack.last();

            if let Some(ks) = key_start
                && (awaits_key(frame) || frame.is_some_and(|f| f.explicit))
            {
                let text = plain_key_text(src, ks, j);
                let len = spans.len();
                name_flow_key(stack.last_mut().expect("frame"), tag_arg.as_deref(), text, len);
            } else if awaits_key(frame) || frame.is_some_and(|f| f.seq && f.key.is_none()) {
                let len = spans.len();
                name_flow_key(stack.last_mut().expect("frame"), tag_arg.as_deref(), Vec::new(), len);
            }

            if let Some(frame) = stack.last_mut() {
                frame.explicit = false;
            }

            key_start = None;
            tagged = false;
            j += 1;
            can_open_quote = true;
            continue;
        }

        if !in_key_position(stack.last()) && stack.last().is_some() {
            let delim_at = nearest_of(src, j, &FLOW_PLAIN_END, &mut cursors.plain_ends);
            let break_at = nearest_of(src, j, &LINE_BREAK, &mut cursors.line_breaks);

            let wraps = matches!((delim_at, break_at), (Some(d), Some(b)) if b < d);

            if let (true, Some(d), Some(b)) = (wraps, delim_at, break_at)
                && skip_flow_space(src, b, n) < d
            {
                folded = true;
                j = plain_scalar_end(src, j, b);
                can_open_quote = false;
                tagged = false;
                continue;
            }

            let limit = if wraps { break_at } else { delim_at };
            let plain_end = limit.map(|l| plain_scalar_end(src, j, l));
            let frame = stack.last().expect("frame");
            let colon = match plain_end {
                Some(pe) if pe > j && frame.seq && frame.key.is_none() => flow_key_colon(src, j, pe),
                _ => None,
            };

            if let Some(colon) = colon {
                let tag_arg = if tagged { tag.clone() } else { None };
                let text = plain_key_text(src, j, colon);
                let len = spans.len();
                name_flow_key(stack.last_mut().expect("frame"), tag_arg.as_deref(), text, len);
                j = colon + 1;
                can_open_quote = true;
                tagged = false;
                continue;
            }

            if let Some(pe) = plain_end
                && pe > j
            {
                if !folded && !tagged && resolves_to_string(&src[j..pe]) {
                    spans.push(LocatedSpan::with(
                        j,
                        pe,
                        false,
                        flow_element_chain(Some(frame), base),
                        true,
                        Vec::new(),
                        Some(flow_scalar_safe),
                    ));
                }

                j = pe;
                can_open_quote = false;
                continue;
            }
        }

        let token_start = j;

        while j < n && (!is_flow_structural(src[j]) || (src[j] == COLON && !flow_colon_separates(src, j))) {
            j += 1;
        }

        let frame = stack.last();

        if j == token_start {
            j += 1;
        } else if (awaits_key(frame) || frame.is_some_and(|f| f.explicit)) && key_start.is_none() {
            key_start = Some(token_start);
        }

        can_open_quote = false;
    }

    Ok(None)
}

fn flow_collection_end(
    src: &[u16],
    pos: usize,
    n: usize,
    chain: &Chain,
    spans: &mut Vec<LocatedSpan>,
    check_budget: CheckBudget<'_>,
    cursors: &mut Cursors,
) -> Result<usize> {
    match scan_flow_collection(src, pos, n, chain, spans, check_budget, cursors)? {
        Some(end) => Ok(end),
        None => Err(Error::input(format!("Unclosed YAML flow collection at offset {pos}"), json!({ "offset": pos }))),
    }
}

fn separates(src: &[u16], at: usize, line_end: usize) -> bool {
    at == line_end || src.get(at) == Some(&SP) || src.get(at) == Some(&TAB)
}

fn find_key_colon(src: &[u16], pos: usize, line_end: usize) -> Option<usize> {
    (pos..line_end).find(|&j| src[j] == COLON && separates(src, j + 1, line_end))
}

fn is_quote(c: Option<u16>) -> bool {
    c == Some(DQ) || c == Some(SQ)
}

fn is_flow_open(c: Option<u16>) -> bool {
    c == Some(LBRACKET) || c == Some(LBRACE)
}

struct BlockFrame {
    indent: usize,
    chain: Chain,
    seq_index: usize,
    indentless: bool,
}

impl BlockFrame {
    fn new(indent: usize, chain: Chain) -> Self {
        BlockFrame { indent, chain, seq_index: 0, indentless: false }
    }
}

struct Scanner<'a> {
    src: &'a [u16],
    n: usize,
    check_budget: CheckBudget<'a>,
    spans: Vec<LocatedSpan>,
    stack: Vec<BlockFrame>,
    cursors: Option<Cursors>,
    pending_chain: Chain,
    run_indent: isize,
    run_chain: Chain,
    run_typeset: bool,
    run_pending_node: bool,
    run_inclusive: bool,
    run_items: bool,
    run_block_indent: isize,
    explicit_key: Option<KeyName>,
    explicit_key_end: isize,
    explicit_key_below: bool,
    explicit_key_tag: Option<JsString>,
    open_key_indent: isize,
    unnamed_from: Option<usize>,
    unnamed_indent: usize,
    skip_to: Option<usize>,
    mid_line: bool,
}

impl Scanner<'_> {
    fn line_content_end(&self, pos: usize) -> usize {
        let src = self.src;
        let stop = js::index_of_unit(src, LF, pos).unwrap_or(self.n);

        if stop > pos && src[stop - 1] == CR { stop - 1 } else { stop }
    }

    fn run_limit(&self, from: usize, indent: isize) -> usize {
        let src = self.src;
        let n = self.n;
        let mut line_start = from;

        while line_start < n {
            let end = self.line_content_end(line_start);
            let p = skip_spaces(src, line_start, end);

            if p < end && (p - line_start) as isize <= indent {
                return line_start;
            }

            match js::index_of_unit(src, LF, line_start) {
                None => return n,
                Some(brk) => line_start = brk + 1,
            }
        }

        n
    }

    fn simple_key_text(&self, kp: usize, line_end: usize, key_indent: isize) -> (Option<JsString>, usize) {
        let src = self.src;

        if kp >= line_end {
            return (None, line_end);
        }

        if src[kp] == DQ || src[kp] == SQ {
            let quoted = scan_quoted(src, kp, line_end)
                .or_else(|| scan_quoted(src, kp, self.run_limit(line_end + 1, key_indent)));

            let Some(quoted) = quoted else { return (None, line_end) };

            let stop = self.line_content_end(quoted.end);
            let after = skip_spaces(src, quoted.end, stop);
            let named = after == stop || (after > quoted.end && src.get(after) == Some(&HASH));

            return (named.then(|| quoted_key_name(&src[kp + 1..quoted.end - 1], src[kp])), quoted.end);
        }

        let text_end = plain_scalar_end(src, kp, line_end);
        let text = &src[kp..text_end];
        let scalar = !text.is_empty() && !plain_indicator_test(text) && find_key_colon(src, kp, text_end).is_none();

        (scalar.then(|| text.to_vec()), line_end)
    }

    fn after_node_property(&self, pos: usize, line_end: usize) -> usize {
        let src = self.src;
        let mut p = pos;

        while p < line_end && (src[p] == AMP || src[p] == BANG) {
            while p < line_end && src[p] != SP && src[p] != TAB {
                p += 1;
            }

            p = skip_spaces(src, p, line_end);
        }

        if p < line_end && src[p] == HASH { line_end } else { p }
    }

    fn key_name(&self, pos: usize, node_start: usize, text: JsString) -> KeyName {
        if node_start == pos {
            KeyName::Named(text)
        } else {
            typed_key_name(property_tag(self.src, pos, node_start).as_deref(), text)
        }
    }

    fn key_chain(&mut self, chain: &Chain, name: KeyName, key_indent: usize) -> Chain {
        if name == KeyName::Unnamed && self.unnamed_from.is_none() {
            self.unnamed_from = Some(self.spans.len());
            self.unnamed_indent = key_indent;
        }

        chain_append(chain, [key_position(&name)])
    }

    fn block_content_indent(&self, pos: usize, line_end: usize, indent: usize) -> isize {
        for &ch in self.src.iter().take(line_end).skip(pos) {
            if (0x31..=0x39).contains(&ch) {
                return (indent + usize::from(ch - 0x30)) as isize;
            }

            if ch != 0x2B && ch != DASH {
                return -1;
            }
        }

        -1
    }

    fn check_block_scalar(&mut self, pos: usize, line_end: usize, indent: usize, chain: Chain) -> bool {
        let ch = self.src.get(pos).copied();

        if ch != Some(0x7C) && ch != Some(GT) {
            return false;
        }

        self.run_indent = indent as isize;
        self.run_chain = chain;
        self.run_typeset = true;
        self.run_inclusive = false;
        self.run_block_indent = self.block_content_indent(pos + 1, line_end, indent);

        true
    }

    fn claim_property_run(&mut self, indent: isize, chain: Chain, inclusive: bool) {
        self.run_indent = indent;
        self.run_chain = chain;
        self.run_typeset = false;
        self.run_pending_node = true;
        self.run_inclusive = inclusive;
        self.run_items = false;
    }

    fn record_plain_scalar(
        &mut self,
        pos: usize,
        line_end: usize,
        indent: usize,
        chain: Chain,
        own_line: bool,
        owner_indent: isize,
    ) {
        let src = self.src;

        if self.mid_line || self.run_indent != -1 {
            return;
        }

        let value_start = self.after_node_property(pos, line_end);

        if value_start >= line_end {
            if value_start > pos && owner_indent != -1 {
                self.claim_property_run(owner_indent, chain, false);
                self.run_items = true;
            } else if value_start > pos {
                self.claim_property_run(indent as isize, chain, own_line);
            }

            return;
        }

        if is_plain_indicator(src[value_start]) {
            return;
        }

        if folds_onto_next_line(src, line_end, self.n, indent) {
            self.run_indent = indent as isize;
            self.run_typeset = false;

            return;
        }

        if value_start != pos {
            return;
        }

        let end = plain_scalar_end(src, pos, line_end);

        if resolves_to_string(&src[pos..end]) {
            self.spans.push(LocatedSpan::with(pos, end, false, chain, true, Vec::new(), Some(plain_scalar_safe)));
        }
    }

    fn flow_end(&mut self, pos: usize, keep: bool, chain: Chain) -> Result<usize> {
        let src = self.src;
        let cursors = self.cursors.get_or_insert_with(|| new_delimiter_cursors(src));

        if keep {
            flow_collection_end(src, pos, self.n, &chain, &mut self.spans, self.check_budget, cursors)
        } else {
            let mut sink = Vec::new();
            flow_collection_end(src, pos, self.n, &None, &mut sink, self.check_budget, cursors)
        }
    }

    fn parse_value_after_colon(&mut self, pos: usize, line_end: usize, chain: Chain, indent: usize) -> Result<bool> {
        let src = self.src;
        let n = self.n;

        if pos >= line_end || src[pos] == HASH {
            self.open_key_indent = indent as isize;

            return Ok(false);
        }

        let ch = src[pos];
        let mut value_start = self.after_node_property(pos, line_end);
        let mut quote = src.get(value_start).copied();
        let mut result = if is_quote(quote) { scan_quoted(src, value_start, line_end) } else { None };

        if let Some(r) = result
            && value_start > pos
            && has_trailing_line_tokens(src, r.end, line_end)
        {
            value_start = pos;
            quote = Some(src[pos]);
            result = None;
        }

        if is_quote(quote) {
            if let Some(r) = result {
                assert_no_trailing_line_tokens(src, r.end, line_end)?;

                let eligible = r.eligible && value_start == pos;

                self.spans.push(LocatedSpan::with(
                    value_start,
                    r.end,
                    false,
                    chain,
                    eligible,
                    vec![quote.expect("quote")],
                    None,
                ));
            } else {
                self.skip_to = Some(scan_quoted(src, value_start, n).map_or(n, |q| q.end));
            }

            return Ok(true);
        }

        if ch == LBRACKET || ch == LBRACE {
            self.skip_to = Some(self.flow_end(pos, true, chain.clone())?);
        } else if is_flow_open(src.get(value_start).copied()) {
            self.skip_to = Some(self.flow_end(value_start, false, None)?);
        }

        self.check_block_scalar(value_start, line_end, indent, chain.clone());

        if value_start >= line_end && value_start > pos {
            self.claim_property_run(indent as isize, chain, false);
            self.run_items = true;

            return Ok(true);
        }

        self.record_plain_scalar(pos, line_end, indent, chain, true, -1);

        Ok(true)
    }

    fn parse_key_or_value(
        &mut self,
        pos: usize,
        line_end: usize,
        chain: Chain,
        indent: usize,
        key_indent: usize,
        owner_indent: isize,
    ) -> Result<bool> {
        let src = self.src;
        let n = self.n;
        let ch = src[pos];
        let pending_key = self.explicit_key.take();

        if ch == QM && separates(src, pos + 1, line_end) {
            let key_start = skip_spaces(src, pos + 1, line_end);
            let kp = self.after_node_property(key_start, line_end);

            if kp < line_end && (src[kp] == LBRACKET || src[kp] == LBRACE) {
                self.skip_to = Some(self.flow_end(kp, false, None)?);
            }

            let (text, end) = self.simple_key_text(kp, line_end, key_indent as isize);

            self.explicit_key_end = end as isize;
            self.explicit_key_below = kp >= line_end;
            self.explicit_key_tag = if self.explicit_key_below { property_tag(src, key_start, line_end) } else { None };

            self.explicit_key = Some(match text {
                Some(text) => self.key_name(key_start, kp, text),
                None if self.explicit_key_below => typed_key_name(self.explicit_key_tag.as_deref(), Vec::new()),
                None => KeyName::Unnamed,
            });

            self.run_indent = key_indent as isize;
            self.run_typeset = false;
            self.pending_chain = chain;

            return Ok(false);
        }

        let mut node_start = self.after_node_property(pos, line_end);
        let mut quote = src.get(node_start).copied();
        let result = if is_quote(quote) { scan_quoted(src, node_start, line_end) } else { None };
        let cp = result.map_or(line_end, |r| skip_spaces(src, r.end, line_end));
        let is_key_here = src.get(cp) == Some(&COLON) && separates(src, cp + 1, line_end);

        if let Some(r) = result
            && node_start > pos
            && !is_key_here
            && has_trailing_line_tokens(src, r.end, line_end)
        {
            node_start = pos;
            quote = Some(src[pos]);
        }

        if is_quote(quote) {
            let q = quote.expect("quote");
            let Some(result) = result else {
                self.skip_to = Some(scan_quoted(src, node_start, n).map_or(n, |q| q.end));
                self.pending_chain = chain;

                return Ok(false);
            };

            if !is_key_here {
                assert_no_trailing_line_tokens(src, result.end, line_end)?;

                let eligible = result.eligible && node_start == pos;

                self.spans.push(LocatedSpan::with(node_start, result.end, false, chain, eligible, vec![q], None));

                return Ok(false);
            }

            let raw_key = &src[node_start + 1..result.end - 1];
            let name = self.key_name(pos, node_start, decode_quoted_key(raw_key, q));
            let full_chain = self.key_chain(&chain, name, key_indent);

            self.spans.push(LocatedSpan::with(
                node_start,
                result.end,
                true,
                full_chain.clone(),
                result.eligible,
                vec![q],
                None,
            ));
            self.pending_chain.clone_from(&full_chain);

            let vp = skip_spaces(src, cp + 1, line_end);

            return self.parse_value_after_colon(vp, line_end, full_chain, key_indent);
        }

        if ch == LBRACKET || ch == LBRACE || is_flow_open(src.get(node_start).copied()) {
            let opaque = node_start != pos;

            self.skip_to = Some(self.flow_end(node_start, !opaque, if opaque { None } else { chain.clone() })?);
            self.pending_chain = chain;

            return Ok(false);
        }

        let Some(colon) = find_key_colon(src, node_start, line_end) else {
            self.check_block_scalar(node_start, line_end, indent, chain.clone());
            self.record_plain_scalar(pos, line_end, indent, chain.clone(), key_indent == indent, owner_indent);
            self.pending_chain = chain;

            return Ok(false);
        };

        let written = plain_key_text(src, node_start, colon);
        let name = match pending_key {
            Some(key) if written.is_empty() => key,
            _ => self.key_name(pos, node_start, written),
        };
        let full_chain = self.key_chain(&chain, name, key_indent);

        self.pending_chain.clone_from(&full_chain);

        let vp = skip_spaces(src, colon + 1, line_end);

        self.parse_value_after_colon(vp, line_end, full_chain, key_indent)
    }

    fn process_line(&mut self, p: usize, line_end: usize, indent: usize, line_start: usize) -> Result<()> {
        let src = self.src;

        if (js::starts_with_at(src, &[DASH, DASH, DASH], p) || js::starts_with_at(src, &[0x2E, 0x2E, 0x2E], p))
            && separates(src, p + 3, line_end)
        {
            self.stack.truncate(1);
            self.stack[0] = BlockFrame::new(0, None);
            self.pending_chain = None;
            self.explicit_key = None;

            let vp = skip_spaces(src, p + 3, line_end);

            self.parse_value_after_colon(vp, line_end, None, 0)?;

            return Ok(());
        }

        let key_above = self.open_key_indent;
        let marks_item = |at: usize| src.get(at) == Some(&DASH) && separates(src, at + 1, line_end);
        let item = marks_item(p);

        self.open_key_indent = -1;

        while self.stack.len() > 1 && self.stack.last().expect("root").indent > indent {
            self.stack.pop();
        }

        let top = self.stack.last().expect("root");
        if top.indentless && top.indent == indent && !item {
            self.stack.pop();
        }

        if self.stack.last().expect("root").indent < indent {
            self.stack.push(BlockFrame::new(indent, self.pending_chain.clone()));
        } else if item && key_above == indent as isize {
            let mut frame = BlockFrame::new(indent, self.pending_chain.clone());
            frame.indentless = true;
            self.stack.push(frame);
        }

        let frame_at = self.stack.len() - 1;

        if item {
            self.explicit_key = None;

            let mut holder = frame_at;
            let mut marker = p;
            let mut item_chain: Chain;
            let mut vp: usize;

            loop {
                item_chain = chain_append(&self.stack[holder].chain, [Position::Index(self.stack[holder].seq_index)]);
                self.stack[holder].seq_index += 1;
                vp = skip_spaces(src, marker + 1, line_end);

                if vp >= line_end || !marks_item(vp) {
                    break;
                }

                self.stack.push(BlockFrame::new(vp - line_start, item_chain.clone()));
                holder = self.stack.len() - 1;
                marker = vp;
            }

            if vp >= line_end {
                self.pending_chain = item_chain;
                self.explicit_key = None;

                return Ok(());
            }

            self.stack.push(BlockFrame::new(vp - line_start, item_chain.clone()));

            self.parse_key_or_value(vp, line_end, item_chain, marker - line_start, vp - line_start, -1)?;

            return Ok(());
        }

        let chain = self.stack[frame_at].chain.clone();
        let owner = if key_above < indent as isize { key_above } else { -1 };

        self.parse_key_or_value(p, line_end, chain, indent, indent, owner)?;

        Ok(())
    }

    fn run(&mut self) -> Result<()> {
        let src = self.src;
        let n = self.n;
        let mut lines: u64 = 0;
        let mut i = usize::from(src.first() == Some(&0xFEFF));
        let mut resumed = false;
        let mut line_start;
        let mut line_indent;

        while i < n {
            if lines & 0x3ff == 0 {
                (self.check_budget)("yaml-scan")?;
            }
            lines += 1;

            let line_end = js::index_of_unit(src, LF, i).unwrap_or(n);
            let next = if line_end < n { line_end + 1 } else { line_end };
            let mut end = line_end;

            if end > i && src[end - 1] == CR {
                end -= 1;
            }

            let p = skip_spaces(src, i, end);

            if resumed {
                line_start = js::last_index_of_from(src, &[LF], i.saturating_sub(1)).map_or(0, |b| b + 1);
                line_indent = skip_spaces(src, line_start, end) - line_start;
                resumed = false;
            } else {
                line_start = i;
                line_indent = p - i;
            }

            let indent = line_indent;
            let blank = p == end;

            self.mid_line = i != line_start;

            let comment = !blank && src[p] == HASH;

            if let Some(from) = self.unnamed_from
                && !blank
                && !comment
                && !self.mid_line
                && (indent < self.unnamed_indent
                    || (indent == self.unnamed_indent && !(src[p] == DASH && separates(src, p + 1, end))))
            {
                mark_unaddressed(&mut self.spans, from);
                self.unnamed_from = None;
            }

            if self.run_indent != -1 {
                let at = indent as isize;

                if self.run_typeset && self.run_block_indent == -1 && !blank && at > self.run_indent {
                    self.run_block_indent = at;
                }

                let at_run_column = at == self.run_indent;
                let inside = if self.run_typeset {
                    blank || (self.run_block_indent != -1 && at >= self.run_block_indent)
                } else {
                    blank
                        || comment
                        || at > self.run_indent
                        || (self.run_inclusive && at_run_column)
                        || (self.run_items && at_run_column && src[p] == DASH && separates(src, p + 1, end))
                };

                if inside {
                    if !blank && self.run_pending_node && src[p] != HASH {
                        let node = self.after_node_property(p, end);

                        if node < end {
                            self.run_pending_node = false;

                            let run_chain = self.run_chain.clone();

                            if self.check_block_scalar(node, end, indent, run_chain) {
                                i = next;
                                continue;
                            }
                        }
                    }

                    if !blank && self.run_typeset {
                        self.spans.push(LocatedSpan::with(
                            p,
                            end,
                            false,
                            self.run_chain.clone(),
                            true,
                            Vec::new(),
                            None,
                        ));
                    }

                    if !blank && self.explicit_key_below && src[p] != HASH {
                        let node = self.after_node_property(p, end);

                        self.explicit_key_tag = property_tag(src, p, node).or(self.explicit_key_tag.take());

                        if node < end {
                            let (text, key_end) = self.simple_key_text(node, end, self.run_indent);

                            self.explicit_key_below = false;
                            self.explicit_key = Some(match text {
                                None => KeyName::Unnamed,
                                Some(text) => typed_key_name(self.explicit_key_tag.as_deref(), text),
                            });
                            self.explicit_key_end = key_end as isize;
                        } else {
                            self.explicit_key = Some(typed_key_name(self.explicit_key_tag.as_deref(), Vec::new()));
                        }
                    }

                    if self.explicit_key.is_some()
                        && !self.explicit_key_below
                        && !blank
                        && src[p] != HASH
                        && i as isize >= self.explicit_key_end
                    {
                        self.explicit_key = Some(KeyName::Unnamed);
                    }

                    i = next;
                    continue;
                }

                self.run_indent = -1;
                self.run_pending_node = false;
                self.run_inclusive = false;
                self.run_items = false;
                self.run_block_indent = -1;
                self.explicit_key_below = false;
            }

            if !blank && src[p] != HASH {
                self.process_line(p, end, indent, line_start)?;
            }

            if let Some(to) = self.skip_to.take() {
                i = to;
                resumed = true;
                continue;
            }

            i = next;
        }

        if let Some(from) = self.unnamed_from {
            mark_unaddressed(&mut self.spans, from);
        }

        Ok(())
    }
}

pub fn scan_yaml(source: &[u16], check_budget: CheckBudget<'_>) -> Result<Vec<LocatedSpan>> {
    let src = line_view(source);
    let mut scanner = Scanner {
        src: &src,
        n: src.len(),
        check_budget,
        spans: Vec::new(),
        stack: vec![BlockFrame::new(0, None)],
        cursors: None,
        pending_chain: None,
        run_indent: -1,
        run_chain: None,
        run_typeset: false,
        run_pending_node: false,
        run_inclusive: false,
        run_items: false,
        run_block_indent: -1,
        explicit_key: None,
        explicit_key_end: -1,
        explicit_key_below: false,
        explicit_key_tag: None,
        open_key_indent: -1,
        unnamed_from: None,
        unnamed_indent: 0,
        skip_to: None,
        mid_line: false,
    };

    scanner.run()?;

    Ok(scanner.spans)
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;

    use super::*;

    fn dump(src: &str) -> String {
        let labels = RefCell::new(Vec::<String>::new());
        let budget = |l: &str| -> Result<()> {
            labels.borrow_mut().push(l.to_owned());
            Ok(())
        };
        let body = match scan_yaml(&js::u(src), &budget) {
            Ok(spans) => spans
                .iter()
                .map(|s| {
                    let guard = s.guard.map_or("", |g| if g(&js::u("a, b")) { "plain" } else { "flow" });
                    format!(
                        "{},{},{},{},{},{},{},{}",
                        s.start,
                        s.end,
                        if s.is_key { "K" } else { "V" },
                        if s.eligible { "E" } else { "-" },
                        js::lossy(&s.delim),
                        js::lossy(&s.path()),
                        if s.unaddressed { "U" } else { "" },
                        guard
                    )
                })
                .collect::<Vec<_>>()
                .join("|"),
            Err(e) => format!("ERR:{}:{}", e.message, e.details),
        };
        format!("{body}#{}", labels.borrow().join(","))
    }

    const CASES: &[(&str, &str)] = &[
        ("title: Привет мир\n", "7,17,V,E,,title,,plain#yaml-scan"),
        ("\"ключ\": 'значение - тест'\n", "0,6,K,E,\",ключ,,|8,25,V,E,',ключ,,#yaml-scan"),
        ("a: \"x\\ny\"\n", "3,9,V,-,\",a,,#yaml-scan"),
        (
            "body: |\n  Первая строка\n  вторая строка\nnext: да нет\n",
            "10,23,V,E,,body,,|26,39,V,E,,body,,|46,52,V,E,,next,,plain#yaml-scan",
        ),
        (
            "list:\n  - один два\n  - [три, \"четыре\", {к: пять шесть}]\n",
            "10,18,V,E,,list.0,,plain|24,27,V,E,,list.1.0,,flow|29,37,V,E,\",list.1.1,,|43,53,V,E,,list.1.2.к,,flow#yaml-scan,yaml-flow-scan",
        ),
        ("a: &anc текст тут\nb: !tag \"строка\"\nc: !!str простой текст\n", "26,34,V,-,\",b,,#yaml-scan"),
        ("# comment\nkey: value text # trailing\n", "15,25,V,E,,key,,plain#yaml-scan"),
        (
            "---\nfirst: документ один\n...\n---\nsecond: другой документ\n",
            "11,24,V,E,,first,,plain|41,56,V,E,,second,,plain#yaml-scan",
        ),
        ("? сложный ключ\n: значение тут\n", "17,29,V,E,,сложный ключ,,plain#yaml-scan"),
        (
            "!!int 0x10: значение тут\nnext: ещё текст\n",
            "12,24,V,E,,Symbol(unnamed key),U,plain|31,40,V,E,,next,,plain#yaml-scan",
        ),
        ("a: [1, 2\n", "ERR:Unclosed YAML flow collection at offset 3:{\"offset\":3}#yaml-scan,yaml-flow-scan"),
        (
            "a: \"x\" trailing\n",
            "ERR:Unexpected trailing content after YAML scalar at offset 7:{\"offset\":7}#yaml-scan",
        ),
        ("- - вложенный элемент\n  - второй элемент\n", "4,21,V,E,,0.0,,plain|26,40,V,E,,0.1,,plain#yaml-scan"),
        ("key: plain\n  continues here\nb: c d\n", "31,34,V,E,,b,,plain#yaml-scan"),
        (
            "{\"a\":b c, [x, y]: z w}\n",
            "1,4,K,E,\",,,|5,8,V,E,,a,,flow|18,21,V,E,,[x, y],,flow#yaml-scan,yaml-flow-scan",
        ),
        ("num: 123\nbool: yes\nword: Арден\n", "25,30,V,E,,word,,plain#yaml-scan"),
        ("\u{feff}k: v w\r\nk2: 'q'\r\n", "4,7,V,E,,k,,plain|13,16,V,E,',k2,,#yaml-scan"),
        (
            "seq:\n- item один\n- item два\nnext: x y\n",
            "7,16,V,E,,seq.0,,plain|19,27,V,E,,seq.1,,plain|34,37,V,E,,next,,plain#yaml-scan",
        ),
        ("[? ключ: значение, a b]\n", "9,17,V,E,,0.ключ,,flow|19,22,V,E,,1,,flow#yaml-scan,yaml-flow-scan"),
        (
            "m: {key with space: value here, \"q\": 'w', empty:}\n",
            "20,30,V,E,,m.key with space,,flow|32,35,K,E,\",m,,|37,40,V,E,',m.q,,#yaml-scan,yaml-flow-scan",
        ),
        ("x: >-\n  folded текст\n\n  ещё\ny: z\n", "8,20,V,E,,x,,|24,27,V,E,,x,,|31,32,V,E,,y,,plain#yaml-scan"),
        ("\"multi\n  line\": v w\n", "#yaml-scan"),
    ];

    #[test]
    fn matches_reference() {
        for (src, expected) in CASES {
            assert_eq!(dump(src), *expected, "source {src:?}");
        }
    }

    #[test]
    fn guards() {
        assert!(plain_scalar_safe(&js::u("Корвин — принц")));
        assert!(!plain_scalar_safe(&js::u("no")));
        assert!(!plain_scalar_safe(&js::u("a: b")));
        assert!(!plain_scalar_safe(&js::u(" a")));
        assert!(!flow_scalar_safe(&js::u("a, b")));
    }
}
