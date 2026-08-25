//! The SQL dialect, handed to an editor that has never heard of one.
//!
//! `ruui-editor` holds a [`Highlighter`] — "give me the coloured runs of this
//! line, given the state the line before it ended in" — and nothing else. It
//! knows no SQL, no dialects, and no `rudbman-sql`. [`DialectHighlighter`] is
//! the whole of what rudbman puts on the other side of that trait: it lexes a
//! line with [`rudbman_sql::lex_line`] in the dialect of the session's driver,
//! maps each [`TokenKind`] onto one of the editor palette's twelve slots, and
//! carries the lexer's [`LineState`](rudbman_sql::LineState) across the line
//! break as the `u32` the widget stores per line.
//!
//! # Why the widget stopped knowing
//!
//! The editor used to be `rudbman-editor` and used to call `lex_line` itself.
//! Extracting it into a kit three applications share meant it could no longer
//! name a `Dialect` — the log viewer's editor lexes log patterns and the
//! template editor's lexes Java — so the lexer became a plug and this module
//! is rudbman's. Nothing was lost in the move except the direction of a call.
//!
//! # What did move
//!
//! Two things the old widget did with a dialect in hand:
//!
//! * **The comment toggle** asked the dialect for its line comment. Every
//!   dialect in `rudbman-sql` answered `--`, MySQL's `#` being an alternative
//!   rather than a replacement, so [`Highlighter::line_comment`] answers `--`
//!   flat.
//! * **The statement under the caret** was cut by [`rudbman_sql::statement_at`]
//!   over a window of the buffer. The widget now cuts it itself, from the
//!   colours it is drawing anyway: a `;` terminates a statement unless the
//!   highlighter called it part of a string or a comment. `statement_boundaries`
//!   in this module's tests is what holds the two answers together across the
//!   dialects where they could drift — MySQL's `#`, PostgreSQL's `$$`, SQL
//!   Server's `[..]`.

use std::sync::Arc;

use rudbman_sql::{Dialect, LineStateCodec, TokenKind, lex_line};
use ruui_editor::{Highlighter, LineState, Span, Token};

/// A [`Highlighter`] that lexes one SQL dialect.
///
/// One per editor, held by the editor behind an `Arc`. The dialect is fixed at
/// construction: a session's driver decides it, and a query pane outlives
/// neither.
#[derive(Debug)]
pub struct DialectHighlighter {
    /// The rules in force.
    dialect: Dialect,
    /// Packs the lexer's sixteen-byte line state into the four bytes the
    /// editor's syntax cache keeps per line. See [`LineStateCodec`]; a dollar
    /// quote is the reason it is a codec with a table rather than a pair of
    /// shifts.
    codec: LineStateCodec,
}

impl DialectHighlighter {
    /// A highlighter for `dialect`.
    pub fn new(dialect: Dialect) -> Self {
        Self {
            dialect,
            codec: LineStateCodec::new(),
        }
    }

    /// A highlighter for `dialect`, ready to hand to an editor.
    pub fn shared(dialect: Dialect) -> Arc<dyn Highlighter> {
        Arc::new(Self::new(dialect))
    }
}

impl Highlighter for DialectHighlighter {
    fn line(&self, text: &str, state: LineState) -> (Vec<Span>, LineState) {
        let (tokens, end) = lex_line(text, self.codec.decode(state.0), &self.dialect);
        let spans = tokens
            .into_iter()
            .filter_map(|token| Some(Span::new(token.range(), paint(token.kind)?)))
            .collect();
        (spans, LineState(self.codec.encode(end)))
    }

    fn line_comment(&self) -> Option<&'static str> {
        Some("--")
    }

    fn statements(&self) -> bool {
        true
    }
}

/// Which palette slot a lexed token is painted in, if any.
///
/// The mapping the editor's element used to hold, unchanged: a parameter is
/// coloured like a number and a quoted identifier like a plain one, because the
/// palette has no slot of its own for either. Whitespace gets no span at all —
/// it used to be painted in the palette's foreground colour, which is what the
/// editor draws the bytes no span covers in.
const fn paint(kind: TokenKind) -> Option<Token> {
    Some(match kind {
        TokenKind::Keyword => Token::Keyword,
        TokenKind::Type => Token::Type,
        TokenKind::Function => Token::Function,
        TokenKind::String => Token::String,
        TokenKind::Number | TokenKind::Parameter => Token::Number,
        TokenKind::Comment => Token::Comment,
        TokenKind::Operator => Token::Operator,
        TokenKind::Punctuation => Token::Punctuation,
        TokenKind::Identifier | TokenKind::QuotedIdentifier => Token::Identifier,
        TokenKind::Error => Token::Error,
        TokenKind::Whitespace => return None,
    })
}

