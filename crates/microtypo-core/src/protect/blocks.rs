use std::collections::HashMap;
use std::fmt;
use std::sync::Arc;

use serde_json::json;

use crate::cat;
use crate::errors::{Error, Result};
use crate::js::{self, Caps, JsString, Piece, Re};
use crate::protect::placeholders::{CLOSE, OPEN};
use crate::protect::tag_spans::{
    COMMENT_CLOSE, COMMENT_OPEN, Span, comment_close_at, inside_tag, self_closing, tag_spans,
};
use crate::protect::vault::Vault;
use crate::protect::visible::shows_nothing;
use crate::shared::strings::skip_spaces;
use crate::shared::tag_name::{TAG_NAME_SOURCE, is_tag_name};

pub type Range = (usize, usize);
pub type CheckBudget<'a> = &'a dyn Fn(&str) -> Result<()>;
pub type Resolve<'a> = &'a dyn Fn(&[u16]) -> JsString;

pub struct ScanCtx<'a> {
    pub check_budget: CheckBudget<'a>,
    pub resolve: Resolve<'a>,
    pub elements: &'a ElementSet,
}

pub type Scanner = Arc<dyn Fn(&[u16], &ScanCtx<'_>) -> Result<Vec<Range>> + Send + Sync>;

pub fn scan_html_comments(text: &[u16], check_budget: CheckBudget<'_>) -> Result<Vec<Range>> {
    let mut spans = Vec::new();
    let mut seen = 0usize;
    let mut at = js::index_of(text, COMMENT_OPEN, 0);
    while let Some(a) = at {
        if seen & 0x3fff == 0 {
            check_budget("html-comment")?;
        }
        seen += 1;
        let Some(close) = comment_close_at(text, a) else { break };
        if close >= a + COMMENT_OPEN.len() {
            spans.push((a + COMMENT_OPEN.len(), close));
        }
        at = js::index_of(text, COMMENT_OPEN, close + COMMENT_CLOSE.len());
    }
    Ok(spans)
}

pub fn scan_line_indent(text: &[u16], check_budget: CheckBudget<'_>) -> Result<Vec<Range>> {
    let mut spans = Vec::new();
    let n = text.len();
    let mut seen = 0usize;
    let mut cr: Option<usize> = None;
    let mut start = 0;
    while start < n {
        if seen & 0x3fff == 0 {
            check_budget("line-indent")?;
        }
        seen += 1;
        let end = skip_spaces(text, start, n);
        let lf = js::index_of_unit(text, 0x0A, end).unwrap_or(n);
        if cr.is_none_or(|c| c < end) {
            cr = Some(js::index_of_unit(text, 0x0D, end).unwrap_or(n));
        }
        let next = lf.min(cr.unwrap_or(n));
        if end > start && next > end {
            spans.push((start, end));
        }
        start = next + 1;
    }
    Ok(spans)
}

static INDENT_RE: std::sync::LazyLock<Re> =
    std::sync::LazyLock::new(|| Re::new(&format!(r"\n({OPEN}W\d+{CLOSE})"), "g"));
static LIFTED_INDENT_RE: std::sync::LazyLock<Re> =
    std::sync::LazyLock::new(|| Re::new(&format!(r"\n({OPEN}W\d+{CLOSE})\n"), "g"));

fn within(ranges: &[Range], at: usize) -> bool {
    let lo = ranges.partition_point(|r| r.1 <= at);
    lo < ranges.len() && ranges[lo].0 <= at
}

fn open_comment_at(text: &[u16], tags: &[Span], opaque: &[Range]) -> Option<usize> {
    let from = js::last_index_of(text, COMMENT_CLOSE).map_or(0, |c| c + COMMENT_CLOSE.len());
    let mut cursor = 0usize;
    let mut at = js::index_of(text, COMMENT_OPEN, from);
    while let Some(a) = at {
        if !inside_tag(text, tags, &mut cursor, a) && !within(opaque, a) {
            return Some(a);
        }
        at = js::index_of(text, COMMENT_OPEN, a + 1);
    }
    None
}

type TagIndex = HashMap<usize, Span>;

fn tag_index(text: &[u16], raw_text: &[String]) -> TagIndex {
    tag_spans(text, Some(raw_text)).into_iter().map(|s| (s.start, s)).collect()
}

fn body_start(text: &[u16], index: &TagIndex, at: usize, slash_opens: bool) -> Option<usize> {
    let span = index.get(&at)?;
    if span.opens_nothing || text.get(span.end) != Some(&0x3E) {
        return None;
    }
    if !slash_opens && self_closing(text, at, span.end) { None } else { Some(span.end + 1) }
}

fn closer_from(text: &[u16], close_re: &Re, from: usize, closes: &dyn Fn(usize) -> bool) -> Option<Range> {
    let mut last = from;
    while let Some(c) = close_re.exec_at(text, last) {
        last = c.end();
        if closes(c.index()) {
            return Some((c.index(), c.end()));
        }
    }
    None
}

#[derive(Debug, Clone)]
pub struct Opener {
    open_re: Re,
    close_re: Re,
    nests: bool,
    renders: bool,
    slash_opens: bool,
}

fn unclosed_opener(
    text: &[u16],
    opener: &Opener,
    opens: &dyn Fn(usize) -> Option<usize>,
    closes: &dyn Fn(usize) -> bool,
    check_budget: CheckBudget<'_>,
) -> Result<Option<Range>> {
    let Opener { open_re, close_re, nests, .. } = opener;
    if !nests {
        let mut last = 0;
        while let Some(m) = open_re.exec_at(text, last) {
            last = m.end();
            if let Some(body) = opens(m.index()) {
                match closer_from(text, close_re, body, closes) {
                    None => return Ok(Some((m.index(), body))),
                    Some((_, end)) => last = end,
                }
            }
        }
        return Ok(None);
    }
    let mut events: Vec<(usize, usize, bool)> = Vec::new();
    for m in open_re.find_all(text) {
        if let Some(body) = opens(m.index()) {
            events.push((m.index(), body, true));
        }
    }
    if events.is_empty() {
        return Ok(None);
    }
    for m in close_re.find_all(text) {
        if closes(m.index()) {
            events.push((m.index(), 0, false));
        }
    }
    events.sort_by_key(|e| e.0);
    let mut stack: Vec<(usize, usize)> = Vec::new();
    for (seen, ev) in events.iter().enumerate() {
        if seen & 0x3fff == 0 {
            check_budget("safeBlocks.tail")?;
        }
        if ev.2 {
            stack.push((ev.0, ev.1));
        } else {
            stack.pop();
        }
    }
    Ok(stack.first().copied())
}

pub(crate) fn to_non_capturing_elements(elements: &[String]) -> String {
    let mut out = String::new();
    let mut escaped = false;
    let mut in_class = false;
    for (i, el) in elements.iter().enumerate() {
        if escaped {
            out.push_str(el);
            escaped = false;
        } else if el == "\\" {
            out.push_str(el);
            escaped = true;
        } else if el == "[" && !in_class {
            in_class = true;
            out.push_str(el);
        } else if el == "]" && in_class {
            in_class = false;
            out.push_str(el);
        } else if el == "(" && !in_class && elements.get(i + 1).is_none_or(|next| next != "?") {
            out.push_str("(?:");
        } else {
            out.push_str(el);
        }
    }
    out
}

pub(crate) fn to_non_capturing(pattern: &str) -> Result<String> {
    let chars: Vec<char> = pattern.chars().collect();
    let mut out = String::new();
    let mut escaped = false;
    let mut in_class = false;
    let mut i = 0;
    while i < chars.len() {
        let ch = chars[i];
        let named = ch == '('
            && chars.get(i + 1) == Some(&'?')
            && chars.get(i + 2) == Some(&'<')
            && chars.get(i + 3).is_some_and(|c| *c != '=' && *c != '!');
        if escaped {
            out.push(ch);
            escaped = false;
        } else if ch == '\\' {
            out.push(ch);
            escaped = true;
        } else if ch == '[' && !in_class {
            in_class = true;
            out.push(ch);
        } else if ch == ']' && in_class {
            in_class = false;
            out.push(ch);
        } else if !in_class && named {
            let Some(name_end) = chars.iter().skip(i + 3).position(|&c| c == '>').map(|p| p + i + 3) else {
                return Err(Error::config(
                    "safe block: unterminated named capture group",
                    json!({ "pattern": pattern }),
                ));
            };
            out.push_str("(?:");
            i = name_end;
        } else if ch == '(' && !in_class && chars.get(i + 1) != Some(&'?') {
            out.push_str("(?:");
        } else {
            out.push(ch);
        }
        i += 1;
    }
    Ok(out)
}

#[derive(Debug, Clone, Default)]
pub struct ElementSet {
    openers: Vec<Opener>,
    raw_text_tags: Vec<String>,
}

#[derive(Debug)]
pub struct ElementReader {
    elements: Vec<Range>,
    i: usize,
}

impl ElementReader {
    pub fn next(&mut self, from: usize) -> Option<(usize, usize, usize)> {
        while self.i < self.elements.len() && self.elements[self.i].0 < from {
            self.i += 1;
        }
        self.elements.get(self.i).map(|e| (e.0, e.1, e.1))
    }
}

impl ElementSet {
    pub fn element_reader(&self, text: &[u16], check_budget: CheckBudget<'_>) -> Result<ElementReader> {
        let mut elements: Vec<Range> = Vec::new();
        if !text.contains(&0x3C) {
            return Ok(ElementReader { elements, i: 0 });
        }
        let mut seen = 0usize;
        let mut tick = || -> Result<()> {
            if seen & 0x3fff == 0 {
                check_budget("safeBlocks.elements")?;
            }
            seen += 1;
            Ok(())
        };
        let mut index: Option<TagIndex> = None;
        for opener in &self.openers {
            let mut closers: Vec<Range> = Vec::new();
            for m in opener.close_re.find_all(text) {
                tick()?;
                let idx = index.get_or_insert_with(|| tag_index(text, &self.raw_text_tags));
                if idx.contains_key(&m.index()) {
                    closers.push((m.index(), m.end()));
                }
            }
            if closers.is_empty() {
                continue;
            }
            let idx = index.as_ref().expect("index built with the closers");
            let mut stack: Vec<usize> = Vec::new();
            let mut c = 0;
            for m in opener.open_re.find_all(text) {
                tick()?;
                if body_start(text, idx, m.index(), opener.slash_opens).is_none() {
                    continue;
                }
                while c < closers.len() && closers[c].0 < m.index() {
                    if opener.nests
                        && let Some(open) = stack.pop()
                    {
                        elements.push((open, closers[c].1));
                    }
                    c += 1;
                }
                if opener.nests {
                    stack.push(m.index());
                } else if c < closers.len() {
                    elements.push((m.index(), closers[c].1));
                }
            }
            while c < closers.len() {
                let Some(open) = stack.pop() else { break };
                elements.push((open, closers[c].1));
                c += 1;
            }
        }
        elements.sort_by_key(|e| e.0);
        Ok(ElementReader { elements, i: 0 })
    }
}

enum Block {
    Delims(Re),
    Tag(usize),
    Scanner(Scanner),
}

pub type IndentScan = fn(&[u16], CheckBudget<'_>) -> Result<Vec<Range>>;

pub struct SafeBlocks {
    vault: Vault,
    hidden_vault: Vault,
    indent_vault: Vault,
    tail_vault: Vault,
    hidden_tail_vault: Vault,
    blocks: Vec<(String, Block)>,
    elements: ElementSet,
    tail: Option<Re>,
    unclosed: bool,
    indents: Option<IndentScan>,
}

impl Default for SafeBlocks {
    fn default() -> Self {
        SafeBlocks {
            vault: Vault::new("B"),
            hidden_vault: Vault::new("N"),
            indent_vault: Vault::new("W"),
            tail_vault: Vault::new("E"),
            hidden_tail_vault: Vault::new("F"),
            blocks: Vec::new(),
            elements: ElementSet::default(),
            tail: None,
            unclosed: false,
            indents: None,
        }
    }
}

#[derive(Clone, Copy)]
enum Target {
    Visible,
    Hidden,
}

enum CallbackArg<'t> {
    Str(&'t [u16]),
    Offset(usize),
    Undefined,
}

impl CallbackArg<'_> {
    fn push_to(&self, out: &mut JsString) {
        match self {
            CallbackArg::Str(s) => out.extend_from_slice(s),
            CallbackArg::Offset(n) => js::Num(*n).push_to(out),
            CallbackArg::Undefined => out.extend("undefined".encode_utf16()),
        }
    }
}

fn callback_arg<'t>(c: &Caps<'t>, position: usize) -> CallbackArg<'t> {
    let groups = c.group_count();
    match position {
        p if p <= groups => c.get(p).map_or(CallbackArg::Undefined, CallbackArg::Str),
        p if p == groups + 1 => CallbackArg::Offset(c.index()),
        p if p == groups + 2 => CallbackArg::Str(c.input),
        _ => CallbackArg::Undefined,
    }
}

