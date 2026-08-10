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
//! Five things, all of them round trips the widget has no business making:
//! fetching the next batch ([`GridEvent::NearEnd`]), re-running the query in a
//! different order ([`GridEvent::SortRequested`] — the grid never sorts what it
//! holds, because it holds only the first n rows of an answer the server has all
//! of), opening a cell ([`GridEvent::CellActivated`], which is how a LOB
//! reaches a viewer), staging a typed value ([`GridEvent::EditCommitted`]), and
//! drawing the right-click menu ([`GridEvent::ContextMenu`] — the grid has no
//! strings to name items with, architecture document §7.8). Copying is *not*
//! among them: gpui owns the clipboard and the grid owns the selection, so the
//! grid does it itself.
//!
//! ## Editing, and the little of it that lives here
//!
//! The grid draws edit state and hosts the field the user types into; it stages
//! nothing and sends nothing. Which rows are marked and which cells are tinted
//! come from [`GridSource::row_status`] and [`GridSource::cell_dirty`], asked
//! only about what is on screen; whether a cell can be typed into at all comes
//! from [`GridSource::cell_editable`].
//!
//! The field itself has to be here for one reason: it is placed over a cell, and
//! nothing else knows where a cell is. A cell's rectangle falls out of
//! `laid_out`, `h_offset`, the row height and the list's scroll offset — four
//! numbers the grid keeps and nobody else sees — so [`GridView::begin_edit`]
//! owns the [`TextInput`] rather than the host owning it and asking where to put
//! it.
//!
//! **A close commits.** `Enter`, focus going elsewhere, a sort, a refresh, a
//! scroll, a column dragged — all of them end the edit by raising
//! [`GridEvent::EditCommitted`], and only `Escape` throws the typing away. The
//! asymmetry is deliberate: what is committed is *staged*, not sent, so the cost
//! of committing something the user did not mean is one undo in the pending
//! changes, while the cost of discarding is the typing. Committing an unchanged
//! field raises nothing at all, so the common case — open a cell, look at it,
//! move on — is silent either way.

use std::ops::Range;

use gpui::{
    AnyElement, App, Bounds, ClickEvent, ClipboardItem, Context, CursorStyle, DragMoveEvent,
    ElementId, Entity, EventEmitter, FocusHandle, Focusable, Hsla, IsZero, KeyBinding, MouseButton,
    MouseDownEvent, MouseMoveEvent, MouseUpEvent, Pixels, Point, ScrollHandle, ScrollStrategy,
    ScrollWheelEvent, SharedString, Size, Subscription, UniformListScrollHandle, Window, actions,
    canvas, div, point, prelude::*, px, size, uniform_list,
};
use rudbman_ui::scrollbar::{
    DraggedThumb, Scrollbar, ScrollbarAxis, ScrollbarState, hide_later, hide_now, scroll_to,
    scrolled,
};
use rudbman_ui::text_input::TextInput;
use rudbman_ui::theme::{Theme, theme, window_translucent};
use unicode_width::UnicodeWidthStr;

use crate::copy::{CopyFormat, DEFAULT_INSERT_TABLE, copy_payload};
use crate::selection::{CellAddress, Selection};
use crate::source::{
    GridCell, GridColumnAlign, GridSource, GridSourceState, NULL_TEXT, RowStatus, cell_label,
    lob_label,
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
        /// Throw away what has been typed into the inline editor and close it.
        CancelEdit,
        /// Commit the inline editor and open the next editable cell of the row.
        EditNext,
        /// Commit the inline editor and open the previous editable cell of the
        /// row.
        EditPrevious,
    ]
);

/// Key context that [`init`] binds the keys above to.
const KEY_CONTEXT: &str = "GridView";

/// Key context that exists only while the inline editor is open.
///
/// Its three keys — `Escape`, `Tab`, `Shift+Tab` — mean nothing to a grid that
/// is merely focused, and binding them on [`KEY_CONTEXT`] would take them away
/// from the app for as long as a grid has the focus: an `Escape` that closed a
/// dialog would close nothing while the user's eye was on a result. A context
/// that only exists for the frames the editor does cannot do that.
///
/// It is a context on the editor's own wrapper, *inside* the grid's, so the
/// stack while typing reads `GridView > GridCellEditor > TextInput`. The field's
/// own bindings sit deepest and therefore win: `Enter` is the field's `Submit`
/// and never the grid's `Activate`, and the arrows walk the caret rather than
/// the selection.
const EDITOR_KEY_CONTEXT: &str = "GridCellEditor";

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

/// Width of the strip down the gutter's left edge that marks a changed row.
///
/// Narrow on purpose: the row number has to stay readable beside it, and the
/// mark is answering "which rows did I touch?" at a glance down the column
/// rather than being read one row at a time.
const STATUS_WIDTH: f32 = 3.;

/// How hard a dirty cell is tinted.
///
/// Low enough that the text on top keeps the contrast the palette promised it,
/// and that a whole dirty row does not out-shout the selection drawn over it.
const DIRTY_TINT: f32 = 0.16;

/// How hard a whole inserted or deleted row is tinted.
///
/// Weaker than a dirty cell: this one covers the full width of the result, so
/// the same alpha would read as a change of theme rather than a change of row.
const ROW_TINT: f32 = 0.10;

/// How tall the inline editor is.
///
/// [`TextInput`] renders at a fixed height, which is taller than a row; the
/// field is centred on the cell rather than squeezed into it, so it reads as
/// something laid *over* the grid — which is what it is.
const EDITOR_HEIGHT: f32 = 32.;

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
        KeyBinding::new("escape", CancelEdit, Some(EDITOR_KEY_CONTEXT)),
        KeyBinding::new("tab", EditNext, Some(EDITOR_KEY_CONTEXT)),
        KeyBinding::new("shift-tab", EditPrevious, Some(EDITOR_KEY_CONTEXT)),
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

/// A value the user typed, on its way to whatever stages it.
///
/// One variant, because one is what a line of text can produce, and an enum
/// rather than a bare `String` because the next ones are already visible: a
/// `Null` for the gesture that clears a cell rather than emptying it — the
/// distinction the whole crate is built around (architecture document, §7.5) —
/// and a `Lob` for a body that arrives from a file instead of a keyboard.
/// Matching on it now costs a host nothing and saves it a signature change
/// later.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EditValue {
    /// What was in the field, verbatim.
    ///
    /// Not parsed and not trimmed: the grid has no idea what the column's type
    /// will make of it, and a layer that silently trimmed a `CHAR(10)` would be
    /// wrong in a way nobody could see.
    Text(String),
}

