use std::collections::HashMap;
use std::fmt;
use std::sync::{Arc, LazyLock};

use serde_json::{Map, Value};

use crate::errors::Result;
use crate::inline_lift::{self, InlineCx, LiftRule};
use crate::js::{self, Caps, JsString, Re};
use crate::protect::blocks::SafeBlocks;
use crate::protect::forgery::TokenBudget;
use crate::protect::placeholders::{drop_paragraphs, strip_reserved_pua};
use crate::protect::sequences::SafeSequences;
use crate::protect::tags::{Layout, MadeTag, NameMatcher, SafeTags, TagSpec, assert_tag_name, object_key_order};
use crate::rules::quote::{AuthoredClosers, QuoteSession};
use crate::rules::quote_state::{EdgeView, QuoteTags};
use crate::rules::text::MarkdownSource;
use crate::shared::glyphs::NBSP;
use crate::shared::nowrap_context::{NowrapCoverage, NowrapFrame, NowrapOpen};
use crate::shared::provenance::{MapEdit, Projection, RestoreMap, SourceMap};
use crate::shared::rule_features::{Features, Requires, allows};
use crate::shared::rule_trace::{Attempt, RuleTrace, TraceKind};
use crate::shared::strings::cycle;

const MAX_CYCLE_ITER: usize = 100;

