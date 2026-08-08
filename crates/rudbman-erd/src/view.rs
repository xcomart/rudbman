//! The diagram widget: a canvas, a pan, a zoom, and boxes that can be dragged.
//!
//! ## What it is not
//!
//! It is not the ERD *panel*. There is no toolbar here, no loading state, no
//! translated string and no file: the host's `ErdPane` wraps this and owns all
//! four, exactly as `QueryPane` wraps `GridView` (architecture document, §7.6).
//! The seam is three calls and one event — [`ErdView::set_model`] to hand a
//! diagram over, [`ErdView::positions`] to read the arrangement back,
//! [`ErdView::export_svg`] to get a file, and [`ErdEvent::LayoutChanged`] to
//! learn that the arrangement is worth saving. The host writes
//! `erd/<profile-uuid>.json` once per gesture because the event fires once per
//! gesture, not once per frame.
//!
//! ## One canvas, drawn by arithmetic
//!
//! Everything inside the diagram — every box, every line, every label — is one
//! gpui canvas. Not one element per table: a box that answers a press needs an
//! id and a hitbox, and a diagram is hundreds of both for a gesture that four
//! numbers settle.
//!
//! Those four numbers, the gestures that change them and the frame they are
//! turned into are [`crate::canvas`], which this widget shares with the query
//! builder (§7.7). What is left here is what only a *schema* diagram means: a
//! model, its foreign keys, the automatic arrangement, and the export.

use std::collections::HashMap;
use std::rc::Rc;

use gpui::{
    AnyElement, App, Context, DragMoveEvent, ElementId, EventEmitter, FocusHandle, Focusable,
    KeyBinding, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, Pixels, Point,
    ScrollWheelEvent, Window, actions, canvas, div, prelude::*,
};
use rudbman_ui::scrollbar::{
    DraggedThumb, Scrollbar, ScrollbarAxis, ScrollbarState, hide_later, hide_now,
};
use rudbman_ui::theme::{Theme, theme, window_translucent};
use rudbman_ui::to_hex;

use crate::canvas::{
    BARS, BoxLabels, CANVAS_KEY_CONTEXT, Drag, Edge, Painted, PanDrag, Scene, Viewport, labels_of,
};
use crate::layout::{NodeRect, auto_layout, grid_layout};
use crate::model::ErdModel;
use crate::svg::{SvgPalette, to_svg};

pub use crate::canvas::{ZoomActual, ZoomIn, ZoomOut};

actions!(
    rudbman_erd,
    [
        /// Re-run the automatic layout over the whole diagram.
        AutoArrange,
    ]
);

/// Key context [`init`] binds this widget's own keys to.
///
/// Public because the host's pane has to name it when it wants a key of its own
/// to reach the diagram rather than the window. The zoom chords are bound to
/// [`CANVAS_KEY_CONTEXT`] instead, which this widget's root also names, because
/// the query builder zooms with the same keys for the same reason.
pub const KEY_CONTEXT: &str = "ErdView";

/// What the diagram tells its host about.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ErdEvent {
    /// The boxes are somewhere other than where they were.
    ///
    /// Deliberately not "a box moved": it is "the arrangement is different from
    /// the one you saved". It arrives once when a drag ends and once when
    /// [`ErdView::auto_arrange`] has run — never while a drag is in flight,
    /// because a host that writes a file per frame is a host that writes a
    /// hundred files per gesture.
    LayoutChanged,
    /// The user right clicked, and wants the menu for what is under the
    /// pointer.
    ///
    /// The diagram has already taken the focus and moved the selection if it
    /// had to; what is left — which items exist, what they are called, which
    /// are greyed out and what they do — is the host's, because this layer
    /// holds no strings (architecture document, §7.8). Everything such a menu
    /// needs is on [`ErdView`] already: [`ErdView::selected`] to name the
    /// table, [`ErdView::auto_arrange`], [`ErdView::export_svg`] and the zoom
    /// actions to act on it.
    ContextMenu {
        /// Which table's box was under the pointer, or `None` for the
        /// background — the difference between a menu about a table and a menu
        /// about the canvas.
        table: Option<usize>,
        /// Where the pointer was, in **window** coordinates, which is what the
        /// menu anchors to.
        position: Point<Pixels>,
    },
}

