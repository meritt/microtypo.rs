use std::sync::LazyLock;

use crate::js::Re;
use crate::shared::boundaries::{CONTENT_END_LOOKAHEAD, CONTENT_START_GROUP};
use crate::shared::glyphs::{EURO, NBSP, RUB};

pub const LENGTH_UNITS: &str = "м|мм|см|дм|км|гм|km|dm|cm|mm";
pub const SPEED_UNITS: &str = r"(?:мм|см|дм|км|гм|м)\/(?:с|ч|мин|сут)";
pub const WEIGHT_UNITS: &str = "г|кг|мг|т";
pub const VOLUME_UNITS: &str = "мл|л";
pub const TIME_UNITS: &str = "мин|ч";
pub const DATA_UNITS: &str = r"(?:Кбит|кбит|Мбит|мбит|Гбит|гбит|Тбит|kbit|Mbit|Gbit|Tbit|Kbps|Mbps|Gbps|kbps|mbps|gbps|KiB|MiB|GiB|TiB|кБ|КБ|МБ|Мб|ГБ|Гб|ТБ|Тб|ПБ|kB|KB|MB|GB|TB|PB|бит|Б|B)(?:\/[сs])?";
pub const FREQUENCY_UNITS: &str = "(?:к|М|Г|Т)?Гц|(?:k|M|G|T)?Hz";
pub const TEMPERATURE_SCALES: &str = "C|С|F";
pub static CURRENCY_PREFIX_SIGNS: LazyLock<String> = LazyLock::new(|| format!("{EURO}${RUB}"));

pub const DECIMAL_NUMBER: &str = r"(?<![\d.,])((?=[.,]*\d)[\d.,]+)";

fn volt_bind_source(gap: &str) -> String {
    format!(r"(?<!\d)(\d+)([вВ]|{gap}В){}", *CONTENT_END_LOOKAHEAD)
}

pub static VOLT_UNIT_RE: LazyLock<Re> = LazyLock::new(|| Re::new(&volt_bind_source(&format!("[ {NBSP}]")), "gu"));

#[derive(Debug, Clone)]
pub struct UnitBind<'a> {
    pub number: &'a str,
    pub flags: &'a str,
    pub gap: String,
}

impl Default for UnitBind<'_> {
    fn default() -> Self {
        UnitBind { number: r"(\d+)", flags: "giu", gap: format!("( |{NBSP})?") }
    }
}

impl UnitBind<'_> {
    pub fn decimal() -> Self {
        UnitBind { number: DECIMAL_NUMBER, flags: "gu", ..UnitBind::default() }
    }
}

pub fn unit_bind_source(units: &str, bind: &UnitBind<'_>) -> String {
    format!("{}{}{}({units}){}", *CONTENT_START_GROUP, bind.number, bind.gap, *CONTENT_END_LOOKAHEAD)
}

pub fn unit_bind(units: &str, bind: &UnitBind<'_>) -> Re {
    Re::new(&unit_bind_source(units, bind), bind.flags)
}

struct Reader {
    re: Re,
    leading: bool,
}

static MEASUREMENT_READERS: LazyLock<Vec<Reader>> = LazyLock::new(|| {
    let gap = format!("([ \t{NBSP}]+)");
    let sticky = |flags: &str| flags.replacen('g', "", 1) + "y";
    let mut readers: Vec<Reader> = [LENGTH_UNITS, WEIGHT_UNITS, VOLUME_UNITS, TIME_UNITS]
        .into_iter()
        .map(|units| (units, UnitBind { gap: gap.clone(), ..UnitBind::default() }))
        .chain([
            (DATA_UNITS, UnitBind { flags: "gu", gap: gap.clone(), ..UnitBind::default() }),
            (FREQUENCY_UNITS, UnitBind { gap: gap.clone(), ..UnitBind::decimal() }),
        ])
        .map(|(units, bind)| Reader {
            re: Re::new(&unit_bind_source(units, &bind), &sticky(bind.flags)),
            leading: true,
        })
        .collect();
    readers.push(Reader { re: Re::new(&volt_bind_source(&format!("[ \t{NBSP}]+")), "uy"), leading: false });
    readers
});

pub fn measurement_before_particle(source: &[u16], start: usize, end: usize) -> bool {
    let mut number = start;
    while number > 0 && matches!(source[number - 1], 0x20 | 0x09 | 0xA0) {
        number -= 1;
    }
    while number > 0 && matches!(source[number - 1], 0x30..=0x39 | 0x2E | 0x2C) {
        number -= 1;
    }
    if number == start {
        return false;
    }
    MEASUREMENT_READERS.iter().any(|Reader { re, leading }| {
        let at = if *leading { number.saturating_sub(1) } else { number };
        re.exec_at(source, at).is_some_and(|m| m.end() == end)
    })
}
