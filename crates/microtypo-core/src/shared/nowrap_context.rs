use std::collections::{HashMap, HashSet};
use std::sync::{Arc, LazyLock};

use crate::cat;
use crate::errors::Result;
use crate::inline_lift::LiftProjection;
use crate::js::{self, JsString, Re};
use crate::protect::placeholders::{CLOSE, OPEN, TAG_PLACEHOLDER_SOURCE};
use crate::protect::tags::NameMatcher;
use crate::rule_runner::Ctx;
use crate::shared::block_tags::VOID_TAG_NAMES;
use crate::shared::inline_boundaries::{inline_end_tag_start, token_tag_name, token_tag_raw};

pub static NOWRAP_TAG_RE: LazyLock<Re> = LazyLock::new(|| Re::new(&TAG_PLACEHOLDER_SOURCE, "g"));
static WRAPPER_NAME_RE: LazyLock<Re> = LazyLock::new(|| Re::new(r"^(?:span|nobr)$", "u"));
static RESETS_RE: LazyLock<Re> = LazyLock::new(|| Re::new(r"\s(?:style|class)\s*=", "iu"));

pub(crate) struct NowrapOpen {
    group: usize,
    open: JsString,
}

fn group_identity(ctx: &Ctx<'_>) -> usize {
    ctx.group.as_ref().map_or(0, |g| Arc::as_ptr(g) as usize)
}

pub fn canonical_nowrap_open(ctx: &mut Ctx<'_>) -> Result<JsString> {
    let group = group_identity(ctx);
    if let Some(known) = ctx.bound.nowrap_open.as_ref()
        && known.group == group
    {
        return Ok(known.open.clone());
    }
    let marker = js::u("###");
    let sample = ctx.tag(&marker, "span", &[("class", "nowrap"), ("__style", "white-space:nowrap;")])?;
    let at = js::index_of(&sample, &marker, 0).unwrap_or(sample.len().saturating_sub(1));
    let open = sample[..at].to_vec();
    ctx.bound.nowrap_open = Some(NowrapOpen { group, open: open.clone() });
    Ok(open)
}

pub fn canonical_nowrap_content_start(
    ctx: &mut Ctx<'_>,
    source: &[u16],
    at: usize,
    skip_padding: bool,
) -> Result<Option<usize>> {
    let open = canonical_nowrap_open(ctx)?;
    if open.is_empty() || !js::starts_with_at(source, &open, at) {
        return Ok(None);
    }
    let mut cursor = at;
    loop {
        cursor += open.len();
        if skip_padding {
            while matches!(source.get(cursor), Some(0x20 | 0x09 | 0xA0)) {
                cursor += 1;
            }
        }
        (ctx.check_budget)("nowrap:content")?;
        if !js::starts_with_at(source, &open, cursor) {
            return Ok(Some(cursor));
        }
    }
}

pub fn span_token_set(ctx: &mut Ctx<'_>) -> HashSet<JsString> {
    ctx.find_by_tag_name(&NameMatcher::Regex(&WRAPPER_NAME_RE))
        .into_iter()
        .map(|(prefix, id)| cat!(OPEN, prefix, js::num(id), CLOSE))
        .collect()
}

#[derive(Debug, Clone, Copy)]
pub struct NowrapSpan {
    pub start: usize,
    pub end: usize,
    nowrap: bool,
    nested: bool,
}

pub fn nowrap_contexts(ctx: &mut Ctx<'_>, text: &[u16], open: &[u16]) -> Vec<NowrapSpan> {
    let spans = span_token_set(ctx);
    let mut stack: Vec<NowrapSpan> = Vec::new();
    let mut drops = Vec::new();
    for m in NOWRAP_TAG_RE.find_all(text) {
        let mut span = NowrapSpan { start: m.index(), end: m.end(), nowrap: m.whole() == open, nested: false };
        if !spans.contains(m.s(2)) {
            if let Some(enclosing) = stack.last_mut() {
                enclosing.nowrap = false;
            }
            continue;
        }
        if m.s(1) != [0x2F] {
            span.nested = span.nowrap && stack.last().is_some_and(|s| s.nowrap);
            stack.push(span);
        } else if let Some(opened) = stack.pop()
            && opened.nested
        {
            drops.push(opened);
            drops.push(span);
        }
    }
    drops
}