/// A diagram: boxes, the lines between them, and the gestures that move them.
///
/// Created as an entity and rendered as a child element, like the grid:
///
/// ```ignore
/// let erd = cx.new(ErdView::new);
/// cx.subscribe(&erd, |pane, erd, event, cx| match event {
///     ErdEvent::LayoutChanged => pane.save_positions(erd.read(cx).positions(), cx),
///     ErdEvent::ContextMenu { table, position } => pane.open_menu(*table, *position, cx),
/// })
/// .detach();
/// ```
pub struct ErdView {
    focus_handle: FocusHandle,
    model: ErdModel,
    /// One rect per table, parallel to `model.tables`.
    rects: Vec<NodeRect>,
    /// The prepared text, shared into the canvas closures rather than copied.
    labels: Rc<Vec<BoxLabels>>,
    /// The relations worth drawing, as box pairs, resolved once.
    edges: Rc<Vec<Edge>>,
    /// Where the diagram is looked at from, and how closely.
    viewport: Viewport,
    selected: Option<usize>,
    drag: Option<Drag>,
    panning: Option<PanDrag>,
    /// Whether the bar down the right-hand edge is showing.
    v_bar: ScrollbarState,
    /// Whether the bar along the bottom edge is showing.
    h_bar: ScrollbarState,
    /// The vertical bar's id, unique to this diagram so that two open at once
    /// do not read each other's drags.
    v_bar_id: ElementId,
    /// The horizontal bar's id, for the same reason.
    h_bar_id: ElementId,
}

impl ErdView {
    /// An empty diagram.
    pub fn new(cx: &mut Context<Self>) -> Self {
        Self {
            focus_handle: cx.focus_handle(),
            model: ErdModel::default(),
            rects: Vec::new(),
            labels: Rc::new(Vec::new()),
            edges: Rc::new(Vec::new()),
            viewport: Viewport::default(),
            selected: None,
            drag: None,
            panning: None,
            v_bar: ScrollbarState::new(),
            h_bar: ScrollbarState::new(),
            v_bar_id: ElementId::from(("erd-vbar", cx.entity_id())),
            h_bar_id: ElementId::from(("erd-hbar", cx.entity_id())),
        }
    }

    /// Shows `model`, arranged from `saved` where it says anything.
    ///
    /// `saved` maps a table's name to its top-left corner and is what the host
    /// read out of `erd/<profile-uuid>.json`. Every table is first given a slot
    /// in [`grid_layout`], and then the ones the file knows about are moved to
    /// where the file puts them — so a table added to the schema since the file
    /// was written appears in its grid slot rather than at the origin under
    /// another box, and a table dropped from the schema takes nothing with it.
    pub fn set_model(
        &mut self,
        model: ErdModel,
        saved: HashMap<String, (f32, f32)>,
        cx: &mut Context<Self>,
    ) {
        let mut rects = grid_layout(&model);
        for (index, table) in model.tables.iter().enumerate() {
            if let (Some(rect), Some((x, y))) = (rects.get_mut(index), saved.get(&table.name)) {
                rect.x = *x;
                rect.y = *y;
            }
        }

        self.labels = Rc::new(labels_of(&model.tables, &rects));
        self.edges = Rc::new(
            model
                .valid_relations()
                .map(|relation| Edge::between(relation.from, relation.to))
                .collect(),
        );
        self.model = model;
        self.rects = rects;
        self.selected = None;
        self.drag = None;
        self.panning = None;
        self.viewport.home();
        cx.notify();
    }

