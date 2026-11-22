use crate::js::JsString;
use crate::js::Re;
use std::sync::LazyLock;

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
pub const SINGLE_PRIME: char = '′';
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

pub const EXTENDED_FRACTION_GLYPHS: &[u16] = &[0x2153, 0x2154, 0x215B, 0x215C, 0x215D, 0x215E];
pub static FRACTION_GLYPHS: LazyLock<String> =
    LazyLock::new(|| format!("{FRAC12}{FRAC14}{FRAC34}{}", crate::js::lossy(EXTENDED_FRACTION_GLYPHS)));
pub const OPTIONAL_GLYPH_RULES: &[(&str, &str, &[u16])] = &[
    ("number", "fraction_extended", EXTENDED_FRACTION_GLYPHS),
    ("number", "minus", EXTENDED_FRACTION_GLYPHS),
    ("number", "prime", &[0x2032]),
    ("symbol", "arrows_extended", &[0x2194, 0x21D2, 0x21D4]),
];

pub fn decode_optional_glyphs(text: &[u16], selected: &[u16], xml: bool) -> JsString {
    static REFERENCE: LazyLock<Re> = LazyLock::new(|| Re::new(r"&#(?:[xX]([0-9A-Fa-f]+)|([0-9]+));", "gu"));
    static XML_REFERENCE: LazyLock<Re> =
        LazyLock::new(|| Re::new(r"&#(?:[xX]([0-9A-Fa-f]+)|([0-9]+))\u{e000}N\d{1,9}\u{e001};", "gu"));
    let reference = if xml { &XML_REFERENCE } else { &REFERENCE };
    reference.replace_with(text, |m| {
        let (digits, radix) = if m.has(1) { (m.s(1), 16) } else { (m.s(2), 10) };
        match u16::from_str_radix(&crate::js::lossy(digits), radix) {
            Ok(value) if selected.contains(&value) => vec![value],
            _ => m.whole().to_vec(),
        }
    })
}

pub fn encode_optional_glyphs(text: &[u16], selected: &[u16]) -> JsString {
    let mut out = Vec::with_capacity(text.len());
    for &g in text {
        if selected.contains(&g) {
            out.extend_from_slice(&[u16::from(b'&'), u16::from(b'#')]);
            push_decimal(g, &mut out);
            out.push(u16::from(b';'));
        } else {
            out.push(g);
        }
    }
    out
}

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

const LAST_GLYPH: u16 = {
    let mut last = 0;
    let mut i = 0;
    while i < GLYPH_TO_ENTITY.len() {
        if unit(GLYPH_TO_ENTITY[i].0) > last {
            last = unit(GLYPH_TO_ENTITY[i].0);
        }
        i += 1;
    }
    last
};

fn entity_for(g: u16) -> Option<&'static str> {
    if !(0xA0..=LAST_GLYPH).contains(&g) || (0x100..0x2000).contains(&g) && g != unit(COMBINING_ACUTE) {
        return None;
    }
    GLYPH_TO_ENTITY.iter().find(|&&(c, _)| unit(c) == g).map(|&(_, e)| e)
}

fn replace_glyphs(text: &[u16], write: impl Fn(u16, &'static str, &mut JsString)) -> JsString {
    let mut out = Vec::with_capacity(text.len());
    let mut copied = 0;
    for (i, &g) in text.iter().enumerate() {
        if let Some(entity) = entity_for(g) {
            out.extend_from_slice(&text[copied..i]);
            write(g, entity, &mut out);
            copied = i + 1;
        }
    }
    out.extend_from_slice(&text[copied..]);
    out
}

pub fn unicode_to_entities(text: &[u16]) -> JsString {
    replace_glyphs(text, |_, entity, out| out.extend(entity.bytes().map(u16::from)))
}

fn push_decimal(n: u16, out: &mut JsString) {
    if n >= 10 {
        push_decimal(n / 10, out);
    }
    out.push(u16::from(b'0') + n % 10);
}

pub fn unicode_to_xml_entities(text: &[u16]) -> JsString {
    replace_glyphs(text, |g, _, out| {
        out.extend_from_slice(&[u16::from(b'&'), u16::from(b'#')]);
        push_decimal(g, out);
        out.push(u16::from(b';'));
    })
}