#[derive(Debug, Clone, Copy)]
pub struct TagOptions {
    pub renders: bool,
    pub nests: bool,
    pub form_feed: bool,
    pub slash_opens: bool,
}

impl Default for TagOptions {
    fn default() -> Self {
        TagOptions { renders: true, nests: true, form_feed: false, slash_opens: false }
    }
}

impl SafeBlocks {
    fn store(&mut self, body: &[u16], target: Target) -> Result<JsString> {
        if shows_nothing(body) {
            return self.hidden_vault.store(body);
        }
        match target {
            Target::Visible => self.vault.store(body),
            Target::Hidden => self.hidden_vault.store(body),
        }
    }

    pub fn add(&mut self, id: &str, open: &str, close: &str, unsafe_regex: bool) -> Result<()> {
        if open.is_empty() || close.is_empty() {
            return Err(Error::config(format!("safe block '{id}': open/close must be non-empty"), json!({ "id": id })));
        }
        let o = if unsafe_regex { to_non_capturing(open)? } else { js::regex::escape_str(open) };
        let c = if unsafe_regex { to_non_capturing(close)? } else { js::regex::escape_str(close) };
        self.add_sources(id, &o, &c, || json!({ "id": id, "open": open, "close": close }))
    }

    pub fn add_sources(
        &mut self,
        id: &str,
        o: &str,
        c: &str,
        details: impl FnOnce() -> serde_json::Value,
    ) -> Result<()> {
        let re = Re::try_new(&format!(r"({o})([\s\S]*?)({c})"), "g")
            .map_err(|_| Error::config(format!("safe block '{id}': invalid delimiter pattern"), details()))?;
        self.blocks.push((id.to_owned(), Block::Delims(re)));
        Ok(())
    }

