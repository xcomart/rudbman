//! What a diagram is made of: tables, their columns, and the foreign keys
//! between them.
//!
//! Four plain structs with public fields and no behaviour worth the name. The
//! point of the module is what it does *not* know: there is no `rudbman-jdbc`
//! here and no `Connection`, because a diagram is built by the host out of
//! `imported_keys` and column metadata it has already fetched (architecture
//! document, §7.6) and handed over whole. That boundary is why the layout tests
//! below can build a twelve-table schema in a dozen lines and why none of this
//! crate's tests need a JVM.
//!
//! Nothing here is `serde`-aware either. Only the *positions* are persisted —
//! to `erd/<profile-uuid>.json`, by the host — and the model itself is rebuilt
//! from the catalog every time the diagram is opened, because a schema that
//! changed underneath a saved copy is worse than no copy at all.

/// Which of a table's or a column's two names a box is drawn with.
///
/// A schema has two vocabularies — the identifiers the SQL is written in and
/// the sentences the catalog's comments carry — and which of them a diagram is
/// worth reading in depends on who is reading it. Nothing here decides: the
/// mode arrives from the host with every call that measures a box or cuts a
/// label, so the screen, the export and the saved arrangement cannot disagree
/// about which vocabulary is on show.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum NameMode {
    /// The identifiers — what the SQL says.
    #[default]
    Physical,
    /// The comments, falling back to the identifier wherever there is none.
    Logical,
}

/// One column of a table, as the diagram needs to draw it.
///
/// Deliberately not the full JDBC column: precision, scale and default have no
/// place on a box that has to stay readable at a quarter zoom. The remark does,
/// but only behind [`NameMode::Logical`] and only *instead of* the name — a row
/// that carried both would be two rows.
/// [`type_name`](Self::type_name) is the string the driver reported, already
/// decorated with its size by the host if it wants it decorated.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct ErdColumn {
    /// The column's name, drawn on the left of its row.
    pub name: String,
    /// The type as it should read, drawn muted on the right of its row.
    pub type_name: String,
    /// The catalog's comment on the column, when it carries one.
    ///
    /// Drawn in place of the name under [`NameMode::Logical`]. `None` and the
    /// empty string mean the same thing here — a product that answers every
    /// column with `""` rather than with SQL NULL must not turn a box into a
    /// column of blanks — so both fall back to the name.
    pub comment: Option<String>,
    /// Whether the column accepts `NULL`.
    ///
    /// Not drawn as a glyph today — the box is tight enough already — but kept
    /// on the model because a diagram that has to grow a "not null" marker
    /// should not have to grow a second fetch first.
    pub nullable: bool,
    /// Whether the column takes part in the table's primary key.
    pub primary_key: bool,
    /// Whether the column takes part in *any* foreign key of this table.
    ///
    /// Redundant with [`ErdModel::relations`], and kept anyway: drawing a row
    /// should not have to search the relation list for every column of every
    /// box, every frame.
    pub foreign_key: bool,
}

impl ErdColumn {
    /// A plain nullable column of the given name and type.
    pub fn new(name: impl Into<String>, type_name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            type_name: type_name.into(),
            comment: None,
            nullable: true,
            primary_key: false,
            foreign_key: false,
        }
    }

    /// The same column with the catalog's comment on it.
    #[must_use]
    pub fn comment(mut self, comment: impl Into<String>) -> Self {
        self.comment = Some(comment.into());
        self
    }

    /// What this column's row reads as in `mode`.
    ///
    /// The comment where there is one to show and the name everywhere else, so
    /// a schema commented halfway still draws every row.
    pub fn display_name(&self, mode: NameMode) -> &str {
        display(&self.name, self.comment.as_deref(), mode)
    }

    /// The same column marked as part of the primary key, and so not nullable.
    #[must_use]
    pub fn primary_key(mut self) -> Self {
        self.primary_key = true;
        self.nullable = false;
        self
    }

    /// The same column marked as part of a foreign key.
    #[must_use]
    pub fn foreign_key(mut self) -> Self {
        self.foreign_key = true;
        self
    }
}

