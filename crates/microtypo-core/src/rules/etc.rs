use std::collections::HashSet;
use std::sync::{Arc, LazyLock, Mutex};

use crate::cat;
use crate::errors::Result;
use crate::js::{self, JsString, Re};
use crate::protect::placeholders::{CLOSE, INLINE_END, OPEN, TRANSPARENT_TAG_KINDS, vaulted_reference};
use crate::protect::tags::NameMatcher;
use crate::rule_runner::{Ctx, Replacement, Rule, RuleGroupDef};
use crate::shared::abbreviation_grammar::{
    CENTURY_MARKER, CENTURY_RANGE, follows_complete_name, has_dotted_location_after, starts_with_compact_name,
};
use crate::shared::boundaries::CONTENT_START_GROUP;
use crate::shared::glyphs::{COMBINING_ACUTE, LDQUO, NBSP, NDASH, NNBSP, NUMERO, RAQUO, unit};
use crate::shared::inline_boundaries::{inline_text_end, inline_text_start};
use crate::shared::nowrap_context::{
    NOWRAP_TAG_RE as ANY_TAG_RE, nowrap as wrap_nowrap, nowrap_contexts, span_token_set,
};
use crate::shared::strings::cycle;

use super::dash::LINE_GAP;
use super::nowrap;
use super::number::{joined_numeric_token, phone_context_at};
use crate::shared::rule_features::F;

struct NobrPatterns {
    open: JsString,
    close: JsString,
    m1: Re,
    m2: Re,
}

fn nobr_patterns(open: JsString, close: &[u16]) -> NobrPatterns {
    let o = js::escape(&open);
    let c = js::escape(close);

    NobrPatterns {
        m1: Re::configured(
            &format!("{}(?<![\\p{{L}}\\d][-\u{2010}\u{2011}])([a-zа-яё]+){NBSP}({o})", *CONTENT_START_GROUP),
            "giu",
        ),
        m2: Re::configured(
            &format!("({c}){NBSP}([a-zа-яё]+)(?![-\u{2010}\u{2011}{RAQUO}{LDQUO}][\\p{{L}}\\d])($|[^a-zа-яё])"),
            "giu",
        ),
        open,
        close: close.to_vec(),
    }
}

static COMPILE_CACHE: Mutex<Vec<(JsString, Arc<NobrPatterns>)>> = Mutex::new(Vec::new());
const COMPILE_CACHE_CAP: usize = 8;

fn compile_nobr_patterns(ctx: &mut Ctx<'_>) -> Result<Option<Arc<NobrPatterns>>> {
    let sample = nowrap(ctx, &js::u("###"))?;
    let mut cache = COMPILE_CACHE.lock().unwrap_or_else(std::sync::PoisonError::into_inner);

    if let Some((_, cached)) = cache.iter().find(|(key, _)| *key == sample) {
        return Ok(Some(cached.clone()));
    }

    let Some(idx) = js::index_of(&sample, &js::u("###"), 0) else { return Ok(None) };
    let open = js::slice(&sample, 0, idx).to_vec();
    let close = js::from(&sample, idx + 3);

    if open.is_empty() && close.is_empty() {
        return Ok(None);
    }

    let compiled = Arc::new(nobr_patterns(open, close));

    if cache.len() >= COMPILE_CACHE_CAP {
        cache.remove(0);
    }

    cache.push((sample, compiled.clone()));

    Ok(Some(compiled))
}

fn split_number(s: &[u16]) -> JsString {
    if s.len() <= 3 {
        return s.to_vec();
    }

    let head = s.len() % 3;
    let mut parts: Vec<&[u16]> = if head > 0 { vec![&s[..head]] } else { Vec::new() };
    let mut i = head;

    while i < s.len() {
        parts.push(js::slice(s, i, i + 3));
        i += 3;
    }

    js::join(&parts, &[0x20])
}