    /// Where every table's box is, keyed by table name.
    ///
    /// What the host persists. Keyed by name rather than by index because the
    /// index only means anything alongside the model it came from, and the
    /// point of saving is to survive the next fetch.
    pub fn positions(&self) -> HashMap<String, (f32, f32)> {
        self.model
            .tables
            .iter()
            .zip(&self.rects)
            .map(|(table, rect)| (table.name.clone(), (rect.x, rect.y)))
            .collect()
    }

    /// Re-arranges every box with [`auto_layout`] and announces the result.
    ///
    /// Announces it because the user asked for it: an automatic arrangement the
    /// host does not save is one the user has to ask for again every time the
    /// diagram is opened.
    pub fn auto_arrange(&mut self, cx: &mut Context<Self>) {
        if self.model.tables.is_empty() {
            return;
        }
        self.rects = auto_layout(&self.model);
        cx.emit(ErdEvent::LayoutChanged);
        cx.notify();
    }

    /// The diagram as an SVG document, in the colours it is being drawn in.
    ///
    /// The theme is read here, at the edge, and converted to the CSS strings
    /// [`to_svg`] takes — which is what keeps [`crate::svg`] free of gpui.
    pub fn export_svg(&self, cx: &App) -> String {
        let palette = theme(cx);
        let colours = SvgPalette {
            background: to_hex(palette.background),
            box_fill: to_hex(palette.surface),
            header_fill: to_hex(palette.grid_header),
            border: to_hex(palette.border),
            text: to_hex(palette.text),
            text_muted: to_hex(palette.text_muted),
            line: to_hex(palette.text_muted),
            pk: to_hex(palette.grid_pk),
        };
        to_svg(&self.model, &self.rects, &colours)
    }

    /// The current scale, where 1.0 is one logical unit to one pixel.
    pub fn zoom(&self) -> f32 {
        self.viewport.zoom
    }

    /// The table the last click picked, if any.
    pub fn selected(&self) -> Option<usize> {
        self.selected
    }

    fn zoom_in(&mut self, _: &ZoomIn, _: &mut Window, cx: &mut Context<Self>) {
        if self.viewport.zoom_in() {
            cx.notify();
        }
    }

    fn zoom_out(&mut self, _: &ZoomOut, _: &mut Window, cx: &mut Context<Self>) {
        if self.viewport.zoom_out() {
            cx.notify();
        }
    }

    fn zoom_actual(&mut self, _: &ZoomActual, _: &mut Window, cx: &mut Context<Self>) {
        self.viewport.reset();
        cx.notify();
    }

    fn rearrange(&mut self, _: &AutoArrange, _: &mut Window, cx: &mut Context<Self>) {
        self.auto_arrange(cx);
    }

    fn on_mouse_down(
        &mut self,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.focus_handle.focus(window);

        match self.viewport.hit(&self.rects, event.position) {
            Some(node) => {
                self.selected = Some(node);
                self.drag = Some(Drag {
                    node,
                    from: self.viewport.to_logical(event.position),
                    origin: (self.rects[node].x, self.rects[node].y),
                });
            }
            None => {
                self.selected = None;
                self.panning = Some(PanDrag {
                    from: event.position,
                    origin: self.viewport.pan,
                });
            }
        }
        cx.notify();
    }

    /// A right click: move the selection onto the box, then hand the gesture to
    /// the host.
    ///
    /// Three deliberate differences from [`Self::on_mouse_down`]. It takes the
    /// selection with it, because the menu the host is about to open is about
    /// *that* table and a menu whose commands act on a box other than the one
    /// under the pointer acts by surprise (§7.8). It does not take the
    /// selection *away* on the background: a right click is a request for a
    /// menu rather than a deselect, and the canvas menu it asks for says
    /// nothing about which box is picked. And it starts neither a drag nor a
    /// pan — the pointer is about to be over a menu, and a gesture left half
    /// open would move a box the moment the menu closed.
    fn on_right_mouse_down(
        &mut self,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        cx.stop_propagation();
        self.focus_handle.focus(window);

        let table = self.viewport.hit(&self.rects, event.position);
        if table.is_some() {
            self.selected = table;
        }
        cx.emit(ErdEvent::ContextMenu {
            table,
            position: event.position,
        });
        cx.notify();
    }

