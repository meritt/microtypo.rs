#![cfg(feature = "utf16")]

use regress::Regex;

type Case = (
    &'static str,
    &'static str,
    &'static str,
    &'static [(usize, usize)],
);

fn matches(pattern: &str, flags: &str, text: &str) -> Vec<(usize, usize)> {
    let re = Regex::with_flags(pattern, flags).unwrap();
    let text: Vec<u16> = text.encode_utf16().collect();
    if flags.contains('u') {
        re.find_from_utf16(&text, 0)
            .map(|found| (found.start(), found.end()))
            .collect()
    } else {
        re.find_from_ucs2(&text, 0)
            .map(|found| (found.start(), found.end()))
            .collect()
    }
}

fn assert_matches(cases: &[Case]) {
    for &(pattern, flags, text, expected) in cases {
        assert_eq!(
            matches(pattern, flags, text),
            expected,
            "/{pattern}/{flags} on {text:?}"
        );
    }
}

#[test]
fn empty_match_after_a_negative_lookbehind_starts_at_the_end_of_input() {
    assert_matches(&[
        ("(?<!a)", "g", "ab", &[(0, 0), (2, 2)]),
        ("(?<!a)", "gu", "ab", &[(0, 0), (2, 2)]),
        ("(?<!a)", "g", "abc", &[(0, 0), (2, 2), (3, 3)]),
        ("(?<!a)", "gu", "aяb", &[(0, 0), (2, 2), (3, 3)]),
        ("(?<!a)", "g", "a😀b", &[(0, 0), (2, 2), (3, 3), (4, 4)]),
        ("(?<!a)$", "gu", "a😀b", &[(4, 4)]),
        ("(?<!\\.)$", "g", "ab", &[(2, 2)]),
        ("(?<!\\.)$", "gu", "ab", &[(2, 2)]),
        ("(?<![.!?])$", "g", "ab", &[(2, 2)]),
        ("(?<!a)\\b", "g", "ab", &[(0, 0), (2, 2)]),
        ("(?<![ab])|x", "g", "abc", &[(0, 0), (3, 3)]),
        ("(?<!a)x*", "g", "ab", &[(0, 0), (2, 2)]),
        ("(?<![a-c])", "g", "abcd", &[(0, 0), (4, 4)]),
        ("(?<!\\uD83D)", "g", "a😀", &[(0, 0), (1, 1), (3, 3)]),
    ]);
}

#[test]
fn multiline_line_start_matches_at_the_end_of_input_after_a_line_break() {
    assert_matches(&[
        ("^", "gm", "a\n", &[(0, 0), (2, 2)]),
        ("^", "gmu", "a\n", &[(0, 0), (2, 2)]),
        ("^\\s*", "gm", "a\n", &[(0, 0), (2, 2)]),
        ("^(?:> )?", "gm", "a\n", &[(0, 0), (2, 2)]),
        ("^", "gm", "", &[(0, 0)]),
    ]);
}

#[test]
fn the_unit_before_the_end_of_input_still_rules_the_start_out() {
    assert_matches(&[
        ("(?<!a)", "g", "a", &[(0, 0)]),
        ("(?<!a)", "g", "", &[(0, 0)]),
        ("(?<!a)$", "g", "ba", &[]),
        ("(?<!\\.)$", "g", "ab.", &[]),
        ("(?<!😀)", "gu", "a😀", &[(0, 0), (1, 1)]),
        ("(?<!b)(?=)", "g", "ab", &[(0, 0), (1, 1)]),
        ("^", "gm", "a\nb", &[(0, 0), (2, 2)]),
    ]);
}

#[test]
fn expressions_without_a_set_before_keep_their_matches() {
    assert_matches(&[
        ("(?<=b)$", "g", "ab", &[(2, 2)]),
        ("(?<!ab)$", "g", "abc", &[(3, 3)]),
        ("$", "g", "ab", &[(2, 2)]),
        ("\\B", "gu", "ab cd", &[(1, 1), (4, 4)]),
    ]);
}
