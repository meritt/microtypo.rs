use std::collections::HashMap;

use serde_json::json;

use crate::errors::{Error, Result};
use crate::input::span_path::{Chain, LocatedSpan, Position, chain_append};
use crate::js::{self, JsString};
use crate::protect::blocks::CheckBudget;
use crate::shared::escape_decoder::decode_escapes;
use crate::shared::strings::{is_flow_space, skip_spaces};

const DQ: u16 = 0x22;
const SQ: u16 = 0x27;
const BACKSLASH: u16 = 0x5C;
const LF: u16 = 0x0A;
const HASH: u16 = 0x23;

fn is_bare_key_char(c: Option<u16>) -> bool {
    matches!(c, Some(0x41..=0x5A | 0x61..=0x7A | 0x30..=0x39 | 0x5F | 0x2D))
}

struct QuotedSpan {
    start: usize,
    end: usize,
    delim: JsString,
    multiline: bool,
    eligible: bool,
}

fn parse_quoted_literal(src: &[u16], i: usize, n: usize) -> Result<QuotedSpan> {
    let q = src[i];
    let start = i;

    if src.get(i + 1) == Some(&q) && src.get(i + 2) == Some(&q) {
        let delim = vec![q, q, q];
        let mut j = i + 3;
        let mut has_backslash = false;

        while j < n {
            if q == DQ && src[j] == BACKSLASH {
                has_backslash = true;
                j += 2;
                continue;
            }

            if js::starts_with_at(src, &delim, j) {
                j += 3;
                while src.get(j) == Some(&q) {
                    j += 1;
                }

                return Ok(QuotedSpan { start, end: j, delim, multiline: true, eligible: q == SQ || !has_backslash });
            }

            j += 1;
        }

        return Err(Error::input(
            format!("Unterminated TOML multiline string at offset {start}"),
            json!({ "offset": start }),
        ));
    }

    let mut j = i + 1;
    let mut has_backslash = false;

    while j < n {
        let c = src[j];

        if c == LF {
            break;
        }

        if q == DQ && c == BACKSLASH {
            has_backslash = true;
            j += 2;
            continue;
        }

        if c == q {
            return Ok(QuotedSpan {
                start,
                end: j + 1,
                delim: vec![q],
                multiline: false,
                eligible: q == SQ || !has_backslash,
            });
        }

        j += 1;
    }

    Err(Error::input(format!("Unterminated TOML string literal at offset {start}"), json!({ "offset": start })))
}

fn assert_no_trailing_line_content(src: &[u16], pos: usize, n: usize) -> Result<()> {
    let mut p = pos;

    while p < n && matches!(src[p], 0x20 | 0x09 | 0x0D) {
        p += 1;
    }

    if p < n && src[p] != LF && src[p] != HASH {
        return Err(Error::input(
            format!("Unexpected trailing content after TOML value at offset {p}"),
            json!({ "offset": p }),
        ));
    }

    Ok(())
}

const TOML_ESCAPE: [(u16, &str); 8] = [
    (0x62, "\u{8}"),
    (0x74, "\t"),
    (0x6E, "\n"),
    (0x66, "\u{c}"),
    (0x72, "\r"),
    (0x65, "\u{1b}"),
    (0x22, "\""),
    (0x5C, "\\"),
];

const TOML_HEX: [(u16, usize); 3] = [(0x78, 2), (0x75, 4), (0x55, 8)];

fn decode_quoted_key(raw: &[u16], quote_char: u16) -> JsString {
    if quote_char == SQ { raw.to_vec() } else { decode_escapes(raw, &TOML_ESCAPE, &TOML_HEX) }
}

#[derive(Default)]
struct AotEntry {
    index: Option<usize>,
    children: HashMap<JsString, AotEntry>,
}