#[cfg(test)]
mod tests {
    use ruui_editor::{Buffer, SyntaxCache, syntax};

    use super::*;

    /// A buffer and its syntax cache over `script`, lexed as `dialect`.
    fn open(script: &str, dialect: Dialect) -> (Buffer, SyntaxCache) {
        let buffer = Buffer::new(script);
        let cache = SyntaxCache::new(&buffer, Some(DialectHighlighter::shared(dialect)));
        (buffer, cache)
    }

    /// The tokens of line `line`, as `(text, token)` pairs.
    fn painted<'a>(
        script: &'a str,
        buffer: &Buffer,
        cache: &SyntaxCache,
        line: usize,
    ) -> Vec<(&'a str, Token)> {
        let base = buffer.line_start(line);
        cache
            .spans(buffer, line)
            .into_iter()
            .map(|span| {
                (
                    &script[base + span.range.start..base + span.range.end],
                    span.token,
                )
            })
            .collect()
    }

    #[test]
    fn the_palette_slots_are_the_ones_the_old_element_painted() {
        let script = "select count(x) as n, 'text', 42, :bind from t -- note\n";
        let (buffer, cache) = open(script, Dialect::GENERIC);
        let spans = painted(script, &buffer, &cache, 0);

        // Whitespace gets no span, so the runs are the non-blank ones only.
        assert_eq!(spans[0], ("select", Token::Keyword));
        assert_eq!(spans[1], ("count", Token::Function));
        assert_eq!(spans[2], ("(", Token::Punctuation));
        assert_eq!(spans[3], ("x", Token::Identifier));
        assert!(spans.contains(&("'text'", Token::String)));
        assert!(spans.contains(&("42", Token::Number)));
        assert!(
            spans.contains(&(":bind", Token::Number)),
            "a bind parameter is painted like a number, as it always was"
        );
        assert!(spans.contains(&("-- note", Token::Comment)));
        assert!(
            !spans.iter().any(|(text, _)| text.trim().is_empty()),
            "whitespace is a gap, not a span"
        );
    }

    #[test]
    fn a_quoted_identifier_is_painted_like_a_plain_one() {
        // Each dialect gets the quoting form it actually has: the palette has
        // no slot for a quoted identifier, so all three land on `Identifier`
        // beside the bare names around them, exactly as they used to.
        for (script, quoted, dialect) in [
            ("select \"a\" from t;\n", "\"a\"", Dialect::POSTGRES),
            ("select `a` from t;\n", "`a`", Dialect::MYSQL),
            ("select [a] from t;\n", "[a]", Dialect::MSSQL),
        ] {
            let (buffer, cache) = open(script, dialect);
            let spans = painted(script, &buffer, &cache, 0);
            assert!(
                spans.contains(&(quoted, Token::Identifier)),
                "{quoted} in {:?} painted {spans:?}",
                dialect.id()
            );
        }

        // MySQL is the one dialect where `"..."` is a string rather than a
        // name, and the colour follows the dialect rather than the character.
        let script = "select \"a\" from t;\n";
        let (buffer, cache) = open(script, Dialect::MYSQL);
        assert!(painted(script, &buffer, &cache, 0).contains(&("\"a\"", Token::String)));
    }

    #[test]
    fn the_dialect_decides_what_a_hash_and_a_backtick_are() {
        // MySQL: `#` opens a comment and a backtick quotes an identifier.
        let script = "select `x` # not sql\n";
        let (buffer, cache) = open(script, Dialect::MYSQL);
        let spans = painted(script, &buffer, &cache, 0);
        assert!(spans.contains(&("`x`", Token::Identifier)));
        assert!(spans.contains(&("# not sql", Token::Comment)));

        // Generic SQL: neither. The backtick is an error and the `#` is not a
        // comment, so the text after it is lexed as SQL.
        let (buffer, cache) = open(script, Dialect::GENERIC);
        let spans = painted(script, &buffer, &cache, 0);
        assert!(spans.contains(&("`", Token::Error)));
        assert!(!spans.iter().any(|(_, token)| *token == Token::Comment));
    }

    #[test]
    fn the_comment_toggle_is_the_one_every_dialect_agrees_on() {
        let highlighter = DialectHighlighter::new(Dialect::MYSQL);
        assert_eq!(
            highlighter.line_comment(),
            Some("--"),
            "MySQL's `#` is an alternative to `--`, not a replacement"
        );
        assert!(highlighter.statements());
    }

    /// The scripts the agreement test runs over, one dialect each.
    ///
    /// Every one of them puts a `;` somewhere the lexer has to be consulted
    /// about: inside a comment, inside a string, inside a dollar-quoted body,
    /// inside a block comment that runs over lines.
    const SCRIPTS: &[(&str, Dialect)] = &[
        ("select 1;\n\nselect 2;\n", Dialect::GENERIC),
        ("-- a comment\nselect 1;\nselect 2", Dialect::GENERIC),
        (
            "insert into t values (';'); -- and a ; here\nselect 1",
            Dialect::GENERIC,
        ),
        (";;;\nselect 1;\n;;\nselect 2;\n", Dialect::GENERIC),
        ("select 1\n", Dialect::GENERIC),
        ("", Dialect::GENERIC),
        ("   \n\n  ", Dialect::GENERIC),
        (
            "/* block\n with a ; in it */\nselect 1;\nselect 2;",
            Dialect::GENERIC,
        ),
        (
            "select '\nmultiline; string\n';\nselect 2;",
            Dialect::GENERIC,
        ),
        // H2: `--` and `/* */`, and a quoted identifier in double quotes.
        ("select \"a b\" from t;\n-- ;\nselect 2;\n", Dialect::H2),
        // MySQL: `#` runs to the end of the line, and `\` escapes inside a
        // string, so the `;` after the escaped quote is still inside it.
        (
            "select 1; # a ; here\nselect 'a\\'; b';\nselect 3;\n",
            Dialect::MYSQL,
        ),
        // PostgreSQL: a dollar-quoted body holds semicolons and a nested block
        // comment holds another.
        (
            "create function f() returns int as $body$\nbegin\n  select 1;\n  select 2;\nend\n$body$ language plpgsql;\nselect 3;\n",
            Dialect::POSTGRES,
        ),
        (
            "/* outer /* inner ; */ still open ; */ select 1;\nselect 2;\n",
            Dialect::POSTGRES,
        ),
        // SQL Server: `[..]` quotes an identifier, and `]]` escapes inside it.
        ("select [a] from [dbo].[t];\nselect 2;\n", Dialect::MSSQL),
    ];

    #[test]
    fn statement_boundaries_agree_with_the_splitter() {
        // The editor no longer calls `rudbman_sql::statement_at`; it cuts
        // statements out of the highlighter's spans. The two have to answer
        // alike, at every offset of every script above, or a `Run statement`
        // would send different SQL than `Run all`.
        for (script, dialect) in SCRIPTS {
            let (buffer, cache) = open(script, *dialect);
            for offset in 0..=script.len() {
                if !script.is_char_boundary(offset) {
                    continue;
                }
                let windowed = syntax::statement_at(&buffer, &cache, offset);
                let whole = rudbman_sql::statement_at(script, offset, dialect);
                let (windowed, whole) = (
                    windowed.map(|span| (span.start, span.end, span.sql_end)),
                    whole.map(|span| (span.start, span.end, span.sql_end)),
                );
                assert_eq!(windowed, whole, "at {offset} of {script:?} ({dialect:?})");
            }
        }
    }

    #[test]
    fn split_statements_and_the_editor_cut_the_same_sql() {
        // `Run all` splits the whole text with `rudbman-sql`; `Run statement`
        // asks the editor. Every statement the first cuts has to be one the
        // second answers with, from an offset inside it.
        for (script, dialect) in SCRIPTS {
            let (buffer, cache) = open(script, *dialect);
            for span in rudbman_sql::split_statements(script, dialect) {
                let inside = (span.start + span.sql_end) / 2;
                let here = syntax::statement_at(&buffer, &cache, inside)
                    .unwrap_or_else(|| panic!("no statement at {inside} of {script:?}"));
                assert_eq!(
                    here.sql(script),
                    span.sql(script),
                    "at {inside} of {script:?} ({dialect:?})"
                );
            }
        }
    }

    #[test]
    fn a_semicolon_inside_a_quoted_identifier_is_the_one_disagreement() {
        // KNOWN DIVERGENCE, and the only one the corpus above found.
        //
        // `ruui-editor` steps over a `;` the highlighter called part of a
        // string or a comment, and the palette has no third slot for a quoted
        // identifier — this module paints one `Identifier`, as the old editor's
        // element did — so a `;` inside `"a;b"`, `` `a;b` `` or `[a;b]` splits a
        // statement the SQL splitter keeps whole. It is a `Run statement` that
        // sends half a query; `Run all`, which splits with `rudbman-sql`, is
        // unaffected.
        //
        // The fix belongs in `ruui-editor`: either a `Token::Identifier` span
        // has to be opaque to the splitter the way a string is, or the palette
        // needs the quoted-identifier slot the lexer already distinguishes.
        // This test pins the wrong answer so that fixing it there is noticed
        // here — when it fails, delete it and put the script back in `SCRIPTS`.
        for (script, dialect) in [
            ("select \"a;b\" from t;\n", Dialect::H2),
            ("select `a;b` from t;\n", Dialect::MYSQL),
            ("select [a;b] from t;\n", Dialect::MSSQL),
        ] {
            let (buffer, cache) = open(script, dialect);
            let here = syntax::statement_at(&buffer, &cache, 0).expect("a statement");
            let whole = rudbman_sql::statement_at(script, 0, &dialect).expect("a statement");
            assert_eq!(whole.sql(script), script.trim_end().trim_end_matches(';'));
            assert_ne!(
                here.sql(script),
                whole.sql(script),
                "{script:?} ({:?}) no longer disagrees — see the comment above",
                dialect.id()
            );
        }
    }

    #[test]
    fn a_block_comment_carries_its_nesting_across_lines() {
        // PostgreSQL nests block comments; the depth travels in the line state,
        // which is the part of it that does not fit in a `u32` on its own.
        let script = "/* one /* two */\nstill commented\n*/ select 1;\n";
        let (buffer, cache) = open(script, Dialect::POSTGRES);
        assert!(!cache.end_state(0).is_start());
        assert!(!cache.end_state(1).is_start());
        assert!(cache.end_state(2).is_start());
        assert_eq!(
            painted(script, &buffer, &cache, 1),
            [("still commented", Token::Comment)]
        );
    }

    #[test]
    fn a_dollar_quote_carries_its_tag_across_lines() {
        // The tag is 96 bits and the state is 32, so this is the case the
        // codec's side table exists for: an inner `$other$` must not close a
        // body opened with `$body$`.
        let script = "select $body$ a\n$other$ b\n$body$, 1;\n";
        let (buffer, cache) = open(script, Dialect::POSTGRES);
        assert!(!cache.end_state(0).is_start());
        assert!(
            !cache.end_state(1).is_start(),
            "`$other$` does not close `$body$`"
        );
        assert!(cache.end_state(2).is_start());
        assert_eq!(
            painted(script, &buffer, &cache, 1),
            [("$other$ b", Token::String)]
        );
    }

    #[test]
    fn the_incremental_cache_agrees_with_a_fresh_one() {
        // The editor re-lexes from an edited line downwards and stops at the
        // first line whose end state is unchanged, comparing the *codes*. Two
        // different lexer states that encoded alike would stop it too early.
        let script = "select $a$ x\n$a$;\nselect 'y'\n, 'z';\n/* c\n*/\n";
        let mut buffer = Buffer::new(script);
        let mut cache =
            SyntaxCache::new(&buffer, Some(DialectHighlighter::shared(Dialect::POSTGRES)));

        for (range, text) in [(7..7, "b"), (0..0, "/* "), (0..3, ""), (11..11, "'")] {
            let first = buffer.line_of(range.start);
            let removed = buffer.line_of(range.end) - first;
            let added = text.bytes().filter(|byte| *byte == b'\n').count();
            buffer.replace(range, text);
            cache.edited(&buffer, first, removed, added);

            let fresh =
                SyntaxCache::new(&buffer, Some(DialectHighlighter::shared(Dialect::POSTGRES)));
            let incremental: Vec<_> = (0..buffer.line_count())
                .map(|line| cache.spans(&buffer, line))
                .collect();
            let rebuilt: Vec<_> = (0..buffer.line_count())
                .map(|line| fresh.spans(&buffer, line))
                .collect();
            assert_eq!(incremental, rebuilt, "after {text:?}");
        }
    }
}
