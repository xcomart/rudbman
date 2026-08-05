//! Which cells are picked.
//!
//! Pure arithmetic over row and column numbers — no window, no source, no
//! colours — so the awkward half of a grid's behaviour can be settled where it
//! is cheap to test. A click, a shift-click, a ctrl-click, a drag, a row header
//! and `Ctrl+A` all end up calling one of five methods on [`Selection`], and
//! everything the widget draws or copies is read back out of it.
//!
//! ## Columns are counted as they are seen
//!
//! A cell's row is the source's row, but its column is the column's *display*
//! position: the nth column of those currently on screen, left to right. Hiding
//! a column therefore renumbers the ones after it, and the widget clears the
//! selection when that happens rather than leaving it pointing somewhere it was
//! never put. The alternative — addressing by source column, so that a
//! rectangle survives a hide — would mean a "rectangle" that draws with a hole
//! in it, which is worse.

use std::ops::RangeInclusive;

/// One cell, addressed the way the user sees it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub struct CellAddress {
    /// The row, counting from the top of the result.
    pub row: usize,
    /// The column's display position, counting visible columns from the left.
    pub column: usize,
}

impl CellAddress {
    /// The cell at `row` and `column`.
    pub fn new(row: usize, column: usize) -> Self {
        Self { row, column }
    }
}

/// A rectangle of cells, both corners included.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CellRange {
    /// First row, included.
    pub top: usize,
    /// Last row, included.
    pub bottom: usize,
    /// First column, included.
    pub left: usize,
    /// Last column, included.
    pub right: usize,
}

impl CellRange {
    /// The rectangle two cells span between them, whichever way round they are.
    pub fn between(a: CellAddress, b: CellAddress) -> Self {
        Self {
            top: a.row.min(b.row),
            bottom: a.row.max(b.row),
            left: a.column.min(b.column),
            right: a.column.max(b.column),
        }
    }

    /// The one-cell rectangle at `cell`.
    pub fn single(cell: CellAddress) -> Self {
        Self::between(cell, cell)
    }

    /// Whether the rectangle covers `row` and `column`.
    pub fn contains(&self, row: usize, column: usize) -> bool {
        (self.top..=self.bottom).contains(&row) && (self.left..=self.right).contains(&column)
    }

    /// The rows it spans.
    pub fn rows(&self) -> RangeInclusive<usize> {
        self.top..=self.bottom
    }

    /// The columns it spans.
    pub fn columns(&self) -> RangeInclusive<usize> {
        self.left..=self.right
    }

    /// The smallest rectangle covering both.
    fn union(self, other: Self) -> Self {
        Self {
            top: self.top.min(other.top),
            bottom: self.bottom.max(other.bottom),
            left: self.left.min(other.left),
            right: self.right.max(other.right),
        }
    }
}

/// The picked cells, and where the keyboard is.
///
/// Several rectangles, because `Ctrl`-click adds one without disturbing the
/// ones already there. The *anchor* is the corner a `Shift` gesture pivots
/// around and the *cursor* is the cell the arrow keys move; a plain click puts
/// both on the same cell, which is why the first `Shift`-click after one
/// stretches from where the user clicked rather than from wherever the
/// selection happens to start.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Selection {
    ranges: Vec<CellRange>,
    anchor: Option<CellAddress>,
    cursor: Option<CellAddress>,
}

impl Selection {
    /// Nothing picked.
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether nothing at all is picked.
    pub fn is_empty(&self) -> bool {
        self.ranges.is_empty()
    }

    /// The rectangles, in the order they were added.
    pub fn ranges(&self) -> &[CellRange] {
        &self.ranges
    }

    /// The cell the arrow keys move, if there is one.
    pub fn cursor(&self) -> Option<CellAddress> {
        self.cursor
    }

    /// The corner a `Shift` gesture pivots around, if there is one.
    pub fn anchor(&self) -> Option<CellAddress> {
        self.anchor
    }

    /// Whether `row` and `column` are picked.
    pub fn contains(&self, row: usize, column: usize) -> bool {
        self.ranges.iter().any(|range| range.contains(row, column))
    }

    /// The smallest rectangle covering everything picked, or `None` when
    /// nothing is.
    ///
    /// What a copy runs over: a selection made of several rectangles has no
    /// shape a spreadsheet could paste, so the cells inside this box but outside
    /// the selection are copied as nulls. See [`crate::copy`].
    pub fn bounds(&self) -> Option<CellRange> {
        self.ranges
            .iter()
            .copied()
            .reduce(|whole, range| whole.union(range))
    }

    /// Forgets everything.
    pub fn clear(&mut self) {
        self.ranges.clear();
        self.anchor = None;
        self.cursor = None;
    }

    /// Picks exactly `cell`, dropping whatever was picked before.
    ///
    /// A plain click, and the landing of every unshifted arrow key.
    pub fn replace(&mut self, cell: CellAddress) {
        self.ranges.clear();
        self.ranges.push(CellRange::single(cell));
        self.anchor = Some(cell);
        self.cursor = Some(cell);
    }

