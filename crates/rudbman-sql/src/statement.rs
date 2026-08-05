//! Cutting a script into statements.
//!
//! This is what "execute the statement under the cursor" is built on, and what
//! a script runner will iterate. It is a semicolon splitter — but one that runs
//! on top of the lexer, so the semicolons inside strings, comments, quoted
//! identifiers and dollar-quoted bodies are already invisible to it. That is
//! most of the value, and it comes for free:
//!
//! ```
//! use rudbman_sql::{Dialect, split_statements};
//!
//! let script = "insert into t values (';'); -- and a ; here\nselect 1";
//! let spans = split_statements(script, &Dialect::GENERIC);
//! assert_eq!(spans.len(), 2);
//! assert_eq!(spans[0].sql(script), "insert into t values (';')");
//! // The comment after the semicolon belongs to the statement it introduces.
//! assert_eq!(spans[1].sql(script), "-- and a ; here\nselect 1");
//! ```
//!
//! # What counts as a statement
//!
//! A span runs from the first non-whitespace byte after the previous semicolon
//! to the semicolon that ends it, that semicolon included. Leading comments are
//! part of the statement that follows them, which is what makes
//! [`statement_at`] do the obvious thing when the cursor is parked in the
//! comment above a query.
//!
//! Fragments holding no SQL are dropped rather than returned empty: a run of
//! `;;;`, the trailing newline at the end of a file, a comment with no statement
//! after it. A statement without a closing semicolon — the usual last line of a
//! script, and the usual case in a scratch buffer — is a statement.
//!
//! # Known limitations
//!
//! Two constructs are **not supported**, deliberately and rather than badly:
//!
//! * **Oracle PL/SQL blocks.** In `BEGIN ... END;` the inner statements end in
//!   semicolons too, so this splitter sees several statements where there is
//!   one, and the `/` on its own line that actually terminates the block is not
//!   a terminator here at all. The same goes for `CREATE PROCEDURE`,
//!   `DECLARE ... BEGIN`, and package bodies.
//! * **MySQL's `DELIMITER`.** The client-side directive that changes the
//!   terminator (`DELIMITER //`) is not a MySQL statement — it is a feature of
//!   the `mysql` command-line tool — and it is not honoured here.
//!
//! Both would need block-structure tracking, which is a parser, and a
//! half-working version of either is worse than none: a script runner that
//! believes it has found a statement boundary in the middle of a trigger body
//! will send a fragment to the server. Where these need to work is script
//! execution (M4), and the decision belongs there, with a whole feature to
//! design against.
//!
//! SQL Server's `GO` is not a terminator either, but for a different reason: it
//! is a batch separator understood by `sqlcmd` and not by the server, and
//! sending it over JDBC is an error however it is split.

use crate::dialect::Dialect;
use crate::lexer::{Lexer, TokenKind};

/// One statement's place in the script, in bytes.
///
/// Two ranges rather than one, because the caller wants different things at
/// different moments: [`Self::range`] covers the statement as written, the
/// terminating semicolon included, and is what to highlight or select;
/// [`Self::sql_range`] stops before that semicolon, and is what to hand a JDBC
/// `Statement` — several drivers, Oracle's above all, reject a trailing `;`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct StatementSpan {
    /// First byte of the statement: its first non-whitespace byte, which may be
    /// the start of a comment that precedes the SQL.
    pub start: usize,
    /// One past the terminating semicolon, or one past the last non-whitespace
    /// byte if the statement is the last in the script and has no semicolon.
    pub end: usize,
    /// One past the last non-whitespace byte before the terminating semicolon.
    ///
    /// Equal to [`Self::end`] when there is no semicolon.
    pub sql_end: usize,
}

impl StatementSpan {
    /// The statement as written, the terminating semicolon included.
    pub const fn range(&self) -> std::ops::Range<usize> {
        self.start..self.end
    }

    /// The statement without its terminating semicolon.
    pub const fn sql_range(&self) -> std::ops::Range<usize> {
        self.start..self.sql_end
    }

    /// The text of [`Self::range`].
    ///
    /// # Panics
    ///
    /// If `source` is not the script this span was cut from.
    pub fn text<'a>(&self, source: &'a str) -> &'a str {
        &source[self.range()]
    }

    /// The text of [`Self::sql_range`] — what to execute.
    ///
    /// # Panics
    ///
    /// If `source` is not the script this span was cut from.
    pub fn sql<'a>(&self, source: &'a str) -> &'a str {
        &source[self.sql_range()]
    }

    /// Whether `offset` falls inside this statement, its terminating semicolon
    /// and the position just after it included.
    pub const fn contains(&self, offset: usize) -> bool {
        self.start <= offset && offset <= self.end
    }
}

/// The statements of a script, one at a time.
///
/// Private: the two public functions are the whole intended surface, and an
/// iterator that borrows a [`Lexer`] would pin the API to it.
struct Statements<'a> {
    /// The script, for looking at the one byte of a punctuation token.
    src: &'a str,
    /// The token stream, whose position carries from call to call.
    lexer: Lexer<'a>,
}

impl<'a> Statements<'a> {
    /// Start splitting `source`.
    fn new(source: &'a str, dialect: &Dialect) -> Self {
        Self {
            src: source,
            lexer: Lexer::new(source, dialect),
        }
    }
}