fn resolve_header(segments: &[JsString], is_array_header: bool, root: &mut HashMap<JsString, AotEntry>) -> Chain {
    let mut chain: Chain = None;
    let mut node = root;

    for (k, seg) in segments.iter().enumerate() {
        chain = chain_append(&chain, [Position::key(seg)]);

        let entry = node.entry(seg.clone()).or_default();

        if is_array_header && k == segments.len() - 1 {
            let index = entry.index.map_or(0, |x| x + 1);
            entry.index = Some(index);
            entry.children = HashMap::new();

            return chain_append(&chain, [Position::Index(index)]);
        }

        if let Some(index) = entry.index {
            chain = chain_append(&chain, [Position::Index(index)]);
        }

        node = &mut entry.children;
    }

    chain
}

struct KeySegment {
    text: JsString,
    end: usize,
    quoted_span: Option<QuotedSpan>,
}

fn read_key_segment(src: &[u16], i: usize, n: usize) -> Result<KeySegment> {
    let ch = src.get(i).copied();

    if let Some(q @ (DQ | SQ)) = ch {
        let span = parse_quoted_literal(src, i, n)?;
        let end = span.end;

        if span.multiline {
            return Ok(KeySegment { text: js::slice(src, i, end).to_vec(), end, quoted_span: None });
        }

        return Ok(KeySegment {
            text: decode_quoted_key(js::slice(src, i + 1, end - 1), q),
            end,
            quoted_span: Some(span),
        });
    }

    let mut j = i;

    while j < n && is_bare_key_char(src.get(j).copied()) {
        j += 1;
    }

    Ok(KeySegment { text: js::slice(src, i, j).to_vec(), end: j, quoted_span: None })
}

struct KeyPath {
    segments: Vec<JsString>,
    end: usize,
    quoted_spans: Vec<QuotedSpan>,
}

fn scan_key_path(src: &[u16], i: usize, n: usize) -> Result<KeyPath> {
    let mut segments = Vec::new();
    let mut quoted_spans = Vec::new();
    let mut pos = i;

    loop {
        pos = skip_spaces(src, pos, n);

        let seg = read_key_segment(src, pos, n)?;

        if seg.quoted_span.is_none() && seg.text.is_empty() {
            return Err(Error::input(format!("Empty TOML key segment at offset {pos}"), json!({ "offset": pos })));
        }

        segments.push(seg.text);

        if let Some(qs) = seg.quoted_span {
            quoted_spans.push(qs);
        }

        pos = seg.end;

        pos = skip_spaces(src, pos, n);

        if src.get(pos) != Some(&0x2E) {
            break;
        }

        pos += 1;
    }

    Ok(KeyPath { segments, end: pos, quoted_spans })
}

#[derive(PartialEq, Eq)]
enum FrameType {
    Array,
    Table,
}

struct Frame {
    kind: FrameType,
    index: usize,
    key_segments: Option<Vec<JsString>>,
    base: Chain,
}

fn element_chain(frame: &Frame) -> Chain {
    match frame.kind {
        FrameType::Array => chain_append(&frame.base, [Position::Index(frame.index)]),
        FrameType::Table => chain_append(&frame.base, frame.key_segments.iter().flatten().map(|s| Position::key(s))),
    }
}

