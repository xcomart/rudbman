//! The JDBC driver manager: which drivers exist, where their JARs are, and how
//! to fetch one from Maven Central.
//!
//! Edits [`DriverStore`] and nothing else. It opens on top of the connection
//! dialog — the two are one workflow, since the first thing a new profile needs
//! is a driver with a JAR behind it — and reports back through
//! [`DriverManagerEvent`] so that the picker reloads when a driver is added or
//! removed.
//!
//! # A JAR arrives one of two ways
//!
//! Either the user points at a copy they already have ([`DriverManager::add_jars`],
//! a platform file dialog), or rudbman fetches it from the coordinate the
//! definition carries ([`DriverManager::download`]). Both end in the same place:
//! a path in [`DriverDef::jars`], which is the class path the bridge builds an
//! isolated loader from.
//!
//! # The class name is looked up, not typed
//!
//! [`DriverManager::detect_class`] asks the bridge which `java.sql.Driver`
//! implementations the registered JARs hold ([`Jvm::probe_drivers`][probe]), so
//! the user does not have to find a fully qualified class name in a vendor's
//! documentation. The scan never runs a static initialiser, so looking at a file
//! opens no socket and loads no native library.
//!
//! [probe]: rudbman_jdbc::Jvm::probe_drivers
//!
//! Three outcomes that are *not* the same thing, and are not worded as if they
//! were: an archive with no driver in it means the wrong file was picked — a
//! sources or javadoc JAR, which is the mistake people actually make; a damaged
//! archive means a broken download; a path that is not there means the file
//! moved. Each names its own fix.
//!
//! # The download does not block the window
//!
//! [`maven::download`] is synchronous and runs on a background task; progress
//! arrives on a channel that a `cx.spawn` drains into the view. Cancelling sets
//! a flag the download checks once per chunk, so the button takes effect within
//! one read rather than at the end of the transfer.

use std::path::PathBuf;

use futures::StreamExt;
use futures::channel::mpsc;
use gpui::{
    App, Context, DragMoveEvent, Entity, EventEmitter, FocusHandle, Focusable, IntoElement,
    MouseButton, MouseUpEvent, PathPromptOptions, Render, ScrollHandle, SharedString, Task, Window,
    div, prelude::*, px,
};
use rudbman_core::{DriverDef, DriverStore, drivers_dir};
use rudbman_jdbc::{BridgeErrorKind, DriverProbe, Error as JdbcError};
use rudbman_ui::{
    Button, ButtonVariant, Checkbox, DraggedThumb, Scrollbar, ScrollbarAxis, ScrollbarState,
    TextInput, Theme, form_row, hide_later, hide_now, scroll_to, scrolled, theme,
};

use crate::app_settings;
use crate::connection::{self, ConnectError};
use crate::i18n::ts;
use crate::maven::{self, Cancel, Coordinate, DownloadError, Progress};

/// Element id of the driver list's overlay scroll indicator.
const LIST_SCROLLBAR: &str = "driver-list-scrollbar";

/// Element id of the detail pane's overlay scroll indicator.
const BODY_SCROLLBAR: &str = "driver-body-scrollbar";

/// Width of the driver list column.
const LIST_WIDTH: f32 = 200.;

/// Height at which the two columns start scrolling.
const BODY_MAX_HEIGHT: f32 = 460.;

/// Tab order of the manager's controls.
mod tab {
    /// First index of the driver list; one per driver from there.
    pub const LIST: isize = 300;
    /// Ceiling of the list's range, so a long driver list cannot run into the
    /// form below it.
    pub const LIST_LIMIT: isize = 399;
    /// Display name.
    pub const NAME: isize = 400;
    /// Driver class.
    pub const CLASS: isize = 410;
    /// The "detect it from the JAR" button beside the class.
    pub const DETECT: isize = 411;
    /// First index of the rows offered when a JAR holds several drivers.
    ///
    /// Nine of them before the URL template's index, which is more drivers than
    /// any archive has ever shipped; a tenth would simply share the last index.
    pub const PROBE_CHOICE: isize = 412;
    /// URL template.
    pub const URL_TEMPLATE: isize = 420;
    /// Default port.
    pub const PORT: isize = 430;
    /// SQL dialect id.
    pub const DIALECT: isize = 440;
    /// Maven coordinate.
    pub const MAVEN: isize = 450;
    /// The download button beside the coordinate.
    pub const DOWNLOAD: isize = 455;
    /// "Add JAR…".
    pub const ADD_JAR: isize = 460;
    /// First index of the per-JAR remove buttons.
    pub const REMOVE_JAR: isize = 461;
    /// "Read table comments with a query".
    pub const USE_TABLE_COMMENTS: isize = 470;
    /// The table-comment statement.
    pub const TABLE_COMMENTS: isize = 471;
    /// "Read column comments with a query".
    pub const USE_COLUMN_COMMENTS: isize = 472;
    /// The column-comment statement.
    pub const COLUMN_COMMENTS: isize = 473;
    /// New driver.
    pub const NEW: isize = 500;
    /// Delete driver.
    pub const DELETE: isize = 510;
    /// Close.
    pub const CLOSE: isize = 520;
    /// Save.
    pub const SAVE: isize = 530;
}

/// What the manager tells the dialog that opened it.
pub enum DriverManagerEvent {
    /// A driver was added, edited or removed and `drivers.json` was rewritten.
    /// The connection dialog re-reads its picker.
    Changed,
    /// The manager was closed.
    Dismissed,
}

/// Where a driver-class probe has got to.
enum Probe {
    /// The scan is running. The task is held rather than detached so that
    /// closing the manager abandons it.
    Running {
        /// Dropped, and so cancelled, with the manager.
        _task: Task<()>,
    },
    /// Several drivers were found and the user has to say which.
    Choosing {
        /// Every class found, the declared services first.
        candidates: Vec<String>,
        /// The one [`DriverProbe::recommended`] would pick, preselected.
        recommended: Option<String>,
    },
}

/// Why a probe produced no class name.
///
/// Four cases rather than one string, because each has a different fix and the
/// UI says so; see [`DriverManager::probed`].
#[derive(Debug)]
enum ProbeFailure {
    /// The JVM would not start — no Java runtime, or no bridge JAR.
    Jvm(String),
    /// A registered JAR is not where the definition says it is.
    Missing(String),
    /// The archive could not be read to the end.
    Damaged(String),
    /// Anything else the JNI layer reported.
    Other(String),
}

impl From<JdbcError> for ProbeFailure {
    fn from(error: JdbcError) -> Self {
        match error {
            JdbcError::JvmStart(message) => ProbeFailure::Jvm(message),
            // The bridge tells the two archive failures apart for us: a path
            // that is not there is `driver`, a stream that dies part way
            // through is `io`.
            JdbcError::Bridge(bridge) => match bridge.kind {
                BridgeErrorKind::Driver => ProbeFailure::Missing(bridge.message),
                BridgeErrorKind::Io => ProbeFailure::Damaged(bridge.message),
                _ => ProbeFailure::Other(bridge.to_string()),
            },
            other => ProbeFailure::Other(other.to_string()),
        }
    }
}

/// The classes a probe offers, in the order to offer them.
///
/// The `META-INF/services` declarations first — that is the vendor naming its
/// own entry point — then whatever the scan turned up that the declaration did
/// not already cover. Deduplicated, because the two lists overlap for every
/// well-formed driver JAR.
fn candidates_of(probe: &DriverProbe) -> Vec<String> {
    let mut found: Vec<String> = Vec::new();
    for class in probe.services.iter().chain(probe.classes.iter()) {
        if !found.contains(class) {
            found.push(class.clone());
        }
    }
    found
}

