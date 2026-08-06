//! The widget: a canvas, a pan, a zoom, and boxes that can be dragged.
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
//! [`canvas`]. Not one element per table: a box that answers a press needs an
//! id and a hitbox, and a diagram is hundreds of both for a gesture that four
//! numbers settle. So the pointer is resolved against [`NodeRect`]s in reverse
//! order (the last box drawn is the first box hit), which is the judgement the
//! result grid reached about cells and for the same reason.
//!
//! ## Pan and zoom are fields, not a scroll container
//!
//! A gpui scroll container lays its content out in full and then clips it,
//! which is the cost this crate exists to avoid, and it cannot zoom at all. So
//! the transform is two of the view's own fields — a translation in screen
//! pixels and a scale — and the canvas applies them itself:
//! `screen = bounds.origin + logical * zoom + pan`. Both halves of a gesture
//! read the same two numbers, so the picture and the hit test cannot disagree.
//!
//! A drag is computed **absolutely**, from the pointer position and the box
//! position that were recorded when it began, rather than by accumulating
//! per-event deltas: an accumulated drag drifts by a fraction of a pixel per
//! event, and over a long drag the box ends up somewhere the pointer is not.
//! The grid's column resize is written the same way.

use std::collections::HashMap;
use std::rc::Rc;

use gpui::{
    App, Bounds, ContentMask, Context, EventEmitter, FocusHandle, Focusable, Hsla, IsZero,
    KeyBinding, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, PaintQuad, PathBuilder,
    Pixels, Point, ScrollWheelEvent, ShapedLine, SharedString, TextRun, Window, actions, canvas,
    div, fill, outline, point, prelude::*, px, size,
};
use rudbman_ui::theme::{Theme, theme};
use rudbman_ui::to_hex;

use crate::layout::{
    BOX_PADDING, HEADER_HEIGHT, NodeRect, ROW_HEIGHT, auto_layout, crow_foot, elide, grid_layout,
    head_direction, key_bar, route, split_row, tail_direction,
};
use crate::model::ErdModel;
use crate::svg::{SvgPalette, to_svg};

actions!(
    rudbman_erd,
    [
        /// Zoom the diagram in one step, about the middle of the viewport.
        ZoomIn,
        /// Zoom the diagram out one step, about the middle of the viewport.
        ZoomOut,
        /// Return to 1:1 and to the top-left corner of the diagram.
        ZoomActual,
        /// Re-run the automatic layout over the whole diagram.
        AutoArrange,
    ]
);

/// Key context [`init`] binds this widget's keys to.
///
/// Public because the host's pane has to name it when it wants a key of its own
/// to reach the diagram rather than the window.
pub const KEY_CONTEXT: &str = "ErdView";

/// Closest the diagram may be zoomed.
const MAX_ZOOM: f32 = 2.;

/// Furthest the diagram may be zoomed out.
///
/// Past a quarter the labels are unreadable anyway, and the point of zooming
/// out is to see the shape of the schema rather than to read it.
const MIN_ZOOM: f32 = 0.25;

/// How much one [`ZoomIn`] or [`ZoomOut`] moves the scale by.
const ZOOM_STEP: f32 = 1.25;

/// How much a pixel of wheel travel moves the scale by, while the secondary
/// modifier is held.
const WHEEL_ZOOM_RATE: f32 = 0.0025;

/// Below this scale the labels are not shaped at all.
///
/// They would be illegible, and shaping is the one part of drawing a diagram
/// whose cost is proportional to the number of *columns* rather than to the
/// number of tables.
const TEXT_FLOOR: f32 = 0.4;

/// Where the diagram sits before anything has been panned.
const INITIAL_PAN: f32 = 24.;

/// Text size of a column row, in logical units.
const FONT_SIZE: f32 = 12.;

/// Text size of a box's title, in logical units.
const TITLE_SIZE: f32 = 13.;

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

/// Which palette slot a column's name is drawn in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RowKind {
    /// An ordinary column.
    Plain,
    /// A column of the primary key.
    PrimaryKey,
    /// A column of a foreign key that is not also part of the primary key.
    ForeignKey,
}

/// One column row, already cut to fit the box it is drawn in.
struct RowLabel {
    name: SharedString,
    type_name: SharedString,
    kind: RowKind,
}

