//! Laying out a `CREATE` statement for reading: [`format_ddl`], what the
//! object panel's DDL tab shows.

use super::{Item, Segment, UPPER, cmp_ignore_case, format_segment, items, needs_space, segments};
use crate::dialect::{Dialect, DialectId};
use crate::lexer::{TokenKind, lex};

/// One level of indentation.
const INDENT: &str = "    ";

/// Words that start a statement and are keywords there whatever the dialect's
/// tables say, so they are written in upper case like the structural words.
const STATEMENT_WORDS: &[&str] = &[
    "CALL", "CLOSE", "CONTINUE", "DECLARE", "EXECUTE", "EXIT", "FETCH", "GET", "ITERATE", "LEAVE",
    "NULL", "OPEN", "PERFORM", "RAISE", "RESIGNAL", "RETURN", "SET", "SIGNAL",
];

/// Words before a routine's name that say what is being created.
const LEAD_WORDS: &[&str] = &[
    "AGGREGATE",
    "CREATE",
    "DEFINER",
    "EDITIONABLE",
    "FUNCTION",
    "NONEDITIONABLE",
    "OR",
    "PROCEDURE",
    "REPLACE",
];

/// The words of a view's header up to its `AS`.
const VIEW_WORDS: &[&str] = &[
    "AS",
    "CREATE",
    "EDITIONABLE",
    "FORCE",
    "MATERIALIZED",
    "NOFORCE",
    "NONEDITIONABLE",
    "OR",
    "RECURSIVE",
    "REPLACE",
    "TEMP",
    "TEMPORARY",
    "VIEW",
];

/// The words that open a sequence's options, one to a line.
const SEQUENCE_OPTIONS: &[&str] = &[
    "AS",
    "CACHE",
    "COMMENT",
    "CYCLE",
    "ENGINE",
    "EXTEND",
    "GLOBAL",
    "INCREMENT",
    "KEEP",
    "MAXVALUE",
    "MINVALUE",
    "NO",
    "NOCACHE",
    "NOCYCLE",
    "NOEXTEND",
    "NOKEEP",
    "NOMAXVALUE",
    "NOMINVALUE",
    "NOORDER",
    "NOSCALE",
    "NOSHARD",
    "ORDER",
    "OWNED",
    "RESTART",
    "SCALE",
    "SESSION",
    "SHARD",
    "START",
];

/// The words of a sequence's options.
const SEQUENCE_WORDS: &[&str] = &[
    "AS",
    "BY",
    "CACHE",
    "CREATE",
    "CYCLE",
    "EXISTS",
    "GLOBAL",
    "IF",
    "INCREMENT",
    "KEEP",
    "MAXVALUE",
    "MINVALUE",
    "NO",
    "NOCACHE",
    "NOCYCLE",
    "NOKEEP",
    "NOMAXVALUE",
    "NOMINVALUE",
    "NOORDER",
    "NOSCALE",
    "NOT",
    "OR",
    "ORDER",
    "REPLACE",
    "RESTART",
    "SCALE",
    "SEQUENCE",
    "SESSION",
    "START",
    "TEMPORARY",
    "WITH",
];

/// The words of a trigger's header, in upper case where they stand there.
const TRIGGER_WORDS: &[&str] = &[
    "AFTER",
    "AS",
    "BEFORE",
    "CASCADE",
    "CONSTRAINT",
    "CREATE",
    "DEFINER",
    "DELETE",
    "EACH",
    "EDITIONABLE",
    "EXECUTE",
    "FOLLOWS",
    "FOR",
    "FORCE",
    "FUNCTION",
    "INSERT",
    "INSTEAD",
    "MODE",
    "NEW",
    "NEW_TABLE",
    "NO",
    "NONEDITIONABLE",
    "OF",
    "OLD",
    "OLD_TABLE",
    "ON",
    "OR",
    "PRECEDES",
    "PROCEDURE",
    "REFERENCING",
    "REPLACE",
    "ROW",
    "STATEMENT",
    "TABLE",
    "TRIGGER",
    "TRUNCATE",
    "UPDATE",
    "WHEN",
];

/// Words that complete a characteristic: `NOT DETERMINISTIC`,
/// `READS SQL DATA`, `SQL SECURITY INVOKER`, `CALLED ON NULL INPUT`, and Db2's
/// `NO EXTERNAL ACTION`, `INHERIT SPECIAL REGISTERS`,
/// `DYNAMIC RESULT SETS 1`, `PARAMETER STYLE SQL`.
const COMPANION_WORDS: &[&str] = &[
    "ACTION",
    "DATA",
    "DEFINER",
    "DETERMINISTIC",
    "EXTERNAL",
    "FENCED",
    "INPUT",
    "INVOKER",
    "LEVEL",
    "NULL",
    "ON",
    "PARALLEL",
    "REGISTERS",
    "RESULT",
    "RETURN",
    "SAVEPOINT",
    "SECURED",
    "SECURITY",
    "SETS",
    "SPECIAL",
    "SQL",
    "STYLE",
    "THREADSAFE",
];

/// Words of a `DECLARE`'s own grammar, in upper case on its first line.
const DECLARE_WORDS: &[&str] = &[
    "CONDITION",
    "CONTINUE",
    "CURSOR",
    "EXIT",
    "FOR",
    "FOUND",
    "HANDLER",
    "SQLEXCEPTION",
    "SQLSTATE",
    "SQLWARNING",
    "UNDO",
    "VALUE",
];

/// Words that can open a MySQL routine's body, which follows the header with
/// nothing to mark where one ends and the other starts.
const MYSQL_BODY: &[&str] = &[
    "ALTER", "BEGIN", "CALL", "CASE", "CLOSE", "COMMIT", "CREATE", "DECLARE", "DELETE", "DO",
    "DROP", "EXECUTE", "FETCH", "FOR", "IF", "INSERT", "ITERATE", "LEAVE", "LOOP", "OPEN",
    "PREPARE", "REPEAT", "REPLACE", "RESIGNAL", "RETURN", "ROLLBACK", "SELECT", "SET", "SIGNAL",
    "START", "TRUNCATE", "UPDATE", "VALUES", "WHILE", "WITH",
];

/// Format the `CREATE` text of a database object for reading.
///
/// ```
/// use rudbman_sql::{Dialect, format_ddl};
///
/// let ddl = "CREATE FUNCTION `f`(a INT) RETURNS int DETERMINISTIC \
///            BEGIN declare x int default 0; if a > 0 then set x = a; end if; return x; END";
/// assert_eq!(
///     format_ddl(ddl, &Dialect::from_id("mariadb")),
///     "CREATE FUNCTION `f`(a INT) RETURNS int
///     DETERMINISTIC
/// BEGIN
///     DECLARE x int DEFAULT 0;
///     IF a > 0 THEN
///         SET x = a;
///     END IF;
///     RETURN x;
/// END",
/// );
/// ```
///
/// [`format_sql`](super::format_sql) lays out queries, and writes anything it
/// does not recognize on one line — which is every `CREATE TABLE` and every
/// routine, the two things a DDL tab shows most. The servers do not help:
/// MySQL quotes a view back on one line, H2 quotes everything on one line, and
/// a routine comes back exactly as whoever created it typed it. This module is
/// the layout for those, on the same terms as the query formatter: it works on
/// the lexer's tokens, has no grammar, and may change only the whitespace
/// between tokens and the case of words it recognizes where they stand. Every
/// string, comment and quoted name is copied byte for byte, so the text stays
/// the statement the server answered with — but for the indentation of a
/// comment's later lines, which move with its `/*` when the comment stands
/// between statements and is indented with them.
///
/// # What each statement becomes
///
/// * **`CREATE TABLE`** — one column or constraint to a line, four spaces in,
///   the closing bracket and the table options on the last line. The shape
///   the reconstructed DDL already has, so that text is left as it is.
/// * **`CREATE VIEW`** — the header to `AS` on one line, and the query under
///   it laid out by [`format_sql`](super::format_sql).
/// * **`CREATE TRIGGER`** — what it fires on and when on the first line, the
///   body under it laid out as a routine's is. A trigger whose action is a
///   call — PostgreSQL's `EXECUTE FUNCTION`, H2's `CALL` — is one line.
/// * **`CREATE FUNCTION` / `PROCEDURE`** — the name on the first line, with
///   its parameter when it has one and one parameter to a line under it when
///   it has more; the return type after the closing bracket; one
///   characteristic (`DETERMINISTIC`, `LANGUAGE`, `COMMENT`...) to a line
///   under them. The body is indented by block — `BEGIN`/`END`,
///   `IF`/`ELSEIF`/`ELSE`, statement `CASE`, the loops, PL/SQL's declaration
///   section and its `EXCEPTION` handlers — and each statement inside it is
///   laid out by the query formatter, so a `SELECT` in a procedure reads as
///   one in the editor does. An empty line the author left between two statements stays, and a
///   comment written on lines of its own moves with its `/*`.
/// * **`CREATE SEQUENCE`** — the name on the first line, one option to a line
///   under it.
/// * **Anything else** — `CREATE INDEX`, `ALTER TABLE`, `COMMENT ON` — on one
///   line, single-spaced.
///
/// Statements are separated by an empty line, except that a run of one-line
/// statements of one kind — the `COMMENT ON`s under a table — stays together.
///
/// # Operators keep their spacing
///
/// A table's options and a routine's header are full of `=` and `@` that
/// belong to their neighbors: `ENGINE=InnoDB`, ``DEFINER=`root`@`%` ``. Outside
/// the statements handed to the query formatter, an operator is therefore
/// spaced as it was written rather than as an expression would be.
///
/// # What is left alone
///
/// * **A body in a string** — PostgreSQL's `AS $$ ... $$`, an H2 alias — is a
///   literal, and literals are copied.
/// * **A SQL Server body** stays as it was written. T-SQL does not end its
///   statements with `;`, and a token-level layout cannot tell where one ends
///   and the next begins without it; only the header is laid out.
///
/// The result has no leading or trailing whitespace.
pub fn format_ddl(source: &str, dialect: &Dialect) -> String {
    let body = source.trim();
    if body.is_empty() {
        return String::new();
    }
    let mut pieces = Vec::new();
    for segment in segments(body, dialect) {
        match segment {
            Segment::Sql(sql) => {
                let formatted = Layout::new(sql, dialect).run();
                if !formatted.is_empty() {
                    pieces.push(formatted);
                }
            }
            Segment::Verbatim(text) => pieces.push(text.trim_end().to_owned()),
        }
    }
    pieces.join("\n")
}

