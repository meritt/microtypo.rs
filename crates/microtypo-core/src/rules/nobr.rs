use std::sync::LazyLock;

use crate::cat;
use crate::errors::Result;
use crate::js::{self, Caps, JsString, Re};
use crate::protect::placeholders::{
    BREAKLINE, CLOSE, INLINE_END, OPEN, PARAGRAPH_CLOSE, PARAGRAPH_OPEN, PARAGRAPH_TAG, vaulted_reference,
};
use crate::rule_runner::{Ctx, Replacement, Rule, RuleGroupDef, SpacedYears};
use crate::shared::abbreviation_grammar::{
    DOTTED_INITIAL_PAIR, DOTTED_INITIALS, SURNAME, follows_complete_name, has_dotted_location_after,
    starts_with_compact_name,
};
use crate::shared::boundaries::{
    BLOCK_CONTENT_START, CONTENT_END_LOOKAHEAD, CONTENT_OPEN_BRACKETS, CONTENT_START, NOT_AFTER_LONE_QUOTE,
};
use crate::shared::glyphs::{
    BDQUO, DEG, FRACTION_GLYPHS, HELLIP, LAQUO, LDQUO, MDASH, MINUS, NBSP, NNBSP, PLUSMN, RAQUO, THINSP, unit,
};
use crate::shared::inline_boundaries::inline_tag_start;
use crate::shared::measurement_units::TEMPERATURE_SCALES;
use crate::shared::nowrap_context::{
    canonical_nowrap_content_start, canonical_nowrap_open, held_before_nowrap_end, nowrap,
};

use super::dash::{EMPHATIC_HEAD, EMPHATIC_PARTICLE};
use super::space::PARTICLE;
use crate::shared::rule_features::F;

static SHORT_WORD_TAIL: LazyLock<String> = LazyLock::new(|| {
    format!(
        r"([a-zа-яё]{{1,2}}[ \t]+|и{NBSP}(?=[дп]р\.))?(?!-+(?![a-zа-яё0-9-]))([a-zа-яё0-9-]{{2,}}|[0-9{}])",
        *FRACTION_GLYPHS
    )
});
const LAST_WORD: &str = r"(?<![\p{L}\d_-])([А-ЯЁа-яё]+|>)[ \t]+([А-ЯЁа-яё]{1,4})";

fn russian_letter(c: Option<u16>) -> bool {
    c.is_some_and(|c| matches!(c, 0x410..=0x44F | 0x401 | 0x451))
}
static NUMBER_WORD_END: LazyLock<String> = LazyLock::new(|| {
    format!("(?:{}|(?={}|{}))", *CONTENT_END_LOOKAHEAD, js::lossy(&PARAGRAPH_CLOSE), js::lossy(&BREAKLINE))
});
static FINAL_PUNCTUATION: LazyLock<String> = LazyLock::new(|| format!(r"[.!?{HELLIP}{RAQUO}{LDQUO})\]}}]*[ \t]*"));

