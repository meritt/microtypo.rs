use std::sync::LazyLock;

use crate::js::Re;

const SPACE_REFERENCE: &str = "(?:nbsp|thinsp|#0*(?:160|8239|8201)|#x0*(?:a0|202f|2009))";
const REFERENCE_BODY: &str = "(?:#[0-9]+|#[xX][0-9a-f]+|[a-z][a-z0-9]*)";
const UNENDING_REFERENCE: &str = "(?:laquo|raquo|ldquo|rdquo|lsquo|rsquo|hellip|#0*(?:171|187|8216|8217|8220|8221|8230)|#x0*(?:ab|bb|2018|2019|201c|201d|2026))";
const UNENDING: &str = r#"\s`![\]{};:'".,<>?«»“”‘’…&"#;
const EMAIL_NAME: &str = r"\p{L}0-9!#$%&'*+/=?^_`{|}~.-";
const NOT_EMAIL_DOMAIN: &str = r"(?<!@[\p{L}0-9.-]{0,253})";

fn amp() -> String {
    format!("&(?!(?:{SPACE_REFERENCE});)(?!{REFERENCE_BODY};)")
}

fn last(unending: &str) -> String {
    let last_reference = format!("&(?!(?:{SPACE_REFERENCE}|{UNENDING_REFERENCE});){REFERENCE_BODY};");
    format!("(?:{last_reference}|[^{unending}]|{})", amp())
}

fn address(end: &str) -> String {
    let reference = format!("&(?!(?:{SPACE_REFERENCE});){REFERENCE_BODY};");
    let body = format!(r#"(?:{reference}|[^\s<>"'`&]|{})"#, amp());
    let mailto = format!(r"mailto:(?=[^\s@]{{1,254}}@)[{EMAIL_NAME}]+@");
    format!(
        r"(?:{mailto}|(?:https?|ftps?|mailto):(?:\/{{1,3}}|[a-z0-9%])|www\d{{0,3}}[.]){body}*{end}|{NOT_EMAIL_DOMAIN}[a-z0-9.-]{{1,253}}[.][a-z]{{2,24}}\/(?:{body}*{end})?"
    )
}

pub static URL_REGEX: LazyLock<Re> = LazyLock::new(|| Re::new(&format!(r"\b({})", address(&last(UNENDING))), "giu"));

pub static URL_UNBRACKETED_START_REGEX: LazyLock<Re> =
    LazyLock::new(|| Re::new(&address(&last(&format!("{UNENDING})"))), "yiu"));

pub static URL_START_REGEX: LazyLock<Re> = LazyLock::new(|| Re::new(&address(&last(UNENDING)), "yiu"));

pub static EMAIL_REGEX: LazyLock<Re> = LazyLock::new(|| {
    Re::new(&format!(r"(?=[^\s@]{{0,254}}@)[{EMAIL_NAME}]+@[\p{{L}}0-9.-]+\.[\p{{L}}0-9-]{{2,}}"), "gimu")
});

pub static IPV6_REGEX: LazyLock<Re> = LazyLock::new(|| {
    Re::new(
        r"(?<![a-zA-Z0-9])\[?(?:[0-9a-fA-F]{1,4}:){1,7}:(?:[0-9a-fA-F]{1,4}(?::[0-9a-fA-F]{1,4})*)?\]?(?::\d{1,5})?(?:\/\d{1,3})?(?![a-zA-Z0-9])",
        "g",
    )
});

pub static UUID_REGEX: LazyLock<Re> = LazyLock::new(|| {
    Re::new(
        r"(?<![a-zA-Z0-9])[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}(?![a-zA-Z0-9])",
        "g",
    )
});
