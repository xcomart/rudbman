//! The widget: a million rows, drawn a screenful at a time.
//!
//! ## Both axes are virtualised
//!
//! Rows go through gpui's [`uniform_list`], which lays out only what the
//! viewport can reach — the same machinery the tree uses, and the reason a
//! result of any length costs the same to draw.
//!
//! Columns are virtualised here, by hand, because there is no `uniform_list`
//! for them and tables with several hundred columns are real. Every column's
//! left edge is kept in a list the grid rebuilds when a width changes, so the run
//! the content area can see is two binary searches; the rest are neither shaped
//! nor painted, and
//! a row is drawn as one absolutely positioned strip slid left by the scroll
//! offset rather than as a flex row of every cell with the invisible ones
//! clipped. Nothing per frame is proportional to the number of rows or to the
//! number of columns — only to the number of both that fit on screen.
//!
//! The horizontal offset is the grid's own field rather than a gpui scroll
//! container's, for the same reason: a scroll container lays its content out in
//! full, which is exactly the cost being avoided. It also makes the header and
//! the body trivially agree — they read the same number.
//!
//! ## What is measured, and when
//!
//! Which columns are visible depends on how wide the content area is, and that
//! is only known once gpui has laid the frame out. A [`canvas`] in the body
//! reports the size during prepaint and asks for a repaint when it changed, so
//! a resize — and the very first frame — costs one extra frame and nothing
//! after that. The overlay scrollbars already trail a resize by a frame for the
//! same reason.
//!
//! ## What the grid asks the host to do
//!
//! Four things, all of them round trips the widget has no business making:
//! fetching the next batch ([`GridEvent::NearEnd`]), re-running the query in a
//! different order ([`GridEvent::SortRequested`] — the grid never sorts what it
//! holds, because it holds only the first n rows of an answer the server has all
//! of), opening a cell ([`GridEvent::CellActivated`], which is how a LOB
//! reaches a viewer), and drawing the right-click menu
//! ([`GridEvent::ContextMenu`] — the grid has no strings to name items with,
//! architecture document §7.8). Copying is *not* among them: gpui owns the
//! clipboard and the grid owns the selection, so the grid does it itself.

use std::ops::Range;

use gpui::{
    AnyElement, App, Bounds, ClickEvent, ClipboardItem, Context, CursorStyle, DragMoveEvent,
    ElementId, EventEmitter, FocusHandle, Focusable, IsZero, KeyBinding, MouseButton,
    MouseDownEvent, MouseMoveEvent, MouseUpEvent, Pixels, Point, ScrollHandle, ScrollStrategy,
    ScrollWheelEvent, SharedString, Size, UniformListScrollHandle, Window, actions, canvas, div,
    point, prelude::*, px, size, uniform_list,
};
use rudbman_ui::scrollbar::{
    DraggedThumb, Scrollbar, ScrollbarAxis, ScrollbarState, hide_later, hide_now, scroll_to,
    scrolled,
};
use rudbman_ui::theme::{Theme, theme, window_translucent};
use unicode_width::UnicodeWidthStr;

use crate::copy::{CopyFormat, DEFAULT_INSERT_TABLE, copy_payload};
use crate::selection::{CellAddress, Selection};
use crate::source::{
    GridCell, GridColumnAlign, GridSource, GridSourceState, NULL_TEXT, cell_label, lob_label,
};

actions!(
    rudbman_grid,
    [
        /// Move the cursor one row up.
        MoveUp,
        /// Move the cursor one row down.
        MoveDown,
        /// Move the cursor one column left.
        MoveLeft,
        /// Move the cursor one column right.
        MoveRight,
        /// Stretch the selection one row up.
        ExtendUp,
        /// Stretch the selection one row down.
        ExtendDown,
        /// Stretch the selection one column left.
        ExtendLeft,
        /// Stretch the selection one column right.
        ExtendRight,
        /// Move to the first column of the current row.
        MoveRowStart,
        /// Move to the last column of the current row.
        MoveRowEnd,
        /// Move to the very first cell.
        MoveFirst,
        /// Move to the very last cell.
        MoveLast,
        /// Move the cursor up by one screenful.
        PageUp,
        /// Move the cursor down by one screenful.
        PageDown,
        /// Stretch the selection up by one screenful.
        ExtendPageUp,
        /// Stretch the selection down by one screenful.
        ExtendPageDown,
        /// Select every cell.
        SelectAll,
        /// Copy the selection as TSV.
        CopyCells,
        /// Open the cell under the cursor, which is what a double click does.
        Activate,
    ]
);

/// Key context that [`init`] binds the keys above to.
const KEY_CONTEXT: &str = "GridView";

/// Height of one body row, and therefore the unit [`uniform_list`] measures in.
const ROW_HEIGHT: f32 = 24.;

/// Height of the column header band.
const HEADER_HEIGHT: f32 = 26.;

/// Width of the row-number gutter down the left-hand edge.
const GUTTER_WIDTH: f32 = 56.;

/// Padding at both ends of a cell.
const CELL_PADDING: f32 = 6.;

/// Width a column is given before anyone has dragged it.
const DEFAULT_COLUMN_WIDTH: f32 = 140.;

/// Narrowest a column may be dragged.
///
/// Not zero: a column dragged shut could not be found again, since the grip is
/// on its right-hand edge.
const MIN_COLUMN_WIDTH: f32 = 32.;

/// Widest a column may be made by *fitting* it.
///
/// A dragged column has no cap — the user can see what they are doing — but a
/// double click on a `TEXT` column would otherwise fit it to a paragraph.
const MAX_AUTOFIT_WIDTH: f32 = 480.;

/// Width of the invisible strip on a column's edge that answers a resize drag.
const GRIP_WIDTH: f32 = 6.;

/// Roughly how wide one character cell is at the grid's text size.
///
/// Auto-fit measures in character cells ([`UnicodeWidthStr`]) and multiplies,
/// rather than shaping the text: shaping several hundred sampled values to size
/// one column would cost more than the column is worth, and being a few pixels
/// out only means the user drags it afterwards — which they can.
const APPROX_ADVANCE: f32 = 7.2;

/// How many rows short of the end the next batch is asked for.
///
/// Asked for *before* the bottom is reached, and by a margin: a fetch that
/// starts when the last row appears has already lost, because the scroll stops
/// while it runs. With the default batch of 500 rows (architecture document,
/// §7.5) this leaves a fifth of a batch of runway.
const NEAR_END_ROWS: usize = 100;

/// How many rows auto-fit looks at.
///
/// The first `n`, not all of them: fitting a column of a million rows would
/// have to read a million values, and the first screenful or two is what the
/// user is looking at anyway.
const AUTOFIT_SAMPLE: usize = 500;

/// Marker drawn in the header of an ascending column.
const SORT_ASCENDING: &str = "\u{25b4}";

/// Marker drawn in the header of a descending column.
const SORT_DESCENDING: &str = "\u{25be}";

/// Registers the key bindings every [`GridView`] relies on.
///
/// Scoped to the `GridView` key context, so the arrows and the clipboard chords
/// keep meaning what they mean everywhere else in the app.
pub fn init(cx: &mut App) {
    let modifier = if cfg!(target_os = "macos") {
        "cmd"
    } else {
        "ctrl"
    };

    cx.bind_keys([
        KeyBinding::new("up", MoveUp, Some(KEY_CONTEXT)),
        KeyBinding::new("down", MoveDown, Some(KEY_CONTEXT)),
        KeyBinding::new("left", MoveLeft, Some(KEY_CONTEXT)),
        KeyBinding::new("right", MoveRight, Some(KEY_CONTEXT)),
        KeyBinding::new("shift-up", ExtendUp, Some(KEY_CONTEXT)),
        KeyBinding::new("shift-down", ExtendDown, Some(KEY_CONTEXT)),
        KeyBinding::new("shift-left", ExtendLeft, Some(KEY_CONTEXT)),
        KeyBinding::new("shift-right", ExtendRight, Some(KEY_CONTEXT)),
        KeyBinding::new("home", MoveRowStart, Some(KEY_CONTEXT)),
        KeyBinding::new("end", MoveRowEnd, Some(KEY_CONTEXT)),
        KeyBinding::new(&format!("{modifier}-home"), MoveFirst, Some(KEY_CONTEXT)),
        KeyBinding::new(&format!("{modifier}-end"), MoveLast, Some(KEY_CONTEXT)),
        KeyBinding::new("pageup", PageUp, Some(KEY_CONTEXT)),
        KeyBinding::new("pagedown", PageDown, Some(KEY_CONTEXT)),
        KeyBinding::new("shift-pageup", ExtendPageUp, Some(KEY_CONTEXT)),
        KeyBinding::new("shift-pagedown", ExtendPageDown, Some(KEY_CONTEXT)),
        KeyBinding::new(&format!("{modifier}-a"), SelectAll, Some(KEY_CONTEXT)),
        KeyBinding::new(&format!("{modifier}-c"), CopyCells, Some(KEY_CONTEXT)),
        KeyBinding::new("enter", Activate, Some(KEY_CONTEXT)),
    ]);
}

/// Which way a column is ordered.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SortDirection {
    /// `ORDER BY … ASC`.
    Ascending,
    /// `ORDER BY … DESC`.
    Descending,
}

/// What a right click landed on, so that the host knows which menu to draw.
///
/// The grid does not name the items and does not run them: it says where the
/// press was and what was under it, and the host — which owns the strings and
/// the commands — does the rest (architecture document, §7.8).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MenuTarget {
    /// The body: a cell, or a row number in the gutter.
    ///
    /// Which cells the menu acts on is [`GridView::selection`], not this — a
    /// right click inside the selection leaves it alone, so the pressed cell is
    /// not necessarily the interesting one.
    Cell,
    /// A column heading.
    Header {
        /// The source column, unaffected by hiding or by column widths.
        column: usize,
    },
}

/// What the grid asks its host for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GridEvent {
    /// The viewport has come within a hundred rows of the last row the source
    /// holds, and the source said there are more.
    ///
    /// Raised once per row count: the host fetches the next batch, drops it into
    /// its source, and the grid — now looking at a longer result — asks again
    /// when the new end comes into view. A burst of scrolling that never reaches
    /// new rows asks once.
    NearEnd,
    /// The user clicked a column header, and wants the query re-run in that
    /// order.
    ///
    /// `direction` is `None` for the third click, which drops the ordering
    /// altogether. The grid does not sort: it holds the first `n` rows of a
    /// result the server holds all of, so sorting what is here would put the
    /// wrong rows at the top (architecture document, §7.5). The host re-runs
    /// with a new `ORDER BY` and replaces the source; until it does, the grid
    /// goes on showing the old order under the new marker.
    SortRequested {
        /// The source column index, unaffected by hiding or by column widths.
        column: usize,
        /// The order asked for, or `None` to drop the ordering.
        direction: Option<SortDirection>,
    },
    /// A cell was double clicked or `Enter` was pressed on it.
    ///
    /// How a LOB reaches its viewer, and later how a cell reaches the editor.
    CellActivated {
        /// The row.
        row: usize,
        /// The source column index.
        column: usize,
    },
    /// The user right clicked, and wants the menu for what is under the
    /// pointer.
    ///
    /// The grid has already taken the focus and moved the selection if it had
    /// to; what is left — deciding which items exist, what they are called,
    /// which are greyed out and what they do — is the host's, because this
    /// layer holds no strings (architecture document, §7.8). Everything such a
    /// menu needs is on [`GridView`] already: [`GridView::copy`],
    /// [`GridView::select_all`], [`GridView::clear_selection`],
    /// [`GridView::toggle_sort`], [`GridView::set_column_hidden`],
    /// [`GridView::show_all_columns`], [`GridView::autofit_column`], and
    /// [`GridView::sort`], [`GridView::is_column_hidden`],
    /// [`GridView::hidden_column_count`], [`GridView::column_name`] to label
    /// and disable them.
    ContextMenu {
        /// What was under the pointer.
        target: MenuTarget,
        /// Where the pointer was, in **window** coordinates, which is what the
        /// menu anchors to.
        position: Point<Pixels>,
    },
}

