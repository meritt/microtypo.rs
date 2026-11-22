use std::sync::LazyLock;

use crate::cat;
use crate::js::{self, Re};
use crate::protect::placeholders::{CLOSE, OPEN};
use crate::rule_runner::{Replacement, Rule, RuleGroupDef};
use crate::shared::glyphs::{
    CONG, FRAC12, FRAC14, FRAC34, GE, LE, MINUS, NDASH, NE, NNBSP, NUMERO, PLUSMN, SECT, TIMES, unit,
};

static DIGIT_RE: LazyLock<Re> = LazyLock::new(|| Re::new(r"\d", ""));

pub fn number_group() -> RuleGroupDef {
    super::group(
        "Числа, дроби, математические знаки",
        super::classes(super::NOWRAP_CLASSES),
        vec![
            Rule::pattern(
                "time",
                Re::new(r"(?<![\d:])(\d{1,2})\s*:\s*(\d{2})(?:\s*:\s*(\d{2}))?(?![\d:])", "g"),
                Replacement::func(|m, _| {
                    Ok(if let Some(ss) = m.get(3) {
                        cat!(m.s(1), ":", m.s(2), ":", ss)
                    } else {
                        cat!(m.s(1), ":", m.s(2))
                    })
                }),
            )
            .describe("Нормализация времени: 08 : 59 → 08:59, 9:30 не трогаем"),
            Rule::pattern(
                "en_range",
                Re::new(r"(?<![\d—−-])(\d+)[—−-](\d+)(?![\d—−-])", "gu"),
                Replacement::Template(cat!("$1", NDASH, "$2")),
            )
            .describe("Среднее тире для диапазонов чисел: 100-500 → 100–500"),
            Rule::pattern(
                "minus_range",
                Re::new(r"(^|\s)([−-])(\d+)(\.\.\.|…)(\s)?(\+|-|−)?(\d+)", "gu"),
                Replacement::func(|m, _| {
                    let sign = match m.get(6) {
                        Some([0x2D]) => cat!(MINUS),
                        Some(p6) => p6.to_vec(),
                        None => Vec::new(),
                    };
                    Ok(cat!(m.s(1), MINUS, m.s(3), m.s(4), m.s(5), sign, m.s(7)))
                }),
            )
            .describe("Минус между диапазоном чисел"),
            Rule::pattern(
                "times",
                Re::new(r"([^a-zA-Z><]|^)(×)?(?<!\d)(\d+)( *)(x|х|×)( *)(\d+)([^a-zA-Z><]|$)", "gu"),
                Replacement::Template(cat!("$1$2$3", TIMES, "$7$8")),
            )
            .describe("x на × в размерных единицах")
            .cycled(),
            Rule::pattern(
                "sub",
                Re::new(r"([a-zа-яё0-9])_(\d{1,3})([^@а-яёa-z0-9]|$)", "giu"),
                Replacement::func(|m, ctx| {
                    let small = ctx.tag(m.s(2), "small", &[])?;
                    let sub = ctx.tag(&small, "sub", &[])?;
                    Ok(cat!(m.s(1), sub, m.s(3)))
                }),
            )
            .describe("Нижний индекс")
            .html_only(),
            Rule::pattern(
                "sup",
                Re::new(r"([a-zа-яё0-9])\^(\d{1,3})([^а-яёa-z0-9]|$)", "giu"),
                Replacement::func(|m, ctx| {
                    let small = ctx.tag(m.s(2), "small", &[])?;
                    let sup = ctx.tag(&small, "sup", &[])?;
                    Ok(cat!(m.s(1), sup, m.s(3)))
                }),
            )
            .describe("Верхний индекс")
            .html_only(),
            Rule::patterns(
                "fraction",
                vec![
                    (Re::new(r#"(^|\D)1\/2(?![\d"″′])"#, "g"), Replacement::Template(cat!("$1", FRAC12))),
                    (Re::new(r#"(^|\D)1\/4(?![\d"″′])"#, "g"), Replacement::Template(cat!("$1", FRAC14))),
                    (Re::new(r#"(^|\D)3\/4(?![\d"″′])"#, "g"), Replacement::Template(cat!("$1", FRAC34))),
                ],
            )
            .describe("Дроби 1/2, 1/4, 3/4 → символы (кроме дюймов)"),
            Rule::patterns(
                "math",
                vec![
                    (Re::new("!=", "g"), Replacement::Template(cat!(NE))),
                    (Re::new("<=", "g"), Replacement::Template(cat!(LE))),
                    (Re::new(&format!("([^={OPEN}{CLOSE}]|^)>="), "gu"), Replacement::Template(cat!("$1", GE))),
                    (Re::new("~=", "g"), Replacement::Template(cat!(CONG))),
                    (Re::new(r"(?<![-+])\+-(?![-+])", "g"), Replacement::Template(cat!(PLUSMN))),
                ],
            )
            .describe("Математические знаки: !=, <=, >=, ~=, +-"),
            Rule::pattern(
                "thin_space_triads",
                Re::new(r"(?<!\d)(?<!\p{L})(?<!\d )([0-9]{1,3}( [0-9]{3}){1,})(?!\d)(.|$)", "gu"),
                Replacement::func(|m, _| {
                    let p4 = m.s(3);
                    if p4 == [0x2D] {
                        return Ok(m.whole().to_vec());
                    }
                    let next = m.input.get(m.end()).map_or_default(|c| vec![*c]);
                    if p4 == [0x20] && DIGIT_RE.test(&next) {
                        return Ok(m.whole().to_vec());
                    }
                    Ok(cat!(js::replace_all_lit(m.s(1), &[0x20], &[unit(NNBSP)]), p4))
                }),
            )
            .describe("Триады чисел через узкий неразрывный пробел"),
            Rule::pattern(
                "nbsp_numero",
                Re::new(r"[№]\s*(\d)", "gu"),
                Replacement::Template(cat!(NUMERO, NNBSP, "$1")),
            )
            .describe("Узкий неразрывный пробел между № и числом"),
            Rule::pattern(
                "nbsp_section",
                Re::new(r"§\s*(\d+|[IVX]+|[a-zа-яё]+)", "giu"),
                Replacement::Template(cat!(SECT, NNBSP, "$1")),
            )
            .describe("Узкий неразрывный пробел между § и числом"),
        ],
    )
}
