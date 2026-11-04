use std::sync::LazyLock;

use crate::clear_special::clear_special_chars;
use crate::js::{self, Re};
use crate::protect::placeholders::{CLOSE, OPEN, OPEN_UNIT, TRANSPARENT_BARE_KINDS};
use crate::shared::tag_name::NAME_START;

static REFERENCES_ONLY: LazyLock<Re> = LazyLock::new(|| Re::new(r"^(?:\s|&[#0-9A-Za-z]+;)+$", "u"));
static NUMERIC_REFERENCE: LazyLock<Re> = LazyLock::new(|| Re::new(r"&#(?:([0-9]{1,7})|[xX]([0-9a-fA-F]{1,6}));", "g"));
static OPAQUE_PART_RE: LazyLock<Re> = LazyLock::new(|| {
    Re::new(&format!(r"{OPEN}[{TRANSPARENT_BARE_KINDS}]\d+{CLOSE}|<\/?[{NAME_START}!?][^<>]*>"), "gu")
});

fn decodes_to_blank(body: &[u16]) -> bool {
    if js::trim(body).is_empty() {
        return true;
    }
    if !REFERENCES_ONLY.test(body) {
        return false;
    }
    let decoded = NUMERIC_REFERENCE.replace_with(&clear_special_chars(body.to_vec()), |c| {
        let code = match c.get(1) {
            Some(decimal) => js::lossy(decimal).parse::<u32>().ok(),
            None => u32::from_str_radix(&js::lossy(c.s(2)), 16).ok(),
        };
        match code {
            Some(code) if code <= 0x10ffff => js::from_code_point(code),
            _ => c.whole().to_vec(),
        }
    });
    js::trim(&decoded).is_empty()
}

pub fn shows_nothing(body: &[u16]) -> bool {
    if decodes_to_blank(body) {
        return true;
    }
    (body.contains(&0x3C) || body.contains(&OPEN_UNIT))
        && decodes_to_blank(&OPAQUE_PART_RE.replace_with(body, |_| Vec::new()))
}
