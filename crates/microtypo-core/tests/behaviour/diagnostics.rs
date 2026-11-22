use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

use microtypo_conformance::HOST;
use microtypo_core::{JsString, MicroTypo, Position, ProcessOptions, Re, Replacement, Rule, RuleGroupDef, TraceKind};
use serde_json::json;

fn units(text: &str) -> JsString {
    text.encode_utf16().collect()
}

fn group(rules: Vec<Rule>) -> RuleGroupDef {
    RuleGroupDef { rules, ..RuleGroupDef::default() }
}

fn assert_edits(input: &[u16], output: &microtypo_core::Diagnostics) {
    let mut result = Vec::new();
    let mut cursor = 0;
    for edit in output.edits.as_ref().unwrap() {
        assert!(edit.start >= cursor && edit.end >= edit.start && edit.end <= input.len());
        for at in [edit.start, edit.end] {
            assert!(
                !(at > 0
                    && at < input.len()
                    && (0xd800..=0xdbff).contains(&input[at - 1])
                    && (0xdc00..=0xdfff).contains(&input[at]))
            );
        }
        result.extend_from_slice(&input[cursor..edit.start]);
        result.extend_from_slice(&edit.text);
        cursor = edit.end;
    }
    result.extend_from_slice(&input[cursor..]);
    assert_eq!(result, output.text);
}

const ALL: ProcessOptions = ProcessOptions { changes: true, edits: true, trace: true };

#[test]
fn successive_regexes_and_native_handler_keep_match_origins() {
    let mut engine = MicroTypo::new(Some(&json!({"presets":false})), HOST).unwrap();
    engine
        .register_rule_group(
            group(vec![
                Rule::pattern("first", Re::try_new("a", "g").unwrap(), Replacement::tpl("b")),
                Rule::pattern("unchanged", Re::try_new("b", "g").unwrap(), Replacement::tpl("b")),
                Rule::pattern("second", Re::try_new("bb", "g").unwrap(), Replacement::tpl("c")),
                Rule::handler("handler", |ctx| {
                    Ok(Some(ctx.text.iter().map(|&c| if c == 99 { 100 } else { c }).collect()))
                }),
            ]),
            Some("custom"),
            Position::End,
        )
        .unwrap();
    let input = units("aa aa");
    let output = engine.process_with_diagnostics(&input, ALL).unwrap();
    assert_eq!(output.text, units("d d"));
    let trace = output.trace.as_ref().unwrap();
    assert_eq!(trace.len(), 3);
    assert_eq!(
        (trace[0].count, trace[0].kind, trace[0].ranges.as_slice()),
        (4, TraceKind::Regex, &[(0, 1), (1, 2), (3, 4), (4, 5)][..])
    );
    assert_eq!((trace[1].count, trace[1].ranges.as_slice()), (2, &[(0, 2), (3, 5)][..]));
    assert_eq!(
        (trace[2].count, trace[2].kind, trace[2].ranges.as_slice()),
        (1, TraceKind::Handler, &[(0, 2), (3, 5)][..])
    );
    assert_edits(&input, &output);
    assert_eq!(engine.process("aa").unwrap(), "d");
}

#[test]
fn native_cycles_count_changing_invocations_and_keep_net_zero_events() {
    let mut engine = MicroTypo::new(Some(&json!({"presets":false})), HOST).unwrap();
    let calls = Arc::new(AtomicUsize::new(0));
    let count = calls.clone();
    engine
        .register_rule_group(
            group(vec![
                Rule::handler("cycle", move |ctx| {
                    count.fetch_add(1, Ordering::Relaxed);
                    Ok(Some(if matches!(ctx.text.first(), Some(97 | 98)) {
                        ctx.text[1..].to_vec()
                    } else {
                        ctx.text.clone()
                    }))
                })
                .cycled(),
            ]),
            Some("custom"),
            Position::End,
        )
        .unwrap();
    let output = engine.process_with_diagnostics(&units("abx"), ALL).unwrap();
    assert_eq!(calls.load(Ordering::Relaxed), 3);
    assert_eq!(output.text, units("x"));
    assert_eq!(output.trace.as_ref().unwrap()[0].count, 2);
    assert_eq!(output.trace.as_ref().unwrap()[0].ranges, [(0, 1), (1, 2)]);
    let limits = Arc::new(AtomicUsize::new(0));
    let limit = limits.clone();
    engine.set_on_cycle_limit(Some(Arc::new(move |id, n| {
        assert_eq!((id, n), (units("roundtrip").as_slice(), 100));
        limit.fetch_add(1, Ordering::Relaxed);
    })));
    engine
        .register_rule_group(
            group(vec![
                Rule::handler("roundtrip", |ctx| Ok(Some(if ctx.text == [97] { vec![98] } else { vec![97] }))).cycled(),
            ]),
            Some("custom"),
            Position::End,
        )
        .unwrap();
    let output = engine.process_with_diagnostics(&[97], ALL).unwrap();
    assert_eq!(output.text, [97]);
    assert_eq!(output.edits.as_ref().unwrap().len(), 0);
    assert_eq!(output.trace.as_ref().unwrap()[0].count, 100);
    assert_eq!(output.trace.as_ref().unwrap()[0].ranges, vec![(0, 1); 100]);
    assert_eq!(limits.load(Ordering::Relaxed), 1);
}

