//! Where a statement that holds a block ends.
//!
//! A semicolon ends a statement, except inside the body of one that has a
//! body: `CREATE PROCEDURE … BEGIN …; …; END`, a PL/SQL package, an anonymous
//! block, a SQLite trigger, PostgreSQL's `BEGIN ATOMIC`. The inner semicolons
//! end the statements of the body, and the one after the closing `END` ends
//! the whole. [`Blocks`] is fed the tokens of one statement in order and
//! answers, at each `;`, which of the two that `;` is.
//!
//! # Not a parser
//!
//! Two questions, each answered from words alone.
//!
//! **Does the statement have a body at all?** Its first few words say, the
//! same few a product's own client looks at: `CREATE [OR REPLACE] PROCEDURE`,
//! `FUNCTION`, `TRIGGER`, `EVENT` or `PACKAGE`, or an `ALTER` that names one,
//! as MySQL's `ALTER EVENT … DO BEGIN` and Db2's `ALTER MODULE … ADD
//! PROCEDURE` do; on Oracle `DECLARE`, `BEGIN`
//! and the `CREATE` of any stored unit; on SQL Server `IF`, `WHILE` and a
//! `BEGIN` that does not start a transaction; on MariaDB, Db2 and a product
//! nobody named, a `DECLARE` that declares no cursor or temporary table of
//! its own, and Firebird's `EXECUTE BLOCK`. Everything else is
//! [`Kind::Plain`], and every `;` ends it, exactly as before this module
//! existed. A `CASE … END` typed half-way through an ordinary `SELECT` never
//! swallows the statements under it.
//!
//! **How deep is the body here?** `BEGIN` and `CASE` open a level and `END`
//! closes one. The other constructs a body is built of close with a word of
//! their own after the `END` — `END IF`, `END LOOP`, `END WHILE`,
//! `END REPEAT`, `END FOR` — and are counted on neither side, which is what
//! keeps MySQL's `IF(a, b, c)` function from opening anything. `END CASE`
//! closes the `CASE` it ends, and so do SQL Server's `END TRY` and
//! `END CATCH`. A `;` at depth zero ends the statement.
//!
//! # PL/SQL
//!
//! A PL/SQL unit's body opens without a `BEGIN`: `IS` or `AS` after the
//! header of a procedure, a function, a package or a type body starts a
//! declaration section, and the `BEGIN` that may follow it continues the same
//! level rather than opening another — `PROCEDURE p IS … BEGIN … END;` is one
//! level, closed by one `END`, and so is `DECLARE … BEGIN … END;`. A
//! subprogram declared inside a declaration section opens on its own first
//! `IS` or `AS`, which is why each declaration's first word is kept: a
//! `CURSOR c IS` or a `TYPE t IS TABLE OF` opens nothing. A call
//! specification (`AS LANGUAGE`, `AS EXTERNAL`, `AS MLE`) has no body, and a
//! type specification's `AS OBJECT (…)` is not one either. A compound
//! trigger's `COMPOUND TRIGGER` opens a section whose timing points open on
//! their `IS` like a subprogram.
//!
//! The `;` that ends a PL/SQL unit is part of it ([`Semicolon::EndsWithin`]):
//! the server rejects `BEGIN NULL; END` without it.
//!
//! MariaDB (under `sql_mode=ORACLE`) and Db2 take PL/SQL too, beside their
//! own SQL/PSM. Which one a procedure, a function or a package is written
//! in, its header says: PL/SQL's ends in `IS` or `AS`, and SQL/PSM's has
//! neither. Such an `IS` or `AS` turns the rest of the statement over to the
//! Oracle rules above. It counts only while the header is being read: once a
//! word that starts a SQL/PSM body has come (`BEGIN`, `SELECT`, `SET`, …,
//! or a `RETURN` after the header's `RETURNS`), an `AS` is a column's alias
//! and an `IS` an `IS NULL`.
//!
//! What has no such header tells by its first word instead. SQL/PSM
//! declares inside a `BEGIN` only, so a `DECLARE` before one — leading the
//! statement, or leading a trigger's body — is PL/SQL's, unless it is Db2's
//! `DECLARE GLOBAL TEMPORARY TABLE` or its command line's `DECLARE c
//! CURSOR`. MariaDB's own `BEGIN` starts a transaction, `BEGIN WORK` at
//! most, so a `BEGIN` before anything else is a block. And MariaDB's package
//! in SQL/PSM, which has no `AS`, opens on its first declaration and runs to
//! the `END` that closes it.
//!
//! # A product nobody named
//!
//! A driver's own dialect is read as the SQL/PSM its `BEGIN … END` is, and
//! PL/SQL and the bodies other products write without a `BEGIN` are told by
//! marks of their own:
//!
//! - **PL/SQL** (Tibero, Altibase): a header's `IS`, or its `AS` before a
//!   declaration — a `CURSOR`, a `TYPE`, a subprogram, or a name and a type.
//!   An `AS` before a statement is T-SQL's, one before a quoted body
//!   PostgreSQL's, and one before `x IS NULL` or `a + b` a DuckDB macro's.
//!   A `DECLARE` is read as on MariaDB, and T-SQL's `DECLARE @n` is no
//!   block's. A `DECLARE c CURSOR …;` is PostgreSQL's statement, or the
//!   first declaration of a Snowflake block, as the `BEGIN` or the next
//!   declaration after its `;` says ([`Semicolon::Unsure`]).
//! - **Firebird**: `AS` and a `DECLARE` to each declaration, read as a
//!   PL/SQL declaration section, sub-routines and all.
//! - **Informix**: a body with no `BEGIN` that runs to `END PROCEDURE` or
//!   `END FUNCTION`, nothing inside it counted. Its header's `RETURNING`, or
//!   a first statement only SPL writes (`DEFINE`, `LET`, `FOREACH`,
//!   `TRACE`), tells it at once. Any other procedure or function is
//!   Informix's if an `END PROCEDURE` or `END FUNCTION` comes before the
//!   next routine's `CREATE`, so the `;` that would end it is
//!   [`Semicolon::Unsure`] and the script after it is read on that far, up
//!   to [`VERDICT_REACH`].
//!
//! # SQL Server
//!
//! A `CREATE PROCEDURE`, `FUNCTION` or `TRIGGER` has no end of its own: the
//! body is the rest of the batch, `BEGIN` or not. No `;` ends it
//! ([`Kind::ToEnd`]); the `GO` that ends the batch does, and that is the
//! caller's to see. Oracle's `CREATE JAVA SOURCE` is the same, up to its `/`.
//!
//! An `IF` needs no `BEGIN` for a branch of one statement, and that statement
//! may end in a `;`: `IF @a = 1 SELECT 1; ELSE SELECT 2;` is one statement.
//! No statement starts with `ELSE`, so a `;` with an `IF` still waiting for
//! one ends the statement only if something else comes next
//! ([`Semicolon::Unsure`]). Each
//! `ELSE` answers the nearest `IF`, so an outer `IF` with none ends at the
//! `;` after the inner one's.
//!
//! # What it gets wrong
//!
//! A label, MariaDB's top-level `IF … END IF` outside a `BEGIN NOT ATOMIC`,
//! an Informix routine longer than [`VERDICT_REACH`] whose header says
//! nothing: each is cut as a semicolon splitter cuts it, which is no worse
//! than before.
//! Counting the other way — a body that never closes
//! because an `END` was missed — runs the statement to the end of the script,
//! or to SQL\*Plus's `/`, which is where a client that counts nothing would
//! have run it.

use crate::dialect::{Dialect, DialectId};
use crate::lexer::TokenKind;

/// What may follow a leading `BEGIN` that starts a transaction rather than a
/// block: `BEGIN TRANSACTION`, `BEGIN WORK`, SQLite's `BEGIN IMMEDIATE`,
/// PostgreSQL's `BEGIN ISOLATION LEVEL ...` and `BEGIN NOT DEFERRABLE`, SQL
/// Server's `BEGIN DISTRIBUTED TRAN`.
pub(crate) const TRANSACTION_WORDS: &[&str] = &[
    "TRANSACTION",
    "TRAN",
    "WORK",
    "DEFERRED",
    "IMMEDIATE",
    "EXCLUSIVE",
    "DISTRIBUTED",
    "ISOLATION",
    "READ",
    "NOT",
    "DEFERRABLE",
];

/// What follows `BEGIN` on SQL Server when it is a statement rather than a
/// block: a transaction, or a Service Broker conversation.
const MSSQL_BEGIN_STATEMENTS: &[&str] = &[
    "TRAN",
    "TRANSACTION",
    "DISTRIBUTED",
    "DIALOG",
    "CONVERSATION",
];

/// The words after `END` that close a construct nobody counted the opening
/// of.
const UNCOUNTED_ENDS: &[&str] = &["IF", "LOOP", "WHILE", "REPEAT", "FOR"];

/// The words after `END` that belong to it and close what it closes.
const COUNTED_ENDS: &[&str] = &["CASE", "TRY", "CATCH"];

