pub mod json;
pub mod regex;

use std::sync::LazyLock;

pub use regex::{Caps, Re, escape, escape_str};

pub type JsString = Vec<u16>;

pub fn u(s: &str) -> JsString {
    s.encode_utf16().collect()
}

pub fn lossy(s: &[u16]) -> String {
    String::from_utf16_lossy(s)
}

pub type Lit = LazyLock<JsString>;

#[macro_export]
macro_rules! lit {
    ($s:expr) => {
        ::std::sync::LazyLock::new(|| $crate::js::u($s))
    };
}

#[macro_export]
macro_rules! cat {
    ($($piece:expr),* $(,)?) => {{
        let pieces: &[&dyn $crate::js::Piece] = &[$(&$piece),*];
        let mut out: $crate::js::JsString =
            ::std::vec::Vec::with_capacity(pieces.iter().map(|p| p.size_hint()).sum());
        for piece in pieces {
            piece.push_to(&mut out);
        }
        out
    }};
}

pub trait Piece {
    fn push_to(&self, out: &mut JsString);

    fn size_hint(&self) -> usize;
}

impl Piece for &[u16] {
    fn push_to(&self, out: &mut JsString) {
        out.extend_from_slice(self);
    }

    fn size_hint(&self) -> usize {
        self.len()
    }
}

impl Piece for JsString {
    fn push_to(&self, out: &mut JsString) {
        out.extend_from_slice(self);
    }

    fn size_hint(&self) -> usize {
        self.len()
    }
}

impl Piece for &JsString {
    fn push_to(&self, out: &mut JsString) {
        out.extend_from_slice(self);
    }

    fn size_hint(&self) -> usize {
        self.len()
    }
}

impl Piece for &str {
    fn push_to(&self, out: &mut JsString) {
        out.extend(self.encode_utf16());
    }

    fn size_hint(&self) -> usize {
        self.len()
    }
}

impl Piece for String {
    fn push_to(&self, out: &mut JsString) {
        out.extend(self.encode_utf16());
    }

    fn size_hint(&self) -> usize {
        self.len()
    }
}

impl Piece for char {
    fn push_to(&self, out: &mut JsString) {
        let mut buf = [0u16; 2];
        out.extend_from_slice(self.encode_utf16(&mut buf));
    }

    fn size_hint(&self) -> usize {
        self.len_utf16()
    }
}

impl Piece for u16 {
    fn push_to(&self, out: &mut JsString) {
        out.push(*self);
    }

    fn size_hint(&self) -> usize {
        1
    }
}

impl Piece for Option<&[u16]> {
    fn push_to(&self, out: &mut JsString) {
        if let Some(s) = self {
            out.extend_from_slice(s);
        }
    }

    fn size_hint(&self) -> usize {
        self.map_or(0, <[u16]>::len)
    }
}

impl<T: Piece> Piece for LazyLock<T> {
    fn push_to(&self, out: &mut JsString) {
        T::push_to(self, out);
    }

    fn size_hint(&self) -> usize {
        T::size_hint(self)
    }
}

#[derive(Debug)]
pub struct Num(pub usize);

impl Piece for Num {
    fn push_to(&self, out: &mut JsString) {
        let start = out.len();
        let mut n = self.0;
        loop {
            out.push(0x30 + (n % 10) as u16);
            n /= 10;
            if n == 0 {
                break;
            }
        }
        out[start..].reverse();
    }

    fn size_hint(&self) -> usize {
        20
    }
}

pub fn is_high(c: u16) -> bool {
    (0xD800..0xDC00).contains(&c)
}

pub fn is_low(c: u16) -> bool {
    (0xDC00..0xE000).contains(&c)
}

pub fn code_point_at(s: &[u16], i: usize) -> Option<u32> {
    let c = *s.get(i)?;
    if is_high(c)
        && let Some(&d) = s.get(i + 1)
        && is_low(d)
    {
        return Some(0x10000 + ((u32::from(c) - 0xD800) << 10) + (u32::from(d) - 0xDC00));
    }
    Some(u32::from(c))
}

