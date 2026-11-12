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
//! | Oracle              | SQL\*Plus  | PL/SQL runs to `/`, SQL\*Plus commands, `EXEC`        |
//! | Db2                 | the CLP    | `--#SET TERMINATOR`                                   |
//! | SQLite              | `sqlite3`  | `CREATE TRIGGER … END;`, `.` commands                  |
//! | H2, anything else   | none       | plain semicolons                                      |
//!
//! None of these needs a parser. Every rule is either a line of its own (a
//! directive, read only where the lexer says no string or comment is open) or
//! a decision taken from the first few words of a statement, the same few
//! words the client itself looks at. That is the difference from the
//! block-structure tracking [`crate::statement`] declines to half-implement:
//! SQL\*Plus does not count `BEGIN` and `END` either. It sees
//! `CREATE PROCEDURE` and waits for the slash.
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
//! * A `/` on a line of its own after a statement that already ran is ignored.
//!   SQL\*Plus would run that statement a second time, which a hand-written
//!   script almost never means.
//! * Blank lines never end a SQL\*Plus statement, as with
//!   `SET SQLBLANKLINES ON`, which is what rudbman's own Oracle backups set.
//! * A semicolon in the middle of a line ends a SQL\*Plus statement too, as
//!   the editor has always split it.
//! * Inside a `sqlcmd` batch, statements are still split on semicolons, so
//!   two `SELECT`s keep their two grids, unless the batch holds a procedural
//!   block ([`holds_procedural_block`]), in which case the batch is sent whole,
//!   as `sqlcmd` sends every batch.
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

