//! Quickly ruling out UTF-16 and UCS-2 input, where the byte start predicates do not apply.
//!
//! Every check is a necessary condition read off the IR, so it only ever skips work: a start
//! position whose code units, or the one before, rule out a match, or a whole search whose
//! remaining input lacks a character every match consumes. A code unit stands for itself and
//! for the astral code points it is the high surrogate of, which keeps the checks a superset in
//! either mode.

use crate::codepointset::{CODE_POINT_MAX, CodePointSet, Interval};
use crate::ir::{self, Node};
#[cfg(not(feature = "std"))]
use alloc::{boxed::Box, sync::Arc, vec::Vec};
#[cfg(feature = "std")]
use std::sync::Arc;

/// What a compiled regex knows about where its matches may be, in code units.
#[derive(Debug, Clone)]
pub struct Utf16Prefilter {
    /// Whether a start needs a unit of the start set, a next unit of the second set, a previous
    /// unit outside the set before, or a run that ends right.
    filters_start: bool,
    /// Whether the next unit of a start away from the start of the input must be in the second
    /// set: a match there is at least two units long.
    second: bool,
    /// Whether the second set is the less common, so a block is tested against its cover
    /// first and against that of the start set only where the first test finds units.
    second_first: bool,
    /// Whether the unit before a start must be outside the set before.
    before: bool,
    /// Whether the covers of the start and second sets hold no other unit and there is no set
    /// before, so the covers alone decide a start.
    exact: bool,
    /// Whether every match starts with a lookahead for a run of one set of characters and then
    /// a character of another, so the first unit at or after a start that cannot continue the
    /// run must be one that may end it.
    run: bool,
    /// Whether a match may also start at the start of the input, whatever the unit there.
    pub at_input_start: bool,
    /// Whether a match may be empty, and so may also start at the end of the input.
    at_input_end: bool,
    /// How many sets the table holds of which every match consumes at least one unit each,
    /// least common first.
    required: usize,
    /// The sets each unit is in: `START` for the units a match may start at, `SECOND` for those
    /// that may follow the first one, both also when the set is any unit, `REQUIRED << i` for
    /// the required set `i`, `BEFORE` for the units no match starts right after, `STOP` for the
    /// units that cannot continue the run and `RUN_END` for those that may end it.
    table: UnitTable,
    /// Covers of the start set, the second set and the required sets, in that order.
    covers: [Cover; 2 + MAX_REQUIRED],
    /// Strings of units one of which every match contains, if rare enough to scan for.
    pub(crate) factors: Option<crate::utf16factors::Factors>,
}

/// The most sets `required` holds: each costs a scan of the input when it is checked.
pub const MAX_REQUIRED: usize = 3;

const START: u8 = 1;
const SECOND: u8 = 2;
const REQUIRED: u8 = 4;
const BEFORE: u8 = REQUIRED << MAX_REQUIRED;
const STOP: u8 = BEFORE << 1;
const RUN_END: u8 = STOP << 1;

/// A set of UTF-16 code units: sorted, disjoint, inclusive ranges. Shared, since the factor
/// analysis copies sets far more often than it makes them.
#[derive(Debug, Clone, PartialEq)]
pub struct UnitSet(pub(crate) Arc<[(u16, u16)]>);

/// Bits for every code unit, two levels deep: a page for its high byte, then its low byte.
#[derive(Debug, Clone)]
pub(crate) struct UnitTable {
    pages: [u8; 256],
    /// The pages, 256 bits each.
    bits: Box<[u8]>,
}

/// \return the ranges of \p set, a None set holding every unit.
fn unit_ranges(set: Option<&UnitSet>) -> &[(u16, u16)] {
    match set {
        Some(set) => &set.0,
        None => &[(0, 0xFFFF)],
    }
}

/// The most pages whose units differ that a table holds exactly; any further page claims every
/// bit but `BEFORE` for all of its units, which keeps each check a necessary condition.
const MAX_MIXED_PAGES: usize = 32;