pub fn from_code_point(cp: u32) -> JsString {
    if cp >= 0x10000 {
        let v = cp - 0x10000;
        vec![(0xD800 + (v >> 10)) as u16, (0xDC00 + (v & 0x3FF)) as u16]
    } else {
        vec![cp as u16]
    }
}

pub fn code_points(s: &[u16]) -> impl Iterator<Item = &[u16]> {
    let mut i = 0;
    std::iter::from_fn(move || {
        if i >= s.len() {
            return None;
        }
        let w = if is_high(s[i]) && i + 1 < s.len() && is_low(s[i + 1]) { 2 } else { 1 };
        let item = &s[i..i + w];
        i += w;
        Some(item)
    })
}

pub fn is_js_space(c: u16) -> bool {
    matches!(
        c,
        0x09..=0x0D | 0x20 | 0xA0 | 0x1680 | 0x2000..=0x200A | 0x2028 | 0x2029 | 0x202F | 0x205F | 0x3000 | 0xFEFF
    )
}

pub fn index_of(hay: &[u16], needle: &[u16], from: usize) -> Option<usize> {
    if needle.is_empty() {
        return Some(from.min(hay.len()));
    }
    if needle.len() > hay.len() {
        return None;
    }
    let last = hay.len() - needle.len();
    let mut i = from;
    while i <= last {
        if hay[i] == needle[0] && &hay[i..i + needle.len()] == needle {
            return Some(i);
        }
        i += 1;
    }
    None
}

pub fn index_of_unit(hay: &[u16], unit: u16, from: usize) -> Option<usize> {
    hay.get(from..)?.iter().position(|&c| c == unit).map(|p| p + from)
}

pub fn last_index_of_from(hay: &[u16], needle: &[u16], from: usize) -> Option<usize> {
    if needle.len() > hay.len() {
        return None;
    }
    let mut i = from.min(hay.len() - needle.len());
    loop {
        if &hay[i..i + needle.len()] == needle {
            return Some(i);
        }
        if i == 0 {
            return None;
        }
        i -= 1;
    }
}

pub fn last_index_of(hay: &[u16], needle: &[u16]) -> Option<usize> {
    last_index_of_from(hay, needle, hay.len())
}

pub fn includes(hay: &[u16], needle: &[u16]) -> bool {
    index_of(hay, needle, 0).is_some()
}

pub fn starts_with_at(hay: &[u16], needle: &[u16], at: usize) -> bool {
    hay.len() >= at + needle.len() && &hay[at..at + needle.len()] == needle
}

pub fn slice(s: &[u16], start: usize, end: usize) -> &[u16] {
    let end = end.min(s.len());
    let start = start.min(end);
    &s[start..end]
}

pub fn slice_signed(s: &[u16], start: isize, end: Option<isize>) -> &[u16] {
    let len = s.len() as isize;
    let norm = |v: isize| if v < 0 { (len + v).max(0) } else { v.min(len) } as usize;
    let from = norm(start);
    let to = end.map_or(s.len(), norm);
    if from >= to { &s[0..0] } else { &s[from..to] }
}

pub fn from(s: &[u16], start: usize) -> &[u16] {
    &s[start.min(s.len())..]
}

pub fn at(s: &[u16], i: isize) -> Option<u16> {
    if i < 0 { None } else { s.get(i as usize).copied() }
}

pub fn trim(s: &[u16]) -> &[u16] {
    let mut start = 0;
    let mut end = s.len();
    while start < end && is_js_space(s[start]) {
        start += 1;
    }
    while end > start && is_js_space(s[end - 1]) {
        end -= 1;
    }
    &s[start..end]
}

pub fn trim_start(s: &[u16]) -> &[u16] {
    let mut start = 0;
    while start < s.len() && is_js_space(s[start]) {
        start += 1;
    }
    &s[start..]
}

