//! Which SQL a buffer is written in, and the handful of lexical rules that
//! answer to it.
//!
//! A [`Dialect`] is a copyable token — an id plus two pointers into the static
//! word tables and one into a static [`Syntax`] record — so passing one around
//! costs nothing and there is no registry to initialize. [`Dialect::from_id`]
//! takes the string in `DriverDef::dialect` (architecture document §8) and
//! answers with a dialect, falling back to [`DialectId::Generic`] for anything it
//! does not know: a driver definition is a hand-editable JSON file, and an
//! unrecognized dialect should degrade to plain SQL rather than fail.
//!
//! [`Syntax`] is deliberately a flat record of `bool`s rather than a set of
//! methods on an enum. It is the whole list of places where the vendors disagree
//! at the *lexical* level, in one screen, and adding a dialect means adding one
//! row rather than editing a dozen `match` arms.

use crate::keywords::WordTables;

/// The SQL dialects rudbman distinguishes.
///
/// The names match the `dialect` strings that appear in `drivers.json`. MariaDB
/// is one of them, and was not always: it read as an alias for
/// [`DialectId::MySql`] until a container test sent MySQL's
/// `ALTER TABLE t DROP CHECK c` to a MariaDB 11 and got SQLSTATE 42000 back.
/// The fork kept MySQL's *lexical* rules to the letter — this file's [`Syntax`]
/// row and its word tables are shared, not copied — and differs in exactly one
/// spelling of the [`ddl`](crate::ddl) table, which is a row that record can
/// hold only if MariaDB has an id to hold it under.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum DialectId {
    /// Plain SQL: the common word tables and nothing vendor-specific.
    ///
    /// Also what an unrecognized `dialect` string resolves to.
    Generic,
    /// H2, the embedded database rudbman's own tests connect to.
    H2,
    /// PostgreSQL.
    Postgres,
    /// MySQL.
    MySql,
    /// MariaDB: MySQL's lexis, and its own `DROP CONSTRAINT`.
    MariaDb,
    /// SQLite.
    Sqlite,
    /// Oracle Database.
    Oracle,
    /// Microsoft SQL Server.
    MsSql,
}

impl DialectId {
    /// The canonical `dialect` string for this variant.
    ///
    /// Round-trips through [`Dialect::from_id`].
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Generic => "generic",
            Self::H2 => "h2",
            Self::Postgres => "postgres",
            Self::MySql => "mysql",
            Self::MariaDb => "mariadb",
            Self::Sqlite => "sqlite",
            Self::Oracle => "oracle",
            Self::MsSql => "mssql",
        }
    }
}