impl UnitTable {
    /// \return the table of the bits each of \p sets gives its units, a None set every unit.
    /// Each page is read off the ranges that reach it, and only a page some set covers in part
    /// is filled unit by unit, so building does not cost a pass over every unit.
    fn new(sets: &[(Option<&UnitSet>, u8)]) -> Self {
        let every = sets.iter().fold(0, |every, &(_, bit)| every | bit) & !BEFORE;
        Self::with_every(sets, every)
    }

    /// \return the table of the bits each of \p sets gives its units, where a page past
    /// `MAX_MIXED_PAGES` gives each of its units \p every.
    pub(crate) fn with_every(sets: &[(Option<&UnitSet>, u8)], every: u8) -> Self {
        let mut table = UnitTable {
            pages: [0; 256],
            bits: Box::default(),
        };
        let mut bits = Vec::new();
        let mut uniform = [None; 256];
        let mut mixed = 0;
        let mut next = vec![0; sets.len()];
        // Every page past the last unit of every set gives its units no bit, as the first does.
        let end = sets
            .iter()
            .filter_map(|&(set, _)| unit_ranges(set).last())
            .map(|&(_, end)| usize::from(end))
            .max()
            .unwrap_or(0);
        for (high, page) in table.pages.iter_mut().enumerate() {
            let (first, last) = (high << 8, high << 8 | 0xFF);
            let scanned = if first <= end { sets } else { &[] };
            let mut whole = 0;
            let mut part = 0;
            for (&(set, bit), next) in scanned.iter().zip(&mut next) {
                let ranges = unit_ranges(set);
                while ranges
                    .get(*next)
                    .is_some_and(|&(_, end)| usize::from(end) < first)
                {
                    *next += 1;
                }
                match ranges.get(*next) {
                    Some(&(start, end))
                        if usize::from(start) <= first && usize::from(end) >= last =>
                    {
                        whole |= bit;
                    }
                    Some(&(start, _)) if usize::from(start) <= last => part |= bit,
                    _ => {}
                }
            }
            let same = if part == 0 {
                Some(whole)
            } else if mixed == MAX_MIXED_PAGES {
                Some(every)
            } else {
                None
            };
            *page = match same {
                Some(b) => *uniform[usize::from(b)].get_or_insert_with(|| {
                    bits.extend_from_slice(&[b; 256]);
                    (bits.len() / 256 - 1) as u8
                }),
                None => {
                    mixed += 1;
                    let mut units = [whole; 256];
                    for (&(set, bit), &next) in sets.iter().zip(&next) {
                        if part & bit == 0 {
                            continue;
                        }
                        for &(start, end) in unit_ranges(set)[next..]
                            .iter()
                            .take_while(|&&(start, _)| usize::from(start) <= last)
                        {
                            let start = usize::from(start).max(first) - first;
                            let end = usize::from(end).min(last) - first;
                            for unit in &mut units[start..=end] {
                                *unit |= bit;
                            }
                        }
                    }
                    bits.extend_from_slice(&units);
                    (bits.len() / 256 - 1) as u8
                }
            };
        }
        table.bits = bits.into_boxed_slice();
        table
    }

    #[inline(always)]
    pub(crate) fn get(&self, u: u16) -> u8 {
        let page = usize::from(self.pages[usize::from(u >> 8)]);
        self.bits[page << 8 | usize::from(u & 0xFF)]
    }
}

/// The units a scan tests before it looks for a hit among them, one bit each.
pub(crate) const BLOCK: usize = 64;

/// The most ranges a cover holds.
const COVER_RANGES: usize = 4;

/// A few ranges of units, as their first unit and how many follow it, that hold every unit of a
/// set and, where the set needs more ranges, the rarest units between them. Testing a block of
/// units against it compiles to vector compares, so a search tests the exact set only where the
/// cover holds a unit.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Cover {
    ranges: [(u16, u16); COVER_RANGES],
    len: usize,
    /// Whether the cover holds no unit outside its set.
    exact: bool,
}

