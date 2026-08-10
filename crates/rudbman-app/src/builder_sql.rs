//! The query builder's model, and the `SELECT` it comes out as.
//!
//! Everything the builder *is* — which tables are on the canvas, what they are
//! called, which of their columns are picked, what joins them, and the four
//! clause lists edited in the form below — is here, in plain data with no view
//! anywhere near it. [`BuilderPane`](crate::builder_pane::BuilderPane) owns one
//! of these and draws it; this module turns it into text.
//!
//! That split is what makes the generator testable. The interesting cases — a
//! three-table join, a self-join, a name that has to be quoted, a table nothing
//! joins to — are all cases about the *string*, and none of them needs a window
//! to be asserted. The panel's own tests are then free to be about the panel.
//!
//! # What the generator will and will not do
//!
//! It emits one `SELECT` over the tables it is given, in the order it is given
//! them, and it never reorders them: the first table is the one the `FROM`
//! names and every table after it is attached to what came before, by a join
//! when an edge reaches back and by a comma when none does. There is no join
//! planner here and there is not meant to be — the user drew the edges, and a
//! statement that rearranged them would stop matching the picture.
//!
//! Identifiers go through [`Dialect::quote_ident`] and [`Dialect::qualify`], so
//! a name is quoted only when leaving it bare would change it (see
//! `rudbman_sql::ident`). An ordinary name in the catalog's own case therefore
//! comes out exactly as it was typed, which is what keeps the generated text
//! readable.
//!
//! Table aliases are written with a space and never with `AS`. Oracle rejects
//! `AS` on a *table* alias — it is legal only on a column one — and a space is
//! accepted everywhere, so there is nothing to branch on.

use rudbman_sql::Dialect;

/// One table on the canvas: where it lives, what it is called here, and the
/// columns that can be picked from it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BuilderTable {
    /// Its catalogue, when the product has them and the driver reported one.
    pub catalog: Option<String>,
    /// Its schema, likewise.
    pub schema: Option<String>,
    /// The table's own name, as the catalogue spells it.
    pub name: String,
    /// What the statement calls it.
    ///
    /// Equal to [`BuilderTable::name`] unless the same table is on the canvas
    /// twice, which is how a self-join is expressed; see [`unique_alias`]. An
    /// alias equal to the name is not written out at all.
    pub alias: String,
    /// The column names, in catalogue order. Indices into this are what
    /// [`BuilderQuery::selected`] and [`Join`] name.
    pub columns: Vec<String>,
}

/// The four join types the form offers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JoinKind {
    /// Rows that match on both sides.
    Inner,
    /// Every row of the tables already in the statement.
    Left,
    /// Every row of the table being attached.
    Right,
    /// Every row of both.
    Full,
}

impl JoinKind {
    /// The four, in the order the dropdown lists them.
    pub const ALL: [JoinKind; 4] = [
        JoinKind::Inner,
        JoinKind::Left,
        JoinKind::Right,
        JoinKind::Full,
    ];

    /// The keyword that precedes `JOIN`.
    ///
    /// `OUTER` is left off `LEFT`, `RIGHT` and `FULL`: it is optional in the
    /// standard and in every product here, and the shorter form is what people
    /// write.
    fn keyword(self) -> &'static str {
        match self {
            JoinKind::Inner => "INNER",
            JoinKind::Left => "LEFT",
            JoinKind::Right => "RIGHT",
            JoinKind::Full => "FULL",
        }
    }
}

/// One edge of the canvas, with the type the form gave it.
///
/// The endpoints are `(table index, column index)` into
/// [`BuilderQuery::tables`] — the same pair `rudbman_erd::BuilderEdge` carries,
/// so an edge drawn on the canvas becomes one of these without a lookup.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Join {
    /// The column the drag began at.
    pub from: (usize, usize),
    /// The column it ended at.
    pub to: (usize, usize),
    /// What kind of join it is; `INNER` until the form says otherwise.
    pub kind: JoinKind,
}

/// Which way an `ORDER BY` term sorts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SortDir {
    /// Ascending, written out rather than left implicit.
    Asc,
    /// Descending.
    Desc,
}

