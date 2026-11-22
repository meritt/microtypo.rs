//! Classical backtracking execution engine

use crate::api::Match;
use crate::bytesearch;
use crate::cursor;
use crate::cursor::{Backward, Direction, Forward};
use crate::exec;
use crate::indexing::{AsciiInput, ElementType, InputIndexer, Utf8Input};
use crate::insn::StartPredicate;
use crate::insn::{CompiledRegex, Insn, LoopFields};
use crate::matchers;
use crate::matchers::CharProperties;
use crate::position::PositionType;
use crate::scm;
use crate::scm::SingleCharMatcher;
use crate::types::{CaptureGroupID, GroupData, IP, LoopData, LoopID, MAX_CAPTURE_GROUPS};
use crate::util::DebugCheckIndex;
#[cfg(not(feature = "std"))]
use alloc::vec::Vec;
use core::ops::Range;

#[derive(Clone, Debug)]
enum BacktrackInsn<Input: InputIndexer> {
    /// Nothing more to backtrack.
    /// This "backstops" our stack.
    Exhausted,

    /// Restore the IP and position.
    SetPosition { ip: IP, pos: Input::Position },

    SetLoopData {
        id: LoopID,
        data: LoopData<Input::Position>,
    },

    SetCaptureGroup {
        id: CaptureGroupID,
        data: GroupData<Input::Position>,
    },

    EnterNonGreedyLoop {
        // The IP of the loop.
        // This is guaranteed to point to an EnterLoopInsn.
        ip: IP,
        // The input position of the loop before entering it.
        // This is used to set up backtracking that restores this position.
        orig_pos: Input::Position,
        data: LoopData<Input::Position>,
    },

    GreedyLoop1Char {
        continuation: IP,
        min: Input::Position,
        max: Input::Position,
    },

    NonGreedyLoop1Char {
        continuation: IP,
        pos: Input::Position,
        remaining: usize,
        loop_ip: IP,
    },
}

#[derive(Debug, Default)]
struct State<Position: PositionType> {
    loops: Vec<LoopData<Position>>,
    groups: Vec<GroupData<Position>>,
}

#[derive(Debug)]
pub(crate) struct MatchAttempter<'a, Input: InputIndexer> {
    re: &'a CompiledRegex,
    bts: Vec<BacktrackInsn<Input>>,
    s: State<Input::Position>,
    // Storage lookarounds reuse across attempts: emptied backtrack stacks, one per nesting
    // level, and a stack of the capture groups each enclosing lookaround saved.
    spare_bts: Vec<Vec<BacktrackInsn<Input>>>,
    saved_groups: Vec<GroupData<Input::Position>>,
    // The backtrack entries of a matcher compiled ahead of time.
    #[cfg(feature = "utf16")]
    aot_bts: Vec<crate::aot::Bt>,
    entry: Input::Position,
    steps: exec::Steps,
}

impl<Input: InputIndexer> Drop for MatchAttempter<'_, Input> {
    fn drop(&mut self) {
        if self.bts.capacity() > MAX_POOLED_BACKTRACK {
            return;
        }
        #[cfg(feature = "utf16")]
        if self.aot_bts.capacity() > MAX_POOLED_BACKTRACK {
            return;
        }
        let mut buffers = Buffers {
            bts: core::mem::take(&mut self.bts),
            loops: core::mem::take(&mut self.s.loops),
            groups: core::mem::take(&mut self.s.groups),
            spare_bts: core::mem::take(&mut self.spare_bts),
            saved_groups: core::mem::take(&mut self.saved_groups),
            #[cfg(feature = "utf16")]
            aot_bts: core::mem::take(&mut self.aot_bts),
        };
        buffers.bts.clear();
        buffers.loops.clear();
        buffers.groups.clear();
        buffers.saved_groups.clear();
        #[cfg(feature = "utf16")]
        buffers.aot_bts.clear();
        buffers
            .spare_bts
            .retain(|spare| spare.capacity() <= MAX_POOLED_BACKTRACK);
        for spare in &mut buffers.spare_bts {
            spare.clear();
        }
        Input::give_buffers(buffers);
    }
}

/// A search's state storage, emptied, as one search hands it to the next on the same thread.
pub struct Buffers<Input: InputIndexer> {
    bts: Vec<BacktrackInsn<Input>>,
    loops: Vec<LoopData<Input::Position>>,
    groups: Vec<GroupData<Input::Position>>,
    spare_bts: Vec<Vec<BacktrackInsn<Input>>>,
    saved_groups: Vec<GroupData<Input::Position>>,
    #[cfg(feature = "utf16")]
    aot_bts: Vec<crate::aot::Bt>,
}

impl<Input: InputIndexer> Default for Buffers<Input> {
    fn default() -> Self {
        Self {
            bts: Vec::new(),
            loops: Vec::new(),
            groups: Vec::new(),
            spare_bts: Vec::new(),
            saved_groups: Vec::new(),
            #[cfg(feature = "utf16")]
            aot_bts: Vec::new(),
        }
    }
}

/// Storage past this many backtrack entries is freed rather than kept for the next search.
const MAX_POOLED_BACKTRACK: usize = 1 << 12;

impl<'a, Input: InputIndexer> MatchAttempter<'a, Input> {
    /// The state is set up by `prepare`, in storage an earlier search on this thread may have
    /// left, so a search that finds no place to try costs nothing.
    pub(crate) fn new(re: &'a CompiledRegex, entry: Input::Position) -> Self {
        let buffers = Input::take_buffers().unwrap_or_default();
        Self {
            re,
            bts: buffers.bts,
            s: State {
                loops: buffers.loops,
                groups: buffers.groups,
            },
            spare_bts: buffers.spare_bts,
            saved_groups: buffers.saved_groups,
            #[cfg(feature = "utf16")]
            aot_bts: buffers.aot_bts,
            entry,
            steps: exec::Steps::default(),
        }
    }

    /// Set up the state, once, before the first `try_at_pos`.
    #[inline(always)]
    fn prepare(&mut self) {
        if self.bts.is_empty() {
            self.bts.reserve(32);
            self.bts.push(BacktrackInsn::Exhausted);
            self.s.loops.clear();
            self.s
                .loops
                .resize(self.re.loops as usize, LoopData::new(self.entry));
            self.s.groups.clear();
            self.s
                .groups
                .resize(self.re.groups as usize, GroupData::new());
        }
    }

    #[inline(always)]
    fn push_backtrack(&mut self, bt: BacktrackInsn<Input>) {
        self.bts.push(bt)
    }

    #[inline(always)]
    fn pop_backtrack(&mut self) {
        // Note we never pop the last instruction so this will never be empty.
        debug_assert!(!self.bts.is_empty());
        if cfg!(feature = "prohibit-unsafe") {
            self.bts.pop();
        } else {
            unsafe { self.bts.set_len(self.bts.len() - 1) }
        }
    }

