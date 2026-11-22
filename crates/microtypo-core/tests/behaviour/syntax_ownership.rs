use microtypo_conformance::HOST;
use microtypo_core::{ErrorKind, MicroTypo};
use serde_json::json;

use crate::support::settles;

#[test]
fn markdown_comments_and_overlapping_liquid_headers_keep_their_owned_bytes() {
    for format in ["text", "html", "markdown", "frontmatter", "mdx"] {
        let mut engine = MicroTypo::new(Some(&json!({"input":{"format":format,"template":"liquid"}})), HOST).unwrap();
        let source = "<!--{%-->{%%}";
        assert_eq!(engine.process(source).unwrap(), source);
        for source in
            ["<!--{%-->{%%} Корвин - принц", "<?{%?> Корвин - принц %}", "<svg><![CDATA[{%]]></svg> Корвин - принц %}"]
        {
            let expected = source.replace("Корвин - принц", "Корвин\u{a0}— принц");
            settles(&mut engine, source, &expected);
        }
    }
}

#[test]
fn independent_mdx_endpoints_bound_template_atoms_and_foreign_self_closing_state() {
    for (template, head, tail) in [("jinja", "{#", "#}"), ("handlebars", "{{!", "}}"), ("erb", "<%#", "%>")] {
        let mut engine =
            MicroTypo::new(Some(&json!({"input":{"format":"mdx","template":template},"maxInputLength":100000})), HOST)
                .unwrap();
        for padding in [String::new(), "x".repeat(9000)] {
            let source =
                format!("<svg data=\"{head}\"/><script/>Корвин - \"принц\"</script>{padding}{tail} Амбер - город");
            let expected = source.replace("Амбер - город", "Амбер\u{a0}— город");
            settles(&mut engine, &source, &expected);
        }
    }
}

#[test]
fn xml_processing_instructions_inside_a_preserved_opener_keep_its_attributes() {
    let mut engine = MicroTypo::new(Some(&json!({"input":"xml"})), HOST).unwrap();
    for opener in
        ["<x<??> xml:space=\"pre&#115;erve\">", "<x<!-- --> xml:space=\"preserve\">", "<x xml:space=\"preserve\">"]
    {
        let source = format!("<r>{opener}Корвин - принц</x> Корвин - принц</r>");
        let expected = format!("<r>{opener}Корвин - принц</x> Корвин\u{a0}— принц</r>");
        settles(&mut engine, &source, &expected);
    }
    let source = "<r><x<??> xml:space=\"pre&#115;erve\"></x></r>";
    assert_eq!(engine.process(source).unwrap(), source);
}

#[test]
fn mdx_foreign_openers_with_a_literal_angle_do_not_panic() {
    let mut engine = MicroTypo::new(Some(&json!({"input":"mdx"})), HOST).unwrap();
    for source in ["<svg \">a/>", "<svg \">a/b/>", "<svg \">a b/>", "<svg \">aa/>", "<svg title=\"a>b\"/>"] {
        assert_eq!(engine.process(source).unwrap(), source);
    }
}

#[test]
fn templates_and_tags_keep_the_first_owner_and_resume_prose() {
    let sources = [
        "<?php echo \"{% raw %}\"; ?> Корвин - принц {% endraw %}",
        "<span title=\"{% raw %}\">Корвин - принц</span> {% endraw %}",
        "{% raw %}<span title=\"{% endraw %}<code>{% raw %}</code> Корвин - принц {% endraw %}",
    ];
    for format in ["text", "html", "markdown"] {
        let mut engine = MicroTypo::new(Some(&json!({"input":{"format":format,"template":"liquid"}})), HOST).unwrap();
        for source in sources {
            let expected = source.replace("Корвин - принц", "Корвин\u{a0}— принц");
            settles(&mut engine, source, &expected);
        }
    }
}

#[test]
fn an_atomic_opener_does_not_reuse_a_failed_literal_body_probe() {
    let source = "До <code><code data={{ \"> <span title='\" }}> {% raw %}</code> Корвин - принц {% endraw %}<i title=\"</code>\">";
    let expected = source.replace("Корвин - принц", "Корвин\u{a0}— принц");
    let mut engine = MicroTypo::new(Some(&json!({"input":{"format":"markdown","template":"liquid"}})), HOST).unwrap();
    settles(&mut engine, source, &expected);
}