/// A replacement function: given a match and the rule context, returns the text that replaces it.
pub type ReplaceFn = Arc<dyn Fn(&Caps<'_>, &mut Ctx<'_>) -> Result<JsString> + Send + Sync>;
/// A rule handler: reads [`Ctx::text`] and returns the new text, or `None` to keep `ctx.text`.
pub type HandlerFn = Arc<dyn Fn(&mut Ctx<'_>) -> Result<Option<JsString>> + Send + Sync>;
/// A hook that runs on the context before the rules of its group.
pub type PreParseFn = Arc<dyn Fn(&mut Ctx<'_>) -> Result<()> + Send + Sync>;
/// Called with the rule id and the iteration count when a cycled rule stops at its limit.
pub type CycleHook = Arc<dyn Fn(&[u16], usize) + Send + Sync>;

/// What replaces a match of a rule pattern.
#[derive(Clone)]
pub enum Replacement {
    /// A template with the `$` patterns of `String.prototype.replace`.
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

/// What a rule does: replace the matches of its patterns in order, or run a handler.
#[derive(Clone)]
pub enum RuleBody {
    /// A pattern without `g` gets it on registration.
    Patterns(Vec<(Re, Replacement)>),
    Handler(HandlerFn),
}

/// A rule of a [`RuleGroupDef`], as an entry of the reference group's `rules`.
#[derive(Clone)]
pub struct Rule {
    /// `id`: what `rules` switches select within the group; an empty one fails registration with `TypeError`.
    pub id: String,
    /// `enabled`: whether the rule runs unless switched; `true` by default.
    pub enabled: bool,
    pub description: String,
    /// `htmlOnly`: run only under `html: true`.
    pub html_only: bool,
    /// `cycled`: repeat until the text stops changing, at most 100 times.
    pub cycled: bool,
    /// `pattern` and `replacement`, or `handler`.
    pub body: RuleBody,
    pub(crate) requires: Requires,
    pub(crate) lift: LiftFlags,
}

#[derive(Clone, Copy, Default, PartialEq, Eq)]
pub(crate) enum SpacedYears {
    #[default]
    Off,
    Ranges,
    Gaps,
}

#[derive(Clone, Copy, Default)]
pub(crate) struct LiftFlags {
    pub(crate) changes_inline_boundary: bool,
    pub(crate) project_edits: bool,
    pub(crate) project_spaced_years: SpacedYears,
}

impl Rule {
    /// A rule replacing the matches of one pattern.
    pub fn pattern(id: &str, re: Re, replacement: Replacement) -> Self {
        Rule::patterns(id, vec![(re, replacement)])
    }

    /// A rule replacing the matches of each pattern in turn.
    pub fn patterns(id: &str, patterns: Vec<(Re, Replacement)>) -> Self {
        Rule {
            id: id.to_owned(),
            enabled: true,
            description: String::new(),
            html_only: false,
            cycled: false,
            body: RuleBody::Patterns(patterns),
            requires: Requires::default(),
            lift: LiftFlags::default(),
        }
    }

    /// A rule running a handler over the whole text.
    pub fn handler(id: &str, f: impl Fn(&mut Ctx<'_>) -> Result<Option<JsString>> + Send + Sync + 'static) -> Self {
        Rule {
            id: id.to_owned(),
            enabled: true,
            description: String::new(),
            html_only: false,
            cycled: false,
            body: RuleBody::Handler(Arc::new(f)),
            requires: Requires::default(),
            lift: LiftFlags::default(),
        }
    }

    #[must_use]
    pub fn describe(mut self, d: &str) -> Self {
        d.clone_into(&mut self.description);
        self
    }

    /// Makes the rule off unless switched on.
    #[must_use]
    pub fn off(mut self) -> Self {
        self.enabled = false;
        self
    }

    /// Makes the rule run only under `html: true`.
    #[must_use]
    pub fn html_only(mut self) -> Self {
        self.html_only = true;
        self
    }

    /// Makes the rule repeat until the text stops changing.
    #[must_use]
    pub fn cycled(mut self) -> Self {
        self.cycled = true;
        self
    }

    #[must_use]
    pub(crate) fn requires(mut self, all: u32) -> Self {
        self.requires.all = all;
        self
    }

    #[must_use]
    pub(crate) fn requires_any(mut self, any: u32) -> Self {
        self.requires.any = any;
        self
    }

    #[must_use]
    pub(crate) fn requires_pattern(mut self, pattern: Re) -> Self {
        self.requires.pattern = Some(pattern);
        self
    }

    #[must_use]
    pub(crate) fn changes_inline_boundary(mut self) -> Self {
        self.lift.changes_inline_boundary = true;
        self
    }

    #[must_use]
    pub(crate) fn project_edits(mut self) -> Self {
        self.lift.project_edits = true;
        self
    }

    #[must_use]
    pub(crate) fn project_spaced_years(mut self, mode: SpacedYears) -> Self {
        self.lift.project_spaced_years = mode;
        self
    }

    #[doc(hidden)]
    pub fn requirements(&self) -> (u32, u32, Option<&Re>) {
        (self.requires.all, self.requires.any, self.requires.pattern.as_ref())
    }
}

/// A rule group for [`MicroTypo::register_rule_group`](crate::MicroTypo::register_rule_group), as the group object of the reference.
#[derive(Clone, Default)]
pub struct RuleGroupDef {
    /// `name`: the group name, unless registration names it.
    pub name: Option<String>,
    /// `title`: the human name; also the group name when there is no other.
    pub title: Option<String>,
    /// `rules`, run in order.
    pub rules: Vec<Rule>,
    /// `classes`: the inline style [`Ctx::tag`] writes for each class name.
    pub classes: Option<Map<String, Value>>,
    pub pre_parse: Option<PreParseFn>,
}

#[derive(Debug, Clone)]
pub struct GroupView {
    pub settings: Map<String, Value>,
    pub classes: Option<Map<String, Value>>,
}

#[derive(Clone)]
pub struct CompiledRule {
    pub id: JsString,
    pub default_enabled: bool,
    pub html_only: bool,
    cycled: bool,
    templates_only: bool,
    body: RuleBody,
    requires: Requires,
    lift: LiftFlags,
}

#[derive(Clone)]
pub struct CompiledGroup {
    pub name: JsString,
    pub budget_label: String,
    pub title: JsString,
    pub rules: Arc<[CompiledRule]>,
    pub enabled_override: HashMap<JsString, bool>,
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
            id: js::u(&rule.id),
            default_enabled: rule.enabled,
            html_only: rule.html_only,
            cycled: rule.cycled,
            body,
            requires: rule.requires.clone(),
            lift: rule.lift,
        });
    }
    Ok(CompiledGroup {
        name: js::u(name),
        budget_label: format!("group:{name}"),
        title: js::u(def.title.as_deref().filter(|t| !t.is_empty()).unwrap_or(name)),
        rules: rules.into(),
        enabled_override: HashMap::new(),
        view: Arc::new(GroupView { settings: Map::new(), classes: def.classes.clone() }),
        pre_parse: def.pre_parse.clone(),
        builtin: false,
    })
}

/// What a rule sees while it runs, as `ctx` of the reference.
pub struct Ctx<'a> {
    pub text: JsString,
    pub(crate) group: Option<Arc<GroupView>>,
    /// Whether the engine runs under `html: true`.
    pub html: bool,
    pub(crate) layout: Layout,
    pub(crate) engine_settings: &'a Map<String, Value>,
    pub(crate) check_budget: &'a dyn Fn(&str) -> Result<()>,
    pub(crate) on_cycle_limit: Option<CycleHook>,
    pub(crate) safe_tags: &'a mut SafeTags,
    pub(crate) safe_blocks: &'a SafeBlocks,
    pub(crate) safe_sequences: &'a mut SafeSequences,
    pub(crate) prefix: &'a str,
    pub(crate) budget: Option<TokenBudget>,
    pub(crate) markdown: Option<MarkdownSource>,
    pub(crate) authored_closers: Option<AuthoredClosers>,
    pub(crate) inline: Option<InlineCx>,
    pub(crate) features: Option<Features>,
    pub(crate) trace: Option<RuleTrace>,
    pub(crate) trace_projection: Option<Projection>,
    pub(crate) trace_escaped: Option<Arc<crate::protect::reserved::Escaped>>,
    pub(crate) bound: Bindings,
}

