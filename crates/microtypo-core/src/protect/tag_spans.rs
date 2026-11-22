use std::collections::{HashMap, HashSet};
use std::sync::{Arc, LazyLock};

use crate::errors::Result;
use crate::protect::blocks::{CheckBudget, Range};

use crate::js::{self, JsString, Re};
use crate::shared::block_tags::VOID_TAG_NAMES;
use crate::shared::tag_name::is_tag_name_start;

pub const COMMENT_OPEN: &[u16] = &[0x3C, 0x21, 0x2D, 0x2D];
pub const COMMENT_CLOSE: &[u16] = &[0x2D, 0x2D, 0x3E];

const LT: u16 = 0x3C;
const GT: u16 = 0x3E;
const SLASH: u16 = 0x2F;
const DQ: u16 = 0x22;
const SQ: u16 = 0x27;

pub fn empty_comment_close(text: &[u16], at: usize) -> Option<usize> {
    let after = at + COMMENT_OPEN.len();
    if text.get(after) == Some(&GT) {
        return Some(after - 2);
    }
    if text.get(after) == Some(&0x2D) && text.get(after + 1) == Some(&GT) { Some(after - 1) } else { None }
}

pub fn comment_close_at(text: &[u16], at: usize) -> Option<usize> {
    match empty_comment_close(text, at) {
        Some(e) => Some(e),
        None => js::index_of(text, COMMENT_CLOSE, at + COMMENT_OPEN.len()),
    }
}

#[derive(Debug, Clone, Copy)]
pub struct Span {
    pub start: usize,
    pub end: usize,
    pub opens_nothing: bool,
}

pub fn inside_tag(text: &[u16], tags: &[Span], cursor: &mut usize, at: usize) -> bool {
    while *cursor < tags.len() && tags[*cursor].end < at {
        *cursor += 1;
    }
    match tags.get(*cursor) {
        Some(span) => span.start < at && text.get(span.end) == Some(&GT),
        None => false,
    }
}

const NAME_END: &[u16] = &[0x20, 0x09, 0x0A, 0x0D, 0x0C, SLASH, GT];

fn name_matches_at(text: &[u16], at: usize, name: &str, terminators: &[u16]) -> bool {
    let end = at + name.len();
    for (k, b) in name.bytes().enumerate() {
        let expected = u32::from(b);
        let ch = js::code_point_at(text, at + k);
        match ch {
            Some(ch) if ch == expected || (ch | 0x20) == expected => {}
            None if 0x20 == expected => {}
            _ => return false,
        }
    }
    end >= text.len() || terminators.contains(&text[end])
}

pub fn named_at(text: &[u16], at: usize, name: &str) -> bool {
    name_matches_at(text, at, name, NAME_END)
}

fn raw_text_name_at<'a>(text: &[u16], at: usize, raw_text: &'a [String], case_sensitive: bool) -> Option<&'a str> {
    raw_text
        .iter()
        .find(|name| {
            (!case_sensitive || js::starts_with_at(text, &js::u(name), at)) && name_matches_at(text, at, name, NAME_END)
        })
        .map(String::as_str)
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum Reading {
    #[default]
    Document,
    Fragment,
    Markdown,
}

const NAME: usize = 0;
const BEFORE_ATTR: usize = 1;
const ATTR: usize = 2;
const BEFORE_VALUE: usize = 3;
const DOUBLE_QUOTED: usize = 4;
const SINGLE_QUOTED: usize = 5;
const UNQUOTED: usize = 6;
const DECLARATION: usize = 7;
const DECLARATION_DOUBLE: usize = 8;
const DECLARATION_SINGLE: usize = 9;
const STATES: usize = 10;
const ENDED: isize = -1;
const FAILED: isize = -2;

const CLOSING_QUOTE: [Option<u16>; STATES] =
    [None, None, None, None, Some(DQ), Some(SQ), None, None, Some(DQ), Some(SQ)];
const AFTER_QUOTE: [usize; STATES] = [0, 0, 0, 0, BEFORE_ATTR, BEFORE_ATTR, 0, 0, DECLARATION, DECLARATION];

fn is_space(ch: u16) -> bool {
    matches!(ch, 0x20 | 0x0A | 0x09 | 0x0D | 0x0C)
}

