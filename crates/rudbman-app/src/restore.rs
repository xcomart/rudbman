//! Running a script file against a database: what the restore dialog starts.
//!
//! A backup is a script written for the product's own command-line client,
//! and so is what `pg_dump`, `mysqldump`, SSMS and SQL Developer write. A
//! restore reads one the way that client would ([`rudbman_sql::ScriptSplitter`])
//! and sends it a statement at a time.
//!
//! # Two passes
//!
//! The file is read twice. The first pass only splits it: a line the run
//! could not carry out — `\connect`, `SOURCE`, `COPY … FROM stdin`, bytes
//! that are not text — refuses the restore before a single statement has
//! changed the database, and the statement count it leaves behind is what
//! the progress card counts toward. The second pass splits it again and runs
//! it. Splitting is a lexer pass, a small fraction of what executing costs, so
//! reading twice is cheap next to finding out at line 90 000 that the script
//! cannot be finished.
//!
//! # A session of its own
//!
//! The script runs on a second session opened beside the connection's
//! ([`SessionHandle::open_sibling`]), with auto-commit on as every client
//! runs a script. A dump changes the session it runs on — `pg_dump` sets
//! `search_path` to nothing, `mysqldump` turns off foreign key and unique
//! checks, an SSMS script says `USE` — and none of that should outlive the
//! restore in the editors that share the connection's session. That session
//! also stays free for the explorer while a long restore runs.
//!
//! # What a file may be
//!
//! Plain text or gzip, told apart by the gzip magic rather than the name.
//! UTF-8, with or without a byte order mark, or UTF-16 with one, which is
//! what SSMS writes when asked for Unicode. Anything else — a legacy code
//! page, a binary dump — stops the first pass at the first line that is not
//! text. A `pg_dump` archive (`-Fc`) is recognized by its own magic and named,
//! because the answer to it is a different program.

use std::fs::File;
use std::io::{self, BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use parking_lot::Mutex;
use rudbman_jdbc::{Canceller, Error as JdbcError, Session, StatementSpec};
use rudbman_sql::{Dialect, DialectId, ScriptError, ScriptSplitter, ScriptStatement};

use crate::connection::{SessionHandle, SiblingSession};
use crate::explorer::Scope;

/// The most failures a run past errors keeps to show; the rest are counted.
const KEPT_FAILURES: usize = 100;

/// How many characters of a failed statement are kept to show.
const PREVIEW_CHARS: usize = 240;

/// How far a statement's results are walked before it is taken as done: a
/// guard against a driver that never says it has no more.
const MAX_RESULTS: usize = 100_000;

/// What to restore, and how.
#[derive(Clone, Debug)]
pub struct RestoreRequest {
    /// The script.
    pub path: PathBuf,
    /// The dialect the script is read in: the connection's.
    pub dialect: Dialect,
    /// Whether a failed statement is recorded and passed, rather than the end
    /// of the run.
    pub continue_on_error: bool,
    /// What the session runs before the script, so that a name the script
    /// does not qualify lands in the scope the dialog was opened over.
    pub enter: Option<String>,
}

/// Where the restore is.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Phase {
    /// Reading the script through once, without running it.
    #[default]
    Checking,
    /// Opening the session the script runs on.
    Connecting,
    /// Running it.
    Running,
}

/// The latest reading of a restore.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Progress {
    /// Where it is.
    pub phase: Phase,
    /// The script's bytes read so far, as text.
    pub bytes_done: u64,
    /// The script's size as text, once the first pass has measured it.
    pub bytes_total: u64,
    /// Statements run so far, or found so far while checking.
    pub statements_done: u64,
    /// The statements the script holds, once the first pass has counted them.
    pub statements_total: u64,
    /// Statements that failed and were passed.
    pub failed: u64,
}

/// One statement the server refused.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Failure {
    /// Its line in the script, or `0` for the statement that entered the
    /// scope before the script.
    pub line: usize,
    /// The start of its text.
    pub sql: String,
    /// The server's message.
    pub message: String,
}