    pub fn element_set(&self) -> &ElementSet {
        &self.elements
    }

    pub fn reset(&mut self) {
        self.vault.reset();
        self.hidden_vault.reset();
        self.indent_vault.reset();
        self.tail_vault.reset();
        self.hidden_tail_vault.reset();
    }

    pub fn add_tag(&mut self, tag: &str, options: TagOptions) -> Result<()> {
        if !is_tag_name(tag) {
            let quoted = js::lossy(&js::json::stringify(&js::u(tag)));
            return Err(Error::type_error(format!(
                "addSafeTag: invalid tag name {quoted} — must match {TAG_NAME_SOURCE}"
            )));
        }
        let open_re = Re::new(&format!(r"<{tag}(?=[ \t\n\r\f/>])"), "gi");
        let ff = if options.form_feed { r"\f" } else { "" };
        let close_re = Re::new(&format!(r"<\/{tag}[ \t\r\n{ff}]*>"), "gi");
        if !options.nests {
            self.elements.raw_text_tags.push(tag.to_ascii_lowercase());
        }
        self.elements.openers.push(Opener {
            open_re,
            close_re,
            nests: options.nests,
            renders: options.renders,
            slash_opens: options.slash_opens,
        });
        self.blocks.push((tag.to_owned(), Block::Tag(self.elements.openers.len() - 1)));
        Ok(())
    }