    fn on_mouse_move(&mut self, event: &MouseMoveEvent, _: &mut Window, cx: &mut Context<Self>) {
        if event.pressed_button != Some(MouseButton::Left) {
            return;
        }

        if let Some(drag) = self.drag {
            let (x, y) = self.viewport.to_logical(event.position);
            let Some(rect) = self.rects.get_mut(drag.node) else {
                return;
            };
            (rect.x, rect.y) = drag.moved_to(x, y);
            cx.notify();
            return;
        }

        if let Some(pan) = self.panning {
            self.viewport.pan = pan.pan_at(event.position);
            cx.notify();
        }
    }

    fn on_mouse_up(&mut self, _: &MouseUpEvent, _: &mut Window, cx: &mut Context<Self>) {
        self.panning = None;
        // Before the early returns below: a release that let go of a thumb had
        // no box drag and no pan to end, and would otherwise leave the bar up
        // for as long as the pointer stayed still.
        self.release_thumb(cx);

        let Some(drag) = self.drag.take() else {
            return;
        };
        let Some(rect) = self.rects.get(drag.node) else {
            return;
        };
        // A click is not a gesture worth saving: only announce when the box
        // actually ended up somewhere else.
        if drag.moved(rect) {
            cx.emit(ErdEvent::LayoutChanged);
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
    /// no diagram for it to say anything about.
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
            .on_hover(cx.listener(move |view, hovered: &bool, _window, cx| {
                view.hover_bar(axis, *hovered, cx);
            }))
            .render(palette)
    }

    /// Notices that the canvas has moved, and arms the expiry that takes the
    /// bars down again.
    ///
    /// One place for every route that moves a viewport — the wheel, a drag of
    /// the background, a zoom chord, a new model — because all of them change
    /// how far the canvas is scrolled and none of them has to announce itself.
    fn watch_bars(&mut self, cx: &mut Context<Self>) {
        for axis in BARS {
            let Some(extent) = self.viewport.bar_extent(&self.rects, axis) else {
                continue;
            };
            if let Some(epoch) = self.bar_mut(axis).moved(extent.scrolled) {
                hide_later(epoch, cx, move |view: &mut Self| Some(view.bar_mut(axis)));
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
            hide_now(self, epoch, cx, move |view: &mut Self| {
                Some(view.bar_mut(axis))
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
                hide_later(epoch, cx, move |view: &mut Self| Some(view.bar_mut(axis)));
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
            self.selected,
            palette.clone(),
        )
    }
}

impl EventEmitter<ErdEvent> for ErdView {}

impl Focusable for ErdView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for ErdView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = theme(cx);
        let view = cx.entity();
        // Both contexts: the zoom chords are the canvas's and the automatic
        // arrangement is this widget's. gpui reads a key context as a set, so a
        // binding scoped to either one finds this element.
        let context = format!("{CANVAS_KEY_CONTEXT} {KEY_CONTEXT}");

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
            .on_action(cx.listener(Self::rearrange))
            .on_mouse_down(MouseButton::Left, cx.listener(Self::on_mouse_down))
            .on_mouse_down(MouseButton::Right, cx.listener(Self::on_right_mouse_down))
            .on_mouse_move(cx.listener(Self::on_mouse_move))
            .on_scroll_wheel(cx.listener(Self::on_scroll_wheel))
            .on_drag_move::<DraggedThumb>(cx.listener(Self::on_drag_bar))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::on_mouse_up))
            // A box dragged out of the window lets go with the pointer outside,
            // which only this half sees.
            .on_mouse_up_out(MouseButton::Left, cx.listener(Self::on_mouse_up))
            .child(measure)
            .child(diagram)
            .children(vertical)
            .children(horizontal)
    }
}

