use std::sync::{Arc, LazyLock};

use crate::errors::Result;
use crate::js::{self, Re};
use crate::protect::blocks::{CheckBudget, Range, Scanner};

static ARGUMENT: LazyLock<Re> = LazyLock::new(|| Re::new(r"^[\p{L}\p{N}_]+", "u"));
static NUMBER: LazyLock<Re> = LazyLock::new(|| Re::new(r"^[+-]?(?:\d+(?:\.\d*)?|\.\d+)", ""));
static INTERPOLATION: LazyLock<Re> = LazyLock::new(|| Re::new(r"\$\{[^{}]*\}", "g"));

#[derive(Clone, Copy, PartialEq, Eq)]
enum Complex {
    Plural,
    SelectOrdinal,
    Select,
}

#[derive(Clone, Copy)]
struct Head {
    kind: Option<Complex>,
    at: usize,
}

#[derive(Clone, Copy)]
struct Open {
    at: usize,
    complex: bool,
    plural: bool,
    kind: Option<Complex>,
}

#[derive(Clone, Copy)]
struct Task {
    start: usize,
    end: usize,
    plural: bool,
}

fn spaces(text: &[u16], mut at: usize, end: usize) -> usize {
    while at < end && js::is_js_space(text[at]) {
        at += 1;
    }
    at
}

fn quoted_end(text: &[u16], start: usize, limit: usize, check: CheckBudget<'_>) -> Result<usize> {
    let mut at = start + 1;
    while at < limit {
        if at & 0x3fff == 0 {
            check("localization")?;
        }
        if text[at] == 0x27 {
            if text.get(at + 1) != Some(&0x27) {
                return Ok(at + 1);
            }
            at += 1;
        }
        at += 1;
    }
    Ok(limit)
}

fn argument_head(text: &[u16], start: usize, end: usize) -> Option<Head> {
    let mut at = spaces(text, start + 1, end);
    at += ARGUMENT.exec(&text[at..end])?.end();
    at = spaces(text, at, end);
    if at == end {
        return Some(Head { kind: None, at });
    }
    if text[at] != 0x2C {
        return None;
    }
    at = spaces(text, at + 1, end);
    let found = ARGUMENT.exec(&text[at..end])?;
    let kind = match js::lossy(found.whole()).as_str() {
        "plural" => Some(Complex::Plural),
        "selectordinal" => Some(Complex::SelectOrdinal),
        "select" => Some(Complex::Select),
        _ => None,
    };
    at += found.end();
    at = spaces(text, at, end);
    if at != end && text[at] != 0x2C {
        return None;
    }
    Some(Head { kind, at: if at == end { at } else { at + 1 } })
}

fn brace_pairs(text: &[u16], check: CheckBudget<'_>) -> Result<Vec<i32>> {
    let mut paired = vec![-1; text.len()];
    let mut stack: Vec<Open> = Vec::new();
    let mut next_check = 0;
    let mut at = 0;
    while at < text.len() {
        if at >= next_check {
            check("localization")?;
            next_check = at + 0x4000;
        }
        let outer = stack.last().copied();
        let next = text.get(at + 1).copied();
        if text[at] == 0x27 && next == Some(0x27) {
            at += 2;
            continue;
        }
        if text[at] == 0x27
            && (matches!(next, Some(0x7B | 0x7D)) || (outer.is_some_and(|o| o.plural) && next == Some(0x23)))
        {
            at = quoted_end(text, at, text.len(), check)?;
            continue;
        }
        if text[at] == 0x7B {
            let head = if outer.is_some_and(|o| o.complex) { None } else { argument_head(text, at, text.len()) };
            stack.push(Open {
                at,
                complex: head.is_some_and(|h| h.kind.is_some()),
                plural: outer.is_some_and(|o| o.complex && o.kind != Some(Complex::Select)),
                kind: head.and_then(|h| h.kind),
            });
        } else if text[at] == 0x7D
            && let Some(open) = stack.pop()
        {
            paired[open.at] = at as i32;
        }
        at += 1;
    }
    Ok(paired)
}

fn branches(text: &[u16], start: usize, end: usize, paired: &[i32], kind: Complex) -> Option<Vec<Task>> {
    let mut at = spaces(text, start, end);
    if kind != Complex::Select && text[at..end].starts_with(&[0x6F, 0x66, 0x66, 0x73, 0x65, 0x74, 0x3A]) {
        at = spaces(text, at + 7, end);
        at += NUMBER.exec(&text[at..end])?.end();
        at = spaces(text, at, end);
    }
    let mut result = Vec::new();
    while at < end {
        let selector = at;
        while at < end && !js::is_js_space(text[at]) && !matches!(text[at], 0x7B | 0x7D) {
            at += 1;
        }
        if at == selector {
            return None;
        }
        at = spaces(text, at, end);
        let close = *paired.get(at)?;
        if text.get(at) != Some(&0x7B) || close < at as i32 || close as usize >= end {
            return None;
        }
        let close = close as usize;
        result.push(Task { start: at + 1, end: close, plural: kind != Complex::Select });
        at = spaces(text, close + 1, end);
    }
    (!result.is_empty()).then_some(result)
}

