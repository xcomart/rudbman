//! Which SQL a buffer is written in, and everything this crate answers
//! differently for it.
//!
//! A [`Dialect`] is a copyable pointer to one product's [`Vendor`]
//! implementation, so passing one around costs nothing and there is no
//! registry to initialize. [`Dialect::from_id`] takes the string in
//! `DriverDef::dialect` (architecture document §8) and answers with a dialect,
//! falling back to [`DialectId::Generic`] for anything it does not know: a
//! driver definition is a hand-editable JSON file, and an unrecognized dialect
//! should degrade to plain SQL rather than fail.
//!
//! Each product lives in a file of its own under `dialect/`: its [`Syntax`]
//! row, its word tables, its completion functions, its `ALTER TABLE`
//! spellings, and the [`Vendor`] implementation that answers everything else.
//! The code that lexes, splits, formats and writes SQL asks the dialect what
//! to do rather than testing which product it is.
//!
//! # Adding a product
//!
//! 1. Write `dialect/<product>.rs`, a unit struct and its [`Vendor`]
//!    implementation, starting from the file of the closest product. Every
//!    method without a default has to be answered, so the compiler lists what
//!    is left to decide.
//! 2. Register it in this file: a [`DialectId`] variant, a `mod` line, a
//!    `Dialect` constant and an entry in [`Dialect::ALL`], which is what
//!    [`Dialect::from_id`] searches.
//! 3. Run the crate's tests. Those that hold for every product iterate over
//!    [`Dialect::ALL`]: names and aliases unique and round-tripping, word
//!    tables sorted.
//!
//! [`Syntax`] is deliberately a flat record of `bool`s rather than a set of
//! methods. It is the whole list of places where the vendors disagree at the
//! *lexical* level, in one screen, and each product's row says in one place
//! which of them it takes.

use std::fmt;

use crate::removal::DropSpelling;

mod db2;
mod generic;
mod h2;
mod mariadb;
mod mssql;
mod mysql;
mod oracle;
mod postgres;
mod sqlite;
mod vendor;

pub(crate) use vendor::Vendor;

/// The SQL dialects rudbman distinguishes.
///
/// The names match the `dialect` strings that appear in `drivers.json`. MariaDB
/// is one of them, and was not always: it read as an alias for
/// [`DialectId::MySql`] until a container test sent MySQL's
/// `ALTER TABLE t DROP CHECK c` to a MariaDB 11 and got SQLSTATE 42000 back.
/// The fork kept MySQL's *lexical* rules to the letter — MySQL's [`Syntax`]
/// row and its word tables are shared, not copied — and differs in exactly one
/// spelling of the [`ddl`](crate::ddl) table, which is a row that record can
/// hold only if MariaDB has an id to hold it under.
///
/// Each variant is answered by one product's file under `dialect/`; adding a
/// variant is the second step of adding a product (module documentation).
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
    /// IBM Db2.
    ///
    /// Written against Db2 for Linux, UNIX and Windows, which is what the
    /// container tests run; the IBM i and z/OS members of the family share
    /// everything this crate asks about except the page clause's minimum
    /// version.
    Db2,
}

impl DialectId {
    /// The canonical `dialect` string for this variant.
    ///
    /// Round-trips through [`Dialect::from_id`].
    pub fn as_str(self) -> &'static str {
        self.dialect().name()
    }

    /// The registered dialect with this id, or [`Dialect::GENERIC`] for one
    /// that is missing from [`Dialect::ALL`].
    pub fn dialect(self) -> Dialect {
        Dialect::ALL
            .iter()
            .copied()
            .find(|dialect| dialect.id() == self)
            .unwrap_or(Dialect::GENERIC)
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
    /// PostgreSQL, SQL Server and Db2. Elsewhere the first `*/` wins, which is
    /// the SQL standard's rule.
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
    /// Everywhere except Oracle, which spells it `HEXTORAW('1F')`, and Db2,
    /// which answers SQL0103N for it and takes the standard `X'1F'` only.
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
    /// SQL Server's `#temp` and `##global` tables, and Oracle and Db2, which
    /// allow `#` inside a name. Mutually exclusive with
    /// [`Self::hash_line_comment`].
    pub hash_identifiers: bool,
}