    fn prepare_to_enter_loop(
        bts: &mut Vec<BacktrackInsn<Input>>,
        pos: Input::Position,
        loop_fields: &LoopFields,
        loop_data: &mut LoopData<Input::Position>,
    ) {
        bts.push(BacktrackInsn::SetLoopData {
            id: loop_fields.loop_id,
            data: *loop_data,
        });
        loop_data.iters += 1;
        loop_data.entry = pos;
    }

    fn run_loop(
        &mut self,
        loop_fields: &'a LoopFields,
        pos: Input::Position,
        ip: IP,
    ) -> Option<IP> {
        let loop_data = &mut self.s.loops[loop_fields.loop_id as usize];
        let iteration = loop_data.iters;

        let do_taken = iteration < loop_fields.max_iters;
        let do_not_taken = iteration >= loop_fields.min_iters;

        let loop_taken_ip = ip + 1;
        let loop_not_taken_ip = loop_fields.exit as IP;

        // If we have looped more than the minimum number of iterations, reject empty
        // matches. ES6 21.2.2.5.1 Note 4: "once the minimum number of
        // repetitions has been satisfied, any more expansions of Atom that match the
        // empty character sequence are not considered for further repetitions."
        if loop_data.entry == pos && iteration > loop_fields.min_iters {
            return None;
        }

        match (do_taken, do_not_taken) {
            (false, false) => {
                // No arms viable.
                None
            }
            (false, true) => {
                // Only skipping is viable.
                Some(loop_not_taken_ip)
            }
            (true, false) => {
                // Only entering is viable.
                MatchAttempter::prepare_to_enter_loop(&mut self.bts, pos, loop_fields, loop_data);
                Some(loop_taken_ip)
            }
            (true, true) if !loop_fields.greedy => {
                // Both arms are viable; backtrack into the loop.
                let orig_pos = loop_data.entry;
                loop_data.entry = pos;
                self.bts.push(BacktrackInsn::EnterNonGreedyLoop {
                    ip,
                    orig_pos,
                    data: *loop_data,
                });
                Some(loop_not_taken_ip)
            }
            (true, true) => {
                debug_assert!(loop_fields.greedy, "Should be greedy");
                // Both arms are viable; backtrack out of the loop.
                self.bts.push(BacktrackInsn::SetPosition {
                    ip: loop_not_taken_ip,
                    pos,
                });
                MatchAttempter::prepare_to_enter_loop(&mut self.bts, pos, loop_fields, loop_data);
                Some(loop_taken_ip)
            }
        }
    }

    // Drive the loop up to \p max times.
    // \return the position (min, max), or None on failure.
    #[inline(always)]
    fn run_scm_loop_impl<Dir: Direction, Scm: SingleCharMatcher<Input, Dir>>(
        input: &Input,
        mut pos: Input::Position,
        min: usize,
        max: usize,
        dir: Dir,
        matcher: Scm,
    ) -> Option<(Input::Position, Input::Position)> {
        debug_assert!(min <= max, "min should be <= max");
        // Drive the iteration min times.
        // That tells us the min position.
        for _ in 0..min {
            if !matcher.matches(input, dir, &mut pos) {
                return None;
            }
        }
        let min_pos = pos;

        // Drive it up to the max.
        for _ in 0..(max - min) {
            let saved = pos;
            if !matcher.matches(input, dir, &mut pos) {
                pos = saved;
                break;
            }
        }
        let max_pos = pos;
        Some((min_pos, max_pos))
    }

    // Compute the maximum position from a starting position, up to a limit.
    // This is used for lazy computation in non-greedy loops.
    fn compute_max_pos<Dir: Direction, Scm: SingleCharMatcher<Input, Dir>>(
        input: &Input,
        mut pos: Input::Position,
        limit: usize,
        dir: Dir,
        matcher: Scm,
    ) -> Input::Position {
        for _ in 0..limit {
            let saved = pos;
            if !matcher.matches(input, dir, &mut pos) {
                pos = saved;
                break;
            }
        }
        pos
    }

    // Helper function to extract the duplicated match blocks that handle different instruction types
    // with different matcher functions. This significantly reduces code duplication and compile times.
    fn with_scm_loop_impl<Dir: Direction>(
        re: &CompiledRegex,
        input: &Input,
        pos: Input::Position,
        min: usize,
        max: usize,
        dir: Dir,
        ip: IP,
    ) -> Option<(Input::Position, Input::Position)> {
        match re.insns.iat(ip + 1) {
            &Insn::Char(c) => {
                let c = <<Input as InputIndexer>::Element as ElementType>::try_from(c)?;
                Self::run_scm_loop_impl(input, pos, min, max, dir, scm::Char { c })
            }
            &Insn::Bracket(idx) => {
                let bc = &re.brackets[idx];
                Self::run_scm_loop_impl(input, pos, min, max, dir, scm::Bracket { bc })
            }
            Insn::AsciiBracket(bitmap) => Self::run_scm_loop_impl(
                input,
                pos,
                min,
                max,
                dir,
                scm::MatchByteSet { bytes: bitmap },
            ),
            Insn::MatchAny => {
                Self::run_scm_loop_impl(input, pos, min, max, dir, scm::MatchAny::new())
            }
            Insn::MatchAnyExceptLineTerminator => Self::run_scm_loop_impl(
                input,
                pos,
                min,
                max,
                dir,
                scm::MatchAnyExceptLineTerminator::new(),
            ),
            Insn::CharSet(chars) => {
                Self::run_scm_loop_impl(input, pos, min, max, dir, scm::CharSet { chars })
            }
            &Insn::ByteSet2(bytes) => {
                Self::run_scm_loop_impl(input, pos, min, max, dir, scm::MatchByteArraySet(bytes))
            }
            &Insn::ByteSet3(bytes) => {
                Self::run_scm_loop_impl(input, pos, min, max, dir, scm::MatchByteArraySet(bytes))
            }
            &Insn::ByteSet4(bytes) => {
                Self::run_scm_loop_impl(input, pos, min, max, dir, scm::MatchByteArraySet(bytes))
            }
            Insn::ByteSeq1(bytes) => {
                Self::run_scm_loop_impl(input, pos, min, max, dir, scm::MatchByteSeq(bytes))
            }
            Insn::ByteSeq2(bytes) => {
                Self::run_scm_loop_impl(input, pos, min, max, dir, scm::MatchByteSeq(bytes))
            }
            Insn::ByteSeq3(bytes) => {
                Self::run_scm_loop_impl(input, pos, min, max, dir, scm::MatchByteSeq(bytes))
            }
            Insn::ByteSeq4(bytes) => {
                Self::run_scm_loop_impl(input, pos, min, max, dir, scm::MatchByteSeq(bytes))
            }
            Insn::ByteSeq5(bytes) => {
                Self::run_scm_loop_impl(input, pos, min, max, dir, scm::MatchByteSeq(bytes))
            }
            Insn::ByteSeq6(bytes) => {
                Self::run_scm_loop_impl(input, pos, min, max, dir, scm::MatchByteSeq(bytes))
            }
            _ => {
                unreachable!("Missing SCM: {:?}", re.insns.iat(ip + 1));
            }
        }
    }

