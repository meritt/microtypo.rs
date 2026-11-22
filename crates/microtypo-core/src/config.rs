use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::{Map, Value};

use crate::core::options::is_on;
use crate::errors::{Error, Result};
use crate::schemas::config::validate_config;

/// The options object of the reference constructor, typed.
///
/// A `None` field or empty `rules` keeps the reference default. Serde uses the reference spellings (`maxInputLength`, `render.hanging`), and deserialising validates as a JSON configuration does, with the same errors. A value the types admit but the reference rejects, such as an unknown `input.format`, fails in [`MicroTypo::with_config`](crate::MicroTypo::with_config) with the error of the JSON configuration.
///
/// ```
/// use microtypo_core::{Config, Input, MicroTypo};
/// # let host = microtypo_conformance::HOST;
///
/// let config = Config {
///     html: Some(true),
///     input: Some(Input { format: Some("markdown".into()), ..Input::default() }),
///     rules: vec![("quote".into(), false)],
///     ..Config::default()
/// };
/// let mut engine = MicroTypo::with_config(config, host)?;
/// assert_eq!(engine.process("Это - \"цитата\"")?, "Это\u{a0}— \"цитата\"");
///
/// let parsed: Config = serde_json::from_str(r#"{ "maxInputLength": 1000, "rules": { "nbsp": "off" } }"#)?;
/// assert_eq!(parsed.max_input_length, Some(1000));
/// assert_eq!(parsed.rules, [("nbsp".to_owned(), false)]);
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Config {
    /// `html`: write HTML markup such as `<p>` and `<span class="nowrap">`.
    pub html: Option<bool>,
    /// `entities`: write glyphs as HTML entities.
    pub entities: Option<bool>,
    /// `maxInputLength`: the longest input in UTF-16 code units; 30 000 by default.
    pub max_input_length: Option<u32>,
    /// `maxProcessingMs`: the time budget of one call in milliseconds, 0 for none; 5 000 by default.
    pub max_processing_ms: Option<f64>,
    pub input: Option<Input>,
    /// `presets`: start from the built-in rule groups; `true` by default.
    pub presets: Option<bool>,
    /// `rules`: rule switches in order, keyed by group, `group.rule` selector or bundle name.
    pub rules: Vec<(String, bool)>,
    pub render: Option<Render>,
    pub rule_groups: Option<Vec<DataRuleGroup>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DataPattern {
    pub source: String,
    pub flags: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DataRule {
    pub id: String,
    pub pattern: DataPattern,
    pub replacement: String,
    pub enabled: Option<bool>,
    pub cycled: Option<bool>,
    pub html_only: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DataRuleGroup {
    pub name: String,
    pub rules: Vec<DataRule>,
    pub title: Option<String>,
    pub position: Option<String>,
}

impl DataRuleGroup {
    fn to_json(&self) -> Value {
        let mut group = Map::new();
        group.insert("name".into(), Value::String(self.name.clone()));
        put(&mut group, "title", self.title.clone().map(Value::String));
        put(&mut group, "position", self.position.clone().map(Value::String));
        group.insert(
            "rules".into(),
            Value::Array(
                self.rules
                    .iter()
                    .map(|rule| {
                        let mut value = Map::new();
                        value.insert("id".into(), Value::String(rule.id.clone()));
                        value.insert(
                            "pattern".into(),
                            serde_json::json!({"source":rule.pattern.source,"flags":rule.pattern.flags}),
                        );
                        value.insert("replacement".into(), Value::String(rule.replacement.clone()));
                        put(&mut value, "enabled", rule.enabled.map(Value::Bool));
                        put(&mut value, "cycled", rule.cycled.map(Value::Bool));
                        put(&mut value, "htmlOnly", rule.html_only.map(Value::Bool));
                        Value::Object(value)
                    })
                    .collect(),
            ),
        );
        Value::Object(group)
    }

    fn read(group: &Value) -> Self {
        let text = |key: &str| group.get(key).and_then(Value::as_str).map(str::to_owned);
        Self {
            name: text("name").expect("validated name"),
            title: text("title"),
            position: text("position"),
            rules: group["rules"]
                .as_array()
                .expect("validated rules")
                .iter()
                .map(|rule| DataRule {
                    id: rule["id"].as_str().expect("validated id").into(),
                    pattern: DataPattern {
                        source: rule["pattern"]["source"].as_str().expect("validated source").into(),
                        flags: rule["pattern"]["flags"].as_str().expect("validated flags").into(),
                    },
                    replacement: rule["replacement"].as_str().expect("validated replacement").into(),
                    enabled: rule.get("enabled").and_then(Value::as_bool),
                    cycled: rule.get("cycled").and_then(Value::as_bool),
                    html_only: rule.get("htmlOnly").and_then(Value::as_bool),
                })
                .collect(),
        }
    }
}

/// The `input` option of a [`Config`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Input {
    /// `format`: `text` (the default), `html`, `plain`, `markdown`, `mdx`, `json`, `xml`, `toml`, `yaml` or `frontmatter`.
    pub format: Option<String>,
    /// `template`: the template syntax to keep intact, `off` by default.
    pub template: Option<String>,
    /// `placeholders`: the localization syntax to keep intact: `all` (the default), `icu`, `printf`, `interpolation` or `off`.
    pub placeholders: Option<String>,
    /// `pragmas`: `microtypo` (the default) keeps `microtypo-ignore` comment regions as written; `off` ignores them.
    pub pragmas: Option<String>,
    /// `math`: keep formulas in Markdown, MDX and a frontmatter body intact; on by default.
    pub math: Option<bool>,
    /// `attributes`: the textual HTML attributes to typeset, all of `alt`, `title`, `aria-label`, `placeholder` and `content` by default.
    pub attributes: Option<Vec<String>>,
    /// `fields`: the paths of the values to typeset in structured formats.
    pub fields: Option<Vec<String>>,
    /// `exclude`: the paths of the values to leave alone in structured formats.
    pub exclude: Option<Vec<String>>,
}

/// The `render` option of a [`Config`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Render {
    /// `paragraphs`: wrap paragraphs in `<p>`.
    pub paragraphs: Option<bool>,
    /// `autolink`: turn URLs and addresses into links.
    pub autolink: Option<bool>,
    /// `breakline`: turn line breaks into `<br>`.
    pub breakline: Option<bool>,
    /// `nowrap`: `span` or `nobr` for unbreakable runs.
    pub nowrap: Option<String>,
    /// `hanging`: `style`, `class` or `both`, how hanging punctuation is laid out.
    pub hanging: Option<String>,
    /// `prefix`: the prefix of emitted class names; empty for none.
    pub prefix: Option<String>,
}

fn put(map: &mut Map<String, Value>, key: &str, value: Option<Value>) {
    if let Some(value) = value {
        map.insert(key.to_owned(), value);
    }
}

fn strings(items: &[String]) -> Value {
    Value::Array(items.iter().cloned().map(Value::String).collect())
}

impl Config {
    /// The configuration as the JSON object of the reference constructor.
    pub fn to_json(&self) -> Value {
        let mut map = Map::new();
        put(&mut map, "html", self.html.map(Value::Bool));
        put(&mut map, "entities", self.entities.map(Value::Bool));
        put(&mut map, "maxInputLength", self.max_input_length.map(Value::from));
        put(&mut map, "maxProcessingMs", self.max_processing_ms.map(Value::from));
        put(&mut map, "input", self.input.as_ref().map(Input::to_json));
        put(&mut map, "presets", self.presets.map(Value::Bool));
        if !self.rules.is_empty() {
            let rules = self.rules.iter().map(|(key, on)| (key.clone(), Value::Bool(*on))).collect();
            map.insert("rules".to_owned(), Value::Object(rules));
        }
        put(&mut map, "render", self.render.as_ref().map(Render::to_json));
        put(
            &mut map,
            "ruleGroups",
            self.rule_groups.as_ref().map(|groups| Value::Array(groups.iter().map(DataRuleGroup::to_json).collect())),
        );
        Value::Object(map)
    }
}

impl Input {
    fn to_json(&self) -> Value {
        let mut map = Map::new();
        put(&mut map, "format", self.format.clone().map(Value::String));
        put(&mut map, "template", self.template.clone().map(Value::String));
        put(&mut map, "placeholders", self.placeholders.clone().map(Value::String));
        put(&mut map, "pragmas", self.pragmas.clone().map(Value::String));
        put(&mut map, "math", self.math.map(Value::Bool));
        put(&mut map, "attributes", self.attributes.as_deref().map(strings));
        put(&mut map, "fields", self.fields.as_deref().map(strings));
        put(&mut map, "exclude", self.exclude.as_deref().map(strings));
        Value::Object(map)
    }

    fn read(value: &Value) -> Input {
        let text = |key: &str| value.get(key).and_then(Value::as_str).map(str::to_owned);
        let list = |key: &str| {
            value
                .get(key)
                .and_then(Value::as_array)
                .map(|items| items.iter().filter_map(Value::as_str).map(str::to_owned).collect())
        };
        match value {
            Value::String(format) => Input { format: Some(format.clone()), ..Input::default() },
            _ => Input {
                format: text("format"),
                template: text("template"),
                placeholders: text("placeholders"),
                pragmas: text("pragmas"),
                math: value.get("math").map(is_on),
                attributes: list("attributes"),
                fields: list("fields"),
                exclude: list("exclude"),
            },
        }
    }
}

impl Render {
    fn to_json(&self) -> Value {
        let mut map = Map::new();
        put(&mut map, "paragraphs", self.paragraphs.map(Value::Bool));
        put(&mut map, "autolink", self.autolink.map(Value::Bool));
        put(&mut map, "breakline", self.breakline.map(Value::Bool));
        put(&mut map, "nowrap", self.nowrap.clone().map(Value::String));
        put(&mut map, "hanging", self.hanging.clone().map(Value::String));
        put(&mut map, "prefix", self.prefix.clone().map(Value::String));
        Value::Object(map)
    }

    fn read(value: &Value) -> Render {
        let on = |key: &str| value.get(key).map(is_on);
        let text = |key: &str| value.get(key).and_then(Value::as_str).map(str::to_owned);
        Render {
            paragraphs: on("paragraphs"),
            autolink: on("autolink"),
            breakline: on("breakline"),
            nowrap: text("nowrap"),
            hanging: text("hanging"),
            prefix: value.get("prefix").map(|p| p.as_str().unwrap_or_default().to_owned()),
        }
    }
}

impl TryFrom<&Value> for Config {
    type Error = Error;

    /// Reads a JSON configuration, validated as by the reference constructor.
    fn try_from(value: &Value) -> Result<Config> {
        validate_config(Some(value))?;
        let on = |key: &str| value.get(key).map(is_on);
        Ok(Config {
            html: on("html"),
            entities: on("entities"),
            max_input_length: value.get("maxInputLength").and_then(Value::as_f64).map(|n| n as u32),
            max_processing_ms: value.get("maxProcessingMs").and_then(Value::as_f64),
            input: value.get("input").map(Input::read),
            presets: value.get("presets").and_then(Value::as_bool),
            rules: value
                .get("rules")
                .and_then(Value::as_object)
                .map_or_default(|rules| rules.iter().map(|(key, on)| (key.clone(), is_on(on))).collect()),
            render: value.get("render").map(Render::read),
            rule_groups: value
                .get("ruleGroups")
                .map(|groups| groups.as_array().expect("validated array").iter().map(DataRuleGroup::read).collect()),
        })
    }
}

impl Serialize for Config {
    fn serialize<S: Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        self.to_json().serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for Config {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> std::result::Result<Self, D::Error> {
        Config::try_from(&Value::deserialize(deserializer)?).map_err(serde::de::Error::custom)
    }
}
