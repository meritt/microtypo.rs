use std::collections::HashSet;

use crate::errors::Result;
use crate::js::{self, JsString};
use crate::protect::blocks::CheckBudget;
use crate::protect::tag_spans::tag_spans;

fn space(c: u16) -> bool {
    matches!(c, 32 | 9 | 10 | 13 | 12)
}
fn lower(text: &[u16]) -> JsString {
    text.iter().map(|&c| if (65..=90).contains(&c) { c + 32 } else { c }).collect()
}
fn name_start(c: u16) -> bool {
    (65..=90).contains(&c) || (97..=122).contains(&c)
}
fn name_part(c: u16) -> bool {
    name_start(c) || (48..=57).contains(&c) || matches!(c, 58 | 45)
}

struct Field {
    name: JsString,
    value: Option<(usize, usize)>,
    quote: Option<u16>,
}

fn attributes(text: &[u16], start: usize, end: usize, check: CheckBudget<'_>) -> Result<Vec<Field>> {
    let mut at = start;
    let mut result = Vec::new();
    while at < end {
        if at & 0x3fff == 0 {
            check("attributes")?;
        }
        while text.get(at).is_some_and(|&c| space(c)) {
            at += 1;
        }
        if at >= end || text[at] == 62 || (text[at] == 47 && text.get(at + 1) == Some(&62)) {
            break;
        }
        let from = at;
        while at < end && !space(text[at]) && !matches!(text[at], 61 | 62 | 47) {
            if at & 0x3fff == 0 {
                check("attributes")?;
            }
            at += 1;
        }
        if from == at {
            at += 1;
            continue;
        }
        let name = lower(&text[from..at]);
        while text.get(at).is_some_and(|&c| space(c)) {
            at += 1;
        }
        if text.get(at) != Some(&61) {
            result.push(Field { name, value: None, quote: None });
            continue;
        }
        at += 1;
        while text.get(at).is_some_and(|&c| space(c)) {
            at += 1;
        }
        let quote = text.get(at).copied().filter(|c| matches!(c, 34 | 39));
        at += usize::from(quote.is_some());
        let start = at;
        while at < end {
            if quote.map_or_else(|| space(text[at]) || text[at] == 62, |q| text[at] == q) {
                break;
            }
            if at & 0x3fff == 0 {
                check("attributes")?;
            }
            at += 1;
        }
        if quote.is_some_and(|q| text.get(at) != Some(&q)) {
            break;
        }
        result.push(Field { name, value: Some((start, at)), quote });
        at += usize::from(quote.is_some());
    }
    Ok(result)
}

fn serialize(value: &[u16], quote: Option<u16>) -> JsString {
    let mut result = Vec::new();
    for &c in value {
        let escape = quote.map_or_else(|| matches!(c, 9 | 10 | 13 | 12 | 32 | 34 | 39 | 96 | 61 | 60 | 62), |q| c == q);
        if escape {
            result.extend(format!("&#{c};").encode_utf16());
        } else {
            result.push(c);
        }
    }
    result
}

pub fn typeset_attributes(
    text: &[u16],
    selected: &[String],
    transform: &mut impl FnMut(&[u16], usize, usize) -> Result<JsString>,
    is_protected: &dyn Fn(&str) -> bool,
    check: CheckBudget<'_>,
    mut edits: Option<&mut Vec<crate::shared::provenance::MapEdit>>,
) -> Result<JsString> {
    if selected.is_empty() || !text.contains(&60) {
        return Ok(text.to_vec());
    }
    let mut result = Vec::new();
    let mut cursor = 0;
    for span in tag_spans(text, None) {
        check("attributes")?;
        if span.end == text.len() || text.get(span.start + 1) == Some(&47) {
            continue;
        }
        let from = span.start + 1;
        if !text.get(from).is_some_and(|&c| name_start(c)) {
            continue;
        }
        let mut end = from + 1;
        while end < span.end && name_part(text[end]) {
            end += 1;
        }
        let name = js::lossy(&lower(&text[from..end]));
        if is_protected(&name) {
            continue;
        }
        let fields = attributes(text, end, span.end, check)?;
        let mut seen = HashSet::new();
        let text_meta = name == "meta"
            && ["name", "property"].iter().any(|key| {
                fields.iter().find(|f| js::eq(&f.name, key)).and_then(|f| f.value).is_some_and(|(a, b)| {
                    let value = lower(&text[a..b]);
                    ["description", "og:title", "og:description", "twitter:title", "twitter:description"]
                        .iter()
                        .any(|k| js::eq(&value, k))
                })
            });
        for field in fields {
            if !seen.insert(field.name.clone()) {
                continue;
            }
            let Some((start, end)) = field.value else {
                continue;
            };
            if !selected.iter().any(|s| js::eq(&field.name, s)) || (js::eq(&field.name, "content") && !text_meta) {
                continue;
            }
            let value = serialize(&transform(&text[start..end], start, end)?, field.quote);
            if value == text[start..end] {
                continue;
            }
            result.extend_from_slice(&text[cursor..start]);
            if let Some(edits) = edits.as_mut() {
                edits.push(crate::shared::provenance::MapEdit::from(crate::Edit { start, end, text: value.clone() }));
            }
            result.extend(value);
            cursor = end;
        }
    }
    result.extend_from_slice(&text[cursor..]);
    Ok(result)
}
