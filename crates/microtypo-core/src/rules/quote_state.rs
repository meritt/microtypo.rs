use std::borrow::Cow;
use std::collections::{HashMap, HashSet};
use std::fmt;
use std::sync::LazyLock;

use crate::errors::Result;
use crate::js::{self, JsString, Re, is_high, is_low};
use crate::protect::blocks::SafeBlocks;
use crate::protect::placeholders::{
    BLOCK_KIND, CLOSE, CLOSE_UNIT, OPEN, OPEN_UNIT, PARAGRAPH_CLOSE, TAG_PLACEHOLDER_SOURCE, restore_paragraphs,
};
use crate::protect::tags::{NameMatcher, SafeTags};
use crate::shared::boundaries::{
    dash_quote_punctuation, is_quote_dash, is_strong_left, is_weak_left, opening_quote_neighbours,
};
use crate::shared::character_reference::{
    character_reference_ending_at, quote_boundary_glyph, read_character_reference_at,
};
use crate::shared::glyphs::{BDQUO, HELLIP, LAQUO, LDQUO, LSQUO, NBSP, PRIME, RAQUO, RSQUO, unit};
use crate::shared::inline_boundaries::{
    inline_end_tag_ends_at, inline_text_end, inline_text_start, native_tag_end, native_tag_start, token_tag_raw,
};
use crate::shared::paragraph_boundaries::PARAGRAPH_SPLIT_RE;
use crate::shared::quote_pairs::{RDQUO_U, resolve_authored_quote_pairs};
use crate::shared::strings::line_view;

const LAQUO_U: u16 = unit(LAQUO);
const RAQUO_U: u16 = unit(RAQUO);
const BDQUO_U: u16 = unit(BDQUO);
const LDQUO_U: u16 = unit(LDQUO);
const PRIME_U: u16 = unit(PRIME);
const NBSP_U: u16 = unit(NBSP);
const RSQUO_U: u16 = unit(RSQUO);
const LSQUO_U: u16 = unit(LSQUO);
const QUOT: u16 = 0x22;
const LT: u16 = 0x3C;
const GT: u16 = 0x3E;
const BLOCK_KIND_U: u16 = BLOCK_KIND as u16;

pub const SEPARATOR: &str = r" \t\r\n";

pub fn is_separator(c: u16) -> bool {
    matches!(c, 0x20 | 0x09 | 0x0D | 0x0A)
}

pub struct EdgeView<'a> {
    pub reader: &'a mut crate::rules::text::EdgeReader,
    pub blocks: &'a SafeBlocks,
    pub sequences: &'a crate::protect::sequences::SafeSequences,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct InlinePadding {
    pub trim_before_punctuation: bool,
    pub space_after_comma: bool,
}

#[derive(Default)]
pub struct QuoteTags<'a> {
    safe_tags: Option<&'a mut SafeTags>,
    paragraphs: bool,
    names: bool,
    reference_names: bool,
    raws: bool,
    lines: Option<&'a SafeBlocks>,
    padding: Option<InlinePadding>,
    references: bool,
    breaks: Option<&'a mut Option<HashSet<JsString>>>,
    edges: Option<EdgeView<'a>>,
    budget: Budget<'a>,
}

impl<'a> QuoteTags<'a> {
    #[allow(clippy::too_many_arguments)]
    pub fn session(
        safe_tags: &'a mut SafeTags,
        paragraphs: bool,
        names: bool,
        lines: Option<&'a SafeBlocks>,
        padding: InlinePadding,
        breaks: &'a mut Option<HashSet<JsString>>,
        edges: Option<EdgeView<'a>>,
        budget: Budget<'a>,
    ) -> Self {
        QuoteTags {
            safe_tags: Some(safe_tags),
            paragraphs,
            names,
            reference_names: true,
            raws: true,
            lines,
            padding: Some(padding),
            references: true,
            breaks: Some(breaks),
            edges,
            budget,
        }
    }

    fn paragraphs(&mut self) -> Option<&mut SafeTags> {
        if self.paragraphs { self.safe_tags.as_deref_mut() } else { None }
    }

    pub(crate) fn names(&mut self) -> Option<&mut SafeTags> {
        if self.names { self.safe_tags.as_deref_mut() } else { None }
    }

    fn reference_names(&mut self) -> Option<&mut SafeTags> {
        if self.reference_names { self.safe_tags.as_deref_mut() } else { None }
    }

    pub(crate) fn raw(&self, token: &[u16]) -> Option<&[u16]> {
        if self.raws { self.safe_tags.as_deref().and_then(|tags| token_tag_raw(tags, token)) } else { None }
    }

    pub(crate) fn raw_comment(&self, token: &[u16]) -> bool {
        self.raw(token).is_some_and(|raw| raw.starts_with(&[0x21, 0x2D, 0x2D]))
    }

    pub(crate) fn check(&self, label: &str) -> Result<()> {
        self.budget.map_or(Ok(()), |check| check(label))
    }

    pub(crate) fn budget(&self) -> Budget<'a> {
        self.budget
    }

    pub(crate) fn set_paragraphs(&mut self, on: bool) -> bool {
        std::mem::replace(&mut self.paragraphs, on)
    }

    pub(crate) fn label_address_before(&self, text: &[u16], end: usize) -> bool {
        self.lines.is_some_and(|blocks| blocks.label_address_before(text, end))
    }

    pub(crate) fn text_end(&mut self, text: &[u16], at: usize) -> usize {
        inline_text_end(self.reference_names(), text, at)
    }

    pub(crate) fn text_end_visit(
        &mut self,
        text: &[u16],
        at: usize,
        visit: &mut dyn FnMut() -> Result<()>,
    ) -> Result<usize> {
        crate::shared::inline_boundaries::inline_text_end_visit(self.reference_names(), text, at, visit)
    }

    pub(crate) fn text_start(&mut self, text: &[u16], at: usize) -> usize {
        inline_text_start(self.reference_names(), text, at)
    }

    fn block_at(&self, text: &[u16], at: usize, after_break: bool) -> bool {
        self.lines.is_some_and(|blocks| {
            blocks.line_block_at(text, at, after_break)
                || (after_break && crate::input::markdown::is_markdown_divider_at(text, at))
        })
    }

    fn line_break_tokens(&mut self) -> HashSet<JsString> {
        let Some(tags) = (if self.paragraphs { self.safe_tags.as_deref_mut() } else { None }) else {
            return HashSet::new();
        };
        if let Some(Some(cached)) = self.breaks.as_deref() {
            return cached.clone();
        }
        let tokens: HashSet<JsString> = tags
            .find_by_tag_name(&NameMatcher::Name(&js::u("br")))
            .into_iter()
            .map(|(prefix, id)| crate::cat!(OPEN, prefix, js::num(id), CLOSE))
            .collect();
        if let Some(cache) = self.breaks.as_deref_mut() {
            *cache = Some(tokens.clone());
        }
        tokens
    }

    fn reference_ending(&self, text: &[u16], semicolon_at: usize) -> Option<Option<JsString>> {
        self.references.then(|| {
            quote_boundary_glyph(character_reference_ending_at(text, semicolon_at, None).ok().flatten().as_ref())
        })
    }

    fn reference_starting(&self, text: &[u16], at: usize) -> Option<Option<JsString>> {
        self.references.then(|| quote_boundary_glyph(read_character_reference_at(text, at).as_ref()))
    }
}

fn single(glyph: &[u16]) -> Option<u16> {
    glyph.first().copied()
}

pub fn quote_content_at(text: &[u16], at: usize, cx: &mut QuoteTags<'_>) -> bool {
    let visible = inline_text_start(cx.names(), text, at);
    content_at(text, visible, cx)
}

fn content_at(text: &[u16], visible: usize, cx: &QuoteTags<'_>) -> bool {
    if text.get(visible) == Some(&OPEN_UNIT) && cx.block_at(text, visible, false) {
        return false;
    }
    if visible >= text.len() || is_separator(text[visible]) {
        return false;
    }
    if text[visible] != LT {
        return true;
    }
    if text.get(visible + 1) != Some(&0x2F) {
        return !(text.get(visible + 1) == Some(&OPEN_UNIT)
            && text.get(visible + 2) == Some(&BLOCK_KIND_U)
            && cx.paragraphs
            && cx.safe_tags.is_some());
    }
    native_tag_end(text, visible).is_none()
}

