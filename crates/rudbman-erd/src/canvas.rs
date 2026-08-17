//! The canvas both diagrams stand on: a viewport, the gestures that move it,
//! and the frame assembly they are drawn by.
//!
//! Two widgets sit on this module — [`crate::view::ErdView`], which draws a
//! schema, and [`crate::builder::BuilderView`], which draws a query. They differ
//! in exactly one thing, *what a press on a box means*; everything else — the
//! transform, the pan and the zoom, the box hit test, the virtualisation, the
//! elided labels, the paint order — is one implementation here rather than two
//! that drift apart (architecture document, §7.7).
//!
//! ## Not a pure module, and not a widget either
//!
//! This is the only module in the crate that knows gpui but is not a widget: it
//! has no entity, no focus handle and no event. It takes a [`Viewport`] and a
//! list of [`NodeRect`]s and answers questions about them. The arithmetic that
//! needs no window at all — where a row sits, which row a `y` is in, where a
//! line attaches — stays in [`crate::layout`] with the rest of the pure
//! geometry, because [`crate::svg`] draws the same rows and must not have to
//! reach through a module that imports gpui to do it.
//!
//! ## Two coordinate systems and one conversion
//!
//! Logical units are what a layout is measured in; screen pixels are what a
//! frame is painted in, and `screen = bounds.origin + logical * zoom + pan`.
//! Both directions of that conversion live on [`Viewport`] —
//! [`Viewport::to_screen`] and [`Viewport::to_logical`] — so a picture and a hit
//! test cannot disagree about where a box is.
//!
//! The one asymmetry is deliberate: [`Viewport::to_logical`] answers against the
//! bounds the last frame *measured*, because that is all a mouse handler has,
//! while [`Viewport::to_screen`] is told the bounds the frame is being painted
//! into. In the frame that follows a resize the two differ by less than the half
//! pixel [`Viewport::measured`] ignores.

use std::collections::HashSet;
use std::rc::Rc;

use gpui::{
    App, Bounds, ContentMask, Hsla, IsZero, KeyBinding, PaintQuad, PathBuilder, Pixels, Point,
    ScrollWheelEvent, ShapedLine, SharedString, TextRun, Window, actions, fill, outline, point, px,
    size,
};
use rudbman_ui::scrollbar::ScrollbarAxis;
use rudbman_ui::theme::Theme;

use crate::layout::{
    BOX_PADDING, HEADER_HEIGHT, NodeRect, ROW_HEIGHT, crow_foot, elide, head_direction, key_bar,
    route_between, row_anchor, row_at, row_offset, split_row, tail_direction,
};
use crate::model::{ErdTable, NameMode};

actions!(
    rudbman_erd,
    [
        /// Zoom the diagram in one step, about the middle of the viewport.
        ZoomIn,
        /// Zoom the diagram out one step, about the middle of the viewport.
        ZoomOut,
        /// Return to 1:1 and to the top-left corner of the diagram.
        ZoomActual,
    ]
);

/// Key context the zoom chords are bound to.
///
/// Named on the root of *both* widgets, alongside the context of the widget
/// itself, so that a chord which means "zoom" on a canvas is registered once
/// rather than once per canvas. gpui reads a key context as a set of
/// identifiers, so `key_context("ErdCanvas ErdView")` answers to both.
pub const CANVAS_KEY_CONTEXT: &str = "ErdCanvas";

/// Closest a canvas may be zoomed.
pub(crate) const MAX_ZOOM: f32 = 2.;

/// Furthest a canvas may be zoomed out.
///
/// Past a quarter the labels are unreadable anyway, and the point of zooming
/// out is to see the shape of the schema rather than to read it.
pub(crate) const MIN_ZOOM: f32 = 0.25;

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

/// Where a canvas sits before anything has been panned.
pub(crate) const INITIAL_PAN: f32 = 24.;

/// How much room a scrollbar counts in front of the content and behind it, in
/// screen pixels.
///
/// [`INITIAL_PAN`] on purpose, and not by coincidence: the home position is the
/// top-left corner of the content with exactly that much room in front of it,
/// so a canvas at home is one scrolled to precisely zero. Any other margin
/// would leave a freshly opened diagram's thumb a little way down its track.
const MARGIN: f32 = INITIAL_PAN;

/// Text size of a column row, in logical units.
const FONT_SIZE: f32 = 12.;

/// Text size of a box's title, in logical units.
const TITLE_SIZE: f32 = 13.;

/// Registers the key bindings every canvas relies on.
///
/// Scoped to [`CANVAS_KEY_CONTEXT`], so the zoom chords keep meaning what they
/// mean everywhere else in the app and both widgets get them from one place.
pub(crate) fn init(cx: &mut App) {
    let modifier = if cfg!(target_os = "macos") {
        "cmd"
    } else {
        "ctrl"
    };

    cx.bind_keys([
        KeyBinding::new(&format!("{modifier}-="), ZoomIn, Some(CANVAS_KEY_CONTEXT)),
        KeyBinding::new(&format!("{modifier}-+"), ZoomIn, Some(CANVAS_KEY_CONTEXT)),
        KeyBinding::new(&format!("{modifier}--"), ZoomOut, Some(CANVAS_KEY_CONTEXT)),
        KeyBinding::new(
            &format!("{modifier}-0"),
            ZoomActual,
            Some(CANVAS_KEY_CONTEXT),
        ),
    ]);
}

