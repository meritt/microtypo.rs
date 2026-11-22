use std::collections::{HashMap, HashSet};
use std::sync::{Arc, LazyLock};

use serde_json::json;

use crate::errors::{Error, Result};
use crate::input::frontmatter::scan_leading_header;
use crate::input::mdx::{EsmReader, MdxReader, NATIVE_BLOCK_AT};
use crate::input::span_path::{Next, ReaderFor, SpanReader, collect_spans, precedes_inline};
use crate::js::{self, JsString, Re};
use crate::protect::blocks::{CheckBudget, Range, Resolve, SafeBlocks, ScanCtx, SpanKind, Spanned};
use crate::protect::placeholders::{CLOSE_UNIT, OPEN_UNIT};
use crate::protect::sequences::LINKABLE_SOURCE;
use crate::protect::tag_spans::{COMMENT_CLOSE, COMMENT_OPEN, Span, empty_comment_close, inside_tag, named_at};
use crate::shared::glyphs::{MDASH, NBSP, NDASH, NNBSP, unit};
use crate::shared::strings::{is_flow_space, line_view, skip_spaces};

const MAX_LINK_SCAN: usize = 8192;

const TAB_STOP: usize = 4;

const TAB: u16 = 0x09;
const LF: u16 = 0x0A;
const CR: u16 = 0x0D;
const SP: u16 = 0x20;
const BANG: u16 = 0x21;
const DQ: u16 = 0x22;
const HASH: u16 = 0x23;
const SQ: u16 = 0x27;
const LPAREN: u16 = 0x28;
const RPAREN: u16 = 0x29;
const STAR: u16 = 0x2A;
const PLUS: u16 = 0x2B;
const DASH: u16 = 0x2D;
const DOT: u16 = 0x2E;
const SLASH: u16 = 0x2F;
const COLON: u16 = 0x3A;
const LT: u16 = 0x3C;
const EQ: u16 = 0x3D;
const GT: u16 = 0x3E;
const QUESTION: u16 = 0x3F;
const LBRACKET: u16 = 0x5B;
const BACKSLASH: u16 = 0x5C;
const RBRACKET: u16 = 0x5D;
const CARET: u16 = 0x5E;
const UNDERSCORE: u16 = 0x5F;
const BACKTICK: u16 = 0x60;
const TILDE: u16 = 0x7E;

pub type ReadOpaque<'a> = Option<&'a dyn Fn() -> Result<Vec<Range>>>;

pub type ScanInline<'a> = &'a dyn Fn(&[u16]) -> Result<Vec<Range>>;

fn is(text: &[u16], i: usize, c: u16) -> bool {
    text.get(i) == Some(&c)
}

fn is_indent(text: &[u16], i: usize) -> bool {
    is(text, i, SP) || is(text, i, TAB)
}

fn lower(s: &[u16]) -> String {
    js::lossy(&js::to_lower(s))
}

fn advance(text: &[u16], at: usize, column: usize) -> usize {
    if is(text, at, TAB) { column + TAB_STOP - (column % TAB_STOP) } else { column + 1 }
}

fn skip_indent(text: &[u16], i: usize, end: usize, column: usize) -> (usize, usize) {
    let mut at = i;
    let mut col = column;
    while at < end && is_indent(text, at) {
        col = advance(text, at, col);
        at += 1;
    }
    (at, col)
}

fn skip_block_indent(text: &[u16], i: usize, end: usize, column: usize, base: usize) -> (usize, usize) {
    let mut at = i;
    let mut col = column;
    while at < end && is_indent(text, at) {
        let next = advance(text, at, col);
        if next > base + 3 {
            break;
        }
        col = next;
        at += 1;
    }
    (at, col)
}

fn index_at_column(text: &[u16], i: usize, end: usize, column: usize, target: usize) -> (usize, usize) {
    let mut at = i;
    let mut col = column;
    while at < end && col < target && is_indent(text, at) {
        col = advance(text, at, col);
        at += 1;
    }
    (at, col)
}

fn after_quote_marker(text: &[u16], mark: usize, mark_column: usize) -> (usize, usize) {
    let next = text.get(mark + 1).copied();
    if next == Some(SP) {
        return (mark + 2, mark_column + 2);
    }
    if next != Some(TAB) {
        return (mark + 1, mark_column + 1);
    }
    let stop = advance(text, mark + 1, mark_column + 1);
    if stop <= mark_column + 2 { (mark + 2, stop) } else { (mark + 1, mark_column + 2) }
}

fn content_end(text: &[u16], start: usize, stop: usize) -> usize {
    if stop > start && is(text, stop - 1, CR) { stop - 1 } else { stop }
}

struct EachLine<'a> {
    check_budget: CheckBudget<'a>,
    label: &'static str,
    i: usize,
    lines: usize,
}

impl<'a> EachLine<'a> {
    fn new(check_budget: CheckBudget<'a>, label: &'static str) -> Self {
        EachLine { check_budget, label, i: 0, lines: 0 }
    }

    fn next(&mut self, text: &[u16]) -> Result<Option<Range>> {
        let n = text.len();
        if self.i >= n {
            return Ok(None);
        }
        if self.lines & 0x3ff == 0 {
            (self.check_budget)(self.label)?;
        }
        self.lines += 1;
        let line_end = js::index_of_unit(text, LF, self.i);
        let item = (self.i, content_end(text, self.i, line_end.unwrap_or(n)));
        self.i = line_end.map_or(n, |e| e + 1);
        Ok(Some(item))
    }
}

pub struct LineTicker<'a> {
    check_budget: CheckBudget<'a>,
    label: &'static str,
    lines: usize,
}

impl<'a> LineTicker<'a> {
    pub fn new(check_budget: CheckBudget<'a>, label: &'static str) -> Self {
        LineTicker { check_budget, label, lines: 0 }
    }

    pub fn tick(&mut self) -> Result<()> {
        let due = self.lines & 0x3ff == 0;
        self.lines += 1;
        if due { (self.check_budget)(self.label) } else { Ok(()) }
    }
}

pub type SharedMdx<'a> = std::rc::Rc<std::cell::RefCell<MdxReader<'a>>>;
pub type OwnerFor = Arc<
    dyn for<'a> Fn(
            &'a [u16],
            &'a ScanCtx<'a>,
            Option<SharedMdx<'a>>,
        ) -> Result<Box<dyn crate::input::span_path::SpanReader + 'a>>
        + Send
        + Sync,
>;
struct MdxOwner<'a>(SharedMdx<'a>);
impl crate::input::span_path::SpanReader for MdxOwner<'_> {
    fn next(&mut self, from: usize) -> Result<Option<(usize, usize, usize)>> {
        self.0.borrow_mut().next(from)
    }
    fn read(&mut self, at: usize) -> Result<crate::input::span_path::OwnedSpan> {
        let span = self.0.borrow_mut().read(at)?;
        Ok(crate::input::span_path::OwnedSpan::new((span.0, span.1, span.2)))
    }
}
struct Candidate<'a> {
    reader: Box<dyn crate::input::span_path::SpanReader + 'a>,
    at: Option<(usize, usize, usize)>,
    report: bool,
    mdx: bool,
    code: bool,
}

pub fn opaque_inline(
    code: Option<ReaderFor>,
    template: Option<ReaderFor>,
    owned: Vec<OwnerFor>,
    mdx: bool,
    report_tags: bool,
) -> crate::protect::blocks::OpaqueScanner {
    Arc::new(move |text, ctx| {
        let syntax = if mdx {
            Some(std::rc::Rc::new(std::cell::RefCell::new(MdxReader::new(text, ctx.check_budget)?)))
        } else {
            None
        };
        let mut readers = Vec::new();
        for (k, factory) in code.iter().map(|f| (true, f)).chain(template.iter().map(|f| (false, f))) {
            let mut reader = factory(text, ctx)?;
            let at = reader.next(0)?;
            readers.push(Candidate { reader, at, report: true, mdx: false, code: k });
        }
        if !mdx && readers.iter().all(|r| r.at.is_none()) {
            return Ok(Vec::new());
        }
        for factory in &owned {
            let mut reader = factory(text, ctx, syntax.clone())?;
            let at = reader.next(0)?;
            readers.push(Candidate { reader, at, report: false, mdx: false, code: false });
        }
        if let Some(syntax) = syntax {
            let mut reader = MdxOwner(syntax);
            let at = reader.next(0)?;
            readers.push(Candidate { reader: Box::new(reader), at, report: true, mdx: true, code: false });
        }
        let mut spans = Vec::new();
        let mut count = 0;
        while readers.iter().any(|r| r.report && r.at.is_some()) {
            if count & 1023 == 0 {
                (ctx.check_budget)(if mdx { "mdx-inline" } else { "md-inline-opaque" })?;
            }
            count += 1;
            let mut first: Option<(usize, (usize, usize, usize))> = None;
            let mut first_address = false;
            for (k, reader) in readers.iter().enumerate() {
                if let Some(at) = reader.at
                    && precedes_inline(at, first.map(|(_, f)| f), first_address)
                {
                    first = Some((k, at));
                    first_address = reader.reader.address();
                }
            }
            let (k, at) = first.expect("reporting construct");
            let span = readers[k].reader.read(at.0)?;
            let markup = span.report == Some(true) || (readers[k].mdx && text[span.start] == LT);
            if !markup || !report_tags {
                for (a, b) in span.syntax {
                    spans.push((a, b, SpanKind::Plain));
                }
            }
            let report = if mdx {
                (readers[k].report && span.report != Some(false)) || span.report == Some(true)
            } else {
                readers[k].report && span.report != Some(false)
            };
            if report && (report_tags || !markup) {
                let kind = if readers[k].code && ctx.code_edges && span.after > span.end {
                    SpanKind::InlineCode(span.after - span.end)
                } else if span.tail {
                    SpanKind::Tail
                } else if mdx && markup && text[span.start] == LT && NATIVE_BLOCK_AT.exec_at(text, span.start).is_some()
                {
                    SpanKind::LineBlock
                } else {
                    SpanKind::Plain
                };
                spans.push((span.start, span.end, kind));
            }
            for reader in &mut readers {
                if reader.at.is_some_and(|a| a.0 < span.after) {
                    reader.at = reader.reader.next(span.after)?;
                }
            }
        }
        Ok(spans)
    })
}

fn fence_open_at(text: &[u16], pos: usize, line_end: usize) -> Option<(u16, usize)> {
    let c = text.get(pos).copied()?;
    if c != BACKTICK && c != TILDE {
        return None;
    }
    let mut j = pos;
    while is(text, j, c) {
        j += 1;
    }
    let len = j - pos;
    if len < 3 {
        return None;
    }
    if c == BACKTICK && js::slice(text, j, line_end).contains(&BACKTICK) {
        return None;
    }
    Some((c, len))
}

fn fence_close_at(
    text: &[u16],
    pos: usize,
    line_end: usize,
    column: usize,
    content: usize,
    c: u16,
    min_len: usize,
) -> Option<usize> {
    let (i, _) = skip_block_indent(text, pos, line_end, column, content);
    let mut j = i;
    while j < line_end && is(text, j, c) {
        j += 1;
    }
    if j - i < min_len {
        return None;
    }
    let after = skip_spaces(text, j, line_end);
    (after == line_end).then_some(after)
}

#[cfg(test)]
pub fn scan_fences(source: &[u16], check_budget: CheckBudget<'_>, read_opaque: ReadOpaque<'_>) -> Result<Vec<Range>> {
    scan_fences_options(source, check_budget, read_opaque, false, None)
}

type ReadLine<'a> = Option<&'a mut dyn FnMut(usize, usize) -> Result<usize>>;

