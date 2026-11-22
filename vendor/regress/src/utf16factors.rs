//! Strings of UTF-16 code units in a successful continuous forward-read prefix, so that a
//! search whose remaining input holds none of them is ruled out before it starts. A terminal
//! positive lookahead may extend this prefix beyond the consumed match.
//!
//! Each position of such a string is a set of units, which keeps case folding and character
//! classes to one string. The strings are read off the IR the way a trigram index reads its
//! queries: for every node, whether it may match the empty string, the strings its matches are
//! exactly, the strings its non-empty matches start and end with, and the strings every match
//! contains. A node whose characters may take two units, or whose matches are not understood,
//! ends what is known about adjacent units; an assertion consumes nothing, so the units on
//! either side of it are adjacent.

use crate::codepointset::{CodePointSet, Interval};
use crate::ir::{self, Node};
use crate::utf16prefilter::{BLOCK, Cover, UnitSet, UnitTable, block_at, single_char_code_points};
#[cfg(not(feature = "std"))]
use alloc::{boxed::Box, sync::Arc, vec::Vec};
#[cfg(feature = "std")]
use std::sync::Arc;

/// A string of unit sets: a unit of the text matches a position if it is in its set.
type Seq = Vec<UnitSet>;

/// The most positions a prefix or suffix keeps.
const MAX_AFFIX: usize = 8;

/// The most positions an exact string or a factor keeps.
const MAX_FACTOR: usize = 16;

/// The most strings a set keeps before the strings of each length merge into one.
const MAX_STRINGS: usize = 16;

/// The most strings a factor set checks; past it, strings of one length merge.
const MAX_CHECKED: usize = 8;

/// How often a factor set may occur per unit of prose and still be worth a scan.
const MAX_FREQUENCY: f64 = 1.0 / 32.0;

/// The most iterations of a loop of exact strings whose matches are listed.
const MAX_LISTED_ITERATIONS: usize = 4;

/// What is known about the matches of a node. A set holding the empty string tells nothing;
/// an empty set of prefixes or suffixes says that no match is non-empty.
#[derive(Clone)]
struct Info {
    /// Whether a match may be empty.
    nullable: bool,
    /// Strings every match is one of, if known.
    exact: Option<Vec<Seq>>,
    /// Strings every non-empty match starts with one of.
    prefix: Vec<Seq>,
    /// Strings every non-empty match ends with one of.
    suffix: Vec<Seq>,
    /// Strings every match contains one of.
    factor: Vec<Seq>,
}

#[derive(Clone, Copy)]
enum Side {
    Front,
    Back,
}

/// \return the set that tells nothing.
fn anything() -> Vec<Seq> {
    Vec::from([Vec::new()])
}

/// \return the union of two unit sets.
fn union(a: &UnitSet, b: &UnitSet) -> UnitSet {
    let mut cps = CodePointSet::new();
    for &(first, last) in a.0.iter().chain(b.0.iter()) {
        cps.add(crate::codepointset::Interval::new(
            u32::from(first),
            u32::from(last),
        ));
    }
    UnitSet::from_units(&cps)
}

/// \return whether \p u is in \p set.
#[inline(always)]
fn holds(set: &UnitSet, u: u16) -> bool {
    let i = set.0.partition_point(|&(_, last)| last < u);
    set.0.get(i).is_some_and(|&(first, _)| first <= u)
}

/// \return the units of \p cps if each of its characters is one unit in either mode.
fn units_of(cps: &CodePointSet) -> Option<UnitSet> {
    let ivs = cps.intervals();
    if ivs.is_empty() || ivs.iter().any(|iv| iv.last > 0xFFFF) {
        return None;
    }
    Some(UnitSet::from_units(cps))
}

/// \return \p set without repeated strings, where too many strings of one length merge
/// position by position into one, which every one of them matches.
fn merge(set: Vec<Seq>, most: usize) -> Vec<Seq> {
    let mut out: Vec<Seq> = Vec::new();
    for seq in set {
        if !out.contains(&seq) {
            out.push(seq);
        }
    }
    if out.len() <= most {
        return out;
    }
    let mut lens: Vec<usize> = out.iter().map(Vec::len).collect();
    lens.sort_unstable();
    lens.dedup();
    lens.into_iter()
        .map(|len| {
            let mut same = out.iter().filter(|seq| seq.len() == len);
            let mut merged = same.next().cloned().unwrap_or_default();
            for seq in same {
                for (kept, set) in merged.iter_mut().zip(seq) {
                    *kept = union(kept, set);
                }
            }
            merged
        })
        .collect()
}

/// \return every string of \p a followed by every string of \p b.
fn cross(a: &[Seq], b: &[Seq]) -> Vec<Seq> {
    a.iter()
        .flat_map(|x| {
            b.iter().map(move |y| {
                let mut seq = x.clone();
                seq.extend_from_slice(y);
                seq
            })
        })
        .collect()
}

/// \return the strings of \p a, then those of \p b.
fn joined(mut a: Vec<Seq>, b: Vec<Seq>) -> Vec<Seq> {
    a.extend(b);
    a
}

