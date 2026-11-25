use microtypo_conformance::HOST;
use microtypo_core::{MicroTypo, Position, Re, Replacement, Rule, RuleGroupDef};
use serde_json::json;

use crate::timing::{self, alone, linear};

fn engine(rule: Option<(&str, &str)>) -> MicroTypo {
    let mut engine = MicroTypo::new(Some(&json!({ "maxInputLength": 5_000_000, "maxProcessingMs": 0 })), HOST).unwrap();
    if let Some((pattern, replacement)) = rule {
        let group = RuleGroupDef {
            name: Some("user".to_owned()),
            rules: vec![Rule::pattern("user", Re::try_new(pattern, "g").unwrap(), Replacement::tpl(replacement))],
            ..RuleGroupDef::default()
        };
        engine.register_rule_group(group, None, Position::End).unwrap();
    }
    engine
}

fn best(engine: &mut MicroTypo, text: &str) -> f64 {
    timing::best(|| {
        engine.process(text).unwrap();
    })
}

fn scales_linearly(rule: Option<(&str, &str)>, unit: &str, count: usize) {
    let _alone = alone();
    let count = if cfg!(debug_assertions) { count / 4 } else { count };
    let mut engine = engine(rule);
    engine.process(&unit.repeat(count / 8)).unwrap();
    let once = best(&mut engine, &unit.repeat(count));
    let twice = best(&mut engine, &unit.repeat(2 * count));
    let ratio = twice / once;
    assert!(ratio < 3.0, "{unit:?}×{count}: {:.1} ms, ×2: {:.1} ms, ratio {ratio:.1}", once * 1000.0, twice * 1000.0);
}

#[test]
fn a_user_rule_with_many_matches_in_one_run_scales_linearly() {
    scales_linearly(Some(("ab", "x")), "ab", 20_000);
}

#[test]
fn a_user_rule_matching_as_itself_in_one_run_scales_linearly() {
    scales_linearly(Some(("ab", "ab")), "ab", 20_000);
}

#[test]
fn a_run_of_straight_quotes_scales_linearly() {
    scales_linearly(None, "\"", 80_000);
}

#[test]
fn repeated_abbreviations_scale_linearly() {
    scales_linearly(None, "т.е. ", 10_000);
}

#[test]
fn repeated_sums_of_money_scale_linearly() {
    scales_linearly(None, "1 000 руб. ", 20_000);
}

#[test]
fn sums_of_money_without_spaces_scale_linearly() {
    scales_linearly(None, "1руб.", 20_000);
}