static NUMERO_MARK: LazyLock<String> = LazyLock::new(|| format!("(?:{NUMERO}|{})", vaulted_reference(NUMERO)));
static WRITTEN_GAP: LazyLock<String> =
    LazyLock::new(|| format!(r"(?:\s|{}|{})", vaulted_reference(NBSP), vaulted_reference(NNBSP)));
static ADDRESS_GAP: LazyLock<String> = LazyLock::new(|| {
    format!(
        r"(?:[^\S\r\n]|{}|{}){{0,2}}(?:(?:<{OPEN}L\d{{1,9}}{CLOSE}>)?\r?\n)?",
        vaulted_reference(NBSP),
        vaulted_reference(NNBSP)
    )
});
const IDENTIFIER_LABEL: &str =
    r"(?<![\p{L}\d])(?:ИНН|КПП|ОГРНИП|ОГРН|БИК|СНИЛС|ОКПО|[Сс]ч[её]т|[Рр]\/с|[Кк]\/с|[Ии]ндекс|[Кк]од|[Аа]рт\.)";
static LABEL_SPLIT_BY_WRAPPER_RE: LazyLock<Re> = LazyLock::new(|| {
    Re::new(
        &format!(r"(?<![\p{{L}}\d])(?:[РрКк]{end}\/с|[Аа]рт{end}\.):?{}{{0,2}}$", *WRITTEN_GAP, end = *INLINE_END),
        "u",
    )
});
static DOTTED_SEGMENTS_AHEAD_RE: LazyLock<Re> = LazyLock::new(|| Re::new(r"\d+\.\d", "y"));

fn drop_nested_nowrap(ctx: &mut Ctx<'_>) -> Result<Option<JsString>> {
    let Some(c) = compile_nobr_patterns(ctx)? else { return Ok(None) };

    if !js::includes(&ctx.text, &c.open) {
        return Ok(None);
    }

    let text = std::mem::take(&mut ctx.text);
    let mut drops = nowrap_contexts(ctx, &text, &c.open);

    if drops.is_empty() {
        return Ok(Some(text));
    }

    drops.sort_by_key(|s| s.start);

    let mut out = Vec::with_capacity(text.len());
    let mut cursor = 0;

    for span in drops {
        out.extend_from_slice(js::slice(&text, cursor, span.start));
        cursor = span.end;
    }

    out.extend_from_slice(js::from(&text, cursor));

    Ok(Some(out))
}

static NBSP_GAP_RE: LazyLock<Re> = LazyLock::new(|| Re::new(r"[ \t]*\u{00A0}[ \t\u{00A0}]*", "gu"));
static WORD_CONTINUATION: LazyLock<Re> = LazyLock::new(|| Re::new(r"[\p{L}\d]+", "uy"));
static WORD_CHARACTER: LazyLock<Re> = LazyLock::new(|| Re::new(r"[\p{L}\d]", "uy"));
static WORD_BEFORE: LazyLock<Re> = LazyLock::new(|| Re::new(r"(?<=([\p{L}\d]+))", "uy"));
static WORD_CHARACTER_BEFORE: LazyLock<Re> = LazyLock::new(|| Re::new(r"(?<=[\p{L}\d])", "uy"));
static CAPITAL_LETTER_RE: LazyLock<Re> = LazyLock::new(|| Re::new("^[А-ЯЁ]$", "u"));

fn is_hyphen(c: Option<&u16>) -> bool {
    matches!(c, Some(0x2D | 0x2010 | 0x2011))
}

fn inline_compound_continues(ctx: &mut Ctx<'_>, text: &[u16], at: usize) -> Result<bool> {
    if text.get(at) != Some(&0x3C) && !(is_hyphen(text.get(at)) && text.get(at + 1) == Some(&0x3C)) {
        return Ok(false);
    }
    let mut next = inline_text_start(ctx.names(), text, at);
    while let Some(m) = WORD_CONTINUATION.exec_at(text, next) {
        next = inline_text_start(ctx.names(), text, m.end());
        (ctx.check_budget)("nowrap:content")?;
    }
    if !is_hyphen(text.get(next)) {
        return Ok(false);
    }
    let after = inline_text_start(ctx.names(), text, next + 1);
    Ok(WORD_CHARACTER.exec_at(text, after).is_some())
}

