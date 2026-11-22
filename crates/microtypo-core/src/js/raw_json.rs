use super::{JsString, json, lossy, u};
use crate::errors::{Error, Result};
use crate::input::span_path::Position;
use serde_json::Value;
use std::collections::HashMap;

type Slot = (usize, Position);

#[derive(Debug)]
pub struct RawJson {
    pub value: Value,
    strings: HashMap<Slot, JsString>,
    keys: HashMap<usize, Vec<(JsString, Value)>>,
    containers: HashMap<Slot, usize>,
    root: Option<usize>,
}

impl RawJson {
    pub fn parse(source: &str) -> Result<Self> {
        let units = u(source);
        let invalid = |message: String| {
            Error::config(format!("invalid config JSON: {message}"), serde_json::json!({"reason":"json"}))
        };
        json::validate(&units).map_err(|e| invalid(e.message))?;
        let scanned = crate::input::json::scan_json_owned(&units, &|_| Ok(()), true)?;
        let root = scanned.containers.first().filter(|(_, parent)| parent.is_none()).map(|(start, _)| *start);
        let containers =
            scanned.containers.into_iter().filter_map(|(start, parent)| parent.map(|slot| (slot, start))).collect();
        let mut strings = HashMap::new();
        let mut normalized = String::new();
        let mut cursor = 0;
        let mut pending = Vec::new();
        for (span, owner) in scanned.spans.into_iter().zip(scanned.owners) {
            normalized.push_str(&lossy(&units[cursor..span.start]));
            let decoded = json::parse_string(&units[span.start..span.end]).map_err(|e| invalid(e.message))?;
            normalized.push_str(&serde_json::to_string(&lossy(&decoded)).expect("string serialization"));
            let position = span.position();
            if span.is_key {
                if matches!(&position,Some(Position::Key(key)) if *key == u("rules")) {
                    pending.push((owner.expect("key owner"), decoded, normalized.len()));
                }
            } else {
                strings.insert((owner.unwrap_or(usize::MAX), position.unwrap_or(Position::Index(0))), decoded);
            }
            cursor = span.end;
        }
        normalized.push_str(&lossy(&units[cursor..]));
        let value = serde_json::from_str(&normalized).map_err(|e| invalid(e.to_string()))?;
        let mut keys: HashMap<usize, Vec<(JsString, Value)>> = HashMap::new();
        let mut indexes: HashMap<usize, HashMap<JsString, usize>> = HashMap::new();
        for (owner, key, end) in pending {
            let remainder = normalized[end..].trim_start().strip_prefix(':').expect("validated key").trim_start();
            let value = serde_json::Deserializer::from_str(remainder)
                .into_iter::<Value>()
                .next()
                .expect("validated value")
                .expect("normalized value");
            let entries = keys.entry(owner).or_default();
            let index = indexes.entry(owner).or_default();
            if let Some(&at) = index.get(&key) {
                entries[at].1 = value;
            } else {
                index.insert(key.clone(), entries.len());
                entries.push((key, value));
            }
        }
        for entries in keys.values_mut() {
            entries.sort_by_key(|(key, _)| {
                let index = String::from_utf16(key)
                    .ok()
                    .and_then(|s| s.parse::<u32>().ok().filter(|&n| n < u32::MAX && n.to_string() == s));
                index.map_or((1, 0), |n| (0, n))
            });
        }
        Ok(Self { value, strings, keys, containers, root })
    }

    pub fn to_json(&self) -> String {
        Node::root(&self.value, Some(self)).to_json()
    }

    pub fn merge(&self, other: &Self) -> Result<Self> {
        Self::parse(&merge_nodes(Node::root(&self.value, Some(self)), Node::root(&other.value, Some(other))))
    }
}

#[derive(Debug, Clone)]
#[must_use]
pub struct Node<'a> {
    pub value: &'a Value,
    raw: Option<&'a RawJson>,
    slot: Option<Slot>,
    container: Option<usize>,
}

impl<'a> Node<'a> {
    pub fn root(value: &'a Value, raw: Option<&'a RawJson>) -> Self {
        Self { value, raw, slot: Some((usize::MAX, Position::Index(0))), container: raw.and_then(|raw| raw.root) }
    }

    fn child<'b>(&self, value: &'b Value, position: Position) -> Node<'b>
    where
        'a: 'b,
    {
        let slot = self.container.map(|owner| (owner, position));
        let container = if value.is_object() || value.is_array() {
            self.raw.zip(slot.as_ref()).and_then(|(raw, slot)| raw.containers.get(slot).copied())
        } else {
            None
        };
        Node { value, raw: self.raw, slot, container }
    }

    pub fn key(&self, key: &str) -> Self {
        self.child(&self.value[key], Position::Key(u(key)))
    }
    pub fn index(&self, index: usize) -> Self {
        self.child(&self.value[index], Position::Index(index))
    }
    pub fn argument(&self, index: usize) -> Self {
        if self.value.is_array() {
            self.index(index)
        } else if index == 0 {
            self.clone()
        } else {
            Self::root(&Value::Null, None)
        }
    }

    pub fn units(&self) -> JsString {
        self.raw
            .zip(self.slot.as_ref())
            .and_then(|(raw, slot)| raw.strings.get(slot))
            .cloned()
            .unwrap_or_else(|| u(self.value.as_str().unwrap_or_default()))
    }

    pub fn entries(&self) -> Vec<(JsString, Value)> {
        if let Some(entries) = self.raw.zip(self.container).and_then(|(raw, owner)| raw.keys.get(&owner)) {
            return entries.clone();
        }
        self.value.as_object().map_or_else(Vec::new, |o| o.iter().map(|(k, v)| (u(k), v.clone())).collect())
    }

    pub fn to_json(&self) -> String {
        match self.value {
            Value::String(_) => lossy(&json::stringify(&self.units())),
            Value::Array(array) => format!(
                "[{}]",
                array.iter().enumerate().map(|(i, _)| self.index(i).to_json()).collect::<Vec<_>>().join(",")
            ),
            Value::Object(_) => format!(
                "{{{}}}",
                self.entries()
                    .iter()
                    .map(|(key, value)| format!(
                        "{}:{}",
                        lossy(&json::stringify(key)),
                        self.child(value, Position::Key(key.clone())).to_json()
                    ))
                    .collect::<Vec<_>>()
                    .join(",")
            ),
            other => other.to_string(),
        }
    }
}

fn merge_nodes(base: Node<'_>, other: Node<'_>) -> String {
    if !base.value.is_object() || !other.value.is_object() {
        return other.to_json();
    }
    let original = base.entries();
    let additions = other.entries();
    let positions: HashMap<_, _> = additions.iter().enumerate().map(|(i, (key, _))| (key, i)).collect();
    let original_keys: std::collections::HashSet<_> = original.iter().map(|(key, _)| key).collect();
    let mut entries = Vec::new();
    for (key, value) in &original {
        let serialized = positions.get(key).map_or_else(
            || base.child(value, Position::Key(key.clone())).to_json(),
            |&at| {
                merge_nodes(
                    base.child(value, Position::Key(key.clone())),
                    other.child(&additions[at].1, Position::Key(key.clone())),
                )
            },
        );
        entries.push(format!("{}:{serialized}", lossy(&json::stringify(key))));
    }
    for (key, value) in &additions {
        if !original_keys.contains(key) {
            entries.push(format!(
                "{}:{}",
                lossy(&json::stringify(key)),
                other.child(value, Position::Key(key.clone())).to_json()
            ));
        }
    }
    format!("{{{}}}", entries.join(","))
}
