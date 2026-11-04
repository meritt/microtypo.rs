use serde_json::{Map, Value, json};

use crate::errors::{Error, Result};
use crate::js::{self, Re};

const MAX_SELECTOR_LEN: usize = 64;
const MAX_SELECTOR_STARS: usize = 4;

pub fn wildcard_to_regex(pattern: &str) -> Result<Re> {
    if pattern.encode_utf16().count() > MAX_SELECTOR_LEN {
        return Err(Error::config(
            format!("selector too long (max {MAX_SELECTOR_LEN})"),
            json!({ "selector": pattern }),
        ));
    }
    let parts: Vec<&str> = pattern.split('*').collect();
    if parts.len() - 1 > MAX_SELECTOR_STARS {
        return Err(Error::config(
            format!("selector has too many '*' (max {MAX_SELECTOR_STARS})"),
            json!({ "selector": pattern }),
        ));
    }
    let body = parts.iter().map(|p| js::regex::escape_str(p)).collect::<Vec<_>>().join("[a-z0-9_-]*");
    Ok(Re::new(&format!("^{body}$"), "i"))
}

pub fn canonical_group_name(name: &str) -> String {
    js::lossy(&js::to_lower(&js::u(name)))
}

fn number_is(value: &Value, n: f64) -> bool {
    value.as_f64() == Some(n)
}

pub fn is_on(value: &Value) -> bool {
    match value {
        Value::Bool(b) => *b,
        Value::Number(_) => number_is(value, 1.0),
        Value::String(s) => {
            let v = canonical_group_name(s);
            v == "on" || v == "1" || v == "true"
        }
        _ => false,
    }
}

pub fn is_off(value: &Value) -> bool {
    match value {
        Value::Bool(b) => !*b,
        Value::Number(_) => number_is(value, 0.0),
        Value::Null => true,
        Value::String(s) => {
            let v = canonical_group_name(s);
            v == "off" || v == "0" || v == "false" || v.is_empty()
        }
        _ => false,
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Override {
    pub group: String,
    pub selector: String,
    pub value: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Setting {
    pub group: String,
    pub key: String,
    pub value: Value,
}

#[derive(Debug, Default)]
pub struct Intent {
    pub engine_settings: Vec<(String, Value)>,
    pub overrides: Vec<Override>,
    pub settings: Vec<Setting>,
}

impl Intent {
    fn set_engine(&mut self, key: &str, value: Value) {
        match self.engine_settings.iter_mut().find(|(k, _)| k == key) {
            Some(slot) => slot.1 = value,
            None => self.engine_settings.push((key.to_owned(), value)),
        }
    }
}

const ENGINE_KEYS: [&str; 4] = ["entities", "html", "maxInputLength", "maxProcessingMs"];

pub fn normalise_options(input: &Map<String, Value>) -> Intent {
    let mut out = Intent::default();
    for key in ENGINE_KEYS {
        if let Some(v) = input.get(key) {
            out.set_engine(key, v.clone());
        }
    }
    if let Some(rules) = input.get("rules") {
        apply_rules(rules, &mut out);
    }
    if let Some(render) = input.get("render") {
        apply_render(render, &mut out);
    }
    out
}

fn push_override(out: &mut Intent, group: &str, selector: &str, value: bool) {
    out.overrides.push(Override { group: canonical_group_name(group), selector: selector.to_owned(), value });
}

fn apply_render(opts: &Value, out: &mut Intent) {
    let Some(opts) = opts.as_object() else { return };
    if let Some(v) = opts.get("paragraphs") {
        push_override(out, "text", "paragraphs", is_on(v));
    }
    if let Some(v) = opts.get("autolink") {
        push_override(out, "text", "autolink", is_on(v));
        push_override(out, "text", "email", is_on(v));
    }
    if let Some(v) = opts.get("breakline") {
        push_override(out, "text", "breakline", is_on(v));
    }
    if let Some(v) = opts.get("nowrap") {
        out.set_engine("nowrap", v.clone());
    }
    if let Some(v) = opts.get("hanging") {
        out.set_engine("layout", v.clone());
    }
    if let Some(v) = opts.get("prefix") {
        out.set_engine("prefix", v.clone());
    }
}

pub const KNOWN_GROUPS: [&str; 12] =
    ["quote", "dash", "symbol", "punctuation", "number", "space", "abbr", "nobr", "date", "hanging", "other", "text"];

pub const BUNDLES: [(&str, &[&str]); 6] = [
    ("nbsp", &["nobr.nbsp_*", "space.nbsp_*", "abbr.nbsp_*", "number.nbsp_*"]),
    ("emdash", &["dash.em", "dash.em_*"]),
    ("endash", &["dash.en_range", "number.en_range"]),
    ("triads", &["number.thin_space_triads", "other.split_triads"]),
    (
        "units",
        &[
            "abbr.nbsp_unit",
            "abbr.nbsp_weight_unit",
            "abbr.nbsp_volume_unit",
            "abbr.nbsp_time_unit",
            "abbr.nbsp_data_unit",
            "abbr.nbsp_frequency_unit",
            "abbr.nbsp_css_unit",
            "abbr.nbsp_volt",
        ],
    ),
    ("currency", &["abbr.currency", "abbr.nbsp_money_magnitude", "abbr.nbsp_currency_prefix"]),
];

pub fn is_bundle(name: &str) -> bool {
    BUNDLES.iter().any(|(n, _)| *n == name)
}

pub const SETTING_SELECTORS: [&str; 2] = ["quote.nested", "quote.inch"];

fn push_dotted(out: &mut Intent, key: &str, value: bool) {
    let dot = key.find('.').expect("a dotted key");
    push_override(out, &key[..dot], &key[dot + 1..], value);
}

fn apply_rules(rules: &Value, out: &mut Intent) {
    let Some(rules) = rules.as_object() else { return };
    for (key, value) in rules {
        if let Some((_, bundle)) = BUNDLES.iter().find(|(n, _)| n == key) {
            for o in *bundle {
                push_dotted(out, o, is_on(value));
            }
            continue;
        }
        if SETTING_SELECTORS.contains(&key.as_str()) {
            let name = if key == "quote.nested" { "allowNested" } else { "convertInches" };
            out.settings.push(Setting {
                group: "quote".to_owned(),
                key: name.to_owned(),
                value: Value::Bool(is_on(value)),
            });
            continue;
        }
        if key.contains('.') {
            push_dotted(out, key, is_on(value));
            continue;
        }
        if KNOWN_GROUPS.contains(&canonical_group_name(key).as_str()) {
            push_override(out, key, "*", is_on(value));
        }
    }
}