fn scan_fences_options(
    source: &[u16],
    check_budget: CheckBudget<'_>,
    read_opaque: ReadOpaque<'_>,
    mdx: bool,
    mut inline: ReadLine<'_>,
) -> Result<Vec<Range>> {
    let text = &line_view(source)[..];
    let n = text.len();
    let mut spans = Vec::new();
    let mut opens_html = HtmlBlockOpener::new_options(read_opaque, mdx, check_budget);
    let mut esm = if mdx { Some(EsmReader::new(text, check_budget)?) } else { None };
    let mut inline_through = 0;
    let mut ends = Ends::default();
    let mut tick = LineTicker::new(check_budget, "md-fence");
    let mut i = 0;
    let mut chain: Vec<Container> = Vec::new();
    let mut paragraph_open = false;

    while i < n {
        tick.tick()?;
        let nl = js::index_of_unit(text, LF, i);
        let line_end = content_end(text, i, nl.unwrap_or(n));
        if inline_through > i {
            inline_through = inline.as_mut().expect("inline reader")(i, line_end)?;
            i = nl.map_or(n, |p| p + 1);
            continue;
        }
        let before = paragraph_open;
        let walk = walk_line(text, i, line_end, &mut chain, before);
        if walk.place.content == i
            && let Some(module) = esm.as_mut().and_then(|r| r.read(text, i))
        {
            spans.push((i, module.end));
            i = module.next;
            paragraph_open = false;
            chain.clear();
            continue;
        }
        let open = fence_open_at(text, walk.place.content, line_end);
        if open.is_none() && js::starts_with_at(text, &[COLON, COLON, COLON], walk.place.content) {
            spans.push((i, line_end));
            paragraph_open = false;
            i = nl.map_or(n, |p| p + 1);
            continue;
        }
        let Some((fence, fence_len)) = open else {
            let html = opens_html.call(text, walk.place.content, !before || walk.fresh)?;
            paragraph_open = html.is_none() && walk.paragraph_open;
            i = nl.map_or(n, |p| p + 1);
            if let Some(m) = html {
                i = html_block_end(text, walk.place.content, &m, i, nl, line_end, &chain, &mut ends, &mut tick)?.next;
            } else if let Some(inline) = inline.as_mut() {
                inline_through = inline(walk.place.content, line_end)?;
            }
            continue;
        };

        paragraph_open = false;
        let end = fence_end(text, (fence, fence_len), &chain, nl, &mut tick)?;
        spans.push((i, end));
        i = end;
    }

    Ok(spans)
}

fn fence_end(
    text: &[u16],
    (fence, fence_len): (u16, usize),
    containers: &[Container],
    nl: Option<usize>,
    tick: &mut LineTicker<'_>,
) -> Result<usize> {
    let n = text.len();
    let mut cursor = nl.map_or(n, |p| p + 1);
    let mut inside = nl.unwrap_or(n);

    while cursor < n {
        tick.tick()?;
        let brk = js::index_of_unit(text, LF, cursor);
        let stop = brk.unwrap_or(n);
        let line_stop = content_end(text, cursor, stop);
        let line = match_containers(text, cursor, line_stop, containers);
        if line.matched < containers.len() {
            return Ok(inside);
        }
        let blank = skip_spaces(text, line.at, line_stop) == line_stop;
        let close_end = if blank {
            None
        } else {
            fence_close_at(text, line.at, line_stop, line.column, line.content, fence, fence_len)
        };
        if let Some(close_end) = close_end {
            return Ok(close_end);
        }
        if !blank {
            inside = stop;
        }
        cursor = brk.map_or(n, |b| b + 1);
    }

    Ok(n)
}

#[derive(Clone, Copy)]
struct Run {
    start: usize,
    len: usize,
    open_len: usize,
    close: Option<usize>,
}

fn backtick_runs(text: &[u16]) -> Vec<Run> {
    let n = text.len();
    let mut runs: Vec<Run> = Vec::new();
    let mut next_of_length: HashMap<usize, usize> = HashMap::new();
    let mut i = 0;
    let mut paragraphs: Vec<usize> = Vec::new();

    while i < n {
        if text[i] == LF {
            let after = skip_spaces(text, i + 1, n);
            if after >= n || is(text, after, LF) || (is(text, after, CR) && is(text, after + 1, LF)) {
                paragraphs.push(runs.len());
            }
            i += 1;
            continue;
        }
        if text[i] != BACKTICK {
            i += 1;
            continue;
        }
        let mut slashes = 0;
        while i > slashes && is(text, i - slashes - 1, BACKSLASH) {
            slashes += 1;
        }
        let start = i;
        while i < n && text[i] == BACKTICK {
            i += 1;
        }
        let len = i - start;
        runs.push(Run { start, len, open_len: if slashes % 2 == 1 { len - 1 } else { len }, close: None });
    }

    let mut boundary = paragraphs.len() as isize - 1;
    for r in (0..runs.len()).rev() {
        while boundary >= 0 && paragraphs[boundary as usize] > r {
            next_of_length.clear();
            boundary -= 1;
        }
        runs[r].close = next_of_length.get(&runs[r].open_len).copied();
        next_of_length.insert(runs[r].len, r);
    }

    runs
}

pub fn code_span_reader<'a>(text: &[u16], check_budget: CheckBudget<'_>) -> Result<Next<'a>> {
    if !text.contains(&BACKTICK) {
        return Ok(Box::new(|_| Ok(None)));
    }
    check_budget("md-code-span")?;
    let runs = backtick_runs(&line_view(text));
    let mut i = 0;
    Ok(Box::new(move |from| {
        while i < runs.len() && (runs[i].start < from || runs[i].open_len == 0 || runs[i].close.is_none()) {
            i += 1;
        }
        if i == runs.len() {
            return Ok(None);
        }
        let Run { start, len, close, .. } = runs[i];
        let closer = runs[close.expect("filtered above")];
        Ok(Some((start + len, closer.start, closer.start + closer.len)))
    }))
}

const DNS_LABEL: &str = "[A-Za-z0-9](?:[A-Za-z0-9-]{0,61}[A-Za-z0-9])?";
const EMAIL_LOCAL: &str = "[A-Za-z0-9.!#$%&'*+/=?^_`{|}~-]+";
const URI: &str = r"[A-Za-z][A-Za-z0-9+.-]{1,31}:[^\x00-\x20<>]*";
static AUTOLINK: LazyLock<Re> =
    LazyLock::new(|| Re::new(&format!(r"<(?:{URI}|{EMAIL_LOCAL}@{DNS_LABEL}(?:\.{DNS_LABEL})*)>"), "g"));
static MAILTO_RE: LazyLock<Re> = LazyLock::new(|| Re::new("^mailto:", "iu"));

struct Matches<'t> {
    re: &'static Re,
    text: &'t [u16],
    last: usize,
}

impl<'t> Matches<'t> {
    fn new(re: &'static Re, text: &'t [u16]) -> Self {
        Matches { re, text, last: 0 }
    }

    fn next(&mut self) -> Option<crate::js::Caps<'t>> {
        if self.last > self.text.len() {
            return None;
        }
        let found = self.re.exec_at(self.text, self.last)?;
        self.last = if found.end() == found.index() { self.re.advance(self.text, found.end()) } else { found.end() };
        Some(found)
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum CodeItem {
    Code,
    Autolink,
    Address,
}

struct DocumentCodeReader<'a> {
    text: &'a [u16],
    check_budget: CheckBudget<'a>,
    code: Next<'a>,
    autolinks: Matches<'a>,
    autolink: Option<(usize, usize)>,
    autolink_count: usize,
    emails: Matches<'a>,
    urls: Matches<'a>,
    url: Option<Option<(usize, usize, bool)>>,
    address_count: usize,
    address: Option<Option<(usize, usize)>>,
    current: Option<(usize, usize, usize, CodeItem)>,
}

impl DocumentCodeReader<'_> {
    fn next_autolink(&mut self) -> Result<Option<(usize, usize)>> {
        while let Some(found) = self.autolinks.next() {
            if self.autolink_count & 1023 == 0 {
                (self.check_budget)("md-address")?;
            }
            self.autolink_count += 1;
            if found.whole().contains(&BACKTICK)
                && !crate::shared::strings::is_backslash_escaped(self.text, found.index())
            {
                return Ok(Some((found.index(), found.end())));
            }
        }
        Ok(None)
    }

    fn next_url(&mut self) -> Option<(usize, usize, bool)> {
        self.urls
            .next()
            .map(|url| (url.index(), url.end(), MAILTO_RE.test(url.whole()) && url.whole().contains(&BACKTICK)))
    }

    fn next_address(&mut self, from: usize) -> Result<Option<(usize, usize)>> {
        while let Some(email) = self.emails.next() {
            if self.address_count & 1023 == 0 {
                (self.check_budget)("md-address")?;
            }
            self.address_count += 1;
            let Some(tick) = email.whole().iter().position(|&c| c == BACKTICK) else { continue };
            let mut start = email.index();
            let mut end = email.end();
            if end <= from {
                continue;
            }
            if self.url.is_none() {
                self.url = Some(self.next_url());
            }
            while let Some(Some(url)) = self.url
                && url.1 <= start
            {
                self.url = Some(self.next_url());
            }
            if let Some(Some(url)) = self.url
                && url.0 <= start
            {
                if !url.2 {
                    continue;
                }
                start = url.0;
                end = url.1;
            } else if tick == 0 {
                continue;
            }
            if start >= from {
                return Ok(Some((start, end)));
            }
        }
        Ok(None)
    }
}

impl SpanReader for DocumentCodeReader<'_> {
    fn next(&mut self, from: usize) -> Result<Option<(usize, usize, usize)>> {
        let span = (self.code)(from)?;
        while let Some(autolink) = self.autolink
            && autolink.0 < from
        {
            self.autolink = self.next_autolink()?;
        }
        let opener = span.map_or(usize::MAX, |span| span.0 - (span.2 - span.1));
        self.current = if let Some(autolink) = self.autolink
            && autolink.0 < opener
        {
            Some((autolink.0, autolink.1, autolink.1, CodeItem::Autolink))
        } else if let Some(span) = span {
            while match self.address {
                None => true,
                Some(Some(address)) => address.1 <= opener || address.0 < from,
                Some(None) => false,
            } {
                self.address = Some(self.next_address(from)?);
            }
            match self.address {
                Some(Some(address)) if address.0 < opener && span.1 != address.1 => {
                    Some((address.0, address.1, address.1, CodeItem::Address))
                }
                _ => Some((span.0, span.1, span.2, CodeItem::Code)),
            }
        } else {
            None
        };
        Ok(self.current.map(|(start, end, after, _)| (start, end, after)))
    }

    fn read(&mut self, at: usize) -> Result<crate::input::span_path::OwnedSpan> {
        let (start, end, after, item) = self.current.filter(|current| current.0 == at).expect("selected candidate");
        let mut span = crate::input::span_path::OwnedSpan::new((start, end, after));
        if item == CodeItem::Address {
            span.report = Some(false);
        }
        Ok(span)
    }

    fn address(&self) -> bool {
        self.current.is_some_and(|current| current.3 == CodeItem::Address)
    }
}

pub fn document_code_span_reader<'a>(
    text: &'a [u16],
    check_budget: CheckBudget<'a>,
) -> Result<Box<dyn SpanReader + 'a>> {
    let code = code_span_reader(text, check_budget)?;
    if !text.contains(&BACKTICK) || (!text.contains(&0x40) && !text.contains(&LT)) {
        return Ok(Box::new(crate::input::span_path::NextReader::new(code)));
    }
    let mut reader = DocumentCodeReader {
        text,
        check_budget,
        code,
        autolinks: Matches::new(&AUTOLINK, text),
        autolink: None,
        autolink_count: 0,
        emails: Matches::new(&crate::shared::url_email_regex::EMAIL_REGEX, text),
        urls: Matches::new(&crate::shared::url_email_regex::URL_REGEX, text),
        url: None,
        address_count: 0,
        address: None,
        current: None,
    };
    reader.autolink = reader.next_autolink()?;
    Ok(Box::new(reader))
}

pub fn scan_code_spans(text: &[u16], check_budget: CheckBudget<'_>) -> Result<Vec<Range>> {
    collect_spans(&mut code_span_reader(text, check_budget)?)
}

fn thematic_tail(text: &[u16], i: usize, end: usize) -> Option<(usize, usize)> {
    let mut ch: Option<u16> = None;
    let mut seen = 0;
    let mut third: Option<usize> = None;
    let mut from = end;
    for j in (i..end).rev() {
        let c = text.get(j).copied();
        if c == Some(SP) || c == Some(TAB) {
            from = j;
            continue;
        }
        if !matches!(c, Some(DASH | STAR | UNDERSCORE)) || ch.is_some_and(|x| Some(x) != c) {
            break;
        }
        ch = c;
        seen += 1;
        from = j;
        if seen == 3 {
            third = Some(j);
        }
    }
    third.map(|t| (from, t))
}

fn is_thematic_break(tail: Option<(usize, usize)>, mark: usize) -> bool {
    tail.is_some_and(|(from, third)| mark >= from && mark <= third)
}

fn interrupts_paragraph(text: &[u16], mark: usize, token: usize, end: usize) -> bool {
    if skip_spaces(text, token, end) == end {
        return false;
    }
    if matches!(text.get(mark), Some(&(DASH | STAR | PLUS))) {
        return true;
    }
    let digits = js::slice(text, mark, token - 1);
    let k = digits.iter().position(|&c| c != 0x30).unwrap_or(digits.len());
    digits[k..] == [0x31]
}

