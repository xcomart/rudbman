//! The connection dialog: saved profiles on the left, one profile's form on the
//! right.
//!
//! Edits [`ConnectionStore`] and the keychain, and hands a profile to the shell
//! to open. The driver manager ([`crate::driver_manager`]) opens *instead of*
//! the form rather than over it, the way the theme editor does inside the
//! settings dialog — one tab ring on screen at a time.
//!
//! # The URL is assembled, not typed
//!
//! A [`DriverDef`] carries a `url_template` with `{host}`, `{port}` and one of
//! `{database}` / `{service}` / `{sid}` / `{file}` in it. The form breaks those
//! into fields and shows the assembled URL underneath, live, because the URL is
//! what actually reaches the driver and a user debugging a refused connection
//! needs to see it. A template hole the form has no field for survives into the
//! preview as `{name}`, which is how a hand-written `drivers.json` says it needs
//! something the editor cannot supply.
//!
//! The assembled URL can be overridden outright: several products accept URL
//! shapes no template covers — an embedded H2 file, an Oracle TNS alias, a
//! failover list — and refusing to let the user type one would make those
//! unreachable.
//!
//! # Secrets
//!
//! The password field is masked and its content goes to
//! [`SecretStore`](rudbman_core::SecretStore) on save, never to
//! `connections.json`. Deleting a profile deletes both of its keychain slots:
//! a password left behind under an id nothing references is a leak the user
//! cannot see, let alone clean up.
//!
//! # Nothing here blocks
//!
//! "Test connection" opens a real session, pings it and closes it — on a
//! background task, with the outcome delivered through `cx.spawn`. The window
//! stays live throughout, which matters most precisely when the connection is
//! the one that hangs.

use std::collections::{BTreeMap, HashMap};

use gpui::{
    App, Context, DragMoveEvent, Entity, EventEmitter, FocusHandle, Focusable, IntoElement,
    MouseButton, MouseDownEvent, MouseUpEvent, Pixels, Point, Render, ScrollHandle, SharedString,
    Subscription, Task, Window, div, prelude::*, px,
};
use rudbman_core::{
    ConnectionProfile, ConnectionStore, DriverDef, DriverStore, KeepAlive, SecretSlot, SecretStore,
    TunnelAuth, TunnelConfig,
};
use rudbman_ui::{
    Button, ButtonVariant, Checkbox, DraggedThumb, Scrollbar, ScrollbarAxis, ScrollbarState,
    Segmented, Select, TextInput, Theme, form_row, hide_later, hide_now, modal, scroll_to,
    scrolled, theme,
};
use uuid::Uuid;

use crate::app_settings;
use crate::connection::{
    self, CATALOGUE_PLACEHOLDERS, ConnectError, Credentials, placeholders_of, substitute,
};
use crate::driver_manager::{DriverManager, DriverManagerEvent};
use crate::i18n::ts;

/// Width of the dialog panel.
///
/// Wider than the settings dialog because this one carries two columns: the
/// profile list has to stay readable beside a form that is itself two columns
/// of label and control.
const DIALOG_WIDTH: f32 = 860.;

/// Width of the profile list column.
const LIST_WIDTH: f32 = 220.;

/// Height at which the form body starts scrolling.
const BODY_MAX_HEIGHT: f32 = 480.;

/// Prefix of the debug selector every saved-profile row carries, followed by
/// the profile's id.
///
/// The id and not the row's position: a test that seeds its own profiles is
/// then asserting about its own rows, and gpui never clears the debug bounds of
/// a frame it has drawn, so a selector two lists could share would answer with
/// whichever drew last.
///
/// Compiled away outside a test build; see [`profile_rows`].
pub(crate) const ROW_SELECTOR: &str = "profile-row:";

/// What a right-click on a saved-profile row is answered with.
///
/// Shared rather than generic because the one list that has no menu — the
/// dialog's own — passes `None`, and a `None` of an unnameable closure type is
/// something the caller cannot write.
pub(crate) type ProfileContextHandler =
    std::rc::Rc<dyn Fn(Uuid, Point<Pixels>, &mut Window, &mut App)>;

/// The dialog's two scrolling surfaces and the id of each one's overlay bar.
const SCROLLBARS: [(&str, Surface); 2] = [
    ("connect-list-scrollbar", Surface::List),
    ("connect-body-scrollbar", Surface::Body),
];

/// Which scrolling surface is meant.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Surface {
    /// The saved profile list.
    List,
    /// The form body.
    Body,
}

/// Colour tags a profile may be marked with.
///
/// Fixed rather than free-form: the tag's job is to make a production
/// connection recognisable at a glance in the tab strip, and a palette of six
/// does that better than a colour picker, which invites two profiles a shade
/// apart. `None` — the first swatch — is "no tag".
const COLORS: [&str; 6] = [
    "#e06c75", "#e5c07b", "#98c379", "#56b6c2", "#61afef", "#c678dd",
];

/// Tab order of the form.
mod tab {
    /// First index of the profile list; one per profile from there.
    pub const LIST: isize = 10;
    /// Ceiling of the list's range, so a long profile list cannot run into the
    /// form below it.
    pub const LIST_LIMIT: isize = 99;
    /// Profile name.
    pub const NAME: isize = 100;
    /// Folder.
    pub const FOLDER: isize = 105;
    /// First index of the colour swatches.
    pub const COLOR: isize = 110;
    /// Driver picker.
    pub const DRIVER: isize = 120;
    /// "Manage drivers…".
    pub const MANAGE_DRIVERS: isize = 121;
    /// First index of the URL part fields.
    pub const URL_PART: isize = 130;
    /// The assembled URL.
    pub const URL: isize = 140;
    /// User name.
    pub const USERNAME: isize = 150;
    /// Password.
    pub const PASSWORD: isize = 155;
    /// Driver property key/value rows.
    pub const PROPS: isize = 160;
    /// Read-only.
    pub const READ_ONLY: isize = 180;
    /// Auto-commit.
    pub const AUTO_COMMIT: isize = 181;
    /// Confirm writes.
    pub const CONFIRM_WRITES: isize = 182;
    /// Keep-alive toggle.
    pub const KEEP_ALIVE: isize = 190;
    /// Keep-alive interval.
    pub const KEEP_ALIVE_INTERVAL: isize = 191;
    /// Keep-alive query.
    pub const KEEP_ALIVE_QUERY: isize = 192;
    /// The tunnel section's disclosure.
    pub const TUNNEL: isize = 200;
    /// Bastion host.
    pub const TUNNEL_HOST: isize = 201;
    /// Bastion port.
    pub const TUNNEL_PORT: isize = 202;
    /// Bastion user.
    pub const TUNNEL_USER: isize = 203;
    /// Authentication method.
    pub const TUNNEL_AUTH: isize = 204;
    /// Private key path.
    pub const TUNNEL_KEY: isize = 205;
    /// Tunnel password or passphrase.
    pub const TUNNEL_SECRET: isize = 206;
    /// Target host, as named inside the remote network.
    pub const TUNNEL_REMOTE_HOST: isize = 207;
    /// Target port.
    pub const TUNNEL_REMOTE_PORT: isize = 208;
    /// New profile.
    pub const NEW: isize = 220;
    /// Duplicate profile.
    pub const DUPLICATE: isize = 221;
    /// Delete profile.
    pub const DELETE: isize = 222;
    /// Test connection.
    pub const TEST: isize = 230;
    /// Cancel.
    pub const CANCEL: isize = 240;
    /// Save.
    pub const SAVE: isize = 241;
    /// Connect.
    pub const CONNECT: isize = 242;
}

/// What the dialog tells the shell.
pub enum ConnectionDialogEvent {
    /// The user asked to open this profile. The shell connects and opens a tab;
    /// the dialog has already saved it.
    Connect(Box<ConnectionProfile>),
    /// The dialog was dismissed.
    Dismissed,
}

/// Severity of the message strip at the bottom of the form.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Level {
    /// A test succeeded, or a profile was saved.
    Ok,
    /// Something failed.
    Error,
    /// Something is in flight.
    Busy,
}

/// A bastion host key the user has been asked to rule on.
///
/// Never a *changed* key: [`ConnectError::HostKeyMismatch`] is what a possible
/// machine-in-the-middle looks like, and offering a one-click "trust it anyway"
/// for that would defeat the whole point of storing the key in the first place.
/// Editing `known_hosts` by hand is the deliberate act that case deserves.
struct PendingHostKey {
    /// The bastion, as the profile names it.
    host: String,
    /// Its SSH port.
    port: u16,
    /// The key algorithm, e.g. `ssh-ed25519`.
    algorithm: String,
    /// The OpenSSH-style SHA-256 fingerprint.
    fingerprint: String,
}

/// One driver property row, as the form holds it.
///
/// Two text fields rather than a map, because a key being typed passes through
/// states — empty, half-written, briefly equal to another row's — that a map
/// cannot represent without rows disappearing under the cursor.
struct PropRow {
    /// The property name.
    key: Entity<TextInput>,
    /// Its value.
    value: Entity<TextInput>,
}

/// The connection dialog.
pub struct ConnectionDialog {
    /// Whether the dialog is visible.
    open: bool,
    /// The profiles on disk, plus unsaved edits to the selected one.
    store: ConnectionStore,
    /// The drivers, re-read whenever the manager reports a change.
    drivers: DriverStore,
    /// Id of the profile being edited.
    selected: Option<Uuid>,
    /// Whether the driver dropdown is showing its list.
    driver_list_open: bool,
    /// Whether the tunnel section is expanded.
    tunnel_open: bool,
    /// Whether the profile has a tunnel at all.
    tunnel_enabled: bool,
    /// The tunnel's authentication method.
    tunnel_auth: TunnelAuth,
    /// Whether the profile has a keep-alive probe.
    keep_alive_enabled: bool,
    /// Whether the session is opened read-only.
    read_only: bool,
    /// Whether statements commit as they run.
    auto_commit: bool,
    /// Whether a write statement is confirmed first.
    confirm_writes: bool,
    /// The colour tag, or `None`.
    color: Option<SharedString>,
    /// Whether the URL field has been edited away from the assembled one.
    ///
    /// Once set, the part fields stop rewriting it: the user has said the
    /// template does not cover what they need, and having their URL overwritten
    /// by the next keystroke in a port field would be maddening.
    url_overridden: bool,
    /// Driver property rows.
    props: Vec<PropRow>,
    /// The driver manager, while one is open.
    manager: Option<Entity<DriverManager>>,
    /// Keeps the manager's subscription alive.
    manager_events: Option<Subscription>,
    /// Message strip under the form.
    status: Option<(Level, SharedString)>,
    /// A host key the last attempt refused, and the button to trust it.
    ///
    /// The verifier runs inside the SSH key exchange on the transport thread
    /// and cannot put a dialog up and wait, so an unknown key fails the attempt
    /// and the fingerprint arrives here instead — where a dialog *is* allowed to
    /// take its time (§9.3).
    pending_host_key: Option<PendingHostKey>,
    /// The test connection in flight, if any. Dropping it abandons the task.
    test: Option<Task<()>>,
    /// Whether the delete confirmation is showing.
    confirming: bool,
    /// Focus of the dialog root.
    focus_handle: FocusHandle,
    /// Whether focus should move into the form on the next render.
    pending_focus: bool,
    /// Scroll of the profile list.
    list_scroll: ScrollHandle,
    /// Whether the list's overlay bar is on screen.
    list_scrollbar: ScrollbarState,
    /// Scroll of the form body.
    body_scroll: ScrollHandle,
    /// Whether the body's overlay bar is on screen.
    body_scrollbar: ScrollbarState,
    /// Profile name.
    name_input: Entity<TextInput>,
    /// Folder the profile is grouped under.
    folder_input: Entity<TextInput>,
    /// One field per `{placeholder}` the driver's template uses.
    url_parts: HashMap<String, Entity<TextInput>>,
    /// The assembled URL, editable.
    url_input: Entity<TextInput>,
    /// Login user on the database.
    username_input: Entity<TextInput>,
    /// The database password, masked.
    password_input: Entity<TextInput>,
    /// Keep-alive interval in seconds.
    keep_alive_interval_input: Entity<TextInput>,
    /// Keep-alive probe statement.
    keep_alive_query_input: Entity<TextInput>,
    /// Bastion host.
    tunnel_host_input: Entity<TextInput>,
    /// Bastion SSH port.
    tunnel_port_input: Entity<TextInput>,
    /// Login user on the bastion.
    tunnel_user_input: Entity<TextInput>,
    /// Private key path, when the method is a key.
    tunnel_key_input: Entity<TextInput>,
    /// The bastion password or the key passphrase, masked.
    tunnel_secret_input: Entity<TextInput>,
    /// The database as named inside the remote network.
    tunnel_remote_host_input: Entity<TextInput>,
    /// The database's port there.
    tunnel_remote_port_input: Entity<TextInput>,
}