pub fn trim_end(s: &[u16]) -> &[u16] {
    let mut end = s.len();
    while end > 0 && is_js_space(s[end - 1]) {
        end -= 1;
    }
    &s[..end]
}

pub fn split<'a>(s: &'a [u16], sep: &[u16]) -> Vec<&'a [u16]> {
    let mut out = Vec::new();
    if sep.is_empty() {
        for i in 0..s.len() {
            out.push(&s[i..=i]);
        }
        return out;
    }
    let mut from = 0;
    while let Some(at) = index_of(s, sep, from) {
        out.push(&s[from..at]);
        from = at + sep.len();
    }
    out.push(&s[from..]);
    out
}

pub fn join<T: AsRef<[u16]>>(parts: &[T], sep: &[u16]) -> JsString {
    let mut out = Vec::new();
    for (i, p) in parts.iter().enumerate() {
        if i > 0 {
            out.extend_from_slice(sep);
        }
        out.extend_from_slice(p.as_ref());
    }
    out
}

pub fn replace_all_lit(s: &[u16], from: &[u16], to: &[u16]) -> JsString {
    if from.is_empty() {
        return s.to_vec();
    }
    let mut out = Vec::with_capacity(s.len());
    let mut i = 0;
    while let Some(at) = index_of(s, from, i) {
        out.extend_from_slice(&s[i..at]);
        out.extend_from_slice(to);
        i = at + from.len();
    }
    out.extend_from_slice(&s[i..]);
    out
}

pub fn replace_first_lit(s: &[u16], from: &[u16], to: &[u16]) -> JsString {
    match index_of(s, from, 0) {
        Some(at) => cat!(&s[..at], to, &s[at + from.len()..]),
        None => s.to_vec(),
    }
}

pub fn repeat(s: &[u16], n: usize) -> JsString {
    s.repeat(n)
}

fn map_case(s: &[u16], upper: bool) -> JsString {
    let mut out = Vec::with_capacity(s.len());
    let mut pending = String::new();
    let flush = |pending: &mut String, out: &mut JsString| {
        let mapped = if upper { pending.to_uppercase() } else { pending.to_lowercase() };
        out.extend(mapped.encode_utf16());
        pending.clear();
    };
    for chunk in char::decode_utf16(s.iter().copied()) {
        match chunk {
            Ok(c) => pending.push(c),
            Err(e) => {
                flush(&mut pending, &mut out);
                out.push(e.unpaired_surrogate());
            }
        }
    }
    flush(&mut pending, &mut out);
    out
}

pub fn to_lower(s: &[u16]) -> JsString {
    if s.iter().all(|&c| c < 0x80) {
        return s.iter().map(|&c| if (u16::from(b'A')..=u16::from(b'Z')).contains(&c) { c + 32 } else { c }).collect();
    }
    map_case(s, false)
}

pub fn to_upper(s: &[u16]) -> JsString {
    if s.iter().all(|&c| c < 0x80) {
        return s.iter().map(|&c| if (u16::from(b'a')..=u16::from(b'z')).contains(&c) { c - 32 } else { c }).collect();
    }
    map_case(s, true)
}

pub fn is_ascii_digit(c: Option<u16>) -> bool {
    matches!(c, Some(0x30..=0x39))
}

pub fn parse_int(s: &[u16]) -> Option<i64> {
    let t = trim(s);
    let (neg, digits) = match t.first() {
        Some(0x2D) => (true, &t[1..]),
        Some(0x2B) => (false, &t[1..]),
        _ => (false, t),
    };
    let mut n: i64 = 0;
    let mut any = false;
    for &c in digits {
        if !(0x30..=0x39).contains(&c) {
            break;
        }
        any = true;
        n = n.saturating_mul(10).saturating_add(i64::from(c - 0x30));
    }
    if any { Some(if neg { -n } else { n }) } else { None }
}

pub fn num(n: usize) -> JsString {
    let mut out = JsString::with_capacity(20);
    Num(n).push_to(&mut out);
    out
}
