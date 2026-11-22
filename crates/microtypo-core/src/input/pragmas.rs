use std::sync::Arc;

use crate::errors::Result;
use crate::input::markdown::comment_reader;
use crate::input::span_path::Next;
use crate::js;
use crate::protect::blocks::{CheckBudget, DocumentScan, DocumentScanner, Resolve};
use crate::protect::tag_spans::{COMMENT_CLOSE, COMMENT_OPEN, Span, comment_close_at, inside_tag, tag_spans};

fn ends_stray_tag(text: &[u16], span: &Span, at: usize) -> bool {
    let close = (span.end + 1).checked_sub(COMMENT_CLOSE.len());
    let Some(close) = close.filter(|&close| text[close..].starts_with(COMMENT_CLOSE)) else { return false };
    if comment_close_at(text, at) != Some(close) {
        return false;
    }
    let mut before = at;
    while before > 0 && matches!(text[before - 1], 0x20 | 0x09) {
        before -= 1;
    }
    before > span.start && before > 0 && matches!(text[before - 1], 0x0A | 0x0D)
}

fn html_comment_reader<'a>(text: &[u16]) -> Next<'a> {
    let source = text.to_vec();
    let mut tags: Option<Vec<Span>> = None;
    let mut cursor = 0;
    let mut tried = None;
    Box::new(move |from| {
        let mut at = js::index_of(&source, COMMENT_OPEN, from);
        while let Some(start) = at {
            let tags = tags.get_or_insert_with(|| tag_spans(&source, None));
            if inside_tag(&source, tags, &mut cursor, start) {
                let first = tried != Some(cursor);
                tried = Some(cursor);
                if !first || !ends_stray_tag(&source, &tags[cursor], start) {
                    at = js::index_of(&source, COMMENT_OPEN, start + COMMENT_OPEN.len());
                    continue;
                }
            }
            let Some(close) = comment_close_at(&source, start) else { return Ok(None) };
            let end = close + COMMENT_CLOSE.len();
            return Ok(Some((start, end, end)));
        }
        Ok(None)
    })
}

fn scan_pragmas(text: &[u16], check: CheckBudget<'_>, markdown: bool, restore: Resolve<'_>) -> Result<DocumentScan> {
    let mut next = if markdown { comment_reader(text, check) } else { html_comment_reader(text) };
    let mut spans = Vec::new();
    let mut start = None;
    let mut depth = 0;
    let mut seen = 0;
    let mut from = 0;
    while let Some((open, end, after)) = next(from)? {
        if seen & 0x3ff == 0 {
            check("pragma")?;
        }
        seen += 1;
        let content = restore(js::slice(text, open + COMMENT_OPEN.len(), end.saturating_sub(COMMENT_CLOSE.len())));
        match js::lossy(js::trim(&content)).as_str() {
            "microtypo-ignore-file" => {
                return Ok(DocumentScan { spans: vec![(0, text.len(), false)], ignore_file: true });
            }
            "microtypo-ignore-start" => {
                if depth == 0 {
                    start = Some(open);
                }
                depth += 1;
            }
            "microtypo-ignore-end" if depth > 0 => {
                depth -= 1;
                if depth == 0 {
                    spans.push((start.expect("an open pragma"), end, false));
                    start = None;
                }
            }
            _ => {}
        }
        from = after;
    }
    if let Some(start) = start {
        spans.push((start, text.len(), true));
    }
    Ok(DocumentScan { spans, ignore_file: false })
}

pub fn pragma_scanner(markdown: bool) -> DocumentScanner {
    Arc::new(move |text, ctx, restore| scan_pragmas(text, ctx.check_budget, markdown, restore))
}
