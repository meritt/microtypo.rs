use std::sync::LazyLock;

use serde_json::{Map, Value, json};

use crate::core::options::{KNOWN_GROUPS, SETTING_SELECTORS, canonical_group_name, is_bundle, wildcard_to_regex};
use crate::errors::{Error, Result};
use crate::js::{self, Re};

fn selector_matches_rule(group: &str, selector: &str) -> Result<bool> {
    if selector == "*" {
        return Ok(true);
    }
    let ids = crate::rules::registry::default_rule_ids(group);
    let re = wildcard_to_regex(selector)?;
    Ok(ids.iter().any(|id| re.test(&js::u(id))))
}

const TOP_LEVEL_KEYS: [&str; 8] =
    ["entities", "maxInputLength", "maxProcessingMs", "input", "presets", "html", "rules", "render"];
const RENDER_KEYS: [&str; 6] = ["paragraphs", "autolink", "breakline", "nowrap", "hanging", "prefix"];

pub static UNSAFE_PREFIX_RE: LazyLock<Re> = LazyLock::new(|| Re::new(r#"["<>&\s]"#, ""));

const MAX_INPUT_LENGTH: f64 = 5_000_000.0;

pub const INPUT_FORMATS: [&str; 8] = ["text", "html", "markdown", "json", "xml", "toml", "yaml", "frontmatter"];
pub const INPUT_TEMPLATES: [&str; 8] = ["off", "handlebars", "mustache", "twig", "jinja", "nunjucks", "liquid", "erb"];
const INPUT_KEYS: [&str; 4] = ["format", "fields", "exclude", "template"];

fn input_format_message() -> String {
    format!("expected one of {}", INPUT_FORMATS.join("|"))
}

pub fn is_booleanish(value: &Value) -> bool {
    match value {
        Value::Bool(_) => true,
        Value::String(s) => ["on", "off", "1", "0", "true", "false"].contains(&s.as_str()),
        Value::Number(_) => value.as_f64() == Some(0.0) || value.as_f64() == Some(1.0),
        _ => false,
    }
}

fn string_array(value: &Value) -> Option<Vec<String>> {
    value.as_array()?.iter().map(|v| v.as_str().map(str::to_owned)).collect()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InputConfig {
    pub format: String,
    pub template: String,
    pub fields: Option<Vec<String>>,
    pub exclude: Option<Vec<String>>,
}

impl InputConfig {
    fn plain(format: &str) -> Self {
        InputConfig { format: format.to_owned(), template: "off".to_owned(), fields: None, exclude: None }
    }
}

fn type_name(v: &Value) -> &'static str {
    match v {
        Value::Null => "object",
        Value::Bool(_) => "boolean",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "object",
        Value::Object(_) => "object",
    }
}

fn normalise_input(input: Option<&Value>, issues: &mut Vec<String>) -> InputConfig {
    let Some(input) = input else { return InputConfig::plain("text") };
    if let Value::String(s) = input {
        if !INPUT_FORMATS.contains(&s.as_str()) {
            issues.push(format!("input: {}", input_format_message()));
        }
        return InputConfig::plain(s);
    }
    let Some(obj) = input.as_object() else {
        issues.push("input: expected a string or an object".to_owned());
        return InputConfig::plain("text");
    };
    for key in obj.keys() {
        if !INPUT_KEYS.contains(&key.as_str()) {
            issues.push(format!("input: unknown key \"{key}\""));
        }
    }
    let format = match obj.get("format") {
        None | Some(Value::Null) => "text".to_owned(),
        Some(Value::String(s)) => s.clone(),
        Some(other) => other.to_string(),
    };
    if !INPUT_FORMATS.contains(&format.as_str())
        || !matches!(obj.get("format"), None | Some(Value::Null | Value::String(_)))
    {
        issues.push(format!("input.format: {}", input_format_message()));
    }
    let template = match obj.get("template") {
        None | Some(Value::Null) => "off".to_owned(),
        Some(Value::String(s)) => s.clone(),
        Some(other) => other.to_string(),
    };
    if !INPUT_TEMPLATES.contains(&template.as_str())
        || !matches!(obj.get("template"), None | Some(Value::Null | Value::String(_)))
    {
        issues.push(format!("input.template: expected one of {}", INPUT_TEMPLATES.join("|")));
    }
    let mut normalised = InputConfig { format, template, fields: None, exclude: None };
    if let Some(fields) = obj.get("fields") {
        match string_array(fields) {
            Some(f) => normalised.fields = Some(f),
            None => issues.push("input.fields: expected an array of strings".to_owned()),
        }
    }
    if let Some(exclude) = obj.get("exclude") {
        match string_array(exclude) {
            Some(e) => normalised.exclude = Some(e),
            None => issues.push("input.exclude: expected an array of strings".to_owned()),
        }
    }
    normalised
}

#[derive(Debug, Clone)]
pub struct ValidatedConfig {
    pub raw: Map<String, Value>,
    pub input: InputConfig,
}

impl ValidatedConfig {
    pub fn presets(&self) -> Option<bool> {
        self.raw.get("presets").and_then(Value::as_bool)
    }
}

fn check_booleanish(cfg: &Map<String, Value>, key: &str, label: &str, issues: &mut Vec<String>) {
    if let Some(v) = cfg.get(key)
        && !is_booleanish(v)
    {
        issues.push(format!("{label}: expected a boolean"));
    }
}

pub fn validate_config(config: Option<&Value>) -> Result<ValidatedConfig> {
    let empty = Map::new();
    let cfg = match config {
        None | Some(Value::Null) => &empty,
        Some(Value::Object(o)) => o,
        Some(other) => {
            let got = if other.is_array() { "an array" } else { type_name(other) };
            return Err(Error::config(
                format!("invalid config: expected an object, got {got}"),
                json!({ "issues": ["config: expected an object"] }),
            ));
        }
    };
    let mut issues: Vec<String> = Vec::new();
    for key in cfg.keys() {
        if !TOP_LEVEL_KEYS.contains(&key.as_str()) {
            issues.push(format!("unknown option: {key}"));
        }
    }
    check_booleanish(cfg, "entities", "entities", &mut issues);
    if let Some(value) = cfg.get("maxInputLength") {
        match value.as_f64() {
            Some(v) if v.fract() == 0.0 && v >= 0.0 => {
                if v > MAX_INPUT_LENGTH {
                    issues.push(format!("maxInputLength: exceeds ceiling {}", MAX_INPUT_LENGTH as u64));
                }
            }
            _ => issues.push("maxInputLength: expected a non-negative integer".to_owned()),
        }
    }
    if let Some(value) = cfg.get("maxProcessingMs") {
        match value.as_f64() {
            Some(v) if v.is_finite() && v >= 0.0 => {}
            _ => issues.push("maxProcessingMs: expected a non-negative, finite number".to_owned()),
        }
    }
    if let Some(value) = cfg.get("presets")
        && !value.is_boolean()
    {
        issues.push("presets: expected a boolean".to_owned());
    }
    let input = normalise_input(cfg.get("input"), &mut issues);
    check_booleanish(cfg, "html", "html", &mut issues);
    if let Some(render) = cfg.get("render") {
        match render.as_object() {
            None => issues.push("render: expected an object".to_owned()),
            Some(render) => {
                for key in render.keys() {
                    if !RENDER_KEYS.contains(&key.as_str()) {
                        issues.push(format!("render: unknown key \"{key}\""));
                    }
                }
                check_booleanish(render, "paragraphs", "render.paragraphs", &mut issues);
                check_booleanish(render, "autolink", "render.autolink", &mut issues);
                check_booleanish(render, "breakline", "render.breakline", &mut issues);
                if let Some(v) = render.get("nowrap")
                    && !matches!(v.as_str(), Some("nobr" | "span"))
                {
                    issues.push("render.nowrap: expected \"nobr\" or \"span\"".to_owned());
                }
                if let Some(v) = render.get("hanging")
                    && !matches!(v.as_str(), Some("style" | "class" | "both"))
                {
                    issues.push("render.hanging: expected \"style\", \"class\", or \"both\"".to_owned());
                }
                match render.get("prefix") {
                    None | Some(Value::Bool(false)) => {}
                    Some(Value::String(s)) => {
                        if UNSAFE_PREFIX_RE.test(&js::u(s)) {
                            issues.push(
                                "render.prefix: must not contain a quote, angle bracket, \"&\", or whitespace"
                                    .to_owned(),
                            );
                        }
                    }
                    Some(_) => issues.push("render.prefix: expected false or a string".to_owned()),
                }
            }
        }
    }
    if let Some(rules) = cfg.get("rules") {
        match rules.as_object() {
            None => issues.push("rules: expected an object".to_owned()),
            Some(rules) => {
                for (key, value) in rules {
                    if !is_booleanish(value) {
                        issues.push(format!("rules: \"{key}\" expected a boolean"));
                    }
                    if is_bundle(key) {
                        continue;
                    }
                    let Some(dot) = key.find('.') else {
                        if !KNOWN_GROUPS.contains(&canonical_group_name(key).as_str()) {
                            issues.push(format!("rules: unknown key \"{key}\""));
                        }
                        continue;
                    };
                    let group_raw = &key[..dot];
                    let group = canonical_group_name(group_raw);
                    if !KNOWN_GROUPS.contains(&group.as_str()) {
                        issues.push(format!("rules: unknown group \"{group_raw}\""));
                        continue;
                    }
                    if SETTING_SELECTORS.contains(&key.as_str()) {
                        continue;
                    }
                    if !selector_matches_rule(&group, &key[dot + 1..])? {
                        issues.push(format!("rules: \"{key}\" does not match any rule id in group \"{group_raw}\""));
                    }
                }
            }
        }
    }
    if !issues.is_empty() {
        return Err(Error::config(format!("invalid config: {}", issues.join("; ")), json!({ "issues": issues })));
    }
    Ok(ValidatedConfig { raw: cfg.clone(), input })
}
