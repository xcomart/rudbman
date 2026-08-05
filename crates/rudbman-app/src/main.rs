// Rust links Windows binaries with the console subsystem by default, which
// flashes a console window before the GUI appears. Release builds use the GUI
// subsystem instead; debug builds keep the console so that env_logger output
// stays visible while developing.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

//! rudbman — a multi-platform GUI database workbench.
//!
//! The binary owns the application shell: a tab strip of open connections, the
//! work area below it, a status bar, and the dialogs rendered on top of
//! everything else. The work area is a tree of panes ([`pane_tree`]), each
//! holding a strip of tabs — a detail panel, an editor over its results, an ERD
//! canvas — and showing the one of them on top, or the empty state while it
//! holds none.
//!
//! # One work area per connection
//!
//! The connection tab at the top is the window's mode: it selects a whole
//! [`WorkArea`] — a pane tree, the pane the marker is on, and the query numbering
//! — and the explorer's visible root along with it. Everything below the strip
//! belongs to exactly one connection, so switching tabs switches the panes, the
//! splits and the sidebar together, and switching back finds them as they were.
//!
//! The alternative, one tree shared by every connection, was tried first and
//! reads badly: a split arranged for one database follows the user into the
//! next, and a strip of tabs from three connections at once needs a colour dot
//! per tab to be legible at all. Scoping the whole area is what makes the tab
//! mean something.
//!
//! What is deliberately *not* here: the connection dialog (M1) and the explorer
//! tree (M2). The menu already carries the row that will open the first of them;
//! its handler is marked `TODO` and does nothing so far.

mod about_dialog;
mod app_settings;
mod caption;
mod connection;
mod connection_dialog;
mod driver_manager;
mod explorer;
mod i18n;
mod icons;
mod maven;
// The pane tree is written as a self-contained data structure with its own
// tests rather than for the call sites the shell currently has, so it offers
// operations nothing reaches yet — merging a subtree, editing a payload — which
// inside a binary crate read as dead code.
#[allow(dead_code)]
mod pane_tree;
mod query;
mod query_source;
mod settings_dialog;
mod table_detail;
mod theme_editor;
mod theme_picker;

// Compiles `locales/*.yml` into the binary and defines the machinery `t!`
// expands to, which is why it has to sit in the crate root. `fallback = "en"`
// is per key, not per locale: a string a translator has not got to yet shows
// in English while the rest of that language stays translated.
rust_i18n::i18n!("locales", fallback = "en");

use std::collections::HashMap;

use gpui::{
    AnyElement, App, Application, Bounds, Context, Div, DragMoveEvent, Entity, FocusHandle,
    Focusable, Hsla, KeyBinding, Menu, MenuItem, MouseButton, MouseUpEvent, Pixels, Point,
    ScrollHandle, SharedString, Stateful, Subscription, Task, TitlebarOptions, Window,
    WindowBackgroundAppearance, WindowBounds, WindowControlArea, WindowOptions, actions, div,
    prelude::*, px, relative, size,
};
use rudbman_core::{AppSettings, ConnectionProfile, DriverStore, TitlebarStyle, WindowState};
use rudbman_ui::{
    Button, ButtonVariant, DraggedThumb, EditorThemeEntry, EditorThemeRegistry, MenuButton,
    MenuEntry, Scrollbar, ScrollbarAxis, ScrollbarState, TabBar, TabItem, TabStatus, Theme,
    ThemeRegistry, WindowControlIcons, WindowControls, hide_later, modal, scroll_to, scrolled,
    set_editor_theme, set_theme, theme, theme_store,
};

use about_dialog::{AboutDialog, AboutDialogEvent};
use app_settings::WindowGeometry;
use caption::apply_caption_theme;
use connection::{ConnectError, Connected};
use connection_dialog::{ConnectionDialog, ConnectionDialogEvent};
use explorer::{ConnectionId, Explorer, ExplorerEvent, NodeId, ObjectTarget, RootInfo};
use i18n::ts;
use icons::Icons;
use pane_tree::{Axis, Pane, PaneId, PaneItem, PaneNode, PaneTree, SplitId};
use query::{ConfirmRequest, QueryPane, QueryPaneEvent};
use settings_dialog::{SettingsDialog, SettingsDialogEvent};
use table_detail::{TableDetail, TableDetailEvent};

actions!(
    rudbman,
    [
        /// Quit the application.
        Quit,
        /// Open the connection dialog with an empty form.
        NewConnection,
        /// Open the settings dialog.
        OpenSettings,
        /// Open the about dialog.
        ShowAbout,
        /// Close the active pane, unless it is the last one.
        ClosePane,
        /// Move keyboard focus to the next pane.
        FocusNextPane,
        /// Move keyboard focus to the previous pane.
        FocusPrevPane,
        /// Split the active pane, putting an empty pane to its right.
        SplitRight,
        /// Split the active pane, putting an empty pane below it.
        SplitBelow,
        /// Show or hide the explorer sidebar.
        ToggleExplorer,
        /// Open an empty query pane on the connection whose tab is showing.
        NewQuery,
        /// Open a query pane over the object selected in the explorer.
        QueryObject,
        /// Close the open dialog or dropdown menu, if there is one.
        DismissDialog,
    ]
);

/// Key context the workspace-wide shortcuts are scoped to.
const KEY_CONTEXT: &str = "Workspace";

/// Height of the toolbar row holding the application menu and the tab strip.
///
/// Must match the height [`TabBar`] gives itself, otherwise the menu button cell
/// and the tab strip would not line up.
const TOOLBAR_HEIGHT: f32 = 36.;

/// Height of the status bar along the bottom of the window.
const STATUS_BAR_HEIGHT: f32 = 24.;

/// Distance from the top left of the window to the top left of the macOS
/// traffic lights, in the custom title bar style.
///
/// The buttons are 14 pt tall, so half the difference to [`TOOLBAR_HEIGHT`]
/// centres them in the toolbar band.
const TRAFFIC_LIGHT_ORIGIN: Point<Pixels> = Point {
    x: px(12.),
    y: px(11.),
};

/// Width kept clear at the left of the toolbar for the macOS traffic lights.
///
/// Three 14 pt buttons, 20 pt apart, starting at [`TRAFFIC_LIGHT_ORIGIN`], plus
/// the same margin again after the last one.
const TRAFFIC_LIGHT_GAP: f32 = 78.;

/// The application's own name, as the window and the title bar write it.
///
/// A wordmark, so it is never translated.
const APP_NAME: &str = "rudbman";

/// Application id published to the desktop.
///
/// Wayland compositors and X11 docks match it against a `.desktop` file of the
/// same name to pick up the application icon, so `packaging/linux` has to ship
/// `com.aihouse.rudbman.desktop` and nothing else.
const APP_ID: &str = "com.aihouse.rudbman";

/// Modifier key named in the shortcut hints of the dropdown menu.
///
/// Never translated: it is the name printed on the key. It follows
/// [`bind_shortcuts`] on every platform so the two never drift.
const SHORTCUT_MODIFIER: &str = if cfg!(target_os = "macos") {
    "Cmd"
} else {
    "Ctrl"
};

/// Modifier the pane commands are bound to.
///
/// Not [`SHORTCUT_MODIFIER`]: off macOS the plain `Ctrl` chords belong to the
/// SQL editor arriving in M3 — `Ctrl+[` and `Ctrl+]` are indent and outdent in
/// every editor anyone has used — and a binding registered here wins over the
/// focused view, because gpui matches key bindings along the whole dispatch
/// path before it delivers the key event itself. macOS has no such contest:
/// `Cmd` reaches no text field.
const PANE_SHORTCUT_MODIFIER: &str = if cfg!(target_os = "macos") {
    "cmd"
} else {
    "alt"
};

/// Smallest share of a split either of its children may be given.
///
/// Both the clamp a divider drag lands on and the renderer's own guard against
/// a stored ratio that would collapse a pane to nothing. A pane dragged to zero
/// would take its divider handle with it and leave no way to drag it back, so
/// the gesture stops short of the edge rather than letting that happen.
const MIN_SPLIT_RATIO: f32 = 0.1;

/// Thickness of the invisible grab area over a split's divider, in pixels.
///
/// The divider itself is drawn by the pane frames on either side of it, which
/// are a hairline each — far too thin to hit with a pointer. The handle is
/// pulled out of the flow with a negative margin of half this on both sides so
/// that widening the grab area moves nothing: it straddles the seam instead of
/// pushing the panes apart.
const SPLIT_HANDLE: f32 = 6.;

/// Element id of the tab strip's overlay scroll indicator.
///
/// Held here rather than inside [`TabBar`] because a drag of the thumb is
/// answered by the workspace, and the id is what tells this bar's drag from any
/// other bar's in the window.
const TAB_SCROLLBAR: &str = "tab-scrollbar";

/// Placeholder for a status bar cell with nothing to report.
///
/// Punctuation rather than a word, so it is the same in every language.
const NOTHING: SharedString = SharedString::new_static("—");

/// Marker for a drag of the explorer's right edge.
///
/// A type of its own rather than a [`DraggedSplit`] with a reserved id: the
/// sidebar is not in the pane tree, and giving it a fake split id would put it
/// in the path of every enclosing split's listener.
struct DraggedExplorer;

/// Narrowest the explorer may be dragged, in logical pixels.
///
/// Mirrors the clamp `AppSettings::sanitize` applies, so a width the drag
/// produced survives the round trip through `settings.json` unchanged.
const MIN_EXPLORER_WIDTH: f32 = 140.;

/// Widest the explorer may be dragged.
const MAX_EXPLORER_WIDTH: f32 = 720.;

/// The divider a drag is currently holding.
///
/// gpui delivers drag moves to every ancestor of the element the drag started
/// on, so a handle inside nested splits makes each enclosing split's listener
/// fire too. The id in here is how a listener recognises its own divider.
struct DraggedSplit {
    /// The split whose ratio the drag is writing.
    split: SplitId,
}

/// Mints an id no connection tab has ever had.
///
/// The explorer keys a whole subtree by it, so it has to survive a tab in the
/// middle of the strip being closed — which an index into the tab list does
/// not.
fn next_connection_id() -> ConnectionId {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    ConnectionId(NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed))
}

/// Everything one connection tab shows below the strip.
///
/// Held by the [`Connection`] itself, because the lifetimes match exactly:
/// closing a tab is what discards the panes it opened, and the drop order that
/// falls out of that is the one §9.3 asks for — every query pane, and with it
/// every [`connection::SessionHandle`] and every open cursor, goes before the
/// session it was running against is closed.
struct WorkArea {
    /// The panes. Never empty, though a pane may hold no tabs.
    panes: PaneTree<Pane>,
    /// The pane the status bar and the pane commands act on.
    active_pane: PaneId,
    /// Number the next query tab of this connection is titled with.
    ///
    /// Per connection rather than per window, so the numbering reads as "the
    /// third query I opened on staging" rather than counting every editor in
    /// the window. Counts up for the life of the tab and is never reused: two
    /// tabs called "Query 3" — one of them a query that was closed and reopened
    /// — would be worse than a gap in the numbering.
    next_query: u64,
}

impl WorkArea {
    /// A work area of one empty pane, which renders the empty state.
    fn new() -> Self {
        let panes = PaneTree::single(Pane::new());
        let active_pane = panes.first_leaf().0;
        Self {
            panes,
            active_pane,
            next_query: 1,
        }
    }

    /// The active pane, falling back to the first one.
    ///
    /// The fallback only matters if [`WorkArea::active_pane`] ever went stale;
    /// the tree always has a pane, so this never fails.
    fn active(&self) -> PaneId {
        if self.panes.contains(self.active_pane) {
            self.active_pane
        } else {
            self.panes.first_leaf().0
        }
    }

    /// Every query pane in the area, in layout order.
    ///
    /// What the connection's death is applied to: the tabs stay and the editors
    /// let go of the session, one by one.
    fn queries(&self) -> Vec<Entity<QueryPane>> {
        let mut found = Vec::new();
        for (_, pane) in self.panes.leaves() {
            for item in pane.items() {
                if let PaneItem::Query { pane, .. } = item {
                    found.push(pane.clone());
                }
            }
        }
        found
    }
}

/// One connection tab: the profile it was opened from, where it has got to, and
/// what it has open.
struct Connection {
    /// Stable identity, for as long as the tab lives.
    id: ConnectionId,
    /// The profile, as it was when the connection was asked for. A later edit
    /// in the dialog does not reach a session that is already open — reopening
    /// is what applies it.
    profile: ConnectionProfile,
    /// What the session is doing.
    state: ConnectionState,
    /// The panes below the strip while this tab is on top.
    work: WorkArea,
}

/// The life of one connection tab.
///
/// [`ConnectionState::Dead`] is deliberately distinct from
/// [`ConnectionState::Failed`]: the first is a session that *was* open and is
/// not any more — a tunnel that closed underneath it, usually — and the second
/// is one that never opened. Both are terminal, and neither is repaired
/// silently (architecture document, §9.3).
enum ConnectionState {
    /// The connect task is in flight.
    ///
    /// The task is held rather than detached so that closing the tab abandons
    /// the attempt: a `Task` that is dropped is cancelled at its next await
    /// point, and the half-opened session that may be in flight behind it is
    /// closed by `Connected`'s own `Drop`.
    Connecting { _task: Task<()> },
    /// The session is live.
    Open(Box<Connected>),
    /// The connection never opened.
    Failed(SharedString),
    /// The session was open and has ended.
    Dead(SharedString),
}

impl ConnectionState {
    /// The dot the tab strip draws in front of the title.
    fn tab_status(&self) -> TabStatus {
        match self {
            ConnectionState::Connecting { .. } => TabStatus::Connecting,
            ConnectionState::Open(_) => TabStatus::Connected,
            ConnectionState::Failed(_) | ConnectionState::Dead(_) => TabStatus::Error,
        }
    }
}

/// A query pane's write confirmation, waiting for an answer.
struct PendingConfirm {
    /// The pane that asked, and that the answer goes back to.
    pane: Entity<QueryPane>,
    /// What the dialog shows.
    request: Box<ConfirmRequest>,
}

/// The root view: title bar, work area, status bar and dialogs.
struct Workspace {
    /// Focus target for the window, so the shortcuts stay live.
    ///
    /// One handle for the whole shell in M0: no pane holds anything focusable
    /// yet, so there is nothing for the keyboard to be inside of. A pane that
    /// grows a view of its own brings a focus handle with it, and this one
    /// becomes what it is meant to be: the fallback that keeps the shortcuts
    /// alive while nothing else holds the keyboard.
    focus_handle: FocusHandle,
    /// The explorer sidebar.
    explorer: Entity<Explorer>,
    /// Whether the sidebar is showing.
    explorer_visible: bool,
    /// Its width in logical pixels, as the divider has left it.
    explorer_width: f32,
    /// The open connections, one per tab, in the order they were opened.
    ///
    /// Each carries the work area shown while its tab is on top, so this list
    /// is what the whole window below the strip is drawn from.
    connections: Vec<Connection>,
    /// Index into [`Workspace::connections`] of the tab on screen.
    active_connection: usize,
    /// Horizontal scroll of the tab strip, used to reveal the active tab.
    tab_scroll: ScrollHandle,
    /// Whether the tab strip's overlay scroll indicator is on screen.
    tab_scrollbar: ScrollbarState,
    /// The about dialog, rendered only while it reports itself open.
    about: Entity<AboutDialog>,
    /// The connection dialog, rendered only while it reports itself open.
    connect: Entity<ConnectionDialog>,
    /// The settings dialog, rendered only while it reports itself open.
    settings: Entity<SettingsDialog>,
    /// Whether the application dropdown menu is showing.
    menu_open: bool,
    /// The write confirmation, while a query pane is waiting on it.
    ///
    /// Held here rather than in the pane because a modal centres itself in its
    /// nearest positioned ancestor, and inside a pane that is the wrong box.
    confirm: Option<PendingConfirm>,
    /// Title bar style currently *on the window*.
    ///
    /// Starts as the style the window was created with. Not read from the
    /// settings directly: the toolbar has to branch on what the window actually
    /// carries, and once the settings dialog can switch a live window this field
    /// is what follows the platform call rather than the stored preference.
    titlebar: TitlebarStyle,
    /// Keeps the about dialog subscription alive.
    _about_events: Subscription,
    /// Keeps the connection dialog subscription alive.
    _connect_events: Subscription,
    /// Keeps the explorer subscription alive.
    _explorer_events: Subscription,
    /// Keeps the settings dialog subscription alive.
    _settings_events: Subscription,
    /// Records the window's placement as it is moved and resized.
    _bounds: Subscription,
}

