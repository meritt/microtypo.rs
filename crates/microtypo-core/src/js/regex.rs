use std::collections::HashMap;
use std::fmt::Write as _;
use std::sync::{Arc, LazyLock, Mutex, PoisonError};

use super::{JsString, is_high, is_low};

#[derive(Clone)]
pub struct Re {
    inner: Arc<Compiled>,
    pub global: bool,
    pub sticky: bool,
    pub unicode: bool,
    named: bool,
}

struct Compiled {
    regex: regress::Regex,
    source: String,
    flags: String,
}

impl std::fmt::Debug for Re {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "/{}/{}", self.source(), self.flags())
    }
}

#[derive(Debug)]
pub struct Caps<'t> {
    pub input: &'t [u16],
    m: regress::Match,
}

impl<'t> Caps<'t> {
    pub fn get(&self, i: usize) -> Option<&'t [u16]> {
        self.m.group(i).map(|r| &self.input[r])
    }

    pub fn s(&self, i: usize) -> &'t [u16] {
        self.get(i).unwrap_or(&[])
    }

    pub fn has(&self, i: usize) -> bool {
        self.m.group(i).is_some()
    }

    pub fn whole(&self) -> &'t [u16] {
        &self.input[self.m.range()]
    }

    pub fn index(&self) -> usize {
        self.m.start()
    }

    pub fn end(&self) -> usize {
        self.m.end()
    }

    pub fn group_count(&self) -> usize {
        self.m.captures.len()
    }

    pub fn range(&self, i: usize) -> Option<std::ops::Range<usize>> {
        self.m.group(i)
    }

    pub fn named(&self, name: &str) -> Option<&'t [u16]> {
        self.m.named_group(name).map(|r| &self.input[r])
    }
}

#[derive(Debug)]
pub struct RegexSyntaxError(pub String);

impl Re {
    pub fn new(source: &str, flags: &str) -> Re {
        Re::try_new(source, flags).unwrap_or_else(|e| panic!("invalid built-in pattern /{source}/{flags}: {}", e.0))
    }

    pub fn try_new(source: &str, flags: &str) -> Result<Re, RegexSyntaxError> {
        for (i, c) in flags.char_indices() {
            if !"dgimsuvy".contains(c) || flags[..i].contains(c) {
                return Err(RegexSyntaxError(format!("Invalid flags supplied to RegExp constructor '{flags}'")));
            }
        }
        Ok(Re {
            inner: compile(source, flags)?,
            global: flags.contains('g'),
            sticky: flags.contains('y'),
            unicode: flags.contains('u') || flags.contains('v'),
            named: source.contains("(?<") && has_named_group(source),
        })
    }

    pub fn source(&self) -> &str {
        &self.inner.source
    }

    pub fn flags(&self) -> &str {
        &self.inner.flags
    }

    #[must_use]
    pub fn with_flags(&self, flags: &str) -> Re {
        Re::new(self.source(), flags)
    }

    fn with_search<R>(
        &self,
        text: &[u16],
        scan: impl FnOnce(&mut dyn FnMut(usize) -> Option<regress::Match>) -> R,
    ) -> R {
        let re = &self.inner.regex;
        if self.unicode {
            let mut matches = re.find_from_utf16(text, 0);
            scan(&mut |from| matches.find_at(from))
        } else {
            let mut matches = re.find_from_ucs2(text, 0);
            scan(&mut |from| matches.find_at(from))
        }
    }