/// What has been done to one column.
///
/// Indexed by *source* column, so hiding one does not renumber the rest and a
/// width survives a hide. Reordering, when it lands, becomes an order vector
/// beside this one rather than a permutation of it, for exactly that reason.
#[derive(Clone, Copy, Debug)]
struct ColumnState {
    width: f32,
    hidden: bool,
    // TODO(M3): `pinned: bool` — a pinned column is drawn in the gutter's strip
    // rather than in the scrolling one, so it never leaves the screen.
}

/// One column's place along the header, worked out from the widths.
///
/// Only the columns that are showing are in this list, and the index into it is
/// a column's *display* position — which is what the selection is written in
/// (see [`crate::selection`]).
#[derive(Clone, Copy, Debug)]
struct Placed {
    /// The source column this is.
    column: usize,
    /// Its left edge, measured from the left of the first column.
    x: f32,
}

/// A resize drag in progress.
#[derive(Clone, Copy, Debug)]
struct Resize {
    /// The source column being dragged.
    column: usize,
    /// Where the pointer was when it took hold.
    from: Pixels,
    /// How wide the column was then, so that the drag is absolute rather than a
    /// running total that could drift.
    width: f32,
}

/// What the pointer landed on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Hit {
    /// The row-number gutter, on the given row.
    Gutter(usize),
    /// A cell.
    Cell(CellAddress),
}

/// A result set, drawn a screenful at a time.
///
/// Created as an entity and rendered as a child element, like the tree:
///
/// ```ignore
/// let grid = cx.new(|cx| GridView::new(Results::default(), cx));
/// cx.subscribe(&grid, |view, grid, event, cx| match event {
///     GridEvent::NearEnd => view.fetch_more(cx),
///     GridEvent::SortRequested { column, direction } => view.reorder(*column, *direction, cx),
///     GridEvent::CellActivated { row, column } => view.open_cell(*row, *column, cx),
///     GridEvent::ContextMenu { target, position } => view.open_menu(*target, *position, cx),
/// })
/// .detach();
/// ```
pub struct GridView<S: GridSource> {
    source: S,
    focus_handle: FocusHandle,
    /// One entry per source column, in source order.
    columns: Vec<ColumnState>,
    /// The showing columns and their left edges, in display order.
    laid_out: Vec<Placed>,
    /// How wide every showing column is, together.
    total_width: f32,
    /// How far the columns are scrolled sideways, counting up from the left.
    h_offset: f32,
    /// How wide the content area is, as of the last frame that measured it.
    viewport_width: f32,
    selection: Selection,
    /// The column the host has been asked to order by, if any.
    sort: Option<(usize, SortDirection)>,
    /// The row count the last [`GridEvent::NearEnd`] was raised at, which is
    /// what keeps a burst of scrolling from raising a fetch per frame.
    asked_at: Option<usize>,
    /// The rows [`uniform_list`] built last frame, which is both what "near the
    /// end" is measured against and what a page key moves by.
    visible_rows: Range<usize>,
    /// The table name written into a copied `INSERT`.
    insert_table: Option<SharedString>,
    resizing: Option<Resize>,
    /// Whether the pointer is dragging a selection out.
    dragging: bool,
    scroll: UniformListScrollHandle,
    v_bar: ScrollbarState,
    h_bar: ScrollbarState,
    v_bar_id: ElementId,
    h_bar_id: ElementId,
}

impl<S: GridSource> GridView<S> {
    /// A grid over `source`, with nothing selected and nothing sorted.
    pub fn new(source: S, cx: &mut Context<Self>) -> Self {
        let mut grid = Self {
            source,
            focus_handle: cx.focus_handle(),
            columns: Vec::new(),
            laid_out: Vec::new(),
            total_width: 0.,
            h_offset: 0.,
            viewport_width: 0.,
            selection: Selection::new(),
            sort: None,
            asked_at: None,
            visible_rows: 0..0,
            insert_table: None,
            resizing: None,
            dragging: false,
            scroll: UniformListScrollHandle::new(),
            v_bar: ScrollbarState::new(),
            h_bar: ScrollbarState::new(),
            v_bar_id: ElementId::from(("rudbman-grid-vbar", cx.entity_id())),
            h_bar_id: ElementId::from(("rudbman-grid-hbar", cx.entity_id())),
        };
        grid.ensure_layout();
        grid
    }

    /// Places the grid at `index` in the window's tab order.
    pub fn tab_index(mut self, index: isize) -> Self {
        self.focus_handle = self.focus_handle.clone().tab_index(index).tab_stop(true);
        self
    }

    /// Sets the table name written into a copied `INSERT`.
    ///
    /// Without one, [`DEFAULT_INSERT_TABLE`] is used — a name that will not
    /// parse, on purpose.
    pub fn insert_table(mut self, table: impl Into<SharedString>) -> Self {
        self.insert_table = Some(table.into());
        self
    }

    /// Sets the table name written into a copied `INSERT`, after the fact.
    pub fn set_insert_table(&mut self, table: Option<SharedString>) {
        self.insert_table = table;
    }

    /// The source, to read.
    pub fn source(&self) -> &S {
        &self.source
    }

    /// The source, to change — dropping a fetched batch in, most of the time.
    ///
    /// Re-reads the shape on the next draw, so the caller has nothing to
    /// remember.
    pub fn source_mut(&mut self, cx: &mut Context<Self>) -> &mut S {
        cx.notify();
        &mut self.source
    }

    /// Re-reads the source, for a change the grid cannot have seen.
    pub fn refresh(&mut self, cx: &mut Context<Self>) {
        self.ensure_layout();
        cx.notify();
    }

    /// Throws away everything the user has done to the columns and the
    /// selection, which is what a *new* result — as opposed to another batch of
    /// the same one — deserves.
    pub fn reset(&mut self, cx: &mut Context<Self>) {
        self.columns.clear();
        self.laid_out.clear();
        self.selection.clear();
        self.sort = None;
        self.asked_at = None;
        self.h_offset = 0.;
        self.scroll.scroll_to_item(0, ScrollStrategy::Top);
        self.ensure_layout();
        cx.notify();
    }

    /// What is selected.
    pub fn selection(&self) -> &Selection {
        &self.selection
    }

    /// Whether the cell at `row` and display position `column` is selected.
    pub fn is_selected(&self, row: usize, column: usize) -> bool {
        self.selection.contains(row, column)
    }

    /// The column the host has been asked to order by, and which way.
    pub fn sort(&self) -> Option<(usize, SortDirection)> {
        self.sort
    }

    /// The rows [`uniform_list`] built for the last frame.
    ///
    /// What the "only the visible rows are touched" guarantee is stated in, and
    /// what a page key moves by.
    pub fn visible_rows(&self) -> Range<usize> {
        self.visible_rows.clone()
    }

    /// The source columns that are showing, left to right.
    ///
    /// The index into this is a cell's display column, which is how the
    /// selection and [`GridView::is_selected`] address one.
    pub fn visible_column_indices(&self) -> Vec<usize> {
        self.laid_out.iter().map(|placed| placed.column).collect()
    }

    /// How wide `column` is, in pixels.
    pub fn column_width(&self, column: usize) -> f32 {
        self.columns
            .get(column)
            .map_or(DEFAULT_COLUMN_WIDTH, |state| state.width)
    }

    /// Sets how wide `column` is, clamped to something that can still be found
    /// and dragged.
    pub fn set_column_width(&mut self, column: usize, width: f32, cx: &mut Context<Self>) {
        self.ensure_layout();
        let Some(state) = self.columns.get_mut(column) else {
            return;
        };
        let width = width.max(MIN_COLUMN_WIDTH);
        if state.width == width {
            return;
        }
        state.width = width;
        self.relayout();
        self.clamp_h_offset();
        cx.notify();
    }

    /// Whether `column` is hidden.
    pub fn is_column_hidden(&self, column: usize) -> bool {
        self.columns.get(column).is_some_and(|state| state.hidden)
    }

    /// How many columns are hidden.
    ///
    /// What tells a host's menu whether "show every column" is worth offering:
    /// zero means there is nothing to show.
    pub fn hidden_column_count(&self) -> usize {
        self.columns.iter().filter(|state| state.hidden).count()
    }

    /// The name of source column `column`, or `None` when there is no such
    /// column.
    ///
    /// The grid draws this in the heading; a host menu labels its items with it
    /// — "hide *ORDER_ID*" — and copies it.
    pub fn column_name(&self, column: usize) -> Option<&str> {
        (column < self.source.column_count()).then(|| self.source.column(column).name)
    }

    /// Hides or shows `column`.
    ///
    /// Clears the selection: display positions are what a selection is written
    /// in, and hiding a column renumbers every one after it (see
    /// [`crate::selection`]).
    pub fn set_column_hidden(&mut self, column: usize, hidden: bool, cx: &mut Context<Self>) {
        self.ensure_layout();
        let Some(state) = self.columns.get_mut(column) else {
            return;
        };
        if state.hidden == hidden {
            return;
        }
        state.hidden = hidden;
        self.relayout();
        self.clamp_h_offset();
        self.selection.clear();
        cx.notify();
    }

    /// Un-hides every column.
    ///
    /// The way back from [`GridView::set_column_hidden`], and the one thing a
    /// header menu needs that no other gesture offers: a hidden column has no
    /// heading to right click. Clears the selection for the same reason hiding
    /// one does — every display position after the first restored column moves.
    pub fn show_all_columns(&mut self, cx: &mut Context<Self>) {
        self.ensure_layout();
        if self.hidden_column_count() == 0 {
            return;
        }
        for state in &mut self.columns {
            state.hidden = false;
        }
        self.relayout();
        self.clamp_h_offset();
        self.selection.clear();
        cx.notify();
    }

    /// Widens or narrows `column` to fit what is in it.
    ///
    /// What a double click on the resize grip does. Only the first few hundred
    /// rows are looked at — see `AUTOFIT_SAMPLE`.
    pub fn autofit_column(&mut self, column: usize, cx: &mut Context<Self>) {
        self.ensure_layout();
        if column >= self.columns.len() {
            return;
        }

        // Two cells of headroom on the header, for the sort marker that appears
        // when the column is ordered by.
        let mut cells = UnicodeWidthStr::width(self.source.column(column).name) + 2;
        let rows = self.source.row_count().min(AUTOFIT_SAMPLE);
        for row in 0..rows {
            let width = match self.source.cell(row, column) {
                GridCell::Null => NULL_TEXT.width(),
                GridCell::Text(text) => text.width(),
                GridCell::Lob { size } => lob_label(size).width(),
            };
            cells = cells.max(width);
        }

        let width = cells as f32 * APPROX_ADVANCE + CELL_PADDING * 2.;
        self.set_column_width(column, width.min(MAX_AUTOFIT_WIDTH), cx);
    }

