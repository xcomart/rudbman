//! What the structure pane is holding that the server has not seen yet.
//!
//! [`StructEdits`] is to §7.10's `ALTER TABLE` what [`crate::data_edit`]'s
//! `EditSet` is to §7.9's `UPDATE`: a staging buffer kept *beside* what was
//! read rather than written into it, and free of gpui, of the session and of
//! the JVM for the same reason. Everything decided here — whether a field the
//! user typed into still differs from the catalog, what a drop and a change on
//! one column add up to, which of them is refused and by what name — is
//! decidable from a [`Structure`] and a [`Dialect`] alone, and a test that
//! needed a database would be a test nobody runs.
//!
//! # The snapshot and the staging beside it
//!
//! A [`Structure`] is one reading of the catalog: the table's name parts, its
//! columns in the order `DESCRIBE columns` gave them, and the constraints
//! `primary_keys` and `imported_keys` reported. It is never modified. Every
//! edit is recorded against it by *index* — a [`BTreeMap`] of changed columns,
//! a [`BTreeSet`] of dropped ones — which is what lets a column be identified
//! while its own name is the thing being retyped, and what makes the plan come
//! out in column order without anything having to sort it.
//!
//! Those indices are only as stable as the snapshot, and that is enough:
//! nothing pages a structure in, and §7.10's rule that a successful apply
//! *reloads* rather than patches means the pane throws the whole pair away and
//! reads again. [`StructEdits::clear`] is what the discard button and that
//! reload both reach for.
//!
//! Added columns are a list of their own, because they have no snapshot row to
//! be indexed against: they are keyed by their position in that list, which is
//! also what a refusal points at when one of them has no name to be pointed at
//! by.
//!
//! # A change equal to the snapshot is not a change
//!
//! The pane stages a draft the moment a field is touched — that is what makes
//! the field editable at all — so a user who types over a type and types it
//! back leaves a draft that differs from the catalog in nothing.
//! [`StructEdits::plan`] drops those before they reach the generator, which is
//! why [`DdlError::NoChange`] cannot come out of this module: the one shape
//! that provokes it is filtered a line earlier.
//!
//! The consequence for the pane is worth stating plainly.
//! [`StructEdits::is_column_changed`] answers "is a draft staged against this
//! row", which is what marks the row the user has been typing in; whether that
//! draft *says* anything is a question the plan answers, and only it has the
//! snapshot in hand to answer it with.
//!
//! # A drop is the later intent
//!
//! Marking a column dropped discards whatever was staged against it. The two
//! cannot both be meant — a column that is going does not need a new type — and
//! of the two the drop is the one the user did last. This is the opposite of
//! `EditSet::toggle_deleted`, which keeps a deleted row's edits so that
//! un-deleting gives them back, and the difference is that a deleted *row* is
//! still drawn with its values in it while a dropped *column* has no cell left
//! to show them in.
//!
//! # A refusal names the thing it refuses
//!
//! Four shapes are refused here rather than passed on: an added column with no
//! name, an added column with no type, an existing column whose name has been
//! emptied, and a rename to the empty string. Each is a [`PlanError`] carrying
//! enough to point at the row it came from — a position for an added column,
//! the catalog's own name for an existing one — for §7.9's reason: by the time
//! a batch has been planned a column is a word inside a string, and "the second
//! statement" is not something to show anybody.
//!
//! What is *not* checked here is anything the generator judges better. A type
//! left empty on an existing column is only a problem on the dialects that
//! restate a whole definition, and [`DdlError::NoTypeSql`] knows which those
//! are; a dialect that cannot express a change at all is
//! [`DdlError::Unsupported`], which already names the product and the reason.
//! Both arrive wrapped in [`PlanError::Ddl`] and are shown as they stand.
//!
//! # Order
//!
//! [`TableAlter`]'s fields are filled in and the generator's own order comes
//! out: constraint drops, column adds, column changes, column drops, the table
//! rename. Within the changes and the drops it is column order, because the
//! staged collections are ordered by the index they are keyed with.
//!
//! # Check constraints
//!
//! In practice a [`Structure`] never holds a [`ConstraintKind::Check`]: it is
//! built from JDBC's `DatabaseMetaData`, which reports primary keys, foreign
//! keys and indexes and has no call for check constraints at all. Nothing here
//! special-cases that. The generator spells all four kinds, and a structure
//! read from native DDL rather than from the metadata API could supply one
//! later; a module that refused the kind it happens not to receive today would
//! only have to be edited then.
//!
//! Nothing here sends anything.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use rudbman_sql::{
    ColumnChange, ColumnDef, ConstraintDrop, ConstraintKind, DdlError, Dialect, TableAlter,
    plan_alter,
};

/// One column as the catalog reported it.
///
/// The four fields the editor offers, and no more: a column's size and scale
/// are folded into [`LoadedColumn::type_sql`] by whoever read it, because that
/// is the form the user retypes and the only form the generator passes on.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct LoadedColumn {
    /// The column's name, unquoted, as the catalog spells it.
    pub name: String,
    /// The type, as SQL — `varchar(80)` rather than a type and a length.
    pub type_sql: String,
    /// Whether the column refuses NULL.
    pub not_null: bool,
    /// The default expression, as SQL. `None` is "no default".
    pub default_sql: Option<String>,
}

