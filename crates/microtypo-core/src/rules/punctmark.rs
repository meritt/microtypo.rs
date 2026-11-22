use std::sync::LazyLock;

use crate::cat;
use crate::errors::Result;
use crate::js::{self, JsString, Re};
use crate::protect::placeholders::INLINE_END;
use crate::rule_runner::{Replacement, Rule, RuleGroupDef, SpacedYears};
use crate::shared::abbreviation_grammar::abbreviation_period;
use crate::shared::boundaries::NOT_AFTER_LONE_QUOTE;
use crate::shared::character_reference::ends_character_reference;
use crate::shared::glyphs::{HELLIP, LDQUO, RAQUO, unit};

use crate::shared::rule_features::F;

static ABBREVIATION_PERIOD_RE: LazyLock<Re> = LazyLock::new(|| abbreviation_period(&INLINE_END));

fn ends_abbreviation(source: &[u16], end: usize) -> bool {
    ABBREVIATION_PERIOD_RE.exec_at(source, end).is_some()
}

static CYRILLIC_RE: LazyLock<Re> = LazyLock::new(|| Re::new("[а-яё]", "iu"));
static WORD_RE: LazyLock<Re> = LazyLock::new(|| Re::new("[a-zа-яё0-9]", "iu"));

fn is_bracket_space(c: Option<u16>) -> bool {
    matches!(c, Some(0x20 | 0x09 | 0xA0))
}

fn is_word(c: Option<u16>) -> bool {
    c.is_some_and(|c| WORD_RE.test(&[c]))
}

pub fn collapses_as_repeat(first: u16, second: Option<u16>) -> bool {
    let Some(second) = second else { return false };
    let swallowed: &[u16] = match first {
        0x2E => &[0x2E, 0x21, 0x3F],
        0x21 => &[0x2E, 0x21, 0x3F, unit(HELLIP)],
        0x3F => &[0x3F, unit(HELLIP)],
        0x2C => &[0x2C],
        0x3B => &[0x3B],
        _ => &[],
    };
    swallowed.contains(&second)
}

fn at(text: &[u16], i: isize) -> Option<u16> {
    if i < 0 { None } else { text.get(i as usize).copied() }
}

fn trim_bracket_punct(text: &[u16], check_budget: &dyn Fn(&str) -> Result<()>) -> Result<JsString> {
    let Some(mut close) = js::index_of_unit(text, 0x29, 0) else { return Ok(text.to_vec()) };
    let mut cuts: Vec<(usize, usize)> = Vec::new();
    loop {
        let mut mark_at = close as isize - 1;
        while mark_at >= 0 && is_bracket_space(at(text, mark_at)) {
            mark_at -= 1;
        }
        let mark = at(text, mark_at);
        if mark == Some(0x2C)
            || (mark == Some(0x3B) && !ends_character_reference(text, mark_at as usize, Some(check_budget))?)
        {
            let word_at = mark_at - 1;
            if word_at >= 0 && is_word(at(text, word_at)) {
                cuts.push((mark_at as usize, close));
            }
        } else if mark == Some(0x3A) {
            let mut word_at = mark_at - 1;
            while word_at >= 0 && is_bracket_space(at(text, word_at)) {
                word_at -= 1;
            }
            if word_at >= 0 && is_word(at(text, word_at)) && has_leading_ellipsis(text, word_at) {
                cuts.push(((word_at + 1) as usize, close));
            }
        }
        match js::index_of_unit(text, 0x29, close + 1) {
            Some(next) => close = next,
            None => break,
        }
    }
    if cuts.is_empty() {
        return Ok(text.to_vec());
    }
    let mut out = Vec::with_capacity(text.len());
    let mut start = 0;
    for (from, to) in cuts {
        out.extend_from_slice(js::slice(text, start, from));
        start = to;
    }
    out.extend_from_slice(js::from(text, start));
    Ok(out)
}

fn has_leading_ellipsis(text: &[u16], from: isize) -> bool {
    let limit = (from - 160).max(0);
    let mut open = from;
    while open >= limit && at(text, open) != Some(0x28) && at(text, open) != Some(0x29) {
        open -= 1;
    }
    if at(text, open) != Some(0x28) {
        return false;
    }
    open += 1;
    while (open as usize) < text.len() && is_bracket_space(at(text, open)) {
        open += 1;
    }
    at(text, open) == Some(unit(HELLIP))
}

