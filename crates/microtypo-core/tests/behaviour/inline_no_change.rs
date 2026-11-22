use microtypo_conformance::HOST;
use microtypo_core::MicroTypo;
use serde_json::json;

#[test]
fn unchanged_rules_preserve_author_markup_and_empty_changes() {
    let mut engine = MicroTypo::new(Some(&json!({"input":"html", "html":false})), HOST).unwrap();
    for _ in 0..2 {
        for source in ["дом", "<b>дом</b>", "<i></i>дом", "<i>http://example.com</i>"] {
            let input: Vec<_> = source.encode_utf16().collect();
            let result = engine.process_with_changes(&input).unwrap();
            assert_eq!(result.text, input);
            assert_eq!(result.changes, [] as [(usize, usize); 0]);
        }
    }
}

#[test]
fn an_unmatched_html_only_rule_keeps_text_for_later_binding() {
    let mut engine = MicroTypo::new(
        Some(&json!({
            "input":"html", "html":true,
            "rules":{
                "quote":false, "dash":false, "symbol":false, "punctuation":false,
                "number":false, "space":false, "abbr":false, "nobr":false,
                "date":false, "hanging":false, "other":false, "text":false
            }
        })),
        HOST,
    )
    .unwrap();
    engine.apply_options(Some(&json!({"rules":{"number.sub":true,"nobr.nbsp_short_word":true}}))).unwrap();
    for _ in 0..2 {
        assert_eq!(engine.process("<i>в</i> доме").unwrap(), "<i>в</i> доме");
        assert_eq!(engine.process("<b>дом</b>").unwrap(), "<b>дом</b>");
    }
}
