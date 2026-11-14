//! Cutting a script into statements the way the product's own client does.
//!
//! [`split_statements`](crate::split_statements) is a semicolon splitter, and
//! that is all a semicolon splitter can be: it has no idea where a trigger body
//! ends. Every product's command-line client answers that question with a
//! convention of its own, and none of them is SQL. A script written for that
//! client (a schema backup, `mysqldump` output, a script SSMS generated, a
//! SQL Developer export) only runs if the reader follows the same convention.
//! [`ScriptSplitter`] does that, one line at a time, so a file of any size can
//! be streamed through it; [`split_script`] runs it over a string already in
//! memory, which is the editor's "Run all".
//!
//! # The clients it reads as
//!
//! | Dialect             | Client     | What it changes                                       |
//! |---------------------|------------|-------------------------------------------------------|
//! | PostgreSQL          | `psql`     | `\` meta-commands at the start of a line              |
//! | MySQL, MariaDB      | `mysql`    | `DELIMITER`, the client's own commands, `/*! … */`    |
//! | SQL Server          | `sqlcmd`   | `GO` ends a batch, `:` commands, `EXIT`/`QUIT`        |
//! | Oracle              | SQL\*Plus  | `/` ends PL/SQL, SQL\*Plus commands, `EXEC`           |
//! | Db2                 | the CLP    | `--#SET TERMINATOR`                                   |
//! | SQLite              | `sqlite3`  | `CREATE TRIGGER … END;`, `.` commands                  |
//! | H2, anything else   | none       | plain semicolons                                      |
//!
//! None of these needs a parser. Every rule is either a line of its own (a
//! directive, read only where the lexer says no string or comment is open) or
//! a decision taken from the words of a statement: its first few, the same
//! ones the client itself looks at, and the `BEGIN`, `CASE` and `END` of a
//! body, which are counted as [`split_statements`] counts them so that a
//! procedure typed without a `DELIMITER` around it — or a PL/SQL block typed
//! without its slash — is still one statement.
//!
//! # What is refused
//!
//! A client command this reader cannot carry out comes back as a
//! [`ScriptError`] naming its line, never as a guess. That covers including
//! another file (`\i`, `SOURCE`, `@`, `:r`), connecting somewhere else
//! (`\connect`, `CONNECT`), variables the script expects to be substituted
//! (`DEFINE x = …`, `:setvar`, most of psql's `\set`), and shell escapes.
//! `COPY … FROM stdin`, `pg_dump`'s default for rows, is refused too: its rows
//! follow in the script as tab-separated text, and a statement executor has no
//! way to hand them over. Commands that only change what the client prints
//! (`SET DEFINE OFF`, `PROMPT`, `\echo`, `\restrict`) are skipped, and so are
//! the ones that only matter to a terminal. `EXIT`, `QUIT` and `\q` end the
//! script there.
//!
//! # Where the readers part from their clients
//!
//! * A PL/SQL unit ends at the `;` after the `END` that closes it, slash or
//!   no slash. SQL\*Plus waits for the slash, and without one takes the
//!   statements under the unit into it.
//! * A `/` on a line of its own after a statement that already ran is ignored.
//!   SQL\*Plus would run that statement a second time, which a hand-written
//!   script almost never means. The `/` still ends whatever is open, which is
//!   what keeps a body the counting gets wrong from running past it.
//! * Blank lines never end a SQL\*Plus statement, as with
//!   `SET SQLBLANKLINES ON`, which is what rudbman's own Oracle backups set.
//! * A semicolon in the middle of a line ends a SQL\*Plus statement too, as
//!   the editor has always split it.
//! * Inside a `sqlcmd` batch, statements are still split on semicolons, so
//!   two `SELECT`s keep their two grids, unless the batch holds a procedural
//!   block ([`holds_procedural_block`]), in which case the batch is sent whole,
//!   as `sqlcmd` sends every batch.
//!
//! # What an editor sees
//!
//! [`script_units`] reads a script the same way for an editor, which wants
//! every statement and where its terminator is rather than a run that stops
//! at the first line it cannot carry out: a refused line is a unit of its
//! own, carrying its [`ScriptError`], and the reading goes on past it,
//! past an `EXIT`, and past the rows of a `COPY … FROM stdin`. A `sqlcmd`
//! batch is cut into its statements, a body kept whole, rather than sent as
//! one: the batch is what "Run all" sends, the statement what the cursor is
//! in.
//!
//! ```
//! use rudbman_sql::{Dialect, split_script};
//!
//! let script = "DELIMITER ;;\n\
//!               CREATE PROCEDURE p() BEGIN SELECT 1; SELECT 2; END;;\n\
//!               DELIMITER ;\n\
//!               CALL p();\n";
//! let statements = split_script(script, &Dialect::MYSQL).unwrap();
//! assert_eq!(statements.len(), 2);
//! assert_eq!(statements[0].sql, "CREATE PROCEDURE p() BEGIN SELECT 1; SELECT 2; END");
//! assert_eq!(statements[1].sql, "CALL p()");
//! assert_eq!(statements[1].line, 4);
//! ```

use std::fmt;
use std::ops::Range;

use crate::block::{Blocks, Semicolon};
use crate::dialect::{Dialect, DialectId};
use crate::lexer::{Lexer, LineState, Token, TokenKind};
use crate::statement::{holds_procedural_block, split_statements};

/// One statement of a script, ready to send.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScriptStatement {
    /// The text to execute: no terminator, except the `END;` a PL/SQL unit
    /// needs as part of itself.
    pub sql: String,
    /// The bytes of the script the statement was cut from, counted from the
    /// first byte the splitter was fed. Equal to where `sql` sits in the
    /// script, except for a SQL\*Plus `EXEC`, which is rewritten into a block
    /// and points at its line.
    pub range: Range<usize>,
    /// The line `range` starts on, counting from one.
    pub line: usize,
}

/// One statement of a script as an editor shows it: where it is, and what
/// running it sends. See [`script_units`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScriptUnit {
    /// The bytes it covers as written: from its first byte that is not
    /// whitespace — a comment above it included — to one past the `;`, the
    /// delimiter or the `/` that ends it, or past its last byte when nothing
    /// does. For a refused line, the line.
    pub range: Range<usize>,
    /// What running it sends, or why it cannot be run.
    pub run: Result<ScriptStatement, ScriptError>,
}

impl ScriptUnit {
    /// One past the last byte it sends: the end of [`ScriptStatement::range`],
    /// or of the refused line.
    pub fn sql_end(&self) -> usize {
        match &self.run {
            Ok(statement) => statement.range.end,
            Err(error) => error.range.end,
        }
    }
}

/// Where the splitter's output goes: a run's statements, which stop at the
/// first refusal, or an editor's units, which do not.
trait Out {
    /// Whether this is an editor's reading ([`script_units`]).
    const UNITS: bool;

    /// A statement that is ready, and one past whatever ended it.
    fn statement(&mut self, statement: ScriptStatement, end: usize);

    /// A line or a statement that cannot be run, and one past whatever ended
    /// it.
    ///
    /// # Errors
    ///
    /// The error itself, for a run, which stops there.
    fn refuse(&mut self, error: ScriptError, end: usize) -> Result<(), ScriptError>;
}

impl Out for Vec<ScriptStatement> {
    const UNITS: bool = false;

    fn statement(&mut self, statement: ScriptStatement, _end: usize) {
        self.push(statement);
    }

    fn refuse(&mut self, error: ScriptError, _end: usize) -> Result<(), ScriptError> {
        Err(error)
    }
}

impl Out for Vec<ScriptUnit> {
    const UNITS: bool = true;

    fn statement(&mut self, statement: ScriptStatement, end: usize) {
        self.push(ScriptUnit {
            range: statement.range.start..end.max(statement.range.end),
            run: Ok(statement),
        });
    }

