use std::sync::LazyLock;

use crate::cat;
use crate::errors::Result;
use crate::js::{self, Caps, JsString, Re};
use crate::protect::placeholders::{CLOSE, INLINE_END, OPEN, TRANSPARENT_BARE_KINDS, TRANSPARENT_TAG_KINDS};
use crate::rule_runner::{Ctx, Replacement, Rule, RuleGroupDef};
use crate::shared::abbreviation_grammar::{
    BUILDING_LABEL, CURRENCY_CODE, DOCUMENT_LABEL, DOCUMENT_PART, DOTTED_INITIAL_PAIR, LABEL_NUMBER_END, MAGNITUDE,
    ORGANIZATION, OTHER_ORGANIZATION, STREET_LABEL, TITLE, currency_unit, follows_complete_name, horsepower,
    postscript,
};
use crate::shared::boundaries::{
    BLOCK_WRAPPER_START_GROUP, CONTENT_END_LOOKAHEAD, CONTENT_START, CONTENT_START_GROUP, WRAPPER_START_GROUP,
};
use crate::shared::character_reference::kept_glyphs_at;
use crate::shared::glyphs::{
    BDQUO, EURO, FRAC12, FRAC14, FRAC34, LAQUO, MDASH, MINUS, NBSP, NDASH, NNBSP, RUB, SUP2, SUP3, unit,
};
use crate::shared::measurement_units::{
    CURRENCY_PREFIX_SIGNS, DATA_UNITS, FREQUENCY_UNITS, LENGTH_UNITS, SPEED_UNITS, TIME_UNITS, UnitBind, VOLT_UNIT_RE,
    VOLUME_UNITS, WEIGHT_UNITS, unit_bind,
};
use crate::shared::nowrap_context::{
    canonical_nowrap_content_start, held_before_nowrap_end, inside_canonical_nowrap, nowrap,
};

use super::dash::{DASH_IN_GAPS, LINE_GAP, SENTENCE_DASH, SENTENCE_DASH_TAIL};
use super::number::phone_context_at;
use crate::shared::rule_features::F;

static INLINE_LEAD_RE: LazyLock<Re> = LazyLock::new(|| {
    Re::new(
        &format!(
            r"^(?:[ \t{NBSP}(]|<\/?{OPEN}[{TRANSPARENT_TAG_KINDS}]\d+{CLOSE}>|{OPEN}[{TRANSPARENT_BARE_KINDS}]\d+{CLOSE})+"
        ),
        "u",
    )
});

static SENTENCE_ENDS_RE: LazyLock<Re> =
    LazyLock::new(|| Re::new(&format!(r"^\s*$|^[ \t{NBSP}]*(?:[\n<{LAQUO}{BDQUO}]|\p{{Lu}})"), "u"));

fn ends_sentence(rest: &[u16]) -> bool {
    let lead = INLINE_LEAD_RE.exec(rest).map_or(0, |m| m.end());
    SENTENCE_ENDS_RE.test(&rest[lead..])
}

static CONTENT_START_RE: LazyLock<Re> = LazyLock::new(|| Re::new(&format!("[{}]", *CONTENT_START), "u"));

fn unbound_number(m: &Caps<'_>, ctx: &mut Ctx<'_>) -> Result<bool> {
    let held = held_before_nowrap_end(ctx, m.input, m.index())?;
    Ok(held.is_some_and(|held| !CONTENT_START_RE.test(&[held]))
        || phone_context_at(m.input, m.index() + m.s(1).len()) == 1)
}

fn bind_unit(m: &Caps<'_>, ctx: &mut Ctx<'_>) -> Result<JsString> {
    Ok(if unbound_number(m, ctx)? { m.whole().to_vec() } else { cat!(m.s(1), m.s(2), NBSP, m.s(4)) })
}

fn bind_counted(m: &Caps<'_>, ctx: &mut Ctx<'_>) -> Result<JsString> {
    Ok(if unbound_number(m, ctx)? { m.whole().to_vec() } else { cat!(m.s(1), m.s(2), NBSP, m.s(3)) })
}

static CURRENCY_GAP: LazyLock<String> = LazyLock::new(|| format!(r"[ \t{NBSP}]*"));
static CURRENCY_AMOUNT: LazyLock<String> = LazyLock::new(|| format!(r"(\d(?:{})?)", *INLINE_END));

const MAGNITUDE_WORD: &str = "рублей|рубля|рубль|долларов|доллара|доллар|евро|человек";