pub fn opening_content_start(text: &[u16], at: usize, cx: &mut QuoteTags<'_>, points: bool) -> Result<Option<usize>> {
    let markdown = cx.lines;
    let mut next = at;
    let mut crossed_source_line = false;
    let mut crossed_br = false;
    let mut budget_at = at + 0x4000;
    let mut line_start = false;
    let mut skip_padding = false;
    while next < text.len() {
        if next >= budget_at {
            cx.check("quote:content")?;
            budget_at = next + 0x4000;
        }
        if skip_padding {
            while matches!(text.get(next), Some(&(0x20 | 0x09))) {
                next += 1;
            }
        }
        if let Some(blocks) = markdown
            && text.get(next) == Some(&OPEN_UNIT)
        {
            let end = blocks.horizontal_space_end(text, next);
            if end != next {
                next = end;
                skip_padding = true;
                continue;
            }
        }
        if line_start && cx.block_at(text, next, true) {
            return Ok(None);
        }
        let visible = inline_text_start(cx.names(), text, next);
        if visible != next {
            line_start = false;
            next = match cx.padding {
                Some(active) => crate::rules::space::punctuation_after_inline_gap(text, visible, active, points),
                None => visible,
            };
        }
        let tag_end = if text.get(next) == Some(&LT) { native_tag_end(text, next) } else { None };
        if let Some(end) = tag_end
            && cx.raw_comment(js::slice(text, next + 1, end - 1))
        {
            if line_start && markdown.is_some() {
                return Ok(None);
            }
            next = end;
            skip_padding = true;
            continue;
        }
        let br = tag_end.is_some() && line_break_at(text, next, cx);
        if br {
            next = tag_end.expect("break tag end");
        }
        let physical = matches!(text.get(next), Some(&(0x0A | 0x0D)));
        if !br && !physical {
            return Ok(content_at(text, next, cx).then_some(next));
        }
        if (physical && crossed_source_line) || (br && crossed_br) {
            return Ok(None);
        }
        if physical {
            let carriage = text[next] == 0x0D;
            next += 1;
            if carriage && text.get(next) == Some(&0x0A) {
                next += 1;
            }
        }
        crossed_source_line |= physical;
        crossed_br |= br;
        line_start = physical;
        skip_padding = true;
    }
    Ok(None)
}

pub fn opening_content_at(text: &[u16], at: usize, cx: &mut QuoteTags<'_>, points: bool) -> Result<bool> {
    Ok(opening_content_start(text, at, cx, points)?.is_some())
}

const SCAN_LIMIT: usize = 32;

const FAR: i32 = 1 << 29;

static PARAGRAPH_TAG_RE: LazyLock<Re> = LazyLock::new(|| Re::new(&TAG_PLACEHOLDER_SOURCE, "g"));
static QUOTE_PARAGRAPH_TOKEN_RE: LazyLock<Re> = LazyLock::new(|| Re::new("\u{E100}PCL\u{E101}", "g"));

const TOKEN_MAX: usize = 16;

fn line_break_at(text: &[u16], at: usize, cx: &mut QuoteTags<'_>) -> bool {
    if text.get(at + 1) != Some(&OPEN_UNIT) {
        return false;
    }
    let breaks = cx.line_break_tokens();
    if breaks.is_empty() {
        return false;
    }
    let mut length = 1;
    let mut i = at + 2;
    while i < text.len() && length < TOKEN_MAX {
        length += 1;
        if text[i] == CLOSE_UNIT {
            return text.get(i + 1) == Some(&GT) && breaks.contains(&text[at + 1..=i]);
        }
        i += 1;
    }
    false
}

static INDENT_RE: LazyLock<Re> = LazyLock::new(|| Re::new(&format!(r"{OPEN}W\d{{1,9}}{CLOSE}"), "uy"));

fn is_quote_mark_ahead(c: u16) -> bool {
    matches!(c, QUOT | LAQUO_U | RAQUO_U | BDQUO_U | LDQUO_U | RDQUO_U)
}

fn parted_at_blocks(
    text: &[u16],
    ranges: Vec<(usize, usize)>,
    edges_of: &mut dyn FnMut(&[u16]) -> Result<Vec<usize>>,
) -> Result<Vec<(usize, usize)>> {
    let mut parted = Vec::with_capacity(ranges.len());
    let mut edges: Option<Vec<usize>> = None;
    let mut e = 0;
    let mut break_at: Option<usize> = None;
    let mut quote_at: Option<usize> = None;
    for (from, to) in ranges {
        if break_at.is_none_or(|b| b < from) {
            break_at = Some(js::index_of_unit(text, 0x0A, from).unwrap_or(text.len()));
        }
        let break_pos = break_at.expect("break position");
        if break_pos < to && quote_at.is_none_or(|q| q < from) {
            quote_at = Some((from..text.len()).find(|&i| is_quote_mark_ahead(text[i])).unwrap_or(text.len()));
        }
        if break_pos >= to || quote_at.is_none_or(|q| q >= to) {
            parted.push((from, to));
            continue;
        }
        if edges.is_none() {
            edges = Some(edges_of(text)?);
        }
        let edges = edges.as_deref().unwrap_or_default();
        let mut cursor = from;
        while e < edges.len() && edges[e] <= from {
            e += 1;
        }
        while e < edges.len() && edges[e] < to {
            parted.push((cursor, edges[e]));
            cursor = edges[e];
            e += 1;
        }
        parted.push((cursor, to));
    }
    Ok(parted)
}

fn tick(seen: &mut u32, check_budget: Budget<'_>) -> Result<()> {
    if let Some(cb) = check_budget {
        *seen = seen.wrapping_add(1);
        if *seen & 1023 == 0 {
            cb("quote:paragraphs")?;
        }
    }
    Ok(())
}

pub fn quote_paragraphs(text: &[u16], cx: &mut QuoteTags<'_>, check_budget: Budget<'_>) -> Result<Vec<(usize, usize)>> {
    let ranges = paragraph_ranges(text, cx, check_budget)?;
    if !cx.paragraphs {
        return Ok(ranges);
    }
    let budget = cx.budget;
    let (Some(view), Some(tags)) = (cx.edges.as_mut(), cx.safe_tags.as_deref()) else {
        return Ok(ranges);
    };
    let EdgeView { reader, blocks, sequences } = view;
    let check: &dyn Fn(&str) -> Result<()> = budget.unwrap_or(&|_| Ok(()));
    let mut restore = |line: &[u16]| {
        crate::rule_runner::restore_protected_with(tags, blocks, sequences, restore_paragraphs(line.to_vec()), false)
    };
    parted_at_blocks(text, ranges, &mut |text| crate::rules::text::paragraph_edges(reader, text, &mut restore, check))
}

fn paragraph_ranges(text: &[u16], cx: &mut QuoteTags<'_>, check_budget: Budget<'_>) -> Result<Vec<(usize, usize)>> {
    let mut plain = Vec::new();
    let mut seen = 0u32;
    let lines: Cow<'_, [u16]> = if text.contains(&0x0D) {
        let mut view = line_view(text);
        for c in &mut view {
            if *c == 0x0D {
                *c = 0x20;
            }
        }
        Cow::Owned(view)
    } else {
        Cow::Borrowed(text)
    };
    let gap = if js::includes(text, &PARAGRAPH_CLOSE) { &*QUOTE_PARAGRAPH_TOKEN_RE } else { &*PARAGRAPH_SPLIT_RE };
    for m in gap.find_all(&lines) {
        tick(&mut seen, check_budget)?;
        plain.push((m.index(), m.end()));
    }
    let has_open = text.contains(&OPEN_UNIT);
    let breaks: HashSet<JsString> = if has_open { cx.line_break_tokens() } else { HashSet::new() };
    let mut markup: Vec<(usize, usize)> = Vec::new();
    if cx.paragraphs().is_some() && has_open {
        let mut previous: Option<(usize, usize)> = None;
        let mut chain: Option<usize> = None;
        for m in PARAGRAPH_TAG_RE.find_all(text) {
            tick(&mut seen, check_budget)?;
            let token = m.s(2);
            let is_break = breaks.contains(token);
            if token.get(1) == Some(&(BLOCK_KIND as u16)) && !is_break {
                markup.push((m.index(), m.end()));
            }
            if !m.s(1).is_empty() || !is_break {
                previous = None;
                chain = None;
                continue;
            }
            let mut empty = false;
            if let Some((start, from)) = previous {
                empty = true;
                let mut at = from;
                while at < m.index() {
                    if let Some(cb) = check_budget
                        && at & 1023 == 0
                    {
                        cb("quote:paragraphs")?;
                    }
                    if let Some(indent) = INDENT_RE.exec_at(text, at) {
                        at = indent.end();
                        continue;
                    }
                    if !is_separator(text[at]) {
                        empty = false;
                        break;
                    }
                    at += 1;
                }
                if empty {
                    if let Some(k) = chain {
                        markup[k].1 = m.end();
                    } else {
                        chain = Some(markup.len());
                        markup.push((start, m.end()));
                    }
                }
            }
            if !empty {
                chain = None;
            }
            previous = Some((m.index(), m.end()));
        }
    }
    let mut ranges = Vec::new();
    let mut cursor = 0;
    let (mut a, mut b) = (0, 0);
    while a < plain.len() || b < markup.len() {
        let boundary = if b >= markup.len() || (a < plain.len() && plain[a].0 <= markup[b].0) {
            a += 1;
            plain[a - 1]
        } else {
            b += 1;
            markup[b - 1]
        };
        if boundary.0 >= cursor {
            ranges.push((cursor, boundary.0));
        }
        cursor = cursor.max(boundary.1);
    }
    ranges.push((cursor, text.len()));
    Ok(ranges)
}

