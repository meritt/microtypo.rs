use std::sync::LazyLock;

use crate::clear_special::read_scalar_with;
use crate::errors::Result;
use crate::js::{self, JsString, Re};
use crate::protect::blocks::CheckBudget;
use crate::protect::placeholders::REFERENCE_HOLE;
use crate::shared::glyphs::{RAQUO, unit};

pub const CHARACTER_REFERENCE_HEAD: &str = "&(?:#[0-9]+|#[xX][0-9a-fA-F]+|[A-Za-z][A-Za-z0-9]*)";

static REFERENCE_AT_RE: LazyLock<Re> =
    LazyLock::new(|| Re::new(&format!("({CHARACTER_REFERENCE_HEAD})({})?;", *REFERENCE_HOLE), "uy"));
static HOLE_END_RE: LazyLock<Re> = LazyLock::new(|| Re::new(&format!("{}$", *REFERENCE_HOLE), "u"));
const HOLE_MAX: usize = 12;
static HEAD_CHAR_RE: LazyLock<Re> = LazyLock::new(|| Re::new("[0-9A-Za-z#]", "u"));

#[derive(Debug, Clone)]
pub struct CharacterReference {
    pub head: JsString,
    pub end: usize,
    pub native: bool,
}

pub fn read_character_reference_at(text: &[u16], at: usize) -> Option<CharacterReference> {
    let m = REFERENCE_AT_RE.exec_at(text, at)?;
    Some(CharacterReference { head: m.s(1).to_vec(), end: m.end(), native: m.has(2) })
}

pub fn kept_reference_glyph(reference: Option<&CharacterReference>) -> Option<JsString> {
    reference.filter(|r| r.head.starts_with(&[0x26, 0x23])).and_then(|r| authored_reference_glyph(Some(r), false))
}

pub fn authored_reference_glyph(
    reference: Option<&CharacterReference>,
    preserve_quote_direction: bool,
) -> Option<JsString> {
    let reference = reference.filter(|r| r.native)?;
    let mut spelled = reference.head.clone();
    spelled.push(0x3B);
    let (glyph, end) = read_scalar_with(&spelled, 0, preserve_quote_direction);
    (end == spelled.len() && glyph != spelled).then_some(glyph)
}

pub fn quote_boundary_glyph(reference: Option<&CharacterReference>) -> Option<JsString> {
    let glyph = authored_reference_glyph(reference, false);
    let numeric = reference.is_some_and(|r| r.head.starts_with(&[0x26, 0x23]));
    if numeric || glyph.as_deref() == Some(&[unit(RAQUO)][..]) { glyph } else { None }
}

pub fn kept_reference_to(glyphs: &[char]) -> String {
    let codes: Vec<u32> = glyphs.iter().map(|&g| u32::from(g)).collect();
    let decimal = codes.iter().map(u32::to_string).collect::<Vec<_>>().join("|");
    let hex = codes
        .iter()
        .map(|code| {
            format!("{code:x}")
                .chars()
                .map(
                    |c| if c.is_ascii_lowercase() { format!("[{c}{}]", c.to_ascii_uppercase()) } else { c.to_string() },
                )
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("|");
    format!("&#(?:0*(?:{decimal})|[xX]0*(?:{hex})){};", *REFERENCE_HOLE)
}

pub fn kept_glyph_at(text: &[u16], at: usize) -> (JsString, usize) {
    let reference = if text.get(at) == Some(&0x26) { read_character_reference_at(text, at) } else { None };
    match (kept_reference_glyph(reference.as_ref()), reference) {
        (Some(glyph), Some(reference)) => (glyph, reference.end),
        _ => (text.get(at).map_or_default(|&c| vec![c]), at + 1),
    }
}

pub fn kept_glyphs_at(text: &[u16], at: usize, count: usize) -> JsString {
    let mut out = Vec::new();
    let mut i = at;
    while out.len() < count && i < text.len() {
        let (glyph, end) = kept_glyph_at(text, i);
        out.extend_from_slice(&glyph);
        i = end;
    }
    out
}

pub fn ends_character_reference(
    text: &[u16],
    semicolon_at: usize,
    check_budget: Option<CheckBudget<'_>>,
) -> Result<bool> {
    Ok(character_reference_ending_at(text, semicolon_at, check_budget)?.is_some())
}

pub fn character_reference_ending_at(
    text: &[u16],
    semicolon_at: usize,
    check_budget: Option<CheckBudget<'_>>,
) -> Result<Option<CharacterReference>> {
    let mut end = semicolon_at;
    if let Some(hole) = HOLE_END_RE.exec(js::slice(text, end.saturating_sub(HOLE_MAX), end)) {
        end -= hole.whole().len();
    }
    let mut from = end;
    while from > 0 && HEAD_CHAR_RE.test(&text[from - 1..from]) {
        from -= 1;
        if let Some(check) = check_budget
            && (end - from) & 1023 == 0
        {
            check("punctuation:reference")?;
        }
    }
    let reference = if from > 0 && text[from - 1] == 0x26 { read_character_reference_at(text, from - 1) } else { None };
    Ok(reference.filter(|r| r.end == semicolon_at + 1))
}
