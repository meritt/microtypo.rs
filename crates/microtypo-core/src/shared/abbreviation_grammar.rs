use std::sync::LazyLock;

use crate::js::Re;
use crate::protect::placeholders::vaulted_reference;
use crate::protect::tags::SafeTags;
use crate::shared::boundaries::CONTENT_END_LOOKAHEAD;
use crate::shared::character_reference::kept_reference_to;
use crate::shared::glyphs::{DEG, FRACTION_GLYPHS, MDASH, NBSP, NDASH};
use crate::shared::inline_boundaries::inline_text_end;
use crate::shared::measurement_units::{
    DATA_UNITS, FREQUENCY_UNITS, LENGTH_UNITS, SPEED_UNITS, TEMPERATURE_SCALES, TIME_UNITS, VOLUME_UNITS, WEIGHT_UNITS,
};

pub const INITIAL_GAP: &str = r"\s[ \t\u{00A0}]*";

pub fn dotted_initials(gap: &str, letter_end: &str) -> String {
    format!(r"([А-ЯЁ]){letter_end}\.({gap})?([А-ЯЁ])\.")
}

pub static DOTTED_INITIALS: LazyLock<String> = LazyLock::new(|| dotted_initials(INITIAL_GAP, ""));
pub static DOTTED_INITIAL_PAIR: LazyLock<String> = LazyLock::new(|| format!("{}({INITIAL_GAP})?", *DOTTED_INITIALS));
pub const SURNAME: &str = "[А-ЯЁ][а-яё]+";

static COMPACT_NAME_RE: LazyLock<Re> = LazyLock::new(|| {
    Re::new(
        &format!(r"(?:[А-ЯЁ]{{2}}{INITIAL_GAP}{SURNAME}|{SURNAME}{INITIAL_GAP}[А-ЯЁ]{{2}})(?=\s|$|[.,;:?!)\]»“<])"),
        "uy",
    )
});

pub fn starts_with_compact_name(text: &[u16], at: usize) -> bool {
    COMPACT_NAME_RE.exec_at(text, at).is_some()
}

static NAME_GAP: LazyLock<String> = LazyLock::new(|| format!(r"(?:[^\S\r\n]|{})", kept_reference_to(&[NBSP])));
static NAME_GAP_END: LazyLock<Re> = LazyLock::new(|| Re::new(&format!("(?<=({}+))", *NAME_GAP), "uy"));
static SURNAME_END: LazyLock<Re> = LazyLock::new(|| Re::new(&format!("(?<=({SURNAME}))"), "uy"));
static INITIALS_END: LazyLock<Re> = LazyLock::new(|| {
    Re::new(&format!(r"(?<=(?:^|[\s.,;:?!>]){})", dotted_initials(&format!("{}+", *NAME_GAP), "")), "uy")
});

fn before_name_gap(names: &mut Option<&mut SafeTags>, source: &[u16], at: usize) -> usize {
    let end = inline_text_end(names.as_deref_mut(), source, at);
    match NAME_GAP_END.exec_at(source, end) {
        Some(gap) => inline_text_end(names.as_deref_mut(), source, end - gap.s(1).len()),
        None => end,
    }
}

pub fn follows_complete_name(names: &mut Option<&mut SafeTags>, source: &[u16], at: usize) -> bool {
    let end = before_name_gap(names, source, at);
    let Some(surname) = SURNAME_END.exec_at(source, end) else { return false };
    let start = before_name_gap(names, source, end - surname.s(1).len());
    INITIALS_END.exec_at(source, start).is_some()
}

static NUMBER_SET_APART: LazyLock<String> = LazyLock::new(|| {
    format!(
        r"(?:(?={DEG}(?:{TEMPERATURE_SCALES}){})(?<=(?<![\p{{L}}\d])\d+)|(?=г\.(?:[^a-zа-яё]|$))(?<=(?<![\p{{L}}\d])\d{{3,4}}))",
        *CONTENT_END_LOOKAHEAD
    )
});
pub static LABEL_NUMBER_END: LazyLock<String> =
    LazyLock::new(|| format!("(?:{}|{})", *CONTENT_END_LOOKAHEAD, *NUMBER_SET_APART));
static DOTTED_LOCATION_AHEAD: LazyLock<Re> = LazyLock::new(|| {
    let gap = &*NAME_GAP;
    Re::new(
        &format!(r"\.{gap}*(?:[Дд]\.{gap}*\d+|[ГгШш]\.{gap}*[а-яёa-z0-9][А-ЯЁа-яёA-Za-z0-9]*){}", *LABEL_NUMBER_END),
        "uy",
    )
});