impl ConnectionDialog {
    /// Builds the dialog. Nothing is read from disk until it is opened.
    pub fn new(cx: &mut Context<Self>) -> Self {
        let field = |cx: &mut Context<Self>, placeholder: &str, index: isize| {
            let placeholder = SharedString::from(placeholder.to_owned());
            cx.new(move |cx| TextInput::new(cx).placeholder(placeholder).tab_index(index))
        };
        let secret = |cx: &mut Context<Self>, index: isize| {
            cx.new(move |cx| TextInput::new(cx).masked(true).tab_index(index))
        };

        // Every placeholder is a sample value — a host name, a port, a
        // statement — and reads the same in every language.
        Self {
            open: false,
            store: ConnectionStore::default(),
            drivers: DriverStore::default(),
            selected: None,
            driver_list_open: false,
            tunnel_open: false,
            tunnel_enabled: false,
            tunnel_auth: TunnelAuth::Agent,
            keep_alive_enabled: false,
            read_only: false,
            auto_commit: true,
            confirm_writes: true,
            color: None,
            url_overridden: false,
            props: Vec::new(),
            manager: None,
            manager_events: None,
            status: None,
            pending_host_key: None,
            test: None,
            confirming: false,
            focus_handle: cx.focus_handle(),
            pending_focus: false,
            list_scroll: ScrollHandle::new(),
            list_scrollbar: ScrollbarState::new(),
            body_scroll: ScrollHandle::new(),
            body_scrollbar: ScrollbarState::new(),
            name_input: field(cx, "staging", tab::NAME),
            folder_input: field(cx, "production", tab::FOLDER),
            url_parts: HashMap::new(),
            url_input: field(cx, "jdbc:postgresql://db:5432/app", tab::URL),
            username_input: field(cx, "app", tab::USERNAME),
            password_input: secret(cx, tab::PASSWORD),
            keep_alive_interval_input: field(cx, "300", tab::KEEP_ALIVE_INTERVAL),
            keep_alive_query_input: field(cx, "select 1", tab::KEEP_ALIVE_QUERY),
            tunnel_host_input: field(cx, "bastion.example.com", tab::TUNNEL_HOST),
            tunnel_port_input: field(cx, "22", tab::TUNNEL_PORT),
            tunnel_user_input: field(cx, "ops", tab::TUNNEL_USER),
            tunnel_key_input: field(cx, "~/.ssh/id_ed25519", tab::TUNNEL_KEY),
            tunnel_secret_input: secret(cx, tab::TUNNEL_SECRET),
            tunnel_remote_host_input: field(cx, "db.internal", tab::TUNNEL_REMOTE_HOST),
            tunnel_remote_port_input: field(cx, "5432", tab::TUNNEL_REMOTE_PORT),
        }
    }

    /// Shows the dialog with `id` selected and its form filled in.
    ///
    /// What the welcome screen's "edit…" row opens: that list offers one
    /// profile at a time and the dialog it leads to has to be showing that
    /// one, not whichever happens to be first. An id no saved profile answers
    /// to leaves [`ConnectionDialog::open`]'s own choice standing rather than
    /// opening a form over nothing — a profile can be deleted between the
    /// screen being drawn and the row being clicked.
    pub fn open_at(&mut self, id: Uuid, cx: &mut Context<Self>) {
        self.open_showing(Some(id), cx);
    }

    /// Shows the dialog, re-reading both stores.
    pub fn open(&mut self, cx: &mut Context<Self>) {
        self.open_showing(None, cx);
    }

    /// The whole of both, differing only in which profile the form opens over.
    fn open_showing(&mut self, wanted: Option<Uuid>, cx: &mut Context<Self>) {
        self.reload(cx);
        self.selected = profile_to_show(&self.store, wanted);
        if self.selected.is_none() {
            self.add_profile(cx);
        } else {
            self.fill_form(cx);
        }
        self.open = true;
        self.status = None;
        self.pending_host_key = None;
        self.confirming = false;
        self.manager = None;
        self.manager_events = None;
        self.pending_focus = true;
        cx.notify();
    }

    /// Whether the dialog is visible.
    pub fn is_open(&self) -> bool {
        self.open
    }

    /// Hides the dialog, abandoning any test in flight.
    pub fn close(&mut self, cx: &mut Context<Self>) {
        self.open = false;
        self.status = None;
        self.pending_host_key = None;
        self.confirming = false;
        self.manager = None;
        self.manager_events = None;
        self.driver_list_open = false;
        // Dropping the task detaches from the connection attempt; the session it
        // may still open is closed by its own `Drop` when the future is
        // cancelled at its next await point.
        self.test = None;
        cx.notify();
    }

    /// Re-reads `connections.json` and `drivers.json`.
    fn reload(&mut self, cx: &mut Context<Self>) {
        match ConnectionStore::load() {
            Ok(store) => self.store = store,
            Err(error) => {
                log::error!("could not read connections.json: {error:#}");
                self.status = Some((
                    Level::Error,
                    ts!("connect.load_failed", error = format!("{error:#}")),
                ));
            }
        }
        match DriverStore::load() {
            Ok(drivers) => self.drivers = drivers,
            Err(error) => {
                log::error!("could not read drivers.json: {error:#}");
                self.drivers = DriverStore::default();
            }
        }
        cx.notify();
    }

    /// The profile being edited, as it stands on disk.
    fn current(&self) -> Option<&ConnectionProfile> {
        self.selected.and_then(|id| self.store.get(id))
    }

    /// The driver the form currently names.
    fn current_driver(&self) -> Option<&DriverDef> {
        self.current()
            .and_then(|profile| self.drivers.get(&profile.driver_id))
    }

    /// Copies the selected profile into every control.
    fn fill_form(&mut self, cx: &mut Context<Self>) {
        let profile = self.current().cloned().unwrap_or_default();

        set_text(&self.name_input, profile.name.clone(), cx);
        set_text(
            &self.folder_input,
            profile.folder.clone().unwrap_or_default(),
            cx,
        );
        self.color = profile.color.clone().map(SharedString::from);
        self.read_only = profile.read_only;
        self.auto_commit = profile.auto_commit;
        self.confirm_writes = profile.confirm_writes;
        set_text(&self.url_input, profile.url.clone(), cx);
        set_text(&self.username_input, profile.username.clone(), cx);

        // The password is read from the keychain, not from the file, and only
        // when a saved profile is selected — a profile that has never been saved
        // has no keychain entry to read.
        let password = if self.store.get(profile.id).is_some() {
            SecretStore::get(profile.id, SecretSlot::Connection)
                .unwrap_or_default()
                .unwrap_or_default()
        } else {
            String::new()
        };
        set_text(&self.password_input, password, cx);

        self.keep_alive_enabled = profile.keep_alive.is_some();
        let keep_alive = profile.keep_alive.clone().unwrap_or_default();
        set_text(
            &self.keep_alive_interval_input,
            keep_alive.interval_s.to_string(),
            cx,
        );
        set_text(&self.keep_alive_query_input, keep_alive.query, cx);

        self.tunnel_enabled = profile.tunnel.is_some();
        self.tunnel_open = self.tunnel_enabled;
        let tunnel = profile.tunnel.clone().unwrap_or_default();
        self.tunnel_auth = tunnel.auth.clone();
        set_text(&self.tunnel_host_input, tunnel.host, cx);
        set_text(&self.tunnel_port_input, tunnel.port.to_string(), cx);
        set_text(&self.tunnel_user_input, tunnel.username, cx);
        set_text(
            &self.tunnel_key_input,
            match &tunnel.auth {
                TunnelAuth::Key { path } => path.display().to_string(),
                _ => String::new(),
            },
            cx,
        );
        let tunnel_secret = if self.store.get(profile.id).is_some() {
            SecretStore::get(profile.id, SecretSlot::Tunnel)
                .unwrap_or_default()
                .unwrap_or_default()
        } else {
            String::new()
        };
        set_text(&self.tunnel_secret_input, tunnel_secret, cx);
        set_text(&self.tunnel_remote_host_input, tunnel.remote_host, cx);
        set_text(
            &self.tunnel_remote_port_input,
            if tunnel.remote_port == 0 {
                String::new()
            } else {
                tunnel.remote_port.to_string()
            },
            cx,
        );

        self.rebuild_props(&profile.props, cx);
        self.rebuild_url_parts(cx);
        // A stored URL that the template cannot reproduce is an override, and
        // saying so up front is what stops the first keystroke in a port field
        // from throwing it away.
        self.url_overridden = !profile.url.is_empty() && profile.url != self.assembled_url(cx);
        cx.notify();
    }

    /// Rebuilds the property rows from `props`, plus one blank row to type into.
    fn rebuild_props(&mut self, props: &BTreeMap<String, String>, cx: &mut Context<Self>) {
        self.props.clear();
        for (index, (key, value)) in props.iter().enumerate() {
            let row = self.new_prop_row(index, cx);
            set_text(&row.key, key.clone(), cx);
            set_text(&row.value, value.clone(), cx);
            self.props.push(row);
        }
        let blank = self.new_prop_row(self.props.len(), cx);
        self.props.push(blank);
    }

