use std::sync::LazyLock;

use crate::js::Re;

pub static URL_REGEX: LazyLock<Re> = LazyLock::new(|| {
    Re::new(
        r#"\b((?:(?:https?|ftps?|mailto):(?:\/{1,3}|[a-z0-9%])|www\d{0,3}[.]|[a-z0-9.-]{1,253}[.][a-z]{2,24}\/)[^\s<>"'`]*[^\s`![\]{};:'".,<>?«»“”‘’])"#,
        "giu",
    )
});

pub static EMAIL_REGEX: LazyLock<Re> = LazyLock::new(|| {
    Re::new(r"(?=[^\s@]{0,254}@)[\p{L}0-9!#$%&'*+/=?^_`{|}~.-]+@[\p{L}0-9.-]+\.[\p{L}0-9-]{2,}", "gimu")
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
