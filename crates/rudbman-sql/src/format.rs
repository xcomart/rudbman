//! Laying a script out again: the editor's "Format SQL".
//!
//! ```
//! use rudbman_sql::{Dialect, format_sql};
//!
//! let sql = "select a, b from t where x = 1 and y = 'a  b'";
//! assert_eq!(
//!     format_sql(sql, &Dialect::GENERIC),
//!     "SELECT a,\n       b\n  FROM t\n WHERE x = 1\n   AND y = 'a  b'",
//! );
//! ```
//!
//! # The layout: <https://www.sqlstyle.guide/>
//!
//! The rules are that guide's. Clause keywords are right-aligned so that the
//! code after them starts in one column, the *river*: `SELECT`, `  FROM`,
//! ` WHERE`, `   AND`. Each block is measured first and its river is as wide
//! as its longest clause keyword, so a `RETURNING` moves the whole block
//! rather than overhanging it. Select-list and `SET` items after a comma, joins
//! and their `ON` start on the content side of the river; a join condition's
//! `AND` lines up after its `ON`. A subquery opens a block of its own whose
//! river runs from its bracket — beside the column it is a value of, or on a
//! line of its own after `IN` or `EXISTS` — and closes on its last line. A
//! `CASE` lines up its `WHEN`s and `ELSE` under the first `WHEN` and its `END`
//! under itself. Statements are separated by an empty line. Anything the rules
//! do not recognize — DDL, a function's arguments — is written on one line,
//! single-spaced.
//!
//! What the guide asks of the *author* — naming, `AS` for every alias, no
//! vendor functions — is not something a formatter can do without changing the
//! SQL, and this one does not.
//!
//! # Whitespace in, whitespace out
//!
//! Like everything else in this crate it works on the lexer's tokens and has
//! no grammar, so what it may change is deliberately narrow: the whitespace
//! *between* tokens, and the case of a short list of structural keywords.
//! Every string, comment, quoted identifier and dollar-quoted body is copied
//! byte for byte, which is what makes the result safe to run whatever the
//! layout rules get wrong.
//!
//! Two token pairs keep the spacing they were written with rather than gaining
//! one: an opening bracket (`count(*)` against `IN (1, 2)`), and two atoms that
//! touch (`_utf8'x'`, where a space could change the meaning). A line comment
//! always ends its line, so nothing that follows it can be swallowed.
//!
//! # Keyword case
//!
//! Only a short list of structural keywords is uppercased, every one of them
//! reserved in MySQL. A word reserved in one dialect may still be a bare table
//! name in another, and MySQL compares table names case-sensitively on most
//! file systems, so `from user` must not become `FROM USER`. Identifiers, types and
//! function names keep the case they were typed in.
//!
//! # Lines that are not SQL
//!
//! A client directive is a line the server never sees: Oracle's `/`, MySQL's
//! `DELIMITER`, psql's `\` commands, SQL Server's `GO`. Each is copied as it
//! stands and the SQL around it is formatted separately, so the directive keeps
//! its line. Between `DELIMITER //` and `DELIMITER ;` the terminator is not a
//! semicolon, and that whole region is copied unchanged.

use std::collections::HashMap;

use crate::dialect::Dialect;
use crate::lexer::{LineState, TokenKind, lex, lex_line};

/// Keywords written in upper case: reserved words of MySQL, the one product
/// whose bare table names are case-sensitive, plus `END`, which closes `CASE`.
/// Sorted, for the binary search.
const UPPER: &[&str] = &[
    "ALL",
    "ALTER",
    "AND",
    "AS",
    "ASC",
    "BETWEEN",
    "BY",
    "CASE",
    "CHECK",
    "CONSTRAINT",
    "CREATE",
    "CROSS",
    "DEFAULT",
    "DELETE",
    "DESC",
    "DISTINCT",
    "DROP",
    "ELSE",
    "END",
    "EXISTS",
    "FOREIGN",
    "FROM",
    "GROUP",
    "HAVING",
    "IN",
    "INDEX",
    "INNER",
    "INSERT",
    "INTO",
    "IS",
    "JOIN",
    "KEY",
    "LEFT",
    "LIKE",
    "LIMIT",
    "NOT",
    "NULL",
    "ON",
    "OR",
    "ORDER",
    "OVER",
    "PARTITION",
    "PRIMARY",
    "REFERENCES",
    "RIGHT",
    "SELECT",
    "SET",
    "TABLE",
    "THEN",
    "UNION",
    "UNIQUE",
    "UPDATE",
    "USING",
    "VALUES",
    "WHEN",
    "WHERE",
    "WITH",
];