/// What a probe failure is shown as.
///
/// Separate from [`DriverManager::probed`] so that "a damaged archive says the
/// download is broken" can be asserted without a window.
fn probe_message(failure: &ProbeFailure) -> SharedString {
    match failure {
        ProbeFailure::Jvm(error) => ts!("driver.probe_jvm_failed", error = error.clone()),
        ProbeFailure::Missing(error) => ts!("driver.probe_missing", error = error.clone()),
        ProbeFailure::Damaged(error) => ts!("driver.probe_damaged", error = error.clone()),
        ProbeFailure::Other(error) => ts!("driver.probe_failed", error = error.clone()),
    }
}

/// A download in flight.
struct Download {
    /// What is being fetched, for the message.
    file_name: SharedString,
    /// How far it has got.
    progress: Progress,
    /// The flag the cancel button raises.
    cancel: Cancel,
}

/// The driver manager view.
pub struct DriverManager {
    /// The store as it stands, including unsaved edits to the selected driver.
    store: DriverStore,
    /// Id of the driver being edited, or `None` when the store is empty.
    selected: Option<String>,
    /// A download in flight, if any.
    download: Option<Download>,
    /// A driver-class probe running, or waiting for the user to pick.
    probe: Option<Probe>,
    /// Message strip under the form.
    status: Option<SharedString>,
    /// Whether the message is a failure rather than a report.
    status_is_error: bool,
    /// Whether the delete confirmation is showing.
    confirming: bool,
    /// Focus of the manager's root.
    focus_handle: FocusHandle,
    /// Scroll of the driver list.
    list_scroll: ScrollHandle,
    /// Whether the list's overlay bar is on screen.
    list_scrollbar: ScrollbarState,
    /// Scroll of the detail pane.
    body_scroll: ScrollHandle,
    /// Whether the detail pane's overlay bar is on screen.
    body_scrollbar: ScrollbarState,
    /// Display name of the selected driver.
    name_input: Entity<TextInput>,
    /// Fully qualified driver class.
    class_input: Entity<TextInput>,
    /// URL skeleton with `{placeholder}` holes.
    url_template_input: Entity<TextInput>,
    /// Port the connection editor pre-fills; blank for a file-backed database.
    port_input: Entity<TextInput>,
    /// SQL dialect id.
    dialect_input: Entity<TextInput>,
    /// Maven coordinate, `group:artifact:version`.
    maven_input: Entity<TextInput>,
    /// Query answering table comments the driver leaves blank.
    table_comments_input: Entity<TextInput>,
    /// Query answering column comments the driver leaves blank.
    column_comments_input: Entity<TextInput>,
}

impl DriverManager {
    /// Builds the manager over the drivers on disk.
    ///
    /// A `drivers.json` that cannot be read is reported and the built-in
    /// definitions are shown instead, so the manager is still the place the
    /// problem can be fixed from.
    pub fn new(cx: &mut Context<Self>) -> Self {
        let field = |cx: &mut Context<Self>, placeholder: SharedString, index: isize| {
            cx.new(move |cx| TextInput::new(cx).placeholder(placeholder).tab_index(index))
        };

        let (store, status) = match DriverStore::load() {
            Ok(store) => (store, None),
            Err(error) => {
                log::error!("could not read drivers.json: {error:#}");
                (
                    DriverStore::default(),
                    Some(ts!("driver.load_failed", error = format!("{error:#}"))),
                )
            }
        };
        let selected = store.drivers().first().map(|driver| driver.id.clone());

        // The placeholders are examples, and every one of them is a value a
        // driver actually uses; none of them is a word, so none is translated.
        let mut manager = Self {
            store,
            selected,
            download: None,
            probe: None,
            status_is_error: status.is_some(),
            status,
            confirming: false,
            focus_handle: cx.focus_handle(),
            list_scroll: ScrollHandle::new(),
            list_scrollbar: ScrollbarState::new(),
            body_scroll: ScrollHandle::new(),
            body_scrollbar: ScrollbarState::new(),
            name_input: field(cx, "PostgreSQL".into(), tab::NAME),
            class_input: field(cx, "org.postgresql.Driver".into(), tab::CLASS),
            url_template_input: field(
                cx,
                "jdbc:postgresql://{host}:{port}/{database}".into(),
                tab::URL_TEMPLATE,
            ),
            port_input: field(cx, "5432".into(), tab::PORT),
            dialect_input: field(cx, "postgres".into(), tab::DIALECT),
            maven_input: field(cx, "org.postgresql:postgresql:42.7.4".into(), tab::MAVEN),
            table_comments_input: field(
                cx,
                "SELECT table_name, comments FROM all_tab_comments WHERE owner = '${schema}'"
                    .into(),
                tab::TABLE_COMMENTS,
            ),
            column_comments_input: field(
                cx,
                "SELECT table_name, column_name, comments FROM all_col_comments \
                 WHERE owner = '${schema}'"
                    .into(),
                tab::COLUMN_COMMENTS,
            ),
        };
        manager.fill_form(cx);
        manager
    }

    /// The driver currently being edited.
    fn current(&self) -> Option<&DriverDef> {
        self.selected.as_deref().and_then(|id| self.store.get(id))
    }

    /// Copies the selected driver into the fields.
    fn fill_form(&mut self, cx: &mut App) {
        let driver = self.current().cloned().unwrap_or_default();
        set_text(&self.name_input, driver.name, cx);
        set_text(&self.class_input, driver.class, cx);
        set_text(&self.url_template_input, driver.url_template, cx);
        set_text(
            &self.port_input,
            driver
                .default_port
                .map(|port| port.to_string())
                .unwrap_or_default(),
            cx,
        );
        set_text(&self.dialect_input, driver.dialect, cx);
        set_text(&self.maven_input, driver.maven.unwrap_or_default(), cx);
        set_text(&self.table_comments_input, driver.table_comments_sql, cx);
        set_text(&self.column_comments_input, driver.column_comments_sql, cx);
    }

    /// Reads the fields back into the selected driver.
    ///
    /// Called before every action that leaves the form — switching drivers,
    /// adding a JAR, saving — so that a half-typed edit is never silently lost.
    /// The JAR list is not a field and is written directly, so it is left alone
    /// here.
    fn collect(&mut self, cx: &App) {
        let Some(id) = self.selected.clone() else {
            return;
        };
        let Some(existing) = self.store.get(&id).cloned() else {
            return;
        };
        let port = text(&self.port_input, cx);
        self.store.upsert(DriverDef {
            name: text(&self.name_input, cx),
            class: text(&self.class_input, cx),
            url_template: text(&self.url_template_input, cx),
            // A blank port is a database with no port, which is what SQLite and
            // an embedded H2 are; it is not zero.
            default_port: port.parse::<u16>().ok().filter(|port| *port > 0),
            dialect: {
                let dialect = text(&self.dialect_input, cx);
                if dialect.is_empty() {
                    "generic".to_string()
                } else {
                    dialect
                }
            },
            maven: Some(text(&self.maven_input, cx)).filter(|text| !text.is_empty()),
            // The statements are kept whatever their flags say: a user who
            // turns a query off is switching it off, not throwing it away.
            table_comments_sql: text(&self.table_comments_input, cx),
            column_comments_sql: text(&self.column_comments_input, cx),
            ..existing
        });
    }

    /// Turns one of the two custom comment queries on or off.
    ///
    /// The flags are not text fields, so they are written straight into the
    /// store the way the JAR list is, rather than being read back by
    /// [`DriverManager::collect`].
    fn toggle_comments(&mut self, table: bool, enabled: bool, cx: &mut Context<Self>) {
        self.collect(cx);
        let Some(id) = self.selected.clone() else {
            return;
        };
        let Some(mut driver) = self.store.get(&id).cloned() else {
            return;
        };
        if table {
            driver.use_table_comments = enabled;
        } else {
            driver.use_column_comments = enabled;
        }
        self.store.upsert(driver);
        cx.notify();
    }

    /// Selects `id`, keeping whatever was typed into the driver being left.
    fn select(&mut self, id: String, cx: &mut Context<Self>) {
        self.collect(cx);
        self.confirming = false;
        self.status = None;
        // A chooser listing another driver's classes would be answered into
        // this one's class field.
        self.probe = None;
        self.selected = Some(id);
        self.fill_form(cx);
        cx.notify();
    }

