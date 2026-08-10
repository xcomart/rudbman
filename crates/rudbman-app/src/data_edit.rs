//! What the data pane is holding that the server has not seen yet.
//!
//! [`EditSet`] is the staging buffer the architecture document's §7.9 puts
//! *beside* the rows rather than inside them, and [`EditableSource`] is the
//! overlay that lays it back over them on the way to the grid. Both are free of
//! gpui, for the reason [`crate::query_source`] is: the awkward halves here —
//! which of two values a cell shows, whether an edit that walked back to where
//! it started still counts, what a delete and an edit on one row add up to —
//! are decidable with no window and no JVM, and a test that needed either would
//! be a test nobody runs.
//!
//! # Two row spaces, one index
//!
//! The source under the grid is append-only: a page adds a batch and nothing
//! already in it moves, so a base row's index is stable for as long as that
//! source is. That is the whole reason an edit can be recorded as
//! `(row, column) → value` at all, and it is also why a sort or a refresh —
//! which throw the source away — have to ask before they run.
//!
//! Rows the user inserts are a list of their own, drawn after the last fetched
//! row. Their grid indices are *not* stable: paging in another batch grows the
//! base and slides them all down. That is fine and deliberate — nothing is keyed
//! by an inserted row's grid index, only by its position in that list, and
//! [`EditSet::locate`] is the one place the two spaces are told apart.
//!
//! A deleted row keeps its slot. It goes on being drawn, struck through, until
//! the change is applied or discarded: a deletion that made the row vanish
//! would renumber every base row under it — which is exactly the thing this
//! design rests on not happening — and would leave the user nothing to change
//! their mind about.
//!
//! # What is staged is what changed
//!
//! Staging a cell back to the value it was read with un-stages it. A row edited
//! `A → B → A` is clean again, and no `UPDATE` is written for it. The
//! comparison is against the base cell's *text*, which is the same text the
//! grid seeded the field with, so the two can only agree or disagree for
//! reasons the user can see.
//!
//! # From the buffer to the statements
//!
//! [`EditSet`] is shaped to fall into [`TableEdits`] with no rearranging:
//! [`EditSet::deleted`] is one key lookup per entry away from `deletes`,
//! [`EditSet::changed`] groups by row into `updates`, and [`EditSet::inserted`]
//! is already one `Vec` per row with one cell per column, which is `inserts`
//! once [`StagedCell`] is mapped onto `InsertCell` — `Unset` to `Unset`, the
//! other two to `Set`. [`plan_apply`] is that mapping, and it ends where the
//! pane picks up: a list of [`PlannedStatement`]s, each carrying the SQL, the
//! bind parameters, the values as the confirmation shows them, and whether the
//! server has to report exactly one row for it.
//!
//! That the planning lives here rather than in the pane is the same choice the
//! rest of this module makes. It has no window and no JVM in it: everything it
//! decides — which rows become which statement, which key value goes in a
//! `WHERE`, whether what the user typed can be bound to the column's type at
//! all — is decidable from the buffer and the column metadata alone, and it is
//! the half of the apply worth testing exhaustively. What is left for the pane
//! is the part that genuinely needs a session: running the statements in order,
//! counting the rows each one reached, and rolling back.
//!
//! Nothing here sends anything.

use std::collections::{BTreeSet, HashMap};
use std::fmt;

use rudbman_grid::{GridCell, GridColumn, GridSource, GridSourceState, RowStatus};
use rudbman_jdbc::{ColumnInfo, Param};
use rudbman_sql::{
    Dialect, DmlError, DmlKind, DmlValue, InsertCell, RowUpdate, TableEdits, plan_edits,
};

use crate::query_source::{ResultSource, bit_is_boolean, sql_types};

/// What has been staged into one cell.
///
/// Three states and not two, because an `INSERT` can say three things about a
/// column: write this, write NULL, or do not name the column at all and let the
/// server decide. The third is what makes an auto-increment key and a
/// `DEFAULT CURRENT_TIMESTAMP` work, and flattening it into NULL would turn
/// both into a rejected statement.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StagedCell {
    /// The column is left out of the statement; the server supplies it.
    ///
    /// Only ever holds for a cell of an inserted row. A row the server already
    /// has holds a value in every column, so there is nothing for it to leave
    /// out — [`EditSet::stage`] treats `Unset` against a base row as "un-stage
    /// this cell" rather than as a value.
    Unset,
    /// SQL NULL, written deliberately.
    Null,
    /// What the user typed, verbatim.
    ///
    /// Not trimmed and not parsed: nothing here knows what the column's type
    /// will make of it, and a layer that silently trimmed a `CHAR(10)` would be
    /// wrong in a way nobody could see.
    Text(String),
}

impl StagedCell {
    /// Whether this is what a cell reading `original` already holds.
    ///
    /// `original` is the base row's text, or `None` for a base cell that is
    /// NULL. The whole of "did the edit walk back to where it started?".
    fn matches(&self, original: Option<&str>) -> bool {
        match self {
            StagedCell::Unset => false,
            StagedCell::Null => original.is_none(),
            StagedCell::Text(text) => original == Some(text.as_str()),
        }
    }
}

/// Which of the two row spaces a grid row index falls in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RowRef {
    /// A row the server gave us, by its index in the append-only source.
    Base(usize),
    /// A row the user is adding, by its position in [`EditSet::inserted`].
    Inserted(usize),
}

/// How much is staged, as the toolbar counts it.
///
/// Rows and not cells: the toolbar's job is to say how many statements an apply
/// would send, and that is one per row whatever was done inside it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct EditCounts {
    /// Base rows with at least one changed cell, deletions excluded.
    pub changed: usize,
    /// Rows staged to be added.
    pub inserted: usize,
    /// Rows staged to go.
    pub deleted: usize,
}

/// Everything staged against one table's rows, and nothing else.
///
/// Deliberately ignorant of the rows themselves: it holds indices and values,
/// and the base text an edit is compared against is passed in by the caller
/// that has it ([`EditableSource`]). That is what keeps this testable in a
/// dozen lines per case.
#[derive(Debug, Default)]
pub struct EditSet {
    /// New values by `(base row, source column)`, which group by row into
    /// `TableEdits::updates`.
    ///
    /// Never holds [`StagedCell::Unset`]: see that variant's own note.
    pub changed: HashMap<(usize, usize), StagedCell>,
    /// Base rows staged to be deleted, which become `TableEdits::deletes` once
    /// each one's key values are read out of the rows underneath.
    ///
    /// Ordered, because the statements a batch becomes should read down the
    /// grid rather than in whatever order a hash gave them.
    pub deleted: BTreeSet<usize>,
    /// Rows staged to be added: one entry per row, one cell per column, which
    /// is already the shape of `TableEdits::inserts`.
    pub inserted: Vec<Vec<StagedCell>>,
}

impl EditSet {
    /// Nothing staged.
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether an apply would send nothing.
    pub fn is_empty(&self) -> bool {
        self.changed.is_empty() && self.deleted.is_empty() && self.inserted.is_empty()
    }

    /// How many rows of each kind are staged.
    pub fn counts(&self) -> EditCounts {
        // A row that is both edited and deleted counts once, as a deletion:
        // that is the statement it will become, and counting it twice would
        // have the toolbar promise more statements than the apply sends.
        let mut rows: BTreeSet<usize> = BTreeSet::new();
        for (row, _) in self.changed.keys() {
            if !self.deleted.contains(row) {
                rows.insert(*row);
            }
        }
        EditCounts {
            changed: rows.len(),
            inserted: self.inserted.len(),
            deleted: self.deleted.len(),
        }
    }

