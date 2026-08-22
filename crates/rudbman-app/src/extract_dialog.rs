//! The script extraction dialog: one explorer object, out to a file.
//!
//! Opened over the object selected in the explorer ([`crate::QueryObject`]'s
//! neighbour in the menu), it collects what the bridge's `JOB_START` needs —
//! an output file, whether the schema goes in, whether the rows go in and in
//! what shape — starts the job, and then turns into a progress card until the
//! job ends one way or another.
//!
//! # Two halves, one entity
//!
//! The dialog is a form *and* a progress card, in that order, and never both:
//! [`Stage`] is which one is on screen. Splitting them into two entities was
//! the first shape tried and reads worse — the object name, the output path and
//! the buttons are the same three rows in either half, and the transition
//! between them is not a navigation the user can go back through.
//!
//! # Where the job lives
//!
//! [`Job`] is not [`Clone`] and [`Job::poll`] takes `&mut self`, so the poll
//! loop has to own it; [`Job::cancel`] takes `&self` and the cancel button is on
//! the other side of the entity boundary. The job is therefore shared as an
//! `Arc<Mutex<Job>>`: the polling task locks it for the length of one reading
//! and the cancel path locks it for the length of one detached cancel. Neither
//! is a long hold — a poll reads a handful of counters the bridge keeps as
//! atomics, and a cancel sets a flag — so the contention is a JNI round trip at
//! worst.
//!
//! The alternative shapes were both worse. An `AtomicBool` the poll loop watches
//! would only cancel at the *next* poll, up to a fifth of a second after the
//! click and never at all while a poll is blocked; a channel is the same thing
//! with more parts. What makes the mutex right is that
//! [`Job::cancel`](rudbman_jdbc::Job::cancel) is detached — it does **not** go
//! through the session's worker — so it lands even while the worker is stuck
//! behind the connection lock the job itself holds.
//!
//! # Closing cancels
//!
//! Dropping the polling task drops the last `Arc`, and `Job`'s own `Drop`
//! cancels an unfinished job. That is what makes closing the window, closing the
//! dialog and pressing `Escape` all safe: none of them can leak a job that goes
//! on writing to a file nobody is watching. `Escape` while a job runs is
//! deliberately *not* a close — it is the cancel button, and the card stays up
//! until the job reports a terminal state, because a partial file the user does
//! not know about is worse than one more click.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use gpui::{
    App, Context, Entity, EventEmitter, FocusHandle, Focusable, IntoElement, PathPromptOptions,
    Render, SharedString, Task, Window, div, prelude::*, px,
};
use parking_lot::Mutex;
use rudbman_jdbc::{
    Constraints, DataMode, DataOptions, DdlOptions, ExtractSpec, Job, JobProgress, JobState,
    ObjectRef,
};
use rudbman_ui::{Button, ButtonVariant, Checkbox, Select, TextInput, form_row, modal, theme};

use crate::connection::SessionHandle;
use crate::explorer::ObjectTarget;
use crate::i18n::ts;

/// Width of the dialog panel.
///
/// Between the about box and the settings dialog: the form is one column of
/// label and control, but the control is sometimes a file path.
const DIALOG_WIDTH: f32 = 520.;

/// How often a running job is asked how it is doing.
///
/// The interval the architecture document (§6) names. Each reading is a JNI
/// round trip, so this is the frame rate of the progress card and not a
/// sampling rate: nothing accumulates between polls that a poll could miss.
///
/// Shared with the transfer and backup dialogs: the three cards poll the same
/// `JOB_POLL` and a card that sampled at its own rate would only be a different
/// frame rate for the same work.
pub const POLL_INTERVAL: Duration = Duration::from_millis(200);

/// Rows one `INSERT` carries when the field is empty or unreadable.
///
/// One, because that is the only portable value — Oracle has no multi-row
/// `VALUES` clause and the bridge clamps the batch back to one there anyway.
const DEFAULT_BATCH_ROWS: u32 = 1;

/// Tab order of the form, spaced so controls can be inserted without
/// renumbering.
mod tab {
    /// "Browse…" beside the output path.
    pub const OUTPUT: isize = 10;
    /// "Include the schema".
    pub const DDL: isize = 20;
    /// "Precede it with DROP statements".
    pub const DROP: isize = 21;
    /// "Include the rows".
    pub const DATA: isize = 30;
    /// The row format dropdown.
    pub const MODE: isize = 31;
    /// Rows per `INSERT`.
    pub const BATCH_ROWS: isize = 32;
    /// "Browse…" beside the template path.
    pub const TEMPLATE: isize = 33;
    /// The `WHERE` clause.
    pub const WHERE: isize = 40;
    /// "Extract".
    pub const START: isize = 50;
    /// "Cancel" / "Close".
    pub const DISMISS: isize = 51;
}

/// The fixed parts of one "path, and a button that picks it" row.
///
/// Three of the five members are only ever two sets of constants — the output
/// file's and the template file's — so they travel as one value rather than as
/// five parameters of a render method.
struct PathRow {
    /// The row's label.
    label: SharedString,
    /// What the cell says while no path has been picked.
    placeholder: SharedString,
    /// Element id of the button.
    id: &'static str,
    /// Where the button sits in the tab ring.
    tab_index: isize,
    /// What the button does; one of the dialog's two pickers.
    pick: fn(&mut ExtractDialog, &mut Context<ExtractDialog>),
}

/// Emitted by [`ExtractDialog`] when the user closes it.
pub enum ExtractDialogEvent {
    /// The dialog was dismissed; the shell should restore focus.
    Dismissed,
}

/// The row formats offered, in the order the dropdown lists them.
///
/// The labels are translated and the order is this array's, so the dropdown's
/// `on_select` maps an index back through here rather than comparing labels —
/// which would only work in one language.
const MODES: [DataMode; 3] = [DataMode::Insert, DataMode::Csv, DataMode::Template];

/// Everything the form collects, apart from the object it was opened over.
///
/// Pulled out of the widgets into a plain value so that
/// [`build_spec`] — the one piece of this module with a rule in it — is a pure
/// function of two arguments and can be tested without a window.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExtractForm {
    /// Where the script goes. `None` until the user has picked a file.
    pub output: Option<PathBuf>,
    /// Whether the schema is written.
    pub ddl: bool,
    /// Whether `DROP` statements precede the `CREATE`s.
    pub include_drop: bool,
    /// Whether the rows are written.
    pub data: bool,
    /// The row format.
    pub mode: DataMode,
    /// How many rows one `INSERT` carries. [`DataMode::Insert`] only.
    pub batch_rows: u32,
    /// The template file. [`DataMode::Template`] only.
    pub template: Option<PathBuf>,
    /// A `WHERE` clause, without the keyword. Blank for none.
    pub where_clause: String,
}

