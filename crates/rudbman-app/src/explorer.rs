//! The explorer sidebar: the tree of the connection whose tab is on top.
//!
//! The panel down the left of the window (architecture document, §7.1). Each
//! open connection is a root; under it the database's catalogues, schemas,
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
//! catalogues *and* schemas, MySQL has only catalogues, H2 has one catalogue and
//! usually one schema, SQLite has neither. A tree that drew every level would
//! make the common case — one database, one schema — cost two clicks that lead
//! nowhere.
//!
//! So a level with **one or no** members is skipped: the fetch that would have
//! produced it goes straight on to the next one, and the single catalogue or
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
//! is routine — permissions, a catalogue the product exposes but will not open —
//! and it must not cost the user the tree.

use std::collections::HashMap;

use gpui::{
    AnyElement, App, Context, Entity, EventEmitter, FocusHandle, Focusable, IntoElement, Pixels,
    Point, Render, SharedString, Subscription, Window, div, prelude::*, px,
};
use rudbman_jdbc::{DescribeRequest, Error as JdbcError, Session};
use rudbman_ui::{ChildState, Theme, TreeEvent, TreeRowInfo, TreeSource, TreeView, theme};

use crate::app_settings;
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
/// documentation. A `None` catalogue with a `Some` schema is ordinary — that is
/// PostgreSQL with one database — and both `None` is SQLite.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub struct Scope {
    /// The catalogue, when the product has more than one or named the one.
    pub catalog: Option<String>,
    /// The schema, likewise.
    pub schema: Option<String>,
}

/// The kinds of object the explorer lists, one folder each.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
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
}

/// The folders under a schema, in the order they are drawn.
///
/// Tables first because that is what anyone opening a database is looking for;
/// sequences last because most products have none.
pub const FOLDERS: [Folder; 5] = [
    Folder::Tables,
    Folder::Views,
    Folder::Procedures,
    Folder::Functions,
    Folder::Sequences,
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
        }
    }

    /// The `types` filter for the `tables` kind, or `None` for the others.
    ///
    /// `TABLE` and `SYSTEM TABLE` are separate types and only the first is
    /// listed: a user opening a schema is not looking for the catalogue's own
    /// bookkeeping. `VIEW` and `SYSTEM VIEW` likewise.
    pub fn table_types(self) -> Option<&'static [&'static str]> {
        match self {
            Folder::Tables => Some(&["TABLE"]),
            Folder::Views => Some(&["VIEW"]),
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
        }
    }

    /// Whether activating an object of this folder opens a detail panel with
    /// columns, keys and DDL in it.
    ///
    /// Only what has columns: a routine and a sequence get the smaller panel.
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
    /// A catalogue, drawn only when the product has more than one.
    Catalog {
        /// The connection it belongs to.
        connection: ConnectionId,
        /// The catalogue's name.
        name: String,
    },
    /// A schema, drawn only when its catalogue has more than one.
    Schema {
        /// The connection it belongs to.
        connection: ConnectionId,
        /// The catalogue it sits in, when there is one.
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
    /// What the ERD is opened over. A catalogue answers with itself and no
    /// schema, which is the right scope on a product whose schema level was
    /// skipped; a folder and an object answer with the scope they were listed
    /// in. The connection root answers `None` — a diagram of every catalogue at
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
    /// Exact catalogue name, or `None` for "wherever the connection points".
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
                .text_size(px(11.))
                .text_color(chrome.text)
                .child(icons::icon(self.icon, px(12.), chrome.text_muted))
                .child(self.label.clone()),
        )
    }
}

/// What the workspace has to tell the explorer about one open connection.
#[derive(Clone, Debug)]
pub struct RootInfo {
    /// The profile's name, which is the row's label.
    pub name: SharedString,
    /// The colour tag, drawn as the status dot's ring.
    pub color: Option<SharedString>,
    /// Whether the session is live. A root whose session is gone still draws —
    /// the user may be reading it — but its children are never refetched.
    pub live: bool,
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
        self.children.insert(node, ChildState::Loading);
    }

    /// Drops a node's children, so that the next draw asks for them again.
    pub fn invalidate(&mut self, node: &NodeId) {
        self.children.remove(node);
        self.errors.remove(node);
        self.counts.remove(node);
    }

    /// Records the children a fetch produced.
    pub fn set_children(&mut self, node: NodeId, children: Vec<NodeId>) {
        if matches!(node, NodeId::Folder { .. }) {
            self.counts.insert(node.clone(), children.len());
        }
        self.errors.remove(&node);
        self.children.insert(node, ChildState::Loaded(children));
    }

    /// Records that a node could not be loaded.
    ///
    /// The node gets exactly one child — the error row — so the failure is where
    /// the user was looking and the rest of the tree is untouched.
    pub fn set_error(&mut self, node: NodeId, message: SharedString) {
        self.errors.insert(node.clone(), message);
        self.counts.remove(&node);
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
                        .text_size(px(11.))
                        .text_color(chrome.danger)
                        .child(label),
                )
                .into_any_element();
        }

        let mark = match id {
            NodeId::Connection(connection) => {
                return self
                    .render_root(*connection, label, &chrome)
                    .into_any_element();
            }
            NodeId::Catalog { .. } | NodeId::Schema { .. } => icons::SCHEMA,
            NodeId::Folder { .. } => icons::FOLDER,
            NodeId::Object { folder, .. } => folder.icon(),
            NodeId::Error(_) => unreachable!("handled above"),
        };

        // Only a loaded folder gets a count: a badge that read "0" while the
        // fetch was still out would be a wrong answer rather than a missing one.
        let badge = self.counts.get(id).map(|count| {
            div()
                .flex_none()
                .text_size(px(10.))
                .text_color(chrome.text_muted)
                .child(SharedString::from(count.to_string()))
        });

        let body = div()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(6.))
            .min_w_0()
            .child(icons::icon(mark, px(14.), chrome.text_muted))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .when(info.selected, |label| label.text_color(chrome.text))
                    .child(label),
            )
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
            .text_size(px(11.))
            .text_color(theme(cx).text_muted)
            .child(ts!("explorer.loading"))
            .into_any_element()
    }
}