    fn protect_raw_text(&mut self, text: &[u16], k: usize, check_budget: CheckBudget<'_>) -> Result<JsString> {
        if !text.contains(&0x3C) {
            return Ok(text.to_vec());
        }
        let opener = self.elements.openers[k].clone();
        let target = if opener.renders { Target::Visible } else { Target::Hidden };
        let mut parts: Vec<JsString> = Vec::new();
        let mut index: Option<TagIndex> = None;
        let mut cursor = 0;
        let mut seen = 0usize;
        let mut last = 0;
        while let Some(open) = opener.open_re.exec_at(text, last) {
            last = open.end();
            if seen & 0x3fff == 0 {
                check_budget("safeBlocks.tag")?;
            }
            seen += 1;
            let idx = index.get_or_insert_with(|| tag_index(text, &self.elements.raw_text_tags));
            let Some(start) = body_start(text, idx, open.index(), opener.slash_opens) else { continue };
            let idx = &*idx;
            let Some((close_at, close_end)) = closer_from(text, &opener.close_re, start, &|at| idx.contains_key(&at))
            else {
                self.unclosed = true;
                break;
            };
            parts.push(text[cursor..start].to_vec());
            let stored = self.store(&text[start..close_at], target)?;
            parts.push(stored);
            parts.push(text[close_at..close_end].to_vec());
            cursor = close_end;
            last = cursor;
        }
        if parts.is_empty() {
            return Ok(text.to_vec());
        }
        parts.push(text[cursor..].to_vec());
        Ok(parts.concat())
    }

