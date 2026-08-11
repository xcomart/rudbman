//! Writing the statements that change a table's shape.
//!
//! The structure pane edits a table the way the grid edits its rows: the user
//! retypes a column's type, ticks `NOT NULL` off, adds a column, drops a
//! constraint, renames the table — and nothing is sent until the whole pile is
//! applied. This module is what turns that pile into SQL. It is a sibling of
//! [`mod@dml`](crate::dml) and answers to the same charter as
//! [`mod@ident`](crate::ident): this crate is the one place that *emits* SQL,
//! and every name it emits goes through [`Dialect::quote_ident`].
//!
//! ```
//! use rudbman_sql::{ColumnDef, Dialect, TableAlter, plan_alter};
//!
//! let mut alter = TableAlter::new(["app", "orders"]);
//! alter.adds.push(ColumnDef::new("note", "varchar(80)"));
//! alter.drops.push("legacy".to_string());
//!
//! assert_eq!(
//!     plan_alter(&alter, &Dialect::POSTGRES).unwrap(),
//!     [
//!         "ALTER TABLE app.orders ADD COLUMN note varchar(80)",
//!         "ALTER TABLE app.orders DROP COLUMN legacy",
//!     ]
//! );
//! ```
//!
//! # A statement is a string, and there are no parameters
//!
//! [`plan_alter`] returns `Vec<String>` where [`plan_edits`](crate::plan_edits)
//! returns SQL plus values, and the difference is not an oversight. No server
//! accepts a `?` in a DDL statement — not for a type, not for a default, not for
//! a name. §7.9's rule that a value is never spliced into statement text does
//! not lapse here so much as it does not apply: there are no values here, only
//! names (through [`Dialect::quote_ident`], as everywhere) and fragments the
//! user wrote.
//!
//! # Types and defaults are the user's own SQL, passed through unread
//!
//! This crate has no type model and is not getting one. `VARCHAR2(30)`,
//! `character varying(30)`, `NVARCHAR(30)` and `TEXT` are four products'
//! answers to one question, and a mapping table between them would be a guess
//! made in the one place that cannot see the server. So
//! [`ColumnDef::type_sql`] is a string the user typed into a field pre-filled
//! from the catalog, [`ColumnDef::default_sql`] is a string in the same shape,
//! and this module's whole contribution is deciding which clause they land in.
//! Neither is parsed, validated or quoted. What makes that safe is what makes
//! the grid's short `WHERE` clause safe — the batch is shown in full before any
//! of it runs, and a type nobody can parse is visible there.
//!
//! # Why a change carries both sides
//!
//! A [`ColumnChange`] is a diff, not a target: it holds the definition that was
//! read *and* the definition that is wanted. Two dialects need the old side.
//! MySQL's `MODIFY COLUMN` and `CHANGE COLUMN` restate the *entire* definition,
//! so a change of type that did not also restate `NOT NULL` would quietly drop
//! it — which is why the restating forms are built from `to` in full rather
//! than from what differs. SQL Server's `ALTER COLUMN` restates the type even
//! when only nullability changed, and — the trap — resets the column to
//! nullable when the clause is omitted, so that clause is written every time.
//! The old side is also what lets a rename be spelled `CHANGE a b <definition>`
//! on MySQL, which is the form that works before 8.0 as well as after, since
//! the client cannot see the server's version.
//!
//! # Statement order
//!
//! Constraint drops, then column adds, then column changes, then column drops,
//! then the table rename. Constraints go first because one naming a column
//! blocks that column's drop. The renames go last — both a column's and the
//! table's — so that every statement before them names its target the way the
//! catalog still holds it, which is the same rule twice: inside a single
//! [`ColumnChange`] that renames *and* retypes, the attribute statements come
//! first and the rename last, and they all name the column by
//! [`ColumnChange::from`].
//!
//! Within one column's attribute statements the order is type, then default,
//! then nullability. A default that is already in place when the column is made
//! `NOT NULL` is the order with a chance of succeeding; the reverse asks the
//! server to reject every existing NULL first.
//!
//! # Where the products differ is a table
//!
//! `AlterStyle`, private to this module, is a flat record of per-dialect
//! spellings, one static per
//! dialect, in the shape and for the reason [`Syntax`](crate::Syntax) is one:
//! adding a dialect is adding a row rather than editing a dozen `match` arms.
//! Four families of attribute change fall out of it — the standard one
//! (PostgreSQL, H2, generic) with an independent clause per attribute; MySQL,
//! which restates the definition; Oracle's `MODIFY (...)`, which restates *only
//! what changed*, because naming `NOT NULL` on a column that already has it is
//! ORA-01442; and SQL Server, whose `ALTER COLUMN` carries type and nullability
//! together. Even the spellings that look universal are not: `ADD COLUMN` is a
//! syntax error on Oracle and SQL Server, which want a bare `ADD`, and the
//! order of `DEFAULT` and `NOT NULL` inside one definition is a field rather
//! than a constant, since Oracle takes the default first.
//!
//! # A refusal names the product and the reason
//!
//! SQLite can add, drop and rename a column and rename a table, and has no
//! `ALTER` for anything else: a type change there is a table rebuild — new
//! table, copy, drop, rename — which is a data-moving operation wearing a
//! schema operation's clothes, and not what a user who typed a new type asked
//! for. SQL Server keeps a default as a separately named constraint rather than
//! a column attribute, so changing one is a drop and an add of a constraint
//! whose name JDBC's `getColumns` does not report. Both are
//! [`DdlError::Unsupported`], in a line that says which product and why, before
//! anything is planned. A generated statement that failed on the server would
//! say less: the driver's message names a syntax error, not the fact that the
//! product cannot do this at all.
//!
//! # What is deliberately not here
//!
//! * **A transaction.** MySQL and Oracle commit implicitly at every DDL
//!   statement, so a batch cannot be rolled back there, and this module says
//!   nothing about how the caller runs one. The batch is a list, it stops at
//!   the first failure, and how many statements were committed before it
//!   stopped is a fact the caller reports rather than a promise made here.
//! * **Adding a constraint.** [`TableAlter`] drops constraints and does not add
//!   them: a new foreign key needs a target table, a match rule and an action
//!   per event, which is a form of its own rather than a field of this one.
//! * **Column position.** No `FIRST`/`AFTER`: two of the six products have no
//!   syntax for it, and a server that puts a new column at the end regardless
//!   is why the pane re-reads the catalog after an apply instead of patching
//!   what it holds.

use std::fmt;

use crate::dialect::{Dialect, DialectId};

/// One column, as the catalog holds it or as the user wants it.
///
/// [`ColumnDef::type_sql`] and [`ColumnDef::default_sql`] are the user's own
/// SQL and never parsed; see the [module documentation](self).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ColumnDef {
    /// The column's name, unquoted, as the catalog spells it.
    pub name: String,
    /// The type, as SQL, exactly as the user wrote it. Never parsed.
    pub type_sql: String,
    /// Whether the column refuses NULL.
    pub not_null: bool,
    /// The default expression, as SQL. `None` is "no default".
    ///
    /// `Some` and `None` are the two states the server distinguishes, so
    /// `Some(x)` against `None` is a default being set or dropped rather than
    /// being changed to nothing.
    pub default_sql: Option<String>,
}

