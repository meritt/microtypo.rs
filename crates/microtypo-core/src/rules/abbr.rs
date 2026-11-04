use std::sync::LazyLock;

use crate::cat;
use crate::errors::Result;
use crate::js::{self, Caps, JsString, Re};
use crate::protect::placeholders::{CLOSE, OPEN, TRANSPARENT_BARE_KINDS, TRANSPARENT_TAG_KINDS};
use crate::rule_runner::{Ctx, Replacement, Rule, RuleGroupDef};
use crate::shared::boundaries::{CONTENT_END_LOOKAHEAD, CONTENT_START_GROUP, WRAPPER_START_GROUP};
use crate::shared::glyphs::{BDQUO, EURO, FRAC12, FRAC14, FRAC34, LAQUO, MDASH, NBSP, NDASH, RUB, SUP2, SUP3};

use super::dash::SENTENCE_DASH;
use super::nowrap;

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
    SENTENCE_ENDS_RE.test(&INLINE_LEAD_RE.replace_tpl(rest, &[]))
}

fn unit_bind(units: &str, number: &str, flags: &str) -> Re {
    Re::new(&format!("{}{number}( |{NBSP})?({units}){}", *CONTENT_START_GROUP, *CONTENT_END_LOOKAHEAD), flags)
}

fn unit_bind_default(units: &str) -> Re {
    unit_bind(units, r"(\d+)", "giu")
}

fn bind_unit() -> Replacement {
    Replacement::Template(cat!("$1$2", NBSP, "$4"))
}

const DECIMAL_NUMBER: &str = r"(?<![\d.,])([\d.,]+)";
const DECIMAL_FLAGS: &str = "gu";

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

        if m.get(3).is_some_and(|t| !t.is_empty()) {
            return Ok(cat!(head, NBSP, MDASH, " "));
        }

        let rest = js::from(m.input, m.end());
        let period = if m.s(2) == [0x2E] && ends_sentence(rest) { "." } else { "" };

        Ok(cat!(head, period))
    })
}

fn nowrap_phrase(phrase: &'static str) -> Replacement {
    Replacement::func(move |m: &Caps<'_>, ctx: &mut Ctx<'_>| {
        let tail = m.s(3);
        Ok(cat!(m.s(1), nowrap(ctx, &js::u(phrase))?, if tail == [0x2E] { &[][..] } else { tail }))
    })
}

fn gost_inner(ctx: &mut Ctx<'_>, m: &Caps<'_>) -> Result<JsString> {
    let range = match m.get(6) {
        Some(p6) if !p6.is_empty() => cat!(NDASH, p6),
        _ => Vec::new(),
    };
    let dash = if m.get(7).is_some_and(|p| !p.is_empty()) { cat!(" ", MDASH) } else { Vec::new() };
    let inner = cat!("ГОСТ ", m.s(3), range, dash);
    Ok(cat!(m.s(1), nowrap(ctx, &inner)?))
}

