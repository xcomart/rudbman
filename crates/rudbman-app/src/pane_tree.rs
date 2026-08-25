//! What a tab of the work area is, and how a pane is asked which one it holds.
//!
//! The layout itself is [`ruui_shell::pane`]: the binary tree of splits, the
//! promotion and collapse rules, and the strip of tabs a leaf holds. None of
//! that is about a database — a split, a promotion and a collapse rearrange
//! *shape* over a payload the tree never looks inside — so it is the shell's,
//! and what stays here is the payload and the lookups over it.
//!
//! The work area shows one tree per connection tab: every leaf is one [`Pane`]
//! — a strip of tabs and the one of them on top — and every interior node
//! divides its area in two along an [`Axis`]. The view layer walks the result
//! through [`PaneTree::root`] and renders one nested flex box per node.
//!
//! # Why a tab is an enum
//!
//! A tab is an enum rather than a boxed trait object because the shell has to
//! know what it is looking at anyway: the status bar reports a row count for a
//! grid and nothing of the sort for an ERD canvas, and a `Box<dyn Panel>` would
//! only push that decision into a downcast. A new kind of panel is one variant
//! of [`PaneItem`], one arm where the shell renders the active tab, and — where
//! opening it twice should navigate rather than duplicate — one lookup on
//! [`PaneLookup`]. The tree stays untouched.
//!
//! # Why a pane holds a list rather than one panel
//!
//! Reading a table's columns while writing the statement that queries it is the
//! ordinary case, and a pane that held exactly one thing answered it by throwing
//! the previous panel away. Tabs make the two coexist without forcing the user
//! to split the window for every object they open. An empty list is a state of
//! its own — the pane keeps standing, showing the empty state — because closing
//! the last tab must not rearrange a layout the user placed.

use gpui::{App, Entity, SharedString};
pub use ruui_shell::pane::{Axis, PaneId, PaneNode, PaneTree, SplitId};

use crate::builder_pane::BuilderPane;
use crate::data_pane::DataPane;
use crate::erd_pane::{ErdPane, ErdTarget};
use crate::explorer::{ConnectionId, ObjectTarget};
use crate::i18n::ts;
use crate::query::QueryPane;
use crate::struct_pane::StructPane;
use crate::table_detail::TableDetail;

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
    /// One table's rows, in a grid of their own.
    ///
    /// A sibling of the query pane rather than a fifth tab of the detail
    /// panel, for the lifetime reason the architecture document's §7.9 gives:
    /// this one holds a cursor and, from the next milestone, edits nobody has
    /// applied yet. Holds only the panel, because the panel knows which object
    /// it is showing — which is also what tells two of them apart.
    TableData(Entity<DataPane>),
    /// One table's shape, and the `ALTER TABLE` batch that would change it.
    ///
    /// A sibling of [`PaneItem::TableData`] rather than a fifth tab of the
    /// detail panel, for the reason §7.10 gives: the panel keeps its rows as
    /// display strings, so nothing an editor needs survives in it. Holds only
    /// the panel, which knows which table it is editing — and that is also what
    /// tells two of them apart.
    TableStruct(Entity<StructPane>),
    /// A canvas of tables, the form under it, and the `SELECT` they describe.
    ///
    /// Numbered like a query pane and for the same reason: several at once is
    /// the ordinary case — one builder per question — and unlike a diagram
    /// there is nothing about a builder that identifies it, because two of them
    /// over the same tables are two different questions.
    QueryBuilder {
        /// The canvas and its form.
        pane: Entity<BuilderPane>,
        /// Which builder of this connection it is, counting from one. Kept
        /// beside the view for the reason a query pane's number is: it is the
        /// tab strip's business and nothing else's.
        number: u64,
    },
}

