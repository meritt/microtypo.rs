use std::sync::atomic::{AtomicU32, Ordering};

use microtypo_conformance::HOST;
use microtypo_core::{ErrorKind, Host, MicroTypo, Position, Rule, RuleGroupDef};
use serde_json::json;

#[test]
fn the_budget_runs_on_the_clock_of_the_host() {
    static READINGS: AtomicU32 = AtomicU32::new(0);
    let host = Host { now_ms: || f64::from(READINGS.fetch_add(1, Ordering::Relaxed)) * 1000.0, ..HOST };
    let mut engine = MicroTypo::new(Some(&json!({ "maxProcessingMs": 200 })), host).unwrap();
    let error = engine.process("Мы - здесь").expect_err("every reading is a second after the last");
    assert_eq!(error.kind, ErrorKind::Budget, "{}", error.message);
    assert!(READINGS.load(Ordering::Relaxed) >= 2);
    let mut unlimited = MicroTypo::new(Some(&json!({ "maxProcessingMs": 0 })), host).unwrap();
    assert_eq!(unlimited.process("Мы - здесь").unwrap(), "Мы\u{a0}— здесь");
}

#[test]
fn the_budget_stops_a_group_before_its_next_rule() {
    thread_local! {
        static CLOCK: std::cell::Cell<f64> = const { std::cell::Cell::new(0.0) };
        static SECOND: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    }
    let host = Host { now_ms: || CLOCK.get(), ..HOST };
    for input in ["html", "markdown"] {
        for limit in [10, 0] {
            CLOCK.set(0.0);
            SECOND.set(0);
            let mut engine =
                MicroTypo::new(Some(&json!({"input":input,"presets":false,"maxProcessingMs":limit})), host).unwrap();
            let group = RuleGroupDef {
                name: Some("probe".to_owned()),
                rules: vec![
                    Rule::handler("first", |_| {
                        CLOCK.set(11.0);
                        Ok(None)
                    }),
                    Rule::handler("second", |_| {
                        SECOND.set(SECOND.get() + 1);
                        Ok(None)
                    }),
                ],
                ..RuleGroupDef::default()
            };
            engine.register_rule_group(group, None, Position::End).unwrap();
            if limit == 0 {
                assert_eq!(engine.process("Амбер").unwrap(), "Амбер");
                assert_eq!(SECOND.get(), 1);
            } else {
                let error = engine.process("Амбер").unwrap_err();
                assert_eq!(error.kind, ErrorKind::Budget);
                assert_eq!(error.details["where"], "group:probe");
                assert_eq!(SECOND.get(), 0);
            }
        }
    }
}

#[test]
fn the_budget_checks_an_unchanged_cycle_before_returning() {
    thread_local! {
        static CLOCK: std::cell::Cell<f64> = const { std::cell::Cell::new(0.0) };
    }
    CLOCK.set(0.0);
    let host = Host { now_ms: || CLOCK.get(), ..HOST };
    let mut engine = MicroTypo::new(Some(&json!({"presets":false,"maxProcessingMs":10})), host).unwrap();
    let group = RuleGroupDef {
        name: Some("probe".to_owned()),
        rules: vec![
            Rule::handler("same", |_| {
                CLOCK.set(11.0);
                Ok(None)
            })
            .cycled(),
        ],
        ..RuleGroupDef::default()
    };
    engine.register_rule_group(group, None, Position::End).unwrap();
    let error = engine.process("Амбер").unwrap_err();
    assert_eq!(error.kind, ErrorKind::Budget);
    assert_eq!(error.details["where"], "cycle");
}

#[test]
fn the_budget_checks_preparse_before_running_a_rule() {
    thread_local! {
        static CLOCK: std::cell::Cell<f64> = const { std::cell::Cell::new(0.0) };
        static RULES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    }
    CLOCK.set(0.0);
    RULES.set(0);
    let host = Host { now_ms: || CLOCK.get(), ..HOST };
    let mut engine = MicroTypo::new(Some(&json!({"presets":false,"maxProcessingMs":10})), host).unwrap();
    let group = RuleGroupDef {
        name: Some("probe".to_owned()),
        pre_parse: Some(std::sync::Arc::new(|_| {
            CLOCK.set(11.0);
            Ok(())
        })),
        rules: vec![Rule::handler("first", |_| {
            RULES.set(RULES.get() + 1);
            Ok(None)
        })],
        ..RuleGroupDef::default()
    };
    engine.register_rule_group(group, None, Position::End).unwrap();
    let error = engine.process("Амбер").unwrap_err();
    assert_eq!(error.kind, ErrorKind::Budget);
    assert_eq!(error.details["where"], "group:probe");
    assert_eq!(RULES.get(), 0);
}