/// One constraint the catalog reported, and can therefore be dropped.
///
/// The kind travels with the name because MySQL has no generic
/// `DROP CONSTRAINT`; see [`ConstraintKind`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoadedConstraint {
    /// What kind of constraint it is.
    pub kind: ConstraintKind,
    /// Its name, as the catalog spells it.
    pub name: String,
    /// The columns it covers, in key order.
    ///
    /// Nothing in a plan reads this — a drop names the constraint and not its
    /// columns — but the pane lists it, and it is the whole of why a user can
    /// tell two foreign keys apart.
    pub columns: Vec<String>,
}

/// A table's structure as it was read: the snapshot every edit is staged
/// against.
///
/// Never modified. A successful apply throws it away and reads again (§7.10),
/// because what a server did with a DDL statement is not always what it was
/// asked.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Structure {
    /// The table's name parts, most significant first: catalog, schema, name.
    ///
    /// Already filtered of the parts the driver reported as absent by whoever
    /// built this. Nothing here filters them again: an empty part that reached
    /// this far is [`DdlError::NoTable`], which is the generator's to report.
    pub table: Vec<String>,
    /// The columns, in the order the catalog gave them.
    pub columns: Vec<LoadedColumn>,
    /// The constraints that can be dropped.
    pub constraints: Vec<LoadedConstraint>,
}

/// One column's editable text, staged beside the snapshot rather than written
/// into it.
///
/// The same four fields [`LoadedColumn`] has, because it starts as a copy of
/// one — or, for a column being added, as four empty fields the user fills in.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ColumnDraft {
    /// The name, as the field holds it.
    pub name: String,
    /// The type, as SQL, exactly as the user wrote it. Never parsed (§7.10).
    pub type_sql: String,
    /// Whether the column should refuse NULL.
    pub not_null: bool,
    /// The default expression, as SQL. `None` is "no default".
    pub default_sql: Option<String>,
}

impl From<&LoadedColumn> for ColumnDraft {
    fn from(column: &LoadedColumn) -> Self {
        ColumnDraft {
            name: column.name.clone(),
            type_sql: column.type_sql.clone(),
            not_null: column.not_null,
            default_sql: column.default_sql.clone(),
        }
    }
}

/// Which field of a draft an edit lands on.
///
/// The pane's four editors per row, named. A [`DraftValue`] answers with the
/// one it carries, which is how a caller that took the value from a widget can
/// still say which widget it came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ColumnField {
    /// [`ColumnDraft::name`].
    Name,
    /// [`ColumnDraft::type_sql`].
    Type,
    /// [`ColumnDraft::not_null`].
    NotNull,
    /// [`ColumnDraft::default_sql`].
    Default,
}

/// One field of a draft, and what to put in it.
///
/// The field and the value travel together rather than as two arguments, and
/// the reason is [`DraftValue::Default`]. "No default" and "a default of the
/// empty string" are two states the server distinguishes and a setter taking a
/// `&str` per field could not: a caller that meant to drop a default would be
/// spelling it the same way as one that meant to set it to `''`. Carrying an
/// `Option` in the one variant that needs one makes the two unconfusable at
/// the call site, and costs the other three nothing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DraftValue {
    /// A new name for the column.
    Name(String),
    /// A new type, as SQL.
    Type(String),
    /// Whether the column should refuse NULL.
    NotNull(bool),
    /// A default expression, or `None` for no default at all.
    Default(Option<String>),
}

impl DraftValue {
    /// Which field this lands on.
    pub fn field(&self) -> ColumnField {
        match self {
            DraftValue::Name(_) => ColumnField::Name,
            DraftValue::Type(_) => ColumnField::Type,
            DraftValue::NotNull(_) => ColumnField::NotNull,
            DraftValue::Default(_) => ColumnField::Default,
        }
    }

    /// Writes this into `draft`, leaving its other three fields alone.
    fn apply(self, draft: &mut ColumnDraft) {
        match self {
            DraftValue::Name(name) => draft.name = name,
            DraftValue::Type(type_sql) => draft.type_sql = type_sql,
            DraftValue::NotNull(not_null) => draft.not_null = not_null,
            DraftValue::Default(default_sql) => draft.default_sql = default_sql,
        }
    }
}

