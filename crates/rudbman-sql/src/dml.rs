//! Writing the statements that put an edited grid back on the server.
//!
//! The result grid stages edits rather than sending them: a changed cell, a row
//! marked for deletion, a row typed into the insert line, all held until the
//! user applies them. This module is what turns that pile into SQL — one
//! `UPDATE` per modified row, one `DELETE` per deleted row, one `INSERT` per new
//! row, in an order a single transaction can carry from top to bottom. It is a
//! sibling of [`mod@ident`](crate::ident) and answers to the same charter: this
//! crate is the one place that *emits* SQL, and every name it emits goes through
//! [`Dialect::quote_ident`].
//!
//! ```
//! use rudbman_sql::{Dialect, DmlKind, DmlValue, RowUpdate, TableEdits, plan_edits};
//!
//! let mut edits = TableEdits::new(["app", "orders"], ["id", "qty"]).with_key([0]);
//! edits.updates.push(RowUpdate {
//!     key: vec![DmlValue::new(DmlKind::I64, "7")],   // the row as it was read
//!     set: vec![(1, DmlValue::new(DmlKind::I64, "3"))],
//! });
//!
//! let batch = plan_edits(&edits, &Dialect::POSTGRES).unwrap();
//! assert_eq!(batch[0].sql, "UPDATE app.orders SET qty = ? WHERE id = ?");
//! assert_eq!(batch[0].values.len(), 2);              // the 3, then the 7
//! ```
//!
//! # Values are bound, never written
//!
//! No [`DmlStatement`] contains a value. Every value in one is a `?`, and the
//! values travel beside the SQL in [`DmlStatement::values`] for the caller to
//! bind as JDBC parameters. That is the whole design, and it is not a matter of
//! taste: a literal cannot be undone. A number rendered through a double arrives
//! rounded, a timestamp rendered in the wrong calendar arrives wrong, and a
//! string rendered with the wrong escaping either fails to parse or — the case
//! that matters — parses into something else and is committed. None of that is
//! visible in the grid afterwards, because the grid shows what the *server* now
//! holds.
//!
//! Formatting a value as text a server will read back unchanged also needs to
//! know things this crate cannot: the driver's own type mapping, the session's
//! calendar, and server modes no client can see (MySQL's `NO_BACKSLASH_ESCAPES`
//! turns one correct escaping into a wrong one). rudbman has exactly one
//! component that knows them, the bridge's Java-side literal formatter, and it
//! is used where a literal is genuinely required — a script the user is meant to
//! read. Reimplementing its per-dialect judgment here would be a second copy of
//! the hardest code in the system, guaranteed to drift from the first.
//!
//! So this module never learns what a value *looks* like. A [`DmlValue`] is a
//! kind tag and an opaque string, and all this module decides is which `?` it
//! lands on and in what order the `?`s appear.
//!
//! # Why NULL in a key is an error
//!
//! Binding NULL is fine in a `SET` clause and in an `INSERT`; it is never fine
//! in the `WHERE` that identifies a row, because `c = ?` bound to NULL matches
//! nothing — the statement would succeed, update zero rows, and the grid would
//! show the edit as applied. Emitting `c IS NULL` instead would be worse: it
//! says the caller had a row whose key was NULL, and a real primary key cannot
//! be. So [`plan_edits`] returns [`DmlError::NullKey`] naming the column, and
//! the caller can tell the user which column it could not identify the row by.
//!
//! # Statement order
//!
//! A batch is all the `DELETE`s, then all the `UPDATE`s, then all the
//! `INSERT`s. The order is what makes the obvious editing session work in one
//! apply: delete a row and type its key into the insert line, and the delete has
//! to reach the server before the insert or the unique index rejects it. The
//! same reasoning puts updates before inserts — an update that moves a value out
//! of the way has to happen before the insert that claims it — and after
//! deletes, since a delete never depends on an update but an update can depend
//! on the room a delete made.
//!
//! Within each group the caller's order is kept. Two edits to the same row are
//! coalesced into one `UPDATE` before that point, so a batch has at most one
//! statement per row per group and their relative order does not change the
//! outcome.
//!
//! # What is deliberately not here
//!
//! * **Optimistic concurrency.** The `WHERE` names the key columns and nothing
//!   else. Comparing every column against the value that was read would detect a
//!   concurrent change, but it also compares columns whose text form does not
//!   round-trip — a `FLOAT`, a `TIMESTAMP` the driver truncated — and would fail
//!   edits that nobody else touched. The row count the server reports is what
//!   tells the caller the row was still there.
//! * **Multi-row `INSERT`.** Each inserted row omits a different set of columns
//!   (see [`InsertCell::Unset`]), so they do not share a column list, and one
//!   statement per row is what lets the caller say which row the server refused.
//! * **`RETURNING` / generated keys.** Which columns a server filled in is a
//!   question for the refresh that follows the apply, not for the statement.