fn map_quote_paragraphs<'a>(
    text: &[u16],
    tags: &mut QuoteTags<'a>,
    check_budget: Budget<'_>,
    mut transform: impl FnMut(&[u16], Option<u16>, &mut QuoteTags<'a>) -> Result<JsString>,
) -> Result<JsString> {
    let ranges = quote_paragraphs(text, tags, check_budget)?;
    let mut out = Vec::with_capacity(text.len());
    let mut cursor = 0;
    for (from, to) in ranges {
        out.extend_from_slice(js::slice(text, cursor, from));
        out.extend_from_slice(&transform(js::slice(text, from, to), text.get(to).copied(), tags)?);
        cursor = to;
    }
    out.extend_from_slice(js::slice(text, cursor, text.len()));
    Ok(out)
}

pub type Budget<'a> = Option<&'a dyn Fn(&str) -> Result<()>>;
pub type Kept<'a> = &'a dyn Fn(&[u16], usize) -> bool;
pub type OpenedBehind<'a> = &'a dyn Fn(&[u16], usize, &mut QuoteTags<'_>) -> Result<Option<JsString>>;

pub struct QuoteOptions<'a> {
    pub allow_nested: bool,
    pub convert_inches: bool,
    pub strips_padding: bool,
    pub pair_remainder: bool,
    pub check_budget: Budget<'a>,
    pub kept_closer: Option<Kept<'a>>,
    pub opened_behind: Option<OpenedBehind<'a>>,
}

impl Default for QuoteOptions<'_> {
    fn default() -> Self {
        QuoteOptions {
            allow_nested: true,
            convert_inches: true,
            strips_padding: false,
            pair_remainder: false,
            check_budget: None,
            kept_closer: None,
            opened_behind: None,
        }
    }
}

static WORD_BEFORE_RE: LazyLock<Re> = LazyLock::new(|| Re::new("[a-zа-яё0-9]", "iu"));
static CYRILLIC_AFTER_RE: LazyLock<Re> = LazyLock::new(|| Re::new("[а-яё]", "iu"));

fn is_padding(c: u16) -> bool {
    c == 0x20 || c == 0x09 || c == NBSP_U
}

fn word_before(c: Option<u16>) -> bool {
    c.is_some_and(|c| WORD_BEFORE_RE.test(&[c]))
}

fn kept_glued(buf: &[u16], at: usize) -> bool {
    word_before(at.checked_sub(1).map(|i| buf[i])) && buf.get(at + 1).is_some_and(|&c| CYRILLIC_AFTER_RE.test(&[c]))
}

fn nothing_kept(_: &[u16], _: usize) -> bool {
    false
}

fn next_quote(buf: &[u16], off: usize) -> Option<(usize, u16)> {
    (off..buf.len()).find(|&i| buf[i] == LAQUO_U || buf[i] == RAQUO_U).map(|i| (i, buf[i]))
}

fn opener_left(c: u16) -> bool {
    is_weak_left(c) || c == CLOSE_UNIT || is_strong_left(c)
}

fn is_any_quote(c: u16) -> bool {
    c == QUOT || c == LAQUO_U || c == RAQUO_U || c == BDQUO_U || c == LDQUO_U || c == RDQUO_U
}

fn closer_where_opener_stands(text: &[u16], high: bool) -> bool {
    let closers: &[u16] = if high { &[RAQUO_U, RDQUO_U] } else { &[RAQUO_U] };
    closers.iter().any(|&closer| (0..text.len()).any(|at| text[at] == closer && (at == 0 || opener_left(text[at - 1]))))
}

fn turn_wrong_way_opener(buf: &mut [u16], cx: &mut QuoteTags<'_>) -> Result<()> {
    if let Some(at) = buf.iter().position(|&c| is_any_quote(c))
        && (buf[at] == RAQUO_U || buf[at] == RDQUO_U)
        && opens_here(buf, at, cx, None)?
    {
        buf[at] = LAQUO_U;
    }
    Ok(())
}

fn high_to_angle(mut text: JsString) -> JsString {
    for c in &mut text {
        if *c == RDQUO_U {
            *c = RAQUO_U;
        }
    }
    text
}

pub fn canonical_quote_roles(
    text: JsString,
    tags: &mut QuoteTags<'_>,
    check_budget: Budget<'_>,
    turns_openers: bool,
) -> Result<JsString> {
    let high = text.iter().any(|&c| c == BDQUO_U || c == LDQUO_U || c == RDQUO_U);
    let nested = high && text.iter().any(|&c| c == BDQUO_U || c == LDQUO_U);
    let turns = turns_openers && closer_where_opener_stands(&text, high);
    if !nested && !turns {
        return Ok(if high { high_to_angle(text) } else { text });
    }
    map_quote_paragraphs(&text, tags, check_budget, |chunk, _, tags| {
        if !nested {
            let first = chunk.iter().position(|&c| is_any_quote(c)).map(|i| chunk[i]);
            if first != Some(RAQUO_U) && first != Some(RDQUO_U) {
                return Ok(if high { high_to_angle(chunk.to_vec()) } else { chunk.to_vec() });
            }
        }
        let mut buf = chunk.to_vec();
        if turns {
            turn_wrong_way_opener(&mut buf, tags)?;
        }
        if !nested {
            return Ok(high_to_angle(buf));
        }
        resolve_authored_quote_pairs(
            &mut buf,
            &mut |buf, i, after_low| authored_opening_at(buf, i, after_low, tags, None),
            false,
            None,
        )?;
        Ok(buf)
    })
}

pub fn process_quotes(text: &[u16], options: &QuoteOptions<'_>, tags: &mut QuoteTags<'_>) -> Result<JsString> {
    if !text.contains(&LAQUO_U) && !text.contains(&RAQUO_U) {
        return Ok(text.to_vec());
    }
    let previous = options.kept_closer.unwrap_or(&nothing_kept);
    let kept = |buf: &[u16], at: usize| kept_glued(buf, at) || previous(buf, at);
    map_quote_paragraphs(text, tags, options.check_budget, |chunk, follower, tags| {
        let inches = options.convert_inches.then(|| InchReader {
            padded: options.strips_padding,
            follower,
            pair_remainder: options.pair_remainder,
            allow_nested: options.allow_nested,
            has_division: options.pair_remainder && chunk.windows(2).any(|w| w == [QUOT, QUOT]),
            check_budget: options.check_budget,
            ends: None,
            gaps: Vec::new(),
        });
        process_chunk(chunk, options.allow_nested, inches, options.check_budget, &kept, tags, options.opened_behind)
    })
}

pub static SPACED_CLOSER_LEFT: LazyLock<String> = LazyLock::new(|| format!(r"(?:[a-zа-яё0-9{HELLIP}!?%)]|\.{{3}})"));
static SPACED_CLOSER_LEFT_RE: LazyLock<Re> = LazyLock::new(|| Re::new(&format!("{}$", *SPACED_CLOSER_LEFT), "iu"));
static SPACE_RE: LazyLock<Re> = LazyLock::new(|| Re::new(r"\s", "u"));

fn is_space(c: Option<u16>) -> bool {
    c.is_some_and(|c| SPACE_RE.test(&[c]))
}

fn before_quote_padding(buf: &[u16], at: usize, padded: bool) -> Option<usize> {
    let mut i = at.checked_sub(1)?;
    if padded {
        while is_padding(buf[i]) {
            i = i.checked_sub(1)?;
        }
    }
    Some(i)
}

pub fn digit_before_quote(buf: &[u16], at: usize, padded: bool) -> Option<usize> {
    before_quote_padding(buf, at, padded).filter(|&i| is_digit(Some(buf[i])))
}

static TITLE_WORD_START: LazyLock<Re> = LazyLock::new(|| Re::new(r"[\p{L}\p{N}]", "u"));
pub static TITLE_WORD_CHAR: LazyLock<Re> = LazyLock::new(|| Re::new(r"[\p{L}\p{M}\p{N}]", "u"));
static TITLE_CONTENT_END: LazyLock<Re> =
    LazyLock::new(|| Re::new(r"[\p{L}\p{M}\p{N}\p{S}\p{Pe}\p{Pf}\p{Terminal_Punctuation}]", "u"));
