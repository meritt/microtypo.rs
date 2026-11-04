pub mod abbr;
pub mod dash;
pub mod date;
pub mod etc;
pub mod nobr;
pub mod number;
pub mod opt_align;
pub mod punctmark;
pub mod quote;
pub mod quote_state;
pub mod registry;
pub mod space;
pub mod symbol;
pub mod text;

use serde_json::{Map, Value};

use crate::errors::Result;
use crate::js::JsString;
use crate::rule_runner::{Ctx, Rule, RuleGroupDef};

pub(crate) fn classes(pairs: &[(&str, &str)]) -> Option<Map<String, Value>> {
    Some(pairs.iter().map(|(k, v)| ((*k).to_owned(), Value::String((*v).to_owned()))).collect())
}

pub(crate) const NOWRAP_CLASSES: &[(&str, &str)] = &[("nowrap", "white-space:nowrap;")];

pub(crate) fn group(title: &str, classes_map: Option<Map<String, Value>>, rules: Vec<Rule>) -> RuleGroupDef {
    RuleGroupDef { name: None, title: Some(title.to_owned()), rules, classes: classes_map, pre_parse: None }
}

pub(crate) fn span(ctx: &mut Ctx<'_>, content: &[u16], class: &str) -> Result<JsString> {
    ctx.tag(content, "span", &[("class", class)])
}

pub(crate) fn nowrap(ctx: &mut Ctx<'_>, content: &[u16]) -> Result<JsString> {
    span(ctx, content, "nowrap")
}
