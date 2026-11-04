//! Quickly ruling out UTF-16 and UCS-2 input, where the byte start predicates do not apply.
//!
//! Both checks are necessary conditions read off the IR, so they only ever skip work: a start
//! position whose code unit cannot begin a match, or a whole search whose remaining input lacks
//! a character every match consumes. A code unit stands for itself and for the astral code
//! points it is the high surrogate of, which keeps both checks a superset in either mode.

use crate::codepointset::{CODE_POINT_MAX, CodePointSet, Interval};
use crate::ir::{self, Node};
#[cfg(not(feature = "std"))]
use alloc::{boxed::Box, vec::Vec};

/// What a compiled regex knows about where its matches may be, in code units.
#[derive(Debug, Clone, Default)]
pub struct Utf16Prefilter {
    /// The units a match may start at, if not any.
    pub start: Option<UnitSet>,
    /// The units that may follow the first one of a match away from the start of the input, if
    /// not any: a match there is at least two units long.
    pub second: Option<UnitSet>,
    /// Whether a match may also start at the start of the input, whatever the unit there.
    pub at_input_start: bool,
    /// Sets of which every match consumes at least one unit each, least common first.
    pub required: Box<[UnitSet]>,
}

/// The most sets `required` holds: each costs a scan of the input when it is checked.
pub const MAX_REQUIRED: usize = 3;

/// A set of UTF-16 code units.
#[derive(Debug, Clone)]
pub struct UnitSet {
    /// Units below `DENSE_UNITS`, one bit each.
    dense: [u64; DENSE_UNITS / 64],
    /// Sorted, disjoint, inclusive ranges of units at or above `DENSE_UNITS`.
    sparse: Box<[(u16, u16)]>,
}

/// Latin, Cyrillic and their neighbours: the bulk of what the sets test.
const DENSE_UNITS: usize = 0x800;

impl UnitSet {
    #[inline(always)]
    pub fn contains(&self, u: u16) -> bool {
        let i = u as usize;
        if i < DENSE_UNITS {
            return self.dense[i / 64] >> (i % 64) & 1 != 0;
        }
        let at = self.sparse.partition_point(|&(_, last)| last < u);
        at < self.sparse.len() && self.sparse[at].0 <= u
    }

    /// \return the units of the given code points, or None if that is every unit.
    fn from_code_points(cps: &CodePointSet) -> Option<Self> {
        let high = |cp: u32| 0xD800 + ((cp - 0x1_0000) >> 10);
        let mut units = CodePointSet::new();
        for iv in cps.intervals() {
            if iv.first <= 0xFFFF {
                units.add(Interval::new(iv.first, iv.last.min(0xFFFF)));
            }
            if iv.last >= 0x1_0000 {
                units.add(Interval::new(high(iv.first.max(0x1_0000)), high(iv.last)));
            }
        }
        if units.intervals() == [Interval::new(0, 0xFFFF)] {
            return None;
        }
        let mut dense = [0u64; DENSE_UNITS / 64];
        let mut sparse = Vec::new();
        for iv in units.intervals() {
            for u in iv.first..=iv.last.min(DENSE_UNITS as u32 - 1) {
                dense[u as usize / 64] |= 1 << (u % 64);
            }
            if iv.last >= DENSE_UNITS as u32 {
                sparse.push((iv.first.max(DENSE_UNITS as u32) as u16, iv.last as u16));
            }
        }
        Some(UnitSet {
            dense,
            sparse: sparse.into_boxed_slice(),
        })
    }
}

/// \return the code points a node matching exactly one character accepts, if it is one.
fn single_char_code_points(n: &Node) -> Option<CodePointSet> {
    let mut cps = CodePointSet::new();
    match n {
        Node::Char { c } => cps.add_one(*c),
        Node::CharSet(chars) => chars.iter().for_each(|&c| cps.add_one(c)),
        Node::Bracket(bc) if bc.invert => return Some(bc.cps.inverted()),
        Node::Bracket(bc) => return Some(bc.cps.clone()),
        Node::MatchAny => cps.add(Interval::new(0, CODE_POINT_MAX)),
        Node::MatchAnyExceptLineTerminator => {
            for c in [0x000A, 0x000D, 0x2028, 0x2029] {
                cps.add_one(c);
            }
            return Some(cps.inverted());
        }
        _ => return None,
    }
    Some(cps)
}