/// One entity box: a name and the columns under it, in catalog order.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct ErdTable {
    /// The table's name as it is drawn and as positions are keyed by.
    ///
    /// Whether that is `orders` or `app.orders` is the host's choice; this
    /// crate only requires that it be unique within the model, because
    /// [`crate::ErdView::positions`] is keyed by it.
    pub name: String,
    /// The catalog's comment on the table, when it carries one.
    ///
    /// Drawn as the box's title under [`NameMode::Logical`]. It is *not* what
    /// positions are keyed by: [`name`](Self::name) stays the key whichever
    /// mode is showing, so toggling the names never moves a box.
    pub comment: Option<String>,
    /// The columns, in the order the catalog reported them.
    pub columns: Vec<ErdColumn>,
}

impl ErdTable {
    /// An empty table of the given name.
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            comment: None,
            columns: Vec::new(),
        }
    }

    /// The same table with the catalog's comment on it.
    #[must_use]
    pub fn comment(mut self, comment: impl Into<String>) -> Self {
        self.comment = Some(comment.into());
        self
    }

    /// The same table with `column` appended.
    #[must_use]
    pub fn column(mut self, column: ErdColumn) -> Self {
        self.columns.push(column);
        self
    }

    /// What this table's title reads as in `mode`.
    pub fn display_name(&self, mode: NameMode) -> &str {
        display(&self.name, self.comment.as_deref(), mode)
    }
}

/// The one rule both display names follow: the comment in
/// [`NameMode::Logical`], and the identifier whenever there is no comment worth
/// drawing.
fn display<'a>(name: &'a str, comment: Option<&'a str>, mode: NameMode) -> &'a str {
    match mode {
        NameMode::Physical => name,
        NameMode::Logical => comment.filter(|text| !text.is_empty()).unwrap_or(name),
    }
}

/// One foreign key, drawn as a line between two boxes.
///
/// The direction is the JDBC one and it matters for the cardinality marks: the
/// crow's foot — the "many" end — goes on [`from`](Self::from), and the single
/// bar — the "one" end — on [`to`](Self::to).
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct ErdRelation {
    /// The constraint name, when the driver reported one.
    pub name: Option<String>,
    /// Index into [`ErdModel::tables`] of the table that *holds* the foreign
    /// key.
    pub from: usize,
    /// Index into [`ErdModel::tables`] of the table whose key is referenced.
    pub to: usize,
    /// The column pairs, `(foreign key column, referenced column)`.
    ///
    /// A list rather than a pair, because composite foreign keys are ordinary
    /// in the schemas this tool is pointed at.
    pub columns: Vec<(String, String)>,
}

/// A whole diagram: the boxes and the lines between them.
///
/// Relations reference tables by index, so a model is only meaningful together
/// with its own table list — which is also what makes the layouts below plain
/// graph algorithms over `0..tables.len()` rather than name lookups.
///
/// Edges pointing out of the scope the host gathered are the host's to drop
/// (architecture document, §7.6); everything here treats an out-of-range index
/// as a relation to skip rather than as a reason to panic, because a model
/// assembled from a catalog that changed mid-fetch should draw what it can.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct ErdModel {
    /// The tables, in the order they are indexed by.
    pub tables: Vec<ErdTable>,
    /// The foreign keys between them.
    pub relations: Vec<ErdRelation>,
}

impl ErdModel {
    /// The index of the table called `name`, if the model has one.
    ///
    /// A scan rather than an index: a diagram is tens of tables, not thousands,
    /// and a map beside the vector would be one more thing to keep in step.
    pub fn index_of(&self, name: &str) -> Option<usize> {
        self.tables.iter().position(|table| table.name == name)
    }

    /// The relations whose endpoints are both inside this model.
    ///
    /// Every consumer of the relation list wants exactly this, so the bounds
    /// check lives here once rather than at each of them.
    pub fn valid_relations(&self) -> impl Iterator<Item = &ErdRelation> {
        let count = self.tables.len();
        self.relations
            .iter()
            .filter(move |relation| relation.from < count && relation.to < count)
    }
}