use std::fmt;

use crate::dialect::{Dialect, DialectId};

/// What kind of value a [`DmlValue`]'s text spells.
///
/// The tag exists so the caller can build the right bind parameter without
/// re-deriving the column's type per value: it is what the application resolved
/// once from the column's JDBC `sql_type`. Nothing in this module branches on
/// it — the SQL is the same `?` whatever the kind — and nothing here validates
/// that the text matches it either. The variants are the parameter forms the
/// bridge accepts, no more.
///
/// There is no floating-point kind. A value the user typed or the grid read is
/// exact text, and [`DmlKind::Decimal`] keeps it exact all the way to
/// `setBigDecimal`; routing it through a double instead would round it on the
/// way to the server, which is the one thing an editor must not do.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DmlKind {
    /// Text of any length: `CHAR`, `VARCHAR`, `CLOB`.
    Str,
    /// An integer that fits in 64 bits.
    I64,
    /// An exact number, in its plain string form — never exponent notation.
    ///
    /// Also where anything numeric that is not an integer belongs.
    Decimal,
    /// A boolean, as `true` or `false`.
    Bool,
    /// A date, `YYYY-MM-DD`.
    Date,
    /// A time, `HH:MM:SS`.
    Time,
    /// A timestamp, `YYYY-MM-DD HH:MM:SS[.fffffffff]`.
    Timestamp,
    /// Binary data, in whatever encoding the caller decodes on the way out.
    ///
    /// This module never looks at the text, so the encoding is between the grid
    /// and the parameter builder.
    Bytes,
}

/// One value bound to one `?` of a generated statement.
///
/// A kind and an optional text: `None` is SQL NULL. It is a struct rather than
/// an enum with a `Null` variant because a NULL still has a type — JDBC's
/// `setNull` takes one, and a driver that is handed the wrong one for a column
/// rejects the statement. Making the type a field keeps "every bound value knows
/// its column's kind" true by construction, where an enum would make a typeless
/// NULL representable and push the question onto every caller.
///
/// The text is opaque here. See the [module documentation](self) for why this
/// module never formats or inspects it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DmlValue {
    /// The column's type, for the caller building the bind parameter.
    kind: DmlKind,
    /// The value in its canonical text form, or `None` for SQL NULL.
    text: Option<String>,
}

impl DmlValue {
    /// A value of `kind` spelled `text`.
    ///
    /// An empty `text` is an empty value, not NULL — the two are distinct
    /// everywhere it matters, and conflating them is how an editor silently
    /// erases a row's contents. Use [`DmlValue::null`] for NULL.
    pub fn new(kind: DmlKind, text: impl Into<String>) -> Self {
        DmlValue {
            kind,
            text: Some(text.into()),
        }
    }

    /// SQL NULL, in a column of `kind`.
    pub const fn null(kind: DmlKind) -> Self {
        DmlValue { kind, text: None }
    }

    /// The column's type.
    pub const fn kind(&self) -> DmlKind {
        self.kind
    }

    /// The text, or `None` for SQL NULL.
    pub fn text(&self) -> Option<&str> {
        self.text.as_deref()
    }

    /// Whether this is SQL NULL.
    pub const fn is_null(&self) -> bool {
        self.text.is_none()
    }
}

/// One column of a row being inserted.
///
/// The distinction that matters is [`InsertCell::Unset`] against
/// `Set(DmlValue::null(..))`: the first leaves the column out of the statement
/// and lets the server supply it, the second writes NULL over whatever the
/// server would have supplied. That is how an auto-increment key and a
/// `DEFAULT CURRENT_TIMESTAMP` work without this module knowing they exist —
/// the grid leaves the cell untouched, the column never appears, and the server
/// does what it was configured to do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InsertCell {
    /// Name the column in the statement and bind this value.
    Set(DmlValue),
    /// Leave the column out of the statement entirely.
    Unset,
}

/// One row's `UPDATE`: which columns changed, and which row they changed in.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RowUpdate {
    /// The row's key values **as they were read**, in [`TableEdits::key`]
    /// order.
    ///
    /// Original, not edited: a key column the user retyped is set to its new
    /// value and found by its old one, which is the only way the statement can
    /// name a row that still exists.
    pub key: Vec<DmlValue>,
    /// The changed columns, as `(index into `[`TableEdits::columns`]`, new
    /// value)`.
    ///
    /// All of a row's changed cells belong in one [`RowUpdate`]; they become one
    /// statement, so the row is either wholly changed or wholly not.
    pub set: Vec<(usize, DmlValue)>,
}