/// Add to \p out every code point that may begin a non-empty match of \p n, and set
/// \p at_input_start if some match can only begin at the start of the input.
/// \return whether \p n may match without consuming a character, in which case
/// whatever follows it may begin the match too.
fn add_first_code_points(n: &Node, out: &mut CodePointSet, at_input_start: &mut bool) -> bool {
    if let Some(cps) = single_char_code_points(n) {
        out.add_set(cps);
        return false;
    }
    match n {
        // A path through a single-line ^ matches nowhere but the start, which is always tried.
        Node::Anchor {
            anchor_type: ir::AnchorType::StartOfLine,
            multiline: false,
        } => {
            *at_input_start = true;
            false
        }
        Node::Empty
        | Node::Goal
        | Node::Anchor { .. }
        | Node::WordBoundary { .. }
        | Node::LookaroundAssertion { .. } => true,
        Node::CaptureGroup { contents, .. } => add_first_code_points(contents, out, at_input_start),
        Node::Cat(nodes) => nodes
            .iter()
            .all(|n| add_first_code_points(n, out, at_input_start)),
        Node::Alt(left, right) => {
            let left = add_first_code_points(left, out, at_input_start);
            add_first_code_points(right, out, at_input_start) || left
        }
        Node::Loop { loopee, quant, .. } | Node::Loop1CharBody { loopee, quant } => {
            add_first_code_points(loopee, out, at_input_start) || quant.min == 0
        }
        // Back-references may match the empty string or anything at all; string sets and
        // literal bytes are rare (and never emitted for UTF-16), so give up on them.
        _ => {
            out.add(Interval::new(0, CODE_POINT_MAX));
            true
        }
    }
}

/// How common the characters of a set are in prose, the kind of text searched most: each
/// lowercase letter weighs more than any set of rarer characters, and a space more than all
/// the letters together, since a set with many of them is found at once and rules out nothing.
fn commonness(cps: &CodePointSet) -> u64 {
    let overlap = |first: u32, last: u32| -> u64 {
        cps.intervals()
            .iter()
            .map(|iv| {
                u64::from(iv.last.min(last).saturating_sub(iv.first.max(first)) + 1)
                    * u64::from(iv.first <= last && first <= iv.last)
            })
            .sum()
    };
    let size: u64 = cps
        .intervals()
        .iter()
        .map(|iv| iv.count_codepoints() as u64)
        .sum();
    let letters = overlap(0x0061, 0x007A) + overlap(0x0430, 0x044F);
    size + (letters << LETTER_SHIFT) + (overlap(0x20, 0x20) << 32)
}

/// One lowercase letter's weight in `commonness`, above the size of any set.
const LETTER_SHIFT: u32 = 21;

/// The most letters a set beyond the rarest may hold and still be checked.
const MAX_REQUIRED_LETTERS: u64 = 4;

/// \return a set of which every match of \p n consumes at least one character, preferring the
/// least common; None if there is no such set.
fn required_code_points(n: &Node) -> Option<CodePointSet> {
    if let Some(cps) = single_char_code_points(n) {
        return Some(cps);
    }
    match n {
        Node::CaptureGroup { contents, .. } => required_code_points(contents),
        Node::Cat(nodes) => nodes
            .iter()
            .filter_map(required_code_points)
            .min_by_key(commonness),
        Node::Alt(left, right) => {
            let mut cps = required_code_points(left)?;
            cps.add_set(required_code_points(right)?);
            Some(cps)
        }
        Node::Loop { loopee, quant, .. } | Node::Loop1CharBody { loopee, quant }
            if quant.min > 0 =>
        {
            required_code_points(loopee)
        }
        // Lookarounds consume nothing of the match, and lookbehinds may read before its start.
        _ => None,
    }
}

