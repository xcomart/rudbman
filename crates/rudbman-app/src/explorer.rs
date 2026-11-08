//! The explorer sidebar: the tree of the connection whose tab is on top.
//!
//! The panel down the left of the window (architecture document, §7.1). Each
//! open connection is a root; under it the database's catalogs, schemas,
//! folders and objects arrive one round trip at a time.
//!
//! # Why every root is kept and only one is drawn
//!
//! The source holds the roots and the fetched children of *every* open
//! connection, and [`TreeSource::children`] answers the root level with the one
//! the workspace named through [`ExplorerSource::set_visible_root`]. Switching
//! the connection tab therefore switches the tree without costing a single round
//! trip: nothing is thrown away, so coming back finds the schemas already open
//! and the folders already counted. Expansion state survives untouched for the
//! same reason — the widget keys it by [`NodeId`], and a node id names the
//! connection it belongs to, so two connections can never collide.
//!
//! # Where the work happens
//!
//! [`ExplorerSource`] is the [`TreeSource`] the widget draws from, and it is a
//! **cache and nothing else** — [`TreeSource::children`] must not block, so it
//! answers whatever has arrived and [`ChildState::NotLoaded`] for the rest. The
//! fetching is the host's: the tree emits [`TreeEvent::LoadChildren`], the
//! [`Explorer`] turns that into [`ExplorerEvent::Load`], the workspace — which
//! owns the sessions — runs the `DESCRIBE` on a background task and hands the
//! answer back through [`Explorer::deliver`].
//!
//! That round trip through the workspace is deliberate. The explorer has no
//! business owning a connection, and a session that a panel could keep alive
//! after its tab closed is exactly the leak §9.3 is about.
//!
//! # The layer-skipping rule
//!
//! Products disagree about how many levels sit above a table. PostgreSQL has
//! catalogs *and* schemas, MySQL has only catalogs, H2 has one catalog and
//! usually one schema, SQLite has neither. A tree that drew every level would
//! make the common case — one database, one schema — cost two clicks that lead
//! nowhere.
//!
//! So a level with **one or no** members is skipped: the fetch that would have
//! produced it goes straight on to the next one, and the single catalog or
//! schema it found becomes part of the scope its children are asked in. A level
//! with two or more is drawn. The rule lives in [`load_children`] and its
//! `next_level`, which is where a fetch decides what it actually asked for,
//! because only the answer can say whether the level was worth a row.
//!
//! What is *never* skipped: the connection root. It is what tells two open
//! connections apart, and it is where the status dot lives.
//!
//! # Dragging a row out of the tree
//!
//! A table or a view row can be picked up and let go of on a query builder's
//! canvas, which carries a [`DraggedObject`] — the same [`ObjectTarget`] the
//! "add to builder" action sends, so the two gestures meet in one place in the
//! workspace. Only relations are draggable, because the canvas draws columns
//! and a routine has none. The tree itself knows nothing about any of this: it
//! hands each row its index, and the drag hangs off the row body drawn here.
//!
//! # A node that will not load
//!
//! A schema the user cannot read is one error row under that node
//! ([`NodeId::Error`]), and nothing else changes: the rest of the tree stays,
//! the connection stays, and the message is the driver's own. A metadata failure
//! is routine — permissions, a catalog the product exposes but will not open —
//! and it must not cost the user the tree.

use std::cell::Cell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;

use gpui::{
    AnyElement, App, Bounds, Context, Entity, EventEmitter, FocusHandle, Focusable, IntoElement,
    Pixels, Point, Render, SharedString, Subscription, Window, div, prelude::*, px,
};
use rudbman_jdbc::{DescribeRequest, Error as JdbcError, Session};
use rugpui::{
    Button, ButtonVariant, ChildState, ContextMenu, Segmented, Select, TextInput, Theme, TreeEvent,
    TreeRowInfo, TreeSource, TreeView, theme,
};
use uuid::Uuid;

use crate::app_settings;
use crate::context_menu::{self, MenuRow};
use crate::explorer_filter::{
    Candidate, Combine, Condition, ExplorerFilter, Field, FilterSnapshot, FolderSet, Op,
};
use crate::explorer_hide::HideRules;
use crate::explorer_history::FilterHistory;
use crate::i18n::ts;
use crate::icons;

/// Key context the sidebar's own shortcuts are scoped to.
///
/// Sits above the tree's own `TreeView` context, so a chord bound here is live
/// while the focus is anywhere in the sidebar and nowhere else — which is what
/// keeps "query the selected object" off the `Ctrl`+`Enter` the SQL editor
/// binds to "run the statement".
pub const KEY_CONTEXT: &str = "Explorer";

/// Identity of one open connection, for as long as its tab lives.
///
/// Not the profile's [`Uuid`](uuid::Uuid) — two tabs may be open on one profile
/// — and not an index into the workspace's list, which shifts when a tab in the
/// middle is closed. The tree keys everything it remembers by node id, so an id
/// that moved would take the open state of a whole subtree with it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ConnectionId(pub u64);

/// Where in a database a node sits.
///
/// Both halves are optional because both levels are skippable; see the module
/// documentation. A `None` catalog with a `Some` schema is ordinary — that is
/// PostgreSQL with one database — and both `None` is SQLite.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct Scope {
    /// The catalog, when the product has more than one or named the one.
    pub catalog: Option<String>,
    /// The schema, likewise.
    pub schema: Option<String>,
}

/// The kinds of object the explorer lists, one folder each.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum Folder {
    /// Base tables.
    Tables,
    /// Views.
    Views,
    /// Stored procedures.
    Procedures,
    /// Stored functions.
    Functions,
    /// Sequences.
    Sequences,
    /// Triggers.
    Triggers,
}

/// The folders under a schema, in the order they are drawn.
///
/// Tables first because that is what anyone opening a database is looking for;
/// sequences and triggers last because most schemas have few.
pub const FOLDERS: [Folder; 6] = [
    Folder::Tables,
    Folder::Views,
    Folder::Procedures,
    Folder::Functions,
    Folder::Sequences,
    Folder::Triggers,
];

impl Folder {
    /// The `DESCRIBE` kind that fills this folder.
    ///
    /// Tables and views share `tables` and are told apart by the `types` filter,
    /// which is what JDBC offers: `getTables` takes the list of types and there
    /// is no separate accessor for views.
    pub fn describe_kind(self) -> &'static str {
        match self {
            Folder::Tables | Folder::Views => "tables",
            Folder::Procedures => "procedures",
            Folder::Functions => "functions",
            Folder::Sequences => "sequences",
            Folder::Triggers => "triggers",
        }
    }

    /// The `types` filter for the `tables` kind, or `None` for the others.
    ///
    /// `TABLE` and `SYSTEM TABLE` are separate types and only the first is
    /// listed: a user opening a schema is not looking for the catalog's own
    /// bookkeeping. `VIEW` and `SYSTEM VIEW` likewise. A materialized view is a
    /// view to its reader, and is listed with them on the products that have
    /// one; a driver that knows no such type ignores the word.
    pub fn table_types(self) -> Option<&'static [&'static str]> {
        match self {
            Folder::Tables => Some(&["TABLE"]),
            Folder::Views => Some(&["VIEW", "MATERIALIZED VIEW"]),
            _ => None,
        }
    }

    /// The folder's label in the active language.
    pub fn label(self) -> SharedString {
        match self {
            Folder::Tables => ts!("explorer.tables"),
            Folder::Views => ts!("explorer.views"),
            Folder::Procedures => ts!("explorer.procedures"),
            Folder::Functions => ts!("explorer.functions"),
            Folder::Sequences => ts!("explorer.sequences"),
            Folder::Triggers => ts!("explorer.triggers"),
        }
    }

    /// The icon an object of this folder is drawn with.
    pub fn icon(self) -> &'static str {
        match self {
            Folder::Tables => icons::TABLE,
            Folder::Views => icons::VIEW,
            Folder::Procedures => icons::PROCEDURE,
            Folder::Functions => icons::FUNCTION,
            Folder::Sequences => icons::SEQUENCE,
            Folder::Triggers => icons::TRIGGER,
        }
    }

    /// Whether activating an object of this folder opens a detail panel with
    /// columns, keys and DDL in it.
    ///
    /// Only what has columns: a routine, a sequence and a trigger get the
    /// smaller panel, their properties and their DDL.
    pub fn is_relation(self) -> bool {
        matches!(self, Folder::Tables | Folder::Views)
    }
}

/// One node of the explorer tree.
///
/// Every variant carries the connection it belongs to, so that closing one tab
/// cannot leave a node of it addressable through another.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum NodeId {
    /// An open connection: the outermost level.
    Connection(ConnectionId),
    /// A catalog, drawn only when the product has more than one.
    Catalog {
        /// The connection it belongs to.
        connection: ConnectionId,
        /// The catalog's name.
        name: String,
    },
    /// A schema, drawn only when its catalog has more than one.
    Schema {
        /// The connection it belongs to.
        connection: ConnectionId,
        /// The catalog it sits in, when there is one.
        catalog: Option<String>,
        /// The schema's name.
        name: String,
    },
    /// One of [`FOLDERS`], under whichever level turned out to be the last one.
    Folder {
        /// The connection it belongs to.
        connection: ConnectionId,
        /// Where the folder's contents are looked up.
        scope: Scope,
        /// Which folder.
        folder: Folder,
    },
    /// One table, view, routine or sequence.
    Object {
        /// The connection it belongs to.
        connection: ConnectionId,
        /// Where it lives.
        scope: Scope,
        /// Which folder it was listed under.
        folder: Folder,
        /// Its name.
        name: String,
    },
    /// The row that says why the node above it could not be loaded.
    ///
    /// A child of the node that failed, so that it appears where the children
    /// would have and disappears when the node is asked again.
    Error(Box<NodeId>),
}

impl NodeId {
    /// Objects refresh their containing folder; error rows retry their parent.
    fn refresh_root(&self) -> Self {
        match self {
            Self::Object {
                connection,
                scope,
                folder,
                ..
            } => Self::Folder {
                connection: *connection,
                scope: scope.clone(),
                folder: *folder,
            },
            Self::Error(parent) => parent.refresh_root(),
            _ => self.clone(),
        }
    }

    /// Includes cached descendants even when catalog or schema levels are hidden.
    fn contains(&self, node: &Self) -> bool {
        if self.connection() != node.connection() {
            return false;
        }
        if self == node {
            return true;
        }
        if let Self::Error(parent) = node {
            return self.contains(parent);
        }
        match self {
            Self::Connection(_) => true,
            Self::Catalog { name, .. } => node
                .as_scope()
                .is_some_and(|scope| scope.catalog.as_ref() == Some(name)),
            Self::Schema { catalog, name, .. } => node.as_scope().is_some_and(|scope| {
                scope.catalog == *catalog && scope.schema.as_ref() == Some(name)
            }),
            Self::Folder { scope, folder, .. } => {
                matches!(node, Self::Object { scope: child_scope, folder: child_folder, .. } if scope == child_scope && folder == child_folder)
            }
            Self::Object { .. } | Self::Error(_) => false,
        }
    }

    /// The connection this node belongs to.
    pub fn connection(&self) -> ConnectionId {
        match self {
            NodeId::Connection(connection)
            | NodeId::Catalog { connection, .. }
            | NodeId::Schema { connection, .. }
            | NodeId::Folder { connection, .. }
            | NodeId::Object { connection, .. } => *connection,
            NodeId::Error(parent) => parent.connection(),
        }
    }

    /// Where in the database this node sits, for the panel that draws a whole
    /// scope rather than one object.
    ///
    /// What the ERD is opened over. A catalog answers with itself and no
    /// schema, which is the right scope on a product whose schema level was
    /// skipped; a folder and an object answer with the scope they were listed
    /// in. The connection root answers `None` — a diagram of every catalog at
    /// once is not a diagram — and so does an error row, which names nothing.
    pub fn as_scope(&self) -> Option<Scope> {
        match self {
            NodeId::Catalog { name, .. } => Some(Scope {
                catalog: Some(name.clone()),
                schema: None,
            }),
            NodeId::Schema { catalog, name, .. } => Some(Scope {
                catalog: catalog.clone(),
                schema: Some(name.clone()),
            }),
            NodeId::Folder { scope, .. } | NodeId::Object { scope, .. } => Some(scope.clone()),
            NodeId::Connection(_) | NodeId::Error(_) => None,
        }
    }

    /// What an object node names, for the panel that opens it.
    pub fn as_target(&self) -> Option<ObjectTarget> {
        let NodeId::Object {
            connection,
            scope,
            folder,
            name,
        } = self
        else {
            return None;
        };
        Some(ObjectTarget {
            connection: *connection,
            catalog: scope.catalog.clone(),
            schema: scope.schema.clone(),
            folder: *folder,
            name: name.clone(),
        })
    }
}

/// Everything a detail panel needs in order to describe one object.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ObjectTarget {
    /// Which connection to ask.
    pub connection: ConnectionId,
    /// Exact catalog name, or `None` for "wherever the connection points".
    pub catalog: Option<String>,
    /// Exact schema name, likewise.
    pub schema: Option<String>,
    /// What kind of object it is.
    pub folder: Folder,
    /// Its name.
    pub name: String,
}

impl ObjectTarget {
    /// The name as the title bar of a panel shows it: qualified by its schema
    /// when there is one, because two schemas routinely hold a table of the
    /// same name.
    pub fn qualified(&self) -> String {
        match self.schema.as_deref().filter(|schema| !schema.is_empty()) {
            Some(schema) => format!("{schema}.{}", self.name),
            None => self.name.clone(),
        }
    }
}

/// What a draggable row's test selector is prefixed with, ahead of the
/// object's qualified name.
const DRAG_SELECTOR: &str = "explorer-drag:";

/// A relational object being dragged out of the tree.
///
/// A wrapper rather than the bare [`ObjectTarget`] because gpui routes a drop
/// by the payload's `TypeId`: a type nothing else in the app drags means the
/// query builder's drop listener can never be woken by the sidebar-resize drag
/// or a split divider, and no listener has to check what it was handed. Only
/// tables and views are ever put in one — the same gate the "add to builder"
/// menu row uses, because a routine has no column list to draw.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DraggedObject(pub ObjectTarget);

/// The chip that follows the pointer while an object is being dragged.
///
/// Held at the cursor by padding, which is how gpui's own drag-and-drop
/// example places a ghost: the view is laid out at the window's origin and the
/// offset of the press inside the row is added as leading space.
struct DragGhost {
    /// Where in the row the press landed.
    offset: Point<Pixels>,
    /// The object's icon, by folder.
    icon: &'static str,
    /// What the chip reads: the schema-qualified name.
    label: SharedString,
}

impl Render for DragGhost {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let chrome = theme(cx);
        div().pl(self.offset.x).pt(self.offset.y).child(
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap(px(6.))
                .px(px(8.))
                .py(px(4.))
                .bg(chrome.surface)
                .border_1()
                .border_color(chrome.border)
                .text_size(rugpui::TextSize::Xs.size(cx))
                .text_color(chrome.text)
                .child(icons::icon(
                    self.icon,
                    rugpui::scaled(12., cx),
                    chrome.text_muted,
                ))
                .child(self.label.clone()),
        )
    }
}

/// What the workspace has to tell the explorer about one open connection.
#[derive(Clone, Debug)]
pub struct RootInfo {
    /// The profile's name, which is the row's label.
    pub name: SharedString,
    /// The color tag, drawn as the status dot's ring.
    pub color: Option<SharedString>,
    /// Whether the session is live. A root whose session is gone still draws —
    /// the user may be reading it — but its children are never refetched.
    pub live: bool,
}

/// One row a fetch produced: the node, and what the catalog says about it.
///
/// A pair rather than a field on [`NodeId`], because the id is what the tree is
/// keyed, selected and expanded by and a comment is not part of an object's
/// identity — see [`ExplorerSource::remarks`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Child {
    /// The node itself.
    pub id: NodeId,
    /// The catalog's comment on it, where it carries one.
    pub remarks: Option<SharedString>,
}

impl Child {
    /// A node the catalog says nothing about — every level above the objects.
    pub fn plain(id: NodeId) -> Self {
        Self { id, remarks: None }
    }
}

/// The tree's data: what has been fetched, and nothing that fetches.
#[derive(Default)]
pub struct ExplorerSource {
    /// The open connections, in tab order.
    roots: Vec<ConnectionId>,
    /// The one root the tree draws: the connection whose tab is on top.
    ///
    /// `None` while no connection is open at all, which is the only state the
    /// panel's own empty rendering answers for. Everything the other roots have
    /// fetched stays in the maps below, waiting for their tab to come back.
    visible_root: Option<ConnectionId>,
    /// What each root draws.
    info: HashMap<ConnectionId, RootInfo>,
    /// Children that have been fetched, or are being fetched.
    ///
    /// Only the nodes whose children are a round trip are in here; a schema's
    /// folders are answered from [`FOLDERS`] without a cache entry.
    children: HashMap<NodeId, ChildState<NodeId>>,
    /// The message an [`NodeId::Error`] row shows, keyed by the node that
    /// failed.
    errors: HashMap<NodeId, SharedString>,
    /// How many items a folder holds, once it has been loaded.
    counts: HashMap<NodeId, usize>,
    /// The catalog's comment on an object, for the objects that carry one.
    ///
    /// A side-map rather than a field of [`NodeId::Object`], for the reason the
    /// counts are one: the id is what the tree is keyed, selected and expanded
    /// by, and a comment edited in the database would otherwise make yesterday's
    /// selection name a node that no longer exists.
    remarks: HashMap<NodeId, SharedString>,
    /// Each connection's filter; a connection with none is drawn whole.
    ///
    /// Per connection for the reason the roots are: switching tabs swaps the
    /// tree, and the text typed over one database means nothing over another.
    filters: HashMap<ConnectionId, ExplorerFilter>,
    /// What a narrowed folder shows: the children its filter lets through.
    ///
    /// Kept beside [`ExplorerSource::children`] rather than written over it,
    /// so that changing the filter is a pass over what is already here and
    /// never a round trip. Present only for a loaded folder its connection's
    /// filter narrows; [`TreeSource::children`] answers from it first.
    shown: HashMap<NodeId, Vec<NodeId>>,
    /// The catalog and schema each connection's session landed in, which is
    /// the database its filter narrows unless told to narrow them all.
    landed: HashMap<ConnectionId, Scope>,
}

impl ExplorerSource {
    /// Adds a connection root, or updates the one already there.
    pub fn upsert_root(&mut self, connection: ConnectionId, info: RootInfo) {
        if !self.roots.contains(&connection) {
            self.roots.push(connection);
        }
        self.info.insert(connection, info);
    }

    /// Removes a connection root and everything fetched under it.
    ///
    /// Closing a tab has to take its subtree with it, or a node of a session
    /// that no longer exists would still be askable.
    pub fn remove_root(&mut self, connection: ConnectionId) {
        self.roots.retain(|root| *root != connection);
        self.info.remove(&connection);
        self.children
            .retain(|node, _| node.connection() != connection);
        self.errors
            .retain(|node, _| node.connection() != connection);
        self.counts
            .retain(|node, _| node.connection() != connection);
        self.remarks
            .retain(|node, _| node.connection() != connection);
        self.shown.retain(|node, _| node.connection() != connection);
        self.filters.remove(&connection);
        self.landed.remove(&connection);
    }

    /// The roots, in tab order — every open connection, drawn or not.
    ///
    /// Test-only: the panel draws from [`ExplorerSource::visible_roots`], and
    /// what the tests here have to be able to say is that the ones *not* drawn
    /// are still there.
    #[cfg(test)]
    pub fn roots(&self) -> &[ConnectionId] {
        &self.roots
    }

    /// Draws only `connection`'s root, or none at all with `None`.
    ///
    /// Called from every place the workspace's active connection changes. Purely
    /// a filter: nothing is fetched, invalidated or forgotten, so the tab this
    /// switches away from is exactly as the user left it when they come back.
    pub fn set_visible_root(&mut self, connection: Option<ConnectionId>) {
        self.visible_root = connection;
    }

    /// The roots the tree actually draws: at most one, and only while its
    /// connection is still open.
    ///
    /// A root whose tab closed between the workspace naming it and this being
    /// asked would otherwise be drawn out of a map it is no longer in, which is
    /// a blank row rather than an error.
    pub fn visible_roots(&self) -> Vec<ConnectionId> {
        self.visible_root
            .filter(|root| self.roots.contains(root))
            .into_iter()
            .collect()
    }

    /// Marks a node's children as being fetched.
    pub fn mark_loading(&mut self, node: NodeId) {
        self.shown.remove(&node);
        self.children.insert(node, ChildState::Loading);
    }

    /// Drops the whole cached subtree, including counts, errors and comments.
    pub fn invalidate(&mut self, node: &NodeId) {
        self.children.retain(|child, _| !node.contains(child));
        self.errors.retain(|child, _| !node.contains(child));
        self.counts.retain(|child, _| !node.contains(child));
        self.remarks.retain(|child, _| !node.contains(child));
        self.shown.retain(|child, _| !node.contains(child));
    }

    /// `connection`'s filter, or `None` while it has never had one.
    pub fn filter(&self, connection: ConnectionId) -> Option<&ExplorerFilter> {
        self.filters.get(&connection)
    }