impl Cover {
    /// \return the cover of \p set, a None set holding every unit: its ranges are joined across
    /// the least common gaps.
    pub(crate) fn new(set: Option<&UnitSet>) -> Self {
        let ranges = unit_ranges(set);
        let mut cover = Cover {
            ranges: [(0, 0); COVER_RANGES],
            len: 0,
            exact: ranges.len() <= COVER_RANGES,
        };
        let Some(&(mut first, _)) = ranges.first() else {
            return cover;
        };
        let mut gaps: Vec<(u64, usize)> = ranges
            .windows(2)
            .enumerate()
            .map(|(i, pair)| {
                let mut gap = CodePointSet::new();
                if pair[0].1 + 1 < pair[1].0 {
                    gap.add(Interval::new(
                        u32::from(pair[0].1) + 1,
                        u32::from(pair[1].0) - 1,
                    ));
                }
                (commonness(&gap), i)
            })
            .collect();
        gaps.sort_unstable_by(|a, b| b.cmp(a));
        let mut kept: Vec<usize> = gaps
            .iter()
            .take(COVER_RANGES - 1)
            .map(|&(_, i)| i)
            .collect();
        kept.sort_unstable();
        for &gap in kept.iter().chain([&(ranges.len() - 1)]) {
            cover.ranges[cover.len] = (first, ranges[gap].1 - first);
            cover.len += 1;
            if let Some(next) = ranges.get(gap + 1) {
                first = next.0;
            }
        }
        cover
    }

    /// \return one bit for each unit of \p block the cover holds, the first unit in the lowest
    /// bit. The ranges are tested one after another rather than in a loop, which the compiler
    /// would vectorize across them instead of across the units.
    #[inline(always)]
    pub(crate) fn bits(&self, block: &[u16; BLOCK]) -> u64 {
        let [r0, r1, r2, r3] = self.ranges;
        let mut bits = 0;
        if self.len > 0 {
            bits |= range_bits(block, r0);
            if self.len > 1 {
                bits |= range_bits(block, r1);
                if self.len > 2 {
                    bits |= range_bits(block, r2);
                    if self.len > 3 {
                        bits |= range_bits(block, r3);
                    }
                }
            }
        }
        bits
    }
}

/// \return one bit for each unit of \p block in the range of \p first and the \p span units
/// after it, the first unit in the lowest bit. Each run of 16 units compiles to a few vector
/// instructions.
#[cfg(not(all(target_arch = "wasm32", target_feature = "simd128")))]
#[inline(always)]
fn range_bits(block: &[u16; BLOCK], (first, span): (u16, u16)) -> u64 {
    block
        .chunks_exact(16)
        .enumerate()
        .fold(0, |bits, (i, units)| {
            let held = units.iter().enumerate().fold(0u16, |held, (k, &u)| {
                held | u16::from(u.wrapping_sub(first) <= span) << k
            });
            bits | u64::from(held) << (16 * i)
        })
}

/// \return one bit for each unit of \p block in the range of \p first and the \p span units
/// after it, the first unit in the lowest bit. A size-optimized wasm build does not vectorize
/// the portable loop.
#[cfg(all(target_arch = "wasm32", target_feature = "simd128"))]
#[inline(always)]
fn range_bits(block: &[u16; BLOCK], (first, span): (u16, u16)) -> u64 {
    use core::arch::wasm32::{u16x8_bitmask, u16x8_le, u16x8_splat, u16x8_sub, v128_load};
    let (first, span) = (u16x8_splat(first), u16x8_splat(span));
    block
        .chunks_exact(8)
        .enumerate()
        .fold(0, |bits, (i, units)| {
            // SAFETY: `units` holds the 16 bytes read, and the load has no alignment requirement.
            let units = unsafe { v128_load(units.as_ptr().cast()) };
            bits | u64::from(u16x8_bitmask(u16x8_le(u16x8_sub(units, first), span))) << (8 * i)
        })
}

