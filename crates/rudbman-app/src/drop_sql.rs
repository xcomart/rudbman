//! The `DROP` and `TRUNCATE` statements the explorer's menu offers.
//!
//! A sibling of [`builder_sql`](crate::builder_sql), and for the same reason:
//! every case worth testing here is a case about the *string* — which keyword a
//! product spells a schema's drop with, which parts a name is qualified by — and
//! none of them needs a window. The shell asks for the statements, shows them
//! in full, and runs exactly what it showed only after the user has typed the
//! object's name back (see `workspace::removal`).
//!
//! # No `CASCADE`, no `IF EXISTS`
//!
//! Every statement is the plain form, which on every product refuses rather
//! than reaches further: a schema that still holds tables, a table another
//! table's key points at, a view something else reads. The refusal is the
//! server's own sentence and it reaches the user unchanged. A `CASCADE` would
//! take other objects with it that the confirmation never named, and an
//! `IF EXISTS` would turn "that is not there any more" into a success.
//!
//! # Names
//!
//! Through [`Dialect::quote_ident`] like every other statement this program
//! writes, and qualified by [`builder_sql::table_parts`], the rule the data pane
//! and the copied `SELECT` already follow — a drop that named its table one way
//! and a read that named it another would be the worst disagreement two menu
//! rows could have.
//!
//! SQL Server is the one exception, and the exception is about *which
//! database*. The explorer lists every database on the server, so an object
//! can sit in a catalog that is not the one the session is using, and the
//! schema-qualified name would then find its namesake in the wrong database.
//! `DROP TABLE` and `TRUNCATE TABLE` take a three-part name there; every other
//! `DROP` refuses a database prefix (Msg 166), so those are run inside the
//! object's own database through its `sys.sp_executesql`.

use rudbman_sql::{Dialect, DialectId};

use crate::builder_sql::table_parts;

/// What one `DROP` removes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DropKind {
    /// A base table.
    Table,
    /// A view.
    View,
    /// A materialized view, which PostgreSQL and Oracle drop by that name.
    MaterializedView,
    /// A sequence.
    Sequence,
    /// A schema — an Oracle user, a MySQL database.
    Schema,
    /// A catalog, on the products that call it a database and can drop one.
    Database,
}

/// One thing to drop, named as the catalog holds it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DropTarget<'a> {
    /// What it is.
    pub kind: DropKind,
    /// The catalog it sits in, or — for a [`DropKind::Database`] — unused.
    pub catalog: Option<&'a str>,
    /// The schema it sits in, or — for a [`DropKind::Schema`] — unused.
    pub schema: Option<&'a str>,
    /// Its own name.
    pub name: &'a str,
}

/// The statement that drops `target`, or `None` where the product has none.
///
/// `None` is a catalog on a product whose catalog is the database file or the
/// server itself (H2, SQLite, Oracle, Db2, the generic profile), and a schema
/// on SQLite, which has no schema to drop. The menu leaves the row out there.
pub fn drop_statement(dialect: &Dialect, target: &DropTarget<'_>) -> Option<String> {
    let id = dialect.id();
    let quote = |name: &str| dialect.quote_ident(name).into_owned();
    let object = |keyword: &str| format!("DROP {keyword} {}", qualified(dialect, target));
    match target.kind {
        DropKind::Table => Some(object("TABLE")),
        DropKind::View => Some(in_database(dialect, target.catalog, object("VIEW"))),
        DropKind::MaterializedView => Some(in_database(
            dialect,
            target.catalog,
            object("MATERIALIZED VIEW"),
        )),
        DropKind::Sequence => Some(in_database(dialect, target.catalog, object("SEQUENCE"))),
        DropKind::Schema => {
            let name = quote(target.name);
            match id {
                DialectId::Sqlite => None,
                // A schema is a user there, and the user is what goes.
                DialectId::Oracle => Some(format!("DROP USER {name}")),
                // `RESTRICT` is not optional on Db2.
                DialectId::Db2 => Some(format!("DROP SCHEMA {name} RESTRICT")),
                // The schema level of the MySQL family is the database.
                DialectId::MySql | DialectId::MariaDb => Some(format!("DROP DATABASE {name}")),
                DialectId::MsSql => Some(in_database(
                    dialect,
                    target.catalog,
                    format!("DROP SCHEMA {name}"),
                )),
                DialectId::Generic | DialectId::H2 | DialectId::Postgres => {
                    Some(format!("DROP SCHEMA {name}"))
                }
            }
        }
        DropKind::Database => match id {
            DialectId::Postgres | DialectId::MySql | DialectId::MariaDb | DialectId::MsSql => {
                Some(format!("DROP DATABASE {}", quote(target.name)))
            }
            DialectId::Generic
            | DialectId::H2
            | DialectId::Sqlite
            | DialectId::Oracle
            | DialectId::Db2 => None,
        },
    }
}