/// Why a script was not run at all.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Refusal {
    /// A line the run could not carry out.
    Script(ScriptError),
    /// The line is not text in an encoding the reader knows.
    NotText {
        /// Counting from one.
        line: usize,
    },
    /// A `pg_dump` archive rather than a script.
    PgArchive,
    /// The file could not be read.
    Io(String),
}

/// How a restore ended.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// Every statement was sent; `failures` are the ones a run past errors
    /// passed, the first [`KEPT_FAILURES`] of `failed`.
    Done {
        /// Statements that ran.
        statements: u64,
        /// The failures kept to show.
        failures: Vec<Failure>,
        /// How many failed in all.
        failed: u64,
    },
    /// A statement failed and the run stopped there.
    Failed {
        /// Statements that ran before it.
        statements: u64,
        /// The one that failed.
        failure: Failure,
    },
    /// Nothing was run.
    Refused(Refusal),
    /// No session could be opened to run the script on.
    NoSession(String),
    /// The run was stopped.
    Canceled {
        /// Statements that ran before it stopped.
        statements: u64,
    },
}

/// A restore in flight, shared by the thread running it and the dialog
/// watching it.
pub struct Restore {
    progress: Mutex<Progress>,
    outcome: Mutex<Option<Outcome>>,
    cancel: AtomicBool,
    /// The running session's canceller, once it has one.
    canceller: Mutex<Option<Canceller>>,
}

impl Restore {
    /// Starts restoring `request` on a session opened beside `session`, on a
    /// thread of its own: a restore can take hours, which is no job for an
    /// executor thread.
    pub fn start(session: SessionHandle, request: RestoreRequest) -> Arc<Self> {
        let restore = Arc::new(Self {
            progress: Mutex::new(Progress::default()),
            outcome: Mutex::new(None),
            cancel: AtomicBool::new(false),
            canceller: Mutex::new(None),
        });
        let shared = Arc::clone(&restore);
        let spawned = std::thread::Builder::new()
            .name("rudbman-restore".into())
            .spawn(move || {
                let outcome = run(&shared, &request, || session.open_sibling());
                *shared.outcome.lock() = Some(outcome);
            });
        if let Err(error) = spawned {
            *restore.outcome.lock() = Some(Outcome::NoSession(error.to_string()));
        }
        restore
    }

    /// The latest reading.
    pub fn progress(&self) -> Progress {
        self.progress.lock().clone()
    }

    /// How it ended, once it has.
    pub fn outcome(&self) -> Option<Outcome> {
        self.outcome.lock().clone()
    }

    /// Asks the run to stop, interrupting the statement in flight.
    ///
    /// What already ran stays: the script runs under auto-commit, as every
    /// client runs one.
    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Release);
        if let Some(canceller) = self.canceller.lock().as_ref()
            && let Err(error) = canceller.cancel()
        {
            log::debug!("canceling the restore's statement failed: {error}");
        }
    }

    fn canceled(&self) -> bool {
        self.cancel.load(Ordering::Acquire)
    }

    fn update(&self, change: impl FnOnce(&mut Progress)) {
        change(&mut self.progress.lock());
    }
}

/// The restore, start to end, on the session `open` opens.
fn run(
    restore: &Restore,
    request: &RestoreRequest,
    open: impl FnOnce() -> Result<SiblingSession, JdbcError>,
) -> Outcome {
    let (bytes_total, statements_total) = match check(restore, request) {
        Ok(Some(totals)) => totals,
        Ok(None) => return Outcome::Canceled { statements: 0 },
        Err(refusal) => return Outcome::Refused(refusal),
    };
    restore.update(|progress| {
        progress.phase = Phase::Connecting;
        progress.bytes_total = bytes_total;
        progress.statements_total = statements_total;
        progress.bytes_done = 0;
        progress.statements_done = 0;
    });

    let opened = match open() {
        Ok(opened) => opened,
        Err(error) => return Outcome::NoSession(error.to_string()),
    };
    let session = opened.session();
    // Every client runs a script under auto-commit; a profile set to manual
    // commit would otherwise leave the whole restore to be rolled back when
    // the session closes.
    if let Err(error) = session.set_auto_commit(true) {
        return Outcome::NoSession(error.to_string());
    }
    *restore.canceller.lock() = Some(session.canceller());
    if restore.canceled() {
        return Outcome::Canceled { statements: 0 };
    }
    if let Some(enter) = &request.enter
        && let Err(message) = execute(session, enter)
    {
        return Outcome::Failed {
            statements: 0,
            failure: failure(0, enter, message),
        };
    }

    restore.update(|progress| progress.phase = Phase::Running);
    let outcome = execute_script(restore, request, session);
    *restore.canceller.lock() = None;
    if let Err(error) = opened.close() {
        log::debug!("closing the restore's session failed: {error}");
    }
    outcome
}