    /// Walks the sort of `column` on one step: ascending, descending, none.
    ///
    /// What a header click does. Raises [`GridEvent::SortRequested`] and moves
    /// the marker; the rows do not move until the host re-runs the query.
    pub fn toggle_sort(&mut self, column: usize, cx: &mut Context<Self>) {
        let direction = match self.sort {
            Some((sorted, SortDirection::Ascending)) if sorted == column => {
                Some(SortDirection::Descending)
            }
            Some((sorted, SortDirection::Descending)) if sorted == column => None,
            _ => Some(SortDirection::Ascending),
        };
        self.sort = direction.map(|direction| (column, direction));
        cx.emit(GridEvent::SortRequested { column, direction });
        cx.notify();
    }

    /// Puts the marker where the host says the result is really ordered,
    /// without asking for anything.
    ///
    /// For a host that ordered the query itself — a table opened with a default
    /// `ORDER BY`, say — so that the header agrees with the rows.
    pub fn set_sort(&mut self, sort: Option<(usize, SortDirection)>, cx: &mut Context<Self>) {
        self.sort = sort;
        cx.notify();
    }

    /// Picks the cell at `row` and display position `column`, dropping whatever
    /// was picked.
    pub fn select_cell(&mut self, row: usize, column: usize, cx: &mut Context<Self>) {
        self.ensure_layout();
        let Some(cell) = self.clamped(row, column) else {
            return;
        };
        self.selection.replace(cell);
        self.reveal(cell);
        cx.notify();
    }

    /// Stretches the selection out to the cell at `row` and `column`.
    pub fn extend_selection(&mut self, row: usize, column: usize, cx: &mut Context<Self>) {
        self.ensure_layout();
        let Some(cell) = self.clamped(row, column) else {
            return;
        };
        self.selection.extend_to(cell);
        self.reveal(cell);
        cx.notify();
    }

    /// Picks a whole row, as a click on its row number does.
    pub fn select_row(&mut self, row: usize, cx: &mut Context<Self>) {
        self.ensure_layout();
        if row >= self.source.row_count() {
            return;
        }
        self.selection.replace_rows(row..=row, self.laid_out.len());
        self.scroll.scroll_to_item(row, ScrollStrategy::Top);
        cx.notify();
    }

    /// Picks everything.
    pub fn select_all(&mut self, cx: &mut Context<Self>) {
        self.ensure_layout();
        self.selection
            .select_all(self.source.row_count(), self.laid_out.len());
        cx.notify();
    }

    /// Drops the selection.
    pub fn clear_selection(&mut self, cx: &mut Context<Self>) {
        self.selection.clear();
        cx.notify();
    }

    /// Writes the selection to the clipboard in `format`.
    ///
    /// Nothing selected writes nothing at all, rather than blanking the
    /// clipboard. See [`crate::copy`] for what each format does with a null.
    pub fn copy(&mut self, format: CopyFormat, cx: &mut Context<Self>) {
        self.ensure_layout();
        let columns = self.visible_column_indices();
        let table = self
            .insert_table
            .as_ref()
            .map_or(DEFAULT_INSERT_TABLE, |table| table.as_ref());
        let text = copy_payload(&self.source, &columns, &self.selection, format, table);
        if text.is_empty() {
            return;
        }
        cx.write_to_clipboard(ClipboardItem::new_string(text));
    }

    /// Brings `row` into view.
    pub fn scroll_to_row(&mut self, row: usize, cx: &mut Context<Self>) {
        self.scroll.scroll_to_item(row, ScrollStrategy::Top);
        cx.notify();
    }

    // TODO(M3): cell editing. A cell of a single-table query with a primary key
    // becomes an `UPDATE`; the widget needs an edit mode, a per-cell dirty mark
    // and a `GridEvent::CellEdited`. Nothing here forecloses it — the selection
    // already names a cell, and `GridSource` already says which columns are key
    // columns.
    //
    // TODO(M3): a filter row under the header, and pinned columns. The filter
    // row is one more fixed band drawn like the header; a pinned column is one
    // drawn in the gutter's strip instead of the scrolling one, which is why
    // `ColumnState` is indexed by source column and the strip's offset is a
    // single field.

    /// Rebuilds the column list when the source has a different number of them
    /// than the grid last saw.
    ///
    /// The whole of "the host replaced the result": widths, hidden flags and the
    /// selection are all keyed to a shape that no longer holds.
    fn ensure_layout(&mut self) {
        let count = self.source.column_count();
        if self.columns.len() == count {
            self.selection
                .clamp(self.source.row_count(), self.laid_out.len());
            return;
        }

        self.columns = vec![
            ColumnState {
                width: DEFAULT_COLUMN_WIDTH,
                hidden: false,
            };
            count
        ];
        self.selection.clear();
        self.h_offset = 0.;
        self.relayout();
    }

    /// Works out where every showing column starts.
    fn relayout(&mut self) {
        let mut laid_out = std::mem::take(&mut self.laid_out);
        laid_out.clear();

        let mut x = 0.;
        for (column, state) in self.columns.iter().enumerate() {
            if state.hidden {
                continue;
            }
            laid_out.push(Placed { column, x });
            x += state.width;
        }

        self.laid_out = laid_out;
        self.total_width = x;
    }

    /// The run of columns the content area can show.
    ///
    /// Two binary searches over the left edges, which is why several hundred
    /// columns cost nothing: the ones off either side are never looked at again.
    fn visible_columns(&self) -> Range<usize> {
        if self.laid_out.is_empty() {
            return 0..0;
        }
        // The viewport is measured by the body's canvas during prepaint, which
        // is after the header for this frame was already built — and the
        // notify that measurement issues does not buy a second frame for an
        // entity that has just been drawn. On that first frame the header must
        // draw every column, clipped by its container, or it stays empty until
        // something else happens to invalidate the grid.
        if self.viewport_width <= 0. {
            return 0..self.laid_out.len();
        }
        let left = self.h_offset;
        let right = left + self.viewport_width;
        let first = self
            .laid_out
            .partition_point(|placed| placed.x + self.column_width(placed.column) <= left);
        let last = self.laid_out.partition_point(|placed| placed.x < right);
        first..last.max(first)
    }

    /// How far the columns could be scrolled sideways.
    fn max_h_offset(&self) -> f32 {
        (self.total_width - self.viewport_width).max(0.)
    }

    /// Pulls the sideways offset back into range, after a resize or a hide.
    fn clamp_h_offset(&mut self) {
        self.h_offset = self.h_offset.clamp(0., self.max_h_offset());
    }

    /// Scrolls the columns sideways.
    fn set_h_offset(&mut self, offset: f32, cx: &mut Context<Self>) {
        let offset = offset.clamp(0., self.max_h_offset());
        if offset == self.h_offset {
            return;
        }
        self.h_offset = offset;
        cx.notify();
    }

    /// Notes how wide the content area turned out to be.
    ///
    /// Called from the body's [`canvas`] during prepaint. Asks for another frame
    /// when the width changed, because the header was drawn against the old one
    /// — see the module docs.
    fn measured(&mut self, area: Size<Pixels>, cx: &mut Context<Self>) {
        let width = (f32::from(area.width) - GUTTER_WIDTH).max(0.);
        if (width - self.viewport_width).abs() < 0.5 {
            return;
        }
        self.viewport_width = width;
        self.clamp_h_offset();
        cx.notify();
    }

    /// Notes which rows the list built, and asks for the next batch when the
    /// end is in sight.
    fn note_visible(&mut self, rows: Range<usize>, cx: &mut Context<Self>) {
        self.visible_rows = rows;

        let count = self.source.row_count();
        if self.source.state() != GridSourceState::HasMore {
            // A source that has stopped growing — or is already fetching —
            // forgets the request, so that the next time it says `HasMore` it
            // is asked afresh.
            self.asked_at = None;
            return;
        }
        if self.visible_rows.end + NEAR_END_ROWS < count {
            return;
        }
        if self.asked_at == Some(count) {
            return;
        }
        self.asked_at = Some(count);
        cx.emit(GridEvent::NearEnd);
    }

    /// `row` and `column` as a cell, or `None` when there is no such cell.
    fn clamped(&self, row: usize, column: usize) -> Option<CellAddress> {
        (row < self.source.row_count() && column < self.laid_out.len())
            .then_some(CellAddress::new(row, column))
    }

    /// Brings `cell` into view on both axes.
    fn reveal(&mut self, cell: CellAddress) {
        self.scroll.scroll_to_item(cell.row, ScrollStrategy::Top);
        let Some(placed) = self.laid_out.get(cell.column).copied() else {
            return;
        };
        if self.viewport_width <= 0. {
            return;
        }

        let left = placed.x;
        let right = left + self.column_width(placed.column);
        if left < self.h_offset {
            self.h_offset = left;
        } else if right > self.h_offset + self.viewport_width {
            self.h_offset = right - self.viewport_width;
        }
        self.clamp_h_offset();
    }

    /// How many rows a page key moves by.
    ///
    /// One short of a screenful, so that the row that was at the bottom is at
    /// the top afterwards and the user has something to hold on to.
    fn page(&self) -> usize {
        self.visible_rows.len().saturating_sub(1).max(1)
    }

    /// Moves the cursor by `rows` and `columns`, stretching the selection or
    /// replacing it.
    fn step(&mut self, rows: isize, columns: isize, extend: bool, cx: &mut Context<Self>) {
        self.ensure_layout();
        let (last_row, last_column) = match (
            self.source.row_count().checked_sub(1),
            self.laid_out.len().checked_sub(1),
        ) {
            (Some(row), Some(column)) => (row, column),
            _ => return,
        };

        // Nothing picked yet: the first keystroke lands on the first cell rather
        // than one step away from it.
        let cell = match self.selection.cursor() {
            None => CellAddress::new(0, 0),
            Some(cursor) => CellAddress::new(
                offset(cursor.row, rows, last_row),
                offset(cursor.column, columns, last_column),
            ),
        };

        if extend {
            self.selection.extend_to(cell);
        } else {
            self.selection.replace(cell);
        }
        self.reveal(cell);
        cx.notify();
    }

    /// Moves the cursor to an absolute cell.
    fn jump(&mut self, row: usize, column: usize, cx: &mut Context<Self>) {
        self.ensure_layout();
        let Some(cell) = self.clamped(row, column) else {
            return;
        };
        self.selection.replace(cell);
        self.reveal(cell);
        cx.notify();
    }

    fn move_up(&mut self, _: &MoveUp, _: &mut Window, cx: &mut Context<Self>) {
        self.step(-1, 0, false, cx);
    }

    fn move_down(&mut self, _: &MoveDown, _: &mut Window, cx: &mut Context<Self>) {
        self.step(1, 0, false, cx);
    }

    fn move_left(&mut self, _: &MoveLeft, _: &mut Window, cx: &mut Context<Self>) {
        self.step(0, -1, false, cx);
    }

    fn move_right(&mut self, _: &MoveRight, _: &mut Window, cx: &mut Context<Self>) {
        self.step(0, 1, false, cx);
    }

    fn extend_up(&mut self, _: &ExtendUp, _: &mut Window, cx: &mut Context<Self>) {
        self.step(-1, 0, true, cx);
    }

