//! H2, the embedded database rudbman's own tests connect to.

use super::{DialectId, Syntax, Vendor};
use crate::block::{BeginRule, Family as Block, Routines};
use crate::ddl::{AlterStyle, ColumnRename};
use crate::explain::Explain;
use crate::format::ddl::Family as Layout;
use crate::ident::Folding;
use crate::keywords::WordTables;
use crate::removal::{DropSpelling, RoutineDrop};
use crate::script::Client;

/// H2.
pub(crate) struct H2;

/// H2: backticks and `$$` from its compatibility modes, `:name` parameters.
const SYNTAX: Syntax = Syntax {
    backtick_identifiers: true,
    dollar_quotes: true,
    ..Syntax::BASE
};

/// H2 keywords.
const KEYWORDS: &[&str] = &[
    "ARRAY",
    "ASYMMETRIC",
    "CURRENT_CATALOG",
    "CURRENT_PATH",
    "CURRENT_ROLE",
    "CURRENT_SCHEMA",
    "DAY",
    "GROUPS",
    "HOUR",
    "ILIKE",
    "INTERSECTS",
    "LIMIT",
    "LOCALTIME",
    "LOCALTIMESTAMP",
    "MINUS",
    "MINUTE",
    "MONTH",
    "OFFSET",
    "QUALIFY",
    "REGEXP",
    "ROWNUM",
    "SECOND",
    "SYMMETRIC",
    "SYSTEM_USER",
    "TOP",
    "UESCAPE",
    "VALUE",
    "YEAR",
    "_ROWID_",
];

/// H2 type names.
const TYPES: &[&str] = &[
    "ENUM",
    "GEOMETRY",
    "IDENTITY",
    "JAVA_OBJECT",
    "JSON",
    "UUID",
    "VARCHAR_IGNORECASE",
];

/// Built-ins SQL completion offers beyond the common ones.
// https://h2database.com/html/functions.html
const FUNCTIONS: &[&str] = &[
    "CEIL",
    "CHAR_LENGTH",
    "CONCAT",
    "DATEADD",
    "DATEDIFF",
    "FLOOR",
    "FORMATDATETIME",
    "GREATEST",
    "IFNULL",
    "LEAST",
    "LENGTH",
    "NVL",
    "PARSEDATETIME",
    "POWER",
    "SQRT",
    "SUBSTRING",
];

/// H2: standard except that a rename is a clause of `ALTER COLUMN`.
const ALTER: AlterStyle = AlterStyle {
    column_rename: ColumnRename::AlterRenameTo,
    ..AlterStyle::STANDARD
};

/// H2 calls every routine an alias.
const DROPS: DropSpelling = DropSpelling {
    routines: RoutineDrop::Alias,
    ..DropSpelling::STANDARD
};

impl Vendor for H2 {
    fn id(&self) -> DialectId {
        DialectId::H2
    }

    fn name(&self) -> &'static str {
        "h2"
    }

    fn product(&self) -> &'static str {
        "H2"
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
        Folding::Upper
    }

    fn case_insensitive_names(&self) -> bool {
        false
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
        Client::Plain
    }

    fn page_clause(&self, limit: u64, offset: u64) -> Option<String> {
        Some(format!("LIMIT {limit} OFFSET {offset}"))
    }

    fn reads_by_server_cursor(&self) -> bool {
        false
    }

    fn explain(&self, sql: &str) -> Option<Explain> {
        Some(Explain::prefixed("EXPLAIN", sql))
    }

    /// The standard form.
    fn empty_insert(&self, table: &str, _first_column: Option<&str>) -> Option<String> {
        Some(format!("INSERT INTO {table} DEFAULT VALUES"))
    }

    fn alter_style(&self) -> &'static AlterStyle {
        &ALTER
    }

    fn drop_spelling(&self) -> &'static DropSpelling {
        &DROPS
    }

    fn enter_scope(&self, _catalog: Option<&str>, schema: Option<&str>) -> Option<String> {
        schema.map(|schema| format!("SET SCHEMA {schema}"))
    }

    fn connect_timeout_property(&self) -> Option<&'static str> {
        None
    }
}
