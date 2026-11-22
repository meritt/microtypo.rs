use microtypo_core::Re;

type Ranges = Vec<Vec<Option<(usize, usize)>>>;

fn ranges(source: &str, flags: &str, text: &[u16]) -> Ranges {
    let re = Re::try_new(source, flags).unwrap();
    re.find_all(text)
        .into_iter()
        .map(|c| (0..=c.group_count()).map(|i| c.range(i).map(|r| (r.start, r.end))).collect())
        .collect()
}

#[test]
fn lazy_repetition_keeps_global_ranges_and_captures() {
    let text: Vec<_> = "xa12ba3b".encode_utf16().collect();
    assert_eq!(ranges("a(.*?)b", "g", &text), vec![vec![Some((1, 5)), Some((2, 4))], vec![Some((5, 8)), Some((6, 7))]]);
    let text: Vec<_> = "a12ba3ba1234b".encode_utf16().collect();
    assert_eq!(
        ranges("a(.{1,3}?)b", "g", &text),
        vec![vec![Some((0, 4)), Some((1, 3))], vec![Some((4, 7)), Some((5, 6))]]
    );
    assert_eq!(
        ranges("(a*?)(a)", "g", &[0x61, 0x61]),
        vec![vec![Some((0, 1)), Some((0, 0)), Some((0, 1))], vec![Some((1, 2)), Some((1, 1)), Some((1, 2))]]
    );
    assert_eq!(ranges("a(.{0}?)b", "g", &[0x61, 0x62]), vec![vec![Some((0, 2)), Some((1, 1))]]);
}

#[test]
fn lazy_repetition_restores_backward_captures_and_alternatives() {
    assert_eq!(ranges("(?<=x(a+?))b", "g", &[0x78, 0x61, 0x61, 0x62]), vec![vec![Some((3, 4)), Some((1, 3))]]);
    let text: Vec<_> = "a1bca2b".encode_utf16().collect();
    assert_eq!(
        ranges("a(.*?)(bc|b)", "g", &text),
        vec![vec![Some((0, 4)), Some((1, 2)), Some((2, 4))], vec![Some((4, 7)), Some((5, 6)), Some((6, 7))]]
    );
}

#[test]
fn lazy_bounds_count_utf16_atoms_and_preserve_lone_surrogates() {
    let text = [0x61, 0xD83D, 0xDE00, 0x62, 0x61, 0x416, 0x62];
    assert_eq!(
        ranges("a(.{0,1}?)b", "gu", &text),
        vec![vec![Some((0, 4)), Some((1, 3))], vec![Some((4, 7)), Some((5, 6))]]
    );
    assert_eq!(ranges("a(.{0,1}?)b", "g", &text), vec![vec![Some((4, 7)), Some((5, 6))]]);
    let text = [0x61, 0xD800, 0x62, 0x61, 0xDC00, 0x62];
    assert_eq!(
        ranges("a(.{0,1}?)b", "gu", &text),
        vec![vec![Some((0, 3)), Some((1, 2))], vec![Some((3, 6)), Some((4, 5))]]
    );
}

#[test]
fn lazy_wrappers_stop_at_the_first_close_and_at_line_terminators() {
    let text: Vec<_> = "<\u{e000}T0\u{e001}>a</\u{e000}T0\u{e001}><\u{e000}T0\u{e001}>\n</\u{e000}T0\u{e001}><\u{e000}T0\u{e001}>b</\u{e000}T0\u{e001}>".encode_utf16().collect();
    assert_eq!(
        ranges("<\u{e000}T0\u{e001}>(.*?)</\u{e000}T0\u{e001}>", "giu", &text),
        vec![vec![Some((0, 14)), Some((6, 7))], vec![Some((28, 42)), Some((34, 35))]]
    );
}

#[test]
fn lazy_runtime_minimum_and_maximum_count_atoms() {
    for atom in [vec![0x71], vec![0xD83D, 0xDE00], vec![0xD800], vec![0xDC00]] {
        for count in 5..=9 {
            let mut text = vec![0x61];
            for _ in 0..count {
                text.extend_from_slice(&atom);
            }
            let end = text.len();
            text.push(0x62);
            for (quantifier, matches) in [("{6,8}?", (6..=8).contains(&count)), ("{6}?", count == 6)] {
                let expected = if matches { vec![vec![Some((0, end + 1)), Some((1, end))]] } else { vec![] };
                assert_eq!(ranges(&format!("a(.{quantifier})b"), "gu", &text), expected);
                text[0] = 0x78;
                let expected = if matches { vec![vec![Some((end, end + 1)), Some((1, end))]] } else { vec![] };
                assert_eq!(ranges(&format!("(?<=x(.{quantifier}))b"), "gu", &text), expected);
                text[0] = 0x61;
            }
        }
    }
}
