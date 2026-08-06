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
    App, Context, EventEmitter, FocusHandle, Focusable, KeyBinding, MouseButton, MouseDownEvent,
    MouseMoveEvent, MouseUpEvent, ScrollWheelEvent, Window, actions, canvas, div, prelude::*,
};
use rudbman_ui::theme::{Theme, theme};
use rudbman_ui::to_hex;

use crate::canvas::{
    BoxLabels, CANVAS_KEY_CONTEXT, Drag, Edge, Painted, PanDrag, Scene, Viewport, labels_of,
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
///
/// One case, and it is deliberately not "a box moved": it is "the arrangement
/// is different from the one you saved". It arrives once when a drag ends and
/// once when [`ErdView::auto_arrange`] has run — never while a drag is in
/// flight, because a host that writes a file per frame is a host that writes a
/// hundred files per gesture.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ErdEvent {
    /// The boxes are somewhere other than where they were.
    LayoutChanged,
}

/// A diagram: boxes, the lines between them, and the gestures that move them.
///
/// Created as an entity and rendered as a child element, like the grid:
///
/// ```ignore
/// let erd = cx.new(ErdView::new);
/// cx.subscribe(&erd, |pane, erd, event, cx| match event {
///     ErdEvent::LayoutChanged => pane.save_positions(erd.read(cx).positions(), cx),
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
            .bg(palette.background)
            .on_action(cx.listener(Self::zoom_in))
            .on_action(cx.listener(Self::zoom_out))
            .on_action(cx.listener(Self::zoom_actual))
            .on_action(cx.listener(Self::rearrange))
            .on_mouse_down(MouseButton::Left, cx.listener(Self::on_mouse_down))
            .on_mouse_move(cx.listener(Self::on_mouse_move))
            .on_scroll_wheel(cx.listener(Self::on_scroll_wheel))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::on_mouse_up))
            // A box dragged out of the window lets go with the pointer outside,
            // which only this half sees.
            .on_mouse_up_out(MouseButton::Left, cx.listener(Self::on_mouse_up))
            .child(measure)
            .child(diagram)
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
    use gpui::{Entity, Modifiers, TestAppContext, VisualTestContext};

    use crate::canvas::test_support::{self, drag_to, press, release, wheel, window_point};
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