impl PaneItem {
    /// The title its tab carries.
    ///
    /// A detail panel is named after the object it describes, qualified by its
    /// schema; a structure pane the same, unless it is making a table that does
    /// not exist yet; a query pane has no name of its own, so it takes its number; a
    /// diagram is named after the scope it covers; a builder takes its number,
    /// as the query pane does and for the same reason. A data pane is named
    /// after its table, exactly as the detail panel is: the two never sit in
    /// one pane without the user having opened both on purpose, and the icons
    /// in the strip are what tell them apart.
    pub fn title(&self, cx: &App) -> SharedString {
        match self {
            PaneItem::TableDetail(panel) => SharedString::from(panel.read(cx).target().qualified()),
            PaneItem::Query { number, .. } => ts!("query.tab", index = number),
            PaneItem::Erd(panel) => panel.read(cx).target().title(),
            PaneItem::TableData(panel) => SharedString::from(panel.read(cx).target().qualified()),
            // Asked of the pane rather than read off its target, because a
            // structure pane in its create mode has no table to be named after
            // yet — only a field somebody is typing into (§7.10).
            PaneItem::TableStruct(panel) => panel.read(cx).title(),
            PaneItem::QueryBuilder { number, .. } => ts!("builder.tab", index = number),
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
            PaneItem::TableData(panel) => panel.read(cx).connection(),
            PaneItem::TableStruct(panel) => panel.read(cx).connection(),
            PaneItem::QueryBuilder { pane, .. } => pane.read(cx).connection(),
        }
    }

    /// Whether closing this tab would throw away work the user has not sent
    /// anywhere.
    ///
    /// The tabs that can say yes are the three that stage edits: a data pane
    /// and a query pane hold rows the user has changed but not applied, and a
    /// structure pane holds columns and constraints. All three live in the
    /// source under a grid or in indices into a snapshot, and dropping the tab
    /// drops them with no way back (architecture document, §7.9, §7.10).
    ///
    /// A query pane says yes only about its *results*. Its SQL is the user's
    /// text and closing the tab is how one gets rid of it; what a close must
    /// not throw away silently is a staged `UPDATE` nobody has sent. Everything
    /// else here holds nothing but a view of something the database still has.
    ///
    /// A question and not a veto: the caller decides what to do about it, and
    /// the shell answers by refusing the close and saying why
    /// ([`DataPane::warn_pending`]).
    pub fn blocks_close(&self, cx: &App) -> bool {
        match self {
            PaneItem::TableData(panel) => panel.read(cx).has_pending_edits(cx),
            PaneItem::TableStruct(panel) => panel.read(cx).has_pending_edits(),
            PaneItem::Query { pane, .. } => pane.read(cx).has_pending_edits(cx),
            PaneItem::TableDetail(_) | PaneItem::Erd(_) | PaneItem::QueryBuilder { .. } => false,
        }
    }
}

/// One pane of the work area: its tabs, and which of them is on top.
///
/// [`ruui_shell::pane::Pane`] at rudbman's own tab type. Only the active tab is
/// rendered, which is what makes closing or switching one a focus hazard — gpui
/// resolves actions against the focused element of the last drawn frame — and
/// why the workspace reclaims the keyboard around every call that changes what
/// [`Pane::active`](ruui_shell::pane::Pane::active) returns.
pub type Pane = ruui_shell::pane::Pane<PaneItem>;

/// Finding the tab that is already showing a given thing.
///
/// An extension trait rather than inherent methods, because the pane is the
/// shell's and these questions are rudbman's: every one of them is "is this
/// object already open here", and the answer decides whether a double click
/// opens a tab or brings one forward. All five sit on
/// [`Pane::position`](ruui_shell::pane::Pane::position), which is the hook the
/// shell leaves for exactly this.
pub trait PaneLookup {
    /// The index of the tab showing `target`, if one is open here.
    fn detail_of(&self, target: &ObjectTarget, cx: &App) -> Option<usize>;

    /// The index of the tab showing `target`'s rows, if one is open here.
    ///
    /// The counterpart of [`PaneLookup::detail_of`], kept apart from it because
    /// the two are different tabs over the same object: a data pane and a
    /// detail panel of one table are both ordinary, and each must find its own.
    fn data_of(&self, target: &ObjectTarget, cx: &App) -> Option<usize>;

    /// The index of the tab editing `target`'s structure, if one is open here.
    ///
    /// A third lookup over the same object rather than a shared one, for
    /// [`PaneLookup::data_of`]'s reason: the shape of a table, its rows and its
    /// description are three tabs a user may perfectly well want at once, and
    /// each has to find its own.
    fn struct_of(&self, target: &ObjectTarget, cx: &App) -> Option<usize>;

