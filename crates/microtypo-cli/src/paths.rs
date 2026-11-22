use crate::errors::{CliError, Result, if_present, is_missing, system, usage};
use microtypo_core::Re;
use serde_json::Value;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::time::Instant;

pub fn resolve(cwd: &Path, path: &str) -> PathBuf {
    let joined = if path.starts_with('/') { path.to_owned() } else { format!("{}/{path}", cwd.display()) };
    let mut parts: Vec<&str> = Vec::new();
    for part in joined.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            _ => parts.push(part),
        }
    }
    PathBuf::from(format!("/{}", parts.join("/")))
}

pub fn dirname(path: &Path) -> PathBuf {
    path.parent().map_or_else(|| PathBuf::from("/"), Path::to_path_buf)
}

pub fn is_inside(root: &Path, target: &Path) -> bool {
    target.starts_with(root)
}

pub fn relative(root: &Path, target: &Path) -> String {
    let a: Vec<_> = root.components().collect();
    let b: Vec<_> = target.components().collect();
    let common = a.iter().zip(&b).take_while(|(a, b)| a == b).count();
    let mut parts = vec!["..".to_owned(); a.len() - common];
    parts.extend(b[common..].iter().map(|c| c.as_os_str().to_string_lossy().into_owned()));
    parts.join("/")
}

fn known_format(path: &str) -> Option<&'static str> {
    let extension = Path::new(path).extension()?.to_str()?.to_ascii_lowercase();
    Some(match extension.as_str() {
        "txt" => "text",
        "html" | "htm" => "html",
        "md" | "markdown" => "markdown",
        "mdx" => "mdx",
        "json" => "json",
        "xml" | "svg" => "xml",
        "yaml" | "yml" => "yaml",
        "toml" => "toml",
        _ => return None,
    })
}

pub fn inferred_format(path: &str) -> &'static str {
    known_format(path).unwrap_or("text")
}

pub fn validate_masks(masks: &Value) -> bool {
    fn valid(v: &Value) -> bool {
        v.as_str().is_some_and(|s| !s.is_empty() && s.encode_utf16().count() <= 1024)
    }
    valid(masks) || masks.as_array().is_some_and(|a| !a.is_empty() && a.len() <= 128 && a.iter().all(valid))
}

pub struct Masks(Vec<Glob>);

impl Masks {
    pub fn compile(masks: &Value) -> Self {
        let masks = masks.as_array().map_or_else(|| vec![masks], |array| array.iter().collect());
        Self(masks.into_iter().filter_map(Value::as_str).filter_map(compile_glob).collect())
    }

    pub fn matches(&self, path: &str) -> bool {
        let path: Vec<u16> = path.encode_utf16().collect();
        self.0.iter().any(|glob| glob.test(&path))
    }
}

fn literal(c: char) -> String {
    if "\\.^$|?*+()[]{}".contains(c) { format!("\\{c}") } else { c.to_string() }
}

fn closing(chars: &[char], from: usize, open: char, close: char) -> Option<usize> {
    let mut depth = 1;
    for (i, &c) in chars.iter().enumerate().skip(from) {
        if c == open {
            depth += 1;
        }
        if c == close {
            depth -= 1;
            if depth == 0 {
                return Some(i);
            }
        }
    }
    None
}

fn alternatives(chars: &[char], separator: char) -> Vec<&[char]> {
    let (mut depth, mut start) = (0, 0);
    let mut result = Vec::new();
    for (i, &c) in chars.iter().enumerate() {
        match c {
            '(' | '{' | '[' => depth += 1,
            ')' | '}' | ']' => depth -= 1,
            _ => {}
        }
        if c == separator && depth == 0 {
            result.push(&chars[start..i]);
            start = i + 1;
        }
    }
    result.push(&chars[start..]);
    result
}