    /// Which row space `row` falls in, against a base of `base_rows` rows.
    ///
    /// `None` for an index past the last inserted row, which the grid can ask
    /// for between a layout and a paint.
    pub fn locate(&self, row: usize, base_rows: usize) -> Option<RowRef> {
        if row < base_rows {
            return Some(RowRef::Base(row));
        }
        let index = row - base_rows;
        (index < self.inserted.len()).then_some(RowRef::Inserted(index))
    }

    /// How many rows the grid sees: the base, plus the ones being added.
    pub fn row_count(&self, base_rows: usize) -> usize {
        base_rows + self.inserted.len()
    }

    /// Stages `value` into a **base** row's cell, or un-stages it.
    ///
    /// `original` is what the server gave for that cell — `None` for NULL. A
    /// value equal to it clears the entry rather than recording a change, so a
    /// cell edited `A → B → A` is clean again and no `UPDATE` names it.
    ///
    /// Answers whether anything is staged against the cell afterwards.
    pub fn stage(
        &mut self,
        row: usize,
        column: usize,
        value: StagedCell,
        original: Option<&str>,
    ) -> bool {
        if matches!(value, StagedCell::Unset) || value.matches(original) {
            self.changed.remove(&(row, column));
            return false;
        }
        self.changed.insert((row, column), value);
        true
    }

    /// What is staged against a base row's cell, if anything.
    pub fn staged(&self, row: usize, column: usize) -> Option<&StagedCell> {
        self.changed.get(&(row, column))
    }

    /// Whether a base row is staged to be deleted.
    pub fn is_deleted(&self, row: usize) -> bool {
        self.deleted.contains(&row)
    }

    /// Marks a base row for deletion, or takes the mark off, and says which it
    /// did.
    ///
    /// The edits staged against the row are left alone either way. Undeleting a
    /// row the user had also typed into gives them back what they typed, which
    /// is the only behaviour that does not punish a mis-click.
    pub fn toggle_deleted(&mut self, row: usize) -> bool {
        if self.deleted.remove(&row) {
            return false;
        }
        self.deleted.insert(row);
        true
    }

    /// Appends a row of `columns` columns, every one of them left to the
    /// server, and answers its position in the insert list.
    pub fn add_insert(&mut self, columns: usize) -> usize {
        self.inserted.push(vec![StagedCell::Unset; columns]);
        self.inserted.len() - 1
    }

    /// Puts `value` into a cell of an inserted row.
    ///
    /// No comparison against anything: an inserted row has no original, so
    /// every one of the three states is a state the user chose. Out-of-range
    /// indices are ignored rather than panicking — the grid can ask about a row
    /// a discard took away between a layout and a paint.
    pub fn stage_inserted(&mut self, index: usize, column: usize, value: StagedCell) {
        if let Some(cell) = self
            .inserted
            .get_mut(index)
            .and_then(|row| row.get_mut(column))
        {
            *cell = value;
        }
    }

    /// What an inserted row's cell holds.
    pub fn inserted_cell(&self, index: usize, column: usize) -> Option<&StagedCell> {
        self.inserted.get(index)?.get(column)
    }

    /// Throws away everything staged against one row, and says whether there
    /// was anything.
    ///
    /// On a base row that is the edits and the deletion mark both. On an
    /// inserted row it is the row itself: the row *is* the change, so there is
    /// nothing left of it to keep, and the ones after it slide up.
    pub fn discard_row(&mut self, row: usize, base_rows: usize) -> bool {
        match self.locate(row, base_rows) {
            Some(RowRef::Base(row)) => {
                let deleted = self.deleted.remove(&row);
                let before = self.changed.len();
                self.changed.retain(|(staged, _), _| *staged != row);
                deleted || self.changed.len() != before
            }
            Some(RowRef::Inserted(index)) => {
                self.inserted.remove(index);
                true
            }
            None => false,
        }
    }

    /// Throws everything away.
    pub fn discard_all(&mut self) {
        self.changed.clear();
        self.deleted.clear();
        self.inserted.clear();
    }

    /// How a row is marked, against a base of `base_rows` rows.
    ///
    /// Deletion wins over modification, because it is the statement the row
    /// will become: a row that was typed into and then struck out sends one
    /// `DELETE` and no `UPDATE`.
    pub fn status(&self, row: usize, base_rows: usize) -> RowStatus {
        match self.locate(row, base_rows) {
            Some(RowRef::Base(row)) => {
                if self.deleted.contains(&row) {
                    RowStatus::Deleted
                } else if self.changed.keys().any(|(staged, _)| *staged == row) {
                    RowStatus::Modified
                } else {
                    RowStatus::Unchanged
                }
            }
            Some(RowRef::Inserted(_)) => RowStatus::Inserted,
            None => RowStatus::Unchanged,
        }
    }

    /// Whether one cell carries a value the server has not seen.
    ///
    /// An inserted row's untouched cells are *not* dirty: nothing has been put
    /// in them, and marking the whole row's width would say the user typed
    /// twenty values when they typed two.
    pub fn cell_dirty(&self, row: usize, column: usize, base_rows: usize) -> bool {
        match self.locate(row, base_rows) {
            Some(RowRef::Base(row)) => self.changed.contains_key(&(row, column)),
            Some(RowRef::Inserted(index)) => !matches!(
                self.inserted_cell(index, column),
                None | Some(StagedCell::Unset)
            ),
            None => false,
        }
    }
}

/// What one column will and will not accept.
///
/// Read once off the result's [`ColumnInfo`], because it cannot change while
/// the result stands and re-deriving it per frame would put a `match` on a JDBC
/// type constant inside the grid's draw loop.
#[derive(Clone, Copy, Debug)]
struct ColumnRules {
    /// Whether a value may be written into it at all.
    ///
    /// False for the two the driver tells us about: a column it reports
    /// read-only, and an auto-increment key — which is not merely pointless to
    /// type into but actively wrong, since leaving it out is what gets the
    /// server to generate one.
    writable: bool,
    /// Whether the catalogue allows NULL.
    ///
    /// `ResultSetMetaData.isNullable` answers 0 no, 1 yes, 2 unknown, and
    /// unknown is taken as yes: refusing "Set NULL" on a column the driver
    /// merely declined to describe would be a guess dressed as a rule, and the
    /// server rejects the statement if the guess was wrong.
    nullable: bool,
    /// The form a value of this column is bound in.
    ///
    /// Resolved from the JDBC `sql_type` once, here, for the same reason the
    /// other two are: it cannot change while the result stands, and the apply
    /// asks it once per staged cell rather than once per frame.
    kind: DmlKind,
}

/// `ResultSetMetaData.columnNoNulls`, the one answer that forbids NULL.
const COLUMN_NO_NULLS: i32 = 0;

impl ColumnRules {
    fn of(column: &ColumnInfo) -> Self {
        Self {
            writable: !column.read_only && !column.auto_increment,
            nullable: column.nullable != COLUMN_NO_NULLS,
            kind: dml_kind(column),
        }
    }
}

