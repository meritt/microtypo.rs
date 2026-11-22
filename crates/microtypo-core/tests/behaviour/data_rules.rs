use microtypo_conformance::HOST;
use microtypo_core::{Config, MicroTypo};
use serde_json::json;

#[test]
fn configured_rules_use_real_selectors_and_repeat_safely() {
    let config = json!({"presets":false,"ruleGroups":[{"name":"terms","rules":[{"id":"amber","pattern":{"source":"Амбер","flags":"gu"},"replacement":"Арден"}]}],"rules":{"terms":false,"terms.a*":true}});
    let typed: Config = serde_json::from_value(config.clone()).unwrap();
    assert_eq!(typed.to_json(), config);
    let mut engine = MicroTypo::with_config(typed, HOST).unwrap();
    assert_eq!(engine.process("Амбер Амбер").unwrap(), "Арден Арден");
    engine.apply_options(Some(&json!({"rules":{"terms":false}}))).unwrap();
    assert_eq!(engine.process("Амбер").unwrap(), "Амбер");
    engine.apply_options(Some(&json!({"rules":{"terms.amber":true}}))).unwrap();
    assert_eq!(engine.process("Амбер").unwrap(), "Арден");
    assert!(engine.apply_options(Some(&json!({"ruleGroups":[]}))).is_err());
    assert_eq!(engine.process("Амбер").unwrap(), "Арден");
}

#[test]
fn registered_pattern_arrays_run_in_order_and_keep_captures() {
    let mut engine = MicroTypo::new(Some(&json!({"presets":false})), HOST).unwrap();
    engine.call("registerRuleGroup",&[json!({"name":"terms","rules":[{"id":"letters","pattern":[{"source":"(a)","flags":"g"},{"source":"b","flags":"g"}],"replacement":["$1b","c"]}]})]).unwrap();
    assert_eq!(engine.process("aa").unwrap(), "acac");
    engine.apply_options(Some(&json!({"rules":{"terms.letters":false}}))).unwrap();
    assert_eq!(engine.process("aa").unwrap(), "aa");
}

#[test]
fn malformed_descriptors_and_flags_raise_typed_errors() {
    for pattern in [
        json!({"source":"a","flags":"gg"}),
        json!({"source":"a","flags":"uv"}),
        json!({"source":"[","flags":"g"}),
        json!({"source":"a","flags":"g","extra":true}),
    ] {
        let config =
            json!({"ruleGroups":[{"name":"terms","rules":[{"id":"bad","pattern":pattern,"replacement":"b"}]}]});
        let error = MicroTypo::new(Some(&config), HOST).err().unwrap();
        assert_eq!(error.name(), "MicroTypoConfigError");
    }
}