/// \return the strings of \p set and, if \p empty, the empty string.
fn or_empty(set: &[Seq], empty: bool) -> Vec<Seq> {
    let mut set = set.to_vec();
    if empty {
        set.push(Vec::new());
    }
    set
}

/// \return the strings of \p set but the empty one.
fn non_empty(set: &[Seq]) -> Vec<Seq> {
    set.iter().filter(|seq| !seq.is_empty()).cloned().collect()
}

/// \return \p set as an exact set, if it stays small enough.
fn exact_set(set: Vec<Seq>) -> Option<Vec<Seq>> {
    if set.iter().any(|seq| seq.len() > MAX_FACTOR) {
        return None;
    }
    let set = merge(set, MAX_STRINGS);
    (set.len() <= MAX_STRINGS).then_some(set)
}

/// \return \p set as strings a match starts or, from the back, ends with: each cut to
/// `MAX_AFFIX` positions, and all of them cut to the shortest while there are too many. An
/// empty set stays empty.
fn affix(set: Vec<Seq>, side: Side) -> Vec<Seq> {
    let cut = |mut seq: Seq, len: usize| {
        if seq.len() > len {
            match side {
                Side::Front => seq.truncate(len),
                Side::Back => {
                    seq.drain(..seq.len() - len);
                }
            }
        }
        seq
    };
    let set: Vec<Seq> = set.into_iter().map(|seq| cut(seq, MAX_AFFIX)).collect();
    if set.iter().any(Vec::is_empty) {
        return anything();
    }
    let set = merge(set, MAX_STRINGS);
    if set.len() <= MAX_STRINGS {
        return set;
    }
    let shortest = set.iter().map(Vec::len).min().unwrap_or(0);
    merge(set.into_iter().map(|seq| cut(seq, shortest)).collect(), 1)
}

/// \return \p set as strings a match contains: each cut to `MAX_FACTOR` positions, and all of
/// them cut to the shortest while there are too many. An empty set tells nothing.
fn factor_set(set: Vec<Seq>, most: usize) -> Vec<Seq> {
    let mut set: Vec<Seq> = set
        .into_iter()
        .map(|mut seq| {
            seq.truncate(MAX_FACTOR);
            seq
        })
        .collect();
    if set.is_empty() || set.iter().any(Vec::is_empty) {
        return anything();
    }
    set = merge(set, most);
    if set.len() <= most {
        return set;
    }
    let shortest = set.iter().map(Vec::len).min().unwrap_or(0);
    merge(
        set.into_iter()
            .map(|mut seq| {
                seq.truncate(shortest);
                seq
            })
            .collect(),
        1,
    )
}

/// Per mille of each Russian letter, а to я, among the letters of Russian prose.
const RUSSIAN_LETTERS: [u16; 32] = [
    80, 16, 45, 17, 30, 85, 9, 16, 74, 12, 35, 44, 32, 67, 110, 28, 47, 55, 63, 26, 3, 10, 5, 14,
    7, 4, 1, 19, 17, 3, 6, 20,
];

/// \return how often a unit of \p set occurs per unit of prose, the kind of text searched
/// most, from rough frequencies of Russian and English text with markup placeholders.
fn frequency(set: &UnitSet) -> f64 {
    share(set, 0.0001).min(1.0)
}

/// \return how often a unit of prose is not in \p set, leaving out units other than those
/// `share` knows.
fn outside(set: &UnitSet) -> f64 {
    share(&UnitSet(Arc::new([(0, 0xFFFF)])), 0.0) - share(set, 0.0)
}

/// \return the share of prose the units of \p set take, where a unit that is neither common
/// punctuation nor a Latin or Russian letter takes \p other.
fn share(set: &UnitSet, other: f64) -> f64 {
    const BANDS: &[(u16, u16, f64)] = &[
        (0x0A, 0x0A, 0.01),
        (0x20, 0x20, 0.15),
        (0x21, 0x2B, 0.001),
        (0x2C, 0x2C, 0.015),
        (0x2D, 0x2D, 0.002),
        (0x2E, 0x2E, 0.012),
        (0x2F, 0x2F, 0.001),
        (0x30, 0x39, 0.002),
        (0x3A, 0x40, 0.001),
        (0x41, 0x5A, 0.0005),
        (0x5B, 0x60, 0.001),
        (0x61, 0x7A, 0.002),
        (0x7B, 0x7E, 0.001),
        (0xA0, 0xA0, 0.005),
        (0xAB, 0xAB, 0.003),
        (0xBB, 0xBB, 0.003),
        (0x451, 0x451, 0.0003),
        (0x2013, 0x2014, 0.003),
        (0x2026, 0x2026, 0.001),
        (0xE000, 0xE001, 0.003),
    ];
    /// The share of prose taken by lowercase and by capital Russian letters.
    const LOWER: f64 = 0.7;
    const CAPITAL: f64 = 0.02;
    let mut total = 0.0;
    for &(first, last) in set.0.iter() {
        total += f64::from(u32::from(last) - u32::from(first) + 1) * other;
        for &(lo, hi, weight) in BANDS {
            let (lo, hi) = (lo.max(first), hi.min(last));
            if lo <= hi {
                total += f64::from(u32::from(hi) - u32::from(lo) + 1) * (weight - other);
            }
        }
        for (base, share) in [(0x410, CAPITAL), (0x430, LOWER)] {
            let (lo, hi) = (first.max(base), last.min(base + 31));
            if lo <= hi {
                let letters = &RUSSIAN_LETTERS[usize::from(lo - base)..=usize::from(hi - base)];
                for &per_mille in letters {
                    total += f64::from(per_mille) / 1000.0 * share - other;
                }
            }
        }
    }
    total
}
/// \return how often a string of \p set occurs per unit of prose; the empty string always.
fn score<'a>(set: impl IntoIterator<Item = &'a Seq>) -> f64 {
    set.into_iter()
        .map(|seq| seq.iter().map(frequency).product::<f64>())
        .sum()
}

