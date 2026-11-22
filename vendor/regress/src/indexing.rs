use crate::bytesearch;
use crate::cursor::Direction;
use crate::matchers::{self, CharProperties};
#[cfg(feature = "utf16")]
use crate::position::IndexPosition;
use crate::position::{DefPosition, PositionType};
use crate::util::{is_utf8_continuation, utf8_w2, utf8_w3, utf8_w4};
use core::convert::TryInto;
use core::ops::Range;
use core::{ops, str};

// A type which may be an Element.
pub trait ElementType:
    core::fmt::Debug
    + Copy
    + Clone
    + core::cmp::Eq
    + core::cmp::Ord
    + core::convert::Into<u32>
    + core::convert::TryFrom<u32>
{
    /// Return another ElementType as self.
    #[inline(always)]
    fn try_from<Elem: ElementType>(v: Elem) -> Option<Self> {
        // Annoying there is no char->u8 conversion.
        let vv: u32 = v.into();
        vv.try_into().ok()
    }

    #[inline(always)]
    fn as_u32(self) -> u32 {
        self.into()
    }
}

impl ElementType for char {}

impl ElementType for u8 {}

impl ElementType for u32 {}

// A helper type that holds a string and allows indexing into it.
pub trait InputIndexer: core::fmt::Debug + Copy + Clone
where
    Self::CharProps: matchers::CharProperties<Element = Self::Element>,
{
    /// The char type, typically u8 or char.
    type Element: ElementType;

    /// The CharProperties to use for the given element.
    type CharProps: matchers::CharProperties<Element = Self::Element>;

    /// A type which references a position in the input string.
    type Position: PositionType;

    /// Whether we have bytes as code units. This can optimize some operations.
    /// This is true for ASCII and UTF8, but not for UCS2 or UTF16.
    const CODE_UNITS_ARE_BYTES: bool;

    /// \return whether we are using unicode for case-folding.
    fn unicode(&self) -> bool;

    /// \return the emptied search state a finished search on this thread left, if any.
    fn take_buffers() -> Option<crate::classicalbacktrack::Buffers<Self>> {
        None
    }

    /// Keep a finished search's emptied state for the next search on this thread.
    fn give_buffers(_buffers: crate::classicalbacktrack::Buffers<Self>) {}

    /// Case-fold an element.
    fn fold(&self, c: Self::Element) -> Self::Element {
        Self::CharProps::fold(c, self.unicode())
    }

    /// Return whether these two elements fold to the same value.
    fn fold_equals(&self, c1: Self::Element, c2: Self::Element) -> bool {
        c1 == c2 || self.fold(c1) == self.fold(c2)
    }

    /// \return a sub-input. Note that positions in the original may no longer be valid in the sub-input.
    fn subinput(&self, range: ops::Range<Self::Position>) -> Self;

    /// \return the char to the right (starting at) \p idx, or None if we are at
    /// the end. Advance the position by the amount.
    fn next_right(&self, pos: &mut Self::Position) -> Option<Self::Element>;

    /// \return the char to the left (ending just before) \p idx, or None if we are at
    /// the end. Retreat the position by the amount.
    fn next_left(&self, pos: &mut Self::Position) -> Option<Self::Element>;

    // Like next_right, but does not decode the element.
    fn next_right_pos(&self, pos: Self::Position) -> Option<Self::Position>;

    // Like next_left, but does not decode the element.
    fn next_left_pos(&self, pos: Self::Position) -> Option<Self::Position>;

    /// \return the byte to the right (starting at) \p idx, or None if we are at
    /// the end. This panics if CODE_UNITS_ARE_BYTES is false.
    fn peek_byte_right(&self, pos: Self::Position) -> Option<u8>;

    /// \return the byte to the left (ending just before) \p idx, or None if we
    /// are at the start. This panics if CODE_UNITS_ARE_BYTES is false.
    fn peek_byte_left(&self, pos: Self::Position) -> Option<u8>;

    /// \return a position at the left end of this input.
    fn left_end(&self) -> Self::Position;

    /// \return a position at the right end of this input.
    fn right_end(&self) -> Self::Position;

    /// Move a position right by a certain amount.
    /// \return the new position, or None if it would exceed the length.
    fn try_move_right(&self, pos: Self::Position, amt: usize) -> Option<Self::Position>;

    /// Move a position left by a certain amount.
    /// \return the new position, or None if it would underflow 0.
    fn try_move_left(&self, pos: Self::Position, amt: usize) -> Option<Self::Position>;

    /// Convert a position to an offset.
    fn pos_to_offset(&self, pos: Self::Position) -> usize;

    /// Apply a literal byte matcher, finding a literal byte sequence in a string.
    /// \return the new position, or None on failure.
    fn find_bytes<Search: bytesearch::ByteSearcher>(
        &self,
        pos: Self::Position,
        search: &Search,
    ) -> Option<Self::Position>;

    /// \return the first position at or right of \p pos and at or left of \p limit, stepping as
    /// next_right_pos does, where \p prefilter allows a match to start, or None if there is
    /// none. \p scan carries what one search learned to the next one over the same input. Byte
    /// inputs do not filter.
    #[cfg(feature = "utf16")]
    #[inline(always)]
    fn find_start_unit(
        &self,
        pos: Self::Position,
        limit: Self::Position,
        _prefilter: &crate::utf16prefilter::Utf16Prefilter,
        _scan: &mut crate::utf16prefilter::UnitScan,
    ) -> Option<Self::Position> {
        (pos <= limit).then_some(pos)
    }

    /// \return the run around \p at, a start of one of the strings of \p factors, of units
    /// every match consumes, from no further left than \p pos, as its first position and the
    /// position right of it; None if \p factors know no such units. Byte inputs do not filter.
    #[cfg(feature = "utf16")]
    #[inline(always)]
    fn factor_run(
        &self,
        _pos: Self::Position,
        _at: Self::Position,
        _factors: &crate::utf16factors::Factors,
    ) -> Option<(Self::Position, Self::Position)> {
        None
    }

    /// \return the first position at or right of \p pos whose code unit is in the required set
    /// \p i of \p prefilter, or None if there is none. Byte inputs do not filter.
    #[cfg(feature = "utf16")]
    #[inline(always)]
    fn find_required_unit(
        &self,
        pos: Self::Position,
        _prefilter: &crate::utf16prefilter::Utf16Prefilter,
        _i: usize,
    ) -> Option<Self::Position> {
        Some(pos)
    }

    /// \return the last position at or right of \p pos whose code unit is in the required set
    /// \p i of \p prefilter, or None if there is none. Byte inputs do not filter.
    #[cfg(feature = "utf16")]
    #[inline(always)]
    fn rfind_required_unit(
        &self,
        _pos: Self::Position,
        _prefilter: &crate::utf16prefilter::Utf16Prefilter,
        _i: usize,
    ) -> Option<Self::Position> {
        Some(self.right_end())
    }

    /// \return the first start at or right of \p pos of one of the strings of \p factors, or
    /// None if there is none. Byte inputs do not filter.
    #[cfg(feature = "utf16")]
    #[inline(always)]
    fn find_factor(
        &self,
        pos: Self::Position,
        _factors: &crate::utf16factors::Factors,
    ) -> Option<Self::Position> {
        Some(pos)
    }

    /// \return the last start at or right of \p pos of one of the strings of \p factors, or
    /// None if there is none. Byte inputs do not filter.
    #[cfg(feature = "utf16")]
    #[inline(always)]
    fn rfind_factor(
        &self,
        _pos: Self::Position,
        _factors: &crate::utf16factors::Factors,
    ) -> Option<Self::Position> {
        Some(self.right_end())
    }

    /// \return the code units of a UTF-16 or UCS-2 input, which a matcher compiled ahead of time
    /// reads. Byte inputs have none and never run such a matcher.
    #[cfg(feature = "utf16")]
    fn code_units(&self) -> &[u16] {
        unreachable!("Only UTF-16 and UCS-2 inputs run matchers compiled ahead of time")
    }

    /// Peek at the char to the right of a position, without changing that position.
    #[inline(always)]
    fn peek_right(&self, mut pos: Self::Position) -> Option<Self::Element> {
        self.next_right(&mut pos)
    }

    /// Peek at the char to the left of a position, without changing that position.
    #[inline(always)]
    fn peek_left(&self, mut pos: Self::Position) -> Option<Self::Element> {
        self.next_left(&mut pos)
    }

    /// Check if the subrange `range` is byte-for-byte equal to a range of the same length from the current position `pos`.
    /// If `dir` is FORWARD, then the range is checked starting at `pos` and ending at `pos + range.len()`.
    /// If `dir` is BACKWARD, then the range is checked starting at `pos - range.len()` and ending at `pos`.
    fn subrange_eq<Dir: Direction>(
        &self,
        dir: Dir,
        pos: &mut Self::Position,
        range: Range<Self::Position>,
    ) -> bool;

    /// Return whether we match some literal bytes.
    /// If so, update the position. If not, the position is unspecified.
    fn match_bytes<const N: usize, Dir: Direction>(
        &self,
        dir: Dir,
        pos: &mut Self::Position,
        bytes: &[u8; N],
    ) -> bool;
}

