use microtypo_conformance::HOST;
use microtypo_core::{MicroTypo, Position, Rule, RuleGroupDef};
use serde_json::json;

use crate::support::settles;

fn breaks(text: &str) -> Vec<&str> {
    let bytes = text.as_bytes();
    let mut result = Vec::new();
    let mut at = 0;
    while at < bytes.len() {
        let start = at;
        match bytes[at] {
            b'\r' => at += if bytes.get(at + 1) == Some(&b'\n') { 2 } else { 1 },
            b'\n' => at += 1,
            _ => {
                at += 1;
                continue;
            }
        }
        result.push(&text[start..at]);
    }
    result
}

#[test]
fn file_formats_keep_physical_line_endings_and_the_final_tail() {
    let files = [
        ("markdown", "Корвин - принц\n\n```\nКод - текст\n```\nЭрик - король"),
        ("frontmatter", "---\ntitle: \"Амбер - город\"\n---\n\nКорвин - принц\nЭрик - король"),
        ("yaml", "title: \"Амбер - город\"\nbody: |\n  Корвин - принц\n  Эрик - король"),
        ("toml", "title = \"Амбер - город\"\nbody = \"\"\"\nКорвин - принц\nЭрик - король\n\"\"\""),
        ("json", "{\n  \"title\": \"Амбер - город\",\n  \"body\": \"Корвин - принц\"\n}"),
        ("xml", "<root>\n<p>Корвин - принц</p>\n<pre>Код - текст</pre>\n</root>"),
    ];
    for (format, source) in files {
        for ending in ["\n", "\r\n", "\r"] {
            if format == "toml" && ending == "\r" {
                continue;
            }
            for tail in [String::new(), ending.to_owned(), ending.repeat(3)] {
                let input = source.replace('\n', ending) + &tail;
                let mut engine = MicroTypo::new(Some(&json!({"html":false,"input":format})), HOST).unwrap();
                let output = engine.process(&input).unwrap();
                assert_eq!(breaks(&output), breaks(&input), "{format} {ending:?} {tail:?}");
                assert!(output.contains("Корвин\u{a0}— принц"));
                assert_eq!(engine.process(&output).unwrap(), output);
            }
        }
    }
}

#[test]
fn mixed_endings_follow_surviving_spans_around_changes_and_protected_content() {
    let mut engine = MicroTypo::new(Some(&json!({"input":"markdown"})), HOST).unwrap();
    for (source, expected) in [
        (
            "Корвин - принц\r\n```\nКод - текст\r\n```\rЭрик - король\n",
            "Корвин\u{a0}— принц\r\n```\nКод - текст\r\n```\rЭрик\u{a0}— король\n",
        ),
        ("5\r\nмая\nКорвин - принц\r\n", "5\u{a0}мая\nКорвин\u{a0}— принц\r\n"),
        ("Корвин\r\n\n\r\n\rЭрик\r\n", "Корвин\r\n\nЭрик\r\n"),
        (
            "<span title=\"a\r\nb\rc\nd\">Корвин - принц</span>\r\n",
            "<span title=\"a\r\nb\rc\nd\">Корвин\u{a0}— принц</span>\r\n",
        ),
        ("<pre>Код\r\nсмешанный\nтекст\r</pre>\n", "<pre>Код\r\nсмешанный\nтекст\r</pre>\n"),
    ] {
        assert_eq!(engine.process(source).unwrap(), expected);
    }
    let mut engine = MicroTypo::new(
        Some(&json!({"html":true,"input":"markdown","render":{"paragraphs":true,"breakline":true}})),
        HOST,
    )
    .unwrap();
    assert_eq!(engine.process("Корвин\r\nЭрик\r\n").unwrap(), "<p>Корвин<br>\r\nЭрик</p>\r\n");
}

#[test]
fn inserted_newlines_use_the_document_style_without_moving_existing_mixed_endings() {
    let mut engine = MicroTypo::new(Some(&json!({"input":"markdown"})), HOST).unwrap();
    let group = RuleGroupDef {
        name: Some("other".to_owned()),
        rules: vec![Rule::handler("break", |ctx| {
            let mut result = ctx.text.clone();
            if let Some(at) = result.iter().position(|&c| c == 0x7C) {
                result[at] = 0x0A;
            }
            Ok(Some(result))
        })],
        ..RuleGroupDef::default()
    };
    engine.register_rule_group(group, None, Position::End).unwrap();
    assert_eq!(engine.process("Корвин\r\nЭрик|Блейз\nРэндом\r\n").unwrap(), "Корвин\r\nЭрик\r\nБлейз\nРэндом\r\n");
}

#[test]
fn json_value_normalization_and_fragment_endings_keep_their_existing_behavior() {
    let mut engine = MicroTypo::new(Some(&json!({"input":"json"})), HOST).unwrap();
    assert_eq!(engine.process("{\"body\":\"Корвин\\r\\nЭрик\"}").unwrap(), "{\"body\":\"Корвин\\nЭрик\"}");
    for format in ["text", "html"] {
        let mut engine = MicroTypo::new(Some(&json!({"input":format})), HOST).unwrap();
        assert_eq!(engine.process("Корвин - принц\r\n").unwrap(), "Корвин\u{a0}— принц");
    }
}

#[test]
fn ordinary_percent_signs_cannot_consume_the_next_printf_argument() {
    for format in ["text", "json", "yaml", "toml"] {
        let wrap = |text: &str| match format {
            "json" => json!({"value":text}).to_string(),
            "yaml" => format!("value: {text}"),
            "toml" => format!("value = \"{text}\""),
            _ => text.to_owned(),
        };
        let mut engine =
            MicroTypo::new(Some(&json!({"input":{"format":format,"placeholders":"printf"}})), HOST).unwrap();
        let source = wrap("7%  %2$.*3$f - текст");
        let expected = wrap("7% %2$.*3$f\u{a0}— текст");
        settles(&mut engine, &source, &expected);
    }
    let mut engine = MicroTypo::new(Some(&json!({"input":{"placeholders":"printf"}})), HOST).unwrap();
    settles(&mut engine, "%%%2$.*3$f - текст", "%%%2$.*3$f\u{a0}— текст");
}
