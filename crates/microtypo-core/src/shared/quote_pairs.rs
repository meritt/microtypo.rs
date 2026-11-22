use std::collections::HashMap;

use crate::errors::Result;

pub const RDQUO: char = '\u{201D}';

pub const QUOT_U: u16 = 0x22;
pub const LAQUO_U: u16 = 0xAB;
pub const RAQUO_U: u16 = 0xBB;
pub const BDQUO_U: u16 = 0x201E;
pub const LDQUO_U: u16 = 0x201C;
pub const RDQUO_U: u16 = 0x201D;

pub fn matching_quote(glyph: u16) -> Option<u16> {
    match glyph {
        LAQUO_U => Some(RAQUO_U),
        BDQUO_U => Some(LDQUO_U),
        LDQUO_U => Some(RDQUO_U),
        _ => None,
    }
}

pub type Visit<'a> = Option<&'a dyn Fn() -> Result<()>>;
pub type MayOpen<'a, T> = dyn FnMut(&[T], usize, bool) -> Result<bool> + 'a;
pub type OpensWithin<'a, T> = dyn FnMut(&[T], usize) -> Result<bool> + 'a;

fn visit(v: Visit<'_>) -> Result<()> {
    v.map_or(Ok(()), |f| f())
}

pub fn resolve_authored_quote_pairs(
    buf: &mut [u16],
    may_open: &mut MayOpen<'_, u16>,
    require_complete_pairs: bool,
    v: Visit<'_>,
) -> Result<()> {
    let mut open: Vec<u16> = Vec::new();
    let mut taken: Vec<Option<usize>> = Vec::new();
    let mut reopened: Option<HashMap<usize, usize>> = require_complete_pairs.then(HashMap::new);
    let taken_at = |taken: &Vec<Option<usize>>, depth: usize| taken.get(depth).copied().flatten();
    let shut = |glyph: u16,
                open: &mut Vec<u16>,
                taken: &mut Vec<Option<usize>>,
                reopened: &mut Option<HashMap<usize, usize>>| {
        if require_complete_pairs && open.last() != Some(&glyph) {
            return;
        }
        if let Some(r) = reopened.as_mut() {
            r.remove(&open.len());
        }
        open.pop();
        taken.truncate(open.len() + 1);
    };
    let set_taken = |taken: &mut Vec<Option<usize>>, depth: usize, value: Option<usize>| {
        if taken.len() <= depth {
            taken.resize(depth + 1, None);
        }
        taken[depth] = value;
    };
    for i in 0..buf.len() {
        visit(v)?;
        let ch = buf[i];
        if ch == LAQUO_U {
            open.push(RAQUO_U);
        } else if ch == RDQUO_U
            && taken_at(&taken, open.len()).is_some()
            && !(require_complete_pairs && open.last() == Some(&RDQUO_U))
        {
            let depth = open.len();
            let high = taken_at(&taken, depth).unwrap_or_default();
            buf[high] = QUOT_U;
            set_taken(&mut taken, depth, None);
            open.push(LDQUO_U);
            if let Some(r) = reopened.as_mut() {
                r.insert(open.len(), high);
            }
            buf[i] = RAQUO_U;
        } else if ch == RAQUO_U || ch == RDQUO_U {
            shut(ch, &mut open, &mut taken, &mut reopened);
            buf[i] = RAQUO_U;
        } else if ch == BDQUO_U {
            open.push(LDQUO_U);
            buf[i] = LAQUO_U;
        } else if ch == LDQUO_U {
            if open.last() == Some(&LDQUO_U) {
                shut(ch, &mut open, &mut taken, &mut reopened);
                buf[i] = QUOT_U;
                if taken_at(&taken, open.len()).is_none() && may_open(buf, i, true)? {
                    set_taken(&mut taken, open.len(), Some(i));
                }
                buf[i] = RAQUO_U;
            } else {
                buf[i] = QUOT_U;
                if may_open(buf, i, false)? {
                    open.push(RDQUO_U);
                } else {
                    shut(ch, &mut open, &mut taken, &mut reopened);
                }
            }
        }
    }
    if let Some(reopened) = reopened {
        let mut highs: Vec<(usize, usize)> = reopened.into_iter().collect();
        highs.sort_unstable();
        for (_, high) in highs {
            visit(v)?;
            buf[high] = RAQUO_U;
        }
    }
    Ok(())
}

#[derive(Debug, Clone, Default)]
pub struct PairSlot {
    pub glyph: u16,
    pub close_only: bool,
    pub closer: Option<u16>,
    pub partner: Option<usize>,
    pub outer: Option<usize>,
}

pub trait QuoteToken {
    fn slot(&self) -> &PairSlot;
    fn slot_mut(&mut self) -> &mut PairSlot;
}

impl QuoteToken for PairSlot {
    fn slot(&self) -> &PairSlot {
        self
    }
    fn slot_mut(&mut self) -> &mut PairSlot {
        self
    }
}

pub fn pair_authored_quotes<T: QuoteToken>(
    tokens: &mut [T],
    may_open: &mut MayOpen<'_, T>,
    opens_within_pair: Option<&mut OpensWithin<'_, T>>,
    visible_roles: bool,
    v: Visit<'_>,
) -> Result<()> {
    let roles = if visible_roles {
        None
    } else {
        let mut roles = Vec::with_capacity(tokens.len());
        for token in tokens.iter() {
            visit(v)?;
            roles.push(token.slot().glyph);
        }
        let view: &[T] = tokens;
        resolve_authored_quote_pairs(&mut roles, &mut |_, i, after_low| may_open(view, i, after_low), true, v)?;
        Some(roles)
    };
    pair_quote_tokens(tokens, roles.as_deref(), may_open, opens_within_pair, v)
}

pub fn pair_quote_tokens<T: QuoteToken>(
    tokens: &mut [T],
    roles: Option<&[u16]>,
    may_open: &mut MayOpen<'_, T>,
    mut opens_within_pair: Option<&mut OpensWithin<'_, T>>,
    v: Visit<'_>,
) -> Result<()> {
    let mut open: Vec<usize> = Vec::new();
    for i in 0..tokens.len() {
        visit(v)?;
        let glyph = tokens[i].slot().glyph;
        let top = open.last().copied();
        let straight = glyph == QUOT_U;
        let contextual = straight || (roles.is_none() && glyph == LDQUO_U);
        let closes = if contextual {
            top.is_some_and(|t| tokens[t].slot().closer == Some(glyph))
                && !match opens_within_pair.as_mut() {
                    Some(f) => f(tokens, i)?,
                    None => false,
                }
        } else {
            roles.and_then(|r| r.get(i)) == Some(&RAQUO_U)
        };
        let mut closer = matching_quote(glyph);
        if closes || tokens[i].slot().close_only {
            closer = None;
        } else if straight && may_open(tokens, i, false)? {
            closer = Some(glyph);
        }
        let slot = tokens[i].slot_mut();
        slot.closer = closer;
        slot.partner = None;
        slot.outer = if closer.is_none() { None } else { top };
        if closer.is_some() {
            open.push(i);
        } else if let Some(t) = top
            && tokens[t].slot().closer == Some(glyph)
        {
            tokens[i].slot_mut().partner = Some(t);
            tokens[t].slot_mut().partner = Some(i);
            open.pop();
        }
    }
    Ok(())
}
