//! Support for matchers compiled to Rust ahead of time.
//!
//! `codegen` turns the bytecode of a regex into Rust functions that do what the classical
//! backtracking interpreter does with it, instruction by instruction: the same choices in the
//! same order, the same backtrack entries, the same loop and capture group state. This module
//! holds what those functions share. Each function runs one `try_at_pos`: the top level or the
//! body of a lookaround, with the backtrack entries of enclosing ones below its base.
//!
//! `utf16` is whether the input decodes surrogate pairs (`Utf16Input`) or treats every code
//! unit as a character (`Ucs2Input`); `forward` is the direction of the cursor.

use crate::indexing::{code_point_from_surrogates, is_high_surrogate, is_low_surrogate};
use crate::matchers::{CharProperties, Utf16CharProperties};
#[cfg(not(feature = "std"))]
use alloc::vec::Vec;
use core::ops::Range;

pub use crate::exec::Steps;

/// A matcher compiled ahead of time: tries a match at \p pos of \p text, with \p bts for the
/// backtrack entries, counting backtracking in \p steps, and stores the ranges of the capture
/// groups in \p captures on success.
/// \return the end of the match.
pub type Attempt = fn(
    text: &[u16],
    pos: usize,
    bts: &mut Vec<Bt>,
    steps: &mut Steps,
    captures: &mut [Option<Range<usize>>],
) -> Option<usize>;

/// The position of a capture group bound that is not set.
pub const UNSET: usize = usize::MAX;

/// A capture group: the counterpart of `GroupData`, with `UNSET` for `None`.
#[derive(Debug, Copy, Clone)]
pub struct Group {
    pub start: usize,
    pub end: usize,
}

impl Group {
    const NEW: Group = Group {
        start: UNSET,
        end: UNSET,
    };
}

/// A loop: the counterpart of `LoopData`.
#[derive(Debug, Copy, Clone)]
pub struct Loop {
    pub iters: usize,
    pub entry: usize,
}

/// A backtrack entry: the counterpart of `BacktrackInsn`, with the label of the code to resume
/// at in place of an instruction pointer.
#[derive(Debug, Copy, Clone)]
pub enum Bt {
    /// `SetPosition`.
    Position { label: u32, pos: usize },
    /// `SetLoopData`.
    Loop { id: u16, data: Loop },
    /// `SetCaptureGroup`.
    Group { id: u16, data: Group },
    /// `EnterNonGreedyLoop`, with the label of the loop body.
    NonGreedyLoop {
        label: u32,
        id: u16,
        orig_pos: usize,
        data: Loop,
    },
    /// `GreedyLoop1Char`.
    GreedyLoop1Char { label: u32, min: usize, max: usize },
    /// `NonGreedyLoop1Char`.
    NonGreedyLoop1Char { label: u32, min: usize, max: usize },
    /// `GreedyLoop1Char` of a forward loop whose continuation must first consume a code unit
    /// that \p until accepts and that is a character by itself: positions short of that fail
    /// at once, so backtracking skips them.
    GreedyLoop1CharUntil {
        label: u32,
        min: usize,
        max: usize,
        until: fn(u16) -> bool,
    },
    /// `NonGreedyLoop1Char` with the skipping of `GreedyLoop1CharUntil`.
    NonGreedyLoop1CharUntil {
        label: u32,
        min: usize,
        max: usize,
        until: fn(u16) -> bool,
    },
}

/// The state of a match attempt with \p G capture groups and \p L loops.
#[derive(Debug)]
pub struct State<'b, const G: usize, const L: usize> {
    pub groups: [Group; G],
    pub loops: [Loop; L],
    pub bts: &'b mut Vec<Bt>,
    pub steps: &'b mut Steps,
}

/// \return the character right of \p pos and the position after it, as `next_right` reads it.
#[inline(always)]
pub fn next_right<const UTF16: bool>(text: &[u16], pos: usize) -> Option<(u32, usize)> {
    let u1 = *text.get(pos)?;
    if !UTF16 || !is_high_surrogate(u1) {
        return Some((u1 as u32, pos + 1));
    }
    match text.get(pos + 1) {
        Some(&u2) if is_low_surrogate(u2) => Some((code_point_from_surrogates(u1, u2), pos + 2)),
        _ => Some((u1 as u32, pos + 1)),
    }
}

/// \return the character left of \p pos and the position before it, as `next_left` reads it.
#[inline(always)]
pub fn next_left<const UTF16: bool>(text: &[u16], pos: usize) -> Option<(u32, usize)> {
    if pos == 0 {
        return None;
    }
    let u2 = *text.get(pos - 1)?;
    let pos = pos - 1;
    if !UTF16 || pos == 0 || !is_low_surrogate(u2) {
        return Some((u2 as u32, pos));
    }
    match text.get(pos - 1) {
        Some(&u1) if is_high_surrogate(u1) => Some((code_point_from_surrogates(u1, u2), pos - 1)),
        _ => Some((u2 as u32, pos)),
    }
}

