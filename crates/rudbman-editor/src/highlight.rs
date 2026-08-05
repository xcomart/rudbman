//! The syntax cache: one [`LineState`] per line, and nothing else.
//!
//! What is *not* cached here is the tokens. Holding a `Vec<Token>` for every
//! line of a 100MB script would cost more than the script does, and it would
//! buy nothing: [`lex_line`] over one line is a few hundred nanoseconds, and
//! the renderer only ever needs the tokens of the forty lines it is about to
//! draw. So the cache holds the sixteen bytes per line that *cannot* be
//! recomputed locally — the state each line ends in — and the tokens are lexed
//! on demand from the state of the line before.
//!
//! That is the whole design, and it is the one `rudbman-sql` was built for.
//! After an edit on line *n*, [`Highlighter::edited`] re-lexes from *n*
//! downwards and stops at the first line whose new end state equals the one it
//! had. For an edit that opens no comment and no quote — which is nearly every
//! edit — that is one line, whatever the buffer's length. Typing `/*` on line
//! three of a hundred thousand walks down until the states stop changing, which
//! is either the line that closes the comment or the end of the file; typing
//! the `*/` that closes it walks back down again. Nothing else in the editor
//! touches a line it is not drawing.
//!
//! [`Highlighter::lex_calls`] counts the calls, which is how the tests hold the
//! two claims above down: that drawing costs one call per visible line, and
//! that an ordinary edit costs one call.

use std::cell::Cell;

use rudbman_sql::{Dialect, LineState, Token, lex_line};

use crate::buffer::Buffer;

/// Per-line syntax state, kept in step with a [`Buffer`].
#[derive(Debug)]
pub struct Highlighter {
    /// The rules in force.
    dialect: Dialect,
    /// `ends[i]` is the state line `i` ends in. Always as long as the buffer
    /// has lines.
    ends: Vec<LineState>,
    /// How many times [`lex_line`] has been called through this cache.
    ///
    /// A [`Cell`] so that [`Highlighter::tokens`] can stay `&self` and be
    /// called from an element's prepaint, which holds the view by shared
    /// reference.
    lex_calls: Cell<usize>,
}

impl Highlighter {
    /// Builds the cache for `buffer`, lexing all of it once.
    ///
    /// The one linear pass in this module. It happens when a file is opened or
    /// [`Highlighter::reset`] is called, and never on an edit.
    pub fn new(buffer: &Buffer, dialect: Dialect) -> Self {
        let mut this = Self {
            dialect,
            ends: Vec::new(),
            lex_calls: Cell::new(0),
        };
        this.reset(buffer);
        this
    }

    /// The dialect the cache was built with.
    pub const fn dialect(&self) -> Dialect {
        self.dialect
    }

    /// Switches dialect and re-lexes the buffer.
    ///
    /// A dialect change moves what a `#` means and whether `"x"` is a string,
    /// so there is no incremental path here and no need for one: it happens
    /// when a session's driver is picked, not while anyone is typing.
    pub fn set_dialect(&mut self, dialect: Dialect, buffer: &Buffer) {
        if self.dialect.id() == dialect.id() {
            return;
        }
        self.dialect = dialect;
        self.reset(buffer);
    }

    /// Discards the cache and rebuilds it from `buffer`.
    pub fn reset(&mut self, buffer: &Buffer) {
        let lines = buffer.line_count();
        self.ends.clear();
        self.ends.reserve(lines);
        let mut state = LineState::START;
        for line in 0..lines {
            state = self.lex_end_state(buffer, line, state);
            self.ends.push(state);
        }
    }

    /// The state line `line` starts in.
    pub fn start_state(&self, line: usize) -> LineState {
        match line.checked_sub(1) {
            None => LineState::START,
            Some(previous) => self.ends.get(previous).copied().unwrap_or_default(),
        }
    }

    /// The state line `line` ends in.
    pub fn end_state(&self, line: usize) -> LineState {
        self.ends.get(line).copied().unwrap_or_default()
    }

    /// The tokens of `line`, lexed from the cached start state.
    ///
    /// Offsets are relative to the start of the line. Cheap enough to call
    /// once per visible line per frame, which is exactly how the renderer uses
    /// it.
    pub fn tokens(&self, buffer: &Buffer, line: usize) -> Vec<Token> {
        let text = buffer.line_text(line);
        self.lex_calls.set(self.lex_calls.get() + 1);
        lex_line(&text, self.start_state(line), &self.dialect).0
    }

    /// Brings the cache back into step after `buffer` changed.
    ///
    /// `first` is the first line the edit touched and `added` the number of
    /// lines the replacement spans, both after the edit; `removed` is how many
    /// lines the replaced text spanned before it. Returns the number of lines
    /// that had to be re-lexed, which is what the performance tests read.
    pub fn edited(&mut self, buffer: &Buffer, first: usize, removed: usize, added: usize) -> usize {
        // Make the vector as long as the buffer again. When the edit changed no
        // line count -- typing inside a line, the common case -- this is a
        // no-op rather than a memmove.
        if removed != added {
            let at = (first + 1).min(self.ends.len());
            let old_end = (at + removed).min(self.ends.len());
            self.ends
                .splice(at..old_end, std::iter::repeat_n(LineState::START, added));
        }
        debug_assert_eq!(self.ends.len(), buffer.line_count());

        // Re-lex downwards. Every line inside the edited region has to be
        // redone whatever its end state comes out as; below the region, an
        // unchanged end state means every line under it is unchanged too.
        let lines = buffer.line_count();
        let last_dirty = first + added;
        let mut state = self.start_state(first);
        let mut relexed = 0;
        for line in first..lines {
            state = self.lex_end_state(buffer, line, state);
            relexed += 1;
            let settled = line > last_dirty && self.ends[line] == state;
            self.ends[line] = state;
            if settled {
                break;
            }
        }
        relexed
    }

