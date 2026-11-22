use std::collections::{HashMap, HashSet};
use std::sync::{Arc, LazyLock};

use crate::errors::Result;
use crate::js::{self, Caps, JsString, Re};
use crate::protect::placeholders::{CLOSE_UNIT, OPEN_UNIT};
use crate::protect::sequences::SafeSequences;
use crate::protect::tags::SafeTags;
pub(crate) use crate::rule_runner::LiftFlags;
use crate::rule_runner::{Ctx, GroupView, SpacedYears};
use crate::shared::block_tags::VOID_TAG_NAMES;
use crate::shared::diff::Edit;
use crate::shared::glyphs::{BDQUO, HELLIP, LAQUO, LDQUO, LSQUO, MDASH, NBSP, NDASH, PRIME, RAQUO, RSQUO, unit};
use crate::shared::nowrap_context::NowrapFrame;
use crate::shared::rule_features::{Requires, allows};
use crate::shared::strings;

const OPEN_CODE: i32 = 0xE000;
const LT: i32 = 0x3C;
const GT: i32 = 0x3E;
const SLASH: i32 = 0x2F;
const NL: i32 = 0x0A;
const BACKTICK: i32 = 0x60;
const BACKSLASH: i32 = 0x5C;
const UNDERSCORE: i32 = 0x5F;
const STAR: i32 = 0x2A;
const TILDE: i32 = 0x7E;
const DELIMITERS: [i32; 3] = [STAR, UNDERSCORE, TILDE];
const LBRACKET: i32 = 0x5B;
const RBRACKET: i32 = 0x5D;
const LPAREN: i32 = 0x28;
const RPAREN: i32 = 0x29;
const BANG: i32 = 0x21;
const HYPHEN: i32 = 0x2D;

const STAND_IN: &str = "слово";
const STAND_IN_LEN: isize = 5;
const STAND_IN_FIRST: i32 = 0x441;

const CLOSER: u8 = 1;
const STANDS_IN: u8 = 2;

const PIN: i32 = -2;
const EDGE: i32 = -1;

const KIND_M: u16 = 0x4D;
const KIND_U: u16 = 0x55;
const KIND_N: u16 = 0x4E;
const KIND_B: u16 = 0x42;
const KIND_T: u16 = 0x54;
const KIND_A: u16 = 0x41;
const KIND_I: u16 = 0x49;
const KIND_H: u16 = 0x48;

const NEAR: isize = 16;
const REACH: usize = 2;
const SPAN: usize = 8;
const LOOK: isize = 16;
const LINES: usize = 3;
const ROUNDS: usize = 4;

static WORD_RE: LazyLock<Re> = LazyLock::new(|| Re::new(r"[\p{L}\p{N}\p{M}]", "u"));
static PUNCT_RE: LazyLock<Re> = LazyLock::new(|| Re::new(r"[\p{P}\p{S}]", "u"));
static ADDRESS_RE: LazyLock<Re> =
    LazyLock::new(|| Re::new(r#"^a href="(?:(?:https?|ftp):\/\/|mailto:)([^"]*)"$"#, "u"));
static TAG_START_RE: LazyLock<Re> = LazyLock::new(|| Re::new(r"<[A-Za-z/]", "u"));

static STAND_IN_UNITS: LazyLock<JsString> = LazyLock::new(|| js::u(STAND_IN));

fn cp(s: &[u16], i: isize) -> i32 {
    if i < 0 {
        return -1;
    }
    js::code_point_at(s, i as usize).map_or(-1, |c| c as i32)
}

fn len(s: &[u16]) -> isize {
    s.len() as isize
}

fn index_of_unit(s: &[u16], unit: u16, from: isize) -> isize {
    js::index_of_unit(s, unit, from.max(0) as usize).map_or(-1, |p| p as isize)
}

fn last_index_of_unit(s: &[u16], unit: u16, from: isize) -> isize {
    if s.is_empty() {
        return -1;
    }
    let start = from.clamp(0, len(s) - 1) as usize;
    s[..=start].iter().rposition(|&c| c == unit).map_or(-1, |p| p as isize)
}

fn starts_with_at(s: &[u16], needle: &[u16], at: isize) -> bool {
    at >= 0 && js::starts_with_at(s, needle, at as usize)
}

pub fn is_space(c: i32) -> bool {
    u16::try_from(c).is_ok_and(js::is_js_space)
}

fn is_word_char(c: i32) -> bool {
    if c < 0 {
        return false;
    }
    if c < 128 {
        return (48..=57).contains(&c) || (97..=122).contains(&(c | 32));
    }
    if (0x400..=0x4FF).contains(&c) {
        return true;
    }
    if c < 0xC0 || is_pua(c) {
        return false;
    }
    WORD_RE.test(&js::from_code_point(c as u32))
}

fn is_quote(c: i32) -> bool {
    const MARKS: [char; 13] = ['"', '\'', LAQUO, RAQUO, BDQUO, LDQUO, LSQUO, RSQUO, '”', '‚', '‹', '›', PRIME];
    c >= 0 && MARKS.iter().any(|&q| q as i32 == c)
}

fn is_edge(c: i32) -> bool {
    c == EDGE || is_space(c)
}

fn is_pua(c: i32) -> bool {
    (0xE000..=0xF8FF).contains(&c)
}

fn md_space(c: i32) -> bool {
    c < 0 || is_space(c) || is_pua(c)
}

fn md_punct(c: i32) -> bool {
    if c < 128 {
        return c >= 0 && u8::try_from(c).is_ok_and(|b| b.is_ascii_punctuation());
    }
    !md_space(c) && !is_word_char(c) && PUNCT_RE.test(&js::from_code_point(c as u32))
}

fn escaped(text: &[u16], at: isize, from: isize) -> bool {
    let mut n = 0;
    let mut i = at - 1;
    while i >= from && cp(text, i) == BACKSLASH {
        n += 1;
        i -= 1;
    }
    n % 2 == 1
}

struct Link {
    open: isize,
    close: isize,
    end: isize,
    image: bool,
}

fn find_links(text: &[u16], from: isize, to: isize, out: &mut Vec<Link>) {
    let mut opens: Vec<isize> = Vec::new();
    let mut i = from;
    while i < to {
        let c = cp(text, i);
        if (c != LBRACKET && c != RBRACKET) || escaped(text, i, from) {
            i += 1;
            continue;
        }
        if c == LBRACKET {
            opens.push(i);
            i += 1;
            continue;
        }
        let open = opens.pop().unwrap_or(-1);
        let next = cp(text, i + 1);
        if (next != LPAREN && next != LBRACKET) || cp(text, i + 2) != OPEN_CODE {
            i += 1;
            continue;
        }
        let shut = index_of_unit(text, CLOSE_UNIT, i + 3);
        let end = if next == LPAREN { RPAREN } else { RBRACKET };
        if shut == -1 || shut >= to || cp(text, shut + 1) != end {
            i += 1;
            continue;
        }
        if open != -1 {
            out.push(Link { open, close: i, end: shut + 2, image: cp(text, open - 1) == BANG });
        }
        i = shut + 2;
    }
}

#[derive(Clone)]
struct Piece {
    start: isize,
    end: isize,
    tag: bool,
    close: bool,
    kind: u16,
    id: isize,
    shut: isize,
    pinned: bool,
    unit: bool,
    lift: bool,
    stand_in: bool,
    mate: Option<usize>,
    index: isize,
}

fn piece(start: isize, end: isize, tag: bool, close: bool, kind: u16, id: isize, shut: isize) -> Piece {
    Piece {
        start,
        end,
        tag,
        close,
        kind,
        id,
        shut,
        pinned: false,
        unit: false,
        lift: false,
        stand_in: false,
        mate: None,
        index: -1,
    }
}

fn pair(marks: &mut Vec<Piece>, os: isize, oe: isize, cs: isize, ce: isize) {
    let at = marks.len();
    let mut open = piece(os, oe, false, false, KIND_M, 0, 0);
    let mut close = piece(cs, ce, false, true, KIND_M, 0, 0);
    open.mate = Some(at + 1);
    close.mate = Some(at);
    marks.push(open);
    marks.push(close);
}

const OPENS: u8 = 1;
const CLOSES: u8 = 2;

fn delimiter_role(c: i32, before: i32, after: i32) -> u8 {
    let left = !md_space(after) && (!md_punct(after) || md_space(before) || md_punct(before));
    let right = !md_space(before) && (!md_punct(before) || md_space(after) || md_punct(after));
    if c == UNDERSCORE {
        return (if left && (!right || md_punct(before)) { OPENS } else { 0 })
            | (if right && (!left || md_punct(after)) { CLOSES } else { 0 });
    }
    (if left { OPENS } else { 0 }) | (if right { CLOSES } else { 0 })
}

struct Opener {
    ch: i32,
    start: isize,
    rest: isize,
    length: isize,
    can_close: bool,
}

fn match_emphasis(text: &[u16], from: isize, to: isize, skip: &[(isize, isize)], marks: &mut Vec<Piece>) {
    let mut stack: Vec<Opener> = Vec::new();
    let mut bottom = [0usize; 18];
    let mut h = 0usize;
    let mut i = from;
    while i < to {
        if h < skip.len() && skip[h].0 <= i {
            i = skip[h].1;
            h += 1;
            continue;
        }
        let c = cp(text, i);
        if (c != STAR && c != UNDERSCORE && c != TILDE) || escaped(text, i, from) {
            i += 1;
            continue;
        }
        let mut j = i + 1;
        while j < to && cp(text, j) == c {
            j += 1;
        }
        let length = j - i;
        let before = if i > 0 { cp(text, i - 1) } else { -1 };
        let after = if j < len(text) { cp(text, j) } else { -1 };
        i = j;
        if c == TILDE && length != 2 {
            continue;
        }
        let role = delimiter_role(c, before, after);
        let can_open = role & OPENS != 0;
        let can_close = role & CLOSES != 0;
        let mut start = j - length;
        let mut rest = length;
        let d = DELIMITERS.iter().position(|&x| x == c).unwrap_or(0);
        let key = d * 6 + if can_open { 3 } else { 0 } + (length % 3) as usize;
        if can_close {
            let mut k = stack.len() as isize - 1;
            while k >= bottom[key] as isize && rest > 0 {
                let op = &mut stack[k as usize];
                if op.ch != c || op.rest == 0 {
                    k -= 1;
                    continue;
                }
                if (op.can_close || can_open)
                    && (op.length + length) % 3 == 0
                    && !(op.length % 3 == 0 && length % 3 == 0)
                {
                    k -= 1;
                    continue;
                }
                let used = op.rest.min(rest).min(2);
                op.rest -= used;
                let (os, op_rest) = (op.start, op.rest);
                pair(marks, os + op_rest, os + op_rest + used, start, start + used);
                start += used;
                rest -= used;
                stack.truncate(if op_rest > 0 { k as usize + 1 } else { k as usize });
                for b in &mut bottom {
                    *b = (*b).min(stack.len());
                }
                if rest > 0 {
                    k = stack.len() as isize;
                }
                k -= 1;
            }
            if rest > 0 {
                bottom[key] = stack.len();
            }
        }
        if rest > 0 && can_open {
            stack.push(Opener { ch: c, start, rest, length, can_close });
        }
    }
}

fn markdown_marks(text: &[u16]) -> Vec<Piece> {
    let mut marks: Vec<Piece> = Vec::new();
    let mut from: isize = 0;
    while from <= len(text) {
        let Some(found) = text[from as usize..].iter().position(|&c| matches!(c, 0x2A | 0x5F | 0x7E | 0x5D)) else {
            break;
        };
        let found = from + found as isize;
        from = last_index_of_unit(text, 0x0A, found) + 1;
        let mut to = index_of_unit(text, 0x0A, found);
        if to == -1 {
            to = len(text);
        }
        if to > from {
            let mut links = Vec::new();
            find_links(text, from, to, &mut links);
            let mut outer: Vec<(isize, isize)> = Vec::new();
            for l in &links {
                let mut k = outer.len();
                while k > 0 && outer[k - 1].0 > l.open {
                    k -= 1;
                }
                if !l.image {
                    let inner: Vec<(isize, isize)> = outer[k..].to_vec();
                    match_emphasis(text, l.open + 1, l.close, &inner, &mut marks);
                    pair(&mut marks, l.open, l.open + 1, l.close, l.end);
                }
                outer.truncate(k);
                outer.push((l.open, l.end));
            }
            match_emphasis(text, from, to, &outer, &mut marks);
        }
        from = to + 1;
    }
    let mut order: Vec<usize> = (0..marks.len()).collect();
    order.sort_by_key(|&i| marks[i].start);
    let mut at = vec![0usize; marks.len()];
    for (new, &old) in order.iter().enumerate() {
        at[old] = new;
    }
    order
        .iter()
        .map(|&old| {
            let mut p = marks[old].clone();
            p.mate = p.mate.map(|m| at[m]);
            p
        })
        .collect()
}

fn scan_placeholders(text: &[u16]) -> Vec<Piece> {
    let mut items = Vec::new();
    let mut j = index_of_unit(text, OPEN_UNIT, 0);
    while j != -1 {
        let shut = index_of_unit(text, CLOSE_UNIT, j + 1);
        if shut == -1 {
            break;
        }
        let kind = text.get((j + 1) as usize).copied().unwrap_or(0);
        let tagged = cp(text, shut + 1) == GT;
        if tagged && cp(text, j - 1) == LT {
            items.push(piece(j - 1, shut + 2, true, false, kind, j + 1, shut));
        } else if tagged && cp(text, j - 1) == SLASH && cp(text, j - 2) == LT {
            items.push(piece(j - 2, shut + 2, true, true, kind, j + 1, shut));
        } else {
            items.push(piece(j, shut + 1, false, false, kind, j + 1, shut));
        }
        j = index_of_unit(text, OPEN_UNIT, shut + 1);
    }
    items
}

fn js_number(digits: &[u16]) -> Option<usize> {
    if digits.is_empty() {
        return Some(0);
    }
    if !digits.iter().all(|c| (0x30..=0x39).contains(c)) {
        return None;
    }
    js::lossy(digits).parse().ok()
}

pub struct Lookup<'x> {
    tags: &'x mut SafeTags,
    sequences: &'x SafeSequences,
}

impl Lookup<'_> {
    fn name_of(&mut self, text: &[u16], item: &Piece) -> Option<JsString> {
        let id = js_number(js::slice_signed(text, item.id + 1, Some(item.shut)))?;
        self.tags.name_of(item.kind, id)
    }

    fn raw_of(&self, text: &[u16], item: &Piece) -> Option<JsString> {
        let id = js_number(js::slice_signed(text, item.id + 1, Some(item.shut)))?;
        self.tags.raw_of(item.kind, id).map(<[u16]>::to_vec)
    }
}