/// \return the length of a UTF8 sequence starting with this byte.
#[inline(always)]
const fn utf8_seq_len(b: u8) -> usize {
    if b < 128 {
        1
    } else {
        match b & 0xF0 {
            0xE0 => 3,
            0xF0 => 4,
            _ => 2,
        }
    }
}

/// \return whether a byte represents the start of a utf8 sequence (aka a
/// char boundary).
#[inline(always)]
fn is_seq_start(b: u8) -> bool {
    // Taken from is_char_boundary.
    // "This is bit magic equivalent to: b < 128 || b >= 192"
    (b as i8) >= -0x40
}

#[derive(Debug, Copy, Clone)]
pub struct Utf8Input<'a> {
    input: &'a str,
    unicode: bool,
}

impl<'a> Utf8Input<'a> {
    #[inline(always)]
    fn contents(&self) -> &[u8] {
        self.input.as_bytes()
    }

    #[inline(always)]
    fn bytelength(&self) -> usize {
        self.input.len()
    }

    #[inline(always)]
    fn slice(
        &self,
        start: <Self as InputIndexer>::Position,
        end: <Self as InputIndexer>::Position,
    ) -> &[u8] {
        self.debug_assert_valid_pos(start);
        self.debug_assert_valid_pos(end);
        debug_assert!(end >= start, "Slice start after end");

        #[cfg(any(feature = "index-positions", feature = "prohibit-unsafe"))]
        let res = &self.contents()[core::ops::Range {
            start: self.pos_to_offset(start),
            end: self.pos_to_offset(end),
        }];

        #[cfg(all(not(feature = "index-positions"), not(feature = "prohibit-unsafe")))]
        let res = unsafe { core::slice::from_raw_parts(start.ptr(), end - start) };

        debug_assert!(res.len() <= self.bytelength() && res.len() == end - start);
        res
    }

    #[inline(always)]
    pub fn new(s: &'a str, unicode: bool) -> Self {
        // The big idea of RefPosition is enforced here.
        <Self as InputIndexer>::Position::check_size();

        Self { input: s, unicode }
    }

    /// \return a byte at a given position.
    /// This asserts that we are not at the right end.
    #[inline(always)]
    fn getb(&self, pos: <Self as InputIndexer>::Position) -> u8 {
        debug_assert!(self.left_end() <= pos && pos < self.right_end());
        if cfg!(feature = "prohibit-unsafe") {
            self.contents()[self.pos_to_offset(pos)]
        } else {
            unsafe { *self.contents().get_unchecked(self.pos_to_offset(pos)) }
        }
    }

    /// \return a slice as a str.
    #[inline(always)]
    fn str_slice(&self, range: ops::Range<<Self as InputIndexer>::Position>) -> &'a str {
        self.debug_assert_boundary(range.start);
        self.debug_assert_boundary(range.end);
        if cfg!(feature = "prohibit-unsafe") {
            &self.input[core::ops::Range {
                start: self.pos_to_offset(range.start),
                end: self.pos_to_offset(range.end),
            }]
        } else {
            unsafe {
                self.input.get_unchecked(core::ops::Range {
                    start: self.pos_to_offset(range.start),
                    end: self.pos_to_offset(range.end),
                })
            }
        }
    }

    /// Assert that a position is valid, i.e. between our left and right ends.
    #[inline(always)]
    fn debug_assert_valid_pos(&self, pos: <Self as InputIndexer>::Position) {
        debug_assert!(self.left_end() <= pos && pos <= self.right_end());
    }

    /// Assert that a position is a valid UTF8 character boundary.
    #[inline(always)]
    fn debug_assert_boundary(&self, pos: <Self as InputIndexer>::Position) {
        self.debug_assert_valid_pos(pos);
        debug_assert!(pos == self.right_end() || is_seq_start(self.getb(pos)));
    }
}