/// \return the block of \p input at \p at, if it holds that many units.
#[inline(always)]
pub(crate) fn block_at(input: &[u16], at: usize) -> Option<&[u16; BLOCK]> {
    input
        .get(at..at + BLOCK)
        .and_then(|block| <&[u16; BLOCK]>::try_from(block).ok())
}

impl Utf16Prefilter {
    /// \return whether some check rules out start positions.
    pub fn filters_start(&self) -> bool {
        self.filters_start
    }

    /// \return how many sets there are of which every match consumes a unit.
    pub fn required(&self) -> usize {
        self.required
    }

    /// \return whether a match may start at \p at of \p input: its unit is in the start set,
    /// the next one, given a second set, in that, and the one before, if any, not in the set
    /// before. The results combine without branches, which the candidates of a block would
    /// mispredict; whether there is a second set or a set before is the same for every one.
    #[inline(always)]
    fn allows(&self, input: &[u16], at: usize) -> bool {
        let mut allows = self.table.get(input[at]) & START != 0;
        if self.second {
            allows &= input
                .get(at + 1)
                .is_some_and(|&next| self.table.get(next) & SECOND != 0);
        }
        if self.before {
            allows &= at
                .checked_sub(1)
                .is_none_or(|before| self.table.get(input[before]) & BEFORE == 0);
        }
        allows
    }

    /// \return one bit for each of the \p len units of \p input at \p at a match may start at,
    /// the first unit in the lowest bit. Within a full block the covers rule out most units at
    /// once, the next units read one unit further on; the table decides the rest, unless the
    /// covers are exact.
    #[inline(always)]
    fn start_bits(&self, input: &[u16], at: usize, len: usize) -> u64 {
        let mut maybe = match block_at(input, at) {
            Some(units) if len == BLOCK => {
                let next = block_at(input, at + 1).filter(|_| self.second);
                let maybe = match next {
                    Some(next) if self.second_first => {
                        let maybe = self.covers[1].bits(next);
                        if maybe == 0 {
                            0
                        } else {
                            maybe & self.covers[0].bits(units)
                        }
                    }
                    Some(next) => {
                        let maybe = self.covers[0].bits(units);
                        if maybe == 0 {
                            0
                        } else {
                            maybe & self.covers[1].bits(next)
                        }
                    }
                    None => self.covers[0].bits(units),
                };
                if self.exact {
                    return maybe;
                }
                maybe
            }
            _ => u64::MAX >> (BLOCK - len),
        };
        let mut bits = 0;
        while maybe != 0 {
            let k = maybe.trailing_zeros() as usize;
            maybe &= maybe - 1;
            bits |= u64::from(self.allows(input, at + k)) << k;
        }
        bits
    }

    /// \return the first index at or after \p from and at or before \p limit where the sets
    /// allow a match to start and, given a run, the first unit that cannot continue it may end
    /// it. \p scan keeps the last block tested and the last run for the next search over the
    /// same input, which a common start set makes start inside them.
    #[inline(always)]
    pub fn find_start(
        &self,
        input: &[u16],
        from: usize,
        limit: usize,
        scan: &mut UnitScan,
    ) -> Option<usize> {
        let mut at = self
            .find_allowed(input, from, limit, scan)
            .filter(|&at| at <= limit)?;
        while self.run {
            match scan.run {
                Some((first, last, ends)) if first <= at && at <= last => {
                    if ends {
                        break;
                    }
                    at = self
                        .find_allowed(input, last + 1, limit, scan)
                        .filter(|&at| at <= limit)?;
                }
                _ => {
                    let last = at
                        + input[at..]
                            .iter()
                            .position(|&u| self.table.get(u) & STOP != 0)?;
                    scan.run = Some((at, last, self.table.get(input[last]) & RUN_END != 0));
                }
            }
        }
        Some(at)
    }