impl ExplorerSource {
    /// The root row: a status dot in the profile's colour, then its name.
    fn render_root(
        &self,
        connection: ConnectionId,
        label: SharedString,
        chrome: &Theme,
    ) -> impl IntoElement + use<> {
        let info = self.info.get(&connection);
        let live = info.is_some_and(|info| info.live);
        let dot = info
            .and_then(|info| info.color.as_deref())
            .and_then(|hex| rudbman_ui::parse_hex(hex))
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
            .child(div().flex_none().size(px(7.)).rounded_full().bg(dot))
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
pub fn load_children(session: &Session, node: &NodeId) -> Result<Vec<NodeId>, String> {
    match node {
        NodeId::Connection(connection) => {
            let catalogs = names(session, &DescribeRequest::new("catalogs"))?;
            if catalogs.len() > 1 {
                return Ok(catalogs
                    .into_iter()
                    .map(|name| NodeId::Catalog {
                        connection: *connection,
                        name,
                    })
                    .collect());
            }
            // One catalogue or none: it becomes part of the scope rather than a
            // row of its own.
            next_level(session, *connection, catalogs.into_iter().next())
        }
        NodeId::Catalog { connection, name } => {
            next_level(session, *connection, Some(name.clone()))
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
                .map(|name| NodeId::Object {
                    connection: *connection,
                    scope: scope.clone(),
                    folder: *folder,
                    name,
                })
                .collect())
        }
        // A schema's folders never reach here — `children` answers them from
        // `FOLDERS` — and an object and an error row are leaves.
        NodeId::Schema { .. } | NodeId::Object { .. } | NodeId::Error(_) => Ok(Vec::new()),
    }
}

/// The schemas of one catalogue, or its folders when there is at most one.
///
/// The second half of the layer-skipping rule. A product with no schemas at all
/// — SQLite — and one with exactly one both land on the folders, with whatever
/// single schema there was folded into the scope so that the queries underneath
/// stay qualified.
fn next_level(
    session: &Session,
    connection: ConnectionId,
    catalog: Option<String>,
) -> Result<Vec<NodeId>, String> {
    let mut request = DescribeRequest::new("schemas");
    request.catalog = catalog.clone();
    let schemas = names(session, &request)?;

    if schemas.len() > 1 {
        return Ok(schemas
            .into_iter()
            .map(|name| NodeId::Schema {
                connection,
                catalog: catalog.clone(),
                name,
            })
            .collect());
    }
    Ok(folders_of(
        connection,
        Scope {
            catalog,
            schema: schemas.into_iter().next(),
        },
    ))
}

/// Runs a `DESCRIBE` and collects the `name` of every item.
///
/// An item with no name is dropped rather than drawn as a blank row: a driver
/// that answers `getSchemas` with a null `TABLE_SCHEM` — some do, for the
/// default schema — would otherwise put an unclickable empty line in the tree.
fn names(session: &Session, request: &DescribeRequest) -> Result<Vec<String>, String> {
    let result = session.describe(request).map_err(describe_failure)?;
    Ok(result
        .items
        .into_iter()
        .filter_map(|item| {
            item.get("name")
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned)
                .filter(|name| !name.is_empty())
        })
        .collect())
}

/// Turns a JNI-layer failure into the sentence the error row shows.
///
/// The bridge's own message, never a stack trace: a metadata refusal is
/// something the user can act on — a permission, a catalogue the product will
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

/// What the explorer asks the workspace for.
pub enum ExplorerEvent {
    /// The children of `node` are wanted; the workspace has the session.
    Load(NodeId),
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
        /// The node the pointer went down on.
        node: NodeId,
        /// Where the pointer was, in window coordinates.
        position: Point<Pixels>,
    },
}

/// The sidebar view.
pub struct Explorer {
    tree: Entity<TreeView<ExplorerSource>>,
    focus_handle: FocusHandle,
    /// Keeps the tree's subscription alive.
    _events: Subscription,
}