    /// One key/value pair of driver properties.
    fn new_prop_row(&self, index: usize, cx: &mut Context<Self>) -> PropRow {
        let base = tab::PROPS + (index as isize) * 2;
        PropRow {
            key: cx.new(move |cx| {
                TextInput::new(cx)
                    .placeholder(ts!("connect.prop_key"))
                    .tab_index(base)
            }),
            value: cx.new(move |cx| {
                TextInput::new(cx)
                    .placeholder(ts!("connect.prop_value"))
                    .tab_index(base + 1)
            }),
        }
    }

    /// Rebuilds the URL part fields to match the selected driver's template.
    ///
    /// Keeps whatever the matching field already held, so switching from
    /// PostgreSQL to MySQL does not lose the host that both of them need.
    fn rebuild_url_parts(&mut self, cx: &mut Context<Self>) {
        let template = self
            .current_driver()
            .map(|driver| driver.url_template.clone())
            .unwrap_or_default();
        let default_port = self.current_driver().and_then(|driver| driver.default_port);
        let names = placeholders_of(&template);

        let mut parts = HashMap::new();
        for (index, name) in names.iter().enumerate() {
            if let Some(existing) = self.url_parts.remove(name) {
                parts.insert(name.clone(), existing);
                continue;
            }
            let placeholder = placeholder_for(name);
            let tab_index = tab::URL_PART + index as isize;
            let input = cx.new(move |cx| {
                TextInput::new(cx)
                    .placeholder(placeholder)
                    .tab_index(tab_index)
            });
            // A driver's own default port is a better starting point than an
            // empty field: it is right nearly every time and it says what the
            // product listens on.
            if name == "port"
                && let Some(port) = default_port
            {
                set_text(&input, port.to_string(), cx);
            }
            parts.insert(name.clone(), input);
        }
        self.url_parts = parts;

        // A saved URL is picked apart back into the fields where it can be, so
        // that opening a profile shows a host in the host field rather than an
        // empty form beside a full URL.
        let url = text(&self.url_input, cx);
        if !url.is_empty()
            && let Some(values) = decompose(&template, &url)
        {
            for (name, value) in values {
                if let Some(input) = self.url_parts.get(&name) {
                    set_text(input, value, cx);
                }
            }
        }
    }

    /// The URL the part fields assemble, without touching the URL field.
    fn assembled_url(&self, cx: &App) -> String {
        let Some(driver) = self.current_driver() else {
            return String::new();
        };
        let values: HashMap<String, String> = self
            .url_parts
            .iter()
            .map(|(name, input)| (name.clone(), text(input, cx)))
            .collect();
        substitute(&driver.url_template, &values)
    }

    /// Writes the assembled URL into the URL field, unless it was overridden.
    fn refresh_url(&mut self, cx: &mut Context<Self>) {
        if self.url_overridden {
            return;
        }
        let url = self.assembled_url(cx);
        set_text(&self.url_input, url, cx);
        cx.notify();
    }

    /// Reads the form back into a profile.
    ///
    /// Starts from the stored profile so that anything the form does not edit —
    /// nothing today, but that will not stay true — survives the trip.
    fn collect(&self, cx: &App) -> ConnectionProfile {
        let mut profile = self.current().cloned().unwrap_or_default();
        profile.name = text(&self.name_input, cx);
        profile.folder = Some(text(&self.folder_input, cx)).filter(|text| !text.is_empty());
        profile.color = self.color.as_ref().map(ToString::to_string);
        profile.url = text(&self.url_input, cx);
        profile.username = text(&self.username_input, cx);
        profile.read_only = self.read_only;
        profile.auto_commit = self.auto_commit;
        profile.confirm_writes = self.confirm_writes;

        profile.props = self
            .props
            .iter()
            .map(|row| (text(&row.key, cx), text(&row.value, cx)))
            // A row whose key is blank is the empty row at the end, or one the
            // user cleared to remove it.
            .filter(|(key, _)| !key.is_empty())
            .collect();

        profile.keep_alive = self.keep_alive_enabled.then(|| KeepAlive {
            interval_s: parse_or(&self.keep_alive_interval_input, 300, cx),
            query: {
                let query = text(&self.keep_alive_query_input, cx);
                if query.is_empty() {
                    "select 1".to_string()
                } else {
                    query
                }
            },
        });

        profile.tunnel = self.tunnel_enabled.then(|| TunnelConfig {
            host: text(&self.tunnel_host_input, cx),
            port: parse_or(&self.tunnel_port_input, 22, cx),
            username: text(&self.tunnel_user_input, cx),
            auth: match self.tunnel_auth {
                TunnelAuth::Key { .. } => TunnelAuth::Key {
                    path: text(&self.tunnel_key_input, cx).into(),
                },
                TunnelAuth::Password => TunnelAuth::Password,
                TunnelAuth::Agent => TunnelAuth::Agent,
            },
            remote_host: text(&self.tunnel_remote_host_input, cx),
            remote_port: parse_or(&self.tunnel_remote_port_input, 0, cx),
            // Always the OS's choice: two profiles that pin the same port cannot
            // be open at once, and nothing downstream needs to know which port
            // it was — the bound one is substituted into the URL.
            local_port: 0,
        });

        profile
    }

    /// Writes the form to `connections.json` and the keychain.
    ///
    /// Returns the profile that was saved, or `None` when something refused.
    fn persist(&mut self, cx: &mut Context<Self>) -> Option<ConnectionProfile> {
        let profile = self.collect(cx);
        if profile.name.trim().is_empty() {
            self.report(Level::Error, ts!("connect.name_required"), cx);
            return None;
        }
        if profile.url.trim().is_empty() {
            self.report(Level::Error, ts!("connect.url_required"), cx);
            return None;
        }

        self.store.upsert(profile.clone());
        if let Err(error) = self.store.save() {
            log::error!("could not write connections.json: {error:#}");
            self.report(
                Level::Error,
                ts!("connect.save_failed", error = format!("{error:#}")),
                cx,
            );
            return None;
        }

        // The two secrets, each written or cleared to match the form. A blank
        // field means "no stored secret", which is a deletion rather than an
        // empty password — an empty entry in the keychain would be handed to the
        // driver as a login attempt.
        self.write_secret(
            profile.id,
            SecretSlot::Connection,
            &text(&self.password_input, cx),
            cx,
        );
        if self.tunnel_enabled {
            self.write_secret(
                profile.id,
                SecretSlot::Tunnel,
                &text(&self.tunnel_secret_input, cx),
                cx,
            );
        }

        self.selected = Some(profile.id);
        Some(profile)
    }

    /// Saves or clears one keychain slot, reporting a refusal.
    fn write_secret(&mut self, id: Uuid, slot: SecretSlot, secret: &str, cx: &mut Context<Self>) {
        let outcome = if secret.is_empty() {
            SecretStore::delete(id, slot)
        } else {
            SecretStore::set(id, slot, secret)
        };
        if let Err(error) = outcome {
            log::warn!("could not write the {slot} secret: {error:#}");
            self.report(
                Level::Error,
                ts!("connect.keychain_failed", error = format!("{error:#}")),
                cx,
            );
        }
    }

    /// Saves and reports it.
    fn save(&mut self, cx: &mut Context<Self>) {
        if self.persist(cx).is_some()
            && self.status.as_ref().map(|(level, _)| *level) != Some(Level::Error)
        {
            self.report(Level::Ok, ts!("connect.saved"), cx);
        }
        cx.notify();
    }

    /// Saves, then asks the shell to open the profile.
    fn connect(&mut self, cx: &mut Context<Self>) {
        let Some(profile) = self.persist(cx) else {
            return;
        };
        if self.status.as_ref().map(|(level, _)| *level) == Some(Level::Error) {
            return;
        }
        cx.emit(ConnectionDialogEvent::Connect(Box::new(profile)));
        self.close(cx);
    }

    /// Opens a session with what the form currently holds, pings it, closes it.
    ///
    /// Deliberately does **not** save first: the point of the button is to find
    /// out whether an edit works before committing to it. The secrets therefore
    /// come from the fields rather than from the keychain.
    fn test_connection(&mut self, cx: &mut Context<Self>) {
        let profile = self.collect(cx);
        let Some(driver) = self.drivers.get(&profile.driver_id).cloned() else {
            self.report(
                Level::Error,
                ts!("connect.no_driver", driver = profile.driver_id.clone()),
                cx,
            );
            return;
        };
        let credentials = Credentials::typed(
            Some(text(&self.password_input, cx)),
            profile
                .tunnel
                .as_ref()
                .map(|_| text(&self.tunnel_secret_input, cx)),
        );
        let settings = app_settings::current(cx);

        self.pending_host_key = None;
        self.report(Level::Busy, ts!("connect.testing"), cx);
        let attempt = cx.background_spawn(async move {
            let connected = connection::connect(&profile, &driver, &credentials, &settings)?;
            let product = connected.product();
            // A session that opened but cannot answer a PING is not a working
            // connection, and finding that out here is the whole point of a
            // test that does more than call `getConnection`.
            let ping = connected.session().ping();
            let closed = connected.close();
            ping.map_err(ConnectError::from)?;
            closed.map_err(ConnectError::from)?;
            Ok::<_, ConnectError>(product)
        });

        self.test = Some(cx.spawn(async move |dialog, cx| {
            let outcome = attempt.await;
            dialog
                .update(cx, |dialog, cx| dialog.tested(outcome, cx))
                .ok();
        }));
    }

    /// Reports what a test produced.
    fn tested(&mut self, outcome: Result<Option<String>, ConnectError>, cx: &mut Context<Self>) {
        self.test = None;
        match outcome {
            Ok(Some(product)) => {
                self.report(Level::Ok, ts!("connect.test_ok", product = product), cx)
            }
            // A driver that answers nothing useful to `DatabaseMetaData` still
            // connected, and saying so is more honest than inventing a name.
            Ok(None) => self.report(Level::Ok, ts!("connect.test_ok_unnamed"), cx),
            Err(error) => {
                // An unknown key is the one failure the user can clear from
                // here; a changed one deliberately gets no button.
                self.pending_host_key = match &error {
                    ConnectError::HostKeyUnknown {
                        host,
                        fingerprint,
                        algorithm,
                    } => Some(PendingHostKey {
                        host: host.clone(),
                        port: self.collect(cx).tunnel.map_or(22, |tunnel| tunnel.port),
                        algorithm: algorithm.clone(),
                        fingerprint: fingerprint.clone(),
                    }),
                    _ => None,
                };
                let message = error.message();
                let hint = error_hint(&error);
                self.report(
                    Level::Error,
                    match hint {
                        Some(hint) => ts!("connect.test_failed_hint", error = message, hint = hint),
                        None => ts!("connect.test_failed", error = message),
                    },
                    cx,
                );
            }
        }
    }

    /// Puts a message under the form.
    fn report(&mut self, level: Level, message: SharedString, cx: &mut Context<Self>) {
        self.status = Some((level, message));
        cx.notify();
    }

