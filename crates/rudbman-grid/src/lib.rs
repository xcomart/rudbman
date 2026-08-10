//! The result grid: a virtualised table over rows it never fetches itself.
//!
//! One widget, [`GridView`], and the three small vocabularies it is built on:
//! [`source`] is where the rows come from, [`selection`] is which cells are
//! picked, and [`copy`] is how they leave for the clipboard. The last two are
//! pure — no window, no gpui state — which is why the awkward half of a grid's
//! behaviour can be tested without one.
//!
//! ## What this crate knows
//!
//! `rudbman-ui` and gpui, and nothing else (architecture document, §3.1). In
//! particular it does **not** know `rudbman-jdbc`: the rows arrive through
//! [`GridSource`], so a test source is twenty lines and the crate's tests need
//! no JVM. The same boundary is what lets the grid be pointed at anything with
//! rows and columns — a `DESCRIBE`, a plan, a diff — rather than only at a
//! result set.
//!
//! ## What it holds to
//!
//! * **A million rows scroll without a stutter.** Neither axis lays out more
//!   than the viewport can show, and no per-frame work is proportional to the
//!   size of the result. See [`grid`] for how the horizontal half is done.
//! * **Null is not the empty string.** They are different values and they are
//!   drawn differently, which too many tools cannot manage (architecture
//!   document, §7.5). Two of the four copy formats can carry the difference and
//!   two cannot; [`copy`] says which, and why.
//! * **The grid does not sort.** It holds the first `n` rows of an answer the
//!   server holds all of, so ordering is a round trip the host makes; a header
//!   click raises [`GridEvent::SortRequested`] and nothing moves until the host
//!   comes back with new rows.
//! * **The grid does not stage an edit either.** It draws which rows and cells
//!   have been changed ([`GridSource::row_status`], [`GridSource::cell_dirty`])
//!   and it hosts the field the user types into, because only it knows where a
//!   cell is; what a typed value *becomes* is the host's, and reaches it as
//!   [`GridEvent::EditCommitted`]. See [`grid`] for what a close does and why
//!   it commits.
//!
//! Call [`init`] once during application start-up so the key bindings are
//! registered.
//!
//! ```ignore
//! let grid = cx.new(|cx| GridView::new(results, cx).insert_table("app.orders"));
//! cx.subscribe_in(&grid, window, |view, grid, event, window, cx| match event {
//!     GridEvent::NearEnd => view.fetch_next_batch(cx),
//!     GridEvent::SortRequested { column, direction } => view.reorder(*column, *direction, cx),
//!     GridEvent::CellActivated { row, column } => {
//!         // Either a viewer or the editor, depending on what the cell holds.
//!         grid.update(cx, |grid, cx| grid.begin_edit(*row, *column, window, cx));
//!     }
//!     GridEvent::EditCommitted { row, column, value } => view.stage(*row, *column, value, cx),
//!     GridEvent::ContextMenu { target, position } => view.open_menu(*target, *position, cx),
//! })
//! .detach();
//! ```

#![warn(missing_docs)]

pub mod copy;
pub mod grid;
pub mod selection;
pub mod source;

pub use copy::{CopyFormat, DEFAULT_INSERT_TABLE, copy_payload};
pub use grid::{EditValue, GridEvent, GridView, MenuTarget, SortDirection};
pub use selection::{CellAddress, CellRange, Selection};
pub use source::{
    CellLabel, DEFAULT_TEXT, GridCell, GridColumn, GridColumnAlign, GridColumnKind, GridSource,
    GridSourceState, NULL_TEXT, RowStatus, cell_label, lob_label,
};

use gpui::App;

/// Registers everything the grid needs before the first window opens.
///
/// Only key bindings, for now; [`rudbman_ui::init`] still has to be called for
/// the palette the grid draws with.
pub fn init(cx: &mut App) {
    grid::init(cx);
}
