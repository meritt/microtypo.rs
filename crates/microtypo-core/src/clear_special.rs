use std::collections::HashMap;
use std::sync::LazyLock;

use crate::errors::Result;
use crate::js::regex::{recycled, spare};
use crate::js::{JsString, code_point_at, from_code_point, lossy, pair_start, u};
use crate::rules::quote_state::{QuoteTags, canonical_quote_roles};
use crate::shared::chars_table::CHARS_TABLE;
use crate::shared::quote_pairs::RDQUO;

struct Table {
    lookup: HashMap<JsString, JsString>,
    roles: HashMap<JsString, JsString>,
    /// The needles by their first unit, longest first and otherwise in table order: the order a
    /// match prefers them in, after a numeric character reference.
    needles: HashMap<u16, Vec<JsString>>,
    /// A bit for each unit a needle starts with, so that a scan skips the others at once.
    starts: Box<[u64; 1024]>,
}

static TABLE: LazyLock<Table> = LazyLock::new(|| {
    let mut entries: Vec<(JsString, JsString)> = Vec::new();
    for entry in CHARS_TABLE {
        let canonical = u(entry.canonical);
        for &v in entry.utf8 {
            entries.push((from_code_point(v), canonical.clone()));
        }
        for v in entry.html {
            entries.push((u(v), canonical.clone()));
        }
    }
    entries.retain(|(needle, canonical)| !(needle == canonical && needle.len() == 1));
    entries.sort_by_key(|e| std::cmp::Reverse(e.0.len()));
    let mut lookup = HashMap::new();
    let mut needles: HashMap<u16, Vec<JsString>> = HashMap::new();
    let mut starts = Box::new([0; 1024]);
    starts[0] |= 1 << 0x26;
    for (needle, canonical) in &entries {
        lookup.insert(needle.clone(), canonical.clone());
        needles.entry(needle[0]).or_default().push(needle.clone());
        starts[usize::from(needle[0] >> 6)] |= 1 << (needle[0] & 63);
    }
    let mut roles = lookup.clone();
    for needle in [RDQUO.to_string(), "&rdquo;".to_owned(), format!("&#{};", u32::from(RDQUO))] {
        roles.insert(u(&needle), u(&RDQUO.to_string()));
    }
    Table { lookup, roles, needles, starts }
});

/// \return the end of the run of units at or after `from` of `text` that `digit` accepts.
fn digits_end(text: &[u16], from: usize, digit: fn(&u8) -> bool) -> usize {
    from + text[from.min(text.len())..].iter().take_while(|&&c| u8::try_from(c).is_ok_and(|c| digit(&c))).count()
}

fn numeric_at(text: &[u16], at: usize) -> Option<(Option<u64>, usize)> {
    if text.get(at..at + 2)? != [0x26, 0x23] {
        return None;
    }
    let semicolon = |end: usize| text.get(end) == Some(&0x3B);
    let start = at + 2;
    if matches!(text.get(start), Some(0x78 | 0x58)) {
        let end = digits_end(text, start + 1, u8::is_ascii_hexdigit);
        if end > start + 1 && semicolon(end) {
            return Some((u64::from_str_radix(&lossy(&text[start + 1..end]), 16).ok(), end + 1));
        }
    }
    let end = digits_end(text, start, u8::is_ascii_digit);
    (end > start && semicolon(end)).then(|| (lossy(&text[start..end]).parse::<u64>().ok(), end + 1))
}

impl Table {
    fn read(&self, lookup: &HashMap<JsString, JsString>, text: &[u16], at: usize) -> Option<(JsString, usize)> {
        let first = *text.get(at)?;
        if self.starts[usize::from(first >> 6)] & 1 << (first & 63) == 0 {
            return None;
        }
        let (decoded, end) = if let Some((code, end)) = numeric_at(text, at) {
            (code.and_then(|code| lookup.get(&u(&format!("&#{code};")))), end)
        } else {
            let needle = self.needles.get(&first)?.iter().find(|needle| text[at..].starts_with(needle))?;
            let end = at + needle.len();
            (lookup.get(&text[at..end]), end)
        };
        Some((decoded.cloned().unwrap_or_else(|| text[at..end].to_vec()), end))
    }
}

pub fn read_scalar_at(text: &[u16], offset: usize) -> (JsString, usize) {
    read_scalar_with(text, offset, false)
}

pub fn read_scalar_with(text: &[u16], offset: usize, preserve_quote_direction: bool) -> (JsString, usize) {
    let t = &*TABLE;
    let lookup = if preserve_quote_direction { &t.roles } else { &t.lookup };
    if let Some(found) = t.read(lookup, text, pair_start(text, offset)) {
        return found;
    }
    let glyph = code_point_at(text, offset).map_or_default(from_code_point);
    let end = offset + glyph.len();
    (glyph, end)
}

pub fn decode_special_chars(text: JsString) -> JsString {
    let t = &*TABLE;
    let mut out: Option<JsString> = None;
    let (mut at, mut next) = (0, 0);
    while at < text.len() {
        let Some((decoded, end)) = t.read(&t.roles, &text, at) else {
            at += 1;
            continue;
        };
        if decoded[..] != text[at..end] {
            let out = out.get_or_insert_with(|| spare(text.len()));
            out.extend_from_slice(&text[next..at]);
            out.extend_from_slice(&decoded);
            next = end;
        }
        at = end;
    }
    if let Some(out) = &mut out {
        out.extend_from_slice(&text[next..]);
    }
    recycled(text, out)
}

pub fn clear_special_chars(text: JsString) -> Result<JsString> {
    canonical_quote_roles(decode_special_chars(text), &mut QuoteTags::default(), None, false)
}
