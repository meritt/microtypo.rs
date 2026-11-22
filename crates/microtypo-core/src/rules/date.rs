use std::sync::LazyLock;

use crate::cat;
use crate::errors::Result;
use crate::js::{Caps, JsString, Re};
use crate::protect::placeholders::{CLOSE, OPEN, TRANSPARENT_TAG_KINDS};
use crate::rule_runner::{Ctx, Replacement, Rule, RuleGroupDef};
use crate::shared::boundaries::{CONTENT_END_LOOKAHEAD, CONTENT_START_GROUP, written_content_end_lookahead};
use crate::shared::character_reference::kept_reference_to;
use crate::shared::glyphs::{COPY, MDASH, MINUS, NBSP, NDASH, NNBSP, unit};
use crate::shared::nowrap_context::nowrap;
use crate::shared::rule_features::{F, Requires};

static YEAR_MARK: LazyLock<String> = LazyLock::new(|| format!(r"[ \t{NBSP}]?г(?:[ \t{NBSP}]*\.|(?=\s))"));

static DATE_YEAR_RE: LazyLock<Re> = LazyLock::new(|| {
    let inline_start = format!(r"<{OPEN}[{TRANSPARENT_TAG_KINDS}]\d{{1,9}}{CLOSE}>");
    Re::new(
        &format!(r"(?<=^|\s|{inline_start})([0-9]{{2}}\.[0-9]{{2}}\.(?:[0-9]{{2}})?[0-9]{{2}}){}", *YEAR_MARK),
        "giu",
    )
});

static GROUPED_NUMBER_PREFIX: LazyLock<String> = LazyLock::new(|| {
    let digits: Vec<char> = "0123456789".chars().collect();
    format!("(?:\\d|{})(?:{NNBSP}|{})", kept_reference_to(&digits), kept_reference_to(&[NNBSP]))
});

fn year_range_pattern(dash: &str, gap: &str) -> Re {
    Re::new(
        &format!(
            r"((?:с|по|период|середины|начала|начало|конца|конец|половины|в|между|\([cс]\)|{COPY})\s+)(\d{{4}})({dash})(\d{{4}})(?:{gap}(г\.г\.|гг\.|гг|г\.|г)(?![а-яёa-z]))?"
        ),
        "giu",
    )
}

const YEAR_DASHES: &str = "—−–";

static YEAR_RANGE_RE: LazyLock<Re> =
    LazyLock::new(|| year_range_pattern(&format!("[-{YEAR_DASHES}]"), &format!("[ {NBSP}]?")));

const SOURCE_GAP: &str = r"[\t\p{Zs}]";

pub static SOURCE_YEAR_RANGE_RE: LazyLock<Re> = LazyLock::new(|| {
    year_range_pattern(
        &format!("{SOURCE_GAP}*(?:-{{1,2}}|[{YEAR_DASHES}]){SOURCE_GAP}*"),
        &format!("(?<suffixGap>{SOURCE_GAP}*)"),
    )
});

pub const NUMERIC_DATE_SOURCE: &str =
    r"(\s|\u{00A0})([0-9]{2}\.[0-9]{2}\.([0-9]{2})?[0-9]{2})(?=\s|\u{00A0}|\.(?:\s|\u{00A0}|$)|$)";

static UNMARKED_DATE_RE: LazyLock<Re> =
    LazyLock::new(|| Re::new(&format!("{NUMERIC_DATE_SOURCE}(?!{})", *YEAR_MARK), "giu"));

pub static YEAR_MARKS: Requires = Requires { all: F::DIGIT | F::PERIOD | F::CYRILLIC, any: 0, pattern: None };

pub fn settle_year_marks(text: &[u16]) -> JsString {
    DATE_YEAR_RE.replace_tpl(text, &cat!("$1", NBSP, "г."))
}

fn wrap_date(m: &Caps<'_>, ctx: &mut Ctx<'_>, body: &[u16], lead: &[u16]) -> Result<JsString> {
    let start = m.index() + lead.len();
    let content = nowrap(ctx, body, m.input, start, m.end())?;
    Ok(cat!(lead, content))
}

const MONTHS: &str = "января|февраля|марта|апреля|мая|июня|июля|августа|сентября|октября|ноября|декабря";
const MONTH_FORM: &str = "(январ|феврал|сентябр|октябр|ноябр|декабр)([ьяюе]|[её]м)|(апрел|июн|июл)([ьяюе]|ем)|(март|август)([ауе]|ом)?|ма[йяюе]|маем";