/// Registers the key bindings only [`ErdView`] has.
///
/// Scoped to the `ErdView` key context; the zoom chords both canvases share are
/// registered by [`crate::canvas::init`] against `ErdCanvas` instead.
pub fn init(cx: &mut App) {
    let modifier = if cfg!(target_os = "macos") {
        "cmd"
    } else {
        "ctrl"
    };

    cx.bind_keys([KeyBinding::new(
        &format!("{modifier}-shift-a"),
        AutoArrange,
        Some(KEY_CONTEXT),
    )]);
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use gpui::{Entity, Modifiers, MouseMoveEvent, TestAppContext, VisualTestContext};
    use rudbman_ui::scrollbar::{FADE_OUT, Fade, SCROLL_LINGER};

    use crate::canvas::test_support::{
        self, drag_to, press, release, right_press, wheel, window_point,
    };
    use crate::canvas::{MAX_ZOOM, MIN_ZOOM};
    use crate::model::{ErdColumn, ErdRelation, ErdTable};

    use super::*;

    /// Everything a test reads back.
    ///
    /// The diagram's own name over [`test_support::Handles`], which is the same
    /// three calls for either widget.
    struct Handles {
        erd: Entity<ErdView>,
        shared: test_support::Handles<ErdView, ErdEvent>,
    }

    impl Handles {
        fn drain(&self) -> Vec<ErdEvent> {
            self.shared.drain()
        }

        fn read<R>(&self, cx: &mut VisualTestContext, f: impl FnOnce(&ErdView) -> R) -> R {
            self.shared.read(cx, f)
        }

        fn update(
            &self,
            cx: &mut VisualTestContext,
            f: impl FnOnce(&mut ErdView, &mut Context<ErdView>),
        ) {
            self.shared.update(cx, f);
        }
    }

    /// Two tables and the key between them, which is the smallest diagram with
    /// a line in it.
    fn model() -> ErdModel {
        ErdModel {
            tables: vec![
                ErdTable::new("orders")
                    .column(ErdColumn::new("id", "NUMBER(19)").primary_key())
                    .column(ErdColumn::new("customer_id", "NUMBER(19)").foreign_key()),
                ErdTable::new("customers")
                    .column(ErdColumn::new("id", "NUMBER(19)").primary_key())
                    .column(ErdColumn::new("name", "VARCHAR2(120)")),
            ],
            relations: vec![ErdRelation {
                name: Some("fk_orders_customer".into()),
                from: 0,
                to: 1,
                columns: vec![("customer_id".into(), "id".into())],
            }],
        }
    }

    /// Opens a focused diagram over `model` and hands back its handles.
    fn open(
        model: ErdModel,
        saved: HashMap<String, (f32, f32)>,
        cx: &mut TestAppContext,
    ) -> (Handles, VisualTestContext) {
        let (shared, mut cx) = test_support::open::<ErdView, ErdEvent>(cx, ErdView::new);
        let handles = Handles {
            erd: shared.widget.clone(),
            shared,
        };
        handles.update(&mut cx, |erd, cx| erd.set_model(model, saved, cx));
        (handles, cx)
    }

    #[gpui::test]
    fn dragging_a_box_moves_it_and_says_so_once(cx: &mut TestAppContext) {
        let (erd, mut cx) = open(model(), HashMap::new(), cx);
        assert_eq!(erd.drain(), Vec::new());

        // The first box starts at the origin of the diagram, so a press ten
        // logical units into it lands on its title band.
        press(&mut cx, window_point(10., 10.));
        assert_eq!(erd.read(&mut cx, |erd| erd.selected()), Some(0));

        drag_to(&mut cx, window_point(70., 40.));
        // Nothing is announced until the gesture is over.
        assert_eq!(erd.drain(), Vec::new());

        drag_to(&mut cx, window_point(130., 70.));
        release(&mut cx, window_point(130., 70.));

        let positions = erd.read(&mut cx, |erd| erd.positions());
        assert_eq!(positions.get("orders"), Some(&(120., 60.)));
        // The other box did not move with it.
        let other = erd.read(&mut cx, |erd| erd.positions());
        assert_ne!(other.get("customers"), Some(&(120., 60.)));

        assert_eq!(erd.drain(), vec![ErdEvent::LayoutChanged]);
    }

    #[gpui::test]
    fn a_click_that_moves_nothing_announces_nothing(cx: &mut TestAppContext) {
        let (erd, mut cx) = open(model(), HashMap::new(), cx);

        press(&mut cx, window_point(10., 10.));
        release(&mut cx, window_point(10., 10.));
        assert_eq!(erd.drain(), Vec::new());

        // A press on the background clears the selection. Well past both
        // boxes, but still inside the test window.
        press(&mut cx, window_point(1400., 800.));
        release(&mut cx, window_point(1400., 800.));
        assert_eq!(erd.read(&mut cx, |erd| erd.selected()), None);
    }

    #[gpui::test]
    fn the_wheel_zooms_only_with_the_secondary_modifier(cx: &mut TestAppContext) {
        let (erd, mut cx) = open(model(), HashMap::new(), cx);
        assert_eq!(erd.read(&mut cx, |erd| erd.zoom()), 1.);

        // Without it the wheel pans, and the scale is untouched.
        wheel(&mut cx, window_point(100., 100.), -120., Modifiers::none());
        assert_eq!(erd.read(&mut cx, |erd| erd.zoom()), 1.);

        wheel(
            &mut cx,
            window_point(100., 100.),
            -100.,
            Modifiers::secondary_key(),
        );
        let zoomed = erd.read(&mut cx, |erd| erd.zoom());
        assert!(zoomed < 1., "the scale stayed at {zoomed}");

        // And it stops at the floor rather than running away.
        for _ in 0..40 {
            wheel(
                &mut cx,
                window_point(100., 100.),
                -200.,
                Modifiers::secondary_key(),
            );
        }
        assert_eq!(erd.read(&mut cx, |erd| erd.zoom()), MIN_ZOOM);

        for _ in 0..80 {
            wheel(
                &mut cx,
                window_point(100., 100.),
                200.,
                Modifiers::secondary_key(),
            );
        }
        assert_eq!(erd.read(&mut cx, |erd| erd.zoom()), MAX_ZOOM);
    }

    #[gpui::test]
    fn dragging_the_background_pans_without_moving_a_box(cx: &mut TestAppContext) {
        let (erd, mut cx) = open(model(), HashMap::new(), cx);
        let before = erd.read(&mut cx, |erd| erd.positions());

        // Well past both boxes, which the grid layout puts near the origin.
        press(&mut cx, window_point(1400., 800.));
        drag_to(&mut cx, window_point(1300., 700.));
        release(&mut cx, window_point(1300., 700.));

        assert_eq!(erd.read(&mut cx, |erd| erd.positions()), before);
        assert_eq!(erd.drain(), Vec::new());
    }

    #[gpui::test]
    fn saved_positions_are_honoured_and_the_rest_fall_into_the_grid(cx: &mut TestAppContext) {
        let mut saved = HashMap::new();
        saved.insert("customers".to_string(), (500., 300.));
        let (erd, mut cx) = open(model(), saved, cx);

        let positions = erd.read(&mut cx, |erd| erd.positions());
        assert_eq!(positions.get("customers"), Some(&(500., 300.)));
        // The table the file said nothing about keeps its grid slot, which for
        // the first table is the origin.
        assert_eq!(positions.get("orders"), Some(&(0., 0.)));
    }

    #[gpui::test]
    fn auto_arranging_announces_the_new_layout(cx: &mut TestAppContext) {
        let (erd, mut cx) = open(model(), HashMap::new(), cx);
        erd.drain();

        erd.update(&mut cx, |erd, cx| erd.auto_arrange(cx));
        assert_eq!(erd.drain(), vec![ErdEvent::LayoutChanged]);

        // The referencing table ends up to the left of the one it references.
        let positions = erd.read(&mut cx, |erd| erd.positions());
        assert!(positions["orders"].0 < positions["customers"].0);
    }

    /// The window point the top of a box's title band is drawn at.
    fn header_point(handles: &Handles, cx: &mut VisualTestContext, table: usize) -> Point<Pixels> {
        let rect = handles.read(cx, |erd| erd.rects[table]);
        window_point(rect.x + rect.w / 2., rect.y + 10.)
    }

    #[gpui::test]
    fn right_clicking_asks_for_a_menu_and_takes_the_selection_with_it(cx: &mut TestAppContext) {
        let (erd, mut cx) = open(model(), HashMap::new(), cx);

        let first = header_point(&erd, &mut cx, 0);
        right_press(&mut cx, first);
        assert_eq!(
            erd.drain(),
            vec![ErdEvent::ContextMenu {
                table: Some(0),
                position: first,
            }]
        );
        assert_eq!(erd.read(&mut cx, |erd| erd.selected()), Some(0));

        // The other box: the menu is about it, so the selection is too.
        let second = header_point(&erd, &mut cx, 1);
        right_press(&mut cx, second);
        assert_eq!(
            erd.drain(),
            vec![ErdEvent::ContextMenu {
                table: Some(1),
                position: second,
            }]
        );
        assert_eq!(erd.read(&mut cx, |erd| erd.selected()), Some(1));

        // The background asks for the canvas's own menu, and does not take the
        // selection away on the way.
        let empty = window_point(1400., 800.);
        right_press(&mut cx, empty);
        assert_eq!(
            erd.drain(),
            vec![ErdEvent::ContextMenu {
                table: None,
                position: empty,
            }]
        );
        assert_eq!(erd.read(&mut cx, |erd| erd.selected()), Some(1));
    }

    #[gpui::test]
    fn a_right_click_starts_neither_a_drag_nor_a_pan(cx: &mut TestAppContext) {
        let (erd, mut cx) = open(model(), HashMap::new(), cx);
        let positions = erd.read(&mut cx, |erd| erd.positions());
        let pan = erd.read(&mut cx, |erd| erd.viewport.pan);

        // A press on a box, and then the moves that would have dragged it.
        right_press(&mut cx, window_point(10., 10.));
        erd.drain();
        drag_to(&mut cx, window_point(130., 70.));
        release(&mut cx, window_point(130., 70.));
        assert_eq!(erd.read(&mut cx, |erd| erd.positions()), positions);
        assert_eq!(erd.drain(), Vec::new());

        // And on the background, which would have panned.
        right_press(&mut cx, window_point(1400., 800.));
        erd.drain();
        drag_to(&mut cx, window_point(1300., 700.));
        release(&mut cx, window_point(1300., 700.));
        assert_eq!(erd.read(&mut cx, |erd| erd.viewport.pan), pan);
        assert_eq!(erd.read(&mut cx, |erd| erd.positions()), positions);
        assert_eq!(erd.drain(), Vec::new());
    }

    /// Long enough for a timer that was due to have fired.
    const A_MOMENT: Duration = Duration::from_millis(10);

    /// The second box parked well past the far corner of any test window, so
    /// that the diagram is bigger than what can be seen of it and there is
    /// something for a bar to say.
    fn spread_out() -> HashMap<String, (f32, f32)> {
        HashMap::from([("customers".to_string(), (4_000., 3_000.))])
    }

    /// A canvas nobody has moved shows no bars; a wheel brings up the bar for
    /// the axis it moved, and only that one.
    #[gpui::test]
    fn scrolling_the_canvas_brings_the_overlay_bars_up(cx: &mut TestAppContext) {
        let (erd, mut cx) = open(model(), spread_out(), cx);
        assert_eq!(
            erd.read(&mut cx, |erd| erd.v_bar.fade()),
            Fade::Hidden,
            "a bar was up before anything moved"
        );

        wheel(&mut cx, window_point(100., 100.), -120., Modifiers::none());
        assert_eq!(
            erd.read(&mut cx, |erd| erd.v_bar.fade()),
            Fade::In,
            "a scrolled canvas did not fade its bar in"
        );
        // The wheel moved the canvas up and down and nothing sideways, so the
        // bar along the bottom has nothing to report.
        assert_eq!(
            erd.read(&mut cx, |erd| erd.h_bar.fade()),
            Fade::Hidden,
            "the other axis put its bar up as well"
        );
    }

    /// And it goes away on its own: up for the linger that tells a stopped
    /// wheel from a paused one, then a fade during which it is still drawn, and
    /// only then gone.
    #[gpui::test]
    fn the_overlay_bar_fades_out_once_the_canvas_stops(cx: &mut TestAppContext) {
        let (erd, mut cx) = open(model(), spread_out(), cx);
        wheel(&mut cx, window_point(100., 100.), -120., Modifiers::none());

        cx.executor().advance_clock(SCROLL_LINGER / 2);
        cx.run_until_parked();
        assert_eq!(
            erd.read(&mut cx, |erd| erd.v_bar.fade()),
            Fade::In,
            "the bar started going before its time was up"
        );

        cx.executor().advance_clock(SCROLL_LINGER / 2 + A_MOMENT);
        cx.run_until_parked();
        assert_eq!(
            erd.read(&mut cx, |erd| erd.v_bar.fade()),
            Fade::Out,
            "the bar did not start fading when its time was up"
        );

        cx.executor().advance_clock(FADE_OUT + A_MOMENT);
        cx.run_until_parked();
        assert_eq!(
            erd.read(&mut cx, |erd| erd.v_bar.fade()),
            Fade::Hidden,
            "the bar never finished going"
        );
    }

    /// The pointer resting on the edge a bar rides brings it up with nothing
    /// having scrolled, and it goes the moment the pointer leaves — no linger,
    /// because a pointer leaving announces itself.
    #[gpui::test]
    fn the_pointer_on_the_edge_brings_the_bar_up(cx: &mut TestAppContext) {
        let (erd, mut cx) = open(model(), spread_out(), cx);
        let bounds = erd.read(&mut cx, |erd| erd.viewport.bounds);
        let middle = bounds.origin.y + bounds.size.height / 2.;
        let on_the_edge = gpui::point(bounds.origin.x + bounds.size.width - gpui::px(3.), middle);
        let off_it = gpui::point(bounds.origin.x + bounds.size.width / 2., middle);

        hover(&mut cx, on_the_edge);
        assert_eq!(
            erd.read(&mut cx, |erd| erd.v_bar.fade()),
            Fade::In,
            "the pointer on the edge did not bring the bar up"
        );

        hover(&mut cx, off_it);
        assert_eq!(
            erd.read(&mut cx, |erd| erd.v_bar.fade()),
            Fade::Out,
            "the bar stayed up after the pointer left"
        );
    }

    /// Moves the pointer to `at` with no button held.
    fn hover(cx: &mut VisualTestContext, at: Point<Pixels>) {
        cx.simulate_event(MouseMoveEvent {
            position: at,
            pressed_button: None,
            modifiers: Modifiers::none(),
        });
        cx.run_until_parked();
    }

    #[gpui::test]
    fn the_export_is_written_in_the_theme_s_colours(cx: &mut TestAppContext) {
        let (erd, mut cx) = open(model(), HashMap::new(), cx);
        let svg = cx.update(|_, cx| erd.erd.read(cx).export_svg(cx));

        assert!(svg.starts_with("<svg"));
        assert!(svg.contains("orders"));
        assert!(svg.contains("customers"));

        let background = cx.update(|_, cx| to_hex(theme(cx).background));
        assert!(
            svg.contains(&background),
            "{background} is not in the export"
        );
    }
}
