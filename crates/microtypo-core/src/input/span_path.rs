use std::sync::{Arc, OnceLock};

use crate::errors::Result;
use crate::js::{self, JsString};
use crate::protect::blocks::{Range, ScanCtx};

pub type Next<'a> = Box<dyn FnMut(usize) -> Result<Option<(usize, usize, usize)>> + 'a>;

pub type ReaderFor = Arc<dyn for<'a> Fn(&'a [u16], &'a ScanCtx<'a>) -> Result<Box<dyn SpanReader + 'a>> + Send + Sync>;

#[derive(Clone)]
pub struct OwnedSpan {
    pub start: usize,
    pub end: usize,
    pub after: usize,
    pub tail: bool,
    pub report: Option<bool>,
    pub syntax: Vec<Range>,
}

impl OwnedSpan {
    pub fn new((start, end, after): (usize, usize, usize)) -> Self {
        Self { start, end, after, tail: false, report: None, syntax: Vec::new() }
    }
}

pub trait SpanReader {
    fn next(&mut self, from: usize) -> Result<Option<(usize, usize, usize)>>;
    fn read(&mut self, at: usize) -> Result<OwnedSpan>;
    fn header(&mut self, _at: usize) -> Result<Option<usize>> {
        Ok(None)
    }
    fn address(&self) -> bool {
        false
    }
}

pub fn precedes_inline(
    candidate: (usize, usize, usize),
    first: Option<(usize, usize, usize)>,
    first_address: bool,
) -> bool {
    first.is_none_or(|first| candidate.0 < first.0 || (candidate.0 == first.0 && first_address))
}

pub struct NextReader<'a> {
    next: Next<'a>,
    at: Option<(usize, usize, usize)>,
}
impl<'a> NextReader<'a> {
    pub fn new(next: Next<'a>) -> Self {
        Self { next, at: None }
    }
}
impl SpanReader for NextReader<'_> {
    fn next(&mut self, from: usize) -> Result<Option<(usize, usize, usize)>> {
        self.at = (self.next)(from)?;
        Ok(self.at)
    }
    fn read(&mut self, at: usize) -> Result<OwnedSpan> {
        Ok(OwnedSpan::new(self.at.filter(|s| s.0 == at).expect("selected candidate")))
    }
}

#[cfg(test)]
pub fn collect_owned(reader: &mut dyn SpanReader) -> Result<Vec<crate::protect::blocks::Spanned>> {
    use crate::protect::blocks::SpanKind;
    let mut spans = Vec::new();
    let mut from = 0;
    while let Some(at) = reader.next(from)? {
        let span = reader.read(at.0)?;
        if span.report != Some(false) {
            spans.push((span.start, span.end, if span.tail { SpanKind::Tail } else { SpanKind::Plain }));
        }
        from = span.after;
    }
    Ok(spans)
}

#[derive(Clone, Copy)]
enum Candidate {
    Unqueried,
    Exhausted,
    Present((usize, usize, usize)),
}

pub struct LineReader<'a> {
    readers: Vec<Box<dyn SpanReader + 'a>>,
    candidates: Vec<Candidate>,
    through: usize,
}
impl<'a> LineReader<'a> {
    pub fn new(readers: Vec<Box<dyn SpanReader + 'a>>) -> Self {
        Self { candidates: vec![Candidate::Unqueried; readers.len()], readers, through: 0 }
    }
    pub fn read(&mut self, from: usize, to: usize) -> Result<usize> {
        let mut cursor = from.max(self.through);
        while cursor < to {
            let mut first: Option<(usize, (usize, usize, usize))> = None;
            let mut first_address = false;
            for (k, reader) in self.readers.iter_mut().enumerate() {
                if matches!(self.candidates[k], Candidate::Unqueried)
                    || matches!(self.candidates[k], Candidate::Present(at) if at.0 < cursor)
                {
                    self.candidates[k] = reader.next(cursor)?.map_or(Candidate::Exhausted, Candidate::Present);
                }
                if let Candidate::Present(at) = self.candidates[k]
                    && precedes_inline(at, first.map(|(_, f)| f), first_address)
                {
                    first = Some((k, at));
                    first_address = reader.address();
                }
            }
            let Some((k, at)) = first.filter(|(_, at)| at.0 < to) else {
                break;
            };
            cursor = self.readers[k].read(at.0)?.after;
            self.through = cursor;
        }
        Ok(self.through)
    }
}