fn item_content_column(text: &[u16], token: usize, end: usize, marker_column: usize) -> usize {
    let (at, column) = skip_indent(text, token, end, marker_column);
    if at == end || column - marker_column > TAB_STOP { marker_column + 1 } else { column }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Container {
    quote: bool,
    column: usize,
    first_quote: isize,
}

fn first_quote_in(chain: &[Container], length: usize, quote: bool) -> isize {
    let before = if length == 0 { -1 } else { chain[length - 1].first_quote };
    if before == -1 && quote { length as isize } else { before }
}

struct LineMatch {
    matched: usize,
    at: usize,
    column: usize,
    content: usize,
}

fn match_containers(text: &[u16], start: usize, end: usize, containers: &[Container]) -> LineMatch {
    if skip_spaces(text, start, end) == end {
        let first = containers.last().map_or(-1, |c| c.first_quote);
        let (blank_at, blank_column) = skip_indent(text, start, end, 0);
        return LineMatch {
            matched: if first == -1 { containers.len() } else { first as usize },
            at: blank_at,
            column: blank_column,
            content: blank_column,
        };
    }

    let mut at = start;
    let mut column = 0;
    let mut content = 0;
    let mut matched = 0;

    while matched < containers.len() {
        let container = containers[matched];
        if container.quote {
            let (mark, mark_column) = skip_block_indent(text, at, end, column, content);
            if mark >= end || !is(text, mark, GT) {
                break;
            }
            (at, column) = after_quote_marker(text, mark, mark_column);
            content = column;
            matched += 1;
            continue;
        }
        let target = content + container.column;
        let (next, next_column) = index_at_column(text, at, end, column, target);
        if next_column < target && next < end {
            break;
        }
        at = next;
        column = next_column;
        content = target;
        matched += 1;
    }

    LineMatch { matched, at, column, content }
}

#[derive(Clone, Copy)]
struct Place {
    base: usize,
    base_column: usize,
    content: usize,
    content_column: usize,
    opened: usize,
}

fn write_container(into: &mut Vec<Container>, at: usize, container: Container) {
    if at < into.len() {
        into[at] = container;
    } else {
        into.push(container);
    }
}

fn open_containers(
    text: &[u16],
    at: usize,
    end: usize,
    column: usize,
    into: &mut Vec<Container>,
    from: usize,
    paragraph_open: bool,
) -> Place {
    let mut base = at;
    let mut base_column = column;
    let mut count = from;
    let mut open = paragraph_open;
    let tail = thematic_tail(text, at, end);

    loop {
        let (mark, mark_column) = skip_block_indent(text, base, end, base_column, base_column);

        if mark < end && is(text, mark, GT) {
            let (next, next_column) = after_quote_marker(text, mark, mark_column);
            let first_quote = first_quote_in(into, count, true);
            write_container(into, count, Container { quote: true, column: next_column - base_column, first_quote });
            count += 1;
            base = next;
            base_column = next_column;
            open = false;
            continue;
        }

        let container = mark < end && !is(text, mark, HASH) && !is_thematic_break(tail, mark);
        let token = if container { marker_token_end(text, mark, end) } else { mark };

        if token == mark || (open && !interrupts_paragraph(text, mark, token, end)) {
            return Place { base, base_column, content: mark, content_column: mark_column, opened: count - from };
        }

        let marker_column = mark_column + (token - mark);
        let content_column = item_content_column(text, token, end, marker_column);
        let first_quote = first_quote_in(into, count, false);
        write_container(into, count, Container { quote: false, column: content_column - base_column, first_quote });
        count += 1;
        open = false;
        (base, base_column) = index_at_column(text, token, end, marker_column, content_column);
    }
}

fn placeholder_line_end(text: &[u16], i: usize, end: usize) -> Option<usize> {
    if !is(text, i, OPEN_UNIT) {
        return None;
    }
    let mut j = i + 1;
    while j < end && !is(text, j, CLOSE_UNIT) {
        j += 1;
    }
    (j < end).then_some(j + 1)
}

fn hash_run_end(text: &[u16], i: usize, end: usize) -> usize {
    let mut j = i;
    while j < end && j < i + 6 && is(text, j, HASH) {
        j += 1;
    }
    j
}

const RAW_BLOCK_NAMES: [&str; 4] = ["pre", "script", "style", "textarea"];

static BLOCK_KIND_6_NAMES: LazyLock<HashSet<&'static str>> = LazyLock::new(|| {
    [
        "address",
        "article",
        "aside",
        "base",
        "basefont",
        "blockquote",
        "body",
        "caption",
        "center",
        "col",
        "colgroup",
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
        "frame",
        "frameset",
        "h1",
        "h2",
        "h3",
        "h4",
        "h5",
        "h6",
        "head",
        "header",
        "hr",
        "html",
        "iframe",
        "legend",
        "li",
        "link",
        "main",
        "menu",
        "menuitem",
        "nav",
        "noframes",
        "ol",
        "optgroup",
        "option",
        "p",
        "param",
        "search",
        "section",
        "summary",
        "table",
        "tbody",
        "td",
        "tfoot",
        "th",
        "thead",
        "title",
        "tr",
        "track",
        "ul",
    ]
    .into_iter()
    .collect()
});

fn is_name_char(c: u16) -> bool {
    c < 0x80 && ((c as u8).is_ascii_alphanumeric() || c == DASH)
}

fn opens_html_block(text: &[u16], i: usize, end: usize) -> bool {
    if !is(text, i, LT) {
        return false;
    }
    if is(text, i + 1, BANG) || is(text, i + 1, QUESTION) {
        return true;
    }
    let name_start = if is(text, i + 1, SLASH) { i + 2 } else { i + 1 };
    let mut j = name_start;
    while j < end && text.get(j).is_some_and(|&c| is_name_char(c)) {
        j += 1;
    }
    let name = lower(js::slice(text, name_start, j));
    let raw = name_start == i + 1 && RAW_BLOCK_NAMES.contains(&name.as_str());
    let boundary = j == end || is_flow_space(text.get(j).copied()) || is(text, j, GT);
    j > name_start
        && ((raw && boundary)
            || (BLOCK_KIND_6_NAMES.contains(name.as_str())
                && (boundary || (is(text, j, SLASH) && is(text, j + 1, GT)))))
}

fn closes_block(text: &[u16], start: usize, end: usize, column: usize, paragraph_open: bool) -> bool {
    if fence_open_at(text, start, end).is_some() {
        return true;
    }

    let (i, _) = skip_block_indent(text, start, end, column, column);
    let c = text.get(i).copied();

    if c == Some(HASH) {
        let j = hash_run_end(text, i, end);
        return j == end || is(text, j, SP) || is(text, j, TAB);
    }

    if c == Some(OPEN_UNIT) {
        return placeholder_line_end(text, i, end).is_some_and(|after| skip_spaces(text, after, end) == end);
    }

    if c == Some(LT) {
        return opens_html_block(text, i, end);
    }

    if is_thematic_break(thematic_tail(text, i, end), i) {
        return true;
    }

    if !paragraph_open || (c != Some(DASH) && c != Some(EQ)) {
        return false;
    }

    let c = c.expect("checked above");
    for j in i..end {
        if !is(text, j, c) && !is(text, j, SP) && !is(text, j, TAB) {
            return false;
        }
    }

    true
}

fn paragraph_open_after(text: &[u16], end: usize, place: &Place, paragraph_open: bool, fresh: bool) -> bool {
    if place.content == end {
        return false;
    }
    let (_, text_column) = skip_indent(text, place.base, end, place.base_column);
    if (!paragraph_open || fresh) && text_column - place.base_column >= TAB_STOP {
        return false;
    }
    !closes_block(text, place.content, end, place.content_column, paragraph_open && !fresh)
}

struct Walk {
    place: Place,
    fresh: bool,
    paragraph_open: bool,
}

fn walk_line(text: &[u16], start: usize, end: usize, open: &mut Vec<Container>, paragraph_open: bool) -> Walk {
    let line = match_containers(text, start, end, open);
    let left = line.matched < open.len();
    let place = open_containers(text, line.at, end, line.column, open, line.matched, paragraph_open && !left);
    let fresh = place.opened > 0;
    let after = paragraph_open_after(text, end, &place, paragraph_open, fresh);
    if !left || fresh || !paragraph_open || !after {
        open.truncate(line.matched + place.opened);
    }
    Walk { place, fresh, paragraph_open: after }
}

#[cfg(test)]
pub fn scan_indented_code(
    source: &[u16],
    check_budget: CheckBudget<'_>,
    read_opaque: ReadOpaque<'_>,
) -> Result<Vec<Range>> {
    scan_indented_code_options(source, check_budget, read_opaque, false, None)
}

fn scan_indented_code_options(
    source: &[u16],
    check_budget: CheckBudget<'_>,
    read_opaque: ReadOpaque<'_>,
    mdx: bool,
    mut inline: ReadLine<'_>,
) -> Result<Vec<Range>> {
    if !source.contains(&TAB) && !js::includes(source, &[SP, SP, SP, SP]) {
        return Ok(Vec::new());
    }

    let text = &line_view(source)[..];
    let n = text.len();
    let mut spans: Vec<Range> = Vec::new();
    let mut opens_html = HtmlBlockOpener::new_options(read_opaque, mdx, check_budget);
    let mut inline_through = 0;
    let mut ends = Ends::default();
    let mut tick = LineTicker::new(check_budget, "md-indented-code");
    let mut i = 0;
    let mut chunk: Option<usize> = None;
    let mut paragraph_open = false;
    let mut open: Vec<Container> = Vec::new();

    while i < n {
        tick.tick()?;
        let nl = js::index_of_unit(text, LF, i);
        let end = content_end(text, i, nl.unwrap_or(n));
        if inline_through > i {
            inline_through = inline.as_mut().expect("inline reader")(i, end)?;
            i = nl.map_or(n, |p| p + 1);
            continue;
        }
        let walk = walk_line(text, i, end, &mut open, paragraph_open);
        let place = walk.place;
        let (_, text_column) = skip_indent(text, place.base, end, place.base_column);
        let blank = place.content == end;
        let code = !blank && (walk.fresh || !paragraph_open) && text_column - place.base_column >= TAB_STOP;
        let html =
            if code || blank { None } else { opens_html.call(text, place.content, !paragraph_open || walk.fresh)? };
        if !code
            && html.is_none()
            && let Some(inline) = inline.as_mut()
        {
            inline_through = inline(i, end)?;
        }

        if code {
            if let Some(k) = chunk {
                spans[k].1 = end;
            } else {
                chunk = Some(spans.len());
                spans.push((place.base, end));
            }
        } else if !blank {
            chunk = None;
        }

        paragraph_open = html.is_none() && walk.paragraph_open;
        i = nl.map_or(n, |p| p + 1);

        if let Some(m) = html {
            i = html_block_end(text, place.content, &m, i, nl, end, &open, &mut ends, &mut tick)?.next;
        }
    }

    Ok(spans)
}

fn marker_run_end(text: &[u16], i: usize, end: usize) -> usize {
    let mut j = i;
    match text.get(j) {
        Some(&(DASH | STAR | PLUS)) => j += 1,
        Some(&HASH) => j = hash_run_end(text, i, end),
        _ => {
            while j < end && j < i + 9 && text.get(j).is_some_and(|c| (0x30..=0x39).contains(c)) {
                j += 1;
            }
            if j == i || (!is(text, j, DOT) && !is(text, j, RPAREN)) {
                return i;
            }
            j += 1;
        }
    }
    j
}

fn marker_token_end(text: &[u16], i: usize, end: usize) -> usize {
    let j = marker_run_end(text, i, end);
    if j != i && (j == end || is(text, j, SP) || is(text, j, TAB)) { j } else { i }
}

fn marker_end(text: &[u16], i: usize, end: usize) -> usize {
    let token = marker_token_end(text, i, end);
    if token == i { i } else { skip_spaces(text, token, end) }
}