    /// Replaces `connection`'s filter and narrows every folder it has loaded.
    ///
    /// Nothing is fetched and nothing is forgotten: the folders keep what
    /// they read, and a folder loaded later is narrowed as it arrives.
    pub fn set_filter(&mut self, connection: ConnectionId, filter: ExplorerFilter) {
        self.filters.insert(connection, filter);
        let folders: Vec<NodeId> = self
            .children
            .keys()
            .filter(|node| node.connection() == connection && matches!(node, NodeId::Folder { .. }))
            .cloned()
            .collect();
        for folder in folders {
            self.narrow(&folder);
        }
    }

    /// Records the catalog and schema `connection`'s session landed in.
    ///
    /// Narrows again whatever its filter has already narrowed, since which
    /// folders count as "the connected database" may have just changed.
    pub fn set_landed(&mut self, connection: ConnectionId, scope: Scope) {
        self.landed.insert(connection, scope);
        if let Some(filter) = self.filters.get(&connection).cloned() {
            self.set_filter(connection, filter);
        }
    }

    /// The name of the database `connection` landed in, for the filter's menu.
    pub fn landed_name(&self, connection: ConnectionId) -> Option<&str> {
        let landed = self.landed.get(&connection)?;
        landed.catalog.as_deref().or(landed.schema.as_deref())
    }

    /// The filter that narrows `node`, when one does: its connection's, when
    /// that names the folder's kind and — unless it narrows every database —
    /// the folder is in the database the session landed in.
    ///
    /// A connection whose landing is not known has every database narrowed,
    /// rather than none: a filter that did nothing would read as broken.
    fn narrowing(&self, node: &NodeId) -> Option<&ExplorerFilter> {
        let NodeId::Folder {
            connection,
            scope,
            folder,
        } = node
        else {
            return None;
        };
        let filter = self
            .filters
            .get(connection)
            .filter(|filter| filter.narrows(*folder))?;
        let in_scope = filter.all_databases()
            || self
                .landed
                .get(connection)
                .is_none_or(|landed| in_database(landed, scope));
        in_scope.then_some(filter)
    }

    /// Recomputes what one folder shows under its connection's filter.
    fn narrow(&mut self, node: &NodeId) {
        let filter = self.narrowing(node);
        let loaded = match self.children.get(node) {
            Some(ChildState::Loaded(ids)) if !self.errors.contains_key(node) => Some(ids),
            _ => None,
        };
        match (filter, loaded) {
            (Some(filter), Some(ids)) => {
                let shown = ids
                    .iter()
                    .filter(|id| match id {
                        NodeId::Object { name, scope, .. } => filter.admits(&Candidate {
                            name,
                            remarks: self.remarks.get(*id).map(SharedString::as_ref),
                            // A product with no schema level keeps its
                            // database in the catalog, and that is the name
                            // a user filtering by schema means.
                            schema: scope.schema.as_deref().or(scope.catalog.as_deref()),
                        }),
                        _ => true,
                    })
                    .cloned()
                    .collect();
                self.shown.insert(node.clone(), shown);
            }
            _ => {
                self.shown.remove(node);
            }
        }
    }

    /// How many of a narrowed folder's objects pass its filter, or `None` when
    /// the folder is not being narrowed — not loaded, or not one the filter
    /// names.
    pub fn matched(&self, node: &NodeId) -> Option<usize> {
        self.shown.get(node).map(Vec::len)
    }

    /// Whether a folder is worth opening to see what the filter found in it:
    /// one its connection's filter narrows that has a match, or that has not
    /// been read yet — or is being read — and so may have one.
    pub fn may_match(&self, node: &NodeId) -> bool {
        if self.narrowing(node).is_none() {
            return false;
        }
        match self.matched(node) {
            Some(count) => count > 0,
            // Read but not narrowed is a folder that failed: its error row is
            // what opening it would show, and that is no match.
            None => !matches!(self.children.get(node), Some(ChildState::Loaded(_))),
        }
    }

    /// Where the selection goes when the filter hides `selected`: the folder
    /// it was drawn in, or `None` while it is still on screen.
    ///
    /// The rule [`TreeView::collapse`] follows for a node closed over the
    /// selection, and for the same reason — but here there is a second one.
    /// The sidebar's chords and its menu act on the selection, and a table the
    /// user can no longer see is not one they meant `Ctrl+Enter` for.
    pub fn hidden_selection_folder(&self, selected: &NodeId) -> Option<NodeId> {
        if !matches!(selected, NodeId::Object { .. }) {
            return None;
        }
        let folder = selected.refresh_root();
        let shown = self.shown.get(&folder)?;
        (!shown.contains(selected)).then_some(folder)
    }

    /// The objects `connection`'s filter lets through, and how many it was
    /// choosing from, over every folder it narrows that has been read.
    pub fn filter_totals(&self, connection: ConnectionId) -> (usize, usize) {
        self.shown
            .iter()
            .filter(|(node, _)| node.connection() == connection)
            .fold((0, 0), |(shown, read), (node, ids)| {
                (
                    shown + ids.len(),
                    read + self.counts.get(node).copied().unwrap_or(ids.len()),
                )
            })
    }

    /// Records the children a fetch produced.
    pub fn set_children(&mut self, node: NodeId, children: Vec<Child>) {
        if matches!(node, NodeId::Folder { .. }) {
            self.counts.insert(node.clone(), children.len());
        }
        self.errors.remove(&node);
        let ids = children
            .into_iter()
            .map(|child| {
                match child.remarks {
                    Some(remarks) => self.remarks.insert(child.id.clone(), remarks),
                    // Removed rather than left: a comment dropped in the
                    // database has to come off the row that a refresh redraws.
                    None => self.remarks.remove(&child.id),
                };
                child.id
            })
            .collect();
        self.children.insert(node.clone(), ChildState::Loaded(ids));
        self.narrow(&node);
    }

    /// The catalog's comment on `id`, for the rows that carry one.
    #[cfg(test)]
    pub fn remarks_of(&self, id: &NodeId) -> Option<&SharedString> {
        self.remarks.get(id)
    }

    /// Records that a node could not be loaded.
    ///
    /// The node gets exactly one child — the error row — so the failure is where
    /// the user was looking and the rest of the tree is untouched.
    pub fn set_error(&mut self, node: NodeId, message: SharedString) {
        self.errors.insert(node.clone(), message);
        self.counts.remove(&node);
        self.shown.remove(&node);
        self.children.insert(
            node.clone(),
            ChildState::Loaded(vec![NodeId::Error(Box::new(node))]),
        );
    }

    /// The label of one node, without its icon or badge.
    pub fn label_of(&self, id: &NodeId) -> SharedString {
        match id {
            NodeId::Connection(connection) => self
                .info
                .get(connection)
                .map(|info| info.name.clone())
                .unwrap_or_else(|| ts!("explorer.unknown_connection")),
            NodeId::Catalog { name, .. } | NodeId::Schema { name, .. } => {
                SharedString::from(name.clone())
            }
            NodeId::Folder { folder, .. } => folder.label(),
            NodeId::Object { name, .. } => SharedString::from(name.clone()),
            NodeId::Error(parent) => self
                .errors
                .get(parent.as_ref())
                .cloned()
                .unwrap_or_else(|| ts!("explorer.load_failed_unknown")),
        }
    }
}

impl TreeSource for ExplorerSource {
    type Id = NodeId;

    fn children(&self, parent: Option<&NodeId>) -> ChildState<NodeId> {
        let Some(parent) = parent else {
            // The roots are always known: the workspace writes them as tabs open
            // and close, so there is nothing to fetch. Only the connection whose
            // tab is on top is answered with — see the module documentation.
            return ChildState::Loaded(
                self.visible_roots()
                    .into_iter()
                    .map(NodeId::Connection)
                    .collect(),
            );
        };
        match parent {
            // A schema's folders are the same five every time and cost no round
            // trip; their *contents* are what has to be fetched.
            NodeId::Schema {
                connection,
                catalog,
                name,
            } => ChildState::Loaded(folders_of(
                *connection,
                Scope {
                    catalog: catalog.clone(),
                    schema: Some(name.clone()),
                },
            )),
            NodeId::Object { .. } | NodeId::Error(_) => ChildState::Leaf,
            // A folder the filter narrows: only what passed. The full list
            // stays in `children`, for the next filter and the count.
            other if self.shown.contains_key(other) => {
                ChildState::Loaded(self.shown.get(other).cloned().unwrap_or_default())
            }
            other => self
                .children
                .get(other)
                .cloned()
                .unwrap_or(ChildState::NotLoaded),
        }
    }

    fn has_children(&self, id: &NodeId) -> bool {
        !matches!(id, NodeId::Object { .. } | NodeId::Error(_))
    }

    fn render_row(
        &self,
        id: &NodeId,
        info: TreeRowInfo,
        _window: &mut Window,
        cx: &mut App,
    ) -> AnyElement {
        let chrome = theme(cx);
        let label = self.label_of(id);

        // The error row is the one that is not an object: no icon of its own, a
        // danger tint, and the driver's whole sentence.
        if let NodeId::Error(_) = id {
            return div()
                .flex()
                .flex_row()
                .items_center()
                .gap(px(6.))
                .min_w_0()
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .text_size(rugpui::TextSize::Xs.size(cx))
                        .text_color(chrome.danger)
                        .child(label),
                )
                .into_any_element();
        }

        let mark = match id {
            NodeId::Connection(connection) => {
                return self
                    .render_root(*connection, label, &chrome, cx)
                    .into_any_element();
            }
            NodeId::Catalog { .. } | NodeId::Schema { .. } => icons::SCHEMA,
            NodeId::Folder { .. } => icons::FOLDER,
            NodeId::Object { folder, .. } => folder.icon(),
            NodeId::Error(_) => unreachable!("handled above"),
        };

        // Only a loaded folder gets a count: a badge that read "0" while the
        // fetch was still out would be a wrong answer rather than a missing one.
        // A folder the filter narrows reads "found/all", in the accent while it
        // found something — the one place the tree says it is not whole.
        let matched = self.matched(id);
        let badge = self.counts.get(id).map(|count| {
            let (text, color) = match matched {
                Some(found) => (
                    format!("{found}/{count}"),
                    if found > 0 {
                        chrome.accent
                    } else {
                        chrome.text_muted
                    },
                ),
                None => (count.to_string(), chrome.text_muted),
            };
            div()
                .flex_none()
                .text_size(rugpui::scaled(10., cx))
                .text_color(color)
                .child(SharedString::from(text))
        });
        // A narrowed folder with nothing in it is drawn quieter, so the eye
        // goes to the ones that did find something.
        let quiet = matched == Some(0);

        // The catalog's comment, after the name and quieter than it — the same
        // treatment the folder count gets, and for the same reason: it is a
        // second fact about the row rather than a second row. Never translated,
        // because it is the database's own text.
        //
        // It is cut to keep the name readable, so the whole of it has to be
        // reachable somewhere; the tooltip hangs off the comment itself rather
        // than off the row, which is both where the reader is already pointing
        // and the one part of the row every kind of object has.
        let subtitle = self.remarks.get(id).map(|remarks| {
            div()
                .id(("tree-remarks", info.index))
                .flex_none()
                .max_w(rugpui::scaled(160., cx))
                .truncate()
                .text_size(rugpui::scaled(10., cx))
                .text_color(chrome.text_muted)
                .tooltip(rugpui::tooltip_label(remarks.clone()))
                .child(remarks.clone())
        });

        let body = div()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(6.))
            .min_w_0()
            .child(icons::icon(
                mark,
                rugpui::scaled(14., cx),
                chrome.text_muted,
            ))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .when(quiet, |label| label.text_color(chrome.text_muted))
                    .when(info.selected, |label| label.text_color(chrome.text))
                    .child(label),
            )
            .children(subtitle)
            .children(badge);

        // Only a table or a view can be dragged onto a query builder, which is
        // the gate the "add to builder" action applies too: the canvas is a
        // picture of columns, and a routine has none to draw. A drag that gets
        // going swallows the row's click, so the selection does not move under
        // a table on its way to the canvas.
        let Some(dragged) = id.as_target().filter(|target| target.folder.is_relation()) else {
            return body.into_any_element();
        };
        let icon = dragged.folder.icon();
        let label = SharedString::from(dragged.qualified());
        body.id(("tree-drag", info.index))
            // Compiled away outside a test build. It marks the element the
            // drag hangs off, which is the one a test has to press on, and
            // saves it working the row's position out from the indent, the row
            // height and the panel's header.
            .debug_selector({
                let label = label.clone();
                move || format!("{DRAG_SELECTOR}{label}")
            })
            .on_drag(
                DraggedObject(dragged),
                move |_dragged, offset, _window, cx| {
                    cx.new(|_cx| DragGhost {
                        offset,
                        icon,
                        label: label.clone(),
                    })
                },
            )
            .into_any_element()
    }

    fn render_loading(&self, _window: &mut Window, cx: &mut App) -> AnyElement {
        div()
            .text_size(rugpui::TextSize::Xs.size(cx))
            .text_color(theme(cx).text_muted)
            .child(ts!("explorer.loading"))
            .into_any_element()
    }
}

impl ExplorerSource {
    /// The root row: a status dot in the profile's color, then its name.
    fn render_root(
        &self,
        connection: ConnectionId,
        label: SharedString,
        chrome: &Theme,
        cx: &App,
    ) -> impl IntoElement + use<> {
        let info = self.info.get(&connection);
        let live = info.is_some_and(|info| info.live);
        let dot = info
            .and_then(|info| info.color.as_deref())
            .and_then(rugpui::parse_hex)
            .unwrap_or(if live {
                chrome.success
            } else {
                chrome.text_muted
            });

        div()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(6.))
            .min_w_0()
            .child(
                div()
                    .flex_none()
                    .size(rugpui::scaled(7., cx))
                    .rounded_full()
                    .bg(dot),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_color(if live { chrome.text } else { chrome.text_muted })
                    .child(label),
            )
    }
}

/// Whether a folder of `scope` is in the database a session landed in.
///
/// "The database" is the catalog where the product has catalogs — every
/// schema of the PostgreSQL or SQL Server database connected to, the MySQL
/// database itself — and the schema where it has none, which is Oracle's
/// user. A side that does not name a level — a catalog level that was
/// skipped, a product with no schemas — does not rule the folder out. Names
/// are compared without regard to ASCII case, because `getCatalog` and
/// `getCatalogs` may fold them differently.
fn in_database(landed: &Scope, scope: &Scope) -> bool {
    let same = |a: &Option<String>, b: &Option<String>| match (a, b) {
        (Some(a), Some(b)) => a.eq_ignore_ascii_case(b),
        _ => true,
    };
    if landed.catalog.is_some() {
        same(&landed.catalog, &scope.catalog)
    } else {
        same(&landed.schema, &scope.schema)
    }
}

/// The five folder nodes of one scope.
pub fn folders_of(connection: ConnectionId, scope: Scope) -> Vec<NodeId> {
    FOLDERS
        .iter()
        .map(|folder| NodeId::Folder {
            connection,
            scope: scope.clone(),
            folder: *folder,
        })
        .collect()
}

/// Reads the children of one node from the database.
///
/// **Blocks.** Called from `cx.background_spawn` with a
/// [`SessionHandle`](crate::connection::SessionHandle), which is what keeps the
/// session alive for as long as the query takes even if the tab closes under it.
///
/// This is where the layer-skipping rule of the module documentation is
/// applied, and it can only be applied here: whether a level is worth a row
/// depends on how many members it turned out to have, which is not knowable
/// until it has been asked for. A level of one or none is therefore not
/// returned — the fetch goes on to the next level in the same call, so opening a
/// connection on a one-schema database lands directly on the folders.
///
/// `hide` is the profile's hide lists, and they are applied *before* a level is
/// counted: a database whose other schemas are all hidden lands on its folders
/// as one that never had them would. A level that had members and has none
/// left once they are taken out is empty rather than skipped — skipping it
/// would read the level under it unqualified, which is what a product with no
/// such level gets, and would show the very things that were hidden.
pub fn load_children(
    session: &Session,
    node: &NodeId,
    hide: &HideRules,
) -> Result<Vec<Child>, String> {
    match node {
        NodeId::Connection(connection) => {
            let listed = names(session, &DescribeRequest::new("catalogs"))?;
            let had_any = !listed.is_empty();
            let catalogs: Vec<_> = listed
                .into_iter()
                .filter(|(name, _)| !hide.hides_catalog(name))
                .collect();
            if catalogs.len() > 1 {
                return Ok(catalogs
                    .into_iter()
                    .map(|(name, _)| {
                        Child::plain(NodeId::Catalog {
                            connection: *connection,
                            name,
                        })
                    })
                    .collect());
            }
            if had_any && catalogs.is_empty() {
                return Ok(Vec::new());
            }
            // One catalog or none: it becomes part of the scope rather than a
            // row of its own.
            next_level(
                session,
                *connection,
                catalogs.into_iter().next().map(|(name, _)| name),
                hide,
            )
        }
        NodeId::Catalog { connection, name } => {
            next_level(session, *connection, Some(name.clone()), hide)
        }
        NodeId::Folder {
            connection,
            scope,
            folder,
        } => {
            let mut request = DescribeRequest::new(folder.describe_kind());
            request.catalog = scope.catalog.clone();
            request.schema = scope.schema.clone();
            if let Some(types) = folder.table_types() {
                request.types = Some(types.iter().map(|kind| (*kind).to_string()).collect());
            }
            Ok(names(session, &request)?
                .into_iter()
                .filter(|(name, _)| !hide.hides_object(*folder, name))
                .map(|(name, remarks)| Child {
                    id: NodeId::Object {
                        connection: *connection,
                        scope: scope.clone(),
                        folder: *folder,
                        name,
                    },
                    remarks,
                })
                .collect())
        }
        // A schema's folders never reach here — `children` answers them from
        // `FOLDERS` — and an object and an error row are leaves.
        NodeId::Schema { .. } | NodeId::Object { .. } | NodeId::Error(_) => Ok(Vec::new()),
    }
}

/// The schemas of one catalog, or its folders when there is at most one.
///
/// The second half of the layer-skipping rule. A product with no schemas at all
/// — SQLite — and one with exactly one both land on the folders, with whatever
/// single schema there was folded into the scope so that the queries underneath
/// stay qualified. The hidden schemas are taken out before they are counted;
/// see [`load_children`].
fn next_level(
    session: &Session,
    connection: ConnectionId,
    catalog: Option<String>,
    hide: &HideRules,
) -> Result<Vec<Child>, String> {
    let mut request = DescribeRequest::new("schemas");
    request.catalog = catalog.clone();
    let listed = names(session, &request)?;
    let had_any = !listed.is_empty();
    let schemas: Vec<_> = listed
        .into_iter()
        .filter(|(name, _)| !hide.hides_schema(name))
        .collect();

    if schemas.len() > 1 {
        return Ok(schemas
            .into_iter()
            .map(|(name, _)| {
                Child::plain(NodeId::Schema {
                    connection,
                    catalog: catalog.clone(),
                    name,
                })
            })
            .collect());
    }
    if had_any && schemas.is_empty() {
        return Ok(Vec::new());
    }
    Ok(folders_of(
        connection,
        Scope {
            catalog,
            schema: schemas.into_iter().next().map(|(name, _)| name),
        },
    )
    .into_iter()
    .map(Child::plain)
    .collect())
}

/// Runs a `DESCRIBE` and collects the `name` and the comment of every item.
///
/// An item with no name is dropped rather than drawn as a blank row: a driver
/// that answers `getSchemas` with a null `TABLE_SCHEM` — some do, for the
/// default schema — would otherwise put an unclickable empty line in the tree.
///
/// The comment comes along for every kind, and is `None` for every kind but the
/// objects: only `getTables` carries a `REMARKS`, and a level that has none
/// simply has none rather than needing a second call to find that out.
fn names(
    session: &Session,
    request: &DescribeRequest,
) -> Result<Vec<(String, Option<SharedString>)>, String> {
    let result = session.describe(request).map_err(describe_failure)?;
    Ok(result
        .items
        .into_iter()
        .filter_map(|item| {
            let name = item
                .get("name")
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned)
                .filter(|name| !name.is_empty())?;
            let remarks = item
                .get("remarks")
                .and_then(serde_json::Value::as_str)
                .map(str::trim)
                .filter(|remarks| !remarks.is_empty())
                .map(|remarks| SharedString::from(remarks.to_owned()));
            Some((name, remarks))
        })
        .collect())
}

/// The catalog or schema among `children` that `target` names.
///
/// By the exact name first, and failing that by the name in any case: the
/// session reports where it landed through `getCatalog`/`getSchema` and the
/// tree lists through `getCatalogs`/`getSchemas`, and a driver is free to fold
/// the case differently between the two. Folders match nothing — a level that
/// is already the folders is the end of the path.
fn revealed_child(children: &[Child], target: &Scope) -> Option<NodeId> {
    let find = |same: fn(&str, &str) -> bool| {
        children
            .iter()
            .map(|child| &child.id)
            .find(|id| {
                let (name, want) = match id {
                    NodeId::Catalog { name, .. } => (name, &target.catalog),
                    NodeId::Schema { name, .. } => (name, &target.schema),
                    _ => return false,
                };
                want.as_deref().is_some_and(|want| same(name, want))
            })
            .cloned()
    };
    find(|name, want| name == want).or_else(|| find(str::eq_ignore_ascii_case))
}

