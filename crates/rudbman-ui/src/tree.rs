//! A virtualised tree whose branches arrive one round trip at a time.
//!
//! The widget knows nothing of what it is showing. It owns the shape of the
//! tree — which nodes are open, which one is selected, where the list is
//! scrolled — while the host owns the nodes themselves and hands them over
//! through [`TreeSource`], keyed by an id the host invents. That split is what
//! keeps a database explorer, a file listing and an object browser the same
//! widget.
//!
//! ## Why a flattened list
//!
//! The tree is drawn as a flat list of the rows that are currently visible,
//! rebuilt whenever the shape changes, and never as nested elements. gpui's
//! [`uniform_list`] only lays out the rows the viewport can see, and it can only
//! do that if the rows are addressable by index — so an open schema with five
//! thousand tables costs the same to draw as an empty one. The children of a
//! collapsed node are not in the list at all, which is also what makes "how many
//! rows are there" and "what does the row below this one hold" answerable
//! without walking anything: arrow keys move by one index, and the subtree of a
//! row is the run of rows after it that are deeper than it is.
//!
//! ## Why loading is in the trait
//!
//! Every node a database explorer opens is a server round trip, so "I do not
//! have these children yet" is not an error state, it is the ordinary one.
//! [`ChildState`] therefore has [`ChildState::NotLoaded`] and
//! [`ChildState::Loading`] beside [`ChildState::Loaded`], and the tree reacts to
//! them rather than asking the host to pretend: an open node with nothing under
//! it yet draws a placeholder row, and the tree emits
//! [`TreeEvent::LoadChildren`] so that the host can go and fetch them. Nothing
//! here ever blocks, and nothing here spawns a task either — the host owns the
//! connection, so the host owns the fetch, and when the answer lands it drops
//! the children into its source and notifies. The tree redraws with them.
//!
//! A request is remembered until the source answers with anything other than
//! [`ChildState::NotLoaded`], so a node is asked for once however many times it
//! is redrawn, and a node whose children the host later drops back to
//! `NotLoaded` is asked again.
//!
//! ## What the host draws
//!
//! The tree draws the indent, the disclosure arrow, the selection and the hover;
//! [`TreeSource::render_row`] draws everything inside the row — icon, label,
//! badges — because only the host knows what a node *is*. It is handed a
//! [`TreeRowInfo`] so that a label can follow the row's state without keeping a
//! copy of it.

use std::collections::HashSet;
use std::hash::Hash;

use gpui::{
    AnyElement, App, ClickEvent, Context, DragMoveEvent, ElementId, EventEmitter, FocusHandle,
    Focusable, KeyBinding, MouseButton, MouseUpEvent, ScrollHandle, ScrollStrategy,
    UniformListScrollHandle, Window, actions, div, prelude::*, px, uniform_list,
};

use crate::scrollbar::{
    DraggedThumb, Scrollbar, ScrollbarAxis, ScrollbarState, hide_later, scroll_to, scrolled,
};
use crate::theme::theme;

actions!(
    rudbman_tree,
    [
        /// Move the selection to the row above.
        SelectPrev,
        /// Move the selection to the row below.
        SelectNext,
        /// Open the selected node, or step into it when it is already open.
        Expand,
        /// Close the selected node, or step out to its parent when it is
        /// already closed.
        Collapse,
        /// Activate the selected node, which is what a double click does.
        Activate,
        /// Move the selection to the first row.
        SelectFirst,
        /// Move the selection to the last row.
        SelectLast,
    ]
);

/// Key context that [`init`] binds the keys above to.
const KEY_CONTEXT: &str = "TreeView";

/// Height of one row, and therefore the unit [`uniform_list`] measures in.
const ROW_HEIGHT: f32 = 24.;

/// How much further in each level of depth sits.
const INDENT: f32 = 14.;

/// Width of the box the disclosure arrow is drawn in.
///
/// Reserved on leaf rows too, so that labels line up down a level instead of
/// stepping sideways depending on whether a node happens to have children.
const ARROW_WIDTH: f32 = 16.;

/// Padding at both ends of a row, before the indent is added.
const ROW_PADDING: f32 = 4.;

/// Arrow of a node that is closed.
const ARROW_CLOSED: &str = "\u{25b8}";

/// Arrow of a node that is open.
const ARROW_OPEN: &str = "\u{25be}";

/// What the placeholder row draws while children are on their way.
///
/// A glyph rather than a word: this layer has no translations, and "Loading…"
/// in English under a Korean tree would be worse than no text at all. A host
/// with a string of its own overrides [`TreeSource::render_loading`].
const LOADING_GLYPH: &str = "\u{2026}";

/// How deep the tree will follow a source before giving up.
///
/// Only reachable by a source that answers `children` with an ancestor of the
/// node it was asked about; the cap is what keeps that a wrong drawing rather
/// than a blown stack.
const MAX_DEPTH: usize = 64;

/// Registers the key bindings every [`TreeView`] relies on.
///
/// Called by [`crate::init`]; scoped to the `TreeView` key context, so the
/// arrows keep meaning what they meant everywhere else in the app.
pub fn init(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("up", SelectPrev, Some(KEY_CONTEXT)),
        KeyBinding::new("down", SelectNext, Some(KEY_CONTEXT)),
        KeyBinding::new("right", Expand, Some(KEY_CONTEXT)),
        KeyBinding::new("left", Collapse, Some(KEY_CONTEXT)),
        KeyBinding::new("enter", Activate, Some(KEY_CONTEXT)),
        KeyBinding::new("space", Activate, Some(KEY_CONTEXT)),
        KeyBinding::new("home", SelectFirst, Some(KEY_CONTEXT)),
        KeyBinding::new("end", SelectLast, Some(KEY_CONTEXT)),
    ]);
}

/// What a source knows about the children of one node.
///
/// The two middle variants are the point of the type: a tree over a network is
/// mostly made of nodes whose children nobody has fetched yet.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ChildState<Id> {
    /// The children, in the order they should be drawn. May be empty, which is
    /// a node that turned out to have nothing under it.
    Loaded(Vec<Id>),
    /// A fetch is in flight. The tree draws a placeholder row and asks for
    /// nothing.
    Loading,
    /// Nobody has asked yet. The tree draws a placeholder row and emits
    /// [`TreeEvent::LoadChildren`].
    NotLoaded,
    /// There can be no children — a table, a column, a file. The row gets no
    /// disclosure arrow and cannot be opened.
    Leaf,
}

/// The state of the row a source is drawing into.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TreeRowInfo {
    /// How deep the row sits, counting the outermost level as zero.
    pub depth: usize,
    /// Whether the node is open.
    pub expanded: bool,
    /// Whether the node is the selected one.
    pub selected: bool,
    /// Whether the row carries a disclosure arrow.
    pub has_children: bool,
}