pub fn scan_line_prefix(source: &[u16], check_budget: CheckBudget<'_>) -> Result<Vec<Spanned>> {
    let text = &line_view(source)[..];
    let mut spans = Vec::new();
    let mut lines = EachLine::new(check_budget, "md-line-prefix");
    let mut containers = Vec::new();
    let mut paragraph_open = false;

    while let Some((start, end)) = lines.next(text)? {
        let walk = walk_line(text, start, end, &mut containers, paragraph_open);
        let (fresh, after) = (walk.fresh, walk.paragraph_open);
        paragraph_open = after;
        let mut i = skip_spaces(text, start, end);
        let mut cut = i;
        let mut structural = false;

        loop {
            if i < end && is(text, i, GT) {
                let after = skip_spaces(text, i + 1, end);
                cut = after;
                i = after;
                structural = true;
                continue;
            }
            let marker = marker_end(text, i, end);
            if marker == i {
                break;
            }
            cut = marker;
            i = marker;
            structural = true;
        }

        if cut > start && (structural || i < end) {
            let kind = if structural && (fresh || !after) { SpanKind::LineBlock } else { SpanKind::Plain };
            spans.push((start, cut, kind));
        }
    }

    Ok(spans)
}

static TABLE_DELIMITER_CELL: LazyLock<Re> = LazyLock::new(|| Re::new(r"^[ \t]*:?-+:?[ \t]*$", "u"));

fn table_cells(text: &[u16], start: usize, end: usize, delimiter: bool) -> usize {
    let mut from = skip_spaces(text, start, end);
    let mut to = end;
    while to > from && matches!(text[to - 1], SP | TAB) {
        to -= 1;
    }
    if from == to {
        return 0;
    }
    let mut pipes = 0;
    if is(text, from, 0x7C) {
        pipes += 1;
        from += 1;
    }
    let mut cells = 0;
    let mut cell = from;
    let mut at = from;
    while at <= to {
        if at + 1 < to && is(text, at, BACKSLASH) {
            at += 2;
            continue;
        }
        if at < to && !is(text, at, 0x7C) {
            at += 1;
            continue;
        }
        if at == to && cell == to && pipes > 0 {
            break;
        }
        if delimiter && !TABLE_DELIMITER_CELL.test(&text[cell..at]) {
            return 0;
        }
        cells += 1;
        cell = at + 1;
        pipes += usize::from(at < to);
        at += 1;
    }
    if delimiter && pipes == 0 { 0 } else { cells }
}

pub fn scan_table_delimiters(source: &[u16], check_budget: CheckBudget<'_>) -> Result<Vec<Spanned>> {
    if !source.contains(&0x7C) {
        return Ok(Vec::new());
    }
    let text = &line_view(source)[..];
    let mut spans = Vec::new();
    let mut chain = Vec::new();
    let mut paragraph_open = false;
    let mut header: Option<(usize, Option<Container>)> = None;
    let mut table = false;
    let mut lines = EachLine::new(check_budget, "md-table");
    while let Some((start, end)) = lines.next(text)? {
        let line = match_containers(text, start, end, &chain);
        let continued = line.matched == chain.len();
        let (content, _) = skip_block_indent(text, line.at, end, line.column, line.column);
        let columns = table_cells(text, content, end, true);
        if !table
            && continued
            && columns > 0
            && header.is_some_and(|(width, parent)| columns == width && chain.last().copied() == parent)
        {
            spans.push((start, end, SpanKind::TableDelimiter));
            table = true;
            header = None;
            paragraph_open = false;
            continue;
        }
        let walk = walk_line(text, start, end, &mut chain, paragraph_open);
        paragraph_open = walk.paragraph_open;
        if !continued || walk.fresh || !paragraph_open {
            table = false;
        }
        header = if table || !paragraph_open {
            None
        } else {
            Some((table_cells(text, walk.place.content, end, false), chain.last().copied()))
        };
    }
    Ok(spans)
}

fn reference_destination_end(text: &[u16], i: usize, line_end: usize) -> Option<usize> {
    if is(text, i, LT) {
        let mut j = i + 1;
        while j < line_end {
            if is(text, j, BACKSLASH) {
                j += 2;
                continue;
            }
            if is(text, j, GT) {
                return Some(j + 1);
            }
            if is(text, j, LT) {
                return None;
            }
            j += 1;
        }
        return None;
    }
    let mut j = i;
    while j < line_end && !is(text, j, SP) && !is(text, j, TAB) {
        j += if is(text, j, BACKSLASH) { 2 } else { 1 };
    }
    if j == i { None } else { Some(j.min(line_end)) }
}

static LABEL_WHITESPACE_RE: LazyLock<Re> = LazyLock::new(|| Re::new(r"[ \t\r\n]+", "g"));
static LABEL_LINE_PREFIX_RE: LazyLock<Re> = LazyLock::new(|| Re::new(r"\n[ \t>]*", "g"));
static LABEL_QUOTE_PREFIX_RE: LazyLock<Re> = LazyLock::new(|| Re::new(r"\n(?:[ \t]{0,3}>[ \t]?)+", "g"));

fn normalize_label(raw: &[u16], resolve: Resolve<'_>) -> JsString {
    let source = LABEL_QUOTE_PREFIX_RE.replace_str(&resolve(raw), "\n");
    if std::char::decode_utf16(source.iter().copied()).take(1000).count() > 999 {
        return Vec::new();
    }
    let stripped = LABEL_LINE_PREFIX_RE.replace_str(&source, "\n");
    let collapsed = LABEL_WHITESPACE_RE.replace_str(js::trim(&stripped), " ");
    js::to_lower(&js::to_upper(&js::to_lower(&collapsed)))
}

fn blank_ranges(text: &[u16], ranges: &[Range]) -> JsString {
    blank_ranges_with(text, ranges, false)
}

fn blank_ranges_with(text: &[u16], ranges: &[Range], keep_lines: bool) -> JsString {
    let mut out = Vec::with_capacity(text.len());
    let mut cursor = 0;
    for &(start, end) in ranges {
        out.extend_from_slice(js::slice(text, cursor, start));
        if keep_lines {
            out.extend(js::slice(text, start, end).iter().map(|&c| if c == LF || c == CR { c } else { SP }));
        } else {
            out.extend(std::iter::repeat_n(SP, end.saturating_sub(start)));
        }
        cursor = end;
    }
    out.extend_from_slice(js::from(text, cursor));
    out
}

fn label_end(text: &[u16], start: usize, limit: usize, code_spans: &[Range], from: usize) -> isize {
    let mut j = start;
    let mut span = from;
    while j < limit {
        while span < code_spans.len() && code_spans[span].1 <= j {
            span += 1;
        }
        if span < code_spans.len() && j >= code_spans[span].0 {
            j = code_spans[span].1;
            continue;
        }
        match text.get(j).copied() {
            Some(BACKSLASH) => {
                j += 2;
                continue;
            }
            Some(LBRACKET) => return -1,
            Some(RBRACKET) => return j as isize,
            _ => {}
        }
        j += 1;
    }
    limit as isize
}

fn paragraph_line(text: &[u16], stop: usize, n: usize, containers: &[Container]) -> Option<Range> {
    let brk = if is(text, stop, CR) { stop + 1 } else { stop };
    if !is(text, brk, LF) {
        return None;
    }
    let start = brk + 1;
    let next = js::index_of_unit(text, LF, start);
    let end = content_end(text, start, next.unwrap_or(n));
    let line = match_containers(text, start, end, containers);
    if skip_spaces(text, line.at, end) == end {
        return None;
    }
    let mut own: Vec<Container> = Vec::new();
    let place = open_containers(text, line.at, end, line.column, &mut own, 0, true);
    if place.opened > 0 || closes_block(text, place.content, end, place.content_column, true) {
        return None;
    }
    Some((skip_spaces(text, place.content, end), end))
}

fn title_closer(c: Option<u16>) -> Option<u16> {
    match c {
        Some(DQ) => Some(DQ),
        Some(SQ) => Some(SQ),
        Some(LPAREN) => Some(RPAREN),
        _ => None,
    }
}

type NextLine<'a> = &'a mut dyn FnMut(usize) -> Result<Option<Range>>;

fn title_close(text: &[u16], open: usize, stop: usize, next_line: NextLine<'_>) -> Result<Option<Range>> {
    let close = title_closer(text.get(open).copied()).expect("a title opener");
    let mut j = open + 1;
    let mut limit = stop;
    loop {
        while j < limit && !is(text, j, close) {
            j += if is(text, j, BACKSLASH) { 2 } else { 1 };
        }
        if j < limit {
            return Ok(Some((j + 1, limit)));
        }
        let Some((at, line_stop)) = next_line(limit)? else { return Ok(None) };
        j = at;
        limit = line_stop;
    }
}

fn whole_title(text: &[u16], at: usize, stop: usize, next_line: NextLine<'_>) -> Result<Option<Range>> {
    let title =
        if title_closer(text.get(at).copied()).is_some() { title_close(text, at, stop, next_line)? } else { None };
    Ok(title.filter(|&(end, line_stop)| skip_spaces(text, end, line_stop) == line_stop))
}

struct Definition {
    start: usize,
    end: usize,
    label: JsString,
}

fn reference_definition(
    text: &[u16],
    i: usize,
    line_end: usize,
    resolve: Resolve<'_>,
    next_line: NextLine<'_>,
) -> Result<Option<Definition>> {
    if !is(text, i, LBRACKET) || is(text, i + 1, CARET) {
        return Ok(None);
    }

    let mut stop = line_end;
    let mut j = label_end(text, i + 1, stop, &[], 0);
    while j == stop as isize {
        let Some((at, line_stop)) = next_line(stop)? else { return Ok(None) };
        stop = line_stop;
        j = label_end(text, at, stop, &[], 0);
    }

    let label = if j == -1 { Vec::new() } else { normalize_label(js::slice(text, i + 1, j as usize), resolve) };
    if label.is_empty() || !is(text, j as usize + 1, COLON) {
        return Ok(None);
    }
    let j = j as usize;

    let mut at = skip_spaces(text, j + 2, stop);
    if at == stop {
        let Some((line_at, line_stop)) = next_line(stop)? else { return Ok(None) };
        at = line_at;
        stop = line_stop;
    }

    let Some(end) = reference_destination_end(text, at, stop) else { return Ok(None) };

    let after = skip_spaces(text, end, stop);
    if after < stop {
        let title = if after > end { whole_title(text, after, stop, next_line)? } else { None };
        return Ok(title.map(|(title_end, _)| Definition { start: i, end: title_end, label }));
    }

    let title = match next_line(stop)? {
        None => None,
        Some((line_at, line_stop)) => whole_title(text, line_at, line_stop, next_line)?,
    };
    Ok(Some(Definition { start: i, end: title.map_or(end, |t| t.0), label }))
}

fn scan_reference_uses(
    text: &[u16],
    labels: &HashSet<JsString>,
    definitions: &[Range],
    code_spans: &[Range],
    resolve: Resolve<'_>,
    check_budget: CheckBudget<'_>,
) -> Result<Vec<Range>> {
    let n = text.len();
    let mut spans = Vec::new();
    let mut definition = 0;
    let mut code = 0;
    let mut i = 0;
    let mut next_check = 0;

    let defined = |span: Option<Range>| {
        span.is_some_and(|(a, b)| labels.contains(&normalize_label(js::slice(text, a, b), resolve)))
    };

    while i < n {
        if i >= next_check {
            check_budget("md-reference-definition")?;
            next_check = i + 0x4000;
        }

        while definition < definitions.len() && definitions[definition].1 <= i {
            definition += 1;
        }
        if definition < definitions.len() && i >= definitions[definition].0 {
            i = definitions[definition].1;
            continue;
        }

        while code < code_spans.len() && code_spans[code].1 <= i {
            code += 1;
        }
        if code < code_spans.len() && i >= code_spans[code].0 {
            i = code_spans[code].1;
            continue;
        }

        if is(text, i, BACKSLASH) {
            i += 2;
            continue;
        }
        if !is(text, i, LBRACKET) {
            i += 1;
            continue;
        }

        let close = label_end(text, i + 1, n, code_spans, code);
        if close < 0 || !is(text, close as usize, RBRACKET) {
            i += 1;
            continue;
        }
        let close = close as usize;

        if is(text, close + 1, LPAREN) {
            match scan_link_tail(text, close + 2, n) {
                LinkTail::Overlimit => {
                    i = close + 1;
                    continue;
                }
                LinkTail::End(inline_end) => {
                    i = inline_end;
                    continue;
                }
                LinkTail::Fail => {}
            }
        }

        let first = (i + 1, close);
        let mut second = None;
        if is(text, close + 1, LBRACKET) {
            let second_close = label_end(text, close + 2, n, code_spans, code);
            if second_close >= 0 && is(text, second_close as usize, RBRACKET) {
                second = Some((close + 2, second_close as usize));
            }
        }

        if defined(second) {
            spans.push(second.expect("defined"));
        } else if defined(Some(first)) {
            spans.push(first);
        }

        i = second.map_or(close, |s| s.1) + 1;
    }

    Ok(spans)
}