impl<'a> InputIndexer for Utf8Input<'a> {
    type Position = DefPosition<'a>;
    type Element = char;
    type CharProps = matchers::UTF8CharProperties;
    const CODE_UNITS_ARE_BYTES: bool = true;

    #[inline(always)]
    fn unicode(&self) -> bool {
        self.unicode
    }

    #[inline(always)]
    fn subinput(&self, range: ops::Range<Self::Position>) -> Self {
        Self::new(self.str_slice(range), self.unicode)
    }

    #[inline(always)]
    fn next_right(&self, pos: &mut Self::Position) -> Option<Self::Element> {
        self.debug_assert_boundary(*pos);
        if *pos == self.right_end() {
            return None;
        }

        let b0 = self.getb(*pos);
        if b0 < 128 {
            *pos += 1;
            return Some(b0 as Self::Element);
        }

        // Multibyte case.
        let len = utf8_seq_len(b0);
        let codepoint = match len {
            2 => utf8_w2(b0, self.getb(*pos + 1)),
            3 => utf8_w3(b0, self.getb(*pos + 1), self.getb(*pos + 2)),
            4 => utf8_w4(
                b0,
                self.getb(*pos + 1),
                self.getb(*pos + 2),
                self.getb(*pos + 3),
            ),
            _ => rs_unreachable!("Invalid utf8 sequence length"),
        };
        *pos += len;
        if let Some(c) = core::char::from_u32(codepoint) {
            Some(c)
        } else {
            rs_unreachable!("Should have decoded a valid char from utf8 sequence");
        }
    }

    #[inline(always)]
    fn next_right_pos(&self, mut pos: Self::Position) -> Option<Self::Position> {
        self.debug_assert_boundary(pos);
        if pos == self.right_end() {
            return None;
        }

        let b0 = self.getb(pos);
        if b0 < 128 {
            return Some(pos + 1);
        }

        // Multibyte case.
        pos += utf8_seq_len(b0);
        self.debug_assert_boundary(pos);
        Some(pos)
    }

    #[inline(always)]
    fn next_left(&self, pos: &mut Self::Position) -> Option<Self::Element> {
        self.debug_assert_boundary(*pos);
        if *pos == self.left_end() {
            return None;
        }

        let z = self.getb(*pos - 1);
        if z < 128 {
            *pos -= 1;
            return Some(z as Self::Element);
        }

        // Multibyte case.
        // bytes are w x y z, with 'pos' pointing after z.
        let codepoint;
        let y = self.getb(*pos - 2);
        if !is_utf8_continuation(y) {
            codepoint = utf8_w2(y, z);
            *pos -= 2;
        } else {
            let x = self.getb(*pos - 3);
            if !is_utf8_continuation(x) {
                codepoint = utf8_w3(x, y, z);
                *pos -= 3;
            } else {
                let w = self.getb(*pos - 4);
                codepoint = utf8_w4(w, x, y, z);
                *pos -= 4;
            }
        }
        self.debug_assert_boundary(*pos);
        if let Some(c) = core::char::from_u32(codepoint) {
            Some(c)
        } else {
            rs_unreachable!("Should have decoded a valid char from utf8 sequence");
        }
    }

    #[inline(always)]
    fn next_left_pos(&self, mut pos: Self::Position) -> Option<Self::Position> {
        self.debug_assert_boundary(pos);
        if pos == self.left_end() {
            return None;
        }

        let z = self.getb(pos - 1);
        if z < 128 {
            pos -= 1;
            self.debug_assert_valid_pos(pos);
            return Some(pos);
        }

        if !is_utf8_continuation(self.getb(pos - 2)) {
            pos -= 2;
        } else if !is_utf8_continuation(self.getb(pos - 3)) {
            pos -= 3;
        } else {
            debug_assert!(!is_utf8_continuation(self.getb(pos - 4)));
            pos -= 4;
        }
        self.debug_assert_valid_pos(pos);
        Some(pos)
    }

    #[inline(always)]
    fn peek_byte_right(&self, pos: Self::Position) -> Option<u8> {
        self.debug_assert_valid_pos(pos);
        if pos == self.right_end() {
            None
        } else {
            Some(self.getb(pos))
        }
    }

    #[inline(always)]
    fn peek_byte_left(&self, pos: Self::Position) -> Option<u8> {
        self.debug_assert_valid_pos(pos);
        if pos == self.left_end() {
            None
        } else {
            Some(self.getb(pos - 1))
        }
    }

    #[inline(always)]
    fn try_move_right(&self, mut pos: Self::Position, amt: usize) -> Option<Self::Position> {
        self.debug_assert_valid_pos(pos);
        if self.right_end() - pos < amt {
            None
        } else {
            pos += amt;
            self.debug_assert_valid_pos(pos);
            Some(pos)
        }
    }

    #[inline(always)]
    fn try_move_left(&self, mut pos: Self::Position, amt: usize) -> Option<Self::Position> {
        self.debug_assert_valid_pos(pos);
        if pos - self.left_end() < amt {
            None
        } else {
            pos -= amt;
            self.debug_assert_valid_pos(pos);
            Some(pos)
        }
    }

    #[cfg(feature = "index-positions")]
    #[inline(always)]
    fn left_end(&self) -> Self::Position {
        Self::Position::new(0)
    }

    #[cfg(feature = "index-positions")]
    #[inline(always)]
    fn right_end(&self) -> Self::Position {
        Self::Position::new(self.bytelength())
    }

    #[cfg(not(feature = "index-positions"))]
    #[inline(always)]
    fn left_end(&self) -> Self::Position {
        Self::Position::new(self.contents().as_ptr())
    }

