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

pub(crate) use bootstrap::{editor_theme_for, theme_dirs};

use crate::{
    AddToBuilder, BackupSchema, CheckUpdates, ClosePane, DismissDialog, ExtractScript,
    FocusNextPane, FocusPrevPane, NewBuilder, NewConnection, NewQuery, OpenErd, OpenSettings,
    OpenSqlFile, QueryObject, Quit, ShowAbout, SplitBelow, SplitRight, ToggleExplorer,
    TransferTable, app_identity, app_settings, builder_pane, builder_sql, connection, context_menu,
    erd_pane, explorer, i18n, icons, table_detail,
};

mod actions;
mod bootstrap;
use bootstrap::{app_menus, apply_themes, chrome_titlebar, record_window_geometry};
mod connections;
mod dialogs;
mod menus;
mod objects;
mod panes;
mod view;
#[cfg(test)]
use view::centered_scroll;

use std::collections::HashMap;
use std::path::PathBuf;

use gpui::{
    AnyElement, App, Context, Div, DragMoveEvent, Entity, FocusHandle, Focusable, Hsla, KeyBinding,
    Menu, MenuItem, MouseButton, MouseUpEvent, Pixels, Point, QuitMode, ScrollHandle, SharedString,
    Subscription, Task, TitlebarOptions, Window, WindowBounds, WindowControlArea, WindowOptions,
    div, img, prelude::*, px,
};
use rudbman_core::{
    AppSettings, ConnectionProfile, ConnectionStore, DriverStore, TitlebarStyle, WindowState,
};
use rugpui::{
    Button, ButtonVariant, DraggedThumb, EditorThemeEntry, EditorThemeRegistry, MenuButton,
    MenuEntry, ResizeHandle, Scrollbar, ScrollbarAxis, ScrollbarState, Splitter, TabBar, TabItem,
    TabStatus, Theme, ThemeRegistry, hide_later, hide_now, modal, scroll_to, scrolled,
    set_editor_theme, set_theme, set_window_tint, theme, theme_store,
};
use rugpui_shell::chrome::{
    SHADOW_BAND, client_tiling, draws_own_titlebar, render_client_frame, titlebar_gestures,
    window_appearance, window_control_strips,
};
pub(crate) use rugpui_shell::menu_rows::SHORTCUT_MODIFIER;
use rugpui_shell::settings::{window_bounds, window_geometry};
use rugpui_shell::update;
use rugpui_shell::{AboutDialog, AboutDialogEvent, UpdateDialog, UpdateDialogEvent};
use rugpui_shell::{apply_caption_theme, window_control_icons};
use uuid::Uuid;

use crate::backup_dialog::{BackupDialog, BackupDialogEvent};
use crate::builder_pane::{BuilderPane, BuilderPaneEvent};
use crate::connection::{ConnectError, Connected};
use crate::connection_dialog::{ConnectionDialog, ConnectionDialogEvent, profile_rows};
use crate::context_menu::MenuRow;
use crate::data_pane::DataPane;
use crate::erd_layout::ErdLayouts;
use crate::erd_pane::{ErdDiagram, ErdPane, ErdPaneEvent, ErdTarget};
use crate::explorer::{
    ConnectionId, Explorer, ExplorerEvent, Folder, NodeId, ObjectTarget, RootInfo,
};
use crate::extract_dialog::{ExtractDialog, ExtractDialogEvent};
use crate::i18n::ts;
use crate::pane_tree::{Axis, Pane, PaneId, PaneItem, PaneLookup, PaneNode, PaneTree, SplitId};
use crate::query::{ConfirmRequest, QueryPane, QueryPaneEvent};
use crate::settings_dialog::{SettingsDialog, SettingsDialogEvent};
use crate::struct_pane::StructPane;
use crate::table_detail::{TableDetail, TableDetailEvent};
use crate::transfer_dialog::{TransferDialog, TransferDialogEvent, TransferTarget};

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