    /// \return the first index at or after \p from where the sets allow a match to start, from
    /// blocks that start at or before \p limit.
    #[inline(always)]
    fn find_allowed(
        &self,
        input: &[u16],
        from: usize,
        limit: usize,
        scan: &mut UnitScan,
    ) -> Option<usize> {
        let mut at = from;
        if scan.at <= from && from < scan.at + scan.len {
            let hits = scan.hits >> (from - scan.at);
            if hits != 0 {
                return Some(from + hits.trailing_zeros() as usize);
            }
            at = scan.at + scan.len;
        }
        self.scan_start(input, at, limit, scan)
    }

    /// \return the first index at or after \p at where the sets allow a match to start, from
    /// the blocks it tests, which start at or before \p limit, or the end of the input when an
    /// empty match may start there. Out of line, so that a search the last block answers does
    /// not set the covers up.
    #[inline(never)]
    fn scan_start(
        &self,
        input: &[u16],
        mut at: usize,
        limit: usize,
        scan: &mut UnitScan,
    ) -> Option<usize> {
        let end = input.len().saturating_sub(usize::from(self.second));
        while at < end && at <= limit {
            let len = (end - at).min(BLOCK);
            let hits = self.start_bits(input, at, len);
            *scan = UnitScan {
                at,
                len,
                hits,
                ..*scan
            };
            if hits != 0 {
                return Some(at + hits.trailing_zeros() as usize);
            }
            at += len;
        }
        let allows = self.at_input_end
            && at == input.len()
            && at <= limit
            && !(self.before
                && input
                    .last()
                    .is_some_and(|&last| self.table.get(last) & BEFORE != 0));
        allows.then_some(at)
    }

    /// \return the first index at or after \p from of a unit in the required set \p i.
    #[inline(always)]
    pub fn find_required(&self, i: usize, input: &[u16], from: usize) -> Option<usize> {
        let bit = REQUIRED << i;
        let mut at = from;
        while let Some(units) = block_at(input, at) {
            let mut maybe = self.covers[2 + i].bits(units);
            while maybe != 0 {
                let k = maybe.trailing_zeros() as usize;
                if self.table.get(units[k]) & bit != 0 {
                    return Some(at + k);
                }
                maybe &= maybe - 1;
            }
            at += BLOCK;
        }
        let k = input[at..]
            .iter()
            .position(|&u| self.table.get(u) & bit != 0)?;
        Some(at + k)
    }

    /// \return the last index at or after \p from of a unit in the required set \p i.
    #[inline(always)]
    pub fn rfind_required(&self, i: usize, input: &[u16], from: usize) -> Option<usize> {
        let bit = REQUIRED << i;
        let mut end = input.len();
        while let Some(units) = end
            .checked_sub(BLOCK)
            .filter(|&at| at >= from)
            .and_then(|at| block_at(input, at))
        {
            end -= BLOCK;
            let mut maybe = self.covers[2 + i].bits(units);
            while maybe != 0 {
                let k = 63 - maybe.leading_zeros() as usize;
                if self.table.get(units[k]) & bit != 0 {
                    return Some(end + k);
                }
                maybe &= !(1 << k);
            }
        }
        let k = input[from..end]
            .iter()
            .rposition(|&u| self.table.get(u) & bit != 0)?;
        Some(from + k)
    }
}

/// The block a search tested last: where it starts, how many units it holds, and which of them
/// a match may start at; and the run it read last.
#[derive(Debug, Clone, Copy, Default)]
pub struct UnitScan {
    at: usize,
    len: usize,
    hits: u64,
    /// Where the run was read from, its first unit that cannot continue it, and whether that
    /// unit may end it.
    run: Option<(usize, usize, bool)>,
}

impl UnitSet {
    /// \return the units of the given code points, or None if that is every unit.
    fn from_code_points(cps: &CodePointSet) -> Option<Self> {
        let units = first_units(cps);
        if units.intervals() == [Interval::new(0, 0xFFFF)] {
            return None;
        }
        Some(UnitSet::from_units(&units))
    }

    pub(crate) fn from_units(units: &CodePointSet) -> Self {
        UnitSet(
            units
                .intervals()
                .iter()
                .map(|iv| (iv.first as u16, iv.last as u16))
                .collect(),
        )
    }