#[derive(Default)]
pub(crate) struct Bindings {
    pub(crate) periods: bool,
    pub(crate) punctuation: bool,
    pub(crate) space_collapse: bool,
    pub(crate) trims_line_end: bool,
    pub(crate) initials_dots: bool,
    pub(crate) quote_tags: bool,
    pub(crate) inline_names: bool,
    pub(crate) line_blocks: bool,
    pub(crate) inline_padding: crate::rules::quote_state::InlinePadding,
    pub(crate) breaks: Option<std::collections::HashSet<JsString>>,
    pub(crate) edges: Option<crate::rules::text::EdgeReader>,
    pub(crate) quote: crate::rules::quote::QuoteSession,
    pub(crate) nowrap_frame: Option<NowrapFrame>,
    pub(crate) nowrap_cache: Option<NowrapCoverage>,
    pub(crate) nowrap_open: Option<NowrapOpen>,
}

pub(crate) fn restore_protected_with(
    tags: &SafeTags,
    blocks: &SafeBlocks,
    sequences: &SafeSequences,
    text: JsString,
    href: bool,
) -> JsString {
    let result = tags.restore_iblocks(text);
    let result = tags.restore(result);
    let result = blocks.restore(result);
    if href { sequences.restore_href(result) } else { sequences.restore(result) }
}