    /// How many times a line has been lexed through this cache.
    ///
    /// For tests and for profiling; the number is meaningless on its own and
    /// only differences between two reads of it mean anything.
    pub fn lex_calls(&self) -> usize {
        self.lex_calls.get()
    }

    /// The state `line` ends in, given the state it starts in.
    fn lex_end_state(&self, buffer: &Buffer, line: usize, start: LineState) -> LineState {
        let text = buffer.line_text(line);
        self.lex_calls.set(self.lex_calls.get() + 1);
        lex_line(&text, start, &self.dialect).1
    }
}

#[cfg(test)]
mod tests {
    use rudbman_sql::TokenKind;

    use super::*;

    /// A cache over `text`, generic SQL.
    fn cache(text: &str) -> (Buffer, Highlighter) {
        let buffer = Buffer::new(text);
        let highlighter = Highlighter::new(&buffer, Dialect::GENERIC);
        (buffer, highlighter)
    }

    /// Replaces `range` in both the buffer and the cache, the way the editor
    /// does, and answers with the number of lines re-lexed.
    fn edit(
        buffer: &mut Buffer,
        highlighter: &mut Highlighter,
        range: std::ops::Range<usize>,
        text: &str,
    ) -> usize {
        let first = buffer.line_of(range.start);
        let removed = buffer.line_of(range.end) - first;
        let added = text.bytes().filter(|b| *b == b'\n').count();
        buffer.replace(range, text);
        highlighter.edited(buffer, first, removed, added)
    }

    #[test]
    fn an_ordinary_edit_relexes_one_line() {
        let mut text = String::new();
        for i in 0..2000 {
            text.push_str(&format!("select {i} from t;\n"));
        }
        let (mut buffer, mut highlighter) = cache(&text);

        // An edit on the third line settles on the fourth: the third's end
        // state is unchanged, and the loop stops the moment it sees that.
        let at = buffer.line_start(2) + 6;
        assert_eq!(edit(&mut buffer, &mut highlighter, at..at, "ion"), 2);
    }

    #[test]
    fn opening_a_block_comment_propagates_and_closing_it_stops() {
        // Two hundred statements with a stray `*/` on the eleventh line: the
        // buffer is long enough that "walked to the end" and "stopped where the
        // states settled" are different numbers.
        let mut text = String::new();
        for line in 0..200 {
            if line == 10 {
                text.push_str("*/\n");
            } else {
                text.push_str(&format!("select {line};\n"));
            }
        }
        let (mut buffer, mut highlighter) = cache(&text);
        assert!(highlighter.end_state(0).is_start());

        // Open a block comment on the first line: every line down to the `*/`
        // is now inside it, and the walk stops there rather than at line 200.
        let relexed = edit(&mut buffer, &mut highlighter, 0..0, "/*");
        assert_eq!(relexed, 11);
        assert!(!highlighter.end_state(0).is_start());
        assert!(!highlighter.end_state(9).is_start());
        assert!(highlighter.end_state(10).is_start());
        assert_eq!(
            highlighter.tokens(&buffer, 1)[0].kind,
            TokenKind::Comment,
            "a line inside the comment lexes as comment throughout"
        );

        // Close it again on the first line and the states walk back, no
        // further than they came.
        let relexed = edit(&mut buffer, &mut highlighter, 2..2, "*/");
        assert_eq!(relexed, 11);
        assert!(highlighter.end_state(0).is_start());
        assert!(highlighter.end_state(9).is_start());
        assert_eq!(highlighter.tokens(&buffer, 1)[0].kind, TokenKind::Keyword);
    }

    #[test]
    fn splitting_and_joining_lines_keeps_the_cache_the_right_length() {
        let (mut buffer, mut highlighter) = cache("select 1 from t;\n");
        edit(&mut buffer, &mut highlighter, 8..9, "\n");
        assert_eq!(buffer.line_count(), 3);
        assert_eq!(buffer.line_text(1), "from t;");

        edit(&mut buffer, &mut highlighter, 8..9, "");
        assert_eq!(buffer.line_count(), 2);
        assert_eq!(buffer.line_text(0), "select 1from t;");
    }

    #[test]
    fn an_incremental_cache_agrees_with_a_fresh_one() {
        let (mut buffer, mut highlighter) =
            cache("select 'a'\n, 'b' from t;\n-- tail\nselect 2;\n");

        // A quote opened mid-buffer, then closed again, then a whole line
        // pasted in: after each the cache has to match a rebuild.
        for (range, text) in [
            (7..7, "'"),
            (0..0, "/* x\n"),
            (0..5, ""),
            (10..10, "\ninsert into u values ('$$');"),
        ] {
            edit(&mut buffer, &mut highlighter, range, text);
            let fresh = Highlighter::new(&buffer, Dialect::GENERIC);
            let incremental: Vec<_> = (0..buffer.line_count())
                .map(|l| highlighter.end_state(l))
                .collect();
            let rebuilt: Vec<_> = (0..buffer.line_count())
                .map(|l| fresh.end_state(l))
                .collect();
            assert_eq!(incremental, rebuilt, "after {text:?}");
        }
    }

    #[test]
    fn drawing_costs_one_lex_per_visible_line() {
        let mut text = String::new();
        for i in 0..5000 {
            text.push_str(&format!("select {i};\n"));
        }
        let (buffer, highlighter) = cache(&text);

        let before = highlighter.lex_calls();
        for line in 100..140 {
            highlighter.tokens(&buffer, line);
        }
        assert_eq!(highlighter.lex_calls() - before, 40);
    }
}
