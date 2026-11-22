use std::sync::Arc;

use crate::errors::Result;
use crate::js::{self, JsString, Re};
use crate::shared::diff::{Edit, replacement_edits};

pub fn replace_recorded(
    text: JsString,
    re: &Re,
    replacement: impl FnMut(&js::Caps<'_>) -> JsString,
    mapping: Option<&mut RestoreMap>,
) -> JsString {
    match mapping {
        Some(mapping) => mapping.replace(text, re, replacement),
        None => re.replace_owned(text, replacement),
    }
}

#[derive(Debug, Clone)]
pub struct SourceMap {
    pub text: JsString,
    start: Vec<usize>,
    end: Vec<usize>,
    offset: usize,
}

impl SourceMap {
    pub fn new(text: &[u16], offset: usize, check: &dyn Fn(&str) -> Result<()>) -> Result<Arc<Self>> {
        let mut map = Self { text: text.to_vec(), start: vec![0; text.len()], end: vec![0; text.len()], offset };
        let mut i = 0;
        while i < text.len() {
            if i & 0x3fff == 0 {
                check("trace")?;
            }
            let length = if js::code_point_at(text, i).is_some_and(|c| c > 0xffff) { 2 } else { 1 };
            map.start[i..i + length].fill(offset + i);
            map.end[i..i + length].fill(offset + i + length);
            i += length;
        }
        Ok(Arc::new(map))
    }

    pub fn range(&self, start: usize, end: usize, check: &dyn Fn(&str) -> Result<()>) -> Result<(usize, usize)> {
        if start == end {
            let at = self
                .start
                .get(start)
                .copied()
                .or_else(|| start.checked_sub(1).and_then(|i| self.end.get(i).copied()))
                .unwrap_or(self.offset);
            return Ok((at, at));
        }
        let mut from = usize::MAX;
        let mut to = 0;
        for i in start..end {
            if i & 0x3fff == 0 {
                check("trace")?;
            }
            from = from.min(self.start[i]);
            to = to.max(self.end[i]);
        }
        Ok((from, to))
    }

    pub fn slice(&self, start: usize, end: usize, check: &dyn Fn(&str) -> Result<()>) -> Result<Arc<Self>> {
        Ok(Arc::new(Self {
            text: self.text[start..end].to_vec(),
            start: self.start[start..end].to_vec(),
            end: self.end[start..end].to_vec(),
            offset: self.range(start, start, check)?.0,
        }))
    }

    pub fn fork(self: &Arc<Self>, text: &[u16], check: &dyn Fn(&str) -> Result<()>) -> Result<Arc<Self>> {
        if self.text == text {
            return Ok(self.clone());
        }
        let edits = replacement_edits(&self.text, text, check)?.into_iter().map(MapEdit::from).collect::<Vec<_>>();
        self.edited(text, &edits, check)
    }

    pub fn edited(
        self: &Arc<Self>,
        text: &[u16],
        edits: &[MapEdit],
        check: &dyn Fn(&str) -> Result<()>,
    ) -> Result<Arc<Self>> {
        if edits.is_empty() {
            return Ok(self.clone());
        }
        let mut next = Self {
            text: text.to_vec(),
            start: Vec::with_capacity(text.len()),
            end: Vec::with_capacity(text.len()),
            offset: self.offset,
        };
        let mut a = 0;
        for edit in edits {
            next.start.extend_from_slice(&self.start[a..edit.start]);
            next.end.extend_from_slice(&self.end[a..edit.start]);
            if let Some(source) = &edit.source_map {
                next.start.extend_from_slice(&source.start);
                next.end.extend_from_slice(&source.end);
            } else {
                let (from, to) = self.range(edit.start, edit.end, check)?;
                next.start.resize(next.start.len() + edit.text.len(), from);
                next.end.resize(next.end.len() + edit.text.len(), to);
            }
            a = edit.end;
        }
        next.start.extend_from_slice(&self.start[a..]);
        next.end.extend_from_slice(&self.end[a..]);
        Ok(Arc::new(next))
    }
}

#[derive(Debug, Clone)]
pub struct MapEdit {
    pub start: usize,
    pub end: usize,
    pub text: JsString,
    pub source_map: Option<Arc<SourceMap>>,
    pub retain: bool,
}

impl From<Edit> for MapEdit {
    fn from(edit: Edit) -> Self {
        Self { start: edit.start, end: edit.end, text: edit.text, source_map: None, retain: false }
    }
}

#[derive(Debug, Clone)]
struct Stage {
    before: JsString,
    after: JsString,
    edits: Vec<MapEdit>,
}

#[derive(Debug, Default, Clone)]
pub struct RestoreMap {
    stages: Vec<Stage>,
}

#[derive(Clone)]
pub struct Projection {
    pub text: JsString,
    pub materialized: JsString,
    pub tokens: Arc<crate::inline_lift::Toks>,
    pub edits: Vec<MapEdit>,
}

pub fn represented_map(
    map: Arc<SourceMap>,
    text: &[u16],
    source_projection: Option<&Projection>,
    mut target_projection: Option<&mut Projection>,
    render: impl Fn(JsString, &mut RestoreMap) -> JsString,
    check: &dyn Fn(&str) -> Result<()>,
) -> Result<Arc<SourceMap>> {
    if map.text == text && source_projection.is_none() && target_projection.is_none() {
        return Ok(map);
    }
    let mut source = RestoreMap::default();
    let input = if let Some(projection) = source_projection {
        source.record(projection.text.clone(), projection.materialized.clone(), projection.edits.clone());
        projection.materialized.clone()
    } else {
        map.text.clone()
    };
    render(input, &mut source);
    let mut target = RestoreMap::default();
    let input = if let Some(projection) = &target_projection {
        target.record(projection.text.clone(), projection.materialized.clone(), projection.edits.clone());
        projection.materialized.clone()
    } else {
        text.to_vec()
    };
    let rendered = render(input, &mut target);
    let result = target.backward(source.forward(map, check)?.fork(&rendered, check)?, check)?;
    if let Some(projection) = target_projection.as_mut()
        && !projection.edits.is_empty()
    {
        projection.edits.clone_from(&target.stages[0].edits);
    }
    Ok(result)
}

impl RestoreMap {
    pub fn replace(
        &mut self,
        text: JsString,
        re: &Re,
        mut replacement: impl FnMut(&js::Caps<'_>) -> JsString,
    ) -> JsString {
        let mut edits = Vec::new();
        let output = re.replace_with(&text, |m| {
            let value = replacement(m);
            if value != m.whole() {
                edits.push(MapEdit::from(Edit { start: m.index(), end: m.end(), text: value.clone() }));
            }
            value
        });
        self.record(text, output.clone(), edits);
        output
    }

    pub fn record(&mut self, before: JsString, after: JsString, edits: Vec<MapEdit>) {
        if !edits.is_empty() {
            self.stages.push(Stage { before, after, edits });
        }
    }

    pub fn forward(&self, mut map: Arc<SourceMap>, check: &dyn Fn(&str) -> Result<()>) -> Result<Arc<SourceMap>> {
        for stage in &self.stages {
            map = map.edited(&stage.after, &stage.edits, check)?;
        }
        Ok(map)
    }

    pub fn backward(&mut self, mut map: Arc<SourceMap>, check: &dyn Fn(&str) -> Result<()>) -> Result<Arc<SourceMap>> {
        for stage in self.stages.iter_mut().rev() {
            let mut inverse = Vec::with_capacity(stage.edits.len());
            let mut a = 0;
            let mut b = 0;
            for edit in &mut stage.edits {
                let start = b + edit.start - a;
                if edit.retain {
                    edit.source_map = Some(map.slice(start, start + edit.text.len(), check)?);
                }
                inverse.push(MapEdit::from(Edit {
                    start,
                    end: start + edit.text.len(),
                    text: stage.before[edit.start..edit.end].to_vec(),
                }));
                a = edit.end;
                b = start + edit.text.len();
            }
            map = map.edited(&stage.before, &inverse, check)?;
        }
        Ok(map)
    }
}

pub fn json_source_map(
    raw: &[u16],
    offset: usize,
    decoded: &[u16],
    check: &dyn Fn(&str) -> Result<()>,
) -> Result<Arc<SourceMap>> {
    let map = SourceMap::new(decoded, offset + 1, check)?;
    let mut map = Arc::try_unwrap(map).expect("new map");
    let mut at = 0;
    let mut i = 1;
    while i < raw.len() - 1 {
        if js::code_point_at(raw, i).is_some_and(|c| c > 0xffff) {
            map.start[at..at + 2].fill(offset + i);
            map.end[at..at + 2].fill(offset + i + 2);
            i += 2;
            at += 2;
            continue;
        }
        let length = if raw[i] == 0x5c { if raw.get(i + 1) == Some(&0x75) { 6 } else { 2 } } else { 1 };
        map.start[at] = offset + i;
        map.end[at] = offset + i + length;
        at += 1;
        i += length;
    }
    Ok(Arc::new(map))
}
