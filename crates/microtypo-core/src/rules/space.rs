use std::collections::HashSet;
use std::sync::LazyLock;

use crate::cat;
use crate::js::{self, JsString, Re};
use crate::rule_runner::{Ctx, Replacement, Rule, RuleGroupDef, SpacedYears};
use crate::shared::abbreviation_grammar::{DOTTED_INITIALS, SURNAME};
use crate::shared::boundaries::{CONTENT_START_GROUP, NOT_AFTER_LONE_QUOTE};
use crate::shared::glyphs::{BDQUO, HELLIP, LAQUO, LDQUO, NBSP, RAQUO, unit};
use crate::shared::measurement_units::CURRENCY_PREFIX_SIGNS;

use super::punctmark::collapses_as_repeat;
use super::quote_state::InlinePadding;
use crate::shared::rule_features::F;

pub(super) const PARTICLE: &str = "[Лл][Ии]|[Бб][Ыы]|[Бб]|[Жж][Ее]|[Жж]";

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

static WORD_MARK_RE: LazyLock<Re> =
    LazyLock::new(|| Re::new(r"(?<![a-zа-яё0-9])([a-zа-яё0-9]+)([ \t\u{00A0}]*)([.:),…]|[!?]+)(?=[а-яёa-z])", "giu"));
static AFTER_PERIOD_RE: LazyLock<Re> = LazyLock::new(|| Re::new("[а-яёa-z]+", "iuy"));
static PLAIN_GAP_RE: LazyLock<Re> = LazyLock::new(|| Re::new(r"^[ \t]*$", "u"));
static PUNCTUATION_START_RE: LazyLock<Re> = LazyLock::new(|| Re::new(r"[ \t\u{00A0}\n]", "u"));
static PERIOD_START_RE: LazyLock<Re> = LazyLock::new(|| Re::new(r"[ \t\u{00A0}]", "u"));
static DIGIT_RE: LazyLock<Re> = LazyLock::new(|| Re::new("[0-9]", "u"));
static SURNAME_RE: LazyLock<Re> = LazyLock::new(|| Re::new(&format!("^{SURNAME}$"), "u"));
static INITIALS_BEFORE_RE: LazyLock<Re> =
    LazyLock::new(|| Re::new(&format!(r"(?<=(?:\s|^|[.,;:?!]){})", *DOTTED_INITIALS), "uy"));

