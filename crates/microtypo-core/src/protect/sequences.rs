use std::collections::HashMap;
use std::sync::LazyLock;

use serde_json::json;

use crate::cat;
use crate::errors::{MAX_ENTRIES, Result, capacity_error};
use crate::js::{self, Caps, JsString, Re};
use crate::shared::url_email_regex::{EMAIL_REGEX, IPV6_REGEX, URL_REGEX, UUID_REGEX};

static URL_RESTORE_RE: LazyLock<Re> =
    LazyLock::new(|| Re::new(r"http:\/\/simonenko-xyz\/A0SAFE([0-9A-F]+)NUM(\d+)ID", "gms"));
static URL_RESTORE_NOPROTO_RE: LazyLock<Re> =
    LazyLock::new(|| Re::new(r"simonenko-xyz\/A0SAFE([0-9A-F]+)NUM(\d+)ID", "gms"));
static EMAIL_RESTORE_RE: LazyLock<Re> = LazyLock::new(|| Re::new(r"A1SAFE([0-9A-F]+)NUM(\d+)ID@simonenko\.xyz", "gms"));
static TOKEN_RESTORE_RE: LazyLock<Re> = LazyLock::new(|| Re::new(r"AzSAFE([0-9A-F]+)TOK(\d+)TK", "gms"));
static SCHEME_RE: LazyLock<Re> = LazyLock::new(|| Re::new(r"^[^:]+:\/\/", ""));

pub static RESTORE_REGEXES: LazyLock<Vec<Re>> = LazyLock::new(|| {
    vec![URL_RESTORE_RE.clone(), URL_RESTORE_NOPROTO_RE.clone(), EMAIL_RESTORE_RE.clone(), TOKEN_RESTORE_RE.clone()]
});

#[derive(Clone, Copy)]
enum Kind {
    Url,
    Email,
    Token,
}

impl Kind {
    fn letter(self) -> u16 {
        match self {
            Kind::Url => 0x75,
            Kind::Email => 0x65,
            Kind::Token => 0x74,
        }
    }

    fn name(self) -> (&'static str, &'static str) {
        match self {
            Kind::Url => ("URLs", "url"),
            Kind::Email => ("emails", "email"),
            Kind::Token => ("tokens", "token"),
        }
    }
}

struct EntropyPool {
    bytes: [u8; 256],
    used: usize,
}

thread_local! {
    static ENTROPY: std::cell::RefCell<EntropyPool> = const { std::cell::RefCell::new(EntropyPool { bytes: [0; 256], used: 256 }) };
}

fn fresh_nonce() -> String {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    let drawn = ENTROPY.with(|pool| {
        let mut pool = pool.borrow_mut();
        if pool.used == pool.bytes.len() {
            getrandom::fill(&mut pool.bytes).ok()?;
            pool.used = 0;
        }
        let at = pool.used;
        pool.used += 8;
        let mut bytes = [0u8; 8];
        bytes.copy_from_slice(&pool.bytes[at..at + 8]);
        pool.bytes[at..at + 8].fill(0);
        Some(bytes)
    });
    let bytes = drawn.expect("the system CSPRNG is unavailable");
    bytes.iter().flat_map(|b| [HEX[usize::from(b >> 4)] as char, HEX[usize::from(b & 15)] as char]).collect()
}

fn trim_url_parens(url: &[u16]) -> &[u16] {
    let open = url.iter().filter(|&&c| c == 0x28).count();
    let mut close = url.iter().filter(|&&c| c == 0x29).count();
    let mut end = url.len();
    while end > 0 && url[end - 1] == 0x29 && close > open {
        close -= 1;
        end -= 1;
    }
    &url[..end]
}

#[derive(Debug, Clone)]
pub struct SafeSequences {
    urls: Vec<JsString>,
    emails: Vec<JsString>,
    tokens: Vec<JsString>,
    ids: HashMap<JsString, usize>,
    nonce: String,
}

impl Default for SafeSequences {
    fn default() -> Self {
        SafeSequences {
            urls: Vec::new(),
            emails: Vec::new(),
            tokens: Vec::new(),
            ids: HashMap::new(),
            nonce: fresh_nonce(),
        }
    }
}

impl SafeSequences {
    pub fn reset(&mut self) {
        self.nonce = fresh_nonce();
        self.reset_frame();
    }

    pub fn reset_frame(&mut self) {
        self.urls.clear();
        self.emails.clear();
        self.tokens.clear();
        self.ids.clear();
    }