/// \return whether every unit of \p b is in \p a.
fn includes(a: &UnitSet, b: &UnitSet) -> bool {
    b.0.iter().all(|&(first, last)| {
        let i = a.0.partition_point(|&(_, end)| end < first);
        a.0.get(i)
            .is_some_and(|&(start, end)| start <= first && last <= end)
    })
}

/// \return which strings of the factor set \p set to keep: all but those that contain another of
/// them, whose occurrence holds an occurrence of that other one.
fn kept(set: &[Seq]) -> Vec<bool> {
    let within = |a: &Seq, b: &Seq| {
        a.len() <= b.len()
            && (0..=b.len() - a.len()).any(|k| a.iter().zip(&b[k..]).all(|(x, y)| includes(x, y)))
    };
    (0..set.len())
        .map(|i| {
            !(0..set.len())
                .any(|j| j != i && within(&set[j], &set[i]) && (j < i || !within(&set[i], &set[j])))
        })
        .collect()
}

/// \return the set of \p sets that occurs least often once pruned to the strings it keeps,
/// leaving out empty ones.
fn rarest(sets: &[&[Seq]]) -> Vec<Seq> {
    fn pruned<'a>(set: &'a [Seq], kept: &'a [bool]) -> impl Iterator<Item = &'a Seq> {
        set.iter()
            .zip(kept)
            .filter_map(|(seq, &kept)| kept.then_some(seq))
    }
    sets.iter()
        .filter(|set| !set.is_empty())
        .map(|&set| {
            let kept = kept(set);
            (score(pruned(set, &kept)), set, kept)
        })
        .min_by(|a, b| a.0.total_cmp(&b.0))
        .map_or_else(anything, |(_, set, kept)| {
            pruned(set, &kept).cloned().collect()
        })
}

impl Info {
    fn unknown() -> Self {
        Info {
            nullable: true,
            exact: None,
            prefix: anything(),
            suffix: anything(),
            factor: anything(),
        }
    }

    /// \return what is known about a character whose units are not.
    fn one_char() -> Self {
        Info {
            nullable: false,
            ..Self::unknown()
        }
    }

    fn exactly(set: Vec<Seq>) -> Self {
        Info {
            nullable: set.iter().any(Vec::is_empty),
            prefix: affix(non_empty(&set), Side::Front),
            suffix: affix(non_empty(&set), Side::Back),
            factor: anything(),
            exact: exact_set(set),
        }
    }

    fn empty() -> Self {
        Self::exactly(anything())
    }

    /// \return the rarest set of strings every match contains one of.
    fn best(&self) -> Vec<Seq> {
        if self.nullable {
            return anything();
        }
        let exact = self
            .exact
            .clone()
            .map_or_else(Vec::new, |set| factor_set(set, MAX_STRINGS));
        rarest(&[&self.factor, &exact, &self.prefix, &self.suffix])
    }
}

fn cat(x: Info, y: Info) -> Info {
    let nullable = x.nullable && y.nullable;
    let exact = match (&x.exact, &y.exact) {
        (Some(a), Some(b)) if a.len() * b.len() <= MAX_STRINGS * MAX_STRINGS => {
            exact_set(cross(a, b))
        }
        _ => None,
    };
    // A non-empty match starts with one of x when y is empty, and with one of y when x is.
    let prefix = match &x.exact {
        Some(a) => {
            let mut set = cross(a, &y.prefix);
            if y.nullable {
                set.extend(non_empty(a));
            }
            affix(set, Side::Front)
        }
        None if x.nullable => affix(joined(x.prefix.clone(), y.prefix.clone()), Side::Front),
        None => x.prefix.clone(),
    };
    let suffix = match &y.exact {
        Some(b) => {
            let mut set = cross(&x.suffix, b);
            if x.nullable {
                set.extend(non_empty(b));
            }
            affix(set, Side::Back)
        }
        None if y.nullable => affix(joined(x.suffix.clone(), y.suffix.clone()), Side::Back),
        None => y.suffix.clone(),
    };
    let factor = if nullable {
        anything()
    } else {
        let joint = cross(
            &or_empty(&x.suffix, x.nullable),
            &or_empty(&y.prefix, y.nullable),
        );
        rarest(&[&x.best(), &y.best(), &factor_set(joint, MAX_STRINGS)])
    };
    Info {
        nullable,
        exact,
        prefix,
        suffix,
        factor,
    }
}

