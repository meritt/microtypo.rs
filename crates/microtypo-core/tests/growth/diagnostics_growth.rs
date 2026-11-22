use std::fmt::Write as _;

use microtypo_conformance::HOST;
use microtypo_core::{MicroTypo, ProcessOptions};
use serde_json::json;

use crate::timing::{alone, median};

fn scalars(format: &str, n: usize, value: char) -> String {
    let mut text = String::new();
    for i in 0..n {
        if format == "yaml" {
            writeln!(text, "k{i}: {value}").unwrap();
        } else {
            writeln!(text, "k{i} = '{value}'").unwrap();
        }
    }
    text
}

fn measure(format: &str, n: usize) -> f64 {
    let (input, expected, first) = match format {
        "yaml" => (scalars(format, n, 'a'), scalars(format, n, 'b'), 4),
        "toml" => (scalars(format, n, 'a'), scalars(format, n, 'b'), 6),
        _ => (
            format!("<code>a</code>{}", "<b title=\"a\">x</b>".repeat(n)),
            format!("<code>a</code>{}", "<b title=\"b\">x</b>".repeat(n)),
            24,
        ),
    };
    let config = json!({"presets":false,"maxInputLength":1000000,"maxProcessingMs":5000,"input":{"format":format,"attributes":if format=="html" {vec!["title"]} else {vec![]}},"ruleGroups":[{"name":"custom","rules":[{"id":"replace","pattern":{"source":"a","flags":"g"},"replacement":"b"}]}]});
    let mut engine = MicroTypo::new(Some(&config), HOST).unwrap();
    let input: Vec<_> = input.encode_utf16().collect();
    let mut outputs = Vec::new();
    let sample = median(|| {
        outputs.push(
            engine
                .process_with_diagnostics(&input, ProcessOptions { trace: true, ..ProcessOptions::default() })
                .unwrap(),
        );
    });
    for output in outputs {
        assert_eq!(String::from_utf16(&output.text).unwrap(), expected);
        let trace = output.trace.unwrap();
        assert_eq!(trace.len(), 1);
        assert_eq!(trace[0].count, n);
        assert_eq!(trace[0].ranges[0], (first, first + 1));
        let expected_last =
            if format == "html" { 24 + (n - 1) * 18 } else { input.len() - if format == "yaml" { 2 } else { 3 } };
        assert_eq!(trace[0].ranges[n - 1], (expected_last, expected_last + 1));
    }
    assert_eq!(engine.process_utf16(&input).unwrap(), expected.encode_utf16().collect::<Vec<_>>());
    engine.apply_options(Some(&json!({"maxProcessingMs":1}))).unwrap();
    assert_eq!(
        engine
            .process_with_diagnostics(&input, ProcessOptions { trace: true, ..ProcessOptions::default() })
            .unwrap_err()
            .code(),
        Some("ERR_MICROTYPO_BUDGET")
    );
    engine.apply_options(Some(&json!({"maxProcessingMs":5000}))).unwrap();
    assert_eq!(engine.process_utf16(&input).unwrap(), expected.encode_utf16().collect::<Vec<_>>());
    sample
}

#[test]
#[ignore = "resource measurement runs separately in release mode"]
fn scalar_and_attribute_trace_growth_and_deadline_recovery() {
    let _alone = alone();
    for (format, n) in [("yaml", 8000), ("toml", 8000), ("html", 1000)] {
        let small = measure(format, n);
        let large = measure(format, n * 4);
        eprintln!("{format} 4x values: {small:.6}s → {large:.6}s, {:.2}x", large / small);
        assert!(large / small < 7.0, "{format} 4x values took {}x", large / small);
    }
    measure("yaml", 64000);
    measure("html", 8000);
}
