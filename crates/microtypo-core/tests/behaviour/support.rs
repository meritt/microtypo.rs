use microtypo_core::MicroTypo;

pub fn settles(engine: &mut MicroTypo, input: &str, expected: &str) {
    assert_eq!(engine.process(input).unwrap(), expected, "{input:?}");
    assert_eq!(engine.process(expected).unwrap(), expected, "{expected:?}");
}
