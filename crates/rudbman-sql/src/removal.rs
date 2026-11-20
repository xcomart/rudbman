//! How each product spells the `DROP` and `TRUNCATE` statements the
//! explorer's menu writes.
//!
//! The statements themselves are written by the application, which knows what
//! the explorer lists; what differs from product to product is the record
//! here, one row per dialect in that dialect's own file, read through
//! [`Dialect::drop_spelling`](crate::Dialect::drop_spelling). The code that
//! writes the statements matches on these fields and never on which product it
//! is, so a product that spells a drop the way an existing one does needs no
//! change there.

/// The per-dialect spellings of the explorer's `DROP` and `TRUNCATE`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct DropSpelling {
    /// How a schema is dropped.
    pub schema: SchemaDrop,
    /// Whether a catalog can be dropped, as `DROP DATABASE`.
    ///
    /// The products whose catalog is a database on the server. Where it is the
    /// database file or the server itself there is nothing a session could
    /// drop.
    pub database: bool,
    /// How a table is emptied.
    pub truncate: TableTruncate,
    /// How a routine is told from its namesakes.
    pub routines: RoutineDrop,
    /// How a trigger is named.
    pub triggers: TriggerDrop,
    /// A drop has to say which database its object sits in.
    ///
    /// SQL Server, whose explorer lists every database on the server, so an
    /// object can sit in a catalog the session is not using and its
    /// schema-qualified name would find a namesake in the wrong one. A table
    /// takes the three-part name there; every other `DROP` refuses a database
    /// prefix (Msg 166), so those run inside the object's own database through
    /// its `sys.sp_executesql`.
    pub in_catalog: bool,
}

impl DropSpelling {
    /// Standard SQL, and what each product's row starts from.
    pub(crate) const STANDARD: Self = Self {
        schema: SchemaDrop::Schema,
        database: false,
        truncate: TableTruncate::Truncate,
        routines: RoutineDrop::Named,
        triggers: TriggerDrop::Named,
        in_catalog: false,
    };
}

/// How a schema is dropped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SchemaDrop {
    /// Not at all: SQLite, which has no schema to drop.
    None,
    /// `DROP SCHEMA s`.
    Schema,
    /// `DROP SCHEMA s RESTRICT`: Db2, where `RESTRICT` is not optional.
    SchemaRestrict,
    /// `DROP USER s`: Oracle, where a schema is a user and the user is what
    /// goes.
    User,
    /// `DROP DATABASE s`: the MySQL family, whose schema level is the
    /// database.
    Database,
}

/// How a table is emptied, keeping the table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TableTruncate {
    /// `TRUNCATE TABLE t`.
    Truncate,
    /// `TRUNCATE TABLE t IMMEDIATE`: Db2, where `IMMEDIATE` says the rows go
    /// now rather than at the next commit.
    Immediate,
    /// `DELETE FROM t`: SQLite, which has no `TRUNCATE` at all and optimizes
    /// an unqualified `DELETE` into one.
    DeleteAll,
}

/// How a function or a procedure is dropped, and what tells it from its
/// namesakes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RoutineDrop {
    /// Not at all: SQLite, which has no routines.
    None,
    /// `DROP FUNCTION r` or `DROP PROCEDURE r`: the name is unique in its
    /// schema.
    Named,
    /// `DROP ALIAS r`: H2, which calls every routine an alias.
    Alias,
    /// `DROP FUNCTION r(arguments)`: PostgreSQL, which overloads a name, so
    /// each overload is dropped with the argument list
    /// `pg_get_function_identity_arguments` wrote for it, under the keyword
    /// its `prokind` says.
    Overloaded,
    /// `DROP SPECIFIC FUNCTION s`: Db2, which overloads too and drops one by
    /// its specific name.
    Specific,
    /// [`Self::Named`], except that a packaged routine cannot be dropped on
    /// its own: Oracle, whose driver files a packaged routine under its
    /// package as the catalog.
    Packaged,
}

/// How a trigger is named in its `DROP`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TriggerDrop {
    /// `DROP TRIGGER t`: the name is unique in its schema.
    Named,
    /// `DROP TRIGGER t ON table`: PostgreSQL, where a trigger's name is only
    /// unique on its table.
    OnTable,
}
