use std::collections::HashMap;

use serde_json::json;

use crate::errors::{Error, Result};
use crate::input::span_path::{Chain, LocatedSpan, Position, chain_append};
use crate::js::{self, JsString};
use crate::protect::blocks::CheckBudget;

enum Kind {
    Object { key: Option<JsString>, expect_key: bool },
    Array { index: usize },
}

struct Frame {
    kind: Kind,
    base: Chain,
}

fn chain_of(frame: Option<&Frame>) -> Chain {
    let frame = frame?;
    let position = match &frame.kind {
        Kind::Object { key, .. } => Position::Key(key.clone().unwrap_or_else(|| js::u("null"))),
        Kind::Array { index } => Position::Index(*index),
    };
    chain_append(&frame.base, [position])
}

pub fn scan_json(src: &[u16], check_budget: CheckBudget<'_>) -> Result<Vec<LocatedSpan>> {
    let mut spans = Vec::new();
    let mut stack: Vec<Frame> = Vec::new();
    let n = src.len();
    let mut i = 0;
    let mut steps = 0usize;

    while i < n {
        if steps & 0x3fff == 0 {
            check_budget("json-scan")?;
        }
        steps += 1;

        let ch = src[i];

        if ch == 0x22 {
            let start = i;
            i += 1;
            let mut closed = false;
            let mut has_escape = false;

            while i < n {
                let c = src[i];
                if c == 0x5C {
                    has_escape = true;
                    i += 2;
                    continue;
                }
                if c == 0x22 {
                    i += 1;
                    closed = true;
                    break;
                }
                i += 1;
            }

            if !closed {
                return Err(Error::input(
                    format!("Unterminated JSON string literal at offset {start}"),
                    json!({ "offset": start }),
                ));
            }

            let end = i;
            match stack.last_mut() {
                None => spans.push(LocatedSpan::new(start, end, false, None)),
                Some(frame) => {
                    if let Kind::Object { key, expect_key: true } = &mut frame.kind {
                        spans.push(LocatedSpan::new(start, end, true, frame.base.clone()));
                        *key = Some(if has_escape {
                            js::json::parse_string(&src[start..end]).map_err(|e| {
                                Error::input(
                                    format!("Malformed JSON object key: {}", e.message),
                                    json!({ "start": start, "end": end }),
                                )
                            })?
                        } else {
                            src[start + 1..end - 1].to_vec()
                        });
                    } else {
                        spans.push(LocatedSpan::new(start, end, false, chain_of(Some(frame))));
                    }
                }
            }
            continue;
        }

        match ch {
            0x7B => {
                let base = chain_of(stack.last());
                stack.push(Frame { kind: Kind::Object { key: None, expect_key: true }, base });
            }
            0x5B => {
                let base = chain_of(stack.last());
                stack.push(Frame { kind: Kind::Array { index: 0 }, base });
            }
            0x7D | 0x5D => {
                let matches = match stack.pop() {
                    Some(Frame { kind: Kind::Object { .. }, .. }) => ch == 0x7D,
                    Some(Frame { kind: Kind::Array { .. }, .. }) => ch == 0x5D,
                    None => false,
                };
                if !matches {
                    return Err(Error::input(
                        format!("Unbalanced JSON structure at offset {i}"),
                        json!({ "offset": i }),
                    ));
                }
            }
            0x3A => {
                if let Some(Frame { kind: Kind::Object { expect_key, .. }, .. }) = stack.last_mut() {
                    *expect_key = false;
                }
            }
            0x2C => match stack.last_mut().map(|f| &mut f.kind) {
                Some(Kind::Object { key, expect_key }) => {
                    *expect_key = true;
                    *key = None;
                }
                Some(Kind::Array { index }) => *index += 1,
                None => {}
            },
            _ => {}
        }
        i += 1;
    }

    if !stack.is_empty() {
        return Err(Error::input(
            "Unbalanced JSON structure: unclosed object or array",
            json!({ "depth": stack.len() }),
        ));
    }

    Ok(spans)
}

#[derive(Debug, Clone)]
enum Test {
    Segments(Vec<JsString>),
    DottedTail(JsString),
    Dotted(Vec<JsString>),
}