    #[cfg(not(feature = "index-positions"))]
    #[inline(always)]
    fn right_end(&self) -> Self::Position {
        self.left_end() + self.bytelength()
    }

    #[inline(always)]
    fn pos_to_offset(&self, pos: Self::Position) -> usize {
        debug_assert!(self.left_end() <= pos && pos <= self.right_end());
        pos - self.left_end()
    }

    #[inline(always)]
    fn find_bytes<Search: bytesearch::ByteSearcher>(
        &self,
        pos: Self::Position,
        search: &Search,
    ) -> Option<Self::Position> {
        let rem = self.slice(pos, self.right_end());
        let idx = search.find_in(rem)?;
        Some(pos + idx)
    }

    fn subrange_eq<Dir: Direction>(
        &self,
        _dir: Dir,
        pos: &mut Self::Position,
        range: Range<Self::Position>,
    ) -> bool {
        let len = range.end - range.start;
        let (start, end) = if Dir::FORWARD {
            if let Some(end) = self.try_move_right(*pos, len) {
                let start = *pos;
                *pos = end;
                (start, end)
            } else {
                return false;
            }
        } else if let Some(start) = self.try_move_left(*pos, len) {
            let end = *pos;
            *pos = start;
            (start, end)
        } else {
            return false;
        };

        #[cfg(any(feature = "index-positions", feature = "prohibit-unsafe"))]
        let new_range = &self.contents()[core::ops::Range {
            start: self.pos_to_offset(start),
            end: self.pos_to_offset(end),
        }];

        #[cfg(all(not(feature = "index-positions"), not(feature = "prohibit-unsafe")))]
        let new_range = unsafe { core::slice::from_raw_parts(start.ptr(), end - start) };

        #[cfg(any(feature = "index-positions", feature = "prohibit-unsafe"))]
        let old_range = &self.contents()[core::ops::Range {
            start: self.pos_to_offset(range.start),
            end: self.pos_to_offset(range.end),
        }];

        #[cfg(all(not(feature = "index-positions"), not(feature = "prohibit-unsafe")))]
        let old_range =
            unsafe { core::slice::from_raw_parts(range.start.ptr(), range.end - range.start) };

        new_range == old_range
    }

    fn match_bytes<const N: usize, Dir: Direction>(
        &self,
        _dir: Dir,
        pos: &mut Self::Position,
        bytes: &[u8; N],
    ) -> bool {
        let len = N;
        let (start, end) = if Dir::FORWARD {
            if let Some(end) = self.try_move_right(*pos, len) {
                let start = *pos;
                *pos = end;
                (start, end)
            } else {
                return false;
            }
        } else if let Some(start) = self.try_move_left(*pos, len) {
            let end = *pos;
            *pos = start;
            (start, end)
        } else {
            return false;
        };

        #[cfg(any(feature = "index-positions", feature = "prohibit-unsafe"))]
        let new_range = &self.contents()[core::ops::Range {
            start: self.pos_to_offset(start),
            end: self.pos_to_offset(end),
        }];

        #[cfg(all(not(feature = "index-positions"), not(feature = "prohibit-unsafe")))]
        let new_range = unsafe { core::slice::from_raw_parts(start.ptr(), end - start) };

        bytes == new_range
    }
}

#[derive(Debug, Copy, Clone)]
pub struct AsciiInput<'a> {
    input: &'a [u8],
    unicode: bool,
}

impl<'a> AsciiInput<'a> {
    #[inline(always)]
    fn contents(&self) -> &[u8] {
        self.input
    }

    #[inline(always)]
    fn bytelength(&self) -> usize {
        self.input.len()
    }

    #[inline(always)]
    fn slice(
        &self,
        start: <Self as InputIndexer>::Position,
        end: <Self as InputIndexer>::Position,
    ) -> &[u8] {
        self.debug_assert_valid_pos(start);
        self.debug_assert_valid_pos(end);

        #[cfg(any(feature = "index-positions", feature = "prohibit-unsafe"))]
        let res = &self.contents()[core::ops::Range {
            start: self.pos_to_offset(start),
            end: self.pos_to_offset(end),
        }];

        #[cfg(all(not(feature = "index-positions"), not(feature = "prohibit-unsafe")))]
        let res = unsafe { core::slice::from_raw_parts(start.ptr(), end - start) };

        debug_assert!(res.len() <= self.bytelength() && res.len() == end - start);
        res
    }

    pub fn new(s: &'a str, unicode: bool) -> Self {
        // The big idea of RefPosition is enforced here.
        <Self as InputIndexer>::Position::check_size();

        Self {
            input: s.as_bytes(),
            unicode,
        }
    }

    /// \return a byte at a given position.
    /// This asserts that we are not at the right end.
    #[inline(always)]
    fn getb(&self, pos: <Self as InputIndexer>::Position) -> u8 {
        debug_assert!(self.left_end() <= pos && pos < self.right_end());
        if cfg!(feature = "prohibit-unsafe") {
            self.contents()[self.pos_to_offset(pos)]
        } else {
            unsafe { *self.contents().get_unchecked(self.pos_to_offset(pos)) }
        }
    }

    #[inline(always)]
    fn debug_assert_valid_pos(&self, pos: <Self as InputIndexer>::Position) -> &Self {
        debug_assert!(self.left_end() <= pos && pos <= self.right_end());
        self
    }
}

impl<'a> InputIndexer for AsciiInput<'a> {
    type Position = DefPosition<'a>;
    type Element = u8;
    type CharProps = matchers::ASCIICharProperties;
    const CODE_UNITS_ARE_BYTES: bool = true;

    #[inline(always)]
    fn unicode(&self) -> bool {
        self.unicode
    }