/// The statement that empties one table, keeping the table.
///
/// `TRUNCATE TABLE` everywhere it exists: Db2 also needs `IMMEDIATE`, which
/// says the rows go now rather than at the next commit, and SQLite has no
/// `TRUNCATE` at all — an unqualified `DELETE` is what it optimizes into one.
pub fn truncate_statement(
    dialect: &Dialect,
    catalog: Option<&str>,
    schema: Option<&str>,
    name: &str,
) -> String {
    let table = qualified(
        dialect,
        &DropTarget {
            kind: DropKind::Table,
            catalog,
            schema,
            name,
        },
    );
    match dialect.id() {
        DialectId::Sqlite => format!("DELETE FROM {table}"),
        DialectId::Db2 => format!("TRUNCATE TABLE {table} IMMEDIATE"),
        _ => format!("TRUNCATE TABLE {table}"),
    }
}

/// An object's name as its `DROP` or `TRUNCATE` spells it.
///
/// [`table_parts`] everywhere but a SQL Server table in a named catalog, which
/// takes the three-part name; see the module documentation.
fn qualified(dialect: &Dialect, target: &DropTarget<'_>) -> String {
    fn present(part: Option<&str>) -> Option<&str> {
        part.filter(|part| !part.is_empty())
    }
    if dialect.id() == DialectId::MsSql
        && target.kind == DropKind::Table
        && let (Some(catalog), Some(schema)) = (present(target.catalog), present(target.schema))
    {
        return dialect.qualify([catalog, schema, target.name]);
    }
    dialect.qualify(
        table_parts(target.catalog, target.schema, target.name)
            .iter()
            .map(String::as_str),
    )
}

