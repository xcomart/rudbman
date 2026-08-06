//! How a work area is divided into panes.
//!
//! The body of the window shows one binary tree — the active connection's; the
//! shell keeps a tree per connection tab — where every leaf is one [`Pane`] — a
//! strip of tabs and the one of them on top — and every interior node divides
//! its area in two along an [`Axis`]. Splitting a pane replaces its leaf with a
//! split; closing a pane collapses the split it sat in and promotes its sibling
//! into the split's place, which is what keeps the tree free of one-child nodes.
//!
//! The *mechanics* are deliberately free of gpui: the promotion and collapse
//! rules are the part of a split layout that is easy to get subtly wrong, so
//! they live in a plain data structure with unit tests of their own, over a
//! payload the tree never looks inside. The view layer walks the result through
//! [`PaneTree::root`] and renders one nested flex box per node.
//!
//! [`Pane`] — the shell's own payload — is the exception, and only because a
//! panel *is* a view: it carries the entity handles the renderer needs. Nothing
//! above it does.
//!
//! # Why the tree is generic *and* the payload is concrete
//!
//! [`PaneTree`] is generic over what a leaf holds, and the shell instantiates it
//! at `PaneTree<Pane>`. Both halves of that are deliberate.
//!
//! The tree is generic because none of its rules depend on the payload: a
//! split, a promotion and a collapse rearrange *shape*, and a tree hardwired to
//! [`Pane`] would make every test of those rules build a database view first.
//! The mechanics tests below stay on a `u32` payload for exactly that reason —
//! distinguishable leaves make an assertion about layout order readable in a way
//! three empty panes never could.
//!
//! A tab is an enum rather than a boxed trait object because the shell has to
//! know what it is looking at anyway: the status bar reports a row count for a
//! grid and nothing of the sort for an ERD canvas, and a `Box<dyn Panel>` would
//! only push that decision into a downcast. A new kind of panel is one variant
//! of [`PaneItem`] plus one arm where the shell renders the active tab; the
//! tree, and every test in this file, stays untouched.
//!
//! # Why a pane holds a list rather than one panel
//!
//! Reading a table's columns while writing the statement that queries it is the
//! ordinary case, and a pane that held exactly one thing answered it by throwing
//! the previous panel away. Tabs make the two coexist without forcing the user
//! to split the window for every object they open. An empty list is a state of
//! its own — the pane keeps standing, showing the empty state — because closing
//! the last tab must not rearrange a layout the user placed.

use std::sync::atomic::{AtomicU64, Ordering};

use gpui::{App, Entity, ScrollHandle, SharedString};

use crate::erd_pane::{ErdPane, ErdTarget};
use crate::explorer::{ConnectionId, ObjectTarget};
use crate::i18n::ts;
use crate::query::QueryPane;
use crate::table_detail::TableDetail;

/// Fraction of a split the first child gets when the split is created.
///
/// Panes always start out even; dragging the divider writes a new ratio through
/// [`PaneTree::set_ratio`]. The value lives on the split rather than on the
/// renderer so that it survives a repaint and being merged into another tree as
/// a subtree.
const EVEN: f32 = 0.5;

/// Failure message for the one invariant this module upholds internally.
const ROOT_INVARIANT: &str = "a pane tree always has a root";

/// Source of [`PaneId`]s.
///
/// Process wide rather than per tree on purpose: [`PaneTree::merge_subtree`]
/// splices a whole foreign tree in verbatim, and ids that are unique everywhere
/// mean the ids its owner had stored — the active pane of the tree being merged
/// in, for instance — still point at the same pane afterwards instead of having
/// to be remapped.
static NEXT_PANE_ID: AtomicU64 = AtomicU64::new(1);

/// Source of [`SplitId`]s.
///
/// Separate from [`NEXT_PANE_ID`] only so the two counters stay readable in a
/// debug dump; the types are distinct either way, which is the point — a split
/// id and a pane id name different things and must not be swappable.
static NEXT_SPLIT_ID: AtomicU64 = AtomicU64::new(1);

/// What one tab of a pane shows.
///
/// The tree knows none of these variants — see the module docs — so a milestone
/// that adds a kind of panel adds a variant here and an arm to the renderer,
/// and touches nothing else.
///
/// This enum is the one thing in the file that is *not* free of gpui: a panel is
/// a view, and a view is an [`Entity`]. The mechanics above it stay generic and
/// testable on a plain payload — that is what [`PaneTree`]'s type parameter is
/// for, and what the tests below use — while the shell's own instantiation
/// carries the handles the renderer needs.
#[derive(Debug)]
pub enum PaneItem {
    /// One object's columns, keys, references and DDL.
    ///
    /// Holds the panel itself: dropping the tab drops the handle, and with it
    /// the view and whatever fetch it had out.
    TableDetail(Entity<TableDetail>),
    /// A SQL editor over the results of what it ran.
    ///
    /// One variant rather than the `Editor | Grid` pair §7.1 sketches: the two
    /// halves share a statement, a cursor and a generation counter, and putting
    /// them in separate panes would mean a channel between them that neither
    /// the user nor the layout ever asked for.
    Query {
        /// The editor and its results.
        pane: Entity<QueryPane>,
        /// Which query pane of this window it is, counting from one.
        ///
        /// Kept beside the view rather than inside it because it is the tab
        /// strip's business and nothing else's: the pane runs statements and
        /// knows nothing of where it is drawn. Numbers are never reused, so two
        /// tabs open at once never carry the same title.
        number: u64,
    },
    /// One schema's tables and the foreign keys between them.
    ///
    /// Holds only the panel, because the panel knows what it is drawing: a
    /// scope, which is also what tells two ERD tabs apart.
    Erd(Entity<ErdPane>),
    // M7 adds `QueryBuilder`; see the architecture document, §7.1.
}

