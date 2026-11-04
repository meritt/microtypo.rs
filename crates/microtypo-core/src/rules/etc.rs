use std::collections::HashSet;
use std::sync::{Arc, LazyLock, Mutex};

use crate::cat;
use crate::errors::Result;
use crate::js::{self, JsString, Re};
use crate::protect::placeholders::{CLOSE, OPEN, TAG_PLACEHOLDER_SOURCE};
use crate::protect::tags::NameMatcher;
use crate::rule_runner::{Ctx, Replacement, Rule, RuleGroupDef};
use crate::shared::glyphs::{COMBINING_ACUTE, NBSP, NDASH, NNBSP, unit};
use crate::shared::strings::cycle;

use super::nowrap;

struct NobrPatterns {
    open: JsString,
    m1: Re,
    m2: Re,
    inner: Re,
}

fn nobr_patterns(open: JsString, close: &[u16]) -> NobrPatterns {
    let o = js::escape(&open);
    let c = js::escape(close);

    NobrPatterns {
        m1: Re::new(&format!("(^|[^a-zа-яё])([a-zа-яё]+){NBSP}({o})"), "giu"),
        m2: Re::new(&format!("({c}){NBSP}([a-zа-яё]+)($|[^a-zа-яё])"), "giu"),
        inner: Re::new(&format!("{o}.*?{c}"), "giu"),
        open,
    }
}

static COMPILE_CACHE: Mutex<Vec<(JsString, Arc<NobrPatterns>)>> = Mutex::new(Vec::new());
const COMPILE_CACHE_CAP: usize = 8;

pub fn compile_cache_size() -> usize {
    COMPILE_CACHE.lock().map_or(0, |c| c.len())
}

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

static ANY_TAG_RE: LazyLock<Re> = LazyLock::new(|| Re::new(&TAG_PLACEHOLDER_SOURCE, "g"));

fn span_token_set(ctx: &mut Ctx<'_>) -> HashSet<JsString> {
    let name = js::u("span");
    ctx.find_by_tag_name(&NameMatcher::Name(&name))
        .into_iter()
        .map(|(prefix, id)| cat!(OPEN, prefix, js::num(id), CLOSE))
        .collect()
}

#[derive(Clone, Copy)]
struct Span {
    start: usize,
    end: usize,
    nowrap: bool,
    nested: bool,
}

fn drop_nested_nowrap(ctx: &mut Ctx<'_>) -> Result<Option<JsString>> {
    let Some(c) = compile_nobr_patterns(ctx)? else { return Ok(None) };

    if !js::includes(&ctx.text, &c.open) {
        return Ok(None);
    }

    let spans = span_token_set(ctx);

    if spans.is_empty() {
        return Ok(None);
    }

    let text = &ctx.text;
    let mut stack: Vec<Span> = Vec::new();
    let mut drops: Vec<Span> = Vec::new();

    for m in ANY_TAG_RE.find_all(text) {
        if !spans.contains(m.s(2)) {
            if let Some(enclosing) = stack.last_mut() {
                enclosing.nowrap = false;
            }
            continue;
        }

        let mut span = Span { start: m.index(), end: m.end(), nowrap: m.whole() == c.open.as_slice(), nested: false };

        if m.s(1) != [0x2F] {
            span.nested = span.nowrap && stack.last().is_some_and(|s| s.nowrap);
            stack.push(span);
            continue;
        }

        let Some(opened) = stack.pop() else { continue };

        if opened.nested {
            drops.push(opened);
            drops.push(span);
        }
    }

    if drops.is_empty() {
        return Ok(None);
    }

    drops.sort_by_key(|s| s.start);

    let mut out = Vec::with_capacity(text.len());
    let mut cursor = 0;

    for Span { start, end, .. } in drops {
        out.extend_from_slice(js::slice(text, cursor, start));
        cursor = end;
    }

    out.extend_from_slice(js::from(text, cursor));

    Ok(Some(out))
}

fn remove_nbsp_in_nobr(ctx: &mut Ctx<'_>) -> Result<Option<JsString>> {
    let Some(c) = compile_nobr_patterns(ctx)? else { return Ok(None) };
    let hook = ctx.on_cycle_limit.clone();
    let mut on_limit = |n: usize| {
        if let Some(h) = &hook {
            h("other.strip_nbsp_in_nowrap", n);
        }
    };
    let m1 = js::u("$1$3$2 ");
    let m2 = js::u(" $2$1$3");

    let text = cycle(&ctx.text, |t| Ok(c.m1.replace_tpl(t, &m1)), 100, &mut on_limit, ctx.check_budget)?;
    let text = cycle(&text, |t| Ok(c.m2.replace_tpl(t, &m2)), 100, &mut on_limit, ctx.check_budget)?;

    Ok(Some(c.inner.replace_with(&text, |m| js::replace_all_lit(m.whole(), &[unit(NBSP)], &[0x20]))))
}

