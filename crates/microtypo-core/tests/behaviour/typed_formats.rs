use microtypo_conformance::HOST;
use microtypo_core::{Config, Input, MicroTypo};

use crate::support::settles;

#[test]
fn typed_math_keeps_formulas_and_physical_inline_boundaries() {
    let mut engine = MicroTypo::with_config(
        Config {
            input: Some(Input { format: Some("markdown".into()), math: Some(true), ..Input::default() }),
            ..Config::default()
        },
        HOST,
    )
    .unwrap();
    for (source, expected) in [
        ("$a - b<c$> Корвин - принц", "$a - b<c$> Корвин\u{a0}— принц"),
        ("$a - b$ Корвин - принц", "$a - b$ Корвин\u{a0}— принц"),
        ("$$a - b\n c - d$$ Корвин - принц", "$$a - b\n c - d$$ Корвин\u{a0}— принц"),
        ("$a - b\\\r\nc - d$", "$a\u{a0}— b\\\r\nc\u{a0}— d$"),
    ] {
        settles(&mut engine, source, expected);
    }
}

#[test]
fn typed_mdx_keeps_value_and_statement_slashes_and_adjacent_prose() {
    let mut engine = MicroTypo::with_config(
        Config { input: Some(Input { format: Some("mdx".into()), ..Input::default() }), ..Config::default() },
        HOST,
    )
    .unwrap();
    for code in [
        "{(() => { if (x) {} else /}/.test(x); })()}",
        "{(() => { label: {} /}/.test(x); })()}",
        "{function() {} / 2}",
        "{class extends Bases[{}] {} / 2}",
        "{<Box value={<Inner>{\"}\"}</Inner>} />}",
    ] {
        let source = format!("{code} Корвин - принц");
        let expected = format!("{code} Корвин\u{a0}— принц");
        settles(&mut engine, &source, &expected);
    }
    let module = "import X from \"./x.js\"\n\nКорвин - принц\n";
    assert_eq!(engine.process(module).unwrap(), "import X from \"./x.js\"\n\nКорвин\u{a0}— принц\n");
}

#[test]
fn typed_attributes_emit_glyphs_keep_entities_and_ignore_duplicate_or_nontext_fields() {
    let mut engine = MicroTypo::with_config(
        Config {
            html: Some(false),
            entities: Some(true),
            input: Some(Input {
                format: Some("html".into()),
                attributes: Some(vec!["alt".into(), "title".into(), "content".into()]),
                ..Input::default()
            }),
            ..Config::default()
        },
        HOST,
    )
    .unwrap();
    for (source, expected) in [
        (
            "<img alt=\" Корвин - принц &quot; \" alt=\"Код - текст\">",
            "<img alt=\" Корвин\u{a0}— принц &quot; \" alt=\"Код - текст\">",
        ),
        (
            "<meta name=\"description\" content=\"Корвин - принц\">",
            "<meta name=\"description\" content=\"Корвин\u{a0}— принц\">",
        ),
        ("<meta name=\"viewport\" content=\"Корвин - принц\">", "<meta name=\"viewport\" content=\"Корвин - принц\">"),
        ("<code title=\"Корвин - принц\">Код - текст</code>", "<code title=\"Корвин - принц\">Код - текст</code>"),
    ] {
        settles(&mut engine, source, expected);
    }
}
