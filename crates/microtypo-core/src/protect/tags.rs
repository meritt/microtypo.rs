use std::collections::HashSet;
use std::fmt;
use std::sync::LazyLock;

use serde_json::json;

use crate::cat;
use crate::errors::{Error, Result};
use crate::js::{self, JsString, Piece, Re};
use crate::protect::placeholders::{CLOSE, OPEN, OPEN_UNIT, TAG_VAULT_KINDS};
use crate::protect::tag_spans::{Reading, self_closing, tag_end, tag_ends_from};
use crate::protect::vault::Vault;
use crate::protect::visible::shows_nothing;
use crate::shared::block_tags::{BLOCK_TAG_NAMES, VOID_BLOCK_TAG_NAMES};
use crate::shared::tag_name::{NAME_START, TAG_NAME_SOURCE, is_tag_name, is_tag_name_start};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Layout {
    pub style: bool,
    pub class: bool,
}

pub const DEFAULT_LAYOUT: Layout = Layout { style: true, class: false };

pub fn assert_tag_name(kind: &str, name: &str) -> Result<()> {
    if !is_tag_name(name) {
        let quoted = js::lossy(&crate::js::json::stringify(&js::u(name)));
        return Err(Error::config(
            format!("ctx.tag: invalid {kind} name {quoted} — must match {TAG_NAME_SOURCE}"),
            json!({ "kind": kind, "name": name }),
        ));
    }
    Ok(())
}

static INNER_PLACEHOLDER_RE: LazyLock<Re> =
    LazyLock::new(|| Re::new(&format!(r"{OPEN}([{TAG_VAULT_KINDS}])(\d+){CLOSE}"), ""));
static VAULTED_TAG_RE: LazyLock<Re> =
    LazyLock::new(|| Re::new(&format!(r"<(\/?){OPEN}([{TAG_VAULT_KINDS}])(\d+){CLOSE}>"), "g"));

static BLOCK_BOUNDARY_NAMES: LazyLock<HashSet<&'static str>> =
    LazyLock::new(|| BLOCK_TAG_NAMES.iter().chain(VOID_BLOCK_TAG_NAMES.iter()).copied().chain(["p", "br"]).collect());
static LEADING_NAME_RE: LazyLock<Re> =
    LazyLock::new(|| Re::new(&format!(r"^([{NAME_START}][\p{{L}}\p{{N}}._:-]*)"), "u"));
const NAMED_KINDS: [u16; 3] = [0x54, 0x41, 0x4C];

fn escape_attr_into(v: &[u16], out: &mut JsString) {
    for &c in v {
        match c {
            0x26 => "&amp;".push_to(out),
            0x22 => "&quot;".push_to(out),
            0x3C => "&lt;".push_to(out),
            0x3E => "&gt;".push_to(out),
            _ => out.push(c),
        }
    }
}

pub enum NameMatcher<'a> {
    Name(&'a [u16]),
    Regex(&'a Re),
    Predicate(&'a dyn Fn(&[u16], bool) -> bool),
}

impl NameMatcher<'_> {
    fn matches(&self, name: &[u16], self_closing: bool) -> bool {
        match self {
            NameMatcher::Name(n) => js::to_lower(name) == js::to_lower(n),
            NameMatcher::Regex(re) => re.test(name),
            NameMatcher::Predicate(f) => f(name, self_closing),
        }
    }
}

#[derive(Debug, Clone)]
struct Head {
    name: JsString,
    self_closing: bool,
}

#[derive(Debug)]
pub struct TagSpec<'a> {
    pub content: &'a [u16],
    pub tag: &'a str,
    pub attributes: &'a [(String, JsString)],
    pub layout: Layout,
}

#[derive(Debug)]
pub struct MadeTag {
    pub html: JsString,
    pub tokens: [JsString; 2],
}

pub fn object_key_order<T>(entries: &mut [T], key: impl Fn(&T) -> &str) {
    fn index(k: &str) -> Option<u32> {
        let n: u32 = k.parse().ok()?;
        (n.to_string() == k && n != u32::MAX).then_some(n)
    }
    if entries.iter().any(|e| index(key(e)).is_some()) {
        entries.sort_by_key(|e| index(key(e)).map_or((1, 0), |n| (0, n)));
    }
}