fn scan_inline_collection(
    src: &[u16],
    i: usize,
    n: usize,
    base: &Chain,
    spans: &mut Vec<LocatedSpan>,
    check_budget: CheckBudget<'_>,
) -> Result<usize> {
    let mut stack: Vec<Frame> = Vec::new();
    let mut j = i;
    let mut steps = 0usize;

    while j < n {
        if steps & 0x3fff == 0 {
            check_budget("toml-inline-scan")?;
        }
        steps += 1;

        let c = src[j];

        if c == HASH {
            while j < n && src[j] != LF {
                j += 1;
            }

            continue;
        }

        if is_flow_space(Some(c)) {
            j += 1;
            continue;
        }

        if c == 0x5B || c == 0x7B {
            let frame_base = stack.last().map_or_else(|| base.clone(), element_chain);
            stack.push(Frame {
                kind: if c == 0x5B { FrameType::Array } else { FrameType::Table },
                index: 0,
                key_segments: None,
                base: frame_base,
            });
            j += 1;
            continue;
        }

        let frame = stack.last_mut().expect("an inline collection opens with its opener");

        if c == 0x5D || c == 0x7D {
            if frame.kind != if c == 0x5D { FrameType::Array } else { FrameType::Table } {
                return Err(Error::input(
                    format!("Mismatched TOML inline collection closer '{}' at offset {j}", js::lossy(&[c])),
                    json!({ "offset": j }),
                ));
            }

            stack.pop();
            j += 1;

            if stack.is_empty() {
                return Ok(j);
            }

            continue;
        }

        if c == 0x2C {
            if frame.kind == FrameType::Array {
                frame.index += 1;
            } else {
                frame.key_segments = None;
            }

            j += 1;
            continue;
        }

        if frame.kind == FrameType::Table && frame.key_segments.is_none() {
            let key = scan_key_path(src, j, n)?;
            frame.key_segments = Some(key.segments);

            for qs in &key.quoted_spans {
                spans.push(LocatedSpan::with(
                    qs.start,
                    qs.end,
                    true,
                    element_chain(frame),
                    qs.eligible,
                    Vec::new(),
                    None,
                ));
            }

            j = key.end;
            continue;
        }

        if c == DQ || c == SQ {
            let span = parse_quoted_literal(src, j, n)?;
            let end = span.end;

            spans.push(LocatedSpan::with(
                span.start,
                span.end,
                false,
                element_chain(frame),
                span.eligible,
                span.delim,
                None,
            ));

            j = end;
            continue;
        }

        j += 1;
    }

    Err(Error::input(format!("Unterminated TOML inline collection at offset {i}"), json!({ "offset": i })))
}

pub fn scan_toml(src: &[u16], check_budget: CheckBudget<'_>) -> Result<Vec<LocatedSpan>> {
    if let Some(offset) =
        src.iter().enumerate().find_map(|(i, &c)| (c == 13 && src.get(i + 1) != Some(&10)).then_some(i))
    {
        return Err(Error::input(
            "TOML newlines must use LF or CRLF; bare CR is not permitted",
            json!({"offset":offset,"reason":"toml-bare-cr"}),
        ));
    }
    let mut spans = Vec::new();
    let n = src.len();
    let mut i = 0;
    let mut table_chain: Chain = None;
    let mut at_line_start = true;
    let mut steps = 0usize;
    let mut aot_tree: HashMap<JsString, AotEntry> = HashMap::new();

    while i < n {
        if steps & 0x3fff == 0 {
            check_budget("toml-scan")?;
        }
        steps += 1;

        let ch = src[i];

        if ch == 0x20 || ch == 0x09 || ch == 0x0D {
            i += 1;
            continue;
        }

        if ch == LF {
            i += 1;
            at_line_start = true;
            continue;
        }

        if ch == HASH {
            while i < n && src[i] != LF {
                i += 1;
            }

            continue;
        }

        if at_line_start && ch == 0x5B {
            let is_array_header = src.get(i + 1) == Some(&0x5B);
            let KeyPath { segments: header_segments, end, quoted_spans } =
                scan_key_path(src, i + if is_array_header { 2 } else { 1 }, n)?;

            table_chain = resolve_header(&header_segments, is_array_header, &mut aot_tree);

            for qs in &quoted_spans {
                spans.push(LocatedSpan::with(
                    qs.start,
                    qs.end,
                    true,
                    table_chain.clone(),
                    qs.eligible,
                    Vec::new(),
                    None,
                ));
            }

            let mut pos = skip_spaces(src, end, n);

            if is_array_header {
                if src.get(pos) == Some(&0x5D) && src.get(pos + 1) == Some(&0x5D) {
                    pos += 2;
                } else {
                    return Err(Error::input(
                        format!("Unterminated TOML array-of-tables header at offset {i}"),
                        json!({ "offset": i }),
                    ));
                }
            } else if src.get(pos) == Some(&0x5D) {
                pos += 1;
            } else {
                return Err(Error::input(
                    format!("Unterminated TOML table header at offset {i}"),
                    json!({ "offset": i }),
                ));
            }

            i = pos;
            at_line_start = false;
            continue;
        }

        at_line_start = false;

        let KeyPath { segments: key_segments, end: after_key, quoted_spans: key_quoted } = scan_key_path(src, i, n)?;
        let full_chain = chain_append(&table_chain, key_segments.iter().map(|s| Position::key(s)));

        for qs in &key_quoted {
            spans.push(LocatedSpan::with(qs.start, qs.end, true, full_chain.clone(), qs.eligible, Vec::new(), None));
        }

        let mut pos = skip_spaces(src, after_key, n);

        if src.get(pos) != Some(&0x3D) {
            while pos < n && src[pos] != LF {
                pos += 1;
            }

            i = pos;
            continue;
        }

        pos += 1;

        pos = skip_spaces(src, pos, n);

        let vch = src.get(pos).copied();

        if vch == Some(DQ) || vch == Some(SQ) {
            let span = parse_quoted_literal(src, pos, n)?;

            spans.push(LocatedSpan::with(
                span.start,
                span.end,
                false,
                full_chain.clone(),
                span.eligible,
                span.delim,
                None,
            ));

            assert_no_trailing_line_content(src, span.end, n)?;
            pos = span.end;
        } else if vch == Some(0x5B) || vch == Some(0x7B) {
            pos = scan_inline_collection(src, pos, n, &full_chain, &mut spans, check_budget)?;
            assert_no_trailing_line_content(src, pos, n)?;
        } else {
            while pos < n && src[pos] != LF && src[pos] != HASH {
                pos += 1;
            }
        }

        i = pos;
    }

    Ok(spans)
}