    // Helper function for compute_max_pos to avoid duplication
    fn with_scm_compute_max<Dir: Direction>(
        re: &CompiledRegex,
        input: &Input,
        pos: Input::Position,
        limit: usize,
        dir: Dir,
        ip: IP,
    ) -> Option<Input::Position> {
        let result = match re.insns.iat(ip + 1) {
            &Insn::Char(c) => {
                let c = <<Input as InputIndexer>::Element as ElementType>::try_from(c)?;
                Self::compute_max_pos(input, pos, limit, dir, scm::Char { c })
            }
            &Insn::Bracket(idx) => {
                let bc = &re.brackets[idx];
                Self::compute_max_pos(input, pos, limit, dir, scm::Bracket { bc })
            }
            Insn::AsciiBracket(bitmap) => {
                Self::compute_max_pos(input, pos, limit, dir, scm::MatchByteSet { bytes: bitmap })
            }
            Insn::MatchAny => Self::compute_max_pos(input, pos, limit, dir, scm::MatchAny::new()),
            Insn::MatchAnyExceptLineTerminator => Self::compute_max_pos(
                input,
                pos,
                limit,
                dir,
                scm::MatchAnyExceptLineTerminator::new(),
            ),
            Insn::CharSet(chars) => {
                Self::compute_max_pos(input, pos, limit, dir, scm::CharSet { chars })
            }
            &Insn::ByteSet2(bytes) => {
                Self::compute_max_pos(input, pos, limit, dir, scm::MatchByteArraySet(bytes))
            }
            &Insn::ByteSet3(bytes) => {
                Self::compute_max_pos(input, pos, limit, dir, scm::MatchByteArraySet(bytes))
            }
            &Insn::ByteSet4(bytes) => {
                Self::compute_max_pos(input, pos, limit, dir, scm::MatchByteArraySet(bytes))
            }
            Insn::ByteSeq1(bytes) => {
                Self::compute_max_pos(input, pos, limit, dir, scm::MatchByteSeq(bytes))
            }
            Insn::ByteSeq2(bytes) => {
                Self::compute_max_pos(input, pos, limit, dir, scm::MatchByteSeq(bytes))
            }
            Insn::ByteSeq3(bytes) => {
                Self::compute_max_pos(input, pos, limit, dir, scm::MatchByteSeq(bytes))
            }
            Insn::ByteSeq4(bytes) => {
                Self::compute_max_pos(input, pos, limit, dir, scm::MatchByteSeq(bytes))
            }
            Insn::ByteSeq5(bytes) => {
                Self::compute_max_pos(input, pos, limit, dir, scm::MatchByteSeq(bytes))
            }
            Insn::ByteSeq6(bytes) => {
                Self::compute_max_pos(input, pos, limit, dir, scm::MatchByteSeq(bytes))
            }
            _ => {
                unreachable!("Missing SCM: {:?}", re.insns.iat(ip + 1));
            }
        };
        Some(result)
    }

    // Given that ip points at a loop whose body matches exactly one character, run
    // a "single character loop". The big idea here is that we don't need to save
    // our position every iteration: we know that our loop body matches a single
    // character so we can backtrack by matching a character backwards.
    // \return the next IP, or None if the loop failed.
    #[allow(clippy::too_many_arguments)]
    fn run_scm_loop<Dir: Direction>(
        &mut self,
        input: &Input,
        dir: Dir,
        pos: &mut Input::Position,
        min: usize,
        max: usize,
        ip: IP,
        greedy: bool,
        possessive: bool,
    ) -> Option<IP> {
        let (min_pos, max_pos) = if greedy {
            Self::with_scm_loop_impl(self.re, input, *pos, min, max, dir, ip)?
        } else {
            Self::with_scm_loop_impl(self.re, input, *pos, min, min, dir, ip)?
        };

        debug_assert!(
            if Dir::FORWARD {
                min_pos <= max_pos
            } else {
                min_pos >= max_pos
            },
            "min should be <= (>=) max if cursor is tracking forwards (backwards)"
        );

        // Oh no where is the continuation? It's one past the loop body, which is one
        // past the loop. Strap in!
        let continuation = ip + 2;
        if !possessive {
            if greedy && min_pos != max_pos {
                self.bts.push(BacktrackInsn::GreedyLoop1Char {
                    continuation,
                    min: min_pos,
                    max: max_pos,
                });
            } else if !greedy && min < max {
                self.bts.push(BacktrackInsn::NonGreedyLoop1Char {
                    continuation,
                    pos: min_pos,
                    remaining: max - min,
                    loop_ip: ip,
                });
            }
        }

        // Start at the max (min) if greedy (nongreedy).
        *pos = if greedy { max_pos } else { min_pos };
        Some(continuation)
    }

    // Run a lookaround instruction, which is either forwards or backwards
    // (according to Direction). The half-open range
    // start_group..end_group is the range of contained capture groups.
    // \return whether we matched and negate was false, or did not match but negate
    // is true.
    fn run_lookaround<Dir: Direction>(
        &mut self,
        input: &Input,
        ip: IP,
        pos: Input::Position,
        start_group: CaptureGroupID,
        end_group: CaptureGroupID,
        negate: bool,
    ) -> bool {
        // Copy capture groups, because if the match fails (or if we are inverted)
        // we need to restore these.
        let range = (start_group as usize)..(end_group as usize);
        // Temporarily defeat backtracking.
        let saved_at = self.saved_groups.len();
        self.saved_groups
            .extend_from_slice(self.s.groups.iat(range.clone()));

        // Start with an "empty" backtrack stack.
        let mut saved_bts = self.spare_bts.pop().unwrap_or_default();
        saved_bts.push(BacktrackInsn::Exhausted);
        core::mem::swap(&mut self.bts, &mut saved_bts);

        // Enter into the lookaround's instruction stream.
        let matched = self.try_at_pos(*input, ip, pos, Dir::new()).is_some();

        // Put back our bts.
        core::mem::swap(&mut self.bts, &mut saved_bts);
        saved_bts.clear();
        self.spare_bts.push(saved_bts);

        // If we are a positive lookahead that successfully matched, retain the
        // capture groups (but we need to set up backtracking). Otherwise restore
        // them.
        if matched && !negate {
            for idx in 0..range.len() {
                debug_assert!(idx + (start_group as usize) < MAX_CAPTURE_GROUPS);
                let data = self.saved_groups[saved_at + idx];
                self.push_backtrack(BacktrackInsn::SetCaptureGroup {
                    id: (idx as CaptureGroupID) + start_group,
                    data,
                });
            }
        } else {
            self.s.groups[range].copy_from_slice(&self.saved_groups[saved_at..]);
        }
        self.saved_groups.truncate(saved_at);
        matched != negate
    }

