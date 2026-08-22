//! The query builder's canvas: tables, their columns, and the joins between
//! them.
//!
//! The same canvas the ERD is drawn on ([`crate::canvas`]) with one thing
//! changed: a press on a *row* means something. In the diagram a box is a
//! table and that is all a press can be about; here a box is a table whose
//! columns are the point, so a click on a row picks a column and a drag from
//! one row to another draws a join (architecture document, §7.7).
//!
//! ## The view is a projection; the state is the host's
//!
//! [`BuilderView`] owns no query. It is handed a table list, a selected-column
//! set and a join list, it draws them, and it answers gestures with
//! [`BuilderEvent`]s — never by editing what it was given. Which columns are
//! selected, what type a join is, what the `WHERE` rows say and what SQL comes
//! out are the host pane's, because all four are edited in a form beside the
//! canvas and a canvas that also owned them would be a second source of truth.
//!
//! That is also why a join's *type* is not drawn and cannot be clicked: hit
//! testing a polyline is harder than a row in a list, and the list is the thing
//! that can be tested without a window.
//!
//! ## What the view does own
//!
//! Where the boxes are. A layout is a property of the picture rather than of
//! the query, so [`BuilderView::set_tables`] keeps a box that is still there
//! where the user left it and finds the new ones a free slot. Nothing here is
//! persisted: the builder's output is SQL, and the editor and the filesystem
//! already know how to keep that.

use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use gpui::{
    AnyElement, App, Context, DragMoveEvent, ElementId, EventEmitter, FocusHandle, Focusable,
    MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, Pixels, Point, ScrollWheelEvent,
    Window, canvas, div, prelude::*,
};
use rudbman_ui::scrollbar::{
    DraggedThumb, Scrollbar, ScrollbarAxis, ScrollbarState, hide_later, hide_now,
};
use rudbman_ui::theme::{Theme, theme, window_translucent};

use crate::canvas::{
    BARS, BoxLabels, CANVAS_KEY_CONTEXT, Drag, Edge, Painted, PanDrag, Scene, Viewport, labels_of,
};
use crate::layout::{NODE_GAP, NodeRect, measure, row_anchor};
use crate::model::{ErdTable, NameMode};

/// The vocabulary the builder's boxes are always drawn in.
///
/// Not a setting, and deliberately not the diagram's: the panel around this
/// canvas generates a `SELECT` out of what is picked on it, and a column picked
/// by its comment would put a sentence where an identifier belongs.
const NAMES: NameMode = NameMode::Physical;

/// Key context the host names when it wants a key of its own to reach the
/// builder's canvas rather than the window.
///
/// The zoom chords are not here: they are bound to
/// [`CANVAS_KEY_CONTEXT`](crate::canvas::CANVAS_KEY_CONTEXT), which this
/// widget's root also names, so that zooming means the same thing on both
/// canvases.
pub const BUILDER_KEY_CONTEXT: &str = "QueryBuilderCanvas";

/// How far the pointer may travel and still count as a click.
///
/// In logical units, so the slack is the same at every scale. Without it a
/// press that shifts a pixel while the button goes down would be a join drawn
/// from a column to nowhere instead of the column being picked.
const CLICK_SLOP: f32 = 3.;

/// What the builder's canvas tells its host about.
///
/// Column coordinates are `(table index, column index)` into the list the host
/// last passed to [`BuilderView::set_tables`] — the same pair the host uses to
/// name a column in its own state, so nothing has to be looked up by name on
/// the way back.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BuilderEvent {
    /// A column row was clicked: the host adds it to the select list, or takes
    /// it out again.
    ColumnToggled {
        /// Which table.
        table: usize,
        /// Which of its columns.
        column: usize,
    },
    /// A join was drawn from one table's column to another's.
    ///
    /// Only ever between two *different* tables: a drag that ends on the box it
    /// began in says nothing, and a self-join is expressed by putting the same
    /// table on the canvas twice under an alias.
    JoinDrawn {
        /// The column the drag began at.
        from: (usize, usize),
        /// The column it ended at.
        to: (usize, usize),
    },
    /// A box was dragged somewhere else.
    ///
    /// Raised once per gesture, as the diagram's own
    /// [`LayoutChanged`](crate::ErdEvent::LayoutChanged) is, so that a host
    /// which reacts to it is not asked to react sixty times a second.
    LayoutChanged,
    /// The user right clicked, and wants the menu for what is under the
    /// pointer.
    ///
    /// The canvas has already taken the focus and outlined the box if it had
    /// to; which items exist, what they are called and what they do is the
    /// host's, because this layer holds no strings (architecture document,
    /// §7.8).
    ContextMenu {
        /// What was under the pointer: `None` for the background, the table
        /// alone for its title band, and the table with one of its columns for
        /// a column row — the same three cases a press is read as, so that the
        /// host can offer "remove this table" and "add this column" from the
        /// one event.
        hit: Option<(usize, Option<usize>)>,
        /// Where the pointer was, in **window** coordinates, which is what the
        /// menu anchors to.
        position: Point<Pixels>,
    },
}

/// One join, as the canvas draws it.
///
/// A pair of column coordinates and nothing else. The join *type* —
/// `INNER`, `LEFT`, and so on — is the host's, because it is chosen in the
/// panel below rather than on the canvas.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BuilderEdge {
    /// The column on the left of the `ON` clause.
    pub from: (usize, usize),
    /// The column on the right of it.
    pub to: (usize, usize),
}

