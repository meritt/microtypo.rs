use microtypo_conformance::HOST;
use microtypo_core::MicroTypo;
use serde_json::json;

use crate::support::settles;

#[test]
fn frontmatter_keeps_its_body_boundary() {
    let mut engine = MicroTypo::new(Some(&json!({"html":false,"input":{"format":"frontmatter"}})), HOST).unwrap();
    for open in ["---", "+++"] {
        for boundary in ["", "\n", "\n\n\n", " \t\r\n\n\t\r"] {
            let header = if open == "---" { "title: Амбер" } else { "title = \"Амбер\"" };
            let lead = format!("{open}\n{header}\n{open}\n");
            let input = format!("{lead}{boundary}Корвин - принц");
            let expected = format!("{lead}{boundary}Корвин\u{00A0}— принц");
            settles(&mut engine, &input, &expected);
        }
    }
}

#[test]
fn frontmatter_boundary_keeps_the_first_code_line_indent() {
    let mut engine = MicroTypo::new(Some(&json!({"html":false,"input":"frontmatter"})), HOST).unwrap();
    let input = "---\ntitle: Амбер\n---\n \t\n    Корвин - принц";
    assert_eq!(engine.process(input).unwrap(), input);
}

#[test]
fn frontmatter_without_a_header_still_trims_leading_blank_lines() {
    let mut engine = MicroTypo::new(Some(&json!({"html":false,"input":"frontmatter"})), HOST).unwrap();
    assert_eq!(engine.process("\n\nКорвин - принц").unwrap(), "Корвин\u{00A0}— принц");
}

#[test]
fn preserving_frontmatter_boundary_keeps_the_original_first_pass_context() {
    let mut engine = MicroTypo::new(Some(&json!({"html":false,"input":"frontmatter"})), HOST).unwrap();
    let header = "---\ntitle: test\n---\n";
    for boundary in ["\n", "\n\n", "\r\n"] {
        for (body, typed) in [("1997год.", "1997\u{a0}год."), ("Гост 5773-90", "ГОСТ\u{a0}5773–90")] {
            let input = format!("{header}{boundary}{body}");
            let expected = format!("{header}{boundary}{typed}");
            settles(&mut engine, &input, &expected);
        }
    }
}
