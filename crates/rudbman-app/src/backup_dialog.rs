//! The backup dialog: one schema, out to a replayable script file.
//!
//! Opened over the scope the explorer's selection sits in — the same gate the
//! ERD command uses, because a schema, a folder and a table all name a scope —
//! it collects where the file goes, whether it is compressed, and which of the
//! schema and the rows go into it. Then it starts the job and turns into a
//! progress card, exactly as the extraction dialog does.
//!
//! # A backup is an extraction with no object list
//!
//! The bridge enumerates the scope's `TABLE`-typed tables itself, writes every
//! `CREATE`, then every foreign-key `ALTER`, then the rows in dependency order
//! (architecture document, §6). So the form is the extraction's minus the parts
//! that only make sense for a single object: no row format — several tables
//! share one file and only `INSERT` survives that — and no `WHERE`, for the
//! same reason.
//!
//! The parts it does share — the two halves in [`Stage`], the job behind an
//! `Arc<Mutex<Job>>`, closing as a cancel, `Escape` over a running job being
//! the cancel button — are [`crate::extract_dialog`]'s decisions, and its module
//! documentation is where the reasoning lives.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use gpui::{
    App, Context, Entity, EventEmitter, FocusHandle, Focusable, IntoElement, Render, SharedString,
    Task, Window, div, prelude::*, px,
};
use parking_lot::Mutex;
use rudbman_jdbc::{
    BackupDataOptions, BackupSpec, Compression, Constraints, DdlOptions, Job, JobProgress,
    JobState, ScopeRef,
};
use rudbman_ui::{Button, ButtonVariant, Checkbox, TextInput, form_row, modal, theme};

use crate::connection::SessionHandle;
use crate::explorer::Scope;
use crate::extract_dialog::{POLL_INTERVAL, default_directory, format_bytes, non_empty};
use crate::i18n::ts;

/// Width of the dialog panel, matching the extraction's.
const DIALOG_WIDTH: f32 = 520.;

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
    /// "Compress the file with gzip".
    pub const COMPRESS: isize = 11;
    /// "Include the schema".
    pub const DDL: isize = 20;
    /// "Precede it with DROP statements".
    pub const DROP: isize = 21;
    /// "Include the rows".
    pub const DATA: isize = 30;
    /// Rows per `INSERT`.
    pub const BATCH_ROWS: isize = 31;
    /// "Back up".
    pub const START: isize = 50;
    /// "Cancel" / "Close".
    pub const DISMISS: isize = 51;
}

/// Emitted by [`BackupDialog`] when the user closes it.
pub enum BackupDialogEvent {
    /// The dialog was dismissed; the shell should restore focus.
    Dismissed,
}

/// Everything the form collects, apart from the scope it was opened over.
///
/// Pulled out of the widgets into a plain value so that [`build_spec`] is a
/// pure function and can be tested without a window.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BackupForm {
    /// Where the script goes. `None` until the user has picked a file.
    pub output: Option<PathBuf>,
    /// Whether the file is wrapped in gzip.
    pub compress: bool,
    /// Whether the schema is written.
    pub ddl: bool,
    /// Whether `DROP` statements precede the `CREATE`s.
    pub include_drop: bool,
    /// Whether the rows are written.
    pub data: bool,
    /// How many rows one `INSERT` carries.
    pub batch_rows: u32,
}

impl Default for BackupForm {
    /// What the dialog opens with: an uncompressed script with the schema and
    /// the rows in it, one `INSERT` per row, and no file chosen yet.
    fn default() -> Self {
        BackupForm {
            output: None,
            compress: false,
            ddl: true,
            include_drop: false,
            data: true,
            batch_rows: DEFAULT_BATCH_ROWS,
        }
    }
}