fn alt(x: Info, y: Info) -> Info {
    let nullable = x.nullable || y.nullable;
    let factor = if nullable {
        anything()
    } else {
        factor_set(joined(x.best(), y.best()), MAX_STRINGS)
    };
    let exact = match (x.exact, y.exact) {
        (Some(a), Some(b)) => exact_set(joined(a, b)),
        _ => None,
    };
    Info {
        nullable,
        exact,
        prefix: affix(joined(x.prefix, y.prefix), Side::Front),
        suffix: affix(joined(x.suffix, y.suffix), Side::Back),
        factor,
    }
}

fn repeat(body: Info, min: usize, max: Option<usize>) -> Info {
    if max == Some(0) {
        return Info::empty();
    }
    if let (Some(exact), Some(max)) = (&body.exact, max)
        && max <= MAX_LISTED_ITERATIONS
    {
        let mut all = if min == 0 { anything() } else { Vec::new() };
        let mut power = Some(anything());
        for k in 1..=max {
            power = power.and_then(|p| exact_set(cross(&p, exact)));
            match &power {
                Some(p) if k >= min => all.extend(p.iter().cloned()),
                Some(_) => {}
                None => break,
            }
        }
        if power.is_some() {
            return Info::exactly(all);
        }
    }
    // Iterations that match empty add nothing, so a non-empty match starts and ends with a
    // non-empty iteration.
    let nullable = min == 0 || body.nullable;
    let factor = if nullable {
        anything()
    } else if min >= 2 {
        rarest(&[
            &body.best(),
            &factor_set(cross(&body.suffix, &body.prefix), MAX_STRINGS),
        ])
    } else {
        body.best()
    };
    Info {
        nullable,
        exact: None,
        prefix: body.prefix,
        suffix: body.suffix,
        factor,
    }
}

/// \return what is known about the matches of \p n, or nothing once \p work runs out: each
/// catenation, alternation and loop costs the product of the weights of what it combines.
fn info(n: &Node, work: &mut usize) -> Info {
    if *work == 0 {
        return Info::unknown();
    }
    if let Some(cps) = single_char_code_points(n) {
        return match units_of(&cps) {
            Some(units) => Info::exactly(Vec::from([Vec::from([units])])),
            None => Info::one_char(),
        };
    }
    match n {
        Node::Empty
        | Node::Goal
        | Node::Anchor { .. }
        | Node::WordBoundary { .. }
        | Node::LookaroundAssertion { .. } => Info::empty(),
        Node::CaptureGroup { contents, .. } => info(contents, work),
        Node::Cat(nodes) => {
            let mut acc = Info::empty();
            for node in nodes.iter() {
                let next = info(node, work);
                if !spend(work, &acc, &next) {
                    return Info::unknown();
                }
                acc = cat(acc, next);
            }
            acc
        }
        Node::Alt(left, right) => {
            let (left, right) = (info(left, work), info(right, work));
            if !spend(work, &left, &right) {
                return Info::unknown();
            }
            alt(left, right)
        }
        Node::Loop { loopee, quant, .. } | Node::Loop1CharBody { loopee, quant } => {
            let body = info(loopee, work);
            if !spend(work, &body, &body) {
                return Info::unknown();
            }
            repeat(body, quant.min, quant.max)
        }
        // Back-references match text known only at run time; string sets and literal bytes
        // are never emitted for UTF-16 matching.
        _ => Info::unknown(),
    }
}

/// \return a unit for each string \p info keeps and for each range of each of its positions.
fn weight(info: &Info) -> usize {
    let set = |set: &[Seq]| -> usize {
        set.iter()
            .map(|seq| 1 + seq.iter().map(|units| units.0.len()).sum::<usize>())
            .sum()
    };
    1 + set(&info.prefix)
        + set(&info.suffix)
        + set(&info.factor)
        + info.exact.as_deref().map_or(0, set)
}

/// Take the work of an operation on \p x and \p y from \p work, down to nothing.
/// \return whether there was enough for it.
fn spend(work: &mut usize, x: &Info, y: &Info) -> bool {
    let cost = weight(x).saturating_mul(weight(y));
    *work = work.saturating_sub(cost);
    *work > 0
}

fn terminal_forward_read(n: &Node) -> Option<Node> {
    match n {
        Node::LookaroundAssertion {
            negate: false,
            backwards: false,
            contents,
            ..
        } => Some((**contents).clone()),
        Node::CaptureGroup { contents, .. } => terminal_forward_read(contents),
        Node::Cat(nodes) => {
            let last = nodes
                .iter()
                .rposition(|node| !matches!(node, Node::Empty | Node::Goal))?;
            let tail = terminal_forward_read(&nodes[last])?;
            let mut read = nodes[..last].to_vec();
            read.push(tail);
            Some(Node::Cat(read))
        }
        _ => None,
    }
}

