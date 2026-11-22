use std::cell::Cell;

use microtypo_conformance::HOST;
use microtypo_core::{ErrorKind, Host, MicroTypo, Position, ProcessOptions, Re, Replacement, Rule, RuleGroupDef};
use serde_json::{Value, json};

thread_local! {
    static READS: Cell<usize> = const { Cell::new(0) };
    static JUMP: Cell<usize> = const { Cell::new(usize::MAX) };
}

fn clock() -> f64 {
    let read = READS.get();
    READS.set(read + 1);
    if read < JUMP.get() { 0.0 } else { 1000.0 }
}

const CLOCKED: Host = Host { now_ms: clock, ..HOST };

fn engine(config: &Value) -> MicroTypo {
    MicroTypo::new(Some(config), CLOCKED).unwrap()
}

fn stages(make: &dyn Fn() -> MicroTypo, text: &str) -> Vec<String> {
    JUMP.set(usize::MAX);
    READS.set(0);
    make().process(text).unwrap();
    let total = READS.get();
    let mut stages: Vec<String> = Vec::new();
    for at in 0..=total {
        let mut engine = make();
        JUMP.set(at);
        READS.set(0);
        let stage = match engine.process(text) {
            Ok(_) => "ok".to_owned(),
            Err(error) => {
                assert_eq!(error.kind, ErrorKind::Budget, "{}", error.message);
                let stage = error.details["where"].as_str().unwrap().to_owned();
                assert_eq!(
                    error.message,
                    format!("Processing exceeded 10ms budget at {stage} \u{2014} likely adversarial input")
                );
                stage
            }
        };
        JUMP.set(usize::MAX);
        if stages.last() != Some(&stage) {
            stages.push(stage);
        }
    }
    stages
}

const SKIPPABLE: &str = r#"{"a":["слово","Иван","дело"]}"#;

#[test]
fn a_skipped_value_checks_the_budget_at_data_value() {
    let json = json!({"input":"json","maxProcessingMs":10});
    assert_eq!(
        stages(&|| engine(&json), SKIPPABLE),
        ["ok", "json-parse", "json-scan", "select", "data-value", "restore", "ok"]
    );
    assert_eq!(
        stages(&|| engine(&json!({"input":"json","maxProcessingMs":10,"rules":{"nobr":false}})), SKIPPABLE),
        ["ok", "json-parse", "json-scan", "select", "data-value", "restore", "ok"]
    );
    assert_eq!(
        stages(&|| engine(&json!({"input":"yaml","maxProcessingMs":10})), "a: слово\nb: Иван\n"),
        ["ok", "yaml-scan", "select", "data-value", "restore", "ok"]
    );
    assert_eq!(
        stages(&|| engine(&json!({"input":"toml","maxProcessingMs":10})), "a = \"слово\"\nb = \"Иван\"\n"),
        ["ok", "toml-scan", "select", "data-value", "restore", "ok"]
    );
}

#[test]
fn a_value_a_rule_can_read_runs_between_the_skipped_ones() {
    let json = json!({"input":"json","maxProcessingMs":10});
    let mixed = stages(&|| engine(&json), r#"{"a":["слово","Мы - здесь","Иван"]}"#);
    let select = mixed.iter().position(|stage| stage == "select").unwrap();
    assert_eq!(mixed[select + 1..select + 3], ["data-value", "safeSequences.protect"]);
    assert_eq!(mixed[mixed.len() - 4..], ["group:nobr", "data-value", "restore", "ok"]);

    let edges = stages(&|| engine(&json), "{\"a\":[\" слово \",\"  \",\"\u{2010}\"]}");
    let select = edges.iter().position(|stage| stage == "select").unwrap();
    assert_eq!(edges[select + 1..select + 3], ["data-value", "safeSequences.protect"]);
    assert_eq!(edges.iter().filter(|stage| *stage == "data-value").count(), 1);
    assert_eq!(edges[edges.len() - 3..], ["group:text", "restore", "ok"]);
}

#[test]
fn nothing_is_skipped_with_custom_protection_a_custom_group_or_a_cycle_hook() {
    let json = json!({"input":"json","maxProcessingMs":10});
    let skipped = |make: &dyn Fn() -> MicroTypo| stages(make, SKIPPABLE).iter().any(|stage| stage == "data-value");
    assert!(skipped(&|| engine(&json)));
    assert!(!skipped(&|| {
        let mut engine = engine(&json);
        engine.add_safe_tag("x").unwrap();
        engine
    }));
    assert!(!skipped(&|| {
        let mut engine = engine(&json);
        engine.add_safe_block("b", "[[", "]]", false).unwrap();
        engine
    }));
    assert!(!skipped(&|| {
        let mut engine = engine(&json);
        engine.call("addSafeBlock", &[json!({"id":"b","open":"[[","close":"]]"})]).unwrap();
        engine
    }));
    assert!(!skipped(&|| {
        let mut engine = engine(&json);
        let group = RuleGroupDef {
            name: Some("mine".to_owned()),
            rules: vec![Rule::pattern("r", Re::try_new("q", "g").unwrap(), Replacement::tpl("q"))],
            ..RuleGroupDef::default()
        };
        engine.register_rule_group(group, None, Position::End).unwrap();
        engine
    }));
    assert!(!skipped(&|| {
        let mut engine = engine(&json);
        engine.set_on_cycle_limit(Some(std::sync::Arc::new(|_, _| {})));
        engine
    }));
    assert!(!skipped(&|| engine(&json!({"input":"markdown","maxProcessingMs":10}))));
}

#[test]
fn documents_with_skipped_values_keep_output_and_diagnostics() {
    for line in include_str!("fixtures/data_value_skip.jsonl").lines() {
        let case: Value = serde_json::from_str(line).unwrap();
        let mut engine = MicroTypo::new(Some(&case["config"]), HOST).unwrap();
        let input: Vec<u16> = case["text"].as_str().unwrap().encode_utf16().collect();
        let output = engine
            .process_with_diagnostics(&input, ProcessOptions { changes: true, edits: true, trace: true })
            .unwrap();
        let output: Value = serde_json::from_str(&output.to_json()).unwrap();
        assert_eq!(output, case["expect"], "{line}");
    }
}
