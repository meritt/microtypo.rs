use std::sync::LazyLock;

use crate::errors::Result;
use crate::js::{Caps, JsString, Re};
use crate::rule_runner::Ctx;
use crate::shared::glyphs::{NBSP, unit};

pub const INDEFINITE_HEAD: &str = "кто|кем|когда|зачем|почему|как|что|чем|где|чего|кого";
pub const INDEFINITE_TAIL: &str = "то|либо|нибудь";

static COLON_PHRASE: LazyLock<Re> =
    LazyLock::new(|| Re::new(&format!(r"(?:{INDEFINITE_HEAD})-?[ \t{NBSP}]+-?(?:{INDEFINITE_TAIL}):"), "iuy"));
static CHAIN_WORD: LazyLock<Re> = LazyLock::new(|| Re::new("[a-zа-яё]+", "iuy"));

fn attached_colon(ctx: &Ctx<'_>, text: &[u16], start: usize) -> Result<usize> {
    let mut at = start;
    let mut words = 0;
    while text.get(at) == Some(&unit(NBSP)) {
        words += 1;
        if words & 1023 == 0 {
            (ctx.check_budget)("nowrap-indefinite-colon")?;
        }
        if let Some(m) = COLON_PHRASE.exec_at(text, at + 1) {
            return Ok(m.end());
        }
        let Some(m) = CHAIN_WORD.exec_at(text, at + 1) else { break };
        at = m.end();
    }
    Ok(start)
}

pub fn nowrap_with_indefinite_colon(
    ctx: &mut Ctx<'_>,
    pattern: &Re,
    text_of: impl Fn(&Caps<'_>) -> JsString,
) -> Result<()> {
    let text = std::mem::take(&mut ctx.text);
    let result = (|| {
        let mut out = Vec::new();
        let mut end = 0;
        let mut at = 0;
        let mut matches = 0;
        while let Some(m) = pattern.exec_at(&text, at) {
            at = m.end();
            matches += 1;
            if matches & 1023 == 0 {
                (ctx.check_budget)("nowrap-indefinite-colon")?;
            }
            let phrase_start = m.index() + m.s(1).len();
            if phrase_start < end {
                continue;
            }
            let after = m.end();
            let suffix_end = attached_colon(ctx, &text, after)?;
            let mut inner = text_of(&m);
            inner.extend_from_slice(&text[after..suffix_end]);
            out.extend_from_slice(&text[end..phrase_start]);
            out.extend(crate::shared::nowrap_context::nowrap(ctx, &inner, &text, phrase_start, suffix_end)?);
            end = suffix_end;
        }
        if matches > 0 {
            out.extend_from_slice(&text[end..]);
            Ok(Some(out))
        } else {
            Ok(None)
        }
    })();
    ctx.text = match result? {
        Some(out) => out,
        None => text,
    };
    Ok(())
}
