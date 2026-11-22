use std::sync::LazyLock;

use crate::cat;
use crate::errors::Result;
use crate::js::{self, Caps, JsString, Re};
use crate::protect::placeholders::{CLOSE, CLOSE_UNIT, INLINE_END, OPEN, TRANSPARENT_TAG_KINDS, vaulted_reference};
use crate::rule_runner::{Ctx, Replacement, Rule, RuleGroupDef, SpacedYears};
use crate::rules::quote_state::can_start_quote;
use crate::shared::boundaries::{CONTENT_END_LOOKAHEAD, CONTENT_START};
use crate::shared::character_reference::{kept_glyph_at, kept_reference_to};
use crate::shared::glyphs::{BDQUO, HELLIP, LAQUO, MDASH, NBSP, NDASH, unit};
use crate::shared::inline_boundaries::inline_text_end;
use crate::shared::measurement_units::measurement_before_particle;
use crate::shared::nowrap_context::{canonical_nowrap_open, inside_canonical_nowrap};
use crate::shared::rule_features::F;

fn join_hyphen(m: &Caps<'_>, _: &mut Ctx<'_>) -> Result<JsString> {
    Ok(cat!(m.s(1), m.s(2), "-", m.s(4)))
}

static HYPHEN_WORD_START: LazyLock<String> =
    LazyLock::new(|| format!(r"(^|[{}]|(?<=[\p{{L}}\d][.?!]|[?!]\.\.))", *CONTENT_START));

fn hyphen_join(head: &str, tail: &str, lead: &str, trail: &str, not_before: &str) -> Re {
    Re::new(
        &format!(
            "{}({head}){lead}( |\\t|{NBSP})+{trail}({tail}){not_before}{}",
            *HYPHEN_WORD_START, *CONTENT_END_LOOKAHEAD
        ),
        "giu",
    )
}

pub const EMPHATIC_HEAD: &str = "[а-яё]+(?:-[а-яё]+)*";
pub const EMPHATIC_PARTICLE: &str = "кась|тка|тко|ка|де";

static EMPHATIC_CHAIN_RE: LazyLock<Re> = LazyLock::new(|| {
    Re::new(
        &format!(
            r"(^|[{start}]|(?<=(?<![\p{{L}}\d])[а-яё]\.))(?<![а-яё]-)({EMPHATIC_HEAD})((?:[ {tab}{NBSP}]+(?:{EMPHATIC_PARTICLE}){end})+)",
            start = *CONTENT_START,
            tab = '\t',
            end = *CONTENT_END_LOOKAHEAD
        ),
        "giu",
    )
});
static PARTICLE_GAPS_RE: LazyLock<Re> = LazyLock::new(|| Re::new(r"[ \t\u{00A0}]+", "gu"));

fn join_emphatic_chain(m: &Caps<'_>, ctx: &mut Ctx<'_>) -> Result<JsString> {
    let (lead, head, tail) = (m.s(1), m.s(2), m.s(3));
    let start = m.index() + lead.len();
    if measurement_before_particle(m.input, start, start + head.len()) || !ends_past_wrapper(ctx, m.input, m.end())? {
        return Ok(m.whole().to_vec());
    }
    Ok(cat!(lead, head, PARTICLE_GAPS_RE.replace_str(tail, "-")))
}

static WRAPPER_END_RE: LazyLock<Re> = LazyLock::new(|| Re::new(&INLINE_END, "y"));
static CONTENT_END_RE: LazyLock<Re> = LazyLock::new(|| Re::new(&CONTENT_END_LOOKAHEAD, "y"));

fn ends_past_wrapper(ctx: &mut Ctx<'_>, source: &[u16], end: usize) -> Result<bool> {
    let Some(wrapper) = WRAPPER_END_RE.exec_at(source, end) else { return Ok(true) };
    let after = wrapper.end();
    if !inside_canonical_nowrap(ctx, source, end - 1, end)? || inside_canonical_nowrap(ctx, source, end - 1, after)? {
        return Ok(true);
    }
    Ok(CONTENT_END_RE.exec_at(source, after).is_some())
}

static AUTHORED_DASH: LazyLock<String> = LazyLock::new(|| format!("{MDASH}{NDASH}-"));

static DASH_REFERENCE: LazyLock<String> =
    LazyLock::new(|| format!("(?:{}|{})", kept_reference_to(&[NDASH]), kept_reference_to(&[MDASH])));
