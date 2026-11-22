use std::collections::HashMap;
use std::sync::{Arc, LazyLock};

use serde_json::json;

use crate::errors::{Error, Result};
#[cfg(test)]
use crate::input::span_path::collect_owned;
use crate::input::span_path::{OwnedSpan, ReaderFor, SpanReader};
use crate::js::{self, JsString, Re};
use crate::protect::blocks::CheckBudget;
#[cfg(test)]
use crate::protect::blocks::{OpaqueScanner, ScanCtx};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    Code,
    HashComments,
    Comment,
    DashedComment,
}
use Mode::{Code, Comment, DashedComment, HashComments};
type Pairs = &'static [(&'static str, &'static str, Mode)];
fn delimiters(name: &str) -> Option<Pairs> {
    const HANDLEBARS: Pairs = &[
        ("{{{{", "}}}}", Code),
        ("{{{", "}}}", Code),
        ("{{!--", "}}", DashedComment),
        ("{{~!--", "}}", DashedComment),
        ("{{!", "}}", Comment),
        ("{{~!", "}}", Comment),
        ("{{", "}}", Code),
    ];
    const MUSTACHE: Pairs = &[("{{{", "}}}", Code), ("{{!", "}}", Comment), ("{{", "}}", Code)];
    const TWIG: Pairs = &[("{%", "%}", Code), ("{{", "}}", Code), ("{#", "#}", Comment)];
    const LIQUID: Pairs = &[("{%", "%}", HashComments), ("{{", "}}", Code)];
    const ERB: Pairs = &[("<%#", "%>", Comment), ("<%", "%>", HashComments)];
    Some(match name {
        "handlebars" => HANDLEBARS,
        "mustache" => MUSTACHE,
        "twig" | "jinja" | "nunjucks" => TWIG,
        "liquid" => LIQUID,
        "erb" => ERB,
        _ => return None,
    })
}
const MAX_TEMPLATE_SCAN: usize = 8192;
static HB_RAW: LazyLock<Re> = LazyLock::new(|| Re::new(r"^\s*raw(?:\s|$)", ""));
static BODY_HEAD: LazyLock<Re> = LazyLock::new(|| Re::new(r"^-?\s*(raw|comment)\s*-?$", ""));
static HB_END: LazyLock<Re> = LazyLock::new(|| Re::new(r"\{\{\{\{\s*\/raw\s*\}\}\}\}", "g"));
static RAW_END: LazyLock<Re> = LazyLock::new(|| Re::new(r"\{%[-]?[ \t\r\n]*endraw[ \t\r\n]*[-]?%\}", "g"));
static COMMENT_END: LazyLock<Re> = LazyLock::new(|| Re::new(r"\{%[-]?[ \t\r\n]*endcomment[ \t\r\n]*[-]?%\}", "g"));

fn next_index(text: &[u16], needle: &[u16], from: usize, known: Option<Option<usize>>) -> Option<usize> {
    match known {
        Some(k) if k.is_none_or(|k| k >= from) => k,
        _ => js::index_of(text, needle, from),
    }
}
fn find_close(
    text: &[u16],
    i: usize,
    close: &[u16],
    limit: usize,
    mode: Mode,
    check: CheckBudget<'_>,
) -> Result<Option<usize>> {
    let (mut j, mut in_comment, mut next_check) = (i, mode == Comment, i);
    while j < limit {
        if j >= next_check {
            check("template-scan")?;
            next_check = j + 0x4000;
        }
        let c = text[j];
        if !in_comment && matches!(c, 34 | 39) {
            j += 1;
            while j < limit && text[j] != c {
                if j >= next_check {
                    check("template-scan")?;
                    next_check = j + 0x4000;
                }
                j += if text[j] == 92 { 2 } else { 1 };
            }
            j += 1;
            continue;
        }
        if js::starts_with_at(text, close, j) {
            return Ok(Some(j + close.len()));
        }
        if mode == HashComments {
            in_comment = c == 35 || (in_comment && c != 10);
        }
        j += 1;
    }
    Ok(None)
}