/// One line of the flattened list.
///
/// Public because it is the whole of what the tree is showing: a host — or a
/// test — that wants to know what is on screen reads [`TreeView::rows`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TreeRow<Id> {
    /// A node of the source.
    Node {
        /// The node's id, as the source named it.
        id: Id,
        /// How deep it sits, counting the outermost level as zero.
        depth: usize,
        /// Whether it can be opened, from [`TreeSource::has_children`].
        has_children: bool,
    },
    /// The placeholder under an open node whose children have not arrived.
    ///
    /// Not selectable, and skipped by the arrow keys: there is nothing there to
    /// act on yet.
    Loading {
        /// Where the placeholder sits, which is one level in from the node
        /// whose children are awaited.
        depth: usize,
    },
}

impl<Id> TreeRow<Id> {
    /// The node this row draws, or `None` for a placeholder.
    pub fn id(&self) -> Option<&Id> {
        match self {
            TreeRow::Node { id, .. } => Some(id),
            TreeRow::Loading { .. } => None,
        }
    }

    /// How deep the row sits.
    pub fn depth(&self) -> usize {
        match self {
            TreeRow::Node { depth, .. } | TreeRow::Loading { depth } => *depth,
        }
    }
}

/// What the tree tells its host about.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TreeEvent<Id> {
    /// The children of a node — or of the root, for `None` — are wanted and
    /// nobody has fetched them.
    ///
    /// Fired once per node until the source answers with something other than
    /// [`ChildState::NotLoaded`]. The host fetches, drops the result into its
    /// source and calls [`TreeView::refresh`] (or mutates through
    /// [`TreeView::source_mut`], which refreshes for it).
    LoadChildren(Option<Id>),
    /// A node was activated: `Enter`, `Space`, or a double click.
    Activated(Id),
    /// The selection moved.
    SelectionChanged(Option<Id>),
}

/// Where a [`TreeView`] gets its nodes and how their rows are drawn.
///
/// Implemented on whatever the host already keeps its nodes in; the tree owns
/// the value and hands it back through [`TreeView::source_mut`], so there is one
/// copy of the data rather than two that can disagree.
pub trait TreeSource: 'static {
    /// How the host names a node.
    ///
    /// Everything the tree remembers — which nodes are open, which is selected —
    /// is keyed by this, which is what lets the host throw its nodes away and
    /// fetch them again without the tree closing up. Ids therefore have to be
    /// stable across a reload: a path or a qualified name, not a row number.
    type Id: Clone + Eq + Hash + 'static;

    /// The children of `parent`, or the outermost level for `None`.
    ///
    /// Called during a rebuild, for the root and for every *open* node, so a
    /// closed subtree costs nothing. It must not block: an implementation that
    /// has to go and fetch returns [`ChildState::NotLoaded`] and waits to be
    /// asked.
    fn children(&self, parent: Option<&Self::Id>) -> ChildState<Self::Id>;

    /// Whether `id` gets a disclosure arrow.
    ///
    /// Asked of every visible row, including closed ones, so a source whose
    /// [`TreeSource::children`] is expensive — one that allocates a large
    /// vector, say — should answer this from something cheaper. The default
    /// derives it, which is right for a source that already holds its children
    /// in memory.
    fn has_children(&self, id: &Self::Id) -> bool {
        !matches!(self.children(Some(id)), ChildState::Leaf)
    }

    /// Draws the inside of a row: icon, label, badges, whatever the node is.
    ///
    /// The tree has already drawn the indent, the arrow and the background, and
    /// laid the row out as a centred flex row; this fills the rest of it.
    fn render_row(
        &self,
        id: &Self::Id,
        info: TreeRowInfo,
        window: &mut Window,
        cx: &mut App,
    ) -> AnyElement;

    /// Draws the inside of the placeholder row under a node whose children are
    /// still coming.
    ///
    /// The default is a muted ellipsis. Override it to say so in the user's
    /// language, or to spin something.
    fn render_loading(&self, _window: &mut Window, cx: &mut App) -> AnyElement {
        div()
            .text_color(theme(cx).text_muted)
            .child(LOADING_GLYPH)
            .into_any_element()
    }
}

/// A tree of rows the host supplies, drawn one viewport at a time.
///
/// Created as an entity and rendered as a child element, like
/// [`TextInput`](crate::TextInput):
///
/// ```ignore
/// let tree = cx.new(|cx| TreeView::new(Explorer::default(), cx));
/// cx.subscribe(&tree, |view, tree, event, cx| match event {
///     TreeEvent::LoadChildren(parent) => view.fetch(parent.clone(), cx),
///     TreeEvent::Activated(id) => view.open(id, cx),
///     TreeEvent::SelectionChanged(_) => {}
/// })
/// .detach();
/// ```
///
/// The keys are `Up`/`Down` to move, `Right` to open a node or step into it,
/// `Left` to close it or step out to its parent, `Enter` and `Space` to
/// activate, `Home` and `End` for the ends of the list. A click selects, a
/// double click activates, and a click on the arrow opens or closes without
/// disturbing the selection.
pub struct TreeView<S: TreeSource> {
    source: S,
    focus_handle: FocusHandle,
    /// Which nodes are open, keyed by id so that a reload cannot close them.
    ///
    /// Holds ids the source may no longer know about — a node that comes back
    /// after a reload comes back open, which is the point.
    expanded: HashSet<S::Id>,
    /// Which nodes have been asked for and not yet answered.
    requested: HashSet<Option<S::Id>>,
    /// The selected node, which may be one no visible row carries; see
    /// [`TreeView::selected_index`].
    selected: Option<S::Id>,
    rows: Vec<TreeRow<S::Id>>,
    /// Whether [`TreeView::rows`] still describes the source.
    dirty: bool,
    scroll: UniformListScrollHandle,
    bar: ScrollbarState,
    /// Id of the overlay bar, made unique per entity so that two trees in one
    /// window do not answer each other's drags.
    bar_id: ElementId,
}

impl<S: TreeSource> TreeView<S> {
    /// A tree over `source`, with nothing open and nothing selected.
    ///
    /// The first draw asks the source for its outermost level, and emits
    /// [`TreeEvent::LoadChildren`] with `None` if it has not been fetched — so a
    /// host that subscribes right after building the tree still catches the
    /// request for the root.
    pub fn new(source: S, cx: &mut Context<Self>) -> Self {
        Self {
            source,
            focus_handle: cx.focus_handle(),
            expanded: HashSet::new(),
            requested: HashSet::new(),
            selected: None,
            rows: Vec::new(),
            dirty: true,
            scroll: UniformListScrollHandle::new(),
            bar: ScrollbarState::new(),
            bar_id: ElementId::from(("rudbman-tree-scrollbar", cx.entity_id())),
        }
    }

