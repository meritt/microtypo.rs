use std::borrow::Cow;
use std::collections::HashMap;
use std::fmt;
use std::sync::{Arc, LazyLock};

use serde_json::{Map, Value, json};

use crate::cat;
use crate::clear_special::{decode_special_chars, read_scalar_at};
use crate::config::Config;
use crate::core::options::{canonical_group_name, is_on, normalise_options, wildcard_to_regex_units};
use crate::errors::{Error, Result};
use crate::host::Host;
use crate::inline_lift::InlineCx;
use crate::input::frontmatter::{Frontmatter, scan_leading_header, split_frontmatter};
use crate::input::json::{Selectors, compile_span_selectors, scan_json, select_value_spans};
use crate::input::localization::localization_scanner;
use crate::input::markdown;
use crate::input::pragmas::pragma_scanner;
use crate::input::span_path::LocatedSpan;
use crate::input::template::{template_body_reader, template_reader};
use crate::input::toml::scan_toml;
use crate::input::xml::{XML_BLOCKS, scan_doctype, scan_xml_space_preserve_whole, validate_xml};
use crate::input::yaml::scan_yaml;
use crate::js::raw_json::{Node, RawJson};
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
    Bindings, CompiledGroup, Ctx, CycleHook, RuleGroupDef, RuleGroupInfo, can_run_rule_group, compile_rule_group,
    describe_rule_group, is_rule_on, run_group, run_year_marks,
};
use crate::rules::quote::prepare_authored_quotes;
use crate::rules::quote_state::{EdgeView, QuoteTags, canonical_quote_roles};
use crate::rules::registry::default_groups;
use crate::rules::text::EdgeReader;
use crate::rules::text::MarkdownSource;
use crate::schemas::config::{InputConfig, UNSAFE_PREFIX_RE, ValidatedConfig, validate_config_node};
use crate::shared::block_tags::{BLOCK_TAG_NAMES, DEFAULT_SAFE_TAGS, VOID_BLOCK_TAG_NAMES};
use crate::shared::character_reference::CHARACTER_REFERENCE_HEAD;
use crate::shared::diff::changed_ranges;
use crate::shared::glyphs::{unicode_to_entities, unicode_to_xml_entities};
use crate::shared::line_endings::{LineEnding, file_line_style, restore_file_endings, restore_file_tail};
use crate::shared::rule_features::{F, Features, text_mask};
use crate::shared::strings::{edge_whitespace, leading_indent, trim_blank_lines};

const FOREIGN_TAGS: [&str; 1] = ["math"];
const NON_RENDERING_TAGS: [&str; 4] = ["script", "style", "noembed", "title"];
const RAW_TEXT_TAGS: [&str; 6] = ["script", "style", "textarea", "xmp", "noembed", "title"];

fn contains_title_tag(text: &[u16]) -> bool {
    text.windows(6).any(|w| w[0] == 0x3C && w[1..].iter().zip(b"title").all(|(&c, &l)| c | 0x20 == u16::from(l)))
}

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

const BLOCK_RENDER_FORMATS: [&str; 3] = ["markdown", "frontmatter", "mdx"];
const FILE_FORMATS: [&str; 7] = ["markdown", "frontmatter", "mdx", "yaml", "toml", "json", "xml"];

/// Default of `maxInputLength`, in UTF-16 code units.
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
static TAG_START_RE: LazyLock<Re> = LazyLock::new(|| Re::new("<[A-Za-z/!?]", "u"));
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

fn budget_check(budget: f64, now_ms: fn() -> f64, start: f64) -> impl Fn(&str) -> Result<()> {
    move |at: &str| {
        if budget > 0.0 && (js::regex::out_of_budget() || now_ms() - start > budget) {
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
    Plain,
    Xml,
    DataValue,
}

type Reads = &'static [(&'static str, &'static str)];

const DEPENDENT_READS: &[(&str, Reads)] = &[
    ("number", &[("dash", "en_range")]),
    (
        "space",
        &[
            ("punctuation", "drop_terminal_garbage"),
            ("punctuation", "collapse_doubled"),
            ("punctuation", "comma_before_conjunction"),
        ],
    ),
    (
        "abbr",
        &[
            ("space", "space_after_period"),
            ("space", "space_after_punctuation"),
            ("space", "nbsp_before_quote"),
            ("abbr", "nbsp_location"),
            ("abbr", "nbsp_organization_extended"),
            ("abbr", "nbsp_weight_unit"),
            ("number", "fraction_extended"),
            ("number", "prime"),
            ("number", "sup"),
            ("number", "sub"),
            ("abbr", "nbsp_short_acronym"),
            ("abbr", "nbsp_currency_prefix"),
            ("abbr", "nbsp_magnitude_spaced"),
            ("abbr", "nbsp_count_unit"),
            ("abbr", "nbsp_si_unit"),
            ("abbr", "nbsp_currency_code"),
            ("space", "space_after_year"),
            ("dash", "emphatic_particle"),
            ("dash", "em"),
        ],
    ),
    ("nobr", &[("abbr", "nowrap_because"), ("punctuation", "comma_before_conjunction"), ("nobr", "nbsp_short_word")]),
    ("date", &[("abbr", "nowrap_etc")]),
    ("other", &[("dash", "em_after_sentence"), ("number", "thin_space_triads")]),
    ("text", &[("space", "space_after_comma"), ("nobr", "nbsp_after_number")]),
];

#[derive(Clone, Copy, PartialEq, Eq)]
enum Trim {
    None,
    Js,
    BlankLines,
}

#[derive(Clone, Copy)]
struct Fragment<'a> {
    destination: Destination,
    trim: Trim,
    source_trim: Trim,
    indents: bool,
    first_indent: &'a [u16],
}

type Inherited<'a> = Option<&'a dyn Fn(&[u16]) -> bool>;

impl Fragment<'_> {
    const fn new(destination: Destination, trim: Trim) -> Self {
        Fragment { destination, trim, source_trim: trim, indents: false, first_indent: &[] }
    }

    const fn data_value() -> Self {
        Fragment { source_trim: Trim::Js, ..Fragment::new(Destination::DataValue, Trim::None) }
    }
}

/// Where [`MicroTypo::register_rule_group`] puts a group in the run order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Position {
    /// After every registered group.
    End,
    /// Before every registered group.
    Start,
    /// The reference spelling: `"start"`, `"end"`, `"after:<group>"` or `"before:<group>"` of a registered group.
    Spelled(String),
}

/// Text processed with its changes, as `process(text, { changes: true })` returns it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Processed {
    pub text: JsString,
    /// Changed ranges as `(start, end)` UTF-16 offsets into `text`.
    pub changes: Vec<(usize, usize)>,
}

/// The typography engine.
///
/// ```
/// use microtypo_core::{Config, MicroTypo};
/// # let host = microtypo_conformance::HOST;
///
/// let mut engine = MicroTypo::with_config(Config { html: Some(true), ..Config::default() }, host)?;
/// assert_eq!(engine.process("Цена 100 руб.")?, "<p>Цена 100\u{a0}₽.</p>");
///
/// let mut json = MicroTypo::from_json(r#"{ "html": true }"#, host)?;
/// assert_eq!(json.process("Цена 100 руб.")?, "<p>Цена 100\u{a0}₽.</p>");
/// # Ok::<(), microtypo_core::Error>(())
/// ```
pub struct MicroTypo {
    host: Host,
    groups: HashMap<JsString, CompiledGroup>,
    order: Vec<JsString>,
    safe_blocks: SafeBlocks,
    safe_sequences: SafeSequences,
    safe_tags: SafeTags,
    custom_protection: bool,
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
    ignored_file: bool,
    line_style: Option<LineEnding>,
    on_cycle_limit: Option<CycleHook>,
    trace_log: Option<crate::shared::rule_trace::TraceLog>,
    trace_map: Option<Arc<crate::shared::provenance::SourceMap>>,
    trace_escaped: Option<Arc<crate::protect::reserved::Escaped>>,
    bound: Bindings,
    markdown_inline: Option<crate::protect::blocks::OpaqueScanner>,
    attribute_workers: [Option<(u64, Box<MicroTypo>)>; 2],
}