type Pair = (JsString, JsString, Mode);
struct Templates<'a> {
    text: &'a [u16],
    check: CheckBudget<'a>,
    name: String,
    pairs: Arc<Vec<Pair>>,
    bodies_only: bool,
    active: bool,
    last_close: HashMap<JsString, Option<usize>>,
    dashed_plain: Option<Option<usize>>,
    dashed_tilde: Option<Option<usize>>,
    body_ends: HashMap<String, Option<(usize, usize)>>,
    header_ends: HashMap<usize, Option<usize>>,
    dashed_headers: Option<HashMap<usize, Option<usize>>>,
}
impl Templates<'_> {
    fn read_at(&mut self, i: usize, header_only: bool) -> Result<Option<OwnedSpan>> {
        (self.check)("template-scan")?;
        let Some((open, close, mode)) =
            self.pairs.iter().find(|(open, _, _)| js::starts_with_at(self.text, open, i)).cloned()
        else {
            return Ok(None);
        };
        if self.last_close.get(&close).copied().flatten().is_none_or(|at| at < i + open.len()) {
            return Ok(None);
        }
        let mut scan_mode = mode;
        if mode == DashedComment {
            let dashes = i + open.len() - 2;
            let end = if header_only {
                if self.dashed_headers.is_none() {
                    let mut headers = HashMap::new();
                    let (mut plain, mut tilde) = (None, None);
                    let mut from = 0;
                    while let Some(at) = js::index_of_unit(self.text, self.pairs[0].0[0], from) {
                        (self.check)("template-comment-headers")?;
                        from = at + 1;
                        let Some((prefix, _, _)) = self.pairs.iter().find(|(prefix, _, kind)| {
                            *kind == DashedComment && js::starts_with_at(self.text, prefix, at)
                        }) else {
                            continue;
                        };
                        let start = at + prefix.len() - 2;
                        plain = Some(next_index(self.text, &js::u("--}}"), start, plain));
                        tilde = Some(next_index(self.text, &js::u("--~}}"), start, tilde));
                        headers.insert(
                            at,
                            [plain.flatten().map(|p| p + 4), tilde.flatten().map(|p| p + 5)]
                                .into_iter()
                                .flatten()
                                .min(),
                        );
                    }
                    self.dashed_headers = Some(headers);
                }
                self.dashed_headers.as_ref().and_then(|h| h.get(&i)).copied().flatten()
            } else {
                self.dashed_plain = Some(next_index(self.text, &js::u("--}}"), dashes, self.dashed_plain));
                self.dashed_tilde = Some(next_index(self.text, &js::u("--~}}"), dashes, self.dashed_tilde));
                [self.dashed_plain.flatten().map(|p| p + 4), self.dashed_tilde.flatten().map(|p| p + 5)]
                    .into_iter()
                    .flatten()
                    .min()
            };
            if let Some(end) = end {
                let mut span = OwnedSpan::new((i, end, end));
                if self.bodies_only && !header_only {
                    span.report = Some(false);
                }
                return Ok(Some(span));
            }
            scan_mode = Comment;
        }
        let n = self.text.len();
        let limit = if self.bodies_only && !header_only { n } else { n.min(i + open.len() + MAX_TEMPLATE_SCAN) };
        if let Some(end) = find_close(self.text, i + open.len(), &close, limit, scan_mode, self.check)? {
            if header_only {
                return Ok(Some(OwnedSpan::new((i, end, end))));
            }
            let head = &self.text[i + open.len()..end - close.len()];
            let block = if self.name == "handlebars" && open == js::u("{{{{") && HB_RAW.test(head) {
                Some("raw".to_owned())
            } else if open == js::u("{%") && matches!(self.name.as_str(), "liquid" | "jinja" | "nunjucks") {
                BODY_HEAD.exec(head).map(|m| js::lossy(m.s(1)))
            } else {
                None
            };
            if let Some(block) = block.filter(|b| b != "comment" || self.name == "liquid") {
                if !self.body_ends.contains_key(&block) || self.body_ends[&block].is_some_and(|(at, _)| at < end) {
                    let pattern = if self.name == "handlebars" {
                        &HB_END
                    } else if block == "raw" {
                        &RAW_END
                    } else {
                        &COMMENT_END
                    };
                    self.body_ends.insert(block.clone(), pattern.exec_at(self.text, end).map(|m| (m.index(), m.end())));
                }
                let ending = self.body_ends[&block];
                let through = ending.map_or(n, |(_, end)| end);
                let mut span = OwnedSpan::new((i, through, through));
                span.tail = ending.is_none();
                return Ok(Some(span));
            }
            let mut span = OwnedSpan::new((i, end, end));
            if self.bodies_only {
                span.report = Some(false);
            }
            return Ok(Some(span));
        }
        if header_only {
            return Ok(None);
        }
        if self.bodies_only {
            let mut span = OwnedSpan::new((i, n, n));
            span.report = Some(false);
            return Ok(Some(span));
        }
        if limit < n {
            return Err(Error::input(
                format!("Template expression exceeds the {MAX_TEMPLATE_SCAN}-char scan bound at offset {i}"),
                json!({ "offset": i, "max": MAX_TEMPLATE_SCAN, "reason": "template-overlimit" }),
            ));
        }
        Ok(None)
    }
}
impl SpanReader for Templates<'_> {
    fn next(&mut self, from: usize) -> Result<Option<(usize, usize, usize)>> {
        if !self.active {
            return Ok(None);
        }
        let mut cursor = from;
        while let Some(at) = js::index_of_unit(self.text, self.pairs[0].0[0], cursor) {
            (self.check)("template-opener")?;
            if self.pairs.iter().any(|(open, _, _)| js::starts_with_at(self.text, open, at)) {
                return Ok(Some((at, at, at)));
            }
            cursor = at + 1;
        }
        Ok(None)
    }
    fn read(&mut self, at: usize) -> Result<OwnedSpan> {
        Ok(self.read_at(at, false)?.unwrap_or_else(|| {
            let mut span = OwnedSpan::new((at, at + 1, at + 1));
            span.report = Some(false);
            span
        }))
    }
    fn header(&mut self, at: usize) -> Result<Option<usize>> {
        if self.text.get(at) != Some(&self.pairs[0].0[0])
            || !self.pairs.iter().any(|(open, _, _)| js::starts_with_at(self.text, open, at))
        {
            return Ok(None);
        }
        if let Some(&end) = self.header_ends.get(&at) {
            return Ok(end);
        }
        let end = self.read_at(at, true)?.map(|s| s.end);
        self.header_ends.insert(at, end);
        Ok(end)
    }
}
pub fn template_reader(name: &str) -> Option<ReaderFor> {
    template_body_reader(name, false)
}
pub fn template_body_reader(name: &str, bodies_only: bool) -> Option<ReaderFor> {
    let pairs: Arc<Vec<Pair>> =
        Arc::new(delimiters(name)?.iter().map(|&(open, close, mode)| (js::u(open), js::u(close), mode)).collect());
    let name = name.to_owned();
    Some(Arc::new(move |text, ctx| {
        let last_close: HashMap<_, _> =
            pairs.iter().map(|(_, close, _)| (close.clone(), js::last_index_of(text, close))).collect();
        let active =
            pairs.iter().any(|(open, _, _)| js::includes(text, open)) && last_close.values().any(Option::is_some);
        Ok(Box::new(Templates {
            text,
            check: ctx.check_budget,
            name: name.clone(),
            pairs: pairs.clone(),
            bodies_only,
            active,
            last_close,
            dashed_plain: None,
            dashed_tilde: None,
            body_ends: HashMap::new(),
            header_ends: HashMap::new(),
            dashed_headers: None,
        }))
    }))
}
#[cfg(test)]
pub fn scanner_for(reader: ReaderFor) -> OpaqueScanner {
    Arc::new(move |text: &[u16], ctx: &ScanCtx<'_>| collect_owned(reader(text, ctx)?.as_mut()))
}
#[cfg(test)]
fn template_scanner(name: &str) -> Option<OpaqueScanner> {
    template_reader(name).map(scanner_for)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protect::blocks::Range;

    fn scan(name: &str, text: &str) -> Result<Vec<Range>> {
        let scanner = template_scanner(name).expect("known engine");
        let elements = crate::protect::blocks::ElementSet::default();
        let ctx =
            ScanCtx { check_budget: &|_| Ok(()), resolve: &|s| s.to_vec(), elements: &elements, code_edges: false };
        scanner(&js::u(text), &ctx).map(|spans| spans.into_iter().map(|(a, b, _)| (a, b)).collect())
    }

    #[test]
    fn handlebars_pairs_and_dashed_comments() {
        assert_eq!(
            scan("handlebars", "{{a}} b {{{c}}} {{!-- x }} --}} d {{~!-- y --~}} {{!--}} {{! q }}").unwrap(),
            vec![(0, 5), (8, 15), (16, 31), (34, 48), (49, 56), (57, 65)]
        );
        assert_eq!(scan("handlebars", "{{!-- never closed }} after").unwrap(), vec![(0, 21)]);
    }

    #[test]
    fn strings_are_stepped_over() {
        assert_eq!(scan("mustache", r#"{{ "a }}" }} x {{! c "q }} y"#).unwrap(), vec![(0, 12), (15, 26)]);
        assert_eq!(scan("twig", "{% if a %}{{ 'b %}' }}{# c #}").unwrap(), vec![(0, 10), (10, 22), (22, 29)]);
        assert_eq!(scan("handlebars", r#"{{ "\"}}" }} {{ a }}"#).unwrap(), vec![(0, 12), (13, 20)]);
    }

    #[test]
    fn unicode_offsets_are_utf16() {
        assert_eq!(scan("jinja", "{{ привет 😀 }} текст {# ключ #}").unwrap(), vec![(0, 15), (22, 32)]);
    }

    #[test]
    fn hash_comments() {
        assert_eq!(scan("liquid", "{% # comment %} x %} {{ y }}").unwrap(), vec![(0, 15), (21, 28)]);
        assert_eq!(scan("liquid", "{% # c\n \"a %}\" %}").unwrap(), vec![(0, 17)]);
        assert_eq!(scan("erb", r#"<%# c %> <% x # y %> z %> <%= "a %>" %>"#).unwrap(), vec![(0, 8), (9, 20), (26, 39)]);
    }

    #[test]
    fn unclosed_is_prose() {
        assert_eq!(scan("nunjucks", "{{ a").unwrap(), vec![]);
        assert_eq!(scan("nunjucks", "no opens here }}").unwrap(), vec![]);
    }

    #[test]
    fn overlimit_fails_closed() {
        for text in [format!("{{{{{}}}}}", "x".repeat(9000)), format!("{{{{\"{}}}}}", "x".repeat(9000))] {
            let e = scan("handlebars", &text).unwrap_err();
            assert_eq!(e.message, "Template expression exceeds the 8192-char scan bound at offset 0");
            assert_eq!(e.details, json!({ "offset": 0, "max": 8192, "reason": "template-overlimit" }));
        }
    }

    #[test]
    fn unknown_engine() {
        assert!(template_reader("unknown").is_none());
        assert!(template_scanner("unknown").is_none());
    }
}
