use std::cell::RefCell;
use std::sync::Arc;

use js_sys::{Array, JSON, Object, Reflect};
use microtypo_core::rule_runner::RuleGroupInfo;
use serde_json::{Value, json};
use wasm_bindgen::prelude::*;

#[wasm_bindgen]
extern "C" {
    #[wasm_bindgen(js_namespace = console)]
    fn error(message: &str);
}

#[wasm_bindgen(start)]
fn start() {
    std::panic::set_hook(Box::new(|info| error(&info.to_string())));
}

fn error_object(name: &str, message: &str, code: Option<&str>, details: &Value) -> JsValue {
    let error = Object::new();
    let _ = Reflect::set(&error, &"engine".into(), &true.into());
    let _ = Reflect::set(&error, &"name".into(), &name.into());
    let _ = Reflect::set(&error, &"message".into(), &message.into());
    if let Some(code) = code {
        let _ = Reflect::set(&error, &"code".into(), &code.into());
    }
    if !details.is_null()
        && let Ok(details) = JSON::parse(&details.to_string())
    {
        let _ = Reflect::set(&error, &"details".into(), &details);
    }
    error.into()
}

fn throw(e: &microtypo_core::Error) -> JsValue {
    error_object(e.name(), &e.message, e.code(), &e.details)
}

fn parse(json: &str) -> Result<Value, JsValue> {
    serde_json::from_str(json).map_err(|e| error_object("TypeError", &format!("invalid JSON: {e}"), None, &Value::Null))
}

thread_local! {
    static CYCLE_LIMITS: RefCell<Vec<(String, usize)>> = const { RefCell::new(Vec::new()) };
}

fn record_cycle_limit(id: &str, n: usize) {
    CYCLE_LIMITS.with_borrow_mut(|limits| limits.push((id.to_owned(), n)));
}

fn describe(group: &RuleGroupInfo) -> Value {
    json!({
        "name": group.name,
        "title": group.title,
        "builtin": group.builtin,
        "rules": group.rules.iter().map(|r| json!({ "id": r.id, "defaultEnabled": r.default_enabled })).collect::<Vec<_>>(),
    })
}

#[wasm_bindgen]
pub struct Engine {
    inner: microtypo_core::MicroTypo,
    changes: Vec<u32>,
}

#[wasm_bindgen]
impl Engine {
    #[wasm_bindgen(constructor)]
    pub fn new(config: Option<String>) -> Result<Engine, JsValue> {
        let config = config.as_deref().map(parse).transpose()?;
        let mut inner = microtypo_core::MicroTypo::new(config.as_ref()).map_err(|e| throw(&e))?;
        inner.set_on_cycle_limit(Some(Arc::new(record_cycle_limit)));
        Ok(Engine { inner, changes: Vec::new() })
    }

    pub fn call(&mut self, method: &str, args: &str) -> Result<(), JsValue> {
        let args = match parse(args)? {
            Value::Array(args) => args,
            other => vec![other],
        };
        self.inner.call(method, &args).map_err(|e| throw(&e))
    }

    pub fn process(&mut self, text: &[u16], with_changes: bool) -> Result<Vec<u16>, JsValue> {
        CYCLE_LIMITS.with_borrow_mut(Vec::clear);
        let result = if with_changes {
            self.inner.process_with_changes(text).map(|p| {
                self.changes = p.changes.iter().flat_map(|&(a, b)| [a as u32, b as u32]).collect();
                p.text
            })
        } else {
            self.inner.process(text)
        };
        result.map_err(|e| throw(&e))
    }

    #[wasm_bindgen(js_name = takeCycleLimits)]
    pub fn take_cycle_limits(&mut self) -> Array {
        CYCLE_LIMITS.take().into_iter().map(|(id, n)| Array::of2(&id.into(), &(n as f64).into())).collect()
    }

    #[wasm_bindgen(js_name = takeChanges)]
    pub fn take_changes(&mut self) -> Vec<u32> {
        std::mem::take(&mut self.changes)
    }

    #[wasm_bindgen(js_name = ruleGroup)]
    pub fn rule_group(&self, name: &str) -> Option<String> {
        self.inner.get_rule_group(name).map(|g| describe(&g).to_string())
    }

    #[wasm_bindgen(js_name = ruleGroups)]
    pub fn rule_groups(&self) -> String {
        Value::Array(self.inner.list_rule_groups().iter().map(describe).collect()).to_string()
    }
}

#[wasm_bindgen]
pub fn version() -> String {
    microtypo_core::VERSION.to_owned()
}

#[wasm_bindgen(js_name = defaultMaxInputLength)]
pub fn default_max_input_length() -> f64 {
    microtypo_core::DEFAULT_MAX_INPUT_LENGTH as f64
}

impl std::fmt::Debug for Engine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Engine").field("inner", &self.inner).finish_non_exhaustive()
    }
}
