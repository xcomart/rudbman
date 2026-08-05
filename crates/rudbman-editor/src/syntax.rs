//! The two questions the editor asks about SQL that a single line cannot
//! answer: which statement the caret is in, and which bracket matches the one
//! next to it.
//!
//! Both have an obvious implementation that is wrong at this size.
//! [`rudbman_sql::statement_at`] takes a `&str` of the whole script, and
//! materialising a 100MB rope into one on every caret move is exactly the cost
//! the rope was chosen to avoid. So both functions here work over a **window**
//! of the buffer, cut at boundaries where the lexer is known to be in its start
//! state, and the window is grown only as far as the answer needs.
//!
//! # Statements
//!
//! A top-level semicolon resets the splitter completely — see the `Statements`
//! iterator in `rudbman-sql` — so a byte offset just past one is a position
//! from which lexing the rest of the script gives the same statements as
//! lexing all of it. That is what makes a window sound. [`statement_at`] walks
//! backwards from the caret's line until it has one whole statement behind the
//! caret (so that "the statement before the cursor wins in the gap" can be
//! answered), forwards until it has one whole statement ahead, and then calls
//! the real [`rudbman_sql::statement_at`] on that window and shifts the span
//! back into buffer coordinates. The answer is identical to the whole-buffer
//! one; the cost is the length of two statements.
//!
//! [`MAX_WINDOW`] caps it, for the pathological buffer with no semicolon in it
//! at all. A script whose single statement is larger than the cap gets a span
//! that starts at the cap rather than at the statement's true start.
//!
//! # Brackets
//!
//! The same shape, and the same reason the depth counter can be trusted: only
//! `Punctuation` tokens count, so a `(` inside a string literal or a comment is
//! not a bracket at all. The scan walks outwards line by line, lexing each line
//! from its cached state, and gives up after [`MAX_BRACKET_LINES`].

use std::ops::Range;

use rudbman_sql::{Dialect, StatementSpan, TokenKind};

use crate::buffer::Buffer;
use crate::highlight::Highlighter;

/// How far either way a statement window may grow, in bytes.
///
/// Two megabytes is far past any statement a person writes and far short of
/// the buffer sizes this editor is meant to survive.
const MAX_WINDOW: usize = 2 * 1024 * 1024;

/// How many lines the bracket scan will walk before giving up.
const MAX_BRACKET_LINES: usize = 5_000;

/// The statement the caret at `offset` is in, in buffer coordinates.
///
/// Agrees with [`rudbman_sql::statement_at`] over the whole buffer; see the
/// module documentation for the window that makes that affordable, and for the
/// one case where it does not.
pub fn statement_at(
    buffer: &Buffer,
    highlighter: &Highlighter,
    offset: usize,
) -> Option<StatementSpan> {
    let dialect = highlighter.dialect();
    let offset = offset.min(buffer.len());
    let start = window_start(buffer, highlighter, offset);
    let end = window_end(buffer, highlighter, offset);
    let text = buffer.slice(start..end);

    let span = rudbman_sql::statement_at(&text, offset - start, &dialect)?;
    Some(StatementSpan {
        start: span.start + start,
        end: span.end + start,
        sql_end: span.sql_end + start,
    })
}

/// The bracket next to the caret and the one it pairs with.
///
/// Looks at the character before the caret first and the one after it second,
/// which is what puts the highlight on the bracket a person has just typed.
/// Answers `None` when neither is a bracket, or when the partner is missing.
pub fn bracket_pair(
    buffer: &Buffer,
    highlighter: &Highlighter,
    caret: usize,
) -> Option<(usize, usize)> {
    let before = buffer.prev_grapheme(caret);
    for at in [before, caret] {
        if at >= buffer.len() {
            continue;
        }
        let Some(bracket) = bracket_at(buffer, highlighter, at) else {
            continue;
        };
        if let Some(partner) = match_bracket(buffer, highlighter, at, bracket) {
            return Some((at, partner));
        }
    }
    None
}

/// One half of a bracket pair, as the scanner sees it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Bracket {
    /// The character itself.
    byte: u8,
    /// The character that closes it, or that it closes.
    partner: u8,
    /// Whether this is the opening half.
    opening: bool,
}

/// The bracket at `offset`, if there is a real one there.
///
/// "Real" means the lexer called it punctuation: a `(` in a string or a comment
/// is not a bracket, and neither is the `[` that quotes an identifier in SQL
/// Server.
fn bracket_at(buffer: &Buffer, highlighter: &Highlighter, offset: usize) -> Option<Bracket> {
    let bracket = classify(buffer.rope().byte(offset))?;
    let (line, column) = buffer.point_of(offset);
    highlighter
        .tokens(buffer, line)
        .iter()
        .any(|token| token.kind == TokenKind::Punctuation && token.start == column)
        .then_some(bracket)
}

/// Whether a byte is a bracket, and which way round.
const fn classify(byte: u8) -> Option<Bracket> {
    let (partner, opening) = match byte {
        b'(' => (b')', true),
        b')' => (b'(', false),
        b'[' => (b']', true),
        b']' => (b'[', false),
        b'{' => (b'}', true),
        b'}' => (b'{', false),
        _ => return None,
    };
    Some(Bracket {
        byte,
        partner,
        opening,
    })
}