/// The specification `form` describes for `scope`, or `None` when the form is
/// not ready to be sent.
///
/// Not ready means one of exactly two things, and both are what the "Back up"
/// button is disabled on: no output file, or neither the schema nor the rows
/// asked for — a job that would write an empty file. Everything else the bridge
/// judges, a scope holding no table included: that is a backup of nothing, not
/// a malformed request.
pub fn build_spec(scope: &Scope, form: &BackupForm) -> Option<BackupSpec> {
    let output = form.output.as_ref()?;
    if !form.ddl && !form.data {
        return None;
    }

    let mut reference = ScopeRef::new();
    if let Some(catalog) = non_empty(scope.catalog.as_deref()) {
        reference = reference.with_catalog(catalog);
    }
    if let Some(schema) = non_empty(scope.schema.as_deref()) {
        reference = reference.with_schema(schema);
    }

    // `BackupSpec::new` already builds the `OutputSpec`, and its defaults —
    // UTF-8 and `\n` — are the two the dialog does not offer, for the
    // extraction dialog's reason: a script read back by this same application
    // has no reason to be anything else.
    let mut spec = BackupSpec::new(output)
        .with_scope(reference)
        .with_compress(if form.compress {
            Compression::Gzip
        } else {
            Compression::None
        });

    if form.ddl {
        spec = spec.with_ddl(
            DdlOptions::included()
                .with_drop(form.include_drop)
                // Always `Alter`, and deliberately not offered: a backup is a
                // whole schema, and two tables that reference each other cannot
                // be created in any order with their keys inline.
                .with_constraints(Constraints::Alter),
        );
    }

    if form.data {
        spec = spec.with_data(BackupDataOptions::included().with_insert_batch_rows(
            // A count of zero would travel as a row count nobody meant.
            form.batch_rows.max(1),
        ));
    }

    Some(spec)
}

