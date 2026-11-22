use microtypo_conformance::HOST;
use microtypo_core::MicroTypo;
use serde_json::json;

#[test]
fn a_call_does_not_depend_on_the_previous_input() {
    let config = json!({"html":true});
    for (input, expected) in
        [("\"а<br>б\"", "<p>«а<br>б»</p>"), ("Он: \"а<br>»б и \"в»", "<p>Он: «а<br>»б и\u{a0}«в»</p>")]
    {
        let mut reused = MicroTypo::new(Some(&config), HOST).unwrap();
        reused.process("<b>«а»</b>").unwrap();
        let mut fresh = MicroTypo::new(Some(&config), HOST).unwrap();
        assert_eq!(fresh.process(input).unwrap(), expected, "{input}");
        assert_eq!(reused.process(input).unwrap(), expected, "{input}");
    }
}
