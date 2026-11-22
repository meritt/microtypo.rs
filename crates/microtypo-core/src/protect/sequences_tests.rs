use super::{Census, SafeSequences, WHOLE_RE};
use crate::js::{self, JsString};

fn census(text: &[u16], stamp: &[u8]) -> Census {
    let mut sequences = SafeSequences::new(|bytes| bytes.fill(0));
    sequences.nonce = String::from_utf8(stamp.to_vec()).unwrap();
    sequences.census(text)
}

fn assert_census(text: &[u16], stamp: &str) {
    let stamp_units = js::u(stamp);
    let mut expected = [0; 4];
    for c in WHOLE_RE.find_all(text) {
        if c.has(2) && c.s(2) == stamp_units.as_slice() {
            expected[0] += 1;
            expected[1] += usize::from(!c.has(1));
        } else if c.has(3) && c.s(3) == stamp_units.as_slice() {
            expected[2] += 1;
        } else if c.has(4) && c.s(4) == stamp_units.as_slice() {
            expected[3] += 1;
        }
    }
    assert_eq!(census(text, stamp.as_bytes()), expected, "stamp={stamp}, text={text:?}");
}

fn forms(stamp: &str) -> Vec<JsString> {
    let core = format!("simonenko-xyz/A0SAFE{stamp}NUM0ID");
    let email = format!("A1SAFE{stamp}NUM0ID@simonenko.xyz");
    let token = format!("AzSAFE{stamp}TOK0TK");
    let mut forms = vec![
        core.clone(),
        format!("http://{core}"),
        format!("<\u{e000}A0\u{e001}>{core}"),
        format!("<\u{e000}A0000123\u{e001}>{core}"),
        format!("</\u{e000}A0\u{e001}>{core}"),
        format!("<\u{e000}A\u{e001}>{core}"),
        format!("<\u{e000}A-1\u{e001}>{core}"),
        format!("<\u{e000}A1\u{e001}> {core}"),
        format!("HTTP://{core}"),
        format!("http:/{core}"),
        format!("http://http://{core}"),
        format!("<\u{e000}A١\u{e001}>{core}"),
        format!("<\u{e000}A１\u{e001}>{core}"),
        format!("http://<\u{e000}A0\u{e001}>{core}"),
        format!("<{core}>"),
        email.clone(),
        token.clone(),
        format!("{core}{email}{token}"),
        format!("{token}{core}{email}"),
        format!("A1SAFEF{stamp}NUM0ID@simonenko.xyz{core}"),
        format!("AzSAFEF{stamp}TOK0TK{core}"),
        format!("http://simonenko-xyz/A0SAFEF{stamp}NUM0ID{core}"),
        format!("{core}NUM0ID{email}TOK0TK{token}"),
    ];
    for base in [&core, &email, &token] {
        for at in 0..base.len() {
            forms.push(base[..at].to_owned());
        }
        for old in ["NUM", "TOK", "ID", "TK", "SAFE"] {
            for new in ["", " ", "0", "G", "A1SAFE", "AzSAFE", "SAFE", "\u{e000}"] {
                forms.push(base.replace(old, new));
            }
        }
    }
    forms.into_iter().map(|s| js::u(&s)).collect()
}

#[test]
fn census_keeps_link_leads_distinct() {
    for (text, expected) in [
        ("simonenko-xyz/A0SAFE0NUM0ID", [1, 1, 0, 0]),
        ("http://simonenko-xyz/A0SAFE0NUM0ID", [1, 0, 0, 0]),
        ("<\u{e000}A0\u{e001}>simonenko-xyz/A0SAFE0NUM0ID", [1, 0, 0, 0]),
        ("<\u{e000}A\u{e001}>simonenko-xyz/A0SAFE0NUM0ID", [1, 1, 0, 0]),
        ("A1SAFE0NUM0ID@simonenko.xyzAzSAFE0TOK0TK", [0, 0, 1, 1]),
        ("A1SAFE00NUM0ID@simonenko.xyzAzSAFE00TOK0TK", [0, 0, 0, 0]),
    ] {
        let expected: Census = expected;
        assert_eq!(census(&js::u(text), b"0"), expected);
    }
}