#[test]
fn callbacks_keep_literal_json_pairs_and_reuse_after_errors() {
    let mut engine = MicroTypo::new(Some(&json!({"presets":false,"input":"json"})), HOST).unwrap();
    engine
        .register_rule_group(
            group(vec![Rule::pattern("low", Re::try_new(r"\uDE00", "g").unwrap(), Replacement::tpl("x"))]),
            Some("custom"),
            Position::End,
        )
        .unwrap();
    let input = units("{\"x\":\"😀\"}");
    let output = engine.process_with_diagnostics(&input, ALL).unwrap();
    assert_eq!(output.trace.as_ref().unwrap()[0].ranges, [(6, 8)]);
    assert_edits(&input, &output);
    assert!(engine.process_with_diagnostics(&units("{broken"), ALL).is_err());
    assert_eq!(engine.process_utf16(&input).unwrap(), output.text);
}

#[test]
fn nonce_and_visible_markup_origins_stay_exact_on_reuse() {
    let mut engine = MicroTypo::new(Some(&json!({"html":true})), HOST).unwrap();
    let input = units("Он www.example.com/t?n=100000 передал.");
    let first = engine.process_with_diagnostics(&input, ALL).unwrap();
    for _ in 0..32 {
        let output = engine.process_with_diagnostics(&input, ALL).unwrap();
        assert_eq!(output, first);
        assert_eq!(
            output.trace.as_ref().unwrap().iter().find(|entry| entry.rule == units("text.autolink")).unwrap().ranges,
            [(0, 29)]
        );
        assert_edits(&input, &output);
    }
    let mut engine =
        MicroTypo::new(Some(&json!({"input":{"format":"html","attributes":[]},"html":false})), HOST).unwrap();
    for (input, rule, range) in [
        ("<b title=\"в дом\">в</b> дом", "nobr.nbsp_short_word", (17, 26)),
        ("<b title=\"a\">в</b> <i title=\"дом\">дом</i>", "nobr.nbsp_short_word", (13, 37)),
        ("<b title=\"10 кг\">10</b> кг", "abbr.nbsp_weight_unit", (17, 26)),
    ] {
        let input = units(input);
        let output = engine.process_with_diagnostics(&input, ALL).unwrap();
        assert_eq!(
            output.trace.as_ref().unwrap().iter().find(|entry| entry.rule == units(rule)).unwrap().ranges,
            [range]
        );
        assert_edits(&input, &output);
        assert_eq!(engine.process_utf16(&input).unwrap(), output.text);
    }
}

#[test]
fn scalar_rollback_keeps_earlier_events_and_accepts_later_values() {
    for (format, input, expected) in [
        ("yaml", "first: a\nsecond: 'c'\nthird: e\nlast: a\n", "first: b\nsecond: 'c'\nthird: e\nlast: b\n"),
        ("toml", "first = 'a'\nsecond = 'c'\nlast = 'a'\n", "first = 'b'\nsecond = 'c'\nlast = 'b'\n"),
    ] {
        let mut engine = MicroTypo::new(Some(&json!({"input":format,"presets":false})), HOST).unwrap();
        engine
            .register_rule_group(
                group(vec![
                    Rule::pattern(
                        "replace",
                        Re::try_new("[ac]", "g").unwrap(),
                        Replacement::func(|m, _| Ok(if m.whole() == [97] { vec![98] } else { vec![100] })),
                    ),
                    Rule::patterns(
                        "guard",
                        vec![
                            (Re::try_new("d", "g").unwrap(), Replacement::tpl("'")),
                            (Re::try_new("e", "g").unwrap(), Replacement::tpl("x: y")),
                        ],
                    ),
                ]),
                Some("custom"),
                Position::End,
            )
            .unwrap();
        let input = units(input);
        let output = engine.process_with_diagnostics(&input, ALL).unwrap();
        assert_eq!(output.text, units(expected));
        let trace = output.trace.as_ref().unwrap();
        assert_eq!(trace.len(), 1);
        assert_eq!(trace[0].rule, units("custom.replace"));
        assert_eq!(trace[0].count, 2);
        assert_edits(&input, &output);
        assert_eq!(engine.process_utf16(&input).unwrap(), output.text);
    }
}

