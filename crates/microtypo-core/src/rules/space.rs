use std::collections::HashSet;
use std::sync::LazyLock;

use crate::cat;
use crate::js::{self, Caps, JsString, Re};
use crate::rule_runner::{Replacement, Rule, RuleGroupDef};
use crate::shared::boundaries::CONTENT_START_GROUP;
use crate::shared::glyphs::{BDQUO, HELLIP, LAQUO, NBSP};

use super::punctmark::collapses_as_repeat;

const DOMAIN_ZONES: &[&str] = &[
    "ru",
    "рф",
    "ру",
    "ком",
    "орг",
    "уа",
    "ua",
    "uk",
    "co",
    "fr",
    "com",
    "net",
    "edu",
    "gov",
    "org",
    "mil",
    "int",
    "info",
    "biz",
    "name",
    "pro",
    "io",
    "dev",
    "app",
    "me",
    "tv",
    "cc",
    "us",
    "eu",
    "de",
    "jp",
    "cn",
    "br",
    "in",
    "au",
    "ai",
    "xyz",
    "example",
    "test",
    "invalid",
    "online",
    "store",
    "cloud",
    "email",
    "site",
    "website",
    "agency",
    "digital",
    "systems",
    "software",
    "technology",
    "network",
    "solutions",
    "studio",
    "space",
    "world",
    "group",
    "media",
    "center",
    "expert",
    "academy",
    "community",
    "moscow",
    "рус",
    "онлайн",
    "сайт",
    "москва",
];

const TECH_SUFFIXES: &[&str] = &[
    "js", "mjs", "cjs", "jsx", "ts", "tsx", "py", "rb", "go", "rs", "php", "java", "kt", "swift", "c", "cpp", "cc",
    "h", "hpp", "cs", "lua", "sh", "bat", "html", "htm", "css", "scss", "sass", "less", "xml", "svg", "json", "yaml",
    "yml", "toml", "ini", "conf", "env", "md", "mdx", "rst", "txt", "rtf", "pdf", "doc", "docx", "odt", "png", "jpg",
    "jpeg", "gif", "webp", "avif", "bmp", "ico", "tiff", "mp3", "mp4", "webm", "wav", "ogg", "flac", "mov", "avi",
    "mkv", "zip", "tar", "gz", "bz2", "xz", "rar", "7z", "exe", "dll", "so", "dmg", "app", "deb", "rpm",
];

fn set(words: &[&str]) -> HashSet<JsString> {
    words.iter().map(|w| js::u(w)).collect()
}

static ZONES: LazyLock<HashSet<JsString>> = LazyLock::new(|| set(DOMAIN_ZONES));
static SUFFIXES: LazyLock<HashSet<JsString>> = LazyLock::new(|| set(TECH_SUFFIXES));

static SENTENCE_WORD_RE: LazyLock<Re> = LazyLock::new(|| Re::new(r"^\p{Lu}\p{Ll}", "u"));
static CAPITAL_START_RE: LazyLock<Re> = LazyLock::new(|| Re::new("^[A-ZА-ЯЁ]", ""));
static LOWER_WORD_RE: LazyLock<Re> = LazyLock::new(|| Re::new("^[a-zа-яё0-9]+$", ""));
static DIGIT_START_RE: LazyLock<Re> = LazyLock::new(|| Re::new(r"^\d", ""));
static WORD_CHAR_RE: LazyLock<Re> = LazyLock::new(|| Re::new(r"[\p{L}\p{N}]", "u"));

fn is_known_tld(after_dot: &[u16], port_follows: bool) -> bool {
    ZONES.contains(&js::to_lower(after_dot)) && (port_follows || !SENTENCE_WORD_RE.test(after_dot))
}

fn is_capitalized_tech_name(before_dot: &[u16], after_dot: &[u16]) -> bool {
    CAPITAL_START_RE.test(before_dot) && LOWER_WORD_RE.test(after_dot)
}

fn is_part_of_multi_segment_id(terminator: &[u16]) -> bool {
    terminator == [0x2E] || DIGIT_START_RE.test(terminator)
}

fn glueing_swallows_a_mark(source: &[u16], at: usize, marks: &[u16]) -> bool {
    let mark = source[at - 1];
    let before = if at >= 2 { vec![source[at - 2]] } else { Vec::new() };

    if mark == 0x2E && WORD_CHAR_RE.test(&before) {
        return false;
    }

    collapses_as_repeat(mark, marks.first().copied())
}

fn should_keep_glued(before_dot: &[u16], after_dot: &[u16], terminator: &[u16], port_follows: bool) -> bool {
    is_known_tld(after_dot, port_follows)
        || SUFFIXES.contains(&js::to_lower(after_dot))
        || is_capitalized_tech_name(before_dot, after_dot)
        || is_part_of_multi_segment_id(terminator)
}

fn space_after_period(m: &Caps<'_>) -> JsString {
    let (host, zone, next) = (m.s(2), m.s(3), m.s(4));
    let port_follows = next == [0x3A] && DIGIT_START_RE.test(js::from(m.input, m.end()));
    let sep = if should_keep_glued(host, zone, next, port_follows) { "" } else { " " };

    cat!(m.s(1), host, ".", sep, zone, next)
}