fn inline_compound_precedes(ctx: &mut Ctx<'_>, text: &[u16], at: usize) -> Result<bool> {
    let before = |k: usize| at.checked_sub(k).and_then(|i| text.get(i));
    if before(1) != Some(&0x3E) && !(is_hyphen(before(1)) && before(2) == Some(&0x3E)) {
        return Ok(false);
    }
    let mut end = inline_text_end(ctx.names(), text, at);
    while let Some(part) = WORD_BEFORE.exec_at(text, end) {
        end = inline_text_end(ctx.names(), text, end - part.s(1).len());
        (ctx.check_budget)("nowrap:content")?;
    }
    if !is_hyphen(end.checked_sub(1).and_then(|i| text.get(i))) {
        return Ok(false);
    }
    let start = inline_text_end(ctx.names(), text, end - 1);
    Ok(WORD_CHARACTER_BEFORE.exec_at(text, start).is_some())
}

fn remove_nbsp_in_nobr(ctx: &mut Ctx<'_>) -> Result<Option<JsString>> {
    let Some(c) = compile_nobr_patterns(ctx)? else { return Ok(None) };
    let hook = ctx.on_cycle_limit.clone();
    let mut on_limit = |n: usize| {
        if let Some(h) = &hook {
            h(&js::u("other.strip_nbsp_in_nowrap"), n);
        }
    };
    let (collapses, trims, dots) = (ctx.bound.space_collapse, ctx.bound.trims_line_end, ctx.bound.initials_dots);
    let check_budget = ctx.check_budget;
    let source = std::mem::take(&mut ctx.text);
    let text = cycle(
        &source,
        |t| {
            c.m1.try_replace_with(t, |m| {
                let lead = m.s(1);
                let start = m.index() + m.whole().len();
                if (dots && starts_with_compact_name(t, start))
                    || inline_compound_precedes(ctx, t, m.index() + lead.len())?
                {
                    return Ok(m.whole().to_vec());
                }
                let taken = match t.get(start) {
                    None => trims,
                    Some(&next) => collapses && (next == 0x20 || next == 0x09),
                };
                let space: &[u16] = if taken { &[] } else { &[0x20] };
                Ok(cat!(lead, m.s(3), m.s(2), space))
            })
        },
        100,
        &mut on_limit,
        check_budget,
    )?;
    ctx.text = source;
    let text = if js::includes(&text, &c.close) {
        let tokens = wrapper_tokens(ctx, &c);
        cycle(
            &text,
            |t| {
                let mut stack = Vec::new();
                let mut closes = HashSet::new();
                for m in ANY_TAG_RE.find_all(t) {
                    if !tokens.contains(m.s(2)) {
                        continue;
                    }
                    if m.s(1) != [0x2F] {
                        stack.push(m.whole() == c.open);
                    } else if stack.pop() == Some(true) {
                        closes.insert(m.index());
                    }
                }
                c.m2.try_replace_with(t, |m| {
                    let (word, after) = (m.s(2), m.s(3));
                    let end = m.index() + m.whole().len();
                    if !closes.contains(&m.index())
                        || inline_compound_continues(ctx, t, end - after.len())?
                        || (after == [0x2E]
                            && CAPITAL_LETTER_RE.test(word)
                            && has_dotted_location_after(t, end - 1)
                            && follows_complete_name(&mut ctx.reference_names(), t, m.index()))
                    {
                        return Ok(m.whole().to_vec());
                    }
                    Ok(cat!(" ", word, m.s(1), after))
                })
            },
            100,
            &mut on_limit,
            check_budget,
        )?
    } else {
        text
    };

    Ok(Some(edit_wrappers(ctx, &text, &c, |m| {
        if collapses { NBSP_GAP_RE.replace_tpl(m, &[0x20]) } else { js::replace_all_lit(m, &[unit(NBSP)], &[0x20]) }
    })))
}