impl ColumnDef {
    /// A nullable column named `name` of type `type_sql`, with no default.
    ///
    /// ```
    /// use rudbman_sql::ColumnDef;
    ///
    /// let c = ColumnDef::new("qty", "integer").with_not_null(true).with_default("0");
    /// assert_eq!(c.default_sql.as_deref(), Some("0"));
    /// ```
    pub fn new(name: impl Into<String>, type_sql: impl Into<String>) -> Self {
        ColumnDef {
            name: name.into(),
            type_sql: type_sql.into(),
            not_null: false,
            default_sql: None,
        }
    }

    /// Sets whether the column refuses NULL.
    pub fn with_not_null(mut self, not_null: bool) -> Self {
        self.not_null = not_null;
        self
    }

    /// Sets the default expression, as SQL.
    pub fn with_default(mut self, default_sql: impl Into<String>) -> Self {
        self.default_sql = Some(default_sql.into());
        self
    }
}

/// One column's before and after.
///
/// Both sides are needed, and not only to tell what changed: the dialects that
/// restate a whole definition build it from [`ColumnChange::to`], and the ones
/// that do not still name the column by [`ColumnChange::from`], because that is
/// the spelling the catalog still holds when the statement arrives.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ColumnChange {
    /// The column as it was read from the catalog.
    pub from: ColumnDef,
    /// The column as the user wants it.
    pub to: ColumnDef,
}

/// Which kind of constraint a [`ConstraintDrop`] names.
///
/// The kind travels with the name because MySQL has no generic
/// `DROP CONSTRAINT` and spells each kind separately. It costs the caller
/// nothing: the detail panel's keys and references tabs already know which is
/// which.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ConstraintKind {
    /// The table's primary key.
    PrimaryKey,
    /// A foreign key.
    ForeignKey,
    /// A unique constraint — on MySQL, the unique index behind it.
    Unique,
    /// A check constraint.
    Check,
}

/// One constraint to drop.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConstraintDrop {
    /// What kind of constraint it is.
    pub kind: ConstraintKind,
    /// Its name, as the catalog spells it.
    ///
    /// Unused for a primary key on MySQL, whose `DROP PRIMARY KEY` names no
    /// constraint — a table has at most one.
    pub name: String,
}

/// Everything staged against one table's structure, and the input to
/// [`plan_alter`].
///
/// The lists are applied in the order they are declared here — see the [module
/// documentation](self) — and each is independent of the others: a column can
/// be added and another dropped and the table renamed in one apply.
///
/// ```
/// use rudbman_sql::{ColumnChange, ColumnDef, Dialect, TableAlter, plan_alter};
///
/// let mut alter = TableAlter::new(["orders"]);
/// assert!(alter.is_empty());
///
/// alter.changes.push(ColumnChange {
///     from: ColumnDef::new("qty", "integer"),
///     to: ColumnDef::new("qty", "bigint").with_not_null(true),
/// });
/// alter.rename_to = Some("order_lines".to_string());
///
/// assert_eq!(
///     plan_alter(&alter, &Dialect::POSTGRES).unwrap(),
///     [
///         "ALTER TABLE orders ALTER COLUMN qty SET DATA TYPE bigint",
///         "ALTER TABLE orders ALTER COLUMN qty SET NOT NULL",
///         "ALTER TABLE orders RENAME TO order_lines",
///     ]
/// );
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct TableAlter {
    /// The table's name parts, most significant first: catalog, schema, name.
    ///
    /// Parts a driver left null are the caller's to drop before they get here;
    /// an empty part is [`DdlError::NoTable`] rather than an empty quoted name
    /// in the middle of a qualified one.
    pub table: Vec<String>,
    /// The constraints to drop, first, before anything they name is touched.
    pub drop_constraints: Vec<ConstraintDrop>,
    /// The columns to add.
    pub adds: Vec<ColumnDef>,
    /// The columns to change, each carrying both sides.
    pub changes: Vec<ColumnChange>,
    /// Column names to drop.
    pub drops: Vec<String>,
    /// The table's new **bare** name.
    ///
    /// Bare because that is what every product takes: a rename moves a table
    /// within its schema, and none of them accepts a qualified target.
    pub rename_to: Option<String>,
}

impl TableAlter {
    /// An empty alter against `table`.
    ///
    /// ```
    /// use rudbman_sql::TableAlter;
    ///
    /// let alter = TableAlter::new(["app", "orders"]);
    /// assert_eq!(alter.table, ["app", "orders"]);
    /// assert!(alter.is_empty());
    /// ```
    pub fn new(table: impl IntoIterator<Item = impl Into<String>>) -> Self {
        TableAlter {
            table: table.into_iter().map(Into::into).collect(),
            ..Self::default()
        }
    }

    /// Whether there is nothing to apply.
    pub fn is_empty(&self) -> bool {
        self.drop_constraints.is_empty()
            && self.adds.is_empty()
            && self.changes.is_empty()
            && self.drops.is_empty()
            && self.rename_to.is_none()
    }
}

/// What a dialect cannot express, in [`DdlError::Unsupported`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Unsupported {
    /// Changing a column's type.
    ColumnType,
    /// Changing whether a column accepts NULL.
    Nullability,
    /// Setting or dropping a column's default.
    Default,
    /// Dropping a constraint.
    ConstraintDrop,
}

/// Why an alter could not be turned into statements.
///
/// Most variants are a caller mistake — a table with no name, a change list
/// holding a row that did not change. [`DdlError::Unsupported`] is the one that
/// is worth showing to the user as it stands: it names the product and the
/// reason, which is more than the server's answer to a statement written
/// anyway would have said.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DdlError {
    /// The table has no name, or one of its parts is empty.
    NoTable,
    /// A column was named by the empty string — in an add, a drop, or either
    /// side of a change.
    NoColumnName,
    /// A statement that has to restate the column's type has none to restate.
    NoTypeSql {
        /// The column's name, for the message shown to the user.
        column: String,
    },
    /// A [`ColumnChange`] whose two sides are equal. A column with nothing
    /// changed does not belong in [`TableAlter::changes`].
    NoChange {
        /// The column's name.
        column: String,
    },
    /// [`TableAlter::rename_to`] is `Some("")`.
    NoNewName,
    /// This dialect has no way to express the change. See the [module
    /// documentation](self).
    Unsupported {
        /// The dialect that cannot express it.
        dialect: DialectId,
        /// What could not be expressed.
        what: Unsupported,
        /// The column it was asked of, where one is involved.
        column: Option<String>,
    },
}

impl fmt::Display for DdlError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DdlError::NoTable => f.write_str("the table has no usable name"),
            DdlError::NoColumnName => f.write_str("a column in the alter has no name"),
            DdlError::NoTypeSql { column } => write!(
                f,
                "column `{column}` has no type text, and the statement to be written has to \
                 restate it: an added column names its type, and MySQL and SQL Server restate \
                 the whole definition on a change"
            ),
            DdlError::NoChange { column } => write!(
                f,
                "column `{column}` is unchanged, so it does not belong in the change list"
            ),
            DdlError::NoNewName => f.write_str("the table's new name is empty"),
            DdlError::Unsupported {
                dialect,
                what,
                column,
            } => {
                let action = match what {
                    Unsupported::ColumnType => "change the type",
                    Unsupported::Nullability => "change whether NULL is accepted",
                    Unsupported::Default => "change the default",
                    Unsupported::ConstraintDrop => "drop a constraint",
                };
                let reason = match (dialect, what) {
                    (DialectId::MsSql, Unsupported::Default) => {
                        "SQL Server holds a default as a separately named constraint rather than \
                         a column attribute, and the catalog does not report that name, so there \
                         is nothing to drop and re-add"
                    }
                    (DialectId::Sqlite, _) => {
                        "SQLite has no ALTER TABLE form for it, and the table would have to be \
                         rebuilt — a new table, a copy of every row, a drop and a rename"
                    }
                    _ => "this product has no ALTER TABLE form for it",
                };
                write!(f, "{} cannot {action}", product(*dialect))?;
                if let Some(column) = column {
                    write!(f, " of column `{column}`")?;
                }
                write!(f, ": {reason}")
            }
        }
    }
}