/// How a routine's body is written, as far as laying it out goes.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Family {
    /// MySQL and MariaDB: the body follows the characteristics unmarked.
    MySql,
    /// PL/SQL: `IS` or `AS`, declarations, then the block.
    Oracle,
    /// T-SQL: only the header is laid out.
    MsSql,
    /// SQL PL: a routine's body starts where everything else's does, but a
    /// trigger's follows its `FOR EACH ROW`, and the `AS` before it names a
    /// transition variable.
    Db2,
    /// Everything else: the body starts at `AS`, `IS`, `BEGIN` or `RETURN`.
    Other,
}

impl Family {
    fn of(dialect: &Dialect) -> Self {
        match dialect.id() {
            DialectId::MySql | DialectId::MariaDb => Family::MySql,
            DialectId::Oracle => Family::Oracle,
            DialectId::MsSql => Family::MsSql,
            DialectId::Db2 => Family::Db2,
            _ => Family::Other,
        }
    }
}

/// What a statement is, for choosing its layout.
enum Kind {
    /// `CREATE TABLE`, with the index of the bracket opening its column list.
    Table(usize),
    /// `CREATE VIEW`, with the index of its `AS`.
    View(usize),
    /// `CREATE FUNCTION` or `CREATE PROCEDURE`, with the index of that word.
    Routine(usize),
    /// `CREATE TRIGGER`, with the index of that word.
    Trigger(usize),
    /// `CREATE SEQUENCE`, with the index of that word.
    Sequence(usize),
    /// Anything else.
    Other,
}

/// Lines of a routine, each already indented. One entry may hold several
/// lines when a token or a formatted statement spans them.
type Lines = Vec<String>;

/// One piece of SQL being laid out.
struct Layout<'a> {
    sql: &'a str,
    dialect: &'a Dialect,
    family: Family,
    items: Vec<Item<'a>>,
}

impl<'a> Layout<'a> {
    fn new(sql: &'a str, dialect: &'a Dialect) -> Self {
        Self {
            sql,
            dialect,
            family: Family::of(dialect),
            items: items(sql, dialect),
        }
    }

    // ----------------------------------------------------------- statements

    fn run(&self) -> String {
        let mut out = String::new();
        // The kind of the last statement when it was one line, so that a run of
        // them stays together.
        let mut previous: Option<String> = None;
        let mut i = 0;
        while i < self.items.len() {
            let item = &self.items[i];
            // A comment written after a statement, on its line, stays there.
            if item.kind == TokenKind::Comment && !item.newline && !out.is_empty() {
                out.push(' ');
                out.push_str(&self.moved(i, last_column(&out)));
                i += 1;
                continue;
            }
            let (lead, first) = self.leading_comments(i);
            let (text, end) = if first >= self.items.len() {
                (String::new(), first)
            } else {
                match self.kind(first) {
                    Kind::Table(open) => self.table(first, open),
                    Kind::View(as_) => self.view(first, as_),
                    Kind::Routine(word) => self.routine(first, word),
                    Kind::Trigger(word) => self.trigger(first, word),
                    Kind::Sequence(word) => self.sequence(first, word),
                    Kind::Other => {
                        let end = self.statement_end(first);
                        (self.phrase(first..end, &[]), end)
                    }
                }
            };
            let text = [lead, text]
                .into_iter()
                .filter(|part| !part.is_empty())
                .collect::<Vec<_>>()
                .join("\n");
            let group = (!text.contains('\n')).then(|| self.group(first));
            if !out.is_empty() {
                let together = group.is_some() && group == previous;
                out.push_str(if together { "\n" } else { "\n\n" });
            }
            out.push_str(&text);
            previous = group;
            i = end.max(i + 1);
        }
        out
    }

    /// The comments before a statement, one to a line, and where it starts.
    fn leading_comments(&self, mut i: usize) -> (String, usize) {
        let mut lines = Vec::new();
        while i < self.items.len() && self.items[i].kind == TokenKind::Comment {
            lines.push(self.moved(i, 0));
            i += 1;
        }
        (lines.join("\n"), i)
    }

    /// Two words naming what a statement does — `CREATE INDEX`, `COMMENT ON` —
    /// with `UNIQUE` passed over, so that the indexes of a table group.
    fn group(&self, i: usize) -> String {
        let mut words = Vec::new();
        let mut k = i;
        while k < self.items.len() && words.len() < 2 {
            if self.is_bare(k) && !self.is_word(k, "UNIQUE") {
                words.push(self.items[k].text.to_ascii_uppercase());
            }
            k += 1;
        }
        words.join(" ")
    }

    fn kind(&self, i: usize) -> Kind {
        if !self.is_word(i, "CREATE") {
            return Kind::Other;
        }
        let mut k = i + 1;
        while k < self.items.len() {
            let item = &self.items[k];
            if item.is_punct("(") || item.is_punct(";") {
                return Kind::Other;
            }
            if self.is_word(k, "TABLE") {
                // `CREATE TABLE ... AS SELECT` has no column list to lay out.
                return self
                    .find(k + 1, |item| item.is_punct("("), &["AS"])
                    .filter(|&open| self.items[open].is_punct("("))
                    .map_or(Kind::Other, Kind::Table);
            }
            if self.is_word(k, "VIEW") {
                return self.find_word(k + 1, "AS").map_or(Kind::Other, Kind::View);
            }
            if self.is_word(k, "FUNCTION") || self.is_word(k, "PROCEDURE") {
                return Kind::Routine(k);
            }
            if self.is_word(k, "TRIGGER") {
                return Kind::Trigger(k);
            }
            if self.is_word(k, "SEQUENCE") {
                return Kind::Sequence(k);
            }
            if self.is_word(k, "AS") {
                return Kind::Other;
            }
            k += 1;
        }
        Kind::Other
    }

    /// One past the `;` that ends the statement starting at `i`, or the end.
    fn statement_end(&self, i: usize) -> usize {
        self.find(i, |item| item.is_punct(";"), &[])
            .map_or(self.items.len(), |semi| semi + 1)
    }

    fn table(&self, i: usize, open: usize) -> (String, usize) {
        let close = self.matching(open);
        let end = self.statement_end(close);
        let mut out = self.phrase(i..open, &[]);
        let elements = self.split(open + 1, close);
        if elements.is_empty() {
            out.push_str(" ()");
        } else {
            out.push_str(" (\n");
            out.push_str(&self.element_lines(&elements, 1).join("\n"));
            out.push_str("\n)");
        }
        if close + 1 < end {
            let partition = (close + 1..end)
                .find(|&k| self.is_word(k, "PARTITION") && self.is_word(k + 1, "BY"));
            let tail = self.phrase(close + 1..partition.unwrap_or(end), &[]);
            if !tail.is_empty() && !tail.starts_with(';') {
                out.push(' ');
            }
            out.push_str(&tail);
            if let Some(partition) = partition {
                out.push('\n');
                out.push_str(&self.partitions(partition, end));
            }
        }
        (out, end)
    }

    /// `PARTITION BY ...` on a line of its own, and the partitions it lists one
    /// to a line, as a table's columns are.
    fn partitions(&self, start: usize, end: usize) -> String {
        let list =
            (start..end).find(|&k| self.items[k].is_punct("(") && self.is_word(k + 1, "PARTITION"));
        let Some(open) = list else {
            return self.phrase(start..end, &[]);
        };
        let close = self.matching(open);
        let mut out = self.phrase(start..open, &[]);
        out.push_str(" (\n");
        out.push_str(
            &self
                .element_lines(&self.split(open + 1, close), 1)
                .join("\n"),
        );
        out.push_str("\n)");
        out.push_str(&self.phrase(close + 1..end, &[]));
        out
    }

    fn view(&self, i: usize, as_: usize) -> (String, usize) {
        let end = self.statement_end(as_);
        let forced = self.words_in(i..as_ + 1, VIEW_WORDS);
        let header = self.phrase(i..as_ + 1, &forced);
        if as_ + 1 >= end {
            return (header, end);
        }
        let query = format_segment(self.source(as_ + 1, end), self.dialect);
        (format!("{header}\n{query}"), end)
    }

    /// A sequence: its name on the first line and one option to a line under
    /// it, the options in upper case — MariaDB quotes them back in lower case.
    fn sequence(&self, i: usize, word: usize) -> (String, usize) {
        let end = self.statement_end(word);
        // The name is not an option, whatever it is called: one part, or
        // several joined by dots, after any `IF NOT EXISTS`.
        let mut name = word + 1;
        if self.is_word(name, "IF") {
            name += if self.is_word(name + 1, "NOT") { 3 } else { 2 };
        }
        let mut after = name + 1;
        while self.items.get(after).is_some_and(|item| item.is_punct(".")) {
            after += 2;
        }
        let after = after.min(end);
        let mut forced = self.words_in(i..name.min(end), SEQUENCE_WORDS);
        forced.extend(self.words_in(after..end, SEQUENCE_WORDS));

        let mut lines = vec![self.phrase(i..after, &forced)];
        let mut k = after;
        while k < end {
            let mut next = k + 1;
            while next < end && !self.starts_sequence_option(next) {
                next += 1;
            }
            let option = self.phrase(k..next, &forced);
            match lines.last_mut() {
                // A lone `;` closes the line before it.
                Some(last) if option == ";" => last.push(';'),
                _ => lines.push(format!("{INDENT}{option}")),
            }
            k = next;
        }
        (lines.join("\n"), end)
    }