#[allow(clippy::too_many_arguments)]
fn protect_sequences(
    sequences: &mut SafeSequences,
    value: &[u16],
    inline: Option<&crate::protect::blocks::OpaqueScanner>,
    elements: &crate::protect::blocks::ElementSet,
    mdx: bool,
    cb: CheckBudget<'_>,
    reads_inches: bool,
    code_cuts: Option<&[usize]>,
) -> Result<JsString> {
    let syntax = inline.map(|scan| {
        move |source: &[u16], _: CheckBudget<'_>| {
            Ok(markdown::markdown_address_syntax(source, scan.clone(), elements, mdx))
        }
    });
    let syntax = syntax.as_ref().map(|syntax| syntax as crate::protect::sequences::AddressSyntax<'_>);
    let protected = sequences.protect(value, syntax, cb, reads_inches, code_cuts)?;
    cb("safeSequences.protect")?;
    Ok(protected)
}

impl MicroTypo {
    pub fn with_config(config: Config, host: Host) -> Result<Self> {
        MicroTypo::new(Some(&config.to_json()), host)
    }

    /// Builds an engine from the configuration object of the reference constructor; `None` is the default configuration.
    pub fn new(config: Option<&Value>, host: Host) -> Result<Self> {
        Self::new_node(Node::root(config.unwrap_or(&Value::Null), None), host)
    }

    fn new_node(config: Node<'_>, host: Host) -> Result<Self> {
        let validated = validate_config_node(config.clone(), &HashMap::new())?;
        let input = validated.input.clone();
        let presets = validated.presets() != Some(false);
        let select = compile_span_selectors(input.fields.as_deref(), input.exclude.as_deref());
        let document = input.format == "text" || input.format == "html";
        let mut engine = MicroTypo {
            host,
            groups: HashMap::new(),
            order: Vec::new(),
            safe_blocks: SafeBlocks::default(),
            safe_sequences: SafeSequences::new(host.fill_random),
            safe_tags: SafeTags::new(document),
            custom_protection: false,
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
            ignored_file: false,
            line_style: None,
            on_cycle_limit: None,
            trace_log: None,
            trace_map: None,
            trace_escaped: None,
            bound: Bindings::default(),
            markdown_inline: None,
            attribute_workers: [None, None],
        };
        if presets {
            for group in DEFAULT_COMPILED.iter() {
                engine.order.push(group.name.clone());
                engine.groups.insert(group.name.clone(), group.clone());
            }
        }
        if let Some(groups) = validated.raw.get("ruleGroups").and_then(Value::as_array) {
            for (index, group) in groups.iter().enumerate() {
                let node = config.key("ruleGroups").index(index);
                let position = if group.get("position").is_some_and(Value::is_string) {
                    node.key("position").units()
                } else {
                    js::u("end")
                };
                let compiled = crate::schemas::data_rules::data_group(node, None)?;
                engine.register_compiled(compiled, &position)?;
            }
        }
        engine.init_blocks()?;
        engine.apply_validated_options(&validated, &config.key("rules").entries())?;
        Ok(engine)
    }

    /// Builds an engine from a JSON configuration; text that is not JSON fails with `MicroTypoConfigError` and `details.reason` `"json"`.
    pub fn from_json(config: &str, host: Host) -> Result<Self> {
        let raw = RawJson::parse(config)?;
        Self::new_node(Node::root(&raw.value, Some(&raw)), host)
    }

    fn init_blocks(&mut self) -> Result<()> {
        let format = self.input.format.clone();
        self.safe_blocks.set_case_sensitive(format == "mdx");
        let read_templates = if self.input.template == "off" { None } else { template_reader(&self.input.template) };
        let is_markdown = BLOCK_RENDER_FORMATS.contains(&format.as_str());
        let comments: Option<crate::protect::tag_spans::CommentFor> =
            is_markdown.then(|| Arc::new(markdown::markdown_comment_closer) as crate::protect::tag_spans::CommentFor);
        self.safe_blocks.configure_reading(
            comments.clone(),
            if matches!(format.as_str(), "text" | "html") {
                crate::protect::tag_spans::Reading::Document
            } else {
                crate::protect::tag_spans::Reading::Fragment
            },
            read_templates.clone(),
            matches!(format.as_str(), "markdown" | "frontmatter"),
            format != "plain",
            format == "xml",
        );
        self.safe_tags.set_comments(comments);
        self.safe_tags.set_xml_names(format == "xml");
        let mut html_owners: Vec<markdown::OwnerFor> = vec![Arc::new(markdown::element_reader_for)];
        if format == "xml" {
            html_owners.insert(
                0,
                Arc::new(|text, ctx, _| {
                    Ok(Box::new(crate::input::span_path::NextReader::new(crate::input::xml::xml_construct_reader(
                        text,
                        ctx.check_budget,
                    ))))
                }),
            );
            html_owners.insert(
                1,
                Arc::new(|text, ctx, _| {
                    let spans = crate::input::xml::scan_xml_space_preserve_whole(text, ctx.check_budget, true)?;
                    let mut cursor = 0;
                    Ok(Box::new(crate::input::span_path::NextReader::new(Box::new(move |from| {
                        while cursor < spans.len() && spans[cursor].0 < from {
                            cursor += 1;
                        }
                        Ok(spans.get(cursor).map(|&(a, b)| (a, b, b)))
                    }))))
                }),
            );
        }
        if !is_markdown && let Some(read) = template_body_reader(&self.input.template, true) {
            self.safe_blocks.add_opaque_scanner(
                "template-bodies",
                markdown::opaque_inline(None, Some(read), html_owners.clone(), false, true),
            );
        }
        if !is_markdown && format != "plain" {
            self.add_safe_elements()?;
        }
        if format == "markdown" || format == "mdx" {
            self.safe_blocks.add_scanner("md-leading-header", Arc::new(|t, _| Ok(scan_leading_header(t))));
        }
        if is_markdown {
            self.markdown_inline =
                Some(markdown::add_block_scanners(&mut self.safe_blocks, read_templates.clone(), format == "mdx"));
            self.add_safe_elements()?;
            let pragmas = (self.input.pragmas == "microtypo").then(|| pragma_scanner(true));
            markdown::add_inline_scanners(&mut self.safe_blocks, self.input.math, pragmas);
            self.add_safe_block("md-hard-break", "(?=[ ]{2,}[\\r\\n])", "(?<=[ ]{2,})(?=[\\r\\n])", true)?;
            self.apply_override(&js::u("text"), &js::u("paragraphs"), false)?;
            self.apply_override(&js::u("text"), &js::u("breakline"), false)?;
        }
        if format == "xml" {
            self.safe_blocks.add_scanner(
                "xml-space-preserve",
                Arc::new(|t, c| scan_xml_space_preserve_whole(t, c.check_budget, true)),
            );
            self.safe_blocks.add_scanner("xml-doctype", Arc::new(|t, c| scan_doctype(t, c.check_budget)));
            for (id, open, close) in XML_BLOCKS {
                self.add_safe_block(id, open, close, false)?;
            }
            self.add_safe_block("xml-entity", &format!("{CHARACTER_REFERENCE_HEAD}(?=;)"), ";", true)?;
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
        if !is_markdown && let Some(read) = read_templates {
            self.safe_blocks.add_opaque_scanner(
                &format!("tmpl-{}", self.input.template),
                markdown::opaque_inline(None, Some(read), html_owners, false, true),
            );
        }
        if self.input.pragmas == "microtypo" && html {
            self.safe_blocks.add_document_scanner("microtypo-pragma", pragma_scanner(false));
        }
        if let Some(scan) = localization_scanner(&self.input.placeholders, format == "xml") {
            self.safe_blocks.add_scanner("localization", scan);
        }
        if html {
            self.safe_blocks.set_tail(NOTG_RE.clone());
        }
        if html || format == "plain" {
            self.safe_blocks.set_indents(scan_line_indent);
        }
        Ok(())
    }

    fn add_safe_elements(&mut self) -> Result<()> {
        let xml = self.input.format == "xml";
        let extra = if matches!(self.input.format.as_str(), "text" | "html") {
            ["xmp", "noembed", "title"].as_slice()
        } else {
            &[]
        };
        for tag in DEFAULT_SAFE_TAGS.into_iter().chain(extra.iter().copied()) {
            self.safe_blocks.add_tag(
                tag,
                TagOptions {
                    renders: !NON_RENDERING_TAGS.contains(&tag),
                    nests: xml || !RAW_TEXT_TAGS.contains(&tag),
                    form_feed: !xml,
                    slash_opens: !xml && !FOREIGN_TAGS.contains(&tag),
                    typeset: tag == "title",
                },
            )?;
        }
        self.safe_blocks.add("span-notg", NOTG_START, NOTG_END, false)
    }

    /// Registers a rule group, as `registerRuleGroup(group, { name, position })` does: the name is `name`, else the group's `name`, else its `title`, else the call fails with `TypeError`; a group of the same name, ignoring case, is replaced.
    ///
    /// ```
    /// use microtypo_core::{MicroTypo, Position, Re, Replacement, Rule, RuleGroupDef};
    /// # let host = microtypo_conformance::HOST;
    ///
    /// let mut engine = MicroTypo::new(None, host)?;
    /// let rule = Rule::pattern("arrow", Re::try_new("->", "g").unwrap(), Replacement::tpl("→"));
    /// let group = RuleGroupDef { name: Some("arrows".into()), rules: vec![rule], ..RuleGroupDef::default() };
    /// engine.register_rule_group(group, None, Position::End)?;
    /// assert_eq!(engine.process("a -> b")?, "a → b");
    /// # Ok::<(), microtypo_core::Error>(())
    /// ```
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
        let position = match position {
            Position::Start => js::u("start"),
            Position::End => js::u("end"),
            Position::Spelled(s) => js::u(&s),
        };
        self.register_compiled(compiled, &position)
    }