/// How a dialect writes comments, strings, identifiers and parameters.
///
/// Every field is a rule the lexer branches on. What is *not* here is anything
/// about grammar: this crate never decides that `SELECT` must be followed by a
/// column list, so nothing in this record could say so.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct Syntax {
    /// `#` starts a comment that runs to the end of the line.
    ///
    /// MySQL only. Everywhere else `#` is an operator character (PostgreSQL's
    /// `#>`) or part of an identifier (see [`Self::hash_identifiers`]).
    pub hash_line_comment: bool,
    /// `--` starts a comment only when the next character is whitespace or the
    /// line ends.
    ///
    /// MySQL only, and it is a real rule rather than a quirk: `a--b` is `a`
    /// minus negative `b` there, and MySQL's own client lexes it that way.
    pub dash_dash_needs_space: bool,
    /// `/* /* */ */` nests, so the first `*/` does not necessarily close.
    ///
    /// PostgreSQL and SQL Server. Elsewhere the first `*/` wins, which is the
    /// SQL standard's rule.
    pub nested_block_comments: bool,
    /// A backslash escapes the next character inside `'...'`.
    ///
    /// MySQL, where `'it\'s'` is one string. Everywhere else the only escape for
    /// a quote is doubling it, and PostgreSQL restricts backslashes to
    /// [`Self::e_strings`].
    pub backslash_escapes: bool,
    /// `E'...'` is a string in which backslashes escape.
    ///
    /// PostgreSQL.
    pub e_strings: bool,
    /// `"..."` is a string literal rather than a quoted identifier.
    ///
    /// MySQL, unless the server runs in `ANSI_QUOTES` mode — which this crate
    /// cannot see, so it assumes the default. Getting this wrong only changes a
    /// color; it cannot change where a statement ends, because both forms end at
    /// the closing quote.
    pub double_quoted_strings: bool,
    /// `` `...` `` quotes an identifier.
    ///
    /// MySQL, and SQLite and H2, which both accept the form for compatibility.
    pub backtick_identifiers: bool,
    /// `[...]` quotes an identifier, with `]]` for a literal `]`.
    ///
    /// SQL Server, and SQLite, which accepts the form for compatibility. Where
    /// it is false, `[` and `]` are punctuation — PostgreSQL's array subscript.
    pub bracket_identifiers: bool,
    /// `$tag$ ... $tag$` is a string that runs until the matching tag.
    ///
    /// PostgreSQL, where it is how function bodies avoid quote-doubling, and H2,
    /// which supports the empty tag `$$ ... $$` only. Treating H2's form as the
    /// general one costs nothing: an H2 script has no `$tag$` for the general
    /// rule to misread.
    pub dollar_quotes: bool,
    /// `0x1F` is a numeric literal.
    ///
    /// Everywhere except Oracle, which spells it `HEXTORAW('1F')`.
    pub hex_literals: bool,
    /// `$1`, `$2` are bind parameters.
    ///
    /// PostgreSQL. Recognized before [`Self::dollar_quotes`], since a digit
    /// cannot start a dollar-quote tag.
    pub numbered_parameters: bool,
    /// `:name` is a bind parameter.
    ///
    /// Oracle and JDBC-style named parameters generally. Never PostgreSQL, where
    /// `::` is the cast operator and `:` alone is array slicing.
    pub colon_parameters: bool,
    /// `@name` is a bind parameter or a session variable.
    ///
    /// SQL Server's `@p`/`@@ROWCOUNT`, MySQL's user variables, SQLite's `@name`.
    pub at_parameters: bool,
    /// `#` may appear in an identifier, including as its first character.
    ///
    /// SQL Server's `#temp` and `##global` tables, and Oracle, which allows `#`
    /// inside a name. Mutually exclusive with [`Self::hash_line_comment`].
    pub hash_identifiers: bool,
}

impl Syntax {
    /// The conservative base every dialect starts from: standard SQL and nothing
    /// else. `?` parameters are not a field because every dialect has them —
    /// this crate is only ever handed SQL on its way to a JDBC driver.
    const BASE: Self = Self {
        hash_line_comment: false,
        dash_dash_needs_space: false,
        nested_block_comments: false,
        backslash_escapes: false,
        e_strings: false,
        double_quoted_strings: false,
        backtick_identifiers: false,
        bracket_identifiers: false,
        dollar_quotes: false,
        hex_literals: true,
        numbered_parameters: false,
        colon_parameters: true,
        at_parameters: false,
        hash_identifiers: false,
    };

    /// Standard SQL, and the fallback for an unknown dialect id.
    const GENERIC: Self = Self::BASE;

    /// H2: backticks and `$$` from its compatibility modes, `:name` parameters.
    const H2: Self = Self {
        backtick_identifiers: true,
        dollar_quotes: true,
        ..Self::BASE
    };

    /// PostgreSQL: dollar quotes, `E'...'`, nested block comments, `$1`.
    const POSTGRES: Self = Self {
        nested_block_comments: true,
        e_strings: true,
        dollar_quotes: true,
        numbered_parameters: true,
        colon_parameters: false,
        ..Self::BASE
    };

    /// MySQL and MariaDB: `#` comments, the `-- ` rule, backslash escapes,
    /// backticks, `"..."` as a string, `@vars`.
    ///
    /// One row for two dialects rather than two identical ones. The fork's
    /// disagreement with MySQL is a `DROP` spelling, and nothing a lexer can
    /// see.
    const MYSQL: Self = Self {
        hash_line_comment: true,
        dash_dash_needs_space: true,
        backslash_escapes: true,
        double_quoted_strings: true,
        backtick_identifiers: true,
        colon_parameters: false,
        at_parameters: true,
        ..Self::BASE
    };

    /// SQLite: every quoting form it accepts for compatibility, `:name` and
    /// `@name` parameters.
    const SQLITE: Self = Self {
        backtick_identifiers: true,
        bracket_identifiers: true,
        at_parameters: true,
        ..Self::BASE
    };

    /// Oracle: no hex literals, `#` inside identifiers, `:name` binds.
    const ORACLE: Self = Self {
        hex_literals: false,
        hash_identifiers: true,
        ..Self::BASE
    };