    #[inline(always)]
    fn subinput(&self, range: ops::Range<Self::Position>) -> AsciiInput<'a> {
        self.debug_assert_valid_pos(range.start);
        self.debug_assert_valid_pos(range.end);
        debug_assert!(range.end >= range.start);
        AsciiInput {
            input: &self.input[core::ops::Range {
                start: self.pos_to_offset(range.start),
                end: self.pos_to_offset(range.end),
            }],
            unicode: self.unicode(),
        }
    }

    #[inline(always)]
    fn next_right(&self, pos: &mut Self::Position) -> Option<Self::Element> {
        self.debug_assert_valid_pos(*pos);
        if *pos == self.right_end() {
            None
        } else {
            let c = self.getb(*pos);
            *pos += 1;
            self.debug_assert_valid_pos(*pos);
            Some(c)
        }
    }

    #[inline(always)]
    fn next_left(&self, pos: &mut Self::Position) -> Option<Self::Element> {
        self.debug_assert_valid_pos(*pos);
        if *pos == self.left_end() {
            None
        } else {
            *pos -= 1;
            self.debug_assert_valid_pos(*pos);
            let c = self.getb(*pos);
            Some(c)
        }
    }

    #[inline(always)]
    fn next_right_pos(&self, pos: Self::Position) -> Option<Self::Position> {
        self.try_move_right(pos, 1)
    }

    #[inline(always)]
    fn next_left_pos(&self, pos: Self::Position) -> Option<Self::Position> {
        self.try_move_left(pos, 1)
    }

    #[inline(always)]
    fn peek_byte_right(&self, mut pos: Self::Position) -> Option<u8> {
        self.next_right(&mut pos)
    }

    #[inline(always)]
    fn peek_byte_left(&self, mut pos: Self::Position) -> Option<u8> {
        self.next_left(&mut pos)
    }

    #[cfg(feature = "index-positions")]
    #[inline(always)]
    fn left_end(&self) -> Self::Position {
        Self::Position::new(0)
    }

    #[cfg(feature = "index-positions")]
    #[inline(always)]
    fn right_end(&self) -> Self::Position {
        Self::Position::new(self.bytelength())
    }

    #[cfg(not(feature = "index-positions"))]
    #[inline(always)]
    fn left_end(&self) -> Self::Position {
        Self::Position::new(self.contents().as_ptr())
    }

    #[cfg(not(feature = "index-positions"))]
    #[inline(always)]
    fn right_end(&self) -> Self::Position {
        self.left_end() + self.bytelength()
    }

    #[inline(always)]
    fn pos_to_offset(&self, pos: Self::Position) -> usize {
        debug_assert!(self.left_end() <= pos && pos <= self.right_end());
        pos - self.left_end()
    }

    fn try_move_right(&self, mut pos: Self::Position, amt: usize) -> Option<Self::Position> {
        self.debug_assert_valid_pos(pos);
        if self.right_end() - pos < amt {
            None
        } else {
            pos += amt;
            self.debug_assert_valid_pos(pos);
            Some(pos)
        }
    }

    #[inline(always)]
    fn try_move_left(&self, mut pos: Self::Position, amt: usize) -> Option<Self::Position> {
        self.debug_assert_valid_pos(pos);
        if pos - self.left_end() < amt {
            None
        } else {
            pos -= amt;
            self.debug_assert_valid_pos(pos);
            Some(pos)
        }
    }

    #[inline(always)]
    fn find_bytes<Search: bytesearch::ByteSearcher>(
        &self,
        pos: Self::Position,
        search: &Search,
    ) -> Option<Self::Position> {
        let rem = self.slice(pos, self.right_end());
        let idx = search.find_in(rem)?;
        Some(pos + idx)
    }

    fn subrange_eq<Dir: Direction>(
        &self,
        _dir: Dir,
        pos: &mut Self::Position,
        range: Range<Self::Position>,
    ) -> bool {
        let len = range.end - range.start;
        let (start, end) = if Dir::FORWARD {
            if let Some(end) = self.try_move_right(*pos, len) {
                let start = *pos;
                *pos = end;
                (start, end)
            } else {
                return false;
            }
        } else if let Some(start) = self.try_move_left(*pos, len) {
            let end = *pos;
            *pos = start;
            (start, end)
        } else {
            return false;
        };

        #[cfg(any(feature = "index-positions", feature = "prohibit-unsafe"))]
        let new_range = &self.contents()[core::ops::Range {
            start: self.pos_to_offset(start),
            end: self.pos_to_offset(end),
        }];

        #[cfg(all(not(feature = "index-positions"), not(feature = "prohibit-unsafe")))]
        let new_range = unsafe { core::slice::from_raw_parts(start.ptr(), end - start) };

        #[cfg(any(feature = "index-positions", feature = "prohibit-unsafe"))]
        let old_range = &self.contents()[core::ops::Range {
            start: self.pos_to_offset(range.start),
            end: self.pos_to_offset(range.end),
        }];

        #[cfg(all(not(feature = "index-positions"), not(feature = "prohibit-unsafe")))]
        let old_range =
            unsafe { core::slice::from_raw_parts(range.start.ptr(), range.end - range.start) };

        new_range == old_range
    }

    fn match_bytes<const N: usize, Dir: Direction>(
        &self,
        _dir: Dir,
        pos: &mut Self::Position,
        bytes: &[u8; N],
    ) -> bool {
        let len = N;
        let (start, end) = if Dir::FORWARD {
            if let Some(end) = self.try_move_right(*pos, len) {
                let start = *pos;
                *pos = end;
                (start, end)
            } else {
                return false;
            }
        } else if let Some(start) = self.try_move_left(*pos, len) {
            let end = *pos;
            *pos = start;
            (start, end)
        } else {
            return false;
        };

        #[cfg(any(feature = "index-positions", feature = "prohibit-unsafe"))]
        let new_range = &self.contents()[core::ops::Range {
            start: self.pos_to_offset(start),
            end: self.pos_to_offset(end),
        }];

        #[cfg(all(not(feature = "index-positions"), not(feature = "prohibit-unsafe")))]
        let new_range = unsafe { core::slice::from_raw_parts(start.ptr(), end - start) };

        bytes == new_range
    }
}

#[cfg(feature = "utf16")]
#[derive(Debug, Copy, Clone)]
pub struct Utf16Input<'a> {
    input: &'a [u16],
    unicode: bool,
}

#[cfg(feature = "utf16")]
impl<'a> Utf16Input<'a> {
    pub fn new(s: &'a [u16], unicode: bool) -> Self {
        Self { input: s, unicode }
    }

    #[inline(always)]
    fn debug_assert_valid_pos(&self, pos: <Self as InputIndexer>::Position) -> &Self {
        debug_assert!(self.left_end() <= pos && pos <= self.right_end());
        self
    }
}