/// Turns a JNI-layer failure into the sentence the error row shows.
///
/// The bridge's own message, never a stack trace: a metadata refusal is
/// something the user can act on — a permission, a catalog the product will
/// not open — and the driver says it better than anything invented here.
pub fn describe_failure(error: JdbcError) -> String {
    match error {
        JdbcError::Bridge(bridge) => {
            let mut message = bridge.message.clone();
            if let Some(cause) = bridge.causes.first() {
                message.push_str(" — ");
                message.push_str(cause);
            }
            message
        }
        other => other.to_string(),
    }
}

/// The names of the nodes among `nodes` that name something, one per line.
///
/// See [`Explorer::copy_names`].
pub fn names_of(nodes: &[NodeId]) -> String {
    nodes
        .iter()
        .filter_map(|node| match node {
            NodeId::Catalog { name, .. }
            | NodeId::Schema { name, .. }
            | NodeId::Object { name, .. } => Some(name.as_str()),
            NodeId::Connection(_) | NodeId::Folder { .. } | NodeId::Error(_) => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// What the explorer asks the workspace for.
pub enum ExplorerEvent {
    /// The children of `node` are wanted; the workspace has the session.
    Load {
        /// The node whose children are wanted.
        node: NodeId,
        /// Identifies this request so a refresh can discard older replies.
        generation: u64,
    },
    /// An object was activated and should be opened in a pane.
    Activated(Box<ObjectTarget>),
    /// A row was right-clicked and wants its menu drawn over it.
    ///
    /// Promoted from the tree's own event and not answered here: the rows of
    /// this panel offer the workspace's five object commands, and every one of
    /// them needs a session, a pane tree or a dialog — none of which the
    /// sidebar has (architecture document, §7.8). The tree has already moved
    /// the selection onto the node, so the menu the workspace builds and the
    /// row the user is looking at name the same thing.
    ContextMenu {
        /// The node the pointer went down on, or the selected one.
        node: NodeId,
        /// Where the pointer was, in window coordinates, or — from the
        /// keyboard — a point just inside the top of the tree.
        position: Point<Pixels>,
        /// Whether `Shift+F10` or the Menu key asked, so the menu opens on its
        /// first row for the arrow keys to start from.
        by_keyboard: bool,
    },
}

gpui::actions!(
    explorer,
    [
        /// Put the keyboard in the filter box.
        FocusFilter,
        /// Empty the filter box, or hand the keyboard back to the tree when
        /// there is nothing in it.
        ClearFilter,
        /// Read the filter box as a regular expression, or as its own terms.
        ToggleRegex,
        /// Put the names of the selected rows on the clipboard, one per line.
        CopyNames,
    ]
);

/// Key context of the filter box, under [`KEY_CONTEXT`].
///
/// Its own, so that `Escape` there clears the box rather than reaching the
/// window, which would close whatever dialog it thinks is open.
const FILTER_KEY_CONTEXT: &str = "ExplorerFilter";

/// The chord that puts the keyboard in the filter box, as the tooltip spells
/// it.
const FILTER_CHORD: &str = if cfg!(target_os = "macos") {
    "Cmd+F"
} else {
    "Ctrl+F"
};

/// Marks the sidebar's own chords as registered, once per application.
struct Bindings;

impl gpui::Global for Bindings {}

/// Registers the filter's chords, once per application.
///
/// `Ctrl+F` is free in the sidebar — the tree has no find of its own — and
/// bound only there, so the SQL editor's find is untouched.
fn bind_keys(cx: &mut App) {
    if cx.has_global::<Bindings>() {
        return;
    }
    let modifier = if cfg!(target_os = "macos") {
        "cmd"
    } else {
        "ctrl"
    };
    cx.bind_keys([
        gpui::KeyBinding::new(&format!("{modifier}-f"), FocusFilter, Some(KEY_CONTEXT)),
        // Only reaches here from the tree: the filter box binds its own copy,
        // on a context inside this one.
        gpui::KeyBinding::new(&format!("{modifier}-c"), CopyNames, Some(KEY_CONTEXT)),
        gpui::KeyBinding::new("escape", ClearFilter, Some(FILTER_KEY_CONTEXT)),
        // `Alt+R`, as the find bars of most editors have it.
        gpui::KeyBinding::new("alt-r", ToggleRegex, Some(FILTER_KEY_CONTEXT)),
    ]);
    cx.set_global(Bindings);
}

/// What the filter's menu button says: which folders it narrows.
fn targets_label(targets: FolderSet) -> SharedString {
    if targets.is_all() {
        return ts!("explorer.filter_all");
    }
    let count = targets.iter().count();
    match targets.iter().next() {
        None => ts!("explorer.filter_none"),
        Some(first) if count == 1 => first.label(),
        Some(first) => ts!(
            "explorer.filter_more",
            first = first.label(),
            count = count - 1
        ),
    }
}

/// How a history entry reads in the menu: what the box held, the conditions
/// spelled out, and the folders when they were not all of them.
fn history_label(entry: &FilterSnapshot) -> SharedString {
    let mut parts = Vec::new();
    if !entry.text.is_empty() {
        // An expression is set off the way one is usually written, so it is
        // not taken for the box's terms.
        parts.push(if entry.regex {
            format!("/{}/", entry.text)
        } else {
            entry.text.clone()
        });
    }
    if !entry.conditions.is_empty() {
        parts.push(entry.conditions_summary());
    }
    if let Some(folders) = &entry.targets {
        let targets = folders
            .iter()
            .fold(FolderSet::NONE, |set, folder| set.toggled(*folder));
        parts.push(format!("[{}]", targets_label(targets)));
    }
    if entry.all_databases {
        parts.push(format!("[{}]", ts!("explorer.scope_all")));
    }
    SharedString::from(parts.join("  \u{b7}  "))
}

/// A button whose menu hangs under it: drawn where `bounds` can see it, so
/// that the menu knows where that is.
///
/// While its menu is `open` the button has no tooltip: the pointer is still
/// resting on it after the click, and the tooltip that rest brings up would
/// sit over the menu's first row. A `note` goes on a line of its own under the
/// tooltip's text, since a tooltip's line never wraps.
fn menu_button(
    id: &'static str,
    tooltip: SharedString,
    note: Option<SharedString>,
    label: String,
    open: bool,
    bounds: Rc<Cell<Bounds<Pixels>>>,
    on_click: impl Fn(&mut Window, &mut App) + 'static,
) -> impl IntoElement {
    div()
        .id(id)
        .relative()
        .flex_none()
        .when(!open, |this| match note {
            Some(note) => this.tooltip(rugpui::Tooltip::new().text(tooltip).note(note).build()),
            None => this.tooltip(rugpui::tooltip_label(tooltip)),
        })
        .child(
            Button::new((id, 0usize), label)
                .compact()
                .variant(ButtonVariant::Secondary)
                .on_click(move |_, window, cx| on_click(window, cx)),
        )
        .child(
            gpui::canvas(move |drawn, _, _| bounds.set(drawn), |_, _, _, _| {})
                .absolute()
                .size_full(),
        )
}

/// The sidebar view.
pub struct Explorer {
    tree: Entity<TreeView<ExplorerSource>>,
    focus_handle: FocusHandle,
    /// Where the tree was drawn last, for the menu `Shift+F10` opens: the
    /// tree does not say where its rows are, so the menu hangs just inside
    /// its top edge.
    tree_bounds: Rc<Cell<Bounds<Pixels>>>,
    generation: u64,
    pending: HashMap<NodeId, u64>,
    /// The connections whose tree is still opening onto the scope their
    /// session landed in; see [`Explorer::reveal`].
    reveals: HashMap<ConnectionId, Reveal>,
    /// The filter box over the tree.
    ///
    /// One box for every connection: the text it holds is each connection's
    /// own, swapped in as the tab on top changes — see [`Explorer::show_root`].
    filter_input: Entity<TextInput>,
    /// Whether the menu of the folders the filter narrows is out.
    targets_open: bool,
    /// Set by a row of that menu, so that the dismissal every activation is
    /// followed by leaves it open. The menu is a row of switches, and ticking
    /// three of them should not take opening it three times.
    targets_kept: bool,
    /// Where the menu's button was drawn last, for the menu to hang under.
    targets_bounds: Rc<Cell<Bounds<Pixels>>>,
    /// The profile each connection was opened from, which its filter history
    /// is kept under; see [`Explorer::set_profile`].
    profiles: HashMap<ConnectionId, Uuid>,
    /// Each connection's hide lists, compiled, with the lists they were
    /// compiled from; see [`Explorer::set_hide`].
    hides: HashMap<ConnectionId, (rudbman_core::ExplorerHide, Arc<HideRules>)>,
    /// Whether the history menu is out.
    history_open: bool,
    /// Where its button was drawn last.
    history_bounds: Rc<Cell<Bounds<Pixels>>>,
    /// Whether the conditions panel is showing.
    conditions_open: bool,
    /// The panel's rows, one per condition of the filter on screen, in order.
    rows: Vec<ConditionRow>,
    /// The key the next row gets.
    next_row: usize,
    /// The row whose field (`true`) or comparison (`false`) list is out.
    open_select: Option<(usize, bool)>,
    /// Keeps the tree's subscription alive.
    _events: Subscription,
    /// Keeps the filter box's subscription alive.
    _filter_events: Subscription,
}

/// One row of the conditions panel, as drawn: the field and the comparison
/// are read from the filter, the value lives in its own box.
struct ConditionRow {
    /// What the row's widgets are keyed by. The row's place changes as rows
    /// above it are removed; this does not.
    key: usize,
    /// The value box.
    input: Entity<TextInput>,
    /// Keeps the box's subscription alive.
    _edits: Subscription,
}

/// A walk from a connection's root down to the catalog and schema its session
/// landed in, one level per delivery.
struct Reveal {
    /// The scope the walk ends at.
    target: Scope,
    /// The node whose children the walk is waiting for — the last one it
    /// opened.
    awaiting: NodeId,
}

impl Explorer {
    /// Builds the panel around an empty tree.
    pub fn new(cx: &mut Context<Self>) -> Self {
        let tree = cx.new(|cx| {
            TreeView::new(ExplorerSource::default(), cx)
                .with_arrow_icons(icons::CHEVRON_RIGHT, icons::CHEVRON_DOWN)
                // Several tables at once, for the commands that make sense on
                // a list of them: copying their names or their statements.
                .multi_select(true)
        });
        let events = cx.subscribe(&tree, |explorer, _tree, event, cx| match event {
            // The widget deduplicates these, so a node is asked for once
            // however many times it is redrawn.
            TreeEvent::LoadChildren(Some(node)) => {
                explorer.request(node.clone(), cx);
            }
            // The root level is the open tabs, which the workspace writes
            // directly; there is nothing to fetch.
            TreeEvent::LoadChildren(None) => {}
            TreeEvent::Activated(node) => {
                if let Some(target) = node.as_target() {
                    cx.emit(ExplorerEvent::Activated(Box::new(target)));
                }
            }
            TreeEvent::SelectionChanged(_) => {}
            // Straight on to the workspace. The selection has already moved
            // here, so the node in the event and the highlighted row are the
            // same one.
            TreeEvent::ContextMenu { id, position } => {
                cx.emit(ExplorerEvent::ContextMenu {
                    node: id.clone(),
                    position: *position,
                    by_keyboard: false,
                });
            }
        });

        bind_keys(cx);
        let this = cx.weak_entity();
        let filter_input = cx.new(|cx| {
            TextInput::new(cx)
                .placeholder(ts!("explorer.filter_placeholder"))
                .on_submit(move |_text, window, cx| {
                    let _ = this.update(cx, |explorer, cx| explorer.open_matches(window, cx));
                })
        });
        // The box owns the text and the source owns the filter, so the one has
        // to tell the other — on every keystroke, as rudbgen's box does: this
        // narrows what is already here, so there is nothing to wait for.
        let filter_events = cx.observe(&filter_input, |explorer, input, cx| {
            let text = input.read(cx).content().to_string();
            explorer.set_filter_text(text, cx);
        });

        Self {
            tree,
            focus_handle: cx.focus_handle(),
            tree_bounds: Rc::default(),
            generation: 0,
            pending: HashMap::new(),
            reveals: HashMap::new(),
            filter_input,
            targets_open: false,
            targets_kept: false,
            targets_bounds: Rc::default(),
            profiles: HashMap::new(),
            hides: HashMap::new(),
            history_open: false,
            history_bounds: Rc::default(),
            conditions_open: false,
            rows: Vec::new(),
            next_row: 0,
            open_select: None,
            _events: events,
            _filter_events: filter_events,
        }
    }

    /// Tells the explorer which profile `connection` was opened from: what its
    /// filter history is kept under, so that every tab on one profile offers
    /// the same filters, run after run.
    pub fn set_profile(&mut self, connection: ConnectionId, profile: Uuid) {
        self.profiles.insert(connection, profile);
    }

    /// Gives `connection` its profile's hide lists, and says whether they
    /// differ from the ones it had — in which case what was read under the old
    /// ones is out of date, and the caller reloads the root.
    pub fn set_hide(
        &mut self,
        connection: ConnectionId,
        hide: &rudbman_core::ExplorerHide,
    ) -> bool {
        if self
            .hides
            .get(&connection)
            .is_some_and(|(current, _)| current == hide)
        {
            return false;
        }
        let had = self.hides.contains_key(&connection);
        self.hides
            .insert(connection, (hide.clone(), Arc::new(HideRules::new(hide))));
        // The first lists a connection is given are not a change: nothing has
        // been read under any others.
        had
    }

    /// The hide lists every read under `connection` is to apply.
    pub fn hide_rules(&self, connection: ConnectionId) -> Arc<HideRules> {
        self.hides
            .get(&connection)
            .map(|(_, rules)| rules.clone())
            .unwrap_or_default()
    }

    /// Writes the filter of `root` into its profile's history, when there is
    /// something in it to keep.
    fn record_history(&self, root: ConnectionId, cx: &mut App) {
        let Some(profile) = self.profiles.get(&root).copied() else {
            return;
        };
        let Some(snapshot) = self
            .tree
            .read(cx)
            .source()
            .filter(root)
            .map(ExplorerFilter::snapshot)
        else {
            return;
        };
        FilterHistory::record(cx, profile, snapshot);
    }

    /// What `connection`'s filter is saved as with its profile's workspace,
    /// or `None` while it lets through what a new tab's would.
    pub fn saved_filter(&self, connection: ConnectionId, cx: &App) -> Option<FilterSnapshot> {
        let snapshot = self.tree.read(cx).source().filter(connection)?.snapshot();
        (snapshot != ExplorerFilter::default().snapshot()).then_some(snapshot)
    }

    /// Gives `connection` the filter its profile's workspace was saved with —
    /// the one left on the tree when the profile's tab last closed — with the
    /// box and the conditions panel's rows following it when the tree is on
    /// screen. The panel stays shut: the Conditions button already says how
    /// many there are.
    pub fn restore_filter(
        &mut self,
        connection: ConnectionId,
        snapshot: &FilterSnapshot,
        cx: &mut Context<Self>,
    ) {
        self.apply_filter(connection, ExplorerFilter::from_snapshot(snapshot), cx);
        if self.visible_root(cx) == Some(connection) {
            if self.filter_input.read(cx).content() != snapshot.text {
                let text = snapshot.text.clone();
                self.filter_input
                    .update(cx, |input, cx| input.set_content(text, cx));
            }
            self.rebuild_rows(cx);
        }
        cx.notify();
    }

    /// Puts a filter from the history back: its text, its folders and its
    /// conditions, with the panel open when it has any.
    fn use_history(&mut self, snapshot: &FilterSnapshot, cx: &mut Context<Self>) {
        let Some(root) = self.visible_root(cx) else {
            return;
        };
        self.apply_filter(root, ExplorerFilter::from_snapshot(snapshot), cx);
        if self.filter_input.read(cx).content() != snapshot.text {
            let text = snapshot.text.clone();
            self.filter_input
                .update(cx, |input, cx| input.set_content(text, cx));
        }
        self.rebuild_rows(cx);
        self.conditions_open = !snapshot.conditions.is_empty();
        // Used again, so it goes back to the top.
        self.record_history(root, cx);
        cx.notify();
    }

    /// A row for the panel, its box holding `value`.
    fn new_row(&mut self, value: &str, cx: &mut Context<Self>) -> ConditionRow {
        let key = self.next_row;
        self.next_row += 1;
        let this = cx.weak_entity();
        let value = value.to_string();
        let input = cx.new(|cx| {
            let mut input = TextInput::new(cx)
                .placeholder(ts!("explorer.condition_value"))
                .on_submit(move |_text, window, cx| {
                    let _ = this.update(cx, |explorer, cx| explorer.open_matches(window, cx));
                });
            input.set_content(value, cx);
            input
        });
        let edits = cx.observe(&input, move |explorer, input, cx| {
            let value = input.read(cx).content().to_string();
            explorer.edit_condition(key, cx, |condition| condition.value = value);
        });
        ConditionRow {
            key,
            input,
            _edits: edits,
        }
    }

    /// Builds the panel's rows afresh from the filter on screen.
    fn rebuild_rows(&mut self, cx: &mut Context<Self>) {
        let conditions = self
            .visible_filter(cx)
            .map(|(_, filter)| filter.conditions().to_vec())
            .unwrap_or_default();
        let mut rows = Vec::with_capacity(conditions.len());
        for condition in &conditions {
            rows.push(self.new_row(&condition.value, cx));
        }
        self.rows = rows;
        self.open_select = None;
        self.mark_invalid(cx);
    }

    /// Marks the filter box, and the value box of every row, whose
    /// expression does not compile.
    fn mark_invalid(&self, cx: &mut Context<Self>) {
        let Some((_, filter)) = self.visible_filter(cx) else {
            return;
        };
        let text_invalid = filter.text_invalid();
        self.filter_input
            .update(cx, |input, cx| input.set_invalid(text_invalid, cx));
        for (index, row) in self.rows.iter().enumerate() {
            let invalid = filter.is_invalid(index);
            row.input
                .update(cx, |input, cx| input.set_invalid(invalid, cx));
        }
    }

    /// Changes the condition drawn by row `key`, and narrows by the result.
    fn edit_condition(
        &mut self,
        key: usize,
        cx: &mut Context<Self>,
        edit: impl FnOnce(&mut Condition),
    ) {
        let Some(index) = self.rows.iter().position(|row| row.key == key) else {
            return;
        };
        let Some((root, mut filter)) = self.visible_filter(cx) else {
            return;
        };
        let mut conditions = filter.conditions().to_vec();
        let Some(condition) = conditions.get_mut(index) else {
            return;
        };
        let before = condition.clone();
        edit(condition);
        if *condition == before {
            return;
        }
        filter.set_conditions(filter.combine(), conditions);
        self.apply_filter(root, filter, cx);
    }

    /// Adds an empty row at the bottom of the panel and puts the keyboard in
    /// it.
    fn add_condition(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some((root, mut filter)) = self.visible_filter(cx) else {
            return;
        };
        let mut conditions = filter.conditions().to_vec();
        conditions.push(Condition::default());
        filter.set_conditions(filter.combine(), conditions);
        let row = self.new_row("", cx);
        let handle = row.input.read(cx).focus_handle(cx);
        self.rows.push(row);
        self.apply_filter(root, filter, cx);
        window.focus(&handle, cx);
    }

    /// Takes row `key` out of the panel, and its condition out of the filter.
    fn remove_condition(&mut self, key: usize, cx: &mut Context<Self>) {
        let Some(index) = self.rows.iter().position(|row| row.key == key) else {
            return;
        };
        let Some((root, mut filter)) = self.visible_filter(cx) else {
            return;
        };
        let mut conditions = filter.conditions().to_vec();
        if index < conditions.len() {
            conditions.remove(index);
        }
        self.rows.remove(index);
        self.open_select = None;
        filter.set_conditions(filter.combine(), conditions);
        self.apply_filter(root, filter, cx);
    }

    /// Empties the panel.
    fn clear_conditions(&mut self, cx: &mut Context<Self>) {
        let Some((root, mut filter)) = self.visible_filter(cx) else {
            return;
        };
        filter.set_conditions(filter.combine(), Vec::new());
        self.rows.clear();
        self.open_select = None;
        self.apply_filter(root, filter, cx);
    }

    /// Puts the filter of the connection on screen back the way a new tab
    /// starts it: the box empty and read as terms, every folder, the landed
    /// database only, no conditions and the panel shut.
    ///
    /// The history is left alone, and is given the filter that was on first:
    /// a reset is the user done with that filter, as leaving the tab is, and
    /// what it threw away is then one pick from the history away.
    fn reset_filter(&mut self, cx: &mut Context<Self>) {
        let Some(root) = self.visible_root(cx) else {
            return;
        };
        self.record_history(root, cx);
        self.apply_filter(root, ExplorerFilter::default(), cx);
        // After the filter, so the box's own notice finds nothing to change.
        if !self.filter_input.read(cx).content().is_empty() {
            self.filter_input.update(cx, |input, cx| input.clear(cx));
        }
        self.rows.clear();
        self.open_select = None;
        self.conditions_open = false;
        self.targets_open = false;
        self.history_open = false;
        cx.notify();
    }

    /// Whether the filter on screen is already the one a reset would leave.
    fn filter_is_reset(&self, cx: &App) -> bool {
        self.visible_filter(cx)
            .is_none_or(|(_, filter)| filter == ExplorerFilter::default())
    }

    /// Puts the conditions together the other way.
    fn set_combine(&mut self, combine: Combine, cx: &mut Context<Self>) {
        let Some((root, mut filter)) = self.visible_filter(cx) else {
            return;
        };
        if filter.combine() == combine {
            return;
        }
        let conditions = filter.conditions().to_vec();
        filter.set_conditions(combine, conditions);
        self.apply_filter(root, filter, cx);
    }

    /// Opens or closes the conditions panel.
    ///
    /// Opening an empty one starts a row, since a panel is opened to write
    /// one. Closing it is the user done with the conditions, which is when
    /// they are worth keeping in the history.
    fn toggle_conditions(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.conditions_open {
            self.conditions_open = false;
            self.open_select = None;
            if let Some(root) = self.visible_root(cx) {
                self.record_history(root, cx);
            }
        } else {
            self.conditions_open = true;
            if self.rows.is_empty() {
                self.add_condition(window, cx);
            }
        }
        cx.notify();
    }

    /// The connection whose tree is on screen.
    fn visible_root(&self, cx: &App) -> Option<ConnectionId> {
        self.tree
            .read(cx)
            .source()
            .visible_roots()
            .into_iter()
            .next()
    }

    /// The filter of the connection on screen, as it stands, and that
    /// connection.
    fn visible_filter(&self, cx: &App) -> Option<(ConnectionId, ExplorerFilter)> {
        let root = self.visible_root(cx)?;
        let filter = self
            .tree
            .read(cx)
            .source()
            .filter(root)
            .cloned()
            .unwrap_or_default();
        Some((root, filter))
    }

    /// Draws the tree of `connection`, or none, and puts its filter in the box.
    ///
    /// What the workspace calls whenever the tab on top changes. The box is
    /// shared, so the text it holds has to follow the tree it filters.
    pub fn show_root(&mut self, connection: Option<ConnectionId>, cx: &mut Context<Self>) {
        let previous = self.visible_root(cx);
        if previous == connection {
            return;
        }
        // Leaving a tab is done with its filter, which is worth keeping.
        if let Some(previous) = previous {
            self.record_history(previous, cx);
        }
        self.update_source(cx, |source| source.set_visible_root(connection));
        let text = connection
            .and_then(|root| self.tree.read(cx).source().filter(root))
            .map(|filter| filter.text().to_string())
            .unwrap_or_default();
        self.targets_open = false;
        self.history_open = false;
        if self.filter_input.read(cx).content() != text {
            self.filter_input
                .update(cx, |input, cx| input.set_content(text, cx));
        }
        self.rebuild_rows(cx);
        self.conditions_open = !self.rows.is_empty() && self.conditions_open;
    }

    /// The box's text changed: narrow the connection on screen by it.
    fn set_filter_text(&mut self, text: String, cx: &mut Context<Self>) {
        let Some((root, mut filter)) = self.visible_filter(cx) else {
            return;
        };
        if filter.text() == text {
            return;
        }
        filter.set_text(text);
        self.apply_filter(root, filter, cx);
    }

    /// Hands `filter` to the source, and lifts a selection it hid onto the
    /// folder that was drawing it.
    fn apply_filter(&mut self, root: ConnectionId, filter: ExplorerFilter, cx: &mut Context<Self>) {
        self.update_source(cx, |source| source.set_filter(root, filter));
        self.mark_invalid(cx);
        self.tree.update(cx, |tree, cx| {
            let lifted = tree
                .selected()
                .and_then(|selected| tree.source().hidden_selection_folder(selected));
            if let Some(folder) = lifted {
                tree.set_selected(Some(folder), cx);
            }
        });
    }

    /// Ticks or unticks one folder in the filter's menu, or with `None` every
    /// folder at once — all of them, or none when all were ticked.
    fn toggle_target(&mut self, folder: Option<Folder>, cx: &mut Context<Self>) {
        let Some((root, mut filter)) = self.visible_filter(cx) else {
            return;
        };
        let targets = filter.targets();
        filter.set_targets(match folder {
            Some(folder) => targets.toggled(folder),
            None if targets.is_all() => FolderSet::NONE,
            None => FolderSet::ALL,
        });
        self.apply_filter(root, filter, cx);
    }

    /// Reads the box as a regular expression, or as its own terms.
    fn toggle_regex(&mut self, cx: &mut Context<Self>) {
        let Some((root, mut filter)) = self.visible_filter(cx) else {
            return;
        };
        filter.set_regex(!filter.regex());
        self.apply_filter(root, filter, cx);
        cx.notify();
    }

    /// Narrows every database, or only the one the connection landed in.
    fn set_all_databases(&mut self, all: bool, cx: &mut Context<Self>) {
        let Some((root, mut filter)) = self.visible_filter(cx) else {
            return;
        };
        if filter.all_databases() == all {
            return;
        }
        filter.set_all_databases(all);
        self.apply_filter(root, filter, cx);
    }

    /// `Enter` in the box: opens every folder on screen the filter may have
    /// found something in, and hands the keyboard to the tree.
    ///
    /// Only the folders already drawn — under a schema that is open — so that
    /// a server with hundreds of schemas is not read end to end by one key. A
    /// folder not read yet is opened too, which is what reads it; it arrives
    /// narrowed.
    fn open_matches(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let tree = self.tree.read(cx);
        let folders: Vec<NodeId> = tree
            .rows()
            .iter()
            .filter_map(|row| row.id())
            .filter(|id| tree.source().may_match(id))
            .cloned()
            .collect();
        self.tree.update(cx, |tree, cx| {
            for folder in &folders {
                tree.expand(folder, cx);
            }
        });
        if let Some(root) = self.visible_root(cx) {
            self.record_history(root, cx);
        }
        self.take_focus(window, cx);
    }

    /// Puts the keyboard in the filter box.
    fn focus_filter(&self, window: &mut Window, cx: &mut Context<Self>) {
        let handle = self.filter_input.read(cx).focus_handle(cx);
        window.focus(&handle, cx);
    }

    /// `Escape` in the box: empties it, or — already empty — goes back to the
    /// tree, so that a second `Escape` is the way out of the box.
    fn clear_filter(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.filter_input.read(cx).content().is_empty() {
            self.take_focus(window, cx);
            return;
        }
        self.filter_input.update(cx, |input, cx| input.clear(cx));
    }

    /// Marks the node as loading and asks the workspace to fetch it.
    fn request(&mut self, node: NodeId, cx: &mut Context<Self>) {
        self.generation += 1;
        let generation = self.generation;
        self.pending.insert(node.clone(), generation);
        self.tree.update(cx, |tree, cx| {
            tree.source_mut(cx).mark_loading(node.clone());
        });
        cx.emit(ExplorerEvent::Load { node, generation });
    }

    /// Runs `edit` against the tree's source and redraws.
    pub fn update_source(
        &mut self,
        cx: &mut Context<Self>,
        edit: impl FnOnce(&mut ExplorerSource),
    ) {
        self.tree.update(cx, |tree, cx| edit(tree.source_mut(cx)));
        cx.notify();
    }

    /// Records the children a fetch produced, or the reason it failed.
    pub fn deliver(
        &mut self,
        node: NodeId,
        generation: u64,
        outcome: Result<Vec<Child>, SharedString>,
        cx: &mut Context<Self>,
    ) {
        if self.pending.get(&node) != Some(&generation) {
            return;
        }
        self.pending.remove(&node);
        if !self
            .tree
            .read(cx)
            .source()
            .info
            .contains_key(&node.connection())
        {
            self.reveals.remove(&node.connection());
            return;
        }
        let next = self.next_revealed(&node, outcome.as_ref().ok());
        self.update_source(cx, |source| match outcome {
            Ok(children) => source.set_children(node, children),
            Err(message) => source.set_error(node, message),
        });
        if let Some((opened, previous)) = next {
            self.open_revealed(opened, &previous, cx);
        }
    }

    /// Opens `connection`'s root, then — as each level arrives — the catalog
    /// and the schema its session landed in.
    ///
    /// What a connection that has just opened asks for: the database its URL
    /// points at is the one the user is about to work in, so the tree opens
    /// onto that database's folders rather than sitting on a closed root.
    ///
    /// The path cannot be opened in one go, because which levels exist — and
    /// so which node ids make it up — is only known once each one has been read
    /// (see the layer-skipping rule in the module documentation). Instead each
    /// delivery of the level the walk is waiting on opens the child of the
    /// matching name, and the walk ends at the folders, or at the first level
    /// with no such child.
    pub fn reveal(&mut self, connection: ConnectionId, target: Scope, cx: &mut Context<Self>) {
        // The same scope is the database the filter narrows by default.
        let landed = target.clone();
        self.update_source(cx, |source| source.set_landed(connection, landed));
        let root = NodeId::Connection(connection);
        self.reveals.insert(
            connection,
            Reveal {
                target,
                awaiting: root.clone(),
            },
        );
        self.tree.update(cx, |tree, cx| tree.expand(&root, cx));
    }

    /// The child of `node` the reveal walk opens next, and the node it opened
    /// before that, when `node` is the one the walk is waiting on.
    ///
    /// Moves the walk on, or ends it: on the next node when that is a catalog,
    /// whose schemas are still to come, and otherwise here — a schema is the
    /// end of the path, and a level that failed or holds no matching child has
    /// nowhere further to go.
    fn next_revealed(
        &mut self,
        node: &NodeId,
        children: Option<&Vec<Child>>,
    ) -> Option<(NodeId, NodeId)> {
        let connection = node.connection();
        if self.reveals.get(&connection)?.awaiting != *node {
            return None;
        }
        let reveal = self.reveals.remove(&connection)?;
        let next = revealed_child(children?, &reveal.target)?;
        if matches!(next, NodeId::Catalog { .. }) {
            self.reveals.insert(
                connection,
                Reveal {
                    target: reveal.target,
                    awaiting: next.clone(),
                },
            );
        }
        Some((next, reveal.awaiting))
    }

    /// Opens one node of the reveal walk, and moves the selection onto it.
    ///
    /// The selection follows so that the opened database is in view — a server
    /// with dozens of them would otherwise open it below the fold — but only on
    /// the tree on screen, and only while the user has not picked something in
    /// it themselves: a selection still on `previous`, the node the walk opened
    /// last, is the walk's own, and one on another connection's node is left
    /// over from a tab that is no longer on top.
    fn open_revealed(&mut self, node: NodeId, previous: &NodeId, cx: &mut Context<Self>) {
        let connection = node.connection();
        self.tree.update(cx, |tree, cx| {
            tree.expand(&node, cx);
            let on_screen = tree.source().visible_roots().contains(&connection);
            let untouched = tree
                .selected()
                .is_none_or(|selected| selected.connection() != connection || selected == previous);
            if on_screen && untouched {
                tree.set_selected(Some(node), cx);
            }
        });
    }

    /// Refreshes cached children under `node`, keeping the tree's expansion.
    ///
    /// What a connection finishing its handshake needs: a root the user opened
    /// while it was still connecting answered with "the connection is closed",
    /// and that row would otherwise stay until the tab did.
    pub fn reload(&mut self, node: &NodeId, cx: &mut Context<Self>) {
        let node = node.refresh_root();
        let mut reload: Vec<_> = self
            .tree
            .read(cx)
            .source()
            .children
            .keys()
            .filter(|child| node.contains(child))
            .cloned()
            .collect();
        if !matches!(node, NodeId::Schema { .. }) && !reload.contains(&node) {
            reload.push(node.clone());
        }
        self.pending.retain(|child, _| !node.contains(child));
        self.update_source(cx, |source| source.invalidate(&node));
        // Start these explicitly: the tree may still remember its previous
        // request if refresh arrives before it draws the loading/result frame.
        // Unvisited folders stay lazy; previously read folders get fresh counts.
        for node in reload {
            self.request(node, cx);
        }
    }

    /// Refreshes the active connection without changing the tree's expansion.
    fn refresh_visible(&mut self, cx: &mut Context<Self>) {
        let source = self.tree.read(cx).source();
        let root = source
            .visible_roots()
            .into_iter()
            .next()
            .filter(|root| source.info.get(root).is_some_and(|info| info.live));
        if let Some(root) = root {
            self.reload(&NodeId::Connection(root), cx);
        }
    }

    /// The connection roots the tree is drawing, for the shell's own tests.
    ///
    /// The source is behind the tree widget, which nothing outside this module
    /// holds; asserting "the sidebar followed the tab" needs a way through.
    #[cfg(test)]
    pub fn visible_roots(&self, cx: &App) -> Vec<ConnectionId> {
        self.tree.read(cx).source().visible_roots()
    }

    /// The selected node, when it names an object with rows to select from.
    ///
    /// What "query the selected object" acts on. Routines and sequences answer
    /// `None`: `SELECT * FROM` a stored procedure is not a statement in any
    /// dialect this program has to care about.
    pub fn selected_relation(&self, cx: &App) -> Option<ObjectTarget> {
        let target = self.tree.read(cx).selected()?.as_target()?;
        target.folder.is_relation().then_some(target)
    }

    /// Every selected node, in the order the tree draws them.
    ///
    /// One node, or none, unless the user picked several with `Ctrl` or
    /// `Shift`.
    pub fn selected_nodes(&self, cx: &App) -> Vec<NodeId> {
        self.tree.read(cx).selection()
    }

    /// Puts the names of `nodes` on the clipboard, one per line.
    ///
    /// Only the nodes that name something in the database — an object, a
    /// schema, a catalog — count: a folder's label is this program's word, not
    /// a name anything could be looked up by. Nothing is written when none of
    /// them does, so a copy over a folder leaves the clipboard as it was.
    pub fn copy_names(nodes: &[NodeId], cx: &mut App) {
        let text = names_of(nodes);
        if !text.is_empty() {
            cx.write_to_clipboard(gpui::ClipboardItem::new_string(text));
        }
    }

    /// The connection and scope the selected node sits in.
    ///
    /// What "draw the ERD of this" acts on. Anything below the connection root
    /// answers — a schema, a folder, a table — because a diagram is of a
    /// *scope*, and every one of those names one. The root itself does not; see
    /// [`NodeId::as_scope`].
    pub fn selected_scope(&self, cx: &App) -> Option<(ConnectionId, Scope)> {
        let selected = self.tree.read(cx).selected()?;
        Some((selected.connection(), selected.as_scope()?))
    }

    /// Moves the selection, for the shell's own tests.
    ///
    /// The tree is behind a widget nothing outside this module holds, and the
    /// actions gated on a selection have no other way in without a mouse.
    #[cfg(test)]
    pub fn select(&mut self, node: NodeId, cx: &mut Context<Self>) {
        self.tree
            .update(cx, |tree, cx| tree.set_selected(Some(node), cx));
    }
}

impl EventEmitter<ExplorerEvent> for Explorer {}

impl Explorer {
    /// The strip over the tree: the filter box on a line of its own, and under
    /// it the three buttons — the folders it narrows, the conditions, and the
    /// history.
    ///
    /// Two lines rather than one because the sidebar is narrow, and a box
    /// squeezed to fit three buttons beside it would show three letters of
    /// what was typed.
    fn render_filter(&mut self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let filter = self
            .visible_filter(cx)
            .map(|(_, filter)| filter)
            .unwrap_or_default();
        // The hint follows a language switch, which the box outlives, and the
        // mode, which changes what the box takes.
        let placeholder = if filter.regex() {
            ts!("explorer.filter_placeholder_regex")
        } else {
            ts!("explorer.filter_placeholder")
        };
        if self.filter_input.read(cx).current_placeholder() != &placeholder {
            self.filter_input
                .update(cx, |input, cx| input.set_placeholder(placeholder, cx));
        }
        let hint = if filter.regex() {
            ts!("explorer.filter_hint_regex", shortcut = FILTER_CHORD)
        } else {
            ts!("explorer.filter_hint", shortcut = FILTER_CHORD)
        };
        let this = cx.entity();
        let regex = {
            let this = this.clone();
            div()
                .id("explorer-filter-regex")
                .flex_none()
                .tooltip(rugpui::tooltip_label(ts!("explorer.filter_regex")))
                .child(
                    Button::new("explorer-filter-regex-button", ".*")
                        .compact()
                        .variant(if filter.regex() {
                            ButtonVariant::Primary
                        } else {
                            ButtonVariant::Secondary
                        })
                        .on_click(move |_, _window, cx| {
                            this.update(cx, |explorer, cx| explorer.toggle_regex(cx));
                        }),
                )
        };

        let targets = {
            let open = self.targets_open;
            let this = this.clone();
            menu_button(
                "explorer-filter-targets",
                ts!("explorer.filter_targets"),
                None,
                if filter.all_databases() {
                    format!(
                        "{} \u{b7} {} \u{25be}",
                        targets_label(filter.targets()),
                        ts!("explorer.scope_all")
                    )
                } else {
                    format!("{} \u{25be}", targets_label(filter.targets()))
                },
                open,
                self.targets_bounds.clone(),
                move |_window, cx| {
                    this.update(cx, |explorer, cx| {
                        explorer.targets_open = !open;
                        cx.notify();
                    });
                },
            )
        };
        let conditions = {
            let live = filter.live_conditions();
            let label = if live > 0 {
                ts!("explorer.conditions_count", count = live)
            } else {
                ts!("explorer.conditions")
            };
            let this = this.clone();
            div()
                .id("explorer-filter-conditions")
                .flex_none()
                .tooltip(rugpui::tooltip_label(ts!("explorer.conditions_hint")))
                .child(
                    Button::new("explorer-filter-conditions-button", label)
                        .compact()
                        .variant(if self.conditions_open || live > 0 {
                            ButtonVariant::Primary
                        } else {
                            ButtonVariant::Secondary
                        })
                        .on_click(move |_, window, cx| {
                            this.update(cx, |explorer, cx| explorer.toggle_conditions(window, cx));
                        }),
                )
        };
        let reset = {
            let this = this.clone();
            div()
                .id("explorer-filter-reset")
                .flex_none()
                .tooltip(
                    rugpui::Tooltip::new()
                        .text(ts!("explorer.filter_reset_hint"))
                        .note(ts!("explorer.filter_reset_note"))
                        .build(),
                )
                .child(
                    Button::new("explorer-filter-reset-button", ts!("explorer.filter_reset"))
                        .compact()
                        .variant(ButtonVariant::Secondary)
                        .disabled(self.filter_is_reset(cx))
                        .on_click(move |_, _window, cx| {
                            this.update(cx, |explorer, cx| explorer.reset_filter(cx));
                        }),
                )
        };
        let history = {
            let open = self.history_open;
            menu_button(
                "explorer-filter-history",
                ts!("explorer.history_hint"),
                Some(ts!("explorer.history_remove_note")),
                format!("{} \u{25be}", ts!("explorer.history")),
                open,
                self.history_bounds.clone(),
                move |_window, cx| {
                    this.update(cx, |explorer, cx| {
                        explorer.history_open = !open;
                        cx.notify();
                    });
                },
            )
        };

        div()
            .flex()
            .flex_col()
            .flex_none()
            .gap(px(4.))
            .px(px(8.))
            .pb(px(6.))
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(4.))
                    .child(
                        div()
                            .id("explorer-filter-field")
                            .key_context(FILTER_KEY_CONTEXT)
                            .flex_1()
                            .min_w_0()
                            // Two lines rather than one long one: a tooltip's
                            // text never wraps, and the rule with the Enter
                            // sentence after it ran wider than a narrow window
                            // in the longer languages.
                            .tooltip(
                                rugpui::Tooltip::new()
                                    .text(hint)
                                    .note(ts!("explorer.filter_hint_enter"))
                                    .build(),
                            )
                            .child(self.filter_input.clone()),
                    )
                    .child(regex),
            )
            .child(
                div()
                    .flex()
                    .flex_row()
                    .flex_wrap()
                    .items_center()
                    .gap(px(4.))
                    .child(targets)
                    .child(conditions)
                    .child(history)
                    .child(reset),
            )
    }

    /// The conditions panel, between the strip and the tree, while it is
    /// open.
    fn render_conditions(
        &self,
        chrome: &Theme,
        cx: &mut Context<Self>,
    ) -> Option<impl IntoElement + use<>> {
        if !self.conditions_open {
            return None;
        }
        let (_, filter) = self.visible_filter(cx)?;
        let this = cx.entity();

        let combine = {
            let this = this.clone();
            Segmented::new("explorer-combine")
                .options([
                    ("all", ts!("explorer.combine_all")),
                    ("any", ts!("explorer.combine_any")),
                ])
                .selected(match filter.combine() {
                    Combine::All => 0,
                    Combine::Any => 1,
                })
                .on_select(move |index, _window, cx| {
                    let combine = if index == 0 {
                        Combine::All
                    } else {
                        Combine::Any
                    };
                    this.update(cx, |explorer, cx| explorer.set_combine(combine, cx));
                })
        };

        let rows = self
            .rows
            .iter()
            .zip(filter.conditions())
            .map(|(row, condition)| {
                let key = row.key;
                let pick = |which: bool, options: Vec<SharedString>, selected: SharedString| {
                    let chosen = this.clone();
                    let toggled = this.clone();
                    let open = self.open_select == Some((key, which));
                    Select::new((
                        if which {
                            "explorer-condition-field"
                        } else {
                            "explorer-condition-op"
                        },
                        key,
                    ))
                    .chevron_icon(icons::CHEVRON_DOWN)
                    .options(options)
                    .selected(Some(selected))
                    .open(open)
                    .on_select(move |index, _label, _window, cx| {
                        chosen.update(cx, |explorer, cx| {
                            explorer.open_select = None;
                            explorer.edit_condition(key, cx, |condition| {
                                if which {
                                    if let Some(field) = Field::ALL.get(index) {
                                        condition.field = *field;
                                    }
                                } else if let Some(op) = Op::ALL.get(index) {
                                    condition.op = *op;
                                }
                            });
                            cx.notify();
                        });
                    })
                    .on_open_change(move |opened, _window, cx| {
                        toggled.update(cx, |explorer, cx| {
                            explorer.open_select = opened.then_some((key, which));
                            cx.notify();
                        });
                    })
                };
                let field = pick(
                    true,
                    Field::ALL.iter().map(|field| field.label()).collect(),
                    condition.field.label(),
                );
                let op = pick(
                    false,
                    Op::ALL.iter().map(|op| op.label()).collect(),
                    condition.op.label(),
                );
                let remove = {
                    let this = this.clone();
                    Button::new(("explorer-condition-remove", key), "\u{2715}")
                        .compact()
                        .variant(ButtonVariant::Ghost)
                        .on_click(move |_, _window, cx| {
                            this.update(cx, |explorer, cx| explorer.remove_condition(key, cx));
                        })
                };
                // A comparison that takes no value has no box: an empty one
                // would look like something still to fill in.
                let value = if condition.op.takes_value() {
                    div().min_w_0().child(row.input.clone()).into_any_element()
                } else {
                    div().into_any_element()
                };
                div()
                    .flex()
                    .flex_col()
                    .gap(px(2.))
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap(px(4.))
                            .child(div().flex_none().w(rugpui::scaled(84., cx)).child(field))
                            .child(div().flex_1().min_w_0().child(op))
                            .child(div().flex_none().child(remove)),
                    )
                    .child(value)
            })
            .collect::<Vec<_>>();

        let add = {
            let this = this.clone();
            Button::new("explorer-condition-add", ts!("explorer.condition_add"))
                .compact()
                .variant(ButtonVariant::Secondary)
                .on_click(move |_, window, cx| {
                    this.update(cx, |explorer, cx| explorer.add_condition(window, cx));
                })
        };
        let clear = Button::new("explorer-condition-clear", ts!("explorer.condition_clear"))
            .compact()
            .variant(ButtonVariant::Secondary)
            .disabled(self.rows.is_empty())
            .on_click(move |_, _window, cx| {
                this.update(cx, |explorer, cx| explorer.clear_conditions(cx));
            });

        Some(
            div()
                .flex()
                .flex_col()
                .flex_none()
                .gap(px(6.))
                .px(px(8.))
                .py(px(6.))
                .border_t_1()
                .border_b_1()
                .border_color(chrome.border)
                .child(combine)
                .children(rows)
                .child(
                    div()
                        .flex()
                        .flex_row()
                        .flex_wrap()
                        .gap(px(4.))
                        .child(add)
                        .child(clear),
                ),
        )
    }

    /// The history menu, while it is out: the profile's filters, newest
    /// first, each with a × that forgets it alone, and the row that forgets
    /// them all.
    fn render_history_menu(&self, cx: &mut Context<Self>) -> Option<impl IntoElement + use<>> {
        if !self.history_open {
            return None;
        }
        let root = self.visible_root(cx)?;
        let rows = self.history_rows(root, cx);
        let this = cx.entity();
        let under = self.history_bounds.get().bottom_left() + gpui::point(px(0.), px(2.));
        Some(
            ContextMenu::new("explorer-filter-history-menu")
                .position(under)
                .entries(context_menu::entries(rows))
                .on_dismiss(move |_window, cx| {
                    this.update(cx, |explorer, cx| {
                        explorer.history_open = false;
                        cx.notify();
                    });
                }),
        )
    }

    /// The rows of the history menu for the profile `root` was opened from.
    fn history_rows(&self, root: ConnectionId, cx: &mut Context<Self>) -> Vec<MenuRow> {
        let profile = self.profiles.get(&root).copied();
        let entries = profile
            .map(|profile| FilterHistory::entries(cx, profile))
            .unwrap_or_default();
        let this = cx.entity();
        let mut rows: Vec<MenuRow> = entries
            .into_iter()
            .map(|entry| {
                let run = this.clone();
                let row = MenuRow::new(history_label(&entry));
                let row = match profile {
                    Some(profile) => {
                        let this = this.clone();
                        let gone = entry.clone();
                        row.on_remove(move |_window, cx| {
                            FilterHistory::remove(cx, profile, &gone);
                            // The menu is drawn from the history, which is no
                            // view of its own to redraw it.
                            this.update(cx, |_, cx| cx.notify());
                        })
                    }
                    None => row,
                };
                row.on_activate(move |_window, cx| {
                    run.update(cx, |explorer, cx| explorer.use_history(&entry, cx));
                })
            })
            .collect();
        if rows.is_empty() {
            rows.push(MenuRow::new(ts!("explorer.history_empty")).enabled(false));
        } else if let Some(profile) = profile {
            rows.push(MenuRow::separator());
            rows.push(MenuRow::new(ts!("explorer.history_clear")).on_activate(
                move |_window, cx| {
                    FilterHistory::clear(cx, profile);
                },
            ));
        }
        rows
    }

    /// The menu of what the filter narrows, while it is out: the database
    /// the connection landed in or all of them, then the folders.
    fn render_targets_menu(&self, cx: &mut Context<Self>) -> Option<impl IntoElement + use<>> {
        if !self.targets_open {
            return None;
        }
        let (root, filter) = self.visible_filter(cx)?;
        let targets = filter.targets();
        let everywhere = filter.all_databases();
        let landed = match self.tree.read(cx).source().landed_name(root) {
            Some(name) => ts!("explorer.scope_landed_named", name = name),
            None => ts!("explorer.scope_landed"),
        };
        let this = cx.entity();
        let scope = |label: SharedString, all: bool| {
            let this = this.clone();
            MenuRow::new(label)
                .checked(everywhere == all)
                .on_activate(move |_window, cx| {
                    this.update(cx, |explorer, cx| {
                        explorer.targets_kept = true;
                        explorer.set_all_databases(all, cx);
                    });
                })
        };
        let row = |label: SharedString, checked: bool, folder: Option<Folder>| {
            let this = this.clone();
            MenuRow::new(label)
                .checked(checked)
                .on_activate(move |_window, cx| {
                    this.update(cx, |explorer, cx| {
                        explorer.targets_kept = true;
                        explorer.toggle_target(folder, cx);
                    });
                })
        };
        let mut rows = vec![
            scope(landed, false),
            scope(ts!("explorer.scope_all"), true),
            MenuRow::separator(),
            row(ts!("explorer.filter_all_objects"), targets.is_all(), None),
            MenuRow::separator(),
        ];
        rows.extend(
            FOLDERS
                .into_iter()
                .map(|folder| row(folder.label(), targets.contains(folder), Some(folder))),
        );
        let under = self.targets_bounds.get().bottom_left() + gpui::point(px(0.), px(2.));
        Some(
            ContextMenu::new("explorer-filter-targets-menu")
                .position(under)
                .entries(context_menu::entries(rows))
                .on_dismiss(move |_window, cx| {
                    this.update(cx, |explorer, cx| {
                        // The dismissal that follows a tick: stay out.
                        if std::mem::take(&mut explorer.targets_kept) {
                            return;
                        }
                        explorer.targets_open = false;
                        cx.notify();
                    });
                }),
        )
    }

    /// The line under the tree while a filter is narrowing it: how much it
    /// let through of what has been read.
    ///
    /// Of what has been *read*, and it says so, because a folder nobody has
    /// opened has not been filtered — the count is not a search of the
    /// database.
    fn render_filter_status(&self, chrome: &Theme, cx: &App) -> Option<impl IntoElement + use<>> {
        let (root, filter) = self.visible_filter(cx)?;
        if !filter.is_active() {
            return None;
        }
        let (shown, read) = self.tree.read(cx).source().filter_totals(root);
        Some(
            div()
                .flex_none()
                .px(px(10.))
                .py(px(4.))
                .border_t_1()
                .border_color(chrome.border)
                .truncate()
                .text_size(rugpui::scaled(10., cx))
                .text_color(chrome.text_muted)
                .child(match filter.live_conditions() {
                    0 => ts!("explorer.filter_status", shown = shown, read = read),
                    count => ts!(
                        "explorer.filter_status_conditions",
                        shown = shown,
                        read = read,
                        count = count
                    ),
                }),
        )
    }
}

