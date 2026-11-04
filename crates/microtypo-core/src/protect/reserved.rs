use std::collections::HashMap;

use serde_json::json;

use crate::errors::{Error, Result};
use crate::js::JsString;
use crate::protect::placeholders::RESERVED_CHARS as RESERVED;

const PUA_FIRST: u16 = 0xE000;
const PUA_LAST: u16 = 0xF8FF;

pub fn has_reserved(text: &[u16]) -> bool {
    text.iter().any(|c| RESERVED.contains(c))
}

#[derive(Debug)]
pub struct Escaped {
    pub text: JsString,
    back: HashMap<u16, u16>,
}

impl Escaped {
    pub fn unescape(&self, typeset: &[u16]) -> JsString {
        typeset.iter().map(|c| self.back.get(c).copied().unwrap_or(*c)).collect()
    }
}

pub fn escape_reserved(text: &[u16]) -> Result<Option<Escaped>> {
    if !has_reserved(text) {
        return Ok(None);
    }
    let mut used = vec![false; usize::from(PUA_LAST - PUA_FIRST) + 1];
    for &c in text {
        if (PUA_FIRST..=PUA_LAST).contains(&c) {
            used[usize::from(c - PUA_FIRST)] = true;
        }
    }
    let mut forward = HashMap::new();
    let mut back = HashMap::new();
    let mut next = u32::from(PUA_FIRST);
    for reserved in RESERVED {
        if !used[usize::from(reserved - PUA_FIRST)] {
            continue;
        }
        while next <= u32::from(PUA_LAST)
            && (used[(next - u32::from(PUA_FIRST)) as usize] || RESERVED.contains(&(next as u16)))
        {
            next += 1;
        }
        if next > u32::from(PUA_LAST) {
            return Err(Error::input(
                "Input uses too many distinct private-use characters to escape the reserved ones",
                json!({ "reason": "reserved-escape" }),
            ));
        }
        let substitute = next as u16;
        next += 1;
        forward.insert(reserved, substitute);
        back.insert(substitute, reserved);
    }
    let text = text.iter().map(|c| forward.get(c).copied().unwrap_or(*c)).collect();
    Ok(Some(Escaped { text, back }))
}
