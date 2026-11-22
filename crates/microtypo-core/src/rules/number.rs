use std::collections::HashSet;
use std::sync::LazyLock;

use crate::cat;
use crate::errors::Result;
use crate::js::{self, Caps, JsString, Re};
use crate::protect::placeholders::{BLOCK_KIND, CLOSE, OPEN, vaulted_reference};
use crate::rule_runner::{Ctx, Replacement, Rule, RuleGroupDef, SpacedYears};
use crate::shared::boundaries::{CONTENT_END_LOOKAHEAD, CONTENT_START};
use crate::shared::character_reference::{
    CharacterReference, character_reference_ending_at, kept_glyph_at, kept_glyphs_at, kept_reference_glyph,
    kept_reference_to, read_character_reference_at,
};
use crate::shared::glyphs::{
    BDQUO, CONG, EXTENDED_FRACTION_GLYPHS, FRAC12, FRAC14, FRAC34, FRACTION_GLYPHS, GE, HELLIP, LAQUO, LE, MDASH,
    MINUS, NBSP, NDASH, NE, NNBSP, NUMERO, PLUSMN, PRIME, SECT, SINGLE_PRIME, THINSP, TIMES, unit,
};
use crate::shared::inline_boundaries::{inline_text_end, inline_text_start, native_tag_end, native_tag_start};
use crate::shared::nowrap_context::held_before_nowrap_end;
use crate::shared::quote_pairs::Visit;

use super::quote_state::QuoteTags;

use super::date::NUMERIC_DATE_SOURCE;
use super::nobr::{ALT_PHONE_PATTERN, COMPACT_PHONE_PATTERNS, EXTENDED_PHONE, FORMATTED_PHONE_PATTERNS, IP_ADDRESS};
use crate::shared::rule_features::F;

static DIMENSION_CHAIN: LazyLock<Re> = LazyLock::new(|| {
    Re::new(&format!(r"(?<!\d)\d+(?:[ \t]*(?:[xх{TIMES}]|{})[ \t]*\d+)+", kept_reference_to(&[TIMES])), "gu")
});
static DIMENSION_X_RE: LazyLock<Re> = LazyLock::new(|| Re::new(r"(?<=\d)[xх](?=\d)", "gu"));

fn dimension_identifier(c: Option<u16>) -> bool {
    c.is_some_and(|c| matches!(c, 0x30..=0x39 | 0x41..=0x5A | 0x61..=0x7A | 0x5F | 0x3C | 0x3E))
}

fn reference_continues_identifier(reference: Option<CharacterReference>) -> bool {
    let Some(reference) = reference.filter(|r| r.native && r.head.starts_with(&[0x26, 0x23])) else { return false };
    let code = numeric_reference_code(&reference.head);
    code <= 0x7f && dimension_identifier(Some(code as u16))
}

fn normalize_dimensions(m: &Caps<'_>, ctx: &mut Ctx<'_>) -> Result<JsString> {
    let (chain, source) = (m.whole(), m.input);
    let start = inline_text_end(ctx.reference_names(), source, m.index());
    let end = inline_text_start(ctx.reference_names(), source, m.end());
    let left =
        if native_tag_start(source, start).is_none() && start > 0 { source.get(start - 1).copied() } else { None };
    let right = if native_tag_end(source, end).is_none() { source.get(end).copied() } else { None };
    if dimension_identifier(left)
        || dimension_identifier(right)
        || (left == Some(0x3B)
            && reference_continues_identifier(character_reference_ending_at(source, start - 1, None)?))
        || (right == Some(0x26) && reference_continues_identifier(read_character_reference_at(source, end)))
    {
        return Ok(chain.to_vec());
    }
    let packed: JsString = chain.iter().copied().filter(|&c| !matches!(c, 0x20 | 0x09)).collect();
    Ok(DIMENSION_X_RE.replace_tpl(&packed, &[unit(TIMES)]))
}