fn step(state: usize, ch: u16, fragment: bool, markdown: bool) -> isize {
    if let Some(quote) = CLOSING_QUOTE[state] {
        return if ch == quote { AFTER_QUOTE[state] as isize } else { state as isize };
    }
    if markdown && ch == 0x60 && state != DECLARATION {
        return FAILED;
    }
    if ch == GT {
        return ENDED;
    }
    if ch == LT && fragment && state != DECLARATION {
        return FAILED;
    }
    (match state {
        NAME => {
            if is_space(ch) || ch == SLASH {
                BEFORE_ATTR
            } else {
                NAME
            }
        }
        BEFORE_ATTR => {
            if is_space(ch) || ch == SLASH {
                BEFORE_ATTR
            } else {
                ATTR
            }
        }
        ATTR => {
            if ch == 0x3D {
                BEFORE_VALUE
            } else if ch == SLASH {
                BEFORE_ATTR
            } else {
                ATTR
            }
        }
        BEFORE_VALUE => {
            if is_space(ch) {
                BEFORE_VALUE
            } else if ch == DQ {
                DOUBLE_QUOTED
            } else if ch == SQ {
                SINGLE_QUOTED
            } else {
                UNQUOTED
            }
        }
        UNQUOTED => {
            if is_space(ch) {
                BEFORE_ATTR
            } else {
                UNQUOTED
            }
        }
        _ => {
            if ch == DQ {
                DECLARATION_DOUBLE
            } else if ch == SQ {
                DECLARATION_SINGLE
            } else {
                DECLARATION
            }
        }
    }) as isize
}

fn opens_declaration(ch: Option<u16>) -> bool {
    matches!(ch, Some(0x21 | 0x3F))
}

fn first_state(ch: Option<u16>) -> usize {
    if opens_declaration(ch) { DECLARATION } else { NAME }
}

pub fn quoted_attribute_spans(
    text: &[u16],
    tags: &[Span],
    check: crate::protect::blocks::CheckBudget<'_>,
) -> crate::errors::Result<Vec<(usize, usize)>> {
    let mut quoted = Vec::new();
    for span in tags {
        let from = span.start + if text.get(span.start + 1) == Some(&SLASH) { 2 } else { 1 };
        if opens_declaration(text.get(from).copied()) {
            continue;
        }
        let mut state = first_state(text.get(from).copied());
        let mut open = None;
        for (at, &ch) in text.iter().enumerate().take(span.end.min(text.len())).skip(from) {
            if at & 1023 == 0 {
                check("tag-quoted-values")?;
            }
            let before = CLOSING_QUOTE[state];
            let to = step(state, ch, false, false);
            if to < 0 {
                break;
            }
            state = to as usize;
            let after = CLOSING_QUOTE[state];
            if before.is_none() && after.is_some() {
                open = Some(at + 1);
            } else if before.is_some() && after.is_none() {
                quoted.push((open.expect("quoted value opened"), at));
                open = None;
            }
        }
        if let Some(open) = open {
            quoted.push((open, span.end.min(text.len())));
        }
    }
    Ok(quoted)
}

pub type AtomRead<'a> = Option<&'a mut dyn FnMut(usize) -> Result<Option<usize>>>;

pub fn tag_end(text: &[u16], at: usize, reading: Reading) -> isize {
    tag_end_with(text, at, reading, &|_| Ok(()), None).expect("unlimited scanner")
}

fn tag_end_with(
    text: &[u16],
    at: usize,
    reading: Reading,
    check: CheckBudget<'_>,
    mut atom: AtomRead<'_>,
) -> Result<isize> {
    let fragment = reading != Reading::Document;
    let mut state = first_state(text.get(at).copied());
    let mut i = at;
    while i < text.len() {
        if i & 0x3fff == 0 {
            check("tag-end")?;
        }
        if let Some(end) = match atom.as_mut() {
            Some(read) => read(i)?,
            None => None,
        } && end > i
        {
            state = step(state, 120, fragment, false) as usize;
            i = end;
            continue;
        }
        if atom.is_none()
            && let Some(quote) = CLOSING_QUOTE[state]
        {
            let Some(close) = js::index_of_unit(text, quote, i) else {
                return Ok(-1);
            };
            i = close + 1;
            state = AFTER_QUOTE[state];
            continue;
        }
        let to = step(state, text[i], fragment, reading == Reading::Markdown);
        if to == ENDED {
            return Ok(i as isize);
        }
        if to == FAILED {
            return Ok(FAILED);
        }
        state = to as usize;
        i += 1;
    }
    Ok(-1)
}

pub fn tag_ends_from(text: &[u16], from: usize, reading: Reading) -> Vec<isize> {
    tag_ends_with(text, from, reading, &|_| Ok(()), None).expect("unlimited scanner")
}

