use serde_json::{Value, json};

use crate::errors::{Error, Result};
use crate::js::raw_json::Node;
use crate::js::{self, Re};
use crate::rule_runner::{CompiledGroup, Replacement, Rule, RuleGroupDef, compile_rule_group};

pub fn data_pattern(pattern: Node<'_>) -> Result<Re> {
    let object = pattern.value.as_object();
    let source = object.and_then(|o| o.get("source")).and_then(Value::as_str);
    let flags = object.and_then(|o| o.get("flags")).and_then(Value::as_str);
    let (Some(source), Some(flags)) = (source, flags) else {
        return Err(pattern_shape_error());
    };
    if object.is_some_and(|o| o.keys().any(|k| k != "source" && k != "flags")) {
        return Err(pattern_shape_error());
    }
    Re::try_new_utf16(&pattern.key("source").units(), flags).map_err(|error| {
        Error::config(format!("invalid rule pattern: {}", error.0), json!({"source":source,"flags":flags}))
    })
}

fn pattern_shape_error() -> Error {
    let message = "pattern: expected { source: string, flags: string }";
    Error::config(message, json!({"issues":[message]}))
}

pub fn validate_data_groups(input: Node<'_>, issues: &mut Vec<String>) -> Result<()> {
    let Some(groups) = input.value.as_array() else {
        issues.push("ruleGroups: expected an array".into());
        return Ok(());
    };
    for (group_index, group) in groups.iter().enumerate() {
        let object = group.as_object();
        let name = group.get("name").and_then(Value::as_str).filter(|s| !s.is_empty());
        let rules = group.get("rules").and_then(Value::as_array);
        if object.is_none_or(|o| o.keys().any(|k| !["name", "title", "position", "rules"].contains(&k.as_str())))
            || name.is_none()
            || rules.is_none()
            || ["title", "position"].iter().any(|k| group.get(k).is_some_and(|v| !v.is_string()))
        {
            issues.push("ruleGroups: expected groups with name, rules, optional title and position".into());
            continue;
        }
        for (rule_index, rule) in rules.expect("checked array").iter().enumerate() {
            if rule.as_object().is_none_or(|o| {
                o.keys()
                    .any(|k| !["id", "pattern", "replacement", "enabled", "cycled", "htmlOnly"].contains(&k.as_str()))
            }) || rule.get("id").and_then(Value::as_str).is_none_or(str::is_empty)
                || !rule.get("replacement").is_some_and(Value::is_string)
                || ["enabled", "cycled", "htmlOnly"].iter().any(|k| rule.get(k).is_some_and(|v| !v.is_boolean()))
            {
                issues.push(format!(
                    "ruleGroups.{}: expected data rules with id, pattern and string replacement",
                    name.expect("checked name")
                ));
                continue;
            }
            data_pattern(input.index(group_index).key("rules").index(rule_index).key("pattern"))?;
        }
    }
    Ok(())
}