pub static TITLE_CONTENT_CHAR: LazyLock<Re> = LazyLock::new(|| Re::new(r"[\p{L}\p{M}\p{N}\p{P}\p{S}]", "u"));
pub static QUOTATION_MARK_RE: LazyLock<Re> = LazyLock::new(|| Re::new(r"\p{Quotation_Mark}", "u"));

pub fn scalar_at(buf: &[u16], at: usize) -> &[u16] {
    match buf.get(at) {
        None => &[],
        Some(&c) if is_high(c) && buf.get(at + 1).is_some_and(|&d| is_low(d)) => &buf[at..at + 2],
        Some(_) => &buf[at..=at],
    }
}

pub fn scalar_before(buf: &[u16], end: usize) -> &[u16] {
    if end == 0 || end > buf.len() {
        return &[];
    }
    if is_low(buf[end - 1]) && end >= 2 && is_high(buf[end - 2]) { &buf[end - 2..end] } else { &buf[end - 1..end] }
}

fn is_glyph(glyph: &[u16], c: u16) -> bool {
    glyph.len() == 1 && glyph[0] == c
}

fn divides_content(buf: &[u16], at: usize) -> bool {
    if at == 0 || buf[at - 1] != QUOT {
        return false;
    }
    let before = scalar_before(buf, at - 1);
    let after = scalar_at(buf, at + 1);
    let single = (before.len() == 1).then(|| before[0]);
    !before.is_empty()
        && !is_glyph(before, QUOT)
        && !single.is_some_and(|c| is_quote_mark(c) || is_digit(Some(c)))
        && TITLE_CONTENT_CHAR.test(before)
        && !after.is_empty()
        && !is_glyph(after, QUOT)
        && TITLE_CONTENT_CHAR.test(after)
        && (!QUOTATION_MARK_RE.test(after) || TITLE_WORD_START.test(scalar_at(buf, at + 1 + after.len())))
}

fn ends_title_walk(c: u16) -> bool {
    c == 0x0A || c == 0x0D
}

fn title_tick(i: usize, check_budget: Budget<'_>) -> Result<()> {
    match check_budget {
        Some(check) if i & 1023 == 0 => check("quote:followingTitle"),
        _ => Ok(()),
    }
}

fn divide_titles(buf: &mut [u16], owners: &Ends, padded: bool, check_budget: Budget<'_>) -> Result<()> {
    let mut closers: Vec<Option<u16>> = Vec::new();
    let mut i = buf.len();
    while i > 0 {
        i -= 1;
        title_tick(i, check_budget)?;
        let c = buf[i];
        if c == RAQUO_U || c == LDQUO_U {
            let content = before_quote_padding(buf, i, padded).map_or(&[][..], |j| scalar_before(buf, j + 1));
            closers.push(
                (owners.has(i) && TITLE_CONTENT_CHAR.test(content) && !TITLE_WORD_CHAR.test(scalar_at(buf, i + 1)))
                    .then_some(c),
            );
        } else if c == LAQUO_U || c == BDQUO_U {
            closers.pop();
        } else if c == QUOT && !closers.is_empty() {
            let close = closers.last().copied().flatten();
            match close {
                Some(close) if divides_content(buf, i) => {
                    buf[i] = if close == RAQUO_U { LAQUO_U } else { BDQUO_U };
                    i -= 1;
                    buf[i] = close;
                }
                _ => {
                    if let Some(last) = closers.last_mut() {
                        *last = None;
                    }
                }
            }
        } else if ends_title_walk(c) {
            closers.clear();
        }
    }
    Ok(())
}

fn write_sibling(buf: &mut [u16], open: usize, end: usize, close: u16, allow_nested: bool) {
    buf[open] = if close == RAQUO_U { LAQUO_U } else { BDQUO_U };
    buf[end] = close;
    if allow_nested {
        for c in &mut buf[open + 1..end] {
            if *c == LAQUO_U {
                *c = BDQUO_U;
            } else if *c == RAQUO_U {
                *c = LDQUO_U;
            }
        }
    }
}

fn write_following_title(
    buf: &mut [u16],
    open: usize,
    close: u16,
    allow_nested: bool,
    padded: bool,
    check_budget: Budget<'_>,
) -> Result<()> {
    if buf.get(open) != Some(&QUOT) {
        return Ok(());
    }
    let mut first = open + 1;
    while matches!(buf.get(first), Some(&(LAQUO_U | BDQUO_U))) {
        first += 1;
        if padded {
            while buf.get(first).is_some_and(|&c| is_padding(c)) {
                first += 1;
            }
        }
    }
    if matches!(buf.get(first), Some(&(0x27 | RSQUO_U | LSQUO_U))) {
        first += 1;
    }
    if !TITLE_WORD_START.test(scalar_at(buf, first)) {
        return Ok(());
    }
    let mut depth = 0usize;
    let mut enclosing_end: Option<usize> = None;
    for i in open + 1..buf.len() {
        title_tick(i, check_budget)?;
        let ch = buf[i];
        if ends_title_walk(ch) {
            return Ok(());
        }
        if ch == LAQUO_U || ch == BDQUO_U {
            depth += 1;
        } else if ch == RAQUO_U || ch == LDQUO_U {
            if depth > 0 {
                depth -= 1;
            } else if close == LDQUO_U && ch == RAQUO_U && enclosing_end.is_none() {
                enclosing_end = Some(i);
            } else {
                return Ok(());
            }
        } else if ch == QUOT {
            if depth > 0
                || !TITLE_CONTENT_END.test(scalar_before(buf, i))
                || TITLE_WORD_CHAR.test(scalar_at(buf, i + 1))
            {
                return Ok(());
            }
            write_sibling(buf, open, i, close, allow_nested);
            if let Some(enclosing) = enclosing_end {
                buf[enclosing] = close;
                buf[i] = RAQUO_U;
            }
            return Ok(());
        }
    }
    Ok(())
}

fn write_siblings(
    buf: &mut [u16],
    at: usize,
    owners: &Ends,
    allow_nested: bool,
    padded: bool,
    check_budget: Budget<'_>,
) -> Result<usize> {
    let mut close = buf[at];
    if close != RAQUO_U && close != LDQUO_U {
        return Ok(at);
    }
    if at > 0 && (buf[at - 1] == LAQUO_U || buf[at - 1] == BDQUO_U) {
        write_following_title(buf, at + 1, close, allow_nested, padded, check_budget)?;
        return Ok(at);
    }
    let mut end = at + 1;
    while buf.get(end).is_some_and(|&c| c == QUOT || is_quote_mark(c)) {
        end += 1;
    }
    let may_start_word = opening_quote_neighbours(Some(0x3B), buf.get(end).copied().unwrap_or(0x20));
    let mut depth = 0usize;
    let mut pending: Option<usize> = None;
    for i in at + 1..end {
        let ch = buf[i];
        if ch == LAQUO_U || ch == BDQUO_U {
            depth += 1;
        } else if ch == RAQUO_U || ch == LDQUO_U {
            if depth > 0 {
                depth -= 1;
            } else {
                if !owners.has(i) {
                    return Ok(i);
                }
                close = ch;
                pending = None;
            }
        } else if depth == 0 && !(i + 1 == end && may_start_word) {
            match pending {
                None => pending = Some(i),
                Some(open) => {
                    write_sibling(buf, open, i, close, allow_nested);
                    pending = None;
                }
            }
        }
    }
    let open = if buf[end - 1] == QUOT { Some(end - 1) } else { pending };
    if let Some(open) = open {
        write_following_title(buf, open, close, allow_nested, padded, check_budget)?;
    }
    Ok(end)
}

#[derive(Default)]
struct Ends {
    order: Vec<Option<usize>>,
    slots: HashMap<usize, usize>,
}

impl Ends {
    fn add(&mut self, at: usize) {
        if !self.slots.contains_key(&at) {
            self.slots.insert(at, self.order.len());
            self.order.push(Some(at));
        }
    }

    fn delete(&mut self, at: usize) {
        if let Some(slot) = self.slots.remove(&at) {
            self.order[slot] = None;
        }
    }

    fn has(&self, at: usize) -> bool {
        self.slots.contains_key(&at)
    }
}

struct InchReader<'a> {
    padded: bool,
    follower: Option<u16>,
    pair_remainder: bool,
    allow_nested: bool,
    has_division: bool,
    check_budget: Budget<'a>,
    ends: Option<Ends>,
    gaps: Vec<(usize, usize)>,
}