static A_DASH: LazyLock<String> = LazyLock::new(|| format!("(?:[{}]|{})", *AUTHORED_DASH, *DASH_REFERENCE));

fn kept_dash(dash: &[u16], glyph: char) -> JsString {
    if kept_glyph_at(dash, 0).0 == [unit(glyph)] { dash.to_vec() } else { vec![unit(glyph)] }
}

fn em_dash(dash: &[u16]) -> JsString {
    kept_dash(dash, MDASH)
}

const DASH_FEATURES: u32 = F::DASH | F::SEMICOLON;

static TAG_GT: LazyLock<String> = LazyLock::new(|| format!(r"(?<!<\/{OPEN}[{TRANSPARENT_TAG_KINDS}]\d+{CLOSE})>"));

pub(super) static SENTENCE_DASH: LazyLock<String> = LazyLock::new(|| format!(" {MDASH}{NBSP}"));
pub static SENTENCE_DASH_TAIL: LazyLock<String> =
    LazyLock::new(|| format!("((?:{})*) ({MDASH}|{}){NBSP}", *INLINE_END, kept_reference_to(&[MDASH])));

pub(super) static DASH_IN_GAPS: LazyLock<String> =
    LazyLock::new(|| format!(r"(?: |\t|{NBSP})+[{}](?: |\t|{NBSP})+", *AUTHORED_DASH));

static OPERAND_CHAR_RE: LazyLock<Re> = LazyLock::new(|| Re::new(r"[\p{L}\p{N}_.,]", "u"));
pub(super) static LINE_GAP: LazyLock<String> =
    LazyLock::new(|| format!(r"(?:[ \t{NBSP}]|{})", vaulted_reference(NBSP)));
static GAP_AFTER_RE: LazyLock<Re> = LazyLock::new(|| Re::new(&format!("^{}+", *LINE_GAP), "u"));
static GAP_BEFORE_RE: LazyLock<Re> = LazyLock::new(|| Re::new(&format!("{}+$", *LINE_GAP), "u"));

fn is_gap(c: Option<&u16>) -> bool {
    matches!(c, Some(0x20 | 0x09 | 0xA0))
}
fn operand(c: Option<&u16>) -> bool {
    c.is_some_and(|&c| match c {
        0..=0x7F => matches!(c, 0x2C | 0x2E | 0x5F | 0x30..=0x39 | 0x41..=0x5A | 0x61..=0x7A),
        0x410..=0x44F | 0x401 | 0x451 => true,
        _ => OPERAND_CHAR_RE.test(&[c]),
    })
}

fn equals_sign(source: &[u16], at: usize) -> bool {
    let before = if at >= 2 && source[at - 1] == 0x3e && source[at - 2] == CLOSE_UNIT {
        0x20
    } else {
        at.checked_sub(1).and_then(|i| source.get(i)).copied().unwrap_or(0x20)
    };
    source.get(at) == Some(&0x3d)
        && ![0x3d, 0x3c, 0x3e, 0x21].contains(&before)
        && ![0x3d, 0x3e].contains(source.get(at + 1).unwrap_or(&0x20))
}

fn in_equation(source: &[u16], left_end: usize, dash_end: usize) -> bool {
    if !is_gap(source.get(dash_end)) {
        return false;
    }
    let mut right_start = dash_end;
    while is_gap(source.get(right_start)) {
        right_start += 1;
    }
    let mut right_end = right_start;
    while operand(source.get(right_end)) {
        right_end += 1;
    }
    let mut left_start = left_end;
    while left_start > 0 && operand(source.get(left_start - 1)) {
        left_start -= 1;
    }
    if left_start == left_end || right_start == right_end {
        return false;
    }
    let before = left_start.saturating_sub(64);
    let left_gap = GAP_BEFORE_RE.exec(&source[before..left_start]);
    let right_gap = GAP_AFTER_RE.exec(&source[right_end..]);
    left_gap.is_some_and(|m| left_start > m.whole().len() && equals_sign(source, left_start - m.whole().len() - 1))
        || right_gap.is_some_and(|m| equals_sign(source, right_end + m.whole().len()))
}

const RANGE_DASH: &str = "\u{2013}-";

static LETTER_RE: LazyLock<Re> = LazyLock::new(|| Re::new(r"\p{L}", "u"));