fn amp_free(s: &[u16]) -> JsString {
    js::replace_all_lit(s, &js::u("&amp;"), &[0x26])
}

fn note_engine_forms(text: &[u16], items: &[Piece], cx: &mut InlineCx, look: &mut Lookup<'_>) {
    for k in 0..items.len().saturating_sub(1) {
        let item = &items[k];
        let next = &items[k + 1];
        if !item.tag || item.close || !next.tag {
            continue;
        }
        let Some(name) = look.name_of(text, item) else { continue };
        let is_a = name == [0x61];
        if !is_a && name != js::u("sup") && name != js::u("sub") {
            continue;
        }
        let raw = look.raw_of(text, item);
        let after = look.raw_of(text, next);
        let open = js::slice_signed(text, item.start, Some(item.end)).to_vec();
        if !is_a {
            if raw.as_deref() == Some(name.as_slice())
                && !next.close
                && next.start == item.end
                && after.as_deref() == Some(&js::u("small")[..])
            {
                cx.engine_openers.insert(open, false);
                cx.engine_openers.insert(js::slice_signed(text, next.start, Some(next.end)).to_vec(), false);
            }
            continue;
        }
        let restored = look.sequences.restore(raw.unwrap_or_default());
        let body = look.sequences.restore(js::slice_signed(text, item.end, Some(next.start)).to_vec());
        if let Some(address) = ADDRESS_RE.exec(&restored)
            && next.close
            && after.as_deref() == Some(&[0x61][..])
            && amp_free(address.s(1)) == amp_free(&body)
        {
            cx.engine_openers.insert(open, false);
        }
    }
}

struct OpenElements<V: Copy> {
    stack: Vec<(JsString, V)>,
    by_name: HashMap<JsString, Vec<usize>>,
}

impl<V: Copy> OpenElements<V> {
    fn new() -> Self {
        OpenElements { stack: Vec::new(), by_name: HashMap::new() }
    }

    fn open(&mut self, name: JsString, value: V) {
        self.by_name.entry(name.clone()).or_default().push(self.stack.len());
        self.stack.push((name, value));
    }

    fn close(&mut self, name: &[u16]) -> Option<V> {
        let k = *self.by_name.get(name)?.last()?;
        let value = self.stack[k].1;
        for i in (k..self.stack.len()).rev() {
            if let Some(list) = self.by_name.get_mut(&self.stack[i].0) {
                list.pop();
            }
        }
        self.stack.truncate(k);
        Some(value)
    }
}

fn is_void(name: &[u16]) -> bool {
    VOID_TAG_NAMES.iter().any(|v| js::eq(name, v))
}

fn pin_engine_wrappers(text: &[u16], items: &mut [Piece], cx: &mut InlineCx, look: &mut Lookup<'_>) {
    note_engine_forms(text, items, cx, look);
    let mut open: OpenElements<bool> = OpenElements::new();
    for item in items.iter_mut() {
        if !item.tag {
            continue;
        }
        let Some(name) = look.name_of(text, item) else {
            item.pinned = true;
            continue;
        };
        if is_void(&name) {
            continue;
        }
        if !item.close {
            item.pinned = cx.engine_openers.contains_key(js::slice_signed(text, item.start, Some(item.end)))
                || look.raw_of(text, item).is_some_and(|raw| TAG_START_RE.test(&raw));
            open.open(name, item.pinned);
            continue;
        }
        item.pinned = open.close(&name) == Some(true);
    }
}

fn pair_tags(text: &[u16], items: &mut [Piece], look: &mut Lookup<'_>) {
    let mut open: OpenElements<usize> = OpenElements::new();
    for k in 0..items.len() {
        let item = &items[k];
        if !item.tag || (item.kind != KIND_T && item.kind != KIND_A) {
            continue;
        }
        let Some(name) = look.name_of(text, item) else { continue };
        if is_void(&name) {
            continue;
        }
        if !item.close {
            open.open(name, k);
            continue;
        }
        if let Some(element) = open.close(&name) {
            items[k].mate = Some(element);
            items[element].mate = Some(k);
        }
    }
}

fn inline_tag(p: Option<&Piece>) -> bool {
    p.is_some_and(|p| p.tag && (p.kind == KIND_T || p.kind == KIND_A))
}

fn find_units(text: &[u16], items: &mut [Piece], markdown: bool) -> Vec<Piece> {
    let mut units = Vec::new();
    for k in 0..items.len() {
        let item = &items[k];
        if item.tag || (item.kind != KIND_B && item.kind != KIND_N) {
            continue;
        }
        let prev = k.checked_sub(1).map(|p| &items[p]);
        let next = items.get(k + 1);
        if inline_tag(prev)
            && prev.is_some_and(|p| !p.close && p.end == item.start)
            && inline_tag(next)
            && next.is_some_and(|n| n.close && n.start == item.end)
        {
            let (start, end, kind) =
                (items[k - 1].start, items[k + 1].end, if item.kind == KIND_N { KIND_N } else { KIND_U });
            items[k - 1].unit = true;
            items[k + 1].unit = true;
            items[k].unit = true;
            units.push(piece(start, end, false, false, kind, 0, 0));
            continue;
        }
        if markdown && item.kind == KIND_B {
            let mut n: isize = 0;
            while cp(text, item.start - n - 1) == BACKTICK {
                n += 1;
            }
            let ticks = vec![0x60u16; n as usize];
            if n > 0 && starts_with_at(text, &ticks, item.end) && cp(text, item.end + n) != BACKTICK {
                let (start, end) = (item.start - n, item.end + n);
                items[k].unit = true;
                units.push(piece(start, end, false, false, KIND_U, 0, 0));
            }
        }
    }
    units
}

fn seen(p: &Piece) -> i32 {
    if p.kind == KIND_U {
        return if p.stand_in { STAND_IN_FIRST } else { PIN };
    }
    if p.kind == KIND_N || p.kind == KIND_I || p.kind == KIND_H { PIN } else { EDGE }
}

fn left_of(text: &[u16], all: &[Piece], i: usize) -> i32 {
    let at = all[i].start;
    if i > 0 && all[i - 1].end == at {
        return seen(&all[i - 1]);
    }
    if at == 0 {
        return EDGE;
    }
    let c = cp(text, at - 1);
    if is_pua(c) { EDGE } else { c }
}

fn right_of(text: &[u16], all: &[Piece], i: usize) -> i32 {
    let at = all[i].end;
    if i + 1 < all.len() && all[i + 1].start == at {
        return seen(&all[i + 1]);
    }
    if at >= len(text) {
        return EDGE;
    }
    let c = cp(text, at);
    if is_pua(c) { EDGE } else { c }
}