impl InchReader<'_> {
    fn digit_before(&self, buf: &[u16], at: usize) -> Option<usize> {
        digit_before_quote(buf, at, self.padded)
    }

    fn remember_close(&mut self, buf: &[u16], at: usize) {
        if self.pair_remainder && (self.has_division || buf.get(at + 1).is_some_and(|&c| c == QUOT || is_quote_mark(c)))
        {
            self.ends.get_or_insert_with(Ends::default).add(at);
        }
    }

    fn forget_close(&mut self, at: usize) {
        if let Some(ends) = self.ends.as_mut() {
            ends.delete(at);
        }
    }

    fn release(&mut self, buf: &[u16], at: usize) {
        let start = self.digit_before(buf, at).map_or(0, |digit| digit + 1);
        if start < at {
            self.gaps.push((start, at));
        }
    }

    fn demote(&mut self, buf: &[u16], at: usize) {
        let mut after = at + 1;
        while buf.get(after) == Some(&QUOT) {
            after += 1;
        }
        if !is_space(buf.get(after).copied().or(self.follower)) {
            return;
        }
        let mut end = at;
        while end > 0 && buf[end - 1] == QUOT {
            end -= 1;
        }
        let mut start = end;
        while start > 0 && is_space(Some(buf[start - 1])) {
            start -= 1;
        }
        if start > 0 && start < end && SPACED_CLOSER_LEFT_RE.test(&buf[start.saturating_sub(3)..start]) {
            self.gaps.push((start, end));
        }
    }

    fn settle(&self, mut buf: JsString) -> Result<JsString> {
        if let Some(ends) = &self.ends {
            if self.has_division {
                divide_titles(&mut buf, ends, self.padded, self.check_budget)?;
            }
            let mut settled = 0usize;
            for &at in ends.order.iter().flatten() {
                if at >= settled {
                    settled = write_siblings(&mut buf, at, ends, self.allow_nested, self.padded, self.check_budget)?;
                }
            }
        }
        if self.gaps.is_empty() {
            return Ok(buf);
        }
        let mut dropped = vec![false; buf.len()];
        for &(from, to) in &self.gaps {
            dropped[from..to].fill(true);
        }
        Ok(buf.into_iter().zip(dropped).filter_map(|(c, drop)| (!drop).then_some(c)).collect())
    }
}

#[allow(clippy::too_many_arguments)]
fn process_chunk(
    chunk: &[u16],
    allow_nested: bool,
    mut inches: Option<InchReader<'_>>,
    check_budget: Budget<'_>,
    kept: Kept<'_>,
    tags: &mut QuoteTags<'_>,
    opened_behind: Option<OpenedBehind<'_>>,
) -> Result<JsString> {
    let open = chunk.iter().position(|&c| c == LAQUO_U);
    let close = chunk.iter().position(|&c| c == RAQUO_U);
    if let (Some(open), Some(close)) = (open, close)
        && close > open
        && !chunk[open + 1..].contains(&LAQUO_U)
        && !chunk[close + 1..].contains(&RAQUO_U)
        && chunk.get(close + 1) != Some(&QUOT)
        && !inches.as_ref().is_some_and(|reader| reader.has_division)
    {
        if let Some(check) = check_budget {
            check("quote:processChunk")?;
        }
        return Ok(chunk.to_vec());
    }

    let mut buf = chunk.to_vec();
    let mut balanced_stack: Vec<usize> = vec![0];
    let mut last_balanced = 0usize;
    let mut level: i64 = 0;
    let mut off = 0usize;
    let mut iter: u32 = 0;
    let mut many: Option<bool> = None;

    loop {
        if let Some(cb) = check_budget {
            let n = iter;
            iter = iter.wrapping_add(1);
            if n & 0x3ff == 0 {
                cb("quote:processChunk")?;
            }
        }
        let Some((pos, ch)) = next_quote(&buf, off) else { break };
        if ch == LAQUO_U {
            if level > 0 && allow_nested {
                buf[pos] = BDQUO_U;
            }
            level += 1;
        } else {
            level -= 1;
            if level >= 0
                && let Some(reader) = inches.as_mut()
            {
                reader.remember_close(&buf, pos);
            }
            if level > 0 && allow_nested {
                buf[pos] = LDQUO_U;
            }
        }
        off = pos + 1;
        if level == 0 {
            last_balanced = off;
            balanced_stack.push(last_balanced);
            continue;
        }
        if level < 0 && !kept(&buf, pos) && opens_here(&buf, pos, tags, None)? {
            buf[pos] = LAQUO_U;
            level = 1;
            if let Some(behind) = opened_behind
                && buf.get(pos + 1) == Some(&QUOT)
            {
                let written = behind(chunk, pos, tags)?.unwrap_or_default();
                for (k, &c) in written.iter().enumerate() {
                    match buf.get_mut(pos + 1 + k) {
                        Some(slot) => *slot = c,
                        None => buf.push(c),
                    }
                }
            }
            continue;
        }
        if level < 0
            && let Some(reader) = inches.as_mut()
        {
            let many = *many.get_or_insert_with(|| count_quote_marks(chunk) > SCAN_LIMIT);
            if many {
                let walk = Walk { allow_nested, check_budget, kept };
                return walk_back(chunk, buf, &balanced_stack, (last_balanced, off), &walk, tags, reader);
            }
            let (fallback, next) = scan_convert_inches(&mut buf, off, &mut balanced_stack, kept, reader);
            off = next;
            level = 0;
            if fallback {
                balanced_stack.clear();
            }
            balanced_stack.push(off);
        }
    }
    if level > 0 {
        let end = buf.len();
        restore_nested_marks(&mut buf, last_balanced, end);
    }
    match &inches {
        Some(reader) => reader.settle(buf),
        None => Ok(buf),
    }
}

pub fn authored_opening_at(
    buf: &[u16],
    at: usize,
    after_low: bool,
    cx: &mut QuoteTags<'_>,
    left: Option<Option<u16>>,
) -> Result<bool> {
    let before = left.unwrap_or_else(|| at.checked_sub(1).map(|i| buf[i]));
    Ok(before == Some(QUOT) || (after_low && word_before(buf.get(at + 1).copied())) || opens_here(buf, at, cx, left)?)
}

fn opens_here(buf: &[u16], at: usize, cx: &mut QuoteTags<'_>, left: Option<Option<u16>>) -> Result<bool> {
    let mut i = at + 1;
    while matches!(buf.get(i), Some(&(0x20 | 0x09))) {
        i += 1;
    }
    let right = quote_follower(buf, at + 1, cx);
    let right = if right == RAQUO_U { QUOT } else { right };
    Ok(can_start_quote(buf, at, right, cx, left) && opening_content_at(buf, i, cx, true)?)
}

pub fn can_start_quote(
    source: &[u16],
    at: usize,
    right: u16,
    cx: &mut QuoteTags<'_>,
    left: Option<Option<u16>>,
) -> bool {
    let left = left.unwrap_or_else(|| glyph_before(source, at, cx));
    opening_quote_neighbours(left, right)
        && !(left == Some(GT)
            && (inline_end_tag_ends_at(source, at - 1) || block_end_tag_start(source, at, cx).is_some())
            && !word_before(Some(right)))
}

pub fn block_end_tag_start(text: &[u16], end: usize, cx: &QuoteTags<'_>) -> Option<usize> {
    if cx.paragraphs && cx.safe_tags.is_some() {
        return None;
    }
    native_tag_start(text, end)
        .filter(|&start| text.get(start + 1) == Some(&0x2F) && text.get(start + 3) == Some(&BLOCK_KIND_U))
}

pub fn token_start(text: &[u16], at: usize) -> usize {
    if at > 0 && text[at - 1] == 0x5C { at - 1 } else { at }
}

fn quote_inline_edge(cx: &mut QuoteTags<'_>, text: &[u16], offset: usize, right: bool) -> Option<usize> {
    let mut at = offset;
    loop {
        let edge = if right {
            inline_text_start(cx.reference_names(), text, at)
        } else {
            inline_text_end(cx.reference_names(), text, at)
        };
        let start = if right { Some(edge) } else { native_tag_start(text, edge) };
        let end = if right { native_tag_end(text, edge) } else { Some(edge) };
        let (Some(start), Some(end)) = (start, end) else { return Some(edge) };
        let head = start + if text.get(start + 1) == Some(&0x2F) { 2 } else { 1 };
        if right
            && ((text.get(head) == Some(&OPEN_UNIT) && text.get(head + 1) == Some(&BLOCK_KIND_U))
                || line_break_at(text, start, cx))
        {
            return None;
        }
        if !cx.raw_comment(js::slice(text, start + 1, end - 1)) {
            return Some(edge);
        }
        at = if right { end } else { start };
    }
}

pub fn quote_left_dash(text: &[u16], at: usize, cx: &mut QuoteTags<'_>) -> Option<u16> {
    let before = if at > 0 && text[at - 1] == GT { quote_inline_edge(cx, text, at, false).unwrap_or(at) } else { at };
    let glyph = before.checked_sub(1).and_then(|i| text.get(i).copied());
    if is_quote_dash(glyph) { glyph } else { None }
}