    fn refuse(&mut self, error: ScriptError, end: usize) -> Result<(), ScriptError> {
        self.push(ScriptUnit {
            range: error.range.start..end.max(error.range.end),
            run: Err(error),
        });
        Ok(())
    }
}

/// A line the splitter would not get past.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScriptError {
    /// The line, counting from one.
    pub line: usize,
    /// The line's bytes, its line break and its indentation left out.
    pub range: Range<usize>,
    /// What is wrong with it.
    pub kind: ScriptErrorKind,
}

/// Why a [`ScriptError`] was raised.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScriptErrorKind {
    /// A client command that cannot be carried out here, as it is spelled in
    /// the script: `\connect`, `SOURCE`, `@`, `:setvar`, `DEFINE`.
    Unsupported(String),
    /// `COPY … FROM stdin`, whose rows follow it in the script.
    CopyFromStdin,
    /// `DELIMITER` or `--#SET TERMINATOR` with nothing after it.
    MissingDelimiter,
}

impl fmt::Display for ScriptError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.kind {
            ScriptErrorKind::Unsupported(command) => write!(
                f,
                "line {}: the client command {command} is not supported",
                self.line
            ),
            ScriptErrorKind::CopyFromStdin => write!(
                f,
                "line {}: COPY ... FROM stdin is not supported; dump with --inserts",
                self.line
            ),
            ScriptErrorKind::MissingDelimiter => {
                write!(f, "line {}: no delimiter follows the command", self.line)
            }
        }
    }
}

impl std::error::Error for ScriptError {}

/// Whose conventions a script is read by.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Client {
    Psql,
    Mysql,
    Sqlcmd,
    SqlPlus,
    Clp,
    Sqlite,
    Plain,
}

impl Client {
    const fn of(dialect: &Dialect) -> Self {
        match dialect.id() {
            DialectId::Postgres => Self::Psql,
            DialectId::MySql | DialectId::MariaDb => Self::Mysql,
            DialectId::MsSql => Self::Sqlcmd,
            DialectId::Oracle => Self::SqlPlus,
            DialectId::Db2 => Self::Clp,
            DialectId::Sqlite => Self::Sqlite,
            DialectId::H2 | DialectId::Generic => Self::Plain,
        }
    }
}

/// What a directive line asked for.
enum Directive {
    /// Nothing reaches the server.
    Skip,
    /// The script ends here.
    End,
    /// This statement runs in the line's place.
    Run(String),
}

/// Cuts a script into statements, one line at a time.
///
/// Feed every line, its line break included, to [`ScriptSplitter::push_line`]
/// in order, then call [`ScriptSplitter::finish`] for the statement the script
/// may end without terminating. Statements come out as soon as they are
/// complete; nothing is held back but the statement being read (and on SQL
/// Server the batch, which is only cut once its `GO` shows whether it holds a
/// block).
pub struct ScriptSplitter {
    dialect: Dialect,
    client: Client,
    /// The lexer's state at the start of the next line.
    lex: LineState,
    /// Bytes fed so far: where the next line starts.
    offset: usize,
    /// Lines fed so far.
    line: usize,
    /// The script from `origin` on: the statement being read and what follows
    /// it on its last line.
    buf: String,
    /// Where `buf` starts.
    origin: usize,
    /// Where the statement being read starts, and on which line: its first
    /// byte that is not whitespace, a comment above it included.
    start: Option<(usize, usize)>,
    /// One past its last byte that is not whitespace.
    end: usize,
    /// Whether it holds anything but comments.
    code: bool,
    /// The terminator, where the client lets a script change it.
    delimiter: String,
    /// Which `;` ends the statement being read, while the terminator is one.
    blocks: Blocks,
    /// One past the terminator that ended the statement about to be handed
    /// out, which is where an editor's unit of it ends.
    closed: Option<usize>,
    /// Whether the lines are the rows of a `COPY … FROM stdin`, which an
    /// editor's reading steps over to the `\.` that ends them.
    copy_rows: bool,
    /// Whether the previous line was a skipped SQL\*Plus command continued
    /// with a trailing `-`.
    continued: bool,
    /// Whether the script said `EXIT`, `QUIT` or `\q`.
    ended: bool,
    /// Whether any line was read as a directive of the client.
    directives: bool,
}

impl ScriptSplitter {
    /// A splitter for a script written for `dialect`'s client.
    pub fn new(dialect: &Dialect) -> Self {
        Self {
            dialect: *dialect,
            client: Client::of(dialect),
            lex: LineState::START,
            offset: 0,
            line: 0,
            buf: String::new(),
            origin: 0,
            start: None,
            end: 0,
            code: false,
            delimiter: ";".to_string(),
            blocks: Blocks::new(dialect),
            closed: None,
            copy_rows: false,
            continued: false,
            ended: false,
            directives: false,
        }
    }

    /// Whether any line so far was one of the client's own: a `DELIMITER`, a
    /// `GO`, a `/`, a skipped or refused command.
    pub fn saw_directives(&self) -> bool {
        self.directives
    }

    /// Whether the script ended itself with `EXIT`, `QUIT` or `\q`.
    pub fn has_ended(&self) -> bool {
        self.ended
    }

    /// Reads one line, its line break included, and appends to `out` every
    /// statement it completes.
    ///
    /// # Errors
    ///
    /// A [`ScriptError`] for a line that cannot be run as written. The
    /// splitter is not meant to be fed further after one.
    pub fn push_line(
        &mut self,
        line: &str,
        out: &mut Vec<ScriptStatement>,
    ) -> Result<(), ScriptError> {
        self.read_line(line, out)
    }

    /// [`Self::push_line`], into either kind of output.
    fn read_line<O: Out>(&mut self, line: &str, out: &mut O) -> Result<(), ScriptError> {
        let at = self.offset;
        self.offset += line.len();
        self.line += 1;
        if self.ended {
            return Ok(());
        }
        if self.copy_rows {
            self.copy_rows = line.trim_end() != "\\.";
            return Ok(());
        }
        if self.continued {
            self.continued = continues(line);
            return Ok(());
        }
        if self.lex.is_start() && self.directive(line, at, out)? {
            self.directives = true;
            return Ok(());
        }

        if self.start.is_none() {
            self.buf.clear();
            self.origin = at;
        }
        self.buf.push_str(line);
        self.scan(line, at, out)?;

        // Keep only the statement still being read.
        match self.start {
            None => {
                self.buf.clear();
                self.origin = self.offset;
            }
            Some((start, _)) if start > self.origin => {
                self.buf.drain(..start - self.origin);
                self.origin = start;
            }
            Some(_) => {}
        }
        Ok(())
    }

    /// Ends the script, appending the statement it ended without terminating,
    /// if there is one.
    ///
    /// # Errors
    ///
    /// The last statement's [`ScriptError`], as [`Self::push_line`] would
    /// have raised it.
    pub fn finish(&mut self, out: &mut Vec<ScriptStatement>) -> Result<(), ScriptError> {
        self.end_script(out)
    }

    /// [`Self::finish`], into either kind of output.
    fn end_script<O: Out>(&mut self, out: &mut O) -> Result<(), ScriptError> {
        if self.ended {
            return Ok(());
        }
        match self.client {
            Client::Sqlcmd => self.flush_batch(1, out),
            _ => self.emit(out),
        }
    }

