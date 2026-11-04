use std::sync::LazyLock;

use crate::cat;
use crate::js::{Caps, JsString, Re};
use crate::protect::placeholders::CLOSE;
use crate::rule_runner::{Ctx, Replacement, Rule, RuleGroupDef};
use crate::shared::boundaries::{CONTENT_END_LOOKAHEAD, CONTENT_START_GROUP};
use crate::shared::glyphs::{BDQUO, HELLIP, LAQUO, MDASH, NBSP, NDASH};

fn join_hyphen(m: &Caps<'_>, _: &mut Ctx<'_>) -> crate::Result<JsString> {
    Ok(cat!(m.s(1), m.s(2), "-", m.s(4)))
}

fn hyphen_join(head: &str, tail: &str, lead: &str, trail: &str, not_before: &str) -> Re {
    Re::new(
        &format!(
            "{}({head}){lead}( |\\t|{NBSP})+{trail}({tail}){not_before}{}",
            *CONTENT_START_GROUP, *CONTENT_END_LOOKAHEAD
        ),
        "giu",
    )
}

static AUTHORED_DASH: LazyLock<String> = LazyLock::new(|| format!("{MDASH}{NDASH}-"));

pub static SENTENCE_DASH: LazyLock<String> = LazyLock::new(|| format!(" {MDASH}{NBSP}"));

const RANGE_DASH: &str = "\u{2013}-";

pub fn dash_group() -> RuleGroupDef {
    let authored = &*AUTHORED_DASH;
    let em_after_content = Re::new(
        &format!(r"([^\s(\[{{<.!?{LAQUO}{BDQUO}{HELLIP}{NDASH}{MDASH}-])( |\t|{NBSP})+([{authored}])(\s|$|<)"),
        "giu",
    );
    let em_glued = Re::new(&format!(r#"(,|:|\)|")([{authored}])(\s|$|<)"#), "giu");
    let dash_gap = format!("(?: |\\t|{NBSP})+");
    let en_range = Re::new(
        &format!(r"(?<![\d{RANGE_DASH}])(\d+){dash_gap}[{RANGE_DASH}]{dash_gap}(\d+)(?![\d{RANGE_DASH}])"),
        "gu",
    );
    let join = || Replacement::func(join_hyphen);
    super::group(
        "Дефисы и тире",
        None,
        vec![
            Rule::pattern(
                "em_double_hyphen",
                Re::new(&format!(r"(\s|^|{CLOSE})--(?=\s)"), "gu"),
                Replacement::Template(cat!("$1", MDASH)),
            )
            .describe("Двойной дефис → тире (как в markdown/AsciiDoc)"),
            Rule::pattern("en_range", en_range, Replacement::Template(cat!("$1", NDASH, "$2")))
                .describe("Диапазон чисел с пробелами вокруг дефиса: «12 - 19» → «12–19»"),
            Rule::patterns(
                "em",
                vec![
                    (em_after_content, Replacement::Template(cat!("$1", NBSP, MDASH, "$4"))),
                    (em_glued, Replacement::Template(cat!("$1", NBSP, MDASH, "$3"))),
                ],
            )
            .describe("Тире после слов, кавычек, скобочек, пунктуации"),
            Rule::pattern(
                "em_line_start",
                Re::new(&format!(r"(\n|\r|^|>|{CLOSE})[{authored}](\t| )"), "gu"),
                Replacement::Template(cat!("$1", MDASH, NBSP)),
            )
            .describe("Тире после переноса строки"),
            Rule::patterns(
                "em_after_sentence",
                vec![
                    (
                        Re::new(&format!(r"([.!?{HELLIP}])(?: |\t|{NBSP})+[{authored}](?: |\t|{NBSP})+"), "gu"),
                        Replacement::Template(cat!("$1", SENTENCE_DASH.as_str())),
                    ),
                    (
                        Re::new(&format!(r"([.!?{HELLIP}])(?: |\t|{NBSP})+[{authored}]$"), "gu"),
                        Replacement::Template(cat!("$1", NBSP, MDASH)),
                    ),
                ],
            )
            .describe("Тире после знаков восклицания, троеточия и прочее"),
            Rule::pattern("compound_preposition", hyphen_join("из", "за|под", "", "-?", ""), join())
                .describe("Дефис между из-за, из-под"),
            Rule::pattern(
                "indefinite_pronoun",
                hyphen_join("кто|кем|когда|зачем|почему|как|что|чем|где|чего|кого", "то|либо|нибудь", "-?", "-?", "(?!:)"),
                join(),
            )
            .describe("Дефисы в обезличенных местоимениях"),
            Rule::patterns(
                "hyphenated_particle",
                vec![
                    (
                        hyphen_join(
                            "кое",
                            "как|что|кто|кого|кому|чей|чья|чьё|чьи|где|куда|откуда|когда|какой|какая|какое|какие|каких|каким",
                            "-?",
                            "-?",
                            "",
                        ),
                        join(),
                    ),
                    (hyphen_join("кой", "кого", "-?", "-?", ""), join()),
                    (hyphen_join("вс[её]", "таки", "-?", "-?", ""), join()),
                ],
            )
            .describe("Кое-как, кой-кого, всё-таки"),
            Rule::pattern("emphatic_particle", hyphen_join("[а-яё]+", "кась|тка|тко|ка|де", "", "", ""), join())
                .describe("Дефис с усилительными частицами ка, кась, тка, тко, де"),
        ],
    )
}
