use std::collections::{HashMap, HashSet};
use std::fmt;
use std::sync::{Arc, OnceLock};

use serde_json::json;

use crate::cat;
use crate::errors::{Error, Result};
use crate::js::{self, Caps, JsString, Piece, Re};
use crate::protect::placeholders::{CLOSE, CLOSE_UNIT, OPEN, OPEN_UNIT};
use crate::protect::tag_spans::{COMMENT_CLOSE, COMMENT_OPEN, Span, comment_close_at, inside_tag, self_closing};
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
    pub code_edges: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpanKind {
    Plain,
    Tail,
    LineBlock,
    TableDelimiter,
    LabelAddress,
    InlineCode(usize),
}

pub type Spanned = (usize, usize, SpanKind);

pub type Scanner = Arc<dyn Fn(&[u16], &ScanCtx<'_>) -> Result<Vec<Range>> + Send + Sync>;
pub type OpaqueScanner = Arc<dyn Fn(&[u16], &ScanCtx<'_>) -> Result<Vec<Spanned>> + Send + Sync>;
pub type BodyTypesetter<'a> = dyn FnMut(&[u16], &SafeBlocks) -> Result<JsString> + 'a;
pub type TypesetBody<'a> = &'a mut BodyTypesetter<'a>;
pub type AfterInline<'a> = &'a mut dyn FnMut(JsString, &[usize], &ElementSet) -> Result<JsString>;

#[derive(Debug, Default)]
pub struct DocumentScan {
    pub spans: Vec<(usize, usize, bool)>,
    pub ignore_file: bool,
}

pub type DocumentScanner = Arc<dyn Fn(&[u16], &ScanCtx<'_>, Resolve<'_>) -> Result<DocumentScan> + Send + Sync>;

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
        if js::starts_with_at(text, &js::u("<p>"), end) {
            let inner_start = end + 3;
            let inner_end = skip_spaces(text, inner_start, next);
            if inner_end > inner_start && inner_end < next {
                spans.push((inner_start, inner_end));
            }
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

fn js_index(digits: &[u16]) -> Option<usize> {
    let start = digits.iter().position(|&c| !js::is_js_space(c)).unwrap_or(digits.len());
    let end = digits.iter().rposition(|&c| !js::is_js_space(c)).map_or(start, |e| e + 1);
    let digits = &digits[start..end];
    if !digits.iter().all(|c| (0x30..=0x39).contains(c)) {
        return None;
    }
    digits.iter().try_fold(0usize, |n, &d| n.checked_mul(10)?.checked_add(usize::from(d - 0x30)))
}

fn tag_index(text: &[u16], elements: &ElementSet) -> Result<TagIndex> {
    let unlimited: CheckBudget<'_> = &|_| Ok(());
    let boundaries =
        if elements.paragraph_limit { crate::input::mdx::paragraph_ends(text, unlimited)? } else { Vec::new() };
    let mut cursor = 0;
    let mut limit = |from: usize| -> Result<Option<usize>> {
        while cursor < boundaries.len() && boundaries[cursor].end <= from {
            cursor += 1;
        }
        Ok(Some(boundaries.get(cursor).map_or(text.len(), |b| b.end)))
    };
    crate::protect::tag_spans::tag_spans_limited(
        text,
        &elements.raw_text_tags,
        elements.case_sensitive,
        elements.comments.as_ref().map(|f| f(text, unlimited)),
        elements.reading,
        &[],
        unlimited,
        if elements.paragraph_limit { Some(&mut limit) } else { None },
        elements.xml_names,
    )
    .map(|spans| spans.into_iter().map(|s| (s.start, s)).collect())
}

fn closing_end(text: &[u16], index: &TagIndex, at: usize) -> Option<usize> {
    index.get(&at).filter(|s| text.get(s.end) == Some(&0x3E)).map(|s| s.end + 1)
}

fn body_start(text: &[u16], index: &TagIndex, at: usize, slash_opens: bool) -> Option<usize> {
    let span = index.get(&at)?;
    if span.opens_nothing || text.get(span.end) != Some(&0x3E) {
        return None;
    }
    if !slash_opens && self_closing(text, at, span.end) { None } else { Some(span.end + 1) }
}

fn closer_from(text: &[u16], close_re: &Re, from: usize, closes: &dyn Fn(usize) -> Option<usize>) -> Option<Range> {
    let mut last = from;
    while let Some(c) = close_re.exec_at(text, last) {
        last = c.end();
        if let Some(end) = closes(c.index()) {
            return Some((c.index(), end));
        }
    }
    None
}

#[derive(Debug, Clone)]
pub struct Opener {
    open_re: Re,
    close_re: Re,
    heads: OnceLock<(Re, Re)>,
    nests: bool,
    renders: bool,
    slash_opens: bool,
    typeset: bool,
}

impl Opener {
    fn heads(&self) -> &(Re, Re) {
        self.heads.get_or_init(|| {
            let head = |re: &Re| Re::configured(&format!("^(?:{})", re.source()), &re.flags().replace(['g', 'y'], ""));
            (head(&self.open_re), head(&self.close_re))
        })
    }
}

fn unclosed_opener(
    text: &[u16],
    opener: &Opener,
    opens: &dyn Fn(usize) -> Option<usize>,
    closes: &dyn Fn(usize) -> Option<usize>,
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
        if closes(m.index()).is_some() {
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

#[derive(Clone)]
pub struct ElementSet {
    openers: Vec<Opener>,
    raw_text_tags: Vec<String>,
    case_sensitive: bool,
    comments: Option<crate::protect::tag_spans::CommentFor>,
    reading: crate::protect::tag_spans::Reading,
    templates: Option<crate::input::span_path::ReaderFor>,
    paragraph_limit: bool,
    syntax: bool,
    xml_names: bool,
}

impl Default for ElementSet {
    fn default() -> Self {
        Self {
            openers: Vec::new(),
            raw_text_tags: Vec::new(),
            case_sensitive: false,
            comments: None,
            reading: crate::protect::tag_spans::Reading::Document,
            templates: None,
            paragraph_limit: false,
            syntax: true,
            xml_names: false,
        }
    }
}

type SharedMdx<'a> = std::rc::Rc<std::cell::RefCell<crate::input::mdx::MdxReader<'a>>>;
#[derive(Clone)]
struct Ending {
    context: crate::protect::tag_spans::ForeignContext,
    scope: crate::protect::tag_spans::Scope,
    body_start: usize,
    stable: bool,
    end: Option<usize>,
}
struct OpenElement {
    at: usize,
    context: crate::protect::tag_spans::ForeignContext,
    scope: crate::protect::tag_spans::Scope,
    depth: usize,
    mark: usize,
    body_start: usize,
}
pub struct ElementReader<'a> {
    elements: &'a ElementSet,
    text: &'a [u16],
    check: CheckBudget<'a>,
    tags: crate::protect::tag_spans::TagSpanReader<'a>,
    last_closes: Vec<Option<isize>>,
    endings: Vec<HashMap<usize, Ending>>,
    templates: Option<Box<dyn crate::input::span_path::SpanReader + 'a>>,
    mdx: Option<SharedMdx<'a>>,
    boundaries: Vec<crate::input::mdx::Boundary>,
    boundary: usize,
}

fn same_context(a: &crate::protect::tag_spans::ForeignContext, b: &crate::protect::tag_spans::ForeignContext) -> bool {
    match (a, b) {
        (Some(a), Some(b)) => Arc::ptr_eq(a, b),
        (None, None) => true,
        _ => false,
    }
}

impl ElementSet {
    pub fn element_reader<'a>(
        &'a self,
        text: &'a [u16],
        ctx: &'a ScanCtx<'a>,
        mdx: Option<SharedMdx<'a>>,
    ) -> Result<ElementReader<'a>> {
        Ok(ElementReader {
            elements: self,
            text,
            check: ctx.check_budget,
            tags: crate::protect::tag_spans::TagSpanReader::new(
                text,
                &self.raw_text_tags,
                self.case_sensitive,
                self.comments.as_ref().map(|f| f(text, ctx.check_budget)),
                self.reading,
                ctx.check_budget,
                self.xml_names,
            ),
            last_closes: vec![None; self.openers.len()],
            endings: vec![HashMap::new(); self.openers.len()],
            templates: self.templates.as_ref().map(|f| f(text, ctx)).transpose()?,
            mdx,
            boundaries: if self.paragraph_limit {
                crate::input::mdx::paragraph_ends(text, ctx.check_budget)?
            } else {
                Vec::new()
            },
            boundary: 0,
        })
    }
}

impl ElementReader<'_> {
    fn opens(&self, k: usize, span: Option<Span>, atom: crate::protect::tag_spans::AtomRead<'_>) -> Result<bool> {
        let Some(span) = span else {
            return Ok(false);
        };
        if span.opens_nothing
            || self.text.get(span.end) != Some(&0x3E)
            || !self.elements.openers[k].heads().0.test(&self.text[span.start..])
        {
            return Ok(false);
        }
        Ok(self.elements.openers[k].slash_opens
            || !crate::protect::tag_spans::self_closing_with(self.text, span.start, span.end, atom, self.check)?)
    }
    fn ordinary(
        &self,
        at: usize,
        token: &crate::protect::tag_spans::TagRead,
        jsx: bool,
    ) -> crate::input::span_path::OwnedSpan {
        let to = match token.span {
            None => at + 1,
            Some(span)
                if !jsx
                    && self.elements.reading == crate::protect::tag_spans::Reading::Fragment
                    && self.text.get(span.end) != Some(&0x3E) =>
            {
                at + 1
            }
            Some(span) => (span.end + 1).min(self.text.len()),
        };
        let end = if self.elements.syntax { to } else { at + 1 };
        let mut span = crate::input::span_path::OwnedSpan::new((at, end, end));
        span.report = Some(jsx);
        if to > at + 1 {
            span.syntax.clone_from(&token.syntax);
        }
        span
    }
    fn fallback(
        &mut self,
        at: usize,
        opening_trial: bool,
        first: &crate::protect::tag_spans::TagRead,
        jsx: bool,
        atom: crate::protect::tag_spans::AtomRead<'_>,
        limit: Option<usize>,
    ) -> Result<crate::input::span_path::OwnedSpan> {
        if !opening_trial {
            return Ok(self.ordinary(at, first, jsx));
        }
        self.tags.finish(false, None)?;
        let token = self.tags.read(at, None, atom, limit)?;
        Ok(self.ordinary(at, &token, jsx))
    }
}

impl crate::input::span_path::SpanReader for ElementReader<'_> {
    fn next(&mut self, from: usize) -> Result<Option<(usize, usize, usize)>> {
        self.tags.next(from)
    }
    fn read(&mut self, at: usize) -> Result<crate::input::span_path::OwnedSpan> {
        use crate::input::span_path::OwnedSpan;
        if !self.elements.syntax && !self.elements.openers.iter().any(|o| o.heads().0.test(&self.text[at..])) {
            return Ok(OwnedSpan::new((at, at + 1, at + 1)));
        }
        if self.mdx.is_some() && crate::shared::strings::is_backslash_escaped(self.text, at) {
            let mut span = OwnedSpan::new((at, at + 1, at + 1));
            span.report = Some(false);
            return Ok(span);
        }
        let jsx = if self.text.get(at + 1).is_some_and(|&c| c == 47 || (c < 128 && (c as u8).is_ascii_alphabetic())) {
            self.mdx.as_ref().map(|m| m.borrow_mut().read(at)).transpose()?
        } else {
            None
        };
        let templates = self.templates.take();
        let mut templates = templates;
        let has_atom = templates.is_some();
        let mut atom = |i| match templates.as_mut() {
            Some(reader) => reader.header(i),
            None => Ok(None),
        };
        let result = (|| {
            while self.boundary < self.boundaries.len() && self.boundaries[self.boundary].end <= at {
                self.boundary += 1;
            }
            let limit = self
                .elements
                .paragraph_limit
                .then(|| self.boundaries.get(self.boundary).map_or(self.text.len(), |b| b.end));
            let opening_trial =
                limit.is_some() && self.elements.openers.iter().any(|o| o.heads().0.test(&self.text[at..]));
            if opening_trial {
                self.tags.begin();
            }
            let first = self.tags.read(
                at,
                jsx.map(|s| s.1 as isize - 1),
                has_atom.then_some(&mut atom),
                if opening_trial { None } else { limit },
            )?;
            let context = self.tags.context();
            let scope = self.tags.scope();
            let depth = self.tags.depth();
            let end = first.span.map_or(at + 1, |s| (s.end + 1).min(self.text.len()));
            let mut selected = None;
            for k in 0..self.elements.openers.len() {
                if self.opens(k, first.span, has_atom.then_some(&mut atom))? {
                    selected = Some(k);
                    break;
                }
            }
            let Some(k) = selected else {
                return self.fallback(at, opening_trial, &first, jsx.is_some(), has_atom.then_some(&mut atom), limit);
            };
            if self.last_closes[k].is_none() {
                let mut last = -1;
                let mut from = 0;
                while let Some(m) = self.elements.openers[k].close_re.exec_at(self.text, from) {
                    (self.check)("safeBlocks.elements")?;
                    last = m.index() as isize;
                    from = m.end();
                }
                self.last_closes[k] = Some(last);
            }
            let last_close = self.last_closes[k].expect("last closer");
            if last_close < end as isize {
                return self.fallback(at, opening_trial, &first, jsx.is_some(), has_atom.then_some(&mut atom), limit);
            }
            if self.endings[k].get(&at).is_some_and(|known| {
                known.end.is_none()
                    && known.body_start == end
                    && known.scope == scope
                    && (same_context(&known.context, &context) || known.stable)
            }) {
                return self.fallback(at, opening_trial, &first, jsx.is_some(), has_atom.then_some(&mut atom), limit);
            }
            let mut stack = vec![OpenElement { at, context, scope, depth, mark: 0, body_start: end }];
            let mut from = first.after;
            let mut through = None;
            let mut read_through = None;
            let mut independent = true;
            self.tags.begin();
            while let Some(candidate) = self.tags.next(from)? {
                let token = self.tags.read(candidate.0, None, None, None)?;
                let inside = self.tags.context();
                let inner_scope = self.tags.scope();
                let inner_depth = self.tags.depth();
                let mark = self.tags.mark();
                from = token.after;
                let Some(span) = token.span.filter(|s| self.text.get(s.end) == Some(&0x3E)) else {
                    continue;
                };
                if self.elements.openers[k].heads().1.test(&self.text[span.start..]) {
                    let open = stack.pop().expect("open element");
                    self.endings[k].insert(
                        open.at,
                        Ending {
                            context: open.context,
                            scope: open.scope,
                            body_start: open.body_start,
                            stable: false,
                            end: Some(span.end + 1),
                        },
                    );
                    if stack.is_empty() {
                        through = Some(span.end + 1);
                        break;
                    }
                } else if span.end as isize >= last_close {
                    read_through = Some(token.mark);
                    break;
                } else if from as isize > last_close {
                    break;
                } else if self.elements.openers[k].nests && self.opens(k, Some(span), None)? {
                    if let Some(nested) = self.endings[k].get(&span.start).filter(|known| {
                        known.end.is_none()
                            && known.body_start == span.end + 1
                            && known.scope == inner_scope
                            && (same_context(&known.context, &inside) || known.stable)
                    }) {
                        independent = nested.stable;
                        break;
                    }
                    stack.push(OpenElement {
                        at: span.start,
                        context: inside,
                        scope: inner_scope,
                        depth: inner_depth,
                        mark,
                        body_start: span.end + 1,
                    });
                }
            }
            let minimums = self.tags.finish(through.is_some(), read_through)?;
            for open in stack {
                let stable =
                    independent && minimums.get(open.mark).copied().unwrap_or(isize::MIN) >= open.depth as isize - 1;
                self.endings[k].insert(
                    open.at,
                    Ending { context: open.context, scope: open.scope, body_start: open.body_start, stable, end: None },
                );
            }
            let Some(through) = through else {
                return self.fallback(at, opening_trial, &first, jsx.is_some(), has_atom.then_some(&mut atom), limit);
            };
            if opening_trial {
                self.tags.finish(true, None)?;
            }
            let mut span = OwnedSpan::new((at, through, through));
            span.syntax = first.syntax;
            Ok(span)
        })();
        self.templates = templates;
        result
    }
}