/// Words that may open a join, `JOIN` itself included.
const JOIN_WORDS: &[&str] = &[
    "CROSS", "FULL", "INNER", "JOIN", "LEFT", "NATURAL", "OUTER", "RIGHT",
];

/// Format `source` for reading.
///
/// Leading and trailing whitespace is kept as it was, so formatting a selection
/// leaves the text around it alone. See the [module documentation](self) for
/// what changes and what cannot.
pub fn format_sql(source: &str, dialect: &Dialect) -> String {
    let body = source.trim();
    if body.is_empty() {
        return source.to_owned();
    }
    let lead = source.len() - source.trim_start().len();
    let mut out = String::with_capacity(source.len() + source.len() / 4);
    out.push_str(&source[..lead]);
    let mut pieces = Vec::new();
    for segment in segments(body, dialect) {
        match segment {
            Segment::Sql(sql) => {
                let formatted = format_segment(sql, dialect);
                if !formatted.is_empty() {
                    pieces.push(formatted);
                }
            }
            Segment::Verbatim(text) => pieces.push(text.trim_end().to_owned()),
        }
    }
    let formatted = pieces.join("\n");
    // A statement led by a short keyword (`WITH`) starts right of column 0 to
    // keep its river. The input's own indentation of that line is kept when it
    // is at least as deep, which is what makes formatting twice a no-op.
    let indent = formatted.len() - formatted.trim_start_matches(' ').len();
    let tail = source[..lead].rfind('\n').map_or(0, |i| i + 1);
    if lead - tail >= indent {
        out.push_str(&formatted[indent..]);
    } else {
        out.truncate(tail);
        out.push_str(&formatted);
    }
    out.push_str(&source[lead + body.len()..]);
    out
}

/// A stretch of the input: SQL to lay out, or lines to copy.
enum Segment<'a> {
    Sql(&'a str),
    Verbatim(&'a str),
}

/// Cut `body` at client directive lines.
///
/// A line only counts when it starts outside every comment and quote, which
/// [`lex_line`]'s carried state answers.
fn segments<'a>(body: &'a str, dialect: &Dialect) -> Vec<Segment<'a>> {
    let mut segments = Vec::new();
    let mut state = LineState::START;
    let mut sql_start = 0;
    let mut verbatim_start: Option<usize> = None;
    let mut offset = 0;
    for line in body.split_inclusive('\n') {
        let start = offset;
        offset += line.len();
        let at_start = state.is_start();
        state = lex_line(line, state, dialect).1;
        if !at_start {
            continue;
        }
        let directive = directive(line.trim());
        if let Some(open) = verbatim_start {
            if directive == Some(Directive::DelimiterReset) {
                segments.push(Segment::Verbatim(&body[open..offset]));
                verbatim_start = None;
                sql_start = offset;
            }
            continue;
        }
        let Some(directive) = directive else {
            continue;
        };
        segments.push(Segment::Sql(&body[sql_start..start]));
        if directive == Directive::DelimiterChange {
            verbatim_start = Some(start);
        } else {
            segments.push(Segment::Verbatim(&body[start..offset]));
            sql_start = offset;
        }
    }
    match verbatim_start {
        Some(open) => segments.push(Segment::Verbatim(&body[open..])),
        None => segments.push(Segment::Sql(&body[sql_start..])),
    }
    segments
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Directive {
    /// A line to copy as it stands.
    Line,
    /// `DELIMITER` with anything but `;`: the start of a region to copy.
    DelimiterChange,
    /// `DELIMITER ;`.
    DelimiterReset,
}

fn directive(line: &str) -> Option<Directive> {
    if line == "/" || line.starts_with('\\') {
        return Some(Directive::Line);
    }
    let mut words = line.split_ascii_whitespace();
    let first = words.next()?;
    if first.eq_ignore_ascii_case("go") {
        let count = words.next();
        return (count.is_none_or(|n| n.bytes().all(|b| b.is_ascii_digit()))
            && words.next().is_none())
        .then_some(Directive::Line);
    }
    if first.eq_ignore_ascii_case("delimiter") {
        let delimiter = words.next()?;
        return Some(if delimiter == ";" {
            Directive::DelimiterReset
        } else {
            Directive::DelimiterChange
        });
    }
    None
}

