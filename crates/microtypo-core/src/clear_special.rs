use std::collections::HashMap;
use std::sync::LazyLock;

use crate::js::{JsString, Re, escape, from_code_point, u};
use crate::shared::chars_table::CHARS_TABLE;

struct Table {
    lookup: HashMap<JsString, JsString>,
    scan: Re,
}

static TABLE: LazyLock<Table> = LazyLock::new(|| {
    let mut entries: Vec<(JsString, JsString)> = Vec::new();
    for entry in CHARS_TABLE {
        let canonical = u(entry.canonical);
        for &v in entry.utf8 {
            entries.push((from_code_point(v), canonical.clone()));
        }
        for v in entry.html {
            entries.push((u(v), canonical.clone()));
        }
    }
    entries.retain(|(needle, canonical)| !(needle == canonical && needle.len() == 1));
    entries.sort_by_key(|e| std::cmp::Reverse(e.0.len()));
    let source = entries.iter().map(|(n, _)| escape(n)).collect::<Vec<_>>().join("|");
    let mut lookup = HashMap::new();
    for (needle, canonical) in &entries {
        lookup.insert(needle.clone(), canonical.clone());
    }
    Table { lookup, scan: Re::new(&source, "gu") }
});

pub fn clear_special_chars(text: JsString) -> JsString {
    let t = &*TABLE;
    t.scan.replace_owned(text, |c| t.lookup.get(c.whole()).cloned().unwrap_or_else(|| c.whole().to_vec()))
}