/// \return the position after the character right of \p pos, as `next_right_pos` steps.
#[inline(always)]
pub fn next_right_pos<const UTF16: bool>(text: &[u16], pos: usize) -> Option<usize> {
    next_right::<UTF16>(text, pos).map(|(_, pos)| pos)
}

/// \return the position before the character left of \p pos, as `next_left_pos` steps.
#[inline(always)]
pub fn next_left_pos<const UTF16: bool>(text: &[u16], pos: usize) -> Option<usize> {
    next_left::<UTF16>(text, pos).map(|(_, pos)| pos)
}

/// \return the character in the direction of the cursor and the position past it.
#[inline(always)]
pub fn next<const UTF16: bool, const FORWARD: bool>(
    text: &[u16],
    pos: usize,
) -> Option<(u32, usize)> {
    if FORWARD {
        next_right::<UTF16>(text, pos)
    } else {
        next_left::<UTF16>(text, pos)
    }
}

/// \return whether \p c is a line terminator.
#[inline(always)]
pub fn is_line_terminator(c: u32) -> bool {
    Utf16CharProperties::is_line_terminator(c)
}

/// \return whether \p pos is a word boundary, as `WordBoundary` sees it.
#[inline(always)]
pub fn word_boundary<const UTF16: bool>(text: &[u16], pos: usize) -> bool {
    let prev =
        next_left::<UTF16>(text, pos).is_some_and(|(c, _)| Utf16CharProperties::is_word_char(c));
    let curr =
        next_right::<UTF16>(text, pos).is_some_and(|(c, _)| Utf16CharProperties::is_word_char(c));
    prev != curr
}

/// \return whether \p pos is a word boundary, as `WordBoundaryUnicodeICase` sees it.
#[inline(always)]
pub fn word_boundary_unicode_icase<const UTF16: bool>(text: &[u16], pos: usize) -> bool {
    let prev = next_left::<UTF16>(text, pos)
        .is_some_and(|(c, _)| Utf16CharProperties::is_word_char_unicode_icase(c));
    let curr = next_right::<UTF16>(text, pos)
        .is_some_and(|(c, _)| Utf16CharProperties::is_word_char_unicode_icase(c));
    prev != curr
}

/// \return whether a code point is in a sorted list of disjoint inclusive ranges.
#[inline]
pub fn in_ranges(ranges: &[(u32, u32)], c: u32) -> bool {
    ranges
        .binary_search_by(|&(first, last)| {
            if last < c {
                core::cmp::Ordering::Less
            } else if first > c {
                core::cmp::Ordering::Greater
            } else {
                core::cmp::Ordering::Equal
            }
        })
        .is_ok()
}

/// Drop the backtrack entries of \p bts above \p base, restoring the \p groups and \p loops
/// they saved without resuming at any, as exhausting them does.
#[cold]
#[inline(never)]
fn unwind(bts: &mut Vec<Bt>, groups: &mut [Group], loops: &mut [Loop], base: usize) {
    while bts.len() > base {
        match bts.pop() {
            Some(Bt::Loop { id, data }) => loops[id as usize] = data,
            Some(Bt::Group { id, data }) => groups[id as usize] = data,
            Some(Bt::NonGreedyLoop {
                id, orig_pos, data, ..
            }) => {
                loops[id as usize] = Loop {
                    entry: orig_pos,
                    ..data
                };
            }
            _ => {}
        }
    }
}

/// A set of code points too large to test range by range: a bitmap of those below 0x800, as
/// `CompiledBracket` keeps, and the sorted ranges of the rest.
#[derive(Debug)]
pub struct Class {
    pub dense: [u64; 32],
    pub ranges: &'static [(u32, u32)],
}

impl Class {
    #[inline(always)]
    pub fn contains(&self, c: u32) -> bool {
        match self.dense.get(c as usize / 64) {
            Some(word) => word >> (c % 64) & 1 != 0,
            None => in_ranges(self.ranges, c),
        }
    }
}