static DIGIT_RE: LazyLock<Re> = LazyLock::new(|| Re::new(r"\d", ""));
static ASCII_DIGIT_RE: LazyLock<Re> = LazyLock::new(|| Re::new("[0-9]", "u"));
static LETTER_RE: LazyLock<Re> = LazyLock::new(|| Re::new(r"\p{L}", "u"));

fn collapse_gaps(text: &[u16], with: u16) -> JsString {
    let mut out = Vec::with_capacity(text.len());
    let mut gap = false;
    for &ch in text {
        if matches!(ch, 0x20 | 0x09) {
            if !gap {
                out.push(with);
            }
            gap = true;
        } else {
            out.push(ch);
            gap = false;
        }
    }
    out
}

static NUMBER_STOP: LazyLock<String> =
    LazyLock::new(|| format!(r"[\d{}{NDASH}{MDASH}{MINUS}\-{HELLIP}]|[.,]\d|\.\.", *FRACTION_GLYPHS));
static NUMBER_STOP_RE: LazyLock<Re> = LazyLock::new(|| Re::new(&format!("^(?:{})", *NUMBER_STOP), "u"));
static TRIAD_RE: LazyLock<Re> = LazyLock::new(|| Re::new(r"^\d{3}(?!\d)", "u"));

static MINUS_PATTERNS: LazyLock<Vec<Re>> = LazyLock::new(|| {
    let fractions = FRACTION_GLYPHS.as_str();
    let negative = format!(r"(\d+(?:[.,]\d+)?[{fractions}]?|[{fractions}])(?!{})", *NUMBER_STOP);
    [
        format!(r"((?:^|\n)[ \t{NBSP}]*)"),
        format!(r"([({LAQUO}{BDQUO}:][ \t{NBSP}]*)"),
        format!(r"((?<=[А-ЯЁа-яё]{{3}})[ \t{NBSP}]+)"),
        format!(r"((?:<{OPEN}[TAL]|<\/{OPEN}{BLOCK_KIND})\d{{1,9}}{CLOSE}>[ \t{NBSP}]*)"),
        format!(r"((?<!{CLOSE})(?<!&#\d{{1,7}});[ \t{NBSP}]*)"),
    ]
    .map(|start| Re::new(&format!("{start}-{negative}"), "gu"))
    .into()
});
static MATH_TAG_RE: LazyLock<Re> = LazyLock::new(|| Re::new(r"^(?:small|sup|sub)$", "u"));

fn negative_numbers(ctx: &mut Ctx<'_>) -> Result<Option<JsString>> {
    let tags: HashSet<_> = ctx
        .find_by_tag_name(&crate::protect::tags::NameMatcher::Regex(&MATH_TAG_RE))
        .into_iter()
        .map(|(prefix, id)| js::u(&format!("<{OPEN}{prefix}{id}{CLOSE}>")))
        .collect();
    let mut changed: Option<JsString> = None;
    for pattern in MINUS_PATTERNS.iter() {
        let next = pattern.replace_matches(changed.as_deref().unwrap_or(&ctx.text), |m| {
            if tags.contains(js::trim_end(m.s(1))) || stops_number(m.input, m.end()) {
                m.whole().to_vec()
            } else {
                cat!(m.s(1), MINUS, m.s(2))
            }
        });
        if next.is_some() {
            changed = next;
        }
    }
    Ok(changed)
}

fn stops_number(source: &[u16], at: usize) -> bool {
    let mut end = at;
    loop {
        let (glyph, gap_end) = kept_glyph_at(source, end);
        if glyph != [unit(NNBSP)] || !TRIAD_RE.test(js::slice(source, gap_end, gap_end + 4)) {
            return NUMBER_STOP_RE.test(&kept_glyphs_at(source, end, 2));
        }
        end = gap_end + 3;
    }
}