impl Explorer {
    /// Builds the panel around an empty tree.
    pub fn new(cx: &mut Context<Self>) -> Self {
        let tree = cx.new(|cx| {
            TreeView::new(ExplorerSource::default(), cx)
                .with_arrow_icons(icons::CHEVRON_RIGHT, icons::CHEVRON_DOWN)
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
                });
            }
        });

        Self {
            tree,
            focus_handle: cx.focus_handle(),
            _events: events,
        }
    }

    /// Marks the node as loading and asks the workspace to fetch it.
    fn request(&mut self, node: NodeId, cx: &mut Context<Self>) {
        self.tree.update(cx, |tree, cx| {
            tree.source_mut(cx).mark_loading(node.clone());
        });
        cx.emit(ExplorerEvent::Load(node));
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
        outcome: Result<Vec<NodeId>, SharedString>,
        cx: &mut Context<Self>,
    ) {
        self.update_source(cx, |source| match outcome {
            Ok(children) => source.set_children(node, children),
            Err(message) => source.set_error(node, message),
        });
    }

    /// Throws away what is under `node` so the next draw fetches it again.
    ///
    /// What a connection finishing its handshake needs: a root the user opened
    /// while it was still connecting answered with "the connection is closed",
    /// and that row would otherwise stay until the tab did.
    pub fn reload(&mut self, node: &NodeId, cx: &mut Context<Self>) {
        let node = node.clone();
        self.update_source(cx, |source| source.invalidate(&node));
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
                    .h(px(24.))
                    .px(px(10.))
                    .text_size(px(10.))
                    .text_color(chrome.text_muted)
                    .child(ts!("explorer.title")),
            )
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
                    .text_size(px(11.))
                    .text_color(chrome.text_muted)
                    .child(ts!("explorer.empty"))
                    .into_any_element()
            } else {
                div()
                    .flex()
                    .flex_1()
                    .min_h_0()
                    .child(self.tree.clone())
                    .into_any_element()
            })
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

        cx.update(rudbman_ui::init);
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
                        node(Folder::Tables, "PERSON"),
                        node(Folder::Procedures, "REBUILD"),
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
    /// H2 has exactly one catalogue, so that level is skipped and opening the
    /// connection lands on its schemas — of which there are two, `PUBLIC` and
    /// `INFORMATION_SCHEMA`, plus the `APP` the fixture makes — so *that* level
    /// is drawn.
    #[test]
    fn a_single_catalogue_is_skipped_and_several_schemas_are_not() {
        let connected = h2_fixture("explorer-levels");
        let session = connected.session();

        let children = load_children(session, &NodeId::Connection(C)).expect("the root loads");
        assert!(
            children
                .iter()
                .all(|node| matches!(node, NodeId::Schema { .. })),
            "one catalogue must not be drawn as a level: {children:?}"
        );
        let names: Vec<String> = children
            .iter()
            .filter_map(|node| match node {
                NodeId::Schema { name, .. } => Some(name.clone()),
                _ => None,
            })
            .collect();
        assert!(names.iter().any(|name| name == "APP"), "{names:?}");
        assert!(names.len() > 1, "H2 has more than one schema: {names:?}");

        // And the single catalogue was folded into the scope rather than lost,
        // so the queries underneath stay qualified.
        let NodeId::Schema { catalog, .. } = &children[0] else {
            unreachable!()
        };
        assert!(catalog.is_some(), "the one catalogue is remembered");
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
            load_children(session, &folder(kind))
                .unwrap_or_else(|error| panic!("{kind:?}: {error}"))
                .into_iter()
                .filter_map(|node| match node {
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
        let children =
            load_children(session, &broken).expect("an unknown schema is empty, not an error");
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
        let items: Vec<NodeId> = ["a", "b", "c"]
            .iter()
            .map(|name| NodeId::Object {
                connection: C,
                scope: scope("public"),
                folder: Folder::Tables,
                name: (*name).to_string(),
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
                folders_of(connection, scope("public")),
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
                folders_of(connection, scope("public")),
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
        for folder in [Folder::Procedures, Folder::Functions, Folder::Sequences] {
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
        assert_eq!(Folder::Views.table_types(), Some(&["VIEW"][..]));
        for folder in [Folder::Procedures, Folder::Functions, Folder::Sequences] {
            assert_eq!(folder.table_types(), None, "{folder:?}");
        }
        assert_eq!(Folder::Sequences.describe_kind(), "sequences");
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
    fn every_label_the_explorer_draws_has_a_translation() {
        for folder in FOLDERS {
            let label = folder.label();
            assert!(!label.is_empty());
            assert!(!label.starts_with("explorer."), "{label:?}");
        }
        for label in [
            ts!("explorer.title"),
            ts!("explorer.empty"),
            ts!("explorer.loading"),
            ts!("explorer.unknown_connection"),
            ts!("explorer.load_failed_unknown"),
        ] {
            assert!(!label.is_empty());
            assert!(!label.starts_with("explorer."), "{label:?}");
        }
    }
}