static USD_TAIL: LazyLock<String> = LazyLock::new(|| {
    format!(r"(?:{})?\.[ *_~]{{0,4}}(?:<{OPEN}[{TRANSPARENT_TAG_KINDS}]\d{{1,9}}{CLOSE}>)?США", *INLINE_END)
});
static USD_ABBREVIATION_RE: LazyLock<Re> =
    LazyLock::new(|| Re::new(&format!("{}{}(долл{})(?![а-яёa-z])", *CURRENCY_AMOUNT, *CURRENCY_GAP, *USD_TAIL), "giu"));

static GLUED_USA_RE: LazyLock<Re> = LazyLock::new(|| Re::new(r"^(долл)\.(?=США)", "iu"));

static CITY_AFTER_RE: LazyLock<Re> = LazyLock::new(|| Re::new(&format!(r"^\.{}{{0,4}}[А-ЯЁA-Z]", *LINE_GAP), "u"));
const CITY_AFTER_REACH: usize = 40;
static INDEX_HALF_BEFORE_RE: LazyLock<Re> =
    LazyLock::new(|| Re::new(&format!(r"(?:^|[^\p{{L}}\d{NNBSP}])\d{{3}}$"), "u"));

fn is_postal_index(start: &[u16], number: &[u16], offset: usize, source: &[u16]) -> bool {
    if start == [unit(NNBSP)] {
        return number.len() == 3 && INDEX_HALF_BEFORE_RE.test(&source[offset.saturating_sub(4)..offset]);
    }
    number.len() == 6
}

static IE_TAIL_RE: LazyLock<Re> = LazyLock::new(|| Re::new(&format!(r"^\.?[ {NBSP}]?е\."), "u"));

static INITIALS_AHEAD_RE: LazyLock<Re> =
    LazyLock::new(|| Re::new(&format!("^{}[А-ЯЁ][а-яё]", *DOTTED_INITIAL_PAIR), "u"));

fn bind_weight(m: &Caps<'_>, ctx: &mut Ctx<'_>) -> Result<JsString> {
    let (source, offset, end) = (m.input, m.index(), m.end());
    let (start, number, unit_name) = (m.s(1), m.s(2), m.s(4));
    let gap = m.has(3);

    if unbound_number(m, ctx)? {
        return Ok(m.whole().to_vec());
    }
    if gap && matches!(unit_name, [0x442 | 0x422]) && IE_TAIL_RE.test(&kept_glyphs_at(source, end, 4)) {
        return Ok(m.whole().to_vec());
    }
    if matches!(unit_name, [0x433 | 0x413])
        && CITY_AFTER_RE.test(js::slice(source, end, end + CITY_AFTER_REACH))
        && is_postal_index(start, number, offset, source)
    {
        return Ok(if gap { m.whole().to_vec() } else { cat!(start, number, " ", unit_name) });
    }

    Ok(cat!(start, number, NBSP, unit_name))
}

static INITIALS_BEFORE_GOST: LazyLock<Re> =
    LazyLock::new(|| Re::new(&format!("(?<={}{})", *CONTENT_START_GROUP, *DOTTED_INITIAL_PAIR), "uy"));

fn is_surname_gost(m: &Caps<'_>) -> bool {
    let at = m.index() + m.s(1).len();
    let source = m.input;
    js::slice(source, at, at + 4) == js::u("Гост") && INITIALS_BEFORE_GOST.exec_at(source, at).is_some()
}

static GOST_NUMBER: LazyLock<String> = LazyLock::new(|| format!(r"\d+(?:\.\d+|{NNBSP}\d{{3}})*"));

fn sup(p5: &[u16]) -> JsString {
    match p5 {
        [0x32] => cat!(SUP2),
        [0x33] => cat!(SUP3),
        _ => p5.to_vec(),
    }
}

fn abbreviated_currency(symbol: char) -> Replacement {
    Replacement::func(move |m: &Caps<'_>, _: &mut Ctx<'_>| {
        let head = cat!(m.s(1), NBSP, symbol);

        if let Some(end_tags) = m.get(3) {
            return Ok(cat!(head, end_tags, NBSP, m.s(4), " "));
        }

        let rest = js::from(m.input, m.end());
        let period = if m.s(2) == [0x2E] && ends_sentence(rest) { "." } else { "" };

        Ok(cat!(head, period))
    })
}

static BECAUSE_PATTERNS: LazyLock<[Re; 2]> = LazyLock::new(|| {
    let start = BLOCK_WRAPPER_START_GROUP.as_str();
    let end = CONTENT_END_LOOKAHEAD.as_str();
    [
        Re::new(&format!(r"{start}([Тт])\.[ \t{NBSP}]*([кно])\.{end}"), "gu"),
        Re::new(&format!(r"{start}([Ии])[ \t{NBSP}]+(др|пр)\.{end}"), "gu"),
    ]
});