    /// Places the tree at `index` in the window's tab order.
    ///
    /// Trees without one stay out of the tab ring, as fields do.
    pub fn tab_index(mut self, index: isize) -> Self {
        self.focus_handle = self.focus_handle.clone().tab_index(index).tab_stop(true);
        self
    }

    /// The source, to read.
    pub fn source(&self) -> &S {
        &self.source
    }

    /// The source, to change — dropping fetched children in, most of the time.
    ///
    /// Rebuilds the row list on the next draw, so the caller has nothing to
    /// remember. A host that keeps its nodes somewhere else entirely, and whose
    /// source only reads them, has to call [`TreeView::refresh`] itself.
    pub fn source_mut(&mut self, cx: &mut Context<Self>) -> &mut S {
        self.dirty = true;
        cx.notify();
        &mut self.source
    }

    /// Rereads the source.
    ///
    /// For changes the tree cannot have seen: data that lives outside the
    /// source, or was written into it without going through
    /// [`TreeView::source_mut`].
    pub fn refresh(&mut self, cx: &mut Context<Self>) {
        self.dirty = true;
        cx.notify();
    }

    /// The rows on screen, outermost first, as of the last draw or keystroke.
    pub fn rows(&self) -> &[TreeRow<S::Id>] {
        &self.rows
    }

    /// Whether `id` is open.
    pub fn is_expanded(&self, id: &S::Id) -> bool {
        self.expanded.contains(id)
    }

    /// The selected node, whether or not a row is currently carrying it.
    pub fn selected(&self) -> Option<&S::Id> {
        self.selected.as_ref()
    }

    /// Opens `id`, asking the host for its children if they are missing.
    ///
    /// A row that is on screen and is a leaf cannot be opened, and this does
    /// nothing for it. A node no row carries *is* opened, and stays open until
    /// it is closed again: that is how a host restores a whole path at once —
    /// open the server, the catalogue and the schema in one go, and each level
    /// is already open by the time the one above it arrives.
    pub fn expand(&mut self, id: &S::Id, cx: &mut Context<Self>) {
        self.ensure_rows(cx);
        if self.expanded.contains(id) {
            return;
        }
        let leaf = self.index_of(id).is_some_and(|ix| {
            matches!(
                self.rows[ix],
                TreeRow::Node {
                    has_children: false,
                    ..
                }
            )
        });
        if leaf {
            return;
        }
        self.expanded.insert(id.clone());
        self.dirty = true;
        self.ensure_rows(cx);
        cx.notify();
    }

    /// Closes `id`.
    ///
    /// A selection that was inside the subtree comes up to `id` rather than
    /// disappearing with it: the highlight stays where the user can see it, on
    /// the node that swallowed what they had picked.
    pub fn collapse(&mut self, id: &S::Id, cx: &mut Context<Self>) {
        self.ensure_rows(cx);
        if !self.expanded.remove(id) {
            return;
        }
        if let Some(ix) = self.index_of(id)
            && self.selection_is_under(ix)
        {
            self.set_selection(Some(id.clone()), cx);
        }
        self.dirty = true;
        self.ensure_rows(cx);
        cx.notify();
    }

    /// Opens `id` if it is closed and closes it if it is open.
    pub fn toggle(&mut self, id: &S::Id, cx: &mut Context<Self>) {
        if self.expanded.contains(id) {
            self.collapse(id, cx);
        } else {
            self.expand(id, cx);
        }
    }

    /// Moves the selection, scrolling the row into view when it has one.
    ///
    /// An id no row carries is remembered rather than refused, for the same
    /// reason the open set is: the node may be on its way back.
    pub fn set_selected(&mut self, id: Option<S::Id>, cx: &mut Context<Self>) {
        self.ensure_rows(cx);
        self.set_selection(id, cx);
        if let Some(ix) = self.selected_index() {
            self.scroll.scroll_to_item(ix, ScrollStrategy::Top);
        }
    }

    /// Where the selected node is in [`TreeView::rows`], or `None` when no row
    /// carries it — because it is inside something closed, or because the host
    /// has reloaded it away.
    pub fn selected_index(&self) -> Option<usize> {
        let selected = self.selected.as_ref()?;
        self.index_of(selected)
    }

    /// Rebuilds the row list when the source may have moved on.
    fn ensure_rows(&mut self, cx: &mut Context<Self>) {
        if self.dirty {
            self.rebuild(cx);
        }
    }

    /// Walks the source and rewrites the row list.
    ///
    /// Requests are collected during the walk and emitted afterwards, so that a
    /// host which answers one synchronously cannot be reading a half-built list.
    fn rebuild(&mut self, cx: &mut Context<Self>) {
        let mut rows = Vec::new();
        let mut asked = Vec::new();
        let mut answered = Vec::new();
        self.walk(None, 0, &mut rows, &mut asked, &mut answered);

        self.rows = rows;
        self.dirty = false;
        for parent in answered {
            self.requested.remove(&parent);
        }
        for parent in asked {
            if self.requested.insert(parent.clone()) {
                cx.emit(TreeEvent::LoadChildren(parent));
            }
        }
    }

    /// Appends the visible rows under `parent` to `rows`.
    fn walk(
        &self,
        parent: Option<&S::Id>,
        depth: usize,
        rows: &mut Vec<TreeRow<S::Id>>,
        asked: &mut Vec<Option<S::Id>>,
        answered: &mut Vec<Option<S::Id>>,
    ) {
        match self.source.children(parent) {
            ChildState::Leaf => {}
            ChildState::NotLoaded => {
                asked.push(parent.cloned());
                rows.push(TreeRow::Loading { depth });
            }
            ChildState::Loading => {
                answered.push(parent.cloned());
                rows.push(TreeRow::Loading { depth });
            }
            ChildState::Loaded(ids) => {
                answered.push(parent.cloned());
                for id in ids {
                    let has_children = self.source.has_children(&id);
                    let open = has_children && self.expanded.contains(&id);
                    rows.push(TreeRow::Node {
                        id: id.clone(),
                        depth,
                        has_children,
                    });
                    if open && depth + 1 < MAX_DEPTH {
                        self.walk(Some(&id), depth + 1, rows, asked, answered);
                    }
                }
            }
        }
    }

    /// Where `id` sits in the row list.
    fn index_of(&self, id: &S::Id) -> Option<usize> {
        self.rows.iter().position(|row| row.id() == Some(id))
    }