/// A join being dragged out of a column row.
#[derive(Clone, Copy, Debug)]
struct JoinDrag {
    /// The column it started at.
    from: (usize, usize),
    /// Where the pointer was, in logical units, when it took hold.
    start: (f32, f32),
    /// Where the pointer is now, in logical units.
    at: (f32, f32),
    /// Whether it has travelled far enough to be a drag rather than a click.
    moved: bool,
}

/// The query builder's canvas: boxes of columns and the joins between them.
///
/// Created as an entity and rendered as a child element, like the diagram:
///
/// ```ignore
/// let builder = cx.new(BuilderView::new);
/// cx.subscribe(&builder, |pane, builder, event, cx| match event {
///     BuilderEvent::ColumnToggled { table, column } => pane.toggle(*table, *column, cx),
///     BuilderEvent::JoinDrawn { from, to } => pane.add_join(*from, *to, cx),
///     BuilderEvent::LayoutChanged => {}
///     BuilderEvent::ContextMenu { hit, position } => pane.open_menu(*hit, *position, cx),
/// })
/// .detach();
/// ```
pub struct BuilderView {
    focus_handle: FocusHandle,
    /// The tables on the canvas, in the order they are indexed by.
    tables: Vec<ErdTable>,
    /// One rect per table, parallel to `tables`.
    rects: Vec<NodeRect>,
    /// The prepared text, shared into the canvas closures rather than copied.
    labels: Rc<Vec<BoxLabels>>,
    /// The joins as the host gave them, kept so that a table list arriving
    /// after them can be drawn without the host having to send them again.
    joins: Vec<BuilderEdge>,
    /// The joins that resolve against the current tables, ready to draw.
    edges: Rc<Vec<Edge>>,
    /// Which columns are highlighted, as the host last said.
    selected: Rc<HashSet<(usize, usize)>>,
    /// Which box is outlined, if any.
    picked: Option<usize>,
    /// Where the canvas is looking, and how closely.
    viewport: Viewport,
    drag: Option<Drag>,
    panning: Option<PanDrag>,
    join: Option<JoinDrag>,
    /// Whether the bar down the right-hand edge is showing.
    v_bar: ScrollbarState,
    /// Whether the bar along the bottom edge is showing.
    h_bar: ScrollbarState,
    /// The vertical bar's id, unique to this canvas so that two open at once do
    /// not read each other's drags.
    v_bar_id: ElementId,
    /// The horizontal bar's id, for the same reason.
    h_bar_id: ElementId,
}

impl BuilderView {
    /// An empty canvas.
    pub fn new(cx: &mut Context<Self>) -> Self {
        Self {
            focus_handle: cx.focus_handle(),
            tables: Vec::new(),
            rects: Vec::new(),
            labels: Rc::new(Vec::new()),
            joins: Vec::new(),
            edges: Rc::new(Vec::new()),
            selected: Rc::new(HashSet::new()),
            picked: None,
            viewport: Viewport::default(),
            drag: None,
            panning: None,
            join: None,
            v_bar: ScrollbarState::new(),
            h_bar: ScrollbarState::new(),
            v_bar_id: ElementId::from(("builder-vbar", cx.entity_id())),
            h_bar_id: ElementId::from(("builder-hbar", cx.entity_id())),
        }
    }

    /// Replaces the tables on the canvas.
    ///
    /// A table that is still there under the same name keeps the position the
    /// user dragged it to — adding a fifth table must not rearrange the four
    /// already placed — and a new one is given the first free slot in a grid
    /// flow that overlaps nothing. A table that has gone takes its box, its
    /// joins and its selected columns with it.
    ///
    /// Names are matched exactly and are assumed unique, as they are in an
    /// [`ErdTable`] everywhere else in this crate; a host that puts the same
    /// table on the canvas twice hands the second one an alias, which is also
    /// what makes a self-join expressible.
    pub fn set_tables(&mut self, tables: Vec<ErdTable>, cx: &mut Context<Self>) {
        let previous: HashMap<&str, NodeRect> = self
            .tables
            .iter()
            .zip(&self.rects)
            .map(|(table, rect)| (table.name.as_str(), *rect))
            .collect();

        // Kept boxes first, so that the free slots the new ones are given are
        // free of every box that is staying rather than only of the ones that
        // happened to be placed already.
        let mut rects: Vec<Option<NodeRect>> = tables
            .iter()
            .map(|table| {
                let (w, h) = measure(table, NAMES);
                previous.get(table.name.as_str()).map(|old| NodeRect {
                    x: old.x,
                    y: old.y,
                    w,
                    h,
                })
            })
            .collect();
        for index in 0..rects.len() {
            if rects[index].is_some() {
                continue;
            }
            let (w, h) = measure(&tables[index], NAMES);
            let taken: Vec<NodeRect> = rects.iter().flatten().copied().collect();
            let (x, y) = free_slot(&taken, w, h);
            rects[index] = Some(NodeRect { x, y, w, h });
        }
        let rects: Vec<NodeRect> = rects.into_iter().flatten().collect();

        // Old index to new index, by name: a table that is still on the canvas
        // has probably moved in the list, and an edge that kept its old index
        // would silently become an edge to a different table.
        let renamed: HashMap<&str, usize> = tables
            .iter()
            .enumerate()
            .map(|(index, table)| (table.name.as_str(), index))
            .collect();
        let moved: Vec<Option<usize>> = self
            .tables
            .iter()
            .map(|table| renamed.get(table.name.as_str()).copied())
            .collect();
        let follow = |(table, column): (usize, usize)| -> Option<(usize, usize)> {
            let table = *moved.get(table)?.as_ref()?;
            (column < tables[table].columns.len()).then_some((table, column))
        };

        self.joins = self
            .joins
            .iter()
            .filter_map(|join| {
                Some(BuilderEdge {
                    from: follow(join.from)?,
                    to: follow(join.to)?,
                })
            })
            .collect();
        self.selected = Rc::new(
            self.selected
                .iter()
                .filter_map(|pair| follow(*pair))
                .collect(),
        );
        self.picked = self
            .picked
            .and_then(|node| moved.get(node).copied())
            .flatten();

        self.labels = Rc::new(labels_of(&tables, &rects, NAMES));
        self.tables = tables;
        self.rects = rects;
        self.drag = None;
        self.panning = None;
        self.join = None;
        self.resolve_edges();
        cx.notify();
    }