#[derive(Debug, Clone, Copy)]
struct Element {
    covered: bool,
    parent: usize,
    depth: usize,
}

const ROOT: usize = 0;

fn is_void(name: &[u16]) -> bool {
    VOID_TAG_NAMES.iter().any(|v| js::eq(name, v))
}

fn element_stretches(ctx: &mut Ctx<'_>, text: &[u16], open: &[u16]) -> (Vec<usize>, Vec<usize>, Vec<Element>) {
    let spans = span_token_set(ctx);
    let mut tree = vec![Element { covered: false, parent: ROOT, depth: 0 }];
    let mut names: Vec<JsString> = vec![Vec::new()];
    let mut starts = Vec::new();
    let mut nodes = Vec::new();
    let mut open_names: HashMap<JsString, usize> = HashMap::new();
    let mut node = ROOT;
    let mut cursor = 0;
    for m in NOWRAP_TAG_RE.find_all(text) {
        starts.push(cursor);
        nodes.push(node);
        cursor = m.end();
        let span = spans.contains(m.s(2));
        let name = token_tag_name(ctx.names(), m.s(2)).or_else(|| span.then(|| js::u("span")));
        let Some(name) = name else { continue };
        if !span && is_void(&name) {
            continue;
        }
        if m.s(1) == [0x2F] {
            if open_names.get(&name).is_some_and(|&n| n > 0) {
                while node != ROOT {
                    let closed = node;
                    if let Some(n) = open_names.get_mut(&names[closed]) {
                        *n -= 1;
                    }
                    node = tree[closed].parent;
                    if names[closed] == name {
                        break;
                    }
                }
            }
            continue;
        }
        *open_names.entry(name.clone()).or_insert(0) += 1;
        let resets = token_tag_raw(ctx.safe_tags, m.s(2)).is_some_and(|raw| RESETS_RE.test(raw));
        let covered = m.whole() == open || (!resets && tree[node].covered);
        tree.push(Element { covered, parent: node, depth: tree[node].depth + 1 });
        names.push(name);
        node = tree.len() - 1;
    }
    starts.push(cursor);
    nodes.push(node);
    (starts, nodes, tree)
}

fn stretch_at(starts: &[usize], at: usize, from: usize) -> usize {
    let mut i = from;
    while i + 1 < starts.len() && starts[i + 1] <= at {
        i += 1;
    }
    i
}

