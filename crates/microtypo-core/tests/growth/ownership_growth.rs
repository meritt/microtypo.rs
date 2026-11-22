use microtypo_conformance::HOST;
use microtypo_core::{ErrorKind, MicroTypo};
use serde_json::json;

use crate::timing::{alone, best, linear};

type Shape = (&'static str, &'static str, fn(usize) -> String, bool);

#[test]
fn opaque_owner_boundaries_scale_linearly_without_moving_bytes() {
    let _alone = alone();
    let shapes = [
        ("comment", "<!--{%-->{%%} Корвин - принц ", "<!--{%-->{%%} Корвин\u{a0}— принц "),
        ("declaration", "<?{%?> Корвин - принц %} ", "<?{%?> Корвин\u{a0}— принц %} "),
        ("CDATA", "<svg><![CDATA[{%]]></svg> Корвин - принц %} ", "<svg><![CDATA[{%]]></svg> Корвин\u{a0}— принц %} "),
    ];
    for format in ["html", "markdown", "mdx"] {
        for (name, unit, output) in shapes {
            measure_owner(format, "liquid", name, unit, output);
        }
    }
    measure_owner(
        "mdx",
        "jinja",
        "independent MDX endpoint",
        "<svg data=\"{#\"/><script/>Корвин - \"принц\"</script>#} Амбер - город ",
        "<svg data=\"{#\"/><script/>Корвин - \"принц\"</script>#} Амбер\u{a0}— город ",
    );
}

fn measure_owner(format: &str, template: &str, name: &str, unit: &str, output: &str) {
    let mut engine = MicroTypo::new(
        Some(&json!({"input":{"format":format,"template":template},"maxInputLength":1_000_000,"maxProcessingMs":0})),
        HOST,
    )
    .unwrap();
    linear(&format!("{format} {name}"), 64, 256, |n| {
        let source = unit.repeat(n).trim_end().to_owned();
        let expected = output.repeat(n).trim_end().to_owned();
        best(|| assert_eq!(engine.process(&source).unwrap(), expected, "{format} {name}"))
    });
}

#[test]
fn syntax_ownership_scales_linearly_with_exact_outputs() {
    let _alone = alone();
    let forms: &[Shape] = &[
        (
            "foreign rollback",
            "html",
            |n| format!("{}<a title=\"</code>\"> Корвин - принц {{% raw %}}x{{% endraw %}}", "<code><svg>".repeat(n)),
            false,
        ),
        (
            "custom foreign rollback",
            "html",
            |n| format!("<svg>{}<a title=\"</review-safe>\">", "<review-safe>{% raw %}<g>{% endraw %}".repeat(n)),
            false,
        ),
        (
            "declaration and reverse raw",
            "html",
            |n| {
                "<?php echo \"{% raw %}\"; ?> Корвин - принц {% endraw %} {% raw %}<span title=\"{% endraw %}<code>{% raw %}</code> Корвин - принц {% endraw %} ".repeat(n).trim_end().to_owned()
            },
            true,
        ),
        (
            "atomic opener cache",
            "markdown",
            |n| {
                "До <code><code data={{ \"> <span title='\" }}> {% raw %}</code> Корвин - принц {% endraw %}<a title=\"</code>\">\n\n".repeat(n)
            },
            true,
        ),
        (
            "ordinary paragraph bound",
            "frontmatter",
            |n| "x <pre data=\n\n`  уложения  меж\tсверху`\n\n> Корвин - принц\n\n".repeat(n),
            true,
        ),
        (
            "early MDX blocks",
            "mdx",
            |n| {
                "<span title=\"{% raw %}\"></span>\n\n<pre>\n~~~\nКод - \"сырой\"\n</pre>\n\nКорвин - принц\n\n{% endraw %}\n\n".repeat(n)
            },
            true,
        ),
    ];
    for &(name, format, build, typeset) in forms {
        let mut engine = MicroTypo::new(
            Some(
                &json!({"input":{"format":format,"template":"liquid"},"maxInputLength":1_000_000,"maxProcessingMs":0}),
            ),
            HOST,
        )
        .unwrap();
        engine.add_safe_tag("review-safe").unwrap();
        let count = if cfg!(debug_assertions) { 64 } else { 256 };
        linear(name, count / 4, count, |n| {
            let source = build(n);
            let expected = if typeset {
                source.replace("Корвин - принц", "Корвин\u{a0}— принц")
            } else {
                source.clone()
            };
            best(|| assert_eq!(engine.process(&source).unwrap(), expected, "{name}"))
        });
    }
}

#[test]
fn ownership_preserves_expression_bounds_deadlines_and_repeated_calls() {
    let mut engine = MicroTypo::new(
        Some(&json!({"input":{"format":"html","template":"liquid"},"maxInputLength":1_000_000,"maxProcessingMs":0})),
        HOST,
    )
    .unwrap();
    let error = engine.process(&format!("{} }}}}", "{{ \"".repeat(5000))).unwrap_err();
    assert_eq!(error.kind, ErrorKind::Input);
    assert_eq!(error.details["reason"], "template-overlimit");
    let mut limited = MicroTypo::new(
        Some(&json!({"input":{"format":"html","template":"liquid"},"maxInputLength":1_000_000,"maxProcessingMs":1})),
        HOST,
    )
    .unwrap();
    let error = limited
        .process(&format!("{}<a title=\"</code>\"> {{% raw %}}x{{% endraw %}}", "<code><svg>".repeat(20_000)))
        .unwrap_err();
    assert_eq!(error.kind, ErrorKind::Budget);
    assert_eq!(engine.process("Корвин - принц").unwrap(), "Корвин\u{a0}— принц");
}

#[test]
fn exhausted_inline_readers_do_not_rescan_multiline_prose() {
    let _alone = alone();
    for input in [json!({"format":"mdx"}), json!({"format":"markdown","template":"liquid"})] {
        let mut engine = MicroTypo::new(
            Some(&json!({"input":input,"presets":false,"maxInputLength":1_000_000,"maxProcessingMs":0})),
            HOST,
        )
        .unwrap();
        linear(&format!("exhausted {input}"), 1000, 4000, |n| {
            let source = "ordinary words\n".repeat(n);
            best(|| assert_eq!(engine.process(&source).unwrap(), source))
        });
    }
}