/// The first pass: the script split without running it. Answers its size as
/// text and its statement count, or `None` when the restore was canceled.
fn check(restore: &Restore, request: &RestoreRequest) -> Result<Option<(u64, u64)>, Refusal> {
    let mut reader = ScriptReader::open(&request.path)?;
    let mut splitter = ScriptSplitter::new(&request.dialect);
    let mut out = Vec::new();
    let (mut bytes, mut statements) = (0u64, 0u64);
    while let Some(line) = reader.next_line()? {
        if restore.canceled() {
            return Ok(None);
        }
        splitter
            .push_line(&line, &mut out)
            .map_err(Refusal::Script)?;
        bytes += line.len() as u64;
        statements += out.len() as u64;
        out.clear();
        if reader.line % 1024 == 0 {
            restore.update(|progress| {
                progress.bytes_done = bytes;
                progress.statements_done = statements;
            });
        }
        if splitter.has_ended() {
            break;
        }
    }
    splitter.finish(&mut out).map_err(Refusal::Script)?;
    statements += out.len() as u64;
    Ok(Some((bytes, statements)))
}

/// The second pass: the script run, a statement at a time.
fn execute_script(restore: &Restore, request: &RestoreRequest, session: &Session) -> Outcome {
    let mut reader = match ScriptReader::open(&request.path) {
        Ok(reader) => reader,
        Err(refusal) => return Outcome::Refused(refusal),
    };
    let mut splitter = ScriptSplitter::new(&request.dialect);
    let mut out = Vec::new();
    let mut ran = 0u64;
    let mut failures = Vec::new();
    let mut failed = 0u64;
    let mut bytes = 0u64;

    loop {
        // The first pass read the same file to the end, so a failure here is
        // a file that changed under the restore; it ends the run as one.
        let line = match reader.next_line() {
            Ok(Some(line)) => Some(line),
            Ok(None) => None,
            Err(refusal) => return Outcome::Refused(refusal),
        };
        let split = match &line {
            Some(line) => splitter.push_line(line, &mut out),
            None => splitter.finish(&mut out),
        };
        if let Err(error) = split {
            return Outcome::Refused(Refusal::Script(error));
        }
        for statement in out.drain(..) {
            if restore.canceled() {
                return Outcome::Canceled { statements: ran };
            }
            match execute(session, &statement.sql) {
                Ok(()) => ran += 1,
                Err(_) if restore.canceled() => return Outcome::Canceled { statements: ran },
                Err(message) => {
                    let failure = failure_of(&statement, message);
                    if !request.continue_on_error {
                        return Outcome::Failed {
                            statements: ran,
                            failure,
                        };
                    }
                    failed += 1;
                    if failures.len() < KEPT_FAILURES {
                        failures.push(failure);
                    }
                }
            }
            restore.update(|progress| {
                progress.statements_done = ran + failed;
                progress.failed = failed;
            });
        }
        let Some(line) = line else {
            break;
        };
        bytes += line.len() as u64;
        restore.update(|progress| progress.bytes_done = bytes);
        if splitter.has_ended() {
            break;
        }
    }
    Outcome::Done {
        statements: ran,
        failures,
        failed,
    }
}

