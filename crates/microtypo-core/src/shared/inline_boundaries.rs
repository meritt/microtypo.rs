use std::sync::LazyLock;

use crate::js::{self, Re};
use crate::protect::placeholders::{CLOSE_UNIT, OPEN_UNIT, TAG_PLACEHOLDER_SOURCE, TRANSPARENT_TAG_KINDS};
use crate::protect::tags::SafeTags;
use crate::shared::block_tags::VOID_TAG_NAMES;

const LT: u16 = 0x3C;
const GT: u16 = 0x3E;
const SLASH: u16 = 0x2F;

static NATIVE_TAG_RE: LazyLock<Re> = LazyLock::new(|| Re::new(&TAG_PLACEHOLDER_SOURCE, "uy"));

fn is_ascii_alnum(c: u16) -> bool {
    u8::try_from(c).is_ok_and(|b| b.is_ascii_alphanumeric())
}

fn transparent_kind(c: Option<&u16>) -> bool {
    c.is_some_and(|&k| TRANSPARENT_TAG_KINDS.encode_utf16().any(|t| t == k))
}

fn token_start(text: &[u16], start: usize) -> usize {
    start + if text.get(start + 1) == Some(&SLASH) { 2 } else { 1 }
}

pub fn native_tag_start(text: &[u16], end: usize) -> Option<usize> {
    if end < 2 || text.get(end - 1) != Some(&GT) || text.get(end - 2) != Some(&CLOSE_UNIT) {
        return None;
    }
    let mut at = end - 2;
    while at > 0 && is_ascii_alnum(text[at - 1]) {
        at -= 1;
    }
    let at = at.checked_sub(1).filter(|&at| text[at] == OPEN_UNIT)?;
    let start = if at >= 1 && text[at - 1] == LT {
        at - 1
    } else if at >= 2 && text[at - 1] == SLASH && text[at - 2] == LT {
        at - 2
    } else {
        return None;
    };
    (native_tag_end(text, start) == Some(end)).then_some(start)
}

pub fn inline_tag_start(text: &[u16], end: usize) -> Option<usize> {
    let start = native_tag_start(text, end)?;
    transparent_kind(text.get(token_start(text, start) + 1)).then_some(start)
}

pub fn native_tag_end(text: &[u16], start: usize) -> Option<usize> {
    if text.get(start) != Some(&LT) || text.get(token_start(text, start)) != Some(&OPEN_UNIT) {
        return None;
    }
    NATIVE_TAG_RE.exec_at(text, start).map(|m| m.end())
}

pub fn inline_end_tag_start(text: &[u16], end: usize) -> Option<usize> {
    inline_tag_start(text, end).filter(|&start| text.get(start + 1) == Some(&SLASH))
}

pub fn inline_end_tag_ends_at(text: &[u16], gt: usize) -> bool {
    inline_end_tag_start(text, gt + 1).is_some()
}

fn is_void(name: Option<&[u16]>) -> bool {
    name.is_some_and(|name| VOID_TAG_NAMES.iter().any(|v| js::eq(name, v)))
}

fn native_name(names: &mut SafeTags, source: &[u16], start: usize, end: usize) -> Option<crate::js::JsString> {
    let prefix = *source.get(start + 1)?;
    let digits = js::slice(source, start + 2, end.saturating_sub(1));
    let id = if digits.is_empty() { 0 } else { js::lossy(digits).parse::<usize>().ok()? };
    names.name_of(prefix, id)
}

fn text_inline(names: &mut SafeTags, source: &[u16], start: usize, end: usize) -> bool {
    if !transparent_kind(source.get(start + 1)) {
        return false;
    }
    let name = native_name(names, source, start, end);
    name.is_some() && !is_void(name.as_deref())
}

fn token_id(token: &[u16]) -> Option<(u16, usize)> {
    let prefix = *token.get(1)?;
    let digits = js::slice(token, 2, token.len().saturating_sub(1));
    let id = if digits.is_empty() { 0 } else { js::lossy(digits).parse::<usize>().ok()? };
    Some((prefix, id))
}

pub fn token_tag_name(names: Option<&mut SafeTags>, token: &[u16]) -> Option<crate::js::JsString> {
    let (prefix, id) = token_id(token)?;
    names?.name_of(prefix, id)
}

pub fn token_tag_raw<'t>(tags: &'t SafeTags, token: &[u16]) -> Option<&'t [u16]> {
    let (prefix, id) = token_id(token)?;
    tags.raw_of(prefix, id)
}

pub fn native_void_tag(names: Option<&mut SafeTags>, source: &[u16], start: usize, end: usize) -> bool {
    let Some(names) = names else { return false };
    let name = native_name(names, source, token_start(source, start), end.saturating_sub(1));
    is_void(name.as_deref())
}

pub fn inline_text_end(names: Option<&mut SafeTags>, source: &[u16], offset: usize) -> usize {
    inline_text_end_visit(names, source, offset, &mut || Ok(())).unwrap_or(offset)
}

pub fn inline_text_end_visit(
    names: Option<&mut SafeTags>,
    source: &[u16],
    offset: usize,
    visit: &mut dyn FnMut() -> crate::errors::Result<()>,
) -> crate::errors::Result<usize> {
    let Some(names) = names else { return Ok(offset) };
    let mut before = offset;
    while let Some(start) = inline_tag_start(source, before) {
        visit()?;
        if !text_inline(names, source, token_start(source, start), before - 1) {
            break;
        }
        before = start;
    }
    Ok(before)
}

pub fn inline_text_start(names: Option<&mut SafeTags>, source: &[u16], offset: usize) -> usize {
    let Some(names) = names else { return offset };
    let mut after = offset;
    while let Some(end) = native_tag_end(source, after) {
        if !text_inline(names, source, token_start(source, after), end - 1) {
            break;
        }
        after = end;
    }
    after
}