/// Add to \p out the characters the matches of \p n consume.
/// \return whether they are known: a back-reference consumes text known only at run time.
fn consumed(n: &Node, out: &mut Vec<Interval>) -> bool {
    match n {
        Node::Char { c } => {
            out.push(Interval::new(*c, *c));
            return true;
        }
        Node::CharSet(chars) => {
            out.extend(chars.iter().map(|&c| Interval::new(c, c)));
            return true;
        }
        _ => {}
    }
    if let Some(cps) = single_char_code_points(n) {
        out.extend_from_slice(cps.intervals());
        return true;
    }
    match n {
        Node::Empty
        | Node::Goal
        | Node::Anchor { .. }
        | Node::WordBoundary { .. }
        | Node::LookaroundAssertion { .. } => true,
        Node::CaptureGroup { contents, .. } => consumed(contents, out),
        Node::Cat(nodes) => nodes.iter().all(|node| consumed(node, out)),
        Node::Alt(left, right) => consumed(left, out) && consumed(right, out),
        Node::Loop { loopee, .. } | Node::Loop1CharBody { loopee, .. } => consumed(loopee, out),
        _ => false,
    }
}

/// \return the units of the characters \p cps, with every surrogate if one of them is or takes
/// one, so that a run of them never starts or ends inside a surrogate pair.
fn span_units(cps: &CodePointSet) -> UnitSet {
    let mut units = CodePointSet::new();
    for iv in cps.intervals() {
        if iv.first <= 0xFFFF {
            units.add(crate::codepointset::Interval::new(
                iv.first,
                iv.last.min(0xFFFF),
            ));
        }
        if iv.last > 0xFFFF || (iv.first <= 0xDFFF && iv.last >= 0xD800) {
            units.add(crate::codepointset::Interval::new(0xD800, 0xDFFF));
        }
    }
    UnitSet::from_units(&units)
}

/// Strings of unit sets in the successful forward-read prefix, and a scan for them.
#[derive(Debug, Clone)]
pub struct Factors {
    /// The strings, each with where its pair of adjacent sets least common in prose starts,
    /// which a scan looks for.
    seqs: Box<[(Box<[UnitSet]>, usize)]>,
    /// The least and the greatest of those starts.
    anchors: (usize, usize),
    /// For each unit, a bit for each string whose first set of the pair holds it.
    first: UnitTable,
    /// The same for the second set; a string of one set has its bit for every unit here, and
    /// in `singles`, which stands for the end of the input.
    second: UnitTable,
    singles: u8,
    /// The cover of the units of every first set, or, with `offset` 1, of every second set,
    /// whichever is less common in prose.
    cover: Cover,
    offset: usize,
    /// The units of the forward-read prefix, given enough prose outside them to cut it into
    /// short runs: a match starts in the run holding its string, including any read suffix.
    span: Option<UnitTable>,
}

/// How often a unit of prose must be outside the forward-read prefix units for searches to
/// keep to the runs of those units that hold one of the strings.
const MIN_OUTSIDE: f64 = 1.0 / 128.0;

/// How often a unit of prose may start the pair of one of the strings, summed over them, and
/// still be worth a scan before a search.
const MAX_CANDIDATES: f64 = 1.0 / 32.0;

/// How often a unit of prose may be in the cover a scan tests blocks against.
const MAX_COVERED: f64 = 1.0 / 8.0;

/// \return the union of \p sets, None, every unit, if one of them is.
fn union_of<'a>(sets: impl Iterator<Item = Option<&'a UnitSet>>) -> Option<UnitSet> {
    let mut out: Option<UnitSet> = None;
    for set in sets {
        let set = set?;
        out = Some(out.map_or_else(|| set.clone(), |acc| union(&acc, set)));
    }
    out
}

/// \return the strings every match of \p re contains one of, if they are rare enough in prose
/// to be worth a scan before a search, tell more than the required sets, and their pairs are
/// rare enough to scan for; None also when reading them off the IR takes more than \p work.
pub fn factors_for_re(re: &ir::Regex, mut work: usize) -> Option<Factors> {
    let tail = terminal_forward_read(&re.node);
    let read = tail.as_ref().unwrap_or(&re.node);
    let info = info(read, &mut work);
    if work == 0 {
        return None;
    }
    let best = factor_set(info.best(), MAX_CHECKED);
    if best.iter().any(Vec::is_empty)
        || best.iter().all(|seq| seq.len() < 2)
        || score(&best) > MAX_FREQUENCY
    {
        return None;
    }
    let pair = |seq: &Seq| {
        (0..seq.len().saturating_sub(1))
            .min_by(|&a, &b| {
                let cost = |k: usize| frequency(&seq[k]) * frequency(&seq[k + 1]);
                cost(a).total_cmp(&cost(b))
            })
            .unwrap_or(0)
    };
    let seqs: Box<[(Box<[UnitSet]>, usize)]> = best
        .into_iter()
        .map(|seq| {
            let k = pair(&seq);
            (seq.into_boxed_slice(), k)
        })
        .collect();
    let (first_units, second_units) = pair_units(&seqs);
    let candidates: f64 = seqs
        .iter()
        .map(|(seq, k)| frequency(&seq[*k]) * seq.get(k + 1).map_or(1.0, frequency))
        .sum();
    let covered = first_units
        .as_ref()
        .map_or(1.0, frequency)
        .min(second_units.as_ref().map_or(1.0, frequency));
    if candidates > MAX_CANDIDATES || covered > MAX_COVERED {
        return None;
    }
    Some(Factors::new(seqs, span(re)))
}