/// Everything staged against one table's structure, and nothing else.
///
/// Deliberately ignorant of the structure itself: it holds indices, drafts and
/// one optional name, and the snapshot they mean anything against is passed in
/// by the caller that has it. That is what keeps this testable in a dozen lines
/// per case, and it is why the same [`StructEdits`] cannot be read against a
/// structure it was not staged against — reloading the catalog is a
/// [`StructEdits::clear`] as well.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct StructEdits {
    /// Drafts by index into [`Structure::columns`].
    ///
    /// A [`BTreeMap`] rather than a hash map so that the statements come out in
    /// column order and in the same order twice: a preview the user checked has
    /// to be the batch that runs.
    changed: BTreeMap<usize, ColumnDraft>,
    /// Indices of the columns staged to be dropped. Disjoint from `changed` by
    /// construction — see [`StructEdits::toggle_column_drop`].
    dropped: BTreeSet<usize>,
    /// The columns being added, in the order they were added.
    added: Vec<ColumnDraft>,
    /// Indices into [`Structure::constraints`] of the constraints to drop.
    dropped_constraints: BTreeSet<usize>,
    /// The table's new bare name, if it is being renamed.
    rename_to: Option<String>,
}

impl StructEdits {
    /// Nothing staged.
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether an apply would send nothing.
    ///
    /// Answered from what is staged, without the snapshot, so a draft that was
    /// typed over and typed back still counts here while planning to nothing.
    /// The pane's Apply button being enabled for a batch that turns out empty
    /// is the harmless direction of that, and the alternative — a button that
    /// greys itself out while the user is still in the field — is not.
    pub fn is_empty(&self) -> bool {
        self.changed.is_empty()
            && self.dropped.is_empty()
            && self.added.is_empty()
            && self.dropped_constraints.is_empty()
            && self.rename_to.is_none()
    }

    /// How many things are staged, for the pane's "N pending changes" line.
    ///
    /// One per changed column, added column, dropped column and dropped
    /// constraint, plus one for the rename. Not one per statement: a change of
    /// three attributes is one row the user edited and three statements on
    /// PostgreSQL, and the line is about the former. Counted the way
    /// [`StructEdits::is_empty`] is, and with the same caveat.
    pub fn pending_count(&self) -> usize {
        self.changed.len()
            + self.dropped.len()
            + self.added.len()
            + self.dropped_constraints.len()
            + usize::from(self.rename_to.is_some())
    }

    /// Discards everything staged.
    ///
    /// The discard button, and also what a reload runs: a snapshot read again
    /// is a new set of indices, and edits carried across would be pointing at
    /// rows that have moved.
    pub fn clear(&mut self) {
        self.changed.clear();
        self.dropped.clear();
        self.added.clear();
        self.dropped_constraints.clear();
        self.rename_to = None;
    }

    /// The column's effective current value: what the pane renders.
    ///
    /// The staged draft if there is one, and the snapshot's own values if there
    /// is not, so the fields of an untouched row are pre-filled from the
    /// catalog without anything having been staged to fill them. An index the
    /// structure does not have answers an empty draft rather than panicking —
    /// the pane can ask about a row between a layout and a paint.
    pub fn draft(&self, structure: &Structure, index: usize) -> ColumnDraft {
        if let Some(draft) = self.changed.get(&index) {
            return draft.clone();
        }
        structure
            .columns
            .get(index)
            .map(ColumnDraft::from)
            .unwrap_or_default()
    }

    /// Stages an edit to an existing column.
    ///
    /// On the first touch the draft is taken from the snapshot and then written
    /// into, so a row whose type was retyped still carries its own name and
    /// nullability — which the restating dialects need and would otherwise
    /// silently drop (§7.10).
    ///
    /// Nothing is staged for an index the structure does not have: an edit that
    /// could never reach a statement is not worth keeping, and dropping it here
    /// keeps [`StructEdits::plan`] from having to explain a row that is not
    /// there.
    pub fn set_column(&mut self, structure: &Structure, index: usize, value: DraftValue) {
        if index >= structure.columns.len() {
            return;
        }
        let mut draft = self.draft(structure, index);
        value.apply(&mut draft);
        self.changed.insert(index, draft);
    }

    /// Marks a column to be dropped, or takes the mark off, and says which it
    /// did.
    ///
    /// Marking one **discards whatever was staged against it**: the two cannot
    /// both be meant, and the drop is the later intent. Unmarking does not give
    /// the draft back, for the same reason — there is nothing left that said it
    /// was wanted.
    pub fn toggle_column_drop(&mut self, index: usize) -> bool {
        if self.dropped.remove(&index) {
            return false;
        }
        self.changed.remove(&index);
        self.dropped.insert(index);
        true
    }

    /// Appends an empty column and answers its position in the added list.
    ///
    /// Empty and not pre-filled with a plausible type: a column added with a
    /// type nobody chose is a column added wrong, and the refusal an empty one
    /// earns names the row it came from.
    pub fn add_column(&mut self) -> usize {
        self.added.push(ColumnDraft::default());
        self.added.len() - 1
    }

    /// Takes an added column back off the list, and says whether there was one.
    ///
    /// The ones after it move up, which is why nothing outside holds a position
    /// across this.
    pub fn remove_added(&mut self, position: usize) -> bool {
        if position >= self.added.len() {
            return false;
        }
        self.added.remove(position);
        true
    }

