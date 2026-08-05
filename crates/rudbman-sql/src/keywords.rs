//! The reserved-word tables, one pair per dialect.
//!
//! Every table is a `&[&str]` of ASCII-uppercase words sorted in byte order, and
//! every lookup is a binary search with an ASCII-case-insensitive comparison, so
//! recognizing a word costs a handful of comparisons and allocates nothing. A
//! unit test at the bottom of this file asserts the sortedness and the absence of
//! duplicates that the binary search relies on — get that wrong and the failure
//! is a keyword that silently stops highlighting, which is exactly the kind of
//! bug nobody reports.
//!
//! The split into *keywords* and *types* is not cosmetic: the editor palette has
//! separate `keyword` and `type` slots (see `rudbman-ui`'s `editor_theme`), and
//! `VARCHAR2` wants the second one.
//!
//! Two things are deliberately absent.
//!
//! Built-in **function** names — `COUNT`, `COALESCE`, `NVL`, `GETDATE` — are not
//! listed anywhere. The lexer classifies any word followed by `(` as
//! [`crate::TokenKind::Function`], which covers the built-ins and the user's own
//! stored procedures with one rule and no table to keep current. Adding `COUNT`
//! to a keyword table would make it *stop* being highlighted as a function.
//!
//! **Multi-word** keywords — `CONNECT BY`, `GROUP BY`, `ORDER SIBLINGS BY`,
//! `IS NOT NULL` — are listed as their individual words. This is a token-level
//! lexer; recognizing a phrase would mean carrying parser state, and the
//! highlighting is identical either way.
//!
//! Sources: each vendor's own reserved-word list — PostgreSQL "SQL Key Words"
//! (appendix C), MySQL 8.4 "Keywords and Reserved Words", Oracle Database SQL
//! Language Reference appendix D plus the PL/SQL reserved words, the SQL Server
//! "Reserved Keywords (Transact-SQL)" page plus the table hints of
//! `WITH (...)`, the SQLite "SQL Keywords" page, and H2's `Keywords / Reserved
//! Words`. They are trimmed to what a person actually types: a word nobody has
//! written since SQL-92 costs a comparison on every identifier in the buffer.

use std::cmp::Ordering;

/// Words shared by every dialect this crate knows.
///
/// Roughly the intersection of the five vendor lists: the SQL-92 core plus the
/// window-function vocabulary (`OVER`, `PARTITION`, `PRECEDING`) that all five
/// have carried since their 2012-era releases. Anything one dialect spells
/// differently, or does not have at all, belongs in that dialect's own table.
const COMMON_KEYWORDS: &[&str] = &[
    "ADD",
    "ALL",
    "ALTER",
    "AND",
    "ANY",
    "AS",
    "ASC",
    "AUTHORIZATION",
    "BEGIN",
    "BETWEEN",
    "BOTH",
    "BY",
    "CASCADE",
    "CASE",
    "CAST",
    "CHECK",
    "CLOSE",
    "COLLATE",
    "COLUMN",
    "COMMENT",
    "COMMIT",
    "CONSTRAINT",
    "CREATE",
    "CROSS",
    "CURRENT",
    "CURRENT_DATE",
    "CURRENT_TIME",
    "CURRENT_TIMESTAMP",
    "CURRENT_USER",
    "CURSOR",
    "DATABASE",
    "DECLARE",
    "DEFAULT",
    "DELETE",
    "DESC",
    "DISTINCT",
    "DROP",
    "ELSE",
    "END",
    "ESCAPE",
    "EXCEPT",
    "EXECUTE",
    "EXISTS",
    "EXPLAIN",
    "FALSE",
    "FETCH",
    "FILTER",
    "FIRST",
    "FOLLOWING",
    "FOR",
    "FOREIGN",
    "FROM",
    "FULL",
    "FUNCTION",
    "GRANT",
    "GROUP",
    "HAVING",
    "IF",
    "IN",
    "INDEX",
    "INNER",
    "INSERT",
    "INTERSECT",
    "INTO",
    "IS",
    "JOIN",
    "KEY",
    "LAST",
    "LEADING",
    "LEFT",
    "LIKE",
    "NATURAL",
    "NEXT",
    "NO",
    "NOT",
    "NULL",
    "NULLS",
    "OF",
    "ON",
    "ONLY",
    "OPEN",
    "OR",
    "ORDER",
    "OUTER",
    "OVER",
    "PARTITION",
    "PRECEDING",
    "PRIMARY",
    "PROCEDURE",
    "RANGE",
    "REFERENCES",
    "RENAME",
    "REPLACE",
    "RESTRICT",
    "RETURN",
    "REVOKE",
    "RIGHT",
    "ROLLBACK",
    "ROW",
    "ROWS",
    "SAVEPOINT",
    "SCHEMA",
    "SELECT",
    "SEQUENCE",
    "SESSION_USER",
    "SET",
    "SOME",
    "TABLE",
    "TEMPORARY",
    "THEN",
    "TO",
    "TRAILING",
    "TRANSACTION",
    "TRIGGER",
    "TRUE",
    "TRUNCATE",
    "UNBOUNDED",
    "UNION",
    "UNIQUE",
    "UNKNOWN",
    "UPDATE",
    "USER",
    "USING",
    "VALUES",
    "VIEW",
    "WHEN",
    "WHERE",
    "WHILE",
    "WINDOW",
    "WITH",
];