impl std::error::Error for DdlError {}

/// The product's name as a person writes it, for a message about that product.
///
/// [`DialectId::as_str`] is deliberately the lower-case `drivers.json` spelling
/// and round-trips through [`Dialect::from_id`]; a sentence that named the
/// product twice, once as `mssql` and once as SQL Server, would be reading a
/// configuration key aloud.
const fn product(dialect: DialectId) -> &'static str {
    match dialect {
        DialectId::Generic => "generic SQL",
        DialectId::H2 => "H2",
        DialectId::Postgres => "PostgreSQL",
        DialectId::MySql => "MySQL",
        DialectId::Sqlite => "SQLite",
        DialectId::Oracle => "Oracle",
        DialectId::MsSql => "SQL Server",
    }
}

/// How a column rename is spelled.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ColumnRename {
    /// `ALTER TABLE t RENAME COLUMN a TO b`: PostgreSQL, Oracle, SQLite,
    /// generic.
    RenameColumn,
    /// `ALTER TABLE t ALTER COLUMN a RENAME TO b`: H2.
    AlterRenameTo,
    /// `ALTER TABLE t CHANGE COLUMN a <definition of b>`: MySQL, which had no
    /// `RENAME COLUMN` before 8.0 and whose version this client cannot see.
    /// The form restates the definition, so it carries the attribute changes
    /// too and a rename-and-retype is one statement.
    Change,
    /// `EXEC sp_rename '<table>.<column>', '<new name>', 'COLUMN'`: SQL
    /// Server, which has no `ALTER` form at all.
    SpRename,
}

/// How a column's type, nullability and default are changed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AttrStyle {
    /// One independent clause per attribute, one statement each: PostgreSQL,
    /// H2, generic.
    Clauses,
    /// One statement restating the whole definition: MySQL's `MODIFY COLUMN`.
    Restate,
    /// One statement restating only what changed: Oracle's `MODIFY (...)`.
    ModifyChanged,
    /// One statement carrying type and nullability together, and no way to
    /// change a default: SQL Server's `ALTER COLUMN`.
    TypeWithNull,
    /// No statement at all: SQLite, where the table would have to be rebuilt.
    Rebuild,
}

/// How a constraint is dropped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DropStyle {
    /// `DROP CONSTRAINT n`, whatever the kind.
    Named,
    /// One spelling per kind: MySQL, which has no generic form.
    PerKind,
    /// Not at all: SQLite.
    Rebuild,
}

/// How the table itself is renamed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TableRename {
    /// `ALTER TABLE t RENAME TO n`.
    RenameTo,
    /// `EXEC sp_rename '<table>', '<new name>'`: SQL Server.
    SpRename,
}

/// The per-dialect spellings of one `ALTER TABLE`.
///
/// A flat record, one static per dialect, in the shape and for the reason
/// [`Syntax`](crate::Syntax) is one: every place the products disagree is
/// listed once, so adding a dialect is adding a row rather than editing a
/// branch in each emitter.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct AlterStyle {
    /// The keyword that adds a column: `ADD COLUMN`, or the bare `ADD` that
    /// Oracle and SQL Server require.
    add: &'static str,
    /// `DEFAULT` precedes `NOT NULL` inside a column definition.
    ///
    /// Oracle, whose grammar puts the default in the datatype clause, ahead of
    /// the inline constraints. Everyone else documents the reverse.
    default_first: bool,
    /// How a column rename is spelled.
    column_rename: ColumnRename,
    /// How a column's attributes are changed.
    attrs: AttrStyle,
    /// How a constraint is dropped.
    constraints: DropStyle,
    /// How the table is renamed.
    table_rename: TableRename,
}

impl AlterStyle {
    /// Standard SQL, and what the rows below start from.
    const GENERIC: Self = Self {
        add: "ADD COLUMN",
        default_first: false,
        column_rename: ColumnRename::RenameColumn,
        attrs: AttrStyle::Clauses,
        constraints: DropStyle::Named,
        table_rename: TableRename::RenameTo,
    };

    /// PostgreSQL: the standard form throughout.
    const POSTGRES: Self = Self::GENERIC;

    /// H2: standard except that a rename is a clause of `ALTER COLUMN`.
    const H2: Self = Self {
        column_rename: ColumnRename::AlterRenameTo,
        ..Self::GENERIC
    };

    /// MySQL: `CHANGE`/`MODIFY COLUMN` restate the definition, and each kind of
    /// constraint has its own `DROP`.
    const MYSQL: Self = Self {
        column_rename: ColumnRename::Change,
        attrs: AttrStyle::Restate,
        constraints: DropStyle::PerKind,
        ..Self::GENERIC
    };

    /// SQLite: adds, drops and renames only.
    const SQLITE: Self = Self {
        attrs: AttrStyle::Rebuild,
        constraints: DropStyle::Rebuild,
        ..Self::GENERIC
    };

    /// Oracle: a bare `ADD`, the default ahead of `NOT NULL`, and a `MODIFY`
    /// that names only what changed.
    const ORACLE: Self = Self {
        add: "ADD",
        default_first: true,
        attrs: AttrStyle::ModifyChanged,
        ..Self::GENERIC
    };

    /// SQL Server: a bare `ADD`, `sp_rename` for both renames, and an
    /// `ALTER COLUMN` that carries type and nullability together.
    const MSSQL: Self = Self {
        add: "ADD",
        column_rename: ColumnRename::SpRename,
        attrs: AttrStyle::TypeWithNull,
        table_rename: TableRename::SpRename,
        ..Self::GENERIC
    };

    /// The row for `dialect`.
    const fn of(dialect: DialectId) -> &'static Self {
        match dialect {
            DialectId::Generic => &Self::GENERIC,
            DialectId::H2 => &Self::H2,
            DialectId::Postgres => &Self::POSTGRES,
            DialectId::MySql => &Self::MYSQL,
            DialectId::Sqlite => &Self::SQLITE,
            DialectId::Oracle => &Self::ORACLE,
            DialectId::MsSql => &Self::MSSQL,
        }
    }
}

