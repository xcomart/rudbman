//! Where the boxes go and how the lines get between them — all of it pure.
//!
//! Nothing in this module knows about gpui, a window, or a frame. That is the
//! point (architecture document, §7.6): the awkward half of a diagram is the
//! arithmetic, and arithmetic that needs a window to run is arithmetic that
//! does not get tested. Everything here is measured in **logical units**, which
//! are pixels at zoom 1.0; the view multiplies by its zoom and the SVG writer
//! does not multiply at all.
//!
//! ## Three things live here
//!
//! * [`measure`] — how big one box is. Worked out from character *widths*
//!   rather than by shaping the text, so it is the same number in a test, on a
//!   canvas and in an SVG file. Being a few pixels out costs nothing: the boxes
//!   are padded and the user can drag them.
//! * [`grid_layout`] and [`auto_layout`] — where the boxes go. The first is the
//!   default and is nothing more than a square of slots. The second is the
//!   four-stage Sugiyama heuristic the architecture document settled on
//!   (§12.4), written out here rather than pulled in: the crates surveyed
//!   assume uniform vertex sizes, and an ERD's boxes differ by a factor of five
//!   in height.
//! * [`route`] — an orthogonal polyline between two boxes, shared by the canvas
//!   and the SVG writer so the two pictures agree.
//!
//! ## Determinism is a requirement, not a nicety
//!
//! The same model must produce the same layout every time it is opened, because
//! a diagram whose boxes move when nothing changed reads as a bug and, worse,
//! makes the saved positions in `erd/<profile-uuid>.json` look wrong. So there
//! is no clock here, no random number generator, and no iteration over a hash
//! map: ties are broken by table index, floats are ordered with
//! [`f32::total_cmp`], and every loop has a fixed bound.

use unicode_width::UnicodeWidthStr;

use crate::model::{ErdModel, ErdTable};

/// Height of a box's title band.
pub const HEADER_HEIGHT: f32 = 26.;

/// Height of one column row inside a box.
pub const ROW_HEIGHT: f32 = 20.;

/// Padding at both ends of a title and of a column row.
pub(crate) const BOX_PADDING: f32 = 8.;

/// Smallest gap between a column's name and its type.
///
/// The two are drawn against opposite edges of the box, so this is only what
/// [`measure`] reserves between them; a box wider than its widest row simply
/// pushes them further apart.
pub(crate) const NAME_TYPE_GAP: f32 = 18.;

/// Roughly how wide one character cell is at the diagram's text size.
///
/// Measured in character cells and multiplied, rather than shaped: shaping is
/// what makes a size depend on a window, and a size that depends on a window
/// cannot be computed by a pure function. The result grid fits its columns the
/// same way, with the same constant.
pub(crate) const APPROX_ADVANCE: f32 = 7.2;

/// Narrowest a box may be.
///
/// A box narrower than this stops reading as a table even when its name is one
/// character long.
pub(crate) const MIN_BOX_WIDTH: f32 = 120.;

/// Widest a box may be.
///
/// A `VARCHAR2(4000) NOT NULL DEFAULT ...` type string would otherwise size a
/// box to a paragraph; past this the text is elided instead.
pub(crate) const MAX_BOX_WIDTH: f32 = 360.;

/// Gap between boxes, on both axes, in every layout here.
pub(crate) const NODE_GAP: f32 = 40.;

/// How far a line leaves a box before it is allowed to turn.
///
/// Without a stub the corner would sit on the box's edge and read as part of
/// the border.
const STUB: f32 = 20.;

/// How wide the loop of a self-referencing relation reaches.
const SELF_LOOP: f32 = 28.;

/// How far back from the box the crow's foot's apex sits.
pub(crate) const FOOT_LENGTH: f32 = 12.;

/// Half the spread of the crow's foot's outer prongs.
pub(crate) const FOOT_SPREAD: f32 = 5.;

/// How far from the box the "one" end's bar is drawn.
pub(crate) const BAR_OFFSET: f32 = 10.;

/// Half the length of the "one" end's bar.
pub(crate) const BAR_HALF: f32 = 6.;

/// How many median sweeps the crossing reduction is allowed.
///
/// A fixed bound rather than "until it converges", because convergence is not
/// guaranteed and a diagram that takes an unbounded time to open is worse than
/// one with a few extra crossings.
const MAX_SWEEPS: usize = 8;

/// How many transposition passes each sweep is allowed.
const MAX_TRANSPOSE: usize = 4;

/// One box's place and size, in logical units.
///
/// The origin is the top-left corner, as everywhere else in gpui. A layout is a
/// vector of these parallel to [`ErdModel::tables`], which is what lets the
/// view move one box by writing one entry.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct NodeRect {
    /// Distance from the diagram's left edge to the box's left edge.
    pub x: f32,
    /// Distance from the diagram's top edge to the box's top edge.
    pub y: f32,
    /// The box's width.
    pub w: f32,
    /// The box's height.
    pub h: f32,
}

impl NodeRect {
    /// The box's right edge.
    pub fn right(&self) -> f32 {
        self.x + self.w
    }

    /// The box's bottom edge.
    pub fn bottom(&self) -> f32 {
        self.y + self.h
    }

    /// The horizontal middle of the box.
    pub fn center_x(&self) -> f32 {
        self.x + self.w / 2.
    }

    /// The vertical middle of the box.
    pub fn center_y(&self) -> f32 {
        self.y + self.h / 2.
    }

    /// Whether `(x, y)` falls inside the box, edges included.
    pub fn contains(&self, x: f32, y: f32) -> bool {
        x >= self.x && x <= self.right() && y >= self.y && y <= self.bottom()
    }

    /// Whether this box and `other` share any area at all.
    ///
    /// Boxes that merely touch along an edge do not overlap, which is what
    /// lets a layout pack them against each other.
    pub fn overlaps(&self, other: &NodeRect) -> bool {
        self.x < other.right()
            && other.x < self.right()
            && self.y < other.bottom()
            && other.y < self.bottom()
    }
}

/// How wide `text` draws, approximately, at the diagram's text size.
pub(crate) fn text_width(text: &str) -> f32 {
    UnicodeWidthStr::width(text) as f32 * APPROX_ADVANCE
}