/// [`PANE_SHORTCUT_MODIFIER`] as a menu hint writes it.
///
/// The same key, capitalised: gpui's binding syntax is lower case and the name
/// printed on the key is not. Never translated, for [`SHORTCUT_MODIFIER`]'s
/// reason.
const PANE_SHORTCUT_LABEL: &str = if cfg!(target_os = "macos") {
    "Cmd"
} else {
    "Alt"
};

/// Thickness of the invisible grab area over the explorer's right edge, in
/// pixels.
///
/// The seam itself is the sidebar's own border, a hairline — far too thin to
/// hit with a pointer. The handle is laid over it rather than beside it, inside
/// the sidebar's own trailing edge, so that widening the grab area moves
/// nothing: it covers the border instead of pushing the work area across. The
/// same six pixels [`rugpui::Splitter`] gives the dividers inside the pane tree
/// — and, since both are the same [`rugpui::ResizeHandle`], the same accent bar
/// under the pointer too, so the two resize gestures of one window are equally
/// easy to find and equally easy to recognise.
const SPLIT_HANDLE: f32 = 6.;

/// A surface of the workspace that scrolls, and so wears an overlay bar.
///
/// Two of them, on different axes and never on screen together in the way that
/// matters: the tab strip runs sideways once the tabs outgrow it, the welcome
/// screen runs down once its column outgrows the window. Naming them lets one
/// set of handlers answer for both instead of one set each — the shape logman
/// uses for the same pair of surfaces.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Surface {
    /// The tab strip.
    Tabs,
    /// The welcome screen shown while no tab is open.
    Welcome,
}

impl Surface {
    /// Which way the surface scrolls, and so which way its bar lies.
    fn axis(self) -> ScrollbarAxis {
        match self {
            Self::Tabs => ScrollbarAxis::Horizontal,
            Self::Welcome => ScrollbarAxis::Vertical,
        }
    }
}

/// Every scrolling surface, with the element id its bar is drawn under.
///
/// The ids live here rather than inside the elements they overlay — [`TabBar`]
/// would be the obvious home for the first — because a drag of a thumb is
/// answered by the workspace, and the id is what tells one bar's drag from any
/// other bar's in the window. Iterating this is how the drag and release paths
/// find which bar an event belongs to.
const SCROLLBARS: [(&str, Surface); 2] = [
    ("tab-scrollbar", Surface::Tabs),
    ("welcome-scrollbar", Surface::Welcome),
];

/// Placeholder for a status bar cell with nothing to report.
///
/// Punctuation rather than a word, so it is the same in every language.
const NOTHING: SharedString = SharedString::new_static("—");

/// Width of the welcome screen's column, in logical pixels.
///
/// Fixed rather than fluid, and the width logman gives the same column: wide
/// enough for a profile's name beside its host, narrow enough to read as one
/// card in a maximised window rather than a screen-wide smear of rows.
const WELCOME_WIDTH: f32 = 320.;

/// Element id of the welcome screen's scrolling box.
const WELCOME_STATE: &str = "welcome-state";

/// Room left above and below a column that [`centered_scroll`] is scrolling.
///
/// Only ever seen once there is scrolling to do — while the column fits, the
/// automatic margins dwarf it — and there it is what keeps the first and last
/// rows off the edges of the body at either end of the travel.
const SCROLL_MARGIN: f32 = 24.;

/// Tab-ring position of the welcome screen's "new connection" button.
///
/// Ahead of the saved list, whose rows carry the indices
/// [`connection_dialog::profile_rows`] gives them.
const WELCOME_NEW_TAB: isize = 1;

/// Debug selector of the welcome screen's "new connection" button.
///
/// Compiled away outside a test build; it saves a test working the button's
/// position out from the centred column's layout.
const WELCOME_NEW_SELECTOR: &str = "welcome-new";

/// Marker for a drag of the explorer's right edge.
///
/// A type of its own rather than a [`rugpui::Splitter`]: the sidebar's width is
/// a setting in logical pixels that survives a window resize, not a share of a
/// box, and the panel can be hidden altogether — neither of which is a ratio.
struct DraggedExplorer;