fn brace_choices(body: &[char]) -> Option<Vec<Vec<char>>> {
    let choices = alternatives(body, ',');
    if choices.len() > 1 {
        return Some(choices.iter().map(|part| part.to_vec()).collect());
    }
    let body: String = body.iter().collect();
    let range: Vec<_> = body.split("..").collect();
    if !(2..=3).contains(&range.len()) {
        return None;
    }
    let number = |s: &str| {
        s.strip_prefix('-').unwrap_or(s).bytes().all(|b| b.is_ascii_digit()).then(|| s.parse::<i64>().ok()).flatten()
    };
    let numbers = number(range[0]).zip(number(range[1]));
    let letters = (range[0].len() == 1
        && range[1].len() == 1
        && range[0].as_bytes()[0].is_ascii_alphabetic()
        && range[1].as_bytes()[0].is_ascii_alphabetic())
    .then(|| (i64::from(range[0].as_bytes()[0]), i64::from(range[1].as_bytes()[0])));
    let (a, b) = numbers.or(letters)?;
    let step = if let Some(step) = range.get(2) { number(step)?.unsigned_abs().max(1) } else { 1 };
    let count = (a.abs_diff(b) / step).min(99_999);
    let padded = range[..2]
        .iter()
        .any(|s| s.strip_prefix('-').unwrap_or(s).starts_with('0') && s.strip_prefix('-').unwrap_or(s).len() > 1);
    let width = range[0].len().max(range[1].len());
    Some(
        (0..=count)
            .map(|n| {
                let value = if a <= b { a + (n * step) as i64 } else { a - (n * step) as i64 };
                if numbers.is_some() {
                    if padded { format!("{value:0width$}") } else { value.to_string() }
                } else {
                    char::from_u32(value as u32).unwrap_or_default().to_string()
                }
                .chars()
                .collect()
            })
            .collect(),
    )
}

fn expand_braces(mask: &[char]) -> Vec<Vec<char>> {
    let mut result = Vec::new();
    expand_braces_into(mask, &mut result, &mut 0);
    result
}

fn expand_braces_into(mask: &[char], result: &mut Vec<Vec<char>>, length: &mut usize) -> bool {
    if result.len() >= 100_000 {
        return false;
    }
    for (start, &c) in mask.iter().enumerate() {
        if c != '{' {
            continue;
        }
        let Some(end) = closing(mask, start + 1, '{', '}') else { continue };
        let Some(choices) = brace_choices(&mask[start + 1..end]) else { continue };
        for choice in choices {
            if !expand_braces_into(&[&mask[..start], choice.as_slice(), &mask[end + 1..]].concat(), result, length) {
                return false;
            }
        }
        return true;
    }
    let units = mask.iter().map(|c| c.len_utf16()).sum::<usize>();
    if *length + units > 4_000_000 {
        return false;
    }
    *length += units;
    result.push(mask.to_vec());
    true
}

#[derive(Clone)]
enum SegmentNode {
    Text(std::ops::Range<usize>),
    Ext(usize),
}

#[derive(Default)]
struct SegmentSequence {
    parts: Vec<SegmentNode>,
    negative_tails: usize,
}

struct Extglob {
    kind: char,
    branches: Vec<usize>,
    registered_negative: bool,
    detached_negatives: usize,
    empty: bool,
    original: std::ops::Range<usize>,
    needs_fill: bool,
}

struct SegmentPattern<'a> {
    source: &'a [char],
    sequences: Vec<SegmentSequence>,
    extglobs: Vec<Extglob>,
}

fn adopts(parent: char, child: char) -> bool {
    match parent {
        '!' => child == '@',
        '?' => matches!(child, '?' | '@'),
        '@' => child == '@',
        '*' => matches!(child, '*' | '+' | '?' | '@'),
        '+' => matches!(child, '+' | '@'),
        _ => false,
    }
}

fn adopts_with_empty(parent: char, child: char) -> bool {
    matches!((parent, child), ('!' | '@' | '+', '?') | ('+', '*'))
}

fn usurps(parent: char, child: char) -> Option<char> {
    match (parent, child) {
        ('!', '!') => Some('@'),
        ('?', '*' | '+') => Some('*'),
        ('@', '!' | '?' | '*' | '+') => Some(child),
        ('+', '?' | '*') => Some('*'),
        _ => None,
    }
}

impl<'a> SegmentPattern<'a> {
    fn sequence(&mut self) -> usize {
        let id = self.sequences.len();
        self.sequences.push(SegmentSequence::default());
        id
    }