impl PaneItem {
    /// The title its tab carries.
    ///
    /// A detail panel is named after the object it describes, qualified by its
    /// schema; a query pane has no name of its own, so it takes its number; a
    /// diagram is named after the scope it covers.
    pub fn title(&self, cx: &App) -> SharedString {
        match self {
            PaneItem::TableDetail(panel) => SharedString::from(panel.read(cx).target().qualified()),
            PaneItem::Query { number, .. } => ts!("query.tab", index = number),
            PaneItem::Erd(panel) => panel.read(cx).target().title(),
        }
    }

    /// The connection this tab belongs to, which is the colour its dot takes.
    ///
    /// One tree belongs to one connection, so every tab of a strip answers the
    /// same — which is what makes the dot readable as "this pane is on staging"
    /// rather than as a per-tab tag.
    pub fn connection(&self, cx: &App) -> ConnectionId {
        match self {
            PaneItem::TableDetail(panel) => panel.read(cx).target().connection,
            PaneItem::Query { pane, .. } => pane.read(cx).connection(),
            PaneItem::Erd(panel) => panel.read(cx).target().connection,
        }
    }
}

/// One pane of the work area: its tabs, and which of them is on top.
///
/// Only the active tab is rendered. That is what makes closing or switching one
/// a focus hazard — gpui resolves actions against the focused element of the
/// last drawn frame — and why the shell reclaims the keyboard around every call
/// that changes what this returns from [`Pane::active`].
#[derive(Debug, Default)]
pub struct Pane {
    /// The tabs, in the order the strip draws them.
    items: Vec<PaneItem>,
    /// Index into [`Pane::items`] of the tab on top. Meaningless while empty,
    /// and kept in range by every method that removes a tab.
    active: usize,
    /// Horizontal scroll of this pane's own tab strip.
    ///
    /// One per pane rather than one per window: two panes side by side each
    /// scroll their own strip, and a shared handle would move both.
    scroll: ScrollHandle,
}

impl Pane {
    /// A pane with nothing in it, which renders the empty state.
    pub fn new() -> Self {
        Self::default()
    }

    /// The tabs, in display order.
    pub fn items(&self) -> &[PaneItem] {
        &self.items
    }

    /// Whether the pane holds no tabs at all.
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// Index of the tab on top; zero while the pane is empty.
    pub fn active_index(&self) -> usize {
        self.active
    }

    /// The tab on top, if there is one.
    pub fn active(&self) -> Option<&PaneItem> {
        self.items.get(self.active)
    }

    /// The tab at `index`, if there is one.
    pub fn get(&self, index: usize) -> Option<&PaneItem> {
        self.items.get(index)
    }

    /// The scroll handle of this pane's tab strip.
    pub fn scroll_handle(&self) -> &ScrollHandle {
        &self.scroll
    }

    /// Brings the tab at `index` to the top. Answers whether it moved.
    ///
    /// An index that names no tab, and one that is already on top, both change
    /// nothing — which is what lets the caller skip the focus dance around a
    /// click on the active tab.
    pub fn activate(&mut self, index: usize) -> bool {
        if index >= self.items.len() || index == self.active {
            return false;
        }
        self.active = index;
        true
    }

    /// Appends a tab and brings it to the top.
    pub fn push(&mut self, item: PaneItem) {
        self.items.push(item);
        self.active = self.items.len() - 1;
    }

    /// Removes the tab at `index` and hands it back.
    ///
    /// The tab that follows the closed one takes its place, and the one before
    /// it when nothing follows: that is what makes a run of closes walk in one
    /// direction rather than jumping to the end of the strip.
    pub fn close(&mut self, index: usize) -> Option<PaneItem> {
        if index >= self.items.len() {
            return None;
        }
        let item = self.items.remove(index);
        if index < self.active {
            self.active -= 1;
        }
        self.active = self.active.min(self.items.len().saturating_sub(1));
        Some(item)
    }

    /// The index of the tab showing `target`, if one is open here.
    pub fn detail_of(&self, target: &ObjectTarget, cx: &App) -> Option<usize> {
        self.items.iter().position(|item| match item {
            PaneItem::TableDetail(panel) => panel.read(cx).target() == target,
            PaneItem::Query { .. } | PaneItem::Erd(_) => false,
        })
    }

    /// The index of the tab drawing `target`, if one is open here.
    ///
    /// The counterpart of [`Pane::detail_of`], and there for the same reason:
    /// a second diagram of one scope would show exactly what the first one
    /// does, so opening it again is a navigation.
    pub fn erd_of(&self, target: &ErdTarget, cx: &App) -> Option<usize> {
        self.items.iter().position(|item| match item {
            PaneItem::Erd(panel) => panel.read(cx).target() == target,
            PaneItem::TableDetail(_) | PaneItem::Query { .. } => false,
        })
    }
}