static INITIAL_BEFORE_LABEL: LazyLock<Re> = LazyLock::new(|| {
    Re::new(
        &format!(
            r"(?<![\p{{L}}\d])(?<!\d{{4}}[-{MDASH}{MINUS}{NDASH}]\d{{4}}[ {NBSP}]?)[А-ЯЁ](?:{end}){{0,2}}\.(?:{end}){{0,2}}{gap}?$",
            end = *INLINE_END,
            gap = *LINE_GAP
        ),
        "u",
    )
});

static CAPITAL_LABEL_RE: LazyLock<Re> = LazyLock::new(|| Re::new("^[А-ЯЁ]$", "u"));
static LOCATION_LABEL_RE: LazyLock<Re> = LazyLock::new(|| Re::new("^[ДГШ]$", "u"));
static SURNAME_VALUE_RE: LazyLock<Re> = LazyLock::new(|| Re::new("^[А-ЯЁ][а-яё]+$", "u"));

fn keeps_gap(ctx: &mut Ctx<'_>, source: &[u16], label: &[u16], value: &[u16], at: usize, end: usize) -> Result<bool> {
    let from = at.saturating_sub(64);
    let previous = if CAPITAL_LABEL_RE.test(label) {
        INITIAL_BEFORE_LABEL.exec(&source[from..at]).map(|m| m.index())
    } else {
        None
    };
    if let Some(previous) = previous {
        if !LOCATION_LABEL_RE.test(label)
            || SURNAME_VALUE_RE.test(value)
            || !follows_complete_name(&mut ctx.reference_names(), source, from + previous)
        {
            return Ok(true);
        }
    } else if at > 0 && source[at - 1] == 0x2E {
        return Ok(true);
    }
    inside_canonical_nowrap(ctx, source, at, end)
}

fn bind_abbreviation(m: &Caps<'_>, ctx: &mut Ctx<'_>) -> Result<JsString> {
    let (lead, label, value) = (m.s(1), m.s(2), m.s(4));
    Ok(if keeps_gap(ctx, m.input, label, value, m.index() + lead.len(), m.end())? {
        m.whole().to_vec()
    } else {
        cat!(lead, label, ".", NBSP, value)
    })
}

fn bind_label(m: &Caps<'_>, ctx: &mut Ctx<'_>) -> Result<JsString> {
    let (label, value) = (m.s(1), m.s(3));
    Ok(if keeps_gap(ctx, m.input, label, value, m.index(), m.end() + value.len())? {
        m.whole().to_vec()
    } else {
        cat!(label, ".", NBSP)
    })
}

static ORGANIZATION_NUMBER_RE: LazyLock<Re> = LazyLock::new(|| Re::new(r"\+?\d", "uy"));

fn bind_organization(m: &Caps<'_>, ctx: &mut Ctx<'_>) -> Result<JsString> {
    let source = m.input;
    let at = m.end();
    if ctx.html
        && source.get(at) == Some(&0x3C)
        && let Some(body) = canonical_nowrap_content_start(ctx, source, at, true)?
        && ORGANIZATION_NUMBER_RE.exec_at(source, body).is_some()
    {
        return Ok(m.whole().to_vec());
    }
    Ok(cat!(m.s(1), NBSP))
}

fn bind_extended_organization(m: &Caps<'_>, ctx: &mut Ctx<'_>) -> Result<JsString> {
    let (source, lead, label) = (m.input, m.s(1), m.s(2));
    let held = if lead == [0x3E] { held_before_nowrap_end(ctx, source, m.index())? } else { None };
    if held.is_some_and(|held| !CONTENT_START_RE.test(&[held]))
        || inside_canonical_nowrap(ctx, source, m.index() + lead.len(), m.end())?
    {
        return Ok(m.whole().to_vec());
    }
    Ok(cat!(lead, label, NBSP))
}

fn wrap_phrase(phrase: &'static str) -> Replacement {
    Replacement::func(move |m: &Caps<'_>, ctx: &mut Ctx<'_>| {
        let (lead, end) = (m.s(1), m.s(3));
        let content = nowrap(ctx, &js::u(phrase), m.input, m.index() + lead.len(), m.end())?;
        Ok(cat!(lead, content, if end == [0x2E] { &[][..] } else { end }))
    })
}

fn wrap_abbreviation(tail: &'static str) -> Replacement {
    Replacement::func(move |m: &Caps<'_>, ctx: &mut Ctx<'_>| {
        let (lead, head) = (m.s(1), m.s(2));
        let body = cat!(head, ". ", tail);
        Ok(cat!(lead, nowrap(ctx, &body, m.input, m.index() + lead.len(), m.end())?))
    })
}

static SI_GAP_RE: LazyLock<Re> = LazyLock::new(|| Re::new(r"[ \t]+", "gu"));