    /// Stages an edit to an added column.
    ///
    /// No snapshot to start from, so nothing is copied and every field is the
    /// user's from the beginning. A position past the end is ignored, as in
    /// [`StructEdits::set_column`].
    pub fn set_added(&mut self, position: usize, value: DraftValue) {
        if let Some(draft) = self.added.get_mut(position) {
            value.apply(draft);
        }
    }

    /// Marks a constraint to be dropped, or takes the mark off, and says which
    /// it did.
    pub fn toggle_constraint_drop(&mut self, index: usize) -> bool {
        if self.dropped_constraints.remove(&index) {
            return false;
        }
        self.dropped_constraints.insert(index);
        true
    }

    /// Sets the table's new **bare** name, or `None` to leave it alone.
    ///
    /// Bare because that is what every product takes: a rename moves a table
    /// within its schema, and none of them accepts a qualified target.
    pub fn set_rename(&mut self, rename_to: Option<String>) {
        self.rename_to = rename_to;
    }

    /// Whether a column is staged to be dropped.
    pub fn is_column_dropped(&self, index: usize) -> bool {
        self.dropped.contains(&index)
    }

    /// Whether a draft is staged against a column.
    ///
    /// What marks the row the user has typed in. A draft that walked back to
    /// the snapshot still reads as changed here and still plans to nothing; see
    /// the [module documentation](self).
    pub fn is_column_changed(&self, index: usize) -> bool {
        self.changed.contains_key(&index)
    }

    /// Whether a constraint is staged to be dropped.
    pub fn is_constraint_dropped(&self, index: usize) -> bool {
        self.dropped_constraints.contains(&index)
    }

    /// The columns being added, in the order the pane draws them.
    pub fn added(&self) -> &[ColumnDraft] {
        &self.added
    }

    /// The table's new name, if it is being renamed.
    pub fn rename_to(&self) -> Option<&str> {
        self.rename_to.as_deref()
    }

    /// Turns everything staged against `structure` into the statements that
    /// apply it.
    ///
    /// The four refusals this makes on its own are the ones that can name a row
    /// the user can go and look at; everything else is the generator's
    /// judgement, wrapped in [`PlanError::Ddl`] and shown as it stands. See the
    /// [module documentation](self) for both, and for why a change equal to the
    /// snapshot is dropped rather than refused.
    pub fn plan(&self, structure: &Structure, dialect: &Dialect) -> Result<Vec<String>, PlanError> {
        let mut alter = TableAlter::new(structure.table.iter().cloned());

        // A constraint naming a column blocks that column's drop, so these go
        // first — which is `plan_alter`'s own order, and all this has to do is
        // fill the fields in.
        for index in &self.dropped_constraints {
            let Some(constraint) = structure.constraints.get(*index) else {
                continue;
            };
            alter.drop_constraints.push(ConstraintDrop {
                kind: constraint.kind,
                name: constraint.name.clone(),
            });
        }

        for (position, draft) in self.added.iter().enumerate() {
            if draft.name.is_empty() {
                return Err(PlanError::AddedHasNoName { position });
            }
            if draft.type_sql.is_empty() {
                return Err(PlanError::AddedHasNoType { position });
            }
            alter.adds.push(draft.def());
        }

        for (index, draft) in &self.changed {
            // A column that is going does not also get a new type. The two are
            // disjoint as `toggle_column_drop` keeps them, and this is the
            // second half of that promise rather than a second rule.
            if self.dropped.contains(index) {
                continue;
            }
            let Some(column) = structure.columns.get(*index) else {
                continue;
            };
            if draft.name.is_empty() {
                return Err(PlanError::ColumnHasNoName {
                    column: column.name.clone(),
                });
            }
            let change = ColumnChange {
                from: column.def(),
                to: draft.def(),
            };
            // Typed over and typed back. Dropping it here is what keeps
            // `DdlError::NoChange` out of reach.
            if change.from == change.to {
                continue;
            }
            alter.changes.push(change);
        }

        for index in &self.dropped {
            let Some(column) = structure.columns.get(*index) else {
                continue;
            };
            alter.drops.push(column.name.clone());
        }

        if let Some(new_name) = &self.rename_to {
            if new_name.is_empty() {
                return Err(PlanError::NoNewName);
            }
            // A table renamed to the name it already has is not a rename, for
            // the reason a column retyped to its own type is not a change: the
            // field was pre-filled from the catalog, and leaving it alone is
            // the commonest thing to do with it.
            if structure.table.last() != Some(new_name) {
                alter.rename_to = Some(new_name.clone());
            }
        }

        // Every identifier from here on is `rudbman-sql`'s to spell; none is
        // written above.
        plan_alter(&alter, dialect).map_err(PlanError::Ddl)
    }
}

impl LoadedColumn {
    /// This column as the generator's own record of one.
    fn def(&self) -> ColumnDef {
        ColumnDef {
            name: self.name.clone(),
            type_sql: self.type_sql.clone(),
            not_null: self.not_null,
            default_sql: self.default_sql.clone(),
        }
    }
}