    /// Adds a driver definition with nothing in it and selects it.
    fn new_driver(&mut self, cx: &mut Context<Self>) {
        self.collect(cx);
        self.probe = None;
        let id = unique_id("driver", &self.store);
        self.store.upsert(DriverDef {
            id: id.clone(),
            name: ts!("driver.new_name").to_string(),
            dialect: "generic".to_string(),
            ..DriverDef::default()
        });
        self.selected = Some(id);
        self.confirming = false;
        self.status = None;
        self.fill_form(cx);
        cx.notify();
    }

    /// Removes the selected driver, once confirmed.
    ///
    /// Profiles that name it are left alone: a profile whose driver has gone
    /// reports "no driver definition named …" when it is opened, which is a
    /// message the user can act on, whereas rewriting their profiles behind
    /// their back is not.
    fn delete_driver(&mut self, cx: &mut Context<Self>) {
        self.confirming = false;
        let Some(id) = self.selected.clone() else {
            return;
        };
        self.store.remove(&id);
        self.selected = self.store.drivers().first().map(|driver| driver.id.clone());
        self.fill_form(cx);
        self.persist(cx);
        cx.notify();
    }

    /// Writes `drivers.json` and tells the connection dialog to re-read it.
    fn persist(&mut self, cx: &mut Context<Self>) {
        self.collect(cx);
        if let Err(error) = self.store.save() {
            log::error!("could not write drivers.json: {error:#}");
            self.report(
                ts!("driver.save_failed", error = format!("{error:#}")),
                true,
                cx,
            );
            return;
        }
        cx.emit(DriverManagerEvent::Changed);
    }

    /// Saves and closes.
    fn save(&mut self, cx: &mut Context<Self>) {
        self.persist(cx);
        if !self.status_is_error {
            cx.emit(DriverManagerEvent::Dismissed);
        }
    }

    /// Puts a message under the form.
    fn report(&mut self, message: SharedString, is_error: bool, cx: &mut Context<Self>) {
        self.status = Some(message);
        self.status_is_error = is_error;
        cx.notify();
    }