pub fn has_dotted_location_after(source: &[u16], at: usize) -> bool {
    DOTTED_LOCATION_AHEAD.exec_at(source, at).is_some()
}

pub fn century_range(dash: &str) -> String {
    format!("([XIV]{{1,5}})({dash})([XIV]{{1,5}})")
}

pub static CENTURY_RANGE: LazyLock<String> = LazyLock::new(|| century_range("[-—–]"));

pub fn century_marker(gap: &str) -> String {
    format!(r"(({gap})?((?:в\.в\.|вв\.|в\.)(?![а-яёa-z])|(?:вв|в)(?![а-яёА-ЯЁa-zA-Z])))")
}

pub static CENTURY_MARKER: LazyLock<String> = LazyLock::new(|| century_marker(r" |\u{00A0}"));

pub const DOCUMENT_LABEL: &str = "гл|стр|рис|илл?|ст|п|с";
pub const DOCUMENT_PART: &str = "т|ч|[Тт]абл|[Рр]азд|пп|стст";
pub const STREET_LABEL: &str = "ул|пер|просп|пл|бул|наб|пр|ш|туп";
pub const BUILDING_LABEL: &str = "д|кв|эт";
pub const TITLE: &str = "[Пп]роф|[Аа]кад|[Дд]оц|[Тт]ов|[Сс]в";
pub const CURRENCY_CODE: &str = "USD|EUR|RUB";
pub const MAGNITUDE: &str = "млрд|трлн|млн|тыс";
pub const ORGANIZATION: &str = "ООО|ЗАО|ОАО|НИИ|ПБОЮЛ";
pub const OTHER_ORGANIZATION: &str = "ПАО|АО|НКО|ИП|ФГУП|МУП|ГК|НПО|АНО";

pub fn postscript(gap: &str) -> String {
    format!(r"([Pp]\.{gap})([Pp]\.{gap})?([Ss]\.)")
}

pub fn horsepower(gap: &str) -> String {
    format!(r"л\.{gap}с\.")
}

pub fn currency_unit(gap: &str) -> String {
    format!(r"у\.?{gap}е")
}

pub fn abbreviation_period(inline_end: &str) -> Re {
    let nbsp = vaulted_reference(NBSP);
    let gap = format!(r"(?:\s|{nbsp})");
    let tag = format!("(?:{inline_end})?");
    let word_start = r"(?<![\p{L}\d])";
    let labels = [
        DOCUMENT_LABEL,
        DOCUMENT_PART,
        STREET_LABEL,
        BUILDING_LABEL,
        TITLE,
        ORGANIZATION,
        OTHER_ORGANIZATION,
        "см|им|[гГ]-(?:н|жа)|проц",
    ]
    .join("|");
    let units = [
        LENGTH_UNITS,
        SPEED_UNITS,
        WEIGHT_UNITS,
        VOLUME_UNITS,
        TIME_UNITS,
        DATA_UNITS,
        FREQUENCY_UNITS,
        CURRENCY_CODE,
        MAGNITUDE,
        "сек|шт|экз|чел|мес|нед|дн|коп|руб|р|долл|евро|В|кВт|Вт|га",
    ]
    .join("|");
    let number = format!(r"[\d{}]{tag}{gap}?", *FRACTION_GLYPHS);
    let forms = [
        format!(
            r"(?:[\s(]|^|{nbsp}){}{}",
            century_range(&format!("[-{MDASH}{NDASH}]|{}", vaulted_reference(NDASH))),
            century_marker(&format!(" |{NBSP}|{nbsp}"))
        ),
        format!(r"(?:\s|^|[.,;:?!]){SURNAME}{tag}{gap}{}", dotted_initials(&gap, &tag)),
        format!(r"{word_start}(?:[Тт]{tag}\.{gap}?[дпчекно]|н{tag}\.{gap}?э|[Ии]{tag}{gap}(?:др|пр))\."),
        format!(r"(?:\d(?:-[а-яё]{{1,3}})?|{word_start}[IVXLCDM]{{1,7}}){tag}{gap}?(?:гг?|вв?)\."),
        format!(r"{number}(?:(?:{MAGNITUDE})\.?{gap}?)?(?:{units}|{})\.", currency_unit(&format!("{gap}?"))),
        format!(r"{number}(?:{}|(?:кв|куб)\.{gap}+м\.)", horsepower(&format!("{gap}+"))),
        format!(r"{word_start}(?:{labels})\."),
        format!("{word_start}{}", postscript(&format!("{gap}?"))),
    ];
    Re::new(&format!("(?<={})", forms.join("|")), "uy")
}
