use serde_json::{Value, json};
use std::fmt::Write as _;

use crate::errors::{Error, Result};
use crate::js::{self, JsString};
use crate::shared::diff::Edit;
use crate::shared::rule_trace::TraceEntry;

/// The `changes`, `edits` and `trace` options of the reference `process(text, options)`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ProcessOptions {
    pub changes: bool,
    pub edits: bool,
    pub trace: bool,
}

impl ProcessOptions {
    pub fn read(options: Option<&Value>) -> Result<Self> {
        let mut result = Self::default();
        for (option, slot) in
            [("changes", &mut result.changes), ("edits", &mut result.edits), ("trace", &mut result.trace)]
        {
            if let Some(value) = options.and_then(Value::as_object).and_then(|o| o.get(option)) {
                *slot = value.as_bool().ok_or_else(|| {
                    Error::config(
                        format!("process() option \"{option}\" must be a boolean"),
                        json!({"option":option,"receivedType":crate::api::type_of(value)}),
                    )
                })?;
            }
        }
        Ok(result)
    }
}

/// What `process(text, options)` of the reference returns for [`ProcessOptions`]: a field is `Some` when its option is set. `changes` are ranges of `text`, `edits` ranges of the input, in UTF-16 code units.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Diagnostics {
    pub text: JsString,
    pub trace: Option<Vec<TraceEntry>>,
    pub changes: Option<Vec<(usize, usize)>>,
    pub edits: Option<Vec<Edit>>,
}

impl Diagnostics {
    pub fn to_json(&self) -> String {
        let quoted = |text: &[u16]| js::lossy(&js::json::stringify(text));
        let mut output = format!("{{\"text\":{}", quoted(&self.text));
        if let Some(trace) = &self.trace {
            output.push_str(",\"trace\":");
            output.push('[');
            for (i, entry) in trace.iter().enumerate() {
                if i > 0 {
                    output.push(',');
                }
                output.push_str(&entry.to_json());
            }
            output.push(']');
        }
        if let Some(changes) = &self.changes {
            output.push_str(",\"changes\":");
            output.push_str(&json!(changes).to_string());
        }
        if let Some(edits) = &self.edits {
            output.push_str(",\"edits\":[");
            for (i, edit) in edits.iter().enumerate() {
                if i > 0 {
                    output.push(',');
                }
                write!(output, "{{\"start\":{},\"end\":{},\"text\":{}}}", edit.start, edit.end, quoted(&edit.text))
                    .expect("write to String");
            }
            output.push(']');
        }
        output.push('}');
        output
    }
}