#[test]
fn an_unclosed_protected_name_cannot_own_the_next_markdown_paragraph() {
    for head in ["x <span data=", "x <<x,", "x <span title=\"", "x <code data=", "x <pre data="] {
        let held = "`  уложения  меж\tсверху`";
        let source = format!("{head}\n\n{held}\n\n> Корвин - принц");
        let expected = format!("<p>{head}</p>\n\n<p>{held}</p>\n\n> Корвин\u{a0}— принц");
        let mut engine = MicroTypo::new(
            Some(&json!({"input":"markdown","html":true,"render":{"paragraphs":true,"breakline":true}})),
            HOST,
        )
        .unwrap();
        settles(&mut engine, &source, &expected);
    }
}

#[test]
fn html_raw_elements_and_noncanonical_closers_keep_their_bodies() {
    for format in ["text", "html"] {
        let mut engine = MicroTypo::new(Some(&json!({"input":format})), HOST).unwrap();
        for tag in ["xmp", "title", "noembed", "script", "textarea"] {
            for close in [format!("</{tag} x>"), format!("</{tag}/>")] {
                let source = format!("<{tag}><svg>Код - \"сырой\"{close} Корвин - принц");
                let mut expected = source.replace("Корвин - принц", "Корвин\u{a0}— принц");
                if tag == "title" {
                    expected = expected.replace("Код - \"сырой\"", "Код\u{a0}— «сырой»");
                }
                settles(&mut engine, &source, &expected);
            }
        }
    }
}

#[test]
fn generated_first_paragraph_holds_the_authored_indent_and_stays_stable() {
    let mut engine = MicroTypo::new(Some(&json!({"html":true,"rules":{"hanging.*":false}})), HOST).unwrap();
    settles(
        &mut engine,
        "  Корвин - принц\n\n  Эрик - король",
        "<p>  Корвин\u{a0}— принц</p>\n\n<p>Эрик\u{a0}— король</p>",
    );
    let authored = "<p>  Корвин\u{a0}— принц</p>\n\n<p>  Эрик\u{a0}— король</p>";
    assert_eq!(engine.process(authored).unwrap(), authored);
    let inline = "<span>начало<p>абзац</p>конец</span>\n\n<p>далее</p>";
    assert_eq!(engine.process(inline).unwrap(), inline);
}

#[test]
fn yaml_tag_handles_and_empty_keys_select_the_resolved_paths() {
    for source in ["{ : Корвин - принц }", "?\n: Корвин - принц"] {
        let mut engine = MicroTypo::new(Some(&json!({"input":{"format":"yaml","exclude":["/null"]}})), HOST).unwrap();
        assert_eq!(engine.process(source).unwrap(), source);
    }
    let source = "%TAG !e! tag:yaml.org,2002:\n---\n!e!str title: Корвин - принц\n";
    let mut engine = MicroTypo::new(Some(&json!({"input":{"format":"yaml","fields":["/title"]}})), HOST).unwrap();
    assert_eq!(engine.process(source).unwrap(), source.replace("Корвин - принц", "Корвин\u{a0}— принц"));
}

#[test]
fn xml_unterminated_comment_after_a_processing_instruction_is_an_input_error() {
    let mut engine = MicroTypo::new(Some(&json!({"input":"xml"})), HOST).unwrap();
    let error = engine.process("<?<code ?><doc><title></title><!-- a@a.--></doc>").unwrap_err();
    assert_eq!(error.kind, ErrorKind::Input);
    assert_eq!(error.message, "Unterminated XML <!-- construct at offset 30");
    assert_eq!(error.details, json!({"offset":30,"construct":"<!--"}));
    assert_eq!(engine.process("<doc>\"а\"</doc>").unwrap(), "<doc>«а»</doc>");
}

#[test]
fn toml_rejects_bare_cr_with_the_reference_error() {
    let mut engine = MicroTypo::new(Some(&json!({"input":"toml"})), HOST).unwrap();
    let error = engine.process("title = \"Корвин\"\rnext = \"Эрик\"").unwrap_err();
    assert_eq!(error.kind, ErrorKind::Input);
    assert_eq!(error.message, "TOML newlines must use LF or CRLF; bare CR is not permitted");
    assert_eq!(error.details, json!({"offset":16,"reason":"toml-bare-cr"}));
}
