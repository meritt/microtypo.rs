use std::collections::HashMap;

use crate::errors::Result;
use crate::js::{self, JsString};
use crate::protect::blocks::CheckBudget;
use crate::shared::strings::is_backslash_escaped;

fn name_start(c: u16) -> bool {
    (65..=90).contains(&c) || (97..=122).contains(&c)
}
fn name_part(c: u16) -> bool {
    name_start(c) || (48..=57).contains(&c) || matches!(c, 95 | 46 | 58 | 45)
}
fn word_start(c: u16) -> bool {
    name_start(c) || matches!(c, 95 | 36)
}
fn word_part(c: u16) -> bool {
    word_start(c) || (48..=57).contains(&c)
}
pub static NATIVE_BLOCK_AT: std::sync::LazyLock<js::Re> = std::sync::LazyLock::new(|| {
    let names: Vec<&str> = crate::shared::block_tags::BLOCK_TAG_NAMES
        .iter()
        .chain(crate::shared::block_tags::VOID_BLOCK_TAG_NAMES)
        .copied()
        .collect();
    js::Re::new(&format!(r"</?(?:p|{})(?=[\s/>])", names.join("|")), "uy")
});

const VALUE_PREFIX: &[&str] =
    &["return", "throw", "case", "yield", "await", "typeof", "void", "delete", "new", "in", "of", "instanceof"];
const CONTROL_HEADS: &[&str] = &["if", "while", "for", "with", "switch", "catch"];
const BLOCK_HEADS: &[&str] = &["else", "try", "finally", "do"];

#[derive(Clone, Copy)]
pub struct Boundary {
    pub end: usize,
    pub next: usize,
}

pub fn paragraph_ends(text: &[u16], check: CheckBudget<'_>) -> Result<Vec<Boundary>> {
    let mut endings = Vec::new();
    let (mut start, mut previous, mut at) = (0, 0, 0);
    while at <= text.len() {
        if at & 1023 == 0 {
            check("mdx")?;
        }
        if at < text.len() && !matches!(text[at], 10 | 13) {
            at += 1;
            continue;
        }
        let mut content = start;
        while content < at && matches!(text[content], 32 | 9) {
            content += 1;
        }
        if content == at {
            endings.push(Boundary { end: previous, next: start });
        }
        previous = at;
        if text.get(at) == Some(&13) && text.get(at + 1) == Some(&10) {
            at += 1;
        }
        start = at + 1;
        at += 1;
    }
    endings.push(Boundary { end: text.len(), next: text.len() });
    Ok(endings)
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Expression = 0,
    Template = 1,
    Tag = 2,
    Jsx = 3,
}

#[derive(Clone, Copy)]
struct Body {
    returns_value: bool,
}
#[derive(Clone, Copy)]
struct Class {
    body: Body,
    depth: usize,
}
#[derive(Clone, Copy)]
struct Paren {
    control: bool,
    function_head: Option<Body>,
}

struct Frame {
    at: usize,
    kind: Kind,
    statement_body: bool,
    statement_start: bool,
    returns_value: bool,
    expect_value: bool,
    body_next: Option<Body>,
    property: bool,
    label_candidate: bool,
    parens: Vec<Paren>,
    brackets: usize,
    classes: Vec<Class>,
    control_next: Option<String>,
    function_head: Option<Body>,
    async_statement: bool,
    name: JsString,
    closing: bool,
    names: Vec<JsString>,
}

impl Frame {
    fn new(at: usize, kind: Kind) -> Self {
        Self {
            at,
            kind,
            statement_body: false,
            statement_start: false,
            returns_value: true,
            expect_value: true,
            body_next: None,
            property: false,
            label_candidate: false,
            parens: Vec::new(),
            brackets: 0,
            classes: Vec::new(),
            control_next: None,
            function_head: None,
            async_statement: false,
            name: Vec::new(),
            closing: false,
            names: Vec::new(),
        }
    }
    fn expression(at: usize, statement: bool, returns_value: bool) -> Self {
        Self { statement_body: statement, statement_start: statement, returns_value, ..Self::new(at, Kind::Expression) }
    }
    fn result(&mut self, value: bool) {
        self.expect_value = !value;
        self.statement_start = !value && self.statement_body;
        self.body_next = None;
        self.property = false;
        self.label_candidate = false;
    }
    fn depth(&self) -> usize {
        self.parens.len() + self.brackets
    }
    fn brace(&mut self, at: usize) -> Self {
        let class_body = if self.body_next.is_none() && self.classes.last().is_some_and(|c| c.depth == self.depth()) {
            self.classes.pop().map(|c| c.body)
        } else {
            None
        };
        let body = self.body_next.or(class_body);
        let statement = body.is_some() || self.statement_start || !self.expect_value;
        Self::expression(at, statement, body.map_or(!statement, |b| b.returns_value))
    }
}

