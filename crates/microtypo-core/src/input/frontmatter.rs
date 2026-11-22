use std::sync::LazyLock;

use crate::js::{self, Re};

static YAML_KEY_RE: LazyLock<Re> = LazyLock::new(|| Re::new(r"^[^\s#][^\r\n]*?:(?:[ \t]|$)", "u"));

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Frontmatter {
    Malformed,
    Split { toml: bool, header_start: usize, header_end: usize, body_start: usize },
}

fn line_at(src: &[u16], i: usize) -> (usize, usize) {
    let mut k = i;
    while k < src.len() {
        match src[k] {
            0x0A => return (k, k + 1),
            0x0D => return (k, if src.get(k + 1) == Some(&0x0A) { k + 2 } else { k + 1 }),
            _ => k += 1,
        }
    }
    (src.len(), src.len())
}

pub fn split_frontmatter(src: &[u16]) -> Option<Frontmatter> {
    let n = src.len();
    let offset = usize::from(src.first() == Some(&0xFEFF));
    let (open_end, header_start) = line_at(src, offset);
    let open = &src[offset..open_end];
    let dashes = js::u("---");
    let pluses = js::u("+++");
    if open != dashes.as_slice() && open != pluses.as_slice() {
        return None;
    }
    if open_end >= n {
        return Some(Frontmatter::Malformed);
    }
    let toml = open == pluses.as_slice();
    let dots = js::u("...");
    let mut i = header_start;
    while i < n {
        let (content_end, next) = line_at(src, i);
        let line = &src[i..content_end];
        let closes =
            if toml { line == pluses.as_slice() } else { line == dashes.as_slice() || line == dots.as_slice() };
        if closes {
            return Some(Frontmatter::Split { toml, header_start, header_end: i, body_start: next });
        }
        i = next;
    }
    Some(Frontmatter::Malformed)
}

fn reads_as_yaml_header(header: &[u16]) -> bool {
    if header.is_empty() {
        return true;
    }
    if js::trim(&header[..line_at(header, 0).0]).is_empty() {
        return false;
    }
    let mut i = 0;
    while i < header.len() {
        let (end, next) = line_at(header, i);
        let line = &header[i..end];
        if !js::trim(line).is_empty() && js::trim_start(line).first() != Some(&0x23) {
            return YAML_KEY_RE.test(line);
        }
        i = next;
    }
    true
}

pub fn scan_leading_header(src: &[u16]) -> Vec<(usize, usize)> {
    match split_frontmatter(src) {
        Some(Frontmatter::Split { toml, header_start, header_end, .. })
            if toml || reads_as_yaml_header(&src[header_start..header_end]) =>
        {
            vec![(0, header_end + 3)]
        }
        _ => Vec::new(),
    }
}

pub fn body_boundary(src: &[u16]) -> usize {
    let mut boundary = 0;
    loop {
        let mut end = boundary;
        while matches!(src.get(end), Some(0x20 | 0x09)) {
            end += 1;
        }
        boundary = match src.get(end) {
            Some(0x0D) => end + if src.get(end + 1) == Some(&0x0A) { 2 } else { 1 },
            Some(0x0A) => end + 1,
            _ => return boundary,
        };
    }
}
