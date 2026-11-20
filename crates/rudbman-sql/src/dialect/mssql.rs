//! Microsoft SQL Server.

use super::{DialectId, Syntax, Vendor};
use crate::block::Family as Block;
use crate::ddl::{AlterStyle, AttrStyle, ColumnRename, TableRename, Unsupported};
use crate::explain::Explain;
use crate::format::ddl::Family as Layout;
use crate::ident::Folding;
use crate::keywords::WordTables;
use crate::removal::DropSpelling;
use crate::script::Client;

/// Microsoft SQL Server.
pub(crate) struct MsSql;

/// SQL Server: `[...]` identifiers, `@variables`, `#temp` tables, nested
/// block comments.
const SYNTAX: Syntax = Syntax {
    nested_block_comments: true,
    bracket_identifiers: true,
    colon_parameters: false,
    at_parameters: true,
    hash_identifiers: true,
    ..Syntax::BASE
};

/// SQL Server keywords, including the table hints of `WITH (...)`.
///
/// `NOLOCK`, `READPAST`, `TABLOCK` and the rest are not reserved words in the
/// grammar, but they appear only in that one position and coloring them as
/// keywords is what every other tool does.
const KEYWORDS: &[&str] = &[
    "APPLY",
    "BREAK",
    "BROWSE",
    "BULK",
    "CATCH",
    "CLUSTERED",
    "COMPUTE",
    "CONTINUE",
    "DBCC",
    "DENY",
    "DISK",
    "EXEC",
    "FILLFACTOR",
    "GOTO",
    "HOLDLOCK",
    "IDENTITY",
    "IDENTITY_INSERT",
    "INCLUDE",
    "MERGE",
    "NOCOUNT",
    "NOLOCK",
    "NONCLUSTERED",
    "OFFSET",
    "OPENQUERY",
    "OPENROWSET",
    "OUTPUT",
    "PAGLOCK",
    "PERSISTED",
    "PIVOT",
    "PRINT",
    "RAISERROR",
    "READPAST",
    "READUNCOMMITTED",
    "RECOMPILE",
    "ROWCOUNT",
    "ROWGUIDCOL",
    "ROWLOCK",
    "SERIALIZABLE",
    "SNAPSHOT",
    "TABLOCK",
    "TABLOCKX",
    "TEXTIMAGE_ON",
    "THROW",
    "TOP",
    "TRY",
    "UNPIVOT",
    "UPDLOCK",
    "WAITFOR",
    "XLOCK",
];

/// SQL Server type names.
const TYPES: &[&str] = &[
    "DATETIME2",
    "DATETIMEOFFSET",
    "GEOGRAPHY",
    "GEOMETRY",
    "HIERARCHYID",
    "IMAGE",
    "MONEY",
    "NTEXT",
    "SMALLDATETIME",
    "SMALLMONEY",
    "SQL_VARIANT",
    "SYSNAME",
    "UNIQUEIDENTIFIER",
    "XML",
];

/// Built-ins SQL completion offers beyond the common ones.
// https://learn.microsoft.com/en-us/sql/t-sql/functions/functions
const FUNCTIONS: &[&str] = &[
    "CEILING",
    "CHARINDEX",
    "CONCAT",
    "CONVERT",
    "DATEADD",
    "DATEDIFF",
    "FLOOR",
    "GETDATE",
    "ISNULL",
    "LEN",
    "POWER",
    "SQRT",
    "STRING_AGG",
    "SUBSTRING",
];

/// SQL Server: a bare `ADD`, `sp_rename` for both renames, and an
/// `ALTER COLUMN` that carries type and nullability together.
const ALTER: AlterStyle = AlterStyle {
    add: "ADD",
    column_rename: ColumnRename::SpRename,
    attrs: AttrStyle::TypeWithNull,
    table_rename: TableRename::SpRename,
    ..AlterStyle::STANDARD
};

/// A database can be dropped, and every drop says which database its object
/// sits in.
const DROPS: DropSpelling = DropSpelling {
    database: true,
    in_catalog: true,
    ..DropSpelling::STANDARD
};

impl Vendor for MsSql {
    fn id(&self) -> DialectId {
        DialectId::MsSql
    }

    fn name(&self) -> &'static str {
        "mssql"
    }

    fn aliases(&self) -> &'static [&'static str] {
        &["sqlserver", "tsql"]
    }

    fn product(&self) -> &'static str {
        "SQL Server"
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
        true
    }

    /// T-SQL.
    fn block_family(&self) -> Block {
        Block::MsSql
    }

    fn routine_layout(&self) -> Layout {
        Layout::MsSql
    }

    fn client(&self) -> Client {
        Client::Sqlcmd
    }

    /// Refused: SQL Server's `OFFSET … FETCH` is an error without an
    /// `ORDER BY` and absent before 2012.
    fn page_clause(&self, _limit: u64, _offset: u64) -> Option<String> {
        None
    }

    /// Its driver streams a default result set until anything else runs on
    /// the connection, then reads the rest of it into memory; a server cursor
    /// fetches one batch per round trip and holds no locks between fetches.
    fn reads_by_server_cursor(&self) -> bool {
        true
    }

    /// `SET SHOWPLAN_TEXT ON`, the statement, `SET SHOWPLAN_TEXT OFF`. While
    /// the option is on the server answers with plans and executes nothing,
    /// which is why the last one has to go out even when the plan fails.
    fn explain(&self, sql: &str) -> Option<Explain> {
        Some(Explain {
            before: vec!["SET SHOWPLAN_TEXT ON".to_string()],
            plan: sql.to_string(),
            after: vec!["SET SHOWPLAN_TEXT OFF".to_string()],
            shown: format!("SET SHOWPLAN_TEXT ON; {sql}"),
        })
    }

    /// The standard form.
    fn empty_insert(&self, table: &str, _first_column: Option<&str>) -> Option<String> {
        Some(format!("INSERT INTO {table} DEFAULT VALUES"))
    }

    fn alter_style(&self) -> &'static AlterStyle {
        &ALTER
    }

    fn unsupported_reason(&self, what: Unsupported) -> Option<&'static str> {
        match what {
            Unsupported::Default => Some(
                "SQL Server holds a default as a separately named constraint rather than \
                 a column attribute, and the catalog does not report that name, so there \
                 is nothing to drop and re-add",
            ),
            _ => None,
        }
    }

    fn drop_spelling(&self) -> &'static DropSpelling {
        &DROPS
    }

    fn enter_scope(&self, catalog: Option<&str>, _schema: Option<&str>) -> Option<String> {
        catalog.map(|catalog| format!("USE {catalog}"))
    }

    fn connect_timeout_property(&self) -> Option<&'static str> {
        None
    }
}
