use std::sync::LazyLock;

use crate::js::{JsString, Lit, Re};
use crate::lit;

pub const OPEN: char = '\u{E000}';
pub const CLOSE: char = '\u{E001}';
pub const OPEN_UNIT: u16 = 0xE000;
pub const CLOSE_UNIT: u16 = 0xE001;
const PARA_DELIM_OPEN: char = '\u{E100}';
const PARA_DELIM_CLOSE: char = '\u{E101}';

pub const RESERVED_CHARS: [u16; 4] = [0xE000, 0xE001, 0xE100, 0xE101];

pub const RESERVED_PUA_CLASS: &str = r"\u{E000}\u{E001}\u{E100}\u{E101}";

static RESERVED_PUA_RE: LazyLock<Re> = LazyLock::new(|| Re::new(&format!("[{RESERVED_PUA_CLASS}]"), "gu"));

pub fn strip_reserved_pua(text: JsString) -> JsString {
    RESERVED_PUA_RE.replace_owned(text, |_| Vec::new())
}

pub static REFERENCE_HOLE: LazyLock<String> = LazyLock::new(|| format!(r"{OPEN}N\d{{1,9}}{CLOSE}"));

pub fn vaulted_reference(glyph: char) -> String {
    format!("&#{}{};", glyph as u32, *REFERENCE_HOLE)
}

pub fn placeholder_regex(prefix: &str, flags: &str) -> Re {
    Re::new(&format!(r"{OPEN}{prefix}(\d+){CLOSE}"), flags)
}

pub static PLACEHOLDER_TOKEN: LazyLock<String> = LazyLock::new(|| format!(r"{OPEN}[A-Za-z]+\d+{CLOSE}"));

pub static TAG_PLACEHOLDER_SOURCE: LazyLock<String> = LazyLock::new(|| format!("<(/?)({})>", *PLACEHOLDER_TOKEN));

pub const BLOCK_KIND: char = 'L';
pub const TRANSPARENT_TAG_KINDS: &str = "TA";
pub static INLINE_END: LazyLock<String> = LazyLock::new(|| format!(r"<\/{OPEN}[{TRANSPARENT_TAG_KINDS}]\d+{CLOSE}>"));
pub const TRANSPARENT_BARE_KINDS: &str = "NHWF";
pub const TAIL_KINDS: &str = "EF";
pub const TAG_VAULT_KINDS: &str = "TALIH";

pub static PARAGRAPH_OPEN: Lit = lit!("\u{E100}POP\u{E101}");
pub static PARAGRAPH_CLOSE: Lit = lit!("\u{E100}PCL\u{E101}");
pub static BREAKLINE: Lit = lit!("\u{E100}BR\u{E101}");

static PARAGRAPH_RESTORE_RE: LazyLock<Re> =
    LazyLock::new(|| Re::new(&format!("{PARA_DELIM_OPEN}(POP|PCL|BR){PARA_DELIM_CLOSE}"), "gu"));

pub static PARAGRAPH_TAG: LazyLock<String> = LazyLock::new(|| format!(r"{OPEN}[TAL]\d+{CLOSE}"));
static PARAGRAPH_OPEN_TAGGED_RE: LazyLock<Re> =
    LazyLock::new(|| Re::new(&format!("{PARA_DELIM_OPEN}POP{PARA_DELIM_CLOSE}({})", *PARAGRAPH_TAG), "gu"));
static PARAGRAPH_CLOSE_TAGGED_RE: LazyLock<Re> =
    LazyLock::new(|| Re::new(&format!("({}){PARA_DELIM_OPEN}PCL{PARA_DELIM_CLOSE}", *PARAGRAPH_TAG), "gu"));

pub fn drop_paragraphs(text: JsString) -> JsString {
    PARAGRAPH_RESTORE_RE.replace_owned(text, |_| Vec::new())
}

pub fn restore_paragraphs(text: JsString) -> JsString {
    restore_paragraphs_with(text, None)
}

pub fn restore_paragraphs_with(
    text: JsString,
    mut mapping: Option<&mut crate::shared::provenance::RestoreMap>,
) -> JsString {
    let text = crate::shared::provenance::replace_recorded(
        text,
        &PARAGRAPH_OPEN_TAGGED_RE,
        |c| crate::cat!("<", c.s(1), ">"),
        mapping.as_deref_mut(),
    );
    let text = crate::shared::provenance::replace_recorded(
        text,
        &PARAGRAPH_CLOSE_TAGGED_RE,
        |c| crate::cat!("</", c.s(1), ">"),
        mapping.as_deref_mut(),
    );
    crate::shared::provenance::replace_recorded(
        text,
        &PARAGRAPH_RESTORE_RE,
        |c| {
            let kind = c.s(1);
            crate::js::u(match kind {
                [0x50, 0x4F, 0x50] => "<p>",
                [0x50, 0x43, 0x4C] => "</p>",
                _ => "<br>",
            })
        },
        mapping,
    )
}