impl Ctx<'_> {
    pub(crate) fn names(&mut self) -> Option<&mut SafeTags> {
        if self.bound.inline_names { Some(&mut *self.safe_tags) } else { None }
    }

    pub(crate) fn reference_names(&mut self) -> Option<&mut SafeTags> {
        Some(&mut *self.safe_tags)
    }

    pub(crate) fn forget_text_identity(&mut self) {
        if let Some(frame) = self.bound.nowrap_frame.as_mut() {
            frame.forget();
        }
        if let Some(cache) = self.bound.nowrap_cache.as_mut() {
            cache.same.forget();
        }
        self.bound.quote.forget_text_identity();
    }

    fn bind_boundary_owner(&mut self, group: &CompiledGroup) {
        self.bound.periods = is_rule_on(group, "space_after_period");
        self.bound.punctuation = is_rule_on(group, "space_after_punctuation");
    }

    fn render_trace(&self, text: JsString, mapping: &mut RestoreMap) -> JsString {
        let text = crate::protect::placeholders::restore_paragraphs_with(text, Some(mapping));
        let text = self.safe_tags.restore_iblocks_with(text, mapping);
        let text = self.safe_tags.restore_with(text, Some(mapping));
        let text = self.safe_blocks.restore_with(text, mapping);
        let text = self.safe_sequences.restore_with(text, mapping);
        match &self.trace_escaped {
            Some(escaped) => escaped.unescape_with(text, mapping),
            None => text,
        }
    }

    fn represented(
        &self,
        map: Arc<SourceMap>,
        text: &[u16],
        source: Option<&Projection>,
        target: Option<&mut Projection>,
    ) -> Result<Arc<SourceMap>> {
        crate::shared::provenance::represented_map(
            map,
            text,
            source,
            target,
            |text, mapping| self.render_trace(text, mapping),
            self.check_budget,
        )
    }

    fn begin_trace(&mut self, group: &[u16], id: &[u16], kind: TraceKind, text: &[u16]) -> Result<()> {
        let Some(mut trace) = self.trace.take() else { return Ok(()) };
        trace.rule = [group, &[0x2e], id].concat();
        trace.kind = kind;
        trace.accepted.clear();
        trace.pending = None;
        trace.frame = self.represented(trace.frame, text, None, None)?;
        self.trace = Some(trace);
        Ok(())
    }

    fn open_trace(&mut self, text: &[u16]) -> Result<()> {
        let Some(mut trace) = self.trace.take() else { return Ok(()) };
        let mut projection = self.trace_projection.take();
        let map = if let Some(map) = trace.retry_map.take() {
            map
        } else {
            let previous = trace.accepted.last();
            self.represented(
                previous.map_or_else(|| trace.frame.clone(), |event| event.map.clone()),
                text,
                previous.and_then(|event| event.projection.as_ref()),
                projection.as_mut(),
            )?
        };
        if projection.is_none() {
            projection = trace.retry_projection.take();
        } else {
            trace.retry_projection = None;
        }
        trace.pending = Some(Attempt { input_map: map.clone(), map, projection, count: 0, ranges: Vec::new() });
        self.trace = Some(trace);
        Ok(())
    }

    pub(crate) fn accept_trace(&mut self, mut projection: Option<Projection>) -> Result<()> {
        let Some(mut trace) = self.trace.take() else { return Ok(()) };
        if let Some(mut pending) = trace.pending.take() {
            if let Some(target) = projection.as_mut() {
                if let Some(previous) = &pending.projection
                    && Arc::ptr_eq(&previous.tokens, &target.tokens)
                {
                    for (edit, previous) in target.edits.iter_mut().zip(&previous.edits) {
                        edit.source_map.clone_from(&previous.source_map);
                    }
                }
                if pending.map.text != target.text {
                    pending.map = self.represented(pending.map, &target.text.clone(), None, Some(target))?;
                }
            }
            pending.projection = projection;
            trace.accepted.push(pending);
        }
        self.trace = Some(trace);
        Ok(())
    }

    fn finish_trace(&mut self, text: &[u16]) -> Result<()> {
        let Some(mut trace) = self.trace.take() else { return Ok(()) };
        trace.publish();
        let previous = trace.accepted.last();
        trace.frame = self.represented(
            previous.map_or_else(|| trace.frame.clone(), |event| event.map.clone()),
            text,
            previous.and_then(|event| event.projection.as_ref()),
            None,
        )?;
        trace.pending = None;
        self.trace = Some(trace);
        Ok(())
    }

    pub(crate) fn discard_trace(&mut self) {
        if let Some(trace) = self.trace.as_mut() {
            trace.discard();
        }
        self.trace_projection = None;
    }

    fn trace_replace(&mut self, text: &[u16], re: &Re, replacement: &Replacement) -> Result<Option<JsString>> {
        let mut trace = self.trace.take().expect("active trace");
        let pending = trace.pending.as_mut().expect("open attempt");
        let map = pending.map.fork(text, self.check_budget)?;
        let mut edits = Vec::new();
        let output = re.try_replace_matches(text, |m| {
            let value = match replacement {
                Replacement::Template(template) => crate::js::regex::substitute(m, template),
                Replacement::Func(function) => function(m, self)?,
            };
            if value != m.whole() {
                pending.count += 1;
                pending.ranges.push(map.range(m.index(), m.end(), self.check_budget)?);
                edits.push(MapEdit::from(crate::Edit { start: m.index(), end: m.end(), text: value.clone() }));
            }
            Ok::<_, crate::errors::Error>(value)
        })?;
        pending.map = map.edited(output.as_deref().unwrap_or(text), &edits, self.check_budget)?;
        self.trace = Some(trace);
        Ok(output)
    }

    fn trace_handler(&mut self, text: &[u16], handler: &HandlerFn) -> Result<Option<JsString>> {
        let mut trace = self.trace.take().expect("active trace");
        let pending = trace.pending.as_mut().expect("open attempt");
        let map = pending.map.fork(text, self.check_budget)?;
        if self.text != text {
            self.text = text.to_vec();
        }
        self.forget_text_identity();
        let output = handler(self)?.unwrap_or_else(|| std::mem::take(&mut self.text));
        if output != text {
            let edits = crate::shared::diff::replacement_edits(text, &output, self.check_budget)?;
            pending.count += 1;
            for edit in &edits {
                pending.ranges.push(map.range(edit.start, edit.end, self.check_budget)?);
            }
            pending.map =
                map.edited(&output, &edits.into_iter().map(MapEdit::from).collect::<Vec<_>>(), self.check_budget)?;
        }
        self.trace = Some(trace);
        Ok(Some(output))
    }

    fn note(&mut self, made: &MadeTag, nowrap: bool) {
        if self.budget.is_none()
            && let Some(inline) = self.inline.as_mut()
        {
            inline.note(&made.tokens, nowrap);
        }
    }

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

    pub(crate) fn restore_protected(&self, text: JsString, href: bool) -> JsString {
        restore_protected_with(self.safe_tags, self.safe_blocks, self.safe_sequences, text, href)
    }

    pub(crate) fn quote_parts(&mut self) -> (QuoteTags<'_>, &mut QuoteSession, &JsString) {
        let Ctx { text, safe_tags, safe_blocks, safe_sequences, check_budget, bound, .. } = self;
        let Bindings { quote_tags, inline_names, line_blocks, inline_padding, breaks, edges, quote, .. } = bound;
        let blocks: &SafeBlocks = safe_blocks;
        let sequences: &SafeSequences = safe_sequences;
        let edges = edges.as_mut().map(|reader| EdgeView { reader, blocks, sequences });
        let cx = QuoteTags::session(
            safe_tags,
            *quote_tags,
            *inline_names,
            line_blocks.then_some(blocks),
            *inline_padding,
            breaks,
            edges,
            Some(*check_budget),
        );
        (cx, quote, text)
    }

    pub(crate) fn quote_cx(&mut self) -> (QuoteTags<'_>, &mut QuoteSession) {
        let (cx, qs, _) = self.quote_parts();
        (cx, qs)
    }

    fn settle_attributes(&mut self, attrs: Vec<(String, JsString)>) -> Vec<(String, JsString)> {
        attrs
            .into_iter()
            .map(|(name, value)| {
                let guarded = match self.budget.as_mut() {
                    None => value,
                    Some(b) => b.scrub(&value),
                };
                let restored = self.restore_protected(guarded, name == "href");
                (name, strip_reserved_pua(drop_paragraphs(restored)))
            })
            .collect()
    }

    /// Vaults markup that later rules must leave alone and returns its placeholder, as `ctx.iblock()`.
    pub fn iblock(&mut self, s: &[u16]) -> Result<JsString> {
        let guarded = self.guard(s);
        let ph = self.safe_tags.iblock(&guarded)?;
        self.credit(&ph);
        Ok(ph)
    }

    pub(crate) fn find_by_tag_name(&mut self, matcher: &NameMatcher<'_>) -> Vec<(char, usize)> {
        self.safe_tags.find_by_tag_name(matcher)
    }

    /// Wraps `content` in a vaulted `tag` element with `attributes`, as `ctx.tag()`; under `html: false` it returns the content, with spaces made non-breaking for class `nowrap`.
    pub fn tag(&mut self, content: &[u16], tag: &str, attributes: &[(&str, &str)]) -> Result<JsString> {
        let attributes: Vec<(String, JsString)> = attributes.iter().map(|(k, v)| ((*k).to_owned(), js::u(v))).collect();
        self.tag_with(content, tag, attributes)
    }

    pub(crate) fn tag_with(
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
                self.note(&made, true);
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
        self.note(&made, nowrap);
        Ok(self.record(made))
    }
}