/// `text` cut down to fit `available` logical units, with an ellipsis when it
/// had to be cut.
///
/// Used by the canvas and by the SVG writer alike, so that a name elided in one
/// picture is elided at the same character in the other.
pub(crate) fn elide(text: &str, available: f32) -> String {
    if available <= 0. {
        return String::new();
    }
    if text_width(text) <= available {
        return text.to_string();
    }
    let room = available - text_width("\u{2026}");
    if room <= 0. {
        return "\u{2026}".to_string();
    }

    let mut used = 0.;
    let mut out = String::new();
    for grapheme in text.chars() {
        let mut buffer = [0u8; 4];
        let width = text_width(grapheme.encode_utf8(&mut buffer));
        if used + width > room {
            break;
        }
        used += width;
        out.push(grapheme);
    }
    out.push('\u{2026}');
    out
}

/// Gap kept between a drawn column name and the type beside it.
const ROW_GAP: f32 = 6.;

/// Least of a row a column name may be cut down to.
const NAME_SHARE: f32 = 0.5;

/// One column row's two labels, each cut to the room it actually gets.
///
/// The name is served first and the type gets what is left, because a name
/// identifies the column and a type only describes it — but the name is never
/// allowed past [`NAME_SHARE`] of the row unless the type is short enough not
/// to want the rest, so a `VARCHAR2(4000 CHAR)` never disappears entirely.
///
/// Shared by the canvas and by the SVG writer, which is what makes the claim
/// that the two pictures agree true rather than hopeful: a row elided in one is
/// elided at the same character in the other.
pub(crate) fn split_row(name: &str, type_name: &str, room: f32) -> (String, String) {
    let for_name = (room - text_width(type_name) - ROW_GAP).max(room * NAME_SHARE);
    let name = elide(name, for_name);
    let type_name = elide(type_name, room - text_width(&name) - ROW_GAP);
    (name, type_name)
}

/// How big `table`'s box has to be to hold its title and its columns.
///
/// The width is the widest of the title and of every `name` + gap + `type_name`
/// row, plus padding, clamped to [`MIN_BOX_WIDTH`]..=[`MAX_BOX_WIDTH`]; the
/// height is the title band plus one row per column. A table with no columns is
/// a title band and nothing else, which is what a view without a fetched column
/// list should look like.
pub fn measure(table: &ErdTable) -> (f32, f32) {
    let mut widest = text_width(&table.name);
    for column in &table.columns {
        let row = text_width(&column.name) + NAME_TYPE_GAP + text_width(&column.type_name);
        widest = widest.max(row);
    }

    let width = (widest + 2. * BOX_PADDING).clamp(MIN_BOX_WIDTH, MAX_BOX_WIDTH);
    let height = HEADER_HEIGHT + table.columns.len() as f32 * ROW_HEIGHT;
    (width, height)
}

/// How far below a box's top edge its `row`th column row begins.
///
/// Split from [`row_top`] because the canvas has the box's top corner in
/// *screen* pixels already and adding `rect.y` back only to subtract it again
/// would round twice; the SVG writer, which works in logical units throughout,
/// wants [`row_top`].
pub fn row_offset(row: usize) -> f32 {
    HEADER_HEIGHT + row as f32 * ROW_HEIGHT
}

/// The top edge of `rect`'s `row`th column row, in logical units.
///
/// One arithmetic for three pictures: the canvas draws its rows here, the SVG
/// writer puts its baselines here, and [`row_at`] answers with the row this
/// puts under a `y`. A row drawn in one picture is the row hit in the other
/// because there is only one expression to be wrong.
pub fn row_top(rect: &NodeRect, row: usize) -> f32 {
    rect.y + row_offset(row)
}

/// How many column rows `rect` was measured to hold.
///
/// Read back out of the height rather than carried alongside it: a rect is the
/// only thing a gesture has, and [`measure`] made its height exactly a title
/// band plus a whole number of rows.
fn row_count(rect: &NodeRect) -> usize {
    let rows = (rect.h - HEADER_HEIGHT) / ROW_HEIGHT;
    if rows <= 0. {
        return 0;
    }
    rows.round() as usize
}

/// Which column row of `rect` the logical `y` falls in.
///
/// [`None`] for the title band, for anything above or below the box, and for
/// the bottom edge — which [`NodeRect::contains`] includes and which is the
/// border rather than a row, so that a press there moves the box rather than
/// picking its last column.
pub fn row_at(rect: &NodeRect, y: f32) -> Option<usize> {
    let rows = row_count(rect);
    if rows == 0 {
        return None;
    }
    let offset = y - (rect.y + HEADER_HEIGHT);
    if offset < 0. {
        return None;
    }
    let row = (offset / ROW_HEIGHT).floor();
    if row >= rows as f32 {
        return None;
    }
    Some(row as usize)
}

/// The point on `rect`'s left or right edge that a line to `row` attaches at.
///
/// The middle of the row, so that a join drawn between two columns arrives at
/// the height of the column rather than at the height of the table. A row the
/// box does not have — an edge left over from a table that lost a column — is
/// pulled back onto the box rather than drawn hanging below it.
pub fn row_anchor(rect: &NodeRect, row: usize, rightwards: bool) -> (f32, f32) {
    let x = if rightwards { rect.right() } else { rect.x };
    (
        x,
        (row_top(rect, row) + ROW_HEIGHT / 2.).clamp(rect.y, rect.bottom()),
    )
}

/// Every table's box in a square of slots, in table order.
///
/// `ceil(sqrt(n))` columns, each as wide as its widest box and each row as tall
/// as its tallest, so no two boxes overlap however uneven the tables are. It is
/// not a pretty layout and it is not meant to be: it is the layout a diagram
/// opens with before anyone has dragged anything, and the one that fills in
/// around the tables a saved file did not mention.
pub fn grid_layout(model: &ErdModel) -> Vec<NodeRect> {
    let sizes: Vec<(f32, f32)> = model.tables.iter().map(measure).collect();
    grid_of(&sizes, 0., 0.)
}