impl Workspace {
    /// Builds the shell with no connection open, and so no work area at all.
    ///
    /// `titlebar` is the style the window was opened with; from then on the
    /// field tracks whatever the applied settings switched the window to.
    fn new(titlebar: TitlebarStyle, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let about = cx.new(AboutDialog::new);
        let about_events =
            cx.subscribe_in(
                &about,
                window,
                |this, dialog, event, window, cx| match event {
                    AboutDialogEvent::Dismissed => {
                        dialog.update(cx, |dialog, cx| dialog.close(cx));
                        this.focus_shell(window, cx);
                    }
                },
            );

        let connect = cx.new(ConnectionDialog::new);
        let connect_events = cx.subscribe_in(
            &connect,
            window,
            |this, dialog, event, window, cx| match event {
                // The dialog has already saved the profile and closed itself;
                // opening the session is the shell's half of the workflow,
                // because the tab it produces belongs here.
                ConnectionDialogEvent::Connect(profile) => {
                    this.open_connection((**profile).clone(), window, cx);
                }
                ConnectionDialogEvent::Dismissed => {
                    dialog.update(cx, |dialog, cx| dialog.close(cx));
                    this.focus_shell(window, cx);
                }
            },
        );

        let explorer = cx.new(Explorer::new);
        let explorer_events = cx.subscribe_in(
            &explorer,
            window,
            |this, _explorer, event, window, cx| match event {
                // The explorer has no session of its own — see its module docs —
                // so every fetch comes back here, where the tabs live.
                ExplorerEvent::Load(node) => this.load_node(node.clone(), cx),
                ExplorerEvent::Activated(target) => {
                    this.open_object((**target).clone(), window, cx);
                }
            },
        );

        let settings = cx.new(SettingsDialog::new);
        let settings_events = cx.subscribe_in(
            &settings,
            window,
            |this, dialog, event, window, cx| match event {
                // The dialog has already replaced and persisted the settings
                // global by the time it emits this; the shell re-applies the
                // parts that touch the live window.
                SettingsDialogEvent::Applied => {
                    this.apply_settings(window, cx);
                    // The dialog closes itself after applying; without a refocus
                    // the window focus dangles on its unrendered controls and
                    // macOS disables every menu item validated through it.
                    this.focus_shell(window, cx);
                }
                // The user is still in the dialog and nothing has been saved, so
                // only the palettes and the fonts follow — and the focus stays
                // where it is, since taking it back now would pull it out from
                // under whoever is typing.
                SettingsDialogEvent::Previewed => this.apply_preview(window, cx),
                // Closing dropped the preview, so re-applying now resolves back
                // to the settings on disk. That is the whole of the undo.
                SettingsDialogEvent::Dismissed => {
                    dialog.update(cx, |dialog, cx| dialog.close(cx));
                    this.apply_preview(window, cx);
                    this.focus_shell(window, cx);
                }
            },
        );

        // In memory only; the file is written once, when the window closes. See
        // [`app_settings::record_window_geometry`].
        let bounds = cx.observe_window_bounds(window, |_this, window, cx| {
            record_window_geometry(window, cx);
        });

        let settings_snapshot = app_settings::current(cx);

        Self {
            focus_handle: cx.focus_handle(),
            explorer,
            explorer_visible: settings_snapshot.explorer_visible,
            explorer_width: settings_snapshot.explorer_width,
            connections: Vec::new(),
            active_connection: 0,
            tab_scroll: ScrollHandle::new(),
            tab_scrollbar: ScrollbarState::new(),
            about,
            connect,
            settings,
            menu_open: false,
            confirm: None,
            titlebar,
            _about_events: about_events,
            _connect_events: connect_events,
            _explorer_events: explorer_events,
            _settings_events: settings_events,
            _bounds: bounds,
        }
    }

    /// The connection the tab strip and the status bar are showing.
    fn active_connection(&self) -> Option<&Connection> {
        self.connections.get(self.active_connection)
    }

    /// The work area on screen: the active connection's.
    ///
    /// `None` only while no connection is open at all. Every pane command, the
    /// status bar and the renderer go through this, which is what makes the
    /// connection tab select the whole window below it — and what makes a pane
    /// command over an empty window a no-op rather than an operation on a tree
    /// nobody can see.
    fn work_area(&self) -> Option<&WorkArea> {
        self.active_connection().map(|open| &open.work)
    }

    /// The work area on screen, mutably.
    fn work_area_mut(&mut self) -> Option<&mut WorkArea> {
        let index = self.active_connection;
        self.connections.get_mut(index).map(|open| &mut open.work)
    }

    /// Opens a session for `profile` in a tab of its own.
    ///
    /// The tab appears immediately, in [`ConnectionState::Connecting`]: the
    /// attempt can take as long as the network does, and a window that showed
    /// nothing until it finished would look frozen. Everything that blocks
    /// happens on a background task, because [`connection::connect`] opens an
    /// SSH channel and a JDBC connection and both of those wait on a socket.
    ///
    /// The new tab comes to the front, which takes the work area that was
    /// showing off screen — so the keyboard has to be asked about before
    /// anything moves; see [`Workspace::follow_work_area`].
    fn open_connection(
        &mut self,
        profile: ConnectionProfile,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let held = self.area_holds_focus(window, cx);
        let drivers = match DriverStore::load() {
            Ok(drivers) => drivers,
            Err(error) => {
                log::error!("could not read drivers.json: {error:#}");
                DriverStore::default()
            }
        };
        let Some(driver) = drivers.get(&profile.driver_id).cloned() else {
            self.connections.push(Connection {
                id: next_connection_id(),
                state: ConnectionState::Failed(ts!(
                    "connect.no_driver",
                    driver = profile.driver_id.clone()
                )),
                profile,
                work: WorkArea::new(),
            });
            self.active_connection = self.connections.len() - 1;
            self.sync_explorer_root(self.active_connection, cx);
            self.sync_visible_root(cx);
            self.follow_work_area(held, window, cx);
            cx.notify();
            return;
        };

        let index = self.connections.len();
        let settings = app_settings::current(cx);
        let attempt = profile.clone();
        // Read here, on the UI thread, and moved straight into the task: the
        // secret exists as a value for the length of one connection attempt and
        // is written to nothing.
        let credentials = connection::Credentials::read(&profile);
        let opening = cx.background_spawn(async move {
            connection::connect(&attempt, &driver, &credentials, &settings)
        });

        let task = cx.spawn(async move |workspace, cx| {
            let outcome = opening.await;
            workspace
                .update(cx, |workspace, cx| workspace.connected(index, outcome, cx))
                .ok();
        });

        self.connections.push(Connection {
            id: next_connection_id(),
            profile,
            state: ConnectionState::Connecting { _task: task },
            work: WorkArea::new(),
        });
        self.active_connection = index;
        self.sync_explorer_root(index, cx);
        self.sync_visible_root(cx);
        self.follow_work_area(held, window, cx);
        cx.notify();
    }

    /// The session of one connection, when it is open.
    fn session_of(&self, connection: ConnectionId) -> Option<connection::SessionHandle> {
        self.connections
            .iter()
            .find(|open| open.id == connection)
            .and_then(|open| match &open.state {
                ConnectionState::Open(connected) => Some(connected.handle()),
                _ => None,
            })
    }

    /// Fetches the children of one explorer node.
    ///
    /// The session's own worker thread serialises this against everything else
    /// that connection is doing, which is the reason the tree draws a
    /// placeholder rather than pretending to be instant: a schema opened while a
    /// statement is running waits for it.
    fn load_node(&mut self, node: NodeId, cx: &mut Context<Self>) {
        let Some(session) = self.session_of(node.connection()) else {
            // The tab was closed, or its session died, between the tree asking
            // and this running. The node keeps its placeholder; there is nobody
            // to ask.
            let explorer = self.explorer.clone();
            let message = ts!("explorer.disconnected");
            cx.defer(move |cx| {
                explorer.update(cx, |explorer, cx| {
                    explorer.deliver(node, Err(message), cx);
                });
            });
            return;
        };

        let fetch = cx.background_spawn({
            let node = node.clone();
            async move { explorer::load_children(session.session(), &node) }
        });
        let explorer = self.explorer.clone();
        cx.spawn(async move |_workspace, cx| {
            let outcome = fetch.await.map_err(SharedString::from);
            explorer
                .update(cx, |explorer, cx| explorer.deliver(node, outcome, cx))
                .ok();
        })
        .detach();
    }

    /// Opens a detail panel for `target`, or brings the open one to the front.
    ///
    /// Activating the same object twice is a navigation, not a request for a
    /// second copy: the panel that is already open shows exactly what a new one
    /// would, and a strip filling up with duplicates of one table is nobody's
    /// idea of a workspace. The search covers every pane of the work area on
    /// screen, so activating an object from one half of a split jumps to the
    /// pane already showing it — and it need cover no more than that, because
    /// the explorer only offers objects of the connection whose area this is.
    fn open_object(&mut self, target: ObjectTarget, window: &mut Window, cx: &mut Context<Self>) {
        if let Some((pane, index)) = self.detail_tab(&target, cx) {
            self.activate_tab(pane, index, window, cx);
            return;
        }

        let panel = cx.new(|cx| TableDetail::new(target, cx));
        // Subscribe first, *then* ask. A panel that requested its own metadata
        // from its constructor would emit into an empty room and sit at
        // "loading…" for ever; see `TableDetail::new`.
        cx.subscribe(&panel, |workspace, panel, event, cx| {
            let TableDetailEvent::Load(target) = event;
            workspace.load_details(panel.clone(), (**target).clone(), cx);
        })
        .detach();
        panel.update(cx, |panel, cx| panel.refresh(cx));
        self.append_tab(PaneItem::TableDetail(panel), window, cx);
    }

    /// Opens a query pane in the active pane, with `sql` already in it.
    ///
    /// Nothing happens without a live session: a SQL editor with no connection
    /// behind it can be typed into and never run, which is worse than a pane
    /// that says there is nothing to connect to.
    fn open_query(&mut self, sql: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(open) = self.active_connection() else {
            return;
        };
        let ConnectionState::Open(connected) = &open.state else {
            return;
        };
        let session = connected.handle();
        let id = open.id;
        let profile = open.profile.clone();
        // The dialect the editor highlights with and the sort round trip quotes
        // with is the *driver's*, not the profile's: a profile names a driver
        // and a driver names a dialect.
        let dialect = DriverStore::load()
            .ok()
            .and_then(|store| {
                store
                    .get(&profile.driver_id)
                    .map(|driver| driver.dialect.clone())
            })
            .unwrap_or_else(|| "generic".to_string());
        let settings = app_settings::current(cx);

        let pane = cx.new(|cx| QueryPane::new(session, id, &profile, &dialect, &settings, sql, cx));
        // The elapsed clock and the row count live in the pane; the status bar
        // that draws them is here, so the shell redraws whenever the pane does.
        cx.observe(&pane, |_workspace, _pane, cx| cx.notify())
            .detach();
        cx.subscribe(&pane, |workspace, pane, event, cx| {
            let QueryPaneEvent::ConfirmWrites(request) = event;
            workspace.confirm = Some(PendingConfirm {
                pane: pane.clone(),
                request: Box::new(ConfirmRequest {
                    count: request.count,
                    preview: request.preview.clone(),
                }),
            });
            cx.notify();
        })
        .detach();

        // Numbered within this connection's own area, which the checks above
        // have already established exists.
        let Some(area) = self.work_area_mut() else {
            return;
        };
        let number = area.next_query;
        area.next_query += 1;
        self.append_tab(
            PaneItem::Query {
                pane: pane.clone(),
                number,
            },
            window,
            cx,
        );
        pane.update(cx, |pane, cx| pane.focus_editor(window, cx));
    }

    /// Opens a query pane over one explorer object, pre-filled with a `SELECT`.
    fn open_query_for(
        &mut self,
        target: &ObjectTarget,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let sql = format!("SELECT * FROM {}", target.qualified());
        self.open_query(&sql, window, cx);
    }

    /// The query pane the status bar and the run commands act on: the active
    /// tab of the active pane of the work area on screen, when that tab is a
    /// query.
    ///
    /// `None` with no connection open, because then there is no work area to
    /// have an active pane at all.
    fn active_query(&self) -> Option<&Entity<QueryPane>> {
        let area = self.work_area()?;
        match area.panes.get(area.active())?.active()? {
            PaneItem::Query { pane, .. } => Some(pane),
            PaneItem::TableDetail(_) => None,
        }
    }

    /// Where `target` is already open, if it is: the pane and the tab in it.
    fn detail_tab(&self, target: &ObjectTarget, cx: &App) -> Option<(PaneId, usize)> {
        self.work_area()?
            .panes
            .leaves()
            .into_iter()
            .find_map(|(id, pane)| pane.detail_of(target, cx).map(|index| (id, index)))
    }

    /// Appends a tab to the active pane and brings it to the front.
    ///
    /// The tab that was showing stops being rendered the moment this returns,
    /// and it may be holding the keyboard; see [`Workspace::reclaim_focus`].
    /// Nothing is focused in its place here — the callers that open something
    /// typeable do that themselves — so the shell takes the keyboard back.
    fn append_tab(&mut self, item: PaneItem, window: &mut Window, cx: &mut Context<Self>) {
        let Some(target) = self.active_pane() else {
            return;
        };
        if self.pane_holds_focus(target, window, cx) {
            self.focus_shell(window, cx);
        }
        if let Some(pane) = self
            .work_area_mut()
            .and_then(|area| area.panes.get_mut(target))
        {
            pane.push(item);
        }
        cx.notify();
    }

    /// Brings the tab `index` of `pane` to the front, and the pane marker with
    /// it.
    ///
    /// The keyboard follows only if it was inside the tab going off screen:
    /// activating a tab from the explorer, which is where a click that lands
    /// here comes from, must not pull the caret out of the sidebar.
    fn activate_tab(
        &mut self,
        pane: PaneId,
        index: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(area) = self.work_area_mut() else {
            return;
        };
        if !area.panes.contains(pane) {
            return;
        }
        area.active_pane = pane;
        let held = self.pane_holds_focus(pane, window, cx);
        let moved = self
            .work_area_mut()
            .and_then(|area| area.panes.get_mut(pane))
            .is_some_and(|pane| pane.activate(index));
        if moved && held {
            self.focus_active_tab(pane, window, cx);
        }
        cx.notify();
    }

    /// Closes the tab `index` of `pane`.
    ///
    /// Dropping the tab drops the view in it, which closes whatever cursor or
    /// fetch it was holding. The neighbour that takes its place inherits the
    /// keyboard when the closed tab had it, because the tab strip is a place a
    /// user closes several tabs in a row from and a focus that fell back to the
    /// shell every time would swallow the editor shortcuts in between.
    fn close_tab(
        &mut self,
        pane: PaneId,
        index: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let active = self
            .work_area()
            .and_then(|area| area.panes.get(pane))
            .is_some_and(|pane| pane.active_index() == index);
        // Only the active tab is rendered, so only it can be holding the
        // keyboard; see [`Workspace::reclaim_focus`].
        let held = active && self.pane_holds_focus(pane, window, cx);
        let Some(closed) = self
            .work_area_mut()
            .and_then(|area| area.panes.get_mut(pane))
            .and_then(|pane| pane.close(index))
        else {
            return;
        };
        drop(closed);
        if held {
            self.focus_active_tab(pane, window, cx);
        }
        self.drop_stale_confirm(cx);
        cx.notify();
    }

    /// Puts the keyboard on the active tab of `pane`, or on the shell when that
    /// tab has nothing to type into.
    ///
    /// A query pane takes the caret into its editor, which is what makes closing
    /// the tab in front of one leave the user typing where they were. A detail
    /// panel and an empty pane fall back to the shell, whose handlers are what
    /// keep the menu rows and the shortcuts alive.
    fn focus_active_tab(&mut self, pane: PaneId, window: &mut Window, cx: &mut Context<Self>) {
        let query = match self
            .work_area()
            .and_then(|area| area.panes.get(pane))
            .and_then(Pane::active)
        {
            Some(PaneItem::Query { pane, .. }) => Some(pane.clone()),
            _ => None,
        };
        match query {
            Some(pane) => pane.update(cx, |pane, cx| pane.focus_editor(window, cx)),
            None => self.focus_shell(window, cx),
        }
    }

    /// Drops a write confirmation whose pane is no longer open anywhere.
    ///
    /// The dialog asks on behalf of one query pane and sends the answer back to
    /// it; a pane that has been closed has nobody to answer.
    ///
    /// Every connection's work area is searched, not just the one on screen: a
    /// pane of another connection is merely hidden, and a confirmation it is
    /// waiting on is still live. Only closing that connection — which discards
    /// its whole area — makes the question unanswerable.
    fn drop_stale_confirm(&mut self, cx: &mut Context<Self>) {
        let Some(pending) = &self.confirm else {
            return;
        };
        let open = self.connections.iter().any(|open| {
            open.work.panes.leaves().into_iter().any(|(_, pane)| {
                pane.items().iter().any(
                    |item| matches!(item, PaneItem::Query { pane, .. } if pane == &pending.pane),
                )
            })
        });
        if !open {
            self.confirm = None;
            cx.notify();
        }
    }

    /// Fetches everything one detail panel shows.
    fn load_details(
        &mut self,
        panel: Entity<TableDetail>,
        target: ObjectTarget,
        cx: &mut Context<Self>,
    ) {
        let Some(session) = self.session_of(target.connection) else {
            let message = ts!("explorer.disconnected");
            cx.defer(move |cx| {
                panel.update(cx, |panel, cx| panel.deliver(Err(message), cx));
            });
            return;
        };

        let fetch = cx.background_spawn({
            let target = target.clone();
            async move { table_detail::load_details(session.session(), &target) }
        });
        cx.spawn(async move |_workspace, cx| {
            let outcome = fetch.await.map_err(SharedString::from);
            panel
                .update(cx, |panel, cx| panel.deliver(outcome, cx))
                .ok();
        })
        .detach();
    }

    /// Puts the explorer's root for one connection in step with its tab.
    fn sync_explorer_root(&mut self, index: usize, cx: &mut Context<Self>) {
        let Some(open) = self.connections.get(index) else {
            return;
        };
        let info = RootInfo {
            name: if open.profile.name.trim().is_empty() {
                ts!("connect.unnamed")
            } else {
                SharedString::from(open.profile.name.clone())
            },
            color: open.profile.color.clone().map(SharedString::from),
            live: matches!(open.state, ConnectionState::Open(_)),
        };
        let id = open.id;
        let live = info.live;
        self.explorer.update(cx, |explorer, cx| {
            explorer.update_source(cx, |source| source.upsert_root(id, info));
            // A root opened while the handshake was still out answered "the
            // connection is closed"; now that there is a session, that row has
            // to go rather than stay until the tab does.
            if live {
                explorer.reload(&NodeId::Connection(id), cx);
            }
        });
    }

    /// Points the explorer at the connection whose tab is on top.
    ///
    /// Called from everywhere [`Workspace::active_connection`] changes, and from
    /// nowhere else: the tree keeps every root it has ever been given, and this
    /// is the whole of what makes it show one of them. `None` — no connection
    /// open at all — leaves it with an empty root level and its own empty
    /// wording.
    fn sync_visible_root(&mut self, cx: &mut Context<Self>) {
        let visible = self.active_connection().map(|open| open.id);
        self.explorer.update(cx, |explorer, cx| {
            explorer.update_source(cx, |source| source.set_visible_root(visible));
        });
    }

    /// Shows or hides the sidebar, and remembers which.
    fn toggle_explorer(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.explorer_visible = !self.explorer_visible;
        if !self.explorer_visible {
            // The tree takes the focus when a row is clicked, and hiding the
            // sidebar leaves it holding a focus nothing renders any more; see
            // [`Workspace::reclaim_focus`].
            let explorer = self.explorer.read(cx).focus_handle(cx);
            self.reclaim_focus(&explorer, window, cx);
        }
        let mut settings = app_settings::current(cx);
        settings.explorer_visible = self.explorer_visible;
        app_settings::replace(settings, cx);
        cx.notify();
    }