#[cfg(test)]
mod tests {
    use super::*;

    type Row = (usize, usize, bool, bool, String, Vec<String>);

    fn scan(src: &str) -> Result<Vec<Row>> {
        let spans = scan_toml(&js::u(src), &|_| Ok(()))?;
        Ok(spans
            .iter()
            .map(|s| {
                let segments = s.segments().iter().map(|p| js::lossy(p)).collect();
                (s.start, s.end, s.is_key, s.eligible, js::lossy(&s.delim), segments)
            })
            .collect())
    }

    fn row(start: usize, end: usize, is_key: bool, eligible: bool, delim: &str, segments: &[&str]) -> Row {
        (start, end, is_key, eligible, delim.to_owned(), segments.iter().map(|s| (*s).to_owned()).collect())
    }

    fn err(src: &str) -> (String, serde_json::Value) {
        let e = scan(src).unwrap_err();
        (e.message, e.details)
    }

    #[test]
    fn basic_and_literal_strings() {
        assert_eq!(
            scan("title = \"Hello\"\nname = 'lit'\n").unwrap(),
            [row(8, 15, false, true, "\"", &["title"]), row(23, 28, false, true, "'", &["name"])]
        );
    }

    #[test]
    fn multiline_strings_take_three_char_delims() {
        assert_eq!(
            scan("a = \"\"\"x\ny\"\"\"\nb = '''raw'''''\nc = \"\"\"q\\\"\"\"\" \n").unwrap(),
            [
                row(4, 13, false, true, "\"\"\"", &["a"]),
                row(18, 29, false, true, "'''", &["b"]),
                row(34, 43, false, false, "\"\"\"", &["c"]),
            ]
        );
    }

    #[test]
    fn backslash_makes_basic_string_ineligible() {
        assert_eq!(
            scan("a = \"x\\ny\"\nb = 'x\\ny'\n").unwrap(),
            [row(4, 10, false, false, "\"", &["a"]), row(15, 21, false, true, "'", &["b"])]
        );
    }

    #[test]
    fn tables_and_dotted_keys() {
        assert_eq!(
            scan("[server.http]\nhost.name = \"h\"\n\"quoted key\" = \"v\"\n").unwrap(),
            [
                row(26, 29, false, true, "\"", &["server", "http", "host", "name"]),
                row(30, 42, true, true, "", &["server", "http", "quoted key"]),
                row(45, 48, false, true, "\"", &["server", "http", "quoted key"]),
            ]
        );
    }