impl ColumnDraft {
    /// This draft as the generator's own record of a column.
    fn def(&self) -> ColumnDef {
        ColumnDef {
            name: self.name.clone(),
            type_sql: self.type_sql.clone(),
            not_null: self.not_null,
            default_sql: self.default_sql.clone(),
        }
    }
}

/// Why a staged structure could not be turned into statements.
///
/// The first four are about one row the user can go and look at, and each
/// carries what points at it. The fifth is `rudbman-sql`'s own refusal, kept
/// whole: [`DdlError::Unsupported`] already names the product and the reason it
/// cannot do this, which is more than anything written here would have said.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlanError {
    /// A column being added has no name.
    AddedHasNoName {
        /// Its position in the added list, which is the row to point at — it
        /// has no name to be pointed at by.
        position: usize,
    },
    /// A column being added has no type, and a column cannot be added without
    /// one on any product.
    AddedHasNoType {
        /// Its position in the added list.
        position: usize,
    },
    /// An existing column's name has been emptied.
    ColumnHasNoName {
        /// The name the catalog gave it, which is what the row still reads as
        /// everywhere but the field that was cleared.
        column: String,
    },
    /// The table's new name is the empty string.
    NoNewName,
    /// `rudbman-sql` refused the alter.
    Ddl(DdlError),
}

impl fmt::Display for PlanError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            // One-based, because the number is read by a person counting rows
            // down a pane and not by anything that indexes with it.
            PlanError::AddedHasNoName { position } => {
                write!(f, "added column #{} has no name", position + 1)
            }
            PlanError::AddedHasNoType { position } => write!(
                f,
                "added column #{} has no type, and a column cannot be added without one",
                position + 1
            ),
            PlanError::ColumnHasNoName { column } => {
                write!(f, "column `{column}` has been left without a name")
            }
            PlanError::NoNewName => f.write_str("the table's new name is empty"),
            PlanError::Ddl(error) => error.fmt(f),
        }
    }
}

impl std::error::Error for PlanError {}

#[cfg(test)]
mod tests {
    use rudbman_sql::{DialectId, Unsupported};

    use super::*;

    /// `app.orders(id, qty, note, legacy)` with a primary key and a foreign
    /// key.
    ///
    /// Every name is lower case and unreserved, so PostgreSQL quotes none of
    /// them and the expected statements below read as SQL rather than as a
    /// quoting test — which `rudbman-sql` has its own.
    fn structure() -> Structure {
        Structure {
            table: vec!["app".to_string(), "orders".to_string()],
            columns: vec![
                LoadedColumn {
                    name: "id".to_string(),
                    type_sql: "integer".to_string(),
                    not_null: true,
                    default_sql: None,
                },
                LoadedColumn {
                    name: "qty".to_string(),
                    type_sql: "integer".to_string(),
                    not_null: false,
                    default_sql: Some("0".to_string()),
                },
                LoadedColumn {
                    name: "note".to_string(),
                    type_sql: "varchar(80)".to_string(),
                    not_null: false,
                    default_sql: None,
                },
                LoadedColumn {
                    name: "legacy".to_string(),
                    type_sql: "integer".to_string(),
                    not_null: false,
                    default_sql: None,
                },
            ],
            constraints: vec![
                LoadedConstraint {
                    kind: ConstraintKind::PrimaryKey,
                    name: "orders_pk".to_string(),
                    columns: vec!["id".to_string()],
                },
                LoadedConstraint {
                    kind: ConstraintKind::ForeignKey,
                    name: "orders_note_fk".to_string(),
                    columns: vec!["note".to_string()],
                },
            ],
        }
    }

    /// The statements, or a panic carrying the refusal.
    fn sql(edits: &StructEdits, structure: &Structure) -> Vec<String> {
        edits
            .plan(structure, &Dialect::POSTGRES)
            .unwrap_or_else(|e| panic!("the edits should plan: {e}"))
    }

    /// An untouched row reads the catalog's own values, and the first edit to
    /// it keeps the three fields it did not touch.
    #[test]
    fn a_draft_starts_from_the_snapshot_and_keeps_the_rest_of_it() {
        let structure = structure();
        let mut edits = StructEdits::new();

        assert_eq!(
            edits.draft(&structure, 1),
            ColumnDraft {
                name: "qty".to_string(),
                type_sql: "integer".to_string(),
                not_null: false,
                default_sql: Some("0".to_string()),
            },
            "nothing staged reads through to the snapshot"
        );
        assert!(!edits.is_column_changed(1));

        edits.set_column(&structure, 1, DraftValue::Type("bigint".to_string()));
        assert_eq!(
            edits.draft(&structure, 1),
            ColumnDraft {
                name: "qty".to_string(),
                type_sql: "bigint".to_string(),
                not_null: false,
                default_sql: Some("0".to_string()),
            },
            "the untouched fields came from the snapshot"
        );
        assert!(edits.is_column_changed(1));
        assert!(!edits.is_column_changed(0), "and only that column");
    }

