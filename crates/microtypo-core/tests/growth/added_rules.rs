use microtypo_conformance::HOST;
use microtypo_core::MicroTypo;
use serde_json::json;

use crate::timing::{alone, median};

#[test]
fn minus_math_tags_and_values_scale_linearly() {
    let _alone = alone();
    let mut costs = Vec::new();
    for n in [2000, 4000, 8000] {
        let input = format!("{}{}", "<sup>x</sup>".repeat(n), "Температура -1; ".repeat(n));
        let expected = format!("{}{}", "<sup>x</sup>".repeat(n), "Температура −1; ".repeat(n)).trim_end().to_owned();
        let config = json!({"html":true,"maxInputLength":5000000,"maxProcessingMs":0,"input":"html","render":{"paragraphs":false}});
        let mut engine = MicroTypo::new(Some(&config), HOST).unwrap();
        assert_eq!(engine.process(&input).unwrap(), expected);
        costs.push(median(|| assert_eq!(engine.process(&input).unwrap(), expected)));
    }
    eprintln!("minus math-tags N/2N/4N: {costs:?}");
    assert!(costs[2] / costs[0] < 7.0, "4x input cost {}x", costs[2] / costs[0]);
}