    /// Replaces the joins drawn on the canvas.
    ///
    /// A join naming a table or a column that is not there is not drawn, in
    /// the same spirit as an out-of-range relation in an [`crate::ErdModel`]:
    /// a canvas whose state has fallen a beat behind its host's should draw
    /// what it can rather than panic.
    pub fn set_edges(&mut self, edges: Vec<BuilderEdge>, cx: &mut Context<Self>) {
        self.joins = edges;
        self.resolve_edges();
        cx.notify();
    }

    /// Replaces the set of highlighted columns.
    ///
    /// The host owns it — a column is selected because it is in the select
    /// list, and the select list is edited in the panel as well as on the
    /// canvas — so this is a projection of that state and never a copy the
    /// view goes on to edit.
    pub fn set_selected(&mut self, selected: HashSet<(usize, usize)>, cx: &mut Context<Self>) {
        self.selected = Rc::new(selected);
        cx.notify();
    }

    /// Where every table's box is, keyed by table name.
    ///
    /// The builder saves nothing, so this is here for the host that wants to
    /// put a new table beside the one it was added from rather than for a file.
    pub fn positions(&self) -> HashMap<String, (f32, f32)> {
        self.tables
            .iter()
            .zip(&self.rects)
            .map(|(table, rect)| (table.name.clone(), (rect.x, rect.y)))
            .collect()
    }

    /// The current scale, where 1.0 is one logical unit to one pixel.
    pub fn zoom(&self) -> f32 {
        self.viewport.zoom
    }

    /// The joins that resolve against the current tables.
    fn resolve_edges(&mut self) {
        let resolves = |(table, column): (usize, usize)| {
            self.tables
                .get(table)
                .is_some_and(|table| column < table.columns.len())
        };
        self.edges = Rc::new(
            self.joins
                .iter()
                .filter(|join| resolves(join.from) && resolves(join.to))
                .map(|join| Edge::between_rows(join.from, join.to))
                .collect(),
        );
    }

    fn zoom_in(&mut self, _: &crate::canvas::ZoomIn, _: &mut Window, cx: &mut Context<Self>) {
        if self.viewport.zoom_in() {
            cx.notify();
        }
    }

    fn zoom_out(&mut self, _: &crate::canvas::ZoomOut, _: &mut Window, cx: &mut Context<Self>) {
        if self.viewport.zoom_out() {
            cx.notify();
        }
    }

    fn zoom_actual(
        &mut self,
        _: &crate::canvas::ZoomActual,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.viewport.reset();
        cx.notify();
    }

    fn on_mouse_down(
        &mut self,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.focus_handle.focus(window, cx);

        match self.viewport.hit_row(&self.rects, event.position) {
            // A row: the gesture is about a column, and which of the two it is
            // — a pick or a join — is not known until the button comes up.
            Some((node, Some(row))) => {
                self.picked = Some(node);
                let at = self.viewport.to_logical(event.position);
                self.join = Some(JoinDrag {
                    from: (node, row),
                    start: at,
                    at,
                    moved: false,
                });
            }
            // The title band: the box moves, exactly as in the diagram.
            Some((node, None)) => {
                self.picked = Some(node);
                self.drag = Some(Drag {
                    node,
                    from: self.viewport.to_logical(event.position),
                    origin: (self.rects[node].x, self.rects[node].y),
                });
            }
            None => {
                self.picked = None;
                self.panning = Some(PanDrag {
                    from: event.position,
                    origin: self.viewport.pan,
                });
            }
        }
        cx.notify();
    }

    /// A right click: outline the box, then hand the gesture to the host.
    ///
    /// The same three differences from [`Self::on_mouse_down`] the diagram's
    /// right click has — it moves the outline, it does not clear it on the
    /// background, and it starts nothing — plus one this canvas needs on its
    /// own: a right click on a *column row* does not toggle that column. The
    /// row is named in the event so that the menu can be about it, but a right
    /// click that had also added the column to the select list would have
    /// edited the query before the user chose anything from the menu.
    fn on_right_mouse_down(
        &mut self,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        cx.stop_propagation();
        self.focus_handle.focus(window, cx);

        let hit = self.viewport.hit_row(&self.rects, event.position);
        if let Some((node, _)) = hit {
            self.picked = Some(node);
        }
        cx.emit(BuilderEvent::ContextMenu {
            hit,
            position: event.position,
        });
        cx.notify();
    }