pub fn abbr_group() -> RuleGroupDef {
    let start = CONTENT_START_GROUP.as_str();
    let end = CONTENT_END_LOOKAHEAD.as_str();
    let wrapper = WRAPPER_START_GROUP.as_str();
    let dash = SENTENCE_DASH.as_str();

    super::group(
        "Сокращения",
        super::classes(super::NOWRAP_CLASSES),
        vec![
            Rule::pattern(
                "nbsp_dpi",
                Re::new(&format!(r"{start}(\d+)( |\t)*(dpi|lpi){end}"), "giu"),
                Replacement::Template(cat!("$1$2", NBSP, "$4")),
            )
            .describe("Пробел перед сокращениями dpi, lpi"),
            Rule::pattern(
                "nbsp_short_acronym",
                Re::new(&format!(r"{start}(гл|стр|рис|илл?|ст|п|с)\.( |\t)*(\d+){end}"), "giu"),
                Replacement::Template(cat!("$1$2.", NBSP, "$4")),
            )
            .describe("Пробел перед сокращениями гл., стр., рис., илл., ст., п."),
            Rule::pattern(
                "nbsp_reference",
                Re::new(&format!(r"{start}(см|им)\.( |\t)*([а-яё0-9a-z]+){end}"), "giu"),
                Replacement::Template(cat!("$1$2.", NBSP, "$4")),
            )
            .describe("Пробел перед сокращениями см., им."),
            Rule::patterns(
                "nbsp_location",
                vec![
                    (
                        Re::new(
                            &format!(r"{start}(г|ул|пер|просп|пл|бул|наб|пр|ш|туп)\.( |\t)*([а-яё0-9a-z]+){end}"),
                            "giu",
                        ),
                        Replacement::Template(cat!("$1$2.", NBSP, "$4")),
                    ),
                    (
                        Re::new(&format!(r"{start}(б-р|пр-кт)( |\t)*([а-яё0-9a-z]+){end}"), "giu"),
                        Replacement::Template(cat!("$1$2", NBSP, "$4")),
                    ),
                    (
                        Re::new(&format!(r"{start}(д|кв|эт)\.( |\t)*(\d+){end}"), "giu"),
                        Replacement::Template(cat!("$1$2.", NBSP, "$4")),
                    ),
                ],
            )
            .describe("Пробелы в сокращениях г., ул., пер., д."),
            Rule::patterns(
                "nbsp_unit",
                vec![
                    (unit_bind_default("м|мм|см|дм|км|гм|km|dm|cm|mm"), bind_unit()),
                    (
                        Re::new(
                            &format!(r"{start}(\d+)( |{NBSP})?(м|мм|см|дм|км|гм|km|dm|cm|mm)([32]|[{SUP2}{SUP3}]){end}"),
                            "giu",
                        ),
                        Replacement::func(|m, _| Ok(cat!(m.s(1), m.s(2), NBSP, m.s(4), sup(m.s(5))))),
                    ),
                    (unit_bind_default(r"(?:мм|см|дм|км|гм|м)\/(?:с|ч|мин|сут)"), bind_unit()),
                ],
            )
            .describe("Привязка размерных сокращений (длина, скорость)"),
            Rule::pattern(
                "nbsp_fraction_unit",
                Re::new(&format!("([{FRAC12}{FRAC14}{FRAC34}])( )(кг|мг|км|мм|см|дм|гм|г|м|т)(?![а-яё])"), "giu"),
                Replacement::Template(cat!("$1", NBSP, "$3")),
            )
            .describe("Неразрывный пробел между дробью и единицей"),
            Rule::pattern("nbsp_weight_unit", unit_bind_default("г|кг|мг|т"), bind_unit()).describe("Привязка весовых сокращений"),
            Rule::pattern("nbsp_volume_unit", unit_bind_default("мл|л"), bind_unit()).describe("Привязка мер объёма"),
            Rule::pattern("nbsp_time_unit", unit_bind_default("мин|ч"), bind_unit()).describe("Привязка мер времени"),
            Rule::pattern(
                "nbsp_data_unit",
                unit_bind(
                    r"(?:Кбит|кбит|Мбит|мбит|Гбит|гбит|Тбит|kbit|Mbit|Gbit|Tbit|Kbps|Mbps|Gbps|kbps|mbps|gbps|KiB|MiB|GiB|TiB|кБ|КБ|МБ|Мб|ГБ|Гб|ТБ|Тб|ПБ|kB|KB|MB|GB|TB|PB|бит|Б|B)(?:\/[сs])?",
                    r"(\d+)",
                    "gu",
                ),
                bind_unit(),
            )
            .describe("Неразрывный пробел перед единицами данных: 7 ГБ, 100 Мбит, 5 ТБ/с"),
            Rule::pattern(
                "nbsp_frequency_unit",
                unit_bind("(?:к|М|Г|Т)?Гц|(?:k|M|G|T)?Hz", DECIMAL_NUMBER, DECIMAL_FLAGS),
                bind_unit(),
            )
            .describe("Неразрывный пробел перед частотами: 100 МГц, 2.4 ГГц"),
            Rule::pattern(
                "nbsp_css_unit",
                unit_bind(
                    "px|pt|pc|em|rem|vh|vw|vmin|vmax|vi|vb|svh|svw|lvh|lvw|dvh|dvw|ch|ex|cap|ic|lh|rlh|fr|deg|rad|grad|turn|s|ms",
                    DECIMAL_NUMBER,
                    DECIMAL_FLAGS,
                ),
                bind_unit(),
            )
            .describe("Неразрывный пробел перед CSS-единицами: 16 px, 1.5 em"),
            Rule::pattern(
                "nbsp_volt",
                Re::new(&format!(r"(?<!\d)(\d+)([вВ]| В){end}"), "gu"),
                Replacement::Template(cat!("$1", NBSP, "В")),
            )
            .describe("Сокращение «вольт»"),
            Rule::pattern(
                "nbsp_magnitude",
                Re::new(r"(\d)(млрд|трлн|млн|тыс)(?![а-яё])", "giu"),
                Replacement::Template(cat!("$1", NBSP, "$2")),
            )
            .describe("Неразрывный пробел между числом и величиной: 23тыс. → 23 тыс."),
            Rule::pattern(
                "nowrap_postscript",
                Re::new(r"(^| |\t|>|\r|\n)(p\. ?)(p\. ?)?(s\.)([^<])", "gi"),
                Replacement::func(|m, ctx| {
                    let second = match m.get(3) {
                        Some(p3) if !p3.is_empty() => cat!(js::trim(p3), " "),
                        _ => Vec::new(),
                    };
                    let inner = cat!(js::trim(m.s(2)), " ", second, m.s(4));
                    Ok(cat!(m.s(1), nowrap(ctx, &inner)?, m.s(5)))
                }),
            )
            .describe("P.S., P.P.S."),
            Rule::patterns(
                "nowrap_etc",
                vec![
                    (Re::new(&format!(r"{wrapper}и( |{NBSP})т\.?[ ]?д(\.|$|\s)"), "gu"), nowrap_phrase("и т. д.")),
                    (Re::new(&format!(r"{wrapper}и( |{NBSP})т\.?[ ]?п(\.|$|\s)"), "gu"), nowrap_phrase("и т. п.")),
                    (Re::new(&format!(r"{wrapper}в( |{NBSP})т\.?[ ]?ч(\.|$|\s)"), "gu"), nowrap_phrase("в т. ч.")),
                ],
            )
            .describe("и т. д., и т. п., в т. ч.")
            .cycled(),
            Rule::pattern(
                "nowrap_ie",
                Re::new(&format!(r"{wrapper}([тТ])\.?[ ]?е\."), "gu"),
                Replacement::func(|m, ctx| Ok(cat!(m.s(1), nowrap(ctx, &cat!(m.s(2), ". е."))?))),
            )
            .describe("т. е."),
            Rule::pattern(
                "nowrap_era",
                Re::new(&format!(r"{wrapper}((?:до )?н)\.?[ ]?э\."), "gu"),
                Replacement::func(|m, ctx| Ok(cat!(m.s(1), nowrap(ctx, &cat!(m.s(2), ". э."))?))),
            )
            .describe("до н. э. / н. э."),
            Rule::pattern(
                "nbsp_honorific",
                Re::new(&format!("{start}([гГ])-(н|жа)( |{NBSP})"), "gu"),
                Replacement::Template(cat!("$1$2-$3", NBSP)),
            )
            .describe("г-н, г-жа"),
            Rule::patterns(
                "currency",
                vec![
                    (
                        Re::new(r"(\d) ?(?:рублями|рублях|рублям|рублей|рублём|рубле|рубля|рубль)(?![а-яёa-z])", "giu"),
                        Replacement::Template(cat!("$1", NBSP, RUB)),
                    ),
                    (Re::new(&format!(r"(\d) ?(?:руб|р)(\.?)(?![а-яёa-z])({dash})?"), "giu"), abbreviated_currency(RUB)),
                    (
                        Re::new(
                            r"(\d) ?(?:долларами|долларах|долларам|долларов|долларе|доллару|доллара|доллар)(?![а-яёa-z])",
                            "giu",
                        ),
                        Replacement::Template(cat!("$1", NBSP, "$")),
                    ),
                    (Re::new(&format!(r"(\d) ?долл(\.?)(?![а-яёa-z])({dash})?"), "giu"), abbreviated_currency('$')),
                    (Re::new(r"(\d) ?евро(?![а-яёa-z])", "giu"), Replacement::Template(cat!("$1", NBSP, EURO))),
                    (Re::new(r"(\d) ?([₽€$])", "g"), Replacement::Template(cat!("$1", NBSP, "$2"))),
                ],
            )
            .describe("Нормализация словесной валюты в символ + неразрывный пробел: 100 руб. → 100 ₽"),
            Rule::pattern(
                "nbsp_money_magnitude",
                Re::new(r"(\d)(( |\u{00A0})?(тыс|млн|млрд)\.?( |\u{00A0})?)?( |\u{00A0})?у\.? ?е\.?(?=\s|$)", "giu"),
                Replacement::func(|m, _| {
                    let mid = match m.get(4) {
                        Some(p4) if !p4.is_empty() => cat!(NBSP, p4, if p4 == js::u("тыс") { "." } else { "" }),
                        _ => Vec::new(),
                    };
                    Ok(cat!(m.s(1), mid, NBSP, "у.е."))
                }),
            )
            .describe("Денежные сокращения с тыс/млн/млрд + у.е."),
            Rule::pattern(
                "nbsp_currency_prefix",
                Re::new(r"(€|\$|₽)\s?(\d)", "gu"),
                Replacement::Template(cat!("$1", NBSP, "$2")),
            )
            .describe("Привязка валюты к числу спереди: $100 → $ 100"),
            Rule::pattern(
                "nbsp_organization",
                Re::new(r#"([^a-zA-Zа-яёА-ЯЁ]|^)(ООО|ЗАО|ОАО|НИИ|ПБОЮЛ) ([a-zA-Zа-яёА-ЯЁ]|"|«|„|<)"#, "gu"),
                Replacement::Template(cat!("$1$2", NBSP, "$3")),
            )
            .describe("Привязка форм собственности (ООО, ЗАО, …)"),
            Rule::patterns(
                "nowrap_gost",
                vec![
                    (
                        Re::new(r"( |\t|\u{00A0}|^)ГОСТ( |\u{00A0})?(\d+(?:\.\d+)*)(([-−—–])(\d+))?(( |\u{00A0})([-—]))?", "giu"),
                        Replacement::func(|m, ctx| gost_inner(ctx, m)),
                    ),
                    (
                        Re::new(r"( |\t|\u{00A0}|^|>)ГОСТ( |\u{00A0})?(\d+(?:\.\d+)*)([-−—–])(\d+)", "giu"),
                        Replacement::func(|m, _| Ok(cat!(m.s(1), "ГОСТ ", m.s(3), NDASH, m.s(5)))),
                    ),
                ],
            )
            .describe("ГОСТ + номер (case-insensitive — нормализуем регистр)"),
        ],
    )
}