fn tag_ends_with(
    text: &[u16],
    from: usize,
    reading: Reading,
    check: CheckBudget<'_>,
    mut atom: AtomRead<'_>,
) -> Result<Vec<isize>> {
    let fragment = reading != Reading::Document;
    let mut ends = vec![0isize; text.len()];
    let mut row = [-1isize; STATES];
    let mut next = [0isize; STATES];
    let mut atoms = HashMap::new();
    let mut after_atoms = HashMap::new();
    if let Some(read) = atom.as_mut() {
        for i in from..text.len() {
            if i & 0x3fff == 0 {
                check("tag-atoms")?;
            }
            if let Some(end) = read(i)?
                && end > i
            {
                atoms.insert(i, end);
                after_atoms.insert(end, [-1isize; STATES]);
            }
        }
    }
    for i in (from..text.len()).rev() {
        if i & 0x3fff == 0 {
            check("tag-ends")?;
        }
        if let Some(after) = after_atoms.get_mut(&(i + 1)) {
            *after = row;
        }
        let ch = text[i];
        for (state, slot) in next.iter_mut().enumerate() {
            if let Some(end) = atoms.get(&i) {
                *slot = after_atoms[end][step(state, 120, fragment, false) as usize];
            } else {
                let to = step(state, ch, fragment, reading == Reading::Markdown);
                *slot = if to == ENDED {
                    i as isize
                } else if to == FAILED {
                    FAILED
                } else {
                    row[to as usize]
                };
            }
        }
        std::mem::swap(&mut row, &mut next);
        if i > 0 && (text[i - 1] == LT || (i > 1 && text[i - 1] == SLASH && text[i - 2] == LT)) {
            ends[i] = row[first_state(Some(ch))];
        }
    }
    Ok(ends)
}

static BREAKOUT: LazyLock<HashSet<&'static str>> = LazyLock::new(|| {
    [
        "b",
        "big",
        "blockquote",
        "body",
        "br",
        "center",
        "code",
        "dd",
        "div",
        "dl",
        "dt",
        "em",
        "embed",
        "h1",
        "h2",
        "h3",
        "h4",
        "h5",
        "h6",
        "head",
        "hr",
        "i",
        "img",
        "li",
        "listing",
        "menu",
        "meta",
        "nobr",
        "ol",
        "p",
        "pre",
        "ruby",
        "s",
        "small",
        "span",
        "strong",
        "strike",
        "sub",
        "sup",
        "table",
        "tt",
        "u",
        "ul",
        "var",
    ]
    .into_iter()
    .collect()
});
const SVG_HTML: &[&str] = &["foreignobject", "desc", "title"];
const MATHML_HTML: &[&str] = &["mi", "mo", "mn", "ms", "mtext"];
static VOID: LazyLock<HashSet<&'static str>> = LazyLock::new(|| {
    VOID_TAG_NAMES.iter().copied().chain(["basefont", "bgsound", "frame", "image", "keygen"]).collect()
});
const INLINE_OPTIONAL: &[&str] = &["option", "optgroup", "rb", "rp", "rt", "rtc"];
static OPTIONAL: LazyLock<HashSet<&'static str>> = LazyLock::new(|| {
    INLINE_OPTIONAL
        .iter()
        .copied()
        .chain(["p", "li", "dd", "dt", "caption", "colgroup", "tbody", "thead", "tfoot", "tr", "td", "th"])
        .collect()
});
static BLOCK: LazyLock<HashSet<&'static str>> = LazyLock::new(|| {
    [
        "address",
        "article",
        "aside",
        "blockquote",
        "center",
        "dd",
        "details",
        "dialog",
        "dir",
        "div",
        "dl",
        "dt",
        "fieldset",
        "figcaption",
        "figure",
        "footer",
        "form",
        "h1",
        "h2",
        "h3",
        "h4",
        "h5",
        "h6",
        "header",
        "hgroup",
        "li",
        "listing",
        "main",
        "menu",
        "nav",
        "ol",
        "p",
        "pre",
        "search",
        "section",
        "summary",
        "ul",
    ]
    .into_iter()
    .collect()
});
const TABLE: &[&str] = &["table", "caption", "colgroup", "tbody", "thead", "tfoot", "tr", "td", "th"];

fn siblings(name: &[u16]) -> Option<&'static [&'static str]> {
    match String::from_utf16_lossy(name).as_str() {
        "li" => Some(&["li", "p"]),
        "dd" | "dt" => Some(&["dd", "dt", "p"]),
        "option" | "optgroup" => Some(&["option"]),
        "td" | "th" => Some(&["td", "th"]),
        "tr" => Some(&["td", "th", "tr"]),
        _ => None,
    }
}