    fn on_mouse_move(&mut self, event: &MouseMoveEvent, _: &mut Window, cx: &mut Context<Self>) {
        if event.pressed_button != Some(MouseButton::Left) {
            return;
        }

        let at = self.viewport.to_logical(event.position);
        if let Some(join) = self.join.as_mut() {
            join.at = at;
            join.moved |= (at.0 - join.start.0).abs() > CLICK_SLOP
                || (at.1 - join.start.1).abs() > CLICK_SLOP;
            cx.notify();
            return;
        }

        if let Some(drag) = self.drag {
            let Some(rect) = self.rects.get_mut(drag.node) else {
                return;
            };
            (rect.x, rect.y) = drag.moved_to(at.0, at.1);
            cx.notify();
            return;
        }

        if let Some(pan) = self.panning {
            self.viewport.pan = pan.pan_at(event.position);
            cx.notify();
        }
    }

    fn on_mouse_up(&mut self, event: &MouseUpEvent, _: &mut Window, cx: &mut Context<Self>) {
        self.panning = None;
        // Before the early returns below: a release that let go of a thumb had
        // no join and no box drag to finish, and would otherwise leave the bar
        // up for as long as the pointer stayed still.
        self.release_thumb(cx);

        if let Some(join) = self.join.take() {
            self.finish_join(join, event.position, cx);
            cx.notify();
            return;
        }

        let Some(drag) = self.drag.take() else {
            return;
        };
        let Some(rect) = self.rects.get(drag.node) else {
            return;
        };
        // A click is not a gesture worth announcing: only say so when the box
        // actually ended up somewhere else.
        if drag.moved(rect) {
            cx.emit(BuilderEvent::LayoutChanged);
        }
    }

    /// Turns a finished column gesture into the one event it meant, if any.
    ///
    /// Three outcomes and they are mutually exclusive: a drag that landed on
    /// another table's row is a join; a press that did not move is a pick;
    /// anything else — a drag onto the background, or from one row of a box to
    /// another row of the same box — meant nothing and says nothing, because
    /// the alternative is a query that gains a term the user did not draw.
    fn finish_join(&mut self, join: JoinDrag, at: Point<Pixels>, cx: &mut Context<Self>) {
        let target = match self.viewport.hit_row(&self.rects, at) {
            Some((node, Some(row))) => Some((node, row)),
            _ => None,
        };

        match target {
            Some(to) if to.0 != join.from.0 => cx.emit(BuilderEvent::JoinDrawn {
                from: join.from,
                to,
            }),
            _ if !join.moved => cx.emit(BuilderEvent::ColumnToggled {
                table: join.from.0,
                column: join.from.1,
            }),
            _ => {}
        }
    }

    fn on_scroll_wheel(
        &mut self,
        event: &ScrollWheelEvent,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.viewport.scroll(event) {
            cx.notify();
        }
    }

    /// The state of whichever bar rides `axis`.
    fn bar_state(&self, axis: ScrollbarAxis) -> &ScrollbarState {
        match axis {
            ScrollbarAxis::Vertical => &self.v_bar,
            ScrollbarAxis::Horizontal => &self.h_bar,
        }
    }

    /// The same, to be moved on.
    fn bar_mut(&mut self, axis: ScrollbarAxis) -> &mut ScrollbarState {
        match axis {
            ScrollbarAxis::Vertical => &mut self.v_bar,
            ScrollbarAxis::Horizontal => &mut self.h_bar,
        }
    }

    /// The bar riding `axis` as it stands this frame, or `None` when there is
    /// no canvas for it to say anything about.
    ///
    /// Its track is the canvas as the last frame *measured* it, which is the
    /// one-frame lag every bar over a scroll container already has: a resize is
    /// corrected in the frame the resize is drawn in.
    fn bar(&self, axis: ScrollbarAxis) -> Option<Scrollbar> {
        let extent = self.viewport.bar_extent(&self.rects, axis)?;
        let id = match axis {
            ScrollbarAxis::Vertical => self.v_bar_id.clone(),
            ScrollbarAxis::Horizontal => self.h_bar_id.clone(),
        };

        Some(
            Scrollbar::new(
                id,
                axis,
                self.viewport.bounds,
                extent.visible,
                extent.scrollable,
                extent.scrolled,
            )
            .fade(self.bar_state(axis).fade()),
        )
    }

    /// The bar riding `axis` as an element, sensor and all.
    fn bar_element(
        &self,
        axis: ScrollbarAxis,
        palette: &Theme,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        self.bar(axis)?
            .on_hover(cx.listener(move |builder, hovered: &bool, _window, cx| {
                builder.hover_bar(axis, *hovered, cx);
            }))
            .render(palette)
    }

    /// Notices that the canvas has moved, and arms the expiry that takes the
    /// bars down again.
    ///
    /// One place for every route that moves a viewport — the wheel, a drag of
    /// the background, a zoom chord, a new table list — because all of them
    /// change how far the canvas is scrolled and none has to announce itself.
    fn watch_bars(&mut self, cx: &mut Context<Self>) {
        for axis in BARS {
            let Some(extent) = self.viewport.bar_extent(&self.rects, axis) else {
                continue;
            };
            if let Some(epoch) = self.bar_mut(axis).moved(extent.scrolled) {
                hide_later(epoch, cx, move |builder: &mut Self| {
                    Some(builder.bar_mut(axis))
                });
            }
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
            hide_now(self, epoch, cx, move |builder: &mut Self| {
                Some(builder.bar_mut(axis))
            });
        }
    }