    fn parse(source: &'a [char]) -> Self {
        struct Frame {
            kind: Option<char>,
            begin: usize,
            depth: usize,
            sequence: usize,
            branches: Vec<usize>,
            text: usize,
        }
        let mut result = Self { source, sequences: vec![SegmentSequence::default()], extglobs: Vec::new() };
        let mut frames = vec![Frame { kind: None, begin: 0, depth: 0, sequence: 0, branches: Vec::new(), text: 0 }];
        let mut at = 0;
        while at < source.len() {
            let c = source[at];
            let frame = frames.last_mut().expect("root frame");
            if c == '[' {
                at = class_source(source, at).map_or(source.len(), |(_, end, _)| end + 1);
                continue;
            }
            if frame.kind.is_some() && matches!(c, '|' | ')') {
                if frame.text < at {
                    result.sequences[frame.sequence].parts.push(SegmentNode::Text(frame.text..at));
                }
                frame.branches.push(frame.sequence);
                if c == '|' {
                    frame.sequence = result.sequence();
                    frame.text = at + 1;
                } else {
                    let frame = frames.pop().expect("extglob frame");
                    let id = result.extglobs.len();
                    let kind = frame.kind.expect("extglob kind");
                    result.extglobs.push(Extglob {
                        kind,
                        branches: frame.branches,
                        registered_negative: kind == '!',
                        detached_negatives: 0,
                        empty: frame.text == at,
                        original: frame.begin..at + 1,
                        needs_fill: false,
                    });
                    result.normalize(id);
                    let parent = frames.last_mut().expect("parent frame");
                    result.sequences[parent.sequence].parts.push(SegmentNode::Ext(id));
                    parent.text = at + 1;
                }
                at += 1;
                continue;
            }
            let adoptable = frame.kind.is_some_and(|p| adopts(p, c) || adopts_with_empty(p, c));
            if "?*+@!".contains(c) && source.get(at + 1) == Some(&'(') && (frame.depth <= 2 || adoptable) {
                if frame.text < at {
                    result.sequences[frame.sequence].parts.push(SegmentNode::Text(frame.text..at));
                }
                let depth = frame.depth + usize::from(!adoptable);
                let sequence = result.sequence();
                frames.push(Frame { kind: Some(c), begin: at, depth, sequence, branches: Vec::new(), text: at + 2 });
                at += 2;
            } else {
                at += 1;
            }
        }
        while frames.len() > 1 {
            let frame = frames.pop().expect("extglob frame");
            let parent = frames.last_mut().expect("parent frame");
            result.sequences[parent.sequence].parts.push(SegmentNode::Text(frame.begin..source.len()));
            parent.text = source.len();
        }
        let root = frames.pop().expect("root frame");
        if root.text < source.len() {
            result.sequences[0].parts.push(SegmentNode::Text(root.text..source.len()));
        }
        result
    }

    fn normalize(&mut self, id: usize) {
        for _ in 0..10 {
            let mut changed = false;
            let mut index = 0;
            while index < self.extglobs[id].branches.len() {
                let branch = self.extglobs[id].branches[index];
                let [SegmentNode::Ext(child)] = self.sequences[branch].parts.as_slice() else {
                    index += 1;
                    continue;
                };
                let child = *child;
                let kind = self.extglobs[id].kind;
                let child_kind = self.extglobs[child].kind;
                let adopt = adopts(kind, child_kind);
                let with_empty = adopts_with_empty(kind, child_kind);
                let usurp = (self.extglobs[id].branches.len() == 1).then(|| usurps(kind, child_kind)).flatten();
                if !adopt && !with_empty && usurp.is_none() {
                    index += 1;
                    continue;
                }
                if with_empty {
                    let empty = self.sequence();
                    self.extglobs[child].branches.push(empty);
                }
                let mut parts = std::mem::take(&mut self.extglobs[child].branches);
                let tails = self.sequences[branch].negative_tails;
                let child_tails = self.extglobs[child].detached_negatives
                    + usize::from(self.extglobs[child].registered_negative && child_kind == '!');
                self.extglobs[id].branches.remove(index);
                if let Some(kind) = usurp {
                    self.extglobs[id].kind = kind;
                    self.extglobs[id].detached_negatives += child_tails;
                    self.extglobs[id].empty = false;
                    for &part in &parts {
                        self.sequences[part].negative_tails += tails;
                    }
                    self.extglobs[id].branches = parts;
                    index = 0;
                } else {
                    for &part in &parts {
                        self.sequences[part].negative_tails += tails + child_tails;
                    }
                    let count = parts.len();
                    self.extglobs[id].branches.splice(index..index, parts.drain(..));
                    index += count;
                }
                changed = true;
            }
            if !changed {
                break;
            }
        }
        let ext = &self.extglobs[id];
        let needs_fill = ext.detached_negatives != 0
            || (ext.registered_negative && ext.kind == '!')
            || ext.branches.iter().any(|&branch| {
                self.sequences[branch].negative_tails != 0
                    || self.sequences[branch]
                        .parts
                        .iter()
                        .any(|node| matches!(node, SegmentNode::Ext(child) if self.extglobs[*child].needs_fill))
            });
        self.extglobs[id].needs_fill = needs_fill;
    }