    /// SQL Server: `[...]` identifiers, `@variables`, `#temp` tables, nested
    /// block comments.
    const MSSQL: Self = Self {
        nested_block_comments: true,
        bracket_identifiers: true,
        colon_parameters: false,
        at_parameters: true,
        hash_identifiers: true,
        ..Self::BASE
    };
}

/// A dialect: an id, its lexical rules, and its word tables.
///
/// Copyable and cheap to build, so callers are free to construct one per call
/// rather than hold it. `Dialect` is what every entry point in this crate takes,
/// and the only thing that makes the lexer's answers vendor-specific.
///
/// ```
/// use rudbman_sql::{Dialect, DialectId};
///
/// let d = Dialect::from_id("postgres");
/// assert_eq!(d.id(), DialectId::Postgres);
/// assert!(d.syntax().dollar_quotes);
///
/// // An unknown id is not an error; it is generic SQL.
/// assert_eq!(Dialect::from_id("cockroach").id(), DialectId::Generic);
/// ```
#[derive(Debug, Clone, Copy)]
pub struct Dialect {
    /// Which dialect this is.
    id: DialectId,
    /// The lexical rules, borrowed from the statics above.
    syntax: &'static Syntax,
    /// The dialect-specific halves of the word tables.
    words: WordTables,
}

impl Default for Dialect {
    fn default() -> Self {
        Self::GENERIC
    }
}

impl PartialEq for Dialect {
    /// Two dialects are equal when they are the same dialect; the tables are a
    /// function of the id.
    fn eq(&self, other: &Self) -> bool {
        self.id == other.id
    }
}

impl Eq for Dialect {}

impl Dialect {
    /// Standard SQL.
    pub const GENERIC: Self = Self {
        id: DialectId::Generic,
        syntax: &Syntax::GENERIC,
        words: WordTables::NONE,
    };
    /// H2.
    pub const H2: Self = Self {
        id: DialectId::H2,
        syntax: &Syntax::H2,
        words: WordTables::H2,
    };
    /// PostgreSQL.
    pub const POSTGRES: Self = Self {
        id: DialectId::Postgres,
        syntax: &Syntax::POSTGRES,
        words: WordTables::POSTGRES,
    };
    /// MySQL.
    pub const MYSQL: Self = Self {
        id: DialectId::MySql,
        syntax: &Syntax::MYSQL,
        words: WordTables::MYSQL,
    };
    /// MariaDB, which borrows MySQL's lexis and its words and keeps only its
    /// own id — the id is what the [`ddl`](crate::ddl) table hangs its one
    /// differing spelling on.
    pub const MARIADB: Self = Self {
        id: DialectId::MariaDb,
        syntax: &Syntax::MYSQL,
        words: WordTables::MYSQL,
    };
    /// SQLite.
    pub const SQLITE: Self = Self {
        id: DialectId::Sqlite,
        syntax: &Syntax::SQLITE,
        words: WordTables::SQLITE,
    };
    /// Oracle Database.
    pub const ORACLE: Self = Self {
        id: DialectId::Oracle,
        syntax: &Syntax::ORACLE,
        words: WordTables::ORACLE,
    };
    /// Microsoft SQL Server.
    pub const MSSQL: Self = Self {
        id: DialectId::MsSql,
        syntax: &Syntax::MSSQL,
        words: WordTables::MSSQL,
    };

    /// Resolve the `dialect` field of a driver definition.
    ///
    /// Case-insensitive, tolerant of surrounding whitespace, and forgiving of
    /// the spellings people write by hand: `postgresql`, `pgsql` and `pg` all
    /// mean [`DialectId::Postgres`], and `sqlserver` and `tsql` both mean
    /// [`DialectId::MsSql`]. Anything else is [`Self::GENERIC`]. `mariadb` is
    /// *not* among them any more: it names [`DialectId::MariaDb`], which is a
    /// dialect of its own.
    pub fn from_id(id: &str) -> Self {
        let id = id.trim();
        // Two dozen candidates at most, all short: a linear scan of
        // `eq_ignore_ascii_case` beats anything that would allocate a lowercase
        // copy first.
        const ALIASES: &[(&str, Dialect)] = &[
            ("generic", Dialect::GENERIC),
            ("sql", Dialect::GENERIC),
            ("ansi", Dialect::GENERIC),
            ("h2", Dialect::H2),
            ("postgres", Dialect::POSTGRES),
            ("postgresql", Dialect::POSTGRES),
            ("pgsql", Dialect::POSTGRES),
            ("pg", Dialect::POSTGRES),
            ("mysql", Dialect::MYSQL),
            ("mariadb", Dialect::MARIADB),
            ("sqlite", Dialect::SQLITE),
            ("sqlite3", Dialect::SQLITE),
            ("oracle", Dialect::ORACLE),
            ("mssql", Dialect::MSSQL),
            ("sqlserver", Dialect::MSSQL),
            ("tsql", Dialect::MSSQL),
        ];
        for (name, dialect) in ALIASES {
            if id.eq_ignore_ascii_case(name) {
                return *dialect;
            }
        }
        Self::GENERIC
    }