/// The bind form a column's values take, from its `java.sql.Types` constant.
///
/// The mapping is written once, here, and everything about it is conservative:
/// a type this version has never heard of becomes [`DmlKind::Str`], which is the
/// form the driver is most likely to be able to read back into whatever the
/// column really is, and the one that cannot lose anything on the way — the text
/// bound is the text the user typed.
///
/// Two choices are worth their reasons. Everything numeric that is not an
/// integer — `FLOAT`, `REAL` and `DOUBLE` as much as `DECIMAL` and `NUMERIC` —
/// is [`DmlKind::Decimal`], because that is the kind that reaches
/// `setBigDecimal` and arrives exact; routing a typed `0.1` through a double
/// would round it on the way to the server, which is the one thing an editor
/// must not do. And `BIT` splits on precision the way the grid's own column kind
/// does, through [`bit_is_boolean`]: MySQL's `BIT(n)` for `n > 1` is a byte
/// string and not a truth value.
pub fn dml_kind(column: &ColumnInfo) -> DmlKind {
    use sql_types::*;
    match column.sql_type {
        BIT if bit_is_boolean(column) => DmlKind::Bool,
        BIT => DmlKind::Bytes,
        BOOLEAN => DmlKind::Bool,
        TINYINT | SMALLINT | INTEGER | BIGINT => DmlKind::I64,
        NUMERIC | DECIMAL | FLOAT | REAL | DOUBLE => DmlKind::Decimal,
        DATE => DmlKind::Date,
        TIME | TIME_WITH_TIMEZONE => DmlKind::Time,
        TIMESTAMP | TIMESTAMP_WITH_TIMEZONE => DmlKind::Timestamp,
        BINARY | VARBINARY | LONGVARBINARY | BLOB => DmlKind::Bytes,
        _ => DmlKind::Str,
    }
}

/// The rows as the grid sees them: what the server sent, with the staging
/// buffer laid over it.
///
/// A wrapper rather than a second life for [`ResultSource`], and the choice
/// matters: the query pane's use of that type is untouched by any of this — it
/// goes on holding a plain [`ResultSource`] whose `cell` is one slice and whose
/// three editing hooks are the trait's own "nothing has changed and nothing
/// may" defaults. Everything editable is additive and lives here, in the one
/// pane that knows the table and its key.
#[derive(Debug)]
pub struct EditableSource {
    base: ResultSource,
    edits: EditSet,
    columns: Vec<ColumnRules>,
    /// Whether the pane as a whole may be written to.
    ///
    /// The single answer to §7.9's two standing reasons — no primary key, or a
    /// read-only profile — decided once when the result arrives, because both
    /// are facts about the object rather than states it passes through.
    writable: bool,
}

impl EditableSource {
    /// An overlay over `base`, whose columns are described by `columns`.
    ///
    /// `writable` is the pane's answer to "may anything here be changed at
    /// all"; false makes every cell refuse an edit however the columns are
    /// described.
    pub fn new(base: ResultSource, columns: &[ColumnInfo], writable: bool) -> Self {
        Self {
            base,
            edits: EditSet::new(),
            columns: columns.iter().map(ColumnRules::of).collect(),
            writable,
        }
    }

    /// The rows the server sent, to append another page to.
    ///
    /// Growing the base slides the inserted rows down the grid, which is right:
    /// they are drawn after the last fetched row, and there is now one more of
    /// those.
    pub fn base_mut(&mut self) -> &mut ResultSource {
        &mut self.base
    }

    /// What is staged.
    pub fn edits(&self) -> &EditSet {
        &self.edits
    }

    /// What is staged, to change.
    pub fn edits_mut(&mut self) -> &mut EditSet {
        &mut self.edits
    }

    /// How many rows the server sent, which is where the inserted ones begin.
    pub fn base_rows(&self) -> usize {
        self.base.row_count()
    }

    /// Says whether more rows are coming, or one is on its way.
    pub fn set_state(&mut self, state: GridSourceState) {
        self.base.set_state(state);
    }

    /// The value the server gave for a cell, whatever is staged over it.
    ///
    /// The `WHERE` clause of an `UPDATE` is written from this and never from
    /// [`GridSource::cell`]: a key column the user retyped is *set* to its new
    /// value and *found* by its old one, so a statement built from the overlay
    /// would name a row that does not exist. `None` for an inserted row, which
    /// the server has never seen.
    pub fn original(&self, row: usize, column: usize) -> Option<GridCell<'_>> {
        (row < self.base.row_count()).then(|| self.base.cell(row, column))
    }

    /// The base cell's text, in the form [`EditSet::stage`] compares against.
    ///
    /// Also what a generated `WHERE` clause binds: the two are the same reading
    /// on purpose, so that a cell the apply can identify a row by is exactly a
    /// cell an edit can be compared against.
    pub fn original_text(&self, row: usize, column: usize) -> Option<&str> {
        match self.original(row, column)? {
            GridCell::Text(text) => Some(text),
            // A LOB has no text here and is not editable anyway; treating it as
            // NULL only decides whether an edit that cannot happen would count.
            GridCell::Null | GridCell::Default | GridCell::Lob { .. } => None,
        }
    }

    /// Stages `value` into whichever row space `row` names.
    ///
    /// The one entry point the pane's gestures go through, so that neither the
    /// commit handler nor the menu has to know which half of the grid it is
    /// pointing at.
    pub fn stage(&mut self, row: usize, column: usize, value: StagedCell) {
        match self.edits.locate(row, self.base.row_count()) {
            Some(RowRef::Base(row)) => {
                let original = self.original_text(row, column).map(str::to_owned);
                self.edits.stage(row, column, value, original.as_deref());
            }
            Some(RowRef::Inserted(index)) => self.edits.stage_inserted(index, column, value),
            None => {}
        }
    }

    /// Whether the column at `column` will take a NULL, as the catalogue
    /// described it.
    pub fn nullable(&self, column: usize) -> bool {
        self.columns.get(column).is_some_and(|rules| rules.nullable)
    }

    /// The form values of `column` are bound in.
    ///
    /// [`DmlKind::Str`] for a column past the end, which is the same fallback
    /// [`dml_kind`] gives a type it does not know: an index the result does not
    /// have cannot reach a statement, and answering with the harmless kind keeps
    /// this total.
    pub fn kind(&self, column: usize) -> DmlKind {
        self.columns
            .get(column)
            .map_or(DmlKind::Str, |rules| rules.kind)
    }

    /// Whether anything here may be changed at all.
    pub fn writable(&self) -> bool {
        self.writable
    }
}

impl GridSource for EditableSource {
    fn column_count(&self) -> usize {
        self.base.column_count()
    }

    fn column(&self, index: usize) -> GridColumn<'_> {
        self.base.column(index)
    }

    fn row_count(&self) -> usize {
        self.edits.row_count(self.base.row_count())
    }

    fn cell(&self, row: usize, column: usize) -> GridCell<'_> {
        match self.edits.locate(row, self.base.row_count()) {
            Some(RowRef::Base(row)) => match self.edits.staged(row, column) {
                Some(StagedCell::Text(text)) => GridCell::Text(text),
                Some(StagedCell::Null) => GridCell::Null,
                // Never recorded against a base row; falling through to the
                // server's value is what it would mean if it were.
                Some(StagedCell::Unset) | None => self.base.cell(row, column),
            },
            Some(RowRef::Inserted(index)) => match self.edits.inserted_cell(index, column) {
                Some(StagedCell::Text(text)) => GridCell::Text(text),
                Some(StagedCell::Null) => GridCell::Null,
                Some(StagedCell::Unset) => GridCell::Default,
                // A column the row was not built with, which only a source that
                // grew a column mid-result could produce.
                None => GridCell::Default,
            },
            None => GridCell::Null,
        }
    }

    fn state(&self) -> GridSourceState {
        self.base.state()
    }

    fn row_status(&self, row: usize) -> RowStatus {
        self.edits.status(row, self.base.row_count())
    }

    fn cell_dirty(&self, row: usize, column: usize) -> bool {
        self.edits.cell_dirty(row, column, self.base.row_count())
    }

    fn cell_editable(&self, row: usize, column: usize) -> bool {
        if !self.writable {
            return false;
        }
        if !self.columns.get(column).is_some_and(|rules| rules.writable) {
            return false;
        }
        match self.edits.locate(row, self.base.row_count()) {
            // A row on its way out takes no more typing: the value would be
            // staged into an `UPDATE` that is never written, and the cell is
            // struck through while the user types it.
            Some(RowRef::Base(row)) => {
                !self.edits.is_deleted(row)
                    && !matches!(self.base.cell(row, column), GridCell::Lob { .. })
            }
            Some(RowRef::Inserted(_)) => true,
            None => false,
        }
    }
}

