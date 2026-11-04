use crate::cat;
use crate::errors::Result;
use crate::js::{self, Caps, JsString, Re};
use crate::protect::placeholders::CLOSE;
use crate::rule_runner::{Ctx, Replacement, Rule, RuleGroupDef};
use crate::shared::boundaries::{CONTENT_END_LOOKAHEAD, CONTENT_OPEN_BRACKETS, CONTENT_START};
use crate::shared::glyphs::{DEG, MDASH, NBSP, PLUSMN, unit};

use super::nowrap;

fn wrap_phone(ctx: &mut Ctx<'_>, m: &Caps<'_>) -> Result<JsString> {
    let inner = cat!(m.s(2), " ", m.s(4), " ", m.s(6), "-", m.s(8), "-", m.s(10));
    tag_phone(ctx, m.s(1), &inner, m.s(11))
}

fn tag_phone(ctx: &mut Ctx<'_>, left: &[u16], inner: &[u16], right: &[u16]) -> Result<JsString> {
    if left == [0x3E] || right == [0x3C] {
        return Ok(cat!(left, inner, right));
    }
    Ok(cat!(left, nowrap(ctx, inner)?, right))
}

fn wrap_compact_plus_phone(ctx: &mut Ctx<'_>, m: &Caps<'_>) -> Result<JsString> {
    let inner = cat!("+7 ", m.s(2), " ", m.s(3), "-", m.s(4), "-", m.s(5));
    tag_phone(ctx, m.s(1), &inner, m.s(6))
}

fn wrap_compact_local_phone(ctx: &mut Ctx<'_>, m: &Caps<'_>) -> Result<JsString> {
    let inner = cat!("8 ", m.s(3), " ", m.s(4), "-", m.s(5), "-", m.s(6));
    let p7 = m.s(7);
    if p7 == [0x3C] {
        return Ok(cat!(m.s(1), m.s(2), inner, p7));
    }
    Ok(cat!(m.s(1), m.s(2), nowrap(ctx, &inner)?, p7))
}

fn ip_nowrap(ctx: &mut Ctx<'_>, ip: &[u16]) -> Result<JsString> {
    for part in js::split(ip, &[0x2E]) {
        if js::parse_int(part).is_some_and(|n| n > 255) {
            return Ok(ip.to_vec());
        }
    }
    nowrap(ctx, ip)
}

fn degree_unit(u: &[u16]) -> JsString {
    if u == [0x0421] { js::u("C") } else { u.to_vec() }
}

const STOP: &str = r"(\s|^|\.|,|;|:|\?|!)";
const STOP_END: &str = r"(\s|$|\.|,|;|:|\?|!)";