/// One token that is not whitespace, with what preceded it.
struct Item<'a> {
    kind: TokenKind,
    text: &'a str,
    /// Whether whitespace (or the start of the input) came right before it.
    spaced: bool,
    /// Whether that whitespace held a line break.
    newline: bool,
}

impl Item<'_> {
    fn is(&self, word: &str) -> bool {
        self.text.eq_ignore_ascii_case(word)
    }

    fn is_keyword(&self, word: &str) -> bool {
        self.kind == TokenKind::Keyword && self.is(word)
    }

    fn is_punct(&self, punct: &str) -> bool {
        self.kind == TokenKind::Punctuation && self.text == punct
    }

    fn is_line_comment(&self) -> bool {
        self.kind == TokenKind::Comment && !self.text.starts_with("/*")
    }
}

/// What the clause being written breaks on.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Clause {
    /// `SELECT`, `FROM`, `GROUP BY`, `SET`, `WITH`...: a comma starts a line.
    List,
    /// `WHERE`, `HAVING`: `AND` and `OR` start a line, right-aligned to the
    /// river.
    Condition,
    /// `LIMIT`, `UNION`, `INSERT`...: nothing breaks.
    Plain,
}

/// An open `CASE`.
struct Case {
    /// Where `CASE` starts, and so where its `END` goes.
    case: usize,
    /// Where its first `WHEN` starts, and so where the others and `ELSE` go.
    when: Option<usize>,
}

/// One level of nesting.
struct Frame {
    /// A subquery or the statement itself; otherwise a bracket written inline.
    block: bool,
    /// Which block this is, for the river widths of the first pass: the index
    /// of its opening bracket, or of the statement's first token.
    key: usize,
    /// Where the block's text starts; for a bracket written inline, where its
    /// continuation lines do.
    left: usize,
    /// The column the block's clause keywords end at.
    river: usize,
    clause: Option<Clause>,
    /// Inside `FROM`, where a join starts a line.
    joins: bool,
    /// After a join's `ON`: where its `AND` and `OR` start.
    on: Option<usize>,
    /// Inside `BETWEEN ... AND`, whose `AND` is not a condition.
    between: bool,
    /// Open `CASE` expressions, inside which no clause or condition breaks.
    cases: Vec<Case>,
}

impl Frame {
    fn new(block: bool, key: usize, left: usize, width: usize) -> Self {
        Self {
            block,
            key,
            left,
            river: left + width,
            clause: None,
            joins: false,
            on: None,
            between: false,
            cases: Vec::new(),
        }
    }

    /// Where a continuation line of this frame starts.
    fn continuation(&self) -> usize {
        if self.block && self.clause.is_some() {
            self.on.unwrap_or(self.river + 1)
        } else {
            self.left
        }
    }
}

/// The output, a line at a time.
struct Writer {
    out: String,
    line_start: usize,
}

impl Writer {
    fn line_is_blank(&self) -> bool {
        self.out[self.line_start..].trim().is_empty()
    }

    /// The column the next character lands in.
    fn column(&self) -> usize {
        self.out[self.line_start..].chars().count()
    }

    fn trim_end(&mut self) {
        let len = self.out.trim_end_matches([' ', '\t']).len();
        self.out.truncate(len);
    }

    /// Start a line at `column`, or re-indent the current one if it is empty.
    fn break_line(&mut self, column: usize) {
        if self.line_is_blank() {
            self.out.truncate(self.line_start);
        } else {
            self.trim_end();
            self.out.push('\n');
            self.line_start = self.out.len();
        }
        self.out.extend(std::iter::repeat_n(' ', column));
    }

    /// End the statement with an empty line.
    fn blank_line(&mut self) {
        self.out.truncate(self.out.trim_end().len());
        self.out.push_str("\n\n");
        self.line_start = self.out.len();
    }

    /// Writes `text`, and answers the column it starts in.
    fn write(&mut self, text: &str, space: bool) -> usize {
        if space && !self.line_is_blank() {
            self.out.push(' ');
        }
        let column = self.column();
        self.out.push_str(text);
        if let Some(i) = text.rfind('\n') {
            self.line_start = self.out.len() - (text.len() - i - 1);
        }
        column
    }
}