    /// The first row after `ix` that is not inside it.
    fn subtree_end(&self, ix: usize) -> usize {
        let depth = self.rows[ix].depth();
        self.rows[ix + 1..]
            .iter()
            .position(|row| row.depth() <= depth)
            .map_or(self.rows.len(), |offset| ix + 1 + offset)
    }

    /// Whether the selected row is inside the subtree of `ix`.
    fn selection_is_under(&self, ix: usize) -> bool {
        self.selected_index()
            .is_some_and(|selected| selected > ix && selected < self.subtree_end(ix))
    }

    /// The row holding the node `ix` hangs from.
    fn parent_of(&self, ix: usize) -> Option<usize> {
        let depth = self.rows[ix].depth();
        if depth == 0 {
            return None;
        }
        self.rows[..ix]
            .iter()
            .rposition(|row| row.depth() < depth && row.id().is_some())
    }

    /// The nearest selectable row at or after `from`, walking by `step`.
    ///
    /// Placeholder rows are not selectable, so they are stepped over rather than
    /// landed on.
    fn selectable(&self, from: usize, step: isize) -> Option<usize> {
        let mut ix = from as isize;
        while ix >= 0 && (ix as usize) < self.rows.len() {
            if self.rows[ix as usize].id().is_some() {
                return Some(ix as usize);
            }
            ix += step;
        }
        None
    }

    /// Selects the node on row `ix` and brings it into view.
    fn select_row(&mut self, ix: usize, cx: &mut Context<Self>) {
        let Some(id) = self.rows[ix].id().cloned() else {
            return;
        };
        self.set_selection(Some(id), cx);
        self.scroll.scroll_to_item(ix, ScrollStrategy::Top);
    }

    /// Records a new selection, announcing it only when it really changed.
    fn set_selection(&mut self, id: Option<S::Id>, cx: &mut Context<Self>) {
        if self.selected == id {
            return;
        }
        self.selected = id.clone();
        cx.emit(TreeEvent::SelectionChanged(id));
        cx.notify();
    }

    fn select_prev(&mut self, _: &SelectPrev, _: &mut Window, cx: &mut Context<Self>) {
        self.ensure_rows(cx);
        let next = match self.selected_index() {
            Some(0) => None,
            Some(ix) => self.selectable(ix - 1, -1),
            None => self.selectable(self.rows.len().saturating_sub(1), -1),
        };
        if let Some(ix) = next {
            self.select_row(ix, cx);
        }
    }

    fn select_next(&mut self, _: &SelectNext, _: &mut Window, cx: &mut Context<Self>) {
        self.ensure_rows(cx);
        let next = match self.selected_index() {
            Some(ix) => self.selectable(ix + 1, 1),
            None => self.selectable(0, 1),
        };
        if let Some(ix) = next {
            self.select_row(ix, cx);
        }
    }

    fn select_first(&mut self, _: &SelectFirst, _: &mut Window, cx: &mut Context<Self>) {
        self.ensure_rows(cx);
        if let Some(ix) = self.selectable(0, 1) {
            self.select_row(ix, cx);
        }
    }

    fn select_last(&mut self, _: &SelectLast, _: &mut Window, cx: &mut Context<Self>) {
        self.ensure_rows(cx);
        if let Some(ix) = self.selectable(self.rows.len().saturating_sub(1), -1) {
            self.select_row(ix, cx);
        }
    }

    /// `Right`: opens the selected node, and steps into an already open one.
    ///
    /// A leaf has nothing to open and nothing under it, so the key does nothing
    /// at all there rather than moving the selection somewhere surprising.
    fn expand_selected(&mut self, _: &Expand, _: &mut Window, cx: &mut Context<Self>) {
        self.ensure_rows(cx);
        let Some(ix) = self.selected_index() else {
            return;
        };
        let TreeRow::Node {
            id, has_children, ..
        } = self.rows[ix].clone()
        else {
            return;
        };
        if !has_children {
            return;
        }
        if self.expanded.contains(&id) {
            if let Some(child) = self
                .selectable(ix + 1, 1)
                .filter(|child| *child < self.subtree_end(ix))
            {
                self.select_row(child, cx);
            }
        } else {
            self.expand(&id, cx);
        }
    }

    /// `Left`: closes the selected node, and steps out of an already closed one.
    fn collapse_selected(&mut self, _: &Collapse, _: &mut Window, cx: &mut Context<Self>) {
        self.ensure_rows(cx);
        let Some(ix) = self.selected_index() else {
            return;
        };
        let TreeRow::Node { id, .. } = self.rows[ix].clone() else {
            return;
        };
        if self.expanded.contains(&id) {
            self.collapse(&id, cx);
        } else if let Some(parent) = self.parent_of(ix) {
            self.select_row(parent, cx);
        }
    }

    fn activate_selected(&mut self, _: &Activate, _: &mut Window, cx: &mut Context<Self>) {
        self.ensure_rows(cx);
        if let Some(id) = self.selected.clone()
            && self.index_of(&id).is_some()
        {
            cx.emit(TreeEvent::Activated(id));
        }
    }

    /// The scroll container behind the list, which is what the bar measures.
    fn base_handle(&self) -> ScrollHandle {
        self.scroll.0.borrow().base_handle.clone()
    }

    /// The overlay bar as it stands this frame.
    fn scrollbar(&self) -> Scrollbar {
        Scrollbar::for_handle(
            self.bar_id.clone(),
            ScrollbarAxis::Vertical,
            &self.base_handle(),
        )
        .fade(self.bar.fade())
    }

    /// Lets go of the thumb and starts the clock that takes the bar down.
    fn release_thumb(&mut self, cx: &mut Context<Self>) {
        if let Some(epoch) = self.bar.release() {
            hide_later(epoch, cx, |tree: &mut Self| Some(&mut tree.bar));
            cx.notify();
        }
    }