pub fn space_group() -> RuleGroupDef {
    super::group(
        "Расстановка и удаление пробелов",
        super::classes(super::NOWRAP_CLASSES),
        vec![
            Rule::pattern(
                "nbsp_two_letter",
                Re::new(r#"(?<=[a-zA-Zа-яёА-ЯЁ])( |\t)+([A-ZА-ЯЁ]{2})(?=[\s;.?!:("»“]|$)"#, "gu"),
                Replacement::Template(cat!(NBSP, "$2")),
            )
            .describe("Неразрывный пробел перед 2-символьной аббревиатурой"),
            Rule::pattern(
                "trim_before_punctuation",
                Re::new(r"(?<!\.\.)(?<=[^ \t\u{00A0}])(( |\t|\u{00A0})+)([,:.;?!…]+)(?=\s|$)", "gu"),
                Replacement::func(|m, _| {
                    Ok(if glueing_swallows_a_mark(m.input, m.index(), m.s(3)) {
                        m.whole().to_vec()
                    } else {
                        m.s(3).to_vec()
                    })
                }),
            )
            .describe("Удаление пробела перед знаками препинания"),
            Rule::patterns(
                "space_after_comma",
                vec![
                    (Re::new(r"( |\t|\u{00A0}),([а-яёa-z0-9])", "giu"), Replacement::tpl(", $2")),
                    (Re::new(r"([^0-9]),([а-яёa-z0-9«„])", "giu"), Replacement::tpl("$1, $2")),
                ],
            )
            .describe("Пробел после запятой"),
            Rule::pattern(
                "space_after_punctuation",
                Re::new(r"( |\t|\u{00A0}|^|\n)([a-zа-яё0-9]+)( |\t|\u{00A0})?(:|\)|,|…|(?:!|\?)+)([а-яёa-z])", "giu"),
                Replacement::tpl("$1$2$4 $5"),
            )
            .describe("Пробел после знаков пунктуации, кроме точки"),
            Rule::patterns(
                "space_after_period",
                vec![
                    (
                        Re::new(r"( |\t|\u{00A0}|^)([a-zа-яё0-9]+)(?: |\t|\u{00A0})?\.([а-яёa-z]{5,})($|[^a-zа-яё])", "giu"),
                        Replacement::func(|m, _| Ok(space_after_period(m))),
                    ),
                    (
                        Re::new(r"( |\t|\u{00A0}|^)([a-zа-яё0-9]+)\.([а-яёa-z]{1,4})($|[^a-zа-яё])", "giu"),
                        Replacement::func(|m, _| Ok(space_after_period(m))),
                    ),
                ],
            )
            .describe("Пробел после точки"),
            Rule::pattern("space_after_ellipsis", Re::new(r"([?!]\.\.)([а-яёa-z])", "giu"), Replacement::tpl("$1 $2"))
                .describe("Пробел после троеточий с восклицанием/вопросом"),
            Rule::pattern("collapse_spaces", Re::new(r"( |\t)+", "g"), Replacement::tpl(" "))
                .describe("Удаление лишних пробельных символов и табуляций"),
            Rule::patterns(
                "strip_quote_padding",
                vec![
                    (Re::new(r"([«„])( |\t|\u{00A0})+", "gu"), Replacement::tpl("$1")),
                    (Re::new(r"( |\t|\u{00A0})+([»“])", "gu"), Replacement::tpl("$2")),
                ],
            )
            .describe("Удаление пробелов сразу после открывающей и перед закрывающей кавычкой"),
            Rule::pattern("trim_before_percent", Re::new(r"(?<!\d)(\d+)([\t ]+)%", "g"), Replacement::tpl("$1%"))
                .describe("Удаление пробела перед %"),
            Rule::pattern(
                "nbsp_before_quote",
                Re::new(&format!("{}([a-zа-яё]{{1,2}}) ([{LAQUO}{BDQUO}])", *CONTENT_START_GROUP), "gu"),
                Replacement::Template(cat!("$1$2", NBSP, "$3")),
            )
            .describe("Неразрывный пробел перед открывающей кавычкой"),
            Rule::pattern(
                "nbsp_before_month",
                Re::new(
                    r"(\d)(\s)+(января|февраля|марта|апреля|мая|июня|июля|августа|сентября|октября|ноября|декабря)([^<]|$)",
                    "giu",
                ),
                Replacement::Template(cat!("$1", NBSP, "$3$4")),
            )
            .describe("Неразрывный пробел в датах перед месяцем"),
            Rule::pattern("trim_line_end", Re::new(" +$", "g"), Replacement::tpl("")).describe("Удаление пробелов в конце текста"),
            Rule::pattern(
                "no_space_after_opening_ellipsis",
                Re::new(r"([«„(])( |\u{00A0})?…( |\u{00A0})?([a-zа-яё])", "giu"),
                Replacement::Template(cat!("$1", HELLIP, "$4")),
            )
            .describe("Отсутствие пробела после … после открывающей кавычки"),
            Rule::pattern(
                "trim_before_parenthetical_ellipsis",
                Re::new(r"([a-zа-яё0-9])(?: |\t|\u{00A0})+…(\))", "giu"),
                Replacement::Template(cat!("$1", HELLIP, "$2")),
            )
            .describe("Удаление пробела перед многоточием в конце скобок"),
            Rule::pattern(
                "space_after_year",
                Re::new(r"(^| |\u{00A0})([0-9]{3,4})(год([ауе]|ом)?)([^a-zа-яё]|$)", "giu"),
                Replacement::tpl("$1$2 $3$5"),
            )
            .describe("Пробел после года"),
        ],
    )
}