/// What the grid asks its host for.
///
/// [`Clone`] but not [`Copy`], since [`GridEvent::EditCommitted`] carries the
/// text the user typed. Every other variant is still four words of nothing.
#[derive(Clone, Debug, PartialEq, Eq)]
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
    /// How a LOB reaches its viewer, and how a cell reaches the editor: a host
    /// that answers this with [`GridView::begin_edit`] has DBeaver's gesture,
    /// and one that answers it with a viewer has the old one. The grid raises
    /// the same event either way, because which of the two a cell deserves
    /// depends on the column's type and on whether the result can be written
    /// to — neither of which is the widget's to judge.
    CellActivated {
        /// The row.
        row: usize,
        /// The source column index.
        column: usize,
    },
    /// The user finished typing into the inline editor, and the value is
    /// different from the one that was in the cell.
    ///
    /// Raised by `Enter`, by `Tab`, by the focus going elsewhere and by anything
    /// that moves the cell out from under the field — see the module docs on why
    /// a close commits. *Not* raised when the field was left as it was found,
    /// which is what keeps opening a null cell and thinking better of it from
    /// turning `NULL` into the empty string.
    ///
    /// The grid has staged nothing and changed nothing by raising this: the
    /// value it holds is what was typed, and the cell goes on drawing whatever
    /// [`GridSource::cell`] returns until the host's staging layer says
    /// otherwise.
    EditCommitted {
        /// The row.
        row: usize,
        /// The source column index, unaffected by hiding or by column widths.
        column: usize,
        /// What was typed.
        value: EditValue,
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

/// The inline editor, while it is open.
///
/// Holds the field and the three things needed to decide what its content
/// *means* when it closes — which cell it was opened over, what was in that cell
/// and whether that was a value at all.
struct Editing {
    /// The row being edited.
    row: usize,
    /// The **source** column being edited, which is what the event names.
    column: usize,
    /// The field. Rebuilt per edit rather than kept and re-seeded: a field
    /// carries a caret, a selection and an in-flight IME composition, and none
    /// of those mean anything in the next cell.
    input: Entity<TextInput>,
    /// What the field was seeded with, so that a close can tell a value the user
    /// changed from one they only looked at.
    seeded: String,
    /// Whether the cell held no value.
    ///
    /// Kept apart from `seeded` being empty, because the two are different
    /// cells: leaving an emptied field on a cell that was `NULL` leaves it
    /// `NULL`, while leaving it on a cell that held the empty string leaves the
    /// empty string. Flattening them here would lose exactly the distinction
    /// [`crate::source`] exists to keep.
    was_null: bool,
    /// Whether a frame has been drawn since the field opened.
    ///
    /// Opening one can scroll the result to bring its row into view, and until
    /// the list has laid itself out again the grid's idea of which rows are on
    /// screen is the one from before that scroll. Asking "has my row scrolled
    /// away?" against it would close the field on the frame it opened, so the
    /// first frame is not asked.
    settled: bool,
    /// The focus-out subscription. Dropped with the rest of this struct, which
    /// is what keeps closing an editor from being heard as the editor blurring.
    _blur: Subscription,
}

impl Editing {
    /// Whether `typed` is something other than what the cell held.
    ///
    /// The whole of "was the field actually changed?", and the reason opening a
    /// cell and pressing `Enter` stages nothing. A cell that held no value is
    /// changed the moment anything is typed into it and not before: an empty
    /// field over a null cell is still the null, which is why `was_null` is a
    /// field of its own rather than `seeded.is_empty()`.
    fn modified(&self, typed: &str) -> bool {
        if self.was_null {
            !typed.is_empty()
        } else {
            typed != self.seeded
        }
    }
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
    /// The inline editor, when one is open.
    editing: Option<Editing>,
    /// Whether the next frame has to take the focus back.
    ///
    /// Closing the editor drops the field, and with it the focus handle the
    /// keyboard was pointing at; something has to catch it or the grid goes
    /// deaf. It cannot be done where the closing happens — a host that calls
    /// [`GridView::refresh`] has no [`Window`] to hand — so the draw that
    /// notices the editor is gone does it instead. Not set by the one close
    /// that starts with the focus already having left.
    refocus: bool,
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
            editing: None,
            refocus: false,
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
    /// remember. Ends any edit in progress first: the rows are about to be
    /// something else, and a field left hanging over the y coordinate its cell
    /// used to be at is a field over the wrong cell.
    pub fn source_mut(&mut self, cx: &mut Context<Self>) -> &mut S {
        self.commit_edit(cx);
        cx.notify();
        &mut self.source
    }

    /// Re-reads the source, for a change the grid cannot have seen.
    pub fn refresh(&mut self, cx: &mut Context<Self>) {
        self.commit_edit(cx);
        self.ensure_layout();
        cx.notify();
    }

    /// Throws away everything the user has done to the columns and the
    /// selection, which is what a *new* result — as opposed to another batch of
    /// the same one — deserves.
    pub fn reset(&mut self, cx: &mut Context<Self>) {
        self.commit_edit(cx);
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
        self.commit_edit(cx);
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
        self.commit_edit(cx);
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
        self.commit_edit(cx);
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
        self.commit_edit(cx);
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
        self.commit_edit(cx);
        self.sort = sort;
        cx.notify();
    }

    /// Picks the cell at `row` and display position `column`, dropping whatever
    /// was picked.
    pub fn select_cell(&mut self, row: usize, column: usize, cx: &mut Context<Self>) {
        self.commit_edit(cx);
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
        self.commit_edit(cx);
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
        self.commit_edit(cx);
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
        self.commit_edit(cx);
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
        self.commit_edit(cx);
        self.scroll.scroll_to_item(row, ScrollStrategy::Top);
        cx.notify();
    }

    /// Which cell the inline editor is open over, as `(row, source column)`.
    ///
    /// `None` while nobody is typing, which is nearly always.
    pub fn editing(&self) -> Option<(usize, usize)> {
        self.editing
            .as_ref()
            .map(|editing| (editing.row, editing.column))
    }

    /// The field the user is typing into, while there is one.
    ///
    /// For a host that wants to read the half-typed value — a live validation
    /// hint beside the grid, say. Nothing about the edit is settled until
    /// [`GridEvent::EditCommitted`] arrives.
    pub fn editor(&self) -> Option<&Entity<TextInput>> {
        self.editing.as_ref().map(|editing| &editing.input)
    }

    /// Opens the inline editor over the cell at `row` and *source* `column`.
    ///
    /// `column` is a source column, the same numbering
    /// [`GridEvent::CellActivated`] hands out and [`GridSource::cell`] takes, so
    /// a host that answers an activation with this needs no translation. Answers
    /// whether the editor opened; it refuses when
    ///
    /// * the cell is not there,
    /// * its column is hidden — a field has to be drawn somewhere,
    /// * [`GridSource::cell_editable`] says no, which is the default and
    ///   therefore the answer for every source that has not opted in,
    /// * or the cell holds a [`GridCell::Lob`], whose body is not in the grid to
    ///   be seeded into a field or replaced from one.
    ///
    /// The field is seeded with the cell's text and a null cell seeds an empty
    /// one, so that the caret starts where typing starts. What the emptiness
    /// *means* is remembered separately: leaving an empty field on a cell that
    /// was null commits nothing, rather than quietly turning `NULL` into `''`.
    ///
    /// Any editor already open is committed first, and the selection moves onto
    /// the cell — a field is a strange place for the cursor not to be.
    pub fn begin_edit(
        &mut self,
        row: usize,
        column: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        self.ensure_layout();
        if row >= self.source.row_count() || column >= self.source.column_count() {
            return false;
        }
        let Some(display) = self.display_of(column) else {
            return false;
        };
        if !self.source.cell_editable(row, column) {
            return false;
        }
        let (seeded, was_null) = match self.source.cell(row, column) {
            GridCell::Null => (String::new(), true),
            GridCell::Text(text) => (text.to_string(), false),
            GridCell::Lob { .. } => return false,
        };

        self.commit_edit(cx);

        let cell = CellAddress::new(row, display);
        self.selection.replace(cell);
        self.reveal(cell);

        let grid = cx.entity().downgrade();
        let content = seeded.clone();
        let input = cx.new(|cx| {
            // `Enter` is the field's own action, bound in the field's own
            // deeper key context, so the grid's `Activate` never sees it and
            // this callback is the only way the keystroke comes back. It is
            // handed the content because the field is mid-update while it runs
            // and cannot be read out of the entity map.
            let mut input = TextInput::new(cx).on_submit(move |typed, _window, cx| {
                let typed = typed.to_string();
                grid.update(cx, |grid, cx| grid.close_edit(Some(&typed), true, cx))
                    .ok();
            });
            input.set_content(content, cx);
            input
        });

        let handle = input.read(cx).focus_handle(cx);
        let blur = cx.on_focus_out(&handle, window, |grid, _event, _window, cx| {
            // The focus has gone somewhere deliberate. Committing is right;
            // taking the focus back is not, which is the one close that leaves
            // `refocus` alone.
            let Some(typed) = grid.typed(cx) else {
                return;
            };
            grid.close_edit(Some(&typed), false, cx);
        });

        self.editing = Some(Editing {
            row,
            column,
            input,
            seeded,
            was_null,
            settled: false,
            _blur: blur,
        });
        self.refocus = false;
        handle.focus(window);
        cx.notify();
        true
    }

    /// Closes the editor, staging whatever is in it.
    ///
    /// What every gesture but `Escape` ends up in — see the module docs on why a
    /// close commits. Raises nothing when the field holds what the cell already
    /// held.
    pub fn commit_edit(&mut self, cx: &mut Context<Self>) {
        let Some(typed) = self.typed(cx) else {
            return;
        };
        self.close_edit(Some(&typed), true, cx);
    }

    /// Closes the editor and throws away what was typed.
    ///
    /// `Escape`, and the only way back out of a field without staging anything.
    pub fn cancel_edit(&mut self, cx: &mut Context<Self>) {
        self.close_edit(None, true, cx);
    }

    /// What is in the field, while there is one.
    ///
    /// Reads the field out of the entity map, so it must not be called while the
    /// field itself is being updated — which is why the submit callback is
    /// handed its content instead of asking for it.
    fn typed(&self, cx: &App) -> Option<String> {
        self.editing
            .as_ref()
            .map(|editing| editing.input.read(cx).content().to_string())
    }

    /// Takes the editor down, raising [`GridEvent::EditCommitted`] when `typed`
    /// is something other than what the cell held.
    ///
    /// `refocus` is false for exactly one caller — the focus having left of its
    /// own accord — because taking the focus back from wherever the user just
    /// put it would be worse than the edit ending quietly.
    fn close_edit(&mut self, typed: Option<&str>, refocus: bool, cx: &mut Context<Self>) {
        let Some(editing) = self.editing.take() else {
            return;
        };
        self.refocus = refocus;
        if let Some(typed) = typed
            && editing.modified(typed)
        {
            cx.emit(GridEvent::EditCommitted {
                row: editing.row,
                column: editing.column,
                value: EditValue::Text(typed.to_string()),
            });
        }
        cx.notify();
    }

    /// Commits the editor and opens the next — or previous — cell of the row
    /// that will take one.
    ///
    /// What `Tab` does. Stops at the ends of the row rather than wrapping onto
    /// the next one: a `Tab` that fell off the end and landed on a different
    /// row would be a keystroke that moved the edit somewhere the user was not
    /// looking. Nothing to move to leaves the commit standing and the editor
    /// closed, which is what `Tab` out of the last field of anything does.
    fn step_edit(&mut self, forward: bool, window: &mut Window, cx: &mut Context<Self>) {
        let Some((row, column)) = self.editing() else {
            return;
        };
        self.commit_edit(cx);
        let Some(display) = self.display_of(column) else {
            return;
        };
        let Some(next) = self.next_editable(row, display, forward) else {
            return;
        };
        self.begin_edit(row, next, window, cx);
    }

    /// The source column of the next cell of `row` that will take an edit,
    /// starting one display position beyond `from`.
    ///
    /// Walks display positions rather than source columns, so `Tab` follows the
    /// order the columns are drawn in and steps over the hidden ones — which
    /// have nowhere to put a field anyway.
    fn next_editable(&self, row: usize, from: usize, forward: bool) -> Option<usize> {
        let range: Box<dyn Iterator<Item = usize>> = if forward {
            Box::new(from + 1..self.laid_out.len())
        } else {
            Box::new((0..from).rev())
        };
        range
            .map(|display| self.laid_out[display].column)
            .find(|&column| self.source.cell_editable(row, column))
    }

    /// Where source column `column` sits along the header, if it is showing.
    fn display_of(&self, column: usize) -> Option<usize> {
        self.laid_out
            .iter()
            .position(|placed| placed.column == column)
    }

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
        // A sideways scroll is the user looking at another part of the row, and
        // a field that rode along would end up over a cell nobody is looking
        // at. `reveal` moves the offset by hand rather than through here, so
        // opening a field off the right-hand edge does not close it again.
        self.commit_edit(cx);
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

        // The field is placed from the list's scroll offset every frame, so a
        // scroll that keeps its row on screen carries it along with the cell
        // and there is nothing to do here. A scroll that takes the row off
        // screen would leave the user typing into something they cannot see,
        // and the edit ends with the row. The wheel is why this is checked here
        // at all: the list owns the vertical axis, so a wheel scroll never
        // passes through any of the grid's own methods.
        if let Some(editing) = self.editing.as_ref()
            && editing.settled
            && !self.row_on_screen(editing.row)
        {
            self.commit_edit(cx);
        }

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

    /// Whether any part of `row` is inside the body.
    ///
    /// Worked out from the list's scroll offset rather than from the range the
    /// list last reported, because that range is not to be trusted at every
    /// point in a frame: the list renders one row on its own to find out how
    /// tall a row is, and reports `0..1` while it does.
    fn row_on_screen(&self, row: usize) -> bool {
        let height = f32::from(self.base_handle().bounds().size.height);
        if height <= 0. {
            return true;
        }
        let top = row as f32 * ROW_HEIGHT + f32::from(self.base_handle().offset().y);
        top + ROW_HEIGHT > 0. && top < height
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
        // Only the keys the field does not want reach here while one is open:
        // `Left` and `Right` are the field's, `Up` and `Down` are not, so an
        // arrow out of a field commits it and walks on, which is what a
        // spreadsheet does.
        self.commit_edit(cx);
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
        self.commit_edit(cx);
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

    fn cancel_editing(&mut self, _: &CancelEdit, _: &mut Window, cx: &mut Context<Self>) {
        self.cancel_edit(cx);
    }

    fn edit_next(&mut self, _: &EditNext, window: &mut Window, cx: &mut Context<Self>) {
        self.step_edit(true, window, cx);
    }

    fn edit_previous(&mut self, _: &EditPrevious, window: &mut Window, cx: &mut Context<Self>) {
        self.step_edit(false, window, cx);
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
        self.commit_edit(cx);
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
        self.commit_edit(cx);
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
        // Asked once for the whole row, here, rather than once per cell: the
        // marker is the row's and the cells only need to know whether they are
        // being struck through.
        let status = self.source.row_status(row);
        let marker = status_color(status, theme);
        let cells: Vec<AnyElement> = visible
            .map(|display| self.render_cell(row, display, status, theme))
            .collect();

        div()
            .flex()
            .flex_row()
            .items_center()
            .h(px(ROW_HEIGHT))
            .w_full()
            // Zebra striping is a hint and nothing more; see the token's docs.
            .when(row % 2 == 1, |stripe| stripe.bg(theme.grid_row_alt))
            // A row that is going or that was never there is tinted whole,
            // because the change is the *row* and not any value in it. Weakly:
            // a wash across the full width at the strength a single dirty cell
            // is tinted at would read as a change of theme.
            .when_some(
                match status {
                    RowStatus::Inserted | RowStatus::Deleted => marker,
                    _ => None,
                },
                |row, colour| row.bg(colour.opacity(ROW_TINT)),
            )
            .child(
                div()
                    .relative()
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
                    .child(SharedString::from((row + 1).to_string()))
                    // The whole of the row marker: a bar on the outer edge of
                    // the gutter, in the colour the status derives from. It is
                    // on the edge rather than beside the number so that a
                    // column of them can be read down at a glance, and it is
                    // three pixels wide so that the number it shares the gutter
                    // with is still the thing being read.
                    .children(marker.map(|colour| {
                        div()
                            .absolute()
                            .left_0()
                            .top_0()
                            .bottom_0()
                            .w(px(STATUS_WIDTH))
                            .bg(colour)
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

    /// Draws one cell.
    ///
    /// Plain divs with no id and no listeners: the whole of the pointer
    /// behaviour is [`GridView::hit`], so a cell is only a box with text in it.
    fn render_cell(
        &self,
        row: usize,
        display: usize,
        status: RowStatus,
        theme: &Theme,
    ) -> AnyElement {
        let placed = self.laid_out[display];
        let column = self.source.column(placed.column);
        let label = cell_label(&self.source.cell(row, placed.column));
        let dirty = self.source.cell_dirty(row, placed.column);
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
            // A child rather than a background, for the same reason the cursor
            // outline is one: the background is the selection's, and a dirty
            // cell that stopped looking dirty the moment it was picked would
            // hide the thing the user is about to copy or revert. Drawn before
            // the text so the text sits on top of it.
            .when(dirty, |cell| {
                cell.child(
                    div()
                        .absolute()
                        .top_0()
                        .bottom_0()
                        .left_0()
                        .right_0()
                        .bg(theme.accent.opacity(DIRTY_TINT)),
                )
            })
            .child(
                div()
                    .truncate()
                    // A deleted row is still shown in its place — see
                    // `RowStatus::Deleted` — so its values need to say that
                    // they are on their way out rather than merely that
                    // something happened to the row.
                    .when(status == RowStatus::Deleted, |text| text.line_through())
                    .child(label.text),
            )
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

    /// Draws the inline editor over the cell it was opened on.
    ///
    /// The only place in the crate that turns a cell address back into a
    /// rectangle, and it does it from the four numbers the grid already keeps:
    /// `laid_out` for the column's left edge, `h_offset` for how far the strip
    /// has slid, `ROW_HEIGHT` for the row's top and the list's own scroll offset
    /// for where that row has been carried to. The same arithmetic
    /// [`GridView::hit`] runs backwards, which is what makes a field land on
    /// exactly the cell a click would have found.
    ///
    /// Everything is recomputed per frame rather than remembered, so a resize, a
    /// scroll or a column dragged out from under the field moves the field with
    /// it instead of stranding it.
    fn render_editor(&self) -> Option<AnyElement> {
        let editing = self.editing.as_ref()?;
        let display = self.display_of(editing.column)?;
        let placed = self.laid_out[display];
        let scrolled_by = f32::from(self.base_handle().offset().y);

        let field = div()
            .key_context(EDITOR_KEY_CONTEXT)
            .absolute()
            .left(px(placed.x - self.h_offset))
            // Centred on the row rather than fitted into it: the field is
            // taller than a row and squeezing it would clip its own border.
            .top(px(
                editing.row as f32 * ROW_HEIGHT + scrolled_by - (EDITOR_HEIGHT - ROW_HEIGHT) / 2.
            ))
            .w(px(self.column_width(editing.column)))
            // Without this the grid's own arithmetic hit test would see every
            // press meant for the field, move the selection and — since moving
            // the selection commits — close the field the user was aiming at.
            .occlude()
            .child(editing.input.clone());

        Some(
            // Clipped to the content area, so a field on a column half off the
            // right-hand edge is half drawn rather than painted over the
            // gutter and the scrollbar.
            div()
                .absolute()
                .left(px(GUTTER_WIDTH))
                .top(px(HEADER_HEIGHT))
                .right_0()
                .bottom_0()
                .overflow_hidden()
                .child(field)
                .into_any_element(),
        )
    }
}

/// The colour a row's marker is drawn in, or `None` for a row nothing has been
/// staged against.
///
/// Derived from the palette rather than added to it, exactly as the grid's own
/// tokens are (architecture document, §7.2): a theme file written by hand knows
/// nothing about staged edits and gains nothing it has to know. The three
/// meanings map onto the three the palette already carries — a change is the
/// accent, something new is a success, something going is a danger — so a theme
/// that made its danger colour green would mark deletions green, which is what
/// its author asked for.
fn status_color(status: RowStatus, theme: &Theme) -> Option<Hsla> {
    match status {
        RowStatus::Unchanged => None,
        RowStatus::Modified => Some(theme.accent),
        RowStatus::Inserted => Some(theme.success),
        RowStatus::Deleted => Some(theme.danger),
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
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.ensure_layout();

        // A result the host swapped out from under an open field: the cell the
        // field was over is not there any more, so there is nothing to commit
        // it *to*. Every route a host actually takes to swap a result commits
        // first; this is the backstop for the one it does not.
        if let Some((row, column)) = self.editing()
            && (row >= self.source.row_count() || self.display_of(column).is_none())
        {
            self.close_edit(None, true, cx);
        }
        // From here on the field's row can be judged against where the list
        // actually is; see the flag's docs for what the first frame is spared.
        if let Some(editing) = self.editing.as_mut() {
            editing.settled = true;
        }
        // Closing dropped the field, and the focus was in it. Done here because
        // this is the first place after a close that has a window to hand — see
        // the field's docs.
        if std::mem::take(&mut self.refocus) {
            self.focus_handle.focus(window);
        }

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
            .on_action(cx.listener(Self::cancel_editing))
            .on_action(cx.listener(Self::edit_next))
            .on_action(cx.listener(Self::edit_previous))
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
            // Last, and absolutely positioned, so it is painted over the rows
            // rather than between them.
            .children(self.render_editor())
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
        /// The same two numbers for the edit-state questions, which are drawn
        /// per row and per cell and are therefore held to the same budget.
        marks: Cell<usize>,
        max_mark_row: Cell<usize>,
    }

    impl Probe {
        fn note(&self, row: usize, column: usize) {
            self.reads.set(self.reads.get() + 1);
            self.max_row.set(self.max_row.get().max(row));
            self.min_column.set(self.min_column.get().min(column));
            self.max_column.set(self.max_column.get().max(column));
        }

        fn note_mark(&self, row: usize) {
            self.marks.set(self.marks.get() + 1);
            self.max_mark_row.set(self.max_mark_row.get().max(row));
        }

        fn forget(&self) {
            self.reads.set(0);
            self.max_row.set(0);
            self.min_column.set(usize::MAX);
            self.max_column.set(0);
            self.marks.set(0);
            self.max_mark_row.set(0);
        }
    }

    /// A result of any size at all, generated rather than stored, that counts
    /// what it was asked for.
    struct Huge {
        rows: Cell<usize>,
        columns: usize,
        state: Cell<GridSourceState>,
        editable: bool,
        probe: Rc<Probe>,
    }

    impl Huge {
        fn new(rows: usize, columns: usize, probe: Rc<Probe>) -> Self {
            Self {
                rows: Cell::new(rows),
                columns,
                state: Cell::new(GridSourceState::Complete),
                editable: false,
                probe,
            }
        }

        fn growing(mut self) -> Self {
            self.state = Cell::new(GridSourceState::HasMore);
            self
        }

        /// Every cell of it takes an edit, for the tests about what happens to
        /// a field rather than about which cells may have one.
        fn editable(mut self) -> Self {
            self.editable = true;
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

        // Every row of the fixture claims to have been changed, so that a grid
        // that asked about one it cannot see would be caught by the counting
        // rather than by the answers happening to be cheap.
        fn row_status(&self, row: usize) -> RowStatus {
            self.probe.note_mark(row);
            RowStatus::Modified
        }

        fn cell_dirty(&self, row: usize, _column: usize) -> bool {
            self.probe.note_mark(row);
            true
        }

        fn cell_editable(&self, _row: usize, _column: usize) -> bool {
            self.editable
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

    /// A result something has been staged against, which is the shape of the
    /// overlay a host wraps a real one in: it knows which rows were touched,
    /// which cells carry the change, and which columns will take one.
    ///
    /// Column 0 is the key and refuses edits; 1 and 2 take them, and column 1 of
    /// row 0 holds no value at all — the cell that must not turn into the empty
    /// string by being looked at.
    struct Staged {
        rows: Vec<Vec<Option<&'static str>>>,
        status: Vec<RowStatus>,
        dirty: Vec<(usize, usize)>,
        editable: Vec<usize>,
    }

    impl Staged {
        fn new() -> Self {
            Self {
                rows: vec![
                    vec![Some("1"), None, Some("here")],
                    vec![Some("2"), Some(""), Some("there")],
                ],
                status: vec![RowStatus::Unchanged, RowStatus::Unchanged],
                dirty: Vec::new(),
                editable: vec![1, 2],
            }
        }
    }

    impl GridSource for Staged {
        fn column_count(&self) -> usize {
            3
        }

        fn column(&self, index: usize) -> GridColumn<'_> {
            GridColumn::new(["id", "nothing", "note"][index], GridColumnKind::Text)
                .primary_key(index == 0)
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

        fn row_status(&self, row: usize) -> RowStatus {
            self.status[row]
        }

        fn cell_dirty(&self, row: usize, column: usize) -> bool {
            self.dirty.contains(&(row, column))
        }

        fn cell_editable(&self, _row: usize, column: usize) -> bool {
            self.editable.contains(&column)
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

        /// Changes the grid where a window is needed too, which everything
        /// about the inline editor is: it has a focus to take.
        fn update_in<R>(
            &self,
            cx: &mut VisualTestContext,
            f: impl FnOnce(&mut GridView<S>, &mut Window, &mut Context<GridView<S>>) -> R,
        ) -> R {
            let out = cx.update(|window, cx| self.grid.update(cx, |grid, cx| f(grid, window, cx)));
            cx.run_until_parked();
            out
        }

        /// What is in the inline editor, if one is open.
        fn typed(&self, cx: &mut VisualTestContext) -> Option<String> {
            cx.update(|_, cx| {
                self.grid
                    .read(cx)
                    .editor()
                    .map(|input| input.read(cx).content().to_string())
            })
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
                // Cloned rather than copied: `GridEvent::EditCommitted` carries
                // the text that was typed.
                cx.subscribe(&grid, move |_: &mut Harness<S>, _, event: &GridEvent, _| {
                    events.borrow_mut().push(event.clone());
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

        // The edit markers are on the same budget as the values, and were the
        // easiest thing in the crate to get wrong: a row marker asked for down
        // the whole result would read a million rows to draw forty.
        assert!(
            probe.marks.get() < 2_000,
            "one frame asked about {} marks",
            probe.marks.get()
        );
        assert!(
            probe.max_mark_row.get() < 60,
            "the mark of row {} was asked for a viewport of {} rows",
            probe.max_mark_row.get(),
            visible_rows.len()
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
        assert!(
            probe.max_mark_row.get() > 800_000,
            "the marks did not follow the viewport"
        );
        assert!(
            probe.marks.get() < 2_000,
            "the scrolled frame asked about {} marks",
            probe.marks.get()
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

    /// A source that never opted in cannot be typed into, however the host
    /// asks: the default `cell_editable` is what stands between a read-only
    /// result and an editor over it.
    #[gpui::test]
    fn a_source_that_did_not_opt_in_cannot_be_edited(cx: &mut TestAppContext) {
        let probe = Rc::new(Probe::default());
        let (grid, mut cx) = open(Huge::new(6, 4, probe), cx);

        assert!(!grid.update_in(&mut cx, |grid, window, cx| {
            grid.begin_edit(1, 1, window, cx)
        }));
        assert_eq!(grid.read(&mut cx, |grid| grid.editing()), None);
        assert_eq!(grid.drain(), vec![], "a refused edit announced something");
    }

    /// The whole round trip: the field opens over the cell holding what the cell
    /// holds, what is typed goes into it, and `Enter` hands the host the value
    /// and the *source* column it belongs to.
    #[gpui::test]
    fn typing_into_a_cell_and_pressing_enter_stages_the_value(cx: &mut TestAppContext) {
        let (grid, mut cx) = open(Staged::new(), cx);

        assert!(grid.update_in(&mut cx, |grid, window, cx| {
            grid.begin_edit(1, 2, window, cx)
        }));
        assert_eq!(grid.read(&mut cx, |grid| grid.editing()), Some((1, 2)));
        assert_eq!(
            grid.typed(&mut cx).as_deref(),
            Some("there"),
            "the field was not seeded with what the cell held"
        );
        // The cursor followed the field, which is where a user would expect it.
        assert_eq!(grid.selected(&mut cx, 2, 3), vec![(1, 2)]);

        cx.simulate_input("!");
        cx.simulate_keystrokes("enter");

        assert_eq!(
            grid.drain(),
            vec![GridEvent::EditCommitted {
                row: 1,
                column: 2,
                value: EditValue::Text("there!".to_owned()),
            }]
        );
        assert_eq!(
            grid.read(&mut cx, |grid| grid.editing()),
            None,
            "the field outlived the commit"
        );
        // Nothing was staged *here*: the grid changed no value of its own, and
        // goes on drawing whatever the source returns.
        assert_eq!(
            grid.read(&mut cx, |grid| match grid.source().cell(1, 2) {
                GridCell::Text(text) => text.to_owned(),
                other => panic!("{other:?}"),
            }),
            "there",
            "the grid wrote the typed value into the result"
        );
        // And the keyboard came back to the grid, rather than being left on a
        // field that no longer exists.
        cx.simulate_keystrokes("down");
        assert_eq!(grid.selected(&mut cx, 2, 3), vec![(1, 2)]);
    }

    /// `Escape` is the one way out that stages nothing, however much was typed.
    #[gpui::test]
    fn escape_throws_the_typing_away(cx: &mut TestAppContext) {
        let (grid, mut cx) = open(Staged::new(), cx);

        grid.update_in(&mut cx, |grid, window, cx| {
            grid.begin_edit(0, 2, window, cx)
        });
        cx.simulate_input("rewritten");
        assert_eq!(grid.typed(&mut cx).as_deref(), Some("hererewritten"));

        cx.simulate_keystrokes("escape");
        assert_eq!(grid.read(&mut cx, |grid| grid.editing()), None);
        assert_eq!(
            grid.drain(),
            vec![],
            "escape staged what it was supposed to throw away"
        );
    }

    /// Opening a null cell and thinking better of it leaves it null.
    ///
    /// The trap this whole crate is built to avoid (architecture document,
    /// §7.5): a field seeded empty because there was no value, committed
    /// unchanged, must not become an `UPDATE … SET x = ''`. The same holds for a
    /// cell that really does hold the empty string, and for one with a value in
    /// it that nobody touched.
    #[gpui::test]
    fn a_field_nobody_changed_stages_nothing(cx: &mut TestAppContext) {
        let (grid, mut cx) = open(Staged::new(), cx);

        // Row 0, column 1 is null.
        grid.update_in(&mut cx, |grid, window, cx| {
            grid.begin_edit(0, 1, window, cx)
        });
        assert_eq!(grid.typed(&mut cx).as_deref(), Some(""));
        cx.simulate_keystrokes("enter");
        assert_eq!(
            grid.drain(),
            vec![],
            "a null cell was turned into the empty string by being looked at"
        );

        // Row 1, column 1 really does hold the empty string.
        grid.update_in(&mut cx, |grid, window, cx| {
            grid.begin_edit(1, 1, window, cx)
        });
        cx.simulate_keystrokes("enter");
        assert_eq!(grid.drain(), vec![]);

        // And a value left exactly as it was found.
        grid.update_in(&mut cx, |grid, window, cx| {
            grid.begin_edit(1, 2, window, cx)
        });
        cx.simulate_keystrokes("enter");
        assert_eq!(grid.drain(), vec![]);

        // Typing into the null one, though, is a change — and the emptiness it
        // started from is not the value it commits.
        grid.update_in(&mut cx, |grid, window, cx| {
            grid.begin_edit(0, 1, window, cx)
        });
        cx.simulate_input("x");
        cx.simulate_keystrokes("enter");
        assert_eq!(
            grid.drain(),
            vec![GridEvent::EditCommitted {
                row: 0,
                column: 1,
                value: EditValue::Text("x".to_owned()),
            }]
        );
    }

    /// `Tab` commits and walks on to the next cell of the row that will take an
    /// edit — stepping over the key column, which will not — and `Shift+Tab`
    /// walks back. Falling off the end leaves the commit standing and the field
    /// closed.
    #[gpui::test]
    fn tab_commits_and_opens_the_next_editable_cell(cx: &mut TestAppContext) {
        let (grid, mut cx) = open(Staged::new(), cx);

        grid.update_in(&mut cx, |grid, window, cx| {
            grid.begin_edit(1, 1, window, cx)
        });
        cx.simulate_input("typed");
        cx.simulate_keystrokes("tab");

        assert_eq!(
            grid.drain(),
            vec![GridEvent::EditCommitted {
                row: 1,
                column: 1,
                value: EditValue::Text("typed".to_owned()),
            }]
        );
        assert_eq!(
            grid.read(&mut cx, |grid| grid.editing()),
            Some((1, 2)),
            "tab did not reopen on the next editable cell"
        );
        assert_eq!(grid.typed(&mut cx).as_deref(), Some("there"));

        // Backwards, over the same gap, and stopping at column 1 rather than
        // landing on the key column.
        cx.simulate_keystrokes("shift-tab");
        assert_eq!(grid.read(&mut cx, |grid| grid.editing()), Some((1, 1)));
        assert_eq!(
            grid.drain(),
            vec![],
            "a field nobody touched was staged on the way back"
        );

        cx.simulate_keystrokes("shift-tab");
        assert_eq!(
            grid.read(&mut cx, |grid| grid.editing()),
            None,
            "shift-tab opened the key column"
        );

        // And off the far end: the last editable cell of the row has nowhere to
        // hand the field on to.
        grid.update_in(&mut cx, |grid, window, cx| {
            grid.begin_edit(1, 2, window, cx)
        });
        cx.simulate_input("!");
        cx.simulate_keystrokes("tab");
        assert_eq!(
            grid.drain(),
            vec![GridEvent::EditCommitted {
                row: 1,
                column: 2,
                value: EditValue::Text("there!".to_owned()),
            }]
        );
        assert_eq!(grid.read(&mut cx, |grid| grid.editing()), None);
    }

    /// Everything that moves the cell out from under the field ends the edit,
    /// and ends it by committing — see the module docs on why that way round.
    #[gpui::test]
    fn anything_that_moves_the_cell_commits_the_field(cx: &mut TestAppContext) {
        let (grid, mut cx) = open(Staged::new(), cx);

        // A sort: the rows are about to be a different set of rows.
        grid.update_in(&mut cx, |grid, window, cx| {
            grid.begin_edit(1, 2, window, cx)
        });
        cx.simulate_input("?");
        grid.update(&mut cx, |grid, cx| grid.toggle_sort(0, cx));
        assert_eq!(grid.read(&mut cx, |grid| grid.editing()), None);
        assert_eq!(
            grid.drain(),
            vec![
                GridEvent::EditCommitted {
                    row: 1,
                    column: 2,
                    value: EditValue::Text("there?".to_owned()),
                },
                GridEvent::SortRequested {
                    column: 0,
                    direction: Some(SortDirection::Ascending),
                },
            ],
            "the commit did not come before the thing that caused it"
        );

        // A column hidden out from under it: there would be nowhere left to
        // draw the field.
        grid.update_in(&mut cx, |grid, window, cx| {
            grid.begin_edit(0, 2, window, cx)
        });
        cx.simulate_input("!");
        grid.update(&mut cx, |grid, cx| grid.set_column_hidden(2, true, cx));
        assert_eq!(grid.read(&mut cx, |grid| grid.editing()), None);
        assert_eq!(
            grid.drain(),
            vec![GridEvent::EditCommitted {
                row: 0,
                column: 2,
                value: EditValue::Text("here!".to_owned()),
            }]
        );

        // An arrow key, which the field does not want and the grid does.
        grid.update(&mut cx, |grid, cx| grid.set_column_hidden(2, false, cx));
        grid.drain();
        grid.update_in(&mut cx, |grid, window, cx| {
            grid.begin_edit(0, 1, window, cx)
        });
        cx.simulate_input("q");
        cx.simulate_keystrokes("down");
        assert_eq!(grid.read(&mut cx, |grid| grid.editing()), None);
        assert_eq!(
            grid.drain(),
            vec![GridEvent::EditCommitted {
                row: 0,
                column: 1,
                value: EditValue::Text("q".to_owned()),
            }]
        );
        assert_eq!(
            grid.selected(&mut cx, 2, 3),
            vec![(1, 1)],
            "the arrow committed but did not move"
        );

        // And the host dropping a batch in, which is the same problem arriving
        // from the other side.
        grid.update_in(&mut cx, |grid, window, cx| {
            grid.begin_edit(0, 2, window, cx)
        });
        cx.simulate_input("z");
        grid.update(&mut cx, |grid, cx| {
            grid.source_mut(cx).status[0] = RowStatus::Modified;
        });
        assert_eq!(grid.read(&mut cx, |grid| grid.editing()), None);
        assert_eq!(
            grid.drain(),
            vec![GridEvent::EditCommitted {
                row: 0,
                column: 2,
                value: EditValue::Text("herez".to_owned()),
            }]
        );
    }

    /// A wheel is the one scroll the grid does not run itself — the list owns
    /// the vertical axis — so the field has to notice on its own that its row
    /// has gone, rather than being told.
    #[gpui::test]
    fn a_row_scrolled_out_of_sight_takes_its_field_with_it(cx: &mut TestAppContext) {
        let probe = Rc::new(Probe::default());
        let (grid, mut cx) = open(Huge::new(10_000, 4, probe).editable(), cx);

        grid.update_in(&mut cx, |grid, window, cx| {
            grid.begin_edit(0, 1, window, cx)
        });
        cx.simulate_input("typed");
        assert_eq!(grid.read(&mut cx, |grid| grid.editing()), Some((0, 1)));

        cx.simulate_event(gpui::ScrollWheelEvent {
            position: point(px(column_x(1)), px(row_y(2))),
            delta: gpui::ScrollDelta::Pixels(point(px(0.), px(-4_000.))),
            modifiers: Modifiers::none(),
            touch_phase: gpui::TouchPhase::Moved,
        });
        cx.run_until_parked();

        assert!(
            grid.read(&mut cx, |grid| grid.visible_rows().start) > 0,
            "the wheel scrolled nothing, so the test proves nothing"
        );
        assert_eq!(
            grid.read(&mut cx, |grid| grid.editing()),
            None,
            "the field was left over a row that is no longer on screen"
        );
        assert_eq!(
            grid.drain(),
            vec![GridEvent::EditCommitted {
                row: 0,
                column: 1,
                value: EditValue::Text("valuetyped".to_owned()),
            }]
        );
    }

    /// The focus going somewhere else commits, and — alone among the closes —
    /// does not drag it back.
    #[gpui::test]
    fn the_focus_leaving_commits_without_taking_itself_back(cx: &mut TestAppContext) {
        let probe = Rc::new(Probe::default());
        let (grid, mut cx) = open(Huge::new(20, 4, probe).editable(), cx);

        // gpui reports a focus change only while the window is active, and a
        // test window is inactive until it is told otherwise.
        cx.update(|window, _| window.activate_window());
        cx.run_until_parked();

        grid.update_in(&mut cx, |grid, window, cx| {
            grid.begin_edit(2, 1, window, cx)
        });
        cx.simulate_input("!");

        // Somewhere else in the window, which for a grid on its own is the grid
        // itself — what matters is that the field is not it.
        cx.update(|window, cx| {
            let handle = grid.grid.read(cx).focus_handle(cx);
            handle.focus(window);
        });
        cx.run_until_parked();

        assert_eq!(grid.read(&mut cx, |grid| grid.editing()), None);
        assert_eq!(
            grid.drain(),
            vec![GridEvent::EditCommitted {
                row: 2,
                column: 1,
                value: EditValue::Text("value!".to_owned()),
            }]
        );
    }

    /// The markers are drawn from what the source says, and the grid asks it
    /// only about the rows and cells it is drawing.
    #[gpui::test]
    fn the_markers_read_only_what_is_drawn(cx: &mut TestAppContext) {
        let probe = Rc::new(Probe::default());
        let (grid, mut cx) = open(Huge::new(500_000, 40, probe.clone()), cx);

        probe.forget();
        grid.update(&mut cx, |grid, cx| grid.refresh(cx));
        assert!(probe.marks.get() > 0, "nothing asked about the markers");
        assert!(
            probe.max_mark_row.get() < 60,
            "the marker of row {} was asked for",
            probe.max_mark_row.get()
        );

        // And the dirty marks are a question per *drawn* cell: hiding all but
        // two columns cuts the count to what two columns and a row marker
        // cost, rather than leaving it at what forty would.
        let before = probe.marks.get();
        for column in 2..40 {
            grid.update(&mut cx, |grid, cx| {
                grid.set_column_hidden(column, true, cx);
            });
        }
        probe.forget();
        grid.update(&mut cx, |grid, cx| grid.refresh(cx));
        assert!(
            probe.marks.get() * 3 < before,
            "hiding thirty-eight of forty columns left {} of {before} marks",
            probe.marks.get()
        );
    }

    /// A staged result draws its markers and its tints, and an unstaged one
    /// draws neither — the whole of what the defaults buy a read-only source.
    #[gpui::test]
    fn a_row_is_marked_only_when_the_source_says_so(cx: &mut TestAppContext) {
        let (grid, mut cx) = open(Staged::new(), cx);
        let palette = cx.update(|_, cx| rudbman_ui::theme::theme(cx));

        assert_eq!(status_color(RowStatus::Unchanged, &palette), None);
        assert_eq!(
            status_color(RowStatus::Modified, &palette),
            Some(palette.accent)
        );
        assert_eq!(
            status_color(RowStatus::Inserted, &palette),
            Some(palette.success)
        );
        assert_eq!(
            status_color(RowStatus::Deleted, &palette),
            Some(palette.danger)
        );

        assert_eq!(
            grid.read(&mut cx, |grid| grid.source().row_status(0)),
            RowStatus::Unchanged
        );
        grid.update(&mut cx, |grid, cx| {
            let source = grid.source_mut(cx);
            source.status[0] = RowStatus::Deleted;
            source.dirty.push((1, 2));
        });
        assert_eq!(
            grid.read(&mut cx, |grid| grid.source().row_status(0)),
            RowStatus::Deleted
        );
        assert!(grid.read(&mut cx, |grid| grid.source().cell_dirty(1, 2)));
        assert!(!grid.read(&mut cx, |grid| grid.source().cell_dirty(1, 1)));
    }
}