    /// Records what a connection attempt produced.
    fn connected(
        &mut self,
        index: usize,
        outcome: Result<Connected, ConnectError>,
        cx: &mut Context<Self>,
    ) {
        let Some(connection) = self.connections.get_mut(index) else {
            // The tab was closed while the attempt was in flight; the session,
            // if one opened, is closed by `Connected`'s own drop.
            return;
        };

        match outcome {
            Ok(connected) => {
                // A tunnel that dies takes the session above it with it, and the
                // tab has to say so rather than going quiet: the transaction the
                // user was in the middle of is gone (§9.3).
                if let Some(lease) = connected.lease() {
                    let died = lease.watch();
                    cx.spawn(async move |workspace, cx| {
                        let Ok(reason) = died.await else {
                            return;
                        };
                        workspace
                            .update(cx, |workspace, cx| {
                                workspace.tunnel_died(index, reason, cx);
                            })
                            .ok();
                    })
                    .detach();
                }
                connection.state = ConnectionState::Open(Box::new(connected));
            }
            Err(error) => {
                log::warn!("connecting {} failed: {error}", connection.profile.name);
                connection.state = ConnectionState::Failed(error.message().into());
            }
        }
        self.sync_explorer_root(index, cx);
        cx.notify();
    }

    /// Marks a connection dead because the tunnel under it closed, and lets its
    /// editors go of the session.
    ///
    /// A query pane holds a [`connection::SessionHandle`], which is what keeps
    /// the session — and the tunnel under it — alive while a fetch is out. That
    /// is right for a fetch and wrong for a session that has ended: without the
    /// detach below, a dead connection's editors would each hold a handle to a
    /// session nobody can use, and §9.3's rule that a tunnel dies with its
    /// session would hold only until someone had opened an editor.
    ///
    /// The two kinds of tab part company here, because what the user would lose
    /// differs. A query pane holds the statement they typed, so its tab stays
    /// and is detached: the SQL and the rows already fetched remain readable and
    /// copyable, and every path that would talk to the database refuses. A
    /// detail panel shows what the database already said and nothing the user
    /// wrote, so it is simply left alone — a refresh of one finds no session and
    /// says so, the same way the explorer does.
    ///
    /// Nothing is closed and no pane is removed. The connection's tab and its
    /// whole work area stay reachable until the user closes the tab themselves;
    /// a session dying under them must not rearrange their window.
    fn tunnel_died(&mut self, index: usize, reason: String, cx: &mut Context<Self>) {
        let Some(connection) = self.connections.get(index) else {
            return;
        };
        if !matches!(connection.state, ConnectionState::Open(_)) {
            return;
        }
        log::warn!(
            "the tunnel under {} closed: {reason}",
            connection.profile.name
        );

        // Before the state is replaced, so that the handles the editors hold are
        // gone by the time the `Connected` below is dropped and the session can
        // actually close rather than outliving its own tab.
        for pane in connection.work.queries() {
            pane.update(cx, |pane, cx| pane.detach(cx));
        }

        let Some(connection) = self.connections.get_mut(index) else {
            return;
        };
        // Replacing the state drops the `Connected`, which closes the session
        // and releases the lease in that order.
        connection.state = ConnectionState::Dead(ts!("statusbar.tunnel_lost", reason = reason));
        self.sync_explorer_root(index, cx);
        cx.notify();
    }

    /// Closes one connection tab, ending its session and discarding everything
    /// it had open.
    ///
    /// The tab's whole work area goes with it. That is the designed cleanup
    /// path: dropping the area drops every pane, every pane drops its tabs, and
    /// every query tab drops the [`connection::SessionHandle`] and the cursors
    /// it was holding — which is what lets the session below actually close
    /// rather than being kept alive by an editor nobody can run anything in
    /// (architecture document, §9.3). Panes and splits of *other* connections
    /// are untouched, because they were never in this tree.
    ///
    /// Closing the tab that is on top brings another area on screen, so the
    /// keyboard has to be asked about before anything is removed; see
    /// [`Workspace::follow_work_area`].
    fn close_connection(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        if index >= self.connections.len() {
            return;
        }
        // Only the area on screen can be holding the keyboard, so only closing
        // that one moves it.
        let held = index == self.active_connection && self.area_holds_focus(window, cx);

        let connection = self.connections.remove(index);
        let closed = connection.id;
        self.explorer.update(cx, |explorer, cx| {
            explorer.update_source(cx, |source| source.remove_root(closed));
        });
        // Explicitly, and before the session is handed over below: the order is
        // the point.
        drop(connection.work);
        if let ConnectionState::Open(connected) = connection.state {
            // CLOSE_SESSION and then the tunnel, in that order, and off the UI
            // thread because both of them talk to a socket.
            cx.background_spawn(async move {
                if let Err(error) = connected.close() {
                    log::warn!("closing the session failed: {error}");
                }
            })
            .detach();
        }

        // Closing a tab to the left of the active one shifts it along; the tab
        // on top must stay the same one, or the work area would change under a
        // user who closed something else entirely. Closing the last tab of all
        // clamps to whatever is left, and to zero when nothing is.
        if index < self.active_connection {
            self.active_connection -= 1;
        }
        self.active_connection = self
            .active_connection
            .min(self.connections.len().saturating_sub(1));
        self.sync_visible_root(cx);
        self.follow_work_area(held, window, cx);
        self.drop_stale_confirm(cx);
        cx.notify();
    }

    /// Brings one connection tab to the front, and its work area with it.
    ///
    /// The outgoing area stops being rendered entirely, editors and all, so this
    /// is the same focus hazard as hiding the sidebar; see
    /// [`Workspace::follow_work_area`] and [`Workspace::reclaim_focus`].
    fn select_connection(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        if index >= self.connections.len() || self.active_connection == index {
            return;
        }
        let held = self.area_holds_focus(window, cx);
        self.active_connection = index;
        self.sync_visible_root(cx);
        self.follow_work_area(held, window, cx);
        cx.notify();
    }

    /// Whether the work area on screen is holding the keyboard.
    ///
    /// Asked *before* a switch, because it reads the last drawn frame — the one
    /// that still holds the outgoing area.
    fn area_holds_focus(&self, window: &Window, cx: &App) -> bool {
        self.active_pane()
            .is_some_and(|pane| self.pane_holds_focus(pane, window, cx))
    }

    /// Moves the keyboard onto the work area now on screen, when the one that
    /// left had it.
    ///
    /// `held` is what [`Workspace::area_holds_focus`] answered before the
    /// switch. The incoming area's active tab takes the caret if it has one to
    /// take, and the shell takes it otherwise — including when no connection is
    /// left at all. Doing nothing instead would leave the focus on an editor
    /// nothing renders, which swallows every action from then on.
    fn follow_work_area(&mut self, held: bool, window: &mut Window, cx: &mut Context<Self>) {
        if !held {
            return;
        }
        match self.active_pane() {
            Some(pane) => self.focus_active_tab(pane, window, cx),
            None => self.focus_shell(window, cx),
        }
    }

    /// The active pane of the work area on screen.
    ///
    /// `None` with no connection open, which is what makes every pane command
    /// below a no-op rather than an operation on a tree nobody can see.
    fn active_pane(&self) -> Option<PaneId> {
        self.work_area().map(WorkArea::active)
    }

    /// Splits the active pane along `axis` and moves the marker to the new one.
    fn split_active(&mut self, axis: Axis, cx: &mut Context<Self>) {
        let Some(target) = self.active_pane() else {
            return;
        };
        let Some(area) = self.work_area_mut() else {
            return;
        };
        let Some(new) = area.panes.split(target, axis, Pane::new()) else {
            return;
        };
        area.active_pane = new;
        cx.notify();
    }

    /// Closes the active pane, unless it is the last one of its work area.
    ///
    /// The marker moves to the pane that follows the closed one in layout order,
    /// which is the neighbour that grew into the freed space.
    fn close_active_pane(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(target) = self.active_pane() else {
            return;
        };
        let Some(area) = self.work_area() else {
            return;
        };
        let next = area.panes.next_leaf(target).unwrap_or(target);
        // Asked before the pane goes, because afterwards there is nothing left
        // to ask whether its editor or its detail panel had the keyboard.
        let held = self.pane_holds_focus(target, window, cx);
        let Some(area) = self.work_area_mut() else {
            return;
        };
        if area.panes.remove(target).is_none() {
            return;
        }
        area.active_pane = if area.panes.contains(next) {
            next
        } else {
            area.panes.first_leaf().0
        };
        if held {
            self.focus_shell(window, cx);
        }
        self.drop_stale_confirm(cx);
        cx.notify();
    }

    /// Moves the pane marker one step along the layout order.
    fn cycle_pane(&mut self, forward: bool, cx: &mut Context<Self>) {
        let Some(from) = self.active_pane() else {
            return;
        };
        let Some(area) = self.work_area_mut() else {
            return;
        };
        let next = if forward {
            area.panes.next_leaf(from)
        } else {
            area.panes.prev_leaf(from)
        };
        if let Some(next) = next {
            area.active_pane = next;
            cx.notify();
        }
    }

    /// Puts the keyboard back on the shell after a dialog closes.
    fn focus_shell(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        window.focus(&self.focus_handle);
        cx.notify();
    }

    /// Takes the keyboard back from `subtree`, which is about to stop being
    /// rendered, if the focus is anywhere inside it.
    ///
    /// gpui never clears the window focus when the focused element leaves the
    /// tree, and it resolves both dispatched actions and key bindings against
    /// the focused element *of the frame that was last drawn*; an element that
    /// is no longer in it resolves to the window root, which the workspace's
    /// `on_action` handlers do not sit on. A focus left behind on a hidden
    /// sidebar or a closed pane therefore swallows every menu row and every
    /// shortcut, silently and for good.
    ///
    /// This has to run in the same update that removes the subtree, and reads
    /// the *previous* frame — the one that still holds it — which is exactly
    /// what [`FocusHandle::contains_focused`] answers from.
    fn reclaim_focus(
        &mut self,
        subtree: &FocusHandle,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if subtree.contains_focused(window, cx) {
            self.focus_shell(window, cx);
        }
    }

    /// Whether the keyboard is inside what `pane` is showing, as the last drawn
    /// frame had it.
    ///
    /// Tab aware, and only the active tab needs asking: the others are not
    /// rendered, so nothing in them can hold the focus. A query pane answers for
    /// both of its focusable halves — the editor and the grid of the active
    /// result — through [`QueryPane::contains_focus`], because its
    /// [`Focusable`] impl names only the editor and a focus left on a grid would
    /// strand exactly as [`Workspace::reclaim_focus`] describes.
    fn pane_holds_focus(&self, pane: PaneId, window: &Window, cx: &App) -> bool {
        match self
            .work_area()
            .and_then(|area| area.panes.get(pane))
            .and_then(Pane::active)
        {
            Some(PaneItem::Query { pane, .. }) => pane.read(cx).contains_focus(window, cx),
            Some(PaneItem::TableDetail(panel)) => {
                panel.read(cx).focus_handle(cx).contains_focused(window, cx)
            }
            None => false,
        }
    }

    /// Closes every dialog and the dropdown menu.
    ///
    /// Every `open_*` method starts here, which is what keeps the modals
    /// mutually exclusive: only one of them can be on screen at a time, and
    /// opening one always puts the menu away.
    ///
    /// Closing the settings dialog drops its live preview, so the palettes are
    /// re-applied on the way out; without that the window would keep wearing a
    /// theme that nothing in the settings names any more.
    fn close_overlays(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.menu_open = false;
        if self.confirm.is_some() {
            // Declining is the only safe reading of "the dialog went away".
            self.answer_confirm(false, window, cx);
        }
        if self.about.read(cx).is_open() {
            self.about.update(cx, |dialog, cx| dialog.close(cx));
        }
        if self.connect.read(cx).is_open() {
            self.connect.update(cx, |dialog, cx| dialog.close(cx));
        }
        if self.settings.read(cx).is_open() {
            self.settings.update(cx, |dialog, cx| dialog.close(cx));
            self.apply_preview(window, cx);
        }
    }

    /// Shows or hides the application dropdown menu.
    fn set_menu_open(&mut self, open: bool, cx: &mut Context<Self>) {
        if self.menu_open != open {
            self.menu_open = open;
            cx.notify();
        }
    }

    /// Opens the about dialog, closing whatever else was showing.
    fn open_about(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.close_overlays(window, cx);
        self.about.update(cx, |dialog, cx| dialog.open(cx));
        cx.notify();
    }

