use crate::cat;
use crate::js::{self, Re};
use crate::rule_runner::{Replacement, Rule, RuleGroupDef};

use super::span;

pub fn opt_align_group() -> RuleGroupDef {
    super::group(
        "Оптическое выравнивание",
        super::classes(&[
            ("oa_obracket_sp_s", "margin-right:0.3em;"),
            ("oa_obracket_sp_b", "margin-left:-0.3em;"),
            ("oa_obracket_nl_b", "margin-left:-0.3em;"),
            ("oa_comma_b", "margin-right:-0.2em;"),
            ("oa_comma_e", "margin-left:0.2em;"),
            ("oa_oquote_nl", "margin-left:-0.44em;"),
            ("oa_oquote_sp_s", "margin-right:0.44em;"),
            ("oa_oquote_sp_q", "margin-left:-0.44em;"),
        ]),
        vec![
            Rule::patterns(
                "quote",
                vec![
                    (
                        Re::new(r"([a-zа-яё-]{3,})( |\u{00A0}|\t)(«)", "giu"),
                        Replacement::func(|m, ctx| {
                            let space = span(ctx, m.s(2), "oa_oquote_sp_s")?;
                            let quote = span(ctx, m.s(3), "oa_oquote_sp_q")?;
                            Ok(cat!(m.s(1), space, quote))
                        }),
                    ),
                    (
                        Re::new(r"(\n|\r|^)(«)", "gu"),
                        Replacement::func(|m, ctx| Ok(cat!(m.s(1), span(ctx, m.s(2), "oa_oquote_nl")?))),
                    ),
                ],
            )
            .describe("Оптическое выравнивание открывающей кавычки")
            .off()
            .html_only(),
            Rule::patterns(
                "bracket",
                vec![
                    (
                        Re::new(r"( |\u{00A0}|\t)\(", "gu"),
                        Replacement::func(|m, ctx| {
                            let space = span(ctx, m.s(1), "oa_obracket_sp_s")?;
                            let bracket = span(ctx, &js::u("("), "oa_obracket_sp_b")?;
                            Ok(cat!(space, bracket))
                        }),
                    ),
                    (
                        Re::new(r"(\n|\r|^)\(", "g"),
                        Replacement::func(|m, ctx| Ok(cat!(m.s(1), span(ctx, &js::u("("), "oa_obracket_nl_b")?))),
                    ),
                ],
            )
            .describe("Оптическое выравнивание скобок")
            .off()
            .html_only(),
        ],
    )
}