impl Default for ExtractForm {
    /// What the dialog opens with: a script with the schema and the rows in it,
    /// one `INSERT` per row, and no file chosen yet.
    fn default() -> Self {
        ExtractForm {
            output: None,
            ddl: true,
            include_drop: false,
            data: true,
            mode: DataMode::Insert,
            batch_rows: DEFAULT_BATCH_ROWS,
            template: None,
            where_clause: String::new(),
        }
    }
}

/// The specification `form` describes for `target`, or `None` when the form is
/// not ready to be sent.
///
/// Not ready means one of exactly two things, and both are what the "Extract"
/// button is disabled on: no output file, or neither the schema nor the rows
/// asked for — a job that would write an empty file. Everything else the bridge
/// judges, because the bridge is the single authority on what a malformed
/// request is (see [`Session::start_job`](rudbman_jdbc::Session::start_job)):
/// a template mode with no template file gets sent and comes straight back as
/// a `protocol` error, which is shown in the dialog.
pub fn build_spec(target: &ObjectTarget, form: &ExtractForm) -> Option<ExtractSpec> {
    let output = form.output.as_ref()?;
    if !form.ddl && !form.data {
        return None;
    }

    let mut object = ObjectRef::new(target.name.clone());
    if let Some(catalog) = non_empty(target.catalog.as_deref()) {
        object = object.with_catalog(catalog);
    }
    if let Some(schema) = non_empty(target.schema.as_deref()) {
        object = object.with_schema(schema);
    }

    // `ExtractSpec::new` already builds the `OutputSpec`, and its defaults —
    // UTF-8 and `\n` — are the two the dialog does not offer: a SQL script read
    // back by this same application has no reason to be anything else, and a
    // charset picker in a script dialogue is a way to write a file that looks
    // fine until someone replays it.
    let mut spec = ExtractSpec::new(output).with_object(object);

    if form.ddl {
        spec = spec.with_ddl(
            DdlOptions::included()
                .with_drop(form.include_drop)
                // Always `Alter`, and deliberately not offered: the point of
                // this dialogue is a script that can be replayed, and two
                // tables that reference each other cannot be created in any
                // order with their keys inline. `Inline` would be the faithful
                // rendering of one table's own DDL, which is what the detail
                // panel already shows.
                .with_constraints(Constraints::Alter),
        );
    }

    if form.data {
        let mut data = DataOptions::included(form.mode);
        match form.mode {
            DataMode::Insert => data = data.with_insert_batch_rows(form.batch_rows.max(1)),
            DataMode::Template => {
                if let Some(template) = &form.template {
                    data = data.with_template_path(template);
                }
            }
            DataMode::Csv => {}
        }
        let predicate = form.where_clause.trim();
        if !predicate.is_empty() {
            data = data.with_where(predicate);
        }
        spec = spec.with_data(data);
    }

    Some(spec)
}

/// `value` when it holds something other than whitespace.
///
/// Shared with the transfer and backup dialogs, which have the same question to
/// ask of a schema name that may be absent, may be blank, and means the same
/// thing either way.
pub fn non_empty(value: Option<&str>) -> Option<&str> {
    value.filter(|text| !text.trim().is_empty())
}

/// Renders a byte count the way a file manager does.
///
/// The unit symbols are not translated, for the same reason the licence
/// identifier is not: `kB` is a symbol, not a word, and the decimal multiples
/// are what every file manager on every platform shows.
///
/// Shared with the backup dialog, whose progress card counts the same bytes.
pub fn format_bytes(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["kB", "MB", "GB", "TB"];
    if bytes < 1000 {
        return format!("{bytes} B");
    }
    let mut value = bytes as f64 / 1000.;
    let mut unit = 0;
    while value >= 1000. && unit + 1 < UNITS.len() {
        value /= 1000.;
        unit += 1;
    }
    format!("{value:.1} {}", UNITS[unit])
}

/// Which half of the dialog is on screen.
enum Stage {
    /// Collecting the form.
    Form,
    /// A job is running.
    Running(Running),
    /// The job ended, and the card says how.
    Ended(Ended),
}

/// A job in flight, and the last reading taken of it.
///
/// The counters are held one by one rather than as a [`JobProgress`]: that type
/// is `#[non_exhaustive]`, so this crate cannot build the "nothing has been read
/// yet" value the card needs between the start and the first poll.
struct Running {
    /// Shared with the polling task; see the module documentation.
    job: Arc<Mutex<Job>>,
    /// The bridge's own phase text, or empty before the first reading.
    phase: SharedString,
    /// Rows written so far.
    rows_done: u64,
    /// Bytes written so far.
    bytes: u64,
    /// Whether a cancel has been issued and the job has not stopped yet.
    cancelling: bool,
}

/// How a job ended.
enum Ended {
    /// Everything asked for was written.
    Done {
        /// Rows written.
        rows: u64,
        /// Bytes written.
        bytes: u64,
    },
    /// The job failed. The message is the first error's, as the bridge wrote it.
    Failed(SharedString),
    /// The job was cancelled. The partial file is left where it is.
    Cancelled,
}

/// Modal dialog that extracts one object to a script file.
///
/// Create it once with [`ExtractDialog::new`], keep the handle, subscribe to
/// [`ExtractDialogEvent`], and render it as the last child of a `relative()`
/// root. It renders nothing while [`ExtractDialog::is_open`] is `false`, so it
/// is safe to render unconditionally.
pub struct ExtractDialog {
    /// Whether the dialog is currently visible.
    open: bool,
    /// Focus of the dialog root; the anchor the shell's `Escape` resolves
    /// against.
    focus_handle: FocusHandle,
    /// Whether focus should move into the dialog on the next render.
    pending_focus: bool,
    /// The object the dialog was opened over, captured at that moment: the
    /// explorer's selection may move while the dialog is up, and a job started
    /// from this card must be the one the user was looking at.
    target: Option<ObjectTarget>,
    /// The session the job runs on.
    ///
    /// Held for as long as the dialog is open — that is what a
    /// [`SessionHandle`] is for. A connection tab closed mid-extraction leaves
    /// the session (and the tunnel under it) standing until this is dropped.
    session: Option<SessionHandle>,
    /// Where the script goes.
    output: Option<PathBuf>,
    /// Whether the schema is written.
    ddl: bool,
    /// Whether `DROP` statements precede the `CREATE`s.
    include_drop: bool,
    /// Whether the rows are written.
    data: bool,
    /// The row format.
    mode: DataMode,
    /// The template file, for [`DataMode::Template`].
    template: Option<PathBuf>,
    /// Whether the row format dropdown is showing its list.
    mode_list_open: bool,
    /// Rows per `INSERT`.
    batch_input: Entity<TextInput>,
    /// The `WHERE` clause, without the keyword.
    where_input: Entity<TextInput>,
    /// Which half of the dialog is on screen.
    stage: Stage,
    /// A refusal to show under the form — a specification the bridge would not
    /// accept, or a file that could not be opened.
    notice: Option<SharedString>,
    /// The task that starts the job and then polls it.
    ///
    /// Dropping it cancels the job; see the module documentation.
    _poll: Option<Task<()>>,
}

