use std::fmt;
use std::sync::LazyLock;

use crate::errors::Result;
use crate::js::{self, JsString, Re};
use crate::protect::placeholders::PARAGRAPH_CLOSE;
use crate::shared::glyphs::{BDQUO, LAQUO, LDQUO, MDASH, NDASH, PRIME, RAQUO, unit};

const LAQUO_U: u16 = unit(LAQUO);
const RAQUO_U: u16 = unit(RAQUO);
const BDQUO_U: u16 = unit(BDQUO);
const LDQUO_U: u16 = unit(LDQUO);
const PRIME_U: u16 = unit(PRIME);
const QUOT: u16 = 0x22;

pub const SEPARATOR: &str = r" \t\r\n";
static SEPARATOR_RE: LazyLock<Re> = LazyLock::new(|| Re::new(&format!("[{SEPARATOR}]"), "u"));

pub static OPEN_LEFT: LazyLock<String> = LazyLock::new(|| format!(r"\s([{LAQUO}{MDASH}{NDASH}>-"));
static OPEN_LEFT_RE: LazyLock<Re> = LazyLock::new(|| Re::new(&format!("[{}]", *OPEN_LEFT), "u"));

const SCAN_LIMIT: usize = 32;

const FAR: i32 = 1 << 29;

pub fn paragraph_separator(text: &[u16]) -> JsString {
    if js::includes(text, &PARAGRAPH_CLOSE) {
        return PARAGRAPH_CLOSE.to_vec();
    }
    js::u(if js::includes(text, &[0x0D, 0x0A]) { "\r\n\r\n" } else { "\n\n" })
}

type Budget<'a> = Option<&'a dyn Fn(&str) -> Result<()>>;

pub struct QuoteOptions<'a> {
    pub allow_nested: bool,
    pub convert_inches: bool,
    pub check_budget: Budget<'a>,
}

impl Default for QuoteOptions<'_> {
    fn default() -> Self {
        QuoteOptions { allow_nested: true, convert_inches: true, check_budget: None }
    }
}

fn next_quote(buf: &[u16], off: usize) -> Option<(usize, u16)> {
    (off..buf.len()).find(|&i| buf[i] == LAQUO_U || buf[i] == RAQUO_U).map(|i| (i, buf[i]))
}

pub fn process_quotes(text: &[u16], options: &QuoteOptions<'_>) -> Result<JsString> {
    if !text.contains(&LAQUO_U) && !text.contains(&RAQUO_U) {
        return Ok(text.to_vec());
    }
    let separator = paragraph_separator(text);
    let mut out = Vec::new();
    for chunk in js::split(text, &separator) {
        out.push(process_chunk(chunk, options.allow_nested, options.convert_inches, options.check_budget)?);
    }
    Ok(js::join(&out, &separator))
}

fn process_chunk(
    chunk: &[u16],
    allow_nested: bool,
    convert_inches: bool,
    check_budget: Budget<'_>,
) -> Result<JsString> {
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
        if level < 0 && opens_here(&buf, pos) {
            buf[pos] = LAQUO_U;
            level = 1;
            continue;
        }
        if level < 0 && convert_inches {
            let many = *many.get_or_insert_with(|| count_quote_marks(chunk) > SCAN_LIMIT);
            if many {
                return walk_back(chunk, buf, &balanced_stack, last_balanced, off, allow_nested, check_budget);
            }
            let (fallback, next) = scan_convert_inches(&mut buf, off, &mut balanced_stack);
            off = next;
            level = 0;
            if fallback {
                balanced_stack.clear();
                balanced_stack.push(off);
            }
        }
    }
    if level > 0 {
        let end = buf.len();
        restore_nested_marks(&mut buf, last_balanced, end);
    }
    Ok(buf)
}

fn opens_here(buf: &[u16], at: usize) -> bool {
    if at > 0 && !OPEN_LEFT_RE.test(&buf[at - 1..at]) {
        return false;
    }
    let mut i = at + 1;
    while buf.get(i) == Some(&0x20) || buf.get(i) == Some(&0x09) {
        i += 1;
    }
    i < buf.len() && !SEPARATOR_RE.test(&buf[i..=i])
}

fn count_quote_marks(chunk: &[u16]) -> usize {
    chunk.iter().filter(|&&c| is_quote_mark(c)).count()
}

