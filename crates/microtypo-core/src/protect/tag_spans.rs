use std::collections::{HashMap, HashSet};
use std::sync::LazyLock;

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

fn raw_text_name_at<'a>(text: &[u16], at: usize, raw_text: &'a [String]) -> Option<&'a str> {
    raw_text.iter().find(|name| name_matches_at(text, at, name, NAME_END)).map(String::as_str)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reading {
    Html,
    Document,
    Fragment,
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

fn step(state: usize, ch: u16, fragment: bool) -> isize {
    if let Some(quote) = CLOSING_QUOTE[state] {
        return if ch == quote { AFTER_QUOTE[state] as isize } else { state as isize };
    }
    if ch == GT {
        return ENDED;
    }
    if ch == LT && (fragment || state == DECLARATION) {
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

pub fn tag_end(text: &[u16], at: usize, reading: Reading) -> isize {
    if reading == Reading::Html && opens_declaration(text.get(at).copied()) {
        return js::index_of_unit(text, GT, at).map_or(-1, |p| p as isize);
    }
    let fragment = reading == Reading::Fragment;
    let mut state = first_state(text.get(at).copied());
    let mut i = at;
    while i < text.len() {
        if let Some(quote) = CLOSING_QUOTE[state] {
            match js::index_of_unit(text, quote, i) {
                None => return -1,
                Some(close) => {
                    i = close;
                    state = AFTER_QUOTE[state];
                    i += 1;
                    continue;
                }
            }
        }
        let next = step(state, text[i], fragment);
        if next == ENDED {
            return i as isize;
        }
        if next == FAILED {
            return FAILED;
        }
        state = next as usize;
        i += 1;
    }
    -1
}

pub fn tag_ends_from(text: &[u16], from: usize, reading: Reading) -> Vec<isize> {
    let fragment = reading == Reading::Fragment;
    let mut ends = vec![0isize; text.len()];
    let mut row = [-1isize; STATES];
    let mut next = [0isize; STATES];
    let mut i = text.len();
    while i > from {
        i -= 1;
        let ch = text[i];
        for (state, slot) in next.iter_mut().enumerate() {
            let to = step(state, ch, fragment);
            *slot = if to == ENDED {
                i as isize
            } else if to == FAILED {
                FAILED
            } else {
                row[to as usize]
            };
        }
        std::mem::swap(&mut row, &mut next);
        let prev = if i >= 1 { text.get(i - 1).copied() } else { None };
        let prev2 = if i >= 2 { text.get(i - 2).copied() } else { None };
        if prev == Some(LT) || (prev == Some(SLASH) && prev2 == Some(LT)) {
            ends[i] = row[first_state(Some(ch))];
        }
    }
    ends
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
    if text.get(gt) != Some(&GT) || gt == 0 || text.get(gt - 1) != Some(&SLASH) {
        return false;
    }
    let mut state = NAME;
    let mut i = start + 1;
    while i + 1 < gt {
        if let Some(quote) = CLOSING_QUOTE[state] {
            match js::index_of_unit(text, quote, i) {
                Some(close) if close < gt - 1 => {
                    i = close + 1;
                    state = AFTER_QUOTE[state];
                    continue;
                }
                _ => return false,
            }
        }
        let next = step(state, text[i], false);
        if next < 0 {
            return false;
        }
        state = next as usize;
        i += 1;
    }
    state == NAME || state == BEFORE_ATTR || state == ATTR
}

struct Frame {
    name: JsString,
    svg: bool,
    html: bool,
    element: bool,
}

#[derive(Default)]
struct ForeignContent {
    stack: Vec<Frame>,
    open: HashMap<JsString, usize>,
}

impl ForeignContent {
    fn html(&self) -> bool {
        self.stack.last().is_none_or(|f| f.html)
    }

    fn cdata(&self) -> bool {
        self.stack.last().is_some_and(|f| !f.element)
    }

    fn start(&mut self, text: &[u16], name_start: usize, start: usize, gt: usize) -> ElementReading {
        if self.html() {
            return self.start_html(text, name_start, start, gt);
        }
        let name = tag_name(text, name_start);
        if in_hash(&name, &BREAKOUT)
            || (eq(&name, "font") && FONT_BREAKOUT_RE.test(js::slice(text, name_start + 4, gt)))
        {
            self.leave();
            return self.start_html(text, name_start, start, gt);
        }
        let empty = self_closing(text, start, gt);
        if !empty {
            let top = self.stack.last().expect("foreign content has a frame");
            let svg = top.svg || (eq(&name, "svg") && eq(&top.name, "annotation-xml"));
            let html = if svg {
                in_set(&name, SVG_HTML)
            } else {
                in_set(&name, MATHML_HTML)
                    || (eq(&name, "annotation-xml") && ANNOTATION_HTML_RE.test(js::slice(text, name_start, gt)))
            };
            self.push(name, svg, html, false);
        }
        if empty { ElementReading::OpensNothing } else { ElementReading::Foreign }
    }

    fn start_html(&mut self, text: &[u16], name_start: usize, start: usize, gt: usize) -> ElementReading {
        let svg = name_matches_at(text, name_start, "svg", NAME_END);
        if svg || name_matches_at(text, name_start, "math", NAME_END) {
            if !self_closing(text, start, gt) {
                self.push(js::u(if svg { "svg" } else { "math" }), svg, false, false);
            }
        } else if !self.stack.is_empty() {
            self.open_html(tag_name(text, name_start));
        }
        ElementReading::Html
    }

    fn end(&mut self, text: &[u16], name_start: usize) {
        if self.stack.is_empty() {
            return;
        }
        let name = tag_name(text, name_start);
        if self.open.get(&name).copied().unwrap_or(0) == 0 {
            if !self.html() {
                self.leave();
            }
            return;
        }
        let html = self.stack.last().is_some_and(|f| f.element);
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
        match at {
            Some(at) => {
                while self.stack.len() > at {
                    self.pop();
                }
            }
            None => {
                if !html && !self.html() {
                    self.leave();
                }
            }
        }
    }

    fn closes(&self, name: &[u16], skip: Option<&[&str]>) -> Option<usize> {
        let html = skip.is_some();
        let floor = self.stack.len().saturating_sub(REACH);
        let mut k = self.stack.len();
        while k > floor {
            k -= 1;
            let frame = &self.stack[k];
            if frame.element != html {
                return None;
            }
            if frame.name == name {
                return Some(k);
            }
            if let Some(skip) = skip
                && !in_set(&frame.name, skip)
            {
                return None;
            }
        }
        None
    }

    fn open_html(&mut self, name: JsString) {
        let closes = siblings(&name);
        while let Some(top) = self.stack.last()
            && top.element
            && closes.is_some_and(|c| in_set(&top.name, c))
        {
            self.pop();
        }
        if in_hash(&name, &BLOCK)
            && let Some(top) = self.stack.last()
            && top.element
            && eq(&top.name, "p")
        {
            self.pop();
        }
        if !in_hash(&name, &VOID) {
            self.push(name, false, true, true);
        }
    }

    fn push(&mut self, name: JsString, svg: bool, html: bool, element: bool) {
        *self.open.entry(name.clone()).or_insert(0) += 1;
        self.stack.push(Frame { name, svg, html, element });
    }

    fn pop(&mut self) {
        if let Some(frame) = self.stack.pop()
            && let Some(n) = self.open.get_mut(&frame.name)
        {
            *n -= 1;
        }
    }

    fn leave(&mut self) {
        while !self.html() {
            self.pop();
        }
    }
}

fn raw_text_close(text: &[u16], from: usize, name: &str) -> Option<usize> {
    let mut i = from;
    while i < text.len() {
        let lt = js::index_of(text, &[LT, SLASH], i)?;
        if name_matches_at(text, lt + 2, name, NAME_END) {
            return Some(lt);
        }
        i = lt + 2;
    }
    None
}

pub fn tag_spans(text: &[u16], raw_text: Option<&[String]>) -> Vec<Span> {
    let mut spans = Vec::new();
    let n = text.len();
    let raw_text = raw_text.filter(|r| !r.is_empty());
    let scan_raw = raw_text.is_some();
    let mut foreign = if scan_raw && FOREIGN_RE.test(text) { Some(ForeignContent::default()) } else { None };
    let mut quiet_until = 0usize;
    let mut absent_closers: HashSet<String> = HashSet::new();
    let mut comment_close: Option<Option<usize>> = None;
    let mut i = 0usize;
    while i < n {
        let Some(lt) = js::index_of_unit(text, LT, i) else { break };
        i = lt;
        let mut j = i + 1;
        let closing = text.get(j) == Some(&SLASH);
        if closing {
            j += 1;
        }
        if !is_tag_name_start(js::code_point_at(text, j)) {
            i += 1;
            continue;
        }
        let start = i;
        let name_start = j;
        if foreign.as_ref().is_some_and(ForeignContent::cdata)
            && start >= quiet_until
            && js::starts_with_at(text, &js::u("<![CDATA["), start)
        {
            i = match js::index_of(text, &js::u("]]>"), start + 9) {
                None => n,
                Some(close) => close + 2,
            };
        } else if !closing && js::starts_with_at(text, COMMENT_OPEN, start) {
            let mut close = empty_comment_close(text, start);
            if close.is_none() {
                let stale = match comment_close {
                    None => true,
                    Some(None) => false,
                    Some(Some(c)) => c < start + COMMENT_OPEN.len(),
                };
                if stale {
                    comment_close = Some(js::index_of(text, COMMENT_CLOSE, start + COMMENT_OPEN.len()));
                }
                close = comment_close.flatten();
            }
            match close {
                None => {
                    quiet_until = n;
                    i = start + 1;
                    continue;
                }
                Some(close) => i = close + COMMENT_CLOSE.len() - 1,
            }
        } else {
            let gt = tag_end(text, name_start, Reading::Html);
            i = if gt == -1 { n } else { gt as usize };
        }
        let mut reading = ElementReading::Html;
        if let Some(f) = foreign.as_mut()
            && start >= quiet_until
            && !opens_declaration(text.get(name_start).copied())
        {
            if closing {
                f.end(text, name_start);
            } else {
                reading = f.start(text, name_start, start, i);
            }
        }
        spans.push(Span { start, end: i, opens_nothing: reading == ElementReading::OpensNothing });
        i += 1;
        if let Some(raw) = raw_text
            && !closing
            && reading == ElementReading::Html
            && let Some(name) = raw_text_name_at(text, name_start, raw)
            && !absent_closers.contains(name)
        {
            match raw_text_close(text, i, name) {
                None => {
                    absent_closers.insert(name.to_owned());
                }
                Some(close) => i = close,
            }
        }
    }
    spans
}
