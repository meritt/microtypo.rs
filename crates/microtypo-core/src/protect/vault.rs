use std::collections::HashMap;
use std::sync::Arc;

use serde_json::json;

use crate::errors::{MAX_ENTRIES, Result, capacity_error};
use crate::js::{self, JsString, Piece, Re};
use crate::protect::placeholders::{CLOSE, OPEN, placeholder_regex};

#[derive(Debug, Clone)]
pub struct Vault {
    entries: Vec<Arc<[u16]>>,
    by_content: HashMap<Arc<[u16]>, usize>,
    prefix: &'static str,
    restore_re: Re,
}

impl Vault {
    pub fn new(prefix: &'static str) -> Self {
        Vault { entries: Vec::new(), by_content: HashMap::new(), prefix, restore_re: placeholder_regex(prefix, "g") }
    }

    pub fn prefix(&self) -> &'static str {
        self.prefix
    }

    fn push_token(&self, id: usize, out: &mut JsString) {
        out.reserve(self.prefix.len() + 8);
        OPEN.push_to(out);
        self.prefix.push_to(out);
        js::Num(id).push_to(out);
        CLOSE.push_to(out);
    }

    pub fn store(&mut self, content: &[u16]) -> Result<JsString> {
        let mut token = JsString::new();
        self.store_into(content, &mut token)?;
        Ok(token)
    }

    pub fn store_into(&mut self, content: &[u16], out: &mut JsString) -> Result<()> {
        if let Some(&existing) = self.by_content.get(content) {
            self.push_token(existing, out);
            return Ok(());
        }
        if self.entries.len() >= MAX_ENTRIES {
            return Err(capacity_error(
                format!("Vault '{}' exceeded {MAX_ENTRIES} entries — possible runaway input", self.prefix),
                json!({ "vault": self.prefix, "max": MAX_ENTRIES }),
            ));
        }
        let id = self.entries.len();
        let entry: Arc<[u16]> = Arc::from(content);
        self.entries.push(entry.clone());
        self.by_content.insert(entry, id);
        self.push_token(id, out);
        Ok(())
    }

    pub fn retrieve(&self, id: usize) -> Option<&[u16]> {
        self.entries.get(id).map(|e| &e[..])
    }

    pub fn reset(&mut self) {
        self.entries.clear();
        self.by_content.clear();
    }

    pub fn size(&self) -> usize {
        self.entries.len()
    }

    pub fn restore_all(&self, text: &[u16]) -> JsString {
        let mut text = text.to_vec();
        self.restore_in(&mut text);
        text
    }

    pub fn restore_in(&self, text: &mut JsString) {
        if let Some(next) = self.restore_matches(text) {
            *text = next;
        }
    }

    pub fn restore_matches(&self, text: &[u16]) -> Option<JsString> {
        if self.entries.is_empty() {
            return None;
        }
        let entries = &self.entries;
        self.restore_re.replace_matches(text, |c| {
            let id = js::parse_int(c.s(1)).and_then(|n| usize::try_from(n).ok());
            match id.and_then(|id| entries.get(id)) {
                Some(value) => value.to_vec(),
                None => c.whole().to_vec(),
            }
        })
    }

    pub fn restore_fixpoint(vaults: &[&Vault], mut text: JsString) -> JsString {
        loop {
            let mut next: Option<JsString> = None;
            for vault in vaults {
                if let Some(n) = vault.restore_matches(next.as_deref().unwrap_or(&text)) {
                    next = Some(n);
                }
            }
            match next {
                Some(n) if n != text => text = n,
                _ => return text,
            }
        }
    }
}