/// Scans for the partner of the bracket at `from`.
fn match_bracket(
    buffer: &Buffer,
    highlighter: &Highlighter,
    from: usize,
    bracket: Bracket,
) -> Option<usize> {
    let first_line = buffer.line_of(from);
    let mut depth = 0i32;
    let lines: Box<dyn Iterator<Item = usize>> = if bracket.opening {
        Box::new(first_line..buffer.line_count().min(first_line + MAX_BRACKET_LINES))
    } else {
        Box::new((first_line.saturating_sub(MAX_BRACKET_LINES)..=first_line).rev())
    };

    for line in lines {
        let start = buffer.line_start(line);
        let text = buffer.line_text(line);
        let bytes = text.as_bytes();
        let tokens = highlighter.tokens(buffer, line);

        // Only punctuation counts, so the scan walks the tokens rather than the
        // bytes. One-byte punctuation tokens are the only ones a bracket can
        // be.
        let mut candidates: Vec<usize> = tokens
            .iter()
            .filter(|token| token.kind == TokenKind::Punctuation && token.len() == 1)
            .map(|token| token.start)
            .filter(|column| {
                bytes
                    .get(*column)
                    .is_some_and(|byte| *byte == bracket.byte || *byte == bracket.partner)
            })
            .collect();
        if !bracket.opening {
            candidates.reverse();
        }

        for column in candidates {
            let at = start + column;
            if bracket.opening && at < from {
                continue;
            }
            if !bracket.opening && at > from {
                continue;
            }
            if bytes[column] == bracket.byte {
                depth += 1;
            } else {
                depth -= 1;
                if depth == 0 {
                    return Some(at);
                }
            }
        }
    }
    None
}

/// Where a statement window may begin: past a top-level semicolon far enough
/// back that one whole statement sits between it and the caret.
fn window_start(buffer: &Buffer, highlighter: &Highlighter, offset: usize) -> usize {
    let floor = offset.saturating_sub(MAX_WINDOW);
    let mut candidate = None;
    let mut code_since_semicolon = false;

    let first_line = buffer.line_of(offset);
    for line in (0..=first_line).rev() {
        let start = buffer.line_start(line);
        if start + buffer.line_text(line).len() < floor {
            break;
        }
        let text = buffer.line_text(line);
        let tokens = highlighter.tokens(buffer, line);
        // Backwards, because the fragment boundaries are found in that order.
        for token in tokens.iter().rev() {
            let at = start + token.start;
            if at >= offset {
                continue;
            }
            if is_semicolon(&text, token) {
                if code_since_semicolon {
                    // One whole statement now lies between here and the caret,
                    // which is all `statement_at` can need behind it.
                    return at + token.len();
                }
                candidate = Some(at + token.len());
                continue;
            }
            if !token.kind.is_trivia() {
                code_since_semicolon = true;
            }
        }
    }
    // Nothing but one fragment behind the caret: start from the top, or from
    // the nearest boundary inside the cap.
    if floor == 0 {
        0
    } else {
        candidate.unwrap_or(floor)
    }
}

/// Where a statement window may end: past the first semicolon that terminates
/// a statement holding actual SQL, or the end of the buffer.
///
/// A run of `;;` terminates nothing, so it does not end the window: rule three
/// of [`rudbman_sql::statement_at`] — before the first statement, the first
/// statement wins — needs a real statement ahead of the caret to answer with.
fn window_end(buffer: &Buffer, highlighter: &Highlighter, offset: usize) -> usize {
    let ceiling = (offset + MAX_WINDOW).min(buffer.len());
    let mut code_seen = false;
    let first_line = buffer.line_of(offset);
    for line in first_line..buffer.line_count() {
        let start = buffer.line_start(line);
        if start > ceiling {
            break;
        }
        let text = buffer.line_text(line);
        for token in &highlighter.tokens(buffer, line) {
            let at = start + token.start;
            if at + token.len() <= offset {
                continue;
            }
            if is_semicolon(&text, token) {
                if code_seen {
                    return at + token.len();
                }
                continue;
            }
            if !token.kind.is_trivia() {
                code_seen = true;
            }
        }
    }
    buffer.len().min(ceiling.max(offset))
}

/// Whether a token is a bare `;`.
fn is_semicolon(line: &str, token: &rudbman_sql::Token) -> bool {
    token.kind == TokenKind::Punctuation && token.len() == 1 && line.as_bytes()[token.start] == b';'
}

/// The line comment prefix of a dialect, for the comment toggle.
///
/// `--` everywhere: it is the only line comment every SQL dialect in
/// `rudbman-sql` agrees on, and MySQL's `#` is an alternative rather than a
/// replacement.
pub const fn line_comment(_dialect: &Dialect) -> &'static str {
    "--"
}