pub fn date_group() -> RuleGroupDef {
    super::group(
        "Даты и дни",
        super::classes(super::NOWRAP_CLASSES),
        Vec::from([
            Rule::pattern(
                "nbsp_century",
                Re::new(
                    &format!(
                        r"{}([1-9]\d?|[IVX]{{1,8}})[ \t{NBSP}]+(век(?:а|е|у|ом)?|в\.){}",
                        *CONTENT_START_GROUP, *CONTENT_END_LOOKAHEAD
                    ),
                    "gu",
                ),
                Replacement::func(|m, _| {
                    let value = crate::js::lossy(m.s(2));
                    let roman = ["", "X", "XX", "XXX"].iter().any(|tens| {
                        ["", "I", "II", "III", "IV", "V", "VI", "VII", "VIII", "IX"]
                            .iter()
                            .any(|ones| value == format!("{tens}{ones}"))
                    });
                    Ok(if roman || value.parse::<u32>().is_ok_and(|n| n > 0) {
                        cat!(m.s(1), m.s(2), NBSP, m.s(3))
                    } else {
                        m.whole().to_vec()
                    })
                }),
            )
            .describe("Привязка римского или арабского номера века к обозначению")
            .requires(F::SPACE)
            .requires_any(F::DIGIT | F::LATIN),
            Rule::pattern(
                "year_range",
                YEAR_RANGE_RE.clone(),
                Replacement::func(|m, _| {
                    let (first, last) = (m.s(2), m.s(4));
                    let separator = if first < last { &[unit(NDASH)][..] } else { m.s(3) };
                    let suffix = if m.has(5) { cat!(NBSP, "гг.") } else { Vec::new() };
                    Ok(cat!(m.s(1), first, separator, last, suffix))
                }),
            )
            .describe("Тире и неразрывные пробелы в периодах дат")
            .requires(F::DIGIT | F::DASH | F::SPACE)
            .requires_pattern(Re::new(r"\d{4}[-—−–]\d{4}", "u"))
            .project_edits(),
            Rule::pattern(
                "month_range",
                Re::new(&format!("(?<![а-яёa-z])({MONTH_FORM})-(?=({MONTH_FORM})(?![а-яёa-z]))"), "giu"),
                Replacement::Template(cat!("$1", NDASH)),
            )
            .describe("Среднее тире между периодами месяцев")
            .requires(F::DASH)
            .requires_pattern(Re::new("-", "u")),
            Rule::pattern(
                "day_month_range",
                Re::new(&format!(r"(\d{{1,2}}\s(?:{MONTHS}))-(\d{{1,2}}\s(?:{MONTHS}))"), "giu"),
                Replacement::Template(cat!("$1", NDASH, "$2")),
            )
            .describe("Среднее тире между полными датами «число месяц»")
            .requires(F::SPACE | F::DIGIT | F::DASH)
            .requires_pattern(Re::new("-", "u")),
            Rule::pattern(
                "nowrap_day_range",
                Re::new(
                    &format!(
                        r"(^|[^\d])(?<!{})(\d+)(?:[-{MINUS}{MDASH}{NDASH}]|{})(\d+)(?: |{NBSP}|{})({MONTHS}){}",
                        *GROUPED_NUMBER_PREFIX,
                        kept_reference_to(&['-', MINUS, MDASH, NDASH]),
                        kept_reference_to(&[NBSP]),
                        written_content_end_lookahead(kept_reference_to)
                    ),
                    "giu",
                ),
                Replacement::func(|m, ctx| wrap_date(m, ctx, &cat!(m.s(2), MDASH, m.s(3), " ", m.s(4)), m.s(1))),
            )
            .describe("Тире и неразрывные периоды дней")
            .off()
            .requires(F::DIGIT | F::CYRILLIC),
            Rule::patterns(
                "nowrap_date",
                vec![
                    (DATE_YEAR_RE.clone(), Replacement::func(|m, ctx| wrap_date(m, ctx, &cat!(m.s(1), " г."), &[]))),
                    (UNMARKED_DATE_RE.clone(), Replacement::func(|m, ctx| wrap_date(m, ctx, m.s(2), m.s(1)))),
                ],
            )
            .describe("Привязка года к дате")
            .requires(F::DIGIT | F::PERIOD)
            .requires_pattern(Re::new(r"[0-9]{2}\.[0-9]{2}\.", "u")),
            Rule::pattern(
                "nbsp_year_suffix",
                Re::new(r#"(^| |\u{00A0}|"|«)([0-9]{3,4})[ ]?(г\.)(?=[^a-zа-яё]|$)"#, "giu"),
                Replacement::Template(cat!("$1$2", NBSP, "$3")),
            )
            .describe("Пробел между годом и сокращением г.")
            .requires(F::DIGIT | F::PERIOD),
        ]),
    )
}