    /// Adds `cell` to what is already picked, and pivots around it from now on.
    ///
    /// A `Ctrl`-click. The new rectangle is the one a following `Shift` gesture
    /// stretches, so ctrl-click-then-shift-click adds a block rather than
    /// redrawing the last one.
    pub fn add(&mut self, cell: CellAddress) {
        self.ranges.push(CellRange::single(cell));
        self.anchor = Some(cell);
        self.cursor = Some(cell);
    }

    /// Stretches the newest rectangle from the anchor out to `cell`.
    ///
    /// A `Shift`-click, a `Shift`-arrow and every pixel of a drag. With nothing
    /// picked yet it is the same as [`Selection::replace`], so a `Shift`-click
    /// into an untouched grid picks one cell instead of nothing.
    pub fn extend_to(&mut self, cell: CellAddress) {
        let Some(anchor) = self.anchor else {
            self.replace(cell);
            return;
        };
        let stretched = CellRange::between(anchor, cell);
        match self.ranges.last_mut() {
            Some(last) => *last = stretched,
            None => self.ranges.push(stretched),
        }
        self.cursor = Some(cell);
    }

    /// Picks whole rows, from the row header.
    ///
    /// `columns` is how many columns are on screen; a row selection is the full
    /// width of those, so it copies as a row rather than as whatever happened to
    /// be picked before.
    pub fn replace_rows(&mut self, rows: RangeInclusive<usize>, columns: usize) {
        self.ranges.clear();
        self.push_rows(rows, columns);
    }

    /// Adds whole rows to what is already picked — a `Ctrl`-click on a row
    /// header.
    pub fn add_rows(&mut self, rows: RangeInclusive<usize>, columns: usize) {
        self.push_rows(rows, columns);
    }

    /// Picks everything: `Ctrl+A`.
    pub fn select_all(&mut self, rows: usize, columns: usize) {
        self.ranges.clear();
        if rows == 0 || columns == 0 {
            self.anchor = None;
            self.cursor = None;
            return;
        }
        self.ranges.push(CellRange {
            top: 0,
            bottom: rows - 1,
            left: 0,
            right: columns - 1,
        });
        self.anchor = Some(CellAddress::new(0, 0));
        self.cursor = Some(CellAddress::new(0, 0));
    }

    /// Drops any part of the selection that has fallen off the end of a source
    /// that shrank, and forgets it entirely when nothing is left.
    ///
    /// Called when the host replaces the result under the grid. A selection
    /// hanging over rows that are gone would copy them.
    pub fn clamp(&mut self, rows: usize, columns: usize) {
        if rows == 0 || columns == 0 {
            self.clear();
            return;
        }
        let (last_row, last_column) = (rows - 1, columns - 1);
        self.ranges
            .retain(|range| range.top <= last_row && range.left <= last_column);
        for range in &mut self.ranges {
            range.bottom = range.bottom.min(last_row);
            range.right = range.right.min(last_column);
        }
        if self.ranges.is_empty() {
            self.clear();
            return;
        }
        for cell in [&mut self.anchor, &mut self.cursor].into_iter().flatten() {
            cell.row = cell.row.min(last_row);
            cell.column = cell.column.min(last_column);
        }
    }