/// Everything staged against one table, and the input to [`plan_edits`].
///
/// The three change lists address columns by index into [`TableEdits::columns`]
/// rather than by name, because the grid already holds them that way and a name
/// would have to be matched back against the catalog's spelling — the one thing
/// [`Dialect::quote_ident`] exists to avoid guessing at.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct TableEdits {
    /// The table's name parts, most significant first: catalog, schema, name.
    ///
    /// Parts a driver left null are the caller's to drop before they get here;
    /// an empty part is [`DmlError::NoTable`] rather than an empty quoted name
    /// in the middle of a qualified one.
    pub table: Vec<String>,
    /// The table's column names, in the order the indices below address.
    pub columns: Vec<String>,
    /// Indices into [`TableEdits::columns`] of the columns that identify a row.
    ///
    /// A primary key, or whatever the caller decided to stand in for one.
    /// Required for updates and deletes, unused by inserts.
    pub key: Vec<usize>,
    /// One entry per deleted row: that row's key values, in `key` order.
    pub deletes: Vec<Vec<DmlValue>>,
    /// One entry per modified row.
    pub updates: Vec<RowUpdate>,
    /// One entry per new row: one cell per column, in `columns` order.
    pub inserts: Vec<Vec<InsertCell>>,
}

impl TableEdits {
    /// An empty set of edits against `table`, whose columns are `columns`.
    ///
    /// ```
    /// use rudbman_sql::TableEdits;
    ///
    /// let edits = TableEdits::new(["app", "orders"], ["id", "qty"]).with_key([0]);
    /// assert_eq!(edits.columns, ["id", "qty"]);
    /// ```
    pub fn new(
        table: impl IntoIterator<Item = impl Into<String>>,
        columns: impl IntoIterator<Item = impl Into<String>>,
    ) -> Self {
        TableEdits {
            table: table.into_iter().map(Into::into).collect(),
            columns: columns.into_iter().map(Into::into).collect(),
            ..Self::default()
        }
    }

    /// Sets the key columns, by index into [`TableEdits::columns`].
    pub fn with_key(mut self, key: impl IntoIterator<Item = usize>) -> Self {
        self.key = key.into_iter().collect();
        self
    }

    /// Whether there is nothing to apply.
    pub fn is_empty(&self) -> bool {
        self.deletes.is_empty() && self.updates.is_empty() && self.inserts.is_empty()
    }
}

/// One statement of a batch: the SQL, and the values its `?`s take in order.
///
/// Named for what it carries rather than borrowing this crate's other sense of
/// the word — a [`StatementSpan`](crate::StatementSpan) is a range of a script
/// somebody typed, and this is a statement rudbman wrote.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DmlStatement {
    /// The SQL, with a `?` everywhere a value goes and no value anywhere.
    pub sql: String,
    /// The values, in the order their `?`s appear in [`DmlStatement::sql`].
    ///
    /// For an `UPDATE` that is the assignments first and the key second, which
    /// is the order the clauses appear in.
    pub values: Vec<DmlValue>,
}

/// Why a set of edits could not be turned into statements.
///
/// Every variant is a caller mistake — a key that is not a key, an index that is
/// not a column — rather than anything a user did, with the exception of
/// [`DmlError::NullKey`], which is worth showing: it names the column the grid
/// could not identify the row by.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DmlError {
    /// The table has no name, or one of its parts is empty.
    NoTable,
    /// A row had to be identified and no key columns were given.
    NoKey,
    /// A row's key does not have one value per key column.
    KeyArity {
        /// How many key columns [`TableEdits::key`] names.
        expected: usize,
        /// How many values the row supplied.
        found: usize,
    },
    /// The original value of a key column is NULL, so no `WHERE` can find the
    /// row. See the [module documentation](self).
    NullKey {
        /// The key column's name, for the message shown to the user.
        column: String,
    },
    /// An update that assigns nothing. A row with no changed cells does not
    /// belong in [`TableEdits::updates`].
    NoAssignments,
    /// One update assigns the same column twice; a row's cells coalesce into one
    /// assignment each.
    DuplicateAssignment {
        /// The column named twice.
        column: String,
    },
    /// A column index that is not in [`TableEdits::columns`].
    NoSuchColumn {
        /// The index given.
        index: usize,
        /// How many columns the table has.
        columns: usize,
    },
    /// An inserted row does not have one cell per column.
    CellArity {
        /// How many columns the table has.
        expected: usize,
        /// How many cells the row supplied.
        found: usize,
    },
    /// Every column of an inserted row is [`InsertCell::Unset`] and this dialect
    /// has no way to say so. Oracle only — see [`plan_edits`].
    NoEmptyInsert {
        /// The dialect that cannot express it.
        dialect: DialectId,
    },
}