pub struct MdxReader<'a> {
    text: &'a [u16],
    check: CheckBudget<'a>,
    caches: [HashMap<usize, isize>; 4],
    strings: HashMap<usize, isize>,
    attributes: HashMap<usize, isize>,
    comments: HashMap<usize, isize>,
    block_comments: Option<HashMap<usize, isize>>,
    regexps: HashMap<usize, isize>,
    boundaries: Vec<Boundary>,
    boundary: usize,
    next_check: usize,
    last_tag_close: Option<usize>,
}

impl<'a> MdxReader<'a> {
    pub fn new(text: &'a [u16], check: CheckBudget<'a>) -> Result<Self> {
        Ok(Self {
            text,
            check,
            caches: std::array::from_fn(|_| HashMap::new()),
            strings: HashMap::new(),
            attributes: HashMap::new(),
            comments: HashMap::new(),
            block_comments: None,
            regexps: HashMap::new(),
            boundaries: paragraph_ends(text, check)?,
            boundary: 0,
            next_check: 0,
            last_tag_close: text.iter().rposition(|&c| c == 62),
        })
    }
    fn tick(&mut self, at: usize) -> Result<()> {
        if at >= self.next_check {
            (self.check)("mdx")?;
            self.next_check = at + 1024;
        }
        Ok(())
    }
    fn quoted(&mut self, at: usize, multiline: bool) -> Result<isize> {
        let cache = if multiline { &self.attributes } else { &self.strings };
        if let Some(&end) = cache.get(&at) {
            return Ok(end);
        }
        let quote = self.text[at];
        let mut end = at + 1;
        while end < self.text.len() && self.text[end] != quote {
            self.tick(end)?;
            if !multiline && matches!(self.text[end], 13 | 10) {
                self.strings.insert(at, -1);
                return Ok(-1);
            }
            end += if !multiline && self.text[end] == 92 { 2 } else { 1 };
        }
        let found = if end < self.text.len() { (end + 1) as isize } else { -1 };
        (if multiline { &mut self.attributes } else { &mut self.strings }).insert(at, found);
        Ok(found)
    }
    fn comment(&mut self, at: usize) -> Result<isize> {
        if let Some(&end) = self.comments.get(&at) {
            return Ok(end);
        }
        let end = if self.text.get(at + 1) == Some(&47) {
            let mut end = at + 2;
            while end < self.text.len() && !matches!(self.text[end], 10 | 13) {
                self.tick(end)?;
                end += 1;
            }
            end as isize
        } else {
            if self.block_comments.is_none() {
                let mut found = HashMap::new();
                let (mut next, mut after) = (-1, -1);
                for i in (0..self.text.len().saturating_sub(1)).rev() {
                    if i & 1023 == 0 {
                        (self.check)("mdx-comment")?;
                    }
                    if self.text[i] == 42 && self.text[i + 1] == 47 {
                        after = next;
                        next = (i + 2) as isize;
                    } else if self.text[i] == 47 && self.text[i + 1] == 42 {
                        found.insert(i, if next == (i + 3) as isize { after } else { next });
                    }
                }
                self.block_comments = Some(found);
            }
            *self.block_comments.as_ref().expect("indexed").get(&at).unwrap_or(&-1)
        };
        self.comments.insert(at, end);
        Ok(end)
    }
    fn regexp(&mut self, start: usize) -> Result<isize> {
        if let Some(&end) = self.regexps.get(&start) {
            return Ok(end);
        }
        let (mut at, mut class) = (start + 1, false);
        while at < self.text.len() && !matches!(self.text[at], 10 | 13) {
            self.tick(at)?;
            if self.text[at] == 92 {
                at += 2;
            } else if self.text[at] == 47 && !class {
                at += 1;
                while self.text.get(at).is_some_and(|&c| name_start(c)) {
                    self.tick(at)?;
                    at += 1;
                }
                self.regexps.insert(start, at as isize);
                return Ok(at as isize);
            } else {
                if self.text[at] == 91 {
                    class = true;
                } else if self.text[at] == 93 {
                    class = false;
                }
                at += 1;
            }
        }
        self.regexps.insert(start, -1);
        Ok(-1)
    }
    fn finish(&mut self, stack: &mut Vec<Frame>, end: usize) -> Frame {
        let frame = stack.pop().expect("active frame");
        self.caches[frame.kind as usize].insert(frame.at, end as isize);
        if let Some(parent) = stack.last_mut()
            && parent.kind == Kind::Expression
        {
            parent.result(frame.kind != Kind::Expression || frame.returns_value);
        }
        frame
    }
    fn read_syntax(&mut self, start: usize, kind: Kind) -> Result<isize> {
        if let Some(&end) = self.caches[kind as usize].get(&start) {
            return Ok(end);
        }
        let mut stack = vec![Frame::new(start, kind)];
        let mut at = if kind == Kind::Tag { start } else { start + 1 };
        while at < self.text.len() && !stack.is_empty() {
            self.tick(at)?;
            let frame = stack.last_mut().expect("active frame");
            let ch = self.text[at];
            if frame.kind == Kind::Tag {
                if at == frame.at {
                    let name_start_at = at + if self.text.get(at + 1) == Some(&47) { 2 } else { 1 };
                    at = name_start_at;
                    if self.text.get(at) != Some(&62) && !self.text.get(at).is_some_and(|&c| name_start(c)) {
                        break;
                    }
                    while at < self.text.len() && name_part(self.text[at]) {
                        self.tick(at)?;
                        at += 1;
                    }
                    let frame = stack.last_mut().expect("tag frame");
                    frame.name = self.text[name_start_at..at].to_vec();
                    frame.closing = self.text.get(frame.at + 1) == Some(&47);
                } else if ch == 62 {
                    let empty = self.text[at - 1] == 47;
                    at += 1;
                    let tag = self.finish(&mut stack, at);
                    if let Some(parent) = stack.last_mut()
                        && parent.kind == Kind::Jsx
                    {
                        if tag.closing {
                            if parent.names.pop().as_ref() != Some(&tag.name) {
                                break;
                            }
                        } else if !empty {
                            parent.names.push(tag.name);
                        }
                        if parent.names.is_empty() {
                            self.finish(&mut stack, at);
                        }
                    }
                } else if ch == 60 {
                    break;
                } else if matches!(ch, 34 | 39) {
                    let end = self.quoted(at, true)?;
                    if end < 0 {
                        break;
                    }
                    at = end as usize;
                } else if ch == 123 {
                    stack.push(Frame::new(at, Kind::Expression));
                    at += 1;
                } else {
                    at += 1;
                }
                continue;
            }
            if frame.kind == Kind::Jsx {
                if ch == 60 {
                    stack.push(Frame::new(at, Kind::Tag));
                } else if ch == 123 {
                    stack.push(Frame::new(at, Kind::Expression));
                    at += 1;
                } else {
                    at += 1;
                }
                continue;
            }
            if frame.kind == Kind::Template {
                if ch == 92 {
                    at += 2;
                } else if ch == 96 {
                    self.finish(&mut stack, at + 1);
                    at += 1;
                } else if ch == 36 && self.text.get(at + 1) == Some(&123) {
                    at += 1;
                    stack.push(Frame::new(at, Kind::Expression));
                    at += 1;
                } else {
                    at += 1;
                }
                continue;
            }
            if matches!(ch, 34 | 39) {
                let end = self.quoted(at, false)?;
                if end < 0 {
                    break;
                }
                at = end as usize;
                stack.last_mut().expect("expression").result(true);
            } else if ch == 47 && matches!(self.text.get(at + 1), Some(47 | 42)) {
                let end = self.comment(at)?;
                if end < 0 {
                    break;
                }
                at = end as usize;
            } else if ch == 47 && frame.expect_value {
                let end = self.regexp(at)?;
                if end < 0 {
                    break;
                }
                at = end as usize;
                stack.last_mut().expect("expression").result(true);
            } else if ch == 96 {
                if let Some(&end) = self.caches[Kind::Template as usize].get(&at) {
                    if end < 0 {
                        break;
                    }
                    at = end as usize;
                    frame.result(true);
                } else {
                    stack.push(Frame::new(at, Kind::Template));
                    at += 1;
                }
            } else if ch == 123 {
                let child = frame.brace(at);
                if let Some(&end) = self.caches[Kind::Expression as usize].get(&at) {
                    if end < 0 {
                        break;
                    }
                    at = end as usize;
                    frame.result(child.returns_value);
                } else {
                    stack.push(child);
                    at += 1;
                }
            } else if ch == 125 {
                self.finish(&mut stack, at + 1);
                at += 1;
            } else if ch == 60
                && frame.expect_value
                && (self.text.get(at + 1).is_some_and(|&c| name_start(c)) || self.text.get(at + 1) == Some(&62))
            {
                stack.push(Frame::new(at, Kind::Jsx));
                stack.push(Frame::new(at, Kind::Tag));
            } else if word_start(ch) {
                let from = at;
                at += 1;
                while self.text.get(at).is_some_and(|&c| word_part(c)) {
                    self.tick(at)?;
                    at += 1;
                }
                let word = js::lossy(&self.text[from..at]);
                let frame = stack.last_mut().expect("expression");
                let property = frame.property;
                let statement = frame.statement_start;
                let for_await = !property && frame.control_next.as_deref() == Some("for") && word == "await";
                frame.control_next = if for_await {
                    Some("for".to_owned())
                } else {
                    (!property && CONTROL_HEADS.contains(&word.as_str())).then(|| word.clone())
                };
                if !property && word == "function" {
                    frame.function_head = Some(Body { returns_value: !(statement || frame.async_statement) });
                }
                if !property && word == "class" {
                    frame.classes.push(Class { body: Body { returns_value: !statement }, depth: frame.depth() });
                }
                frame.async_statement = !property && word == "async" && statement;
                let block = !property && BLOCK_HEADS.contains(&word.as_str());
                frame.label_candidate =
                    statement && !property && !block && !CONTROL_HEADS.contains(&word.as_str()) && word != "function";
                frame.statement_start = block;
                frame.expect_value = block || for_await || (!property && VALUE_PREFIX.contains(&word.as_str()));
                frame.property = false;
            } else if matches!(ch, 43 | 45) && self.text.get(at + 1) == Some(&ch) {
                frame.control_next = None;
                frame.statement_start = false;
                frame.label_candidate = false;
                at += 2;
            } else if ch == 91 {
                frame.brackets += 1;
                frame.expect_value = true;
                frame.statement_start = false;
                frame.label_candidate = false;
                frame.property = false;
                at += 1;
            } else if ch == 93 {
                frame.brackets = frame.brackets.saturating_sub(1);
                frame.result(true);
                at += 1;
            } else if ch == 40 {
                frame
                    .parens
                    .push(Paren { control: frame.control_next.is_some(), function_head: frame.function_head.take() });
                frame.control_next = None;
                frame.expect_value = true;
                frame.statement_start = false;
                frame.label_candidate = false;
                frame.property = false;
                at += 1;
            } else if ch == 41 {
                let header = frame.parens.pop();
                frame.expect_value = header.is_some_and(|h| h.control || h.function_head.is_some());
                frame.statement_start = header.is_some_and(|h| h.control);
                frame.body_next = header.and_then(|h| h.function_head);
                frame.control_next = None;
                frame.label_candidate = false;
                frame.property = false;
                at += 1;
            } else {
                if !js::is_js_space(ch) {
                    let arrow = ch == 62 && at > 0 && self.text[at - 1] == 61;
                    let label = ch == 58 && frame.label_candidate;
                    frame.body_next = arrow.then_some(Body { returns_value: true });
                    frame.statement_start = label || (ch == 59 && frame.statement_body && frame.parens.is_empty());
                    frame.expect_value = "([,:;=!?&|+*%<>~^/-".bytes().any(|c| ch == u16::from(c));
                    frame.property = ch == 46 || (ch == 35 && frame.property);
                    frame.control_next = None;
                    frame.label_candidate = false;
                }
                at += 1;
            }
        }
        for frame in stack {
            self.caches[frame.kind as usize].insert(frame.at, -1);
        }
        Ok(*self.caches[kind as usize].get(&start).unwrap_or(&-1))
    }
    pub fn next(&mut self, from: usize) -> Result<Option<(usize, usize, usize)>> {
        for at in from..self.text.len() {
            let c = self.text[at];
            let tag = c == 60
                && (self.text.get(at + 1).is_some_and(|&c| name_start(c))
                    || self.text.get(at + 1) == Some(&62)
                    || (self.text.get(at + 1) == Some(&47)
                        && (self.text.get(at + 2).is_some_and(|&c| name_start(c))
                            || self.text.get(at + 2) == Some(&62))));
            if c != 123 && !tag {
                continue;
            }
            self.tick(at)?;
            if !is_backslash_escaped(self.text, at) {
                return Ok(Some((at, at, at)));
            }
        }
        Ok(None)
    }
    pub fn read(&mut self, start: usize) -> Result<(usize, usize, usize, bool)> {
        let end = if self.text[start] == 123 {
            self.read_syntax(start, Kind::Expression)?
        } else if self.text[start] == 60 {
            if self.last_tag_close.is_none_or(|p| p < start) { -1 } else { self.read_syntax(start, Kind::Tag)? }
        } else {
            (start + 1) as isize
        };
        let closed = end != -1;
        let end = if closed {
            end as usize
        } else {
            while self.boundary < self.boundaries.len() && self.boundaries[self.boundary].end <= start {
                self.boundary += 1;
            }
            self.boundaries.get(self.boundary).map_or(self.text.len(), |b| b.end)
        };
        Ok((start, end, end, closed))
    }
}