    fn extend_down(&mut self, _: &ExtendDown, _: &mut Window, cx: &mut Context<Self>) {
        self.step(1, 0, true, cx);
    }

    fn extend_left(&mut self, _: &ExtendLeft, _: &mut Window, cx: &mut Context<Self>) {
        self.step(0, -1, true, cx);
    }

    fn extend_right(&mut self, _: &ExtendRight, _: &mut Window, cx: &mut Context<Self>) {
        self.step(0, 1, true, cx);
    }

    fn page_up(&mut self, _: &PageUp, _: &mut Window, cx: &mut Context<Self>) {
        let page = self.page() as isize;
        self.step(-page, 0, false, cx);
    }

    fn page_down(&mut self, _: &PageDown, _: &mut Window, cx: &mut Context<Self>) {
        let page = self.page() as isize;
        self.step(page, 0, false, cx);
    }

    fn extend_page_up(&mut self, _: &ExtendPageUp, _: &mut Window, cx: &mut Context<Self>) {
        let page = self.page() as isize;
        self.step(-page, 0, true, cx);
    }

    fn extend_page_down(&mut self, _: &ExtendPageDown, _: &mut Window, cx: &mut Context<Self>) {
        let page = self.page() as isize;
        self.step(page, 0, true, cx);
    }

    fn move_row_start(&mut self, _: &MoveRowStart, _: &mut Window, cx: &mut Context<Self>) {
        let row = self.selection.cursor().map_or(0, |cursor| cursor.row);
        self.jump(row, 0, cx);
    }

    fn move_row_end(&mut self, _: &MoveRowEnd, _: &mut Window, cx: &mut Context<Self>) {
        let row = self.selection.cursor().map_or(0, |cursor| cursor.row);
        let column = self.laid_out.len().saturating_sub(1);
        self.jump(row, column, cx);
    }

    fn move_first(&mut self, _: &MoveFirst, _: &mut Window, cx: &mut Context<Self>) {
        self.jump(0, 0, cx);
    }

    fn move_last(&mut self, _: &MoveLast, _: &mut Window, cx: &mut Context<Self>) {
        let row = self.source.row_count().saturating_sub(1);
        let column = self.laid_out.len().saturating_sub(1);
        self.jump(row, column, cx);
    }

    fn select_everything(&mut self, _: &SelectAll, _: &mut Window, cx: &mut Context<Self>) {
        self.select_all(cx);
    }

    fn copy_selection(&mut self, _: &CopyCells, _: &mut Window, cx: &mut Context<Self>) {
        self.copy(CopyFormat::Tsv, cx);
    }

    fn activate(&mut self, _: &Activate, _: &mut Window, cx: &mut Context<Self>) {
        let Some(cursor) = self.selection.cursor() else {
            return;
        };
        let Some(placed) = self.laid_out.get(cursor.column) else {
            return;
        };
        cx.emit(GridEvent::CellActivated {
            row: cursor.row,
            column: placed.column,
        });
    }

    /// What the pointer is over, worked out from the grid's own geometry.
    ///
    /// Done arithmetically rather than with a listener per cell: a cell that
    /// answers presses needs an id and a hitbox, and a screenful of them is
    /// several hundred of both, every frame, for a gesture that can be resolved
    /// from four numbers.
    fn hit(&self, position: Point<Pixels>) -> Option<Hit> {
        let body = self.base_handle().bounds();
        if body.size.width <= px(0.) || !body.contains(&position) {
            return None;
        }

        let scrolled_by = f32::from(self.base_handle().offset().y);
        let local_x = f32::from(position.x - body.origin.x);
        let content_y = f32::from(position.y - body.origin.y) - scrolled_by;
        if content_y < 0. {
            return None;
        }

        let row = (content_y / ROW_HEIGHT) as usize;
        if row >= self.source.row_count() {
            return None;
        }
        if local_x < GUTTER_WIDTH {
            return Some(Hit::Gutter(row));
        }

        let x = local_x - GUTTER_WIDTH + self.h_offset;
        let display = self
            .laid_out
            .partition_point(|placed| placed.x + self.column_width(placed.column) <= x);
        let placed = self.laid_out.get(display)?;
        (x >= placed.x).then_some(Hit::Cell(CellAddress::new(row, display)))
    }

    fn on_mouse_down(
        &mut self,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.ensure_layout();
        let Some(hit) = self.hit(event.position) else {
            return;
        };
        self.focus_handle.focus(window);

        let columns = self.laid_out.len();
        match hit {
            Hit::Gutter(row) => {
                if event.modifiers.shift {
                    // From the pivot, which a row selection puts on its top row
                    // — so a shift-click below the block grows it and one above
                    // it redraws from where the block started.
                    let anchor = self.selection.anchor().map_or(row, |cell| cell.row);
                    self.selection
                        .replace_rows(anchor.min(row)..=anchor.max(row), columns);
                } else if event.modifiers.secondary() {
                    self.selection.add_rows(row..=row, columns);
                } else {
                    self.selection.replace_rows(row..=row, columns);
                }
            }
            Hit::Cell(cell) => {
                if event.modifiers.shift {
                    self.selection.extend_to(cell);
                } else if event.modifiers.secondary() {
                    self.selection.add(cell);
                } else {
                    self.selection.replace(cell);
                }
                self.dragging = true;

                if event.click_count >= 2
                    && let Some(placed) = self.laid_out.get(cell.column)
                {
                    cx.emit(GridEvent::CellActivated {
                        row: cell.row,
                        column: placed.column,
                    });
                }
            }
        }
        cx.notify();
    }

    /// A right click on a column heading, from the heading itself or from its
    /// resize grip.
    ///
    /// Takes the focus — the menu's items act on the grid, so the keys should
    /// too afterwards — and leaves the selection exactly as it was: a header
    /// menu is about the column, and "hide this column" would be a strange
    /// thing to have just cleared the selection for.
    fn on_header_menu(
        &mut self,
        column: usize,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        cx.stop_propagation();
        self.focus_handle.focus(window);
        cx.emit(GridEvent::ContextMenu {
            target: MenuTarget::Header { column },
            position: event.position,
        });
    }

    /// A right click in the body: move the selection if the press fell outside
    /// it, then hand the gesture to the host.
    ///
    /// The selection rule is the one every grid and file list uses, and the one
    /// §7.8 states: a press *inside* what is picked leaves it alone — otherwise
    /// "copy" on a block of a hundred cells would copy one — and a press
    /// outside picks what was pressed, so the menu is never about something the
    /// user cannot see. A press in the gutter picks the whole row, exactly as a
    /// left one does.
    ///
    /// Nothing else happens: no drag is started, and no
    /// [`GridEvent::CellActivated`] is raised however many times the button is
    /// clicked.
    fn on_right_mouse_down(
        &mut self,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.ensure_layout();
        let Some(hit) = self.hit(event.position) else {
            return;
        };
        self.focus_handle.focus(window);
        cx.stop_propagation();

        let columns = self.laid_out.len();
        match hit {
            Hit::Gutter(row) => {
                let picked = (0..columns).any(|column| self.selection.contains(row, column));
                if !picked {
                    self.selection.replace_rows(row..=row, columns);
                }
            }
            Hit::Cell(cell) => {
                if !self.selection.contains(cell.row, cell.column) {
                    self.selection.replace(cell);
                }
            }
        }

        cx.emit(GridEvent::ContextMenu {
            target: MenuTarget::Cell,
            position: event.position,
        });
        cx.notify();
    }

    fn on_mouse_move(&mut self, event: &MouseMoveEvent, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(resize) = self.resizing {
            let width = resize.width + f32::from(event.position.x - resize.from);
            self.set_column_width(resize.column, width, cx);
            return;
        }
        if !self.dragging || event.pressed_button != Some(MouseButton::Left) {
            return;
        }
        if let Some(Hit::Cell(cell)) = self.hit(event.position) {
            self.selection.extend_to(cell);
            cx.notify();
        }
    }

    fn on_mouse_up(&mut self, _: &MouseUpEvent, _: &mut Window, cx: &mut Context<Self>) {
        self.dragging = false;
        self.resizing = None;
        if let Some(epoch) = self.v_bar.release() {
            hide_later(epoch, cx, |grid: &mut Self| Some(&mut grid.v_bar));
        }
        if let Some(epoch) = self.h_bar.release() {
            hide_later(epoch, cx, |grid: &mut Self| Some(&mut grid.h_bar));
        }
        cx.notify();
    }

    fn on_scroll_wheel(
        &mut self,
        event: &ScrollWheelEvent,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let delta = event.delta.pixel_delta(px(ROW_HEIGHT));
        // A plain mouse has no sideways wheel, so `Shift` folds the vertical one
        // onto the horizontal axis — the convention every other scrolling
        // surface uses.
        let sideways = if delta.x.is_zero() && event.modifiers.shift {
            delta.y
        } else {
            delta.x
        };
        if sideways.is_zero() {
            return;
        }
        self.set_h_offset(self.h_offset - f32::from(sideways), cx);
    }

    /// The scroll container behind the list, which is what the vertical bar
    /// measures and what the pointer arithmetic is done against.
    fn base_handle(&self) -> ScrollHandle {
        self.scroll.0.borrow().base_handle.clone()
    }

    /// The vertical bar as it stands this frame.
    fn vertical_bar(&self) -> Scrollbar {
        Scrollbar::for_handle(
            self.v_bar_id.clone(),
            ScrollbarAxis::Vertical,
            &self.base_handle(),
        )
        .fade(self.v_bar.fade())
    }

    /// The horizontal bar as it stands this frame.
    ///
    /// Built from the grid's own numbers rather than from a scroll handle,
    /// because the columns are not in a scroll container: its track is the
    /// content area, which is the body less the gutter.
    fn horizontal_bar(&self) -> Scrollbar {
        let body = self.base_handle().bounds();
        let track = Bounds::new(
            body.origin + point(px(GUTTER_WIDTH), px(0.)),
            size(
                (body.size.width - px(GUTTER_WIDTH)).max(px(0.)),
                body.size.height,
            ),
        );
        Scrollbar::new(
            self.h_bar_id.clone(),
            ScrollbarAxis::Horizontal,
            track,
            self.viewport_width,
            self.max_h_offset(),
            self.h_offset,
        )
        .fade(self.h_bar.fade())
    }

    /// The state of whichever bar rides `axis`.
    fn bar_mut(&mut self, axis: ScrollbarAxis) -> &mut ScrollbarState {
        match axis {
            ScrollbarAxis::Vertical => &mut self.v_bar,
            ScrollbarAxis::Horizontal => &mut self.h_bar,
        }
    }

    /// Puts a bar up while the pointer rests on the edge it rides, and starts
    /// it going the moment the pointer leaves.
    fn hover_bar(&mut self, axis: ScrollbarAxis, hovered: bool, cx: &mut Context<Self>) {
        if hovered {
            if self.bar_mut(axis).hover_enter() {
                cx.notify();
            }
            return;
        }

        if let Some(epoch) = self.bar_mut(axis).hover_leave() {
            hide_now(self, epoch, cx, move |grid: &mut Self| {
                Some(grid.bar_mut(axis))
            });
        }
    }