    /// Opens the settings dialog, closing whatever else was showing.
    fn open_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.close_overlays(window, cx);
        self.settings.update(cx, |dialog, cx| dialog.open(cx));
        cx.notify();
    }

    /// Re-applies the saved settings to the window.
    ///
    /// The counterpart of the start-up sequence in [`main`], and the only place
    /// a setting reaches a *live* window: the language the next frame is built
    /// in, the menu bar the platform owns, both palettes, the title bar style
    /// and the surface's background treatment.
    ///
    /// Deliberately does not move the focus — where the focus belongs after this
    /// depends on whether the dialog closed, which only the caller knows.
    ///
    /// Every platform call in here acts on the window, and one of them —
    /// `request_decorations` on X11 — is the call that used to re-enter gpui's
    /// window callbacks and panic. It is safe from this stack: the settings
    /// dialog emits its event, gpui delivers it after the button's own callback
    /// has returned and released every borrow, and this runs from there. It must
    /// stay that way; calling it from inside a widget callback would put the
    /// borrow back.
    fn apply_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let settings = app_settings::current(cx);
        // The sidebar is not in the settings dialog, but the file it writes
        // carries the width and the visibility all the same, so the live window
        // follows what was actually saved.
        self.explorer_visible = settings.explorer_visible;
        self.explorer_width = settings.explorer_width;
        // Before the repaint below, so the next frame is already drawn in the
        // newly chosen language.
        i18n::apply(settings.language.as_deref());
        // The native macOS menu bar is built once and owned by the platform, so
        // unlike the in-app menu it does not follow a repaint; it has to be
        // handed over again.
        cx.set_menus(app_menus());
        apply_themes(&settings, cx);
        // Ahead of the repaint, so the toolbar's next frame already knows
        // whether it has to stand in for a title bar; and ahead of the two calls
        // below, which leave the accent policy and the caption colors on the
        // window, so a caption that comes back here comes back already themed.
        //
        // The field follows the call rather than the stored setting: everything
        // that branches on it is asking what the window carries, not what was
        // last saved.
        if settings.window.titlebar != self.titlebar {
            self.titlebar = settings.window.titlebar;
            let custom = self.titlebar == TitlebarStyle::Custom;
            window.set_titlebar_transparent(custom, custom.then_some(TRAFFIC_LIGHT_ORIGIN));
            // The Linux counterpart of the call above, which only the Windows
            // and macOS backends implement: swap the compositor's frame for
            // client-side decorations (or back) on the live window.
            #[cfg(not(any(target_os = "windows", target_os = "macos")))]
            window.request_decorations(if custom {
                gpui::WindowDecorations::Client
            } else {
                gpui::WindowDecorations::Server
            });
        }
        cx.refresh_windows();
        window.set_background_appearance(window_appearance(&settings.window));
        // After the background appearance, never before: on Windows that call
        // re-arms the accent policy that would otherwise repaint the caption out
        // from under us.
        apply_caption_theme(window, &theme(cx));
    }

    /// Re-applies the palettes the settings dialog is currently showing.
    ///
    /// The unsaved half of [`Workspace::apply_settings`], and deliberately much
    /// smaller: only the two palettes and the fonts are previewed, so this
    /// touches no platform state beyond the native caption's colours, which have
    /// to follow the chrome theme or the window would be half repainted.
    ///
    /// Reads [`app_settings::effective`], which answers the preview while one is
    /// installed and the saved settings once it is dropped — so the same call
    /// both applies a preview and undoes it.
    fn apply_preview(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        apply_themes(&app_settings::effective(cx), cx);
        cx.refresh_windows();
        apply_caption_theme(window, &theme(cx));
    }

    /// Opens the connection dialog, closing whatever else was showing.
    fn open_connect(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.close_overlays(window, cx);
        self.connect.update(cx, |dialog, cx| dialog.open(cx));
        cx.notify();
    }

    /// Opens the connection dialog.
    fn new_connection_action(
        &mut self,
        _: &NewConnection,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.set_menu_open(false, cx);
        self.open_connect(window, cx);
    }

    /// Opens the settings dialog.
    fn open_settings_action(
        &mut self,
        _: &OpenSettings,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_settings(window, cx);
    }

    /// Opens the about dialog.
    fn show_about_action(&mut self, _: &ShowAbout, window: &mut Window, cx: &mut Context<Self>) {
        self.open_about(window, cx);
    }

    /// Shows or hides the explorer sidebar.
    fn toggle_explorer_action(
        &mut self,
        _: &ToggleExplorer,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.set_menu_open(false, cx);
        self.toggle_explorer(window, cx);
    }

    /// Opens an empty query pane on the connection whose tab is showing.
    fn new_query_action(&mut self, _: &NewQuery, window: &mut Window, cx: &mut Context<Self>) {
        self.set_menu_open(false, cx);
        self.open_query("", window, cx);
    }

    /// Opens a query pane over the object selected in the explorer.
    ///
    /// Scoped to the sidebar's key context, so the same chord means "run the
    /// statement" once the focus is in a SQL editor.
    fn query_object_action(
        &mut self,
        _: &QueryObject,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.set_menu_open(false, cx);
        let Some(target) = self.explorer.read(cx).selected_relation(cx) else {
            return;
        };
        self.open_query_for(&target, window, cx);
    }

    /// Closes the active pane.
    fn close_pane_action(&mut self, _: &ClosePane, window: &mut Window, cx: &mut Context<Self>) {
        self.close_active_pane(window, cx);
    }

    /// Moves the pane marker forwards.
    fn focus_next_pane_action(
        &mut self,
        _: &FocusNextPane,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.cycle_pane(true, cx);
    }

    /// Moves the pane marker backwards.
    fn focus_prev_pane_action(
        &mut self,
        _: &FocusPrevPane,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.cycle_pane(false, cx);
    }

    /// Splits the active pane to the right.
    fn split_right_action(&mut self, _: &SplitRight, _window: &mut Window, cx: &mut Context<Self>) {
        self.split_active(Axis::Horizontal, cx);
    }

    /// Splits the active pane downwards.
    fn split_below_action(&mut self, _: &SplitBelow, _window: &mut Window, cx: &mut Context<Self>) {
        self.split_active(Axis::Vertical, cx);
    }

    /// Closes whatever overlay is on top, in the order they are stacked.
    fn dismiss_dialog_action(
        &mut self,
        _: &DismissDialog,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // The dropdown menu paints above everything else, so it goes first.
        if self.menu_open {
            self.set_menu_open(false, cx);
            return;
        }
        if self.confirm.is_some() {
            self.answer_confirm(false, window, cx);
            self.focus_shell(window, cx);
            return;
        }
        if self.about.read(cx).is_open() {
            self.about.update(cx, |dialog, cx| dialog.close(cx));
            self.focus_shell(window, cx);
            return;
        }
        if self.connect.read(cx).is_open() {
            // Routed through the dialog for the same reason the settings one is:
            // it stacks a driver manager, a dropdown and a delete confirmation,
            // and each of those has to be able to take `Escape` for itself
            // before the whole form is thrown away.
            self.connect.update(cx, |dialog, cx| dialog.escape(cx));
            return;
        }
        if self.settings.read(cx).is_open() {
            // Routed through the dialog rather than closed from here: it stacks
            // a colour editor, two dropdowns and a delete confirmation of its
            // own, and each of those has to be able to take `Escape` for itself
            // before the whole form is thrown away. gpui matches key bindings
            // ahead of key listeners, so this handler — not the dialog's own —
            // is where the key actually lands.
            self.settings.update(cx, |dialog, cx| dialog.escape(cx));
            return;
        }
        cx.propagate();
    }

    /// Renders the toolbar: the application menu button and the tab strip.
    ///
    /// The button is left out on macOS, where [`app_menus`] puts the same
    /// commands in the system menu bar.
    ///
    /// In the custom title bar style this row *is* the title bar. It then marks
    /// itself as the window's drag area, takes over writing the application's
    /// name at its left end, and — off macOS, which keeps its native traffic
    /// lights — grows a set of caption buttons at its right end. Every *control*
    /// inside it occludes, so the drag area only ever answers for the gaps
    /// between them; see [`rudbman_ui::window_controls`]. The name is not a
    /// control and deliberately does not.
    fn render_toolbar(&self, window: &Window, cx: &mut Context<Self>) -> AnyElement {
        let theme = theme(cx);
        let custom = draws_own_titlebar(self.titlebar, window);
        let menu = (!cfg!(target_os = "macos")).then(|| self.render_app_menu(cx));
        // Built before the row is assembled: both of these borrow the context to
        // register listeners, and the builders below borrow it to read the theme.
        let tab_bar = self.render_tab_bar(cx);

        // One cell for the leading controls, so the menu button shares the
        // toolbar's fill and bottom hairline with the strip.
        let leading = menu.map(|menu| {
            div()
                .flex()
                .flex_row()
                .flex_none()
                .items_center()
                .gap(px(2.))
                .h(px(TOOLBAR_HEIGHT))
                .px(px(4.))
                .bg(theme.surface)
                .border_b_1()
                .border_color(theme.border)
                .child(menu)
        });

        // Room for the traffic lights AppKit still draws over the transparent
        // title bar. Painted like the leading cell rather than left empty, so
        // the band reads as one strip. Fullscreen hides the buttons, and the
        // gap goes with them.
        let traffic_lights =
            (custom && cfg!(target_os = "macos") && !window.is_fullscreen()).then(|| {
                div()
                    .flex_none()
                    .w(px(TRAFFIC_LIGHT_GAP))
                    .h(px(TOOLBAR_HEIGHT))
                    .bg(theme.surface)
                    .border_b_1()
                    .border_color(theme.border)
            });

        // The application's own name, which only the custom style has to write:
        // a system title bar already carries it, and drawing it twice would put
        // it in two places at once.
        //
        // Windows and the GTK/KDE captions set an application icon beside the
        // title and macOS does not, so the mark follows that split.
        //
        // Nothing here is interactive, and — unlike every control in this row —
        // nothing here occludes either. The name and the mark are part of the
        // *empty* title bar as far as the window is concerned, so a press on
        // them has to reach the drag area underneath and move the window.
        let title = custom.then(|| {
            // Tinted like the other icons of the row rather than painted in the
            // shipped icon's own colours; see [`icons::LOGO`].
            let icon =
                (!cfg!(target_os = "macos")).then(|| icons::icon(icons::LOGO, px(16.), theme.icon));
            div()
                .flex()
                .flex_row()
                .flex_none()
                .items_center()
                .gap(px(6.))
                .h(px(TOOLBAR_HEIGHT))
                .px(px(10.))
                .bg(theme.surface)
                .border_b_1()
                .border_color(theme.border)
                // A shade quieter than a tab title, which is the one label in
                // this row that has to be read.
                .text_size(px(12.))
                .text_color(theme.text_muted)
                .children(icon)
                .child(APP_NAME)
        });

        // The caption buttons the other two platforms have to draw themselves.
        let controls = (custom && !cfg!(target_os = "macos")).then(|| {
            WindowControls::new(
                "window-controls",
                WindowControlIcons {
                    minimize: icons::WINDOW_MINIMIZE.into(),
                    maximize: icons::WINDOW_MAXIMIZE.into(),
                    restore: icons::WINDOW_RESTORE.into(),
                    close: icons::WINDOW_CLOSE.into(),
                },
            )
        });

        div()
            .id("toolbar")
            .flex()
            .flex_row()
            .flex_none()
            .items_center()
            .w_full()
            .h(px(TOOLBAR_HEIGHT))
            .when(custom, |this| {
                // Occluding is load-bearing, not just hygiene: the workspace
                // root tracks focus, and gpui's focus transfer marks every
                // mouse down over it `default_prevented` — which the Windows
                // backend reads as "the app took this press", swallowing the
                // `HTCAPTION` down that would have started the system drag.
                // Cutting the root's hitbox out from under the strip keeps the
                // press unclaimed.
                titlebar_gestures(this.occlude().window_control_area(WindowControlArea::Drag))
            })
            .children(traffic_lights)
            .children(title)
            .children(leading)
            .child(div().flex_1().min_w_0().child(tab_bar))
            .children(controls)
            .into_any_element()
    }

    /// Builds the dropdown menu shown on the platforms without a native one.
    ///
    /// Every row dispatches the action its keyboard shortcut dispatches, so the
    /// menu adds a way in rather than a second implementation.
    ///
    /// The pane commands are deliberately absent. They act on panes that hold
    /// nothing yet, so a row offering to split the empty state would promise
    /// something it cannot deliver; the shortcuts stay bound so that the layout
    /// code is exercised, and the rows arrive with the panels in M2.
    fn render_app_menu(&self, cx: &mut Context<Self>) -> MenuButton {
        let this = cx.entity();
        let entries = vec![
            MenuEntry::new(ts!("menu.new_connection"))
                .shortcut(format!("{SHORTCUT_MODIFIER}+N"))
                .on_activate(|window, cx| window.dispatch_action(Box::new(NewConnection), cx)),
            MenuEntry::new(ts!("menu.new_query"))
                .shortcut(format!("{SHORTCUT_MODIFIER}+T"))
                .on_activate(|window, cx| window.dispatch_action(Box::new(NewQuery), cx)),
            MenuEntry::new(ts!("menu.query_object"))
                .shortcut(format!("{SHORTCUT_MODIFIER}+Enter"))
                .on_activate(|window, cx| window.dispatch_action(Box::new(QueryObject), cx)),
            MenuEntry::new(ts!("menu.toggle_explorer"))
                .shortcut(format!("{SHORTCUT_MODIFIER}+B"))
                .on_activate(|window, cx| window.dispatch_action(Box::new(ToggleExplorer), cx)),
            MenuEntry::new(ts!("menu.settings"))
                .shortcut(format!("{SHORTCUT_MODIFIER}+,"))
                .on_activate(|window, cx| window.dispatch_action(Box::new(OpenSettings), cx)),
            MenuEntry::separator(),
            MenuEntry::new(ts!("menu.about"))
                .on_activate(|window, cx| window.dispatch_action(Box::new(ShowAbout), cx)),
            MenuEntry::separator(),
            MenuEntry::new(ts!("menu.quit"))
                .shortcut(format!("{SHORTCUT_MODIFIER}+Q"))
                .on_activate(|window, cx| window.dispatch_action(Box::new(Quit), cx)),
        ];

        MenuButton::new("app-menu")
            .tooltip(ts!("menu.tip_menu"))
            .open(self.menu_open)
            .entries(entries)
            .on_open_change(move |open, _window, cx| {
                this.update(cx, |workspace, cx| workspace.set_menu_open(open, cx));
            })
    }

    /// Renders the tab strip: one tab per open connection.
    ///
    /// The title is the profile's name and the dot is where the session has got
    /// to, so a tab that is still connecting, one that is live and one whose
    /// tunnel died are told apart without opening any of them.
    ///
    /// Selecting a tab is the window's one mode switch: it swaps the whole work
    /// area below and the explorer's root along with it.
    fn render_tab_bar(&self, cx: &mut Context<Self>) -> TabBar {
        let this = cx.entity();
        let tabs: Vec<TabItem> = self
            .connections
            .iter()
            .enumerate()
            .map(|(index, connection)| {
                let title = if connection.profile.name.trim().is_empty() {
                    ts!("connect.unnamed")
                } else {
                    SharedString::from(connection.profile.name.clone())
                };
                TabItem::new(("connection", index), title).status(connection.state.tab_status())
            })
            .collect();

        TabBar::new("connection-tabs")
            .tabs(tabs)
            .active(self.active_connection)
            .on_select({
                let this = this.clone();
                move |index, window, cx| {
                    this.update(cx, |workspace, cx| {
                        workspace.select_connection(index, window, cx);
                    });
                }
            })
            .on_close({
                let this = this.clone();
                move |index, window, cx| {
                    this.update(cx, |workspace, cx| {
                        workspace.close_connection(index, window, cx)
                    });
                }
            })
            .scroll_handle(&self.tab_scroll)
            .scrollbar(self.tab_scrollbar())
            .menu_icon(icons::TAB_LIST)
            .new_icon(icons::NEW_TAB)
            // The close button reuses the tab menu's own row: it is the same
            // command, worded the same way, and neither takes an ellipsis.
            .tooltips(
                ts!("tab.tip_list"),
                ts!("tab.tip_new", shortcut = format!("{SHORTCUT_MODIFIER}+N")),
                ts!("tab.close"),
            )
            .on_new(|window, cx| window.dispatch_action(Box::new(NewConnection), cx))
    }

    /// Renders the work area: the sidebar, and the active connection's panes.
    ///
    /// With no connection open there is no work area to draw, so the empty state
    /// stands in for the whole of it — the same words a pane with no tabs shows,
    /// in the wording that says there is nothing to connect to.
    fn render_body(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = theme(cx);
        let work = self.work_area().map(|area| {
            let chrome = PaneChrome {
                active: area.active(),
                // A lone pane with nothing beside it needs no frame: there is
                // only one thing on screen, so nothing has to be said about
                // which one is active.
                frame: area.panes.leaf_count() > 1,
                theme: theme.clone(),
                colors: self
                    .connections
                    .iter()
                    .filter_map(|open| {
                        let color = rudbman_ui::parse_hex(open.profile.color.as_deref()?)?;
                        Some((open.id, color))
                    })
                    .collect(),
            };
            (area.panes.root(), chrome)
        });

        // The sidebar and the handle that resizes it, both left out entirely
        // when the panel is hidden — a zero-width flex child would still take
        // the divider's hit area with it.
        let sidebar = self.explorer_visible.then(|| {
            div()
                .flex()
                .flex_none()
                .w(px(self.explorer_width))
                .min_h_0()
                .child(self.explorer.clone())
        });
        let handle = self.explorer_visible.then(|| {
            div()
                .id("explorer-divider")
                .occlude()
                .flex_none()
                .w(px(SPLIT_HANDLE))
                // Pulled back over the sidebar's own border so the grab area
                // straddles the seam rather than pushing the work area across.
                .ml(px(-SPLIT_HANDLE))
                .cursor_ew_resize()
                .on_drag(DraggedExplorer, |_, _, _, cx| cx.new(|_| gpui::Empty))
        });

        div()
            .flex()
            .flex_row()
            .flex_grow()
            .min_w_0()
            .min_h_0()
            // The only fill covering the body, which is what makes it the one
            // place the window opacity may be applied; see
            // [`app_settings::window_tint`].
            .bg(app_settings::window_tint(theme.background, cx))
            // Measured against this box rather than tracked as a delta, exactly
            // as a split divider is: the width follows the pointer however far
            // the gesture wandered.
            .on_drag_move::<DraggedExplorer>(cx.listener(
                |workspace, event: &DragMoveEvent<DraggedExplorer>, _window, cx| {
                    workspace.drag_explorer(event, cx);
                },
            ))
            .children(sidebar)
            .children(handle)
            .child(div().flex().flex_1().min_w_0().min_h_0().child(match work {
                Some((root, chrome)) => render_pane(root, &chrome, cx),
                None => render_placeholder(false, &theme),
            }))
            .into_any_element()
    }

    /// Moves the sidebar's edge to wherever the pointer has dragged it.
    ///
    /// The width is persisted on release rather than per move: a drag is
    /// hundreds of events and `settings.json` is written once when the window
    /// closes, so writing the global on every one of them would be the only
    /// thing in the frame doing work.
    fn drag_explorer(&mut self, event: &DragMoveEvent<DraggedExplorer>, cx: &mut Context<Self>) {
        let width = f32::from(event.event.position.x - event.bounds.left());
        if !width.is_finite() {
            return;
        }
        let width = width.clamp(MIN_EXPLORER_WIDTH, MAX_EXPLORER_WIDTH);
        if (self.explorer_width - width).abs() > f32::EPSILON {
            self.explorer_width = width;
            cx.notify();
        }
    }

    /// Writes the sidebar's width into the settings global.
    ///
    /// Called when the drag ends; [`app_settings::save`] takes it to disk with
    /// everything else when the last window closes.
    fn release_explorer(&mut self, cx: &mut Context<Self>) {
        let mut settings = app_settings::current(cx);
        if (settings.explorer_width - self.explorer_width).abs() > f32::EPSILON {
            settings.explorer_width = self.explorer_width;
            app_settings::replace(settings, cx);
        }
    }

    /// Moves the divider of `split` to wherever the pointer has dragged it.
    ///
    /// The share is measured against the split's own box rather than tracked as
    /// a delta, so the divider sits under the pointer however far the gesture
    /// wandered — including outside the window, which a delta would have to keep
    /// integrating. [`MIN_SPLIT_RATIO`] stops it short of either edge.
    fn drag_split(
        &mut self,
        split: SplitId,
        axis: Axis,
        event: &DragMoveEvent<DraggedSplit>,
        cx: &mut Context<Self>,
    ) {
        // Enclosing splits see the same moves, so a listener has to check that
        // the divider being dragged is the one it renders.
        if event.drag(cx).split != split {
            return;
        }

        let bounds = event.bounds;
        let position = event.event.position;
        let share = match axis {
            Axis::Horizontal => (position.x - bounds.left()) / bounds.size.width,
            Axis::Vertical => (position.y - bounds.top()) / bounds.size.height,
        };
        // Zero-sized bounds cannot happen in a laid-out frame, but the division
        // above says otherwise; a `NaN` would poison the stored ratio for good.
        if !share.is_finite() {
            return;
        }

        let clamped = share.clamp(MIN_SPLIT_RATIO, 1. - MIN_SPLIT_RATIO);
        if self
            .work_area_mut()
            .is_some_and(|area| area.panes.set_ratio(split, clamped))
        {
            cx.notify();
        }
    }

    /// The tab strip's overlay scroll indicator, as it stands.
    ///
    /// Rebuilt on demand rather than kept, because everything it is made of —
    /// the strip's box, how far it overflows, where it sits — is measured afresh
    /// by gpui on every layout pass.
    fn tab_scrollbar(&self) -> Scrollbar {
        Scrollbar::for_handle(TAB_SCROLLBAR, ScrollbarAxis::Horizontal, &self.tab_scroll)
            .fade(self.tab_scrollbar.fade())
    }

    /// Puts the strip's bar up whenever the strip has moved, and starts the
    /// clock that takes it down again.
    ///
    /// Called from `render` because that is where every way of scrolling the
    /// strip meets: a wheel over the tabs, and the jump that brings a newly
    /// activated tab back into view.
    fn watch_tab_scroll(&mut self, cx: &mut Context<Self>) {
        let scrolled = scrolled(&self.tab_scroll, ScrollbarAxis::Horizontal);
        if let Some(epoch) = self.tab_scrollbar.moved(scrolled) {
            hide_later(epoch, cx, |workspace| Some(&mut workspace.tab_scrollbar));
        }
    }

    /// Scrolls the tab strip to wherever its thumb has been dragged.
    ///
    /// Every element listening for this drag type hears every such drag, so the
    /// bar checks that the one being dragged is its own before answering.
    fn drag_tab_scrollbar(&mut self, event: &DragMoveEvent<DraggedThumb>, cx: &mut Context<Self>) {
        let Some(progress) = self.tab_scrollbar().dragged(event, cx) else {
            return;
        };

        // Held even when the pointer moved along the other axis and the strip
        // has not budged: the bar has to stay up for as long as it is being
        // held, and a still pointer moves nothing to notice.
        self.tab_scrollbar.hold();
        scroll_to(&self.tab_scroll, ScrollbarAxis::Horizontal, progress);
        cx.notify();
    }

    /// Lets go of the strip's thumb, and starts the clock on the bar again.
    ///
    /// Every mouse release in the window arrives here; all but the one ending a
    /// drag of this bar find nothing to let go of.
    fn release_tab_scrollbar(&mut self, cx: &mut Context<Self>) {
        if let Some(epoch) = self.tab_scrollbar.release() {
            hide_later(epoch, cx, |workspace| Some(&mut workspace.tab_scrollbar));
            cx.notify();
        }
    }

    /// Renders the bottom status bar.
    ///
    /// The layout is the one the architecture document asks for — connection,
    /// transaction state, rows, elapsed time. The first cell names the database
    /// product and version the active session reported, and the second says
    /// what the session is doing; the last two stand empty until there is a
    /// statement behind them, which is M3.
    ///
    /// The second cell is where a failure is written out, which is why it is the
    /// one that shrinks: a driver's refusal is the longest text this row ever
    /// carries.
    ///
    /// The two texts come from [`Workspace::status_cells`], which is separate so
    /// that "the bar says H2 2.3.232" can be asserted without laying out a
    /// window.
    fn status_cells(&self) -> (SharedString, SharedString) {
        let Some(connection) = self.active_connection() else {
            return (ts!("statusbar.no_connection"), ts!("statusbar.idle"));
        };
        // The product and its version once the session has answered
        // SESSION_INFO, and the profile's own name until then — a cell that went
        // blank while connecting would read as no connection at all.
        let label = match &connection.state {
            ConnectionState::Open(connected) => connected
                .product()
                .map(SharedString::from)
                .unwrap_or_else(|| SharedString::from(connection.profile.name.clone())),
            _ => SharedString::from(connection.profile.name.clone()),
        };
        let state = match &connection.state {
            ConnectionState::Connecting { .. } => ts!("statusbar.connecting"),
            ConnectionState::Open(_) => ts!("statusbar.connected"),
            ConnectionState::Failed(error) => ts!("statusbar.failed", error = error.to_string()),
            ConnectionState::Dead(reason) => reason.clone(),
        };
        (label, state)
    }

    /// The two right-hand status bar cells — rows and elapsed time — which the
    /// active query pane owns.
    ///
    /// Blank for every other kind of pane, because there is nothing running
    /// behind them to count.
    fn query_cells(&self, cx: &App) -> (SharedString, SharedString) {
        self.active_query()
            .map(|pane| pane.read(cx).status_cells())
            .unwrap_or_default()
    }

    fn render_status_bar(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = theme(cx);
        let (connection, state) = self.status_cells();
        let (rows, elapsed) = self.query_cells(cx);
        let state_color = match self.active_connection().map(|open| &open.state) {
            Some(ConnectionState::Open(_)) => Some(theme.success),
            Some(ConnectionState::Failed(_) | ConnectionState::Dead(_)) => Some(theme.danger),
            _ => None,
        };

        div()
            .flex()
            .flex_row()
            .flex_none()
            .items_center()
            .gap(px(14.))
            .h(px(STATUS_BAR_HEIGHT))
            .px(px(10.))
            // The bar is inert, so a press on it must not move the keyboard.
            // Without this the workspace root's `track_focus` would claim the
            // click.
            .on_any_mouse_down(|_, window, _cx| window.prevent_default())
            .bg(theme.surface)
            .border_t_1()
            .border_color(theme.border)
            .text_size(px(11.))
            .text_color(theme.text_muted)
            .child(div().flex_none().whitespace_nowrap().child(connection))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .when_some(state_color, |cell, color| cell.text_color(color))
                    .child(state),
            )
            .child(
                div()
                    .flex_none()
                    .whitespace_nowrap()
                    .child(if rows.is_empty() { NOTHING } else { rows }),
            )
            .child(
                div()
                    .flex_none()
                    .whitespace_nowrap()
                    .child(if elapsed.is_empty() { NOTHING } else { elapsed }),
            )
            .into_any_element()
    }

    /// The write confirmation, while a query pane is waiting on one.
    ///
    /// Two buttons and the statement itself: a dialog that asked "are you
    /// sure?" without showing what it is about would be asking the user to
    /// remember rather than to read.
    fn render_confirm(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let pending = self.confirm.as_ref()?;
        let theme = theme(cx);
        let this = cx.entity();
        let dismiss = this.clone();

        let body = div()
            .flex()
            .flex_col()
            .gap(px(12.))
            .child(
                div()
                    .text_size(px(12.))
                    .text_color(theme.text)
                    .child(ts!("query.confirm_body", count = pending.request.count)),
            )
            .child(
                div()
                    .id("confirm-preview")
                    .max_h(px(200.))
                    .overflow_y_scroll()
                    .p(px(8.))
                    .rounded_md()
                    .bg(theme.surface)
                    .border_1()
                    .border_color(theme.border)
                    .text_size(px(11.))
                    .text_color(theme.text_muted)
                    .child(pending.request.preview.clone()),
            )
            .child(
                div()
                    .flex()
                    .flex_row()
                    .justify_end()
                    .gap(px(8.))
                    .child(
                        Button::new("confirm-cancel", ts!("common.cancel"))
                            .variant(ButtonVariant::Secondary)
                            .on_click({
                                let this = this.clone();
                                move |_, window, cx| {
                                    this.update(cx, |workspace, cx| {
                                        workspace.answer_confirm(false, window, cx);
                                    });
                                }
                            }),
                    )
                    .child(
                        Button::new("confirm-run", ts!("query.confirm_run"))
                            .variant(ButtonVariant::Danger)
                            .on_click(move |_, window, cx| {
                                this.update(cx, |workspace, cx| {
                                    workspace.answer_confirm(true, window, cx);
                                });
                            }),
                    ),
            );

        Some(
            modal(
                "query-confirm",
                ts!("query.confirm_title"),
                px(460.),
                body,
                move |window, cx| {
                    dismiss.update(cx, |workspace, cx| {
                        workspace.answer_confirm(false, window, cx);
                    });
                },
            )
            .into_any_element(),
        )
    }

    /// Answers the write confirmation, one way or the other.
    fn answer_confirm(&mut self, run: bool, window: &mut Window, cx: &mut Context<Self>) {
        let Some(pending) = self.confirm.take() else {
            return;
        };
        pending.pane.update(cx, |pane, cx| {
            if run {
                pane.confirmed(cx);
            } else {
                pane.declined(cx);
            }
        });
        if run {
            pending
                .pane
                .update(cx, |pane, cx| pane.focus_editor(window, cx));
        }
        cx.notify();
    }
}

