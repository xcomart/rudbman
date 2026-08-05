//! The document: a rope, plus the three coordinate systems that have to agree
//! about it.
//!
//! Everything above this module addresses the buffer in **byte offsets**, the
//! same choice `rudbman-ui`'s text field made and for the same reason: the SQL
//! lexer returns byte spans, so a caret kept in bytes needs no translation to
//! be compared against a token. The other two systems exist because someone
//! else insists on them.
//!
//! * **Lines.** The syntax cache is addressed by line and the renderer draws by
//!   line, so `byte <-> (line, column)` has to be cheap. A rope answers it in
//!   O(log n); a flat `String` would answer it by counting newlines.
//! * **UTF-16 code units.** Every platform IME speaks UTF-16 — see
//!   [`crate::editor`] for the whole of that argument — and a 100k-line buffer
//!   cannot afford the linear walk a single-line field gets away with. Ropey
//!   keeps a UTF-16 length metric alongside its byte and char metrics, so
//!   [`Buffer::offset_to_utf16`] and [`Buffer::offset_from_utf16`] are two
//!   O(log n) index lookups each.
//!
//! Grapheme boundaries are the fourth thing, and they are not a coordinate
//! system but a predicate on byte offsets: which offsets a caret is allowed to
//! stop at. They are computed with [`GraphemeCursor`] fed chunks straight out
//! of the rope, rather than by running `grapheme_indices` over a line, so that
//! finding the boundary next to an offset costs the length of one grapheme and
//! not the length of the line it is in.

use std::borrow::Cow;
use std::ops::Range;

use ropey::{Rope, RopeSlice};
use unicode_segmentation::{GraphemeCursor, GraphemeIncomplete};

/// The editor's text, with the indexes the layers above it need.
#[derive(Clone, Debug, Default)]
pub struct Buffer {
    /// The text itself.
    rope: Rope,
}

impl Buffer {
    /// A buffer holding `text`.
    pub fn new(text: &str) -> Self {
        Self {
            rope: Rope::from_str(text),
        }
    }

    /// The whole text as a `String`.
    ///
    /// O(n) and an allocation the size of the buffer, so this is for saving and
    /// for tests. Nothing on the editing or drawing path calls it.
    pub fn text(&self) -> String {
        self.rope.to_string()
    }

    /// The rope, for the callers that want to slice it without copying.
    pub const fn rope(&self) -> &Rope {
        &self.rope
    }

    /// Length of the buffer in bytes.
    pub fn len(&self) -> usize {
        self.rope.len_bytes()
    }

    /// Whether the buffer holds no text at all.
    pub fn is_empty(&self) -> bool {
        self.rope.len_bytes() == 0
    }

    /// Number of lines.
    ///
    /// A buffer ending in a newline has an empty last line, and an empty buffer
    /// has one line, which is what an editor draws in both cases.
    pub fn line_count(&self) -> usize {
        self.rope.len_lines()
    }

    /// Byte offset of the first character of `line`.
    ///
    /// Clamped, so a line index past the end answers with the end of the
    /// buffer rather than panicking.
    pub fn line_start(&self, line: usize) -> usize {
        if line >= self.line_count() {
            return self.len();
        }
        self.rope.line_to_byte(line)
    }

    /// Byte offset one past the last character of `line`, the line terminator
    /// excluded.
    pub fn line_end(&self, line: usize) -> usize {
        let slice = self.line_slice(line);
        self.line_start(line) + trim_line_break(slice).len_bytes()
    }