pub fn data_group(input: Node<'_>, override_name: Option<&[u16]>) -> Result<CompiledGroup> {
    let group = input.value;
    let name_units = override_name
        .filter(|name| !name.is_empty())
        .map(<[u16]>::to_vec)
        .or_else(|| {
            group.get("name").and_then(Value::as_str).filter(|name| !name.is_empty()).map(|_| input.key("name").units())
        })
        .or_else(|| {
            group
                .get("title")
                .and_then(Value::as_str)
                .filter(|name| !name.is_empty())
                .map(|_| input.key("title").units())
        })
        .ok_or_else(|| Error::type_error("Rule group must have a name"))?;
    let name_units = js::to_lower(&name_units);
    let name = js::lossy(&name_units);
    let Some(rules) = group.get("rules").and_then(Value::as_array) else {
        return Err(Error::type_error(format!("Rule group '{name}': rules must be an array")));
    };
    let mut compiled = Vec::with_capacity(rules.len());
    let mut ids = Vec::with_capacity(rules.len());
    for (rule_index, rule) in rules.iter().enumerate() {
        let rule_input = input.key("rules").index(rule_index);
        if !rule.is_object() {
            return Err(Error::type_error(format!("Rule group '{name}': rule must be an object")));
        }
        let Some(id) = rule.get("id").and_then(Value::as_str).filter(|id| !id.is_empty()) else {
            return Err(Error::type_error(format!("Rule group '{name}': rule.id must be a non-empty string")));
        };
        let pattern = rule.get("pattern").filter(|v| !v.is_null()).ok_or_else(|| {
            Error::type_error(format!(
                "Rule '{name}.{id}': must define either {{ pattern, replacement }} or {{ handler }}"
            ))
        })?;
        let patterns = pattern.as_array().map_or_else(|| vec![pattern], |a| a.iter().collect());
        let patterns = patterns
            .into_iter()
            .enumerate()
            .map(|(index, pattern)| {
                if pattern.is_object() && !pattern.is_null() {
                    let pattern = rule_input.key("pattern");
                    data_pattern(if pattern.value.is_array() { pattern.index(index) } else { pattern })
                } else {
                    let kind = match pattern {
                        Value::Null | Value::Array(_) | Value::Object(_) => "object",
                        Value::Bool(_) => "boolean",
                        Value::Number(_) => "number",
                        Value::String(_) => "string",
                    };
                    Err(Error::type_error(format!("Rule '{name}.{id}': pattern must be RegExp (got {kind})")))
                }
            })
            .collect::<Result<Vec<_>>>()?;
        let replacement = rule.get("replacement").ok_or_else(|| {
            Error::config("data rule replacement must be a string", json!({"rule":format!("{name}.{id}")}))
        })?;
        let replacements = if let Some(array) = replacement.as_array() {
            if array.iter().any(|v| !v.is_string()) {
                return Err(Error::config(
                    "data rule replacement must be a string",
                    json!({"rule":format!("{name}.{id}")}),
                ));
            }
            if array.len() != patterns.len() {
                return Err(Error::type_error(format!(
                    "Rule '{name}.{id}': replacement array length ({}) must match pattern array length ({})",
                    array.len(),
                    patterns.len()
                )));
            }
            array.iter().enumerate().map(|(i, _)| rule_input.key("replacement").index(i).units()).collect::<Vec<_>>()
        } else if replacement.is_string() {
            vec![rule_input.key("replacement").units(); patterns.len()]
        } else {
            return Err(Error::config(
                "data rule replacement must be a string",
                json!({"rule":format!("{name}.{id}")}),
            ));
        };
        let mut item = Rule::patterns(
            id,
            patterns.into_iter().zip(replacements).map(|(p, r)| (p, Replacement::Template(r))).collect(),
        );
        item.enabled = rule.get("enabled") != Some(&Value::Bool(false));
        item.cycled = rule.get("cycled").is_some_and(crate::api::truthy);
        item.html_only = rule.get("htmlOnly").is_some_and(crate::api::truthy);
        item.description = rule.get("description").and_then(Value::as_str).unwrap_or_default().into();
        compiled.push(item);
        ids.push(rule_input.key("id").units());
    }
    let definition = RuleGroupDef {
        name: Some(name.clone()),
        title: group.get("title").and_then(Value::as_str).map(str::to_owned),
        rules: compiled,
        classes: group.get("classes").and_then(Value::as_object).cloned(),
        pre_parse: None,
    };
    let mut compiled = compile_rule_group(&definition, &name)?;
    compiled.name = name_units.clone();
    compiled.title = if group.get("title").and_then(Value::as_str).is_some_and(|s| !s.is_empty()) {
        input.key("title").units()
    } else {
        name_units
    };
    for (rule, id) in std::sync::Arc::make_mut(&mut compiled.rules).iter_mut().zip(ids) {
        rule.id = id;
    }
    Ok(compiled)
}