/// What every pane of one frame is drawn against.
///
/// Gathered once by [`Workspace::render_body`] and handed down the recursion
/// rather than recomputed per leaf: the palette and the connection colours are
/// the same for every pane on screen, and the alternative is a `render_pane`
/// with seven parameters, most of them constants of the frame.
struct PaneChrome {
    /// The pane the marker is on, drawn with an accent frame.
    active: PaneId,
    /// Whether panes are framed at all, which they are once there are two.
    frame: bool,
    /// The palette.
    theme: Theme,
    /// The colour tag of every connection that has one, for the tab dots.
    ///
    /// A profile with no colour is simply absent, and its tabs draw no dot: an
    /// invented colour would read as a tag the user chose.
    colors: HashMap<ConnectionId, Hsla>,
}

/// Renders one pane's tab strip.
///
/// The same widget as the connection strip at the top of the window, minus the
/// dropdown and the "+": a pane's tabs are opened from the explorer and the
/// query commands, so a "new tab" button here would have nothing to open.
///
/// Every tab of the strip belongs to the connection whose tab is on top — the
/// work area is that connection's — so the dots all carry one colour, the one
/// the explorer marks that connection's root with. That is the point of keeping
/// them: with several connections open, a glance at a pane says which database
/// its panels are about without reading the strip above the window.
fn render_tab_strip(
    id: PaneId,
    pane: &Pane,
    chrome: &PaneChrome,
    cx: &mut Context<Workspace>,
) -> TabBar {
    let this = cx.entity();
    let tabs: Vec<TabItem> = pane
        .items()
        .iter()
        .enumerate()
        .map(|(index, item)| {
            let tab = TabItem::new(("pane-tab", index), item.title(cx));
            match chrome.colors.get(&item.connection(cx)) {
                Some(color) => tab.dot(*color),
                None => tab,
            }
        })
        .collect();

    TabBar::new(("pane-tabs", id.as_u64()))
        .tabs(tabs)
        .active(pane.active_index())
        .scroll_handle(pane.scroll_handle())
        // Only the third slot can ever be read: this strip carries neither the
        // dropdown nor the "+" the first two label.
        .tooltips("", "", ts!("tab.close"))
        .on_select({
            let this = this.clone();
            move |index, window, cx| {
                this.update(cx, |workspace, cx| {
                    workspace.activate_tab(id, index, window, cx);
                });
            }
        })
        .on_close(move |index, window, cx| {
            this.update(cx, |workspace, cx| {
                workspace.close_tab(id, index, window, cx);
            });
        })
}

/// Renders one node of a pane tree.
///
/// A split becomes a flex box in the direction of its axis, with each child
/// sized by `flex_basis`; the `min_w_0` / `min_h_0` on the box *and* on both
/// children is what lets those bases actually divide the space, instead of the
/// content inside insisting on its measured width.
///
/// When `frame` is set — the work area holds more than one pane — every leaf is
/// framed with a hairline, accent coloured on the active one. The frames double
/// as the divider between neighbours, which is why there is no separate divider
/// element: a third hairline squeezed between two of them would only thicken the
/// seam. Every pane is framed, not just the active one, so that moving the
/// marker recolours a frame without shifting the layout by a pixel. It is a
/// border rather than a fill because a translucent window allows only one tinted
/// fill per pixel and the body already owns it.
///
/// A split also lays an invisible handle over its divider, last so that it wins
/// the hit test against the panes it straddles, and positioned absolutely so
/// that it can straddle them at all: an in-flow handle would have to be given
/// room, which is exactly what the hairline seam is meant not to need.
fn render_pane(
    node: &PaneNode<Pane>,
    chrome: &PaneChrome,
    cx: &mut Context<Workspace>,
) -> AnyElement {
    match node {
        PaneNode::Leaf { id, payload } => {
            let theme = &chrome.theme;
            let border = if *id == chrome.active {
                theme.accent
            } else {
                theme.border
            };
            let body = match payload.active() {
                Some(PaneItem::TableDetail(panel)) => panel.clone().into_any_element(),
                Some(PaneItem::Query { pane, .. }) => pane.clone().into_any_element(),
                // A work area belongs to a connection, so a pane inside one is
                // empty because nothing that would fill it has been opened yet
                // — never because there is nothing to connect to.
                None => render_placeholder(true, theme),
            };
            div()
                .id(("pane", id.as_u64()))
                .flex()
                .flex_col()
                .size_full()
                .min_w_0()
                .min_h_0()
                .when(chrome.frame, |pane| pane.border_1().border_color(border))
                // No strip over an empty pane: a bar with nothing in it would
                // be a band of chrome saying nothing over the very words that
                // explain what the pane is for.
                .children((!payload.is_empty()).then(|| {
                    div()
                        .flex()
                        .flex_none()
                        .w_full()
                        .child(render_tab_strip(*id, payload, chrome, cx))
                }))
                .child(div().flex().flex_1().min_w_0().min_h_0().child(body))
                .into_any_element()
        }
        PaneNode::Split {
            id,
            axis,
            ratio,
            first,
            second,
        } => {
            let id = *id;
            let axis = *axis;
            let ratio = ratio.clamp(MIN_SPLIT_RATIO, 1. - MIN_SPLIT_RATIO);
            // Both children are rendered up front because each one needs `cx`
            // for the handles further down the tree, and a closure holding it
            // could not then be called twice.
            let first = render_pane(first, chrome, cx);
            let second = render_pane(second, chrome, cx);
            let half = |share: f32, child: AnyElement| {
                div()
                    .flex()
                    .flex_basis(relative(share))
                    .min_w_0()
                    .min_h_0()
                    .child(child)
            };
            // Centred on the seam by pulling it back half its own thickness, so
            // the grab area is symmetric about the line the user sees.
            let offset = px(-SPLIT_HANDLE / 2.);
            let handle = div()
                .id(("split-handle", id.as_u64()))
                .absolute()
                // A plain hitbox does not stop events reaching what is under it.
                .occlude()
                .map(|handle| match axis {
                    Axis::Horizontal => handle
                        .top_0()
                        .bottom_0()
                        .left(relative(ratio))
                        .ml(offset)
                        .w(px(SPLIT_HANDLE))
                        .cursor_ew_resize(),
                    Axis::Vertical => handle
                        .left_0()
                        .right_0()
                        .top(relative(ratio))
                        .mt(offset)
                        .h(px(SPLIT_HANDLE))
                        .cursor_ns_resize(),
                })
                // An empty preview: the divider follows the pointer directly, so
                // a ghost trailing it would only be a second thing to watch.
                .on_drag(DraggedSplit { split: id }, |_, _, _, cx| {
                    cx.new(|_| gpui::Empty)
                });

            div()
                .flex()
                .map(|container| match axis {
                    Axis::Horizontal => container.flex_row(),
                    Axis::Vertical => container.flex_col(),
                })
                .size_full()
                .min_w_0()
                .min_h_0()
                // Listening here rather than on the handle because the handle
                // moves out from under the pointer as the drag goes on, while
                // this box stays put and is what the new ratio is measured
                // against.
                .on_drag_move::<DraggedSplit>(cx.listener(
                    move |workspace, event: &DragMoveEvent<DraggedSplit>, _window, cx| {
                        workspace.drag_split(id, axis, event, cx);
                    },
                ))
                .child(half(ratio, first))
                .child(half(1. - ratio, second))
                .child(handle)
                .into_any_element()
        }
    }
}

/// Renders the empty state: a pane with no tabs, or the whole body while no
/// connection is open at all.
///
/// Two wordings, and the difference matters. With nothing open the body says
/// there is no connection and points at the way to make one — the menu row and
/// its shortcut. Inside a work area it says the opposite: the connection is
/// live, and the pane is empty because nothing has been opened into it yet. One
/// wording for both states would have a live tab sitting above the words "no
/// connections".
///
/// Text only, and deliberately no button: a button that opens nothing is worse
/// than no button at all.
///
/// It paints no fill of its own. The body behind it already carries the one
/// tinted fill the window permits, and a second one here would compose back to
/// opaque; see [`app_settings::window_tint`].
fn render_placeholder(connected: bool, theme: &Theme) -> AnyElement {
    let (title, hint) = if connected {
        (ts!("empty.connected_title"), ts!("empty.connected_hint"))
    } else {
        (ts!("empty.title"), ts!("empty.hint"))
    };
    div()
        .flex()
        .flex_col()
        .flex_grow()
        .min_w_0()
        .min_h_0()
        .items_center()
        .justify_center()
        .gap(px(8.))
        .child(div().text_size(px(18.)).text_color(theme.text).child(title))
        .child(
            div()
                .text_size(px(13.))
                .text_color(theme.text_muted)
                .child(hint),
        )
        .into_any_element()
}

impl Render for Workspace {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = theme(cx);
        // The one place the interface font size is read: everything below
        // inherits it unless it sets a size of its own, which is what makes the
        // setting — and the settings dialog's live preview of it — visible.
        let ui_font_size = app_settings::effective(cx).ui_font_size;
        self.watch_tab_scroll(cx);
        let toolbar = self.render_toolbar(window, cx);
        let body = self.render_body(cx);
        let status_bar = self.render_status_bar(cx);
        let about = self
            .about
            .read(cx)
            .is_open()
            .then(|| div().absolute().inset_0().child(self.about.clone()));
        let connect = self
            .connect
            .read(cx)
            .is_open()
            .then(|| div().absolute().inset_0().child(self.connect.clone()));
        let settings = self
            .settings
            .read(cx)
            .is_open()
            .then(|| div().absolute().inset_0().child(self.settings.clone()));
        let confirm = self.render_confirm(cx);

        // With client-side decorations the compositor stops drawing the drop
        // shadow along with the frame, so the window has to bring its own: the
        // surface grows a transparent band all round, the content is inset by
        // it, and the shadow is painted into it. The inset call keeps
        // `_GTK_FRAME_EXTENTS` in step so the compositor treats the content
        // edge, not the surface edge, as the window.
        let tiling = client_tiling(window);
        if tiling.is_some() {
            window.set_client_inset(px(SHADOW_BAND));
        } else {
            // Clears the extents a client-side frame may have left behind when
            // the setting switches back to the system title bar on a live
            // window; a no-op under decorations that never set any.
            window.set_client_inset(px(0.));
        }