impl Explorer {
    /// Puts the keyboard on the tree, where the arrows walk it.
    pub fn take_focus(&self, window: &mut Window, cx: &mut Context<Self>) {
        let handle = self.tree.read(cx).focus_handle(cx);
        window.focus(&handle, cx);
    }
}

impl Focusable for Explorer {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for Explorer {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let chrome = theme(cx);
        // What the tree would draw at the root level, not how many connections
        // are open: with the roots filtered down to the active tab's, the two
        // part company only while nothing is open at all — and that is the state
        // the words below are for.
        let empty = self.tree.read(cx).source().visible_roots().is_empty();
        let source = self.tree.read(cx).source();
        let live = source
            .visible_roots()
            .iter()
            .any(|root| source.info.get(root).is_some_and(|info| info.live));
        let this = cx.entity();
        // Only over a tree: with nothing open there is nothing to narrow.
        let filter = (!empty).then(|| self.render_filter(cx));
        let status = self.render_filter_status(&chrome, cx);
        let targets_menu = self.render_targets_menu(cx);
        let history_menu = self.render_history_menu(cx);
        let conditions = (!empty)
            .then(|| self.render_conditions(&chrome, cx))
            .flatten();

        div()
            .id("explorer")
            .key_context(KEY_CONTEXT)
            .track_focus(&self.focus_handle)
            .flex()
            .flex_col()
            .size_full()
            .min_w_0()
            .min_h_0()
            // Tinted, because this is the only fill over the sidebar: the body's
            // own stops at the work area beside it rather than reaching under
            // here. Left untinted, the blur behind the window would stop dead at
            // the sidebar's edge; see [`app_settings::window_tint`].
            .bg(app_settings::window_tint(chrome.surface, cx))
            .border_r_1()
            .border_color(chrome.border)
            .child(
                div()
                    .flex()
                    .flex_none()
                    .items_center()
                    .justify_between()
                    .h(rugpui::scaled(32., cx))
                    .px(px(10.))
                    .text_size(rugpui::scaled(10., cx))
                    .text_color(chrome.text_muted)
                    .child(ts!("explorer.title"))
                    .child(
                        Button::new("explorer-refresh", ts!("explorer.refresh"))
                            .compact()
                            .variant(ButtonVariant::Secondary)
                            .disabled(!live)
                            .on_click(move |_, _window, cx| {
                                this.update(cx, |explorer, cx| explorer.refresh_visible(cx));
                            }),
                    ),
            )
            .children(filter)
            .children(conditions)
            .child(if empty {
                // A tree with no roots would draw as a blank rectangle, which
                // reads as broken rather than as empty.
                div()
                    .flex()
                    .flex_1()
                    .min_h_0()
                    .items_center()
                    .justify_center()
                    .px(px(12.))
                    .text_size(rugpui::TextSize::Xs.size(cx))
                    .text_color(chrome.text_muted)
                    .child(ts!("explorer.empty"))
                    .into_any_element()
            } else {
                let bounds = self.tree_bounds.clone();
                div()
                    .relative()
                    .flex()
                    .flex_1()
                    .min_h_0()
                    .child(self.tree.clone())
                    .child(
                        gpui::canvas(move |drawn, _, _| bounds.set(drawn), |_, _, _, _| {})
                            .absolute()
                            .size_full(),
                    )
                    .into_any_element()
            })
            .children(status)
            .children(targets_menu)
            .children(history_menu)
            .on_action(cx.listener(|explorer, _: &FocusFilter, window, cx| {
                explorer.focus_filter(window, cx);
            }))
            .on_action(cx.listener(|explorer, _: &ClearFilter, window, cx| {
                explorer.clear_filter(window, cx);
            }))
            .on_action(cx.listener(|explorer, _: &ToggleRegex, _window, cx| {
                explorer.toggle_regex(cx);
            }))
            .on_action(cx.listener(|explorer, _: &CopyNames, _window, cx| {
                Self::copy_names(&explorer.selected_nodes(cx), cx);
            }))
            // `Shift+F10` and the Menu key: the selected node's menu, the one a
            // right click on it raises.
            .on_action(
                cx.listener(|explorer, _: &rugpui::OpenContextMenu, _window, cx| {
                    let Some(node) = explorer.tree.read(cx).selected().cloned() else {
                        return;
                    };
                    let origin = explorer.tree_bounds.get().origin;
                    cx.emit(ExplorerEvent::ContextMenu {
                        node,
                        position: origin + gpui::point(px(16.), px(16.)),
                        by_keyboard: true,
                    });
                }),
            )
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    const C: ConnectionId = ConnectionId(1);

