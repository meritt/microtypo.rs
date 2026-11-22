use std::sync::LazyLock;

use crate::cat;
use crate::js::{Caps, JsString, Re};
use crate::protect::placeholders::{CLOSE, OPEN, OPEN_UNIT, TRANSPARENT_TAG_KINDS};
use crate::rule_runner::{Ctx, Replacement, Rule, RuleGroupDef};
use crate::shared::glyphs::{BDQUO, COPY, DEG, LAQUO, LARR, NBSP, PLUSMN, RARR, REG, RSQUO, TRADE, unit};
use crate::shared::nowrap_context::{held_before_nowrap_end, nowrap};
use crate::shared::rule_features::F;

static EXTENDED_ARROW_LEAD: LazyLock<String> = LazyLock::new(|| format!(r">\s({LAQUO}{BDQUO}"));
static EXTENDED_ARROW_LEAD_RE: LazyLock<Re> = LazyLock::new(|| Re::new(&format!("[{}]", *EXTENDED_ARROW_LEAD), "u"));
static PLUS_MINUS_LEAD: LazyLock<String> = LazyLock::new(|| format!(r">\s({LAQUO}{BDQUO}:;"));
static PLUS_MINUS_LEAD_RE: LazyLock<Re> = LazyLock::new(|| Re::new(&format!("[{}]", *PLUS_MINUS_LEAD), "u"));

fn led_by_wrapper_end(m: &Caps<'_>, ctx: &mut Ctx<'_>, lead: &Re) -> crate::Result<bool> {
    let held = match m.index().checked_sub(1) {
        Some(gt) => held_before_nowrap_end(ctx, m.input, gt)?,
        None => None,
    };
    Ok(held.is_some_and(|held| !lead.test(&[held])))
}

fn extended_arrow(m: &Caps<'_>, ctx: &mut Ctx<'_>) -> crate::Result<JsString> {
    if led_by_wrapper_end(m, ctx, &EXTENDED_ARROW_LEAD_RE)? {
        return Ok(m.whole().to_vec());
    }
    Ok(crate::js::u(match crate::js::lossy(m.s(1)).as_str() {
        "<->" => "↔",
        "=>" => "⇒",
        _ => "⇔",
    }))
}

fn plus_minus(m: &Caps<'_>, ctx: &mut Ctx<'_>) -> crate::Result<JsString> {
    Ok(if led_by_wrapper_end(m, ctx, &PLUS_MINUS_LEAD_RE)? { m.whole().to_vec() } else { cat!(PLUSMN) })
}

static NEXT_COPYRIGHT_RE: LazyLock<Re> = LazyLock::new(|| Re::new(r"^\((?:c|с)\)", "iu"));
static MARK_AFTER_QUOTE_RE: LazyLock<Re> = LazyLock::new(|| Re::new("[.,;:!?…)]", "u"));

fn has_next_copyright_token(source: &[u16], index: usize) -> bool {
    NEXT_COPYRIGHT_RE.test(crate::js::slice(source, index, index + 3))
}

fn inline_markup() -> String {
    format!(r"(?:<\/?{OPEN}[{TRANSPARENT_TAG_KINDS}]\d{{1,9}}{CLOSE}>|[*_~])")
}

fn mark_gap() -> String {
    let markup = inline_markup();
    format!(r"{markup}{{0,4}}(?:[ \t{NBSP}]{{1,8}}{markup}{{0,4}})?")
}

static REGISTERED_RE: LazyLock<Re> = LazyLock::new(|| {
    let gap = mark_gap();
    Re::new(&format!(r"(?<=(?<!\p{{L}})(?=\p{{L}}{{0,39}}\p{{Lu}})\p{{L}}{{2,40}}{gap})\([rR]\)"), "gu")
});
static COPYRIGHT_SCAN_RE: LazyLock<Re> = LazyLock::new(|| {
    let markup = inline_markup();
    let gap = mark_gap();
    let mark = r"\([cсCС]\)";
    Re::new(
        &format!(
            r"(\n[ \t]*\n|<\/{OPEN}L\d+{CLOSE}>)|(\({markup}{{0,4}}[abаб]\))|(?<=(?<![\p{{L}}\d])[Cc][Oo][Pp][Yy][Rr][Ii][Gg][Hh][Tt]{gap}){mark}|(?<![\p{{L}}\d]{markup}{{0,4}}){mark}(?={gap}(?:\d{{4}}(?!\d)|\p{{Lu}}))"
        ),
        "gu",
    )
});
static COPYRIGHT_GAP_RE: LazyLock<Re> = LazyLock::new(|| Re::new(&format!(r"{COPY}[ \t]+"), "gu"));