/// Add to \p out sets of which every match of \p n consumes at least one character each: one
/// per part of a catenation that has one, where `required_code_points` keeps only the rarest.
fn add_required_code_points(n: &Node, out: &mut Vec<CodePointSet>) {
    match n {
        Node::CaptureGroup { contents, .. } => add_required_code_points(contents, out),
        Node::Cat(nodes) => nodes.iter().for_each(|n| add_required_code_points(n, out)),
        Node::Loop { loopee, quant, .. } | Node::Loop1CharBody { loopee, quant }
            if quant.min > 0 =>
        {
            add_required_code_points(loopee, out)
        }
        _ => out.extend(required_code_points(n)),
    }
}

/// \return the sets worth checking before a search: the rarest, and any others that rule
/// something out in prose.
fn required_unit_sets(n: &Node) -> Box<[UnitSet]> {
    let mut sets = Vec::new();
    add_required_code_points(n, &mut sets);
    let mut sets: Vec<(u64, CodePointSet)> =
        sets.into_iter().map(|s| (commonness(&s), s)).collect();
    sets.sort_by_key(|(cost, _)| *cost);
    let mut out: Vec<UnitSet> = Vec::new();
    for (cost, cps) in sets {
        if out.len() == MAX_REQUIRED
            || (!out.is_empty() && cost > MAX_REQUIRED_LETTERS << LETTER_SHIFT)
        {
            break;
        }
        // The same set twice rules out nothing more.
        if let Some(units) = UnitSet::from_code_points(&cps) {
            if !out
                .iter()
                .any(|kept| kept.dense == units.dense && kept.sparse == units.sparse)
            {
                out.push(units);
            }
        }
    }
    out.into_boxed_slice()
}

/// Add to \p out every code point the first character a match of \p n consumes may be.
/// \return whether \p n may match without consuming one. Unlike `add_first_code_points`,
/// every assertion passes through, since a position here is never only the start.
fn add_consumed_first(n: &Node, out: &mut CodePointSet) -> bool {
    if let Some(cps) = single_char_code_points(n) {
        out.add_set(cps);
        return false;
    }
    match n {
        Node::Empty
        | Node::Goal
        | Node::Anchor { .. }
        | Node::WordBoundary { .. }
        | Node::LookaroundAssertion { .. } => true,
        Node::CaptureGroup { contents, .. } => add_consumed_first(contents, out),
        Node::Cat(nodes) => nodes.iter().all(|n| add_consumed_first(n, out)),
        Node::Alt(left, right) => {
            let left = add_consumed_first(left, out);
            add_consumed_first(right, out) || left
        }
        Node::Loop { loopee, quant, .. } | Node::Loop1CharBody { loopee, quant } => {
            add_consumed_first(loopee, out) || quant.min == 0
        }
        _ => {
            out.add(Interval::new(0, CODE_POINT_MAX));
            true
        }
    }
}

/// Add to \p out every code point that may follow the first character a match of \p n consumes,
/// where \p after is what may follow \p n, or None if the match may end there, which bounds
/// nothing. Paths through a single-line ^ are left out, as in `add_first_code_points`: a match
/// on them starts at the start of the input, which is always tried.
/// \return whether \p n may match without consuming a character, or None if what follows its
/// first character cannot be bounded.
fn add_second_code_points(
    n: &Node,
    after: Option<&CodePointSet>,
    out: &mut CodePointSet,
) -> Option<bool> {
    if let Some(cps) = single_char_code_points(n) {
        out.add_set(after?.clone());
        // An astral character takes two units, and the second is its low surrogate.
        if cps.intervals().last().is_some_and(|iv| iv.last >= 0x1_0000) {
            out.add(Interval::new(0xDC00, 0xDFFF));
        }
        return Some(false);
    }
    match n {
        Node::Anchor {
            anchor_type: ir::AnchorType::StartOfLine,
            multiline: false,
        } => Some(false),
        Node::Empty
        | Node::Goal
        | Node::Anchor { .. }
        | Node::WordBoundary { .. }
        | Node::LookaroundAssertion { .. } => Some(true),
        Node::CaptureGroup { contents, .. } => add_second_code_points(contents, after, out),
        Node::Cat(nodes) => {
            // What may follow each node: what the rest may consume first, and past a rest that
            // may consume nothing, whatever follows the catenation.
            let mut follows = Vec::with_capacity(nodes.len());
            let mut next = after.cloned();
            for node in nodes.iter().rev() {
                follows.push(next.clone());
                let mut first = CodePointSet::new();
                next = if add_consumed_first(node, &mut first) {
                    next.map(|mut next| {
                        next.add_set(first);
                        next
                    })
                } else {
                    Some(first)
                };
            }
            for (node, follow) in nodes.iter().zip(follows.iter().rev()) {
                if !add_second_code_points(node, follow.as_ref(), out)? {
                    return Some(false);
                }
            }
            Some(true)
        }
        Node::Alt(left, right) => {
            let left = add_second_code_points(left, after, out)?;
            Some(add_second_code_points(right, after, out)? || left)
        }
        Node::Loop { loopee, quant, .. } | Node::Loop1CharBody { loopee, quant } => {
            // Another iteration may follow an iteration, and so may whatever follows the loop.
            let inner = if quant.max.is_none_or(|max| max > 1) {
                after.map(|after| {
                    let mut inner = after.clone();
                    add_consumed_first(loopee, &mut inner);
                    inner
                })
            } else {
                after.cloned()
            };
            Some(add_second_code_points(loopee, inner.as_ref(), out)? || quant.min == 0)
        }
        _ => None,
    }
}