/// What an overlay scrollbar over a canvas is drawn from, along one axis.
///
/// The same three numbers [`rudbman_ui::scrollbar::thumb`] takes, in screen
/// pixels. A canvas has no scroll container to read them off — it has a pan, a
/// zoom and a list of boxes — so they are worked out from those instead, and
/// the bar is then wired exactly as every other surface's is.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct BarExtent {
    /// How much of the axis the viewport shows.
    pub(crate) visible: f32,
    /// How much of the content, its margins included, lies outside the
    /// viewport.
    ///
    /// Zero or less when the whole diagram fits, which is exactly what a bar
    /// reads as "there is nothing to draw" — so a canvas with room to spare
    /// needs no branch of its own.
    pub(crate) scrollable: f32,
    /// How far past the start of the content the viewport is looking.
    ///
    /// Runs negative, or past `scrollable`, while the canvas is panned off the
    /// end of its own content, which a canvas may be: the thumb pins itself to
    /// the nearer end and says nothing about it.
    pub(crate) scrolled: f32,
}

/// The two edges a canvas hangs an overlay bar on.
///
/// Both widgets wire either bar the same way — notice the canvas moved, arm the
/// expiry, read a drag, let go — so the axes are walked rather than written out
/// twice per widget.
pub(crate) const BARS: [ScrollbarAxis; 2] = [ScrollbarAxis::Vertical, ScrollbarAxis::Horizontal];

/// Where the boxes start and where they end along `axis`, in logical units.
///
/// `None` for a canvas with no boxes on it, which has no content and therefore
/// no extent — rather than the empty range at the origin, which would claim
/// the diagram was somewhere.
fn extent(rects: &[NodeRect], axis: ScrollbarAxis) -> Option<(f32, f32)> {
    let mut spans = rects.iter().map(|rect| match axis {
        ScrollbarAxis::Horizontal => (rect.x, rect.right()),
        ScrollbarAxis::Vertical => (rect.y, rect.bottom()),
    });
    let first = spans.next()?;
    Some(spans.fold(first, |(start, end), (x, y)| (start.min(x), end.max(y))))
}

/// Where a canvas is looking, and how closely.
///
/// Three numbers and no element: a gpui scroll container lays its content out
/// in full and then clips it, which is the cost this crate exists to avoid, and
/// it cannot zoom at all.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Viewport {
    /// Translation from logical units to the viewport, in screen pixels.
    pub(crate) pan: Point<f32>,
    /// Scale from logical units to screen pixels.
    pub(crate) zoom: f32,
    /// Where the canvas was, as of the last frame that measured it.
    pub(crate) bounds: Bounds<Pixels>,
}

impl Default for Viewport {
    fn default() -> Self {
        Self {
            pan: point(INITIAL_PAN, INITIAL_PAN),
            zoom: 1.,
            bounds: Bounds::default(),
        }
    }
}

impl Viewport {
    /// Where `at` falls in the diagram's own coordinates.
    ///
    /// By value rather than by reference because a viewport is three numbers
    /// and copying them is cheaper than borrowing them.
    pub(crate) fn to_logical(self, at: Point<Pixels>) -> (f32, f32) {
        let x = f32::from(at.x - self.bounds.origin.x) - self.pan.x;
        let y = f32::from(at.y - self.bounds.origin.y) - self.pan.y;
        (x / self.zoom, y / self.zoom)
    }

    /// Where the logical point `(x, y)` is drawn, in a frame painting into
    /// `bounds`.
    pub(crate) fn to_screen(self, bounds: &Bounds<Pixels>, x: f32, y: f32) -> Point<Pixels> {
        point(
            bounds.origin.x + px(x * self.zoom + self.pan.x),
            bounds.origin.y + px(y * self.zoom + self.pan.y),
        )
    }

    /// Whether any part of `rect` falls inside `bounds`.
    ///
    /// The whole of the virtualisation: a diagram of two hundred tables zoomed
    /// in on one of them costs what one costs.
    pub(crate) fn visible(&self, bounds: &Bounds<Pixels>, rect: &NodeRect) -> bool {
        let top_left = self.to_screen(bounds, rect.x, rect.y);
        let bottom_right = self.to_screen(bounds, rect.right(), rect.bottom());
        bottom_right.x >= bounds.origin.x
            && top_left.x <= bounds.origin.x + bounds.size.width
            && bottom_right.y >= bounds.origin.y
            && top_left.y <= bounds.origin.y + bounds.size.height
    }

    /// Notes where the canvas turned out to be, and says whether it moved.
    ///
    /// Called from the measuring canvas during prepaint, and only reports a
    /// move when the box actually moved by half a pixel — a repaint per frame
    /// would be a repaint forever.
    pub(crate) fn measured(&mut self, bounds: Bounds<Pixels>) -> bool {
        let moved = (f32::from(bounds.origin.x - self.bounds.origin.x)).abs() >= 0.5
            || (f32::from(bounds.origin.y - self.bounds.origin.y)).abs() >= 0.5
            || (f32::from(bounds.size.width - self.bounds.size.width)).abs() >= 0.5
            || (f32::from(bounds.size.height - self.bounds.size.height)).abs() >= 0.5;
        if !moved {
            return false;
        }
        self.bounds = bounds;
        true
    }

    /// Scales to `wanted`, keeping whatever is under `anchor` under it.
    ///
    /// Answers whether anything changed, so that a caller can decide whether to
    /// ask for another frame.
    pub(crate) fn zoom_to(&mut self, wanted: f32, anchor: Point<Pixels>) -> bool {
        let zoom = wanted.clamp(MIN_ZOOM, MAX_ZOOM);
        if (zoom - self.zoom).abs() < 1e-4 {
            return false;
        }

        // The point under the pointer before the scale changes is the point the
        // pan is then chosen to put back under it.
        let (x, y) = self.to_logical(anchor);
        let local_x = f32::from(anchor.x - self.bounds.origin.x);
        let local_y = f32::from(anchor.y - self.bounds.origin.y);
        self.zoom = zoom;
        self.pan = point(local_x - x * zoom, local_y - y * zoom);
        true
    }