fn nobr_to_nbsp(ctx: &mut Ctx<'_>) -> Result<Option<JsString>> {
    let Some(c) = compile_nobr_patterns(ctx)? else { return Ok(None) };
    Ok(Some(c.inner.replace_with(&ctx.text, |m| js::replace_all_lit(m.whole(), &[0x20], &[unit(NBSP)]))))
}

pub fn etc_group() -> RuleGroupDef {
    super::group(
        "Прочее",
        super::classes(super::NOWRAP_CLASSES),
        vec![
            Rule::pattern(
                "acute_accent",
                Re::new(r"([уеыаоэяиюёУЕЫАОЭЯИЮЁ])`(\p{L})", "gu"),
                Replacement::Template(cat!("$1", COMBINING_ACUTE, "$2")),
            )
            .describe("Ударение на гласной"),
            Rule::pattern(
                "sup_word",
                Re::new(r"(?<!\s)(\s+|^)\^([a-zа-яё0-9.:,-]+)(\s|$|\.$)", "giu"),
                Replacement::func(|m, ctx| {
                    let small = ctx.tag(m.s(2), "small", &[])?;
                    Ok(cat!(ctx.tag(&small, "sup", &[])?, m.s(3)))
                }),
            )
            .describe("Надстрочный текст после ^")
            .html_only(),
            Rule::pattern(
                "roman_range_dash",
                Re::new(r"(^|[\s(])([XIV]{1,5})[-−]([XIV]{1,5})(?=[\s).,;:!?]|$)", "gu"),
                Replacement::Template(cat!("$1$2", NDASH, "$3")),
            )
            .describe("Среднее тире в диапазоне римских чисел"),
            Rule::pattern(
                "en_century_range",
                Re::new(
                    r"([\s(]|^)([XIV]{1,5})([-—–])([XIV]{1,5})(( |\u{00A0})?(в\.в\.|вв\.|вв|в\.|в)(?![а-яёa-z]))",
                    "gu",
                ),
                Replacement::func(|m, ctx| {
                    let inner = cat!(m.s(2), NDASH, m.s(4), " вв.");
                    Ok(cat!(m.s(1), nowrap(ctx, &inner)?))
                }),
            )
            .describe("Тире между диапазоном веков"),
            Rule::pattern(
                "en_time_range",
                Re::new(r"(?<![\d>])(\d{1,2}:\d{2})([-—−–])(\d{1,2}:\d{2})([^\d<]|$)", "gu"),
                Replacement::func(|m, ctx| {
                    let inner = cat!(m.s(1), NDASH, m.s(3));
                    Ok(cat!(nowrap(ctx, &inner)?, m.s(4)))
                }),
            )
            .describe("Тире и неразрывный пробел в диапазоне времени"),
            Rule::pattern(
                "split_triads",
                Re::new(r"(?=[0-9]{5})(?<![\p{L}0-9<)])(?<!\d-)(?<!\d )(?<!&#)([0-9]{5,})([^a-zA-Z>(]|$)", "gu"),
                Replacement::func(|m, _| {
                    let p2 = m.s(2);
                    if p2 == [0x2D] {
                        return Ok(m.whole().to_vec());
                    }
                    Ok(cat!(js::replace_all_lit(&split_number(m.s(1)), &[0x20], &[unit(NNBSP)]), p2))
                }),
            )
            .describe("Триады большого числа через узкий неразрывный пробел"),
            Rule::handler("drop_nested_nowrap", drop_nested_nowrap)
                .describe("Удаление nowrap-обёртки, вложенной в другую nowrap-обёртку"),
            Rule::handler("strip_nbsp_in_nowrap", remove_nbsp_in_nobr)
                .describe("Удаление неразрывных пробелов в nowrap-конструкциях"),
            Rule::handler("nbsp_in_nowrap", nobr_to_nbsp)
                .describe("Преобразование nowrap-конструкций в неразрывные пробелы")
                .off(),
        ],
    )
}