impl fmt::Display for DmlError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DmlError::NoTable => f.write_str("the table has no usable name"),
            DmlError::NoKey => f.write_str(
                "the table has no key columns, so no row can be updated or deleted by name",
            ),
            DmlError::KeyArity { expected, found } => write!(
                f,
                "a row was identified by {found} value(s), but the key has {expected} column(s)"
            ),
            DmlError::NullKey { column } => write!(
                f,
                "the original value of key column `{column}` is NULL, so no statement can find that row"
            ),
            DmlError::NoAssignments => f.write_str("an updated row has no changed columns"),
            DmlError::DuplicateAssignment { column } => {
                write!(f, "column `{column}` is assigned twice in one update")
            }
            DmlError::NoSuchColumn { index, columns } => write!(
                f,
                "column index {index} is out of range: the table has {columns} column(s)"
            ),
            DmlError::CellArity { expected, found } => write!(
                f,
                "an inserted row has {found} value(s) for a table of {expected} column(s)"
            ),
            DmlError::NoEmptyInsert { dialect } => write!(
                f,
                "an inserted row sets no columns, and {} has no syntax for that",
                dialect.as_str()
            ),
        }
    }
}

impl std::error::Error for DmlError {}

/// Turns one table's staged edits into the statements that apply them.
///
/// The batch is ordered deletes, updates, inserts; see the [module
/// documentation](self) for why, and for why no value appears in any of the
/// returned SQL.
///
/// A row whose every cell is [`InsertCell::Unset`] — the user asked for a row of
/// nothing but defaults — becomes the dialect's empty insert: `INSERT INTO t
/// DEFAULT VALUES` for everything that takes the standard form, `INSERT INTO t
/// () VALUES ()` for MySQL, which does not. Oracle has neither and is
/// [`DmlError::NoEmptyInsert`]; the alternative would be to invent a column list
/// out of the catalog's defaults, which is a guess this module is in no position
/// to make.
///
/// ```
/// use rudbman_sql::{Dialect, InsertCell, TableEdits, plan_edits};
///
/// let mut edits = TableEdits::new(["t"], ["id", "note"]).with_key([0]);
/// edits.inserts.push(vec![InsertCell::Unset, InsertCell::Unset]);
///
/// assert_eq!(
///     plan_edits(&edits, &Dialect::POSTGRES).unwrap()[0].sql,
///     "INSERT INTO t DEFAULT VALUES"
/// );
/// assert_eq!(
///     plan_edits(&edits, &Dialect::MYSQL).unwrap()[0].sql,
///     "INSERT INTO t () VALUES ()"
/// );
/// assert!(plan_edits(&edits, &Dialect::ORACLE).is_err());
/// ```
pub fn plan_edits(edits: &TableEdits, dialect: &Dialect) -> Result<Vec<DmlStatement>, DmlError> {
    if edits.is_empty() {
        return Ok(Vec::new());
    }
    if edits.table.is_empty() || edits.table.iter().any(|part| part.is_empty()) {
        return Err(DmlError::NoTable);
    }
    let table = dialect.qualify(edits.table.iter().map(String::as_str));

    let mut batch =
        Vec::with_capacity(edits.deletes.len() + edits.updates.len() + edits.inserts.len());
    for key in &edits.deletes {
        batch.push(delete(&table, key, edits, dialect)?);
    }
    for update in &edits.updates {
        batch.push(self::update(&table, update, edits, dialect)?);
    }
    for cells in &edits.inserts {
        batch.push(insert(&table, cells, edits, dialect)?);
    }
    Ok(batch)
}

/// `DELETE FROM t WHERE k = ? [AND ...]`.
fn delete(
    table: &str,
    key: &[DmlValue],
    edits: &TableEdits,
    dialect: &Dialect,
) -> Result<DmlStatement, DmlError> {
    let mut sql = format!("DELETE FROM {table}");
    let mut values = Vec::with_capacity(key.len());
    push_where(&mut sql, &mut values, key, edits, dialect)?;
    Ok(DmlStatement { sql, values })
}