/// \return the units of the forward-read prefix of \p re, `terminal_forward_read` or the whole
/// pattern, if they leave enough prose outside them for searches to keep to their runs.
fn span(re: &ir::Regex) -> Option<UnitSet> {
    let mut ivs = Vec::new();
    let known = match read_consumed(&re.node, &mut ivs) {
        Some(known) => known,
        None => consumed(&re.node, &mut ivs),
    };
    known
        .then(|| span_units(&CodePointSet::from_intervals(ivs)))
        .filter(|units| outside(units) >= MIN_OUTSIDE)
}

/// Add to \p out what `consumed` adds for `terminal_forward_read` of \p n, without building it.
/// \return what `consumed` returns, or None, adding nothing, if \p n has no such read.
fn read_consumed(n: &Node, out: &mut Vec<Interval>) -> Option<bool> {
    match n {
        Node::LookaroundAssertion {
            negate: false,
            backwards: false,
            contents,
            ..
        } => Some(consumed(contents, out)),
        Node::CaptureGroup { contents, .. } => read_consumed(contents, out),
        Node::Cat(nodes) => {
            let last = nodes
                .iter()
                .rposition(|node| !matches!(node, Node::Empty | Node::Goal))?;
            let tail = read_consumed(&nodes[last], out)?;
            Some(tail && nodes[..last].iter().all(|node| consumed(node, out)))
        }
        _ => None,
    }
}

#[cfg(feature = "codegen")]
fn put(bytes: &mut Vec<u8>, mut n: usize) {
    while n >= 0x80 {
        bytes.push(n as u8 | 0x80);
        n >>= 7;
    }
    bytes.push(n as u8);
}

fn take(bytes: &mut &[u8]) -> usize {
    let mut n = 0;
    let mut shift = 0;
    loop {
        let (&byte, rest) = bytes
            .split_first()
            .expect("factor tables end where their counts say");
        *bytes = rest;
        n |= usize::from(byte & 0x7F) << shift;
        if byte < 0x80 {
            return n;
        }
        shift += 7;
    }
}

/// \return the union of the first sets of the pairs of \p seqs and that of the second sets.
fn pair_units(seqs: &[(Box<[UnitSet]>, usize)]) -> (Option<UnitSet>, Option<UnitSet>) {
    (
        union_of(seqs.iter().map(|(seq, k)| Some(&seq[*k]))),
        union_of(seqs.iter().map(|(seq, k)| seq.get(k + 1))),
    )
}

impl Factors {
    /// \return the scan for the strings \p seqs, each with where its pair starts, that keeps
    /// searches to runs of the units \p span, if given.
    fn new(seqs: Box<[(Box<[UnitSet]>, usize)]>, span: Option<UnitSet>) -> Self {
        let (first_units, second_units) = pair_units(&seqs);
        let (first_frequency, second_frequency) = (
            first_units.as_ref().map_or(1.0, frequency),
            second_units.as_ref().map_or(1.0, frequency),
        );
        let bits = |i: usize| 1u8 << i;
        let every = u8::MAX >> (8 - seqs.len());
        let first: Vec<(Option<&UnitSet>, u8)> = (0..)
            .zip(seqs.iter())
            .map(|(i, (seq, k))| (Some(&seq[*k]), bits(i)))
            .collect();
        let second: Vec<(Option<&UnitSet>, u8)> = (0..)
            .zip(seqs.iter())
            .map(|(i, (seq, k))| (seq.get(k + 1), bits(i)))
            .collect();
        let singles = (0..)
            .zip(seqs.iter())
            .filter(|(_, (seq, _))| seq.len() < 2)
            .fold(0, |singles, (i, _)| singles | bits(i));
        let anchors = seqs
            .iter()
            .fold((usize::MAX, 0), |(lo, hi), &(_, k)| (lo.min(k), hi.max(k)));
        Factors {
            first: UnitTable::with_every(&first, every),
            second: UnitTable::with_every(&second, every),
            singles,
            cover: if second_frequency < first_frequency {
                Cover::new(second_units.as_ref())
            } else {
                Cover::new(first_units.as_ref())
            },
            offset: usize::from(second_frequency < first_frequency),
            anchors,
            span: span
                .as_ref()
                .map(|units| UnitTable::with_every(&[(Some(units), 1)], 1)),
            seqs,
        }
    }