/// Narrowest the explorer may be dragged, in logical pixels.
///
/// Mirrors the clamp `AppSettings::sanitize` applies, so a width the drag
/// produced survives the round trip through `settings.json` unchanged.
const MIN_EXPLORER_WIDTH: f32 = 140.;

/// Widest the explorer may be dragged.
const MAX_EXPLORER_WIDTH: f32 = 720.;

/// Mints an id no connection tab has ever had.
///
/// The explorer keys a whole subtree by it, so it has to survive a tab in the
/// middle of the strip being closed — which an index into the tab list does
/// not.
fn next_connection_id() -> ConnectionId {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    ConnectionId(NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed))
}

/// Reads `connections.json` for the welcome screen's list.
///
/// A store that cannot be read is logged and answered as an empty one: the
/// welcome screen would otherwise have to grow an error strip of its own for a
/// file the connection dialog already reports on, and an empty list still shows
/// the button that makes the first profile.
fn load_profiles() -> ConnectionStore {
    match ConnectionStore::load() {
        Ok(store) => store,
        Err(error) => {
            log::error!("could not read connections.json: {error:#}");
            ConnectionStore::default()
        }
    }
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
    /// Number the next query builder tab of this connection is titled with.
    ///
    /// A counter of its own rather than a share of [`WorkArea::next_query`]:
    /// the two kinds of tab are titled separately, and a window holding
    /// "Query 1" and "Builder 2" would read as though a builder had been
    /// numbered by something it has nothing to do with.
    next_builder: u64,
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
            next_builder: 1,
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

    /// Every data pane in the area, in layout order.
    ///
    /// The other half of what a connection's death is applied to, and for the
    /// same reason: a data pane holds a session handle and a cursor, so one
    /// left attached to a dead connection would keep the session — and the
    /// tunnel under it — standing (§9.3).
    fn data_panes(&self) -> Vec<Entity<DataPane>> {
        let mut found = Vec::new();
        for (_, pane) in self.panes.leaves() {
            for item in pane.items() {
                if let PaneItem::TableData(panel) = item {
                    found.push(panel.clone());
                }
            }
        }
        found
    }

    /// Every structure pane in the area, in layout order.
    ///
    /// The third thing a connection's death is applied to, and for the reason
    /// the other two are: this one holds a session handle of its own, and one
    /// left attached to a dead connection would keep the session — and the
    /// tunnel under it — standing (§9.3).
    fn struct_panes(&self) -> Vec<Entity<StructPane>> {
        let mut found = Vec::new();
        for (_, pane) in self.panes.leaves() {
            for item in pane.items() {
                if let PaneItem::TableStruct(panel) = item {
                    found.push(panel.clone());
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

/// What the keyboard is being handed to when a tab comes to the front.
///
/// Resolved out of the pane tree *before* anything is focused, because reading
/// the tree borrows the application immutably and focusing borrows it mutably.
enum FocusTarget {
    /// A query pane; the caret goes into its editor.
    Query(Entity<QueryPane>),
    /// A diagram; the keyboard goes onto its canvas.
    Erd(Entity<ErdPane>),
    /// A query builder; the keyboard goes onto its canvas too, once it has one.
    Builder(Entity<BuilderPane>),
    /// A data pane; the keyboard goes onto its grid.
    Data(Entity<DataPane>),
    /// A structure pane; the keyboard goes onto the pane itself, which is what
    /// its own fields are reached from.
    Struct(Entity<StructPane>),
    /// Anything with nothing to type into, and the empty pane.
    Shell,
}

/// The pane a close was refused over, so that it can be told to say why.
///
/// Three kinds hold work a close would destroy, and each says so in its own
/// words: a data pane's staged rows, a query pane's staged result rows (§7.9)
/// and a structure pane's staged columns are all keyed to indices of a result
/// the close would drop. Resolved out of the tree before anything is updated,
/// for the reason [`FocusTarget`] is.
enum Pending {
    /// Rows staged against a data pane's grid.
    Data(Entity<DataPane>),
    /// Rows staged against a query result's grid.
    Query(Entity<QueryPane>),
    /// Columns and constraints staged against a structure.
    Struct(Entity<StructPane>),
}

/// What a right-click on one of the shell's own surfaces landed on.
///
/// Four surfaces, one field, and that is the point: the shell can only ever
/// have one context menu open, and a single slot makes opening the second one
/// close the first without anybody having to remember to.
///
/// The panes' own surfaces — the SQL editor, the result grid, the two canvases
/// — are *not* here. Their menus are drawn by the views that own them, because
/// every command in them acts on state the shell does not have (architecture
/// document, §7.8).
enum ContextTarget {
    /// A row of the explorer tree. The tree has already moved the selection
    /// onto it, so the menu and the highlight name the same node.
    Explorer(Box<NodeId>),
    /// A connection tab, by index. Right-clicking a tab does not select it —
    /// the menu of the tab on top and of any other differ — so the index is
    /// the whole of what says which connection is meant.
    Connection(usize),
    /// A tab of one pane's strip.
    PaneTab {
        /// The pane whose strip was right-clicked.
        pane: PaneId,
        /// The tab in it.
        index: usize,
    },
    /// A row of the welcome screen's saved-connection list.
    Profile(Uuid),
}

/// A right-click on one of the shell's surfaces, while its menu is open.
struct OpenContextMenu {
    /// What was under the pointer.
    target: ContextTarget,
    /// Where the pointer was, in window coordinates.
    position: Point<Pixels>,
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
    /// The saved profiles, as the welcome screen offers them.
    ///
    /// A copy of `connections.json`, read at start-up and again whenever the
    /// connection dialog closes — the dialog is the only thing that edits the
    /// file, and it may have saved, renamed or deleted a profile while it was
    /// up. Nothing else reads this: opening a session goes through the profile
    /// this hands over, not through the file.
    profiles: ConnectionStore,
    /// Horizontal scroll of the tab strip, used to reveal the active tab.
    tab_scroll: ScrollHandle,
    /// Whether the tab strip's overlay scroll indicator is on screen.
    tab_scrollbar: ScrollbarState,
    /// Vertical scroll of the welcome screen.
    welcome_scroll: ScrollHandle,
    /// Whether the welcome screen's overlay scroll indicator is on screen.
    welcome_scrollbar: ScrollbarState,
    /// The about dialog, rendered only while it reports itself open.
    about: Entity<AboutDialog>,
    /// The connection dialog, rendered only while it reports itself open.
    connect: Entity<ConnectionDialog>,
    /// The settings dialog, rendered only while it reports itself open.
    settings: Entity<SettingsDialog>,
    /// The script extraction dialog, rendered only while it reports itself open.
    extract: Entity<ExtractDialog>,
    /// The DB-to-DB transfer dialog, rendered only while it reports itself open.
    transfer: Entity<TransferDialog>,
    /// The schema backup dialog, rendered only while it reports itself open.
    backup: Entity<BackupDialog>,
    /// The update dialog, rendered only while it reports itself open.
    ///
    /// Two things open it: the start-up check in [`update`], at most once per
    /// run and only when it found something worth saying, and the "Check for
    /// updates" command, as often as the user asks. It also owns the download
    /// and the swap that "Update" starts, which is why it is the one dialog the
    /// shell cannot always close.
    update: Entity<UpdateDialog>,
    /// Whether the application dropdown menu is showing.
    menu_open: bool,
    /// The shell's own right-click menu, while one is open.
    ///
    /// Mutually exclusive with [`Workspace::menu_open`]: the dropdown and a
    /// context menu each lay a full-window backdrop, so two of them at once
    /// would be two sheets fighting over the same press.
    context_menu: Option<OpenContextMenu>,
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
    /// Keeps the extraction dialog subscription alive.
    _extract_events: Subscription,
    /// Keeps the transfer dialog subscription alive.
    _transfer_events: Subscription,
    /// Keeps the backup dialog subscription alive.
    _backup_events: Subscription,
    /// Keeps the update dialog subscription alive.
    _update_events: Subscription,
    /// Records the window's placement as it is moved and resized.
    _bounds: Subscription,
    /// Redraws the title bar when the desktop moves its caption buttons.
    _button_layout: Subscription,
}

impl Workspace {
    /// Builds the shell with no connection open, and so no work area at all.
    ///
    /// `titlebar` is the style the window was opened with; from then on the
    /// field tracks whatever the applied settings switched the window to.
    fn new(titlebar: TitlebarStyle, window: &mut Window, cx: &mut Context<Self>) -> Self {
        // What `main` does before it opens a window, for the tests that build a
        // workspace without one: every dialog the shell draws reads the
        // identity, and the start-up check has to be talked out of reaching
        // github.com — gpui's test executor runs background tasks inline
        // whenever a test parks, and this suite builds dozens of workspaces.
        #[cfg(test)]
        {
            app_identity::install(cx);
            update::set_startup_check_enabled(false);
        }

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
                //
                // The welcome screen is re-read on the way out of the dialog by
                // either door: whichever one was taken, the file behind the
                // list may have been saved, renamed or deleted since it was
                // last read, and the list is what the user comes back to when
                // this tab is closed again.
                ConnectionDialogEvent::Connect(profile) => {
                    this.profiles = load_profiles();
                    this.open_connection((**profile).clone(), window, cx);
                }
                ConnectionDialogEvent::Dismissed => {
                    dialog.update(cx, |dialog, cx| dialog.close(cx));
                    this.profiles = load_profiles();
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
                // The tree holds no strings and none of the five object
                // commands, so its menu is drawn here (architecture document,
                // §7.8).
                //
                // An error row is the one node with no menu at all: it names
                // nothing — it is the sentence saying why its parent could not
                // be read — so every command would be greyed, and a panel of
                // nothing but greyed rows says less than no panel.
                ExplorerEvent::ContextMenu { node, position } => {
                    if !matches!(node, NodeId::Error(_)) {
                        this.open_context_menu(
                            ContextTarget::Explorer(Box::new(node.clone())),
                            *position,
                            cx,
                        );
                    }
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

        let extract = cx.new(ExtractDialog::new);
        let extract_events = cx.subscribe_in(
            &extract,
            window,
            |this, dialog, event, window, cx| match event {
                ExtractDialogEvent::Dismissed => {
                    dialog.update(cx, |dialog, cx| dialog.close(cx));
                    this.focus_shell(window, cx);
                }
            },
        );

        let transfer = cx.new(TransferDialog::new);
        let transfer_events = cx.subscribe_in(
            &transfer,
            window,
            |this, dialog, event, window, cx| match event {
                TransferDialogEvent::Dismissed => {
                    dialog.update(cx, |dialog, cx| dialog.close(cx));
                    this.focus_shell(window, cx);
                }
            },
        );

        let backup = cx.new(BackupDialog::new);
        let backup_events =
            cx.subscribe_in(
                &backup,
                window,
                |this, dialog, event, window, cx| match event {
                    BackupDialogEvent::Dismissed => {
                        dialog.update(cx, |dialog, cx| dialog.close(cx));
                        this.focus_shell(window, cx);
                    }
                },
            );

        let update = cx.new(UpdateDialog::new);
        let update_events = cx.subscribe_in(&update, window, |this, dialog, event, window, cx| {
            match event {
                UpdateDialogEvent::Ignored { tag } => {
                    // The dialog has already closed itself; writing the file is
                    // this workspace's job, through the policy `app_identity`
                    // installed, because the settings are rudbman's.
                    update::remember_ignored(tag, cx);
                    this.focus_shell(window, cx);
                }
                UpdateDialogEvent::Installed(_) => {
                    // The new build is on disk and the dialog is still on
                    // screen, deliberately: the restart is imminent and a
                    // dialog that closed itself first would flash the window
                    // back into view for a fraction of a second. Restarting is
                    // the application's call and not the shell's — a staged
                    // update merely spends its restart applying itself and
                    // re-executing once more.
                    //
                    // The path comes from the shell because only it knows that
                    // the install just renamed the running image aside:
                    // `restart_path` is the `current_exe()` of the moment
                    // `init_process_identity` ran, before any of that happened.
                    // `None` — the platform never said, or no identity was
                    // installed — is left unset, which is gpui's own default
                    // and therefore exactly the same thing.
                    if let Some(path) = rugpui_shell::restart_path() {
                        cx.set_restart_path(path);
                    }
                    cx.restart();
                }
                UpdateDialogEvent::Dismissed => {
                    dialog.update(cx, |dialog, cx| dialog.close(cx));
                    this.focus_shell(window, cx);
                }
            }
        });

        // The start-up update check, off the UI thread: it is an HTTPS request
        // to GitHub, and nothing on screen waits for it. The tag the user may
        // have ignored is read here, on the UI thread, because the settings
        // global is only reachable from it.
        //
        // The answer opens a dialog, so it deliberately does *not* go through
        // `open_about`'s `close_overlays` route: this is the one dialog nobody
        // asked for, arriving at a moment nobody chose, and it must never take
        // the screen from something the user opened themselves — a half-typed
        // connection form above all. If anything is already up, the check simply
        // says nothing and tries again next launch.
        //
        let ignored = app_settings::current(cx).ignored_update;
        cx.spawn(async move |this, cx| {
            let found = cx
                .background_executor()
                .spawn(async move { update::check(ignored.as_deref()) })
                .await;
            let Some(release) = found else {
                return;
            };
            this.update(cx, |workspace, cx| {
                if workspace.dialog_open(cx) {
                    log::debug!("update {} announced while a dialog is open", release.tag);
                    return;
                }
                workspace.update.update(cx, |dialog, cx| {
                    dialog.open(release, cx);
                });
                cx.notify();
            })
            .ok();
        })
        .detach();

        // In memory only; the file is written once, when the window closes. See
        // [`app_settings::record_window_geometry`].
        let bounds = cx.observe_window_bounds(window, |_this, window, cx| {
            record_window_geometry(window, cx);
        });

        // The desktop decides where the caption buttons go, and it can be told
        // to change its mind while the window is open — the settings dialog of
        // GNOME or KDE moves them the moment the choice is made. Nothing else
        // in the window changes when it does, so the layout is read afresh on
        // every frame (see [`Workspace::render_toolbar`]) and this only has to
        // ask for a frame.
        let this = cx.weak_entity();
        let button_layout = window.observe_button_layout_changed(move |_window, cx| {
            this.update(cx, |_, cx| cx.notify()).ok();
        });

        let settings_snapshot = app_settings::current(cx);

        Self {
            focus_handle: cx.focus_handle(),
            explorer,
            explorer_visible: settings_snapshot.explorer_visible,
            explorer_width: settings_snapshot.explorer_width,
            connections: Vec::new(),
            active_connection: 0,
            profiles: load_profiles(),
            tab_scroll: ScrollHandle::new(),
            tab_scrollbar: ScrollbarState::new(),
            welcome_scroll: ScrollHandle::new(),
            welcome_scrollbar: ScrollbarState::new(),
            about,
            connect,
            settings,
            extract,
            transfer,
            backup,
            update,
            menu_open: false,
            context_menu: None,
            confirm: None,
            titlebar,
            _about_events: about_events,
            _connect_events: connect_events,
            _explorer_events: explorer_events,
            _settings_events: settings_events,
            _extract_events: extract_events,
            _transfer_events: transfer_events,
            _backup_events: backup_events,
            _update_events: update_events,
            _bounds: bounds,
            _button_layout: button_layout,
        }
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

#[cfg(test)]
mod tests;

/// What the welcome screen's box does when its column outgrows the window.
///
/// Only [`centered_scroll`] is put under test, and only through what its scroll
/// handle reports: the arrangement is entirely a question of layout, and the
/// handle is where gpui writes down the answer — the box it measured, and how
/// far past it the column ran.
#[cfg(test)]
mod centered_scroll_tests;

pub(crate) fn run() {
    bootstrap::run();
}