/// The boxes of `sizes` packed into a square of slots whose top-left corner is
/// `(origin_x, origin_y)`.
fn grid_of(sizes: &[(f32, f32)], origin_x: f32, origin_y: f32) -> Vec<NodeRect> {
    if sizes.is_empty() {
        return Vec::new();
    }

    let columns = (sizes.len() as f32).sqrt().ceil().max(1.) as usize;
    let rows = sizes.len().div_ceil(columns);

    // Column widths and row heights first, so that a tall box in the middle of
    // the square pushes the row below it down rather than being drawn over.
    let mut column_widths = vec![0f32; columns];
    let mut row_heights = vec![0f32; rows];
    for (index, (width, height)) in sizes.iter().enumerate() {
        let column = index % columns;
        let row = index / columns;
        column_widths[column] = column_widths[column].max(*width);
        row_heights[row] = row_heights[row].max(*height);
    }

    let mut column_x = Vec::with_capacity(columns);
    let mut cursor = origin_x;
    for width in &column_widths {
        column_x.push(cursor);
        cursor += width + NODE_GAP;
    }
    let mut row_y = Vec::with_capacity(rows);
    let mut cursor = origin_y;
    for height in &row_heights {
        row_y.push(cursor);
        cursor += height + NODE_GAP;
    }

    sizes
        .iter()
        .enumerate()
        .map(|(index, (w, h))| NodeRect {
            x: column_x[index % columns],
            y: row_y[index / columns],
            w: *w,
            h: *h,
        })
        .collect()
}

/// Every table's box, arranged so that foreign keys read left to right.
///
/// The four stages the architecture document settled on (§12.4):
///
/// 1. **Break the cycles.** A depth-first walk in table order reverses every
///    back edge it meets, which leaves a DAG. Self-references take no part —
///    a table that references itself is drawn with a loop, not with a rank of
///    its own.
/// 2. **Rank.** Longest path: a table sits one rank to the right of the
///    furthest table that references it, so every edge points rightwards.
/// 3. **Order within a rank.** Median sweeps followed by adjacent
///    transpositions, keeping the best ordering seen and stopping as soon as a
///    sweep fails to improve on it.
/// 4. **Place.** Ranks are laid out left to right, each as far right as the
///    widest box of every rank before it allows; within a rank the boxes are
///    stacked downwards, each pulled towards the middle of the boxes that
///    reference it as far as the stacking allows.
///
/// Tables with no relations at all are gathered into a grid to the right of the
/// ranks rather than being given ranks of their own, which would stretch the
/// diagram sideways for nothing.
pub fn auto_layout(model: &ErdModel) -> Vec<NodeRect> {
    let count = model.tables.len();
    let sizes: Vec<(f32, f32)> = model.tables.iter().map(measure).collect();
    if count == 0 {
        return Vec::new();
    }

    let edges = acyclic_edges(model);

    // A table takes part in the ranking only if some relation other than a
    // self-reference touches it.
    let mut connected = vec![false; count];
    for &(from, to) in &edges {
        connected[from] = true;
        connected[to] = true;
    }

    let ranked: Vec<usize> = (0..count).filter(|node| connected[*node]).collect();
    let isolated: Vec<usize> = (0..count).filter(|node| !connected[*node]).collect();

    let mut rects = vec![
        NodeRect {
            x: 0.,
            y: 0.,
            w: 0.,
            h: 0.,
        };
        count
    ];
    for (node, size) in sizes.iter().enumerate() {
        rects[node].w = size.0;
        rects[node].h = size.1;
    }

    let mut ranked_width = 0f32;
    if !ranked.is_empty() {
        let (predecessors, successors) = adjacency(count, &edges);
        let rank = longest_path_ranks(count, &ranked, &predecessors, &successors);
        let layers = reduce_crossings(layers_of(&ranked, &rank), &predecessors, &successors);
        place_layers(&layers, &predecessors, &mut rects);
        ranked_width = rects
            .iter()
            .enumerate()
            .filter(|(node, _)| connected[*node])
            .fold(0f32, |widest, (_, rect)| widest.max(rect.right()));
    }

    if !isolated.is_empty() {
        let origin_x = if ranked.is_empty() {
            0.
        } else {
            ranked_width + NODE_GAP
        };
        let sizes: Vec<(f32, f32)> = isolated.iter().map(|node| sizes[*node]).collect();
        for (slot, rect) in isolated.iter().zip(grid_of(&sizes, origin_x, 0.)) {
            rects[*slot] = rect;
        }
    }

    normalise(&mut rects);
    rects
}

/// The model's relations as a DAG's edges, with the back edges of a depth-first
/// walk turned around.
///
/// Turning an edge around rather than dropping it keeps both boxes at their
/// proper distance — a cycle drawn as a chain still reads — and it is what
/// makes the ranking below terminate on the `A → B → C → A` shapes that
/// ordinary schemas are full of.
fn acyclic_edges(model: &ErdModel) -> Vec<(usize, usize)> {
    let count = model.tables.len();
    let mut raw: Vec<(usize, usize)> = Vec::new();
    for relation in model.valid_relations() {
        if relation.from != relation.to {
            raw.push((relation.from, relation.to));
        }
    }

    let mut out_edges: Vec<Vec<usize>> = vec![Vec::new(); count];
    for (index, (from, _)) in raw.iter().enumerate() {
        out_edges[*from].push(index);
    }

    // 0 = untouched, 1 = on the stack, 2 = finished. An edge into a node that
    // is still on the stack closes a cycle, and is the one to turn around.
    const WHITE: u8 = 0;
    const GREY: u8 = 1;
    const BLACK: u8 = 2;
    let mut state = vec![WHITE; count];
    let mut reversed = vec![false; raw.len()];

    // Iterative rather than recursive: the recursion depth would be the length
    // of the longest foreign-key chain, and a generated schema can have one
    // deeper than the stack.
    let mut stack: Vec<(usize, usize)> = Vec::new();
    for start in 0..count {
        if state[start] != WHITE {
            continue;
        }
        state[start] = GREY;
        stack.push((start, 0));
        while let Some(&(node, cursor)) = stack.last() {
            if cursor >= out_edges[node].len() {
                state[node] = BLACK;
                stack.pop();
                continue;
            }
            if let Some(top) = stack.last_mut() {
                top.1 += 1;
            }
            let edge = out_edges[node][cursor];
            let target = raw[edge].1;
            match state[target] {
                WHITE => {
                    state[target] = GREY;
                    stack.push((target, 0));
                }
                GREY => reversed[edge] = true,
                _ => {}
            }
        }
    }

    raw.into_iter()
        .enumerate()
        .map(|(index, (from, to))| {
            if reversed[index] {
                (to, from)
            } else {
                (from, to)
            }
        })
        .collect()
}

