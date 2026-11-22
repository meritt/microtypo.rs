use std::sync::{Arc, Mutex};

use crate::shared::provenance::{Projection, SourceMap};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TraceKind {
    Regex,
    Handler,
}

impl TraceKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Regex => "regex",
            Self::Handler => "handler",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TraceEntry {
    pub rule: crate::JsString,
    pub kind: TraceKind,
    pub count: usize,
    pub ranges: Vec<(usize, usize)>,
}

impl TraceEntry {
    pub fn to_json(&self) -> String {
        format!(
            "{{\"rule\":{},\"kind\":{},\"count\":{},\"ranges\":{}}}",
            crate::js::lossy(&crate::js::json::stringify(&self.rule)),
            serde_json::json!(self.kind.as_str()),
            self.count,
            serde_json::json!(self.ranges)
        )
    }
}

pub type TraceLog = Arc<Mutex<Vec<TraceEntry>>>;

pub struct Attempt {
    pub map: Arc<SourceMap>,
    pub input_map: Arc<SourceMap>,
    pub projection: Option<Projection>,
    pub count: usize,
    pub ranges: Vec<(usize, usize)>,
}

pub struct RuleTrace {
    pub frame: Arc<SourceMap>,
    pub entries: TraceLog,
    pub pending: Option<Attempt>,
    pub accepted: Vec<Attempt>,
    pub rule: crate::JsString,
    pub kind: TraceKind,
    pub retry_map: Option<Arc<SourceMap>>,
    pub retry_projection: Option<Projection>,
}

impl RuleTrace {
    pub fn new(frame: Arc<SourceMap>, entries: TraceLog) -> Self {
        Self {
            frame,
            entries,
            pending: None,
            accepted: Vec::new(),
            rule: Vec::new(),
            kind: TraceKind::Handler,
            retry_map: None,
            retry_projection: None,
        }
    }

    pub fn discard(&mut self) {
        self.pending = None;
        self.retry_map = None;
        self.retry_projection = None;
    }

    pub fn publish(&self) {
        let count: usize = self.accepted.iter().map(|event| event.count).sum();
        if count == 0 {
            return;
        }
        let mut entries = self.entries.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let at =
            entries.iter().position(|entry| entry.rule == self.rule && entry.kind == self.kind).unwrap_or_else(|| {
                entries.push(TraceEntry { rule: self.rule.clone(), kind: self.kind, count: 0, ranges: Vec::new() });
                entries.len() - 1
            });
        entries[at].count += count;
        for event in &self.accepted {
            entries[at].ranges.extend_from_slice(&event.ranges);
        }
    }
}

impl std::fmt::Debug for RuleTrace {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RuleTrace").field("rule", &self.rule).field("kind", &self.kind).finish_non_exhaustive()
    }
}

impl std::fmt::Debug for Attempt {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Attempt").field("count", &self.count).field("ranges", &self.ranges).finish_non_exhaustive()
    }
}