/// The words that may stand between `CREATE` and the kind of a PL/SQL unit.
const PLSQL_MODIFIERS: &[&str] = &[
    "OR",
    "REPLACE",
    "EDITIONABLE",
    "NONEDITIONABLE",
    "AND",
    "COMPILE",
    "RESOLVE",
    "NOFORCE",
    "FORCE",
];

/// The first words of a declaration whose first `IS` or `AS` opens a body: a
/// subprogram, a type body's method, a compound trigger's timing point.
const SUBPROGRAM_HEADS: &[&str] = &[
    "PROCEDURE",
    "FUNCTION",
    "CONSTRUCTOR",
    "MEMBER",
    "STATIC",
    "MAP",
    "ORDER",
    "OVERRIDING",
    "FINAL",
    "INSTANTIABLE",
    "NOT",
    "BEFORE",
    "AFTER",
    "INSTEAD",
];

/// What follows an `IS` or `AS` that is a call specification, not a body.
const CALL_SPECS: &[&str] = &["LANGUAGE", "EXTERNAL", "MLE"];

/// The routine words a `CREATE` names within its first few tokens.
const ROUTINES: &[&str] = &["PROCEDURE", "FUNCTION", "TRIGGER", "EVENT", "PACKAGE"];

/// The routines MariaDB and Db2 also take in PL/SQL.
const PLSQL_ROUTINES: &[&str] = &["PROCEDURE", "FUNCTION", "PACKAGE"];

/// The first words of a PL/SQL declaration that is not a variable's.
const PLSQL_DECLARATIONS: &[&str] = &[
    "CURSOR",
    "TYPE",
    "SUBTYPE",
    "PRAGMA",
    "PROCEDURE",
    "FUNCTION",
];

/// What follows the name in a `DECLARE name …` that is a statement of its
/// own rather than the start of a PL/SQL block: a cursor, as Db2's command
/// line, PostgreSQL and Informix declare one.
const CURSOR_DECLARATIONS: &[&str] = &[
    "CURSOR",
    "BINARY",
    "NO",
    "SCROLL",
    "INSENSITIVE",
    "ASENSITIVE",
    "SENSITIVE",
];

/// The words besides [`PSM_BODY_STARTS`] that may follow a routine's `AS`
/// when no PL/SQL declaration does: a T-SQL body's first statement, a
/// DuckDB macro's `AS TABLE`. On a product nobody named, they keep that `AS`
/// from being taken for PL/SQL's.
const AS_STATEMENTS: &[&str] = &[
    "RETURN",
    "PRINT",
    "EXEC",
    "RAISERROR",
    "THROW",
    "WAITFOR",
    "BREAK",
    "CONTINUE",
    "DENY",
    "SAVE",
    "DBCC",
    "BULK",
    "BACKUP",
    "RESTORE",
    "CHECKPOINT",
    "READTEXT",
    "WRITETEXT",
    "UPDATETEXT",
    "DUMP",
    "RECONFIGURE",
    "SETUSER",
    "SHUTDOWN",
    "TABLE",
];

/// The words besides [`PSM_BODY_STARTS`] and [`AS_STATEMENTS`] that start a
/// statement after a cursor's `DECLARE`, as PostgreSQL's `MOVE` and `END`
/// do: what follows that cursor's `;` when it was a statement of its own.
const MORE_STATEMENTS: &[&str] = &[
    "MOVE", "END", "ABORT", "VACUUM", "COPY", "LISTEN", "NOTIFY", "UNLISTEN", "DISCARD", "CLUSTER",
    "REINDEX", "REFRESH", "RESET", "COMMENT", "EXPLAIN", "DESCRIBE",
];

/// The words that, second after a routine's `AS`, make the first one an
/// expression's rather than a variable's, as in DuckDB's
/// `CREATE FUNCTION f(x) AS x IS NULL`.
const OPERATOR_WORDS: &[&str] = &[
    "IS", "AND", "OR", "NOT", "IN", "LIKE", "ILIKE", "BETWEEN", "COLLATE", "ESCAPE", "AS", "FROM",
    "OVER", "FILTER", "DIV", "MOD", "XOR", "REGEXP", "GLOB", "SIMILAR",
];

/// What only Informix's SPL writes before a routine's first statement or as
/// that statement: the header's `RETURNING`, and the statements its bodies
/// most often open with.
const SPL_MARKS: &[&str] = &["RETURNING", "DEFINE", "LET", "FOREACH", "TRACE"];

/// What a routine's name may follow: `CREATE PROCEDURE IF NOT EXISTS p`.
const NAME_GUARDS: &[&str] = &["IF", "NOT", "EXISTS"];

/// The words a SQL/PSM body can start with, which end its routine's header:
/// an `AS` or an `IS` after one is the body's own, as in
/// `CREATE PROCEDURE p() SELECT a AS b FROM t`.
const PSM_BODY_STARTS: &[&str] = &[
    "BEGIN",
    "DECLARE",
    "SELECT",
    "WITH",
    "VALUES",
    "INSERT",
    "UPDATE",
    "DELETE",
    "REPLACE",
    "MERGE",
    "SET",
    "CALL",
    "DO",
    "IF",
    "CASE",
    "WHILE",
    "REPEAT",
    "LOOP",
    "FOR",
    "LEAVE",
    "ITERATE",
    "OPEN",
    "FETCH",
    "CLOSE",
    "SIGNAL",
    "RESIGNAL",
    "GET",
    "EXECUTE",
    "PREPARE",
    "DEALLOCATE",
    "CREATE",
    "ALTER",
    "DROP",
    "RENAME",
    "TRUNCATE",
    "GRANT",
    "REVOKE",
    "COMMIT",
    "ROLLBACK",
    "START",
    "SAVEPOINT",
    "RELEASE",
    "LOCK",
    "UNLOCK",
    "HANDLER",
    "LOAD",
    "ANALYZE",
    "OPTIMIZE",
    "SHOW",
    "FLUSH",
    "PURGE",
    "KILL",
    "USE",
    "XA",
    "GOTO",
    "PIPE",
];

/// The words a `CREATE` names an object with that is not a routine: once one
/// is read, a routine word after it is a name or a column — `CREATE VIEW v AS
/// SELECT event, begin FROM t` holds MySQL's `begin` column, not a block.
const OBJECTS: &[&str] = &[
    "TABLE",
    "VIEW",
    "INDEX",
    "SEQUENCE",
    "DATABASE",
    "SCHEMA",
    "USER",
    "ROLE",
    "DOMAIN",
    "TYPE",
    "EXTENSION",
    "TABLESPACE",
    "SERVER",
    "SYNONYM",
];

/// How many tokens of a `CREATE` are read for one of [`ROUTINES`]: enough
/// for MySQL's `CREATE DEFINER = 'root'@'%' SQL SECURITY INVOKER PROCEDURE`.
const CREATE_LOOKAHEAD: u8 = 12;

/// What a `;` does to the statement being read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Semicolon {
    /// It ends one of the statements inside a body: the statement goes on, and
    /// the `;` is part of it.
    Inside,
    /// It ends the statement, and is not part of what is sent.
    Ends,
    /// It ends the statement and is part of it, as PL/SQL's `END;` is.
    EndsWithin,
    /// It ends the statement unless what comes after it carries the statement
    /// on: an `ELSE` after T-SQL's `IF @a = 1 SELECT 1;`, a `BEGIN` after a
    /// cursor's `DECLARE`, an `END PROCEDURE` further on. The statement is
    /// read on as if it did not end, the `;` part of it, and
    /// [`Blocks::verdict`] says after each token whether it did: a caller
    /// that hears [`Verdict::Ended`] ends the statement at this `;`, calls
    /// [`Blocks::reset`], and reads what came after it again as the next
    /// statement's.
    Unsure,
}

/// What the tokens after a [`Semicolon::Unsure`] have told so far.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Verdict {
    /// Not enough yet.
    Open,
    /// The statement goes on past that `;`.
    Continues,
    /// It ended at that `;`.
    Ended,
}

/// How far past a [`Semicolon::Unsure`] a caller reads for its
/// [`Verdict`] before taking it as [`Verdict::Ended`]: no Informix routine
/// is as long, and a script streamed from a file is not held back further.
pub(crate) const VERDICT_REACH: usize = 1 << 20;

/// What the tokens after a [`Semicolon::Unsure`] are waited on for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Guess {
    /// Nothing.
    None,
    /// T-SQL: an `ELSE` next.
    Else,
    /// A cursor's `DECLARE` on a product nobody named: a `BEGIN` next, or
    /// another declaration — a name, which has been read once `named`, and
    /// its type.
    Cursor {
        /// Whether the name has been read.
        named: bool,
    },
    /// A procedure or a function on a product nobody named: an
    /// `END PROCEDURE` or `END FUNCTION` before the next one's `CREATE`,
    /// which makes it Informix's.
    Spl {
        /// Whether the last token was a `;`, after which a `CREATE` starts a
        /// statement.
        after_semicolon: bool,
        /// How many tokens after such a `CREATE` have been read.
        create: Option<u8>,
    },
    /// Told: whether the statement goes on.
    Told(bool),
}

