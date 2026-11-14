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
//! `FUNCTION`, `TRIGGER`, `EVENT` or `PACKAGE`; on Oracle `DECLARE`, `BEGIN`
//! and the `CREATE` of any stored unit; on SQL Server `IF`, `WHILE` and a
//! `BEGIN` that does not start a transaction. Everything else is
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
//! # SQL Server
//!
//! A `CREATE PROCEDURE`, `FUNCTION` or `TRIGGER` has no end of its own: the
//! body is the rest of the batch, `BEGIN` or not. No `;` ends it
//! ([`Kind::ToEnd`]); the `GO` that ends the batch does, and that is the
//! caller's to see. Oracle's `CREATE JAVA SOURCE` is the same, up to its `/`.
//!
//! # What it gets wrong
//!
//! A label, an `ELSE` with no `BEGIN` after a T-SQL `IF`, MariaDB's top-level
//! `IF … END IF` outside a `BEGIN NOT ATOMIC`: each is cut as a semicolon
//! splitter cuts it, which is no worse than before. Counting the other way —
//! a body that never closes because an `END` was missed — runs the statement
//! to the end of the script, or to SQL\*Plus's `/`, which is where a client
//! that counts nothing would have run it.

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
    },
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
            DialectId::MySql | DialectId::MariaDb => Self::Psm {
                begin: BeginRule::NotAtomic,
            },
            DialectId::Db2 | DialectId::Generic => Self::Psm {
                begin: BeginRule::UnlessTransaction,
            },
            DialectId::Postgres | DialectId::Sqlite | DialectId::H2 => Self::Psm {
                begin: BeginRule::Transaction,
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
}

/// Reads one statement's tokens and says what each of its `;` does.
///
/// Fed every token that is not whitespace, a comment or a `;` through
/// [`Blocks::token`], and every `;` through [`Blocks::semicolon`], which
/// starts the next statement whenever it ends this one. [`Blocks::reset`]
/// starts it over for a caller that dropped what it was reading.
#[derive(Debug, Clone)]
pub(crate) struct Blocks {
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
}

impl Blocks {
    /// A reader for statements in `dialect`.
    pub(crate) fn new(dialect: &Dialect) -> Self {
        Self {
            family: Family::of(dialect),
            kind: Kind::Open(Lead::Start),
            frames: Vec::new(),
            pending: Pending::None,
            parens: 0,
            item_start: false,
            item_opens: false,
            unit_opened: false,
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
    }

    /// Reads one token of the statement: anything but whitespace, a comment
    /// and a `;`.
    pub(crate) fn token(&mut self, kind: TokenKind, text: &str) {
        let word = matches!(
            kind,
            TokenKind::Keyword | TokenKind::Identifier | TokenKind::Function | TokenKind::Type
        )
        .then_some(text);

        if !self.resolve(word) {
            return;
        }
        if let Kind::Open(lead) = self.kind {
            self.kind = self.classify(lead, word, text);
        }
        self.structure(word, text);
    }

    /// Reads a `;`, and says what it does.
    pub(crate) fn semicolon(&mut self) -> Semicolon {
        self.resolve(None);
        if let Kind::Open(_) = self.kind {
            self.kind = Kind::Plain;
        }
        let answer = match self.kind {
            Kind::Open(_) | Kind::Plain => Semicolon::Ends,
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
        }
        answer
    }

    /// Settles a [`Pending`] word against the token after it, `None` for a
    /// `;`. Answers whether that token still has to be read for itself.
    fn resolve(&mut self, word: Option<&str>) -> bool {
        let pending = std::mem::replace(&mut self.pending, Pending::None);
        let is = |set: &[&str]| word.is_some_and(|word| any(word, set));
        match pending {
            Pending::None => true,
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

            (Family::Psm { begin }, Lead::Start) => {
                if is(&["CREATE"]) {
                    Kind::Open(Lead::Create(0))
                } else if is(&["BEGIN"]) && begin != BeginRule::Transaction {
                    Kind::Open(Lead::Begin)
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
            (Family::Psm { begin }, Lead::Begin) => match begin {
                BeginRule::NotAtomic if is(&["NOT"]) => Kind::Open(Lead::BeginNot),
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
            self.open_declare();
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
        for token in Lexer::new(script, dialect) {
            if token.kind.is_trivia() {
                continue;
            }
            let text = token.text(script);
            if text == ";" && token.kind == TokenKind::Punctuation {
                match blocks.semicolon() {
                    Semicolon::Inside => {
                        end = token.end;
                        continue;
                    }
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
}