const REACH: usize = 64;
static FOREIGN_RE: LazyLock<Re> = LazyLock::new(|| Re::new(r"<(?:svg|math)[\s/>]", "i"));
static FONT_BREAKOUT_RE: LazyLock<Re> = LazyLock::new(|| Re::new("color|face|size", "i"));
static ANNOTATION_HTML_RE: LazyLock<Re> = LazyLock::new(|| Re::new(r"text\/html|application\/xhtml\+xml", "i"));

#[derive(Clone, Copy, PartialEq, Eq)]
enum ElementReading {
    Html,
    Foreign,
    OpensNothing,
}

fn tag_name(text: &[u16], at: usize) -> JsString {
    let mut end = at;
    while end < text.len() && !NAME_END.contains(&text[end]) {
        end += 1;
    }
    js::to_lower(&text[at.min(end)..end])
}

fn eq(name: &[u16], s: &str) -> bool {
    name.len() == s.len() && name.iter().zip(s.bytes()).all(|(&a, b)| a == u16::from(b))
}

fn in_set(name: &[u16], set: &[&str]) -> bool {
    set.iter().any(|s| eq(name, s))
}

fn in_hash(name: &[u16], set: &HashSet<&'static str>) -> bool {
    name.iter().all(|&c| c < 0x80) && set.contains(String::from_utf16_lossy(name).as_str())
}

pub fn self_closing(text: &[u16], start: usize, gt: usize) -> bool {
    self_closing_with(text, start, gt, None, &|_| Ok(())).expect("unlimited scanner")
}

pub fn self_closing_with(
    text: &[u16],
    start: usize,
    gt: usize,
    mut atom: AtomRead<'_>,
    check: CheckBudget<'_>,
) -> Result<bool> {
    if text.get(gt) != Some(&GT) || gt == 0 || text.get(gt - 1) != Some(&SLASH) {
        return Ok(false);
    }
    let mut state = NAME;
    let mut i = start + 1;
    while i + 1 < gt {
        if i & 0x3fff == 0 {
            check("tag-self-close")?;
        }
        if let Some(end) = match atom.as_mut() {
            Some(read) => read(i)?,
            None => None,
        } && end > i
            && end <= gt
        {
            state = step(state, 120, false, false) as usize;
            i = end;
            continue;
        }
        if atom.is_none()
            && let Some(quote) = CLOSING_QUOTE[state]
        {
            let Some(close) = js::index_of_unit(text, quote, i).filter(|&p| p < gt - 1) else {
                return Ok(false);
            };
            i = close + 1;
            state = AFTER_QUOTE[state];
            continue;
        }
        let next = step(state, text[i], false, false);
        if next < 0 {
            return Ok(false);
        }
        state = next as usize;
        i += 1;
    }
    Ok(matches!(state, NAME | BEFORE_ATTR | ATTR))
}