    /// Selects `id`, keeping the edits to the profile being left.
    fn select(&mut self, id: Uuid, cx: &mut Context<Self>) {
        if self.selected == Some(id) {
            return;
        }
        // Edits are kept in memory so that flicking between two profiles does
        // not discard a half-typed one; only `save` writes the file.
        let edited = self.collect(cx);
        if !edited.name.trim().is_empty() {
            self.store.upsert(edited);
        }
        self.selected = Some(id);
        self.confirming = false;
        self.status = None;
        self.fill_form(cx);
    }

    /// Adds a blank profile and selects it.
    fn add_profile(&mut self, cx: &mut Context<Self>) {
        let settings = app_settings::current(cx);
        let driver_id = self
            .drivers
            .drivers()
            .first()
            .map(|driver| driver.id.clone())
            .unwrap_or_default();
        let mut profile = ConnectionProfile::new(
            ts!("connect.new_name").to_string(),
            driver_id,
            String::new(),
            String::new(),
        );
        profile.confirm_writes = settings.confirm_writes_default;
        let id = profile.id;
        self.store.upsert(profile);
        self.selected = Some(id);
        self.confirming = false;
        self.status = None;
        self.fill_form(cx);
        self.refresh_url(cx);
        self.pending_focus = true;
        cx.notify();
    }

    /// Copies the selected profile under a new id and selects the copy.
    ///
    /// The password is *not* copied: it lives in the keychain under the original
    /// id, and duplicating a credential without being asked to is not something
    /// a connection editor should do quietly.
    fn duplicate_profile(&mut self, cx: &mut Context<Self>) {
        let mut profile = self.collect(cx);
        profile.id = Uuid::new_v4();
        profile.name = ts!("connect.copy_name", name = profile.name).to_string();
        let id = profile.id;
        self.store.upsert(profile);
        self.selected = Some(id);
        self.confirming = false;
        self.status = None;
        self.fill_form(cx);
        cx.notify();
    }

    /// Removes the selected profile and both of its keychain entries.
    fn delete_profile(&mut self, cx: &mut Context<Self>) {
        self.confirming = false;
        let Some(id) = self.selected else {
            return;
        };
        self.store.remove(id);
        if let Err(error) = SecretStore::delete_all(id) {
            log::warn!("could not remove the secrets of {id}: {error:#}");
        }
        if let Err(error) = self.store.save() {
            log::error!("could not write connections.json: {error:#}");
            self.report(
                Level::Error,
                ts!("connect.save_failed", error = format!("{error:#}")),
                cx,
            );
        }
        self.selected = self.store.connections().first().map(|profile| profile.id);
        if self.selected.is_none() {
            self.add_profile(cx);
        } else {
            self.fill_form(cx);
        }
        cx.notify();
    }

    /// Puts the driver manager in front of the form.
    fn open_driver_manager(&mut self, cx: &mut Context<Self>) {
        let manager = cx.new(DriverManager::new);
        self.manager_events = Some(cx.subscribe(&manager, |dialog, _manager, event, cx| {
            match event {
                DriverManagerEvent::Changed => {
                    // The picker and the URL parts both follow `drivers.json`,
                    // so a driver gaining a JAR — or a template changing — has
                    // to reach the form while it is still open.
                    if let Ok(drivers) = DriverStore::load() {
                        dialog.drivers = drivers;
                    }
                    dialog.rebuild_url_parts(cx);
                    dialog.refresh_url(cx);
                    cx.notify();
                }
                DriverManagerEvent::Dismissed => {
                    dialog.manager = None;
                    dialog.manager_events = None;
                    dialog.pending_focus = true;
                    cx.notify();
                }
            }
        }));
        self.manager = Some(manager);
        self.driver_list_open = false;
        cx.notify();
    }

    /// `Escape`, one layer at a time.
    ///
    /// Public because the key never actually arrives here: gpui matches key
    /// bindings ahead of key events, so the shell's binding wins and calls this.
    pub fn escape(&mut self, cx: &mut Context<Self>) {
        if let Some(manager) = self.manager.clone() {
            manager.update(cx, |manager, cx| manager.escape(cx));
            return;
        }
        if self.driver_list_open {
            self.driver_list_open = false;
            cx.notify();
            return;
        }
        if self.confirming {
            self.confirming = false;
            cx.notify();
            return;
        }
        cx.emit(ConnectionDialogEvent::Dismissed);
        self.close(cx);
    }

    /// Moves focus into the name field when the dialog opens.
    fn apply_pending_focus(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.pending_focus || self.manager.is_some() {
            return;
        }
        self.pending_focus = false;
        let handle = self.name_input.read(cx).focus_handle(cx);
        window.focus(&handle);
    }

    /// The handle and bar state of one scrolling surface.
    fn surface(&mut self, surface: Surface) -> (&ScrollHandle, &mut ScrollbarState) {
        match surface {
            Surface::List => (&self.list_scroll, &mut self.list_scrollbar),
            Surface::Body => (&self.body_scroll, &mut self.body_scrollbar),
        }
    }

    /// The overlay bar of one surface, as it stands.
    fn scrollbar(&self, id: &'static str, surface: Surface) -> Scrollbar {
        let (handle, state) = match surface {
            Surface::List => (&self.list_scroll, &self.list_scrollbar),
            Surface::Body => (&self.body_scroll, &self.body_scrollbar),
        };
        Scrollbar::for_handle(id, ScrollbarAxis::Vertical, handle).fade(state.fade())
    }

    /// The same bar, listening for the pointer reaching the edge it rides.
    ///
    /// Only the bars that are drawn need it: the one the drag path builds is
    /// there to be measured, and never reaches an element tree.
    fn hovering_scrollbar(
        &self,
        id: &'static str,
        surface: Surface,
        cx: &mut Context<Self>,
    ) -> Scrollbar {
        self.scrollbar(id, surface).on_hover(cx.listener(
            move |dialog, hovered: &bool, _window, cx| {
                dialog.hover_scrollbar(surface, *hovered, cx);
            },
        ))
    }

    /// Puts each bar up whenever its surface has moved.
    fn watch_scroll(&mut self, cx: &mut Context<Self>) {
        for (_, surface) in SCROLLBARS {
            let (handle, state) = self.surface(surface);
            let moved = scrolled(handle, ScrollbarAxis::Vertical);
            if let Some(epoch) = state.moved(moved) {
                hide_later(epoch, cx, move |dialog: &mut Self| {
                    Some(dialog.surface(surface).1)
                });
            }
        }
    }

    /// Scrolls whichever surface's thumb is being dragged.
    fn drag_scrollbar(&mut self, event: &DragMoveEvent<DraggedThumb>, cx: &mut Context<Self>) {
        for (id, surface) in SCROLLBARS {
            let Some(progress) = self.scrollbar(id, surface).dragged(event, cx) else {
                continue;
            };
            let (handle, state) = self.surface(surface);
            state.hold();
            let handle = handle.clone();
            scroll_to(&handle, ScrollbarAxis::Vertical, progress);
            cx.notify();
            return;
        }
    }

    /// Lets go of whichever thumb was held.
    fn release_scrollbars(&mut self, cx: &mut Context<Self>) {
        for (_, surface) in SCROLLBARS {
            if let Some(epoch) = self.surface(surface).1.release() {
                hide_later(epoch, cx, move |dialog: &mut Self| {
                    Some(dialog.surface(surface).1)
                });
                cx.notify();
            }
        }
    }

