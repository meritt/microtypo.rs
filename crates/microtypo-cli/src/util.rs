use serde_json::{Map, Value};

use crate::errors::{Result, usage};
use crate::js;

const UNSAFE_KEYS: [&str; 3] = ["__proto__", "prototype", "constructor"];

fn assert_safe_key(key: &str) -> Result<()> {
    if UNSAFE_KEYS.contains(&key) {
        return Err(usage(format!("unsafe config key: \"{key}\"")));
    }
    Ok(())
}

pub fn assert_safe_merge(base: &Map<String, Value>, overrides: &Map<String, Value>) -> Result<()> {
    for (key, value) in overrides {
        assert_safe_key(key)?;
        if let (Value::Object(value), Some(Value::Object(current))) = (value, base.get(key)) {
            assert_safe_merge(current, value)?;
        }
    }
    Ok(())
}

pub fn set_path(target: &mut Map<String, Value>, path: &str, value: Value) -> Result<()> {
    let keys: Vec<&str> = path.split('.').collect();
    for key in &keys {
        assert_safe_key(key)?;
    }
    let (last, parents) = keys.split_last().expect("split yields at least one key");
    let mut node = target;
    for key in parents {
        let entry = node.entry((*key).to_owned()).or_insert(Value::Null);
        if !entry.is_object() {
            *entry = Value::Object(Map::new());
        }
        node = entry.as_object_mut().expect("made an object above");
    }
    node.insert((*last).to_owned(), value);
    Ok(())
}

pub fn coerce_scalar(raw: &str) -> Value {
    match raw {
        "true" => Value::Bool(true),
        "false" => Value::Bool(false),
        _ => {
            let n = js::to_number(raw);
            if raw.is_empty() || n.is_nan() { Value::String(raw.to_owned()) } else { js::number(n) }
        }
    }
}