static WORD_START_RE: LazyLock<Re> = LazyLock::new(|| Re::new(r"^[\p{L}\p{N}]", "u"));
static ACRONYM_AHEAD_RE: LazyLock<Re> = LazyLock::new(|| Re::new(r#"^[A-ZА-ЯЁ]{2}(?=[\s;.?!:("»“]|$)"#, "u"));
static SHORT_WORD_AHEAD_RE: LazyLock<Re> =
    LazyLock::new(|| Re::new(&format!(r"^[a-zа-яё]{{1,2}}[ \t]+{}", *SHORT_WORD_TAIL), "iu"));
static SHORT_WORD_LEAD_RE: LazyLock<Re> =
    LazyLock::new(|| Re::new(&format!(r"[ \t\n\r{}>{CLOSE}]", *CONTENT_OPEN_BRACKETS), "u"));
static TEMPERATURE_LEAD_RE: LazyLock<Re> = LazyLock::new(|| Re::new(&format!("[{}+{PLUSMN}]", *CONTENT_START), "u"));
static DIGIT_RE: LazyLock<Re> = LazyLock::new(|| Re::new(r"\d", "u"));
static INITIAL_LEAD_RE: LazyLock<Re> = LazyLock::new(|| Re::new(&format!("[{}]", *CONTENT_START), "u"));
static INITIAL_GAP_CHAR_RE: LazyLock<Re> = LazyLock::new(|| Re::new(&format!(r"[ \t{NBSP}]"), "u"));

fn takes_glue_back(source: &[u16], at: usize) -> bool {
    let tail = js::from(source, at);
    WORD_START_RE.test(tail) && !ACRONYM_AHEAD_RE.test(tail) && !SHORT_WORD_AHEAD_RE.test(tail)
}

fn wrap_phone(ctx: &mut Ctx<'_>, m: &Caps<'_>) -> Result<JsString> {
    let inner = cat!(m.s(2), " ", m.s(4), " ", m.s(6), "-", m.s(8), "-", m.s(10));
    tag_phone(ctx, m.s(1), &inner, m, true)
}

fn digit_behind_tags(source: &[u16], end: usize) -> bool {
    let mut at = end;
    while let Some(start) = inline_tag_start(source, at) {
        at = start;
    }
    at > 0 && DIGIT_RE.test(&source[at - 1..at])
}

fn tag_phone(ctx: &mut Ctx<'_>, left: &[u16], inner: &[u16], m: &Caps<'_>, guard_edges: bool) -> Result<JsString> {
    let source = m.input;
    let start = m.index() + left.len();
    let end = m.end();
    if left == [0x3E] && digit_behind_tags(source, start) {
        return Ok(m.whole().to_vec());
    }
    if ctx.html && guard_edges && (left == [0x3E] || source.get(end) == Some(&0x3C)) {
        return Ok(cat!(left, inner));
    }
    Ok(cat!(left, nowrap(ctx, inner, source, start, end)?))
}

fn wrap_compact_plus_phone(ctx: &mut Ctx<'_>, m: &Caps<'_>) -> Result<JsString> {
    let inner = cat!("+7 ", m.s(2), " ", m.s(3), "-", m.s(4), "-", m.s(5));
    tag_phone(ctx, m.s(1), &inner, m, true)
}

fn wrap_compact_local_phone(ctx: &mut Ctx<'_>, m: &Caps<'_>) -> Result<JsString> {
    let (p1, label) = (m.s(1), m.s(2));
    let inner = cat!("8 ", m.s(3), " ", m.s(4), "-", m.s(5), "-", m.s(6));
    let source = m.input;
    let start = m.index() + p1.len() + label.len();
    let end = m.end();
    if ctx.html && source.get(end) == Some(&0x3C) {
        return Ok(cat!(p1, label, inner));
    }
    Ok(cat!(p1, label, nowrap(ctx, &inner, source, start, end)?))
}

fn ip_nowrap(ctx: &mut Ctx<'_>, m: &Caps<'_>) -> Result<JsString> {
    let ip = m.s(2);
    for part in js::split(ip, &[0x2E]) {
        if js::parse_int(part).is_some_and(|n| n > 255) {
            return Ok(m.whole().to_vec());
        }
    }
    wrap_owned(m, ctx, ip, &[])
}

fn degree_unit(u: &[u16]) -> JsString {
    if u == [0x0421] { js::u("C") } else { u.to_vec() }
}

pub static EXTENDED_PHONE: LazyLock<Re> = LazyLock::new(|| {
    Re::new(
        &format!(
            r"(^|[\s({LAQUO}{BDQUO},;:]|{})(\+7[ \t{NBSP}]+\(\d{{3}}\)[ \t{NBSP}]+\d{{3}}[ \t{NBSP}]+\d{{2}}[ \t{NBSP}]+\d{{2}}|(?:8|\+7)-\d{{3}}-\d{{3}}-\d{{2}}-\d{{2}}){}",
            *BLOCK_CONTENT_START, *CONTENT_END_LOOKAHEAD
        ),
        "gu",
    )
});

pub const IP_ADDRESS: &str = r"\d{1,3}\.\d{1,3}\.\d{1,3}\.\d{1,3}(?![\d\p{L}]|\.\d)";

const BOUND_LABEL: &str = r"[А-ЯЁа-яё]+\.?";
static BOUND_LABEL_AHEAD: LazyLock<String> = LazyLock::new(|| {
    format!(
        r"(?!{BOUND_LABEL}(?:{NBSP}|{})(?:<{OPEN}T\d{{1,9}}{CLOSE}>)*[\d{}])",
        vaulted_reference(NBSP),
        *FRACTION_GLYPHS
    )
});
static WRAPPED_LABEL_RE: LazyLock<Re> =
    LazyLock::new(|| Re::new(&format!(r"{BOUND_LABEL}[ \t{NBSP}][\d{}]", *FRACTION_GLYPHS), "uy"));

fn bind_number(m: &Caps<'_>, ctx: &mut Ctx<'_>) -> Result<JsString> {
    let source = m.input;
    let at = m.end();
    if source.get(at) == Some(&0x3C) {
        let body = canonical_nowrap_content_start(ctx, source, at, true)?;
        if body.is_none_or(|body| source.get(body) == Some(&0x3C) || WRAPPED_LABEL_RE.exec_at(source, body).is_some()) {
            return Ok(m.whole().to_vec());
        }
    }
    Ok(cat!(m.s(1), NBSP))
}

const SINGLE_INITIAL: &str = "(?:[А-ЯЁ]|Дж)";
static INITIAL_GAP: LazyLock<String> = LazyLock::new(|| format!(r"[ \t{NBSP}]+"));
static NEXT_INITIAL_RE: LazyLock<Re> = LazyLock::new(|| Re::new(&format!(r"{SINGLE_INITIAL}\.{}", *INITIAL_GAP), "uy"));
static FAMILY_NAME_RE: LazyLock<Re> = LazyLock::new(|| Re::new(&format!("{SURNAME}{}", *CONTENT_END_LOOKAHEAD), "uy"));
static FAMILY_AFTER_GAP_RE: LazyLock<Re> =
    LazyLock::new(|| Re::new(&format!("{}{SURNAME}{}", *INITIAL_GAP, *CONTENT_END_LOOKAHEAD), "uy"));

fn surname_before_initials(m: &Caps<'_>, ctx: &mut Ctx<'_>) -> Result<JsString> {
    let (surname, gap, first, dot, second) = (m.s(2), m.s(3), m.s(4), m.s(5), m.s(7));
    if gap.len() > 1
        && !dot.is_empty()
        && m.whole().last() == Some(&0x2E)
        && FAMILY_AFTER_GAP_RE.exec_at(m.input, m.end()).is_some()
    {
        return Ok(m.whole().to_vec());
    }
    wrap_name(m, ctx, &cat!(surname, " ", first, ". ", second, "."), true)
}

fn bind_single_initial(m: &Caps<'_>, ctx: &mut Ctx<'_>) -> Result<JsString> {
    let source = m.input;
    let lead = m.index().checked_sub(1);
    let mut before = lead;
    while let Some(at) = before
        && INITIAL_GAP_CHAR_RE.test(&source[at..=at])
    {
        before = at.checked_sub(1);
    }
    let held = match before {
        Some(gt) => held_before_nowrap_end(ctx, source, gt)?,
        None => None,
    };
    if held.is_some_and(|held| if before == lead { !INITIAL_LEAD_RE.test(&[held]) } else { DIGIT_RE.test(&[held]) }) {
        return Ok(m.whole().to_vec());
    }
    let mut at = m.end();
    for _ in 0..=3 {
        if ctx.html && source.get(at) == Some(&0x3C) {
            match canonical_nowrap_content_start(ctx, source, at, true)? {
                Some(start) => at = start,
                None => break,
            }
        }
        if FAMILY_NAME_RE.exec_at(source, at).is_some() {
            return Ok(cat!(m.s(1), ".", NBSP));
        }
        let Some(next) = NEXT_INITIAL_RE.exec_at(source, at) else { break };
        at = next.end();
    }
    Ok(m.whole().to_vec())
}

fn wrap_name(m: &Caps<'_>, ctx: &mut Ctx<'_>, body: &[u16], adds_dots: bool) -> Result<JsString> {
    if m.s(1) == [0x3E] {
        let open = if ctx.html { canonical_nowrap_open(ctx)? } else { Vec::new() };
        let source = m.input;
        let start = m.index() + 1;
        if open.is_empty()
            || !start.checked_sub(open.len()).is_some_and(|from| js::starts_with_at(source, &open, from))
            || (adds_dots && starts_with_compact_name(source, start))
        {
            return Ok(m.whole().to_vec());
        }
    }
    wrap_owned(m, ctx, body, &[])
}

fn wrap_owned(m: &Caps<'_>, ctx: &mut Ctx<'_>, body: &[u16], tail: &[u16]) -> Result<JsString> {
    let start = m.index() + m.s(1).len();
    let end = m.end() - tail.len();
    let content = nowrap(ctx, body, m.input, start, end)?;
    Ok(cat!(m.s(1), content, tail))
}

static PHONE_GAP: LazyLock<String> = LazyLock::new(|| format!("( |{NBSP}|{THINSP}|{})", vaulted_reference(NBSP)));

pub static FORMATTED_PHONE_PATTERNS: LazyLock<[Re; 2]> = LazyLock::new(|| {
    let gap = PHONE_GAP.as_str();
    [
        Re::new(
            &format!(
                r"([^\d+]|^)(\+?[0-9]{{1,3}}){gap}([0-9]{{3,4}}|\([0-9]{{3,4}}\)){gap}([0-9]{{2,3}})([-{MINUS}])([0-9]{{2}})([-{MINUS}])([0-9]{{2}})(?!\d)"
            ),
            "gu",
        ),
        Re::new(
            &format!(
                r"([^\d+]|^)(\+?[0-9]{{1,3}}){gap}([0-9]{{3,4}}){gap}([0-9]{{2,3}})([-{MINUS}])([0-9]{{2}})([-{MINUS}])([0-9]{{2}})(?!\d)"
            ),
            "gu",
        ),
    ]
});

pub static COMPACT_PHONE_PATTERNS: LazyLock<[Re; 2]> = LazyLock::new(|| {
    [
        Re::new(r"([^\d+]|^)\+7([0-9]{3})([0-9]{3})([0-9]{2})([0-9]{2})(?!\d)", "gu"),
        Re::new(r"(^|[^a-zа-яё0-9+])((?:тел|телефон)\.?:?\s*)8([0-9]{3})([0-9]{3})([0-9]{2})([0-9]{2})(?!\d)", "giu"),
    ]
});

pub static ALT_PHONE_PATTERN: LazyLock<Re> =
    LazyLock::new(|| Re::new(r"([^\d]|^)\+\s?([0-9]{1})\s?\(([0-9]{3,4})\)\s?(\d{3})(\d{2})(\d{2})(?!\d)", "gi"));

pub fn nobr_group() -> RuleGroupDef {
    LazyLock::force(&WORD_START_RE);
    LazyLock::force(&ACRONYM_AHEAD_RE);
    LazyLock::force(&SHORT_WORD_AHEAD_RE);
    LazyLock::force(&SHORT_WORD_LEAD_RE);
    LazyLock::force(&TEMPERATURE_LEAD_RE);
    LazyLock::force(&DIGIT_RE);
    LazyLock::force(&WRAPPED_LABEL_RE);
    let initials_end = format!(r"(?=\s|$|[.,;:?!)\]{RAQUO}{LDQUO}<])");
    let [formatted, formatted_plain] = FORMATTED_PHONE_PATTERNS.clone();
    let [compact_plus, compact_local] = COMPACT_PHONE_PATTERNS.clone();
    super::group(
        "Неразрывные конструкции",
        super::classes(super::NOWRAP_CLASSES),
        Vec::from([
            Rule::pattern(
                "nbsp_short_word",
                Re::new(
                    &format!(
                        r"([ \t\n\r]|^|[{}]|>|{CLOSE}|{MDASH}{NBSP})([a-zа-яё]{{1,2}}(?<!{NBSP}{MDASH}{NBSP}(?:{PARTICLE}))[ \t]+){}",
                        *CONTENT_OPEN_BRACKETS,
                        *SHORT_WORD_TAIL
                    ),
                    "giu",
                ),
                Replacement::func(|m, ctx| {
                    let (p1, p3, p5) = (m.s(1), m.s(2), m.s(4));
                    let p4 = m.get(3).filter(|p4| !p4.is_empty());
                    let held = if p1 == [0x3E] { held_before_nowrap_end(ctx, m.input, m.index())? } else { None };
                    if let Some(held) = held
                        && !SHORT_WORD_LEAD_RE.test(&[held])
                    {
                        return Ok(match p4 {
                            Some(p4) => cat!(p1, p3, js::trim(p4), NBSP, p5),
                            None => m.whole().to_vec(),
                        });
                    }
                    let second = match p4 {
                        Some(p4) => cat!(js::trim(p4), NBSP),
                        None => Vec::new(),
                    };
                    Ok(cat!(p1, js::trim(p3), NBSP, second, p5))
                }),
            )
            .describe("Привязка союзов и предлогов к следующему слову")
            .requires(F::SPACE)
            .project_spaced_years(SpacedYears::Gaps),
            Rule::pattern(
                "nbsp_final_word",
                Re::new(r"((?<![a-zа-яё0-9-])[a-zа-яё0-9-]{3,}) ([a-zа-яё]{1,2})\.([?!]?(?: [A-ZА-ЯЁ]|$))", "gu"),
                Replacement::Template(cat!("$1", NBSP, "$2.$3")),
            )
            .describe("Привязка предлогов к предыдущему слову в конце предложения").requires(F::SPACE | F::PERIOD),
            Rule::patterns(
                "nowrap_phone",
                vec![
                    (formatted, Replacement::func(|m, ctx| wrap_phone(ctx, m))),
                    (formatted_plain, Replacement::func(|m, ctx| wrap_phone(ctx, m))),
                    (compact_plus, Replacement::func(|m, ctx| wrap_compact_plus_phone(ctx, m))),
                    (compact_local, Replacement::func(|m, ctx| wrap_compact_local_phone(ctx, m))),
                ],
            )
            .describe("Неразрывные номера телефонов").requires(F::DIGIT).requires_pattern(Re::new("[-−+]|тел", "iu")),
            Rule::pattern(
                "nowrap_phone_alt",
                ALT_PHONE_PATTERN.clone(),
                Replacement::func(|m, ctx| {
                    let inner = cat!("+", m.s(2), " ", m.s(3), " ", m.s(4), "-", m.s(5), "-", m.s(6));
                    tag_phone(ctx, m.s(1), &inner, m, false)
                }),
            )
            .describe("Доп. формат номеров телефонов").requires(F::DIGIT | F::PAREN | F::MATH),
            Rule::pattern(
                "nowrap_ip",
                Re::new(&format!(r"(\s|{NBSP}|^)({IP_ADDRESS})"), "gu"),
                Replacement::func(|m, ctx| ip_nowrap(ctx, m)),
            )
            .describe("Объединение IP-адресов").requires(F::DIGIT | F::PERIOD),
            Rule::patterns(
                "initials_dots",
                vec![
                    (
                        Re::new(
                            r"(\s|^|[.,;:?!>])([А-ЯЁ])\.?(\s[ \t\u{00A0}]*)?([А-ЯЁ])(\s[ \t\u{00A0}]*)([А-ЯЁ][а-яё]+)(?=\s|$|[.,;:?!)\]»“<])",
                            "gu",
                        ),
                        Replacement::func(|m, ctx| wrap_name(m, ctx, &cat!(m.s(2), ". ", m.s(4), ". ", m.s(6)), true)),
                    ),
                    (
                        Re::new(
                            r"(\s|^|[.,;:?!>])([А-ЯЁ][а-яё]+)(\s[ \t\u{00A0}]*)([А-ЯЁ])(\.?)(\s[ \t\u{00A0}]*)?([А-ЯЁ])(?:\.|(?!\.))(?=\s|$|[.,;:?!)\]»“<])",
                            "gu",
                        ),
                        Replacement::func(surname_before_initials),
                    ),
                ],
            )
            .describe("Простановка точек к инициалам у фамилии")
            .off().requires(F::SPACE).requires_pattern(Re::new("[А-ЯЁ](?![а-яёА-ЯЁ])", "u")),
            Rule::patterns(
                "nowrap_initials",
                vec![
                    (
                        Re::new(
                            &format!(
                                r"(\s|^|[.,;:?!>]){}([А-ЯЁ][а-яё]+(?:-(?:{EMPHATIC_PARTICLE}))?){initials_end}",
                                *DOTTED_INITIAL_PAIR
                            ),
                            "gu",
                        ),
                        Replacement::func(|m, ctx| wrap_name(m, ctx, &cat!(m.s(2), ". ", m.s(4), ". ", m.s(6)), false)),
                    ),
                    (
                        Re::new(&format!(r"(\s|^|[.,;:?!>])({SURNAME})(\s){}{initials_end}", *DOTTED_INITIALS), "gu"),
                        Replacement::func(|m, ctx| {
                            wrap_name(m, ctx, &cat!(m.s(2), " ", m.s(4), ". ", m.s(6), "."), false)
                        }),
                    ),
                    (
                        Re::new(
                            r"(\s|^|\.|,|;|:|\?|!|>)([А-ЯЁ])(\s[ \t\u{00A0}]*)?([А-ЯЁ])(\s[ \t\u{00A0}]*)([А-ЯЁ][а-яё]+)(?=\s|$|\.|,|;|:|\?|!)",
                            "gu",
                        ),
                        Replacement::func(|m, ctx| {
                            let gap = |i: usize| if m.has(i) { " " } else { "" };
                            wrap_name(m, ctx, &cat!(m.s(2), gap(3), m.s(4), gap(5), m.s(6)), false)
                        }),
                    ),
                    (
                        Re::new(
                            r"(\s|^|\.|,|;|:|\?|!|>)([А-ЯЁ][а-яё]+)(\s[ \t\u{00A0}]*)([А-ЯЁ])(\s[ \t\u{00A0}]*)?([А-ЯЁ])(?=\s|$|\.|,|;|:|\?|!)",
                            "gu",
                        ),
                        Replacement::func(|m, ctx| {
                            let gap = if m.has(5) { " " } else { "" };
                            wrap_name(m, ctx, &cat!(m.s(2), " ", m.s(4), gap, m.s(6)), false)
                        }),
                    ),
                ],
            )
            .describe("Привязка инициалов к фамилиям").requires_any(F::SPACE | F::PERIOD).requires_pattern(Re::new("[А-ЯЁ](?![а-яёА-ЯЁ])", "u")),
            Rule::patterns(
                "nbsp_particle",
                vec![
                    (
                        Re::new(
                            &format!(
                                r"(?<![ \t]){}( |\t)+({PARTICLE})(?:({NBSP})|(?=[.,:;{HELLIP}?\s]))",
                                *NOT_AFTER_LONE_QUOTE
                            ),
                            "gu",
                        ),
                        Replacement::func(|m, ctx| {
                            let source = m.input;
                            let offset = m.index();
                            if m.s(2) == js::u("Б")
                                && has_dotted_location_after(source, m.end())
                                && follows_complete_name(&mut ctx.reference_names(), source, offset)
                            {
                                return Ok(m.whole().to_vec());
                            }
                            let keeps_glue = offset >= 1
                                && source[offset - 1] == unit(MDASH)
                                && (offset < 2 || source[offset - 2] != unit(NBSP));
                            let after: &[u16] = if !m.has(3) {
                                &[]
                            } else if !keeps_glue && takes_glue_back(source, m.end()) {
                                &[0x20]
                            } else {
                                &[0xA0]
                            };
                            Ok(cat!(NBSP, m.s(2), after))
                        }),
                    ),
                    (Re::new(r"(?<=\p{L})[ \t]+(ли|бы|б|же|ж)(?=!|\)|$)", "gu"), Replacement::Template(cat!(NBSP, "$1"))),
                ],
            )
            .describe("Неразрывный пробел перед частицей")
            .cycled().requires(F::SPACE),
            Rule::pattern(
                "nbsp_after_particle",
                Re::new(
                    &format!(
                        r"(?<=^|[{}])(?<![а-яё]-)({EMPHATIC_HEAD}-(?:{EMPHATIC_PARTICLE})) (?=[а-яё]|<{OPEN}T\d{{1,9}}{CLOSE}>)",
                        *CONTENT_START
                    ),
                    "giu",
                ),
                Replacement::func(|m, ctx| {
                    let source = m.input;
                    let next = m.end();
                    if source.get(next) == Some(&0x3C) {
                        let start = canonical_nowrap_content_start(ctx, source, next, false)?;
                        if !start.is_some_and(|start| russian_letter(source.get(start).copied())) {
                            return Ok(m.whole().to_vec());
                        }
                    }
                    Ok(cat!(m.s(1), NBSP))
                }),
            )
            .describe("Неразрывный пробел после усилительной частицы: Поди-кась так → Поди-кась так").requires(F::SPACE | F::DASH).requires_pattern(Re::new("-", "u")),
            Rule::pattern("nbsp_namely", Re::new("как то:", "giu"), Replacement::Template(cat!("как", NBSP, "то:")))
                .describe("Неразрывный пробел в \"как то\"").requires(F::SPACE | F::COLON),
            Rule::pattern(
                "nbsp_celsius",
                Re::new(
                    &format!(
                        r"(^|[{}+{PLUSMN}])(\d+)( |{NBSP})?{DEG}({TEMPERATURE_SCALES}){}",
                        *CONTENT_START, *CONTENT_END_LOOKAHEAD
                    ),
                    "gu",
                ),
                Replacement::func(|m, ctx| {
                    let held = if m.s(1) == [0x3E] { held_before_nowrap_end(ctx, m.input, m.index())? } else { None };
                    Ok(match held {
                        Some(held) if !TEMPERATURE_LEAD_RE.test(&[held]) => m.whole().to_vec(),
                        _ => cat!(m.s(1), m.s(2), NBSP, DEG, degree_unit(m.s(4))),
                    })
                }),
            )
            .describe("Привязка градусов к числу (включая ±, +, − перед числом)").requires(F::DIGIT | F::DEGREE),
            Rule::pattern(
                "nowrap_hyphen_short",
                Re::new(
                    r"(\s|>|^)([a-zа-яё]{1}-[a-zа-яё]{4}|[a-zа-яё]{2}-[a-zа-яё]{3}|[a-zа-яё]{3}-[a-zа-яё]{2}|[a-zа-яё]{4}-[a-zа-яё]{1}|когда-то|кое-как|кой-кого|вс[её]-таки|[а-яё]+-(кась|ка|де))(\s|\.|,|!|\?|…|$)",
                    "giu",
                ),
                Replacement::func(|m, ctx| wrap_owned(m, ctx, m.s(2), m.s(4))),
            )
            .describe("Пятисимвольные слова с дефисом в неразрывные блоки")
            .off()
            .cycled().requires(F::DASH),
            Rule::pattern(
                "nowrap_hyphen",
                Re::new(
                    &format!(
                        r"({NBSP}|\s|>|^|[{}])([a-zа-яё]+)((-([a-zа-яё]+)){{1,2}})(\s|\.|,|!|\?|{NBSP}|{HELLIP}|$)",
                        *CONTENT_OPEN_BRACKETS
                    ),
                    "giu",
                ),
                Replacement::func(|m, ctx| wrap_owned(m, ctx, &cat!(m.s(2), m.s(3)), m.s(6))),
            )
            .describe("Отмена переноса слова с дефисом")
            .off()
            .cycled().requires(F::DASH),
            Rule::handler("nowrap_phone_extended", |ctx| {
                crate::shared::indefinite_pronoun::nowrap_with_indefinite_colon(ctx, &EXTENDED_PHONE, |m| m.s(2).to_vec())?;
                Ok(None)
            })
            .describe("Неразрывные полные российские телефоны с пробелами или дефисами").requires(F::DIGIT).requires_any(F::DASH | F::PAREN),
            Rule::patterns(
                "nbsp_last_word",
                [
                    r"(?:\n[ \t]*\n|$)".to_owned(),
                    r"\n[ \t]*$".to_owned(),
                    format!(
                        r"(?:(?:{})?{}|<\/{OPEN}L\d+{CLOSE}>|{OPEN}H\d+{CLOSE}{})",
                        *PARAGRAPH_TAG,
                        js::lossy(&PARAGRAPH_CLOSE),
                        js::lossy(&PARAGRAPH_OPEN)
                    ),
                    format!(r"[\r\n][\r\n \t]*(?:<\/?{OPEN}L\d+{CLOSE}>|{})", js::lossy(&PARAGRAPH_OPEN)),
                ]
                .into_iter()
                .map(|end| {
                    (
                        Re::new(&format!("{LAST_WORD}(?={}{end})", *FINAL_PUNCTUATION), "gu"),
                        Replacement::func(|m, ctx| {
                            if m.s(1) == [0x3E] {
                                let held = held_before_nowrap_end(ctx, m.input, m.index())?;
                                if !russian_letter(held) {
                                    return Ok(m.whole().to_vec());
                                }
                            }
                            Ok(cat!(m.s(1), NBSP, m.s(2)))
                        }),
                    )
                })
                .collect(),
            )
            .describe("Привязка последнего русского слова длиной до четырёх букв в абзаце").requires(F::SPACE),
            Rule::pattern(
                "nbsp_after_number",
                Re::new(
                    &format!(
                        r"(?<=^|[{}])(?<![.,{NNBSP}])(?<!\d-)(?<!{inline_end})(\d+(?:[.,]\d+|[{fraction}])?|[{fraction}])[ \t{NBSP}]+(?=(?:<{OPEN}T\d{{1,9}}{CLOSE}>)*(?![Уу](?:[Ее])?{end}){label}[А-ЯЁа-яё]+{end})",
                        *CONTENT_START,
                        inline_end = *INLINE_END,
                        fraction = *FRACTION_GLYPHS,
                        end = *NUMBER_WORD_END,
                        label = *BOUND_LABEL_AHEAD
                    ),
                    "gu",
                ),
                Replacement::func(bind_number),
            )
            .describe("Привязка числа к следующему русскому слову").requires(F::SPACE).requires_any(F::DIGIT | F::FRACTION)
            .project_spaced_years(SpacedYears::Gaps),
            Rule::pattern(
                "nbsp_single_initial",
                Re::new(
                    &format!(
                        r"(?<=^|[{}])(?<!\d(?:{inline_end})*{gap})({SINGLE_INITIAL})\.{gap}",
                        *CONTENT_START,
                        inline_end = *INLINE_END,
                        gap = *INITIAL_GAP
                    ),
                    "gu",
                ),
                Replacement::func(bind_single_initial),
            )
            .describe("Привязка одиночного инициала и цепочки инициалов к фамилии").requires(F::PERIOD | F::SPACE),
        ]),
    )
}