    fn fill_negative_tails(&mut self) {
        struct Tail {
            sequence: usize,
            from: usize,
            to: usize,
            next: Option<usize>,
        }
        enum Task {
            Sequence(usize, Option<usize>),
            FinishSequence(usize, Option<usize>),
            Ext(usize, Option<usize>),
            FinishExt(usize, Option<usize>),
        }
        let mut tails: Vec<Tail> = Vec::new();
        let mut tasks = vec![Task::Sequence(0, None)];
        while let Some(task) = tasks.pop() {
            match task {
                Task::Sequence(id, following) => {
                    tasks.push(Task::FinishSequence(id, following));
                    let length = self.sequences[id].parts.len();
                    for (index, node) in self.sequences[id].parts.iter().enumerate() {
                        if let SegmentNode::Ext(ext) = node
                            && self.extglobs[*ext].needs_fill
                        {
                            let tail = if index + 1 == length {
                                following
                            } else {
                                tails.push(Tail { sequence: id, from: index + 1, to: length, next: following });
                                Some(tails.len() - 1)
                            };
                            tasks.push(Task::Ext(*ext, tail));
                        }
                    }
                }
                Task::Ext(id, tail) => {
                    tasks.push(Task::FinishExt(id, tail));
                    for &branch in &self.extglobs[id].branches {
                        tasks.push(Task::Sequence(branch, tail));
                    }
                }
                Task::FinishSequence(id, following) => {
                    let passes = self.sequences[id].negative_tails;
                    self.sequences[id].negative_tails = 0;
                    for _ in 0..passes {
                        let mut cursor = following;
                        while let Some(tail) = cursor {
                            let tail = &tails[tail];
                            let parts = self.sequences[tail.sequence].parts[tail.from..tail.to].to_vec();
                            self.sequences[id].parts.extend(parts);
                            cursor = tail.next;
                        }
                    }
                }
                Task::FinishExt(id, following) => {
                    let passes = self.extglobs[id].detached_negatives
                        + usize::from(self.extglobs[id].registered_negative && self.extglobs[id].kind == '!');
                    for &branch in &self.extglobs[id].branches {
                        for _ in 0..passes {
                            let mut cursor = following;
                            while let Some(tail) = cursor {
                                let tail = &tails[tail];
                                let parts = self.sequences[tail.sequence].parts[tail.from..tail.to].to_vec();
                                self.sequences[branch].parts.extend(parts);
                                cursor = tail.next;
                            }
                        }
                    }
                    self.extglobs[id].detached_negatives = 0;
                    self.extglobs[id].registered_negative = false;
                    self.extglobs[id].needs_fill = false;
                }
            }
        }
    }

    fn sensitivities(&self) -> (Vec<bool>, Vec<bool>) {
        enum Task {
            Sequence(usize, bool),
            Ext(usize, bool),
        }
        let mut sequences = vec![None; self.sequences.len()];
        let mut extglobs = vec![None; self.extglobs.len()];
        let mut tasks = vec![Task::Sequence(0, false)];
        while let Some(task) = tasks.pop() {
            match task {
                Task::Sequence(id, false) if sequences[id].is_none() => {
                    tasks.push(Task::Sequence(id, true));
                    for node in &self.sequences[id].parts {
                        if let SegmentNode::Ext(ext) = node {
                            tasks.push(Task::Ext(*ext, false));
                        }
                    }
                }
                Task::Ext(id, false) if extglobs[id].is_none() => {
                    tasks.push(Task::Ext(id, true));
                    for &branch in &self.extglobs[id].branches {
                        tasks.push(Task::Sequence(branch, false));
                    }
                }
                Task::Sequence(id, true) => {
                    let mut sensitive = false;
                    for node in &self.sequences[id].parts {
                        match node {
                            SegmentNode::Text(range) => {
                                sensitive |= text_source(&self.source[range.clone()], false, true, &mut false)
                                    .starts_with(['[', '.']);
                                break;
                            }
                            SegmentNode::Ext(ext) => {
                                sensitive |= extglobs[*ext].unwrap_or(false);
                                if self.extglobs[*ext].kind != '!' {
                                    break;
                                }
                            }
                        }
                    }
                    sequences[id] = Some(sensitive);
                }
                Task::Ext(id, true) => {
                    extglobs[id] = Some(
                        self.extglobs[id].kind == '!'
                            || self.extglobs[id].branches.iter().any(|&b| sequences[b].unwrap_or(false)),
                    );
                }
                _ => {}
            }
        }
        (
            sequences.into_iter().map(Option::unwrap_or_default).collect(),
            extglobs.into_iter().map(Option::unwrap_or_default).collect(),
        )
    }