pub fn dash_quote_at(text: &[u16], after: usize, left: Option<u16>, cx: &mut QuoteTags<'_>) -> bool {
    let mut edge = |offset: usize| quote_inline_edge(cx, text, offset, true);
    dash_quote_punctuation(text, after, left, Some(&mut edge))
}

pub fn could_open(text: &[u16], at: usize, cx: &mut QuoteTags<'_>, after: usize, previous_glyph: Option<u16>) -> bool {
    let start = token_start(text, at);
    let previous = previous_glyph
        .or_else(|| quote_left_dash(text, start, cx))
        .or_else(|| start.checked_sub(1).map(|left| text[left]));
    if previous == Some(LAQUO_U) {
        return false;
    }
    if dash_quote_at(text, after, previous, cx) {
        return true;
    }
    let dash = is_quote_dash(previous);
    let mut i = after;
    while i < text.len() && (text[i] == 0x20 || text[i] == 0x09 || (dash && text[i] == NBSP_U)) {
        i += 1;
    }
    i > after
        && can_start_quote(text, start, text[after], cx, previous_glyph.map(Some))
        && quote_content_at(text, i, cx)
}

const AMP: u16 = 0x26;

pub fn quote_follower_glyph(source: &[u16], after: usize, cx: &mut QuoteTags<'_>) -> JsString {
    let visible = inline_text_start(cx.names(), source, after);
    match source.get(visible) {
        None => vec![0x20],
        Some(&c) if c != AMP => vec![c],
        Some(_) => match cx.reference_starting(source, visible) {
            Some(Some(glyph)) => glyph,
            _ => vec![AMP],
        },
    }
}

pub fn quote_follower(source: &[u16], after: usize, cx: &mut QuoteTags<'_>) -> u16 {
    let visible = inline_text_start(cx.names(), source, after);
    match source.get(visible) {
        None => 0x20,
        Some(&c) if c != AMP => c,
        Some(_) => match cx.reference_starting(source, visible) {
            Some(Some(glyph)) => single(&glyph).unwrap_or(AMP),
            _ => AMP,
        },
    }
}

static INDENT_END_RE: LazyLock<Re> = LazyLock::new(|| Re::new(&format!(r"{OPEN}W\d{{1,9}}{CLOSE}$"), "u"));

fn glyph_before(buf: &[u16], at: usize, cx: &QuoteTags<'_>) -> Option<u16> {
    if at == 0 {
        return None;
    }
    if buf[at - 1] == CLOSE_UNIT && INDENT_END_RE.test(&buf[at.saturating_sub(TOKEN_MAX)..at]) {
        return Some(0x20);
    }
    if buf[at - 1] != 0x3B {
        return Some(buf[at - 1]);
    }
    match cx.reference_ending(buf, at - 1) {
        None => Some(0x3B),
        Some(glyph) => Some(glyph.as_deref().and_then(single).unwrap_or(0x3B)),
    }
}

fn count_quote_marks(chunk: &[u16]) -> usize {
    chunk.iter().filter(|&&c| is_quote_mark(c)).count()
}

fn scan_convert_inches(
    buf: &mut [u16],
    off: usize,
    balanced_stack: &mut Vec<usize>,
    kept: Kept<'_>,
    inches: &mut InchReader<'_>,
) -> (bool, usize) {
    loop {
        let candidate_start = balanced_stack.pop().unwrap_or(0);
        let had_nested_marks = has_nested_marks(buf, candidate_start, off);
        if let Some(quote_pos) = find_last_inch_quote(buf, candidate_start, off, had_nested_marks, inches) {
            if had_nested_marks {
                restore_nested_marks(buf, candidate_start, off);
            }
            inches.release(buf, quote_pos);
            buf[quote_pos] = PRIME_U;
            return (false, candidate_start);
        }
        if balanced_stack.is_empty() {
            break;
        }
    }
    let fallback_pos = off - 1;
    inches.forget_close(fallback_pos);
    if !kept(buf, fallback_pos) {
        buf[fallback_pos] = QUOT;
        inches.demote(buf, fallback_pos);
    }
    (true, fallback_pos + 1)
}

fn has_nested_marks(buf: &[u16], start: usize, end: usize) -> bool {
    (start..end).any(|i| buf[i] == BDQUO_U || buf[i] == LDQUO_U)
}

fn find_last_inch_quote(
    buf: &[u16],
    start: usize,
    end: usize,
    include_nested_close: bool,
    inches: &InchReader<'_>,
) -> Option<usize> {
    let mut i = end as isize - 1;
    while i > start as isize {
        let k = i as usize;
        let ch = buf[k];
        if (ch == RAQUO_U || (include_nested_close && ch == LDQUO_U)) && inches.digit_before(buf, k).is_some() {
            return Some(k);
        }
        i -= 1;
    }
    None
}

pub fn is_quote_mark(ch: u16) -> bool {
    ch == LAQUO_U || ch == RAQUO_U || ch == BDQUO_U || ch == LDQUO_U
}

#[derive(Clone, Copy)]
struct Run {
    from: usize,
    to: usize,
    target: i64,
}

enum Balanced {
    Point(usize),
    Run(Run),
}

struct QuoteMarks {
    buf: JsString,
    at: Vec<usize>,
    length: usize,
    size: usize,
    role: Vec<i8>,
    sum: Vec<i32>,
    low: Vec<i32>,
    low_close: Vec<i32>,
    high_plain: Vec<i32>,
    low_nested: Vec<i32>,
    marked: Vec<i32>,
    candidate: Vec<i32>,
    unread: Vec<usize>,
    acc: i64,
    base: i64,
    nest: bool,
}

impl QuoteMarks {
    fn new(buf: JsString, source: &[u16], inches: &mut InchReader<'_>, from_at: usize) -> Self {
        let astral = source.windows(2).any(|w| is_high(w[0]) && is_low(w[1]));
        let scanned: &[u16] = if astral { &buf } else { source };
        let at: Vec<usize> = (0..scanned.len()).filter(|&i| is_quote_mark(scanned[i])).collect();
        let length = at.len();
        let mut size = 1;
        while size < length {
            size *= 2;
        }
        let mut marks = QuoteMarks {
            buf,
            at,
            length,
            size,
            role: vec![0; size],
            sum: vec![0; 2 * size],
            low: vec![0; 2 * size],
            low_close: vec![0; 2 * size],
            high_plain: vec![0; 2 * size],
            low_nested: vec![0; 2 * size],
            marked: vec![0; 2 * size],
            candidate: vec![0; length],
            unread: vec![0; length + 1],
            acc: 0,
            base: 0,
            nest: true,
        };
        for k in 0..length {
            let pos = marks.at[k];
            let ch = marks.buf[pos];
            let typed = source[pos];
            if typed == LAQUO_U || typed == RAQUO_U {
                marks.role[k] = if ch == LAQUO_U || ch == BDQUO_U { 1 } else { -1 };
            }
            let closes = ch == RAQUO_U || ch == LDQUO_U;
            if pos >= from_at && marks.role[k] < 0 {
                inches.remember_close(&marks.buf, pos);
            }
            let after_digit = inches.digit_before(&marks.buf, pos).is_some();
            marks.candidate[k] = if closes && after_digit { k as i32 } else { k as i32 - 1 };
            marks.unread[k] = if typed == BDQUO_U || typed == LDQUO_U { k } else { k + 1 };
        }
        marks.unread[length] = length;
        for k in 0..size {
            marks.leaf(k);
        }
        for node in (1..size).rev() {
            marks.pull(node);
        }
        marks
    }

    fn index_at(&self, pos: usize) -> usize {
        let (mut lo, mut hi) = (0, self.length);
        while lo < hi {
            let mid = usize::midpoint(lo, hi);
            if self.at[mid] < pos {
                lo = mid + 1;
            } else {
                hi = mid;
            }
        }
        lo
    }

    fn write(&mut self, k: usize, ch: u16) {
        let role = self.role[k];
        self.write_role(k, ch, role);
    }

    fn write_role(&mut self, k: usize, ch: u16, role: i8) {
        let pos = self.at[k];
        if self.buf[pos] == ch && self.role[k] == role {
            return;
        }
        self.buf[pos] = ch;
        self.role[k] = role;
        if ch != RAQUO_U && ch != LDQUO_U && self.candidate[k] == k as i32 {
            self.candidate[k] = k as i32 - 1;
        }
        self.update(k);
    }

    fn closer_below(&mut self, from: usize, level: i64) -> isize {
        self.acc = 0;
        self.drop(1, 0, self.size as isize - 1, from as isize, -level - 1)
    }