    /// Whether the token at `k` opens one of a sequence's options. `NO` opens
    /// `NO CYCLE` and its kin, so the word after it does not open another.
    fn starts_sequence_option(&self, k: usize) -> bool {
        if self.items[k].is_punct(";") {
            return true;
        }
        SEQUENCE_OPTIONS.iter().any(|w| self.is_word(k, w))
            && !self.is_word(k.wrapping_sub(1), "NO")
            && !self.next_is_punct(k, ".")
            && !(k > 0 && self.items[k - 1].is_punct("."))
    }

    /// The bare words in `range` that are in `words`, and not part of a dotted
    /// name.
    fn words_in(&self, range: std::ops::Range<usize>, words: &[&str]) -> Vec<usize> {
        range
            .filter(|&k| {
                words.iter().any(|w| self.is_word(k, w))
                    && !self.next_is_punct(k, ".")
                    && !(k > 0 && self.items[k - 1].is_punct("."))
            })
            .collect()
    }

    // -------------------------------------------------------------- routine

    fn routine(&self, i: usize, word: usize) -> (String, usize) {
        let mut lines = Lines::new();
        let params = self.parameters(word);
        let after = params.map_or(word + 1, |(_, close)| close + 1);
        let body = self.body_start(after);
        let header_end = body.unwrap_or_else(|| self.statement_end(after));
        self.header(i, word, params, header_end, &mut lines);
        let end = match body {
            None => header_end,
            Some(body) => self.body(body, &mut lines),
        };
        (lines.join("\n"), end)
    }

    // -------------------------------------------------------------- trigger

    /// A trigger: its header on one line, its body laid out as a routine's.
    fn trigger(&self, i: usize, word: usize) -> (String, usize) {
        let body = self.trigger_body(word + 1);
        let header_end = body.unwrap_or_else(|| self.statement_end(word));
        // The header's own words are keywords where they stand; a name after a
        // dot (`NEW.x`, in a `WHEN`) is not one of them.
        let forced: Vec<usize> = (i..header_end)
            .filter(|&k| {
                TRIGGER_WORDS.iter().any(|w| self.is_word(k, w))
                    && !self.next_is_punct(k, ".")
                    && !(k > 0 && self.items[k - 1].is_punct("."))
            })
            .collect();
        let mut lines = vec![self.phrase(i..header_end, &forced)];
        let end = match body {
            None => header_end,
            Some(body) => self.body(body, &mut lines),
        };
        (lines.join("\n"), end)
    }

    /// Where a trigger's body starts, searching from just after `TRIGGER`; or
    /// `None` for a trigger whose action is part of its header.
    fn trigger_body(&self, from: usize) -> Option<usize> {
        let mut depth = 0usize;
        for k in from..self.items.len() {
            let item = &self.items[k];
            if item.is_punct("(") {
                depth += 1;
                continue;
            }
            if item.is_punct(")") {
                depth = depth.saturating_sub(1);
                continue;
            }
            if depth > 0 {
                continue;
            }
            if item.is_punct(";") {
                return None;
            }
            let starts = match self.family {
                // `FOR EACH ROW [FOLLOWS | PRECEDES other]`, then the statement.
                Family::MySql => {
                    if self.is_word(k, "FOR")
                        && self.is_word(k + 1, "EACH")
                        && self.is_word(k + 2, "ROW")
                    {
                        let mut body = k + 3;
                        if self.is_word(body, "FOLLOWS") || self.is_word(body, "PRECEDES") {
                            body += 2;
                        }
                        return (body < self.items.len()).then_some(body);
                    }
                    false
                }
                Family::Oracle => self.is_word(k, "DECLARE") || self.is_word(k, "BEGIN"),
                Family::MsSql => {
                    self.is_word(k, "AS") && !self.is_word(k.wrapping_sub(1), "EXECUTE")
                }
                // `FOR EACH ROW` or `STATEMENT`, the old `MODE DB2SQL`, a
                // `WHEN (...)`, then one statement or a `BEGIN ATOMIC` block.
                // `REFERENCING NEW AS n` is passed over on the way: its `AS`
                // names a row, and starts nothing.
                Family::Db2 => {
                    if self.is_word(k, "FOR")
                        && self.is_word(k + 1, "EACH")
                        && (self.is_word(k + 2, "ROW") || self.is_word(k + 2, "STATEMENT"))
                    {
                        let mut body = k + 3;
                        if self.is_word(body, "MODE") {
                            body += 2;
                        }
                        if self.is_word(body, "WHEN")
                            && self
                                .items
                                .get(body + 1)
                                .is_some_and(|item| item.is_punct("("))
                        {
                            body = self.matching(body + 1) + 1;
                        }
                        return (body < self.items.len()).then_some(body);
                    }
                    false
                }
                Family::Other => self.is_word(k, "BEGIN") || self.is_word(k, "AS"),
            };
            if starts {
                return Some(k);
            }
        }
        None
    }

    /// The bracket around a routine's parameters, when its name is followed by
    /// one. T-SQL writes its parameters without.
    fn parameters(&self, word: usize) -> Option<(usize, usize)> {
        let mut k = word + 1;
        while k < self.items.len() {
            let item = &self.items[k];
            let named = matches!(
                item.kind,
                TokenKind::Identifier
                    | TokenKind::QuotedIdentifier
                    | TokenKind::Keyword
                    | TokenKind::Function
                    | TokenKind::Type
            ) || item.is_punct(".");
            if !named {
                break;
            }
            k += 1;
        }
        (k > word + 1 && self.items.get(k).is_some_and(|item| item.is_punct("(")))
            .then(|| (k, self.matching(k)))
    }

    /// Where a routine's body starts, searching from just after its parameters.
    fn body_start(&self, from: usize) -> Option<usize> {
        let mut depth = 0usize;
        for k in from..self.items.len() {
            let item = &self.items[k];
            if item.is_punct("(") {
                depth += 1;
                continue;
            }
            if item.is_punct(")") {
                depth = depth.saturating_sub(1);
                continue;
            }
            if depth > 0 || !self.is_bare(k) {
                if depth == 0 && item.is_punct(";") {
                    return None;
                }
                continue;
            }
            let upper = item.text.to_ascii_uppercase();
            let starts = match self.family {
                Family::MySql => {
                    self.is_label(k)
                        || (MYSQL_BODY.contains(&upper.as_str())
                            && !(upper == "SET"
                                && (self.is_word(k.wrapping_sub(1), "CHARACTER")
                                    || self.next_is_punct(k, "("))))
                }
                Family::Oracle => matches!(upper.as_str(), "IS" | "AS" | "BEGIN" | "DECLARE"),
                Family::MsSql => upper == "AS" && !self.is_word(k.wrapping_sub(1), "EXECUTE"),
                // `COMMIT ON RETURN NO` is a procedure's option, not its body.
                Family::Db2 => match upper.as_str() {
                    "AS" | "IS" | "BEGIN" => true,
                    "RETURN" => !self.is_word(k.wrapping_sub(1), "ON"),
                    _ => false,
                },
                Family::Other => matches!(upper.as_str(), "AS" | "IS" | "BEGIN" | "RETURN"),
            };
            if starts {
                return Some(k);
            }
        }
        None
    }

    /// The header: the name with its parameter on the first line, or with the
    /// bracket of several and one of them to a line under it; the return type
    /// after them, and one characteristic to a line after that.
    ///
    /// The words that make it a header — `CREATE ... PROCEDURE`, `RETURNS`,
    /// each characteristic's own — are keywords where they stand and are
    /// written in upper case, as the body's structural words are.
    fn header(
        &self,
        i: usize,
        word: usize,
        params: Option<(usize, usize)>,
        end: usize,
        lines: &mut Lines,
    ) {
        let after = params.map_or(word + 1, |(_, close)| close + 1);
        let mut forced: Vec<usize> = (i..=word)
            .filter(|&k| LEAD_WORDS.iter().any(|lead| self.is_word(k, lead)))
            .collect();
        // The return type stays beside the parameters it answers.
        let mut first_end = after;
        if first_end < end && self.is_word(first_end, "RETURNS") {
            forced.push(first_end);
            first_end += 1;
        }
        first_end = self.next_characteristic(first_end, end);
        if self.family == Family::Oracle {
            forced.extend((after..first_end).find(|&k| self.is_word(k, "RETURN")));
        }

        let elements = params.map(|(open, close)| self.split(open + 1, close));
        match (params, elements) {
            (Some((open, close)), Some(elements)) if elements.len() > 1 => {
                lines.push(format!("{}(", self.phrase(i..open, &forced)));
                lines.extend(self.element_lines(&elements, 1));
                let rest = self.phrase(close + 1..first_end, &forced);
                lines.push(if rest.is_empty() {
                    ")".to_owned()
                } else {
                    format!(") {rest}")
                });
            }
            _ => lines.push(self.phrase(i..first_end, &forced)),
        }

        let mut k = first_end;
        while k < end {
            let next = self.next_characteristic(k + 1, end);
            // `LANGUAGE sql` names a language, which keeps the case it came in
            // like `LANGUAGE plpgsql` beside it.
            let named = self.is_word(k, "LANGUAGE");
            let forced: Vec<usize> = std::iter::once(k)
                .chain((k + 1..next).filter(|&c| {
                    !named
                        && COMPANION_WORDS
                            .iter()
                            .any(|companion| self.is_word(c, companion))
                }))
                .collect();
            lines.push(format!("{INDENT}{}", self.phrase(k..next, &forced)));
            k = next;
        }
    }

    /// The first characteristic at or after `from`, or `end`.
    fn next_characteristic(&self, from: usize, end: usize) -> usize {
        let mut depth = 0usize;
        for k in from..end {
            let item = &self.items[k];
            if item.is_punct("(") {
                depth += 1;
            } else if item.is_punct(")") {
                depth = depth.saturating_sub(1);
            } else if depth == 0 && self.is_characteristic(k) {
                return k;
            }
        }
        end
    }