#[cfg(feature = "utf16")]
const SURROGATE_HIGH_START: u16 = 0xD800;
#[cfg(feature = "utf16")]
const SURROGATE_HIGH_END: u16 = 0xDBFF;
#[cfg(feature = "utf16")]
const SURROGATE_LOW_START: u16 = 0xDC00;
#[cfg(feature = "utf16")]
const SURROGATE_LOW_END: u16 = 0xDFFF;

#[cfg(feature = "utf16")]
#[inline(always)]
pub(crate) fn is_high_surrogate(b: u16) -> bool {
    b >= SURROGATE_HIGH_START && b <= SURROGATE_HIGH_END
}

#[cfg(feature = "utf16")]
#[inline(always)]
pub(crate) fn is_low_surrogate(b: u16) -> bool {
    b >= SURROGATE_LOW_START && b <= SURROGATE_LOW_END
}

#[cfg(feature = "utf16")]
#[inline(always)]
pub(crate) fn code_point_from_surrogates(high: u16, low: u16) -> u32 {
    (((high & 0x3ff) as u32) << 10 | (low & 0x3ff) as u32) + 0x1_0000
}

/// Defines a thread's pool of emptied search state for one input type, and the trait methods
/// that draw on it. The pool is keyed by the type with its lifetime erased; it only ever holds
/// empty vectors, whose layout no lifetime changes.
#[cfg(all(feature = "utf16", feature = "std"))]
macro_rules! buffer_pool {
    ($pool:ident, $input:ident) => {
        std::thread_local! {
            static $pool: core::cell::RefCell<Vec<crate::classicalbacktrack::Buffers<$input<'static>>>> =
                const { core::cell::RefCell::new(Vec::new()) };
        }
    };
}

#[cfg(all(feature = "utf16", feature = "std"))]
macro_rules! pooled_buffers {
    ($pool:ident, $input:ident, $a:lifetime) => {
        fn take_buffers() -> Option<crate::classicalbacktrack::Buffers<Self>> {
            let buffers = $pool.try_with(|p| p.borrow_mut().pop()).ok().flatten()?;
            // SAFETY: pooled buffers hold no elements, and the types differ only in a lifetime.
            Some(unsafe {
                core::mem::transmute::<
                    crate::classicalbacktrack::Buffers<$input<'static>>,
                    crate::classicalbacktrack::Buffers<$input<$a>>,
                >(buffers)
            })
        }

        fn give_buffers(buffers: crate::classicalbacktrack::Buffers<Self>) {
            // SAFETY: `MatchAttempter` empties every vector before it gives them back.
            let buffers = unsafe {
                core::mem::transmute::<
                    crate::classicalbacktrack::Buffers<$input<$a>>,
                    crate::classicalbacktrack::Buffers<$input<'static>>,
                >(buffers)
            };
            let _ = $pool.try_with(|p| {
                let mut pool = p.borrow_mut();
                if pool.len() < 8 {
                    pool.push(buffers);
                }
            });
        }
    };
}

#[cfg(feature = "utf16")]
macro_rules! unit_searches {
    () => {
        #[inline(always)]
        fn find_required_unit(
            &self,
            pos: Self::Position,
            prefilter: &crate::utf16prefilter::Utf16Prefilter,
            i: usize,
        ) -> Option<Self::Position> {
            let at = prefilter.find_required(i, self.input, self.pos_to_offset(pos))?;
            Some(Self::Position::new(at))
        }

        #[inline(always)]
        fn rfind_required_unit(
            &self,
            pos: Self::Position,
            prefilter: &crate::utf16prefilter::Utf16Prefilter,
            i: usize,
        ) -> Option<Self::Position> {
            let at = prefilter.rfind_required(i, self.input, self.pos_to_offset(pos))?;
            Some(Self::Position::new(at))
        }

        #[inline(always)]
        fn find_factor(
            &self,
            pos: Self::Position,
            factors: &crate::utf16factors::Factors,
        ) -> Option<Self::Position> {
            let at = factors.find(self.input, self.pos_to_offset(pos))?;
            Some(Self::Position::new(at))
        }

        #[inline(always)]
        fn rfind_factor(
            &self,
            pos: Self::Position,
            factors: &crate::utf16factors::Factors,
        ) -> Option<Self::Position> {
            let at = factors.rfind(self.input, self.pos_to_offset(pos))?;
            Some(Self::Position::new(at))
        }

        #[inline(always)]
        fn factor_run(
            &self,
            pos: Self::Position,
            at: Self::Position,
            factors: &crate::utf16factors::Factors,
        ) -> Option<(Self::Position, Self::Position)> {
            let (start, end) =
                factors.run(self.input, self.pos_to_offset(pos), self.pos_to_offset(at))?;
            Some((Self::Position::new(start), Self::Position::new(end)))
        }
    };
}

#[cfg(all(feature = "utf16", feature = "std"))]
buffer_pool!(UTF16_BUFFERS, Utf16Input);
#[cfg(all(feature = "utf16", feature = "std"))]
buffer_pool!(UCS2_BUFFERS, Ucs2Input);

#[cfg(feature = "utf16")]
impl<'a> InputIndexer for Utf16Input<'a> {
    type Position = IndexPosition<'a>;
    type Element = u32;
    type CharProps = matchers::Utf16CharProperties;
    const CODE_UNITS_ARE_BYTES: bool = false;

    #[inline(always)]
    fn unicode(&self) -> bool {
        self.unicode
    }

    #[cfg(feature = "std")]
    pooled_buffers!(UTF16_BUFFERS, Utf16Input, 'a);

    #[inline(always)]
    fn code_units(&self) -> &[u16] {
        self.input
    }

    #[inline(always)]
    fn subinput(&self, range: ops::Range<Self::Position>) -> Utf16Input<'a> {
        self.debug_assert_valid_pos(range.start);
        self.debug_assert_valid_pos(range.end);
        debug_assert!(range.end >= range.start);
        Utf16Input {
            input: &self.input[core::ops::Range {
                start: self.pos_to_offset(range.start),
                end: self.pos_to_offset(range.end),
            }],
            unicode: self.unicode(),
        }
    }

