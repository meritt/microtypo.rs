use std::sync::LazyLock;

use crate::errors::Result;
use crate::js::{self, JsString, Re};

pub const WHITESPACE: &str = " \t\n\r\u{B}\u{C}\u{A0}\u{1680}\u{2000}\u{2001}\u{2002}\u{2003}\u{2004}\u{2005}\u{2006}\u{2007}\u{2008}\u{2009}\u{200A}\u{2028}\u{2029}\u{202F}\u{205F}\u{3000}\u{FEFF}";

pub fn is_backslash_escaped(text: &[u16], at: usize) -> bool {
    let mut start = at;
    while start > 0 && text[start - 1] == 0x5C {
        start -= 1;
    }
    (at - start) % 2 == 1
}

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
    check_budget("cycle")?;
    for _ in 0..max_iter {
        let next = f(&current)?;
        check_budget("cycle")?;
        if next == current {
            return Ok(current);
        }
        current = next;
    }
    on_limit(max_iter);
    Ok(current)
}

pub fn trim_blank_lines(text: &[u16], is_blank: &dyn Fn(&[u16]) -> bool) -> JsString {
    let mut lines = Vec::new();
    let mut start = 0;
    let mut at = 0;
    while at < text.len() {
        if matches!(text[at], 0x0D | 0x0A) {
            lines.push((start, at));
            at += if text[at] == 0x0D && text.get(at + 1) == Some(&0x0A) { 2 } else { 1 };
            start = at;
        } else {
            at += 1;
        }
    }
    lines.push((start, text.len()));
    let mut from = 0;
    let mut to = lines.len();
    while from < to && is_blank(&text[lines[from].0..lines[from].1]) {
        from += 1;
    }
    while to > from && is_blank(&text[lines[to - 1].0..lines[to - 1].1]) {
        to -= 1;
    }
    if from == to { Vec::new() } else { text[lines[from].0..lines[to - 1].1].to_vec() }
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

pub fn leading_indent(
    text: &[u16],
    read_scalar: fn(&[u16], usize) -> (JsString, usize),
    check_budget: &dyn Fn(&str) -> Result<()>,
) -> Result<(usize, usize)> {
    let mut end = 0;
    let mut start = 0;
    let mut opens_line = true;
    let mut previous = None;
    let mut steps = 0usize;
    while end < text.len() {
        if steps & 1023 == 0 {
            check_budget("leadingIndent")?;
        }
        steps += 1;
        let (glyph, next) = read_scalar(text, end);
        if glyph.len() != 1 || !is_whitespace_code(Some(u32::from(glyph[0]))) {
            break;
        }
        let ascii = matches!(glyph[0], 0x20 | 0x09);
        if !ascii || !matches!(previous, Some(0x20 | 0x09)) {
            start = if ascii { end } else { next };
            opens_line = ascii && (end == 0 || matches!(previous, Some(0x0A | 0x0D)));
        }
        previous = Some(glyph[0]);
        end = next;
    }
    if end == text.len() {
        return Ok((0, end));
    }
    Ok(if opens_line { (start, end) } else { (end, end) })
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