fn wrapper_tokens(ctx: &mut Ctx<'_>, c: &NobrPatterns) -> HashSet<JsString> {
    let spans = span_token_set(ctx);
    if spans.contains(&c.close[2..c.close.len() - 1]) {
        return spans;
    }
    let name = js::u("nobr");
    ctx.find_by_tag_name(&NameMatcher::Name(&name))
        .into_iter()
        .map(|(prefix, id)| cat!(OPEN, prefix, js::num(id), CLOSE))
        .collect()
}

fn edit_wrappers(ctx: &mut Ctx<'_>, text: &[u16], c: &NobrPatterns, edit: impl Fn(&[u16]) -> JsString) -> JsString {
    if !js::includes(text, &c.open) {
        return text.to_vec();
    }
    let tokens = wrapper_tokens(ctx, c);
    let mut out = Vec::with_capacity(text.len());
    let mut stack = Vec::new();
    let (mut depth, mut start, mut cursor) = (0_usize, 0, 0);
    for m in ANY_TAG_RE.find_all(text) {
        if !tokens.contains(m.s(2)) {
            continue;
        }
        if m.s(1) != [0x2F] {
            let wrapper = m.whole() == c.open;
            stack.push(wrapper);
            if wrapper {
                if depth == 0 {
                    start = m.index();
                }
                depth += 1;
            }
        } else if stack.pop() == Some(true) {
            depth -= 1;
            if depth == 0 {
                out.extend_from_slice(&text[cursor..start]);
                out.extend(edit(&text[start..m.end()]));
                cursor = m.end();
            }
        }
    }
    out.extend_from_slice(&text[cursor..]);
    out
}

fn nobr_to_nbsp(ctx: &mut Ctx<'_>) -> Result<Option<JsString>> {
    let Some(c) = compile_nobr_patterns(ctx)? else { return Ok(None) };
    let source = std::mem::take(&mut ctx.text);
    let text = edit_wrappers(ctx, &source, &c, |m| js::replace_all_lit(m, &[0x20], &[unit(NBSP)]));
    ctx.text = source;
    Ok(Some(text))
}