    /// Puts one surface's bar up while the pointer rests on the edge it rides,
    /// and starts it going the moment the pointer leaves.
    ///
    /// Told which surface rather than asked to work it out: each strip knows
    /// only its own.
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
        hide_now(self, epoch, cx, move |dialog: &mut Self| {
            Some(dialog.surface(surface).1)
        });
    }

    /// The saved profile list, grouped by folder.
    fn render_list(&self, chrome: &Theme, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let this = cx.entity();
        let rows = profile_rows(
            self.store.connections(),
            self.selected,
            chrome,
            move |id, _window, cx| {
                this.update(cx, |dialog, cx| dialog.select(id, cx));
            },
            // No menu here: the form beside the list is already open on the
            // selected profile, so every command a menu would carry is a
            // control the user can see.
            None,
        );

        div()
            .relative()
            .flex()
            .flex_col()
            .flex_none()
            .w(px(LIST_WIDTH))
            .min_h_0()
            .child(
                div()
                    .id("connect-list")
                    .track_scroll(&self.list_scroll)
                    .flex()
                    .flex_col()
                    .gap(px(1.))
                    .min_h_0()
                    .max_h(px(BODY_MAX_HEIGHT))
                    .overflow_y_scroll()
                    .children(rows),
            )
            .children(
                self.hovering_scrollbar(SCROLLBARS[0].0, Surface::List, cx)
                    .render(chrome),
            )
    }

    /// The colour tag swatches.
    fn render_colors(&self, chrome: &Theme, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let this = cx.entity();
        let swatch = |index: usize, value: Option<&'static str>| {
            let selected = self.color.as_ref().map(SharedString::as_ref) == value;
            let fill = value
                .and_then(rudbman_ui::parse_hex)
                .unwrap_or(chrome.surface_active);
            let this = this.clone();
            div()
                .id(("connect-color", index))
                .flex_none()
                .size(px(18.))
                .rounded_full()
                .cursor_pointer()
                .bg(fill)
                .border_2()
                .border_color(if selected {
                    chrome.accent
                } else {
                    chrome.border
                })
                .tab_index(tab::COLOR + index as isize)
                .on_click(move |_, _window, cx| {
                    this.update(cx, |dialog, cx| {
                        dialog.color = value.map(SharedString::from);
                        cx.notify();
                    });
                })
        };

        div()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(6.))
            .child(swatch(0, None))
            .children(
                COLORS
                    .iter()
                    .enumerate()
                    .map(|(index, value)| swatch(index + 1, Some(value))),
            )
    }

    /// The driver picker and the button that opens the driver manager.
    fn render_driver(&self, chrome: &Theme, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let this = cx.entity();
        let options: Vec<SharedString> = self
            .drivers
            .drivers()
            .iter()
            .map(|driver| SharedString::from(driver.name.clone()))
            .collect();
        let selected = self
            .current_driver()
            .map(|driver| SharedString::from(driver.name.clone()));
        let ids: Vec<String> = self
            .drivers
            .drivers()
            .iter()
            .map(|driver| driver.id.clone())
            .collect();

        // A driver with no JAR cannot open anything; saying so beside the picker
        // is what turns "no suitable driver" into a sentence with a next step.
        let warning = self
            .current_driver()
            .filter(|driver| driver.jars.is_empty())
            .map(|_| {
                div()
                    .text_size(px(11.))
                    .text_color(chrome.danger)
                    .child(ts!("connect.driver_has_no_jar"))
            });

        let select = Select::new("connect-driver")
            .options(options)
            .selected(selected)
            .placeholder(ts!("connect.pick_driver"))
            .open(self.driver_list_open)
            .tab_index(tab::DRIVER)
            .on_select({
                let this = this.clone();
                // By index: two drivers may legitimately share a display name,
                // and the id is what a profile stores.
                move |index, _label, _window, cx| {
                    let Some(id) = ids.get(index).cloned() else {
                        return;
                    };
                    this.update(cx, |dialog, cx| dialog.set_driver(id, cx));
                }
            })
            .on_open_change({
                let this = this.clone();
                move |open, _window, cx| {
                    this.update(cx, |dialog, cx| {
                        dialog.driver_list_open = open;
                        cx.notify();
                    });
                }
            });

        div()
            .flex()
            .flex_col()
            .gap(px(6.))
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(8.))
                    .child(div().flex_1().min_w_0().child(select))
                    .child(
                        Button::new("connect-manage-drivers", ts!("connect.manage_drivers"))
                            .variant(ButtonVariant::Secondary)
                            .tab_index(tab::MANAGE_DRIVERS)
                            .on_click(move |_, _window, cx| {
                                this.update(cx, |dialog, cx| dialog.open_driver_manager(cx));
                            }),
                    ),
            )
            .children(warning)
    }

    /// Points the profile at another driver and re-derives the URL fields.
    fn set_driver(&mut self, id: String, cx: &mut Context<Self>) {
        let Some(selected) = self.selected else {
            return;
        };
        let Some(mut profile) = self.store.get(selected).cloned() else {
            return;
        };
        if profile.driver_id == id {
            return;
        }
        profile.driver_id = id;
        self.store.upsert(profile);
        self.driver_list_open = false;
        self.rebuild_url_parts(cx);
        // The template changed, so an override that was written for the old
        // driver's grammar is no longer an override of *this* template.
        self.url_overridden = false;
        self.refresh_url(cx);
        cx.notify();
    }

    /// One field per hole in the driver's URL template, plus the URL itself.
    fn render_url(&self, chrome: &Theme, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let this = cx.entity();
        let template = self
            .current_driver()
            .map(|driver| driver.url_template.clone())
            .unwrap_or_default();

        let parts: Vec<_> = placeholders_of(&template)
            .into_iter()
            .filter_map(|name| {
                let input = self.url_parts.get(&name)?.clone();
                Some(form_row(label_for(&name), input))
            })
            .collect();

        let overridden = self.url_overridden;
        let reset = overridden.then(|| {
            let this = this.clone();
            Button::new("connect-url-reset", ts!("connect.url_reset"))
                .variant(ButtonVariant::Secondary)
                .on_click(move |_, _window, cx| {
                    this.update(cx, |dialog, cx| {
                        dialog.url_overridden = false;
                        dialog.refresh_url(cx);
                    });
                })
        });

        div()
            .flex()
            .flex_col()
            .gap(px(8.))
            .children(parts)
            .child(form_row(
                ts!("connect.url"),
                div()
                    .flex()
                    .flex_col()
                    .gap(px(4.))
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap(px(8.))
                            .child(div().flex_1().min_w_0().child(self.url_input.clone()))
                            .children(reset),
                    )
                    .child(
                        div()
                            .text_size(px(11.))
                            .text_color(chrome.text_muted)
                            .child(if overridden {
                                ts!("connect.url_overridden")
                            } else {
                                ts!("connect.url_hint")
                            }),
                    ),
            ))
    }

    /// The driver property rows.
    fn render_props(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let rows: Vec<_> = self
            .props
            .iter()
            .enumerate()
            .map(|(index, row)| {
                div()
                    .id(("connect-prop", index))
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(6.))
                    .child(div().flex_1().min_w_0().child(row.key.clone()))
                    .child(div().flex_1().min_w_0().child(row.value.clone()))
            })
            .collect();
        let this = cx.entity();

        div().flex().flex_col().gap(px(6.)).children(rows).child(
            Button::new("connect-add-prop", ts!("connect.add_prop"))
                .variant(ButtonVariant::Secondary)
                .on_click(move |_, _window, cx| {
                    this.update(cx, |dialog, cx| {
                        let row = dialog.new_prop_row(dialog.props.len(), cx);
                        dialog.props.push(row);
                        cx.notify();
                    });
                }),
        )
    }

    /// The tunnel section: a switch, and the settings behind it.
    fn render_tunnel(&self, chrome: &Theme, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let this = cx.entity();
        let enabled = self.tunnel_enabled;

        let toggle = Checkbox::new("connect-tunnel", ts!("connect.tunnel_enabled"))
            .checked(enabled)
            .tab_index(tab::TUNNEL)
            .on_toggle({
                let this = this.clone();
                move |checked, _window, cx| {
                    this.update(cx, |dialog, cx| {
                        dialog.tunnel_enabled = checked;
                        dialog.tunnel_open = checked;
                        cx.notify();
                    });
                }
            });

        // Folded away when off, per §9.2: a profile without a bastion should not
        // be asked eight questions about one.
        let body = (enabled && self.tunnel_open).then(|| {
            let auth = Segmented::new("connect-tunnel-auth")
                .options([
                    ("agent", ts!("connect.tunnel_auth_agent")),
                    ("key", ts!("connect.tunnel_auth_key")),
                    ("password", ts!("connect.tunnel_auth_password")),
                ])
                .selected(match self.tunnel_auth {
                    TunnelAuth::Agent => 0,
                    TunnelAuth::Key { .. } => 1,
                    TunnelAuth::Password => 2,
                })
                .tab_index(tab::TUNNEL_AUTH)
                .on_select({
                    let this = this.clone();
                    move |index, _window, cx| {
                        this.update(cx, |dialog, cx| {
                            dialog.tunnel_auth = match index {
                                1 => TunnelAuth::Key {
                                    path: Default::default(),
                                },
                                2 => TunnelAuth::Password,
                                _ => TunnelAuth::Agent,
                            };
                            cx.notify();
                        });
                    }
                });

            let key_row = matches!(self.tunnel_auth, TunnelAuth::Key { .. })
                .then(|| form_row(ts!("connect.tunnel_key"), self.tunnel_key_input.clone()));
            // The agent holds its own credentials; asking for one would suggest
            // rudbman does something with it.
            let secret_row = (!matches!(self.tunnel_auth, TunnelAuth::Agent)).then(|| {
                form_row(
                    match self.tunnel_auth {
                        TunnelAuth::Key { .. } => ts!("connect.tunnel_passphrase"),
                        _ => ts!("connect.tunnel_password"),
                    },
                    self.tunnel_secret_input.clone(),
                )
            });

            div()
                .flex()
                .flex_col()
                .gap(px(8.))
                .child(form_row(
                    ts!("connect.tunnel_host"),
                    div()
                        .flex()
                        .flex_row()
                        .gap(px(8.))
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .child(self.tunnel_host_input.clone()),
                        )
                        .child(
                            div()
                                .flex_none()
                                .w(px(72.))
                                .child(self.tunnel_port_input.clone()),
                        ),
                ))
                .child(form_row(
                    ts!("connect.tunnel_user"),
                    self.tunnel_user_input.clone(),
                ))
                .child(form_row(ts!("connect.tunnel_auth"), auth))
                .children(key_row)
                .children(secret_row)
                .child(form_row(
                    ts!("connect.tunnel_remote"),
                    div()
                        .flex()
                        .flex_row()
                        .gap(px(8.))
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .child(self.tunnel_remote_host_input.clone()),
                        )
                        .child(
                            div()
                                .flex_none()
                                .w(px(72.))
                                .child(self.tunnel_remote_port_input.clone()),
                        ),
                ))
                .child(
                    div()
                        .text_size(px(11.))
                        .text_color(chrome.text_muted)
                        .child(ts!("connect.tunnel_hint")),
                )
        });

        div()
            .flex()
            .flex_col()
            .gap(px(8.))
            .child(toggle)
            .children(body)
    }

    /// The behaviour switches and the keep-alive settings.
    fn render_behaviour(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let this = cx.entity();
        let toggle = |id: &'static str,
                      label: SharedString,
                      checked: bool,
                      index: isize,
                      set: fn(&mut Self, bool)| {
            let this = this.clone();
            Checkbox::new(id, label)
                .checked(checked)
                .tab_index(index)
                .on_toggle(move |checked, _window, cx| {
                    this.update(cx, |dialog, cx| {
                        set(dialog, checked);
                        cx.notify();
                    });
                })
        };

        let keep_alive_body = self.keep_alive_enabled.then(|| {
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap(px(8.))
                .child(
                    div()
                        .flex_none()
                        .w(px(72.))
                        .child(self.keep_alive_interval_input.clone()),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .child(self.keep_alive_query_input.clone()),
                )
        });

        div()
            .flex()
            .flex_col()
            .gap(px(8.))
            .child(toggle(
                "connect-read-only",
                ts!("connect.read_only"),
                self.read_only,
                tab::READ_ONLY,
                |dialog, value| dialog.read_only = value,
            ))
            .child(toggle(
                "connect-auto-commit",
                ts!("connect.auto_commit"),
                self.auto_commit,
                tab::AUTO_COMMIT,
                |dialog, value| dialog.auto_commit = value,
            ))
            .child(toggle(
                "connect-confirm-writes",
                ts!("connect.confirm_writes"),
                self.confirm_writes,
                tab::CONFIRM_WRITES,
                |dialog, value| dialog.confirm_writes = value,
            ))
            .child(toggle(
                "connect-keep-alive",
                ts!("connect.keep_alive"),
                self.keep_alive_enabled,
                tab::KEEP_ALIVE,
                |dialog, value| dialog.keep_alive_enabled = value,
            ))
            .children(keep_alive_body)
    }

    /// The scrolling form for the selected profile.
    fn render_form(&self, chrome: &Theme, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let colors = self.render_colors(chrome, cx);
        let driver = self.render_driver(chrome, cx);
        let url = self.render_url(chrome, cx);
        let props = self.render_props(cx);
        let behaviour = self.render_behaviour(cx);
        let tunnel = self.render_tunnel(chrome, cx);

        div()
            .relative()
            .flex()
            .flex_col()
            .flex_1()
            .min_w_0()
            .min_h_0()
            .child(
                div()
                    .id("connect-body")
                    .track_scroll(&self.body_scroll)
                    .flex()
                    .flex_col()
                    .gap(px(10.))
                    .min_h_0()
                    .max_h(px(BODY_MAX_HEIGHT))
                    .overflow_y_scroll()
                    .child(form_row(ts!("connect.name"), self.name_input.clone()))
                    .child(form_row(ts!("connect.folder"), self.folder_input.clone()))
                    .child(form_row(ts!("connect.color"), colors))
                    .child(form_row(ts!("connect.driver"), driver))
                    .child(url)
                    .child(form_row(
                        ts!("connect.username"),
                        self.username_input.clone(),
                    ))
                    .child(form_row(
                        ts!("connect.password"),
                        self.password_input.clone(),
                    ))
                    .child(form_row(ts!("connect.props"), props))
                    .child(section(ts!("connect.section.behaviour"), chrome, behaviour))
                    .child(section(ts!("connect.section.tunnel"), chrome, tunnel)),
            )
            .children(
                self.hovering_scrollbar(SCROLLBARS[1].0, Surface::Body, cx)
                    .render(chrome),
            )
    }

    /// The message strip and the buttons.
    fn render_footer(&self, chrome: &Theme, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let this = cx.entity();
        let testing = self.test.is_some();

        let status = self.status.clone().map(|(level, message)| {
            div()
                .text_size(px(11.))
                .text_color(match level {
                    Level::Ok => chrome.success,
                    Level::Error => chrome.danger,
                    Level::Busy => chrome.text_muted,
                })
                .child(message)
        });

        let confirm = self.confirming.then(|| {
            let name = self
                .current()
                .map(|profile| profile.name.clone())
                .unwrap_or_default();
            let cancel = this.clone();
            let delete = this.clone();
            div()
                .flex()
                .flex_row()
                .flex_wrap()
                .items_center()
                .gap(px(8.))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .text_size(px(12.))
                        .child(ts!("connect.delete_confirm", name = name)),
                )
                .child(
                    Button::new("connect-delete-cancel", ts!("common.cancel"))
                        .variant(ButtonVariant::Secondary)
                        .on_click(move |_, _window, cx| {
                            cancel.update(cx, |dialog, cx| {
                                dialog.confirming = false;
                                cx.notify();
                            });
                        }),
                )
                .child(
                    Button::new("connect-delete-confirm", ts!("connect.delete"))
                        .variant(ButtonVariant::Danger)
                        .on_click(move |_, _window, cx| {
                            delete.update(cx, |dialog, cx| dialog.delete_profile(cx));
                        }),
                )
        });

        let button = |id: &'static str,
                      label: SharedString,
                      variant: ButtonVariant,
                      index: isize,
                      action: fn(&mut Self, &mut Context<Self>)| {
            let this = this.clone();
            Button::new(id, label)
                .variant(variant)
                .disabled(testing)
                .tab_index(index)
                .on_click(move |_, _window, cx| {
                    this.update(cx, action);
                })
        };

        // A bastion whose key nothing has ever seen: the fingerprint is shown in
        // full — it is what the user has to compare against what the machine's
        // administrator told them — and trusting it writes `known_hosts` and
        // nothing else. The connection is not retried automatically; pressing
        // Test again is the user saying they are satisfied.
        let host_key = self.pending_host_key.as_ref().map(|pending| {
            let this = this.clone();
            let host = pending.host.clone();
            let port = pending.port;
            let algorithm = pending.algorithm.clone();
            let fingerprint = pending.fingerprint.clone();
            div()
                .flex()
                .flex_row()
                .flex_wrap()
                .items_center()
                .gap(px(8.))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .text_size(px(11.))
                        .text_color(chrome.text)
                        .child(ts!(
                            "connect.host_key_question",
                            host = host.clone(),
                            algorithm = algorithm.clone(),
                            fingerprint = fingerprint.clone()
                        )),
                )
                .child(
                    Button::new("connect-trust-host-key", ts!("connect.trust_host_key"))
                        .variant(ButtonVariant::Secondary)
                        .on_click(move |_, _window, cx| {
                            let outcome =
                                connection::trust_host_key(&host, port, &algorithm, &fingerprint);
                            this.update(cx, |dialog, cx| {
                                dialog.pending_host_key = None;
                                match outcome {
                                    Ok(()) => dialog.report(
                                        Level::Ok,
                                        ts!("connect.host_key_trusted"),
                                        cx,
                                    ),
                                    Err(error) => dialog.report(
                                        Level::Error,
                                        ts!("connect.save_failed", error = format!("{error:#}")),
                                        cx,
                                    ),
                                }
                            });
                        }),
                )
        });

        div()
            .flex()
            .flex_col()
            .flex_none()
            .gap(px(10.))
            .child(div().h(px(1.)).w_full().flex_none().bg(chrome.border))
            .children(status)
            .children(host_key)
            .children(confirm)
            .child(
                div()
                    .flex()
                    .flex_row()
                    .flex_wrap()
                    .items_center()
                    .gap(px(8.))
                    .child(button(
                        "connect-new",
                        ts!("connect.new"),
                        ButtonVariant::Secondary,
                        tab::NEW,
                        Self::add_profile,
                    ))
                    .child(button(
                        "connect-duplicate",
                        ts!("connect.duplicate"),
                        ButtonVariant::Secondary,
                        tab::DUPLICATE,
                        Self::duplicate_profile,
                    ))
                    .child(button(
                        "connect-delete",
                        ts!("connect.delete"),
                        ButtonVariant::Secondary,
                        tab::DELETE,
                        |dialog, cx| {
                            dialog.confirming = true;
                            cx.notify();
                        },
                    ))
                    .child(div().flex_1())
                    .child(button(
                        "connect-test",
                        ts!("connect.test"),
                        ButtonVariant::Secondary,
                        tab::TEST,
                        Self::test_connection,
                    ))
                    .child(button(
                        "connect-cancel",
                        ts!("common.cancel"),
                        ButtonVariant::Secondary,
                        tab::CANCEL,
                        |dialog, cx| {
                            cx.emit(ConnectionDialogEvent::Dismissed);
                            dialog.close(cx);
                        },
                    ))
                    .child(button(
                        "connect-save",
                        ts!("common.save"),
                        ButtonVariant::Secondary,
                        tab::SAVE,
                        Self::save,
                    ))
                    .child(button(
                        "connect-connect",
                        ts!("connect.connect"),
                        ButtonVariant::Primary,
                        tab::CONNECT,
                        Self::connect,
                    )),
            )
    }
}

