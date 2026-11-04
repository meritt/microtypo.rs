use std::collections::HashMap;
use std::fmt;
use std::sync::{Arc, LazyLock};

use serde_json::{Map, Value, json};

use crate::cat;
use crate::clear_special::clear_special_chars;
use crate::core::options::{canonical_group_name, is_on, normalise_options, wildcard_to_regex};
use crate::errors::{Error, Result};
use crate::input::frontmatter::{Frontmatter, split_frontmatter};
use crate::input::json::{Selectors, compile_span_selectors, scan_json, select_value_spans};
use crate::input::markdown;
use crate::input::span_path::LocatedSpan;
use crate::input::template::{scanner_for, template_reader};
use crate::input::toml::scan_toml;
use crate::input::xml::{XML_BLOCKS, scan_doctype, scan_xml_space_preserve, validate_xml};
use crate::input::yaml::scan_yaml;
use crate::js::{self, JsString, Lit, Re};
use crate::lit;
use crate::protect::blocks::{
    CheckBudget, SafeBlocks, ScanCtx, Scanner, TagOptions, scan_html_comments, scan_line_indent,
};
use crate::protect::placeholders::restore_paragraphs;
use crate::protect::reserved::{escape_reserved, has_reserved};
use crate::protect::sequences::SafeSequences;
use crate::protect::tag_spans::tag_spans;
use crate::protect::tags::{DEFAULT_LAYOUT, Layout, SafeTags};
use crate::rule_runner::{
    CompiledGroup, Ctx, CycleHook, RuleGroupDef, RuleGroupInfo, compile_rule_group, describe_rule_group, run_group,
};
use crate::rules::registry::default_groups;
use crate::schemas::config::{InputConfig, UNSAFE_PREFIX_RE, ValidatedConfig, validate_config};
use crate::shared::block_tags::{BLOCK_TAG_NAMES, VOID_BLOCK_TAG_NAMES};
use crate::shared::diff::changed_ranges;
use crate::shared::glyphs::{unicode_to_entities, unicode_to_xml_entities};
use crate::shared::strings::{edge_whitespace, leading_indent, trim_blank_lines};

const DEFAULT_SAFE_TAGS: [&str; 9] = ["pre", "code", "script", "style", "kbd", "samp", "var", "tt", "textarea"];
const NON_RENDERING_TAGS: [&str; 2] = ["script", "style"];
const RAW_TEXT_TAGS: [&str; 3] = ["script", "style", "textarea"];

const NOTG_START: &str = r#"<span class="_notg_start"></span>"#;
const NOTG_END: &str = r#"<span class="_notg_end"></span>"#;
static DEFAULT_COMPILED: LazyLock<Vec<CompiledGroup>> = LazyLock::new(|| {
    default_groups()
        .iter()
        .map(|(name, def)| {
            let mut group = compile_rule_group(def, &canonical_group_name(name)).expect("built-in group compiles");
            group.builtin = true;
            group
        })
        .collect()
});