    fn zeros(&self, from: usize, to: usize, level: i64) -> Option<Run> {
        let target = self.total(0, from) - level;
        let last = self.last_at_most(from, to as isize, target);
        if last == -1 { None } else { Some(Run { from, to: last as usize + 1, target }) }
    }

    fn last_at_most(&self, from: usize, to: isize, target: i64) -> isize {
        self.last_close(1, 0, self.size as isize - 1, 0, from as isize, to, target)
    }

    fn restyle(&mut self, from: usize, to: usize, level: i64, nest: bool) {
        self.nest = nest;
        let mut k = from;
        let mut base = level;
        loop {
            self.acc = 0;
            self.base = base;
            let j = self.wrong(1, 0, self.size as isize - 1, k as isize);
            if j == -1 || j as usize >= to {
                return;
            }
            let j = j as usize;
            let before = base + self.acc;
            if self.role[j] > 0 {
                self.write(j, if nest && before > 0 { BDQUO_U } else { LAQUO_U });
            } else {
                self.write(j, if nest && before > 1 { LDQUO_U } else { RAQUO_U });
            }
            base = before + i64::from(self.role[j]);
            k = j + 1;
        }
    }

    fn total(&self, from: usize, to: usize) -> i64 {
        let mut sum = 0i64;
        let (mut l, mut r) = (from + self.size, to + self.size);
        while l < r {
            if l & 1 == 1 {
                sum += i64::from(self.sum[l]);
                l += 1;
            }
            if r & 1 == 1 {
                r -= 1;
                sum += i64::from(self.sum[r]);
            }
            l >>= 1;
            r >>= 1;
        }
        sum
    }

    fn has_nested(&self, from: usize, to: usize) -> bool {
        let mut count = 0i64;
        let (mut l, mut r) = (from + self.size, to + self.size);
        while l < r {
            if l & 1 == 1 {
                count += i64::from(self.marked[l]);
                l += 1;
            }
            if r & 1 == 1 {
                r -= 1;
                count += i64::from(self.marked[r]);
            }
            l >>= 1;
            r >>= 1;
        }
        count > 0
    }

    fn last_inch_quote(&mut self, k: isize) -> isize {
        let mut root = k;
        while root >= 0 && self.candidate[root as usize] as isize != root {
            root = self.candidate[root as usize] as isize;
        }
        let mut i = k;
        while i > root {
            let next = self.candidate[i as usize] as isize;
            self.candidate[i as usize] = root as i32;
            i = next;
        }
        root
    }

    fn activate(&mut self, from: usize, to: usize) {
        let mut k = self.first_unread(from);
        while k < to {
            self.unread[k] = k + 1;
            self.role[k] = if self.buf[self.at[k]] == BDQUO_U { 1 } else { -1 };
            self.update(k);
            k = self.first_unread(k + 1);
        }
    }

    fn restore(&mut self, from: usize) {
        for k in from..self.length {
            let pos = self.at[k];
            if self.buf[pos] == BDQUO_U {
                self.buf[pos] = LAQUO_U;
            } else if self.buf[pos] == LDQUO_U {
                self.buf[pos] = RAQUO_U;
            }
        }
    }

    fn first_unread(&mut self, from: usize) -> usize {
        let mut root = from;
        while self.unread[root] != root {
            root = self.unread[root];
        }
        let mut i = from;
        while i < root {
            let next = self.unread[i];
            self.unread[i] = root;
            i = next;
        }
        root
    }

    fn drop(&mut self, node: usize, lo: isize, hi: isize, from: isize, target: i64) -> isize {
        if hi < from {
            return -1;
        }
        if lo >= from {
            if self.acc + i64::from(self.low[node]) > target {
                self.acc += i64::from(self.sum[node]);
                return -1;
            }
            if lo == hi {
                return lo;
            }
        }
        let mid = (lo + hi) >> 1;
        let left = self.drop(2 * node, lo, mid, from, target);
        if left == -1 { self.drop(2 * node + 1, mid + 1, hi, from, target) } else { left }
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "a segment-tree descent carries the node range, the running sum, the query window and the target"
    )]
    fn last_close(&self, node: usize, lo: isize, hi: isize, before: i64, from: isize, to: isize, target: i64) -> isize {
        if hi < from || lo >= to || before + i64::from(self.low_close[node]) > target {
            return -1;
        }
        if lo == hi {
            return lo;
        }
        let mid = (lo + hi) >> 1;
        let l = 2 * node;
        let right = self.last_close(l + 1, mid + 1, hi, before + i64::from(self.sum[l]), from, to, target);
        if right == -1 { self.last_close(l, lo, mid, before, from, to, target) } else { right }
    }

    fn wrong(&mut self, node: usize, lo: isize, hi: isize, from: isize) -> isize {
        if hi < from {
            return -1;
        }
        if lo >= from {
            let level = self.base + self.acc;
            let wrong = if self.nest {
                level + i64::from(self.high_plain[node]) > 0 || level + i64::from(self.low_nested[node]) < 1
            } else {
                self.low_nested[node] < FAR / 2
            };
            if !wrong {
                self.acc += i64::from(self.sum[node]);
                return -1;
            }
            if lo == hi {
                return lo;
            }
        }
        let mid = (lo + hi) >> 1;
        let left = self.wrong(2 * node, lo, mid, from);
        if left == -1 { self.wrong(2 * node + 1, mid + 1, hi, from) } else { left }
    }

    fn leaf(&mut self, k: usize) {
        let node = self.size + k;
        let role = i32::from(self.role[k]);
        let nested_form = k < self.length && matches!(self.buf[self.at[k]], BDQUO_U | LDQUO_U);
        let offset = if role > 0 { 0 } else { -1 };
        self.sum[node] = role;
        self.low[node] = role;
        self.low_close[node] = if role < 0 { -1 } else { FAR };
        self.high_plain[node] = if role != 0 && !nested_form { offset } else { -FAR };
        self.low_nested[node] = if role != 0 && nested_form { offset } else { FAR };
        self.marked[node] = i32::from(nested_form);
    }

    fn pull(&mut self, node: usize) {
        let l = 2 * node;
        let r = l + 1;
        let left = self.sum[l];
        self.sum[node] = left.wrapping_add(self.sum[r]);
        self.low[node] = self.low[l].min(left.wrapping_add(self.low[r]));
        self.low_close[node] = self.low_close[l].min(left.wrapping_add(self.low_close[r]));
        self.high_plain[node] = self.high_plain[l].max(left.wrapping_add(self.high_plain[r]));
        self.low_nested[node] = self.low_nested[l].min(left.wrapping_add(self.low_nested[r]));
        self.marked[node] = self.marked[l].wrapping_add(self.marked[r]);
    }

    fn update(&mut self, k: usize) {
        self.leaf(k);
        let mut node = usize::midpoint(self.size, k);
        while node > 0 {
            self.pull(node);
            node >>= 1;
        }
    }
}

struct Walk<'a> {
    allow_nested: bool,
    check_budget: Budget<'a>,
    kept: Kept<'a>,
}

fn walk_back(
    chunk: &[u16],
    buf: JsString,
    balanced_points: &[usize],
    positions: (usize, usize),
    walk: &Walk<'_>,
    tags: &mut QuoteTags<'_>,
    inches: &mut InchReader<'_>,
) -> Result<JsString> {
    let kept = walk.kept;
    let mut marks = QuoteMarks::new(buf, chunk, inches, positions.1 - 1);
    let mut balanced_stack: Vec<Balanced> =
        balanced_points.iter().map(|&pos| Balanced::Point(marks.index_at(pos))).collect();
    let mut last_balanced = marks.index_at(positions.0);
    let start = marks.index_at(positions.1);
    let mut off = try_convert_inches(&mut marks, start, &mut balanced_stack, kept, inches);
    let mut level: i64 = 0;
    let mut iter: u32 = 0;

    loop {
        if let Some(cb) = walk.check_budget {
            let n = iter;
            iter = iter.wrapping_add(1);
            if n & 0x3ff == 0 {
                cb("quote:processChunk")?;
            }
        }
        let p = marks.closer_below(off, level);
        let end = if p == -1 { marks.length } else { p as usize };
        let zeros = marks.zeros(off, end, level);
        marks.restyle(off, end, level, walk.allow_nested);
        if let Some(z) = zeros {
            last_balanced = z.to;
            balanced_stack.push(Balanced::Run(z));
        }
        if p == -1 {
            level += marks.total(off, end);
            break;
        }
        let p = p as usize;
        marks.write(p, RAQUO_U);
        off = p + 1;
        if !kept(&marks.buf, marks.at[p]) && opens_here(&marks.buf, marks.at[p], tags, None)? {
            marks.write_role(p, LAQUO_U, 1);
            level = 1;
        } else {
            off = try_convert_inches(&mut marks, off, &mut balanced_stack, kept, inches);
            level = 0;
        }
    }
    if level > 0 {
        marks.restore(last_balanced);
    }
    inches.settle(marks.buf)
}