impl SortDir {
    /// The keyword that follows the column.
    ///
    /// `ASC` is written even though it is the default: the form offers three
    /// states — none, ascending, descending — and a statement where two of them
    /// looked identical would make the control seem broken.
    fn keyword(self) -> &'static str {
        match self {
            SortDir::Asc => "ASC",
            SortDir::Desc => "DESC",
        }
    }
}

/// Everything one builder tab holds, as data.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BuilderQuery {
    /// The tables, in the order they were added, which is the order the `FROM`
    /// attaches them in.
    pub tables: Vec<BuilderTable>,
    /// The joins, in the order they were drawn.
    pub joins: Vec<Join>,
    /// The picked columns, in the order they were picked — which is the order
    /// the select list writes them in. A `Vec` rather than a set for exactly
    /// that reason.
    pub selected: Vec<(usize, usize)>,
    /// The `WHERE` rows, free text. Blank rows are ignored and the rest are
    /// combined with `AND`.
    pub where_clauses: Vec<String>,
    /// The columns grouped by.
    pub group_by: Vec<(usize, usize)>,
    /// The columns sorted by, in the order the terms are written.
    pub order_by: Vec<((usize, usize), SortDir)>,
}

/// An alias for `name` that no entry of `taken` already uses.
///
/// The table's own name when it is free, and `name_2`, `name_3`… when it is
/// not. That is the whole of the self-join story: the canvas keys its boxes by
/// name and requires them to be unique, so the second copy of one table has to
/// arrive under a different one, and the statement then has two names to write
/// `ON` between.
pub fn unique_alias(name: &str, taken: &[String]) -> String {
    if !taken.iter().any(|used| used == name) {
        return name.to_string();
    }
    (2..)
        .map(|n| format!("{name}_{n}"))
        .find(|candidate| !taken.iter().any(|used| used == candidate))
        // Unreachable: `taken` is finite, so some suffix is free. Spelled as a
        // fallback rather than an `expect` to keep the function total.
        .unwrap_or_else(|| name.to_string())
}

/// One table's name parts, most significant first and with the absent ones
/// dropped.
///
/// The schema wins when there is one, the catalogue stands in for it when there
/// is not — which is MySQL, where the catalogue *is* the database — and a
/// product with neither gives one bare part. Naming *one* qualification rule is
/// the point: [`table_ref`] writes the `SELECT`'s table and the data pane's
/// apply hands the same parts to `rudbman_sql::plan_edits`, and a statement that
/// read a row from one table and wrote it back to another would be the worst bug
/// this pane could have.
pub fn table_parts(catalog: Option<&str>, schema: Option<&str>, name: &str) -> Vec<String> {
    fn present(part: Option<&str>) -> Option<&str> {
        part.filter(|part| !part.is_empty())
    }
    let qualifier = present(schema).or_else(|| present(catalog));
    qualifier
        .into_iter()
        .chain(std::iter::once(name))
        .map(str::to_string)
        .collect()
}

/// One table's qualified name, quoted as the dialect requires.
///
/// [`table_parts`] decides which parts there are; this only spells them. Shared
/// with the explorer's "query this object", so the two agree on how a name is
/// written.
pub fn table_ref(
    dialect: &Dialect,
    catalog: Option<&str>,
    schema: Option<&str>,
    name: &str,
) -> String {
    dialect.qualify(
        table_parts(catalog, schema, name)
            .iter()
            .map(String::as_str),
    )
}