#[test]
fn invalid_patterns_keep_ecmascript_error_reasons() {
    for (source, flags, reason) in [
        ("(", "g", "Unterminated group"),
        (")", "u", "Unmatched ')'"),
        ("[", "g", "Unterminated character class"),
        ("a{2,1}", "g", "numbers out of order in {} quantifier"),
        ("(?<1>a)", "u", "Invalid capture group name"),
        (r"\u{110000}", "u", "Invalid Unicode escape"),
        (r"\xZ", "u", "Invalid escape"),
        (r"\1", "u", "Invalid escape"),
        (r"\c1", "u", "Invalid Unicode escape"),
        (r"[\q]", "gv", "Invalid escape"),
        ("[\\", "gv", "\\ at end of pattern"),
        ("[\\q{\\", "gv", "\\ at end of pattern"),
        (r"[\q{", "gv", "Unterminated character class"),
        (r"[\q{a", "gv", "Unterminated character class"),
        (r"[\q{\c", "gv", "Invalid Unicode escape"),
        (r"[\q{\x", "gv", "Invalid escape"),
        (r"[\q{\u", "gv", "Invalid Unicode escape"),
        ("[a&&]", "v", "Invalid character in character class"),
        ("[a--]", "v", "Invalid character in character class"),
        ("[a&&", "v", "Unterminated character class"),
        ("[a--", "v", "Unterminated character class"),
        ("[&&a]", "v", "Invalid set operation in character class"),
        ("(?x:a)", "u", "Invalid group"),
        (r"(?<name>a)\k<other>", "u", "Invalid named capture referenced"),
        ("a{", "u", "Incomplete quantifier"),
        ("*", "g", "Nothing to repeat"),
        ("{3,5}", "g", "Nothing to repeat"),
        ("{3,5}", "u", "Nothing to repeat"),
        ("{3,5}", "v", "Nothing to repeat"),
        ("{3", "v", "Lone quantifier brackets"),
        ("a{3", "v", "Incomplete quantifier"),
        ("]", "v", "Lone quantifier brackets"),
        (r"[a-\d]", "u", "Invalid character class"),
        (r"[a-\d]", "gv", "Invalid character class"),
        (r"[a-\d]", "giv", "Invalid character class"),
        (r"[\d-a]", "gv", "Invalid character class"),
        (r"[\d-a]", "giv", "Invalid character class"),
        ("[z-a]", "u", "Range out of order in character class"),
        ("[z-a]", "v", "Range out of order in character class"),
        (r"[\d-a]", "u", "Invalid character class"),
        ("(?<=a)+", "u", "Invalid quantifier"),
        ("(?=a)+", "u", "Invalid quantifier"),
        (r"\pX", "u", "Invalid property name"),
        ("[a&&b-c]", "v", "Invalid set operation in character class"),
        ("[a--b&&c]", "v", "Invalid set operation in character class"),
        (r"[^\q{ab}]", "v", "Negated character class may contain strings"),
        (r"[^\q{}]", "v", "Negated character class may contain strings"),
        (r"[^[[\q{ab}]--[\q{ab}]]]", "v", "Negated character class may contain strings"),
        (r"[^[[\q{}]--[\q{}]]]", "v", "Negated character class may contain strings"),
    ] {
        let config = json!({"presets":false,"ruleGroups":[{"name":"terms","rules":[{"id":"bad","pattern":{"source":source,"flags":flags},"replacement":"x"}]}]});
        let error = MicroTypo::new(Some(&config), HOST).err().unwrap();
        assert_eq!(error.name(), "MicroTypoConfigError");
        assert_eq!(error.code(), Some("ERR_MICROTYPO_CONFIG"));
        assert_eq!(
            error.message,
            format!("invalid rule pattern: Invalid regular expression: /{source}/{flags}: {reason}")
        );
        assert_eq!(error.details, json!({"source":source,"flags":flags}));
    }
    for (source, flags) in [("[a--b]", "v"), (r"(a)\1", "u"), (r"(?<name>a)\k<name>", "u"), (r"\x61", "u")] {
        let config = json!({"presets":false,"ruleGroups":[{"name":"terms","rules":[{"id":"valid","pattern":{"source":source,"flags":flags},"replacement":"x"}]}]});
        let mut engine = MicroTypo::new(Some(&config), HOST).unwrap();
        assert_eq!(engine.process("aa").unwrap(), if source == "[a--b]" || source == r"\x61" { "xx" } else { "x" });
    }
}

