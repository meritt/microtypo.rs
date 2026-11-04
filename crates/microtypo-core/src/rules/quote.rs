use std::sync::LazyLock;

use serde_json::Value;

use crate::cat;
use crate::js::{self, Caps, JsString, Re};
use crate::protect::placeholders::{CLOSE, OPEN};
use crate::rule_runner::{Replacement, Rule, RuleGroupDef};
use crate::shared::glyphs::{HELLIP, LAQUO, MDASH, NDASH, RAQUO, unit};
use crate::shared::strings::count;

use super::quote_state::{OPEN_LEFT, QuoteOptions, SEPARATOR, is_digit, paragraph_separator, process_quotes};

const FIRS_OPEN: u16 = unit(LAQUO);
const FIRS_CLOSE: u16 = unit(RAQUO);
const QUOT: u16 = 0x22;

static QUOTES_OUTSIDE_A_RE: LazyLock<Re> =
    LazyLock::new(|| Re::new(&format!(r#"(<{OPEN}A\d+{CLOSE}>)"([\s\S]+?)"(<\/{OPEN}A\d+{CLOSE}>)"#), "g"));

static WORD_END_RE: LazyLock<Re> = LazyLock::new(|| Re::new(&format!("[a-zа-яё0-9{HELLIP}]$"), "iu"));
static LONE_SPACE_RE: LazyLock<Re> = LazyLock::new(|| Re::new(r"^\s$", "u"));
static SPACE_RE: LazyLock<Re> = LazyLock::new(|| Re::new(r"\s", "u"));

fn repeat_char(s: &[u16], ch: u16) -> JsString {
    vec![ch; count(s, &[QUOT])]
}

fn closes_before_paren(p1: &[u16], next: &[u16], offset: usize, source: &[u16]) -> bool {
    next.first() == Some(&0x28) && LONE_SPACE_RE.test(p1) && WORD_END_RE.test(js::slice(source, 0, offset))
}

static OPEN_AFTER_STRONG_RE: LazyLock<Re> =
    LazyLock::new(|| Re::new(&format!(r#"(^|[{}])((?:"|\\")+)([^{SEPARATOR}])"#, *OPEN_LEFT), "giu"));

static OPEN_AFTER_WEAK_RE: LazyLock<Re> =
    LazyLock::new(|| Re::new(&format!(r#"([,;:*_~`{CLOSE}])((?:"|\\")+)([^\s.,;:!?){RAQUO}{HELLIP}])"#), "giu"));

static OPEN_BEFORE_SPACE_RE: LazyLock<Re> =
    LazyLock::new(|| Re::new(&format!(r#"(^|\(|\s|[{LAQUO}{MDASH}{NDASH}])("|\\")([ \t]+)(\S+)"#), "giu"));

fn open_before_word(m: &Caps<'_>) -> JsString {
    let (p1, p2, p3) = (m.s(1), m.s(2), m.s(3));
    if closes_before_paren(p1, p3, m.index(), m.input) {
        return m.whole().to_vec();
    }
    cat!(p1, repeat_char(p2, FIRS_OPEN), p3)
}

static SPACED_OPEN_LEFT_RE: LazyLock<Re> = LazyLock::new(|| Re::new(&format!(r"[(\s{MDASH}{NDASH}]"), "u"));

fn token_start(text: &[u16], at: usize) -> usize {
    if at > 0 && text[at - 1] == 0x5C { at - 1 } else { at }
}

fn could_open(text: &[u16], at: usize) -> bool {
    let left = token_start(text, at) as isize - 1;
    if left >= 0 && !SPACED_OPEN_LEFT_RE.test(&text[(left as usize)..=(left as usize)]) {
        return false;
    }
    let mut i = at + 1;
    while text.get(i) == Some(&0x20) || text.get(i) == Some(&0x09) {
        i += 1;
    }
    i > at + 1 && i < text.len() && !SPACE_RE.test(&text[i..=i])
}

const OPENS: i32 = 1;
const UNDECIDED: i32 = 0;
const CLOSES: i32 = -1;

fn quote_role(text: &[u16], at: usize) -> i32 {
    let c = text[at];
    if c == FIRS_OPEN {
        return OPENS;
    }
    if c == FIRS_CLOSE {
        return CLOSES;
    }
    if could_open(text, at) { UNDECIDED } else { CLOSES }
}

fn empty_pair_at(text: &[u16], at: &[usize], j: usize, end: usize) -> bool {
    if j + 1 >= end || text[at[j]] != QUOT || text[at[j + 1]] != QUOT {
        return false;
    }
    let from = at[j] + 1;
    let to = token_start(text, at[j + 1]);
    if to <= from {
        return false;
    }
    text[from..to].iter().all(|&c| c == 0x20 || c == 0x09)
}

#[derive(Default)]
struct Assigned {
    at: Vec<usize>,
    roles: Vec<i32>,
    depths: Vec<i64>,
    pairs: Vec<bool>,
}

fn assign_paragraph_roles(text: &[u16], from: usize, stop: usize, a: &mut Assigned) {
    let base = a.at.len();
    let mut opens: i64 = 0;
    let mut closes: i64 = 0;
    let mut undecided: i64 = 0;
    for i in from..stop {
        let c = text[i];
        if c != FIRS_OPEN && c != FIRS_CLOSE && c != QUOT {
            continue;
        }
        let role = quote_role(text, i);
        let start = token_start(text, i);
        let before = if start > 0 { Some(text[start - 1]) } else { None };
        if role == CLOSES && opens - closes <= 0 && undecided == 0 && c == QUOT && is_digit(before) {
            continue;
        }
        a.at.push(i);
        a.roles.push(role);
        if role == OPENS {
            opens += 1;
        } else if role == CLOSES {
            closes += 1;
        } else {
            undecided += 1;
        }
    }

    let mut closings = undecided - ((closes + undecided - opens).div_euclid(2)).clamp(0, undecided);
    let end = a.at.len();
    let mut depth: i64 = 0;

    let mut need = vec![0i64; end - base + 1];
    for j in (base..end).rev() {
        let k = j - base;
        need[k] = if a.roles[j] == CLOSES { need[k + 1] + 1 } else { (need[k + 1] - 1).max(0) };
    }

    let mut later_undecided = undecided;
    let mut j = base;
    while j < end {
        if a.roles[j] == UNDECIDED {
            later_undecided -= 1;
            if empty_pair_at(text, &a.at, j, end) {
                later_undecided -= i64::from(a.roles[j + 1] == UNDECIDED);
                closings -= i64::from(closings > 0);
                a.roles[j] = OPENS;
                a.roles[j + 1] = CLOSES;
                a.depths.push(depth);
                a.depths.push(depth + 1);
                a.pairs.push(true);
                a.pairs.push(false);
                j += 2;
                continue;
            }
            let starves = depth - 1 < need[j - base + 1] && closings <= later_undecided;
            let shuts = depth > 0 && closings > 0 && !starves;
            a.roles[j] = if shuts { CLOSES } else { OPENS };
            closings -= i64::from(shuts);
        }
        a.depths.push(depth);
        a.pairs.push(false);
        depth = (depth + i64::from(a.roles[j])).max(0);
        j += 1;
    }
}

fn assign_quote_roles(text: &[u16]) -> Assigned {
    let separator = paragraph_separator(text);
    let mut a = Assigned::default();
    let mut from = 0;
    loop {
        let found = js::index_of(text, &separator, from);
        assign_paragraph_roles(text, from, found.unwrap_or(text.len()), &mut a);
        match found {
            None => return a,
            Some(f) => from = f + separator.len(),
        }
    }
}

fn open_spaced_quotes(text: &[u16]) -> JsString {
    if !text.contains(&QUOT) {
        return text.to_vec();
    }
    let re = &*OPEN_BEFORE_SPACE_RE;
    let mut parts: JsString = Vec::with_capacity(text.len());

    let mut assigned: Option<Assigned> = None;
    let mut seen = 0usize;
    let mut quote_at = |at: usize| -> (i32, i64, Option<usize>) {
        let a = assigned.get_or_insert_with(|| assign_quote_roles(text));
        while seen < a.at.len() && a.at[seen] < at {
            seen += 1;
        }
        if seen >= a.at.len() || a.at[seen] != at {
            return (UNDECIDED, 0, None);
        }
        (a.roles[seen], a.depths[seen], if a.pairs[seen] { Some(a.at[seen + 1] + 1) } else { None })
    };

    let mut cursor = 0usize;
    let mut last_index = 0usize;
    while let Some(m) = re.exec_at(text, last_index) {
        let (full, p1, p2, p4) = (m.whole(), m.s(1), m.s(2), m.s(4));
        let after_left = m.index() + p1.len();
        let (role, depth, pair_end) = quote_at(after_left + p2.len() - 1);

        if let Some(pair_end) = pair_end {
            parts.extend_from_slice(js::slice(text, cursor, m.index()));
            parts.extend_from_slice(p1);
            parts.push(FIRS_OPEN);
            parts.push(FIRS_CLOSE);
            cursor = pair_end;
            last_index = cursor;
            continue;
        }

        let opens_nested = depth > 0 && p1 != [FIRS_OPEN] && role == OPENS;

        if !opens_nested && (depth > 0 || closes_before_paren(p1, p4, m.index(), text)) {
            let after_quote = after_left + p2.len();
            parts.extend_from_slice(js::slice(text, cursor, after_quote));
            cursor = after_quote;
            last_index = after_quote;
            continue;
        }

        parts.extend_from_slice(js::slice(text, cursor, m.index()));
        parts.extend_from_slice(p1);
        parts.push(FIRS_OPEN);
        parts.extend_from_slice(p4);
        cursor = m.index() + full.len();
        last_index = cursor;
    }
    parts.extend_from_slice(js::from(text, cursor));
    parts
}

fn setting_on(settings: Option<&serde_json::Map<String, Value>>, key: &str) -> bool {
    settings.and_then(|s| s.get(key)) != Some(&Value::Bool(false))
}

fn close_tail(m: &Caps<'_>) -> JsString {
    cat!(m.s(1), repeat_char(m.s(2), FIRS_CLOSE))
}

fn close_before_tag(m: &Caps<'_>) -> JsString {
    let n = count(m.s(2), &[QUOT]) + count(m.s(2), &[FIRS_OPEN]);
    cat!(m.s(1), vec![FIRS_CLOSE; n], m.s(3), m.s(4))
}

fn close_spaced(m: &Caps<'_>) -> JsString {
    let n = count(m.s(3), &[QUOT]) + count(m.s(3), &[FIRS_OPEN]);
    cat!(m.s(1), m.s(2), vec![FIRS_CLOSE; n], m.s(4), m.s(5))
}

fn close_lost_word(m: &Caps<'_>) -> JsString {
    cat!(m.s(1), repeat_char(m.s(3), FIRS_CLOSE))
}

fn close_at_end(m: &Caps<'_>) -> JsString {
    repeat_char(m.s(1), FIRS_CLOSE)
}

fn close_before_punct(m: &Caps<'_>) -> JsString {
    cat!(m.s(1), repeat_char(m.s(2), FIRS_CLOSE), m.s(3))
}

fn func(f: fn(&Caps<'_>) -> JsString) -> Replacement {
    Replacement::func(move |m, _| Ok(f(m)))
}

pub fn quote_group() -> RuleGroupDef {
    const WORD_END: &str = r"([a-zа-яё0-9]|\.|…|!|\?|>|\)|:|\+|%|@|#|\$|\*)";
    super::group(
        "Кавычки",
        None,
        vec![
            Rule::pattern("around_link", QUOTES_OUTSIDE_A_RE.clone(), Replacement::tpl(r#""$1$2$3""#))
                .describe("Кавычки вынесены за тег <a>"),
            Rule::handler("open", |ctx| {
                if !ctx.text.contains(&QUOT) {
                    return Ok(None);
                }
                let stepped = OPEN_AFTER_STRONG_RE.replace_with(&ctx.text, open_before_word);
                let stepped = OPEN_AFTER_WEAK_RE.replace_with(&stepped, open_before_word);
                Ok(Some(open_spaced_quotes(&stepped)))
            })
            .describe("Открывающая кавычка"),
            Rule::pattern(
                "open_cyrillic",
                Re::new(r#"([a-zа-яё0-9])"([а-яё])"#, "giu"),
                Replacement::Template(cat!("$1 ", FIRS_OPEN, "$2")),
            )
            .describe("Приклеенная кавычка у кириллицы → « с пробелом"),
            Rule::patterns(
                "close",
                vec![
                    (
                        Re::new(r#"(?<=\\[\w:]*)\\"(?=$|\s|[.,;:!?)]|<)"#, "u"),
                        Replacement::Template(cat!("\\", FIRS_CLOSE)),
                    ),
                    (Re::new(&format!(r#"{WORD_END}((?:"|\\")+)(?![a-zа-яё0-9])"#), "giu"), func(close_tail)),
                    (
                        Re::new(&format!(r#"{WORD_END}((?:"|\\"|«)+)(<[^>]+>)(\.|…|;|:|\?|!|,|\)|<\/|$| )"#), "giu"),
                        func(close_before_tag),
                    ),
                    (
                        Re::new(&format!(r#"{WORD_END}(\s+)((?:"|\\")+)(\s+)(\.|…|;|:|\?|!|,|\)|\(|<\/|$| )"#), "giu"),
                        func(close_spaced),
                    ),
                    (Re::new(r#"([a-zа-яё0-9…!?%)])(\s+)((?:"|\\")+)(?=\s)"#, "giu"), func(close_lost_word)),
                    (Re::new(&format!(r#"({LAQUO})([ \t]+)((?:"|\\")+)(?=\s)"#), "gu"), func(close_lost_word)),
                    (Re::new(r">«\.($|\s|<)", "gu"), Replacement::Template(cat!(">", FIRS_CLOSE, ".$1"))),
                    (Re::new(r">«,($|\s|<|\S)", "gu"), Replacement::Template(cat!(">", FIRS_CLOSE, ",$1"))),
                    (Re::new(r">«:($|\s|<|\S)", "gu"), Replacement::Template(cat!(">", FIRS_CLOSE, ":$1"))),
                    (Re::new(r">«;($|\s|<|\S)", "gu"), Replacement::Template(cat!(">", FIRS_CLOSE, ";$1"))),
                    (Re::new(r">«\)($|\s|<|\S)", "gu"), Replacement::Template(cat!(">", FIRS_CLOSE, ")$1"))),
                    (Re::new(r#"((?:"|\\")+)$"#, "giu"), func(close_at_end)),
                    (
                        Re::new(
                            &format!(r#"([^{SEPARATOR}])((?:"|\\")+)(\.|{HELLIP}|;|:|\?|!|,|\s|\)|<\/|<|$)"#),
                            "giu",
                        ),
                        func(close_before_punct),
                    ),
                ],
            )
            .describe("Закрывающая кавычка"),
            Rule::pattern(
                "fix_orphan_open",
                Re::new(r"([a-zа-яё0-9])«(?=\s|$)", "giu"),
                Replacement::Template(cat!("$1", FIRS_CLOSE)),
            )
            .describe("Открывающая «, приклеенная к концу слова перед пробелом/концом → закрывающая »"),
            Rule::handler("nested", |ctx| {
                let settings = ctx.group.as_ref().map(|g| &g.settings);
                let options = QuoteOptions {
                    allow_nested: setting_on(settings, "allowNested"),
                    convert_inches: setting_on(settings, "convertInches"),
                    check_budget: Some(ctx.check_budget),
                };
                Ok(Some(process_quotes(&ctx.text, &options)?))
            })
            .describe("Внутренние кавычки-лапки и дюймы"),
        ],
    )
}

#[cfg(test)]
mod tests {
    use serde_json::{Map, Value};

    use crate::js::{lossy, u};
    use crate::protect::blocks::SafeBlocks;
    use crate::protect::sequences::SafeSequences;
    use crate::protect::tags::{DEFAULT_LAYOUT, SafeTags};
    use crate::rule_runner::{Ctx, compile_rule_group, run_rule_group};

    use super::quote_group;

    const GROUP_CASES: &[(&str, bool, bool, &str)] = &[
        ("Он сказал \"привет\" и ушёл", true, true, "Он сказал «привет» и ушёл"),
        ("\"Мама \"мыла\" раму\"", true, true, "«Мама „мыла“ раму»"),
        ("\"Книга \"Война и \"мир\"\" прочитана\"", true, true, "«Книга „Война и „мир““ прочитана»"),
        ("Экран 15\" и 17\"", true, true, "Экран 15″ и 17″"),
        ("Доска 2\"x4\" длиной", true, true, "Доска 2\"x4″ длиной"),
        ("It's \"quoted\" text, isn't it?", true, true, "It's «quoted» text, isn't it?"),
        ("\" Новый Амбер\" вышел", true, true, "«Новый Амбер» вышел"),
        ("Слово \" отдельно \" стоит", true, true, "Слово «отдельно» стоит"),
        ("Пустые \" \" кавычки", true, true, "Пустые «» кавычки"),
        ("**\"Амбер\"** и \"это *важно*\".", true, true, "**«Амбер»** и «это *важно*»."),
        (
            "Ссылка <\u{e000}A1\u{e001}>\"текст\"</\u{e000}A1\u{e001}> тут",
            true,
            true,
            "Ссылка «<\u{e000}A1\u{e001}>текст</\u{e000}A1\u{e001}>» тут",
        ),
        ("слово\"кириллица", true, true, "слово «кириллица"),
        ("\"Мама \"мыла\" раму\"", false, true, "«Мама «мыла» раму»"),
        ("Экран 15\" и \"фильм\"", true, false, "Экран 15» и «фильм»"),
        ("путь C:\\dir\\\"", true, true, "путь C:\\dir\\\""),
        ("Он сказал: \"Корвин\" (принц)", true, true, "Он сказал: «Корвин» (принц)"),
        ("\"Первый\"\n\n\"Второй \"вложенный\"\"", true, true, "«Первый»\n\n«Второй „вложенный“»"),
        ("«ёлка» и «а «б» в»", true, true, "«ёлка» и «а „б“ в»"),
        ("цитата \"a\", \"b\"; \"c\": \"d\"!", true, true, "цитата «a», «b»; «c»: «d»!"),
        ("😀 \"эмодзи 😀\" 12\" 😀", true, true, "😀 «эмодзи 😀» 12″ 😀"),
        ("\"раз\" \"два\" \"три", true, true, "«раз» «два» «три"),
        ("слово «, дальше", true, true, "слово «, дальше"),
        ("Он сказал \"Кот\" (2020) и", true, true, "Он сказал «Кот» (2020) и"),
    ];

    fn run(text: &str, allow_nested: bool, convert_inches: bool) -> String {
        let mut group = compile_rule_group(&quote_group(), "quote").unwrap();
        if !allow_nested {
            group.set_setting("allowNested", Value::Bool(false));
        }
        if !convert_inches {
            group.set_setting("convertInches", Value::Bool(false));
        }
        let settings = Map::new();
        let mut tags = SafeTags::new(false);
        let blocks = SafeBlocks::default();
        let sequences = SafeSequences::default();
        let mut ctx = Ctx {
            text: Vec::new(),
            group: None,
            html: true,
            layout: DEFAULT_LAYOUT,
            engine_settings: &settings,
            check_budget: &|_| Ok(()),
            on_cycle_limit: None,
            safe_tags: &mut tags,
            safe_blocks: &blocks,
            safe_sequences: &sequences,
            prefix: "",
            budget: None,
        };
        lossy(&run_rule_group(&group, u(text), &mut ctx).unwrap())
    }

    #[test]
    fn matches_reference() {
        for &(input, allow_nested, convert_inches, expected) in GROUP_CASES {
            assert_eq!(run(input, allow_nested, convert_inches), expected, "input: {input:?}");
        }
    }
}
