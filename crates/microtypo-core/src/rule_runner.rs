use std::collections::HashMap;
use std::fmt;
use std::sync::Arc;

use serde_json::{Map, Value};

use crate::errors::Result;
use crate::js::{self, Caps, JsString, Re};
use crate::protect::blocks::SafeBlocks;
use crate::protect::forgery::TokenBudget;
use crate::protect::placeholders::{drop_paragraphs, strip_reserved_pua};
use crate::protect::sequences::SafeSequences;
use crate::protect::tags::{Layout, MadeTag, NameMatcher, SafeTags, TagSpec, assert_tag_name, object_key_order};
use crate::shared::glyphs::NBSP;
use crate::shared::strings::cycle;

const MAX_CYCLE_ITER: usize = 100;

pub type ReplaceFn = Arc<dyn Fn(&Caps<'_>, &mut Ctx<'_>) -> Result<JsString> + Send + Sync>;
pub type HandlerFn = Arc<dyn Fn(&mut Ctx<'_>) -> Result<Option<JsString>> + Send + Sync>;
pub type PreParseFn = Arc<dyn Fn(&mut Ctx<'_>) -> Result<()> + Send + Sync>;
pub type CycleHook = Arc<dyn Fn(&str, usize) + Send + Sync>;

#[derive(Clone)]
pub enum Replacement {
    Template(JsString),
    Func(ReplaceFn),
}

impl Replacement {
    pub fn tpl(s: &str) -> Self {
        Replacement::Template(js::u(s))
    }

    pub fn func(f: impl Fn(&Caps<'_>, &mut Ctx<'_>) -> Result<JsString> + Send + Sync + 'static) -> Self {
        Replacement::Func(Arc::new(f))
    }
}

#[derive(Clone)]
pub enum RuleBody {
    Patterns(Vec<(Re, Replacement)>),
    Handler(HandlerFn),
}

#[derive(Clone)]
pub struct Rule {
    pub id: String,
    pub enabled: bool,
    pub description: String,
    pub html_only: bool,
    pub cycled: bool,
    pub body: RuleBody,
}

impl Rule {
    pub fn pattern(id: &str, re: Re, replacement: Replacement) -> Self {
        Rule::patterns(id, vec![(re, replacement)])
    }

    pub fn patterns(id: &str, patterns: Vec<(Re, Replacement)>) -> Self {
        Rule {
            id: id.to_owned(),
            enabled: true,
            description: String::new(),
            html_only: false,
            cycled: false,
            body: RuleBody::Patterns(patterns),
        }
    }

    pub fn handler(id: &str, f: impl Fn(&mut Ctx<'_>) -> Result<Option<JsString>> + Send + Sync + 'static) -> Self {
        Rule {
            id: id.to_owned(),
            enabled: true,
            description: String::new(),
            html_only: false,
            cycled: false,
            body: RuleBody::Handler(Arc::new(f)),
        }
    }

    #[must_use]
    pub fn describe(mut self, d: &str) -> Self {
        d.clone_into(&mut self.description);
        self
    }

    #[must_use]
    pub fn off(mut self) -> Self {
        self.enabled = false;
        self
    }

    #[must_use]
    pub fn html_only(mut self) -> Self {
        self.html_only = true;
        self
    }

    #[must_use]
    pub fn cycled(mut self) -> Self {
        self.cycled = true;
        self
    }
}

#[derive(Clone, Default)]
pub struct RuleGroupDef {
    pub name: Option<String>,
    pub title: Option<String>,
    pub rules: Vec<Rule>,
    pub classes: Option<Map<String, Value>>,
    pub pre_parse: Option<PreParseFn>,
}

#[derive(Debug, Clone)]
pub struct GroupView {
    pub name: String,
    pub title: String,
    pub settings: Map<String, Value>,
    pub classes: Option<Map<String, Value>>,
}

pub struct CompiledRule {
    pub id: String,
    pub default_enabled: bool,
    pub description: String,
    pub html_only: bool,
    cycled: bool,
    templates_only: bool,
    body: RuleBody,
}

#[derive(Clone)]
pub struct CompiledGroup {
    pub name: String,
    pub budget_label: String,
    pub title: String,
    pub rules: Arc<[CompiledRule]>,
    pub enabled_override: HashMap<String, bool>,
    pub view: Arc<GroupView>,
    pub pre_parse: Option<PreParseFn>,
    pub builtin: bool,
}

impl CompiledGroup {
    pub fn set_setting(&mut self, key: &str, value: Value) {
        Arc::make_mut(&mut self.view).settings.insert(key.to_owned(), value);
    }
}

pub fn compile_rule_group(def: &RuleGroupDef, name: &str) -> Result<CompiledGroup> {
    let mut rules = Vec::with_capacity(def.rules.len());
    for rule in &def.rules {
        if rule.id.is_empty() {
            return Err(crate::errors::Error::type_error(format!(
                "Rule group '{name}': rule.id must be a non-empty string"
            )));
        }
        let body = match &rule.body {
            RuleBody::Patterns(p) => RuleBody::Patterns(
                p.iter()
                    .map(|(re, r)| {
                        (if re.global { re.clone() } else { re.with_flags(&format!("{}g", re.flags())) }, r.clone())
                    })
                    .collect(),
            ),
            h @ RuleBody::Handler(_) => h.clone(),
        };
        let templates_only =
            matches!(&body, RuleBody::Patterns(p) if p.iter().all(|(_, r)| matches!(r, Replacement::Template(_))));
        rules.push(CompiledRule {
            templates_only,
            id: rule.id.clone(),
            default_enabled: rule.enabled,
            description: rule.description.clone(),
            html_only: rule.html_only,
            cycled: rule.cycled,
            body,
        });
    }
    let title = def.title.clone().filter(|t| !t.is_empty()).unwrap_or_else(|| name.to_owned());
    Ok(CompiledGroup {
        name: name.to_owned(),
        budget_label: format!("group:{name}"),
        title: title.clone(),
        rules: rules.into(),
        enabled_override: HashMap::new(),
        view: Arc::new(GroupView { name: name.to_owned(), title, settings: Map::new(), classes: def.classes.clone() }),
        pre_parse: def.pre_parse.clone(),
        builtin: false,
    })
}

pub struct Ctx<'a> {
    pub text: JsString,
    pub group: Option<Arc<GroupView>>,
    pub html: bool,
    pub layout: Layout,
    pub engine_settings: &'a Map<String, Value>,
    pub check_budget: &'a dyn Fn(&str) -> Result<()>,
    pub on_cycle_limit: Option<CycleHook>,
    pub(crate) safe_tags: &'a mut SafeTags,
    pub(crate) safe_blocks: &'a SafeBlocks,
    pub(crate) safe_sequences: &'a SafeSequences,
    pub(crate) prefix: &'a str,
    pub(crate) budget: Option<TokenBudget>,
}

impl Ctx<'_> {
    fn credit(&mut self, token: &[u16]) {
        if let Some(b) = self.budget.as_mut() {
            b.mint(token);
        }
    }

    fn guard(&mut self, value: &[u16]) -> JsString {
        match self.budget.as_mut() {
            None => value.to_vec(),
            Some(b) => b.scrub(value),
        }
    }

    fn record(&mut self, made: MadeTag) -> JsString {
        for token in &made.tokens {
            self.credit(token);
        }
        made.html
    }

    fn restore_protected(&self, text: JsString) -> JsString {
        let result = self.safe_tags.restore_iblocks(text);
        let result = self.safe_tags.restore(result);
        let result = self.safe_blocks.restore(result);
        self.safe_sequences.restore(result)
    }

    fn settle_attributes(&mut self, attrs: Vec<(String, JsString)>) -> Vec<(String, JsString)> {
        attrs
            .into_iter()
            .map(|(name, value)| {
                let guarded = match self.budget.as_mut() {
                    None => value,
                    Some(b) => b.scrub(&value),
                };
                let restored = self.restore_protected(guarded);
                (name, strip_reserved_pua(drop_paragraphs(restored)))
            })
            .collect()
    }

    pub fn iblock(&mut self, s: &[u16]) -> Result<JsString> {
        let guarded = self.guard(s);
        let ph = self.safe_tags.iblock(&guarded)?;
        self.credit(&ph);
        Ok(ph)
    }

    pub fn find_by_tag_name(&mut self, matcher: &NameMatcher<'_>) -> Vec<(char, usize)> {
        self.safe_tags.find_by_tag_name(matcher)
    }

    pub fn tag(&mut self, content: &[u16], tag: &str, attributes: &[(&str, &str)]) -> Result<JsString> {
        let attributes: Vec<(String, JsString)> = attributes.iter().map(|(k, v)| ((*k).to_owned(), js::u(v))).collect();
        self.tag_with(content, tag, attributes)
    }

    pub fn tag_with(
        &mut self,
        content: &[u16],
        tag: &str,
        mut attributes: Vec<(String, JsString)>,
    ) -> Result<JsString> {
        static NOWRAP: js::Lit = crate::lit!("nowrap");
        object_key_order(&mut attributes, |e| &e.0);
        assert_tag_name("tag", tag)?;
        for (name, _) in &attributes {
            if name != "class" && name != "__style" {
                assert_tag_name("attribute", name)?;
            }
        }
        let class_at = attributes.iter().position(|(k, _)| k == "class");
        let nowrap = class_at.is_some_and(|i| attributes[i].1 == *NOWRAP);
        if !self.html {
            return Ok(if nowrap { js::replace_all_lit(content, &[0x20], &[NBSP as u16]) } else { content.to_vec() });
        }
        let mut attrs = attributes;
        if let Some(at) = class_at.filter(|&i| !attrs[i].1.is_empty()) {
            if nowrap && self.engine_settings.get("nowrap").and_then(Value::as_str) == Some("nobr") {
                let made =
                    self.safe_tags.make_tag(TagSpec { content, tag: "nobr", attributes: &[], layout: self.layout })?;
                return Ok(self.record(made));
            }
            let style = self
                .group
                .as_ref()
                .and_then(|g| g.classes.as_ref())
                .and_then(|c| c.get(&js::lossy(&attrs[at].1)))
                .and_then(|v| match v {
                    Value::String(s) if !s.is_empty() => Some(js::u(s)),
                    Value::Null | Value::Bool(false) | Value::String(_) => None,
                    Value::Number(n) if n.as_f64() == Some(0.0) => None,
                    other => Some(js::u(&other.to_string())),
                });
            if let Some(style) = style {
                match attrs.iter_mut().find(|(k, _)| k == "__style") {
                    Some(slot) => slot.1 = style,
                    None => attrs.push(("__style".to_owned(), style)),
                }
            }
            if !self.prefix.is_empty() {
                attrs[at].1 = crate::cat!(self.prefix, attrs[at].1);
            }
        }
        let settled = self.settle_attributes(attrs);
        let made = self.safe_tags.make_tag(TagSpec { content, tag, attributes: &settled, layout: self.layout })?;
        Ok(self.record(made))
    }
}

fn apply_rule(rule: &CompiledRule, text: &[u16], ctx: &mut Ctx<'_>) -> Result<Option<JsString>> {
    match &rule.body {
        RuleBody::Handler(handler) => {
            if ctx.text != text {
                ctx.text = text.to_vec();
            }
            let r = handler(ctx)?;
            Ok(Some(r.unwrap_or_else(|| std::mem::take(&mut ctx.text))))
        }
        RuleBody::Patterns(patterns) => {
            let mut result: Option<JsString> = None;
            for (re, repl) in patterns {
                let current = result.as_deref().unwrap_or(text);
                let next = match repl {
                    Replacement::Template(t) => re.replace_tpl_matches(current, t),
                    Replacement::Func(f) => re.try_replace_matches(current, |c| f(c, ctx))?,
                };
                if next.is_some() {
                    result = next;
                }
            }
            Ok(result)
        }
    }
}

fn apply_templates(patterns: &[(Re, Replacement)], text: &[u16]) -> Option<JsString> {
    let mut result: Option<JsString> = None;
    for (re, repl) in patterns {
        let Replacement::Template(t) = repl else { unreachable!("a rule of templates alone") };
        if let Some(next) = re.replace_tpl_matches(result.as_deref().unwrap_or(text), t) {
            result = Some(next);
        }
    }
    result
}

fn run_rule(rule: &CompiledRule, text: &[u16], ctx: &mut Ctx<'_>) -> Result<Option<JsString>> {
    if !rule.cycled {
        return apply_rule(rule, text, ctx);
    }
    let hook = ctx.on_cycle_limit.clone();
    let check_budget = ctx.check_budget;
    let id = rule.id.clone();
    cycle(
        text,
        |t| Ok(apply_rule(rule, t, ctx)?.unwrap_or_else(|| t.to_vec())),
        MAX_CYCLE_ITER,
        &mut |n| {
            if let Some(h) = &hook {
                h(&id, n);
            }
        },
        check_budget,
    )
    .map(Some)
}

fn is_rule_enabled(group: &CompiledGroup, rule: &CompiledRule) -> bool {
    group.enabled_override.get(&rule.id).copied().unwrap_or(rule.default_enabled)
}

pub fn run_rule_group(group: &CompiledGroup, text: JsString, ctx: &mut Ctx<'_>) -> Result<JsString> {
    ctx.text = text;
    ctx.group = Some(group.view.clone());
    if let Some(pre) = &group.pre_parse {
        pre(ctx)?;
    }
    for rule in group.rules.iter() {
        if !is_rule_enabled(group, rule) || (rule.html_only && !ctx.html) {
            continue;
        }
        if rule.templates_only
            && !rule.cycled
            && let RuleBody::Patterns(patterns) = &rule.body
        {
            if let Some(next) = apply_templates(patterns, &ctx.text) {
                ctx.text = next;
            }
            continue;
        }
        if !rule.cycled {
            match &rule.body {
                RuleBody::Handler(handler) => {
                    if let Some(next) = handler(ctx)? {
                        ctx.text = next;
                    }
                }
                RuleBody::Patterns(_) => {
                    let input = std::mem::take(&mut ctx.text);
                    ctx.text = apply_rule(rule, &input, ctx)?.unwrap_or(input);
                }
            }
            continue;
        }
        let input = ctx.text.clone();
        ctx.text = run_rule(rule, &input, ctx)?.unwrap_or(input);
    }
    Ok(std::mem::take(&mut ctx.text))
}

pub fn run_group(group: &CompiledGroup, text: JsString, ctx: &mut Ctx<'_>) -> Result<JsString> {
    if group.builtin {
        return run_rule_group(group, text, ctx);
    }
    ctx.budget = Some(TokenBudget::new(text.clone(), ctx.safe_sequences.restore_regexes().to_vec()));
    let outcome = run_rule_group(group, text.clone(), ctx);
    let mut budget = ctx.budget.take().expect("budget opened above");
    let after = outcome?;
    Ok(if after == text && budget.untouched() { after } else { budget.scrub(&after) })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuleGroupInfo {
    pub name: String,
    pub title: String,
    pub builtin: bool,
    pub rules: Vec<RuleInfo>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuleInfo {
    pub id: String,
    pub default_enabled: bool,
}

pub fn describe_rule_group(group: &CompiledGroup) -> RuleGroupInfo {
    RuleGroupInfo {
        name: group.name.clone(),
        title: group.title.clone(),
        builtin: group.builtin,
        rules: group.rules.iter().map(|r| RuleInfo { id: r.id.clone(), default_enabled: r.default_enabled }).collect(),
    }
}

impl fmt::Debug for Replacement {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Replacement::Template(t) => f.debug_tuple("Template").field(&String::from_utf16_lossy(t)).finish(),
            Replacement::Func(_) => f.write_str("Func"),
        }
    }
}

impl fmt::Debug for RuleBody {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RuleBody::Patterns(p) => f.debug_tuple("Patterns").field(&p.len()).finish(),
            RuleBody::Handler(_) => f.write_str("Handler"),
        }
    }
}

impl fmt::Debug for Rule {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Rule").field("id", &self.id).field("body", &self.body).finish_non_exhaustive()
    }
}

impl fmt::Debug for RuleGroupDef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RuleGroupDef").field("name", &self.name).field("rules", &self.rules).finish_non_exhaustive()
    }
}

impl fmt::Debug for CompiledRule {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CompiledRule").field("id", &self.id).finish_non_exhaustive()
    }
}

impl fmt::Debug for CompiledGroup {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CompiledGroup").field("name", &self.name).field("rules", &self.rules).finish_non_exhaustive()
    }
}

impl fmt::Debug for Ctx<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Ctx").field("html", &self.html).finish_non_exhaustive()
    }
}