    /// Handles `line` when it is one of the client's own. Answers whether it
    /// was.
    fn directive<O: Out>(
        &mut self,
        line: &str,
        at: usize,
        out: &mut O,
    ) -> Result<bool, ScriptError> {
        let text = line.trim();
        if text.is_empty() {
            return Ok(false);
        }
        let lead = line.len() - line.trim_start().len();
        let range = at + lead..at + lead + text.len();

        // The lines that end what came before them, read whatever is pending.
        match self.client {
            Client::Sqlcmd => {
                if let Some(count) = go_count(text) {
                    self.flush_batch(count, out)?;
                    return Ok(true);
                }
                let (word, _) = first_word(text);
                if word.eq_ignore_ascii_case("EXIT")
                    || word.eq_ignore_ascii_case("QUIT")
                    || starts_with_ignore_case(text, "EXIT(")
                {
                    self.flush_batch(1, out)?;
                    // An editor reads on: the statements under an `EXIT` are
                    // still statements, run one at a time.
                    self.ended = !O::UNITS;
                    return Ok(true);
                }
            }
            Client::SqlPlus if text == "/" => {
                self.closed = Some(range.end);
                self.emit(out)?;
                return Ok(true);
            }
            _ => {}
        }

        // The rest are read only where a statement would start.
        if self.code {
            return Ok(false);
        }
        let directive = match self.client {
            Client::Psql => psql_command(text),
            Client::Mysql => self.mysql_command(text),
            Client::Sqlcmd => sqlcmd_command(text),
            Client::SqlPlus => self.sqlplus_command(text),
            Client::Clp => self.clp_command(text),
            Client::Sqlite => sqlite_command(text),
            Client::Plain => Ok(None),
        };
        let directive = match directive {
            Ok(Some(directive)) => directive,
            Ok(None) => return Ok(false),
            Err(kind) => {
                self.discard();
                let end = range.end;
                out.refuse(
                    ScriptError {
                        line: self.line,
                        range,
                        kind,
                    },
                    end,
                )?;
                return Ok(true);
            }
        };
        // A comment above a command belongs to nothing that runs.
        self.discard();
        match directive {
            Directive::Skip => {}
            Directive::End => self.ended = !O::UNITS,
            Directive::Run(sql) => {
                let end = range.end;
                out.statement(
                    ScriptStatement {
                        sql,
                        range,
                        line: self.line,
                    },
                    end,
                );
            }
        }
        Ok(true)
    }

    /// Walks `line`'s tokens, emitting each statement that ends on it.
    fn scan<O: Out>(&mut self, line: &str, at: usize, out: &mut O) -> Result<(), ScriptError> {
        let mut lexer = Lexer::with_state(line, self.lex, &self.dialect);
        let tokens: Vec<_> = lexer.by_ref().collect();
        self.lex = lexer.state();
        let batch = self.client == Client::Sqlcmd;
        let executable_comments = self.client == Client::Mysql;

        let mut i = 0;
        while i < tokens.len() {
            let token = tokens[i];
            match token.kind {
                TokenKind::Whitespace => i += 1,
                TokenKind::Comment => {
                    let text = &line[token.start..token.end];
                    let executable = executable_comments
                        && (text.starts_with("/*!") || text.starts_with("/*M!"));
                    self.mark(at + token.start, at + token.end, executable);
                    i += 1;
                }
                TokenKind::String | TokenKind::QuotedIdentifier => {
                    self.mark(at + token.start, at + token.end, true);
                    self.blocks.token(token.kind, &line[token.start..token.end]);
                    i += 1;
                }
                _ => {
                    // A run of code with nothing between its tokens: a
                    // delimiter has no whitespace in it and never starts in a
                    // string or a comment, so it lies inside one of these.
                    let first = i;
                    let run_start = token.start;
                    let mut run_end = token.end;
                    i += 1;
                    while let Some(next) = tokens.get(i) {
                        if next.start != run_end
                            || matches!(
                                next.kind,
                                TokenKind::Whitespace
                                    | TokenKind::Comment
                                    | TokenKind::String
                                    | TokenKind::QuotedIdentifier
                            )
                        {
                            break;
                        }
                        run_end = next.end;
                        i += 1;
                    }
                    if batch {
                        self.mark(at + run_start, at + run_end, true);
                    } else {
                        self.run(line, &tokens[first..i], at, out)?;
                    }
                }
            }
        }
        Ok(())
    }

    /// Reads a run of code — `tokens`, of `line`, which starts at `at` — cutting
    /// at each terminator.
    fn run<O: Out>(
        &mut self,
        line: &str,
        tokens: &[Token],
        at: usize,
        out: &mut O,
    ) -> Result<(), ScriptError> {
        if self.delimiter == ";" {
            for token in tokens {
                let text = &line[token.start..token.end];
                if token.kind == TokenKind::Punctuation && text == ";" {
                    self.semicolon(at + token.start, at + token.end, out)?;
                } else {
                    self.mark(at + token.start, at + token.end, true);
                    self.blocks.token(token.kind, text);
                }
            }
            return Ok(());
        }

        // A delimiter the script chose ends a statement wherever it appears,
        // whatever the lexer made of it: `END$$` is one word to MySQL's lexer.
        let (Some(first), Some(last)) = (tokens.first(), tokens.last()) else {
            return Ok(());
        };
        let text = &line[first.start..last.end];
        let at = at + first.start;
        let mut cursor = 0;
        while let Some(found) = text[cursor..].find(self.delimiter.as_str()) {
            let p = cursor + found;
            if p > cursor {
                self.mark(at + cursor, at + p, true);
            }
            let after = p + self.delimiter.len();
            self.closed = Some(at + after);
            self.emit(out)?;
            cursor = after;
        }
        if cursor < text.len() {
            self.mark(at + cursor, at + text.len(), true);
        }
        Ok(())
    }

    /// Reads the `;` at `start..end`: the end of the statement, or a part of
    /// its body.
    fn semicolon<O: Out>(
        &mut self,
        start: usize,
        end: usize,
        out: &mut O,
    ) -> Result<(), ScriptError> {
        if self.start.is_none() || !self.code {
            // `;;`, or a comment with nothing under it.
            return self.emit(out);
        }
        match self.blocks.semicolon() {
            Semicolon::Inside => {
                self.mark(start, end, true);
                Ok(())
            }
            Semicolon::Ends => {
                self.closed = Some(end);
                self.emit(out)
            }
            Semicolon::EndsWithin => {
                self.mark(start, end, true);
                self.closed = Some(end);
                self.emit(out)
            }
        }
    }

    /// Records the bytes `start..end` as part of the statement being read.
    fn mark(&mut self, start: usize, end: usize, code: bool) {
        if self.start.is_none() {
            let line = self.line;
            self.start = Some((start, line));
        }
        self.end = end;
        self.code |= code;
    }

    /// Forgets the statement being read.
    fn discard(&mut self) {
        self.start = None;
        self.code = false;
        self.closed = None;
        self.blocks.reset();
    }

    /// Hands out the statement being read, if it holds any SQL, and starts
    /// the next.
    fn emit<O: Out>(&mut self, out: &mut O) -> Result<(), ScriptError> {
        let closed = self.closed;
        let (Some((start, line)), true) = (self.start, self.code) else {
            self.discard();
            return Ok(());
        };
        let sql = self.buf[start - self.origin..self.end - self.origin].to_string();
        let range = start..self.end;
        let end = closed.unwrap_or(range.end);
        self.discard();
        if self.client == Client::Psql && copies_from_stdin(&sql, &self.dialect) {
            // Its rows follow it; an editor steps over them to their `\.`.
            self.copy_rows = O::UNITS;
            return out.refuse(
                ScriptError {
                    line,
                    range,
                    kind: ScriptErrorKind::CopyFromStdin,
                },
                end,
            );
        }
        out.statement(ScriptStatement { sql, range, line }, end);
        Ok(())
    }

    /// Hands out the `sqlcmd` batch being read, `count` times over.
    fn flush_batch<O: Out>(&mut self, count: u32, out: &mut O) -> Result<(), ScriptError> {
        let (Some((start, line)), true) = (self.start, self.code) else {
            self.discard();
            return Ok(());
        };
        let text = &self.buf[start - self.origin..self.end - self.origin];
        if O::UNITS {
            // An editor's statements, not the batch a run sends: a body is
            // still kept whole, and a routine still runs to the `GO`.
            for span in split_statements(text, &self.dialect) {
                out.statement(
                    ScriptStatement {
                        sql: span.sql(text).to_string(),
                        range: start + span.start..start + span.sql_end,
                        line: line + text[..span.start].matches('\n').count(),
                    },
                    start + span.end,
                );
            }
            self.discard();
            return Ok(());
        }
        let pieces: Vec<ScriptStatement> = if holds_procedural_block(text, &self.dialect) {
            vec![ScriptStatement {
                sql: text.to_string(),
                range: start..self.end,
                line,
            }]
        } else {
            split_statements(text, &self.dialect)
                .into_iter()
                .map(|span| ScriptStatement {
                    sql: span.sql(text).to_string(),
                    range: start + span.start..start + span.sql_end,
                    line: line + text[..span.start].matches('\n').count(),
                })
                .collect()
        };
        for _ in 0..count {
            for piece in &pieces {
                let end = piece.range.end;
                out.statement(piece.clone(), end);
            }
        }
        self.discard();
        Ok(())
    }