impl Syntax {
    /// The conservative base every dialect starts from: standard SQL and nothing
    /// else. `?` parameters are not a field because every dialect has them —
    /// this crate is only ever handed SQL on its way to a JDBC driver.
    ///
    /// Each product's row in its own file is this one with the rules it
    /// takes switched.
    pub(crate) const BASE: Self = Self {
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
}

/// A dialect: a pointer to one product's [`Vendor`] implementation.
///
/// Copyable and cheap to build, so callers are free to construct one per call
/// rather than hold it. `Dialect` is what every entry point in this crate takes,
/// and the only thing that makes the crate's answers vendor-specific.
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
#[derive(Clone, Copy)]
pub struct Dialect {
    /// The product's answers.
    vendor: &'static dyn Vendor,
}

impl fmt::Debug for Dialect {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("Dialect").field(&self.id()).finish()
    }
}

impl Default for Dialect {
    fn default() -> Self {
        Self::GENERIC
    }
}

impl PartialEq for Dialect {
    /// Two dialects are equal when they are the same dialect; everything else
    /// is a function of the id.
    fn eq(&self, other: &Self) -> bool {
        self.id() == other.id()
    }
}

impl Eq for Dialect {}

impl Dialect {
    /// Standard SQL.
    pub const GENERIC: Self = Self {
        vendor: &generic::Generic,
    };
    /// H2.
    pub const H2: Self = Self { vendor: &h2::H2 };
    /// PostgreSQL.
    pub const POSTGRES: Self = Self {
        vendor: &postgres::Postgres,
    };
    /// MySQL.
    pub const MYSQL: Self = Self {
        vendor: &mysql::MySql,
    };
    /// MariaDB, which borrows MySQL's lexis and its words and parts company in
    /// a few spellings.
    pub const MARIADB: Self = Self {
        vendor: &mariadb::MariaDb,
    };
    /// SQLite.
    pub const SQLITE: Self = Self {
        vendor: &sqlite::Sqlite,
    };
    /// Oracle Database.
    pub const ORACLE: Self = Self {
        vendor: &oracle::Oracle,
    };
    /// Microsoft SQL Server.
    pub const MSSQL: Self = Self {
        vendor: &mssql::MsSql,
    };
    /// IBM Db2.
    pub const DB2: Self = Self { vendor: &db2::Db2 };

