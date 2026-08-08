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
mod backup_dialog;
mod builder_pane;
mod builder_sql;
mod caption;
mod connection;
mod connection_dialog;
mod context_menu;
mod driver_manager;
mod erd_layout;
mod erd_pane;
mod explorer;
mod extract_dialog;
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
mod transfer_dialog;
mod update;
mod update_dialog;

// Compiles `locales/*.yml` into the binary and defines the machinery `t!`
// expands to, which is why it has to sit in the crate root. `fallback = "en"`
// is per key, not per locale: a string a translator has not got to yet shows
// in English while the rest of that language stays translated.
rust_i18n::i18n!("locales", fallback = "en");

use std::collections::HashMap;
use std::path::PathBuf;

use gpui::{
    AnyElement, App, Application, Bounds, Context, Div, DragMoveEvent, Entity, FocusHandle,
    Focusable, Hsla, KeyBinding, Menu, MenuItem, MouseButton, MouseUpEvent, Pixels, Point,
    ScrollHandle, SharedString, Stateful, Subscription, Task, TitlebarOptions, Window,
    WindowBackgroundAppearance, WindowBounds, WindowControlArea, WindowOptions, actions, div, img,
    prelude::*, px, relative, size,
};
use rudbman_core::{
    AppSettings, ConnectionProfile, ConnectionStore, DriverStore, TitlebarStyle, WindowState,
};
use rudbman_ui::{
    Button, ButtonVariant, DraggedThumb, EditorThemeEntry, EditorThemeRegistry, MenuButton,
    MenuEntry, Scrollbar, ScrollbarAxis, ScrollbarState, TabBar, TabItem, TabStatus, Theme,
    ThemeRegistry, WindowControlIcons, WindowControls, hide_later, hide_now, modal, scroll_to,
    scrolled, set_editor_theme, set_theme, set_window_tint, theme, theme_store,
};
use uuid::Uuid;