    /// The `mysql` client's commands.
    fn mysql_command(&mut self, text: &str) -> Result<Option<Directive>, ScriptErrorKind> {
        // `mariadb-dump`'s first line: a command for its own client, behind a
        // version no server reaches.
        if text.starts_with("/*M!999999\\-") || text.starts_with("/*!999999\\-") {
            return Ok(Some(Directive::Skip));
        }
        if let Some(short) = text.strip_prefix('\\') {
            let mut chars = short.chars();
            let Some(command) = chars.next() else {
                return Ok(None);
            };
            let rest = chars.as_str().trim();
            return match command {
                'd' => self.set_delimiter(rest).map(Some),
                'q' => Ok(Some(Directive::End)),
                'u' => Ok(Some(Directive::Run(format!(
                    "USE {}",
                    rest.trim_end_matches(';')
                )))),
                'W' | 'w' | 'T' | 't' | 'R' | 'C' | 's' | '#' | 'P' | 'n' | 'c' | 'p' | 'g'
                | 'G' => Ok(Some(Directive::Skip)),
                _ => Err(ScriptErrorKind::Unsupported(format!("\\{command}"))),
            };
        }
        let (word, rest) = first_word(text);
        let upper = word.trim_end_matches(';').to_ascii_uppercase();
        match upper.as_str() {
            "DELIMITER" => self.set_delimiter(rest).map(Some),
            "QUIT" | "EXIT" => Ok(Some(Directive::End)),
            "SOURCE" | "CONNECT" | "SYSTEM" => Err(ScriptErrorKind::Unsupported(word.to_string())),
            "TEE" | "NOTEE" | "WARNINGS" | "NOWARNING" | "PROMPT" | "CHARSET" | "STATUS"
            | "PAGER" | "NOPAGER" | "REHASH" => Ok(Some(Directive::Skip)),
            _ => Ok(None),
        }
    }

    /// The Db2 CLP's one directive a script can carry.
    fn clp_command(&mut self, text: &str) -> Result<Option<Directive>, ScriptErrorKind> {
        let Some(rest) = strip_prefix_ignore_case(text, "--#SET") else {
            return Ok(None);
        };
        let (option, value) = first_word(rest.trim_start());
        if !option.eq_ignore_ascii_case("TERMINATOR") {
            return Ok(Some(Directive::Skip));
        }
        self.set_delimiter(value).map(Some)
    }

    /// Takes the first word of `rest` as the new terminator.
    fn set_delimiter(&mut self, rest: &str) -> Result<Directive, ScriptErrorKind> {
        let (delimiter, _) = first_word(rest.trim());
        if delimiter.is_empty() {
            return Err(ScriptErrorKind::MissingDelimiter);
        }
        self.delimiter = delimiter.to_string();
        Ok(Directive::Skip)
    }

    /// SQL\*Plus's commands, which may be shortened to their first few
    /// letters.
    fn sqlplus_command(&mut self, text: &str) -> Result<Option<Directive>, ScriptErrorKind> {
        if text.starts_with('@') || text.starts_with('!') || text.starts_with('$') {
            let (word, _) = first_word(text);
            return Err(ScriptErrorKind::Unsupported(word.to_string()));
        }
        let (word, rest) = first_word(text);
        let word = word.trim_end_matches(';');
        let upper = word.to_ascii_uppercase();
        let is = |full: &str, shortest: usize| upper.len() >= shortest && full.starts_with(&upper);

        if is("SET", 3) {
            let (option, _) = first_word(rest);
            let option = option.trim_end_matches(';').to_ascii_uppercase();
            if ["TRANSACTION", "ROLE", "CONSTRAINT", "CONSTRAINTS"].contains(&option.as_str()) {
                return Ok(None);
            }
            return Ok(Some(self.skip_continued(text)));
        }
        if is("REMARK", 3) {
            return Ok(Some(Directive::Skip));
        }
        if is("PROMPT", 3)
            || is("SHOW", 3)
            || is("COLUMN", 3)
            || is("TTITLE", 3)
            || is("BTITLE", 3)
            || is("REPHEADER", 4)
            || is("REPFOOTER", 4)
            || is("BREAK", 3)
            || is("COMPUTE", 4)
            || is("CLEAR", 2)
            || is("PAUSE", 3)
            || is("SPOOL", 3)
            || is("TIMING", 4)
            || upper == "WHENEVER"
            || is("DESCRIBE", 4)
            || is("PRINT", 3)
            || is("UNDEFINE", 5)
            || is("ATTRIBUTE", 4)
        {
            return Ok(Some(self.skip_continued(text)));
        }
        if upper == "EXIT" || upper == "QUIT" {
            return Ok(Some(Directive::End));
        }
        if is("EXECUTE", 4) {
            let call = rest.trim().trim_end_matches(';').trim_end();
            return Ok(Some(Directive::Run(format!("BEGIN {call}; END;"))));
        }
        if is("DEFINE", 3) {
            return if rest.trim().trim_end_matches(';').is_empty() {
                Ok(Some(Directive::Skip))
            } else {
                Err(ScriptErrorKind::Unsupported(word.to_string()))
            };
        }
        if is("ACCEPT", 3)
            || is("VARIABLE", 3)
            || is("CONNECT", 4)
            || is("DISCONNECT", 4)
            || is("HOST", 2)
            || is("START", 3)
            || upper == "GET"
            || is("SAVE", 3)
            || is("EDIT", 2)
            || upper == "DEL"
            || is("PASSWORD", 5)
            || upper == "STARTUP"
            || upper == "SHUTDOWN"
            || upper == "RECOVER"
        {
            return Err(ScriptErrorKind::Unsupported(word.to_string()));
        }
        Ok(None)
    }

    /// Skips a SQL\*Plus command, and the lines it continues onto with a
    /// trailing `-`.
    fn skip_continued(&mut self, text: &str) -> Directive {
        self.continued = continues(text);
        Directive::Skip
    }
}

/// Runs the splitter over a whole script.
///
/// # Errors
///
/// The first line that cannot be run, as [`ScriptSplitter::push_line`]
/// reports it.
pub fn split_script(source: &str, dialect: &Dialect) -> Result<Vec<ScriptStatement>, ScriptError> {
    let mut splitter = ScriptSplitter::new(dialect);
    let mut out = Vec::new();
    for line in source.split_inclusive('\n') {
        splitter.push_line(line, &mut out)?;
    }
    splitter.finish(&mut out)?;
    Ok(out)
}