fn is_closing_mark(c: i32) -> bool {
    c >= 0 && [',', '.', ';', ':', '!', '?', HELLIP, ')', ']', '}'].iter().any(|&m| m as i32 == c)
}

fn is_opening_quote(c: i32) -> bool {
    c == LAQUO as i32 || c == BDQUO as i32
}

fn opens_quote_before(text: &[u16], at: isize) -> bool {
    let mut p = at;
    while p >= 0 && is_space(cp(text, p)) {
        p -= 1;
    }
    p >= 0 && is_opening_quote(cp(text, p)) && !is_word_char(cp(text, p - 1))
}

fn dash_sign(c: i32, beyond: i32) -> bool {
    c == MDASH as i32 || c == NDASH as i32 || (c == HYPHEN && !is_word_char(beyond))
}

fn liftable(text: &[u16], all: &[Piece], i: usize, j: usize, closes: bool, opens: bool) -> bool {
    let l = left_of(text, all, i);
    let r = right_of(text, all, j);
    if l == PIN || r == PIN || is_quote(l) || is_quote(r) {
        return false;
    }
    if is_word_char(l) && is_word_char(r) {
        return false;
    }
    if (closes && is_edge(l)) || (opens && is_edge(r)) {
        return false;
    }
    if opens && is_closing_mark(r) {
        return false;
    }
    if (closes && dash_sign(l, cp(text, all[i].start - 2))) || (opens && dash_sign(r, cp(text, all[j].end + 1))) {
        return false;
    }
    !(is_space(l) && opens_quote_before(text, all[i].start - 1))
}

fn has_inline_tags(text: &[u16]) -> bool {
    text.windows(2).any(|w| w[0] == OPEN_UNIT && (w[1] == KIND_T || w[1] == KIND_A))
}

fn has_marker(text: &[u16]) -> bool {
    text.iter().any(|&c| matches!(c, 0x2A | 0x5F | 0x7E | 0x5B | 0x60))
}

pub fn may_lift(text: &[u16], cx: &InlineCx) -> bool {
    has_inline_tags(text) || (cx.markdown && has_marker(text))
}

#[derive(Debug, Default)]
pub(crate) struct Toks {
    str: Vec<JsString>,
    flag: Vec<u8>,
    partner: Vec<isize>,
}

pub struct State {
    visible: JsString,
    pos: Vec<isize>,
    toks: Arc<Toks>,
}

fn lift(text: &[u16], cx: &mut InlineCx, look: &mut Lookup<'_>) -> Option<State> {
    if !may_lift(text, cx) {
        return None;
    }
    let mut items = scan_placeholders(text);
    if has_inline_tags(text) {
        pin_engine_wrappers(text, &mut items, cx, look);
        pair_tags(text, &mut items, look);
    }
    let units = find_units(text, &mut items, cx.markdown);
    let marks = if cx.markdown { markdown_marks(text) } else { Vec::new() };

    let mut arena: Vec<Piece> = items;
    let item_count = arena.len();
    let mut all: Vec<usize> = (0..item_count).filter(|&k| units.is_empty() || !arena[k].unit).collect();
    let unit_base = arena.len();
    let unit_spans: Vec<(isize, isize)> = units.iter().map(|u| (u.start, u.end)).collect();
    arena.extend(units);
    all.extend(unit_base..arena.len());
    let resorted = !unit_spans.is_empty() || cx.markdown;
    if cx.markdown {
        let mark_base = arena.len();
        let mut keep_mark = vec![false; marks.len()];
        let mut u = 0usize;
        for (m, mark) in marks.iter().enumerate() {
            while u < unit_spans.len() && unit_spans[u].1 <= mark.start {
                u += 1;
            }
            keep_mark[m] = u == unit_spans.len() || mark.start < unit_spans[u].0;
        }
        for mut mark in marks {
            mark.mate = mark.mate.map(|m| m + mark_base);
            arena.push(mark);
        }
        all.extend((0..keep_mark.len()).filter(|&m| keep_mark[m]).map(|m| m + mark_base));
    }
    if resorted {
        all.sort_by_key(|&k| arena[k].start);
    }

    let n = all.len();
    let mut view: Vec<Piece> = all.iter().map(|&k| arena[k].clone()).collect();
    let mut place = vec![usize::MAX; arena.len()];
    for (i, &k) in all.iter().enumerate() {
        place[k] = i;
    }
    for p in &mut view {
        p.mate = p.mate.and_then(|m| (place[m] != usize::MAX).then_some(place[m]));
    }
    let detached: Vec<bool> = all.iter().map(|&k| arena[k].mate.is_some_and(|m| place[m] == usize::MAX)).collect();
    let all = &mut view;

    for u in 0..n {
        if all[u].kind != KIND_U {
            continue;
        }
        let mut i = u;
        let mut j = u;
        while i > 0 && is_candidate_at(all, &detached, i - 1) && all[i - 1].end == all[i].start {
            i -= 1;
        }
        while j + 1 < n && is_candidate_at(all, &detached, j + 1) && all[j + 1].start == all[j].end {
            j += 1;
        }
        let l = left_of(text, all, i);
        let r = right_of(text, all, j);
        let stand_in = l != PIN
            && r != PIN
            && !is_word_char(l)
            && !is_word_char(r)
            && !(is_quote(l) && word_past_quote_at(text, all, &detached, all[i].start - 1, i as isize - 1, -1))
            && !(is_quote(r) && word_past_quote_at(text, all, &detached, all[j].end, j as isize + 1, 1));
        all[u].stand_in = stand_in;
    }

    let mut i = 0usize;
    while i < n {
        if !is_candidate_at(all, &detached, i) {
            i += 1;
            continue;
        }
        let mut j = i;
        while j + 1 < n && is_candidate_at(all, &detached, j + 1) && all[j + 1].start == all[j].end {
            j += 1;
        }
        let closes = all[i..=j].iter().any(|p| p.close);
        let opens = all[i..=j].iter().any(|p| !p.close);
        if liftable(text, all, i, j, closes, opens) {
            for p in &mut all[i..=j] {
                p.lift = true;
            }
        }
        i = j + 1;
    }

    lift_year_ranges(text, all, &detached, cx.year_range);
    let mut parts: JsString = Vec::with_capacity(text.len());
    let mut pos = Vec::new();
    let mut toks = Toks::default();
    let mut lifted: Vec<usize> = Vec::new();
    let mut cursor: isize = 0;
    let mut length: isize = 0;
    for k in 0..n {
        let t = &all[k];
        let mate_lift = t.mate.is_some_and(|m| all[m].lift);
        if !(t.stand_in || (t.lift && mate_lift)) {
            continue;
        }
        let (start, end, close, stand_in) = (t.start, t.end, t.close, t.stand_in);
        all[k].index = lifted.len() as isize;
        lifted.push(k);
        parts.extend_from_slice(js::slice_signed(text, cursor, Some(start)));
        length += start - cursor;
        pos.push(length);
        toks.str.push(js::slice_signed(text, start, Some(end)).to_vec());
        toks.flag.push((if close { CLOSER } else { 0 }) | (if stand_in { STANDS_IN } else { 0 }));
        cursor = end;
        if stand_in {
            parts.extend_from_slice(&STAND_IN_UNITS);
            length += STAND_IN_LEN;
        }
    }
    if lifted.is_empty() {
        return None;
    }
    parts.extend_from_slice(js::slice_signed(text, cursor, None));
    toks.partner = lifted
        .iter()
        .enumerate()
        .map(|(k, &t)| match all[t].mate {
            None if !detached[t] => k as isize,
            None => -1,
            Some(m) => all[m].index,
        })
        .collect();
    Some(State { visible: parts, pos, toks: Arc::new(toks) })
}

fn lift_year_ranges(text: &[u16], all: &mut [Piece], detached: &[bool], pattern: Option<&Re>) {
    let Some(pattern) = pattern else { return };
    if !has_year_dash(text) || !(0..all.len()).any(|i| is_candidate_at(all, detached, i) && !all[i].lift) {
        return;
    }
    let mut parts: JsString = Vec::with_capacity(text.len());
    let mut positions: Vec<(usize, isize)> = Vec::new();
    let mut offset_of: Vec<Option<isize>> = vec![None; all.len()];
    let mut start = 0;
    let mut length = 0;
    for i in 0..all.len() {
        if !is_candidate_at(all, detached, i) {
            continue;
        }
        parts.extend_from_slice(js::slice_signed(text, start, Some(all[i].start)));
        length += all[i].start - start;
        positions.push((i, length));
        offset_of[i] = Some(length);
        start = all[i].end;
    }
    parts.extend_from_slice(js::slice_signed(text, start, None));
    let matches = pattern.find_all(&parts);
    let mut m = 0;
    for &(i, offset) in &positions {
        if all[i].close {
            continue;
        }
        while m < matches.len() && matches[m].end() as isize <= offset {
            m += 1;
        }
        let Some(found) = matches.get(m) else { break };
        if let Some(mate) = all[i].mate
            && let Some(mate_offset) = offset_of[mate]
            && (found.index() as isize) < mate_offset
        {
            all[i].lift = true;
            all[mate].lift = true;
        }
    }
}

fn is_candidate_at(all: &[Piece], detached: &[bool], i: usize) -> bool {
    let p = &all[i];
    p.kind == KIND_M
        || (p.tag && !p.pinned && (p.mate.is_some() || detached[i]) && (p.kind == KIND_T || p.kind == KIND_A))
}

fn word_past_quote_at(text: &[u16], all: &[Piece], detached: &[bool], at: isize, from: isize, step: isize) -> bool {
    let mut b = at + step;
    let mut k = from;
    while k >= 0 && (k as usize) < all.len() && is_candidate_at(all, detached, k as usize) {
        let p = &all[k as usize];
        if if step < 0 { p.end != b + 1 } else { p.start != b } {
            break;
        }
        b = if step < 0 { p.start - 1 } else { p.end };
        k += step;
    }
    is_word_char(cp(text, b))
}

fn materialize(state: &State) -> JsString {
    let visible = &state.visible;
    let mut parts: JsString = Vec::with_capacity(visible.len() + state.toks.str.iter().map(Vec::len).sum::<usize>());
    let mut cursor: isize = 0;
    for (k, &p) in state.pos.iter().enumerate() {
        parts.extend_from_slice(js::slice_signed(visible, cursor, Some(p)));
        parts.extend_from_slice(&state.toks.str[k]);
        cursor = if state.toks.flag[k] & STANDS_IN != 0 { p + STAND_IN_LEN } else { p };
    }
    parts.extend_from_slice(js::slice_signed(visible, cursor, None));
    parts
}

pub(crate) struct LiftProjection {
    state: Arc<State>,
    source: Option<JsString>,
    cursor: usize,
    shift: isize,
    last: Option<usize>,
}