    /// Draws row `ix`: the indent, the arrow and the background are the tree's,
    /// everything inside them is the source's.
    fn render_row(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let theme = theme(cx);
        let row = self.rows[ix].clone();
        let depth = row.depth();
        let indent = px(ROW_PADDING + depth as f32 * INDENT);

        let TreeRow::Node {
            id, has_children, ..
        } = row
        else {
            return div()
                .flex()
                .flex_row()
                .items_center()
                .h(px(ROW_HEIGHT))
                .w_full()
                .pl(indent)
                .pr(px(ROW_PADDING))
                .child(div().flex_none().w(px(ARROW_WIDTH)))
                .child(self.source.render_loading(window, cx))
                .into_any_element();
        };

        let expanded = self.expanded.contains(&id);
        let selected = self.selected.as_ref() == Some(&id);
        let info = TreeRowInfo {
            depth,
            expanded,
            selected,
            has_children,
        };
        let content = self.source.render_row(&id, info, window, cx);

        let arrow = div()
            .id(ElementId::from(("tree-arrow", ix)))
            .flex()
            .flex_none()
            .items_center()
            .justify_center()
            .w(px(ARROW_WIDTH))
            .h(px(ROW_HEIGHT))
            .text_size(px(9.))
            .text_color(theme.text_muted)
            .when(has_children, |this| {
                let id = id.clone();
                this.cursor_pointer()
                    // The press is taken here so that the row underneath never
                    // sees the click: aiming at the arrow is aiming at the
                    // arrow, and it must not also move the selection.
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .on_click(cx.listener(move |tree, _, _window, cx| {
                        tree.toggle(&id, cx);
                        cx.stop_propagation();
                    }))
                    .child(if expanded { ARROW_OPEN } else { ARROW_CLOSED })
            });

        div()
            .id(ElementId::from(("tree-row", ix)))
            .flex()
            .flex_row()
            .items_center()
            .h(px(ROW_HEIGHT))
            .w_full()
            .pl(indent)
            .pr(px(ROW_PADDING))
            .when(selected, |this| this.bg(theme.surface_active))
            .cursor_pointer()
            .hover(|style| style.bg(theme.surface_hover))
            .on_click(cx.listener(move |tree, event: &ClickEvent, window, cx| {
                tree.focus_handle.focus(window);
                tree.set_selected(Some(id.clone()), cx);
                if event.click_count() >= 2 {
                    cx.emit(TreeEvent::Activated(id.clone()));
                }
            }))
            .child(arrow)
            .child(content)
            .into_any_element()
    }
}

impl<S: TreeSource> EventEmitter<TreeEvent<S::Id>> for TreeView<S> {}

impl<S: TreeSource> Focusable for TreeView<S> {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl<S: TreeSource> Render for TreeView<S> {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.ensure_rows(cx);
        let theme = theme(cx);
        let count = self.rows.len();
        let tree = cx.entity();

        // The owner's half of an overlay bar, as every scrolling surface in the
        // app wires it: notice the list has moved, and arm the expiry from
        // inside the draw that noticed.
        if let Some(epoch) = self
            .bar
            .moved(scrolled(&self.base_handle(), ScrollbarAxis::Vertical))
        {
            hide_later(epoch, cx, |tree: &mut Self| Some(&mut tree.bar));
        }

        // Only the rows the viewport can reach are built, which is what lets a
        // schema with thousands of tables be opened at all.
        let list = uniform_list("tree-rows", count, move |range, window, cx| {
            tree.update(cx, |tree, cx| {
                range
                    .map(|ix| tree.render_row(ix, window, cx))
                    .collect::<Vec<_>>()
            })
        })
        .track_scroll(self.scroll.clone())
        .size_full();