/// The statements of `source` as an editor shows them, in order: what a
/// cursor can sit in, and what running each one alone sends.
///
/// Cut by the same rules as [`split_script`], with three differences for a
/// reader that wants every statement rather than a run: a line the run would
/// refuse is a unit of its own, its [`ScriptError`] in [`ScriptUnit::run`],
/// and the reading goes on past it; an `EXIT` ends nothing; and a `sqlcmd`
/// batch is cut into statements rather than sent whole. A client's own lines
/// that run nothing — a `DELIMITER`, a `GO`, a `SET DEFINE OFF` — belong to no
/// unit.
///
/// ```
/// use rudbman_sql::{Dialect, script_units};
///
/// let script = "DELIMITER //\nCREATE PROCEDURE p() BEGIN SELECT 1; END//\nDELIMITER ;\nCALL p();";
/// let units = script_units(script, &Dialect::MYSQL);
/// assert_eq!(units.len(), 2);
/// assert_eq!(&script[units[0].range.clone()], "CREATE PROCEDURE p() BEGIN SELECT 1; END//");
/// assert_eq!(units[0].run.as_ref().unwrap().sql, "CREATE PROCEDURE p() BEGIN SELECT 1; END");
/// assert_eq!(&script[units[1].range.clone()], "CALL p();");
/// ```
pub fn script_units(source: &str, dialect: &Dialect) -> Vec<ScriptUnit> {
    let mut splitter = ScriptSplitter::new(dialect);
    let mut out = Vec::new();
    for line in source.split_inclusive('\n') {
        // Never an error: refusals are units.
        let _ = splitter.read_line(line, &mut out);
    }
    let _ = splitter.end_script(&mut out);
    out
}

/// Whether `source` holds a line its client would read as a command of its
/// own: a `DELIMITER`, a `GO`, a `/`, a skipped command, or one it would
/// refuse.
pub fn has_client_directives(source: &str, dialect: &Dialect) -> bool {
    let mut splitter = ScriptSplitter::new(dialect);
    let mut sink = Vec::new();
    for line in source.split_inclusive('\n') {
        if splitter.push_line(line, &mut sink).is_err() || splitter.saw_directives() {
            return true;
        }
        sink.clear();
    }
    false
}

/// psql's meta-commands.
fn psql_command(text: &str) -> Result<Option<Directive>, ScriptErrorKind> {
    let Some(command) = text.strip_prefix('\\') else {
        return Ok(None);
    };
    let (word, rest) = first_word(command);
    match word {
        // `pg_dump` brackets its output in `\restrict`/`\unrestrict` with a
        // random key, which only stops psql from running meta-commands a
        // malicious server slipped into the dump. Nothing here runs them.
        "restrict" | "unrestrict" | "echo" | "qecho" | "warn" | "timing" | "pset" | "encoding"
        | "conninfo" | "x" | "a" | "t" | "o" | "out" => Ok(Some(Directive::Skip)),
        "q" | "quit" => Ok(Some(Directive::End)),
        "set" => {
            let (name, _) = first_word(rest);
            match name.to_ascii_uppercase().as_str() {
                "ON_ERROR_STOP" | "ECHO" | "ECHO_HIDDEN" | "QUIET" | "VERBOSITY"
                | "SHOW_CONTEXT" => Ok(Some(Directive::Skip)),
                _ => Err(ScriptErrorKind::Unsupported(format!("\\set {name}"))),
            }
        }
        _ => Err(ScriptErrorKind::Unsupported(format!("\\{word}"))),
    }
}

/// `sqlcmd`'s `:` commands and `!!`.
fn sqlcmd_command(text: &str) -> Result<Option<Directive>, ScriptErrorKind> {
    if text.starts_with("!!") {
        return Err(ScriptErrorKind::Unsupported("!!".to_string()));
    }
    let Some(command) = text.strip_prefix(':') else {
        return Ok(None);
    };
    let (word, _) = first_word(command);
    match word.to_ascii_lowercase().as_str() {
        // `:on error exit` is what this reader does anyway, and `ignore` is
        // what the restore's own "continue past errors" is for.
        "on" | "reset" | "list" | "listvar" | "help" | "serverlist" => Ok(Some(Directive::Skip)),
        "exit" | "quit" => Ok(Some(Directive::End)),
        _ => Err(ScriptErrorKind::Unsupported(format!(":{word}"))),
    }
}

/// The `sqlite3` shell's dot-commands.
fn sqlite_command(text: &str) -> Result<Option<Directive>, ScriptErrorKind> {
    if !text.starts_with('.') {
        return Ok(None);
    }
    let (word, _) = first_word(text);
    Err(ScriptErrorKind::Unsupported(word.to_string()))
}

/// `GO`, or `GO 5`, alone on its line: the batch count, or `None` for any
/// other line.
fn go_count(text: &str) -> Option<u32> {
    let (word, rest) = first_word(text);
    if !word.eq_ignore_ascii_case("GO") {
        return None;
    }
    let rest = match rest.find("--") {
        Some(comment) => &rest[..comment],
        None => rest,
    }
    .trim();
    if rest.is_empty() {
        return Some(1);
    }
    rest.parse::<u32>().ok().map(|count| count.max(1))
}

/// The first whitespace-separated word of `text`, and what follows it.
fn first_word(text: &str) -> (&str, &str) {
    let text = text.trim_start();
    match text.find(char::is_whitespace) {
        Some(end) => (&text[..end], &text[end..]),
        None => (text, ""),
    }
}

fn starts_with_ignore_case(text: &str, prefix: &str) -> bool {
    text.len() >= prefix.len()
        && text.is_char_boundary(prefix.len())
        && text[..prefix.len()].eq_ignore_ascii_case(prefix)
}

fn strip_prefix_ignore_case<'a>(text: &'a str, prefix: &str) -> Option<&'a str> {
    starts_with_ignore_case(text, prefix).then(|| &text[prefix.len()..])
}

/// Whether a SQL\*Plus command line goes on to the next one.
fn continues(line: &str) -> bool {
    let line = line.trim_end();
    line == "-" || line.ends_with(" -") || line.ends_with("\t-")
}

/// The first `n` words of `sql`, upper-cased, comments and strings stepped
/// over.
fn leading_words(sql: &str, dialect: &Dialect, n: usize) -> Vec<String> {
    Lexer::new(sql, dialect)
        .filter(|token| !token.kind.is_trivia())
        .take(n)
        .map(|token| {
            if matches!(
                token.kind,
                TokenKind::Keyword | TokenKind::Identifier | TokenKind::Function | TokenKind::Type
            ) {
                token.text(sql).to_ascii_uppercase()
            } else {
                String::new()
            }
        })
        .collect()
}