impl EventEmitter<ConnectionDialogEvent> for ConnectionDialog {}

impl Focusable for ConnectionDialog {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for ConnectionDialog {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if !self.open {
            return div().id("connection-dialog");
        }

        self.apply_pending_focus(window, cx);
        self.watch_scroll(cx);
        // The URL preview follows the part fields, and gpui gives no change
        // notification for a text field it does not own the model of; rebuilding
        // it here is what makes the preview live. It writes only when the text
        // actually differs, so it cannot loop.
        //
        // Which of the two fields moved is what focus answers: typing in a part
        // field leaves the caret there and the URL field unfocused, so the
        // preview is stale and gets rewritten; typing in the URL field puts the
        // caret *in* it, so the same divergence is the user overriding the
        // template — and overwriting that would undo the keystroke they just
        // made.
        if !self.url_overridden && self.manager.is_none() {
            let assembled = self.assembled_url(cx);
            let diverged = !assembled.is_empty() && assembled != text(&self.url_input, cx);
            if self.url_input.read(cx).focus_handle(cx).is_focused(window) {
                if diverged {
                    self.url_overridden = true;
                }
            } else if diverged {
                set_text(&self.url_input, assembled, cx);
            }
        }

        let chrome = theme(cx);

        // The driver manager replaces the form rather than covering it, so the
        // window's tab ring only ever holds controls that are on screen — the
        // same rule the settings dialog's colour editor follows.
        let (title, body) = match self.manager.clone() {
            Some(manager) => (manager.read(cx).title(), manager.into_any_element()),
            None => {
                let list = self.render_list(&chrome, cx);
                let form = self.render_form(&chrome, cx);
                let footer = self.render_footer(&chrome, cx);
                (
                    ts!("connect.title"),
                    div()
                        .flex()
                        .flex_col()
                        .min_h_0()
                        .gap(px(12.))
                        .child(
                            div()
                                .flex()
                                .flex_row()
                                .min_h_0()
                                .gap(px(12.))
                                .child(list)
                                .child(form),
                        )
                        .child(footer)
                        .into_any_element(),
                )
            }
        };

        let on_dismiss = {
            let this = cx.entity();
            move |_window: &mut Window, cx: &mut App| {
                this.update(cx, |dialog, cx| dialog.escape(cx));
            }
        };

        div()
            .id("connection-dialog")
            .absolute()
            .inset_0()
            .size_full()
            .track_focus(&self.focus_handle)
            .on_drag_move::<DraggedThumb>(cx.listener(
                |dialog, event: &DragMoveEvent<DraggedThumb>, _window, cx| {
                    dialog.drag_scrollbar(event, cx);
                },
            ))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|dialog, _: &MouseUpEvent, _window, cx| {
                    dialog.release_scrollbars(cx);
                }),
            )
            .on_mouse_up_out(
                MouseButton::Left,
                cx.listener(|dialog, _: &MouseUpEvent, _window, cx| {
                    dialog.release_scrollbars(cx);
                }),
            )
            .child(modal(
                "connect-modal",
                title,
                px(DIALOG_WIDTH),
                body,
                on_dismiss,
            ))
    }
}

/// Which saved profile a dialog opening over `wanted` should show.
///
/// `wanted` is the welcome list's "edit…" naming the row that was
/// right-clicked; `None` is the plain "new connection" door, which has no
/// opinion. Either way the answer falls back to the first saved profile, which
/// is what the dialog has always opened over — a profile can be deleted between
/// the welcome screen being drawn and its row being clicked, and a form opened
/// over nothing would be worse than a form opened over the wrong thing.
///
/// Separate from [`ConnectionDialog::open_showing`] because that one re-reads
/// the file first, and the rule itself is worth pinning down without one.
fn profile_to_show(store: &ConnectionStore, wanted: Option<Uuid>) -> Option<Uuid> {
    wanted
        .filter(|id| store.get(*id).is_some())
        .or_else(|| store.connections().first().map(|profile| profile.id))
}

