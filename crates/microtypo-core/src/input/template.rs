use std::collections::HashMap;
use std::sync::Arc;

use serde_json::json;

use crate::errors::{Error, Result};
use crate::input::span_path::{Next, ReaderFor, collect_spans};
use crate::js::{self, JsString};
use crate::protect::blocks::{ScanCtx, Scanner};

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

fn next_index(text: &[u16], needle: &[u16], from: usize, known: Option<Option<usize>>) -> Option<usize> {
    match known {
        Some(k) if k.is_none_or(|k| k >= from) => k,
        _ => js::index_of(text, needle, from),
    }
}

fn find_close(text: &[u16], i: usize, close: &[u16], limit: usize, mode: Mode) -> Option<usize> {
    let mut j = i;
    let mut in_comment = mode == Comment;

    while j < limit {
        let c = text[j];

        if !in_comment && (c == 0x22 || c == 0x27) {
            j += 1;
            while j < limit && text[j] != c {
                j += if text[j] == 0x5C { 2 } else { 1 };
            }
            j += 1;
            continue;
        }

        if js::starts_with_at(text, close, j) {
            return Some(j + close.len());
        }

        if mode == HashComments {
            in_comment = c == 0x23 || (in_comment && c != 0x0A);
        }

        j += 1;
    }

    None
}

fn reader_for<F>(f: F) -> ReaderFor
where
    F: for<'a> Fn(&'a [u16], &'a ScanCtx<'a>) -> Result<Next<'a>> + Send + Sync + 'static,
{
    Arc::new(f)
}

pub fn template_reader(name: &str) -> Option<ReaderFor> {
    let pairs: Arc<Vec<(JsString, JsString, Mode)>> =
        Arc::new(delimiters(name)?.iter().map(|&(open, close, mode)| (js::u(open), js::u(close), mode)).collect());

    Some(reader_for(move |text, ctx| {
        let n = text.len();
        let pairs = Arc::clone(&pairs);

        if !pairs.iter().any(|(open, _, _)| js::includes(text, open)) {
            return Ok(Box::new(|_| Ok(None)) as Next<'_>);
        }

        let mut last_close: HashMap<JsString, Option<usize>> = HashMap::new();
        let mut has_close = false;
        for (_, close, _) in pairs.iter() {
            if !last_close.contains_key(close) {
                let at = js::last_index_of(text, close);
                last_close.insert(close.clone(), at);
                has_close |= at.is_some();
            }
        }

        if !has_close {
            return Ok(Box::new(|_| Ok(None)) as Next<'_>);
        }

        let mut dashed_plain: Option<Option<usize>> = None;
        let mut dashed_tilde: Option<Option<usize>> = None;
        let first = pairs[0].0[0];
        let dashed_plain_needle = js::u("--}}");
        let dashed_tilde_needle = js::u("--~}}");

        Ok(Box::new(move |from| {
            let mut i = from;
            let mut next_check = from;

            while i < n {
                if i >= next_check {
                    (ctx.check_budget)("template-scan")?;
                    next_check = i + 0x4000;
                }

                let Some(at) = js::index_of_unit(text, first, i) else { break };
                i = at;

                for (open, close, mode) in pairs.iter() {
                    if !js::starts_with_at(text, open, i) {
                        continue;
                    }

                    if last_close[close].is_none_or(|at| at < i + open.len()) {
                        break;
                    }

                    let mut scan_mode = *mode;

                    if *mode == DashedComment {
                        let dashes = i + open.len() - 2;

                        dashed_plain = Some(next_index(text, &dashed_plain_needle, dashes, dashed_plain));
                        dashed_tilde = Some(next_index(text, &dashed_tilde_needle, dashes, dashed_tilde));

                        let end = [dashed_plain.flatten().map(|p| p + 4), dashed_tilde.flatten().map(|p| p + 5)]
                            .into_iter()
                            .flatten()
                            .min();

                        if let Some(end) = end {
                            return Ok(Some((i, end, end)));
                        }

                        scan_mode = Comment;
                    }

                    let limit = n.min(i + open.len() + MAX_TEMPLATE_SCAN);

                    if let Some(end) = find_close(text, i + open.len(), close, limit, scan_mode) {
                        return Ok(Some((i, end, end)));
                    }

                    if limit < n {
                        return Err(Error::input(
                            format!(
                                "Template expression exceeds the {MAX_TEMPLATE_SCAN}-char scan bound at offset {i}"
                            ),
                            json!({ "offset": i, "max": MAX_TEMPLATE_SCAN, "reason": "template-overlimit" }),
                        ));
                    }

                    break;
                }

                i += 1;
            }

            Ok(None)
        }) as Next<'_>)
    }))
}

pub fn scanner_for(reader: ReaderFor) -> Scanner {
    Arc::new(move |text: &[u16], ctx: &ScanCtx<'_>| {
        let mut next = reader(text, ctx)?;
        collect_spans(&mut next)
    })
}

pub fn template_scanner(name: &str) -> Option<Scanner> {
    template_reader(name).map(scanner_for)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protect::blocks::Range;

    fn scan(name: &str, text: &str) -> Result<Vec<Range>> {
        let scanner = template_scanner(name).expect("known engine");
        let elements = crate::protect::blocks::ElementSet::default();
        let ctx = ScanCtx { check_budget: &|_| Ok(()), resolve: &|s| s.to_vec(), elements: &elements };
        scanner(&js::u(text), &ctx)
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