    fn emit(&self, unicode: &mut bool) -> String {
        enum Task {
            Sequence(usize, bool, bool, bool, bool),
            Ext(usize, bool, bool, bool),
            Text(std::ops::Range<usize>, bool, bool, bool),
            Literal(&'static str),
        }
        let (sensitive, _) = self.sensitivities();
        let mut output = String::new();
        let mut tasks = vec![Task::Sequence(0, true, true, false, false)];
        while let Some(task) = tasks.pop() {
            match task {
                Task::Literal(text) => output.push_str(text),
                Task::Text(range, start, dot, literal_only) => {
                    if literal_only {
                        for &c in &self.source[range] {
                            output.push_str(&literal(c));
                        }
                    } else {
                        output.push_str(&text_source(&self.source[range], start, dot, unicode));
                    }
                }
                Task::Sequence(id, start, end, parent_negative, dot) => {
                    if end && parent_negative {
                        tasks.push(Task::Literal("(?:$|\\/)"));
                    }
                    let mut parts = Vec::new();
                    let mut at_start = start;
                    for (index, node) in self.sequences[id].parts.iter().enumerate() {
                        let node_end = end && index + 1 == self.sequences[id].parts.len();
                        parts.push(match node {
                            SegmentNode::Text(range) => Task::Text(range.clone(), at_start, dot, false),
                            SegmentNode::Ext(ext) => Task::Ext(*ext, at_start, node_end, dot),
                        });
                        at_start &= matches!(node, SegmentNode::Ext(ext) if self.extglobs[*ext].kind == '!');
                    }
                    tasks.extend(parts.into_iter().rev());
                }
                Task::Ext(id, start, end, dot) => {
                    let ext = &self.extglobs[id];
                    if ext.kind == '!' && ext.empty {
                        tasks.push(Task::Literal("[^/]+?"));
                        if start && !dot {
                            tasks.push(Task::Literal("(?!\\.)"));
                        }
                        continue;
                    }
                    let branches: Vec<_> = ext
                        .branches
                        .iter()
                        .copied()
                        .filter(|&b| !(start && end) || ext.kind == '!' || !self.sequences[b].parts.is_empty())
                        .collect();
                    if branches.is_empty() && start && end && ext.kind != '!' {
                        tasks.push(Task::Text(ext.original.clone(), start, dot, true));
                        continue;
                    }
                    let separate_dot =
                        matches!(ext.kind, '*' | '+') && !dot && start && branches.iter().any(|&b| sensitive[b]);
                    let mut parts = Vec::new();
                    if ext.kind == '!' {
                        parts.push(Task::Literal("(?:(?!(?:"));
                    } else {
                        parts.push(Task::Literal("(?:"));
                    }
                    if separate_dot {
                        parts.push(Task::Literal("(?:"));
                    }
                    for (i, &branch) in branches.iter().enumerate() {
                        if i != 0 {
                            parts.push(Task::Literal("|"));
                        }
                        parts.push(Task::Sequence(branch, start, ext.kind == '!' || end, ext.kind == '!', dot));
                    }
                    if separate_dot {
                        parts.push(Task::Literal(")(?:(?:"));
                        for (i, &branch) in branches.iter().enumerate() {
                            if i != 0 {
                                parts.push(Task::Literal("|"));
                            }
                            parts.push(Task::Sequence(branch, start, end, false, true));
                        }
                        parts.push(Task::Literal("))*?"));
                    }
                    if ext.kind == '!' {
                        parts.push(Task::Literal("))"));
                        if start && !dot {
                            parts.push(Task::Literal("(?!\\.)"));
                        }
                        parts.push(Task::Literal("[^/]*?)"));
                    } else {
                        parts.push(Task::Literal(match ext.kind {
                            '@' => ")",
                            '?' => ")?",
                            '+' if separate_dot => ")",
                            '*' if separate_dot => ")?",
                            '+' => ")+",
                            '*' => ")*",
                            _ => unreachable!(),
                        }));
                    }
                    tasks.extend(parts.into_iter().rev());
                }
            }
        }
        output
    }
}

fn text_source(chars: &[char], start: bool, allow_dot: bool, unicode: &mut bool) -> String {
    let mut output = String::new();
    let mut i = 0;
    while i < chars.len() {
        match chars[i] {
            '*' => {
                while chars.get(i + 1) == Some(&'*') {
                    i += 1;
                }
                output.push_str("[^/]*?");
            }
            '?' => output.push_str("[^/]"),
            '[' => {
                if let Some((source, end, needs_unicode)) = class_source(chars, i) {
                    output.push_str(&source);
                    *unicode |= needs_unicode;
                    i = end;
                } else {
                    output.push_str("\\[");
                }
            }
            c => output.push_str(&literal(c)),
        }
        i += 1;
    }
    if start && !allow_dot && output.starts_with(['[', '.']) {
        output.insert_str(0, "(?!\\.)");
    }
    output
}

fn segment_source(chars: &[char], unicode: &mut bool) -> String {
    let mut pattern = SegmentPattern::parse(chars);
    pattern.fill_negative_tails();
    pattern.emit(unicode)
}

fn class_source(chars: &[char], start: usize) -> Option<(String, usize, bool)> {
    let mut i = start + 1;
    let negate = matches!(chars.get(i), Some('!' | '^'));
    i += usize::from(negate);
    let mut ranges = Vec::new();
    let mut negatives = Vec::new();
    let mut unicode = false;
    let mut saw = false;
    while i < chars.len() {
        if chars[i] == ']' && saw {
            let positive = format!("[{}{}]", if negate { "^" } else { "" }, ranges.join(""));
            let negative = format!("[{}{}]", if negate { "" } else { "^" }, negatives.join(""));
            let source = match (ranges.is_empty(), negatives.is_empty()) {
                (true, true) => "$.".to_owned(),
                (false, true) => positive,
                (true, false) => negative,
                (false, false) => format!("(?:{positive}|{negative})"),
            };
            return Some((source, i, unicode));
        }
        saw = true;
        if chars[i] == '['
            && chars.get(i + 1) == Some(&':')
            && let Some(end) =
                (i + 2..chars.len().saturating_sub(1)).find(|&end| chars[end] == ':' && chars[end + 1] == ']')
        {
            let name: String = chars[i + 2..end].iter().collect();
            let (source, needs_unicode, negative) = match name.as_str() {
                "alnum" => ("\\p{L}\\p{Nl}\\p{Nd}", true, false),
                "alpha" => ("\\p{L}\\p{Nl}", true, false),
                "ascii" => ("\\x00-\\x7f", false, false),
                "blank" => ("\\p{Zs}\\t", true, false),
                "cntrl" => ("\\p{Cc}", true, false),
                "digit" => ("\\p{Nd}", true, false),
                "graph" => ("\\p{Z}\\p{C}", true, true),
                "lower" => ("\\p{Ll}", true, false),
                "print" => ("\\p{C}", true, false),
                "punct" => ("\\p{P}", true, false),
                "space" => ("\\p{Z}\\t\\r\\n\\v\\f", true, false),
                "upper" => ("\\p{Lu}", true, false),
                "word" => ("\\p{L}\\p{Nl}\\p{Nd}\\p{Pc}", true, false),
                "xdigit" => ("A-Fa-f0-9", false, false),
                _ => ("", false, false),
            };
            if !source.is_empty() {
                if negative {
                    negatives.push(source.to_owned());
                } else {
                    ranges.push(source.to_owned());
                }
                unicode |= needs_unicode;
                i = end + 2;
                continue;
            }
        }
        let escape = |c: char| if "[]\\-".contains(c) { format!("\\{c}") } else { c.to_string() };
        let c = chars[i];
        if chars.get(i + 1) == Some(&'-') && chars.get(i + 2).is_some_and(|&c| c != ']') {
            let last = chars[i + 2];
            if c < last {
                ranges.push(format!("{}-{}", escape(c), escape(last)));
            } else if c == last {
                ranges.push(escape(c));
            }
            i += 3;
        } else {
            ranges.push(escape(c));
            i += 1;
        }
    }
    None
}

enum GlobPart {
    Recursive,
    Segment(Re),
}

struct Glob {
    variants: Vec<Vec<GlobPart>>,
}

impl Glob {
    fn matches(&self, path: &str) -> bool {
        self.test(&path.encode_utf16().collect::<Vec<_>>())
    }