    /// \return the units of the given code points that are a character by themselves in
    /// either mode, outside the surrogates; None if there are none.
    fn from_whole_chars(cps: &CodePointSet) -> Option<Self> {
        let ranges: Box<[(u16, u16)]> = cps
            .intervals()
            .iter()
            .flat_map(|iv| {
                [(0, 0xD7FF), (0xE000, 0xFFFF)]
                    .into_iter()
                    .filter(|&(first, last)| iv.first <= last && first <= iv.last)
                    .map(|(first, last)| (iv.first.max(first) as u16, iv.last.min(last) as u16))
            })
            .collect();
        (!ranges.is_empty()).then(|| UnitSet(ranges.into()))
    }
}

/// \return the units the given code points start with: themselves below U+10000, and their
/// high surrogates above.
fn first_units(cps: &CodePointSet) -> CodePointSet {
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
    units
}

/// \return the code points a node matching exactly one character accepts, if it is one.
pub(crate) fn single_char_code_points(n: &Node) -> Option<CodePointSet> {
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
        // A lookahead that consumes a character bounds the unit where it is tested, which is also
        // where what follows it starts, whether or not that consumes anything.
        Node::Cat(nodes) => {
            for (i, node) in nodes.iter().enumerate() {
                if let Some(mut ahead) = lookahead_first_code_points(node, at_input_start) {
                    let mut rest = CodePointSet::new();
                    if !nodes[i + 1..]
                        .iter()
                        .all(|n| add_first_code_points(n, &mut rest, at_input_start))
                    {
                        let mut outside = ahead.inverted();
                        outside.add_set(rest.inverted());
                        ahead = outside.inverted();
                    }
                    out.add_set(ahead);
                    return false;
                }
                if !add_first_code_points(node, out, at_input_start) {
                    return false;
                }
            }
            true
        }
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

/// \return the code points the character where \p n matches may be, if \p n is a lookahead,
/// possibly in a group, every match of which consumes one; and set \p at_input_start as
/// `add_first_code_points` does.
fn lookahead_first_code_points(n: &Node, at_input_start: &mut bool) -> Option<CodePointSet> {
    match n {
        Node::CaptureGroup { contents, .. } => {
            lookahead_first_code_points(contents, at_input_start)
        }
        Node::LookaroundAssertion {
            negate: false,
            backwards: false,
            contents,
            ..
        } => {
            let mut first = CodePointSet::new();
            let mut only_at_input_start = false;
            if add_first_code_points(contents, &mut first, &mut only_at_input_start) {
                return None;
            }
            *at_input_start |= only_at_input_start;
            Some(first)
        }
        _ => None,
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
            if !out.iter().any(|kept| *kept == units) {
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

/// Add to \p out the characters that rule out a match of \p n starting right after one of them:
/// those of the negative lookbehinds for one character every match tests where it starts, and
/// all but the line terminators for a multiline `^` there.
/// \return whether \p n consumes nothing, which leaves what follows it at the start too.
fn add_not_before(n: &Node, out: &mut CodePointSet) -> bool {
    match n {
        Node::LookaroundAssertion {
            negate,
            backwards,
            contents,
            ..
        } => {
            if let (true, true, Some(cps)) =
                (*negate, *backwards, single_char_code_points(contents))
            {
                out.add_set(cps);
            }
            true
        }
        Node::Anchor {
            anchor_type: ir::AnchorType::StartOfLine,
            multiline: true,
        } => {
            let mut terminators = CodePointSet::new();
            for c in [0x000A, 0x000D, 0x2028, 0x2029] {
                terminators.add_one(c);
            }
            out.add_set(terminators.inverted());
            true
        }
        Node::Empty | Node::Anchor { .. } | Node::WordBoundary { .. } => true,
        Node::CaptureGroup { contents, .. } => add_not_before(contents, out),
        Node::Cat(nodes) => nodes.iter().all(|n| add_not_before(n, out)),
        _ => false,
    }
}

/// \return the units that cannot continue the run and those that may end it, if every match of
/// \p n starts with a lookahead for a loop over one set of characters and then a character of
/// another, and no unit of the second can be part of a character of the first. The first unit
/// at or after a start that cannot continue the run is then where the lookahead needs its end.
fn lookahead_run(n: &Node) -> Option<(CodePointSet, CodePointSet)> {
    let mut n = n;
    loop {
        n = match n {
            Node::CaptureGroup { contents, .. } => contents,
            Node::Cat(nodes) => nodes.first()?,
            _ => break,
        };
    }
    let Node::LookaroundAssertion {
        negate: false,
        backwards: false,
        contents,
        ..
    } = n
    else {
        return None;
    };
    let mut contents = &**contents;
    while let Node::CaptureGroup {
        contents: inner, ..
    } = contents
    {
        contents = inner;
    }
    let Node::Cat(nodes) = contents else {
        return None;
    };
    let (Node::Loop { loopee, .. } | Node::Loop1CharBody { loopee, .. }) = nodes.first()? else {
        return None;
    };
    let body = single_char_code_points(loopee)?;
    let end = first_units(&single_char_code_points(nodes.get(1)?)?);
    let mut run = first_units(&body);
    if body
        .intervals()
        .last()
        .is_some_and(|iv| iv.last >= 0x1_0000)
    {
        run.add(Interval::new(0xDC00, 0xDFFF));
    }
    if !disjoint(&run, &end) {
        return None;
    }
    run.add(Interval::new(0x1_0000, CODE_POINT_MAX));
    Some((run.inverted(), end))
}

/// \return the prefilter for \p re, with the factor strings from \p factors.
pub fn prefilter_for_re(re: &ir::Regex, factors: crate::FactorSource) -> Utf16Prefilter {
    let mut first = CodePointSet::new();
    let mut at_input_start = false;
    let nullable = add_first_code_points(&re.node, &mut first, &mut at_input_start);
    let mut second_cps = CodePointSet::new();
    let second =
        if nullable || add_second_code_points(&re.node, None, &mut second_cps) != Some(false) {
            None
        } else {
            UnitSet::from_code_points(&second_cps)
        };
    let start = if nullable {
        None
    } else {
        UnitSet::from_code_points(&first)
    };
    let second_first = second.is_some() && commonness(&second_cps) < commonness(&first);
    let required = required_unit_sets(&re.node);
    let mut before = CodePointSet::new();
    add_not_before(&re.node, &mut before);
    let before = UnitSet::from_whole_chars(&before);
    let mut sets = vec![(start.as_ref(), START), (second.as_ref(), SECOND)];
    sets.extend(
        (0..)
            .zip(required.iter())
            .map(|(i, set)| (Some(set), REQUIRED << i)),
    );
    let mut covers = [Cover::new(None); 2 + MAX_REQUIRED];
    for (cover, (set, _)) in covers.iter_mut().zip(&sets) {
        *cover = Cover::new(*set);
    }
    if let Some(before) = &before {
        sets.push((Some(before), BEFORE));
    }
    let run = lookahead_run(&re.node).and_then(|(stop, end)| {
        Some((
            UnitSet::from_code_points(&stop),
            UnitSet::from_code_points(&end)?,
        ))
    });
    if let Some((stop, end)) = &run {
        sets.push((stop.as_ref(), STOP));
        sets.push((Some(end), RUN_END));
    }
    Utf16Prefilter {
        filters_start: start.is_some() || second.is_some() || before.is_some() || run.is_some(),
        second: second.is_some(),
        second_first,
        before: before.is_some(),
        exact: covers[0].exact && (second.is_none() || covers[1].exact) && before.is_none(),
        run: run.is_some(),
        at_input_start,
        at_input_end: nullable,
        required: required.len(),
        table: UnitTable::new(&sets),
        covers,
        factors: match factors {
            crate::FactorSource::Analyze(work) => crate::utf16factors::factors_for_re(re, work),
            crate::FactorSource::Given(given) => crate::utf16factors::Factors::from_aot(given, re),
        },
    }
}