type Shape = (&'static str, fn(usize) -> String);

fn scales_document(config: serde_json::Value, name: &str, build: fn(usize) -> String, count: usize) {
    let _alone = alone();
    let count = if cfg!(debug_assertions) { count / 4 } else { count };
    let mut engine = MicroTypo::new(Some(&config), HOST).unwrap();
    engine.process(&build(count / 8)).unwrap();
    let one = best(&mut engine, &build(count));
    let four = best(&mut engine, &build(4 * count));
    assert!(four < 8.0 * one, "{name}: 4x input cost {:.2}x", four / one);
}

#[test]
fn inline_markup_scales_linearly() {
    let html = json!({"input": "html", "html": false, "maxInputLength": 5_000_000, "maxProcessingMs": 0});
    let wrappers = json!({"input": "html", "html": true, "maxInputLength": 5_000_000, "maxProcessingMs": 0});
    let markdown = json!({"input": "markdown", "html": false, "maxInputLength": 5_000_000, "maxProcessingMs": 0});
    let html_shapes: &[Shape] = &[
        ("tagged words", |n| format!("<p>{}доме</p>", "<em>слово</em> в ".repeat(n))),
        ("nested tags", |n| format!("<p>{}доме{}</p>", "<b>в ".repeat(n), "</b>".repeat(n))),
        ("unclosed tags", |n| format!("<p>{}доме</p>", "<b>в ".repeat(n))),
        ("unopened tags", |n| format!("<p>{}доме</p>", "в</b> ".repeat(n))),
        ("empty tags", |n| format!("<p>в {} доме</p>", "<i></i><b></b>".repeat(n))),
        ("empty tags between words", |n| format!("<p>{}</p>", "в <i></i><b></b> доме ".repeat(n))),
        ("stray closers", |n| format!("<p>{}{}</p>", "<b>в ".repeat(n), "</i> д ".repeat(n))),
        ("stray closers across paragraphs", |n| format!("{}{}", "<p><b>в</p>".repeat(n), "<p></i> д</p>".repeat(n))),
        ("rewrites by tags", |n| format!("<p>{}</p>", "и <i>т.д.</i> 5 <b>руб.</b> ".repeat(n))),
    ];
    for &(name, build) in html_shapes {
        scales_document(html.clone(), name, build, 2_000);
    }
    scales_document(
        json!({"maxInputLength": 5_000_000, "maxProcessingMs": 0}),
        "stray closers in text",
        |n| format!("{}{}", "<b>в ".repeat(n), "</i> д ".repeat(n)),
        2_000,
    );
    let wrapper_shapes: &[Shape] = &[
        ("tagged words with wrappers", |n| format!("<p>{}доме</p>", "<em>слово</em> в ".repeat(n))),
        ("nested wrappers", |n| format!("<p>{}{}</p>", "<span>и т.д. ".repeat(n), "</span>".repeat(n))),
        ("rewrites with wrappers", |n| format!("<p>{}</p>", "и <i>т.д.</i> \"<b>сл</b>\" ".repeat(n))),
        ("crossing wrappers", |n| format!("<p>{}</p>", "<b>Он и</b> т.д. ".repeat(n))),
    ];
    for &(name, build) in wrapper_shapes {
        scales_document(wrappers.clone(), name, build, 2_000);
    }
    let markdown_shapes: &[Shape] = &[
        ("markers", |n| "*_".repeat(n)),
        ("marked words and links", |n| "*в* _доме_ [и](/x) **саду** ".repeat(n)),
        ("openers and closers", |n| format!("{}{}", "_а в ".repeat(n), "а* в ".repeat(n))),
        ("unpaired link closers", |n| "в x](/x) ".repeat(n)),
        ("unclosed brackets", |n| "[в *доме* ".repeat(n)),
        ("emphasized paragraphs", |n| "Он в *доме* и саду.\n\n".repeat(n)),
        ("emphasized lines", |n| "Он в *доме*\nи саду\n".repeat(n)),
        ("links on one line", |n| "в [доме](/x) и ".repeat(n)),
        ("links on separate lines", |n| "в [доме](/x) и\n".repeat(n)),
        ("code and emphasis", |n| "`к` *в* ".repeat(n)),
        ("nested links", |n| format!("{}в{}", "[".repeat(n), "](/x)".repeat(n))),
    ];
    for &(name, build) in markdown_shapes {
        scales_document(markdown.clone(), name, build, 2_000);
    }
}

#[test]
fn markdown_links_and_paragraph_layout_scale_linearly() {
    let config = json!({"input": "markdown", "html": true, "render": {"paragraphs": true}, "maxInputLength": 5_000_000, "maxProcessingMs": 0});
    let shapes: &[Shape] = &[
        ("inline links", |n| "[a](b) ".repeat(n)),
        ("full references", |n| "[a][b] ".repeat(n)),
        ("bare link boundaries", |n| "][".repeat(n)),
        ("references in prose", |n| "слово [текст][ref] и ещё слово, ".repeat(n)),
        ("chained labels", |n| "][a".repeat(n)),
        ("escaped link boundaries", |n| "\\][".repeat(n)),
        ("unclosed labels", |n| "[a][".repeat(n)),
        ("adjacent links and references", |n| "[a](b)[c][d]".repeat(n)),
        ("one open label", |n| format!("[x][{}", "a".repeat(n))),
        ("link boundaries before a far closer", |n| format!("{}{}]", "][".repeat(n), "a".repeat(n))),
        ("nested texts with empty labels", |n| format!("{}{}", "[".repeat(n), "][]".repeat(n))),
        ("addresses in link text", |n| "[http://a.example/x][r] ".repeat(n)),
        ("near-miss table lines", |n| "Корвин | Эрик\n| - | x\n".repeat(n)),
        ("setext headings", |n| format!("{}===\n\n", "Корвин\n".repeat(n))),
    ];
    for &(name, build) in shapes {
        scales_document(config.clone(), name, build, 2_000);
    }
}

#[test]
fn nowrap_context_reads_scale_linearly() {
    let limits = json!({"maxInputLength": 5_000_000, "maxProcessingMs": 0});
    let html = json!({"html": true, "maxInputLength": 5_000_000, "maxProcessingMs": 0});
    let shapes: &[(serde_json::Value, Shape)] = &[
        (html.clone(), ("labels after initials", |n| "Р. Г. ул. Мира в г. пр. Ленина ".repeat(n))),
        (
            html.clone(),
            ("labels before a rewritten end", |n| format!("{}а - б", "в г. пр. Ленина ул. пр. Мира ".repeat(n))),
        ),
        (
            html.clone(),
            ("account labels and split digit groups", |n| {
                format!("{}{}", "нэ. р/с 3786968270 и пр.арт. 3539555 ".repeat(n), "12345 ".repeat(2 * n))
            }),
        ),
        (
            json!({"html": true, "rules": {"other.drop_nested_nowrap": false}, "maxInputLength": 5_000_000, "maxProcessingMs": 0}),
            ("era marks with nested wrappers kept", |n| "у нэ. м те. слово слово слово слово ".repeat(n)),
        ),
        (
            json!({"html": true, "input": "html", "maxInputLength": 5_000_000, "maxProcessingMs": 0}),
            ("a label before a label past lifted markup", |n| "<b>Р</b> в г. пр. Ленина ул. пр. Мира ".repeat(n)),
        ),
        (
            html.clone(),
            ("initials and an address", |n| "Письмо получил А. С. Пушкин, ул. Мира, д. 5, г. Москва. ".repeat(n)),
        ),
        (html.clone(), ("era marks", |n| "В 5 в. до н. э. и в 3 в. н. э. жили люди. ".repeat(n))),
        (html.clone(), ("и т. д. and т. е.", |n| "Яблоки, груши и т. д. лежали, т. е. были тут. ".repeat(n))),
        (html, ("time ranges", |n| "Встреча с 10:00-11:00 и 12:30-13:45 в офисе, затем обед и т. д. ".repeat(n))),
        (limits.clone(), ("closers turned into openers", |n| " »\"а»»".repeat(n))),
        (limits, ("an incomplete dimension chain", |n| format!("{}1xL", "1x 1x ".repeat(n)))),
    ];
    for (config, (name, build)) in shapes {
        scales_document(config.clone(), name, *build, 2_000);
    }
}

#[test]
fn autolink_with_many_closing_parentheses_scales_linearly() {
    scales_document(
        json!({"html": true, "render": {"paragraphs": false}, "maxInputLength": 5_000_000, "maxProcessingMs": 0}),
        "autolink trailing parentheses",
        |n| format!("http://{}", ")".repeat(n)),
        10_000,
    );
}

#[test]
fn address_link_protection_scales_linearly() {
    let _alone = alone();
    let count = if cfg!(debug_assertions) { 500 } else { 2_000 };
    let config = json!({"input":"html","html":true,"render":{"paragraphs":false},"maxInputLength":5_000_000,"maxProcessingMs":0});
    let shapes: &[Shape] = &[
        ("address-text links", |n| "<a href=\"http://10000\">10000</a> ".repeat(n)),
        ("other link text", |n| "<a href=\"http://10000\">20000</a> ".repeat(n)),
        ("unterminated address href", |n| format!("<a href=\"http://{}", "x".repeat(32 * n))),
        ("partial address prefixes", |n| "<a href=\"http://xxxx ".repeat(n)),
    ];
    for &(name, build) in shapes {
        let mut engine = MicroTypo::new(Some(&config), HOST).unwrap();
        linear(name, count / 8, count, |n| best(&mut engine, &build(n)));
    }
}

#[test]
fn file_endings_and_table_scanners_scale_linearly() {
    let _alone = alone();
    let count = if cfg!(debug_assertions) { 250 / 4 } else { 250 };
    let config = json!({"input":"markdown","html":false,"maxInputLength":1_000_000,"maxProcessingMs":0});
    let shapes: &[Shape] = &[
        ("CRLF prose", |n| "Корвин - принц\r\nЭрик - король\r\n".repeat(n)),
        ("mixed file endings", |n| "Корвин - принц\r\nЭрик - король\nБлейз - брат\r".repeat(n)),
        ("table delimiters", |n| "| Альфа | Бета |\n| :- | -: |\n| Корвин - принц | Эрик |\n\n".repeat(n)),
        ("long non-delimiter row", |n| format!("| Альфа | Бета |\n| {}x | - |", "-".repeat(n * 20))),
    ];
    for &(name, build) in shapes {
        let mut engine = MicroTypo::new(Some(&config), HOST).unwrap();
        eprintln!("{name}: units {:?}", [count, 2 * count, 4 * count].map(|n| build(n).encode_utf16().count()));
        linear(name, count / 8, count, |n| best(&mut engine, &build(n)));
    }
}
