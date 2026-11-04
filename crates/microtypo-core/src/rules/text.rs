use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, LazyLock};

use crate::cat;
use crate::errors::Result;
use crate::js::{self, Caps, JsString, Re};
use crate::protect::placeholders::{
    BREAKLINE, CLOSE, OPEN, OPEN_UNIT, PARAGRAPH_CLOSE, PARAGRAPH_OPEN, RESERVED_PUA_CLASS, TAG_PLACEHOLDER_SOURCE,
    TAIL_KINDS, TRANSPARENT_TAG_KINDS,
};
use crate::protect::tags::NameMatcher;
use crate::rule_runner::{Ctx, Replacement, Rule, RuleGroupDef};
use crate::shared::block_tags::{BLOCK_TAG_NAMES, VOID_BLOCK_TAG_NAMES, VOID_TAG_NAMES, implied_end_tags};
use crate::shared::glyphs::{MDASH, NBSP};
use crate::shared::strings::count;

fn block_boundary_res(alternation: &str) -> [Re; 2] {
    [
        Re::new(&format!(r"(?<![ \t])[ \t]*[\r\n][\r\n \t]*(?=<\/?(?:{alternation})>)"), "g"),
        Re::new(&format!(r"(<\/?(?:{alternation})>)[ \t]*[\r\n][\r\n \t]*"), "g"),
    ]
}

fn after_author_break_re(alternation: &str) -> Re {
    Re::new(&format!(r"(?<!<(?:{alternation})>)\n"), "g")
}

static ANY_TAG_LIFT_RE: LazyLock<Re> = LazyLock::new(|| Re::new(&TAG_PLACEHOLDER_SOURCE, "g"));

const PARAGRAPH_SPLIT_SOURCE: &str = r"(?<![ \t])([ \t]+)?(\n)+([ \t]*)(\n)+";

static PARAGRAPH_SPLIT_RE: LazyLock<Re> = LazyLock::new(|| Re::new(PARAGRAPH_SPLIT_SOURCE, "g"));

static EMPTY_PARAGRAPH_RE: LazyLock<Re> = LazyLock::new(|| {
    let (open, close) = (js::lossy(&PARAGRAPH_OPEN), js::lossy(&PARAGRAPH_CLOSE));
    let (o, c) = (open.chars().next().unwrap(), close.chars().next().unwrap());
    Re::new(&format!("{open}(?:[^{o}{c}]*?)?{close}"), "g")
});

static PARAGRAPH_GAP_RE: LazyLock<Re> = LazyLock::new(|| {
    Re::new(&format!(r"({})([\r\n \t]+)({})", js::lossy(&PARAGRAPH_CLOSE), js::lossy(&PARAGRAPH_OPEN)), "gms")
});

static BLOCK_SCAN_RE: LazyLock<Re> =
    LazyLock::new(|| Re::new(&format!("{}|{PARAGRAPH_SPLIT_SOURCE}", *TAG_PLACEHOLDER_SOURCE), "g"));

static P_NAME_RE: LazyLock<Re> = LazyLock::new(|| Re::new("^p$", "i"));

static AUTHOR_BREAK_RE: LazyLock<Re> = LazyLock::new(|| Re::new(r"^br\b", "i"));

static CLOSING_TAG_RE: LazyLock<Re> = LazyLock::new(|| Re::new(r"^<\/(.+)>$", ""));

static TAIL_RE: LazyLock<Re> = LazyLock::new(|| {
    Re::new(&format!(r"(?:<{OPEN}[{TRANSPARENT_TAG_KINDS}]\d+{CLOSE}>|<!--)?{OPEN}[{TAIL_KINDS}]\d+{CLOSE}"), "")
});

fn has_non_space(s: &[u16]) -> bool {
    s.iter().any(|&c| !js::is_js_space(c))
}

fn in_names(list: &[&str], name: &[u16]) -> bool {
    list.iter().any(|n| n.encode_utf16().eq(name.iter().copied()))
}

fn is_block_name(name: &[u16]) -> bool {
    in_names(BLOCK_TAG_NAMES, name) || in_names(VOID_BLOCK_TAG_NAMES, name) || in_names(&["html", "head", "body"], name)
}

fn placeholder(prefix: char, id: usize) -> JsString {
    cat!(OPEN, prefix, js::num(id), CLOSE)
}

fn lift_paragraphs(text: &[u16], ctx: &mut Ctx<'_>) -> JsString {
    let ps: HashSet<JsString> = tag_placeholders(ctx, &NameMatcher::Regex(&P_NAME_RE)).into_iter().collect();
    if ps.is_empty() {
        return text.to_vec();
    }
    ANY_TAG_LIFT_RE.replace_with(text, |m| {
        let ph = m.s(2);
        if !ps.contains(ph) {
            return m.whole().to_vec();
        }
        if m.s(1) == [0x2F] { cat!(ph, PARAGRAPH_CLOSE) } else { cat!(PARAGRAPH_OPEN, ph) }
    })
}