    fn test(&self, path: &[u16]) -> bool {
        let path = path.strip_suffix(&[47]).unwrap_or(path);
        let segments: Vec<_> = path.split(|&c| c == 47).collect();
        self.variants.iter().any(|variant| {
            let mut positions = vec![false; segments.len() + 1];
            positions[0] = true;
            for part in variant {
                let mut next = vec![false; segments.len() + 1];
                for i in 0..=segments.len() {
                    match part {
                        GlobPart::Recursive => {
                            next[i] |= positions[i];
                            if i < segments.len() && next[i] && !segments[i].is_empty() && segments[i][0] != 46 {
                                next[i + 1] = true;
                            }
                        }
                        GlobPart::Segment(re) if i < segments.len() && positions[i] && re.test(segments[i]) => {
                            next[i + 1] = true;
                        }
                        GlobPart::Segment(_) => {}
                    }
                }
                positions = next;
            }
            positions[segments.len()]
        })
    }
}

fn is_pattern(chars: &[char]) -> bool {
    chars.iter().any(|c| "*?[".contains(*c)) || chars.windows(2).any(|w| "+@!".contains(w[0]) && w[1] == '(')
}

fn compile_glob(mask: &str) -> Option<Glob> {
    let mask = mask.replace('\\', "/");
    let mut variants = Vec::new();
    for mask in expand_braces(&mask.chars().collect::<Vec<_>>()) {
        let mut parts = Vec::new();
        for segment in mask.split(|&c| c == '/') {
            if segment == ['*', '*'] {
                parts.push(GlobPart::Recursive);
                continue;
            }
            let mut unicode = false;
            let source = segment_source(segment, &mut unicode);
            let insensitive = cfg!(any(target_os = "macos", target_os = "windows")) && is_pattern(segment);
            let flags = match (insensitive, unicode) {
                (true, true) => "iu",
                (true, false) => "i",
                (false, true) => "u",
                (false, false) => "",
            };
            parts.push(GlobPart::Segment(Re::try_new(&format!("^(?:{source})$"), flags).ok()?));
        }
        variants.push(parts);
    }
    Some(Glob { variants })
}

fn directory_patterns(mask: &str) -> Option<Vec<Glob>> {
    let mut depth = 0;
    let mut prefixes = Vec::new();
    for (offset, c) in mask.char_indices() {
        match c {
            '{' | '(' | '[' => depth += 1,
            '}' | ')' | ']' => depth -= 1,
            _ => {}
        }
        if c == '/' {
            if depth != 0 {
                return None;
            }
            if let Some(pattern) = compile_glob(&mask[..offset]) {
                prefixes.push(pattern);
            }
        }
    }
    if mask.split('/').next_back() == Some("**")
        && let Some(pattern) = compile_glob(mask)
    {
        prefixes.push(pattern);
    }
    Some(prefixes)
}

struct IgnoreRule {
    negate: bool,
    directory: bool,
    pattern: Option<Glob>,
}
pub struct Ignore {
    root: PathBuf,
    rules: Vec<IgnoreRule>,
}
impl Ignore {
    pub fn load(root: &Path) -> Result<Self> {
        let path = root.join(".microtypoignore");
        let mut bytes = Vec::new();
        match std::fs::File::open(&path) {
            Ok(mut file) => {
                std::io::Read::read_to_end(&mut file, &mut bytes).map_err(|e| system(&e, "read", Some(&path), None))?;
            }
            Err(e) if is_missing(&e) => {}
            Err(e) => return Err(system(&e, "open", Some(&path), None)),
        }
        let source = String::from_utf8_lossy(&bytes);
        let mut rules = Vec::new();
        for raw in source.split(['\r', '\n']) {
            let line = raw.trim_matches(crate::js::is_js_space);
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let negate = line.starts_with('!');
            let body = if negate { &line[1..] } else { line };
            if body.is_empty()
                || body.contains(['\\', '{', '}'])
                || body.as_bytes().windows(2).any(|w| b"!+@?*".contains(&w[0]) && w[1] == b'(')
                || body.encode_utf16().count() > 1024
            {
                return Err(usage("unsupported .microtypoignore pattern"));
            }
            let mask = body
                .strip_prefix('/')
                .unwrap_or(body)
                .strip_suffix('/')
                .unwrap_or_else(|| body.strip_prefix('/').unwrap_or(body));
            let mask = if body.starts_with('/') || mask.contains('/') { mask.to_owned() } else { format!("**/{mask}") };
            rules.push(IgnoreRule { negate, directory: body.ends_with('/'), pattern: compile_glob(&mask) });
        }
        Ok(Self { root: root.to_path_buf(), rules })
    }