/// Predecessor and successor lists, deduplicated and sorted.
///
/// Sorted because a median is taken over them and two tables joined by a
/// composite key would otherwise weigh twice as much as one joined by a single
/// column — and deduplicated in the same pass for the same reason.
fn adjacency(count: usize, edges: &[(usize, usize)]) -> (Vec<Vec<usize>>, Vec<Vec<usize>>) {
    let mut predecessors: Vec<Vec<usize>> = vec![Vec::new(); count];
    let mut successors: Vec<Vec<usize>> = vec![Vec::new(); count];
    for &(from, to) in edges {
        predecessors[to].push(from);
        successors[from].push(to);
    }
    for list in predecessors.iter_mut().chain(successors.iter_mut()) {
        list.sort_unstable();
        list.dedup();
    }
    (predecessors, successors)
}

/// One rank per table: as far right as the longest chain of references
/// reaching it.
///
/// Kahn's ordering, smallest index first, so the answer does not depend on how
/// the edges happened to be listed. A node the ordering never reaches — which
/// can only happen if a cycle survived stage one — keeps rank zero rather than
/// stopping the layout.
fn longest_path_ranks(
    count: usize,
    ranked: &[usize],
    predecessors: &[Vec<usize>],
    successors: &[Vec<usize>],
) -> Vec<usize> {
    let mut rank = vec![0usize; count];
    let mut remaining: Vec<usize> = vec![0; count];
    for &node in ranked {
        remaining[node] = predecessors[node].len();
    }

    let mut ready: Vec<usize> = ranked
        .iter()
        .copied()
        .filter(|node| remaining[*node] == 0)
        .collect();
    // Kept in descending order so that `pop` always takes the smallest index.
    ready.sort_unstable_by(|a, b| b.cmp(a));

    while let Some(node) = ready.pop() {
        for &next in &successors[node] {
            rank[next] = rank[next].max(rank[node] + 1);
            remaining[next] -= 1;
            if remaining[next] == 0 {
                ready.push(next);
            }
        }
        ready.sort_unstable_by(|a, b| b.cmp(a));
    }
    rank
}

/// The ranked tables gathered into one list per rank, in table order.
fn layers_of(ranked: &[usize], rank: &[usize]) -> Vec<Vec<usize>> {
    let depth = ranked.iter().map(|node| rank[*node]).max().unwrap_or(0) + 1;
    let mut layers: Vec<Vec<usize>> = vec![Vec::new(); depth];
    for &node in ranked {
        layers[rank[node]].push(node);
    }
    layers
}

/// Where every table sits inside its own rank.
fn positions(layers: &[Vec<usize>], count: usize) -> Vec<usize> {
    let mut pos = vec![0usize; count];
    for layer in layers {
        for (index, node) in layer.iter().enumerate() {
            pos[*node] = index;
        }
    }
    pos
}

/// Reorders each rank so that fewer lines cross.
///
/// Median sweeps alternating downwards and upwards, each followed by
/// transpositions, keeping the best ordering seen. It stops the moment a sweep
/// fails to beat the best — a heuristic that has no optimum to converge on, so
/// the useful stopping rule is "no longer helping".
fn reduce_crossings(
    mut layers: Vec<Vec<usize>>,
    predecessors: &[Vec<usize>],
    successors: &[Vec<usize>],
) -> Vec<Vec<usize>> {
    let count = predecessors.len();
    let mut best = layers.clone();
    let mut fewest = crossings(&layers, successors, count);

    for sweep in 0..MAX_SWEEPS {
        if fewest == 0 {
            break;
        }
        if sweep % 2 == 0 {
            median_sweep(&mut layers, predecessors, count, true);
        } else {
            median_sweep(&mut layers, successors, count, false);
        }
        transpose(&mut layers, predecessors, successors, count);

        let found = crossings(&layers, successors, count);
        if found >= fewest {
            break;
        }
        fewest = found;
        best = layers.clone();
    }

    best
}

/// One median pass: every rank is reordered by the middle position of its
/// neighbours in the rank before it (`downwards`) or after it.
///
/// A table with no neighbour on that side keeps its place, which is what stops
/// the isolated-looking members of a rank from being swept to one end.
fn median_sweep(
    layers: &mut [Vec<usize>],
    neighbours: &[Vec<usize>],
    count: usize,
    downwards: bool,
) {
    let order: Vec<usize> = if downwards {
        (1..layers.len()).collect()
    } else {
        (0..layers.len().saturating_sub(1)).rev().collect()
    };

    for index in order {
        let pos = positions(layers, count);
        let mut keyed: Vec<(usize, f32)> = layers[index]
            .iter()
            .enumerate()
            .map(|(slot, node)| {
                let mut of: Vec<usize> = neighbours[*node].iter().map(|next| pos[*next]).collect();
                of.sort_unstable();
                let median = if of.is_empty() {
                    slot as f32
                } else if of.len() % 2 == 1 {
                    of[of.len() / 2] as f32
                } else {
                    (of[of.len() / 2 - 1] + of[of.len() / 2]) as f32 / 2.
                };
                (*node, median)
            })
            .collect();
        // Ties fall back to the table index, so the answer never depends on the
        // sort's stability or on the order the edges arrived in.
        keyed.sort_by(|a, b| a.1.total_cmp(&b.1).then_with(|| a.0.cmp(&b.0)));
        layers[index] = keyed.into_iter().map(|(node, _)| node).collect();
    }
}