impl LiftProjection {
    fn new(state: Arc<State>) -> Self {
        LiftProjection { state, source: None, cursor: 0, shift: 0, last: None }
    }

    pub(crate) fn visible(&self) -> &[u16] {
        &self.state.visible
    }

    pub(crate) fn source(&mut self) -> &[u16] {
        let state = &self.state;
        self.source.get_or_insert_with(|| materialize(state))
    }

    pub(crate) fn at(&mut self, offset: usize, after: bool) -> usize {
        let back = self.last.is_some_and(|last| offset < last);
        self.last = Some(offset);
        let offset = offset as isize;
        let limit = offset + isize::from(after);
        let State { pos, toks, .. } = &*self.state;
        if back {
            while self.cursor > 0 {
                let start = pos[self.cursor - 1];
                let held = toks.flag[self.cursor - 1] & STANDS_IN != 0;
                if start < limit && !(held && offset < start + STAND_IN_LEN) {
                    break;
                }
                self.cursor -= 1;
                self.shift -= len(&toks.str[self.cursor]) - if held { STAND_IN_LEN } else { 0 };
            }
        }
        while self.cursor < pos.len() && pos[self.cursor] < limit {
            let start = pos[self.cursor];
            let held = toks.flag[self.cursor] & STANDS_IN != 0;
            if held && offset < start + STAND_IN_LEN {
                return (start + self.shift) as usize;
            }
            self.shift += len(&toks.str[self.cursor]) - if held { STAND_IN_LEN } else { 0 };
            self.cursor += 1;
        }
        (offset + self.shift) as usize
    }
}

fn common(x: &[u16], a: isize, y: &[u16], b: isize) -> isize {
    let max = (len(x) - a).min(len(y) - b);
    let near = if max < NEAR { max } else { NEAR };
    let mut k = 0;
    while k < near && cp(x, a + k) == cp(y, b + k) {
        k += 1;
    }
    if k < near || k == max {
        return k;
    }
    let mut block: isize = 32;
    while k + block <= max
        && js::slice_signed(x, a + k, Some(a + k + block)) == js::slice_signed(y, b + k, Some(b + k + block))
    {
        k += block;
        block = if block < 2048 { block << 1 } else { block };
    }
    block >>= 1;
    while block >= 16 {
        if k + block <= max
            && js::slice_signed(x, a + k, Some(a + k + block)) == js::slice_signed(y, b + k, Some(b + k + block))
        {
            k += block;
        }
        block >>= 1;
    }
    while k < max && cp(x, a + k) == cp(y, b + k) {
        k += 1;
    }
    k
}

fn same_word(s: &[u16], x: isize, se: isize, t: &[u16], y: isize, te: isize) -> bool {
    if x == se || y == te {
        return x == se && y == te;
    }
    let mut k = 0;
    loop {
        let c = if x + k < se { cp(s, x + k) } else { -1 };
        let d = if y + k < te { cp(t, y + k) } else { -1 };
        let end_s = c == -1 || is_space(c);
        let end_t = d == -1 || is_space(d);
        if end_s || end_t {
            return end_s && end_t;
        }
        if c != d {
            return false;
        }
        k += 1;
    }
}

fn word_starts(s: &[u16], from: isize, end: isize) -> Vec<isize> {
    let mut starts = Vec::new();
    let mut p = from;
    if p > 0 && !is_space(cp(s, p - 1)) {
        while p < end && !is_space(cp(s, p)) {
            p += 1;
        }
    }
    while starts.len() < SPAN && p < end {
        while p < end && is_space(cp(s, p)) {
            p += 1;
        }
        if p < end {
            starts.push(p);
        }
        while p < end && !is_space(cp(s, p)) {
            p += 1;
        }
    }
    starts.push(end);
    starts
}

fn resync(before: &[u16], a: isize, le: isize, after: &[u16], b: isize, be: isize) -> Option<(isize, isize)> {
    let xs = word_starts(before, a, le);
    let ys = word_starts(after, b, be);
    for d in 0..xs.len() + ys.len() - 1 {
        let lo = (d + 1).saturating_sub(ys.len());
        let mut i = lo;
        while i <= d && i < xs.len() {
            if same_word(before, xs[i], le, after, ys[d - i], be) {
                return Some((xs[i], ys[d - i]));
            }
            i += 1;
        }
    }
    None
}

fn reach(s: &[u16], from: isize, to: isize, ls: isize, le: isize, words: usize) -> (isize, isize) {
    let mut lo = from;
    let mut hi = to;
    if lo < le && !is_space(cp(s, lo)) {
        while lo > ls && !is_space(cp(s, lo - 1)) {
            lo -= 1;
        }
    }
    for _ in 0..words {
        while lo > ls && is_space(cp(s, lo - 1)) {
            lo -= 1;
        }
        while lo > ls && !is_space(cp(s, lo - 1)) {
            lo -= 1;
        }
        while hi < le && is_space(cp(s, hi)) {
            hi += 1;
        }
        while hi < le && !is_space(cp(s, hi)) {
            hi += 1;
        }
    }
    (lo, hi)
}

fn tag_at(s: &[u16], at: isize) -> isize {
    if cp(s, at) != LT {
        return -1;
    }
    let j = if cp(s, at + 1) == SLASH { at + 2 } else { at + 1 };
    if cp(s, j) != OPEN_CODE {
        return -1;
    }
    let shut = index_of_unit(s, CLOSE_UNIT, j + 1);
    if shut != -1 && cp(s, shut + 1) == GT { shut + 2 } else { -1 }
}

fn tag_start_before(s: &[u16], at: isize, from: isize) -> isize {
    let mut p = at - 1;
    while p >= from {
        let c = cp(s, p);
        if c == LT {
            let next = cp(s, p + 1);
            return if p + 1 == at || next == OPEN_CODE || next == SLASH { p } else { at };
        }
        if c != OPEN_CODE && c != SLASH && !(48..=57).contains(&c) && !(97..=122).contains(&(c | 32)) {
            return at;
        }
        p -= 1;
    }
    at
}

fn first_at_or_after(list: &[isize], at: isize) -> usize {
    list.partition_point(|&v| v < at)
}

fn line_has_token(pos: &[isize], from: isize, to: isize) -> bool {
    let lo = first_at_or_after(pos, from);
    lo < pos.len() && pos[lo] <= to
}

fn stand_ins_beside(pos: &[isize], toks: &Toks, lo: isize, hi: isize, from: isize, to: isize) -> bool {
    let mut k = first_at_or_after(pos, lo);
    while k < pos.len() && pos[k] <= hi {
        if toks.flag[k] & STANDS_IN == 0 || (pos[k] + STAND_IN_LEN > from && pos[k] < to) {
            return false;
        }
        k += 1;
    }
    true
}

fn tag_ending_at(s: &[u16], end: isize) -> isize {
    if cp(s, end - 1) != GT || end - 2 < 0 || s.get((end - 2) as usize) != Some(&CLOSE_UNIT) {
        return -1;
    }
    let at = last_index_of_unit(s, 0x3C, end - 3);
    if at != -1 && tag_at(s, at) == end { at } else { -1 }
}

fn past_tags(s: &[u16], at: isize, step: isize) -> isize {
    let mut p = at;
    loop {
        let tag = if step < 0 { tag_ending_at(s, p + 1) } else { tag_at(s, p) };
        if tag == -1 {
            return p;
        }
        p = if step < 0 { tag - 1 } else { tag };
    }
}

fn mark_at(s: &[u16], at: isize, from: isize, to: isize) -> i32 {
    let c = if at < from || at >= to { 32 } else { cp(s, at) };
    if is_space(c) {
        return 0;
    }
    if is_quote(c) { 1 } else { c }
}

#[allow(clippy::too_many_arguments)]
fn same_side(
    before: &[u16],
    p: isize,
    pf: isize,
    pt: isize,
    after: &[u16],
    q: isize,
    qf: isize,
    qt: isize,
    step: isize,
) -> bool {
    let mut i = p;
    let mut j = q;
    loop {
        i = past_tags(before, i, step);
        j = past_tags(after, j, step);
        let x = mark_at(before, i, pf, pt);
        if x != mark_at(after, j, qf, qt) {
            return false;
        }
        if x == 0 {
            return true;
        }
        i += step;
        j += step;
    }
}

fn holds_tag(s: &[u16], from: isize, to: isize, tags: &HashSet<JsString>) -> bool {
    let part = js::slice_signed(s, from, Some(to));
    let mut i = index_of_unit(part, 0x3C, 0);
    while i != -1 {
        let end = tag_at(s, from + i);
        if end != -1 && tags.contains(js::slice_signed(s, from + i, Some(end))) {
            return true;
        }
        i = index_of_unit(part, 0x3C, i + 1);
    }
    false
}

fn held_in(s: &[u16], at: &[isize], flag: &[u8], cx: &InlineCx) -> Vec<i32> {
    let mut held = vec![0; at.len()];
    let mut stack: Vec<i32> = Vec::new();
    let mut keeps = 0;
    let mut p = index_of_unit(s, 0x3C, 0);
    for k in 0..at.len() {
        if flag[k] & STANDS_IN == 0 {
            continue;
        }
        while p != -1 && p < at[k] {
            let end = tag_at(s, p);
            if end == -1 {
                p = index_of_unit(s, 0x3C, p + 1);
                continue;
            }
            let tag = js::slice_signed(s, p, Some(end));
            if cp(s, p + 1) == SLASH {
                if !stack.is_empty() && cx.engine_tags.contains(tag) {
                    keeps -= stack.pop().unwrap_or(0);
                }
            } else if let Some(&nowrap) = cx.engine_openers.get(tag) {
                let one = i32::from(nowrap);
                stack.push(one);
                keeps += one;
            }
            p = index_of_unit(s, 0x3C, end);
        }
        held[k] = keeps;
    }
    held
}

enum Touched {
    None,
    Rewritten,
    Spans(Vec<isize>),
}

