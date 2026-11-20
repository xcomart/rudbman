//! IBM Db2.
//!
//! Written against Db2 for Linux, UNIX and Windows, which is what the
//! container tests run; the IBM i and z/OS members of the family share
//! everything this crate asks about except the page clause's minimum version.

use super::{DialectId, Syntax, Vendor};
use crate::block::{BeginRule, Family as Block, Routines};
use crate::ddl::{AlterStyle, TableRename};
use crate::explain::Explain;
use crate::format::ddl::Family as Layout;
use crate::ident::Folding;
use crate::keywords::WordTables;
use crate::script::Client;

/// IBM Db2.
pub(crate) struct Db2;

/// Db2: nested block comments, no hex literals, `#` inside identifiers,
/// and `:name` host variables.
const SYNTAX: Syntax = Syntax {
    nested_block_comments: true,
    hex_literals: false,
    hash_identifiers: true,
    ..Syntax::BASE
};

/// Db2 keywords, SQL and SQL PL together.
///
/// The SQL PL words (`ELSEIF`, `HANDLER`, `LEAVE`, `SIGNAL`) are here for the
/// reason Oracle's PL/SQL words are: a procedure body is read in the editor
/// even though the splitter does not understand its blocks. `REORG` and
/// `RUNSTATS` are commands rather than SQL, and are here because a Db2 user
/// sends them through `SYSPROC.ADMIN_CMD` often enough to read them colored.
const KEYWORDS: &[&str] = &[
    "ATOMIC",
    "CALL",
    "CONCURRENT",
    "CONTINUE",
    "DATA",
    "DETERMINISTIC",
    "DO",
    "EACH",
    "ELSEIF",
    "EXCLUSIVE",
    "EXIT",
    "GENERATED",
    "GET",
    "GLOBAL",
    "HANDLER",
    "IDENTITY",
    "IMMEDIATE",
    "INCLUDE",
    "INOUT",
    "ITERATE",
    "LANGUAGE",
    "LEAVE",
    "LIMIT",
    "LOCK",
    "LOOP",
    "MERGE",
    "MODE",
    "MODIFIES",
    "NOCACHE",
    "NOCYCLE",
    "OFFSET",
    "OPTIMIZE",
    "ORGANIZE",
    "OUT",
    "READS",
    "REFERENCING",
    "REORG",
    "REPEAT",
    "RESIGNAL",
    "RESULT",
    "RETURNS",
    "RUNSTATS",
    "SIGNAL",
    "SPECIFIC",
    "SQL",
    "SQLSTATE",
    "STATEMENT",
    "SYSIBM",
    "TABLESPACE",
    "UNTIL",
    "VARIABLE",
    "VOLATILE",
];

/// Db2 type names.
const TYPES: &[&str] = &[
    "DBCLOB",
    "DECFLOAT",
    "GRAPHIC",
    "LONG",
    "NCLOB",
    "VARGRAPHIC",
    "XML",
];

/// Built-ins SQL completion offers beyond the common ones.
// https://www.ibm.com/docs/en/db2/12.1?topic=functions-built-in
const FUNCTIONS: &[&str] = &[
    "CEIL",
    "CONCAT",
    "DAYS",
    "DIGITS",
    "FLOOR",
    "HEX",
    "LCASE",
    "LENGTH",
    "LISTAGG",
    "LOCATE",
    "MOD",
    "NVL",
    "POSSTR",
    "POWER",
    "SQRT",
    "SUBSTR",
    "TIMESTAMPDIFF",
    "TO_CHAR",
    "TO_DATE",
    "TRUNCATE",
    "UCASE",
    "VARCHAR_FORMAT",
];

/// Db2: the standard clauses, `RENAME TABLE` for the table, and a
/// `REORG` after the changes that need one.
const ALTER: AlterStyle = AlterStyle {
    table_rename: TableRename::RenameTable,
    reorg: true,
    ..AlterStyle::STANDARD
};

impl Vendor for Db2 {
    fn id(&self) -> DialectId {
        DialectId::Db2
    }

    fn name(&self) -> &'static str {
        "db2"
    }

    fn product(&self) -> &'static str {
        "Db2"
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

    /// `BEGIN ATOMIC` opens a block unless a transaction word follows, and
    /// a body may be PL/SQL.
    fn block_family(&self) -> Block {
        Block::Psm {
            begin: BeginRule::UnlessTransaction,
            routines: Routines::PlSql,
        }
    }

    fn routine_layout(&self) -> Layout {
        Layout::Db2
    }

    fn client(&self) -> Client {
        Client::Clp
    }

    /// The standard `OFFSET … ROWS FETCH FIRST … ROWS ONLY`, which Db2 for
    /// LUW has had since 11.1 and the IBM i and z/OS members spell the same
    /// way, where `LIMIT` is a later addition on two of the three.
    fn page_clause(&self, limit: u64, offset: u64) -> Option<String> {
        Some(format!(
            "OFFSET {offset} ROWS FETCH FIRST {limit} ROWS ONLY"
        ))
    }

    fn reads_by_server_cursor(&self) -> bool {
        false
    }

    /// Refused: Db2's `EXPLAIN` writes into a set of explain tables that have
    /// to be created beforehand, and reads nothing back.
    fn explain(&self, _sql: &str) -> Option<Explain> {
        None
    }

    /// Db2 answers SQL0104N for `DEFAULT VALUES` and has no empty lists
    /// either, but it takes the keyword `DEFAULT` as a value, and every column
    /// a statement leaves out gets its default anyway — so one column set to
    /// its default is a row of nothing but defaults, without the catalog
    /// having to be asked which columns have one.
    fn empty_insert(&self, table: &str, first_column: Option<&str>) -> Option<String> {
        first_column.map(|column| format!("INSERT INTO {table} ({column}) VALUES (DEFAULT)"))
    }

    fn alter_style(&self) -> &'static AlterStyle {
        &ALTER
    }
}
