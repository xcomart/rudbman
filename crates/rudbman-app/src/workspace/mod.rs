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
//! next, and a strip of tabs from three connections at once needs a color dot
//! per tab to be legible at all. Scoping the whole area is what makes the tab
//! mean something.
//!
//! What is deliberately *not* here: the connection dialog (M1) and the explorer
//! tree (M2). The menu already carries the row that will open the first of them;
//! its handler is marked `TODO` and does nothing so far.

pub(crate) use bootstrap::{editor_theme_for, theme_dirs};

use crate::{
    AddToBuilder, BackupSchema, CheckUpdates, ClosePane, CloseTab, DismissDialog, ExtractScript,
    FocusNextPane, FocusPrevPane, NewBuilder, NewConnection, NewQuery, NextConnection, NextRegion,
    NextTab, OpenErd, OpenSettings, OpenSqlFile, PrevConnection, PrevRegion, PrevTab, QueryObject,
    Quit, ReopenClosedTab, SaveSqlFile, SaveSqlFileAs, SelectTab, ShowAbout, SplitBelow,
    SplitRight, ToggleExplorer, TransferTable, app_identity, app_settings, builder_pane,
    builder_sql, connection, context_menu, erd_pane, explorer, i18n, icons, table_detail,
};

mod actions;
mod bootstrap;
mod events;
mod startup;
use bootstrap::{app_menus, apply_themes, chrome_titlebar, record_window_geometry};
mod closing;
mod connections;
mod dialogs;
mod menus;
mod objects;
mod panes;
mod session;
mod tab_name;
mod view;
#[cfg(test)]
use view::centered_scroll;

use std::collections::HashMap;
use std::path::{Path, PathBuf};

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
    Button, ButtonVariant, DraggedTab, DraggedThumb, EditorThemeEntry, EditorThemeRegistry,
    MenuButton, MenuEntry, ResizeHandle, Scrollbar, ScrollbarAxis, ScrollbarState, Splitter,
    TabBar, TabDropState, TabItem, TabStatus, Theme, ThemeRegistry, hide_later, hide_now, modal,
    scroll_to, scrolled, set_editor_theme, set_theme, set_window_tint, theme, theme_store,
};
use rugpui_shell::chrome::{
    apply_client_inset, client_tiling, draws_own_titlebar, opening_client_inset,
    render_client_frame, titlebar_gestures, window_appearance, window_control_strips,
};
pub(crate) use rugpui_shell::menu_rows::SHORTCUT_MODIFIER;
use rugpui_shell::settings::{window_bounds_at_least, window_geometry};
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
use crate::query::{
    ConfirmRequest, FileStamp, Overwrite, QueryPane, QueryPaneEvent, SaveJob, Saved,
    decode_sql_file, read_sql_file, write_sql_file,
};
use crate::settings_dialog::{SettingsDialog, SettingsDialogEvent};
use crate::struct_pane::StructPane;
use crate::table_detail::{TableDetail, TableDetailEvent};
use crate::transfer_dialog::{TransferDialog, TransferDialogEvent, TransferTarget};

/// Key context the workspace-wide shortcuts are scoped to.
const KEY_CONTEXT: &str = "Workspace";

/// Height of the toolbar row holding the application menu and the tab strip.
///
/// Must match the height [`TabBar`] gives itself, otherwise the menu button cell
/// and the tab strip would not line up — at the interface size's scale, as the
/// strip's is (`rugpui::scaled`).
const TOOLBAR_HEIGHT: f32 = 36.;

/// Height of the status bar along the bottom of the window, before the
/// interface size scales it.
const STATUS_BAR_HEIGHT: f32 = 24.;

/// Distance from the top left of the window to the top left of the macOS
/// traffic lights, in the custom title bar style.
///
/// The buttons are 14 pt tall, so half the difference to [`TOOLBAR_HEIGHT`]
/// centers them in the toolbar band.
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

/// The chord "Save SQL file as" is bound to, and the same chord as a menu
/// hint writes it.
///
/// `Ctrl+Shift+S` where every editor puts it, and none on macOS: `Cmd` is the
/// pane modifier there, so `Cmd+Shift+S` is already [`SplitBelow`].
const SAVE_AS_SHORTCUT: Option<(&str, &str)> = if cfg!(target_os = "macos") {
    None
} else {
    Some(("ctrl-shift-s", "Ctrl+Shift+S"))
};

/// The chord "Close tab" is bound to, and the same chord as a menu hint
/// writes it.
///
/// `Ctrl+W` where every editor and browser puts it, and none on macOS for now:
/// there `Cmd+W` is [`ClosePane`] — `Cmd` is the pane modifier — and moving it
/// is a decision that has been put on hold.
const CLOSE_TAB_SHORTCUT: Option<(&str, &str)> = if cfg!(target_os = "macos") {
    None
} else {
    Some(("ctrl-w", "Ctrl+W"))
};