/// Type names shared by every dialect.
///
/// `PRECISION`, `VARYING` and `ZONE` are here because the types they belong to
/// are written as several words — `DOUBLE PRECISION`, `CHARACTER VARYING`,
/// `TIMESTAMP WITH TIME ZONE` — and a token-level lexer colors the tail words
/// only if they are listed on their own.
const COMMON_TYPES: &[&str] = &[
    "BIGINT",
    "BINARY",
    "BIT",
    "BLOB",
    "BOOL",
    "BOOLEAN",
    "CHAR",
    "CHARACTER",
    "CLOB",
    "DATE",
    "DATETIME",
    "DEC",
    "DECIMAL",
    "DOUBLE",
    "FLOAT",
    "INT",
    "INTEGER",
    "INTERVAL",
    "NCHAR",
    "NUMERIC",
    "NVARCHAR",
    "PRECISION",
    "REAL",
    "SMALLINT",
    "TEXT",
    "TIME",
    "TIMESTAMP",
    "TINYINT",
    "VARBINARY",
    "VARCHAR",
    "VARYING",
    "ZONE",
];

/// PostgreSQL-only keywords.
const POSTGRES_KEYWORDS: &[&str] = &[
    "ABORT",
    "ALWAYS",
    "ANALYSE",
    "ANALYZE",
    "ARRAY",
    "ASYMMETRIC",
    "ATTACH",
    "CONCURRENTLY",
    "CONFLICT",
    "COPY",
    "DEFERRABLE",
    "DEFERRED",
    "DO",
    "ENUM",
    "EXCLUDE",
    "EXTENSION",
    "FREEZE",
    "GENERATED",
    "ILIKE",
    "IMMEDIATE",
    "IMMUTABLE",
    "INHERITS",
    "INITIALLY",
    "ISNULL",
    "LANGUAGE",
    "LATERAL",
    "LEAKPROOF",
    "LIMIT",
    "LISTEN",
    "MATERIALIZED",
    "NOTHING",
    "NOTIFY",
    "NOTNULL",
    "OFFSET",
    "ORDINALITY",
    "OVERLAPS",
    "OWNER",
    "PARALLEL",
    "PLACING",
    "POLICY",
    "RECURSIVE",
    "REFRESH",
    "REINDEX",
    "RETURNING",
    "RETURNS",
    "ROLE",
    "SETOF",
    "SIMILAR",
    "STABLE",
    "STORED",
    "STRICT",
    "SYMMETRIC",
    "TABLESAMPLE",
    "TABLESPACE",
    "UNLOGGED",
    "VACUUM",
    "VARIADIC",
    "VERBOSE",
    "VOLATILE",
    "WITHIN",
];

