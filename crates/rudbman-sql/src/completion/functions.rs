//! Frequently used built-ins, independent of database metadata. Function names
//! stay separate from reserved words: completing COUNT must not make a column
//! called count require identifier quotes or change syntax classification.
use crate::{Dialect, DialectId};

const COMMON: &[&str] = &[
    "ABS", "AVG", "COALESCE", "COUNT", "LOWER", "LTRIM", "MAX", "MIN", "NULLIF", "REPLACE",
    "ROUND", "RTRIM", "SUM", "TRIM", "UPPER",
];

/// A deliberately small list of widely available built-ins, not a claim that
/// these are every function available on every version of a database product.
/// Names are inserted alone, preserving call arguments and special syntax.
pub(super) fn for_dialect(dialect: Dialect) -> impl Iterator<Item = &'static str> {
    let vendor: &[&str] = match dialect.id() {
        DialectId::Generic => &[],
        // https://h2database.com/html/functions.html
        DialectId::H2 => &[
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
        ],
        // https://www.postgresql.org/docs/current/functions.html
        DialectId::Postgres => &[
            "ARRAY_AGG",
            "CEIL",
            "CHAR_LENGTH",
            "CONCAT",
            "DATE_TRUNC",
            "FLOOR",
            "GREATEST",
            "LEAST",
            "LENGTH",
            "NOW",
            "POWER",
            "SQRT",
            "STRING_AGG",
            "SUBSTRING",
            "TO_CHAR",
            "TO_DATE",
        ],
        // https://dev.mysql.com/doc/refman/8.4/en/built-in-function-reference.html
        // https://mariadb.com/docs/server/reference/sql-functions
        DialectId::MySql | DialectId::MariaDb => &[
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
        ],
        // https://sqlite.org/lang_corefunc.html
        // https://sqlite.org/lang_aggfunc.html
        // https://sqlite.org/lang_datefunc.html
        DialectId::Sqlite => &[
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
        ],
        // https://docs.oracle.com/en/database/oracle/oracle-database/19/sqlrf/Single-Row-Functions.html
        DialectId::Oracle => &[
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
        ],
        // https://learn.microsoft.com/en-us/sql/t-sql/functions/functions
        DialectId::MsSql => &[
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
        ],
        // https://www.ibm.com/docs/en/db2/12.1?topic=functions-built-in
        DialectId::Db2 => &[
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
        ],
    };
    COMMON.iter().chain(vendor).copied()
}