fn touched_stand_ins(state: &State, after: &[u16], out: &[isize], cx: &InlineCx, tagged: bool) -> Touched {
    let before = &state.visible;
    let pos = &state.pos;
    let flag = &state.toks.flag;
    let mut ks = Vec::new();
    for k in 0..pos.len() {
        if flag[k] & STANDS_IN != 0 {
            if !starts_with_at(after, &STAND_IN_UNITS, out[k]) {
                return Touched::Rewritten;
            }
            ks.push(k);
        }
    }
    if ks.is_empty() {
        return Touched::None;
    }
    let held = tagged.then(|| (held_in(before, pos, flag, cx), held_in(after, out, flag, cx)));
    let l = STAND_IN_LEN;
    let mut spans: Option<Vec<isize>> = None;
    for i in 0..ks.len() {
        let k = ks[i];
        let p = pos[k];
        let q = out[k];
        let pf = if i > 0 { pos[ks[i - 1]] + l } else { 0 };
        let qf = if i > 0 { out[ks[i - 1]] + l } else { 0 };
        let pt = if i + 1 < ks.len() { pos[ks[i + 1]] } else { len(before) };
        let qt = if i + 1 < ks.len() { out[ks[i + 1]] } else { len(after) };
        if held.as_ref().is_some_and(|(h0, h1)| h1[k] > h0[k])
            || !same_side(before, p - 1, pf, p, after, q - 1, qf, q, -1)
            || !same_side(before, p + l, p + l, pt, after, q + l, q + l, qt, 1)
        {
            spans.get_or_insert_with(Vec::new).extend([p, p]);
        }
    }
    match spans {
        None => Touched::None,
        Some(s) => Touched::Spans(s),
    }
}

fn same_line_end(before: &[u16], le: isize, after: &[u16], be: isize) -> bool {
    if le == len(before) || be == len(after) {
        return le == len(before) && be == len(after);
    }
    let k = LOOK.min(len(before) - le).min(len(after) - be);
    (0..k).all(|i| cp(before, le + i) == cp(after, be + i))
}

fn line_end(s: &[u16], at: isize) -> isize {
    let end = index_of_unit(s, 0x0A, at + 1);
    if end == -1 { len(s) } else { end }
}

fn changed_lines(before: &[u16], le: isize, after: &[u16], be: isize, pos: &[isize]) -> Option<(isize, isize)> {
    if same_line_end(before, le, after, be) {
        return Some((le, be));
    }
    let mut xs = vec![le];
    let mut ys = vec![be];
    let mut x = le;
    while xs.len() <= LINES && x < len(before) {
        let end = line_end(before, x);
        if line_has_token(pos, x + 1, end) {
            break;
        }
        xs.push(end);
        x = end;
    }
    let mut y = be;
    while ys.len() <= LINES && y < len(after) {
        y = line_end(after, y);
        ys.push(y);
    }
    for d in 1..(xs.len() + ys.len() - 1) as isize {
        for s in 0..=d {
            let i = (d >> 1) + if s % 2 == 1 { (s + 1) >> 1 } else { -(s >> 1) };
            let j = d - i;
            if i < 0 || i >= xs.len() as isize || j < 0 || j >= ys.len() as isize {
                continue;
            }
            let (xi, yj) = (xs[i as usize], ys[j as usize]);
            let tx = len(before) - xi;
            let ty = len(after) - yj;
            if (tx == ty || (tx >= LOOK && ty >= LOOK)) && same_line_end(before, xi, after, yj) {
                return Some((xi, yj));
            }
        }
    }
    None
}

fn line_of(s: &[u16], line: &mut (isize, isize), at: isize) -> (isize, isize) {
    if at < line.0 || at > line.1 {
        let end = index_of_unit(s, 0x0A, at);
        line.0 = last_index_of_unit(s, 0x0A, at - 1) + 1;
        line.1 = if end == -1 { len(s) } else { end };
    }
    *line
}

fn minted_of(s: &[u16], found: &[isize], known: &[JsString], between: &[&[u16]]) -> Option<Vec<isize>> {
    let mut early = Vec::new();
    let mut late = Vec::new();
    let mut early_at = Vec::new();
    let mut late_at = Vec::new();
    let mut k = 0usize;
    for i in (0..found.len()).step_by(2) {
        if k < known.len() && js::slice_signed(s, found[i], Some(found[i + 1])) == known[k].as_slice() {
            early_at.push(i);
            k += 1;
        } else {
            early.extend([found[i], found[i + 1]]);
        }
    }
    if k < known.len() {
        return None;
    }
    let mut k = known.len() as isize - 1;
    let mut i = found.len() as isize - 2;
    while i >= 0 {
        let (a, b) = (found[i as usize], found[i as usize + 1]);
        if k >= 0 && js::slice_signed(s, a, Some(b)) == known[k as usize].as_slice() {
            late_at.insert(0, i as usize);
            k -= 1;
        } else {
            late.splice(0..0, [a, b]);
        }
        i -= 2;
    }
    if early == late {
        return Some(early);
    }
    let holds = |at: &[usize]| {
        at.iter()
            .enumerate()
            .all(|(j, &i)| j == 0 || js::slice_signed(s, found[at[j - 1] + 1], Some(found[i])) == between[j - 1])
    };
    let early_holds = holds(&early_at);
    if early_holds == holds(&late_at) {
        return None;
    }
    Some(if early_holds { early } else { late })
}

fn mints_new(minted: &HashSet<JsString>, before: &[u16], after: &[u16]) -> bool {
    minted.iter().any(|tag| strings::count(after, tag) > strings::count(before, tag))
}

struct Realigner<'s> {
    before: &'s [u16],
    after: &'s [u16],
    pos: &'s [isize],
    out: Vec<isize>,
    t: usize,
    a: isize,
    b: isize,
    inserted: Option<Vec<isize>>,
    open: isize,
    minted: Option<&'s HashSet<JsString>>,
    stands_in: bool,
    tagged: bool,
}

impl Realigner<'_> {
    fn record(&mut self, p: isize, e: isize) {
        self.inserted.get_or_insert_with(Vec::new).extend([p, e]);
        self.open += if cp(self.after, p + 1) == SLASH { -1 } else { 1 };
    }

    fn changed(&mut self, af: isize, at: isize, bf: isize, bt: isize, tags: &HashSet<JsString>) {
        self.tagged = self.tagged
            || (self.stands_in
                && (holds_tag(self.before, tag_start_before(self.before, af, 0), at, tags)
                    || holds_tag(self.after, tag_start_before(self.after, bf, 0), bt, tags)));
    }

    fn shift_to(&mut self, limit: isize, delta: isize) {
        while self.t < self.pos.len() && self.pos[self.t] < limit {
            self.out[self.t] = self.pos[self.t] + delta;
            self.t += 1;
        }
    }

    fn tags_in(&self, s: &[u16], from: isize, to: isize, found: &mut Vec<isize>) {
        let Some(minted) = self.minted else { return };
        let mut p = index_of_unit(s, 0x3C, from);
        while p != -1 && p < to {
            let end = tag_at(s, p);
            if end != -1 && minted.contains(js::slice_signed(s, p, Some(end))) {
                found.extend([p, end]);
            }
            p = index_of_unit(s, 0x3C, p + 1);
        }
    }

    fn mints_in(&mut self, was: isize, end: isize, from: isize, to: isize) -> bool {
        if self.minted.is_none() {
            return true;
        }
        let mut known_at = Vec::new();
        let mut found = Vec::new();
        self.tags_in(self.before, was, end, &mut known_at);
        self.tags_in(self.after, from, to, &mut found);
        let known: Vec<JsString> =
            known_at.chunks(2).map(|c| js::slice_signed(self.before, c[0], Some(c[1])).to_vec()).collect();
        let between: Vec<&[u16]> =
            known_at.windows(4).step_by(2).map(|w| js::slice_signed(self.before, w[1], Some(w[2]))).collect();
        let fresh = if known.is_empty() { Some(found) } else { minted_of(self.after, &found, &known, &between) };
        let Some(fresh) = fresh else { return false };
        for c in fresh.chunks(2) {
            self.record(c[0], c[1]);
        }
        true
    }

    fn pass_line(&mut self, ls: isize, le: isize, be: isize, tags: &HashSet<JsString>) -> i32 {
        let ends = if line_has_token(self.pos, ls, le) {
            None
        } else {
            changed_lines(self.before, le, self.after, be, self.pos)
        };
        let Some((e0, e1)) = ends else { return 0 };
        if self.open > 0 && !self.mints_in(self.a, e0, self.b, e1) {
            return -1;
        }
        self.shift_to(ls, self.b - self.a);
        self.changed(self.a, e0, self.b, e1, tags);
        self.a = e0;
        self.b = e1;
        1
    }
}

