//! Laying a script out again: the editor's "Format SQL".
//!
//! ```
//! use rudbman_sql::{Dialect, format_sql};
//!
//! let sql = "select a, b from t where x = 1 and y = 'a  b'";
//! assert_eq!(
//!     format_sql(sql, &Dialect::GENERIC),
//!     "SELECT a,\n    b\nFROM t\nWHERE x = 1\n    AND y = 'a  b'",
//! );
//! ```
//!
//! # Whitespace in, whitespace out
//!
//! Like everything else in this crate it works on the lexer's tokens and has
//! no grammar, so what it may change is deliberately narrow: the whitespace
//! *between* tokens, and the case of a short list of structural keywords.
//! Every string, comment, quoted identifier and dollar-quoted body is copied
//! byte for byte, which is what makes the result safe to run whatever the
//! layout rules get wrong. The layout itself is a handful of rules —
//! clauses start lines, commas in a select list and `AND`/`OR` in a condition
//! break onto indented continuation lines, a parenthesized subquery is a block
//! of its own — and anything they do not recognize is written on one line,
//! single-spaced.
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

use crate::dialect::Dialect;
use crate::lexer::{LineState, TokenKind, lex, lex_line};

/// Spaces per indentation level, matching the editor's tab.
const INDENT: &str = "    ";

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
    out.push_str(&pieces.join("\n"));
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
    /// `SELECT`, `FROM`, `GROUP BY`, `SET`...: a comma starts a line.
    List,
    /// `WITH`: a comma starts a line at the clause's own level.
    Ctes,
    /// `WHERE`, `HAVING`, a join's `ON`: `AND` and `OR` start a line.
    Condition,
    /// `LIMIT`, `UNION`...: nothing breaks.
    Plain,
}

/// One level of nesting.
struct Frame {
    /// A subquery or the statement itself; otherwise a bracket written inline.
    block: bool,
    /// Indentation of the clause keywords in a block; of continuation lines
    /// otherwise.
    base: usize,
    clause: Option<Clause>,
    /// Inside `FROM`, where a join starts a line.
    joins: bool,
    /// Inside `BETWEEN ... AND`, whose `AND` is not a condition.
    between: bool,
    /// Open `CASE` expressions, inside which nothing breaks.
    cases: usize,
}

impl Frame {
    const fn new(block: bool, base: usize) -> Self {
        Self {
            block,
            base,
            clause: None,
            joins: false,
            between: false,
            cases: 0,
        }
    }