    /// Whether the word at `k` opens one of a routine's characteristics.
    ///
    /// The union of what the products write between the parameters and the
    /// body. A word that only means that inside a phrase — `SQL` of
    /// `LANGUAGE SQL`, `NO` of anything but `NO SQL` — is told apart by its
    /// neighbor.
    fn is_characteristic(&self, k: usize) -> bool {
        if !self.is_bare(k) {
            return false;
        }
        let previous = |word| self.is_word(k.wrapping_sub(1), word);
        let next = |word| self.is_word(k + 1, word);
        match self.items[k].text.to_ascii_uppercase().as_str() {
            "AUTHID" | "AUTONOMOUS" | "CALLED" | "COMMENT" | "CONTAINS" | "COST" | "DYNAMIC"
            | "IMMUTABLE" | "INHERIT" | "LANGUAGE" | "LEAKPROOF" | "MODIFIES"
            | "PARALLEL_ENABLE" | "PIPELINED" | "READS" | "RESULT_CACHE" | "RETURNS" | "ROWS"
            | "SPECIFIC" | "STABLE" | "STRICT" | "SUPPORT" | "TRANSFORM" | "VOLATILE"
            | "WINDOW" | "WITH" => true,
            // Db2 spells several characteristics as a phrase whose first
            // word means nothing on its own: `NO EXTERNAL ACTION`,
            // `ALLOW PARALLEL`, `PARAMETER STYLE SQL`, `NOT SECURED`,
            // `OLD SAVEPOINT LEVEL`, `COMMIT ON RETURN NO`.
            "DETERMINISTIC" | "FENCED" | "SECURED" | "THREADSAFE" => !previous("NOT"),
            "NOT" => {
                next("DETERMINISTIC") || next("FENCED") || next("SECURED") || next("THREADSAFE")
            }
            "NO" => next("SQL") || next("EXTERNAL"),
            "EXTERNAL" => !previous("NO"),
            "PARALLEL" => !previous("ALLOW") && !previous("DISALLOW"),
            "ALLOW" | "DISALLOW" => next("PARALLEL"),
            "PARAMETER" => next("STYLE"),
            "OLD" | "NEW" => next("SAVEPOINT"),
            "COMMIT" => next("ON"),
            "SQL" => next("SECURITY"),
            "SECURITY" => !previous("SQL") && !previous("EXTERNAL"),
            "SET" => self.family != Family::MySql,
            _ => false,
        }
    }

    /// Lays out a routine's body from `start`; answers one past its end.
    fn body(&self, start: usize, lines: &mut Lines) -> usize {
        if self.family == Family::MsSql {
            lines.push(self.sql[self.items[start].start..].trim_end().to_owned());
            return self.items.len();
        }
        let is_section = self.is_word(start, "AS") || self.is_word(start, "IS");
        if is_section
            && self
                .items
                .get(start + 1)
                .is_some_and(|item| item.kind == TokenKind::String)
        {
            // A body in a string — PostgreSQL's dollar quotes, an external
            // routine's link name — and whatever options follow it.
            let end = self.statement_end(start);
            let mut k = start;
            let mut level = 0;
            while k < end {
                let next = self.next_characteristic(k + 1, end);
                lines.push(format!(
                    "{}{}",
                    INDENT.repeat(level),
                    self.phrase(k..next, &[start])
                ));
                level = 1;
                k = next;
            }
            return end;
        }
        if is_section {
            return self.section(start, 0, lines);
        }
        self.statement(start, 0, lines)
    }

    /// PL/SQL's declaration section, from its `IS`, `AS` or `DECLARE` to the
    /// end of the block it declares for.
    fn section(&self, start: usize, level: usize, lines: &mut Lines) -> usize {
        lines.push(self.line(level, &self.items[start].text.to_ascii_uppercase()));
        let mut k = start + 1;
        let mut first = true;
        loop {
            k = self.comments(k, level + 1, lines);
            if k >= self.items.len() || self.is_word(k, "CREATE") {
                return k;
            }
            if self.is_word(k, "BEGIN") {
                return self.block(k, k, level, lines);
            }
            self.gap(k, &mut first, lines);
            k = self.statement(k, level + 1, lines);
        }
    }

    /// One statement of a body, at `level`; answers one past its end.
    fn statement(&self, i: usize, level: usize, lines: &mut Lines) -> usize {
        let k = if self.is_label(i) { i + 2 } else { i };
        if k >= self.items.len() {
            return self.simple(i, level, lines);
        }
        let upper = self.items[k].text.to_ascii_uppercase();
        if !self.is_bare(k) {
            return self.simple(i, level, lines);
        }
        match upper.as_str() {
            "BEGIN" if !self.is_transaction(k) => self.block(i, k, level, lines),
            "IF" if k == i => self.if_block(i, level, lines),
            "CASE" if k == i => self.case_block(i, level, lines),
            "LOOP" | "REPEAT" => self.loop_block(i, k, k + 1, level, lines),
            "WHILE" | "FOR" => match self.find(k + 1, |_| false, &["DO", "LOOP"]) {
                Some(open) => self.loop_block(i, k, open + 1, level, lines),
                None => self.simple(i, level, lines),
            },
            "DECLARE" if self.family == Family::Oracle => self.section(i, level, lines),
            _ => self.simple(i, level, lines),
        }
    }

    /// A statement with no block of its own, laid out by the query formatter.
    ///
    /// It ends at its `;`, or before an `END` that closes the block around it
    /// — the last statement before an `END` may go without one. A `BEGIN`
    /// part-way through makes it the lead of a block instead: a handler's
    /// `DECLARE ... HANDLER FOR SQLEXCEPTION BEGIN`.
    fn simple(&self, i: usize, level: usize, lines: &mut Lines) -> usize {
        if i >= self.items.len() {
            return i;
        }
        let mut depth = 0usize;
        let mut cases = 0usize;
        let mut j = i;
        while j < self.items.len() {
            let item = &self.items[j];
            if item.is_punct("(") {
                depth += 1;
            } else if item.is_punct(")") {
                depth = depth.saturating_sub(1);
            } else if depth == 0 && item.is_punct(";") {
                j += 1;
                break;
            } else if depth == 0 && self.is_bare(j) {
                if self.is_word(j, "CASE") {
                    cases += 1;
                } else if self.is_word(j, "END") && j > i {
                    if cases == 0 {
                        break;
                    }
                    cases -= 1;
                } else if self.is_word(j, "BEGIN") && j > i && !self.is_transaction(j) {
                    return self.block(i, j, level, lines);
                }
            }
            j += 1;
        }
        let mut text = upper_first_word(&format_segment(self.source(i, j), self.dialect));
        if self.is_word(i, "DECLARE") {
            text = self.upper_words(&text, DECLARE_WORDS);
        }
        lines.push(self.line(level, &text));
        j
    }

    /// `text` with the bare words of its first line that are in `words` in
    /// upper case: a `DECLARE`'s `CURSOR FOR`, `CONTINUE HANDLER FOR NOT FOUND`.
    /// Only the first line, because what follows a cursor's `FOR` is a query
    /// with names of its own in it.
    fn upper_words(&self, text: &str, words: &[&str]) -> String {
        let mut out = String::with_capacity(text.len());
        let mut first_line = true;
        for token in lex(text, self.dialect) {
            let piece = token.text(text);
            let bare = matches!(
                token.kind,
                TokenKind::Keyword | TokenKind::Identifier | TokenKind::Type
            );
            if first_line && bare && words.iter().any(|word| word.eq_ignore_ascii_case(piece)) {
                out.push_str(&piece.to_ascii_uppercase());
            } else {
                out.push_str(piece);
            }
            first_line &= !(token.kind == TokenKind::Whitespace && piece.contains('\n'));
        }
        out
    }

    /// `BEGIN` ... `END`, `lead` being where the line holding `BEGIN` starts.
    fn block(&self, lead: usize, begin: usize, level: usize, lines: &mut Lines) -> usize {
        let mut k = begin + 1;
        if self.is_word(k, "NOT") && self.is_word(k + 1, "ATOMIC") {
            k += 2;
        } else if self.is_word(k, "ATOMIC") {
            k += 1;
        }
        let forced: Vec<usize> = (begin..k).collect();
        let opening = upper_first_word(&self.phrase(lead..k, &forced));
        lines.push(self.line(level, &opening));
        let mut handlers = false;
        let mut first = true;
        loop {
            let inner = level + if handlers { 2 } else { 1 };
            k = self.comments(k, inner, lines);
            if k >= self.items.len() {
                return k;
            }
            if self.is_word(k, "END") {
                return self.end_line(k, level, lines);
            }
            if self.is_word(k, "EXCEPTION") {
                lines.push(self.line(level, "EXCEPTION"));
                handlers = true;
                first = true;
                k += 1;
                continue;
            }
            if handlers
                && self.is_word(k, "WHEN")
                && let Some(then) = self.find_word(k + 1, "THEN")
            {
                lines.push(self.line(level + 1, &self.phrase(k..then + 1, &[k, then])));
                first = true;
                k = then + 1;
                continue;
            }
            self.gap(k, &mut first, lines);
            k = self.statement(k, inner, lines);
        }
    }

    fn if_block(&self, i: usize, level: usize, lines: &mut Lines) -> usize {
        let Some(then) = self.find_word(i + 1, "THEN") else {
            return self.simple(i, level, lines);
        };
        lines.push(self.line(level, &self.phrase(i..then + 1, &[i, then])));
        let mut k = then + 1;
        let mut first = true;
        loop {
            k = self.comments(k, level + 1, lines);
            if k >= self.items.len() {
                return k;
            }
            if self.is_word(k, "ELSEIF") || self.is_word(k, "ELSIF") {
                let Some(then) = self.find_word(k + 1, "THEN") else {
                    return self.simple(k, level, lines);
                };
                lines.push(self.line(level, &self.phrase(k..then + 1, &[k, then])));
                first = true;
                k = then + 1;
            } else if self.is_word(k, "ELSE") {
                lines.push(self.line(level, "ELSE"));
                first = true;
                k += 1;
            } else if self.is_word(k, "END") {
                return self.end_line(k, level, lines);
            } else {
                self.gap(k, &mut first, lines);
                k = self.statement(k, level + 1, lines);
            }
        }
    }