fn realign(state: &State, after: &[u16], cx: &mut InlineCx, safe_tags: &mut SafeTags, words: usize) -> Option<State> {
    cx.near = None;
    let before = &state.visible[..];
    let pos = &state.pos[..];
    let toks = &state.toks;
    let n = pos.len();
    let na = len(before);
    let nb = len(after);
    let minted = (cx.mints_active && mints_new(&cx.mint_log, before, after)).then_some(&cx.mint_log);
    let mut r = Realigner {
        before,
        after,
        pos,
        out: vec![0; n],
        t: 0,
        a: 0,
        b: 0,
        inserted: None,
        open: 0,
        minted,
        stands_in: toks.flag.iter().any(|f| f & STANDS_IN != 0),
        tagged: false,
    };
    let tags = &cx.engine_tags;
    let mut near: Option<Vec<isize>> = None;
    let mut line_a = (0isize, -1isize);
    let mut line_b = (0isize, -1isize);

    loop {
        let from_a = r.a;
        let k = common(before, r.a, after, r.b);
        r.a += k;
        r.b += k;
        if r.a == na && r.b == nb {
            let delta = r.b - r.a;
            r.shift_to(isize::MAX, delta);
            break;
        }
        if cp(before, r.a) != NL && cp(after, r.b) != NL {
            let (ls, le) = line_of(before, &mut line_a, r.a);
            let be = line_of(after, &mut line_b, r.b).1;
            let passed = r.pass_line(ls, le, be, tags);
            if passed < 0 {
                return None;
            }
            if passed > 0 {
                continue;
            }
        }
        let back = tag_start_before(before, r.a, from_a);
        r.b -= r.a - back;
        r.a = back;
        let a = r.a;
        let b = r.b;
        let mut ga0 = a;
        while ga0 > 0 && is_space(cp(before, ga0 - 1)) {
            ga0 -= 1;
        }
        let gb0 = b - (a - ga0);
        let mut ga1 = a;
        let mut gb1 = b;
        let mut marks: Option<Vec<isize>> = None;
        loop {
            while ga1 < na && is_space(cp(before, ga1)) {
                ga1 += 1;
            }
            while gb1 < nb && is_space(cp(after, gb1)) {
                gb1 += 1;
            }
            let ea = tag_at(before, ga1);
            let eb = tag_at(after, gb1);
            if ea != -1 && eb != -1 && js::slice_signed(before, ga1, Some(ea)) == js::slice_signed(after, gb1, Some(eb))
            {
                break;
            }
            if eb != -1 && r.minted.is_some_and(|m| m.contains(js::slice_signed(after, gb1, Some(eb)))) {
                marks.get_or_insert_with(Vec::new).extend([gb1, eb]);
                gb1 = eb;
                continue;
            }
            if ea != -1 && tags.contains(js::slice_signed(before, ga1, Some(ea))) {
                ga1 = ea;
                continue;
            }
            break;
        }
        let both_end = ga1 == na && gb1 == nb;
        let resumes = ga1 < na && gb1 < nb && cp(before, ga1) == cp(after, gb1);
        if (ga1 == a && gb1 == b) || !(both_end || resumes) {
            let (ls, le) = line_of(before, &mut line_a, a);
            let be = line_of(after, &mut line_b, b).1;
            let passed = r.pass_line(ls, le, be, tags);
            if passed < 0 {
                return None;
            }
            if passed > 0 {
                continue;
            }
            if (le == na) != (be == nb) {
                return None;
            }
            let met = resync(before, a, le, after, b, be);
            let met = met.filter(|&(x, y)| !(x == a && y == b))?;
            let (lo, hi) = reach(before, a, met.0, ls, le, words);
            if line_has_token(pos, lo, hi) && !stand_ins_beside(pos, toks, lo, hi, a, met.0) {
                near.get_or_insert_with(Vec::new).extend([lo, hi]);
            }
            r.shift_to(a, b - a);
            if !r.mints_in(a, met.0, b, met.1) {
                return None;
            }
            r.changed(a, met.0, b, met.1, tags);
            r.a = met.0;
            r.b = met.1;
            continue;
        }
        r.shift_to(ga0, gb0 - ga0);
        let first = r.t;
        while r.t < n && pos[r.t] <= ga1 {
            r.t += 1;
        }
        if first < r.t {
            if ga0 == ga1 {
                let mut k2 = first;
                while k2 < r.t && toks.flag[k2] & CLOSER != 0 {
                    r.out[k2] = gb0;
                    k2 += 1;
                }
                for k2 in k2..r.t {
                    r.out[k2] = gb1;
                }
            } else {
                for (k2, &position) in pos.iter().enumerate().take(r.t).skip(first) {
                    if position == ga0 {
                        r.out[k2] = gb0;
                    } else if position == ga1 {
                        r.out[k2] = gb1;
                    } else {
                        return None;
                    }
                }
            }
        }
        if let Some(marks) = marks {
            for c in marks.chunks(2) {
                r.record(c[0], c[1]);
            }
        }
        r.changed(ga0, ga1, gb0, gb1, tags);
        r.a = ga1;
        r.b = gb1;
    }

    let Realigner { out, inserted, tagged, .. } = r;
    if near.is_some() {
        cx.near = near;
        return None;
    }
    match touched_stand_ins(state, after, &out, cx, tagged) {
        Touched::None => {}
        Touched::Rewritten => {
            cx.near = None;
            return None;
        }
        Touched::Spans(spans) => {
            cx.near = Some(spans);
            return None;
        }
    }
    if let Some(inserted) = inserted {
        return settle_wrappers(after, out, toks, &inserted, cx, safe_tags);
    }
    Some(State { visible: after.to_vec(), pos: out, toks: toks.clone() })
}

fn keeps_together(s: &[u16], tags: &mut SafeTags) -> bool {
    let shut = index_of_unit(s, CLOSE_UNIT, 2);
    let kind = s.get(2).copied().unwrap_or(0);
    let Some(id) = js_number(js::slice_signed(s, 3, Some(shut))) else { return false };
    let Some(raw) = tags.raw_of(kind, id).map(<[u16]>::to_vec) else { return false };
    let name = tags.name_of(kind, id);
    name.as_deref() == Some(&js::u("nobr")[..])
        || (name.as_deref() == Some(&js::u("span")[..]) && js::includes(&raw, &js::u("nowrap")))
}

fn name_of_tag(s: &[u16], tags: &mut SafeTags) -> Option<JsString> {
    let j = if cp(s, 1) == SLASH { 3 } else { 2 };
    let kind = *s.get(j as usize)?;
    let shut = index_of_unit(s, CLOSE_UNIT, j);
    let digits = if shut == -1 { js::slice_signed(s, j + 1, None) } else { js::slice_signed(s, j + 1, Some(shut)) };
    tags.name_of(kind, js_number(digits)?)
}

fn own_close(after: &[u16], oe: isize, c: isize, ce: isize, tags: &mut SafeTags) -> isize {
    let tag = js::slice_signed(after, c, Some(ce));
    if !starts_with_at(after, tag, c - len(tag)) {
        return c;
    }
    let name = name_of_tag(tag, tags);
    let mut depth = 0;
    let mut p = index_of_unit(after, 0x3C, oe);
    while p != -1 && p < c {
        let end = tag_at(after, p);
        if end != -1 {
            let found = js::slice_signed(after, p, Some(end));
            if found == tag {
                if depth == 0 {
                    return p;
                }
                depth -= 1;
            } else if cp(found, 1) != SLASH && name_of_tag(found, tags) == name {
                depth += 1;
            }
        }
        p = index_of_unit(after, 0x3C, p + 1);
    }
    c
}

fn loose_tags(text: &[u16], tags: &HashSet<JsString>) -> Vec<isize> {
    let mut found = Vec::new();
    let mut p = index_of_unit(text, 0x3C, 0);
    while p != -1 {
        let end = tag_at(text, p);
        if end != -1 && tags.contains(js::slice_signed(text, p, Some(end))) {
            found.extend([p, end]);
        }
        p = index_of_unit(text, 0x3C, p + 1);
    }
    found
}

fn settle_wrappers(
    after: &[u16],
    out: Vec<isize>,
    toks: &Arc<Toks>,
    inserted: &[isize],
    cx: &InlineCx,
    tags: &mut SafeTags,
) -> Option<State> {
    let mut pairs: Vec<[isize; 4]> = Vec::new();
    let mut stack: Vec<(isize, isize)> = Vec::new();
    for c in inserted.chunks(2) {
        let (at, end) = (c[0], c[1]);
        if cp(after, at + 1) == SLASH {
            let open = stack.pop()?;
            let close = own_close(after, open.1, at, end, tags);
            pairs.push([open.0, open.1, close, close + end - at]);
        } else {
            stack.push((at, end));
        }
    }
    if !stack.is_empty() {
        return None;
    }
    let partner = &toks.partner;
    let mut changes: Vec<(isize, isize, isize, isize, bool)> = Vec::new();
    let loose = cx.loose.as_ref().map(|l| loose_tags(after, l));
    for &[o, oe, c, ce] in &pairs {
        let mut whole = true;
        let mut first: isize = -1;
        let mut last: isize = -1;
        let mut k = first_at_or_after(&out, oe);
        while k < out.len() && out[k] <= c && whole {
            let p = partner[k];
            whole = p != -1 && out[p as usize] >= oe && out[p as usize] <= c;
            first = if first == -1 { k as isize } else { first };
            last = k as isize;
            k += 1;
        }
        if whole && let Some(loose) = &loose {
            let found = first_at_or_after(loose, oe);
            let i = found + (found & 1);
            whole = i >= loose.len() || loose[i + 1] > c;
        }
        if whole {
            if first != last && out[first as usize] == oe && out[last as usize] == c && partner[first as usize] == last
            {
                let nbsp = !keeps_together(&toks.str[first as usize], tags);
                changes.push((o, oe, c, ce, nbsp));
            }
            continue;
        }
        if cx.engine_openers.get(js::slice_signed(after, o, Some(oe))) != Some(&true) {
            return None;
        }
        changes.push((o, oe, c, ce, true));
    }
    if changes.is_empty() {
        return Some(State { visible: after.to_vec(), pos: out, toks: toks.clone() });
    }
    changes.sort_by_key(|x| x.0);
    for i in 1..changes.len() {
        if changes[i - 1].3 > changes[i].0 {
            return None;
        }
    }
    let mut parts: JsString = Vec::with_capacity(after.len());
    let mut cursor: isize = 0;
    for &(o, oe, c, ce, nbsp) in &changes {
        parts.extend_from_slice(js::slice_signed(after, cursor, Some(o)));
        let body = js::slice_signed(after, oe, Some(c));
        if nbsp {
            parts.extend(body.iter().map(|&u| if u == 0x20 { unit(NBSP) } else { u }));
        } else {
            parts.extend_from_slice(body);
        }
        cursor = ce;
    }
    parts.extend_from_slice(js::slice_signed(after, cursor, None));
    let mut pos = vec![0isize; out.len()];
    let mut shift: isize = 0;
    let mut j = 0usize;
    for k in 0..out.len() {
        while j < changes.len() && out[k] >= changes[j].3 {
            let (o, oe, c, ce, _) = changes[j];
            shift += oe - o + (ce - c);
            j += 1;
        }
        if j < changes.len() && out[k] >= changes[j].1 {
            pos[k] = out[k] - shift - (changes[j].1 - changes[j].0);
        } else if j < changes.len() && out[k] > changes[j].0 {
            return None;
        } else {
            pos[k] = out[k] - shift;
        }
    }
    Some(State { visible: parts, pos, toks: toks.clone() })
}

fn put_back(state: &State, spans: &[isize], cx: &mut InlineCx) -> State {
    let visible = &state.visible;
    let pos = &state.pos;
    let Toks { str, flag, partner } = &*state.toks;
    let n = pos.len();
    let mut back = vec![false; n];
    let mut s = 0usize;
    for k in 0..n {
        while s < spans.len() && spans[s + 1] < pos[k] {
            s += 2;
        }
        back[k] = s < spans.len() && pos[k] >= spans[s];
    }
    for k in 0..n {
        let p = partner[k];
        if back[k] && p != -1 && p != k as isize && !back[p as usize] {
            cx.loose.get_or_insert_with(HashSet::new).insert(str[k].clone());
        }
    }
    let mut parts: JsString = Vec::with_capacity(visible.len());
    let mut index = vec![-1isize; n];
    let mut kept = Toks::default();
    let mut at = Vec::new();
    let mut cursor: isize = 0;
    let mut length: isize = 0;
    for k in 0..n {
        parts.extend_from_slice(js::slice_signed(visible, cursor, Some(pos[k])));
        length += pos[k] - cursor;
        cursor = pos[k];
        if back[k] {
            parts.extend_from_slice(&str[k]);
            length += len(&str[k]);
            if flag[k] & STANDS_IN != 0 {
                cursor += STAND_IN_LEN;
            }
        } else {
            index[k] = at.len() as isize;
            at.push(length);
            kept.str.push(str[k].clone());
            kept.flag.push(flag[k]);
        }
    }
    parts.extend_from_slice(js::slice_signed(visible, cursor, None));
    for k in 0..n {
        if !back[k] {
            let p = partner[k];
            kept.partner.push(if p == -1 { -1 } else { index[p as usize] });
        }
    }
    State { visible: parts, pos: at, toks: Arc::new(kept) }
}