#[derive(Debug, Clone)]
struct Selector {
    dotted: bool,
    tail: JsString,
    test: Test,
}

impl Selector {
    fn test(&self, span: &LocatedSpan) -> bool {
        match &self.test {
            Test::Segments(wanted) => span.segments_match(wanted),
            Test::DottedTail(raw) => span.dotted_tail() == *raw,
            Test::Dotted(wanted) => span.dotted_match(wanted),
        }
    }
}

fn compile_selector(raw: &str) -> Selector {
    let raw = js::u(raw);

    if raw.first() == Some(&0x2F) {
        let wanted: Vec<JsString> = js::split(&raw[1..], &[0x2F])
            .into_iter()
            .map(|seg| js::replace_all_lit(&js::replace_all_lit(seg, &js::u("~1"), &[0x2F]), &js::u("~0"), &[0x7E]))
            .collect();
        let tail = wanted.last().cloned().unwrap_or_default();
        return Selector { dotted: false, tail, test: Test::Segments(wanted) };
    }

    if !raw.contains(&0x2E) {
        return Selector { dotted: true, tail: raw.clone(), test: Test::DottedTail(raw) };
    }

    let wanted: Vec<JsString> = js::split(&raw, &[0x2E]).into_iter().map(<[u16]>::to_vec).collect();
    let tail = wanted.last().cloned().unwrap_or_default();
    Selector { dotted: true, tail, test: Test::Dotted(wanted) }
}

#[derive(Debug, Clone)]
enum Bucket {
    List(Vec<Selector>),
    Indexed {
        list: Vec<Selector>,
        by_dotted_tail: HashMap<JsString, Vec<usize>>,
        by_segment_tail: HashMap<JsString, Vec<usize>>,
        star: Vec<usize>,
    },
}

impl Bucket {
    fn list(&self) -> &[Selector] {
        match self {
            Bucket::List(list) | Bucket::Indexed { list, .. } => list,
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct Selectors {
    fields: Option<Bucket>,
    exclude: Option<Bucket>,
}

pub fn compile_span_selectors(fields: Option<&[String]>, exclude: Option<&[String]>) -> Selectors {
    Selectors { fields: fields.map(compile_selector_bucket), exclude: exclude.map(compile_selector_bucket) }
}

fn compile_selector_bucket(raws: &[String]) -> Bucket {
    let list: Vec<Selector> = raws.iter().map(|r| compile_selector(r)).collect();

    if list.len() <= 8 {
        return Bucket::List(list);
    }

    let mut by_dotted_tail: HashMap<JsString, Vec<usize>> = HashMap::new();
    let mut by_segment_tail: HashMap<JsString, Vec<usize>> = HashMap::new();
    let mut star = Vec::new();

    for (k, selector) in list.iter().enumerate() {
        if selector.tail.as_slice() == [0x2A] {
            star.push(k);
            continue;
        }
        let index = if selector.dotted { &mut by_dotted_tail } else { &mut by_segment_tail };
        index.entry(selector.tail.clone()).or_default().push(k);
    }

    Bucket::Indexed { list, by_dotted_tail, by_segment_tail, star }
}

fn list_matches<'a>(span: &LocatedSpan, mut selectors: impl Iterator<Item = &'a Selector>) -> bool {
    selectors.any(|selector| selector.test(span))
}

fn index_matches(span: &LocatedSpan, list: &[Selector], index: &HashMap<JsString, Vec<usize>>, tail: &[u16]) -> bool {
    index.get(tail).is_some_and(|group| list_matches(span, group.iter().map(|&k| &list[k])))
}

fn any_matches(span: &LocatedSpan, selectors: &Bucket) -> bool {
    match selectors {
        Bucket::List(list) => list_matches(span, list.iter()),
        Bucket::Indexed { list, by_dotted_tail, by_segment_tail, star } => {
            if !by_dotted_tail.is_empty() && index_matches(span, list, by_dotted_tail, &span.dotted_tail()) {
                return true;
            }
            if !by_segment_tail.is_empty() && index_matches(span, list, by_segment_tail, &span.tail()) {
                return true;
            }
            !star.is_empty() && list_matches(span, star.iter().map(|&k| &list[k]))
        }
    }
}

fn has_first(selectors: &Option<Bucket>) -> bool {
    selectors.as_ref().is_some_and(|b| !b.list().is_empty())
}

pub fn select_value_spans<'s>(
    spans: &'s [LocatedSpan],
    selectors: &Selectors,
    check_budget: CheckBudget<'_>,
) -> Result<Vec<&'s LocatedSpan>> {
    let Selectors { fields, exclude } = selectors;
    let restricted = has_first(fields) || has_first(exclude);
    let mut selected = Vec::new();