    pub(crate) fn from_aot(factors: crate::aot::Factors, re: &ir::Regex) -> Option<Self> {
        let mut data = factors.0;
        let bytes = &mut data;
        let mut seqs = Vec::new();
        for _ in 0..take(bytes) {
            let k = take(bytes);
            let sets = (0..take(bytes))
                .map(|_| {
                    let mut from = 0;
                    let ranges = (0..take(bytes)).map(|_| {
                        let step = take(bytes);
                        let first = from + step / 2;
                        let last = first + if step % 2 == 1 { take(bytes) + 1 } else { 0 };
                        from = last + 1;
                        (first as u16, last as u16)
                    });
                    UnitSet(ranges.collect())
                })
                .collect();
            seqs.push((sets, k));
        }
        (!seqs.is_empty()).then(|| Self::new(seqs.into(), span(re)))
    }

    #[cfg(feature = "codegen")]
    pub(crate) fn to_aot(&self) -> Vec<u8> {
        let mut bytes = Vec::new();
        put(&mut bytes, self.seqs.len());
        for (seq, k) in &self.seqs {
            put(&mut bytes, *k);
            put(&mut bytes, seq.len());
            for set in seq {
                put(&mut bytes, set.0.len());
                let mut from = 0;
                for &(first, last) in set.0.iter() {
                    let (first, last) = (usize::from(first), usize::from(last));
                    if first == last {
                        put(&mut bytes, (first - from) * 2);
                    } else {
                        put(&mut bytes, (first - from) * 2 + 1);
                        put(&mut bytes, last - first - 1);
                    }
                    from = last + 1;
                }
            }
        }
        bytes
    }

    /// \return the strings whose pair may start at the unit \p u followed by \p next, None at
    /// the end of the input, a bit each.
    #[inline(always)]
    fn pair_at(&self, u: u16, next: Option<u16>) -> u8 {
        self.first.get(u) & next.map_or(self.singles, |next| self.second.get(next))
    }

    /// \return the start of the string \p seq if its pair starts at \p q of \p input and the
    /// whole string is there at or after \p from.
    #[inline(always)]
    fn occurs(
        input: &[u16],
        from: usize,
        q: usize,
        (seq, k): &(Box<[UnitSet]>, usize),
    ) -> Option<usize> {
        let start = q.checked_sub(*k).filter(|&start| start >= from)?;
        let units = input.get(start..start + seq.len())?;
        units
            .iter()
            .zip(seq.iter())
            .all(|(&u, set)| holds(set, u))
            .then_some(start)
    }

    /// \return the starts of the strings in \p mask whose pair starts at \p q.
    fn starts(
        &self,
        input: &[u16],
        from: usize,
        q: usize,
        mut mask: u8,
    ) -> impl Iterator<Item = usize> {
        core::iter::from_fn(move || {
            while mask != 0 {
                let i = mask.trailing_zeros() as usize;
                mask &= mask - 1;
                if let Some(start) = Self::occurs(input, from, q, &self.seqs[i]) {
                    return Some(start);
                }
            }
            None
        })
    }

    /// \return the first string at or after \p from in \p input, as the least start; None if
    /// there is none.
    pub fn find(&self, input: &[u16], from: usize) -> Option<usize> {
        let mut best: Option<usize> = None;
        let mut at = from + self.anchors.0;
        while at + BLOCK < input.len()
            && let Some(units) = block_at(input, at + self.offset)
        {
            let mut maybe = self.cover.bits(units);
            while maybe != 0 {
                let q = at + maybe.trailing_zeros() as usize;
                maybe &= maybe - 1;
                let mask = self.pair_at(input[q], Some(input[q + 1]));
                if mask != 0 {
                    best = self.starts(input, from, q, mask).chain(best).min();
                    // A pair after `q` starts a string after the best one.
                    if best.is_some_and(|b| q >= b + self.anchors.1) {
                        return best;
                    }
                }
            }
            at += BLOCK;
            // A pair at or after `at` starts a string after the best one.
            if best.is_some_and(|b| at > b + self.anchors.1) {
                return best;
            }
        }
        for q in at..input.len() {
            if best.is_some_and(|b| q > b + self.anchors.1) {
                break;
            }
            let mask = self.pair_at(input[q], input.get(q + 1).copied());
            if mask != 0 {
                best = self.starts(input, from, q, mask).chain(best).min();
            }
        }
        best
    }

    /// \return whether searches keep to runs of the forward-read prefix units.
    #[inline(always)]
    pub fn spans(&self) -> bool {
        self.span.is_some()
    }

    /// \return the run of the forward-read prefix units around \p at of \p input, from no
    /// further left than \p from, as its first index and the index past it; None if the units
    /// are not kept. The unit at \p at, which starts a string, is one of them.
    pub fn run(&self, input: &[u16], from: usize, at: usize) -> Option<(usize, usize)> {
        let span = self.span.as_ref()?;
        let outside = |&i: &usize| span.get(input[i]) == 0;
        let start = (from..at).rev().find(outside).map_or(from, |i| i + 1);
        let end = (at + 1..input.len()).find(outside).unwrap_or(input.len());
        Some((start, end))
    }