pub fn scan_reference_definitions(
    source: &[u16],
    check_budget: CheckBudget<'_>,
    resolve: Resolve<'_>,
    scan_inline: Option<ScanInline<'_>>,
) -> Result<Vec<Range>> {
    if !js::includes(source, &[RBRACKET, COLON]) {
        return Ok(Vec::new());
    }

    let text = &line_view(source)[..];
    let mut definitions: Vec<Range> = Vec::new();
    let mut labels: HashSet<JsString> = HashSet::new();
    let n = text.len();
    let mut open: Vec<Container> = Vec::new();
    let mut may_define = true;
    let mut paragraph_open = false;
    let mut consumed = 0;
    let mut crossed = 0usize;
    let mut lines = EachLine::new(check_budget, "md-reference-definition");

    while let Some((start, end)) = lines.next(text)? {
        if start < consumed {
            continue;
        }
        let walk = walk_line(text, start, end, &mut open, paragraph_open);
        let definition = if may_define || walk.fresh {
            let open = &open;
            let mut next_line = |stop: usize| -> Result<Option<Range>> {
                if crossed & 0x3ff == 0 {
                    check_budget("md-reference-definition")?;
                }
                crossed += 1;
                Ok(paragraph_line(text, stop, n, open))
            };
            reference_definition(text, walk.place.content, end, resolve, &mut next_line)?
        } else {
            None
        };

        may_define = definition.is_some() || !walk.paragraph_open;
        if let Some(d) = definition {
            definitions.push((d.start, d.end));
            labels.insert(d.label);
            consumed = d.end;
        }
        paragraph_open = walk.paragraph_open;
    }

    if definitions.is_empty() {
        return Ok(definitions);
    }

    let code_spans = if text.contains(&BACKTICK) {
        let masked = blank_ranges(text, &definitions);
        match scan_inline {
            Some(scan) => scan(&masked)?,
            None => scan_code_spans(&masked, check_budget)?,
        }
    } else {
        Vec::new()
    };
    let uses = scan_reference_uses(text, &labels, &definitions, &code_spans, resolve, check_budget)?;

    if uses.is_empty() {
        return Ok(definitions);
    }
    let mut all = definitions;
    all.extend(uses);
    all.sort_by_key(|r| r.0);
    Ok(all)
}

enum Dest {
    Found { end: usize, closed: bool },
    Fail,
    Overlimit,
}

enum LinkTail {
    End(usize),
    Fail,
    Overlimit,
}

fn scan_bare_destination(text: &[u16], i: usize, limit: usize, capped: bool) -> Dest {
    let mut depth = 0usize;
    let mut j = i;
    while j < limit {
        let c = text.get(j).copied();
        if c == Some(BACKSLASH) {
            j += 2;
            continue;
        }
        if c == Some(LPAREN) {
            depth += 1;
            j += 1;
            continue;
        }
        if c == Some(RPAREN) {
            if depth == 0 {
                return Dest::Found { end: j + 1, closed: true };
            }
            depth -= 1;
            j += 1;
            continue;
        }
        if is_flow_space(c) {
            return if depth == 0 { Dest::Found { end: j, closed: false } } else { Dest::Fail };
        }
        j += 1;
    }
    if capped { Dest::Overlimit } else { Dest::Fail }
}

fn scan_angle_destination(text: &[u16], i: usize, limit: usize, capped: bool) -> Dest {
    let mut j = i + 1;
    while j < limit {
        let c = text.get(j).copied();
        if c == Some(BACKSLASH) {
            j += 2;
            continue;
        }
        if c == Some(GT) {
            return Dest::Found { end: j + 1, closed: false };
        }
        if c == Some(LF) || c == Some(CR) {
            return Dest::Fail;
        }
        j += 1;
    }
    if capped { Dest::Overlimit } else { Dest::Fail }
}

fn skip_link_space(text: &[u16], i: usize, limit: usize) -> usize {
    let mut j = i;
    while j < limit && is_flow_space(text.get(j).copied()) {
        j += 1;
    }
    j
}

fn scan_title(text: &[u16], i: usize, limit: usize, capped: bool) -> LinkTail {
    let open = text.get(i).copied();
    let close = if open == Some(LPAREN) { Some(RPAREN) } else { open };
    let mut j = i + 1;
    while j < limit {
        let c = text.get(j).copied();
        if c == Some(BACKSLASH) {
            j += 2;
            continue;
        }
        if c == close {
            return LinkTail::End(j + 1);
        }
        j += 1;
    }
    if capped { LinkTail::Overlimit } else { LinkTail::Fail }
}

fn scan_link_tail(text: &[u16], i: usize, n: usize) -> LinkTail {
    let limit = n.min(i + MAX_LINK_SCAN);
    let capped = limit < n;
    let mut j = skip_link_space(text, i, limit);

    let dest = if is(text, j, LT) {
        scan_angle_destination(text, j, limit, capped)
    } else {
        scan_bare_destination(text, j, limit, capped)
    };

    match dest {
        Dest::Overlimit => return LinkTail::Overlimit,
        Dest::Fail => return LinkTail::Fail,
        Dest::Found { end, closed: true } => return LinkTail::End(end),
        Dest::Found { end, closed: false } => j = end,
    }

    j = skip_link_space(text, j, limit);

    if is(text, j, DQ) || is(text, j, SQ) || is(text, j, LPAREN) {
        match scan_title(text, j, limit, capped) {
            LinkTail::Overlimit => return LinkTail::Overlimit,
            LinkTail::Fail => return LinkTail::Fail,
            LinkTail::End(title_end) => j = title_end,
        }
        j = skip_link_space(text, j, limit);
    }

    if is(text, j, RPAREN) { LinkTail::End(j + 1) } else { LinkTail::Fail }
}

static PARAGRAPH_BREAK_RE: LazyLock<Re> = LazyLock::new(|| Re::new(r"\n[ \t]*\r?\n", ""));
static LINKABLE_RE: LazyLock<Re> = LazyLock::new(|| Re::new(&LINKABLE_SOURCE, "g"));

fn reference_label_end(text: &[u16], at: usize, resolve: Resolve<'_>) -> Option<usize> {
    let close = usize::try_from(label_end(text, at + 1, text.len(), &[], 0)).ok()?;
    (is(text, close, RBRACKET)
        && !is(text, close + 1, LPAREN)
        && (close == at + 1 || !normalize_label(&text[at + 1..close], resolve).is_empty())
        && !PARAGRAPH_BREAK_RE.test(&text[at..close]))
    .then_some(close)
}

fn link_syntax_cuts(text: &[u16], mut hidden: Option<&mut Vec<Range>>) -> Vec<usize> {
    let n = text.len();
    let mut cuts = Vec::new();
    let mut from = 0;
    while let Some(at) = js::index_of_unit(text, RBRACKET, from) {
        from = at + 1;
        if is(text, at + 1, LBRACKET) {
            if let Some(label) = reference_label_end(text, at + 1, &|s| s.to_vec()) {
                cuts.extend([at, label]);
                if let Some(hidden) = hidden.as_deref_mut() {
                    hidden.push((at, label + 1));
                }
                from = label + 1;
            }
            continue;
        }
        if !is(text, at + 1, LPAREN) {
            continue;
        }
        cuts.push(at);
        match scan_link_tail(text, at + 2, n) {
            LinkTail::Overlimit => break,
            LinkTail::Fail => {
                if let Some(hidden) = hidden.as_deref_mut() {
                    hidden.push((at, at + 1));
                }
            }
            LinkTail::End(end) => {
                cuts.push(end - 1);
                if let Some(hidden) = hidden.as_deref_mut() {
                    hidden.push((at, end));
                }
                from = end;
            }
        }
    }
    cuts
}

pub fn markdown_address_syntax<'a>(
    text: &[u16],
    scan_inline: crate::protect::blocks::OpaqueScanner,
    elements: &'a crate::protect::blocks::ElementSet,
    mdx: bool,
) -> crate::protect::sequences::AddressLayout<'a> {
    let mut hidden = Vec::new();
    let cuts = link_syntax_cuts(text, Some(&mut hidden));
    let has_cuts = !cuts.is_empty();
    let source = text.to_vec();
    let prose = move |check_budget: CheckBudget<'_>| -> Result<JsString> {
        let resolve = |s: &[u16]| s.to_vec();
        let ctx = ScanCtx { check_budget, resolve: &resolve, elements, code_edges: false };
        let inline: Vec<Range> = scan_inline(&source, &ctx)?.into_iter().map(|(a, b, _)| (a, b)).collect();
        let mut visible = blank_ranges_with(&source, &inline, true);
        let mut ranges = Vec::new();
        {
            let mut reader = elements.element_reader(&visible, &ctx, None)?;
            let mut at = reader.next(0)?;
            while let Some(found) = at {
                let span = reader.read(found.0)?;
                if span.report != Some(false) {
                    ranges.push((span.start, span.end));
                }
                at = reader.next(span.after)?;
            }
        }
        visible = blank_ranges_with(&visible, &ranges, true);
        let destinations = if has_cuts && visible != source {
            let mut destinations = Vec::new();
            link_syntax_cuts(&visible, Some(&mut destinations));
            destinations
        } else {
            hidden
        };
        visible = blank_ranges_with(&visible, &destinations, true);
        if !source.iter().any(|&c| c == LF || c == CR) {
            return Ok(visible);
        }
        let paragraph = prose_paragraphs_options(&source, false, check_budget, mdx)?.paragraph;
        let mut out = Vec::with_capacity(visible.len());
        let mut line = 0;
        let mut i = 0;
        while i < visible.len() {
            let c = visible[i];
            if c != LF && c != CR {
                out.push(c);
                i += 1;
                continue;
            }
            let len = if c == CR && visible.get(i + 1) == Some(&LF) { 2 } else { 1 };
            let current = paragraph.get(line).copied();
            line += 1;
            if current.is_some_and(|current| current >= 0) && current == paragraph.get(line).copied() {
                out.extend_from_slice(&visible[i..i + len]);
            } else {
                out.extend(std::iter::repeat_n(0x2029, len));
            }
            i += len;
        }
        Ok(out)
    };
    crate::protect::sequences::AddressLayout { cuts, prose: Box::new(prose) }
}

fn linkable_addresses(text: &[u16]) -> Vec<Range> {
    LINKABLE_RE.find_all(text).iter().map(|m| (m.index(), m.end())).collect()
}

fn link_text_addresses(addresses: &[Range], from: usize, to: usize, spans: &mut Vec<Spanned>) {
    let first = addresses.partition_point(|a| a.0 < from);
    for &(start, end) in addresses[first..].iter().take_while(|a| a.0 < to) {
        if end <= to {
            spans.push((start, end, SpanKind::LabelAddress));
        }
    }
}

#[cfg(test)]
pub fn scan_link_destinations(text: &[u16], check_budget: CheckBudget<'_>) -> Result<Vec<Range>> {
    Ok(scan_link_destinations_resolved(text, check_budget, &|s| s.to_vec())?
        .into_iter()
        .map(|(a, b, _)| (a, b))
        .collect())
}

pub fn scan_link_destinations_resolved(
    text: &[u16],
    check_budget: CheckBudget<'_>,
    resolve: Resolve<'_>,
) -> Result<Vec<Spanned>> {
    let n = text.len();
    let mut spans: Vec<Spanned> = Vec::new();
    let last_close = js::last_index_of(text, &[RPAREN]).map_or(-1, |p| p as isize);
    let mut opens: Vec<usize> = Vec::new();
    let mut addresses: Option<Vec<Range>> = None;
    let mut i = 0;
    let mut next_check = 0;

    while i + 1 < n {
        if i >= next_check {
            check_budget("md-link-scan")?;
            next_check = i + 0x4000;
        }

        if is(text, i, LBRACKET) && (i == 0 || text[i - 1] != BACKSLASH) {
            opens.push(i);
        }

        if !is(text, i, RBRACKET) || !is(text, i + 1, LPAREN) {
            if is(text, i, RBRACKET) {
                let open = opens.pop();
                let label = if is(text, i + 1, LBRACKET) { reference_label_end(text, i + 1, resolve) } else { None };
                if let (Some(open), Some(label)) = (open, label) {
                    let addresses = addresses.get_or_insert_with(|| linkable_addresses(text));
                    let from = (open + 1).max(spans.last().map_or(0, |s| s.1));
                    link_text_addresses(addresses, from, i, &mut spans);
                    if label > i + 2 {
                        spans.push((i + 2, label, SpanKind::Plain));
                    }
                    i = label + 1;
                    continue;
                }
            }
            i += 1;
            continue;
        }

        let open = opens.pop();

        match scan_link_tail(text, i + 2, n) {
            LinkTail::Overlimit => {
                if last_close < (i + 2) as isize {
                    return Ok(spans);
                }
                return Err(Error::input(
                    format!(
                        "Markdown link destination/title exceeds the {MAX_LINK_SCAN}-char scan bound at offset {i}"
                    ),
                    json!({ "offset": i, "max": MAX_LINK_SCAN, "reason": "link-overlimit" }),
                ));
            }
            LinkTail::Fail => {
                i += 1;
            }
            LinkTail::End(end) => {
                if let Some(open) = open {
                    let addresses = addresses.get_or_insert_with(|| linkable_addresses(text));
                    let from = (open + 1).max(spans.last().map_or(0, |s| s.1));
                    link_text_addresses(addresses, from, i, &mut spans);
                }
                spans.push((i + 2, end - 1, SpanKind::Plain));
                i = end;
            }
        }
    }

    Ok(spans)
}