fn apply_rule(rule: &CompiledRule, text: &[u16], ctx: &mut Ctx<'_>) -> Result<Option<JsString>> {
    match &rule.body {
        RuleBody::Handler(handler) => {
            if ctx.trace.is_some() {
                return ctx.trace_handler(text, handler);
            }
            if ctx.text != text {
                ctx.text = text.to_vec();
            }
            ctx.forget_text_identity();
            let r = handler(ctx)?;
            Ok(Some(r.unwrap_or_else(|| std::mem::take(&mut ctx.text))))
        }
        RuleBody::Patterns(patterns) => {
            let mut result: Option<JsString> = None;
            for (re, repl) in patterns {
                let current = result.as_deref().unwrap_or(text);
                ctx.forget_text_identity();
                let next = if ctx.trace.is_some() {
                    ctx.trace_replace(current, re, repl)?
                } else {
                    match repl {
                        Replacement::Template(t) => re.replace_tpl_matches(current, t),
                        Replacement::Func(f) => re.try_replace_matches(current, |c| f(c, ctx))?,
                    }
                };
                if let Some(next) = next
                    && let Some(old) = result.replace(next)
                {
                    js::regex::recycle(old);
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
        if let Some(next) = re.replace_tpl_matches(result.as_deref().unwrap_or(text), t)
            && let Some(old) = result.replace(next)
        {
            js::regex::recycle(old);
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

fn execute_rule(rule: &CompiledRule, text: &[u16], ctx: &mut Ctx<'_>, stage: &str) -> Result<Option<JsString>> {
    ctx.open_trace(text)?;
    let result = run_rule(rule, text, ctx)?;
    (ctx.check_budget)(stage)?;
    Ok(result)
}

struct KeptRule<'a>(&'a CompiledRule, &'a str);

fn keep_sequences(
    before: &[u16],
    after: JsString,
    ctx: &mut Ctx<'_>,
    rerun: impl FnOnce(&[u16], &mut Ctx<'_>) -> Result<JsString>,
) -> Result<JsString> {
    let settled = if ctx.trace.is_some() {
        ctx.safe_sequences.settle_with(before, after, true)
    } else {
        ctx.safe_sequences.settle(before, after)
    };
    match settled {
        Ok(kept) => Ok(kept),
        Err(sealed) => {
            if let Some(trace) = ctx.trace.as_mut() {
                let pending = trace.pending.as_ref().expect("attempt before seal");
                trace.retry_map = Some(pending.input_map.edited(
                    &sealed.text,
                    sealed.edits.as_deref().unwrap_or_default(),
                    ctx.check_budget,
                )?);
                trace.retry_projection = pending.projection.clone();
            }
            let again = rerun(&sealed.text, ctx)?;
            if ctx.trace.is_none() {
                return Ok(ctx.safe_sequences.unseal(before, sealed, again));
            }
            let mut mapping = ctx.trace.as_ref().map(|_| RestoreMap::default());
            let (result, accepted) = ctx.safe_sequences.unseal_with(before, sealed, again, mapping.as_mut());
            if !accepted {
                ctx.discard_trace();
            } else if let (Some(trace), Some(mapping)) = (ctx.trace.as_mut(), mapping) {
                let pending = trace.pending.as_mut().expect("sealed attempt");
                pending.map = mapping.forward(pending.map.clone(), ctx.check_budget)?;
            }
            Ok(result)
        }
    }
}

impl LiftRule for KeptRule<'_> {
    fn html_only(&self) -> bool {
        self.0.html_only
    }

    fn requires(&self) -> &Requires {
        &self.0.requires
    }

    fn lift_flags(&self) -> LiftFlags {
        self.0.lift
    }

    fn apply(&self, input: &[u16], ctx: &mut Ctx<'_>) -> Result<Option<JsString>> {
        let Some(output) = execute_rule(self.0, input, ctx, self.1)? else { return Ok(None) };
        keep_sequences(input, output, ctx, |text, ctx| {
            execute_rule(self.0, text, ctx, self.1).map(|out| out.unwrap_or_else(|| text.to_vec()))
        })
        .map(Some)
    }
}

static YEAR_MARKS_RULE: LazyLock<CompiledRule> = LazyLock::new(|| CompiledRule {
    id: js::u("settle_year_marks"),
    default_enabled: true,
    html_only: false,
    cycled: false,
    templates_only: false,
    body: RuleBody::Handler(Arc::new(|ctx: &mut Ctx<'_>| Ok(Some(crate::rules::date::settle_year_marks(&ctx.text))))),
    requires: crate::rules::date::YEAR_MARKS.clone(),
    lift: LiftFlags::default(),
});

pub fn run_year_marks(text: JsString, group: &CompiledGroup, ctx: &mut Ctx<'_>) -> Result<JsString> {
    run_compiled_step(text, group, &YEAR_MARKS_RULE, ctx)
}

pub fn is_rule_on(group: &CompiledGroup, id: &str) -> bool {
    group.rules.iter().find(|r| js::eq(&r.id, id)).is_some_and(|rule| is_rule_enabled(group, rule))
}

pub fn run_step(text: JsString, group: &CompiledGroup, id: &str, ctx: &mut Ctx<'_>) -> Result<JsString> {
    let rule = group.rules.iter().find(|rule| js::eq(&rule.id, id)).expect("selected builtin rule");
    run_compiled_step(text, group, rule, ctx)
}

fn run_compiled_step(
    text: JsString,
    group: &CompiledGroup,
    rule: &CompiledRule,
    ctx: &mut Ctx<'_>,
) -> Result<JsString> {
    let kind = if matches!(rule.body, RuleBody::Handler(_)) { TraceKind::Handler } else { TraceKind::Regex };
    ctx.bind_boundary_owner(group);
    ctx.begin_trace(&group.name, &rule.id, kind, &text)?;
    let result = run_step_body(text, group, rule, ctx)?;
    ctx.accept_trace(None)?;
    ctx.finish_trace(&result)?;
    Ok(result)
}

fn run_step_body(text: JsString, group: &CompiledGroup, rule: &CompiledRule, ctx: &mut Ctx<'_>) -> Result<JsString> {
    ctx.group = Some(group.view.clone());
    let mut known = None;
    if !allows(&mut ctx.features, &rule.requires, &text, &mut known, false) {
        return Ok(text);
    }
    if !inline_lift::wants_lift(&text, ctx) {
        if !allows(&mut ctx.features, &rule.requires, &text, &mut known, true) {
            return Ok(text);
        }
        return execute_kept(rule, text, ctx, &group.budget_label);
    }
    match inline_lift::begin_group(text, ctx)? {
        Ok(mut lifted) => {
            inline_lift::apply_rule(&mut lifted, &KeptRule(rule, &group.budget_label), ctx)?;
            Ok(inline_lift::end_group(lifted, ctx))
        }
        Err(text) => execute_kept(rule, text, ctx, &group.budget_label),
    }
}

fn execute_kept(rule: &CompiledRule, text: JsString, ctx: &mut Ctx<'_>, stage: &str) -> Result<JsString> {
    let Some(output) = execute_rule(rule, &text, ctx, stage)? else { return Ok(text) };
    if !ctx.safe_sequences.has_entries() {
        return Ok(output);
    }
    keep_sequences(&text, output, ctx, |input, ctx| {
        execute_rule(rule, input, ctx, stage).map(|out| out.unwrap_or_else(|| input.to_vec()))
    })
}

pub fn can_run_rule_group(group: &CompiledGroup, mask: u32, html: bool) -> bool {
    group
        .rules
        .iter()
        .any(|rule| is_rule_enabled(group, rule) && (!rule.html_only || html) && rule.requires.met_by(mask))
}

fn is_rule_enabled(group: &CompiledGroup, rule: &CompiledRule) -> bool {
    group.enabled_override.get(&rule.id).copied().unwrap_or(rule.default_enabled)
}

fn run_selected_rule(
    rule: &CompiledRule,
    lifted: Option<&mut inline_lift::Lifted>,
    keep: bool,
    ctx: &mut Ctx<'_>,
    stage: &str,
) -> Result<bool> {
    if ctx.trace.is_some() && lifted.is_none() && !keep {
        let input = std::mem::take(&mut ctx.text);
        let output = execute_rule(rule, &input, ctx, stage)?;
        let kept = output.is_none();
        ctx.text = output.unwrap_or(input);
        ctx.accept_trace(None)?;
        return Ok(kept);
    }
    if let Some(lifted) = lifted {
        inline_lift::apply_rule(lifted, &KeptRule(rule, stage), ctx)?;
        return Ok(false);
    }
    if keep {
        let input = std::mem::take(&mut ctx.text);
        let output = execute_rule(rule, &input, ctx, stage)?;
        let kept = output.is_none();
        ctx.text = match output {
            None => input,
            Some(output) => keep_sequences(&input, output, ctx, |text, ctx| {
                execute_rule(rule, text, ctx, stage).map(|out| out.unwrap_or_else(|| text.to_vec()))
            })?,
        };
        ctx.accept_trace(None)?;
        return Ok(kept);
    }
    if rule.templates_only
        && !rule.cycled
        && let RuleBody::Patterns(patterns) = &rule.body
    {
        let next = apply_templates(patterns, &ctx.text);
        let kept = next.is_none();
        if let Some(next) = next {
            js::regex::recycle(std::mem::replace(&mut ctx.text, next));
        }
        (ctx.check_budget)(stage)?;
        return Ok(kept);
    }
    if !rule.cycled {
        let kept = match &rule.body {
            RuleBody::Handler(handler) => {
                if let Some(next) = handler(ctx)? {
                    ctx.text = next;
                }
                false
            }
            RuleBody::Patterns(_) => {
                let input = std::mem::take(&mut ctx.text);
                if let Some(next) = apply_rule(rule, &input, ctx)? {
                    ctx.text = next;
                    js::regex::recycle(input);
                    false
                } else {
                    ctx.text = input;
                    true
                }
            }
        };
        (ctx.check_budget)(stage)?;
        return Ok(kept);
    }
    let input = ctx.text.clone();
    let output = execute_rule(rule, &input, ctx, stage)?;
    let kept = output.is_none();
    ctx.text = output.unwrap_or(input);
    Ok(kept)
}

pub fn run_rule_group(group: &CompiledGroup, text: JsString, ctx: &mut Ctx<'_>) -> Result<JsString> {
    ctx.text = text;
    ctx.group = Some(group.view.clone());
    if let Some(pre) = &group.pre_parse {
        pre(ctx)?;
        (ctx.check_budget)(&group.budget_label)?;
    }
    let keep = group.builtin && ctx.safe_sequences.has_entries();
    let mut lifted = if group.builtin && ctx.inline.is_some() {
        match inline_lift::begin_group(std::mem::take(&mut ctx.text), ctx)? {
            Ok(lifted) => Some(lifted),
            Err(text) => {
                ctx.text = text;
                None
            }
        }
    } else {
        None
    };
    let mut known = None;
    for rule in group.rules.iter() {
        if !is_rule_enabled(group, rule) || (rule.html_only && !ctx.html) {
            continue;
        }
        if lifted.is_none() && !allows(&mut ctx.features, &rule.requires, &ctx.text, &mut known, true) {
            continue;
        }
        let tracing = ctx.trace.is_some();
        if tracing {
            let input = lifted.as_ref().map_or_else(|| ctx.text.clone(), inline_lift::materialized);
            let kind = if matches!(rule.body, RuleBody::Handler(_)) { TraceKind::Handler } else { TraceKind::Regex };
            ctx.begin_trace(&group.name, &rule.id, kind, &input)?;
        }
        if !run_selected_rule(rule, lifted.as_mut(), keep, ctx, &group.budget_label)? {
            known = None;
        }
        if tracing {
            let output = lifted.as_ref().map_or_else(|| ctx.text.clone(), inline_lift::materialized);
            ctx.finish_trace(&output)?;
        }
    }
    if let Some(lifted) = lifted {
        ctx.text = inline_lift::end_group(lifted, ctx);
    }
    Ok(std::mem::take(&mut ctx.text))
}

pub fn run_group(group: &CompiledGroup, text: JsString, ctx: &mut Ctx<'_>) -> Result<JsString> {
    ctx.bind_boundary_owner(group);
    if group.builtin {
        return run_rule_group(group, text, ctx);
    }
    ctx.budget = Some(TokenBudget::new(text.clone(), SafeSequences::restore_regexes().to_vec()));
    let outcome = run_rule_group(group, text.clone(), ctx);
    let mut budget = ctx.budget.take().expect("budget opened above");
    let after = outcome?;
    Ok(if after == text && budget.untouched() { after } else { budget.scrub(&after) })
}

/// A registered rule group, as `getRuleGroup` describes it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuleGroupInfo {
    /// The group name, in lower case.
    pub name: JsString,
    /// The title, or the name when there is none.
    pub title: JsString,
    pub builtin: bool,
    /// The rules in run order.
    pub rules: Vec<RuleInfo>,
}

/// A rule of a [`RuleGroupInfo`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuleInfo {
    pub id: JsString,
    /// Whether the rule runs unless switched.
    pub default_enabled: bool,
}

impl RuleGroupInfo {
    pub fn to_json(&self) -> String {
        let quote = |units: &[u16]| js::lossy(&js::json::stringify(units));
        let rules = self
            .rules
            .iter()
            .map(|rule| format!("{{\"id\":{},\"defaultEnabled\":{}}}", quote(&rule.id), rule.default_enabled))
            .collect::<Vec<_>>()
            .join(",");
        format!(
            "{{\"name\":{},\"title\":{},\"builtin\":{},\"rules\":[{}]}}",
            quote(&self.name),
            quote(&self.title),
            self.builtin,
            rules
        )
    }
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