    fn scope(schema: &str) -> Scope {
        Scope {
            catalog: None,
            schema: Some(schema.to_string()),
        }
    }

    /// An object node of `folder` under `APP`.
    fn node(folder: Folder, name: &str) -> NodeId {
        NodeId::Object {
            connection: C,
            scope: scope("APP"),
            folder,
            name: name.to_string(),
        }
    }

    /// A tree with one open connection whose children are `children`, drawn in
    /// the top half of a window with a drop target under it.
    struct DragHarness {
        /// The panel under test.
        explorer: Entity<Explorer>,
        /// What has been let go of over the target, in the order it arrived.
        dropped: std::rc::Rc<std::cell::RefCell<Vec<ObjectTarget>>>,
    }

    impl Render for DragHarness {
        fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
            let dropped = std::rc::Rc::clone(&self.dropped);
            div()
                .flex()
                .flex_col()
                .size_full()
                .child(
                    div()
                        .flex_none()
                        .h(px(300.))
                        .w(px(240.))
                        .child(self.explorer.clone()),
                )
                .child(
                    div()
                        .flex()
                        .flex_1()
                        .debug_selector(|| "drop-target".to_string())
                        .on_drop::<DraggedObject>(move |dragged: &DraggedObject, _window, _cx| {
                            dropped.borrow_mut().push(dragged.0.clone());
                        }),
                )
        }
    }

    /// A table row dragged out of the tree and let go of somewhere that takes
    /// drops: the payload is the object the row names, and a row that is not a
    /// relation carries no drag at all.
    ///
    /// The row is found by its test selector rather than by arithmetic over the
    /// row height, so the test says nothing about how the panel is laid out.
    #[gpui::test]
    fn a_table_row_drags_the_object_it_names(cx: &mut gpui::TestAppContext) {
        use gpui::{
            Modifiers, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent,
            VisualTestContext, point,
        };

        cx.update(rugpui::init);
        let dropped = std::rc::Rc::new(std::cell::RefCell::new(Vec::<ObjectTarget>::new()));
        let explorer = cx.new(Explorer::new);

        explorer.update(cx, |explorer, cx| {
            explorer.update_source(cx, |source| {
                source.upsert_root(
                    C,
                    RootInfo {
                        name: "staging".into(),
                        color: None,
                        live: true,
                    },
                );
                source.set_visible_root(Some(C));
                // Straight under the root rather than through schemas and
                // folders: what is being tested is the row, and every level
                // between here and it is drawn by the same `render_row`.
                source.set_children(
                    NodeId::Connection(C),
                    vec![
                        Child::plain(node(Folder::Tables, "PERSON")),
                        Child::plain(node(Folder::Procedures, "REBUILD")),
                    ],
                );
            });
            explorer
                .tree
                .update(cx, |tree, cx| tree.expand(&NodeId::Connection(C), cx));
        });

        let window = cx.add_window({
            let explorer = explorer.clone();
            let dropped = std::rc::Rc::clone(&dropped);
            move |_window, _cx| DragHarness { explorer, dropped }
        });
        let mut cx = VisualTestContext::from_window(window.into(), cx);
        cx.run_until_parked();

        // A routine has no columns to draw, so its row is not draggable — and
        // the selector sits on the very element the drag would hang off, so its
        // absence is the absence of the drag.
        assert!(
            cx.debug_bounds("explorer-drag:APP.REBUILD").is_none(),
            "a procedure row must not be draggable"
        );
        let row = cx
            .debug_bounds("explorer-drag:APP.PERSON")
            .expect("the table row is drawn and draggable");
        let target = cx
            .debug_bounds("drop-target")
            .expect("the drop target is drawn");

        cx.simulate_event(MouseDownEvent {
            position: row.center(),
            modifiers: Modifiers::none(),
            button: MouseButton::Left,
            click_count: 1,
            first_mouse: false,
        });
        cx.run_until_parked();
        // Two moves: the first takes the press past the 2 px gpui asks for
        // before it is a drag, the second carries it over the target.
        for position in [
            row.center() + point(px(0.), px(20.)),
            target.center(),
            target.center(),
        ] {
            cx.simulate_event(MouseMoveEvent {
                position,
                pressed_button: Some(MouseButton::Left),
                modifiers: Modifiers::none(),
            });
            cx.run_until_parked();
        }
        cx.simulate_event(MouseUpEvent {
            position: target.center(),
            modifiers: Modifiers::none(),
            button: MouseButton::Left,
            click_count: 1,
        });
        cx.run_until_parked();

        assert_eq!(
            dropped.borrow().as_slice(),
            [node(Folder::Tables, "PERSON")
                .as_target()
                .expect("an object node names a target")]
        );
    }

    /// A live H2 database with something of every kind in it.
    ///
    /// `DB_CLOSE_DELAY=-1` keeps it alive between connections, which is what
    /// lets a test open a second session on the same data.
    pub(crate) fn h2_fixture(name: &str) -> crate::connection::Connected {
        use rudbman_jdbc::StatementSpec;

        let mut profile = crate::connection::h2::profile(name);
        profile.url = format!("{};DB_CLOSE_DELAY=-1", profile.url);
        let connected = crate::connection::connect(
            &profile,
            &crate::connection::h2::driver(),
            &crate::connection::Credentials::typed(Some(String::new()), None),
            &rudbman_core::AppSettings::default(),
        )
        .expect("H2 opens an in-memory database without a server");

        for sql in [
            "create schema if not exists APP",
            "create table APP.TEAM (ID int primary key, NAME varchar(40) not null)",
            "comment on table APP.TEAM is 'the squads people belong to'",
            "comment on column APP.TEAM.NAME is 'what the team is called'",
            "create table APP.PERSON (\
                 ID int primary key, \
                 TEAM_ID int not null, \
                 EMAIL varchar(200), \
                 SALARY numeric(12,2), \
                 constraint FK_PERSON_TEAM foreign key (TEAM_ID) \
                     references APP.TEAM(ID) on delete cascade)",
            "create unique index UX_PERSON_EMAIL on APP.PERSON(EMAIL)",
            "create view APP.RICH as select ID, EMAIL from APP.PERSON where SALARY > 100",
            "create sequence APP.PERSON_SEQ start with 5 increment by 2",
            "create alias APP.F_ABS for 'java.lang.Math.abs(int)'",
        ] {
            connected
                .session()
                .execute(&StatementSpec::new(sql))
                .unwrap_or_else(|error| panic!("{sql}: {error}"));
        }
        connected
    }

    /// The layer-skipping rule against a real product.
    ///
    /// H2 has exactly one catalog, so that level is skipped and opening the
    /// connection lands on its schemas — of which there are two, `PUBLIC` and
    /// `INFORMATION_SCHEMA`, plus the `APP` the fixture makes — so *that* level
    /// is drawn.
    #[test]
    fn a_single_catalog_is_skipped_and_several_schemas_are_not() {
        let connected = h2_fixture("explorer-levels");
        let session = connected.session();

        let children = load_children(session, &NodeId::Connection(C), &HideRules::default())
            .expect("the root loads");
        assert!(
            children
                .iter()
                .all(|child| matches!(child.id, NodeId::Schema { .. })),
            "one catalog must not be drawn as a level: {children:?}"
        );
        let names: Vec<String> = children
            .iter()
            .filter_map(|child| match &child.id {
                NodeId::Schema { name, .. } => Some(name.clone()),
                _ => None,
            })
            .collect();
        assert!(names.iter().any(|name| name == "APP"), "{names:?}");
        assert!(names.len() > 1, "H2 has more than one schema: {names:?}");

        // And the single catalog was folded into the scope rather than lost,
        // so the queries underneath stay qualified.
        let NodeId::Schema { catalog, .. } = &children[0].id else {
            unreachable!()
        };
        assert!(catalog.is_some(), "the one catalog is remembered");
    }