pub fn collect_spans(next: &mut Next<'_>) -> Result<Vec<Range>> {
    let mut spans = Vec::new();
    let mut from = 0;
    while let Some((start, end, after)) = next(from)? {
        spans.push((start, end));
        from = after;
    }
    Ok(spans)
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Position {
    Key(JsString),
    Index(usize),
}

impl Position {
    pub fn key(s: &[u16]) -> Self {
        Position::Key(s.to_vec())
    }

    pub fn to_js(&self) -> JsString {
        match self {
            Position::Key(k) => k.clone(),
            Position::Index(i) => js::num(*i),
        }
    }
}

#[derive(Debug)]
pub struct ChainNode {
    parent: Chain,
    position: Position,
    depth: usize,
    pieces: OnceLock<Vec<JsString>>,
}

impl ChainNode {
    fn pieces(&self) -> &[JsString] {
        self.pieces
            .get_or_init(|| js::split(&self.position.to_js(), &[0x2E]).into_iter().map(<[u16]>::to_vec).collect())
    }
}

impl Drop for ChainNode {
    fn drop(&mut self) {
        let mut next = self.parent.take();
        while let Some(node) = next {
            next = Arc::into_inner(node).and_then(|mut node| node.parent.take());
        }
    }
}

pub type Chain = Option<Arc<ChainNode>>;

pub fn chain_append(base: &Chain, positions: impl IntoIterator<Item = Position>) -> Chain {
    let mut chain = base.clone();
    for position in positions {
        let depth = chain.as_ref().map_or(0, |c| c.depth) + 1;
        chain = Some(Arc::new(ChainNode { parent: chain, position, depth, pieces: OnceLock::new() }));
    }
    chain
}

#[cfg(test)]
fn materialize(chain: &Chain) -> Vec<JsString> {
    let mut segments = Vec::new();
    let mut node = chain.as_ref();
    while let Some(n) = node {
        segments.push(n.position.to_js());
        node = n.parent.as_ref();
    }
    segments.reverse();
    segments
}

#[derive(Debug, Clone)]
pub struct LocatedSpan {
    pub start: usize,
    pub end: usize,
    pub is_key: bool,
    pub eligible: bool,
    pub delim: JsString,
    pub guard: Option<fn(&[u16]) -> bool>,
    pub unaddressed: bool,
    chain: Chain,
}

impl LocatedSpan {
    pub fn position(&self) -> Option<Position> {
        self.chain.as_ref().map(|node| node.position.clone())
    }
    pub fn new(start: usize, end: usize, is_key: bool, chain: Chain) -> Self {
        LocatedSpan { start, end, is_key, eligible: true, delim: Vec::new(), guard: None, unaddressed: false, chain }
    }

    pub fn with(
        start: usize,
        end: usize,
        is_key: bool,
        chain: Chain,
        eligible: bool,
        delim: JsString,
        guard: Option<fn(&[u16]) -> bool>,
    ) -> Self {
        LocatedSpan { start, end, is_key, eligible, delim, guard, unaddressed: false, chain }
    }

    #[cfg(test)]
    pub fn segments(&self) -> Vec<JsString> {
        materialize(&self.chain)
    }

    pub fn tail(&self) -> JsString {
        self.chain.as_ref().map_or_else(Vec::new, |c| c.position.to_js())
    }

    #[cfg(test)]
    pub fn path(&self) -> JsString {
        js::join(&self.segments(), &[0x2E])
    }

    pub fn dotted_tail(&self) -> JsString {
        self.chain.as_ref().map_or_else(Vec::new, |c| c.pieces().last().cloned().unwrap_or_default())
    }

    pub fn segments_match(&self, wanted: &[JsString]) -> bool {
        let mut node = self.chain.as_ref();
        if node.map_or(0, |n| n.depth) != wanted.len() {
            return false;
        }
        for w in wanted.iter().rev() {
            let n = node.expect("depth matches");
            if w.as_slice() != [0x2A] && *w != n.position.to_js() {
                return false;
            }
            node = n.parent.as_ref();
        }
        true
    }

    pub fn dotted_match(&self, wanted: &[JsString]) -> bool {
        let mut i = wanted.len() as isize;
        let mut node = self.chain.as_ref();
        while let Some(n) = node {
            for piece in n.pieces().iter().rev() {
                i -= 1;
                if i < 0 {
                    return false;
                }
                let w = &wanted[i as usize];
                if w.as_slice() != [0x2A] && w != piece {
                    return false;
                }
            }
            node = n.parent.as_ref();
        }
        if i == wanted.len() as isize {
            return i == 1 && (wanted[0].as_slice() == [0x2A] || wanted[0].is_empty());
        }
        i == 0
    }
}