/// One box's text, prepared once when the model arrives.
///
/// Elision depends only on a box's *width*, and a box's width never changes
/// once it has been measured — dragging moves boxes, it does not resize them —
/// so the cutting is done here and not once per frame.
struct BoxLabels {
    title: SharedString,
    rows: Vec<RowLabel>,
}

/// A box being dragged.
#[derive(Clone, Copy, Debug)]
struct Drag {
    /// Which box.
    node: usize,
    /// Where the pointer was, in logical units, when it took hold.
    from: (f32, f32),
    /// Where the box was then, so that the drag is absolute rather than a
    /// running total that could drift.
    origin: (f32, f32),
}

/// The background being dragged.
#[derive(Clone, Copy, Debug)]
struct PanDrag {
    /// Where the pointer was, in window coordinates, when it took hold.
    from: Point<Pixels>,
    /// The pan offset then.
    origin: Point<f32>,
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
    /// The relations worth drawing, as index pairs, resolved once.
    relations: Rc<Vec<(usize, usize)>>,
    /// Translation from logical units to the viewport, in screen pixels.
    pan: Point<f32>,
    /// Scale from logical units to screen pixels.
    zoom: f32,
    selected: Option<usize>,
    drag: Option<Drag>,
    panning: Option<PanDrag>,
    /// Where the canvas was, as of the last frame that measured it.
    bounds: Bounds<Pixels>,
}

