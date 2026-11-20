//! SQLite.

use super::{DialectId, Syntax, Vendor};
use crate::block::{BeginRule, Family as Block, Routines};
use crate::ddl::{AlterStyle, AttrStyle, DropStyle, Unsupported};
use crate::explain::Explain;
use crate::format::ddl::Family as Layout;
use crate::ident::Folding;
use crate::keywords::WordTables;
use crate::removal::{DropSpelling, RoutineDrop, SchemaDrop, TableTruncate};
use crate::script::Client;

/// SQLite.
pub(crate) struct Sqlite;

/// SQLite: every quoting form it accepts for compatibility, `:name` and
/// `@name` parameters.
const SYNTAX: Syntax = Syntax {
    backtick_identifiers: true,
    bracket_identifiers: true,
    at_parameters: true,
    ..Syntax::BASE
};

/// SQLite keywords.
const KEYWORDS: &[&str] = &[
    "ABORT",
    "ANALYZE",
    "ATTACH",
    "AUTOINCREMENT",
    "CONFLICT",
    "DEFERRABLE",
    "DEFERRED",
    "DETACH",
    "DO",
    "EXCLUSIVE",
    "FAIL",
    "GLOB",
    "IGNORE",
    "IMMEDIATE",
    "INDEXED",
    "INSTEAD",
    "ISNULL",
    "LIMIT",
    "MATCH",
    "NOTHING",
    "NOTNULL",
    "OFFSET",
    "PLAN",
    "PRAGMA",
    "QUERY",
    "RAISE",
    "RECURSIVE",
    "REGEXP",
    "REINDEX",
    "RETURNING",
    "VACUUM",
    "VIRTUAL",
    "WITHOUT",
];

/// SQLite type names.
///
/// Short on purpose: SQLite's declared types are free text mapped onto five
/// storage classes, and the names people write — `INTEGER`, `TEXT`, `REAL`,
/// `BLOB`, `NUMERIC` — are all in the common table already. `ROWID` is here for
/// `WITHOUT ROWID` tables.
const TYPES: &[&str] = &["ROWID"];

/// Built-ins SQL completion offers beyond the common ones.
// https://sqlite.org/lang_corefunc.html
// https://sqlite.org/lang_aggfunc.html
// https://sqlite.org/lang_datefunc.html
const FUNCTIONS: &[&str] = &[
    "DATE",
    "DATETIME",
    "GROUP_CONCAT",
    "IFNULL",
    "INSTR",
    "JULIANDAY",
    "LENGTH",
    "STRFTIME",
    "SUBSTR",
    "TIME",
    "TOTAL",
];

/// SQLite: adds, drops and renames only.
const ALTER: AlterStyle = AlterStyle {
    attrs: AttrStyle::Rebuild,
    constraints: DropStyle::Rebuild,
    ..AlterStyle::STANDARD
};

/// No schema, no `TRUNCATE` and no routines.
const DROPS: DropSpelling = DropSpelling {
    schema: SchemaDrop::None,
    truncate: TableTruncate::DeleteAll,
    routines: RoutineDrop::None,
    ..DropSpelling::STANDARD
};

impl Vendor for Sqlite {
    fn id(&self) -> DialectId {
        DialectId::Sqlite
    }

    fn name(&self) -> &'static str {
        "sqlite"
    }

    fn aliases(&self) -> &'static [&'static str] {
        &["sqlite3"]
    }

    fn product(&self) -> &'static str {
        "SQLite"
    }

    fn syntax(&self) -> &'static Syntax {
        &SYNTAX
    }

    fn words(&self) -> WordTables {
        WordTables {
            keywords: KEYWORDS,
            types: TYPES,
        }
    }

    fn functions(&self) -> &'static [&'static str] {
        FUNCTIONS
    }

    fn folding(&self) -> Folding {
        Folding::Preserve
    }

    fn case_insensitive_names(&self) -> bool {
        true
    }

    fn catalog_first(&self) -> bool {
        false
    }

    fn uses_catalogs(&self) -> bool {
        false
    }

    /// `BEGIN` is always a transaction.
    fn block_family(&self) -> Block {
        Block::Psm {
            begin: BeginRule::Transaction,
            routines: Routines::Psm,
        }
    }

    fn routine_layout(&self) -> Layout {
        Layout::Other
    }

    fn client(&self) -> Client {
        Client::Sqlite
    }

    fn page_clause(&self, limit: u64, offset: u64) -> Option<String> {
        Some(format!("LIMIT {limit} OFFSET {offset}"))
    }

    fn reads_by_server_cursor(&self) -> bool {
        false
    }

    /// `EXPLAIN QUERY PLAN <sql>`, the readable plan rather than the virtual
    /// machine's program, which is what a bare `EXPLAIN` lists.
    fn explain(&self, sql: &str) -> Option<Explain> {
        Some(Explain::prefixed("EXPLAIN QUERY PLAN", sql))
    }

    /// The standard form.
    fn empty_insert(&self, table: &str, _first_column: Option<&str>) -> Option<String> {
        Some(format!("INSERT INTO {table} DEFAULT VALUES"))
    }

    fn alter_style(&self) -> &'static AlterStyle {
        &ALTER
    }

    fn unsupported_reason(&self, _what: Unsupported) -> Option<&'static str> {
        Some(
            "SQLite has no ALTER TABLE form for it, and the table would have to be \
             rebuilt — a new table, a copy of every row, a drop and a rename",
        )
    }

    fn drop_spelling(&self) -> &'static DropSpelling {
        &DROPS
    }

    /// One database, one scope.
    fn enter_scope(&self, _catalog: Option<&str>, _schema: Option<&str>) -> Option<String> {
        None
    }

    fn connect_timeout_property(&self) -> Option<&'static str> {
        None
    }
}