/// Runs one statement and walks every result it produced, so that an error
/// in a later statement of a batch is not left unread.
fn execute(session: &Session, sql: &str) -> Result<(), String> {
    let mut cursor = session
        .execute(&StatementSpec::new(sql))
        .map_err(|error| error.to_string())?;
    let mut walked = 0;
    while !cursor.result().is_exhausted() && walked < MAX_RESULTS {
        cursor.more_results().map_err(|error| error.to_string())?;
        walked += 1;
    }
    cursor.close().map_err(|error| error.to_string())
}

fn failure_of(statement: &ScriptStatement, message: String) -> Failure {
    failure(statement.line, &statement.sql, message)
}

fn failure(line: usize, sql: &str, message: String) -> Failure {
    let sql = sql.trim();
    let preview: String = sql.chars().take(PREVIEW_CHARS).collect();
    Failure {
        line,
        sql: if preview.len() < sql.len() {
            format!("{preview}…")
        } else {
            preview
        },
        message,
    }
}

/// What a restore over `scope` runs before the script, so that the names the
/// script leaves unqualified land in it: `USE` on the products whose scope is
/// a catalog, the current schema on the others. `None` for SQLite, which has
/// one scope, and for a scope with nothing to name.
pub fn enter_scope(dialect: &Dialect, scope: &Scope) -> Option<String> {
    let named = |part: &Option<String>| {
        part.as_deref()
            .map(str::trim)
            .filter(|name| !name.is_empty())
            .map(|name| dialect.quote_ident(name).into_owned())
    };
    match dialect.id() {
        DialectId::MySql | DialectId::MariaDb | DialectId::MsSql => {
            named(&scope.catalog).map(|catalog| format!("USE {catalog}"))
        }
        DialectId::Postgres => {
            named(&scope.schema).map(|schema| format!("SET search_path TO {schema}"))
        }
        DialectId::Oracle => named(&scope.schema)
            .map(|schema| format!("ALTER SESSION SET CURRENT_SCHEMA = {schema}")),
        DialectId::H2 | DialectId::Db2 => {
            named(&scope.schema).map(|schema| format!("SET SCHEMA {schema}"))
        }
        DialectId::Sqlite | DialectId::Generic => None,
    }
}

/// How the bytes of a script are text.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Encoding {
    Utf8,
    Utf16Le,
    Utf16Be,
}

/// A script file, a line at a time, unpacked and decoded.
struct ScriptReader {
    input: BufReader<Box<dyn Read + Send>>,
    encoding: Encoding,
    /// Lines read so far.
    line: usize,
}

impl ScriptReader {
    fn open(path: &Path) -> Result<Self, Refusal> {
        let file = File::open(path).map_err(io_refusal)?;
        let mut raw = BufReader::new(file);
        let gzip = raw
            .fill_buf()
            .map_err(io_refusal)?
            .starts_with(&[0x1f, 0x8b]);
        let inner: Box<dyn Read + Send> = if gzip {
            Box::new(flate2::bufread::MultiGzDecoder::new(raw))
        } else {
            Box::new(raw)
        };
        let mut input = BufReader::with_capacity(64 * 1024, inner);

        let head = input.fill_buf().map_err(io_refusal)?;
        if head.starts_with(b"PGDMP") {
            return Err(Refusal::PgArchive);
        }
        let (encoding, mark) = if head.starts_with(&[0xEF, 0xBB, 0xBF]) {
            (Encoding::Utf8, 3)
        } else if head.starts_with(&[0xFF, 0xFE]) {
            (Encoding::Utf16Le, 2)
        } else if head.starts_with(&[0xFE, 0xFF]) {
            (Encoding::Utf16Be, 2)
        } else {
            (Encoding::Utf8, 0)
        };
        input.consume(mark);
        Ok(Self {
            input,
            encoding,
            line: 0,
        })
    }

    /// The next line, its line break included, or `None` at the end.
    fn next_line(&mut self) -> Result<Option<String>, Refusal> {
        self.line += 1;
        let line = self.line;
        match self.encoding {
            Encoding::Utf8 => {
                let mut bytes = Vec::new();
                if self
                    .input
                    .read_until(b'\n', &mut bytes)
                    .map_err(io_refusal)?
                    == 0
                {
                    return Ok(None);
                }
                // A NUL is where a binary file gives itself away; no script
                // a client reads holds one.
                if bytes.contains(&0) {
                    return Err(Refusal::NotText { line });
                }
                String::from_utf8(bytes)
                    .map(Some)
                    .map_err(|_| Refusal::NotText { line })
            }
            Encoding::Utf16Le | Encoding::Utf16Be => self.next_utf16_line(line),
        }
    }