    /// Drop the backtrack entries, restoring the state they saved without resuming at any, as
    /// exhausting them does.
    #[cold]
    #[inline(never)]
    fn unwind(&mut self) {
        while self.bts.len() > 1 {
            match self.bts.pop() {
                Some(BacktrackInsn::SetLoopData { id, data }) => {
                    *self.s.loops.mat(id as usize) = data;
                }
                Some(BacktrackInsn::SetCaptureGroup { id, data }) => {
                    *self.s.groups.mat(id as usize) = data;
                }
                Some(BacktrackInsn::EnterNonGreedyLoop { ip, orig_pos, data }) => {
                    let loop_fields = match &self.re.insns.iat(ip) {
                        Insn::EnterLoop(loop_fields) => loop_fields,
                        _ => rs_unreachable!("EnterNonGreedyLoop must point at a loop instruction"),
                    };
                    *self.s.loops.mat(loop_fields.loop_id as usize) = LoopData {
                        entry: orig_pos,
                        ..data
                    };
                }
                _ => {}
            }
        }
    }

    /// Attempt to backtrack.
    /// \return true if we backtracked, false if we exhaust the backtrack stack.
    fn try_backtrack<Dir: Direction>(
        &mut self,
        input: &Input,
        ip: &mut IP,
        pos: &mut Input::Position,
        dir: Dir,
    ) -> bool {
        loop {
            // We always have a single Exhausted instruction backstopping our stack,
            // so we do not need to check for empty bts.
            debug_assert!(!self.bts.is_empty(), "Backtrack stack should not be empty");
            let bt = match self.bts.last_mut() {
                Some(bt) => bt,
                None => rs_unreachable!("BT stack should never be empty"),
            };
            match bt {
                BacktrackInsn::Exhausted => return false,

                BacktrackInsn::SetPosition {
                    ip: saved_ip,
                    pos: saved_pos,
                } => {
                    *ip = *saved_ip;
                    *pos = *saved_pos;
                    self.pop_backtrack();
                    return true;
                }
                BacktrackInsn::SetLoopData { id, data } => {
                    *self.s.loops.mat(*id as usize) = *data;
                    self.pop_backtrack();
                }
                BacktrackInsn::SetCaptureGroup { id, data } => {
                    *self.s.groups.mat(*id as usize) = *data;
                    self.pop_backtrack();
                }

                &mut BacktrackInsn::EnterNonGreedyLoop {
                    ip: loop_ip,
                    orig_pos,
                    data,
                } => {
                    *ip = loop_ip + 1;
                    *pos = data.entry;
                    let loop_fields = match &self.re.insns.iat(loop_ip) {
                        Insn::EnterLoop(loop_fields) => loop_fields,
                        _ => rs_unreachable!("EnterNonGreedyLoop must point at a loop instruction"),
                    };
                    let loop_data = self.s.loops.mat(loop_fields.loop_id as usize);
                    // Need to restore the position should we backtrack out of the loop (#131).
                    *bt = BacktrackInsn::SetLoopData {
                        id: loop_fields.loop_id,
                        data: LoopData {
                            entry: orig_pos,
                            ..data
                        },
                    };
                    *loop_data = data;
                    MatchAttempter::prepare_to_enter_loop(
                        &mut self.bts,
                        *pos,
                        loop_fields,
                        loop_data,
                    );
                    return true;
                }

                BacktrackInsn::GreedyLoop1Char {
                    continuation,
                    min,
                    max,
                } => {
                    // The match failed at the max location.
                    debug_assert!(
                        if Dir::FORWARD { max >= min } else { max <= min },
                        "max should be >= min (or <= if tracking backwards)"
                    );
                    // If min is equal to max, there is no more backtracking to be done;
                    // otherwise move opposite the direction of the cursor.
                    if *max == *min {
                        // We have backtracked this loop as far as possible.
                        self.bts.pop();
                        continue;
                    }
                    let newmax = if Dir::FORWARD {
                        input.next_left_pos(*max)
                    } else {
                        input.next_right_pos(*max)
                    };
                    if let Some(newmax) = newmax {
                        *pos = newmax;
                        *max = newmax;
                    } else {
                        rs_unreachable!("Should always be able to advance since min != max")
                    }
                    *ip = *continuation;
                    return true;
                }

                BacktrackInsn::NonGreedyLoop1Char {
                    continuation,
                    pos: current,
                    remaining,
                    loop_ip,
                } => {
                    if *remaining == 0 {
                        self.bts.pop();
                        continue;
                    }
                    let next =
                        Self::with_scm_compute_max(self.re, input, *current, 1, dir, *loop_ip);
                    let Some(next) = next else {
                        self.bts.pop();
                        continue;
                    };
                    if next == *current {
                        self.bts.pop();
                        continue;
                    }
                    *remaining -= 1;
                    *current = next;
                    *pos = next;
                    *ip = *continuation;
                    return true;
                }
            }
        }
    }