impl ExtractDialog {
    /// Builds the dialog, closed.
    pub fn new(cx: &mut Context<Self>) -> Self {
        // Both placeholders are sample values rather than words, so neither is
        // translated: a row count and a SQL fragment read the same everywhere.
        let batch_input = cx.new(|cx| {
            TextInput::new(cx)
                .placeholder("1")
                .tab_index(tab::BATCH_ROWS)
        });
        let where_input = cx.new(|cx| {
            TextInput::new(cx)
                .placeholder("id > 1000")
                .tab_index(tab::WHERE)
        });

        Self {
            open: false,
            focus_handle: cx.focus_handle(),
            pending_focus: false,
            target: None,
            session: None,
            output: None,
            ddl: true,
            include_drop: false,
            data: true,
            mode: DataMode::Insert,
            template: None,
            mode_list_open: false,
            batch_input,
            where_input,
            stage: Stage::Form,
            notice: None,
            _poll: None,
        }
    }

    /// Shows the dialog over `target`, extracting through `session`.
    ///
    /// The form starts from its defaults every time rather than from the last
    /// extraction: the output path is the one field that must not be inherited
    /// — writing over the previous object's script because the path was still
    /// in the box is not a mistake a user can undo — and a form where one field
    /// resets and the rest do not reads as a bug.
    pub fn open(&mut self, target: ObjectTarget, session: SessionHandle, cx: &mut Context<Self>) {
        let defaults = ExtractForm::default();
        self.target = Some(target);
        self.session = Some(session);
        self.output = defaults.output;
        self.ddl = defaults.ddl;
        self.include_drop = defaults.include_drop;
        self.data = defaults.data;
        self.mode = defaults.mode;
        self.template = defaults.template;
        self.mode_list_open = false;
        self.batch_input
            .update(cx, |input, cx| input.set_content("1", cx));
        self.where_input.update(cx, |input, cx| input.clear(cx));
        self.stage = Stage::Form;
        self.notice = None;
        self._poll = None;
        self.open = true;
        self.pending_focus = true;
        cx.notify();
    }

    /// Whether the dialog is visible.
    pub fn is_open(&self) -> bool {
        self.open
    }

    /// Hides the dialog without emitting an event.
    ///
    /// **A job still running is cancelled**, by the drop chain the module
    /// documentation describes: the stage goes back to [`Stage::Form`] and the
    /// polling task is dropped, which leaves nobody holding the `Job` and its
    /// `Drop` asks the bridge to stop. The partial file stays on disk, which is
    /// what the bridge promises and what a user who chose that path expects.
    pub fn close(&mut self, cx: &mut Context<Self>) {
        self.open = false;
        self.pending_focus = false;
        self.mode_list_open = false;
        self.stage = Stage::Form;
        self._poll = None;
        self.session = None;
        self.target = None;
        self.notice = None;
        cx.notify();
    }

    /// Closes the dialog and reports it, so the shell can restore focus.
    pub fn dismiss(&mut self, cx: &mut Context<Self>) {
        cx.emit(ExtractDialogEvent::Dismissed);
        self.close(cx);
    }

    /// What `Escape` means here, which depends on what is on screen.
    ///
    /// A dropdown takes it first, then a running job — where `Escape` is the
    /// cancel button and *not* a close, so that the card cannot be dismissed
    /// into a poll loop nobody is watching. Only a form or a finished job is
    /// thrown away by it.
    pub fn escape(&mut self, cx: &mut Context<Self>) {
        if self.mode_list_open {
            self.mode_list_open = false;
            cx.notify();
            return;
        }
        if matches!(self.stage, Stage::Running(_)) {
            self.request_cancel(cx);
            return;
        }
        self.dismiss(cx);
    }

    /// The form as a plain value, read out of the widgets.
    fn form(&self, cx: &App) -> ExtractForm {
        ExtractForm {
            output: self.output.clone(),
            ddl: self.ddl,
            include_drop: self.include_drop,
            data: self.data,
            mode: self.mode,
            batch_rows: self
                .batch_input
                .read(cx)
                .content()
                .trim()
                .parse()
                .unwrap_or(DEFAULT_BATCH_ROWS),
            template: self.template.clone(),
            where_clause: self.where_input.read(cx).content().to_owned(),
        }
    }

    /// The specification the form describes, or `None` while it is incomplete.
    fn spec(&self, cx: &App) -> Option<ExtractSpec> {
        build_spec(self.target.as_ref()?, &self.form(cx))
    }

    /// The file name suggested by the save dialog.
    ///
    /// The object's own name, with the extension the chosen format writes:
    /// picking CSV and being handed `ORDERS.sql` is the kind of small lie that
    /// ends with a file nothing will open.
    fn suggested_name(&self) -> String {
        let name = self
            .target
            .as_ref()
            .map(|target| target.name.as_str())
            .unwrap_or("extract");
        let extension = if self.data && self.mode == DataMode::Csv && !self.ddl {
            "csv"
        } else {
            "sql"
        };
        format!("{name}.{extension}")
    }

    /// Asks the platform where to write, and records the answer.
    ///
    /// Nothing waits on the prompt: on X11 that call is exactly the one gpui had
    /// to be patched around, so the click returns immediately and the answer is
    /// picked up on a task of its own — the shape the settings dialog's export
    /// uses.
    fn pick_output(&mut self, cx: &mut Context<Self>) {
        let suggested = self.suggested_name();
        let directory = self
            .output
            .as_ref()
            .and_then(|path| path.parent().map(Path::to_path_buf))
            .unwrap_or_else(default_directory);
        let prompt = cx.prompt_for_new_path(&directory, Some(&suggested));

        cx.spawn(async move |dialog, cx| {
            let path = match prompt.await {
                Ok(Ok(Some(path))) => path,
                Ok(Ok(None)) | Err(_) => return,
                Ok(Err(error)) => {
                    log::warn!("the save dialog could not be opened: {error:#}");
                    return;
                }
            };
            dialog
                .update(cx, |dialog, cx| {
                    dialog.output = Some(path);
                    dialog.notice = None;
                    cx.notify();
                })
                .ok();
        })
        .detach();
    }