/// The tokens of `sql` that are not whitespace.
fn items<'a>(sql: &'a str, dialect: &Dialect) -> Vec<Item<'a>> {
    let mut items = Vec::new();
    let mut spaced = true;
    let mut newline = false;
    for token in lex(sql, dialect) {
        let text = token.text(sql);
        if token.kind == TokenKind::Whitespace {
            spaced = true;
            newline |= text.contains('\n');
            continue;
        }
        let text = if token.kind == TokenKind::Comment && !text.starts_with("/*") {
            text.trim_end()
        } else {
            text
        };
        items.push(Item {
            kind: token.kind,
            text,
            spaced,
            newline,
        });
        spaced = false;
        newline = false;
    }
    items
}

/// The next token after `i` that is not a comment.
fn next_significant(items: &[Item<'_>], i: usize) -> Option<usize> {
    (i + 1..items.len()).find(|&j| items[j].kind != TokenKind::Comment)
}

/// Whether `JOIN` follows the run of join words starting at `i`.
fn starts_join(items: &[Item<'_>], mut i: usize) -> bool {
    loop {
        let item = &items[i];
        if item.kind != TokenKind::Keyword {
            return false;
        }
        if item.is("JOIN") || item.is("APPLY") {
            return true;
        }
        if !JOIN_WORDS.iter().any(|w| item.is(w)) {
            return false;
        }
        match next_significant(items, i) {
            Some(next) => i = next,
            None => return false,
        }
    }
}

/// Whether a space goes between `prev` and `cur`; `before` precedes `prev`.
fn needs_space(prev: Option<&Item<'_>>, before: Option<&Item<'_>>, cur: &Item<'_>) -> bool {
    use TokenKind::{Error, Keyword, Operator, Punctuation};
    let Some(prev) = prev else {
        return false;
    };
    if cur.kind == Punctuation && matches!(cur.text, "," | ";" | ")" | "]" | ".") {
        return false;
    }
    if prev.kind == Punctuation && matches!(prev.text, "(" | "[" | ".") {
        return false;
    }
    if (cur.kind == Operator && cur.text == "::") || (prev.kind == Operator && prev.text == "::") {
        return false;
    }
    if cur.kind == Punctuation && matches!(cur.text, "(" | "[") {
        return cur.spaced;
    }
    let atom = |kind| !matches!(kind, Operator | Punctuation);
    if !cur.spaced && (prev.kind == Error || cur.kind == Error || atom(prev.kind) && atom(cur.kind))
    {
        return false;
    }
    let unary = prev.kind == Operator
        && matches!(prev.text, "-" | "+" | "~")
        && before.is_none_or(|b| {
            matches!(b.kind, Operator | Keyword)
                || (b.kind == Punctuation && matches!(b.text, "(" | "[" | ","))
        });
    if unary {
        return cur.spaced;
    }
    true
}

/// River width when a block has no clause keyword to measure: `SELECT`'s.
const RIVER: usize = 6;

/// Lay out a piece of SQL holding no directive lines, trimmed.
///
/// Two passes: the first measures each block's longest clause keyword, which
/// the second needs from the block's first line on to right-align them all.
/// The layout of a block never depends on its own river width, so both passes
/// see the same blocks.
fn format_segment(sql: &str, dialect: &Dialect) -> String {
    let items = items(sql, dialect);
    let (_, widths) = layout(&items, &HashMap::new());
    layout(&items, &widths).0
}

/// The clause keyword `item` is, if it is one where it stands, and what its
/// clause breaks on.
fn clause_of(
    items: &[Item<'_>],
    i: usize,
    prev: Option<&Item<'_>>,
    first_keyword: Option<&str>,
) -> Option<Clause> {
    let item = &items[i];
    let follows = |word: &str| prev.is_some_and(|p| p.is_keyword(word));
    let leads = prev.is_none_or(|p| p.is_punct("(") || p.is_punct(")"));
    let by_next = next_significant(items, i).is_some_and(|n| items[n].is_keyword("BY"));
    let updating = first_keyword
        .is_some_and(|k| k.eq_ignore_ascii_case("UPDATE") || k.eq_ignore_ascii_case("MERGE"));
    match item.text.to_ascii_uppercase().as_str() {
        "SELECT" | "VALUES" | "RETURNING" | "WINDOW" => Some(Clause::List),
        "GROUP" | "ORDER" if by_next => Some(Clause::List),
        "WITH" if prev.is_none_or(|p| p.is_punct("(")) => Some(Clause::List),
        "SET" if updating || follows("UPDATE") => Some(Clause::List),
        "FROM" if !follows("DELETE") && !follows("DISTINCT") => Some(Clause::List),
        "WHERE" | "HAVING" => Some(Clause::Condition),
        "LIMIT" | "OFFSET" | "FETCH" | "UNION" | "INTERSECT" | "EXCEPT" | "MINUS" => {
            Some(Clause::Plain)
        }
        "INSERT" | "UPDATE" | "DELETE" if leads => Some(Clause::Plain),
        _ => None,
    }
}

/// One pass over the tokens: the text, and each block's widest clause keyword.
fn layout(items: &[Item<'_>], widths: &HashMap<usize, usize>) -> (String, HashMap<usize, usize>) {
    let width = |key: usize| widths.get(&key).copied().unwrap_or(RIVER);
    let mut measured: HashMap<usize, usize> = HashMap::new();
    let mut w = Writer {
        out: String::new(),
        line_start: 0,
    };
    let mut stack = vec![Frame::new(true, 0, 0, width(0))];
    // A line break owed before the next token that is not a trailing comment.
    let mut pending: Option<usize> = None;
    let mut statement_ended = false;
    // The two significant (non-comment) tokens before the current one.
    let mut prev: Option<usize> = None;
    let mut before: Option<usize> = None;
    let mut first_keyword: Option<&str> = None;

    for (i, item) in items.iter().enumerate() {
        if statement_ended && (item.kind != TokenKind::Comment || item.newline) {
            w.blank_line();
            statement_ended = false;
            pending = None;
            stack = vec![Frame::new(true, i, 0, width(i))];
            prev = None;
            before = None;
            first_keyword = None;
        }
        let continuation = stack.last().map_or(0, Frame::continuation);

        if item.kind == TokenKind::Comment {
            if item.newline {
                let column = pending.take().unwrap_or(continuation);
                w.break_line(column);
                w.write(item.text, false);
            } else {
                w.write(item.text, true);
            }
            if item.is_line_comment() || items.get(i + 1).is_some_and(|next| next.newline) {
                pending.get_or_insert(continuation);
            }
            continue;
        }

        let prev_item = prev.map(|p| &items[p]);
        let next = next_significant(items, i);
        let mut break_at: Option<usize> = None;
        let mut opens: Option<bool> = None;
        let mut opens_case = false;

        if item.kind == TokenKind::Keyword {
            first_keyword.get_or_insert(item.text);
        }
        let frame = stack
            .last_mut()
            .expect("the statement frame is never popped");
        let keyword = item.kind == TokenKind::Keyword;
        let clause = (keyword && frame.block && frame.cases.is_empty())
            .then(|| clause_of(items, i, prev_item, first_keyword))
            .flatten();
        if let Some(clause) = clause {
            let len = item.text.chars().count();
            let widest = measured.entry(frame.key).or_insert(0);
            *widest = (*widest).max(len);
            frame.clause = Some(clause);
            frame.joins = item.is("FROM");
            frame.on = None;
            frame.between = false;
            // The block's first keyword stays beside the bracket that opens it.
            if !prev_item.is_some_and(|p| p.is_punct("(")) {
                break_at = Some(frame.river.saturating_sub(len).max(frame.left));
            }
        } else if keyword && frame.block && frame.cases.is_empty() {
            let upper = item.text.to_ascii_uppercase();
            if frame.joins
                && JOIN_WORDS.contains(&upper.as_str())
                && !prev_item.is_some_and(|p| {
                    p.kind == TokenKind::Keyword && JOIN_WORDS.iter().any(|w| p.is(w))
                })
                && !next.is_some_and(|n| items[n].is_punct("("))
                && starts_join(items, i)
            {
                frame.clause = Some(Clause::List);
                frame.on = None;
                break_at = Some(frame.river + 1);
            } else if upper == "ON" && frame.joins {
                break_at = Some(frame.river + 1);
                frame.on = Some(frame.river + 1 + "ON ".len());
            } else if upper == "AND" && frame.between {
                frame.between = false;
            } else if upper == "AND" || upper == "OR" {
                if let Some(on) = frame.on {
                    break_at = Some(on);
                } else if frame.clause == Some(Clause::Condition) {
                    break_at = Some(frame.river.saturating_sub(upper.len()));
                }
            }
        }
        if keyword {
            if item.is("BETWEEN") {
                frame.between = true;
            } else if item.is("CASE") {
                opens_case = true;
            } else if let Some(case) = frame.cases.last() {
                let branch = item.is("WHEN") || item.is("ELSE");
                if branch && !prev_item.is_some_and(|p| p.is_keyword("CASE")) {
                    break_at = Some(case.when.unwrap_or(case.case));
                } else if item.is("END") {
                    break_at = Some(case.case);
                }
            }
        } else if item.is_punct("(") {
            let block = next.is_some_and(|n| {
                ["SELECT", "WITH", "VALUES"]
                    .iter()
                    .any(|w| items[n].is_keyword(w))
            });
            // `IN (SELECT ...)` and its kin: the subquery starts its own line
            // on the content side of the river.
            if block
                && frame.block
                && frame.cases.is_empty()
                && frame.clause.is_some()
                && prev_item.is_some_and(|p| {
                    ["IN", "EXISTS", "ANY", "ALL", "SOME"]
                        .iter()
                        .any(|w| p.is_keyword(w))
                })
            {
                break_at = Some(frame.continuation());
            }
            opens = Some(block);
        } else if item.is_punct(")") && stack.len() > 1 {
            stack.pop();
        }

        if let Some(column) = break_at {
            pending = None;
            w.break_line(column);
        } else if let Some(column) = pending.take() {
            w.break_line(column);
        }

        let dotted = prev_item.is_some_and(|p| p.is_punct("."))
            || next.is_some_and(|n| items[n].is_punct("."));
        let upper;
        let text = if keyword
            && !dotted
            && UPPER
                .binary_search_by(|w| cmp_ignore_case(w, item.text))
                .is_ok()
        {
            upper = item.text.to_ascii_uppercase();
            upper.as_str()
        } else {
            item.text
        };
        let space = needs_space(prev_item, before.map(|b| &items[b]), item);
        let column = w.write(text, space);

        let frame = stack
            .last_mut()
            .expect("the statement frame is never popped");
        if opens_case {
            frame.cases.push(Case {
                case: column,
                when: None,
            });
        } else if keyword && item.is("WHEN") {
            if let Some(case) = frame.cases.last_mut() {
                case.when.get_or_insert(column);
            }
        } else if keyword && item.is("END") {
            frame.cases.pop();
        }
        if let Some(block) = opens {
            let left = w.column();
            stack.push(if block {
                Frame::new(true, i, left, width(i))
            } else {
                Frame::new(false, i, left, 0)
            });
        } else if item.is_punct(",")
            && frame.block
            && frame.cases.is_empty()
            && frame.clause == Some(Clause::List)
        {
            pending = Some(frame.river + 1);
        } else if item.is_punct(";") {
            statement_ended = true;
        }
        before = prev;
        prev = Some(i);
    }
    w.trim_end();
    let text = w.out.trim_start_matches(['\n', '\r']).trim_end().to_owned();
    (text, measured)
}

/// `word` (upper case) against `text` in any case.
fn cmp_ignore_case(word: &str, text: &str) -> std::cmp::Ordering {
    word.bytes()
        .cmp(text.bytes().map(|b| b.to_ascii_uppercase()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fmt(sql: &str) -> String {
        format_sql(sql, &Dialect::GENERIC)
    }

    #[test]
    fn upper_is_sorted() {
        assert!(UPPER.windows(2).all(|w| w[0] < w[1]));
    }

    /// The examples of <https://www.sqlstyle.guide/>, as that guide writes them.
    #[test]
    fn follows_the_sql_style_guide() {
        let pg = Dialect::from_id("postgres");
        assert_eq!(
            format_sql(
                "select r.last_name, (select max(year(championship_date)) from champions as c \
                 where c.last_name = r.last_name and c.confirmed = 'Y') as last_championship_year \
                 from riders as r where r.last_name in (select c.last_name from champions as c \
                 where year(championship_date) > '2008' and c.confirmed = 'Y');",
                &pg,
            ),
            "SELECT r.last_name,
       (SELECT max(year(championship_date))
          FROM champions AS c
         WHERE c.last_name = r.last_name
           AND c.confirmed = 'Y') AS last_championship_year
  FROM riders AS r
 WHERE r.last_name IN
       (SELECT c.last_name
          FROM champions AS c
         WHERE year(championship_date) > '2008'
           AND c.confirmed = 'Y');",
        );
        assert_eq!(
            format_sql(
                "select r.last_name from riders as r inner join bikes as b \
                 on r.bike_vin_num = b.vin_num and b.engine_tally > 2 \
                 inner join crew as c on r.crew_chief_last_name = c.last_name and c.chief = 'Y';",
                &pg,
            ),
            "SELECT r.last_name
  FROM riders AS r
       INNER JOIN bikes AS b
       ON r.bike_vin_num = b.vin_num
          AND b.engine_tally > 2
       INNER JOIN crew AS c
       ON r.crew_chief_last_name = c.last_name
          AND c.chief = 'Y';",
        );
        assert_eq!(
            format_sql(
                "select case postcode when 'BN1' then 'Brighton' when 'EH1' then 'Edinburgh' \
                 end as city from office_locations where country = 'United Kingdom' \
                 and opening_time between 8 and 9 and postcode in ('EH1', 'BN1', 'NN1', 'KW1')",
                &pg,
            ),
            "SELECT CASE postcode
       WHEN 'BN1' THEN 'Brighton'
       WHEN 'EH1' THEN 'Edinburgh'
       END AS city
  FROM office_locations
 WHERE country = 'United Kingdom'
   AND opening_time BETWEEN 8 AND 9
   AND postcode IN ('EH1', 'BN1', 'NN1', 'KW1')",
        );
    }

    #[test]
    fn clause_keywords_are_right_aligned_to_the_river() {
        assert_eq!(
            format_sql(
                "select a, b as c, count(*) from t left join u on t.id = u.id \
                 where a = 1 and b between 1 and 2 or c is not null \
                 group by a, b order by a desc limit 10",
                &Dialect::from_id("postgres"),
            ),
            "SELECT a,
       b AS c,
       count(*)
  FROM t
       LEFT JOIN u
       ON t.id = u.id
 WHERE a = 1
   AND b BETWEEN 1 AND 2
    OR c IS NOT NULL
 GROUP BY a,
       b
 ORDER BY a DESC
 LIMIT 10",
        );
    }

    #[test]
    fn a_longer_keyword_widens_the_river() {
        assert_eq!(
            format_sql(
                "select a from t union all select b from u returning id",
                &Dialect::from_id("postgres"),
            ),
            "   SELECT a
     FROM t
    UNION ALL
   SELECT b
     FROM u
returning id",
        );
    }

    #[test]
    fn a_case_aligns_its_branches() {
        assert_eq!(
            fmt("select case when a = 1 then 'x' when a = 2 then 'y' else 'z' end as k from t"),
            "SELECT CASE WHEN a = 1 THEN 'x'
            WHEN a = 2 THEN 'y'
            ELSE 'z'
       END AS k
  FROM t",
        );
    }

    #[test]
    fn subqueries_hang_off_their_bracket() {
        assert_eq!(
            fmt(
                "select * from (select id from t where x in (1,2)) s where exists (select 1 from u)"
            ),
            "SELECT *
  FROM (SELECT id
          FROM t
         WHERE x IN (1, 2)) s
 WHERE EXISTS
       (SELECT 1
          FROM u)",
        );
        assert_eq!(
            fmt("with a as (select 1 x), b as (select 2 y) select * from a, b"),
            "  WITH a AS (SELECT 1 x),
       b AS (SELECT 2 y)
SELECT *
  FROM a,
       b",
        );
    }

    #[test]
    fn statements_are_separated_by_a_blank_line() {
        assert_eq!(
            fmt(
                "insert into t (a, b) values (1, 'x'), (2, 'y');update t set a = 1, b = 2 where c = 3;delete from t where a=-1"
            ),
            "INSERT INTO t (a, b)
VALUES (1, 'x'),
       (2, 'y');

UPDATE t
   SET a = 1,
       b = 2
 WHERE c = 3;

DELETE FROM t
 WHERE a = -1",
        );
    }

    #[test]
    fn literals_comments_and_quoted_names_are_untouched() {
        let sql = "select 'a  ,  b' , \"Mixed  Name\" -- keep  this\n, /* x   y */ 1 from t";
        assert_eq!(
            fmt(sql),
            "SELECT 'a  ,  b',
       \"Mixed  Name\" -- keep  this
       , /* x   y */
       1
  FROM t",
        );
        let pg = Dialect::from_id("postgres");
        let body = "create function f() returns int as $$ select  1 ; $$ language sql";
        assert!(format_sql(body, &pg).contains("$$ select  1 ; $$"));
        let my = Dialect::from_id("mysql");
        assert_eq!(
            format_sql("select `a  b`, x from `t` # tail", &my),
            "SELECT `a  b`,\n       x\n  FROM `t` # tail",
        );
    }

    #[test]
    fn a_line_comment_always_ends_its_line() {
        assert_eq!(
            fmt("select a -- why\n+ 1 from t"),
            "SELECT a -- why\n       + 1\n  FROM t"
        );
    }

    #[test]
    fn keyword_case_changes_only_for_safe_words() {
        assert_eq!(
            fmt("select user, t.order from user"),
            "SELECT user,\n       t.order\n  FROM user"
        );
        assert_eq!(
            fmt("Select LEFT(name, 2)::text From t"),
            "SELECT LEFT(name, 2)::text\n  FROM t"
        );
    }

    #[test]
    fn leading_and_trailing_whitespace_is_kept() {
        assert_eq!(fmt("\n  select 1\n\n"), "\n  SELECT 1\n\n");
        assert_eq!(fmt(" select 1"), " SELECT 1");
        assert_eq!(fmt("   "), "   ");
    }

    #[test]
    fn directive_lines_are_copied() {
        let ora = Dialect::from_id("oracle");
        assert_eq!(
            format_sql("begin  null; end;\n/\nselect 1 from dual", &ora),
            "begin NULL;\n\nEND;\n/\nSELECT 1\n  FROM dual",
        );
        let my = Dialect::from_id("mysql");
        let script =
            "delimiter //\ncreate procedure p() begin  select 1; end//\ndelimiter ;\nselect  2";
        assert_eq!(
            format_sql(script, &my),
            "delimiter //\ncreate procedure p() begin  select 1; end//\ndelimiter ;\nSELECT 2",
        );
    }

    #[test]
    fn formatting_is_idempotent() {
        for sql in [
            "with a as (select 1 x), b as (select 2 y) select * from a, b where a.x <> b.y",
            "select case when a then 1 else 2 end, x from t join u on t.a = u.a and t.b = u.b",
            "select a from t union select b from u order by 1 returning a",
        ] {
            let once = fmt(sql);
            assert_eq!(fmt(&once), once);
        }
    }

    #[test]
    fn unterminated_input_is_kept_whole() {
        assert_eq!(fmt("select 'abc  "), "SELECT 'abc  ");
        assert_eq!(fmt("select 1 /* open  "), "SELECT 1 /* open  ");
    }

    #[test]
    fn only_whitespace_and_keyword_case_change() {
        let samples = [
            (
                "postgres",
                "select a::int, -b, arr[1], $1, e'x\\'y' from s.t where j->>'k' = :name;",
            ),
            (
                "mysql",
                "select _utf8'x', @v, @@global.x, `q` from t where a<=>b # c\n;",
            ),
            (
                "mssql",
                "select top 5 [a b], @p from dbo.t with (nolock) where x like N'%y%'",
            ),
            (
                "oracle",
                "select q'[it]' , nvl(a,0) from dual connect by level < 3",
            ),
            (
                "generic",
                "select {fn now()}, case when a then -1 else +2 end from t",
            ),
        ];
        for (dialect, sql) in samples {
            let dialect = Dialect::from_id(dialect);
            let out = format_sql(sql, &dialect);
            let words = |s: &str| -> Vec<String> {
                items(s, &dialect)
                    .iter()
                    .map(|i| match i.kind {
                        TokenKind::Keyword => i.text.to_ascii_uppercase(),
                        _ => i.text.to_owned(),
                    })
                    .collect()
            };
            assert_eq!(words(sql), words(&out), "{out}");
        }
    }
}