static HTML_BLOCK_OPEN_RE: LazyLock<Re> =
    LazyLock::new(|| Re::new(r"^<(?:(pre|script|style|textarea)(?=[\s>]|$)|(!--)|\/?([A-Za-z][A-Za-z0-9-]*))", "i"));
static LONE_TAG_RE: LazyLock<Re> = LazyLock::new(|| {
    Re::new(
        r#"^(?:<([A-Za-z][A-Za-z0-9-]*)(?:[ \t]+[A-Za-z_:][\w.:-]*(?:[ \t]*=[ \t]*(?:[^ \t\n\v\f\r"'=<>`]+|'[^'\n\r]*'|"[^"\n\r]*"))?)*[ \t]*\/?|<\/([A-Za-z][A-Za-z0-9-]*)[ \t]*)>[ \t]*(?:$|(?=[\n\r  ]))"#,
        "",
    )
});
static HTML_BLOCK_LINE_RE: LazyLock<Re> = LazyLock::new(|| Re::new(r"^[ \t>*+0-9.)-]*<[!/A-Za-z]", "m"));
static RAW_BLOCK_END_RE: LazyLock<Re> = LazyLock::new(|| Re::new(r"<\/(?:pre|script|style|textarea)>", "gi"));
static JSX_RAW_BLOCK_END_RE: LazyLock<Re> = LazyLock::new(|| Re::new(r"<\/(?:pre|script|style|textarea)>", "g"));
static COMMENT_END: LazyLock<Re> = LazyLock::new(|| Re::new("-->", "g"));
static BLANK_LINE_RE: LazyLock<Re> = LazyLock::new(|| Re::new(r"\n[ \t]*\r?\n", "g"));

struct HtmlOpen {
    len: usize,
    raw: Option<Range>,
    name: Option<Range>,
    case_sensitive: bool,
}

fn ends_at_blank(text: &[u16], at: usize, m: &HtmlOpen, may_interrupt: bool) -> bool {
    let next = at + m.len;
    let c = text.get(next).copied();
    let (name_start, name_end) = m.name.expect("a tag name");

    if BLOCK_KIND_6_NAMES.contains(lower(&text[name_start..name_end]).as_str())
        && (next == text.len() || is_flow_space(c) || c == Some(GT) || (c == Some(SLASH) && is(text, next + 1, GT)))
    {
        return true;
    }

    if !may_interrupt {
        return false;
    }

    let eol = js::index_of_unit(text, LF, at).unwrap_or(text.len());
    LONE_TAG_RE.test(&text[at..eol])
}

static RAW_TEXT_NAMES: LazyLock<Vec<String>> =
    LazyLock::new(|| ["script", "style", "textarea"].into_iter().map(String::from).collect());

#[derive(Clone, Copy)]
enum EndRe {
    RawBlockEnd = 0,
    CommentEnd = 1,
    BlankLine = 2,
    JsxRawBlockEnd = 3,
}

impl EndRe {
    fn re(self) -> &'static Re {
        match self {
            EndRe::RawBlockEnd => &RAW_BLOCK_END_RE,
            EndRe::CommentEnd => &COMMENT_END,
            EndRe::BlankLine => &BLANK_LINE_RE,
            EndRe::JsxRawBlockEnd => &JSX_RAW_BLOCK_END_RE,
        }
    }
}

#[derive(Default)]
struct Ends {
    known: [Option<Option<usize>>; 4],
    block_tags: Option<Vec<Span>>,
}

fn next_match(re: EndRe, text: &[u16], from: usize, seen: &mut Ends) -> Option<usize> {
    if let Some(known) = seen.known[re as usize]
        && known.is_none_or(|k| k >= from)
    {
        return known;
    }
    let at = re.re().exec_at(text, from).map(|c| c.index());
    seen.known[re as usize] = Some(at);
    at
}

fn raw_block_end(
    text: &[u16],
    name: &str,
    at: usize,
    line_stop: usize,
    ends: &mut Ends,
    case_sensitive: bool,
) -> usize {
    let tags = ends.block_tags.get_or_insert_with(|| {
        crate::protect::tag_spans::tag_spans_options(
            text,
            &RAW_TEXT_NAMES,
            case_sensitive,
            Some(markdown_comment_closer(text, &|_| Ok(()))),
            crate::protect::tag_spans::Reading::Document,
            &[],
            &|_| Ok(()),
        )
        .expect("unlimited scanner")
    });
    let lo = tags.partition_point(|t| t.start < at);

    if tags.get(lo).map(|t| t.start) != Some(at) {
        return line_stop;
    }

    let mut depth: isize = 0;
    let mut k = lo;
    while k < tags.len() && tags[k].start < line_stop {
        let Span { start, end: gt, opens_nothing: empty } = tags[k];
        let closing = is(text, start + 1, SLASH);
        let name_start = if closing { start + 2 } else { start + 1 };
        if (!case_sensitive || js::starts_with_at(text, &js::u(name), name_start)) && named_at(text, name_start, name) {
            if closing {
                depth -= 1;
                if depth == 0 {
                    return if gt < line_stop { gt + 1 } else { line_stop };
                }
            } else {
                depth += isize::from(!empty);
            }
        }
        k += 1;
    }

    line_stop
}

fn opens_line(text: &[u16], at: usize) -> bool {
    let mut i = at;
    while i > 0 && at - i < 3 && is(text, i - 1, SP) {
        i -= 1;
    }
    i == 0 || is(text, i - 1, LF)
}

fn covered(spans: &[Range], cursor: &mut usize, at: usize) -> bool {
    while *cursor < spans.len() && spans[*cursor].1 <= at {
        *cursor += 1;
    }
    *cursor < spans.len() && spans[*cursor].0 <= at
}

struct HtmlBlockOpener<'r> {
    read_opaque: ReadOpaque<'r>,
    tags: Option<Vec<Span>>,
    opaque: Option<Vec<Range>>,
    tag_cursor: usize,
    opaque_cursor: usize,
    case_sensitive: bool,
    check_budget: CheckBudget<'r>,
}

impl<'r> HtmlBlockOpener<'r> {
    fn new_options(read_opaque: ReadOpaque<'r>, case_sensitive: bool, check_budget: CheckBudget<'r>) -> Self {
        HtmlBlockOpener {
            read_opaque,
            tags: None,
            opaque: None,
            tag_cursor: 0,
            opaque_cursor: 0,
            case_sensitive,
            check_budget,
        }
    }

    fn call(&mut self, text: &[u16], at: usize, may_interrupt: bool) -> Result<Option<HtmlOpen>> {
        if !is(text, at, LT) {
            return Ok(None);
        }

        let eol = js::index_of_unit(text, LF, at).unwrap_or(text.len());
        let Some(m) = HTML_BLOCK_OPEN_RE.exec(&text[at..eol]) else { return Ok(None) };
        let shift = |r: std::ops::Range<usize>| (r.start + at, r.end + at);
        let open = HtmlOpen {
            len: m.end(),
            raw: m.range(1).map(shift),
            name: m.range(3).map(shift),
            case_sensitive: self.case_sensitive,
        };

        if open.name.is_some() && !ends_at_blank(text, at, &open, may_interrupt) {
            return Ok(None);
        }

        if self.case_sensitive
            && let Some((a, b)) = open.raw.or(open.name)
            && text[a..b] != js::to_lower(&text[a..b])
        {
            return Ok(None);
        }
        if self.opaque.is_none() {
            self.opaque = Some(match self.read_opaque {
                Some(read) => read()?,
                None => Vec::new(),
            });
        }
        if self.tags.is_none() {
            self.tags = Some(if self.case_sensitive {
                crate::input::mdx::tag_spans(text, self.check_budget, self.opaque.as_deref().unwrap_or_default())?
            } else {
                crate::protect::tag_spans::tag_spans_options(
                    text,
                    &[],
                    false,
                    Some(markdown_comment_closer(text, self.check_budget)),
                    crate::protect::tag_spans::Reading::Fragment,
                    self.opaque.as_deref().unwrap_or_default(),
                    self.check_budget,
                )?
            });
        }
        let tags = self.tags.as_ref().expect("tag spans");
        if self.opaque.is_none() {
            self.opaque = Some(match self.read_opaque {
                Some(read) => read()?,
                None => Vec::new(),
            });
        }
        let opaque = self.opaque.as_deref().unwrap_or_default();

        let hidden = inside_tag(text, tags, &mut self.tag_cursor, at) || covered(opaque, &mut self.opaque_cursor, at);
        Ok(if hidden { None } else { Some(open) })
    }
}

struct BlockEnd {
    next: usize,
    span: Option<Range>,
}

fn raw_span(text: &[u16], m: &HtmlOpen, at: usize, line_stop: usize, ends: &mut Ends) -> Range {
    let (a, b) = m.raw.expect("a raw block");
    (at, raw_block_end(text, &lower(&text[a..b]), at, line_stop, ends, m.case_sensitive))
}

#[expect(clippy::too_many_arguments, reason = "the block scanner's line, container chain, ends and budget ticker")]
fn html_block_end(
    text: &[u16],
    at: usize,
    m: &HtmlOpen,
    next: usize,
    nl: Option<usize>,
    line_end: usize,
    chain: &[Container],
    ends: &mut Ends,
    tick: &mut LineTicker<'_>,
) -> Result<BlockEnd> {
    let n = text.len();

    let raw = m.raw.is_some();
    let to_blank = m.name.is_some();
    let end_re =
        if raw { if m.case_sensitive { EndRe::JsxRawBlockEnd } else { EndRe::RawBlockEnd } } else { EndRe::CommentEnd };
    let empty = if raw || to_blank { None } else { empty_comment_close(text, at) };
    let end = if to_blank || empty.is_some() { empty } else { next_match(end_re, text, at + m.len, ends) };

    if chain.is_empty() {
        if to_blank {
            let blank = next_match(EndRe::BlankLine, text, at, ends);
            return Ok(BlockEnd { next: blank.map_or(n, |b| b + 1), span: None });
        }
        let Some(end) = end else { return Ok(BlockEnd { next: n, span: Some((at, n)) }) };
        let stop = end_of_line(text, end);
        let span = if raw { Some(raw_span(text, m, at, content_end(text, at, stop), ends)) } else { None };
        return Ok(BlockEnd { next: stop + 1, span });
    }

    if let Some(e) = end
        && e < line_end
    {
        let span = if raw { Some(raw_span(text, m, at, line_end, ends)) } else { None };
        return Ok(BlockEnd { next, span });
    }

    let mut inside = nl.unwrap_or(n);
    let mut i = next;
    let mut left = false;

    while i < n {
        tick.tick()?;
        let brk = js::index_of_unit(text, LF, i);
        let stop = brk.unwrap_or(n);
        let line_stop = content_end(text, i, stop);
        let line = match_containers(text, i, line_stop, chain);

        if line.matched < chain.len() {
            left = true;
            break;
        }

        let blank = skip_spaces(text, line.at, line_stop) == line_stop;
        if to_blank && blank {
            break;
        }

        i = brk.map_or(n, |b| b + 1);
        if !blank {
            inside = stop;
        }

        if let Some(e) = end
            && e < line_stop
        {
            let span = if raw { raw_span(text, m, at, line_stop, ends) } else { (at + COMMENT_OPEN.len(), e) };
            return Ok(BlockEnd { next: i, span: Some(span) });
        }
    }

    Ok(BlockEnd { next: i, span: if to_blank { None } else { Some((at, if left { inside } else { n })) } })
}