    /// Draws the fixed header band.
    fn render_header(&self, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let visible = self.visible_columns();
        let start = self
            .laid_out
            .get(visible.start)
            .map_or(0., |placed| placed.x);

        let cells: Vec<AnyElement> = visible
            .map(|display| self.render_heading(display, theme, cx))
            .collect();

        div()
            .flex()
            .flex_row()
            .flex_none()
            .h(px(HEADER_HEIGHT))
            .w_full()
            .bg(theme.grid_header)
            .border_b_1()
            .border_color(theme.border)
            .child(
                div()
                    .id("grid-corner")
                    .flex_none()
                    .w(px(GUTTER_WIDTH))
                    .h_full()
                    .border_r_1()
                    .border_color(theme.border)
                    .cursor_pointer()
                    .on_click(cx.listener(|grid, _: &ClickEvent, window, cx| {
                        grid.focus_handle.focus(window);
                        grid.select_all(cx);
                    })),
            )
            .child(
                div()
                    .relative()
                    .flex_grow()
                    .h_full()
                    .overflow_hidden()
                    .child(
                        div()
                            .absolute()
                            .top_0()
                            .h_full()
                            .left(px(start - self.h_offset))
                            .flex()
                            .flex_row()
                            .children(cells),
                    ),
            )
            .into_any_element()
    }

    /// Draws one column heading, with its sort marker and its resize grip.
    fn render_heading(&self, display: usize, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let placed = self.laid_out[display];
        let column = self.source.column(placed.column);
        let marker = self.sort.and_then(|(sorted, direction)| {
            (sorted == placed.column).then_some(match direction {
                SortDirection::Ascending => SORT_ASCENDING,
                SortDirection::Descending => SORT_DESCENDING,
            })
        });
        let source_column = placed.column;

        div()
            .id(ElementId::from(("grid-heading", display)))
            .relative()
            .flex_none()
            .w(px(self.column_width(source_column)))
            .h_full()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(4.))
            .px(px(CELL_PADDING))
            .border_r_1()
            .border_color(theme.border)
            .cursor_pointer()
            // The one thing the primary key gets: its own colour, on the header
            // and nowhere else. A key icon would need a font this layer does not
            // pick.
            .text_color(if column.primary_key {
                theme.grid_pk
            } else {
                theme.text
            })
            .on_click(cx.listener(move |grid, _: &ClickEvent, window, cx| {
                grid.focus_handle.focus(window);
                grid.toggle_sort(source_column, cx);
            }))
            // A right click on a heading is a menu about that column and does
            // not re-sort it, so it does not go through `on_click`. The header
            // band is its own element tree above the body, which the body's
            // arithmetic hit test does not cover — hence a listener here rather
            // than another branch in `hit`.
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(move |grid, event: &MouseDownEvent, window, cx| {
                    grid.on_header_menu(source_column, event, window, cx);
                }),
            )
            .child(
                div()
                    .flex_1()
                    .truncate()
                    .when(column.align == GridColumnAlign::Right, |label| {
                        label.text_right()
                    })
                    .child(SharedString::from(column.name.to_string())),
            )
            .children(marker.map(|marker| {
                div()
                    .flex_none()
                    .text_size(px(8.))
                    .text_color(theme.accent)
                    .child(marker)
            }))
            .child(
                div()
                    .id(ElementId::from(("grid-grip", display)))
                    .absolute()
                    .top_0()
                    .bottom_0()
                    .right(px(-GRIP_WIDTH / 2.))
                    .w(px(GRIP_WIDTH))
                    // Occluding is what keeps the press off the heading
                    // underneath, so that grabbing the edge of a column does not
                    // also re-sort it.
                    .occlude()
                    .cursor(CursorStyle::ResizeLeftRight)
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |grid, event: &MouseDownEvent, _window, cx| {
                            cx.stop_propagation();
                            if event.click_count >= 2 {
                                grid.autofit_column(source_column, cx);
                            } else {
                                grid.resizing = Some(Resize {
                                    column: source_column,
                                    from: event.position.x,
                                    width: grid.column_width(source_column),
                                });
                            }
                        }),
                    )
                    // Occluding keeps the heading underneath from seeing the
                    // press at all, so the grip has to raise the menu itself —
                    // otherwise the last few pixels of every heading would be
                    // the one part of the header with no menu.
                    .on_mouse_down(
                        MouseButton::Right,
                        cx.listener(move |grid, event: &MouseDownEvent, window, cx| {
                            grid.on_header_menu(source_column, event, window, cx);
                        }),
                    ),
            )
            .into_any_element()
    }

    /// Draws one body row: the number in the gutter, and the strip of cells the
    /// content area can see.
    fn render_row(&self, row: usize, theme: &Theme) -> AnyElement {
        let visible = self.visible_columns();
        let start = self
            .laid_out
            .get(visible.start)
            .map_or(0., |placed| placed.x);
        let cells: Vec<AnyElement> = visible
            .map(|display| self.render_cell(row, display, theme))
            .collect();

        div()
            .flex()
            .flex_row()
            .items_center()
            .h(px(ROW_HEIGHT))
            .w_full()
            // Zebra striping is a hint and nothing more; see the token's docs.
            .when(row % 2 == 1, |stripe| stripe.bg(theme.grid_row_alt))
            .child(
                div()
                    .flex_none()
                    .w(px(GUTTER_WIDTH))
                    .h_full()
                    .flex()
                    .items_center()
                    .justify_end()
                    .px(px(CELL_PADDING))
                    .bg(theme.grid_header)
                    .border_r_1()
                    .border_b_1()
                    .border_color(theme.border)
                    .text_color(theme.text_muted)
                    .child(SharedString::from((row + 1).to_string())),
            )
            .child(
                div()
                    .relative()
                    .flex_grow()
                    .h_full()
                    .overflow_hidden()
                    .child(
                        div()
                            .absolute()
                            .top_0()
                            .h_full()
                            .left(px(start - self.h_offset))
                            .flex()
                            .flex_row()
                            .children(cells),
                    ),
            )
            .into_any_element()
    }

    /// Draws one cell.
    ///
    /// Plain divs with no id and no listeners: the whole of the pointer
    /// behaviour is [`GridView::hit`], so a cell is only a box with text in it.
    fn render_cell(&self, row: usize, display: usize, theme: &Theme) -> AnyElement {
        let placed = self.laid_out[display];
        let column = self.source.column(placed.column);
        let label = cell_label(&self.source.cell(row, placed.column));
        let selected = self.selection.contains(row, display);
        let cursor = self.selection.cursor() == Some(CellAddress::new(row, display));

        div()
            .relative()
            .flex_none()
            .w(px(self.column_width(placed.column)))
            .h_full()
            .flex()
            .items_center()
            .when(column.align == GridColumnAlign::Right, |cell| {
                cell.justify_end()
            })
            .px(px(CELL_PADDING))
            .border_r_1()
            .border_b_1()
            .border_color(theme.border)
            .when(selected, |cell| cell.bg(theme.grid_selection))
            .when(label.muted, |cell| cell.text_color(theme.grid_null))
            .child(div().truncate().child(label.text))
            // The cursor outline is a child rather than a border, so that the
            // cell it is on stays exactly as wide as the others and the text
            // under it does not shift by a pixel as the cursor arrives.
            .when(cursor, |cell| {
                cell.child(
                    div()
                        .absolute()
                        .top_0()
                        .bottom_0()
                        .left_0()
                        .right_0()
                        .border_1()
                        .border_color(theme.accent),
                )
            })
            .into_any_element()
    }
}

/// `base` moved by `step`, kept inside `0..=last`.
fn offset(base: usize, step: isize, last: usize) -> usize {
    let moved = base as isize + step;
    moved.clamp(0, last as isize) as usize
}

impl<S: GridSource> EventEmitter<GridEvent> for GridView<S> {}

impl<S: GridSource> Focusable for GridView<S> {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl<S: GridSource> Render for GridView<S> {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.ensure_layout();
        let palette = theme(cx);
        let rows = self.source.row_count();
        let grid = cx.entity();

        // Both bars, wired as every scrolling surface in the app wires one:
        // notice the surface moved, and arm the expiry from inside the draw that
        // noticed.
        if let Some(epoch) = self
            .v_bar
            .moved(scrolled(&self.base_handle(), ScrollbarAxis::Vertical))
        {
            hide_later(epoch, cx, |grid: &mut Self| Some(&mut grid.v_bar));
        }
        if let Some(epoch) = self.h_bar.moved(self.h_offset) {
            hide_later(epoch, cx, |grid: &mut Self| Some(&mut grid.h_bar));
        }

        let measure = {
            let grid = grid.clone();
            canvas(
                move |bounds, _window, cx| {
                    grid.update(cx, |grid, cx| grid.measured(bounds.size, cx));
                },
                |_, _, _, _| {},
            )
            .absolute()
            .size_full()
        };

        let list = uniform_list("grid-rows", rows, move |range, _window, cx| {
            grid.update(cx, |grid, cx| {
                grid.note_visible(range.clone(), cx);
                let palette = theme(cx);
                range
                    .map(|row| grid.render_row(row, &palette))
                    .collect::<Vec<_>>()
            })
        })
        .track_scroll(self.scroll.clone())
        .size_full();

        let body = div()
            .relative()
            .flex_grow()
            .w_full()
            .overflow_hidden()
            .child(measure)
            .child(list)
            .children(
                self.vertical_bar()
                    .on_hover(cx.listener(|grid, hovered: &bool, _window, cx| {
                        grid.hover_bar(ScrollbarAxis::Vertical, *hovered, cx);
                    }))
                    .render(&palette),
            )
            .child(
                // The horizontal thumb rides the content area rather than the
                // whole body, so its box has to be that area and not the body:
                // `Scrollbar::render` places the thumb against its parent.
                div()
                    .absolute()
                    .left(px(GUTTER_WIDTH))
                    .right_0()
                    .top_0()
                    .bottom_0()
                    .children(
                        self.horizontal_bar()
                            .on_hover(cx.listener(|grid, hovered: &bool, _window, cx| {
                                grid.hover_bar(ScrollbarAxis::Horizontal, *hovered, cx);
                            }))
                            .render(&palette),
                    ),
            );