/// The identity of one pane, stable for as long as the pane exists.
///
/// Ids are never reused, so a stale id reads as "gone" rather than as some
/// other pane that happened to take the slot.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct PaneId(u64);

impl PaneId {
    /// Mints an id no pane has ever had.
    fn next() -> Self {
        Self(NEXT_PANE_ID.fetch_add(1, Ordering::Relaxed))
    }

    /// The id as a plain integer, for building element ids.
    pub fn as_u64(self) -> u64 {
        self.0
    }
}

/// The identity of one split, stable for as long as the split exists.
///
/// A divider is dragged, not clicked: the view starts a drag on one handle and
/// then receives move events from every enclosing split as well, so it needs a
/// way to tell "this is the divider being dragged" from "this is an ancestor
/// watching the same gesture". Positions cannot answer that — the tree is
/// rewritten whenever a pane opens or closes — so each split carries an id, as
/// process wide and as non-reusable as [`PaneId`] and for the same reasons.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct SplitId(u64);

impl SplitId {
    /// Mints an id no split has ever had.
    fn next() -> Self {
        Self(NEXT_SPLIT_ID.fetch_add(1, Ordering::Relaxed))
    }

    /// The id as a plain integer, for building element ids.
    pub fn as_u64(self) -> u64 {
        self.0
    }
}

/// The direction a split lays its two children out in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Axis {
    /// Side by side, first child on the left, parted by a vertical line.
    Horizontal,
    /// Stacked, first child on top, parted by a horizontal line.
    Vertical,
}

/// One node of a [`PaneTree`].
#[derive(Debug)]
pub enum PaneNode<T> {
    /// A pane: the smallest thing the tree divides its area into.
    Leaf {
        /// Identity of the pane.
        id: PaneId,
        /// What the pane shows.
        payload: T,
    },
    /// Two children sharing this node's area.
    Split {
        /// Identity of the split, for aiming [`PaneTree::set_ratio`] at it.
        id: SplitId,
        /// Direction the children are laid out in.
        axis: Axis,
        /// Fraction of the area the first child gets, in `0.0..=1.0`.
        ratio: f32,
        /// The child drawn first: left of, or above, the divider.
        first: Box<PaneNode<T>>,
        /// The child drawn second: right of, or below, the divider.
        second: Box<PaneNode<T>>,
    },
}

/// The panes of the work area, arranged as a binary tree.
///
/// A tree always holds at least one pane: [`PaneTree::remove`] refuses to take
/// the last one, leaving the caller to decide what an empty work area means.
#[derive(Debug)]
pub struct PaneTree<T> {
    /// The root node.
    ///
    /// Wrapped in an `Option` only because the rewrites below rebuild the tree
    /// by value: each one takes the root out, hands it to a recursive function
    /// and puts the result back before it returns. From the outside the tree
    /// always has a root, which is what [`PaneTree::root`] relies on.
    root: Option<PaneNode<T>>,
}

impl<T> PaneTree<T> {
    /// A tree of one pane showing `payload`.
    pub fn single(payload: T) -> Self {
        Self {
            root: Some(PaneNode::Leaf {
                id: PaneId::next(),
                payload,
            }),
        }
    }

    /// The root node, for rendering.
    pub fn root(&self) -> &PaneNode<T> {
        self.root.as_ref().expect(ROOT_INVARIANT)
    }

    /// Splits the pane `target` in two along `axis`, putting `payload` in the
    /// new pane.
    ///
    /// The existing pane becomes the first child and the new one the second, so
    /// a split always appears below or to the right of the pane it was asked
    /// for. Returns the new pane's id, or `None` when `target` is not in this
    /// tree.
    pub fn split(&mut self, target: PaneId, axis: Axis, payload: T) -> Option<PaneId> {
        let subtree = Self::single(payload);
        let id = subtree.first_leaf().0;
        self.merge_subtree(target, axis, subtree).then_some(id)
    }

    /// Splits the pane `target` along `axis` and puts `subtree` — a whole tree,
    /// splits and all — in the new half.
    ///
    /// This is how a detached layout comes back in: its panes move over
    /// unchanged, keeping both their arrangement and their ids. Returns whether
    /// `target` was found; on `false` nothing changed and `subtree` is dropped.
    pub fn merge_subtree(&mut self, target: PaneId, axis: Axis, subtree: Self) -> bool {
        if !self.contains(target) {
            return false;
        }

        // Carried in an `Option` so that `splice` can move it into place at the
        // single node that matches, without the recursion needing a way to
        // clone it for the branches that do not.
        let mut incoming = subtree.root;
        let root = self.root.take().expect(ROOT_INVARIANT);
        self.root = Some(splice(root, target, axis, &mut incoming));
        debug_assert!(incoming.is_none(), "the target leaf occurs exactly once");
        true
    }