/// Whose rules the words are read by.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Family {
    /// PL/SQL.
    Oracle,
    /// T-SQL.
    MsSql,
    /// Everything else, which writes a body as SQL/PSM does, `BEGIN … END`
    /// with `CASE` inside it.
    Psm {
        /// What a statement starting with `BEGIN` is.
        begin: BeginRule,
        /// What else a routine and a block may be written in.
        routines: Routines,
    },
}

/// What a body may be written in besides SQL/PSM, outside Oracle and SQL
/// Server.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Routines {
    /// Nothing else: MySQL, PostgreSQL, SQLite, H2.
    Psm,
    /// PL/SQL: MariaDB under `sql_mode=ORACLE`, and Db2.
    PlSql,
    /// Whatever a product nobody named writes: PL/SQL, Firebird's PSQL,
    /// Informix's SPL, each told by marks of its own.
    Unknown,
}

/// What a statement that starts with `BEGIN` is, outside Oracle and SQL
/// Server.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BeginRule {
    /// A transaction, always: PostgreSQL, SQLite, H2.
    Transaction,
    /// A block only as `BEGIN NOT ATOMIC`: MySQL and MariaDB.
    NotAtomic,
    /// A block unless a transaction word follows: Db2's `BEGIN ATOMIC`, and
    /// the guess for a product nobody named.
    UnlessTransaction,
}

impl Family {
    /// The family `dialect` belongs to.
    const fn of(dialect: &Dialect) -> Self {
        match dialect.id() {
            DialectId::Oracle => Self::Oracle,
            DialectId::MsSql => Self::MsSql,
            DialectId::MySql => Self::Psm {
                begin: BeginRule::NotAtomic,
                routines: Routines::Psm,
            },
            DialectId::MariaDb => Self::Psm {
                begin: BeginRule::NotAtomic,
                routines: Routines::PlSql,
            },
            DialectId::Db2 => Self::Psm {
                begin: BeginRule::UnlessTransaction,
                routines: Routines::PlSql,
            },
            DialectId::Generic => Self::Psm {
                begin: BeginRule::UnlessTransaction,
                routines: Routines::Unknown,
            },
            DialectId::Postgres | DialectId::Sqlite | DialectId::H2 => Self::Psm {
                begin: BeginRule::Transaction,
                routines: Routines::Psm,
            },
        }
    }
}

/// What kind of statement is being read, as far as its first words have told.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    /// Too few words yet.
    Open(Lead),
    /// An ordinary statement: every `;` ends it.
    Plain,
    /// A statement with a body: a `;` ends it outside the body only.
    Blocks,
    /// A PL/SQL unit: as [`Kind::Blocks`], and the `;` that ends it is part of
    /// it.
    PlSql(Unit),
    /// A body with no end a `;` could mark.
    ToEnd,
    /// A product nobody named: `DECLARE c CURSOR …`, a statement of its own
    /// or the first declaration of a block, as the token after its `;` tells.
    Cursor,
}

/// The words of a statement's head that the kind is still waiting on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Lead {
    /// Nothing yet.
    Start,
    /// `CREATE`, and so many tokens after it (or `ALTER`, on SQL Server).
    Create(u8),
    /// Oracle's `CREATE … TYPE`, which `BODY` may follow.
    CreateType,
    /// A leading `BEGIN`.
    Begin,
    /// MySQL's `BEGIN NOT`.
    BeginNot,
    /// A leading `DECLARE`, outside Oracle and SQL Server.
    Declare,
    /// `DECLARE` and a name, which `CURSOR` makes a cursor's declaration.
    DeclareName,
    /// A leading `EXECUTE`, which Firebird's `BLOCK` makes a block.
    Execute,
}

/// What a PL/SQL unit is, which decides what its own `IS` or `AS` does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Unit {
    /// `DECLARE` or `BEGIN`.
    Anonymous,
    /// A procedure, a function, a package or a type body: `IS` opens it.
    Opens,
    /// A type specification or a library, which a `;` ends.
    Spec,
    /// A trigger, whose `AS` is `REFERENCING NEW AS n`.
    Trigger,
}

/// One open level of a body.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Frame {
    /// `BEGIN` or `CASE`.
    Block,
    /// A PL/SQL declaration section, which the `BEGIN` after it continues
    /// rather than nests in. `begun` once it has.
    Declare {
        /// Whether its `BEGIN` has been read.
        begun: bool,
    },
    /// An Informix SPL routine's body, which only its `END PROCEDURE` or
    /// `END FUNCTION` closes.
    Spl,
}

/// A word whose meaning waits on the token after it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Pending {
    /// Nothing.
    None,
    /// `END`, which `IF` or `LOOP` may turn into something that closes no
    /// counted level.
    End,
    /// SQL Server's `BEGIN`, which `TRAN` turns into a statement.
    Begin,
    /// PL/SQL's `IS` or `AS` where a body may open, which `LANGUAGE` turns into
    /// a call specification.
    Opener,
    /// Oracle's `COMPOUND`, waiting for `TRIGGER`.
    Compound,
    /// A `DECLARE` at the top of a body outside Oracle, which opens a
    /// PL/SQL declaration section unless a T-SQL `@variable` follows.
    Declare,
    /// A routine's `AS` on a product nobody named, which the word after it
    /// tells PL/SQL's from anything else's.
    As,
    /// That `AS` and a name, which a word after it makes a PL/SQL
    /// variable's.
    AsName,
}

/// How far the header of a routine that may be in something other than
/// SQL/PSM has been read, outside Oracle and SQL Server.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Head {
    /// Not in one: no such routine, or its header is over.
    Closed,
    /// Before the routine's name, or a part of it after a `.`.
    Name {
        /// Whether the routine is a package.
        package: bool,
    },
    /// After the name, where an `IS` or an `AS` makes the routine PL/SQL.
    Named {
        /// Whether the routine is a package.
        package: bool,
        /// Whether `RETURNS` has been read, which only SQL/PSM writes: a
        /// `RETURN` after it is the body's.
        returns: bool,
    },
}

/// Reads one statement's tokens and says what each of its `;` does.
///
/// Fed every token that is not whitespace, a comment or a `;` through
/// [`Blocks::token`], and every `;` through [`Blocks::semicolon`], which
/// starts the next statement whenever it ends this one. [`Blocks::reset`]
/// starts it over for a caller that dropped what it was reading.
#[derive(Debug, Clone)]
pub(crate) struct Blocks {
    /// The dialect's family.
    home: Family,
    /// Whose rules the statement is read by: [`Self::home`]'s, or Oracle's
    /// once PL/SQL has been told from SQL/PSM outside Oracle.
    family: Family,
    kind: Kind,
    frames: Vec<Frame>,
    pending: Pending,
    /// How many `(` are open.
    parens: u32,
    /// PL/SQL: the next word is the first of a declaration.
    item_start: bool,
    /// PL/SQL: the declaration being read is a subprogram whose first `IS`
    /// or `AS` opens its body.
    item_opens: bool,
    /// PL/SQL: the unit's own `IS` or `AS` has been read.
    unit_opened: bool,
    /// T-SQL: how many `IF`s outside any level are still without their
    /// `ELSE`.
    ifs: u32,
    /// Outside Oracle and SQL Server: the header of a routine that may be in
    /// something other than SQL/PSM.
    head: Head,
    /// A product nobody named: the statement is a procedure or a function
    /// that may yet turn out Informix's, nothing in its header having said.
    maybe_spl: bool,
    /// What the tokens after a [`Semicolon::Unsure`] are waited on for.
    guess: Guess,
}

impl Blocks {
    /// A reader for statements in `dialect`.
    pub(crate) fn new(dialect: &Dialect) -> Self {
        let family = Family::of(dialect);
        Self {
            home: family,
            family,
            kind: Kind::Open(Lead::Start),
            frames: Vec::new(),
            pending: Pending::None,
            parens: 0,
            item_start: false,
            item_opens: false,
            unit_opened: false,
            ifs: 0,
            head: Head::Closed,
            maybe_spl: false,
            guess: Guess::None,
        }
    }

    /// Forgets the statement being read.
    pub(crate) fn reset(&mut self) {
        self.kind = Kind::Open(Lead::Start);
        self.frames.clear();
        self.pending = Pending::None;
        self.parens = 0;
        self.item_start = false;
        self.item_opens = false;
        self.unit_opened = false;
        self.ifs = 0;
        self.family = self.home;
        self.head = Head::Closed;
        self.maybe_spl = false;
        self.guess = Guess::None;
    }

    /// What the tokens read since the last [`Semicolon::Unsure`] have told.
    pub(crate) const fn verdict(&self) -> Verdict {
        match self.guess {
            Guess::Else | Guess::Cursor { .. } | Guess::Spl { .. } => Verdict::Open,
            Guess::Told(false) => Verdict::Ended,
            Guess::None | Guess::Told(true) => Verdict::Continues,
        }
    }