/// One statement of an apply: ready to send, and ready to show.
///
/// The three parts are three audiences. `params` goes on the wire, `values` goes
/// in the confirmation the user reads before it does, and `checked` is what the
/// apply asks the server about afterwards. They are carried together because
/// they are one decision: the statement the user approved must be the statement
/// that runs, and pulling them apart is how a preview drifts from a batch.
#[derive(Clone, Debug, PartialEq)]
pub struct PlannedStatement {
    /// The SQL, with a `?` everywhere a value goes.
    pub sql: String,
    /// The bound parameters, in the order their `?`s appear.
    pub params: Vec<Param>,
    /// The same values as text, `None` for SQL NULL.
    ///
    /// What the confirmation lists under the statement. Kept beside `params`
    /// rather than derived from it because a [`Param::Bytes`] is bytes by then
    /// and the user typed hex.
    pub values: Vec<Option<String>>,
    /// Whether the server must report exactly one changed row.
    ///
    /// True for every `UPDATE` and `DELETE` and false for every `INSERT`, which
    /// is §7.9's staleness guard: the `WHERE` clause names the primary key and
    /// nothing else, and the row count is what says the row named is still the
    /// row that was read. An `INSERT` reaches no existing row, so there is
    /// nothing about it a count could tell us.
    pub checked: bool,
}

/// Why a staged buffer could not be turned into statements.
///
/// All four are worth showing. Three are about one column the user can go and
/// look at, and the fourth carries `rudbman-sql`'s own words for the shapes it
/// refuses.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PlanError {
    /// The table named no key columns, so no row can be identified.
    ///
    /// Unreachable from the pane, which draws no Apply button at all without a
    /// key (§7.9), and kept as a value rather than an assertion because this
    /// function is the one that knows.
    NoKey,
    /// A key column the metadata named is not among the result's columns.
    UnknownKeyColumn {
        /// The key column's name, as the catalogue spelled it.
        column: String,
    },
    /// What is in a cell cannot be bound to that column's type.
    BadValue {
        /// The column's name.
        column: String,
        /// The form the column takes.
        kind: DmlKind,
        /// What was in the cell.
        text: String,
    },
    /// `rudbman-sql` refused the edits.
    Dml(DmlError),
}

impl fmt::Display for PlanError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PlanError::NoKey => f.write_str("the table has no primary key"),
            PlanError::UnknownKeyColumn { column } => {
                write!(f, "key column `{column}` is not in the result")
            }
            PlanError::BadValue { column, kind, text } => write!(
                f,
                "`{text}` cannot be bound to column `{column}` as {kind:?}"
            ),
            PlanError::Dml(error) => error.fmt(f),
        }
    }
}

impl std::error::Error for PlanError {}

/// Turns everything staged against `source` into the statements that apply it.
///
/// `columns` is the result's column metadata — the same slice the source was
/// built from — and `keys` the primary key's column names in key order.
/// `table` is the name parts `crate::builder_sql::table_parts` produced for the
/// `SELECT`, so the statements write back to the table the rows were read from.
///
/// Every value is checked here rather than at bind time, which is what lets the
/// failure name a column: by the time a batch has been planned a value is a `?`
/// in a string, and "the third parameter of statement two" is not something to
/// show anybody. What is checked is only whether the text can *become* the bound
/// form — an integer that parses, hex of an even length — and never whether the
/// server will like it. That judgement is the server's, and its refusal says
/// more than a guess made here would.
pub fn plan_apply(
    source: &EditableSource,
    columns: &[ColumnInfo],
    table: Vec<String>,
    keys: &[String],
    dialect: &Dialect,
) -> Result<Vec<PlannedStatement>, PlanError> {
    if keys.is_empty() {
        return Err(PlanError::NoKey);
    }
    let names: Vec<String> = columns.iter().map(column_name).collect();
    let key: Vec<usize> = keys
        .iter()
        .map(|wanted| {
            key_index(&names, wanted).ok_or_else(|| PlanError::UnknownKeyColumn {
                column: wanted.clone(),
            })
        })
        .collect::<Result<_, _>>()?;

    let edits = source.edits();
    let mut planned = TableEdits {
        table,
        columns: names.clone(),
        key: key.clone(),
        ..TableEdits::default()
    };

    // `deleted` is a `BTreeSet`, so the statements read down the grid.
    for row in &edits.deleted {
        planned.deletes.push(row_key(source, &names, &key, *row)?);
    }

    // `changed` is a `HashMap`, so the rows it names have to be put back in
    // order before anything is generated: a batch whose statements came out in
    // a different order every time would be a preview nobody could check
    // against what ran, and two tests of it would disagree with each other.
    let mut changed: Vec<(usize, Vec<usize>)> = Vec::new();
    {
        let mut by_row: HashMap<usize, Vec<usize>> = HashMap::new();
        for (row, column) in edits.changed.keys() {
            if edits.deleted.contains(row) {
                // The row is going; §7.9 sends the `DELETE` and no `UPDATE`.
                continue;
            }
            by_row.entry(*row).or_default().push(*column);
        }
        changed.extend(by_row);
        changed.sort_by_key(|(row, _)| *row);
        for (_, columns) in &mut changed {
            columns.sort_unstable();
        }
    }
    for (row, cells) in changed {
        let mut set = Vec::with_capacity(cells.len());
        for column in cells {
            let Some(staged) = edits.staged(row, column) else {
                continue;
            };
            let name = names.get(column).map_or("", String::as_str);
            let kind = source.kind(column);
            match staged {
                // Never recorded against a base row (see `StagedCell::Unset`),
                // and nothing to assign if it somehow were.
                StagedCell::Unset => continue,
                StagedCell::Null => set.push((column, DmlValue::null(kind))),
                StagedCell::Text(text) => {
                    set.push((column, checked_value(kind, text, name)?));
                }
            }
        }
        if set.is_empty() {
            continue;
        }
        planned.updates.push(RowUpdate {
            key: row_key(source, &names, &key, row)?,
            set,
        });
    }

    for row in &edits.inserted {
        let mut cells = Vec::with_capacity(row.len());
        for (column, staged) in row.iter().enumerate() {
            let name = names.get(column).map_or("", String::as_str);
            let kind = source.kind(column);
            cells.push(match staged {
                StagedCell::Unset => InsertCell::Unset,
                StagedCell::Null => InsertCell::Set(DmlValue::null(kind)),
                StagedCell::Text(text) => InsertCell::Set(checked_value(kind, text, name)?),
            });
        }
        planned.inserts.push(cells);
    }

    // Everything before this line is `plan_edits`'s input; the SQL itself is
    // that function's, and no identifier is spelled here.
    let batch = plan_edits(&planned, dialect).map_err(PlanError::Dml)?;
    // The batch is deletes, then updates, then inserts — `plan_edits` says so
    // and its tests hold it — so the two lengths are where the checked
    // statements end.
    let checked = planned.deletes.len() + planned.updates.len();
    Ok(batch
        .into_iter()
        .enumerate()
        .map(|(index, statement)| PlannedStatement {
            sql: statement.sql,
            params: statement.values.iter().map(param).collect(),
            values: statement
                .values
                .iter()
                .map(|value| value.text().map(str::to_owned))
                .collect(),
            checked: index < checked,
        })
        .collect())
}