    /// NULL-the-absence and the empty string stay two things: dropping a
    /// default and setting one to `''` are different statements.
    #[test]
    fn a_default_can_be_dropped_or_set_to_nothing() {
        let structure = structure();
        let mut edits = StructEdits::new();

        edits.set_column(&structure, 1, DraftValue::Default(None));
        assert_eq!(
            sql(&edits, &structure),
            ["ALTER TABLE app.orders ALTER COLUMN qty DROP DEFAULT"]
        );

        edits.set_column(&structure, 1, DraftValue::Default(Some("''".to_string())));
        assert_eq!(
            sql(&edits, &structure),
            ["ALTER TABLE app.orders ALTER COLUMN qty SET DEFAULT ''"]
        );

        // And the field an edit lands on is answerable from the value itself.
        assert_eq!(DraftValue::Default(None).field(), ColumnField::Default);
        assert_eq!(DraftValue::Name("x".to_string()).field(), ColumnField::Name);
        assert_eq!(DraftValue::Type("x".to_string()).field(), ColumnField::Type);
        assert_eq!(DraftValue::NotNull(true).field(), ColumnField::NotNull);
    }

    /// A field typed over and typed back plans nothing, on every field there
    /// is — so `DdlError::NoChange` is unreachable from here.
    #[test]
    fn a_draft_equal_to_the_snapshot_is_not_a_change() {
        let structure = structure();
        let mut edits = StructEdits::new();

        edits.set_column(&structure, 1, DraftValue::Type("bigint".to_string()));
        edits.set_column(&structure, 1, DraftValue::Type("integer".to_string()));
        edits.set_column(&structure, 1, DraftValue::NotNull(true));
        edits.set_column(&structure, 1, DraftValue::NotNull(false));
        edits.set_column(&structure, 1, DraftValue::Default(None));
        edits.set_column(&structure, 1, DraftValue::Default(Some("0".to_string())));
        edits.set_column(&structure, 1, DraftValue::Name("quantity".to_string()));
        edits.set_column(&structure, 1, DraftValue::Name("qty".to_string()));

        assert!(
            edits.is_column_changed(1),
            "the row was typed in, and the pane marks it"
        );
        assert!(!edits.is_empty());
        assert_eq!(edits.pending_count(), 1);
        assert_eq!(
            edits.plan(&structure, &Dialect::POSTGRES),
            Ok(Vec::new()),
            "but it says nothing the catalog does not already"
        );
    }

    /// A dropped column contributes a drop and never also a change, and the
    /// mark takes the staged draft with it.
    #[test]
    fn a_dropped_column_contributes_a_drop_and_nothing_else() {
        let structure = structure();
        let mut edits = StructEdits::new();
        edits.set_column(&structure, 2, DraftValue::Type("text".to_string()));
        assert!(edits.is_column_changed(2));

        assert!(edits.toggle_column_drop(2), "marked");
        assert!(edits.is_column_dropped(2));
        assert!(!edits.is_column_changed(2), "the draft went with it");
        assert_eq!(edits.pending_count(), 1);
        assert_eq!(
            sql(&edits, &structure),
            ["ALTER TABLE app.orders DROP COLUMN note"]
        );

        // Unmarking does not give the draft back: nothing is left that said it
        // was wanted.
        assert!(!edits.toggle_column_drop(2), "unmarked");
        assert!(!edits.is_column_dropped(2));
        assert!(!edits.is_column_changed(2));
        assert!(edits.is_empty());
    }

    /// The five groups come out in the generator's order, and the changes and
    /// drops inside them in column order however they were staged.
    #[test]
    fn the_plan_is_ordered_by_group_and_then_by_column() {
        let structure = structure();
        let mut edits = StructEdits::new();
        edits.set_rename(Some("order_lines".to_string()));
        edits.toggle_column_drop(3);
        edits.toggle_column_drop(2);
        edits.set_column(&structure, 1, DraftValue::Type("bigint".to_string()));
        edits.set_column(&structure, 0, DraftValue::NotNull(false));
        let added = edits.add_column();
        edits.set_added(added, DraftValue::Name("memo".to_string()));
        edits.set_added(added, DraftValue::Type("text".to_string()));
        edits.toggle_constraint_drop(0);

        assert_eq!(
            sql(&edits, &structure),
            [
                "ALTER TABLE app.orders DROP CONSTRAINT orders_pk",
                "ALTER TABLE app.orders ADD COLUMN memo text",
                "ALTER TABLE app.orders ALTER COLUMN id DROP NOT NULL",
                "ALTER TABLE app.orders ALTER COLUMN qty SET DATA TYPE bigint",
                "ALTER TABLE app.orders DROP COLUMN note",
                "ALTER TABLE app.orders DROP COLUMN legacy",
                "ALTER TABLE app.orders RENAME TO order_lines",
            ]
        );
        assert_eq!(edits.pending_count(), 7);
    }