    fn id_of(&mut self, kind: Kind, content: &[u16]) -> Result<usize> {
        let entries = match kind {
            Kind::Url => &mut self.urls,
            Kind::Email => &mut self.emails,
            Kind::Token => &mut self.tokens,
        };
        if entries.len() >= MAX_ENTRIES {
            let (name, reason) = kind.name();
            return Err(capacity_error(
                format!("SafeSequences: too many {name} in input"),
                json!({ "kind": reason, "max": MAX_ENTRIES }),
            ));
        }
        let key = cat!(kind.letter(), content);
        if let Some(&known) = self.ids.get(&key) {
            return Ok(known);
        }
        let id = entries.len();
        entries.push(content.to_vec());
        self.ids.insert(key, id);
        Ok(id)
    }

    fn replace_fallible(
        &mut self,
        re: &Re,
        text: &[u16],
        mut f: impl FnMut(&mut Self, &Caps<'_>) -> Result<JsString>,
    ) -> Result<JsString> {
        let mut error = None;
        let out = re.replace_with(text, |c| {
            if error.is_some() {
                return Vec::new();
            }
            match f(self, c) {
                Ok(s) => s,
                Err(e) => {
                    error = Some(e);
                    Vec::new()
                }
            }
        });
        match error {
            Some(e) => Err(e),
            None => Ok(out),
        }
    }

    pub fn protect(&mut self, text: &[u16]) -> Result<JsString> {
        if !text.iter().any(|&c| matches!(c, 0x2E | 0x3A | 0x40 | 0x2D)) {
            return Ok(text.to_vec());
        }
        let nonce = self.nonce.clone();
        let mut result = self.replace_fallible(&URL_REGEX, text, |this, c| {
            let m = c.whole();
            let url = trim_url_parens(m);
            let tail = &m[url.len()..];
            let id = this.id_of(Kind::Url, url)?;
            Ok(cat!("http://simonenko-xyz/A0SAFE", nonce.as_str(), "NUM", js::num(id), "ID", tail))
        })?;
        if result.contains(&0x40) {
            result = self.replace_fallible(&EMAIL_REGEX, &result, |this, c| {
                let id = this.id_of(Kind::Email, c.whole())?;
                Ok(cat!("A1SAFE", nonce.as_str(), "NUM", js::num(id), "ID@simonenko.xyz"))
            })?;
        }
        if result.contains(&0x2D) {
            result = self.replace_fallible(&UUID_REGEX, &result, |this, c| this.store_token(c.whole()))?;
        }
        if result.contains(&0x3A) {
            result = self.replace_fallible(&IPV6_REGEX, &result, |this, c| this.store_token(c.whole()))?;
        }
        Ok(result)
    }

    fn store_token(&mut self, content: &[u16]) -> Result<JsString> {
        let id = self.id_of(Kind::Token, content)?;
        Ok(cat!("AzSAFE", self.nonce.as_str(), "TOK", js::num(id), "TK"))
    }

    pub fn restore(&self, text: JsString) -> JsString {
        let mut result = text;
        if self.urls.is_empty() && self.emails.is_empty() && self.tokens.is_empty() {
            return result;
        }
        let nonce = js::u(&self.nonce);
        let lookup = |entries: &Vec<JsString>, c: &Caps<'_>| -> Option<JsString> {
            if c.s(1) != nonce.as_slice() {
                return None;
            }
            let id = js::parse_int(c.s(2)).and_then(|n| usize::try_from(n).ok())?;
            entries.get(id).cloned()
        };
        if !self.urls.is_empty() {
            result =
                URL_RESTORE_RE.replace_owned(result, |c| lookup(&self.urls, c).unwrap_or_else(|| c.whole().to_vec()));
            result = URL_RESTORE_NOPROTO_RE.replace_owned(result, |c| match lookup(&self.urls, c) {
                Some(u) => SCHEME_RE.replace_with(&u, |_| Vec::new()),
                None => c.whole().to_vec(),
            });
        }
        if !self.emails.is_empty() {
            result = EMAIL_RESTORE_RE
                .replace_owned(result, |c| lookup(&self.emails, c).unwrap_or_else(|| c.whole().to_vec()));
        }
        if !self.tokens.is_empty() {
            result = TOKEN_RESTORE_RE
                .replace_owned(result, |c| lookup(&self.tokens, c).unwrap_or_else(|| c.whole().to_vec()));
        }
        result
    }

    pub fn restore_regexes(&self) -> &'static [Re] {
        &RESTORE_REGEXES
    }
}