    /// The hide lists are applied before a level is counted, not after.
    ///
    /// With every schema but `APP` hidden, the schema level has one member
    /// and is skipped, so the root lands on `APP`'s folders — exactly as a
    /// database that only ever had `APP` would. An object list takes its names
    /// out of the folders it names and no others, and a level whose members
    /// are all hidden comes back empty rather than skipped.
    #[test]
    fn the_hide_lists_apply_before_the_layer_skipping_count() {
        use rudbman_core::{ExplorerHide, HideList};

        let connected = h2_fixture("explorer-hide");
        let session = connected.session();
        let hide = |hide: ExplorerHide| HideRules::new(&hide);
        let list = |patterns: &[&str], show_only: bool| HideList {
            patterns: patterns.iter().map(|p| (*p).to_owned()).collect(),
            show_only,
        };

        let only_app = hide(ExplorerHide {
            schemas: list(&["app"], true),
            objects: list(&["person"], false),
            object_folders: Some(vec![crate::explorer_hide::folder_key(Folder::Tables)]),
            ..ExplorerHide::default()
        });
        let children =
            load_children(session, &NodeId::Connection(C), &only_app).expect("the root loads");
        assert!(
            children
                .iter()
                .all(|child| matches!(child.id, NodeId::Folder { .. })),
            "one schema left is no level of its own: {children:?}"
        );
        let NodeId::Folder { scope, .. } = &children[0].id else {
            unreachable!()
        };
        assert_eq!(scope.schema.as_deref(), Some("APP"));

        let objects = |folder: Folder, rules: &HideRules| -> Vec<String> {
            let node = NodeId::Folder {
                connection: C,
                scope: scope.clone(),
                folder,
            };
            load_children(session, &node, rules)
                .expect("the folder loads")
                .into_iter()
                .filter_map(|child| match child.id {
                    NodeId::Object { name, .. } => Some(name),
                    _ => None,
                })
                .collect()
        };
        assert_eq!(objects(Folder::Tables, &only_app), vec!["TEAM".to_string()]);
        // The sequence named for the table is in a folder the list does not
        // name, and stays.
        assert!(objects(Folder::Sequences, &only_app).contains(&"PERSON_SEQ".to_string()));

        let nothing = hide(ExplorerHide {
            schemas: list(&[".*"], false),
            ..ExplorerHide::default()
        });
        assert!(
            load_children(session, &NodeId::Connection(C), &nothing)
                .expect("the root loads")
                .is_empty(),
            "every schema hidden is an empty tree, not the unqualified folders"
        );
    }

    /// The folders under a schema, and what they hold.
    #[test]
    fn a_schema_lists_its_tables_views_and_sequences() {
        let connected = h2_fixture("explorer-folders");
        let session = connected.session();
        let scope = Scope {
            catalog: None,
            schema: Some("APP".to_string()),
        };

        let folder = |kind: Folder| NodeId::Folder {
            connection: C,
            scope: scope.clone(),
            folder: kind,
        };
        let names = |kind: Folder| -> Vec<String> {
            load_children(session, &folder(kind), &HideRules::default())
                .unwrap_or_else(|error| panic!("{kind:?}: {error}"))
                .into_iter()
                .filter_map(|child| match child.id {
                    NodeId::Object { name, .. } => Some(name),
                    _ => None,
                })
                .collect()
        };

        let tables = names(Folder::Tables);
        assert!(tables.contains(&"PERSON".to_string()), "{tables:?}");
        assert!(tables.contains(&"TEAM".to_string()), "{tables:?}");
        // The view is *not* in the tables folder: the `types` filter is what
        // keeps the two apart, and without it every view would be listed twice.
        assert!(!tables.contains(&"RICH".to_string()), "{tables:?}");

        let views = names(Folder::Views);
        assert_eq!(views, vec!["RICH".to_string()]);

        let sequences = names(Folder::Sequences);
        assert!(
            sequences.contains(&"PERSON_SEQ".to_string()),
            "{sequences:?}"
        );

        // H2 files `CREATE ALIAS` under procedures and answers `getFunctions`
        // with nothing, which the JNI layer documents; an empty folder is a
        // correct answer, not a failure.
        assert!(names(Folder::Functions).is_empty());
    }

    /// The catalog's comment on an object comes down with the row and is kept
    /// beside it, so the tree can draw it without a second describe.
    #[test]
    fn a_commented_table_carries_its_comment_onto_its_row() {
        let connected = h2_fixture("explorer-remarks");
        let folder = NodeId::Folder {
            connection: C,
            scope: Scope {
                catalog: None,
                schema: Some("APP".to_string()),
            },
            folder: Folder::Tables,
        };
        let children = load_children(connected.session(), &folder, &HideRules::default())
            .expect("the folder loads");

        let mut source = ExplorerSource::default();
        source.set_children(folder, children);

        let object = |name: &str| NodeId::Object {
            connection: C,
            scope: Scope {
                catalog: None,
                schema: Some("APP".to_string()),
            },
            folder: Folder::Tables,
            name: name.to_string(),
        };
        assert_eq!(
            source.remarks_of(&object("TEAM")).map(SharedString::as_ref),
            Some("the squads people belong to")
        );
        // And a table nobody commented gets no subtitle at all, rather than an
        // empty one that would draw as a gap after the name.
        assert!(source.remarks_of(&object("PERSON")).is_none());
    }

    /// A node that will not load leaves the rest of the tree standing.
    #[test]
    fn a_schema_that_does_not_exist_becomes_an_error_row() {
        let connected = h2_fixture("explorer-failure");
        let session = connected.session();

        // A folder whose kind requires a table name it has not been given is
        // the reliable way to make the bridge refuse; a missing schema is
        // answered with an empty list by most drivers, which is not a failure.
        let broken = NodeId::Folder {
            connection: C,
            scope: Scope {
                catalog: None,
                schema: Some("NO_SUCH_SCHEMA".to_string()),
            },
            folder: Folder::Tables,
        };
        let children = load_children(session, &broken, &HideRules::default())
            .expect("an unknown schema is empty, not an error");
        assert!(children.is_empty(), "{children:?}");

        // What a real refusal does to the source: one error row under the node,
        // and everything else untouched.
        let mut source = ExplorerSource::default();
        source.upsert_root(
            C,
            RootInfo {
                name: "h2".into(),
                color: None,
                live: true,
            },
        );
        source.set_visible_root(Some(C));
        source.set_error(broken.clone(), "permission denied".into());
        let ChildState::Loaded(rows) = source.children(Some(&broken)) else {
            panic!("a failed node still answers")
        };
        assert!(matches!(rows[0], NodeId::Error(_)));
        assert_eq!(
            source.children(None),
            ChildState::Loaded(vec![NodeId::Connection(C)]),
            "the tree survives one unreadable node"
        );
    }

    #[test]
    fn the_bridges_own_words_are_what_an_error_row_shows() {
        let connected = h2_fixture("explorer-message");
        // `indexes` requires a table name; asking without one is a protocol
        // refusal, and the message has to be the bridge's rather than a
        // stringified enum.
        let error = connected
            .session()
            .describe(&DescribeRequest::new("indexes"))
            .expect_err("indexes without a table is refused");
        let message = describe_failure(error);
        assert!(!message.is_empty());
        assert!(!message.contains("BridgeError"), "{message}");
    }

    #[test]
    fn the_root_level_is_the_open_tabs_and_needs_no_fetch() {
        let mut source = ExplorerSource::default();
        assert_eq!(source.children(None), ChildState::Loaded(Vec::new()));

        source.upsert_root(
            C,
            RootInfo {
                name: "staging".into(),
                color: None,
                live: true,
            },
        );
        source.set_visible_root(Some(C));
        assert_eq!(
            source.children(None),
            ChildState::Loaded(vec![NodeId::Connection(C)])
        );
        // A connection's own children *are* a fetch.
        assert_eq!(
            source.children(Some(&NodeId::Connection(C))),
            ChildState::NotLoaded
        );
    }

    #[test]
    fn a_schemas_folders_cost_no_round_trip() {
        // The five are the same every time, so answering them from the cache
        // would mean a wasted request per schema opened.
        let source = ExplorerSource::default();
        let schema = NodeId::Schema {
            connection: C,
            catalog: None,
            name: "public".to_string(),
        };
        let ChildState::Loaded(children) = source.children(Some(&schema)) else {
            panic!("a schema's folders are known without asking")
        };
        assert_eq!(children.len(), FOLDERS.len());
        assert!(matches!(
            children[0],
            NodeId::Folder {
                folder: Folder::Tables,
                ..
            }
        ));
    }

    #[test]
    fn an_object_and_an_error_row_are_leaves() {
        let source = ExplorerSource::default();
        let object = NodeId::Object {
            connection: C,
            scope: scope("public"),
            folder: Folder::Tables,
            name: "person".to_string(),
        };
        assert_eq!(source.children(Some(&object)), ChildState::Leaf);
        assert!(!source.has_children(&object));

        let error = NodeId::Error(Box::new(NodeId::Connection(C)));
        assert_eq!(source.children(Some(&error)), ChildState::Leaf);
        assert!(!source.has_children(&error));
    }

    #[test]
    fn a_failed_node_gets_one_error_row_and_the_tree_survives() {
        let mut source = ExplorerSource::default();
        source.upsert_root(
            C,
            RootInfo {
                name: "staging".into(),
                color: None,
                live: true,
            },
        );
        source.set_visible_root(Some(C));
        let folder = NodeId::Folder {
            connection: C,
            scope: scope("secret"),
            folder: Folder::Tables,
        };
        source.set_error(folder.clone(), "permission denied for schema secret".into());

        let ChildState::Loaded(children) = source.children(Some(&folder)) else {
            panic!("a failed node still answers, with the failure")
        };
        assert_eq!(children.len(), 1);
        assert!(matches!(children[0], NodeId::Error(_)));
        assert_eq!(
            source.label_of(&children[0]),
            "permission denied for schema secret"
        );
        // The root is untouched: one unreadable schema is not a dead tree.
        assert_eq!(
            source.children(None),
            ChildState::Loaded(vec![NodeId::Connection(C)])
        );
        // And no badge, because nothing was counted.
        assert!(!source.counts.contains_key(&folder));
    }

    #[test]
    fn a_loaded_folder_carries_its_count_and_a_reload_drops_it() {
        let mut source = ExplorerSource::default();
        let folder = NodeId::Folder {
            connection: C,
            scope: scope("public"),
            folder: Folder::Tables,
        };
        let items: Vec<Child> = ["a", "b", "c"]
            .iter()
            .map(|name| {
                Child::plain(NodeId::Object {
                    connection: C,
                    scope: scope("public"),
                    folder: Folder::Tables,
                    name: (*name).to_string(),
                })
            })
            .collect();
        source.set_children(folder.clone(), items);
        assert_eq!(source.counts.get(&folder), Some(&3));

        source.invalidate(&folder);
        assert_eq!(source.children(Some(&folder)), ChildState::NotLoaded);
        assert!(!source.counts.contains_key(&folder));
    }

    #[test]
    fn closing_a_connection_takes_its_whole_subtree_with_it() {
        let other = ConnectionId(2);
        let mut source = ExplorerSource::default();
        for connection in [C, other] {
            source.upsert_root(
                connection,
                RootInfo {
                    name: "x".into(),
                    color: None,
                    live: true,
                },
            );
            source.set_children(
                NodeId::Connection(connection),
                folders_of(connection, scope("public"))
                    .into_iter()
                    .map(Child::plain)
                    .collect(),
            );
        }

        source.remove_root(C);
        // The workspace names the tab that took the closed one's place; the
        // filter follows it.
        source.set_visible_root(Some(other));
        assert_eq!(
            source.children(None),
            ChildState::Loaded(vec![NodeId::Connection(other)])
        );
        // Nothing of the closed connection is still askable.
        assert_eq!(
            source.children(Some(&NodeId::Connection(C))),
            ChildState::NotLoaded
        );
        // And the one that stayed is untouched.
        assert!(matches!(
            source.children(Some(&NodeId::Connection(other))),
            ChildState::Loaded(_)
        ));
    }

    /// Switching the connection tab switches the tree, and costs nothing.
    ///
    /// The root level answers with the active connection alone, but everything
    /// the other one fetched is still in the source — so coming back finds it
    /// there rather than asking the database again.
    #[test]
    fn only_the_active_connections_root_is_drawn_and_the_rest_are_kept() {
        let other = ConnectionId(2);
        let mut source = ExplorerSource::default();
        for connection in [C, other] {
            source.upsert_root(
                connection,
                RootInfo {
                    name: "x".into(),
                    color: None,
                    live: true,
                },
            );
            source.set_children(
                NodeId::Connection(connection),
                folders_of(connection, scope("public"))
                    .into_iter()
                    .map(Child::plain)
                    .collect(),
            );
        }

        // Nothing named yet, which is what "no connections at all" looks like:
        // an empty root level, and the panel's own empty wording over it.
        assert_eq!(source.children(None), ChildState::Loaded(Vec::new()));
        assert!(source.visible_roots().is_empty());

        source.set_visible_root(Some(C));
        assert_eq!(
            source.children(None),
            ChildState::Loaded(vec![NodeId::Connection(C)])
        );

        source.set_visible_root(Some(other));
        assert_eq!(
            source.children(None),
            ChildState::Loaded(vec![NodeId::Connection(other)])
        );
        // The one switched away from kept every child it had fetched, so
        // switching back is a filter and not a reload.
        assert!(matches!(
            source.children(Some(&NodeId::Connection(C))),
            ChildState::Loaded(_)
        ));
        // And both are still roots: the filter hides one, it does not close it.
        assert_eq!(source.roots(), [C, other]);

        // A root named after its tab closed draws nothing rather than a row out
        // of a map it has been removed from.
        source.remove_root(other);
        assert_eq!(source.children(None), ChildState::Loaded(Vec::new()));
    }

    #[test]
    fn only_a_table_or_a_view_opens_the_full_detail_panel() {
        assert!(Folder::Tables.is_relation());
        assert!(Folder::Views.is_relation());
        for folder in [
            Folder::Procedures,
            Folder::Functions,
            Folder::Sequences,
            Folder::Triggers,
        ] {
            assert!(!folder.is_relation(), "{folder:?}");
        }
    }

    #[test]
    fn tables_and_views_are_told_apart_by_the_types_filter() {
        // JDBC has one accessor for both and a `types` argument to separate
        // them; the system variants are deliberately left out.
        assert_eq!(Folder::Tables.describe_kind(), "tables");
        assert_eq!(Folder::Views.describe_kind(), "tables");
        assert_eq!(Folder::Tables.table_types(), Some(&["TABLE"][..]));
        assert_eq!(
            Folder::Views.table_types(),
            Some(&["VIEW", "MATERIALIZED VIEW"][..])
        );
        for folder in [
            Folder::Procedures,
            Folder::Functions,
            Folder::Sequences,
            Folder::Triggers,
        ] {
            assert_eq!(folder.table_types(), None, "{folder:?}");
        }
        assert_eq!(Folder::Sequences.describe_kind(), "sequences");
        assert_eq!(Folder::Triggers.describe_kind(), "triggers");
    }

    #[test]
    fn an_object_node_names_what_a_panel_has_to_ask_for() {
        let node = NodeId::Object {
            connection: C,
            scope: Scope {
                catalog: Some("app".to_string()),
                schema: Some("public".to_string()),
            },
            folder: Folder::Views,
            name: "active_users".to_string(),
        };
        let target = node.as_target().expect("an object names a target");
        assert_eq!(target.catalog.as_deref(), Some("app"));
        assert_eq!(target.schema.as_deref(), Some("public"));
        assert_eq!(target.name, "active_users");
        assert_eq!(target.qualified(), "public.active_users");

        // Nothing else does.
        assert!(NodeId::Connection(C).as_target().is_none());
        assert!(
            NodeId::Folder {
                connection: C,
                scope: scope("public"),
                folder: Folder::Tables,
            }
            .as_target()
            .is_none()
        );

        // A product with no schemas leaves the name unqualified rather than
        // growing a leading dot.
        let bare = ObjectTarget {
            connection: C,
            catalog: None,
            schema: None,
            folder: Folder::Tables,
            name: "person".to_string(),
        };
        assert_eq!(bare.qualified(), "person");
    }

    #[test]
    fn refreshing_a_schema_clears_its_cached_subtree_only() {
        let mut source = ExplorerSource::default();
        let tables = NodeId::Folder {
            connection: C,
            scope: scope("APP"),
            folder: Folder::Tables,
        };
        let views = NodeId::Folder {
            connection: C,
            scope: scope("APP"),
            folder: Folder::Views,
        };
        let other_schema = NodeId::Folder {
            connection: C,
            scope: scope("OTHER"),
            folder: Folder::Tables,
        };
        let other_connection = NodeId::Folder {
            connection: ConnectionId(2),
            scope: scope("APP"),
            folder: Folder::Tables,
        };
        let table = node(Folder::Tables, "PERSON");
        source.set_children(
            tables.clone(),
            vec![Child {
                id: table.clone(),
                remarks: Some("old comment".into()),
            }],
        );
        source.set_error(views.clone(), "permission denied".into());
        source.set_children(other_schema.clone(), vec![]);
        source.set_children(other_connection.clone(), vec![]);

        source.invalidate(&NodeId::Schema {
            connection: C,
            catalog: None,
            name: "APP".to_string(),
        });
        assert_eq!(source.children(Some(&tables)), ChildState::NotLoaded);
        assert_eq!(source.children(Some(&views)), ChildState::NotLoaded);
        assert!(!source.counts.contains_key(&tables));
        assert!(!source.errors.contains_key(&views));
        assert!(source.remarks_of(&table).is_none());
        assert_eq!(
            source.children(Some(&other_schema)),
            ChildState::Loaded(vec![])
        );
        assert_eq!(
            source.children(Some(&other_connection)),
            ChildState::Loaded(vec![])
        );

        source.invalidate(&NodeId::Connection(C));
        assert_eq!(source.children(Some(&other_schema)), ChildState::NotLoaded);
        assert_eq!(
            source.children(Some(&other_connection)),
            ChildState::Loaded(vec![])
        );
    }

    #[test]
    fn refreshing_a_catalog_includes_folders_with_no_schema_level() {
        let mut source = ExplorerSource::default();
        let folder = |catalog: &str| NodeId::Folder {
            connection: C,
            scope: Scope {
                catalog: Some(catalog.into()),
                schema: None,
            },
            folder: Folder::Tables,
        };
        source.set_children(folder("first"), vec![]);
        source.set_children(folder("second"), vec![]);
        source.invalidate(&NodeId::Catalog {
            connection: C,
            name: "first".into(),
        });
        assert_eq!(
            source.children(Some(&folder("first"))),
            ChildState::NotLoaded
        );
        assert_eq!(
            source.children(Some(&folder("second"))),
            ChildState::Loaded(vec![])
        );
    }

    #[gpui::test]
    fn a_refresh_discards_older_replies_and_retries_an_objects_folder(
        cx: &mut gpui::TestAppContext,
    ) {
        cx.update(rugpui::init);
        let explorer = cx.new(Explorer::new);
        let folder = node(Folder::Tables, "PERSON").refresh_root();
        explorer.update(cx, |explorer, cx| {
            explorer.update_source(cx, |source| {
                source.upsert_root(
                    C,
                    RootInfo {
                        name: "test".into(),
                        color: None,
                        live: true,
                    },
                )
            });
            explorer.request(folder.clone(), cx);
            let old = explorer.generation;
            explorer.reload(&node(Folder::Tables, "PERSON"), cx);
            let current = explorer.generation;
            explorer.deliver(folder.clone(), old, Err("old failure".into()), cx);
            assert_eq!(
                explorer.tree.read(cx).source().children(Some(&folder)),
                ChildState::Loading
            );
            explorer.deliver(
                folder.clone(),
                current,
                Ok(vec![Child::plain(node(Folder::Tables, "NOTE"))]),
                cx,
            );
            explorer.deliver(folder.clone(), old, Ok(vec![]), cx);
            assert_eq!(
                explorer.tree.read(cx).source().children(Some(&folder)),
                ChildState::Loaded(vec![node(Folder::Tables, "NOTE")])
            );

            explorer.request(folder.clone(), cx);
            let current = explorer.generation;
            explorer.update_source(cx, |source| source.remove_root(C));
            explorer.deliver(folder.clone(), current, Ok(vec![]), cx);
            assert_eq!(
                explorer.tree.read(cx).source().children(Some(&folder)),
                ChildState::NotLoaded
            );
        });
    }