#[test]
fn rejected_decimal_escapes_preserve_context_and_registration_recovery() {
    let config = json!({"presets":false,"input":"plain","ruleGroups":[{"name":"custom","rules":[{"id":"r","pattern":{"source":r"[\b]","flags":"gv"},"replacement":"x"}]}]});
    let mut engine = MicroTypo::new(Some(&config), HOST).unwrap();
    let options = microtypo_core::ProcessOptions { trace: true, edits: true, changes: true };
    let expected = json!({"text":"bx","trace":[{"rule":"custom.r","kind":"regex","count":1,"ranges":[[1,2]]}],"changes":[[1,2]],"edits":[{"start":1,"end":2,"text":"x"}]});
    for (source, flags, reason) in [
        (r"[\1]", "gv", "Invalid decimal escape"),
        (r"[\7]", "gu", "Invalid decimal escape"),
        (r"[\q{\7}]", "giv", "Invalid decimal escape"),
        (r"[a-\1]", "gv", "Invalid decimal escape"),
        (r"[\00]", "gv", "Invalid decimal escape"),
        (r"[\q{\09}]", "gv", "Invalid decimal escape"),
        (r"\08", "gu", "Invalid decimal escape"),
        (r"\01", "gv", "Invalid decimal escape"),
        (r"[\8]", "gv", "Invalid escape"),
        (r"[\q{\9}]", "gv", "Invalid escape"),
        (r"\1", "gv", "Invalid escape"),
        (r"[\q]", "gv", "Invalid escape"),
        ("[\\", "gv", "\\ at end of pattern"),
        ("[\\q{\\", "gv", "\\ at end of pattern"),
        (r"[\q{a", "gv", "Unterminated character class"),
        (r"[\q{\u", "gv", "Invalid Unicode escape"),
    ] {
        let group =
            json!({"name":"custom","rules":[{"id":"bad","pattern":{"source":source,"flags":flags},"replacement":"y"}]});
        let error = engine.call("registerRuleGroup", std::slice::from_ref(&group)).unwrap_err();
        let constructor = MicroTypo::new(Some(&json!({"presets":false,"ruleGroups":[group]})), HOST).err().unwrap();
        for error in [error, constructor] {
            assert_eq!(error.name(), "MicroTypoConfigError");
            assert_eq!(error.code(), Some("ERR_MICROTYPO_CONFIG"));
            assert_eq!(
                error.message,
                format!("invalid rule pattern: Invalid regular expression: /{source}/{flags}: {reason}")
            );
            assert_eq!(error.details, json!({"source":source,"flags":flags}));
        }
        let actual: serde_json::Value =
            serde_json::from_str(&engine.process_with_diagnostics(&[98, 8], options).unwrap().to_json()).unwrap();
        assert_eq!(actual, expected);
    }
    for (source, flags, input, expected) in [
        (r"\0", "gu", "\0a", "xa"),
        (r"\0a", "gv", "\0a", "x"),
        (r"[\0]", "gv", "\0a", "xa"),
        (r"[\q{\0}]", "gv", "\0a", "xa"),
        (r"\01", "g", "\u{1}a", "xa"),
        (r"[\07]", "g", "\u{7}a", "xa"),
        (r"\08", "g", "\08", "x"),
        (r"\8", "g", "8a", "xa"),
        (r"(a)\1", "gu", "aa", "x"),
        (r"(a)\1", "gv", "aa", "x"),
    ] {
        let config = json!({"presets":false,"input":"plain","ruleGroups":[{"name":"custom","rules":[{"id":"r","pattern":{"source":source,"flags":flags},"replacement":"x"}]}]});
        assert_eq!(MicroTypo::new(Some(&config), HOST).unwrap().process(input).unwrap(), expected, "/{source}/{flags}");
    }
}