    /// Asks the platform for a template file, and records the answer.
    fn pick_template(&mut self, cx: &mut Context<Self>) {
        let paths = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some(ts!("extract.template_select")),
        });

        cx.spawn(async move |dialog, cx| {
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
            dialog
                .update(cx, |dialog, cx| {
                    dialog.template = Some(path);
                    dialog.notice = None;
                    cx.notify();
                })
                .ok();
        })
        .detach();
    }

    /// Starts the job the form describes, and begins polling it.
    ///
    /// `start_job` is a JNI round trip through the session's worker, so it goes
    /// on a background task like everything else that talks to the bridge; a
    /// specification the bridge refuses comes back as an error and lands in the
    /// notice line with the form still up, because the fix is in the form.
    fn start(&mut self, cx: &mut Context<Self>) {
        if !matches!(self.stage, Stage::Form) {
            return;
        }
        let (Some(spec), Some(session)) = (self.spec(cx), self.session.clone()) else {
            return;
        };
        self.notice = None;

        // The handle travels into the task and back out of the first step: it
        // is what keeps the session — and the tunnel under it — alive for the
        // length of the job, and the job itself carries no claim on either.
        let starting = cx.background_spawn(async move {
            let outcome = session.session().start_job(&spec);
            (session, outcome)
        });

        self._poll = Some(cx.spawn(async move |dialog, cx| {
            let (_session, outcome) = starting.await;
            let job = match outcome {
                Ok(job) => Arc::new(Mutex::new(job)),
                Err(error) => {
                    dialog
                        .update(cx, |dialog, cx| {
                            dialog.notice = Some(SharedString::from(format!("{error}")));
                            cx.notify();
                        })
                        .ok();
                    return;
                }
            };

            if dialog
                .update(cx, |dialog, cx| dialog.enter_progress(Arc::clone(&job), cx))
                .is_err()
            {
                return;
            }

            loop {
                let reading = cx
                    .background_spawn({
                        let job = Arc::clone(&job);
                        async move { job.lock().poll() }
                    })
                    .await;
                let carry_on = dialog
                    .update(cx, |dialog, cx| dialog.deliver(reading, cx))
                    .unwrap_or(false);
                if !carry_on {
                    return;
                }
                cx.background_executor().timer(POLL_INTERVAL).await;
            }
        }));
        cx.notify();
    }

    /// Switches the card into progress mode, before the first reading.
    fn enter_progress(&mut self, job: Arc<Mutex<Job>>, cx: &mut Context<Self>) {
        self.stage = Stage::Running(Running {
            job,
            phase: SharedString::default(),
            rows_done: 0,
            bytes: 0,
            cancelling: false,
        });
        cx.notify();
    }

    /// Records one reading. Answers whether the loop should take another.
    ///
    /// **A terminal reading retires the job's handle inside the bridge**, so a
    /// further poll would be a protocol error — which is why this, and not the
    /// task, is what decides that the loop stops.
    fn deliver(
        &mut self,
        reading: rudbman_jdbc::Result<JobProgress>,
        cx: &mut Context<Self>,
    ) -> bool {
        let Stage::Running(running) = &mut self.stage else {
            // The dialog was closed under the task; there is nothing to record
            // and nothing left to poll.
            return false;
        };

        let progress = match reading {
            Ok(progress) => progress,
            Err(error) => {
                // A poll that cannot be taken is the end of the job as far as
                // this card is concerned: the handle is either gone or the
                // bridge is unreachable, and either way nothing more will
                // arrive.
                self.stage = Stage::Ended(Ended::Failed(SharedString::from(format!("{error}"))));
                cx.notify();
                return false;
            }
        };

        running.phase = SharedString::from(progress.phase.clone());
        running.rows_done = progress.rows_done;
        running.bytes = progress.bytes;

        if !progress.is_terminal() {
            cx.notify();
            return true;
        }

        self.stage = Stage::Ended(match progress.state {
            JobState::Done => Ended::Done {
                rows: progress.rows_done,
                bytes: progress.bytes,
            },
            JobState::Cancelled => Ended::Cancelled,
            // `Running` cannot reach here — `is_terminal` excluded it — and a
            // failure without an error envelope is a bridge that broke its own
            // contract, so the fallback says so rather than showing nothing.
            JobState::Failed | JobState::Running => Ended::Failed(
                progress
                    .errors
                    .first()
                    .map(|error| SharedString::from(error.message.clone()))
                    .unwrap_or_else(|| ts!("extract.failed_unknown")),
            ),
        });
        cx.notify();
        false
    }

    /// Asks the job to stop.
    ///
    /// The card stays up: a cancel is a request the job notices at its next row
    /// or phase boundary, and the state that follows is still read by a poll.
    /// The button changes to say so.
    fn request_cancel(&mut self, cx: &mut Context<Self>) {
        let Stage::Running(running) = &mut self.stage else {
            return;
        };
        if running.cancelling {
            return;
        }
        running.cancelling = true;
        // Detached inside the bridge and quick — it sets a flag and interrupts
        // the statement in flight — so it is taken here rather than handed to a
        // task that would arrive after the work it was meant to stop.
        if let Err(error) = running.job.lock().cancel() {
            log::warn!("cancelling the extraction failed: {error}");
        }
        cx.notify();
    }

    /// Moves focus into the dialog when it opens, so `Escape` reaches the shell
    /// from inside it.
    fn apply_pending_focus(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.pending_focus {
            return;
        }
        self.pending_focus = false;
        let handle = self.focus_handle.clone();
        window.focus(&handle, cx);
        cx.notify();
    }

    /// The object name row, which both halves of the dialog show.
    fn render_object(&self, cx: &App) -> impl IntoElement + use<> {
        let chrome = theme(cx);
        let name = self
            .target
            .as_ref()
            .map(ObjectTarget::qualified)
            .unwrap_or_default();
        form_row(
            ts!("extract.object"),
            div().text_color(chrome.text).child(name),
        )
    }

    /// A read-only path cell with a "Browse…" button beside it.
    ///
    /// The path is never typeable: it is what a platform save or open dialog
    /// handed back, and a field the user can edit into a directory that does not
    /// exist buys nothing — the bridge resolves the path on the machine the JVM
    /// runs on, so a typo would only be discovered when the job refuses to
    /// start.
    fn render_path_row(
        &self,
        row: PathRow,
        path: Option<&PathBuf>,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let chrome = theme(cx);
        let this = cx.entity();
        let (text, color) = match path {
            Some(path) => (path.display().to_string(), chrome.text),
            None => (row.placeholder.to_string(), chrome.text_muted),
        };
        let pick = row.pick;

        form_row(
            row.label,
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap(px(8.))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .overflow_hidden()
                        .text_size(px(12.))
                        .text_color(color)
                        .child(text),
                )
                .child(
                    Button::new(row.id, ts!("extract.browse"))
                        .variant(ButtonVariant::Secondary)
                        .tab_index(row.tab_index)
                        .on_click(move |_, _window, cx| {
                            this.update(cx, pick);
                        }),
                ),
        )
    }

    /// The form half: everything the job is described by.
    fn render_form(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
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

        let output = self.render_path_row(
            PathRow {
                label: ts!("extract.output"),
                placeholder: ts!("extract.output_none"),
                id: "extract-output",
                tab_index: tab::OUTPUT,
                pick: Self::pick_output,
            },
            self.output.as_ref(),
            cx,
        );

        // Only while the schema is going in: a "precede it with DROP" that does
        // nothing is a control that lies. Rendering it conditionally is the same
        // treatment the connection dialog gives its keep-alive fields.
        let drop_row = self.ddl.then(|| {
            div().pl(px(22.)).child(toggle(
                "extract-drop",
                ts!("extract.drop"),
                self.include_drop,
                tab::DROP,
                |dialog, value| dialog.include_drop = value,
            ))
        });

        let data_body = self.data.then(|| {
            let mode = Select::new("extract-mode")
                .options(MODES.iter().map(|mode| mode_label(*mode)))
                .selected(Some(mode_label(self.mode)))
                .open(self.mode_list_open)
                .tab_index(tab::MODE)
                .on_select({
                    let this = this.clone();
                    // By index: the labels are translated and say nothing about
                    // which `DataMode` they are.
                    move |index, _label, _window, cx| {
                        let Some(mode) = MODES.get(index).copied() else {
                            return;
                        };
                        this.update(cx, |dialog, cx| {
                            dialog.mode = mode;
                            cx.notify();
                        });
                    }
                })
                .on_open_change({
                    let this = this.clone();
                    move |open, _window, cx| {
                        this.update(cx, |dialog, cx| {
                            dialog.mode_list_open = open;
                            cx.notify();
                        });
                    }
                });

            let batch = (self.mode == DataMode::Insert).then(|| {
                form_row(
                    ts!("extract.batch_rows"),
                    div().flex_none().w(px(96.)).child(self.batch_input.clone()),
                )
            });
            let template = (self.mode == DataMode::Template).then(|| {
                self.render_path_row(
                    PathRow {
                        label: ts!("extract.template"),
                        placeholder: ts!("extract.template_none"),
                        id: "extract-template",
                        tab_index: tab::TEMPLATE,
                        pick: Self::pick_template,
                    },
                    self.template.as_ref(),
                    cx,
                )
            });

            div()
                .pl(px(22.))
                .flex()
                .flex_col()
                .gap(px(8.))
                .child(form_row(ts!("extract.mode"), mode))
                .children(batch)
                .children(template)
        });

        div()
            .flex()
            .flex_col()
            .gap(px(10.))
            .child(self.render_object(cx))
            .child(output)
            .child(toggle(
                "extract-ddl",
                ts!("extract.ddl"),
                self.ddl,
                tab::DDL,
                |dialog, value| dialog.ddl = value,
            ))
            .children(drop_row)
            .child(toggle(
                "extract-data",
                ts!("extract.data"),
                self.data,
                tab::DATA,
                |dialog, value| dialog.data = value,
            ))
            .children(data_body)
            .child(form_row(ts!("extract.where"), self.where_input.clone()))
    }

    /// The progress half: what the job is doing, and how far it has got.
    fn render_running(
        &self,
        running: &Running,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let chrome = theme(cx);
        let phase = if running.phase.is_empty() {
            ts!("extract.starting")
        } else {
            running.phase.clone()
        };

        div()
            .flex()
            .flex_col()
            .gap(px(10.))
            .child(self.render_object(cx))
            .child(
                div()
                    .text_size(px(12.))
                    .text_color(chrome.text_muted)
                    .child(if running.cancelling {
                        ts!("extract.cancelling")
                    } else {
                        phase
                    }),
            )
            .child(div().text_color(chrome.text).child(ts!(
                "extract.counters",
                rows = running.rows_done,
                bytes = format_bytes(running.bytes)
            )))
    }

    /// The card a finished job leaves behind.
    fn render_ended(&self, ended: &Ended, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let chrome = theme(cx);
        let path = self
            .output
            .as_ref()
            .map(|path| path.display().to_string())
            .unwrap_or_default();

        let (message, color) = match ended {
            Ended::Done { rows, bytes } => (
                ts!(
                    "extract.done",
                    path = path,
                    rows = rows,
                    bytes = format_bytes(*bytes)
                ),
                chrome.success,
            ),
            Ended::Cancelled => (ts!("extract.cancelled", path = path), chrome.text),
            Ended::Failed(message) => (
                ts!("extract.failed", error = message.as_ref()),
                chrome.danger,
            ),
        };

        div()
            .flex()
            .flex_col()
            .gap(px(10.))
            .child(self.render_object(cx))
            .child(div().text_size(px(12.)).text_color(color).child(message))
    }

    /// The button row, which is a different pair in each stage.
    fn render_footer(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let this = cx.entity();
        let chrome = theme(cx);

        let notice = self.notice.clone().map(|message| {
            div()
                .flex_1()
                .min_w_0()
                .text_size(px(11.))
                .text_color(chrome.danger)
                .child(message)
        });

        let buttons = match &self.stage {
            Stage::Form => {
                let ready = self.spec(cx).is_some();
                div()
                    .flex()
                    .flex_row()
                    .gap(px(8.))
                    .child(
                        Button::new("extract-cancel", ts!("common.cancel"))
                            .variant(ButtonVariant::Secondary)
                            .tab_index(tab::DISMISS)
                            .on_click({
                                let this = this.clone();
                                move |_, _window, cx| {
                                    this.update(cx, |dialog, cx| dialog.dismiss(cx));
                                }
                            }),
                    )
                    .child(
                        Button::new("extract-start", ts!("extract.start"))
                            .variant(ButtonVariant::Primary)
                            .disabled(!ready)
                            .tab_index(tab::START)
                            .on_click({
                                let this = this.clone();
                                move |_, _window, cx| {
                                    this.update(cx, |dialog, cx| dialog.start(cx));
                                }
                            }),
                    )
            }
            Stage::Running(running) => div().flex().flex_row().gap(px(8.)).child(
                Button::new("extract-stop", ts!("common.cancel"))
                    .variant(ButtonVariant::Secondary)
                    .disabled(running.cancelling)
                    .tab_index(tab::DISMISS)
                    .on_click({
                        let this = this.clone();
                        move |_, _window, cx| {
                            this.update(cx, |dialog, cx| dialog.request_cancel(cx));
                        }
                    }),
            ),
            Stage::Ended(_) => div().flex().flex_row().gap(px(8.)).child(
                Button::new("extract-close", ts!("common.close"))
                    .variant(ButtonVariant::Primary)
                    .tab_index(tab::DISMISS)
                    .on_click({
                        let this = this.clone();
                        move |_, _window, cx| {
                            this.update(cx, |dialog, cx| dialog.dismiss(cx));
                        }
                    }),
            ),
        };

        div()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(12.))
            .children(notice)
            .child(div().flex_1())
            .child(buttons)
    }
}