    #[gpui::test]
    fn refreshing_before_the_loading_frame_still_starts_a_new_request(
        cx: &mut gpui::TestAppContext,
    ) {
        cx.update(rugpui::init);
        let window = cx.add_window(|_window, cx| Explorer::new(cx));
        let explorer = window
            .update(cx, |_explorer, _window, cx| cx.entity())
            .unwrap();
        let loads = std::rc::Rc::new(std::cell::Cell::new(0));
        let _subscription = cx.update(|cx| {
            let loads = loads.clone();
            cx.subscribe(&explorer, move |explorer, event, cx| {
                if let ExplorerEvent::Load { node, generation } = event {
                    loads.set(loads.get() + 1);
                    explorer.update(cx, |explorer, cx| {
                        if loads.get() == 1 {
                            explorer.reload(node, cx);
                        } else {
                            explorer.deliver(node.clone(), *generation, Ok(vec![]), cx);
                        }
                    });
                }
            })
        });
        explorer.update(cx, |explorer, cx| {
            explorer.update_source(cx, |source| {
                source.upsert_root(
                    C,
                    RootInfo {
                        name: "test".into(),
                        color: None,
                        live: true,
                    },
                );
                source.set_visible_root(Some(C));
            });
            explorer
                .tree
                .update(cx, |tree, cx| tree.expand(&NodeId::Connection(C), cx));
        });
        cx.run_until_parked();
        assert_eq!(
            loads.get(),
            2,
            "refresh must not wait for the original reply"
        );
        explorer.update(cx, |explorer, cx| {
            assert_eq!(
                explorer
                    .tree
                    .read(cx)
                    .source()
                    .children(Some(&NodeId::Connection(C))),
                ChildState::Loaded(vec![])
            );
        });
    }

    #[gpui::test]
    fn refreshing_the_visible_connection_reloads_expanded_folders(cx: &mut gpui::TestAppContext) {
        cx.update(rugpui::init);
        let window = cx.add_window(|_window, cx| Explorer::new(cx));
        let root = NodeId::Connection(C);
        let folder = node(Folder::Tables, "PERSON").refresh_root();
        window
            .update(cx, |explorer, _window, cx| {
                explorer.update_source(cx, |source| {
                    source.upsert_root(
                        C,
                        RootInfo {
                            name: "test".into(),
                            color: None,
                            live: true,
                        },
                    );
                    source.set_visible_root(Some(C));
                    source.set_children(root.clone(), vec![Child::plain(folder.clone())]);
                    source.set_children(
                        folder.clone(),
                        vec![Child::plain(node(Folder::Tables, "PERSON"))],
                    );
                });
                explorer.tree.update(cx, |tree, cx| {
                    tree.expand(&root, cx);
                    tree.expand(&folder, cx);
                });
            })
            .unwrap();
        cx.run_until_parked();
        window
            .update(cx, |explorer, _window, cx| {
                explorer.refresh_visible(cx);
            })
            .unwrap();
        cx.run_until_parked();
        window
            .update(cx, |explorer, _window, cx| {
                let generation = *explorer
                    .pending
                    .get(&root)
                    .expect("the root was requested again");
                explorer.deliver(
                    root.clone(),
                    generation,
                    Ok(vec![Child::plain(folder.clone())]),
                    cx,
                );
            })
            .unwrap();
        cx.run_until_parked();
        window
            .update(cx, |explorer, _window, cx| {
                let generation = *explorer
                    .pending
                    .get(&folder)
                    .expect("the expanded folder was requested again");
                explorer.deliver(
                    folder.clone(),
                    generation,
                    Ok(vec![Child::plain(node(Folder::Tables, "NOTE"))]),
                    cx,
                );
                assert!(explorer.tree.read(cx).is_expanded(&root));
                assert!(explorer.tree.read(cx).is_expanded(&folder));
                assert_eq!(
                    explorer.tree.read(cx).source().children(Some(&folder)),
                    ChildState::Loaded(vec![node(Folder::Tables, "NOTE")])
                );
            })
            .unwrap();
    }

    /// Adds `C` as a live connection and puts its root on screen.
    fn show_live_root(source: &mut ExplorerSource) {
        source.upsert_root(
            C,
            RootInfo {
                name: "test".into(),
                color: None,
                live: true,
            },
        );
        source.set_visible_root(Some(C));
    }

    /// Answers the request the explorer has out for `node`.
    fn answer(
        explorer: &mut Explorer,
        node: &NodeId,
        children: Vec<NodeId>,
        cx: &mut Context<Explorer>,
    ) {
        let generation = *explorer
            .pending
            .get(node)
            .unwrap_or_else(|| panic!("{node:?} was not asked for"));
        explorer.deliver(
            node.clone(),
            generation,
            Ok(children.into_iter().map(Child::plain).collect()),
            cx,
        );
    }

    /// A connection opens onto where its session landed: the catalog, then
    /// the schema, each opened as its level arrives — and only from the level
    /// the walk is waiting on, not from a sibling catalog the user opened.
    #[gpui::test]
    fn a_new_connection_opens_onto_the_catalog_and_schema_it_landed_in(
        cx: &mut gpui::TestAppContext,
    ) {
        cx.update(rugpui::init);
        let window = cx.add_window(|_window, cx| Explorer::new(cx));
        let root = NodeId::Connection(C);
        let catalog = |name: &str| NodeId::Catalog {
            connection: C,
            name: name.into(),
        };
        let schema = |catalog: &str, name: &str| NodeId::Schema {
            connection: C,
            catalog: Some(catalog.into()),
            name: name.into(),
        };
        window
            .update(cx, |explorer, _window, cx| {
                explorer.update_source(cx, show_live_root);
                explorer.reveal(
                    C,
                    Scope {
                        catalog: Some("app".into()),
                        schema: Some("dbo".into()),
                    },
                    cx,
                );
                assert!(explorer.tree.read(cx).is_expanded(&root));
            })
            .unwrap();
        cx.run_until_parked();
        window
            .update(cx, |explorer, _window, cx| {
                answer(explorer, &root, vec![catalog("other"), catalog("app")], cx);
                let tree = explorer.tree.read(cx);
                assert!(tree.is_expanded(&catalog("app")));
                assert!(!tree.is_expanded(&catalog("other")));
                assert_eq!(tree.selected(), Some(&catalog("app")));
                explorer
                    .tree
                    .update(cx, |tree, cx| tree.expand(&catalog("other"), cx));
            })
            .unwrap();
        cx.run_until_parked();
        window
            .update(cx, |explorer, _window, cx| {
                answer(
                    explorer,
                    &catalog("other"),
                    vec![schema("other", "dbo")],
                    cx,
                );
                assert!(!explorer.tree.read(cx).is_expanded(&schema("other", "dbo")));

                answer(
                    explorer,
                    &catalog("app"),
                    vec![schema("app", "dbo"), schema("app", "sales")],
                    cx,
                );
                let tree = explorer.tree.read(cx);
                assert!(tree.is_expanded(&schema("app", "dbo")));
                assert!(!tree.is_expanded(&schema("app", "sales")));
                assert_eq!(tree.selected(), Some(&schema("app", "dbo")));
                assert!(explorer.reveals.is_empty(), "a schema ends the walk");
            })
            .unwrap();
    }

    /// The name is matched in any case when no child has it exactly, and a
    /// selection the user made while the level was loading stays theirs.
    #[gpui::test]
    fn a_revealed_schema_matches_in_any_case_and_leaves_a_users_selection(
        cx: &mut gpui::TestAppContext,
    ) {
        cx.update(rugpui::init);
        let window = cx.add_window(|_window, cx| Explorer::new(cx));
        let root = NodeId::Connection(C);
        let schema = |name: &str| NodeId::Schema {
            connection: C,
            catalog: Some("TEST".into()),
            name: name.into(),
        };
        let picked = node(Folder::Tables, "PERSON").refresh_root();
        window
            .update(cx, |explorer, _window, cx| {
                explorer.update_source(cx, show_live_root);
                explorer.reveal(
                    C,
                    Scope {
                        catalog: Some("test".into()),
                        schema: Some("public".into()),
                    },
                    cx,
                );
                explorer
                    .tree
                    .update(cx, |tree, cx| tree.set_selected(Some(picked.clone()), cx));
            })
            .unwrap();
        cx.run_until_parked();
        window
            .update(cx, |explorer, _window, cx| {
                answer(
                    explorer,
                    &root,
                    vec![schema("INFORMATION_SCHEMA"), schema("PUBLIC")],
                    cx,
                );
                let tree = explorer.tree.read(cx);
                assert!(tree.is_expanded(&schema("PUBLIC")));
                assert!(!tree.is_expanded(&schema("INFORMATION_SCHEMA")));
                assert_eq!(tree.selected(), Some(&picked));
            })
            .unwrap();
    }

    /// A database whose catalog and schema were both skipped is already open
    /// once the root is: the walk ends on the folders and selects nothing.
    #[gpui::test]
    fn a_reveal_that_lands_on_folders_ends_with_the_root_open(cx: &mut gpui::TestAppContext) {
        cx.update(rugpui::init);
        let window = cx.add_window(|_window, cx| Explorer::new(cx));
        let root = NodeId::Connection(C);
        window
            .update(cx, |explorer, _window, cx| {
                explorer.update_source(cx, show_live_root);
                explorer.reveal(
                    C,
                    Scope {
                        catalog: Some("main".into()),
                        schema: None,
                    },
                    cx,
                );
            })
            .unwrap();
        cx.run_until_parked();
        window
            .update(cx, |explorer, _window, cx| {
                let folders = folders_of(
                    C,
                    Scope {
                        catalog: Some("main".into()),
                        schema: None,
                    },
                );
                answer(explorer, &root, folders.clone(), cx);
                let tree = explorer.tree.read(cx);
                assert!(tree.is_expanded(&root));
                assert!(folders.iter().all(|folder| !tree.is_expanded(folder)));
                assert_eq!(tree.selected(), None);
                assert!(explorer.reveals.is_empty());
            })
            .unwrap();
    }

    /// Where a real session says it landed is a schema the root lists, so the
    /// walk has a node to open rather than ending on the first level.
    #[test]
    fn a_sessions_own_schema_is_one_the_root_lists() {
        let connected = h2_fixture("explorer-landed");
        let target = Scope {
            catalog: connected.info.catalog.clone(),
            schema: connected.info.schema.clone(),
        };
        let children = load_children(
            connected.session(),
            &NodeId::Connection(C),
            &HideRules::default(),
        )
        .expect("the root loads");
        let found = revealed_child(&children, &target);
        assert!(
            matches!(&found, Some(NodeId::Schema { name, .. }) if name == "PUBLIC"),
            "{target:?} in {children:?}"
        );
    }

    /// A folder of `C` under `public`, and the objects to fill it with.
    fn folder_of(kind: Folder, names: &[&str]) -> (NodeId, Vec<Child>) {
        let folder = NodeId::Folder {
            connection: C,
            scope: scope("public"),
            folder: kind,
        };
        let items = names
            .iter()
            .map(|name| {
                Child::plain(NodeId::Object {
                    connection: C,
                    scope: scope("public"),
                    folder: kind,
                    name: (*name).to_string(),
                })
            })
            .collect();
        (folder, items)
    }

    /// The names the tree would draw under `folder`.
    fn names_in(source: &ExplorerSource, folder: &NodeId) -> Vec<String> {
        match source.children(Some(folder)) {
            ChildState::Loaded(ids) => ids
                .into_iter()
                .filter_map(|id| match id {
                    NodeId::Object { name, .. } => Some(name),
                    _ => None,
                })
                .collect(),
            other => panic!("{folder:?} is {other:?}"),
        }
    }

    /// A filter of `text` over `targets`.
    fn filter(text: &str, targets: FolderSet) -> ExplorerFilter {
        let mut filter = ExplorerFilter::default();
        filter.set_text(text);
        filter.set_targets(targets);
        filter
    }

    #[test]
    fn a_filter_narrows_only_the_folders_it_names_and_keeps_the_whole_list() {
        let mut source = ExplorerSource::default();
        show_live_root(&mut source);
        let (tables, table_items) =
            folder_of(Folder::Tables, &["customer", "orders", "tmp_customer"]);
        let (sequences, sequence_items) =
            folder_of(Folder::Sequences, &["customer_seq", "orders_seq"]);
        source.set_children(tables.clone(), table_items);
        source.set_children(sequences.clone(), sequence_items);

        let tables_only = FolderSet::NONE.toggled(Folder::Tables);
        source.set_filter(C, filter("cust, !tmp*", tables_only));
        assert_eq!(names_in(&source, &tables), ["customer"]);
        assert_eq!(source.matched(&tables), Some(1));
        // The count is still the whole folder's: the badge reads "1/3".
        assert_eq!(source.counts.get(&tables), Some(&3));
        // Not one of the filter's folders, so drawn whole and not counted.
        assert_eq!(
            names_in(&source, &sequences),
            ["customer_seq", "orders_seq"]
        );
        assert_eq!(source.matched(&sequences), None);
        assert_eq!(source.filter_totals(C), (1, 3));

        // Every folder, same text: the sequences narrow too, with nothing
        // read again.
        source.set_filter(C, filter("cust, !tmp*", FolderSet::ALL));
        assert_eq!(names_in(&source, &sequences), ["customer_seq"]);
        assert_eq!(source.filter_totals(C), (2, 5));

        // A blank box gives the tree back whole.
        source.set_filter(C, filter("", FolderSet::ALL));
        assert_eq!(names_in(&source, &tables).len(), 3);
        assert_eq!(source.matched(&tables), None);
        assert_eq!(source.filter_totals(C), (0, 0));
    }

    #[test]
    fn a_folder_read_after_the_filter_arrives_narrowed_and_stays_so_on_a_reload() {
        let mut source = ExplorerSource::default();
        show_live_root(&mut source);
        source.set_filter(C, filter("*_seq", FolderSet::ALL));
        let (sequences, items) = folder_of(Folder::Sequences, &["orders_seq", "orders_seq_old"]);
        assert!(
            source.may_match(&sequences),
            "not read yet, so it may hold one"
        );

        source.set_children(sequences.clone(), items.clone());
        assert_eq!(names_in(&source, &sequences), ["orders_seq"]);

        source.mark_loading(sequences.clone());
        assert_eq!(source.children(Some(&sequences)), ChildState::Loading);
        source.set_children(sequences.clone(), items);
        assert_eq!(names_in(&source, &sequences), ["orders_seq"]);
    }

    #[test]
    fn only_a_narrowed_folder_with_a_match_or_not_read_yet_is_worth_opening() {
        let mut source = ExplorerSource::default();
        show_live_root(&mut source);
        let (tables, table_items) = folder_of(Folder::Tables, &["orders"]);
        let (views, view_items) = folder_of(Folder::Views, &["v_orders"]);
        let (procedures, _) = folder_of(Folder::Procedures, &[]);
        let (triggers, _) = folder_of(Folder::Triggers, &[]);
        source.set_children(tables.clone(), table_items);
        source.set_children(views.clone(), view_items);
        assert!(!source.may_match(&views), "no filter, nothing to look for");

        source.set_filter(C, filter("v_*", FolderSet::ALL.toggled(Folder::Triggers)));
        assert!(
            !source.may_match(&tables),
            "read, and nothing in it matched"
        );
        assert!(source.may_match(&views));
        assert!(source.may_match(&procedures), "not read yet");
        source.mark_loading(procedures.clone());
        assert!(source.may_match(&procedures), "being read");
        source.set_error(procedures.clone(), "denied".into());
        assert!(!source.may_match(&procedures), "its error row is no match");
        assert!(
            !source.may_match(&triggers),
            "not one of the filter's folders"
        );
        assert!(!source.may_match(&NodeId::Connection(C)));
    }

    /// A copy takes the names of the selected rows that name something in
    /// the database, one per line, and passes over the folders between them.
    #[test]
    fn a_copy_names_objects_schemas_and_catalogs_and_skips_folders() {
        let (tables, items) = folder_of(Folder::Tables, &["orders", "customer"]);
        let mut nodes: Vec<NodeId> = items.into_iter().map(|child| child.id).collect();
        nodes.insert(1, tables);
        nodes.push(NodeId::Connection(C));
        nodes.push(NodeId::Schema {
            connection: C,
            catalog: None,
            name: "public".into(),
        });
        assert_eq!(names_of(&nodes), "orders\ncustomer\npublic");
        assert_eq!(names_of(&[NodeId::Connection(C)]), "");
    }

    /// `Ctrl+C` in the tree copies what is selected, and the tree takes several
    /// rows at once.
    #[gpui::test]
    fn ctrl_c_in_the_tree_copies_the_selected_names(cx: &mut gpui::TestAppContext) {
        cx.update(rugpui::init);
        let window = cx.add_window(|_window, cx| Explorer::new(cx));
        let root = NodeId::Connection(C);
        let (tables, items) = folder_of(Folder::Tables, &["orders", "customer"]);
        let orders = items[0].id.clone();
        window
            .update(cx, |explorer, window, cx| {
                explorer.update_source(cx, |source| {
                    show_live_root(source);
                    source.set_children(root.clone(), vec![Child::plain(tables.clone())]);
                    source.set_children(tables.clone(), items);
                });
                explorer.tree.update(cx, |tree, cx| {
                    tree.expand(&root, cx);
                    tree.expand(&tables, cx);
                    tree.set_selected(Some(orders.clone()), cx);
                });
                explorer.tree.read(cx).focus_handle(cx).focus(window, cx);
            })
            .unwrap();
        cx.run_until_parked();
        let mut cx = gpui::VisualTestContext::from_window(window.into(), cx);
        cx.simulate_keystrokes("shift-down");
        cx.simulate_keystrokes(if cfg!(target_os = "macos") {
            "cmd-c"
        } else {
            "ctrl-c"
        });
        cx.update(|_, cx| {
            let copied = cx.read_from_clipboard().and_then(|item| item.text());
            assert_eq!(copied.as_deref(), Some("orders\ncustomer"));
        });
    }

    #[gpui::test]
    fn a_selection_the_filter_hides_comes_up_to_its_folder(cx: &mut gpui::TestAppContext) {
        cx.update(rugpui::init);
        let window = cx.add_window(|_window, cx| Explorer::new(cx));
        let root = NodeId::Connection(C);
        let (tables, items) = folder_of(Folder::Tables, &["orders", "customer"]);
        let orders = items[0].id.clone();
        let customer = items[1].id.clone();
        window
            .update(cx, |explorer, _window, cx| {
                explorer.update_source(cx, |source| {
                    show_live_root(source);
                    source.set_children(root.clone(), vec![Child::plain(tables.clone())]);
                    source.set_children(tables.clone(), items);
                });
                explorer.tree.update(cx, |tree, cx| {
                    tree.expand(&root, cx);
                    tree.expand(&tables, cx);
                    tree.set_selected(Some(orders.clone()), cx);
                });
                explorer
                    .filter_input
                    .update(cx, |input, cx| input.set_content("ord", cx));
            })
            .unwrap();
        cx.run_until_parked();
        window
            .update(cx, |explorer, _window, cx| {
                // Still on screen: left where it was.
                assert_eq!(explorer.tree.read(cx).selected(), Some(&orders));
                explorer
                    .filter_input
                    .update(cx, |input, cx| input.set_content("cust", cx));
            })
            .unwrap();
        cx.run_until_parked();
        window
            .update(cx, |explorer, _window, cx| {
                assert_eq!(explorer.tree.read(cx).selected(), Some(&tables));
                let source = explorer.tree.read(cx).source();
                assert_eq!(source.hidden_selection_folder(&customer), None);
                assert_eq!(source.hidden_selection_folder(&tables), None);
            })
            .unwrap();
    }

    #[test]
    fn a_failed_folder_and_a_closed_connection_drop_what_the_filter_showed() {
        let mut source = ExplorerSource::default();
        show_live_root(&mut source);
        source.set_filter(C, filter("a", FolderSet::ALL));
        let (tables, items) = folder_of(Folder::Tables, &["a", "b"]);
        source.set_children(tables.clone(), items);
        assert_eq!(source.matched(&tables), Some(1));

        // The error row is what the folder shows, filter or not.
        source.set_error(tables.clone(), "denied".into());
        assert_eq!(source.matched(&tables), None);
        assert_eq!(
            source.children(Some(&tables)),
            ChildState::Loaded(vec![NodeId::Error(Box::new(tables.clone()))])
        );

        source.remove_root(C);
        assert!(source.filter(C).is_none());
        assert!(source.shown.is_empty());
    }

    #[gpui::test]
    fn each_connection_keeps_its_own_text_in_the_filter_box(cx: &mut gpui::TestAppContext) {
        const D: ConnectionId = ConnectionId(2);
        cx.update(rugpui::init);
        let window = cx.add_window(|_window, cx| Explorer::new(cx));
        let text_of = |explorer: &Explorer, connection, cx: &App| {
            explorer
                .tree
                .read(cx)
                .source()
                .filter(connection)
                .map(|filter| filter.text().to_string())
        };
        window
            .update(cx, |explorer, _window, cx| {
                explorer.update_source(cx, |source| {
                    for connection in [C, D] {
                        source.upsert_root(
                            connection,
                            RootInfo {
                                name: "test".into(),
                                color: None,
                                live: true,
                            },
                        );
                    }
                });
                explorer.show_root(Some(C), cx);
                explorer
                    .filter_input
                    .update(cx, |input, cx| input.set_content("orders", cx));
            })
            .unwrap();
        cx.run_until_parked();
        window
            .update(cx, |explorer, _window, cx| {
                assert_eq!(text_of(explorer, C, cx).as_deref(), Some("orders"));
                explorer.show_root(Some(D), cx);
                assert_eq!(explorer.filter_input.read(cx).content(), "");
            })
            .unwrap();
        cx.run_until_parked();
        window
            .update(cx, |explorer, _window, cx| {
                // Emptying the box for D did not empty C's filter.
                assert_eq!(text_of(explorer, C, cx).as_deref(), Some("orders"));
                explorer.show_root(Some(C), cx);
                assert_eq!(explorer.filter_input.read(cx).content(), "orders");
            })
            .unwrap();
    }