/// Swaps neighbours within a rank while doing so removes crossings.
fn transpose(
    layers: &mut [Vec<usize>],
    predecessors: &[Vec<usize>],
    successors: &[Vec<usize>],
    count: usize,
) {
    for _ in 0..MAX_TRANSPOSE {
        let mut improved = false;
        for index in 0..layers.len() {
            let pos = positions(layers, count);
            let mut slot = 0;
            while slot + 1 < layers[index].len() {
                let left = layers[index][slot];
                let right = layers[index][slot + 1];
                let before = pair_crossings(left, right, &pos, predecessors, successors);
                let after = pair_crossings(right, left, &pos, predecessors, successors);
                if after < before {
                    layers[index].swap(slot, slot + 1);
                    improved = true;
                }
                slot += 1;
            }
        }
        if !improved {
            break;
        }
    }
}

/// How many crossings the edges of `left` and `right` make when `left` is drawn
/// above `right` in their rank.
fn pair_crossings(
    left: usize,
    right: usize,
    pos: &[usize],
    predecessors: &[Vec<usize>],
    successors: &[Vec<usize>],
) -> usize {
    let mut total = 0;
    for side in [predecessors, successors] {
        for &a in &side[left] {
            for &b in &side[right] {
                if pos[a] > pos[b] {
                    total += 1;
                }
            }
        }
    }
    total
}

/// How many crossings the whole ordering makes, counted rank pair by rank pair.
fn crossings(layers: &[Vec<usize>], successors: &[Vec<usize>], count: usize) -> usize {
    let pos = positions(layers, count);
    let mut rank = vec![usize::MAX; count];
    for (index, layer) in layers.iter().enumerate() {
        for node in layer {
            rank[*node] = index;
        }
    }

    let mut total = 0;
    for (index, layer) in layers.iter().enumerate() {
        let mut pairs: Vec<(usize, usize)> = Vec::new();
        for node in layer {
            for &next in &successors[*node] {
                if rank[next] == index + 1 {
                    pairs.push((pos[*node], pos[next]));
                }
            }
        }
        pairs.sort_unstable();
        for (first, pair) in pairs.iter().enumerate() {
            for other in &pairs[first + 1..] {
                if pair.1 > other.1 {
                    total += 1;
                }
            }
        }
    }
    total
}

/// Gives every ranked table an `x` from its rank and a `y` from its place in it.
///
/// A rank's `x` clears every rank before it, so a wide box never overlaps the
/// next column of boxes. Within a rank each box is pulled towards the middle of
/// the boxes that reference it and then pushed back down until it clears the box
/// above — which is what turns a chain of references into a straight line
/// without ever letting two boxes touch.
fn place_layers(layers: &[Vec<usize>], predecessors: &[Vec<usize>], rects: &mut [NodeRect]) {
    let mut x = 0f32;
    for layer in layers {
        let widest = layer
            .iter()
            .fold(0f32, |widest, node| widest.max(rects[*node].w));
        let mut cursor = 0f32;
        for node in layer {
            // Every predecessor sits in an earlier rank, so it has already been
            // placed and its centre is the one it will keep.
            let mut wanted: Vec<f32> = predecessors[*node]
                .iter()
                .map(|before| rects[*before].center_y())
                .collect();
            wanted.sort_by(f32::total_cmp);
            let centre = match wanted.len() {
                0 => f32::MIN,
                odd if odd % 2 == 1 => wanted[odd / 2],
                even => (wanted[even / 2 - 1] + wanted[even / 2]) / 2.,
            };

            let top = if centre == f32::MIN {
                cursor
            } else {
                cursor.max(centre - rects[*node].h / 2.)
            };
            rects[*node].x = x;
            rects[*node].y = top;
            cursor = top + rects[*node].h + NODE_GAP;
        }
        x += widest + NODE_GAP;
    }
}

/// Slides the whole layout so its top-left corner is the origin.
fn normalise(rects: &mut [NodeRect]) {
    let left = rects.iter().fold(f32::MAX, |least, rect| least.min(rect.x));
    let top = rects.iter().fold(f32::MAX, |least, rect| least.min(rect.y));
    if left == f32::MAX || top == f32::MAX {
        return;
    }
    for rect in rects {
        rect.x -= left;
        rect.y -= top;
    }
}

/// An orthogonal polyline from `from`'s edge to `to`'s edge.
///
/// The line leaves whichever vertical edge of `from` faces `to` and arrives at
/// the opposite edge of `to`, so it never runs across either box. When the two
/// have room between them that is three segments with the turn halfway; when
/// they do not — boxes side by side, or `to` behind `from` — it is five, out to
/// a stub, across, and back in.
///
/// Deliberately *not* an obstacle-avoiding router. A line that detours around a
/// third box is a line whose shape changes when an unrelated box is dragged,
/// and in a diagram whose whole point is that the user arranges it by hand,
/// predictable beats tidy.
///
/// A relation from a table to itself — `from` and `to` being the same box — is
/// drawn as a small loop off the right-hand edge instead.
pub fn route(from: &NodeRect, to: &NodeRect) -> Vec<(f32, f32)> {
    route_between(from, from.center_y(), to, to.center_y())
}