    /// Where a continuation line of this frame starts.
    fn continuation(&self) -> usize {
        if self.block && self.clause.is_some() {
            self.base + 1
        } else {
            self.base
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

    fn line_indent(&self) -> usize {
        let line = &self.out[self.line_start..];
        (line.len() - line.trim_start_matches(' ').len()) / INDENT.len()
    }

    fn trim_end(&mut self) {
        let len = self.out.trim_end_matches([' ', '\t']).len();
        self.out.truncate(len);
    }

    /// Start a line at `indent`, or re-indent the current one if it is empty.
    fn break_line(&mut self, indent: usize) {
        if self.line_is_blank() {
            self.out.truncate(self.line_start);
        } else {
            self.trim_end();
            self.out.push('\n');
            self.line_start = self.out.len();
        }
        self.out.push_str(&INDENT.repeat(indent));
    }

    /// End the statement with an empty line.
    fn blank_line(&mut self) {
        self.out.truncate(self.out.trim_end().len());
        self.out.push_str("\n\n");
        self.line_start = self.out.len();
    }

    fn write(&mut self, text: &str, space: bool) {
        if space && !self.line_is_blank() {
            self.out.push(' ');
        }
        self.out.push_str(text);
        if let Some(i) = text.rfind('\n') {
            self.line_start = self.out.len() - (text.len() - i - 1);
        }
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

/// Lay out a piece of SQL holding no directive lines, trimmed.
fn format_segment(sql: &str, dialect: &Dialect) -> String {
    let items = items(sql, dialect);
    let mut w = Writer {
        out: String::with_capacity(sql.len() + sql.len() / 4),
        line_start: 0,
    };
    let mut stack = vec![Frame::new(true, 0)];
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
            stack = vec![Frame::new(true, 0)];
            prev = None;
            before = None;
            first_keyword = None;
        }
        let continuation = stack.last().map_or(0, Frame::continuation);

        if item.kind == TokenKind::Comment {
            if item.newline {
                let indent = pending.take().unwrap_or(continuation);
                w.break_line(indent);
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
        let next = next_significant(&items, i);
        let follows = |word: &str| prev_item.is_some_and(|p| p.is_keyword(word));
        let mut break_at: Option<usize> = None;
        let mut opens: Option<bool> = None;

        if item.kind == TokenKind::Keyword {
            first_keyword.get_or_insert(item.text);
        }
        let frame = stack
            .last_mut()
            .expect("the statement frame is never popped");
        if item.kind == TokenKind::Keyword && frame.block && frame.cases == 0 {
            let upper = item.text.to_ascii_uppercase();
            let by_next = next.is_some_and(|n| items[n].is_keyword("BY"));
            let updating = first_keyword.is_some_and(|k| {
                k.eq_ignore_ascii_case("UPDATE") || k.eq_ignore_ascii_case("MERGE")
            });
            let clause = match upper.as_str() {
                "SELECT" | "VALUES" | "RETURNING" | "WINDOW" => Some(Clause::List),
                "GROUP" | "ORDER" if by_next => Some(Clause::List),
                "WITH" if prev_item.is_none_or(|p| p.is_punct("(")) => Some(Clause::Ctes),
                "SET" if updating || follows("UPDATE") => Some(Clause::List),
                "FROM" if !follows("DELETE") && !follows("DISTINCT") => Some(Clause::List),
                "WHERE" | "HAVING" => Some(Clause::Condition),
                "LIMIT" | "OFFSET" | "FETCH" | "UNION" | "INTERSECT" | "EXCEPT" | "MINUS" => {
                    Some(Clause::Plain)
                }
                _ => None,
            };
            if let Some(clause) = clause {
                frame.clause = Some(clause);
                frame.joins = upper == "FROM";
                frame.between = false;
                break_at = Some(frame.base);
            } else if frame.joins
                && JOIN_WORDS.contains(&upper.as_str())
                && !prev_item.is_some_and(|p| {
                    p.kind == TokenKind::Keyword && JOIN_WORDS.iter().any(|w| p.is(w))
                })
                && !next.is_some_and(|n| items[n].is_punct("("))
                && starts_join(&items, i)
            {
                frame.clause = Some(Clause::List);
                break_at = Some(frame.base + 1);
            } else if upper == "ON" && frame.joins {
                frame.clause = Some(Clause::Condition);
            } else if upper == "AND" && frame.between {
                frame.between = false;
            } else if (upper == "AND" || upper == "OR") && frame.clause == Some(Clause::Condition) {
                break_at = Some(frame.base + 1);
            }
        }
        if item.kind == TokenKind::Keyword {
            if item.is("BETWEEN") {
                frame.between = true;
            } else if item.is("CASE") {
                frame.cases += 1;
            } else if item.is("END") && frame.cases > 0 {
                frame.cases -= 1;
            }
        } else if item.is_punct("(") {
            opens = Some(next.is_some_and(|n| {
                ["SELECT", "WITH", "VALUES"]
                    .iter()
                    .any(|w| items[n].is_keyword(w))
            }));
        } else if item.is_punct(")") && stack.len() > 1 {
            let closed = stack.pop().expect("checked above");
            if closed.block {
                break_at = Some(closed.base.saturating_sub(1));
            }
        }

        if let Some(indent) = break_at {
            pending = None;
            w.break_line(indent);
        } else if let Some(indent) = pending.take() {
            w.break_line(indent);
        }

        let dotted = prev_item.is_some_and(|p| p.is_punct("."))
            || next.is_some_and(|n| items[n].is_punct("."));
        let upper;
        let text = if item.kind == TokenKind::Keyword
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
        w.write(text, space);

        let frame = stack.last().expect("the statement frame is never popped");
        if let Some(block) = opens {
            stack.push(Frame::new(block, w.line_indent() + 1));
        } else if item.is_punct(",") && frame.block && frame.cases == 0 {
            match frame.clause {
                Some(Clause::List) => pending = Some(frame.base + 1),
                Some(Clause::Ctes) => pending = Some(frame.base),
                _ => {}
            }
        } else if item.is_punct(";") {
            statement_ended = true;
        }
        before = prev;
        prev = Some(i);
    }
    w.out.trim().to_owned()
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

    #[test]
    fn lays_out_a_select() {
        assert_eq!(
            format_sql(
                "select a, b as c, count(*) from t left join u on t.id = u.id \
                 where a = 1 and b between 1 and 2 or c is not null \
                 group by a, b order by a desc limit 10",
                &Dialect::from_id("postgres"),
            ),
            "SELECT a,\n    b AS c,\n    count(*)\nFROM t\n    LEFT JOIN u ON t.id = u.id\n\
             WHERE a = 1\n    AND b BETWEEN 1 AND 2\n    OR c IS NOT NULL\n\
             GROUP BY a,\n    b\nORDER BY a DESC\nLIMIT 10",
        );
    }

    #[test]
    fn subqueries_are_blocks() {
        assert_eq!(
            fmt(
                "select * from (select id from t where x in (1,2)) s where exists (select 1 from u)"
            ),
            "SELECT *\nFROM (\n    SELECT id\n    FROM t\n    WHERE x IN (1, 2)\n) s\n\
             WHERE EXISTS (\n    SELECT 1\n    FROM u\n)",
        );
    }

    #[test]
    fn statements_are_separated_by_a_blank_line() {
        assert_eq!(
            fmt(
                "insert into t (a, b) values (1, 'x'), (2, 'y');update t set a = 1, b = 2 where c = 3;delete from t where a=-1"
            ),
            "INSERT INTO t (a, b)\nVALUES (1, 'x'),\n    (2, 'y');\n\n\
             UPDATE t\nSET a = 1,\n    b = 2\nWHERE c = 3;\n\n\
             DELETE FROM t\nWHERE a = -1",
        );
    }

    #[test]
    fn literals_comments_and_quoted_names_are_untouched() {
        let sql = "select 'a  ,  b' , \"Mixed  Name\" -- keep  this\n, /* x   y */ 1 from t";
        assert_eq!(
            fmt(sql),
            "SELECT 'a  ,  b',\n    \"Mixed  Name\" -- keep  this\n    , /* x   y */\n    1\nFROM t",
        );
        let pg = Dialect::from_id("postgres");
        let body = "create function f() returns int as $$ select  1 ; $$ language sql";
        assert!(format_sql(body, &pg).contains("$$ select  1 ; $$"));
        let my = Dialect::from_id("mysql");
        assert_eq!(
            format_sql("select `a  b`, x from `t` # tail", &my),
            "SELECT `a  b`,\n    x\nFROM `t` # tail",
        );
    }

    #[test]
    fn a_line_comment_always_ends_its_line() {
        assert_eq!(
            fmt("select a -- why\n+ 1 from t"),
            "SELECT a -- why\n    + 1\nFROM t"
        );
    }

    #[test]
    fn keyword_case_changes_only_for_safe_words() {
        assert_eq!(
            fmt("select user, t.order from user"),
            "SELECT user,\n    t.order\nFROM user"
        );
        assert_eq!(
            fmt("Select LEFT(name, 2)::text From t"),
            "SELECT LEFT(name, 2)::text\nFROM t"
        );
    }

    #[test]
    fn leading_and_trailing_whitespace_is_kept() {
        assert_eq!(fmt("\n  select 1\n\n"), "\n  SELECT 1\n\n");
        assert_eq!(fmt("   "), "   ");
    }

    #[test]
    fn directive_lines_are_copied() {
        let ora = Dialect::from_id("oracle");
        assert_eq!(
            format_sql("begin  null; end;\n/\nselect 1 from dual", &ora),
            "begin NULL;\n\nEND;\n/\nSELECT 1\nFROM dual",
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
        let once =
            fmt("with a as (select 1 x), b as (select 2 y) select * from a, b where a.x <> b.y");
        assert_eq!(fmt(&once), once);
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