#[test]
fn unicode_sets_preserve_strings_negation_and_casefolded_operations() {
    for (source, flags, input, expected, count) in [
        (r"[\q{}]", "gv", "b", "xbx", 2),
        ("[\\q{A}]", "giv", "aA", "xA", 1),
        ("[\\q{ſ}]", "giv", "ſsS", "ſxS", 1),
        ("[^\\q{a}]", "giv", "aA", "ax", 1),
        ("[[a-z]--[\\q{a}]]", "giv", "abAB1😀ſ\u{212a}!", "axxx1😀xx!", 5),
        ("[[a-z]&&[\\q{A}]]", "giv", "aA", "xA", 1),
        ("[[\\q{A}]]", "giv", "aA", "xA", 1),
        ("[a\\q{A}]", "giv", "aA", "xx", 2),
        ("[\\q{Ab|A|}]", "giv", "ABaA", "xxxAx", 4),
        ("[^\\W]", "giu", "aA😀ſ\u{212a}", "xx😀xx", 4),
        ("[\\W]", "giu", "aA😀ſ\u{212a}!", "aAxſ\u{212a}x", 2),
        ("[a\\W]", "giu", "aA😀ſ\u{212a}!", "xxxſ\u{212a}x", 4),
        ("😀", "g", "😀", "x", 1),
        ("😀", "gu", "😀", "x", 1),
        ("[😀]", "g", "😀", "xx", 2),
        ("[😀]", "gu", "😀", "x", 1),
        ("😀+", "g", "😀😀", "xx", 2),
        ("😀+", "gu", "😀😀", "x", 1),
        ("\\ud83d\\ude00", "g", "😀", "x", 1),
        ("\\ud83d\\ude00+", "g", "😀😀", "xx", 2),
        ("\\u{1F600}", "g", "u{1F600} 😀", "x 😀", 1),
        ("(?<\\u{61}>a)\\k<a>", "g", "aa", "x", 1),
        ("(?<𐐀>a)\\k<𐐀>", "g", "aa", "x", 1),
        ("(?<\\u{10400}>a)\\k<𐐀>", "g", "aa", "x", 1),
        ("(?<\\uD801\\uDC00>a)\\k<𐐀>", "g", "aa", "x", 1),
        (r"[\q{}]", "gv", "😀", "x😀x", 2),
        (r"[a&]", "gv", "a&b", "xxb", 2),
        (r"[\q{😀}]", "gv", "😀b", "xb", 1),
        (r".", "gv", "😀b", "xx", 2),
        (r"[^\P{Lowercase_Letter}]", "giv", "Aa1", "xx1", 2),
        (r"[\q{a|}]", "gv", "b", "xbx", 2),
        (r"[\q{a|}]", "gv", "a", "xx", 2),
        (r"[\q{a|}]", "gv", "ab", "xxbx", 3),
        (r"[a\q{}]", "gv", "ab", "xxbx", 3),
        (r"[a\q{ab|}]", "gv", "a", "xx", 2),
        (r"[a\q{ab|}]", "gv", "ab", "xx", 2),
        (r"[^\q{a}]", "gv", "ab", "ax", 1),
        (r"[\q{ab|a}]", "gv", "ab a", "x x", 2),
        (r"\P{Lowercase_Letter}", "giv", "Aa1", "Aax", 1),
        (r"\p{Lowercase_Letter}", "giv", "Aa1", "xx1", 2),
        (r"\P{Lowercase_Letter}", "giu", "Aa1", "xxx", 3),
        (r"[^[[\q{a}]--[\q{a}]]]", "gv", "abc", "xxx", 3),
        (r"[^[[\q{ab}]&&[a]]]", "gv", "abc", "xxx", 3),
        (r"[[\q{Ab}]&&[\q{aB}]]", "giv", "AB ab", "x x", 2),
        (r"[[a-z]--[A-Z]]", "giv", "aA1", "aA1", 0),
        (r"\W", "giv", "w\u{212a}ſ1!", "w\u{212a}ſ1x", 1),
    ] {
        let config = json!({"presets":false,"input":{"format":"plain","placeholders":"off"},"ruleGroups":[{"name":"custom","rules":[{"id":"r","pattern":{"source":source,"flags":flags},"replacement":"x"}]}]});
        let mut engine = MicroTypo::new(Some(&config), HOST).unwrap();
        let input: Vec<_> = input.encode_utf16().collect();
        let result = engine
            .process_with_diagnostics(
                &input,
                microtypo_core::ProcessOptions { trace: true, edits: true, changes: true },
            )
            .unwrap();
        assert_eq!(result.text, expected.encode_utf16().collect::<Vec<_>>(), "/{source}/{flags}");
        assert_eq!(
            result.trace.as_ref().unwrap().iter().map(|entry| entry.count).sum::<usize>(),
            count,
            "/{source}/{flags}"
        );
        let mut rebuilt = Vec::new();
        let mut cursor = 0;
        for edit in result.edits.unwrap() {
            rebuilt.extend_from_slice(&input[cursor..edit.start]);
            rebuilt.extend(edit.text);
            cursor = edit.end;
        }
        rebuilt.extend_from_slice(&input[cursor..]);
        assert_eq!(rebuilt, result.text);
        assert!(engine.call("registerRuleGroup",&[json!({"name":"custom","rules":[{"id":"bad","pattern":{"source":"(","flags":"v"},"replacement":"y"}]})]).is_err());
        assert_eq!(engine.process_utf16(&input).unwrap(), result.text);
    }
}

