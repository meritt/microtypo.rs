//! Bytecode instructions for a compiled regex

#[cfg(not(feature = "std"))]
use alloc::{boxed::Box, vec::Vec};

use crate::api;
use crate::bytesearch::{AsciiBitmap, ByteArraySet, ByteBitmap};
use crate::types::{BracketContents, CaptureGroupID, LoopID};
extern crate memchr;
use memchr::memmem;

type JumpTarget = u32;

/// The maximum size of a byte sequence instruction.
pub const MAX_BYTE_SEQ_LENGTH: usize = 16;

/// The maximum size of an array-type-charset instruction.
/// This also happens to be the maximum number of characters in case-insensitive
/// equivalence classes.
pub const MAX_CHAR_SET_LENGTH: usize = 4;

#[derive(Debug, Clone)]
pub struct LoopFields {
    pub loop_id: LoopID,
    pub min_iters: usize,
    pub max_iters: usize,
    pub greedy: bool,
    pub exit: JumpTarget,
}

#[derive(Debug, Clone)]
/// The list of bytecode instructions.
pub enum Insn {
    /// The match was successful.
    Goal,

    /// Match a single char.
    Char(u32),

    /// Match the start of a line (if multiline); emitted by '^'
    StartOfLine {
        multiline: bool,
    },

    /// Match the end of a line; emitted by '$'
    EndOfLine {
        multiline: bool,
    },

    /// Match any character except a line terminator; emitted by '.' only when
    /// the dot_all flag is set to true.
    MatchAny,

    /// Match any character except a line terminator; emitted by '.'
    MatchAnyExceptLineTerminator,

    /// Enter a loop from "outside".
    EnterLoop(LoopFields),

    /// Re-enter a loop.
    LoopAgain {
        begin: JumpTarget,
    },

    /// The next instruction is a "1Char" instruction which always matches one
    /// character. Attempt to match it [min, max] times.
    Loop1CharBody {
        min_iters: usize,
        max_iters: usize,
        greedy: bool,
        /// Whether backtracking into the loop cannot succeed, so none need be recorded.
        possessive: bool,
    },

    /// Set the IP to a new value.
    Jump {
        target: JumpTarget,
    },

    /// The next instruction is the primary branch.
    /// If it fails to match, jump to secondary.
    Alt {
        secondary: JumpTarget,
    },

    /// Enter a capture group.
    BeginCaptureGroup(CaptureGroupID),

    /// Exit a capture group.
    EndCaptureGroup(CaptureGroupID),

    /// Clear a capture group.
    ResetCaptureGroup(CaptureGroupID),

    /// Perform a backreference match.
    BackRef {
        group: u32,
        icase: bool,
    },

    /// Match the next character against the bracket contents, stored at the given index in the CompiledRegex.
    Bracket(usize),

    /// A simple bitmap bracket for ASCII.
    /// It contains a bitmap of the range [0, 127].
    AsciiBracket(AsciiBitmap),

    /// Perform a lookahead assertion.
    Lookahead {
        negate: bool,
        start_group: CaptureGroupID,
        end_group: CaptureGroupID,
        continuation: JumpTarget,
    },

    /// Perform a lookbehind assertion.
    Lookbehind {
        negate: bool,
        start_group: CaptureGroupID,
        end_group: CaptureGroupID,
        continuation: JumpTarget,
    },

    /// \w or \W word boundaries.
    WordBoundary {
        invert: bool,
    },

    /// \w or \W word boundaries in Unicode case-insensitive mode.
    /// This considers characters that fold to ASCII word chars as word chars,
    /// per ES9 21.2.2.6.1.
    WordBoundaryUnicodeICase {
        invert: bool,
    },

    /// Match any of the contained chars
    /// There is no length field; characters are simply duplicated as necessary.
    CharSet([u32; MAX_CHAR_SET_LENGTH]),

    /// Match the next byte against some possibilities.
    ByteSet2(ByteArraySet<[u8; 2]>),
    ByteSet3(ByteArraySet<[u8; 3]>),
    ByteSet4(ByteArraySet<[u8; 4]>),