/// The label the dropdown lists one row format under.
fn mode_label(mode: DataMode) -> SharedString {
    match mode {
        DataMode::Insert => ts!("extract.mode_insert"),
        DataMode::Csv => ts!("extract.mode_csv"),
        DataMode::Template => ts!("extract.mode_template"),
    }
}

/// Where the save dialog opens when no output has been chosen yet.
///
/// The user's home directory, and the working directory when even that cannot
/// be resolved — the bridge resolves a relative path against the *JVM's*
/// working directory, which is rarely what someone picking a file means, so
/// the prompt is always given somewhere absolute to start from.
///
/// Shared with the backup dialog, which opens the same kind of save prompt.
pub fn default_directory() -> PathBuf {
    directories::UserDirs::new()
        .map(|dirs| dirs.home_dir().to_path_buf())
        .unwrap_or_else(|| PathBuf::from("."))
}

impl EventEmitter<ExtractDialogEvent> for ExtractDialog {}

impl Focusable for ExtractDialog {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for ExtractDialog {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if !self.open {
            return div().id("extract-dialog");
        }

        self.apply_pending_focus(window, cx);

        let body = match &self.stage {
            Stage::Form => self.render_form(cx).into_any_element(),
            Stage::Running(running) => self.render_running(running, cx).into_any_element(),
            Stage::Ended(ended) => self.render_ended(ended, cx).into_any_element(),
        };
        let footer = self.render_footer(cx);