#[cfg(test)]
pub fn scan_unclosed_html_block(
    source: &[u16],
    check_budget: CheckBudget<'_>,
    read_opaque: ReadOpaque<'_>,
) -> Result<Vec<Range>> {
    scan_unclosed_html_block_options(source, check_budget, read_opaque, false, None)
}

fn scan_unclosed_html_block_options(
    source: &[u16],
    check_budget: CheckBudget<'_>,
    read_opaque: ReadOpaque<'_>,
    mdx: bool,
    mut inline: ReadLine<'_>,
) -> Result<Vec<Range>> {
    if !HTML_BLOCK_LINE_RE.test(source) {
        return Ok(Vec::new());
    }

    let text = &line_view(source)[..];
    let n = text.len();
    let mut spans = Vec::new();
    let mut opens = HtmlBlockOpener::new_options(read_opaque, mdx, check_budget);
    let mut inline_through = 0;
    let mut ends = Ends::default();
    let mut chain: Vec<Container> = Vec::new();
    let mut tick = LineTicker::new(check_budget, "md-html-block");
    let mut paragraph_open = false;
    let mut i = 0;

    while i < n {
        tick.tick()?;
        let nl = js::index_of_unit(text, LF, i);
        let line_end = content_end(text, i, nl.unwrap_or(n));
        if inline_through > i {
            inline_through = inline.as_mut().expect("inline reader")(i, line_end)?;
            i = nl.map_or(n, |p| p + 1);
            continue;
        }
        let before = paragraph_open;
        let walk = walk_line(text, i, line_end, &mut chain, before);
        let m = opens.call(text, walk.place.content, !before || walk.fresh)?;
        if m.is_none()
            && let Some(inline) = inline.as_mut()
        {
            inline_through = inline(i, line_end)?;
        }

        paragraph_open = m.is_none() && walk.paragraph_open;
        i = nl.map_or(n, |p| p + 1);

        if let Some(m) = m {
            let block = html_block_end(text, walk.place.content, &m, i, nl, line_end, &chain, &mut ends, &mut tick)?;
            if let Some(span) = block.span {
                spans.push(span);
            }
            i = block.next;
        }
    }

    Ok(spans)
}

fn end_of_line(text: &[u16], at: usize) -> usize {
    js::index_of_unit(text, LF, at).unwrap_or(text.len())
}

pub fn is_markdown_divider_at(text: &[u16], at: usize) -> bool {
    if !matches!(text.get(at), Some(&(DASH | STAR | UNDERSCORE | EQ))) {
        return false;
    }
    let end = end_of_line(text, at);
    is_thematic_break(thematic_tail(text, at, end), at) || setext_underline(text, at, end)
}

fn setext_underline(text: &[u16], i: usize, end: usize) -> bool {
    let Some(c) = text.get(i).copied().filter(|&c| c == EQ || c == DASH) else { return false };
    let mut j = i;
    while j < end && text[j] == c {
        j += 1;
    }
    skip_spaces(text, j, end) == end
}

fn is_delimiter_dash(text: &[u16], j: usize) -> bool {
    matches!(text.get(j).copied(), Some(c) if c == DASH || c == unit(NDASH) || c == unit(MDASH))
}

fn skip_delimiter_spaces(text: &[u16], i: usize, end: usize) -> usize {
    let mut j = i;
    while j < end && matches!(text[j], c if c == SP || c == TAB || c == unit(NBSP) || c == unit(NNBSP)) {
        j += 1;
    }
    j
}

fn delimiter_row(text: &[u16], start: usize, end: usize) -> bool {
    const PIPE: u16 = 0x7C;
    let mut j = skip_delimiter_spaces(text, start, end);
    let mut pipes = 0;
    let mut cells = 0;
    if !is(text, j, PIPE) && !is(text, j, COLON) && !is_delimiter_dash(text, j) {
        return false;
    }
    if is(text, j, PIPE) {
        pipes += 1;
        j = skip_delimiter_spaces(text, j + 1, end);
    }
    while j < end {
        if is(text, j, COLON) {
            j = skip_delimiter_spaces(text, j + 1, end);
        }
        let dashes = j;
        while j < end && is_delimiter_dash(text, j) {
            j += 1;
        }
        if j == dashes {
            return false;
        }
        j = skip_delimiter_spaces(text, j, end);
        if is(text, j, COLON) {
            j = skip_delimiter_spaces(text, j + 1, end);
        }
        cells += 1;
        if j == end {
            break;
        }
        if !is(text, j, PIPE) {
            return false;
        }
        pipes += 1;
        j = skip_delimiter_spaces(text, j + 1, end);
    }
    cells > 0 && pipes > 0
}

fn hard_break_at(text: &[u16], start: usize, end: usize) -> bool {
    let mut j = end;
    while j > start && text[j - 1] == SP {
        j -= 1;
    }
    if j != end {
        return end - j >= 2;
    }
    while j > start && text[j - 1] == BACKSLASH {
        j -= 1;
    }
    (end - j) % 2 == 1
}

fn lines_in(text: &[u16], from: usize, to: usize) -> usize {
    text[from.min(text.len())..to.min(text.len())].iter().filter(|&&c| c == LF).count()
}

fn line_after(text: &[u16], at: usize) -> usize {
    (end_of_line(text, at) + 1).min(text.len())
}

pub struct Prose {
    pub paragraph: Vec<isize>,
    pub closed: Vec<bool>,
    pub hard: Vec<bool>,
}

pub fn prose_paragraphs_options(
    source: &[u16],
    header: bool,
    check_budget: CheckBudget<'_>,
    mdx: bool,
) -> Result<Prose> {
    let text = &line_view(source)[..];
    let n = text.len();
    let lines = lines_in(text, 0, n) + 1;
    let mut paragraph = vec![-1isize; lines];
    let mut hard = vec![false; lines];
    let mut closed: Vec<bool> = Vec::new();
    let mut opens_html = HtmlBlockOpener::new_options(None, mdx, check_budget);
    let mut module_at = if mdx { Some(EsmReader::new(text, check_budget)?) } else { None };
    let mut syntax = if mdx { Some(MdxReader::new(text, check_budget)?) } else { None };
    let mut ends = Ends::default();
    let mut tick = LineTicker::new(check_budget, "md-prose");
    let mut chain: Vec<Container> = Vec::new();
    let header_span = if header { scan_leading_header(text).first().copied() } else { None };
    let mut paragraph_open = false;
    let mut may_define = true;
    let mut table = false;
    let mut current: isize = -1;
    let mut first = 0usize;
    let mut i = header_span.map_or(0, |(_, end)| line_after(text, end));
    let mut line = lines_in(text, 0, i);
    let identity = |s: &[u16]| s.to_vec();

    let close = |current: &mut isize, closed: &mut Vec<bool>, wrapped: bool| {
        if *current != -1 {
            closed[*current as usize] = wrapped;
            *current = -1;
        }
    };

    while i < n {
        tick.tick()?;
        let nl = js::index_of_unit(text, LF, i);
        let next = nl.map_or(n, |p| p + 1);
        let line_end = content_end(text, i, nl.unwrap_or(n));
        let before = paragraph_open;
        let walk = walk_line(text, i, line_end, &mut chain, before);
        let (place, fresh, after) = (walk.place, walk.fresh, walk.paragraph_open);
        let blank = skip_spaces(text, i, line_end) == line_end;
        let fence = fence_open_at(text, place.content, line_end);
        let mut to = next;
        let mut defined = false;

        if let Some(syntax) = syntax.as_mut() {
            let module = if place.content == i { module_at.as_mut().and_then(|m| m.read(text, i)) } else { None };
            let candidate = syntax.next(place.content)?;
            let span =
                if candidate.is_some_and(|c| c.0 == place.content) { Some(syntax.read(place.content)?) } else { None };
            if module.is_some() || span.is_some_and(|s| js::trim(&text[s.1..end_of_line(text, s.1)]).is_empty()) {
                close(&mut current, &mut closed, false);
                to = module.map_or_else(|| line_after(text, span.expect("syntax span").1), |m| m.next);
                line += lines_in(text, i, to);
                i = to;
                paragraph_open = false;
                continue;
            }
        }

        paragraph_open = false;

        if let Some(fence) = fence {
            to = line_after(text, fence_end(text, fence, &chain, nl, &mut tick)?);
            close(&mut current, &mut closed, false);
        } else if js::starts_with_at(text, &[COLON, COLON, COLON], place.content) {
            close(&mut current, &mut closed, false);
        } else {
            let (_, text_column) = skip_indent(text, place.base, line_end, place.base_column);
            let code = !blank && (fresh || !before) && text_column - place.base_column >= TAB_STOP;
            let html = if code || blank { None } else { opens_html.call(text, place.content, !before || fresh)? };
            let definition = if html.is_none() && (may_define || fresh) && !blank {
                let chain = &chain;
                let mut next_line = |stop: usize| -> Result<Option<Range>> { Ok(paragraph_line(text, stop, n, chain)) };
                reference_definition(text, place.content, line_end, &identity, &mut next_line)?
            } else {
                None
            };

            if let Some(m) = &html {
                to = html_block_end(text, place.content, m, next, nl, line_end, &chain, &mut ends, &mut tick)?.next;
                close(&mut current, &mut closed, false);
            } else if blank {
                close(&mut current, &mut closed, true);
                table = false;
            } else if code || !chain.is_empty() || table || definition.is_some() {
                close(&mut current, &mut closed, false);
            } else if !after {
                if current != -1 && setext_underline(text, place.content, line_end) {
                    paragraph[first..line].fill(-1);
                    closed.pop();
                    current = -1;
                } else {
                    close(&mut current, &mut closed, false);
                }
            } else if next < n && delimiter_row(text, next, content_end(text, next, end_of_line(text, next))) {
                close(&mut current, &mut closed, false);
                table = true;
            } else {
                if current == -1 {
                    closed.push(false);
                    current = closed.len() as isize - 1;
                    first = line;
                }
                paragraph[line] = current;
                hard[line] = hard_break_at(text, i, line_end);
            }

            paragraph_open = html.is_none() && after;

            if let Some(d) = definition {
                to = line_after(text, d.end);
                defined = true;
            }
        }

        may_define = defined || !paragraph_open;
        line += lines_in(text, i, to);
        i = to;
    }

    close(&mut current, &mut closed, true);

    Ok(Prose { paragraph, closed, hard })
}

fn comment_end(text: &[u16], at: usize, seen: &mut Ends) -> Option<usize> {
    if let Some(empty) = empty_comment_close(text, at) {
        return Some(empty);
    }
    let close = next_match(EndRe::CommentEnd, text, at + COMMENT_OPEN.len(), seen)?;
    if opens_line(text, at) {
        return Some(close);
    }
    match next_match(EndRe::BlankLine, text, at, seen) {
        Some(blank) if close >= blank => None,
        _ => Some(close),
    }
}

pub fn markdown_comment_closer<'a>(
    source: &'a [u16],
    check: CheckBudget<'a>,
) -> crate::protect::tag_spans::CommentRead<'a> {
    let text = line_view(source);
    let mut ends = None;
    Box::new(move |at| {
        if ends.is_none() {
            let mut result = HashMap::new();
            let mut seen = Ends::default();
            let mut from = 0;
            while let Some(start) = js::index_of(&text, COMMENT_OPEN, from) {
                check("md-comment-ends")?;
                result.insert(start, comment_end(&text, start, &mut seen));
                from = start + 1;
            }
            ends = Some(result);
        }
        Ok(ends.as_ref().and_then(|e| e.get(&at)).copied().flatten())
    })
}

pub fn comment_reader<'a>(source: &[u16], check_budget: CheckBudget<'a>) -> Next<'a> {
    let text = line_view(source);
    let mut seen = Ends::default();
    let mut tag_cursor = 0usize;
    let mut tags: Option<Vec<Span>> = None;
    let mut count = 0usize;

    Box::new(move |from| {
        let mut at = js::index_of(&text, COMMENT_OPEN, from);
        while let Some(a) = at {
            if count & 0x3fff == 0 {
                check_budget("md-comment")?;
            }
            count += 1;

            let tags = tags.get_or_insert_with(|| {
                crate::protect::tag_spans::tag_spans_options(
                    &text,
                    &[],
                    false,
                    Some(markdown_comment_closer(&text, &|_| Ok(()))),
                    crate::protect::tag_spans::Reading::Fragment,
                    &[],
                    &|_| Ok(()),
                )
                .expect("unlimited scanner")
            });
            if inside_tag(&text, tags, &mut tag_cursor, a) {
                at = js::index_of(&text, COMMENT_OPEN, a + COMMENT_OPEN.len());
                continue;
            }

            if let Some(close) = comment_end(&text, a, &mut seen) {
                return Ok(Some((a, close + COMMENT_CLOSE.len(), close + COMMENT_CLOSE.len())));
            }

            if seen.known[EndRe::CommentEnd as usize] == Some(None) {
                return Ok(None);
            }

            at = js::index_of(&text, COMMENT_OPEN, a + COMMENT_OPEN.len());
        }
        Ok(None)
    })
}