/// One row's key values, as the server gave them.
///
/// Original and never staged: a key column the user retyped is *set* to the new
/// value and *found* by the old one, so a `WHERE` built from the overlay would
/// name a row that does not exist.
fn row_key(
    source: &EditableSource,
    names: &[String],
    key: &[usize],
    row: usize,
) -> Result<Vec<DmlValue>, PlanError> {
    key.iter()
        .map(|column| {
            let name = names.get(*column).map_or("", String::as_str);
            let kind = source.kind(*column);
            match source.original_text(row, *column) {
                Some(text) => checked_value(kind, text, name),
                // A NULL — or a LOB, which has no text and cannot be a key —
                // becomes `DmlError::NullKey` in `plan_edits`, which names the
                // column it could not identify the row by.
                None => Ok(DmlValue::null(kind)),
            }
        })
        .collect()
}

/// A value of `kind` spelling `text`, if the text can be bound as one.
fn checked_value(kind: DmlKind, text: &str, column: &str) -> Result<DmlValue, PlanError> {
    let readable = match kind {
        DmlKind::I64 => text.trim().parse::<i64>().is_ok(),
        DmlKind::Bool => parse_bool(text).is_some(),
        DmlKind::Bytes => parse_hex(text).is_some(),
        // Everything else travels as the text it is: this side has no business
        // deciding what a server will make of a `DECIMAL` or a `TIMESTAMP`, and
        // a parser here would refuse forms some product accepts.
        DmlKind::Str | DmlKind::Decimal | DmlKind::Date | DmlKind::Time | DmlKind::Timestamp => {
            true
        }
    };
    if !readable {
        return Err(PlanError::BadValue {
            column: column.to_string(),
            kind,
            text: text.to_string(),
        });
    }
    Ok(DmlValue::new(kind, text))
}

/// The bind parameter one planned value becomes.
///
/// Total, and it can be: every text that reaches here came through
/// [`checked_value`], which refused anything the parses below cannot read. The
/// fallbacks are what a kind added later without a check would land on — a bound
/// string, which the driver judges for itself — rather than a panic in the
/// middle of an apply.
fn param(value: &DmlValue) -> Param {
    let Some(text) = value.text() else {
        return Param::Null;
    };
    match value.kind() {
        DmlKind::Str => Param::Str(text.to_string()),
        DmlKind::I64 => text
            .trim()
            .parse::<i64>()
            .map_or_else(|_| Param::Str(text.to_string()), Param::I64),
        // Typed, not a JSON number: a `DECIMAL(20,8)` sent as one goes through a
        // double and arrives rounded (§4.4).
        DmlKind::Decimal => Param::Decimal(text.to_string()),
        DmlKind::Bool => parse_bool(text).map_or_else(|| Param::Str(text.to_string()), Param::Bool),
        DmlKind::Date => Param::Date(text.to_string()),
        DmlKind::Time => Param::Time(text.to_string()),
        DmlKind::Timestamp => Param::Timestamp(text.to_string()),
        DmlKind::Bytes => {
            parse_hex(text).map_or_else(|| Param::Str(text.to_string()), Param::Bytes)
        }
    }
}

/// What the user may write into a boolean column.
///
/// More spellings than the grid renders, and deliberately: the grid draws
/// `true`/`false`, but a user retyping a column they think of as a flag writes
/// `1`, `Y` or `T` as readily, and refusing those would be pedantry with a
/// modal on it. Anything else is [`PlanError::BadValue`] rather than a guess.
fn parse_bool(text: &str) -> Option<bool> {
    match text.trim().to_ascii_lowercase().as_str() {
        "true" | "t" | "yes" | "y" | "1" => Some(true),
        "false" | "f" | "no" | "n" | "0" => Some(false),
        _ => None,
    }
}

/// Bytes from the hex the grid renders them as.
///
/// `Value::to_text` writes a binary cell as upper-case hex with no separator,
/// which is the form the field is seeded with and therefore the form an edited
/// one has to be read back from. Empty is a zero-length value and not an error;
/// an odd length or a non-hex digit is.
fn parse_hex(text: &str) -> Option<Vec<u8>> {
    let text = text.trim();
    if !text.len().is_multiple_of(2) {
        return None;
    }
    text.as_bytes()
        .chunks(2)
        .map(|pair| {
            let pair = std::str::from_utf8(pair).ok()?;
            u8::from_str_radix(pair, 16).ok()
        })
        .collect()
}

/// The name a statement should spell a column with.
///
/// The catalogue's name and not [`ColumnInfo::display_name`]'s preference for
/// the label: a `SELECT *` gives the two the same value, but where a driver
/// reports a label of its own it is a heading, and a heading is not something
/// an `UPDATE` can assign to.
fn column_name(column: &ColumnInfo) -> String {
    column
        .name
        .as_deref()
        .filter(|name| !name.is_empty())
        .map_or_else(|| column.display_name(), str::to_string)
}

/// Which column of the result a key column's name refers to.
///
/// Exactly first, then ignoring case. The second pass is for the products that
/// answer `getPrimaryKeys` in one case and `ResultSetMetaData` in another —
/// which several do, and where the exact match would leave a keyed table looking
/// keyless.
fn key_index(names: &[String], wanted: &str) -> Option<usize> {
    names.iter().position(|name| name == wanted).or_else(|| {
        names
            .iter()
            .position(|name| name.eq_ignore_ascii_case(wanted))
    })
}

#[cfg(test)]
mod tests {
    use rudbman_grid::GridColumnKind;

    use super::*;
    use crate::query_source::render_batch;
    use crate::query_source::tests::{batch, info};

    /// Three text columns, two rows: `("a", NULL, "")` and `("b", "y", "z")`.
    ///
    /// Goes through the codec the way the pane's own rows do, so the NULL in it
    /// is a real one rather than a value that happens to be empty.
    fn source(writable: bool) -> EditableSource {
        let columns = vec![
            info(1, "ID", 4, 0),
            info(2, "NAME", 12, 0),
            info(3, "NOTE", 12, 0),
        ];
        let mut base = ResultSource::new(&columns);
        base.push(render_batch(
            &batch(&[
                &[Some("a"), Some("b")],
                &[None, Some("y")],
                &[Some(""), Some("z")],
            ]),
            &columns,
        ));
        base.mark_primary_keys(&["ID".to_string()]);
        EditableSource::new(base, &columns, writable)
    }

    /// The overlay answers before the rows do, and only where something is
    /// staged.
    #[test]
    fn a_staged_value_hides_the_one_underneath_it() {
        let mut source = source(true);
        source.stage(0, 0, StagedCell::Text("A".into()));

        assert_eq!(source.cell(0, 0), GridCell::Text("A"));
        assert_eq!(source.cell(0, 1), GridCell::Null, "untouched, still NULL");
        assert_eq!(source.cell(1, 0), GridCell::Text("b"), "another row");
        assert!(source.cell_dirty(0, 0));
        assert!(!source.cell_dirty(0, 1));
        assert_eq!(source.row_status(0), RowStatus::Modified);
        assert_eq!(source.row_status(1), RowStatus::Unchanged);
    }

