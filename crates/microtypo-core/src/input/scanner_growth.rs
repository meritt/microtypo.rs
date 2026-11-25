use std::hint::black_box;
use std::sync::{Mutex, PoisonError};
use std::time::Instant;

use crate::input::localization::localization_scanner;
use crate::input::pragmas::pragma_scanner;
use crate::js;
use crate::protect::blocks::{ElementSet, ScanCtx};
use crate::shared::line_endings::restore_file_tail;

type Shape = (&'static str, &'static str, fn(usize) -> String);
type PragmaShape = (&'static str, fn(usize) -> String);

const ROUNDS: usize = 5;
static ONE_AT_A_TIME: Mutex<()> = Mutex::new(());

fn sample(source: &[u16], repeats: usize, action: &mut impl FnMut(&[u16])) -> f64 {
    let start = Instant::now();
    for _ in 0..repeats {
        action(black_box(source));
    }
    start.elapsed().as_secs_f64() * 1000.0
}

fn growth(name: &str, build: fn(usize) -> String, count: usize, repeats: usize, mut action: impl FnMut(&[u16])) {
    let _alone = ONE_AT_A_TIME.lock().unwrap_or_else(PoisonError::into_inner);
    let count = if cfg!(debug_assertions) { count / 4 } else { count };
    let sources = [count, 2 * count, 4 * count].map(|n| js::u(&build(n)));
    sample(&sources[0], repeats, &mut action);
    let mut best = [f64::INFINITY; 3];
    for _ in 0..ROUNDS {
        for (best, source) in best.iter_mut().zip(&sources) {
            *best = best.min(sample(source, repeats, &mut action));
        }
    }
    let [one, two, four] = best;
    eprintln!(
        "{name}: units {:?}; N={count} {one:.3} ms, 2N {two:.3} ms, 4N {four:.3} ms",
        sources.each_ref().map(Vec::len)
    );
    assert!(two < 3.0 * one, "{name}: 2x input cost {:.2}x", two / one);
    assert!(four < 8.0 * one, "{name}: 4x input cost {:.2}x", four / one);
}

#[test]
fn localization_scanners_scale_linearly() {
    let shapes: &[Shape] = &[
        ("nested ICU branches", "icu", |n| {
            format!("{}# день - текст{}", "{n, plural, one {".repeat(n), "} other {# дней}}".repeat(n))
        }),
        ("unclosed ICU braces", "icu", |n| "{ ".repeat(n * 20)),
        ("quoted ICU braces", "icu", |n| "{n, plural, one {'#{' день} other {# дней}} ".repeat(n)),
        ("unfinished printf width", "printf", |n| format!("%{}!", "0".repeat(n * 20))),
        ("unfinished interpolation", "interpolation", |n| "${".repeat(n * 20)),
    ];
    let check = |_: &str| Ok(());
    let resolve = |value: &[u16]| value.to_vec();
    let elements = ElementSet::default();
    let ctx = ScanCtx { check_budget: &check, resolve: &resolve, elements: &elements, code_edges: false };
    for &(name, mode, build) in shapes {
        let scan = localization_scanner(mode, false).unwrap();
        growth(name, build, 1_000, 10, |source| {
            black_box(scan(source, &ctx).unwrap());
        });
    }
}

#[test]
fn pragma_scanners_scale_linearly() {
    let shapes: &[PragmaShape] = &[
        ("unclosed pragma", |n| format!("<!-- microtypo-ignore-start -->\n{}", "Корвин - принц\n".repeat(n))),
        ("nested pragmas", |n| {
            format!(
                "{}Текст{}",
                "<!-- microtypo-ignore-start -->\n".repeat(n),
                "<!-- microtypo-ignore-end -->\n".repeat(n)
            )
        }),
        ("ordinary comments", |n| "<!-- обычный комментарий -->\n".repeat(n)),
    ];
    let check = |_: &str| Ok(());
    let resolve = |value: &[u16]| value.to_vec();
    let elements = ElementSet::default();
    let ctx = ScanCtx { check_budget: &check, resolve: &resolve, elements: &elements, code_edges: false };
    let scan = pragma_scanner(true);
    for &(name, build) in shapes {
        growth(name, build, 1_000, 10, |source| {
            black_box(scan(source, &ctx, &resolve).unwrap());
        });
    }
}

#[test]
fn long_file_prefixes_and_final_newlines_scale_linearly() {
    growth(
        "file newline prefix and suffix",
        |n| format!("{}Источник{}", "\r\n".repeat(n), "\r\n".repeat(n)),
        4_000,
        30,
        |source| {
            let mut result = js::u("Результат");
            let tail = source.iter().rposition(|&unit| !matches!(unit, 0x0D | 0x0A)).unwrap() + 1;
            result.extend_from_slice(&source[tail..]);
            assert_eq!(restore_file_tail(source, result.clone()), result);
        },
    );
}

#[test]
fn every_short_crlf_file_tail_is_restored_exactly() {
    for length in 0..7 {
        for bits in 0..1 << length {
            let tail: String = (0..length).map(|at| if bits & (1 << at) == 0 { '\r' } else { '\n' }).collect();
            for prefix in [String::new(), "\r\n\r\n".to_owned(), "\n".repeat(20)] {
                let source = js::u(&format!("{prefix}Источник{tail}"));
                let result = restore_file_tail(&source, js::u("Результат\n\r\n"));
                assert_eq!(result, js::u(&format!("Результат{tail}")));
            }
        }
    }
}

#[test]
fn mdx_syntax_scales_linearly_and_keeps_exact_endpoints() {
    for (name, build) in [
        (
            "MDX nested classes",
            (|n| format!("{{{}0{}}}", "class extends Bases[".repeat(n), "] {}".repeat(n))) as fn(usize) -> String,
        ),
        ("MDX overlapping comments", |n| format!("{{{} }}", "/*/ ".repeat(n))),
        ("MDX nested templates", |n| format!("{{{}0{}}}", "`${".repeat(n), "}`".repeat(n))),
    ] {
        growth(name, build, 1000, 3, |text| {
            let mut reader = crate::input::mdx::MdxReader::new(text, &|_| Ok(())).unwrap();
            assert_eq!(reader.next(0).unwrap().unwrap().0, 0);
            let span = reader.read(0).unwrap();
            assert_eq!(span.1, text.len());
            assert_eq!(span.2, text.len());
        });
    }
}

#[test]
fn math_tokens_and_quoted_attribute_intervals_scale_linearly() {
    growth(
        "math late angle closers",
        |n| "$a - b<c$> ".repeat(n),
        4000,
        5,
        |text| {
            let spans = crate::input::math::scan_math(text, &|_| Ok(())).unwrap();
            let width = "$a - b<c$> ".encode_utf16().count();
            assert_eq!(spans.len(), text.len() / width);
            for (i, span) in spans.iter().enumerate() {
                assert_eq!(*span, (i * width, i * width + 9));
            }
        },
    );
    growth(
        "math dollars in quoted attributes",
        |n| format!("<span title=\"{}\">", "$a - b$ ".repeat(n)),
        4000,
        5,
        |text| {
            assert_eq!(crate::input::math::scan_math(text, &|_| Ok(())).unwrap(), [] as [(usize, usize); 0]);
        },
    );
}

#[test]
fn mdx_and_math_scans_observe_a_deadline_during_long_input() {
    use crate::errors::Error;
    use std::cell::Cell;
    let checks = Cell::new(0);
    let limit = Cell::new(usize::MAX);
    let check = |label: &str| {
        checks.set(checks.get() + 1);
        if checks.get() > limit.get() {
            Err(Error::budget("deadline", serde_json::json!({"where":label})))
        } else {
            Ok(())
        }
    };
    let text = js::u(&format!("{{{}", "/* ".repeat(100_000)));
    let mut reader = crate::input::mdx::MdxReader::new(&text, &check).unwrap();
    limit.set(checks.get() + 100);
    assert!(reader.read(0).is_err());
    checks.set(0);
    limit.set(100);
    assert!(crate::input::math::scan_math(&js::u(&"$a - b$ ".repeat(100_000)), &check).is_err());
}

#[test]
fn declaration_and_raw_tag_scanners_scale_linearly() {
    for (name, build) in [
        ("unclosed quoted declarations", (|n| "<? \"".repeat(n)) as fn(usize) -> String),
        ("unclosed bang declarations", |n| "<! ".repeat(n)),
    ] {
        growth(name, build, 4000, 5, |text| assert_eq!(crate::protect::tag_spans::tag_spans(text, None).len(), 0));
    }
    growth(
        "raw xmp openers",
        |n| "<xmp><noembed><title>".repeat(n),
        4000,
        3,
        |text| {
            let raw = ["xmp", "noembed", "title"].map(str::to_owned);
            let spans = crate::protect::tag_spans::tag_spans(text, Some(&raw));
            assert_eq!(spans.len(), text.len() / "<xmp><noembed><title>".encode_utf16().count() * 3);
            for span in spans {
                assert_eq!(text[span.start], 60);
                assert_eq!(text[span.end], 62);
            }
        },
    );
}

#[test]
fn document_local_yaml_tags_and_empty_keys_scale_linearly() {
    growth(
        "document-local YAML tags",
        |n| "%TAG !e! tag:yaml.org,2002:\n---\n{ !e!str x: текст }\n".repeat(n),
        2000,
        3,
        |text| {
            let spans = crate::input::yaml::scan_yaml(text, &|_| Ok(())).unwrap();
            assert_eq!(
                spans.len(),
                text.len() / "%TAG !e! tag:yaml.org,2002:\n---\n{ !e!str x: текст }\n".encode_utf16().count()
            );
            for span in spans {
                assert_eq!(&text[span.start..span.end], js::u("текст"));
            }
        },
    );
    growth(
        "empty YAML flow keys",
        |n| format!("[{}]", "? : текст,".repeat(n)),
        2000,
        3,
        |text| {
            let spans = crate::input::yaml::scan_yaml(text, &|_| Ok(())).unwrap();
            assert_eq!(spans.len(), (text.len() - 2) / "? : текст,".encode_utf16().count());
            for span in spans {
                assert_eq!(span.tail(), js::u("null"));
            }
        },
    );
}

#[test]
fn raw_template_bodies_and_rejected_long_labels_scale_linearly() {
    let elements = ElementSet::default();
    let check = |_: &str| Ok(());
    let resolve = |text: &[u16]| text.to_vec();
    let ctx = ScanCtx { check_budget: &check, resolve: &resolve, elements: &elements, code_edges: false };
    for (name, mode, build) in [
        ("unclosed Liquid raw bodies", "liquid", (|n| "{% raw %}".repeat(n)) as fn(usize) -> String),
        ("closed Liquid comments", "liquid", |n| "{% comment %}код - \"да\"{% endcomment %}".repeat(n)),
        ("Handlebars raw bodies", "handlebars", |n| "{{{{raw}}}}код - \"да\"{{{{/raw}}}}".repeat(n)),
    ] {
        let scan = crate::input::template::scanner_for(crate::input::template::template_reader(mode).unwrap());
        growth(name, build, 2000, 3, |text| {
            let spans = scan(text, &ctx).unwrap();
            assert_eq!(spans.first().unwrap().0, 0);
            assert_eq!(spans.last().unwrap().1, text.len());
            assert_eq!(spans[0].2 == crate::protect::blocks::SpanKind::Tail, name == "unclosed Liquid raw bodies");
        });
    }
    growth(
        "rejected long reference definitions",
        |n| format!("[{} - да]: /url\n\n", "я".repeat(n)),
        4000,
        5,
        |text| {
            assert_eq!(
                crate::input::markdown::scan_reference_definitions(text, &check, &resolve, None).unwrap().len(),
                0
            );
        },
    );
    growth(
        "rejected long second labels",
        |n| format!("[текст][{} - да]", "я".repeat(n)),
        4000,
        5,
        |text| {
            assert_eq!(crate::input::markdown::scan_link_destinations(text, &check).unwrap().len(), 0);
        },
    );
    growth(
        "TOML CRLF comments",
        |n| "# заголовок\r\n".repeat(n),
        4000,
        5,
        |text| {
            assert_eq!(crate::input::toml::scan_toml(text, &check).unwrap().len(), 0);
        },
    );
}
