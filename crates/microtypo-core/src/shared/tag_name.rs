use std::sync::LazyLock;

use crate::js::{self, Re};

pub const NAME_START: &str = r"\p{L}_:";

static NAME_START_RE: LazyLock<Re> = LazyLock::new(|| Re::new(&format!("[{NAME_START}!?]"), "u"));

pub fn is_tag_name_start(code_point: Option<u32>) -> bool {
    match code_point {
        Some(cp) if cp < 0x80 => {
            let c = cp as u8;
            c.is_ascii_alphabetic() || matches!(c, b'_' | b':' | b'!' | b'?')
        }
        Some(cp) => NAME_START_RE.test(&js::from_code_point(cp)),
        None => false,
    }
}

pub fn starts_tag_name(text: &[u16]) -> bool {
    is_tag_name_start(js::code_point_at(text, 0))
}

pub const TAG_NAME_SOURCE: &str = "^[a-zA-Z][a-zA-Z0-9-]*$";

pub fn is_tag_name(name: &str) -> bool {
    let mut bytes = name.bytes();
    bytes.next().is_some_and(|c| c.is_ascii_alphabetic()) && bytes.all(|c| c.is_ascii_alphanumeric() || c == b'-')
}