    /// Removes the pane `target` and returns what it was showing.
    ///
    /// The split it sat in collapses: its sibling — a single pane or an entire
    /// subtree — takes the split's place. Removing the last pane of a tree is
    /// refused with `None`, because a work area with no panes would have
    /// nothing to render; the caller decides what to do instead.
    ///
    /// The returned payload is still live, so this doubles as "detach": a
    /// caller may drop it to close the pane, or move it into a tree of its own.
    pub fn remove(&mut self, target: PaneId) -> Option<T> {
        if self.leaf_count() < 2 || !self.contains(target) {
            return None;
        }

        let root = self.root.take().expect(ROOT_INVARIANT);
        let (rebuilt, payload) = take_leaf(root, target);
        self.root = Some(rebuilt.expect("a tree of two or more panes cannot empty"));
        payload
    }

    /// Moves the divider of the split `id`, giving its first child `ratio` of
    /// the area.
    ///
    /// Returns whether the split was found; an unknown id changes nothing, so a
    /// drag whose split closed mid-gesture is simply ignored. Only the `0..=1`
    /// range is enforced here, because how much of a pane must stay visible is
    /// a question about the rendered layout, not about the tree — the view
    /// clamps to its own minimum before calling.
    pub fn set_ratio(&mut self, id: SplitId, ratio: f32) -> bool {
        let Some(split) = find_split_mut(self.root.as_mut().expect(ROOT_INVARIANT), id) else {
            return false;
        };
        *split = ratio.clamp(0., 1.);
        true
    }

    /// What the pane `id` shows, if it is in this tree.
    pub fn get(&self, id: PaneId) -> Option<&T> {
        find(self.root(), id)
    }

    /// What the pane `id` shows, mutably.
    pub fn get_mut(&mut self, id: PaneId) -> Option<&mut T> {
        find_mut(self.root.as_mut().expect(ROOT_INVARIANT), id)
    }

    /// Whether `id` names a pane of this tree.
    pub fn contains(&self, id: PaneId) -> bool {
        self.get(id).is_some()
    }

    /// Every pane, in layout order: first child before second, depth first.
    ///
    /// This is also the order the focus cycle follows, so that
    /// [`PaneTree::next_leaf`] walks the panes the way they are drawn.
    pub fn leaves(&self) -> Vec<(PaneId, &T)> {
        let mut leaves = Vec::new();
        collect(self.root(), &mut leaves);
        leaves
    }

    /// The ids of every pane, in layout order.
    pub fn leaf_ids(&self) -> Vec<PaneId> {
        self.leaves().into_iter().map(|(id, _)| id).collect()
    }

    /// How many panes the tree holds; never zero.
    pub fn leaf_count(&self) -> usize {
        count(self.root())
    }

    /// The first pane in layout order — the top-left one.
    ///
    /// Always present, so callers holding an id that may have gone stale can
    /// fall back to it instead of having nothing to show.
    pub fn first_leaf(&self) -> (PaneId, &T) {
        let mut leaf = self.root();
        while let PaneNode::Split { first, .. } = leaf {
            leaf = first;
        }
        match leaf {
            PaneNode::Leaf { id, payload } => (*id, payload),
            PaneNode::Split { .. } => unreachable!("the loop above ends on a leaf"),
        }
    }

    /// The pane after `from` in layout order, wrapping around at the end.
    ///
    /// `None` when `from` is not in this tree.
    pub fn next_leaf(&self, from: PaneId) -> Option<PaneId> {
        let ids = self.leaf_ids();
        let index = ids.iter().position(|id| *id == from)?;
        ids.get((index + 1) % ids.len()).copied()
    }

    /// The pane before `from` in layout order, wrapping around at the start.
    ///
    /// `None` when `from` is not in this tree.
    pub fn prev_leaf(&self, from: PaneId) -> Option<PaneId> {
        let ids = self.leaf_ids();
        let index = ids.iter().position(|id| *id == from)?;
        ids.get((index + ids.len() - 1) % ids.len()).copied()
    }
}

/// Rebuilds `node` with the leaf `target` split along `axis`, the leaf itself
/// as the first child and `incoming` as the second.
///
/// `incoming` is taken at the one leaf that matches; the caller has already
/// established that such a leaf exists.
fn splice<T>(
    node: PaneNode<T>,
    target: PaneId,
    axis: Axis,
    incoming: &mut Option<PaneNode<T>>,
) -> PaneNode<T> {
    match node {
        PaneNode::Leaf { id, payload } => {
            let leaf = PaneNode::Leaf { id, payload };
            if id != target {
                return leaf;
            }
            let second = incoming
                .take()
                .expect("the target leaf is spliced exactly once");
            PaneNode::Split {
                id: SplitId::next(),
                axis,
                ratio: EVEN,
                first: Box::new(leaf),
                second: Box::new(second),
            }
        }
        // Rebuilt by value, so every field an untouched split already had —
        // its id above all — has to be carried over rather than minted afresh,
        // or a drag in flight would lose track of the divider it holds.
        PaneNode::Split {
            id,
            axis: split_axis,
            ratio,
            first,
            second,
        } => PaneNode::Split {
            id,
            axis: split_axis,
            ratio,
            first: Box::new(splice(*first, target, axis, incoming)),
            second: Box::new(splice(*second, target, axis, incoming)),
        },
    }
}

