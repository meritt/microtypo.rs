use microtypo_conformance::HOST;
use microtypo_core::{MicroTypo, ProcessOptions};

const ALL: ProcessOptions = ProcessOptions { changes: true, edits: true, trace: true };

#[test]
fn data_strings_preserve_lone_units_in_matching_replacement_and_trace() {
    for unit in [0xd800, 0xdc00] {
        for flags in ["g", "gu", "gv"] {
            let config = format!(
                r#"{{"presets":false,"ruleGroups":[{{"name":"custom\ud800","rules":[{{"id":"replace\udc00","pattern":{{"source":"\u{unit:04x}","flags":"{flags}"}},"replacement":"\u{unit:04x}x"}}]}}]}}"#
            );
            let mut engine = MicroTypo::from_json(&config, HOST).unwrap();
            let result = engine.process_with_diagnostics(&[unit], ALL).unwrap();
            assert_eq!(result.text, [unit, 120]);
            assert_eq!(result.edits.unwrap()[0].text, [120]);
            let rule: Vec<_> =
                "custom".encode_utf16().chain([0xd800, 46]).chain("replace".encode_utf16()).chain([0xdc00]).collect();
            assert_eq!(result.trace.unwrap()[0].rule, rule);
        }
    }
}

#[test]
fn distinct_names_selectors_and_registration_share_exact_identity() {
    let mut engine = MicroTypo::from_json(r#"{"presets":false,"ruleGroups":[{"name":"custom\ud800","rules":[{"id":"r\ud800","pattern":{"source":"a","flags":"g"},"replacement":"b"},{"id":"r\udc00","pattern":{"source":"b","flags":"g"},"replacement":"c"},{"id":"r�","pattern":{"source":"c","flags":"g"},"replacement":"d"}]}],"rules":{"custom\ud800.r\ud800":false,"custom\ud800.r\udc00":false,"custom\ud800.r\ud800":true}}"#,HOST).unwrap();
    assert_eq!(engine.process("a").unwrap(), "b");
    engine.call_json("applyOptions", r#"[{"rules":{"custom\ud800.r\udc00":true}}]"#).unwrap();
    assert_eq!(engine.process("a").unwrap(), "d");
    engine.call_json("registerRuleGroup",r#"[{"name":"first\udc00","rules":[{"id":"r","pattern":{"source":"d","flags":"g"},"replacement":"e"}]},{"position":"after:custom\ud800"}]"#).unwrap();
    assert_eq!(engine.process("a").unwrap(), "e");
    let name: Vec<_> = "custom".encode_utf16().chain([0xd800]).collect();
    let group = engine.get_rule_group_utf16(&name).unwrap();
    assert_eq!(group.name, name);
    assert_ne!(group.rules[0].id, group.rules[1].id);
    assert_ne!(group.rules[0].id, group.rules[2].id);
    assert!(group.to_json().contains(r#""id":"r\ud800""#));
    assert_eq!(engine.list_rule_groups()[1].name, "first".encode_utf16().chain([0xdc00]).collect::<Vec<_>>());
}

#[test]
fn replacement_literal_escape_replacement_character_and_private_use_do_not_collide() {
    for (replacement, expected) in [(r"\\ud800", "\\ud800"), ("�", "�"), ("\u{e000}", "\u{e000}")] {
        let config = format!(
            r#"{{"presets":false,"ruleGroups":[{{"name":"custom","rules":[{{"id":"r","pattern":{{"source":"a","flags":"g"}},"replacement":"{replacement}"}}]}}]}}"#
        );
        let mut engine = MicroTypo::from_json(&config, HOST).unwrap();
        assert_eq!(engine.process("a").unwrap(), expected);
    }
}

#[test]
fn the_last_selector_object_replaces_earlier_objects() {
    let config = r#"{"presets":false,"ruleGroups":[{"name":"custom","rules":[{"id":"r","pattern":{"source":"a","flags":"g"},"replacement":"b"}]}],"rules":{"custom":false},"rules":{}}"#;
    let mut engine = MicroTypo::from_json(config, HOST).unwrap();
    assert_eq!(engine.process("a").unwrap(), "b");
    engine.call_json("applyOptions", r#"[{"rules":{"custom":false},"rules":{}}]"#).unwrap();
    assert_eq!(engine.process("a").unwrap(), "b");
}

#[test]
fn mixed_scalar_and_lone_pattern_uses_the_selected_atom_encoding() {
    for (flags, expected, count) in
        [("g", vec![120, 120, 120], 3), ("gu", vec![120, 120], 2), ("gv", vec![120, 120], 2)]
    {
        let config = format!(
            r#"{{"presets":false,"input":"plain","ruleGroups":[{{"name":"custom","rules":[{{"id":"r","pattern":{{"source":"[😀\ud800]","flags":"{flags}"}},"replacement":"x"}}]}}]}}"#
        );
        let mut engine = MicroTypo::from_json(&config, HOST).unwrap();
        let result = engine.process_with_diagnostics(&[0xd83d, 0xde00, 0xd800], ALL).unwrap();
        assert_eq!(result.text, expected);
        assert_eq!(result.trace.unwrap()[0].count, count);
    }
}

#[test]
fn surrogate_escape_pair_probe_restores_the_next_atom_on_failure() {
    for flags in ["g", "gu", "gv"] {
        for (source, input) in [
            (r"\uD800\u0041", vec![0xd800, 65]),
            (r"\uD800\uD801", vec![0xd800, 0xd801]),
            (r"\uD83D\uDE00", vec![0xd83d, 0xde00]),
        ] {
            let config = serde_json::json!({"presets":false,"input":"plain","ruleGroups":[{"name":"custom","rules":[{"id":"r","pattern":{"source":source,"flags":flags},"replacement":"x"}]}]});
            let mut engine = MicroTypo::new(Some(&config), HOST).unwrap();
            let output = engine.process_with_diagnostics(&input, ALL).unwrap();
            assert_eq!(output.text, [120], "/{source}/{flags}");
            assert_eq!(output.trace.unwrap()[0].count, 1);
        }
    }
}