    #[gpui::test]
    fn enter_opens_the_folders_the_filter_may_have_found_something_in(
        cx: &mut gpui::TestAppContext,
    ) {
        cx.update(rugpui::init);
        let window = cx.add_window(|_window, cx| Explorer::new(cx));
        let root = NodeId::Connection(C);
        let (tables, table_items) = folder_of(Folder::Tables, &["orders", "customer"]);
        let (views, view_items) = folder_of(Folder::Views, &["v_customer"]);
        let (triggers, _) = folder_of(Folder::Triggers, &[]);
        window
            .update(cx, |explorer, _window, cx| {
                explorer.update_source(cx, |source| {
                    show_live_root(source);
                    source.set_children(
                        root.clone(),
                        [&tables, &views, &triggers]
                            .map(|folder| Child::plain(folder.clone()))
                            .to_vec(),
                    );
                    source.set_children(tables.clone(), table_items);
                    source.set_children(views.clone(), view_items);
                });
                explorer.tree.update(cx, |tree, cx| tree.expand(&root, cx));
                explorer
                    .filter_input
                    .update(cx, |input, cx| input.set_content("order", cx));
            })
            .unwrap();
        cx.run_until_parked();
        window
            .update(cx, |explorer, window, cx| {
                explorer.open_matches(window, cx);
                let tree = explorer.tree.read(cx);
                assert!(tree.is_expanded(&tables), "it holds a match");
                assert!(!tree.is_expanded(&views), "read, and nothing matched");
                assert!(tree.is_expanded(&triggers), "not read yet, so it may");
            })
            .unwrap();
        cx.run_until_parked();
        window
            .update(cx, |explorer, _window, _cx| {
                assert!(
                    explorer.pending.contains_key(&triggers),
                    "opening the unread folder asked for it"
                );
            })
            .unwrap();
    }

    #[test]
    fn the_connected_database_is_the_catalog_or_where_there_is_none_the_schema() {
        let at = |catalog: Option<&str>, schema: Option<&str>| Scope {
            catalog: catalog.map(str::to_string),
            schema: schema.map(str::to_string),
        };
        // PostgreSQL, SQL Server: every schema of the database connected to.
        let postgres = at(Some("app"), Some("public"));
        assert!(in_database(&postgres, &at(Some("app"), Some("sales"))));
        assert!(!in_database(&postgres, &at(Some("other"), Some("public"))));
        // A catalog level nobody named rules nothing out.
        assert!(in_database(&postgres, &at(None, Some("sales"))));
        // MySQL: the catalog is the database, whatever case it comes back in.
        assert!(in_database(
            &at(Some("shop"), None),
            &at(Some("SHOP"), None)
        ));
        // Oracle: no catalogs, so the user's own schema.
        let oracle = at(None, Some("SCOTT"));
        assert!(in_database(&oracle, &at(None, Some("scott"))));
        assert!(!in_database(&oracle, &at(None, Some("HR"))));
        // SQLite: neither level, so everything.
        assert!(in_database(&at(None, None), &at(None, None)));
    }

    #[test]
    fn the_filter_narrows_the_database_the_session_landed_in_unless_told_otherwise() {
        let mut source = ExplorerSource::default();
        show_live_root(&mut source);
        let scope_in = |catalog: &str| Scope {
            catalog: Some(catalog.to_string()),
            schema: Some("public".to_string()),
        };
        let folder_in = |catalog: &str| NodeId::Folder {
            connection: C,
            scope: scope_in(catalog),
            folder: Folder::Tables,
        };
        let objects_in = |catalog: &str| {
            ["orders", "customer"]
                .map(|name| {
                    Child::plain(NodeId::Object {
                        connection: C,
                        scope: scope_in(catalog),
                        folder: Folder::Tables,
                        name: name.to_string(),
                    })
                })
                .to_vec()
        };
        let app = folder_in("app");
        let other = folder_in("other");
        source.set_children(app.clone(), objects_in("app"));
        source.set_children(other.clone(), objects_in("other"));
        source.set_filter(C, filter("cust", FolderSet::ALL));
        // Where the session landed is not known yet: every database narrows.
        assert_eq!(source.matched(&app), Some(1));
        assert_eq!(source.matched(&other), Some(1));

        source.set_landed(
            C,
            Scope {
                catalog: Some("APP".to_string()),
                schema: Some("public".to_string()),
            },
        );
        assert_eq!(source.landed_name(C), Some("APP"));
        assert_eq!(names_in(&source, &app), ["customer"]);
        assert_eq!(names_in(&source, &other), ["orders", "customer"]);
        assert_eq!(source.matched(&other), None);
        assert!(!source.may_match(&other));
        assert_eq!(source.filter_totals(C), (1, 2));

        let mut everywhere = filter("cust", FolderSet::ALL);
        everywhere.set_all_databases(true);
        source.set_filter(C, everywhere);
        assert_eq!(names_in(&source, &other), ["customer"]);
        assert_eq!(source.filter_totals(C), (2, 4));
    }

    #[gpui::test]
    fn the_conditions_panel_narrows_and_a_used_filter_comes_back_from_the_history(
        cx: &mut gpui::TestAppContext,
    ) {
        cx.update(rugpui::init);
        let window = cx.add_window(|_window, cx| Explorer::new(cx));
        let profile = Uuid::new_v4();
        let root = NodeId::Connection(C);
        let (tables, items) = folder_of(Folder::Tables, &["orders", "customer", "audit"]);
        window
            .update(cx, |explorer, window, cx| {
                explorer.update_source(cx, |source| {
                    show_live_root(source);
                    source.set_children(root.clone(), vec![Child::plain(tables.clone())]);
                    source.set_children(tables.clone(), items);
                });
                explorer.set_profile(C, profile);
                // Opening an empty panel starts a row.
                explorer.toggle_conditions(window, cx);
                assert!(explorer.conditions_open);
                assert_eq!(explorer.rows.len(), 1);
                let key = explorer.rows[0].key;
                explorer.edit_condition(key, cx, |condition| condition.op = Op::StartsWith);
                // Still no value: the row narrows nothing yet.
                assert_eq!(names_in(explorer.tree.read(cx).source(), &tables).len(), 3);
            })
            .unwrap();
        // Typed in a later update, as a user types: gpui turns a new
        // subscription on only after the update that made it, so a value set
        // in that same update would go unseen.
        cx.run_until_parked();
        window
            .update(cx, |explorer, _window, cx| {
                explorer.rows[0]
                    .input
                    .update(cx, |input, cx| input.set_content("cu", cx));
            })
            .unwrap();
        cx.run_until_parked();
        window
            .update(cx, |explorer, window, cx| {
                assert_eq!(
                    names_in(explorer.tree.read(cx).source(), &tables),
                    ["customer"]
                );
                // Closing the panel is the filter used: into the history.
                explorer.toggle_conditions(window, cx);
                let entries = FilterHistory::entries(cx, profile);
                assert_eq!(entries.len(), 1);
                assert_eq!(
                    entries[0].conditions,
                    vec![Condition {
                        field: Field::Name,
                        op: Op::StartsWith,
                        value: "cu".to_string(),
                    }]
                );

                explorer.clear_conditions(cx);
                assert!(explorer.rows.is_empty());
                assert_eq!(names_in(explorer.tree.read(cx).source(), &tables).len(), 3);

                explorer.use_history(&entries[0], cx);
                assert!(explorer.conditions_open);
                assert_eq!(explorer.rows.len(), 1);
                assert_eq!(explorer.rows[0].input.read(cx).content(), "cu");
            })
            .unwrap();
        cx.run_until_parked();
        window
            .update(cx, |explorer, _window, cx| {
                assert_eq!(
                    names_in(explorer.tree.read(cx).source(), &tables),
                    ["customer"]
                );
                let key = explorer.rows[0].key;
                explorer.remove_condition(key, cx);
                assert!(explorer.rows.is_empty());
                assert_eq!(names_in(explorer.tree.read(cx).source(), &tables).len(), 3);
            })
            .unwrap();
    }

    /// A connection's first hide lists are no change — nothing was read under
    /// others — the same lists again are none either, and different ones are,
    /// which is what tells the workspace to read the tree again.
    #[gpui::test]
    fn new_hide_lists_are_a_change_and_the_same_ones_are_not(cx: &mut gpui::TestAppContext) {
        use rudbman_core::{ExplorerHide, HideList};

        cx.update(rugpui::init);
        let window = cx.add_window(|_window, cx| Explorer::new(cx));
        let some = ExplorerHide {
            schemas: HideList {
                patterns: vec!["sys".to_owned()],
                show_only: false,
            },
            ..ExplorerHide::default()
        };
        window
            .update(cx, |explorer, _window, _cx| {
                assert!(
                    !explorer.hide_rules(C).hides_schema("sys"),
                    "none given yet"
                );
                assert!(!explorer.set_hide(C, &ExplorerHide::default()));
                assert!(!explorer.set_hide(C, &ExplorerHide::default()));
                assert!(explorer.set_hide(C, &some));
                assert!(explorer.hide_rules(C).hides_schema("SYS"));
                assert!(!explorer.set_hide(C, &some));
            })
            .expect("the window is open");
    }

    #[gpui::test]
    fn leaving_a_tab_keeps_its_filter_in_its_profiles_history(cx: &mut gpui::TestAppContext) {
        const D: ConnectionId = ConnectionId(2);
        cx.update(rugpui::init);
        let window = cx.add_window(|_window, cx| Explorer::new(cx));
        let (first, second) = (Uuid::new_v4(), Uuid::new_v4());
        window
            .update(cx, |explorer, _window, cx| {
                explorer.update_source(cx, |source| {
                    for connection in [C, D] {
                        source.upsert_root(
                            connection,
                            RootInfo {
                                name: "test".into(),
                                color: None,
                                live: true,
                            },
                        );
                    }
                });
                explorer.set_profile(C, first);
                explorer.set_profile(D, second);
                explorer.show_root(Some(C), cx);
                explorer
                    .filter_input
                    .update(cx, |input, cx| input.set_content("orders", cx));
            })
            .unwrap();
        cx.run_until_parked();
        window
            .update(cx, |explorer, _window, cx| {
                explorer.show_root(Some(D), cx);
                let kept = FilterHistory::entries(cx, first);
                assert_eq!(kept.len(), 1);
                assert_eq!(kept[0].text, "orders");
                assert!(FilterHistory::entries(cx, second).is_empty());
            })
            .unwrap();
    }

    /// The reset puts every part of the filter back — the box, the mode, the
    /// folders, the scope and the conditions — and leaves the history alone
    /// but for the filter it threw away, which goes on top.
    #[gpui::test]
    fn a_reset_puts_the_filter_back_and_keeps_the_old_one_in_the_history(
        cx: &mut gpui::TestAppContext,
    ) {
        cx.update(rugpui::init);
        let window = cx.add_window(|_window, cx| Explorer::new(cx));
        let profile = Uuid::new_v4();
        let root = NodeId::Connection(C);
        let (tables, items) = folder_of(Folder::Tables, &["orders", "customer", "audit"]);
        let earlier = FilterSnapshot {
            text: "orders".to_string(),
            ..FilterSnapshot::default()
        };
        window
            .update(cx, |explorer, _window, cx| {
                explorer.update_source(cx, |source| {
                    show_live_root(source);
                    source.set_children(root.clone(), vec![Child::plain(tables.clone())]);
                    source.set_children(tables.clone(), items);
                });
                explorer.set_profile(C, profile);
                FilterHistory::record(cx, profile, earlier.clone());
                assert!(explorer.filter_is_reset(cx), "nothing typed yet");
                explorer
                    .filter_input
                    .update(cx, |input, cx| input.set_content("cust", cx));
            })
            .unwrap();
        cx.run_until_parked();
        window
            .update(cx, |explorer, window, cx| {
                explorer.toggle_regex(cx);
                explorer.toggle_target(Some(Folder::Views), cx);
                explorer.set_all_databases(true, cx);
                explorer.toggle_conditions(window, cx);
                assert_eq!(
                    names_in(explorer.tree.read(cx).source(), &tables),
                    ["customer"]
                );
                assert!(!explorer.filter_is_reset(cx));

                explorer.reset_filter(cx);
                let (_, filter) = explorer.visible_filter(cx).expect("a tree on screen");
                assert!(filter == ExplorerFilter::default());
                assert!(explorer.filter_is_reset(cx));
                assert_eq!(explorer.filter_input.read(cx).content(), "");
                assert!(explorer.rows.is_empty());
                assert!(!explorer.conditions_open);
                assert_eq!(names_in(explorer.tree.read(cx).source(), &tables).len(), 3);

                let entries = FilterHistory::entries(cx, profile);
                assert_eq!(entries.len(), 2, "the old filter added, nothing taken");
                assert_eq!(entries[0].text, "cust");
                assert!(entries[0].regex && entries[0].all_databases);
                assert_eq!(entries[1], earlier);
            })
            .unwrap();
        // The box's notice of its emptied text comes after, and finds nothing
        // left to change.
        cx.run_until_parked();
        window
            .update(cx, |explorer, _window, cx| {
                assert!(explorer.filter_is_reset(cx));
            })
            .unwrap();
    }

    /// A filter put back from a saved workspace lands in the box and the
    /// panel's rows of the tree on screen, narrows the tree, and is what the
    /// workspace would save again; one that narrows nothing saves as nothing.
    #[gpui::test]
    fn a_restored_filter_fills_the_box_and_is_saved_back(cx: &mut gpui::TestAppContext) {
        cx.update(rugpui::init);
        let window = cx.add_window(|_window, cx| Explorer::new(cx));
        let root = NodeId::Connection(C);
        let (tables, items) = folder_of(Folder::Tables, &["orders", "customer", "audit"]);
        let saved = FilterSnapshot {
            text: "cust".to_string(),
            conditions: vec![Condition {
                field: Field::Name,
                op: Op::EndsWith,
                value: "mer".to_string(),
            }],
            ..FilterSnapshot::default()
        };
        window
            .update(cx, |explorer, _window, cx| {
                explorer.update_source(cx, |source| {
                    show_live_root(source);
                    source.set_children(root.clone(), vec![Child::plain(tables.clone())]);
                    source.set_children(tables.clone(), items);
                });
                assert_eq!(explorer.saved_filter(C, cx), None, "none set yet");
                explorer.restore_filter(C, &saved, cx);
                assert_eq!(explorer.filter_input.read(cx).content(), "cust");
                assert_eq!(explorer.rows.len(), 1);
                assert!(!explorer.conditions_open);
                assert_eq!(
                    names_in(explorer.tree.read(cx).source(), &tables),
                    ["customer"]
                );
                assert_eq!(explorer.saved_filter(C, cx), Some(saved.clone()));
            })
            .unwrap();
        // The box's notice of its new text finds the filter already set.
        cx.run_until_parked();
        window
            .update(cx, |explorer, _window, cx| {
                assert_eq!(explorer.saved_filter(C, cx), Some(saved.clone()));
                explorer.reset_filter(cx);
                assert_eq!(explorer.saved_filter(C, cx), None);
            })
            .unwrap();
    }

    /// Every entry of the history menu carries a × that forgets it alone; the
    /// row that forgets them all, and the one that says there are none, do
    /// not.
    #[gpui::test]
    fn one_history_entry_goes_from_its_menu_row(cx: &mut gpui::TestAppContext) {
        use crate::context_menu::{labels, row};

        cx.update(rugpui::init);
        let window = cx.add_window(|_window, cx| Explorer::new(cx));
        let profile = Uuid::new_v4();
        let named = |text: &str| FilterSnapshot {
            text: text.to_string(),
            ..FilterSnapshot::default()
        };
        let label = |text: &str| history_label(&named(text)).to_string();
        let rows = window
            .update(cx, |explorer, _window, cx| {
                explorer.update_source(cx, show_live_root);
                explorer.set_profile(C, profile);
                for text in ["orders", "cust", "*_seq"] {
                    FilterHistory::record(cx, profile, named(text));
                }
                explorer.history_rows(C, cx)
            })
            .unwrap();
        let clear = ts!("explorer.history_clear").to_string();
        assert_eq!(
            labels(&rows),
            [
                label("*_seq"),
                label("cust"),
                label("orders"),
                String::new(),
                clear.clone()
            ]
        );
        assert!(row(&rows, &label("cust")).is_removable());
        assert!(!row(&rows, &clear).is_removable());

        cx.update_window(window.into(), |_, window, cx| {
            row(&rows, &label("cust")).remove(window, cx);
        })
        .unwrap();
        let rows = window
            .update(cx, |explorer, _window, cx| {
                assert_eq!(
                    FilterHistory::entries(cx, profile),
                    vec![named("*_seq"), named("orders")],
                    "the rest stay, in their order"
                );
                FilterHistory::clear(cx, profile);
                explorer.history_rows(C, cx)
            })
            .unwrap();
        assert_eq!(labels(&rows), [ts!("explorer.history_empty").to_string()]);
        assert!(!rows[0].is_removable());
    }

    #[test]
    fn a_history_entry_reads_as_its_text_conditions_and_folders() {
        let entry = FilterSnapshot {
            text: "cust".to_string(),
            targets: Some(vec![Folder::Views]),
            combine: Combine::All,
            conditions: vec![Condition {
                field: Field::Remarks,
                op: Op::NotEmpty,
                value: String::new(),
            }],
            all_databases: true,
            regex: false,
        };
        assert_eq!(
            history_label(&entry).to_string(),
            format!(
                "cust  \u{b7}  {} {}  \u{b7}  [{}]  \u{b7}  [{}]",
                Field::Remarks.label(),
                Op::NotEmpty.label(),
                Folder::Views.label(),
                ts!("explorer.scope_all")
            )
        );
    }

    #[test]
    fn the_targets_button_names_what_the_filter_narrows() {
        assert_eq!(targets_label(FolderSet::ALL), ts!("explorer.filter_all"));
        assert_eq!(targets_label(FolderSet::NONE), ts!("explorer.filter_none"));
        let views = FolderSet::NONE.toggled(Folder::Views);
        assert_eq!(targets_label(views), Folder::Views.label());
        let more = views.toggled(Folder::Sequences).toggled(Folder::Tables);
        assert_eq!(
            targets_label(more),
            ts!(
                "explorer.filter_more",
                first = Folder::Tables.label(),
                count = 2
            )
        );
    }

    #[test]
    fn every_label_the_explorer_draws_has_a_translation() {
        for folder in FOLDERS {
            let label = folder.label();
            assert!(!label.is_empty());
            assert!(!label.starts_with("explorer."), "{label:?}");
        }
        for label in [
            ts!("explorer.title"),
            ts!("explorer.refresh"),
            ts!("explorer.empty"),
            ts!("explorer.loading"),
            ts!("explorer.unknown_connection"),
            ts!("explorer.load_failed_unknown"),
            ts!("explorer.filter_placeholder"),
            ts!("explorer.filter_hint", shortcut = FILTER_CHORD),
            ts!("explorer.filter_targets"),
            ts!("explorer.filter_all"),
            ts!("explorer.filter_none"),
            ts!("explorer.filter_more", first = "x", count = 1),
            ts!("explorer.filter_all_objects"),
            ts!("explorer.filter_status", shown = 1, read = 2),
            ts!(
                "explorer.filter_status_conditions",
                shown = 1,
                read = 2,
                count = 1
            ),
            ts!("explorer.conditions"),
            ts!("explorer.conditions_count", count = 2),
            ts!("explorer.conditions_hint"),
            ts!("explorer.combine_all"),
            ts!("explorer.combine_any"),
            ts!("explorer.combine_and"),
            ts!("explorer.combine_or"),
            ts!("explorer.condition_value"),
            ts!("explorer.condition_add"),
            ts!("explorer.condition_clear"),
            ts!("explorer.history"),
            ts!("explorer.history_hint"),
            ts!("explorer.history_empty"),
            ts!("explorer.history_clear"),
            ts!("explorer.history_remove_note"),
            ts!("explorer.filter_reset"),
            ts!("explorer.filter_reset_hint"),
            ts!("explorer.filter_reset_note"),
            ts!("explorer.scope_landed_named", name = "app"),
            ts!("explorer.scope_landed"),
            ts!("explorer.scope_all"),
            ts!("explorer.filter_placeholder_regex"),
            ts!("explorer.filter_hint_regex", shortcut = FILTER_CHORD),
            ts!("explorer.filter_hint_enter"),
            ts!("explorer.filter_regex"),
        ]
        .into_iter()
        .chain(Field::ALL.map(Field::label))
        .chain(Op::ALL.map(Op::label))
        {
            assert!(!label.is_empty());
            assert!(!label.starts_with("explorer."), "{label:?}");
        }
    }
}