        div()
            .key_context(KEY_CONTEXT)
            .track_focus(&self.focus_handle)
            .relative()
            .size_full()
            .overflow_hidden()
            // Nothing, while the window is translucent. The pane behind the grid
            // already tints these same pixels with the very same colour, so an
            // opaque fill here would hide the blur and a tinted one would
            // saturate the surface alpha back to opaque; see
            // `app_settings::window_tint`. The header, the row stripes and the
            // selection go on painting — they are accents over the background,
            // not the background.
            .when(!window_translucent(cx), |grid| grid.bg(palette.background))
            .text_size(px(13.))
            .text_color(palette.text)
            .on_action(cx.listener(Self::move_up))
            .on_action(cx.listener(Self::move_down))
            .on_action(cx.listener(Self::move_left))
            .on_action(cx.listener(Self::move_right))
            .on_action(cx.listener(Self::extend_up))
            .on_action(cx.listener(Self::extend_down))
            .on_action(cx.listener(Self::extend_left))
            .on_action(cx.listener(Self::extend_right))
            .on_action(cx.listener(Self::move_row_start))
            .on_action(cx.listener(Self::move_row_end))
            .on_action(cx.listener(Self::move_first))
            .on_action(cx.listener(Self::move_last))
            .on_action(cx.listener(Self::page_up))
            .on_action(cx.listener(Self::page_down))
            .on_action(cx.listener(Self::extend_page_up))
            .on_action(cx.listener(Self::extend_page_down))
            .on_action(cx.listener(Self::select_everything))
            .on_action(cx.listener(Self::copy_selection))
            .on_action(cx.listener(Self::activate))
            .on_mouse_down(MouseButton::Left, cx.listener(Self::on_mouse_down))
            .on_mouse_down(MouseButton::Right, cx.listener(Self::on_right_mouse_down))
            .on_mouse_move(cx.listener(Self::on_mouse_move))
            .on_scroll_wheel(cx.listener(Self::on_scroll_wheel))
            .on_drag_move::<DraggedThumb>(cx.listener(
                |grid, event: &DragMoveEvent<DraggedThumb>, _window, cx| {
                    if let Some(progress) = grid.vertical_bar().dragged(event, cx) {
                        grid.v_bar.hold();
                        scroll_to(&grid.base_handle(), ScrollbarAxis::Vertical, progress);
                        cx.notify();
                    }
                    if let Some(progress) = grid.horizontal_bar().dragged(event, cx) {
                        grid.h_bar.hold();
                        let offset = grid.max_h_offset() * progress;
                        grid.set_h_offset(offset, cx);
                    }
                },
            ))
            // Both halves: a thumb dragged off the end of its track, or a
            // selection dragged out of the window, lets go with the pointer
            // outside, which only the second sees.
            .on_mouse_up(MouseButton::Left, cx.listener(Self::on_mouse_up))
            .on_mouse_up_out(MouseButton::Left, cx.listener(Self::on_mouse_up))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .size_full()
                    .child(self.render_header(&palette, cx))
                    .child(body),
            )
    }
}

#[cfg(test)]
mod tests {
    use std::cell::{Cell, RefCell};
    use std::ops::Deref;
    use std::rc::Rc;

    use gpui::{
        Entity, Modifiers, MouseDownEvent, MouseUpEvent, TestAppContext, VisualTestContext,
    };

    use crate::source::{GridColumn, GridColumnKind};

    use super::*;

    /// The test display, and so the test window, is 1920 by 1080.
    const WINDOW_WIDTH: f32 = 1920.;

    /// How wide the cells have to play with, which is the window less the
    /// gutter.
    const CONTENT_WIDTH: f32 = WINDOW_WIDTH - GUTTER_WIDTH;

    /// The vertical middle of body row `row`, in window coordinates.
    fn row_y(row: usize) -> f32 {
        HEADER_HEIGHT + row as f32 * ROW_HEIGHT + ROW_HEIGHT / 2.
    }

    /// The horizontal middle of display column `column`, in window coordinates,
    /// with the columns at their default width and not scrolled sideways.
    fn column_x(column: usize) -> f32 {
        GUTTER_WIDTH + column as f32 * DEFAULT_COLUMN_WIDTH + DEFAULT_COLUMN_WIDTH / 2.
    }

    /// What a source was asked for, so that "only what is on screen" can be
    /// asserted rather than believed.
    #[derive(Default)]
    struct Probe {
        reads: Cell<usize>,
        max_row: Cell<usize>,
        min_column: Cell<usize>,
        max_column: Cell<usize>,
    }

    impl Probe {
        fn note(&self, row: usize, column: usize) {
            self.reads.set(self.reads.get() + 1);
            self.max_row.set(self.max_row.get().max(row));
            self.min_column.set(self.min_column.get().min(column));
            self.max_column.set(self.max_column.get().max(column));
        }

        fn forget(&self) {
            self.reads.set(0);
            self.max_row.set(0);
            self.min_column.set(usize::MAX);
            self.max_column.set(0);
        }
    }

    /// A result of any size at all, generated rather than stored, that counts
    /// what it was asked for.
    struct Huge {
        rows: Cell<usize>,
        columns: usize,
        state: Cell<GridSourceState>,
        probe: Rc<Probe>,
    }

    impl Huge {
        fn new(rows: usize, columns: usize, probe: Rc<Probe>) -> Self {
            Self {
                rows: Cell::new(rows),
                columns,
                state: Cell::new(GridSourceState::Complete),
                probe,
            }
        }

        fn growing(mut self) -> Self {
            self.state = Cell::new(GridSourceState::HasMore);
            self
        }
    }

    impl GridSource for Huge {
        fn column_count(&self) -> usize {
            self.columns
        }

        fn column(&self, index: usize) -> GridColumn<'_> {
            // A `&'static str` rather than a built one: the point of the fixture
            // is that nothing per row or per column is allocated behind the
            // trait either.
            GridColumn::new("column", GridColumnKind::Text).primary_key(index == 0)
        }

        fn row_count(&self) -> usize {
            self.rows.get()
        }