    /// Reads one token of the statement: anything but whitespace, a comment
    /// and a `;`.
    pub(crate) fn token(&mut self, kind: TokenKind, text: &str) {
        let word = matches!(
            kind,
            TokenKind::Keyword | TokenKind::Identifier | TokenKind::Function | TokenKind::Type
        )
        .then_some(text);

        if !self.weigh(word, text) {
            return;
        }
        if !self.resolve(word, text) {
            return;
        }
        if let Kind::Open(lead) = self.kind {
            self.kind = self.classify(lead, word, text);
            if let (Kind::PlSql(_), Family::Psm { .. }) = (self.kind, self.family) {
                // An anonymous PL/SQL block outside Oracle.
                self.family = Family::Oracle;
                match lead {
                    // This token heads the block's first declaration.
                    Lead::Declare => self.open_declare(),
                    // This one is the type of a declaration already begun.
                    Lead::DeclareName => {
                        self.open_declare();
                        self.item_start = false;
                    }
                    // A `BEGIN`, whose level is open already.
                    _ => {}
                }
            }
            if self.kind == Kind::Blocks
                && self.routines() != Routines::Psm
                && word.is_some_and(|word| any(word, PLSQL_ROUTINES))
            {
                self.head = Head::Name {
                    package: word.is_some_and(|word| word.eq_ignore_ascii_case("PACKAGE")),
                };
                self.maybe_spl = self.routines() == Routines::Unknown
                    && word.is_some_and(|word| any(word, &["PROCEDURE", "FUNCTION"]));
                // The routine's word is not its name.
                self.structure(word, text);
                return;
            }
        }
        if self.head != Head::Closed && self.frames.is_empty() && self.parens == 0 {
            self.read_head(word, text);
        }
        if self.kind == Kind::Blocks
            && self.frames.is_empty()
            && self.parens == 0
            && self.routines() != Routines::Psm
            && word.is_some_and(|word| word.eq_ignore_ascii_case("DECLARE"))
        {
            // SQL/PSM declares inside a `BEGIN` only: one before any is a
            // PL/SQL trigger's, or Firebird's after a routine's `AS`.
            self.pending = Pending::Declare;
        }
        self.structure(word, text);
    }

    /// Reads a `;`, and says what it does.
    pub(crate) fn semicolon(&mut self) -> Semicolon {
        match self.guess {
            // Not the word waited for.
            Guess::Else | Guess::Cursor { .. } => {
                self.guess = Guess::Told(false);
                return Semicolon::Inside;
            }
            Guess::Spl { .. } => {
                self.guess = Guess::Spl {
                    after_semicolon: true,
                    create: None,
                };
            }
            Guess::None | Guess::Told(_) => {}
        }
        self.resolve(None, "");
        self.head = Head::Closed;
        if let Kind::Open(_) = self.kind {
            self.kind = Kind::Plain;
        }
        let answer = match self.kind {
            Kind::Open(_) | Kind::Plain => Semicolon::Ends,
            Kind::Cursor => {
                self.guess = Guess::Cursor { named: false };
                Semicolon::Unsure
            }
            Kind::Blocks if self.frames.is_empty() && self.ifs > 0 => {
                self.guess = Guess::Else;
                Semicolon::Unsure
            }
            Kind::Blocks if self.frames.is_empty() && self.maybe_spl => {
                // Informix's if an `END PROCEDURE` comes before the next
                // routine: its body until then, counting nothing.
                self.maybe_spl = false;
                self.frames.push(Frame::Spl);
                self.guess = Guess::Spl {
                    after_semicolon: true,
                    create: None,
                };
                Semicolon::Unsure
            }
            Kind::Blocks if self.frames.is_empty() => Semicolon::Ends,
            Kind::PlSql(_) if self.frames.is_empty() => Semicolon::EndsWithin,
            Kind::Blocks | Kind::PlSql(_) | Kind::ToEnd => Semicolon::Inside,
        };
        match answer {
            Semicolon::Inside => {
                // Inside a declaration section, a `;` ends one declaration and
                // the next word starts another.
                if matches!(self.frames.last(), Some(Frame::Declare { begun: false })) {
                    self.item_start = true;
                    self.item_opens = false;
                }
            }
            Semicolon::Ends | Semicolon::EndsWithin => self.reset(),
            // The tokens after it settle it.
            Semicolon::Unsure => {}
        }
        answer
    }

    /// Weighs a token against what a [`Semicolon::Unsure`] waits on. Answers
    /// whether the token is still to be read as part of the statement: not
    /// once it has shown that the statement ended.
    fn weigh(&mut self, word: Option<&str>, text: &str) -> bool {
        let is = |set: &[&str]| word.is_some_and(|word| any(word, set));
        match self.guess {
            Guess::None | Guess::Told(_) => true,
            Guess::Else => {
                let goes_on = is(&["ELSE"]);
                self.guess = Guess::Told(goes_on);
                goes_on
            }
            Guess::Cursor { named: false } => {
                if is(&["BEGIN"]) {
                    self.guess = Guess::Told(true);
                    self.open_cursor_block();
                    true
                } else {
                    // A name, unless it starts a statement of its own.
                    self.guess = if word.is_some_and(|word| {
                        !word.starts_with('$')
                            && !any(word, PSM_BODY_STARTS)
                            && !any(word, AS_STATEMENTS)
                            && !any(word, MORE_STATEMENTS)
                    }) {
                        Guess::Cursor { named: true }
                    } else {
                        Guess::Told(false)
                    };
                    false
                }
            }
            Guess::Cursor { named: true } => {
                // A type, or a default given straight away.
                let goes_on = word.is_some() || text == ":=";
                self.guess = Guess::Told(goes_on);
                if goes_on {
                    self.open_cursor_block();
                }
                goes_on
            }
            Guess::Spl {
                after_semicolon,
                create,
            } => {
                let create = if after_semicolon && is(&["CREATE"]) {
                    Some(0)
                } else if let Some(seen) = create {
                    if is(&["PROCEDURE", "FUNCTION"]) {
                        // The next routine, which no SPL body holds.
                        self.guess = Guess::Told(false);
                        return false;
                    }
                    (!is(OBJECTS) && text != "(" && seen + 1 < CREATE_LOOKAHEAD).then_some(seen + 1)
                } else {
                    None
                };
                self.guess = Guess::Spl {
                    after_semicolon: false,
                    create,
                };
                true
            }
        }
    }

    /// Takes the cursor's `DECLARE` read so far as the first declaration of
    /// a block.
    fn open_cursor_block(&mut self) {
        self.family = Family::Oracle;
        self.kind = Kind::PlSql(Unit::Anonymous);
        // Read as a plain statement's, its query left nothing open.
        self.frames.clear();
        self.parens = 0;
        self.frames.push(Frame::Declare { begun: false });
        self.item_start = false;
        self.item_opens = false;
    }

    /// Settles a [`Pending`] word against the token after it, whose text is
    /// `text`: `word` if it is one, and nothing at all for a `;`. Answers
    /// whether that token still has to be read for itself.
    fn resolve(&mut self, word: Option<&str>, text: &str) -> bool {
        let pending = std::mem::replace(&mut self.pending, Pending::None);
        let is = |set: &[&str]| word.is_some_and(|word| any(word, set));
        match pending {
            Pending::None => true,
            Pending::End if self.frames.last() == Some(&Frame::Spl) => {
                if is(&["PROCEDURE", "FUNCTION"]) {
                    self.frames.pop();
                    if matches!(self.guess, Guess::Spl { .. }) {
                        self.guess = Guess::Told(true);
                    }
                    return false;
                }
                true
            }
            Pending::End => {
                if is(UNCOUNTED_ENDS) || (self.family == Family::MsSql && is(&["CONVERSATION"])) {
                    return false;
                }
                self.frames.pop();
                !is(COUNTED_ENDS)
            }
            Pending::Begin => {
                if !is(MSSQL_BEGIN_STATEMENTS) && word.is_some() {
                    self.frames.push(Frame::Block);
                }
                true
            }
            Pending::Opener => {
                if !is(CALL_SPECS) {
                    self.open_declare();
                }
                true
            }
            Pending::Compound => {
                if is(&["TRIGGER"]) {
                    self.open_declare();
                    return false;
                }
                true
            }
            Pending::Declare => {
                if !text.is_empty() && !text.starts_with('@') {
                    self.family = Family::Oracle;
                    // A unit whose own `AS` is behind it, as a trigger's is.
                    self.kind = Kind::PlSql(Unit::Trigger);
                    self.open_declare();
                }
                true
            }
            Pending::As => {
                if is(PLSQL_DECLARATIONS) {
                    self.open_plsql_unit();
                    self.open_declare();
                } else if word.is_some_and(|word| {
                    !word.starts_with('$')
                        && !any(word, PSM_BODY_STARTS)
                        && !any(word, AS_STATEMENTS)
                        && !any(word, CALL_SPECS)
                }) {
                    self.pending = Pending::AsName;
                }
                true
            }
            Pending::AsName => {
                if word.is_some_and(|word| !any(word, OPERATOR_WORDS)) {
                    // `n NUMBER`: a variable, whose declaration has begun.
                    self.open_plsql_unit();
                    self.frames.push(Frame::Declare { begun: false });
                }
                true
            }
        }
    }