use crate::dialect::{Dialect, DialectId};
use crate::lexer::{Lexer, LineState, TokenKind};
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
    /// Whether the statement is a PL/SQL unit (Oracle) or a trigger (SQLite),
    /// once its first terminator has made the question come up.
    block: Option<bool>,
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
            block: None,
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
        let at = self.offset;
        self.offset += line.len();
        self.line += 1;
        if self.ended {
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
    fn directive(
        &mut self,
        line: &str,
        at: usize,
        out: &mut Vec<ScriptStatement>,
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
                    self.ended = true;
                    return Ok(true);
                }
            }
            Client::SqlPlus if text == "/" => {
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
        let directive = directive.map_err(|kind| ScriptError {
            line: self.line,
            range: range.clone(),
            kind,
        })?;
        let Some(directive) = directive else {
            return Ok(false);
        };
        // A comment above a command belongs to nothing that runs.
        self.discard();
        match directive {
            Directive::Skip => {}
            Directive::End => self.ended = true,
            Directive::Run(sql) => out.push(ScriptStatement {
                sql,
                range,
                line: self.line,
            }),
        }
        Ok(true)
    }

    /// Walks `line`'s tokens, emitting each statement that ends on it.
    fn scan(
        &mut self,
        line: &str,
        at: usize,
        out: &mut Vec<ScriptStatement>,
    ) -> Result<(), ScriptError> {
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
                    i += 1;
                }
                _ => {
                    // A run of code with nothing between its tokens: a
                    // delimiter has no whitespace in it and never starts in a
                    // string or a comment, so it lies inside one of these.
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
                        self.run(&line[run_start..run_end], at + run_start, out)?;
                    }
                }
            }
        }
        Ok(())
    }

    /// Reads a run of code that starts at `at`, cutting at each terminator.
    fn run(
        &mut self,
        text: &str,
        at: usize,
        out: &mut Vec<ScriptStatement>,
    ) -> Result<(), ScriptError> {
        let mut cursor = 0;
        while let Some(found) = text[cursor..].find(self.delimiter.as_str()) {
            let p = cursor + found;
            if p > cursor {
                self.mark(at + cursor, at + p, true);
            }
            let after = p + self.delimiter.len();
            if self.terminates(at + p) {
                self.emit(out)?;
            } else {
                self.mark(at + p, at + after, true);
            }
            cursor = after;
        }
        if cursor < text.len() {
            self.mark(at + cursor, at + text.len(), true);
        }
        Ok(())
    }

    /// Whether the terminator at `p` ends the statement being read.
    fn terminates(&mut self, p: usize) -> bool {
        let Some((start, _)) = self.start else {
            return true;
        };
        if !self.code {
            return true;
        }
        let so_far = &self.buf[start - self.origin..p - self.origin];
        match self.client {
            Client::SqlPlus => {
                let dialect = self.dialect;
                !*self
                    .block
                    .get_or_insert_with(|| plsql_unit(so_far, &dialect))
            }
            Client::Sqlite => {
                let dialect = self.dialect;
                let trigger = *self
                    .block
                    .get_or_insert_with(|| sqlite_trigger(so_far, &dialect));
                !trigger || ends_with_word(so_far, "END")
            }
            _ => true,
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
        self.block = None;
    }

    /// Hands out the statement being read, if it holds any SQL, and starts
    /// the next.
    fn emit(&mut self, out: &mut Vec<ScriptStatement>) -> Result<(), ScriptError> {
        let (Some((start, line)), true) = (self.start, self.code) else {
            self.discard();
            return Ok(());
        };
        let sql = self.buf[start - self.origin..self.end - self.origin].to_string();
        let range = start..self.end;
        self.discard();
        if self.client == Client::Psql && copies_from_stdin(&sql, &self.dialect) {
            return Err(ScriptError {
                line,
                range,
                kind: ScriptErrorKind::CopyFromStdin,
            });
        }
        out.push(ScriptStatement { sql, range, line });
        Ok(())
    }

    /// Hands out the `sqlcmd` batch being read, `count` times over.
    fn flush_batch(
        &mut self,
        count: u32,
        out: &mut Vec<ScriptStatement>,
    ) -> Result<(), ScriptError> {
        let (Some((start, line)), true) = (self.start, self.code) else {
            self.discard();
            return Ok(());
        };
        let text = &self.buf[start - self.origin..self.end - self.origin];
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
            out.extend(pieces.iter().cloned());
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

/// Whether `text`, trimmed, ends in the word `word`.
fn ends_with_word(text: &str, word: &str) -> bool {
    let text = text.trim_end();
    if text.len() < word.len() || !text.is_char_boundary(text.len() - word.len()) {
        return false;
    }
    let (head, tail) = text.split_at(text.len() - word.len());
    tail.eq_ignore_ascii_case(word)
        && !head
            .chars()
            .next_back()
            .is_some_and(|c| c.is_alphanumeric() || c == '_' || c == '$' || c == '#')
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

/// Whether a statement starting `so_far` is a PL/SQL unit, which SQL\*Plus
/// reads to its `/`: an anonymous block, or the `CREATE` of a stored unit.
fn plsql_unit(so_far: &str, dialect: &Dialect) -> bool {
    let words = leading_words(so_far, dialect, 8);
    let word = |i: usize| words.get(i).map_or("", String::as_str);
    match word(0) {
        "DECLARE" | "BEGIN" => true,
        "CREATE" => {
            let mut i = 1;
            while matches!(
                word(i),
                "OR" | "REPLACE"
                    | "EDITIONABLE"
                    | "NONEDITIONABLE"
                    | "AND"
                    | "COMPILE"
                    | "RESOLVE"
                    | "NOFORCE"
            ) {
                i += 1;
            }
            matches!(
                word(i),
                "FUNCTION" | "PROCEDURE" | "PACKAGE" | "TRIGGER" | "TYPE" | "LIBRARY" | "JAVA"
            )
        }
        _ => false,
    }
}

/// Whether a statement starting `so_far` is SQLite's `CREATE TRIGGER`, whose
/// body ends at `END;`.
fn sqlite_trigger(so_far: &str, dialect: &Dialect) -> bool {
    let words = leading_words(so_far, dialect, 3);
    let word = |i: usize| words.get(i).map_or("", String::as_str);
    word(0) == "CREATE"
        && (word(1) == "TRIGGER"
            || (matches!(word(1), "TEMP" | "TEMPORARY") && word(2) == "TRIGGER"))
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
