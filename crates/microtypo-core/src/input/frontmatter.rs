use crate::js;

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