    /// Reads a token of the header of a routine that may be in something
    /// other than SQL/PSM, outside any parentheses: its parameters and the
    /// length of its return type are none of the header's business.
    fn read_head(&mut self, word: Option<&str>, text: &str) {
        let Head::Named { package, returns } = self.head else {
            // `Head::Name`: the routine's name, or the guard before it.
            if let (Head::Name { package }, Some(word)) = (self.head, word)
                && !any(word, NAME_GUARDS)
            {
                self.head = Head::Named {
                    package,
                    returns: false,
                };
            }
            return;
        };
        let Some(word) = word else {
            if text == "." && !returns {
                self.head = Head::Name { package };
            }
            return;
        };
        let routines = self.routines();
        if any(word, &["IS", "AS"]) {
            // No routine of Informix's has either.
            self.maybe_spl = false;
            self.head = Head::Closed;
            if routines == Routines::Unknown {
                // Only PL/SQL writes an `IS` here, but `AS` heads PostgreSQL's
                // quoted body, Firebird's and T-SQL's as well: what follows it
                // tells. A function's `RETURNS` was never PL/SQL's.
                if returns {
                    return;
                }
                if word.eq_ignore_ascii_case("AS") {
                    self.pending = Pending::As;
                    return;
                }
            }
            // The header of a PL/SQL unit, whose `IS` or `AS` the Oracle
            // rules take as its own.
            self.family = Family::Oracle;
            self.kind = Kind::PlSql(Unit::Opens);
        } else if word.eq_ignore_ascii_case("RETURNS") {
            self.head = Head::Named {
                package,
                returns: true,
            };
        } else if routines == Routines::PlSql
            && package
            && (any(word, &["PROCEDURE", "FUNCTION"]) || any(word, PSM_BODY_STARTS))
        {
            // MariaDB's package in SQL/PSM, whose body starts with its first
            // declaration and runs to the `END` that closes the package.
            self.head = Head::Closed;
            self.frames.push(Frame::Block);
        } else if routines == Routines::Unknown && any(word, SPL_MARKS) {
            self.head = Head::Closed;
            self.maybe_spl = false;
            self.frames.push(Frame::Spl);
        } else if any(word, PSM_BODY_STARTS) || (returns && word.eq_ignore_ascii_case("RETURN")) {
            self.head = Head::Closed;
        }
    }

    /// Hands a routine whose header has been read over to the Oracle rules,
    /// its own `IS` or `AS` behind it.
    fn open_plsql_unit(&mut self) {
        self.family = Family::Oracle;
        self.kind = Kind::PlSql(Unit::Opens);
        self.unit_opened = true;
    }

    /// What a body may be written in besides SQL/PSM, by the rules being
    /// read.
    const fn routines(&self) -> Routines {
        match self.family {
            Family::Psm { routines, .. } => routines,
            Family::Oracle | Family::MsSql => Routines::Psm,
        }
    }

    /// Takes the statement's kind one token further.
    fn classify(&self, lead: Lead, word: Option<&str>, text: &str) -> Kind {
        let is = |set: &[&str]| word.is_some_and(|word| any(word, set));
        match (self.family, lead) {
            (Family::Oracle, Lead::Start) => {
                if is(&["DECLARE", "BEGIN"]) {
                    Kind::PlSql(Unit::Anonymous)
                } else if is(&["CREATE"]) {
                    Kind::Open(Lead::Create(0))
                } else {
                    Kind::Plain
                }
            }
            (Family::Oracle, Lead::Create(_)) => {
                if is(PLSQL_MODIFIERS) {
                    Kind::Open(lead)
                } else if is(&["FUNCTION", "PROCEDURE", "PACKAGE"]) {
                    Kind::PlSql(Unit::Opens)
                } else if is(&["TRIGGER"]) {
                    Kind::PlSql(Unit::Trigger)
                } else if is(&["LIBRARY"]) {
                    Kind::PlSql(Unit::Spec)
                } else if is(&["TYPE"]) {
                    Kind::Open(Lead::CreateType)
                } else if is(&["JAVA"]) {
                    Kind::ToEnd
                } else {
                    Kind::Plain
                }
            }
            (Family::Oracle, Lead::CreateType) => Kind::PlSql(if is(&["BODY"]) {
                Unit::Opens
            } else {
                Unit::Spec
            }),

            (Family::MsSql, Lead::Start) => {
                if is(&["CREATE", "ALTER"]) {
                    Kind::Open(Lead::Create(0))
                } else if is(&["BEGIN"]) {
                    Kind::Open(Lead::Begin)
                } else if is(&["IF", "WHILE"]) {
                    Kind::Blocks
                } else {
                    Kind::Plain
                }
            }
            (Family::MsSql, Lead::Create(_)) => {
                if is(&["OR", "ALTER"]) {
                    Kind::Open(lead)
                } else if is(&["PROC", "PROCEDURE", "FUNCTION", "TRIGGER"]) {
                    Kind::ToEnd
                } else {
                    Kind::Plain
                }
            }
            (Family::MsSql, Lead::Begin) => {
                if is(MSSQL_BEGIN_STATEMENTS) {
                    Kind::Plain
                } else {
                    Kind::Blocks
                }
            }

            (Family::Psm { begin, routines }, Lead::Start) => {
                if is(&["CREATE", "ALTER", "RECREATE"]) {
                    Kind::Open(Lead::Create(0))
                } else if is(&["BEGIN"]) && begin != BeginRule::Transaction {
                    Kind::Open(Lead::Begin)
                } else if is(&["DECLARE"]) && routines != Routines::Psm {
                    Kind::Open(Lead::Declare)
                } else if is(&["EXECUTE"]) && routines == Routines::Unknown {
                    Kind::Open(Lead::Execute)
                } else {
                    Kind::Plain
                }
            }
            // An anonymous PL/SQL block, or a statement of its own: Db2's
            // `DECLARE GLOBAL TEMPORARY TABLE`, T-SQL's `DECLARE @n INT`.
            (Family::Psm { .. }, Lead::Declare) => {
                if text.starts_with('@') || is(&["GLOBAL"]) {
                    Kind::Plain
                } else if is(PLSQL_DECLARATIONS) || is(&["BEGIN"]) {
                    Kind::PlSql(Unit::Anonymous)
                } else {
                    Kind::Open(Lead::DeclareName)
                }
            }
            (Family::Psm { routines, .. }, Lead::DeclareName) => {
                if is(CURSOR_DECLARATIONS) {
                    // Snowflake's blocks declare cursors so too.
                    if routines == Routines::Unknown {
                        Kind::Cursor
                    } else {
                        Kind::Plain
                    }
                } else {
                    Kind::PlSql(Unit::Anonymous)
                }
            }
            (Family::Psm { .. }, Lead::Execute) => {
                if is(&["BLOCK"]) {
                    Kind::Blocks
                } else {
                    Kind::Plain
                }
            }
            (Family::Psm { .. }, Lead::Create(seen)) => {
                if is(ROUTINES) {
                    Kind::Blocks
                } else if is(OBJECTS) || text == "(" || seen + 1 >= CREATE_LOOKAHEAD {
                    Kind::Plain
                } else {
                    Kind::Open(Lead::Create(seen + 1))
                }
            }
            (Family::Psm { begin, routines }, Lead::Begin) => match begin {
                BeginRule::NotAtomic if is(&["NOT"]) => Kind::Open(Lead::BeginNot),
                // MariaDB's own `BEGIN` is a transaction's, `BEGIN WORK` at
                // most: one before anything else is `sql_mode=ORACLE`'s block.
                BeginRule::NotAtomic if routines == Routines::PlSql && !is(TRANSACTION_WORDS) => {
                    Kind::PlSql(Unit::Anonymous)
                }
                BeginRule::UnlessTransaction if word.is_some() && !is(TRANSACTION_WORDS) => {
                    Kind::Blocks
                }
                _ => Kind::Plain,
            },
            (Family::Psm { .. }, Lead::BeginNot) => {
                if is(&["ATOMIC"]) {
                    Kind::Blocks
                } else {
                    Kind::Plain
                }
            }

            // A lead one family never enters.
            _ => Kind::Plain,
        }
    }