    /// Every dialect this crate knows, standard SQL first.
    ///
    /// The registry: [`Self::from_id`] searches it, and the tests that hold
    /// for every product iterate over it.
    pub const ALL: &'static [Self] = &[
        Self::GENERIC,
        Self::H2,
        Self::POSTGRES,
        Self::MYSQL,
        Self::MARIADB,
        Self::SQLITE,
        Self::ORACLE,
        Self::MSSQL,
        Self::DB2,
    ];

    /// Resolve the `dialect` field of a driver definition.
    ///
    /// Case-insensitive, tolerant of surrounding whitespace, and forgiving of
    /// the spellings people write by hand: each product's
    /// [`aliases`](Vendor::aliases) — `postgresql`, `pgsql` and `pg` all mean
    /// [`DialectId::Postgres`], and `sqlserver` and `tsql` both mean
    /// [`DialectId::MsSql`]. Anything else is [`Self::GENERIC`]. `mariadb` is
    /// *not* among MySQL's: it names [`DialectId::MariaDb`], which is a
    /// dialect of its own.
    pub fn from_id(id: &str) -> Self {
        let id = id.trim();
        // A dozen products with a few spellings each, all short: a linear scan
        // of `eq_ignore_ascii_case` beats anything that would allocate a
        // lowercase copy first.
        Self::ALL
            .iter()
            .copied()
            .find(|dialect| {
                let vendor = dialect.vendor;
                vendor.name().eq_ignore_ascii_case(id)
                    || vendor
                        .aliases()
                        .iter()
                        .any(|alias| alias.eq_ignore_ascii_case(id))
            })
            .unwrap_or(Self::GENERIC)
    }

    /// Which dialect this is.
    pub fn id(self) -> DialectId {
        self.vendor.id()
    }

    /// The canonical `dialect` string, as it would appear in `drivers.json`.
    pub fn name(self) -> &'static str {
        self.vendor.name()
    }

    /// The lexical rules the lexer branches on.
    pub fn syntax(self) -> &'static Syntax {
        self.vendor.syntax()
    }

    /// Whether `word` is a reserved word here. ASCII-case-insensitive.
    pub fn is_keyword(&self, word: &str) -> bool {
        self.vendor.words().is_keyword(word)
    }

    /// Whether `word` is a type name here. ASCII-case-insensitive.
    ///
    /// Checked *after* [`Self::is_keyword`] by the lexer, so a word in both
    /// tables is highlighted as a keyword.
    pub fn is_type(&self, word: &str) -> bool {
        self.vendor.words().is_type(word)
    }

    /// Keywords and type names offered by SQL completion, in uppercase.
    /// A word shared by the common and vendor tables may occur twice.
    pub fn completion_words(self) -> impl Iterator<Item = &'static str> {
        self.vendor.words().completions()
    }

    /// The clause that cuts one page out of an ordered `SELECT`: `limit` rows,
    /// after skipping `offset`.
    ///
    /// Appended after any `ORDER BY`, as the last clause of the statement.
    /// `None` where this crate will not write one, which is a refusal rather
    /// than an oversight: the products that answer it say why in their own
    /// file. A caller that gets `None` has to page some other way — the data
    /// pane keeps the statement's cursor open and fetches from it.
    pub fn page_clause(self, limit: u64, offset: u64) -> Option<String> {
        self.vendor.page_clause(limit, offset)
    }

    /// Whether a grid that has no [`Self::page_clause`] reads its rows through
    /// a server-side cursor rather than the driver's default result set.
    ///
    /// SQL Server only; its file says why.
    pub fn reads_by_server_cursor(self) -> bool {
        self.vendor.reads_by_server_cursor()
    }

    /// How the explorer's `DROP` and `TRUNCATE` are spelled here.
    pub fn drop_spelling(self) -> &'static DropSpelling {
        self.vendor.drop_spelling()
    }

    /// The statement that makes `catalog` or `schema` current, so that the
    /// names a script leaves unqualified land in it: `USE` where the scope is
    /// a catalog, the product's current-schema statement elsewhere.
    ///
    /// `None` where the product has one scope, and where the part it needs is
    /// missing or blank. The names are quoted here.
    pub fn enter_scope(self, catalog: Option<&str>, schema: Option<&str>) -> Option<String> {
        let named = |part: Option<&str>| {
            part.map(str::trim)
                .filter(|name| !name.is_empty())
                .map(|name| self.quote_ident(name).into_owned())
        };
        self.vendor
            .enter_scope(named(catalog).as_deref(), named(schema).as_deref())
    }

    /// The property this product's driver reads a login timeout from, in
    /// milliseconds, beside the standard `loginTimeout` in seconds; `None`
    /// where that one is enough.
    pub fn connect_timeout_property(self) -> Option<&'static str> {
        self.vendor.connect_timeout_property()
    }

    /// The product's answers to everything else this crate asks.
    pub(crate) fn vendor(self) -> &'static dyn Vendor {
        self.vendor
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every canonical name and every alias resolves back to the dialect it
    /// names, and every id back to its registered dialect.
    #[test]
    fn canonical_ids_round_trip() {
        for &d in Dialect::ALL {
            assert_eq!(Dialect::from_id(d.name()), d, "{}", d.name());
            assert_eq!(d.id().dialect(), d, "{}", d.name());
            assert_eq!(d.id().as_str(), d.name());
            for alias in d.vendor().aliases() {
                assert_eq!(Dialect::from_id(alias), d, "{alias}");
            }
        }
    }

    /// No two registered products share an id, a name or an alias, so which
    /// one a spelling resolves to does not depend on the registry's order.
    #[test]
    fn every_dialect_is_registered_once() {
        let mut ids = Vec::new();
        let mut spellings = Vec::new();
        for &d in Dialect::ALL {
            ids.push(d.id());
            spellings.push(d.name().to_ascii_lowercase());
            spellings.extend(d.vendor().aliases().iter().map(|a| a.to_ascii_lowercase()));
        }
        let (id_count, spelling_count) = (ids.len(), spellings.len());
        ids.sort();
        ids.dedup();
        spellings.sort();
        spellings.dedup();
        assert_eq!(ids.len(), id_count, "an id is registered twice");
        assert_eq!(
            spellings.len(),
            spelling_count,
            "a spelling names two products"
        );
        assert_eq!(Dialect::ALL[0], Dialect::GENERIC);
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
        assert_eq!(Dialect::from_id(" DB2"), Dialect::DB2);
        assert_eq!(Dialect::from_id("informix"), Dialect::GENERIC);
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
        assert!(Dialect::DB2.is_keyword("reorg"));
        assert!(!Dialect::ORACLE.is_keyword("reorg"));
        assert!(Dialect::DB2.is_type("vargraphic"));
        assert!(Dialect::DB2.is_type("decfloat"));

        assert!(Dialect::ORACLE.is_type("varchar2"));
        assert!(!Dialect::POSTGRES.is_type("varchar2"));
        assert!(Dialect::POSTGRES.is_type("jsonb"));

        // The common tables reach every dialect.
        for d in [Dialect::GENERIC, Dialect::ORACLE, Dialect::MSSQL] {
            assert!(d.is_keyword("select"));
            assert!(d.is_type("varchar"));
        }
    }

    /// `LIMIT … OFFSET …` where every supported version takes it, and nothing
    /// at all where the spelling depends on a version or an `ORDER BY`.
    #[test]
    fn page_clauses() {
        for d in [
            Dialect::H2,
            Dialect::POSTGRES,
            Dialect::MYSQL,
            Dialect::MARIADB,
            Dialect::SQLITE,
        ] {
            assert_eq!(
                d.page_clause(500, 0).as_deref(),
                Some("LIMIT 500 OFFSET 0"),
                "{}",
                d.name()
            );
            assert_eq!(
                d.page_clause(100, 2_500).as_deref(),
                Some("LIMIT 100 OFFSET 2500"),
                "{}",
                d.name()
            );
        }
        for d in [Dialect::GENERIC, Dialect::ORACLE, Dialect::MSSQL] {
            assert_eq!(d.page_clause(500, 0), None, "{}", d.name());
        }
        assert_eq!(
            Dialect::DB2.page_clause(100, 2_500).as_deref(),
            Some("OFFSET 2500 ROWS FETCH FIRST 100 ROWS ONLY")
        );
    }

    /// Only SQL Server asks for a server cursor; every other dialect either
    /// pages by statement or already has cursors on the server.
    #[test]
    fn only_sql_server_reads_by_server_cursor() {
        for &d in Dialect::ALL {
            assert_eq!(
                d.reads_by_server_cursor(),
                d == Dialect::MSSQL,
                "{}",
                d.name()
            );
        }
    }

    /// A scope is entered by the part the product names it by, quoted, and
    /// a blank part names nothing.
    #[test]
    fn enter_scope_uses_the_part_the_product_needs() {
        let cases = [
            (Dialect::MYSQL, Some("USE `app db`")),
            (Dialect::MARIADB, Some("USE `app db`")),
            (Dialect::MSSQL, Some("USE \"app db\"")),
            (Dialect::POSTGRES, Some("SET search_path TO \"Sales\"")),
            (
                Dialect::ORACLE,
                Some("ALTER SESSION SET CURRENT_SCHEMA = \"Sales\""),
            ),
            (Dialect::H2, Some("SET SCHEMA \"Sales\"")),
            (Dialect::DB2, Some("SET SCHEMA \"Sales\"")),
            (Dialect::SQLITE, None),
            (Dialect::GENERIC, None),
        ];
        for (d, expected) in cases {
            assert_eq!(
                d.enter_scope(Some(" app db "), Some("Sales")).as_deref(),
                expected,
                "{}",
                d.name()
            );
        }
        assert_eq!(Dialect::MYSQL.enter_scope(Some("  "), Some("s")), None);
        assert_eq!(Dialect::POSTGRES.enter_scope(Some("c"), None), None);
    }

    /// Only the drivers that ignore `loginTimeout` get a property of their
    /// own.
    #[test]
    fn connect_timeout_properties() {
        for &d in Dialect::ALL {
            let expected = match d.id() {
                DialectId::MySql | DialectId::MariaDb => Some("connectTimeout"),
                DialectId::Oracle => Some("oracle.net.CONNECT_TIMEOUT"),
                _ => None,
            };
            assert_eq!(d.connect_timeout_property(), expected, "{}", d.name());
        }
    }
}