    fn protect_nesting(&mut self, text: &[u16], k: usize, check_budget: CheckBudget<'_>) -> Result<JsString> {
        if !text.contains(&0x3C) {
            return Ok(text.to_vec());
        }
        let opener = self.elements.openers[k].clone();
        let target = if opener.renders { Target::Visible } else { Target::Hidden };
        let mut events: Vec<(usize, Option<usize>, bool)> = Vec::new();
        for m in opener.open_re.find_all(text) {
            events.push((m.index(), None, true));
        }
        for m in opener.close_re.find_all(text) {
            events.push((m.index(), None, false));
        }
        if events.is_empty() {
            return Ok(text.to_vec());
        }
        let index = tag_index(text, &self.elements.raw_text_tags);
        let mut kept: Vec<(usize, Option<usize>, bool)> = events
            .into_iter()
            .filter_map(|(at, _, open)| {
                if !open {
                    return index.contains_key(&at).then_some((at, None, false));
                }
                body_start(text, &index, at, opener.slash_opens).map(|end| (at, Some(end), true))
            })
            .collect();
        if kept.is_empty() {
            return Ok(text.to_vec());
        }
        kept.sort_by_key(|e| e.0);
        let mut spans: Vec<(usize, usize)> = Vec::new();
        let mut stack: Vec<usize> = Vec::new();
        for (seen, ev) in kept.iter().enumerate() {
            if seen & 0x3fff == 0 {
                check_budget("safeBlocks.tag")?;
            }
            if ev.2 {
                stack.push(ev.1.expect("an opener has a body"));
            } else if let Some(start) = stack.pop()
                && stack.is_empty()
            {
                spans.push((start, ev.0));
            }
        }
        if !stack.is_empty() {
            self.unclosed = true;
        }
        if spans.is_empty() {
            return Ok(text.to_vec());
        }
        let mut parts: Vec<JsString> = Vec::new();
        let mut cursor = 0;
        for (body, close) in spans {
            parts.push(text[cursor..body].to_vec());
            let stored = self.store(&text[body..close], target)?;
            parts.push(stored);
            cursor = close;
        }
        parts.push(text[cursor..].to_vec());
        Ok(parts.concat())
    }

    pub fn add_scanner(&mut self, id: &str, scan: Scanner) {
        self.blocks.push((id.to_owned(), Block::Scanner(scan)));
    }

    fn vault_spans(
        text: &[u16],
        spans: &[Range],
        store: &mut dyn FnMut(&[u16]) -> Result<JsString>,
    ) -> Result<JsString> {
        if spans.is_empty() {
            return Ok(text.to_vec());
        }
        let mut parts: Vec<JsString> = Vec::new();
        let mut cursor = 0;
        for &(start, end) in spans {
            parts.push(text[cursor..start].to_vec());
            parts.push(store(&text[start..end])?);
            cursor = end;
        }
        parts.push(text[cursor..].to_vec());
        Ok(parts.concat())
    }

    pub fn set_tail(&mut self, opaque: Re) {
        self.tail = Some(opaque);
    }

    pub fn set_indents(&mut self, scan: IndentScan) {
        self.indents = Some(scan);
    }

