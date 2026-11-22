use microtypo_conformance::HOST;
use microtypo_core::{Config, ErrorKind, Input, MicroTypo};
use serde_json::json;

use crate::support::settles;

#[test]
fn plain_keeps_angle_text_literal_under_html_and_entities_options() {
    for html in [false, true] {
        let mut engine = MicroTypo::new(
            Some(&json!({"input":"plain","html":html,"entities":true,"render":{"paragraphs":true,"breakline":true}})),
            HOST,
        )
        .unwrap();
        for (source, expected) in [
            ("если a<b, то \"c\" - d", "если a<b, то\u{a0}«c»\u{a0}— d"),
            ("<code>Корвин - принц</code>", "<code>Корвин\u{a0}— принц</code>"),
            ("<notg>Корвин - принц</notg>", "<notg>Корвин\u{a0}— принц</notg>"),
            ("Назад <- сюда", "Назад ← сюда"),
        ] {
            settles(&mut engine, source, expected);
        }
    }
}

#[test]
fn localization_keeps_syntax_and_typesets_plural_branch_prose() {
    for (mode, source, expected) in [
        (
            "icu",
            "{name}: {n, plural, one {# день - \"текст\"} other {# дней - \"тексты\"}}",
            "{name}: {n, plural, one {# день\u{a0}— «текст»} other {# дней\u{a0}— «тексты»}}",
        ),
        ("icu", "L'homme {name} - текст", "L’homme {name}\u{a0}— текст"),
        (
            "icu",
            "{n, plural, one {'#{' день - текст} other {'#}' дней - тексты}}",
            "{n, plural, one {'#{' день\u{a0}— текст} other {'#}' дней\u{a0}— тексты}}",
        ),
        ("interpolation", "${var} - \"текст\"", "${var}\u{a0}— «текст»"),
        ("all", "${var} {name} %1$s - \"текст\"", "${var} {name} %1$s\u{a0}— «текст»"),
    ] {
        let mut engine = MicroTypo::new(Some(&json!({"input":{"placeholders":mode}})), HOST).unwrap();
        assert_eq!(engine.process(source).unwrap(), expected);
    }
    for placeholder in ["%s", "%1$s", "%@", "%d", "%05d", "%2$.*3$f", "%%"] {
        let mut engine = MicroTypo::new(Some(&json!({"input":{"placeholders":"printf"}})), HOST).unwrap();
        assert_eq!(
            engine.process(&format!("{placeholder} - \"текст\"")).unwrap(),
            format!("{placeholder}\u{a0}— «текст»")
        );
    }
}

#[test]
fn new_input_modes_validate_and_round_trip_through_typed_config() {
    for (key, value) in [
        ("placeholders", json!(true)),
        ("placeholders", json!([])),
        ("placeholders", json!("ICU")),
        ("pragmas", json!(true)),
        ("pragmas", json!("on")),
    ] {
        let mut input = json!({});
        input[key] = value;
        let error = MicroTypo::new(Some(&json!({"input":input})), HOST).unwrap_err();
        assert_eq!(error.kind, ErrorKind::Config);
        assert!(
            error.details["issues"]
                .as_array()
                .unwrap()
                .iter()
                .any(|issue| issue.as_str().unwrap().starts_with(&format!("input.{key}: expected one of")))
        );
    }
    let config = Config {
        input: Some(Input {
            placeholders: Some("all".to_owned()),
            pragmas: Some("microtypo".to_owned()),
            ..Input::default()
        }),
        ..Config::default()
    };
    let encoded = config.to_json();
    assert_eq!(encoded["input"]["placeholders"], "all");
    assert_eq!(encoded["input"]["pragmas"], "microtypo");
    assert_eq!(serde_json::from_value::<Config>(encoded).unwrap(), config);
    let mut engine = MicroTypo::new(Some(&json!({"input":{"placeholders":"printf"}})), HOST).unwrap();
    engine.apply_options(Some(&json!({"rules":{"symbol.arrows":false}}))).unwrap();
    assert_eq!(engine.process("%1$s - текст").unwrap(), "%1$s\u{a0}— текст");
    assert!(engine.apply_options(Some(&json!({"input":{"placeholders":"icu"}}))).is_err());
}

#[test]
fn gfm_delimiters_keep_their_hyphens_but_data_rows_remain_prose() {
    let mut engine = MicroTypo::new(Some(&json!({"input":"markdown"})), HOST).unwrap();
    for delimiter in ["| - | - |", "| -- | -- |", "| :- | -: |", "| :--: | --: |", "- | -"] {
        for prefix in ["", "> ", "- "] {
            let continuation = if prefix == "- " { "  " } else { prefix };
            let source =
                format!("{prefix}| Альфа | Бета |\n{continuation}{delimiter}\n{continuation}| Корвин - принц | Эрик |");
            let expected = source.replace("Корвин - принц", "Корвин\u{a0}— принц");
            settles(&mut engine, &source, &expected);
        }
    }
    assert_eq!(engine.process("| - | - |").unwrap(), "|\u{a0}— |\u{a0}— |");
    assert_eq!(
        engine.process("| Альфа | Бета |\n| - | - |\n| - | - |").unwrap(),
        "| Альфа | Бета |\n| - | - |\n|\u{a0}— |\u{a0}— |"
    );
}

#[test]
fn pragma_ignore_file_and_unclosed_tails_do_not_leak_into_the_next_call() {
    let start = "<!-- microtypo-ignore-start -->";
    let file = "<!-- microtypo-ignore-file -->";
    for format in ["text", "html", "markdown", "frontmatter"] {
        for html in [false, true] {
            let mut engine = MicroTypo::new(Some(&json!({"html":html,"input":{"format":format,"pragmas":"microtypo"},"render":{"paragraphs":true,"breakline":true}})), HOST).unwrap();
            let source = format!("\nКорвин - принц\r\n{file}\nКод - \"текст\"\r\n");
            assert_eq!(engine.process(&source).unwrap(), source);
            let tail = format!("{start}\nКод - \"текст\"\n");
            let source = format!("Корвин - принц\n{tail}");
            let output = engine.process(&source).unwrap();
            assert!(output.ends_with(&tail));
            assert!(output.contains("Корвин\u{a0}— принц"));
            assert_eq!(engine.process(&output).unwrap(), output);
        }
    }
}

#[test]
fn plain_inline_bytes_match_the_pinned_reference() {
    use microtypo_conformance::{Json, Replay, replay};

    let cases: Vec<_> = include_str!("fixtures/plain_inline.jsonl").lines().skip(1).collect();
    assert_eq!(cases.len(), 88);
    for line in cases {
        let case = Json::parse(line);
        let expected = case.get("expect").unwrap();
        match replay(&case) {
            Replay::Done(actual) => assert!(actual.same(expected), "{line}\nactual: {}", actual.to_json()),
            Replay::Skipped => panic!("plain reference fixture was skipped: {line}"),
            Replay::Panicked(message) => panic!("plain reference fixture panicked: {message}: {line}"),
        }
    }
}