/// Rebuilds `node` without the leaf `target`, returning what should stand in
/// its place together with the payload that was removed.
///
/// A `None` replacement means the node itself was the removed leaf; the caller
/// one level up answers that by promoting the sibling, which is where the
/// collapse happens.
fn take_leaf<T>(node: PaneNode<T>, target: PaneId) -> (Option<PaneNode<T>>, Option<T>) {
    match node {
        PaneNode::Leaf { id, payload } => {
            if id == target {
                (None, Some(payload))
            } else {
                (Some(PaneNode::Leaf { id, payload }), None)
            }
        }
        // A split that survives keeps its id and its ratio: only the collapsed
        // one goes away, and the divider the user dragged is not it.
        PaneNode::Split {
            id,
            axis,
            ratio,
            first,
            second,
        } => {
            let (rebuilt, payload) = take_leaf(*first, target);
            if let Some(payload) = payload {
                return match rebuilt {
                    Some(first) => (
                        Some(PaneNode::Split {
                            id,
                            axis,
                            ratio,
                            first: Box::new(first),
                            second,
                        }),
                        Some(payload),
                    ),
                    // The first child was the removed leaf, so this split has
                    // one child left and the second takes its place.
                    None => (Some(*second), Some(payload)),
                };
            }

            let first = rebuilt.expect("an untouched subtree survives unchanged");
            let (rebuilt, payload) = take_leaf(*second, target);
            match rebuilt {
                Some(second) => (
                    Some(PaneNode::Split {
                        id,
                        axis,
                        ratio,
                        first: Box::new(first),
                        second: Box::new(second),
                    }),
                    payload,
                ),
                None => (Some(first), payload),
            }
        }
    }
}

/// Appends every leaf of `node` to `leaves`, in layout order.
fn collect<'a, T>(node: &'a PaneNode<T>, leaves: &mut Vec<(PaneId, &'a T)>) {
    match node {
        PaneNode::Leaf { id, payload } => leaves.push((*id, payload)),
        PaneNode::Split { first, second, .. } => {
            collect(first, leaves);
            collect(second, leaves);
        }
    }
}

/// The payload of the leaf `target` inside `node`.
fn find<T>(node: &PaneNode<T>, target: PaneId) -> Option<&T> {
    match node {
        PaneNode::Leaf { id, payload } => (*id == target).then_some(payload),
        PaneNode::Split { first, second, .. } => {
            find(first, target).or_else(|| find(second, target))
        }
    }
}

/// The payload of the leaf `target` inside `node`, mutably.
fn find_mut<T>(node: &mut PaneNode<T>, target: PaneId) -> Option<&mut T> {
    match node {
        PaneNode::Leaf { id, payload } => (*id == target).then_some(payload),
        PaneNode::Split { first, second, .. } => match find_mut(first, target) {
            Some(payload) => Some(payload),
            None => find_mut(second, target),
        },
    }
}

/// The ratio of the split `target` inside `node`, mutably.
fn find_split_mut<T>(node: &mut PaneNode<T>, target: SplitId) -> Option<&mut f32> {
    match node {
        PaneNode::Leaf { .. } => None,
        PaneNode::Split {
            id,
            ratio,
            first,
            second,
            ..
        } => {
            if *id == target {
                return Some(ratio);
            }
            // Spelled out rather than as `or_else`, which would need the first
            // subtree's mutable borrow to still be live while the second one is
            // taken.
            match find_split_mut(first, target) {
                Some(ratio) => Some(ratio),
                None => find_split_mut(second, target),
            }
        }
    }
}

