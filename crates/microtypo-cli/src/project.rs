use std::path::{Path, PathBuf};

use microtypo_core::MicroTypo;
use microtypo_core::internals::{JsonNode, RawJson};
use serde_json::{Value, json};

use crate::args::Options;
use crate::config::load_config_file;
use crate::errors::{Result, usage};
use crate::paths::{Masks, inferred_format, relative, resolve, validate_masks};
use crate::util::assert_safe_merge;

pub struct Project {
    pub root: PathBuf,
    base: RawJson,
    overrides: Vec<Override>,
}

struct Override {
    files: Masks,
    exclude: Option<Masks>,
    options: RawJson,
}

impl Project {
    pub fn load(opts: &Options, cwd: &Path) -> Result<Self> {
        let (base, root, source) = load_config_file(opts.config_path.as_deref(), cwd)?;
        let mut raw = source
            .as_deref()
            .and_then(|s| RawJson::parse(s).ok())
            .map_or_else(|| RawJson::parse(&Value::Object(base.clone()).to_string()), Ok)?;
        let overrides = base.get("overrides").cloned().unwrap_or_else(|| json!([]));
        let overrides = overrides.as_array().ok_or_else(|| usage("overrides: expected an array"))?;
        let options = (0..overrides.len())
            .map(|index| {
                let item = JsonNode::root(&raw.value, Some(&raw)).key("overrides").index(index);
                if item.value.get("options").is_some_and(Value::is_object) {
                    RawJson::parse(&item.key("options").to_json()).map(Some)
                } else {
                    Ok(None)
                }
            })
            .collect::<std::result::Result<Vec<_>, _>>()?;
        raw.value.as_object_mut().expect("loaded object").remove("overrides");
        let mut compiled = Vec::new();
        for (item, options) in overrides.iter().zip(options) {
            let Some(options) = options.filter(|_| {
                item.as_object()
                    .is_some_and(|o| o.keys().all(|k| ["files", "exclude", "options"].contains(&k.as_str())))
                    && item.get("files").is_some_and(validate_masks)
                    && item.get("exclude").is_none_or(validate_masks)
            }) else {
                return Err(usage("overrides: expected { files, exclude?, options }"));
            };
            let merged = merge(&raw, &options)?;
            MicroTypo::from_json(&merged.to_json(), super::run::HOST)?;
            compiled.push(Override {
                files: Masks::compile(&item["files"]),
                exclude: item.get("exclude").map(Masks::compile),
                options,
            });
        }
        let project = Self { root, base: raw, overrides: compiled };
        MicroTypo::from_json(&project.config_for(None, opts, cwd)?.to_json(), super::run::HOST)?;
        Ok(project)
    }

    pub fn config_for(&self, file: Option<&str>, opts: &Options, cwd: &Path) -> Result<RawJson> {
        let defaults = json!({"maxInputLength":5_000_000,"input":{"format":inferred_format(file.unwrap_or_default())}});
        let mut config = merge(&RawJson::parse(&defaults.to_string())?, &self.base)?;
        if let Some(file) = file {
            let path = relative(&self.root, &resolve(cwd, file));
            for item in &self.overrides {
                if item.files.matches(&path) && item.exclude.as_ref().is_none_or(|masks| !masks.matches(&path)) {
                    config = merge(&config, &item.options)?;
                }
            }
        }
        let mut config = merge(&config, &RawJson::parse(&Value::Object(opts.config.clone()).to_string())?)?;
        crate::js::order_keys(&mut config.value);
        Ok(config)
    }
}

fn merge(base: &RawJson, overrides: &RawJson) -> Result<RawJson> {
    if overrides.value.get("input").is_some_and(Value::is_string) {
        let format = JsonNode::root(&overrides.value, Some(overrides)).key("input").to_json();
        let overrides = overrides.merge(&RawJson::parse(&format!(r#"{{"input":{{"format":{format}}}}}"#))?)?;
        return merge(base, &overrides);
    }
    assert_safe_merge(
        base.value.as_object().expect("config object"),
        overrides.value.as_object().expect("options object"),
    )?;
    Ok(base.merge(overrides)?)
}