    /// \return the last string at or after \p from in \p input, as the greatest start; None if
    /// there is none.
    pub fn rfind(&self, input: &[u16], from: usize) -> Option<usize> {
        let low = from + self.anchors.0;
        let mut best: Option<usize> = None;
        let mut end = input.len().saturating_sub(1);
        if let Some(&u) = input.get(end).filter(|_| end >= low) {
            let mask = self.pair_at(u, None);
            if mask != 0 {
                best = self.starts(input, from, end, mask).chain(best).max();
            }
        }
        while end >= low + BLOCK
            && let Some(units) = block_at(input, end - BLOCK + self.offset)
        {
            end -= BLOCK;
            let mut maybe = self.cover.bits(units);
            while maybe != 0 {
                let k = 63 - maybe.leading_zeros() as usize;
                maybe &= !(1 << k);
                let q = end + k;
                let mask = self.pair_at(input[q], Some(input[q + 1]));
                if mask != 0 {
                    best = self.starts(input, from, q, mask).chain(best).max();
                }
            }
            // A pair before `end` starts a string before the best one.
            if best.is_some_and(|b| end <= b + self.anchors.0) {
                return best;
            }
        }
        for q in (low..end).rev() {
            if best.is_some_and(|b| q < b + self.anchors.0) {
                break;
            }
            let mask = self.pair_at(input[q], input.get(q + 1).copied());
            if mask != 0 {
                best = self.starts(input, from, q, mask).chain(best).max();
            }
        }
        best
    }
}

#[cfg(test)]
mod tests {
    use crate::Regex;

    #[test]
    fn terminal_assertion_factors_cover_the_read_suffix() {
        for (pattern, text, expected) in [
            ("a(?=bc)", "abc", 0..1),
            ("a(?=(?<=Xa)bc)", "Xabc", 1..2),
            ("(?=bc)", "bc", 0..0),
        ] {
            let re = Regex::with_flags(pattern, "u").unwrap();
            let text: Vec<_> = text.encode_utf16().collect();
            let factors = re.cr.utf16.factors.as_ref().unwrap();
            assert!(factors.spans());
            let at = factors.find(&text, 0).unwrap();
            let (start, end) = factors.run(&text, 0, at).unwrap();
            assert!(start <= expected.start && expected.start < end);
            assert!(
                factors
                    .seqs
                    .iter()
                    .all(|(seq, _)| seq.len() > expected.len())
            );
            assert_eq!(re.find_from_utf16(&text, 0).next().unwrap().range, expected);
        }
    }

    #[test]
    fn analysis_past_its_work_goes_without_factors() {
        use crate::FactorSource;
        let words: Vec<String> = (0..64).map(|i| format!("&q{i}z;")).collect();
        let pattern: Vec<u16> = words.join("|").encode_utf16().collect();
        let compile = |work| Regex::from_utf16_with(&pattern, "u", FactorSource::Analyze(work));
        let full = compile(usize::MAX).unwrap();
        let cut = compile(1 << 10).unwrap();
        assert!(full.cr.utf16.factors.is_some());
        assert!(cut.cr.utf16.factors.is_none());
        let text: Vec<u16> = "a &q7z; b &q63z;".encode_utf16().collect();
        let ranges =
            |re: &Regex| -> Vec<_> { re.find_from_utf16(&text, 0).map(|m| m.range).collect() };
        assert_eq!(ranges(&full), vec![2..7, 10..16]);
        assert_eq!(ranges(&cut), ranges(&full));
    }

    #[cfg(feature = "codegen")]
    #[test]
    fn written_out_factors_read_back_as_analyzed() {
        use crate::FactorSource;
        for (pattern, flags) in [
            ("a(?=bc)", "u"),
            (r"(a)(?=\1bc)", "u"),
            ("(?:кг|мг|км)\\.(?![а-яё])", "iu"),
            ("x[a-cfh-k\u{FFF0}-\u{FFFF}]yz|q\u{1F600}rs", "u"),
            ("(?<![\\p{L}\\d])(?:ул|пер|просп)\\.[ \\t]+\\d", "giu"),
        ] {
            let units: Vec<u16> = pattern.encode_utf16().collect();
            let compile = |factors| Regex::from_utf16_with(&units, flags, factors).unwrap();
            let analyzed = compile(FactorSource::Analyze(usize::MAX));
            let bytes = crate::codegen::Codegen::factors(&analyzed).unwrap();
            let bytes: &'static [u8] = Box::leak(bytes.into_boxed_slice());
            let given = compile(FactorSource::Given(crate::aot::Factors(bytes)));
            assert_eq!(
                format!("{:?}", given.cr.utf16.factors),
                format!("{:?}", analyzed.cr.utf16.factors),
                "/{pattern}/{flags}"
            );
        }
    }

    #[test]
    fn terminal_assertion_backreference_keeps_the_span_unknown() {
        let re = Regex::with_flags(r"(a)(?=\1bc)", "u").unwrap();
        let factors = re.cr.utf16.factors.as_ref().unwrap();
        assert!(!factors.spans());
        let text: Vec<_> = "aabc".encode_utf16().collect();
        assert!(factors.find(&text, 0).is_some());
        let found = re.find_from_utf16(&text, 0).next().unwrap();
        assert_eq!(found.range, 0..1);
        assert_eq!(found.captures, vec![Some(0..1)]);
    }
}
