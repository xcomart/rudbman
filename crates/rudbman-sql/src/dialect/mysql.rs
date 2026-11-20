//! MySQL.
//!
//! MariaDB answers most of these the way MySQL does and reads them from here;
//! see its own file for where the two part.

use super::{DialectId, Syntax, Vendor};
use crate::block::{BeginRule, Family as Block, Routines};
use crate::ddl::{AlterStyle, AttrStyle, ColumnRename, DropStyle, Unsupported};
use crate::explain::Explain;
use crate::format::ddl::Family as Layout;
use crate::ident::Folding;
use crate::keywords::WordTables;
use crate::script::Client;

/// MySQL.
pub(crate) struct MySql;

/// MySQL and MariaDB: `#` comments, the `-- ` rule, backslash escapes,
/// backticks, `"..."` as a string, `@vars`.
///
/// One row for two dialects rather than two identical ones. The fork's
/// disagreement with MySQL is a `DROP` spelling, and nothing a lexer can
/// see.
const SYNTAX: Syntax = Syntax {
    hash_line_comment: true,
    dash_dash_needs_space: true,
    backslash_escapes: true,
    double_quoted_strings: true,
    backtick_identifiers: true,
    colon_parameters: false,
    at_parameters: true,
    ..Syntax::BASE
};

/// MySQL and MariaDB keywords.
///
/// The two share this table: MariaDB is a fork that kept MySQL's grammar, and
/// the words it added on its own are not ones people highlight-check.
const KEYWORDS: &[&str] = &[
    "AUTO_INCREMENT",
    "BINLOG",
    "CHANGE",
    "CHARSET",
    "CHECKSUM",
    "DELAYED",
    "DESCRIBE",
    "DISTINCTROW",
    "DIV",
    "DUAL",
    "DUPLICATE",
    "ENGINE",
    "FLUSH",
    "FORCE",
    "FULLTEXT",
    "HIGH_PRIORITY",
    "IGNORE",
    "INFILE",
    "KEYS",
    "KILL",
    "LIMIT",
    "LOAD",
    "LOCK",
    "LOW_PRIORITY",
    "MOD",
    "MODIFY",
    "OFFSET",
    "OPTIMIZE",
    "OUTFILE",
    "PURGE",
    "QUICK",
    "RECURSIVE",
    "REGEXP",
    "REPEAT",
    "REQUIRE",
    "RLIKE",
    "SEPARATOR",
    "SHOW",
    "SIGNED",
    "SPATIAL",
    "SQL_BIG_RESULT",
    "SQL_CALC_FOUND_ROWS",
    "SQL_SMALL_RESULT",
    "STRAIGHT_JOIN",
    "UNLOCK",
    "UNSIGNED",
    "USE",
    "XOR",
    "ZEROFILL",
];

/// MySQL and MariaDB type names.
const TYPES: &[&str] = &[
    "ENUM",
    "GEOMETRY",
    "JSON",
    "LINESTRING",
    "LONGBLOB",
    "LONGTEXT",
    "MEDIUMBLOB",
    "MEDIUMINT",
    "MEDIUMTEXT",
    "MULTILINESTRING",
    "MULTIPOINT",
    "MULTIPOLYGON",
    "POINT",
    "POLYGON",
    "TINYBLOB",
    "TINYTEXT",
    "YEAR",
];

/// Built-ins SQL completion offers beyond the common ones.
// https://dev.mysql.com/doc/refman/8.4/en/built-in-function-reference.html
// https://mariadb.com/docs/server/reference/sql-functions
const FUNCTIONS: &[&str] = &[
    "CEIL",
    "CHAR_LENGTH",
    "CONCAT",
    "CONCAT_WS",
    "DATEDIFF",
    "DATE_ADD",
    "DATE_FORMAT",
    "DATE_SUB",
    "FLOOR",
    "GREATEST",
    "GROUP_CONCAT",
    "IFNULL",
    "INSTR",
    "LEAST",
    "LENGTH",
    "NOW",
    "POWER",
    "SQRT",
    "SUBSTRING",
];

/// MySQL: `CHANGE`/`MODIFY COLUMN` restate the definition, each kind of
/// constraint has its own `DROP`, and a reference names its columns.
pub(super) const ALTER: AlterStyle = AlterStyle {
    named_reference_columns: true,
    column_rename: ColumnRename::Change,
    attrs: AttrStyle::Restate,
    constraints: DropStyle::PerKind { check: "CHECK" },
    ..AlterStyle::STANDARD
};

impl Vendor for MySql {
    fn id(&self) -> DialectId {
        DialectId::MySql
    }

    fn name(&self) -> &'static str {
        "mysql"
    }

    fn product(&self) -> &'static str {
        "MySQL"
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
        true
    }

    fn uses_catalogs(&self) -> bool {
        true
    }

    /// A block only as `BEGIN NOT ATOMIC`.
    fn block_family(&self) -> Block {
        Block::Psm {
            begin: BeginRule::NotAtomic,
            routines: Routines::Psm,
        }
    }

    fn routine_layout(&self) -> Layout {
        Layout::MySql
    }

    fn client(&self) -> Client {
        Client::Mysql
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

    /// The MySQL family rejects `DEFAULT VALUES` and spells it with two empty
    /// lists.
    fn empty_insert(&self, table: &str, _first_column: Option<&str>) -> Option<String> {
        Some(format!("INSERT INTO {table} () VALUES ()"))
    }

    fn alter_style(&self) -> &'static AlterStyle {
        &ALTER
    }

    fn unsupported_reason(&self, what: Unsupported) -> Option<&'static str> {
        match what {
            Unsupported::ReferenceWithoutColumns => Some(
                "this product resolves no reference on its own, so the columns of the \
                 parent's key have to be spelled out, where the others read an omitted \
                 list as that table's own primary key",
            ),
            _ => None,
        }
    }
}