    fn next_utf16_line(&mut self, line: usize) -> Result<Option<String>, Refusal> {
        let mut text = String::new();
        loop {
            let Some(unit) = self.unit(line)? else {
                return Ok((!text.is_empty()).then_some(text));
            };
            let c = if (0xD800..0xDC00).contains(&unit) {
                let low = self.unit(line)?.ok_or(Refusal::NotText { line })?;
                if !(0xDC00..0xE000).contains(&low) {
                    return Err(Refusal::NotText { line });
                }
                char::from_u32(
                    0x10000 + ((u32::from(unit) - 0xD800) << 10) + (u32::from(low) - 0xDC00),
                )
            } else {
                char::from_u32(u32::from(unit))
            };
            let c = c.ok_or(Refusal::NotText { line })?;
            if c == '\0' {
                return Err(Refusal::NotText { line });
            }
            text.push(c);
            if c == '\n' {
                return Ok(Some(text));
            }
        }
    }

    /// One UTF-16 code unit, or `None` at the end.
    fn unit(&mut self, line: usize) -> Result<Option<u16>, Refusal> {
        let mut pair = [0u8; 2];
        if self.input.read(&mut pair[..1]).map_err(io_refusal)? == 0 {
            return Ok(None);
        }
        self.input
            .read_exact(&mut pair[1..])
            .map_err(|_| Refusal::NotText { line })?;
        Ok(Some(match self.encoding {
            Encoding::Utf16Be => u16::from_be_bytes(pair),
            _ => u16::from_le_bytes(pair),
        }))
    }
}

fn io_refusal(error: io::Error) -> Refusal {
    Refusal::Io(error.to_string())
}

#[cfg(test)]
mod containers;

#[cfg(test)]
pub(crate) mod tests {
    use std::io::Write;

    use rudbman_core::AppSettings;

    use super::*;
    use crate::connection::{self, Connected};

    /// A live H2 database of its own, with `setup` run against it.
    pub(crate) fn h2(name: &str, setup: &[&str]) -> Connected {
        let mut profile = connection::h2::profile(name);
        profile.url = format!("{};DB_CLOSE_DELAY=-1", profile.url);
        profile.confirm_writes = false;
        profile.read_only = false;
        let connected = connection::connect(
            &profile,
            &connection::h2::driver(),
            &connection::Credentials::typed(Some(String::new()), None),
            &AppSettings::default(),
        )
        .expect("H2 opens an in-memory database without a server");
        for sql in setup {
            connected
                .session()
                .execute(&StatementSpec::new(*sql))
                .unwrap_or_else(|error| panic!("{sql}: {error}"));
        }
        connected
    }

    /// One integer `sql` answers on `connected`'s own session.
    fn count(connected: &Connected, sql: &str) -> i64 {
        let cursor = connected
            .session()
            .execute(&StatementSpec::new(sql))
            .unwrap_or_else(|error| panic!("{sql}: {error}"));
        let batch = cursor.fetch(1).expect("the count comes back");
        match batch.value(0, 0) {
            Some(rudbman_jdbc::Value::I64(count)) => count,
            other => panic!("{sql} answered {other:?}"),
        }
    }

