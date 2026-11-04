use std::sync::LazyLock;

use crate::cat;
use crate::js::{self, JsString, Re};
use crate::rule_runner::{Replacement, Rule, RuleGroupDef};
use crate::shared::glyphs::{HELLIP, LDQUO, RAQUO, unit};

static WORD_RE: LazyLock<Re> = LazyLock::new(|| Re::new("[a-zа-яё0-9]", "iu"));
static REFERENCE_TAIL_RE: LazyLock<Re> = LazyLock::new(|| Re::new("&[0-9A-Za-z#]+$", ""));
const REFERENCE_MAX: isize = 32;

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

fn ends_character_reference(text: &[u16], semicolon_at: isize) -> bool {
    let from = (semicolon_at - REFERENCE_MAX).max(0) as usize;
    REFERENCE_TAIL_RE.test(js::slice(text, from, semicolon_at as usize))
}

fn trim_bracket_punct(text: &[u16]) -> JsString {
    let Some(mut close) = js::index_of_unit(text, 0x29, 0) else { return text.to_vec() };
    let mut cuts: Vec<(usize, usize)> = Vec::new();
    loop {
        let mut mark_at = close as isize - 1;
        while mark_at >= 0 && is_bracket_space(at(text, mark_at)) {
            mark_at -= 1;
        }
        let mark = at(text, mark_at);
        if mark == Some(0x2C) || (mark == Some(0x3B) && !ends_character_reference(text, mark_at)) {
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
        return text.to_vec();
    }
    let mut out = Vec::with_capacity(text.len());
    let mut start = 0;
    for (from, to) in cuts {
        out.extend_from_slice(js::slice(text, start, from));
        start = to;
    }
    out.extend_from_slice(js::from(text, start));
    out
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
            .off(),
            Rule::pattern(
                "repeat_limit",
                Re::new(r"([!?])\1+", "g"),
                Replacement::func(|m, _| {
                    Ok(if m.whole().len() >= 3 { js::repeat(m.s(1), 3) } else { m.s(1).to_vec() })
                }),
            )
            .describe("Повтор !/?: допустимы только 1 или 3 (!!→!, !!!→!!!, !!!!→!!!)"),
            Rule::patterns(
                "collapse_repeated",
                vec![
                    (Re::new("(,){2,}", "g"), Replacement::tpl("$1")),
                    (Re::new("(:){3,}", "g"), Replacement::tpl("$1")),
                    (Re::new("(;){2,}", "g"), Replacement::tpl("$1")),
                    (Re::new("((,|:);){2,}", "g"), Replacement::tpl("$1")),
                    (Re::new(r",\s+,", "g"), Replacement::tpl(",")),
                ],
            )
            .describe("Лишние запятые, двоеточия, точки с запятой")
            .cycled(),
            Rule::pattern("ellipsis", Re::new(r"\.{3,}", "g"), Replacement::Template(cat!(HELLIP)))
                .describe("Три и более точек → многоточие"),
            Rule::pattern("mark_ellipsis", Re::new(&format!("([?!]){HELLIP}"), "gu"), Replacement::tpl("$1.."))
                .describe("Многоточие после ? или ! теряет точку: ?… → ?.."),
            Rule::pattern(
                "swap_exclamation_question",
                Re::new(r"([a-zа-яё0-9])(?: |\t|\u{00A0})?!\?(\s|$|<)", "giu"),
                Replacement::tpl("$1?!$2"),
            )
            .describe("Перестановка !? → ?!"),
            Rule::patterns(
                "drop_terminal_garbage",
                vec![
                    (Re::new(r"([!?])[,;]+[!?]+", "g"), Replacement::tpl("$1")),
                    (Re::new(&format!(r"\.{RAQUO}\."), "g"), Replacement::Template(cat!(RAQUO, "."))),
                ],
            )
            .describe("Удаление мусорных хвостов пунктуации после терминального знака"),
            Rule::patterns(
                "collapse_doubled",
                vec![
                    (
                        Re::new(&format!(r"([a-zа-яё0-9]|\s|^)\.\.(\s|$|<|[),\]}}{RAQUO}{LDQUO}])"), "giu"),
                        Replacement::tpl("$1.$2"),
                    ),
                    (Re::new(r"([a-zа-яё0-9])(!|\.)(!|\.|\?)(\s|$|<)", "giu"), Replacement::tpl("$1$2$4")),
                    (Re::new(r"([a-zа-яё0-9])(\?)(\?)(\s|$|<)", "giu"), Replacement::tpl("$1$2$4")),
                ],
            )
            .describe("Сдвоенные знаки препинания → одиночные"),
            Rule::handler("trim_bracket_terminal_punct", |ctx| Ok(Some(trim_bracket_punct(&ctx.text))))
                .describe("Удаление случайной пунктуации перед закрывающей скобкой"),
            Rule::patterns(
                "bracket_spaces",
                vec![
                    (Re::new(r"(?<![:;=])(\()( |\t)+", "g"), Replacement::tpl("$1")),
                    (Re::new(r"(?<![ \t])( |\t)+(\))", "g"), Replacement::tpl("$2")),
                ],
            )
            .describe("Лишние пробелы внутри скобок"),
            Rule::pattern("space_before_bracket", Re::new(r"([a-zа-яё])(\()", "giu"), Replacement::tpl("$1 $2"))
                .describe("Пробел перед открывающей скобочкой"),
            Rule::pattern("period_at_end", Re::new(r"([a-zа-яё0-9])( |\t|\u{00A0})*$", "giu"), Replacement::tpl("$1."))
                .describe("Точка в конце текста, если её там нет")
                .off(),
        ],
    )
}