fn try_convert_inches(
    marks: &mut QuoteMarks,
    off: usize,
    balanced_stack: &mut Vec<Balanced>,
    kept: Kept<'_>,
    inches: &mut InchReader<'_>,
) -> usize {
    let quote_pos = marks.last_inch_quote(off as isize - 1);
    let candidate_start = if balanced_stack.is_empty() { 0 } else { pop_balanced(marks, balanced_stack, quote_pos) };

    if candidate_start != -1 && quote_pos >= candidate_start {
        let candidate_start = candidate_start as usize;
        let had_nested_marks = marks.has_nested(candidate_start, off);
        if had_nested_marks {
            marks.activate(candidate_start, off);
        }
        inches.release(&marks.buf, marks.at[quote_pos as usize]);
        marks.write_role(quote_pos as usize, PRIME_U, 0);
        balanced_stack.push(Balanced::Point(candidate_start));
        return candidate_start;
    }
    inches.forget_close(marks.at[off - 1]);
    if kept(&marks.buf, marks.at[off - 1]) {
        marks.write_role(off - 1, RAQUO_U, 0);
    } else {
        marks.write_role(off - 1, QUOT, 0);
        inches.demote(&marks.buf, marks.at[off - 1]);
    }
    balanced_stack.clear();
    balanced_stack.push(Balanced::Point(off));
    off
}

fn pop_balanced(marks: &QuoteMarks, balanced_stack: &mut Vec<Balanced>, limit: isize) -> isize {
    while let Some(top) = balanced_stack.last() {
        let run = match top {
            Balanced::Point(p) => {
                let p = *p as isize;
                balanced_stack.pop();
                if p <= limit {
                    return p;
                }
                continue;
            }
            Balanced::Run(run) => *run,
        };
        let closer = marks.last_at_most(run.from, (run.to as isize).min(limit), run.target);
        if closer == -1 {
            balanced_stack.pop();
            continue;
        }
        if let Some(Balanced::Run(top)) = balanced_stack.last_mut() {
            top.to = closer as usize;
        }
        if marks.last_at_most(run.from, closer, run.target) == -1 {
            balanced_stack.pop();
        }
        return closer + 1;
    }
    -1
}

pub fn is_digit(ch: Option<u16>) -> bool {
    matches!(ch, Some(0x30..=0x39))
}

fn restore_nested_marks(buf: &mut [u16], start: usize, end: usize) {
    for c in &mut buf[start..end] {
        if *c == BDQUO_U {
            *c = LAQUO_U;
        } else if *c == LDQUO_U {
            *c = RAQUO_U;
        }
    }
}

impl fmt::Debug for QuoteOptions<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("QuoteOptions")
            .field("allow_nested", &self.allow_nested)
            .field("convert_inches", &self.convert_inches)
            .field("strips_padding", &self.strips_padding)
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::{QuoteOptions, QuoteTags, process_quotes};
    use crate::js::{lossy, u};

    const STATE_CASES: &[(&str, bool, bool, &str)] = &[
        ("«a «b» c»", true, true, "«a „b“ c»"),
        ("12» и «x»", true, true, "12″ и «x»"),
        ("«15» экран»", true, true, "«15″ экран»"),
        ("«a» 3» «b «c» d» 4»", true, true, "«a» 3″ «b „c“ d» 4″"),
        ("» слово", true, true, "« слово"),
        (
            "«a «b» c» 0» «x1» «x2» «a «b» c» 3» «x4» «x5» «a «b» c» 6» «x7» «x8» «a «b» c» 9» «x10» «x11» «a «b» c» 12» «x13» «x14» «a «b» c» 15» «x16» «x17» «a «b» c» 18» «x19» ",
            true,
            true,
            "«a „b“ c» 0″ «x1» «x2» «a „b“ c» 3″ «x4» «x5» «a „b“ c» 6″ «x7» «x8» «a „b“ c» 9″ «x10» «x11» «a „b“ c» 12″ «x13» «x14» «a „b“ c» 15″ «x16» «x17» «a „b“ c» 18″ «x19» ",
        ),
        (
            "«a «b» c» 0» «x1» «x2» «a «b» c» 3» «x4» «x5» «a «b» c» 6» «x7» «x8» «a «b» c» 9» «x10» «x11» «a «b» c» 12» «x13» «x14» «a «b» c» 15» «x16» «x17» «a «b» c» 18» «x19» ",
            false,
            true,
            "«a «b» c» 0″ «x1» «x2» «a «b» c» 3″ «x4» «x5» «a «b» c» 6″ «x7» «x8» «a «b» c» 9″ «x10» «x11» «a «b» c» 12″ «x13» «x14» «a «b» c» 15″ «x16» «x17» «a «b» c» 18″ «x19» ",
        ),
        (
            "«a «b» c» 0» «x1» «x2» «a «b» c» 3» «x4» «x5» «a «b» c» 6» «x7» «x8» «a «b» c» 9» «x10» «x11» «a «b» c» 12» «x13» «x14» «a «b» c» 15» «x16» «x17» «a «b» c» 18» «x19» «x20» «a «b» c» 21» «x22» «x23» «a «b» c» 24» «x25» «x26» «a «b» c» 27» «x28» «x29» «a «b» c» 30» «x31» «x32» «a «b» c» 33» «x34» «x35» «a «b» c» 36» «x37» «x38» «a «b» c» 39»  7» «tail «x»",
            true,
            true,
            "«a „b“ c» 0″ «x1» «x2» «a „b“ c» 3″ «x4» «x5» «a „b“ c» 6″ «x7» «x8» «a „b“ c» 9″ «x10» «x11» «a „b“ c» 12″ «x13» «x14» «a „b“ c» 15″ «x16» «x17» «a „b“ c» 18″ «x19» «x20» «a „b“ c» 21″ «x22» «x23» «a „b“ c» 24″ «x25» «x26» «a „b“ c» 27″ «x28» «x29» «a „b“ c» 30″ «x31» «x32» «a „b“ c» 33″ «x34» «x35» «a „b“ c» 36″ «x37» «x38» «a „b“ c» 39″  7″ «tail «x»",
        ),
        (
            "««x» 5» «x» 5» «x» 5» «x» 5» «x» 5» «x» 5» «x» 5» «x» 5» «x» 5» «x» 5» «x» 5» «x» 5» «x» 5» «x» 5» «x» 5» «x» 5» «x» 5» «x» 5» «x» 5» «x» 5» y» 9» z",
            true,
            true,
            "«„x“ 5″ „x“ 5″ „x“ 5″ „x“ 5″ „x“ 5″ „x“ 5″ „x“ 5″ „x“ 5″ „x“ 5″ „x“ 5″ „x“ 5″ „x“ 5″ „x“ 5″ „x“ 5″ „x“ 5″ „x“ 5″ „x“ 5″ „x“ 5″ „x“ 5″ „x“ 5″ y» 9″ z",
        ),
        (
            "«a „b“ c» 1» «q» «q» «q» «q» «q» «q» «q» «q» «q» «q» «q» «q» «q» «q» «q» «q» «q» «q» «q» «q» «q» «q» «q» «q» «q» «q» «q» «q» «q» «q» «q» «q» «q» «q» «q» 2»",
            true,
            true,
            "«a „b“ c» 1″ «q» «q» «q» «q» «q» «q» «q» «q» «q» «q» «q» «q» «q» «q» «q» «q» «q» «q» «q» «q» «q» «q» «q» «q» «q» «q» «q» «q» «q» «q» «q» «q» «q» «q» «q» 2″",
        ),
        (
            "😀«a «b» 3» c» «q» «q» «q» «q» «q» «q» «q» «q» «q» «q» «q» «q» «q» «q» «q» «q» «q» «q» «q» «q» «q» «q» «q» «q» «q» «q» «q» «q» «q» «q» «q» «q» «q» «q» «q» 😀 4» «open «x",
            true,
            true,
            "😀«a „b“ 3″ c» «q» «q» «q» «q» «q» «q» «q» «q» «q» «q» «q» «q» «q» «q» «q» «q» «q» «q» «q» «q» «q» «q» «q» «q» «q» «q» «q» «q» «q» «q» «q» «q» «q» «q» «q» 😀 4″ «open «x",
        ),
    ];

    #[test]
    fn matches_reference() {
        for &(input, allow_nested, convert_inches, expected) in STATE_CASES {
            let options = QuoteOptions { allow_nested, convert_inches, ..QuoteOptions::default() };
            let output = process_quotes(&u(input), &options, &mut QuoteTags::default()).unwrap();
            assert_eq!(lossy(&output), expected, "input: {input:?}");
        }
    }
}