    /// Attempt to match at a given IP and position.
    fn try_at_pos<Dir: Direction>(
        &mut self,
        inp: Input,
        mut ip: IP,
        mut pos: Input::Position,
        dir: Dir,
    ) -> Option<Input::Position> {
        debug_assert!(
            self.bts.len() == 1,
            "Should be only initial exhausted backtrack insn"
        );
        // TODO: we are inconsistent about passing Input by reference or value.
        let input = &inp;
        let re = self.re;
        // These are not really loops, they are just labels that we effectively 'goto'
        // to.
        #[allow(clippy::never_loop)]
        'nextinsn: loop {
            'backtrack: loop {
                // Helper macro to either increment ip and go to the next insn, or backtrack.
                macro_rules! next_or_bt {
                    ($e:expr) => {
                        if $e {
                            ip += 1;
                            continue 'nextinsn;
                        } else {
                            break 'backtrack;
                        }
                    };
                }

                match re.insns.iat(ip) {
                    &Insn::Char(c) => {
                        let m = match <<Input as InputIndexer>::Element as ElementType>::try_from(c)
                        {
                            Some(c) => scm::Char { c }.matches(input, dir, &mut pos),
                            None => false,
                        };
                        next_or_bt!(m);
                    }

                    Insn::CharSet(chars) => {
                        let m = scm::CharSet { chars }.matches(input, dir, &mut pos);
                        next_or_bt!(m);
                    }

                    &Insn::ByteSet2(bytes) => {
                        next_or_bt!(scm::MatchByteArraySet(bytes).matches(input, dir, &mut pos))
                    }
                    &Insn::ByteSet3(bytes) => {
                        next_or_bt!(scm::MatchByteArraySet(bytes).matches(input, dir, &mut pos))
                    }
                    &Insn::ByteSet4(bytes) => {
                        next_or_bt!(scm::MatchByteArraySet(bytes).matches(input, dir, &mut pos))
                    }

                    Insn::ByteSeq1(v) => {
                        next_or_bt!(cursor::try_match_lit(input, dir, &mut pos, v))
                    }
                    Insn::ByteSeq2(v) => {
                        next_or_bt!(cursor::try_match_lit(input, dir, &mut pos, v))
                    }
                    Insn::ByteSeq3(v) => {
                        next_or_bt!(cursor::try_match_lit(input, dir, &mut pos, v))
                    }
                    Insn::ByteSeq4(v) => {
                        next_or_bt!(cursor::try_match_lit(input, dir, &mut pos, v))
                    }
                    Insn::ByteSeq5(v) => {
                        next_or_bt!(cursor::try_match_lit(input, dir, &mut pos, v))
                    }
                    Insn::ByteSeq6(v) => {
                        next_or_bt!(cursor::try_match_lit(input, dir, &mut pos, v))
                    }
                    Insn::ByteSeq7(v) => {
                        next_or_bt!(cursor::try_match_lit(input, dir, &mut pos, v))
                    }
                    Insn::ByteSeq8(v) => {
                        next_or_bt!(cursor::try_match_lit(input, dir, &mut pos, v))
                    }
                    Insn::ByteSeq9(v) => {
                        next_or_bt!(cursor::try_match_lit(input, dir, &mut pos, v))
                    }
                    Insn::ByteSeq10(v) => {
                        next_or_bt!(cursor::try_match_lit(input, dir, &mut pos, v))
                    }
                    Insn::ByteSeq11(v) => {
                        next_or_bt!(cursor::try_match_lit(input, dir, &mut pos, v))
                    }
                    Insn::ByteSeq12(v) => {
                        next_or_bt!(cursor::try_match_lit(input, dir, &mut pos, v))
                    }
                    Insn::ByteSeq13(v) => {
                        next_or_bt!(cursor::try_match_lit(input, dir, &mut pos, v))
                    }
                    Insn::ByteSeq14(v) => {
                        next_or_bt!(cursor::try_match_lit(input, dir, &mut pos, v))
                    }
                    Insn::ByteSeq15(v) => {
                        next_or_bt!(cursor::try_match_lit(input, dir, &mut pos, v))
                    }
                    Insn::ByteSeq16(v) => {
                        next_or_bt!(cursor::try_match_lit(input, dir, &mut pos, v))
                    }

                    Insn::AsciiBracket(bitmap) => next_or_bt!(
                        scm::MatchByteSet { bytes: bitmap }.matches(input, dir, &mut pos)
                    ),

                    &Insn::Bracket(idx) => {
                        next_or_bt!(
                            scm::Bracket {
                                bc: &self.re.brackets[idx]
                            }
                            .matches(input, dir, &mut pos)
                        )
                    }

                    Insn::MatchAny => {
                        next_or_bt!(scm::MatchAny::new().matches(input, dir, &mut pos))
                    }

                    Insn::MatchAnyExceptLineTerminator => {
                        next_or_bt!(
                            scm::MatchAnyExceptLineTerminator::new().matches(input, dir, &mut pos)
                        )
                    }

                    &Insn::WordBoundary { invert } => {
                        // Copy the positions since these destructively move them.
                        let prev_wordchar = input
                            .peek_left(pos)
                            .is_some_and(Input::CharProps::is_word_char);
                        let curr_wordchar = input
                            .peek_right(pos)
                            .is_some_and(Input::CharProps::is_word_char);
                        let is_boundary = prev_wordchar != curr_wordchar;
                        next_or_bt!(is_boundary != invert)
                    }

                    &Insn::WordBoundaryUnicodeICase { invert } => {
                        let prev_wordchar = input
                            .peek_left(pos)
                            .is_some_and(Input::CharProps::is_word_char_unicode_icase);
                        let curr_wordchar = input
                            .peek_right(pos)
                            .is_some_and(Input::CharProps::is_word_char_unicode_icase);
                        let is_boundary = prev_wordchar != curr_wordchar;
                        next_or_bt!(is_boundary != invert)
                    }

                    Insn::StartOfLine { multiline } => {
                        let multiline = *multiline;
                        let matches = match input.peek_left(pos) {
                            None => true,
                            Some(c) if multiline && Input::CharProps::is_line_terminator(c) => true,
                            _ => false,
                        };
                        next_or_bt!(matches)
                    }
                    Insn::EndOfLine { multiline } => {
                        let multiline = *multiline;
                        let matches = match input.peek_right(pos) {
                            None => true, // we're at the right of the string
                            Some(c) if multiline && Input::CharProps::is_line_terminator(c) => true,
                            _ => false,
                        };
                        next_or_bt!(matches)
                    }

                    &Insn::Jump { target } => {
                        ip = target as usize;
                        continue 'nextinsn;
                    }

                    &Insn::BeginCaptureGroup(cg_idx) => {
                        let cg = self.s.groups.mat(cg_idx as usize);
                        self.bts.push(BacktrackInsn::SetCaptureGroup {
                            id: cg_idx,
                            data: *cg,
                        });
                        if Dir::FORWARD {
                            cg.start = Some(pos);
                            debug_assert!(
                                cg.end.is_none(),
                                "Should not have already exited capture group we are entering"
                            )
                        } else {
                            cg.end = Some(pos);
                            debug_assert!(
                                cg.start.is_none(),
                                "Should not have already exited capture group we are entering"
                            )
                        }
                        next_or_bt!(true)
                    }

                    &Insn::EndCaptureGroup(cg_idx) => {
                        let cg = self.s.groups.mat(cg_idx as usize);
                        if Dir::FORWARD {
                            debug_assert!(
                                cg.start_matched(),
                                "Capture group should have been entered"
                            );
                            cg.end = Some(pos);
                        } else {
                            debug_assert!(
                                cg.end_matched(),
                                "Capture group should have been entered"
                            );
                            cg.start = Some(pos)
                        }
                        next_or_bt!(true)
                    }

                    &Insn::ResetCaptureGroup(cg_idx) => {
                        let cg = self.s.groups.mat(cg_idx as usize);
                        self.bts.push(BacktrackInsn::SetCaptureGroup {
                            id: cg_idx,
                            data: *cg,
                        });
                        cg.reset();
                        next_or_bt!(true)
                    }

                    &Insn::BackRef {
                        group: cg_idx,
                        icase,
                    } => {
                        let cg = self.s.groups.mat(cg_idx as usize);
                        // Backreferences to a capture group that did not match always succeed (ES5
                        // 15.10.2.9).
                        // Note we may be in the capture group we are examining, e.g. /(abc\1)/.
                        let matched;
                        if let Some(orig_range) = cg.as_range() {
                            if icase {
                                matched = matchers::backref_icase(input, dir, orig_range, &mut pos);
                            } else {
                                matched = matchers::backref(input, dir, orig_range, &mut pos);
                            }
                        } else {
                            // This group has not been exited and so the match succeeds (ES6
                            // 21.2.2.9).
                            matched = true;
                        }
                        next_or_bt!(matched)
                    }

                    &Insn::Lookahead {
                        negate,
                        start_group,
                        end_group,
                        continuation,
                    } => {
                        if self.run_lookaround::<Forward>(
                            input,
                            ip + 1,
                            pos,
                            start_group,
                            end_group,
                            negate,
                        ) {
                            ip = continuation as IP;
                            continue 'nextinsn;
                        } else {
                            break 'backtrack;
                        }
                    }

                    &Insn::Lookbehind {
                        negate,
                        start_group,
                        end_group,
                        continuation,
                    } => {
                        if self.run_lookaround::<Backward>(
                            input,
                            ip + 1,
                            pos,
                            start_group,
                            end_group,
                            negate,
                        ) {
                            ip = continuation as IP;
                            continue 'nextinsn;
                        } else {
                            break 'backtrack;
                        }
                    }

                    &Insn::Alt { secondary } => {
                        self.push_backtrack(BacktrackInsn::SetPosition {
                            ip: secondary as IP,
                            pos,
                        });
                        next_or_bt!(true);
                    }

                    Insn::EnterLoop(fields) => {
                        // Entering a loop, not re-entering it.
                        self.s.loops.mat(fields.loop_id as usize).iters = 0;
                        match self.run_loop(fields, pos, ip) {
                            Some(next_ip) => {
                                ip = next_ip;
                                continue 'nextinsn;
                            }
                            None => {
                                break 'backtrack;
                            }
                        }
                    }

                    &Insn::LoopAgain { begin } => {
                        let act = match re.insns.iat(begin as IP) {
                            Insn::EnterLoop(fields) => self.run_loop(fields, pos, begin as IP),
                            _ => rs_unreachable!("EnterLoop should always refer to loop field"),
                        };
                        match act {
                            Some(next_ip) => {
                                ip = next_ip;
                                continue 'nextinsn;
                            }
                            None => break 'backtrack,
                        }
                    }

                    &Insn::Loop1CharBody {
                        min_iters,
                        max_iters,
                        greedy,
                        possessive,
                    } => {
                        if let Some(next_ip) = self.run_scm_loop(
                            input, dir, &mut pos, min_iters, max_iters, ip, greedy, possessive,
                        ) {
                            ip = next_ip;
                            continue 'nextinsn;
                        } else {
                            break 'backtrack;
                        }
                    }

                    Insn::Goal => {
                        // Keep all but the initial give-up bts.
                        self.bts.truncate(1);
                        return Some(pos);
                    }

                    Insn::JustFail => {
                        break 'backtrack;
                    }
                }
            }