#[test]
fn census_matches_whole_tokens_at_boundaries() {
    for stamp in ["", "0", "0000000000000000", "0123456789ABCDEF", "FFFFFFFFFFFFFFFF"] {
        for form in forms(stamp) {
            for offset in 0..=129 {
                let mut text = vec![0xD800; offset];
                text.extend_from_slice(&form);
                text.extend_from_slice(&[0xDC00, 0x53, 0x41, 0x46, 0x45]);
                assert_census(&text, stamp);
            }
        }
        for n in [0, 1, 2, 63, 64, 65, 127, 128, 1024, 30_000] {
            let digits = "0".repeat(n);
            for text in [
                format!("simonenko-xyz/A0SAFE{stamp}NUM{digits}ID"),
                format!("http://simonenko-xyz/A0SAFE{stamp}NUM{digits}I"),
                format!("<\u{e000}A{digits}\u{e001}>simonenko-xyz/A0SAFE{stamp}NUM0ID"),
                format!("A1SAFE{stamp}NUM{digits}ID@simonenko.xyz"),
                format!("AzSAFE{stamp}TOK{digits}TK"),
                format!("simonenko-xyz/A0SAFE{}NUM0ID", "F".repeat(n)),
                format!("A1SAFE{}NUM0ID@simonenko.xyz", "F".repeat(n)),
                format!("AzSAFE{}TOK0TK", "F".repeat(n)),
            ] {
                assert_census(&js::u(&text), stamp);
            }
        }
    }
}

#[test]
fn census_matches_whole_tokens_after_mutations() {
    let mut seed = 0x5EED_15A0_u64;
    let mut next = || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed as usize
    };
    for stamp in ["0", "0000000000000000", "0123456789ABCDEF", "FFFFFFFFFFFFFFFF"] {
        let forms = forms(stamp);
        for _ in 0..4096 {
            let mut text = forms[next() % forms.len()].clone();
            text.extend_from_slice(&forms[next() % forms.len()]);
            for _ in 0..=next() % 8 {
                let at = next() % (text.len() + 1);
                let end = (at + next() % 8).min(text.len());
                let insert = &forms[next() % forms.len()];
                let from = next() % (insert.len() + 1);
                let to = (from + next() % 8).min(insert.len());
                text.splice(at..end, insert[from..to].iter().copied());
            }
            assert_census(&text, stamp);
        }
    }
}

#[test]
fn census_stops_long_scans_on_the_host_budget() {
    for text in [
        vec![0x78; 1 << 18],
        vec![0x53; 1 << 18],
        js::u(&format!("simonenko-xyz/A0SAFE0NUM{}I", "0".repeat(1 << 18))),
        js::u(&format!("<\u{e000}A{}\u{e001}>simonenko-xyz/A0SAFE0NUM0ID", "0".repeat(1 << 18))),
    ] {
        js::regex::with_budget(
            || 10.0,
            0.0,
            1.0,
            || {
                census(&text, b"0");
                assert!(js::regex::out_of_budget());
            },
        );
        assert!(!js::regex::out_of_budget());
    }
}

#[test]
fn census_matches_whole_tokens_across_scan_blocks() {
    for stamp in ["0", "0123456789ABCDEF"] {
        let forms = [
            format!("http://simonenko-xyz/A0SAFE{stamp}NUM0ID"),
            format!("A1SAFE{stamp}NUM0ID@simonenko.xyz"),
            format!("AzSAFE{stamp}TOK0TK"),
        ];
        for offset in [0, 1, 15, 16, 17, 31, 32, 33, 65_535, 65_536, 65_537, 65_551, 65_552, 65_553] {
            for form in &forms {
                for tail in [0, 1, 15, 16, 17] {
                    let mut text = vec![0xD800; offset];
                    text.extend(form.encode_utf16());
                    text.extend(std::iter::repeat_n(0xDC00, tail));
                    assert_census(&text, stamp);
                }
            }
        }
        for count in [0, 1, 15, 16, 17, 65_535, 65_536, 65_537] {
            let digits = "0".repeat(count);
            for text in [
                format!("simonenko-xyz/A0SAFE{stamp}NUM{digits}ID"),
                format!("A1SAFE{stamp}NUM{digits}ID@simonenko.xyz"),
                format!("AzSAFE{stamp}TOK{digits}TK"),
            ] {
                assert_census(&js::u(&text), stamp);
            }
        }
    }
}
