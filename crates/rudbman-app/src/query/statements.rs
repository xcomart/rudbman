//! Statements.

use super::*;

/// Whether `sql` is a statement `confirm_writes` should ask about.
///
/// Blank input and a buffer holding nothing but comments are neither reads nor
/// writes: there is no statement, and nothing will be sent.
pub fn is_write_statement(sql: &str, dialect: &Dialect) -> bool {
    let access = statement_access(sql, dialect);
    matches!(access, StatementAccess::Write)
        || matches!(access, StatementAccess::Unknown)
            && (!split_statements(sql, dialect).is_empty()
                || sql.contains("/*!")
                || sql.contains("/*M!"))
}

/// Quotes an identifier the way `dialect` spells a quoted identifier.
///
/// MySQL is the one that has to be told apart: `"` opens a *string* there
/// unless the server runs in `ANSI_QUOTES`, which no client can see, so a
/// column name in double quotes would be compared against its own text. The
/// backtick is what MySQL, SQLite and H2 all accept.
pub(super) fn quote_identifier(name: &str, dialect: &Dialect) -> String {
    if dialect.syntax().double_quoted_strings {
        format!("`{}`", name.replace('`', "``"))
    } else {
        format!("\"{}\"", name.replace('"', "\"\""))
    }
}

/// Wraps `sql` so the server returns it in `column` order.
///
/// A derived table rather than an appended `ORDER BY`, because the original may
/// already have one and two of them do not compose. That is also this
/// approach's limit, and the limit is left visible on purpose: a statement a
/// dialect will not accept inside `FROM (…)` — SQL Server with an inner
/// `ORDER BY` and no `TOP`, a product that will not nest a `WITH` — fails, and
/// the driver's own refusal is shown rather than swallowed. Sorting is offered
/// only for reading statements, so nothing is ever re-executed for its side
/// effects.
pub fn order_by(sql: &str, column: &str, direction: SortDirection, dialect: &Dialect) -> String {
    let inner = sql.trim().trim_end_matches(';').trim_end();
    let order = match direction {
        SortDirection::Ascending => "ASC",
        SortDirection::Descending => "DESC",
    };
    format!(
        "SELECT * FROM ({inner}) {SORT_ALIAS} ORDER BY {} {order}",
        quote_identifier(column, dialect)
    )
}

/// One name part of a column's source, or `None` where the driver would not
/// say.
///
/// The filter is the first thing any of §7.9's gate does, and getting it wrong
/// is the one mistake that would matter: JDBC answers the **empty string**, not
/// null, for a column with no source table, so both spellings mean "unknown"
/// and a comparison that treated `""` as a name would have two computed columns
/// agreeing on a table nobody has.
pub(super) fn name_part(part: &Option<String>) -> Option<&str> {
    part.as_deref().filter(|part| !part.is_empty())
}

/// The one table a result's columns were all read from, when there is one.
///
/// The first two clauses of §7.9's gate, and nothing else — the third, that the
/// table's primary key is present in the result, needs the catalogue and is
/// asked afterwards ([`QueryPane::keyed`]). Answers the table's `(catalog,
/// schema, table)` parts, each already normalised by [`name_part`].
///
/// * A column that names **no** table takes no part in the vote and is not
///   disqualifying. It is a computed column — an expression, a literal, an
///   aggregate — and refusing the whole result because of one would refuse
///   `SELECT id, name, name || '!' FROM users`, where the first two columns are
///   perfectly writable. [`crate::data_edit`]'s column rules are what keep the
///   third read-only.
/// * Every column that **does** name one must agree on the whole triple. Two
///   tables mean a join, and an `UPDATE` names one table.
/// * At least one column must name a table, or there is nothing to write to.
///
/// The metadata is a hint and is allowed to be (§7.9): a driver that reports an
/// alias where the table was asked for, or `""` for a schema it knows perfectly
/// well, only ever costs an editing offer — a wrong table name finds no primary
/// key and the result stays read-only, and a right name over rows that are not
/// its own is caught by the update count of exactly one that every generated
/// statement is checked against. The hint may offer editing; it can never make
/// a statement safe.
pub(super) fn source_table(columns: &[ColumnInfo]) -> Option<TableName> {
    let mut found: Option<(Option<&str>, Option<&str>, &str)> = None;
    for column in columns {
        let Some(table) = name_part(&column.table) else {
            continue;
        };
        let candidate = (name_part(&column.catalog), name_part(&column.schema), table);
        match found {
            None => found = Some(candidate),
            Some(agreed) if agreed == candidate => {}
            // Two tables: a join, and there is no one row for a `WHERE` clause
            // over one key to name.
            Some(_) => return None,
        }
    }
    found.map(|(catalog, schema, table)| {
        (
            catalog.map(str::to_string),
            schema.map(str::to_string),
            table.to_string(),
        )
    })
}