pub fn etc_group() -> RuleGroupDef {
    LazyLock::force(&LABEL_SPLIT_BY_WRAPPER_RE);
    LazyLock::force(&DOTTED_SEGMENTS_AHEAD_RE);
    LazyLock::force(&NBSP_GAP_RE);
    super::group(
        "Прочее",
        super::classes(super::NOWRAP_CLASSES),
        vec![
            Rule::pattern(
                "acute_accent",
                Re::new(r"([уеыаоэяиюёУЕЫАОЭЯИЮЁ])`(\p{L})", "gu"),
                Replacement::Template(cat!("$1", COMBINING_ACUTE, "$2")),
            )
            .describe("Ударение на гласной").requires(F::BACKTICK),
            Rule::pattern(
                "sup_word",
                Re::new(r"(?<!\s)(\s+|^)\^([a-zа-яё0-9.:,-]+)(\s|$|\.$)", "giu"),
                Replacement::func(|m, ctx| {
                    let small = ctx.tag(m.s(2), "small", &[])?;
                    Ok(cat!(ctx.tag(&small, "sup", &[])?, m.s(3)))
                }),
            )
            .describe("Надстрочный текст после ^")
            .html_only().requires(F::CARET),
            Rule::pattern(
                "roman_range_dash",
                Re::new(r"(^|[\s(])([XIV]{1,5})[-−]([XIV]{1,5})(?=[\s).,;:!?]|$)", "gu"),
                Replacement::Template(cat!("$1$2", NDASH, "$3")),
            )
            .describe("Среднее тире в диапазоне римских чисел").requires(F::DASH | F::LATIN),
            Rule::pattern(
                "en_century_range",
                Re::new(&format!(r"([\s(]|^){}{}", *CENTURY_RANGE, *CENTURY_MARKER), "gu"),
                Replacement::func(|m, ctx| {
                    let body = cat!(m.s(2), NDASH, m.s(4), " вв.");
                    let start = m.index() + m.s(1).len();
                    Ok(cat!(m.s(1), wrap_nowrap(ctx, &body, m.input, start, m.end())?))
                }),
            )
            .describe("Тире между диапазоном веков").requires(F::DASH | F::LATIN),
            Rule::pattern(
                "en_time_range",
                Re::new(r"(?<![\d>])(\d{1,2}:\d{2})([-—−–])(\d{1,2}:\d{2})([^\d<]|$)", "gu"),
                Replacement::func(|m, ctx| {
                    let inner = cat!(m.s(1), NDASH, m.s(3));
                    let end = m.end() - m.s(4).len();
                    Ok(cat!(wrap_nowrap(ctx, &inner, m.input, m.index(), end)?, m.s(4)))
                }),
            )
            .describe("Тире и неразрывный пробел в диапазоне времени").requires(F::DIGIT | F::DASH | F::COLON),
            Rule::pattern(
                "split_triads",
                Re::new(
                    &format!(
                        r"(?=[1-9][0-9]{{4}})(?<![\p{{L}}\p{{N}}<)])(?<![A-Za-z]\.)(?<!^\.)(?<![^\p{{L}}]\.)(?<!\d-)(?<!\d[ \t]+)(?<!\d[.,])(?<!&#)(?<!{numero}{gap}{{0,2}})(?<!{IDENTIFIER_LABEL}:?(?:{inline_end}){{0,4}}{gap}{{0,2}})(?![0-9]{{6}}(?:,{address}|{city}{{0,2}}г\.{address})(?:<{OPEN}[{TRANSPARENT_TAG_KINDS}]\d{{1,9}}{CLOSE}>){{0,4}}\p{{Lu}})([0-9]{{5,15}})(?!\p{{N}})(?![ \t]+[0-9]{{3}}(?![0-9]))([^a-zA-Z>(]|$)",
                        numero = *NUMERO_MARK,
                        gap = *WRITTEN_GAP,
                        city = *LINE_GAP,
                        inline_end = *INLINE_END,
                        address = *ADDRESS_GAP,
                    ),
                    "gu",
                ),
                Replacement::func(|m, ctx| {
                    let (source, offset) = (m.input, m.index());
                    let p2 = m.s(2);
                    if p2 == [0x2D]
                        || (p2 == [0x2E] && DOTTED_SEGMENTS_AHEAD_RE.exec_at(source, m.end()).is_some())
                        || LABEL_SPLIT_BY_WRAPPER_RE.test(&source[offset.saturating_sub(64)..offset])
                        || joined_numeric_token(ctx, source, offset)
                        || phone_context_at(source, offset) == 1
                    {
                        return Ok(m.whole().to_vec());
                    }
                    Ok(cat!(js::replace_all_lit(&split_number(m.s(1)), &[0x20], &[unit(NNBSP)]), p2))
                }),
            )
            .describe("Триады большого числа через узкий неразрывный пробел").requires(F::DIGIT).requires_pattern(Re::new("[1-9][0-9]{4}", "u")),
            Rule::handler("drop_nested_nowrap", drop_nested_nowrap)
                .describe("Удаление nowrap-обёртки, вложенной в другую nowrap-обёртку").requires(F::MARKUP),
            Rule::handler("strip_nbsp_in_nowrap", remove_nbsp_in_nobr)
                .describe("Удаление неразрывных пробелов в nowrap-конструкциях").requires(F::MARKUP),
            Rule::handler("nbsp_in_nowrap", nobr_to_nbsp)
                .describe("Преобразование nowrap-конструкций в неразрывные пробелы")
                .off().requires(F::MARKUP),
        ],
    )
}
