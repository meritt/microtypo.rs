use std::collections::HashMap;

use crate::errors::Result;
use crate::js;

const ANCHORS: [usize; 3] = [4, 2, 1];
const SHORT_SEARCH: usize = 32;
const WINDOW_GROWTH: usize = 4;
const MYERS_SPAN: usize = 128;

pub type Range = (usize, usize);

struct Steps<'a> {
    seen: usize,
    check_budget: &'a dyn Fn(&str) -> Result<()>,
}

pub fn changed_ranges(before: &[u16], after: &[u16], check_budget: &dyn Fn(&str) -> Result<()>) -> Result<Vec<Range>> {
    let mut ranges = Vec::new();
    let mut steps = Steps { seen: 0, check_budget };
    walk(before, 0, before.len(), after, 0, after.len(), 0, &mut ranges, &mut steps)?;
    Ok(merge_touching(ranges))
}

#[allow(clippy::too_many_arguments)]
fn walk(
    a: &[u16],
    a_start: usize,
    a_end: usize,
    b: &[u16],
    b_start: usize,
    b_end: usize,
    level: usize,
    out: &mut Vec<Range>,
    steps: &mut Steps<'_>,
) -> Result<()> {
    let anchor = ANCHORS[level];
    let mut i = a_start;
    let mut j = b_start;
    loop {
        if steps.seen & 0x3f == 0 {
            (steps.check_budget)("changes")?;
        }
        steps.seen += 1;
        while i < a_end && j < b_end && a[i] == b[j] {
            i += 1;
            j += 1;
        }
        if i == a_end || j == b_end {
            if i != a_end || j != b_end {
                out.push((j, b_end));
            }
            return Ok(());
        }
        let (i2, j2) = resync(a, i, a_end, b, j, b_end, anchor);
        if i2 - i + (j2 - j) <= MYERS_SPAN {
            diff_into(a, i, i2, b, j, j2, out);
        } else if level + 1 < ANCHORS.len() {
            walk(a, i, i2, b, j, j2, level + 1, out, steps)?;
        } else {
            out.push((j, j2));
        }
        i = i2;
        j = j2;
    }
}

fn resync(a: &[u16], i: usize, a_end: usize, b: &[u16], j: usize, b_end: usize, anchor: usize) -> (usize, usize) {
    for skipped in 1..=SHORT_SEARCH {
        for di in 0..=skipped {
            if anchored(a, i + di, a_end, b, j + skipped - di, b_end, anchor) {
                return (i + di, j + skipped - di);
            }
        }
    }
    let mut width = SHORT_SEARCH * 2;
    loop {
        if let Some(found) = nearest_common_gram(a, i, a_end, b, j, b_end, anchor, width) {
            return found;
        }
        width *= WINDOW_GROWTH;
    }
}

fn anchored(a: &[u16], p: usize, a_end: usize, b: &[u16], q: usize, b_end: usize, anchor: usize) -> bool {
    if p > a_end || q > b_end {
        return false;
    }
    let length = anchor.min(a_end - p).min(b_end - q);
    if length < anchor && a_end - p != b_end - q {
        return false;
    }
    (0..length).all(|k| a[p + k] == b[q + k])
}

#[allow(clippy::too_many_arguments)]
fn nearest_common_gram(
    a: &[u16],
    i: usize,
    a_end: usize,
    b: &[u16],
    j: usize,
    b_end: usize,
    anchor: usize,
    width: usize,
) -> Option<(usize, usize)> {
    let a_stop = a_end.min(i + width);
    let b_stop = b_end.min(j + width);
    let mut first: HashMap<i32, usize> = HashMap::new();
    let mut p = i;
    while p + anchor <= a_stop {
        first.entry(gram_hash(a, p, anchor)).or_insert(p);
        p += 1;
    }
    let mut best = if a_stop == a_end && b_stop == b_end { Some((a_end, b_end)) } else { None };
    let mut best_skipped = if best.is_none() { usize::MAX } else { a_end - i + (b_end - j) };
    let mut q = j;
    while q + anchor <= b_stop && q - j < best_skipped {
        if let Some(&p) = first.get(&gram_hash(b, q, anchor))
            && p - i + (q - j) < best_skipped
            && anchored(a, p, a_end, b, q, b_end, anchor)
        {
            best = Some((p, q));
            best_skipped = p - i + (q - j);
        }
        q += 1;
    }
    best
}