    /// The middle of the viewport, which is what a keyboard zoom is about.
    pub(crate) fn centre(&self) -> Point<Pixels> {
        point(
            self.bounds.origin.x + self.bounds.size.width / 2.,
            self.bounds.origin.y + self.bounds.size.height / 2.,
        )
    }

    /// One step closer, about the middle of the viewport.
    pub(crate) fn zoom_in(&mut self) -> bool {
        self.zoom_to(self.zoom * ZOOM_STEP, self.centre())
    }

    /// One step further out, about the middle of the viewport.
    pub(crate) fn zoom_out(&mut self) -> bool {
        self.zoom_to(self.zoom / ZOOM_STEP, self.centre())
    }

    /// Back to the corner the diagram starts in, at whatever the scale is.
    pub(crate) fn home(&mut self) {
        self.pan = point(INITIAL_PAN, INITIAL_PAN);
    }

    /// Back to 1:1 and to the top-left corner of the diagram.
    pub(crate) fn reset(&mut self) {
        self.zoom = 1.;
        self.home();
    }

    /// Pans or zooms with the wheel, and says whether anything changed.
    pub(crate) fn scroll(&mut self, event: &ScrollWheelEvent) -> bool {
        let delta = event.delta.pixel_delta(px(ROW_HEIGHT));

        // The secondary modifier — command on macOS, control everywhere else —
        // is what every canvas in this app turns the wheel into a zoom with.
        if event.modifiers.secondary() {
            if delta.y.is_zero() {
                return false;
            }
            let factor = 1. + f32::from(delta.y) * WHEEL_ZOOM_RATE;
            return self.zoom_to(self.zoom * factor, event.position);
        }

        // A plain mouse has no sideways wheel, so `Shift` folds the vertical one
        // onto the horizontal axis.
        let (dx, dy) = if delta.x.is_zero() && event.modifiers.shift {
            (f32::from(delta.y), 0.)
        } else {
            (f32::from(delta.x), f32::from(delta.y))
        };
        if dx == 0. && dy == 0. {
            return false;
        }
        self.pan = point(self.pan.x + dx, self.pan.y + dy);
        true
    }

    /// The component of the pan that runs along `axis`.
    pub(crate) fn pan_along(&self, axis: ScrollbarAxis) -> f32 {
        match axis {
            ScrollbarAxis::Horizontal => self.pan.x,
            ScrollbarAxis::Vertical => self.pan.y,
        }
    }

    /// Moves the pan along `axis`, leaving the other axis where it was.
    pub(crate) fn set_pan_along(&mut self, axis: ScrollbarAxis, pan: f32) {
        match axis {
            ScrollbarAxis::Horizontal => self.pan.x = pan,
            ScrollbarAxis::Vertical => self.pan.y = pan,
        }
    }

    /// What a bar riding `axis` is drawn from this frame.
    ///
    /// Everything is in screen pixels, so the content's logical extent is
    /// scaled by the zoom: zooming in makes a diagram longer to scroll through
    /// without moving a single box, and the thumb has to shrink to say so.
    ///
    /// `None` for a canvas with nothing on it. Everything else — a diagram
    /// smaller than the window, a viewport panned off the end of one — comes
    /// back as numbers the bar itself knows what to do with.
    pub(crate) fn bar_extent(&self, rects: &[NodeRect], axis: ScrollbarAxis) -> Option<BarExtent> {
        let (start, end) = extent(rects, axis)?;
        let visible = f32::from(match axis {
            ScrollbarAxis::Horizontal => self.bounds.size.width,
            ScrollbarAxis::Vertical => self.bounds.size.height,
        });

        Some(BarExtent {
            visible,
            scrollable: (end - start) * self.zoom + 2. * MARGIN - visible,
            scrolled: MARGIN - start * self.zoom - self.pan_along(axis),
        })
    }

    /// The pan along `axis` that a thumb dragged `progress` of the way along
    /// its track asks for.
    ///
    /// The inverse of [`BarExtent::scrolled`], written as a correction to the
    /// pan rather than from scratch so that the two cannot drift apart: the
    /// distance the bar wants to move is the difference between where it is and
    /// where the pointer put it.
    pub(crate) fn panned_to(
        &self,
        rects: &[NodeRect],
        axis: ScrollbarAxis,
        progress: f32,
    ) -> Option<f32> {
        let bar = self.bar_extent(rects, axis)?;
        Some(self.pan_along(axis) + bar.scrolled - progress * bar.scrollable)
    }

    /// Which box is under `at`, if any.
    pub(crate) fn hit(&self, rects: &[NodeRect], at: Point<Pixels>) -> Option<usize> {
        if !self.bounds.contains(&at) {
            return None;
        }
        let (x, y) = self.to_logical(at);
        hit_box(rects, x, y)
    }

    /// Which box holds the logical point `at`, and which of its rows.
    ///
    /// `None` for the title band and for the background, which is what makes a
    /// press on a header a move and a press on a row a column gesture.
    pub(crate) fn hit_row(
        &self,
        rects: &[NodeRect],
        at: Point<Pixels>,
    ) -> Option<(usize, Option<usize>)> {
        let node = self.hit(rects, at)?;
        let (_, y) = self.to_logical(at);
        Some((node, row_at(&rects[node], y)))
    }
}