/// PostgreSQL-only type names, including the geometric and range families.
const POSTGRES_TYPES: &[&str] = &[
    "BIGSERIAL",
    "BOX",
    "BYTEA",
    "CIDR",
    "CIRCLE",
    "DATERANGE",
    "FLOAT4",
    "FLOAT8",
    "INET",
    "INT2",
    "INT4",
    "INT4RANGE",
    "INT8",
    "INT8RANGE",
    "JSON",
    "JSONB",
    "LINE",
    "LSEG",
    "MACADDR",
    "MACADDR8",
    "MONEY",
    "NUMRANGE",
    "OID",
    "PATH",
    "POINT",
    "POLYGON",
    "SERIAL",
    "SERIAL2",
    "SERIAL4",
    "SERIAL8",
    "SMALLSERIAL",
    "TSQUERY",
    "TSRANGE",
    "TSTZRANGE",
    "TSVECTOR",
    "UUID",
    "VARBIT",
    "XML",
];

/// MySQL and MariaDB keywords.
///
/// The two share this table: MariaDB is a fork that kept MySQL's grammar, and
/// the words it added on its own are not ones people highlight-check.
const MYSQL_KEYWORDS: &[&str] = &[
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
const MYSQL_TYPES: &[&str] = &[
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

/// Oracle keywords, SQL and PL/SQL together.
///
/// The PL/SQL words (`LOOP`, `EXCEPTION`, `PACKAGE`, `ELSIF`) are here even
/// though [`crate::split_statements`] does not understand PL/SQL blocks: the
/// splitter's limitation is about `;`, and someone reading a package body still
/// wants it colored.
const ORACLE_KEYWORDS: &[&str] = &[
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
const ORACLE_TYPES: &[&str] = &[
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

/// SQL Server keywords, including the table hints of `WITH (...)`.
///
/// `NOLOCK`, `READPAST`, `TABLOCK` and the rest are not reserved words in the
/// grammar, but they appear only in that one position and coloring them as
/// keywords is what every other tool does.
const MSSQL_KEYWORDS: &[&str] = &[
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
const MSSQL_TYPES: &[&str] = &[
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

/// SQLite keywords.
const SQLITE_KEYWORDS: &[&str] = &[
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
/// `BLOB`, `NUMERIC` — are all in [`COMMON_TYPES`] already. `ROWID` is here for
/// `WITHOUT ROWID` tables.
const SQLITE_TYPES: &[&str] = &["ROWID"];

/// H2 keywords.
const H2_KEYWORDS: &[&str] = &[
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
const H2_TYPES: &[&str] = &[
    "ENUM",
    "GEOMETRY",
    "IDENTITY",
    "JAVA_OBJECT",
    "JSON",
    "UUID",
    "VARCHAR_IGNORECASE",
];

/// The two tables a dialect adds on top of the common ones.
#[derive(Debug, Clone, Copy)]
pub(crate) struct WordTables {
    /// Reserved words specific to this dialect.
    pub(crate) keywords: &'static [&'static str],
    /// Type names specific to this dialect.
    pub(crate) types: &'static [&'static str],
}

/// The dialect-specific tables, in the order of [`crate::DialectId`].
impl WordTables {
    /// Nothing beyond the common tables.
    pub(crate) const NONE: Self = Self {
        keywords: &[],
        types: &[],
    };
    /// PostgreSQL.
    pub(crate) const POSTGRES: Self = Self {
        keywords: POSTGRES_KEYWORDS,
        types: POSTGRES_TYPES,
    };
    /// MySQL and MariaDB.
    pub(crate) const MYSQL: Self = Self {
        keywords: MYSQL_KEYWORDS,
        types: MYSQL_TYPES,
    };
    /// Oracle.
    pub(crate) const ORACLE: Self = Self {
        keywords: ORACLE_KEYWORDS,
        types: ORACLE_TYPES,
    };
    /// SQL Server.
    pub(crate) const MSSQL: Self = Self {
        keywords: MSSQL_KEYWORDS,
        types: MSSQL_TYPES,
    };
    /// SQLite.
    pub(crate) const SQLITE: Self = Self {
        keywords: SQLITE_KEYWORDS,
        types: SQLITE_TYPES,
    };
    /// H2.
    pub(crate) const H2: Self = Self {
        keywords: H2_KEYWORDS,
        types: H2_TYPES,
    };

    /// Whether `word` is a reserved word in this dialect.
    pub(crate) fn is_keyword(&self, word: &str) -> bool {
        contains(COMMON_KEYWORDS, word) || contains(self.keywords, word)
    }

    /// Whether `word` is a type name in this dialect.
    pub(crate) fn is_type(&self, word: &str) -> bool {
        contains(COMMON_TYPES, word) || contains(self.types, word)
    }
}

/// Binary-search `table` for `word`, ignoring ASCII case.
///
/// The table is uppercase and sorted by [`cmp_upper_ascii`], which is the same
/// order as a plain byte sort for uppercase ASCII — so the search agrees with the
/// sortedness the tests assert.
fn contains(table: &[&str], word: &str) -> bool {
    // Anything longer than the longest word in any table cannot match, and
    // identifiers are usually longer than keywords, so this rejects most words
    // before the search starts.
    if word.is_empty() || word.len() > 24 {
        return false;
    }
    table
        .binary_search_by(|entry| cmp_upper_ascii(entry, word))
        .is_ok()
}

/// Compare two words by their ASCII-uppercase form, byte by byte.
///
/// Bytes outside ASCII are compared as they are: keyword tables are pure ASCII,
/// so a word carrying a multi-byte character can only ever compare unequal, which
/// is the right answer for it.
fn cmp_upper_ascii(a: &str, b: &str) -> Ordering {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    let n = a.len().min(b.len());
    for i in 0..n {
        let ord = a[i].to_ascii_uppercase().cmp(&b[i].to_ascii_uppercase());
        if ord != Ordering::Equal {
            return ord;
        }
    }
    a.len().cmp(&b.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every table this module hands to [`contains`].
    const ALL: &[(&str, &[&str])] = &[
        ("COMMON_KEYWORDS", COMMON_KEYWORDS),
        ("COMMON_TYPES", COMMON_TYPES),
        ("POSTGRES_KEYWORDS", POSTGRES_KEYWORDS),
        ("POSTGRES_TYPES", POSTGRES_TYPES),
        ("MYSQL_KEYWORDS", MYSQL_KEYWORDS),
        ("MYSQL_TYPES", MYSQL_TYPES),
        ("ORACLE_KEYWORDS", ORACLE_KEYWORDS),
        ("ORACLE_TYPES", ORACLE_TYPES),
        ("MSSQL_KEYWORDS", MSSQL_KEYWORDS),
        ("MSSQL_TYPES", MSSQL_TYPES),
        ("SQLITE_KEYWORDS", SQLITE_KEYWORDS),
        ("SQLITE_TYPES", SQLITE_TYPES),
        ("H2_KEYWORDS", H2_KEYWORDS),
        ("H2_TYPES", H2_TYPES),
    ];

    /// The binary search is only correct on a sorted, duplicate-free table.
    #[test]
    fn tables_are_sorted_and_unique() {
        for (name, table) in ALL {
            for pair in table.windows(2) {
                assert_eq!(
                    cmp_upper_ascii(pair[0], pair[1]),
                    Ordering::Less,
                    "{name}: {:?} must sort before {:?}",
                    pair[0],
                    pair[1]
                );
            }
        }
    }

    /// The length guard in [`contains`] must not reject a real entry.
    #[test]
    fn no_entry_exceeds_the_length_guard() {
        for (name, table) in ALL {
            for word in *table {
                assert!(word.len() <= 24, "{name}: {word:?} is too long to be found");
                assert!(
                    word.is_ascii() && *word == word.to_ascii_uppercase(),
                    "{name}: {word:?} must be uppercase ASCII"
                );
            }
        }
    }

    /// Every entry is findable, in any case, and near-misses are not.
    #[test]
    fn lookup_is_case_insensitive() {
        for (_, table) in ALL {
            for word in *table {
                assert!(contains(table, word));
                assert!(contains(table, &word.to_ascii_lowercase()));
            }
        }
        assert!(!contains(COMMON_KEYWORDS, "SELECTED"));
        assert!(!contains(COMMON_KEYWORDS, "SELEC"));
        assert!(!contains(COMMON_KEYWORDS, ""));
        assert!(!contains(COMMON_KEYWORDS, "고객"));
    }
}
