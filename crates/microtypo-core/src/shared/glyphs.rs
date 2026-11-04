use std::sync::LazyLock;

use crate::js::{JsString, Re, escape};

pub const NBSP: char = '\u{00A0}';
pub const NNBSP: char = '\u{202F}';
pub const THINSP: char = '\u{2009}';
pub const LAQUO: char = '«';
pub const RAQUO: char = '»';
pub const BDQUO: char = '„';
pub const LDQUO: char = '“';
pub const LSQUO: char = '‘';
pub const RSQUO: char = '’';
pub const PRIME: char = '″';
pub const MDASH: char = '—';
pub const NDASH: char = '–';
pub const MINUS: char = '−';
pub const HELLIP: char = '…';
pub const COPY: char = '©';
pub const REG: char = '®';
pub const TRADE: char = '™';
pub const SECT: char = '§';
pub const EURO: char = '€';
pub const RUB: char = '₽';
pub const DEG: char = '°';
pub const TIMES: char = '×';
pub const PLUSMN: char = '±';
pub const NUMERO: char = '№';
pub const NE: char = '≠';
pub const LE: char = '≤';
pub const GE: char = '≥';
pub const CONG: char = '≅';
pub const EQUIV: char = '≡';
pub const LARR: char = '←';
pub const RARR: char = '→';
pub const SUP2: char = '²';
pub const SUP3: char = '³';
pub const FRAC12: char = '½';
pub const FRAC14: char = '¼';
pub const FRAC34: char = '¾';
pub const COMBINING_ACUTE: char = '\u{0301}';

pub const fn unit(c: char) -> u16 {
    c as u32 as u16
}

pub const GLYPH_TO_ENTITY: &[(char, &str)] = &[
    (NBSP, "&nbsp;"),
    (NNBSP, "&#8239;"),
    (THINSP, "&thinsp;"),
    (LAQUO, "&laquo;"),
    (RAQUO, "&raquo;"),
    (BDQUO, "&bdquo;"),
    (LDQUO, "&ldquo;"),
    (LSQUO, "&lsquo;"),
    (RSQUO, "&rsquo;"),
    (PRIME, "&Prime;"),
    (MDASH, "&mdash;"),
    (NDASH, "&ndash;"),
    (MINUS, "&minus;"),
    (HELLIP, "&hellip;"),
    (COPY, "&copy;"),
    (REG, "&reg;"),
    (TRADE, "&trade;"),
    (SECT, "&sect;"),
    (EURO, "&euro;"),
    (RUB, "&#8381;"),
    (DEG, "&deg;"),
    (TIMES, "&times;"),
    (PLUSMN, "&plusmn;"),
    (NUMERO, "&#8470;"),
    (NE, "&ne;"),
    (LE, "&le;"),
    (GE, "&ge;"),
    (CONG, "&cong;"),
    (EQUIV, "&equiv;"),
    (LARR, "&larr;"),
    (RARR, "&rarr;"),
    (SUP2, "&sup2;"),
    (SUP3, "&sup3;"),
    (FRAC12, "&frac12;"),
    (FRAC14, "&frac14;"),
    (FRAC34, "&frac34;"),
    (COMBINING_ACUTE, "&#769;"),
];

static GLYPH_RE: LazyLock<Re> = LazyLock::new(|| {
    let body: String = GLYPH_TO_ENTITY.iter().map(|(g, _)| escape(&crate::js::u(&g.to_string()))).collect();
    Re::new(&format!("[{body}]"), "gu")
});

fn entity_for(g: u16) -> Option<&'static str> {
    GLYPH_TO_ENTITY.iter().find(|(c, _)| unit(*c) == g).map(|(_, e)| *e)
}

pub fn unicode_to_entities(text: &[u16]) -> JsString {
    GLYPH_RE.replace_with(text, |c| {
        let g = c.whole()[0];
        entity_for(g).map_or_else(|| c.whole().to_vec(), crate::js::u)
    })
}

pub fn unicode_to_xml_entities(text: &[u16]) -> JsString {
    GLYPH_RE.replace_with(text, |c| {
        let g = c.whole()[0];
        if entity_for(g).is_some() { crate::js::u(&format!("&#{g};")) } else { c.whole().to_vec() }
    })
}