    /// Asks the platform for JAR files and appends what it hands back.
    ///
    /// Nothing waits on the prompt: on X11 that call is exactly the one gpui had
    /// to be patched around, so the click returns immediately and the answer is
    /// picked up on a task of its own — the same shape the settings dialog's
    /// import uses.
    fn add_jars(&mut self, cx: &mut Context<Self>) {
        self.collect(cx);
        let paths = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: true,
            prompt: Some(ts!("driver.add_jar_select")),
        });

        cx.spawn(async move |manager, cx| {
            let chosen = match paths.await {
                Ok(Ok(Some(paths))) => paths,
                Ok(Ok(None)) | Err(_) => return,
                Ok(Err(error)) => {
                    log::warn!("the file picker could not be opened: {error:#}");
                    return;
                }
            };
            manager
                .update(cx, |manager, cx| manager.install_jars(chosen, cx))
                .ok();
        })
        .detach();
    }

    /// Appends `paths` to the selected driver's class path, in the order picked.
    ///
    /// Order is part of the identity of the loader the bridge caches: when two
    /// JARs carry the same class, the first one wins, so appending rather than
    /// sorting is what keeps a user-chosen precedence.
    fn install_jars(&mut self, paths: Vec<PathBuf>, cx: &mut Context<Self>) {
        let Some(id) = self.selected.clone() else {
            return;
        };
        let Some(mut driver) = self.store.get(&id).cloned() else {
            return;
        };
        for path in paths {
            if !driver.jars.contains(&path) {
                driver.jars.push(path);
            }
        }
        self.store.upsert(driver);
        self.status = None;
        self.persist(cx);
        cx.notify();
    }

    /// Drops one JAR from the selected driver's class path.
    fn remove_jar(&mut self, index: usize, cx: &mut Context<Self>) {
        let Some(id) = self.selected.clone() else {
            return;
        };
        let Some(mut driver) = self.store.get(&id).cloned() else {
            return;
        };
        if index >= driver.jars.len() {
            return;
        }
        driver.jars.remove(index);
        self.store.upsert(driver);
        self.persist(cx);
        cx.notify();
    }

    /// Fetches the selected driver's Maven coordinate into the drivers
    /// directory and adds the result to its class path.
    fn download(&mut self, cx: &mut Context<Self>) {
        self.collect(cx);
        if self.download.is_some() {
            return;
        }
        let coordinate = text(&self.maven_input, cx);
        let Some(coordinate) = Coordinate::parse(&coordinate) else {
            self.report(
                ts!("driver.bad_coordinate", coordinate = coordinate),
                true,
                cx,
            );
            return;
        };
        let directory = match drivers_dir() {
            Ok(directory) => directory,
            Err(error) => {
                self.report(
                    ts!("driver.save_failed", error = format!("{error:#}")),
                    true,
                    cx,
                );
                return;
            }
        };

        let cancel = Cancel::new();
        self.download = Some(Download {
            file_name: coordinate.file_name().into(),
            progress: Progress {
                received: 0,
                total: None,
            },
            cancel: cancel.clone(),
        });
        self.status = None;
        cx.notify();

        // The download blocks; the progress channel is what brings it back to
        // the window without the view ever waiting on it.
        let (reporter, mut reports) = mpsc::unbounded::<Progress>();
        let flag = cancel.clone();
        let fetch = cx.background_spawn(async move {
            maven::download(&coordinate, &directory, &flag, |progress| {
                // A closed receiver means the view is gone; the cancel flag is
                // what stops the transfer in that case, not this send.
                let _ = reporter.unbounded_send(progress);
            })
        });

        cx.spawn(async move |manager, cx| {
            // Drained on the same task as the result is awaited on, so a
            // progress report can never arrive after the outcome.
            let drain = async {
                while let Some(progress) = reports.next().await {
                    let updated = manager.update(cx, |manager, cx| {
                        if let Some(download) = manager.download.as_mut() {
                            download.progress = progress;
                            cx.notify();
                        }
                    });
                    if updated.is_err() {
                        return;
                    }
                }
            };
            let (outcome, ()) = futures::join!(fetch, drain);
            manager
                .update(cx, |manager, cx| manager.downloaded(outcome, cx))
                .ok();
        })
        .detach();
    }

    /// Takes the progress bar down and reports what the download produced.
    fn downloaded(&mut self, outcome: Result<PathBuf, DownloadError>, cx: &mut Context<Self>) {
        self.download = None;
        match outcome {
            Ok(path) => {
                let name = file_name_of(&path);
                self.install_jars(vec![path], cx);
                self.report(ts!("driver.downloaded", file = name), false, cx);
            }
            // A cancelled download is a thing the user did, not a failure, so it
            // is reported in the neutral colour.
            Err(DownloadError::Cancelled) => {
                self.report(ts!("driver.download_cancelled"), false, cx);
            }
            Err(error) => {
                let message = match error {
                    DownloadError::NotFound(url) => ts!("driver.not_found", url = url),
                    DownloadError::Network(message) => {
                        ts!("driver.network_failed", error = message)
                    }
                    DownloadError::Checksum { expected, actual } => {
                        ts!(
                            "driver.checksum_failed",
                            expected = expected,
                            actual = actual
                        )
                    }
                    DownloadError::Io(message) => ts!("driver.save_failed", error = message),
                    DownloadError::Cancelled => unreachable!("handled above"),
                };
                self.report(message, true, cx);
            }
        }
    }

    /// Raises the cancel flag of the download in flight.
    fn cancel_download(&mut self, cx: &mut Context<Self>) {
        if let Some(download) = self.download.as_ref() {
            download.cancel.cancel();
            cx.notify();
        }
    }

    /// `Escape`, one layer at a time.
    pub fn escape(&mut self, cx: &mut Context<Self>) {
        if self.confirming {
            self.confirming = false;
            cx.notify();
            return;
        }
        // Backing out of the class chooser — or of a probe still running —
        // undoes only that, not the whole form.
        if self.probe.take().is_some() {
            cx.notify();
            return;
        }
        if self.download.is_some() {
            self.cancel_download(cx);
            return;
        }
        cx.emit(DriverManagerEvent::Dismissed);
    }

    /// The two overlay bars, as they stand.
    fn scrollbar(&self, id: &'static str) -> Scrollbar {
        let (handle, state) = if id == LIST_SCROLLBAR {
            (&self.list_scroll, &self.list_scrollbar)
        } else {
            (&self.body_scroll, &self.body_scrollbar)
        };
        Scrollbar::for_handle(id, ScrollbarAxis::Vertical, handle).fade(state.fade())
    }

    /// The same bar, listening for the pointer reaching the edge it rides.
    ///
    /// Only the bars that are drawn need it: the ones the drag path builds are
    /// there to be measured, and never reach an element tree.
    fn hovering_scrollbar(&self, id: &'static str, cx: &mut Context<Self>) -> Scrollbar {
        let list = id == LIST_SCROLLBAR;
        self.scrollbar(id)
            .on_hover(cx.listener(move |manager, hovered: &bool, _window, cx| {
                manager.hover_scrollbar(list, *hovered, cx);
            }))
    }

    /// Puts each bar up when its surface has moved, and starts the clock.
    fn watch_scroll(&mut self, cx: &mut Context<Self>) {
        for list in [true, false] {
            let (handle, state) = if list {
                (&self.list_scroll, &mut self.list_scrollbar)
            } else {
                (&self.body_scroll, &mut self.body_scrollbar)
            };
            let moved = scrolled(handle, ScrollbarAxis::Vertical);
            if let Some(epoch) = state.moved(moved) {
                hide_later(epoch, cx, move |manager: &mut Self| {
                    Some(if list {
                        &mut manager.list_scrollbar
                    } else {
                        &mut manager.body_scrollbar
                    })
                });
            }
        }
    }

    /// Scrolls whichever bar's thumb is being dragged.
    fn drag_scrollbar(&mut self, event: &DragMoveEvent<DraggedThumb>, cx: &mut Context<Self>) {
        for id in [LIST_SCROLLBAR, BODY_SCROLLBAR] {
            let Some(progress) = self.scrollbar(id).dragged(event, cx) else {
                continue;
            };
            let list = id == LIST_SCROLLBAR;
            let (handle, state) = if list {
                (&self.list_scroll, &mut self.list_scrollbar)
            } else {
                (&self.body_scroll, &mut self.body_scrollbar)
            };
            state.hold();
            let handle = handle.clone();
            scroll_to(&handle, ScrollbarAxis::Vertical, progress);
            cx.notify();
            return;
        }
    }

    /// Lets go of whichever thumb was held.
    fn release_scrollbars(&mut self, cx: &mut Context<Self>) {
        for list in [true, false] {
            let state = if list {
                &mut self.list_scrollbar
            } else {
                &mut self.body_scrollbar
            };
            if let Some(epoch) = state.release() {
                hide_later(epoch, cx, move |manager: &mut Self| {
                    Some(if list {
                        &mut manager.list_scrollbar
                    } else {
                        &mut manager.body_scrollbar
                    })
                });
                cx.notify();
            }
        }
    }

    /// Puts one surface's bar up while the pointer rests on the edge it rides,
    /// and starts it going the moment the pointer leaves.
    ///
    /// Told which bar rather than asked to work it out: each strip senses only
    /// its own edge.
    fn hover_scrollbar(&mut self, list: bool, hovered: bool, cx: &mut Context<Self>) {
        let state = if list {
            &mut self.list_scrollbar
        } else {
            &mut self.body_scrollbar
        };
        if hovered {
            if state.hover_enter() {
                cx.notify();
            }
            return;
        }

        let Some(epoch) = state.hover_leave() else {
            return;
        };
        hide_now(self, epoch, cx, move |manager: &mut Self| {
            Some(if list {
                &mut manager.list_scrollbar
            } else {
                &mut manager.body_scrollbar
            })
        });
    }

    /// The list of driver definitions.
    fn render_list(&self, chrome: &Theme, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let this = cx.entity();
        let rows: Vec<_> = self
            .store
            .drivers()
            .iter()
            .enumerate()
            .map(|(index, driver)| {
                let id = driver.id.clone();
                let selected = self.selected.as_deref() == Some(driver.id.as_str());
                let name = if driver.name.is_empty() {
                    SharedString::from(driver.id.clone())
                } else {
                    SharedString::from(driver.name.clone())
                };
                // A driver with no JAR cannot open anything, and that is the one
                // fact about it worth carrying in a one-line row.
                let subtitle = if driver.jars.is_empty() {
                    ts!("driver.no_jar")
                } else {
                    ts!("driver.jar_count", count = driver.jars.len())
                };
                let this = this.clone();
                div()
                    .id(("driver-row", index))
                    .flex()
                    .flex_col()
                    .gap(px(1.))
                    .px(px(8.))
                    .py(px(5.))
                    .rounded_md()
                    .cursor_pointer()
                    .tab_index((tab::LIST + index as isize).min(tab::LIST_LIMIT))
                    .when(selected, |row| row.bg(chrome.surface_active))
                    .when(!selected, |row| {
                        row.hover(|row| row.bg(chrome.surface_hover))
                    })
                    .child(
                        div()
                            .text_size(px(12.))
                            .text_color(chrome.text)
                            .truncate()
                            .child(name),
                    )
                    .child(
                        div()
                            .text_size(px(10.))
                            .text_color(if driver.jars.is_empty() {
                                chrome.danger
                            } else {
                                chrome.text_muted
                            })
                            .truncate()
                            .child(subtitle),
                    )
                    .on_click(move |_, _window, cx| {
                        this.update(cx, |manager, cx| manager.select(id.clone(), cx));
                    })
            })
            .collect();

        div()
            .relative()
            .flex()
            .flex_col()
            .flex_none()
            .w(px(LIST_WIDTH))
            .min_h_0()
            .child(
                div()
                    .id("driver-list")
                    .track_scroll(&self.list_scroll)
                    .flex()
                    .flex_col()
                    .gap(px(2.))
                    .min_h_0()
                    .max_h(px(BODY_MAX_HEIGHT))
                    .overflow_y_scroll()
                    .children(rows),
            )
            .children(self.hovering_scrollbar(LIST_SCROLLBAR, cx).render(chrome))
    }

    /// The driver class field, its "detect" button, and whatever the last probe
    /// had to say.
    ///
    /// The button is held while the selected driver has no JAR: there is nothing
    /// to look inside of, and a button that can only report "add a JAR first" is
    /// a worse way of saying what the JAR row already says.
    fn render_class_row(&self, chrome: &Theme, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let this = cx.entity();
        let has_jars = self.current().is_some_and(|driver| !driver.jars.is_empty());
        let probing = matches!(self.probe, Some(Probe::Running { .. }));

        let detect = Button::new("driver-detect", ts!("driver.detect"))
            .variant(ButtonVariant::Secondary)
            .disabled(!has_jars || probing)
            .tab_index(tab::DETECT)
            .on_click({
                let this = this.clone();
                move |_, _window, cx| {
                    this.update(cx, |manager, cx| manager.detect_class(cx));
                }
            });

        // Starting the JVM is part of the first probe of a process and takes
        // seconds, so the wait is said out loud rather than left to a button
        // that has simply gone quiet.
        let running = probing.then(|| hint(ts!("driver.detecting"), cx));

        // More than one driver in the archive: the vendor's own
        // `META-INF/services` declaration heads the list and is preselected, and
        // the rest — internal and deprecated drivers, mostly — follow. Picking
        // one for the user silently is exactly what the scan cannot be trusted
        // to do.
        let choice = match &self.probe {
            Some(Probe::Choosing {
                candidates,
                recommended,
            }) => {
                let rows: Vec<_> = candidates
                    .iter()
                    .enumerate()
                    .map(|(index, class)| {
                        let this = this.clone();
                        let picked = class.clone();
                        let preferred = Some(class) == recommended.as_ref();
                        div()
                            .id(("driver-probe-choice", index))
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap(px(6.))
                            .px(px(8.))
                            .py(px(4.))
                            .rounded_md()
                            .cursor_pointer()
                            .tab_index(tab::PROBE_CHOICE + index as isize)
                            .when(preferred, |row| row.bg(chrome.surface_active))
                            .when(!preferred, |row| {
                                row.hover(|row| row.bg(chrome.surface_hover))
                            })
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .truncate()
                                    .text_size(px(11.))
                                    .text_color(chrome.text)
                                    .child(SharedString::from(class.clone())),
                            )
                            .when(preferred, |row| {
                                row.child(
                                    div()
                                        .flex_none()
                                        .text_size(px(10.))
                                        .text_color(chrome.accent)
                                        .child(ts!("driver.probe_recommended")),
                                )
                            })
                            .on_click(move |_, _window, cx| {
                                this.update(cx, |manager, cx| {
                                    manager.take_class(picked.clone(), cx);
                                });
                            })
                    })
                    .collect();

                Some(
                    div()
                        .flex()
                        .flex_col()
                        .gap(px(2.))
                        .child(hint(ts!("driver.probe_choose"), cx))
                        .children(rows),
                )
            }
            _ => None,
        };

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
                    .child(div().flex_1().min_w_0().child(self.class_input.clone()))
                    .child(detect),
            )
            .children(running)
            .children(choice)
            .child(hint(ts!("driver.class_hint"), cx))
    }

    /// Asks the bridge which `java.sql.Driver` implementations the selected
    /// driver's JARs hold.
    ///
    /// Everything about this blocks — the first probe of a process starts the
    /// JVM, and the scan then reads every entry of every archive — so it runs on
    /// a background task and the view only ever sees the answer.
    fn detect_class(&mut self, cx: &mut Context<Self>) {
        self.collect(cx);
        let jars = self
            .current()
            .map(|driver| driver.jars.clone())
            .unwrap_or_default();
        if jars.is_empty() || matches!(self.probe, Some(Probe::Running { .. })) {
            return;
        }

        let settings = app_settings::current(cx);
        self.status = None;
        cx.notify();

        let scan = cx.background_spawn(async move {
            // The same bootstrap the connection path uses, so a probe and the
            // connection that follows it run in one VM under one set of options.
            let jvm = connection::start_jvm(&settings).map_err(|error| match error {
                ConnectError::JvmStart(message) => ProbeFailure::Jvm(message),
                other => ProbeFailure::Other(other.message()),
            })?;
            jvm.probe_drivers(&jars).map_err(ProbeFailure::from)
        });

        let task = cx.spawn(async move |manager, cx| {
            let outcome = scan.await;
            manager
                .update(cx, |manager, cx| manager.probed(outcome, cx))
                .ok();
        });
        self.probe = Some(Probe::Running { _task: task });
    }

    /// Records what a probe found.
    ///
    /// The three failure modes get three different sentences on purpose. An
    /// archive with nothing in it is a *wrong file* — a sources or javadoc JAR,
    /// which is the mistake that is actually made; a damaged archive is a *broken
    /// download*; a path that is not there is a *moved file*. One "could not
    /// probe" for all three would leave the user guessing which.
    fn probed(&mut self, outcome: Result<DriverProbe, ProbeFailure>, cx: &mut Context<Self>) {
        self.probe = None;
        let probe = match outcome {
            Ok(probe) => probe,
            Err(failure) => {
                self.report(probe_message(&failure), true, cx);
                return;
            }
        };

        let candidates = candidates_of(&probe);
        match candidates.len() {
            0 => self.report(ts!("driver.probe_none"), true, cx),
            1 => self.take_class(candidates[0].clone(), cx),
            _ => {
                self.probe = Some(Probe::Choosing {
                    recommended: probe.recommended().map(str::to_owned),
                    candidates,
                });
                cx.notify();
            }
        }
    }

    /// Writes a probed class into the field and puts the chooser away.
    fn take_class(&mut self, class: String, cx: &mut Context<Self>) {
        self.probe = None;
        set_text(&self.class_input, class.clone(), cx);
        self.report(ts!("driver.probe_found", class = class), false, cx);
    }

    /// The class path: one row per JAR, plus the two ways to add one.
    fn render_jars(&self, chrome: &Theme, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let this = cx.entity();
        let jars = self
            .current()
            .map(|driver| driver.jars.clone())
            .unwrap_or_default();

        let rows: Vec<_> = jars
            .iter()
            .enumerate()
            .map(|(index, path)| {
                let this = this.clone();
                let missing = !path.is_file();
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(6.))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_size(px(11.))
                            .text_color(if missing { chrome.danger } else { chrome.text })
                            .child(SharedString::from(path.display().to_string())),
                    )
                    // A path in `drivers.json` that no longer exists is worth
                    // saying out loud: the failure it otherwise produces is a
                    // ClassNotFoundException, which points at the class rather
                    // than at the file that is missing.
                    .when(missing, |row| {
                        row.child(
                            div()
                                .flex_none()
                                .text_size(px(10.))
                                .text_color(chrome.danger)
                                .child(ts!("driver.jar_missing")),
                        )
                    })
                    .child(
                        Button::new(("driver-remove-jar", index), ts!("driver.remove_jar"))
                            .variant(ButtonVariant::Secondary)
                            .tab_index(tab::REMOVE_JAR + index as isize)
                            .on_click(move |_, _window, cx| {
                                this.update(cx, |manager, cx| manager.remove_jar(index, cx));
                            }),
                    )
            })
            .collect();

        let empty = jars.is_empty().then(|| hint(ts!("driver.no_jar_hint"), cx));
        let add = {
            let this = this.clone();
            Button::new("driver-add-jar", ts!("driver.add_jar"))
                .variant(ButtonVariant::Secondary)
                .tab_index(tab::ADD_JAR)
                .on_click(move |_, _window, cx| {
                    this.update(cx, |manager, cx| manager.add_jars(cx));
                })
        };

        div()
            .flex()
            .flex_col()
            .gap(px(6.))
            .children(rows)
            .children(empty)
            .child(add)
    }

    /// The Maven coordinate, its download button, and the progress of a fetch
    /// in flight.
    fn render_maven(&self, chrome: &Theme, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let this = cx.entity();
        let downloading = self.download.is_some();

        let button = {
            let this = this.clone();
            Button::new("driver-download", ts!("driver.download"))
                .variant(ButtonVariant::Secondary)
                .disabled(downloading)
                .tab_index(tab::DOWNLOAD)
                .on_click(move |_, _window, cx| {
                    this.update(cx, |manager, cx| manager.download(cx));
                })
        };

        let progress = self.download.as_ref().map(|download| {
            let fraction = download.progress.fraction();
            let label = match fraction {
                Some(fraction) => ts!(
                    "driver.downloading_percent",
                    file = download.file_name.clone(),
                    percent = (fraction * 100.).round() as u32
                ),
                None => ts!("driver.downloading", file = download.file_name.clone()),
            };
            let this = this.clone();

            div()
                .flex()
                .flex_col()
                .gap(px(5.))
                .child(
                    div()
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap(px(8.))
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .text_size(px(11.))
                                .text_color(chrome.text_muted)
                                .child(label),
                        )
                        .child(
                            Button::new("driver-cancel-download", ts!("common.cancel"))
                                .variant(ButtonVariant::Secondary)
                                .on_click(move |_, _window, cx| {
                                    this.update(cx, |manager, cx| manager.cancel_download(cx));
                                }),
                        ),
                )
                // A determinate bar when the server declared a length and a full
                // muted track when it did not: a bar that never moves would read
                // as a stalled download rather than an unknown one.
                .child(
                    div()
                        .w_full()
                        .h(px(4.))
                        .rounded_full()
                        .bg(chrome.surface_active)
                        .child(
                            div()
                                .h_full()
                                .rounded_full()
                                .bg(chrome.accent)
                                .w(gpui::relative(fraction.unwrap_or(1.))),
                        ),
                )
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
                    .child(div().flex_1().min_w_0().child(self.maven_input.clone()))
                    .child(button),
            )
            .children(progress)
    }

    /// One custom comment query: the box that switches it on, the statement,
    /// and the contract the statement has to keep.
    ///
    /// Inherited from jdbgen, and for the same reason: several products answer
    /// `DatabaseMetaData` with an empty `REMARKS` and keep their comments in a
    /// catalogue view instead, so the only way to see them is to name the query.
    /// The flag is separate from the text on purpose — turning a query off must
    /// not mean deleting it.
    ///
    /// The statement is a one-line field because that is the only text control
    /// this dialog has; a long query still fits, it just scrolls.
    fn render_comment_query(
        &self,
        table: bool,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let this = cx.entity();
        let enabled = self.current().is_some_and(|driver| {
            if table {
                driver.use_table_comments
            } else {
                driver.use_column_comments
            }
        });

        let (id, label, hint_key, index, input) = if table {
            (
                "driver-use-table-comments",
                ts!("driver.use_table_comments"),
                ts!("driver.table_comments_hint"),
                tab::USE_TABLE_COMMENTS,
                self.table_comments_input.clone(),
            )
        } else {
            (
                "driver-use-column-comments",
                ts!("driver.use_column_comments"),
                ts!("driver.column_comments_hint"),
                tab::USE_COLUMN_COMMENTS,
                self.column_comments_input.clone(),
            )
        };

        div()
            .flex()
            .flex_col()
            .gap(px(4.))
            .child(
                Checkbox::new(id, label)
                    .checked(enabled)
                    .tab_index(index)
                    .on_toggle(move |checked, _window, cx| {
                        this.update(cx, |manager, cx| {
                            manager.toggle_comments(table, checked, cx);
                        });
                    }),
            )
            .child(input)
            .child(hint(hint_key, cx))
    }

    /// The detail pane for the selected driver.
    fn render_details(&self, chrome: &Theme, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        if self.selected.is_none() {
            return div()
                .flex()
                .flex_1()
                .min_w_0()
                .items_center()
                .justify_center()
                .text_size(px(12.))
                .text_color(chrome.text_muted)
                .child(ts!("driver.none_selected"))
                .into_any_element();
        }

        let class_row = self.render_class_row(chrome, cx);
        let jars = self.render_jars(chrome, cx);
        let maven = self.render_maven(chrome, cx);
        let table_comments = self.render_comment_query(true, cx);
        let column_comments = self.render_comment_query(false, cx);

        div()
            .relative()
            .flex()
            .flex_col()
            .flex_1()
            .min_w_0()
            .min_h_0()
            .child(
                div()
                    .id("driver-body")
                    .track_scroll(&self.body_scroll)
                    .flex()
                    .flex_col()
                    .gap(px(10.))
                    .min_h_0()
                    .max_h(px(BODY_MAX_HEIGHT))
                    .overflow_y_scroll()
                    .child(form_row(ts!("driver.name"), self.name_input.clone()))
                    .child(form_row(ts!("driver.class"), class_row))
                    .child(form_row(
                        ts!("driver.url_template"),
                        self.url_template_input.clone(),
                    ))
                    .child(form_row(
                        ts!("driver.default_port"),
                        self.port_input.clone(),
                    ))
                    .child(form_row(ts!("driver.dialect"), self.dialect_input.clone()))
                    .child(form_row(ts!("driver.maven"), maven))
                    .child(form_row(ts!("driver.jars"), jars))
                    .child(form_row(ts!("driver.table_comments"), table_comments))
                    .child(form_row(ts!("driver.column_comments"), column_comments)),
            )
            .children(self.hovering_scrollbar(BODY_SCROLLBAR, cx).render(chrome))
            .into_any_element()
    }

    /// The message strip and the buttons.
    fn render_footer(&self, chrome: &Theme, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let this = cx.entity();
        let has_selection = self.selected.is_some();

        let status = self.status.clone().map(|message| {
            div()
                .text_size(px(11.))
                .text_color(if self.status_is_error {
                    chrome.danger
                } else {
                    chrome.success
                })
                .child(message)
        });

        let confirm = self.confirming.then(|| {
            let name = self
                .current()
                .map(|driver| driver.name.clone())
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
                        .child(ts!("driver.delete_confirm", name = name)),
                )
                .child(
                    Button::new("driver-delete-cancel", ts!("common.cancel"))
                        .variant(ButtonVariant::Secondary)
                        .on_click(move |_, _window, cx| {
                            cancel.update(cx, |manager, cx| {
                                manager.confirming = false;
                                cx.notify();
                            });
                        }),
                )
                .child(
                    Button::new("driver-delete-confirm", ts!("driver.delete"))
                        .variant(ButtonVariant::Danger)
                        .on_click(move |_, _window, cx| {
                            delete.update(cx, |manager, cx| manager.delete_driver(cx));
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
            .children(confirm)
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(8.))
                    .child(
                        Button::new("driver-new", ts!("driver.new"))
                            .variant(ButtonVariant::Secondary)
                            .tab_index(tab::NEW)
                            .on_click({
                                let this = this.clone();
                                move |_, _window, cx| {
                                    this.update(cx, |manager, cx| manager.new_driver(cx));
                                }
                            }),
                    )
                    .child(
                        Button::new("driver-delete", ts!("driver.delete"))
                            .variant(ButtonVariant::Secondary)
                            .disabled(!has_selection || self.confirming)
                            .tab_index(tab::DELETE)
                            .on_click({
                                let this = this.clone();
                                move |_, _window, cx| {
                                    this.update(cx, |manager, cx| {
                                        manager.confirming = true;
                                        cx.notify();
                                    });
                                }
                            }),
                    )
                    .child(div().flex_1())
                    .child(
                        Button::new("driver-close", ts!("common.close"))
                            .variant(ButtonVariant::Secondary)
                            .tab_index(tab::CLOSE)
                            .on_click({
                                let this = this.clone();
                                move |_, _window, cx| {
                                    this.update(cx, |_manager, cx| {
                                        cx.emit(DriverManagerEvent::Dismissed);
                                    });
                                }
                            }),
                    )
                    .child(
                        Button::new("driver-save", ts!("common.save"))
                            .variant(ButtonVariant::Primary)
                            .tab_index(tab::SAVE)
                            .on_click({
                                let this = this.clone();
                                move |_, _window, cx| {
                                    this.update(cx, |manager, cx| manager.save(cx));
                                }
                            }),
                    ),
            )
    }

    /// The manager's title, which the dialog puts in the modal's header.
    pub fn title(&self) -> SharedString {
        ts!("driver.title")
    }
}

impl EventEmitter<DriverManagerEvent> for DriverManager {}

impl Focusable for DriverManager {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for DriverManager {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.watch_scroll(cx);
        let chrome = theme(cx);
        let list = self.render_list(&chrome, cx);
        let details = self.render_details(&chrome, cx);
        let footer = self.render_footer(&chrome, cx);

        div()
            .id("driver-manager")
            .track_focus(&self.focus_handle)
            .flex()
            .flex_col()
            .min_h_0()
            .gap(px(12.))
            .on_drag_move::<DraggedThumb>(cx.listener(
                |manager, event: &DragMoveEvent<DraggedThumb>, _window, cx| {
                    manager.drag_scrollbar(event, cx);
                },
            ))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|manager, _: &MouseUpEvent, _window, cx| {
                    manager.release_scrollbars(cx);
                }),
            )
            .on_mouse_up_out(
                MouseButton::Left,
                cx.listener(|manager, _: &MouseUpEvent, _window, cx| {
                    manager.release_scrollbars(cx);
                }),
            )
            .child(
                div()
                    .flex()
                    .flex_row()
                    .min_h_0()
                    .gap(px(12.))
                    .child(list)
                    .child(details),
            )
            .child(footer)
    }
}