/// The `SELECT` the builder's state describes.
///
/// Laid out over several lines with the continuations indented two spaces, and
/// with no trailing semicolon: it goes into a SQL editor that splits statements
/// itself, and a terminator the user did not type is one they would have to
/// delete.
///
/// A query with no tables in it has no statement, and comes out empty rather
/// than as a `SELECT *` over nothing.
pub fn generate(query: &BuilderQuery, dialect: &Dialect) -> String {
    let Some(first) = query.tables.first() else {
        return String::new();
    };

    let column = |(table, column): (usize, usize)| -> Option<String> {
        let table = query.tables.get(table)?;
        let column = table.columns.get(column)?;
        Some(dialect.qualify([table.alias.as_str(), column.as_str()]))
    };

    let selected: Vec<String> = query.selected.iter().filter_map(|at| column(*at)).collect();
    let mut lines = vec![if selected.is_empty() {
        // Nothing picked yet is a statement that shows the table rather than
        // one that shows nothing: the builder is used by adding a table and
        // looking at it.
        "SELECT *".to_string()
    } else {
        format!("SELECT {}", selected.join(", "))
    }];

    lines.push(format!("FROM {}", table_expr(first, dialect)));
    for index in 1..query.tables.len() {
        let table = &query.tables[index];
        // Every edge that reaches back to a table already in the statement, so
        // that a composite key drawn as two edges becomes one `ON` with an
        // `AND` in it rather than two joins of the same table.
        let reaching: Vec<&Join> = query
            .joins
            .iter()
            .filter(|join| {
                (join.from.0 == index && join.to.0 < index)
                    || (join.to.0 == index && join.from.0 < index)
            })
            .collect();
        let terms: Vec<String> = reaching
            .iter()
            .filter_map(|join| {
                // The table already in the statement goes on the left of the
                // `=`, whichever end of the edge it happens to be: the join
                // reads as "attach this to what is there".
                let (earlier, added) = if join.from.0 == index {
                    (join.to, join.from)
                } else {
                    (join.from, join.to)
                };
                Some(format!("{} = {}", column(earlier)?, column(added)?))
            })
            .collect();

        match (reaching.first(), terms.is_empty()) {
            (Some(join), false) => lines.push(format!(
                "  {} JOIN {} ON {}",
                join.kind.keyword(),
                table_expr(table, dialect),
                terms.join(" AND ")
            )),
            // Nothing joins it to what came before, so it is a cross join —
            // written as a comma, which is what the user will recognise as the
            // thing they have not finished drawing yet.
            _ => {
                if let Some(previous) = lines.last_mut() {
                    previous.push(',');
                }
                lines.push(format!("  {}", table_expr(table, dialect)));
            }
        }
    }

    let conditions: Vec<&str> = query
        .where_clauses
        .iter()
        .map(|clause| clause.trim())
        .filter(|clause| !clause.is_empty())
        .collect();
    if let Some((first, rest)) = conditions.split_first() {
        lines.push(format!("WHERE {first}"));
        lines.extend(rest.iter().map(|clause| format!("  AND {clause}")));
    }

    let grouped: Vec<String> = query.group_by.iter().filter_map(|at| column(*at)).collect();
    if !grouped.is_empty() {
        lines.push(format!("GROUP BY {}", grouped.join(", ")));
    }

    let ordered: Vec<String> = query
        .order_by
        .iter()
        .filter_map(|(at, direction)| Some(format!("{} {}", column(*at)?, direction.keyword())))
        .collect();
    if !ordered.is_empty() {
        lines.push(format!("ORDER BY {}", ordered.join(", ")));
    }

    lines.join("\n")
}