    /// The text of `line` without its line terminator.
    ///
    /// Borrowed when the line sits inside one rope chunk, which is the usual
    /// case; copied when it straddles two.
    pub fn line_text(&self, line: usize) -> Cow<'_, str> {
        trim_line_break(self.line_slice(line)).into()
    }

    /// The line `offset` falls on.
    pub fn line_of(&self, offset: usize) -> usize {
        self.rope.byte_to_line(offset.min(self.len()))
    }

    /// The `(line, byte column)` of `offset`.
    pub fn point_of(&self, offset: usize) -> (usize, usize) {
        let line = self.line_of(offset);
        (line, offset - self.line_start(line))
    }

    /// The byte offset of `column` bytes into `line`, clamped to the line.
    pub fn offset_of(&self, line: usize, column: usize) -> usize {
        let start = self.line_start(line);
        let end = self.line_end(line);
        (start + column).min(end)
    }

    /// The text of `range`, as a `String`.
    ///
    /// # Panics
    ///
    /// If `range` is out of bounds or off a character boundary.
    pub fn slice(&self, range: Range<usize>) -> String {
        self.rope.byte_slice(range).to_string()
    }

    /// Replaces `range` with `text`.
    ///
    /// # Panics
    ///
    /// If `range` is out of bounds or off a character boundary.
    pub fn replace(&mut self, range: Range<usize>, text: &str) {
        let start = self.rope.byte_to_char(range.start);
        let end = self.rope.byte_to_char(range.end);
        if start != end {
            self.rope.remove(start..end);
        }
        if !text.is_empty() {
            self.rope.insert(start, text);
        }
    }

    // --- UTF-16, for the platform input handler ------------------------------

    /// Converts a byte offset into the UTF-16 code unit offset the platform
    /// IME talks in.
    pub fn offset_to_utf16(&self, offset: usize) -> usize {
        let char_idx = self.rope.byte_to_char(offset.min(self.len()));
        self.rope.char_to_utf16_cu(char_idx)
    }

    /// Converts a UTF-16 code unit offset back into a byte offset.
    ///
    /// An offset that lands inside a surrogate pair resolves to the start of
    /// the character it splits, which is the only answer that is a valid byte
    /// offset at all.
    pub fn offset_from_utf16(&self, offset_utf16: usize) -> usize {
        let clamped = offset_utf16.min(self.rope.len_utf16_cu());
        let char_idx = self.rope.utf16_cu_to_char(clamped);
        self.rope.char_to_byte(char_idx)
    }

    /// [`Self::offset_to_utf16`] over both ends of a range.
    pub fn range_to_utf16(&self, range: &Range<usize>) -> Range<usize> {
        self.offset_to_utf16(range.start)..self.offset_to_utf16(range.end)
    }

    /// [`Self::offset_from_utf16`] over both ends of a range.
    pub fn range_from_utf16(&self, range_utf16: &Range<usize>) -> Range<usize> {
        self.offset_from_utf16(range_utf16.start)..self.offset_from_utf16(range_utf16.end)
    }

    // --- grapheme boundaries -------------------------------------------------

    /// The grapheme boundary before `offset`, or `0`.
    pub fn prev_grapheme(&self, offset: usize) -> usize {
        let offset = offset.min(self.len());
        if offset == 0 {
            return 0;
        }
        let mut cursor = GraphemeCursor::new(offset, self.len(), true);
        let (mut chunk, mut chunk_start, _, _) = self.rope.chunk_at_byte(offset);
        loop {
            match cursor.prev_boundary(chunk, chunk_start) {
                Ok(None) => return 0,
                Ok(Some(boundary)) => return boundary,
                Err(GraphemeIncomplete::PrevChunk) => {
                    if chunk_start == 0 {
                        return 0;
                    }
                    let (c, start, _, _) = self.rope.chunk_at_byte(chunk_start - 1);
                    chunk = c;
                    chunk_start = start;
                }
                Err(GraphemeIncomplete::PreContext(end)) => {
                    let (context, start, _, _) = self.rope.chunk_at_byte(end.saturating_sub(1));
                    cursor.provide_context(context, start);
                }
                // The remaining variants are answers to `is_boundary`, which
                // this call never asks.
                Err(_) => return offset.saturating_sub(1),
            }
        }
    }

    /// The grapheme boundary after `offset`, or the end of the buffer.
    pub fn next_grapheme(&self, offset: usize) -> usize {
        let len = self.len();
        let offset = offset.min(len);
        if offset == len {
            return len;
        }
        let mut cursor = GraphemeCursor::new(offset, len, true);
        let (mut chunk, mut chunk_start, _, _) = self.rope.chunk_at_byte(offset);
        loop {
            match cursor.next_boundary(chunk, chunk_start) {
                Ok(None) => return len,
                Ok(Some(boundary)) => return boundary,
                Err(GraphemeIncomplete::NextChunk) => {
                    let next = chunk_start + chunk.len();
                    if next >= len {
                        return len;
                    }
                    let (c, start, _, _) = self.rope.chunk_at_byte(next);
                    chunk = c;
                    chunk_start = start;
                }
                Err(GraphemeIncomplete::PreContext(end)) => {
                    let (context, start, _, _) = self.rope.chunk_at_byte(end.saturating_sub(1));
                    cursor.provide_context(context, start);
                }
                Err(_) => return (offset + 1).min(len),
            }
        }
    }

    /// The number of graphemes between the start of `offset`'s line and
    /// `offset` — the column a vertical move aims for.
    pub fn grapheme_column(&self, offset: usize) -> usize {
        let line = self.line_of(offset);
        let mut at = self.line_start(line);
        let mut column = 0;
        while at < offset {
            at = self.next_grapheme(at);
            column += 1;
        }
        column
    }

    /// The offset `column` graphemes into `line`, clamped to the end of it.
    pub fn offset_at_column(&self, line: usize, column: usize) -> usize {
        let end = self.line_end(line);
        let mut at = self.line_start(line);
        for _ in 0..column {
            if at >= end {
                return end;
            }
            at = self.next_grapheme(at);
        }
        at.min(end)
    }

    // --- words, for double click and the word-wise keys -----------------------

    /// The word around `offset`, or the run of whitespace or punctuation there.
    ///
    /// Never empty unless the buffer is, so a double click always selects
    /// something.
    pub fn word_at(&self, offset: usize) -> Range<usize> {
        let line = self.line_of(offset);
        let start = self.line_start(line);
        let text = self.line_text(line);
        let column = offset - start;
        let bytes = text.as_bytes();
        if bytes.is_empty() {
            return start..start;
        }

        // A click at the very end of a line takes the character before it,
        // which is what makes double clicking after the last word select it.
        let probe = if column >= bytes.len() {
            bytes.len() - 1
        } else {
            column
        };
        let class = char_class(bytes[probe]);

        let mut from = probe;
        while from > 0 && char_class(bytes[from - 1]) == class {
            from -= 1;
        }
        let mut to = probe + 1;
        while to < bytes.len() && char_class(bytes[to]) == class {
            to += 1;
        }
        // The scan is byte-wise, so a multi-byte character lands wholly inside
        // one class run; snapping to grapheme boundaries is what keeps the
        // range sliceable when it does not.
        let from = start + from;
        let to = start + to;
        self.snap_back(from)..self.snap_forward(to)
    }

    /// The start of the word before `offset`, for `ctrl-left`.
    pub fn prev_word(&self, offset: usize) -> usize {
        let line = self.line_of(offset);
        let start = self.line_start(line);
        if offset <= start {
            // Already at the head of a line: step over the break.
            return if line == 0 {
                0
            } else {
                self.line_end(line - 1)
            };
        }
        let text = self.line_text(line);
        let bytes = text.as_bytes();
        let mut at = (offset - start).min(bytes.len());
        while at > 0 && char_class(bytes[at - 1]) == CharClass::Space {
            at -= 1;
        }
        if at > 0 {
            let class = char_class(bytes[at - 1]);
            while at > 0 && char_class(bytes[at - 1]) == class {
                at -= 1;
            }
        }
        self.snap_back(start + at)
    }

    /// The end of the word after `offset`, for `ctrl-right`.
    pub fn next_word(&self, offset: usize) -> usize {
        let line = self.line_of(offset);
        let end = self.line_end(line);
        if offset >= end {
            // Already at the tail of a line: step over the break.
            return if line + 1 >= self.line_count() {
                self.len()
            } else {
                self.line_start(line + 1)
            };
        }
        let start = self.line_start(line);
        let text = self.line_text(line);
        let bytes = text.as_bytes();
        let mut at = offset - start;
        while at < bytes.len() && char_class(bytes[at]) == CharClass::Space {
            at += 1;
        }
        if at < bytes.len() {
            let class = char_class(bytes[at]);
            while at < bytes.len() && char_class(bytes[at]) == class {
                at += 1;
            }
        }
        self.snap_forward(start + at)
    }

    /// The line's slice, terminator included.
    fn line_slice(&self, line: usize) -> RopeSlice<'_> {
        if line >= self.line_count() {
            return self.rope.byte_slice(self.len()..self.len());
        }
        self.rope.line(line)
    }

    /// `offset`, or the character boundary before it.
    fn snap_back(&self, offset: usize) -> usize {
        let offset = offset.min(self.len());
        let char_idx = self.rope.byte_to_char(offset);
        self.rope.char_to_byte(char_idx)
    }

    /// `offset`, or the character boundary after it.
    fn snap_forward(&self, offset: usize) -> usize {
        let offset = offset.min(self.len());
        let snapped = self.snap_back(offset);
        if snapped == offset {
            offset
        } else {
            self.next_grapheme(snapped)
        }
    }
}