    pub(crate) fn register_compiled(&mut self, compiled: CompiledGroup, position: &[u16]) -> Result<&mut Self> {
        let order = self.order_with(&compiled.name, position)?;
        self.groups.insert(compiled.name.clone(), compiled);
        self.order = order;
        Ok(self)
    }

    fn order_with(&self, name: &[u16], spelled: &[u16]) -> Result<Vec<JsString>> {
        let mut order: Vec<JsString> = self.order.iter().filter(|n| n.as_slice() != name).cloned().collect();
        if spelled == js::u("end") || spelled.is_empty() {
            order.push(name.to_owned());
            return Ok(order);
        }
        if spelled == js::u("start") {
            order.insert(0, name.to_owned());
            return Ok(order);
        }
        let found = POSITION_RE.exec(spelled).and_then(|m| {
            let after = m.s(1) == js::u("after").as_slice();
            let target = js::to_lower(m.s(2));
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
                    json_quote(&js::lossy(spelled))
                ),
                json!({ "position": js::lossy(spelled) }),
            )),
        }
    }

    /// The registered group of that name, ignoring case, as `getRuleGroup` describes it.
    pub fn get_rule_group(&self, name: &str) -> Option<RuleGroupInfo> {
        self.get_rule_group_utf16(&js::u(name))
    }

    pub fn get_rule_group_utf16(&self, name: &[u16]) -> Option<RuleGroupInfo> {
        self.groups.get(&js::to_lower(name)).map(describe_rule_group)
    }

    /// The registered groups in run order, as `listRuleGroups` describes them.
    pub fn list_rule_groups(&self) -> Vec<RuleGroupInfo> {
        self.order.iter().map(|n| describe_rule_group(&self.groups[n])).collect()
    }

    /// Leaves the content of every `tag` element as it is, as `addSafeTag` does.
    pub fn add_safe_tag(&mut self, tag: &str) -> Result<&mut Self> {
        let form_feed = self.input.format != "xml";
        self.safe_blocks.add_tag(tag, TagOptions { form_feed, ..TagOptions::default() })?;
        self.custom_protection = true;
        Ok(self)
    }

    /// Leaves everything from `open` to `close` as it is, as `addSafeBlock({ id, open, close, unsafeRegex })` does; with `unsafe_regex` the delimiters are regular expression sources.
    pub fn add_safe_block(&mut self, id: &str, open: &str, close: &str, unsafe_regex: bool) -> Result<&mut Self> {
        self.safe_blocks.add(id, open, close, unsafe_regex)?;
        self.custom_protection = true;
        Ok(self)
    }

    pub(crate) fn add_safe_block_sources(
        &mut self,
        id: &str,
        o: &str,
        c: &str,
        details: impl FnOnce() -> Value,
    ) -> Result<()> {
        self.safe_blocks.add_sources(id, o, c, details)?;
        self.custom_protection = true;
        Ok(())
    }

    /// Sets how hanging punctuation is laid out, as `setLayout`: `"style"`, `"class"` or `"both"`; another value fails with `MicroTypoConfigError`.
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

    pub(crate) fn set_layout_value(&mut self, value: &Value) -> Result<()> {
        match value.as_str() {
            Some(s) => self.set_layout(s).map(|_| ()),
            None => Err(Error::config(
                format!("layout must be \"style\", \"class\", or \"both\": {value}"),
                json!({ "layout": value }),
            )),
        }
    }

    pub(crate) fn set_prefix_value(&mut self, p: &Value) -> Result<&mut Self> {
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

    /// Sets the prefix of emitted class names, as `setPrefix`; a quote, angle bracket, `&` or whitespace in it fails with `MicroTypoConfigError`.
    pub fn set_prefix(&mut self, prefix: &str) -> Result<&mut Self> {
        self.set_prefix_value(&Value::String(prefix.to_owned()))
    }

    /// Sets the [`CycleHook`](crate::CycleHook), as the `onCycleLimit` option; `None` removes it.
    pub fn set_on_cycle_limit(&mut self, hook: Option<CycleHook>) {
        self.on_cycle_limit = hook;
    }

    /// Applies options to a built engine, as `applyOptions`; `ruleGroups`, or `input` or `presets` other than the constructor settled, fails with `MicroTypoConfigError`.
    pub fn apply_options(&mut self, options: Option<&Value>) -> Result<&mut Self> {
        self.apply_options_node(Node::root(options.unwrap_or(&Value::Null), None))
    }

    pub(crate) fn apply_options_node(&mut self, options: Node<'_>) -> Result<&mut Self> {
        let catalogue = self
            .groups
            .iter()
            .map(|(name, group)| (name.clone(), group.rules.iter().map(|rule| rule.id.clone()).collect()))
            .collect();
        let validated = validate_config_node(options.clone(), &catalogue)?;
        if options.value.as_object().is_some_and(|o| o.contains_key("ruleGroups")) {
            return Err(Error::config(
                "ruleGroups is settled by the constructor; use registerRuleGroup",
                json!({"option":"ruleGroups"}),
            ));
        }
        let asked_input = options.value.as_object().is_some_and(|o| o.contains_key("input"));
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
        self.apply_validated_options(&validated, &options.key("rules").entries())?;
        Ok(self)
    }

    fn apply_validated_options(&mut self, validated: &ValidatedConfig, rules: &[(JsString, Value)]) -> Result<()> {
        let intent = normalise_options(&validated.raw, &self.order, rules);
        for (key, value) in &intent.engine_settings {
            self.apply_engine_setting(key, value)?;
        }
        for s in &intent.settings {
            if let Some(g) = self.groups.get_mut(&s.group) {
                g.set_setting(&s.key, s.value.clone());
            }
        }
        for o in &intent.overrides {
            self.apply_override(&o.group, &o.selector, o.value)?;
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

    fn apply_override(&mut self, group: &[u16], selector: &[u16], value: bool) -> Result<()> {
        if group == js::u("*") {
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

    fn override_group(group: &mut CompiledGroup, selector: &[u16], value: bool) -> Result<()> {
        if selector == js::u("*") {
            for r in group.rules.iter() {
                group.enabled_override.insert(r.id.clone(), value);
            }
            return Ok(());
        }
        let re = wildcard_to_regex_units(selector)?;
        for r in group.rules.iter() {
            if re.test(&r.id) {
                group.enabled_override.insert(r.id.clone(), value);
            }
        }
        Ok(())
    }

    /// Typesets a text; a text the engine leaves unchanged comes back borrowed.
    ///
    /// The engine works on UTF-16 code units, which `maxInputLength` counts. [`process_utf16`] takes and returns them directly and keeps a lone surrogate in the output, which this method replaces with U+FFFD.
    ///
    /// [`process_utf16`]: MicroTypo::process_utf16
    ///
    /// ```
    /// use std::borrow::Cow;
    ///
    /// use microtypo_core::MicroTypo;
    /// # let host = microtypo_conformance::HOST;
    ///
    /// let mut engine = MicroTypo::new(None, host)?;
    /// assert_eq!(engine.process("Мы - здесь")?, "Мы\u{a0}— здесь");
    /// assert!(matches!(engine.process("Готово.")?, Cow::Borrowed(_)));
    /// # Ok::<(), microtypo_core::Error>(())
    /// ```
    pub fn process<'t>(&mut self, text: &'t str) -> Result<Cow<'t, str>> {
        let units = js::u(text);
        let result = self.process_utf16(&units)?;
        Ok(if result == units { Cow::Borrowed(text) } else { Cow::Owned(js::lossy(&result)) })
    }

    /// Typesets a text of UTF-16 code units; lone surrogates pass through.
    pub fn process_utf16(&mut self, text: &[u16]) -> Result<JsString> {
        self.process_with_diagnostics(text, crate::ProcessOptions::default()).map(|p| p.text)
    }

    /// Typesets a text of UTF-16 code units and reports the changed ranges, as `process(text, { changes: true })`.
    pub fn process_with_changes(&mut self, text: &[u16]) -> Result<Processed> {
        self.process_with_diagnostics(text, crate::ProcessOptions { changes: true, ..crate::ProcessOptions::default() })
            .map(|output| Processed { text: output.text, changes: output.changes.expect("requested changes") })
    }

    /// Typesets a text of UTF-16 code units and reports what `options` requests, as `process(text, options)`.
    pub fn process_with_diagnostics(
        &mut self,
        text: &[u16],
        options: crate::ProcessOptions,
    ) -> Result<crate::Diagnostics> {
        if text.len() as f64 > self.max_input_length {
            return Err(Error::input(
                format!("Input length {} exceeds maxInputLength ({})", text.len(), js_number(self.max_input_length)),
                json!({ "length": text.len(), "max": num_value(self.max_input_length) }),
            ));
        }
        let now_ms = self.host.now_ms;
        let start = now_ms();
        let check = budget_check(self.max_processing_ms, now_ms, start);
        self.trace_log = options.trace.then(|| Arc::new(std::sync::Mutex::new(Vec::new())));
        self.trace_map = None;
        self.trace_escaped = None;
        self.reserved_possible = self.input.format == "json" || has_reserved(text);
        let outcome = js::regex::with_budget(now_ms, start, self.max_processing_ms, || {
            let bom = usize::from(text.first() == Some(&0xFEFF));
            let mut result = self.typeset(&text[bom..], &check, bom)?;
            if bom == 1 {
                result.insert(0, 0xFEFF);
            }
            let as_given = self.ignored_file || self.safe_sequences.left_in(&result);
            if as_given {
                result = text.to_vec();
            }
            if FILE_FORMATS.contains(&self.input.format.as_str()) {
                result = restore_file_tail(text, result);
            }
            check("restore")?;
            let trace = self.trace_log.as_ref().map(|log| {
                if as_given {
                    Vec::new()
                } else {
                    std::mem::take(&mut *log.lock().unwrap_or_else(std::sync::PoisonError::into_inner))
                }
            });
            let changes = options.changes.then(|| changed_ranges(text, &result, &check)).transpose()?;
            let edits =
                options.edits.then(|| crate::shared::diff::replacement_edits(text, &result, &check)).transpose()?;
            Ok(crate::Diagnostics { text: result, trace, changes, edits })
        });
        self.trace_log = None;
        self.trace_map = None;
        self.trace_escaped = None;
        outcome
    }

    fn reset_vaults(&mut self) {
        self.safe_blocks.reset();
        self.safe_sequences.reset();
        self.safe_tags.reset();
    }

    fn source_map(
        &self,
        text: &[u16],
        offset: usize,
        cb: CheckBudget<'_>,
    ) -> Result<Option<Arc<crate::shared::provenance::SourceMap>>> {
        self.trace_log.as_ref().map(|_| crate::shared::provenance::SourceMap::new(text, offset, cb)).transpose()
    }

    fn typeset(&mut self, text: &[u16], cb: CheckBudget<'_>, offset: usize) -> Result<JsString> {
        self.ignored_file = false;
        self.bound = Bindings::default();
        self.reset_vaults();
        self.line_style = if FILE_FORMATS.contains(&self.input.format.as_str())
            && self.input.format != "json"
            && text.contains(&0x0D)
        {
            Some(file_line_style(text, cb)?)
        } else {
            None
        };
        match self.input.format.as_str() {
            "json" => self.process_json(text, cb, offset),
            "toml" => self.typeset_structured(text, scan_toml, cb, offset),
            "yaml" => self.typeset_structured(text, scan_yaml, cb, offset),
            "xml" => {
                validate_xml(text, cb)?;
                let edges = edge_whitespace(text);
                let core = &text[edges.start..edges.end];
                let map = self.source_map(core, offset + edges.start, cb)?;
                let typed = self.run_pipeline(core, cb, Fragment::new(Destination::Xml, Trim::None), map)?;
                Ok(cat!(&text[..edges.start], typed, &text[edges.end..]))
            }
            "frontmatter" => self.typeset_frontmatter(text, cb, offset),
            "markdown" | "mdx" => {
                let map = self.source_map(text, offset, cb)?;
                self.run_pipeline(text, cb, Fragment::new(Destination::Html, Trim::BlankLines), map)
            }
            _ => {
                let (from, to) = leading_indent(text, read_scalar_at, cb)?;
                let spliced = cat!(&text[..from], &text[to..]);
                let map = self.source_map(text, offset, cb)?.map(|map| map.fork(&spliced, cb)).transpose()?;
                let body = self.run_pipeline(
                    &spliced,
                    cb,
                    Fragment {
                        destination: if self.input.format == "plain" { Destination::Plain } else { Destination::Html },
                        trim: Trim::Js,
                        source_trim: Trim::Js,
                        indents: true,
                        first_indent: if to < text.len() { &text[from..to] } else { &[] },
                    },
                    map,
                )?;
                Ok(body)
            }
        }
    }

    fn typeset_frontmatter(&mut self, text: &[u16], cb: CheckBudget<'_>, offset: usize) -> Result<JsString> {
        let body_fragment = Fragment::new(Destination::Html, Trim::BlankLines);
        match split_frontmatter(text) {
            None => {
                let map = self.source_map(text, offset, cb)?;
                self.run_pipeline(text, cb, body_fragment, map)
            }
            Some(Frontmatter::Malformed) => Err(Error::input(
                "Malformed frontmatter: opening delimiter has no matching closing delimiter",
                json!({}),
            )),
            Some(Frontmatter::Split { toml, header_start, header_end, body_start }) => {
                let header = &text[header_start..header_end];
                let typed_header = if toml {
                    self.typeset_structured(header, scan_toml, cb, offset + header_start)?
                } else {
                    self.typeset_structured(header, scan_yaml, cb, offset + header_start)?
                };
                let body = &text[body_start..];
                let boundary = crate::input::frontmatter::body_boundary(body);
                let map = self.source_map(body, offset + body_start, cb)?;
                let typed_body = self.run_pipeline(body, cb, body_fragment, map)?;
                Ok(cat!(
                    &text[..header_start],
                    typed_header,
                    &text[header_end..body_start],
                    &body[..boundary],
                    typed_body
                ))
            }
        }
    }

    fn process_json(&mut self, text: &[u16], cb: CheckBudget<'_>, offset: usize) -> Result<JsString> {
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
            let map = this
                .trace_log
                .as_ref()
                .map(|_| {
                    crate::shared::provenance::json_source_map(
                        &text[span.start..span.end],
                        offset + span.start,
                        &decoded,
                        cb,
                    )
                })
                .transpose()?;
            let typed = this.typeset_value(&decoded, cb, map)?;
            Ok(js::json::stringify(&typed))
        })
    }

    fn typeset_value(
        &mut self,
        value: &[u16],
        cb: CheckBudget<'_>,
        map: Option<Arc<crate::shared::provenance::SourceMap>>,
    ) -> Result<JsString> {
        let edges = edge_whitespace(value);
        if edges.start == edges.end {
            return Ok(value.to_vec());
        }
        let core = &value[edges.start..edges.end];
        if self.can_skip_value(core) {
            cb("data-value")?;
            return Ok(value.to_vec());
        }
        let map = map.map(|map| map.slice(edges.start, edges.end, cb)).transpose()?;
        let typed = self.run_pipeline(core, cb, Fragment::data_value(), map)?;
        Ok(cat!(&value[..edges.start], js::trim(&typed), &value[edges.end..]))
    }

    fn features_on(&self) -> bool {
        self.on_cycle_limit.is_none() && self.groups.values().all(|group| group.builtin)
    }

    fn can_skip_value(&self, value: &[u16]) -> bool {
        if self.custom_protection || !self.features_on() {
            return false;
        }
        let mask = text_mask(value);
        mask & F::SOURCE == 0
            && !self
                .order
                .iter()
                .any(|name| self.groups.get(name).is_some_and(|group| can_run_rule_group(group, mask, false)))
    }

    fn typeset_structured(
        &mut self,
        text: &[u16],
        scan: fn(&[u16], CheckBudget<'_>) -> Result<Vec<LocatedSpan>>,
        cb: CheckBudget<'_>,
        offset: usize,
    ) -> Result<JsString> {
        let spans: Vec<LocatedSpan> = scan(text, cb)?.into_iter().filter(|s| s.eligible).collect();
        let selected = select_value_spans(&spans, &self.select, cb)?;
        self.splice_spans(text, &selected, &mut |this, span| {
            let quote = &span.delim;
            let interior = &text[span.start + quote.len()..span.end - quote.len()];
            let checkpoint = this.trace_log.as_ref().map(|log| {
                log.lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .iter()
                    .map(|entry| (entry.count, entry.ranges.len()))
                    .collect::<Vec<_>>()
            });
            let map = this.source_map(interior, offset + span.start + quote.len(), cb)?;
            let typed = this.typeset_value(interior, cb, map)?;
            if (!quote.is_empty() && js::includes(&typed, quote)) || span.guard.is_some_and(|guard| !guard(&typed)) {
                if let (Some(checkpoint), Some(log)) = (checkpoint, &this.trace_log) {
                    let mut entries = log.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                    entries.truncate(checkpoint.len());
                    for (entry, (count, length)) in entries.iter_mut().zip(checkpoint) {
                        entry.count = count;
                        entry.ranges.truncate(length);
                    }
                }
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

    fn run_pipeline(
        &mut self,
        text: &[u16],
        cb: CheckBudget<'_>,
        fragment: Fragment<'_>,
        map: Option<Arc<crate::shared::provenance::SourceMap>>,
    ) -> Result<JsString> {
        self.run_pipeline_inherited(text, cb, fragment, None, map)
    }

    fn run_pipeline_inherited(
        &mut self,
        text: &[u16],
        cb: CheckBudget<'_>,
        fragment: Fragment<'_>,
        inherited: Inherited<'_>,
        map: Option<Arc<crate::shared::provenance::SourceMap>>,
    ) -> Result<JsString> {
        let escaped = if self.reserved_possible { escape_reserved(text)? } else { None };
        if self.trace_log.is_none() {
            return match escaped {
                None => self.typeset_fragment(text, cb, fragment, inherited),
                Some(escaped) => {
                    self.typeset_fragment(&escaped.text, cb, fragment, inherited).map(|typed| escaped.unescape(&typed))
                }
            };
        }
        let escaped = escaped.map(Arc::new);
        let previous_map = std::mem::replace(&mut self.trace_map, map);
        let previous_escaped = std::mem::replace(&mut self.trace_escaped, escaped.clone());
        let outcome = match escaped {
            None => self.typeset_fragment(text, cb, fragment, inherited),
            Some(e) => self.typeset_fragment(&e.text, cb, fragment, inherited).map(|typed| e.unescape(&typed)),
        };
        self.trace_map = previous_map;
        self.trace_escaped = previous_escaped;
        outcome
    }

    fn is_markdown(&self) -> bool {
        BLOCK_RENDER_FORMATS.contains(&self.input.format.as_str())
    }

    fn builtin_rule_on(&self, group: &str, id: &str) -> bool {
        self.groups.get(js::u(group).as_slice()).is_some_and(|owner| owner.builtin && is_rule_on(owner, id))
            && self.order.iter().any(|name| js::eq(name, group))
    }

    fn session_bindings(&mut self, destination: Destination) -> Bindings {
        let space = |id: &str| self.builtin_rule_on("space", id);
        let space_collapse = space("collapse_spaces");
        let trims_line_end = space("trim_line_end");
        let inline_padding = crate::rules::quote_state::InlinePadding {
            trim_before_punctuation: space("trim_before_punctuation"),
            space_after_comma: space("space_after_comma"),
        };
        let padding_strip = space("strip_quote_padding");
        let initials_dots = self.builtin_rule_on("nobr", "initials_dots");
        let open = self.builtin_rule_on("quote", "open");
        let close = self.builtin_rule_on("quote", "close");
        let read_foot = self.builtin_rule_on("number", "prime");
        let quote_tags = destination == Destination::Html && self.input.format != "plain";
        let line_blocks = self.is_markdown();
        let mut bound = std::mem::take(&mut self.bound);
        bound.space_collapse = space_collapse;
        bound.trims_line_end = trims_line_end;
        bound.initials_dots = initials_dots;
        bound.quote_tags = quote_tags;
        if quote_tags {
            bound.breaks = None;
        }
        bound.inline_names = destination != Destination::Xml;
        bound.line_blocks = line_blocks;
        bound.inline_padding = inline_padding;
        bound.quote.padding_strip = padding_strip;
        bound.quote.open = open;
        bound.quote.close = close;
        bound.quote.read_foot = read_foot;
        bound
    }

    fn trim_edges(&self, trim: Trim, text: &[u16]) -> JsString {
        match trim {
            Trim::None => text.to_vec(),
            Trim::Js => js::trim(text).to_vec(),
            Trim::BlankLines => trim_blank_lines(text, &|line| js::trim(&self.restore_protected(line)).is_empty()),
        }
    }

    fn restore_protected(&self, text: &[u16]) -> JsString {
        let result = self.safe_tags.restore_iblocks(text.to_vec());
        let result = self.safe_tags.restore(result);
        let result = self.safe_blocks.restore(result);
        self.safe_sequences.restore(result)
    }

    fn render_trace(&self, text: JsString, mapping: &mut crate::shared::provenance::RestoreMap) -> JsString {
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

    fn typeset_fragment(
        &mut self,
        text: &[u16],
        cb: CheckBudget<'_>,
        fragment: Fragment<'_>,
        inherited: Inherited<'_>,
    ) -> Result<JsString> {
        let eff_html = fragment.destination == Destination::Html && self.html;
        self.safe_sequences.reset_frame();
        let inherited_source;
        let text = if let Some(accepts) = inherited {
            static FOREIGN: LazyLock<Re> = LazyLock::new(|| {
                let mut patterns = vec![crate::protect::placeholders::PLACEHOLDER_TOKEN.to_string()];
                patterns.extend(SafeSequences::restore_regexes().iter().map(|r| r.source().to_owned()));
                Re::new(&patterns.join("|"), "g")
            });
            inherited_source = self.safe_sequences.protect_tokens(text, &FOREIGN, accepts)?;
            inherited_source.as_slice()
        } else {
            text
        };
        let nested = self.builtin_rule_on("quote", "nested");
        let reads_inches = nested
            && self.groups.get(js::u("quote").as_slice()).is_some_and(|quote| {
                quote.view.settings.get("convertInches").is_none_or(|value| value != &Value::Bool(false))
            });
        let inline = if fragment.destination == Destination::Html { self.markdown_inline.clone() } else { None };
        let mdx = self.input.format == "mdx";
        let inline_first = inline.is_some() && text.contains(&0x40) && text.contains(&0x60);
        let mut result = if inline_first {
            self.safe_sequences.protect_address_links(text)?
        } else {
            protect_sequences(
                &mut self.safe_sequences,
                text,
                inline.as_ref(),
                self.safe_blocks.elements(),
                mdx,
                cb,
                reads_inches,
                None,
            )?
        };
        let titles = fragment.destination == Destination::Html && contains_title_tag(&result);
        let mut title_workers: [Option<Box<Self>>; 2] = [None, None];
        let body_glyphs = if self.entities && titles { Some(self.optional_glyphs()) } else { None };
        {
            let source = WorkerSource {
                host: self.host,
                max_processing_ms: self.max_processing_ms,
                input: &self.input,
                groups: &self.groups,
                order: &self.order,
                engine_settings: &self.engine_settings,
                on_cycle_limit: &self.on_cycle_limit,
            };
            let cached = &mut self.attribute_workers;
            let safe_tags = &self.safe_tags;
            let sequences = std::cell::RefCell::new(&mut self.safe_sequences);
            let resolve = |value: &[u16]| sequences.borrow().restore(value.to_vec());
            let mut typeset_body = |body: &[u16], blocks: &crate::protect::blocks::SafeBlocks| -> Result<JsString> {
                let edges = edge_whitespace(body);
                if !titles || edges.start == edges.end {
                    return Ok(body.to_vec());
                }
                let format = if TAG_START_RE.test(body) { WorkerFormat::Text } else { WorkerFormat::Plain };
                let worker = match &mut title_workers[format as usize] {
                    Some(worker) => worker,
                    slot @ None => slot.insert(source.worker(cached[format as usize].take(), format)?),
                };
                let inherited = |value: &[u16]| {
                    let restored = safe_tags.restore(safe_tags.restore_iblocks(value.to_vec()));
                    sequences.borrow().restore(blocks.restore(restored)) != value
                };
                worker.reset_vaults();
                worker.line_style = if body.contains(&13) { Some(file_line_style(body, cb)?) } else { None };
                let typed = worker.run_pipeline_inherited(
                    &body[edges.start..edges.end],
                    cb,
                    Fragment::data_value(),
                    Some(&inherited),
                    None,
                )?;
                let typed = cat!(&body[..edges.start], js::trim(&typed), &body[edges.end..]);
                Ok(match &body_glyphs {
                    Some(glyphs) => {
                        let encoded = unicode_to_entities(&typed);
                        if glyphs.is_empty() {
                            encoded
                        } else {
                            crate::shared::glyphs::encode_optional_glyphs(&encoded, glyphs)
                        }
                    }
                    None => typed,
                })
            };
            let mut after = |hidden: JsString, cuts: &[usize], elements: &crate::protect::blocks::ElementSet| {
                protect_sequences(
                    &mut sequences.borrow_mut(),
                    &hidden,
                    inline.as_ref(),
                    elements,
                    mdx,
                    cb,
                    reads_inches,
                    Some(cuts),
                )
            };
            result = self.safe_blocks.protect(
                &result,
                cb,
                &resolve,
                fragment.destination == Destination::Html,
                if inline_first { Some(&mut after) } else { None },
                Some(&mut typeset_body),
            )?;
        }
        let [plain_worker, text_worker] = title_workers;
        if let Some(worker) = plain_worker {
            self.keep_attribute_worker(WorkerFormat::Plain, worker);
        }
        if let Some(worker) = text_worker {
            self.keep_attribute_worker(WorkerFormat::Text, worker);
        }
        cb("safeBlocks.protect")?;
        if self.safe_blocks.ignored_file() {
            self.ignored_file = true;
            return Ok(text.to_vec());
        }
        if self.input.format != "plain" {
            result = self.protect_notg(&result)?;
        }
        cb("protectNotg")?;
        if fragment.destination == Destination::Html
            && !self.input.attributes.is_empty()
            && matches!(self.input.format.as_str(), "text" | "html" | "markdown" | "frontmatter")
        {
            result = self.typeset_attributes(&result, cb)?;
        }
        if self.input.format != "plain" {
            result = self.safe_tags.protect(&result)?;
        }
        cb("safeTags.protect")?;
        let line_source = self.line_style.map(|_| result.clone());
        result = NEWLINES_RE.replace_tpl_owned(CRLF_RE.replace_tpl_owned(result, &LF), &LF_LF);
        cb("newlineInvariant")?;
        let mut bound = self.session_bindings(fragment.destination);
        if eff_html && bound.quote_tags && self.is_markdown() && self.builtin_rule_on("text", "paragraphs") {
            bound.edges = Some(EdgeReader::new(
                matches!(self.input.format.as_str(), "markdown" | "mdx"),
                self.input.format == "mdx",
            ));
        }
        {
            let blocks = &self.safe_blocks;
            let sequences = &self.safe_sequences;
            let Bindings { quote_tags, inline_names, line_blocks, inline_padding, breaks, edges, quote, .. } =
                &mut bound;
            let edges = edges.as_mut().map(|reader| EdgeView { reader, blocks, sequences });
            let mut cx = QuoteTags::session(
                &mut self.safe_tags,
                *quote_tags,
                *inline_names,
                line_blocks.then_some(blocks),
                *inline_padding,
                breaks,
                edges,
                Some(cb),
            );
            result = decode_special_chars(result);
            if quote.open {
                let close = quote.close;
                result = prepare_authored_quotes(result, &mut cx, quote, close, reads_inches)?;
            }
            result = canonical_quote_roles(result, &mut cx, Some(cb), nested)?;
        }
        let optional_glyphs = self.optional_glyphs();
        if !optional_glyphs.is_empty() {
            result = crate::shared::glyphs::decode_optional_glyphs(
                &result,
                &optional_glyphs,
                fragment.destination == Destination::Xml,
            );
        }
        cb("clearSpecialChars")?;
        if fragment.source_trim != Trim::None {
            let view = self.safe_blocks.source_view(&result);
            let trimmed = self.trim_edges(fragment.source_trim, &view);
            result = self.safe_blocks.vault_trimmed(&result, &view, trimmed, cb)?;
            cb("sourceEdges")?;
        }
        if fragment.indents {
            result = SafeBlocks::lift_indents(&result);
        }
        {
            let inline = (fragment.destination != Destination::Xml).then(|| {
                let mint_forms = self
                    .order
                    .iter()
                    .filter_map(|name| self.groups.get(name).filter(|group| group.builtin))
                    .map(|group| group.view.clone())
                    .collect();
                let year_range =
                    self.builtin_rule_on("date", "year_range").then(|| &*crate::rules::date::SOURCE_YEAR_RANGE_RE);
                let gap_rules = crate::inline_lift::YearGapRules {
                    range: self.builtin_rule_on("number", "en_range"),
                    collapse: self.builtin_rule_on("space", "collapse_spaces"),
                    bind: self.builtin_rule_on("nobr", "nbsp_after_number"),
                };
                InlineCx::new(
                    fragment.destination == Destination::Html && self.is_markdown(),
                    mint_forms,
                    year_range,
                    gap_rules,
                )
            });
            let features_on = self.features_on();
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
                safe_sequences: &mut self.safe_sequences,
                prefix: &self.prefix,
                budget: None,
                markdown: BLOCK_RENDER_FORMATS.contains(&self.input.format.as_str()).then(|| {
                    MarkdownSource::new(
                        self.input.format == "markdown" || self.input.format == "mdx",
                        self.input.format == "mdx",
                    )
                }),
                authored_closers: None,
                inline,
                features: features_on.then(Features::default),
                trace: self
                    .trace_map
                    .as_ref()
                    .zip(self.trace_log.as_ref())
                    .map(|(map, log)| crate::shared::rule_trace::RuleTrace::new(map.clone(), log.clone())),
                trace_projection: None,
                trace_escaped: self.trace_escaped.clone(),
                bound,
            };
            if let Some(date) = self.groups.get(js::u("date").as_slice())
                && date.builtin
                && self.order.iter().any(|name| js::eq(name, "date"))
                && is_rule_on(date, "nowrap_date")
            {
                result = run_year_marks(result, date, &mut ctx)?;
            }
            if eff_html
                && let Some(nobr) = self.groups.get(js::u("nobr").as_slice())
                && nobr.builtin
                && self.order.iter().any(|name| js::eq(name, "nobr"))
                && is_rule_on(nobr, "nowrap_phone_extended")
            {
                result = crate::rule_runner::run_step(result, nobr, "nowrap_phone_extended", &mut ctx)?;
            }
            let etc = self.groups.get(js::u("other").as_slice());
            let wrapper_spaces: Vec<&str> = etc
                .filter(|etc| etc.builtin && self.order.iter().any(|name| js::eq(name, "other")))
                .map_or_default(|etc| {
                    etc.rules
                        .iter()
                        .filter_map(|rule| {
                            ["strip_nbsp_in_nowrap", "nbsp_in_nowrap"].into_iter().find(|id| js::eq(&rule.id, id))
                        })
                        .filter(|id| is_rule_on(etc, id))
                        .collect()
                });
            let mut etc_ran = false;
            let reread = |owner: &CompiledGroup,
                          id: &str,
                          before: JsString,
                          etc_ran: bool,
                          ctx: &mut Ctx<'_>|
             -> Result<JsString> {
                let (Some(etc), true) = (etc, etc_ran && !wrapper_spaces.is_empty()) else {
                    return crate::rule_runner::run_step(before, owner, id, ctx);
                };
                let snapshot = before.clone();
                let mut next = crate::rule_runner::run_step(before, owner, id, ctx)?;
                if next != snapshot {
                    for rule in &wrapper_spaces {
                        next = crate::rule_runner::run_step(next, etc, rule, ctx)?;
                    }
                }
                Ok(next)
            };
            for name in &self.order {
                let Some(group) = self.groups.get(name) else { continue };
                if fragment.indents && js::eq(name, "text") {
                    result = self.safe_blocks.settle_indents(&result);
                }
                if js::eq(name, "abbr")
                    && let Some(other) = self.groups.get(js::u("other").as_slice())
                    && other.builtin
                    && self.order.iter().any(|name| js::eq(name, "other"))
                    && is_rule_on(other, "split_triads")
                {
                    result = crate::rule_runner::run_step(result, other, "split_triads", &mut ctx)?;
                }
                result = run_group(group, result, &mut ctx)?;
                etc_ran |= js::eq(name, "other");
                let reads = DEPENDENT_READS.iter().find(|(reader, _)| js::eq(name, reader)).map_or(&[][..], |r| r.1);
                for &(owner_name, id) in reads {
                    if let Some(owner) = self.groups.get(js::u(owner_name).as_slice())
                        && owner.builtin
                        && self.order.iter().any(|name| js::eq(name, owner_name))
                        && is_rule_on(owner, id)
                        && (eff_html || !owner.rules.iter().any(|rule| js::eq(&rule.id, id) && rule.html_only))
                    {
                        result = reread(owner, id, result, etc_ran, &mut ctx)?;
                    }
                }
                cb(&group.budget_label)?;
            }
            if fragment.indents {
                result = self.safe_blocks.settle_indents(&result);
            }
            for (name, id) in [("abbr", "nbsp_part"), ("nobr", "nbsp_last_word")] {
                if let Some(owner) = self.groups.get(js::u(name).as_slice())
                    && owner.builtin
                    && self.order.iter().any(|group| js::eq(group, name))
                    && is_rule_on(owner, id)
                {
                    result = reread(owner, id, result, etc_ran, &mut ctx)?;
                }
            }
            self.bound = std::mem::take(&mut ctx.bound);
        }
        if self.entities {
            match fragment.destination {
                Destination::Xml => result = unicode_to_xml_entities(&result),
                Destination::Html => result = unicode_to_entities(&result),
                Destination::DataValue | Destination::Plain => {}
            }
            if matches!(fragment.destination, Destination::Xml | Destination::Html) && !optional_glyphs.is_empty() {
                result = crate::shared::glyphs::encode_optional_glyphs(&result, &optional_glyphs);
            }
        }
        if let Some(line_source) = line_source {
            result = restore_file_endings(&line_source, result, self.line_style.expect("file line style"), cb)?;
        }
        result = self.safe_tags.restore_iblocks(result);
        let paragraph_open = &crate::protect::placeholders::PARAGRAPH_OPEN;
        let generated_first =
            result.starts_with(paragraph_open) && !result.starts_with(&cat!(*paragraph_open, "\u{e000}L"));
        result = restore_paragraphs(result);
        result = self.safe_tags.restore(result);
        if eff_html {
            result = unwrap_blocks(result);
        }
        if fragment.trim != Trim::None {
            result = self.trim_edges(fragment.trim, &result);
        }
        result = self.safe_blocks.restore(result);
        result = self.safe_sequences.restore(result);
        if !fragment.first_indent.is_empty() && !result.is_empty() {
            return Ok(if generated_first && result.starts_with(&js::u("<p>")) {
                cat!("<p>", fragment.first_indent, &result[3..])
            } else {
                cat!(fragment.first_indent, result)
            });
        }
        Ok(result)
    }

    fn optional_glyphs(&self) -> Vec<u16> {
        let mut optional_glyphs = Vec::new();
        for &(name, id, glyphs) in crate::shared::glyphs::OPTIONAL_GLYPH_RULES {
            if self.groups.get(js::u(name).as_slice()).is_some_and(|group| group.builtin && is_rule_on(group, id))
                && self.order.iter().any(|group| js::eq(group, name))
            {
                for &glyph in glyphs {
                    if !optional_glyphs.contains(&glyph) {
                        optional_glyphs.push(glyph);
                    }
                }
            }
        }
        optional_glyphs
    }

    fn worker_source(&self) -> WorkerSource<'_> {
        WorkerSource {
            host: self.host,
            max_processing_ms: self.max_processing_ms,
            input: &self.input,
            groups: &self.groups,
            order: &self.order,
            engine_settings: &self.engine_settings,
            on_cycle_limit: &self.on_cycle_limit,
        }
    }

    fn keep_attribute_worker(&mut self, format: WorkerFormat, mut worker: Box<Self>) {
        worker.reset_vaults();
        worker.ignored_file = false;
        worker.line_style = None;
        worker.trace_log = None;
        worker.trace_map = None;
        worker.trace_escaped = None;
        worker.bound = Bindings::default();
        self.attribute_workers[format as usize] = Some((self.max_processing_ms.to_bits(), worker));
    }

    fn typeset_attributes(&mut self, text: &[u16], cb: CheckBudget<'_>) -> Result<JsString> {
        let mut cached = self.attribute_workers[WorkerFormat::Plain as usize].take();
        let mut worker: Option<Box<Self>> = None;
        let inherited = |value: &[u16]| self.restore_protected(value) != value;
        let mut source_map = None;
        let mut edits = self.trace_log.as_ref().map(|_| Vec::new());
        let result = crate::input::attributes::typeset_attributes(
            text,
            &self.input.attributes,
            &mut |value, source_start, source_end| {
                let edges = edge_whitespace(value);
                if edges.start == edges.end {
                    return Ok(value.to_vec());
                }
                if source_map.is_none()
                    && let Some(map) = &self.trace_map
                {
                    source_map = Some(crate::shared::provenance::represented_map(
                        map.clone(),
                        text,
                        None,
                        None,
                        |text, mapping| self.render_trace(text, mapping),
                        cb,
                    )?);
                }
                let map = source_map
                    .as_ref()
                    .map(|map| map.slice(source_start, source_end, cb)?.slice(edges.start, edges.end, cb))
                    .transpose()?;
                let worker = match &mut worker {
                    Some(worker) => worker,
                    slot @ None => {
                        let worker = slot.insert(self.worker_source().worker(cached.take(), WorkerFormat::Plain)?);
                        worker.trace_log.clone_from(&self.trace_log);
                        worker
                    }
                };
                worker.reset_vaults();
                worker.line_style = if value.contains(&13) { Some(file_line_style(value, cb)?) } else { None };
                let typed = worker.run_pipeline_inherited(
                    &value[edges.start..edges.end],
                    cb,
                    Fragment::data_value(),
                    Some(&inherited),
                    map,
                )?;
                Ok(cat!(&value[..edges.start], js::trim(&typed), &value[edges.end..]))
            },
            &|name| self.safe_blocks.protects_tag(name),
            cb,
            edits.as_mut(),
        )?;
        if let Some(map) = source_map {
            self.trace_map = Some(map.edited(&result, edits.as_deref().unwrap_or_default(), cb)?);
        }
        match worker {
            Some(worker) => self.keep_attribute_worker(WorkerFormat::Plain, worker),
            None => self.attribute_workers[WorkerFormat::Plain as usize] = cached,
        }
        Ok(result)
    }
}

#[derive(Clone, Copy)]
enum WorkerFormat {
    Plain,
    Text,
}

struct WorkerSource<'a> {
    host: Host,
    max_processing_ms: f64,
    input: &'a InputConfig,
    groups: &'a HashMap<JsString, CompiledGroup>,
    order: &'a Vec<JsString>,
    engine_settings: &'a Map<String, Value>,
    on_cycle_limit: &'a Option<CycleHook>,
}

impl WorkerSource<'_> {
    fn worker(&self, cached: Option<(u64, Box<MicroTypo>)>, format: WorkerFormat) -> Result<Box<MicroTypo>> {
        let cached = cached.filter(|(built_for, _)| *built_for == self.max_processing_ms.to_bits());
        let mut worker = if let Some((_, worker)) = cached {
            worker
        } else {
            let format = match format {
                WorkerFormat::Plain => "plain",
                WorkerFormat::Text => "text",
            };
            let mut worker = MicroTypo::new(
                Some(&json!({ "presets": false, "html": false, "entities": false,
                    "maxProcessingMs": self.max_processing_ms,
                    "input": { "format": format, "template": self.input.template, "placeholders": self.input.placeholders }
                })),
                self.host,
            )?;
            worker.add_safe_block("attribute-entity", &format!("{CHARACTER_REFERENCE_HEAD}(?=;)"), ";", true)?;
            Box::new(worker)
        };
        worker.groups.clone_from(self.groups);
        worker.order.clone_from(self.order);
        worker.engine_settings.clone_from(self.engine_settings);
        worker.on_cycle_limit.clone_from(self.on_cycle_limit);
        Ok(worker)
    }
}

/// Typesets a text with a one-off engine, as the reference `microtypo(text, options)`.
pub fn microtypo(text: &str, config: Config, host: Host) -> Result<String> {
    MicroTypo::with_config(config, host)?.process(text).map(Cow::into_owned)
}

impl fmt::Debug for MicroTypo {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("MicroTypo").field("groups", &self.order).finish_non_exhaustive()
    }
}