pub trait LiftRule {
    fn html_only(&self) -> bool;
    fn requires(&self) -> &Requires;
    fn apply(&self, input: &[u16], ctx: &mut Ctx<'_>) -> Result<Option<JsString>>;
    fn lift_flags(&self) -> LiftFlags {
        LiftFlags::default()
    }
}

#[derive(Clone, Copy, Default)]
pub struct YearGapRules {
    pub range: bool,
    pub collapse: bool,
    pub bind: bool,
}

#[derive(Clone, Copy)]
enum Align {
    Realign,
    Edits,
    SpacedYears(bool),
}

static YEAR_ANCHOR_RE: LazyLock<Re> = LazyLock::new(|| Re::new(r"[\p{L}\p{N}]+|([-–—−]+)", "gu"));

fn has_year_dash(s: &[u16]) -> bool {
    s.iter().any(|&c| matches!(c, 0x2D | 0x2014 | 0x2212 | 0x2013))
}

fn align(mode: Align, state: &State, after: &[u16], ctx: &mut Ctx<'_>) -> Result<Option<State>> {
    match mode {
        Align::Realign => Ok(realign_in(state, after, ctx, REACH)),
        Align::Edits => {
            let edits = crate::shared::diff::replacement_edits(&state.visible, after, ctx.check_budget)?;
            Ok(project_edits(state, after, inline(ctx), &edits))
        }
        Align::SpacedYears(gaps) => align_spaced_years(state, after, ctx, gaps),
    }
}

fn project_edits(state: &State, after: &[u16], cx: &mut InlineCx, edits: &[Edit]) -> Option<State> {
    let State { pos, toks, visible } = state;
    let n = pos.len();
    let mut mapped = Vec::with_capacity(n);
    let mut k = 0;
    let mut shift: isize = 0;
    for edit in edits {
        let (start, end, text) = (edit.start as isize, edit.end as isize, &edit.text);
        while k < n && pos[k] < start {
            mapped.push(pos[k] + shift);
            k += 1;
        }
        let next = start + shift;
        if start == end {
            while k < n && pos[k] == start && toks.flag[k] & CLOSER != 0 {
                mapped.push(next);
                k += 1;
            }
        }
        let mut content = start;
        if !text.is_empty() && !is_space(cp(text, 0)) {
            while content < end && is_space(cp(visible, content)) {
                content += 1;
            }
        }
        while k < n && pos[k] < end {
            mapped.push(next + (pos[k] - content).clamp(0, len(text)).max(0));
            k += 1;
        }
        shift += len(text) - (end - start);
    }
    while k < n {
        mapped.push(pos[k] + shift);
        k += 1;
    }
    match touched_stand_ins(state, after, &mapped, cx, false) {
        Touched::None => {
            cx.near = None;
            Some(State { visible: after.to_vec(), pos: mapped, toks: toks.clone() })
        }
        Touched::Rewritten => {
            cx.near = None;
            None
        }
        Touched::Spans(spans) => {
            cx.near = Some(spans);
            None
        }
    }
}

fn spaced_year_ranges<'t>(visible: &'t [u16], cx: &InlineCx, project_gaps: bool) -> Vec<Caps<'t>> {
    let Some(pattern) = cx.year_range else { return Vec::new() };
    let gap_rules = cx.year_gap_rules;
    pattern
        .find_all(visible)
        .into_iter()
        .filter(|m| {
            let separator = m.s(3);
            let suffix_gap = m.named("suffixGap");
            let lead = m.s(1);
            let repeated_gap = suffix_gap.is_some_and(|g| g.len() > 1) || is_space(cp(lead, len(lead) - 2));
            if separator.len() > 1 || (project_gaps && repeated_gap) {
                return true;
            }
            let Some(gap) = suffix_gap.filter(|g| !g.is_empty() && *g != [0x20] && *g != [unit(NBSP)]) else {
                return false;
            };
            let collapses = gap_rules.collapse && gap.iter().all(|&c| matches!(c, 0x20 | 0x09));
            let binds = gap_rules.bind
                && (separator != [0x2D] || gap_rules.range)
                && gap.iter().all(|&c| matches!(c, 0x20 | 0x09 | 0xA0));
            !collapses && !binds
        })
        .collect()
}

fn ordered_spacing_edits(
    before: &[u16],
    after: &[u16],
    check: &dyn Fn(&str) -> Result<()>,
) -> Result<Option<Vec<Edit>>> {
    let rights = YEAR_ANCHOR_RE.find_all(after);
    let mut next = rights.iter();
    let mut edits = Vec::new();
    let mut from = 0;
    let mut to = 0;
    let gap = |from: usize, to: usize, a: usize, b: usize, edits: &mut Vec<Edit>| -> Result<()> {
        let previous = &before[from.min(a)..a];
        let following = &after[to.min(b)..b];
        if previous != following {
            for edit in crate::shared::diff::replacement_edits(previous, following, check)? {
                edits.push(Edit { start: from + edit.start, end: from + edit.end, text: edit.text });
            }
        }
        Ok(())
    };
    for (count, left) in YEAR_ANCHOR_RE.find_all(before).iter().enumerate() {
        if count & 0xff == 0 {
            check("changes")?;
        }
        let Some(right) = next.next() else { return Ok(None) };
        gap(from, to, left.index(), right.index(), &mut edits)?;
        from = left.end();
        to = right.end();
        if left.whole() != right.whole() {
            if !left.has(1) || !right.has(1) {
                return Ok(None);
            }
            edits.push(Edit { start: left.index(), end: from, text: right.whole().to_vec() });
        }
    }
    if next.next().is_some() {
        return Ok(None);
    }
    gap(from, to, before.len(), after.len(), &mut edits)?;
    Ok(Some(edits))
}

fn align_spaced_years(state: &State, after: &[u16], ctx: &mut Ctx<'_>, project_gaps: bool) -> Result<Option<State>> {
    let ordinary = realign_in(state, after, ctx, REACH);
    if ordinary.is_some() || !has_year_dash(&state.visible) {
        return Ok(ordinary);
    }
    let visible = &state.visible;
    let ranges = spaced_year_ranges(visible, inline(ctx), project_gaps);
    if ranges.is_empty() {
        return Ok(None);
    }
    let original_near = inline(ctx).near.clone();
    let Some(changes) = ordered_spacing_edits(visible, after, ctx.check_budget)? else { return Ok(None) };
    let mut edits = Vec::new();
    let mut r = 0;
    for edit in &changes {
        while r < ranges.len() && ranges[r].end() <= edit.start {
            r += 1;
        }
        let Some(range) = ranges.get(r) else { break };
        if range.index() <= edit.start && edit.end <= range.end() {
            edits.push(edit.clone());
        }
    }
    if edits.is_empty() {
        return Ok(None);
    }
    if edits.len() == changes.len() {
        return Ok(project_edits(state, after, inline(ctx), &edits));
    }
    let mut parts: JsString = Vec::with_capacity(visible.len());
    let mut cursor = 0;
    for edit in &edits {
        parts.extend_from_slice(&visible[cursor..edit.start]);
        parts.extend_from_slice(&edit.text);
        cursor = edit.end;
    }
    parts.extend_from_slice(&visible[cursor..]);
    let partial = project_edits(state, &parts, inline(ctx), &edits);
    let next = partial.and_then(|partial| realign_in(&partial, after, ctx, REACH));
    if next.is_none() {
        inline(ctx).near = original_near;
    }
    Ok(next)
}

fn inline<'a>(ctx: &'a mut Ctx<'_>) -> &'a mut InlineCx {
    ctx.inline.as_mut().expect("a lifting is open")
}

fn run(state: &Arc<State>, rule: &dyn LiftRule, ctx: &mut Ctx<'_>) -> Result<Option<JsString>> {
    ctx.bound.nowrap_frame = Some(NowrapFrame::new(LiftProjection::new(Arc::clone(state)), !state.pos.is_empty()));
    ctx.bound.nowrap_cache = None;
    ctx.bound.nowrap_open = None;
    let cx = inline(ctx);
    cx.mint_log.clear();
    cx.mints_active = true;
    if ctx.trace.is_some() {
        ctx.trace_projection = Some(trace_projection(state));
    }
    let out = rule.apply(&state.visible, ctx);
    if out.is_err() {
        inline(ctx).mints_active = false;
    }
    out
}

fn with_lookup<R>(ctx: &mut Ctx<'_>, f: impl FnOnce(&mut InlineCx, &mut Lookup<'_>) -> R) -> R {
    let cx = ctx.inline.as_mut().expect("a lifting is open");
    let mut look = Lookup { tags: &mut *ctx.safe_tags, sequences: &*ctx.safe_sequences };
    f(cx, &mut look)
}

fn relift(text: JsString, ctx: &mut Ctx<'_>) -> State {
    match with_lookup(ctx, |cx, look| lift(&text, cx, look)) {
        Some(state) => state,
        None => bare(text),
    }
}

fn bare(text: JsString) -> State {
    State { visible: text, pos: Vec::new(), toks: Arc::new(Toks::default()) }
}

fn realign_in(state: &State, after: &[u16], ctx: &mut Ctx<'_>, words: usize) -> Option<State> {
    let cx = ctx.inline.as_mut().expect("a lifting is open");
    realign(state, after, cx, &mut *ctx.safe_tags, words)
}

fn apply_lifted(state: &Arc<State>, rule: &dyn LiftRule, ctx: &mut Ctx<'_>) -> Result<Option<Arc<State>>> {
    inline(ctx).loose = None;
    let Some(mut after) = run(state, rule, ctx)? else {
        inline(ctx).mints_active = false;
        accept_trace(ctx, state)?;
        return Ok(None);
    };
    let unchanged = after == state.visible;
    if unchanged || state.pos.is_empty() {
        inline(ctx).mints_active = false;
        if ctx.trace.is_some() {
            let next = State { visible: after.clone(), pos: state.pos.clone(), toks: state.toks.clone() };
            accept_trace(ctx, &next)?;
        }
        return Ok(if unchanged {
            None
        } else {
            Some(Arc::new(State { visible: after, pos: state.pos.clone(), toks: state.toks.clone() }))
        });
    }
    let flags = rule.lift_flags();
    let mode = if flags.project_edits {
        Align::Edits
    } else if flags.project_spaced_years != SpacedYears::Off && inline(ctx).year_range.is_some() {
        Align::SpacedYears(flags.project_spaced_years == SpacedYears::Gaps)
    } else {
        Align::Realign
    };
    let mut moved = false;
    let mut current: Option<Arc<State>> = None;
    let mut next = align(mode, state, &after, ctx)?;
    let mut round = 0;
    while next.is_none() && round < ROUNDS {
        let Some(near) = inline(ctx).near.take() else { break };
        let now = current.as_ref().unwrap_or(state);
        current = Some(Arc::new(put_back(now, &near, inline(ctx))));
        let current = current.as_ref().expect("markup returned above");
        moved = true;
        after = run(current, rule, ctx)?.unwrap_or_else(|| current.visible.clone());
        next = if current.pos.is_empty() || after == current.visible {
            Some(State { visible: after.clone(), pos: current.pos.clone(), toks: current.toks.clone() })
        } else {
            align(mode, current, &after, ctx)?
        };
        round += 1;
    }
    let cx = inline(ctx);
    cx.mints_active = false;
    cx.loose = None;
    if let Some(next) = next {
        accept_trace(ctx, &next)?;
        if !moved {
            return Ok(Some(Arc::new(next)));
        }
        let lifted = relift(materialize(&next), ctx);
        return Ok(Some(settle(Arc::new(lifted), rule, ctx)?));
    }
    let input = materialize(state);
    let output = rule.apply(&input, ctx)?.unwrap_or(input);
    let lifted = relift(output, ctx);
    accept_trace(ctx, &lifted)?;
    Ok(Some(settle(Arc::new(lifted), rule, ctx)?))
}