type Ranges<'a> = &'a [(&'a str, &'a [(usize, usize)])];

fn assert_trace(config: &serde_json::Value, input: &str, expected: Ranges<'_>) {
    let mut engine = MicroTypo::new(Some(config), HOST).unwrap();
    let input = units(input);
    let output = engine.process_with_diagnostics(&input, ALL).unwrap();
    let trace: Vec<_> = output
        .trace
        .as_ref()
        .unwrap()
        .iter()
        .map(|entry| (String::from_utf16_lossy(&entry.rule), entry.ranges.clone()))
        .collect();
    let expected: Vec<_> = expected.iter().map(|(rule, ranges)| ((*rule).to_owned(), ranges.to_vec())).collect();
    assert_eq!(trace, expected, "{config} {}", String::from_utf16_lossy(&input));
    assert_edits(&input, &output);
}

#[test]
fn range_after_dash_over_protected_link_reaches_the_rewrite_end() {
    let forms: &[(&str, Ranges<'_>)] = &[
        ("a www.a -", &[("dash.em", &[(2, 9)]), ("nobr.nbsp_short_word", &[(0, 9)])]),
        ("a www.a - b", &[("dash.em", &[(2, 10)]), ("nobr.nbsp_short_word", &[(0, 10)])]),
        (
            "a www.a -- b",
            &[("dash.em_double_hyphen", &[(7, 10)]), ("dash.em", &[(2, 11)]), ("nobr.nbsp_short_word", &[(0, 11)])],
        ),
        ("a www.a —", &[("dash.em", &[(2, 9)]), ("nobr.nbsp_short_word", &[(0, 9)])]),
        ("a www.a — b", &[("dash.em", &[(2, 10)]), ("nobr.nbsp_short_word", &[(0, 10)])]),
        ("в www.a -", &[("dash.em", &[(2, 9)]), ("nobr.nbsp_short_word", &[(0, 9)])]),
        ("в www.amber.ru - город", &[("dash.em", &[(2, 17)]), ("nobr.nbsp_short_word", &[(0, 17)])]),
        ("Он в www.a - и", &[("dash.em", &[(5, 13)]), ("nobr.nbsp_short_word", &[(0, 13)])]),
        ("в www.a - в www.b - c", &[("dash.em", &[(2, 10), (12, 20)]), ("nobr.nbsp_short_word", &[(0, 10), (2, 20)])]),
        ("в http://a.ru - b", &[("dash.em", &[(2, 16)]), ("nobr.nbsp_short_word", &[(0, 16)])]),
        ("к user@mail.ru - b", &[("dash.em", &[(2, 17)]), ("nobr.nbsp_short_word", &[(0, 17)])]),
        ("a www.a", &[("nobr.nbsp_short_word", &[(0, 7)])]),
        ("a site.ru -", &[("dash.em", &[(8, 11)]), ("nobr.nbsp_short_word", &[(0, 6)])]),
        ("слово www.a - b", &[("dash.em", &[(6, 14)])]),
        ("a «www.a» - b", &[("dash.em", &[(8, 12)]), ("space.nbsp_before_quote", &[(0, 2)])]),
        ("a www.a b - c", &[("dash.em", &[(8, 12)]), ("nobr.nbsp_short_word", &[(0, 7)])]),
    ];
    for (input, expected) in forms {
        assert_trace(&json!({}), input, expected);
    }
    let without_em: &[(&str, Ranges<'_>)] = &[
        ("a www.a -", &[("nobr.nbsp_short_word", &[(0, 7)])]),
        ("a www.a -- b", &[("dash.em_double_hyphen", &[(7, 10)]), ("nobr.nbsp_short_word", &[(0, 7)])]),
        ("a www.a — b", &[("nobr.nbsp_short_word", &[(0, 7)])]),
        ("в www.amber.ru - город", &[("nobr.nbsp_short_word", &[(0, 14)])]),
        ("Он в www.a - и", &[("nobr.nbsp_short_word", &[(0, 10)])]),
        ("в www.a - в www.b - c", &[("nobr.nbsp_short_word", &[(0, 7), (9, 17)])]),
        ("в http://a.ru - b", &[("nobr.nbsp_short_word", &[(0, 13)])]),
        ("к user@mail.ru - b", &[("nobr.nbsp_short_word", &[(0, 14)])]),
    ];
    for (input, expected) in without_em {
        assert_trace(&json!({"rules":{"dash.em":false}}), input, expected);
        let undashed: Vec<_> = expected.iter().copied().filter(|(rule, _)| !rule.starts_with("dash.")).collect();
        assert_trace(&json!({"rules":{"dash":false}}), input, &undashed);
    }
}