        let on_dismiss = {
            let this = cx.entity();
            move |_window: &mut Window, cx: &mut App| {
                this.update(cx, |dialog, cx| dialog.escape(cx));
            }
        };

        // Absolute and full-size for the same reason as the other dialogs: an
        // absolutely positioned child is laid out against its direct parent.
        div()
            .id("extract-dialog")
            .absolute()
            .inset_0()
            .size_full()
            .track_focus(&self.focus_handle)
            .child(modal(
                "extract-modal",
                ts!("extract.title"),
                px(DIALOG_WIDTH),
                div()
                    .flex()
                    .flex_col()
                    .gap(px(14.))
                    .child(body)
                    .child(footer),
                on_dismiss,
            ))
    }
}

#[cfg(test)]
mod tests {
    use gpui::{TestAppContext, WindowHandle};
    use rudbman_core::AppSettings;
    use rudbman_jdbc::StatementSpec;

    use super::*;
    use crate::connection::{self, Connected};
    use crate::explorer::{ConnectionId, Folder};

    /// The object every test below extracts.
    fn target(name: &str) -> ObjectTarget {
        ObjectTarget {
            connection: ConnectionId(1),
            catalog: None,
            schema: Some("PUBLIC".to_string()),
            folder: Folder::Tables,
            name: name.to_string(),
        }
    }

    /// A live H2 database with `setup` already run against it.
    fn h2(name: &str, setup: &[&str]) -> Connected {
        let mut profile = connection::h2::profile(name);
        profile.url = format!("{};DB_CLOSE_DELAY=-1", profile.url);
        profile.confirm_writes = false;
        profile.read_only = false;
        let connected = connection::connect(
            &profile,
            &connection::h2::driver(),
            &connection::Credentials::typed(Some(String::new()), None),
            &AppSettings::default(),
        )
        .expect("H2 opens an in-memory database without a server");

        for sql in setup {
            connected
                .session()
                .execute(&StatementSpec::new(*sql))
                .unwrap_or_else(|error| panic!("{sql}: {error}"));
        }
        connected
    }

    /// A window holding nothing but the dialog, already open over `name`.
    fn dialog(
        connected: &Connected,
        name: &str,
        output: &Path,
        cx: &mut TestAppContext,
    ) -> WindowHandle<ExtractDialog> {
        cx.update(|cx| {
            rudbman_ui::init(cx);
        });
        let session = connected.handle();
        let target = target(name);
        let output = output.to_path_buf();
        let window = cx.add_window(|_window, cx| ExtractDialog::new(cx));
        window
            .update(cx, |dialog, _window, cx| {
                dialog.open(target, session, cx);
                dialog.output = Some(output);
            })
            .expect("the window is open");
        window
    }