#[derive(Debug)]
pub struct ForeignFrame {
    name: JsString,
    svg: bool,
    html: bool,
    element: bool,
}
pub type ForeignContext = Option<Arc<ForeignFrame>>;
pub type Scope = (usize, Option<(JsString, bool, bool, bool)>);
type Undo = Vec<(bool, Arc<ForeignFrame>)>;
struct ForeignContent<'a> {
    stack: Vec<Arc<ForeignFrame>>,
    open: HashMap<JsString, Vec<usize>>,
    undo: Option<Undo>,
    reads: Option<Vec<isize>>,
    parents: Vec<(Option<Undo>, Option<Vec<isize>>)>,
    check: CheckBudget<'a>,
}
impl<'a> ForeignContent<'a> {
    fn new(check: CheckBudget<'a>) -> Self {
        Self { stack: Vec::new(), open: HashMap::new(), undo: None, reads: None, parents: Vec::new(), check }
    }
    fn touch(&mut self, depth: isize) {
        if let Some(reads) = self.reads.as_mut() {
            reads.push(depth);
        }
    }
    fn top(&mut self) -> ForeignContext {
        self.touch(self.stack.len() as isize - 1);
        self.stack.last().cloned()
    }
    fn context(&self) -> ForeignContext {
        self.stack.last().cloned()
    }
    fn mark(&self) -> usize {
        self.reads.as_ref().map_or(0, Vec::len)
    }
    fn begin(&mut self) {
        self.parents.push((self.undo.take(), self.reads.take()));
        self.undo = Some(Vec::new());
        self.reads = Some(Vec::new());
    }
    fn finish(&mut self, keep: bool, through: Option<usize>) -> Result<Vec<isize>> {
        let undo = self.undo.take().expect("foreign transaction");
        let reads = self.reads.take().expect("foreign dependency journal");
        let through = through.unwrap_or(reads.len());
        let mut minimums = vec![isize::MAX; through + 1];
        for i in (0..through).rev() {
            if i & 0x3fff == 0 {
                (self.check)("foreign-dependencies")?;
            }
            minimums[i] = minimums[i + 1].min(reads[i]);
        }
        if !keep {
            for (i, (push, frame)) in undo.iter().enumerate().rev() {
                if i & 0x3fff == 0 {
                    (self.check)("foreign-rollback")?;
                }
                if *push {
                    self.open.entry(frame.name.clone()).or_default().push(self.stack.len());
                    self.stack.push(frame.clone());
                } else {
                    self.pop();
                }
            }
        }
        let (parent_undo, parent_reads) = self.parents.pop().expect("parent foreign transaction");
        self.undo = parent_undo;
        self.reads = parent_reads;
        if keep && let Some(parent) = self.undo.as_mut() {
            for (i, operation) in undo.into_iter().enumerate() {
                if i & 0x3fff == 0 {
                    (self.check)("foreign-commit")?;
                }
                parent.push(operation);
            }
        }
        if let Some(parent) = self.reads.as_mut() {
            for (i, depth) in reads.into_iter().enumerate() {
                if i & 0x3fff == 0 {
                    (self.check)("foreign-dependencies")?;
                }
                parent.push(depth);
            }
        }
        Ok(minimums)
    }
    fn html(&mut self) -> bool {
        self.top().is_none_or(|f| f.html)
    }
    fn cdata(&mut self) -> bool {
        self.top().is_some_and(|f| !f.element)
    }
    fn start(
        &mut self,
        text: &[u16],
        name_start: usize,
        start: usize,
        gt: usize,
        atom: AtomRead<'_>,
    ) -> Result<ElementReading> {
        if self.html() {
            return self.start_html(text, name_start, start, gt, atom);
        }
        let name = tag_name(text, name_start);
        if in_hash(&name, &BREAKOUT)
            || (eq(&name, "font") && FONT_BREAKOUT_RE.test(js::slice(text, name_start + 4, gt)))
        {
            self.leave();
            return self.start_html(text, name_start, start, gt, atom);
        }
        let empty = self_closing_with(text, start, gt, atom, self.check)?;
        if !empty {
            let top = self.top().expect("foreign content frame");
            let svg = top.svg || (eq(&name, "svg") && eq(&top.name, "annotation-xml"));
            let html = if svg {
                in_set(&name, SVG_HTML)
            } else {
                in_set(&name, MATHML_HTML)
                    || (eq(&name, "annotation-xml") && ANNOTATION_HTML_RE.test(js::slice(text, name_start, gt)))
            };
            self.push(name, svg, html, false);
        }
        Ok(if empty { ElementReading::OpensNothing } else { ElementReading::Foreign })
    }
    fn start_html(
        &mut self,
        text: &[u16],
        name_start: usize,
        start: usize,
        gt: usize,
        atom: AtomRead<'_>,
    ) -> Result<ElementReading> {
        let svg = name_matches_at(text, name_start, "svg", NAME_END);
        if svg || name_matches_at(text, name_start, "math", NAME_END) {
            if !self_closing_with(text, start, gt, atom, self.check)? {
                self.push(js::u(if svg { "svg" } else { "math" }), svg, false, false);
            }
        } else if !self.stack.is_empty() {
            self.open_html(tag_name(text, name_start));
        }
        Ok(ElementReading::Html)
    }
    fn end(&mut self, text: &[u16], name_start: usize) {
        if self.stack.is_empty() {
            return;
        }
        let name = tag_name(text, name_start);
        let position = self.open.get(&name).and_then(|p| p.last()).copied();
        self.touch(position.map_or(-1, |p| p as isize));
        if position.is_none() {
            if !self.html() {
                self.leave();
            }
            return;
        }
        let html = self.top().expect("open frame").element;
        let at = if html {
            let skip: Vec<&str> = if in_hash(&name, &BLOCK) || in_set(&name, TABLE) {
                OPTIONAL.iter().copied().collect()
            } else {
                INLINE_OPTIONAL.to_vec()
            };
            self.closes(&name, Some(&skip))
        } else {
            self.closes(&name, None)
        };
        if let Some(at) = at {
            while self.stack.len() > at {
                self.pop();
            }
        } else if !html && !self.html() {
            self.leave();
        }
    }
    fn closes(&mut self, name: &[u16], skip: Option<&[&str]>) -> Option<usize> {
        let html = skip.is_some();
        let floor = self.stack.len().saturating_sub(REACH);
        for k in (floor..self.stack.len()).rev() {
            self.touch(k as isize);
            let frame = &self.stack[k];
            if frame.element != html {
                return None;
            }
            if frame.name == name {
                return Some(k);
            }
            if skip.is_some_and(|s| !in_set(&frame.name, s)) {
                return None;
            }
        }
        None
    }
    fn open_html(&mut self, name: JsString) {
        let closes = siblings(&name);
        while self.top().is_some_and(|f| f.element)
            && closes.is_some_and(|c| self.top().is_some_and(|f| in_set(&f.name, c)))
        {
            self.pop();
        }
        if in_hash(&name, &BLOCK)
            && self.top().is_some_and(|f| f.element)
            && self.top().is_some_and(|f| eq(&f.name, "p"))
        {
            self.pop();
        }
        if !in_hash(&name, &VOID) {
            self.push(name, false, true, true);
        }
    }
    fn push(&mut self, name: JsString, svg: bool, html: bool, element: bool) {
        let frame = Arc::new(ForeignFrame { name, svg, html, element });
        self.open.entry(frame.name.clone()).or_default().push(self.stack.len());
        self.stack.push(frame.clone());
        if let Some(undo) = self.undo.as_mut() {
            undo.push((false, frame));
        }
    }
    fn pop(&mut self) {
        self.touch(self.stack.len() as isize - 1);
        if let Some(frame) = self.stack.pop() {
            self.open.get_mut(&frame.name).expect("open index").pop();
            if let Some(undo) = self.undo.as_mut() {
                undo.push((true, frame));
            }
        }
    }
    fn leave(&mut self) {
        while !self.html() {
            self.pop();
        }
    }
}