/// The byte range of the lines `range` touches, terminators of the last one
/// excluded.
pub fn line_span(buffer: &Buffer, range: &Range<usize>) -> (usize, usize) {
    let first = buffer.line_of(range.start);
    // A selection that ends exactly at the head of a line has not touched it.
    let last_offset =
        if range.end > range.start && buffer.line_start(buffer.line_of(range.end)) == range.end {
            range.end - 1
        } else {
            range.end
        };
    (first, buffer.line_of(last_offset))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A buffer and its cache over `text`, generic SQL.
    fn open(text: &str) -> (Buffer, Highlighter) {
        let buffer = Buffer::new(text);
        let highlighter = Highlighter::new(&buffer, Dialect::GENERIC);
        (buffer, highlighter)
    }

    /// The scripts the agreement test runs over.
    const SCRIPTS: &[&str] = &[
        "select 1;\n\nselect 2;\n",
        "-- a comment\nselect 1;\nselect 2",
        "insert into t values (';'); -- and a ; here\nselect 1",
        ";;;\nselect 1;\n;;\nselect 2;\n",
        "select 1\n",
        "",
        "   \n\n  ",
        "/* block\n with a ; in it */\nselect 1;\nselect 2;",
        "select '\nmultiline; string\n';\nselect 2;",
    ];

    #[test]
    fn the_windowed_answer_is_the_whole_buffer_answer() {
        for script in SCRIPTS {
            let (buffer, highlighter) = open(script);
            for offset in 0..=script.len() {
                if !script.is_char_boundary(offset) {
                    continue;
                }
                let windowed = statement_at(&buffer, &highlighter, offset);
                let whole = rudbman_sql::statement_at(script, offset, &Dialect::GENERIC);
                assert_eq!(windowed, whole, "at {offset} of {script:?}");
            }
        }
    }

    #[test]
    fn the_statement_changes_at_a_semicolon() {
        let script = "select 1;\n\nselect 2;\n";
        let (buffer, highlighter) = open(script);
        let sql = |offset| {
            statement_at(&buffer, &highlighter, offset)
                .map(|span| span.sql(script).to_owned())
                .expect("a statement")
        };
        assert_eq!(sql(3), "select 1");
        assert_eq!(sql(9), "select 1", "just past the semicolon");
        assert_eq!(sql(10), "select 1", "the blank line between them");
        assert_eq!(sql(12), "select 2");
    }

    #[test]
    fn the_window_holds_over_a_long_script() {
        // Ten thousand statements: the answer in the middle has to be the same
        // as the whole-buffer one, and getting it must not read all of it.
        let mut script = String::new();
        for i in 0..10_000 {
            script.push_str(&format!("select {i} from t;\n"));
        }
        let (buffer, highlighter) = open(&script);

        let offset = buffer.line_start(5_000) + 3;
        let before = highlighter.lex_calls();
        let span = statement_at(&buffer, &highlighter, offset).expect("a statement");
        let lexed = highlighter.lex_calls() - before;

        assert_eq!(span.sql(&script), "select 5000 from t");
        assert!(lexed < 20, "read {lexed} lines of ten thousand");
    }

    #[test]
    fn brackets_pair_across_lines() {
        let script = "select coalesce(\n  a,\n  (b + c)\n)\nfrom t;\n";
        let (buffer, highlighter) = open(script);

        let open_paren = script.find('(').expect("an opener");
        let close_paren = script.rfind(')').expect("a closer");
        assert_eq!(
            bracket_pair(&buffer, &highlighter, open_paren + 1),
            Some((open_paren, close_paren))
        );
        assert_eq!(
            bracket_pair(&buffer, &highlighter, close_paren + 1),
            Some((close_paren, open_paren))
        );
    }

    #[test]
    fn a_bracket_in_a_string_or_a_comment_is_not_a_bracket() {
        let script = "select '(' , 1); -- )\n";
        let (buffer, highlighter) = open(script);

        // The `(` inside the quotes must not be found as the partner of the
        // real `)`.
        let close_paren = script.find(')').expect("a closer");
        assert_eq!(bracket_pair(&buffer, &highlighter, close_paren + 1), None);

        // And the caret next to the quoted one finds nothing at all.
        let quoted = script.find('(').expect("an opener");
        assert_eq!(bracket_pair(&buffer, &highlighter, quoted + 1), None);
    }

    #[test]
    fn an_unmatched_bracket_pairs_with_nothing() {
        let (buffer, highlighter) = open("select (1;\n");
        assert_eq!(bracket_pair(&buffer, &highlighter, 8), None);
    }

    #[test]
    fn a_line_span_stops_at_the_head_of_a_line() {
        let (buffer, _) = open("a\nb\nc\n");
        assert_eq!(line_span(&buffer, &(0..1)), (0, 0));
        // A selection that stops at the head of the next line has not reached
        // it, which is what keeps `shift-down` from indenting one line too
        // many.
        assert_eq!(line_span(&buffer, &(0..2)), (0, 0));
        assert_eq!(line_span(&buffer, &(0..4)), (0, 1));
        assert_eq!(line_span(&buffer, &(2..2)), (1, 1));
    }
}