fn scan_convert_inches(buf: &mut [u16], off: usize, balanced_stack: &mut Vec<usize>) -> (bool, usize) {
    loop {
        let candidate_start = balanced_stack.pop().unwrap_or(0);
        let had_nested_marks = has_nested_marks(buf, candidate_start, off);
        if let Some(quote_pos) = find_last_inch_quote(buf, candidate_start, off, had_nested_marks) {
            if had_nested_marks {
                restore_nested_marks(buf, candidate_start, off);
            }
            buf[quote_pos] = PRIME_U;
            if had_nested_marks {
                return (false, candidate_start);
            }
            balanced_stack.push(off);
            return (false, off);
        }
        if balanced_stack.is_empty() {
            break;
        }
    }
    let fallback_pos = off - 1;
    buf[fallback_pos] = QUOT;
    (true, fallback_pos + 1)
}

fn has_nested_marks(buf: &[u16], start: usize, end: usize) -> bool {
    (start..end).any(|i| buf[i] == BDQUO_U || buf[i] == LDQUO_U)
}

fn find_last_inch_quote(buf: &[u16], start: usize, end: usize, include_nested_close: bool) -> Option<usize> {
    let mut i = end as isize - 1;
    while i > start as isize {
        let k = i as usize;
        let ch = buf[k];
        if (ch == RAQUO_U || (include_nested_close && ch == LDQUO_U)) && is_digit(Some(buf[k - 1])) {
            return Some(k);
        }
        i -= 1;
    }
    None
}

fn is_quote_mark(ch: u16) -> bool {
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
    fn new(buf: JsString, source: &[u16]) -> Self {
        let at: Vec<usize> = (0..source.len()).filter(|&i| is_quote_mark(source[i])).collect();
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
            let after_digit = pos > 0 && is_digit(Some(marks.buf[pos - 1]));
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

    #[allow(clippy::too_many_arguments)]
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

fn walk_back(
    chunk: &[u16],
    buf: JsString,
    balanced_points: &[usize],
    last_balanced_at: usize,
    off_at: usize,
    allow_nested: bool,
    check_budget: Budget<'_>,
) -> Result<JsString> {
    let mut marks = QuoteMarks::new(buf, chunk);
    let mut balanced_stack: Vec<Balanced> =
        balanced_points.iter().map(|&pos| Balanced::Point(marks.index_at(pos))).collect();
    let mut last_balanced = marks.index_at(last_balanced_at);
    let start = marks.index_at(off_at);
    let mut off = try_convert_inches(&mut marks, start, &mut balanced_stack);
    let mut level: i64 = 0;
    let mut iter: u32 = 0;

    loop {
        if let Some(cb) = check_budget {
            let n = iter;
            iter = iter.wrapping_add(1);
            if n & 0x3ff == 0 {
                cb("quote:processChunk")?;
            }
        }
        let p = marks.closer_below(off, level);
        let end = if p == -1 { marks.length } else { p as usize };
        let zeros = marks.zeros(off, end, level);
        marks.restyle(off, end, level, allow_nested);
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
        if opens_here(&marks.buf, marks.at[p]) {
            marks.write_role(p, LAQUO_U, 1);
            level = 1;
        } else {
            off = try_convert_inches(&mut marks, off, &mut balanced_stack);
            level = 0;
        }
    }
    if level > 0 {
        marks.restore(last_balanced);
    }
    Ok(marks.buf)
}

fn try_convert_inches(marks: &mut QuoteMarks, off: usize, balanced_stack: &mut Vec<Balanced>) -> usize {
    let quote_pos = marks.last_inch_quote(off as isize - 1);
    let candidate_start = if balanced_stack.is_empty() { 0 } else { pop_balanced(marks, balanced_stack, quote_pos) };

    if candidate_start != -1 && quote_pos >= candidate_start {
        let candidate_start = candidate_start as usize;
        let had_nested_marks = marks.has_nested(candidate_start, off);
        if had_nested_marks {
            marks.activate(candidate_start, off);
        }
        marks.write_role(quote_pos as usize, PRIME_U, 0);
        if had_nested_marks {
            return candidate_start;
        }
        balanced_stack.push(Balanced::Point(off));
        return off;
    }
    marks.write_role(off - 1, QUOT, 0);
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
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::{QuoteOptions, process_quotes};
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
            let options = QuoteOptions { allow_nested, convert_inches, check_budget: None };
            assert_eq!(lossy(&process_quotes(&u(input), &options).unwrap()), expected, "input: {input:?}");
        }
    }
}