            // This after the backtrack loop.
            // A break 'backtrack will jump here.
            if self.bts.len() > 1 && self.steps.step() {
                self.unwind();
                return None;
            }
            if self.try_backtrack(input, &mut ip, &mut pos, dir) {
                continue 'nextinsn;
            } else {
                // We have exhausted the backtracking stack.
                debug_assert!(self.bts.len() == 1, "Should have exhausted backtrack stack");
                return None;
            }
        }

        // This is outside the nextinsn loop.
        // It is an error to get here.
        // Every instruction should either continue 'nextinsn, or break 'backtrack.
        {
            #![allow(unreachable_code)]
            rs_unreachable!("Should not fall to end of nextinsn loop")
        }
    }
}

#[derive(Debug)]
pub struct BacktrackExecutor<'r, Input: InputIndexer> {
    input: Input,
    matcher: MatchAttempter<'r, Input>,
    // The scans for each required set: forward, where it started and the first unit found, if
    // any; then, once a search starts past that unit, backward, where it stopped and the last
    // unit found. Searches that start right of where a scan started need not scan again.
    #[cfg(feature = "utf16")]
    required_scan: [RequiredScan<Input::Position>; crate::utf16prefilter::MAX_REQUIRED],
    // The scans for the required strings, the same way, with the starts of the strings found.
    #[cfg(feature = "utf16")]
    factor_scan: RequiredScan<Input::Position>,
    // The run of the units every match consumes that the last search kept to, from where it
    // started to the end of the run. A search starting in it whose next string is in it too keeps
    // to the same run without reading it again.
    #[cfg(feature = "utf16")]
    factor_run: Option<(Input::Position, Input::Position)>,
    // The block the last search for a start tested and the run it read.
    #[cfg(feature = "utf16")]
    start_scan: crate::utf16prefilter::UnitScan,
    // The matcher compiled ahead of time that attempts matches in place of the interpreter.
    #[cfg(feature = "utf16")]
    attempt: Option<crate::aot::Attempt>,
}

#[cfg(feature = "utf16")]
type RequiredScan<Position> = [Option<(Position, Option<Position>)>; 2];

#[cfg(feature = "utf16")]
impl<'r, Input: InputIndexer> BacktrackExecutor<'r, Input> {
    pub(crate) fn new(input: Input, matcher: MatchAttempter<'r, Input>) -> Self {
        Self {
            input,
            attempt: matcher.re.attempt,
            matcher,
            required_scan: [[None; 2]; crate::utf16prefilter::MAX_REQUIRED],
            factor_scan: [None; 2],
            factor_run: None,
            start_scan: Default::default(),
        }
    }

    /// \return whether the input at or right of \p pos may hold a match by what \p prefilter
    /// requires of every match: a unit of each required set and one of the required strings.
    fn may_match(
        &mut self,
        pos: Input::Position,
        prefilter: &crate::utf16prefilter::Utf16Prefilter,
    ) -> bool {
        (prefilter.required() == 0 || self.has_required(pos, prefilter))
            && prefilter
                .factors
                .as_ref()
                .is_none_or(|factors| factors.spans() || self.has_factor(pos, factors))
    }

    /// \return the first start of one of the strings of \p factors at or right of \p pos.
    fn next_factor(
        &mut self,
        pos: Input::Position,
        factors: &crate::utf16factors::Factors,
    ) -> Option<Input::Position> {
        let [first, _] = &mut self.factor_scan;
        if let Some((from, found)) = *first
            && pos >= from
            && found.is_none_or(|at| pos <= at)
        {
            return found;
        }
        let found = self.input.find_factor(pos, factors);
        *first = Some((pos, found));
        found
    }

    /// \return the next match at or right of \p pos, trying only starts in the runs of units
    /// every match consumes that hold one of the strings of \p factors.
    fn next_match_in_runs(
        &mut self,
        mut pos: Input::Position,
        next_start: &mut Option<Input::Position>,
        factors: &crate::utf16factors::Factors,
        captures: &mut [Option<Range<usize>>],
    ) -> Option<Range<usize>> {
        loop {
            let at = self.next_factor(pos, factors)?;
            let (start, end) = match self.factor_run {
                Some((start, end)) if start <= pos && at < end => (pos, end),
                _ => {
                    let Some(run) = self.input.factor_run(pos, at, factors) else {
                        return self.search_until(
                            pos,
                            self.input.right_end(),
                            next_start,
                            captures,
                        );
                    };
                    self.factor_run = Some(run);
                    run
                }
            };
            if let Some(range) = self.search_until(start, end, next_start, captures) {
                return Some(range);
            }
            if self.matcher.steps.stopped() {
                return None;
            }
            pos = end;
        }
    }