    /// Pans the canvas to wherever a thumb has been dragged.
    ///
    /// gpui hands a drag to every listener of that drag type, so both bars are
    /// asked and only the one the drag began on answers — see the scrollbar
    /// module for why the payload rather than the event is what says which.
    fn on_drag_bar(
        &mut self,
        event: &DragMoveEvent<DraggedThumb>,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        for axis in BARS {
            let Some(progress) = self.bar(axis).and_then(|bar| bar.dragged(event, cx)) else {
                continue;
            };
            let Some(pan) = self.viewport.panned_to(&self.rects, axis, progress) else {
                continue;
            };
            self.viewport.set_pan_along(axis, pan);
            self.bar_mut(axis).hold();
            cx.notify();
        }
    }

    /// Lets go of whichever thumb was held, and starts the clock that takes its
    /// bar down.
    fn release_thumb(&mut self, cx: &mut Context<Self>) {
        for axis in BARS {
            if let Some(epoch) = self.bar_mut(axis).release() {
                hide_later(epoch, cx, move |builder: &mut Self| {
                    Some(builder.bar_mut(axis))
                });
                cx.notify();
            }
        }
    }

    /// Everything the canvas needs this frame, detached from `self`.
    fn scene(&self, palette: &Theme) -> Scene {
        Scene::new(
            self.labels.clone(),
            self.edges.clone(),
            self.rects.clone(),
            self.viewport,
            self.picked,
            palette.clone(),
        )
        .rows(self.selected.clone())
        .bare()
        .rubber(self.rubber())
    }

    /// The join being dragged, as two logical points.
    ///
    /// It leaves whichever side of the box the pointer is on, so that a join
    /// drawn to the left does not begin by crossing the box it starts in.
    fn rubber(&self) -> Option<((f32, f32), (f32, f32))> {
        let join = self.join.filter(|join| join.moved)?;
        let rect = self.rects.get(join.from.0)?;
        let anchor = row_anchor(rect, join.from.1, join.at.0 >= rect.center_x());
        Some((anchor, join.at))
    }
}

impl EventEmitter<BuilderEvent> for BuilderView {}

impl Focusable for BuilderView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for BuilderView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = theme(cx);
        let view = cx.entity();
        // The canvas's context as well as this widget's, so that the zoom
        // chords bound once for both canvases reach this one too.
        let context = format!("{CANVAS_KEY_CONTEXT} {BUILDER_KEY_CONTEXT}");

        // Both bars, wired as every scrolling surface in the app wires one:
        // notice the surface moved, and arm the expiry from inside the draw
        // that noticed.
        self.watch_bars(cx);
        let vertical = self.bar_element(ScrollbarAxis::Vertical, &palette, cx);
        let horizontal = self.bar_element(ScrollbarAxis::Horizontal, &palette, cx);

        let measure = canvas(
            move |bounds, _window, cx| {
                view.update(cx, |view, cx| {
                    if view.viewport.measured(bounds) {
                        cx.notify();
                    }
                });
            },
            |_, _, _, _| {},
        )
        .absolute()
        .size_full();

        let scene = self.scene(&palette);
        let diagram = canvas(
            move |bounds, window, _cx| scene.prepaint(bounds, window),
            |bounds, painted: Painted, window, cx| painted.paint(bounds, window, cx),
        )
        .absolute()
        .size_full();

        div()
            .key_context(context.as_str())
            .track_focus(&self.focus_handle)
            .relative()
            .size_full()
            .overflow_hidden()
            // Nothing, while the window is translucent. The pane behind the
            // canvas already tints these same pixels with the very same colour,
            // and a second fill over them would either hide the blur or saturate
            // the surface alpha back to opaque; see `app_settings::window_tint`.
            // The boxes and the joins the canvas draws are unaffected — they sit
            // over the background rather than being it.
            .when(!window_translucent(cx), |canvas| {
                canvas.bg(palette.background)
            })
            .on_action(cx.listener(Self::zoom_in))
            .on_action(cx.listener(Self::zoom_out))
            .on_action(cx.listener(Self::zoom_actual))
            .on_mouse_down(MouseButton::Left, cx.listener(Self::on_mouse_down))
            .on_mouse_down(MouseButton::Right, cx.listener(Self::on_right_mouse_down))
            .on_mouse_move(cx.listener(Self::on_mouse_move))
            .on_scroll_wheel(cx.listener(Self::on_scroll_wheel))
            .on_drag_move::<DraggedThumb>(cx.listener(Self::on_drag_bar))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::on_mouse_up))
            // A join dragged out of the window lets go with the pointer
            // outside, which only this half sees.
            .on_mouse_up_out(MouseButton::Left, cx.listener(Self::on_mouse_up))
            .child(measure)
            .child(diagram)
            .children(vertical)
            .children(horizontal)
    }
}

/// The first place in a grid flow that `taken` leaves free for a `w` by `h` box.
///
/// Left to right along a row and then down to the next, but stepping *past the
/// boxes in the way* rather than along a lattice: the boxes on a query builder's
/// canvas differ in width by a factor of three, and a lattice of the new box's
/// own size would leave a gap beside every box wider than it.
///
/// Terminates because each step to the right clears at least one box for good —
/// `x` only ever grows — and each step down clears at least one row of them, so
/// the bounds are the number of boxes already placed. The fallback under
/// everything is there to make the function total, not because it is reachable.
fn free_slot(taken: &[NodeRect], w: f32, h: f32) -> (f32, f32) {
    let mut y = 0f32;
    for _ in 0..=taken.len() {
        let mut x = 0f32;
        // The shallowest box this row ran into, which is how far down the next
        // row has to start to be clear of it.
        let mut lowest: Option<f32> = None;
        for _ in 0..=taken.len() {
            let slot = NodeRect { x, y, w, h };
            let blocked = taken.iter().filter(|rect| rect.overlaps(&slot)).fold(
                None,
                |found: Option<(f32, f32)>, rect| {
                    Some(match found {
                        None => (rect.right(), rect.bottom()),
                        Some((right, bottom)) => {
                            (right.max(rect.right()), bottom.min(rect.bottom()))
                        }
                    })
                },
            );
            let Some((right, bottom)) = blocked else {
                return (x, y);
            };
            x = right + NODE_GAP;
            lowest = Some(lowest.map_or(bottom, |found: f32| found.min(bottom)));
        }
        y = lowest.map_or(y + h + NODE_GAP, |bottom| bottom + NODE_GAP);
    }

    let bottom = taken
        .iter()
        .fold(0f32, |lowest, rect| lowest.max(rect.bottom()));
    (0., bottom + NODE_GAP)
}