    /// The index of the tab drawing `target`, if one is open here.
    ///
    /// There for [`PaneLookup::detail_of`]'s reason: a second diagram of one
    /// scope would show exactly what the first one does, so opening it again is
    /// a navigation.
    fn erd_of(&self, target: &ErdTarget, cx: &App) -> Option<usize>;

    /// The index of the first query builder open here, if there is one.
    ///
    /// Unlike a detail panel or a diagram, a builder is not *of* anything, so
    /// this cannot ask "is this one showing X". It answers the question "add to
    /// builder" actually has — where is there a builder to add to — and the
    /// workspace prefers the tab already on top over the answer from here.
    fn first_builder(&self) -> Option<usize>;
}

impl PaneLookup for Pane {
    fn detail_of(&self, target: &ObjectTarget, cx: &App) -> Option<usize> {
        self.position(|item| match item {
            PaneItem::TableDetail(panel) => panel.read(cx).target() == target,
            PaneItem::Query { .. }
            | PaneItem::Erd(_)
            | PaneItem::TableData(_)
            | PaneItem::TableStruct(_)
            | PaneItem::QueryBuilder { .. } => false,
        })
    }

    fn data_of(&self, target: &ObjectTarget, cx: &App) -> Option<usize> {
        self.position(|item| match item {
            PaneItem::TableData(panel) => panel.read(cx).target() == target,
            PaneItem::TableDetail(_)
            | PaneItem::Query { .. }
            | PaneItem::Erd(_)
            | PaneItem::TableStruct(_)
            | PaneItem::QueryBuilder { .. } => false,
        })
    }

    fn struct_of(&self, target: &ObjectTarget, cx: &App) -> Option<usize> {
        self.position(|item| match item {
            PaneItem::TableStruct(panel) => panel.read(cx).target() == target,
            PaneItem::TableDetail(_)
            | PaneItem::Query { .. }
            | PaneItem::Erd(_)
            | PaneItem::TableData(_)
            | PaneItem::QueryBuilder { .. } => false,
        })
    }

    fn erd_of(&self, target: &ErdTarget, cx: &App) -> Option<usize> {
        self.position(|item| match item {
            PaneItem::Erd(panel) => panel.read(cx).target() == target,
            PaneItem::TableDetail(_)
            | PaneItem::Query { .. }
            | PaneItem::TableData(_)
            | PaneItem::TableStruct(_)
            | PaneItem::QueryBuilder { .. } => false,
        })
    }

    fn first_builder(&self) -> Option<usize> {
        self.position(|item| matches!(item, PaneItem::QueryBuilder { .. }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The shell's tests pin the mechanics down with a payload that can be told
    /// apart at a glance. What is worth asserting here is the instantiation
    /// rudbman actually builds — the part a new variant of [`PaneItem`] could
    /// break without any of those noticing.
    #[test]
    fn the_work_area_layout_splits_and_collapses_like_any_other() {
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
        // Down to the one pane the workspace always keeps, which cannot be
        // closed.
        assert_eq!(tree.leaf_count(), 1);
        assert!(tree.remove(one).is_none());
    }

    /// A tab is a view, so a pane holding one is asserted in the workspace's own
    /// tests, where a session exists to build one from. What can be said here is
    /// that an empty pane answers every rudbman lookup without panicking: it is
    /// the state a freshly split pane starts in and the one it returns to when
    /// its last tab is closed.
    #[gpui::test]
    fn an_empty_pane_is_showing_none_of_the_things_that_can_be_looked_up(
        cx: &mut gpui::TestAppContext,
    ) {
        cx.update(|cx| {
            let pane = Pane::new();
            let connection = ConnectionId(1);
            let object = ObjectTarget {
                connection,
                catalog: None,
                schema: Some("public".to_string()),
                folder: crate::explorer::Folder::Tables,
                name: "orders".to_string(),
            };
            let diagram = ErdTarget {
                connection,
                scope: crate::explorer::Scope {
                    catalog: None,
                    schema: Some("public".to_string()),
                },
            };
            assert!(pane.detail_of(&object, cx).is_none());
            assert!(pane.data_of(&object, cx).is_none());
            assert!(pane.struct_of(&object, cx).is_none());
            assert!(pane.erd_of(&diagram, cx).is_none());
            assert!(pane.first_builder().is_none());
        });
    }
}
