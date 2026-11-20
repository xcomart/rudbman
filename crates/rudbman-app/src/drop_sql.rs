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
//!
//! # Routines and triggers
//!
//! The tree lists a routine by its name, and a name is not always enough to
//! drop one. What tells the candidates apart comes from a lookup at drop time
//! ([`Identity`]), and the statement names it:
//!
//! - **PostgreSQL** overloads a name, so each overload is dropped with the
//!   argument list `pg_get_function_identity_arguments` wrote for it, under
//!   the keyword its `prokind` says — a function, a procedure or an
//!   aggregate — whichever folder the driver filed it in. A trigger's name is
//!   only unique on its table, and `DROP TRIGGER … ON` names the table.
//! - **Db2** overloads too, and drops one by its specific name.
//! - **H2** calls every routine an alias.
//! - **Oracle** cannot drop a packaged routine on its own; the package owns it.
//! - Everywhere else a routine's and a trigger's name is unique in its schema,
//!   and the plain `DROP FUNCTION`, `DROP PROCEDURE` or `DROP TRIGGER` is it.
//!   SQLite has no routines.
//!
//! Which of these a product does is its [`Dialect::drop_spelling`] row, kept
//! with the rest of that product's answers in `rudbman-sql`; this module only
//! writes what the row says.

use rudbman_sql::{Dialect, RoutineDrop, SchemaDrop, TableTruncate, TriggerDrop};

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
    /// A function, as its folder files it; PostgreSQL's catalog may say
    /// otherwise through [`Identity::routine_kind`].
    Function,
    /// A procedure, as its folder files it.
    Procedure,
    /// A trigger.
    Trigger,
}

/// What tells one routine or trigger of a name from another, as a lookup at
/// drop time found it. Every member is `None` where the product does not need
/// it, or the lookup did not say.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Identity<'a> {
    /// PostgreSQL: the argument list that picks one overload out of several,
    /// as `pg_get_function_identity_arguments` writes it — empty for none.
    pub arguments: Option<&'a str>,
    /// PostgreSQL: `prokind` — `f` function, `p` procedure, `a` aggregate,
    /// `w` window function.
    pub routine_kind: Option<&'a str>,
    /// Db2: the specific name, unique where the routine's name is not.
    pub specific_name: Option<&'a str>,
    /// Oracle: the package a routine belongs to, which keeps it from being
    /// dropped on its own.
    pub package: Option<&'a str>,
    /// The table a trigger fires on, which PostgreSQL drops it through.
    pub table: Option<&'a str>,
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
    /// What picks a routine or a trigger out of its namesakes; empty for
    /// everything else.
    pub identity: Identity<'a>,
}

impl<'a> DropTarget<'a> {
    /// `kind` named by its scope and name alone.
    pub fn new(
        kind: DropKind,
        catalog: Option<&'a str>,
        schema: Option<&'a str>,
        name: &'a str,
    ) -> Self {
        Self {
            kind,
            catalog,
            schema,
            name,
            identity: Identity::default(),
        }
    }
}

/// The statement that drops `target`, or `None` where the product has none.
///
/// `None` is a catalog on a product whose catalog is the database file or the
/// server itself (H2, SQLite, Oracle, Db2, the generic profile), a schema on
/// SQLite, which has no schema to drop, a routine on SQLite, an Oracle routine
/// inside a package, and a PostgreSQL trigger whose table the lookup did not
/// find. The menu leaves the row out where it can tell in advance.
pub fn drop_statement(dialect: &Dialect, target: &DropTarget<'_>) -> Option<String> {
    let spelling = dialect.drop_spelling();
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
            let statement = match spelling.schema {
                SchemaDrop::None => return None,
                SchemaDrop::Schema => format!("DROP SCHEMA {name}"),
                SchemaDrop::SchemaRestrict => format!("DROP SCHEMA {name} RESTRICT"),
                SchemaDrop::User => format!("DROP USER {name}"),
                SchemaDrop::Database => format!("DROP DATABASE {name}"),
            };
            Some(in_database(dialect, target.catalog, statement))
        }
        DropKind::Function | DropKind::Procedure => routine(dialect, target),
        DropKind::Trigger => trigger(dialect, target),
        DropKind::Database => spelling
            .database
            .then(|| format!("DROP DATABASE {}", quote(target.name))),
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
        &DropTarget::new(DropKind::Table, catalog, schema, name),
    );
    match dialect.drop_spelling().truncate {
        TableTruncate::Truncate => format!("TRUNCATE TABLE {table}"),
        TableTruncate::Immediate => format!("TRUNCATE TABLE {table} IMMEDIATE"),
        TableTruncate::DeleteAll => format!("DELETE FROM {table}"),
    }
}

