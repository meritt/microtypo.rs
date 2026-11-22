use microtypo_conformance::HOST;
use microtypo_core::{Config, Input, MicroTypo, Render};
use serde_json::{Value, json};

fn outcome(engine: Result<MicroTypo, microtypo_core::Error>, text: &str) -> Result<String, (String, String, Value)> {
    engine
        .and_then(|mut e| e.process(text).map(std::borrow::Cow::into_owned))
        .map_err(|e| (e.name().to_owned(), e.message.clone(), e.details.clone()))
}

#[test]
fn a_typed_config_behaves_as_its_json_object() {
    let text = "\"Цена\" - 100 руб. за http://example.com";
    let configs = [
        (Config::default(), json!({})),
        (
            Config {
                html: Some(true),
                entities: Some(true),
                rules: vec![("quote".into(), false), ("nbsp".into(), true)],
                render: Some(Render { hanging: Some("class".into()), prefix: Some("t-".into()), ..Render::default() }),
                ..Config::default()
            },
            json!({ "html": true, "entities": true, "rules": { "quote": false, "nbsp": true },
                    "render": { "hanging": "class", "prefix": "t-" } }),
        ),
        (
            Config { input: Some(Input { format: Some("markdown".into()), ..Input::default() }), ..Config::default() },
            json!({ "input": { "format": "markdown" } }),
        ),
        (Config { max_input_length: Some(10), ..Config::default() }, json!({ "maxInputLength": 10 })),
        (Config { max_input_length: Some(5_000_001), ..Config::default() }, json!({ "maxInputLength": 5_000_001 })),
        (Config { max_processing_ms: Some(f64::NAN), ..Config::default() }, json!({ "maxProcessingMs": null })),
        (
            Config {
                input: Some(Input { format: Some("csv".into()), template: Some("php".into()), ..Input::default() }),
                rules: vec![("quotes".into(), true), ("dash.nope".into(), false)],
                render: Some(Render { nowrap: Some("div".into()), prefix: Some("a b".into()), ..Render::default() }),
                ..Config::default()
            },
            json!({ "input": { "format": "csv", "template": "php" }, "rules": { "quotes": true, "dash.nope": false },
                    "render": { "nowrap": "div", "prefix": "a b" } }),
        ),
    ];
    for (typed, object) in configs {
        assert_eq!(typed.to_json(), object);
        assert_eq!(
            outcome(MicroTypo::with_config(typed, HOST), text),
            outcome(MicroTypo::new(Some(&object), HOST), text)
        );
    }
}

#[test]
fn reading_a_config_validates_it_as_the_engine_does() {
    let read: Config = serde_json::from_str(
        r#"{ "html": "on", "input": "yaml", "rules": { "nbsp": 0 }, "render": { "prefix": false } }"#,
    )
    .unwrap();
    assert_eq!(
        read,
        Config {
            html: Some(true),
            input: Some(Input { format: Some("yaml".into()), ..Input::default() }),
            rules: vec![("nbsp".into(), false)],
            render: Some(Render { prefix: Some(String::new()), ..Render::default() }),
            ..Config::default()
        }
    );
    assert_eq!(serde_json::from_value::<Config>(serde_json::to_value(&read).unwrap()).unwrap(), read);

    let invalid = json!({ "html": "maybe", "colour": 1 });
    let engine = MicroTypo::new(Some(&invalid), HOST).unwrap_err();
    let error = serde_json::from_value::<Config>(invalid).unwrap_err();
    assert_eq!(error.to_string(), engine.to_string());
}