    /// Adds one rectangle spanning `rows` across every column, and pivots on
    /// its first cell.
    fn push_rows(&mut self, rows: RangeInclusive<usize>, columns: usize) {
        if columns == 0 || rows.is_empty() {
            return;
        }
        let (top, bottom) = (*rows.start(), *rows.end());
        self.ranges.push(CellRange {
            top,
            bottom,
            left: 0,
            right: columns - 1,
        });
        self.anchor = Some(CellAddress::new(top, 0));
        self.cursor = Some(CellAddress::new(top, 0));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The cells a selection covers, so that a test can state the shape it
    /// expects instead of the rectangles that happen to make it up.
    fn covered(selection: &Selection, rows: usize, columns: usize) -> Vec<(usize, usize)> {
        (0..rows)
            .flat_map(|row| (0..columns).map(move |column| (row, column)))
            .filter(|(row, column)| selection.contains(*row, *column))
            .collect()
    }

    /// A click picks one cell and drops whatever was there.
    #[test]
    fn a_click_picks_one_cell() {
        let mut selection = Selection::new();
        assert!(selection.is_empty());

        selection.replace(CellAddress::new(1, 2));
        assert_eq!(covered(&selection, 4, 4), vec![(1, 2)]);
        assert_eq!(selection.cursor(), Some(CellAddress::new(1, 2)));
        assert_eq!(selection.anchor(), Some(CellAddress::new(1, 2)));

        selection.replace(CellAddress::new(3, 0));
        assert_eq!(covered(&selection, 4, 4), vec![(3, 0)]);
    }

    /// Shift stretches a rectangle from the anchor, and stretches it again
    /// rather than adding another: dragging back is not two selections.
    #[test]
    fn shift_stretches_a_rectangle_from_the_anchor() {
        let mut selection = Selection::new();
        selection.replace(CellAddress::new(1, 1));
        selection.extend_to(CellAddress::new(2, 2));

        assert_eq!(
            covered(&selection, 4, 4),
            vec![(1, 1), (1, 2), (2, 1), (2, 2)]
        );
        assert_eq!(selection.ranges().len(), 1);
        assert_eq!(selection.cursor(), Some(CellAddress::new(2, 2)));
        assert_eq!(
            selection.anchor(),
            Some(CellAddress::new(1, 1)),
            "the pivot moved with the drag"
        );

        // Back past the anchor: the rectangle is the box between the two, which
        // is what makes a drag up and to the left behave like one down and to
        // the right.
        selection.extend_to(CellAddress::new(0, 0));
        assert_eq!(
            covered(&selection, 4, 4),
            vec![(0, 0), (0, 1), (1, 0), (1, 1)]
        );
        assert_eq!(selection.ranges().len(), 1);
    }

    /// Shift into an untouched grid picks the cell rather than nothing.
    #[test]
    fn shift_with_no_anchor_picks_one_cell() {
        let mut selection = Selection::new();
        selection.extend_to(CellAddress::new(2, 3));

        assert_eq!(covered(&selection, 4, 4), vec![(2, 3)]);
    }

    /// Ctrl adds a block without disturbing the ones already picked, and the
    /// shift that follows stretches the new one.
    #[test]
    fn ctrl_adds_a_block_and_shift_stretches_the_new_one() {
        let mut selection = Selection::new();
        selection.replace(CellAddress::new(0, 0));
        selection.add(CellAddress::new(2, 2));
        assert_eq!(covered(&selection, 4, 4), vec![(0, 0), (2, 2)]);

        selection.extend_to(CellAddress::new(3, 3));
        assert_eq!(
            covered(&selection, 4, 4),
            vec![(0, 0), (2, 2), (2, 3), (3, 2), (3, 3)],
            "the first block was disturbed"
        );
        assert_eq!(selection.ranges().len(), 2);
    }

    /// A row header picks the whole width, whatever was picked before.
    #[test]
    fn a_row_header_picks_the_whole_row() {
        let mut selection = Selection::new();
        selection.replace(CellAddress::new(3, 3));
        selection.replace_rows(1..=1, 3);

        assert_eq!(covered(&selection, 4, 3), vec![(1, 0), (1, 1), (1, 2)]);
        assert_eq!(selection.cursor(), Some(CellAddress::new(1, 0)));

        selection.add_rows(3..=3, 3);
        assert_eq!(
            covered(&selection, 4, 3),
            vec![(1, 0), (1, 1), (1, 2), (3, 0), (3, 1), (3, 2)]
        );
    }

    /// Select-all is one rectangle over everything, and over nothing when there
    /// is nothing.
    #[test]
    fn select_all_covers_everything_or_nothing() {
        let mut selection = Selection::new();
        selection.select_all(2, 2);
        assert_eq!(
            covered(&selection, 2, 2),
            vec![(0, 0), (0, 1), (1, 0), (1, 1)]
        );
        assert_eq!(selection.ranges().len(), 1);

        selection.select_all(0, 5);
        assert!(selection.is_empty());
        assert_eq!(selection.cursor(), None);
    }

    /// The copied box is the union of the blocks, holes and all — the shape a
    /// spreadsheet has to be handed.
    #[test]
    fn the_bounds_are_the_union_of_the_blocks() {
        let mut selection = Selection::new();
        assert_eq!(selection.bounds(), None);

        selection.replace(CellAddress::new(1, 1));
        selection.add(CellAddress::new(3, 0));
        assert_eq!(
            selection.bounds(),
            Some(CellRange {
                top: 1,
                bottom: 3,
                left: 0,
                right: 1
            })
        );
        assert!(!selection.contains(1, 0), "the hole was filled in");
    }

    /// A result the host replaced with a smaller one leaves nothing hanging
    /// over the end.
    #[test]
    fn a_shrinking_result_pulls_the_selection_back_in() {
        let mut selection = Selection::new();
        selection.select_all(10, 4);
        selection.clamp(3, 2);

        assert_eq!(
            selection.bounds(),
            Some(CellRange {
                top: 0,
                bottom: 2,
                left: 0,
                right: 1
            })
        );
        assert_eq!(selection.cursor(), Some(CellAddress::new(0, 0)));

        // A block entirely past the end goes rather than being squashed onto
        // the last row.
        let mut past = Selection::new();
        past.replace(CellAddress::new(0, 0));
        past.add(CellAddress::new(9, 0));
        past.clamp(3, 2);
        assert_eq!(past.ranges().len(), 1);
        assert_eq!(covered(&past, 3, 2), vec![(0, 0)]);

        past.clamp(0, 0);
        assert!(past.is_empty());
    }
}