/// Which box is under the logical point `(x, y)`, if any.
///
/// Reverse order, because the boxes are drawn in table order and the one drawn
/// last is the one the user sees on top — the judgement the result grid reached
/// about cells, for the same reason.
pub(crate) fn hit_box(rects: &[NodeRect], x: f32, y: f32) -> Option<usize> {
    rects
        .iter()
        .enumerate()
        .rev()
        .find(|(_, rect)| rect.contains(x, y))
        .map(|(index, _)| index)
}

/// A box being dragged.
///
/// A drag is computed **absolutely**, from the pointer position and the box
/// position that were recorded when it began, rather than by accumulating
/// per-event deltas: an accumulated drag drifts by a fraction of a pixel per
/// event, and over a long drag the box ends up somewhere the pointer is not.
/// The grid's column resize is written the same way.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Drag {
    /// Which box.
    pub(crate) node: usize,
    /// Where the pointer was, in logical units, when it took hold.
    pub(crate) from: (f32, f32),
    /// Where the box was then, so that the drag is absolute rather than a
    /// running total that could drift.
    pub(crate) origin: (f32, f32),
}

impl Drag {
    /// Where the box belongs with the pointer at the logical point `(x, y)`.
    pub(crate) fn moved_to(&self, x: f32, y: f32) -> (f32, f32) {
        (
            self.origin.0 + (x - self.from.0),
            self.origin.1 + (y - self.from.1),
        )
    }

    /// Whether the box ended up somewhere other than where it started.
    pub(crate) fn moved(&self, rect: &NodeRect) -> bool {
        rect.x != self.origin.0 || rect.y != self.origin.1
    }
}

/// The background being dragged.
#[derive(Clone, Copy, Debug)]
pub(crate) struct PanDrag {
    /// Where the pointer was, in window coordinates, when it took hold.
    pub(crate) from: Point<Pixels>,
    /// The pan offset then.
    pub(crate) origin: Point<f32>,
}

impl PanDrag {
    /// The pan offset that belongs with the pointer at `at`.
    pub(crate) fn pan_at(&self, at: Point<Pixels>) -> Point<f32> {
        point(
            self.origin.x + f32::from(at.x - self.from.x),
            self.origin.y + f32::from(at.y - self.from.y),
        )
    }
}

/// Which palette slot a column's name is drawn in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RowKind {
    /// An ordinary column.
    Plain,
    /// A column of the primary key.
    PrimaryKey,
    /// A column of a foreign key that is not also part of the primary key.
    ForeignKey,
}

/// One column row, already cut to fit the box it is drawn in.
pub(crate) struct RowLabel {
    /// The column's name, elided to its share of the row.
    pub(crate) name: SharedString,
    /// The type as it should read, elided to what the name left.
    pub(crate) type_name: SharedString,
    /// Which colour the name is drawn in.
    pub(crate) kind: RowKind,
}

/// One box's text, prepared once when the tables arrive.
///
/// Elision depends only on a box's *width* and on which name is being drawn,
/// and neither changes while the diagram is being used — dragging moves boxes,
/// it does not resize them — so the cutting is done here and not once per
/// frame. Switching [`NameMode`] changes both at once, which is why it goes
/// back through the measure and through this.
pub(crate) struct BoxLabels {
    /// The table's name, elided to the width of its box.
    pub(crate) title: SharedString,
    /// One entry per column, in catalog order.
    pub(crate) rows: Vec<RowLabel>,
}

