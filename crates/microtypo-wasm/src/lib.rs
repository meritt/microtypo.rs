use std::cell::RefCell;
use std::sync::Arc;

use js_sys::{Array, Date, JSON, Object, Reflect};
use microtypo_core::{Host, JsString};
use wasm_bindgen::prelude::*;

#[cfg(debug_assertions)]
#[wasm_bindgen]
extern "C" {
    #[wasm_bindgen(js_namespace = console)]
    fn error(message: &str);
}

const HOST: Host = Host { now_ms: Date::now, fill_random };

fn fill_random(bytes: &mut [u8]) {
    getrandom::fill(bytes).expect("crypto.getRandomValues is available");
}

#[cfg(debug_assertions)]
#[wasm_bindgen(start)]
fn start() {
    std::panic::set_hook(Box::new(|info| error(&info.to_string())));
}

fn throw(e: &microtypo_core::Error) -> JsValue {
    let error = Object::new();
    let _ = Reflect::set(&error, &"engine".into(), &true.into());
    let _ = Reflect::set(&error, &"name".into(), &e.name().into());
    let _ = Reflect::set(&error, &"message".into(), &e.message.as_str().into());
    if let Some(code) = e.code() {
        let _ = Reflect::set(&error, &"code".into(), &code.into());
    }
    if !e.details.is_null()
        && let Ok(details) = JSON::parse(&e.details.to_string())
    {
        let _ = Reflect::set(&error, &"details".into(), &details);
    }
    error.into()
}

thread_local! {
    static CYCLE_LIMITS: RefCell<Vec<(JsString, usize)>> = const { RefCell::new(Vec::new()) };
}

fn record_cycle_limit(id: &[u16], n: usize) {
    CYCLE_LIMITS.with_borrow_mut(|limits| limits.push((id.to_owned(), n)));
}

#[wasm_bindgen]
pub struct Engine {
    inner: microtypo_core::MicroTypo,
    diagnostics: Option<String>,
}

#[wasm_bindgen]
impl Engine {
    #[wasm_bindgen(constructor)]
    pub fn new(config: Option<String>) -> Result<Engine, JsValue> {
        let inner = match config {
            Some(json) => microtypo_core::MicroTypo::from_json(&json, HOST),
            None => microtypo_core::MicroTypo::new(None, HOST),
        }
        .map_err(|e| throw(&e))?;
        Ok(Engine { inner, diagnostics: None })
    }

    pub fn call(&mut self, method: &str, args: &str) -> Result<(), JsValue> {
        self.inner.call_json(method, args).map_err(|e| throw(&e))
    }

    #[wasm_bindgen(js_name = setCycleHook)]
    pub fn set_cycle_hook(&mut self, on: bool) {
        self.inner.set_on_cycle_limit(on.then(|| Arc::new(record_cycle_limit) as microtypo_core::CycleHook));
    }

    pub fn process(&mut self, text: &[u16], changes: bool, edits: bool, trace: bool) -> Result<Vec<u16>, JsValue> {
        CYCLE_LIMITS.with_borrow_mut(Vec::clear);
        self.diagnostics = None;
        let result = if changes || edits || trace {
            self.inner.process_with_diagnostics(text, microtypo_core::ProcessOptions { changes, edits, trace }).map(
                |output| {
                    self.diagnostics = Some(output.to_json());
                    Vec::new()
                },
            )
        } else {
            self.inner.process_utf16(text)
        };
        result.map_err(|e| throw(&e))
    }

    #[wasm_bindgen(js_name = takeCycleLimits)]
    pub fn take_cycle_limits(&mut self) -> Array {
        CYCLE_LIMITS
            .take()
            .into_iter()
            .map(|(id, n)| {
                let id = id.chunks(0x2000).fold(js_sys::JsString::from(""), |text, chunk| {
                    text.concat(&js_sys::JsString::from_char_code(chunk))
                });
                Array::of2(&id.into(), &(n as f64).into())
            })
            .collect()
    }

    #[wasm_bindgen(js_name = takeDiagnostics)]
    pub fn take_diagnostics(&mut self) -> Option<String> {
        self.diagnostics.take()
    }

    #[wasm_bindgen(js_name = ruleGroup)]
    pub fn rule_group(&self, name: &[u16]) -> Option<String> {
        self.inner.get_rule_group_utf16(name).map(|g| g.to_json())
    }

    #[wasm_bindgen(js_name = ruleGroups)]
    pub fn rule_groups(&self) -> String {
        format!(
            "[{}]",
            self.inner
                .list_rule_groups()
                .iter()
                .map(microtypo_core::RuleGroupInfo::to_json)
                .collect::<Vec<_>>()
                .join(",")
        )
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
