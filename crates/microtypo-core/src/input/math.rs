use std::sync::LazyLock;

use crate::clear_special::read_scalar_at;
use crate::errors::Result;
use crate::js::{self, Re};
use crate::protect::blocks::{CheckBudget, Range};
use crate::protect::tag_spans::{quoted_attribute_spans, tag_spans};
use crate::shared::character_reference::CHARACTER_REFERENCE_HEAD;

static REFERENCE_BEFORE_RE: LazyLock<Re> = LazyLock::new(|| Re::new(&format!("{CHARACTER_REFERENCE_HEAD};$"), "u"));
const REFERENCE_REACH: usize = 40;

fn space_at(text: &[u16], at: usize) -> bool {
    match text.get(at) {
        Some(&c) if js::is_js_space(c) => true,
        Some(&0x26) => {
            let (glyph, end) = read_scalar_at(text, at);
            end > at + 1 && glyph.first().is_some_and(|&c| js::is_js_space(c))
        }
        _ => false,
    }
}

fn space_before(text: &[u16], at: usize) -> bool {
    if js::is_js_space(text[at - 1]) {
        return true;
    }
    text[at - 1] == 0x3B
        && REFERENCE_BEFORE_RE
            .exec(&text[at.saturating_sub(REFERENCE_REACH)..at])
            .is_some_and(|reference| space_at(reference.whole(), 0))
}

fn bound_currency(text: &[u16], at: usize) -> bool {
    let after_amount = match at.checked_sub(1).map(|i| text[i]) {
        Some(0x3B) => {
            let Some(reference) = REFERENCE_BEFORE_RE.exec(&text[at.saturating_sub(REFERENCE_REACH)..at]) else {
                return false;
            };
            if read_scalar_at(reference.whole(), 0).0[..] != [0xA0] {
                return false;
            }
            at - reference.whole().len()
        }
        Some(0xA0) => at - 1,
        _ => return false,
    };
    after_amount.checked_sub(1).is_some_and(|i| (48..=57).contains(&text[i]))
}

fn printf_position(text: &[u16], at: usize) -> bool {
    let mut i = at;
    while i > 0 && (48..=57).contains(&text[i - 1]) {
        i -= 1;
    }
    i < at && i > 0 && text[i - 1] == 0x25
}

struct Token {
    at: usize,
    width: usize,
    kind: usize,
    line: usize,
    open: bool,
    close: bool,
    end: Option<usize>,
}

pub fn scan_math(text: &[u16], check: CheckBudget<'_>) -> Result<Vec<Range>> {
    let tags = tag_spans(text, None);
    let quoted = quoted_attribute_spans(text, &tags, check)?;
    let mut quote = 0;
    let mut line = 0;
    let mut tokens = Vec::new();
    let mut i = 0;
    while i < text.len() {
        if i & 1023 == 0 {
            check("markdown-math")?;
        }
        let c = text[i];
        if c == 13 || c == 10 {
            line += 1;
            if c == 13 && text.get(i + 1) == Some(&10) {
                i += 1;
            }
            i += 1;
            continue;
        }
        while quote < quoted.len() && quoted[quote].1 <= i {
            quote += 1;
        }
        if quote < quoted.len() && quoted[quote].0 <= i {
            i += 1;
            continue;
        }
        if c == 92 {
            let next = text.get(i + 1).copied();
            let kind = match next {
                Some(40 | 41) => Some(2),
                Some(91 | 93) => Some(3),
                _ => None,
            };
            if let Some(kind) = kind {
                tokens.push(Token {
                    at: i,
                    width: 2,
                    kind,
                    line,
                    open: matches!(next, Some(40 | 91)),
                    close: matches!(next, Some(41 | 93)),
                    end: None,
                });
            }
            if !matches!(next, Some(10 | 13)) {
                i += 1;
            }
            i += 1;
            continue;
        }
        if c != 36 {
            i += 1;
            continue;
        }
        let mut end = i + 1;
        while text.get(end) == Some(&36) {
            if end & 1023 == 0 {
                check("markdown-math")?;
            }
            end += 1;
        }
        let width = end - i;
        if width == 2 || (width == 1 && !printf_position(text, i)) {
            tokens.push(Token {
                at: i,
                width,
                kind: width - 1,
                line,
                open: width == 2 || (end < text.len() && !space_at(text, end) && !bound_currency(text, i)),
                close: width == 2
                    || (i > 0 && !space_before(text, i) && !text.get(end).is_some_and(|&c| (48..=57).contains(&c))),
                end: None,
            });
        }
        i = end;
    }
    let mut next: [Option<usize>; 4] = [None; 4];
    for i in (0..tokens.len()).rev() {
        if i & 1023 == 0 {
            check("markdown-math")?;
        }
        if let Some(k) = next[tokens[i].kind] {
            let closer = &tokens[k];
            if tokens[i].open
                && closer.at > tokens[i].at + tokens[i].width
                && (!matches!(tokens[i].kind, 0 | 2) || closer.line == tokens[i].line)
            {
                tokens[i].end = Some(closer.at + closer.width);
            }
        }
        if tokens[i].close {
            next[tokens[i].kind] = Some(i);
        }
    }
    let mut spans = Vec::new();
    let mut cursor = 0;
    let mut tag = 0;
    for token in tokens {
        while tag < tags.len() && (tags[tag].end < token.at || tags[tag].start < cursor) {
            tag += 1;
        }
        if tag < tags.len() && tags[tag].start < token.at && token.at <= tags[tag].end {
            continue;
        }
        if token.at >= cursor
            && let Some(end) = token.end
        {
            spans.push((token.at, end));
            cursor = end;
        }
    }
    Ok(spans)
}