#[test]
fn data_rules_preserve_generated_paragraph_tokens() {
    let config = json!({"html":true,"ruleGroups":[{"name":"custom","rules":[{"id":"amber","pattern":{"source":"Amber","flags":"g"},"replacement":"Shadow"}]}]});
    let mut engine = MicroTypo::new(Some(&config), HOST).unwrap();
    assert_eq!(engine.process("Amber").unwrap(), "<p>Shadow</p>");
}

#[test]
fn data_rules_match_the_empty_string_at_the_end_of_the_text() {
    for line in include_str!("fixtures/end_of_input_match.jsonl").lines() {
        let case: serde_json::Value = serde_json::from_str(line).unwrap();
        let mut engine = MicroTypo::new(Some(&case["config"]), HOST).unwrap();
        let input: Vec<u16> = case["text"].as_str().unwrap().encode_utf16().collect();
        let options = microtypo_core::ProcessOptions { changes: true, edits: true, trace: true };
        let output = engine.process_with_diagnostics(&input, options).unwrap();
        let output: serde_json::Value = serde_json::from_str(&output.to_json()).unwrap();
        assert_eq!(output, case["expect"], "{line}");
    }
}

#[test]
fn unicode_set_lexical_characters_share_escape_and_punctuation_rules() {
    let escapes = [
        (r"\b", '\u{8}'),
        (r"\n", '\n'),
        (r"\r", '\r'),
        (r"\t", '\t'),
        (r"\f", '\u{c}'),
        (r"\v", '\u{b}'),
        (r"\0", '\0'),
        (r"\cA", '\u{1}'),
        (r"\x42", 'B'),
        (r"\u0043", 'C'),
        (r"\u{1F600}", '😀'),
    ];
    let mut cases: Vec<(String, char)> = escapes.into_iter().map(|(s, c)| (s.to_owned(), c)).collect();
    cases.extend("&-!#%,:;<=>@`~()[]{}|/\\^$.*+?".chars().map(|c| (format!("\\{c}"), c)));
    for (escaped, character) in cases {
        let input: Vec<_> = format!("z{character}z").encode_utf16().collect();
        for source in [format!("[{escaped}]"), format!(r"[\q{{{escaped}}}]")] {
            let pattern = microtypo_core::Re::try_new(&source, "gv").unwrap();
            assert_eq!(pattern.replace_tpl(&input, &[120]), [122, 120, 122], "{source}");
            assert_eq!(
                pattern.find_all(&input).iter().map(|c| (c.index(), c.end())).collect::<Vec<_>>(),
                [(1, input.len() - 1)],
                "{source}"
            );
        }
    }
    for (source, input, expected, ranges) in [
        (r"[!#]", "!#$[]@", "xx$[]@", vec![(0, 1), (1, 2)]),
        (r"[!$]", "!#$[]@", "x#x[]@", vec![(0, 1), (2, 3)]),
        (r"[#@]", "!#$[]@", "!x$[]x", vec![(1, 2), (5, 6)]),
        (r"[\q{!#}]", "!#$[]@", "x$[]@", vec![(0, 2)]),
        (r"[\q{__}]", "__", "x", vec![(0, 2)]),
        (r"[^^]", "^a", "^x", vec![(1, 2)]),
        (r"[\b-\x0a]", "b\u{8}\t\n", "bxxx", vec![(1, 2), (2, 3), (3, 4)]),
        (r"[[\b]&&[\q{\b}]]", "b\u{8}", "bx", vec![(1, 2)]),
        (r"[[\b]--[\q{\b}]]", "b\u{8}", "b\u{8}", vec![]),
        (r"[^\q{\b}]", "b\u{8}", "x\u{8}", vec![(0, 1)]),
    ] {
        let pattern = microtypo_core::Re::try_new(source, "gv").unwrap();
        let input: Vec<_> = input.encode_utf16().collect();
        assert_eq!(pattern.replace_tpl(&input, &[120]), expected.encode_utf16().collect::<Vec<_>>(), "{source}");
        assert_eq!(
            pattern.find_all(&input).iter().map(|c| (c.index(), c.end())).collect::<Vec<_>>(),
            ranges,
            "{source}"
        );
    }
    for punctuator in "&!#$%*+,.:;<=>?@^`~".chars() {
        for source in [
            if punctuator == '^' { "[a^^]".to_owned() } else { format!("[{punctuator}{punctuator}]") },
            format!(r"[\q{{{punctuator}{punctuator}}}]"),
        ] {
            let error = microtypo_core::Re::try_new(&source, "gv").unwrap_err();
            assert_eq!(
                error.0,
                format!("Invalid regular expression: /{source}/gv: Invalid set operation in character class")
            );
        }
    }
    for character in ['(', ')', '[', ']', '{', '/', '-'] {
        let source = format!(r"[\q{{{character}}}]");
        let error = microtypo_core::Re::try_new(&source, "gv").unwrap_err();
        assert_eq!(error.0, format!("Invalid regular expression: /{source}/gv: Invalid character in character class"));
    }
}