fn disjoint(a: &CodePointSet, b: &CodePointSet) -> bool {
    let (a, b) = (a.intervals(), b.intervals());
    let (mut i, mut j) = (0, 0);
    while i < a.len() && j < b.len() {
        if a[i].last < b[j].first {
            i += 1;
        } else if b[j].last < a[i].first {
            j += 1;
        } else {
            return false;
        }
    }
    true
}

/// \return the greedy single-character loops of \p re that backtracking cannot help: what
/// follows each must first consume a character its body never matches, and every position
/// short of the longest run is followed by a character the body did match. Matching them
/// without backtracking only skips attempts that fail, so every match stays the same.
pub fn possessive_loops(re: &ir::Regex) -> Vec<*const Node> {
    let mut out = Vec::new();
    add_possessive_loops(&re.node, None, &mut out);
    out
}

/// \p follow is what the rest of the match may consume first, or None if it may end there.
fn add_possessive_loops(n: &Node, follow: Option<&CodePointSet>, out: &mut Vec<*const Node>) {
    match n {
        Node::Cat(nodes) => {
            let mut after = follow.cloned();
            for node in nodes.iter().rev() {
                add_possessive_loops(node, after.as_ref(), out);
                let mut first = CodePointSet::new();
                after = if add_consumed_first(node, &mut first) {
                    after.map(|mut after| {
                        after.add_set(first);
                        after
                    })
                } else {
                    Some(first)
                };
            }
        }
        Node::Alt(left, right) => {
            add_possessive_loops(left, follow, out);
            add_possessive_loops(right, follow, out);
        }
        Node::CaptureGroup { contents, .. } => add_possessive_loops(contents, follow, out),
        Node::Loop { loopee, .. } => {
            let follow = follow.map(|follow| {
                let mut next = follow.clone();
                add_consumed_first(loopee, &mut next);
                next
            });
            add_possessive_loops(loopee, follow.as_ref(), out);
        }
        Node::Loop1CharBody { loopee, quant } => {
            if let (true, Some(follow), Some(body)) =
                (quant.greedy, follow, single_char_code_points(loopee))
            {
                if disjoint(&body, follow) {
                    out.push(n as *const Node);
                }
            }
        }
        // A lookaround matches in its own direction and ends where its contents do.
        _ => {}
    }
}

/// \return the prefilter for \p re.
pub fn prefilter_for_re(re: &ir::Regex) -> Utf16Prefilter {
    let mut first = CodePointSet::new();
    let mut at_input_start = false;
    let nullable = add_first_code_points(&re.node, &mut first, &mut at_input_start);
    let mut second = CodePointSet::new();
    let second = if nullable || add_second_code_points(&re.node, None, &mut second) != Some(false) {
        None
    } else {
        UnitSet::from_code_points(&second)
    };
    Utf16Prefilter {
        start: if nullable {
            None
        } else {
            UnitSet::from_code_points(&first)
        },
        second,
        at_input_start,
        required: required_unit_sets(&re.node),
    }
}