/// `UPDATE t SET c = ? [, ...] WHERE k = ? [AND ...]`.
///
/// The assignments come from the row's changed cells and the `WHERE` from its
/// original key, so a row whose key the user retyped is set to the new key and
/// found by the old one.
fn update(
    table: &str,
    row: &RowUpdate,
    edits: &TableEdits,
    dialect: &Dialect,
) -> Result<DmlStatement, DmlError> {
    if row.set.is_empty() {
        return Err(DmlError::NoAssignments);
    }
    let mut sql = format!("UPDATE {table} SET ");
    let mut values = Vec::with_capacity(row.set.len() + row.key.len());
    for (position, (index, value)) in row.set.iter().enumerate() {
        let name = column(edits, *index)?;
        if row.set[..position].iter().any(|(seen, _)| seen == index) {
            return Err(DmlError::DuplicateAssignment {
                column: name.to_string(),
            });
        }
        if position > 0 {
            sql.push_str(", ");
        }
        sql.push_str(&dialect.quote_ident(name));
        sql.push_str(" = ?");
        values.push(value.clone());
    }
    push_where(&mut sql, &mut values, &row.key, edits, dialect)?;
    Ok(DmlStatement { sql, values })
}

/// `INSERT INTO t (c, ...) VALUES (?, ...)`, over the columns that are set.
fn insert(
    table: &str,
    cells: &[InsertCell],
    edits: &TableEdits,
    dialect: &Dialect,
) -> Result<DmlStatement, DmlError> {
    if cells.len() != edits.columns.len() {
        return Err(DmlError::CellArity {
            expected: edits.columns.len(),
            found: cells.len(),
        });
    }

    let mut names = String::new();
    let mut placeholders = String::new();
    let mut values = Vec::new();
    for (index, cell) in cells.iter().enumerate() {
        let InsertCell::Set(value) = cell else {
            continue;
        };
        if !values.is_empty() {
            names.push_str(", ");
            placeholders.push_str(", ");
        }
        names.push_str(&dialect.quote_ident(&edits.columns[index]));
        placeholders.push('?');
        values.push(value.clone());
    }

    if values.is_empty() {
        return Ok(DmlStatement {
            sql: empty_insert(table, dialect)?,
            values,
        });
    }
    Ok(DmlStatement {
        sql: format!("INSERT INTO {table} ({names}) VALUES ({placeholders})"),
        values,
    })
}

/// The dialect's way of saying "a row of nothing but defaults".
fn empty_insert(table: &str, dialect: &Dialect) -> Result<String, DmlError> {
    match dialect.id() {
        // The MySQL family rejects `DEFAULT VALUES` and spells it with two
        // empty lists. MariaDB is named beside MySQL rather than left to the
        // fallback below: it inherited the refusal along with the grammar, and
        // the arm it would otherwise fall into writes the form neither takes.
        DialectId::MySql | DialectId::MariaDb => Ok(format!("INSERT INTO {table} () VALUES ()")),
        // Oracle has neither form. Its `INSERT INTO t VALUES (DEFAULT, ...)`
        // needs one `DEFAULT` per column, and a column without a default is a
        // different statement again — a shape only the catalog could build, and
        // wrongly at that, since a NOT NULL column with no default has no valid
        // row to insert here at all.
        DialectId::Oracle => Err(DmlError::NoEmptyInsert {
            dialect: DialectId::Oracle,
        }),
        // The standard form: PostgreSQL, SQLite, H2, SQL Server, and generic.
        _ => Ok(format!("INSERT INTO {table} DEFAULT VALUES")),
    }
}

/// Appends ` WHERE k = ? [AND ...]` and the key values behind it.
fn push_where(
    sql: &mut String,
    values: &mut Vec<DmlValue>,
    key: &[DmlValue],
    edits: &TableEdits,
    dialect: &Dialect,
) -> Result<(), DmlError> {
    if edits.key.is_empty() {
        return Err(DmlError::NoKey);
    }
    if key.len() != edits.key.len() {
        return Err(DmlError::KeyArity {
            expected: edits.key.len(),
            found: key.len(),
        });
    }
    sql.push_str(" WHERE ");
    for (position, (index, value)) in edits.key.iter().zip(key).enumerate() {
        let name = column(edits, *index)?;
        if value.is_null() {
            return Err(DmlError::NullKey {
                column: name.to_string(),
            });
        }
        if position > 0 {
            sql.push_str(" AND ");
        }
        sql.push_str(&dialect.quote_ident(name));
        sql.push_str(" = ?");
        values.push(value.clone());
    }
    Ok(())
}