#[test]
fn class_backspace_preserves_exact_diagnostics_and_word_boundary_controls() {
    for (source, flags, expected, ranges, changes, edits) in [
        (r"[\b]", "gv", "bx", vec![(1, 2)], json!([[1, 2]]), json!([{"start":1,"end":2,"text":"x"}])),
        (r"[\b]", "giv", "bx", vec![(1, 2)], json!([[1, 2]]), json!([{"start":1,"end":2,"text":"x"}])),
        (r"[\q{\b}]", "gv", "bx", vec![(1, 2)], json!([[1, 2]]), json!([{"start":1,"end":2,"text":"x"}])),
        (r"[\q{\b}]", "giv", "bx", vec![(1, 2)], json!([[1, 2]]), json!([{"start":1,"end":2,"text":"x"}])),
        (r"[\b]", "gu", "bx", vec![(1, 2)], json!([[1, 2]]), json!([{"start":1,"end":2,"text":"x"}])),
        (
            r"\b",
            "gu",
            "xbx\u{8}",
            vec![(0, 0), (1, 1)],
            json!([[0, 1], [2, 3]]),
            json!([{"start":0,"end":0,"text":"x"},{"start":1,"end":1,"text":"x"}]),
        ),
    ] {
        let config = json!({"presets":false,"input":"plain","ruleGroups":[{"name":"custom","rules":[{"id":"r","pattern":{"source":source,"flags":flags},"replacement":"x"}]}]});
        let mut engine = MicroTypo::new(Some(&config), HOST).unwrap();
        let options = microtypo_core::ProcessOptions { trace: true, edits: true, changes: true };
        let output = engine.process_with_diagnostics(&[98, 8], options).unwrap();
        let actual: serde_json::Value = serde_json::from_str(&output.to_json()).unwrap();
        assert_eq!(
            actual,
            json!({"text":expected,"trace":[{"rule":"custom.r","kind":"regex","count":ranges.len(),"ranges":ranges}],"changes":changes,"edits":edits}),
            "/{source}/{flags}"
        );
        let mut rebuilt = Vec::new();
        let mut at = 0;
        let input = [98, 8];
        for edit in output.edits.as_ref().unwrap() {
            rebuilt.extend_from_slice(&input[at..edit.start]);
            rebuilt.extend(&edit.text);
            at = edit.end;
        }
        rebuilt.extend_from_slice(&input[at..]);
        assert_eq!(rebuilt, output.text);
        assert!(engine.call("registerRuleGroup",&[json!({"name":"custom","rules":[{"id":"bad","pattern":{"source":r"[\q{[}]","flags":"gv"},"replacement":"y"}]})]).is_err());
        assert_eq!(engine.process_with_diagnostics(&input, options).unwrap().to_json(), output.to_json());
    }
}
