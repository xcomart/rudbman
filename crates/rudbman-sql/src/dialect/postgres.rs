//! PostgreSQL.

use super::{DialectId, Syntax, Vendor};
use crate::block::{BeginRule, Family as Block, Routines};
use crate::ddl::AlterStyle;
use crate::explain::Explain;
use crate::format::ddl::Family as Layout;
use crate::ident::Folding;
use crate::keywords::WordTables;
use crate::script::Client;

/// PostgreSQL.
pub(crate) struct Postgres;

/// PostgreSQL: dollar quotes, `E'...'`, nested block comments, `$1`.
const SYNTAX: Syntax = Syntax {
    nested_block_comments: true,
    e_strings: true,
    dollar_quotes: true,
    numbered_parameters: true,
    colon_parameters: false,
    ..Syntax::BASE
};

/// PostgreSQL-only keywords.
const KEYWORDS: &[&str] = &[
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
const TYPES: &[&str] = &[
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

/// Built-ins SQL completion offers beyond the common ones.
// https://www.postgresql.org/docs/current/functions.html
const FUNCTIONS: &[&str] = &[
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
];

impl Vendor for Postgres {
    fn id(&self) -> DialectId {
        DialectId::Postgres
    }

    fn name(&self) -> &'static str {
        "postgres"
    }

    fn aliases(&self) -> &'static [&'static str] {
        &["postgresql", "pgsql", "pg"]
    }

    fn product(&self) -> &'static str {
        "PostgreSQL"
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
        Folding::Lower
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
        Client::Psql
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

    /// The standard form throughout.
    fn alter_style(&self) -> &'static AlterStyle {
        &AlterStyle::STANDARD
    }
}