/// [`PANE_SHORTCUT_MODIFIER`] as a menu hint writes it.
///
/// The same key, capitalized: gpui's binding syntax is lower case and the name
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
/// easy to find and equally easy to recognize.
const SPLIT_HANDLE: f32 = 6.;

/// A surface of the workspace that scrolls, and so wears an overlay bar.
///
/// One of them: the welcome screen runs down once its column outgrows the
/// window. The tab strips are not on the list — [`TabBar`] runs the bar along
/// each of them itself. Naming the surface keeps the handlers below in the
/// shape logman uses for its own, ready for a second one.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Surface {
    /// The welcome screen shown while no tab is open.
    Welcome,
}

impl Surface {
    /// Which way the surface scrolls, and so which way its bar lies.
    fn axis(self) -> ScrollbarAxis {
        match self {
            Self::Welcome => ScrollbarAxis::Vertical,
        }
    }
}

/// Every scrolling surface, with the element id its bar is drawn under.
///
/// The ids live here rather than inside the elements they overlay because a
/// drag of a thumb is answered by the workspace, and the id is what tells one
/// bar's drag from any other bar's in the window. Iterating this is how the
/// drag and release paths find which bar an event belongs to.
const SCROLLBARS: [(&str, Surface); 1] = [("welcome-scrollbar", Surface::Welcome)];

/// Placeholder for a status bar cell with nothing to report.
///
/// Punctuation rather than a word, so it is the same in every language.
const NOTHING: SharedString = SharedString::new_static("—");

/// Width of the welcome screen's column, in logical pixels.
///
/// Fixed rather than fluid, and the width logman gives the same column: wide
/// enough for a profile's name beside its host, narrow enough to read as one
/// card in a maximized window rather than a screen-wide smear of rows.
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
/// position out from the centered column's layout.
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

/// Tag identifying the connection strip as a drag source.
///
/// Pane strips use the pane id behind them, which comes from a process-wide
/// counter starting at 1; the maximum `u64` can never collide with one.
pub(super) const CONNECTION_STRIP_TAG: u64 = u64::MAX;

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
    /// User titles keyed by the view, never by a shifting tab index.
    tab_names: HashMap<gpui::EntityId, String>,
    checkpoint: Option<session::SavedWorkspace>,
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
    /// — would be worse than a gap in the numbering. A tab brought back by
    /// "Reopen closed tab" is not a new one, and takes its old number back.
    next_query: u64,
    /// Number the next query builder tab of this connection is titled with.
    ///
    /// A counter of its own rather than a share of [`WorkArea::next_query`]:
    /// the two kinds of tab are titled separately, and a window holding
    /// "Query 1" and "Builder 2" would read as though a builder had been
    /// numbered by something it has nothing to do with.
    next_builder: u64,
    /// The query and builder tabs closed here most recently, oldest first, as
    /// "Reopen closed tab" brings them back; at most
    /// [`closing::CLOSED_TABS_KEPT`] of them.
    ///
    /// Only those two kinds: they hold what the user wrote, where every other
    /// tab is a view of something the database still has and is one click in
    /// the explorer away. Not saved with the workspace — a restart is not a
    /// mistake a moment ago.
    closed: Vec<session::SavedTab>,
}