fn common_element(tree: &[Element], a: usize, b: usize) -> usize {
    let (mut x, mut y) = (a, b);
    while tree[x].depth > tree[y].depth {
        x = tree[x].parent;
    }
    while tree[y].depth > tree[x].depth {
        y = tree[y].parent;
    }
    while x != y {
        x = tree[x].parent;
        y = tree[y].parent;
    }
    x
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
struct Identity(usize, usize);

impl Identity {
    fn of(text: &[u16]) -> Self {
        Identity(text.as_ptr() as usize, text.len())
    }
}

#[derive(Debug, Default, Clone, Copy)]
pub(crate) struct SameText(Option<Identity>);

impl SameText {
    pub(crate) fn holds(&mut self, kept: &[u16], text: &[u16]) -> bool {
        let identity = Identity::of(text);
        if self.0 == Some(identity) {
            return true;
        }
        let same = kept == text;
        if same {
            self.0 = Some(identity);
        }
        same
    }

    pub(crate) fn forget(&mut self) {
        self.0 = None;
    }
}

pub(crate) struct NowrapFrame {
    projection: LiftProjection,
    lifted: bool,
    styled: Option<bool>,
    edges: Option<(Identity, usize, usize)>,
    same: SameText,
}

impl NowrapFrame {
    pub(crate) fn new(projection: LiftProjection, lifted: bool) -> Self {
        NowrapFrame { projection, lifted, styled: None, edges: None, same: SameText::default() }
    }

    pub(crate) fn forget(&mut self) {
        self.same.forget();
        self.edges = None;
    }

    fn unchanged_shift(&mut self, source: &[u16], start: usize, end: usize) -> Option<isize> {
        let visible = self.projection.visible();
        let identity = Identity::of(source);
        let (prefix, suffix) = match self.edges {
            Some((known, prefix, suffix)) if known == identity => (prefix, suffix),
            _ => {
                let limit = source.len().min(visible.len());
                let prefix = source.iter().zip(visible).take_while(|(a, b)| a == b).count();
                let suffix = source
                    .iter()
                    .rev()
                    .zip(visible.iter().rev())
                    .take(limit - prefix)
                    .take_while(|(a, b)| a == b)
                    .count();
                self.edges = Some((identity, prefix, suffix));
                (prefix, suffix)
            }
        };
        if end <= prefix {
            return Some(0);
        }
        (start >= source.len() - suffix).then(|| visible.len() as isize - source.len() as isize)
    }
}

pub(crate) struct NowrapCoverage {
    source: JsString,
    open: JsString,
    starts: Vec<usize>,
    nodes: Vec<usize>,
    tree: Vec<Element>,
    cursor: usize,
    pub(crate) same: SameText,
}

pub fn held_before_nowrap_end(ctx: &mut Ctx<'_>, source: &[u16], gt: usize) -> Result<Option<u16>> {
    if !ctx.html || source.get(gt) != Some(&0x3E) {
        return Ok(None);
    }
    let Some(start) = inline_end_tag_start(source, gt + 1) else { return Ok(None) };
    Ok(if start > 0 && inside_canonical_nowrap(ctx, source, start - 1, start)? {
        Some(source[start - 1])
    } else {
        None
    })
}

fn lifts_styled(ctx: &mut Ctx<'_>, frame: &mut NowrapFrame, open: &[u16]) -> bool {
    if let Some(styled) = frame.styled {
        return styled;
    }
    let (_, nodes, tree) = element_stretches(ctx, frame.projection.source(), open);
    let styled = nodes.iter().any(|&node| !tree[node].covered && tree[tree[node].parent].covered);
    frame.styled = Some(styled);
    styled
}

pub fn inside_canonical_nowrap(ctx: &mut Ctx<'_>, source: &[u16], start: usize, end: usize) -> Result<bool> {
    if !ctx.html {
        return Ok(false);
    }
    let open = canonical_nowrap_open(ctx)?;
    let mut frame = ctx.bound.nowrap_frame.take();
    let mut range = (start, end);
    let mut shift = None;
    if frame.as_mut().is_some_and(|f| f.same.holds(f.projection.visible(), source)) {
        shift = Some(0);
    } else if let Some(f) = frame.as_mut().filter(|f| f.lifted)
        && lifts_styled(ctx, f, &open)
    {
        shift = f.unchanged_shift(source, start, end);
        if shift.is_none() {
            ctx.bound.nowrap_frame = frame;
            return Ok(false);
        }
    }
    let projected = shift.is_some();
    if let (Some(f), Some(shift)) = (frame.as_mut(), shift) {
        let at = |i: usize| (i as isize + shift) as usize;
        range = (f.projection.at(at(start), true), f.projection.at(at(end), false));
    }
    let active: &[u16] = match frame.as_mut() {
        Some(f) if projected => f.projection.source(),
        _ => source,
    };
    let mut cached = ctx.bound.nowrap_cache.take().filter(|state| state.open == open);
    let kept = cached.as_mut().is_some_and(|state| state.same.holds(&state.source, active));
    let mut state = match cached {
        Some(state) if kept => state,
        _ => {
            let (starts, nodes, tree) = element_stretches(ctx, active, &open);
            NowrapCoverage {
                source: active.to_vec(),
                open,
                starts,
                nodes,
                tree,
                cursor: 0,
                same: SameText(Some(Identity::of(active))),
            }
        }
    };
    let (from, to) = range;
    if state.starts[state.cursor] > from {
        state.cursor = 0;
    }
    state.cursor = stretch_at(&state.starts, from, state.cursor);
    let last = stretch_at(&state.starts, from.max(to), state.cursor);
    let inside = state.tree[common_element(&state.tree, state.nodes[state.cursor], state.nodes[last])].covered;
    ctx.bound.nowrap_cache = Some(state);
    ctx.bound.nowrap_frame = frame;
    Ok(inside)
}

pub fn nowrap(ctx: &mut Ctx<'_>, body: &[u16], source: &[u16], start: usize, end: usize) -> Result<JsString> {
    if inside_canonical_nowrap(ctx, source, start, end)? {
        Ok(body.to_vec())
    } else {
        ctx.tag(body, "span", &[("class", "nowrap")])
    }
}