enum Block {
    Delims(Re),
    Tag(usize),
    Scanner(Scanner, bool),
    OpaqueScanner(OpaqueScanner),
    InlineScanner(OpaqueScanner),
    DocumentScanner(DocumentScanner),
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
    ignored_file: bool,
    indents: Option<IndentScan>,
    label_addresses: HashSet<JsString>,
    line_blocks: HashSet<JsString>,
    line_block_contents: HashMap<Arc<[u16]>, usize>,
    table_delimiters: HashSet<JsString>,
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
            ignored_file: false,
            indents: None,
            label_addresses: HashSet::new(),
            line_blocks: HashSet::new(),
            line_block_contents: HashMap::new(),
            table_delimiters: HashSet::new(),
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
    pub typeset: bool,
}

impl Default for TagOptions {
    fn default() -> Self {
        TagOptions { renders: true, nests: true, form_feed: false, slash_opens: false, typeset: false }
    }
}

impl SafeBlocks {
    pub fn configure_reading(
        &mut self,
        comments: Option<crate::protect::tag_spans::CommentFor>,
        reading: crate::protect::tag_spans::Reading,
        templates: Option<crate::input::span_path::ReaderFor>,
        paragraph_limit: bool,
        syntax: bool,
        xml_names: bool,
    ) {
        self.elements.xml_names = xml_names;
        self.elements.comments = comments;
        self.elements.reading = reading;
        self.elements.templates = templates;
        self.elements.paragraph_limit = paragraph_limit;
        self.elements.syntax = syntax;
    }
    pub fn set_case_sensitive(&mut self, value: bool) {
        self.elements.case_sensitive = value;
    }
    pub fn protects_tag(&self, name: &str) -> bool {
        self.blocks.iter().any(|(id, block)| matches!(block, Block::Tag(_)) && id.eq_ignore_ascii_case(name))
    }
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