    /// Opens or closes a level for a token.
    fn structure(&mut self, word: Option<&str>, text: &str) {
        match text {
            "(" => self.parens += 1,
            ")" => self.parens = self.parens.saturating_sub(1),
            _ => {}
        }
        if self.frames.last() == Some(&Frame::Spl) {
            // An SPL body ends at its `END PROCEDURE` or `END FUNCTION`, and
            // nothing inside it is counted.
            if word.is_some_and(|word| word.eq_ignore_ascii_case("END")) {
                self.pending = Pending::End;
            }
            return;
        }
        let Some(word) = word else {
            self.item_start = false;
            return;
        };
        let oracle = self.family == Family::Oracle;
        let at_item_start = std::mem::replace(&mut self.item_start, false);

        if word.eq_ignore_ascii_case("BEGIN") {
            match self.family {
                Family::Oracle => match self.frames.last_mut() {
                    Some(Frame::Declare { begun }) if !*begun => *begun = true,
                    _ => self.frames.push(Frame::Block),
                },
                Family::MsSql => self.pending = Pending::Begin,
                Family::Psm { .. } => self.frames.push(Frame::Block),
            }
        } else if word.eq_ignore_ascii_case("CASE") {
            self.frames.push(Frame::Block);
        } else if word.eq_ignore_ascii_case("END") {
            self.pending = Pending::End;
        } else if oracle && word.eq_ignore_ascii_case("DECLARE") {
            if matches!(self.frames.last(), Some(Frame::Declare { begun: false })) {
                // Firebird's `DECLARE VARIABLE`, a `DECLARE` to each
                // declaration: PL/SQL writes none inside a section.
                self.item_start = true;
            } else {
                self.open_declare();
            }
        } else if oracle && any(word, &["IS", "AS"]) {
            if self.parens > 0 {
                return;
            }
            if self.frames.is_empty() {
                if self.kind == Kind::PlSql(Unit::Opens) && !self.unit_opened {
                    self.unit_opened = true;
                    self.pending = Pending::Opener;
                }
            } else if self.item_opens
                && matches!(self.frames.last(), Some(Frame::Declare { begun: false }))
            {
                self.item_opens = false;
                self.pending = Pending::Opener;
            }
        } else if oracle
            && word.eq_ignore_ascii_case("COMPOUND")
            && self.frames.is_empty()
            && self.kind == Kind::PlSql(Unit::Trigger)
        {
            self.pending = Pending::Compound;
        } else if self.family == Family::MsSql
            && self.kind == Kind::Blocks
            && self.frames.is_empty()
            && self.parens == 0
        {
            if word.eq_ignore_ascii_case("IF") {
                self.ifs += 1;
            } else if word.eq_ignore_ascii_case("ELSE") {
                self.ifs = self.ifs.saturating_sub(1);
            }
        } else if at_item_start {
            self.item_opens = any(word, SUBPROGRAM_HEADS);
        }
    }

    /// Opens a PL/SQL declaration section, whose first word is the head of
    /// its first declaration.
    fn open_declare(&mut self) {
        self.frames.push(Frame::Declare { begun: false });
        self.item_start = true;
        self.item_opens = false;
    }
}