/// One table as the `FROM` writes it: its qualified name, and its alias when
/// that is not the name itself.
fn table_expr(table: &BuilderTable, dialect: &Dialect) -> String {
    let name = table_ref(
        dialect,
        table.catalog.as_deref(),
        table.schema.as_deref(),
        &table.name,
    );
    if table.alias == table.name {
        return name;
    }
    format!("{name} {}", dialect.quote_ident(&table.alias))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A table of `columns` in the `APP` schema.
    fn table(name: &str, alias: &str, columns: &[&str]) -> BuilderTable {
        BuilderTable {
            catalog: None,
            schema: Some("APP".to_string()),
            name: name.to_string(),
            alias: alias.to_string(),
            columns: columns.iter().map(|column| (*column).to_string()).collect(),
        }
    }

    /// The three tables the milestone's own acceptance test joins.
    fn three() -> Vec<BuilderTable> {
        vec![
            table("PERSON", "PERSON", &["ID", "TEAM_ID", "NAME"]),
            table("TEAM", "TEAM", &["ID", "NAME", "OFFICE_ID"]),
            table("OFFICE", "OFFICE", &["ID", "CITY"]),
        ]
    }

    fn join(from: (usize, usize), to: (usize, usize), kind: JoinKind) -> Join {
        Join { from, to, kind }
    }

    #[test]
    fn three_tables_two_joins_and_every_clause() {
        let query = BuilderQuery {
            tables: three(),
            // Drawn the other way round on purpose: the edge says which columns
            // match, not which side of the `ON` they go on.
            joins: vec![
                join((0, 1), (1, 0), JoinKind::Inner),
                join((2, 0), (1, 2), JoinKind::Left),
            ],
            selected: vec![(0, 2), (1, 1), (2, 1)],
            where_clauses: vec![
                "PERSON.NAME LIKE 'A%'".to_string(),
                "  ".to_string(),
                "OFFICE.CITY <> 'Seoul'".to_string(),
            ],
            group_by: vec![(1, 1)],
            order_by: vec![((2, 1), SortDir::Desc), ((0, 2), SortDir::Asc)],
        };

        assert_eq!(
            generate(&query, &Dialect::H2),
            "SELECT PERSON.NAME, TEAM.NAME, OFFICE.CITY\n\
             FROM APP.PERSON\n\
             \x20 INNER JOIN APP.TEAM ON PERSON.TEAM_ID = TEAM.ID\n\
             \x20 LEFT JOIN APP.OFFICE ON TEAM.OFFICE_ID = OFFICE.ID\n\
             WHERE PERSON.NAME LIKE 'A%'\n\
             \x20 AND OFFICE.CITY <> 'Seoul'\n\
             GROUP BY TEAM.NAME\n\
             ORDER BY OFFICE.CITY DESC, PERSON.NAME ASC"
        );
    }

    /// Two edges between the same pair are one join with an `AND` in it, which
    /// is what a composite foreign key draws as.
    #[test]
    fn a_composite_key_is_one_join_with_two_terms() {
        let query = BuilderQuery {
            tables: vec![
                table("PARENT", "PARENT", &["A", "B"]),
                table("CHILD", "CHILD", &["A_ID", "B_ID"]),
            ],
            joins: vec![
                join((0, 0), (1, 0), JoinKind::Full),
                join((0, 1), (1, 1), JoinKind::Full),
            ],
            ..BuilderQuery::default()
        };

        assert_eq!(
            generate(&query, &Dialect::H2),
            "SELECT *\n\
             FROM APP.PARENT\n\
             \x20 FULL JOIN APP.CHILD ON PARENT.A = CHILD.A_ID AND PARENT.B = CHILD.B_ID"
        );
    }

    /// Nothing picked is `*`, and a table nothing joins to is a comma.
    #[test]
    fn an_unjoined_table_is_a_comma_and_no_selection_is_a_star() {
        let query = BuilderQuery {
            tables: vec![
                table("PERSON", "PERSON", &["ID"]),
                table("TEAM", "TEAM", &["ID"]),
                table("OFFICE", "OFFICE", &["ID"]),
            ],
            joins: vec![join((1, 0), (2, 0), JoinKind::Inner)],
            ..BuilderQuery::default()
        };

        assert_eq!(
            generate(&query, &Dialect::H2),
            "SELECT *\n\
             FROM APP.PERSON,\n\
             \x20 APP.TEAM\n\
             \x20 INNER JOIN APP.OFFICE ON TEAM.ID = OFFICE.ID"
        );
    }

    /// The same table twice, which is the only way a self-join is expressible.
    #[test]
    fn a_self_join_writes_the_alias_out() {
        let taken = vec!["PERSON".to_string()];
        assert_eq!(unique_alias("PERSON", &[]), "PERSON");
        assert_eq!(unique_alias("PERSON", &taken), "PERSON_2");
        assert_eq!(
            unique_alias("PERSON", &[taken[0].clone(), "PERSON_2".to_string()]),
            "PERSON_3"
        );

        let query = BuilderQuery {
            tables: vec![
                table("PERSON", "PERSON", &["ID", "MANAGER_ID"]),
                table("PERSON", "PERSON_2", &["ID", "MANAGER_ID"]),
            ],
            joins: vec![join((0, 1), (1, 0), JoinKind::Left)],
            selected: vec![(0, 0), (1, 0)],
            ..BuilderQuery::default()
        };

        assert_eq!(
            generate(&query, &Dialect::H2),
            "SELECT PERSON.ID, PERSON_2.ID\n\
             FROM APP.PERSON\n\
             \x20 LEFT JOIN APP.PERSON PERSON_2 ON PERSON.MANAGER_ID = PERSON_2.ID"
        );
    }

    /// A name that would not survive being written bare is quoted, and one that
    /// would is not.
    #[test]
    fn names_are_quoted_only_where_they_have_to_be() {
        let query = BuilderQuery {
            tables: vec![
                BuilderTable {
                    catalog: None,
                    schema: Some("dbo".to_string()),
                    name: "Order Details".to_string(),
                    alias: "Order Details".to_string(),
                    columns: vec!["select".to_string(), "Quantity".to_string()],
                },
                BuilderTable {
                    catalog: Some("shop".to_string()),
                    schema: None,
                    name: "orders".to_string(),
                    alias: "o".to_string(),
                    columns: vec!["id".to_string()],
                },
            ],
            joins: vec![join((0, 0), (1, 0), JoinKind::Inner)],
            selected: vec![(0, 1)],
            ..BuilderQuery::default()
        };

        // MySQL: back ticks, case preserved, and the catalogue standing in for
        // the schema on the second table.
        assert_eq!(
            generate(&query, &Dialect::MYSQL),
            "SELECT `Order Details`.Quantity\n\
             FROM dbo.`Order Details`\n\
             \x20 INNER JOIN shop.orders o ON `Order Details`.`select` = o.id"
        );

        // PostgreSQL folds down, so every capital has to be quoted — and the
        // quote character is the standard one.
        assert_eq!(
            generate(&query, &Dialect::POSTGRES),
            "SELECT \"Order Details\".\"Quantity\"\n\
             FROM dbo.\"Order Details\"\n\
             \x20 INNER JOIN shop.orders o ON \"Order Details\".\"select\" = o.id"
        );
    }

    /// Oracle refuses `AS` before a table alias, so nothing here writes one.
    #[test]
    fn a_table_alias_is_never_introduced_with_as() {
        let query = BuilderQuery {
            tables: vec![
                BuilderTable {
                    catalog: None,
                    schema: Some("APP".to_string()),
                    name: "PERSON".to_string(),
                    alias: "P".to_string(),
                    columns: vec!["ID".to_string()],
                },
                BuilderTable {
                    catalog: None,
                    schema: Some("APP".to_string()),
                    name: "TEAM".to_string(),
                    alias: "T".to_string(),
                    columns: vec!["ID".to_string()],
                },
            ],
            joins: vec![join((0, 0), (1, 0), JoinKind::Right)],
            ..BuilderQuery::default()
        };

        let sql = generate(&query, &Dialect::ORACLE);
        assert_eq!(
            sql,
            "SELECT *\n\
             FROM APP.PERSON P\n\
             \x20 RIGHT JOIN APP.TEAM T ON P.ID = T.ID"
        );
        assert!(!sql.contains(" AS "), "{sql}");
        assert!(!sql.ends_with(';'), "{sql}");
    }

    /// The rule the explorer's "query this object" now shares.
    #[test]
    fn a_table_reference_prefers_the_schema_and_falls_back_to_the_catalogue() {
        let h2 = Dialect::H2;
        assert_eq!(table_ref(&h2, None, Some("APP"), "PERSON"), "APP.PERSON");
        assert_eq!(
            table_ref(&h2, Some("SHOP"), Some("APP"), "PERSON"),
            "APP.PERSON"
        );
        assert_eq!(table_ref(&h2, Some("SHOP"), None, "PERSON"), "SHOP.PERSON");
        assert_eq!(table_ref(&h2, None, None, "PERSON"), "PERSON");
        // Empty is absent, not a part to be quoted into a leading dot.
        assert_eq!(table_ref(&h2, Some(""), Some(""), "PERSON"), "PERSON");
        // And an ordinary name is left exactly as it was, which is what keeps
        // the output of "query this object" unchanged for the common case.
        assert_eq!(
            table_ref(&Dialect::MYSQL, None, Some("shop"), "orders"),
            "shop.orders"
        );
    }

    #[test]
    fn a_query_with_no_tables_has_no_statement() {
        assert_eq!(generate(&BuilderQuery::default(), &Dialect::H2), "");
    }
}