/// Turns one table's staged structure edits into the statements that apply
/// them.
///
/// The batch is ordered constraint drops, column adds, column changes, column
/// drops, table rename; see the [module documentation](self) for why, and for
/// why a statement is a string with nothing bound to it.
///
/// A dialect that cannot express one of the changes is
/// [`DdlError::Unsupported`] rather than a statement it would reject — SQLite
/// has no `ALTER` for a column's type, nullability or default, and SQL Server
/// none for a default:
///
/// ```
/// use rudbman_sql::{ColumnDef, Dialect, TableAlter, plan_alter};
///
/// let mut alter = TableAlter::new(["orders"]);
/// alter.adds.push(ColumnDef::new("qty", "NUMBER").with_not_null(true).with_default("0"));
///
/// // Oracle wants a bare `ADD`, and the default ahead of `NOT NULL`.
/// assert_eq!(
///     plan_alter(&alter, &Dialect::ORACLE).unwrap(),
///     [r#"ALTER TABLE "orders" ADD "qty" NUMBER DEFAULT 0 NOT NULL"#]
/// );
/// // PostgreSQL takes the standard spelling.
/// assert_eq!(
///     plan_alter(&alter, &Dialect::POSTGRES).unwrap(),
///     ["ALTER TABLE orders ADD COLUMN qty NUMBER NOT NULL DEFAULT 0"]
/// );
/// ```
pub fn plan_alter(alter: &TableAlter, dialect: &Dialect) -> Result<Vec<String>, DdlError> {
    if alter.is_empty() {
        return Ok(Vec::new());
    }
    if alter.table.is_empty() || alter.table.iter().any(|part| part.is_empty()) {
        return Err(DdlError::NoTable);
    }
    let table = dialect.qualify(alter.table.iter().map(String::as_str));
    let style = AlterStyle::of(dialect.id());

    let mut batch = Vec::new();
    for constraint in &alter.drop_constraints {
        batch.push(drop_constraint(&table, constraint, style, dialect)?);
    }
    for column in &alter.adds {
        batch.push(format!(
            "ALTER TABLE {table} {} {}",
            style.add,
            column_def(column, style, dialect)?
        ));
    }
    for change in &alter.changes {
        change_column(&mut batch, &table, change, style, dialect)?;
    }
    for name in &alter.drops {
        let name = named(name)?;
        batch.push(format!(
            "ALTER TABLE {table} DROP COLUMN {}",
            dialect.quote_ident(name)
        ));
    }
    if let Some(new_name) = &alter.rename_to {
        if new_name.is_empty() {
            return Err(DdlError::NoNewName);
        }
        batch.push(rename_table(&table, new_name, style, dialect));
    }
    Ok(batch)
}

/// `<name> <type>[ NOT NULL][ DEFAULT <x>]`, with the two trailing clauses in
/// the order this dialect's grammar takes them.
fn column_def(
    column: &ColumnDef,
    style: &AlterStyle,
    dialect: &Dialect,
) -> Result<String, DdlError> {
    let name = named(&column.name)?;
    if column.type_sql.is_empty() {
        return Err(DdlError::NoTypeSql {
            column: name.to_string(),
        });
    }
    let not_null = if column.not_null { " NOT NULL" } else { "" };
    let default = match &column.default_sql {
        Some(default) => format!(" DEFAULT {default}"),
        None => String::new(),
    };
    let mut out = format!("{} {}", dialect.quote_ident(name), column.type_sql);
    if style.default_first {
        out.push_str(&default);
        out.push_str(not_null);
    } else {
        out.push_str(not_null);
        out.push_str(&default);
    }
    Ok(out)
}

/// The statements for one changed column: its attributes first, its rename
/// last, so every one of them names the column the way the catalog still holds
/// it.
fn change_column(
    batch: &mut Vec<String>,
    table: &str,
    change: &ColumnChange,
    style: &AlterStyle,
    dialect: &Dialect,
) -> Result<(), DdlError> {
    named(&change.from.name)?;
    named(&change.to.name)?;
    if change.from == change.to {
        return Err(DdlError::NoChange {
            column: change.from.name.clone(),
        });
    }

    let renamed = change.from.name != change.to.name;
    // MySQL's `CHANGE COLUMN` restates the definition, so the rename statement
    // *is* the attribute statement and a second one would only undo it.
    let absorbed = renamed && style.column_rename == ColumnRename::Change;
    if !absorbed {
        change_attributes(batch, table, change, style, dialect)?;
    }
    if renamed {
        batch.push(rename_column(table, change, style, dialect)?);
    }
    Ok(())
}

/// The statements for a column's type, default and nullability, in that order,
/// and only for what actually differs.
fn change_attributes(
    batch: &mut Vec<String>,
    table: &str,
    change: &ColumnChange,
    style: &AlterStyle,
    dialect: &Dialect,
) -> Result<(), DdlError> {
    let (from, to) = (&change.from, &change.to);
    let retyped = from.type_sql != to.type_sql;
    let redefaulted = from.default_sql != to.default_sql;
    let renullabled = from.not_null != to.not_null;
    if !(retyped || redefaulted || renullabled) {
        return Ok(());
    }
    // The catalog's spelling, which is what every statement here names: a
    // rename, if there is one, comes after all of them.
    let column = dialect.quote_ident(&from.name);
    let unsupported = |what| DdlError::Unsupported {
        dialect: dialect.id(),
        what,
        column: Some(from.name.clone()),
    };
    let type_sql = || {
        if to.type_sql.is_empty() {
            return Err(DdlError::NoTypeSql {
                column: from.name.clone(),
            });
        }
        Ok(&to.type_sql)
    };
    let nullability = if to.not_null { "NOT NULL" } else { "NULL" };

    match style.attrs {
        AttrStyle::Clauses => {
            if retyped {
                batch.push(format!(
                    "ALTER TABLE {table} ALTER COLUMN {column} SET DATA TYPE {}",
                    type_sql()?
                ));
            }
            if redefaulted {
                batch.push(default_clause(table, &column, to));
            }
            if renullabled {
                let verb = if to.not_null { "SET" } else { "DROP" };
                batch.push(format!(
                    "ALTER TABLE {table} ALTER COLUMN {column} {verb} NOT NULL"
                ));
            }
        }
        // MySQL restates everything, which is the point: a `MODIFY` that named
        // only the new type would drop the column's `NOT NULL` and its default
        // along with the old type. The exception is a change to the default
        // alone, which `ALTER COLUMN` does as metadata where `MODIFY` rewrites
        // the table.
        AttrStyle::Restate => {
            if redefaulted && !retyped && !renullabled {
                batch.push(default_clause(table, &column, to));
            } else {
                batch.push(format!(
                    "ALTER TABLE {table} MODIFY COLUMN {}",
                    column_def(to, style, dialect)?
                ));
            }
        }
        // Oracle names only what changed, and not as an optimization: naming
        // `NOT NULL` on a column that already has it is ORA-01442. A dropped
        // default is spelled `DEFAULT NULL`, which is Oracle's way of removing
        // one rather than a default of NULL.
        AttrStyle::ModifyChanged => {
            let mut parts = Vec::new();
            if retyped {
                parts.push(type_sql()?.clone());
            }
            if redefaulted {
                parts.push(match &to.default_sql {
                    Some(default) => format!("DEFAULT {default}"),
                    None => "DEFAULT NULL".to_string(),
                });
            }
            if renullabled {
                parts.push(nullability.to_string());
            }
            batch.push(format!(
                "ALTER TABLE {table} MODIFY ({column} {})",
                parts.join(" ")
            ));
        }
        // SQL Server's nullability clause is written every time, changed or
        // not: omitting it resets the column to nullable. The type comes from
        // `to` for the same reason — the statement restates both or neither.
        AttrStyle::TypeWithNull => {
            if redefaulted {
                return Err(unsupported(Unsupported::Default));
            }
            batch.push(format!(
                "ALTER TABLE {table} ALTER COLUMN {column} {} {nullability}",
                type_sql()?
            ));
        }
        AttrStyle::Rebuild => {
            return Err(unsupported(if retyped {
                Unsupported::ColumnType
            } else if renullabled {
                Unsupported::Nullability
            } else {
                Unsupported::Default
            }));
        }
    }
    Ok(())
}