pub fn nobr_group() -> RuleGroupDef {
    let initials = |a: &str, b: &str| Re::new(&format!("{STOP}{a}{b}{STOP_END}"), "gu");
    super::group(
        "Неразрывные конструкции",
        super::classes(super::NOWRAP_CLASSES),
        vec![
            Rule::pattern(
                "nbsp_short_word",
                Re::new(
                    &format!(
                        r"([ \t\n\r]|^|[{}]|>|{CLOSE}|{MDASH}{NBSP})([a-zа-яё]{{1,2}}[ \t]+)([a-zа-яё]{{1,2}}[ \t]+)?([a-zа-яё0-9-]{{2,}}|[0-9])",
                        *CONTENT_OPEN_BRACKETS
                    ),
                    "giu",
                ),
                Replacement::func(|m, _| {
                    let second = match m.get(3) {
                        Some(p4) if !p4.is_empty() => cat!(js::trim(p4), NBSP),
                        _ => Vec::new(),
                    };
                    Ok(cat!(m.s(1), js::trim(m.s(2)), NBSP, second, m.s(4)))
                }),
            )
            .describe("Привязка союзов и предлогов к следующему слову"),
            Rule::pattern(
                "nbsp_final_word",
                Re::new(r"((?<![a-zа-яё0-9-])[a-zа-яё0-9-]{3,}) ([a-zа-яё]{1,2})\.( [A-ZА-ЯЁ]|$)", "gu"),
                Replacement::Template(cat!("$1", NBSP, "$2.$3")),
            )
            .describe("Привязка предлогов к предыдущему слову в конце предложения"),
            Rule::patterns(
                "nowrap_phone",
                vec![
                    (
                        Re::new(r"([^\d+]|^)(\+?[0-9]{1,3})( |\u{00A0}|\u{2009})([0-9]{3,4}|\([0-9]{3,4}\))( |\u{00A0}|\u{2009})([0-9]{2,3})([-−])([0-9]{2})([-−])([0-9]{2})([^\d]|$)", "gu"),
                        Replacement::func(|m, ctx| wrap_phone(ctx, m)),
                    ),
                    (
                        Re::new(r"([^\d+]|^)(\+?[0-9]{1,3})( |\u{00A0}|\u{2009})([0-9]{3,4}|[0-9]{3,4})( |\u{00A0}|\u{2009})([0-9]{2,3})([-−])([0-9]{2})([-−])([0-9]{2})([^\d]|$)", "gu"),
                        Replacement::func(|m, ctx| wrap_phone(ctx, m)),
                    ),
                    (
                        Re::new(r"([^\d+]|^)\+7([0-9]{3})([0-9]{3})([0-9]{2})([0-9]{2})([^\d]|$)", "gu"),
                        Replacement::func(|m, ctx| wrap_compact_plus_phone(ctx, m)),
                    ),
                    (
                        Re::new(r"(^|[^a-zа-яё0-9+])((?:тел|телефон)\.?:?\s*)8([0-9]{3})([0-9]{3})([0-9]{2})([0-9]{2})([^\d]|$)", "giu"),
                        Replacement::func(|m, ctx| wrap_compact_local_phone(ctx, m)),
                    ),
                ],
            )
            .describe("Неразрывные номера телефонов"),
            Rule::pattern(
                "nowrap_phone_alt",
                Re::new(r"([^\d]|^)\+\s?([0-9]{1})\s?\(([0-9]{3,4})\)\s?(\d{3})(\d{2})(\d{2})([^\d]|$)", "gi"),
                Replacement::func(|m, ctx| {
                    let inner = cat!("+", m.s(2), " ", m.s(3), " ", m.s(4), "-", m.s(5), "-", m.s(6));
                    Ok(cat!(m.s(1), nowrap(ctx, &inner)?, m.s(7)))
                }),
            )
            .describe("Доп. формат номеров телефонов"),
            Rule::pattern(
                "nowrap_ip",
                Re::new(r"(\s|\u{00A0}|^)(\d{1,3}\.\d{1,3}\.\d{1,3}\.\d{1,3})", "gu"),
                Replacement::func(|m, ctx| Ok(cat!(m.s(1), ip_nowrap(ctx, m.s(2))?))),
            )
            .describe("Объединение IP-адресов"),
            Rule::patterns(
                "initials_dots",
                vec![
                    (
                        initials(r"([А-ЯЁ])\.?(\s)?([А-ЯЁ])(\s)", "([А-ЯЁ][а-яё]+)"),
                        Replacement::func(|m, ctx| {
                            let inner = cat!(m.s(2), ". ", m.s(4), ". ", m.s(6));
                            Ok(cat!(m.s(1), nowrap(ctx, &inner)?, m.s(7)))
                        }),
                    ),
                    (
                        initials(r"([А-ЯЁ][а-яё]+)(\s)([А-ЯЁ])\.?(\s)?", r"([А-ЯЁ])\.?"),
                        Replacement::func(|m, ctx| {
                            let inner = cat!(m.s(2), " ", m.s(4), ". ", m.s(6), ".");
                            Ok(cat!(m.s(1), nowrap(ctx, &inner)?, m.s(7)))
                        }),
                    ),
                ],
            )
            .describe("Простановка точек к инициалам у фамилии")
            .off(),
            Rule::patterns(
                "nowrap_initials",
                vec![
                    (
                        initials(r"([А-ЯЁ])\.(\s)?([А-ЯЁ])\.(\s)?", "([А-ЯЁ][а-яё]+)"),
                        Replacement::func(|m, ctx| {
                            let inner = cat!(m.s(2), ". ", m.s(4), ". ", m.s(6));
                            Ok(cat!(m.s(1), nowrap(ctx, &inner)?, m.s(7)))
                        }),
                    ),
                    (
                        initials(r"([А-ЯЁ][а-яё]+)(\s)([А-ЯЁ])\.(\s)?", r"([А-ЯЁ])\."),
                        Replacement::func(|m, ctx| {
                            let inner = cat!(m.s(2), " ", m.s(4), ". ", m.s(6), ".");
                            Ok(cat!(m.s(1), nowrap(ctx, &inner)?, m.s(7)))
                        }),
                    ),
                    (
                        initials(r"([А-ЯЁ])(\s)?([А-ЯЁ])(\s)", "([А-ЯЁ][а-яё]+)"),
                        Replacement::func(|m, ctx| {
                            let gap = |i: usize| if m.s(i).is_empty() { "" } else { " " };
                            let inner = cat!(m.s(2), gap(3), m.s(4), gap(5), m.s(6));
                            Ok(cat!(m.s(1), nowrap(ctx, &inner)?, m.s(7)))
                        }),
                    ),
                    (
                        initials(r"([А-ЯЁ][а-яё]+)(\s)([А-ЯЁ])(\s)?", "([А-ЯЁ])"),
                        Replacement::func(|m, ctx| {
                            let gap = if m.s(5).is_empty() { "" } else { " " };
                            let inner = cat!(m.s(2), " ", m.s(4), gap, m.s(6));
                            Ok(cat!(m.s(1), nowrap(ctx, &inner)?, m.s(7)))
                        }),
                    ),
                ],
            )
            .describe("Привязка инициалов к фамилиям"),
            Rule::pattern(
                "nbsp_particle",
                Re::new(r"( |\t)+(ли|бы|б|же|ж)(\u{00A0}|\.|,|:|;|…|\?|\s)", "giu"),
                Replacement::func(|m, _| {
                    let p3 = m.s(3);
                    Ok(cat!(NBSP, m.s(2), if p3 == [unit(NBSP)] { &[0x20u16][..] } else { p3 }))
                }),
            )
            .describe("Неразрывный пробел перед частицей"),
            Rule::pattern(
                "nbsp_after_particle",
                Re::new(r"(\s|^|\u{00A0}|>)([а-яё]+-(?:кась|тка|тко|ка|де))( )([а-яё]+)", "giu"),
                Replacement::Template(cat!("$1$2", NBSP, "$4")),
            )
            .describe("Неразрывный пробел после усилительной частицы: Поди-кась так → Поди-кась так"),
            Rule::pattern("nbsp_namely", Re::new("как то:", "giu"), Replacement::Template(cat!("как", NBSP, "то:")))
                .describe("Неразрывный пробел в \"как то\""),
            Rule::pattern(
                "nbsp_celsius",
                Re::new(&format!(r"(^|[{}+{PLUSMN}])(\d+)( |{NBSP})?{DEG}(C|С|F){}", *CONTENT_START, *CONTENT_END_LOOKAHEAD), "gu"),
                Replacement::func(|m, _| Ok(cat!(m.s(1), m.s(2), NBSP, DEG, degree_unit(m.s(4))))),
            )
            .describe("Привязка градусов к числу (включая ±, +, − перед числом)"),
            Rule::pattern(
                "nowrap_hyphen_short",
                Re::new(
                    r"(\s|>|^)([a-zа-яё]{1}-[a-zа-яё]{4}|[a-zа-яё]{2}-[a-zа-яё]{3}|[a-zа-яё]{3}-[a-zа-яё]{2}|[a-zа-яё]{4}-[a-zа-яё]{1}|когда-то|кое-как|кой-кого|вс[её]-таки|[а-яё]+-(кась|ка|де))(\s|\.|,|!|\?|…|$)",
                    "giu",
                ),
                Replacement::func(|m, ctx| Ok(cat!(m.s(1), nowrap(ctx, m.s(2))?, m.s(4)))),
            )
            .describe("Пятисимвольные слова с дефисом в неразрывные блоки")
            .off()
            .cycled(),
            Rule::pattern(
                "nowrap_hyphen",
                Re::new(r"(\u{00A0}|\s|>|^)([a-zа-яё]+)((-([a-zа-яё]+)){1,2})(\s|\.|,|!|\?|\u{00A0}|…|$)", "giu"),
                Replacement::func(|m, ctx| Ok(cat!(m.s(1), nowrap(ctx, &cat!(m.s(2), m.s(3)))?, m.s(6)))),
            )
            .describe("Отмена переноса слова с дефисом")
            .off()
            .cycled(),
        ],
    )
}
