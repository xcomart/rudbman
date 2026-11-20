//! The reserved-word tables: the common pair here, and one pair per dialect in
//! that dialect's own file under `dialect/`.
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
//! separate `keyword` and `type` slots (see `rugpui`'s `editor_theme`), and
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
//! `WITH (...)`, Db2's "Reserved schema names and reserved words" plus the SQL
//! PL control statements, the SQLite "SQL Keywords" page, and H2's
//! `Keywords / Reserved Words`. They are trimmed to what a person actually types: a word nobody has
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

/// The two tables a dialect adds on top of the common ones.
#[derive(Debug, Clone, Copy)]
pub(crate) struct WordTables {
    /// Reserved words specific to this dialect.
    pub(crate) keywords: &'static [&'static str],
    /// Type names specific to this dialect.
    pub(crate) types: &'static [&'static str],
}

/// What a dialect adds on top of the common tables; each product's file holds
/// its own pair.
impl WordTables {
    pub(crate) fn completions(self) -> impl Iterator<Item = &'static str> {
        COMMON_KEYWORDS
            .iter()
            .chain(self.keywords)
            .chain(COMMON_TYPES)
            .chain(self.types)
            .copied()
    }

    /// Nothing beyond the common tables.
    pub(crate) const NONE: Self = Self {
        keywords: &[],
        types: &[],
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

    /// Every table [`contains`] is handed: the common pair and each
    /// registered product's.
    fn all() -> Vec<(String, &'static [&'static str])> {
        let mut all = vec![
            ("COMMON_KEYWORDS".to_string(), COMMON_KEYWORDS),
            ("COMMON_TYPES".to_string(), COMMON_TYPES),
        ];
        for dialect in crate::Dialect::ALL {
            let words = dialect.vendor().words();
            all.push((format!("{} keywords", dialect.name()), words.keywords));
            all.push((format!("{} types", dialect.name()), words.types));
        }
        all
    }

    /// The binary search is only correct on a sorted, duplicate-free table.
    #[test]
    fn tables_are_sorted_and_unique() {
        for (name, table) in all() {
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
        for (name, table) in all() {
            for word in table {
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
        for (_, table) in all() {
            for word in table {
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