/// Every box's text, cut to the width the layout gave it.
///
/// Takes the tables and the rects rather than a model, because the query
/// builder has a table list and no relations to go with it.
pub(crate) fn labels_of(tables: &[ErdTable], rects: &[NodeRect], mode: NameMode) -> Vec<BoxLabels> {
    tables
        .iter()
        .enumerate()
        .map(|(index, table)| {
            let room = rects
                .get(index)
                .map_or(0., |rect| rect.w - 2. * BOX_PADDING);
            BoxLabels {
                title: SharedString::from(elide(table.display_name(mode), room)),
                rows: table
                    .columns
                    .iter()
                    .map(|column| {
                        let (name, type_name) =
                            split_row(column.display_name(mode), &column.type_name, room);
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

/// One line between two boxes, at box centres or at column rows.
///
/// A foreign key has no row to attach to — the relation belongs to the tables,
/// not to one row of them — so [`None`] means the box's centre and is what the
/// ERD passes. A join drawn in the query builder is a statement about two
/// *columns*, so it carries both rows and is drawn at their heights.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Edge {
    /// Index of the box the line leaves.
    pub(crate) from: usize,
    /// Index of the box the line arrives at.
    pub(crate) to: usize,
    /// Which row of `from` the line leaves, or the box's centre.
    pub(crate) from_row: Option<usize>,
    /// Which row of `to` the line arrives at, or the box's centre.
    pub(crate) to_row: Option<usize>,
}

impl Edge {
    /// A line between two boxes' centres, as a foreign key is drawn.
    pub(crate) fn between(from: usize, to: usize) -> Self {
        Self {
            from,
            to,
            from_row: None,
            to_row: None,
        }
    }

    /// A line between two column rows, as a join is drawn.
    pub(crate) fn between_rows(from: (usize, usize), to: (usize, usize)) -> Self {
        Self {
            from: from.0,
            to: to.0,
            from_row: Some(from.1),
            to_row: Some(to.1),
        }
    }
}

/// One frame's worth of canvas, detached from the view that asked for it.
///
/// The closures a gpui canvas takes are `'static`, so they cannot borrow a
/// view. What they get instead is two reference counts — the prepared text and
/// the edge list, neither of which changes between models — and a copy of the
/// rects, which are sixteen bytes each and *do* change as a box is dragged.
pub(crate) struct Scene {
    /// The prepared text, one entry per box.
    pub(crate) labels: Rc<Vec<BoxLabels>>,
    /// The lines worth drawing, resolved to indices once.
    pub(crate) edges: Rc<Vec<Edge>>,
    /// Where every box is, in logical units.
    pub(crate) rects: Vec<NodeRect>,
    /// Where the canvas is looking.
    pub(crate) viewport: Viewport,
    /// Which box is outlined in the accent colour, if any.
    pub(crate) selected: Option<usize>,
    /// Which `(box, row)` pairs are highlighted.
    pub(crate) selected_rows: Rc<HashSet<(usize, usize)>>,
    /// Whether the lines carry cardinality marks.
    ///
    /// A foreign key has a "many" end and a "one" end and says so; a join's
    /// type is `INNER` or `LEFT` and is edited in a panel, not read off a
    /// glyph, so the builder draws its lines bare.
    pub(crate) marks: bool,
    /// A line being dragged, from a point on a box to the pointer.
    pub(crate) rubber: Option<((f32, f32), (f32, f32))>,
    /// The colours this frame is drawn in.
    pub(crate) palette: Theme,
}

impl Scene {
    /// A frame of boxes and lines, with nothing selected and nothing in flight.
    pub(crate) fn new(
        labels: Rc<Vec<BoxLabels>>,
        edges: Rc<Vec<Edge>>,
        rects: Vec<NodeRect>,
        viewport: Viewport,
        selected: Option<usize>,
        palette: Theme,
    ) -> Self {
        Self {
            labels,
            edges,
            rects,
            viewport,
            selected,
            selected_rows: Rc::new(HashSet::new()),
            marks: true,
            rubber: None,
            palette,
        }
    }

    /// The same frame with `rows` highlighted.
    pub(crate) fn rows(mut self, rows: Rc<HashSet<(usize, usize)>>) -> Self {
        self.selected_rows = rows;
        self
    }

    /// The same frame with its lines drawn bare, without cardinality marks.
    pub(crate) fn bare(mut self) -> Self {
        self.marks = false;
        self
    }

    /// The same frame with a line dragged from `rubber.0` to `rubber.1`.
    pub(crate) fn rubber(mut self, rubber: Option<((f32, f32), (f32, f32))>) -> Self {
        self.rubber = rubber;
        self
    }

    /// Works out where everything is and shapes the text that will be legible.
    ///
    /// **The virtualisation.** Only boxes that intersect `bounds` are placed at
    /// all, and only their labels are shaped. Below [`TEXT_FLOOR`] no text is
    /// shaped at all, because none of it could be read.
    pub(crate) fn prepaint(self, bounds: Bounds<Pixels>, window: &mut Window) -> Painted {
        let zoom = self.viewport.zoom;
        let at = |x: f32, y: f32| self.viewport.to_screen(&bounds, x, y);
        let visible = |rect: &NodeRect| self.viewport.visible(&bounds, rect);

        let mut painted = Painted::default();

        for edge in self.edges.iter() {
            let (Some(from), Some(to)) = (self.rects.get(edge.from), self.rects.get(edge.to))
            else {
                continue;
            };
            if !visible(from) && !visible(to) {
                continue;
            }

            // Which side each line leaves by is the router's judgement, and the
            // row anchors have to be asked the same question so that a line at
            // a row's height still leaves the edge the route runs from.
            let rightwards = to.center_x() >= from.center_x();
            let from_y = edge
                .from_row
                .map_or(from.center_y(), |row| row_anchor(from, row, rightwards).1);
            let to_y = edge
                .to_row
                .map_or(to.center_y(), |row| row_anchor(to, row, !rightwards).1);
            let points = route_between(from, from_y, to, to_y);
            if points.len() < 2 {
                continue;
            }

            let colour = self.palette.text_muted;
            painted
                .under_lines
                .push((points.iter().map(|(x, y)| at(*x, *y)).collect(), colour));
            if !self.marks {
                continue;
            }
            for prong in crow_foot(points[0], head_direction(&points)) {
                painted.under_lines.push((
                    vec![at(prong[0].0, prong[0].1), at(prong[1].0, prong[1].1)],
                    colour,
                ));
            }
            let bar = key_bar(points[points.len() - 1], tail_direction(&points));
            painted
                .under_lines
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
            let labels = self.labels.get(index);
            let rows = labels.map_or(0, |labels| labels.rows.len());
            box_quads(
                &mut painted,
                &self.palette,
                rect,
                origin,
                zoom,
                self.selected == Some(index),
                rows,
                |row| self.selected_rows.contains(&(index, row)),
            );

            let Some(labels) = labels else {
                continue;
            };
            if !shape_text {
                continue;
            }
            shape_box_labels(
                &mut painted,
                window,
                &bounds,
                &self.palette,
                &font,
                rect,
                origin,
                zoom,
                labels,
            );
        }

        if let Some((from, to)) = self.rubber {
            painted.over_lines.push((
                vec![at(from.0, from.1), at(to.0, to.1)],
                self.palette.accent,
            ));
        }

        painted
    }
}

/// One box: its body, its title band, its highlighted rows and its outline.
///
/// In that order, because each is drawn over the last: a highlighted row sits
/// on the body and under the border, and the border is what says the box is
/// selected. A diagram with nothing selected — every ERD — pushes exactly the
/// three quads it always did, because `highlight` is never asked about a row
/// that is not there.
#[allow(clippy::too_many_arguments)]
fn box_quads(
    painted: &mut Painted,
    palette: &Theme,
    rect: &NodeRect,
    origin: Point<Pixels>,
    zoom: f32,
    selected: bool,
    rows: usize,
    highlight: impl Fn(usize) -> bool,
) {
    let body = Bounds::new(origin, size(px(rect.w * zoom), px(rect.h * zoom)));
    painted.quads.push(fill(body, palette.surface));
    painted.quads.push(fill(
        Bounds::new(
            origin,
            size(px(rect.w * zoom), px(HEADER_HEIGHT.min(rect.h) * zoom)),
        ),
        palette.grid_header,
    ));

    for row in 0..rows {
        if !highlight(row) {
            continue;
        }
        painted.quads.push(fill(
            Bounds::new(
                point(origin.x, origin.y + px(row_offset(row) * zoom)),
                size(px(rect.w * zoom), px(ROW_HEIGHT * zoom)),
            ),
            palette.grid_selection,
        ));
    }

    let border = if selected {
        palette.accent
    } else {
        palette.border
    };
    painted
        .quads
        .push(outline(body, border, gpui::BorderStyle::Solid));
}

/// One box's title and column rows, shaped and placed.
///
/// The rows are culled against `bounds` a second time: a box can be taller than
/// the window, and shaping the rows that fall outside it is the cost this
/// module exists to avoid.
#[allow(clippy::too_many_arguments)]
fn shape_box_labels(
    painted: &mut Painted,
    window: &mut Window,
    bounds: &Bounds<Pixels>,
    palette: &Theme,
    font: &gpui::Font,
    rect: &NodeRect,
    origin: Point<Pixels>,
    zoom: f32,
    labels: &BoxLabels,
) {
    let left = origin.x + px(BOX_PADDING * zoom);
    let right = origin.x + px((rect.w - BOX_PADDING) * zoom);
    let title_height = px(HEADER_HEIGHT * zoom);
    let row_height = px(ROW_HEIGHT * zoom);

    let title = window.text_system().shape_line(
        labels.title.clone(),
        px(TITLE_SIZE * zoom),
        &[run(&labels.title, palette.text, font)],
        None,
    );
    painted
        .labels
        .push((title, point(left, origin.y), title_height));

    for (row, label) in labels.rows.iter().enumerate() {
        let top = origin.y + px(row_offset(row) * zoom);
        if top > bounds.origin.y + bounds.size.height || top + row_height < bounds.origin.y {
            continue;
        }

        let colour = match label.kind {
            RowKind::PrimaryKey => palette.grid_pk,
            RowKind::ForeignKey => palette.accent,
            RowKind::Plain => palette.text,
        };
        let name = window.text_system().shape_line(
            label.name.clone(),
            px(FONT_SIZE * zoom),
            &[run(&label.name, colour, font)],
            None,
        );
        painted.labels.push((name, point(left, top), row_height));

        if label.type_name.is_empty() {
            continue;
        }
        let type_name = window.text_system().shape_line(
            label.type_name.clone(),
            px(FONT_SIZE * zoom),
            &[run(&label.type_name, palette.text_muted, font)],
            None,
        );
        let x = right - type_name.width;
        painted.labels.push((type_name, point(x, top), row_height));
    }
}

/// One frame's worth of canvas, placed and shaped, ready to be painted.
///
/// Built during prepaint and drained during paint, in the order the fields are
/// declared. Four layers rather than three: a foreign key belongs *behind* the
/// boxes it joins, because a box is opaque and a line that ends behind one
/// reads better than one that ends on top of it — but a join being dragged
/// belongs in front of them, because it is following the pointer and a line the
/// user cannot see is a line they cannot aim.
#[derive(Default)]
pub(crate) struct Painted {
    /// Lines drawn behind the boxes.
    pub(crate) under_lines: Vec<(Vec<Point<Pixels>>, Hsla)>,
    /// The boxes themselves.
    pub(crate) quads: Vec<PaintQuad>,
    /// Lines drawn in front of the boxes.
    pub(crate) over_lines: Vec<(Vec<Point<Pixels>>, Hsla)>,
    /// The text, drawn last of all.
    pub(crate) labels: Vec<(ShapedLine, Point<Pixels>, Pixels)>,
}

impl Painted {
    /// Draws the frame, clipped to the viewport.
    pub(crate) fn paint(mut self, bounds: Bounds<Pixels>, window: &mut Window, cx: &mut App) {
        window.with_content_mask(Some(ContentMask { bounds }), |window| {
            for (points, colour) in self.under_lines.drain(..) {
                stroke(window, points, colour);
            }

            for quad in self.quads.drain(..) {
                window.paint_quad(quad);
            }

            for (points, colour) in self.over_lines.drain(..) {
                stroke(window, points, colour);
            }

            for (line, origin, line_height) in self.labels.drain(..) {
                line.paint(origin, line_height, window, cx).ok();
            }
        });
    }
}

/// One polyline, a pixel wide.
fn stroke(window: &mut Window, points: Vec<Point<Pixels>>, colour: Hsla) {
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

/// One run covering the whole of `text`, in one colour and the window's font.
pub(crate) fn run(text: &str, colour: Hsla, font: &gpui::Font) -> TextRun {
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
pub(crate) mod test_support {
    //! What a widget test needs before it can press anything.
    //!
    //! Both widgets are opened the same way — a window, a host view that holds
    //! nothing but the widget, a subscription that collects its events, and the
    //! focus — so the opening is written once here and parameterised by the
    //! widget rather than copied per widget.

    use std::cell::RefCell;
    use std::ops::Deref;
    use std::rc::Rc;

    use gpui::{
        AppContext as _, Context, Entity, EventEmitter, Focusable, IntoElement, Modifiers,
        MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, ParentElement as _, Pixels,
        Point, Render, ScrollDelta, ScrollWheelEvent, Styled as _, TestAppContext, TouchPhase,
        VisualTestContext, Window, div, point, px,
    };

    use super::INITIAL_PAN;

    /// A view that does nothing but hold the widget, as the host's pane would.
    pub(crate) struct Harness<V: Render> {
        /// The widget under test.
        pub(crate) widget: Entity<V>,
    }

    impl<V: Render> Render for Harness<V> {
        fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
            div().size_full().child(self.widget.clone())
        }
    }

    /// Everything a test reads back.
    pub(crate) struct Handles<V: Render, E: 'static> {
        /// The widget itself, for the tests that need the entity.
        pub(crate) widget: Entity<V>,
        /// Every event it has emitted since the last drain.
        events: Rc<RefCell<Vec<E>>>,
    }

    impl<V: Render, E: Clone + 'static> Handles<V, E> {
        /// The events emitted since this was last called.
        pub(crate) fn drain(&self) -> Vec<E> {
            self.events.borrow_mut().drain(..).collect()
        }

        /// Reads something out of the widget.
        pub(crate) fn read<R>(&self, cx: &mut VisualTestContext, f: impl FnOnce(&V) -> R) -> R {
            cx.update(|_, cx| f(self.widget.read(cx)))
        }

        /// Drives the widget and lets the resulting frame settle.
        pub(crate) fn update(
            &self,
            cx: &mut VisualTestContext,
            f: impl FnOnce(&mut V, &mut Context<V>),
        ) {
            cx.update(|_, cx| self.widget.update(cx, f));
            cx.run_until_parked();
        }
    }

    /// Opens a focused widget in a window of its own and hands back its handles.
    pub(crate) fn open<V, E>(
        cx: &mut TestAppContext,
        build: impl FnOnce(&mut Context<V>) -> V + 'static,
    ) -> (Handles<V, E>, VisualTestContext)
    where
        V: Render + Focusable + EventEmitter<E> + 'static,
        E: Clone + 'static,
    {
        cx.update(rudbman_ui::init);
        cx.update(crate::init);

        let events: Rc<RefCell<Vec<E>>> = Rc::new(RefCell::new(Vec::new()));
        let window = cx.add_window({
            let events = events.clone();
            move |_, cx| {
                let widget = cx.new(build);
                cx.subscribe(&widget, move |_: &mut Harness<V>, _, event: &E, _| {
                    events.borrow_mut().push(event.clone());
                })
                .detach();
                Harness { widget }
            }
        });
        let widget = window
            .update(cx, |harness, _, _| harness.widget.clone())
            .expect("the window is open");

        let mut cx = VisualTestContext::from_window(*window.deref(), cx);
        cx.update(|window, cx| {
            let handle = widget.read(cx).focus_handle(cx);
            handle.focus(window);
        });
        cx.run_until_parked();

        (Handles { widget, events }, cx)
    }

    /// The window point a diagram point is drawn at, with the view unpanned and
    /// unzoomed.
    pub(crate) fn window_point(x: f32, y: f32) -> Point<Pixels> {
        point(px(x + INITIAL_PAN), px(y + INITIAL_PAN))
    }

    /// Presses the left button at `at`.
    pub(crate) fn press(cx: &mut VisualTestContext, at: Point<Pixels>) {
        cx.simulate_event(MouseDownEvent {
            position: at,
            modifiers: Modifiers::none(),
            button: MouseButton::Left,
            click_count: 1,
            first_mouse: false,
        });
        cx.run_until_parked();
    }

    /// Presses the right button at `at`.
    pub(crate) fn right_press(cx: &mut VisualTestContext, at: Point<Pixels>) {
        cx.simulate_event(MouseDownEvent {
            position: at,
            modifiers: Modifiers::none(),
            button: MouseButton::Right,
            click_count: 1,
            first_mouse: false,
        });
        cx.run_until_parked();
    }

    /// Moves the pointer to `at` with the left button down.
    pub(crate) fn drag_to(cx: &mut VisualTestContext, at: Point<Pixels>) {
        cx.simulate_event(MouseMoveEvent {
            position: at,
            pressed_button: Some(MouseButton::Left),
            modifiers: Modifiers::none(),
        });
        cx.run_until_parked();
    }

    /// Releases the left button at `at`.
    pub(crate) fn release(cx: &mut VisualTestContext, at: Point<Pixels>) {
        cx.simulate_event(MouseUpEvent {
            position: at,
            modifiers: Modifiers::none(),
            button: MouseButton::Left,
            click_count: 1,
        });
        cx.run_until_parked();
    }

    /// Turns the wheel by `dy` pixels at `at`.
    pub(crate) fn wheel(
        cx: &mut VisualTestContext,
        at: Point<Pixels>,
        dy: f32,
        modifiers: Modifiers,
    ) {
        cx.simulate_event(ScrollWheelEvent {
            position: at,
            delta: ScrollDelta::Pixels(point(px(0.), px(dy))),
            modifiers,
            touch_phase: TouchPhase::Moved,
        });
        cx.run_until_parked();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_box_under_the_pointer_is_the_last_one_drawn() {
        let rects = vec![
            NodeRect {
                x: 0.,
                y: 0.,
                w: 100.,
                h: 100.,
            },
            NodeRect {
                x: 50.,
                y: 50.,
                w: 100.,
                h: 100.,
            },
        ];
        assert_eq!(hit_box(&rects, 10., 10.), Some(0));
        assert_eq!(hit_box(&rects, 60., 60.), Some(1));
        assert_eq!(hit_box(&rects, 400., 400.), None);
    }

    /// A viewport that has been measured at `width` by `height`, looking at
    /// the corner every canvas starts in.
    fn viewport(width: f32, height: f32) -> Viewport {
        Viewport {
            bounds: Bounds::new(point(px(0.), px(0.)), size(px(width), px(height))),
            ..Viewport::default()
        }
    }

    /// One box `w` by `h` at `(x, y)`.
    fn rect(x: f32, y: f32, w: f32, h: f32) -> NodeRect {
        NodeRect { x, y, w, h }
    }

    /// A canvas nobody has panned is a canvas scrolled to exactly zero, which
    /// is what makes the margin and the initial pan the same number.
    #[test]
    fn a_canvas_at_home_is_scrolled_to_the_start() {
        let rects = vec![rect(0., 0., 400., 2_000.)];
        let bar = viewport(800., 600.)
            .bar_extent(&rects, ScrollbarAxis::Vertical)
            .expect("a canvas with a box on it");

        assert_eq!(bar.scrolled, 0.);
        assert_eq!(bar.visible, 600.);
        assert_eq!(bar.scrollable, 2_000. + 2. * MARGIN - 600.);
    }

    /// The extent is the whole spread of the boxes and not the first one's, and
    /// a diagram that fits its window with its margins to spare has nothing to
    /// scroll — the bar draws itself from that alone and needs no other answer.
    #[test]
    fn a_diagram_that_fits_has_nothing_to_scroll() {
        let rects = vec![rect(0., 0., 100., 80.), rect(200., 300., 100., 80.)];
        let viewport = viewport(800., 600.);

        let across = viewport
            .bar_extent(&rects, ScrollbarAxis::Horizontal)
            .expect("a canvas with boxes on it");
        assert_eq!(across.scrollable, 300. + 2. * MARGIN - 800.);
        assert!(across.scrollable < 0.);

        let down = viewport
            .bar_extent(&rects, ScrollbarAxis::Vertical)
            .expect("a canvas with boxes on it");
        assert_eq!(down.scrollable, 380. + 2. * MARGIN - 600.);
        assert!(down.scrollable < 0.);
    }

    /// Zooming in lengthens the run without moving a box: the content is
    /// measured in screen pixels, because that is what the thumb is drawn in.
    #[test]
    fn zooming_in_lengthens_what_there_is_to_scroll() {
        let rects = vec![rect(0., 0., 1_000., 1_000.)];
        let mut viewport = viewport(800., 600.);
        let before = viewport
            .bar_extent(&rects, ScrollbarAxis::Horizontal)
            .expect("a canvas with a box on it");

        viewport.zoom = 2.;
        let after = viewport
            .bar_extent(&rects, ScrollbarAxis::Horizontal)
            .expect("a canvas with a box on it");
        assert_eq!(after.scrollable, 2_000. + 2. * MARGIN - 800.);
        assert!(after.scrollable > before.scrollable);
    }

    /// A box left of the origin is still the start of the diagram, so the
    /// canvas at home is scrolled to it rather than past it.
    #[test]
    fn the_start_of_the_content_is_where_the_leftmost_box_is() {
        let rects = vec![rect(-500., -200., 100., 80.), rect(0., 0., 100., 80.)];
        let bar = viewport(800., 600.)
            .bar_extent(&rects, ScrollbarAxis::Horizontal)
            .expect("a canvas with boxes on it");

        // Home puts the origin 24 pixels in, and the content starts 500 units
        // further back again.
        assert_eq!(bar.scrolled, 500.);
    }

    /// A drag and the thumb it moves are the same arithmetic read in opposite
    /// directions: the pan a progress asks for is the pan that reports that
    /// progress back.
    #[test]
    fn a_dragged_thumb_lands_where_it_was_dragged_to() {
        let rects = vec![rect(120., 60., 400., 3_000.)];
        let mut viewport = viewport(800., 600.);
        viewport.zoom = 1.5;

        for progress in [0., 0.35, 1.] {
            let pan = viewport
                .panned_to(&rects, ScrollbarAxis::Vertical, progress)
                .expect("a canvas with a box on it");

            let mut moved = viewport;
            moved.set_pan_along(ScrollbarAxis::Vertical, pan);
            let bar = moved
                .bar_extent(&rects, ScrollbarAxis::Vertical)
                .expect("a canvas with a box on it");
            assert!(
                (bar.scrolled / bar.scrollable - progress).abs() < 1e-4,
                "a thumb dragged to {progress} came back at {}",
                bar.scrolled / bar.scrollable
            );
            // And the other axis is left exactly where it was.
            assert_eq!(moved.pan.x, viewport.pan.x);
        }
    }

    /// A canvas with nothing on it has no content and says so, rather than
    /// claiming an empty diagram sits at the origin.
    #[test]
    fn an_empty_canvas_has_no_bar_at_all() {
        let viewport = viewport(800., 600.);

        assert_eq!(viewport.bar_extent(&[], ScrollbarAxis::Vertical), None);
        assert_eq!(viewport.bar_extent(&[], ScrollbarAxis::Horizontal), None);
        assert_eq!(viewport.panned_to(&[], ScrollbarAxis::Vertical, 0.5), None);
    }
}