/// How many leaves `node` holds.
fn count<T>(node: &PaneNode<T>) -> usize {
    match node {
        PaneNode::Leaf { .. } => 1,
        PaneNode::Split { first, second, .. } => count(first) + count(second),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The axis and ratio of a node, or `None` when it is a leaf.
    fn split_of<T>(node: &PaneNode<T>) -> Option<(Axis, f32)> {
        match node {
            PaneNode::Leaf { .. } => None,
            PaneNode::Split { axis, ratio, .. } => Some((*axis, *ratio)),
        }
    }

    /// The payloads of a tree, in layout order.
    fn payloads(tree: &PaneTree<u32>) -> Vec<u32> {
        tree.leaves().into_iter().map(|(_, value)| *value).collect()
    }

    /// The id of a node, or `None` when it is a leaf.
    fn split_id<T>(node: &PaneNode<T>) -> Option<SplitId> {
        match node {
            PaneNode::Leaf { .. } => None,
            PaneNode::Split { id, .. } => Some(*id),
        }
    }

    /// The id of the split at the root, which the tests below build on purpose.
    fn root_split<T>(tree: &PaneTree<T>) -> SplitId {
        split_id(tree.root()).expect("the tree was split")
    }

    #[test]
    fn a_fresh_tree_holds_exactly_one_pane() {
        let tree = PaneTree::single(1u32);
        let (id, payload) = tree.first_leaf();

        assert_eq!(tree.leaf_count(), 1);
        assert_eq!(*payload, 1);
        assert_eq!(tree.leaf_ids(), vec![id]);
        assert_eq!(tree.get(id), Some(&1));
        assert!(tree.contains(id));
    }

    #[test]
    fn every_pane_gets_its_own_id() {
        let mut tree = PaneTree::single(1u32);
        let root = tree.first_leaf().0;
        let second = tree.split(root, Axis::Horizontal, 2).expect("root exists");
        let third = tree
            .split(second, Axis::Vertical, 3)
            .expect("second exists");

        assert_ne!(root, second);
        assert_ne!(second, third);
        assert_ne!(root, third);
    }

    #[test]
    fn splitting_puts_the_new_pane_second() {
        let mut tree = PaneTree::single(1u32);
        let root = tree.first_leaf().0;
        let new = tree.split(root, Axis::Horizontal, 2).expect("root exists");

        assert_eq!(tree.leaf_count(), 2);
        assert_eq!(payloads(&tree), vec![1, 2]);
        assert_eq!(split_of(tree.root()), Some((Axis::Horizontal, EVEN)));
        assert_eq!(tree.get(new), Some(&2));
    }

    #[test]
    fn splitting_an_unknown_pane_changes_nothing() {
        let mut tree = PaneTree::single(1u32);
        let stale = PaneTree::single(9u32).first_leaf().0;

        assert_eq!(tree.split(stale, Axis::Vertical, 2), None);
        assert_eq!(tree.leaf_count(), 1);
        assert_eq!(payloads(&tree), vec![1]);
    }

    #[test]
    fn splitting_and_closing_round_trips() {
        let mut tree = PaneTree::single(1u32);
        let root = tree.first_leaf().0;
        let new = tree.split(root, Axis::Vertical, 2).expect("root exists");

        assert_eq!(tree.remove(new), Some(2));
        assert_eq!(tree.leaf_count(), 1);
        // Back to a bare leaf, and the surviving pane kept its identity.
        assert_eq!(split_of(tree.root()), None);
        assert_eq!(tree.first_leaf().0, root);
        assert!(!tree.contains(new));
    }

    #[test]
    fn removing_a_nested_pane_promotes_its_sibling() {
        // 1 | (2 / 3): closing 2 has to leave 1 | 3 with the outer split intact,
        // not a split with a single child.
        let mut tree = PaneTree::single(1u32);
        let one = tree.first_leaf().0;
        let two = tree.split(one, Axis::Horizontal, 2).expect("1 exists");
        let three = tree.split(two, Axis::Vertical, 3).expect("2 exists");

        assert_eq!(tree.leaf_count(), 3);
        assert_eq!(payloads(&tree), vec![1, 2, 3]);

        assert_eq!(tree.remove(two), Some(2));
        assert_eq!(tree.leaf_count(), 2);
        assert_eq!(payloads(&tree), vec![1, 3]);
        // The outer split survived; the inner one collapsed into 3.
        assert_eq!(split_of(tree.root()), Some((Axis::Horizontal, EVEN)));
        assert_eq!(tree.leaf_ids(), vec![one, three]);
    }

    #[test]
    fn removing_a_first_child_promotes_the_second_subtree() {
        // (1 / 2) | 3: closing the whole left half one pane at a time must not
        // strand the right half.
        let mut tree = PaneTree::single(1u32);
        let one = tree.first_leaf().0;
        let three = tree.split(one, Axis::Horizontal, 3).expect("1 exists");
        let two = tree.split(one, Axis::Vertical, 2).expect("1 exists");

        assert_eq!(payloads(&tree), vec![1, 2, 3]);

        assert_eq!(tree.remove(one), Some(1));
        assert_eq!(payloads(&tree), vec![2, 3]);
        assert_eq!(tree.leaf_ids(), vec![two, three]);

        assert_eq!(tree.remove(two), Some(2));
        assert_eq!(tree.leaf_count(), 1);
        assert_eq!(tree.first_leaf().0, three);
    }

    #[test]
    fn the_last_pane_cannot_be_removed() {
        let mut tree = PaneTree::single(1u32);
        let root = tree.first_leaf().0;

        assert_eq!(tree.remove(root), None);
        assert_eq!(tree.leaf_count(), 1);
        assert_eq!(tree.first_leaf().0, root);
    }

    #[test]
    fn removing_an_unknown_pane_changes_nothing() {
        let mut tree = PaneTree::single(1u32);
        let root = tree.first_leaf().0;
        tree.split(root, Axis::Horizontal, 2).expect("root exists");
        let stale = PaneTree::single(9u32).first_leaf().0;

        assert_eq!(tree.remove(stale), None);
        assert_eq!(payloads(&tree), vec![1, 2]);
    }

    #[test]
    fn the_focus_cycle_follows_layout_order_and_wraps() {
        let mut tree = PaneTree::single(1u32);
        let one = tree.first_leaf().0;
        let two = tree.split(one, Axis::Horizontal, 2).expect("1 exists");
        let three = tree.split(two, Axis::Vertical, 3).expect("2 exists");

        assert_eq!(tree.leaf_ids(), vec![one, two, three]);

        assert_eq!(tree.next_leaf(one), Some(two));
        assert_eq!(tree.next_leaf(two), Some(three));
        assert_eq!(tree.next_leaf(three), Some(one));

        assert_eq!(tree.prev_leaf(three), Some(two));
        assert_eq!(tree.prev_leaf(two), Some(one));
        assert_eq!(tree.prev_leaf(one), Some(three));
    }

    #[test]
    fn cycling_a_single_pane_stays_on_it() {
        let tree = PaneTree::single(1u32);
        let root = tree.first_leaf().0;

        assert_eq!(tree.next_leaf(root), Some(root));
        assert_eq!(tree.prev_leaf(root), Some(root));
    }

    #[test]
    fn cycling_from_an_unknown_pane_reports_nothing() {
        let tree = PaneTree::single(1u32);
        let stale = PaneTree::single(9u32).first_leaf().0;

        assert_eq!(tree.next_leaf(stale), None);
        assert_eq!(tree.prev_leaf(stale), None);
    }

    #[test]
    fn merging_a_subtree_keeps_its_layout_and_its_ids() {
        let mut target = PaneTree::single(1u32);
        let one = target.first_leaf().0;

        let mut source = PaneTree::single(2u32);
        let two = source.first_leaf().0;
        let three = source.split(two, Axis::Vertical, 3).expect("2 exists");

        assert!(target.merge_subtree(one, Axis::Horizontal, source));

        assert_eq!(target.leaf_count(), 3);
        assert_eq!(payloads(&target), vec![1, 2, 3]);
        // The ids the source layout was tracking still name the same panes.
        assert_eq!(target.get(two), Some(&2));
        assert_eq!(target.get(three), Some(&3));
        assert_eq!(target.leaf_ids(), vec![one, two, three]);
        // The merge axis belongs to the new outer split; the source's own split
        // is untouched.
        assert_eq!(split_of(target.root()), Some((Axis::Horizontal, EVEN)));
    }

    #[test]
    fn merging_into_an_unknown_pane_is_refused() {
        let mut target = PaneTree::single(1u32);
        let stale = PaneTree::single(9u32).first_leaf().0;

        assert!(!target.merge_subtree(stale, Axis::Vertical, PaneTree::single(2u32)));
        assert_eq!(payloads(&target), vec![1]);
    }

    #[test]
    fn merging_into_a_nested_pane_splits_only_that_pane() {
        let mut target = PaneTree::single(1u32);
        let one = target.first_leaf().0;
        let two = target.split(one, Axis::Horizontal, 2).expect("1 exists");

        assert!(target.merge_subtree(two, Axis::Vertical, PaneTree::single(3u32)));

        assert_eq!(payloads(&target), vec![1, 2, 3]);
        // The outer split kept its own axis.
        assert_eq!(split_of(target.root()), Some((Axis::Horizontal, EVEN)));
    }

    #[test]
    fn a_removed_pane_can_be_replanted_in_a_tree_of_its_own() {
        // How detaching a pane works: remove hands the payload back, so it
        // moves rather than being rebuilt.
        let mut tree = PaneTree::single(1u32);
        let one = tree.first_leaf().0;
        let two = tree.split(one, Axis::Horizontal, 2).expect("1 exists");

        let detached = tree.remove(two).expect("two panes, so this is allowed");
        let broken_out = PaneTree::single(detached);

        assert_eq!(payloads(&tree), vec![1]);
        assert_eq!(payloads(&broken_out), vec![2]);
        // A fresh leaf, so the old id is gone for good.
        assert_ne!(broken_out.first_leaf().0, two);
    }

    #[test]
    fn every_split_gets_its_own_id() {
        let mut tree = PaneTree::single(1u32);
        let one = tree.first_leaf().0;
        let two = tree.split(one, Axis::Horizontal, 2).expect("1 exists");
        let outer = root_split(&tree);
        tree.split(two, Axis::Vertical, 3).expect("2 exists");

        let inner = match tree.root() {
            PaneNode::Split { second, .. } => split_id(second).expect("2 was split in two"),
            PaneNode::Leaf { .. } => panic!("the tree holds three panes"),
        };
        assert_ne!(outer, inner);
        // The outer split was rebuilt around the inner one and kept its id.
        assert_eq!(root_split(&tree), outer);
    }

    #[test]
    fn a_divider_moves_where_it_is_put() {
        let mut tree = PaneTree::single(1u32);
        let one = tree.first_leaf().0;
        tree.split(one, Axis::Horizontal, 2).expect("1 exists");
        let split = root_split(&tree);

        assert!(tree.set_ratio(split, 0.3));
        assert_eq!(split_of(tree.root()), Some((Axis::Horizontal, 0.3)));
    }

    #[test]
    fn moving_an_unknown_divider_changes_nothing() {
        let mut tree = PaneTree::single(1u32);
        let one = tree.first_leaf().0;
        tree.split(one, Axis::Vertical, 2).expect("1 exists");

        // A split of another tree, standing in for one that closed mid-drag.
        let mut other = PaneTree::single(9u32);
        let nine = other.first_leaf().0;
        other.split(nine, Axis::Vertical, 8).expect("9 exists");
        let stale = root_split(&other);

        assert!(!tree.set_ratio(stale, 0.3));
        assert_eq!(split_of(tree.root()), Some((Axis::Vertical, EVEN)));
    }

    #[test]
    fn a_divider_cannot_be_pushed_past_the_edges() {
        let mut tree = PaneTree::single(1u32);
        let one = tree.first_leaf().0;
        tree.split(one, Axis::Horizontal, 2).expect("1 exists");
        let split = root_split(&tree);

        assert!(tree.set_ratio(split, -4.));
        assert_eq!(split_of(tree.root()), Some((Axis::Horizontal, 0.)));

        assert!(tree.set_ratio(split, 4.));
        assert_eq!(split_of(tree.root()), Some((Axis::Horizontal, 1.)));
    }

    #[test]
    fn a_nested_divider_moves_without_disturbing_its_parent() {
        // 1 | (2 / 3): dragging the inner divider must leave the outer one be.
        let mut tree = PaneTree::single(1u32);
        let one = tree.first_leaf().0;
        let two = tree.split(one, Axis::Horizontal, 2).expect("1 exists");
        let outer = root_split(&tree);
        tree.split(two, Axis::Vertical, 3).expect("2 exists");
        let inner = match tree.root() {
            PaneNode::Split { second, .. } => split_id(second).expect("2 was split in two"),
            PaneNode::Leaf { .. } => panic!("the tree holds three panes"),
        };

        assert!(tree.set_ratio(inner, 0.25));

        assert_eq!(split_of(tree.root()), Some((Axis::Horizontal, EVEN)));
        match tree.root() {
            PaneNode::Split { second, .. } => {
                assert_eq!(split_of(second), Some((Axis::Vertical, 0.25)));
            }
            PaneNode::Leaf { .. } => panic!("the tree holds three panes"),
        }
        assert_eq!(root_split(&tree), outer);
    }

    #[test]
    fn a_collapsing_split_leaves_the_surviving_one_untouched() {
        // 1 | (2 / 3) with both dividers moved: closing 3 collapses the inner
        // split, and the outer one has to come through with its id and its
        // ratio intact so a divider drag survives a pane closing.
        let mut tree = PaneTree::single(1u32);
        let one = tree.first_leaf().0;
        let two = tree.split(one, Axis::Horizontal, 2).expect("1 exists");
        let outer = root_split(&tree);
        let three = tree.split(two, Axis::Vertical, 3).expect("2 exists");
        assert!(tree.set_ratio(outer, 0.3));

        assert_eq!(tree.remove(three), Some(3));

        assert_eq!(payloads(&tree), vec![1, 2]);
        assert_eq!(root_split(&tree), outer);
        assert_eq!(split_of(tree.root()), Some((Axis::Horizontal, 0.3)));
        assert!(tree.set_ratio(outer, 0.6));
        assert_eq!(split_of(tree.root()), Some((Axis::Horizontal, 0.6)));
    }

    #[test]
    fn a_merged_subtree_brings_its_dividers_with_it() {
        let mut target = PaneTree::single(1u32);
        let one = target.first_leaf().0;

        let mut source = PaneTree::single(2u32);
        let two = source.first_leaf().0;
        source.split(two, Axis::Vertical, 3).expect("2 exists");
        let moved = root_split(&source);
        assert!(source.set_ratio(moved, 0.75));

        assert!(target.merge_subtree(one, Axis::Horizontal, source));

        // The subtree's divider kept both its position and its id, so the
        // handle rendered for it still drags the same split.
        match target.root() {
            PaneNode::Split { second, .. } => {
                assert_eq!(split_id(second), Some(moved));
                assert_eq!(split_of(second), Some((Axis::Vertical, 0.75)));
            }
            PaneNode::Leaf { .. } => panic!("the merge produced a split"),
        }
        assert_ne!(root_split(&target), moved);
        assert!(target.set_ratio(moved, 0.4));
    }

    #[test]
    fn a_payload_can_be_mutated_in_place() {
        let mut tree = PaneTree::single(1u32);
        let one = tree.first_leaf().0;
        let two = tree.split(one, Axis::Vertical, 2).expect("1 exists");

        *tree.get_mut(two).expect("two exists") = 20;

        assert_eq!(payloads(&tree), vec![1, 20]);
        assert_eq!(tree.get_mut(PaneTree::single(9u32).first_leaf().0), None);
    }

    // The tests above pin the mechanics down with a payload that can be told
    // apart at a glance; the ones below pin down the instantiation the shell
    // actually builds, which is the part a new variant of `PaneItem` could
    // break without any of the above noticing.

    #[test]
    fn the_shell_layout_splits_and_collapses_like_any_other() {
        let mut tree = PaneTree::single(Pane::new());
        let one = tree.first_leaf().0;
        let two = tree
            .split(one, Axis::Horizontal, Pane::new())
            .expect("1 exists");
        let three = tree
            .split(two, Axis::Vertical, Pane::new())
            .expect("2 exists");

        assert_eq!(tree.leaf_count(), 3);
        assert_eq!(tree.leaf_ids(), vec![one, two, three]);
        assert!(tree.get(three).expect("three exists").is_empty());

        assert!(tree.remove(three).is_some());
        assert!(tree.remove(two).is_some());
        // Down to the one pane the shell always keeps, which cannot be closed.
        assert_eq!(tree.leaf_count(), 1);
        assert!(tree.remove(one).is_none());
    }

    /// A tab is a view, so the ordering rules are asserted in the shell's own
    /// tests, where a session exists to build one from. What can be said here is
    /// that an empty pane answers every question without panicking: it is the
    /// state a freshly split pane starts in and the one it returns to when its
    /// last tab is closed.
    #[test]
    fn an_empty_pane_has_no_active_tab_and_nothing_to_close() {
        let mut pane = Pane::new();
        assert!(pane.is_empty());
        assert!(pane.active().is_none());
        // Nothing to close, and nothing to activate.
        assert!(pane.close(0).is_none());
        assert!(!pane.activate(0));
    }
}