    /// A constraint is dropped by its kind and its name, and which of the two
    /// spellings that is remains the generator's business.
    #[test]
    fn a_dropped_constraint_carries_its_kind() {
        let structure = structure();
        let mut edits = StructEdits::new();
        assert!(edits.toggle_constraint_drop(1));
        assert!(edits.is_constraint_dropped(1));
        assert!(!edits.is_constraint_dropped(0));

        assert_eq!(
            sql(&edits, &structure),
            ["ALTER TABLE app.orders DROP CONSTRAINT orders_note_fk"]
        );
        // MySQL has no generic form, which is the whole reason the kind is
        // carried rather than looked up.
        assert_eq!(
            edits.plan(&structure, &Dialect::MYSQL),
            Ok(vec![
                "ALTER TABLE app.orders DROP FOREIGN KEY orders_note_fk".to_string()
            ])
        );

        assert!(!edits.toggle_constraint_drop(1));
        assert!(edits.is_empty());
    }

    /// An added column is the user's from the first field, and can be taken
    /// back off the list.
    #[test]
    fn an_added_column_is_typed_from_nothing() {
        let structure = structure();
        let mut edits = StructEdits::new();
        let first = edits.add_column();
        let second = edits.add_column();
        assert_eq!((first, second), (0, 1));

        edits.set_added(first, DraftValue::Name("memo".to_string()));
        edits.set_added(first, DraftValue::Type("text".to_string()));
        edits.set_added(second, DraftValue::Name("ts".to_string()));
        edits.set_added(second, DraftValue::Type("timestamp".to_string()));
        edits.set_added(second, DraftValue::NotNull(true));
        edits.set_added(second, DraftValue::Default(Some("now()".to_string())));

        assert_eq!(edits.added().len(), 2);
        assert_eq!(
            edits.added()[1],
            ColumnDraft {
                name: "ts".to_string(),
                type_sql: "timestamp".to_string(),
                not_null: true,
                default_sql: Some("now()".to_string()),
            }
        );
        assert_eq!(
            sql(&edits, &structure),
            [
                "ALTER TABLE app.orders ADD COLUMN memo text",
                "ALTER TABLE app.orders ADD COLUMN ts timestamp NOT NULL DEFAULT now()",
            ]
        );

        assert!(edits.remove_added(0));
        assert_eq!(edits.added().len(), 1);
        assert_eq!(
            sql(&edits, &structure),
            ["ALTER TABLE app.orders ADD COLUMN ts timestamp NOT NULL DEFAULT now()"]
        );
        assert!(!edits.remove_added(9), "past the end takes nothing");
    }

    /// The four refusals this module makes on its own, each naming the row it
    /// came from and each made before any SQL exists.
    #[test]
    fn a_refusal_names_the_row_it_came_from() {
        let structure = structure();

        let mut nameless = StructEdits::new();
        nameless.add_column();
        let added = nameless.add_column();
        nameless.set_added(added, DraftValue::Type("text".to_string()));
        assert_eq!(
            nameless.plan(&structure, &Dialect::POSTGRES),
            Err(PlanError::AddedHasNoName { position: 0 })
        );
        // One-based in the message, and zero-based in the value.
        assert!(
            PlanError::AddedHasNoName { position: 1 }
                .to_string()
                .contains("#2")
        );

        let mut typeless = StructEdits::new();
        let added = typeless.add_column();
        typeless.set_added(added, DraftValue::Name("memo".to_string()));
        assert_eq!(
            typeless.plan(&structure, &Dialect::POSTGRES),
            Err(PlanError::AddedHasNoType { position: 0 })
        );

        let mut emptied = StructEdits::new();
        emptied.set_column(&structure, 2, DraftValue::Name(String::new()));
        assert_eq!(
            emptied.plan(&structure, &Dialect::POSTGRES),
            Err(PlanError::ColumnHasNoName {
                column: "note".to_string()
            }),
            "named by what the catalog still calls it"
        );
        assert!(
            emptied
                .plan(&structure, &Dialect::POSTGRES)
                .unwrap_err()
                .to_string()
                .contains("note")
        );

        let mut renamed = StructEdits::new();
        renamed.set_rename(Some(String::new()));
        assert_eq!(
            renamed.plan(&structure, &Dialect::POSTGRES),
            Err(PlanError::NoNewName)
        );
    }

    /// A rename to the name the table already has is not a rename — the field
    /// is pre-filled from the catalog, and leaving it alone is the commonest
    /// thing to do with it.
    #[test]
    fn a_rename_to_the_current_name_is_not_a_rename() {
        let structure = structure();
        let mut edits = StructEdits::new();

        edits.set_rename(Some("orders".to_string()));
        assert_eq!(edits.rename_to(), Some("orders"));
        assert!(!edits.is_empty(), "the field is still filled in");
        assert_eq!(edits.plan(&structure, &Dialect::POSTGRES), Ok(Vec::new()));

        // The schema is not what a rename compares against: only the bare name.
        edits.set_rename(Some("app".to_string()));
        assert_eq!(
            sql(&edits, &structure),
            ["ALTER TABLE app.orders RENAME TO app"]
        );

        edits.set_rename(None);
        assert_eq!(edits.rename_to(), None);
        assert!(edits.is_empty());
    }

