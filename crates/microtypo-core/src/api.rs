use serde_json::{Value, json};

use crate::engine::{MicroTypo, Processed, js_to_string};
use crate::errors::{Error, Result};
use crate::js::JsString;
use crate::protect::blocks::{to_non_capturing, to_non_capturing_elements};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Output {
    Text(JsString),
    WithChanges(Processed),
}

fn type_of(value: &Value) -> &'static str {
    match value {
        Value::Null | Value::Array(_) | Value::Object(_) => "object",
        Value::Bool(_) => "boolean",
        Value::Number(_) => "number",
        Value::String(_) => "string",
    }
}

fn truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().is_some_and(|n| n != 0.0),
        Value::String(s) => !s.is_empty(),
        Value::Array(_) | Value::Object(_) => true,
    }
}

pub fn read_process_options(options: Option<&Value>) -> Result<bool> {
    let Some(changes) = options.and_then(Value::as_object).and_then(|o| o.get("changes")) else {
        return Ok(false);
    };

    match changes {
        Value::Bool(b) => Ok(*b),
        other => Err(Error::config(
            "process() option \"changes\" must be a boolean",
            json!({ "option": "changes", "receivedType": type_of(other) }),
        )),
    }
}

impl MicroTypo {
    pub fn process_with_options(&mut self, text: &[u16], options: Option<&Value>) -> Result<Output> {
        if read_process_options(options)? {
            self.process_with_changes(text).map(Output::WithChanges)
        } else {
            self.process(text).map(Output::Text)
        }
    }

    pub fn call(&mut self, method: &str, args: &[Value]) -> Result<()> {
        match method {
            "applyOptions" => self.apply_options(args.first()).map(|_| ()),
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

    pub fn call_str(&mut self, method: &str, arg: &str) -> Result<()> {
        self.call(method, &[Value::String(arg.to_owned())])
    }
}
