#![cfg(feature = "utf16")]

use core::ops::Range;

use regress::Regex;

fn units(text: &str) -> Vec<u16> {
    text.encode_utf16().collect()
}

fn attempt(
    pattern: &str,
    flags: &str,
    text: &str,
    at: usize,
) -> Option<(Range<usize>, Vec<Option<Range<usize>>>)> {
    let re = Regex::with_flags(pattern, flags).unwrap();
    let text = units(text);
    let mut captures = vec![None; re.group_count()];
    let found = if flags.contains('u') {
        re.find_from_utf16(&text, 0)
            .attempt_at_into(at, &mut captures)
    } else {
        re.find_from_ucs2(&text, 0)
            .attempt_at_into(at, &mut captures)
    };
    found.map(|range| (range, captures))
}

#[test]
fn sticky_attempt_matches_only_at_its_position() {
    for flags in ["y", "gy", "uy", "guy"] {
        assert_eq!(
            attempt("(b)c", flags, "abcbc", 1),
            Some((1..3, vec![Some(1..2)])),
            "{flags}"
        );
        assert_eq!(
            attempt("(b)c", flags, "abcbc", 3),
            Some((3..5, vec![Some(3..4)])),
            "{flags}"
        );
        assert_eq!(attempt("(b)c", flags, "abcbc", 0), None, "{flags}");
        assert_eq!(attempt("(b)c", flags, "abcbc", 2), None, "{flags}");
        assert_eq!(attempt("(b)c", flags, "abcbc", 5), None, "{flags}");
    }
}

#[test]
fn sticky_attempt_keeps_an_empty_match_at_its_position() {
    for flags in ["y", "gy", "uy"] {
        assert_eq!(
            attempt("x*", flags, "ab", 1),
            Some((1..1, vec![])),
            "{flags}"
        );
        assert_eq!(
            attempt("x*", flags, "ab", 2),
            Some((2..2, vec![])),
            "{flags}"
        );
        assert_eq!(
            attempt("(?=b)", flags, "ab", 1),
            Some((1..1, vec![])),
            "{flags}"
        );
        assert_eq!(attempt("(?=b)", flags, "ab", 0), None, "{flags}");
    }
}

#[test]
fn failed_sticky_attempt_leaves_the_next_one_intact() {
    for flags in ["y", "gy", "uy"] {
        let re = Regex::with_flags("b+", flags).unwrap();
        let text = units("abba");
        let mut captures = vec![];
        if flags.contains('u') {
            let mut utf16 = re.find_from_utf16(&text, 0);
            assert_eq!(utf16.attempt_at_into(0, &mut captures), None, "{flags}");
            assert_eq!(
                utf16.attempt_at_into(1, &mut captures),
                Some(1..3),
                "{flags}"
            );
            assert_eq!(utf16.attempt_at_into(3, &mut captures), None, "{flags}");
            assert!(utf16.next().is_none(), "{flags}");
        } else {
            let mut ucs2 = re.find_from_ucs2(&text, 0);
            assert_eq!(ucs2.attempt_at_into(0, &mut captures), None, "{flags}");
            assert_eq!(
                ucs2.attempt_at_into(1, &mut captures),
                Some(1..3),
                "{flags}"
            );
            assert_eq!(ucs2.attempt_at_into(3, &mut captures), None, "{flags}");
            assert!(ucs2.next().is_none(), "{flags}");
        }
    }
}