pub type CommentRead<'a> = Box<dyn FnMut(usize) -> Result<Option<usize>> + 'a>;
pub type CommentFor = Arc<dyn for<'a> Fn(&'a [u16], CheckBudget<'a>) -> CommentRead<'a> + Send + Sync>;

fn borrow_atom<'a>(atom: &'a mut AtomRead<'_>) -> AtomRead<'a> {
    match atom {
        Some(read) => Some(&mut **read),
        None => None,
    }
}

fn raw_text_close(
    text: &[u16],
    from: usize,
    name: &str,
    case_sensitive: bool,
    check: CheckBudget<'_>,
) -> Result<Option<usize>> {
    let mut i = from;
    while i < text.len() {
        check("raw-text-close")?;
        let Some(lt) = js::index_of(text, &[LT, SLASH], i) else {
            return Ok(None);
        };
        if (!case_sensitive || js::starts_with_at(text, &js::u(name), lt + 2))
            && name_matches_at(text, lt + 2, name, NAME_END)
        {
            return Ok(Some(lt));
        }
        i = lt + 2;
    }
    Ok(None)
}

pub struct TagRead {
    pub span: Option<Span>,
    pub after: usize,
    pub mark: usize,
    pub syntax: Vec<Range>,
}
impl TagRead {
    fn absent(after: usize) -> Self {
        Self { span: None, after, mark: 0, syntax: Vec::new() }
    }
}