/// `ALTER TABLE t ALTER COLUMN c SET DEFAULT <x>`, or `DROP DEFAULT` when the
/// default is gone.
fn default_clause(table: &str, column: &str, to: &ColumnDef) -> String {
    match &to.default_sql {
        Some(default) => format!("ALTER TABLE {table} ALTER COLUMN {column} SET DEFAULT {default}"),
        None => format!("ALTER TABLE {table} ALTER COLUMN {column} DROP DEFAULT"),
    }
}

/// One column rename, in the four spellings the products have for it.
fn rename_column(
    table: &str,
    change: &ColumnChange,
    style: &AlterStyle,
    dialect: &Dialect,
) -> Result<String, DdlError> {
    let old = dialect.quote_ident(&change.from.name);
    let new = dialect.quote_ident(&change.to.name);
    Ok(match style.column_rename {
        ColumnRename::RenameColumn => format!("ALTER TABLE {table} RENAME COLUMN {old} TO {new}"),
        ColumnRename::AlterRenameTo => {
            format!("ALTER TABLE {table} ALTER COLUMN {old} RENAME TO {new}")
        }
        ColumnRename::Change => format!(
            "ALTER TABLE {table} CHANGE COLUMN {old} {}",
            column_def(&change.to, style, dialect)?
        ),
        // The one place this module writes a string literal. The old column is
        // named through the table, the new one is bare — a name, not an
        // identifier, so it is not quoted as one.
        ColumnRename::SpRename => format!(
            "EXEC sp_rename '{}', '{}', 'COLUMN'",
            quote_literal(&format!("{table}.{old}")),
            quote_literal(&change.to.name)
        ),
    })
}

/// The table rename, which is the same statement everywhere but SQL Server.
fn rename_table(table: &str, new_name: &str, style: &AlterStyle, dialect: &Dialect) -> String {
    match style.table_rename {
        TableRename::RenameTo => format!(
            "ALTER TABLE {table} RENAME TO {}",
            dialect.quote_ident(new_name)
        ),
        TableRename::SpRename => format!(
            "EXEC sp_rename '{}', '{}'",
            quote_literal(table),
            quote_literal(new_name)
        ),
    }
}

/// One constraint drop: `DROP CONSTRAINT n` everywhere but MySQL, which spells
/// each kind separately, and SQLite, which cannot.
fn drop_constraint(
    table: &str,
    constraint: &ConstraintDrop,
    style: &AlterStyle,
    dialect: &Dialect,
) -> Result<String, DdlError> {
    let name = dialect.quote_ident(&constraint.name);
    Ok(match style.constraints {
        DropStyle::Named => format!("ALTER TABLE {table} DROP CONSTRAINT {name}"),
        DropStyle::PerKind => match constraint.kind {
            // A table has one primary key, and MySQL's form names no
            // constraint because there is nothing to disambiguate.
            ConstraintKind::PrimaryKey => format!("ALTER TABLE {table} DROP PRIMARY KEY"),
            ConstraintKind::ForeignKey => format!("ALTER TABLE {table} DROP FOREIGN KEY {name}"),
            // MySQL's unique constraint *is* its index, and that is the word
            // its `DROP` takes.
            ConstraintKind::Unique => format!("ALTER TABLE {table} DROP INDEX {name}"),
            ConstraintKind::Check => format!("ALTER TABLE {table} DROP CHECK {name}"),
        },
        DropStyle::Rebuild => {
            return Err(DdlError::Unsupported {
                dialect: dialect.id(),
                what: Unsupported::ConstraintDrop,
                column: None,
            });
        }
    })
}

/// `name`, or [`DdlError::NoColumnName`] if it is empty.
fn named(name: &str) -> Result<&str, DdlError> {
    if name.is_empty() {
        return Err(DdlError::NoColumnName);
    }
    Ok(name)
}