    /// A `CASE` statement: its `WHEN`s one level in, their statements two.
    fn case_block(&self, i: usize, level: usize, lines: &mut Lines) -> usize {
        let Some(when) = self.find_word(i + 1, "WHEN") else {
            return self.simple(i, level, lines);
        };
        lines.push(self.line(level, &self.phrase(i..when, &[i])));
        let mut k = when;
        let mut first = true;
        loop {
            k = self.comments(k, level + 2, lines);
            if k >= self.items.len() {
                return k;
            }
            if self.is_word(k, "WHEN") {
                let Some(then) = self.find_word(k + 1, "THEN") else {
                    return self.simple(k, level + 1, lines);
                };
                lines.push(self.line(level + 1, &self.phrase(k..then + 1, &[k, then])));
                first = true;
                k = then + 1;
            } else if self.is_word(k, "ELSE") {
                lines.push(self.line(level + 1, "ELSE"));
                first = true;
                k += 1;
            } else if self.is_word(k, "END") {
                return self.end_line(k, level, lines);
            } else {
                self.gap(k, &mut first, lines);
                k = self.statement(k, level + 2, lines);
            }
        }
    }

    /// `LOOP`, `WHILE ... DO`, `FOR ... LOOP`, `REPEAT ... UNTIL`; `word` is
    /// the loop's own keyword and `open` one past the end of its header.
    fn loop_block(
        &self,
        i: usize,
        word: usize,
        open: usize,
        level: usize,
        lines: &mut Lines,
    ) -> usize {
        lines.push(self.line(level, &self.phrase(i..open, &[word, open - 1])));
        let mut k = open;
        let mut first = true;
        loop {
            k = self.comments(k, level + 1, lines);
            if k >= self.items.len() {
                return k;
            }
            if self.is_word(k, "END") {
                return self.end_line(k, level, lines);
            }
            if self.is_word(k, "UNTIL") {
                let end = self.find_word(k + 1, "END").unwrap_or(self.items.len());
                lines.push(self.line(level, &self.phrase(k..end, &[k])));
                k = end;
                continue;
            }
            self.gap(k, &mut first, lines);
            k = self.statement(k, level + 1, lines);
        }
    }

    /// `END`, the word naming what it closes, the block's label, and its `;`.
    fn end_line(&self, end: usize, level: usize, lines: &mut Lines) -> usize {
        let mut k = end + 1;
        let mut forced = vec![end];
        let names = ["IF", "LOOP", "WHILE", "REPEAT", "CASE", "FOR"];
        if names.iter().any(|word| self.is_word(k, word)) {
            forced.push(k);
            k += 1;
        }
        // A label only when the `;` follows it: a body whose `END` has none
        // is followed by whatever comes next, which is not part of it.
        if self.items.get(k).is_some_and(|item| {
            matches!(
                item.kind,
                TokenKind::Identifier | TokenKind::QuotedIdentifier
            )
        }) && self.next_is_punct(k, ";")
        {
            k += 1;
        }
        if self.items.get(k).is_some_and(|item| item.is_punct(";")) {
            k += 1;
        }
        lines.push(self.line(level, &self.phrase(end..k, &forced)));
        k
    }

    /// Writes the comments starting at `k`; answers the first token after them.
    ///
    /// One written after a statement on its line is appended to that line.
    fn comments(&self, mut k: usize, level: usize, lines: &mut Lines) -> usize {
        while k < self.items.len() && self.items[k].kind == TokenKind::Comment {
            let item = &self.items[k];
            match lines.last_mut() {
                Some(last) if !item.newline => {
                    last.push(' ');
                    let moved = self.moved(k, last_column(last));
                    last.push_str(&moved);
                }
                _ => {
                    if item.blank && lines.last().is_some_and(|last| !last.is_empty()) {
                        lines.push(String::new());
                    }
                    let moved = self.moved(k, level * INDENT.len());
                    lines.push(self.line(level, &moved));
                }
            }
            k += 1;
        }
        k
    }

    /// Keeps an empty line the author left before a statement, other than the
    /// first of its block.
    fn gap(&self, k: usize, first: &mut bool, lines: &mut Lines) {
        if !*first && self.items[k].blank && lines.last().is_some_and(|last| !last.is_empty()) {
            lines.push(String::new());
        }
        *first = false;
    }

    // ---------------------------------------------------------------- words

    /// A word as written: a keyword, a name or a type, not quoted.
    fn is_bare(&self, k: usize) -> bool {
        self.items.get(k).is_some_and(|item| {
            matches!(
                item.kind,
                TokenKind::Keyword | TokenKind::Identifier | TokenKind::Type | TokenKind::Function
            )
        })
    }

    fn is_word(&self, k: usize, word: &str) -> bool {
        self.is_bare(k) && self.items[k].is(word)
    }

    fn next_is_punct(&self, k: usize, punct: &str) -> bool {
        self.items
            .get(k + 1)
            .is_some_and(|item| item.is_punct(punct))
    }

    /// A block's label: a name and a colon, before the block's keyword.
    fn is_label(&self, k: usize) -> bool {
        self.is_bare(k)
            && self.next_is_punct(k, ":")
            && ["BEGIN", "LOOP", "REPEAT", "WHILE", "FOR"]
                .iter()
                .any(|word| self.is_word(k + 2, word))
    }

    /// `BEGIN` starting a transaction rather than a block.
    fn is_transaction(&self, k: usize) -> bool {
        ["TRAN", "TRANSACTION", "WORK", "DISTRIBUTED"]
            .iter()
            .any(|word| self.is_word(k + 1, word))
            || self.next_is_punct(k, ";")
    }

    /// The first token from `from` at bracket depth zero that `hit` accepts or
    /// that is one of `words`, before the statement's `;`.
    fn find(&self, from: usize, hit: impl Fn(&Item<'_>) -> bool, words: &[&str]) -> Option<usize> {
        let mut depth = 0usize;
        for k in from..self.items.len() {
            let item = &self.items[k];
            if depth == 0 && (hit(item) || words.iter().any(|word| self.is_word(k, word))) {
                return Some(k);
            }
            if item.is_punct("(") {
                depth += 1;
            } else if item.is_punct(")") {
                depth = depth.saturating_sub(1);
            } else if depth == 0 && item.is_punct(";") {
                return None;
            }
        }
        None
    }

    fn find_word(&self, from: usize, word: &str) -> Option<usize> {
        self.find(from, |_| false, &[word])
    }

    /// The bracket closing the one at `open`, or the last token.
    fn matching(&self, open: usize) -> usize {
        let mut depth = 0usize;
        for k in open..self.items.len() {
            if self.items[k].is_punct("(") {
                depth += 1;
            } else if self.items[k].is_punct(")") {
                depth -= 1;
                if depth == 0 {
                    return k;
                }
            }
        }
        self.items.len().saturating_sub(1)
    }

    /// The comma-separated parts between `from` and `to`, at their own depth.
    fn split(&self, from: usize, to: usize) -> Vec<(usize, usize)> {
        let mut parts = Vec::new();
        let mut depth = 0usize;
        let mut start = from;
        for k in from..to {
            let item = &self.items[k];
            if item.is_punct("(") {
                depth += 1;
            } else if item.is_punct(")") {
                depth = depth.saturating_sub(1);
            } else if depth == 0 && item.is_punct(",") {
                parts.push((start, k));
                start = k + 1;
            }
        }
        if start < to {
            parts.push((start, to));
        }
        parts
    }

    /// One line per element at `level`, each but the last with its comma.
    ///
    /// A comment written after the comma stays on the element's line, and one
    /// written before it goes after it: a comma after a line comment would be
    /// part of the comment.
    fn element_lines(&self, elements: &[(usize, usize)], level: usize) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        for (n, &(mut start, end)) in elements.iter().enumerate() {
            if let Some(last) = out.last_mut() {
                while start < end
                    && self.items[start].kind == TokenKind::Comment
                    && !self.items[start].newline
                {
                    last.push(' ');
                    last.push_str(self.items[start].text);
                    start += 1;
                }
            }
            let mut body_end = end;
            while body_end > start && self.items[body_end - 1].kind == TokenKind::Comment {
                body_end -= 1;
            }
            let mut line = self.line(level, &self.phrase(start..body_end, &[]));
            if n + 1 < elements.len() {
                line.push(',');
            }
            for comment in &self.items[body_end..end] {
                line.push(' ');
                line.push_str(comment.text);
            }
            out.push(line);
        }
        out
    }

    // ----------------------------------------------------------------- text

    /// The comment at `k` written from column `to`, its later lines moved with
    /// it from where it stood in the input.
    fn moved(&self, k: usize, to: usize) -> String {
        let item = &self.items[k];
        let line = self.sql[..item.start].rfind('\n').map_or(0, |n| n + 1);
        let from = self.sql[line..item.start].chars().count();
        move_comment(item.text, from, to)
    }

