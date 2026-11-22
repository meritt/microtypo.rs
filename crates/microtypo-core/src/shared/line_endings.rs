use std::collections::HashMap;

use crate::errors::Result;
use crate::js::JsString;
use crate::shared::diff::equal_ranges;

type Check<'a> = &'a dyn Fn(&str) -> Result<()>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineEnding {
    Lf,
    Cr,
    CrLf,
}

impl LineEnding {
    fn units(self) -> &'static [u16] {
        match self {
            Self::Lf => &[0x0A],
            Self::Cr => &[0x0D],
            Self::CrLf => &[0x0D, 0x0A],
        }
    }
}

fn each_ending(text: &[u16], check: Check<'_>, mut found: impl FnMut(usize, LineEnding)) -> Result<()> {
    let mut at = 0;
    let mut seen = 0;
    let mut next_check = 1 << 16;
    while at < text.len() {
        if at >= next_check {
            check("file-line-endings")?;
            next_check = at + (1 << 16);
        }
        let ending = match text[at] {
            0x0D if text.get(at + 1) == Some(&0x0A) => LineEnding::CrLf,
            0x0D => LineEnding::Cr,
            0x0A => LineEnding::Lf,
            _ => {
                at += 1;
                continue;
            }
        };
        if seen & 0x3ff == 0 {
            check("file-line-endings")?;
        }
        seen += 1;
        found(at, ending);
        at += ending.units().len();
    }
    Ok(())
}

pub fn file_line_style(source: &[u16], check: Check<'_>) -> Result<LineEnding> {
    if !source.contains(&0x0D) {
        return Ok(LineEnding::Lf);
    }
    let mut counts: Vec<(LineEnding, usize)> = Vec::new();
    each_ending(source, check, |_, ending| {
        if let Some((_, count)) = counts.iter_mut().find(|(kind, _)| *kind == ending) {
            *count += 1;
        } else {
            counts.push((ending, 1));
        }
    })?;
    let (mut style, mut best) = counts[0];
    for &(candidate, count) in &counts {
        if count > best {
            style = candidate;
            best = count;
        }
    }
    Ok(style)
}

fn normalize(text: &[u16]) -> JsString {
    let mut output = Vec::with_capacity(text.len());
    let mut at = 0;
    while at < text.len() {
        if text[at] == 0x0D {
            output.push(0x0A);
            at += if text.get(at + 1) == Some(&0x0A) { 2 } else { 1 };
        } else {
            output.push(text[at]);
            at += 1;
        }
    }
    output
}

fn replace_endings(
    text: &[u16],
    check: Check<'_>,
    mut replacement: impl FnMut(usize) -> LineEnding,
) -> Result<JsString> {
    let mut output = Vec::with_capacity(text.len());
    let mut cursor = 0;
    each_ending(text, check, |at, ending| {
        output.extend_from_slice(&text[cursor..at]);
        output.extend_from_slice(replacement(at).units());
        cursor = at + ending.units().len();
    })?;
    output.extend_from_slice(&text[cursor..]);
    Ok(output)
}

pub fn restore_file_endings(source: &[u16], result: JsString, style: LineEnding, check: Check<'_>) -> Result<JsString> {
    if !source.contains(&0x0D) && style == LineEnding::Lf {
        return Ok(result);
    }
    let mut endings = Vec::new();
    let mut kinds = Vec::new();
    let mut removed = 0;
    each_ending(source, check, |at, ending| {
        endings.push((at - removed, ending));
        if !kinds.contains(&ending) {
            kinds.push(ending);
        }
        removed += ending.units().len() - 1;
    })?;
    if kinds.len() == 1 && endings[0].1 == style {
        return replace_endings(&result, check, |_| style);
    }
    let before = normalize(source);
    let after = normalize(&result);
    let mut mapped = HashMap::new();
    let mut next = 0;
    for (from, to, length) in equal_ranges(&before, &after, check)? {
        while next < endings.len() && endings[next].0 < from {
            next += 1;
        }
        while next < endings.len() && endings[next].0 < from + length {
            let (at, ending) = endings[next];
            next += 1;
            mapped.insert(to + at - from, ending);
        }
    }
    replace_endings(&after, check, |at| mapped.get(&at).copied().unwrap_or(style))
}

fn before_final_endings(text: &[u16]) -> usize {
    let mut end = text.len();
    while end > 0 && matches!(text[end - 1], 0x0D | 0x0A) {
        end -= 1;
    }
    end
}

pub fn restore_file_tail(source: &[u16], mut result: JsString) -> JsString {
    result.truncate(before_final_endings(&result));
    result.extend_from_slice(&source[before_final_endings(source)..]);
    result
}