impl Iterator for Statements<'_> {
    type Item = StatementSpan;

    fn next(&mut self) -> Option<StatementSpan> {
        // `start` is the first non-whitespace byte seen, `last_end` the end of
        // the last one, and `has_code` whether any of them was more than a
        // comment. A fragment that never sets `has_code` is thrown away rather
        // than returned.
        let mut start = None;
        let mut last_end = 0;
        let mut has_code = false;

        for token in self.lexer.by_ref() {
            if token.kind == TokenKind::Whitespace {
                continue;
            }
            if token.kind == TokenKind::Punctuation
                && self.src.as_bytes()[token.start] == b';'
                && token.len() == 1
            {
                if has_code {
                    return Some(StatementSpan {
                        start: start.unwrap_or(token.start),
                        end: token.end,
                        sql_end: last_end,
                    });
                }
                // `;;` or a comment with no statement under it: forget what came
                // before and start the next fragment from scratch.
                start = None;
                last_end = 0;
                continue;
            }
            if start.is_none() {
                start = Some(token.start);
            }
            last_end = token.end;
            if token.kind != TokenKind::Comment {
                has_code = true;
            }
        }

        // End of the script with no semicolon: still a statement, if it had any
        // SQL in it.
        has_code.then(|| StatementSpan {
            start: start.unwrap_or(last_end),
            end: last_end,
            sql_end: last_end,
        })
    }
}

/// Cut `source` into statements.
///
/// The spans are in order and do not overlap. The gaps between them are
/// whitespace and discarded fragments; the spans do not tile the source, unlike
/// the lexer's tokens.
///
/// Cost is one pass of the lexer over the whole script. A caller that splits the
/// same buffer repeatedly — an editor, on every execute — should keep the result
/// and re-split when the buffer changes, rather than call [`statement_at`] in a
/// loop.
pub fn split_statements(source: &str, dialect: &Dialect) -> Vec<StatementSpan> {
    Statements::new(source, dialect).collect()
}

/// The statement the cursor at `offset` is in, for "execute the statement under
/// the cursor".
///
/// `offset` is a byte offset into `source`. The rules, in order:
///
/// 1. A statement containing `offset` wins, where "containing" includes the
///    position just after its semicolon — a cursor at `select 1;|` is in that
///    statement, not between two.
/// 2. In the whitespace between two statements, the one *before* the cursor
///    wins. This is what a person means after typing a query and pressing
///    return: the statement they just finished, not the empty line they are on.
/// 3. Before the first statement, the first statement wins.
///
/// So the answer is `None` only when the script holds no statement at all, and
/// an offset past the end of `source` gets the last one.
///
/// ```
/// use rudbman_sql::{Dialect, statement_at};
///
/// let script = "select 1;\n\nselect 2;\n";
/// let at = |o| statement_at(script, o, &Dialect::GENERIC).unwrap().sql(script);
///
/// assert_eq!(at(3), "select 1");   // inside the first
/// assert_eq!(at(9), "select 1");   // just after its semicolon
/// assert_eq!(at(10), "select 1");  // on the blank line after it
/// assert_eq!(at(12), "select 2");  // inside the second
/// assert_eq!(at(script.len()), "select 2");
/// ```
pub fn statement_at(source: &str, offset: usize, dialect: &Dialect) -> Option<StatementSpan> {
    let mut previous = None;
    for span in Statements::new(source, dialect) {
        if offset < span.start {
            // The cursor is in the gap ahead of this statement, so it belongs to
            // whatever came before — or to this one, if nothing did.
            return previous.or(Some(span));
        }
        if span.contains(offset) {
            return Some(span);
        }
        previous = Some(span);
    }
    previous
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The `sql` text of every statement in `source`.
    fn sqls<'a>(source: &'a str, dialect: &Dialect) -> Vec<&'a str> {
        split_statements(source, dialect)
            .into_iter()
            .map(|s| s.sql(source))
            .collect()
    }

    #[test]
    fn splits_on_semicolons_and_keeps_the_last_unterminated_one() {
        assert_eq!(
            sqls("select 1; select 2;\nselect 3", &Dialect::GENERIC),
            ["select 1", "select 2", "select 3"]
        );
    }

    #[test]
    fn empty_fragments_are_dropped() {
        assert_eq!(sqls(";;; select 1 ;;\n;", &Dialect::GENERIC), ["select 1"]);
        assert!(sqls("", &Dialect::GENERIC).is_empty());
        assert!(sqls("   \n\t ", &Dialect::GENERIC).is_empty());
        assert!(sqls("-- nothing here\n/* nor here */", &Dialect::GENERIC).is_empty());
    }

    #[test]
    fn a_leading_comment_belongs_to_the_statement_under_it() {
        let script = "-- how many\nselect count(*) from t;";
        let span = split_statements(script, &Dialect::GENERIC)[0];
        assert!(span.text(script).starts_with("-- how many"));
        assert_eq!(span.sql(script), script.trim_end_matches(';'));
    }

    #[test]
    fn the_ranges_differ_only_in_the_semicolon() {
        let script = "select 1 ;";
        let span = split_statements(script, &Dialect::GENERIC)[0];
        assert_eq!(span.text(script), "select 1 ;");
        assert_eq!(span.sql(script), "select 1");
    }
}
