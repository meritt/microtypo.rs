use std::time::{Duration, Instant};

use microtypo_conformance::HOST;
use microtypo_core::{ErrorKind, MicroTypo, Position, Re, Replacement, Rule, RuleGroupDef};
use serde_json::json;

use crate::timing::alone;

fn budget_error_within(engine: &mut MicroTypo, text: &str, limit: Duration) {
    let _alone = alone();
    let start = Instant::now();
    let result = engine.process(text);
    let elapsed = start.elapsed();
    let error = result.expect_err("the budget runs out");
    assert_eq!(error.kind, ErrorKind::Budget, "{}", error.message);
    assert!(error.message.starts_with("Processing exceeded 200ms budget at "), "{}", error.message);
    assert!(elapsed < limit, "the budget error came after {elapsed:?}");
}

#[test]
fn a_catastrophic_user_rule_ends_with_the_budget_error_in_time() {
    let mut engine = MicroTypo::new(Some(&json!({ "maxProcessingMs": 200 })), HOST).unwrap();
    let group = RuleGroupDef {
        name: Some("nested".to_owned()),
        rules: vec![Rule::pattern("nested", Re::try_new("(a+)+$", "g").unwrap(), Replacement::tpl("b"))],
        ..RuleGroupDef::default()
    };
    engine.register_rule_group(group, None, Position::End).unwrap();
    budget_error_within(&mut engine, &format!("{}b", "a".repeat(40)), Duration::from_millis(400));
    assert_eq!(engine.process("aab").unwrap(), "aab");
}

#[test]
fn a_catastrophic_built_in_pattern_ends_with_the_budget_error_in_time() {
    let config = json!({ "input": "markdown", "maxProcessingMs": 200, "maxInputLength": 60_000 });
    let mut engine = MicroTypo::new(Some(&config), HOST).unwrap();
    budget_error_within(&mut engine, &" ".repeat(60_000), Duration::from_millis(400));
}