/// An id nothing in `store` answers to yet.
fn unique_id(prefix: &str, store: &DriverStore) -> String {
    let mut index = 1;
    loop {
        let candidate = format!("{prefix}-{index}");
        if store.get(&candidate).is_none() {
            return candidate;
        }
        index += 1;
    }
}

/// The last component of a path, for a message that names a file.
fn file_name_of(path: &std::path::Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string())
}

/// A muted paragraph under a control.
fn hint(text: SharedString, cx: &App) -> impl IntoElement + use<> {
    let chrome = theme(cx);
    div()
        .text_size(px(11.))
        .text_color(chrome.text_muted)
        .child(text)
}

/// Trimmed content of `input`.
fn text(input: &Entity<TextInput>, cx: &App) -> String {
    input.read(cx).content().trim().to_owned()
}

/// Replaces the contents of `input`.
fn set_text(input: &Entity<TextInput>, value: impl Into<SharedString>, cx: &mut App) {
    input.update(cx, |input, cx| input.set_content(value, cx));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_generated_driver_id_never_collides() {
        let mut store = DriverStore::default();
        assert_eq!(unique_id("driver", &store), "driver-1");
        store.upsert(DriverDef {
            id: "driver-1".into(),
            ..DriverDef::default()
        });
        assert_eq!(unique_id("driver", &store), "driver-2");
    }

    /// The two custom comment queries survive the trip through the form.
    ///
    /// The box and the statement are two separate decisions, and the form has
    /// to keep both: turning a query off must leave the text where it was, so
    /// that turning it back on does not mean typing it again.
    #[gpui::test]
    fn the_comment_queries_and_their_boxes_round_trip_through_the_form(
        cx: &mut gpui::TestAppContext,
    ) {
        let manager = manager_over(Vec::new(), cx);
        let query = "SELECT TABLE_NAME, 'x' FROM META WHERE OWNER = '${schema}'";

        cx.update(|cx| {
            manager.update(cx, |manager, cx| {
                set_text(&manager.table_comments_input, query, cx);
                manager.toggle_comments(true, true, cx);
            });
        });
        cx.update(|cx| {
            let driver = manager.read(cx).current().expect("H2 is selected");
            assert!(driver.use_table_comments);
            assert_eq!(driver.table_comments_query(), Some(query));
            // The column query was never touched and stays off and empty.
            assert!(!driver.use_column_comments);
            assert!(driver.column_comments_sql.is_empty());
        });

        // Switching it off keeps the statement.
        cx.update(|cx| {
            manager.update(cx, |manager, cx| manager.toggle_comments(true, false, cx));
        });
        cx.update(|cx| {
            let driver = manager.read(cx).current().expect("H2 is selected");
            assert!(!driver.use_table_comments);
            assert_eq!(driver.table_comments_sql, query);
            assert_eq!(driver.table_comments_query(), None);
        });

        // And leaving the driver and coming back shows what was typed.
        cx.update(|cx| {
            manager.update(cx, |manager, cx| {
                manager.select("postgresql".to_string(), cx);
                manager.select("h2".to_string(), cx);
            });
        });
        cx.update(|cx| {
            assert_eq!(
                manager.read(cx).table_comments_input.read(cx).content(),
                query
            );
        });
    }

    /// A throwaway file under the temp directory, removed with the test.
    struct TempFile(PathBuf);

    impl TempFile {
        fn new(name: &str, contents: &[u8]) -> TempFile {
            let path = std::env::temp_dir().join(format!("rudbman-app-probe-{name}"));
            std::fs::write(&path, contents).expect("the temp directory is writable");
            TempFile(path)
        }
    }

    impl Drop for TempFile {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }

    /// The 22 bytes of an archive with no entries at all: a valid JAR holding no
    /// driver, which is what a sources or javadoc archive looks like here.
    const EMPTY_ZIP: [u8; 22] = [
        0x50, 0x4b, 0x05, 0x06, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
    ];

    /// Probes `jars` through the real JVM and the real bridge.
    fn probe(jars: &[PathBuf]) -> Result<DriverProbe, ProbeFailure> {
        let jvm = connection::start_jvm(&rudbman_core::AppSettings::default())
            .expect("the JVM starts; build the bridge with `cd bridge && ./gradlew jar`");
        jvm.probe_drivers(jars).map_err(ProbeFailure::from)
    }

    /// A manager whose selected driver is H2 with `jars` and no class name.
    fn manager_over(
        jars: Vec<PathBuf>,
        cx: &mut gpui::TestAppContext,
    ) -> gpui::Entity<DriverManager> {
        cx.update(|cx| {
            app_settings::init(cx);
            rudbman_ui::init(cx);
        });
        let manager = cx.new(DriverManager::new);
        cx.update(|cx| {
            manager.update(cx, |manager, cx| {
                // The built-in definitions rather than whatever is on this
                // machine, so the test says the same thing everywhere.
                manager.store = DriverStore::default();
                let mut driver = manager.store.get("h2").cloned().expect("built-in H2");
                driver.jars = jars;
                driver.class = String::new();
                manager.store.upsert(driver);
                manager.selected = Some("h2".to_string());
                manager.fill_form(cx);
            });
        });
        manager
    }

    /// What the manager is currently showing under the form.
    fn status(
        manager: &gpui::Entity<DriverManager>,
        cx: &mut gpui::TestAppContext,
    ) -> (SharedString, bool) {
        cx.update(|cx| {
            let manager = manager.read(cx);
            (
                manager.status.clone().unwrap_or_default(),
                manager.status_is_error,
            )
        })
    }

    /// The whole point of the button, against the real H2 archive: the user
    /// registers a JAR and is told the class name instead of having to find it.
    #[gpui::test]
    fn detecting_the_class_of_a_real_driver_jar_fills_the_field(cx: &mut gpui::TestAppContext) {
        let jar = crate::connection::h2::jar();
        let manager = manager_over(vec![jar.clone()], cx);
        let probed = probe(&[jar]);

        cx.update(|cx| {
            manager.update(cx, |manager, cx| manager.probed(probed, cx));
        });

        cx.update(|cx| {
            let manager = manager.read(cx);
            assert_eq!(
                manager.class_input.read(cx).content(),
                "org.h2.Driver",
                "the declared service is what lands in the field"
            );
            // H2 ships exactly one driver, so there is nothing to choose.
            assert!(manager.probe.is_none());
        });
        let (message, is_error) = status(&manager, cx);
        assert!(!is_error, "{message}");
        assert!(message.contains("org.h2.Driver"), "{message}");
    }

    /// A sources or javadoc JAR: a perfectly good archive with no driver in it.
    /// Not an error from the bridge, and a sentence of its own here.
    #[gpui::test]
    fn an_archive_with_no_driver_says_so_rather_than_failing(cx: &mut gpui::TestAppContext) {
        let jar = TempFile::new("empty.jar", &EMPTY_ZIP);
        let manager = manager_over(vec![jar.0.clone()], cx);
        let probed = probe(std::slice::from_ref(&jar.0));
        assert!(
            probed.is_ok(),
            "an archive without a driver is not a failure"
        );

        cx.update(|cx| {
            manager.update(cx, |manager, cx| manager.probed(probed, cx));
        });

        let (message, is_error) = status(&manager, cx);
        assert!(is_error);
        assert_eq!(message, ts!("driver.probe_none"));
        // Nothing was written into the field on the way past.
        cx.update(|cx| {
            assert_eq!(manager.read(cx).class_input.read(cx).content(), "");
        });
    }

    /// A half-written download: the archive starts reading and then fails. It
    /// must not be reported as "no driver in it" — the file is right, the bytes
    /// are not, and the fix is to fetch it again.
    #[gpui::test]
    fn a_damaged_archive_is_reported_as_a_broken_download(cx: &mut gpui::TestAppContext) {
        let whole = std::fs::read(crate::connection::h2::jar()).expect("the H2 jar is readable");
        let jar = TempFile::new("truncated.jar", &whole[..whole.len() / 2]);
        let manager = manager_over(vec![jar.0.clone()], cx);

        let probed = probe(std::slice::from_ref(&jar.0));
        let Err(ProbeFailure::Damaged(reported)) = &probed else {
            panic!("the bridge answers `io` for a truncated archive, got {probed:?}");
        };
        let expected = ts!("driver.probe_damaged", error = reported.clone());

        cx.update(|cx| {
            manager.update(cx, |manager, cx| manager.probed(probed, cx));
        });

        let (message, is_error) = status(&manager, cx);
        assert!(is_error);
        assert_eq!(message, expected);
        assert_ne!(
            message,
            ts!("driver.probe_none"),
            "a broken file is not an empty one"
        );
    }

    /// A path the definition names and the disk does not have.
    #[gpui::test]
    fn a_jar_that_is_not_there_names_the_file_rather_than_the_archive(
        cx: &mut gpui::TestAppContext,
    ) {
        let missing = PathBuf::from("/nonexistent/rudbman/nope.jar");
        let manager = manager_over(vec![missing.clone()], cx);
        let probed = probe(&[missing]);
        assert!(
            matches!(probed, Err(ProbeFailure::Missing(_))),
            "expected a driver error"
        );

        cx.update(|cx| {
            manager.update(cx, |manager, cx| manager.probed(probed, cx));
        });
        let (message, is_error) = status(&manager, cx);
        assert!(is_error);
        assert!(message.contains("nope.jar"), "{message}");
    }

    /// Several drivers in one archive: nothing is chosen for the user, and the
    /// vendor's own declaration is what comes up preselected.
    #[gpui::test]
    fn several_drivers_are_offered_rather_than_guessed_between(cx: &mut gpui::TestAppContext) {
        let manager = manager_over(vec![PathBuf::from("/tmp/whatever.jar")], cx);
        // The shape a driver JAR with an internal second driver has: the scan
        // finds both, the declaration names one.
        let probed = DriverProbe {
            classes: vec![
                "com.example.LegacyDriver".to_string(),
                "com.example.Driver".to_string(),
            ],
            services: vec!["com.example.Driver".to_string()],
        };

        cx.update(|cx| {
            manager.update(cx, |manager, cx| manager.probed(Ok(probed), cx));
        });

        cx.update(|cx| {
            let manager = manager.read(cx);
            let Some(Probe::Choosing {
                candidates,
                recommended,
            }) = &manager.probe
            else {
                panic!("two drivers must be offered, not picked between")
            };
            // The declaration heads the list and is the default.
            assert_eq!(
                candidates,
                &vec![
                    "com.example.Driver".to_string(),
                    "com.example.LegacyDriver".to_string()
                ]
            );
            assert_eq!(recommended.as_deref(), Some("com.example.Driver"));
            // And nothing has been written into the field yet.
            assert_eq!(manager.class_input.read(cx).content(), "");
        });

        // Picking one fills the field and puts the chooser away.
        cx.update(|cx| {
            manager.update(cx, |manager, cx| {
                manager.take_class("com.example.LegacyDriver".to_string(), cx);
            });
        });
        cx.update(|cx| {
            let manager = manager.read(cx);
            assert_eq!(
                manager.class_input.read(cx).content(),
                "com.example.LegacyDriver"
            );
            assert!(manager.probe.is_none());
        });
    }

    #[test]
    fn the_declared_service_heads_the_candidate_list_and_duplicates_collapse() {
        let probe = DriverProbe {
            classes: vec!["a.B".to_string(), "a.C".to_string()],
            services: vec!["a.C".to_string()],
        };
        assert_eq!(
            candidates_of(&probe),
            vec!["a.C".to_string(), "a.B".to_string()],
            "the vendor's own declaration comes first and is not repeated"
        );

        // No declaration: the scan order stands.
        let scanned = DriverProbe {
            classes: vec!["a.B".to_string(), "a.C".to_string()],
            services: Vec::new(),
        };
        assert_eq!(
            candidates_of(&scanned),
            vec!["a.B".to_string(), "a.C".to_string()]
        );

        assert!(
            candidates_of(&DriverProbe {
                classes: Vec::new(),
                services: Vec::new()
            })
            .is_empty()
        );
    }

    #[test]
    fn the_four_probe_failures_read_as_four_different_things() {
        // Each names a different fix; one sentence for all four would leave the
        // user guessing which of them they are looking at.
        let messages = [
            probe_message(&ProbeFailure::Jvm("no runtime".into())),
            probe_message(&ProbeFailure::Missing("/nope.jar".into())),
            probe_message(&ProbeFailure::Damaged("unexpected end".into())),
            probe_message(&ProbeFailure::Other("something".into())),
        ];
        for message in &messages {
            assert!(!message.is_empty());
            assert!(!message.contains("driver."), "untranslated: {message:?}");
        }
        let mut unique: Vec<&str> = messages.iter().map(SharedString::as_ref).collect();
        unique.sort_unstable();
        unique.dedup();
        assert_eq!(unique.len(), 4, "two failures read the same: {messages:?}");
        // And none of them reads like the empty-archive case, which is not a
        // failure at all.
        assert!(!unique.contains(&ts!("driver.probe_none").as_ref()));
    }

    #[test]
    fn every_label_the_manager_draws_has_a_translation() {
        // `t!` answers with the key path when a key is missing, so a typo
        // reaches the screen as "driver.nmae".
        for label in [
            ts!("driver.title"),
            ts!("driver.name"),
            ts!("driver.class"),
            ts!("driver.class_hint"),
            ts!("driver.url_template"),
            ts!("driver.default_port"),
            ts!("driver.dialect"),
            ts!("driver.maven"),
            ts!("driver.jars"),
            ts!("driver.add_jar"),
            ts!("driver.add_jar_select"),
            ts!("driver.remove_jar"),
            ts!("driver.jar_missing"),
            ts!("driver.no_jar"),
            ts!("driver.no_jar_hint"),
            ts!("driver.jar_count", count = 2),
            ts!("driver.download"),
            ts!("driver.downloading", file = "x.jar"),
            ts!("driver.downloading_percent", file = "x.jar", percent = 40),
            ts!("driver.downloaded", file = "x.jar"),
            ts!("driver.download_cancelled"),
            ts!("driver.bad_coordinate", coordinate = "x"),
            ts!("driver.not_found", url = "u"),
            ts!("driver.network_failed", error = "e"),
            ts!("driver.checksum_failed", expected = "a", actual = "b"),
            ts!("driver.load_failed", error = "e"),
            ts!("driver.save_failed", error = "e"),
            ts!("driver.new"),
            ts!("driver.new_name"),
            ts!("driver.delete"),
            ts!("driver.delete_confirm", name = "X"),
            ts!("driver.none_selected"),
            ts!("driver.detect"),
            ts!("driver.detecting"),
            ts!("driver.probe_found", class = "org.h2.Driver"),
            ts!("driver.probe_choose"),
            ts!("driver.probe_recommended"),
            ts!("driver.probe_none"),
            ts!("driver.probe_damaged", error = "e"),
            ts!("driver.probe_missing", error = "e"),
            ts!("driver.probe_jvm_failed", error = "e"),
            ts!("driver.probe_failed", error = "e"),
            ts!("driver.table_comments"),
            ts!("driver.use_table_comments"),
            ts!("driver.table_comments_hint"),
            ts!("driver.column_comments"),
            ts!("driver.use_column_comments"),
            ts!("driver.column_comments_hint"),
        ] {
            assert!(!label.is_empty(), "empty label");
            assert!(
                !label.starts_with("driver."),
                "untranslated label {label:?}"
            );
        }
    }
}