    pub fn contains(&self, path: &Path) -> bool {
        let path = relative(&self.root, path);
        if path == ".." || path.starts_with("../") {
            return false;
        }
        let path: Vec<u16> = path.encode_utf16().collect();
        let ends: Vec<usize> = (0..path.len()).filter(|&i| path[i] == u16::from(b'/')).chain([path.len()]).collect();
        let mut ignored = false;
        for rule in &self.rules {
            if ends.iter().enumerate().any(|(i, &end)| {
                (!rule.directory || i < ends.len() - 1) && rule.pattern.as_ref().is_some_and(|re| re.test(&path[..end]))
            }) {
                ignored = !rule.negate;
            }
        }
        ignored
    }
}

fn info(path: &Path, follow: bool) -> Result<Option<std::fs::Metadata>> {
    if_present(if follow { std::fs::metadata(path) } else { std::fs::symlink_metadata(path) }, |e| {
        system(e, if follow { "stat" } else { "lstat" }, Some(path), None)
    })
}

pub fn select_files(arguments: &[String], cwd: &Path, ignored: &Ignore) -> Result<Vec<String>> {
    let clock = crate::run::HOST.now_ms;
    microtypo_core::internals::with_budget(clock, clock(), 30_000.0, || {
        let result = select_files_inner(arguments, cwd, ignored);
        if microtypo_core::internals::out_of_budget() {
            return Err(discovery_limit());
        }
        result
    })
}

fn discovery_limit() -> CliError {
    usage("file discovery exceeded its entry or time limit")
}

fn is_selectable(path: &str, documents: bool) -> bool {
    known_format(path).is_some_and(|format| !documents || matches!(format, "html" | "markdown" | "mdx"))
        && !path.ends_with(".microtyporc.json")
}

fn is_found_file(path: &Path) -> Result<bool> {
    Ok(info(path, false)?.is_some_and(|m| !m.is_dir() && !m.file_type().is_symlink()))
}

fn excluded_discovery(root: &Path, path: &Path) -> bool {
    let path = relative(root, path);
    let mut parts = path.split('/').peekable();
    while let Some(part) = parts.next() {
        if parts.peek().is_some() && matches!(part, ".git" | "node_modules") {
            return true;
        }
    }
    false
}

fn select_files_inner(arguments: &[String], cwd: &Path, ignored: &Ignore) -> Result<Vec<String>> {
    let mut selected = BTreeSet::new();
    let start = Instant::now();
    let within_time = || start.elapsed().as_secs() < 30;
    let mut visited = 0;
    let mut add = |path: PathBuf| -> Result<()> {
        if !ignored.contains(&path) {
            selected.insert(path);
            if selected.len() > 10_000 {
                return Err(usage("too many files (max 10000)"));
            }
        }
        Ok(())
    };
    for argument in arguments {
        let absolute = resolve(cwd, argument);
        let mut entry = info(&absolute, false)?;
        if entry.as_ref().is_some_and(|m| m.file_type().is_symlink()) {
            entry = info(&absolute, true)?.or(entry);
        }
        if entry.as_ref().is_some_and(|m| !m.is_dir()) {
            add(absolute)?;
            continue;
        }
        let walk = entry.is_some();
        let (mut root, mut mask) =
            if walk { (absolute, "**/*".to_owned()) } else { (cwd.to_path_buf(), argument.replace('\\', "/")) };
        let discovery_root = root.clone();
        if entry.is_none() {
            let segments: Vec<_> = mask.split('/').collect();
            let fixed = segments
                .iter()
                .take(segments.len().saturating_sub(1))
                .take_while(|s| !s.contains(['*', '?', '[', '{', '(']))
                .count();
            if fixed > 0 {
                let prefix = segments[..fixed].join("/");
                root = resolve(&root, if prefix.is_empty() { "/" } else { &prefix });
                mask = segments[fixed..].join("/");
            }
            if info(&root, true)?.is_none_or(|m| !m.is_dir()) {
                return Err(usage(format!("no files match: {argument}")));
            }
        }
        let pattern = compile_glob(&mask);
        let directory_patterns = directory_patterns(&mask);
        let mut directories = vec![root.clone()];
        let mut matched = false;
        for variant in expand_braces(&mask.chars().collect::<Vec<_>>()) {
            if is_pattern(&variant) {
                continue;
            }
            let variant: String = variant.iter().collect();
            let file = resolve(&root, &variant);
            if !excluded_discovery(&discovery_root, &file) && is_found_file(&file)? {
                matched = true;
                if is_selectable(&variant, walk) {
                    add(file)?;
                }
            }
        }
        while let Some(directory) = directories.pop() {
            if !within_time() {
                return Err(discovery_limit());
            }
            let Ok(listing) = std::fs::read_dir(&directory) else { continue };
            for item in listing {
                if !within_time() {
                    return Err(discovery_limit());
                }
                let Ok(item) = item else { break };
                let path = item.path();
                let file_name = item.file_name();
                let name = file_name.to_string_lossy();
                let kind = item.file_type().map_err(|e| system(&e, "stat", Some(&path), None))?;
                let relative_path = relative(&root, &path);
                if excluded_discovery(&discovery_root, &path) {
                    continue;
                }
                if kind.is_dir()
                    && name != ".git"
                    && name != "node_modules"
                    && directory_patterns
                        .as_ref()
                        .is_none_or(|patterns| patterns.iter().any(|re| re.matches(&relative_path)))
                {
                    directories.push(path.clone());
                }
                let candidate = if mask.starts_with('/') { path.to_string_lossy().into_owned() } else { relative_path };
                if !pattern.as_ref().is_some_and(|re| re.matches(&candidate)) {
                    continue;
                }
                visited += 1;
                if visited > 100_000 || !within_time() {
                    return Err(discovery_limit());
                }
                if !is_found_file(&path)? {
                    continue;
                }
                matched = true;
                if is_selectable(&path.to_string_lossy(), walk) {
                    add(path)?;
                }
            }
        }
        if entry.is_none() && !matched {
            return Err(usage(format!("no files match: {argument}")));
        }
    }
    let mut selected: Vec<_> = selected.into_iter().collect();
    selected.sort_by_cached_key(|path| path.to_string_lossy().encode_utf16().collect::<Vec<_>>());
    Ok(selected.iter().map(|path| relative(cwd, path)).collect())
}
