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
//! Nothing here loads a driver class. Probing a JAR for the driver classes
//! inside it is a bridge operation (`PROBE_DRIVER`, `0x50`), and this build of
//! `rudbman-jdbc` offers no way to reach it without an open session — see the
//! note on [`DriverManager::render_class_row`].
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
    MouseButton, MouseUpEvent, PathPromptOptions, Render, ScrollHandle, SharedString, Window, div,
    prelude::*, px,
};
use rudbman_core::{DriverDef, DriverStore, drivers_dir};
use rudbman_ui::{
    Button, ButtonVariant, DraggedThumb, Scrollbar, ScrollbarAxis, ScrollbarState, TextInput,
    Theme, form_row, hide_later, scroll_to, scrolled, theme,
};

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
            ..existing
        });
    }

    /// Selects `id`, keeping whatever was typed into the driver being left.
    fn select(&mut self, id: String, cx: &mut Context<Self>) {
        self.collect(cx);
        self.confirming = false;
        self.status = None;
        self.selected = Some(id);
        self.fill_form(cx);
        cx.notify();
    }

    /// Adds a driver definition with nothing in it and selects it.
    fn new_driver(&mut self, cx: &mut Context<Self>) {
        self.collect(cx);
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
            .children(self.scrollbar(LIST_SCROLLBAR).render(chrome))
    }

    /// The driver class field.
    ///
    /// There is no "detect it from the JAR" button, and its absence is
    /// deliberate rather than an oversight: the bridge implements
    /// `PROBE_DRIVER` (`0x50`), which scans a JAR for `java.sql.Driver`
    /// implementations without initialising any of them, but this build of
    /// `rudbman-jdbc` exposes no way to invoke an operation without an open
    /// session — `Session::call_raw` goes through a session's worker thread, and
    /// `Jvm::call_detached` is crate-private. Probing happens *before* there is
    /// a session, so until the JNI layer publishes an entry point for it the
    /// class is typed in, with the hint below naming the two places it is
    /// written down in every driver's own documentation.
    fn render_class_row(&self, cx: &App) -> impl IntoElement + use<> {
        div()
            .flex()
            .flex_col()
            .gap(px(4.))
            .child(self.class_input.clone())
            .child(hint(ts!("driver.class_hint"), cx))
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

        let class_row = self.render_class_row(cx);
        let jars = self.render_jars(chrome, cx);
        let maven = self.render_maven(chrome, cx);

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
                    .child(form_row(ts!("driver.jars"), jars)),
            )
            .children(self.scrollbar(BODY_SCROLLBAR).render(chrome))
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
        ] {
            assert!(!label.is_empty(), "empty label");
            assert!(!label.contains("driver."), "untranslated label {label:?}");
        }
    }
}
