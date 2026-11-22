use microtypo_core::Re;

fn exec_at(source: &str, flags: &str, at: usize) -> Option<(usize, usize)> {
    let text: Vec<u16> = "a\u{1F600}b".encode_utf16().collect();
    Re::try_new(source, flags).unwrap().exec_at(&text, at).map(|c| (c.index(), c.end()))
}

#[test]
fn last_index_inside_a_surrogate_pair_starts_at_the_pair_under_unicode() {
    for (source, flags, at, expected) in [
        (".", "uy", 2, Some((1, 3))),
        (".", "ug", 2, Some((1, 3))),
        (".", "vy", 2, Some((1, 3))),
        ("(?:)", "uy", 2, Some((1, 1))),
        ("b", "uy", 2, None),
        ("b", "ug", 2, Some((3, 4))),
        (".", "y", 2, Some((2, 3))),
        (".", "g", 2, Some((2, 3))),
        (".", "uy", 1, Some((1, 3))),
        (".", "uy", 3, Some((3, 4))),
    ] {
        assert_eq!(exec_at(source, flags, at), expected, "/{source}/{flags} at {at}");
    }
}