    /// Match a sequence of literal bytes.
    ByteSeq1([u8; 1]),
    ByteSeq2([u8; 2]),
    ByteSeq3([u8; 3]),
    ByteSeq4([u8; 4]),
    ByteSeq5([u8; 5]),
    ByteSeq6([u8; 6]),
    ByteSeq7([u8; 7]),
    ByteSeq8([u8; 8]),
    ByteSeq9([u8; 9]),
    ByteSeq10([u8; 10]),
    ByteSeq11([u8; 11]),
    ByteSeq12([u8; 12]),
    ByteSeq13([u8; 13]),
    ByteSeq14([u8; 14]),
    ByteSeq15([u8; 15]),
    ByteSeq16([u8; 16]),

    /// An instruction that always fails, which may be produced in weird cases
    /// like an inverted bracket which matches everything.
    JustFail,
}

/// The peeled prefix start predicate.
/// This is a fast way of locating the first potential match.
#[derive(Debug, Clone)]
pub enum StartPredicate {
    /// May match an arbitrary sequence.
    Arbitrary,

    /// Look for the first instance of any of the given bytes.
    ByteSet1([u8; 1]),
    ByteSet2([u8; 2]),
    ByteSet3([u8; 3]),

    /// Look for a byte sequence.
    ByteSeq(Box<memmem::Finder<'static>>),

    /// Look for a byte which matches the bitmap.
    ByteBracket(ByteBitmap),

    /// The regex is anchored to the start of the line/string.
    /// This avoids string searching entirely and only tries matching at the beginning.
    StartAnchored,
}

/// Code points below this are matched against a bracket's bitmap: Latin, Cyrillic and their
/// neighbours, where the bulk of the text lies.
const DENSE_BRACKET_CPS: u32 = 0x800;

/// The contents of a `Bracket` instruction, with a bitmap of the low code points it matches, so
/// that matching those needs no search of its intervals.
#[derive(Debug, Clone)]
pub struct CompiledBracket {
    /// Whether each code point below `DENSE_BRACKET_CPS` matches, inversion applied.
    dense: [u64; DENSE_BRACKET_CPS as usize / 64],
    contents: BracketContents,
}

impl CompiledBracket {
    pub fn new(contents: BracketContents) -> Self {
        let mut dense = [0u64; DENSE_BRACKET_CPS as usize / 64];
        for iv in contents.cps.intervals() {
            if iv.first >= DENSE_BRACKET_CPS {
                break;
            }
            let last = iv.last.min(DENSE_BRACKET_CPS - 1);
            for word in iv.first / 64..=last / 64 {
                let lo = iv.first.max(word * 64) % 64;
                let hi = last.min(word * 64 + 63) % 64;
                dense[word as usize] |= (u64::MAX >> (63 - hi)) & (u64::MAX << lo);
            }
        }
        if contents.invert {
            dense.iter_mut().for_each(|word| *word = !*word);
        }
        Self { dense, contents }
    }

    /// \return whether the bracket matches \p cp, respecting 'invert'.
    #[inline(always)]
    pub fn matches(&self, cp: u32) -> bool {
        match self.dense.get(cp as usize / 64) {
            Some(word) => word >> (cp % 64) & 1 != 0,
            None => self.contents.cps.contains(cp) != self.contents.invert,
        }
    }
}

#[derive(Debug, Clone)]
pub struct CompiledRegex {
    // Sequence of instructions.
    pub insns: Vec<Insn>,

    // The bracket contents, indexed by the value of the `Bracket` instruction.
    pub brackets: Vec<CompiledBracket>,

    // Predicate to rapidly find the first potential match.
    pub start_pred: StartPredicate,

    // Where matches may be in UTF-16 or UCS-2 input.
    #[cfg(feature = "utf16")]
    pub utf16: crate::utf16prefilter::Utf16Prefilter,

    // Number of loops, used to populate loop data.
    pub loops: u32,

    // Number of capture groups, used to populate capture group data.
    pub groups: u32,

    // A list of capture group names. This is either:
    //   - Empty, if there were no named capture groups.
    //   - A list of names with length `groups`, corresponding to the capture
    //     group names in order. Groups without names have an empty string.
    pub group_names: Box<[Box<str>]>,

    // Flags controlling matching.
    pub flags: api::Flags,
}