/// The scope as a title reads it: `catalog.schema`, or whichever half there is.
///
/// [`ErdTarget::qualified`](crate::erd_pane::ErdTarget) says the same thing for
/// the same reason; a product that skipped both levels — SQLite — leaves this
/// empty and the row falls back to a word.
fn qualified(scope: &Scope) -> String {
    [scope.catalog.as_deref(), scope.schema.as_deref()]
        .into_iter()
        .flatten()
        .filter(|part| !part.trim().is_empty())
        .collect::<Vec<_>>()
        .join(".")
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
struct Running {
    /// Shared with the polling task; see [`crate::extract_dialog`].
    job: Arc<Mutex<Job>>,
    /// The bridge's own phase text, or empty before the first reading.
    phase: SharedString,
    /// Rows written so far.
    rows_done: u64,
    /// Bytes written so far — after compression, when there is any.
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

/// Modal dialog that backs one schema up to a script file.
///
/// Create it once with [`BackupDialog::new`], keep the handle, subscribe to
/// [`BackupDialogEvent`], and render it as the last child of a `relative()`
/// root. It renders nothing while [`BackupDialog::is_open`] is `false`.
pub struct BackupDialog {
    /// Whether the dialog is currently visible.
    open: bool,
    /// Focus of the dialog root; the anchor the shell's `Escape` resolves
    /// against.
    focus_handle: FocusHandle,
    /// Whether focus should move into the dialog on the next render.
    pending_focus: bool,
    /// The scope the dialog was opened over, captured at that moment: the
    /// explorer's selection may move while the dialog is up.
    scope: Option<Scope>,
    /// The session the job runs on.
    ///
    /// Held for as long as the dialog is open, so that a connection tab closed
    /// mid-backup leaves the session — and the tunnel under it — standing.
    session: Option<SessionHandle>,
    /// Where the script goes.
    output: Option<PathBuf>,
    /// Whether the file is wrapped in gzip.
    compress: bool,
    /// Whether the schema is written.
    ddl: bool,
    /// Whether `DROP` statements precede the `CREATE`s.
    include_drop: bool,
    /// Whether the rows are written.
    data: bool,
    /// Rows per `INSERT`.
    batch_input: Entity<TextInput>,
    /// Which half of the dialog is on screen.
    stage: Stage,
    /// A refusal to show under the form.
    notice: Option<SharedString>,
    /// The task that starts the job and then polls it.
    ///
    /// Dropping it cancels the job.
    _poll: Option<Task<()>>,
}

impl BackupDialog {
    /// Builds the dialog, closed.
    pub fn new(cx: &mut Context<Self>) -> Self {
        // A sample value rather than a word, so it is not translated.
        let batch_input = cx.new(|cx| {
            TextInput::new(cx)
                .placeholder("1")
                .tab_index(tab::BATCH_ROWS)
        });

        Self {
            open: false,
            focus_handle: cx.focus_handle(),
            pending_focus: false,
            scope: None,
            session: None,
            output: None,
            compress: false,
            ddl: true,
            include_drop: false,
            data: true,
            batch_input,
            stage: Stage::Form,
            notice: None,
            _poll: None,
        }
    }

    /// Shows the dialog over `scope`, backing up through `session`.
    ///
    /// The form starts from its defaults every time rather than from the last
    /// backup, for the extraction dialog's reason: the output path must not be
    /// inherited, and a form where one field resets and the rest do not reads
    /// as a bug.
    pub fn open(&mut self, scope: Scope, session: SessionHandle, cx: &mut Context<Self>) {
        let defaults = BackupForm::default();
        self.scope = Some(scope);
        self.session = Some(session);
        self.output = defaults.output;
        self.compress = defaults.compress;
        self.ddl = defaults.ddl;
        self.include_drop = defaults.include_drop;
        self.data = defaults.data;
        self.batch_input
            .update(cx, |input, cx| input.set_content("1", cx));
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
    /// **A job still running is cancelled**, by the drop chain
    /// [`crate::extract_dialog`] documents. The partial file stays on disk,
    /// which is what the bridge promises and what a user who chose that path
    /// expects.
    pub fn close(&mut self, cx: &mut Context<Self>) {
        self.open = false;
        self.pending_focus = false;
        self.stage = Stage::Form;
        self._poll = None;
        self.session = None;
        self.scope = None;
        self.notice = None;
        cx.notify();
    }

    /// Closes the dialog and reports it, so the shell can restore focus.
    pub fn dismiss(&mut self, cx: &mut Context<Self>) {
        cx.emit(BackupDialogEvent::Dismissed);
        self.close(cx);
    }

    /// What `Escape` means here.
    ///
    /// The form holds no dropdown, so a running job takes it first — where
    /// `Escape` is the cancel button and *not* a close, so that the card cannot
    /// be dismissed into a poll loop nobody is watching.
    pub fn escape(&mut self, cx: &mut Context<Self>) {
        if matches!(self.stage, Stage::Running(_)) {
            self.request_cancel(cx);
            return;
        }
        self.dismiss(cx);
    }

    /// The form as a plain value, read out of the widgets.
    fn form(&self, cx: &App) -> BackupForm {
        BackupForm {
            output: self.output.clone(),
            compress: self.compress,
            ddl: self.ddl,
            include_drop: self.include_drop,
            data: self.data,
            batch_rows: self
                .batch_input
                .read(cx)
                .content()
                .trim()
                .parse()
                .unwrap_or(DEFAULT_BATCH_ROWS),
        }
    }

    /// The specification the form describes, or `None` while it is incomplete.
    fn spec(&self, cx: &App) -> Option<BackupSpec> {
        build_spec(self.scope.as_ref()?, &self.form(cx))
    }

    /// The file name suggested by the save dialog.
    ///
    /// The schema's own name with `-backup` after it, and the extension the
    /// chosen compression actually writes: being handed `APP-backup.sql` for a
    /// file that turns out to be gzip is the kind of small lie that ends with a
    /// file nothing will open.
    fn suggested_name(&self) -> String {
        let name = self
            .scope
            .as_ref()
            .map(qualified)
            .filter(|scope| !scope.is_empty())
            .unwrap_or_else(|| "database".to_string());
        let extension = if self.compress { "sql.gz" } else { "sql" };
        format!("{name}-backup.{extension}")
    }

    /// Asks the platform where to write, and records the answer.
    ///
    /// Nothing waits on the prompt: on X11 that call is exactly the one gpui
    /// had to be patched around, so the click returns immediately and the
    /// answer is picked up on a task of its own.
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

    /// Starts the job the form describes, and begins polling it.
    ///
    /// `start_backup` is a JNI round trip through the session's worker, so it
    /// goes on a background task; a specification the bridge refuses comes back
    /// as an error and lands in the notice line with the form still up, because
    /// the fix is in the form.
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
        // length of the job.
        let starting = cx.background_spawn(async move {
            let outcome = session.session().start_backup(&spec);
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
    /// further poll would be a protocol error.
    fn deliver(
        &mut self,
        reading: rudbman_jdbc::Result<JobProgress>,
        cx: &mut Context<Self>,
    ) -> bool {
        let Stage::Running(running) = &mut self.stage else {
            // The dialog was closed under the task.
            return false;
        };

        let progress = match reading {
            Ok(progress) => progress,
            Err(error) => {
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
                    .unwrap_or_else(|| ts!("backup.failed_unknown")),
            ),
        });
        cx.notify();
        false
    }

    /// Asks the job to stop. The card stays up until a poll says it did.
    fn request_cancel(&mut self, cx: &mut Context<Self>) {
        let Stage::Running(running) = &mut self.stage else {
            return;
        };
        if running.cancelling {
            return;
        }
        running.cancelling = true;
        if let Err(error) = running.job.lock().cancel() {
            log::warn!("cancelling the backup failed: {error}");
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

    /// The scope row, which every half of the dialog shows.
    fn render_scope(&self, cx: &App) -> impl IntoElement + use<> {
        let chrome = theme(cx);
        let name = self
            .scope
            .as_ref()
            .map(qualified)
            .filter(|scope| !scope.is_empty())
            .map(SharedString::from)
            // A product that skipped both the catalogue and the schema level —
            // SQLite — has one scope and no name for it, and a blank row would
            // be worse than a word.
            .unwrap_or_else(|| ts!("backup.scope_all"));
        form_row(
            ts!("backup.scope"),
            div().text_color(chrome.text).child(name),
        )
    }

    /// The output path cell and the button that picks it.
    ///
    /// The path is never typeable, for the extraction dialog's reason: it is
    /// what a platform save dialog handed back, and the bridge resolves it on
    /// the machine the JVM runs on.
    fn render_output(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let chrome = theme(cx);
        let this = cx.entity();
        let (text, color) = match &self.output {
            Some(path) => (path.display().to_string(), chrome.text),
            None => (ts!("backup.output_none").to_string(), chrome.text_muted),
        };

        form_row(
            ts!("backup.output"),
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
                    Button::new("backup-output", ts!("backup.browse"))
                        .variant(ButtonVariant::Secondary)
                        .tab_index(tab::OUTPUT)
                        .on_click(move |_, _window, cx| {
                            this.update(cx, BackupDialog::pick_output);
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

        // Only while the schema is going in: a "precede it with DROP" that does
        // nothing is a control that lies.
        let drop_row = self.ddl.then(|| {
            div().pl(px(22.)).child(toggle(
                "backup-drop",
                ts!("backup.drop"),
                self.include_drop,
                tab::DROP,
                |dialog, value| dialog.include_drop = value,
            ))
        });

        // Always shown while the rows are going in, and only then: a backup
        // writes `INSERT`s and nothing else, so there is no format to choose
        // and the batch size is the whole of the row half's settings.
        let batch_row = self.data.then(|| {
            div().pl(px(22.)).child(form_row(
                ts!("backup.batch_rows"),
                div().flex_none().w(px(96.)).child(self.batch_input.clone()),
            ))
        });

        div()
            .flex()
            .flex_col()
            .gap(px(10.))
            .child(self.render_scope(cx))
            .child(self.render_output(cx))
            .child(toggle(
                "backup-compress",
                ts!("backup.compress"),
                self.compress,
                tab::COMPRESS,
                |dialog, value| dialog.compress = value,
            ))
            .child(toggle(
                "backup-ddl",
                ts!("backup.ddl"),
                self.ddl,
                tab::DDL,
                |dialog, value| dialog.ddl = value,
            ))
            .children(drop_row)
            .child(toggle(
                "backup-data",
                ts!("backup.data"),
                self.data,
                tab::DATA,
                |dialog, value| dialog.data = value,
            ))
            .children(batch_row)
    }

    /// The progress half: what the job is doing, and how far it has got.
    fn render_running(
        &self,
        running: &Running,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let chrome = theme(cx);
        let phase = if running.phase.is_empty() {
            ts!("backup.starting")
        } else {
            running.phase.clone()
        };

        div()
            .flex()
            .flex_col()
            .gap(px(10.))
            .child(self.render_scope(cx))
            .child(
                div()
                    .text_size(px(12.))
                    .text_color(chrome.text_muted)
                    .child(if running.cancelling {
                        ts!("backup.cancelling")
                    } else {
                        phase
                    }),
            )
            .child(div().text_color(chrome.text).child(ts!(
                "backup.counters",
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
                    "backup.done",
                    path = path,
                    rows = rows,
                    bytes = format_bytes(*bytes)
                ),
                chrome.success,
            ),
            Ended::Cancelled => (ts!("backup.cancelled", path = path), chrome.text),
            Ended::Failed(message) => (
                ts!("backup.failed", error = message.as_ref()),
                chrome.danger,
            ),
        };

        div()
            .flex()
            .flex_col()
            .gap(px(10.))
            .child(self.render_scope(cx))
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
                        Button::new("backup-cancel", ts!("common.cancel"))
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
                        Button::new("backup-start", ts!("backup.start"))
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
                Button::new("backup-stop", ts!("common.cancel"))
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
                Button::new("backup-close", ts!("common.close"))
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

impl EventEmitter<BackupDialogEvent> for BackupDialog {}

impl Focusable for BackupDialog {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for BackupDialog {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if !self.open {
            return div().id("backup-dialog");
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
            .id("backup-dialog")
            .absolute()
            .inset_0()
            .size_full()
            .track_focus(&self.focus_handle)
            .child(modal(
                "backup-modal",
                ts!("backup.title"),
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

    /// The scope every test below backs up.
    fn scope(schema: &str) -> Scope {
        Scope {
            catalog: None,
            schema: Some(schema.to_string()),
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

    /// A window holding nothing but the dialog, already open over `schema`.
    fn dialog(
        connected: &Connected,
        schema: &str,
        output: &Path,
        cx: &mut TestAppContext,
    ) -> WindowHandle<BackupDialog> {
        cx.update(|cx| {
            rudbman_ui::init(cx);
        });
        let session = connected.handle();
        let scope = scope(schema);
        let output = output.to_path_buf();
        let window = cx.add_window(|_window, cx| BackupDialog::new(cx));
        window
            .update(cx, |dialog, _window, cx| {
                dialog.open(scope, session, cx);
                // The file picker would be a platform dialog; the field behind
                // it is what the picker sets.
                dialog.output = Some(output);
            })
            .expect("the window is open");
        window
    }

    /// Runs the poll loop to its end, moving the test clock the way the real
    /// one moves on its own.
    ///
    /// `run_until_parked` runs what is ready; it does *not* advance the
    /// simulated clock, so a loop that sleeps between readings has to be walked
    /// forward one interval at a time. The one real millisecond per turn is
    /// what lets the bridge thread — which is not simulated — actually move.
    fn drive(window: &WindowHandle<BackupDialog>, cx: &mut TestAppContext) {
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
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        panic!("the job never reported a terminal state");
    }

    #[test]
    fn the_form_becomes_the_specification_the_bridge_is_sent() {
        let scope = scope("PUBLIC");

        // Schema only: no data member is switched on at all, so the bridge
        // writes nothing but the CREATEs and the ALTERs.
        let ddl_only = BackupForm {
            output: Some(PathBuf::from("/tmp/app.sql")),
            ddl: true,
            include_drop: true,
            data: false,
            ..BackupForm::default()
        };
        let spec = build_spec(&scope, &ddl_only).expect("a path and the schema are enough");
        assert_eq!(spec.output.path, PathBuf::from("/tmp/app.sql"));
        assert_eq!(spec.scope.schema.as_deref(), Some("PUBLIC"));
        assert_eq!(spec.scope.catalog, None);
        assert_eq!(spec.compress, Compression::None);
        assert!(spec.ddl.include);
        assert!(spec.ddl.include_drop);
        // Never offered in the form, and never anything else: a whole schema
        // needs its foreign keys out of the CREATEs.
        assert_eq!(spec.ddl.constraints, Constraints::Alter);
        assert!(!spec.data.include);

        // Rows only, compressed, in batches.
        let rows = BackupForm {
            output: Some(PathBuf::from("/tmp/app.sql.gz")),
            compress: true,
            ddl: false,
            data: true,
            batch_rows: 100,
            ..BackupForm::default()
        };
        let spec = build_spec(&scope, &rows).expect("a path and the rows are enough");
        assert_eq!(spec.compress, Compression::Gzip);
        assert!(!spec.ddl.include);
        assert!(spec.data.include);
        assert_eq!(spec.data.insert_batch_rows, 100);

        // A count of zero would travel as a row count nobody meant; the form's
        // own field is clamped rather than sent.
        let zero = BackupForm {
            batch_rows: 0,
            ..rows.clone()
        };
        assert_eq!(
            build_spec(&scope, &zero)
                .expect("a path and the rows are enough")
                .data
                .insert_batch_rows,
            1
        );

        // A catalogue on the scope reaches the request; a blank half does not,
        // because "" is not a name — it is the absence of one, and the bridge
        // reads an absent member as "wherever the connection is pointed".
        let catalogued = Scope {
            catalog: Some("TESTDB".to_string()),
            schema: Some("   ".to_string()),
        };
        let spec = build_spec(&catalogued, &ddl_only).expect("a path and the schema are enough");
        assert_eq!(spec.scope.catalog.as_deref(), Some("TESTDB"));
        assert_eq!(spec.scope.schema, None);
    }

    #[test]
    fn an_incomplete_form_produces_no_specification_at_all() {
        let scope = scope("PUBLIC");

        // No output file: this is what disables the "Back up" button.
        assert!(
            build_spec(
                &scope,
                &BackupForm {
                    output: None,
                    ..BackupForm::default()
                }
            )
            .is_none()
        );

        // Neither half asked for: the job would write an empty file, which the
        // bridge refuses anyway — refusing it here keeps the button honest.
        assert!(
            build_spec(
                &scope,
                &BackupForm {
                    output: Some(PathBuf::from("/tmp/app.sql")),
                    ddl: false,
                    data: false,
                    ..BackupForm::default()
                }
            )
            .is_none()
        );
    }

    #[test]
    fn a_scope_reads_as_its_catalogue_and_its_schema() {
        assert_eq!(
            qualified(&Scope {
                catalog: Some("APP".to_string()),
                schema: Some("PUBLIC".to_string()),
            }),
            "APP.PUBLIC"
        );
        // Whichever half there is, and nothing when there is neither: the row
        // then falls back to a word rather than drawing a lone separator.
        assert_eq!(
            qualified(&Scope {
                catalog: None,
                schema: Some("PUBLIC".to_string()),
            }),
            "PUBLIC"
        );
        assert_eq!(
            qualified(&Scope {
                catalog: None,
                schema: None,
            }),
            ""
        );
    }

    /// Every half of the dialog lays out, in every shape the form can take.
    ///
    /// A render test rather than an assertion about pixels: the DROP toggle is
    /// only built with the schema and the batch field only with the rows, and a
    /// branch that is never drawn is a branch that can panic on the day it is.
    #[gpui::test]
    fn every_shape_of_the_dialog_lays_out(cx: &mut TestAppContext) {
        let connected = h2("backup-render", &["CREATE TABLE ORDERS (ID INT)"]);
        let dir = tempfile::tempdir().expect("a temporary directory");
        let window = dialog(&connected, "PUBLIC", &dir.path().join("app.sql"), cx);
        let mut cx = gpui::VisualTestContext::from_window(window.into(), cx);

        // What the save dialog is seeded with. The extension has to follow what
        // is actually written, or a gzip file goes out named `.sql` and nothing
        // will open it by double-click.
        window
            .update(&mut cx, |dialog, _window, _cx| {
                assert_eq!(dialog.suggested_name(), "PUBLIC-backup.sql");
                dialog.compress = true;
                assert_eq!(dialog.suggested_name(), "PUBLIC-backup.sql.gz");
                dialog.compress = false;
            })
            .expect("the window is open");

        for (ddl, data, compress) in [
            (true, true, false),
            (true, false, true),
            (false, true, false),
        ] {
            window
                .update(&mut cx, |dialog, _window, cx| {
                    dialog.ddl = ddl;
                    dialog.include_drop = ddl;
                    dialog.data = data;
                    dialog.compress = compress;
                    // And the "no file chosen" cell, which the helper filled in.
                    dialog.output = compress.then(|| dir.path().join("app.sql.gz"));
                    cx.notify();
                })
                .expect("the window is open");
            cx.run_until_parked();
        }

        // A scope with neither half named — SQLite's shape — falls back to a
        // word rather than drawing a blank row.
        window
            .update(&mut cx, |dialog, _window, cx| {
                dialog.scope = Some(Scope {
                    catalog: None,
                    schema: None,
                });
                cx.notify();
            })
            .expect("the window is open");
        cx.run_until_parked();

        // And the three cards a job leaves behind, without running one.
        for ended in [
            Ended::Done {
                rows: 12,
                bytes: 3_400,
            },
            Ended::Cancelled,
            Ended::Failed(SharedString::from("the schema went away")),
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
    /// is polled to its end, and the file on disk is a script holding every
    /// table of the scope.
    #[gpui::test]
    fn a_backup_writes_every_table_of_the_schema(cx: &mut TestAppContext) {
        let connected = h2(
            "backup-done",
            &[
                "CREATE SCHEMA APP",
                "CREATE TABLE APP.PARENT (ID INT PRIMARY KEY, NAME VARCHAR(20))",
                "CREATE TABLE APP.CHILD (ID INT PRIMARY KEY, PARENT_ID INT NOT NULL, \
                     CONSTRAINT FK_CHILD_PARENT FOREIGN KEY (PARENT_ID) \
                     REFERENCES APP.PARENT(ID))",
                "INSERT INTO APP.PARENT VALUES (1, 'a'), (2, 'b')",
                "INSERT INTO APP.CHILD VALUES (10, 1), (11, 2), (12, 1)",
            ],
        );
        let dir = tempfile::tempdir().expect("a temporary directory");
        let path = dir.path().join("app.sql");
        let window = dialog(&connected, "APP", &path, cx);

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
                // No object list was given: the bridge enumerated the schema
                // itself and wrote every row of both tables.
                assert_eq!(*rows, 5, "two parents and three children");
                assert!(*bytes > 0, "a script with five rows in it is not empty");
                assert!(
                    dialog.notice.is_none(),
                    "nothing was refused, so nothing is reported"
                );
            })
            .expect("the window is open");

        let script = std::fs::read_to_string(&path).expect("the job wrote the file");
        let upper = script.to_uppercase();
        assert!(upper.contains("CREATE TABLE"), "no schema in {script}");
        assert!(upper.contains("PARENT"), "a table is missing from {script}");
        assert!(upper.contains("CHILD"), "a table is missing from {script}");
        // The foreign key comes after the CREATEs as an ALTER, which is what
        // makes the script replayable whatever order the tables are in.
        assert!(upper.contains("ALTER TABLE"), "no foreign key in {script}");
        assert!(upper.contains("INSERT INTO"), "no rows in {script}");
    }

    /// Compression is the file's, not the counter's: the bytes on disk start
    /// with a gzip member and the card's count is what was written after
    /// compression.
    #[gpui::test]
    fn a_compressed_backup_writes_a_gzip_file(cx: &mut TestAppContext) {
        let connected = h2(
            "backup-gzip",
            &[
                "CREATE SCHEMA GZ",
                "CREATE TABLE GZ.ORDERS (ID INT PRIMARY KEY, STATE VARCHAR(16))",
                "INSERT INTO GZ.ORDERS VALUES (1, 'open'), (2, 'shipped')",
            ],
        );
        let dir = tempfile::tempdir().expect("a temporary directory");
        let path = dir.path().join("gz-backup.sql.gz");
        let window = dialog(&connected, "GZ", &path, cx);

        window
            .update(cx, |dialog, _window, cx| {
                dialog.compress = true;
                dialog.start(cx);
            })
            .expect("the window is open");
        drive(&window, cx);

        let written = window
            .update(cx, |dialog, _window, _cx| {
                let Stage::Ended(Ended::Done { rows, bytes }) = &dialog.stage else {
                    match &dialog.stage {
                        Stage::Ended(Ended::Failed(message)) => {
                            panic!("the job failed: {message}")
                        }
                        _ => panic!("the job did not finish cleanly"),
                    }
                };
                assert_eq!(*rows, 2);
                *bytes
            })
            .expect("the window is open");

        let bytes = std::fs::read(&path).expect("the job wrote the file");
        // Two magic bytes rather than a decompressor: this asserts that the
        // gzip flag reached the bridge, and `rudbman-jdbc`'s own H2 test
        // already asserts that the member unpacks.
        assert_eq!(
            &bytes[..2],
            &[0x1f, 0x8b],
            "a gzip member starts with its magic number, or nothing will unpack it"
        );
        assert_eq!(
            written,
            bytes.len() as u64,
            "the byte count is the compressed size, so it matches the file on disc"
        );
    }
}
