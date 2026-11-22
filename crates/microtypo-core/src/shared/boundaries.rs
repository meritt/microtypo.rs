use std::sync::LazyLock;

use crate::js::{self, Re};
use crate::protect::placeholders::{BLOCK_KIND, CLOSE, OPEN, TRANSPARENT_TAG_KINDS};
use crate::shared::glyphs::{BDQUO, HELLIP, LAQUO, LDQUO, MDASH, MINUS, NDASH, PLUSMN, RAQUO};
use crate::shared::strings::WHITESPACE;

pub static CONTENT_OPEN_BRACKETS: LazyLock<String> = LazyLock::new(|| format!(r"(\[{{{LAQUO}{BDQUO}"));

static CONTENT_START_INSIDE: LazyLock<String> =
    LazyLock::new(|| format!(r"\s,\-{CLOSE}{MINUS}{MDASH}{NDASH}{}", *CONTENT_OPEN_BRACKETS));

pub static CONTENT_START: LazyLock<String> = LazyLock::new(|| format!(">{}", *CONTENT_START_INSIDE));

pub static CONTENT_START_GROUP: LazyLock<String> = LazyLock::new(|| format!("(^|[{}])", *CONTENT_START));

pub static WRAPPER_START_GROUP: LazyLock<String> = LazyLock::new(|| format!("(^|[{}])", *CONTENT_START_INSIDE));

pub static BLOCK_CONTENT_START: LazyLock<String> =
    LazyLock::new(|| format!(r"(?<=<{OPEN}{BLOCK_KIND}\d{{1,9}}{CLOSE}>)"));

pub static BLOCK_WRAPPER_START_GROUP: LazyLock<String> =
    LazyLock::new(|| format!("(^|[{}]|{})", *CONTENT_START_INSIDE, *BLOCK_CONTENT_START));

static CONTENT_END_SIGNS: LazyLock<String> = LazyLock::new(|| format!(".,;:!?{PLUSMN}%<)]}}{RAQUO}{LDQUO}{HELLIP}"));

static CONTENT_END: LazyLock<String> =
    LazyLock::new(|| format!(r"\s{}{OPEN}", js::regex::escape_str(&CONTENT_END_SIGNS)));

pub static CONTENT_END_LOOKAHEAD: LazyLock<String> = LazyLock::new(|| format!("(?=[{}]|$)", *CONTENT_END));

pub fn written_content_end_lookahead(reference_to: fn(&[char]) -> String) -> String {
    let glyphs: Vec<char> = WHITESPACE.chars().chain(CONTENT_END_SIGNS.chars()).collect();
    format!("(?:{}|(?={}))", *CONTENT_END_LOOKAHEAD, reference_to(&glyphs))
}

static STRONG_QUOTE_LEFT: LazyLock<String> = LazyLock::new(|| format!(r"\s([{LAQUO}{MDASH}{NDASH}\-"));
pub static OPEN_LEFT: LazyLock<String> = LazyLock::new(|| format!("{}>", *STRONG_QUOTE_LEFT));
pub const WEAK_LEFT: &str = ",;:*_~`";
pub static WEAK_STOP: LazyLock<String> = LazyLock::new(|| format!(r"\s.,;:!?\p{{Pe}}{RAQUO}{HELLIP}"));
static WEAK_STOP_RE: LazyLock<Re> = LazyLock::new(|| Re::new(&format!("[{}]", *WEAK_STOP), "u"));

pub(crate) fn is_strong_left(c: u16) -> bool {
    js::is_js_space(c) || matches!(c, 0x28 | 0x5B | 0xAB | 0x2014 | 0x2013 | 0x2D | 0x3E)
}

pub(crate) fn is_weak_left(c: u16) -> bool {
    matches!(c, 0x2C | 0x3B | 0x3A | 0x2A | 0x5F | 0x7E | 0x60 | 0xE001)
}

fn is_weak_stop(c: u16) -> bool {
    WEAK_STOP_RE.test(&[c])
}

pub const QUOTE_PUNCTUATION: &str = r"\p{Terminal_Punctuation}\p{Pe}…";
static QUOTE_PUNCTUATION_RE: LazyLock<Re> = LazyLock::new(|| Re::new(&format!("[{QUOTE_PUNCTUATION}]+"), "uy"));

pub fn is_quote_dash(glyph: Option<u16>) -> bool {
    matches!(glyph, Some(0x2014 | 0x2013))
}

pub fn dash_quote_punctuation(
    text: &[u16],
    after: usize,
    left: Option<u16>,
    mut next_inline: Option<&mut dyn FnMut(usize) -> Option<usize>>,
) -> bool {
    if !is_quote_dash(left) {
        return false;
    }
    let mut at = after;
    while at < text.len() {
        if text[at] == 0x3C
            && let Some(next_inline) = next_inline.as_mut()
        {
            match next_inline(at) {
                None => return true,
                Some(next) if next != at => {
                    at = next;
                    continue;
                }
                Some(_) => {}
            }
        }
        match QUOTE_PUNCTUATION_RE.exec_at(text, at) {
            Some(m) => at = m.end(),
            None => return at > after && js::is_js_space(text[at]),
        }
    }
    at > after
}

pub fn opening_quote_neighbours(left: Option<u16>, right: u16) -> bool {
    match left {
        None => true,
        Some(left) => is_strong_left(left) || (is_weak_left(left) && !is_weak_stop(right)),
    }
}

static OPENING_GT: LazyLock<String> = LazyLock::new(|| format!(r"(?<!<\/{OPEN}[{TRANSPARENT_TAG_KINDS}]\d+{CLOSE})>"));
pub static NOT_AFTER_LONE_QUOTE: LazyLock<String> =
    LazyLock::new(|| format!(r#"(?<!(?:^|[{}]|{})")"#, *STRONG_QUOTE_LEFT, *OPENING_GT));

pub const QUOTE_WORD: &str = "[a-zа-яё0-9]";

static QUOTE_WORD_RE: LazyLock<Re> = LazyLock::new(|| Re::new(QUOTE_WORD, "iu"));

fn is_quote_word(c: u16) -> bool {
    QUOTE_WORD_RE.test(&[c])
}

pub fn inside_quote_word(left: Option<u16>, right: Option<u16>) -> bool {
    left.is_some_and(is_quote_word) && right.is_some_and(is_quote_word)
}

pub const OPEN_CYRILLIC_SOURCE: &str = r#"([a-zа-яё0-9])"([а-яё])"#;
static GLUED_OPEN: LazyLock<Re> = LazyLock::new(|| Re::new(OPEN_CYRILLIC_SOURCE, "iu"));

pub fn opens_glued_cyrillic(left: Option<u16>, right: Option<u16>) -> bool {
    let mut text: Vec<u16> = left.into_iter().collect();
    text.push(0x22);
    text.extend(right);
    GLUED_OPEN.test(&text)
}