    pub fn protect(&mut self, text: &[u16], check_budget: CheckBudget<'_>, resolve: Resolve<'_>) -> Result<JsString> {
        let mut result = text.to_vec();
        self.unclosed = false;
        let blocks = std::mem::take(&mut self.blocks);
        let outcome = (|| -> Result<()> {
            for (_, block) in &blocks {
                result = match block {
                    Block::Delims(re) => re.try_replace_with(&result, |c: &Caps<'_>| {
                        let open = callback_arg(c, 1);
                        let stored = match callback_arg(c, 2) {
                            CallbackArg::Str(body) => self.store(body, Target::Visible)?,
                            CallbackArg::Offset(_) => return Err(Error::type_error("body.trim is not a function")),
                            CallbackArg::Undefined => {
                                return Err(Error::type_error("Cannot read properties of undefined (reading 'trim')"));
                            }
                        };
                        let mut out = JsString::new();
                        open.push_to(&mut out);
                        out.extend(stored);
                        callback_arg(c, 3).push_to(&mut out);
                        Ok(out)
                    })?,
                    Block::Tag(k) => {
                        if self.elements.openers[*k].nests {
                            self.protect_nesting(&result, *k, check_budget)?
                        } else {
                            self.protect_raw_text(&result, *k, check_budget)?
                        }
                    }
                    Block::Scanner(scan) => {
                        let ctx = ScanCtx { check_budget, resolve, elements: &self.elements };
                        let spans = scan(&result, &ctx)?;
                        Self::vault_spans(&result, &spans, &mut |body| self.store(body, Target::Visible))?
                    }
                };
            }
            Ok(())
        })();
        self.blocks = blocks;
        outcome?;
        if self.tail.is_some() {
            result = self.protect_tail(&result, check_budget)?;
        }
        match self.indents {
            None => Ok(result),
            Some(scan) => {
                let spans = scan(&result, check_budget)?;
                let vault = &mut self.indent_vault;
                Self::vault_spans(&result, &spans, &mut |indent| vault.store(indent))
            }
        }
    }

    fn protect_tail(&mut self, text: &[u16], check_budget: CheckBudget<'_>) -> Result<JsString> {
        if !text.contains(&0x3C) {
            return Ok(text.to_vec());
        }
        let open_comment =
            js::last_index_of(text, COMMENT_OPEN).is_some_and(|last| comment_close_at(text, last).is_none());
        let tags = tag_spans(text, Some(&self.elements.raw_text_tags));
        let unended = tags.last().filter(|s| s.end == text.len()).map(|s| s.start);
        if !self.unclosed && !open_comment && unended.is_none() {
            return Ok(text.to_vec());
        }
        let index: TagIndex = tags.iter().map(|s| (s.start, *s)).collect();
        let opaque: Vec<Range> =
            self.tail.as_ref().expect("tail set").find_all(text).iter().map(|m| (m.index(), m.end())).collect();
        let closes = |at: usize| index.contains_key(&at) && !within(&opaque, at);
        let mut first = unended.filter(|&u| !within(&opaque, u));
        let mut first_end = first;
        let mut hidden_vault = true;
        let comment = open_comment_at(text, &tags, &opaque);
        if let Some(comment) = comment
            && first.is_none_or(|f| comment <= f)
        {
            first = Some(comment);
            first_end = Some(comment + COMMENT_OPEN.len());
            hidden_vault = false;
        }
        for opener in &self.elements.openers {
            let opens =
                |at: usize| if within(&opaque, at) { None } else { body_start(text, &index, at, opener.slash_opens) };
            let open = unclosed_opener(text, opener, &opens, &closes, check_budget)?;
            if let Some((at, end)) = open
                && first.is_none_or(|f| at < f)
            {
                first = Some(at);
                first_end = Some(end);
                hidden_vault = !opener.renders;
            }
        }
        let Some(first_end) = first_end else { return Ok(text.to_vec()) };
        let rest = &text[first_end..];
        let hidden = hidden_vault || shows_nothing(rest);
        let stored = if hidden { self.hidden_tail_vault.store(rest)? } else { self.tail_vault.store(rest)? };
        Ok(cat!(&text[..first_end], stored))
    }

    pub fn lift_indents(&self, text: &[u16]) -> JsString {
        INDENT_RE.replace_str(text, "\n$1\n")
    }

    pub fn settle_indents(&self, text: &[u16]) -> JsString {
        LIFTED_INDENT_RE.replace_with(text, |c| cat!("\n", self.indent_vault.restore_all(c.s(1))))
    }

    pub fn restore(&self, text: JsString) -> JsString {
        let vaults = [&self.tail_vault, &self.hidden_tail_vault, &self.vault, &self.hidden_vault, &self.indent_vault];
        Vault::restore_fixpoint(&vaults, text)
    }
}

impl fmt::Debug for ScanCtx<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ScanCtx").finish_non_exhaustive()
    }
}

impl fmt::Debug for SafeBlocks {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let ids: Vec<&str> = self.blocks.iter().map(|(id, _)| id.as_str()).collect();
        f.debug_struct("SafeBlocks").field("blocks", &ids).finish_non_exhaustive()
    }
}