/// `statement`, run inside `catalog` on SQL Server and unchanged elsewhere.
///
/// The statement is passed to that database's own `sp_executesql` as an
/// `N'…'` literal, its quotes doubled, so it runs there without moving the
/// session — which every pane on this connection shares — off its own
/// database.
fn in_database(dialect: &Dialect, catalog: Option<&str>, statement: String) -> String {
    match catalog.filter(|catalog| !catalog.is_empty()) {
        Some(catalog) if dialect.id() == DialectId::MsSql => format!(
            "EXEC {}.sys.sp_executesql N'{}'",
            dialect.quote_ident(catalog),
            statement.replace('\'', "''")
        ),
        _ => statement,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn target<'a>(
        kind: DropKind,
        catalog: Option<&'a str>,
        schema: Option<&'a str>,
        name: &'a str,
    ) -> DropTarget<'a> {
        DropTarget {
            kind,
            catalog,
            schema,
            name,
        }
    }

    #[test]
    fn an_object_is_dropped_under_the_name_a_read_would_use() {
        let pg = Dialect::POSTGRES;
        // The catalog a PostgreSQL scope carries is the connected database,
        // and the schema is what qualifies.
        let table = target(DropKind::Table, Some("app_db"), Some("app"), "Orders");
        assert_eq!(
            drop_statement(&pg, &table).unwrap(),
            "DROP TABLE app.\"Orders\""
        );
        assert_eq!(
            drop_statement(
                &pg,
                &DropTarget {
                    kind: DropKind::View,
                    ..table
                }
            )
            .unwrap(),
            "DROP VIEW app.\"Orders\""
        );
        assert_eq!(
            drop_statement(
                &pg,
                &DropTarget {
                    kind: DropKind::MaterializedView,
                    ..table
                }
            )
            .unwrap(),
            "DROP MATERIALIZED VIEW app.\"Orders\""
        );
        assert_eq!(
            drop_statement(
                &pg,
                &DropTarget {
                    kind: DropKind::Sequence,
                    ..table
                }
            )
            .unwrap(),
            "DROP SEQUENCE app.\"Orders\""
        );
        // MySQL's catalog is the database, and it stands in for the schema.
        assert_eq!(
            drop_statement(
                &Dialect::MYSQL,
                &target(DropKind::Table, Some("shop"), None, "order")
            )
            .unwrap(),
            "DROP TABLE shop.`order`"
        );
        // SQLite has neither level.
        assert_eq!(
            drop_statement(&Dialect::SQLITE, &target(DropKind::Table, None, None, "t")).unwrap(),
            "DROP TABLE t"
        );
    }

    #[test]
    fn sql_server_reaches_into_the_objects_own_database() {
        let ms = Dialect::MSSQL;
        let table = target(DropKind::Table, Some("Sales"), Some("dbo"), "It's");
        assert_eq!(
            drop_statement(&ms, &table).unwrap(),
            "DROP TABLE Sales.dbo.\"It's\""
        );
        assert_eq!(
            drop_statement(
                &ms,
                &DropTarget {
                    kind: DropKind::View,
                    ..table
                }
            )
            .unwrap(),
            "EXEC Sales.sys.sp_executesql N'DROP VIEW dbo.\"It''s\"'"
        );
        assert_eq!(
            drop_statement(&ms, &target(DropKind::Schema, Some("Sales"), None, "audit")).unwrap(),
            "EXEC Sales.sys.sp_executesql N'DROP SCHEMA audit'"
        );
        assert_eq!(
            drop_statement(&ms, &target(DropKind::Schema, None, None, "audit")).unwrap(),
            "DROP SCHEMA audit"
        );
        assert_eq!(
            truncate_statement(&ms, Some("Sales"), Some("dbo"), "Orders"),
            "TRUNCATE TABLE Sales.dbo.Orders"
        );
        // Only SQL Server: a PostgreSQL view in a named catalog is not wrapped.
        assert_eq!(
            drop_statement(
                &Dialect::POSTGRES,
                &target(DropKind::View, Some("db"), Some("app"), "v")
            )
            .unwrap(),
            "DROP VIEW app.v"
        );
    }

    #[test]
    fn a_schema_is_dropped_the_way_each_product_spells_it() {
        let schema = |dialect: &Dialect| {
            drop_statement(dialect, &target(DropKind::Schema, None, None, "APP"))
        };
        assert_eq!(schema(&Dialect::POSTGRES).unwrap(), "DROP SCHEMA \"APP\"");
        assert_eq!(schema(&Dialect::H2).unwrap(), "DROP SCHEMA APP");
        assert_eq!(schema(&Dialect::GENERIC).unwrap(), "DROP SCHEMA APP");
        assert_eq!(schema(&Dialect::ORACLE).unwrap(), "DROP USER APP");
        assert_eq!(schema(&Dialect::DB2).unwrap(), "DROP SCHEMA APP RESTRICT");
        assert_eq!(schema(&Dialect::MYSQL).unwrap(), "DROP DATABASE APP");
        assert_eq!(schema(&Dialect::MARIADB).unwrap(), "DROP DATABASE APP");
        assert_eq!(schema(&Dialect::SQLITE), None);
    }

    #[test]
    fn only_a_product_with_databases_on_a_server_drops_a_catalog() {
        let database = |dialect: &Dialect| {
            drop_statement(dialect, &target(DropKind::Database, None, None, "shop"))
        };
        assert_eq!(database(&Dialect::MYSQL).unwrap(), "DROP DATABASE shop");
        assert_eq!(database(&Dialect::MARIADB).unwrap(), "DROP DATABASE shop");
        assert_eq!(database(&Dialect::MSSQL).unwrap(), "DROP DATABASE shop");
        assert_eq!(database(&Dialect::POSTGRES).unwrap(), "DROP DATABASE shop");
        for dialect in [
            Dialect::H2,
            Dialect::SQLITE,
            Dialect::ORACLE,
            Dialect::DB2,
            Dialect::GENERIC,
        ] {
            assert_eq!(database(&dialect), None, "{}", dialect.name());
        }
    }

    #[test]
    fn a_table_is_emptied_with_the_products_own_statement() {
        let truncate = |dialect: &Dialect| truncate_statement(dialect, None, Some("APP"), "T");
        assert_eq!(truncate(&Dialect::H2), "TRUNCATE TABLE APP.T");
        assert_eq!(truncate(&Dialect::ORACLE), "TRUNCATE TABLE APP.T");
        assert_eq!(truncate(&Dialect::POSTGRES), "TRUNCATE TABLE \"APP\".\"T\"");
        assert_eq!(truncate(&Dialect::DB2), "TRUNCATE TABLE APP.T IMMEDIATE");
        assert_eq!(truncate(&Dialect::SQLITE), "DELETE FROM APP.T");
    }
}
