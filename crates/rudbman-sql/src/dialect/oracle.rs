//! Oracle Database.

use super::{DialectId, Syntax, Vendor};
use crate::block::Family as Block;
use crate::ddl::{AlterStyle, AttrStyle};
use crate::explain::Explain;
use crate::format::ddl::Family as Layout;
use crate::ident::Folding;
use crate::keywords::WordTables;
use crate::script::Client;

/// Oracle Database.
pub(crate) struct Oracle;

/// Oracle: no hex literals, `#` inside identifiers, `:name` binds.
const SYNTAX: Syntax = Syntax {
    hex_literals: false,
    hash_identifiers: true,
    ..Syntax::BASE
};

/// Oracle keywords, SQL and PL/SQL together.
///
/// The PL/SQL words (`LOOP`, `EXCEPTION`, `PACKAGE`, `ELSIF`) are here even
/// though [`crate::split_statements`] does not understand PL/SQL blocks: the
/// splitter's limitation is about `;`, and someone reading a package body still
/// wants it colored.
const KEYWORDS: &[&str] = &[
    "BODY",
    "BULK",
    "COLLECT",
    "CONNECT",
    "CONNECT_BY_ROOT",
    "DBMS_OUTPUT",
    "ELSIF",
    "EXCEPTION",
    "EXCLUSIVE",
    "EXIT",
    "FORALL",
    "LEVEL",
    "LOOP",
    "MERGE",
    "MINUS",
    "NOCOMPRESS",
    "NOCYCLE",
    "NOWAIT",
    "OUT",
    "PACKAGE",
    "PCTFREE",
    "PIVOT",
    "PRAGMA",
    "PRIOR",
    "PURGE",
    "RAISE",
    "RECORD",
    "REF",
    "ROWNUM",
    "SHARE",
    "SIBLINGS",
    "START",
    "STORAGE",
    "SYSDATE",
    "SYSTIMESTAMP",
    "TABLESPACE",
    "UNPIVOT",
    "VARRAY",
];

/// Oracle type names.
const TYPES: &[&str] = &[
    "BFILE",
    "BINARY_DOUBLE",
    "BINARY_FLOAT",
    "LONG",
    "NCLOB",
    "NUMBER",
    "NVARCHAR2",
    "PLS_INTEGER",
    "RAW",
    "ROWID",
    "SIMPLE_INTEGER",
    "UROWID",
    "VARCHAR2",
    "XMLTYPE",
];

/// Built-ins SQL completion offers beyond the common ones.
// https://docs.oracle.com/en/database/oracle/oracle-database/19/sqlrf/Single-Row-Functions.html
const FUNCTIONS: &[&str] = &[
    "ADD_MONTHS",
    "CEIL",
    "CONCAT",
    "DECODE",
    "FLOOR",
    "GREATEST",
    "INSTR",
    "LAST_DAY",
    "LEAST",
    "LENGTH",
    "LISTAGG",
    "MONTHS_BETWEEN",
    "NVL",
    "NVL2",
    "POWER",
    "SQRT",
    "SUBSTR",
    "TO_CHAR",
    "TO_DATE",
    "TO_NUMBER",
    "TRUNC",
];

/// Oracle: a bare `ADD`, the default ahead of `NOT NULL`, and a `MODIFY`
/// that names only what changed.
const ALTER: AlterStyle = AlterStyle {
    add: "ADD",
    default_first: true,
    attrs: AttrStyle::ModifyChanged,
    ..AlterStyle::STANDARD
};

impl Vendor for Oracle {
    fn id(&self) -> DialectId {
        DialectId::Oracle
    }

    fn name(&self) -> &'static str {
        "oracle"
    }

    fn product(&self) -> &'static str {
        "Oracle"
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

    /// PL/SQL.
    fn block_family(&self) -> Block {
        Block::Oracle
    }

    fn routine_layout(&self) -> Layout {
        Layout::Oracle
    }

    fn client(&self) -> Client {
        Client::SqlPlus
    }

    /// Refused: Oracle has `OFFSET … FETCH` only from 12c on and `ROWNUM`
    /// before it.
    fn page_clause(&self, _limit: u64, _offset: u64) -> Option<String> {
        None
    }

    /// Oracle's cursors are server-side already.
    fn reads_by_server_cursor(&self) -> bool {
        false
    }

    /// `EXPLAIN PLAN FOR <sql>`, then the rows `DBMS_XPLAN.DISPLAY()` formats
    /// out of `PLAN_TABLE`. The first statement writes the plan into that
    /// table, which every Oracle since 10g provides as a temporary table
    /// private to the session.
    fn explain(&self, sql: &str) -> Option<Explain> {
        let write = format!("EXPLAIN PLAN FOR {sql}");
        Some(Explain {
            before: vec![write.clone()],
            plan: "SELECT PLAN_TABLE_OUTPUT FROM TABLE(DBMS_XPLAN.DISPLAY())".to_string(),
            after: Vec::new(),
            shown: write,
        })
    }

    /// Oracle has neither form. Its `INSERT INTO t VALUES (DEFAULT, ...)`
    /// needs one `DEFAULT` per column, and a column without a default is a
    /// different statement again — a shape only the catalog could build, and
    /// wrongly at that, since a NOT NULL column with no default has no valid
    /// row to insert here at all.
    fn empty_insert(&self, _table: &str, _first_column: Option<&str>) -> Option<String> {
        None
    }

    fn alter_style(&self) -> &'static AlterStyle {
        &ALTER
    }
}