/// [`route`], but leaving and arriving at heights of the caller's choosing.
///
/// A foreign key belongs to two *tables* and leaves from the middle of each
/// box, which is what [`route`] asks for. A join belongs to two *columns*, so
/// the query builder asks for the same polyline at two row anchors
/// ([`row_anchor`]) instead. One router, because a diagram in which the two
/// kinds of line turn differently reads as two diagrams.
///
/// The heights only choose where the line meets each box: which edge it leaves
/// by is still judged from the two centres, so a line does not flip sides when
/// a join is drawn from a lower row.
pub fn route_between(from: &NodeRect, from_y: f32, to: &NodeRect, to_y: f32) -> Vec<(f32, f32)> {
    if from == to {
        // A loop needs two different heights to have a shape at all, so a
        // relation to the same box — which arrives with one — is given the
        // thirds it has always been drawn with.
        let (top, bottom) = if from_y == to_y {
            (from.y + from.h * 0.35, from.y + from.h * 0.65)
        } else {
            (from_y, to_y)
        };
        let out = from.right() + SELF_LOOP;
        return vec![
            (from.right(), top),
            (out, top),
            (out, bottom),
            (from.right(), bottom),
        ];
    }

    // Which way out: towards `to`, judged by the two centres so that a box
    // directly above another still picks a side rather than flickering between
    // them as it is dragged.
    let rightwards = to.center_x() >= from.center_x();
    let (start, end, direction) = if rightwards {
        ((from.right(), from_y), (to.x, to_y), 1.)
    } else {
        ((from.x, from_y), (to.right(), to_y), -1.)
    };

    let gap = (end.0 - start.0) * direction;
    if gap >= 2. * STUB {
        let middle = (start.0 + end.0) / 2.;
        return without_repeats(vec![start, (middle, start.1), (middle, end.1), end]);
    }

    let out = start.0 + direction * STUB;
    let back = end.0 - direction * STUB;
    let middle = (start.1 + end.1) / 2.;
    without_repeats(vec![
        start,
        (out, start.1),
        (out, middle),
        (back, middle),
        (back, end.1),
        end,
    ])
}

/// The polyline with every repeated point dropped.
///
/// Two boxes whose centres line up produce a turn that turns through nothing,
/// and a zero-length segment is not merely redundant: it is what a stroke
/// tessellator chokes on, and it would put a cardinality mark's direction at
/// the mercy of a division by zero.
fn without_repeats(mut points: Vec<(f32, f32)>) -> Vec<(f32, f32)> {
    points.dedup();
    points
}

/// The unit vector a polyline leaves its first point along.
///
/// The cardinality marks are drawn against it, so both pictures ask the same
/// question of the same polyline and get the same answer.
pub(crate) fn head_direction(points: &[(f32, f32)]) -> (f32, f32) {
    match (points.first(), points.get(1)) {
        (Some(first), Some(second)) => unit(*first, *second),
        _ => (1., 0.),
    }
}

/// The unit vector a polyline leaves its last point along, pointing back up the
/// line.
pub(crate) fn tail_direction(points: &[(f32, f32)]) -> (f32, f32) {
    let len = points.len();
    if len < 2 {
        return (-1., 0.);
    }
    unit(points[len - 1], points[len - 2])
}

/// `to - from`, normalised, or the identity when the two coincide.
fn unit(from: (f32, f32), to: (f32, f32)) -> (f32, f32) {
    let (dx, dy) = (to.0 - from.0, to.1 - from.1);
    let length = (dx * dx + dy * dy).sqrt();
    if length <= f32::EPSILON {
        (1., 0.)
    } else {
        (dx / length, dy / length)
    }
}

/// The three prongs of the crow's foot that marks the "many" end.
///
/// `at` is the point on the box's edge and `direction` points away from it,
/// along the line. Returned as segments rather than as a path so that the
/// canvas and the SVG writer can each draw them the way their own API prefers.
pub(crate) fn crow_foot(at: (f32, f32), direction: (f32, f32)) -> [[(f32, f32); 2]; 3] {
    let apex = (
        at.0 + direction.0 * FOOT_LENGTH,
        at.1 + direction.1 * FOOT_LENGTH,
    );
    // Perpendicular to the line, which for these routes is always the other
    // axis.
    let (px, py) = (-direction.1 * FOOT_SPREAD, direction.0 * FOOT_SPREAD);
    [
        [apex, at],
        [apex, (at.0 + px, at.1 + py)],
        [apex, (at.0 - px, at.1 - py)],
    ]
}

/// The single bar that marks the "one" end.
pub(crate) fn key_bar(at: (f32, f32), direction: (f32, f32)) -> [(f32, f32); 2] {
    let centre = (
        at.0 + direction.0 * BAR_OFFSET,
        at.1 + direction.1 * BAR_OFFSET,
    );
    let (px, py) = (-direction.1 * BAR_HALF, direction.0 * BAR_HALF);
    [
        (centre.0 + px, centre.1 + py),
        (centre.0 - px, centre.1 - py),
    ]
}

