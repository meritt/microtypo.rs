use std::sync::LazyLock;

use crate::cat;
use crate::js::{Caps, JsString, Re};
use crate::protect::placeholders::OPEN_UNIT;
use crate::rule_runner::{Ctx, Replacement, Rule, RuleGroupDef};
use crate::shared::glyphs::{COPY, DEG, LARR, NBSP, RARR, REG, RSQUO, TRADE, unit};

static NEXT_COPYRIGHT_RE: LazyLock<Re> = LazyLock::new(|| Re::new(r"^\((?:c|с)\)", "iu"));

fn has_next_copyright_token(source: &[u16], index: usize) -> bool {
    NEXT_COPYRIGHT_RE.test(crate::js::slice(source, index, index + 3))
}

fn copyright_space(m: &Caps<'_>, _: &mut Ctx<'_>) -> crate::Result<JsString> {
    let after = m.end();
    let next = m.input.get(after).copied();
    let spaced = next == Some(OPEN_UNIT)
        || next == Some(0x3C)
        || [COPY, REG, TRADE].iter().any(|g| next == Some(unit(*g)))
        || has_next_copyright_token(m.input, after);
    Ok(cat!(COPY, if spaced { ' ' } else { NBSP }))
}

pub fn symbol_group() -> RuleGroupDef {
    super::group(
        "Специальные символы",
        super::classes(super::NOWRAP_CLASSES),
        vec![
            Rule::pattern("trademark", Re::new(r"\(tm\)", "gi"), Replacement::Template(cat!(TRADE)))
                .describe("(tm) → ™"),
            Rule::pattern("registered", Re::new(r"\(r\)", "gi"), Replacement::Template(cat!(REG))).describe("(R) → ®"),
            Rule::patterns(
                "copyright",
                vec![
                    (Re::new(r"\((?:c|с)\)(?=\s|[a-zа-яёA-ZА-ЯЁ0-9.,!?]|$)", "giu"), Replacement::Template(cat!(COPY))),
                    (Re::new(&format!(r"{COPY}[ \t]+"), "gu"), Replacement::func(copyright_space)),
                ],
            )
            .describe("(c) → ©"),
            Rule::pattern(
                "apostrophe",
                Re::new(r"(\s|^|>|’)([a-zа-яё]{1,})'{1,2}([a-zа-яё]+)", "giu"),
                Replacement::Template(cat!("$1$2", RSQUO, "$3")),
            )
            .describe("Правильный апостроф")
            .cycled(),
            Rule::pattern(
                "fahrenheit",
                Re::new(r"(?<!\d)([0-9]+)F($|\s|\.|,|;|:|\u{00A0}|\?|!)", "gu"),
                Replacement::func(|m, ctx| {
                    let tagged = super::nowrap(ctx, &cat!(m.s(1), " ", DEG, "F"))?;
                    Ok(cat!(tagged, m.s(2)))
                }),
            )
            .describe("Градусы по Фаренгейту"),
            Rule::patterns(
                "arrows",
                vec![
                    (Re::new("([^-]|^)->", "g"), Replacement::Template(cat!("$1", RARR))),
                    (Re::new("<-(?!-)", "g"), Replacement::Template(cat!(LARR))),
                ],
            )
            .describe("Преобразование -> в → и <- в ←"),
        ],
    )
}