        fn cell(&self, row: usize, column: usize) -> GridCell<'_> {
            self.probe.note(row, column);
            GridCell::Text("value")
        }

        fn state(&self) -> GridSourceState {
            self.state.get()
        }
    }

    /// A small result written out in full, for the tests that care what is in
    /// the cells rather than how many of them were touched.
    struct Small {
        headings: Vec<(&'static str, GridColumnKind)>,
        rows: Vec<Vec<Option<&'static str>>>,
    }

    impl GridSource for Small {
        fn column_count(&self) -> usize {
            self.headings.len()
        }

        fn column(&self, index: usize) -> GridColumn<'_> {
            let (name, kind) = self.headings[index];
            GridColumn::new(name, kind)
        }

        fn row_count(&self) -> usize {
            self.rows.len()
        }

        fn cell(&self, row: usize, column: usize) -> GridCell<'_> {
            match self.rows[row][column] {
                Some(text) => GridCell::Text(text),
                None => GridCell::Null,
            }
        }
    }

    /// Three columns, two rows, and both of the values that too many tools
    /// cannot tell apart.
    fn null_and_empty() -> Small {
        Small {
            headings: vec![
                ("id", GridColumnKind::Number),
                ("nothing", GridColumnKind::Text),
                ("empty", GridColumnKind::Text),
            ],
            rows: vec![
                vec![Some("1"), None, Some("")],
                vec![Some("2"), Some("here"), Some("")],
            ],
        }
    }

    /// A view that does nothing but hold the grid, as a result panel would.
    struct Harness<S: GridSource> {
        grid: Entity<GridView<S>>,
    }

    impl<S: GridSource> Render for Harness<S> {
        fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
            div().size_full().child(self.grid.clone())
        }
    }

    /// Everything a test reads back: the grid, and what it announced.
    struct Handles<S: GridSource> {
        grid: Entity<GridView<S>>,
        events: Rc<RefCell<Vec<GridEvent>>>,
    }

    impl<S: GridSource> Handles<S> {
        /// Everything announced since the last look.
        fn drain(&self) -> Vec<GridEvent> {
            self.events.borrow_mut().drain(..).collect()
        }

        /// Reads something off the grid.
        fn read<R>(&self, cx: &mut VisualTestContext, f: impl FnOnce(&GridView<S>) -> R) -> R {
            cx.update(|_, cx| f(self.grid.read(cx)))
        }

        /// Changes the grid, and lets the frame it asks for happen.
        fn update(
            &self,
            cx: &mut VisualTestContext,
            f: impl FnOnce(&mut GridView<S>, &mut Context<GridView<S>>),
        ) {
            cx.update(|_, cx| self.grid.update(cx, f));
            cx.run_until_parked();
        }

        /// The cells the selection covers, as `(row, display column)`.
        fn selected(
            &self,
            cx: &mut VisualTestContext,
            rows: usize,
            columns: usize,
        ) -> Vec<(usize, usize)> {
            self.read(cx, |grid| {
                (0..rows)
                    .flat_map(|row| (0..columns).map(move |column| (row, column)))
                    .filter(|(row, column)| grid.is_selected(*row, *column))
                    .collect()
            })
        }
    }

    /// Opens a focused grid over `source` and hands back its handles.
    fn open<S: GridSource>(source: S, cx: &mut TestAppContext) -> (Handles<S>, VisualTestContext) {
        cx.update(rudbman_ui::init);
        cx.update(crate::init);

        let events: Rc<RefCell<Vec<GridEvent>>> = Rc::new(RefCell::new(Vec::new()));
        let window = cx.add_window({
            let events = events.clone();
            move |_, cx| {
                let grid = cx.new(|cx| GridView::new(source, cx));
                cx.subscribe(&grid, move |_: &mut Harness<S>, _, event: &GridEvent, _| {
                    events.borrow_mut().push(*event);
                })
                .detach();
                Harness { grid }
            }
        });
        let grid = window
            .update(cx, |harness, _, _| harness.grid.clone())
            .expect("the window is open");

        let mut cx = VisualTestContext::from_window(*window.deref(), cx);
        cx.update(|window, cx| {
            let handle = grid.read(cx).focus_handle(cx);
            handle.focus(window);
        });
        cx.run_until_parked();

        (Handles { grid, events }, cx)
    }

    /// Presses and releases the left button over a point, with modifiers.
    fn click_at(cx: &mut VisualTestContext, x: f32, y: f32, modifiers: Modifiers, count: usize) {
        let position = point(px(x), px(y));
        cx.simulate_event(MouseDownEvent {
            position,
            modifiers,
            button: MouseButton::Left,
            click_count: count,
            first_mouse: false,
        });
        cx.simulate_event(MouseUpEvent {
            position,
            modifiers,
            button: MouseButton::Left,
            click_count: count,
        });
        cx.run_until_parked();
    }

    /// A plain click on the cell at `row` and display column `column`.
    fn click_cell(cx: &mut VisualTestContext, row: usize, column: usize) {
        click_at(cx, column_x(column), row_y(row), Modifiers::none(), 1);
    }

    /// Presses and releases the right button over a point, and hands back where
    /// it was pressed — which is what the event carries.
    fn right_click_at(cx: &mut VisualTestContext, x: f32, y: f32) -> Point<Pixels> {
        let position = point(px(x), px(y));
        cx.simulate_event(MouseDownEvent {
            position,
            modifiers: Modifiers::none(),
            button: MouseButton::Right,
            click_count: 1,
            first_mouse: false,
        });
        cx.simulate_event(MouseUpEvent {
            position,
            modifiers: Modifiers::none(),
            button: MouseButton::Right,
            click_count: 1,
        });
        cx.run_until_parked();
        position
    }

    /// The claim the whole crate is built around: a million rows and forty
    /// columns cost exactly one screenful of reads per frame, and the reads land
    /// where the viewport is rather than at the start of the result.
    #[gpui::test]
    fn only_the_visible_rows_and_columns_are_read(cx: &mut TestAppContext) {
        let probe = Rc::new(Probe::default());
        let (grid, mut cx) = open(Huge::new(1_000_000, 40, probe.clone()), cx);

        // A screenful is what fits in 1920 by 1080 at the default sizes: about
        // forty rows and thirteen columns. The bound is deliberately loose —
        // what matters is that it does not scale with the million.
        let visible_rows = grid.read(&mut cx, |grid| grid.visible_rows());
        assert!(
            visible_rows.len() < 60,
            "the list built {} rows",
            visible_rows.len()
        );
        assert!(
            (CONTENT_WIDTH / DEFAULT_COLUMN_WIDTH) as usize <= 14,
            "the fixture no longer matches the test window"
        );

        probe.forget();
        grid.update(&mut cx, |grid, cx| grid.refresh(cx));

        // Forty million cells exist; one frame reads six hundred odd of them —
        // 44 rows by 14 columns, plus the row `uniform_list` measures twice to
        // find the row height. The bound is loose on purpose: what must hold is
        // that it is a function of the window and not of the result.
        assert!(
            probe.reads.get() < 2_000,
            "one frame read {} cells",
            probe.reads.get()
        );
        assert!(
            probe.max_row.get() < 60,
            "row {} was read for a viewport of {} rows",
            probe.max_row.get(),
            visible_rows.len()
        );
        assert!(
            probe.max_column.get() < 20,
            "column {} was read of forty",
            probe.max_column.get()
        );

        // And scrolling moves the window of reads rather than widening it: the
        // rows around row 900,000 are read, and none of the ones before them.
        probe.forget();
        grid.update(&mut cx, |grid, cx| grid.scroll_to_row(900_000, cx));
        cx.run_until_parked();

        assert!(
            probe.reads.get() < 2_000,
            "the scrolled frame read {} cells",
            probe.reads.get()
        );
        assert!(
            grid.read(&mut cx, |grid| grid.visible_rows().start) > 800_000,
            "the viewport did not follow the scroll"
        );
        assert!(
            probe.max_row.get() > 800_000,
            "the reads did not follow the viewport"
        );
    }

    /// The frame that has laid columns out but not yet measured the viewport —
    /// the first one, where the header is built before the body's canvas runs —
    /// draws every column rather than none.
    ///
    /// A `VisualTestContext` draws repeatedly, so the ordinary tests never see
    /// this frame: the real app did, as a permanently empty header band,
    /// because the notify issued by the measurement does not buy a second
    /// frame for an entity that was just drawn.
    #[gpui::test]
    fn an_unmeasured_viewport_shows_every_header(cx: &mut TestAppContext) {
        let probe = Rc::new(Probe::default());
        let (grid, mut cx) = open(Huge::new(5, 7, probe), cx);

        grid.update(&mut cx, |grid, _| {
            assert!(!grid.laid_out.is_empty(), "the fixture never laid out");
            grid.viewport_width = 0.;
            assert_eq!(
                grid.visible_columns(),
                0..grid.laid_out.len(),
                "the header of the unmeasured frame"
            );
        });
    }

    /// The next batch is asked for once, not once per frame, and asked for again
    /// only when the answer to the first one has landed.
    #[gpui::test]
    fn the_next_batch_is_asked_for_once(cx: &mut TestAppContext) {
        let probe = Rc::new(Probe::default());
        let (grid, mut cx) = open(Huge::new(20, 3, probe).growing(), cx);

        assert_eq!(
            grid.drain(),
            vec![GridEvent::NearEnd],
            "the end was in sight and nobody was told"
        );

        // A burst of repaints — which is what a fast scroll is — asks for
        // nothing more, because nothing has changed about how much there is.
        for _ in 0..10 {
            grid.update(&mut cx, |grid, cx| grid.refresh(cx));
        }
        assert_eq!(grid.drain(), vec![], "a redraw was mistaken for a scroll");

        // The batch lands: more rows, and the new end is in sight too.
        grid.update(&mut cx, |grid, cx| {
            grid.source_mut(cx).rows.set(60);
        });
        assert_eq!(grid.drain(), vec![GridEvent::NearEnd]);

        // And a source that has everything is never asked again, however often
        // it is redrawn.
        grid.update(&mut cx, |grid, cx| {
            grid.source_mut(cx).state.set(GridSourceState::Complete);
        });
        for _ in 0..5 {
            grid.update(&mut cx, |grid, cx| grid.refresh(cx));
        }
        assert_eq!(grid.drain(), vec![]);
    }

    /// A fetch already in flight is not asked for again either: `Loading` is an
    /// answer, and the request stands until it turns back into `HasMore`.
    #[gpui::test]
    fn a_fetch_in_flight_is_not_asked_for_again(cx: &mut TestAppContext) {
        let probe = Rc::new(Probe::default());
        let (grid, mut cx) = open(Huge::new(20, 3, probe).growing(), cx);
        assert_eq!(grid.drain(), vec![GridEvent::NearEnd]);

        grid.update(&mut cx, |grid, cx| {
            grid.source_mut(cx).state.set(GridSourceState::Loading);
        });
        for _ in 0..5 {
            grid.update(&mut cx, |grid, cx| grid.refresh(cx));
        }
        assert_eq!(grid.drain(), vec![]);
    }

    /// Ascending, descending, gone — and the grid never touches its own rows.
    #[gpui::test]
    fn a_header_click_walks_the_sort_round(cx: &mut TestAppContext) {
        let (grid, mut cx) = open(null_and_empty(), cx);

        grid.update(&mut cx, |grid, cx| grid.toggle_sort(1, cx));
        assert_eq!(
            grid.drain(),
            vec![GridEvent::SortRequested {
                column: 1,
                direction: Some(SortDirection::Ascending)
            }]
        );
        assert_eq!(
            grid.read(&mut cx, |grid| grid.sort()),
            Some((1, SortDirection::Ascending))
        );

        grid.update(&mut cx, |grid, cx| grid.toggle_sort(1, cx));
        assert_eq!(
            grid.drain(),
            vec![GridEvent::SortRequested {
                column: 1,
                direction: Some(SortDirection::Descending)
            }]
        );

        grid.update(&mut cx, |grid, cx| grid.toggle_sort(1, cx));
        assert_eq!(
            grid.drain(),
            vec![GridEvent::SortRequested {
                column: 1,
                direction: None
            }]
        );
        assert_eq!(
            grid.read(&mut cx, |grid| grid.sort()),
            None,
            "the third click left the column ordered"
        );

        // Another column starts its own round from the top rather than picking
        // up where the last one left off.
        grid.update(&mut cx, |grid, cx| grid.toggle_sort(1, cx));
        grid.drain();
        grid.update(&mut cx, |grid, cx| grid.toggle_sort(2, cx));
        assert_eq!(
            grid.read(&mut cx, |grid| grid.sort()),
            Some((2, SortDirection::Ascending))
        );
    }

    /// A click picks a cell; shift stretches a block; ctrl adds one; a row
    /// number takes the whole row.
    #[gpui::test]
    fn the_pointer_picks_cells_blocks_and_rows(cx: &mut TestAppContext) {
        let probe = Rc::new(Probe::default());
        let (grid, mut cx) = open(Huge::new(6, 4, probe), cx);

        click_cell(&mut cx, 1, 1);
        assert_eq!(grid.selected(&mut cx, 6, 4), vec![(1, 1)]);

        click_at(&mut cx, column_x(2), row_y(2), Modifiers::shift(), 1);
        assert_eq!(
            grid.selected(&mut cx, 6, 4),
            vec![(1, 1), (1, 2), (2, 1), (2, 2)]
        );

        click_at(
            &mut cx,
            column_x(0),
            row_y(4),
            Modifiers::secondary_key(),
            1,
        );
        assert_eq!(
            grid.selected(&mut cx, 6, 4),
            vec![(1, 1), (1, 2), (2, 1), (2, 2), (4, 0)]
        );

        // The row-number gutter takes the whole width, and drops the blocks.
        click_at(&mut cx, GUTTER_WIDTH / 2., row_y(3), Modifiers::none(), 1);
        assert_eq!(
            grid.selected(&mut cx, 6, 4),
            vec![(3, 0), (3, 1), (3, 2), (3, 3)]
        );
    }

    /// A right click asks for a menu and moves the selection onto what was
    /// pressed — unless the press was already inside it, which is what keeps a
    /// menu raised over a block from being about one cell of it.
    #[gpui::test]
    fn a_right_click_asks_for_a_menu_and_moves_the_selection(cx: &mut TestAppContext) {
        let probe = Rc::new(Probe::default());
        let (grid, mut cx) = open(Huge::new(6, 4, probe), cx);

        // A block, so that "inside" and "outside" both exist.
        click_cell(&mut cx, 1, 1);
        click_at(&mut cx, column_x(2), row_y(2), Modifiers::shift(), 1);
        grid.drain();

        // Outside it: the selection follows the press.
        let position = right_click_at(&mut cx, column_x(3), row_y(4));
        assert_eq!(grid.selected(&mut cx, 6, 4), vec![(4, 3)]);
        assert_eq!(
            grid.drain(),
            vec![GridEvent::ContextMenu {
                target: MenuTarget::Cell,
                position,
            }],
            "the press was not reported in window coordinates"
        );

        // Inside it: the selection stays whole.
        click_cell(&mut cx, 1, 1);
        click_at(&mut cx, column_x(2), row_y(2), Modifiers::shift(), 1);
        grid.drain();
        let block = grid.selected(&mut cx, 6, 4);
        let position = right_click_at(&mut cx, column_x(2), row_y(1));
        assert_eq!(
            grid.selected(&mut cx, 6, 4),
            block,
            "a right click inside the selection shrank it"
        );
        assert_eq!(
            grid.drain(),
            vec![GridEvent::ContextMenu {
                target: MenuTarget::Cell,
                position,
            }]
        );

        // The gutter takes the whole row, as a left click there does.
        let position = right_click_at(&mut cx, GUTTER_WIDTH / 2., row_y(5));
        assert_eq!(
            grid.selected(&mut cx, 6, 4),
            vec![(5, 0), (5, 1), (5, 2), (5, 3)]
        );
        assert_eq!(
            grid.drain(),
            vec![GridEvent::ContextMenu {
                target: MenuTarget::Cell,
                position,
            }]
        );
    }

    /// A right click on a heading raises the column's menu, names the *source*
    /// column, and does not re-sort what it was pressed on.
    #[gpui::test]
    fn a_right_click_on_a_heading_names_the_source_column(cx: &mut TestAppContext) {
        let probe = Rc::new(Probe::default());
        let (grid, mut cx) = open(Huge::new(6, 4, probe), cx);

        click_cell(&mut cx, 1, 1);
        grid.update(&mut cx, |grid, cx| grid.set_column_hidden(0, true, cx));
        grid.drain();

        // Display column 1 is now source column 2.
        let position = right_click_at(&mut cx, column_x(1), HEADER_HEIGHT / 2.);
        assert_eq!(
            grid.drain(),
            vec![GridEvent::ContextMenu {
                target: MenuTarget::Header { column: 2 },
                position,
            }]
        );
        assert_eq!(
            grid.read(&mut cx, |grid| grid.sort()),
            None,
            "a right click sorted the column"
        );
    }

    /// The way back from hiding: the only column gesture with no heading of its
    /// own to be reached from, so a host menu is the only route to it.
    #[gpui::test]
    fn every_hidden_column_can_be_shown_again(cx: &mut TestAppContext) {
        let (grid, mut cx) = open(null_and_empty(), cx);

        assert_eq!(grid.read(&mut cx, |grid| grid.hidden_column_count()), 0);
        assert_eq!(
            grid.read(&mut cx, |grid| grid.column_name(0).map(str::to_owned)),
            Some("id".to_owned())
        );
        assert_eq!(
            grid.read(&mut cx, |grid| grid.column_name(9).map(str::to_owned)),
            None
        );

        grid.update(&mut cx, |grid, cx| grid.set_column_hidden(0, true, cx));
        grid.update(&mut cx, |grid, cx| grid.set_column_hidden(2, true, cx));
        assert_eq!(grid.read(&mut cx, |grid| grid.hidden_column_count()), 2);
        assert_eq!(
            grid.read(&mut cx, |grid| grid.visible_column_indices()),
            vec![1]
        );

        grid.update(&mut cx, |grid, cx| grid.show_all_columns(cx));
        assert_eq!(grid.read(&mut cx, |grid| grid.hidden_column_count()), 0);
        assert_eq!(
            grid.read(&mut cx, |grid| grid.visible_column_indices()),
            vec![0, 1, 2]
        );
        assert!(
            grid.read(&mut cx, |grid| grid.selection().is_empty()),
            "the display positions moved under the selection"
        );
    }

    /// A double click is how a LOB reaches its viewer, and it names the *source*
    /// column rather than the one the user happens to be looking at.
    #[gpui::test]
    fn a_double_click_activates_the_cell(cx: &mut TestAppContext) {
        let probe = Rc::new(Probe::default());
        let (grid, mut cx) = open(Huge::new(6, 4, probe), cx);

        grid.update(&mut cx, |grid, cx| grid.set_column_hidden(1, true, cx));
        grid.drain();

        // Display column 1 is now source column 2.
        click_at(&mut cx, column_x(1), row_y(2), Modifiers::none(), 2);
        assert_eq!(
            grid.drain(),
            vec![GridEvent::CellActivated { row: 2, column: 2 }]
        );
    }

    /// The arrows walk the cells, shift stretches from where they started, and
    /// `Ctrl+A` takes everything.
    #[gpui::test]
    fn the_keyboard_walks_and_stretches(cx: &mut TestAppContext) {
        let probe = Rc::new(Probe::default());
        let (grid, mut cx) = open(Huge::new(8, 4, probe), cx);

        // Nothing picked yet: the first key lands on the first cell rather than
        // one step away from it.
        cx.simulate_keystrokes("down");
        assert_eq!(grid.selected(&mut cx, 8, 4), vec![(0, 0)]);

        cx.simulate_keystrokes("down right");
        assert_eq!(grid.selected(&mut cx, 8, 4), vec![(1, 1)]);

        cx.simulate_keystrokes("shift-down shift-right");
        assert_eq!(
            grid.selected(&mut cx, 8, 4),
            vec![(1, 1), (1, 2), (2, 1), (2, 2)]
        );

        // And the ends: `Home` and `End` on the row, the modifier for the whole
        // result.
        cx.simulate_keystrokes("end");
        assert_eq!(grid.selected(&mut cx, 8, 4), vec![(2, 3)]);
        cx.simulate_keystrokes("home");
        assert_eq!(grid.selected(&mut cx, 8, 4), vec![(2, 0)]);

        let modifier = if cfg!(target_os = "macos") {
            "cmd"
        } else {
            "ctrl"
        };
        cx.simulate_keystrokes(&format!("{modifier}-end"));
        assert_eq!(grid.selected(&mut cx, 8, 4), vec![(7, 3)]);
        cx.simulate_keystrokes(&format!("{modifier}-home"));
        assert_eq!(grid.selected(&mut cx, 8, 4), vec![(0, 0)]);

        cx.simulate_keystrokes(&format!("{modifier}-a"));
        assert_eq!(grid.selected(&mut cx, 8, 4).len(), 32);
    }

    /// A page key moves by a screenful and stops at the end rather than running
    /// off it.
    #[gpui::test]
    fn a_page_key_moves_by_a_screenful(cx: &mut TestAppContext) {
        let probe = Rc::new(Probe::default());
        let (grid, mut cx) = open(Huge::new(1_000, 3, probe), cx);
        let page = grid.read(&mut cx, |grid| grid.visible_rows().len() - 1);
        assert!(page > 10, "the test window is smaller than it was");

        cx.simulate_keystrokes("down");
        cx.simulate_keystrokes("pagedown");
        assert_eq!(
            grid.selected(&mut cx, 1_000, 1).first().map(|c| c.0),
            Some(page)
        );

        cx.simulate_keystrokes("pageup pageup");
        assert_eq!(
            grid.selected(&mut cx, 1_000, 1).first().map(|c| c.0),
            Some(0)
        );
    }

    /// `Ctrl+C` puts the selection on the clipboard as TSV, and the other three
    /// formats are a method call away.
    #[gpui::test]
    fn the_selection_reaches_the_clipboard(cx: &mut TestAppContext) {
        let (grid, mut cx) = open(null_and_empty(), cx);

        grid.update(&mut cx, |grid, cx| grid.select_all(cx));
        cx.simulate_keystrokes(if cfg!(target_os = "macos") {
            "cmd-c"
        } else {
            "ctrl-c"
        });

        let tsv = cx
            .update(|_, cx| cx.read_from_clipboard())
            .and_then(|item| item.text())
            .expect("the clipboard was not written");
        assert_eq!(tsv, "1\t\t\n2\there\t");

        // The same block in the format that can carry the difference the TSV
        // above cannot: row one's second column is null and its third is the
        // empty string.
        grid.update(&mut cx, |grid, cx| grid.copy(CopyFormat::Json, cx));
        let json = cx
            .update(|_, cx| cx.read_from_clipboard())
            .and_then(|item| item.text())
            .expect("the clipboard was not written");
        assert!(json.contains("\"nothing\": null,"), "{json}");
        assert!(json.contains("\"empty\": \"\""), "{json}");
    }

    /// Hiding a column takes it out of the grid, out of a copy and out of the
    /// numbering the selection is written in.
    #[gpui::test]
    fn a_hidden_column_leaves_the_grid_entirely(cx: &mut TestAppContext) {
        let (grid, mut cx) = open(null_and_empty(), cx);

        grid.update(&mut cx, |grid, cx| grid.set_column_hidden(1, true, cx));
        assert!(grid.read(&mut cx, |grid| grid.is_column_hidden(1)));
        assert_eq!(
            grid.read(&mut cx, |grid| grid.visible_column_indices()),
            vec![0, 2]
        );

        grid.update(&mut cx, |grid, cx| grid.select_all(cx));
        grid.update(&mut cx, |grid, cx| grid.copy(CopyFormat::Tsv, cx));
        let tsv = cx
            .update(|_, cx| cx.read_from_clipboard())
            .and_then(|item| item.text())
            .expect("the clipboard was not written");
        assert_eq!(tsv, "1\t\n2\t");

        grid.update(&mut cx, |grid, cx| grid.set_column_hidden(1, false, cx));
        assert_eq!(
            grid.read(&mut cx, |grid| grid.visible_column_indices()),
            vec![0, 1, 2]
        );
    }

    /// A column can be widened and fitted, and a fit never shrinks a column
    /// below what can be grabbed again.
    #[gpui::test]
    fn a_column_can_be_widened_and_fitted(cx: &mut TestAppContext) {
        let (grid, mut cx) = open(
            Small {
                headings: vec![("id", GridColumnKind::Number)],
                rows: vec![vec![Some("a rather long value indeed")]],
            },
            cx,
        );

        assert_eq!(
            grid.read(&mut cx, |grid| grid.column_width(0)),
            DEFAULT_COLUMN_WIDTH
        );

        grid.update(&mut cx, |grid, cx| grid.set_column_width(0, 4., cx));
        assert_eq!(
            grid.read(&mut cx, |grid| grid.column_width(0)),
            MIN_COLUMN_WIDTH,
            "a column was dragged shut"
        );

        grid.update(&mut cx, |grid, cx| grid.autofit_column(0, cx));
        let fitted = grid.read(&mut cx, |grid| grid.column_width(0));
        assert!(
            fitted > DEFAULT_COLUMN_WIDTH && fitted <= MAX_AUTOFIT_WIDTH,
            "a twenty-six character value fitted to {fitted}"
        );
    }

    /// The other axis, which no `uniform_list` does for us: scrolling sideways
    /// moves the run of columns that is read, and the ones behind the left edge
    /// stop being read at all.
    #[gpui::test]
    fn only_the_visible_columns_are_read(cx: &mut TestAppContext) {
        let probe = Rc::new(Probe::default());
        let (grid, mut cx) = open(Huge::new(200, 60, probe.clone()), cx);

        probe.forget();
        grid.update(&mut cx, |grid, cx| grid.refresh(cx));
        assert_eq!(probe.min_column.get(), 0, "the left edge was not drawn");
        let first_screen = probe.max_column.get();
        assert!(
            first_screen < 20,
            "column {first_screen} of sixty was drawn"
        );

        // Walking the cursor out to column fifty scrolls the strip along; the
        // columns at the left-hand end are now off screen, and are not asked
        // about at all.
        grid.update(&mut cx, |grid, cx| grid.select_cell(0, 50, cx));
        probe.forget();
        grid.update(&mut cx, |grid, cx| grid.refresh(cx));

        assert!(
            probe.min_column.get() > first_screen,
            "columns 0..={} were still being read after scrolling to fifty",
            probe.min_column.get()
        );
        assert!(probe.max_column.get() >= 50, "column fifty was not drawn");
        assert!(
            probe.reads.get() < 2_000,
            "one frame read {} cells",
            probe.reads.get()
        );
    }

    /// A sideways wheel — or a plain one with `Shift`, which is what a mouse
    /// without a second axis has — scrolls the columns.
    #[gpui::test]
    fn the_wheel_scrolls_the_columns_sideways(cx: &mut TestAppContext) {
        let probe = Rc::new(Probe::default());
        let (grid, mut cx) = open(Huge::new(20, 60, probe.clone()), cx);
        probe.forget();
        grid.update(&mut cx, |grid, cx| grid.refresh(cx));
        let before = probe.max_column.get();

        cx.simulate_event(gpui::ScrollWheelEvent {
            position: point(px(column_x(2)), px(row_y(2))),
            delta: gpui::ScrollDelta::Pixels(point(px(-600.), px(0.))),
            modifiers: Modifiers::none(),
            touch_phase: gpui::TouchPhase::Moved,
        });
        cx.run_until_parked();

        probe.forget();
        grid.update(&mut cx, |grid, cx| grid.refresh(cx));
        assert!(
            probe.max_column.get() > before,
            "the wheel moved nothing: still stopping at column {}",
            probe.max_column.get()
        );
        assert!(probe.min_column.get() > 0, "the left edge never left");
    }

    /// A result the host replaces with a smaller one leaves no selection
    /// hanging over rows that are gone.
    #[gpui::test]
    fn a_replaced_result_pulls_the_selection_back_in(cx: &mut TestAppContext) {
        let probe = Rc::new(Probe::default());
        let (grid, mut cx) = open(Huge::new(50, 4, probe), cx);

        grid.update(&mut cx, |grid, cx| grid.select_all(cx));
        assert!(grid.read(&mut cx, |grid| grid.is_selected(49, 3)));

        grid.update(&mut cx, |grid, cx| {
            grid.source_mut(cx).rows.set(3);
        });
        assert!(!grid.read(&mut cx, |grid| grid.is_selected(49, 3)));
        assert!(grid.read(&mut cx, |grid| grid.is_selected(2, 3)));

        // And a new result — a different shape entirely — starts clean.
        grid.update(&mut cx, |grid, cx| grid.reset(cx));
        assert!(grid.read(&mut cx, |grid| grid.selection().is_empty()));
        assert_eq!(grid.read(&mut cx, |grid| grid.sort()), None);
    }
}