/// What a byte counts as when a word is being picked out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CharClass {
    /// Spaces and tabs.
    Space,
    /// Letters, digits, `_`, `$`, and every byte of a multi-byte character —
    /// an identifier in Korean is one word, not one word per byte.
    Word,
    /// Everything else: operators, brackets, quotes.
    Symbol,
}

/// Classifies one byte.
const fn char_class(byte: u8) -> CharClass {
    match byte {
        b' ' | b'\t' => CharClass::Space,
        b'0'..=b'9' | b'a'..=b'z' | b'A'..=b'Z' | b'_' | b'$' => CharClass::Word,
        // Everything from 0x80 up is a continuation or lead byte of a
        // non-ASCII character, and those are word constituents here.
        0x80..=0xff => CharClass::Word,
        _ => CharClass::Symbol,
    }
}

/// The slice without its trailing `\n` or `\r\n`.
fn trim_line_break(slice: RopeSlice<'_>) -> RopeSlice<'_> {
    let len = slice.len_bytes();
    if len == 0 {
        return slice;
    }
    let mut end = len;
    if slice.byte(end - 1) == b'\n' {
        end -= 1;
        if end > 0 && slice.byte(end - 1) == b'\r' {
            end -= 1;
        }
    }
    slice.byte_slice(..end)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lines_exclude_their_terminator() {
        let buffer = Buffer::new("select 1\nfrom t\r\nwhere x\n");
        assert_eq!(buffer.line_count(), 4);
        assert_eq!(buffer.line_text(0), "select 1");
        assert_eq!(buffer.line_text(1), "from t");
        assert_eq!(buffer.line_text(2), "where x");
        assert_eq!(buffer.line_text(3), "");
        assert_eq!(buffer.line_end(1), buffer.line_start(1) + 6);
    }

    #[test]
    fn an_empty_buffer_still_has_a_line() {
        let buffer = Buffer::new("");
        assert_eq!(buffer.line_count(), 1);
        assert_eq!(buffer.line_text(0), "");
        assert!(buffer.is_empty());
    }

    #[test]
    fn a_hangul_syllable_is_one_cursor_step() {
        let buffer = Buffer::new("한글");
        assert_eq!(buffer.next_grapheme(0), 3);
        assert_eq!(buffer.next_grapheme(3), 6);
        assert_eq!(buffer.prev_grapheme(6), 3);
        assert_eq!(buffer.prev_grapheme(3), 0);
    }

    #[test]
    fn a_joined_emoji_is_one_cursor_step() {
        // Family: man + ZWJ + woman + ZWJ + girl, eleven code points of it.
        let family = "\u{1f468}\u{200d}\u{1f469}\u{200d}\u{1f467}";
        let buffer = Buffer::new(family);
        assert_eq!(buffer.next_grapheme(0), family.len());
        assert_eq!(buffer.prev_grapheme(family.len()), 0);
    }

    #[test]
    fn utf16_offsets_round_trip_across_the_planes() {
        let buffer = Buffer::new("a한\u{1f600}b");
        // 'a' is one unit, '한' one, the emoji a surrogate pair, 'b' one.
        assert_eq!(buffer.offset_to_utf16(0), 0);
        assert_eq!(buffer.offset_to_utf16(1), 1);
        assert_eq!(buffer.offset_to_utf16(4), 2);
        assert_eq!(buffer.offset_to_utf16(8), 4);
        assert_eq!(buffer.offset_to_utf16(9), 5);
        // Round trips everywhere except the inside of the surrogate pair,
        // which is not a byte offset at all and resolves to the start of the
        // character it splits.
        for utf16 in [0, 1, 2, 4, 5] {
            let byte = buffer.offset_from_utf16(utf16);
            assert_eq!(buffer.offset_to_utf16(byte), utf16);
        }
        assert_eq!(buffer.offset_from_utf16(3), 4);
    }

    #[test]
    fn replacing_a_range_keeps_the_line_index_honest() {
        let mut buffer = Buffer::new("select 1\nselect 2\n");
        buffer.replace(9..15, "insert");
        assert_eq!(buffer.text(), "select 1\ninsert 2\n");
        buffer.replace(8..8, "\nunion all");
        assert_eq!(buffer.line_count(), 4);
        assert_eq!(buffer.line_text(1), "union all");
    }

    #[test]
    fn a_word_is_a_run_of_one_class() {
        let buffer = Buffer::new("select count(*) from t");
        assert_eq!(buffer.word_at(9), 7..12);
        // `(*)` is one run of symbols; a token-aware selection is not what a
        // double click means.
        assert_eq!(buffer.word_at(12), 12..15);
        assert_eq!(buffer.word_at(21), 21..22);
    }

    #[test]
    fn word_motion_steps_over_line_breaks() {
        let buffer = Buffer::new("select a\nfrom t");
        assert_eq!(buffer.next_word(6), 8);
        assert_eq!(buffer.next_word(8), 9);
        assert_eq!(buffer.prev_word(9), 8);
        assert_eq!(buffer.prev_word(8), 7);
    }

    #[test]
    fn columns_are_counted_in_graphemes() {
        let buffer = Buffer::new("한글 sql\nx");
        let offset = buffer.line_start(0) + 7;
        assert_eq!(buffer.grapheme_column(offset), 3);
        assert_eq!(buffer.offset_at_column(0, 3), offset);
        // Past the end of the line clamps rather than running on.
        assert_eq!(buffer.offset_at_column(1, 99), buffer.line_end(1));
    }
}