#[derive(Debug, Clone)]
pub struct SafeTags {
    reading: Reading,
    tag_vault: Vault,
    anchor_vault: Vault,
    block_vault: Vault,
    iblock_vault: Vault,
    hidden_iblock_vault: Vault,
    heads: [Vec<Option<Head>>; 3],
}

#[derive(Clone, Copy)]
enum Which {
    Tag,
    Anchor,
    Block,
}

impl SafeTags {
    pub fn new(document: bool) -> Self {
        SafeTags {
            reading: if document { Reading::Document } else { Reading::Fragment },
            tag_vault: Vault::new("T"),
            anchor_vault: Vault::new("A"),
            block_vault: Vault::new("L"),
            iblock_vault: Vault::new("I"),
            hidden_iblock_vault: Vault::new("H"),
            heads: [Vec::new(), Vec::new(), Vec::new()],
        }
    }

    pub fn reset(&mut self) {
        for v in [
            &mut self.tag_vault,
            &mut self.anchor_vault,
            &mut self.block_vault,
            &mut self.iblock_vault,
            &mut self.hidden_iblock_vault,
        ] {
            v.reset();
        }
        for h in &mut self.heads {
            h.clear();
        }
    }

    fn vault(&self, prefix: u16) -> Option<&Vault> {
        match prefix {
            0x54 => Some(&self.tag_vault),
            0x41 => Some(&self.anchor_vault),
            0x4C => Some(&self.block_vault),
            0x49 => Some(&self.iblock_vault),
            0x48 => Some(&self.hidden_iblock_vault),
            _ => None,
        }
    }

    fn which(content: &[u16]) -> Which {
        let Some(m) = LEADING_NAME_RE.exec(content) else { return Which::Tag };
        let name = js::to_lower(m.s(1));
        if name == [0x61] {
            return Which::Anchor;
        }
        let ascii = name.iter().all(|&c| c < 0x80);
        if ascii && BLOCK_BOUNDARY_NAMES.contains(js::lossy(&name).as_str()) { Which::Block } else { Which::Tag }
    }

    fn store_for(&mut self, content: &[u16]) -> Result<JsString> {
        let mut token = JsString::new();
        self.store_for_into(content, &mut token)?;
        Ok(token)
    }

    fn store_for_into(&mut self, content: &[u16], out: &mut JsString) -> Result<()> {
        match Self::which(content) {
            Which::Tag => self.tag_vault.store_into(content, out),
            Which::Anchor => self.anchor_vault.store_into(content, out),
            Which::Block => self.block_vault.store_into(content, out),
        }
    }

    pub fn protect(&mut self, text: &[u16]) -> Result<JsString> {
        let mut out: Option<JsString> = None;
        let mut cursor = 0usize;
        let mut ends: Option<Vec<isize>> = None;
        let mut lt = js::index_of_unit(text, 0x3C, 0);
        while let Some(at_lt) = lt {
            let at = if text.get(at_lt + 1) == Some(&0x2F) { at_lt + 2 } else { at_lt + 1 };
            let first = js::code_point_at(text, at);
            if !is_tag_name_start(first) && first != Some(u32::from(OPEN_UNIT)) {
                lt = js::index_of_unit(text, 0x3C, at_lt + 1);
                continue;
            }
            let gt = match &ends {
                None => tag_end(text, at, self.reading),
                Some(ends) => ends[at],
            };
            if gt < 0 {
                if gt == -1 && ends.is_none() {
                    ends = Some(tag_ends_from(text, at + 1, self.reading));
                }
                lt = js::index_of_unit(text, 0x3C, at_lt + 1);
                continue;
            }
            let gt = gt as usize;
            let content = &text[at..gt];
            if !INNER_PLACEHOLDER_RE.test(content) {
                let out = out.get_or_insert_with(|| JsString::with_capacity(text.len()));
                out.extend_from_slice(&text[cursor..at]);
                self.store_for_into(content, out)?;
                cursor = gt;
            }
            lt = js::index_of_unit(text, 0x3C, gt + 1);
        }
        let Some(mut out) = out else { return Ok(text.to_vec()) };
        out.extend_from_slice(&text[cursor..]);
        Ok(out)
    }

    pub fn restore(&self, text: JsString) -> JsString {
        VAULTED_TAG_RE.replace_owned(text, |c| {
            let prefix = c.s(2)[0];
            let id = js::parse_int(c.s(3)).and_then(|n| usize::try_from(n).ok());
            match id.and_then(|id| self.vault(prefix)?.retrieve(id)) {
                Some(value) => cat!("<", c.s(1), value, ">"),
                None => c.whole().to_vec(),
            }
        })
    }