    /// Runs `request` on a sibling of `connected` to its end.
    fn restore(connected: &Connected, request: RestoreRequest) -> (Outcome, Progress) {
        let restore = Restore::start(connected.handle(), request);
        for _ in 0..30_000 {
            if let Some(outcome) = restore.outcome() {
                return (outcome, restore.progress());
            }
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        panic!("the restore never ended");
    }

    fn request(path: &Path) -> RestoreRequest {
        RestoreRequest {
            path: path.to_path_buf(),
            dialect: Dialect::H2,
            continue_on_error: false,
            enter: None,
        }
    }

    fn write(dir: &Path, name: &str, bytes: &[u8]) -> PathBuf {
        let path = dir.join(name);
        std::fs::write(&path, bytes).expect("the script is written");
        path
    }

    #[test]
    fn a_script_runs_on_a_session_of_its_own() {
        let connected = h2("restore-runs", &["CREATE SCHEMA APP"]);
        let dir = tempfile::tempdir().unwrap();
        let path = write(
            dir.path(),
            "app.sql",
            b"-- a script\nCREATE TABLE T (ID INT PRIMARY KEY, NAME VARCHAR(20));\n\
              INSERT INTO T VALUES (1, 'one; still one'), (2, 'two');\n\
              SET SCHEMA PUBLIC;\n",
        );
        let mut request = request(&path);
        request.enter = enter_scope(
            &Dialect::H2,
            &Scope {
                catalog: None,
                schema: Some("APP".into()),
            },
        );
        let (outcome, progress) = restore(&connected, request);
        assert_eq!(
            outcome,
            Outcome::Done {
                statements: 3,
                failures: Vec::new(),
                failed: 0
            }
        );
        assert_eq!(progress.statements_total, 3);
        assert_eq!(progress.statements_done, 3);
        assert_eq!(progress.bytes_done, progress.bytes_total);
        // The unqualified table landed in the scope, and the script's own
        // `SET SCHEMA` changed nothing on the connection's session.
        assert_eq!(count(&connected, "SELECT COUNT(*) FROM APP.T"), 2);
        assert_eq!(
            count(
                &connected,
                "SELECT COUNT(*) FROM INFORMATION_SCHEMA.TABLES WHERE TABLE_SCHEMA = CURRENT_SCHEMA AND TABLE_NAME = 'T'"
            ),
            0
        );
    }

    #[test]
    fn a_failure_stops_the_run_or_is_passed() {
        let connected = h2("restore-fails", &[]);
        let dir = tempfile::tempdir().unwrap();
        let path = write(
            dir.path(),
            "bad.sql",
            b"CREATE TABLE A (ID INT);\nINSERT INTO NOWHERE VALUES (1);\nINSERT INTO A VALUES (1);\n",
        );
        let (outcome, _) = restore(&connected, request(&path));
        let Outcome::Failed {
            statements,
            failure,
        } = outcome
        else {
            panic!("the run should stop: {outcome:?}");
        };
        assert_eq!(statements, 1);
        assert_eq!(failure.line, 2);
        assert_eq!(failure.sql, "INSERT INTO NOWHERE VALUES (1)");
        assert!(!failure.message.is_empty());
        assert_eq!(count(&connected, "SELECT COUNT(*) FROM A"), 0);

        let mut past = request(&path);
        past.continue_on_error = true;
        connected
            .session()
            .execute(&StatementSpec::new("DROP TABLE A"))
            .unwrap();
        let (outcome, progress) = restore(&connected, past);
        let Outcome::Done {
            statements,
            failures,
            failed,
        } = outcome
        else {
            panic!("the run should go on: {outcome:?}");
        };
        assert_eq!((statements, failed), (2, 1));
        assert_eq!(failures[0].line, 2);
        assert_eq!(progress.failed, 1);
        assert_eq!(count(&connected, "SELECT COUNT(*) FROM A"), 1);
    }

    #[test]
    fn a_script_that_cannot_be_finished_runs_nothing() {
        let connected = h2("restore-refused", &[]);
        let dir = tempfile::tempdir().unwrap();
        let path = write(
            dir.path(),
            "pg.sql",
            b"CREATE TABLE B (ID INT);\nCOPY public.b (id) FROM stdin;\n1\n\\.\n",
        );
        let mut request = request(&path);
        request.dialect = Dialect::POSTGRES;
        let (outcome, _) = restore(&connected, request);
        let Outcome::Refused(Refusal::Script(error)) = outcome else {
            panic!("COPY FROM stdin should be refused: {outcome:?}");
        };
        assert_eq!(error.line, 2);
        assert_eq!(
            count(
                &connected,
                "SELECT COUNT(*) FROM INFORMATION_SCHEMA.TABLES WHERE TABLE_NAME = 'B'"
            ),
            0
        );

        let archive = write(dir.path(), "dump.backup", b"PGDMP\x01\x0e\x00binary");
        assert_eq!(
            restore(&connected, super::tests::request(&archive)).0,
            Outcome::Refused(Refusal::PgArchive)
        );
        let latin1 = write(dir.path(), "latin1.sql", b"select 1;\nselect 'caf\xe9';\n");
        assert_eq!(
            restore(&connected, super::tests::request(&latin1)).0,
            Outcome::Refused(Refusal::NotText { line: 2 })
        );
    }

    #[test]
    fn gzip_and_utf16_are_read_as_text() {
        let connected = h2("restore-encodings", &[]);
        let dir = tempfile::tempdir().unwrap();
        let script = "CREATE TABLE C (NAME VARCHAR(20));\nINSERT INTO C VALUES ('한글 🙂');\n";

        let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        gz.write_all(script.as_bytes()).unwrap();
        let path = write(dir.path(), "c.sql.gz", &gz.finish().unwrap());
        assert!(matches!(
            restore(&connected, request(&path)).0,
            Outcome::Done { statements: 2, .. }
        ));

        let mut utf16 = vec![0xFF, 0xFE];
        for unit in script
            .replace("TABLE C", "TABLE D")
            .replace("INTO C", "INTO D")
            .encode_utf16()
        {
            utf16.extend_from_slice(&unit.to_le_bytes());
        }
        let path = write(dir.path(), "d.sql", &utf16);
        assert!(matches!(
            restore(&connected, request(&path)).0,
            Outcome::Done { statements: 2, .. }
        ));
        assert_eq!(
            count(
                &connected,
                "SELECT COUNT(*) FROM C JOIN D ON C.NAME = D.NAME WHERE C.NAME = '한글 🙂'"
            ),
            1
        );
    }

    #[test]
    fn a_cancel_stops_the_run_between_statements_and_keeps_what_ran() {
        let connected = h2("restore-cancel", &["CREATE TABLE E (ID INT)"]);
        let dir = tempfile::tempdir().unwrap();
        let script: String = (0..20_000)
            .map(|id| format!("INSERT INTO E VALUES ({id});\n"))
            .collect();
        let path = write(dir.path(), "long.sql", script.as_bytes());
        let restore = Restore::start(connected.handle(), request(&path));
        while restore.progress().phase != Phase::Running || restore.progress().statements_done == 0
        {
            assert!(restore.outcome().is_none(), "{:?}", restore.outcome());
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        restore.cancel();
        let outcome = loop {
            if let Some(outcome) = restore.outcome() {
                break outcome;
            }
            std::thread::sleep(std::time::Duration::from_millis(2));
        };
        let Outcome::Canceled { statements } = outcome else {
            panic!("the run should stop: {outcome:?}");
        };
        assert!(statements > 0 && statements < 20_000, "{statements}");
        // Auto-commit: every statement that ran before the cancel stays.
        assert_eq!(
            count(&connected, "SELECT COUNT(*) FROM E"),
            statements as i64
        );
    }

    #[test]
    fn the_scope_is_entered_the_way_each_product_names_it() {
        let scope = Scope {
            catalog: Some("shop".into()),
            schema: Some("Sales".into()),
        };
        assert_eq!(
            enter_scope(&Dialect::MYSQL, &scope).as_deref(),
            Some("USE shop")
        );
        assert_eq!(
            enter_scope(&Dialect::POSTGRES, &scope).as_deref(),
            Some("SET search_path TO \"Sales\"")
        );
        assert_eq!(
            enter_scope(&Dialect::ORACLE, &scope).as_deref(),
            Some("ALTER SESSION SET CURRENT_SCHEMA = \"Sales\"")
        );
        assert_eq!(enter_scope(&Dialect::SQLITE, &scope), None);
        assert_eq!(enter_scope(&Dialect::POSTGRES, &Scope::default()), None);
    }
}