    /// \return whether one of the strings of \p factors starts at or right of \p pos.
    fn has_factor(&mut self, pos: Input::Position, factors: &crate::utf16factors::Factors) -> bool {
        let input = &self.input;
        scan_finds(
            &mut self.factor_scan,
            pos,
            || input.find_factor(pos, factors),
            || input.rfind_factor(pos, factors),
        )
    }

    /// \return whether the input at or right of \p pos holds a unit of each required set of
    /// \p prefilter.
    fn has_required(
        &mut self,
        pos: Input::Position,
        prefilter: &crate::utf16prefilter::Utf16Prefilter,
    ) -> bool {
        let input = &self.input;
        self.required_scan
            .iter_mut()
            .enumerate()
            .take(prefilter.required())
            .all(|(i, scan)| {
                scan_finds(
                    scan,
                    pos,
                    || input.find_required_unit(pos, prefilter, i),
                    || input.rfind_required_unit(pos, prefilter, i),
                )
            })
    }
}

#[cfg(feature = "utf16")]
fn scan_finds<Position: PositionType>(
    scan: &mut RequiredScan<Position>,
    pos: Position,
    find: impl FnOnce() -> Option<Position>,
    rfind: impl FnOnce() -> Option<Position>,
) -> bool {
    let [first, last] = scan;
    if let Some((from, found)) = *last
        && pos >= from
    {
        return found.is_some_and(|at| pos <= at);
    }
    match *first {
        Some((from, found)) if pos >= from => match found {
            None => false,
            Some(at) if pos <= at => true,
            Some(_) => {
                let found = rfind();
                *last = Some((pos, found));
                found.is_some()
            }
        },
        _ => {
            let found = find();
            *first = Some((pos, found));
            found.is_some()
        }
    }
}

impl<Input: InputIndexer> BacktrackExecutor<'_, Input> {
    /// Attempt a match at \p pos, with the matcher compiled ahead of time if there is one.
    /// On success, store the capture groups in \p captures, one slot per group, and clear them.
    /// \return the end of the match.
    #[inline(always)]
    fn attempt_at(
        &mut self,
        pos: Input::Position,
        captures: &mut [Option<Range<usize>>],
    ) -> Option<Input::Position> {
        let inp = self.input;
        #[cfg(feature = "utf16")]
        if let Some(attempt) = self.attempt {
            let end = attempt(
                inp.code_units(),
                inp.pos_to_offset(pos),
                &mut self.matcher.aot_bts,
                &mut self.matcher.steps,
                captures,
            )?;
            return inp.try_move_right(inp.left_end(), end);
        }
        self.matcher.prepare();
        let end = self.matcher.try_at_pos(inp, 0, pos, Forward::new())?;
        // We want to simultaneously map our groups to offsets, and clear the groups.
        for (idx, gd) in self.matcher.s.groups.iter_mut().enumerate() {
            if let Some(out) = captures.get_mut(idx) {
                *out = gd.as_range().map(|r| Range {
                    start: inp.pos_to_offset(r.start),
                    end: inp.pos_to_offset(r.end),
                });
            }
            gd.start = None;
            gd.end = None;
        }
        Some(end)
    }

    /// \return the range of a match from \p pos to \p end, and set \p next_start to where the
    /// next search starts.
    #[inline(always)]
    fn successful_match(
        &self,
        pos: Input::Position,
        end: Input::Position,
        next_start: &mut Option<Input::Position>,
    ) -> Range<usize> {
        // If we matched the empty string, we have to increment.
        if end != pos {
            *next_start = Some(end)
        } else {
            *next_start = self.input.next_right_pos(end);
        }
        self.input.pos_to_offset(pos)..self.input.pos_to_offset(end)
    }

    /// \return the next match for an anchored regex that only matches at the start.
    /// This avoids any string searching and only tries matching at the given position.
    fn next_match_anchored(
        &mut self,
        pos: Input::Position,
        next_start: &mut Option<Input::Position>,
        captures: &mut [Option<Range<usize>>],
    ) -> Option<Range<usize>> {
        // For anchored regexes, only try matching at the current position
        let end = self.attempt_at(pos, captures)?;
        Some(self.successful_match(pos, end, next_start))
    }

    /// \return the next match that starts at or left of \p limit, searching the remaining bytes
    /// using the given prefix searcher to quickly find the first potential match location.
    fn next_match_with_prefix_search<PrefixSearch: bytesearch::ByteSearcher>(
        &mut self,
        mut pos: Input::Position,
        limit: Input::Position,
        next_start: &mut Option<Input::Position>,
        prefix_search: &PrefixSearch,
        captures: &mut [Option<Range<usize>>],
    ) -> Option<Range<usize>> {
        let inp = self.input;
        loop {
            // Find the next start location, or None if none.
            // Don't try this unless CODE_UNITS_ARE_BYTES - i.e. don't do byte searches
            // on UTF-16 or UCS2.
            if Input::CODE_UNITS_ARE_BYTES {
                pos = inp
                    .find_bytes(pos, prefix_search)
                    .filter(|&pos| pos <= limit)?;
            }
            if let Some(end) = self.attempt_at(pos, captures) {
                return Some(self.successful_match(pos, end, next_start));
            }
            if self.matcher.steps.stopped() {
                return None;
            }
            // Didn't find it at this position, try the next one.
            pos = inp.next_right_pos(pos).filter(|&pos| pos <= limit)?;
        }
    }

    /// \return the next match that starts at or left of \p limit, trying only positions whose
    /// code unit may begin one and, away from the start of the input, whose next unit may
    /// follow it.
    #[cfg(feature = "utf16")]
    fn next_match_with_unit_search(
        &mut self,
        mut pos: Input::Position,
        limit: Input::Position,
        next_start: &mut Option<Input::Position>,
        prefilter: &crate::utf16prefilter::Utf16Prefilter,
        captures: &mut [Option<Range<usize>>],
    ) -> Option<Range<usize>> {
        let inp = self.input;
        loop {
            if !(prefilter.at_input_start && pos == inp.left_end()) {
                pos = inp.find_start_unit(pos, limit, prefilter, &mut self.start_scan)?;
            }
            if let Some(end) = self.attempt_at(pos, captures) {
                return Some(self.successful_match(pos, end, next_start));
            }
            if self.matcher.steps.stopped() {
                return None;
            }
            pos = inp.next_right_pos(pos)?;
        }
    }
}