/// The outcome of the factor analysis of a regex, written out by `codegen::factors` so that
/// compiling the same pattern and flags again skips the analysis: the strings of unit sets
/// every match contains one of, each with where the pair of its sets a scan looks for starts.
/// The units searches keep to runs of come from the regex itself, which takes no analysis.
///
/// The bytes are numbers, seven bits a byte from the lowest, with the top bit set on all bytes
/// of a number but its last: the number of strings, then for each string where its pair
/// starts, the number of its sets and each set. A set is its number of ranges and, for each
/// range, twice how far its first unit is past the last unit of the range before (or from 0,
/// for the first range), one more if the range has more than one unit, and then for such a
/// range its number of units less two. Bytes past the end are not read, so the outcomes of
/// many regexes can share one table. A regex without such strings has none: its number of
/// strings is 0.
#[derive(Debug, Clone, Copy)]
pub struct Factors(pub &'static [u8]);

impl<'b, const G: usize, const L: usize> State<'b, G, L> {
    #[inline(always)]
    pub fn new(bts: &'b mut Vec<Bt>, steps: &'b mut Steps) -> Self {
        Self {
            groups: [Group::NEW; G],
            loops: [Loop { iters: 0, entry: 0 }; L],
            bts,
            steps,
        }
    }

    /// Store the capture groups in \p captures, as `successful_match` maps them.
    #[inline(always)]
    pub fn captures(&self, captures: &mut [Option<Range<usize>>]) {
        for (out, g) in captures.iter_mut().zip(self.groups.iter()) {
            *out = if g.start != UNSET && g.end != UNSET {
                Some(g.start..g.end)
            } else {
                None
            };
        }
    }

    /// Push the entry that restores capture group \p id, unless the function whose entries
    /// start at \p base has none: with nothing to backtrack to, it fails without reading the
    /// group again, and a lookaround that fails restores its groups itself.
    #[inline(always)]
    pub fn save_group(&mut self, base: usize, id: usize) {
        if self.bts.len() != base {
            self.bts.push(Bt::Group {
                id: id as u16,
                data: self.groups[id],
            });
        }
    }

    /// Enter loop \p id at \p pos, saving its data as `save_group` saves a group.
    #[inline(always)]
    fn prepare_to_enter_loop(&mut self, base: usize, id: usize, pos: usize) {
        if self.bts.len() != base {
            self.bts.push(Bt::Loop {
                id: id as u16,
                data: self.loops[id],
            });
        }
        self.loops[id].iters += 1;
        self.loops[id].entry = pos;
    }

    /// Decide whether loop \p id runs its body again at \p pos, as `run_loop` does, in the
    /// function whose entries start at \p base.
    /// \p enter is false only for a greedy loop whose body cannot match at \p pos: running it
    /// would fail and undo all it did, so the decision goes straight to what follows that.
    /// \return the label to go to, or None to backtrack.
    #[allow(clippy::too_many_arguments)]
    #[inline(always)]
    pub fn run_loop(
        &mut self,
        base: usize,
        id: usize,
        min_iters: usize,
        max_iters: usize,
        greedy: bool,
        pos: usize,
        body: u32,
        exit: u32,
        enter: bool,
    ) -> Option<u32> {
        let iteration = self.loops[id].iters;
        let do_taken = iteration < max_iters;
        let do_not_taken = iteration >= min_iters;
        if self.loops[id].entry == pos && iteration > min_iters {
            return None;
        }
        match (do_taken, do_not_taken) {
            (false, false) => None,
            (false, true) => Some(exit),
            (true, false) if !enter => None,
            (true, false) => {
                self.prepare_to_enter_loop(base, id, pos);
                Some(body)
            }
            (true, true) if greedy && !enter => Some(exit),
            (true, true) if !greedy => {
                let orig_pos = self.loops[id].entry;
                self.loops[id].entry = pos;
                self.bts.push(Bt::NonGreedyLoop {
                    label: body,
                    id: id as u16,
                    orig_pos,
                    data: self.loops[id],
                });
                Some(exit)
            }
            (true, true) => {
                self.bts.push(Bt::Position { label: exit, pos });
                self.prepare_to_enter_loop(base, id, pos);
                Some(body)
            }
        }
    }

    /// Match a backreference to capture group \p id at \p pos, as `BackRef` does.
    /// \return the position past it, or None to backtrack.
    #[inline]
    pub fn backref<const UTF16: bool, const FORWARD: bool>(
        &self,
        text: &[u16],
        pos: usize,
        id: usize,
        icase: bool,
        unicode: bool,
    ) -> Option<usize> {
        let g = self.groups[id];
        if g.start == UNSET || g.end == UNSET {
            return Some(pos);
        }
        if icase {
            return backref_icase::<UTF16, FORWARD>(text, pos, g.start..g.end, unicode);
        }
        let len = g.end - g.start;
        let (start, end) = if FORWARD {
            if text.len() - pos < len {
                return None;
            }
            (pos, pos + len)
        } else {
            if pos < len {
                return None;
            }
            (pos - len, pos)
        };
        if text[start..end] != text[g.start..g.end] {
            return None;
        }
        Some(if FORWARD { end } else { start })
    }

    /// Restore state from the backtrack entries above \p base, as `try_backtrack` does.
    /// \return the label and position to resume at, or None once they are exhausted or the
    /// search is to stop.
    #[inline]
    pub fn backtrack<const UTF16: bool, const FORWARD: bool>(
        &mut self,
        text: &[u16],
        base: usize,
    ) -> Option<(u32, usize)> {
        if self.bts.len() == base {
            return None;
        }
        if self.steps.step() {
            unwind(self.bts, &mut self.groups, &mut self.loops, base);
            return None;
        }
        loop {
            if self.bts.len() == base {
                return None;
            }
            let last = self.bts.len() - 1;
            match self.bts[last] {
                Bt::Position { label, pos } => {
                    self.bts.truncate(last);
                    return Some((label, pos));
                }
                Bt::Loop { id, data } => {
                    self.loops[id as usize] = data;
                    self.bts.truncate(last);
                }
                Bt::Group { id, data } => {
                    self.groups[id as usize] = data;
                    self.bts.truncate(last);
                }
                Bt::NonGreedyLoop {
                    label,
                    id,
                    orig_pos,
                    data,
                } => {
                    let pos = data.entry;
                    self.bts[last] = Bt::Loop {
                        id,
                        data: Loop {
                            entry: orig_pos,
                            ..data
                        },
                    };
                    self.loops[id as usize] = data;
                    self.prepare_to_enter_loop(base, id as usize, pos);
                    return Some((label, pos));
                }
                Bt::GreedyLoop1Char { label, min, max } => {
                    if max == min {
                        self.bts.truncate(last);
                        continue;
                    }
                    let newmax = if FORWARD {
                        next_left_pos::<UTF16>(text, max)
                    } else {
                        next_right_pos::<UTF16>(text, max)
                    };
                    let newmax = newmax.expect("Should always be able to advance since min != max");
                    self.bts[last] = Bt::GreedyLoop1Char {
                        label,
                        min,
                        max: newmax,
                    };
                    return Some((label, newmax));
                }
                Bt::NonGreedyLoop1Char { label, min, max } => {
                    if max == min {
                        self.bts.truncate(last);
                        continue;
                    }
                    let newmin = if FORWARD {
                        next_right_pos::<UTF16>(text, min)
                    } else {
                        next_left_pos::<UTF16>(text, min)
                    };
                    let newmin = newmin.expect("Should always be able to advance since min != max");
                    self.bts[last] = Bt::NonGreedyLoop1Char {
                        label,
                        min: newmin,
                        max,
                    };
                    return Some((label, newmin));
                }
                Bt::GreedyLoop1CharUntil {
                    label,
                    min,
                    max,
                    until,
                } => {
                    // Every unit \p until accepts starts a character, so stepping by units
                    // stops at the same positions stepping by characters would.
                    match text[min..max].iter().rposition(|&u| until(u)) {
                        Some(k) => {
                            let newmax = min + k;
                            self.bts[last] = Bt::GreedyLoop1CharUntil {
                                label,
                                min,
                                max: newmax,
                                until,
                            };
                            return Some((label, newmax));
                        }
                        None => self.bts.truncate(last),
                    }
                }
                Bt::NonGreedyLoop1CharUntil {
                    label,
                    min,
                    max,
                    until,
                } => {
                    let from = min + 1;
                    let to = max.min(text.len().saturating_sub(1));
                    let found = text
                        .get(from..=to)
                        .and_then(|units| units.iter().position(|&u| until(u)));
                    match found {
                        Some(k) => {
                            let newmin = from + k;
                            self.bts[last] = Bt::NonGreedyLoop1CharUntil {
                                label,
                                min: newmin,
                                max,
                                until,
                            };
                            return Some((label, newmin));
                        }
                        None => self.bts.truncate(last),
                    }
                }
            }
        }
    }
}

/// `backref_icase`: compare the characters of \p range with those at \p pos, case folded.
fn backref_icase<const UTF16: bool, const FORWARD: bool>(
    text: &[u16],
    mut pos: usize,
    range: Range<usize>,
    unicode: bool,
) -> Option<usize> {
    let reference = &text[range];
    let mut ref_pos = if FORWARD { 0 } else { reference.len() };
    while let Some((c1, next_ref)) = next::<UTF16, FORWARD>(reference, ref_pos) {
        ref_pos = next_ref;
        let (c2, next_pos) = next::<UTF16, FORWARD>(text, pos)?;
        pos = next_pos;
        if !(c1 == c2
            || crate::unicode::fold_code_point(c1, unicode)
                == crate::unicode::fold_code_point(c2, unicode))
        {
            return None;
        }
    }
    Some(pos)
}