fn tag_placeholders(ctx: &mut Ctx<'_>, matcher: &NameMatcher<'_>) -> Vec<JsString> {
    ctx.find_by_tag_name(matcher).into_iter().map(|(prefix, id)| placeholder(prefix, id)).collect()
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Kind {
    Prose,
    Run,
    Element,
}

#[derive(Clone, Copy, Debug)]
struct Item {
    kind: Kind,
    i: usize,
    opens: bool,
    closed: bool,
    wrapped: bool,
}

impl Item {
    fn text(kind: Kind, i: usize) -> Self {
        Item { kind, i, opens: false, closed: false, wrapped: false }
    }

    fn element(opens: bool, closed: bool, wrapped: bool) -> Self {
        Item { kind: Kind::Element, i: usize::MAX, opens, closed, wrapped }
    }
}

fn unwrap_paragraph(paragraph: &[u16]) -> JsString {
    js::replace_all_lit(&js::replace_all_lit(paragraph, &PARAGRAPH_OPEN, &[]), &PARAGRAPH_CLOSE, &[])
}

fn drop_empty_paragraphs(text: &[u16], gap: &[u16]) -> JsString {
    let is_empty = |paragraph: &[u16]| !has_non_space(&js::replace_all_lit(&unwrap_paragraph(paragraph), gap, &[]));
    EMPTY_PARAGRAPH_RE.replace_with(text, |m| if is_empty(m.whole()) { Vec::new() } else { m.whole().to_vec() })
}

#[derive(Default, Clone, Debug)]
struct Holder {
    elements: Vec<JsString>,
    counts: HashMap<JsString, usize>,
}

fn track_element(holder: &mut Holder, tag: &[u16], closing: bool) -> bool {
    let empty = tag.last() == Some(&0x2F);
    let name = if empty { &tag[..tag.len() - 1] } else { tag };
    if !closing {
        if empty {
            return true;
        }
        holder.elements.push(name.to_vec());
        *holder.counts.entry(name.to_vec()).or_insert(0) += 1;
        return true;
    }
    if holder.counts.get(name).copied().unwrap_or(0) == 0 {
        return false;
    }
    while let Some(popped) = holder.elements.pop() {
        if let Some(n) = holder.counts.get_mut(&popped) {
            *n -= 1;
        }
        if popped == name {
            break;
        }
    }
    true
}

fn inside_element(holder: &Holder) -> bool {
    !holder.elements.is_empty()
}

#[derive(Default)]
struct TagNames {
    elements: HashMap<JsString, JsString>,
    blocks: HashMap<JsString, JsString>,
    block_order: Vec<JsString>,
    ids: [Vec<Option<JsString>>; 3],
    breaks: Vec<JsString>,
}

fn kind_index(prefix: Option<u16>) -> Option<usize> {
    match prefix {
        Some(0x54) => Some(0),
        Some(0x41) => Some(1),
        Some(0x4C) => Some(2),
        _ => None,
    }
}

fn top_level(text: &[u16], names: &TagNames, on_top: &mut dyn FnMut(usize, usize) -> Result<()>) -> Result<Holder> {
    let mut holder = Holder::default();
    let mut from = 0usize;
    let mut inner = 0usize;
    let mut line_break: isize = -1;
    let mut next = if names.elements.is_empty() { None } else { js::index_of_unit(text, OPEN_UNIT, 0) };
    while let Some(i) = next {
        next = js::index_of_unit(text, OPEN_UNIT, i + 1);
        let Some(named) = kind_index(text.get(i + 1).copied()).map(|k| &names.ids[k]) else { continue };
        let mut id = 0usize;
        let mut end = i + 2;
        while let Some(d) = text.get(end).copied().filter(|d| (0x30..=0x39).contains(d)) {
            id = id.saturating_mul(10).saturating_add(usize::from(d - 0x30));
            end += 1;
        }
        end += 1;
        let Some(name) = named.get(id).and_then(Option::as_ref) else { continue };
        let mut start = i as isize;
        let mut after = end;
        let mut closing = false;
        match js::at(text, i as isize - 1) {
            Some(0x3C) => {
                start -= 1;
                after += 1;
            }
            Some(0x2F) => {
                start -= 2;
                after += 1;
                closing = true;
            }
            _ => {
                if js::starts_with_at(text, &PARAGRAPH_CLOSE, end) {
                    closing = true;
                }
            }
        }
        let was = inside_element(&holder);
        track_element(&mut holder, name, closing);
        if !was && inside_element(&holder) {
            inner = after;
        } else if was && !inside_element(&holder) {
            if line_break < inner as isize {
                line_break = js::index_of_unit(text, 0x0A, inner).map_or(isize::MAX, |at| at as isize);
            }
            if line_break < start {
                on_top(from, inner)?;
                from = start.max(0) as usize;
            }
        }
    }
    if inside_element(&holder) {
        on_top(from, inner)?;
    } else {
        on_top(from, text.len())?;
    }
    Ok(holder)
}

fn as_written(segment: &[Item]) -> bool {
    segment.iter().any(|item| item.wrapped)
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Edge {
    Head,
    Tail,
}

fn read_to(tags: &[Caps<'_>], t: &mut usize, end: usize, elements: &HashMap<JsString, JsString>, holder: &mut Holder) {
    while let Some(tag) = tags.get(*t).filter(|tag| tag.index() < end) {
        if let Some(name) = elements.get(tag.s(2)) {
            track_element(holder, name, tag.s(1) == [0x2F]);
        }
        *t += 1;
    }
}

fn do_paragraphs(
    text: &[u16],
    ctx: &mut Ctx<'_>,
    elements: &HashMap<JsString, JsString>,
    edge: Option<Edge>,
) -> Result<(JsString, bool)> {
    let gap = ctx.iblock(&js::u("\n\n"))?;
    let split = |p1: Option<&[u16]>| cat!(p1, PARAGRAPH_CLOSE, gap, PARAGRAPH_OPEN);
    let result;
    if elements.is_empty() {
        result = PARAGRAPH_SPLIT_RE.replace_with(text, |m| split(m.get(1)));
    } else {
        let mut holder = Holder::default();
        let tags = ANY_TAG_LIFT_RE.find_all(text);
        let mut t = 0;
        result = PARAGRAPH_SPLIT_RE.replace_with(text, |m| {
            read_to(&tags, &mut t, m.index(), elements, &mut holder);
            if inside_element(&holder) { m.whole().to_vec() } else { split(m.get(1)) }
        });
        if edge == Some(Edge::Head) {
            read_to(&tags, &mut t, text.len(), elements, &mut holder);
        }
        if edge == Some(Edge::Head) && inside_element(&holder) {
            let body = cat!(PARAGRAPH_OPEN, result);
            let marker = cat!(PARAGRAPH_CLOSE, gap, PARAGRAPH_OPEN);
            let Some(at) = js::last_index_of(&body, &marker) else {
                return Ok((text.to_vec(), true));
            };
            let done = drop_empty_paragraphs(&cat!(&body[..at], PARAGRAPH_CLOSE), &gap);
            return Ok((cat!(done, js::from(&body, at + marker.len())), true));
        }
    }
    Ok((drop_empty_paragraphs(&cat!(PARAGRAPH_OPEN, result, PARAGRAPH_CLOSE), &gap), false))
}

const FOREIGN_ROOTS: &[&str] = &["svg", "math"];

fn tag_names(ctx: &mut Ctx<'_>) -> TagNames {
    let names: RefCell<Vec<(JsString, bool)>> = RefCell::new(Vec::new());
    let record = |name: &[u16], self_closing: bool| {
        names.borrow_mut().push((name.to_vec(), self_closing));
        true
    };
    let found = ctx.find_by_tag_name(&NameMatcher::Predicate(&record));
    let names = names.into_inner();
    let mut out = TagNames::default();
    for (i, (prefix, id)) in found.into_iter().enumerate() {
        let (raw, self_closing) = &names[i];
        let name = js::to_lower(raw);
        let ph = placeholder(prefix, id);
        if name.starts_with(&[0x62, 0x72]) && AUTHOR_BREAK_RE.test(raw) {
            out.breaks.push(ph.clone());
        }
        if is_block_name(&name) {
            out.block_order.push(ph.clone());
            out.blocks.insert(ph.clone(), name.clone());
        }
        if !in_names(VOID_TAG_NAMES, &name) {
            let tag = if *self_closing && in_names(FOREIGN_ROOTS, &name) { cat!(name, "/") } else { name };
            if let Some(k) = kind_index(Some(prefix as u16)) {
                let slots = &mut out.ids[k];
                if slots.len() <= id {
                    slots.resize(id + 1, None);
                }
                slots[id] = Some(tag.clone());
            }
            out.elements.insert(ph, tag);
        }
    }
    out
}

fn edge_space(s: &[u16]) -> JsString {
    if s.iter().any(|&c| c == 0x0A || c == 0x0D) { s.to_vec() } else { Vec::new() }
}

fn rebind_leading_dash(core: &[u16]) -> JsString {
    let bound = cat!(NBSP, MDASH, " ");
    if core.starts_with(&bound) { cat!(MDASH, NBSP, &core[bound.len()..]) } else { core.to_vec() }
}

fn is_edge_space(ch: u16) -> bool {
    ch == 0x20 || ch == 0x09 || ch == 0x0A
}

fn split_edges(s: &[u16]) -> (&[u16], &[u16], &[u16]) {
    let mut start = 0;
    let mut end = s.len();
    while start < end && is_edge_space(s[start]) {
        start += 1;
    }
    while end > start && is_edge_space(s[end - 1]) {
        end -= 1;
    }
    (&s[..start], &s[start..end], &s[end..])
}

struct Frame {
    holder: Holder,
    name: Option<JsString>,
    items: Vec<Item>,
    keep: bool,
    wrapped: bool,
}

impl Frame {
    fn root(keep: bool) -> Self {
        Frame { holder: Holder::default(), name: None, items: Vec::new(), keep, wrapped: false }
    }
}

fn solid(out: &[JsString], item: &Item) -> bool {
    item.kind == Kind::Element || (item.kind == Kind::Prose && has_non_space(&out[item.i]))
}

fn ends_closed(out: &[JsString], names: &HashMap<JsString, JsString>, item: &Item) -> bool {
    if item.kind == Kind::Element {
        return item.closed;
    }
    let s = js::trim_end(&out[item.i]);
    let last = match js::last_index_of(s, &[0x3C, 0x2F]) {
        Some(at) => &s[at..],
        None => js::slice_signed(s, -1, None),
    };
    CLOSING_TAG_RE.exec(last).is_some_and(|m| names.contains_key(m.s(1)))
}

struct Chunk<'t> {
    text: &'t [u16],
    names: &'t HashMap<JsString, JsString>,
    gap: JsString,
    out: Vec<JsString>,
    open: HashMap<JsString, usize>,
    stack: Vec<Frame>,
    cursor: usize,
    shaped: bool,
}

impl Chunk<'_> {
    fn top(&mut self) -> &mut Frame {
        self.stack.last_mut().expect("the root frame stays")
    }

    fn prose(&mut self, end: usize) {
        if end > self.cursor {
            self.out.push(self.text[self.cursor..end].to_vec());
            let i = self.out.len() - 1;
            self.top().items.push(Item::text(Kind::Prose, i));
        }
    }

    fn finish(&mut self, frame: Frame, closed: bool) {
        let ends = closed
            || frame
                .items
                .iter()
                .rev()
                .find(|item| solid(&self.out, item))
                .is_some_and(|last| ends_closed(&self.out, self.names, last));
        if let Some(n) = frame.name.as_ref().and_then(|name| self.open.get_mut(name)) {
            *n = n.saturating_sub(1);
        }
        self.top().items.push(Item::element(true, ends, frame.wrapped));
    }

    fn restart(&mut self, index: usize, token: &[u16], outer: &Holder) {
        self.prose(index);
        self.cursor = index + token.len();
        self.shaped = true;
        while self.stack.len() > 1 {
            let frame = self.stack.pop().expect("checked above");
            self.finish(frame, false);
        }
        self.out.push(token.to_vec());
        self.stack[0] = Frame::root(inside_element(outer));
    }

    fn shape_segment(&mut self, segment: &[Item], first: bool, last: bool) {
        if as_written(segment) {
            return;
        }
        if !segment.iter().any(|item| item.kind == Kind::Element) {
            if let Some(item) = segment.first() {
                let s = std::mem::take(&mut self.out[item.i]);
                let (lead, core, trail) = split_edges(&s);
                let inner = cat!(if first { &[][..] } else { lead }, core, if last { &[][..] } else { trail });
                self.out[item.i] = cat!(
                    if first { edge_space(lead) } else { Vec::new() },
                    PARAGRAPH_OPEN,
                    inner,
                    PARAGRAPH_CLOSE,
                    if last { edge_space(trail) } else { Vec::new() }
                );
            }
            return;
        }
        let from = segment.iter().position(|item| solid(&self.out, item)).expect("an element is solid");
        let to = segment.iter().rposition(|item| solid(&self.out, item)).expect("an element is solid");
        let bare = segment[from].opens && ends_closed(&self.out, self.names, &segment[to]);
        for (k, item) in segment.iter().enumerate() {
            if item.kind == Kind::Element {
                continue;
            }
            if k < from || k > to {
                self.out[item.i] = Vec::new();
            } else if bare {
                if k == to {
                    self.out[item.i] = js::trim_end(&self.out[item.i]).to_vec();
                }
            } else {
                let s = std::mem::take(&mut self.out[item.i]);
                let (lead, core, trail) = split_edges(&s);
                let keep_lead = if k > 0 {
                    js::u("\n")
                } else if first {
                    edge_space(lead)
                } else {
                    Vec::new()
                };
                let keep_trail = if k < segment.len() - 1 {
                    js::u("\n")
                } else if last {
                    edge_space(trail)
                } else {
                    Vec::new()
                };
                let body = if k > 0 { rebind_leading_dash(core) } else { core.to_vec() };
                self.out[item.i] = cat!(keep_lead, PARAGRAPH_OPEN, body, PARAGRAPH_CLOSE, keep_trail);
            }
        }
    }

    fn shape_level(&mut self, items: &[Item]) {
        let from = items.iter().position(|item| solid(&self.out, item));
        let to = items.iter().rposition(|item| solid(&self.out, item));
        for (k, item) in items.iter().enumerate() {
            let outside = match (from, to) {
                (Some(f), Some(t)) => k < f || k > t,
                _ => true,
            };
            if outside && item.kind != Kind::Element {
                self.out[item.i] = edge_space(&self.out[item.i]);
            }
        }
        let (lo, hi) = match (from, to) {
            (Some(f), Some(t)) => (f, t + 1),
            _ => (0, 0),
        };
        let mut segments: Vec<Vec<Item>> = vec![Vec::new()];
        let mut runs = Vec::new();
        for item in &items[lo..hi] {
            if item.kind == Kind::Run {
                runs.push(*item);
                segments.push(Vec::new());
            } else {
                segments.last_mut().expect("one segment at least").push(*item);
            }
        }
        for (k, run) in runs.iter().enumerate() {
            self.out[run.i] =
                if as_written(&segments[k]) || as_written(&segments[k + 1]) { Vec::new() } else { self.gap.clone() };
        }
        let is_run = |item: &Item| item.kind == Kind::Run;
        let leads = match from {
            Some(f) => items[..f].iter().any(is_run),
            None => items[..items.len().saturating_sub(1)].iter().any(is_run),
        };
        let trails = match to {
            Some(t) => items[t + 1..].iter().any(is_run),
            None => items.iter().any(is_run),
        };
        let n = segments.len();
        for (k, s) in segments.iter().enumerate() {
            self.shape_segment(s, k == 0 && !leads, k == n - 1 && !trails);
        }
    }
}

fn flat(
    text: &[u16],
    ctx: &mut Ctx<'_>,
    elements: &HashMap<JsString, JsString>,
    edge: Option<Edge>,
    facing: &[u16],
) -> Result<(JsString, JsString)> {
    let (text, inside) = do_paragraphs(text, ctx, elements, edge)?;
    Ok((text, if inside { facing.to_vec() } else { js::u("\n") }))
}

fn paragraph_chunk(
    raw: &[u16],
    ctx: &mut Ctx<'_>,
    names: &TagNames,
    edge: Option<Edge>,
    mut outer: Holder,
) -> Result<(JsString, JsString)> {
    let text =
        js::trim(&js::replace_all_lit(&js::replace_all_lit(raw, &[0x0D, 0x0A], &[0x0A]), &[0x0D], &[0x0A])).to_vec();
    if text.is_empty() {
        return Ok((Vec::new(), Vec::new()));
    }
    let facing = if edge == Some(Edge::Head) {
        js::from(raw, js::trim_end(raw).len()).to_vec()
    } else {
        raw[..raw.len() - js::trim_start(raw).len()].to_vec()
    };
    let elements = &names.elements;
    let starts_inside = inside_element(&outer);
    if names.blocks.is_empty() && !starts_inside {
        return flat(&text, ctx, elements, edge, &facing);
    }
    let gap = ctx.iblock(&js::u("\n\n"))?;
    let mut c = Chunk {
        text: &text,
        names: &names.blocks,
        gap,
        out: Vec::new(),
        open: HashMap::new(),
        stack: vec![Frame::root(starts_inside)],
        cursor: 0,
        shaped: false,
    };
    let mut sep = if starts_inside { facing.clone() } else { js::u("\n") };

    for m in BLOCK_SCAN_RE.find_all(&text) {
        let token = m.whole();
        let slash = m.s(1) == [0x2F];
        let Some(ph) = m.get(2) else {
            if inside_element(&c.top().holder) {
                continue;
            }
            let at = m.index() + m.get(3).map_or(0, <[u16]>::len);
            c.prose(at);
            c.out.push(text[at..m.end()].to_vec());
            let i = c.out.len() - 1;
            c.top().items.push(Item::text(Kind::Run, i));
            c.cursor = m.end();
            continue;
        };
        let Some(name) = names.blocks.get(ph) else {
            let Some(tag) = elements.get(ph) else { continue };
            if track_element(&mut c.top().holder, tag, slash) {
                continue;
            }
            if c.stack.len() == 1 && track_element(&mut outer, tag, true) {
                c.restart(m.index(), token, &outer);
            }
            continue;
        };
        if slash && c.open.get(name).copied().unwrap_or(0) == 0 {
            if track_element(&mut outer, name, true) {
                c.restart(m.index(), token, &outer);
            }
            continue;
        }
        c.prose(m.index());
        c.cursor = m.end();
        c.shaped = true;
        if slash {
            while c.top().name.as_deref() != Some(&name[..]) {
                let frame = c.stack.pop().expect("an open frame of that name");
                c.finish(frame, false);
            }
            let frame = c.stack.pop().expect("the frame of that name");
            c.out.push(token.to_vec());
            c.finish(frame, true);
        } else {
            let implied = implied_end_tags(&js::lossy(name));
            while let Some(list) = implied
                && c.top().name.as_ref().is_some_and(|top| in_names(list, top))
            {
                let frame = c.stack.pop().expect("a named frame");
                c.finish(frame, false);
            }
            let wrapped = inside_element(&c.top().holder);
            c.out.push(token.to_vec());
            if in_names(VOID_BLOCK_TAG_NAMES, name) {
                c.top().items.push(Item::element(false, false, wrapped));
            } else {
                c.stack.push(Frame {
                    holder: Holder::default(),
                    name: Some(name.clone()),
                    items: Vec::new(),
                    keep: false,
                    wrapped,
                });
                *c.open.entry(name.clone()).or_insert(0) += 1;
            }
        }
    }

    c.prose(text.len());
    if !c.shaped {
        return if starts_inside { Ok((text.clone(), facing)) } else { flat(&text, ctx, elements, edge, &facing) };
    }
    let ends_inside = edge == Some(Edge::Head) && inside_element(&c.stack[0].holder);
    if edge == Some(Edge::Head) && (c.stack.len() > 1 || ends_inside) {
        sep = facing;
    }
    while c.stack.len() > 1 {
        let frame = c.stack.pop().expect("checked above");
        c.finish(frame, false);
    }
    if ends_inside && let Some(last) = c.stack[0].items.last_mut() {
        last.wrapped = true;
    }
    if !c.stack[0].keep {
        let items = std::mem::take(&mut c.stack[0].items);
        c.shape_level(&items);
    }
    let joined = c.out.concat();
    Ok((drop_empty_paragraphs(&joined, &c.gap), sep))
}

fn split_tail(text: &[u16]) -> Option<(&[u16], &[u16], &[u16])> {
    let m = TAIL_RE.exec(text)?;
    let head = &text[..m.index()];
    let core = js::trim_end(head);
    Some((core, &head[core.len()..], &text[m.index()..]))
}

fn around_tail(text: &[u16], mut lay_out: impl FnMut(&[u16], bool) -> Result<JsString>) -> Result<JsString> {
    match split_tail(text) {
        None => lay_out(text, false),
        Some((core, gap, tail)) => Ok(cat!(lay_out(core, true)?, gap, tail)),
    }
}

fn build_paragraphs(ctx: &mut Ctx<'_>) -> Result<Option<JsString>> {
    let names = tag_names(ctx);
    let text = ctx.text.clone();
    ctx.text = around_tail(&text, |text, runs_on| lay_out_paragraphs(text, ctx, &names, runs_on))?;
    Ok(None)
}

fn lay_out_paragraphs(text: &[u16], ctx: &mut Ctx<'_>, names: &TagNames, runs_on: bool) -> Result<JsString> {
    let r = js::index_of(text, &PARAGRAPH_OPEN, 0);
    let p = js::last_index_of(text, &PARAGRAPH_CLOSE);
    let signed = |v: Option<usize>| v.map_or(-1, |v| v as isize);
    if runs_on
        && let Some(r) = r
        && signed(js::last_index_of(text, &PARAGRAPH_OPEN)) > signed(p)
    {
        let (head, sep) = paragraph_chunk(&text[..r], ctx, names, Some(Edge::Head), Holder::default())?;
        return Ok(cat!(head, sep, &text[r..]));
    }
    if let (Some(r), Some(p)) = (r, p)
        && p > r
    {
        let start = p + PARAGRAPH_CLOSE.len();
        let mid = js::slice(text, r + PARAGRAPH_OPEN.len(), p);
        let (head, head_sep) = paragraph_chunk(&text[..r], ctx, names, Some(Edge::Head), Holder::default())?;
        let rest = &text[start..];
        let outer = if js::trim(rest).is_empty() {
            Holder::default()
        } else {
            top_level(&text[..start], names, &mut |_, _| Ok(()))?
        };
        let (tail, tail_sep) = paragraph_chunk(rest, ctx, names, Some(Edge::Tail), outer)?;
        return Ok(cat!(head, head_sep, PARAGRAPH_OPEN, mid, PARAGRAPH_CLOSE, tail_sep, tail));
    }
    Ok(paragraph_chunk(text, ctx, names, None, Holder::default())?.0)
}

fn build_breaklines(ctx: &mut Ctx<'_>) -> Result<Option<JsString>> {
    let names = tag_names(ctx);
    let text = ctx.text.clone();
    ctx.text = around_tail(&text, |text, _| breaklines(text, ctx, &names))?;
    Ok(None)
}

fn break_top(
    part: &[u16],
    ctx: &mut Ctx<'_>,
    boundaries: Option<&[Re; 2]>,
    line_break: Option<&Re>,
) -> Result<JsString> {
    let mut s = PARAGRAPH_GAP_RE.try_replace_with(part, |m| {
        let ws = m.s(2);
        let inner = if ws.iter().any(|&c| c == 0x0A || c == 0x0D) { js::u("\n\n") } else { ws.to_vec() };
        Ok(cat!(m.s(1), ctx.iblock(&inner)?, m.s(3)))
    })?;
    if let Some([before_block, after_block]) = boundaries {
        s = before_block.try_replace_with(&s, |_| ctx.iblock(&js::u("\n\n")))?;
        s = after_block.try_replace_with(&s, |m| Ok(cat!(m.s(1), ctx.iblock(&js::u("\n\n"))?)))?;
    }
    Ok(match line_break {
        None => s,
        Some(re) => re.replace_tpl(&s, &cat!(BREAKLINE, "\n")),
    })
}

fn breaklines(text: &[u16], ctx: &mut Ctx<'_>, names: &TagNames) -> Result<JsString> {
    let alternation = |phs: &[JsString]| js::lossy(&js::join(phs, &[0x7C]));
    let boundaries =
        if names.blocks.is_empty() { None } else { Some(block_boundary_res(&alternation(&names.block_order))) };
    let line_break = if js::includes(text, &BREAKLINE) {
        None
    } else if names.breaks.is_empty() {
        Some(Re::new(r"\n", "g"))
    } else {
        Some(after_author_break_re(&alternation(&names.breaks)))
    };
    let mut parts: Vec<JsString> = Vec::new();
    let mut last = 0usize;
    top_level(text, names, &mut |from, to| {
        parts.push(js::slice(text, last, from).to_vec());
        parts.push(break_top(js::slice(text, from, to), ctx, boundaries.as_ref(), line_break.as_ref())?);
        last = to;
        Ok(())
    })?;
    parts.push(js::from(text, last).to_vec());
    Ok(parts.concat())
}

fn pre_parse(ctx: &mut Ctx<'_>) -> Result<()> {
    if ctx.html {
        let text = ctx.text.clone();
        ctx.text = lift_paragraphs(&text, ctx);
    }
    Ok(())
}

pub fn text_group() -> RuleGroupDef {
    let autolink = Re::new(
        &format!(r#"(\s|^)(http|ftp|mailto|https)(:\/\/)([^\s,!<>"'{RESERVED_PUA_CLASS}]{{4,}})(?=\s|\.|,|!|\?|<|$)"#),
        "giu",
    );
    let mut group = super::group(
        "Текст и абзацы",
        super::classes(super::NOWRAP_CLASSES),
        vec![
            Rule::pattern(
                "autolink",
                autolink,
                Replacement::func(|m, ctx| {
                    let mut body = m.s(4);
                    let mut tail: JsString = Vec::new();
                    while body.len() > 4
                        && (body.last() == Some(&0x2E)
                            || (body.last() == Some(&0x29) && count(body, &[0x28]) < count(body, &[0x29])))
                    {
                        tail = cat!(&body[body.len() - 1..], tail);
                        body = &body[..body.len() - 1];
                    }
                    let href = cat!(m.s(2), m.s(3), body);
                    Ok(cat!(m.s(1), ctx.tag_with(body, "a", vec![("href".to_owned(), href)])?, tail))
                }),
            )
            .describe("Выделение ссылок из текста")
            .html_only(),
            Rule::pattern(
                "email",
                Re::new(r"(\s|^|\()([a-z0-9\-_.]{2,})@([a-z0-9\-.]{2,})\.([a-z]{2,6})(?=\)|\s|\.|,|!|\?|$|<)", "giu"),
                Replacement::func(|m, ctx| {
                    let addr = cat!(m.s(2), "@", m.s(3), ".", m.s(4));
                    let href = cat!("mailto:", addr);
                    Ok(cat!(m.s(1), ctx.tag_with(&addr, "a", vec![("href".to_owned(), href)])?))
                }),
            )
            .describe("Выделение эл. почты из текста")
            .html_only(),
            Rule::patterns(
                "deduplicate_words",
                vec![
                    (Re::new(r"([а-яё]{3,})( |\t|\u{00A0})\1", "giu"), Replacement::tpl("$1")),
                    (
                        Re::new(r"(\s|\u{00A0}|^|\.|!|\?)(([А-ЯЁ])([а-яё]{2,}))( |\t|\u{00A0})(([а-яё])\4)", "gu"),
                        Replacement::func(|m, _| {
                            Ok(if m.s(7) == js::to_lower(m.s(4)) {
                                cat!(m.s(1), m.s(2))
                            } else {
                                cat!(m.s(1), m.s(2), m.s(5), m.s(6))
                            })
                        }),
                    ),
                ],
            )
            .describe("Удаление повторяющихся слов")
            .off()
            .html_only(),
            Rule::handler("paragraphs", build_paragraphs).describe("Простановка параграфов").html_only(),
            Rule::handler("breakline", build_breaklines).describe("Простановка переносов строк").html_only(),
        ],
    );
    group.pre_parse = Some(Arc::new(pre_parse));
    group
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &str) -> JsString {
        js::u(v)
    }

    #[test]
    fn split_edges_takes_ascii_space_only() {
        let text = s(" \n\tа\u{00A0}б \n");
        let (lead, core, trail) = split_edges(&text);
        assert_eq!(lead, &s(" \n\t")[..]);
        assert_eq!(core, &s("а\u{00A0}б")[..]);
        assert_eq!(trail, &s(" \n")[..]);
        let blank = s("  \t");
        assert_eq!(split_edges(&blank), (&blank[..], &[][..], &[][..]));
    }

    #[test]
    fn edge_space_keeps_line_breaks_only() {
        assert_eq!(edge_space(&s("  ")), s(""));
        assert_eq!(edge_space(&s(" \n ")), s(" \n "));
        assert_eq!(edge_space(&s("\r")), s("\r"));
    }

    #[test]
    fn rebinds_a_leading_dash() {
        assert_eq!(rebind_leading_dash(&s("\u{00A0}— слово")), s("—\u{00A0}слово"));
        assert_eq!(rebind_leading_dash(&s("— слово")), s("— слово"));
    }

    #[test]
    fn tracks_elements() {
        let mut h = Holder::default();
        assert!(!track_element(&mut h, &s("b"), true));
        assert!(track_element(&mut h, &s("svg/"), false));
        assert!(!inside_element(&h));
        track_element(&mut h, &s("a"), false);
        track_element(&mut h, &s("b"), false);
        track_element(&mut h, &s("i"), false);
        assert!(track_element(&mut h, &s("b"), true));
        assert_eq!(h.elements, vec![s("a")]);
        assert!(!track_element(&mut h, &s("i"), true));
        assert!(track_element(&mut h, &s("a/"), true));
        assert!(!inside_element(&h));
    }

    #[test]
    fn splits_at_a_tail_region() {
        let text = cat!("abc  ", "<", OPEN, "T1", CLOSE, ">", OPEN, "E2", CLOSE, " x");
        let (core, gap, tail) = split_tail(&text).unwrap();
        assert_eq!(core, &s("abc")[..]);
        assert_eq!(gap, &s("  ")[..]);
        assert_eq!(tail, &cat!("<", OPEN, "T1", CLOSE, ">", OPEN, "E2", CLOSE, " x")[..]);
        let comment = cat!("a\n<!--", OPEN, "F0", CLOSE);
        assert_eq!(split_tail(&comment).unwrap().2, &cat!("<!--", OPEN, "F0", CLOSE)[..]);
        assert!(split_tail(&cat!("a", OPEN, "T1", CLOSE)).is_none());
    }

    #[test]
    fn drops_empty_paragraphs() {
        let gap = cat!(OPEN, "I0", CLOSE);
        let text = cat!(PARAGRAPH_OPEN, " ", gap, "\n", PARAGRAPH_CLOSE, PARAGRAPH_OPEN, "x", PARAGRAPH_CLOSE);
        assert_eq!(drop_empty_paragraphs(&text, &gap), cat!(PARAGRAPH_OPEN, "x", PARAGRAPH_CLOSE));
        let other = cat!(PARAGRAPH_OPEN, OPEN, "I1", CLOSE, PARAGRAPH_CLOSE);
        assert_eq!(drop_empty_paragraphs(&other, &gap), other);
    }

    #[test]
    fn matches_the_reference() {
        use crate::protect::blocks::SafeBlocks;
        use crate::protect::placeholders::restore_paragraphs;
        use crate::protect::sequences::SafeSequences;
        use crate::protect::tags::{DEFAULT_LAYOUT, SafeTags};
        use crate::rule_runner::{compile_rule_group, run_rule_group};

        let cases = [
            ("Первый абзац.\n\nВторой абзац.", "<p>Первый абзац.</p>\n\n<p>Второй абзац.</p>"),
            ("Строка один\nстрока два", "<p>Строка один<br>\nстрока два</p>"),
            ("  \n\nТекст\n\n\n  ещё\n", "<p>Текст</p>\n\n<p>  ещё</p>"),
            (
                "Смотрите http://example.com/path. и (https://ru.wikipedia.org/wiki/Foo_(bar)) тут",
                "<p>Смотрите <a href=\"http://example.com/path\">example.com/path</a>. и (https://ru.wikipedia.org/wiki/Foo_(bar)) тут</p>",
            ),
            (
                "Пишите на mail@example.ru или (info@test.com). и a@b.c",
                "<p>Пишите на <a href=\"mailto:mail@example.ru\">mail@example.ru</a> или (<a href=\"mailto:info@test.com\">info@test.com</a>). и a@b.c</p>",
            ),
            ("<div>\nблок\n</div>\nТекст после\n\nи ещё", "<div>\nблок\n</div>\n\n<p>Текст после</p>\n\n<p>и ещё</p>"),
            ("<p>Автор</p>\n\nхвост\nстрока", "<p>Автор</p>\n\n<p>хвост<br>\nстрока</p>"),
            ("голова\n\n<p>абзац</p>", "<p>голова</p>\n\n<p>абзац</p>"),
            ("<b>жирный\n\nтекст</b> и дальше\n\nновый", "<p><b>жирный\n\nтекст</b> и дальше</p>\n\n<p>новый</p>"),
            ("строка<br>\nдругая\nтретья", "<p>строка<br>\nдругая<br>\nтретья</p>"),
            ("<ul>\n<li>один\n<li>два\n</ul>\nконец", "<ul>\n<li>один\n<li>два\n</ul>\n\n<p>конец</p>"),
            ("a\r\nb\r\n\r\nc", "<p>a<br>\nb</p>\n\n<p>c</p>"),
            (
                "<p>открыт\n\n<div>x</div>\n\nи <i>курсив\n</i> конец",
                "<p><p>открыт</p>\n\n<div>x</div>\n\n<p>и <i>курсив\n</i> конец</p>",
            ),
            ("текст <svg/> и\n\n<svg><g/></svg>\nконец", "<p>текст <svg/> и</p>\n\n<p><svg><g/></svg><br>\nконец</p>"),
            (
                "До\n\n<hr>\n\nПосле\n\n<div>\n\u{A0}\u{2014} dash\n\nвторой</div>",
                "<p>До</p>\n\n<hr>\n\n<p>После</p>\n\n<div>\n\u{A0}\u{2014} dash\n\nвторой</div>",
            ),
            ("Привет <span>\nмир\n</span>\n\nпока", "<p>Привет <span>\nмир\n</span></p>\n\n<p>пока</p>"),
            ("<p>один</p><p>два</p>\n<p>три", "<p>один</p><p>два</p>\n\n<p><p>три</p>"),
            (
                "текст\n\nещё\n\n\u{E000}F0\u{E001} хвост\nстрока",
                "<p>текст</p>\n\n<p>ещё</p>\n\n\u{E000}F0\u{E001} хвост\nстрока",
            ),
            ("<p>открыт\n\nдальше  \u{E000}E0\u{E001}\nтам", "<p>открыт\n\nдальше  \u{E000}E0\u{E001}\nтам"),
            (
                "<section>\n<h1>Заголовок</h1>\n\nтекст\n<table><tr><td>1\n<td>2</table>\n</section>\n\n</div>после",
                "<section>\n<h1>Заголовок</h1>\n\nтекст\n<table><tr><td>1\n<td>2</table>\n</section>\n\n<p></div>после</p>",
            ),
            (
                "<p class=\"x\">а</p>\n<p class=\"y\">б</p>\n\nв",
                "<p class=\"x\">а</p>\n\n<p class=\"y\">б</p>\n\n<p>в</p>",
            ),
            ("<em>начало\nтекста</p>\nконец <b>x</b>\n\nещё", "<p><em>начало\nтекста</p>\nконец <b>x</b>\n\nещё</p>"),
            (
                "\u{E000}T5\u{E001} /\u{E000}T0\u{E001} текст\nстрока",
                "<\u{E000}T5\u{E001}> /\u{E000}T0\u{E001} текст<br>\nстрока</p>",
            ),
            (
                "www http://ab.cd... mailto://x@y.zz)) ftp://host.ru/a(b)c.",
                "<p>www <a href=\"http://ab.cd\">ab.cd</a>... <a href=\"mailto://x@y.zz\">x@y.zz</a>)) <a href=\"ftp://host.ru/a(b)c\">host.ru/a(b)c</a>.</p>",
            ),
            ("<div>x</div>\u{A0}\u{2014} текст\nещё", "<div>x</div>\n\n<p>\u{2014}\u{A0}текст<br>\nещё</p>"),
            ("<b><p>a</p> b</b> c\n\nd", "<b><p>a</p> b</b><p>c</p>\n\n<p>d</p>"),
            ("<b>x <p>a</p> b\n\n</b> c\n\n<div>d</div>", "<b>x <p>a</p> b\n\n</b><p>c</p>\n\n<div>d</div>"),
            ("<p>a</p>\n</b>\n<div>z</div> хвост", "<p>a</p>\n\n<p></b></p>\n\n<div>z</div>\n\n<p>хвост</p>"),
            (
                "<i>\n<p>a</p>\n</i>\n\n<div>q</div>\n\n</i>после",
                "<i>\n<p>a</p>\n</i>\n\n<div>q</div>\n\n<p></i>после</p>",
            ),
            (
                "текст <div>блок</div> текст\n\nи <span><div>в</div></span>\n\nконец",
                "<p>текст</p>\n\n<div>блок</div>\n\n<p>текст</p>и <span><div>в</div></span><p>конец</p>",
            ),
            (
                "<dl><dt>a<dd>b</dl>\n\n<blockquote>\nцитата\n\nвторая\n</blockquote>",
                "<dl><dt>a<dd>b</dl>\n\n<blockquote>\nцитата\n\nвторая\n</blockquote>",
            ),
            (
                "<html><body>\nтело\n\n<main>m</main>\n</body></html>",
                "<html><body>\nтело\n\n<main>m</main>\n</body></html>",
            ),
            ("текст\n<hr/>\nещё <br/>\nи<BR>\nконец", "<p>текст</p>\n\n<hr/>\n\n<p>ещё <br/>\nи<BR>\nконец</p>"),
            ("<div></div>\n\n  \n\n<div> </div>", "<div></div>\n\n<div> </div>"),
            ("<span>a\n\nb\n\n<p>c</p>", "<span>a\n\nb\n\n<p>c</p>"),
            ("x<p>a\n\nb</p>y\n\n<p>z", "<p>x</p>\n\n<p>a\n\nb</p>\n\n<p>y</p>\n\n<p><p>z</p>"),
            (
                "<a href=\"u\">ссылка\n</a>\n\nhttp://x.ru/путь",
                "<p><a href=\"u\">ссылка\n</a></p>\n\n<p><a href=\"http://x.ru/путь\">x.ru/путь</a></p>",
            ),
        ];
        let compiled = compile_rule_group(&text_group(), "text").unwrap();
        let settings = serde_json::Map::new();
        let check_budget = |_: &str| Ok(());
        let blocks = SafeBlocks::default();
        let sequences = SafeSequences::default();
        let mut failures = Vec::new();
        for (input, expected) in cases {
            let mut tags = SafeTags::new(false);
            let protected = tags.protect(&s(input)).unwrap();
            let mut ctx = Ctx {
                text: Vec::new(),
                group: None,
                html: true,
                layout: DEFAULT_LAYOUT,
                engine_settings: &settings,
                check_budget: &check_budget,
                on_cycle_limit: None,
                safe_tags: &mut tags,
                safe_blocks: &blocks,
                safe_sequences: &sequences,
                prefix: "",
                budget: None,
            };
            let r = run_rule_group(&compiled, protected, &mut ctx).unwrap();
            let r = tags.restore(restore_paragraphs(tags.restore_iblocks(r)));
            if r != s(expected) {
                failures.push(format!("{input:?}\n  got {:?}\n want {expected:?}", js::lossy(&r)));
            }
        }
        assert!(failures.is_empty(), "{}", failures.join("\n"));
    }

    #[test]
    fn closing_tag_ends_a_stretch() {
        let ph = cat!(OPEN, "T3", CLOSE);
        let names: HashMap<JsString, JsString> = [(ph.clone(), s("div"))].into_iter().collect();
        let out = vec![cat!("x </", ph, ">  "), s("x"), cat!("</", OPEN, "T4", CLOSE, ">")];
        assert!(ends_closed(&out, &names, &Item::text(Kind::Prose, 0)));
        assert!(!ends_closed(&out, &names, &Item::text(Kind::Prose, 1)));
        assert!(!ends_closed(&out, &names, &Item::text(Kind::Prose, 2)));
    }
}