/// `text` as the body of a SQL string literal: every `'` doubled.
///
/// Only `sp_rename`'s arguments need this. Everything else this module writes
/// is an identifier or a fragment the user typed.
fn quote_literal(text: &str) -> String {
    text.replace('\'', "''")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every dialect, so a test can say what each of them makes of one input.
    const ALL: [Dialect; 7] = [
        Dialect::GENERIC,
        Dialect::H2,
        Dialect::POSTGRES,
        Dialect::MYSQL,
        Dialect::SQLITE,
        Dialect::ORACLE,
        Dialect::MSSQL,
    ];

    /// An alter against `orders`, whose name needs no quoting anywhere but
    /// Oracle-style dialects — so tests that are not about quoting use a table
    /// of one part and check a single dialect.
    fn orders() -> TableAlter {
        TableAlter::new(["orders"])
    }

    /// A change of `qty` from `from` to `to`.
    fn change(from: ColumnDef, to: ColumnDef) -> ColumnChange {
        ColumnChange { from, to }
    }

    /// The batch, or a panic naming the dialect that failed.
    fn sql(alter: &TableAlter, dialect: &Dialect) -> Vec<String> {
        plan_alter(alter, dialect).unwrap_or_else(|e| panic!("{}: {e}", dialect.name()))
    }

    /// The five groups run in the order the catalog can survive: constraints,
    /// adds, changes, drops, and the table rename last.
    #[test]
    fn batch_is_ordered_by_group() {
        let mut alter = orders();
        alter.rename_to = Some("order_lines".to_string());
        alter.drops.push("legacy".to_string());
        alter.changes.push(change(
            ColumnDef::new("qty", "integer"),
            ColumnDef::new("qty", "bigint"),
        ));
        alter.adds.push(ColumnDef::new("note", "varchar(80)"));
        alter.drop_constraints.push(ConstraintDrop {
            kind: ConstraintKind::Check,
            name: "qty_positive".to_string(),
        });

        assert_eq!(
            sql(&alter, &Dialect::POSTGRES),
            [
                "ALTER TABLE orders DROP CONSTRAINT qty_positive",
                "ALTER TABLE orders ADD COLUMN note varchar(80)",
                "ALTER TABLE orders ALTER COLUMN qty SET DATA TYPE bigint",
                "ALTER TABLE orders DROP COLUMN legacy",
                "ALTER TABLE orders RENAME TO order_lines",
            ]
        );
    }

    /// Within one column, the attribute statements come first and the rename
    /// last, so each of them names the column the way the catalog holds it.
    #[test]
    fn attributes_precede_a_columns_rename() {
        let mut alter = orders();
        alter.changes.push(change(
            ColumnDef::new("qty", "integer"),
            ColumnDef::new("quantity", "bigint").with_not_null(true),
        ));

        assert_eq!(
            sql(&alter, &Dialect::POSTGRES),
            [
                "ALTER TABLE orders ALTER COLUMN qty SET DATA TYPE bigint",
                "ALTER TABLE orders ALTER COLUMN qty SET NOT NULL",
                "ALTER TABLE orders RENAME COLUMN qty TO quantity",
            ]
        );
    }

    /// Type, then default, then nullability: the default is in place before the
    /// column is asked to refuse NULL.
    #[test]
    fn one_columns_clauses_are_type_default_nullability() {
        let mut alter = orders();
        alter.changes.push(change(
            ColumnDef::new("qty", "integer"),
            ColumnDef::new("qty", "bigint")
                .with_not_null(true)
                .with_default("0"),
        ));

        assert_eq!(
            sql(&alter, &Dialect::POSTGRES),
            [
                "ALTER TABLE orders ALTER COLUMN qty SET DATA TYPE bigint",
                "ALTER TABLE orders ALTER COLUMN qty SET DEFAULT 0",
                "ALTER TABLE orders ALTER COLUMN qty SET NOT NULL",
            ]
        );
    }

    /// `ADD COLUMN` everywhere but Oracle and SQL Server, which want a bare
    /// `ADD` — and Oracle takes the default ahead of `NOT NULL`.
    #[test]
    fn add_is_spelled_per_dialect() {
        let mut alter = orders();
        alter.adds.push(
            ColumnDef::new("qty", "integer")
                .with_not_null(true)
                .with_default("0"),
        );

        for dialect in [
            Dialect::GENERIC,
            Dialect::H2,
            Dialect::POSTGRES,
            Dialect::MYSQL,
            Dialect::SQLITE,
        ] {
            let table = dialect.qualify(["orders"]);
            let column = dialect.quote_ident("qty");
            assert_eq!(
                sql(&alter, &dialect),
                [format!(
                    "ALTER TABLE {table} ADD COLUMN {column} integer NOT NULL DEFAULT 0"
                )],
                "{}",
                dialect.name()
            );
        }

        assert_eq!(
            sql(&alter, &Dialect::ORACLE),
            [r#"ALTER TABLE "orders" ADD "qty" integer DEFAULT 0 NOT NULL"#]
        );
        assert_eq!(
            sql(&alter, &Dialect::MSSQL),
            ["ALTER TABLE orders ADD qty integer NOT NULL DEFAULT 0"]
        );
    }

    /// A column with neither flag is name and type and nothing else.
    #[test]
    fn add_writes_only_the_clauses_it_has() {
        let mut alter = orders();
        alter.adds.push(ColumnDef::new("note", "text"));
        alter
            .adds
            .push(ColumnDef::new("ts", "timestamp").with_default("now()"));

        assert_eq!(
            sql(&alter, &Dialect::POSTGRES),
            [
                "ALTER TABLE orders ADD COLUMN note text",
                "ALTER TABLE orders ADD COLUMN ts timestamp DEFAULT now()",
            ]
        );
    }

    /// Dropping a column is the one statement every product spells alike.
    #[test]
    fn drop_column_is_universal() {
        let mut alter = orders();
        alter.drops.push("legacy".to_string());

        for dialect in ALL {
            assert_eq!(
                sql(&alter, &dialect),
                [format!(
                    "ALTER TABLE {} DROP COLUMN {}",
                    dialect.qualify(["orders"]),
                    dialect.quote_ident("legacy")
                )],
                "{}",
                dialect.name()
            );
        }
    }

    /// A rename alone, in the six shapes the products have for it.
    #[test]
    fn column_rename_is_spelled_per_dialect() {
        let mut alter = orders();
        alter.changes.push(change(
            ColumnDef::new("qty", "integer"),
            ColumnDef::new("quantity", "integer"),
        ));

        for dialect in [Dialect::GENERIC, Dialect::POSTGRES, Dialect::SQLITE] {
            let table = dialect.qualify(["orders"]);
            assert_eq!(
                sql(&alter, &dialect),
                [format!(
                    "ALTER TABLE {table} RENAME COLUMN {} TO {}",
                    dialect.quote_ident("qty"),
                    dialect.quote_ident("quantity")
                )],
                "{}",
                dialect.name()
            );
        }
        assert_eq!(
            sql(&alter, &Dialect::ORACLE),
            [r#"ALTER TABLE "orders" RENAME COLUMN "qty" TO "quantity""#]
        );
        assert_eq!(
            sql(&alter, &Dialect::H2),
            [r#"ALTER TABLE "orders" ALTER COLUMN "qty" RENAME TO "quantity""#]
        );
        // MySQL restates the definition, because `RENAME COLUMN` arrived in
        // 8.0 and this client cannot see the server's version.
        assert_eq!(
            sql(&alter, &Dialect::MYSQL),
            ["ALTER TABLE orders CHANGE COLUMN qty quantity integer"]
        );
        assert_eq!(
            sql(&alter, &Dialect::MSSQL),
            ["EXEC sp_rename 'orders.qty', 'quantity', 'COLUMN'"]
        );
    }

    /// `sp_rename` takes names as string literals, so a quote in one is
    /// doubled — and the new name is bare, not an identifier.
    #[test]
    fn sp_rename_doubles_quotes_in_a_name() {
        let mut alter = TableAlter::new(["dbo", "orders"]);
        alter.changes.push(change(
            ColumnDef::new("it's", "integer"),
            ColumnDef::new("o'clock", "integer"),
        ));

        assert_eq!(
            sql(&alter, &Dialect::MSSQL),
            [r#"EXEC sp_rename 'dbo.orders."it''s"', 'o''clock', 'COLUMN'"#]
        );
    }

    /// The standard family emits one independent statement per attribute.
    #[test]
    fn each_attribute_alone_on_the_standard_family() {
        let base = ColumnDef::new("qty", "integer")
            .with_not_null(true)
            .with_default("0");

        for dialect in [Dialect::GENERIC, Dialect::POSTGRES, Dialect::H2] {
            let table = dialect.qualify(["orders"]);
            let column = dialect.quote_ident("qty");

            let mut retype = orders();
            retype.changes.push(change(
                base.clone(),
                ColumnDef {
                    type_sql: "bigint".to_string(),
                    ..base.clone()
                },
            ));
            assert_eq!(
                sql(&retype, &dialect),
                [format!(
                    "ALTER TABLE {table} ALTER COLUMN {column} SET DATA TYPE bigint"
                )],
                "{}",
                dialect.name()
            );

            let mut nullable = orders();
            nullable
                .changes
                .push(change(base.clone(), base.clone().with_not_null(false)));
            assert_eq!(
                sql(&nullable, &dialect),
                [format!(
                    "ALTER TABLE {table} ALTER COLUMN {column} DROP NOT NULL"
                )],
                "{}",
                dialect.name()
            );

            let mut default = orders();
            default
                .changes
                .push(change(base.clone(), base.clone().with_default("1")));
            assert_eq!(
                sql(&default, &dialect),
                [format!(
                    "ALTER TABLE {table} ALTER COLUMN {column} SET DEFAULT 1"
                )],
                "{}",
                dialect.name()
            );

            let mut dropped = orders();
            dropped.changes.push(change(
                base.clone(),
                ColumnDef {
                    default_sql: None,
                    ..base.clone()
                },
            ));
            assert_eq!(
                sql(&dropped, &dialect),
                [format!(
                    "ALTER TABLE {table} ALTER COLUMN {column} DROP DEFAULT"
                )],
                "{}",
                dialect.name()
            );
        }
    }

    /// MySQL restates the whole definition on any change but a default, which
    /// is why `from` is carried: a `MODIFY` that named only the new type would
    /// drop `NOT NULL` with it.
    #[test]
    fn mysql_restates_the_definition() {
        let base = ColumnDef::new("qty", "integer")
            .with_not_null(true)
            .with_default("0");

        let mut retype = orders();
        retype.changes.push(change(
            base.clone(),
            ColumnDef {
                type_sql: "bigint".to_string(),
                ..base.clone()
            },
        ));
        assert_eq!(
            sql(&retype, &Dialect::MYSQL),
            ["ALTER TABLE orders MODIFY COLUMN qty bigint NOT NULL DEFAULT 0"]
        );

        let mut nullable = orders();
        nullable
            .changes
            .push(change(base.clone(), base.clone().with_not_null(false)));
        assert_eq!(
            sql(&nullable, &Dialect::MYSQL),
            ["ALTER TABLE orders MODIFY COLUMN qty integer DEFAULT 0"]
        );
    }

    /// A default is the exception: `ALTER COLUMN` changes metadata where
    /// `MODIFY` rewrites the table.
    #[test]
    fn mysql_changes_a_default_without_rewriting() {
        let base = ColumnDef::new("qty", "integer").with_not_null(true);

        let mut set = orders();
        set.changes
            .push(change(base.clone(), base.clone().with_default("0")));
        assert_eq!(
            sql(&set, &Dialect::MYSQL),
            ["ALTER TABLE orders ALTER COLUMN qty SET DEFAULT 0"]
        );

        let mut dropped = orders();
        dropped
            .changes
            .push(change(base.clone().with_default("0"), base.clone()));
        assert_eq!(
            sql(&dropped, &Dialect::MYSQL),
            ["ALTER TABLE orders ALTER COLUMN qty DROP DEFAULT"]
        );

        // With anything else in the same change, it is a `MODIFY` again.
        let mut both = orders();
        both.changes.push(change(
            base.clone(),
            ColumnDef {
                type_sql: "bigint".to_string(),
                default_sql: Some("0".to_string()),
                ..base.clone()
            },
        ));
        assert_eq!(
            sql(&both, &Dialect::MYSQL),
            ["ALTER TABLE orders MODIFY COLUMN qty bigint NOT NULL DEFAULT 0"]
        );
    }

    /// A MySQL rename and retype is one statement, not two: `CHANGE COLUMN`
    /// restates the definition, so a `MODIFY` beside it would be redundant.
    #[test]
    fn mysql_change_absorbs_a_rename_and_a_retype() {
        let mut alter = orders();
        alter.changes.push(change(
            ColumnDef::new("qty", "integer"),
            ColumnDef::new("quantity", "bigint")
                .with_not_null(true)
                .with_default("0"),
        ));

        assert_eq!(
            sql(&alter, &Dialect::MYSQL),
            ["ALTER TABLE orders CHANGE COLUMN qty quantity bigint NOT NULL DEFAULT 0"]
        );
    }

    /// Oracle names only what changed — naming `NOT NULL` on a column that has
    /// it is ORA-01442 — and spells a dropped default `DEFAULT NULL`.
    #[test]
    fn oracle_modifies_only_what_changed() {
        let base = ColumnDef::new("QTY", "NUMBER")
            .with_not_null(true)
            .with_default("0");

        let mut retype = orders();
        retype.changes.push(change(
            base.clone(),
            ColumnDef {
                type_sql: "NUMBER(10)".to_string(),
                ..base.clone()
            },
        ));
        assert_eq!(
            sql(&retype, &Dialect::ORACLE),
            [r#"ALTER TABLE "orders" MODIFY (QTY NUMBER(10))"#]
        );

        let mut nullable = orders();
        nullable
            .changes
            .push(change(base.clone(), base.clone().with_not_null(false)));
        assert_eq!(
            sql(&nullable, &Dialect::ORACLE),
            [r#"ALTER TABLE "orders" MODIFY (QTY NULL)"#]
        );

        let mut dropped = orders();
        dropped.changes.push(change(
            base.clone(),
            ColumnDef {
                default_sql: None,
                ..base.clone()
            },
        ));
        assert_eq!(
            sql(&dropped, &Dialect::ORACLE),
            [r#"ALTER TABLE "orders" MODIFY (QTY DEFAULT NULL)"#]
        );

        // All three at once, in the order type, default, nullability.
        let mut all = orders();
        all.changes.push(change(
            base.clone(),
            ColumnDef {
                type_sql: "NUMBER(10)".to_string(),
                not_null: false,
                default_sql: Some("1".to_string()),
                ..base.clone()
            },
        ));
        assert_eq!(
            sql(&all, &Dialect::ORACLE),
            [r#"ALTER TABLE "orders" MODIFY (QTY NUMBER(10) DEFAULT 1 NULL)"#]
        );
    }

    /// SQL Server carries type and nullability in one statement, and writes the
    /// nullability every time: omitting it resets the column to nullable.
    #[test]
    fn mssql_always_writes_the_nullability() {
        let base = ColumnDef::new("qty", "int").with_not_null(true);

        let mut retype = orders();
        retype.changes.push(change(
            base.clone(),
            ColumnDef {
                type_sql: "bigint".to_string(),
                ..base.clone()
            },
        ));
        assert_eq!(
            sql(&retype, &Dialect::MSSQL),
            ["ALTER TABLE orders ALTER COLUMN qty bigint NOT NULL"]
        );

        // Only the nullability changed, and the type is restated anyway.
        let mut nullable = orders();
        nullable
            .changes
            .push(change(base.clone(), base.clone().with_not_null(false)));
        assert_eq!(
            sql(&nullable, &Dialect::MSSQL),
            ["ALTER TABLE orders ALTER COLUMN qty int NULL"]
        );
    }

    /// SQL Server has no statement for a default: the name of the constraint
    /// that holds it is not in the catalog, so there is nothing to drop.
    #[test]
    fn mssql_cannot_change_a_default() {
        let base = ColumnDef::new("qty", "int");
        let mut alter = orders();
        alter
            .changes
            .push(change(base.clone(), base.with_default("0")));

        let error = plan_alter(&alter, &Dialect::MSSQL).unwrap_err();
        assert_eq!(
            error,
            DdlError::Unsupported {
                dialect: DialectId::MsSql,
                what: Unsupported::Default,
                column: Some("qty".to_string()),
            }
        );
        let message = error.to_string();
        assert!(message.contains("SQL Server"), "{message}");
        assert!(message.contains("constraint"), "{message}");
    }

    /// SQLite refuses every attribute change, naming the one it was asked for,
    /// and still does the renames and the add and drop.
    #[test]
    fn sqlite_refuses_attribute_changes() {
        let base = ColumnDef::new("qty", "integer");
        for (to, what) in [
            (ColumnDef::new("qty", "bigint"), Unsupported::ColumnType),
            (base.clone().with_not_null(true), Unsupported::Nullability),
            (base.clone().with_default("0"), Unsupported::Default),
        ] {
            let mut alter = orders();
            alter.changes.push(change(base.clone(), to));
            assert_eq!(
                plan_alter(&alter, &Dialect::SQLITE),
                Err(DdlError::Unsupported {
                    dialect: DialectId::Sqlite,
                    what,
                    column: Some("qty".to_string()),
                })
            );
        }

        let message = DdlError::Unsupported {
            dialect: DialectId::Sqlite,
            what: Unsupported::ColumnType,
            column: Some("qty".to_string()),
        }
        .to_string();
        assert!(message.contains("SQLite"), "{message}");
        assert!(message.contains("rebuilt"), "{message}");
        // The product is named the way a person writes it, not the way
        // `drivers.json` spells it.
        assert!(!message.contains("sqlite"), "{message}");

        // What SQLite *can* do still works.
        let mut fine = orders();
        fine.adds.push(ColumnDef::new("note", "text"));
        fine.drops.push("legacy".to_string());
        fine.rename_to = Some("order_lines".to_string());
        assert_eq!(
            sql(&fine, &Dialect::SQLITE),
            [
                "ALTER TABLE orders ADD COLUMN note text",
                "ALTER TABLE orders DROP COLUMN legacy",
                "ALTER TABLE orders RENAME TO order_lines",
            ]
        );
    }

    /// Dropping a constraint: one generic form, MySQL's four, SQLite's refusal.
    #[test]
    fn constraint_drops_are_spelled_per_dialect() {
        let kinds = [
            ConstraintKind::PrimaryKey,
            ConstraintKind::ForeignKey,
            ConstraintKind::Unique,
            ConstraintKind::Check,
        ];
        let mut alter = orders();
        for kind in kinds {
            alter.drop_constraints.push(ConstraintDrop {
                kind,
                name: "c1".to_string(),
            });
        }

        for dialect in [
            Dialect::GENERIC,
            Dialect::H2,
            Dialect::POSTGRES,
            Dialect::ORACLE,
            Dialect::MSSQL,
        ] {
            let expected = format!(
                "ALTER TABLE {} DROP CONSTRAINT {}",
                dialect.qualify(["orders"]),
                dialect.quote_ident("c1")
            );
            assert_eq!(
                sql(&alter, &dialect),
                [
                    expected.clone(),
                    expected.clone(),
                    expected.clone(),
                    expected.clone()
                ],
                "{}",
                dialect.name()
            );
        }

        assert_eq!(
            sql(&alter, &Dialect::MYSQL),
            [
                "ALTER TABLE orders DROP PRIMARY KEY",
                "ALTER TABLE orders DROP FOREIGN KEY c1",
                "ALTER TABLE orders DROP INDEX c1",
                "ALTER TABLE orders DROP CHECK c1",
            ]
        );

        assert_eq!(
            plan_alter(&alter, &Dialect::SQLITE),
            Err(DdlError::Unsupported {
                dialect: DialectId::Sqlite,
                what: Unsupported::ConstraintDrop,
                column: None,
            })
        );
    }

    /// The table's new name is bare everywhere, and SQL Server's rename is a
    /// procedure call rather than a statement.
    #[test]
    fn table_rename_is_bare_and_sp_rename_on_mssql() {
        let mut alter = TableAlter::new(["app", "orders"]);
        alter.rename_to = Some("order lines".to_string());

        assert_eq!(
            sql(&alter, &Dialect::POSTGRES),
            [r#"ALTER TABLE app.orders RENAME TO "order lines""#]
        );
        assert_eq!(
            sql(&alter, &Dialect::ORACLE),
            [r#"ALTER TABLE "app"."orders" RENAME TO "order lines""#]
        );
        assert_eq!(
            sql(&alter, &Dialect::MYSQL),
            ["ALTER TABLE app.orders RENAME TO `order lines`"]
        );
        assert_eq!(
            sql(&alter, &Dialect::MSSQL),
            [r#"EXEC sp_rename 'app.orders', 'order lines'"#]
        );
    }

    /// Every name goes through `quote_ident`, so the same alter reads
    /// differently per dialect — and correctly in each.
    #[test]
    fn identifiers_are_quoted_per_dialect() {
        let mut alter = TableAlter::new(["app", "Orders"]);
        alter.adds.push(ColumnDef::new("order", "integer"));
        alter.drops.push("unit price".to_string());

        // PostgreSQL folds down, so a capital is quoted; `order` is reserved.
        assert_eq!(
            sql(&alter, &Dialect::POSTGRES),
            [
                r#"ALTER TABLE app."Orders" ADD COLUMN "order" integer"#,
                r#"ALTER TABLE app."Orders" DROP COLUMN "unit price""#,
            ]
        );
        // MySQL preserves case and quotes with backticks.
        assert_eq!(
            sql(&alter, &Dialect::MYSQL),
            [
                "ALTER TABLE app.Orders ADD COLUMN `order` integer",
                "ALTER TABLE app.Orders DROP COLUMN `unit price`",
            ]
        );
        // Oracle folds up, so every lower-case name is quoted — and its `ADD`
        // takes no `COLUMN`.
        assert_eq!(
            sql(&alter, &Dialect::ORACLE),
            [
                r#"ALTER TABLE "app"."Orders" ADD "order" integer"#,
                r#"ALTER TABLE "app"."Orders" DROP COLUMN "unit price""#,
            ]
        );
    }

    /// A column that did not change does not belong in the change list.
    #[test]
    fn an_unchanged_column_is_rejected() {
        let mut alter = orders();
        let column = ColumnDef::new("qty", "integer").with_not_null(true);
        alter.changes.push(change(column.clone(), column));

        assert_eq!(
            plan_alter(&alter, &Dialect::POSTGRES),
            Err(DdlError::NoChange {
                column: "qty".to_string()
            })
        );
        assert!(
            plan_alter(&alter, &Dialect::POSTGRES)
                .unwrap_err()
                .to_string()
                .contains("qty")
        );
    }

    /// The rest of the input errors, each with the shape that provokes it.
    #[test]
    fn malformed_input_is_rejected() {
        assert_eq!(plan_alter(&orders(), &Dialect::POSTGRES), Ok(Vec::new()));

        let mut no_table = TableAlter::new(Vec::<String>::new());
        no_table.drops.push("legacy".to_string());
        assert_eq!(
            plan_alter(&no_table, &Dialect::POSTGRES),
            Err(DdlError::NoTable)
        );

        let mut blank_part = TableAlter::new(["", "orders"]);
        blank_part.drops.push("legacy".to_string());
        assert_eq!(
            plan_alter(&blank_part, &Dialect::POSTGRES),
            Err(DdlError::NoTable)
        );

        let mut nameless_drop = orders();
        nameless_drop.drops.push(String::new());
        assert_eq!(
            plan_alter(&nameless_drop, &Dialect::POSTGRES),
            Err(DdlError::NoColumnName)
        );

        let mut nameless_add = orders();
        nameless_add.adds.push(ColumnDef::new("", "integer"));
        assert_eq!(
            plan_alter(&nameless_add, &Dialect::POSTGRES),
            Err(DdlError::NoColumnName)
        );

        let mut nameless_change = orders();
        nameless_change.changes.push(change(
            ColumnDef::new("qty", "integer"),
            ColumnDef::new("", "bigint"),
        ));
        assert_eq!(
            plan_alter(&nameless_change, &Dialect::POSTGRES),
            Err(DdlError::NoColumnName)
        );

        let mut typeless_add = orders();
        typeless_add.adds.push(ColumnDef::new("qty", ""));
        assert_eq!(
            plan_alter(&typeless_add, &Dialect::POSTGRES),
            Err(DdlError::NoTypeSql {
                column: "qty".to_string()
            })
        );

        let mut empty_name = orders();
        empty_name.rename_to = Some(String::new());
        assert_eq!(
            plan_alter(&empty_name, &Dialect::POSTGRES),
            Err(DdlError::NoNewName)
        );
    }

    /// The restating dialects need a type even when only the nullability
    /// changed, and say so; PostgreSQL, which restates nothing, does not.
    #[test]
    fn a_restating_dialect_needs_a_type() {
        let mut alter = orders();
        alter.changes.push(change(
            ColumnDef::new("qty", ""),
            ColumnDef::new("qty", "").with_not_null(true),
        ));

        for dialect in [Dialect::MYSQL, Dialect::MSSQL] {
            assert_eq!(
                plan_alter(&alter, &dialect),
                Err(DdlError::NoTypeSql {
                    column: "qty".to_string()
                }),
                "{}",
                dialect.name()
            );
        }
        assert_eq!(
            sql(&alter, &Dialect::POSTGRES),
            ["ALTER TABLE orders ALTER COLUMN qty SET NOT NULL"]
        );

        let message = DdlError::NoTypeSql {
            column: "qty".to_string(),
        }
        .to_string();
        assert!(message.contains("restate"), "{message}");
    }
}