    /// Which dialect this is.
    pub const fn id(self) -> DialectId {
        self.id
    }

    /// The canonical `dialect` string, as it would appear in `drivers.json`.
    pub const fn name(self) -> &'static str {
        self.id.as_str()
    }

    /// The lexical rules the lexer branches on.
    pub const fn syntax(self) -> &'static Syntax {
        self.syntax
    }

    /// Whether `word` is a reserved word here. ASCII-case-insensitive.
    pub fn is_keyword(&self, word: &str) -> bool {
        self.words.is_keyword(word)
    }

    /// Whether `word` is a type name here. ASCII-case-insensitive.
    ///
    /// Checked *after* [`Self::is_keyword`] by the lexer, so a word in both
    /// tables is highlighted as a keyword.
    pub fn is_type(&self, word: &str) -> bool {
        self.words.is_type(word)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every canonical name resolves back to the dialect it names.
    #[test]
    fn canonical_ids_round_trip() {
        for d in [
            Dialect::GENERIC,
            Dialect::H2,
            Dialect::POSTGRES,
            Dialect::MYSQL,
            Dialect::MARIADB,
            Dialect::SQLITE,
            Dialect::ORACLE,
            Dialect::MSSQL,
        ] {
            assert_eq!(Dialect::from_id(d.name()), d, "{}", d.name());
        }
    }

    /// MariaDB is its own dialect and borrows MySQL's lexis: everything a
    /// lexer asks about answers the same, and the id does not.
    #[test]
    fn mariadb_is_mysqls_lexis_under_an_id_of_its_own() {
        assert_ne!(Dialect::MARIADB, Dialect::MYSQL);
        assert_eq!(Dialect::MARIADB.syntax(), Dialect::MYSQL.syntax());
        for word in ["straight_join", "auto_increment", "select"] {
            assert!(Dialect::MARIADB.is_keyword(word), "{word}");
        }
        for word in ["mediumtext", "varchar"] {
            assert!(Dialect::MARIADB.is_type(word), "{word}");
        }
    }

    /// The forgiving half of [`Dialect::from_id`].
    #[test]
    fn aliases_and_unknown_ids() {
        assert_eq!(Dialect::from_id("PostgreSQL"), Dialect::POSTGRES);
        // `mariadb` is a name rather than an alias, and still tolerates the
        // whitespace and the capitals a hand-edited file comes with.
        assert_eq!(Dialect::from_id("  MariaDB "), Dialect::MARIADB);
        assert_eq!(Dialect::from_id("SqlServer"), Dialect::MSSQL);
        assert_eq!(Dialect::from_id("db2"), Dialect::GENERIC);
        assert_eq!(Dialect::from_id(""), Dialect::GENERIC);
        assert_eq!(Dialect::default(), Dialect::GENERIC);
    }

    /// The word tables are wired to the right dialect.
    #[test]
    fn dialect_specific_words() {
        assert!(Dialect::POSTGRES.is_keyword("ilike"));
        assert!(!Dialect::MYSQL.is_keyword("ilike"));
        assert!(Dialect::MYSQL.is_keyword("straight_join"));
        assert!(Dialect::ORACLE.is_keyword("connect"));
        assert!(Dialect::ORACLE.is_keyword("rownum"));
        assert!(Dialect::MSSQL.is_keyword("nolock"));
        assert!(Dialect::MSSQL.is_keyword("top"));

        assert!(Dialect::ORACLE.is_type("varchar2"));
        assert!(!Dialect::POSTGRES.is_type("varchar2"));
        assert!(Dialect::POSTGRES.is_type("jsonb"));

        // The common tables reach every dialect.
        for d in [Dialect::GENERIC, Dialect::ORACLE, Dialect::MSSQL] {
            assert!(d.is_keyword("select"));
            assert!(d.is_type("varchar"));
        }
    }
}