#[cfg(test)]
mod tests {
    use gpui::{Modifiers, TestAppContext, VisualTestContext};

    use crate::canvas::test_support::{
        self, drag_to, press, release, right_press, wheel, window_point,
    };
    use crate::layout::{HEADER_HEIGHT, ROW_HEIGHT, row_top};
    use crate::model::ErdColumn;

    use super::*;

    /// Handles over the widget, under the name the tests read best.
    type Handles = test_support::Handles<BuilderView, BuilderEvent>;

    /// A table of `columns` named columns.
    fn table(name: &str, columns: &[&str]) -> ErdTable {
        columns.iter().fold(ErdTable::new(name), |table, column| {
            table.column(ErdColumn::new(*column, "NUMBER(19)"))
        })
    }

    /// Two tables of two columns each, which is the smallest canvas a join can
    /// be drawn on.
    fn tables() -> Vec<ErdTable> {
        vec![
            table("orders", &["id", "customer_id"]),
            table("customers", &["id", "name"]),
        ]
    }

    /// Opens a focused builder over `tables` and hands back its handles.
    fn open(tables: Vec<ErdTable>, cx: &mut TestAppContext) -> (Handles, VisualTestContext) {
        let (handles, mut cx) =
            test_support::open::<BuilderView, BuilderEvent>(cx, BuilderView::new);
        handles.update(&mut cx, |builder, cx| builder.set_tables(tables, cx));
        (handles, cx)
    }

    /// The window point the middle of a box's `row`th row is drawn at.
    fn row_point(
        handles: &Handles,
        cx: &mut VisualTestContext,
        table: usize,
        row: usize,
    ) -> Point<Pixels> {
        let rect = handles.read(cx, |builder| builder.rects[table]);
        window_point(rect.x + rect.w / 2., row_top(&rect, row) + ROW_HEIGHT / 2.)
    }

    /// Asserts that two positions are the same to within a rounding error.
    ///
    /// A gesture goes through window pixels and back, and a box whose middle is
    /// at `92.6` comes back a millionth of a unit out. Nothing the user could
    /// see, and not what any of these tests are about.
    fn assert_near(found: (f32, f32), wanted: (f32, f32)) {
        assert!(
            (found.0 - wanted.0).abs() < 0.01 && (found.1 - wanted.1).abs() < 0.01,
            "{found:?} is not {wanted:?}"
        );
    }

    /// The window point the middle of a box's title band is drawn at.
    fn header_point(handles: &Handles, cx: &mut VisualTestContext, table: usize) -> Point<Pixels> {
        let rect = handles.read(cx, |builder| builder.rects[table]);
        window_point(rect.x + rect.w / 2., rect.y + HEADER_HEIGHT / 2.)
    }

    #[gpui::test]
    fn clicking_a_column_row_toggles_that_column(cx: &mut TestAppContext) {
        let (builder, mut cx) = open(tables(), cx);
        assert_eq!(builder.drain(), Vec::new());

        let at = row_point(&builder, &mut cx, 0, 1);
        press(&mut cx, at);
        // Nothing until the button comes up: it might still become a join.
        assert_eq!(builder.drain(), Vec::new());
        release(&mut cx, at);
        assert_eq!(
            builder.drain(),
            vec![BuilderEvent::ColumnToggled {
                table: 0,
                column: 1
            }]
        );

        // And the other box's rows are its own.
        let at = row_point(&builder, &mut cx, 1, 0);
        press(&mut cx, at);
        release(&mut cx, at);
        assert_eq!(
            builder.drain(),
            vec![BuilderEvent::ColumnToggled {
                table: 1,
                column: 0
            }]
        );
    }

    #[gpui::test]
    fn dragging_from_one_column_to_another_draws_a_join(cx: &mut TestAppContext) {
        let (builder, mut cx) = open(tables(), cx);
        let before = builder.read(&mut cx, |builder| builder.positions());

        let from = row_point(&builder, &mut cx, 0, 1);
        let to = row_point(&builder, &mut cx, 1, 0);
        press(&mut cx, from);
        drag_to(&mut cx, to);
        release(&mut cx, to);

        assert_eq!(
            builder.drain(),
            vec![BuilderEvent::JoinDrawn {
                from: (0, 1),
                to: (1, 0)
            }]
        );
        // Drawing one does not move a box.
        assert_eq!(
            builder.read(&mut cx, |builder| builder.positions()),
            before,
            "the boxes moved while a join was drawn"
        );
    }