pub fn scan_markdown_comments(source: &[u16], check_budget: CheckBudget<'_>) -> Result<Vec<Range>> {
    let mut next = comment_reader(source, check_budget);
    let mut spans = Vec::new();
    let mut comment = next(0)?;

    while let Some((open, close, after)) = comment {
        let start = open + COMMENT_OPEN.len();
        let end = close - COMMENT_CLOSE.len();
        if end >= start {
            spans.push((start, end));
        }
        comment = next(after)?;
    }

    Ok(spans)
}

fn code_reader_for<'a>(
    text: &'a [u16],
    ctx: &'a ScanCtx<'a>,
) -> Result<Box<dyn crate::input::span_path::SpanReader + 'a>> {
    document_code_span_reader(text, ctx.check_budget)
}

pub fn element_reader_for<'a>(
    text: &'a [u16],
    ctx: &'a ScanCtx<'a>,
    mdx: Option<SharedMdx<'a>>,
) -> Result<Box<dyn crate::input::span_path::SpanReader + 'a>> {
    Ok(Box::new(ctx.elements.element_reader(text, ctx, mdx)?))
}

fn line_reader_for<'a>(
    text: &'a [u16],
    ctx: &'a ScanCtx<'a>,
    templates: Option<&ReaderFor>,
    mdx: bool,
) -> Result<crate::input::span_path::LineReader<'a>> {
    let syntax = if mdx {
        Some(std::rc::Rc::new(std::cell::RefCell::new(MdxReader::new(text, ctx.check_budget)?)))
    } else {
        None
    };
    let mut readers = vec![code_reader_for(text, ctx)?];
    if let Some(factory) = templates {
        readers.push(factory(text, ctx)?);
    }
    readers.push(element_reader_for(text, ctx, syntax.clone())?);
    if let Some(syntax) = syntax {
        readers.push(Box::new(MdxOwner(syntax)));
    }
    Ok(crate::input::span_path::LineReader::new(readers))
}

pub fn add_block_scanners(
    blocks: &mut SafeBlocks,
    read_templates: Option<ReaderFor>,
    mdx: bool,
) -> crate::protect::blocks::OpaqueScanner {
    let code: ReaderFor = Arc::new(code_reader_for);
    let elements: OwnerFor = Arc::new(element_reader_for);
    let boundary = opaque_inline(Some(code.clone()), read_templates.clone(), vec![elements.clone()], mdx, false);
    let line_reader = mdx || read_templates.is_some();
    let rt = read_templates.clone();
    let scan = boundary.clone();
    blocks.add_line_block_scanner(
        "md-fence",
        Arc::new(move |text, ctx| {
            let read = || Ok(scan(text, ctx)?.into_iter().map(|(a, b, _)| (a, b)).collect());
            let mut inline = if line_reader { Some(line_reader_for(text, ctx, rt.as_ref(), mdx)?) } else { None };
            let mut line = |a, b| inline.as_mut().expect("inline line reader").read(a, b);
            scan_fences_options(text, ctx.check_budget, Some(&read), mdx, line_reader.then_some(&mut line))
        }),
    );
    let rt = read_templates.clone();
    let scan = boundary.clone();
    blocks.add_line_block_scanner(
        "md-indented-code",
        Arc::new(move |text, ctx| {
            let read = || Ok(scan(text, ctx)?.into_iter().map(|(a, b, _)| (a, b)).collect());
            let mut inline = if line_reader { Some(line_reader_for(text, ctx, rt.as_ref(), mdx)?) } else { None };
            let mut line = |a, b| inline.as_mut().expect("inline line reader").read(a, b);
            scan_indented_code_options(text, ctx.check_budget, Some(&read), mdx, line_reader.then_some(&mut line))
        }),
    );
    let rt = read_templates.clone();
    let scan = boundary;
    blocks.add_line_block_scanner(
        "md-html-block",
        Arc::new(move |text, ctx| {
            let read = || Ok(scan(text, ctx)?.into_iter().map(|(a, b, _)| (a, b)).collect());
            let mut inline = if line_reader { Some(line_reader_for(text, ctx, rt.as_ref(), mdx)?) } else { None };
            let mut line = |a, b| inline.as_mut().expect("inline line reader").read(a, b);
            scan_unclosed_html_block_options(text, ctx.check_budget, Some(&read), mdx, line_reader.then_some(&mut line))
        }),
    );
    let scan_inline = opaque_inline(Some(code), read_templates, vec![elements], mdx, true);
    let inline = scan_inline.clone();
    blocks.add_scanner(
        "md-reference-definition",
        Arc::new(move |text, ctx| {
            scan_reference_definitions(
                text,
                ctx.check_budget,
                ctx.resolve,
                Some(&|t| Ok(inline(t, ctx)?.into_iter().map(|(a, b, _)| (a, b)).collect())),
            )
        }),
    );
    blocks.add_inline_scanner("md-inline-opaque", scan_inline.clone());
    scan_inline
}

pub fn add_inline_scanners(
    blocks: &mut SafeBlocks,
    math: bool,
    pragmas: Option<crate::protect::blocks::DocumentScanner>,
) {
    blocks.add_opaque_scanner(
        "md-link-destination",
        Arc::new(|text: &[u16], ctx: &ScanCtx<'_>| {
            scan_link_destinations_resolved(text, ctx.check_budget, ctx.resolve)
        }),
    );
    if math {
        blocks.add_scanner("md-math", Arc::new(|text, ctx| crate::input::math::scan_math(text, ctx.check_budget)));
    }
    if let Some(scan) = pragmas {
        blocks.add_document_scanner("microtypo-pragma", scan);
    }
    blocks
        .add_opaque_scanner("md-table-delimiter", Arc::new(|text, ctx| scan_table_delimiters(text, ctx.check_budget)));
    blocks.add_opaque_scanner(
        "md-line-prefix",
        Arc::new(|text: &[u16], ctx: &ScanCtx<'_>| scan_line_prefix(text, ctx.check_budget)),
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::errors::ErrorKind;
    use crate::js::u;
    use crate::protect::blocks::ElementSet;

    fn ok(_: &str) -> Result<()> {
        Ok(())
    }

    fn same(s: &[u16]) -> JsString {
        s.to_vec()
    }

    fn fences(s: &str) -> Vec<Range> {
        scan_fences(&u(s), &ok, None).unwrap()
    }

    fn indented(s: &str) -> Vec<Range> {
        scan_indented_code(&u(s), &ok, None).unwrap()
    }

    fn html(s: &str) -> Vec<Range> {
        scan_unclosed_html_block(&u(s), &ok, None).unwrap()
    }

    fn refs(s: &str) -> Vec<Range> {
        scan_reference_definitions(&u(s), &ok, &same, None).unwrap()
    }

    #[test]
    fn fenced_blocks() {
        assert_eq!(fences("a\n```js\ncode\n```\nb"), [(2, 16)]);
        assert_eq!(fences("> ```\n> x\n\ny"), [(0, 9)]);
        assert_eq!(fences("- ~~~\n  a\n\n  b\n  ~~~\nz"), [(0, 20)]);
        assert_eq!(fences("```\nunclosed\nmore"), [(0, 17)]);
        assert_eq!(fences("<div>\n```\n</div>\n\n```\nx\n```"), [(18, 27)]);
    }

    #[test]
    fn indented_code() {
        assert_eq!(indented("para\n    lazy\n\n    code\n\n    more\n\ntext"), [(15, 33)]);
        assert_eq!(indented("- item\n\n      code\n\tx"), [(10, 18)]);
        assert_eq!(indented("a\r\n\r\n    crlf\r\n"), [(5, 13)]);
    }

    #[test]
    fn html_blocks() {
        assert_eq!(html("<div>\nx\n\ny"), [] as [(usize, usize); 0]);
        assert_eq!(html("<pre>\na\n</pre> tail\nz"), [(0, 14)]);
        assert_eq!(html("<!-- open\nno close"), [(0, 18)]);
        assert_eq!(html("> <!-- a\n> b -->\nz"), [(6, 13)]);
        assert_eq!(html("> <script>\n> x\nlazy"), [(2, 14)]);
        assert_eq!(html("text\n<span a=\"1\">\n"), [] as [(usize, usize); 0]);
        let opaque = || Ok(vec![(0, 3)]);
        assert_eq!(
            scan_unclosed_html_block(&u("<x-tag>\nb\n"), &ok, Some(&opaque)).unwrap(),
            [] as [(usize, usize); 0]
        );
    }

    #[test]
    fn code_spans() {
        assert_eq!(scan_code_spans(&u("a `b` c ``d ` e`` \\`f` g\n\n`h"), &ok).unwrap(), [(3, 4), (10, 15)]);
        assert_eq!(scan_code_spans(&u("`a\n\n` ```x``` `"), &ok).unwrap(), [(5, 14)]);
    }

    #[test]
    fn reference_definitions() {
        assert_eq!(
            refs("[Foo Bar]: /url \"Title\"\n\nSee [foo  bar] and [x][FOO BAR] and [y](z) `[foo bar]`"),
            [(0, 23), (30, 38), (48, 55)]
        );
        assert_eq!(
            refs("para\n[a]: /u\n\n[b]:\n  <x y>\n  (t\n  t)\n[b]\n> [c\n> d]: e\n[C D]"),
            [(14, 36), (38, 39), (43, 53), (55, 58)]
        );
        assert_eq!(refs("[^n]: note\n[ß]: /s\n[SS] [ẞ]"), [] as [(usize, usize); 0]);
        assert_eq!(refs("[ß]: /s\n\n[SS] [ẞ] [x][ss]"), [(0, 7), (10, 12), (15, 16), (22, 24)]);
    }

    #[test]
    fn opaque_inline_lets_a_comment_own_backticks() {
        let elements = ElementSet::default();
        let ctx = ScanCtx { check_budget: &ok, resolve: &same, elements: &elements, code_edges: false };
        let scan =
            opaque_inline(Some(Arc::new(code_reader_for)), None, vec![Arc::new(element_reader_for)], false, true);
        assert_eq!(
            scan(&u("a `b` <!-- `c --> d` `e`"), &ctx).unwrap(),
            [(3, 4, SpanKind::Plain), (20, 21, SpanKind::Plain)]
        );
    }

    #[test]
    fn link_destinations() {
        let text = u("[a](b) [c](<d e> \"t\") [f](g(h)i) [j]( k 'l' ) [m](n o)");
        assert_eq!(scan_link_destinations(&text, &ok).unwrap(), [(4, 5), (11, 20), (26, 31), (37, 44)]);
    }

    #[test]
    fn link_destination_over_the_bound() {
        let long = format!("[a]({})", "x".repeat(9000));
        let error = scan_link_destinations(&u(&long), &ok).unwrap_err();
        assert_eq!(error.kind, ErrorKind::Input);
        assert_eq!(error.message, "Markdown link destination/title exceeds the 8192-char scan bound at offset 2");
        assert_eq!(error.details, json!({ "offset": 2, "max": 8192, "reason": "link-overlimit" }));
        let unclosed = format!("[a]({}", "x".repeat(9000));
        assert_eq!(scan_link_destinations(&u(&unclosed), &ok).unwrap(), [] as [(usize, usize); 0]);
    }

    #[test]
    fn line_prefixes() {
        let text = u("> - a\n  - b\n    \n# h\n1. x\n10) y\n- - -\n\t> q\n#nope\n   ");
        assert_eq!(
            scan_line_prefix(&text, &ok).unwrap().into_iter().map(|(a, b, _)| (a, b)).collect::<Vec<_>>(),
            [(0, 4), (6, 10), (17, 19), (21, 24), (26, 30), (32, 37), (38, 41)]
        );
    }

    #[test]
    fn comments() {
        let text = u("a <!-- b --> c <!--> d <!---> <x a=\"<!--\"> <!-- e\n\nf --> <!--\ng-->");
        assert_eq!(scan_markdown_comments(&text, &ok).unwrap(), [(6, 9), (61, 63)]);
        assert_eq!(scan_markdown_comments(&u("<!-- open"), &ok).unwrap(), [] as [(usize, usize); 0]);
    }
}