    /// NULL and the empty string stay two things all the way through the
    /// staging layer, in both directions.
    #[test]
    fn null_and_the_empty_string_are_staged_apart() {
        let mut source = source(true);

        // The NULL cell becomes the empty string, which is a change.
        source.stage(0, 1, StagedCell::Text(String::new()));
        assert_eq!(source.cell(0, 1), GridCell::Text(""));
        assert!(source.cell_dirty(0, 1));

        // The empty-string cell becomes NULL, which is also a change.
        source.stage(0, 2, StagedCell::Null);
        assert_eq!(source.cell(0, 2), GridCell::Null);
        assert!(source.cell_dirty(0, 2));

        // And each staged back to what it was is no change at all.
        source.stage(0, 1, StagedCell::Null);
        source.stage(0, 2, StagedCell::Text(String::new()));
        assert!(!source.cell_dirty(0, 1));
        assert!(!source.cell_dirty(0, 2));
        assert!(source.edits().is_empty());
    }

    /// A to B to A leaves nothing behind.
    #[test]
    fn an_edit_that_walks_back_un_stages_itself() {
        let mut source = source(true);
        source.stage(1, 1, StagedCell::Text("Y".into()));
        assert_eq!(source.edits().counts().changed, 1);

        source.stage(1, 1, StagedCell::Text("y".into()));
        assert!(source.edits().is_empty(), "the round trip left a change");
        assert_eq!(source.cell(1, 1), GridCell::Text("y"));
        assert_eq!(source.row_status(1), RowStatus::Unchanged);
    }

    /// A struck-out row is still drawn, still holds what was typed into it, and
    /// counts as one statement rather than two.
    #[test]
    fn a_delete_covers_an_edit_on_the_same_row() {
        let mut source = source(true);
        source.stage(0, 0, StagedCell::Text("A".into()));
        source.edits_mut().toggle_deleted(0);

        assert_eq!(source.row_status(0), RowStatus::Deleted, "deletion wins");
        assert_eq!(source.cell(0, 0), GridCell::Text("A"), "still drawn");
        assert!(!source.cell_editable(0, 0), "a row on its way out");
        assert_eq!(
            source.edits().counts(),
            EditCounts {
                changed: 0,
                inserted: 0,
                deleted: 1
            }
        );

        // And putting it back gives the edit back with it.
        source.edits_mut().toggle_deleted(0);
        assert_eq!(source.row_status(0), RowStatus::Modified);
        assert!(source.cell_editable(0, 0));
        assert_eq!(source.edits().counts().changed, 1);
    }

    /// An inserted row lives after the last fetched row, holds the three states
    /// a cell of one can hold, and draws each of them differently.
    #[test]
    fn an_inserted_rows_cells_are_unset_null_or_typed() {
        let mut source = source(true);
        assert_eq!(source.row_count(), 2);

        let index = source.edits_mut().add_insert(3);
        assert_eq!(index, 0);
        assert_eq!(source.row_count(), 3);
        assert_eq!(source.row_status(2), RowStatus::Inserted);

        source.stage(2, 0, StagedCell::Text("c".into()));
        source.stage(2, 1, StagedCell::Null);
        // Column 2 is left alone.

        assert_eq!(source.cell(2, 0), GridCell::Text("c"));
        assert_eq!(source.cell(2, 1), GridCell::Null);
        assert_eq!(
            source.cell(2, 2),
            GridCell::Default,
            "an untouched column is the server's, not NULL"
        );
        assert!(source.cell_dirty(2, 0));
        assert!(source.cell_dirty(2, 1), "a deliberate NULL is a value");
        assert!(!source.cell_dirty(2, 2), "an untouched column is not typed");
        assert_eq!(
            source.edits().inserted[0],
            [
                StagedCell::Text("c".into()),
                StagedCell::Null,
                StagedCell::Unset
            ]
        );
    }

    /// Paging in another batch slides the inserted rows down and leaves the
    /// base rows' staging exactly where it was.
    #[test]
    fn a_page_moves_the_inserted_rows_and_nothing_else() {
        let mut source = source(true);
        source.stage(1, 0, StagedCell::Text("B".into()));
        source.edits_mut().add_insert(3);
        source.stage(2, 0, StagedCell::Text("new".into()));

        let columns = vec![
            info(1, "ID", 4, 0),
            info(2, "NAME", 12, 0),
            info(3, "NOTE", 12, 0),
        ];
        source.base_mut().push(render_batch(
            &batch(&[&[Some("c")], &[Some("n")], &[Some("o")]]),
            &columns,
        ));

        assert_eq!(source.row_count(), 4);
        assert_eq!(source.cell(1, 0), GridCell::Text("B"), "the edit held");
        assert_eq!(source.cell(2, 0), GridCell::Text("c"), "the new base row");
        assert_eq!(
            source.cell(3, 0),
            GridCell::Text("new"),
            "the inserted row moved down with the base"
        );
        assert_eq!(source.row_status(3), RowStatus::Inserted);
    }

    /// The `WHERE` clause reads the row the server has, not the one on screen.
    #[test]
    fn the_original_is_readable_under_a_staged_key() {
        let mut source = source(true);
        source.stage(0, 0, StagedCell::Text("A".into()));

        assert_eq!(source.cell(0, 0), GridCell::Text("A"));
        assert_eq!(source.original(0, 0), Some(GridCell::Text("a")));
        assert_eq!(source.original(0, 1), Some(GridCell::Null));

        source.edits_mut().add_insert(3);
        assert_eq!(
            source.original(2, 0),
            None,
            "an inserted row has no original"
        );
    }

    /// Discarding one row takes that row's changes and nothing else; an
    /// inserted row is discarded by going away.
    #[test]
    fn a_row_can_be_discarded_on_its_own() {
        let mut source = source(true);
        source.stage(0, 0, StagedCell::Text("A".into()));
        source.stage(1, 0, StagedCell::Text("B".into()));
        source.edits_mut().toggle_deleted(0);
        source.edits_mut().add_insert(3);
        source.stage(2, 0, StagedCell::Text("new".into()));

        let base = source.base_rows();
        assert!(source.edits_mut().discard_row(0, base));
        assert_eq!(source.row_status(0), RowStatus::Unchanged);
        assert_eq!(source.cell(0, 0), GridCell::Text("a"));
        assert_eq!(source.cell(1, 0), GridCell::Text("B"), "row 1 kept its own");

        assert!(source.edits_mut().discard_row(2, base));
        assert_eq!(source.row_count(), 2, "the inserted row went with it");
        assert!(!source.edits().is_empty(), "row 1 is still staged");

        source.edits_mut().discard_all();
        assert!(source.edits().is_empty());
        assert_eq!(source.cell(1, 0), GridCell::Text("b"));
    }

    /// The counters say how many statements an apply would send.
    #[test]
    fn the_counts_are_rows_and_not_cells() {
        let mut source = source(true);
        source.stage(0, 0, StagedCell::Text("A".into()));
        source.stage(0, 1, StagedCell::Text("N".into()));
        source.stage(0, 2, StagedCell::Null);
        source.edits_mut().toggle_deleted(1);
        source.edits_mut().add_insert(3);
        source.edits_mut().add_insert(3);

        assert_eq!(
            source.edits().counts(),
            EditCounts {
                changed: 1,
                inserted: 2,
                deleted: 1
            },
            "three cells of one row are one UPDATE"
        );
        assert!(!source.edits().is_empty());
    }

