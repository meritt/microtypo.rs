use serde_json::Value;

use crate::engine::{MicroTypo, js_to_string};
use crate::errors::{Error, Result};
use crate::js::JsString;
use crate::js::raw_json::{Node, RawJson};
use crate::protect::blocks::{to_non_capturing, to_non_capturing_elements};

/// The result of [`MicroTypo::process_with_options`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Output {
    /// The processed text, when none of `changes`, `edits` and `trace` is set.
    Text(JsString),
    /// The processed text with what `changes`, `edits` and `trace` request.
    Diagnostics(crate::Diagnostics),
}

pub(crate) fn type_of(value: &Value) -> &'static str {
    match value {
        Value::Null | Value::Array(_) | Value::Object(_) => "object",
        Value::Bool(_) => "boolean",
        Value::Number(_) => "number",
        Value::String(_) => "string",
    }
}

pub(crate) fn truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().is_some_and(|n| n != 0.0),
        Value::String(s) => !s.is_empty(),
        Value::Array(_) | Value::Object(_) => true,
    }
}

impl MicroTypo {
    /// Typesets UTF-16 code units with the `options` object of the reference `process(text, options)`; a non-boolean option fails with `MicroTypoConfigError`.
    pub fn process_with_options(&mut self, text: &[u16], options: Option<&Value>) -> Result<Output> {
        let diagnostics = crate::ProcessOptions::read(options)?;
        if diagnostics.changes || diagnostics.trace || diagnostics.edits {
            self.process_with_diagnostics(text, diagnostics).map(Output::Diagnostics)
        } else {
            self.process_utf16(text).map(Output::Text)
        }
    }

    /// Calls a configuration method of the reference engine by name: `registerRuleGroup`, `applyOptions`, `setLayout`, `setPrefix`, `addSafeTag` or `addSafeBlock`; another name fails with `TypeError`.
    pub fn call(&mut self, method: &str, args: &[Value]) -> Result<()> {
        let args = Value::Array(args.to_vec());
        self.call_node(method, Node::root(&args, None))
    }

    /// [`call`](Self::call) with the arguments as JSON text, whose strings keep lone surrogates; text that is not JSON fails with `MicroTypoConfigError` and `details.reason` `"json"`.
    pub fn call_json(&mut self, method: &str, args: &str) -> Result<()> {
        let raw = RawJson::parse(args)?;
        self.call_node(method, Node::root(&raw.value, Some(&raw)))
    }

    fn call_node(&mut self, method: &str, input: Node<'_>) -> Result<()> {
        let args = input.value.as_array().cloned().unwrap_or_else(|| vec![input.value.clone()]);
        match method {
            "registerRuleGroup" => {
                let options = input.argument(1);
                let name = options.value.get("name").and_then(Value::as_str).map(|_| options.key("name").units());
                let position = if options.value.get("position").is_some_and(Value::is_string) {
                    options.key("position").units()
                } else {
                    crate::js::u("end")
                };
                let group = crate::schemas::data_rules::data_group(input.argument(0), name.as_deref())?;
                self.register_compiled(group, &position).map(|_| ())
            }
            "applyOptions" => self.apply_options_node(input.argument(0)).map(|_| ()),
            "setLayout" => self.set_layout_value(args.first().unwrap_or(&Value::Null)),
            "setPrefix" => self.set_prefix_value(args.first().unwrap_or(&Value::Null)).map(|_| ()),
            "addSafeTag" => match args.first() {
                Some(Value::String(tag)) => self.add_safe_tag(tag).map(|_| ()),
                other => Err(Error::type_error(format!(
                    "addSafeTag: invalid tag name {} — must match {}",
                    other.map_or("undefined".to_owned(), Value::to_string),
                    crate::shared::tag_name::TAG_NAME_SOURCE
                ))),
            },
            "addSafeBlock" => self.add_safe_block_value(args.first()),
            _ => Err(Error::type_error(format!("{method} is not a configuration call"))),
        }
    }

    fn add_safe_block_value(&mut self, spec: Option<&Value>) -> Result<()> {
        let fields = match spec {
            None => {
                return Err(Error::type_error("Cannot destructure property 'id' of 'undefined' as it is undefined."));
            }
            Some(Value::Null) => {
                return Err(Error::type_error("Cannot destructure property 'id' of 'object null' as it is null."));
            }
            Some(Value::Object(fields)) => Some(fields),
            Some(_) => None,
        };
        let field = |key: &str| fields.and_then(|f| f.get(key));
        let (id, open, close) = (field("id"), field("open"), field("close"));
        let id_text = id.map_or_else(|| "undefined".to_owned(), js_to_string);
        let with_id = |rest: &[(&str, &Value)]| {
            let id = id.map(|id| ("id", id));
            Value::Object(id.iter().chain(rest).map(|&(k, v)| (k.to_owned(), v.clone())).collect())
        };
        if !open.is_some_and(truthy) || !close.is_some_and(truthy) {
            return Err(Error::config(format!("safe block '{id_text}': open/close must be non-empty"), with_id(&[])));
        }
        let (open, close) = (open.expect("checked truthy"), close.expect("checked truthy"));
        let unsafe_regex = field("unsafeRegex").is_some_and(truthy);
        let source = |delimiter: &Value| -> Result<String> {
            match (unsafe_regex, delimiter) {
                (false, Value::String(s)) => Ok(crate::js::regex::escape_str(s)),
                (false, _) => Err(Error::type_error("input argument must be a string")),
                (true, Value::String(s)) => to_non_capturing(s),
                (true, Value::Array(items)) => {
                    Ok(to_non_capturing_elements(&items.iter().map(js_to_string).collect::<Vec<_>>()))
                }
                (true, _) => Ok(String::new()),
            }
        };
        let (o, c) = (source(open)?, source(close)?);
        self.add_safe_block_sources(&id_text, &o, &c, || with_id(&[("open", open), ("close", close)]))
    }
}