    pub fn reset(&mut self) {
        self.ignored_file = false;
        self.vault.reset();
        self.label_addresses.clear();
        self.line_blocks.clear();
        self.line_block_contents.clear();
        self.table_delimiters.clear();
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
        let flags = if self.elements.case_sensitive { "g" } else { "gi" };
        let open_re = Re::configured(&format!(r"<{tag}(?=[ \t\n\r\f/>])"), flags);
        let ff = if options.form_feed { r"\f" } else { "" };
        let close_re = Re::configured(
            &if options.form_feed {
                format!(r"<\/{tag}(?=[ \t\r\n\f/>])")
            } else {
                format!(r"<\/{tag}[ \t\r\n{ff}]*>")
            },
            flags,
        );
        if !options.nests {
            self.elements.raw_text_tags.push(tag.to_ascii_lowercase());
        }
        self.elements.openers.push(Opener {
            open_re,
            close_re,
            heads: OnceLock::new(),
            nests: options.nests,
            renders: options.renders,
            slash_opens: options.slash_opens,
            typeset: options.typeset,
        });
        self.blocks.push((tag.to_owned(), Block::Tag(self.elements.openers.len() - 1)));
        Ok(())
    }

    fn protect_raw_text(
        &mut self,
        text: &[u16],
        k: usize,
        check_budget: CheckBudget<'_>,
        mut typeset_body: Option<&mut BodyTypesetter<'_>>,
    ) -> Result<JsString> {
        if !text.contains(&0x3C) {
            return Ok(text.to_vec());
        }
        let opener = self.elements.openers[k].clone();
        if !opener.typeset {
            typeset_body = None;
        }
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
            let idx = match &mut index {
                Some(idx) => idx,
                None => index.insert(tag_index(text, &self.elements)?),
            };
            let Some(start) = body_start(text, idx, open.index(), opener.slash_opens) else { continue };
            let idx = &*idx;
            let Some((close_at, close_end)) =
                closer_from(text, &opener.close_re, start, &|at| closing_end(text, idx, at))
            else {
                self.unclosed = true;
                break;
            };
            parts.push(text[cursor..start].to_vec());
            let body = &text[start..close_at];
            let stored = match typeset_body.as_mut() {
                Some(typeset) => {
                    let typed = typeset(body, self)?;
                    self.store(&typed, target)?
                }
                None => self.store(body, target)?,
            };
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
        let index = tag_index(text, &self.elements)?;
        let mut kept: Vec<(usize, Option<usize>, bool)> = events
            .into_iter()
            .filter_map(|(at, _, open)| {
                if !open {
                    return closing_end(text, &index, at).is_some().then_some((at, None, false));
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
        self.blocks.push((id.to_owned(), Block::Scanner(scan, false)));
    }

    pub fn add_line_block_scanner(&mut self, id: &str, scan: Scanner) {
        self.blocks.push((id.to_owned(), Block::Scanner(scan, true)));
    }

    pub fn add_opaque_scanner(&mut self, id: &str, scan: OpaqueScanner) {
        self.blocks.push((id.to_owned(), Block::OpaqueScanner(scan)));
    }

    pub fn add_inline_scanner(&mut self, id: &str, scan: OpaqueScanner) {
        self.blocks.push((id.to_owned(), Block::InlineScanner(scan)));
    }

    pub fn label_address_before(&self, text: &[u16], end: usize) -> bool {
        if self.label_addresses.is_empty() {
            return false;
        }
        let tail = &text[end.saturating_sub(12)..end];
        tail.iter().rposition(|&c| c == OPEN_UNIT).is_some_and(|start| self.label_addresses.contains(&tail[start..]))
    }

    pub fn line_block_at(&self, text: &[u16], at: usize, after_break: bool) -> bool {
        if after_break && !self.table_delimiters.is_empty() {
            let next = js::index_of_unit(text, 0x0A, at).map_or(0, |n| n + 1);
            if next > 0 && Self::marked_at(&self.table_delimiters, text, next) {
                return true;
            }
        }
        Self::marked_at(&self.line_blocks, text, at)
    }

    fn marked_at(tokens: &HashSet<JsString>, text: &[u16], at: usize) -> bool {
        text.get(at) == Some(&OPEN_UNIT) && !tokens.is_empty() && tokens.contains(Self::token_at(text, at))
    }

    fn token_at(text: &[u16], at: usize) -> &[u16] {
        let mut length = 1;
        let mut i = at + 1;
        while i < text.len() && length < 12 {
            length += 1;
            if text[i] == CLOSE_UNIT {
                return &text[at..=i];
            }
            i += 1;
        }
        &[]
    }

    pub fn horizontal_space_end(&self, text: &[u16], at: usize) -> usize {
        let kind = text.get(at + 1).copied();
        if kind != Some(u16::from(b'N')) && kind != Some(u16::from(b'W')) {
            return at;
        }
        let token = Self::token_at(text, at);
        if token.is_empty() {
            return at;
        }
        let vault = if kind == Some(u16::from(b'N')) { &self.hidden_vault } else { &self.indent_vault };
        let raw = js_index(&token[2..token.len() - 1]).and_then(|id| vault.retrieve(id));
        match raw {
            Some(raw) if !raw.is_empty() && raw.iter().all(|&c| c == 0x20 || c == 0x09) => at + token.len(),
            _ => at,
        }
    }

    fn vault_kinded(
        &mut self,
        text: &[u16],
        spans: &[Spanned],
        line_block: bool,
        mut cuts: Option<&mut Vec<usize>>,
    ) -> Result<JsString> {
        if spans.is_empty() {
            return Ok(text.to_vec());
        }
        let mut output = Vec::with_capacity(text.len());
        let mut cursor = 0;
        let mut shift = 0isize;
        for &(start, end, kind) in spans {
            output.extend_from_slice(&text[cursor..start]);
            let body = &text[start..end];
            let token = if kind == SpanKind::Tail {
                if shows_nothing(body) { self.hidden_tail_vault.store(body)? } else { self.tail_vault.store(body)? }
            } else if line_block || kind == SpanKind::LineBlock {
                let vault = if shows_nothing(body) { &mut self.hidden_vault } else { &mut self.vault };
                let token = vault.store_in(body, &mut self.line_block_contents)?;
                self.line_blocks.insert(token.clone());
                token
            } else {
                self.store(body, Target::Visible)?
            };
            match kind {
                SpanKind::TableDelimiter => {
                    self.table_delimiters.insert(token.clone());
                }
                SpanKind::LabelAddress => {
                    self.label_addresses.insert(token.clone());
                }
                _ => {}
            }
            if let Some(cuts) = cuts.as_deref_mut() {
                let at = start.cast_signed() + shift;
                if let SpanKind::InlineCode(width) = kind {
                    cuts.push((at - width.cast_signed()).cast_unsigned());
                    cuts.push((at + (token.len() + width).cast_signed()).cast_unsigned());
                }
                shift += token.len().cast_signed() - (end - start).cast_signed();
            }
            output.extend(token);
            cursor = end;
        }
        output.extend_from_slice(&text[cursor..]);
        Ok(output)
    }

    pub fn add_document_scanner(&mut self, id: &str, scan: DocumentScanner) {
        self.blocks.push((id.to_owned(), Block::DocumentScanner(scan)));
    }

    pub fn elements(&self) -> &ElementSet {
        &self.elements
    }

    pub fn ignored_file(&self) -> bool {
        self.ignored_file
    }

    fn vault_document_spans(&mut self, text: &[u16], spans: &[(usize, usize, bool)]) -> Result<JsString> {
        let spans: Vec<Spanned> = spans
            .iter()
            .map(|&(start, end, tail)| (start, end, if tail { SpanKind::Tail } else { SpanKind::Plain }))
            .collect();
        self.vault_kinded(text, &spans, false, None)
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

    pub fn source_view(&self, text: &[u16]) -> JsString {
        INDENT_RE.replace_with(text, |c| cat!("\n", self.indent_vault.restore_all(c.s(1))))
    }

    pub fn vault_trimmed(
        &mut self,
        text: &[u16],
        view: &[u16],
        result: JsString,
        check_budget: CheckBudget<'_>,
    ) -> Result<JsString> {
        if result == view {
            return Ok(text.to_vec());
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

    pub fn set_tail(&mut self, opaque: Re) {
        self.tail = Some(opaque);
    }

    pub fn set_indents(&mut self, scan: IndentScan) {
        self.indents = Some(scan);
    }

    pub fn protect(
        &mut self,
        text: &[u16],
        check_budget: CheckBudget<'_>,
        resolve: Resolve<'_>,
        document: bool,
        mut after_inline: Option<AfterInline<'_>>,
        mut typeset_body: Option<TypesetBody<'_>>,
    ) -> Result<JsString> {
        let mut result = text.to_vec();
        self.unclosed = false;
        self.ignored_file = false;
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
                            self.protect_raw_text(&result, *k, check_budget, typeset_body.as_deref_mut())?
                        }
                    }
                    Block::Scanner(scan, line_block) => {
                        let ctx = ScanCtx { check_budget, resolve, elements: &self.elements, code_edges: false };
                        let spans: Vec<Spanned> =
                            scan(&result, &ctx)?.into_iter().map(|(a, b)| (a, b, SpanKind::Plain)).collect();
                        self.vault_kinded(&result, &spans, *line_block, None)?
                    }
                    Block::DocumentScanner(_) if !document => continue,
                    Block::OpaqueScanner(scan) => {
                        let ctx = ScanCtx { check_budget, resolve, elements: &self.elements, code_edges: false };
                        let spans = scan(&result, &ctx)?;
                        self.vault_kinded(&result, &spans, false, None)?
                    }
                    Block::InlineScanner(scan) => {
                        let code_edges = after_inline.is_some();
                        let ctx = ScanCtx { check_budget, resolve, elements: &self.elements, code_edges };
                        let spans = scan(&result, &ctx)?;
                        let mut cuts = Vec::new();
                        let hidden = self.vault_kinded(&result, &spans, false, code_edges.then_some(&mut cuts))?;
                        match after_inline.as_deref_mut() {
                            Some(after) => after(hidden, &cuts, &self.elements)?,
                            None => hidden,
                        }
                    }
                    Block::DocumentScanner(scan) => {
                        let scanned = {
                            let ctx = ScanCtx { check_budget, resolve, elements: &self.elements, code_edges: false };
                            let restore = |value: &[u16]| self.restore(value.to_vec());
                            scan(&result, &ctx, &restore)?
                        };
                        self.ignored_file |= scanned.ignore_file;
                        self.vault_document_spans(&result, &scanned.spans)?
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
        let tags = crate::protect::tag_spans::tag_spans_limited(
            text,
            &self.elements.raw_text_tags,
            self.elements.case_sensitive,
            None,
            crate::protect::tag_spans::Reading::Document,
            &[],
            &|_| Ok(()),
            None,
            self.elements.xml_names,
        )?;
        let unended = tags.last().filter(|s| s.end == text.len()).map(|s| s.start);
        if !self.unclosed && !open_comment && unended.is_none() {
            return Ok(text.to_vec());
        }
        let index: TagIndex = tags.iter().map(|s| (s.start, *s)).collect();
        let opaque: Vec<Range> =
            self.tail.as_ref().expect("tail set").find_all(text).iter().map(|m| (m.index(), m.end())).collect();
        let closes = |at: usize| if within(&opaque, at) { None } else { closing_end(text, &index, at) };
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

    pub fn lift_indents(text: &[u16]) -> JsString {
        INDENT_RE.replace_str(text, "\n$1\n")
    }

    pub fn settle_indents(&self, text: &[u16]) -> JsString {
        LIFTED_INDENT_RE.replace_with(text, |c| cat!("\n", self.indent_vault.restore_all(c.s(1))))
    }

    pub fn restore(&self, text: JsString) -> JsString {
        let vaults = [&self.tail_vault, &self.hidden_tail_vault, &self.vault, &self.hidden_vault, &self.indent_vault];
        Vault::restore_fixpoint(&vaults, text)
    }

    pub fn restore_with(&self, text: JsString, mapping: &mut crate::shared::provenance::RestoreMap) -> JsString {
        let vaults = [&self.tail_vault, &self.hidden_tail_vault, &self.vault, &self.hidden_vault, &self.indent_vault];
        Vault::restore_fixpoint_with(&vaults, text, Some(mapping))
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
