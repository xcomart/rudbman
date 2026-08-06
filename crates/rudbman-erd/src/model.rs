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

/// One column of a table, as the diagram needs to draw it.
///
/// Deliberately not the full JDBC column: precision, scale, default and remarks
/// have no place on a box that has to stay readable at a quarter zoom.
/// [`type_name`](Self::type_name) is the string the driver reported, already
/// decorated with its size by the host if it wants it decorated.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct ErdColumn {
    /// The column's name, drawn on the left of its row.
    pub name: String,
    /// The type as it should read, drawn muted on the right of its row.
    pub type_name: String,
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
            nullable: true,
            primary_key: false,
            foreign_key: false,
        }
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
    /// The columns, in the order the catalog reported them.
    pub columns: Vec<ErdColumn>,
}

impl ErdTable {
    /// An empty table of the given name.
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            columns: Vec::new(),
        }
    }

    /// The same table with `column` appended.
    #[must_use]
    pub fn column(mut self, column: ErdColumn) -> Self {
        self.columns.push(column);
        self
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