    #[inline(always)]
    fn next_right(&self, pos: &mut Self::Position) -> Option<Self::Element> {
        let u1 = self.input.get(self.pos_to_offset(*pos)).copied()?;
        *pos += 1;

        // If the code unit is not a high surrogate, it is not the start of a surrogate pair.
        if !is_high_surrogate(u1) {
            return Some(u1.into());
        }

        let Some(u2) = self.input.get(self.pos_to_offset(*pos)).copied() else {
            return Some(u1.into());
        };

        // If the code unit is not a low surrogate, it is not a surrogate pair.
        if !is_low_surrogate(u2) {
            return Some(u1.into());
        }

        *pos += 1;
        Some(code_point_from_surrogates(u1, u2))
    }

    #[inline(always)]
    fn next_left(&self, pos: &mut Self::Position) -> Option<Self::Element> {
        let left_end = self.left_end();
        if *pos == left_end {
            return None;
        }

        let u2 = self.input.get(self.pos_to_offset(*pos - 1)).copied()?;
        *pos -= 1;

        // If the code unit is not a low surrogate, it is not the end of a surrogate pair.
        if *pos == left_end || !is_low_surrogate(u2) {
            return Some(u2.into());
        }

        let Some(u1) = self.input.get(self.pos_to_offset(*pos - 1)).copied() else {
            return Some(u2.into());
        };

        // If the code unit is not a high surrogate, it is not a surrogate pair.
        if !is_high_surrogate(u1) {
            return Some(u2.into());
        }

        *pos -= 1;
        Some(code_point_from_surrogates(u1, u2))
    }

    #[inline(always)]
    fn next_right_pos(&self, mut pos: Self::Position) -> Option<Self::Position> {
        let u1 = self.input.get(self.pos_to_offset(pos)).copied()?;
        pos += 1;

        // If the code unit is not a high surrogate, it is not the start of a surrogate pair.
        if !is_high_surrogate(u1) {
            return Some(pos);
        }

        let Some(u2) = self.input.get(self.pos_to_offset(pos)).copied() else {
            return Some(pos);
        };

        // If the code unit is not a low surrogate, it is not a surrogate pair.
        if !is_low_surrogate(u2) {
            return Some(pos);
        }

        pos += 1;

        Some(pos)
    }

    #[inline(always)]
    fn next_left_pos(&self, mut pos: Self::Position) -> Option<Self::Position> {
        let left_end = self.left_end();
        if pos == left_end {
            return None;
        }

        let u2 = self.input.get(self.pos_to_offset(pos - 1)).copied()?;
        pos -= 1;

        // If the code unit is not a low surrogate, it is not the end of a surrogate pair.
        if pos == left_end || !is_low_surrogate(u2) {
            return Some(pos);
        }

        let Some(u1) = self.input.get(self.pos_to_offset(pos - 1)).copied() else {
            return Some(pos);
        };

        // If the code unit is not a high surrogate, it is not a surrogate pair.
        if !is_high_surrogate(u1) {
            return Some(pos);
        }

        pos -= 1;
        Some(pos)
    }

    #[inline(always)]
    fn peek_byte_right(&self, _pos: Self::Position) -> Option<u8> {
        panic!("Should never be inspecting bytes for utf16");
    }

    #[inline(always)]
    fn peek_byte_left(&self, _pos: Self::Position) -> Option<u8> {
        panic!("Should never be inspecting bytes for utf16");
    }

    #[inline(always)]
    fn left_end(&self) -> Self::Position {
        Self::Position::new(0)
    }

    #[inline(always)]
    fn right_end(&self) -> Self::Position {
        Self::Position::new(self.input.len())
    }

    #[inline(always)]
    fn pos_to_offset(&self, pos: Self::Position) -> usize {
        debug_assert!(self.left_end() <= pos && pos <= self.right_end());
        pos - self.left_end()
    }

    fn try_move_right(&self, mut pos: Self::Position, amt: usize) -> Option<Self::Position> {
        self.debug_assert_valid_pos(pos);
        if self.right_end() - pos < amt {
            None
        } else {
            pos += amt;
            self.debug_assert_valid_pos(pos);
            Some(pos)
        }
    }

    #[inline(always)]
    fn try_move_left(&self, mut pos: Self::Position, amt: usize) -> Option<Self::Position> {
        self.debug_assert_valid_pos(pos);
        if pos - self.left_end() < amt {
            None
        } else {
            pos -= amt;
            self.debug_assert_valid_pos(pos);
            Some(pos)
        }
    }

    #[inline(always)]
    fn find_bytes<Search: bytesearch::ByteSearcher>(
        &self,
        _pos: Self::Position,
        _search: &Search,
    ) -> Option<Self::Position> {
        panic!("Should never be finding bytes for utf16");
    }

    unit_searches!();

    // Positions step over surrogate pairs, so a low surrogate right of a high one is no start,
    // unless the search starts there.
    #[inline(always)]
    fn find_start_unit(
        &self,
        pos: Self::Position,
        limit: Self::Position,
        prefilter: &crate::utf16prefilter::Utf16Prefilter,
        scan: &mut crate::utf16prefilter::UnitScan,
    ) -> Option<Self::Position> {
        let from = self.pos_to_offset(pos);
        let limit = self.pos_to_offset(limit);
        let mut at = from;
        loop {
            at = prefilter.find_start(self.input, at, limit, scan)?;
            if at == from
                || self
                    .input
                    .get(at)
                    .is_none_or(|&unit| !is_low_surrogate(unit))
                || !is_high_surrogate(self.input[at - 1])
            {
                return Some(Self::Position::new(at));
            }
            at += 1;
        }
    }

    fn subrange_eq<Dir: Direction>(
        &self,
        _dir: Dir,
        pos: &mut Self::Position,
        range: Range<Self::Position>,
    ) -> bool {
        let len = range.end - range.start;
        let (start, end) = if Dir::FORWARD {
            if let Some(end) = self.try_move_right(*pos, len) {
                let start = *pos;
                *pos = end;
                (start, end)
            } else {
                return false;
            }
        } else if let Some(start) = self.try_move_left(*pos, len) {
            let end = *pos;
            *pos = start;
            (start, end)
        } else {
            return false;
        };

        let new_range = &self.input[self.pos_to_offset(start)..self.pos_to_offset(end)];
        let old_range = &self.input[self.pos_to_offset(range.start)..self.pos_to_offset(range.end)];

        new_range == old_range
    }

