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
//! # Where this is going
//!
//! [`EditSet`] is shaped to fall into `rudbman_sql::TableEdits` with no
//! rearranging: [`EditSet::deleted`] is one key lookup per entry away from
//! `deletes`, [`EditSet::changed`] groups by row into `updates`, and
//! [`EditSet::inserted`] is already one `Vec` per row with one cell per column,
//! which is `inserts` once [`StagedCell`] is mapped onto `InsertCell` — `Unset`
//! to `Unset`, the other two to `Set`. Generating and applying that is the
//! milestone after this one; nothing here sends anything.

use std::collections::{BTreeSet, HashMap};

use rudbman_grid::{GridCell, GridColumn, GridSource, GridSourceState, RowStatus};
use rudbman_jdbc::ColumnInfo;

use crate::query_source::ResultSource;

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
}

/// `ResultSetMetaData.columnNoNulls`, the one answer that forbids NULL.
const COLUMN_NO_NULLS: i32 = 0;

impl ColumnRules {
    fn of(column: &ColumnInfo) -> Self {
        Self {
            writable: !column.read_only && !column.auto_increment,
            nullable: column.nullable != COLUMN_NO_NULLS,
        }
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
    fn original_text(&self, row: usize, column: usize) -> Option<&str> {
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