fn copyright(ctx: &mut Ctx<'_>) -> crate::Result<Option<JsString>> {
    let mut enumeration = false;
    let marked = COPYRIGHT_SCAN_RE.replace_matches(&ctx.text, |m| {
        if m.has(1) {
            enumeration = false;
            return m.whole().to_vec();
        }
        if m.has(2) {
            enumeration = true;
            return m.whole().to_vec();
        }
        if enumeration { m.whole().to_vec() } else { cat!(COPY) }
    });
    let spaced = COPYRIGHT_GAP_RE.replace_matches(marked.as_deref().unwrap_or(&ctx.text), |m| {
        let after = m.end();
        let next = m.input.get(after).copied();
        let spaced = next == Some(OPEN_UNIT)
            || next == Some(0x3C)
            || (next == Some(0x22) && m.input.get(after + 1).is_some_and(|&c| MARK_AFTER_QUOTE_RE.test(&[c])))
            || [COPY, REG, TRADE].iter().any(|g| next == Some(unit(*g)))
            || has_next_copyright_token(m.input, after);
        cat!(COPY, if spaced { ' ' } else { NBSP })
    });
    Ok(spaced.or(marked))
}

pub fn symbol_group() -> RuleGroupDef {
    LazyLock::force(&COPYRIGHT_SCAN_RE);
    LazyLock::force(&COPYRIGHT_GAP_RE);
    LazyLock::force(&MARK_AFTER_QUOTE_RE);
    LazyLock::force(&EXTENDED_ARROW_LEAD_RE);
    LazyLock::force(&PLUS_MINUS_LEAD_RE);
    super::group(
        "Специальные символы",
        super::classes(super::NOWRAP_CLASSES),
        vec![
            Rule::patterns(
                "plus_minus_slash",
                vec![(
                    Re::new(&format!(r"(?<=^|[{}])\+\/-(?=$|[\s\d).,;:!?»“]|<\/)", *PLUS_MINUS_LEAD), "gu"),
                    Replacement::func(plus_minus),
                )],
            )
            .describe("Преобразование +/- в ±")
            .requires(F::MATH | F::SLASH | F::DASH),
            Rule::patterns(
                "arrows_extended",
                vec![(
                    Re::new(&format!(r"(?<=^|[{}])(<=>|<->|=>)(?=$|[\s).,;:!?»“]|<\/)", *EXTENDED_ARROW_LEAD), "gu"),
                    Replacement::func(extended_arrow),
                )],
            )
            .describe("Дополнительные стрелки: <-> → ↔, => → ⇒, <=> → ⇔")
            .requires_any(F::MATH | F::DASH),
            Rule::pattern("trademark", Re::new(r"\(tm\)", "gi"), Replacement::Template(cat!(TRADE)))
                .describe("(tm) → ™")
                .requires(F::PAREN | F::LATIN),
            Rule::pattern("registered", REGISTERED_RE.clone(), Replacement::Template(cat!(REG)))
                .describe("(R) → ®")
                .requires(F::PAREN | F::LATIN),
            Rule::handler("copyright", copyright).describe("(c) → ©").requires_any(F::PAREN | F::COPYRIGHT),
            Rule::pattern(
                "apostrophe",
                Re::new(r"(\s|^|>|’)([a-zа-яё]{1,})'{1,2}([a-zа-яё]+)", "giu"),
                Replacement::Template(cat!("$1$2", RSQUO, "$3")),
            )
            .describe("Правильный апостроф")
            .cycled()
            .requires(F::APOSTROPHE),
            Rule::pattern(
                "fahrenheit",
                Re::new(r"(?<!\d)([0-9]+)F($|\s|\.|,|;|:|\u{00A0}|\?|!)", "gu"),
                Replacement::func(|m, ctx| {
                    let end = m.index() + m.s(1).len() + 1;
                    let tagged = nowrap(ctx, &cat!(m.s(1), " ", DEG, "F"), m.input, m.index(), end)?;
                    Ok(cat!(tagged, m.s(2)))
                }),
            )
            .describe("Градусы по Фаренгейту")
            .requires(F::DIGIT | F::LATIN),
            Rule::patterns(
                "arrows",
                vec![
                    (Re::new("([^<-]|^)->", "g"), Replacement::Template(cat!("$1", RARR))),
                    (Re::new("<-(?![->])", "g"), Replacement::Template(cat!(LARR))),
                ],
            )
            .describe("Преобразование -> в → и <- в ←")
            .requires(F::DASH),
        ],
    )
}