/// Whether `sql` is `COPY … FROM stdin`.
fn copies_from_stdin(sql: &str, dialect: &Dialect) -> bool {
    if leading_words(sql, dialect, 1)
        .first()
        .is_none_or(|word| word != "COPY")
    {
        return false;
    }
    leading_words(sql, dialect, usize::MAX)
        .windows(2)
        .any(|pair| pair[0] == "FROM" && pair[1] == "STDIN")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The `sql` of every statement of `script`.
    fn sqls(script: &str, dialect: &Dialect) -> Vec<String> {
        split_script(script, dialect)
            .unwrap_or_else(|error| panic!("{error}"))
            .into_iter()
            .map(|statement| statement.sql)
            .collect()
    }

    /// The error `script` stops at.
    fn refusal(script: &str, dialect: &Dialect) -> ScriptError {
        split_script(script, dialect).expect_err("the script should be refused")
    }

    #[test]
    fn plain_scripts_split_as_the_semicolon_splitter_does() {
        let script = "-- first\nselect 1; select 2;\n\nselect ';' from t\n";
        assert_eq!(
            sqls(script, &Dialect::H2),
            ["-- first\nselect 1", "select 2", "select ';' from t"]
        );
        for dialect in [Dialect::H2, Dialect::POSTGRES, Dialect::GENERIC] {
            let spans: Vec<_> = split_statements(script, &dialect)
                .into_iter()
                .map(|span| span.sql_range())
                .collect();
            let ranges: Vec<_> = split_script(script, &dialect)
                .unwrap()
                .into_iter()
                .map(|statement| statement.range)
                .collect();
            assert_eq!(spans, ranges, "{dialect:?}");
        }
    }

    #[test]
    fn ranges_and_lines_point_into_the_script() {
        let script = "select 1;\n\n  select\n  2;";
        let statements = split_script(script, &Dialect::POSTGRES).unwrap();
        assert_eq!(statements.len(), 2);
        assert_eq!(&script[statements[1].range.clone()], "select\n  2");
        assert_eq!(statements[1].line, 3);
        assert_eq!(statements[0].line, 1);
    }

    #[test]
    fn a_statement_across_lines_and_a_quote_across_lines_stay_whole() {
        let script = "insert into t values ('a;\nb');\ncreate function f() returns int as $$\n\
                      begin\n  return 1;\nend;\n$$ language plpgsql;\n";
        assert_eq!(
            sqls(script, &Dialect::POSTGRES),
            [
                "insert into t values ('a;\nb')",
                "create function f() returns int as $$\nbegin\n  return 1;\nend;\n$$ language plpgsql",
            ]
        );
    }

    #[test]
    fn mysql_delimiters_change_the_terminator() {
        let script = "DELIMITER //\nCREATE TRIGGER t BEFORE INSERT ON x FOR EACH ROW\nBEGIN\n  \
                      SET NEW.a = 1;\nEND//\ndelimiter ;\nINSERT INTO x VALUES (1);\n\
                      DELIMITER $$\nCREATE PROCEDURE p() BEGIN SELECT 1; END$$\n";
        assert_eq!(
            sqls(script, &Dialect::MYSQL),
            [
                "CREATE TRIGGER t BEFORE INSERT ON x FOR EACH ROW\nBEGIN\n  SET NEW.a = 1;\nEND",
                "INSERT INTO x VALUES (1)",
                "CREATE PROCEDURE p() BEGIN SELECT 1; END",
            ]
        );
        assert!(has_client_directives(script, &Dialect::MYSQL));
    }

    #[test]
    fn mysqldump_executable_comments_are_statements() {
        let script = "/*M!999999\\- enable the sandbox mode */ \n\
                      -- MariaDB dump 10.19\n\
                      /*!40101 SET @OLD_CHARACTER_SET_CLIENT=@@CHARACTER_SET_CLIENT */;\n\
                      /*!40014 SET @OLD_UNIQUE_CHECKS=@@UNIQUE_CHECKS, UNIQUE_CHECKS=0 */;\n\
                      DROP TABLE IF EXISTS `t`;\n\
                      /*!40101 SET character_set_client = utf8mb4 */;\n\
                      LOCK TABLES `t` WRITE;\n\
                      INSERT INTO `t` VALUES (1,'it\\'s; fine'),(2,\"two\");\n\
                      DELIMITER ;;\n\
                      /*!50003 CREATE*/ /*!50017 DEFINER=`root`@`%`*/ /*!50003 TRIGGER `tr` \
                      BEFORE INSERT ON `t` FOR EACH ROW BEGIN SET NEW.a = 1; SET NEW.b = 2; END */;;\n\
                      DELIMITER ;\n";
        let statements = sqls(script, &Dialect::MARIADB);
        assert_eq!(statements.len(), 7, "{statements:#?}");
        assert_eq!(
            statements[0],
            "-- MariaDB dump 10.19\n/*!40101 SET @OLD_CHARACTER_SET_CLIENT=@@CHARACTER_SET_CLIENT */"
        );
        assert_eq!(statements[4], "LOCK TABLES `t` WRITE");
        assert_eq!(
            statements[5],
            "INSERT INTO `t` VALUES (1,'it\\'s; fine'),(2,\"two\")"
        );
        assert!(statements[6].starts_with("/*!50003 CREATE*/"));
        assert!(statements[6].ends_with("END */"));
    }

    #[test]
    fn mysql_client_commands() {
        assert_eq!(
            sqls("\\u shop\nselect 1;\nquit\nselect 2;", &Dialect::MYSQL),
            ["USE shop", "select 1"]
        );
        let error = refusal("select 1;\nsource other.sql\n", &Dialect::MYSQL);
        assert_eq!(error.line, 2);
        assert_eq!(error.kind, ScriptErrorKind::Unsupported("source".into()));
        assert_eq!(
            refusal("DELIMITER\n", &Dialect::MYSQL).kind,
            ScriptErrorKind::MissingDelimiter
        );
        // A word that only looks like a command, in the middle of a
        // statement, is SQL.
        assert_eq!(
            sqls("select\nstatus from t;", &Dialect::MYSQL),
            ["select\nstatus from t"]
        );
    }

    #[test]
    fn psql_meta_commands() {
        let script = "\\restrict 1hBcWbGmx2\nSET statement_timeout = 0;\n\
                      SELECT pg_catalog.set_config('search_path', '', false);\n\
                      \\set ON_ERROR_STOP on\nCREATE TABLE public.t (id integer);\n\
                      \\unrestrict 1hBcWbGmx2\n";
        assert_eq!(
            sqls(script, &Dialect::POSTGRES),
            [
                "SET statement_timeout = 0",
                "SELECT pg_catalog.set_config('search_path', '', false)",
                "CREATE TABLE public.t (id integer)",
            ]
        );
        let error = refusal("select 1;\n\\connect other\n", &Dialect::POSTGRES);
        assert_eq!(error.kind, ScriptErrorKind::Unsupported("\\connect".into()));
        assert_eq!(error.line, 2);
        assert_eq!(
            refusal("\\set x 1\n", &Dialect::POSTGRES).kind,
            ScriptErrorKind::Unsupported("\\set x".into())
        );
        assert_eq!(
            sqls("select 1;\n\\q\nselect 2;\n", &Dialect::POSTGRES),
            ["select 1"]
        );
    }

    #[test]
    fn copy_from_stdin_is_refused_where_it_starts() {
        let script = "SET x = 1;\nCOPY public.t (id, name) FROM stdin;\n1\tone\n\\.\n";
        let error = refusal(script, &Dialect::POSTGRES);
        assert_eq!(error.kind, ScriptErrorKind::CopyFromStdin);
        assert_eq!(error.line, 2);
        // COPY from a file on the server is an ordinary statement.
        assert_eq!(
            sqls("COPY t FROM '/tmp/t.csv';", &Dialect::POSTGRES),
            ["COPY t FROM '/tmp/t.csv'"]
        );
    }

    #[test]
    fn sqlcmd_batches_end_at_go() {
        let script = "SET ANSI_NULLS ON\nGO\nSET QUOTED_IDENTIFIER ON\ngo\n\
                      CREATE PROCEDURE dbo.p AS\nBEGIN\n  SELECT 1;\n  SELECT 2;\nEND\nGO\n\
                      INSERT INTO t VALUES (1); INSERT INTO t VALUES (2)\nGO 2\n\
                      select 'go'\n  GO  -- the end\n";
        assert_eq!(
            sqls(script, &Dialect::MSSQL),
            [
                "SET ANSI_NULLS ON",
                "SET QUOTED_IDENTIFIER ON",
                "CREATE PROCEDURE dbo.p AS\nBEGIN\n  SELECT 1;\n  SELECT 2;\nEND",
                "INSERT INTO t VALUES (1)",
                "INSERT INTO t VALUES (2)",
                "INSERT INTO t VALUES (1)",
                "INSERT INTO t VALUES (2)",
                "select 'go'",
            ]
        );
        let statements = split_script(script, &Dialect::MSSQL).unwrap();
        assert_eq!(statements[2].line, 5);
        assert_eq!(statements[4].line, 11);
        // A `GO` inside a comment is part of the comment.
        assert_eq!(
            sqls("select 1\n/*\nGO\n*/\nselect 2", &Dialect::MSSQL),
            ["select 1\n/*\nGO\n*/\nselect 2"]
        );
    }

    #[test]
    fn sqlcmd_commands() {
        assert_eq!(
            sqls(
                ":on error exit\nselect 1\nGO\nselect 2\nEXIT\nselect 3",
                &Dialect::MSSQL
            ),
            ["select 1", "select 2"]
        );
        let error = refusal("select 1\nGO\n:setvar db x\n", &Dialect::MSSQL);
        assert_eq!(error.kind, ScriptErrorKind::Unsupported(":setvar".into()));
        assert_eq!(error.line, 3);
    }

    #[test]
    fn sqlplus_reads_plsql_to_the_slash() {
        let script = "SET DEFINE OFF\nSET SQLBLANKLINES ON\nREM a remark\n\
                      CREATE TABLE t (id NUMBER);\n\
                      CREATE OR REPLACE PROCEDURE p AS\nBEGIN\n  INSERT INTO t VALUES (1);\n\n  \
                      COMMIT;\nEND;\n/\n\
                      INSERT INTO t VALUES (2);\n/\n\
                      BEGIN DBMS_UTILITY.COMPILE_SCHEMA(USER, FALSE); END;\n/\n\
                      create or replace editionable package pkg as\n  procedure q;\nend pkg;\n/\n\
                      select * from t\n/\n";
        assert_eq!(
            sqls(script, &Dialect::ORACLE),
            [
                "CREATE TABLE t (id NUMBER)",
                "CREATE OR REPLACE PROCEDURE p AS\nBEGIN\n  INSERT INTO t VALUES (1);\n\n  COMMIT;\nEND;",
                "INSERT INTO t VALUES (2)",
                "BEGIN DBMS_UTILITY.COMPILE_SCHEMA(USER, FALSE); END;",
                "create or replace editionable package pkg as\n  procedure q;\nend pkg;",
                "select * from t",
            ]
        );
    }

    #[test]
    fn sqlplus_commands() {
        let script = "set define off;\nprompt Loading -\n  the rows\nexec dbms_stats.gather_schema_stats(user);\n\
                      set transaction read only;\nwhenever sqlerror exit failure\n\
                      select 1 from dual;\nexit\nselect 2 from dual;\n";
        assert_eq!(
            sqls(script, &Dialect::ORACLE),
            [
                "BEGIN dbms_stats.gather_schema_stats(user); END;",
                "set transaction read only",
                "select 1 from dual",
            ]
        );
        assert_eq!(
            refusal("@other.sql\n", &Dialect::ORACLE).kind,
            ScriptErrorKind::Unsupported("@other.sql".into())
        );
        assert_eq!(
            refusal("define owner = 'APP'\n", &Dialect::ORACLE).kind,
            ScriptErrorKind::Unsupported("define".into())
        );
        // An unterminated block at the end of an editor buffer still runs.
        assert_eq!(
            sqls("begin\n  null;\nend;", &Dialect::ORACLE),
            ["begin\n  null;\nend;"]
        );
        // `DELETE` is not `DEL`, nor `COMMENT` `COMPUTE`.
        assert_eq!(
            sqls(
                "delete from t;\ncomment on table t is 'x';",
                &Dialect::ORACLE
            ),
            ["delete from t", "comment on table t is 'x'"]
        );
    }

    #[test]
    fn db2_terminators() {
        let script = "CREATE TABLE t (id INT);\n--#SET TERMINATOR @\n\
                      CREATE PROCEDURE p() BEGIN INSERT INTO t VALUES (1); END@\n\
                      --#SET TERMINATOR ;\nINSERT INTO t VALUES (2);\n";
        assert_eq!(
            sqls(script, &Dialect::DB2),
            [
                "CREATE TABLE t (id INT)",
                "CREATE PROCEDURE p() BEGIN INSERT INTO t VALUES (1); END",
                "INSERT INTO t VALUES (2)",
            ]
        );
    }

    #[test]
    fn sqlite_triggers_end_at_end() {
        let script = "PRAGMA foreign_keys=OFF;\nBEGIN TRANSACTION;\nCREATE TABLE t(a);\n\
                      CREATE TRIGGER tr AFTER INSERT ON t BEGIN\n  UPDATE t SET a = 1;\n  \
                      DELETE FROM t WHERE a = 2;\nEND;\nCOMMIT;\n";
        assert_eq!(
            sqls(script, &Dialect::SQLITE),
            [
                "PRAGMA foreign_keys=OFF",
                "BEGIN TRANSACTION",
                "CREATE TABLE t(a)",
                "CREATE TRIGGER tr AFTER INSERT ON t BEGIN\n  UPDATE t SET a = 1;\n  DELETE FROM t WHERE a = 2;\nEND",
                "COMMIT",
            ]
        );
        assert_eq!(
            refusal(".read other.sql\n", &Dialect::SQLITE).kind,
            ScriptErrorKind::Unsupported(".read".into())
        );
    }

    #[test]
    fn mysql_routines_typed_without_a_delimiter_stay_whole() {
        // What a person types in the editor: no `DELIMITER`, which only the
        // `mysql` client understands, around a routine JDBC sends whole.
        let script = "CREATE PROCEDURE p(IN n INT)\nBEGIN\n  IF n > 0 THEN\n    SELECT 1;\n  \
                      END IF;\n  SELECT CASE WHEN n > 1 THEN 2 END;\nEND;\nCALL p(1);\n";
        assert_eq!(
            sqls(script, &Dialect::MYSQL),
            [
                "CREATE PROCEDURE p(IN n INT)\nBEGIN\n  IF n > 0 THEN\n    SELECT 1;\n  END IF;\n  \
                 SELECT CASE WHEN n > 1 THEN 2 END;\nEND",
                "CALL p(1)",
            ]
        );
        let trigger = "CREATE TRIGGER t BEFORE INSERT ON x FOR EACH ROW\nBEGIN\n  \
                       SET NEW.a = 1;\n  SET NEW.b = 2;\nEND;\nINSERT INTO x VALUES (1);";
        assert_eq!(sqls(trigger, &Dialect::MARIADB).len(), 2);
        let compound = "BEGIN NOT ATOMIC\n  SELECT 1;\n  SELECT 2;\nEND;\nSELECT 3;";
        assert_eq!(sqls(compound, &Dialect::MARIADB).len(), 2);
    }

    #[test]
    fn postgres_atomic_bodies_stay_whole() {
        let script = "CREATE FUNCTION one() RETURNS int LANGUAGE sql\nBEGIN ATOMIC\n  \
                      SELECT 1;\nEND;\nBEGIN;\nSELECT one();\nCOMMIT;\n";
        assert_eq!(
            sqls(script, &Dialect::POSTGRES),
            [
                "CREATE FUNCTION one() RETURNS int LANGUAGE sql\nBEGIN ATOMIC\n  SELECT 1;\nEND",
                "BEGIN",
                "SELECT one()",
                "COMMIT",
            ]
        );
    }

    #[test]
    fn plsql_typed_without_its_slash_ends_at_its_end() {
        // SQL*Plus would wait for a `/` and take the `SELECT` into the block.
        let script = "CREATE OR REPLACE PROCEDURE p AS\n  n NUMBER;\nBEGIN\n  \
                      SELECT COUNT(*) INTO n FROM t;\n  IF n > 0 THEN\n    NULL;\n  END IF;\n\
                      END p;\nSELECT 1 FROM dual;\n\
                      DECLARE\n  v NUMBER;\nBEGIN\n  v := 1;\nEND;\n\
                      CREATE OR REPLACE PACKAGE BODY pkg AS\n  PROCEDURE q IS\n  BEGIN\n    \
                      NULL;\n  END q;\nEND pkg;\n";
        assert_eq!(
            sqls(script, &Dialect::ORACLE),
            [
                "CREATE OR REPLACE PROCEDURE p AS\n  n NUMBER;\nBEGIN\n  SELECT COUNT(*) INTO n FROM t;\n  \
                 IF n > 0 THEN\n    NULL;\n  END IF;\nEND p;",
                "SELECT 1 FROM dual",
                "DECLARE\n  v NUMBER;\nBEGIN\n  v := 1;\nEND;",
                "CREATE OR REPLACE PACKAGE BODY pkg AS\n  PROCEDURE q IS\n  BEGIN\n    NULL;\n  END q;\nEND pkg;",
            ]
        );
        // The slash still ends what is open, so a body that never closes runs
        // no further than SQL*Plus would run it.
        assert_eq!(
            sqls(
                "CREATE JAVA SOURCE NAMED x AS class X { void f() { a(); } };\n/\nSELECT 1 FROM dual;",
                &Dialect::ORACLE
            ),
            [
                "CREATE JAVA SOURCE NAMED x AS class X { void f() { a(); } };",
                "SELECT 1 FROM dual",
            ]
        );
    }

    #[test]
    fn sqlite_trigger_bodies_count_their_case() {
        let script = "CREATE TRIGGER tr AFTER INSERT ON t BEGIN\n  \
                      UPDATE t SET a = CASE WHEN new.a > 0 THEN 1 END;\n  DELETE FROM t;\nEND;\n\
                      SELECT 1;\n";
        assert_eq!(
            sqls(script, &Dialect::SQLITE),
            [
                "CREATE TRIGGER tr AFTER INSERT ON t BEGIN\n  UPDATE t SET a = CASE WHEN new.a > 0 THEN 1 END;\n  \
                 DELETE FROM t;\nEND",
                "SELECT 1",
            ]
        );
    }

    #[test]
    fn db2_and_h2_bodies_stay_whole() {
        assert_eq!(
            sqls(
                "CREATE PROCEDURE p() BEGIN INSERT INTO t VALUES (1); INSERT INTO t VALUES (2); END;\n\
                 CALL p();",
                &Dialect::DB2
            ),
            [
                "CREATE PROCEDURE p() BEGIN INSERT INTO t VALUES (1); INSERT INTO t VALUES (2); END",
                "CALL p()",
            ]
        );
        assert_eq!(
            sqls(
                "CREATE ALIAS twice AS $$ int twice(int x) { int y = x; return y * 2; } $$;\n\
                 CALL twice(2);",
                &Dialect::H2
            ),
            [
                "CREATE ALIAS twice AS $$ int twice(int x) { int y = x; return y * 2; } $$",
                "CALL twice(2)",
            ]
        );
    }

    /// The text of each unit of `script`, terminator included, and what
    /// running it sends — or the error, as `Err`.
    fn units(script: &str, dialect: &Dialect) -> Vec<(String, Result<String, ScriptErrorKind>)> {
        script_units(script, dialect)
            .into_iter()
            .map(|unit| {
                (
                    script[unit.range.clone()].to_string(),
                    unit.run.map(|run| run.sql).map_err(|error| error.kind),
                )
            })
            .collect()
    }

    #[test]
    fn units_reach_their_terminator_and_skip_the_client_lines() {
        let script = "DELIMITER //\nCREATE PROCEDURE p()\nBEGIN\n  SELECT 1;\nEND//\n\
                      DELIMITER ;\nCALL p();\n";
        assert_eq!(
            units(script, &Dialect::MYSQL),
            [
                (
                    "CREATE PROCEDURE p()\nBEGIN\n  SELECT 1;\nEND//".to_string(),
                    Ok("CREATE PROCEDURE p()\nBEGIN\n  SELECT 1;\nEND".to_string())
                ),
                ("CALL p();".to_string(), Ok("CALL p()".to_string())),
            ]
        );

        let oracle = "SET DEFINE OFF\nBEGIN\n  NULL;\nEND;\n/\nselect 1 from dual\n/\nexec p\n";
        assert_eq!(
            units(oracle, &Dialect::ORACLE),
            [
                (
                    "BEGIN\n  NULL;\nEND;".to_string(),
                    Ok("BEGIN\n  NULL;\nEND;".to_string())
                ),
                (
                    "select 1 from dual\n/".to_string(),
                    Ok("select 1 from dual".to_string())
                ),
                ("exec p".to_string(), Ok("BEGIN p; END;".to_string())),
            ]
        );
    }

    #[test]
    fn units_carry_on_past_what_a_run_refuses() {
        let script = "select 1;\n\\connect other\nselect 2;\n\\q\nselect 3;\n\
                      COPY t FROM stdin;\n1\t'x\n\\.\nselect 4;\n";
        assert_eq!(
            units(script, &Dialect::POSTGRES),
            [
                ("select 1;".to_string(), Ok("select 1".to_string())),
                (
                    "\\connect other".to_string(),
                    Err(ScriptErrorKind::Unsupported("\\connect".into()))
                ),
                ("select 2;".to_string(), Ok("select 2".to_string())),
                ("select 3;".to_string(), Ok("select 3".to_string())),
                (
                    "COPY t FROM stdin;".to_string(),
                    Err(ScriptErrorKind::CopyFromStdin)
                ),
                ("select 4;".to_string(), Ok("select 4".to_string())),
            ]
        );
    }

    #[test]
    fn units_cut_a_sqlcmd_batch_into_its_statements() {
        let script = "select 1;\nif @a = 1\nbegin\n  select 2;\n  select 3;\nend;\nselect 4\nGO\n\
                      create procedure p as\n  select 5;\n  select 6;\nGO\n";
        assert_eq!(
            units(script, &Dialect::MSSQL),
            [
                ("select 1;".to_string(), Ok("select 1".to_string())),
                (
                    "if @a = 1\nbegin\n  select 2;\n  select 3;\nend;".to_string(),
                    Ok("if @a = 1\nbegin\n  select 2;\n  select 3;\nend".to_string())
                ),
                ("select 4".to_string(), Ok("select 4".to_string())),
                (
                    "create procedure p as\n  select 5;\n  select 6;".to_string(),
                    Ok("create procedure p as\n  select 5;\n  select 6;".to_string())
                ),
            ]
        );
        // "Run all" still sends the batch that holds a block whole, as sqlcmd
        // does, so that the variables declared in it stay in scope.
        assert_eq!(sqls(script, &Dialect::MSSQL).len(), 2);
    }

    #[test]
    fn units_and_a_run_cut_alike_where_nothing_is_refused() {
        for (script, dialect) in [
            (
                "-- first\nselect 1; select 2;\n\nselect ';' from t\n",
                Dialect::H2,
            ),
            (
                "DELIMITER ;;\nCREATE PROCEDURE p() BEGIN SELECT 1; END;;\nDELIMITER ;\nCALL p();",
                Dialect::MYSQL,
            ),
            (
                "CREATE OR REPLACE PROCEDURE p AS\nBEGIN\n  NULL;\nEND;\n/\nSELECT 1 FROM dual;",
                Dialect::ORACLE,
            ),
            (
                "CREATE TRIGGER tr AFTER INSERT ON t BEGIN\n  DELETE FROM t;\nEND;\nSELECT 1;",
                Dialect::SQLITE,
            ),
        ] {
            let run = split_script(script, &dialect).unwrap();
            let units: Vec<_> = script_units(script, &dialect)
                .into_iter()
                .map(|unit| unit.run.unwrap())
                .collect();
            assert_eq!(units, run, "{script:?}");
        }
    }

    #[test]
    fn line_by_line_and_all_at_once_agree() {
        let script = "DELIMITER ;;\r\nCREATE PROCEDURE p() BEGIN SELECT 1; END;;\r\n\
                      DELIMITER ;\r\nselect 'a\r\nb';\r\n";
        let whole = split_script(script, &Dialect::MYSQL).unwrap();
        let mut splitter = ScriptSplitter::new(&Dialect::MYSQL);
        let mut fed = Vec::new();
        for line in script.split_inclusive('\n') {
            splitter.push_line(line, &mut fed).unwrap();
        }
        splitter.finish(&mut fed).unwrap();
        assert_eq!(whole, fed);
        assert_eq!(fed[1].sql, "select 'a\r\nb'");
    }

    #[test]
    fn a_script_without_directives_says_so() {
        assert!(!has_client_directives(
            "create procedure p() begin select 1; end",
            &Dialect::MYSQL
        ));
        assert!(has_client_directives("select 1\ngo\n", &Dialect::MSSQL));
        assert!(has_client_directives(
            "begin null; end;\n/\n",
            &Dialect::ORACLE
        ));
        assert!(!has_client_directives(
            "select 1;\nselect 2;",
            &Dialect::ORACLE
        ));
    }
}