        // No background fill here on purpose. The three bands below — toolbar,
        // body and status bar — cover the window between them, and each paints
        // its own. A fill at this level would sit *under* the translucent body
        // fill and compose back to opaque, which is the mistake that makes
        // `window.background_opacity` and `background_blur` look as though they
        // did nothing at all.
        let content = div()
            .key_context(KEY_CONTEXT)
            .track_focus(&self.focus_handle)
            .relative()
            .size_full()
            .flex()
            .flex_col()
            .text_color(theme.text)
            .text_size(px(ui_font_size))
            // The tab strip's overlay bar is answered from here rather than
            // from the strip: gpui hands a drag move to every listener of that
            // type wherever it sits, and the root is the one element that is
            // always mounted while a drag of it is in flight.
            .on_drag_move::<DraggedThumb>(cx.listener(
                move |workspace, event: &DragMoveEvent<DraggedThumb>, _window, cx| {
                    workspace.drag_tab_scrollbar(event, cx);
                },
            ))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|workspace, _: &MouseUpEvent, _window, cx| {
                    workspace.release_tab_scrollbar(cx);
                    workspace.release_explorer(cx);
                }),
            )
            .on_mouse_up_out(
                MouseButton::Left,
                cx.listener(|workspace, _: &MouseUpEvent, _window, cx| {
                    workspace.release_tab_scrollbar(cx);
                    workspace.release_explorer(cx);
                }),
            )
            .on_action(cx.listener(Self::new_connection_action))
            .on_action(cx.listener(Self::open_settings_action))
            .on_action(cx.listener(Self::show_about_action))
            .on_action(cx.listener(Self::toggle_explorer_action))
            .on_action(cx.listener(Self::new_query_action))
            .on_action(cx.listener(Self::query_object_action))
            .on_action(cx.listener(Self::close_pane_action))
            .on_action(cx.listener(Self::focus_next_pane_action))
            .on_action(cx.listener(Self::focus_prev_pane_action))
            .on_action(cx.listener(Self::split_right_action))
            .on_action(cx.listener(Self::split_below_action))
            .on_action(cx.listener(Self::dismiss_dialog_action))
            .child(toolbar)
            .child(body)
            .child(status_bar)
            .children(about)
            .children(connect)
            .children(settings)
            .children(confirm);

        let Some(tiling) = tiling else {
            // A server-decorated window: the compositor frames and shadows it,
            // and the content is the whole surface.
            return content.into_any_element();
        };

        div()
            .size_full()
            .relative()
            .bg(gpui::transparent_black())
            .when(!tiling.top, |outer| outer.pt(px(SHADOW_BAND)))
            .when(!tiling.bottom, |outer| outer.pb(px(SHADOW_BAND)))
            .when(!tiling.left, |outer| outer.pl(px(SHADOW_BAND)))
            .when(!tiling.right, |outer| outer.pr(px(SHADOW_BAND)))
            .child(
                content
                    // A hairline where the frame's own outline used to be, per
                    // untiled edge; a tiled edge meets the neighbour flush, the
                    // way the compositor would have drawn it.
                    .border_color(theme.border)
                    .when(!tiling.top, |content| content.border_t_1())
                    .when(!tiling.bottom, |content| content.border_b_1())
                    .when(!tiling.left, |content| content.border_l_1())
                    .when(!tiling.right, |content| content.border_r_1())
                    .when(!tiling.is_tiled(), |content| {
                        content.shadow(vec![gpui::BoxShadow {
                            color: gpui::hsla(0., 0., 0., 0.35),
                            blur_radius: px(SHADOW_BAND / 2.),
                            spread_radius: px(0.),
                            offset: gpui::point(px(0.), px(2.)),
                        }])
                    }),
            )
            // Last on purpose: the window border outranks whatever it crosses,
            // dialogs included, the way a compositor frame would.
            .children(render_resize_edges(tiling))
            .into_any_element()
    }
}

/// Installs both palettes the settings name.
///
/// The chrome theme comes straight from the configured id; the editor theme
/// goes through [`editor_theme_for`], which is where "follow the UI theme" is
/// decided. That decision lives here rather than in the settings dialog because
/// it has to hold whatever changed the inputs — a theme file appearing in the
/// user's directory moves the answer without anybody having opened a dialog.
///
/// An id nothing answers to — a theme file the user has since deleted — falls
/// back to the default rather than failing; see [`ThemeRegistry::resolve`].
fn apply_themes(settings: &AppSettings, cx: &mut App) {
    let ui = ThemeRegistry::resolve(&settings.theme, cx);
    let editor_id = editor_theme_for(
        &settings.editor_theme,
        settings.editor_theme_follows_ui,
        &settings.theme,
        ui.dark,
        &EditorThemeRegistry::all(cx),
    );
    let editor = EditorThemeRegistry::resolve(&editor_id, cx);
    set_theme(ui, cx);
    set_editor_theme(editor, cx);
}

/// The editor theme to install, given the configured one and the chrome theme.
///
/// With the "follows the UI" switch off the configured id is used as it stands.
/// With it on the answer is the first of these that exists:
///
/// 1. the configured theme, when its cast already matches the chrome — a user
///    who picked a dark editor for a dark window keeps the editor they picked;
/// 2. the editor theme sharing the chrome theme's id, which is how the pairs
///    that ship under one name (`one-dark`, `one-light`) stay together;
/// 3. any editor theme of the right cast;
/// 4. the configured id after all, when nothing of the right cast exists.
///
/// Pure and taking the theme list as an argument so that the rule can be tested
/// without an [`App`]; the caller supplies [`EditorThemeRegistry::all`].
fn editor_theme_for(
    configured: &str,
    follows_ui: bool,
    ui_theme_id: &str,
    ui_dark: bool,
    entries: &[EditorThemeEntry],
) -> String {
    if !follows_ui {
        return configured.to_string();
    }

    let matching = |id: &str| {
        entries
            .iter()
            .find(|entry| entry.id.eq_ignore_ascii_case(id) && entry.dark == ui_dark)
    };
    if let Some(entry) = matching(configured).or_else(|| matching(ui_theme_id)) {
        return entry.id.clone();
    }
    entries
        .iter()
        .find(|entry| entry.dark == ui_dark)
        .map(|entry| entry.id.clone())
        .unwrap_or_else(|| configured.to_string())
}

/// Records the window's placement in the settings global.
///
/// Fullscreen is knowingly stored as "not maximized". gpui hands out the
/// restore bounds either way, so the size survives; coming back fullscreen with
/// no title bar and no way to tell why would read as a broken window.
fn record_window_geometry(window: &Window, cx: &mut App) {
    let (bounds, maximized) = match window.window_bounds() {
        WindowBounds::Windowed(bounds) => (bounds, false),
        WindowBounds::Maximized(bounds) => (bounds, true),
        WindowBounds::Fullscreen(bounds) => (bounds, false),
    };
    app_settings::record_window_geometry(WindowGeometry::of(bounds, maximized), cx);
}

/// The placement to open the window at.
///
/// A saved position is used as it stands; without one the saved *size* is
/// centred on the active display, which is what a first run does and what a
/// window that has never been moved deserves.
fn window_bounds(state: &WindowState, cx: &mut App) -> WindowBounds {
    let bounds = match WindowGeometry::saved(state) {
        Some(geometry) => geometry.bounds(),
        None => Bounds::centered(
            None,
            size(px(state.width as f32), px(state.height as f32)),
            cx,
        ),
    };
    if state.maximized {
        WindowBounds::Maximized(bounds)
    } else {
        WindowBounds::Windowed(bounds)
    }
}

/// Whether the toolbar has to stand in for the window's title bar.
///
/// On Windows and macOS the style applied to the window settles it: a
/// transparent title bar leaves no platform caption, so the toolbar is all
/// there is.
#[cfg(any(target_os = "windows", target_os = "macos"))]
fn draws_own_titlebar(style: TitlebarStyle, _window: &Window) -> bool {
    style == TitlebarStyle::Custom
}

/// Whether the toolbar has to stand in for the window's title bar.
///
/// Linux is not the configured style alone. The custom style makes the window
/// ask for client-side decorations, but the ask can be declined — gpui falls
/// back to server decorations when no compositor is running — so what the window
/// actually ended up with is what decides here. Deciding from the style alone
/// would draw a second caption under the compositor's own.
#[cfg(not(any(target_os = "windows", target_os = "macos")))]
fn draws_own_titlebar(style: TitlebarStyle, window: &Window) -> bool {
    style == TitlebarStyle::Custom
        && matches!(
            window.window_decorations(),
            gpui::Decorations::Client { .. }
        )
}

/// Wires the gestures a system title bar answers to onto the custom one.
///
/// Windows needs none of them. The row reports itself as
/// [`WindowControlArea::Drag`], the hit test turns that into `HTCAPTION`, and
/// the window procedure then does the dragging, the aero-snap gestures and the
/// double-click to maximise on its own — before the app is ever told a button
/// went down.
#[cfg(target_os = "windows")]
fn titlebar_gestures(row: Stateful<Div>) -> Stateful<Div> {
    row
}

/// Wires the gestures a system title bar answers to onto the custom one.
///
/// AppKit still drags the window for the strip its own title bar would have
/// covered, so only the double-click is left to answer — and it has to go
/// through [`Window::titlebar_double_click`], which follows whatever the user
/// picked in System Settings (zoom, minimise, or nothing at all).
#[cfg(target_os = "macos")]
fn titlebar_gestures(row: Stateful<Div>) -> Stateful<Div> {
    row.on_click(|event, window, _cx| {
        if event.standard_click() && event.click_count() == 2 {
            window.titlebar_double_click();
        }
    })
}

/// Wires the gestures a system title bar answers to onto the custom one.
///
/// Everything is the app's here: the compositor is told to take over the move,
/// and the window menu and the zoom have to be asked for explicitly. Only
/// meaningful once the window carries client-side decorations, which is why the
/// caller gates them on [`Window::window_decorations`].
///
/// The move starts on the press rather than the click because the compositor
/// takes the pointer with it, so a release would never arrive.
#[cfg(not(any(target_os = "windows", target_os = "macos")))]
fn titlebar_gestures(row: Stateful<Div>) -> Stateful<Div> {
    row.on_click(|event, window, _cx| {
        if event.standard_click() && event.click_count() == 2 {
            window.zoom_window();
        }
    })
    .on_mouse_down(MouseButton::Left, |_, window, _cx| {
        window.start_window_move();
    })
    .on_mouse_down(MouseButton::Right, |event, window, _cx| {
        window.show_window_menu(event.position);
    })
}

/// Width of the transparent band around a self-decorated window.
///
/// The band carries the drop shadow the compositor no longer draws once the
/// window asks for client-side decorations, and doubles as the resize grip. It
/// is part of the window's surface but not of the window as the user
/// understands it: [`Window::set_client_inset`] publishes the visible bounds
/// through `_GTK_FRAME_EXTENTS`, so the compositor snaps, maximises and stacks
/// by the visible edge, exactly as it does for GTK's frames.
const SHADOW_BAND: f32 = 12.;

/// Edge length of the corner squares, where the resize goes diagonal.
const RESIZE_CORNER: f32 = 24.;

/// The tiling state of a window that draws its own frame, `None` under a
/// server-side one.
///
/// Always `None` here: Windows keeps resizing and framing the window through the
/// caption hit test even under a custom title bar, and AppKit never gives the
/// frame up at all — neither window ever carries the shadow band.
#[cfg(any(target_os = "windows", target_os = "macos"))]
fn client_tiling(_window: &Window) -> Option<gpui::Tiling> {
    None
}

/// The tiling state of a window that draws its own frame, `None` under a
/// server-side one.
///
/// `Some` exactly when the compositor granted client-side decorations, with the
/// edges that currently touch a screen or neighbour edge marked tiled — those
/// edges get no band, no shadow and no resize grip. Fullscreen counts as tiled
/// all round.
#[cfg(not(any(target_os = "windows", target_os = "macos")))]
fn client_tiling(window: &Window) -> Option<gpui::Tiling> {
    match window.window_decorations() {
        gpui::Decorations::Client { tiling } => Some(tiling),
        gpui::Decorations::Server => None,
    }
}

/// The resize handles the compositor's frame would have provided.
///
/// Asking for client-side decorations takes the frame away, resize borders
/// included, so the shadow band has to start the resize itself — the compositor
/// takes over once told, exactly as it does for the title-bar drag. The strips
/// cover the band, the corner squares reach past it into the window, and every
/// tiled edge goes without: a maximised or snapped window has no border to drag
/// there.
fn render_resize_edges(tiling: gpui::Tiling) -> Vec<AnyElement> {
    use gpui::{CursorStyle, ResizeEdge};

    let strip = px(SHADOW_BAND);
    let corner = px(RESIZE_CORNER);
    // A strip stops short of a corner square only where that square exists;
    // against a tiled perpendicular edge it runs to the end of the band.
    let inset = |tiled: bool| if tiled { px(0.) } else { corner };
    let handle = |id: &'static str, cursor: CursorStyle, edge: ResizeEdge| {
        div()
            .id(id)
            .occlude()
            .absolute()
            .cursor(cursor)
            .on_mouse_down(MouseButton::Left, move |_, window, _cx| {
                window.start_window_resize(edge);
            })
    };

    let mut handles: Vec<AnyElement> = Vec::new();
    if !tiling.top {
        handles.push(
            handle("resize-top", CursorStyle::ResizeUpDown, ResizeEdge::Top)
                .top_0()
                .left(inset(tiling.left))
                .right(inset(tiling.right))
                .h(strip)
                .into_any_element(),
        );
    }
    if !tiling.bottom {
        handles.push(
            handle(
                "resize-bottom",
                CursorStyle::ResizeUpDown,
                ResizeEdge::Bottom,
            )
            .bottom_0()
            .left(inset(tiling.left))
            .right(inset(tiling.right))
            .h(strip)
            .into_any_element(),
        );
    }
    if !tiling.left {
        handles.push(
            handle(
                "resize-left",
                CursorStyle::ResizeLeftRight,
                ResizeEdge::Left,
            )
            .left_0()
            .top(inset(tiling.top))
            .bottom(inset(tiling.bottom))
            .w(strip)
            .into_any_element(),
        );
    }
    if !tiling.right {
        handles.push(
            handle(
                "resize-right",
                CursorStyle::ResizeLeftRight,
                ResizeEdge::Right,
            )
            .right_0()
            .top(inset(tiling.top))
            .bottom(inset(tiling.bottom))
            .w(strip)
            .into_any_element(),
        );
    }
    if !tiling.top && !tiling.left {
        handles.push(
            handle(
                "resize-top-left",
                CursorStyle::ResizeUpLeftDownRight,
                ResizeEdge::TopLeft,
            )
            .top_0()
            .left_0()
            .size(corner)
            .into_any_element(),
        );
    }
    if !tiling.top && !tiling.right {
        handles.push(
            handle(
                "resize-top-right",
                CursorStyle::ResizeUpRightDownLeft,
                ResizeEdge::TopRight,
            )
            .top_0()
            .right_0()
            .size(corner)
            .into_any_element(),
        );
    }
    if !tiling.bottom && !tiling.left {
        handles.push(
            handle(
                "resize-bottom-left",
                CursorStyle::ResizeUpRightDownLeft,
                ResizeEdge::BottomLeft,
            )
            .bottom_0()
            .left_0()
            .size(corner)
            .into_any_element(),
        );
    }
    if !tiling.bottom && !tiling.right {
        handles.push(
            handle(
                "resize-bottom-right",
                CursorStyle::ResizeUpLeftDownRight,
                ResizeEdge::BottomRight,
            )
            .bottom_0()
            .right_0()
            .size(corner)
            .into_any_element(),
        );
    }
    handles
}

/// Maps the window settings onto a gpui background appearance.
///
/// Blur wins when requested; failing that, any opacity below fully opaque asks
/// for a plain transparent window; otherwise the window stays opaque.
fn window_appearance(window: &WindowState) -> WindowBackgroundAppearance {
    if window.background_blur {
        WindowBackgroundAppearance::Blurred
    } else if window.background_opacity < 1.0 {
        WindowBackgroundAppearance::Transparent
    } else {
        WindowBackgroundAppearance::Opaque
    }
}

/// The application menu bar, in macOS layout.
///
/// gpui only turns this into a real menu bar on macOS — the Windows and Linux
/// backends store it and draw nothing — so the other platforms get the same
/// commands from the in-app dropdown built by [`Workspace::render_app_menu`].
/// Every item dispatches an action that is also bound to a shortcut in
/// [`bind_shortcuts`], which is what lets the macOS backend label the items with
/// their key equivalents; register the bindings first so the keymap it reads is
/// already populated.
///
/// About, Settings and Quit live in the application menu because that is where
/// macOS users look for them.
///
/// The item labels are translated, but the application menu's own name is the
/// "rudbman" wordmark and stays as it is. Rebuilt and re-installed whenever the
/// language changes, because gpui takes the menu bar by value.
fn app_menus() -> Vec<Menu> {
    vec![
        Menu {
            name: APP_NAME.into(),
            items: vec![
                MenuItem::action(ts!("menu.about"), ShowAbout),
                MenuItem::separator(),
                MenuItem::action(ts!("menu.settings"), OpenSettings),
                MenuItem::separator(),
                MenuItem::action(ts!("menu.mac.quit"), Quit),
            ],
        },
        Menu {
            name: ts!("menu.connection"),
            items: vec![
                MenuItem::action(ts!("menu.mac.new_connection"), NewConnection),
                MenuItem::separator(),
                MenuItem::action(ts!("menu.new_query"), NewQuery),
                MenuItem::action(ts!("menu.query_object"), QueryObject),
            ],
        },
        Menu {
            name: ts!("menu.view"),
            items: vec![MenuItem::action(
                ts!("menu.toggle_explorer"),
                ToggleExplorer,
            )],
        },
    ]
}

/// Registers every shortcut the workspace listens for.
///
/// A binding here beats the focused view: gpui matches key bindings along the
/// whole dispatch path before it delivers the key event itself, so every chord
/// bound in this function is taken away from the SQL editor that will one day
/// be inside a pane. That is what decides [`PANE_SHORTCUT_MODIFIER`].
fn bind_shortcuts(cx: &mut App) {
    let modifier = if cfg!(target_os = "macos") {
        "cmd"
    } else {
        "ctrl"
    };
    let pane = PANE_SHORTCUT_MODIFIER;

    cx.bind_keys(vec![
        KeyBinding::new(&format!("{modifier}-q"), Quit, None),
        KeyBinding::new(&format!("{modifier}-n"), NewConnection, Some(KEY_CONTEXT)),
        KeyBinding::new(&format!("{modifier}-,"), OpenSettings, Some(KEY_CONTEXT)),
        // `Ctrl+B` is what every editor with a sidebar binds it to, and unlike
        // the pane chords it has no contender inside a SQL editor.
        KeyBinding::new(&format!("{modifier}-b"), ToggleExplorer, Some(KEY_CONTEXT)),
        // `Ctrl+T` is free: the SQL editor binds no `T` chord, and the shell has
        // no tab-cycling gesture to clash with.
        KeyBinding::new(&format!("{modifier}-t"), NewQuery, Some(KEY_CONTEXT)),
        // Scoped to the sidebar rather than the window, because the same chord
        // is the editor's "run the statement" — see `explorer::KEY_CONTEXT`.
        KeyBinding::new(
            &format!("{modifier}-enter"),
            QueryObject,
            Some(explorer::KEY_CONTEXT),
        ),
        KeyBinding::new("escape", DismissDialog, Some(KEY_CONTEXT)),
        KeyBinding::new(&format!("{pane}-w"), ClosePane, Some(KEY_CONTEXT)),
        KeyBinding::new(&format!("{pane}-]"), FocusNextPane, Some(KEY_CONTEXT)),
        KeyBinding::new(&format!("{pane}-["), FocusPrevPane, Some(KEY_CONTEXT)),
        // Shifted because off macOS the pane modifier is `alt`, and a bare
        // `Alt+D`/`Alt+S` is a menu mnemonic on Windows. The bracket keys above
        // stay unshifted on purpose: macOS and Windows both report a shifted
        // bracket as `}` with the shift flag already consumed, so a `shift-]`
        // binding would never match.
        KeyBinding::new(&format!("{pane}-shift-d"), SplitRight, Some(KEY_CONTEXT)),
        KeyBinding::new(&format!("{pane}-shift-s"), SplitBelow, Some(KEY_CONTEXT)),
    ]);
}

