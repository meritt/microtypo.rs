use microtypo_conformance::HOST;
use microtypo_core::MicroTypo;
use serde_json::json;

const DEPTH: usize = 1_000_000;
const LIMIT: usize = 5_000_000;
const VALUE: &str = "\"а - б\"";
const DOTTED: &str = "b = \"а - б\"";

#[test]
fn structured_input_nested_a_million_levels_deep_fits_a_two_mebibyte_stack() {
    for (format, before, open, middle, close, after) in [
        ("json", "", "[", VALUE, "]", ""),
        ("json", "", "{\"a\":", VALUE, "}", ""),
        ("toml", "", "a.", DOTTED, "", "\n"),
        ("toml", "a = ", "[", VALUE, "]", "\n"),
        ("yaml", "a: ", "[", VALUE, "]", "\n"),
        ("frontmatter", "+++\n", "a.", DOTTED, "", "\n+++\n\nа - б\n"),
        ("frontmatter", "---\na: ", "[", VALUE, "]", "\n---\n\nа - б\n"),
    ] {
        let fixed = before.len() + middle.len() + after.len();
        let depth = DEPTH.min((LIMIT - fixed) / (open.len() + close.len()));
        let input = format!("{before}{}{middle}{}{after}", open.repeat(depth), close.repeat(depth));
        let expected = input.replace("а - б", "а\u{a0}— б");
        let worker = std::thread::Builder::new().stack_size(2 << 20).spawn(move || {
            let config = json!({ "input": format, "html": false, "maxProcessingMs": 0, "maxInputLength": LIMIT });
            let mut engine = MicroTypo::new(Some(&config), HOST).unwrap();
            engine.process(&input).map(std::borrow::Cow::into_owned)
        });
        let output = worker.unwrap().join().expect("the worker thread finishes");
        assert_eq!(output.unwrap(), expected, "{format} {open}");
    }
}