use about_dialog::{AboutDialog, AboutDialogEvent};
use app_settings::WindowGeometry;
use backup_dialog::{BackupDialog, BackupDialogEvent};
use builder_pane::{BuilderPane, BuilderPaneEvent};
use caption::apply_caption_theme;
use connection::{ConnectError, Connected};
use connection_dialog::{ConnectionDialog, ConnectionDialogEvent, profile_rows};
use context_menu::MenuRow;
use erd_layout::ErdLayouts;
use erd_pane::{ErdDiagram, ErdPane, ErdPaneEvent, ErdTarget};
use explorer::{ConnectionId, Explorer, ExplorerEvent, NodeId, ObjectTarget, RootInfo};
use extract_dialog::{ExtractDialog, ExtractDialogEvent};
use i18n::ts;
use icons::Icons;
use pane_tree::{Axis, Pane, PaneId, PaneItem, PaneNode, PaneTree, SplitId};
use query::{ConfirmRequest, QueryPane, QueryPaneEvent};
use settings_dialog::{SettingsDialog, SettingsDialogEvent};
use table_detail::{TableDetail, TableDetailEvent};
use transfer_dialog::{TransferDialog, TransferDialogEvent, TransferTarget};
use update_dialog::{UpdateDialog, UpdateDialogEvent};

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
        /// Ask GitHub whether a newer release exists, showing the answer either
        /// way. Unlike the start-up check, this one is not silent and does not
        /// respect the ignored-version tag.
        CheckUpdates,
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
        /// Read a `.sql` file into a query pane on the connection whose tab is
        /// showing.
        OpenSqlFile,
        /// Open the extraction dialog over the object selected in the explorer.
        ExtractScript,
        /// Open the transfer dialog over the object selected in the explorer.
        TransferTable,
        /// Open the backup dialog over the scope the explorer's selection sits
        /// in.
        BackupSchema,
        /// Draw the ERD of the scope the explorer's selection sits in.
        OpenErd,
        /// Put the object selected in the explorer onto a query builder.
        AddToBuilder,
        /// Open an empty query builder on the connection whose tab is showing.
        NewBuilder,
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
pub(crate) const SHORTCUT_MODIFIER: &str = if cfg!(target_os = "macos") {
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
    /// Anything with nothing to type into, and the empty pane.
    Shell,
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
                    // the shell's job because the shell is what owns settings.
                    update::remember_ignored(tag, cx);
                    this.focus_shell(window, cx);
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
        // `update::check` answers `None` outright in a test build; see the note
        // on it for why the guard is there and not here.
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

    /// Opens a session on the saved profile `id`, with nothing asked first.
    ///
    /// What clicking a row of the welcome screen's list does. The profile has
    /// been saved already, so there is nothing to fill in: putting the dialog up
    /// over a profile the user has just picked would be a form to dismiss
    /// between them and the database.
    ///
    /// Nothing is checked ahead of the attempt either — not the driver, not the
    /// password. A profile whose driver has gone shows that in its tab, and a
    /// profile with no secret in the keychain is one the database is asked
    /// about: trust authentication is a perfectly ordinary way to be let in, and
    /// a dialog demanding a password first would lock those users out of their
    /// own connection.
    fn open_profile(&mut self, id: Uuid, window: &mut Window, cx: &mut Context<Self>) {
        let Some(profile) = self.profiles.get(id).cloned() else {
            // The list is a snapshot; the file may have lost the profile since.
            return;
        };
        self.open_connection(profile, window, cx);
        // The tab takes the welcome screen off screen, and with it the very row
        // that was clicked — which is holding the keyboard, because the rows are
        // in the tab ring. Left there it would swallow every action from then
        // on; see [`Workspace::reclaim_focus`]. Nothing else can have it at this
        // point: there was no work area and no sidebar to hold it.
        self.focus_shell(window, cx);
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

    /// Draws the ERD of one scope, or brings the open one to the front.
    ///
    /// The same navigation rule the detail panels follow: a second diagram of
    /// one scope would show exactly what the first one does, and — unlike a
    /// second query pane — there is nothing of the user's in it to keep apart.
    ///
    /// Nothing happens without a session behind the scope. The panel's whole
    /// content is a fetch, so a diagram over a dead connection would be a tab
    /// that can only ever say "the connection is closed".
    fn open_erd(&mut self, target: ErdTarget, window: &mut Window, cx: &mut Context<Self>) {
        if let Some((pane, index)) = self.erd_tab(&target, cx) {
            self.activate_tab(pane, index, window, cx);
            return;
        }
        if self.session_of(target.connection).is_none() {
            return;
        }

        let panel = cx.new(|cx| ErdPane::new(target, cx));
        // Subscribe first, *then* ask, for the reason `ErdPane::new` does not
        // emit: a request from a constructor reaches nobody.
        cx.subscribe_in(&panel, window, |workspace, panel, event, window, cx| {
            match event {
                ErdPaneEvent::Load(target) => {
                    workspace.load_erd(panel.clone(), (**target).clone(), cx);
                }
                ErdPaneEvent::LayoutChanged(target) => {
                    let positions = panel.read(cx).positions(cx);
                    workspace.save_erd_layout(target, positions, cx);
                }
                // Through the same gate the explorer's own double click goes
                // through, so a table reached from a diagram lands on the tab
                // that is already open for it.
                ErdPaneEvent::OpenTable(target) => {
                    workspace.open_object((**target).clone(), window, cx);
                }
            }
        })
        .detach();
        panel.update(cx, |panel, cx| panel.refresh(cx));
        self.append_tab(PaneItem::Erd(panel.clone()), window, cx);
        // A diagram opened with the keyboard should answer the keyboard: the
        // zoom and auto-arrange chords are the canvas's, and until the fetch
        // comes back the panel's own root stands in for it.
        panel.update(cx, |panel, cx| panel.take_focus(window, cx));
    }

    /// Fetches one diagram: the catalogue, and the arrangement it was left in.
    ///
    /// Both on one background task. They are wanted at the same moment and
    /// handing them to [`ErdPane::deliver`] separately would draw the grid
    /// layout for a frame and then jump.
    ///
    /// A layout file that cannot be read is logged and treated as absent: the
    /// diagram is worth drawing in its default arrangement, and a schema the
    /// user can see beats an error about a file they did not write.
    fn load_erd(&mut self, panel: Entity<ErdPane>, target: ErdTarget, cx: &mut Context<Self>) {
        let Some(session) = self.session_of(target.connection) else {
            let message = ts!("explorer.disconnected");
            cx.defer(move |cx| {
                panel.update(cx, |panel, cx| panel.deliver(Err(message), cx));
            });
            return;
        };
        let profile = self.profile_of(target.connection);

        let fetch = cx.background_spawn(async move {
            let model = erd_pane::load_model(session.session(), &target)?;
            let saved = profile
                .map(|profile| match ErdLayouts::load(profile) {
                    Ok(layouts) => layouts.positions(&target.scope),
                    Err(error) => {
                        log::warn!("the saved ERD layout could not be read: {error:#}");
                        HashMap::new()
                    }
                })
                .unwrap_or_default();
            Ok::<ErdDiagram, String>(ErdDiagram { model, saved })
        });
        cx.spawn(async move |_workspace, cx| {
            let outcome = fetch.await.map_err(SharedString::from);
            panel
                .update(cx, |panel, cx| panel.deliver(outcome, cx))
                .ok();
        })
        .detach();
    }

    /// Writes one scope's box positions to `erd/<profile-uuid>.json`.
    ///
    /// Once per gesture, because [`ErdPaneEvent::LayoutChanged`] arrives once
    /// per gesture — the same discipline the sidebar's width follows, and for
    /// the same reason: a file written per frame would be the only thing in the
    /// frame doing work.
    ///
    /// Read, edit and write happen together on one background task. Two
    /// gestures finishing at once therefore settle as last writer wins, which
    /// for one user dragging one box is the only outcome there is.
    fn save_erd_layout(
        &mut self,
        target: &ErdTarget,
        positions: HashMap<String, (f32, f32)>,
        cx: &mut Context<Self>,
    ) {
        let Some(profile) = self.profile_of(target.connection) else {
            return;
        };
        let scope = target.scope.clone();
        cx.background_spawn(async move {
            let mut layouts = ErdLayouts::load(profile).unwrap_or_else(|error| {
                log::warn!("the saved ERD layout could not be read: {error:#}");
                ErdLayouts::default()
            });
            layouts.set_positions(&scope, positions);
            if let Err(error) = layouts.save(profile) {
                log::error!("the ERD layout could not be saved: {error:#}");
            }
        })
        .detach();
    }

    /// The profile one open connection was created from.
    ///
    /// The layout file is keyed by it rather than by [`ConnectionId`], which
    /// lives only as long as the tab does.
    fn profile_of(&self, connection: ConnectionId) -> Option<uuid::Uuid> {
        self.connections
            .iter()
            .find(|open| open.id == connection)
            .map(|open| open.profile.id)
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
        let dialect = Self::dialect_of(&profile);
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

    /// The dialect one profile's statements are written for.
    ///
    /// The *driver's*, not the profile's: a profile names a driver and a driver
    /// names a dialect. Read from `drivers.json` each time rather than cached,
    /// because the driver manager can rewrite that file while the window is
    /// open, and a driver that has gone falls back to the generic profile
    /// rather than to nothing.
    fn dialect_of(profile: &ConnectionProfile) -> String {
        DriverStore::load()
            .ok()
            .and_then(|store| {
                store
                    .get(&profile.driver_id)
                    .map(|driver| driver.dialect.clone())
            })
            .unwrap_or_else(|| "generic".to_string())
    }

    /// Opens a query pane over one explorer object, pre-filled with a `SELECT`.
    ///
    /// The name is written by [`builder_sql::table_ref`], which is also what
    /// the query builder's `FROM` goes through: a name that needs quoting gets
    /// it, a catalogue is not dropped on a product that has no schemas, and an
    /// ordinary name in the catalogue's own case comes out exactly as before.
    fn open_query_for(
        &mut self,
        target: &ObjectTarget,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let dialect = rudbman_sql::Dialect::from_id(&self.active_dialect());
        let name = builder_sql::table_ref(
            &dialect,
            target.catalog.as_deref(),
            target.schema.as_deref(),
            &target.name,
        );
        self.open_query(&format!("SELECT * FROM {name}"), window, cx);
    }

    /// The dialect of the connection whose tab is showing.
    fn active_dialect(&self) -> String {
        self.active_connection()
            .map(|open| Self::dialect_of(&open.profile))
            .unwrap_or_else(|| "generic".to_string())
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
            PaneItem::TableDetail(_) | PaneItem::Erd(_) | PaneItem::QueryBuilder { .. } => None,
        }
    }

    /// Opens an empty query builder in the active pane and hands it back.
    ///
    /// Gated on a live session for the reason a query pane is: the builder's
    /// tables come from `DESCRIBE`, and a canvas over a dead connection could
    /// never have anything put on it.
    fn open_builder(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<Entity<BuilderPane>> {
        let open = self.active_connection()?;
        if !matches!(open.state, ConnectionState::Open(_)) {
            return None;
        }
        let connection = open.id;
        let dialect = Self::dialect_of(&open.profile);

        let panel = cx.new(|cx| BuilderPane::new(connection, &dialect, cx));
        // Through `open_query`, which is the one gate every new query pane
        // comes through: running, cancelling and the write confirmation are its
        // pipeline, and the builder has no business owning a second one.
        cx.subscribe_in(
            &panel,
            window,
            |workspace, panel, event, window, cx| match event {
                BuilderPaneEvent::OpenSql(sql) => workspace.open_query(sql, window, cx),
                // On the panel the pointer was released over, which is the one
                // that emitted this — not whichever builder the action would have
                // picked. Dropping on a builder is aiming at it.
                BuilderPaneEvent::TableDropped(target) => {
                    workspace.add_to_builder_on(panel.clone(), target.clone(), cx);
                }
            },
        )
        .detach();

        let area = self.work_area_mut()?;
        let number = area.next_builder;
        area.next_builder += 1;
        self.append_tab(
            PaneItem::QueryBuilder {
                pane: panel.clone(),
                number,
            },
            window,
            cx,
        );
        // A builder opened with the keyboard should answer the keyboard: the
        // zoom chords are the canvas's, and until a table arrives the panel's
        // own root stands in for it.
        panel.update(cx, |panel, cx| panel.take_focus(window, cx));
        Some(panel)
    }

    /// Puts one explorer object on a query builder, opening one if there is
    /// none.
    ///
    /// The builder the object lands on is the one already in front when that is
    /// a builder, and otherwise the first one open anywhere in the work area —
    /// which is brought to the front so that the table can be seen arriving.
    /// A window with no builder at all gets one.
    ///
    /// Only ever this connection's own: a builder belongs to the connection its
    /// tab is under, and the explorer draws only the active connection's tree,
    /// so a target from anywhere else would be a table the statement could not
    /// name.
    fn add_to_builder(
        &mut self,
        target: ObjectTarget,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // Checked before a tab is opened as well as inside
        // [`Workspace::add_to_builder_on`], so that a target from a connection
        // that is not the one on screen cannot leave an empty builder behind.
        if self.active_connection().map(|open| open.id) != Some(target.connection) {
            return;
        }
        let panel = match self.builder_tab() {
            Some((pane, index)) => {
                self.activate_tab(pane, index, window, cx);
                self.builder_at(pane, index)
            }
            None => self.open_builder(window, cx),
        };
        let Some(panel) = panel else {
            return;
        };
        self.add_to_builder_on(panel, target, cx);
    }

    /// Puts one explorer object on *this* builder.
    ///
    /// Split out from [`Workspace::add_to_builder`] because a drop has already
    /// chosen its builder — the one the pointer was over — and re-running the
    /// "which builder?" rule over it could move the table to a different tab
    /// than the one the user aimed at. What both paths share is everything
    /// after that choice: the same guards, the same one-round-trip column load
    /// and the same `add_table`.
    ///
    /// Only ever this connection's own: a builder belongs to the connection its
    /// tab is under, and the explorer draws only the active connection's tree,
    /// so a target from anywhere else would be a table the statement could not
    /// name.
    fn add_to_builder_on(
        &mut self,
        panel: Entity<BuilderPane>,
        target: ObjectTarget,
        cx: &mut Context<Self>,
    ) {
        if self.active_connection().map(|open| open.id) != Some(target.connection) {
            return;
        }
        let Some(session) = self.session_of(target.connection) else {
            return;
        };

        let fetch = cx.background_spawn({
            let target = target.clone();
            async move { builder_pane::load_columns(session.session(), &target) }
        });
        cx.spawn(async move |_workspace, cx| {
            match fetch.await {
                Ok(columns) => {
                    panel
                        .update(cx, |panel, cx| panel.add_table(&target, columns, cx))
                        .ok();
                }
                // Nowhere to put this on screen: the builder has no message
                // strip, and a column list that could not be read is the same
                // failure the explorer already reports on the node itself.
                Err(error) => log::error!("the column list could not be read: {error}"),
            }
        })
        .detach();
    }

    /// Where a table added from the explorer should land.
    ///
    /// The tab in front when it is a builder, so that adding several tables in
    /// a row keeps putting them where the user is looking; otherwise the first
    /// builder in layout order.
    fn builder_tab(&self) -> Option<(PaneId, usize)> {
        let area = self.work_area()?;
        let active = area.active();
        if let Some(pane) = area.panes.get(active)
            && matches!(pane.active(), Some(PaneItem::QueryBuilder { .. }))
        {
            return Some((active, pane.active_index()));
        }
        area.panes
            .leaves()
            .into_iter()
            .find_map(|(id, pane)| pane.first_builder().map(|index| (id, index)))
    }

    /// The builder in tab `index` of `pane`, when that tab is one.
    fn builder_at(&self, pane: PaneId, index: usize) -> Option<Entity<BuilderPane>> {
        match self.work_area()?.panes.get(pane)?.get(index)? {
            PaneItem::QueryBuilder { pane, .. } => Some(pane.clone()),
            _ => None,
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

    /// Where `target`'s diagram is already open, if it is.
    fn erd_tab(&self, target: &ErdTarget, cx: &App) -> Option<(PaneId, usize)> {
        self.work_area()?
            .panes
            .leaves()
            .into_iter()
            .find_map(|(id, pane)| pane.erd_of(target, cx).map(|index| (id, index)))
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

    /// Closes several tabs of `pane` at once.
    ///
    /// `victims` are tab indices into the strip as it stands, in any order.
    /// They are removed from the highest down so that the ones still to go do
    /// not shift out from under the list — which is the whole reason this is
    /// not a loop over [`Workspace::close_tab`].
    ///
    /// The keyboard follows the same rule one close does, and is asked about
    /// once rather than once per tab: only the active tab is rendered, so only
    /// it can be holding the focus, and whatever is left on top afterwards
    /// takes it. That is what makes "close the other tabs" leave the user
    /// typing in the tab they kept.
    fn close_tabs(
        &mut self,
        pane: PaneId,
        victims: &[usize],
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if victims.is_empty() {
            return;
        }
        let held = self.pane_holds_focus(pane, window, cx);
        let Some(target) = self
            .work_area_mut()
            .and_then(|area| area.panes.get_mut(pane))
        else {
            return;
        };

        let mut order = victims.to_vec();
        order.sort_unstable();
        order.dedup();
        // Dropping the tabs drops the views in them, and with those whatever
        // cursor or fetch each was holding.
        let mut closed = Vec::new();
        for index in order.into_iter().rev() {
            closed.extend(target.close(index));
        }
        drop(closed);

        if held {
            self.focus_active_tab(pane, window, cx);
        }
        self.drop_stale_confirm(cx);
        cx.notify();
    }

    /// Splits `pane` along `axis`, whether or not the marker was on it.
    ///
    /// The menu row acts on the pane that was right-clicked, which is not
    /// necessarily the active one — a right-click moves no marker — so the
    /// marker is moved there first and the ordinary command run from where it
    /// then stands.
    fn split_pane(&mut self, pane: PaneId, axis: Axis, cx: &mut Context<Self>) {
        if !self.mark_pane(pane) {
            return;
        }
        self.split_active(axis, cx);
    }

    /// Closes `pane`, whether or not the marker was on it.
    ///
    /// The marker is moved for the reason [`Workspace::split_pane`] moves it,
    /// and moved *before* the pane goes so that the focus question
    /// [`Workspace::close_active_pane`] asks is asked about the right one.
    fn close_pane(&mut self, pane: PaneId, window: &mut Window, cx: &mut Context<Self>) {
        if !self.mark_pane(pane) {
            return;
        }
        self.close_active_pane(window, cx);
    }

    /// Puts the pane marker on `pane`, and says whether that pane exists.
    fn mark_pane(&mut self, pane: PaneId) -> bool {
        let Some(area) = self.work_area_mut() else {
            return false;
        };
        if !area.panes.contains(pane) {
            return false;
        }
        area.active_pane = pane;
        true
    }

    /// Puts the keyboard on the active tab of `pane`, or on the shell when that
    /// tab has nothing to type into.
    ///
    /// A query pane takes the caret into its editor, which is what makes closing
    /// the tab in front of one leave the user typing where they were. An ERD
    /// takes the keyboard onto its canvas, where the zoom and auto-arrange
    /// chords are bound. A detail panel and an empty pane fall back to the
    /// shell, whose handlers are what keep the menu rows and the shortcuts
    /// alive.
    fn focus_active_tab(&mut self, pane: PaneId, window: &mut Window, cx: &mut Context<Self>) {
        let target = match self
            .work_area()
            .and_then(|area| area.panes.get(pane))
            .and_then(Pane::active)
        {
            Some(PaneItem::Query { pane, .. }) => FocusTarget::Query(pane.clone()),
            Some(PaneItem::Erd(panel)) => FocusTarget::Erd(panel.clone()),
            Some(PaneItem::QueryBuilder { pane, .. }) => FocusTarget::Builder(pane.clone()),
            Some(PaneItem::TableDetail(_)) | None => FocusTarget::Shell,
        };
        match target {
            FocusTarget::Query(pane) => pane.update(cx, |pane, cx| pane.focus_editor(window, cx)),
            FocusTarget::Erd(panel) => panel.update(cx, |panel, cx| panel.take_focus(window, cx)),
            FocusTarget::Builder(panel) => {
                panel.update(cx, |panel, cx| panel.take_focus(window, cx));
            }
            FocusTarget::Shell => self.focus_shell(window, cx),
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

    /// Whether the sidebar is actually on screen.
    ///
    /// Two conditions, and only one of them is the user's: the panel is drawn
    /// when they have asked for it *and* there is a connection for it to show.
    /// A tree of nothing beside a welcome screen is a column of chrome with no
    /// content, so the welcome screen takes the whole width — and because the
    /// preference itself is left alone, the sidebar comes straight back with the
    /// first connection rather than having to be asked for again.
    fn explorer_showing(&self) -> bool {
        !self.connections.is_empty() && self.explorer_visible
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
        // Asked before the tab goes, for the same reason: it reads the frame
        // that still has the sidebar in it.
        let sidebar = self.explorer_showing();

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
        // Closing the last tab takes the sidebar off screen with it — see
        // [`Workspace::explorer_showing`] — which is the same focus hazard as
        // hiding it by hand: a focus left on the tree would swallow every action
        // from then on, the `Ctrl+B` that would bring it back included.
        if sidebar && !self.explorer_showing() {
            let explorer = self.explorer.read(cx).focus_handle(cx);
            self.reclaim_focus(&explorer, window, cx);
        }
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
            // Two handles, for the reason a query pane has two: the canvas
            // takes the focus for itself when a box is pressed, and an ERD
            // whose canvas held the keyboard would strand it exactly as
            // [`Workspace::reclaim_focus`] describes.
            Some(PaneItem::Erd(panel)) => panel.read(cx).contains_focus(window, cx),
            // Two handles again, and for the same reason: the builder's canvas
            // takes the focus when a box or a column row is pressed.
            Some(PaneItem::QueryBuilder { pane, .. }) => pane.read(cx).contains_focus(window, cx),
            None => false,
        }
    }

    /// Whether any modal is on screen.
    ///
    /// Exactly the set [`Workspace::close_overlays`] closes, minus the dropdown
    /// and the context menus: those are transient and dismiss themselves on the
    /// next press, so a dialog appearing over one takes nothing away.
    ///
    /// One caller, and the reason this exists at all: the start-up update check
    /// announces itself only into an empty window. It is the one dialog nobody
    /// asked for, and it must not land on top of a half-typed connection form
    /// or a running backup.
    fn dialog_open(&self, cx: &App) -> bool {
        self.confirm.is_some()
            || self.about.read(cx).is_open()
            || self.connect.read(cx).is_open()
            || self.settings.read(cx).is_open()
            || self.extract.read(cx).is_open()
            || self.transfer.read(cx).is_open()
            || self.backup.read(cx).is_open()
            || self.update.read(cx).is_open()
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
    ///
    /// The update dialog is closed here like the rest, so a user who reaches
    /// for a command instead of one of its buttons is not left with a stale
    /// announcement floating over the window — except while it is installing,
    /// when its own `close` refuses and the swap is allowed to finish; see
    /// [`UpdateDialog::close`].
    fn close_overlays(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.menu_open = false;
        self.close_context_menus(cx);
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
        if self.extract.read(cx).is_open() {
            // A job still running is cancelled by the close, through the drop
            // chain `extract_dialog` documents. That is the honest reading of
            // "the card is gone": a job whose progress nobody can see is one
            // nobody can stop either.
            self.extract.update(cx, |dialog, cx| dialog.close(cx));
        }
        if self.transfer.read(cx).is_open() {
            // Same reading, and the same drop chain — a transfer's cancel rolls
            // back the uncommitted tail and leaves what was committed, which is
            // §6's contract and not something closing a window can change.
            self.transfer.update(cx, |dialog, cx| dialog.close(cx));
        }
        if self.backup.read(cx).is_open() {
            self.backup.update(cx, |dialog, cx| dialog.close(cx));
        }
        if self.update.read(cx).is_open() {
            self.update.update(cx, |dialog, cx| dialog.close(cx));
        }
    }

    /// Shows or hides the application dropdown menu.
    ///
    /// Opening it puts every context menu away first. Both lay a full-window
    /// backdrop that dismisses on any press, so two of them on screen at once
    /// would be two sheets arguing over one click.
    fn set_menu_open(&mut self, open: bool, cx: &mut Context<Self>) {
        if open {
            self.close_context_menus(cx);
        }
        if self.menu_open != open {
            self.menu_open = open;
            cx.notify();
        }
    }

    /// Opens the shell's own context menu over `target`.
    ///
    /// The application dropdown goes away for the reason
    /// [`Workspace::set_menu_open`] gives, and the one slot means the menu that
    /// was open — whichever surface it belonged to — goes with it.
    fn open_context_menu(
        &mut self,
        target: ContextTarget,
        position: Point<Pixels>,
        cx: &mut Context<Self>,
    ) {
        self.menu_open = false;
        self.close_pane_context_menus(cx);
        self.context_menu = Some(OpenContextMenu { target, position });
        cx.notify();
    }

    /// Puts away every context menu anywhere in the window, and says whether
    /// there was one.
    ///
    /// Both halves: the shell's own, and the ones the panes draw for their
    /// editor, grid and canvases. What `Escape` runs first of all, and what the
    /// application dropdown runs before it opens.
    fn close_context_menus(&mut self, cx: &mut Context<Self>) -> bool {
        let mine = self.context_menu.take().is_some();
        if mine {
            cx.notify();
        }
        // Both halves, always: `|` rather than `||`, because a pane menu left
        // open behind a dismissed shell menu is exactly the state this exists
        // to prevent.
        self.close_pane_context_menus(cx) | mine
    }

    /// Puts away the context menu of every pane of the work area on screen.
    ///
    /// Every leaf and every tab of each, rather than the active pane alone:
    /// only the active tab is rendered, so only it can have a menu open — but
    /// a right-click does not move the pane marker, so the pane holding one is
    /// not necessarily the active one, and asking them all costs a walk of a
    /// tree with a handful of leaves in it.
    fn close_pane_context_menus(&mut self, cx: &mut Context<Self>) -> bool {
        let Some(area) = self.work_area() else {
            return false;
        };
        // Gathered before anything is updated: reading the tree borrows the
        // application immutably and closing a menu borrows it mutably.
        let mut queries = Vec::new();
        let mut diagrams = Vec::new();
        let mut builders = Vec::new();
        for (_, pane) in area.panes.leaves() {
            for item in pane.items() {
                match item {
                    PaneItem::Query { pane, .. } => queries.push(pane.clone()),
                    PaneItem::Erd(panel) => diagrams.push(panel.clone()),
                    PaneItem::QueryBuilder { pane, .. } => builders.push(pane.clone()),
                    PaneItem::TableDetail(_) => {}
                }
            }
        }

        let mut closed = false;
        for pane in queries {
            closed |= pane.update(cx, |pane, cx| pane.close_context_menu(cx));
        }
        for panel in diagrams {
            closed |= panel.update(cx, |panel, cx| panel.close_context_menu(cx));
        }
        for panel in builders {
            closed |= panel.update(cx, |panel, cx| panel.close_context_menu(cx));
        }
        closed
    }

    /// Opens the about dialog, closing whatever else was showing.
    fn open_about(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.close_overlays(window, cx);
        self.about.update(cx, |dialog, cx| dialog.open(cx));
        cx.notify();
    }

    /// Asks GitHub for the latest release and shows the answer.
    ///
    /// Goes through `close_overlays` where the start-up check pointedly does
    /// not: this dialog was asked for, so it is entitled to the screen the way
    /// every other menu command is.
    ///
    /// Refuses while an install is already running, which is the one case where
    /// the update dialog cannot be closed and so must not be reopened into a
    /// different state.
    fn check_updates(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.update.read(cx).is_busy() {
            return;
        }
        self.close_overlays(window, cx);
        self.update.update(cx, |dialog, cx| dialog.start_check(cx));
        cx.notify();
    }

    /// Opens the settings dialog, closing whatever else was showing.
    fn open_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.close_overlays(window, cx);
        self.settings.update(cx, |dialog, cx| dialog.open(cx));
        cx.notify();
    }

    /// Opens the extraction dialog over `target`, closing whatever else was
    /// showing.
    ///
    /// Nothing happens without a live session behind the object: the dialog's
    /// only button starts a job on one, so a card that could not have run
    /// anything is worse than no card. The session handle is passed in rather
    /// than looked up later — the tab may be closed while the dialog is up, and
    /// holding a [`connection::SessionHandle`] is what keeps the session and its
    /// tunnel standing until the job is done with them.
    fn open_extract(&mut self, target: ObjectTarget, window: &mut Window, cx: &mut Context<Self>) {
        let Some(session) = self.session_of(target.connection) else {
            return;
        };
        self.close_overlays(window, cx);
        self.extract
            .update(cx, |dialog, cx| dialog.open(target, session, cx));
        cx.notify();
    }

    /// Opens the transfer dialog over `target`, closing whatever else was
    /// showing.
    ///
    /// Nothing happens without a live session behind the object: the source
    /// query runs on it. The dialog is handed every open connection as a
    /// candidate target, the source's own included — a transfer into another
    /// schema of the same database is a real one, and the bridge's lock is
    /// reentrant — and it holds a [`connection::SessionHandle`] for whichever
    /// it is pointed at, so that closing that tab mid-transfer leaves the
    /// session standing until the job is done with it.
    fn open_transfer(&mut self, target: ObjectTarget, window: &mut Window, cx: &mut Context<Self>) {
        let Some(session) = self.session_of(target.connection) else {
            return;
        };
        let candidates = self.transfer_targets();
        self.close_overlays(window, cx);
        self.transfer.update(cx, |dialog, cx| {
            dialog.open(target, session, candidates, cx)
        });
        cx.notify();
    }

    /// Every connection a transfer could write into: the open ones, in tab
    /// order.
    fn transfer_targets(&self) -> Vec<TransferTarget> {
        self.connections
            .iter()
            .filter_map(|open| {
                let ConnectionState::Open(connected) = &open.state else {
                    return None;
                };
                Some(TransferTarget {
                    connection: open.id,
                    name: if open.profile.name.trim().is_empty() {
                        ts!("connect.unnamed")
                    } else {
                        SharedString::from(open.profile.name.clone())
                    },
                    session: connected.handle(),
                })
            })
            .collect()
    }

    /// Opens the backup dialog over `scope`, closing whatever else was showing.
    ///
    /// Gated on the session the same way, and holding its handle for the same
    /// reason: the job writes a file for as long as it takes, and the tab it
    /// was started from may be closed in the meantime.
    fn open_backup(
        &mut self,
        connection: ConnectionId,
        scope: explorer::Scope,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(session) = self.session_of(connection) else {
            return;
        };
        self.close_overlays(window, cx);
        self.backup
            .update(cx, |dialog, cx| dialog.open(scope, session, cx));
        cx.notify();
    }

    /// Reads `path` into a query pane on the connection whose tab is showing.
    ///
    /// The read is a background task because a script can be a hundred
    /// megabytes and the rope behind the editor is built for exactly that; the
    /// editor, the statement splitter and "run everything" then handle it like
    /// anything else that was typed.
    ///
    /// Invalid UTF-8 is replaced rather than refused. A `.sql` file in a legacy
    /// encoding is still mostly readable as UTF-8 — the ASCII half of it always
    /// is — and a user who can see their script can fix the part that came out
    /// wrong, which is more than an error message offers.
    fn load_sql_file(&mut self, path: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        if !self.has_live_connection() {
            return;
        }
        let reading =
            cx.background_spawn(async move { std::fs::read(&path).map(|bytes| (path, bytes)) });

        cx.spawn_in(window, async move |workspace, cx| {
            match reading.await {
                Ok((path, bytes)) => {
                    let text = String::from_utf8_lossy(&bytes).into_owned();
                    workspace
                        .update_in(cx, |workspace, window, cx| {
                            workspace.open_query(&text, window, cx);
                            log::debug!("opened {}", path.display());
                        })
                        .ok();
                }
                // Nowhere to put this on screen: the shell has no transient
                // message strip, and inventing one for the case where a file
                // the user just picked has gone away is out of proportion.
                Err(error) => log::error!("could not read the SQL file: {error}"),
            }
        })
        .detach();
    }

    /// Whether the tab on screen has a session behind it.
    fn has_live_connection(&self) -> bool {
        self.active_connection()
            .is_some_and(|open| matches!(open.state, ConnectionState::Open(_)))
    }

    /// Asks the platform for a `.sql` file and opens what it hands back.
    ///
    /// Nothing waits on the prompt, for the reason the other pickers in this
    /// application do not: on X11 that call is the one gpui had to be patched
    /// around.
    fn open_sql_file(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // Asked before the prompt as well as after: opening a file picker over
        // a window that has nowhere to put the file is a dead end the user only
        // finds out about once they have chosen one.
        if !self.has_live_connection() {
            return;
        }
        let paths = cx.prompt_for_paths(gpui::PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some(ts!("query.open_file_select")),
        });

        cx.spawn_in(window, async move |workspace, cx| {
            let chosen = match paths.await {
                Ok(Ok(Some(paths))) => paths,
                Ok(Ok(None)) | Err(_) => return,
                Ok(Err(error)) => {
                    log::warn!("the file picker could not be opened: {error:#}");
                    return;
                }
            };
            let Some(path) = chosen.into_iter().next() else {
                return;
            };
            workspace
                .update_in(cx, |workspace, window, cx| {
                    workspace.load_sql_file(path, window, cx);
                })
                .ok();
        })
        .detach();
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
        // Paired with the call below, and never with a preview: the leaf crates
        // read this to decide whether to paint their own background, and the
        // answer is only right once the surface itself permits alpha. Ahead of
        // the repaint, so the next frame already draws under the new answer.
        set_window_tint(settings.window.background_opacity, cx);
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

    /// Opens the connection dialog on one saved profile.
    ///
    /// What the welcome list's "edit…" row does, as against its "connect" row:
    /// the same dialog the button beside the list opens, showing the profile
    /// that was right-clicked rather than the first one saved.
    fn edit_profile(&mut self, id: Uuid, window: &mut Window, cx: &mut Context<Self>) {
        self.close_overlays(window, cx);
        self.connect.update(cx, |dialog, cx| dialog.open_at(id, cx));
        cx.notify();
    }

    /// The shell's own context menu, while one is open.
    ///
    /// One element for four surfaces; which rows it carries is
    /// [`ContextTarget`]. It is rendered from the workspace root rather than
    /// from the surface each menu belongs to because the tab strips and the
    /// welcome rows are built by free functions and `RenderOnce` widgets that
    /// have nowhere to keep the state — and because the root is the one box
    /// every one of those surfaces is inside of.
    fn render_context_menu(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let menu = self.context_menu.as_ref()?;
        let this = cx.entity();
        let rows = match &menu.target {
            ContextTarget::Explorer(node) => self.explorer_rows(node, cx),
            ContextTarget::Connection(index) => self.connection_rows(*index, cx),
            ContextTarget::PaneTab { pane, index } => self.pane_tab_rows(*pane, *index, cx),
            ContextTarget::Profile(id) => self.profile_rows(*id, cx),
        };

        Some(
            rudbman_ui::ContextMenu::new("workspace-context")
                .position(menu.position)
                .entries(context_menu::entries(rows))
                .on_dismiss(move |_window, cx| {
                    this.update(cx, |workspace, cx| {
                        workspace.context_menu = None;
                        cx.notify();
                    });
                })
                .into_any_element(),
        )
    }

    /// The menu of one explorer row.
    ///
    /// Node-kind driven, and the rows a kind cannot answer are left *out*
    /// rather than greyed: a schema has no "extract script" the way a table
    /// with no rows selected has no "copy", and offering one greyed for ever
    /// would be describing a command that never applies here. What *is* greyed
    /// is everything on a connection that is not open — the tree can still be
    /// read after a session dies, and none of these commands can run without
    /// one.
    ///
    /// The commands are the workspace's own methods rather than dispatched
    /// actions, and they are handed the node the menu was opened over rather
    /// than reading the selection: the two are the same thing today, because
    /// the tree moves the selection before it asks for a menu, and a row that
    /// acted on the selection would be one refactor away from acting on
    /// something else.
    fn explorer_rows(&self, node: &NodeId, cx: &mut Context<Self>) -> Vec<MenuRow> {
        let this = cx.entity();
        let connection = node.connection();
        let live = self
            .connections
            .iter()
            .any(|open| open.id == connection && matches!(open.state, ConnectionState::Open(_)));
        let relation = node
            .as_target()
            .filter(|target| target.folder.is_relation());
        let scope = node.as_scope();
        let mut rows = Vec::new();

        if let Some(target) = relation {
            let object =
                |run: fn(&mut Workspace, ObjectTarget, &mut Window, &mut Context<Self>)| {
                    let this = this.clone();
                    let target = target.clone();
                    move |window: &mut Window, cx: &mut App| {
                        let target = target.clone();
                        this.update(cx, |workspace, cx| run(workspace, target, window, cx));
                    }
                };
            rows.push(
                MenuRow::new(ts!("menu.query_object"))
                    .shortcut(format!("{SHORTCUT_MODIFIER}+Enter"))
                    .enabled(live)
                    .on_activate(object(|workspace, target, window, cx| {
                        workspace.open_query_for(&target, window, cx);
                    })),
            );
            rows.push(
                MenuRow::new(ts!("menu.add_to_builder"))
                    .enabled(live)
                    .on_activate(object(Workspace::add_to_builder)),
            );
            rows.push(
                MenuRow::new(ts!("menu.extract_script"))
                    .enabled(live)
                    .on_activate(object(Workspace::open_extract)),
            );
            rows.push(
                MenuRow::new(ts!("menu.transfer_table"))
                    .enabled(live)
                    .on_activate(object(Workspace::open_transfer)),
            );
            rows.push(MenuRow::separator());
        }

        // The connection root names no scope — a diagram of every catalogue at
        // once is not a diagram — so its rows are drawn and greyed: the menu of
        // a row that offered nothing at all would read as a broken right-click.
        let scoped = |run: fn(
            &mut Workspace,
            ConnectionId,
            explorer::Scope,
            &mut Window,
            &mut Context<Self>,
        )| {
            let this = this.clone();
            let scope = scope.clone();
            move |window: &mut Window, cx: &mut App| {
                let Some(scope) = scope.clone() else {
                    return;
                };
                this.update(cx, |workspace, cx| {
                    run(workspace, connection, scope, window, cx)
                });
            }
        };
        rows.push(
            MenuRow::new(ts!("menu.erd"))
                .shortcut(format!("{SHORTCUT_MODIFIER}+E"))
                .enabled(live && scope.is_some())
                .on_activate(scoped(|workspace, connection, scope, window, cx| {
                    workspace.open_erd(ErdTarget { connection, scope }, window, cx);
                })),
        );
        rows.push(
            MenuRow::new(ts!("menu.backup_schema"))
                .enabled(live && scope.is_some())
                .on_activate(scoped(Workspace::open_backup)),
        );
        rows
    }

    /// The menu of one connection tab.
    ///
    /// A right-click does not select the tab (see
    /// [`TabBar::on_context_menu`]), so every row here names the tab that was
    /// pressed rather than the one on screen. The two commands that open
    /// something into a work area bring that tab to the front first: they act
    /// on "the connection showing", and the alternative would be a new query
    /// pane appearing in a work area the user is not looking at.
    fn connection_rows(&self, index: usize, cx: &mut Context<Self>) -> Vec<MenuRow> {
        let this = cx.entity();
        let live = self
            .connections
            .get(index)
            .is_some_and(|open| matches!(open.state, ConnectionState::Open(_)));
        let on_tab = |run: fn(&mut Workspace, &mut Window, &mut Context<Self>)| {
            let this = this.clone();
            move |window: &mut Window, cx: &mut App| {
                this.update(cx, |workspace, cx| {
                    workspace.select_connection(index, window, cx);
                    run(workspace, window, cx);
                });
            }
        };

        vec![
            MenuRow::new(ts!("menu.new_query"))
                .shortcut(format!("{SHORTCUT_MODIFIER}+T"))
                .enabled(live)
                .on_activate(on_tab(|workspace, window, cx| {
                    workspace.open_query("", window, cx);
                })),
            MenuRow::new(ts!("menu.new_builder"))
                .enabled(live)
                .on_activate(on_tab(|workspace, window, cx| {
                    workspace.open_builder(window, cx);
                })),
            MenuRow::separator(),
            MenuRow::new(ts!("tab.close")).on_activate({
                let this = this.clone();
                move |window, cx| {
                    this.update(cx, |workspace, cx| {
                        workspace.close_connection(index, window, cx);
                    });
                }
            }),
        ]
    }

    /// The menu of one tab of one pane's strip.
    ///
    /// The three closes are the strip's, the three pane commands are the
    /// layout's, and the split between them is the separator: above it the
    /// rows act on tabs, below it on the box holding them. "Close the other
    /// tabs" and "close the tabs to the right" exist nowhere else in the
    /// program — there is no gesture for them — which is exactly the case §7.8
    /// says a menu should grow an API for.
    fn pane_tab_rows(&self, pane: PaneId, index: usize, cx: &mut Context<Self>) -> Vec<MenuRow> {
        let this = cx.entity();
        let Some(area) = self.work_area() else {
            return Vec::new();
        };
        let count = area.panes.get(pane).map_or(0, |pane| pane.items().len());
        let lone_pane = area.panes.leaf_count() <= 1;
        let close = |victims: Vec<usize>| {
            let this = this.clone();
            move |window: &mut Window, cx: &mut App| {
                this.update(cx, |workspace, cx| {
                    workspace.close_tabs(pane, &victims, window, cx);
                });
            }
        };
        let others: Vec<usize> = (0..count).filter(|other| *other != index).collect();
        let to_the_right: Vec<usize> = (index + 1..count).collect();

        vec![
            MenuRow::new(ts!("context.close_tab")).on_activate(close(vec![index])),
            MenuRow::new(ts!("context.close_others"))
                .enabled(!others.is_empty())
                .on_activate(close(others)),
            MenuRow::new(ts!("context.close_right"))
                .enabled(!to_the_right.is_empty())
                .on_activate(close(to_the_right)),
            MenuRow::separator(),
            MenuRow::new(ts!("context.split_right"))
                .shortcut(format!("{PANE_SHORTCUT_LABEL}+Shift+D"))
                .on_activate({
                    let this = this.clone();
                    move |_window, cx| {
                        this.update(cx, |workspace, cx| {
                            workspace.split_pane(pane, Axis::Horizontal, cx);
                        });
                    }
                }),
            MenuRow::new(ts!("context.split_below"))
                .shortcut(format!("{PANE_SHORTCUT_LABEL}+Shift+S"))
                .on_activate({
                    let this = this.clone();
                    move |_window, cx| {
                        this.update(cx, |workspace, cx| {
                            workspace.split_pane(pane, Axis::Vertical, cx);
                        });
                    }
                }),
            MenuRow::new(ts!("context.close_pane"))
                .shortcut(format!("{PANE_SHORTCUT_LABEL}+W"))
                .enabled(!lone_pane)
                .on_activate({
                    let this = this.clone();
                    move |window, cx| {
                        this.update(cx, |workspace, cx| workspace.close_pane(pane, window, cx));
                    }
                }),
        ]
    }

    /// The menu of one row of the welcome screen's saved list.
    ///
    /// The two things there are to do with a saved connection: open it, which
    /// is what clicking the row already does, and change it — which otherwise
    /// means opening the dialog from the button above and finding the profile
    /// again in a second copy of the same list.
    fn profile_rows(&self, id: Uuid, cx: &mut Context<Self>) -> Vec<MenuRow> {
        let this = cx.entity();
        vec![
            MenuRow::new(ts!("context.connect")).on_activate({
                let this = this.clone();
                move |window, cx| {
                    this.update(cx, |workspace, cx| workspace.open_profile(id, window, cx));
                }
            }),
            MenuRow::separator(),
            MenuRow::new(ts!("context.edit")).on_activate(move |window, cx| {
                this.update(cx, |workspace, cx| workspace.edit_profile(id, window, cx));
            }),
        ]
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

    /// Handles the "Check for updates" menu item.
    fn check_updates_action(
        &mut self,
        _: &CheckUpdates,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.check_updates(window, cx);
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

    /// Reads a `.sql` file into a query pane.
    fn open_sql_file_action(
        &mut self,
        _: &OpenSqlFile,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.set_menu_open(false, cx);
        self.open_sql_file(window, cx);
    }

    /// Opens the extraction dialog over the object selected in the explorer.
    ///
    /// Gated exactly as [`Workspace::query_object_action`] is: without a
    /// relation selected there is nothing to extract, and a dialog that opened
    /// on no object would have to invent one.
    fn extract_script_action(
        &mut self,
        _: &ExtractScript,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.set_menu_open(false, cx);
        let Some(target) = self.explorer.read(cx).selected_relation(cx) else {
            return;
        };
        self.open_extract(target, window, cx);
    }

    /// Opens the transfer dialog over the object selected in the explorer.
    ///
    /// Gated exactly as [`Workspace::extract_script_action`] is: a transfer
    /// reads one relation's rows, so without one selected there is nothing to
    /// copy.
    fn transfer_table_action(
        &mut self,
        _: &TransferTable,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.set_menu_open(false, cx);
        let Some(target) = self.explorer.read(cx).selected_relation(cx) else {
            return;
        };
        self.open_transfer(target, window, cx);
    }

    /// Opens the backup dialog over the scope the explorer's selection sits in.
    ///
    /// Gated one level wider than the transfer, exactly as
    /// [`Workspace::open_erd_action`] is: a backup is of a *scope*, so a
    /// schema, a folder and a table all name one and the connection root does
    /// not.
    fn backup_schema_action(
        &mut self,
        _: &BackupSchema,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.set_menu_open(false, cx);
        let Some((connection, scope)) = self.explorer.read(cx).selected_scope(cx) else {
            return;
        };
        self.open_backup(connection, scope, window, cx);
    }

    /// Draws the ERD of the scope the explorer's selection sits in.
    ///
    /// Wired exactly as [`Workspace::extract_script_action`] is, and gated one
    /// level wider: a diagram is of a *scope*, so a schema, a folder and a
    /// table all name one and the connection root does not. The panel opens on
    /// the connection the selected node belongs to rather than on the tab
    /// showing, which is the same thing today and would not be if the sidebar
    /// ever drew more than one root.
    fn open_erd_action(&mut self, _: &OpenErd, window: &mut Window, cx: &mut Context<Self>) {
        self.set_menu_open(false, cx);
        let Some((connection, scope)) = self.explorer.read(cx).selected_scope(cx) else {
            return;
        };
        self.open_erd(ErdTarget { connection, scope }, window, cx);
    }

    /// Puts the object selected in the explorer onto a query builder.
    ///
    /// Gated exactly as [`Workspace::query_object_action`] is, and on the same
    /// selection: a builder holds relations, so a routine or a sequence names
    /// nothing it could add.
    fn add_to_builder_action(
        &mut self,
        _: &AddToBuilder,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.set_menu_open(false, cx);
        let Some(target) = self.explorer.read(cx).selected_relation(cx) else {
            return;
        };
        self.add_to_builder(target, window, cx);
    }

    /// Opens an empty query builder on the connection whose tab is showing.
    fn new_builder_action(&mut self, _: &NewBuilder, window: &mut Window, cx: &mut Context<Self>) {
        self.set_menu_open(false, cx);
        self.open_builder(window, cx);
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
        // A context menu paints above even the dropdown, and is the most
        // transient thing on screen, so it goes first of all — the shell's own
        // and the panes' alike (architecture document, §7.8).
        if self.close_context_menus(cx) {
            return;
        }
        // The dropdown menu paints above everything else, so it goes next.
        if self.menu_open {
            self.set_menu_open(false, cx);
            return;
        }
        if self.confirm.is_some() {
            self.answer_confirm(false, window, cx);
            self.focus_shell(window, cx);
            return;
        }
        if self.update.read(cx).is_open() {
            // Swallowed rather than propagated while an install runs: the key
            // must not reach a pane, but nothing may take the screen from a
            // swap either, so `Escape` simply does nothing until it is over.
            if !self.update.read(cx).is_busy() {
                self.update.update(cx, |dialog, cx| dialog.close(cx));
                self.focus_shell(window, cx);
            }
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
        if self.extract.read(cx).is_open() {
            // Routed through the dialog: it stacks a dropdown, and while a job
            // is running `Escape` is the cancel button rather than a close —
            // dismissing the card would leave a job writing to a file with
            // nobody left to stop it.
            self.extract.update(cx, |dialog, cx| dialog.escape(cx));
            return;
        }
        if self.transfer.read(cx).is_open() {
            // Routed through the dialog for the extraction's reasons, and it
            // stacks three dropdowns rather than one.
            self.transfer.update(cx, |dialog, cx| dialog.escape(cx));
            return;
        }
        if self.backup.read(cx).is_open() {
            // No dropdown of its own, but a running job still has to take
            // `Escape` as its cancel button rather than as a close.
            self.backup.update(cx, |dialog, cx| dialog.escape(cx));
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
            // The shipped icon in its own colours: img() keeps them, where the
            // svg element would flatten the mark into a theme-tinted glyph;
            // see [`icons::APP_ICON`].
            let icon = (!cfg!(target_os = "macos"))
                .then(|| img(icons::APP_ICON).size(px(16.)).flex_none());
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
            MenuEntry::new(ts!("menu.open_sql_file"))
                .shortcut(format!("{SHORTCUT_MODIFIER}+O"))
                .on_activate(|window, cx| window.dispatch_action(Box::new(OpenSqlFile), cx)),
            MenuEntry::new(ts!("menu.extract_script"))
                .on_activate(|window, cx| window.dispatch_action(Box::new(ExtractScript), cx)),
            MenuEntry::new(ts!("menu.transfer_table"))
                .on_activate(|window, cx| window.dispatch_action(Box::new(TransferTable), cx)),
            MenuEntry::new(ts!("menu.backup_schema"))
                .on_activate(|window, cx| window.dispatch_action(Box::new(BackupSchema), cx)),
            MenuEntry::new(ts!("menu.erd"))
                .shortcut(format!("{SHORTCUT_MODIFIER}+E"))
                .on_activate(|window, cx| window.dispatch_action(Box::new(OpenErd), cx)),
            MenuEntry::new(ts!("menu.new_builder"))
                .on_activate(|window, cx| window.dispatch_action(Box::new(NewBuilder), cx)),
            MenuEntry::new(ts!("menu.add_to_builder"))
                .on_activate(|window, cx| window.dispatch_action(Box::new(AddToBuilder), cx)),
            MenuEntry::new(ts!("menu.toggle_explorer"))
                .shortcut(format!("{SHORTCUT_MODIFIER}+B"))
                .on_activate(|window, cx| window.dispatch_action(Box::new(ToggleExplorer), cx)),
            MenuEntry::new(ts!("menu.settings"))
                .shortcut(format!("{SHORTCUT_MODIFIER}+,"))
                .on_activate(|window, cx| window.dispatch_action(Box::new(OpenSettings), cx)),
            MenuEntry::separator(),
            // Next to About, where a Help menu would put it and where users of
            // every other desktop application look for it.
            MenuEntry::new(ts!("menu.check_updates"))
                .on_activate(|window, cx| window.dispatch_action(Box::new(CheckUpdates), cx)),
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
            .on_context_menu({
                let this = this.clone();
                move |index, position, _window, cx| {
                    this.update(cx, |workspace, cx| {
                        workspace.open_context_menu(ContextTarget::Connection(index), position, cx);
                    });
                }
            })
            .scroll_handle(&self.tab_scroll)
            .scrollbar(self.hovering_scrollbar(SCROLLBARS[0].0, Surface::Tabs, cx))
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
        let sidebar = self.explorer_showing().then(|| {
            div()
                .flex()
                .flex_none()
                .w(px(self.explorer_width))
                .min_h_0()
                .child(self.explorer.clone())
        });
        let handle = self.explorer_showing().then(|| {
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

        // The row paints no fill of its own. Its children tile it, and each of
        // them tints its own share: the explorer's surface under the sidebar, the
        // background below over the work area. Side by side rather than stacked
        // is exactly what [`app_settings::window_tint`] requires, and it is what
        // lets the blur behind the window carry on under the sidebar too.
        div()
            .flex()
            .flex_row()
            .flex_grow()
            .min_w_0()
            .min_h_0()
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
            .child(
                div()
                    .flex()
                    .flex_1()
                    .min_w_0()
                    .min_h_0()
                    // Everything the row is not already covering with the
                    // sidebar's own fill, and so the one fill over these pixels;
                    // see [`app_settings::window_tint`].
                    .bg(app_settings::window_tint(theme.background, cx))
                    .child(match work {
                        Some((root, chrome)) => render_pane(root, &chrome, cx),
                        None => self.render_welcome(&theme, cx),
                    }),
            )
            .into_any_element()
    }

    /// Renders the welcome screen: what the window is, with nothing open.
    ///
    /// The first screen of a first run, and the one a user comes back to every
    /// time they close their last tab, so it carries the two things there are to
    /// do from here rather than describing them: the button that makes a
    /// connection, and the connections already saved. A row of that list opens
    /// straight away — see [`Workspace::open_profile`] — which is what makes the
    /// list a way in rather than a reminder that the dialog exists.
    ///
    /// Laid out the way logman lays out its own empty state, deliberately —
    /// the two are the same author's tools and greet an empty window the same
    /// way: the application's name over one line of hint, then a fixed-width
    /// column carrying the button and, under its own small heading, the saved
    /// list. The column sits centred while it fits and scrolls from the top
    /// once it does not — [`centered_scroll`] says why those are one
    /// arrangement — under the same overlay bar the tab strip wears.
    ///
    /// It paints no fill of its own. The work area behind it already carries the
    /// tinted fill for these pixels, and a second one here would compose back
    /// to opaque; see [`app_settings::window_tint`].
    fn render_welcome(&self, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let this = cx.entity();
        let profiles = self.profiles.connections();
        let rows = profile_rows(
            profiles,
            None,
            theme,
            {
                let this = this.clone();
                move |id, window, cx| {
                    this.update(cx, |workspace, cx| workspace.open_profile(id, window, cx));
                }
            },
            Some(std::rc::Rc::new(move |id, position, _window, cx| {
                this.update(cx, |workspace, cx| {
                    workspace.open_context_menu(ContextTarget::Profile(id), position, cx);
                });
            })),
        );

        // A first run has nothing saved and no habit of the chord yet, so the
        // line under the name says what a connection is for; once something is
        // saved it carries the shortcut that skips the button instead.
        let hint = if profiles.is_empty() {
            ts!("empty.hint")
        } else {
            ts!("welcome.hint", shortcut = format!("{SHORTCUT_MODIFIER}+N"))
        };

        let saved = (!rows.is_empty()).then(|| {
            div()
                .flex()
                .flex_col()
                .gap(px(6.))
                .w(px(WELCOME_WIDTH))
                .child(
                    div()
                        .text_size(px(11.))
                        .text_color(theme.text_muted)
                        .child(ts!("welcome.saved")),
                )
                .child(div().flex().flex_col().gap(px(1.)).children(rows))
        });

        let bar = self.hovering_scrollbar(SCROLLBARS[1].0, Surface::Welcome, cx);

        let content = div()
            .flex()
            .flex_col()
            .items_center()
            .gap(px(14.))
            .child(
                div()
                    .text_size(px(30.))
                    .text_color(theme.text)
                    .child(APP_NAME),
            )
            .child(
                div()
                    .text_size(px(13.))
                    .text_color(theme.text_muted)
                    .child(hint),
            )
            .child(
                div()
                    .w(px(WELCOME_WIDTH))
                    .debug_selector(|| WELCOME_NEW_SELECTOR.to_string())
                    .child(
                        Button::new("welcome-new", ts!("welcome.new_connection"))
                            .variant(ButtonVariant::Primary)
                            .full_width(true)
                            .tab_index(WELCOME_NEW_TAB)
                            // The same action the menu row, the tab strip's
                            // plus and Ctrl+N dispatch: one command, however
                            // it is reached.
                            .on_click(|_, window, cx| {
                                window.dispatch_action(Box::new(NewConnection), cx);
                            }),
                    ),
            )
            .children(saved);

        centered_scroll(WELCOME_STATE, &self.welcome_scroll, bar, theme, content).into_any_element()
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

    /// One surface's scroll offset and the state of the bar over it.
    fn surface(&mut self, surface: Surface) -> (&ScrollHandle, &mut ScrollbarState) {
        match surface {
            Surface::Tabs => (&self.tab_scroll, &mut self.tab_scrollbar),
            Surface::Welcome => (&self.welcome_scroll, &mut self.welcome_scrollbar),
        }
    }

    /// The same pair, for the paths that only read.
    fn surface_ref(&self, surface: Surface) -> (&ScrollHandle, &ScrollbarState) {
        match surface {
            Surface::Tabs => (&self.tab_scroll, &self.tab_scrollbar),
            Surface::Welcome => (&self.welcome_scroll, &self.welcome_scrollbar),
        }
    }

    /// One surface's overlay scroll indicator, as it stands.
    ///
    /// Rebuilt on demand rather than kept, because everything it is made of —
    /// the surface's box, how far it overflows, where it sits — is measured
    /// afresh by gpui on every layout pass.
    fn scrollbar(&self, id: &'static str, surface: Surface) -> Scrollbar {
        let (handle, state) = self.surface_ref(surface);
        Scrollbar::for_handle(id, surface.axis(), handle).fade(state.fade())
    }

    /// The same bar, listening for the pointer reaching the edge it rides.
    ///
    /// Only the bars that are drawn need it: the ones the drag path builds are
    /// there to be measured, and never reach an element tree.
    fn hovering_scrollbar(
        &self,
        id: &'static str,
        surface: Surface,
        cx: &mut Context<Self>,
    ) -> Scrollbar {
        self.scrollbar(id, surface).on_hover(cx.listener(
            move |workspace, hovered: &bool, _window, cx| {
                workspace.hover_scrollbar(surface, *hovered, cx);
            },
        ))
    }

    /// Puts each surface's bar up whenever that surface has moved, and starts
    /// the clock that takes it down again.
    ///
    /// Called from `render` because that is where every way of scrolling them
    /// meets: a wheel over the tabs or the welcome screen, and the jump that
    /// brings a newly activated tab back into view.
    fn watch_scroll(&mut self, cx: &mut Context<Self>) {
        for (_, surface) in SCROLLBARS {
            let (handle, state) = self.surface(surface);
            let scrolled = scrolled(handle, surface.axis());
            if let Some(epoch) = state.moved(scrolled) {
                hide_later(epoch, cx, move |workspace| {
                    Some(workspace.surface(surface).1)
                });
            }
        }
    }

    /// Scrolls whichever surface's thumb has been dragged.
    ///
    /// Every element listening for this drag type hears every such drag, so each
    /// bar checks that the one being dragged is its own before answering.
    fn drag_scrollbar(&mut self, event: &DragMoveEvent<DraggedThumb>, cx: &mut Context<Self>) {
        for (id, surface) in SCROLLBARS {
            let Some(progress) = self.scrollbar(id, surface).dragged(event, cx) else {
                continue;
            };

            // Held even when the pointer moved along the other axis and the
            // surface has not budged: the bar has to stay up for as long as it
            // is being held, and a still pointer moves nothing to notice.
            let (handle, state) = self.surface(surface);
            state.hold();
            scroll_to(handle, surface.axis(), progress);
            cx.notify();
            return;
        }
    }

    /// Lets go of whichever thumb was being held, and starts its clock again.
    ///
    /// Every mouse release in the window arrives here; all but the one ending a
    /// drag of a bar find nothing to let go of.
    fn release_scrollbars(&mut self, cx: &mut Context<Self>) {
        for (_, surface) in SCROLLBARS {
            if let Some(epoch) = self.surface(surface).1.release() {
                hide_later(epoch, cx, move |workspace| {
                    Some(workspace.surface(surface).1)
                });
                cx.notify();
            }
        }
    }

    /// Puts one surface's bar up while the pointer rests on the edge it rides,
    /// and starts it going the moment the pointer leaves.
    ///
    /// Told which surface rather than asked to work it out: each strip carries
    /// this listener already and knows only its own.
    fn hover_scrollbar(&mut self, surface: Surface, hovered: bool, cx: &mut Context<Self>) {
        let state = self.surface(surface).1;
        if hovered {
            if state.hover_enter() {
                cx.notify();
            }
            return;
        }

        let Some(epoch) = state.hover_leave() else {
            return;
        };
        hide_now(self, epoch, cx, move |workspace| {
            Some(workspace.surface(surface).1)
        });
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
        .on_close({
            let this = this.clone();
            move |index, window, cx| {
                this.update(cx, |workspace, cx| {
                    workspace.close_tab(id, index, window, cx);
                });
            }
        })
        // Only over a tab. The empty stretch of a strip is where a title bar
        // gesture would otherwise be — on Linux a right-click there is the
        // window menu — and the widget answers for tabs alone for exactly that
        // reason.
        .on_context_menu(move |index, position, _window, cx| {
            this.update(cx, |workspace, cx| {
                workspace.open_context_menu(
                    ContextTarget::PaneTab { pane: id, index },
                    position,
                    cx,
                );
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
                Some(PaneItem::Erd(panel)) => panel.clone().into_any_element(),
                Some(PaneItem::QueryBuilder { pane, .. }) => pane.clone().into_any_element(),
                // A work area belongs to a connection, so a pane inside one is
                // empty because nothing that would fill it has been opened yet
                // — never because there is nothing to connect to.
                None => render_placeholder(theme),
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

/// Renders the empty state of a pane with no tabs.
///
/// The wording is the opposite of the welcome screen's, and the difference
/// matters: here the connection is live and the pane is empty because nothing
/// has been opened into it yet, so the words point at the explorer beside it
/// rather than at the connection dialog. One wording for both states would have
/// a live tab sitting above the words "no connections".
///
/// Text only, and no button. There is nothing here a single command would do —
/// what fills a pane is whatever the user picks out of the tree — whereas the
/// window with no connection at all has exactly one next step and
/// [`Workspace::render_welcome`] offers it as a button.
///
/// It paints no fill of its own. The work area behind it already carries the
/// tinted fill for these pixels, and a second one here would compose back to
/// opaque; see [`app_settings::window_tint`].
fn render_placeholder(theme: &Theme) -> AnyElement {
    let (title, hint) = (ts!("empty.connected_title"), ts!("empty.connected_hint"));
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

/// A box that keeps `content` in the middle while it fits, and lets it be
/// scrolled from the top once it does not.
///
/// `justify_center` does the first half and ruins the second. With more content
/// than room, a centred column hangs off both ends of its box, and scrolling
/// only ever reaches what lies past the *end* of one — so the head of the column
/// goes off the top edge and stays there, unreachable. Automatic margins share
/// out whatever room is spare, which centres the column exactly as `justify_center`
/// would, and collapse to nothing when there is none, which leaves the column at
/// the top with all of it below the fold and so all of it reachable.
///
/// Three boxes. The outermost is what the overlay bar hangs off, because the
/// scrolling box cannot hold it — its children are what scroll away underneath
/// it — and it is what the caller styles. Inside it is the box that scrolls,
/// and inside that the one carrying the margins and the breathing room that
/// keeps either end of the scroll off the edge.
fn centered_scroll(
    id: &'static str,
    scroll: &ScrollHandle,
    bar: Scrollbar,
    theme: &Theme,
    content: impl IntoElement,
) -> Div {
    div()
        .relative()
        .flex()
        .flex_col()
        .flex_grow()
        .min_h_0()
        .child(
            div()
                .id(id)
                .track_scroll(scroll)
                .flex()
                .flex_col()
                .flex_grow()
                .min_h_0()
                .items_center()
                .overflow_y_scroll()
                .child(
                    // `flex_none` so that a column taller than the box overflows
                    // it — and is scrolled to — rather than being squeezed into
                    // it, which is what a flex item does by default.
                    div()
                        .flex()
                        .flex_col()
                        .flex_none()
                        .items_center()
                        .my_auto()
                        .py(px(SCROLL_MARGIN))
                        .child(content),
                ),
        )
        .children(bar.render(theme))
}

impl Render for Workspace {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = theme(cx);
        // The one place the interface font size is read: everything below
        // inherits it unless it sets a size of its own, which is what makes the
        // setting — and the settings dialog's live preview of it — visible.
        let ui_font_size = app_settings::effective(cx).ui_font_size;
        self.watch_scroll(cx);
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
        let extract = self
            .extract
            .read(cx)
            .is_open()
            .then(|| div().absolute().inset_0().child(self.extract.clone()));
        let transfer = self
            .transfer
            .read(cx)
            .is_open()
            .then(|| div().absolute().inset_0().child(self.transfer.clone()));
        let backup = self
            .backup
            .read(cx)
            .is_open()
            .then(|| div().absolute().inset_0().child(self.backup.clone()));
        let update = self
            .update
            .read(cx)
            .is_open()
            .then(|| div().absolute().inset_0().child(self.update.clone()));
        let confirm = self.render_confirm(cx);
        let context_menu = self.render_context_menu(cx);

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
            // The overlay bars are answered from here rather than from the
            // surfaces they ride: gpui hands a drag move to every listener of
            // that type wherever it sits, and the root is the one element that
            // is always mounted while a drag of one is in flight.
            .on_drag_move::<DraggedThumb>(cx.listener(
                move |workspace, event: &DragMoveEvent<DraggedThumb>, _window, cx| {
                    workspace.drag_scrollbar(event, cx);
                },
            ))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|workspace, _: &MouseUpEvent, _window, cx| {
                    workspace.release_scrollbars(cx);
                    workspace.release_explorer(cx);
                }),
            )
            .on_mouse_up_out(
                MouseButton::Left,
                cx.listener(|workspace, _: &MouseUpEvent, _window, cx| {
                    workspace.release_scrollbars(cx);
                    workspace.release_explorer(cx);
                }),
            )
            .on_action(cx.listener(Self::new_connection_action))
            .on_action(cx.listener(Self::open_settings_action))
            .on_action(cx.listener(Self::show_about_action))
            .on_action(cx.listener(Self::check_updates_action))
            .on_action(cx.listener(Self::toggle_explorer_action))
            .on_action(cx.listener(Self::new_query_action))
            .on_action(cx.listener(Self::query_object_action))
            .on_action(cx.listener(Self::open_sql_file_action))
            .on_action(cx.listener(Self::extract_script_action))
            .on_action(cx.listener(Self::transfer_table_action))
            .on_action(cx.listener(Self::backup_schema_action))
            .on_action(cx.listener(Self::open_erd_action))
            .on_action(cx.listener(Self::add_to_builder_action))
            .on_action(cx.listener(Self::new_builder_action))
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
            .children(extract)
            .children(transfer)
            .children(backup)
            .children(update)
            .children(confirm)
            // Last: it paints above the dialogs, as its own backdrop already
            // implies, and it takes no room in the column — the element is an
            // empty absolute box whose two halves are anchored to the window.
            .children(context_menu);

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
/// About, Check for updates, Settings and Quit live in the application menu
/// because that is where macOS users look for them.
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
                MenuItem::action(ts!("menu.check_updates"), CheckUpdates),
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
                MenuItem::action(ts!("menu.open_sql_file"), OpenSqlFile),
                MenuItem::action(ts!("menu.extract_script"), ExtractScript),
                MenuItem::action(ts!("menu.transfer_table"), TransferTable),
                MenuItem::action(ts!("menu.backup_schema"), BackupSchema),
                MenuItem::action(ts!("menu.erd"), OpenErd),
                MenuItem::action(ts!("menu.new_builder"), NewBuilder),
                MenuItem::action(ts!("menu.add_to_builder"), AddToBuilder),
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
        // `Ctrl+O` is "open a file" everywhere, and the SQL editor binds no `O`
        // chord of its own for it to be taken away from.
        KeyBinding::new(&format!("{modifier}-o"), OpenSqlFile, Some(KEY_CONTEXT)),
        // Scoped to the sidebar rather than the window, because the same chord
        // is the editor's "run the statement" — see `explorer::KEY_CONTEXT`.
        KeyBinding::new(
            &format!("{modifier}-enter"),
            QueryObject,
            Some(explorer::KEY_CONTEXT),
        ),
        // Scoped to the sidebar for the same reason: it acts on what is
        // selected there, and `Ctrl+E` is a line command in several editors.
        KeyBinding::new(
            &format!("{modifier}-e"),
            OpenErd,
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

    // An update the previous run could only stage — because a JVM was loaded
    // into it and Windows will not let its files be renamed — is applied here,
    // synchronously, before the application exists and therefore before anything
    // can load a JVM into *this* process. It answers `true` only when it has
    // already spawned a fresh process on the new build, at which point the one
    // useful thing left to do is get out of its way. See `update::apply_pending`.
    if update::apply_pending() {
        return;
    }

    // The icon set has to be installed before the app runs: `svg()` resolves
    // every path through this source, and the default one answers `None`.
    Application::new().with_assets(Icons).run(|cx: &mut App| {
        if let Err(error) = rudbman_core::init_secrets() {
            log::warn!("the OS keychain is unavailable: {error}");
        }

        // A self-update renames the copies it replaces aside instead of
        // deleting them — Windows cannot delete a running image, and one code
        // path for three platforms is worth more than an immediate unlink on
        // the two that could. This is the other half: the leftovers are swept
        // up on the next launch. On the background executor because a bundled
        // JRE or a `.app` bundle is a recursive delete of thousands of files
        // and nothing on screen depends on it.
        cx.background_executor()
            .spawn(async { update::clean_leftovers() })
            .detach();

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
        rudbman_erd::init(cx);
        bind_shortcuts(cx);
        cx.set_menus(app_menus());

        // Before the palettes are applied: the ids in the settings may well
        // name themes of the user's own.
        theme_store::reload(cx);
        apply_themes(&settings, cx);
        // The same value `window_appearance` below reads, handed to the widget
        // layer so the result grid and the ERD canvases know whether to paint a
        // background of their own; see [`app_settings::window_tint`].
        set_window_tint(settings.window.background_opacity, cx);

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
    fn every_empty_state_wording_is_translated() {
        // `t!` answers with the key path when a key is missing, so a typo
        // reaches the screen as "empty.connected_ttle". The two hints have to
        // differ, or the connected state would still read like the welcome
        // screen's.
        for label in [
            ts!("welcome.hint", shortcut = "Ctrl+N"),
            ts!("welcome.new_connection"),
            ts!("welcome.saved"),
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
            for namespace in ["welcome.", "empty.", "statusbar."] {
                assert!(
                    !label.starts_with(namespace),
                    "untranslated label {label:?}"
                );
            }
        }
        assert_ne!(ts!("empty.hint"), ts!("empty.connected_hint"));
        // The welcome screen shows one hint line or the other, never both, so
        // a shared wording would make the two states indistinguishable.
        assert_ne!(ts!("welcome.hint", shortcut = "Ctrl+N"), ts!("empty.hint"));
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
        // hiding one that is showing — which takes a connection as well as the
        // preference, since the welcome screen has the window to itself; see
        // [`Workspace::explorer_showing`]. The tab needs no session behind it:
        // what is being tested is the panel, and a failed connection renders one
        // exactly as a live one does.
        window
            .update(cx, |workspace, _window, cx| {
                workspace.connections.push(Connection {
                    id: next_connection_id(),
                    profile: connection::h2::profile("explorer-focus"),
                    state: ConnectionState::Failed("no driver".into()),
                    work: WorkArea::new(),
                });
                workspace.active_connection = 0;
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

    /// The debug selector of one profile's row, as `debug_bounds` wants it.
    ///
    /// That takes a `&'static str` and the id is only known at run time, so the
    /// string is leaked: a handful of bytes for the length of a test process.
    fn row_selector(profile: &ConnectionProfile) -> &'static str {
        Box::leak(format!("{}{}", connection_dialog::ROW_SELECTOR, profile.id).into_boxed_str())
    }

    /// A driver id no `drivers.json` defines, and none ever will.
    ///
    /// Opening a profile that names it takes [`Workspace::open_connection`]'s
    /// no-driver path, which is the one outcome that does not depend on what the
    /// machine running the test has installed — and it still produces the tab
    /// these tests are about, with the reason in it.
    const MISSING_DRIVER: &str = "no-such-driver.welcome-test";

    /// A saved profile that opens into a tab and no further.
    fn unopenable_profile(name: &str) -> ConnectionProfile {
        ConnectionProfile::new(name, MISSING_DRIVER, "jdbc:rudbman:none", "sa")
    }

    /// A window showing the welcome screen, with `profiles` saved behind it.
    ///
    /// The store is set directly rather than written to `connections.json`: the
    /// file is the user's own, and what these tests are about is what the shell
    /// does with the list once it has one.
    fn workspace_over_welcome(
        profiles: &[ConnectionProfile],
        cx: &mut gpui::TestAppContext,
    ) -> gpui::WindowHandle<Workspace> {
        cx.update(|cx| {
            app_settings::init(cx);
            rudbman_ui::init(cx);
        });
        let window = cx.add_window(|window, cx| Workspace::new(TitlebarStyle::Custom, window, cx));
        window
            .update(cx, |workspace, _window, cx| {
                let mut store = ConnectionStore::default();
                for profile in profiles {
                    store.upsert(profile.clone());
                }
                workspace.profiles = store;
                cx.notify();
            })
            .expect("the window is open");
        window
    }

    /// With nothing open the welcome screen has the window to itself: no
    /// sidebar beside it, whatever the stored preference says — and the
    /// preference untouched, so the first connection brings the panel back
    /// without the user having to ask for it again.
    #[gpui::test]
    fn the_welcome_screen_has_the_window_to_itself(cx: &mut gpui::TestAppContext) {
        let saved = unopenable_profile("saved");
        let window = workspace_over_welcome(std::slice::from_ref(&saved), cx);
        window
            .update(cx, |workspace, _window, cx| {
                workspace.explorer_visible = true;
                cx.notify();
            })
            .expect("the window is open");
        let mut cx = gpui::VisualTestContext::from_window(window.into(), cx);
        cx.run_until_parked();

        // Nothing but the welcome screen draws a saved profile, so the row's
        // bounds are the assertion that the welcome screen is what the body
        // drew — and it is this test's own profile, not whatever the machine
        // running it happens to have in `connections.json`.
        assert!(
            cx.debug_bounds(row_selector(&saved)).is_some(),
            "the saved connections were not drawn"
        );

        window
            .update(&mut cx, |workspace, _window, cx| {
                assert!(
                    !workspace.explorer_showing(),
                    "the sidebar was drawn beside the welcome screen"
                );
                assert!(
                    workspace.explorer_visible,
                    "the welcome screen wrote to the user's preference"
                );

                // A tab, and the panel comes back on the preference that was
                // never touched.
                workspace.connections.push(Connection {
                    id: next_connection_id(),
                    profile: unopenable_profile("open"),
                    state: ConnectionState::Failed("no driver".into()),
                    work: WorkArea::new(),
                });
                workspace.active_connection = 0;
                workspace.sync_visible_root(cx);
                assert!(
                    workspace.explorer_showing(),
                    "the sidebar did not come back with the first connection"
                );
                // And the welcome screen is not what the body draws any more:
                // there is a work area now, and that is what stands in its
                // place.
                assert!(workspace.work_area().is_some());
            })
            .expect("the window is open");
        cx.run_until_parked();
    }

    /// Clicking a saved connection opens it, with no form in between.
    ///
    /// The profile was filled in when it was saved; putting the dialog up over
    /// one the user has just picked would be a form to dismiss between them and
    /// their database. Nothing is checked ahead of the attempt either — the tab
    /// is where a missing driver is reported, exactly as it is for a refused
    /// password.
    #[gpui::test]
    fn clicking_a_saved_connection_opens_it_with_nothing_in_between(cx: &mut gpui::TestAppContext) {
        let profile = unopenable_profile("staging");
        let window =
            workspace_over_welcome(&[unopenable_profile("production"), profile.clone()], cx);
        let mut cx = gpui::VisualTestContext::from_window(window.into(), cx);
        cx.run_until_parked();

        // The second row, so that a handler wired to the wrong profile shows up
        // as the wrong tab rather than passing by luck.
        let row = cx
            .debug_bounds(row_selector(&profile))
            .expect("both saved connections are drawn");
        cx.simulate_click(row.center(), gpui::Modifiers::none());
        cx.run_until_parked();

        window
            .update(&mut cx, |workspace, window, cx| {
                assert_eq!(workspace.connections.len(), 1, "one click, one tab");
                let open = workspace.active_connection().expect("the tab is on top");
                assert_eq!(open.profile.id, profile.id, "another profile was opened");
                assert!(
                    !workspace.connect.read(cx).is_open(),
                    "a dialog came up between the click and the connection"
                );
                match &open.state {
                    ConnectionState::Failed(message) => assert!(
                        message.contains(MISSING_DRIVER),
                        "the attempt stopped before the driver lookup: {message}"
                    ),
                    _ => panic!("the attempt did not reach the driver lookup"),
                }
                // The row that was clicked is not rendered any more, so the
                // keyboard must not still be on it.
                assert!(
                    workspace.focus_handle.is_focused(window),
                    "the focus was left on the welcome screen"
                );
            })
            .expect("the window is open");

        // The regression that rule exists for: with the focus stranded, this
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

    /// The welcome screen's button is the same command as the menu row.
    #[gpui::test]
    fn the_welcome_button_opens_the_connection_dialog(cx: &mut gpui::TestAppContext) {
        let window = workspace_over_welcome(&[], cx);
        let mut cx = gpui::VisualTestContext::from_window(window.into(), cx);
        cx.run_until_parked();

        // Nothing saved, so the screen is the heading, the button, and a line of
        // words where the list would be.
        let button = cx
            .debug_bounds(WELCOME_NEW_SELECTOR)
            .expect("the button is drawn");
        cx.simulate_click(button.center(), gpui::Modifiers::none());
        cx.run_until_parked();

        window
            .update(&mut cx, |workspace, _window, cx| {
                assert!(
                    workspace.connect.read(cx).is_open(),
                    "the button did not open the connection dialog"
                );
            })
            .expect("the window is open");
    }

    /// Closing the last tab comes back to the welcome screen, and the keyboard
    /// has to come back with it.
    ///
    /// The sidebar goes off screen the moment the last connection does — see
    /// [`Workspace::explorer_showing`] — which is the same hazard as hiding it
    /// by hand: a focus left on the tree resolves to the window root, which
    /// carries none of the workspace's handlers, and every action after it is
    /// dropped without a trace.
    #[gpui::test]
    fn closing_the_last_tab_takes_the_focus_back_from_the_sidebar(cx: &mut gpui::TestAppContext) {
        let window = workspace_over_welcome(&[], cx);
        window
            .update(cx, |workspace, _window, cx| {
                workspace.connections.push(Connection {
                    id: next_connection_id(),
                    profile: unopenable_profile("last"),
                    state: ConnectionState::Failed("no driver".into()),
                    work: WorkArea::new(),
                });
                workspace.active_connection = 0;
                workspace.explorer_visible = true;
                workspace.sync_explorer_root(0, cx);
                workspace.sync_visible_root(cx);
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

        window
            .update(&mut cx, |workspace, window, cx| {
                workspace.close_connection(0, window, cx);
                assert!(workspace.connections.is_empty());
                assert!(
                    !workspace.explorer_showing(),
                    "the sidebar outlived the last connection"
                );
                assert!(
                    workspace.focus_handle.is_focused(window),
                    "the focus was left on a sidebar nothing renders"
                );
            })
            .expect("the window is open");
        cx.run_until_parked();

        // The regression: with the focus stranded, this never arrives, and the
        // window is inert from the welcome screen on.
        cx.dispatch_action(NewConnection);
        window
            .update(&mut cx, |workspace, _window, cx| {
                assert!(
                    workspace.connect.read(cx).is_open(),
                    "the action was dropped: the focus is on something unrendered"
                );
            })
            .expect("the window is open");
    }

    /// The welcome list is re-read when the dialog closes.
    ///
    /// The dialog is the only thing that edits `connections.json`, and it may
    /// have saved, renamed or deleted a profile while it was up; a list left as
    /// it was would offer a profile that is gone, or hide one just made.
    #[gpui::test]
    fn the_welcome_list_follows_what_the_dialog_did(cx: &mut gpui::TestAppContext) {
        let stale = unopenable_profile("stale");
        let window = workspace_over_welcome(std::slice::from_ref(&stale), cx);
        let mut cx = gpui::VisualTestContext::from_window(window.into(), cx);
        cx.run_until_parked();

        window
            .update(&mut cx, |workspace, _window, cx| {
                // Emitted by the dialog on its way out, whichever way it went.
                workspace
                    .connect
                    .update(cx, |_dialog, cx| cx.emit(ConnectionDialogEvent::Dismissed));
            })
            .expect("the window is open");
        cx.run_until_parked();

        window
            .update(&mut cx, |workspace, _window, _cx| {
                // Whatever is on disk — an empty store on a machine that has
                // never saved a profile — but never the list from before.
                let ids: Vec<_> = workspace
                    .profiles
                    .connections()
                    .iter()
                    .map(|profile| profile.id)
                    .collect();
                let disk: Vec<_> = load_profiles()
                    .connections()
                    .iter()
                    .map(|profile| profile.id)
                    .collect();
                assert_eq!(ids, disk, "the list was not re-read");
                assert!(
                    workspace.profiles.get(stale.id).is_none(),
                    "the list the dialog opened over survived it"
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

    /// The scope of `connection`'s public schema, which is what a diagram is
    /// drawn over.
    fn erd_target(connection: ConnectionId) -> ErdTarget {
        ErdTarget {
            connection,
            scope: explorer::Scope {
                catalog: None,
                schema: Some("PUBLIC".to_string()),
            },
        }
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

    /// The ERD's end of the tab discipline: one diagram per scope, opened by
    /// the action the menu row dispatches, and a focus that comes back to the
    /// shell when the tab is closed.
    #[gpui::test]
    fn an_erd_opens_once_per_scope_and_the_action_finds_it(cx: &mut gpui::TestAppContext) {
        let (window, id) = workspace_over_h2("erd-tabs", cx);
        let mut cx = gpui::VisualTestContext::from_window(window.into(), cx);

        let pane = window
            .update(&mut cx, |workspace, window, cx| {
                workspace.open_object(object(id, "ORDERS"), window, cx);
                workspace.open_erd(erd_target(id), window, cx);

                let pane = active_pane(workspace);
                let titles = tab_titles(workspace, pane, cx);
                assert_eq!(titles.len(), 2, "{titles:?}");
                assert_eq!(titles[1], ts!("erd.tab", scope = "PUBLIC").to_string());
                assert_eq!(active_tab(workspace, pane), 1);
                // A diagram is not a query, so the status bar's own cells stay
                // empty over one.
                assert!(workspace.active_query().is_none());
                pane
            })
            .expect("the window is open");
        cx.run_until_parked();

        window
            .update(&mut cx, |workspace, window, cx| {
                // Back to the detail tab, and then the same scope again: a
                // navigation, not a second copy.
                workspace.activate_tab(pane, 0, window, cx);
                assert_eq!(active_tab(workspace, pane), 0);

                workspace.open_erd(erd_target(id), window, cx);
                assert_eq!(
                    tab_titles(workspace, pane, cx).len(),
                    2,
                    "the same scope opened a second tab"
                );
                assert_eq!(active_tab(workspace, pane), 1);
            })
            .expect("the window is open");
        cx.run_until_parked();

        // And through the action the menu row and the shortcut dispatch, which
        // reads the explorer's selection rather than being handed a target.
        window
            .update(&mut cx, |workspace, window, cx| {
                workspace.activate_tab(pane, 0, window, cx);
                workspace.explorer.update(cx, |explorer, cx| {
                    explorer.select(
                        NodeId::Folder {
                            connection: id,
                            scope: explorer::Scope {
                                catalog: None,
                                schema: Some("PUBLIC".to_string()),
                            },
                            folder: explorer::Folder::Tables,
                        },
                        cx,
                    );
                });
            })
            .expect("the window is open");
        cx.run_until_parked();

        cx.dispatch_action(OpenErd);
        window
            .update(&mut cx, |workspace, window, cx| {
                assert_eq!(
                    tab_titles(workspace, pane, cx).len(),
                    2,
                    "the action opened a diagram beside the one already showing"
                );
                assert_eq!(active_tab(workspace, pane), 1);

                // With the keyboard inside the diagram, closing it has to hand
                // the keyboard on: the panel stops being rendered, and gpui
                // resolves actions against the last drawn frame.
                workspace.focus_active_tab(pane, window, cx);
                assert!(
                    workspace.pane_holds_focus(pane, window, cx),
                    "the diagram did not take the keyboard"
                );
                workspace.close_tab(pane, 1, window, cx);
                assert_eq!(tab_titles(workspace, pane, cx), ["PUBLIC.ORDERS"]);
                assert!(
                    workspace.focus_handle.is_focused(window),
                    "a detail panel has nothing to type into, so the shell takes over"
                );
            })
            .expect("the window is open");
        cx.run_until_parked();
    }

    /// The whole thread, over a real database: the loader reaches H2, the
    /// panel is handed a model, and moving a box writes the layout file that
    /// the next open reads back.
    #[gpui::test]
    fn a_diagram_loads_from_the_database_and_its_layout_survives(cx: &mut gpui::TestAppContext) {
        let (window, id) = workspace_over_h2("erd-load", cx);
        let mut cx = gpui::VisualTestContext::from_window(window.into(), cx);

        // A schema of this connection's own, so the diagram has a foreign key
        // in it whatever else the H2 instance holds. Outside the update: the
        // call blocks, and the rule the shell itself follows is that nothing
        // blocking runs inside one.
        let session = window
            .update(&mut cx, |workspace, _window, _cx| {
                workspace.session_of(id).expect("the session is live")
            })
            .expect("the window is open");
        for sql in [
            "create schema if not exists ERD",
            "create table ERD.TEAM (ID int primary key, NAME varchar(40))",
            "create table ERD.PERSON (ID int primary key, TEAM_ID int not null, \
                 constraint FK_ERD_PERSON_TEAM foreign key (TEAM_ID) references ERD.TEAM(ID))",
        ] {
            session
                .session()
                .execute(&rudbman_jdbc::StatementSpec::new(sql))
                .unwrap_or_else(|error| panic!("{sql}: {error}"));
        }
        drop(session);

        let target = ErdTarget {
            connection: id,
            scope: explorer::Scope {
                catalog: None,
                schema: Some("ERD".to_string()),
            },
        };
        let panel = window
            .update(&mut cx, |workspace, window, cx| {
                workspace.open_erd(target.clone(), window, cx);
                let pane = active_pane(workspace);
                match area(workspace)
                    .panes
                    .get(pane)
                    .expect("the pane is in the tree")
                    .active()
                {
                    Some(PaneItem::Erd(panel)) => panel.clone(),
                    other => panic!("the ERD is not the tab on top: {other:?}"),
                }
            })
            .expect("the window is open");
        cx.run_until_parked();

        let positions = window
            .update(&mut cx, |_workspace, _window, cx| {
                let panel = panel.read(cx);
                assert!(!panel.is_loading(), "the fetch never came back");
                assert_eq!(panel.failure(), None);
                assert_eq!(panel.table_count(), Some(2));
                panel.positions(cx)
            })
            .expect("the window is open");
        assert!(positions.contains_key("PERSON"), "{positions:?}");
        assert!(positions.contains_key("TEAM"), "{positions:?}");

        // What a drag amounts to, without the mouse: the file is written from
        // the event the canvas raises, and the workspace is what writes it.
        let dir = tempfile::tempdir().expect("a temporary directory");
        let path = dir.path().join("layout.json");
        let mut layouts = ErdLayouts::default();
        layouts.set_positions(&target.scope, positions);
        layouts.save_to(&path).expect("the layout is written");

        let read = ErdLayouts::load_from(&path).expect("the layout is read back");
        let saved = read.positions(&target.scope);
        assert_eq!(saved.len(), 2, "{saved:?}");
        assert!(saved.contains_key("PERSON"));
    }

    /// The chord the SQL editor binds "run everything" to.
    ///
    /// Follows `rudbman_editor::init`, which is what the test harness
    /// registers; the action itself is that crate's and is not exported.
    const RUN_ALL: &str = if cfg!(target_os = "macos") {
        "cmd-shift-enter"
    } else {
        "ctrl-shift-enter"
    };

    /// Selects one table in the explorer, which is what the builder's action
    /// reads.
    fn select_table(
        window: &gpui::WindowHandle<Workspace>,
        cx: &mut gpui::VisualTestContext,
        connection: ConnectionId,
        schema: &str,
        name: &str,
    ) {
        window
            .update(cx, |workspace, _window, cx| {
                workspace.explorer.update(cx, |explorer, cx| {
                    explorer.select(
                        NodeId::Object {
                            connection,
                            scope: explorer::Scope {
                                catalog: None,
                                schema: Some(schema.to_string()),
                            },
                            folder: explorer::Folder::Tables,
                            name: name.to_string(),
                        },
                        cx,
                    );
                });
            })
            .expect("the window is open");
    }

    /// The builder that is the tab on top, if one is.
    fn active_builder(workspace: &Workspace) -> Entity<BuilderPane> {
        let pane = active_pane(workspace);
        match area(workspace)
            .panes
            .get(pane)
            .expect("the pane is in the tree")
            .active()
        {
            Some(PaneItem::QueryBuilder { pane, .. }) => pane.clone(),
            other => panic!("the builder is not the tab on top: {other:?}"),
        }
    }

    /// M7's own acceptance test: three tables joined in the builder, and the
    /// statement it produces run in a query pane that answers with rows.
    ///
    /// Everything up to the joins goes through the action the menu row
    /// dispatches, so the path being asserted is the one a user takes. The two
    /// joins are injected rather than dragged: the drag itself is the canvas
    /// widget's, tested in `rudbman-erd` against real pointer events, and what
    /// is at stake here is what the *shell* does with the result.
    #[gpui::test]
    fn three_tables_joined_in_the_builder_run_as_one_statement(cx: &mut gpui::TestAppContext) {
        let (window, id) = workspace_over_h2("builder-join", cx);
        let mut cx = gpui::VisualTestContext::from_window(window.into(), cx);

        // Outside the update, because the calls block; the same rule the ERD
        // test above follows.
        let session = window
            .update(&mut cx, |workspace, _window, _cx| {
                workspace.session_of(id).expect("the session is live")
            })
            .expect("the window is open");
        for sql in [
            "create schema if not exists BLD",
            "create table BLD.OFFICE (ID int primary key, CITY varchar(40))",
            "create table BLD.TEAM (ID int primary key, NAME varchar(40), OFFICE_ID int not null, \
                 constraint FK_BLD_TEAM_OFFICE foreign key (OFFICE_ID) references BLD.OFFICE(ID))",
            "create table BLD.PERSON (ID int primary key, NAME varchar(40), TEAM_ID int not null, \
                 constraint FK_BLD_PERSON_TEAM foreign key (TEAM_ID) references BLD.TEAM(ID))",
            "insert into BLD.OFFICE values (1, 'Seoul')",
            "insert into BLD.TEAM values (10, 'Core', 1)",
            "insert into BLD.PERSON values (100, 'Ada', 10)",
            "insert into BLD.PERSON values (101, 'Linus', 10)",
        ] {
            session
                .session()
                .execute(&rudbman_jdbc::StatementSpec::new(sql))
                .unwrap_or_else(|error| panic!("{sql}: {error}"));
        }
        drop(session);

        // The keyboard has to be somewhere inside the shell before an action is
        // dispatched: gpui resolves one against the focused element of the last
        // drawn frame, and the window the harness builds — unlike the one
        // `main` opens — starts with the focus nowhere.
        window
            .update(&mut cx, |workspace, window, cx| {
                workspace.focus_shell(window, cx);
            })
            .expect("the window is open");
        cx.run_until_parked();

        // Three tables, each through the explorer selection and the action —
        // the first of which has to open the builder tab as well.
        for name in ["PERSON", "TEAM", "OFFICE"] {
            select_table(&window, &mut cx, id, "BLD", name);
            cx.run_until_parked();
            cx.dispatch_action(AddToBuilder);
            cx.run_until_parked();
        }

        let (pane, panel) = window
            .update(&mut cx, |workspace, _window, cx| {
                let pane = active_pane(workspace);
                let panel = active_builder(workspace);
                // One tab, not three: every table lands on the builder already
                // in front.
                assert_eq!(
                    tab_titles(workspace, pane, cx),
                    [ts!("builder.tab", index = 1).to_string()]
                );
                assert_eq!(panel.read(cx).table_count(), 3);
                // A builder is not a query, so the status bar's own cells stay
                // empty over one.
                assert!(workspace.active_query().is_none());
                (pane, panel)
            })
            .expect("the window is open");

        // What drawing the two lines and clicking the three rows amounts to.
        window
            .update(&mut cx, |_workspace, _window, cx| {
                panel.update(cx, |panel, cx| {
                    panel.add_join((0, 2), (1, 0), cx);
                    panel.add_join((1, 2), (2, 0), cx);
                    panel.toggle_column(0, 1, cx);
                    panel.toggle_column(1, 1, cx);
                    panel.toggle_column(2, 1, cx);
                });
            })
            .expect("the window is open");
        cx.run_until_parked();

        let sql = window
            .update(&mut cx, |_workspace, _window, cx| panel.read(cx).sql(cx))
            .expect("the window is open");
        assert_eq!(
            sql,
            "SELECT PERSON.NAME, TEAM.NAME, OFFICE.CITY\n\
             FROM BLD.PERSON\n\
             \x20 INNER JOIN BLD.TEAM ON PERSON.TEAM_ID = TEAM.ID\n\
             \x20 INNER JOIN BLD.OFFICE ON TEAM.OFFICE_ID = OFFICE.ID"
        );

        // "Open in editor": the panel's one message, the workspace's one gate.
        window
            .update(&mut cx, |_workspace, _window, cx| {
                panel.update(cx, |panel, cx| panel.open_in_editor(cx));
            })
            .expect("the window is open");
        cx.run_until_parked();

        let query = window
            .update(&mut cx, |workspace, _window, cx| {
                assert_eq!(
                    tab_titles(workspace, pane, cx),
                    [
                        ts!("builder.tab", index = 1).to_string(),
                        ts!("query.tab", index = 1).to_string()
                    ]
                );
                let query = workspace
                    .active_query()
                    .expect("the query pane is the tab on top")
                    .clone();
                assert_eq!(query.read(cx).editor_text(cx), sql);
                query
            })
            .expect("the window is open");

        // And it runs, through the editor's own chord: the builder produced a
        // statement the database accepts, which is the whole of the milestone.
        cx.simulate_keystrokes(RUN_ALL);
        cx.run_until_parked();

        window
            .update(&mut cx, |_workspace, _window, cx| {
                let query = query.read(cx);
                assert!(!query.is_running(), "the run never came back");
                assert_eq!(
                    query.status_cells().0,
                    ts!("query.row_count", count = 2),
                    "the join did not produce the two people"
                );
            })
            .expect("the window is open");
    }

    /// A drop lands on the builder it was let go of, not on the one the action
    /// would have chosen.
    ///
    /// Two builders, the second in front — which is where "add to builder"
    /// puts everything. The table announced by the *first* has to arrive
    /// there, because the pointer was over it, and nowhere else. The drop
    /// gesture itself is `BuilderPane`'s, tested there against real pointer
    /// events; what is at stake here is which panel the shell then loads for.
    #[gpui::test]
    fn a_dropped_table_lands_on_the_builder_it_was_dropped_on(cx: &mut gpui::TestAppContext) {
        let (window, id) = workspace_over_h2("builder-drop", cx);
        let mut cx = gpui::VisualTestContext::from_window(window.into(), cx);

        let (first, second) = window
            .update(&mut cx, |workspace, window, cx| {
                let first = workspace.open_builder(window, cx).expect("a builder opens");
                let second = workspace
                    .open_builder(window, cx)
                    .expect("a second builder opens");
                (first, second)
            })
            .expect("the window is open");
        cx.run_until_parked();

        window
            .update(&mut cx, |workspace, _window, cx| {
                // The one in front is the second, so the action's rule and the
                // drop's would disagree — which is what makes the assertion
                // below mean something.
                assert_eq!(
                    workspace
                        .builder_tab()
                        .and_then(|(pane, index)| workspace.builder_at(pane, index)),
                    Some(second.clone())
                );
                first.update(cx, |_panel, cx| {
                    cx.emit(BuilderPaneEvent::TableDropped(object(id, "ORDERS")));
                });
            })
            .expect("the window is open");
        cx.run_until_parked();

        window
            .update(&mut cx, |_workspace, _window, cx| {
                assert_eq!(first.read(cx).table_count(), 1);
                assert_eq!(second.read(cx).table_count(), 0);
            })
            .expect("the window is open");
    }

    /// The explorer's own "query this object", which now writes its `FROM`
    /// through the same quoting the builder uses.
    ///
    /// The assertion is that an ordinary name is *unchanged*: quoting only
    /// where it is needed is the whole point of the new API, and a statement
    /// that suddenly came out as `SELECT * FROM "PUBLIC"."ORDERS"` would be a
    /// regression rather than a fix.
    #[gpui::test]
    fn querying_an_object_leaves_an_ordinary_name_bare(cx: &mut gpui::TestAppContext) {
        let (window, id) = workspace_over_h2("query-for", cx);
        let mut cx = gpui::VisualTestContext::from_window(window.into(), cx);

        window
            .update(&mut cx, |workspace, window, cx| {
                workspace.open_query_for(&object(id, "ORDERS"), window, cx);
                let query = workspace
                    .active_query()
                    .expect("the query pane is the tab on top");
                assert_eq!(
                    query.read(cx).editor_text(cx),
                    "SELECT * FROM PUBLIC.ORDERS"
                );

                // A name that would not survive being written bare is quoted,
                // and the schema is still in front of it.
                let mut awkward = object(id, "Order Details");
                awkward.schema = Some("PUBLIC".to_string());
                workspace.open_query_for(&awkward, window, cx);
                let query = workspace
                    .active_query()
                    .expect("the second query pane is the tab on top");
                assert_eq!(
                    query.read(cx).editor_text(cx),
                    "SELECT * FROM PUBLIC.\"Order Details\""
                );
            })
            .expect("the window is open");
        cx.run_until_parked();
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

    /// Opening a `.sql` file is a query pane with the file already in it.
    ///
    /// Driven through [`Workspace::load_sql_file`] rather than the action: the
    /// action's first step is a platform file picker, which a headless test has
    /// no way to answer.
    #[gpui::test]
    fn a_sql_file_opens_as_a_query_pane_with_its_text_in_it(cx: &mut gpui::TestAppContext) {
        let (window, _id) = workspace_over_h2("open-file", cx);
        let mut cx = gpui::VisualTestContext::from_window(window.into(), cx);

        let dir = tempfile::tempdir().expect("a temporary directory");
        let path = dir.path().join("report.sql");
        let script = "-- nightly report\nSELECT 1;\nSELECT 2;\n";
        std::fs::write(&path, script).expect("the fixture is written");

        window
            .update(&mut cx, |workspace, window, cx| {
                workspace.load_sql_file(path.clone(), window, cx);
            })
            .expect("the window is open");
        cx.run_until_parked();

        window
            .update(&mut cx, |workspace, _window, cx| {
                let pane = workspace
                    .active_query()
                    .expect("the file opened a query pane and brought it to the front");
                assert_eq!(pane.read(cx).editor_text(cx), script);
            })
            .expect("the window is open");
    }

    /// A file whose bytes are not UTF-8 still opens: the invalid runs are
    /// replaced rather than the whole file refused.
    #[gpui::test]
    fn a_file_that_is_not_utf8_opens_lossily(cx: &mut gpui::TestAppContext) {
        let (window, _id) = workspace_over_h2("open-latin1", cx);
        let mut cx = gpui::VisualTestContext::from_window(window.into(), cx);

        let dir = tempfile::tempdir().expect("a temporary directory");
        let path = dir.path().join("latin1.sql");
        // `SELECT 'é'` as Latin-1: the accented byte is not valid UTF-8.
        let mut bytes = b"SELECT '".to_vec();
        bytes.push(0xE9);
        bytes.extend_from_slice(b"'");
        std::fs::write(&path, &bytes).expect("the fixture is written");

        window
            .update(&mut cx, |workspace, window, cx| {
                workspace.load_sql_file(path.clone(), window, cx);
            })
            .expect("the window is open");
        cx.run_until_parked();

        window
            .update(&mut cx, |workspace, _window, cx| {
                let pane = workspace
                    .active_query()
                    .expect("the file opened a query pane");
                let text = pane.read(cx).editor_text(cx);
                assert!(
                    text.starts_with("SELECT '"),
                    "the readable part of the file was thrown away: {text:?}"
                );
                assert!(
                    text.contains('\u{fffd}'),
                    "the undecodable byte was dropped rather than replaced: {text:?}"
                );
            })
            .expect("the window is open");
    }

    /// With no connection open there is nowhere to put a file, so nothing is
    /// opened — the same gate [`Workspace::open_query`] applies.
    #[gpui::test]
    fn a_sql_file_needs_a_connection_to_open_into(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| {
            app_settings::init(cx);
            rudbman_ui::init(cx);
            rudbman_editor::init(cx);
            rudbman_grid::init(cx);
        });
        let window = cx.add_window(|window, cx| Workspace::new(TitlebarStyle::Custom, window, cx));
        let mut cx = gpui::VisualTestContext::from_window(window.into(), cx);

        let dir = tempfile::tempdir().expect("a temporary directory");
        let path = dir.path().join("orphan.sql");
        std::fs::write(&path, "SELECT 1").expect("the fixture is written");

        window
            .update(&mut cx, |workspace, window, cx| {
                workspace.load_sql_file(path.clone(), window, cx);
            })
            .expect("the window is open");
        cx.run_until_parked();

        window
            .update(&mut cx, |workspace, _window, _cx| {
                assert!(workspace.work_area().is_none(), "no connection, no area");
                assert!(workspace.active_query().is_none());
            })
            .expect("the window is open");
    }

    /// The scope every explorer node in these tests sits in.
    fn public() -> explorer::Scope {
        explorer::Scope {
            catalog: None,
            schema: Some("PUBLIC".to_string()),
        }
    }

    /// The two rows every explorer node offers, whatever kind it is.
    fn scope_labels() -> Vec<String> {
        vec![
            ts!("menu.erd").to_string(),
            ts!("menu.backup_schema").to_string(),
        ]
    }

    /// The four rows only a table or a view offers, and the rule under them.
    fn relation_labels() -> Vec<String> {
        vec![
            ts!("menu.query_object").to_string(),
            ts!("menu.add_to_builder").to_string(),
            ts!("menu.extract_script").to_string(),
            ts!("menu.transfer_table").to_string(),
            String::new(),
        ]
    }

    /// A right-click on a tree row asks the shell for a menu, and what that
    /// menu carries follows the kind of node it was raised over: a command a
    /// node cannot answer is left out rather than greyed for ever, and the
    /// commands that need a scope are greyed on the one node that names none.
    #[gpui::test]
    fn a_tree_menu_offers_what_its_node_can_answer(cx: &mut gpui::TestAppContext) {
        let (window, connection) = workspace_over_h2("tree-menu", cx);
        let mut cx = gpui::VisualTestContext::from_window(window.into(), cx);
        cx.run_until_parked();

        let table = NodeId::Object {
            connection,
            scope: public(),
            folder: explorer::Folder::Tables,
            name: "PERSON".to_string(),
        };
        let routine = NodeId::Object {
            connection,
            scope: public(),
            folder: explorer::Folder::Procedures,
            name: "DO_IT".to_string(),
        };
        let folder = NodeId::Folder {
            connection,
            scope: public(),
            folder: explorer::Folder::Tables,
        };
        let root = NodeId::Connection(connection);

        window
            .update(&mut cx, |workspace, _window, cx| {
                // A relation answers everything.
                let rows = workspace.explorer_rows(&table, cx);
                let mut expected = relation_labels();
                expected.extend(scope_labels());
                assert_eq!(context_menu::labels(&rows), expected);
                assert!(context_menu::greyed(&rows).is_empty());

                // A routine is an object and not a relation: `SELECT * FROM` a
                // procedure is not a statement, so those rows are absent rather
                // than greyed.
                assert_eq!(
                    context_menu::labels(&workspace.explorer_rows(&routine, cx)),
                    scope_labels()
                );
                assert_eq!(
                    context_menu::labels(&workspace.explorer_rows(&folder, cx)),
                    scope_labels()
                );

                // The connection root names no scope — a diagram of every
                // catalogue at once is not a diagram — so it keeps the rows and
                // greys them.
                let rows = workspace.explorer_rows(&root, cx);
                assert_eq!(context_menu::labels(&rows), scope_labels());
                assert_eq!(context_menu::greyed(&rows), scope_labels());
            })
            .expect("the window is open");

        // A connection that never opened greys everything: the tree can still
        // be read, and none of these commands can run without a session.
        window
            .update(&mut cx, |workspace, _window, cx| {
                let id = next_connection_id();
                workspace.connections.push(Connection {
                    id,
                    profile: unopenable_profile("dead"),
                    state: ConnectionState::Failed(SharedString::new_static("refused")),
                    work: WorkArea::new(),
                });
                let node = NodeId::Object {
                    connection: id,
                    scope: public(),
                    folder: explorer::Folder::Tables,
                    name: "PERSON".to_string(),
                };
                let rows = workspace.explorer_rows(&node, cx);
                let mut expected = relation_labels();
                expected.pop();
                expected.extend(scope_labels());
                assert_eq!(
                    context_menu::greyed(&rows),
                    expected,
                    "a dead connection offered a live command"
                );
            })
            .expect("the window is open");

        // The whole path, from the widget's event to a menu on screen: the
        // explorer promotes it, the shell keeps it, and the frame draws.
        let at = gpui::point(px(120.), px(90.));
        window
            .update(&mut cx, |workspace, _window, cx| {
                workspace.explorer.update(cx, |_explorer, cx| {
                    cx.emit(ExplorerEvent::ContextMenu {
                        node: table.clone(),
                        position: at,
                    });
                });
            })
            .expect("the window is open");
        cx.run_until_parked();
        window
            .update(&mut cx, |workspace, _window, _cx| {
                let menu = workspace.context_menu.as_ref().expect("a menu is open");
                assert_eq!(menu.position, at);
                assert!(matches!(&menu.target, ContextTarget::Explorer(node) if **node == table));
            })
            .expect("the window is open");

        // An error row names nothing — it is the sentence saying why its parent
        // could not be read — so it gets no menu at all, and the one that was
        // open is left where it is rather than replaced by an empty panel.
        window
            .update(&mut cx, |workspace, _window, cx| {
                workspace.context_menu = None;
                workspace.explorer.update(cx, |_explorer, cx| {
                    cx.emit(ExplorerEvent::ContextMenu {
                        node: NodeId::Error(Box::new(table.clone())),
                        position: at,
                    });
                });
            })
            .expect("the window is open");
        cx.run_until_parked();
        window
            .update(&mut cx, |workspace, _window, _cx| {
                assert!(
                    workspace.context_menu.is_none(),
                    "an error row was given a menu of greyed rows"
                );
            })
            .expect("the window is open");
    }

    /// "Close the other tabs" is a command with no gesture behind it, and the
    /// keyboard has to end up in the tab that stays: the strip is a place a
    /// user closes several tabs in a row from, and a focus that fell back to
    /// the shell would swallow the editor shortcuts in between.
    #[gpui::test]
    fn closing_the_other_tabs_leaves_the_keyboard_in_the_one_that_stays(
        cx: &mut gpui::TestAppContext,
    ) {
        let (window, _connection) = workspace_over_h2("tab-menu", cx);
        let mut cx = gpui::VisualTestContext::from_window(window.into(), cx);

        let pane = window
            .update(&mut cx, |workspace, window, cx| {
                for sql in ["select 1", "select 2", "select 3"] {
                    workspace.open_query(sql, window, cx);
                }
                workspace.active_pane().expect("a work area is open")
            })
            .expect("the window is open");
        cx.run_until_parked();

        window
            .update(&mut cx, |workspace, _window, cx| {
                assert_eq!(tab_titles(workspace, pane, cx).len(), 3);
                assert_eq!(active_tab(workspace, pane), 2);
            })
            .expect("the window is open");

        // Raised over the *first* tab, which is not the one on top: a right
        // click selects no tab, so the menu has to act on what was pressed.
        let rows = window
            .update(&mut cx, |workspace, _window, cx| {
                workspace.pane_tab_rows(pane, 0, cx)
            })
            .expect("the window is open");
        assert_eq!(
            context_menu::labels(&rows),
            [
                ts!("context.close_tab").to_string(),
                ts!("context.close_others").to_string(),
                ts!("context.close_right").to_string(),
                String::new(),
                ts!("context.split_right").to_string(),
                ts!("context.split_below").to_string(),
                ts!("context.close_pane").to_string(),
            ]
        );
        assert!(
            !context_menu::row(&rows, &ts!("context.close_pane")).is_enabled(),
            "the last pane of a work area was offered for closing"
        );

        cx.update(|window, cx| {
            context_menu::row(&rows, &ts!("context.close_others")).activate(window, cx);
        });
        cx.run_until_parked();

        window
            .update(&mut cx, |workspace, window, cx| {
                assert_eq!(tab_titles(workspace, pane, cx).len(), 1);
                assert_eq!(active_tab(workspace, pane), 0);
                let editor = workspace
                    .active_query()
                    .expect("the tab that stayed is the query pane")
                    .read(cx)
                    .focus_handle(cx);
                assert!(
                    editor.is_focused(window),
                    "the keyboard was left on an editor nothing renders"
                );

                // And with one tab left, both of the multi-tab rows say so.
                let rows = workspace.pane_tab_rows(pane, 0, cx);
                assert!(!context_menu::row(&rows, &ts!("context.close_others")).is_enabled());
                assert!(!context_menu::row(&rows, &ts!("context.close_right")).is_enabled());
            })
            .expect("the window is open");
    }

    /// The tabs to the right go and the ones to the left stay, whichever tab is
    /// on top.
    #[gpui::test]
    fn closing_the_tabs_to_the_right_keeps_the_ones_before_them(cx: &mut gpui::TestAppContext) {
        let (window, _connection) = workspace_over_h2("tab-menu-right", cx);
        let mut cx = gpui::VisualTestContext::from_window(window.into(), cx);

        let pane = window
            .update(&mut cx, |workspace, window, cx| {
                for sql in ["select 1", "select 2", "select 3"] {
                    workspace.open_query(sql, window, cx);
                }
                workspace.active_pane().expect("a work area is open")
            })
            .expect("the window is open");
        cx.run_until_parked();

        let before = window
            .update(&mut cx, |workspace, _window, cx| {
                tab_titles(workspace, pane, cx)
            })
            .expect("the window is open");

        let rows = window
            .update(&mut cx, |workspace, _window, cx| {
                workspace.pane_tab_rows(pane, 1, cx)
            })
            .expect("the window is open");
        cx.update(|window, cx| {
            context_menu::row(&rows, &ts!("context.close_right")).activate(window, cx);
        });
        cx.run_until_parked();

        window
            .update(&mut cx, |workspace, _window, cx| {
                assert_eq!(tab_titles(workspace, pane, cx), before[..2]);
                assert_eq!(
                    active_tab(workspace, pane),
                    1,
                    "the last tab left is on top"
                );
            })
            .expect("the window is open");
    }

    /// A connection tab's menu acts on the tab that was pressed rather than on
    /// the one showing, and greys what needs a session.
    #[gpui::test]
    fn a_connection_tab_menu_acts_on_the_tab_it_was_raised_over(cx: &mut gpui::TestAppContext) {
        let (window, _connection) = workspace_over_h2("conn-menu", cx);
        let mut cx = gpui::VisualTestContext::from_window(window.into(), cx);

        window
            .update(&mut cx, |workspace, _window, cx| {
                workspace.connections.push(Connection {
                    id: next_connection_id(),
                    profile: unopenable_profile("refused"),
                    state: ConnectionState::Failed(SharedString::new_static("no driver")),
                    work: WorkArea::new(),
                });

                let live = workspace.connection_rows(0, cx);
                assert_eq!(
                    context_menu::labels(&live),
                    [
                        ts!("menu.new_query").to_string(),
                        ts!("menu.new_builder").to_string(),
                        String::new(),
                        ts!("tab.close").to_string(),
                    ]
                );
                assert!(context_menu::greyed(&live).is_empty());

                let dead = workspace.connection_rows(1, cx);
                assert!(!context_menu::row(&dead, &ts!("menu.new_query")).is_enabled());
                assert!(!context_menu::row(&dead, &ts!("menu.new_builder")).is_enabled());
                assert!(
                    context_menu::row(&dead, &ts!("tab.close")).is_enabled(),
                    "a tab that failed to open still has to be closable"
                );
            })
            .expect("the window is open");

        // Closing acts on the pressed tab, not on the one on screen.
        let rows = window
            .update(&mut cx, |workspace, _window, cx| {
                workspace.connection_rows(1, cx)
            })
            .expect("the window is open");
        cx.update(|window, cx| {
            context_menu::row(&rows, &ts!("tab.close")).activate(window, cx);
        });
        cx.run_until_parked();
        window
            .update(&mut cx, |workspace, _window, _cx| {
                assert_eq!(workspace.connections.len(), 1);
                assert_eq!(workspace.active_connection, 0);
            })
            .expect("the window is open");
    }

    /// The welcome list's menu offers the two things there are to do with a
    /// saved connection, and "edit…" opens the dialog rather than a session.
    #[gpui::test]
    fn the_welcome_menu_opens_the_dialog_over_the_row(cx: &mut gpui::TestAppContext) {
        let saved = unopenable_profile("saved");
        let window = workspace_over_welcome(std::slice::from_ref(&saved), cx);
        let mut cx = gpui::VisualTestContext::from_window(window.into(), cx);
        cx.run_until_parked();

        let at = gpui::point(px(60.), px(200.));
        window
            .update(&mut cx, |workspace, _window, cx| {
                workspace.open_context_menu(ContextTarget::Profile(saved.id), at, cx);
            })
            .expect("the window is open");
        cx.run_until_parked();

        let rows = window
            .update(&mut cx, |workspace, _window, cx| {
                workspace.profile_rows(saved.id, cx)
            })
            .expect("the window is open");
        assert_eq!(
            context_menu::labels(&rows),
            [
                ts!("context.connect").to_string(),
                String::new(),
                ts!("context.edit").to_string(),
            ]
        );

        cx.update(|window, cx| {
            context_menu::row(&rows, &ts!("context.edit")).activate(window, cx);
        });
        cx.run_until_parked();

        window
            .update(&mut cx, |workspace, _window, cx| {
                assert!(
                    workspace.connect.read(cx).is_open(),
                    "the row did not open the dialog"
                );
                assert!(
                    workspace.connections.is_empty(),
                    "editing a profile opened a session"
                );
                // Opening a dialog closes the menu that led to it, the way
                // every other overlay does.
                assert!(workspace.context_menu.is_none());
            })
            .expect("the window is open");
    }

    /// `Escape` closes the context menu before it closes anything else — the
    /// shell's own and the panes' alike — and leaves the dialog stack exactly
    /// as it was, so the next press finds it.
    #[gpui::test]
    fn escape_closes_the_context_menu_before_the_dialog_under_it(cx: &mut gpui::TestAppContext) {
        let (window, connection) = workspace_over_h2("escape-menu", cx);
        let mut cx = gpui::VisualTestContext::from_window(window.into(), cx);

        let query = window
            .update(&mut cx, |workspace, window, cx| {
                workspace.open_query("select 1", window, cx);
                workspace.open_about(window, cx);
                workspace.open_context_menu(
                    ContextTarget::Connection(0),
                    gpui::point(px(30.), px(10.)),
                    cx,
                );
                workspace.active_query().expect("the pane is open").clone()
            })
            .expect("the window is open");
        // A pane menu of its own, which the shell has to reach as well: a right
        // click moves no pane marker, so the pane holding one is not
        // necessarily the active one.
        window
            .update(&mut cx, |_workspace, _window, cx| {
                query.update(cx, |pane, cx| {
                    pane.open_editor_menu(gpui::point(px(40.), px(80.)), cx);
                });
            })
            .expect("the window is open");
        cx.run_until_parked();

        cx.dispatch_action(DismissDialog);
        window
            .update(&mut cx, |workspace, _window, cx| {
                assert!(workspace.context_menu.is_none(), "the shell menu stayed");
                assert!(
                    !query.read(cx).has_context_menu(),
                    "the pane's own menu was left behind the one the shell owns"
                );
                assert!(
                    workspace.about.read(cx).is_open(),
                    "the dialog under the menu went with it"
                );
            })
            .expect("the window is open");

        // And the next press finds the dialog, which is the whole point of the
        // menu going first rather than instead.
        cx.dispatch_action(DismissDialog);
        window
            .update(&mut cx, |workspace, _window, cx| {
                assert!(!workspace.about.read(cx).is_open());
            })
            .expect("the window is open");

        // Opening the application dropdown puts a context menu away, because
        // both of them lay a full-window backdrop.
        window
            .update(&mut cx, |workspace, _window, cx| {
                workspace.open_context_menu(
                    ContextTarget::Explorer(Box::new(NodeId::Connection(connection))),
                    gpui::point(px(30.), px(10.)),
                    cx,
                );
                workspace.set_menu_open(true, cx);
                assert!(workspace.context_menu.is_none());
                assert!(workspace.menu_open);
            })
            .expect("the window is open");
    }

    #[test]
    fn every_label_the_shell_menus_draw_has_a_translation() {
        for label in [
            ts!("context.close_tab"),
            ts!("context.close_others"),
            ts!("context.close_right"),
            ts!("context.split_right"),
            ts!("context.split_below"),
            ts!("context.close_pane"),
            ts!("context.connect"),
            ts!("context.edit"),
        ] {
            assert!(!label.is_empty(), "empty label");
            assert!(!label.starts_with("context."), "untranslated {label:?}");
        }
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

/// What the welcome screen's box does when its column outgrows the window.
///
/// Only [`centered_scroll`] is put under test, and only through what its scroll
/// handle reports: the arrangement is entirely a question of layout, and the
/// handle is where gpui writes down the answer — the box it measured, and how
/// far past it the column ran.
#[cfg(test)]
mod centered_scroll_tests {
    use std::ops::Deref;

    use gpui::{TestAppContext, VisualTestContext, point};

    use super::*;

    /// Height of the stand-in column.
    ///
    /// Nothing about the real welcome screen's contents matters here — only that
    /// there is a definite height to hold the window against — so the test hands
    /// the box one plain child rather than rebuilding the screen.
    const COLUMN: f32 = 400.;

    /// A window tall enough for the column and both its margins, several times
    /// over.
    const ROOMY: f32 = 900.;

    /// A window shorter than the column, which is the whole point of the box.
    const CRAMPED: f32 = 300.;

    /// Wide enough that nothing wraps; the box only scrolls one way.
    const WIDTH: f32 = 600.;

    /// How far apart two measurements may be and still count as the same, in a
    /// layout whose lengths are rounded to hundredths of a pixel.
    const SLACK: f32 = 0.5;

    /// A window holding nothing but the box under test.
    struct Harness {
        scroll: ScrollHandle,
        bar: ScrollbarState,
    }

    impl Render for Harness {
        fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
            let theme = Theme::dark();
            let bar = Scrollbar::for_handle(SCROLLBARS[1].0, Surface::Welcome.axis(), &self.scroll)
                .fade(self.bar.fade());

            div().flex().flex_col().size_full().child(centered_scroll(
                WELCOME_STATE,
                &self.scroll,
                bar,
                &theme,
                div().flex_none().w(px(320.)).h(px(COLUMN)),
            ))
        }
    }

    /// Opens the harness in a window `height` tall and hands back its handle.
    ///
    /// Drawn twice: a bar is built from the box as the previous frame measured
    /// it, so the opening frame has nothing to build one out of.
    fn open(cx: &mut TestAppContext, height: f32) -> ScrollHandle {
        let scroll = ScrollHandle::new();
        let window = cx.add_window({
            let scroll = scroll.clone();
            move |_, _| Harness {
                scroll,
                bar: ScrollbarState::new(),
            }
        });

        let mut cx = VisualTestContext::from_window(*window.deref(), cx);
        cx.simulate_resize(size(px(WIDTH), px(height)));
        cx.run_until_parked();
        cx.update(|window, _| window.refresh());
        cx.run_until_parked();

        scroll
    }

    /// The bar the workspace would draw over the box as it now stands.
    fn scrollbar(scroll: &ScrollHandle) -> Scrollbar {
        Scrollbar::for_handle(SCROLLBARS[1].0, Surface::Welcome.axis(), scroll)
    }

    /// With room to spare the column sits in the middle, exactly where
    /// `justify_center` used to put it, and there is nothing to scroll — so no
    /// bar is drawn either.
    #[gpui::test]
    fn a_column_that_fits_stays_in_the_middle(cx: &mut TestAppContext) {
        let scroll = open(cx, ROOMY);
        let box_ = scroll.bounds();
        let column = scroll
            .bounds_for_item(0)
            .expect("the box never measured its column");

        let above = f32::from(column.top() - box_.top());
        let below = f32::from(box_.bottom() - column.bottom());
        assert!(
            (above - below).abs() < SLACK,
            "the column was not centred: {above} above, {below} below"
        );
        assert_eq!(
            scroll.max_offset().height,
            px(0.),
            "a column that fits left something to scroll"
        );
        assert!(
            scrollbar(&scroll).thumb().is_none(),
            "a box with nothing to scroll drew a bar anyway"
        );
    }

    /// The regression: with less room than the column needs, the head of it used
    /// to be pushed off the top edge and left there. It now starts at the top of
    /// the box, and everything past the bottom is reachable by scrolling.
    #[gpui::test]
    fn a_column_that_does_not_fit_starts_at_the_top(cx: &mut TestAppContext) {
        let scroll = open(cx, CRAMPED);
        let box_ = scroll.bounds();
        let column = scroll
            .bounds_for_item(0)
            .expect("the box never measured its column");

        assert!(
            f32::from(column.top() - box_.top()).abs() < SLACK,
            "the column did not start at the top of the box: {:?} in {:?}",
            column,
            box_
        );
        assert!(
            (f32::from(scroll.max_offset().height)
                - f32::from(column.size.height - box_.size.height))
            .abs()
                < SLACK,
            "the scrollable range did not cover the whole of the column"
        );
        assert!(
            scrollbar(&scroll).thumb().is_some(),
            "a box with something to scroll drew no bar"
        );
    }

    /// And the far end of that scroll reaches the foot of the column, margin and
    /// all, rather than stopping short of the last button.
    #[gpui::test]
    fn scrolling_to_the_end_reaches_the_foot_of_the_column(cx: &mut TestAppContext) {
        let scroll = open(cx, CRAMPED);
        scroll.set_offset(point(px(0.), -scroll.max_offset().height));
        let box_ = scroll.bounds();
        let column = scroll
            .bounds_for_item(0)
            .expect("the box never measured its column");

        let foot = column.bottom() + scroll.offset().y;
        assert!(
            f32::from(foot - box_.bottom()).abs() < SLACK,
            "the end of the scroll left {:?} of the column below the box",
            foot - box_.bottom()
        );
        assert!(
            f32::from(column.size.height) > COLUMN + SCROLL_MARGIN,
            "the column was scrolled to its last button rather than past it"
        );
    }
}