    #[test]
    fn arrays_of_tables_index_their_elements() {
        let src = "[[posts]]\ntitle = \"a\"\n[posts.meta]\nk = \"x\"\n[[posts]]\ntitle = \"b\"\n[[posts.tags]]\nn = \"t\"\n[[posts.tags]]\nn = \"u\"\n";
        assert_eq!(
            scan(src).unwrap(),
            [
                row(18, 21, false, true, "\"", &["posts", "0", "title"]),
                row(39, 42, false, true, "\"", &["posts", "0", "meta", "k"]),
                row(61, 64, false, true, "\"", &["posts", "1", "title"]),
                row(84, 87, false, true, "\"", &["posts", "1", "tags", "0", "n"]),
                row(107, 110, false, true, "\"", &["posts", "1", "tags", "1", "n"]),
            ]
        );
    }

    #[test]
    fn inline_tables_and_arrays() {
        assert_eq!(
            scan("pt = { x = \"1\", y = [\"a\", 'b', { z = \"c\" }] }\n").unwrap(),
            [
                row(11, 14, false, true, "\"", &["pt", "x"]),
                row(21, 24, false, true, "\"", &["pt", "y", "0"]),
                row(26, 29, false, true, "'", &["pt", "y", "1"]),
                row(37, 40, false, true, "\"", &["pt", "y", "2", "z"]),
            ]
        );
    }

    #[test]
    fn quoted_key_escapes() {
        let src = "\"\\u0074itle\" = \"x\"\n\"a\\eb\" = 'y'\n'lit\\u0074' = \"z\"\n\"\\q\" = \"w\"\n";
        assert_eq!(
            scan(src).unwrap(),
            [
                row(0, 12, true, false, "", &["title"]),
                row(15, 18, false, true, "\"", &["title"]),
                row(19, 25, true, false, "", &["a\u{1b}b"]),
                row(28, 31, false, true, "'", &["a\u{1b}b"]),
                row(32, 43, true, true, "", &["lit\\u0074"]),
                row(46, 49, false, true, "\"", &["lit\\u0074"]),
                row(50, 54, true, false, "", &["\\q"]),
                row(57, 60, false, true, "\"", &["\\q"]),
            ]
        );
    }

    #[test]
    fn comments_and_non_string_values() {
        assert_eq!(
            scan("# c\nk = \"v\" # trailing\nwords here\nn = 42 # c\nm = \"#\"\n").unwrap(),
            [row(8, 11, false, true, "\"", &["k"]), row(49, 52, false, true, "\"", &["m"])]
        );
        assert_eq!(
            scan("\"\" = \"x\"\n").unwrap(),
            [row(0, 2, true, true, "", &[""]), row(5, 8, false, true, "\"", &[""])]
        );
    }

    #[test]
    fn malformed_input_is_rejected() {
        let cases = [
            ("a = \"unterminated\n", "Unterminated TOML string literal at offset 4", 4),
            ("a = [ \"x\" }\n", "Mismatched TOML inline collection closer '}' at offset 10", 10),
            ("a = \"x\" junk\n", "Unexpected trailing content after TOML value at offset 8", 8),
            ("a..b = \"x\"\n", "Empty TOML key segment at offset 2", 2),
            ("[t\n", "Unterminated TOML table header at offset 0", 0),
            ("[[t]\n", "Unterminated TOML array-of-tables header at offset 0", 0),
            ("a = [ \"x\"\n", "Unterminated TOML inline collection at offset 4", 4),
            ("a = \"\"\"x\n", "Unterminated TOML multiline string at offset 4", 4),
        ];
        for (src, message, offset) in cases {
            assert_eq!(err(src), (message.to_owned(), json!({ "offset": offset })), "{src:?}");
        }
    }

    #[test]
    fn budget_is_checked_with_its_labels() {
        let labels = std::cell::RefCell::new(Vec::new());
        let budget = |label: &str| {
            labels.borrow_mut().push(label.to_owned());
            Ok(())
        };
        scan_toml(&js::u("a = [\"x\"]\n"), &budget).unwrap();
        assert_eq!(*labels.borrow(), ["toml-scan", "toml-inline-scan"]);
    }
}