pub struct TagSpanReader<'a> {
    text: &'a [u16],
    raw_text: &'a [String],
    case_sensitive: bool,
    read_comment: Option<CommentRead<'a>>,
    mode: Reading,
    check: CheckBudget<'a>,
    foreign: Option<ForeignContent<'a>>,
    quiet_until: usize,
    absent_closers: HashMap<String, usize>,
    ends: [Option<Vec<isize>>; 4],
    furthest: [isize; 4],
    comments: Option<HashMap<usize, Option<usize>>>,
    quiet_stack: Vec<usize>,
    xml_names: bool,
}
impl<'a> TagSpanReader<'a> {
    pub fn new(
        text: &'a [u16],
        raw_text: &'a [String],
        case_sensitive: bool,
        read_comment: Option<CommentRead<'a>>,
        mode: Reading,
        check: CheckBudget<'a>,
        xml_names: bool,
    ) -> Self {
        let foreign = (!raw_text.is_empty() && FOREIGN_RE.test(text)).then(|| ForeignContent::new(check));
        Self {
            text,
            raw_text,
            case_sensitive,
            read_comment,
            mode,
            check,
            foreign,
            quiet_until: 0,
            absent_closers: HashMap::new(),
            ends: std::array::from_fn(|_| None),
            furthest: [-1; 4],
            comments: None,
            quiet_stack: Vec::new(),
            xml_names,
        }
    }
    pub fn context(&self) -> ForeignContext {
        self.foreign.as_ref().and_then(ForeignContent::context)
    }
    pub fn scope(&self) -> Scope {
        (self.quiet_until, self.context().map(|f| (f.name.clone(), f.svg, f.html, f.element)))
    }
    pub fn depth(&self) -> usize {
        self.foreign.as_ref().map_or(0, |f| f.stack.len())
    }
    pub fn mark(&self) -> usize {
        self.foreign.as_ref().map_or(0, ForeignContent::mark)
    }
    pub fn begin(&mut self) {
        self.quiet_stack.push(self.quiet_until);
        if let Some(f) = self.foreign.as_mut() {
            f.begin();
        }
    }
    pub fn finish(&mut self, keep: bool, through: Option<usize>) -> Result<Vec<isize>> {
        let minimums = match self.foreign.as_mut() {
            Some(f) => f.finish(keep, through)?,
            None => vec![0],
        };
        let quiet = self.quiet_stack.pop().expect("tag transaction");
        if !keep {
            self.quiet_until = quiet;
        }
        Ok(minimums)
    }
    pub fn next(&self, from: usize) -> Result<Option<(usize, usize, usize)>> {
        let mut cursor = from;
        while let Some(at) = js::index_of_unit(self.text, LT, cursor) {
            (self.check)("tag-opener")?;
            let name_start = at + if self.text.get(at + 1) == Some(&SLASH) { 2 } else { 1 };
            if is_tag_name_start(js::code_point_at(self.text, name_start), self.xml_names) {
                return Ok(Some((at, at, at)));
            }
            cursor = at + 1;
        }
        Ok(None)
    }
    pub fn read(
        &mut self,
        start: usize,
        tag_close: Option<isize>,
        mut atom: AtomRead<'_>,
        limit: Option<usize>,
    ) -> Result<TagRead> {
        (self.check)("tag-read")?;
        let text = self.text;
        let n = text.len();
        let closing = text.get(start + 1) == Some(&SLASH);
        let name_start = start + if closing { 2 } else { 1 };
        if opens_declaration(text.get(name_start).copied()) {
            atom = None;
        }
        let inline = limit.is_some() && !opens_declaration(text.get(name_start).copied());
        let mode = if inline { Reading::Markdown } else { self.mode };
        let flavor = usize::from(atom.is_some()) + if inline { 2 } else { 0 };
        let mut i;
        let xml_end = if self.xml_names && opens_declaration(text.get(start + 1).copied()) {
            crate::input::xml::xml_construct_end(text, start, self.check)?
        } else {
            None
        };
        if let Some(xml_end) = xml_end {
            i = xml_end - 1;
        } else if self.foreign.as_mut().is_some_and(ForeignContent::cdata)
            && start >= self.quiet_until
            && js::starts_with_at(text, &js::u("<![CDATA["), start)
        {
            i = js::index_of(text, &js::u("]]>"), start + 9).map_or(n, |p| p + 2);
        } else if !closing && js::starts_with_at(text, COMMENT_OPEN, start) {
            if self.read_comment.is_none() && self.comments.is_none() {
                let mut comments = HashMap::new();
                let mut next: Option<Option<usize>> = None;
                let mut from = 0;
                while let Some(at) = js::index_of(text, COMMENT_OPEN, from) {
                    (self.check)("html-comment-ends")?;
                    from = at + 1;
                    let close = empty_comment_close(text, at).or_else(|| {
                        if next.is_none_or(|c| c.is_some_and(|p| p < at + COMMENT_OPEN.len())) {
                            next = Some(js::index_of(text, COMMENT_CLOSE, at + COMMENT_OPEN.len()));
                        }
                        next.flatten()
                    });
                    comments.insert(at, close);
                }
                self.comments = Some(comments);
            }
            let close = match self.read_comment.as_mut() {
                Some(read) => read(start)?,
                None => self.comments.as_ref().and_then(|c| c.get(&start)).copied().flatten(),
            };
            let Some(close) = close else {
                if self.read_comment.is_none() {
                    self.quiet_until = n;
                }
                return Ok(TagRead::absent(start + 1));
            };
            i = close + COMMENT_CLOSE.len() - 1;
        } else {
            if (start as isize) < self.furthest[flavor] && self.ends[flavor].is_none() {
                self.ends[flavor] = Some(tag_ends_with(text, 0, mode, self.check, borrow_atom(&mut atom))?);
            }
            let gt = match tag_close {
                Some(gt) => gt,
                None => match self.ends[flavor].as_ref() {
                    Some(ends) => ends[name_start],
                    None => tag_end_with(text, name_start, mode, self.check, borrow_atom(&mut atom))?,
                },
            };
            self.furthest[flavor] = self.furthest[flavor].max(if gt == -1 { n as isize } else { gt });
            if gt == FAILED {
                return Ok(TagRead::absent(start + 1));
            }
            if gt == -1 {
                if self.ends[flavor].is_none() {
                    self.ends[flavor] = Some(tag_ends_with(text, 0, mode, self.check, borrow_atom(&mut atom))?);
                }
                if opens_declaration(text.get(name_start).copied()) {
                    return Ok(TagRead::absent(start + 1));
                }
            }
            i = if gt == -1 { n } else { gt as usize };
            if limit.is_some_and(|limit| i >= limit) && !opens_declaration(text.get(name_start).copied()) {
                return Ok(TagRead::absent(start + 1));
            }
        }
        let mut reading = ElementReading::Html;
        let mark = self.mark();
        if let Some(f) = self.foreign.as_mut()
            && start >= self.quiet_until
            && !opens_declaration(text.get(name_start).copied())
        {
            if closing {
                f.end(text, name_start);
            } else {
                reading = f.start(text, name_start, start, i, borrow_atom(&mut atom))?;
            }
        }
        let span = Span { start, end: i, opens_nothing: reading == ElementReading::OpensNothing };
        i += 1;
        if !self.raw_text.is_empty()
            && !closing
            && reading == ElementReading::Html
            && let Some(name) = raw_text_name_at(text, name_start, self.raw_text, self.case_sensitive)
            && i < self.absent_closers.get(name).copied().unwrap_or(usize::MAX)
        {
            if let Some(close) = raw_text_close(text, i, name, self.case_sensitive, self.check)? {
                i = close;
            } else {
                self.absent_closers.insert(name.to_owned(), i);
            }
        }
        let mut syntax = Vec::new();
        if let Some(read) = atom.as_mut() {
            let mut at = name_start;
            while at <= span.end.min(n.saturating_sub(1)) {
                if at & 0x3fff == 0 {
                    (self.check)("tag-syntax")?;
                }
                if let Some(end) = read(at)?
                    && end > at
                    && end <= span.end + 1
                {
                    syntax.push((at, end));
                    at = end;
                } else {
                    at += 1;
                }
            }
        }
        Ok(TagRead { span: Some(span), after: i, mark, syntax })
    }
}