impl ErdView {
    /// An empty diagram.
    pub fn new(cx: &mut Context<Self>) -> Self {
        Self {
            focus_handle: cx.focus_handle(),
            model: ErdModel::default(),
            rects: Vec::new(),
            labels: Rc::new(Vec::new()),
            relations: Rc::new(Vec::new()),
            pan: point(INITIAL_PAN, INITIAL_PAN),
            zoom: 1.,
            selected: None,
            drag: None,
            panning: None,
            bounds: Bounds::default(),
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

        self.labels = Rc::new(labels_of(&model, &rects));
        self.relations = Rc::new(
            model
                .valid_relations()
                .map(|relation| (relation.from, relation.to))
                .collect(),
        );
        self.model = model;
        self.rects = rects;
        self.selected = None;
        self.drag = None;
        self.panning = None;
        self.pan = point(INITIAL_PAN, INITIAL_PAN);
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
        self.zoom
    }

    /// The table the last click picked, if any.
    pub fn selected(&self) -> Option<usize> {
        self.selected
    }

    /// Where `at` falls in the diagram's own coordinates.
    fn to_logical(&self, at: Point<Pixels>) -> (f32, f32) {
        let x = f32::from(at.x - self.bounds.origin.x) - self.pan.x;
        let y = f32::from(at.y - self.bounds.origin.y) - self.pan.y;
        (x / self.zoom, y / self.zoom)
    }

    /// Which box is under `at`, if any.
    ///
    /// Reverse order, because the boxes are drawn in table order and the one
    /// drawn last is the one the user sees on top.
    fn hit(&self, at: Point<Pixels>) -> Option<usize> {
        if !self.bounds.contains(&at) {
            return None;
        }
        let (x, y) = self.to_logical(at);
        self.rects
            .iter()
            .enumerate()
            .rev()
            .find(|(_, rect)| rect.contains(x, y))
            .map(|(index, _)| index)
    }

    /// Notes where the canvas turned out to be.
    ///
    /// Called from the measuring [`canvas`] during prepaint, and only asks for
    /// another frame when the box actually moved — a repaint per frame would
    /// be a repaint forever.
    fn measured(&mut self, bounds: Bounds<Pixels>, cx: &mut Context<Self>) {
        let moved = (f32::from(bounds.origin.x - self.bounds.origin.x)).abs() >= 0.5
            || (f32::from(bounds.origin.y - self.bounds.origin.y)).abs() >= 0.5
            || (f32::from(bounds.size.width - self.bounds.size.width)).abs() >= 0.5
            || (f32::from(bounds.size.height - self.bounds.size.height)).abs() >= 0.5;
        if !moved {
            return;
        }
        self.bounds = bounds;
        cx.notify();
    }

    /// Scales to `wanted`, keeping whatever is under `anchor` under it.
    fn zoom_to(&mut self, wanted: f32, anchor: Point<Pixels>, cx: &mut Context<Self>) {
        let zoom = wanted.clamp(MIN_ZOOM, MAX_ZOOM);
        if (zoom - self.zoom).abs() < 1e-4 {
            return;
        }

        // The point under the pointer before the scale changes is the point the
        // pan is then chosen to put back under it.
        let (x, y) = self.to_logical(anchor);
        let local_x = f32::from(anchor.x - self.bounds.origin.x);
        let local_y = f32::from(anchor.y - self.bounds.origin.y);
        self.zoom = zoom;
        self.pan = point(local_x - x * zoom, local_y - y * zoom);
        cx.notify();
    }

    /// The middle of the viewport, which is what a keyboard zoom is about.
    fn viewport_centre(&self) -> Point<Pixels> {
        point(
            self.bounds.origin.x + self.bounds.size.width / 2.,
            self.bounds.origin.y + self.bounds.size.height / 2.,
        )
    }

    fn zoom_in(&mut self, _: &ZoomIn, _: &mut Window, cx: &mut Context<Self>) {
        self.zoom_to(self.zoom * ZOOM_STEP, self.viewport_centre(), cx);
    }

    fn zoom_out(&mut self, _: &ZoomOut, _: &mut Window, cx: &mut Context<Self>) {
        self.zoom_to(self.zoom / ZOOM_STEP, self.viewport_centre(), cx);
    }

    fn zoom_actual(&mut self, _: &ZoomActual, _: &mut Window, cx: &mut Context<Self>) {
        self.zoom = 1.;
        self.pan = point(INITIAL_PAN, INITIAL_PAN);
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

        match self.hit(event.position) {
            Some(node) => {
                self.selected = Some(node);
                self.drag = Some(Drag {
                    node,
                    from: self.to_logical(event.position),
                    origin: (self.rects[node].x, self.rects[node].y),
                });
            }
            None => {
                self.selected = None;
                self.panning = Some(PanDrag {
                    from: event.position,
                    origin: self.pan,
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
            let (x, y) = self.to_logical(event.position);
            let Some(rect) = self.rects.get_mut(drag.node) else {
                return;
            };
            rect.x = drag.origin.0 + (x - drag.from.0);
            rect.y = drag.origin.1 + (y - drag.from.1);
            cx.notify();
            return;
        }

        if let Some(pan) = self.panning {
            self.pan = point(
                pan.origin.x + f32::from(event.position.x - pan.from.x),
                pan.origin.y + f32::from(event.position.y - pan.from.y),
            );
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
        if rect.x != drag.origin.0 || rect.y != drag.origin.1 {
            cx.emit(ErdEvent::LayoutChanged);
        }
    }

    fn on_scroll_wheel(
        &mut self,
        event: &ScrollWheelEvent,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let delta = event.delta.pixel_delta(px(ROW_HEIGHT));

        // The secondary modifier — command on macOS, control everywhere else —
        // is what every canvas in this app turns the wheel into a zoom with.
        if event.modifiers.secondary() {
            if delta.y.is_zero() {
                return;
            }
            let factor = 1. + f32::from(delta.y) * WHEEL_ZOOM_RATE;
            self.zoom_to(self.zoom * factor, event.position, cx);
            return;
        }

        // A plain mouse has no sideways wheel, so `Shift` folds the vertical one
        // onto the horizontal axis.
        let (dx, dy) = if delta.x.is_zero() && event.modifiers.shift {
            (f32::from(delta.y), 0.)
        } else {
            (f32::from(delta.x), f32::from(delta.y))
        };
        if dx == 0. && dy == 0. {
            return;
        }
        self.pan = point(self.pan.x + dx, self.pan.y + dy);
        cx.notify();
    }

    /// Everything the canvas needs this frame, detached from `self`.
    ///
    /// The closures a [`canvas`] takes are `'static`, so they cannot borrow the
    /// view. What they get instead is two reference counts — the prepared text
    /// and the relation list, neither of which changes between models — and a
    /// copy of the rects, which are sixteen bytes each and *do* change as a box
    /// is dragged.
    fn scene(&self, palette: &Theme) -> Scene {
        Scene {
            labels: self.labels.clone(),
            relations: self.relations.clone(),
            rects: self.rects.clone(),
            pan: self.pan,
            zoom: self.zoom,
            selected: self.selected,
            palette: palette.clone(),
        }
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

        let measure = canvas(
            move |bounds, _window, cx| {
                view.update(cx, |view, cx| view.measured(bounds, cx));
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
            .key_context(KEY_CONTEXT)
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

/// Registers the key bindings every [`ErdView`] relies on.
///
/// Scoped to the `ErdView` key context, so the zoom chords keep meaning what
/// they mean everywhere else in the app.
pub fn init(cx: &mut App) {
    let modifier = if cfg!(target_os = "macos") {
        "cmd"
    } else {
        "ctrl"
    };

    cx.bind_keys([
        KeyBinding::new(&format!("{modifier}-="), ZoomIn, Some(KEY_CONTEXT)),
        KeyBinding::new(&format!("{modifier}-+"), ZoomIn, Some(KEY_CONTEXT)),
        KeyBinding::new(&format!("{modifier}--"), ZoomOut, Some(KEY_CONTEXT)),
        KeyBinding::new(&format!("{modifier}-0"), ZoomActual, Some(KEY_CONTEXT)),
        KeyBinding::new(
            &format!("{modifier}-shift-a"),
            AutoArrange,
            Some(KEY_CONTEXT),
        ),
    ]);
}

/// Every box's text, cut to the width the layout gave it.
fn labels_of(model: &ErdModel, rects: &[NodeRect]) -> Vec<BoxLabels> {
    model
        .tables
        .iter()
        .enumerate()
        .map(|(index, table)| {
            let room = rects
                .get(index)
                .map_or(0., |rect| rect.w - 2. * BOX_PADDING);
            BoxLabels {
                title: SharedString::from(elide(&table.name, room)),
                rows: table
                    .columns
                    .iter()
                    .map(|column| {
                        let (name, type_name) = split_row(&column.name, &column.type_name, room);
                        RowLabel {
                            name: SharedString::from(name),
                            type_name: SharedString::from(type_name),
                            kind: match (column.primary_key, column.foreign_key) {
                                (true, _) => RowKind::PrimaryKey,
                                (false, true) => RowKind::ForeignKey,
                                (false, false) => RowKind::Plain,
                            },
                        }
                    })
                    .collect(),
            }
        })
        .collect()
}

/// One frame's worth of diagram, ready to be laid out against a viewport.
struct Scene {
    labels: Rc<Vec<BoxLabels>>,
    relations: Rc<Vec<(usize, usize)>>,
    rects: Vec<NodeRect>,
    pan: Point<f32>,
    zoom: f32,
    selected: Option<usize>,
    palette: Theme,
}

/// One frame's worth of diagram, placed and shaped, ready to be painted.
///
/// Built during prepaint and drained during paint, in the order the fields are
/// declared: lines are behind boxes, and boxes are behind their own labels.
struct Painted {
    lines: Vec<(Vec<Point<Pixels>>, Hsla)>,
    quads: Vec<PaintQuad>,
    labels: Vec<(ShapedLine, Point<Pixels>, Pixels)>,
}

impl Scene {
    /// Works out where everything is and shapes the text that will be legible.
    ///
    /// **The virtualisation.** Only boxes that intersect `bounds` are placed at
    /// all, and only their labels are shaped; a diagram of two hundred tables
    /// zoomed in on one of them costs what one costs. Below [`TEXT_FLOOR`] no
    /// text is shaped at all, because none of it could be read.
    fn prepaint(self, bounds: Bounds<Pixels>, window: &mut Window) -> Painted {
        let zoom = self.zoom;
        let at = |x: f32, y: f32| {
            point(
                bounds.origin.x + px(x * zoom + self.pan.x),
                bounds.origin.y + px(y * zoom + self.pan.y),
            )
        };
        let visible = |rect: &NodeRect| {
            let top_left = at(rect.x, rect.y);
            let bottom_right = at(rect.right(), rect.bottom());
            bottom_right.x >= bounds.origin.x
                && top_left.x <= bounds.origin.x + bounds.size.width
                && bottom_right.y >= bounds.origin.y
                && top_left.y <= bounds.origin.y + bounds.size.height
        };

        let mut painted = Painted {
            lines: Vec::new(),
            quads: Vec::new(),
            labels: Vec::new(),
        };

        for &(from, to) in self.relations.iter() {
            let (Some(from), Some(to)) = (self.rects.get(from), self.rects.get(to)) else {
                continue;
            };
            if !visible(from) && !visible(to) {
                continue;
            }
            let points = route(from, to);
            if points.len() < 2 {
                continue;
            }

            let colour = self.palette.text_muted;
            painted
                .lines
                .push((points.iter().map(|(x, y)| at(*x, *y)).collect(), colour));
            for prong in crow_foot(points[0], head_direction(&points)) {
                painted.lines.push((
                    vec![at(prong[0].0, prong[0].1), at(prong[1].0, prong[1].1)],
                    colour,
                ));
            }
            let bar = key_bar(points[points.len() - 1], tail_direction(&points));
            painted
                .lines
                .push((vec![at(bar[0].0, bar[0].1), at(bar[1].0, bar[1].1)], colour));
        }

        let style = window.text_style();
        let font = style.font();
        let shape_text = zoom >= TEXT_FLOOR;

        for (index, rect) in self.rects.iter().enumerate() {
            if !visible(rect) {
                continue;
            }

            let origin = at(rect.x, rect.y);
            let body = Bounds::new(origin, size(px(rect.w * zoom), px(rect.h * zoom)));
            painted.quads.push(fill(body, self.palette.surface));
            painted.quads.push(fill(
                Bounds::new(
                    origin,
                    size(px(rect.w * zoom), px(HEADER_HEIGHT.min(rect.h) * zoom)),
                ),
                self.palette.grid_header,
            ));

            let border = if self.selected == Some(index) {
                self.palette.accent
            } else {
                self.palette.border
            };
            painted
                .quads
                .push(outline(body, border, gpui::BorderStyle::Solid));

            let Some(labels) = self.labels.get(index) else {
                continue;
            };
            if !shape_text {
                continue;
            }

            let left = origin.x + px(BOX_PADDING * zoom);
            let right = origin.x + px((rect.w - BOX_PADDING) * zoom);
            let title_height = px(HEADER_HEIGHT * zoom);
            let row_height = px(ROW_HEIGHT * zoom);

            let title = window.text_system().shape_line(
                labels.title.clone(),
                px(TITLE_SIZE * zoom),
                &[run(&labels.title, self.palette.text, &font)],
                None,
            );
            painted
                .labels
                .push((title, point(left, origin.y), title_height));

            for (row, label) in labels.rows.iter().enumerate() {
                let top = origin.y + px((HEADER_HEIGHT + row as f32 * ROW_HEIGHT) * zoom);
                if top > bounds.origin.y + bounds.size.height || top + row_height < bounds.origin.y
                {
                    continue;
                }

                let colour = match label.kind {
                    RowKind::PrimaryKey => self.palette.grid_pk,
                    RowKind::ForeignKey => self.palette.accent,
                    RowKind::Plain => self.palette.text,
                };
                let name = window.text_system().shape_line(
                    label.name.clone(),
                    px(FONT_SIZE * zoom),
                    &[run(&label.name, colour, &font)],
                    None,
                );
                painted.labels.push((name, point(left, top), row_height));

                if label.type_name.is_empty() {
                    continue;
                }
                let type_name = window.text_system().shape_line(
                    label.type_name.clone(),
                    px(FONT_SIZE * zoom),
                    &[run(&label.type_name, self.palette.text_muted, &font)],
                    None,
                );
                let x = right - type_name.width;
                painted.labels.push((type_name, point(x, top), row_height));
            }
        }

        painted
    }
}

impl Painted {
    /// Draws the frame, clipped to the viewport.
    fn paint(mut self, bounds: Bounds<Pixels>, window: &mut Window, cx: &mut App) {
        window.with_content_mask(Some(ContentMask { bounds }), |window| {
            for (points, colour) in self.lines.drain(..) {
                let mut builder = PathBuilder::stroke(px(1.));
                for (index, at) in points.into_iter().enumerate() {
                    if index == 0 {
                        builder.move_to(at);
                    } else {
                        builder.line_to(at);
                    }
                }
                if let Ok(path) = builder.build() {
                    window.paint_path(path, colour);
                }
            }

            for quad in self.quads.drain(..) {
                window.paint_quad(quad);
            }

            for (line, origin, line_height) in self.labels.drain(..) {
                line.paint(origin, line_height, window, cx).ok();
            }
        });
    }
}

/// One run covering the whole of `text`, in one colour and the window's font.
fn run(text: &str, colour: Hsla, font: &gpui::Font) -> TextRun {
    TextRun {
        len: text.len(),
        font: font.clone(),
        color: colour,
        background_color: None,
        underline: None,
        strikethrough: None,
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::ops::Deref;

    use gpui::{
        Entity, Modifiers, MouseDownEvent, MouseMoveEvent, MouseUpEvent, ScrollDelta,
        TestAppContext, TouchPhase, VisualTestContext,
    };

    use crate::model::{ErdColumn, ErdRelation, ErdTable};

    use super::*;

    /// A view that does nothing but hold the diagram, as the host's pane would.
    struct Harness {
        erd: Entity<ErdView>,
    }

    impl Render for Harness {
        fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
            div().size_full().child(self.erd.clone())
        }
    }

    /// Everything a test reads back.
    struct Handles {
        erd: Entity<ErdView>,
        events: Rc<RefCell<Vec<ErdEvent>>>,
    }

    impl Handles {
        fn drain(&self) -> Vec<ErdEvent> {
            self.events.borrow_mut().drain(..).collect()
        }

        fn read<R>(&self, cx: &mut VisualTestContext, f: impl FnOnce(&ErdView) -> R) -> R {
            cx.update(|_, cx| f(self.erd.read(cx)))
        }

        fn update(
            &self,
            cx: &mut VisualTestContext,
            f: impl FnOnce(&mut ErdView, &mut Context<ErdView>),
        ) {
            cx.update(|_, cx| self.erd.update(cx, f));
            cx.run_until_parked();
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
        cx.update(rudbman_ui::init);
        cx.update(crate::init);

        let events: Rc<RefCell<Vec<ErdEvent>>> = Rc::new(RefCell::new(Vec::new()));
        let window = cx.add_window({
            let events = events.clone();
            move |_, cx| {
                let erd = cx.new(ErdView::new);
                cx.subscribe(&erd, move |_: &mut Harness, _, event: &ErdEvent, _| {
                    events.borrow_mut().push(*event);
                })
                .detach();
                Harness { erd }
            }
        });
        let erd = window
            .update(cx, |harness, _, _| harness.erd.clone())
            .expect("the window is open");

        let mut cx = VisualTestContext::from_window(*window.deref(), cx);
        cx.update(|window, cx| {
            let handle = erd.read(cx).focus_handle(cx);
            handle.focus(window);
        });
        cx.run_until_parked();

        let handles = Handles { erd, events };
        handles.update(&mut cx, |erd, cx| erd.set_model(model, saved, cx));
        (handles, cx)
    }

    /// The window point a diagram point is drawn at, with the view unpanned and
    /// unzoomed.
    fn window_point(x: f32, y: f32) -> Point<Pixels> {
        point(px(x + INITIAL_PAN), px(y + INITIAL_PAN))
    }

    fn press(cx: &mut VisualTestContext, at: Point<Pixels>) {
        cx.simulate_event(MouseDownEvent {
            position: at,
            modifiers: Modifiers::none(),
            button: MouseButton::Left,
            click_count: 1,
            first_mouse: false,
        });
        cx.run_until_parked();
    }

    fn drag_to(cx: &mut VisualTestContext, at: Point<Pixels>) {
        cx.simulate_event(MouseMoveEvent {
            position: at,
            pressed_button: Some(MouseButton::Left),
            modifiers: Modifiers::none(),
        });
        cx.run_until_parked();
    }

    fn release(cx: &mut VisualTestContext, at: Point<Pixels>) {
        cx.simulate_event(MouseUpEvent {
            position: at,
            modifiers: Modifiers::none(),
            button: MouseButton::Left,
            click_count: 1,
        });
        cx.run_until_parked();
    }

    fn wheel(cx: &mut VisualTestContext, at: Point<Pixels>, dy: f32, modifiers: Modifiers) {
        cx.simulate_event(ScrollWheelEvent {
            position: at,
            delta: ScrollDelta::Pixels(point(px(0.), px(dy))),
            modifiers,
            touch_phase: TouchPhase::Moved,
        });
        cx.run_until_parked();
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