impl<Input: InputIndexer> exec::MatchProducer for BacktrackExecutor<'_, Input> {
    type Position = Input::Position;

    fn initial_position(&self, offset: usize) -> Option<Self::Position> {
        self.input.try_move_right(self.input.left_end(), offset)
    }

    fn next_match(
        &mut self,
        pos: Input::Position,
        next_start: &mut Option<Input::Position>,
    ) -> Option<Match> {
        let mut captures = vec![None; self.matcher.re.groups as usize];
        let range = self.next_match_into(pos, next_start, &mut captures)?;
        Some(Match {
            range,
            captures,
            group_names: self.matcher.re.group_names.clone(),
        })
    }

    fn attempt_into(
        &mut self,
        pos: Input::Position,
        captures: &mut [Option<Range<usize>>],
    ) -> Option<Range<usize>> {
        let end = self.attempt_at(pos, captures)?;
        Some(self.input.pos_to_offset(pos)..self.input.pos_to_offset(end))
    }

    fn stop_when(&mut self, stop: fn() -> bool) {
        self.matcher.steps = exec::Steps::new(Some(stop));
    }

    fn stopped(&self) -> bool {
        self.matcher.steps.stopped()
    }

    fn next_match_into(
        &mut self,
        pos: Input::Position,
        next_start: &mut Option<Input::Position>,
        captures: &mut [Option<Range<usize>>],
    ) -> Option<Range<usize>> {
        // When UTF-16 support is active byte prefix search is not used due to the different
        // encoding; UTF-16 and UCS-2 inputs search for a possible first code unit instead.
        #[cfg(feature = "utf16")]
        {
            // An anchored search makes one attempt, which costs less than reading the rest of
            // the input for what the prefilter requires.
            let anchored = matches!(self.matcher.re.start_pred, StartPredicate::StartAnchored);
            if !anchored && !self.may_match(pos, &self.matcher.re.utf16) {
                // A search the prefilter rules out runs anyway in debug builds, to show that it
                // finds nothing.
                #[cfg(debug_assertions)]
                if let Some(range) = self.search(pos, next_start, captures) {
                    panic!("A search the prefilter ruled out from {pos:?} matched {range:?}");
                }
                return None;
            }
            self.search(pos, next_start, captures)
        }

        #[cfg(not(feature = "utf16"))]
        self.search(pos, next_start, captures)
    }
}

impl<Input: InputIndexer> BacktrackExecutor<'_, Input> {
    /// \return the next match at or right of \p pos.
    #[inline(always)]
    fn search(
        &mut self,
        pos: Input::Position,
        next_start: &mut Option<Input::Position>,
        captures: &mut [Option<Range<usize>>],
    ) -> Option<Range<usize>> {
        #[cfg(feature = "utf16")]
        {
            let re = self.matcher.re;
            if matches!(re.start_pred, StartPredicate::StartAnchored) {
                return self.next_match_anchored(pos, next_start, captures);
            }
            if let Some(factors) = re.utf16.factors.as_ref().filter(|f| f.spans()) {
                let found = self.next_match_in_runs(pos, next_start, factors, captures);
                // A search kept to runs finds what a search of the rest of the input does, which
                // debug builds check.
                #[cfg(debug_assertions)]
                {
                    let mut all = captures.to_vec();
                    let whole = self.search_until(pos, self.input.right_end(), &mut None, &mut all);
                    assert_eq!(
                        found, whole,
                        "A search kept to runs from {pos:?} found {found:?}, not {whole:?}"
                    );
                }
                return found;
            }
            self.search_until(pos, self.input.right_end(), next_start, captures)
        }

        #[cfg(not(feature = "utf16"))]
        {
            let limit = self.input.right_end();
            match &self.matcher.re.start_pred {
                StartPredicate::Arbitrary => self.next_match_with_prefix_search(
                    pos,
                    limit,
                    next_start,
                    &bytesearch::EmptyString {},
                    captures,
                ),
                StartPredicate::StartAnchored => {
                    self.next_match_anchored(pos, next_start, captures)
                }
                StartPredicate::ByteSet1(bytes) => {
                    self.next_match_with_prefix_search(pos, limit, next_start, bytes, captures)
                }
                StartPredicate::ByteSet2(bytes) => {
                    self.next_match_with_prefix_search(pos, limit, next_start, bytes, captures)
                }
                StartPredicate::ByteSet3(bytes) => {
                    self.next_match_with_prefix_search(pos, limit, next_start, bytes, captures)
                }
                StartPredicate::ByteSeq(bytes) => self.next_match_with_prefix_search(
                    pos,
                    limit,
                    next_start,
                    bytes.as_ref(),
                    captures,
                ),
                StartPredicate::ByteBracket(bitmap) => {
                    self.next_match_with_prefix_search(pos, limit, next_start, bitmap, captures)
                }
            }
        }
    }

    /// \return the next match at or right of \p pos that starts at or left of \p limit.
    #[cfg(feature = "utf16")]
    #[inline(always)]
    fn search_until(
        &mut self,
        pos: Input::Position,
        limit: Input::Position,
        next_start: &mut Option<Input::Position>,
        captures: &mut [Option<Range<usize>>],
    ) -> Option<Range<usize>> {
        let re = self.matcher.re;
        if re.utf16.filters_start() {
            self.next_match_with_unit_search(pos, limit, next_start, &re.utf16, captures)
        } else {
            self.next_match_with_prefix_search(
                pos,
                limit,
                next_start,
                &bytesearch::EmptyString {},
                captures,
            )
        }
    }
}

impl<'r, 't> exec::Executor<'r, 't> for BacktrackExecutor<'r, Utf8Input<'t>> {
    type AsAscii = BacktrackExecutor<'r, AsciiInput<'t>>;

    fn new(re: &'r CompiledRegex, text: &'t str) -> Self {
        let input = Utf8Input::new(text, re.flags.unicode_mode());
        Self {
            input,
            matcher: MatchAttempter::new(re, input.left_end()),
            #[cfg(feature = "utf16")]
            required_scan: [[None; 2]; crate::utf16prefilter::MAX_REQUIRED],
            #[cfg(feature = "utf16")]
            factor_scan: [None; 2],
            #[cfg(feature = "utf16")]
            factor_run: None,
            #[cfg(feature = "utf16")]
            start_scan: Default::default(),
            #[cfg(feature = "utf16")]
            attempt: None,
        }
    }
}

impl<'r, 't> exec::Executor<'r, 't> for BacktrackExecutor<'r, AsciiInput<'t>> {
    type AsAscii = BacktrackExecutor<'r, AsciiInput<'t>>;

    fn new(re: &'r CompiledRegex, text: &'t str) -> Self {
        let input = AsciiInput::new(text, re.flags.unicode_mode());
        Self {
            input,
            matcher: MatchAttempter::new(re, input.left_end()),
            #[cfg(feature = "utf16")]
            required_scan: [[None; 2]; crate::utf16prefilter::MAX_REQUIRED],
            #[cfg(feature = "utf16")]
            factor_scan: [None; 2],
            #[cfg(feature = "utf16")]
            factor_run: None,
            #[cfg(feature = "utf16")]
            start_scan: Default::default(),
            #[cfg(feature = "utf16")]
            attempt: None,
        }
    }
}
