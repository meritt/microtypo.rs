use std::sync::OnceLock;

use crate::js::{JsString, Re};
use crate::shared::strings::WHITESPACE;

pub struct F;

impl F {
    pub const SPACE: u32 = 1 << 0;
    pub const DIGIT: u32 = 1 << 1;
    pub const QUOTE: u32 = 1 << 2;
    pub const DASH: u32 = 1 << 3;
    pub const PERIOD: u32 = 1 << 4;
    pub const COMMA: u32 = 1 << 5;
    pub const COLON: u32 = 1 << 6;
    pub const EXCLAMATION: u32 = 1 << 7;
    pub const QUESTION: u32 = 1 << 8;
    pub const PAREN: u32 = 1 << 9;
    pub const APOSTROPHE: u32 = 1 << 10;
    pub const DEGREE: u32 = 1 << 11;
    pub const SLASH: u32 = 1 << 12;
    pub const MATH: u32 = 1 << 13;
    pub const CARET: u32 = 1 << 14;
    pub const UNDERSCORE: u32 = 1 << 15;
    pub const NUMERO: u32 = 1 << 16;
    pub const SECTION: u32 = 1 << 17;
    pub const ELLIPSIS: u32 = 1 << 18;
    pub const FRACTION: u32 = 1 << 19;
    pub const LATIN: u32 = 1 << 20;
    pub const COPYRIGHT: u32 = 1 << 21;
    pub const MARKUP: u32 = 1 << 22;
    pub const BACKTICK: u32 = 1 << 23;
    pub const PERCENT: u32 = 1 << 24;
    pub const MONEY: u32 = 1 << 25;
    pub const SEMICOLON: u32 = 1 << 26;
    pub const CYRILLIC: u32 = 1 << 27;
    pub const NEWLINE: u32 = 1 << 28;
    pub const AT: u32 = 1 << 29;
    pub const SOURCE: u32 = 1 << 30;
}

const CHARACTER_SETS: [(u32, &str); 31] = [
    (F::SPACE, WHITESPACE),
    (F::DIGIT, "0123456789"),
    (F::QUOTE, "\"«»„“"),
    (F::DASH, "-—–−"),
    (F::PERIOD, "."),
    (F::COMMA, ","),
    (F::COLON, ":"),
    (F::EXCLAMATION, "!"),
    (F::QUESTION, "?"),
    (F::PAREN, "()"),
    (F::APOSTROPHE, "'"),
    (F::DEGREE, "°"),
    (F::SLASH, "/"),
    (F::MATH, "=+"),
    (F::CARET, "^"),
    (F::UNDERSCORE, "_"),
    (F::NUMERO, "№"),
    (F::SECTION, "§"),
    (F::ELLIPSIS, "…"),
    (F::FRACTION, "½¼¾⅓⅔⅛⅜⅝⅞"),
    (F::LATIN, "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ"),
    (F::COPYRIGHT, "©"),
    (F::MARKUP, "<"),
    (F::BACKTICK, "`"),
    (F::PERCENT, "%"),
    (F::MONEY, "$€₽"),
    (F::SEMICOLON, ";"),
    (F::CYRILLIC, "абвгдеёжзийклмнопрстуфхцчшщъыьэюяАБВГДЕЁЖЗИЙКЛМНОПРСТУФХЦЧШЩЪЫЬЭЮЯ"),
    (F::NEWLINE, "\r\n"),
    (F::AT, "@"),
    (F::SOURCE, "&<>{}[]*_`.:@-\r\n\u{2010}\u{2012}\u{201C}\u{201D}\u{201E}\u{201F}\u{E000}\u{E001}\u{E100}\u{E101}"),
];

fn by_code() -> &'static [u32] {
    static BY_CODE: OnceLock<Box<[u32]>> = OnceLock::new();
    BY_CODE.get_or_init(|| {
        let mut table = vec![0; 0x10000].into_boxed_slice();
        for (feature, characters) in CHARACTER_SETS {
            for code in characters.encode_utf16() {
                table[usize::from(code)] |= feature;
            }
        }
        table
    })
}

pub fn text_mask(text: &[u16]) -> u32 {
    let by_code = by_code();
    let mut mask = 0;
    for &code in text {
        let known = by_code[usize::from(code)];
        mask |= if known != 0 {
            known
        } else if code >= 0x80 {
            F::LATIN | F::CYRILLIC
        } else {
            0
        };
    }
    mask
}

#[derive(Debug, Default)]
pub struct Features {
    text: Option<JsString>,
    mask: u32,
    previous_text: Option<JsString>,
    previous_mask: u32,
}

pub fn feature_mask(text: &[u16], features: &mut Features) -> u32 {
    if features.text.as_deref() == Some(text) {
        return features.mask;
    }
    if features.previous_text.as_deref() == Some(text) {
        return features.previous_mask;
    }

    let mask = text_mask(text);
    let mut reused = features.previous_text.take().unwrap_or_default();
    reused.clear();
    reused.extend_from_slice(text);
    features.previous_text = features.text.replace(reused);
    features.previous_mask = features.mask;
    features.mask = mask;
    mask
}

#[derive(Debug, Clone, Default)]
pub struct Requires {
    pub all: u32,
    pub any: u32,
    pub pattern: Option<Re>,
}

impl Requires {
    pub fn met_by(&self, mask: u32) -> bool {
        mask & self.all == self.all && (self.any == 0 || mask & self.any != 0)
    }
}

pub fn has_required_features(
    rule: &Requires,
    text: &[u16],
    features: &mut Features,
    known: &mut Option<u32>,
    lexical: bool,
) -> bool {
    if (rule.all != 0 || rule.any != 0) && !rule.met_by(*known.get_or_insert_with(|| feature_mask(text, features))) {
        return false;
    }

    !lexical || rule.pattern.as_ref().is_none_or(|re| re.test(text))
}

pub fn allows(
    features: &mut Option<Features>,
    rule: &Requires,
    text: &[u16],
    known: &mut Option<u32>,
    lexical: bool,
) -> bool {
    features.as_mut().is_none_or(|features| has_required_features(rule, text, features, known, lexical))
}