    for (i, span) in spans.iter().enumerate() {
        if i & 0x3ff == 0 {
            check_budget("select")?;
        }

        if !span.is_key
            && (!restricted || !span.unaddressed)
            && fields.as_ref().is_none_or(|f| any_matches(span, f))
            && !exclude.as_ref().is_some_and(|e| any_matches(span, e))
        {
            selected.push(span);
        }
    }

    Ok(selected)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ok(_: &str) -> Result<()> {
        Ok(())
    }

    type Row = (usize, usize, bool, Vec<String>);

    fn scan(src: &str) -> Result<Vec<Row>> {
        let spans = scan_json(&js::u(src), &ok)?;
        Ok(spans
            .iter()
            .map(|s| (s.start, s.end, s.is_key, s.segments().iter().map(|x| js::lossy(x)).collect()))
            .collect())
    }

    fn row(start: usize, end: usize, is_key: bool, path: &[&str]) -> Row {
        (start, end, is_key, path.iter().map(std::string::ToString::to_string).collect())
    }

    fn sel(src: &str, fields: Option<&[&str]>, exclude: Option<&[&str]>) -> Vec<(usize, usize)> {
        let owned = |v: Option<&[&str]>| v.map(|v| v.iter().map(std::string::ToString::to_string).collect::<Vec<_>>());
        let (f, e) = (owned(fields), owned(exclude));
        let selectors = compile_span_selectors(f.as_deref(), e.as_deref());
        let spans = scan_json(&js::u(src), &ok).unwrap();
        select_value_spans(&spans, &selectors, &ok).unwrap().iter().map(|s| (s.start, s.end)).collect()
    }

