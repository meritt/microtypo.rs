use std::sync::LazyLock;

use crate::protect::placeholders::{CLOSE, OPEN};
use crate::shared::glyphs::{BDQUO, HELLIP, LAQUO, LDQUO, MDASH, MINUS, NDASH, PLUSMN, RAQUO};

pub static CONTENT_OPEN_BRACKETS: LazyLock<String> = LazyLock::new(|| format!(r"(\[{{{LAQUO}{BDQUO}"));

static CONTENT_START_INSIDE: LazyLock<String> =
    LazyLock::new(|| format!(r"\s,\-{CLOSE}{MINUS}{MDASH}{NDASH}{}", *CONTENT_OPEN_BRACKETS));

pub static CONTENT_START: LazyLock<String> = LazyLock::new(|| format!(">{}", *CONTENT_START_INSIDE));

pub static CONTENT_START_GROUP: LazyLock<String> = LazyLock::new(|| format!("(^|[{}])", *CONTENT_START));

pub static WRAPPER_START_GROUP: LazyLock<String> = LazyLock::new(|| format!("(^|[{}])", *CONTENT_START_INSIDE));

static CONTENT_END: LazyLock<String> =
    LazyLock::new(|| format!(r"\s.,;:!?{PLUSMN}%<{OPEN})\]}}{RAQUO}{LDQUO}{HELLIP}"));

pub static CONTENT_END_LOOKAHEAD: LazyLock<String> = LazyLock::new(|| format!("(?=[{}]|$)", *CONTENT_END));