    /// The input from token `from` to the end of token `to - 1`.
    fn source(&self, from: usize, to: usize) -> &'a str {
        let last = &self.items[to - 1];
        &self.sql[self.items[from].start..last.start + last.text.len()]
    }

    /// The tokens in `range` on one line, single-spaced, in the case
    /// [`format_sql`](super::format_sql) would write them and with the words at
    /// `forced` in upper case. A line comment still ends its line.
    fn phrase(&self, range: std::ops::Range<usize>, forced: &[usize]) -> String {
        let mut out = String::new();
        let mut prev: Option<usize> = None;
        let mut before: Option<usize> = None;
        for k in range {
            let item = &self.items[k];
            let fresh = out.is_empty() || out.ends_with('\n');
            if item.kind == TokenKind::Comment {
                // A comment written on a line of its own keeps one.
                if !fresh {
                    out.push(if item.newline { '\n' } else { ' ' });
                }
                out.push_str(item.text);
                if item.is_line_comment() {
                    out.push('\n');
                }
                continue;
            }
            let space = prev.is_some_and(|p| {
                let p = &self.items[p];
                let kept = |item: &Item<'_>| item.kind == TokenKind::Operator || item.is_punct(":");
                if kept(p) || kept(item) {
                    item.spaced
                } else {
                    needs_space(Some(p), before.map(|b| &self.items[b]), item)
                }
            });
            if space && !fresh {
                out.push(' ');
            }
            let dotted = prev.is_some_and(|p| self.items[p].is_punct("."))
                || self.items.get(k + 1).is_some_and(|next| next.is_punct("."));
            if forced.contains(&k)
                || (item.kind == TokenKind::Keyword
                    && !dotted
                    && UPPER
                        .binary_search_by(|w| cmp_ignore_case(w, item.text))
                        .is_ok())
            {
                out.push_str(&item.text.to_ascii_uppercase());
            } else {
                out.push_str(item.text);
            }
            before = prev;
            prev = Some(k);
        }
        out.trim_end().to_owned()
    }

    /// `text` indented to `level`, every line of it.
    ///
    /// Only the line breaks *between* tokens are indented — re-lexing finds
    /// them — so a string or a comment that spans lines keeps its own.
    fn line(&self, level: usize, text: &str) -> String {
        let pad = INDENT.repeat(level);
        let mut out = String::with_capacity(text.len() + pad.len());
        if level > 0 && !text.is_empty() {
            out.push_str(&pad);
        }
        for token in lex(text, self.dialect) {
            let piece = token.text(text);
            if token.kind != TokenKind::Whitespace || level == 0 {
                out.push_str(piece);
                continue;
            }
            let mut parts = piece.split('\n');
            out.push_str(parts.next().unwrap_or_default());
            for part in parts {
                out.push('\n');
                out.push_str(&pad);
                out.push_str(part);
            }
        }
        out
    }
}

/// The column the next character after `text` lands in.
fn last_column(text: &str) -> usize {
    text.rsplit('\n').next().unwrap_or_default().chars().count()
}

/// A block comment whose `/*` moves from column `from` to column `to`, with
/// its later lines.
///
/// When each of them starts with `*` — the way such a comment is usually
/// written — the stars line up one column in from the `/*`, wherever they
/// stood. Otherwise a line keeps its indentation past the `/*`'s, and one
/// indented less lines up with it. A MySQL `/*! ... */` is code the server
/// runs, so it is copied as it is.
fn move_comment(text: &str, from: usize, to: usize) -> String {
    let mut lines = text.split('\n');
    let first = lines.next().unwrap_or_default();
    let rest: Vec<&str> = lines.collect();
    if rest.is_empty() || !text.starts_with("/*") || text.starts_with("/*!") {
        return text.to_owned();
    }
    let starred = rest.iter().all(|line| {
        let body = line.trim_start();
        body.is_empty() || body.starts_with('*')
    });
    let mut out = String::with_capacity(text.len() + rest.len() * (to + 1));
    out.push_str(first);
    for line in rest {
        out.push('\n');
        let body = line.trim_start();
        if body.is_empty() {
            continue;
        }
        if starred {
            out.extend(std::iter::repeat_n(' ', to + 1));
            out.push_str(body);
        } else {
            let past = line
                .char_indices()
                .take_while(|&(_, c)| c.is_whitespace())
                .nth(from)
                .map_or(body, |(n, _)| &line[n..]);
            out.extend(std::iter::repeat_n(' ', to));
            out.push_str(past);
        }
    }
    out
}