/// Whether `word` is one of `set`, which is upper case.
fn any(word: &str, set: &[&str]) -> bool {
    set.iter()
        .any(|candidate| word.eq_ignore_ascii_case(candidate))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lexer::Lexer;

    /// The SQL of each statement of `script`, as [`Blocks`] cuts it.
    fn cut(script: &str, dialect: &Dialect) -> Vec<String> {
        let mut blocks = Blocks::new(dialect);
        let mut out = Vec::new();
        let mut start = None;
        let mut end = 0;
        let tokens: Vec<_> = Lexer::new(script, dialect)
            .filter(|token| !token.kind.is_trivia())
            .collect();
        for (i, token) in tokens.iter().enumerate() {
            let text = token.text(script);
            if text == ";" && token.kind == TokenKind::Punctuation {
                match blocks.semicolon() {
                    Semicolon::Inside => {
                        end = token.end;
                        continue;
                    }
                    Semicolon::Unsure if !ends_here(&blocks, &tokens[i + 1..], script) => {
                        end = token.end;
                        continue;
                    }
                    Semicolon::Unsure => blocks.reset(),
                    Semicolon::Ends => {}
                    Semicolon::EndsWithin => end = token.end,
                }
                if let Some(from) = start.take() {
                    out.push(script[from..end].to_string());
                }
                continue;
            }
            start.get_or_insert(token.start);
            end = token.end;
            blocks.token(token.kind, text);
        }
        if let Some(from) = start {
            out.push(script[from..end].to_string());
        }
        out
    }

    /// Whether the statement `blocks` was unsure of ended at its `;`, by
    /// the tokens after it.
    fn ends_here(blocks: &Blocks, after: &[crate::lexer::Token], script: &str) -> bool {
        let mut ahead = blocks.clone();
        for token in after {
            let text = token.text(script);
            if text == ";" && token.kind == TokenKind::Punctuation {
                ahead.semicolon();
            } else {
                ahead.token(token.kind, text);
            }
            match ahead.verdict() {
                Verdict::Open => {}
                Verdict::Continues => return false,
                Verdict::Ended => return true,
            }
        }
        true
    }

    #[test]
    fn plain_statements_end_at_every_semicolon() {
        for dialect in [
            Dialect::GENERIC,
            Dialect::POSTGRES,
            Dialect::MYSQL,
            Dialect::ORACLE,
            Dialect::MSSQL,
            Dialect::SQLITE,
        ] {
            assert_eq!(
                cut(
                    "select case when a then 1 end from t; begin; select 2; commit;",
                    &dialect
                )
                .len(),
                if dialect.id() == DialectId::Oracle {
                    // `begin` opens a block on Oracle, whatever follows it.
                    2
                } else {
                    4
                },
                "{:?}",
                dialect.id()
            );
        }
    }

    #[test]
    fn a_routine_word_in_a_view_or_a_table_opens_nothing() {
        assert_eq!(
            cut(
                "create view v as select event, begin from t;
select 1;",
                &Dialect::MYSQL
            )
            .len(),
            2
        );
        assert_eq!(
            cut(
                "create table if not exists log (event int, begin int);
select 1;",
                &Dialect::MYSQL
            )
            .len(),
            2
        );
    }

    #[test]
    fn a_half_typed_case_does_not_swallow_what_follows() {
        assert_eq!(
            cut(
                "select case when a then 1 from t;\nselect 2;",
                &Dialect::MYSQL
            ),
            ["select case when a then 1 from t", "select 2"]
        );
    }

    #[test]
    fn mysql_routines_end_after_their_end() {
        let script = "CREATE DEFINER=`root`@`%` PROCEDURE p(IN n INT)\nBEGIN\n  \
                      DECLARE i INT DEFAULT 0;\n  IF n > 0 THEN SELECT IF(n > 1, 'a', 'b');\n  \
                      END IF;\n  lbl: LOOP\n    SET i = i + 1;\n    IF i > n THEN LEAVE lbl; \
                      END IF;\n  END LOOP lbl;\n  CASE n WHEN 1 THEN SELECT 1; ELSE BEGIN END; \
                      END CASE;\n  SELECT CASE WHEN n THEN 1 END;\nEND;\nCALL p(1);";
        let units = cut(script, &Dialect::MYSQL);
        assert_eq!(units.len(), 2, "{units:#?}");
        assert!(units[0].ends_with("END CASE;\n  SELECT CASE WHEN n THEN 1 END;\nEND"));
        assert_eq!(units[1], "CALL p(1)");

        let trigger = "create trigger t before insert on x for each row begin\n  \
                       set new.a = 1;\n  set new.b = 2;\nend;\nselect 1;";
        assert_eq!(cut(trigger, &Dialect::MARIADB).len(), 2);
        let compound = "begin not atomic\n  select 1;\n  select 2;\nend;\nselect 3;";
        assert_eq!(cut(compound, &Dialect::MARIADB).len(), 2);
        // A routine with no compound body ends at its own `;`.
        assert_eq!(
            cut(
                "create function f() returns int return 1;\nselect f();",
                &Dialect::MYSQL
            ),
            ["create function f() returns int return 1", "select f()"]
        );
    }

    #[test]
    fn postgres_atomic_bodies_and_transactions() {
        let script = "create procedure p() language sql\nbegin atomic\n  insert into t values (1);\n  \
                      insert into t values (case when true then 2 end);\nend;\n\
                      begin;\nselect 1;\ncommit;";
        let units = cut(script, &Dialect::POSTGRES);
        assert_eq!(units.len(), 4, "{units:#?}");
        assert!(units[0].ends_with("end"));
        assert_eq!(units[1], "begin");
    }

    #[test]
    fn sqlite_triggers_count_their_case() {
        let script = "create trigger tr after insert on t begin\n  update t set a = case \
                      when new.a then 1 end;\n  delete from t;\nend;\nselect 1;";
        let units = cut(script, &Dialect::SQLITE);
        assert_eq!(units.len(), 2, "{units:#?}");
        assert!(units[0].ends_with("delete from t;\nend"));
    }

    #[test]
    fn plsql_units_end_after_their_end_with_it() {
        let package = "create or replace package body pkg as\n  g number;\n  \
                       cursor c is select 1 from dual;\n  type r is record (a number);\n  \
                       procedure fwd;\n  procedure p(a number default 1) is\n    \
                       v number := case when a > 0 then 1 end;\n  begin\n    \
                       if a > 0 then null; end if;\n    for i in 1..2 loop null; end loop;\n  \
                       end p;\n  function f return number is\n    \
                       procedure inner is begin null; end;\n  begin\n    return 1;\n  end;\n  \
                       procedure fwd is language java name 'X.y()';\n\
                       begin\n  g := 1;\nexception\n  when others then null;\nend pkg;\n\
                       select 1 from dual;";
        let units = cut(package, &Dialect::ORACLE);
        assert_eq!(units.len(), 2, "{units:#?}");
        assert!(
            units[0].ends_with("end pkg;"),
            "the `;` is part of the unit"
        );
        assert_eq!(units[1], "select 1 from dual");

        let spec = "create package pkg authid definer as\n  procedure p;\n  \
                    function f return number;\nend;\nselect 1 from dual;";
        assert_eq!(cut(spec, &Dialect::ORACLE).len(), 2);

        let anonymous = "declare\n  procedure log(m varchar2) is begin null; end;\n\
                         begin\n  log('x');\nend;\nselect 1 from dual;";
        assert_eq!(cut(anonymous, &Dialect::ORACLE).len(), 2);

        let call_spec = "create function f return number as language java name 'X.f()';\n\
                         select 1 from dual;";
        assert_eq!(cut(call_spec, &Dialect::ORACLE).len(), 2);

        let type_spec = "create type t as object (a number, member function f return number);\n\
                         create type body t as\n  member function f return number is\n  \
                         begin\n    return 1;\n  end;\n  constructor function t return self as \
                         result is begin return; end;\nend;\nselect 1 from dual;";
        assert_eq!(cut(type_spec, &Dialect::ORACLE).len(), 3);

        let trigger = "create trigger t before insert on x referencing new as n\n\
                       for each row\ndeclare\n  v number;\nbegin\n  :n.a := 1;\nend;\n\
                       create trigger c for insert on x compound trigger\n  \
                       before statement is begin null; end before statement;\n  \
                       after each row is begin null; end after each row;\nend c;\n\
                       select 1 from dual;";
        assert_eq!(cut(trigger, &Dialect::ORACLE).len(), 3);

        let conditional = "begin\n  $if dbms_db_version.ver_le_12 $then null; $else null; \
                           $end\nend;\nselect 1 from dual;";
        assert_eq!(cut(conditional, &Dialect::ORACLE).len(), 2);
    }

    #[test]
    fn tsql_routines_run_to_the_end_and_blocks_close() {
        assert_eq!(
            cut(
                "create procedure p as\nbegin\n  select 1;\n  select 2;\nend;\nselect 3;",
                &Dialect::MSSQL
            )
            .len(),
            1
        );
        let script = "if @a = 1\nbegin\n  begin tran;\n  update t set a = 1;\n  commit;\nend;\n\
                      begin try\n  select 1;\nend try\nbegin catch\n  select 2;\nend catch;\n\
                      while 1 = 1 begin break; end;\nbegin tran;\nselect 4;";
        let units = cut(script, &Dialect::MSSQL);
        assert_eq!(units.len(), 5, "{units:#?}");
        assert_eq!(units[3], "begin tran");
    }

    #[test]
    fn a_tsql_if_reads_on_into_its_else() {
        let script = "if @a = 1 select 1;\nelse if @a = 2 select 2;\nelse select 3;\n\
                      if @a = 1 select 4;\nselect 5;\n\
                      if @a = case when @b = 1 then 1 else 2 end select 6; else select 7;\n\
                      select 8; else select 9;";
        assert_eq!(
            cut(script, &Dialect::MSSQL),
            [
                "if @a = 1 select 1;\nelse if @a = 2 select 2;\nelse select 3",
                "if @a = 1 select 4",
                "select 5",
                "if @a = case when @b = 1 then 1 else 2 end select 6; else select 7",
                // Nothing but an `IF` waits for an `ELSE`.
                "select 8",
                "else select 9",
            ]
        );
        // Elsewhere an `IF` is MySQL's function or a body's own statement.
        assert_eq!(cut("select if(1, 2, 3); else;", &Dialect::MYSQL).len(), 2);
    }

    #[test]
    fn plsql_on_mariadb_and_db2_is_read_as_oracle_reads_it() {
        // MariaDB's `sql_mode=ORACLE` and Db2's PL/SQL: a routine's header
        // that ends in `IS` or `AS` opens a declaration section.
        let body = "CREATE OR REPLACE PACKAGE BODY pkg AS\n  CURSOR c IS SELECT 1 FROM t;\n  \
                    n INT := 0;\n  PROCEDURE p IS\n  BEGIN\n    NULL;\n  END p;\n  \
                    FUNCTION f RETURN INT AS\n  BEGIN\n    RETURN n;\n  END;\nBEGIN\n  n := 1;\nEND pkg;";
        for dialect in [Dialect::MARIADB, Dialect::DB2] {
            for unit in [
                "CREATE PACKAGE pkg AS\n  PROCEDURE p;\n  FUNCTION f RETURN INT;\nEND;",
                body,
                "CREATE PROCEDURE p AS\n  n INT;\nBEGIN\n  n := 1;\nEND;",
                "CREATE OR REPLACE PROCEDURE p(a IN INT) IS\n  n INT;\nBEGIN\n  \
                 IF a > 0 THEN n := a; END IF;\nEND p;",
                "CREATE FUNCTION f(a INT) RETURN INT AS\nBEGIN\n  RETURN a;\nEND;",
                "CREATE PROCEDURE IF NOT EXISTS db.load AS\nBEGIN\n  NULL;\nEND;",
            ] {
                let script = format!("{unit}\nSELECT 1;");
                assert_eq!(
                    cut(&script, &dialect),
                    [unit, "SELECT 1"],
                    "{:?}",
                    dialect.id()
                );
            }
        }
        // Their own SQL/PSM is read as before: an `IS` or an `AS` in a body
        // that started without a `BEGIN` is the body's.
        for (dialect, script) in [
            (
                Dialect::MARIADB,
                "CREATE PROCEDURE p() SELECT a AS b FROM t;\nSELECT 1;",
            ),
            (
                Dialect::MARIADB,
                "CREATE PROCEDURE load() SELECT 1 AS a;\nSELECT 1;",
            ),
            (
                Dialect::MARIADB,
                "CREATE FUNCTION f(a INT) RETURNS BOOL RETURN a IS NULL;\nSELECT 1;",
            ),
            (
                Dialect::MARIADB,
                "CREATE PROCEDURE p()\nmain: BEGIN\n  SELECT a AS b FROM t;\nEND main;\nSELECT 1;",
            ),
            (
                Dialect::DB2,
                "CREATE FUNCTION t() RETURNS TABLE (a INT) LANGUAGE SQL\n  \
                 RETURN SELECT x AS a FROM t;\nSELECT 1;",
            ),
        ] {
            assert_eq!(cut(script, &dialect).len(), 2, "{script}");
        }
        // MySQL has no PL/SQL to read.
        assert_eq!(
            cut(
                "CREATE PROCEDURE p AS\n  n INT;\nBEGIN NOT ATOMIC\nEND;",
                &Dialect::MYSQL
            ),
            ["CREATE PROCEDURE p AS\n  n INT", "BEGIN NOT ATOMIC\nEND"]
        );
    }

    #[test]
    fn plsql_blocks_and_triggers_on_mariadb_and_db2_have_no_header_to_tell() {
        for dialect in [Dialect::MARIADB, Dialect::DB2] {
            for unit in [
                "DECLARE\n  n INT;\n  CURSOR c IS SELECT 1 FROM t;\nBEGIN\n  n := 1;\nEND;",
                "DECLARE\n  PROCEDURE log(m VARCHAR2) IS BEGIN NULL; END;\nBEGIN\n  log('x');\nEND;",
                "CREATE OR REPLACE TRIGGER trg BEFORE INSERT ON t FOR EACH ROW\nDECLARE\n  \
                 v INT := 1;\nBEGIN\n  :NEW.a := v;\nEND;",
            ] {
                let script = format!("{unit}\nSELECT 1;");
                assert_eq!(
                    cut(&script, &dialect),
                    [unit, "SELECT 1"],
                    "{:?}",
                    dialect.id()
                );
            }
        }
        // A MariaDB `BEGIN` with anything after it but `WORK` or `NOT ATOMIC`
        // is `sql_mode=ORACLE`'s block.
        assert_eq!(
            cut(
                "BEGIN\n  FOR i IN 1..3 LOOP\n    INSERT INTO t VALUES (i);\n  END LOOP;\nEND;\n\
                 BEGIN;\nBEGIN WORK;\nBEGIN NOT ATOMIC\n  SELECT 1;\nEND;",
                &Dialect::MARIADB
            ),
            [
                "BEGIN\n  FOR i IN 1..3 LOOP\n    INSERT INTO t VALUES (i);\n  END LOOP;\nEND;",
                "BEGIN",
                "BEGIN WORK",
                "BEGIN NOT ATOMIC\n  SELECT 1;\nEND"
            ]
        );
        // A `DECLARE` that is a statement of its own stays one.
        assert_eq!(
            cut(
                "DECLARE GLOBAL TEMPORARY TABLE session.t (a INT);\n\
                 DECLARE c1 CURSOR WITH HOLD FOR SELECT a FROM t;\nSELECT 1;",
                &Dialect::DB2
            )
            .len(),
            3
        );
        // MySQL has neither.
        assert_eq!(
            cut("DECLARE\n  n INT;\nBEGIN\n  n := 1;\nEND;", &Dialect::MYSQL).len(),
            3
        );
    }

    #[test]
    fn a_mariadb_package_in_sql_psm_runs_to_its_end() {
        let spec = "CREATE OR REPLACE PACKAGE pkg\n  PROCEDURE p1();\n  \
                    FUNCTION f1() RETURNS INT;\nEND";
        let body = "CREATE OR REPLACE PACKAGE BODY pkg\n  DECLARE a INT DEFAULT 11;\n  \
                    FUNCTION f1() RETURNS INT\n  BEGIN\n    SET a = a - 1;\n    RETURN a;\n  \
                    END;\n  PROCEDURE p1() SELECT a, f1();\n  SET a = a - 1;\nEND";
        assert_eq!(
            cut(
                &format!("{spec};\n{body};\nSELECT 1;\nCREATE USER package IDENTIFIED BY 'x';"),
                &Dialect::MARIADB
            ),
            [
                spec,
                body,
                "SELECT 1",
                "CREATE USER package IDENTIFIED BY 'x'"
            ]
        );
    }

    #[test]
    fn routines_of_a_product_nobody_named_are_told_by_their_own_marks() {
        let firebird = [
            "CREATE OR ALTER PROCEDURE p (a INT) RETURNS (b INT) AS\n  DECLARE VARIABLE x INT;\n  \
             DECLARE y INT = 0;\nBEGIN\n  IF (a > 0) THEN\n  BEGIN\n    x = a;\n  END\n  \
             SUSPEND;\nEND;",
            "CREATE PROCEDURE p AS\n  DECLARE PROCEDURE sub (a INT) AS\n    DECLARE i INT;\n  \
             BEGIN\n    i = a;\n  END\n  DECLARE FUNCTION f RETURNS INT AS\n  BEGIN\n    \
             RETURN 1;\n  END\nBEGIN\n  EXECUTE PROCEDURE sub(f());\nEND;",
            "CREATE TRIGGER t FOR tbl ACTIVE BEFORE INSERT POSITION 0 AS\n  \
             DECLARE VARIABLE x INT;\nBEGIN\n  NEW.id = GEN_ID(g, 1);\nEND;",
            "EXECUTE BLOCK RETURNS (n INT) AS\n  DECLARE i INT = 0;\nBEGIN\n  WHILE (i < 3) DO\n  \
             BEGIN\n    i = i + 1;\n    SUSPEND;\n  END\nEND;",
            // Without declarations it is SQL/PSM's `BEGIN … END`, as before.
            "RECREATE PROCEDURE p AS\nBEGIN\n  INSERT INTO t VALUES (1);\nEND",
        ];
        let informix = [
            "CREATE FUNCTION f(a INT)\n  RETURNING INT;\n  RETURN a + 1;\nEND FUNCTION\n\
             DOCUMENT 'adds one'\nWITH LISTING IN '/tmp/f.log'",
            "CREATE DBA PROCEDURE p()\n  DEFINE x INT;\n  ON EXCEPTION IN (-206)\n    LET x = 0;\n  \
             END EXCEPTION WITH RESUME;\n  FOREACH SELECT a INTO x FROM t\n    \
             IF x > 0 THEN CONTINUE FOREACH; END IF;\n  END FOREACH;\nEND PROCEDURE",
        ];
        let plsql = [
            "CREATE OR REPLACE PROCEDURE p AS\n  n NUMBER;\nBEGIN\n  n := 1;\nEND;",
            "CREATE OR REPLACE PROCEDURE p IS\n  CURSOR c IS SELECT 1 FROM dual;\nBEGIN\n  NULL;\nEND;",
            "CREATE PACKAGE pkg AS\n  PROCEDURE p;\n  FUNCTION f RETURN NUMBER;\nEND;",
            "DECLARE\n  n NUMBER;\nBEGIN\n  n := 1;\nEND;",
        ];
        for unit in firebird.into_iter().chain(informix).chain(plsql) {
            // A unit the `;` after it is part of shows it.
            let terminator = if unit.ends_with(';') { "" } else { ";" };
            let script = format!("{unit}{terminator}\nSELECT 1;");
            assert_eq!(cut(&script, &Dialect::GENERIC), [unit, "SELECT 1"]);
        }
        // Everything else's `AS` and `DECLARE` are read as before.
        for first in [
            "CREATE PROCEDURE p AS SELECT a AS b FROM t",
            "CREATE PROCEDURE p @a INT AS EXEC q @a",
            "CREATE PROCEDURE p AS DECLARE @x INT",
            "CREATE FUNCTION f() RETURNS int AS $$ select 1 $$ LANGUAGE sql",
            "CREATE FUNCTION add(a, b) AS a + b",
            "CREATE FUNCTION is_null(x) AS x IS NULL",
            "CREATE FUNCTION t() AS TABLE SELECT 1 AS a",
            "DECLARE c NO SCROLL CURSOR FOR SELECT 1",
            "DECLARE @x INT",
            "EXECUTE PROCEDURE p",
        ] {
            let script = format!("{first};\nSELECT 1;\nSELECT 2;");
            assert_eq!(cut(&script, &Dialect::GENERIC)[0], first);
            assert_eq!(cut(&script, &Dialect::GENERIC).len(), 3, "{first}");
        }
    }

    #[test]
    fn what_follows_tells_an_informix_routine_and_a_cursor_block() {
        let routines = [
            "CREATE PROCEDURE raise_prices (per_cent INT)\n  \
             UPDATE stock SET price = price * per_cent;\nEND PROCEDURE",
            "CREATE FUNCTION f(a INT) RETURNS INT;\n  RETURN a + 1;\nEND FUNCTION",
            "CREATE PROCEDURE p()\n  BEGIN\n    INSERT INTO t VALUES (1);\n  END;\n  \
             INSERT INTO t VALUES (2);\nEND PROCEDURE",
        ];
        let blocks = [
            "DECLARE\n  c1 CURSOR FOR SELECT id FROM t;\nBEGIN\n  OPEN c1;\nEND;",
            "DECLARE\n  c1 CURSOR FOR SELECT id FROM t;\n  total INTEGER DEFAULT 0;\nBEGIN\n  \
             RETURN total;\nEND;",
        ];
        for unit in routines {
            assert_eq!(
                cut(&format!("{unit};\nSELECT 1;"), &Dialect::GENERIC),
                [unit, "SELECT 1"]
            );
        }
        for unit in blocks {
            assert_eq!(
                cut(&format!("{unit}\nSELECT 1;"), &Dialect::GENERIC),
                [unit, "SELECT 1"]
            );
        }
        // Without an `END PROCEDURE` before the next routine, a routine ends
        // at its `;` as before, and so does a cursor a statement follows.
        assert_eq!(
            cut(
                "CREATE PROCEDURE p() SELECT 1;\nSELECT 2;\nCREATE PROCEDURE q() SELECT 3;\n\
                 INSERT INTO t VALUES (4);\nCREATE PROCEDURE r()\n  INSERT INTO t VALUES (5);\n\
                 END PROCEDURE;\nDECLARE c CURSOR FOR SELECT 6;\nMOVE NEXT FROM c;\n\
                 DECLARE d CURSOR FOR SELECT 7;",
                &Dialect::GENERIC
            ),
            [
                "CREATE PROCEDURE p() SELECT 1",
                "SELECT 2",
                "CREATE PROCEDURE q() SELECT 3",
                "INSERT INTO t VALUES (4)",
                "CREATE PROCEDURE r()\n  INSERT INTO t VALUES (5);\nEND PROCEDURE",
                "DECLARE c CURSOR FOR SELECT 6",
                "MOVE NEXT FROM c",
                "DECLARE d CURSOR FOR SELECT 7",
            ]
        );
        // Only a product nobody named is read so.
        assert_eq!(
            cut(
                "CREATE PROCEDURE p() SELECT 1;\nSELECT 2;\nEND PROCEDURE;",
                &Dialect::MYSQL
            )
            .len(),
            3
        );
    }

    #[test]
    fn a_routine_an_alter_names_has_its_body_too() {
        assert_eq!(
            cut(
                "ALTER MODULE m ADD PROCEDURE p()\nBEGIN\n  SELECT 1 FROM t;\n  SELECT 2 FROM t;\nEND;\n\
                 SELECT 3 FROM t;",
                &Dialect::DB2
            ),
            [
                "ALTER MODULE m ADD PROCEDURE p()\nBEGIN\n  SELECT 1 FROM t;\n  SELECT 2 FROM t;\nEND",
                "SELECT 3 FROM t"
            ]
        );
        assert_eq!(
            cut(
                "ALTER EVENT e DO BEGIN DELETE FROM t; DELETE FROM u; END;\nSELECT 1;",
                &Dialect::MYSQL
            )
            .len(),
            2
        );
        assert_eq!(
            cut(
                "ALTER TABLE t ADD c INT; ALTER PROCEDURE p COMMENT 'x'; ALTER VIEW v AS SELECT CASE WHEN a THEN 1 END; SELECT 1;",
                &Dialect::MYSQL
            )
            .len(),
            4
        );
    }
}