fn main() {
    env_logger::init();

    // The icon set has to be installed before the app runs: `svg()` resolves
    // every path through this source, and the default one answers `None`.
    Application::new().with_assets(Icons).run(|cx: &mut App| {
        if let Err(error) = rudbman_core::init_secrets() {
            log::warn!("the OS keychain is unavailable: {error}");
        }

        // Load the settings before the widget layer installs its default
        // palettes, then override those to match what the user configured.
        app_settings::init(cx);
        let settings = app_settings::current(cx);
        // Ahead of everything that renders a string — the menu bar included —
        // so nothing is ever built in the wrong language and then corrected.
        i18n::apply(settings.language.as_deref());

        rudbman_ui::init(cx);
        // After the widget layer, because both scope their bindings to key
        // contexts the shell's own bindings have to be able to outrank.
        rudbman_editor::init(cx);
        rudbman_grid::init(cx);
        bind_shortcuts(cx);
        cx.set_menus(app_menus());

        // Before the palettes are applied: the ids in the settings may well
        // name themes of the user's own.
        theme_store::reload(cx);
        apply_themes(&settings, cx);

        cx.on_action(|_: &Quit, cx: &mut App| cx.quit());
        // The window's geometry is only in memory until here; this is the one
        // write of `settings.json` the shell performs. Nothing in the closure
        // re-enters gpui — the file write is the whole of it — which is what
        // keeps it clear of the X11 backend's re-entrancy trap, the one the
        // vendored `client.rs` patch exists for.
        cx.on_window_closed(|cx| {
            if cx.windows().is_empty() {
                app_settings::save(cx);
                cx.quit();
            }
        })
        .detach();

        let bounds = window_bounds(&settings.window, cx);
        // Read once, here: `appears_transparent` is what strips the platform
        // caption, and both Windows and macOS decide that when the window is
        // created. Changing the setting later cannot reach an open window,
        // which is why the settings dialog has to say a restart is needed.
        let titlebar = settings.window.titlebar;
        cx.open_window(
            WindowOptions {
                window_bounds: Some(bounds),
                titlebar: Some(TitlebarOptions {
                    title: Some(APP_NAME.into()),
                    appears_transparent: titlebar == TitlebarStyle::Custom,
                    // Ignored unless the caption is transparent; it moves the
                    // traffic lights AppKit keeps drawing into the toolbar band
                    // the app puts in the caption's place.
                    traffic_light_position: (titlebar == TitlebarStyle::Custom)
                        .then_some(TRAFFIC_LIGHT_ORIGIN),
                }),
                // Only the Linux backends read this. `appears_transparent`
                // above means nothing to X11 and Wayland: the caption stays the
                // compositor's until the window asks for client-side
                // decorations outright. gpui falls back to server decorations
                // on its own when no compositor is present, and
                // [`draws_own_titlebar`] follows what the window actually got.
                window_decorations: (titlebar == TitlebarStyle::Custom)
                    .then_some(gpui::WindowDecorations::Client),
                app_id: Some(APP_ID.into()),
                // A translucent or blurred window needs the platform surface to
                // permit alpha; the body then tints its own background.
                window_background: window_appearance(&settings.window),
                ..Default::default()
            },
            |window, cx| {
                let workspace = cx.new(|cx| Workspace::new(titlebar, window, cx));
                window.focus(&workspace.read(cx).focus_handle);
                apply_caption_theme(window, &theme(cx));
                workspace
            },
        )
        .expect("failed to open the rudbman window");

        cx.activate(true);
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A stand-in registry listing: two pairs of casts, plus one dark theme that
    /// no chrome theme shares a name with.
    fn entries() -> Vec<EditorThemeEntry> {
        [
            ("one-dark", true),
            ("one-light", false),
            ("tokyo-night", true),
            ("solarized-light", false),
        ]
        .into_iter()
        .map(|(id, dark)| EditorThemeEntry {
            id: id.to_string(),
            name: id.to_string(),
            dark,
            builtin: true,
        })
        .collect()
    }

    #[test]
    fn a_pinned_editor_theme_is_left_alone() {
        // The switch is off, so nothing about the chrome may reach the editor —
        // not even a cast that clashes with it.
        assert_eq!(
            editor_theme_for("tokyo-night", false, "one-light", false, &entries()),
            "tokyo-night"
        );
    }

    #[test]
    fn following_the_ui_keeps_a_pick_of_the_right_cast() {
        // A user who chose Tokyo Night for a dark window keeps Tokyo Night, and
        // is not dragged onto the chrome theme's namesake.
        assert_eq!(
            editor_theme_for("tokyo-night", true, "one-dark", true, &entries()),
            "tokyo-night"
        );
    }

    #[test]
    fn following_the_ui_prefers_the_chrome_themes_namesake() {
        // A light window with a dark editor pinned: the pair that ships under
        // the chrome theme's own id wins over any other light theme.
        assert_eq!(
            editor_theme_for("tokyo-night", true, "one-light", false, &entries()),
            "one-light"
        );
    }

    #[test]
    fn following_the_ui_falls_back_to_any_theme_of_the_right_cast() {
        // A chrome theme with no editor theme of its name — a palette the user
        // wrote themselves, say — still has to produce a light editor.
        assert_eq!(
            editor_theme_for("one-dark", true, "my-light-theme", false, &entries()),
            "one-light"
        );
    }

    #[test]
    fn following_the_ui_keeps_the_configured_id_when_nothing_matches() {
        // Nothing of the right cast exists, so there is no better answer than
        // the id the settings already carry; resolving it falls back on its own.
        let only_dark = vec![EditorThemeEntry {
            id: "one-dark".to_string(),
            name: "One Dark".to_string(),
            dark: true,
            builtin: true,
        }];
        assert_eq!(
            editor_theme_for("one-dark", true, "one-light", false, &only_dark),
            "one-dark"
        );
        // And an empty registry cannot make one up either.
        assert_eq!(
            editor_theme_for("whatever", true, "one-light", false, &[]),
            "whatever"
        );
    }

    #[test]
    fn ids_are_matched_case_insensitively() {
        // `settings.json` is hand-editable and the registries resolve ids
        // case-insensitively, so this rule has to as well.
        assert_eq!(
            editor_theme_for("One-Dark", true, "irrelevant", true, &entries()),
            "one-dark"
        );
    }

    #[test]
    fn both_empty_states_of_a_pane_are_translated() {
        // `t!` answers with the key path when a key is missing, so a typo
        // reaches the screen as "empty.connected_ttle". The two pairs have to
        // differ, or the connected state would still read "no connections".
        for label in [
            ts!("empty.title"),
            ts!("empty.hint"),
            ts!("empty.connected_title"),
            ts!("empty.connected_hint"),
            ts!("statusbar.no_connection"),
            ts!("statusbar.idle"),
            ts!("statusbar.connecting"),
            ts!("statusbar.connected"),
            ts!("statusbar.disconnected"),
            ts!("statusbar.failed", error = "e"),
            ts!("statusbar.tunnel_lost", reason = "r"),
        ] {
            assert!(!label.is_empty(), "empty label");
            assert!(!label.starts_with("empty."), "untranslated label {label:?}");
            assert!(
                !label.starts_with("statusbar."),
                "untranslated label {label:?}"
            );
        }
        assert_ne!(ts!("empty.title"), ts!("empty.connected_title"));
        assert_ne!(ts!("empty.hint"), ts!("empty.connected_hint"));
    }

    /// The end of the M1 thread, in one test: a real H2 session opens, a tab
    /// appears carrying the profile's name and a "connected" dot, the status bar
    /// names the product and version the driver reported, and closing the tab
    /// takes the session with it.
    ///
    /// The session is opened before the window is built so that the blocking
    /// call is nowhere near a gpui update — which is also the rule the shell
    /// itself follows, by way of `background_spawn`.
    #[gpui::test]
    fn a_real_connection_reaches_the_tab_strip_and_the_status_bar(cx: &mut gpui::TestAppContext) {
        let profile = connection::h2::profile("workspace");
        let connected = connection::connect(
            &profile,
            &connection::h2::driver(),
            &connection::Credentials::typed(Some(String::new()), None),
            &AppSettings::default(),
        )
        .expect("H2 opens an in-memory database without a server");
        let product = connected.product().expect("H2 names itself");

        cx.update(|cx| {
            app_settings::init(cx);
            rudbman_ui::init(cx);
        });
        let window = cx.add_window(|window, cx| Workspace::new(TitlebarStyle::Custom, window, cx));

        window
            .update(cx, |workspace, window, cx| {
                // Nothing open yet: both cells say so.
                let (name, state) = workspace.status_cells();
                assert_eq!(name, ts!("statusbar.no_connection"));
                assert_eq!(state, ts!("statusbar.idle"));

                workspace.connections.push(Connection {
                    id: next_connection_id(),
                    profile: profile.clone(),
                    state: ConnectionState::Open(Box::new(connected)),
                    work: WorkArea::new(),
                });
                workspace.active_connection = 0;

                let connection = workspace.active_connection().expect("one tab");
                assert_eq!(connection.profile.name, "workspace");
                assert_eq!(connection.state.tab_status(), TabStatus::Connected);

                let (name, state) = workspace.status_cells();
                assert_eq!(name, SharedString::from(product.clone()));
                assert!(name.starts_with("H2 "), "{name}");
                assert_eq!(state, ts!("statusbar.connected"));

                // Closing the tab hands the session to a background task that
                // closes it; the tab is gone either way.
                workspace.close_connection(0, window, cx);
                assert!(workspace.connections.is_empty());
                let (name, state) = workspace.status_cells();
                assert_eq!(name, ts!("statusbar.no_connection"));
                assert_eq!(state, ts!("statusbar.idle"));
            })
            .expect("the window is open");
        cx.run_until_parked();
    }

    /// A failed attempt lands in the tab rather than in a log nobody reads.
    #[gpui::test]
    fn a_refused_connection_shows_the_drivers_own_message(cx: &mut gpui::TestAppContext) {
        let mut profile = connection::h2::profile("refused");
        profile.url = format!("{};DB_CLOSE_DELAY=-1", profile.url);
        let created = connection::connect(
            &profile,
            &connection::h2::driver(),
            &connection::Credentials::typed(Some("hunter2".into()), None),
            &AppSettings::default(),
        )
        .expect("the first connection creates the database");

        let error = connection::connect(
            &profile,
            &connection::h2::driver(),
            &connection::Credentials::typed(Some("s3cr3t-pa55w0rd".into()), None),
            &AppSettings::default(),
        )
        .expect_err("a wrong password must be refused");
        assert!(error.is_authentication(), "{error}");

        cx.update(|cx| {
            app_settings::init(cx);
            rudbman_ui::init(cx);
        });
        let window = cx.add_window(|window, cx| Workspace::new(TitlebarStyle::Custom, window, cx));

        window
            .update(cx, |workspace, _window, _cx| {
                workspace.connections.push(Connection {
                    id: next_connection_id(),
                    profile,
                    state: ConnectionState::Failed(error.message().into()),
                    work: WorkArea::new(),
                });
                workspace.active_connection = 0;

                assert_eq!(
                    workspace
                        .active_connection()
                        .expect("one tab")
                        .state
                        .tab_status(),
                    TabStatus::Error
                );
                let (name, state) = workspace.status_cells();
                // The profile's name, since there is no product to report.
                assert_eq!(name, "refused");
                // The driver's own words, and no password in them.
                assert!(state.len() > ts!("statusbar.connected").len(), "{state}");
                assert!(
                    !state.contains("s3cr3t-pa55w0rd") && !state.contains("hunter2"),
                    "the refused password reached the status bar: {state}"
                );
            })
            .expect("the window is open");

        created.close().expect("close");
        cx.run_until_parked();
    }

    /// Hiding the sidebar has to bring the keyboard back with it.
    ///
    /// The explorer's tree focuses itself when a row is clicked, and gpui never
    /// clears the window focus when the focused element stops being rendered:
    /// it resolves a dispatched action against the focused element of the last
    /// drawn frame and falls back to the window root, which carries none of the
    /// workspace's handlers, when that element has gone. So without
    /// [`Workspace::reclaim_focus`] the first `ToggleExplorer` hides the panel
    /// and every action after it — the menu rows and the shortcuts alike — is
    /// dropped without a trace. The second dispatch below is that bug.
    #[gpui::test]
    fn hiding_the_explorer_takes_the_focus_back(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| {
            app_settings::init(cx);
            rudbman_ui::init(cx);
        });
        let window = cx.add_window(|window, cx| Workspace::new(TitlebarStyle::Custom, window, cx));
        // The setting on disk decides how the sidebar starts, and this is about
        // hiding one that is showing.
        window
            .update(cx, |workspace, _window, cx| {
                workspace.explorer_visible = true;
                cx.notify();
            })
            .expect("the window is open");

        let mut cx = gpui::VisualTestContext::from_window(window.into(), cx);
        cx.run_until_parked();

        // What clicking a row in the tree amounts to, without the mouse.
        window
            .update(&mut cx, |workspace, window, cx| {
                workspace.explorer.read(cx).focus_handle(cx).focus(window);
            })
            .expect("the window is open");
        cx.run_until_parked();

        // Through `Window::dispatch_action`, which is the path both the menu row
        // and the keyboard shortcut take.
        cx.dispatch_action(ToggleExplorer);
        window
            .update(&mut cx, |workspace, window, _cx| {
                assert!(!workspace.explorer_visible, "the sidebar is still showing");
                assert!(
                    workspace.focus_handle.is_focused(window),
                    "the focus is still on the hidden sidebar"
                );
            })
            .expect("the window is open");

        // The regression: with the focus stranded, this one never arrives.
        cx.dispatch_action(ToggleExplorer);
        window
            .update(&mut cx, |workspace, _window, _cx| {
                assert!(
                    workspace.explorer_visible,
                    "the second toggle was dropped: the sidebar cannot be brought back"
                );
            })
            .expect("the window is open");
    }

    /// The same hazard one pane down: closing a pane whose editor has the
    /// keyboard must not leave the focus on an editor nothing renders.
    #[gpui::test]
    fn closing_a_focused_pane_takes_the_focus_back(cx: &mut gpui::TestAppContext) {
        let profile = connection::h2::profile("panes");
        let connected = connection::connect(
            &profile,
            &connection::h2::driver(),
            &connection::Credentials::typed(Some(String::new()), None),
            &AppSettings::default(),
        )
        .expect("H2 opens an in-memory database without a server");

        cx.update(|cx| {
            app_settings::init(cx);
            rudbman_ui::init(cx);
            rudbman_editor::init(cx);
        });
        let window = cx.add_window(|window, cx| Workspace::new(TitlebarStyle::Custom, window, cx));
        window
            .update(cx, |workspace, window, cx| {
                workspace.connections.push(Connection {
                    id: next_connection_id(),
                    profile: profile.clone(),
                    state: ConnectionState::Open(Box::new(connected)),
                    work: WorkArea::new(),
                });
                workspace.active_connection = 0;
                // Two panes, because the last one is never closed.
                workspace.split_active(Axis::Horizontal, cx);
                // Opens in the new pane and puts the caret in its editor.
                workspace.open_query("SELECT 1", window, cx);
            })
            .expect("the window is open");

        let mut cx = gpui::VisualTestContext::from_window(window.into(), cx);
        cx.run_until_parked();

        window
            .update(&mut cx, |workspace, window, cx| {
                let editor = workspace
                    .active_query()
                    .expect("the new pane holds the editor")
                    .read(cx)
                    .focus_handle(cx);
                assert!(editor.is_focused(window), "the editor did not take focus");
                workspace.close_active_pane(window, cx);
                assert!(
                    workspace.focus_handle.is_focused(window),
                    "the focus stayed on the editor of the closed pane"
                );
            })
            .expect("the window is open");
        cx.run_until_parked();
    }

    /// A live H2 session and the profile it was opened from.
    ///
    /// Opened outside every gpui update on purpose, as each test here does: the
    /// call blocks, and the rule the shell itself follows is that nothing
    /// blocking runs inside an update.
    fn h2_connection(name: &str) -> (ConnectionProfile, Connected) {
        let profile = connection::h2::profile(name);
        let connected = connection::connect(
            &profile,
            &connection::h2::driver(),
            &connection::Credentials::typed(Some(String::new()), None),
            &AppSettings::default(),
        )
        .expect("H2 opens an in-memory database without a server");
        (profile, connected)
    }

    /// Pushes a live connection tab, brings it to the front, and puts the
    /// explorer in step with it — everything [`Workspace::open_connection`] does
    /// once the handshake is in, minus the handshake.
    fn push_connection(
        workspace: &mut Workspace,
        profile: ConnectionProfile,
        connected: Connected,
        window: &mut Window,
        cx: &mut Context<Workspace>,
    ) -> ConnectionId {
        let id = next_connection_id();
        let index = workspace.connections.len();
        workspace.connections.push(Connection {
            id,
            profile,
            state: ConnectionState::Open(Box::new(connected)),
            work: WorkArea::new(),
        });
        workspace.select_connection(index, window, cx);
        workspace.sync_explorer_root(index, cx);
        workspace.sync_visible_root(cx);
        id
    }

    /// A window with one live H2 connection in its tab strip, and the id that
    /// connection was given.
    fn workspace_over_h2(
        name: &str,
        cx: &mut gpui::TestAppContext,
    ) -> (gpui::WindowHandle<Workspace>, ConnectionId) {
        let (profile, connected) = h2_connection(name);

        cx.update(|cx| {
            app_settings::init(cx);
            rudbman_ui::init(cx);
            rudbman_editor::init(cx);
            rudbman_grid::init(cx);
        });
        let window = cx.add_window(|window, cx| Workspace::new(TitlebarStyle::Custom, window, cx));
        let id = window
            .update(cx, |workspace, window, cx| {
                push_connection(workspace, profile, connected, window, cx)
            })
            .expect("the window is open");
        (window, id)
    }

    /// The work area on screen, which every assertion about panes goes through.
    fn area(workspace: &Workspace) -> &WorkArea {
        workspace.work_area().expect("a connection tab is open")
    }

    /// An object of `connection` in the public schema.
    fn object(connection: ConnectionId, name: &str) -> ObjectTarget {
        ObjectTarget {
            connection,
            catalog: None,
            schema: Some("PUBLIC".to_string()),
            folder: explorer::Folder::Tables,
            name: name.to_string(),
        }
    }

    /// The titles of one pane's tabs, in strip order.
    fn tab_titles(workspace: &Workspace, pane: PaneId, cx: &App) -> Vec<String> {
        area(workspace)
            .panes
            .get(pane)
            .expect("the pane is in the tree")
            .items()
            .iter()
            .map(|item| item.title(cx).to_string())
            .collect()
    }

    /// Which tab of one pane is on top.
    fn active_tab(workspace: &Workspace, pane: PaneId) -> usize {
        area(workspace)
            .panes
            .get(pane)
            .expect("the pane is in the tree")
            .active_index()
    }

    /// The active pane of the work area on screen.
    fn active_pane(workspace: &Workspace) -> PaneId {
        workspace.active_pane().expect("a connection tab is open")
    }

    /// Opening things does not throw away what the pane already held, and
    /// opening the *same* object twice does not open it twice.
    #[gpui::test]
    fn a_detail_and_a_query_are_two_tabs_of_one_pane(cx: &mut gpui::TestAppContext) {
        let (window, id) = workspace_over_h2("tabs", cx);
        let target = object(id, "ORDERS");
        let mut cx = gpui::VisualTestContext::from_window(window.into(), cx);

        let first = window
            .update(&mut cx, |workspace, window, cx| {
                workspace.open_object(target.clone(), window, cx);
                workspace.open_query("SELECT 1", window, cx);

                let pane = active_pane(workspace);
                // Both are open, in the order they were opened, and the query —
                // the one just asked for — is the one showing.
                assert_eq!(
                    tab_titles(workspace, pane, cx),
                    ["PUBLIC.ORDERS", "Query 1"]
                );
                assert_eq!(active_tab(workspace, pane), 1);
                assert!(workspace.active_query().is_some());

                // The same object again is a navigation, not a second copy.
                workspace.open_object(target.clone(), window, cx);
                assert_eq!(
                    tab_titles(workspace, pane, cx),
                    ["PUBLIC.ORDERS", "Query 1"]
                );
                assert_eq!(active_tab(workspace, pane), 0);
                assert!(
                    workspace.active_query().is_none(),
                    "the detail tab is the one showing, so no query pane is active"
                );
                pane
            })
            .expect("the window is open");
        cx.run_until_parked();

        window
            .update(&mut cx, |workspace, window, cx| {
                workspace.split_active(Axis::Horizontal, cx);
                let second = active_pane(workspace);
                assert_ne!(second, first, "the split produced a pane of its own");
                workspace.open_query("SELECT 2", window, cx);
                assert_eq!(tab_titles(workspace, second, cx), ["Query 2"]);

                // Asking for the object from the other pane jumps to the pane
                // already showing it rather than opening a copy beside it.
                workspace.open_object(target.clone(), window, cx);
                assert_eq!(active_pane(workspace), first);
                assert_eq!(active_tab(workspace, first), 0);
                assert_eq!(
                    tab_titles(workspace, first, cx),
                    ["PUBLIC.ORDERS", "Query 1"]
                );
                assert_eq!(tab_titles(workspace, second, cx), ["Query 2"]);
            })
            .expect("the window is open");
        cx.run_until_parked();
    }

    /// Closing a tab is the same focus hazard as closing a pane: the view in it
    /// stops being rendered, and gpui resolves actions against the focused
    /// element of the last drawn frame. The keyboard has to land on the
    /// neighbour that took its place — or on the shell, when nothing did.
    #[gpui::test]
    fn closing_a_tab_hands_the_keyboard_to_the_tab_beside_it(cx: &mut gpui::TestAppContext) {
        let (window, _id) = workspace_over_h2("tab-focus", cx);
        let mut cx = gpui::VisualTestContext::from_window(window.into(), cx);

        let pane = window
            .update(&mut cx, |workspace, window, cx| {
                workspace.open_query("SELECT 1", window, cx);
                workspace.open_query("SELECT 2", window, cx);
                active_pane(workspace)
            })
            .expect("the window is open");
        cx.run_until_parked();

        window
            .update(&mut cx, |workspace, window, cx| {
                let editor = workspace
                    .active_query()
                    .expect("the second query is showing")
                    .read(cx)
                    .focus_handle(cx);
                assert!(editor.is_focused(window), "the editor did not take focus");

                workspace.close_tab(pane, 1, window, cx);
                assert_eq!(tab_titles(workspace, pane, cx), ["Query 1"]);
                assert_eq!(active_tab(workspace, pane), 0);
                let neighbour = workspace
                    .active_query()
                    .expect("the first query took its place")
                    .read(cx)
                    .focus_handle(cx);
                assert!(
                    neighbour.is_focused(window),
                    "the focus stayed on the editor of the closed tab"
                );
            })
            .expect("the window is open");
        cx.run_until_parked();

        window
            .update(&mut cx, |workspace, window, cx| {
                workspace.close_tab(pane, 0, window, cx);
                assert!(
                    area(workspace)
                        .panes
                        .get(pane)
                        .expect("the pane outlives its tabs")
                        .is_empty(),
                    "closing the last tab must leave the pane standing"
                );
                assert!(
                    workspace.focus_handle.is_focused(window),
                    "an empty pane has nothing to type into, so the shell takes over"
                );
            })
            .expect("the window is open");

        // The regression the rule exists for: with the focus stranded on an
        // editor nothing renders, this dispatch would never arrive.
        let showing = window
            .update(&mut cx, |workspace, _window, _cx| {
                workspace.explorer_visible
            })
            .expect("the window is open");
        cx.dispatch_action(ToggleExplorer);
        window
            .update(&mut cx, |workspace, _window, _cx| {
                assert_ne!(
                    workspace.explorer_visible, showing,
                    "the action was dropped: the focus is on something unrendered"
                );
            })
            .expect("the window is open");
    }

    /// Closing a connection tab takes its whole work area with it.
    ///
    /// The tabs belonged to that connection and nothing else, so keeping them
    /// would leave the window carrying panels of a database nobody can ask —
    /// and, worse, editors still holding a session handle each. A write
    /// confirmation waiting on one of those panes has nobody left to answer it
    /// and goes too.
    #[gpui::test]
    fn closing_a_connection_discards_its_work_area(cx: &mut gpui::TestAppContext) {
        let (window, id) = workspace_over_h2("abandon", cx);
        let mut cx = gpui::VisualTestContext::from_window(window.into(), cx);

        window
            .update(&mut cx, |workspace, window, cx| {
                workspace.open_object(object(id, "ORDERS"), window, cx);
                workspace.open_query("SELECT 1", window, cx);
                let pane = active_pane(workspace);
                assert_eq!(
                    tab_titles(workspace, pane, cx),
                    ["PUBLIC.ORDERS", "Query 1"]
                );
                // As if the editor had asked before running a `DELETE`.
                let query = workspace
                    .active_query()
                    .expect("the query is the tab on top")
                    .clone();
                workspace.confirm = Some(PendingConfirm {
                    pane: query,
                    request: Box::new(ConfirmRequest {
                        count: 1,
                        preview: "DELETE FROM PUBLIC.ORDERS".into(),
                    }),
                });
            })
            .expect("the window is open");
        cx.run_until_parked();

        window
            .update(&mut cx, |workspace, window, cx| {
                workspace.close_connection(0, window, cx);

                assert!(workspace.connections.is_empty());
                // No connection, so no work area at all: the body renders the
                // empty state and every pane command is a no-op.
                assert!(workspace.work_area().is_none());
                assert!(workspace.active_pane().is_none());
                assert!(workspace.active_query().is_none());
                assert!(
                    workspace.confirm.is_none(),
                    "the confirmation outlived the pane that asked"
                );
                assert!(
                    workspace.explorer.read(cx).visible_roots(cx).is_empty(),
                    "the sidebar is still showing a closed connection"
                );

                // And the pane commands, which have no tree to act on.
                workspace.split_active(Axis::Horizontal, cx);
                workspace.close_active_pane(window, cx);
                workspace.cycle_pane(true, cx);
                assert!(workspace.work_area().is_none());
            })
            .expect("the window is open");
        cx.run_until_parked();
    }

    /// A connection that dies under the user keeps its tabs and lets go of the
    /// session: what the database said stays readable, what would ask it again
    /// refuses.
    #[gpui::test]
    fn a_dead_connection_detaches_its_query_panes(cx: &mut gpui::TestAppContext) {
        let (window, id) = workspace_over_h2("tunnel-death", cx);
        let mut cx = gpui::VisualTestContext::from_window(window.into(), cx);

        let pane = window
            .update(&mut cx, |workspace, window, cx| {
                workspace.open_object(object(id, "ORDERS"), window, cx);
                workspace.open_query("SELECT 1", window, cx);
                active_pane(workspace)
            })
            .expect("the window is open");
        cx.run_until_parked();

        window
            .update(&mut cx, |workspace, _window, cx| {
                // What the tunnel watcher does when the SSH channel closes.
                workspace.tunnel_died(0, "the channel closed".to_string(), cx);

                // Both tabs stay: the layout is the user's, and a session dying
                // must not rearrange their window.
                assert_eq!(
                    tab_titles(workspace, pane, cx),
                    ["PUBLIC.ORDERS", "Query 1"]
                );
                let query = workspace
                    .active_query()
                    .expect("the query tab survived the death")
                    .read(cx);
                assert!(!query.is_attached(), "the session was not let go of");
                assert_eq!(query.status_cells().0, ts!("statusbar.disconnected"));

                // The tab and its area are still reachable, and the strip says
                // what happened.
                let connection = workspace
                    .active_connection()
                    .expect("the tab is still open");
                assert!(matches!(connection.state, ConnectionState::Dead(_)));
                assert_eq!(connection.state.tab_status(), TabStatus::Error);
            })
            .expect("the window is open");
        cx.run_until_parked();
    }

    /// The whole point of the design: the connection tab selects the window.
    ///
    /// Tabs opened under one connection are not visible under another, the
    /// explorer's root follows the same switch, and coming back finds the area
    /// exactly as it was left — same tabs, same one on top.
    #[gpui::test]
    fn switching_the_connection_tab_switches_the_work_area(cx: &mut gpui::TestAppContext) {
        let (window, first) = workspace_over_h2("switch-a", cx);
        let (profile, connected) = h2_connection("switch-b");
        let mut cx = gpui::VisualTestContext::from_window(window.into(), cx);

        let pane_a = window
            .update(&mut cx, |workspace, window, cx| {
                workspace.open_object(object(first, "ORDERS"), window, cx);
                workspace.open_query("SELECT 1", window, cx);
                let pane = active_pane(workspace);
                assert_eq!(
                    tab_titles(workspace, pane, cx),
                    ["PUBLIC.ORDERS", "Query 1"]
                );
                assert_eq!(active_tab(workspace, pane), 1);
                assert_eq!(workspace.explorer.read(cx).visible_roots(cx), [first]);
                pane
            })
            .expect("the window is open");
        cx.run_until_parked();

        let second = window
            .update(&mut cx, |workspace, window, cx| {
                let second = push_connection(workspace, profile, connected, window, cx);

                // A work area of its own: nothing of the first connection is in
                // it, and its numbering starts again at one.
                let pane = active_pane(workspace);
                assert_ne!(pane, pane_a, "the second tab brought its own pane tree");
                assert!(tab_titles(workspace, pane, cx).is_empty());
                workspace.open_query("SELECT 2", window, cx);
                assert_eq!(
                    tab_titles(workspace, pane, cx),
                    ["Query 1"],
                    "query numbering is per connection"
                );
                assert_eq!(workspace.explorer.read(cx).visible_roots(cx), [second]);
                second
            })
            .expect("the window is open");
        cx.run_until_parked();

        window
            .update(&mut cx, |workspace, window, cx| {
                workspace.select_connection(0, window, cx);

                // Back where the first connection was left, tab on top and all.
                assert_eq!(active_pane(workspace), pane_a);
                assert_eq!(
                    tab_titles(workspace, pane_a, cx),
                    ["PUBLIC.ORDERS", "Query 1"]
                );
                assert_eq!(active_tab(workspace, pane_a), 1);
                assert_eq!(workspace.explorer.read(cx).visible_roots(cx), [first]);
                assert_ne!(first, second);
            })
            .expect("the window is open");
        cx.run_until_parked();
    }

    /// Switching the connection tab takes the outgoing area off screen, editors
    /// and all, which is the hazard [`Workspace::reclaim_focus`] describes one
    /// level up. The keyboard has to land on something the next frame renders.
    #[gpui::test]
    fn switching_the_connection_tab_does_not_strand_the_keyboard(cx: &mut gpui::TestAppContext) {
        let (window, _first) = workspace_over_h2("focus-a", cx);
        let (profile, connected) = h2_connection("focus-b");
        let mut cx = gpui::VisualTestContext::from_window(window.into(), cx);

        window
            .update(&mut cx, |workspace, window, cx| {
                workspace.open_query("SELECT 1", window, cx);
            })
            .expect("the window is open");
        cx.run_until_parked();

        let editor = window
            .update(&mut cx, |workspace, window, cx| {
                let editor = workspace
                    .active_query()
                    .expect("the editor is the tab on top")
                    .read(cx)
                    .focus_handle(cx);
                assert!(editor.is_focused(window), "the editor did not take focus");

                push_connection(workspace, profile, connected, window, cx);
                assert!(
                    !editor.is_focused(window),
                    "the keyboard stayed on an editor the new tab does not render"
                );
                assert!(
                    workspace.focus_handle.is_focused(window),
                    "the incoming area holds nothing typeable, so the shell takes over"
                );
                editor
            })
            .expect("the window is open");
        cx.run_until_parked();

        // The other direction, where the incoming area *does* have something to
        // type into: an editor going off screen hands the caret to the editor
        // coming on, so a user switching between two connections keeps typing.
        window
            .update(&mut cx, |workspace, window, cx| {
                workspace.open_query("SELECT 2", window, cx);
                let second = workspace
                    .active_query()
                    .expect("the new tab is the editor")
                    .read(cx)
                    .focus_handle(cx);
                assert!(second.is_focused(window), "the editor did not take focus");

                workspace.select_connection(0, window, cx);
                assert!(
                    !second.is_focused(window),
                    "the keyboard stayed on the editor of the tab switched away from"
                );
                assert!(
                    editor.is_focused(window),
                    "the editor coming back on screen did not take the caret"
                );
            })
            .expect("the window is open");
        cx.run_until_parked();

        // The regression the rule exists for: with the focus stranded, this
        // dispatch would never arrive.
        let showing = window
            .update(&mut cx, |workspace, _window, _cx| {
                workspace.explorer_visible
            })
            .expect("the window is open");
        cx.dispatch_action(ToggleExplorer);
        window
            .update(&mut cx, |workspace, _window, _cx| {
                assert_ne!(
                    workspace.explorer_visible, showing,
                    "the action was dropped: the focus is on something unrendered"
                );
            })
            .expect("the window is open");
    }

    /// A split is part of a work area, so it belongs to one connection: the tab
    /// beside it keeps the single pane it started with.
    #[gpui::test]
    fn the_split_layout_is_per_connection(cx: &mut gpui::TestAppContext) {
        let (window, _first) = workspace_over_h2("split-a", cx);
        let (profile, connected) = h2_connection("split-b");
        let mut cx = gpui::VisualTestContext::from_window(window.into(), cx);

        let (left, right) = window
            .update(&mut cx, |workspace, window, cx| {
                let left = active_pane(workspace);
                workspace.split_active(Axis::Horizontal, cx);
                let right = active_pane(workspace);
                // A marker in the new pane, so that coming back can be told from
                // a layout that merely happens to have two panes.
                workspace.open_query("SELECT 1", window, cx);
                assert_eq!(area(workspace).panes.leaf_count(), 2);
                (left, right)
            })
            .expect("the window is open");
        cx.run_until_parked();

        window
            .update(&mut cx, |workspace, window, cx| {
                push_connection(workspace, profile, connected, window, cx);
                assert_eq!(
                    area(workspace).panes.leaf_count(),
                    1,
                    "the new connection inherited a split it never asked for"
                );

                workspace.select_connection(0, window, cx);
                let area = area(workspace);
                assert_eq!(area.panes.leaf_count(), 2);
                assert_eq!(area.panes.leaf_ids(), vec![left, right]);
                assert_eq!(area.active(), right, "the marker moved while away");
                assert_eq!(tab_titles(workspace, right, cx), ["Query 1"]);
            })
            .expect("the window is open");
        cx.run_until_parked();
    }

    #[test]
    fn a_maximized_window_is_restored_maximized() {
        // The bounds a maximized window carries are its *restore* size, so both
        // halves have to survive: the state, and the size to un-maximize to.
        let state = WindowState {
            x: Some(10),
            y: Some(20),
            width: 1280,
            height: 720,
            maximized: true,
            ..WindowState::default()
        };
        let geometry = WindowGeometry::saved(&state).expect("the position is set");
        assert_eq!(geometry.bounds().size.width, px(1280.));
        assert_eq!(geometry.bounds().origin.x, px(10.));
        assert!(state.maximized);
    }

    #[test]
    fn the_window_appearance_follows_the_settings() {
        let opaque = WindowState::default();
        assert_eq!(
            window_appearance(&opaque),
            WindowBackgroundAppearance::Opaque
        );

        let translucent = WindowState {
            background_opacity: 0.8,
            ..WindowState::default()
        };
        assert_eq!(
            window_appearance(&translucent),
            WindowBackgroundAppearance::Transparent
        );

        // Blur wins even at full opacity: it is the stronger request, and a
        // blurred surface has to permit alpha whatever the fill does.
        let blurred = WindowState {
            background_blur: true,
            ..WindowState::default()
        };
        assert_eq!(
            window_appearance(&blurred),
            WindowBackgroundAppearance::Blurred
        );
    }
}
