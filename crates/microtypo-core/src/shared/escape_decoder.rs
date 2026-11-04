use crate::js::{self, JsString};

pub fn decode_escapes(raw: &[u16], simple: &[(u16, &str)], hex_widths: &[(u16, usize)]) -> JsString {
    if !raw.contains(&0x5C) {
        return raw.to_vec();
    }
    let mut out = Vec::new();
    let mut i = 0;
    while i < raw.len() {
        if raw[i] != 0x5C {
            out.push(raw[i]);
            i += 1;
            continue;
        }
        let marker = raw.get(i + 1).copied();
        if let Some(&(_, width)) = hex_widths.iter().find(|(m, _)| Some(*m) == marker) {
            let digits = js::slice(raw, i + 2, i + 2 + width);
            let hex = !digits.is_empty() && digits.iter().all(|&c| (c as u8 as char).is_ascii_hexdigit() && c < 0x80);
            let code = if hex { u32::from_str_radix(&String::from_utf16_lossy(digits), 16).ok() } else { None };
            match code {
                Some(code) if digits.len() == width && code <= 0x10ffff => {
                    out.extend(js::from_code_point(code));
                    i += 2 + width;
                    continue;
                }
                _ => return raw.to_vec(),
            }
        }
        match simple.iter().find(|(m, _)| Some(*m) == marker) {
            Some((_, v)) => {
                out.extend(js::u(v));
                i += 2;
            }
            None => return raw.to_vec(),
        }
    }
    out
}