impl WorkArea {
    /// A work area of one empty pane, which renders the empty state.
    fn new() -> Self {
        let panes = PaneTree::single(Pane::new());
        let active_pane = panes.first_leaf().0;
        Self {
            panes,
            active_pane,
            tab_names: HashMap::new(),
            checkpoint: None,
            next_query: 1,
            next_builder: 1,
            closed: Vec::new(),
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
/// silently (architecture document, §9.3): what repairs one is the user's
/// "Reconnect", from the card the work area shows or the tab's menu, which
/// starts a new attempt in the same tab.
enum ConnectionState {
    /// The connect task is in flight.
    ///
    /// The task is held rather than detached so that closing the tab abandons
    /// the attempt: a `Task` that is dropped is canceled at its next await
    /// point, and the half-opened session that may be in flight behind it is
    /// closed by `Connected`'s own `Drop`.
    Connecting { _task: Task<()> },
    /// The session is live.
    Open(Box<Connected>),
    /// The connection never opened, or the attempt was canceled.
    Failed(Failure),
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

/// Why a connection tab never opened, as the card in its work area says it.
///
/// Everything the connection dialog's own "Test" would have said about the
/// same failure: the whole message rather than the one line of the status bar,
/// the hint [`crate::connection_dialog::error_hint`] reads off the error's
/// kind, and — for a bastion whose host key nothing has trusted yet — the key,
/// so that it can be trusted from here instead of only from a test.
#[derive(Clone, Debug)]
struct Failure {
    /// The driver's, the bridge's or the tunnel's own words.
    message: SharedString,
    /// What to look at first, when the failure's kind says.
    hint: Option<SharedString>,
    /// The unknown host key the bastion presented, when that is the failure.
    host_key: Option<Box<HostKeyOffer>>,
}

impl Failure {
    /// What an attempt that ended in `error` says, for a profile tunneling
    /// through `tunnel` when it does.
    fn of(error: &ConnectError, profile: &ConnectionProfile) -> Self {
        let host_key = match error {
            ConnectError::HostKeyUnknown {
                host,
                fingerprint,
                algorithm,
            } => Some(Box::new(HostKeyOffer {
                host: host.clone(),
                port: profile.tunnel.as_ref().map_or(22, |tunnel| tunnel.port),
                algorithm: algorithm.clone(),
                fingerprint: fingerprint.clone(),
            })),
            _ => None,
        };
        Self {
            message: error.message().into(),
            hint: crate::connection_dialog::error_hint(error),
            host_key,
        }
    }
}

impl From<SharedString> for Failure {
    /// A failure that is only its message: no driver of that name, or an
    /// attempt the user canceled.
    fn from(message: SharedString) -> Self {
        Self {
            message,
            hint: None,
            host_key: None,
        }
    }
}

impl From<&'static str> for Failure {
    fn from(message: &'static str) -> Self {
        SharedString::new_static(message).into()
    }
}

impl From<String> for Failure {
    fn from(message: String) -> Self {
        SharedString::from(message).into()
    }
}

/// A bastion's host key that nothing has trusted yet, offered for trusting.
#[derive(Clone, Debug)]
struct HostKeyOffer {
    /// The bastion, as the profile names it.
    host: String,
    /// Its SSH port.
    port: u16,
    /// The key algorithm, e.g. `ssh-ed25519`.
    algorithm: String,
    /// The OpenSSH-style SHA-256 fingerprint.
    fingerprint: String,
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

/// One of the regions `F6` moves the keyboard between.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Region {
    /// The explorer's tree.
    Explorer,
    /// The active pane's tab on top: a query tab's editor, a grid, a canvas.
    Tab,
    /// A query tab's result grid.
    Results,
}

/// Where the keyboard was before a dialog took it.
///
/// A region rather than a focus handle: by the time the dialog closes, the
/// element that had the keyboard may have stopped being rendered — its tab
/// closed, its connection switched away — and handing the focus back to it
/// would strand it exactly as [`Workspace::reclaim_focus`] describes. A
/// region is asked again for what it is showing *now*.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum FocusOrigin {
    /// Nowhere in particular: the shell.
    #[default]
    Shell,
    /// The explorer's tree.
    Explorer,
    /// The work area on screen.
    Area,
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
    /// Whether `Shift+F10` or the Menu key opened it, so it opens on its
    /// first row.
    by_keyboard: bool,
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
    sessions: session::SessionStorage,
    autosave: Option<Task<()>>,
    rename_tab: Option<tab_name::RenameTab>,
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
    /// Whether `F10` opened it, so it opens on its first row.
    menu_by_keyboard: bool,
    /// The shell's own right-click menu, while one is open.
    ///
    /// Mutually exclusive with [`Workspace::menu_open`]: the dropdown and a
    /// context menu each lay a full-window backdrop, so two of them at once
    /// would be two sheets fighting over the same press.
    context_menu: Option<OpenContextMenu>,
    /// The write confirmation, while a query pane is waiting on it.
    ///
    /// Held here rather than in the pane because a modal centers itself in its
    /// nearest positioned ancestor, and inside a pane that is the wrong box.
    confirm: Option<PendingConfirm>,
    /// A close waiting on the user's answer; see [`closing`].
    close_question: Option<closing::CloseQuestion>,
    /// The write confirmation's Cancel button, which takes the keyboard when
    /// the confirmation opens.
    confirm_focus: FocusHandle,
    /// The close question's default answer — Save over a file, Cancel over a
    /// connection — which takes the keyboard when the question opens.
    close_focus: FocusHandle,
    /// The connection tab that asked the dialog for a password to reconnect
    /// with, and the profile it asked about; see [`Workspace::reconnect_with`].
    pending_reconnect: Option<(ConnectionId, Uuid)>,
    /// Where the keyboard was when the dialog on screen opened, for it to go
    /// back to when the dialog closes; see [`Workspace::restore_focus`].
    focus_origin: FocusOrigin,
    /// Where a dragged tab would land, while a drag is over a strip.
    ///
    /// Strips are tagged with a pane id for a pane's strip, and
    /// [`CONNECTION_STRIP_TAG`] for the connection strip; at most one of them
    /// shows a bar at a time.
    tab_drop: TabDropState,
    /// Title bar style currently *on the window*.
    ///
    /// Starts as the style the window was created with. Not read from the
    /// settings directly: the toolbar has to branch on what the window actually
    /// carries, and once the settings dialog can switch a live window this field
    /// is what follows the platform call rather than the stored preference.
    titlebar: TitlebarStyle,
    /// The window's close guard: asks before a quit, a close of the window
    /// or an update's restart would lose what [`Workspace::unsaved_work`]
    /// lists. Drawn last of everything, so its question is on top.
    close_guard: Entity<rugpui_shell::CloseGuard>,
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
        let about_events = cx.subscribe_in(&about, window, Self::on_about_event);

        let connect = cx.new(ConnectionDialog::new);
        let connect_events = cx.subscribe_in(&connect, window, Self::on_connect_event);

        let explorer = cx.new(Explorer::new);
        let explorer_events = cx.subscribe_in(&explorer, window, Self::on_explorer_event);

        let settings = cx.new(SettingsDialog::new);
        let settings_events = cx.subscribe_in(&settings, window, Self::on_settings_event);

        let extract = cx.new(ExtractDialog::new);
        let extract_events = cx.subscribe_in(&extract, window, Self::on_extract_event);

        let transfer = cx.new(TransferDialog::new);
        let transfer_events = cx.subscribe_in(&transfer, window, Self::on_transfer_event);

        let backup = cx.new(BackupDialog::new);
        let backup_events = cx.subscribe_in(&backup, window, Self::on_backup_event);

        let update = cx.new(UpdateDialog::new);
        let update_events = cx.subscribe_in(&update, window, Self::on_update_event);

        Self::start_background_work(cx);

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

        // Read synchronously when the platform asks whether the window may
        // close, so the list has to be a plain read of state — which
        // `unsaved_work` is.
        let workspace = cx.weak_entity();
        let saver = workspace.clone();
        let close_guard = cx.new(|cx| {
            rugpui_shell::CloseGuard::new(window, cx, move |_intent, _window, cx| {
                workspace
                    .read_with(cx, |workspace, cx| workspace.unsaved_work(cx))
                    .unwrap_or_default()
            })
            .with_save_all(move |window, cx| {
                saver
                    .update(cx, |workspace, cx| workspace.save_all_files(window, cx))
                    .unwrap_or_else(|_| Task::ready(false))
            })
        });

        cx.observe_self(|workspace, cx| workspace.schedule_workspace_save(cx))
            .detach();
        cx.on_release(|workspace, cx| workspace.flush_workspaces(cx))
            .detach();
        cx.on_app_quit(|workspace, cx| {
            workspace.flush_workspaces(cx);
            async {}
        })
        .detach();

        Self {
            sessions: session::SessionStorage::default(),
            autosave: None,
            rename_tab: None,
            focus_handle: cx.focus_handle(),
            explorer,
            explorer_visible: settings_snapshot.explorer_visible,
            explorer_width: settings_snapshot.explorer_width,
            connections: Vec::new(),
            active_connection: 0,
            profiles: load_profiles(),
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
            menu_by_keyboard: false,
            context_menu: None,
            confirm: None,
            close_question: None,
            confirm_focus: cx.focus_handle(),
            close_focus: cx.focus_handle(),
            pending_reconnect: None,
            focus_origin: FocusOrigin::Shell,
            tab_drop: TabDropState::new(),
            titlebar,
            close_guard,
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
/// rather than recomputed per leaf: the palette and the connection colors are
/// the same for every pane on screen, and the alternative is a `render_pane`
/// with seven parameters, most of them constants of the frame.
struct PaneChrome {
    tab_names: HashMap<gpui::EntityId, String>,
    /// The pane the marker is on, drawn with an accent frame.
    active: PaneId,
    /// Whether panes are framed at all, which they are once there are two.
    frame: bool,
    /// The palette.
    theme: Theme,
    /// The color tag of every connection that has one, for the tab dots.
    ///
    /// A profile with no color is simply absent, and its tabs draw no dot: an
    /// invented color would read as a tag the user chose.
    colors: HashMap<ConnectionId, Hsla>,
    /// Where a dragged tab would land, while a drag is over a strip.
    ///
    /// Copied from [`Workspace::tab_drop`] for the frame: each strip draws its
    /// own bar only when the tag names it.
    drop: TabDropState,
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