/// The name of column `index`, or [`DmlError::NoSuchColumn`].
fn column(edits: &TableEdits, index: usize) -> Result<&str, DmlError> {
    edits
        .columns
        .get(index)
        .map(String::as_str)
        .ok_or(DmlError::NoSuchColumn {
            index,
            columns: edits.columns.len(),
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `orders(id, qty, note)`, keyed by `id`, in schema `app`.
    fn orders() -> TableEdits {
        TableEdits::new(["app", "orders"], ["id", "qty", "note"]).with_key([0])
    }

    /// A text value.
    fn text(value: &str) -> DmlValue {
        DmlValue::new(DmlKind::Str, value)
    }

    /// An integer value.
    fn int(value: &str) -> DmlValue {
        DmlValue::new(DmlKind::I64, value)
    }

    /// The SQL of every statement in a batch.
    fn sql(edits: &TableEdits, dialect: &Dialect) -> Vec<String> {
        plan_edits(edits, dialect)
            .unwrap()
            .into_iter()
            .map(|s| s.sql)
            .collect()
    }

    /// A row's changed cells become one statement, in the order given.
    #[test]
    fn multiple_cells_coalesce_into_one_update() {
        let mut edits = orders();
        edits.updates.push(RowUpdate {
            key: vec![int("7")],
            set: vec![(1, int("3")), (2, text("late"))],
        });

        let batch = plan_edits(&edits, &Dialect::POSTGRES).unwrap();
        assert_eq!(batch.len(), 1);
        assert_eq!(
            batch[0].sql,
            "UPDATE app.orders SET qty = ?, note = ? WHERE id = ?"
        );
        // Assignments first, key second: the order the `?`s appear in.
        assert_eq!(batch[0].values, [int("3"), text("late"), int("7")]);
    }

    /// A retyped key is set to the new value and found by the old one.
    #[test]
    fn editing_the_key_sets_new_and_matches_original() {
        let mut edits = orders();
        edits.updates.push(RowUpdate {
            key: vec![int("7")],
            set: vec![(0, int("8"))],
        });

        let batch = plan_edits(&edits, &Dialect::POSTGRES).unwrap();
        assert_eq!(batch[0].sql, "UPDATE app.orders SET id = ? WHERE id = ?");
        assert_eq!(batch[0].values, [int("8"), int("7")]);
    }

    /// Every key column is ANDed, in the order `key` lists them — which is not
    /// necessarily the column order.
    #[test]
    fn composite_keys_are_anded() {
        let mut edits = TableEdits::new(["lines"], ["order_id", "line_no", "qty"]).with_key([1, 0]);
        edits.updates.push(RowUpdate {
            key: vec![int("2"), int("7")],
            set: vec![(2, int("5"))],
        });
        edits.deletes.push(vec![int("3"), int("7")]);

        assert_eq!(
            sql(&edits, &Dialect::POSTGRES),
            [
                "DELETE FROM lines WHERE line_no = ? AND order_id = ?",
                "UPDATE lines SET qty = ? WHERE line_no = ? AND order_id = ?",
            ]
        );
        let batch = plan_edits(&edits, &Dialect::POSTGRES).unwrap();
        assert_eq!(batch[0].values, [int("3"), int("7")]);
        assert_eq!(batch[1].values, [int("5"), int("2"), int("7")]);
    }

    /// A delete names the key and nothing else.
    #[test]
    fn delete_is_the_key_only() {
        let mut edits = orders();
        edits.deletes.push(vec![int("7")]);

        let batch = plan_edits(&edits, &Dialect::POSTGRES).unwrap();
        assert_eq!(batch[0].sql, "DELETE FROM app.orders WHERE id = ?");
        assert_eq!(batch[0].values, [int("7")]);
    }

    /// NULL and the empty string are different bound values, and neither is
    /// written into the SQL.
    #[test]
    fn null_and_empty_string_are_distinct() {
        let mut edits = orders();
        edits.updates.push(RowUpdate {
            key: vec![int("7")],
            set: vec![(2, DmlValue::null(DmlKind::Str))],
        });
        edits.updates.push(RowUpdate {
            key: vec![int("8")],
            set: vec![(2, text(""))],
        });

        let batch = plan_edits(&edits, &Dialect::POSTGRES).unwrap();
        assert_eq!(batch[0].sql, "UPDATE app.orders SET note = ? WHERE id = ?");
        assert_eq!(batch[0].sql, batch[1].sql);
        assert_ne!(batch[0].values, batch[1].values);
        assert!(batch[0].values[0].is_null());
        assert_eq!(batch[1].values[0].text(), Some(""));
        // Neither spelling reached the statement.
        for statement in &batch {
            assert!(!statement.sql.contains("NULL"), "{}", statement.sql);
            assert!(!statement.sql.contains('\''), "{}", statement.sql);
        }
    }

    /// An unset column is left out, which is how a server default fills it in.
    #[test]
    fn insert_omits_unset_columns() {
        let mut edits = orders();
        edits.inserts.push(vec![
            InsertCell::Unset, // auto-increment `id`
            InsertCell::Set(int("2")),
            InsertCell::Set(DmlValue::null(DmlKind::Str)),
        ]);

        let batch = plan_edits(&edits, &Dialect::POSTGRES).unwrap();
        assert_eq!(
            batch[0].sql,
            "INSERT INTO app.orders (qty, note) VALUES (?, ?)"
        );
        // The explicit NULL is bound; the unset column is not there at all.
        assert_eq!(batch[0].values, [int("2"), DmlValue::null(DmlKind::Str)]);
    }

    /// A row of nothing but defaults, in the three shapes it has.
    #[test]
    fn insert_with_every_column_unset() {
        let mut edits = orders();
        edits.inserts.push(vec![
            InsertCell::Unset,
            InsertCell::Unset,
            InsertCell::Unset,
        ]);

        for dialect in [
            Dialect::POSTGRES,
            Dialect::SQLITE,
            Dialect::H2,
            Dialect::MSSQL,
            Dialect::GENERIC,
        ] {
            let batch = plan_edits(&edits, &dialect).unwrap();
            assert_eq!(
                batch[0].sql,
                format!(
                    "INSERT INTO {} DEFAULT VALUES",
                    dialect.qualify(["app", "orders"])
                ),
                "{}",
                dialect.name()
            );
            assert!(batch[0].values.is_empty());
        }

        assert_eq!(
            sql(&edits, &Dialect::MYSQL),
            ["INSERT INTO app.orders () VALUES ()"]
        );
        assert_eq!(
            plan_edits(&edits, &Dialect::ORACLE),
            Err(DmlError::NoEmptyInsert {
                dialect: DialectId::Oracle
            })
        );
    }

    /// Deletes, then updates, then inserts — whatever order they were staged
    /// in, so that deleting a row and re-entering its key applies in one go.
    #[test]
    fn batch_is_ordered_delete_update_insert() {
        let mut edits = orders();
        edits.inserts.push(vec![
            InsertCell::Set(int("7")),
            InsertCell::Set(int("1")),
            InsertCell::Unset,
        ]);
        edits.updates.push(RowUpdate {
            key: vec![int("9")],
            set: vec![(1, int("4"))],
        });
        edits.deletes.push(vec![int("7")]);

        assert_eq!(
            sql(&edits, &Dialect::POSTGRES),
            [
                "DELETE FROM app.orders WHERE id = ?",
                "UPDATE app.orders SET qty = ? WHERE id = ?",
                "INSERT INTO app.orders (id, qty) VALUES (?, ?)",
            ]
        );
    }

    /// Within a group the caller's order survives.
    #[test]
    fn rows_keep_their_order_within_a_group() {
        let mut edits = orders();
        for id in ["1", "2", "3"] {
            edits.deletes.push(vec![int(id)]);
        }
        let batch = plan_edits(&edits, &Dialect::POSTGRES).unwrap();
        let ids: Vec<_> = batch.iter().map(|s| s.values[0].clone()).collect();
        assert_eq!(ids, [int("1"), int("2"), int("3")]);
    }

    /// Every name goes through `quote_ident`, so the same edit reads
    /// differently per dialect — and correctly in each.
    #[test]
    fn identifiers_are_quoted_per_dialect() {
        let mut edits =
            TableEdits::new(["app", "Orders"], ["id", "order", "unit price"]).with_key([0]);
        edits.updates.push(RowUpdate {
            key: vec![int("7")],
            set: vec![(1, int("2")), (2, DmlValue::new(DmlKind::Decimal, "1.50"))],
        });

        // PostgreSQL folds down, so a capital is quoted; `order` is reserved.
        assert_eq!(
            sql(&edits, &Dialect::POSTGRES),
            [concat!(
                r#"UPDATE app."Orders" SET "order" = ?, "unit price" = ? "#,
                "WHERE id = ?"
            )]
        );
        // MySQL preserves case and quotes with backticks.
        assert_eq!(
            sql(&edits, &Dialect::MYSQL),
            ["UPDATE app.Orders SET `order` = ?, `unit price` = ? WHERE id = ?"]
        );
        // Oracle folds up, so every lower-case name is quoted.
        assert_eq!(
            sql(&edits, &Dialect::ORACLE),
            [concat!(
                r#"UPDATE "app"."Orders" SET "order" = ?, "unit price" = ? "#,
                r#"WHERE "id" = ?"#
            )]
        );
    }

    /// The same, for the statements that name a column list.
    #[test]
    fn insert_and_delete_quote_too() {
        let mut edits = TableEdits::new(["Orders"], ["id", "select"]).with_key([0]);
        edits.deletes.push(vec![int("1")]);
        edits
            .inserts
            .push(vec![InsertCell::Unset, InsertCell::Set(text("x"))]);

        assert_eq!(
            sql(&edits, &Dialect::POSTGRES),
            [
                r#"DELETE FROM "Orders" WHERE id = ?"#,
                r#"INSERT INTO "Orders" ("select") VALUES (?)"#,
            ]
        );
        assert_eq!(
            sql(&edits, &Dialect::SQLITE),
            [
                "DELETE FROM Orders WHERE id = ?",
                r#"INSERT INTO Orders ("select") VALUES (?)"#,
            ]
        );
    }

    /// A NULL among a row's original key values names the column it came from.
    #[test]
    fn null_in_the_original_key_is_an_error() {
        let mut edits = TableEdits::new(["lines"], ["order_id", "line_no", "qty"]).with_key([0, 1]);
        edits
            .deletes
            .push(vec![int("7"), DmlValue::null(DmlKind::I64)]);

        assert_eq!(
            plan_edits(&edits, &Dialect::POSTGRES),
            Err(DmlError::NullKey {
                column: "line_no".to_string()
            })
        );
        assert!(
            plan_edits(&edits, &Dialect::POSTGRES)
                .unwrap_err()
                .to_string()
                .contains("line_no")
        );
    }

    /// The same rule on the update path.
    #[test]
    fn null_key_on_an_update_too() {
        let mut edits = orders();
        edits.updates.push(RowUpdate {
            key: vec![DmlValue::null(DmlKind::I64)],
            set: vec![(1, int("3"))],
        });
        assert_eq!(
            plan_edits(&edits, &Dialect::POSTGRES),
            Err(DmlError::NullKey {
                column: "id".to_string()
            })
        );
    }

    /// The rest of the input errors, each with the shape that provokes it.
    #[test]
    fn malformed_input_is_rejected() {
        let empty = orders();
        assert_eq!(plan_edits(&empty, &Dialect::POSTGRES), Ok(Vec::new()));

        let mut no_table = TableEdits::new(Vec::<String>::new(), ["id"]).with_key([0]);
        no_table.deletes.push(vec![int("1")]);
        assert_eq!(
            plan_edits(&no_table, &Dialect::POSTGRES),
            Err(DmlError::NoTable)
        );

        let mut blank_part = TableEdits::new(["", "t"], ["id"]).with_key([0]);
        blank_part.deletes.push(vec![int("1")]);
        assert_eq!(
            plan_edits(&blank_part, &Dialect::POSTGRES),
            Err(DmlError::NoTable)
        );

        let mut no_key = TableEdits::new(["t"], ["id"]);
        no_key.deletes.push(vec![int("1")]);
        assert_eq!(
            plan_edits(&no_key, &Dialect::POSTGRES),
            Err(DmlError::NoKey)
        );

        let mut arity = orders();
        arity.deletes.push(vec![int("1"), int("2")]);
        assert_eq!(
            plan_edits(&arity, &Dialect::POSTGRES),
            Err(DmlError::KeyArity {
                expected: 1,
                found: 2
            })
        );

        let mut nothing_set = orders();
        nothing_set.updates.push(RowUpdate {
            key: vec![int("1")],
            set: Vec::new(),
        });
        assert_eq!(
            plan_edits(&nothing_set, &Dialect::POSTGRES),
            Err(DmlError::NoAssignments)
        );

        let mut twice = orders();
        twice.updates.push(RowUpdate {
            key: vec![int("1")],
            set: vec![(1, int("2")), (1, int("3"))],
        });
        assert_eq!(
            plan_edits(&twice, &Dialect::POSTGRES),
            Err(DmlError::DuplicateAssignment {
                column: "qty".to_string()
            })
        );

        let mut out_of_range = orders();
        out_of_range.updates.push(RowUpdate {
            key: vec![int("1")],
            set: vec![(9, int("2"))],
        });
        assert_eq!(
            plan_edits(&out_of_range, &Dialect::POSTGRES),
            Err(DmlError::NoSuchColumn {
                index: 9,
                columns: 3
            })
        );

        let mut short_row = orders();
        short_row.inserts.push(vec![InsertCell::Set(int("1"))]);
        assert_eq!(
            plan_edits(&short_row, &Dialect::POSTGRES),
            Err(DmlError::CellArity {
                expected: 3,
                found: 1
            })
        );
    }

    /// A NULL keeps its column's type, which is what `setNull` needs.
    #[test]
    fn a_null_still_has_a_kind() {
        let null = DmlValue::null(DmlKind::Timestamp);
        assert!(null.is_null());
        assert_eq!(null.kind(), DmlKind::Timestamp);
        assert_eq!(null.text(), None);
        assert_ne!(null, DmlValue::null(DmlKind::Str));
        assert_ne!(null, DmlValue::new(DmlKind::Timestamp, ""));
    }
}