/// An object's name as its `DROP` or `TRUNCATE` spells it.
///
/// [`table_parts`] everywhere but a SQL Server table in a named catalog, which
/// takes the three-part name; see the module documentation.
fn qualified(dialect: &Dialect, target: &DropTarget<'_>) -> String {
    if dialect.drop_spelling().in_catalog
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

/// `part`, unless it is missing or empty.
fn present(part: Option<&str>) -> Option<&str> {
    part.filter(|part| !part.is_empty())
}

/// `name` in the scope `target` sits in, quoted, the way [`table_parts`]
/// qualifies it: a trigger's table, a Db2 routine's specific name.
fn sibling(dialect: &Dialect, target: &DropTarget<'_>, name: &str) -> String {
    dialect.qualify(
        table_parts(target.catalog, target.schema, name)
            .iter()
            .map(String::as_str),
    )
}

/// A function's or a procedure's `DROP`; see the module documentation.
///
/// The keyword is the catalog's word for it where PostgreSQL gave one, and the
/// folder's everywhere else.
fn routine(dialect: &Dialect, target: &DropTarget<'_>) -> Option<String> {
    let identity = &target.identity;
    let keyword = match (identity.routine_kind, target.kind) {
        (Some("p"), _) => "PROCEDURE",
        (Some("a"), _) => "AGGREGATE",
        (Some(_), _) => "FUNCTION",
        (None, DropKind::Procedure) => "PROCEDURE",
        (None, _) => "FUNCTION",
    };
    let name = qualified(dialect, target);
    let statement = match dialect.drop_spelling().routines {
        RoutineDrop::None => return None,
        RoutineDrop::Alias => format!("DROP ALIAS {name}"),
        RoutineDrop::Overloaded => match identity.arguments {
            // An aggregate over no arguments is written with a star.
            Some("") if keyword == "AGGREGATE" => format!("DROP AGGREGATE {name}(*)"),
            Some(arguments) => format!("DROP {keyword} {name}({arguments})"),
            None => format!("DROP {keyword} {name}"),
        },
        RoutineDrop::Specific => match present(identity.specific_name) {
            Some(specific) => format!(
                "DROP SPECIFIC {keyword} {}",
                sibling(dialect, target, specific)
            ),
            None => format!("DROP {keyword} {name}"),
        },
        RoutineDrop::Packaged if present(identity.package).is_some() => return None,
        RoutineDrop::Packaged | RoutineDrop::Named => format!("DROP {keyword} {name}"),
    };
    Some(in_database(dialect, target.catalog, statement))
}

/// A trigger's `DROP`: by its table on PostgreSQL, by its own name elsewhere.
fn trigger(dialect: &Dialect, target: &DropTarget<'_>) -> Option<String> {
    let statement = match dialect.drop_spelling().triggers {
        TriggerDrop::OnTable => {
            let table = present(target.identity.table)?;
            format!(
                "DROP TRIGGER {} ON {}",
                dialect.quote_ident(target.name),
                sibling(dialect, target, table)
            )
        }
        TriggerDrop::Named => format!("DROP TRIGGER {}", qualified(dialect, target)),
    };
    Some(in_database(dialect, target.catalog, statement))
}

/// `statement`, run inside `catalog` where a drop has to say which database
/// its object sits in — SQL Server — and unchanged elsewhere.
///
/// The statement is passed to that database's own `sp_executesql` as an
/// `N'…'` literal, its quotes doubled, so it runs there without moving the
/// session — which every pane on this connection shares — off its own
/// database.
fn in_database(dialect: &Dialect, catalog: Option<&str>, statement: String) -> String {
    match catalog.filter(|catalog| !catalog.is_empty()) {
        Some(catalog) if dialect.drop_spelling().in_catalog => format!(
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
        DropTarget::new(kind, catalog, schema, name)
    }

    fn with<'a>(target: DropTarget<'a>, identity: Identity<'a>) -> DropTarget<'a> {
        DropTarget { identity, ..target }
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
    fn a_postgresql_routine_is_dropped_by_its_arguments_under_its_own_kind() {
        let pg = Dialect::POSTGRES;
        let function = target(DropKind::Function, Some("db"), Some("app"), "total");
        let identity = |arguments, kind| Identity {
            arguments: Some(arguments),
            routine_kind: Some(kind),
            ..Identity::default()
        };
        assert_eq!(
            drop_statement(&pg, &with(function, identity("a integer, b integer", "f"))).unwrap(),
            "DROP FUNCTION app.total(a integer, b integer)"
        );
        // The catalog's word wins over the folder's.
        assert_eq!(
            drop_statement(&pg, &with(function, identity("", "p"))).unwrap(),
            "DROP PROCEDURE app.total()"
        );
        assert_eq!(
            drop_statement(&pg, &with(function, identity("numeric", "a"))).unwrap(),
            "DROP AGGREGATE app.total(numeric)"
        );
        assert_eq!(
            drop_statement(&pg, &with(function, identity("", "a"))).unwrap(),
            "DROP AGGREGATE app.total(*)"
        );
        assert_eq!(
            drop_statement(&pg, &with(function, identity("VARIADIC xs integer[]", "w"))).unwrap(),
            "DROP FUNCTION app.total(VARIADIC xs integer[])"
        );
        // A lookup that said nothing leaves the name alone, which the server
        // takes when it is not overloaded and refuses when it is.
        assert_eq!(
            drop_statement(&pg, &function).unwrap(),
            "DROP FUNCTION app.total"
        );
        assert_eq!(
            drop_statement(
                &pg,
                &target(DropKind::Procedure, None, Some("app"), "total")
            )
            .unwrap(),
            "DROP PROCEDURE app.total"
        );
    }

    #[test]
    fn a_routine_elsewhere_is_dropped_the_way_its_product_names_it() {
        let upper = |kind| target(kind, None, Some("APP"), "TOTAL");
        let specific = Identity {
            specific_name: Some("SQL251118093000123"),
            ..Identity::default()
        };
        assert_eq!(
            drop_statement(&Dialect::DB2, &with(upper(DropKind::Function), specific)).unwrap(),
            "DROP SPECIFIC FUNCTION APP.SQL251118093000123"
        );
        assert_eq!(
            drop_statement(&Dialect::DB2, &with(upper(DropKind::Procedure), specific)).unwrap(),
            "DROP SPECIFIC PROCEDURE APP.SQL251118093000123"
        );
        assert_eq!(
            drop_statement(&Dialect::DB2, &upper(DropKind::Function)).unwrap(),
            "DROP FUNCTION APP.TOTAL"
        );
        for kind in [DropKind::Function, DropKind::Procedure] {
            assert_eq!(
                drop_statement(&Dialect::H2, &upper(kind)).unwrap(),
                "DROP ALIAS APP.TOTAL"
            );
        }
        assert_eq!(
            drop_statement(&Dialect::ORACLE, &upper(DropKind::Procedure)).unwrap(),
            "DROP PROCEDURE APP.TOTAL"
        );
        let packaged = Identity {
            package: Some("BILLING"),
            ..Identity::default()
        };
        assert_eq!(
            drop_statement(&Dialect::ORACLE, &with(upper(DropKind::Function), packaged)),
            None
        );
        assert_eq!(
            drop_statement(
                &Dialect::MYSQL,
                &target(DropKind::Function, Some("shop"), None, "total")
            )
            .unwrap(),
            "DROP FUNCTION shop.total"
        );
        assert_eq!(
            drop_statement(
                &Dialect::MARIADB,
                &target(DropKind::Procedure, Some("shop"), None, "total")
            )
            .unwrap(),
            "DROP PROCEDURE shop.total"
        );
        assert_eq!(
            drop_statement(
                &Dialect::MSSQL,
                &target(DropKind::Procedure, Some("Sales"), Some("dbo"), "total")
            )
            .unwrap(),
            "EXEC Sales.sys.sp_executesql N'DROP PROCEDURE dbo.total'"
        );
        assert_eq!(
            drop_statement(&Dialect::GENERIC, &upper(DropKind::Function)).unwrap(),
            "DROP FUNCTION APP.TOTAL"
        );
        assert_eq!(
            drop_statement(
                &Dialect::SQLITE,
                &target(DropKind::Function, None, None, "f")
            ),
            None
        );
    }

    #[test]
    fn a_trigger_is_dropped_through_its_table_where_its_name_is_not_enough() {
        let on = |table| Identity {
            table: Some(table),
            ..Identity::default()
        };
        let pg = target(DropKind::Trigger, Some("db"), Some("app"), "Audit");
        assert_eq!(
            drop_statement(&Dialect::POSTGRES, &with(pg, on("orders"))).unwrap(),
            "DROP TRIGGER \"Audit\" ON app.orders"
        );
        assert_eq!(drop_statement(&Dialect::POSTGRES, &pg), None);
        assert_eq!(
            drop_statement(
                &Dialect::MYSQL,
                &with(
                    target(DropKind::Trigger, Some("shop"), None, "audit"),
                    on("orders")
                )
            )
            .unwrap(),
            "DROP TRIGGER shop.audit"
        );
        assert_eq!(
            drop_statement(
                &Dialect::MSSQL,
                &target(DropKind::Trigger, Some("Sales"), Some("dbo"), "audit")
            )
            .unwrap(),
            "EXEC Sales.sys.sp_executesql N'DROP TRIGGER dbo.audit'"
        );
        for dialect in [Dialect::ORACLE, Dialect::DB2, Dialect::H2, Dialect::GENERIC] {
            assert_eq!(
                drop_statement(
                    &dialect,
                    &target(DropKind::Trigger, None, Some("APP"), "AUDIT")
                )
                .unwrap(),
                "DROP TRIGGER APP.AUDIT",
                "{}",
                dialect.name()
            );
        }
        assert_eq!(
            drop_statement(
                &Dialect::SQLITE,
                &target(DropKind::Trigger, None, None, "audit")
            )
            .unwrap(),
            "DROP TRIGGER audit"
        );
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