    fn match_bytes<const N: usize, Dir: Direction>(
        &self,
        _dir: Dir,
        _pos: &mut Self::Position,
        _bytes: &[u8; N],
    ) -> bool {
        panic!("Should never be matching bytes for utf16");
    }
}

#[cfg(feature = "utf16")]
#[derive(Debug, Copy, Clone)]
pub struct Ucs2Input<'a> {
    input: &'a [u16],
    unicode: bool,
}

#[cfg(feature = "utf16")]
impl<'a> Ucs2Input<'a> {
    pub fn new(s: &'a [u16], unicode: bool) -> Self {
        Self { input: s, unicode }
    }

    #[inline(always)]
    fn debug_assert_valid_pos(&self, pos: <Self as InputIndexer>::Position) -> &Self {
        debug_assert!(self.left_end() <= pos && pos <= self.right_end());
        self
    }
}

#[cfg(feature = "utf16")]
impl<'a> InputIndexer for Ucs2Input<'a> {
    type Position = IndexPosition<'a>;
    type Element = u32;
    type CharProps = matchers::Utf16CharProperties;
    const CODE_UNITS_ARE_BYTES: bool = false;

    #[inline(always)]
    fn unicode(&self) -> bool {
        self.unicode
    }

    #[cfg(feature = "std")]
    pooled_buffers!(UCS2_BUFFERS, Ucs2Input, 'a);

    #[inline(always)]
    fn code_units(&self) -> &[u16] {
        self.input
    }

    #[inline(always)]
    fn subinput(&self, range: ops::Range<Self::Position>) -> Ucs2Input<'a> {
        self.debug_assert_valid_pos(range.start);
        self.debug_assert_valid_pos(range.end);
        debug_assert!(range.end >= range.start);
        Ucs2Input {
            input: &self.input[core::ops::Range {
                start: self.pos_to_offset(range.start),
                end: self.pos_to_offset(range.end),
            }],
            unicode: self.unicode(),
        }
    }

    #[inline(always)]
    fn next_right(&self, pos: &mut Self::Position) -> Option<Self::Element> {
        let u1 = self.input.get(self.pos_to_offset(*pos)).copied()?;
        *pos += 1;

        Some(u1.into())
    }

    #[inline(always)]
    fn next_left(&self, pos: &mut Self::Position) -> Option<Self::Element> {
        if *pos == self.left_end() {
            return None;
        }

        let u2 = self.input.get(self.pos_to_offset(*pos - 1)).copied()?;
        *pos -= 1;

        Some(u2.into())
    }

    #[inline(always)]
    fn next_right_pos(&self, pos: Self::Position) -> Option<Self::Position> {
        self.try_move_right(pos, 1)
    }

    #[inline(always)]
    fn next_left_pos(&self, pos: Self::Position) -> Option<Self::Position> {
        self.try_move_left(pos, 1)
    }

    #[inline(always)]
    fn peek_byte_right(&self, _pos: Self::Position) -> Option<u8> {
        panic!("Should never be inspecting bytes for ucs2");
    }

    #[inline(always)]
    fn peek_byte_left(&self, _pos: Self::Position) -> Option<u8> {
        panic!("Should never be inspecting bytes for ucs2");
    }

    #[inline(always)]
    fn left_end(&self) -> Self::Position {
        Self::Position::new(0)
    }

    #[inline(always)]
    fn right_end(&self) -> Self::Position {
        Self::Position::new(self.input.len())
    }

    #[inline(always)]
    fn pos_to_offset(&self, pos: Self::Position) -> usize {
        debug_assert!(self.left_end() <= pos && pos <= self.right_end());
        pos - self.left_end()
    }

    fn try_move_right(&self, mut pos: Self::Position, amt: usize) -> Option<Self::Position> {
        self.debug_assert_valid_pos(pos);
        if self.right_end() - pos < amt {
            None
        } else {
            pos += amt;
            self.debug_assert_valid_pos(pos);
            Some(pos)
        }
    }

    #[inline(always)]
    fn try_move_left(&self, mut pos: Self::Position, amt: usize) -> Option<Self::Position> {
        self.debug_assert_valid_pos(pos);
        if pos - self.left_end() < amt {
            None
        } else {
            pos -= amt;
            self.debug_assert_valid_pos(pos);
            Some(pos)
        }
    }

    #[inline(always)]
    fn find_bytes<Search: bytesearch::ByteSearcher>(
        &self,
        _pos: Self::Position,
        _search: &Search,
    ) -> Option<Self::Position> {
        panic!("Should never be finding bytes for ucs2");
    }

    unit_searches!();

    // Every unit is a character here, so a start is any unit in the set.
    #[inline(always)]
    fn find_start_unit(
        &self,
        pos: Self::Position,
        limit: Self::Position,
        prefilter: &crate::utf16prefilter::Utf16Prefilter,
        scan: &mut crate::utf16prefilter::UnitScan,
    ) -> Option<Self::Position> {
        let at = prefilter.find_start(
            self.input,
            self.pos_to_offset(pos),
            self.pos_to_offset(limit),
            scan,
        )?;
        Some(Self::Position::new(at))
    }

    fn subrange_eq<Dir: Direction>(
        &self,
        _dir: Dir,
        pos: &mut Self::Position,
        range: Range<Self::Position>,
    ) -> bool {
        let len = range.end - range.start;
        let (start, end) = if Dir::FORWARD {
            if let Some(end) = self.try_move_right(*pos, len) {
                let start = *pos;
                *pos = end;
                (start, end)
            } else {
                return false;
            }
        } else if let Some(start) = self.try_move_left(*pos, len) {
            let end = *pos;
            *pos = start;
            (start, end)
        } else {
            return false;
        };

        let new_range = &self.input[self.pos_to_offset(start)..self.pos_to_offset(end)];
        let old_range = &self.input[self.pos_to_offset(range.start)..self.pos_to_offset(range.end)];

        new_range == old_range
    }

    fn match_bytes<const N: usize, Dir: Direction>(
        &self,
        _dir: Dir,
        _pos: &mut Self::Position,
        _bytes: &[u8; N],
    ) -> bool {
        panic!("Should never be matching bytes for ucs2");
    }
}
