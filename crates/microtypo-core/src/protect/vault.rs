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
        let id = self.push_entry(content)?;
        self.by_content.insert(self.entries[id].clone(), id);
        self.push_token(id, out);
        Ok(())
    }

    pub fn store_in(&mut self, content: &[u16], by_content: &mut HashMap<Arc<[u16]>, usize>) -> Result<JsString> {
        let mut out = JsString::new();
        if let Some(&existing) = by_content.get(content) {
            self.push_token(existing, &mut out);
            return Ok(out);
        }
        let id = self.push_entry(content)?;
        by_content.insert(self.entries[id].clone(), id);
        self.push_token(id, &mut out);
        Ok(out)
    }

    fn push_entry(&mut self, content: &[u16]) -> Result<usize> {
        if self.entries.len() >= MAX_ENTRIES {
            return Err(capacity_error(
                format!("Vault '{}' exceeded {MAX_ENTRIES} entries — possible runaway input", self.prefix),
                json!({ "vault": self.prefix, "max": MAX_ENTRIES }),
            ));
        }
        self.entries.push(Arc::from(content));
        Ok(self.entries.len() - 1)
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
        self.restore_matches_with(text, None)
    }

    pub fn restore_matches_with(
        &self,
        text: &[u16],
        mapping: Option<&mut crate::shared::provenance::RestoreMap>,
    ) -> Option<JsString> {
        if self.entries.is_empty() {
            return None;
        }
        let entries = &self.entries;
        let replacement = |c: &js::Caps<'_>| {
            let id = js::parse_int(c.s(1)).and_then(|n| usize::try_from(n).ok());
            match id.and_then(|id| entries.get(id)) {
                Some(value) => value.to_vec(),
                None => c.whole().to_vec(),
            }
        };
        match mapping {
            Some(map) => Some(map.replace(text.to_vec(), &self.restore_re, replacement)).filter(|next| next != text),
            None => self.restore_re.replace_matches(text, replacement),
        }
    }

    pub fn restore_fixpoint(vaults: &[&Vault], text: JsString) -> JsString {
        Self::restore_fixpoint_with(vaults, text, None)
    }

    pub fn restore_fixpoint_with(
        vaults: &[&Vault],
        mut text: JsString,
        mut mapping: Option<&mut crate::shared::provenance::RestoreMap>,
    ) -> JsString {
        loop {
            let mut next: Option<JsString> = None;
            for vault in vaults {
                if let Some(n) = vault.restore_matches_with(next.as_deref().unwrap_or(&text), mapping.as_deref_mut()) {
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

#[cfg(test)]
mod tests {
    use super::Vault;
    use crate::js;

    #[test]
    fn vault_general_restore_preserves_single_entry_zero_ids_and_literal_dollars() {
        for prefix in ["B", "E", "F", "H", "I", "N", "W"] {
            for content in ["Амбер", "$&$1$$"] {
                let mut vault = Vault::new(prefix);
                vault.store(&js::u(content)).unwrap();
                for id in ["0", "00", "0000000000000000"] {
                    let token = js::u(&format!("\u{e000}{prefix}{id}\u{e001}"));
                    assert_eq!(vault.restore_all(&token), js::u(content));
                }
                for id in ["1", "10", "0001", "999999999999999999999999999999999999"] {
                    let token = js::u(&format!("\u{e000}{prefix}{id}\u{e001}"));
                    assert_eq!(vault.restore_all(&token), token);
                }
                let foreign = js::u("\u{e000}Foreign0\u{e001}");
                assert_eq!(vault.restore_all(&foreign), foreign);
                vault.store(&js::u("Арден")).unwrap();
                let second = js::u(&format!("\u{e000}{prefix}0001\u{e001}"));
                assert_eq!(vault.restore_all(&second), js::u("Арден"));
            }
        }
    }
}