    #[test]
    fn nested_paths() {
        assert_eq!(
            scan(r#"{"a":"x","b":["y",{"c":"z"}]}"#).unwrap(),
            vec![
                row(1, 4, true, &[]),
                row(5, 8, false, &["a"]),
                row(9, 12, true, &[]),
                row(14, 17, false, &["b", "0"]),
                row(19, 22, true, &["b", "1"]),
                row(23, 26, false, &["b", "1", "c"]),
            ]
        );
    }

    #[test]
    fn unicode_offsets_are_utf16() {
        assert_eq!(
            scan(r#"{"ключ":"значение","😀":"é"}"#).unwrap(),
            vec![
                row(1, 7, true, &[]),
                row(8, 18, false, &["ключ"]),
                row(19, 23, true, &[]),
                row(24, 27, false, &["😀"])
            ]
        );
    }

    #[test]
    fn escaped_keys_are_decoded() {
        assert_eq!(
            scan(r#"{"a\u002eb":"v","\"q":"w"}"#).unwrap(),
            vec![
                row(1, 11, true, &[]),
                row(12, 15, false, &["a.b"]),
                row(16, 21, true, &[]),
                row(22, 25, false, &["\"q"])
            ]
        );
    }

    #[test]
    fn keyless_and_root_values() {
        assert_eq!(scan(r#"{:"x"}"#).unwrap(), vec![row(2, 5, false, &["null"])]);
        assert_eq!(scan(r#""hello""#).unwrap(), vec![row(0, 7, false, &[])]);
        assert_eq!(
            scan(r#"{"a":"x" "y", "b": 1}"#).unwrap(),
            vec![row(1, 4, true, &[]), row(5, 8, false, &["a"]), row(9, 12, false, &["a"]), row(14, 17, true, &[])]
        );
        assert_eq!(
            scan(r#"[[], "a", ["b", "c"], {"k": "v"}]"#).unwrap(),
            vec![
                row(5, 8, false, &["1"]),
                row(11, 14, false, &["2", "0"]),
                row(16, 19, false, &["2", "1"]),
                row(23, 26, true, &["3"]),
                row(28, 31, false, &["3", "k"]),
            ]
        );
    }

    #[test]
    fn structural_errors() {
        let e = scan(r#"{"a":"x"#).unwrap_err();
        assert_eq!(
            (e.message.as_str(), e.details),
            ("Unterminated JSON string literal at offset 5", json!({ "offset": 5 }))
        );
        let e = scan(r#"{"a":["x"}"#).unwrap_err();
        assert_eq!((e.message.as_str(), e.details), ("Unbalanced JSON structure at offset 9", json!({ "offset": 9 })));
        let e = scan(r#"{"a":["#).unwrap_err();
        assert_eq!(
            (e.message.as_str(), e.details),
            ("Unbalanced JSON structure: unclosed object or array", json!({ "depth": 2 }))
        );
    }

    #[test]
    fn malformed_key_errors() {
        let e = scan(r#"{"\x":"v"}"#).unwrap_err();
        assert_eq!(
            e.message,
            "Malformed JSON object key: Bad escaped character in JSON at position 2 (line 1 column 3)"
        );
        assert_eq!(e.details, json!({ "start": 1, "end": 5 }));
        let e = scan(r#"{"\u12":1}"#).unwrap_err();
        assert_eq!(e.message, "Malformed JSON object key: Bad Unicode escape in JSON at position 5 (line 1 column 6)");
        let e = scan("{\"\\n\n\\t\":1}").unwrap_err();
        assert_eq!(
            e.message,
            "Malformed JSON object key: Bad control character in string literal in JSON at position 3 (line 1 column 4)"
        );
    }

    const DOC: &str = r#"{"a":"x","b":["y",{"c":"z"}],"a.b":{"c":"d"},"t":"u"}"#;
    const MANY: [&str; 8] = ["q1", "q2", "q3", "q4", "q5", "q6", "q7", "q8"];

    #[test]
    fn plain_selectors() {
        assert_eq!(sel(DOC, None, None), vec![(5, 8), (14, 17), (23, 26), (40, 43), (49, 52)]);
        assert_eq!(sel(DOC, Some(&["a"]), None), vec![(5, 8)]);
        assert_eq!(sel(DOC, Some(&["b.c"]), None), vec![]);
        assert_eq!(sel(DOC, Some(&["/b/1/c"]), None), vec![(23, 26)]);
        assert_eq!(sel(DOC, Some(&["a.b.c"]), None), vec![(40, 43)]);
        assert_eq!(sel(DOC, Some(&["/a.b/c"]), None), vec![(40, 43)]);
        assert_eq!(sel(DOC, Some(&["c"]), None), vec![(23, 26), (40, 43)]);
        assert_eq!(sel(DOC, None, Some(&["c"])), vec![(5, 8), (14, 17), (49, 52)]);
        assert_eq!(sel(DOC, Some(&["*"]), Some(&["a"])), vec![]);
        assert_eq!(sel(DOC, Some(&[]), None), vec![]);
        assert_eq!(sel(DOC, Some(&[""]), None), vec![]);
    }

    #[test]
    fn root_and_escaped_pointer_selectors() {
        assert_eq!(sel(r#""r""#, Some(&[""]), None), vec![(0, 3)]);
        assert_eq!(sel(r#""r""#, Some(&["*"]), None), vec![]);
        assert_eq!(sel(r#""r""#, Some(&["/"]), None), vec![]);
        assert_eq!(sel(r#"{"~x/y":"v"}"#, Some(&["/~0x~1y"]), None), vec![(8, 11)]);
    }

    #[test]
    fn indexed_selectors() {
        let with = |extra: &'static str| MANY.iter().copied().chain([extra]).collect::<Vec<_>>();
        assert_eq!(sel(DOC, Some(&with("c")), None), vec![(23, 26), (40, 43)]);
        assert_eq!(sel(DOC, Some(&with("/a.b/c")), None), vec![(40, 43)]);
        assert_eq!(sel(DOC, Some(&with("b.*")), None), vec![(14, 17)]);
        let exclude: Vec<&str> = ["/b/0"].into_iter().chain(MANY).collect();
        assert_eq!(sel(DOC, Some(&with("*")), Some(&exclude)), vec![]);
    }
}