static NOTG_RE: LazyLock<Re> =
    LazyLock::new(|| Re::new(r#"<notg(?:\s(?:[^<>"']|"[^"]*"|'[^']*')*)?>([\s\S]*?)<\/notg>"#, "gi"));

const BLOCK_RENDER_FORMATS: [&str; 2] = ["markdown", "frontmatter"];

pub const DEFAULT_MAX_INPUT_LENGTH: usize = 30_000;

static BLOCK_TAG_RE: LazyLock<Re> = LazyLock::new(|| {
    let names = BLOCK_TAG_NAMES.join("|");
    Re::new(&format!(r"<p>(\s*)(<(?:{names})\b[^>]*>(?:(?!<\/p>)[\s\S])*?<\/(?:{names})>)(\s*)<\/p>"), "gi")
});
static VOID_BLOCK_RE: LazyLock<Re> = LazyLock::new(|| {
    let names = VOID_BLOCK_TAG_NAMES.join("|");
    Re::new(&format!(r"<p>(\s*)(<(?:{names})\b[^>]*\/?>)(\s*)<\/p>"), "gi")
});
static P_RE: LazyLock<Re> = LazyLock::new(|| Re::new("<p>", "i"));
static CRLF_RE: LazyLock<Re> = LazyLock::new(|| Re::new(r"\r\n?", "g"));
static NEWLINES_RE: LazyLock<Re> = LazyLock::new(|| Re::new(r"\n{3,}", "g"));
static LF: Lit = lit!("\n");
static LF_LF: Lit = lit!("\n\n");
static GROUP_2: Lit = lit!("$2");
static POSITION_RE: LazyLock<Re> = LazyLock::new(|| Re::new(r"^(after|before):(.+)$", ""));

pub(crate) fn js_number(n: f64) -> String {
    if n == 0.0 {
        return "0".to_owned();
    }
    if !n.is_finite() {
        return if n.is_nan() {
            "NaN"
        } else if n > 0.0 {
            "Infinity"
        } else {
            "-Infinity"
        }
        .to_owned();
    }
    let sci = format!("{:e}", n.abs());
    let (mantissa, exp) = sci.split_once('e').unwrap_or((&sci, "0"));
    let digits = mantissa.replace('.', "");
    let k = digits.len() as i32;
    let point = exp.parse::<i32>().unwrap_or(0) + 1;
    let body = if k <= point && point <= 21 {
        format!("{digits}{}", "0".repeat((point - k) as usize))
    } else if 0 < point && point <= 21 {
        format!("{}.{}", &digits[..point as usize], &digits[point as usize..])
    } else if -6 < point && point <= 0 {
        format!("0.{}{digits}", "0".repeat((-point) as usize))
    } else {
        let sign = if point - 1 < 0 { '-' } else { '+' };
        let fraction = if k > 1 { format!(".{}", &digits[1..]) } else { String::new() };
        format!("{}{fraction}e{sign}{}", &digits[..1], (point - 1).abs())
    };
    if n < 0.0 { format!("-{body}") } else { body }
}

pub(crate) fn num_value(n: f64) -> Value {
    if n.fract() == 0.0 && n.abs() < 9.0e15 { json!(n as i64) } else { json!(n) }
}

fn json_quote(s: &str) -> String {
    Value::String(s.to_owned()).to_string()
}

pub(crate) fn js_to_string(value: &Value) -> String {
    match value {
        Value::Null => "null".to_owned(),
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => js_number(n.as_f64().unwrap_or(0.0)),
        Value::String(s) => s.clone(),
        Value::Array(items) => items
            .iter()
            .map(|item| if item.is_null() { String::new() } else { js_to_string(item) })
            .collect::<Vec<_>>()
            .join(","),
        Value::Object(_) => "[object Object]".to_owned(),
    }
}

#[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
fn now_ms() -> f64 {
    js_sys::Date::now()
}

#[cfg(not(all(target_arch = "wasm32", target_os = "unknown")))]
fn now_ms() -> f64 {
    static START: LazyLock<std::time::Instant> = LazyLock::new(std::time::Instant::now);
    START.elapsed().as_secs_f64() * 1000.0
}

fn budget_check(budget: f64, start: f64) -> impl Fn(&str) -> Result<()> {
    move |at: &str| {
        if budget > 0.0 && now_ms() - start > budget {
            return Err(Error::budget(
                format!("Processing exceeded {}ms budget at {at} — likely adversarial input", js_number(budget)),
                json!({ "budgetMs": num_value(budget), "where": at }),
            ));
        }
        Ok(())
    }
}

fn unwrap_blocks(text: JsString) -> JsString {
    if !P_RE.test(&text) {
        return text;
    }
    let mut result = text;
    loop {
        let next = VOID_BLOCK_RE.replace_tpl_owned(BLOCK_TAG_RE.replace_tpl(&result, &GROUP_2), &GROUP_2);
        if next == result {
            return result;
        }
        result = next;
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Destination {
    Html,
    Xml,
    DataValue,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Trim {
    None,
    Js,
    BlankLines,
}

#[derive(Clone, Copy)]
struct Fragment {
    destination: Destination,
    trim: Trim,
    indents: bool,
}

impl Fragment {
    const fn new(destination: Destination, trim: Trim) -> Self {
        Fragment { destination, trim, indents: false }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Position {
    End,
    Start,
    Spelled(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Processed {
    pub text: JsString,
    pub changes: Vec<(usize, usize)>,
}

pub struct MicroTypo {
    groups: HashMap<String, CompiledGroup>,
    order: Vec<String>,
    safe_blocks: SafeBlocks,
    safe_sequences: SafeSequences,
    safe_tags: SafeTags,
    engine_settings: Map<String, Value>,
    layout: Layout,
    prefix: String,
    max_input_length: f64,
    max_processing_ms: f64,
    entities: bool,
    html: bool,
    input: InputConfig,
    presets: bool,
    select: Selectors,
    reserved_possible: bool,
    on_cycle_limit: Option<CycleHook>,
}

impl MicroTypo {
    pub fn new(config: Option<&Value>) -> Result<Self> {
        let validated = validate_config(config)?;
        let input = validated.input.clone();
        let presets = validated.presets() != Some(false);
        let select = compile_span_selectors(input.fields.as_deref(), input.exclude.as_deref());
        let document = input.format == "text" || input.format == "html";
        let mut engine = MicroTypo {
            groups: HashMap::new(),
            order: Vec::new(),
            safe_blocks: SafeBlocks::default(),
            safe_sequences: SafeSequences::default(),
            safe_tags: SafeTags::new(document),
            engine_settings: Map::new(),
            layout: DEFAULT_LAYOUT,
            prefix: String::new(),
            max_input_length: DEFAULT_MAX_INPUT_LENGTH as f64,
            max_processing_ms: 5_000.0,
            entities: false,
            html: false,
            input,
            presets,
            select,
            reserved_possible: false,
            on_cycle_limit: None,
        };
        if presets {
            for group in DEFAULT_COMPILED.iter() {
                engine.order.push(group.name.clone());
                engine.groups.insert(group.name.clone(), group.clone());
            }
        }
        engine.init_blocks()?;
        engine.apply_validated_options(&validated)?;
        Ok(engine)
    }

    pub fn from_json(config: &str) -> Result<Self> {
        let value: Value = serde_json::from_str(config)
            .map_err(|e| Error::config(format!("invalid config JSON: {e}"), json!({ "reason": "json" })))?;
        MicroTypo::new(Some(&value))
    }

    fn init_blocks(&mut self) -> Result<()> {
        let format = self.input.format.clone();
        let read_templates = if self.input.template == "off" { None } else { template_reader(&self.input.template) };
        let scan_template: Option<Scanner> = read_templates.clone().map(scanner_for);
        let is_markdown = BLOCK_RENDER_FORMATS.contains(&format.as_str());
        if !is_markdown {
            self.add_safe_elements()?;
        }
        if is_markdown {
            markdown::add_block_scanners(&mut self.safe_blocks, read_templates.clone());
            self.add_safe_elements()?;
            markdown::add_inline_scanners(&mut self.safe_blocks);
            self.add_safe_block("md-hard-break", "(?=[ ]{2,}[\\r\\n])", "(?<=[ ]{2,})(?=[\\r\\n])", true)?;
            self.apply_override("text", "paragraphs", false)?;
            self.apply_override("text", "breakline", false)?;
        }
        if format == "xml" {
            self.safe_blocks
                .add_scanner("xml-space-preserve", Arc::new(|t, c| scan_xml_space_preserve(t, c.check_budget)));
            self.safe_blocks.add_scanner("xml-doctype", Arc::new(|t, c| scan_doctype(t, c.check_budget)));
            for (id, open, close) in XML_BLOCKS {
                self.add_safe_block(id, open, close, false)?;
            }
            self.add_safe_block("xml-entity", "&(?:#[0-9]+|#x[0-9a-fA-F]+|[A-Za-z][A-Za-z0-9]*)(?=;)", ";", true)?;
        }
        let html = format == "text" || format == "html";
        if html || is_markdown {
            let scan: Scanner = if is_markdown {
                Arc::new(|t, c: &ScanCtx<'_>| markdown::scan_markdown_comments(t, c.check_budget))
            } else {
                Arc::new(|t, c: &ScanCtx<'_>| scan_html_comments(t, c.check_budget))
            };
            self.safe_blocks.add_scanner("html-comment", scan);
        }
        if !is_markdown && let Some(scan) = scan_template {
            self.safe_blocks.add_scanner(&format!("tmpl-{}", self.input.template), scan);
        }
        if html {
            self.safe_blocks.set_tail(NOTG_RE.clone());
            self.safe_blocks.set_indents(scan_line_indent);
        }
        Ok(())
    }

    fn add_safe_elements(&mut self) -> Result<()> {
        let xml = self.input.format == "xml";
        for tag in DEFAULT_SAFE_TAGS {
            self.safe_blocks.add_tag(
                tag,
                TagOptions {
                    renders: !NON_RENDERING_TAGS.contains(&tag),
                    nests: xml || !RAW_TEXT_TAGS.contains(&tag),
                    form_feed: !xml,
                    slash_opens: !xml,
                },
            )?;
        }
        self.safe_blocks.add("span-notg", NOTG_START, NOTG_END, false)
    }

    pub fn register_rule_group(
        &mut self,
        group: RuleGroupDef,
        name: Option<&str>,
        position: Position,
    ) -> Result<&mut Self> {
        let group_name = name
            .map(str::to_owned)
            .filter(|n| !n.is_empty())
            .or_else(|| group.name.clone().filter(|n| !n.is_empty()))
            .or_else(|| group.title.clone().filter(|n| !n.is_empty()))
            .ok_or_else(|| Error::type_error("Rule group must have a name"))?;
        let canonical = canonical_group_name(&group_name);
        let compiled = compile_rule_group(&group, &canonical)?;
        let order = self.order_with(&canonical, &position)?;
        self.groups.insert(canonical, compiled);
        self.order = order;
        Ok(self)
    }

    fn order_with(&self, name: &str, position: &Position) -> Result<Vec<String>> {
        let mut order: Vec<String> = self.order.iter().filter(|n| *n != name).cloned().collect();
        let spelled = match position {
            Position::End => {
                order.push(name.to_owned());
                return Ok(order);
            }
            Position::Start => {
                order.insert(0, name.to_owned());
                return Ok(order);
            }
            Position::Spelled(s) if s == "end" || s.is_empty() => {
                order.push(name.to_owned());
                return Ok(order);
            }
            Position::Spelled(s) if s == "start" => {
                order.insert(0, name.to_owned());
                return Ok(order);
            }
            Position::Spelled(s) => s,
        };
        let u = js::u(spelled);
        let found = POSITION_RE.exec(&u).and_then(|m| {
            let after = m.s(1) == js::u("after").as_slice();
            let target = canonical_group_name(&js::lossy(m.s(2)));
            order.iter().position(|n| *n == target).map(|idx| (after, idx))
        });
        match found {
            Some((after, idx)) => {
                order.insert(if after { idx + 1 } else { idx }, name.to_owned());
                Ok(order)
            }
            None => Err(Error::config(
                format!(
                    "position must be \"start\", \"end\", or \"after:\"/\"before:\" an already registered group: {}",
                    json_quote(spelled)
                ),
                json!({ "position": spelled }),
            )),
        }
    }

    pub fn get_rule_group(&self, name: &str) -> Option<RuleGroupInfo> {
        self.groups.get(&canonical_group_name(name)).map(describe_rule_group)
    }

    pub fn list_rule_groups(&self) -> Vec<RuleGroupInfo> {
        self.order.iter().map(|n| describe_rule_group(&self.groups[n])).collect()
    }

    pub fn add_safe_tag(&mut self, tag: &str) -> Result<&mut Self> {
        let form_feed = self.input.format != "xml";
        self.safe_blocks.add_tag(tag, TagOptions { form_feed, ..TagOptions::default() })?;
        Ok(self)
    }

    pub fn add_safe_block(&mut self, id: &str, open: &str, close: &str, unsafe_regex: bool) -> Result<&mut Self> {
        self.safe_blocks.add(id, open, close, unsafe_regex)?;
        Ok(self)
    }

    pub(crate) fn add_safe_block_sources(
        &mut self,
        id: &str,
        o: &str,
        c: &str,
        details: impl FnOnce() -> Value,
    ) -> Result<()> {
        self.safe_blocks.add_sources(id, o, c, details)
    }

    pub fn set_layout(&mut self, layout: &str) -> Result<&mut Self> {
        if !matches!(layout, "style" | "class" | "both") {
            return Err(Error::config(
                format!("layout must be \"style\", \"class\", or \"both\": {}", json_quote(layout)),
                json!({ "layout": layout }),
            ));
        }
        self.layout = Layout { style: layout != "class", class: layout != "style" };
        Ok(self)
    }

    pub fn set_layout_value(&mut self, value: &Value) -> Result<()> {
        match value.as_str() {
            Some(s) => self.set_layout(s).map(|_| ()),
            None => Err(Error::config(
                format!("layout must be \"style\", \"class\", or \"both\": {value}"),
                json!({ "layout": value }),
            )),
        }
    }

    pub fn set_prefix_value(&mut self, p: &Value) -> Result<&mut Self> {
        let prefix = match p {
            Value::Bool(true) => "mt_".to_owned(),
            Value::String(s) => s.clone(),
            Value::Number(n) if n.as_f64() != Some(0.0) => js_number(n.as_f64().unwrap_or(0.0)),
            Value::Array(_) | Value::Object(_) => js_to_string(p),
            _ => String::new(),
        };
        if UNSAFE_PREFIX_RE.test(&js::u(&prefix)) {
            let (shown, details) = match p {
                Value::Array(_) | Value::Object(_) => (p.to_string(), p.clone()),
                _ => (json_quote(&prefix), Value::String(prefix)),
            };
            return Err(Error::config(
                format!("prefix must not contain a quote, angle bracket, \"&\", or whitespace: {shown}"),
                json!({ "prefix": details }),
            ));
        }
        self.prefix = prefix;
        Ok(self)
    }

    pub fn set_prefix(&mut self, prefix: &str) -> Result<&mut Self> {
        self.set_prefix_value(&Value::String(prefix.to_owned()))
    }

    pub fn set_on_cycle_limit(&mut self, hook: Option<CycleHook>) {
        self.on_cycle_limit = hook;
    }

    pub fn apply_options(&mut self, options: Option<&Value>) -> Result<&mut Self> {
        let validated = validate_config(options)?;
        let asked_input = options.and_then(Value::as_object).is_some_and(|o| o.contains_key("input"));
        if asked_input && validated.input != self.input {
            return Err(Error::config(
                format!(
                    "input is settled by the constructor; build a MicroTypo for \"{}\" instead of changing this one",
                    validated.input.format
                ),
                json!({ "format": validated.input.format, "current": self.input.format }),
            ));
        }
        if let Some(presets) = validated.presets()
            && presets != self.presets
        {
            return Err(Error::config(
                "presets is settled by the constructor",
                json!({ "presets": presets, "current": self.presets }),
            ));
        }
        self.apply_validated_options(&validated)?;
        Ok(self)
    }

    fn apply_validated_options(&mut self, validated: &ValidatedConfig) -> Result<()> {
        let intent = normalise_options(&validated.raw);
        for (key, value) in &intent.engine_settings {
            self.apply_engine_setting(key, value)?;
        }
        for s in &intent.settings {
            if let Some(g) = self.groups.get_mut(&canonical_group_name(&s.group)) {
                g.set_setting(&s.key, s.value.clone());
            }
        }
        for o in &intent.overrides {
            self.apply_override(&canonical_group_name(&o.group), &o.selector, o.value)?;
        }
        Ok(())
    }

    fn apply_engine_setting(&mut self, key: &str, value: &Value) -> Result<()> {
        match key {
            "entities" => self.entities = is_on(value),
            "html" => self.html = is_on(value),
            "layout" => self.set_layout_value(value)?,
            "prefix" => {
                self.set_prefix_value(value)?;
            }
            "maxInputLength" => {
                if let Some(n) = value.as_f64() {
                    self.max_input_length = n;
                }
            }
            "maxProcessingMs" => {
                if let Some(n) = value.as_f64() {
                    self.max_processing_ms = n;
                }
            }
            _ => {
                self.engine_settings.insert(key.to_owned(), value.clone());
            }
        }
        Ok(())
    }

    fn apply_override(&mut self, group: &str, selector: &str, value: bool) -> Result<()> {
        if group == "*" {
            for g in self.groups.values_mut() {
                Self::override_group(g, selector, value)?;
            }
            return Ok(());
        }
        if let Some(g) = self.groups.get_mut(group) {
            Self::override_group(g, selector, value)?;
        }
        Ok(())
    }

    fn override_group(group: &mut CompiledGroup, selector: &str, value: bool) -> Result<()> {
        if selector == "*" {
            for r in group.rules.iter() {
                group.enabled_override.insert(r.id.clone(), value);
            }
            return Ok(());
        }
        let re = wildcard_to_regex(selector)?;
        for r in group.rules.iter() {
            if re.test(&js::u(&r.id)) {
                group.enabled_override.insert(r.id.clone(), value);
            }
        }
        Ok(())
    }

    pub fn process(&mut self, text: &[u16]) -> Result<JsString> {
        self.process_inner(text, false).map(|p| p.text)
    }

    pub fn process_with_changes(&mut self, text: &[u16]) -> Result<Processed> {
        self.process_inner(text, true)
    }

    pub fn process_str(&mut self, text: &str) -> Result<String> {
        self.process(&js::u(text)).map(|t| js::lossy(&t))
    }

    fn process_inner(&mut self, text: &[u16], with_changes: bool) -> Result<Processed> {
        if text.len() as f64 > self.max_input_length {
            return Err(Error::input(
                format!("Input length {} exceeds maxInputLength ({})", text.len(), js_number(self.max_input_length)),
                json!({ "length": text.len(), "max": num_value(self.max_input_length) }),
            ));
        }
        let check = budget_check(self.max_processing_ms, now_ms());
        self.reserved_possible = self.input.format == "json" || has_reserved(text);
        let result = self.typeset(text, &check)?;
        if !with_changes {
            return Ok(Processed { text: result, changes: Vec::new() });
        }
        let changes = changed_ranges(text, &result, &check)?;
        Ok(Processed { text: result, changes })
    }

    fn reset_vaults(&mut self) {
        self.safe_blocks.reset();
        self.safe_sequences.reset();
        self.safe_tags.reset();
    }

    fn typeset(&mut self, text: &[u16], cb: CheckBudget<'_>) -> Result<JsString> {
        self.reset_vaults();
        match self.input.format.as_str() {
            "json" => self.process_json(text, cb),
            "toml" => self.typeset_structured(text, scan_toml, cb),
            "yaml" => self.typeset_structured(text, scan_yaml, cb),
            "xml" => {
                validate_xml(text, cb)?;
                let edges = edge_whitespace(text);
                let core = &text[edges.start..edges.end];
                let typed = self.run_pipeline(core, cb, Fragment::new(Destination::Xml, Trim::None))?;
                Ok(cat!(&text[..edges.start], typed, &text[edges.end..]))
            }
            "frontmatter" => self.typeset_frontmatter(text, cb),
            "markdown" => self.run_pipeline(text, cb, Fragment::new(Destination::Html, Trim::BlankLines)),
            _ => {
                let (from, to) = leading_indent(text);
                let spliced = cat!(&text[..from], &text[to..]);
                let body = self.run_pipeline(
                    &spliced,
                    cb,
                    Fragment { destination: Destination::Html, trim: Trim::Js, indents: true },
                )?;
                Ok(if body.is_empty() { body } else { cat!(&text[from..to], body) })
            }
        }
    }

    fn typeset_frontmatter(&mut self, text: &[u16], cb: CheckBudget<'_>) -> Result<JsString> {
        let body_fragment = Fragment::new(Destination::Html, Trim::BlankLines);
        match split_frontmatter(text) {
            None => self.run_pipeline(text, cb, body_fragment),
            Some(Frontmatter::Malformed) => Err(Error::input(
                "Malformed frontmatter: opening delimiter has no matching closing delimiter",
                json!({}),
            )),
            Some(Frontmatter::Split { toml, header_start, header_end, body_start }) => {
                let header = &text[header_start..header_end];
                let typed_header = if toml {
                    self.typeset_structured(header, scan_toml, cb)?
                } else {
                    self.typeset_structured(header, scan_yaml, cb)?
                };
                let typed_body = self.run_pipeline(&text[body_start..], cb, body_fragment)?;
                Ok(cat!(&text[..header_start], typed_header, &text[header_end..body_start], typed_body))
            }
        }
    }

    fn process_json(&mut self, text: &[u16], cb: CheckBudget<'_>) -> Result<JsString> {
        if let Err(e) = js::json::validate(text) {
            return Err(Error::input(
                format!("Malformed JSON document: {}", e.message),
                json!({ "message": e.message }),
            ));
        }
        cb("json-parse")?;
        let spans = scan_json(text, cb)?;
        let selected = select_value_spans(&spans, &self.select, cb)?;
        self.splice_spans(text, &selected, &mut |this, span| {
            let decoded = js::json::parse_string(&text[span.start..span.end]).map_err(|e| {
                Error::input(
                    format!("Malformed JSON string literal: {}", e.message),
                    json!({ "start": span.start, "end": span.end }),
                )
            })?;
            let typed = this.typeset_value(&decoded, cb)?;
            Ok(js::json::stringify(&typed))
        })
    }

    fn typeset_value(&mut self, value: &[u16], cb: CheckBudget<'_>) -> Result<JsString> {
        let edges = edge_whitespace(value);
        if edges.start == edges.end {
            return Ok(value.to_vec());
        }
        let typed =
            self.run_pipeline(&value[edges.start..edges.end], cb, Fragment::new(Destination::DataValue, Trim::None))?;
        Ok(cat!(&value[..edges.start], js::trim(&typed), &value[edges.end..]))
    }

    fn typeset_structured(
        &mut self,
        text: &[u16],
        scan: fn(&[u16], CheckBudget<'_>) -> Result<Vec<LocatedSpan>>,
        cb: CheckBudget<'_>,
    ) -> Result<JsString> {
        let spans: Vec<LocatedSpan> = scan(text, cb)?.into_iter().filter(|s| s.eligible).collect();
        let selected = select_value_spans(&spans, &self.select, cb)?;
        self.splice_spans(text, &selected, &mut |this, span| {
            let quote = &span.delim;
            let interior = &text[span.start + quote.len()..span.end - quote.len()];
            let typed = this.typeset_value(interior, cb)?;
            if !quote.is_empty() && js::includes(&typed, quote) {
                return Ok(text[span.start..span.end].to_vec());
            }
            if let Some(guard) = span.guard
                && !guard(&typed)
            {
                return Ok(text[span.start..span.end].to_vec());
            }
            Ok(cat!(quote, typed, quote))
        })
    }

    fn splice_spans(
        &mut self,
        text: &[u16],
        spans: &[&LocatedSpan],
        replacement_for: &mut dyn FnMut(&mut Self, &LocatedSpan) -> Result<JsString>,
    ) -> Result<JsString> {
        if spans.is_empty() {
            return Ok(text.to_vec());
        }
        let mut out = Vec::with_capacity(text.len());
        let mut cursor = 0;
        for span in spans {
            out.extend_from_slice(&text[cursor..span.start]);
            out.extend(replacement_for(self, span)?);
            cursor = span.end;
        }
        out.extend_from_slice(&text[cursor..]);
        Ok(out)
    }

    fn protect_notg(&mut self, text: &[u16]) -> Result<JsString> {
        let mut spans = None;
        let mut s = 0usize;
        let tags = &mut self.safe_tags;
        NOTG_RE.try_replace_with(text, |c| {
            let offset = c.index();
            let spans = spans.get_or_insert_with(|| tag_spans(text, None));
            while s < spans.len() && spans[s].end <= offset {
                s += 1;
            }
            if s < spans.len() && spans[s].start < offset {
                return Ok(c.whole().to_vec());
            }
            Ok(cat!(NOTG_START, tags.iblock(c.s(1))?, NOTG_END))
        })
    }

    fn run_pipeline(&mut self, text: &[u16], cb: CheckBudget<'_>, fragment: Fragment) -> Result<JsString> {
        let escaped = if self.reserved_possible { escape_reserved(text)? } else { None };
        match escaped {
            None => self.typeset_fragment(text, cb, fragment),
            Some(e) => {
                let typed = self.typeset_fragment(&e.text, cb, fragment)?;
                Ok(e.unescape(&typed))
            }
        }
    }

    fn restore_protected(&self, text: &[u16]) -> JsString {
        let result = self.safe_tags.restore_iblocks(text.to_vec());
        let result = self.safe_tags.restore(result);
        let result = self.safe_blocks.restore(result);
        self.safe_sequences.restore(result)
    }

    fn typeset_fragment(&mut self, text: &[u16], cb: CheckBudget<'_>, fragment: Fragment) -> Result<JsString> {
        let eff_html = fragment.destination == Destination::Html && self.html;
        self.safe_sequences.reset_frame();
        let mut result = self.safe_sequences.protect(text)?;
        cb("safeSequences.protect")?;
        {
            let sequences = &self.safe_sequences;
            let resolve = |value: &[u16]| sequences.restore(value.to_vec());
            result = self.safe_blocks.protect(&result, cb, &resolve)?;
        }
        cb("safeBlocks.protect")?;
        result = self.protect_notg(&result)?;
        cb("protectNotg")?;
        result = self.safe_tags.protect(&result)?;
        cb("safeTags.protect")?;
        result = clear_special_chars(result);
        cb("clearSpecialChars")?;
        result = NEWLINES_RE.replace_tpl_owned(CRLF_RE.replace_tpl_owned(result, &LF), &LF_LF);
        cb("newlineInvariant")?;
        if fragment.indents {
            result = self.safe_blocks.lift_indents(&result);
        }
        {
            let mut ctx = Ctx {
                text: Vec::new(),
                group: None,
                html: eff_html,
                layout: self.layout,
                engine_settings: &self.engine_settings,
                check_budget: cb,
                on_cycle_limit: self.on_cycle_limit.clone(),
                safe_tags: &mut self.safe_tags,
                safe_blocks: &self.safe_blocks,
                safe_sequences: &self.safe_sequences,
                prefix: &self.prefix,
                budget: None,
            };
            for name in &self.order {
                let Some(group) = self.groups.get(name) else { continue };
                if fragment.indents && name == "text" {
                    result = self.safe_blocks.settle_indents(&result);
                }
                result = run_group(group, result, &mut ctx)?;
                cb(&group.budget_label)?;
            }
        }
        if fragment.indents {
            result = self.safe_blocks.settle_indents(&result);
        }
        if self.entities {
            match fragment.destination {
                Destination::Xml => result = unicode_to_xml_entities(&result),
                Destination::Html => result = unicode_to_entities(&result),
                Destination::DataValue => {}
            }
        }
        result = self.safe_tags.restore_iblocks(result);
        result = restore_paragraphs(result);
        result = self.safe_tags.restore(result);
        if eff_html {
            result = unwrap_blocks(result);
        }
        match fragment.trim {
            Trim::None => {}
            Trim::Js => result = js::trim(&result).to_vec(),
            Trim::BlankLines => {
                result = trim_blank_lines(&result, &|line| js::trim(&self.restore_protected(line)).is_empty());
            }
        }
        result = self.safe_blocks.restore(result);
        Ok(self.safe_sequences.restore(result))
    }
}

pub fn microtypo(text: &[u16], options: Option<&Value>) -> Result<JsString> {
    MicroTypo::new(options)?.process(text)
}

impl fmt::Debug for MicroTypo {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("MicroTypo").field("groups", &self.order).finish_non_exhaustive()
    }
}