    /// Runs the poll loop to its end, moving the test clock the way the real one
    /// moves on its own.
    ///
    /// `run_until_parked` runs what is ready; it does *not* advance the
    /// simulated clock, so a loop that sleeps between readings has to be walked
    /// forward one interval at a time.
    fn drive(window: &WindowHandle<ExtractDialog>, cx: &mut TestAppContext) {
        for _ in 0..2_000 {
            cx.run_until_parked();
            let ended = window
                .update(cx, |dialog, _window, _cx| {
                    matches!(dialog.stage, Stage::Ended(_))
                })
                .expect("the window is open");
            if ended {
                return;
            }
            cx.executor().advance_clock(POLL_INTERVAL);
            // The clock above is virtual and advancing it costs no wall time,
            // but the job lives on a real bridge thread: noticing a cancel,
            // closing the statement and finalising the file take real
            // milliseconds. Two thousand instantaneous polls can all fire
            // inside that window, so each turn yields one real millisecond —
            // a two-second ceiling — to let the bridge actually move.
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        panic!("the job never reported a terminal state");
    }

    #[test]
    fn the_form_becomes_the_specification_the_bridge_is_sent() {
        let target = target("ORDERS");

        // Schema only: no data member is set at all, so the bridge writes
        // nothing but the CREATE.
        let ddl_only = ExtractForm {
            output: Some(PathBuf::from("/tmp/orders.sql")),
            ddl: true,
            include_drop: true,
            data: false,
            ..ExtractForm::default()
        };
        let spec = build_spec(&target, &ddl_only).expect("a path and the schema are enough");
        assert_eq!(spec.objects.len(), 1);
        assert_eq!(spec.objects[0].name, "ORDERS");
        assert_eq!(spec.objects[0].schema.as_deref(), Some("PUBLIC"));
        assert_eq!(spec.objects[0].catalog, None);
        assert_eq!(spec.output.path, PathBuf::from("/tmp/orders.sql"));
        assert!(spec.ddl.include);
        assert!(spec.ddl.include_drop);
        // Never offered in the form, and never anything else: a replayable
        // script needs the foreign keys out of the CREATEs.
        assert_eq!(spec.ddl.constraints, Constraints::Alter);
        assert!(!spec.data.include);

        // Rows only, as CSV. The batch size is irrelevant to that mode and the
        // WHERE travels without its keyword.
        let csv = ExtractForm {
            output: Some(PathBuf::from("/tmp/orders.csv")),
            ddl: false,
            data: true,
            mode: DataMode::Csv,
            where_clause: "  state = 'open'  ".to_string(),
            ..ExtractForm::default()
        };
        let spec = build_spec(&target, &csv).expect("a path and the rows are enough");
        assert!(!spec.ddl.include);
        assert!(spec.data.include);
        assert_eq!(spec.data.mode, DataMode::Csv);
        assert_eq!(spec.data.where_clause.as_deref(), Some("state = 'open'"));
        assert_eq!(spec.data.template_path, None);

        // Template mode carries its file; INSERT mode carries its batch size,
        // and neither carries the other's.
        let template = ExtractForm {
            output: Some(PathBuf::from("/tmp/orders.txt")),
            ddl: false,
            data: true,
            mode: DataMode::Template,
            template: Some(PathBuf::from("/tmp/row.tpl")),
            batch_rows: 50,
            ..ExtractForm::default()
        };
        let spec = build_spec(&target, &template).expect("a path and the rows are enough");
        assert_eq!(spec.data.mode, DataMode::Template);
        assert_eq!(
            spec.data.template_path.as_deref(),
            Some(Path::new("/tmp/row.tpl"))
        );
        assert_eq!(
            spec.data.insert_batch_rows, 1,
            "the batch size belongs to INSERT mode and is left at the portable default"
        );

        let batched = ExtractForm {
            output: Some(PathBuf::from("/tmp/orders.sql")),
            data: true,
            mode: DataMode::Insert,
            batch_rows: 200,
            ..ExtractForm::default()
        };
        let spec = build_spec(&target, &batched).expect("a path and the rows are enough");
        assert_eq!(spec.data.insert_batch_rows, 200);
        // A blank WHERE is no WHERE, not an empty predicate the bridge would
        // have to make sense of.
        assert_eq!(spec.data.where_clause, None);

        // A count of zero would travel as a row count nobody meant; the form's
        // own field is clamped rather than sent.
        let zero = ExtractForm {
            output: Some(PathBuf::from("/tmp/orders.sql")),
            data: true,
            batch_rows: 0,
            ..ExtractForm::default()
        };
        assert_eq!(
            build_spec(&target, &zero)
                .expect("a path and the rows are enough")
                .data
                .insert_batch_rows,
            1
        );

        // A catalogue on the target reaches the object reference; a blank one
        // does not, because "" is not a catalogue name.
        let mut catalogued = target.clone();
        catalogued.catalog = Some("TESTDB".to_string());
        let spec = build_spec(&catalogued, &ddl_only).expect("a path and the schema are enough");
        assert_eq!(spec.objects[0].catalog.as_deref(), Some("TESTDB"));
        let mut blank = target.clone();
        blank.schema = Some("   ".to_string());
        let spec = build_spec(&blank, &ddl_only).expect("a path and the schema are enough");
        assert_eq!(spec.objects[0].schema, None);
    }

    #[test]
    fn an_incomplete_form_produces_no_specification_at_all() {
        let target = target("ORDERS");

        // No output file: this is what disables the "Extract" button.
        assert!(
            build_spec(
                &target,
                &ExtractForm {
                    output: None,
                    ..ExtractForm::default()
                }
            )
            .is_none()
        );

        // Neither half asked for: the job would write an empty file, which the
        // bridge refuses anyway — refusing it here keeps the button honest.
        assert!(
            build_spec(
                &target,
                &ExtractForm {
                    output: Some(PathBuf::from("/tmp/orders.sql")),
                    ddl: false,
                    data: false,
                    ..ExtractForm::default()
                }
            )
            .is_none()
        );
    }

    #[test]
    fn byte_counts_read_like_a_file_manager() {
        assert_eq!(format_bytes(0), "0 B");
        assert_eq!(format_bytes(999), "999 B");
        assert_eq!(format_bytes(1_000), "1.0 kB");
        assert_eq!(format_bytes(1_500), "1.5 kB");
        assert_eq!(format_bytes(2_500_000), "2.5 MB");
        assert_eq!(format_bytes(3_000_000_000), "3.0 GB");
    }

    /// Every half of the dialog lays out, in every shape the form can take.
    ///
    /// A render test rather than an assertion about pixels: the controls are
    /// built conditionally — the DROP toggle only with the schema, the batch
    /// field only with INSERT, the template row only with templates — and a
    /// branch that is never drawn is a branch that can panic on the day it is.
    #[gpui::test]
    fn every_shape_of_the_dialog_lays_out(cx: &mut TestAppContext) {
        let connected = h2("extract-render", &["CREATE TABLE ORDERS (ID INT)"]);
        let dir = tempfile::tempdir().expect("a temporary directory");
        let window = dialog(&connected, "ORDERS", &dir.path().join("orders.sql"), cx);
        let mut cx = gpui::VisualTestContext::from_window(window.into(), cx);

        for (ddl, mode) in [
            (true, DataMode::Insert),
            (false, DataMode::Csv),
            (true, DataMode::Template),
        ] {
            window
                .update(&mut cx, |dialog, _window, cx| {
                    dialog.ddl = ddl;
                    dialog.include_drop = ddl;
                    dialog.mode = mode;
                    dialog.mode_list_open = mode == DataMode::Template;
                    cx.notify();
                })
                .expect("the window is open");
            cx.run_until_parked();
        }

        // And the two cards a job leaves behind, without running one.
        for ended in [
            Ended::Done {
                rows: 12,
                bytes: 3_400,
            },
            Ended::Cancelled,
            Ended::Failed(SharedString::from("the table went away")),
        ] {
            window
                .update(&mut cx, |dialog, _window, cx| {
                    dialog.stage = Stage::Ended(ended);
                    cx.notify();
                })
                .expect("the window is open");
            cx.run_until_parked();
        }
    }

    /// The whole path, against a real database: the form becomes a job, the job
    /// is polled to its end, and the file on disk is a script.
    #[gpui::test]
    fn an_extraction_writes_a_replayable_script(cx: &mut TestAppContext) {
        let connected = h2(
            "extract-done",
            &[
                "CREATE TABLE ORDERS (ID INT PRIMARY KEY, STATE VARCHAR(16))",
                "INSERT INTO ORDERS VALUES (1, 'open'), (2, 'shipped'), (3, 'open')",
            ],
        );
        let dir = tempfile::tempdir().expect("a temporary directory");
        let path = dir.path().join("orders.sql");
        let window = dialog(&connected, "ORDERS", &path, cx);

        window
            .update(cx, |dialog, _window, cx| dialog.start(cx))
            .expect("the window is open");
        drive(&window, cx);

        window
            .update(cx, |dialog, _window, _cx| {
                let Stage::Ended(Ended::Done { rows, bytes }) = &dialog.stage else {
                    // The bridge's own words, because a CI log has nothing else
                    // to diagnose a one-off failure by.
                    match &dialog.stage {
                        Stage::Ended(Ended::Failed(message)) => {
                            panic!("the job failed: {message}")
                        }
                        _ => panic!("the job did not finish cleanly"),
                    }
                };
                assert_eq!(*rows, 3);
                assert!(*bytes > 0, "a script with three rows in it is not empty");
                assert!(
                    dialog.notice.is_none(),
                    "nothing was refused, so nothing is reported"
                );
            })
            .expect("the window is open");

        let script = std::fs::read_to_string(&path).expect("the job wrote the file");
        assert!(
            script.to_uppercase().contains("CREATE TABLE"),
            "the schema is missing from {script}"
        );
        assert!(
            script.to_uppercase().contains("INSERT INTO"),
            "the rows are missing from {script}"
        );
        assert!(script.contains("shipped"), "a value is missing: {script}");
    }

    /// A specification the bridge will not accept is a refusal, not a job: the
    /// form stays up with the reason under it.
    #[gpui::test]
    fn a_refused_specification_keeps_the_form_up(cx: &mut TestAppContext) {
        let connected = h2(
            "extract-refused",
            &["CREATE TABLE ORDERS (ID INT PRIMARY KEY)"],
        );
        let dir = tempfile::tempdir().expect("a temporary directory");
        let path = dir.path().join("orders.txt");
        let window = dialog(&connected, "ORDERS", &path, cx);

        // Template mode with no template file: the one refusal the form itself
        // deliberately does not pre-empt, because the bridge is the authority
        // on what a malformed request is.
        window
            .update(cx, |dialog, _window, cx| {
                dialog.ddl = false;
                dialog.mode = DataMode::Template;
                dialog.start(cx);
            })
            .expect("the window is open");
        cx.run_until_parked();

        window
            .update(cx, |dialog, _window, _cx| {
                assert!(
                    matches!(dialog.stage, Stage::Form),
                    "a refused start never becomes a progress card"
                );
                assert!(
                    dialog.notice.is_some(),
                    "the reason the bridge gave is shown in the form"
                );
            })
            .expect("the window is open");
        assert!(
            !path.exists(),
            "a specification that was never accepted wrote nothing"
        );
    }

    /// Cancelling ends the job as cancelled, leaves the partial file where it
    /// is, and hands the session back in one piece.
    #[gpui::test]
    fn cancelling_stops_the_job_and_leaves_the_session_usable(cx: &mut TestAppContext) {
        // Not a big table but a slow one. A big table only loses the race on a
        // fast enough disk — one CI runner finished 400k rows before the first
        // poll — whereas a view that sleeps a millisecond per row cannot reach
        // the end inside the test's lifetime, so the cancel always lands
        // mid-flight.
        let connected = h2(
            "extract-cancel",
            &[
                "CREATE ALIAS NAP AS 'long nap(long ms) throws Exception { \
                     Thread.sleep(ms); return ms; }'",
                "CREATE VIEW BIG AS SELECT NAP(1) AS PAUSE, X AS ID \
                     FROM SYSTEM_RANGE(1, 400000)",
            ],
        );
        let dir = tempfile::tempdir().expect("a temporary directory");
        let path = dir.path().join("big.sql");
        let window = dialog(&connected, "BIG", &path, cx);

        window
            .update(cx, |dialog, _window, cx| {
                dialog.ddl = false;
                dialog.start(cx);
            })
            .expect("the window is open");
        // One turn of the loop: the job is accepted, the card is up, and the
        // first reading has been taken.
        cx.run_until_parked();
        window
            .update(cx, |dialog, _window, cx| {
                assert!(
                    matches!(dialog.stage, Stage::Running(_)),
                    "the job was accepted and the card is showing it"
                );
                dialog.request_cancel(cx);
            })
            .expect("the window is open");

        drive(&window, cx);

        window
            .update(cx, |dialog, _window, _cx| {
                assert!(
                    matches!(dialog.stage, Stage::Ended(Ended::Cancelled)),
                    "a cancelled job ends as cancelled, not as done or failed"
                );
            })
            .expect("the window is open");

        // The partial output is left on disk on purpose — it is work the user
        // may still want — and it is partial: four hundred thousand rows do
        // not fit in what a job cancelled after milliseconds managed to write.
        let written = std::fs::metadata(&path)
            .expect("the partial file is left where it is")
            .len();
        assert!(
            written < 400_000 * 20,
            "the job wrote {written} bytes, which is not a cancelled extraction"
        );

        // And the session is not left holding the connection lock. Counting a
        // plain range, not the sleeping view, so the check itself is instant.
        let cursor = connected
            .session()
            .execute(&StatementSpec::new(
                "SELECT COUNT(*) FROM SYSTEM_RANGE(1, 10)",
            ))
            .expect("the session survived the cancelled job");
        drop(cursor);
    }
}
