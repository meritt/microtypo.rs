use crate::cat;
use crate::js::{self, Re};
use crate::rule_runner::{Replacement, Rule, RuleGroupDef};
use crate::shared::glyphs::{MDASH, NBSP, NDASH};

const MONTHS: &str = "января|февраля|марта|апреля|мая|июня|июля|августа|сентября|октября|ноября|декабря";
const MONTH_FORM: &str = "(январ|феврал|сентябр|октябр|ноябр|декабр)([ьяюе]|[её]м)|(апрел|июн|июл)([ьяюе]|ем)|(март|август)([ауе]|ом)?|ма[йяюе]|маем";

pub fn date_group() -> RuleGroupDef {
    super::group(
        "Даты и дни",
        super::classes(super::NOWRAP_CLASSES),
        vec![
            Rule::pattern(
                "year_range",
                Re::new(
                    r"(с|по|период|середины|начала|начало|конца|конец|половины|в|между|\([cс]\)|©)(\s+)(\d{4})([-—−–])(\d{4})(( |\u{00A0})?(г\.г\.|гг\.|гг|г\.|г)(?![а-яёa-z]))?",
                    "giu",
                ),
                Replacement::func(|m, _| {
                    let (p3, p5) = (m.s(3), m.s(5));
                    let range = if js::parse_int(p3) >= js::parse_int(p5) {
                        cat!(p3, m.s(4), p5)
                    } else {
                        cat!(p3, NDASH, p5)
                    };
                    let suffix = if m.s(6).is_empty() { Vec::new() } else { cat!(NBSP, "гг.") };
                    Ok(cat!(m.s(1), m.s(2), range, suffix))
                }),
            )
            .describe("Тире и неразрывные пробелы в периодах дат"),
            Rule::pattern(
                "month_range",
                Re::new(&format!("(?<![а-яёa-z])({MONTH_FORM})-({MONTH_FORM})(?![а-яёa-z])"), "giu"),
                Replacement::Template(cat!("$1", NDASH, "$8")),
            )
            .describe("Среднее тире между периодами месяцев"),
            Rule::pattern(
                "day_month_range",
                Re::new(&format!(r"(\d{{1,2}}\s(?:{MONTHS}))-(\d{{1,2}}\s(?:{MONTHS}))"), "giu"),
                Replacement::Template(cat!("$1", NDASH, "$2")),
            )
            .describe("Среднее тире между полными датами «число месяц»"),
            Rule::pattern(
                "nowrap_day_range",
                Re::new(&format!(r"([^>]|^)(\d+)([-−—–])(\d+)( |\u{{00A0}})({MONTHS})([^<]|$)"), "giu"),
                Replacement::func(|m, ctx| {
                    let tagged = super::nowrap(ctx, &cat!(m.s(2), MDASH, m.s(4), " ", m.s(6)))?;
                    Ok(cat!(m.s(1), tagged, m.s(7)))
                }),
            )
            .describe("Тире и неразрывные периоды дней")
            .off(),
            Rule::patterns(
                "nowrap_date",
                vec![
                    (
                        Re::new(r"(\s|\u{00A0})([0-9]{2}\.[0-9]{2}\.([0-9]{2})?[0-9]{2})(\s|\u{00A0})?г(\.|\s|\u{00A0})", "giu"),
                        Replacement::func(|m, ctx| {
                            let tagged = super::nowrap(ctx, &cat!(m.s(2), " г."))?;
                            Ok(cat!(m.s(1), tagged, if m.s(5) == [0x2E] { "" } else { " " }))
                        }),
                    ),
                    (
                        Re::new(r"(\s|\u{00A0})([0-9]{2}\.[0-9]{2}\.([0-9]{2})?[0-9]{2})(\s|\u{00A0}|\.(\s|\u{00A0}|$)|$)", "giu"),
                        Replacement::func(|m, ctx| {
                            let tagged = super::nowrap(ctx, m.s(2))?;
                            Ok(cat!(m.s(1), tagged, m.s(4)))
                        }),
                    ),
                ],
            )
            .describe("Привязка года к дате"),
            Rule::pattern(
                "nbsp_year_suffix",
                Re::new(r#"(^| |\u{00A0}|"|«)([0-9]{3,4})[ ]?(г\.)([^a-zа-яё]|$)"#, "giu"),
                Replacement::Template(cat!("$1$2", NBSP, "$3$4")),
            )
            .describe("Пробел между годом и сокращением г."),
        ],
    )
}