pub fn tag_spans(text: &[u16], raw_text: Option<&[String]>) -> Vec<Span> {
    tag_spans_case(text, raw_text, false)
}
pub fn tag_spans_case(text: &[u16], raw_text: Option<&[String]>, case_sensitive: bool) -> Vec<Span> {
    tag_spans_options(text, raw_text.unwrap_or_default(), case_sensitive, None, Reading::Document, &[], &|_| Ok(()))
        .expect("unlimited scanner")
}
pub fn tag_spans_options(
    text: &[u16],
    raw_text: &[String],
    case_sensitive: bool,
    read_comment: Option<CommentRead<'_>>,
    mode: Reading,
    opaque: &[Range],
    check: CheckBudget<'_>,
) -> Result<Vec<Span>> {
    tag_spans_limited(text, raw_text, case_sensitive, read_comment, mode, opaque, check, None, false)
}
#[allow(clippy::too_many_arguments)]
pub fn tag_spans_limited(
    text: &[u16],
    raw_text: &[String],
    case_sensitive: bool,
    read_comment: Option<CommentRead<'_>>,
    mode: Reading,
    opaque: &[Range],
    check: CheckBudget<'_>,
    mut limit_at: Option<&mut dyn FnMut(usize) -> Result<Option<usize>>>,
    xml_names: bool,
) -> Result<Vec<Span>> {
    let mut reader = TagSpanReader::new(text, raw_text, case_sensitive, read_comment, mode, check, xml_names);
    let atoms: HashMap<_, _> = opaque.iter().copied().collect();
    let mut atom_end = |at| Ok(atoms.get(&at).copied());
    let mut spans = Vec::new();
    let mut from = 0;
    let mut skip = 0;
    while let Some(candidate) = reader.next(from)? {
        while skip < opaque.len() && opaque[skip].1 <= candidate.0 {
            skip += 1;
        }
        if skip < opaque.len() && opaque[skip].0 <= candidate.0 {
            from = opaque[skip].1;
            continue;
        }
        let limit = match limit_at.as_mut() {
            Some(limit_at) => limit_at(candidate.0)?,
            None => None,
        };
        let token = reader.read(candidate.0, None, (!opaque.is_empty()).then_some(&mut atom_end), limit)?;
        if let Some(span) = token.span {
            spans.push(span);
        }
        from = token.after;
    }
    Ok(spans)
}
