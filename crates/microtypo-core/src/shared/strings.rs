use std::sync::LazyLock;

use crate::errors::Result;
use crate::js::{self, JsString, Re};

pub fn cycle<F>(
    text: &[u16],
    mut f: F,
    max_iter: usize,
    on_limit: &mut dyn FnMut(usize),
    check_budget: &dyn Fn(&str) -> Result<()>,
) -> Result<JsString>
where
    F: FnMut(&[u16]) -> Result<JsString>,
{
    let mut current = text.to_vec();
    for _ in 0..max_iter {
        check_budget("cycle")?;
        let next = f(&current)?;
        if next == current {
            return Ok(current);
        }
        current = next;
    }
    on_limit(max_iter);
    Ok(current)
}

pub fn trim_blank_lines(text: &[u16], is_blank: &dyn Fn(&[u16]) -> bool) -> JsString {
    let lines = js::split(text, &[0x0A]);
    let mut start = 0usize;
    let mut end = lines.len() as isize - 1;
    while start < lines.len() && is_blank(lines[start]) {
        start += 1;
    }
    while end >= start as isize && is_blank(lines[end as usize]) {
        end -= 1;
    }
    if end < start as isize {
        return Vec::new();
    }
    js::join(&lines[start..=end as usize], &[0x0A])
}

fn is_whitespace_code(code: Option<u32>) -> bool {
    matches!(
        code,
        Some(
            0x09..=0x0d | 0x20 | 0xa0 | 0x1680 | 0x2000..=0x200a | 0x2028 | 0x2029 | 0x202f | 0x205f | 0x3000 | 0xfeff,
        )
    )
}

#[derive(Debug)]
pub struct Edges {
    pub start: usize,
    pub end: usize,
}

pub fn edge_whitespace(text: &[u16]) -> Edges {
    let mut start = 0;
    while start < text.len() && is_whitespace_code(js::code_point_at(text, start)) {
        start += 1;
    }
    let mut end = text.len();
    while end > start && is_whitespace_code(js::code_point_at(text, end - 1)) {
        end -= 1;
    }
    Edges { start, end }
}

pub fn leading_indent(text: &[u16]) -> (usize, usize) {
    let end = edge_whitespace(text).start;
    let mut start = end;
    while start > 0 && (text[start - 1] == 0x20 || text[start - 1] == 0x09) {
        start -= 1;
    }
    let opens_line = start == 0 || text[start - 1] == 0x0A || text[start - 1] == 0x0D;
    if opens_line && end < text.len() { (start, end) } else { (end, end) }
}

static LONE_CR_RE: LazyLock<Re> = LazyLock::new(|| Re::new(r"\r(?!\n)", "g"));

pub fn line_view(text: &[u16]) -> JsString {
    if text.contains(&0x0D) { LONE_CR_RE.replace_tpl(text, &[0x0A]) } else { text.to_vec() }
}

pub fn is_flow_space(ch: Option<u16>) -> bool {
    matches!(ch, Some(0x20 | 0x09 | 0x0A | 0x0D))
}

pub fn skip_spaces(text: &[u16], i: usize, limit: usize) -> usize {
    let mut j = i;
    while j < limit && (text.get(j) == Some(&0x20) || text.get(j) == Some(&0x09)) {
        j += 1;
    }
    j
}

pub fn count(haystack: &[u16], needle: &[u16]) -> usize {
    if needle.is_empty() {
        return 0;
    }
    let mut n = 0;
    let mut pos = 0;
    while let Some(at) = js::index_of(haystack, needle, pos) {
        n += 1;
        pos = at + needle.len();
    }
    n
}

pub fn count_unit(haystack: &[u16], unit: u16) -> usize {
    haystack.iter().filter(|&&c| c == unit).count()
}