fn after_glued_initials(text: &[u16], offset: usize, word: &[u16]) -> bool {
    SURNAME_RE.test(word) && INITIALS_BEFORE_RE.exec_at(text, offset).is_some()
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Owner {
    Punctuation,
    Period,
}

fn unit_at(text: &[u16], at: usize) -> &[u16] {
    text.get(at..=at).unwrap_or_default()
}

fn space_boundaries(ctx: &Ctx<'_>, owner: Owner) -> Option<JsString> {
    let (periods, punctuation) = (ctx.bound.periods, ctx.bound.punctuation);
    let text = &ctx.text;
    let mut previous_boundary = None;
    WORD_MARK_RE.replace_matches(text, |m| {
        let (word, gap, mark) = (m.s(1), m.s(2), m.s(3));
        let offset = m.index();
        let period = mark == [0x2E];
        if !(if period { periods } else { punctuation }) {
            return m.whole().to_vec();
        }
        let boundary = if period { &*PERIOD_START_RE } else { &*PUNCTUATION_START_RE };
        if offset != 0
            && previous_boundary != Some(offset)
            && !boundary.test(&text[offset - 1..offset])
            && !after_glued_initials(text, offset, word)
        {
            return m.whole().to_vec();
        }
        if gap != [unit(NBSP)] && !PLAIN_GAP_RE.test(gap) {
            return m.whole().to_vec();
        }
        let after_mark = m.end();
        let mut separator: &[u16] = &[0x20];
        if period {
            let zone = AFTER_PERIOD_RE.exec_at(text, after_mark).map_or(&[][..], |z| z.whole());
            if zone.len() < 5 && !gap.is_empty() {
                return m.whole().to_vec();
            }
            let next = unit_at(text, after_mark + zone.len());
            let port = next == [0x3A] && DIGIT_RE.test(unit_at(text, after_mark + zone.len() + 1));
            if should_keep_glued(word, zone, next, port) {
                separator = &[];
            }
        }
        if !separator.is_empty() {
            previous_boundary = Some(after_mark);
        }
        if period == (owner == Owner::Period) { cat!(word, mark, separator) } else { m.whole().to_vec() }
    })
}

const TRIM_MARKS: &str = ",:.;?!…";
static TRIM_BEFORE_PUNCTUATION: LazyLock<String> = LazyLock::new(|| {
    format!(
        r"(?<!\.\.)(?<=[^ \t{NBSP}]){}(( |\t|{NBSP})+)([{TRIM_MARKS}]+)(?=\s|$|(?<=[{TRIM_MARKS}],))",
        *NOT_AFTER_LONE_QUOTE
    )
});
const GAP_BEFORE_COMMA_WORD: &str = r"(?<![ \t\u{00A0}])[ \t\u{00A0}]+,([а-яёa-z0-9«„])";
static TRIM_AT: LazyLock<Re> = LazyLock::new(|| Re::new(&TRIM_BEFORE_PUNCTUATION, "uy"));
static COMMA_GAP_AT: LazyLock<Re> = LazyLock::new(|| Re::new(GAP_BEFORE_COMMA_WORD, "iuy"));

fn is_trim_mark(c: Option<u16>) -> bool {
    c.is_some_and(|c| TRIM_MARKS.encode_utf16().any(|m| m == c))
}

pub fn punctuation_after_inline_gap(text: &[u16], at: usize, active: InlinePadding, points: bool) -> usize {
    let mut mark = at;
    while matches!(text.get(mark), Some(&(0x20 | 0x09 | 0xA0))) {
        mark += 1;
    }
    if mark == at || !is_trim_mark(text.get(mark).copied()) {
        return at;
    }
    let local;
    let (source, start) = if points {
        let mut end = mark + 1;
        while end < text.len() && is_trim_mark(Some(text[end])) {
            end += 1;
        }
        local = cat!("> ", js::slice(text, mark, end + 1));
        (&local[..], 1)
    } else {
        (text, at)
    };
    if (active.trim_before_punctuation && TRIM_AT.exec_at(source, start).is_some())
        || (active.space_after_comma && COMMA_GAP_AT.exec_at(source, start).is_some())
    {
        mark
    } else {
        at
    }
}

pub fn space_group() -> RuleGroupDef {
    for re in [&WORD_MARK_RE, &AFTER_PERIOD_RE, &PLAIN_GAP_RE, &PUNCTUATION_START_RE, &PERIOD_START_RE, &DIGIT_RE] {
        LazyLock::force(re);
    }
    LazyLock::force(&SURNAME_RE);
    LazyLock::force(&INITIALS_BEFORE_RE);
    super::group(
        "Расстановка и удаление пробелов",
        super::classes(super::NOWRAP_CLASSES),
        vec![
            Rule::pattern(
                "nbsp_two_letter",
                Re::new(r#"(?<=[a-zA-Zа-яёА-ЯЁ])( |\t)+([A-ZА-ЯЁ]{2})(?=[\s;.?!:("»“]|$)"#, "gu"),
                Replacement::Template(cat!(NBSP, "$2")),
            )
            .describe("Неразрывный пробел перед 2-символьной аббревиатурой").requires(F::SPACE),
            Rule::pattern(
                "trim_before_punctuation",
                Re::new(&TRIM_BEFORE_PUNCTUATION, "gu"),
                Replacement::func(|m, _| {
                    Ok(if glueing_swallows_a_mark(m.input, m.index(), m.s(3)) {
                        m.whole().to_vec()
                    } else {
                        m.s(3).to_vec()
                    })
                }),
            )
            .describe("Удаление пробела перед знаками препинания").requires(F::SPACE).requires_any(F::PERIOD | F::COMMA | F::COLON | F::EXCLAMATION | F::QUESTION | F::ELLIPSIS | F::SEMICOLON),
            Rule::patterns(
                "space_after_comma",
                vec![
                    (Re::new(GAP_BEFORE_COMMA_WORD, "giu"), Replacement::tpl(", $1")),
                    (Re::new(r"([^0-9]),([а-яёa-z0-9«„])", "giu"), Replacement::tpl("$1, $2")),
                ],
            )
            .describe("Пробел после запятой").requires(F::COMMA),
            Rule::handler("space_after_punctuation", |ctx| Ok(space_boundaries(ctx, Owner::Punctuation)))
                .describe("Пробел после знаков пунктуации, кроме точки").requires_any(F::COMMA | F::COLON | F::EXCLAMATION | F::QUESTION | F::PAREN | F::ELLIPSIS)
                .requires_pattern(Re::new("[:),…!?][а-яёa-z]", "iu")),
            Rule::handler("space_after_period", |ctx| Ok(space_boundaries(ctx, Owner::Period)))
                .describe("Пробел после точки").requires(F::PERIOD)
                .requires_pattern(Re::new(r"\.[а-яёa-z]", "iu")),
            Rule::pattern("space_after_ellipsis", Re::new(r"([?!]\.\.)([а-яёa-z])", "giu"), Replacement::tpl("$1 $2"))
                .describe("Пробел после троеточий с восклицанием/вопросом").requires(F::PERIOD).requires_any(F::EXCLAMATION | F::QUESTION),
            Rule::pattern("collapse_spaces", Re::new(r"( |\t)+", "g"), Replacement::tpl(" "))
                .describe("Удаление лишних пробельных символов и табуляций")
                .requires(F::SPACE)
                .project_spaced_years(SpacedYears::Gaps),
            Rule::patterns(
                "strip_quote_padding",
                vec![
                    (Re::new(r"([«„])( |\t|\u{00A0})+", "gu"), Replacement::tpl("$1")),
                    (
                        Re::new(&format!(r"(?<![ \t{NBSP}]){}( |\t|{NBSP})+([{RAQUO}{LDQUO}])", *NOT_AFTER_LONE_QUOTE), "gu"),
                        Replacement::tpl("$2"),
                    ),
                ],
            )
            .describe("Удаление пробелов сразу после открывающей и перед закрывающей кавычкой").requires(F::SPACE | F::QUOTE),
            Rule::pattern("trim_before_percent", Re::new(r"(?<!\d)(\d+)([\t ]+)%", "g"), Replacement::tpl("$1%"))
                .describe("Удаление пробела перед %").requires(F::DIGIT | F::PERCENT),
            Rule::pattern(
                "nbsp_before_quote",
                Re::new(&format!(r"{}(?<!\d{NBSP})([a-zа-яё]{{1,2}})(?<![ \t{NBSP}](?:{PARTICLE})) (?=[{LAQUO}{BDQUO}])", *CONTENT_START_GROUP), "gu"),
                Replacement::Template(cat!("$1$2", NBSP)),
            )
            .describe("Неразрывный пробел перед открывающей кавычкой").requires(F::SPACE | F::QUOTE),
            Rule::pattern(
                "nbsp_before_month",
                Re::new(
                    r"(\d)(\s)+(января|февраля|марта|апреля|мая|июня|июля|августа|сентября|октября|ноября|декабря)([^<]|$)",
                    "giu",
                ),
                Replacement::Template(cat!("$1", NBSP, "$3$4")),
            )
            .describe("Неразрывный пробел в датах перед месяцем").requires(F::SPACE | F::DIGIT),
            Rule::pattern("trim_line_end", Re::new("(?<! ) +$", "g"), Replacement::tpl("")).describe("Удаление пробелов в конце текста").requires(F::SPACE),
            Rule::pattern(
                "no_space_after_opening_ellipsis",
                Re::new(r"([«„(])( |\u{00A0})?…( |\u{00A0})?([a-zа-яё])", "giu"),
                Replacement::Template(cat!("$1", HELLIP, "$4")),
            )
            .describe("Отсутствие пробела после … после открывающей кавычки").requires(F::ELLIPSIS),
            Rule::pattern(
                "trim_before_parenthetical_ellipsis",
                Re::new(r"([a-zа-яё0-9])(?: |\t|\u{00A0})+…(\))", "giu"),
                Replacement::Template(cat!("$1", HELLIP, "$2")),
            )
            .describe("Удаление пробела перед многоточием в конце скобок").requires(F::PAREN | F::ELLIPSIS),
            Rule::pattern(
                "space_after_year",
                Re::new(
                    &format!(
                        r"(^|[ \t{NBSP}{}]|(?<=\p{{L}})\.)([0-9]{{3,4}})(год([ауе]|ом)?)(?=[^a-zа-яё]|$)",
                        *CURRENCY_PREFIX_SIGNS
                    ),
                    "giu",
                ),
                Replacement::tpl("$1$2 $3"),
            )
            .describe("Пробел после года")
            .requires(F::DIGIT)
            .requires_pattern(Re::new(r"\dгод", "iu")),
        ],
    )
}