        div()
            .key_context(KEY_CONTEXT)
            .track_focus(&self.focus_handle)
            .relative()
            .size_full()
            .overflow_hidden()
            .text_size(px(13.))
            .text_color(theme.text)
            .on_action(cx.listener(Self::select_prev))
            .on_action(cx.listener(Self::select_next))
            .on_action(cx.listener(Self::select_first))
            .on_action(cx.listener(Self::select_last))
            .on_action(cx.listener(Self::expand_selected))
            .on_action(cx.listener(Self::collapse_selected))
            .on_action(cx.listener(Self::activate_selected))
            .on_drag_move::<DraggedThumb>(cx.listener(
                |tree, event: &DragMoveEvent<DraggedThumb>, _window, cx| {
                    let Some(progress) = tree.scrollbar().dragged(event, cx) else {
                        return;
                    };
                    tree.bar.hold();
                    scroll_to(&tree.base_handle(), ScrollbarAxis::Vertical, progress);
                    cx.notify();
                },
            ))
            // Both halves: a thumb dragged off the end of its track lets go with
            // the pointer outside the window, which only the second sees.
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|tree, _: &MouseUpEvent, _window, cx| tree.release_thumb(cx)),
            )
            .on_mouse_up_out(
                MouseButton::Left,
                cx.listener(|tree, _: &MouseUpEvent, _window, cx| tree.release_thumb(cx)),
            )
            .child(list)
            .children(self.scrollbar().render(&theme))
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::collections::HashMap;
    use std::ops::Deref;
    use std::rc::Rc;

    use gpui::{
        Entity, Modifiers, MouseDownEvent, MouseUpEvent, Pixels, Point, SharedString,
        TestAppContext, VisualTestContext, point,
    };

    use super::*;

    /// Node ids are plain strings here, as a host's would be a qualified name.
    type Id = &'static str;

    /// Vertical middle of the first row.
    const FIRST_ROW: f32 = ROW_HEIGHT / 2.;

    /// Vertical middle of the second row.
    const SECOND_ROW: f32 = ROW_HEIGHT + ROW_HEIGHT / 2.;

    /// A column inside the disclosure arrow of an outermost row.
    const ON_THE_ARROW: f32 = ROW_PADDING + ARROW_WIDTH / 2.;

    /// A column well past the arrow, on the label the source drew.
    const ON_THE_LABEL: f32 = 60.;

    /// A source that is nothing but the map a host would keep, so that a test
    /// can put a node into any of the four states by hand.
    #[derive(Default)]
    struct Fixture {
        children: HashMap<Option<Id>, ChildState<Id>>,
    }

    impl Fixture {
        /// Records what is under `parent`.
        fn set(&mut self, parent: Option<Id>, state: ChildState<Id>) {
            self.children.insert(parent, state);
        }
    }

    impl TreeSource for Fixture {
        type Id = Id;

        fn children(&self, parent: Option<&Id>) -> ChildState<Id> {
            self.children
                .get(&parent.copied())
                .cloned()
                .unwrap_or(ChildState::Leaf)
        }

        fn render_row(
            &self,
            id: &Id,
            _info: TreeRowInfo,
            _window: &mut Window,
            _cx: &mut App,
        ) -> AnyElement {
            div().child(SharedString::from(*id)).into_any_element()
        }
    }

    /// A tree three levels deep, everything already fetched:
    ///
    /// ```text
    /// a          b
    /// ├ a1       (leaf)
    /// │ └ a1x
    /// └ a2
    /// ```
    fn three_deep() -> Fixture {
        let mut fixture = Fixture::default();
        fixture.set(None, ChildState::Loaded(vec!["a", "b"]));
        fixture.set(Some("a"), ChildState::Loaded(vec!["a1", "a2"]));
        fixture.set(Some("a1"), ChildState::Loaded(vec!["a1x"]));
        fixture
    }

    /// A view that does nothing but hold the tree, as a panel would.
    struct Harness {
        tree: Entity<TreeView<Fixture>>,
    }

    impl Render for Harness {
        fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
            div().size_full().child(self.tree.clone())
        }
    }

    /// Everything a test reads back: the tree, and what it announced.
    struct Handles {
        tree: Entity<TreeView<Fixture>>,
        events: Rc<RefCell<Vec<TreeEvent<Id>>>>,
    }

    impl Handles {
        /// The visible rows as `(id, depth)`, with `None` for a placeholder.
        fn shape(&self, cx: &mut VisualTestContext) -> Vec<(Option<Id>, usize)> {
            cx.update(|_, cx| {
                self.tree
                    .read(cx)
                    .rows()
                    .iter()
                    .map(|row| (row.id().copied(), row.depth()))
                    .collect()
            })
        }

        /// The selected node.
        fn selected(&self, cx: &mut VisualTestContext) -> Option<Id> {
            cx.update(|_, cx| self.tree.read(cx).selected().copied())
        }

        /// Everything announced since the last look.
        fn drain(&self) -> Vec<TreeEvent<Id>> {
            self.events.borrow_mut().drain(..).collect()
        }

        /// The nodes asked for since the last look.
        fn loads(&self) -> Vec<Option<Id>> {
            self.drain()
                .into_iter()
                .filter_map(|event| match event {
                    TreeEvent::LoadChildren(parent) => Some(parent),
                    _ => None,
                })
                .collect()
        }

        /// Changes the source the way a host does when an answer lands.
        fn load(&self, parent: Option<Id>, state: ChildState<Id>, cx: &mut VisualTestContext) {
            cx.update(|_, cx| {
                self.tree
                    .update(cx, |tree, cx| tree.source_mut(cx).set(parent, state))
            });
            cx.run_until_parked();
        }
    }

    /// Opens a focused tree over `fixture` and hands back its handles.
    fn open(fixture: Fixture, cx: &mut TestAppContext) -> (Handles, VisualTestContext) {
        cx.update(crate::init);

        let events: Rc<RefCell<Vec<TreeEvent<Id>>>> = Rc::new(RefCell::new(Vec::new()));
        let window = cx.add_window({
            let events = events.clone();
            move |_, cx| {
                let tree = cx.new(|cx| TreeView::new(fixture, cx));
                cx.subscribe(
                    &tree,
                    move |_: &mut Harness, _, event: &TreeEvent<Id>, _| {
                        events.borrow_mut().push(event.clone());
                    },
                )
                .detach();
                Harness { tree }
            }
        });
        let tree = window
            .update(cx, |harness, _, _| harness.tree.clone())
            .expect("the window is open");

        let mut cx = VisualTestContext::from_window(*window.deref(), cx);
        cx.update(|window, cx| {
            let handle = tree.read(cx).focus_handle(cx);
            handle.focus(window);
        });
        cx.run_until_parked();

        (Handles { tree, events }, cx)
    }

    /// Presses the left button `count` times over `position` in one go, which
    /// is how a double click reaches an element.
    fn click(cx: &mut VisualTestContext, position: Point<Pixels>, count: usize) {
        cx.simulate_event(MouseDownEvent {
            position,
            modifiers: Modifiers::none(),
            button: MouseButton::Left,
            click_count: count,
            first_mouse: false,
        });
        cx.simulate_event(MouseUpEvent {
            position,
            modifiers: Modifiers::none(),
            button: MouseButton::Left,
            click_count: count,
        });
        cx.run_until_parked();
    }

    /// The whole point of the flattened list: what is under a closed node is
    /// not in it at all, and what is under an open one is, at one more level.
    #[gpui::test]
    fn opening_and_closing_a_node_changes_the_visible_rows(cx: &mut TestAppContext) {
        let (tree, mut cx) = open(three_deep(), cx);
        assert_eq!(tree.shape(&mut cx), vec![(Some("a"), 0), (Some("b"), 0)]);

        cx.update(|_, cx| tree.tree.update(cx, |tree, cx| tree.expand(&"a", cx)));
        assert_eq!(
            tree.shape(&mut cx),
            vec![
                (Some("a"), 0),
                (Some("a1"), 1),
                (Some("a2"), 1),
                (Some("b"), 0)
            ]
        );

        cx.update(|_, cx| tree.tree.update(cx, |tree, cx| tree.expand(&"a1", cx)));
        assert_eq!(
            tree.shape(&mut cx),
            vec![
                (Some("a"), 0),
                (Some("a1"), 1),
                (Some("a1x"), 2),
                (Some("a2"), 1),
                (Some("b"), 0)
            ]
        );

        cx.update(|_, cx| tree.tree.update(cx, |tree, cx| tree.collapse(&"a", cx)));
        assert_eq!(tree.shape(&mut cx), vec![(Some("a"), 0), (Some("b"), 0)]);

        // And the level below it was only hidden, not forgotten: reopening the
        // parent brings back the grandchild that was showing.
        cx.update(|_, cx| tree.tree.update(cx, |tree, cx| tree.expand(&"a", cx)));
        assert_eq!(
            tree.shape(&mut cx),
            vec![
                (Some("a"), 0),
                (Some("a1"), 1),
                (Some("a1x"), 2),
                (Some("a2"), 1),
                (Some("b"), 0)
            ]
        );
    }

    /// A leaf has no arrow to press and cannot be opened.
    #[gpui::test]
    fn a_leaf_cannot_be_opened(cx: &mut TestAppContext) {
        let (tree, mut cx) = open(three_deep(), cx);
        cx.update(|_, cx| tree.tree.update(cx, |tree, cx| tree.expand(&"b", cx)));

        assert_eq!(tree.shape(&mut cx), vec![(Some("a"), 0), (Some("b"), 0)]);
        assert!(cx.update(|_, cx| !tree.tree.read(cx).is_expanded(&"b")));
        assert_eq!(
            cx.update(|_, cx| tree.tree.read(cx).rows()[1].clone()),
            TreeRow::Node {
                id: "b",
                depth: 0,
                has_children: false
            }
        );
    }

    /// The round trip, from both ends: the tree asks once, waits with a
    /// placeholder however many times it is redrawn, and shows the children the
    /// moment the host drops them in.
    #[gpui::test]
    fn an_unfetched_node_is_asked_for_once_and_waited_on(cx: &mut TestAppContext) {
        let mut fixture = Fixture::default();
        fixture.set(None, ChildState::NotLoaded);
        let (tree, mut cx) = open(fixture, cx);

        // Nobody has fetched the outermost level either, and the request for it
        // reaches a host that subscribed after building the tree.
        assert_eq!(tree.loads(), vec![None]);
        assert_eq!(tree.shape(&mut cx), vec![(None, 0)]);

        tree.load(None, ChildState::Loaded(vec!["a"]), &mut cx);
        tree.load(Some("a"), ChildState::NotLoaded, &mut cx);
        assert_eq!(tree.shape(&mut cx), vec![(Some("a"), 0)]);
        assert_eq!(tree.loads(), vec![], "a closed node is nobody's business");

        cx.update(|_, cx| tree.tree.update(cx, |tree, cx| tree.expand(&"a", cx)));
        cx.run_until_parked();
        assert_eq!(tree.loads(), vec![Some("a")]);
        assert_eq!(tree.shape(&mut cx), vec![(Some("a"), 0), (None, 1)]);

        // A fetch in flight is still a placeholder, and is not asked for again
        // however often the tree is rebuilt.
        tree.load(Some("a"), ChildState::Loading, &mut cx);
        assert_eq!(tree.shape(&mut cx), vec![(Some("a"), 0), (None, 1)]);
        cx.update(|_, cx| tree.tree.update(cx, |tree, cx| tree.refresh(cx)));
        cx.run_until_parked();
        assert_eq!(tree.loads(), vec![]);

        tree.load(Some("a"), ChildState::Loaded(vec!["a1", "a2"]), &mut cx);
        assert_eq!(
            tree.shape(&mut cx),
            vec![(Some("a"), 0), (Some("a1"), 1), (Some("a2"), 1)]
        );
        assert_eq!(tree.loads(), vec![]);
    }

    /// A host that drops a node's children back to "not fetched" gets asked
    /// again — the request is remembered only until it is answered.
    #[gpui::test]
    fn dropping_children_again_asks_again(cx: &mut TestAppContext) {
        let (tree, mut cx) = open(three_deep(), cx);
        cx.update(|_, cx| tree.tree.update(cx, |tree, cx| tree.expand(&"a", cx)));
        cx.run_until_parked();
        assert_eq!(tree.loads(), vec![]);

        tree.load(Some("a"), ChildState::NotLoaded, &mut cx);
        assert_eq!(tree.loads(), vec![Some("a")]);
        assert_eq!(
            tree.shape(&mut cx),
            vec![(Some("a"), 0), (None, 1), (Some("b"), 0)]
        );
    }

    /// Up and down walk the rows that are showing, and stop at the ends.
    #[gpui::test]
    fn the_arrow_keys_walk_the_visible_rows(cx: &mut TestAppContext) {
        let (tree, mut cx) = open(three_deep(), cx);

        cx.simulate_keystrokes("down");
        assert_eq!(tree.selected(&mut cx), Some("a"));
        // "b" and not "a1": what is inside a closed node is not on screen.
        cx.simulate_keystrokes("down");
        assert_eq!(tree.selected(&mut cx), Some("b"));
        cx.simulate_keystrokes("down");
        assert_eq!(
            tree.selected(&mut cx),
            Some("b"),
            "the last row is the last"
        );

        cx.simulate_keystrokes("up");
        assert_eq!(tree.selected(&mut cx), Some("a"));
        cx.simulate_keystrokes("up");
        assert_eq!(
            tree.selected(&mut cx),
            Some("a"),
            "the first row is the first"
        );

        cx.simulate_keystrokes("right");
        assert_eq!(tree.selected(&mut cx), Some("a"), "opening moves nothing");
        assert_eq!(
            tree.shape(&mut cx),
            vec![
                (Some("a"), 0),
                (Some("a1"), 1),
                (Some("a2"), 1),
                (Some("b"), 0)
            ]
        );

        cx.simulate_keystrokes("end");
        assert_eq!(tree.selected(&mut cx), Some("b"));
        cx.simulate_keystrokes("home");
        assert_eq!(tree.selected(&mut cx), Some("a"));
    }

    /// Right on a leaf is not "move somewhere else", it is nothing.
    #[gpui::test]
    fn right_does_nothing_on_a_leaf(cx: &mut TestAppContext) {
        let (tree, mut cx) = open(three_deep(), cx);
        cx.simulate_keystrokes("end");
        assert_eq!(tree.selected(&mut cx), Some("b"));

        cx.simulate_keystrokes("right");
        assert_eq!(tree.selected(&mut cx), Some("b"));
        assert_eq!(tree.shape(&mut cx), vec![(Some("a"), 0), (Some("b"), 0)]);
    }

    /// Left closes what is open and climbs out of what is not.
    #[gpui::test]
    fn left_closes_a_node_and_then_climbs_to_its_parent(cx: &mut TestAppContext) {
        let (tree, mut cx) = open(three_deep(), cx);
        cx.update(|_, cx| tree.tree.update(cx, |tree, cx| tree.expand(&"a", cx)));
        cx.simulate_keystrokes("down down");
        assert_eq!(tree.selected(&mut cx), Some("a1"));

        // "a1" is closed, so the key steps out to the node it hangs from.
        cx.simulate_keystrokes("left");
        assert_eq!(tree.selected(&mut cx), Some("a"));

        // "a" is open, so the same key closes it instead.
        cx.simulate_keystrokes("left");
        assert_eq!(tree.selected(&mut cx), Some("a"));
        assert_eq!(tree.shape(&mut cx), vec![(Some("a"), 0), (Some("b"), 0)]);

        // And at the outermost level there is nowhere left to climb.
        cx.simulate_keystrokes("left");
        assert_eq!(tree.selected(&mut cx), Some("a"));
    }

    /// Right on an open node steps into it, which is what makes left symmetric.
    #[gpui::test]
    fn right_steps_into_a_node_that_is_already_open(cx: &mut TestAppContext) {
        let (tree, mut cx) = open(three_deep(), cx);
        cx.simulate_keystrokes("down right");
        assert_eq!(tree.selected(&mut cx), Some("a"));

        cx.simulate_keystrokes("right");
        assert_eq!(tree.selected(&mut cx), Some("a1"));
    }

    /// Enter and space are the host's cue to do something with the node.
    #[gpui::test]
    fn enter_and_space_activate_the_selection(cx: &mut TestAppContext) {
        let (tree, mut cx) = open(three_deep(), cx);
        cx.simulate_keystrokes("down");
        tree.drain();

        cx.simulate_keystrokes("enter");
        assert_eq!(tree.drain(), vec![TreeEvent::Activated("a")]);
        cx.simulate_keystrokes("space");
        assert_eq!(tree.drain(), vec![TreeEvent::Activated("a")]);
    }

    /// The keys reach a placeholder row's neighbours without ever landing on
    /// the placeholder: there is nothing there to act on.
    #[gpui::test]
    fn a_placeholder_row_cannot_be_selected(cx: &mut TestAppContext) {
        let mut fixture = Fixture::default();
        fixture.set(None, ChildState::Loaded(vec!["a", "b"]));
        fixture.set(Some("a"), ChildState::Loading);
        let (tree, mut cx) = open(fixture, cx);
        cx.update(|_, cx| tree.tree.update(cx, |tree, cx| tree.expand(&"a", cx)));
        assert_eq!(
            tree.shape(&mut cx),
            vec![(Some("a"), 0), (None, 1), (Some("b"), 0)]
        );

        cx.simulate_keystrokes("down down");
        assert_eq!(tree.selected(&mut cx), Some("b"));
        cx.simulate_keystrokes("up");
        assert_eq!(tree.selected(&mut cx), Some("a"));
    }

    /// The whole reason the open set is keyed by id: a host that throws its
    /// nodes away and fetches them again finds the tree exactly as it left it.
    #[gpui::test]
    fn open_nodes_survive_a_reload(cx: &mut TestAppContext) {
        let (tree, mut cx) = open(three_deep(), cx);
        cx.update(|_, cx| {
            tree.tree.update(cx, |tree, cx| {
                tree.expand(&"a", cx);
                tree.expand(&"a1", cx);
                tree.set_selected(Some("a2"), cx);
            })
        });
        let before = tree.shape(&mut cx);

        // A reload, as a host performs one: every level replaced by a fresh
        // answer that happens to name the same nodes.
        cx.update(|_, cx| {
            tree.tree.update(cx, |tree, cx| {
                let source = tree.source_mut(cx);
                source.set(None, ChildState::Loaded(vec!["a", "b"]));
                source.set(Some("a"), ChildState::Loaded(vec!["a1", "a2"]));
                source.set(Some("a1"), ChildState::Loaded(vec!["a1x"]));
            })
        });
        cx.run_until_parked();

        assert_eq!(tree.shape(&mut cx), before);
        assert_eq!(tree.selected(&mut cx), Some("a2"));
    }

    /// Closing a node the selection was inside brings the selection up to that
    /// node rather than leaving it pointing at something nobody can see.
    #[gpui::test]
    fn closing_a_node_takes_the_selection_up_with_it(cx: &mut TestAppContext) {
        let (tree, mut cx) = open(three_deep(), cx);
        cx.update(|_, cx| {
            tree.tree.update(cx, |tree, cx| {
                tree.expand(&"a", cx);
                tree.expand(&"a1", cx);
                tree.set_selected(Some("a1x"), cx);
            })
        });
        tree.drain();

        cx.update(|_, cx| tree.tree.update(cx, |tree, cx| tree.collapse(&"a", cx)));
        cx.run_until_parked();

        assert_eq!(tree.selected(&mut cx), Some("a"));
        assert_eq!(
            tree.drain(),
            vec![TreeEvent::SelectionChanged(Some("a"))],
            "the host is told the selection moved"
        );
        assert_eq!(tree.shape(&mut cx), vec![(Some("a"), 0), (Some("b"), 0)]);
    }

    /// A selection somewhere else entirely is left alone by a close: only the
    /// subtree that disappeared is anybody's problem.
    #[gpui::test]
    fn closing_a_node_leaves_a_selection_outside_it_alone(cx: &mut TestAppContext) {
        let (tree, mut cx) = open(three_deep(), cx);
        cx.update(|_, cx| {
            tree.tree.update(cx, |tree, cx| {
                tree.expand(&"a", cx);
                tree.set_selected(Some("b"), cx);
                tree.collapse(&"a", cx);
            })
        });

        assert_eq!(tree.selected(&mut cx), Some("b"));
    }

    /// A node the host has reloaded away keeps the selection in the tree's
    /// pocket — it highlights nothing while no row carries it, and comes back
    /// with the node.
    #[gpui::test]
    fn a_selection_the_host_reloaded_away_is_kept_but_not_shown(cx: &mut TestAppContext) {
        let (tree, mut cx) = open(three_deep(), cx);
        cx.update(|_, cx| {
            tree.tree
                .update(cx, |tree, cx| tree.set_selected(Some("b"), cx))
        });

        tree.load(None, ChildState::Loaded(vec!["a"]), &mut cx);
        assert_eq!(tree.selected(&mut cx), Some("b"));
        assert_eq!(
            cx.update(|_, cx| tree.tree.read(cx).selected_index()),
            None,
            "nothing on screen is highlighted"
        );

        tree.load(None, ChildState::Loaded(vec!["a", "b"]), &mut cx);
        assert_eq!(
            cx.update(|_, cx| tree.tree.read(cx).selected_index()),
            Some(1)
        );
    }

    /// A click picks a row; a second one in the same gesture opens it.
    #[gpui::test]
    fn a_click_selects_and_a_double_click_activates(cx: &mut TestAppContext) {
        let (tree, mut cx) = open(three_deep(), cx);

        click(&mut cx, point(px(ON_THE_LABEL), px(FIRST_ROW)), 1);
        assert_eq!(tree.selected(&mut cx), Some("a"));
        assert_eq!(
            tree.drain(),
            vec![TreeEvent::SelectionChanged(Some("a"))],
            "one click is not an activation"
        );

        click(&mut cx, point(px(ON_THE_LABEL), px(SECOND_ROW)), 1);
        click(&mut cx, point(px(ON_THE_LABEL), px(SECOND_ROW)), 2);
        assert_eq!(tree.selected(&mut cx), Some("b"));
        assert!(tree.drain().contains(&TreeEvent::Activated("b")));
    }

    /// The arrow is its own control: pressing it opens the node and leaves the
    /// selection where the user put it.
    #[gpui::test]
    fn a_click_on_the_arrow_opens_without_moving_the_selection(cx: &mut TestAppContext) {
        let (tree, mut cx) = open(three_deep(), cx);
        click(&mut cx, point(px(ON_THE_LABEL), px(SECOND_ROW)), 1);
        assert_eq!(tree.selected(&mut cx), Some("b"));

        click(&mut cx, point(px(ON_THE_ARROW), px(FIRST_ROW)), 1);
        assert_eq!(
            tree.shape(&mut cx),
            vec![
                (Some("a"), 0),
                (Some("a1"), 1),
                (Some("a2"), 1),
                (Some("b"), 0)
            ]
        );
        assert_eq!(tree.selected(&mut cx), Some("b"));

        click(&mut cx, point(px(ON_THE_ARROW), px(FIRST_ROW)), 1);
        assert_eq!(tree.shape(&mut cx), vec![(Some("a"), 0), (Some("b"), 0)]);
        assert_eq!(tree.selected(&mut cx), Some("b"));
    }
}