fn past_space_and_mints(s: &[u16], at: isize, minted: &HashSet<JsString>) -> isize {
    let mut p = at;
    loop {
        while p < len(s) && is_space(cp(s, p)) {
            p += 1;
        }
        let end = tag_at(s, p);
        if end == -1 || !minted.contains(js::slice_signed(s, p, Some(end))) {
            return p;
        }
        p = end;
    }
}

fn same_words(x: &[u16], y: &[u16], minted: &HashSet<JsString>) -> bool {
    let mut i = 0;
    let mut j = 0;
    loop {
        i = past_space_and_mints(x, i, minted);
        j = past_space_and_mints(y, j, minted);
        if i == len(x) || j == len(y) {
            return i == len(x) && j == len(y);
        }
        if cp(x, i) != cp(y, j) {
            return false;
        }
        i += 1;
        j += 1;
    }
}

fn settle(state: Arc<State>, rule: &dyn LiftRule, ctx: &mut Ctx<'_>) -> Result<Arc<State>> {
    if state.pos.is_empty() {
        return Ok(state);
    }
    let mut current: Option<Arc<State>> = None;
    let mut next: Option<State> = None;
    for _ in 0..=ROUNDS {
        let now = current.as_ref().unwrap_or(&state);
        let after = run(now, rule, ctx)?;
        inline(ctx).near = None;
        let Some(after) = after else { break };
        if after == now.visible {
            break;
        }
        let same = {
            let cx = inline(ctx);
            same_words(&now.visible, &after, &cx.mint_log)
        };
        if now.pos.is_empty() {
            next = same.then(|| State { visible: after, pos: now.pos.clone(), toks: now.toks.clone() });
            break;
        }
        let now = Arc::clone(now);
        next = realign_in(&now, &after, ctx, if same { REACH } else { 0 });
        if !same {
            next = None;
        }
        let near = inline(ctx).near.take();
        if next.is_some() || near.is_none() {
            break;
        }
        current = Some(Arc::new(put_back(&now, &near.unwrap_or_default(), inline(ctx))));
    }
    let cx = inline(ctx);
    cx.mints_active = false;
    cx.loose = None;
    let Some(next) = next else {
        ctx.discard_trace();
        return Ok(state);
    };
    accept_trace(ctx, &next)?;
    Ok(Arc::new(if current.is_none() { next } else { relift(materialize(&next), ctx) }))
}

pub struct InlineCx {
    markdown: bool,
    engine_openers: HashMap<JsString, bool>,
    engine_tags: HashSet<JsString>,
    mint_forms: Option<Vec<Arc<GroupView>>>,
    mints_active: bool,
    mint_log: HashSet<JsString>,
    near: Option<Vec<isize>>,
    loose: Option<HashSet<JsString>>,
    cache: Option<(JsString, Option<Arc<State>>)>,
    bare: bool,
    year_range: Option<&'static Re>,
    year_gap_rules: YearGapRules,
}

impl InlineCx {
    pub fn new(
        markdown: bool,
        mint_forms: Vec<Arc<GroupView>>,
        year_range: Option<&'static Re>,
        year_gap_rules: YearGapRules,
    ) -> Self {
        InlineCx {
            year_range,
            year_gap_rules,
            markdown,
            engine_openers: HashMap::new(),
            engine_tags: HashSet::new(),
            mint_forms: Some(mint_forms),
            mints_active: false,
            mint_log: HashSet::new(),
            near: None,
            loose: None,
            cache: None,
            bare: false,
        }
    }

    pub fn note(&mut self, tokens: &[JsString; 2], nowrap: bool) {
        let open = crate::cat!("<", tokens[0].as_slice(), ">");
        let close = crate::cat!("</", tokens[1].as_slice(), ">");
        self.engine_openers.insert(open.clone(), nowrap);
        self.engine_tags.insert(open.clone());
        self.engine_tags.insert(close.clone());
        if self.mints_active {
            self.mint_log.insert(open);
            self.mint_log.insert(close);
        }
    }
}

pub struct Lifted {
    marked: Option<JsString>,
    state: Option<Arc<State>>,
    marked_mask: Option<u32>,
    visible_mask: Option<u32>,
}

pub fn materialized(group: &Lifted) -> JsString {
    group.marked.clone().unwrap_or_else(|| materialize(group.state.as_ref().expect("lifted state")))
}

fn trace_projection(state: &State) -> crate::shared::provenance::Projection {
    use crate::shared::provenance::MapEdit;
    let edits = state
        .pos
        .iter()
        .enumerate()
        .map(|(k, &start)| MapEdit {
            start: start as usize,
            end: start as usize + if state.toks.flag[k] & STANDS_IN != 0 { STAND_IN_LEN as usize } else { 0 },
            text: state.toks.str[k].clone(),
            source_map: None,
            retain: true,
        })
        .collect();
    crate::shared::provenance::Projection {
        text: state.visible.clone(),
        materialized: materialize(state),
        tokens: state.toks.clone(),
        edits,
    }
}

fn accept_trace(ctx: &mut Ctx<'_>, state: &State) -> Result<()> {
    if ctx.trace.is_some() {
        ctx.accept_trace(Some(trace_projection(state)))?;
    }
    Ok(())
}

pub fn begin_group(text: JsString, ctx: &mut Ctx<'_>) -> Result<std::result::Result<Lifted, JsString>> {
    let Some(cx) = ctx.inline.as_mut() else { return Ok(Err(text)) };
    if cx.bare {
        return Ok(Err(text));
    }
    if ctx.html && cx.mint_forms.is_some() && has_inline_tags(&text) {
        let known = cx.engine_openers.len();
        let views = cx.mint_forms.take().unwrap_or_default();
        let group = ctx.group.take();
        let mut minted = Ok(());
        for view in views {
            let names: Vec<String> = view.classes.as_ref().map_or_default(|c| c.keys().cloned().collect());
            ctx.group = Some(view);
            for name in names {
                if let Err(e) = ctx.tag_with(&[], "span", vec![("class".to_owned(), js::u(&name))]) {
                    minted = Err(e);
                    break;
                }
            }
        }
        ctx.group = group;
        minted?;
        let cx = inline(ctx);
        if cx.engine_openers.len() != known {
            cx.cache = None;
        }
    }
    Ok(Ok(Lifted { marked: Some(text), state: None, marked_mask: None, visible_mask: None }))
}

fn lifted_run(group: &mut Lifted, ctx: &mut Ctx<'_>) {
    if group.state.is_some() {
        return;
    }
    let text = group.marked.as_deref().unwrap_or_default();
    let cached = match &inline(ctx).cache {
        Some((src, state)) if src.as_slice() == text => Some(state.clone()),
        _ => None,
    };
    let state = cached.unwrap_or_else(|| {
        let state = with_lookup(ctx, |cx, look| lift(text, cx, look)).map(Arc::new);
        inline(ctx).cache = Some((text.to_vec(), state.clone()));
        state
    });
    group.visible_mask = None;
    group.state = Some(state.unwrap_or_else(|| {
        let cx = inline(ctx);
        cx.bare = !may_lift(text, cx);
        Arc::new(bare(text.to_vec()))
    }));
}

pub fn apply_rule(group: &mut Lifted, rule: &dyn LiftRule, ctx: &mut Ctx<'_>) -> Result<()> {
    if rule.html_only() {
        let marked = group.marked.take();
        let authored = marked.is_some();
        let input = marked.unwrap_or_else(|| materialize(group.state.as_ref().expect("a lifted run")));
        if !allows(&mut ctx.features, rule.requires(), &input, &mut group.marked_mask, true) {
            if authored {
                group.marked = Some(input);
            }
            return Ok(());
        }
        let Some(output) = rule.apply(&input, ctx)? else {
            group.marked = Some(input);
            ctx.accept_trace(None)?;
            return Ok(());
        };
        if output != input {
            group.state = None;
            group.marked_mask = None;
            group.visible_mask = None;
        }
        group.marked = Some(output);
        ctx.accept_trace(None)?;
        return Ok(());
    }
    let lifted = group.state.is_some();
    let allowed = if let Some(state) = &group.state {
        allows(&mut ctx.features, rule.requires(), &state.visible, &mut group.visible_mask, true)
    } else {
        let marked = group.marked.as_ref().expect("a marked-up run");
        allows(&mut ctx.features, rule.requires(), marked, &mut group.marked_mask, false)
    };
    if !allowed {
        return Ok(());
    }
    lifted_run(group, ctx);
    let state = group.state.as_ref().expect("a lifted run");
    if !lifted && !allows(&mut ctx.features, rule.requires(), &state.visible, &mut group.visible_mask, true) {
        return Ok(());
    }
    if let Some(next) = apply_lifted(state, rule, ctx)? {
        let next = if rule.lift_flags().changes_inline_boundary && !inline(ctx).bare {
            Arc::new(relift(materialize(&next), ctx))
        } else {
            next
        };
        group.state = Some(next);
        group.marked = None;
        group.marked_mask = None;
        group.visible_mask = None;
    }
    Ok(())
}

pub fn end_group(group: Lifted, ctx: &mut Ctx<'_>) -> JsString {
    let Lifted { marked, state, .. } = group;
    let text = match marked {
        Some(marked) => marked,
        None => materialize(state.as_ref().expect("a lifted run")),
    };
    if let Some(state) = state {
        let kept = (!state.pos.is_empty()).then_some(state);
        inline(ctx).cache = Some((text.clone(), kept));
    }
    text
}

pub fn wants_lift(text: &[u16], ctx: &Ctx<'_>) -> bool {
    ctx.inline.as_ref().is_some_and(|cx| may_lift(text, cx))
}