    #[gpui::test]
    fn a_drag_inside_one_table_says_nothing(cx: &mut TestAppContext) {
        let (builder, mut cx) = open(tables(), cx);

        let from = row_point(&builder, &mut cx, 0, 0);
        let to = row_point(&builder, &mut cx, 0, 1);
        press(&mut cx, from);
        drag_to(&mut cx, to);
        release(&mut cx, to);
        assert_eq!(builder.drain(), Vec::new());

        // Nor does one that ends on the background.
        let from = row_point(&builder, &mut cx, 0, 0);
        press(&mut cx, from);
        drag_to(&mut cx, window_point(1400., 800.));
        release(&mut cx, window_point(1400., 800.));
        assert_eq!(builder.drain(), Vec::new());
    }

    #[gpui::test]
    fn dragging_a_header_moves_the_box_and_says_so_once(cx: &mut TestAppContext) {
        let (builder, mut cx) = open(tables(), cx);
        let before = builder.read(&mut cx, |builder| builder.positions());

        let at = header_point(&builder, &mut cx, 0);
        press(&mut cx, at);
        drag_to(
            &mut cx,
            gpui::point(at.x + gpui::px(60.), at.y + gpui::px(30.)),
        );
        assert_eq!(builder.drain(), Vec::new());
        drag_to(
            &mut cx,
            gpui::point(at.x + gpui::px(120.), at.y + gpui::px(60.)),
        );
        release(
            &mut cx,
            gpui::point(at.x + gpui::px(120.), at.y + gpui::px(60.)),
        );

        let after = builder.read(&mut cx, |builder| builder.positions());
        assert_near(
            after["orders"],
            (before["orders"].0 + 120., before["orders"].1 + 60.),
        );
        assert_eq!(after["customers"], before["customers"]);
        assert_eq!(builder.drain(), vec![BuilderEvent::LayoutChanged]);
    }

    #[gpui::test]
    fn replacing_the_tables_keeps_the_ones_that_stayed_where_they_were(cx: &mut TestAppContext) {
        let (builder, mut cx) = open(tables(), cx);

        // Move one box somewhere of its own.
        let at = header_point(&builder, &mut cx, 0);
        press(&mut cx, at);
        drag_to(&mut cx, gpui::point(at.x, at.y + gpui::px(400.)));
        release(&mut cx, gpui::point(at.x, at.y + gpui::px(400.)));
        let moved = builder.read(&mut cx, |builder| builder.positions())["orders"];

        // A third table arrives and the first is dropped.
        builder.update(&mut cx, |builder, cx| {
            builder.set_tables(
                vec![
                    table("customers", &["id", "name"]),
                    table("orders", &["id", "customer_id"]),
                    table("items", &["id", "order_id", "sku"]),
                ],
                cx,
            )
        });

        let positions = builder.read(&mut cx, |builder| builder.positions());
        assert_eq!(positions.len(), 3);
        assert_eq!(positions["orders"], moved, "a kept box moved");
        assert!(positions.contains_key("items"));

        // And nothing overlaps anything, which is the point of the free slot.
        let rects = builder.read(&mut cx, |builder| builder.rects.clone());
        for (index, first) in rects.iter().enumerate() {
            for second in &rects[index + 1..] {
                assert!(!first.overlaps(second), "{first:?} overlaps {second:?}");
            }
        }
    }

    #[gpui::test]
    fn a_join_follows_its_tables_and_a_dropped_table_takes_it_with_it(cx: &mut TestAppContext) {
        let (builder, mut cx) = open(tables(), cx);
        builder.update(&mut cx, |builder, cx| {
            builder.set_edges(
                vec![BuilderEdge {
                    from: (0, 1),
                    to: (1, 0),
                }],
                cx,
            )
        });
        assert_eq!(
            builder.read(&mut cx, |builder| builder.edges.len()),
            1,
            "the join was not drawn"
        );

        // The same two tables, the other way round: the join is the same join
        // and now names the other indices.
        builder.update(&mut cx, |builder, cx| {
            builder.set_tables(
                vec![
                    table("customers", &["id", "name"]),
                    table("orders", &["id", "customer_id"]),
                ],
                cx,
            )
        });
        assert_eq!(
            builder.read(&mut cx, |builder| builder.joins.clone()),
            vec![BuilderEdge {
                from: (1, 1),
                to: (0, 0)
            }]
        );

        // One of them leaves, and the join leaves with it.
        builder.update(&mut cx, |builder, cx| {
            builder.set_tables(vec![table("orders", &["id", "customer_id"])], cx)
        });
        assert_eq!(builder.read(&mut cx, |builder| builder.joins.len()), 0);
        assert_eq!(builder.read(&mut cx, |builder| builder.edges.len()), 0);
    }

    #[gpui::test]
    fn a_selected_column_is_drawn_and_follows_its_table(cx: &mut TestAppContext) {
        let (builder, mut cx) = open(tables(), cx);

        let mut selected = HashSet::new();
        selected.insert((0, 1));
        selected.insert((1, 0));
        builder.update(&mut cx, |builder, cx| builder.set_selected(selected, cx));
        // The frame that drew them is the one `update` waited for; if a
        // highlight could panic the canvas, it would have by now.
        assert_eq!(builder.read(&mut cx, |builder| builder.selected.len()), 2);

        builder.update(&mut cx, |builder, cx| {
            builder.set_tables(vec![table("customers", &["id", "name"])], cx)
        });
        let kept: Vec<(usize, usize)> = builder.read(&mut cx, |builder| {
            builder.selected.iter().copied().collect()
        });
        assert_eq!(kept, vec![(0, 0)], "the selection did not follow");
    }

