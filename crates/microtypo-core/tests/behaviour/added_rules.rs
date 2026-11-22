use microtypo_conformance::HOST;
use microtypo_core::MicroTypo;
use serde_json::json;

#[test]
fn added_rules_compose_with_existing_rules_and_protection() {
    for (config, input, expected) in [
        (json!({}), "ч. 37479°C", "ч.\u{a0}37\u{202f}479\u{a0}°C"),
        (
            json!({"html":true,"render":{"paragraphs":false}}),
            "НИИ +7 (999) 123 45 67",
            "НИИ <span style=\"white-space:nowrap;\">+7 (999) 123 45 67</span>",
        ),
        (
            json!({"html":true,"input":{"template":"liquid"}}),
            "Это мой дом. {% comment %}rest",
            "<p>Это мой\u{a0}дом.</p> {% comment %}rest",
        ),
        (json!({}), "1/8долл.", "⅛\u{a0}$."),
        (
            json!({"html":true,"render":{"paragraphs":false}}),
            "и пр.\u{a0}как то:+7-999-123-45-67; текст <code>:+7-999-123-45-67</code>",
            "<span style=\"white-space:nowrap;\">и пр. как то:</span><span style=\"white-space:nowrap;\">+7-999-123-45-67</span>; текст <code>:+7-999-123-45-67</code>",
        ),
    ] {
        let mut engine = MicroTypo::new(Some(&config), HOST).unwrap();
        assert_eq!(engine.process(input).unwrap(), expected, "{input}");
    }
}