/// The smallest box holding every rect, or `None` when there are none.
pub(crate) fn extent(rects: &[NodeRect]) -> Option<NodeRect> {
    let first = rects.first()?;
    let mut left = first.x;
    let mut top = first.y;
    let mut right = first.right();
    let mut bottom = first.bottom();
    for rect in &rects[1..] {
        left = left.min(rect.x);
        top = top.min(rect.y);
        right = right.max(rect.right());
        bottom = bottom.max(rect.bottom());
    }
    Some(NodeRect {
        x: left,
        y: top,
        w: right - left,
        h: bottom - top,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{ErdColumn, ErdRelation};

    /// A table of `columns` plain columns, wide enough to be interesting.
    fn table(name: &str, columns: &[(&str, &str)]) -> ErdTable {
        ErdTable {
            name: name.to_string(),
            columns: columns
                .iter()
                .map(|(column, kind)| ErdColumn::new(*column, *kind))
                .collect(),
        }
    }

    /// A relation on one column pair.
    fn relation(from: usize, to: usize) -> ErdRelation {
        ErdRelation {
            name: None,
            from,
            to,
            columns: vec![("fk".to_string(), "pk".to_string())],
        }
    }

    /// `A → B → C → A`, the shape that made a hand-written cycle break
    /// necessary in the first place.
    fn cyclic() -> ErdModel {
        ErdModel {
            tables: vec![
                table("a", &[("id", "int"), ("c_id", "int")]),
                table("b", &[("id", "int"), ("a_id", "int")]),
                table("c", &[("id", "int"), ("b_id", "int")]),
            ],
            relations: vec![relation(0, 1), relation(1, 2), relation(2, 0)],
        }
    }

    #[test]
    fn a_box_is_measured_the_same_way_every_time() {
        let table = table(
            "orders",
            &[("id", "NUMBER(19)"), ("customer_id", "NUMBER(19)")],
        );
        let first = measure(&table);
        let second = measure(&table);
        assert_eq!(first, second);
        assert_eq!(first.1, HEADER_HEIGHT + 2. * ROW_HEIGHT);
    }

    #[test]
    fn a_box_is_never_narrower_or_wider_than_the_clamp() {
        let tiny = measure(&table("a", &[]));
        assert_eq!(tiny.0, MIN_BOX_WIDTH);

        let huge = measure(&table(
            "a",
            &[(
                "a_very_long_column_name_indeed_and_then_some_more",
                "VARCHAR2(4000 CHAR) NOT NULL DEFAULT 'nothing at all'",
            )],
        ));
        assert_eq!(huge.0, MAX_BOX_WIDTH);
    }

    #[test]
    fn a_wide_column_widens_its_box() {
        let narrow = measure(&table("t", &[("id", "int")])).0;
        let wide = measure(&table("t", &[("a_much_longer_column", "int")])).0;
        assert!(wide > narrow, "{wide} should exceed {narrow}");
    }

    #[test]
    fn the_grid_never_overlaps_two_boxes() {
        let model = ErdModel {
            tables: (0..11)
                .map(|index| {
                    let name = format!("table_{index}");
                    let columns: Vec<(&str, &str)> =
                        (0..index).map(|_| ("column", "int")).collect();
                    table(&name, &columns)
                })
                .collect(),
            relations: Vec::new(),
        };

        let rects = grid_layout(&model);
        assert_eq!(rects.len(), model.tables.len());
        for (first, a) in rects.iter().enumerate() {
            for b in &rects[first + 1..] {
                assert!(!a.overlaps(b), "{a:?} overlaps {b:?}");
            }
        }
    }

    #[test]
    fn the_grid_of_nothing_is_nothing() {
        assert!(grid_layout(&ErdModel::default()).is_empty());
    }

    #[test]
    fn a_cycle_lays_out_without_panicking() {
        let rects = auto_layout(&cyclic());
        assert_eq!(rects.len(), 3);
        for (first, a) in rects.iter().enumerate() {
            for b in &rects[first + 1..] {
                assert!(!a.overlaps(b), "{a:?} overlaps {b:?}");
            }
        }
    }

    #[test]
    fn the_same_model_always_lays_out_the_same_way() {
        let model = cyclic();
        let first = auto_layout(&model);
        for _ in 0..8 {
            assert_eq!(auto_layout(&model), first);
        }
    }

    #[test]
    fn a_self_reference_takes_no_rank_of_its_own() {
        let model = ErdModel {
            tables: vec![
                table("employee", &[("id", "int"), ("manager_id", "int")]),
                table("department", &[("id", "int")]),
            ],
            relations: vec![relation(0, 0), relation(0, 1)],
        };

        let rects = auto_layout(&model);
        assert_eq!(rects.len(), 2);
        assert!(!rects[0].overlaps(&rects[1]));
        // The self-reference contributes no edge, so `employee` is ranked only
        // by the relation to `department` and sits to its left.
        assert!(rects[0].x < rects[1].x, "{rects:?}");
    }

    #[test]
    fn a_self_reference_alone_still_lays_out() {
        let model = ErdModel {
            tables: vec![table("employee", &[("id", "int"), ("manager_id", "int")])],
            relations: vec![relation(0, 0)],
        };
        let rects = auto_layout(&model);
        assert_eq!(rects.len(), 1);
        assert_eq!((rects[0].x, rects[0].y), (0., 0.));
    }

    #[test]
    fn references_run_left_to_right() {
        let model = ErdModel {
            tables: vec![
                table("order_line", &[("id", "int")]),
                table("order", &[("id", "int")]),
                table("customer", &[("id", "int")]),
            ],
            relations: vec![relation(0, 1), relation(1, 2)],
        };
        let rects = auto_layout(&model);
        assert!(rects[0].x < rects[1].x);
        assert!(rects[1].x < rects[2].x);
    }

    #[test]
    fn tables_no_relation_touches_are_gathered_beside_the_ranks() {
        let model = ErdModel {
            tables: vec![
                table("a", &[("id", "int")]),
                table("b", &[("id", "int")]),
                table("lonely", &[("id", "int")]),
            ],
            relations: vec![relation(0, 1)],
        };
        let rects = auto_layout(&model);
        assert!(rects[2].x >= rects[1].right(), "{rects:?}");
    }

    #[test]
    fn out_of_range_relations_are_skipped_rather_than_indexed() {
        let model = ErdModel {
            tables: vec![table("a", &[("id", "int")])],
            relations: vec![relation(0, 9)],
        };
        assert_eq!(auto_layout(&model).len(), 1);
    }

    #[test]
    fn a_route_starts_and_ends_on_an_edge_wherever_the_boxes_are() {
        let from = NodeRect {
            x: 0.,
            y: 0.,
            w: 100.,
            h: 60.,
        };
        let elsewhere = [
            // to the right, with room
            (400., 0.),
            // to the left, with room
            (-400., 0.),
            // above and to the right
            (400., -300.),
            // below and to the left
            (-400., 300.),
            // side by side, no room to turn
            (110., 0.),
            // overlapping horizontally
            (40., 200.),
        ];

        for (x, y) in elsewhere {
            let to = NodeRect {
                x,
                y,
                w: 120.,
                h: 80.,
            };
            let points = route(&from, &to);
            assert!(points.len() >= 3, "{points:?}");

            let start = points[0];
            let end = points[points.len() - 1];
            assert!(
                start.0 == from.x || start.0 == from.right(),
                "start {start:?} is not on a vertical edge of {from:?}"
            );
            assert!(start.1 >= from.y && start.1 <= from.bottom());
            assert!(
                end.0 == to.x || end.0 == to.right(),
                "end {end:?} is not on a vertical edge of {to:?}"
            );
            assert!(end.1 >= to.y && end.1 <= to.bottom());

            // Every segment is horizontal or vertical: that is what makes it an
            // orthogonal route rather than a line.
            for pair in points.windows(2) {
                assert!(
                    pair[0].0 == pair[1].0 || pair[0].1 == pair[1].1,
                    "{pair:?} is neither horizontal nor vertical"
                );
            }
        }
    }

    #[test]
    fn a_self_route_leaves_and_returns_on_the_right_edge() {
        let rect = NodeRect {
            x: 10.,
            y: 20.,
            w: 100.,
            h: 60.,
        };
        let points = route(&rect, &rect);
        assert_eq!(points.len(), 4);
        assert_eq!(points[0].0, rect.right());
        assert_eq!(points[3].0, rect.right());
        assert!(points[1].0 > rect.right());
    }

    /// A box with three rows, away from the origin so that a bug that forgets
    /// to add `rect.y` shows up.
    fn rows_rect() -> NodeRect {
        NodeRect {
            x: 100.,
            y: 200.,
            w: 160.,
            h: HEADER_HEIGHT + 3. * ROW_HEIGHT,
        }
    }

    #[test]
    fn a_row_is_found_where_it_was_drawn() {
        let rect = rows_rect();
        for row in 0..3 {
            let top = row_top(&rect, row);
            assert_eq!(top, rect.y + row_offset(row));
            assert_eq!(row_at(&rect, top), Some(row));
            assert_eq!(row_at(&rect, top + ROW_HEIGHT / 2.), Some(row));
            assert_eq!(row_at(&rect, top + ROW_HEIGHT - 0.01), Some(row));
        }
    }

    #[test]
    fn the_title_band_and_everything_past_the_last_row_belong_to_no_row() {
        let rect = rows_rect();
        assert_eq!(row_at(&rect, rect.y), None);
        assert_eq!(row_at(&rect, rect.y + HEADER_HEIGHT - 0.01), None);
        // The bottom edge, which `contains` includes, is the border rather than
        // the last row: a press there moves the box instead.
        assert_eq!(row_at(&rect, rect.bottom()), None);
        assert_eq!(row_at(&rect, rect.bottom() + 40.), None);
        assert_eq!(row_at(&rect, rect.y - 40.), None);

        // A box with no columns is a title band and nothing else.
        let empty = NodeRect {
            h: HEADER_HEIGHT,
            ..rect
        };
        assert_eq!(row_at(&empty, empty.y + HEADER_HEIGHT), None);
    }

    #[test]
    fn an_anchor_sits_on_an_edge_at_the_middle_of_its_row() {
        let rect = rows_rect();
        let right = row_anchor(&rect, 1, true);
        let left = row_anchor(&rect, 1, false);
        assert_eq!(right.0, rect.right());
        assert_eq!(left.0, rect.x);
        assert_eq!(right.1, left.1);
        assert_eq!(right.1, row_top(&rect, 1) + ROW_HEIGHT / 2.);
        // Round trip: the anchor of a row is in that row.
        assert_eq!(row_at(&rect, right.1), Some(1));

        // A row the box does not have — an edge left over from a table that
        // lost a column — is pulled back onto the box rather than drawn below
        // it.
        let past = row_anchor(&rect, 99, true);
        assert!(past.1 <= rect.bottom() && past.1 >= rect.y, "{past:?}");
    }

    #[test]
    fn a_route_is_the_generalised_route_at_the_two_centres() {
        let from = rows_rect();
        let elsewhere = [
            (400., 0.),
            (-400., 0.),
            (400., -300.),
            (-400., 300.),
            (110., 0.),
            (40., 200.),
        ];
        for (x, y) in elsewhere {
            let to = NodeRect {
                x,
                y,
                w: 120.,
                h: 80.,
            };
            assert_eq!(
                route(&from, &to),
                route_between(&from, from.center_y(), &to, to.center_y())
            );
        }
        // Including the self-reference, whose loop the generalisation must not
        // have moved.
        assert_eq!(
            route(&from, &from),
            route_between(&from, from.center_y(), &from, from.center_y())
        );
    }

    #[test]
    fn a_row_route_leaves_and_arrives_at_the_rows_it_was_given() {
        let from = rows_rect();
        let to = NodeRect {
            x: 600.,
            y: 40.,
            w: 120.,
            h: HEADER_HEIGHT + 2. * ROW_HEIGHT,
        };
        let start = row_anchor(&from, 2, true);
        let end = row_anchor(&to, 1, false);
        let points = route_between(&from, start.1, &to, end.1);

        assert_eq!(points[0], start);
        assert_eq!(points[points.len() - 1], end);
        for pair in points.windows(2) {
            assert!(
                pair[0].0 == pair[1].0 || pair[0].1 == pair[1].1,
                "{pair:?} is neither horizontal nor vertical"
            );
        }
    }

    #[test]
    fn a_self_route_between_two_rows_loops_at_their_heights() {
        let rect = rows_rect();
        let top = row_anchor(&rect, 0, true);
        let bottom = row_anchor(&rect, 2, true);
        let points = route_between(&rect, top.1, &rect, bottom.1);
        assert_eq!(points.len(), 4);
        assert_eq!(points[0], top);
        assert_eq!(points[3], bottom);
        assert!(points[1].0 > rect.right());
    }

    #[test]
    fn elision_never_grows_the_text() {
        assert_eq!(elide("short", 500.), "short");
        let cut = elide("a_rather_long_identifier", 40.);
        assert!(cut.ends_with('\u{2026}'));
        assert!(text_width(&cut) <= 40.);
        assert_eq!(elide("anything", 0.), "");
    }

    #[test]
    fn a_crow_foot_has_three_prongs_meeting_at_one_apex() {
        let prongs = crow_foot((10., 20.), (1., 0.));
        assert_eq!(prongs[0][0], prongs[1][0]);
        assert_eq!(prongs[1][0], prongs[2][0]);
        assert_eq!(prongs[0][1], (10., 20.));
        assert_eq!(prongs[0][0], (10. + FOOT_LENGTH, 20.));
    }

    #[test]
    fn a_key_bar_crosses_the_line() {
        let bar = key_bar((10., 20.), (1., 0.));
        assert_eq!(bar[0].0, bar[1].0);
        assert_eq!(bar[0].0, 10. + BAR_OFFSET);
        assert_eq!(bar[1].1 - bar[0].1, -2. * BAR_HALF);
    }

    #[test]
    fn directions_follow_the_polyline() {
        let points = vec![(0., 0.), (10., 0.), (10., 10.)];
        assert_eq!(head_direction(&points), (1., 0.));
        assert_eq!(tail_direction(&points), (0., -1.));
    }
}