pub fn punctmark_group() -> RuleGroupDef {
    LazyLock::force(&ABBREVIATION_PERIOD_RE);
    super::group(
        "Пунктуация и знаки препинания",
        None,
        vec![
            Rule::pattern(
                "comma_before_conjunction",
                Re::new(r"([a-zа-яё])(\s|\u{00A0})(но|а)(\s|\u{00A0})", "gu"),
                Replacement::tpl("$1,$2$3$4"),
            )
            .describe("Расстановка запятых перед а, но")
            .off()
            .requires(F::SPACE),
            Rule::pattern(
                "repeat_limit",
                Re::new(r"([!?])\1+", "g"),
                Replacement::func(|m, _| {
                    Ok(if m.whole().len() >= 3 { js::repeat(m.s(1), 3) } else { m.s(1).to_vec() })
                }),
            )
            .describe("Повтор !/?: допустимы только 1 или 3 (!!→!, !!!→!!!, !!!!→!!!)")
            .requires_any(F::EXCLAMATION | F::QUESTION),
            Rule::patterns(
                "collapse_repeated",
                vec![
                    (Re::new("(,){2,}", "g"), Replacement::tpl("$1")),
                    (Re::new("(:){3,}", "g"), Replacement::tpl("$1")),
                    (
                        Re::new("(;){2,}", "g"),
                        Replacement::func(|m, ctx| {
                            Ok(js::u(if ends_character_reference(m.input, m.index(), Some(ctx.check_budget))? {
                                ";;"
                            } else {
                                ";"
                            }))
                        }),
                    ),
                    (Re::new("((,|:);){2,}", "g"), Replacement::tpl("$1")),
                    (Re::new(r",\s+,", "g"), Replacement::tpl(",")),
                ],
            )
            .describe("Лишние запятые, двоеточия, точки с запятой")
            .cycled()
            .requires_any(F::COMMA | F::COLON | F::SEMICOLON),
            Rule::pattern("ellipsis", Re::new(r"\.{3,}", "g"), Replacement::Template(cat!(HELLIP)))
                .describe("Три и более точек → многоточие")
                .requires(F::PERIOD),
            Rule::pattern("mark_ellipsis", Re::new(&format!(r"([?!]){HELLIP}\.*"), "gu"), Replacement::tpl("$1.."))
                .describe("Многоточие после ? или ! теряет точку: ?… → ?..")
                .requires(F::ELLIPSIS),
            Rule::pattern(
                "swap_exclamation_question",
                Re::new(r"([a-zа-яё0-9])(?: |\t|\u{00A0})?!\?(\s|$|<)", "giu"),
                Replacement::tpl("$1?!$2"),
            )
            .describe("Перестановка !? → ?!")
            .requires(F::EXCLAMATION | F::QUESTION),
            Rule::patterns(
                "drop_terminal_garbage",
                vec![
                    (Re::new(r"([!?])(?:[,;]+[!?]+)+", "g"), Replacement::tpl("$1")),
                    (Re::new(&format!(r"\.{RAQUO}\."), "g"), Replacement::Template(cat!(RAQUO, "."))),
                ],
            )
            .describe("Удаление мусорных хвостов пунктуации после терминального знака")
            .requires_any(F::PERIOD | F::EXCLAMATION | F::QUESTION)
            .project_spaced_years(SpacedYears::Ranges),
            Rule::patterns(
                "collapse_doubled",
                vec![
                    (
                        Re::new(&format!(r"([a-zа-яё0-9]|\s|^)\.\.(\s|$|<|[),\]}}{RAQUO}{LDQUO}])"), "giu"),
                        Replacement::tpl("$1.$2"),
                    ),
                    (
                        Re::new(r"([a-zа-яё0-9])(!|\.)(!|\.|\?)(\s|$|<)", "giu"),
                        Replacement::func(|m, _| {
                            let (first, second) = (m.s(2), m.s(3));
                            Ok(if first == [0x2E] && second != [0x2E] && ends_abbreviation(m.input, m.index() + 2) {
                                m.whole().to_vec()
                            } else {
                                cat!(m.s(1), first, m.s(4))
                            })
                        }),
                    ),
                    (Re::new(r"([a-zа-яё0-9])(\?)(\?)(\s|$|<)", "giu"), Replacement::tpl("$1$2$4")),
                ],
            )
            .describe("Сдвоенные знаки препинания → одиночные")
            .requires_any(F::PERIOD | F::EXCLAMATION | F::QUESTION),
            Rule::handler("trim_bracket_terminal_punct", |ctx| {
                Ok(Some(trim_bracket_punct(&ctx.text, ctx.check_budget)?))
            })
            .describe("Удаление случайной пунктуации перед закрывающей скобкой")
            .requires(F::PAREN),
            Rule::patterns(
                "bracket_spaces",
                vec![
                    (Re::new(r"(?<![:;=])(\()( |\t)+", "g"), Replacement::tpl("$1")),
                    (Re::new(&format!(r"(?<![ \t]){}( |\t)+(\))", *NOT_AFTER_LONE_QUOTE), "g"), Replacement::tpl("$2")),
                ],
            )
            .describe("Лишние пробелы внутри скобок")
            .requires(F::SPACE | F::PAREN),
            Rule::pattern(
                "space_before_bracket",
                Re::new(r"([a-zа-яё])\((?!\)|[cс]\))", "giu"),
                Replacement::func(|m, _| {
                    let mut start = m.index();
                    while start > 0 && is_word(m.input.get(start - 1).copied()) {
                        start -= 1;
                    }
                    Ok(if CYRILLIC_RE.test(&m.input[start..=m.index()]) {
                        cat!(m.s(1), " (")
                    } else {
                        m.whole().to_vec()
                    })
                }),
            )
            .describe("Пробел перед открывающей скобочкой")
            .requires(F::PAREN),
            Rule::pattern("period_at_end", Re::new(r"([a-zа-яё0-9])( |\t|\u{00A0})*$", "giu"), Replacement::tpl("$1."))
                .describe("Точка в конце текста, если её там нет")
                .off(),
        ],
    )
}
