//! Diagrams on a canvas: a model, two layouts, a router, an SVG writer, and the
//! two widgets that draw on them.
//!
//! Two widgets over four modules. [`ErdView`] draws a *schema* — boxes, the
//! foreign keys between them, and a file to export it to — and [`BuilderView`]
//! draws a *query*: the same boxes, with the columns picked out and the joins
//! drawn between them by hand. What they have in common is [`canvas`], the
//! viewport, the gestures and the frame assembly neither of them owns
//! (architecture document, §7.7), so that panning, zooming, dragging and
//! virtualising are one implementation rather than two that drift.
//!
//! Under both of them are three pure modules. [`model`] is what a diagram *is*,
//! [`layout`] is where the boxes go and how the lines get between them, and
//! [`svg`] is how the picture leaves for a file. All three are pure — no
//! window, no gpui state, no clock — which is why the hard half of a diagram is
//! tested without one.
//!
//! ## What this crate knows
//!
//! `rudbman-ui` and gpui, and nothing else (architecture document, §3.1). In
//! particular it does **not** know `rudbman-jdbc`: an [`ErdModel`] is assembled
//! by the host from the `imported_keys` and column metadata it has already
//! fetched, so this crate's tests need no JVM and no driver. The query builder
//! keeps the same boundary from the other side: it is handed [`ErdTable`]s and
//! answers with [`BuilderEvent`]s, and the query itself — the select list, the
//! join types, the SQL — stays with the host pane that owns the form.
//!
//! ## What it holds to
//!
//! * **The same schema always draws the same way.** No clock, no random number
//!   generator and no iteration over a hash map reaches a layout, because a
//!   diagram whose boxes move when nothing changed makes the positions saved in
//!   `erd/<profile-uuid>.json` look wrong. See [`layout`].
//! * **A cyclic foreign-key graph is ordinary, not exceptional.** `A → B → C →
//!   A` and a table that references itself are both things real schemas do, and
//!   both lay out without a panic and without a special case at the call site
//!   (architecture document, §12.4).
//! * **The screen and the file cannot drift apart.** [`svg`] is a second
//!   renderer over the *same* [`layout::measure`] and [`layout::route`] the
//!   canvas draws with, rather than a second guess at them. That is why the
//!   [`NameMode`] is threaded through both: which vocabulary a box is measured
//!   in decides where its text is elided, and an export that chose for itself
//!   would cut the same name at a different character.
//! * **The widget does not save anything.** It raises
//!   [`ErdEvent::LayoutChanged`] once per gesture and the host writes the file,
//!   exactly as the grid asks its host to fetch and to sort. The builder goes
//!   further and holds no query at all: it is a projection of the host's state
//!   and a source of events, and nothing else.
//!
//! Call [`init`] once during application start-up so the key bindings are
//! registered.
//!
//! ```ignore
//! let erd = cx.new(ErdView::new);
//! erd.update(cx, |erd, cx| erd.set_model(model, saved_positions, cx));
//! cx.subscribe(&erd, |pane, erd, event, cx| match event {
//!     ErdEvent::LayoutChanged => pane.save(erd.read(cx).positions(), cx),
//! })
//! .detach();
//! ```

#![warn(missing_docs)]

pub mod builder;
mod canvas;
pub mod layout;
pub mod model;
pub mod svg;
pub mod view;

pub use builder::{BUILDER_KEY_CONTEXT, BuilderEdge, BuilderEvent, BuilderView};
pub use canvas::CANVAS_KEY_CONTEXT;
pub use layout::{HEADER_HEIGHT, NodeRect, ROW_HEIGHT, auto_layout, grid_layout, measure, route};
pub use model::{ErdColumn, ErdModel, ErdRelation, ErdTable, NameMode};
pub use svg::{SvgPalette, to_svg};
pub use view::{ErdEvent, ErdView, KEY_CONTEXT};

use gpui::App;

/// Registers everything the two canvases need before the first window opens.
///
/// Only key bindings, for now; [`rudbman_ui::init`] still has to be called for
/// the palette they draw with.
pub fn init(cx: &mut App) {
    canvas::init(cx);
    view::init(cx);
}