    fn exec_with<'t>(
        &self,
        text: &'t [u16],
        search: &mut dyn FnMut(usize) -> Option<regress::Match>,
        last_index: usize,
    ) -> Option<Caps<'t>> {
        let m = search(last_index)?;
        if self.sticky && m.start() != last_index {
            return None;
        }
        Some(Caps { input: text, m })
    }

    pub fn exec_at<'t>(&self, text: &'t [u16], last_index: usize) -> Option<Caps<'t>> {
        self.with_search(text, |search| self.exec_with(text, search, last_index))
    }

    fn next_index(&self, text: &[u16], c: &Caps<'_>) -> usize {
        if c.end() == c.index() { self.advance(text, c.end()) } else { c.end() }
    }

    pub fn test(&self, text: &[u16]) -> bool {
        self.exec_at(text, 0).is_some()
    }

    pub fn exec<'t>(&self, text: &'t [u16]) -> Option<Caps<'t>> {
        self.exec_at(text, 0)
    }

    pub fn advance(&self, text: &[u16], index: usize) -> usize {
        if !self.unicode || index + 1 >= text.len() {
            return index + 1;
        }
        if is_high(text[index]) && is_low(text[index + 1]) { index + 2 } else { index + 1 }
    }

    pub fn find_all<'t>(&self, text: &'t [u16]) -> Vec<Caps<'t>> {
        self.with_search(text, |search| {
            let mut out = Vec::new();
            let mut last = 0usize;
            while let Some(c) = self.exec_with(text, search, last) {
                last = self.next_index(text, &c);
                out.push(c);
                if last > text.len() {
                    break;
                }
            }
            out
        })
    }

    pub fn replace_with<F>(&self, text: &[u16], f: F) -> JsString
    where
        F: FnMut(&Caps<'_>) -> JsString,
    {
        self.replace_matches(text, f).unwrap_or_else(|| text.to_vec())
    }

    pub fn replace_owned<F>(&self, text: JsString, f: F) -> JsString
    where
        F: FnMut(&Caps<'_>) -> JsString,
    {
        self.replace_matches(&text, f).unwrap_or(text)
    }

    pub fn replace_matches<F>(&self, text: &[u16], mut f: F) -> Option<JsString>
    where
        F: FnMut(&Caps<'_>) -> JsString,
    {
        self.replace_into(text, |c, out| out.extend_from_slice(&f(c)))
    }

    fn replace_into<F>(&self, text: &[u16], mut write: F) -> Option<JsString>
    where
        F: FnMut(&Caps<'_>, &mut JsString),
    {
        self.with_search(text, |search| {
            let mut out: Option<JsString> = None;
            let mut next = 0;
            let mut last = 0usize;
            while let Some(c) = self.exec_with(text, search, last) {
                let out = out.get_or_insert_with(|| Vec::with_capacity(text.len()));
                if c.index() >= next {
                    out.extend_from_slice(&text[next..c.index()]);
                    write(&c, out);
                    next = c.end();
                } else {
                    write(&c, &mut Vec::new());
                }
                last = self.next_index(text, &c);
                if !self.global || last > text.len() {
                    break;
                }
            }
            let mut out = out?;
            out.extend_from_slice(&text[next.min(text.len())..]);
            Some(out)
        })
    }

    pub fn try_replace_with<F, E>(&self, text: &[u16], f: F) -> Result<JsString, E>
    where
        F: FnMut(&Caps<'_>) -> Result<JsString, E>,
    {
        Ok(self.try_replace_matches(text, f)?.unwrap_or_else(|| text.to_vec()))
    }

    pub fn try_replace_matches<F, E>(&self, text: &[u16], mut f: F) -> Result<Option<JsString>, E>
    where
        F: FnMut(&Caps<'_>) -> Result<JsString, E>,
    {
        let mut error = None;
        let out = self.replace_matches(text, |c| {
            if error.is_some() {
                return Vec::new();
            }
            f(c).unwrap_or_else(|e| {
                error = Some(e);
                Vec::new()
            })
        });
        match error {
            Some(e) => Err(e),
            None => Ok(out),
        }
    }

    pub fn replace_tpl(&self, text: &[u16], template: &[u16]) -> JsString {
        self.replace_tpl_matches(text, template).unwrap_or_else(|| text.to_vec())
    }

    pub fn replace_tpl_matches(&self, text: &[u16], template: &[u16]) -> Option<JsString> {
        if !template.contains(&0x24) {
            return self.replace_into(text, |_, out| out.extend_from_slice(template));
        }
        let named = self.named;
        self.replace_into(text, |c, out| substitute_into(c, template, named, out))
    }

    pub fn replace_tpl_owned(&self, text: JsString, template: &[u16]) -> JsString {
        self.replace_tpl_matches(&text, template).unwrap_or(text)
    }

    pub fn replace_str(&self, text: &[u16], template: &str) -> JsString {
        self.replace_tpl(text, &super::u(template))
    }
}

static COMPILED: LazyLock<Mutex<HashMap<String, Vec<Arc<Compiled>>>>> = LazyLock::new(Default::default);
const COMPILED_LIMIT: usize = 4096;

fn compile(source: &str, flags: &str) -> Result<Arc<Compiled>, RegexSyntaxError> {
    let cached = |compiled: &HashMap<String, Vec<Arc<Compiled>>>| {
        compiled.get(source).and_then(|variants| variants.iter().find(|c| c.flags == flags).cloned())
    };
    if let Some(c) = cached(&COMPILED.lock().unwrap_or_else(PoisonError::into_inner)) {
        return Ok(c);
    }
    let regex = regress::Regex::with_flags(source, flags).map_err(|e| RegexSyntaxError(e.to_string()))?;
    let c = Arc::new(Compiled { regex, source: source.to_owned(), flags: flags.to_owned() });
    let mut compiled = COMPILED.lock().unwrap_or_else(PoisonError::into_inner);
    if compiled.len() >= COMPILED_LIMIT {
        compiled.clear();
    }
    compiled.entry(source.to_owned()).or_default().push(c.clone());
    Ok(c)
}

fn has_named_group(source: &str) -> bool {
    let b = source.as_bytes();
    let mut i = 0;
    while i + 3 < b.len() {
        if b[i] == b'\\' {
            i += 2;
            continue;
        }
        if b[i] == b'(' && b[i + 1] == b'?' && b[i + 2] == b'<' && b[i + 3] != b'=' && b[i + 3] != b'!' {
            return true;
        }
        i += 1;
    }
    false
}

pub fn substitute(c: &Caps<'_>, template: &[u16], named: bool) -> JsString {
    let mut out = Vec::new();
    substitute_into(c, template, named, &mut out);
    out
}

fn substitute_into(c: &Caps<'_>, template: &[u16], named: bool, out: &mut JsString) {
    let text = c.input;
    let m = c.group_count();
    let mut i = 0;
    let digit = |u: u16| (0x30..=0x39).contains(&u);
    while i < template.len() {
        let ch = template[i];
        if ch != 0x24 || i + 1 >= template.len() {
            out.push(ch);
            i += 1;
            continue;
        }
        let next = template[i + 1];
        match next {
            0x24 => {
                out.push(0x24);
                i += 2;
            }
            0x26 => {
                out.extend_from_slice(c.whole());
                i += 2;
            }
            0x60 => {
                out.extend_from_slice(&text[..c.index()]);
                i += 2;
            }
            0x27 => {
                out.extend_from_slice(&text[c.end().min(text.len())..]);
                i += 2;
            }
            d if digit(d) => {
                let two = i + 2 < template.len() && digit(template[i + 2]);
                let mut count = if two { 2 } else { 1 };
                let mut index = usize::from(d - 0x30);
                if two {
                    index = index * 10 + usize::from(template[i + 2] - 0x30);
                }
                if index > m && count == 2 {
                    count = 1;
                    index = usize::from(d - 0x30);
                }
                if (1..=m).contains(&index) {
                    if let Some(g) = c.get(index) {
                        out.extend_from_slice(g);
                    }
                } else {
                    out.extend_from_slice(&template[i..i + 1 + count]);
                }
                i += 1 + count;
            }
            0x3C if named => {
                let rest = &template[i + 2..];
                if let Some(end) = rest.iter().position(|&u| u == 0x3E) {
                    let name = String::from_utf16_lossy(&rest[..end]);
                    if let Some(g) = c.named(&name) {
                        out.extend_from_slice(g);
                    }
                    i += 2 + end + 1;
                } else {
                    out.extend_from_slice(&template[i..i + 2]);
                    i += 2;
                }
            }
            _ => {
                out.push(0x24);
                i += 1;
            }
        }
    }
}

pub fn escape(s: &[u16]) -> String {
    let mut out = String::new();
    for (i, &c) in s.iter().enumerate() {
        let ascii_alnum = c < 0x80 && (c as u8).is_ascii_alphanumeric();
        if ascii_alnum && i > 0 {
            out.push(c as u8 as char);
        } else if c < 0x80 && b"^$\\.*+?()[]{}|/".contains(&(c as u8)) {
            out.push('\\');
            out.push(c as u8 as char);
        } else if c < 0x100 {
            let _ = write!(out, "\\x{c:02x}");
        } else {
            let _ = write!(out, "\\u{c:04x}");
        }
    }
    out
}

pub fn escape_str(s: &str) -> String {
    escape(&super::u(s))
}