fn gram_hash(text: &[u16], start: usize, length: usize) -> i32 {
    let mut hash: i32 = 0;
    for k in 0..length {
        let cp = js::code_point_at(text, start + k).unwrap_or(0) as i32;
        hash = hash.wrapping_mul(65_599).wrapping_add(cp);
    }
    hash
}

fn diff_into(a: &[u16], a_start: usize, a_end: usize, b: &[u16], b_start: usize, b_end: usize, out: &mut Vec<Range>) {
    let (mut a0, mut a1, mut b0, mut b1) = (a_start, a_end, b_start, b_end);
    while a0 < a1 && b0 < b1 && a[a0] == b[b0] {
        a0 += 1;
        b0 += 1;
    }
    while a0 < a1 && b0 < b1 && a[a1 - 1] == b[b1 - 1] {
        a1 -= 1;
        b1 -= 1;
    }
    if a0 == a1 || b0 == b1 {
        if a0 != a1 || b0 != b1 {
            out.push((b0, b1));
        }
        return;
    }
    match middle_snake(a, a0, a1, b, b0, b1) {
        None => out.push((b0, b1)),
        Some((sa, sb)) => {
            diff_into(a, a0, sa, b, b0, sb, out);
            diff_into(a, sa, a1, b, sb, b1, out);
        }
    }
}

fn middle_snake(a: &[u16], a0: usize, a1: usize, b: &[u16], b0: usize, b1: usize) -> Option<(usize, usize)> {
    let n = (a1 - a0) as i64;
    let m = (b1 - b0) as i64;
    let max_d = (n + m + 1) / 2;
    let offset = max_d;
    let size = 2 * max_d + 2;
    let mut forward = vec![-1i64; size as usize];
    let mut backward = vec![-1i64; size as usize];
    let delta = n - m;
    let meet_forward = delta % 2 != 0;
    let (mut k1start, mut k1end, mut k2start, mut k2end) = (0i64, 0i64, 0i64, 0i64);
    forward[(offset + 1) as usize] = 0;
    backward[(offset + 1) as usize] = 0;
    let at = |s: usize, i: i64| s as i64 + i;
    for d in 0..max_d {
        let mut k1 = -d + k1start;
        while k1 <= d - k1end {
            let i = (offset + k1) as usize;
            let mut x1 = if k1 == -d || (k1 != d && forward[i - 1] < forward[i + 1]) {
                forward[i + 1]
            } else {
                forward[i - 1] + 1
            };
            let mut y1 = x1 - k1;
            while x1 < n && y1 < m && a[at(a0, x1) as usize] == b[at(b0, y1) as usize] {
                x1 += 1;
                y1 += 1;
            }
            forward[i] = x1;
            if x1 > n {
                k1end += 2;
            } else if y1 > m {
                k1start += 2;
            } else if meet_forward {
                let j = offset + delta - k1;
                if j >= 0 && j < size && backward[j as usize] != -1 && x1 >= n - backward[j as usize] {
                    return Some((at(a0, x1) as usize, at(b0, y1) as usize));
                }
            }
            k1 += 2;
        }
        let mut k2 = -d + k2start;
        while k2 <= d - k2end {
            let j = (offset + k2) as usize;
            let mut x2 = if k2 == -d || (k2 != d && backward[j - 1] < backward[j + 1]) {
                backward[j + 1]
            } else {
                backward[j - 1] + 1
            };
            let mut y2 = x2 - k2;
            while x2 < n && y2 < m && a[(a1 as i64 - x2 - 1) as usize] == b[(b1 as i64 - y2 - 1) as usize] {
                x2 += 1;
                y2 += 1;
            }
            backward[j] = x2;
            if x2 > n {
                k2end += 2;
            } else if y2 > m {
                k2start += 2;
            } else if !meet_forward {
                let i = offset + delta - k2;
                if i >= 0 && i < size && forward[i as usize] != -1 {
                    let x1 = forward[i as usize];
                    if x1 >= n - x2 {
                        return Some((at(a0, x1) as usize, at(b0, x1 - (i - offset)) as usize));
                    }
                }
            }
            k2 += 2;
        }
    }
    None
}

fn merge_touching(ranges: Vec<Range>) -> Vec<Range> {
    let mut merged: Vec<Range> = Vec::new();
    for range in ranges {
        if let Some(last) = merged.last_mut()
            && range.0 <= last.1
        {
            last.1 = last.1.max(range.1);
            continue;
        }
        merged.push(range);
    }
    merged
}
