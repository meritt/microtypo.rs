use std::path::{Path, PathBuf};

use serde_json::{Map, Value};

use crate::errors::{Result, usage};
use crate::js;
use crate::paths::resolve;

const RC_NAME: &str = ".microtyporc.json";

pub fn load_config_file(explicit: Option<&str>, cwd: &Path) -> Result<(Map<String, Value>, PathBuf, Option<String>)> {
    let path = match explicit {
        Some(p) if !p.is_empty() => resolve(cwd, p),
        _ => match discover(cwd) {
            Some(p) => p,
            None => return Ok((Map::new(), cwd.to_path_buf(), None)),
        },
    };
    let Ok(bytes) = std::fs::read(&path) else {
        return Err(usage(format!("cannot read config file: {}", path.display())));
    };
    let Some(config) = js::parse_json(&String::from_utf8_lossy(&bytes)) else {
        return Err(usage(format!("invalid JSON in config file: {}", path.display())));
    };
    match config {
        Value::Null => Ok((Map::new(), crate::paths::dirname(&path), None)),
        Value::Object(map) => {
            Ok((map, crate::paths::dirname(&path), Some(String::from_utf8_lossy(&bytes).into_owned())))
        }
        other => {
            let got = match other {
                Value::Array(_) => "an array",
                Value::String(_) => "string",
                Value::Number(_) => "number",
                _ => "boolean",
            };
            Err(usage(format!("config file must hold an object, got {got}: {}", path.display())))
        }
    }
}

fn discover(cwd: &Path) -> Option<PathBuf> {
    let mut dir = Some(cwd);
    while let Some(d) = dir {
        let candidate = d.join(RC_NAME);
        if std::fs::exists(&candidate).unwrap_or(false) {
            return Some(candidate);
        }
        dir = d.parent();
    }
    None
}