pub struct EsmReader {
    boundaries: Vec<Boundary>,
    cursor: usize,
}
impl EsmReader {
    pub fn new(text: &[u16], check: CheckBudget<'_>) -> Result<Self> {
        Ok(Self { boundaries: paragraph_ends(text, check)?, cursor: 0 })
    }
    pub fn read(&mut self, text: &[u16], at: usize) -> Option<Boundary> {
        let width = if js::starts_with_at(text, &js::u("import"), at) || js::starts_with_at(text, &js::u("export"), at)
        {
            6
        } else {
            return None;
        };
        if text.get(at + width).is_some_and(|c| !matches!(c, 32 | 9 | 123 | 42 | 39 | 34 | 13 | 10)) {
            return None;
        }
        while self.cursor < self.boundaries.len() && self.boundaries[self.cursor].end <= at {
            self.cursor += 1;
        }
        Some(self.boundaries.get(self.cursor).copied().unwrap_or(Boundary { end: text.len(), next: text.len() }))
    }
}

pub fn tag_spans(
    text: &[u16],
    check: CheckBudget<'_>,
    opaque: &[(usize, usize)],
) -> Result<Vec<crate::protect::tag_spans::Span>> {
    let mut reader = MdxReader::new(text, check)?;
    let mut tags = Vec::new();
    let mut candidate = reader.next(0)?;
    let mut skip = 0;
    while let Some((start, _, _)) = candidate {
        while skip < opaque.len() && opaque[skip].1 <= start {
            skip += 1;
        }
        if skip < opaque.len() && opaque[skip].0 <= start {
            candidate = reader.next(opaque[skip].1)?;
            continue;
        }
        let (_, end, after, closed) = reader.read(start)?;
        if text[start] == 60 && closed {
            tags.push(crate::protect::tag_spans::Span {
                start,
                end: end - 1,
                opens_nothing: text.get(end.wrapping_sub(2)) == Some(&47),
            });
        }
        candidate = reader.next(after)?;
    }
    Ok(tags)
}