    pub fn make_tag(&mut self, spec: TagSpec<'_>) -> Result<MadeTag> {
        let attrs = spec.attributes;
        let find = |k: &str| attrs.iter().position(|(n, _)| n == k);
        let extra = find("__style").map(|i| attrs[i].1.as_slice()).filter(|v| spec.layout.style && !v.is_empty());
        let style_at = find("style");
        let joined = match (extra, style_at) {
            (Some(extra), Some(i)) if !attrs[i].1.is_empty() => {
                let mut st = js::trim(&attrs[i].1).to_vec();
                if st.last() != Some(&0x3B) {
                    st.push(0x3B);
                }
                st.extend_from_slice(extra);
                Some(st)
            }
            _ => None,
        };
        let mut list: Vec<(&str, &[u16])> = Vec::with_capacity(attrs.len() + 1);
        for (i, (k, v)) in attrs.iter().enumerate() {
            if k == "__style" {
                continue;
            }
            let v = match extra {
                Some(extra) if Some(i) == style_at => joined.as_deref().unwrap_or(extra),
                _ => v.as_slice(),
            };
            list.push((k, v));
        }
        if let (Some(extra), None) = (extra, style_at) {
            list.push(("style", extra));
        }
        object_key_order(&mut list, |e| e.0);
        let mut head = JsString::with_capacity(64);
        spec.tag.push_to(&mut head);
        let mut class_name: &[u16] = &[];
        for &(k, v) in &list {
            if k == "class" {
                class_name = v;
                continue;
            }
            ' '.push_to(&mut head);
            k.push_to(&mut head);
            "=\"".push_to(&mut head);
            escape_attr_into(v, &mut head);
            '"'.push_to(&mut head);
        }
        if spec.layout.class && !class_name.is_empty() {
            " class=\"".push_to(&mut head);
            escape_attr_into(class_name, &mut head);
            '"'.push_to(&mut head);
        }
        let open = self.store_for(&head)?;
        head.clear();
        spec.tag.push_to(&mut head);
        let close = self.store_for(&head)?;
        Ok(MadeTag { html: cat!("<", open, ">", spec.content, "</", close, ">"), tokens: [open, close] })
    }

    pub fn iblock(&mut self, content: &[u16]) -> Result<JsString> {
        if shows_nothing(content) { self.hidden_iblock_vault.store(content) } else { self.iblock_vault.store(content) }
    }

    pub fn restore_iblocks(&self, text: JsString) -> JsString {
        Vault::restore_fixpoint(&[&self.iblock_vault, &self.hidden_iblock_vault], text)
    }

    pub fn find_by_tag_name(&mut self, matcher: &NameMatcher<'_>) -> Vec<(char, usize)> {
        let mut out = Vec::new();
        for (k, &prefix) in NAMED_KINDS.iter().enumerate() {
            self.fill_heads(k, prefix);
            for (id, head) in self.heads[k].iter().enumerate() {
                if let Some(head) = head
                    && matcher.matches(&head.name, head.self_closing)
                {
                    out.push((char::from(prefix as u8), id));
                }
            }
        }
        out
    }

    fn fill_heads(&mut self, k: usize, prefix: u16) {
        let size = self.vault(prefix).map_or(0, Vault::size);
        for id in self.heads[k].len()..size {
            let raw = self.vault(prefix).and_then(|v| v.retrieve(id)).map(<[u16]>::to_vec).unwrap_or_default();
            let head = LEADING_NAME_RE.exec(&raw).map(|m| {
                let wrapped = cat!("<", raw.as_slice(), ">");
                Head {
                    name: m.s(1).to_vec(),
                    self_closing: raw.last() == Some(&0x2F) && self_closing(&wrapped, 0, raw.len() + 1),
                }
            });
            self.heads[k].push(head);
        }
    }
}

impl fmt::Debug for NameMatcher<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            NameMatcher::Name(name) => f.debug_tuple("Name").field(&String::from_utf16_lossy(name)).finish(),
            NameMatcher::Regex(re) => f.debug_tuple("Regex").field(&re.source()).finish(),
            NameMatcher::Predicate(_) => f.write_str("Predicate"),
        }
    }
}
