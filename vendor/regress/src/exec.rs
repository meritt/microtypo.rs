//! Execution engine bits.

use crate::api::Match;
use crate::insn::CompiledRegex;
use crate::position::PositionType;
use core::ops::Range;

/// How many backtracking steps a search takes between asking whether to stop.
const STEPS_PER_CHECK: u32 = 1 << 16;

/// The backtracking steps of a search, counted to ask a function now and then whether to stop.
/// A search that stops finds nothing more, as if every attempt failed.
#[derive(Debug, Clone, Copy)]
pub struct Steps {
    left: u32,
    stop: Option<fn() -> bool>,
    stopped: bool,
}

impl Default for Steps {
    fn default() -> Self {
        Self::new(None)
    }
}

impl Steps {
    /// Steps that ask \p stop, if any.
    pub const fn new(stop: Option<fn() -> bool>) -> Self {
        Steps {
            left: STEPS_PER_CHECK,
            stop,
            stopped: false,
        }
    }

    /// Count a backtracking step.
    /// \return whether the search is to stop.
    #[inline(always)]
    pub fn step(&mut self) -> bool {
        self.left -= 1;
        self.left == 0 && self.check()
    }

    /// \return whether the search has stopped.
    pub fn stopped(&self) -> bool {
        self.stopped
    }

    #[cold]
    #[inline(never)]
    fn check(&mut self) -> bool {
        if !self.stopped && !self.stop.is_some_and(|stop| stop()) {
            self.left = STEPS_PER_CHECK;
            return false;
        }
        // Every later step stops as well.
        self.stopped = true;
        self.left = 1;
        true
    }
}

/// A trait for finding the next match in a regex.
/// This is broken out from Executor to avoid needing to thread lifetimes
/// around.
pub trait MatchProducer: core::fmt::Debug {
    /// The position type of our indexer.
    type Position: PositionType;

    /// \return an initial position for the given start offset.
    fn initial_position(&self, offset: usize) -> Option<Self::Position>;

    /// Attempt to match at the given location.
    /// \return either the Match and the position to start looking for the next
    /// match, or None on failure.
    fn next_match(
        &mut self,
        pos: Self::Position,
        next_start: &mut Option<Self::Position>,
    ) -> Option<Match>;

    /// Like `next_match`, but store the ranges of the capture groups in \p captures, one slot
    /// per group, rather than in a fresh `Match`.
    /// \return the range of the match.
    fn next_match_into(
        &mut self,
        pos: Self::Position,
        next_start: &mut Option<Self::Position>,
        captures: &mut [Option<Range<usize>>],
    ) -> Option<Range<usize>> {
        let m = self.next_match(pos, next_start)?;
        for (out, group) in captures.iter_mut().zip(m.captures) {
            *out = group;
        }
        Some(m.range)
    }

    /// Attempt a match starting exactly at \p pos, as a sticky search does, and store the ranges
    /// of the capture groups in \p captures.
    /// \return the range of the match.
    fn attempt_into(
        &mut self,
        pos: Self::Position,
        captures: &mut [Option<Range<usize>>],
    ) -> Option<Range<usize>> {
        let mut next_start = None;
        let start = self.next_match_into(pos, &mut next_start, captures)?;
        let offset = self.initial_position(start.start)?;
        (offset == pos).then_some(start)
    }

    /// Ask \p stop, every so many backtracking steps, whether to stop searching.
    fn stop_when(&mut self, _stop: fn() -> bool) {}

    /// \return whether searching stopped because `stop_when` said so.
    fn stopped(&self) -> bool {
        false
    }
}

/// A trait for executing a regex.
pub trait Executor<'r, 't>: MatchProducer {
    /// The ASCII variant.
    type AsAscii: Executor<'r, 't>;

    /// Construct a new Executor.
    fn new(re: &'r CompiledRegex, text: &'t str) -> Self;
}

/// A struct which enables iteration over matches.
#[derive(Debug)]
pub struct Matches<Producer: MatchProducer> {
    mp: Producer,
    position: Option<Producer::Position>,
}

impl<Producer: MatchProducer> Matches<Producer> {
    pub fn new(mp: Producer, start: usize) -> Self {
        let position = mp.initial_position(start);
        Matches { mp, position }
    }

    /// \return the first match at or after the offset \p start, as a fresh iterator from there
    /// would find it, but reusing this iterator's matcher state.
    pub fn find_at(&mut self, start: usize) -> Option<Match> {
        self.position = self.mp.initial_position(start);
        self.next()
    }

    /// Like `find_at`, but store the ranges of the capture groups in \p captures, one slot per
    /// group, rather than allocate a `Match`.
    /// \return the range of the match.
    pub fn find_at_into(
        &mut self,
        start: usize,
        captures: &mut [Option<Range<usize>>],
    ) -> Option<Range<usize>> {
        self.position = self.mp.initial_position(start);
        let pos = self.position?;
        self.mp.next_match_into(pos, &mut self.position, captures)
    }

    /// Like `find_at_into`, but only a match that starts exactly at the offset \p start counts,
    /// and no other start is tried: the search of a sticky expression.
    /// \return the range of the match.
    pub fn attempt_at_into(
        &mut self,
        start: usize,
        captures: &mut [Option<Range<usize>>],
    ) -> Option<Range<usize>> {
        self.position = None;
        let pos = self.mp.initial_position(start)?;
        self.mp.attempt_into(pos, captures)
    }

    /// Ask \p stop, every so many backtracking steps, whether to stop searching. Once it says
    /// so, every attempt that backtracks fails, and `stopped` tells.
    pub fn stop_when(&mut self, stop: fn() -> bool) {
        self.mp.stop_when(stop)
    }

    /// \return whether searching stopped because the function `stop_when` set said so.
    pub fn stopped(&self) -> bool {
        self.mp.stopped()
    }
}

impl<Producer: MatchProducer> Iterator for Matches<Producer> {
    type Item = Match;
    fn next(&mut self) -> Option<Self::Item> {
        let pos = self.position?;
        self.mp.next_match(pos, &mut self.position)
    }
}