    #[gpui::test]
    fn the_canvas_pans_and_zooms_like_the_diagram(cx: &mut TestAppContext) {
        let (builder, mut cx) = open(tables(), cx);
        assert_eq!(builder.read(&mut cx, |builder| builder.zoom()), 1.);

        // The background drags the whole canvas rather than a box.
        let before = builder.read(&mut cx, |builder| builder.positions());
        press(&mut cx, window_point(1400., 800.));
        drag_to(&mut cx, window_point(1300., 700.));
        release(&mut cx, window_point(1300., 700.));
        assert_eq!(builder.read(&mut cx, |builder| builder.positions()), before);
        assert_eq!(builder.drain(), Vec::new());

        // The wheel pans without the secondary modifier and zooms with it.
        wheel(&mut cx, window_point(100., 100.), -120., Modifiers::none());
        assert_eq!(builder.read(&mut cx, |builder| builder.zoom()), 1.);
        wheel(
            &mut cx,
            window_point(100., 100.),
            -100.,
            Modifiers::secondary_key(),
        );
        let zoomed = builder.read(&mut cx, |builder| builder.zoom());
        assert!(zoomed < 1., "the scale stayed at {zoomed}");
    }

    #[gpui::test]
    fn right_clicking_asks_for_a_menu_and_names_what_was_under_it(cx: &mut TestAppContext) {
        let (builder, mut cx) = open(tables(), cx);

        // A column row: the row is named so the menu can be about it, and the
        // column is not toggled on the way.
        let at = row_point(&builder, &mut cx, 0, 1);
        right_press(&mut cx, at);
        assert_eq!(
            builder.drain(),
            vec![BuilderEvent::ContextMenu {
                hit: Some((0, Some(1))),
                position: at,
            }]
        );
        assert_eq!(builder.read(&mut cx, |builder| builder.picked), Some(0));

        // A title band: the table alone, and the outline follows.
        let at = header_point(&builder, &mut cx, 1);
        right_press(&mut cx, at);
        assert_eq!(
            builder.drain(),
            vec![BuilderEvent::ContextMenu {
                hit: Some((1, None)),
                position: at,
            }]
        );
        assert_eq!(builder.read(&mut cx, |builder| builder.picked), Some(1));

        // The background: the canvas's own menu, with the outline left alone.
        let empty = window_point(1400., 800.);
        right_press(&mut cx, empty);
        assert_eq!(
            builder.drain(),
            vec![BuilderEvent::ContextMenu {
                hit: None,
                position: empty,
            }]
        );
        assert_eq!(builder.read(&mut cx, |builder| builder.picked), Some(1));
    }

    #[gpui::test]
    fn a_right_click_draws_no_join_moves_no_box_and_pans_nothing(cx: &mut TestAppContext) {
        let (builder, mut cx) = open(tables(), cx);
        let positions = builder.read(&mut cx, |builder| builder.positions());
        let pan = builder.read(&mut cx, |builder| builder.viewport.pan);

        // From one table's row to another's, which pressed with the left
        // button would have drawn a join.
        let from = row_point(&builder, &mut cx, 0, 1);
        let to = row_point(&builder, &mut cx, 1, 0);
        right_press(&mut cx, from);
        builder.drain();
        drag_to(&mut cx, to);
        release(&mut cx, to);
        assert_eq!(builder.drain(), Vec::new());

        // From a title band, which would have moved the box.
        let at = header_point(&builder, &mut cx, 0);
        let moved = gpui::point(at.x + gpui::px(120.), at.y + gpui::px(60.));
        right_press(&mut cx, at);
        builder.drain();
        drag_to(&mut cx, moved);
        release(&mut cx, moved);
        assert_eq!(
            builder.read(&mut cx, |builder| builder.positions()),
            positions
        );
        assert_eq!(builder.drain(), Vec::new());

        // And from the background, which would have panned.
        right_press(&mut cx, window_point(1400., 800.));
        builder.drain();
        drag_to(&mut cx, window_point(1300., 700.));
        release(&mut cx, window_point(1300., 700.));
        assert_eq!(builder.read(&mut cx, |builder| builder.viewport.pan), pan);
        assert_eq!(builder.drain(), Vec::new());
    }

    /// The builder never draws a comment, whatever the diagram beside it is
    /// showing: what is picked here becomes a `SELECT`, and a sentence is not
    /// an identifier.
    #[gpui::test]
    fn a_commented_table_is_still_drawn_by_its_identifier(cx: &mut TestAppContext) {
        let commented = vec![
            table("orders", &["id"])
                .comment("everything anyone has ever ordered")
                .column(ErdColumn::new("total", "NUMBER(19)").comment("what it came to")),
        ];
        let (builder, mut cx) = open(commented, cx);

        let drawn = builder.read(&mut cx, |builder| {
            (
                builder.labels[0].title.clone(),
                builder.labels[0].rows[1].name.clone(),
            )
        });
        assert_eq!(drawn.0, "orders");
        assert_eq!(drawn.1, "total");
    }

    #[test]
    fn a_free_slot_overlaps_nothing_that_is_already_there() {
        let mut taken: Vec<NodeRect> = Vec::new();
        for _ in 0..6 {
            let (x, y) = free_slot(&taken, 160., 90.);
            let slot = NodeRect {
                x,
                y,
                w: 160.,
                h: 90.,
            };
            for rect in &taken {
                assert!(!rect.overlaps(&slot), "{rect:?} overlaps {slot:?}");
            }
            taken.push(slot);
        }
    }
}