    /// Nothing is editable in a pane that may not be written to, and nothing is
    /// editable in a column the driver said to leave alone.
    #[test]
    fn the_two_standing_refusals_are_answered_per_cell() {
        let read_only = source(false);
        assert!(!read_only.cell_editable(0, 0));
        assert!(!read_only.writable());

        let mut columns = vec![
            info(1, "ID", 4, 0),
            info(2, "NAME", 12, 0),
            info(3, "NOTE", 12, 0),
        ];
        columns[0].auto_increment = true;
        columns[2].read_only = true;
        columns[1].nullable = COLUMN_NO_NULLS;
        let source = EditableSource::new(ResultSource::new(&columns), &columns, true);

        assert!(!source.cell_editable(0, 0), "an auto-increment key");
        assert!(!source.cell_editable(0, 2), "a read-only column");
        assert!(!source.nullable(1), "the catalogue forbids NULL");
        assert!(source.nullable(0), "unknown nullability is taken as yes");
    }

    /// An auto-increment column stays out of an inserted row too, so the server
    /// is the one that fills it in.
    #[test]
    fn an_auto_increment_column_is_not_typed_into_on_a_new_row() {
        let mut columns = vec![info(1, "ID", 4, 0), info(2, "NAME", 12, 0)];
        columns[0].auto_increment = true;
        let mut source = EditableSource::new(ResultSource::new(&columns), &columns, true);
        source.edits_mut().add_insert(2);

        assert!(!source.cell_editable(0, 0));
        assert!(source.cell_editable(0, 1));
        assert_eq!(source.cell(0, 0), GridCell::Default);
    }

    /// The column headings and the key marking come straight through, so the
    /// grid over an overlay looks like the grid over the rows.
    #[test]
    fn the_columns_pass_through_untouched() {
        let source = source(true);
        assert_eq!(source.column_count(), 3);
        assert_eq!(source.column(0).name, "ID");
        assert!(source.column(0).primary_key);
        assert!(!source.column(1).primary_key);
        assert_eq!(source.column(0).kind, GridColumnKind::Number);
        assert_eq!(source.state(), GridSourceState::Complete);
    }

    /// `APP.PERSON(ID integer, NAME varchar, NOTE varchar)`, keyed by `ID`.
    ///
    /// Its own fixture rather than [`source`]'s, because the planning cares
    /// about two things that one does not: the key column has to hold something
    /// an `INTEGER` can be bound from, and the names have to be spelled the way
    /// H2 stores them or every statement below would be quoted.
    fn plan_columns() -> Vec<ColumnInfo> {
        vec![
            info(1, "ID", sql_types::INTEGER, 0),
            info(2, "NAME", 12, 0),
            info(3, "NOTE", 12, 0),
        ]
    }

    /// Two rows of it: `(1, "a", "n1")` and `(2, "b", "")`.
    fn plan_source() -> EditableSource {
        let columns = plan_columns();
        let mut base = ResultSource::new(&columns);
        base.push(render_batch(
            &batch(&[
                &[Some("1"), Some("2")],
                &[Some("a"), Some("b")],
                &[Some("n1"), Some("")],
            ]),
            &columns,
        ));
        base.mark_primary_keys(&["ID".to_string()]);
        EditableSource::new(base, &columns, true)
    }

    /// Plans what is staged against [`plan_source`].
    fn plan(source: &EditableSource) -> Result<Vec<PlannedStatement>, PlanError> {
        plan_apply(
            source,
            &plan_columns(),
            vec!["APP".to_string(), "PERSON".to_string()],
            &["ID".to_string()],
            &Dialect::H2,
        )
    }

    /// Every branch of the `sql_type` table, and the fallback for what is not
    /// in it.
    #[test]
    fn a_columns_bind_form_comes_from_its_jdbc_type() {
        use sql_types::*;
        for (sql_type, precision, expected) in [
            (BOOLEAN, 0, DmlKind::Bool),
            // `BIT(1)` is a flag; `BIT(8)` is a byte string, and the split is
            // the same one the grid draws on.
            (BIT, 1, DmlKind::Bool),
            (BIT, 8, DmlKind::Bytes),
            (TINYINT, 0, DmlKind::I64),
            (SMALLINT, 0, DmlKind::I64),
            (INTEGER, 0, DmlKind::I64),
            (BIGINT, 0, DmlKind::I64),
            (DECIMAL, 0, DmlKind::Decimal),
            (NUMERIC, 0, DmlKind::Decimal),
            // Exact all the way to `setBigDecimal`, never through a double.
            (FLOAT, 0, DmlKind::Decimal),
            (REAL, 0, DmlKind::Decimal),
            (DOUBLE, 0, DmlKind::Decimal),
            (DATE, 0, DmlKind::Date),
            (TIME, 0, DmlKind::Time),
            (TIME_WITH_TIMEZONE, 0, DmlKind::Time),
            (TIMESTAMP, 0, DmlKind::Timestamp),
            (TIMESTAMP_WITH_TIMEZONE, 0, DmlKind::Timestamp),
            (BINARY, 0, DmlKind::Bytes),
            (VARBINARY, 0, DmlKind::Bytes),
            (LONGVARBINARY, 0, DmlKind::Bytes),
            (BLOB, 0, DmlKind::Bytes),
            // `VARCHAR`, and then the exotics: an array, a vendor type, and a
            // constant no version of this table has heard of. All text.
            (12, 0, DmlKind::Str),
            (2003, 0, DmlKind::Str),
            (1111, 0, DmlKind::Str),
            (31_337, 0, DmlKind::Str),
        ] {
            assert_eq!(
                dml_kind(&info(1, "C", sql_type, precision)),
                expected,
                "type {sql_type}"
            );
        }
    }

    /// A change, a deletion and an insertion, in the order one transaction can
    /// carry them.
    #[test]
    fn a_mixed_edit_set_becomes_a_delete_an_update_and_an_insert() {
        let mut source = plan_source();
        source.stage(0, 1, StagedCell::Text("A".into()));
        source.stage(0, 2, StagedCell::Null);
        source.edits_mut().toggle_deleted(1);
        source.edits_mut().add_insert(3);
        source.stage(2, 0, StagedCell::Text("9".into()));
        source.stage(2, 1, StagedCell::Text("new".into()));
        // NOTE is left alone, so the server supplies it.

        let batch = plan(&source).expect("the edits plan");
        let sql: Vec<&str> = batch.iter().map(|s| s.sql.as_str()).collect();
        assert_eq!(
            sql,
            [
                "DELETE FROM APP.PERSON WHERE ID = ?",
                "UPDATE APP.PERSON SET NAME = ?, NOTE = ? WHERE ID = ?",
                "INSERT INTO APP.PERSON (ID, NAME) VALUES (?, ?)",
            ]
        );

        // The `WHERE` binds the row's own key, typed as the column is.
        assert_eq!(batch[0].params, [Param::I64(2)]);
        // Assignments first, key second, and the deliberate NULL is a bound
        // NULL rather than the word.
        assert_eq!(
            batch[1].params,
            [Param::Str("A".into()), Param::Null, Param::I64(1)]
        );
        assert_eq!(batch[2].params, [Param::I64(9), Param::Str("new".into())]);

        // Only the two statements that name an existing row are counted.
        assert_eq!(
            batch.iter().map(|s| s.checked).collect::<Vec<_>>(),
            [true, true, false]
        );
        // And the confirmation reads the values as text, NULL included.
        assert_eq!(
            batch[1].values,
            [Some("A".to_string()), None, Some("1".to_string())]
        );
    }

    /// A row that is both typed into and struck out sends the `DELETE` alone.
    #[test]
    fn a_deleted_row_contributes_no_update() {
        let mut source = plan_source();
        source.stage(0, 1, StagedCell::Text("A".into()));
        source.edits_mut().toggle_deleted(0);

        let batch = plan(&source).expect("the edits plan");
        assert_eq!(batch.len(), 1);
        assert_eq!(batch[0].sql, "DELETE FROM APP.PERSON WHERE ID = ?");
    }

