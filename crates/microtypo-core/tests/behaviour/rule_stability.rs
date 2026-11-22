use microtypo_conformance::HOST;
use microtypo_core::MicroTypo;
use serde_json::{Value, json};

use crate::support;

fn settles(config: &Value, cases: &[(&str, &str)]) {
    let mut engine = MicroTypo::new(Some(config), HOST).unwrap();
    for &(input, expected) in cases {
        support::settles(&mut engine, input, expected);
    }
}

#[test]
fn consecutive_organizations_bind_each_following_word() {
    settles(
        &json!({}),
        &[
            ("НИИ ООО в", "НИИ\u{a0}ООО\u{a0}в"),
            ("ООО НИИ Рос", "ООО\u{a0}НИИ\u{a0}Рос"),
            ("ООО Ромашка", "ООО\u{a0}Ромашка"),
            ("СООО Ромашка", "СООО Ромашка"),
            ("ООО 5", "ООО 5"),
        ],
    );
}

#[test]
fn terminal_punctuation_drops_the_complete_garbage_tail() {
    settles(
        &json!({}),
        &[
            ("!,?,!!!", "!"),
            ("?,!;?", "?"),
            ("!,,!;!!!", "!"),
            ("что!,?", "что!"),
            ("что!?,", "что!?,"),
            ("!,", "!,"),
            ("!?", "!?"),
        ],
    );
}

#[test]
fn grouping_triads_before_a_terminal_line_break_keeps_other_line_boundaries() {
    settles(&json!({}), &[("цена 9 127\n", "цена 9\u{202f}127"), ("9 127\nдалее", "9\u{202f}127\nдалее")]);
}

#[test]
fn repeated_gaps_before_a_comma_are_removed_on_the_first_pass() {
    settles(
        &json!({}),
        &[
            ("привет  ,мир", "привет, мир"),
            ("привет ,мир", "привет, мир"),
            ("Слово :текст", "Слово: текст"),
            ("Слово  ) и", "Слово) и"),
        ],
    );
}

#[test]
fn nested_nobr_wrappers_settle_like_span_wrappers() {
    settles(&json!({"html":true,"render":{"nowrap":"nobr"}}), &[("а 28.08.20г.", "<p><nobr>а 28.08.20 г.</nobr></p>")]);
    settles(
        &json!({"html":true}),
        &[("а 28.08.20г.", "<p><span style=\"white-space:nowrap;\">а 28.08.20 г.</span></p>")],
    );
}

#[test]
fn generated_address_links_keep_their_address_text_on_the_next_pass() {
    settles(
        &json!({"html":true}),
        &[
            ("http://10000", "<p><a href=\"http://10000\">10000</a></p>"),
            ("ftps://10000", "<p><a href=\"ftps://10000\">10000</a></p>"),
            ("http://10000&#65;/x", "<p><a href=\"http://10000A/x\">10000&#65;/x</a></p>"),
            ("http://10000&#x41;/x", "<p><a href=\"http://10000A/x\">10000&#x41;/x</a></p>"),
            ("http://...b/c", "<p><a href=\"http://...b/c\">...b/c</a></p>"),
            ("mailto://12345 и 20000", "<p><a href=\"mailto://12345\">12345</a> и\u{a0}20\u{202f}000</p>"),
            ("http://10&20", "<p><a href=\"http://10&amp;20\">10&20</a></p>"),
        ],
    );
}

#[test]
fn links_with_different_visible_text_still_typeset_that_text() {
    settles(
        &json!({"html":true}),
        &[
            ("<a href=\"http://10000\">10 000</a>", "<p><a href=\"http://10000\">10\u{202f}000</a></p>"),
            ("<a href=\"ftps://10000\">10 000</a>", "<p><a href=\"ftps://10000\">10\u{202f}000</a></p>"),
            (
                "<a href=\"http://10000A/x\">10000&#66;/x</a>",
                "<p><a href=\"http://10000A/x\">10\u{202f}000&#66;/x</a></p>",
            ),
            ("<a href=\"http://b.ru\">10000...</a>", "<p><a href=\"http://b.ru\">10\u{202f}000…</a></p>"),
        ],
    );
}