fn glued_to_nowrap(m: &Caps<'_>, ctx: &mut Ctx<'_>, after: &[u16]) -> Result<bool> {
    if after != [0x3C] || !ctx.html {
        return Ok(false);
    }
    let open = canonical_nowrap_open(ctx)?;
    let at = m.end() - after.len();
    let source = m.input;
    Ok(js::starts_with_at(source, &open, at) && source.get(at + open.len()).is_some_and(|&c| LETTER_RE.test(&[c])))
}

pub fn dash_group() -> RuleGroupDef {
    LazyLock::force(&OPERAND_CHAR_RE);
    LazyLock::force(&GAP_AFTER_RE);
    LazyLock::force(&GAP_BEFORE_RE);
    LazyLock::force(&PARTICLE_GAPS_RE);
    LazyLock::force(&WRAPPER_END_RE);
    LazyLock::force(&CONTENT_END_RE);
    LazyLock::force(&LETTER_RE);
    let a_dash = &*A_DASH;
    let (inline_end, tag_gt) = (&*INLINE_END, &*TAG_GT);
    let dash_reference_end = format!("(?<!{})", *DASH_REFERENCE);
    let em_after_content = Re::new(
        &format!(
            r"((?:[^\s(\[{{<>.!?{LAQUO}{BDQUO}{HELLIP}{NDASH}{MDASH}-]{dash_reference_end}|{tag_gt}|(?<=^|[\n\r]))(?:{inline_end})*)( |\t|{NBSP})+({a_dash})(\s|$|<(?!{OPEN}[A-Za-z]+\d+{CLOSE}>\d))"
        ),
        "giu",
    );
    let em_glued = Re::new(&format!(r#"(,|:|\)|")({a_dash})(\s|$|<)"#), "giu");
    let dash_gap = format!("(?: |\\t|{NBSP})+");
    let en_range = Re::new(
        &format!(
            r"(?<![\d{RANGE_DASH}])(\d+){dash_gap}([{RANGE_DASH}]|{}){dash_gap}(\d+)(?![\d{RANGE_DASH}])",
            kept_reference_to(&[NDASH])
        ),
        "gu",
    );
    let join = || Replacement::func(join_hyphen);
    super::group(
        "Дефисы и тире",
        None,
        vec![
            Rule::patterns(
                "em_triple_hyphen",
                vec![
(Re::new(&format!(r"(?<![\p{{L}}\d_])([А-ЯЁа-яё]{{2,}})[ \t{NBSP}]*---[ \t{NBSP}]*(?=[А-ЯЁа-яё]{{2,}}(?:{}|[ \t{NBSP}]*---))", *CONTENT_END_LOOKAHEAD), "gu"), Replacement::Template(cat!("$1", NBSP, MDASH, " "))),
                ],
            ).describe("Три дефиса между русскими словами в одной строке → тире").requires(F::DASH),
            Rule::pattern(
                "em_double_hyphen",
                Re::new(&format!(r"(\s|^|{CLOSE})--(?=\s)"), "gu"),
                Replacement::Template(cat!("$1", MDASH)),
            )
            .describe("Двойной дефис → тире (как в markdown/AsciiDoc)")
            .requires(F::DASH)
            .project_spaced_years(SpacedYears::Ranges),
            Rule::pattern("en_range", en_range, Replacement::func(|m, _| {
                let mut dash = m.s(1).len();
                while is_gap(m.whole().get(dash)) { dash += 1; }
                if m.whole().get(dash) == Some(&0x2d) && in_equation(m.input, m.index() + m.s(1).len(), m.index() + dash + 1) {
                    Ok(m.whole().to_vec())
                } else { Ok(cat!(m.s(1), kept_dash(m.s(2), NDASH), m.s(3))) }
            }))
                .describe("Диапазон чисел с пробелами вокруг дефиса: «12 - 19» → «12–19»")
                .requires(F::DIGIT)
                .requires_any(DASH_FEATURES)
                .project_spaced_years(SpacedYears::Ranges),
            Rule::patterns(
                "em",
                vec![
                    (em_after_content, Replacement::func(|m, ctx| {
                        let (before, dash, after) = (m.s(1), m.s(3), m.s(4));
                        if glued_to_nowrap(m, ctx, after)? {
                            return Ok(m.whole().to_vec());
                        }
                        let dash_end = m.end() - after.len();
                        if m.input.get(dash_end - 1) == Some(&0x2d) && in_equation(m.input, m.index() + before.len(), dash_end) {
                            return Ok(m.whole().to_vec());
                        }
                        let source = m.input;
                        let at = inline_text_end(ctx.names(), source, m.index() + before.len()).checked_sub(1);
                        let lone = at.is_some_and(|at| {
                            source[at] == 0x22 && {
                                let (mut cx, _) = ctx.quote_cx();
                                can_start_quote(source, at, unit(NBSP), &mut cx, None)
                            }
                        });
                        Ok(cat!(before, if lone { ' ' } else { NBSP }, em_dash(dash), after))
                    })),
                    (em_glued, Replacement::func(|m, ctx| {
                        Ok(if glued_to_nowrap(m, ctx, m.s(3))? {
                            m.whole().to_vec()
                        } else {
                            cat!(m.s(1), NBSP, em_dash(m.s(2)), m.s(3))
                        })
                    })),
                ],
            )
            .describe("Тире после слов, кавычек, скобочек, пунктуации")
            .requires_any(DASH_FEATURES)
            .project_spaced_years(SpacedYears::Ranges),
            Rule::pattern(
                "em_line_start",
                Re::new(&format!(r"(\n|\r|^|{tag_gt}|{CLOSE})({a_dash})(\t| )"), "gu"),
                Replacement::func(|m, _| Ok(cat!(m.s(1), em_dash(m.s(2)), NBSP))),
            )
            .describe("Тире после переноса строки")
            .requires_any(DASH_FEATURES),
            Rule::patterns(
                "em_after_sentence",
                vec![
                    (
                        Re::new(&format!(r"([.!?{HELLIP}](?:{inline_end})*){dash_gap}({a_dash}){dash_gap}"), "gu"),
                        Replacement::func(|m, _| Ok(cat!(m.s(1), " ", em_dash(m.s(2)), NBSP))),
                    ),
                    (
                        Re::new(&format!(r"([.!?{HELLIP}](?:{inline_end})*){dash_gap}({a_dash})$"), "gu"),
                        Replacement::func(|m, _| Ok(cat!(m.s(1), NBSP, em_dash(m.s(2))))),
                    ),
                ],
            )
            .describe("Тире после знаков восклицания, троеточия и прочее")
            .requires_any(DASH_FEATURES),
            Rule::pattern("compound_preposition", hyphen_join("из", "за|под", "", "-?", ""), join())
                .describe("Дефис между из-за, из-под").requires(F::SPACE),
            Rule::pattern(
                "indefinite_pronoun",
                hyphen_join(
                    crate::shared::indefinite_pronoun::INDEFINITE_HEAD,
                    crate::shared::indefinite_pronoun::INDEFINITE_TAIL,
                    "-?",
                    "-?",
                    &format!("(?!(?:{inline_end})*:)"),
                ),
                join(),
            )
            .describe("Дефисы в обезличенных местоимениях").requires(F::SPACE),
            Rule::patterns(
                "hyphenated_particle",
                vec![
                    (
                        hyphen_join(
                            "кое",
                            "как|что|кто|кого|кому|чей|чья|чьё|чьи|где|куда|откуда|когда|какой|какая|какое|какие|каких|каким",
                            "-?",
                            "-?",
                            "",
                        ),
                        join(),
                    ),
                    (hyphen_join("кой", "кого", "-?", "-?", ""), join()),
                    (hyphen_join("вс[её]", "таки", "-?", "-?", ""), join()),
                ],
            )
            .describe("Кое-как, кой-кого, всё-таки").requires(F::SPACE),
            Rule::pattern("emphatic_particle", EMPHATIC_CHAIN_RE.clone(), Replacement::func(join_emphatic_chain))
                .describe("Дефис с усилительными частицами ка, кась, тка, тко, де")
                .requires(F::SPACE)
                .requires_pattern(Re::new(
                    &format!("[ \t{NBSP}](?:{EMPHATIC_PARTICLE}){}", *CONTENT_END_LOOKAHEAD),
                    "iu",
                )),
        ],
    )
}

#[cfg(test)]
mod tests {
    use super::{OPERAND_CHAR_RE, operand};

    #[test]
    fn operands_match_the_unicode_property_for_every_utf16_unit() {
        assert!(!operand(None));
        for c in u16::MIN..=u16::MAX {
            assert_eq!(operand(Some(&c)), OPERAND_CHAR_RE.test(&[c]), "unit U+{c:04X}");
        }
    }
}
