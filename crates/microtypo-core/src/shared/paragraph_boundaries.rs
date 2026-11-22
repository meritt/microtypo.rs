use std::sync::LazyLock;

use crate::js::Re;

const PARAGRAPH_SPLIT_SOURCE: &str = r"(?<![ \t])([ \t]+)?(\n)+([ \t]*)(\n)+";

pub static PARAGRAPH_SPLIT_RE: LazyLock<Re> = LazyLock::new(|| Re::new(PARAGRAPH_SPLIT_SOURCE, "g"));
pub static PARAGRAPH_INDENT_SPLIT_RE: LazyLock<Re> =
    LazyLock::new(|| Re::new(&format!(r"{PARAGRAPH_SPLIT_SOURCE}([ \t]*)"), "g"));