fn mark_icu(text: &[u16], mask: &mut [u8], xml: bool, check: CheckBudget<'_>) -> Result<()> {
    let paired = brace_pairs(text, check)?;
    let mut queue = vec![Task { start: 0, end: text.len(), plural: false }];
    let mut task = 0;
    while task < queue.len() {
        let range = queue[task];
        let mut at = range.start;
        let mut next_check = at;
        while at < range.end {
            if at >= next_check {
                check("localization")?;
                next_check = at + 0x4000;
            }
            if mask[at] != 0 {
                at += 1;
                continue;
            }
            let next = text.get(at + 1).copied();
            if text[at] == 0x27 && next == Some(0x27) {
                at += 2;
                continue;
            }
            if text[at] == 0x27 && (matches!(next, Some(0x7B | 0x7D)) || (range.plural && next == Some(0x23))) {
                let end = quoted_end(text, at, range.end, check)?;
                mask[at..end].fill(1);
                at = end;
                continue;
            }
            if range.plural
                && text[at] == 0x23
                && (!xml
                    || at == 0
                    || text[at - 1] != 0x26
                    || crate::shared::character_reference::read_character_reference_at(text, at - 1).is_none())
            {
                mask[at] = 1;
            }
            let end = paired[at];
            if text[at] != 0x7B || end < at as i32 || end as usize >= range.end {
                at += 1;
                continue;
            }
            let end = end as usize;
            let Some(head) = argument_head(text, at, end) else {
                at += 1;
                continue;
            };
            let inner = head.kind.and_then(|kind| branches(text, head.at, end, &paired, kind));
            if let Some(inner) = inner {
                let mut from = at;
                for branch in inner {
                    mask[from..branch.start].fill(1);
                    queue.push(branch);
                    from = branch.end;
                }
                mask[from..=end].fill(1);
            } else {
                mask[at..=end].fill(1);
            }
            at = end + 1;
        }
        task += 1;
    }
    Ok(())
}

fn digits(text: &[u16], mut at: usize) -> usize {
    while text.get(at).is_some_and(|c| (0x30..=0x39).contains(c)) {
        at += 1;
    }
    at
}

fn star(text: &[u16], at: usize) -> usize {
    if text.get(at) != Some(&0x2A) {
        return digits(text, at);
    }
    let next = digits(text, at + 1);
    if text.get(next) == Some(&0x24) { next + 1 } else { at + 1 }
}

fn ascii_contains(set: &[u8], unit: Option<&u16>) -> bool {
    unit.is_some_and(|&unit| unit <= 0x7F && set.contains(&(unit as u8)))
}

fn printf_end(text: &[u16], start: usize) -> Option<usize> {
    let mut at = start + 1;
    if text.get(at) == Some(&0x25) {
        return Some(at + 1);
    }
    let numbered = digits(text, at);
    if text.get(numbered) == Some(&0x24) {
        at = numbered + 1;
    }
    while ascii_contains(b"-+#0'", text.get(at)) {
        at += 1;
    }
    at = star(text, at);
    if text.get(at) == Some(&0x2E) {
        at = star(text, at + 1);
    }
    if text[at..].starts_with(&[0x68, 0x68]) || text[at..].starts_with(&[0x6C, 0x6C]) {
        at += 2;
    } else if ascii_contains(b"hljztL", text.get(at)) {
        at += 1;
    }
    ascii_contains(b"diuoxXfFeEgGaAcspn@", text.get(at)).then_some(at + 1)
}

fn mark_printf(text: &[u16], mask: &mut [u8], check: CheckBudget<'_>) -> Result<()> {
    let mut count = 0;
    let mut at = js::index_of_unit(text, 0x25, 0);
    while let Some(start) = at {
        if count & 0x3ff == 0 {
            check("localization")?;
        }
        count += 1;
        let end = printf_end(text, start);
        if let Some(end) = end {
            mask[start..end].fill(1);
        }
        at = js::index_of_unit(text, 0x25, end.unwrap_or(start + 1));
    }
    Ok(())
}

fn mark_interpolation(text: &[u16], mask: &mut [u8], check: CheckBudget<'_>) -> Result<()> {
    let mut at = 0;
    let mut count = 0;
    while let Some(found) = INTERPOLATION.exec_at(text, at) {
        if count & 0x3ff == 0 {
            check("localization")?;
        }
        count += 1;
        mask[found.index()..found.end()].fill(1);
        at = found.end();
    }
    Ok(())
}

pub fn localization_scanner(mode: &str, xml: bool) -> Option<Scanner> {
    let (icu, printf, interpolation) = match mode {
        "off" => return None,
        "icu" => (true, false, false),
        "printf" => (false, true, false),
        "interpolation" => (false, false, true),
        "all" => (true, true, true),
        _ => unreachable!("validated localization mode"),
    };
    Some(Arc::new(move |text, ctx| -> Result<Vec<Range>> {
        let mut mask = vec![0; text.len()];
        if interpolation {
            mark_interpolation(text, &mut mask, ctx.check_budget)?;
        }
        if printf {
            mark_printf(text, &mut mask, ctx.check_budget)?;
        }
        if icu {
            mark_icu(text, &mut mask, xml, ctx.check_budget)?;
        }
        let mut spans = Vec::new();
        let mut at = 0;
        while at < mask.len() {
            if at & 0x3fff == 0 {
                (ctx.check_budget)("localization")?;
            }
            if mask[at] == 0 {
                at += 1;
                continue;
            }
            let start = at;
            while at < mask.len() && mask[at] != 0 {
                at += 1;
            }
            spans.push((start, at));
            at += 1;
        }
        Ok(spans)
    }))
}