pub fn abbr_group() -> RuleGroupDef {
    LazyLock::force(&INLINE_LEAD_RE);
    LazyLock::force(&SENTENCE_ENDS_RE);
    LazyLock::force(&CONTENT_START_RE);
    LazyLock::force(&GLUED_USA_RE);
    LazyLock::force(&CITY_AFTER_RE);
    LazyLock::force(&IE_TAIL_RE);
    LazyLock::force(&INITIALS_AHEAD_RE);
    LazyLock::force(&INITIALS_BEFORE_GOST);
    LazyLock::force(&INITIAL_BEFORE_LABEL);
    LazyLock::force(&CAPITAL_LABEL_RE);
    LazyLock::force(&LOCATION_LABEL_RE);
    LazyLock::force(&SURNAME_VALUE_RE);
    LazyLock::force(&ORGANIZATION_NUMBER_RE);
    LazyLock::force(&SI_GAP_RE);
    let start = CONTENT_START_GROUP.as_str();
    let content_start = CONTENT_START.as_str();
    let end = CONTENT_END_LOOKAHEAD.as_str();
    let label_end = LABEL_NUMBER_END.as_str();
    let wrapper = WRAPPER_START_GROUP.as_str();
    let dash = SENTENCE_DASH_TAIL.as_str();
    let amount = format!("{}{}", *CURRENCY_AMOUNT, *CURRENCY_GAP);
    let gost = GOST_NUMBER.as_str();
    let numeral_after =
        format!(r"(?<!(?:^|[^\p{{L}}\d])\d+(?:<\/{OPEN}[{TRANSPARENT_TAG_KINDS}]\d+{CLOSE}>)*[ \t{NBSP}]");
    let [because, because_list] = BECAUSE_PATTERNS.clone();

    super::group(
        "Сокращения",
        super::classes(super::NOWRAP_CLASSES),
        Vec::from([
            Rule::pattern(
                "nbsp_dpi",
                Re::new(&format!(r"{start}(\d+)( |\t)*(dpi|lpi){end}"), "giu"),
                Replacement::Template(cat!("$1$2", NBSP, "$4")),
            )
            .describe("Пробел перед сокращениями dpi, lpi").requires(F::DIGIT | F::LATIN),
            Rule::pattern(
                "nbsp_short_acronym",
                Re::new(&format!(r"{start}({DOCUMENT_LABEL})\.( |\t)*(\d+){label_end}"), "giu"),
                Replacement::func(bind_abbreviation),
            )
            .describe("Пробел перед сокращениями гл., стр., рис., илл., ст., п.").requires(F::DIGIT | F::PERIOD),
            Rule::pattern(
                "nbsp_reference",
                Re::new(
                    &format!(
                        r"(?<=^|[{content_start}])({numeral_after}?)см|им)\.((?:(?:см|им)\.)*)( |\t)*(?=[а-яё0-9a-z]+{end})"
                    ),
                    "giu",
                ),
                Replacement::func(|m, _| {
                    Ok(cat!(m.s(1), ".", NBSP, js::replace_all_lit(m.s(2), &[0x2E], &[0x2E, unit(NBSP)])))
                }),
            )
            .describe("Пробел перед сокращениями см., им.").requires(F::PERIOD),
            Rule::patterns(
                "nbsp_location",
                vec![
                    (
                        Re::new(
                            &format!(
                                r"(?<=(?:^|[^\p{{L}}\d])\d{{6}}[ \t{NBSP}]*){start}([гГ])\.( |\t)*([А-ЯЁA-Z][а-яёА-ЯЁa-zA-Z0-9]*){end}"
                            ),
                            "gu",
                        ),
                        Replacement::func(bind_abbreviation),
                    ),
                    (
                        Re::new(
                            &format!(
                                r"(?<=^|[.{content_start}])({numeral_after}*)г|{STREET_LABEL})\.( |\t)*(?=([а-яё0-9a-z]+){label_end})"
                            ),
                            "giu",
                        ),
                        Replacement::func(bind_label),
                    ),
                    (
                        Re::new(&format!(r"{start}(б-р|пр-кт)( |\t)*([а-яё0-9a-z]+){end}"), "giu"),
                        Replacement::Template(cat!("$1$2", NBSP, "$4")),
                    ),
                    (
                        Re::new(
                            &format!(r"(^|[.{content_start}])({BUILDING_LABEL})\.( |\t)*(\d+){label_end}"),
                            "giu",
                        ),
                        Replacement::func(bind_abbreviation),
                    ),
                ],
            )
            .describe("Пробелы в сокращениях г., ул., пер., д.").requires_any(F::DASH | F::PERIOD).requires_pattern(Re::new(r"(?:г|ул|пер|просп|пл|бул|наб|пр|ш|туп|д|кв|эт)\.|б-р|пр-кт", "iu")),
            Rule::pattern(
                "nbsp_currency_prefix",
                Re::new(&format!(r"([{}])\s?(\d)", *CURRENCY_PREFIX_SIGNS), "gu"),
                Replacement::Template(cat!("$1", NBSP, "$2")),
            )
            .describe("Привязка валюты к числу спереди: $100 → $ 100").requires(F::DIGIT | F::MONEY),
            Rule::patterns(
                "nbsp_unit",
                vec![
                    (unit_bind(LENGTH_UNITS, &UnitBind::default()), Replacement::func(bind_unit)),
                    (
                        Re::new(
                            &format!(r"{start}(\d+)( |{NBSP})?({LENGTH_UNITS})([32]|[{SUP2}{SUP3}]){end}"),
                            "giu",
                        ),
                        Replacement::func(|m, ctx| {
                            if unbound_number(m, ctx)? {
                                return Ok(m.whole().to_vec());
                            }
                            Ok(cat!(m.s(1), m.s(2), NBSP, m.s(4), sup(m.s(5))))
                        }),
                    ),
                    (unit_bind(SPEED_UNITS, &UnitBind::default()), Replacement::func(bind_unit)),
                ],
            )
            .describe("Привязка размерных сокращений (длина, скорость)").requires(F::DIGIT),
            Rule::pattern(
                "nbsp_fraction_unit",
                Re::new(&format!("([{FRAC12}{FRAC14}{FRAC34}])( )(кг|мг|км|мм|см|дм|гм|г|м|т)(?![а-яё])"), "giu"),
                Replacement::Template(cat!("$1", NBSP, "$3")),
            )
            .describe("Неразрывный пробел между дробью и единицей").requires(F::SPACE | F::FRACTION),
            Rule::pattern("nbsp_weight_unit", unit_bind(WEIGHT_UNITS, &UnitBind::default()), Replacement::func(bind_weight))
                .describe("Привязка весовых сокращений").requires(F::DIGIT),
            Rule::pattern("nbsp_volume_unit", unit_bind(VOLUME_UNITS, &UnitBind::default()), Replacement::func(bind_unit))
                .describe("Привязка мер объёма").requires(F::DIGIT),
            Rule::pattern("nbsp_time_unit", unit_bind(TIME_UNITS, &UnitBind::default()), Replacement::func(bind_unit))
                .describe("Привязка мер времени").requires(F::DIGIT),
            Rule::pattern(
                "nbsp_data_unit",
                unit_bind(DATA_UNITS, &UnitBind { flags: "gu", ..UnitBind::default() }),
                Replacement::func(bind_unit),
            )
            .describe("Неразрывный пробел перед единицами данных: 7 ГБ, 100 Мбит, 5 ТБ/с").requires(F::DIGIT),
            Rule::pattern(
                "nbsp_frequency_unit",
                unit_bind(FREQUENCY_UNITS, &UnitBind::decimal()),
                Replacement::func(bind_unit),
            )
            .describe("Неразрывный пробел перед частотами: 100 МГц, 2.4 ГГц").requires_any(F::DIGIT | F::PERIOD | F::COMMA).requires_pattern(Re::new("гц|hz", "iu")),
            Rule::pattern(
                "nbsp_css_unit",
                unit_bind(
                    "px|pt|pc|em|rem|vh|vw|vmin|vmax|vi|vb|svh|svw|lvh|lvw|dvh|dvw|ch|ex|cap|ic|lh|rlh|fr|deg|rad|grad|turn|s|ms",
                    &UnitBind::decimal(),
                ),
                Replacement::func(bind_unit),
            )
            .describe("Неразрывный пробел перед CSS-единицами: 16 px, 1.5 em").requires(F::LATIN).requires_any(F::DIGIT | F::PERIOD | F::COMMA),
            Rule::pattern(
                "nbsp_volt",
                VOLT_UNIT_RE.clone(),
                Replacement::func(|m, _| {
                    let letter = m.end() - 1;
                    Ok(
                        if phone_context_at(m.input, m.index()) == 1
                            || INITIALS_AHEAD_RE.test(&kept_glyphs_at(m.input, letter, 64))
                        {
                            m.whole().to_vec()
                        } else {
                            cat!(m.s(1), NBSP, "В")
                        },
                    )
                }),
            )
            .describe("Сокращение «вольт»").requires(F::DIGIT),
            Rule::pattern(
                "nbsp_magnitude",
                Re::new(&format!(r"(\d)({MAGNITUDE})(?![а-яё])"), "giu"),
                Replacement::Template(cat!("$1", NBSP, "$2")),
            )
            .describe("Неразрывный пробел между числом и величиной: 23тыс. → 23 тыс.").requires(F::DIGIT),
            Rule::pattern(
                "nowrap_postscript",
                Re::new(&format!(r"(^|[ \t{NBSP}>\r\n]){}([^<])", postscript(" ?")), "gu"),
                Replacement::func(|m, ctx| {
                    let second = match m.get(3) {
                        Some(p3) if !p3.is_empty() => cat!(js::trim(p3), " "),
                        _ => Vec::new(),
                    };
                    let inner = cat!(js::trim(m.s(2)), " ", second, m.s(4));
                    let start = m.index() + m.s(1).len();
                    let end = m.end() - m.s(5).len();
                    Ok(cat!(m.s(1), nowrap(ctx, &inner, m.input, start, end)?, m.s(5)))
                }),
            )
            .describe("P.S., P.P.S.").requires(F::PERIOD | F::LATIN),
            Rule::patterns(
                "nowrap_etc",
                vec![
                    (Re::new(&format!(r"{wrapper}и( |{NBSP})т\.?[ ]?д(\.|$|\s)"), "gu"), wrap_phrase("и т. д.")),
                    (Re::new(&format!(r"{wrapper}и( |{NBSP})т\.?[ ]?п(\.|$|\s)"), "gu"), wrap_phrase("и т. п.")),
                    (Re::new(&format!(r"{wrapper}в( |{NBSP})т\.?[ ]?ч(\.|$|\s)"), "gu"), wrap_phrase("в т. ч.")),
                    (
                        Re::new(&format!(r"(и{NBSP}т\.{NBSP}[дп]\.)[ \t]*(?=\d+{end})"), "gu"),
                        Replacement::Template(cat!("$1", NBSP)),
                    ),
                    (
                        Re::new(
                            &format!(
                                r"((?:и[ {NBSP}]т\.[ {NBSP}][дп]|в[ {NBSP}]т\.[ {NBSP}]ч)\.(?:{})*){}",
                                *INLINE_END, *DASH_IN_GAPS
                            ),
                            "gu",
                        ),
                        Replacement::Template(cat!("$1", SENTENCE_DASH.as_str())),
                    ),
                ],
            )
            .describe("и т. д., и т. п., в т. ч.")
            .cycled().requires(F::SPACE).requires_pattern(Re::new(r"[ив][ \u{00A0}]т", "u")),
            Rule::pattern(
                "nowrap_ie",
                Re::new(&format!(r"{wrapper}([тТ])\.?[ ]?е\."), "gu"),
                wrap_abbreviation("е."),
            )
            .describe("т. е.").requires(F::PERIOD),
            Rule::pattern(
                "nowrap_era",
                Re::new(&format!(r"{wrapper}(?<!(?:^|[^\p{{L}}])до )((?:до )?н)\.?[ ]?э\."), "gu"),
                wrap_abbreviation("э."),
            )
            .describe("до н. э. / н. э.").requires(F::PERIOD),
            Rule::pattern(
                "nbsp_honorific",
                Re::new(&format!("{start}([гГ])-(н|жа)( |{NBSP})"), "gu"),
                Replacement::Template(cat!("$1$2-$3", NBSP)),
            )
            .describe("г-н, г-жа").requires(F::SPACE | F::DASH),
            Rule::patterns(
                "currency",
                vec![
                    (
                        Re::new(
                            &format!("{amount}(?:рублями|рублях|рублям|рублей|рублём|рубле|рубля|рубль)(?![а-яёa-z])"),
                            "giu",
                        ),
                        Replacement::Template(cat!("$1", NBSP, RUB)),
                    ),
                    (
                        Re::new(&format!(r"{amount}(?:руб|р)(\.?)(?![а-яёa-z])(?:{dash})?"), "giu"),
                        abbreviated_currency(RUB),
                    ),
                    (
                        Re::new(
                            &format!(
                                "{amount}(?:долларами|долларах|долларам|долларов|долларе|доллару|доллара|доллар)(?![а-яёa-z])"
                            ),
                            "giu",
                        ),
                        Replacement::Template(cat!("$1", NBSP, "$")),
                    ),
                    (
                        USD_ABBREVIATION_RE.clone(),
                        Replacement::func(|m, _| Ok(cat!(m.s(1), NBSP, GLUED_USA_RE.replace_tpl(m.s(2), &js::u("$1. "))))),
                    ),
                    (
                        Re::new(
                            &format!(r"{amount}долл(?!{}(?![а-яёa-z]))(\.?)(?![а-яёa-z])(?:{dash})?", *USD_TAIL),
                            "giu",
                        ),
                        abbreviated_currency('$'),
                    ),
                    (
                        Re::new(&format!("{amount}евро(?![а-яёa-z])"), "giu"),
                        Replacement::Template(cat!("$1", NBSP, EURO)),
                    ),
                    (
                        Re::new(&format!(r"(?<!%\d{{0,3}}){amount}([{RUB}{EURO}$])"), "g"),
                        Replacement::Template(cat!("$1", NBSP, "$2")),
                    ),
                ],
            )
            .describe("Нормализация словесной валюты в символ + неразрывный пробел: 100 руб. → 100 ₽").requires(F::DIGIT).requires_pattern(Re::new(&format!("{}{}[рде{RUB}{EURO}$]", *CURRENCY_AMOUNT, *CURRENCY_GAP), "iu")),
            Rule::pattern(
                "nbsp_money_magnitude",
                Re::new(
                    &format!(
                        r"(\d)(( |{NBSP})?(тыс|млн|млрд)\.?( |{NBSP})?)?( |{NBSP})?{}\.?(?=\s|[!?]|$)",
                        currency_unit(" ?")
                    ),
                    "giu",
                ),
                Replacement::func(|m, _| {
                    let mid = match m.get(4) {
                        Some(p4) if !p4.is_empty() => cat!(NBSP, p4, if p4 == js::u("тыс") { "." } else { "" }),
                        _ => Vec::new(),
                    };
                    Ok(cat!(m.s(1), mid, NBSP, "у.е."))
                }),
            )
            .describe("Денежные сокращения с тыс/млн/млрд + у.е.").requires(F::DIGIT),
            Rule::pattern(
                "nbsp_organization",
                Re::new(&format!(r#"(?<![a-zA-Zа-яёА-ЯЁ])({ORGANIZATION}) (?=[a-zA-Zа-яёА-ЯЁ"{LAQUO}{BDQUO}<])"#), "gu"),
                Replacement::func(bind_organization),
            )
            .describe("Привязка форм собственности (ООО, ЗАО, …)").requires(F::SPACE),
            Rule::patterns(
                "nowrap_gost",
                vec![
                    (
                        Re::new(
                            &format!(
                                r"(\s|^)ГОСТ( |{NBSP})?({gost})(([-{MINUS}{MDASH}{NDASH}])(\d+))?(( |{NBSP})(-(?!->)|{MDASH}))?"
                            ),
                            "giu",
                        ),
                        Replacement::func(|m, ctx| {
                            if is_surname_gost(m) {
                                return Ok(m.whole().to_vec());
                            }
                            let range = match m.get(6) {
                                Some(p6) if !p6.is_empty() => cat!(NDASH, p6),
                                _ => Vec::new(),
                            };
                            let tail = if m.get(7).is_some_and(|p7| !p7.is_empty()) { cat!(" ", MDASH) } else { Vec::new() };
                            let inner = cat!("ГОСТ ", m.s(3), range, tail);
                            let start = m.index() + m.s(1).len();
                            Ok(cat!(m.s(1), nowrap(ctx, &inner, m.input, start, m.end())?))
                        }),
                    ),
                    (
                        Re::new(&format!(r"(\s|^|>)ГОСТ( |{NBSP})?({gost})([-{MINUS}{MDASH}{NDASH}])(\d+)"), "giu"),
                        Replacement::func(|m, ctx| {
                            if is_surname_gost(m) {
                                return Ok(m.whole().to_vec());
                            }
                            let inner = cat!("ГОСТ ", m.s(3), NDASH, m.s(5));
                            let start = m.index() + m.s(1).len();
                            Ok(cat!(m.s(1), if ctx.html { inner } else { nowrap(ctx, &inner, m.input, start, m.end())? }))
                        }),
                    ),
                ],
            )
            .describe("ГОСТ + номер (case-insensitive — нормализуем регистр)").requires(F::DIGIT).requires_pattern(Re::new("гост", "iu")),
            Rule::pattern(
                "nbsp_organization_extended",
                Re::new(
                    &format!(
                        r#"{start}({OTHER_ORGANIZATION})[ \t{NBSP}]+(?=[А-ЯЁа-яёA-Za-z"{LAQUO}{BDQUO}])"#
                    ),
                    "gu",
                ),
                Replacement::func(bind_extended_organization),
            )
            .describe("Привязка дополнительных форм организаций к названию").requires(F::SPACE),
            Rule::handler("nowrap_because", move |ctx| {
                crate::shared::indefinite_pronoun::nowrap_with_indefinite_colon(ctx, &because, |m| {
                    cat!(m.s(2), ". ", m.s(3), ".")
                })?;
                crate::shared::indefinite_pronoun::nowrap_with_indefinite_colon(ctx, &because_list, |m| {
                    cat!(m.s(2), " ", m.s(3), ".")
                })?;
                Ok(None)
            })
            .describe("Неразрывные сокращения т. к., т. н., т. о., и др., и пр.").requires(F::PERIOD),
            Rule::pattern(
                "nbsp_part",
                Re::new(
                    &format!(
                        r"{start}({DOCUMENT_PART})\.[ \t{NBSP}]+(\d+(?:{NNBSP}\d{{3}})*)(?![\d{NNBSP}]){end}"
                    ),
                    "gu",
                ),
                Replacement::Template(cat!("$1$2.", NBSP, "$3")),
            )
            .describe("Привязка сокращения части документа к номеру").requires(F::DIGIT | F::PERIOD | F::SPACE),
            Rule::pattern(
                "nbsp_title",
                Re::new(
                    &format!(
                        r"{start}((?:{TITLE})\.|[Дд]-р)[ \t{NBSP}]+(?=[А-ЯЁ][а-яёА-ЯЁ-]*{end})"
                    ),
                    "gu",
                ),
                Replacement::Template(cat!("$1$2", NBSP)),
            )
            .describe("Привязка титула к слову с заглавной буквы").requires(F::SPACE).requires_any(F::PERIOD | F::DASH),
            Rule::pattern(
                "nbsp_currency_code",
                Re::new(
                    &format!(r"{start}(?<!{NNBSP})(\d+(?:[.,]\d+)?)[ \t{NBSP}]+({CURRENCY_CODE}){end}"),
                    "gu",
                ),
                Replacement::func(bind_counted),
            )
            .describe("Привязка кодов USD, EUR, RUB к числу").requires(F::DIGIT | F::SPACE | F::LATIN),
            Rule::pattern(
                "nbsp_si_unit",
                Re::new(
                    &format!(
                        r"{start}(?<!{NNBSP})(\d+(?:[.,]\d+)?)[ \t{NBSP}]+(кВт·ч|кВт|Вт|А|га|‰|кв\.[ \t{NBSP}]+м|куб\.[ \t{NBSP}]+м|{}){end}",
                        horsepower(&format!("[ \\t{NBSP}]+"))
                    ),
                    "gu",
                ),
                Replacement::func(|m, ctx| {
                    if unbound_number(m, ctx)? {
                        return Ok(m.whole().to_vec());
                    }
                    Ok(cat!(m.s(1), m.s(2), NBSP, SI_GAP_RE.replace_tpl(m.s(3), &[unit(NBSP)])))
                }),
            )
            .describe("Привязка дополнительных единиц и составных сокращений").requires(F::DIGIT | F::SPACE),
            Rule::pattern(
                "nbsp_count_unit",
                Re::new(
                    &format!(
                        r"{start}(\d+(?:[.,]\d+)?)[ \t{NBSP}]+(шт\.|экз\.|чел\.|сек|с(?![ \t{NBSP}]+(?:<{OPEN}[{TRANSPARENT_TAG_KINDS}]\d{{1,9}}{CLOSE}>)*[А-ЯЁа-яё])|мес\.|нед\.|дн\.|коп\.){end}"
                    ),
                    "gu",
                ),
                Replacement::func(bind_counted),
            )
            .describe("Привязка счётных сокращений к числу").requires(F::DIGIT | F::SPACE),
            Rule::patterns(
                "nbsp_magnitude_spaced",
                Vec::from([
                    (
                        Re::new(
                            &format!(
                                r"{start}(\d+(?:[.,]\d+)?)[ \t{NBSP}]+(тыс\.|млн|млрд|трлн)(?:{end}|(?<=\.)(?=(?:{MAGNITUDE_WORD}){end}))"
                            ),
                            "gu",
                        ),
                        Replacement::func(|m, ctx| {
                            Ok(if unbound_number(m, ctx)? {
                                m.whole().to_vec()
                            } else {
                                cat!(m.s(1), m.s(2), NBSP, m.s(3))
                            })
                        }),
                    ),
                    (
                        Re::new(
                            &format!(
                                r"(\d{NBSP}(?:тыс\.|млн|млрд|трлн))(?:[ \t{NBSP}]+(руб\.|чел\.|шт\.|экз\.|{MAGNITUDE_WORD})|(?<=\.)({MAGNITUDE_WORD})){end}"
                            ),
                            "gu",
                        ),
                        Replacement::func(|m, _| {
                            Ok(if phone_context_at(m.input, m.index()) == 1 {
                                m.whole().to_vec()
                            } else {
                                cat!(m.s(1), NBSP, m.get(2).or(m.get(3)))
                            })
                        }),
                    ),
                ]),
            )
            .describe("Привязка числа к разрядному сокращению и валюте или счётной единице").requires(F::DIGIT | F::SPACE),
        ]),
    )
}