/// `text` with its first word in upper case when that word is one of
/// [`STATEMENT_WORDS`].
fn upper_first_word(text: &str) -> String {
    let lead = text.len() - text.trim_start().len();
    let word_len = text[lead..]
        .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
        .unwrap_or(text.len() - lead);
    let word = &text[lead..lead + word_len];
    if STATEMENT_WORDS
        .iter()
        .any(|known| known.eq_ignore_ascii_case(word))
    {
        format!(
            "{}{}{}",
            &text[..lead],
            word.to_ascii_uppercase(),
            &text[lead + word_len..]
        )
    } else {
        text.to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn my(sql: &str) -> String {
        format_ddl(sql, &Dialect::from_id("mariadb"))
    }

    #[test]
    fn a_mariadb_function_is_laid_out_by_block() {
        let ddl = "CREATE DEFINER=`rudbman`@`%` FUNCTION `f_add`(a INT, b INT) RETURNS int(11)\n    \
                   DETERMINISTIC\n    COMMENT 'adds'\nBEGIN declare x int default 0; if a > b then \
                   set x = a; elseif a < b then set x = b; else set x = 0; end if; \
                   return a + b + x; END;";
        assert_eq!(
            my(ddl),
            "CREATE DEFINER=`rudbman`@`%` FUNCTION `f_add`(
    a INT,
    b INT
) RETURNS int(11)
    DETERMINISTIC
    COMMENT 'adds'
BEGIN
    DECLARE x int DEFAULT 0;
    IF a > b THEN
        SET x = a;
    ELSEIF a < b THEN
        SET x = b;
    ELSE
        SET x = 0;
    END IF;
    RETURN a + b + x;
END;"
        );
    }

    #[test]
    fn statements_in_a_body_are_laid_out_like_queries() {
        let ddl = "CREATE DEFINER=`rudbman`@`%` PROCEDURE `p_hello`(IN who VARCHAR(20), OUT msg \
                   VARCHAR(100))\nBEGIN select concat('hi ', who) into msg from dual where 1 = 1 \
                   and who <> ''; END;";
        assert_eq!(
            my(ddl),
            "CREATE DEFINER=`rudbman`@`%` PROCEDURE `p_hello`(
    IN who VARCHAR(20),
    OUT msg VARCHAR(100)
)
BEGIN
    SELECT concat('hi ', who) INTO msg
      FROM dual
     WHERE 1 = 1
       AND who <> '';
END;"
        );
    }

    #[test]
    fn loops_cases_handlers_and_labels_nest() {
        let ddl = "CREATE PROCEDURE p() BEGIN DECLARE done INT DEFAULT FALSE; \
                   DECLARE c CURSOR FOR SELECT id FROM t; \
                   DECLARE CONTINUE HANDLER FOR NOT FOUND SET done = TRUE; \
                   DECLARE EXIT HANDLER FOR SQLEXCEPTION BEGIN ROLLBACK; RESIGNAL; END; \
                   OPEN c; read_loop: LOOP FETCH c INTO v; IF done THEN LEAVE read_loop; END IF; \
                   CASE v WHEN 1 THEN SET x = CASE WHEN y THEN 1 ELSE 2 END; ELSE BEGIN END; \
                   END CASE; END LOOP read_loop; WHILE i < 3 DO SET i = i + 1; END WHILE; \
                   REPEAT SET i = i - 1; UNTIL i = 0 END REPEAT; CLOSE c; END";
        assert_eq!(
            my(ddl),
            "CREATE PROCEDURE p()
BEGIN
    DECLARE done INT DEFAULT FALSE;
    DECLARE c CURSOR FOR
    SELECT id
      FROM t;
    DECLARE CONTINUE HANDLER FOR NOT FOUND SET done = TRUE;
    DECLARE EXIT HANDLER FOR SQLEXCEPTION BEGIN
        ROLLBACK;
        RESIGNAL;
    END;
    OPEN c;
    read_loop: LOOP
        FETCH c INTO v;
        IF done THEN
            LEAVE read_loop;
        END IF;
        CASE v
            WHEN 1 THEN
                SET x = CASE WHEN y THEN 1
                             ELSE 2
                        END;
            ELSE
                BEGIN
                END;
        END CASE;
    END LOOP read_loop;
    WHILE i < 3 DO
        SET i = i + 1;
    END WHILE;
    REPEAT
        SET i = i - 1;
    UNTIL i = 0
    END REPEAT;
    CLOSE c;
END"
        );
    }

    #[test]
    fn a_single_statement_body_and_comments_keep_their_place() {
        assert_eq!(
            my("CREATE FUNCTION f(a INT) RETURNS INT DETERMINISTIC RETURN a + 1;"),
            "CREATE FUNCTION f(a INT) RETURNS INT\n    DETERMINISTIC\nRETURN a + 1;"
        );
        assert_eq!(
            my("CREATE PROCEDURE p()\nBEGIN\n  -- first\n  SELECT 1; -- one\n\n  SELECT 2;\nEND"),
            "CREATE PROCEDURE p()\nBEGIN\n    -- first\n    SELECT 1; -- one\n\n    SELECT 2;\nEND"
        );
    }

    #[test]
    fn a_string_keeps_its_lines_when_its_statement_is_indented() {
        assert_eq!(
            my("CREATE PROCEDURE p() BEGIN set msg = 'one\ntwo'; END"),
            "CREATE PROCEDURE p()\nBEGIN\n    SET msg = 'one\ntwo';\nEND"
        );
    }

    #[test]
    fn a_long_parameter_list_goes_one_to_a_line() {
        let ddl = "CREATE PROCEDURE `register_customer`(IN p_first_name VARCHAR(100), \
                   IN p_last_name VARCHAR(100), IN p_email VARCHAR(255), OUT p_id BIGINT) \
                   MODIFIES SQL DATA SQL SECURITY INVOKER BEGIN SELECT 1; END";
        assert_eq!(
            my(ddl),
            "CREATE PROCEDURE `register_customer`(
    IN p_first_name VARCHAR(100),
    IN p_last_name VARCHAR(100),
    IN p_email VARCHAR(255),
    OUT p_id BIGINT
)
    MODIFIES SQL DATA
    SQL SECURITY INVOKER
BEGIN
    SELECT 1;
END"
        );
    }

    #[test]
    fn several_parameters_go_one_to_a_line_and_one_stays_beside_the_name() {
        let ddl = "CREATE FUNCTION `FN_TEST`(`v_str_date` VARCHAR(100), `v_dateformat` \
                   VARCHAR(100)) RETURNS date\nBEGIN\n/*\n * 테스트 작업\n */\n    DECLARE v_date \
                   DATE;\n\n    SELECT STR_TO_DATE(v_str_date, v_dateformat) INTO v_date;\n\n    \
                   RETURN v_date;\nEND";
        assert_eq!(
            my(ddl),
            "CREATE FUNCTION `FN_TEST`(
    `v_str_date` VARCHAR(100),
    `v_dateformat` VARCHAR(100)
) RETURNS date
BEGIN
    /*
     * 테스트 작업
     */
    DECLARE v_date DATE;

    SELECT STR_TO_DATE(v_str_date, v_dateformat) INTO v_date;

    RETURN v_date;
END"
        );
        assert_eq!(
            my("CREATE FUNCTION f(a INT) RETURNS INT RETURN a;"),
            "CREATE FUNCTION f(a INT) RETURNS INT\nRETURN a;"
        );
    }

    #[test]
    fn a_comment_on_lines_of_its_own_moves_with_its_opening() {
        // Stars line up under the `/*` wherever they stood, even out of line.
        assert_eq!(
            my("CREATE PROCEDURE p()\nBEGIN\n    /*\n * one\n\n *two\n*/\n    SELECT 1;\nEND"),
            "CREATE PROCEDURE p()\nBEGIN\n    /*\n     * one\n\n     *two\n     */\n    SELECT 1;\nEND"
        );
        // Other lines keep their indentation past the `/*`'s.
        assert_eq!(
            my(
                "CREATE PROCEDURE p()\nBEGIN\n/* first\n     second\n*/\nSELECT 1; /* a\n          b */\nEND"
            ),
            "CREATE PROCEDURE p()
BEGIN
    /* first
         second
    */
    SELECT 1; /* a
              b */
END"
        );
        // Before the statement, at the left margin.
        assert_eq!(
            my("  /**\n   * Adds.\n   */\nCREATE FUNCTION f() RETURNS INT RETURN 1;"),
            "/**\n * Adds.\n */\nCREATE FUNCTION f() RETURNS INT\nRETURN 1;"
        );
        // A versioned comment is code, and stays as written.
        assert_eq!(
            move_comment("/*!50100 PARTITION BY\n  HASH (id) */", 0, 4),
            "/*!50100 PARTITION BY\n  HASH (id) */"
        );
    }

    #[test]
    fn a_trigger_puts_its_body_under_its_header() {
        assert_eq!(
            my(
                "CREATE DEFINER=`root`@`%` trigger aud.aud_trg before insert on aud.aud_child \
                for each row set new.note = upper(new.note);"
            ),
            "CREATE DEFINER=`root`@`%` TRIGGER aud.aud_trg BEFORE INSERT ON aud.aud_child \
             FOR EACH ROW\nSET new.note = upper(new.note);"
        );
        assert_eq!(
            my(
                "CREATE TRIGGER t AFTER UPDATE ON x FOR EACH ROW FOLLOWS other BEGIN \
                IF new.a <> old.a THEN INSERT INTO log VALUES (new.a); END IF; END;"
            ),
            "CREATE TRIGGER t AFTER UPDATE ON x FOR EACH ROW FOLLOWS other
BEGIN
    IF new.a <> old.a THEN
        INSERT INTO log
        VALUES (new.a);
    END IF;
END;"
        );
        let ora = Dialect::from_id("oracle");
        assert_eq!(
            format_ddl(
                "CREATE OR REPLACE trigger aud_trg before insert on aud_child for each row \
                 when (new.note is not null) begin :new.note := upper(:new.note); end;",
                &ora
            ),
            "CREATE OR REPLACE TRIGGER aud_trg BEFORE INSERT ON aud_child FOR EACH ROW \
             WHEN (new.note IS NOT NULL)\nBEGIN\n    :new.note := upper(:new.note);\nEND;"
        );
        let sqlite = Dialect::from_id("sqlite");
        assert_eq!(
            format_ddl(
                "CREATE TRIGGER aud_trg after insert on aud_child begin update aud_child \
                 set note = upper(note) where id = new.id; end;",
                &sqlite
            ),
            "CREATE TRIGGER aud_trg AFTER INSERT ON aud_child
BEGIN
    UPDATE aud_child
       SET note = upper(note)
     WHERE id = new.id;
END;"
        );
        // A call is the whole trigger.
        let pg = Dialect::from_id("postgres");
        assert_eq!(
            format_ddl(
                "CREATE TRIGGER aud_trg BEFORE INSERT ON aud.aud_child FOR EACH ROW \
                 EXECUTE FUNCTION aud.aud_trgfn();",
                &pg
            ),
            "CREATE TRIGGER aud_trg BEFORE INSERT ON aud.aud_child FOR EACH ROW \
             EXECUTE FUNCTION aud.aud_trgfn();"
        );
        // And T-SQL keeps its body as written.
        let ms = Dialect::from_id("mssql");
        assert_eq!(
            format_ddl(
                "create trigger aud.aud_trg on aud.aud_child after insert as begin\n  \
                 set nocount on;\nend",
                &ms
            ),
            "CREATE TRIGGER aud.aud_trg ON aud.aud_child AFTER INSERT\nas begin\n  set nocount \
             on;\nend"
        );
    }

    #[test]
    fn a_sequence_and_a_view_write_their_own_words_in_upper_case() {
        assert_eq!(
            my(
                "CREATE SEQUENCE `cache` start with 100 minvalue 1 maxvalue 9 increment by 5 \
                cache 20 nocycle ENGINE=InnoDB COMMENT='order numbers';"
            ),
            "CREATE SEQUENCE `cache`
    START WITH 100
    MINVALUE 1
    MAXVALUE 9
    INCREMENT BY 5
    CACHE 20
    NOCYCLE
    ENGINE=InnoDB
    COMMENT='order numbers';"
        );
        assert_eq!(
            format_ddl(
                "create sequence if not exists s.cycle as bigint start with 1 no minvalue \
                 no cycle",
                &Dialect::from_id("postgres")
            ),
            "CREATE SEQUENCE IF NOT EXISTS s.cycle
    AS bigint
    START WITH 1
    NO MINVALUE
    NO CYCLE"
        );
        assert_eq!(
            format_ddl("CREATE SEQUENCE s;", &Dialect::GENERIC),
            "CREATE SEQUENCE s;"
        );
        assert_eq!(
            format_ddl(
                "create view v as select id from t",
                &Dialect::from_id("mssql")
            ),
            "CREATE VIEW v AS\nSELECT id\n  FROM t"
        );
    }

    #[test]
    fn a_table_puts_one_column_to_a_line() {
        let h2 = Dialect::from_id("h2");
        assert_eq!(
            format_ddl(
                "CREATE MEMORY TABLE \"APP\".\"T\"(\"ID\" INTEGER NOT NULL, \"NAME\" \
                 CHARACTER VARYING(40) DEFAULT 'a, b', CONSTRAINT \"C\" CHECK((\"ID\" > 0)));\n\
                 ALTER TABLE \"APP\".\"T\" ADD CONSTRAINT \"PK\" PRIMARY KEY(\"ID\");\n\
                 COMMENT ON TABLE \"APP\".\"T\" IS 'x';\nCOMMENT ON COLUMN \"APP\".\"T\".\"ID\" IS 'y';",
                &h2,
            ),
            "CREATE MEMORY TABLE \"APP\".\"T\" (
    \"ID\" INTEGER NOT NULL,
    \"NAME\" CHARACTER VARYING(40) DEFAULT 'a, b',
    CONSTRAINT \"C\" CHECK((\"ID\" > 0))
);

ALTER TABLE \"APP\".\"T\" ADD CONSTRAINT \"PK\" PRIMARY KEY(\"ID\");

COMMENT ON TABLE \"APP\".\"T\" IS 'x';
COMMENT ON COLUMN \"APP\".\"T\".\"ID\" IS 'y';"
        );
    }

    #[test]
    fn mysql_table_options_keep_their_spacing() {
        let ddl = "CREATE TABLE `t` (\n  `id` int(11) NOT NULL AUTO_INCREMENT, -- key\n  \
                   `n` varchar(10) DEFAULT NULL,\n  PRIMARY KEY (`id`)\n) ENGINE=InnoDB DEFAULT \
                   CHARSET=utf8mb4;";
        assert_eq!(
            my(ddl),
            "CREATE TABLE `t` (
    `id` int(11) NOT NULL AUTO_INCREMENT, -- key
    `n` varchar(10) DEFAULT NULL,
    PRIMARY KEY (`id`)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;"
        );
    }

    #[test]
    fn partitions_go_one_to_a_line_under_the_table() {
        assert_eq!(
            my(
                "CREATE TABLE `t` (\n  `id` int NOT NULL,\n  `created` timestamp NOT NULL\n) \
                ENGINE=InnoDB\n PARTITION BY RANGE (unix_timestamp(`created`))\n(PARTITION `p0` \
                VALUES LESS THAN (1700000000) ENGINE = InnoDB,\n PARTITION `p1` VALUES LESS THAN \
                MAXVALUE ENGINE = InnoDB);"
            ),
            "CREATE TABLE `t` (
    `id` int NOT NULL,
    `created` timestamp NOT NULL
) ENGINE=InnoDB
PARTITION BY RANGE (unix_timestamp(`created`)) (
    PARTITION `p0` VALUES LESS THAN (1700000000) ENGINE = InnoDB,
    PARTITION `p1` VALUES LESS THAN MAXVALUE ENGINE = InnoDB
);"
        );
        // MySQL hides the same clause in a versioned comment, which is copied
        // and keeps the line it was written on.
        assert_eq!(
            format_ddl(
                "CREATE TABLE `t` (\n  `id` int NOT NULL\n) ENGINE=InnoDB\n/*!50100 PARTITION BY \
                 HASH (`id`)\nPARTITIONS 4 */;",
                &Dialect::from_id("mysql")
            ),
            "CREATE TABLE `t` (\n    `id` int NOT NULL\n) ENGINE=InnoDB\n/*!50100 PARTITION BY \
             HASH (`id`)\nPARTITIONS 4 */;"
        );
    }

    #[test]
    fn a_declare_writes_its_own_words_in_upper_case() {
        assert_eq!(
            my(
                "CREATE PROCEDURE p() BEGIN declare cur cursor for select found from t; \
                declare continue handler for not found set done = true; END"
            ),
            "CREATE PROCEDURE p()
BEGIN
    DECLARE cur CURSOR FOR
    SELECT found
      FROM t;
    DECLARE CONTINUE HANDLER FOR NOT FOUND SET done = true;
END"
        );
    }

    #[test]
    fn a_view_puts_its_query_under_its_header() {
        assert_eq!(
            my(
                "CREATE ALGORITHM=UNDEFINED DEFINER=`rudbman`@`%` SQL SECURITY DEFINER VIEW `v1` \
                AS select 1 AS `a`,'x' AS `b` from DUAL  where 1 = 1;"
            ),
            "CREATE ALGORITHM=UNDEFINED DEFINER=`rudbman`@`%` SQL SECURITY DEFINER VIEW `v1` AS
SELECT 1 AS `a`,
       'x' AS `b`
  FROM DUAL
 WHERE 1 = 1;"
        );
    }

    #[test]
    fn a_postgres_body_in_dollar_quotes_is_copied() {
        let pg = Dialect::from_id("postgres");
        let ddl = "CREATE OR REPLACE FUNCTION public.f_add(a integer, b integer)\n RETURNS integer\n \
                   LANGUAGE plpgsql\n IMMUTABLE STRICT\nAS $function$\nbegin\n  return a  +  b;\nend\n\
                   $function$;\n\nCREATE OR REPLACE PROCEDURE public.p(IN who text)\n LANGUAGE sql\n\
                   AS $procedure$select 1$procedure$;";
        assert_eq!(
            format_ddl(ddl, &pg),
            "CREATE OR REPLACE FUNCTION public.f_add(
    a integer,
    b integer
) RETURNS integer
    LANGUAGE plpgsql
    IMMUTABLE
    STRICT
AS $function$
begin
  return a  +  b;
end
$function$;

CREATE OR REPLACE PROCEDURE public.p(IN who text)
    LANGUAGE sql
AS $procedure$select 1$procedure$;"
        );
    }

    #[test]
    fn an_oracle_routine_gets_its_declaration_section() {
        let ora = Dialect::from_id("oracle");
        let ddl = "CREATE OR REPLACE PROCEDURE p_hello(who IN VARCHAR2) IS x NUMBER := 0; \
                   CURSOR c IS SELECT id FROM t; BEGIN FOR r IN c LOOP x := x + r.id; END LOOP; \
                   IF x > 0 THEN dbms_output.put_line(who); ELSIF x < 0 THEN NULL; END IF; \
                   EXCEPTION WHEN no_data_found THEN NULL; WHEN OTHERS THEN RAISE; END p_hello;";
        assert_eq!(
            format_ddl(ddl, &ora),
            "CREATE OR REPLACE PROCEDURE p_hello(who IN VARCHAR2)
IS
    x NUMBER := 0;
    CURSOR c IS
    SELECT id
      FROM t;
BEGIN
    FOR r IN c LOOP
        x := x + r.id;
    END LOOP;
    IF x > 0 THEN
        dbms_output.put_line(who);
    ELSIF x < 0 THEN
        NULL;
    END IF;
EXCEPTION
    WHEN no_data_found THEN
        NULL;
    WHEN OTHERS THEN
        RAISE;
END p_hello;"
        );
    }

    #[test]
    fn a_sql_server_body_is_left_as_written() {
        let ms = Dialect::from_id("mssql");
        let ddl = "CREATE PROCEDURE dbo.p @a int,   @b int\nWITH RECOMPILE\nAS\nBEGIN\n  \
                   SET NOCOUNT ON\n  SELECT @a\nEND";
        assert_eq!(
            format_ddl(ddl, &ms),
            "CREATE PROCEDURE dbo.p @a int, @b int\n    WITH RECOMPILE\nAS\nBEGIN\n  SET NOCOUNT \
             ON\n  SELECT @a\nEND"
        );
    }

    /// What `SYSCAT.TRIGGERS` and `SYSCAT.ROUTINES` hand back on a Db2 12.1:
    /// a trigger whose `AS` names its new row rather than opening a body, and
    /// a function whose `NO EXTERNAL ACTION` is one characteristic, not two.
    #[test]
    fn db2_triggers_and_characteristics_keep_their_phrases() {
        let db2 = Dialect::from_id("db2");
        assert_eq!(
            format_ddl(
                "CREATE OR REPLACE TRIGGER t_upper no cascade BEFORE INSERT ON t \
                 REFERENCING new AS n for each row SET n.\"note\" = upper(n.\"note\")",
                &db2
            ),
            "CREATE OR REPLACE TRIGGER t_upper NO CASCADE BEFORE INSERT ON t REFERENCING NEW \
             AS n FOR EACH ROW\nSET n.\"note\" = upper(n.\"note\")"
        );
        assert_eq!(
            format_ddl(
                "CREATE TRIGGER t_audit AFTER UPDATE ON t REFERENCING OLD AS o NEW AS n \
                 FOR EACH ROW WHEN (o.qty <> n.qty) BEGIN ATOMIC \
                 INSERT INTO audit VALUES (n.id); END",
                &db2
            ),
            "CREATE TRIGGER t_audit AFTER UPDATE ON t REFERENCING OLD AS o NEW AS n FOR EACH ROW \
             WHEN (o.qty <> n.qty)\nBEGIN ATOMIC\n    INSERT INTO audit\n    VALUES (n.id);\nEND"
        );
        assert_eq!(
            format_ddl(
                "CREATE PROCEDURE p (IN a int) LANGUAGE SQL dynamic result sets 1 \
                 no external action not fenced commit on return no BEGIN SET a = 1; END",
                &db2
            ),
            "CREATE PROCEDURE p (IN a int)\n    LANGUAGE SQL\n    DYNAMIC RESULT SETS 1\n    \
             NO EXTERNAL ACTION\n    NOT FENCED\n    COMMIT ON RETURN no\nBEGIN\n    SET a = 1;\nEND"
        );
    }

    #[test]
    fn other_statements_go_on_one_line() {
        assert_eq!(
            format_ddl(
                "CREATE FORCE ALIAS \"APP\".\"F\"   FOR 'x.Y.add';",
                &Dialect::from_id("h2")
            ),
            "CREATE FORCE ALIAS \"APP\".\"F\" FOR 'x.Y.add';"
        );
        assert_eq!(
            format_ddl(
                "CREATE INDEX i ON t (a);\n\nCREATE INDEX j ON t (b);",
                &Dialect::GENERIC
            ),
            "CREATE INDEX i ON t (a);\nCREATE INDEX j ON t (b);"
        );
    }

    #[test]
    fn formatting_is_idempotent_and_changes_only_whitespace() {
        let samples = [
            (
                "mariadb",
                "CREATE PROCEDURE p() BEGIN DECLARE c CURSOR FOR SELECT id FROM t WHERE a = 1; \
                         lbl: WHILE i < 3 DO IF a THEN SELECT 1; ELSE SELECT 2; END IF; END WHILE lbl; END;",
            ),
            (
                "mysql",
                "CREATE TABLE `t` (`id` int NOT NULL, KEY `k` (`id`)) ENGINE=InnoDB COMMENT='a; b';",
            ),
            (
                "postgres",
                "CREATE FUNCTION f() RETURNS int LANGUAGE sql AS 'select 1';",
            ),
            (
                "oracle",
                "CREATE FUNCTION f(a NUMBER) RETURN NUMBER IS BEGIN RETURN a; END f;",
            ),
            (
                "h2",
                "CREATE FORCE VIEW \"APP\".\"V\"(\"ID\") AS\nSELECT\n    \"ID\"\nFROM \"APP\".\"T\";",
            ),
            (
                "oracle",
                "CREATE TRIGGER t BEFORE UPDATE OF a ON x FOR EACH ROW DECLARE n NUMBER; \
                 BEGIN n := 1; END;",
            ),
            (
                "mysql",
                "CREATE TRIGGER t BEFORE INSERT ON x FOR EACH ROW BEGIN SET NEW.a = 1; END",
            ),
            (
                "oracle",
                "CREATE SEQUENCE \"S\".\"Q\" MINVALUE 1 MAXVALUE 9 INCREMENT BY 5 START WITH 1 \
                 CACHE 20 NOORDER NOCYCLE GLOBAL;",
            ),
            (
                "mariadb",
                "/*\n   * doc\n */\nCREATE FUNCTION f(a INT, b INT) RETURNS INT BEGIN\n\
                 /* x\n     y */\n  /*\n * z\n */\nRETURN a; /* r\n   s */\nEND",
            ),
        ];
        for (dialect, sql) in samples {
            let dialect = Dialect::from_id(dialect);
            let once = format_ddl(sql, &dialect);
            assert_eq!(format_ddl(&once, &dialect), once, "{once}");
            // A comment's later lines may move, so only its words count.
            let words = |s: &str| -> Vec<String> {
                items(s, &dialect)
                    .iter()
                    .map(|i| match i.kind {
                        TokenKind::Comment => {
                            i.text.split_whitespace().collect::<Vec<_>>().join(" ")
                        }
                        _ => i.text.to_ascii_uppercase(),
                    })
                    .collect()
            };
            assert_eq!(words(sql), words(&once), "{once}");
        }
    }
}