    /// What the generator refuses arrives whole, so the pane can show the line
    /// that names the product and the reason.
    #[test]
    fn the_generators_refusal_is_wrapped_and_not_swallowed() {
        let structure = structure();
        let mut edits = StructEdits::new();
        edits.set_column(&structure, 1, DraftValue::Type("bigint".to_string()));

        let error = edits
            .plan(&structure, &Dialect::SQLITE)
            .expect_err("SQLite has no ALTER for a column's type");
        assert_eq!(
            error,
            PlanError::Ddl(DdlError::Unsupported {
                dialect: DialectId::Sqlite,
                what: Unsupported::ColumnType,
                column: Some("qty".to_string()),
            })
        );
        // And `Display` delegates, rather than saying "the generator refused".
        assert_eq!(
            error.to_string(),
            DdlError::Unsupported {
                dialect: DialectId::Sqlite,
                what: Unsupported::ColumnType,
                column: Some("qty".to_string()),
            }
            .to_string()
        );
        assert!(error.to_string().contains("SQLite"));
    }

    /// A type emptied on an existing column is not one of this module's four
    /// refusals: the generator already knows which statements have to restate a
    /// type, and its answer names the column.
    #[test]
    fn an_emptied_type_is_left_to_the_generator() {
        let structure = structure();
        let mut edits = StructEdits::new();
        edits.set_column(&structure, 1, DraftValue::Type(String::new()));

        for dialect in [Dialect::POSTGRES, Dialect::MYSQL] {
            assert_eq!(
                edits.plan(&structure, &dialect),
                Err(PlanError::Ddl(DdlError::NoTypeSql {
                    column: "qty".to_string()
                })),
                "{}",
                dialect.name()
            );
        }

        // A type left alone is not a type change, so a column whose *other*
        // fields were touched plans on the dialects that restate nothing.
        let mut untyped = structure.clone();
        untyped.columns[1].type_sql = String::new();
        let mut edits = StructEdits::new();
        edits.set_column(&untyped, 1, DraftValue::NotNull(true));
        assert_eq!(
            sql(&edits, &untyped),
            ["ALTER TABLE app.orders ALTER COLUMN qty SET NOT NULL"],
            "PostgreSQL restates nothing, so it needs nothing"
        );
        assert_eq!(
            edits.plan(&untyped, &Dialect::MYSQL),
            Err(PlanError::Ddl(DdlError::NoTypeSql {
                column: "qty".to_string()
            })),
            "MySQL restates the whole definition"
        );
    }

    /// A table whose name parts did not survive the catalog is the generator's
    /// refusal too, and nothing here re-filters them.
    #[test]
    fn an_unusable_table_name_is_the_generators_to_refuse() {
        let mut structure = structure();
        structure.table = vec![String::new(), "orders".to_string()];
        let mut edits = StructEdits::new();
        edits.toggle_column_drop(3);

        assert_eq!(
            edits.plan(&structure, &Dialect::POSTGRES),
            Err(PlanError::Ddl(DdlError::NoTable))
        );
    }

    /// Nothing staged is nothing to send, and a discard puts it back there.
    #[test]
    fn an_empty_buffer_plans_nothing_and_a_discard_empties_it() {
        let structure = structure();
        let edits = StructEdits::new();
        assert!(edits.is_empty());
        assert_eq!(edits.pending_count(), 0);
        assert_eq!(edits.plan(&structure, &Dialect::POSTGRES), Ok(Vec::new()));

        let mut edits = StructEdits::new();
        edits.set_column(&structure, 0, DraftValue::Type("bigint".to_string()));
        edits.toggle_column_drop(3);
        edits.toggle_constraint_drop(0);
        edits.add_column();
        edits.set_rename(Some("order_lines".to_string()));
        assert_eq!(edits.pending_count(), 5);

        edits.clear();
        assert!(edits.is_empty());
        assert!(edits.added().is_empty());
        assert_eq!(edits.rename_to(), None);
        assert_eq!(edits.plan(&structure, &Dialect::POSTGRES), Ok(Vec::new()));
    }

    /// A row the structure does not have answers quietly rather than panicking
    /// or staging something no statement could carry.
    #[test]
    fn an_index_the_structure_does_not_have_stages_nothing() {
        let structure = structure();
        let mut edits = StructEdits::new();

        assert_eq!(edits.draft(&structure, 9), ColumnDraft::default());
        edits.set_column(&structure, 9, DraftValue::Name("x".to_string()));
        assert!(edits.is_empty(), "an edit that could never be sent");
        edits.set_added(9, DraftValue::Name("x".to_string()));
        assert!(edits.is_empty());

        // A mark against a row that is not there is kept — it is a set of
        // indices and nothing reads it against the structure until the plan —
        // and planned as nothing.
        edits.toggle_column_drop(9);
        edits.toggle_constraint_drop(9);
        assert!(!edits.is_empty());
        assert_eq!(edits.plan(&structure, &Dialect::POSTGRES), Ok(Vec::new()));
    }
}