/// The saved profiles as rows, grouped under the folder each is filed in.
///
/// Shared by this dialog's list and the welcome screen the shell draws while no
/// connection is open, so a profile is recognised the same way wherever it is
/// offered: filed under its folder, marked with its colour, named by its name.
/// Groups keep the order the folders first appear in, which is the order the
/// user arranged the profiles in — sorting them would move a row the moment it
/// is renamed.
///
/// Only the rows. The box they scroll inside belongs to whoever draws them: the
/// dialog gives them a fixed column beside the form, the welcome screen a
/// centred one under a button, and neither shape suits the other.
///
/// `selected` marks the row the dialog is editing; a list with no selection to
/// show — the welcome screen's — passes `None`. `on_click` is handed the
/// profile's id, which is all either caller needs: the dialog selects it, the
/// shell opens a connection on it.
///
/// `on_context` is the right-click, and only the welcome screen passes one.
/// The dialog's own list gets no menu: everything such a menu would offer —
/// edit this profile, delete it — is the form standing beside the row, already
/// open on whatever is selected.
pub(crate) fn profile_rows(
    profiles: &[ConnectionProfile],
    selected: Option<Uuid>,
    chrome: &Theme,
    on_click: impl Fn(Uuid, &mut Window, &mut App) + Clone + 'static,
    on_context: Option<ProfileContextHandler>,
) -> Vec<gpui::AnyElement> {
    let mut groups: Vec<(Option<String>, Vec<&ConnectionProfile>)> = Vec::new();
    for profile in profiles {
        let folder = profile.folder.clone().filter(|folder| !folder.is_empty());
        match groups.iter_mut().find(|(name, _)| *name == folder) {
            Some((_, members)) => members.push(profile),
            None => groups.push((folder, vec![profile])),
        }
    }

    let mut index = 0usize;
    let mut rows: Vec<gpui::AnyElement> = Vec::new();
    for (folder, members) in groups {
        if let Some(folder) = folder {
            rows.push(
                div()
                    .px(px(6.))
                    .pt(px(6.))
                    .text_size(px(10.))
                    .text_color(chrome.text_muted)
                    .truncate()
                    .child(SharedString::from(folder))
                    .into_any_element(),
            );
        }
        for profile in members {
            let id = profile.id;
            let selected = selected == Some(id);
            let name = if profile.name.trim().is_empty() {
                ts!("connect.unnamed")
            } else {
                SharedString::from(profile.name.clone())
            };
            let tag = profile
                .color
                .as_deref()
                .and_then(rudbman_ui::parse_hex)
                .map(|color| {
                    div()
                        .flex_none()
                        .w(px(3.))
                        .h(px(16.))
                        .rounded_full()
                        .bg(color)
                });
            let on_click = on_click.clone();
            let on_context = on_context.clone();
            rows.push(
                div()
                    .id(("profile-row", index))
                    // Compiled away outside a test build. The row is the whole
                    // hit area, so this is the one box a test has to press on,
                    // and it saves working the position out from the row height
                    // and however many folder headings sit above it.
                    .debug_selector(move || format!("{ROW_SELECTOR}{id}"))
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(6.))
                    .px(px(6.))
                    .py(px(5.))
                    .rounded_md()
                    .cursor_pointer()
                    // The same range in both lists, because neither puts
                    // anything else between the tab ring's start and the rows.
                    .tab_index((tab::LIST + index as isize).min(tab::LIST_LIMIT))
                    .when(selected, |row| row.bg(chrome.surface_active))
                    .when(!selected, |row| {
                        row.hover(|row| row.bg(chrome.surface_hover))
                    })
                    .children(tag)
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_size(px(12.))
                            .text_color(chrome.text)
                            .child(name),
                    )
                    .on_click(move |_, window, cx| on_click(id, window, cx))
                    // Taken on the press, as every right-click in the shell is:
                    // a menu that waited for the release would lag the gesture
                    // here and nowhere else. The press is swallowed so that it
                    // belongs to the menu about to open rather than to the
                    // screen behind it.
                    .when_some(on_context, |row, on_context| {
                        row.on_mouse_down(
                            MouseButton::Right,
                            move |event: &MouseDownEvent, window, cx| {
                                cx.stop_propagation();
                                on_context(id, event.position, window, cx);
                            },
                        )
                    })
                    .into_any_element(),
            );
            index += 1;
        }
    }
    rows
}

/// Wraps `body` in a titled card.
fn section<E: IntoElement>(
    title: SharedString,
    chrome: &Theme,
    body: E,
) -> impl IntoElement + use<E> {
    div()
        .flex()
        .flex_col()
        .gap(px(10.))
        .p(px(12.))
        .rounded_lg()
        .border_1()
        .border_color(chrome.border)
        .bg(chrome.surface)
        .child(
            div()
                .text_size(px(11.))
                .text_color(chrome.text_muted)
                .child(title),
        )
        .child(body)
}

/// The label of one URL part field.
///
/// The four catalogue spellings share one label — what a product calls the thing
/// you connect to differs, but the question does not — while anything a
/// hand-written template invents is shown under its own name.
fn label_for(placeholder: &str) -> SharedString {
    match placeholder {
        "host" => ts!("connect.host"),
        "port" => ts!("connect.port"),
        name if CATALOGUE_PLACEHOLDERS.contains(&name) => ts!("connect.database"),
        other => SharedString::from(other.to_owned()),
    }
}

/// The sample value shown in an empty URL part field.
fn placeholder_for(name: &str) -> SharedString {
    match name {
        "host" => SharedString::new_static("db.example.com"),
        "port" => SharedString::new_static("5432"),
        "file" => SharedString::new_static("/var/lib/app.sqlite"),
        _ => SharedString::new_static("app"),
    }
}

/// Reads a URL back into the values its template's holes were filled with.
///
/// The template is split on its `{holes}` and the URL matched against the
/// literal pieces between them; a URL that does not fit — because it was
/// overridden, or written for another driver — yields `None`, which is what
/// makes the form show it as an override rather than shredding it into fields
/// it does not belong in.
fn decompose(template: &str, url: &str) -> Option<Vec<(String, String)>> {
    let names = placeholders_of(template);
    if names.is_empty() {
        return None;
    }

    let mut values = Vec::new();
    let mut rest_template = template;
    let mut rest_url = url;

    while let Some(open) = rest_template.find('{') {
        let close = rest_template[open..].find('}').map(|index| open + index)?;
        let literal = &rest_template[..open];
        if !rest_url.starts_with(literal) {
            return None;
        }
        rest_url = &rest_url[literal.len()..];
        let name = rest_template[open + 1..close].to_string();
        rest_template = &rest_template[close + 1..];

        // The value runs up to the next literal, or to the end of the URL when
        // the hole is the last thing in the template.
        let next_literal_end = rest_template
            .find('{')
            .map_or(rest_template.len(), |index| index);
        let next_literal = &rest_template[..next_literal_end];
        let value_end = if next_literal.is_empty() {
            rest_url.len()
        } else {
            rest_url.find(next_literal)?
        };
        values.push((name, rest_url[..value_end].to_string()));
        rest_url = &rest_url[value_end..];
    }

    if rest_url != rest_template {
        return None;
    }
    Some(values)
}

/// A one-line suggestion for a failure the `SQLSTATE` class can classify.
///
/// Only the classes worth a hint get one: `28` is the user's password, `08` is
/// the address, and a driver failure is the JAR. Everything else is left to the
/// driver's own message, which is usually better than anything invented here.
fn error_hint(error: &ConnectError) -> Option<SharedString> {
    if error.is_authentication() {
        return Some(ts!("connect.hint_auth"));
    }
    if error.is_driver() {
        return Some(ts!("connect.hint_driver"));
    }
    match error {
        ConnectError::JvmStart(_) => Some(ts!("connect.hint_jvm")),
        ConnectError::HostKeyUnknown { .. } | ConnectError::HostKeyMismatch { .. } => {
            Some(ts!("connect.hint_host_key"))
        }
        _ if error.sql_state_class() == Some("08") => Some(ts!("connect.hint_network")),
        _ => None,
    }
}

/// Trimmed content of `input`.
fn text(input: &Entity<TextInput>, cx: &App) -> String {
    input.read(cx).content().trim().to_owned()
}

/// Replaces the contents of `input`.
fn set_text(input: &Entity<TextInput>, value: impl Into<SharedString>, cx: &mut App) {
    input.update(cx, |input, cx| input.set_content(value, cx));
}

/// Parses `input`, falling back to `default` when it is blank or malformed.
fn parse_or<T: std::str::FromStr>(input: &Entity<TextInput>, default: T, cx: &App) -> T {
    text(input, cx).parse::<T>().unwrap_or(default)
}

#[cfg(test)]
mod tests {
    use std::ops::Deref;

    use gpui::VisualTestContext;

    use super::*;

    #[test]
    fn a_dialog_opens_over_the_profile_it_was_asked_for() {
        let mut store = ConnectionStore::default();
        let first = ConnectionProfile::new("first", "h2", "jdbc:h2:mem:a", "sa");
        let second = ConnectionProfile::new("second", "h2", "jdbc:h2:mem:b", "sa");
        store.upsert(first.clone());
        store.upsert(second.clone());

        // The welcome list's "edit…" names a row, and that is the one the form
        // opens over — not whichever profile happens to be first.
        assert_eq!(profile_to_show(&store, Some(second.id)), Some(second.id));
        // The plain door has no opinion and takes the first, as it always has.
        assert_eq!(profile_to_show(&store, None), Some(first.id));
        // A profile deleted between the screen being drawn and the row being
        // clicked falls back rather than opening a form over nothing.
        assert_eq!(
            profile_to_show(&store, Some(Uuid::new_v4())),
            Some(first.id)
        );
        // And with nothing saved there is nothing to show, which is what makes
        // the dialog start a new profile instead.
        assert_eq!(
            profile_to_show(&ConnectionStore::default(), Some(first.id)),
            None
        );
    }

    #[test]
    fn a_url_is_taken_apart_by_the_template_that_built_it() {
        let parts = decompose(
            "jdbc:postgresql://{host}:{port}/{database}",
            "jdbc:postgresql://db.example.com:5432/app",
        )
        .expect("the URL fits the template");
        assert_eq!(
            parts,
            vec![
                ("host".to_string(), "db.example.com".to_string()),
                ("port".to_string(), "5432".to_string()),
                ("database".to_string(), "app".to_string()),
            ]
        );

        // A hole at the very end of a template runs to the end of the URL.
        assert_eq!(
            decompose("jdbc:sqlite:{file}", "jdbc:sqlite:/var/lib/app.db"),
            Some(vec![("file".to_string(), "/var/lib/app.db".to_string())])
        );
        // SQL Server's semicolon grammar comes apart the same way.
        assert_eq!(
            decompose(
                "jdbc:sqlserver://{host}:{port};databaseName={database}",
                "jdbc:sqlserver://sql:1433;databaseName=app"
            ),
            Some(vec![
                ("host".to_string(), "sql".to_string()),
                ("port".to_string(), "1433".to_string()),
                ("database".to_string(), "app".to_string()),
            ])
        );
    }

    #[test]
    fn a_url_that_does_not_fit_its_template_is_left_whole() {
        // An embedded H2 file against the server template: the user typed a URL
        // the template cannot express, and shredding it into fields would lose
        // it. `None` is what makes the form call it an override.
        assert_eq!(
            decompose("jdbc:h2:tcp://{host}:{port}/{database}", "jdbc:h2:mem:test"),
            None
        );
        // A URL for another driver entirely.
        assert_eq!(
            decompose(
                "jdbc:postgresql://{host}:{port}/{database}",
                "jdbc:mysql://h:3306/app"
            ),
            None
        );
        // A template with no holes has nothing to take apart.
        assert_eq!(decompose("jdbc:h2:mem:test", "jdbc:h2:mem:test"), None);
    }