static FRACTION_TAIL: LazyLock<String> = LazyLock::new(|| format!(r#"[\p{{L}}\d_/"{PRIME}{SINGLE_PRIME}]"#));
static FRACTION_TAIL_RE: LazyLock<Re> = LazyLock::new(|| Re::new(&FRACTION_TAIL, "u"));
const FRACTION_LEAD: &str = r"[\p{L}\d_./]";
static FRACTION_LEAD_RE: LazyLock<Re> = LazyLock::new(|| Re::new(FRACTION_LEAD, "u"));

static LETTER_OR_NUMBER_RE: LazyLock<Re> = LazyLock::new(|| Re::new(r"[\p{L}\p{N}]", "u"));

pub fn joined_numeric_token(ctx: &mut Ctx<'_>, source: &[u16], offset: usize) -> bool {
    let before = inline_text_end(ctx.names(), source, offset);
    let width = if before >= 2 && js::code_point_at(source, before - 2).is_some_and(|c| c > 0xFFFF) { 2 } else { 1 };
    before < offset && LETTER_OR_NUMBER_RE.test(&source[before.saturating_sub(width)..before])
}

fn digit_before_gap(source: &[u16], at: usize) -> bool {
    let mut i = at;
    while i > 0 && matches!(source[i - 1], 0x20 | 0x09) {
        i -= 1;
    }
    if i == at || i == 0 || !ASCII_DIGIT_RE.test(&source[i - 1..i]) {
        return false;
    }
    let mut before = i - 1;
    while before > 0 && ASCII_DIGIT_RE.test(&source[before - 1..before]) {
        before -= 1;
    }
    before == 0 || !LETTER_RE.test(&source[before - 1..before])
}

static RANGE_DASH: LazyLock<String> =
    LazyLock::new(|| format!("(?:[{MDASH}{MINUS}-]|{})", kept_reference_to(&[MINUS, MDASH])));

fn range_dash_glyph(glyph: Option<&[u16]>) -> bool {
    matches!(glyph, Some([0x2D | 0x2014 | 0x2212]))
}

fn normalize_range(m: &Caps<'_>) -> Result<JsString> {
    let (source, at) = (m.input, m.index());
    let left = match at.checked_sub(1).map(|i| source[i]) {
        Some(0x3B) => kept_reference_glyph(character_reference_ending_at(source, at - 1, None)?.as_ref()),
        Some(c) => Some(vec![c]),
        None => None,
    };
    let right = kept_glyph_at(source, m.end()).0;
    Ok(if range_dash_glyph(left.as_deref()) || range_dash_glyph(Some(&right)) {
        m.whole().to_vec()
    } else {
        cat!(m.s(1), NDASH, m.s(2))
    })
}

static PHONE_NBSP_RE: LazyLock<Re> = LazyLock::new(|| Re::new(&vaulted_reference(NBSP), "gu"));
static PHONE_NBSP_END_RE: LazyLock<Re> = LazyLock::new(|| Re::new(&format!("{}$", vaulted_reference(NBSP)), "u"));
static PHONE_NBSP_START_RE: LazyLock<Re> = LazyLock::new(|| Re::new(&vaulted_reference(NBSP), "uy"));
const PHONE_REFERENCE_LENGTH: usize = 18;

fn phone_spaces(text: &[u16]) -> JsString {
    collapse_gaps(&PHONE_NBSP_RE.replace_tpl(text, &[unit(NBSP)]), 0x20)
}

fn phone_context_patterns() -> [&'static Re; 6] {
    [
        &FORMATTED_PHONE_PATTERNS[0],
        &FORMATTED_PHONE_PATTERNS[1],
        &COMPACT_PHONE_PATTERNS[0],
        &COMPACT_PHONE_PATTERNS[1],
        &*ALT_PHONE_PATTERN,
        &*EXTENDED_PHONE,
    ]
}

pub fn phone_context_at(source: &[u16], offset: usize) -> i8 {
    let mut from = offset;
    let mut characters = 0;
    while from > 0 && characters < 32 {
        if source[from - 1] == 0x3B
            && let Some(reference) =
                PHONE_NBSP_END_RE.exec(js::slice(source, from.saturating_sub(PHONE_REFERENCE_LENGTH), from))
        {
            from -= reference.whole().len();
            continue;
        }
        from -= 1;
        if !matches!(source[from], 0x20 | 0x09) {
            characters += 1;
        }
    }
    let mut to = offset;
    characters = 0;
    while to < source.len() && characters < 32 {
        if source[to] == 0x26
            && let Some(reference) = PHONE_NBSP_START_RE.exec_at(source, to)
        {
            to += reference.whole().len();
            continue;
        }
        if !matches!(source[to], 0x20 | 0x09) {
            characters += 1;
        }
        to += 1;
    }
    let head = phone_spaces(&source[from..offset]);
    let at = head.len();
    let head_end = js::trim_end(&head).len();
    let window = cat!(head, phone_spaces(&source[offset..to]));
    for pattern in phone_context_patterns() {
        for m in pattern.find_all(&window) {
            let start = m.index() + m.s(1).len();
            let end = m.end();
            if start <= at && at < end {
                return 1;
            }
            if end == head_end {
                return -1;
            }
        }
    }
    0
}

static DATE_FOLLOW_RE: LazyLock<Re> = LazyLock::new(|| Re::new(NUMERIC_DATE_SOURCE, "iuy"));

static TRIAD_AHEAD_RE: LazyLock<Re> = LazyLock::new(|| {
    Re::new(
        &format!(
            r"(?:[ \t]+|[{NBSP}{NNBSP}{THINSP}]|{}|{}|{})(?!{IP_ADDRESS})\d{{3}}(?!\d)",
            vaulted_reference(NBSP),
            vaulted_reference(NNBSP),
            vaulted_reference(THINSP)
        ),
        "uy",
    )
});

fn extended_fraction(m: &Caps<'_>) -> JsString {
    let glyph = match m.s(1) {
        [0x31, 0x2f, 0x33] => 0,
        [0x32, 0x2f, 0x33] => 1,
        [0x31, 0x2f, 0x38] => 2,
        [0x33, 0x2f, 0x38] => 3,
        [0x35, 0x2f, 0x38] => 4,
        _ => 5,
    };
    vec![EXTENDED_FRACTION_GLYPHS[glyph]]
}

static FOOT_SOURCE: LazyLock<String> = LazyLock::new(|| {
    format!(
        r"(?<=^|[{}])(?<![.,])(?<!(?:{NUMERO}|{})(?:\s|{}){{0,2}})({DECIMAL_NUMBER})'{}",
        *CONTENT_START,
        vaulted_reference(NUMERO),
        vaulted_reference(NNBSP),
        *CONTENT_END_LOOKAHEAD
    )
});
static FOOT_AT: LazyLock<Re> = LazyLock::new(|| Re::new(&FOOT_SOURCE, "uy"));

const DECIMAL_NUMBER: &str = r"\d+(?:[.,]\d+)?";

fn numeric_reference_code(head: &[u16]) -> u64 {
    let (digits, radix) =
        if head.get(2).is_some_and(|&c| c == 0x78 || c == 0x58) { (&head[3..], 16) } else { (&head[2..], 10) };
    let mut code: u64 = 0;
    for &c in digits {
        let Some(d) = char::from_u32(u32::from(c)).and_then(|c| c.to_digit(radix)) else { break };
        code = code.saturating_mul(u64::from(radix)).saturating_add(u64::from(d));
    }
    code
}

fn visit_once(v: Visit<'_>) -> Result<()> {
    v.map_or(Ok(()), |f| f())
}

pub fn authored_foot_at(text: &[u16], at: usize, v: Visit<'_>) -> Result<bool> {
    let mut start = at;
    while start > 0 && matches!(text[start - 1], 0x30..=0x39 | 0x2E | 0x2C) {
        visit_once(v)?;
        start -= 1;
    }
    Ok(FOOT_AT.exec_at(text, start).is_some_and(|m| m.end() == at + 1) && phone_context_at(text, start) != 1)
}

static AMOUNT_GAP: LazyLock<String> = LazyLock::new(|| format!(r"[ \t{NBSP}{NNBSP}{THINSP}]"));
static AMOUNT_GAP_RE: LazyLock<Re> = LazyLock::new(|| Re::new(&AMOUNT_GAP, "u"));
static AMOUNT_PART: LazyLock<Re> =
    LazyLock::new(|| Re::new(&format!(r"[\d.,/⁄ \t{NBSP}{NNBSP}{THINSP}{MINUS}-]"), "u"));
static INCH_AMOUNT: LazyLock<Re> = LazyLock::new(|| {
    let gap = &*AMOUNT_GAP;
    Re::new(
        &format!(r"^[-{MINUS}]?(?:{DECIMAL_NUMBER}|\d{{1,3}}(?:{gap}+\d{{3}})+(?:[.,]\d+)?|(?:\d+{gap}+)?\d+[/⁄]\d+)$"),
        "u",
    )
});

fn amount_entity(head: &[u16]) -> Option<u16> {
    let name = String::from_utf16_lossy(head);
    Some(match name.as_str() {
        "&prime" => unit(SINGLE_PRIME),
        "&comma" => 0x2C,
        "&period" => 0x2E,
        "&sol" => 0x2F,
        "&frasl" => 0x2044,
        "&minus" => unit(MINUS),
        "&nbsp" | "&NonBreakingSpace" => unit(NBSP),
        "&thinsp" | "&ThinSpace" => unit(THINSP),
        "&Tab" => 0x09,
        _ => return None,
    })
}

fn amount_glyph_before(
    text: &[u16],
    offset: usize,
    cx: &mut QuoteTags<'_>,
    v: Visit<'_>,
) -> Result<Option<(u16, usize, usize)>> {
    let end = if offset > 0 && text[offset - 1] == 0x3E {
        cx.text_end_visit(text, offset, &mut || visit_once(v))?
    } else {
        offset
    };
    let Some(mut start) = end.checked_sub(1) else { return Ok(None) };
    let mut glyph = Some(text[start]);
    if glyph == Some(0x3B) {
        let Some(reference) = character_reference_ending_at(text, start, cx.budget())? else { return Ok(None) };
        start = text[..=start].iter().rposition(|&c| c == 0x26).unwrap_or(0);
        glyph = if reference.head.starts_with(&[0x26, 0x23]) {
            let code = numeric_reference_code(&reference.head);
            u16::try_from(code).ok()
        } else if reference.native {
            None
        } else {
            amount_entity(&reference.head)
        };
    }
    Ok(glyph.map(|glyph| (glyph, start, end)))
}

pub fn follows_feet(text: &[u16], digit: usize, cx: &mut QuoteTags<'_>, v: Visit<'_>, read_foot: bool) -> Result<bool> {
    let mut pieces: Vec<JsString> = Vec::new();
    let mut end = digit + 1;
    let mut tail = end;
    while end > 0 {
        visit_once(v)?;
        let Some((glyph, start, part_end)) = amount_glyph_before(text, end, cx, v)? else { return Ok(false) };
        if part_end < end {
            pieces.push(text[end..tail].to_vec());
            tail = part_end;
        }
        if glyph == unit(SINGLE_PRIME) || glyph == 0x27 {
            let mut before = amount_glyph_before(text, start, cx, v)?;
            while let Some((gap, gap_start, _)) = before
                && AMOUNT_GAP_RE.test(&[gap])
            {
                visit_once(v)?;
                before = amount_glyph_before(text, gap_start, cx, v)?;
            }
            if !before.is_some_and(|(g, _, _)| matches!(g, 0x30..=0x39)) {
                return Ok(false);
            }
            if glyph != unit(SINGLE_PRIME) && !(read_foot && authored_foot_at(text, start, v)?) {
                return Ok(false);
            }
            let mut amount = text[part_end..tail].to_vec();
            for piece in pieces.iter().rev() {
                amount.extend_from_slice(piece);
            }
            return Ok(INCH_AMOUNT.test(js::trim(&amount)));
        }
        if !AMOUNT_PART.test(&[glyph]) {
            return Ok(false);
        }
        if start + 1 < part_end {
            pieces.push(text[part_end..tail].to_vec());
            pieces.push(vec![glyph]);
            tail = start;
        }
        end = start;
    }
    Ok(false)
}

pub fn number_group() -> RuleGroupDef {
    super::group(
        "Числа, дроби, математические знаки",
        super::classes(super::NOWRAP_CLASSES),
        Vec::from([
            Rule::pattern(
                "prime",
                Re::new(&FOOT_SOURCE, "gu"),
                Replacement::func(|m, _| {
                    Ok(if phone_context_at(m.input, m.index()) == 1 {
                        m.whole().to_vec()
                    } else {
                        cat!(m.s(1), SINGLE_PRIME)
                    })
                }),
            )
            .describe("Штрих после числа для футов и угловых минут")
            .requires(F::DIGIT | F::APOSTROPHE)
            .changes_inline_boundary(),
            Rule::pattern(
                "fraction_extended",
                Re::new(&format!(r"(?<!{FRACTION_LEAD})([12]\/3|[1357]\/8)(?!{})", *FRACTION_TAIL), "gu"),
                Replacement::func(|m, ctx| {
                    let end = m.end();
                    let held = match m.index().checked_sub(1) {
                        Some(gt) => held_before_nowrap_end(ctx, m.input, gt)?,
                        None => None,
                    };
                    Ok(
                        if held.is_some_and(|held| FRACTION_LEAD_RE.test(&[held]))
                            || (m.input.get(end) == Some(&0x26)
                                && FRACTION_TAIL_RE.test(&kept_glyph_at(m.input, end).0))
                        {
                            m.whole().to_vec()
                        } else {
                            extended_fraction(m)
                        },
                    )
                }),
            )
            .describe("Дополнительные дроби: ⅓ ⅔ ⅛ ⅜ ⅝ ⅞")
            .requires(F::DIGIT | F::SLASH),
            Rule::handler("minus", negative_numbers)
                .describe("Знак отрицательного числа вне диапазона и вычитания")
                .requires(F::DASH),
            Rule::pattern(
                "time",
                Re::new(r"(?<![\d:])(\d{1,2})\s*:\s*(\d{2})(?:\s*:\s*(\d{2}))?(?![\d:])", "g"),
                Replacement::func(|m, _| {
                    let time = if let Some(ss) = m.get(3) {
                        cat!(m.s(1), ":", m.s(2), ":", ss)
                    } else {
                        cat!(m.s(1), ":", m.s(2))
                    };
                    if time == m.whole()
                        || TRIAD_AHEAD_RE.exec_at(m.input, m.end()).is_some()
                        || phone_context_at(m.input, m.index()) == 1
                    {
                        return Ok(m.whole().to_vec());
                    }
                    Ok(time)
                }),
            )
            .describe("Нормализация времени: 08 : 59 → 08:59, 9:30 не трогаем")
            .requires(F::DIGIT | F::COLON),
            Rule::pattern(
                "en_range",
                Re::new(&format!(r"(?<!\d)(\d+){}(\d+)(?!\d)", *RANGE_DASH), "gu"),
                Replacement::func(|m, _| normalize_range(m)),
            )
            .describe("Среднее тире для диапазонов чисел: 100-500 → 100–500")
            .requires(F::DIGIT)
            .requires_any(F::DASH | F::SEMICOLON)
            .project_spaced_years(SpacedYears::Gaps),
            Rule::pattern(
                "minus_range",
                Re::new(r"(^|\s)([−-])(\d+)(\.\.\.|…)([ \t]+|\s)?(\+|-|−)?(\d+)", "gu"),
                Replacement::func(|m, _| {
                    let sign = match m.get(6) {
                        Some([0x2D]) => cat!(MINUS),
                        Some(p6) => p6.to_vec(),
                        None => Vec::new(),
                    };
                    Ok(cat!(m.s(1), MINUS, m.s(3), m.s(4), m.s(5), sign, m.s(7)))
                }),
            )
            .describe("Минус между диапазоном чисел")
            .requires(F::DIGIT | F::DASH),
            Rule::pattern("times", DIMENSION_CHAIN.clone(), Replacement::func(normalize_dimensions))
                .describe("x на × в размерных единицах")
                .requires(F::DIGIT)
                .requires_pattern(Re::new("[xх×]|&#", "u")),
            Rule::pattern(
                "sub",
                Re::new(r"(?<![\p{L}\d_])([a-zа-яё])_(\d{1,3})([^@а-яёa-z0-9]|$)", "giu"),
                Replacement::func(|m, ctx| {
                    let small = ctx.tag(m.s(2), "small", &[])?;
                    let sub = ctx.tag(&small, "sub", &[])?;
                    Ok(cat!(m.s(1), sub, m.s(3)))
                }),
            )
            .describe("Нижний индекс")
            .html_only()
            .requires(F::DIGIT | F::UNDERSCORE),
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
            .html_only()
            .requires(F::DIGIT | F::CARET),
            Rule::patterns(
                "fraction",
                vec![
                    (Re::new(r#"(^|\D)1\/2(?![\d"″′])"#, "g"), Replacement::Template(cat!("$1", FRAC12))),
                    (Re::new(r#"(^|\D)1\/4(?![\d"″′])"#, "g"), Replacement::Template(cat!("$1", FRAC14))),
                    (Re::new(r#"(^|\D)3\/4(?![\d"″′])"#, "g"), Replacement::Template(cat!("$1", FRAC34))),
                ],
            )
            .describe("Дроби 1/2, 1/4, 3/4 → символы (кроме дюймов)")
            .requires(F::DIGIT | F::SLASH),
            Rule::patterns(
                "math",
                vec![
                    (Re::new("(?<!!)!=", "g"), Replacement::Template(cat!(NE))),
                    (Re::new("<=(?![>=])", "g"), Replacement::Template(cat!(LE))),
                    (Re::new(&format!("([^={OPEN}{CLOSE}]|^)>="), "gu"), Replacement::Template(cat!("$1", GE))),
                    (Re::new("~=", "g"), Replacement::Template(cat!(CONG))),
                    (Re::new(r"(?<![-+])\+-(?![-+])", "g"), Replacement::Template(cat!(PLUSMN))),
                ],
            )
            .describe("Математические знаки: !=, <=, >=, ~=, +-")
            .requires(F::MATH),
            Rule::pattern(
                "thin_space_triads",
                Re::new(r"(?<![\p{L}\p{N}])([0-9]{1,3}([ \t]+[0-9]{3}){1,})(?!\p{N})(?=([\s\S]|$))", "gu"),
                Replacement::func(|m, ctx| {
                    let (source, offset) = (m.input, m.index());
                    let p4 = m.s(3);
                    let phone = phone_context_at(source, offset);
                    if p4 == [0x2D]
                        || joined_numeric_token(ctx, source, offset)
                        || phone == 1
                        || (digit_before_gap(source, offset) && phone != -1)
                    {
                        return Ok(m.whole().to_vec());
                    }
                    if p4 == [0x20]
                        && DIGIT_RE.test(js::slice(source, m.end() + 1, m.end() + 2))
                        && DATE_FOLLOW_RE.exec_at(source, m.end()).is_none()
                    {
                        return Ok(m.whole().to_vec());
                    }
                    Ok(collapse_gaps(m.s(1), unit(NNBSP)))
                }),
            )
            .describe("Триады чисел через узкий неразрывный пробел")
            .requires(F::SPACE | F::DIGIT)
            .requires_pattern(Re::new(r"[0-9][ \t]+[0-9]{3}", "u")),
            Rule::pattern(
                "nbsp_numero",
                Re::new(r"[№]\s*(\d)", "gu"),
                Replacement::Template(cat!(NUMERO, NNBSP, "$1")),
            )
            .describe("Узкий неразрывный пробел между № и числом")
            .requires(F::DIGIT | F::NUMERO),
            Rule::pattern(
                "nbsp_section",
                Re::new(r"§\s*(\d+|[IVX]+|[a-zа-яё]+)", "giu"),
                Replacement::Template(cat!(SECT, NNBSP, "$1")),
            )
            .describe("Узкий неразрывный пробел между § и числом")
            .requires(F::SECTION),
        ]),
    )
}