    /// The `WHERE` clause is written from the row the server gave, even where
    /// the key itself is what was retyped.
    #[test]
    fn a_retyped_key_is_set_to_the_new_value_and_found_by_the_old_one() {
        let mut source = plan_source();
        source.stage(0, 0, StagedCell::Text("7".into()));

        let batch = plan(&source).expect("the edits plan");
        assert_eq!(batch[0].sql, "UPDATE APP.PERSON SET ID = ? WHERE ID = ?");
        assert_eq!(batch[0].params, [Param::I64(7), Param::I64(1)]);
    }

    /// Two changed rows come out in row order however the buffer happens to be
    /// iterated, and so do a row's own columns.
    #[test]
    fn the_statements_come_out_in_a_settled_order() {
        let mut source = plan_source();
        source.stage(1, 2, StagedCell::Text("z2".into()));
        source.stage(1, 1, StagedCell::Text("y2".into()));
        source.stage(0, 1, StagedCell::Text("A".into()));

        let batch = plan(&source).expect("the edits plan");
        assert_eq!(
            batch.iter().map(|s| s.sql.as_str()).collect::<Vec<_>>(),
            [
                "UPDATE APP.PERSON SET NAME = ? WHERE ID = ?",
                "UPDATE APP.PERSON SET NAME = ?, NOTE = ? WHERE ID = ?",
            ]
        );
        assert_eq!(batch[0].params[1], Param::I64(1), "row 0 came first");
        assert_eq!(batch[1].params[2], Param::I64(2));
    }

    /// A value the column's type cannot take is refused here, naming the
    /// column, rather than sent for the driver to reject halfway through a
    /// batch.
    #[test]
    fn a_value_that_cannot_be_bound_is_refused_by_name() {
        let mut source = plan_source();
        source.stage(0, 0, StagedCell::Text("not a number".into()));

        assert_eq!(
            plan(&source),
            Err(PlanError::BadValue {
                column: "ID".to_string(),
                kind: DmlKind::I64,
                text: "not a number".to_string(),
            })
        );
    }

    /// The three kinds whose text is parsed on the way to a parameter, and the
    /// spellings each accepts.
    #[test]
    fn the_parsed_kinds_read_what_the_grid_writes() {
        assert_eq!(param(&DmlValue::new(DmlKind::I64, " 42 ")), Param::I64(42));
        for text in ["true", "TRUE", "t", "yes", "Y", "1"] {
            assert_eq!(
                param(&DmlValue::new(DmlKind::Bool, text)),
                Param::Bool(true)
            );
        }
        for text in ["false", "F", "no", "n", "0"] {
            assert_eq!(
                param(&DmlValue::new(DmlKind::Bool, text)),
                Param::Bool(false)
            );
        }
        // Upper-case hex with no separator is what `Value::to_text` writes.
        assert_eq!(
            param(&DmlValue::new(DmlKind::Bytes, "DEADBEEF")),
            Param::Bytes(vec![0xde, 0xad, 0xbe, 0xef])
        );
        assert_eq!(
            param(&DmlValue::new(DmlKind::Bytes, "")),
            Param::Bytes(Vec::new()),
            "a zero-length value is not an error"
        );

        // And what none of them will read.
        for (kind, text) in [
            (DmlKind::I64, "1.5"),
            (DmlKind::Bool, "maybe"),
            (DmlKind::Bytes, "ABC"),
            (DmlKind::Bytes, "ZZ"),
        ] {
            assert!(
                checked_value(kind, text, "C").is_err(),
                "{kind:?} accepted {text:?}"
            );
        }

        // The typed forms travel verbatim: nothing here decides what a server
        // will make of a decimal or a timestamp.
        assert_eq!(
            param(&DmlValue::new(DmlKind::Decimal, "1.500")),
            Param::Decimal("1.500".into())
        );
        assert_eq!(
            param(&DmlValue::new(DmlKind::Timestamp, "2026-08-10 09:30:00")),
            Param::Timestamp("2026-08-10 09:30:00".into())
        );
        assert_eq!(param(&DmlValue::null(DmlKind::Date)), Param::Null);
    }

    /// A key column whose original is NULL cannot be found again, and the
    /// refusal names it.
    #[test]
    fn a_null_key_is_refused_before_anything_is_sent() {
        let columns = vec![info(1, "NAME", 12, 0), info(2, "NOTE", 12, 0)];
        let mut base = ResultSource::new(&columns);
        base.push(render_batch(&batch(&[&[None], &[Some("z")]]), &columns));
        let mut source = EditableSource::new(base, &columns, true);
        source.stage(0, 1, StagedCell::Text("Z".into()));

        let planned = plan_apply(
            &source,
            &columns,
            vec!["t".to_string()],
            &["NAME".to_string()],
            &Dialect::H2,
        );
        assert_eq!(
            planned,
            Err(PlanError::Dml(rudbman_sql::DmlError::NullKey {
                column: "NAME".to_string()
            }))
        );
    }

    /// The two ways a key can fail to name a column of the result.
    #[test]
    fn a_key_the_result_does_not_carry_is_named_in_the_refusal() {
        let mut source = plan_source();
        source.stage(0, 1, StagedCell::Text("A".into()));

        assert_eq!(
            plan_apply(
                &source,
                &plan_columns(),
                vec!["PERSON".to_string()],
                &["ROWID".to_string()],
                &Dialect::H2,
            ),
            Err(PlanError::UnknownKeyColumn {
                column: "ROWID".to_string()
            })
        );
        assert_eq!(
            plan_apply(
                &source,
                &plan_columns(),
                vec!["PERSON".to_string()],
                &[],
                &Dialect::H2,
            ),
            Err(PlanError::NoKey)
        );

        // A product that spells the key one way in the metadata and another in
        // the result set is still keyed, not keyless.
        let batch = plan_apply(
            &source,
            &plan_columns(),
            vec!["PERSON".to_string()],
            &["id".to_string()],
            &Dialect::H2,
        )
        .expect("the case-insensitive pass found it");
        assert_eq!(batch[0].sql, "UPDATE PERSON SET NAME = ? WHERE ID = ?");
    }

    /// A row of nothing but defaults is the dialect's empty insert, and it
    /// binds nothing for the confirmation to list.
    #[test]
    fn an_insert_of_nothing_but_defaults_still_plans() {
        let mut source = plan_source();
        source.edits_mut().add_insert(3);

        let batch = plan(&source).expect("the edits plan");
        assert_eq!(batch[0].sql, "INSERT INTO APP.PERSON DEFAULT VALUES");
        assert!(batch[0].params.is_empty());
        assert!(batch[0].values.is_empty());
        assert!(!batch[0].checked, "an INSERT reaches no existing row");
    }

    /// Nothing staged is nothing to send.
    #[test]
    fn an_empty_buffer_plans_no_statements() {
        assert_eq!(plan(&plan_source()), Ok(Vec::new()));
    }

    /// Past the end answers rather than panicking: the grid can ask about a row
    /// a discard took away between the layout and the paint.
    #[test]
    fn a_row_that_is_no_longer_there_answers_quietly() {
        let mut source = source(true);
        assert_eq!(source.cell(9, 0), GridCell::Null);
        assert_eq!(source.row_status(9), RowStatus::Unchanged);
        assert!(!source.cell_dirty(9, 0));
        assert!(!source.cell_editable(9, 0));

        // And staging into one records nothing at all.
        source.stage(9, 0, StagedCell::Text("x".into()));
        assert!(source.edits().is_empty());
        assert!(!source.edits_mut().discard_row(9, 2));
    }
}