    #[test]
    fn a_trailing_hole_takes_everything_that_follows_it() {
        // `{database}` is the last thing in the template, so a query string
        // lands in the database field rather than being dropped. It reads a
        // little oddly, and it is the right trade: the round trip stays exact,
        // so the URL the driver is handed is the one the user typed. Losing the
        // parameters — or refusing the whole URL over them — would be worse.
        let parts = decompose(
            "jdbc:postgresql://{host}:{port}/{database}",
            "jdbc:postgresql://h:5432/app?ssl=true",
        )
        .expect("the literal parts still line up");
        assert_eq!(
            parts.last(),
            Some(&("database".to_string(), "app?ssl=true".to_string()))
        );
        let values: HashMap<String, String> = parts.into_iter().collect();
        assert_eq!(
            substitute("jdbc:postgresql://{host}:{port}/{database}", &values),
            "jdbc:postgresql://h:5432/app?ssl=true"
        );
    }

    #[test]
    fn taking_a_url_apart_and_putting_it_back_is_the_identity() {
        // The two halves of the URL editor have to agree, or opening a saved
        // profile would rewrite its URL the moment the form was drawn.
        for (template, url) in [
            (
                "jdbc:postgresql://{host}:{port}/{database}",
                "jdbc:postgresql://db:5432/app",
            ),
            (
                "jdbc:oracle:thin:@//{host}:{port}/{service}",
                "jdbc:oracle:thin:@//ora:1521/ORCLPDB",
            ),
            (
                "jdbc:sqlserver://{host}:{port};databaseName={database}",
                "jdbc:sqlserver://sql:1433;databaseName=app",
            ),
            ("jdbc:sqlite:{file}", "jdbc:sqlite:/tmp/a.db"),
        ] {
            let parts: HashMap<String, String> = decompose(template, url)
                .expect(template)
                .into_iter()
                .collect();
            assert_eq!(substitute(template, &parts), url, "{template}");
        }
    }

    #[test]
    fn every_url_hole_of_every_built_in_driver_has_a_label() {
        // A driver whose template names something the form has no label for
        // would draw a field headed `{service}`. The four catalogue spellings
        // share one label; `host` and `port` have their own.
        for driver in DriverDef::builtins() {
            for name in placeholders_of(&driver.url_template) {
                let label = label_for(&name);
                assert!(!label.is_empty(), "{} / {name}", driver.id);
                assert_ne!(
                    label.as_ref(),
                    name.as_str(),
                    "{} has no label for {name}",
                    driver.id
                );
                assert!(
                    !label.starts_with("connect."),
                    "untranslated label for {name}: {label:?}"
                );
            }
        }
    }

    #[test]
    fn every_label_the_dialog_draws_has_a_translation() {
        for label in [
            ts!("connect.title"),
            ts!("connect.name"),
            ts!("connect.folder"),
            ts!("connect.color"),
            ts!("connect.driver"),
            ts!("connect.pick_driver"),
            ts!("connect.manage_drivers"),
            ts!("connect.driver_has_no_jar"),
            ts!("connect.host"),
            ts!("connect.port"),
            ts!("connect.database"),
            ts!("connect.url"),
            ts!("connect.url_hint"),
            ts!("connect.url_overridden"),
            ts!("connect.url_reset"),
            ts!("connect.username"),
            ts!("connect.password"),
            ts!("connect.props"),
            ts!("connect.prop_key"),
            ts!("connect.prop_value"),
            ts!("connect.add_prop"),
            ts!("connect.section.behaviour"),
            ts!("connect.section.tunnel"),
            ts!("connect.read_only"),
            ts!("connect.auto_commit"),
            ts!("connect.confirm_writes"),
            ts!("connect.keep_alive"),
            ts!("connect.tunnel_enabled"),
            ts!("connect.tunnel_host"),
            ts!("connect.tunnel_user"),
            ts!("connect.tunnel_auth"),
            ts!("connect.tunnel_auth_agent"),
            ts!("connect.tunnel_auth_key"),
            ts!("connect.tunnel_auth_password"),
            ts!("connect.tunnel_key"),
            ts!("connect.tunnel_password"),
            ts!("connect.tunnel_passphrase"),
            ts!("connect.tunnel_remote"),
            ts!("connect.tunnel_hint"),
            ts!("connect.new"),
            ts!("connect.new_name"),
            ts!("connect.duplicate"),
            ts!("connect.delete"),
            ts!("connect.delete_confirm", name = "X"),
            ts!("connect.test"),
            ts!("connect.testing"),
            ts!("connect.test_ok", product = "H2 2.3.232"),
            ts!("connect.test_ok_unnamed"),
            ts!("connect.test_failed", error = "e"),
            ts!("connect.test_failed_hint", error = "e", hint = "h"),
            ts!("connect.connect"),
            ts!("connect.saved"),
            ts!("connect.unnamed"),
            ts!("connect.copy_name", name = "X"),
            ts!("connect.name_required"),
            ts!("connect.url_required"),
            ts!("connect.no_driver", driver = "x"),
            ts!("connect.load_failed", error = "e"),
            ts!("connect.save_failed", error = "e"),
            ts!("connect.keychain_failed", error = "e"),
            ts!("connect.hint_auth"),
            ts!("connect.hint_driver"),
            ts!("connect.hint_jvm"),
            ts!("connect.hint_network"),
            ts!("connect.hint_host_key"),
        ] {
            assert!(!label.is_empty(), "empty label");
            assert!(
                !label.starts_with("connect."),
                "untranslated label {label:?}"
            );
        }

        // The copy's name has to carry the original's, or duplicating twice
        // would produce two profiles that read identically.
        let copy = ts!("connect.copy_name", name = "staging");
        assert!(copy.contains("staging"), "{copy:?}");
        assert_ne!(copy, "staging");
    }

    #[test]
    fn the_colour_tags_are_all_parseable() {
        // A swatch whose value the theme layer cannot read would draw as the
        // fallback and two tags would look the same.
        for color in COLORS {
            assert!(rudbman_ui::parse_hex(color).is_some(), "{color}");
        }
        let mut unique = COLORS.to_vec();
        unique.sort_unstable();
        unique.dedup();
        assert_eq!(unique.len(), COLORS.len(), "two tags share a colour");
    }

    #[test]
    fn an_authentication_failure_gets_the_password_hint() {
        let auth = ConnectError::Database(
            serde_json::from_str(
                r#"{"kind":"sql","sql_state":"28000","vendor_code":0,
                    "message":"nope","causes":[],"stack":null}"#,
            )
            .expect("envelope"),
        );
        assert_eq!(error_hint(&auth), Some(ts!("connect.hint_auth")));

        let network = ConnectError::Database(
            serde_json::from_str(
                r#"{"kind":"sql","sql_state":"08001","vendor_code":0,
                    "message":"nope","causes":[],"stack":null}"#,
            )
            .expect("envelope"),
        );
        assert_eq!(error_hint(&network), Some(ts!("connect.hint_network")));

        assert_eq!(
            error_hint(&ConnectError::NoDriverJar("H2".into())),
            Some(ts!("connect.hint_driver"))
        );
        assert_eq!(
            error_hint(&ConnectError::JvmStart("no runtime".into())),
            Some(ts!("connect.hint_jvm"))
        );
        // A failure nothing can be said about gets no hint rather than a
        // platitude.
        assert_eq!(error_hint(&ConnectError::Tunnel("nope".into())), None);
    }

    /// The dialog open over a saved PostgreSQL profile, drawn in a window so
    /// that `render` — where the URL preview is kept in step — actually runs.
    ///
    /// Seeded by hand rather than through [`ConnectionDialog::open`], which
    /// would read `connections.json` and the keychain off whichever machine is
    /// running the test.
    fn open_over_a_profile(
        cx: &mut gpui::TestAppContext,
    ) -> (Entity<ConnectionDialog>, VisualTestContext) {
        cx.update(|cx| {
            app_settings::init(cx);
            rudbman_ui::init(cx);
        });

        let window = cx.add_window(|_, cx| ConnectionDialog::new(cx));
        let dialog = window
            .update(cx, |_, _, cx| cx.entity())
            .expect("the window is open");
        cx.update(|cx| {
            dialog.update(cx, |dialog, cx| {
                let profile = ConnectionProfile::new("staging", "postgresql", "", "app");
                dialog.selected = Some(profile.id);
                dialog.store.upsert(profile);
                dialog.rebuild_url_parts(cx);
                set_text(&dialog.url_parts["host"], "db", cx);
                set_text(&dialog.url_parts["database"], "app", cx);
                dialog.open = true;
            });
        });

        let mut cx = VisualTestContext::from_window(*window.deref(), cx);
        cx.update(|window, _| window.refresh());
        cx.run_until_parked();
        (dialog, cx)
    }

    /// The hint under the field says the URL can be typed over, so it has to
    /// survive the next frame: the sync that keeps the preview live must not
    /// take the user's own URL back.
    #[gpui::test]
    fn a_url_typed_over_the_assembled_one_becomes_an_override(cx: &mut gpui::TestAppContext) {
        let (dialog, mut cx) = open_over_a_profile(cx);
        cx.update(|_, cx| {
            let dialog = dialog.read(cx);
            assert_eq!(text(&dialog.url_input, cx), "jdbc:postgresql://db:5432/app");
            assert!(!dialog.url_overridden);
        });

        // What typing into the field amounts to: the caret in it, and content
        // the template cannot produce.
        cx.update(|window, cx| {
            let input = dialog.read(cx).url_input.clone();
            input.read(cx).focus_handle(cx).focus(window);
            set_text(&input, "jdbc:postgresql://db:5432/app?ssl=true", cx);
        });
        cx.update(|window, _| window.refresh());
        cx.run_until_parked();

        cx.update(|_, cx| {
            let dialog = dialog.read(cx);
            assert_eq!(
                text(&dialog.url_input, cx),
                "jdbc:postgresql://db:5432/app?ssl=true",
                "the field was written back over the user's URL"
            );
            assert!(
                dialog.url_overridden,
                "an edited URL field is an override, badge and reset button and all"
            );
        });

        // And having said so, the part fields leave it alone from here.
        cx.update(|_, cx| {
            let host = dialog.read(cx).url_parts["host"].clone();
            set_text(&host, "replica", cx);
        });
        cx.update(|window, _| window.refresh());
        cx.run_until_parked();
        cx.update(|_, cx| {
            assert_eq!(
                text(&dialog.read(cx).url_input, cx),
                "jdbc:postgresql://db:5432/app?ssl=true"
            );
        });
    }

    /// The other half of the same rule: with the caret in a part field, the URL
    /// is the preview it has always been.
    #[gpui::test]
    fn a_part_field_still_rewrites_the_url_it_is_not_typing_into(cx: &mut gpui::TestAppContext) {
        let (dialog, mut cx) = open_over_a_profile(cx);

        cx.update(|window, cx| {
            let host = dialog.read(cx).url_parts["host"].clone();
            host.read(cx).focus_handle(cx).focus(window);
            set_text(&host, "replica", cx);
        });
        cx.update(|window, _| window.refresh());
        cx.run_until_parked();

        cx.update(|_, cx| {
            let dialog = dialog.read(cx);
            assert_eq!(
                text(&dialog.url_input, cx),
                "jdbc:postgresql://replica:5432/app"
            );
            assert!(
                !dialog.url_overridden,
                "the preview following its fields is not an override"
            );
        });
    }
}
