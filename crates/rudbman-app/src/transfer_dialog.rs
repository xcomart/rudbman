//! The transfer dialog: one explorer object's rows, into a table on a
//! connection the user picks.
//!
//! Opened over the object selected in the explorer, beside the extraction row
//! in the menu. It collects what `JOB_START`'s `transfer` kind needs — which
//! connection to write into, which table there, what writing a row means, and
//! what to do with a row that will not go in — starts the job on the *source*
//! session, and turns into a progress card until the job ends.
//!
//! # The same two halves the extraction has
//!
//! [`Stage`] is which one is on screen, the job is shared as an
//! `Arc<Mutex<Job>>`, dropping the polling task cancels, and `Escape` over a
//! running job is the cancel button rather than a close. All four are
//! [`crate::extract_dialog`]'s decisions and its module documentation is where
//! the reasoning lives; repeating it here would only let the two drift.
//!
//! # Two session handles, not one
//!
//! A transfer names its target by handle inside the specification, but a bare
//! `i64` keeps nothing alive: closing the target's tab would close the session
//! under a job that is still writing into it, and the bridge would cancel the
//! job. So the dialog holds a [`SessionHandle`] for *both* ends — the source it
//! reads with and the target it was pointed at — and both travel into the
//! polling task, which is what makes a tab closed mid-transfer harmless.
//!
//! # What the form deliberately does not offer
//!
//! `batch_size` and `commit_every` are left at the specification's defaults.
//! They are throughput knobs, not decisions about the result: every value of
//! them produces the same rows in the same table, and a form that asks four
//! questions instead of six is the one that gets answered correctly. The column
//! map is absent for a stronger reason — the source query is `SELECT *`, so the
//! source's own column names *are* the target's, which is exactly what an
//! absent map asks for.

use std::sync::Arc;

use gpui::{
    App, Context, Entity, EventEmitter, FocusHandle, Focusable, IntoElement, Render, SharedString,
    Task, Window, div, prelude::*, px,
};
use parking_lot::Mutex;
use rudbman_jdbc::{Job, JobProgress, JobState, ObjectRef, OnError, TransferMode, TransferSpec};
use rudbman_ui::{Button, ButtonVariant, Select, TextInput, form_row, modal, theme};

use crate::connection::SessionHandle;
use crate::explorer::{ConnectionId, ObjectTarget};
use crate::extract_dialog::{POLL_INTERVAL, non_empty};
use crate::i18n::ts;

/// Width of the dialog panel, matching the extraction's.
const DIALOG_WIDTH: f32 = 520.;

/// Tab order of the form, spaced so controls can be inserted without
/// renumbering.
mod tab {
    /// The connection to write into.
    pub const TARGET_CONNECTION: isize = 10;
    /// The schema there.
    pub const TARGET_SCHEMA: isize = 20;
    /// The table there.
    pub const TARGET_TABLE: isize = 21;
    /// What writing a row means.
    pub const MODE: isize = 30;
    /// The `WHERE` clause narrowing the source query.
    pub const WHERE: isize = 40;
    /// What a row the target refuses does to the job.
    pub const ON_ERROR: isize = 50;
    /// "Transfer".
    pub const START: isize = 60;
    /// "Cancel" / "Close".
    pub const DISMISS: isize = 61;
}

/// The write modes offered, in the order the dropdown lists them.
///
/// The labels are translated and the order is this array's, so `on_select` maps
/// an index back through here rather than comparing labels.
const MODES: [TransferMode; 3] = [
    TransferMode::Insert,
    TransferMode::Upsert,
    TransferMode::TruncateInsert,
];

/// The error policies offered, in dropdown order. Read like [`MODES`].
const POLICIES: [OnError; 3] = [OnError::Abort, OnError::Skip, OnError::Log];

/// Emitted by [`TransferDialog`] when the user closes it.
pub enum TransferDialogEvent {
    /// The dialog was dismissed; the shell should restore focus.
    Dismissed,
}

/// One connection a transfer can be pointed at.
///
/// Carries the [`SessionHandle`] rather than only the [`ConnectionId`], because
/// the dialog has to keep the target session standing for the length of the
/// job; see the module documentation. Named rather than a tuple: three members
/// of which two are opaque handles read as nothing at a call site.
#[derive(Clone)]
pub struct TransferTarget {
    /// Which tab this is.
    pub connection: ConnectionId,
    /// What the dropdown lists it as: the profile's name.
    pub name: SharedString,
    /// The session the rows are written through.
    pub session: SessionHandle,
}

/// Everything the form collects, apart from the object it was opened over and
/// the target session it was pointed at.
///
/// Pulled out of the widgets into a plain value so that [`build_spec`] is a
/// pure function and can be tested without a window — the shape the extraction
/// dialog established.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TransferForm {
    /// The schema the target table lives in. Blank means "wherever the target
    /// connection is pointed".
    pub target_schema: String,
    /// The target table's name. Blank is what disables the start button.
    pub target_table: String,
    /// What writing a row means.
    pub mode: TransferMode,
    /// A `WHERE` clause narrowing the source query, without the keyword.
    pub where_clause: String,
    /// What a row the target will not take does to the job.
    pub on_error: OnError,
}

impl Default for TransferForm {
    /// What the dialog would open with over an object with no name: a plain
    /// insert of every row, stopping at the first the target refuses.
    ///
    /// The two name fields are filled in from the source object by
    /// [`TransferDialog::open`]; a transfer into a table of the same name on
    /// another connection is what the command is for, and it is the one thing
    /// the user should not have to type.
    fn default() -> Self {
        TransferForm {
            target_schema: String::new(),
            target_table: String::new(),
            mode: TransferMode::default(),
            where_clause: String::new(),
            on_error: OnError::default(),
        }
    }
}

/// The specification `form` describes for `target`, or `None` when the form is
/// not ready to be sent.
///
/// Not ready means exactly one thing, and it is what the "Transfer" button is
/// disabled on: no target table named. Everything else the bridge judges,
/// because the bridge is the single authority on what a malformed request is —
/// an upsert into a table with no primary key is refused there, synchronously,
/// and shown in the dialog.
///
/// `target_session` is [`Session::handle`](rudbman_jdbc::Session::handle) of the
/// connection the dropdown is on. It is an argument rather than read out of the
/// form because a handle is not something a form collects: it is what the
/// session the dialog is holding happens to be called inside the bridge.
pub fn build_spec(
    target: &ObjectTarget,
    form: &TransferForm,
    target_session: i64,
) -> Option<TransferSpec> {
    let name = form.target_table.trim();
    if name.is_empty() {
        return None;
    }

    let mut table = ObjectRef::new(name);
    if let Some(schema) = non_empty(Some(&form.target_schema)) {
        table = table.with_schema(schema);
    }
    // No catalogue: the form does not ask for one. A target catalogue is
    // meaningful on two products out of the eight, and an absent one means
    // "wherever the target connection is pointed", which is what a user who
    // chose that connection already said.

    // `batch_size` and `commit_every` are left where `TransferSpec::new` puts
    // them; see the module documentation.
    Some(
        TransferSpec::new(
            source_sql(target, &form.where_clause),
            target_session,
            table,
        )
        .with_mode(form.mode)
        .with_on_error(form.on_error),
    )
}

/// The query the source session runs, as the explorer's own object reference
/// spells it.
///
/// `SELECT *` qualified by the schema, which is exactly what
/// [`Workspace::open_query_for`](crate::Workspace) puts in a query pane opened
/// over the same row: two commands over one explorer object have to read the
/// same rows, or "transfer this" would quietly mean a different table from
/// "query this".
fn source_sql(target: &ObjectTarget, where_clause: &str) -> String {
    let mut sql = format!("SELECT * FROM {}", target.qualified());
    let predicate = where_clause.trim();
    if !predicate.is_empty() {
        sql.push_str(" WHERE ");
        sql.push_str(predicate);
    }
    sql
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
/// No byte count: a transfer writes no file and the bridge leaves `bytes` at
/// zero, so a card showing it would only ever show "0 B".
struct Running {
    /// Shared with the polling task; see [`crate::extract_dialog`].
    job: Arc<Mutex<Job>>,
    /// The bridge's own phase text, or empty before the first reading.
    phase: SharedString,
    /// Rows written so far.
    rows_done: u64,
    /// Rows the target refused, under a policy that drops them.
    rows_skipped: u64,
    /// Whether a cancel has been issued and the job has not stopped yet.
    cancelling: bool,
}

/// How a job ended.
enum Ended {
    /// The source query was read to its end.
    Done {
        /// Rows written.
        rows: u64,
        /// Rows dropped, which is zero unless the policy allowed any.
        skipped: u64,
    },
    /// The job failed. The message is the first error's, as the bridge wrote it.
    Failed(SharedString),
    /// The job was cancelled. The rows already committed stay in the target,
    /// which is why the count is kept: it is the only record of what landed.
    Cancelled {
        /// Rows written before the cancel took.
        rows: u64,
    },
}

/// Modal dialog that copies one object's rows into a table on another
/// connection.
///
/// Create it once with [`TransferDialog::new`], keep the handle, subscribe to
/// [`TransferDialogEvent`], and render it as the last child of a `relative()`
/// root. It renders nothing while [`TransferDialog::is_open`] is `false`.
pub struct TransferDialog {
    /// Whether the dialog is currently visible.
    open: bool,
    /// Focus of the dialog root; the anchor the shell's `Escape` resolves
    /// against.
    focus_handle: FocusHandle,
    /// Whether focus should move into the dialog on the next render.
    pending_focus: bool,
    /// The object the dialog was opened over, captured at that moment.
    target: Option<ObjectTarget>,
    /// The session the source query runs on, and which `JOB_START` is called
    /// on.
    source: Option<SessionHandle>,
    /// Every open connection, as the dropdown lists them.
    targets: Vec<TransferTarget>,
    /// Index into [`TransferDialog::targets`] of the chosen one.
    selected_target: usize,
    /// Whether the connection dropdown is showing its list.
    connection_list_open: bool,
    /// The target schema.
    schema_input: Entity<TextInput>,
    /// The target table's name.
    table_input: Entity<TextInput>,
    /// What writing a row means.
    mode: TransferMode,
    /// Whether the mode dropdown is showing its list.
    mode_list_open: bool,
    /// The `WHERE` clause, without the keyword.
    where_input: Entity<TextInput>,
    /// What a refused row does to the job.
    on_error: OnError,
    /// Whether the policy dropdown is showing its list.
    on_error_list_open: bool,
    /// Which half of the dialog is on screen.
    stage: Stage,
    /// A refusal to show under the form — a specification the bridge would not
    /// accept.
    notice: Option<SharedString>,
    /// The task that starts the job and then polls it.
    ///
    /// Dropping it cancels the job.
    _poll: Option<Task<()>>,
}

impl TransferDialog {
    /// Builds the dialog, closed.
    pub fn new(cx: &mut Context<Self>) -> Self {
        // No translated placeholders: these entities are built once, when the
        // shell is, and a placeholder captured then would still be in the old
        // language after the settings dialog switches it. The sample predicate
        // is not a word and reads the same everywhere.
        let schema_input = cx.new(|cx| TextInput::new(cx).tab_index(tab::TARGET_SCHEMA));
        let table_input = cx.new(|cx| TextInput::new(cx).tab_index(tab::TARGET_TABLE));
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
            source: None,
            targets: Vec::new(),
            selected_target: 0,
            connection_list_open: false,
            schema_input,
            table_input,
            mode: TransferMode::default(),
            mode_list_open: false,
            where_input,
            on_error: OnError::default(),
            on_error_list_open: false,
            stage: Stage::Form,
            notice: None,
            _poll: None,
        }
    }

    /// Shows the dialog over `target`, reading through `source`.
    ///
    /// `targets` is every connection the rows could go into, the source
    /// included: copying a table into another schema of the same database is a
    /// real transfer and the bridge's lock is reentrant, so refusing it here
    /// would only be a rule the engine does not have.
    ///
    /// The form starts from the source object every time: the same schema, the
    /// same table name, on the connection the object came from. That is the
    /// shape of the command — "this table, over there" — and it is a form the
    /// user finishes by changing one dropdown.
    pub fn open(
        &mut self,
        target: ObjectTarget,
        source: SessionHandle,
        targets: Vec<TransferTarget>,
        cx: &mut Context<Self>,
    ) {
        let defaults = TransferForm::default();
        self.selected_target = targets
            .iter()
            .position(|candidate| candidate.connection == target.connection)
            .unwrap_or(0);
        self.targets = targets;
        let schema = target.schema.clone().unwrap_or_default();
        let table = target.name.clone();
        self.schema_input
            .update(cx, |input, cx| input.set_content(schema, cx));
        self.table_input
            .update(cx, |input, cx| input.set_content(table, cx));
        self.where_input.update(cx, |input, cx| input.clear(cx));
        self.target = Some(target);
        self.source = Some(source);
        self.mode = defaults.mode;
        self.on_error = defaults.on_error;
        self.connection_list_open = false;
        self.mode_list_open = false;
        self.on_error_list_open = false;
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
    /// [`crate::extract_dialog`] documents: the polling task goes, nobody holds
    /// the `Job`, and its `Drop` asks the bridge to stop. Rows already
    /// committed on the target stay there, which is the contract §6 states and
    /// what [`Ended::Cancelled`] reports while the card is still up.
    pub fn close(&mut self, cx: &mut Context<Self>) {
        self.open = false;
        self.pending_focus = false;
        self.connection_list_open = false;
        self.mode_list_open = false;
        self.on_error_list_open = false;
        self.stage = Stage::Form;
        self._poll = None;
        self.source = None;
        self.targets = Vec::new();
        self.target = None;
        self.notice = None;
        cx.notify();
    }

    /// Closes the dialog and reports it, so the shell can restore focus.
    pub fn dismiss(&mut self, cx: &mut Context<Self>) {
        cx.emit(TransferDialogEvent::Dismissed);
        self.close(cx);
    }

    /// What `Escape` means here, which depends on what is on screen.
    ///
    /// A dropdown takes it first — there are three of them — then a running
    /// job, where `Escape` is the cancel button and not a close. Only a form or
    /// a finished job is thrown away by it.
    pub fn escape(&mut self, cx: &mut Context<Self>) {
        if self.connection_list_open || self.mode_list_open || self.on_error_list_open {
            self.connection_list_open = false;
            self.mode_list_open = false;
            self.on_error_list_open = false;
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
    fn form(&self, cx: &App) -> TransferForm {
        TransferForm {
            target_schema: self.schema_input.read(cx).content().to_owned(),
            target_table: self.table_input.read(cx).content().to_owned(),
            mode: self.mode,
            where_clause: self.where_input.read(cx).content().to_owned(),
            on_error: self.on_error,
        }
    }

    /// The connection the rows go into.
    fn chosen(&self) -> Option<&TransferTarget> {
        self.targets.get(self.selected_target)
    }

    /// The specification the form describes, or `None` while it is incomplete.
    fn spec(&self, cx: &App) -> Option<TransferSpec> {
        let handle = self.chosen()?.session.session().handle();
        build_spec(self.target.as_ref()?, &self.form(cx), handle)
    }

    /// The target table as the result card names it.
    fn target_name(&self, cx: &App) -> String {
        let form = self.form(cx);
        match non_empty(Some(&form.target_schema)) {
            Some(schema) => format!("{schema}.{}", form.target_table.trim()),
            None => form.target_table.trim().to_owned(),
        }
    }

    /// Starts the job the form describes, and begins polling it.
    ///
    /// `start_transfer` is called on the **source** session and goes on a
    /// background task like everything else that talks to the bridge. A
    /// specification the bridge refuses — an unknown target handle, an upsert
    /// into a table with no primary key — comes back as an error and lands in
    /// the notice line with the form still up, because the fix is in the form.
    fn start(&mut self, cx: &mut Context<Self>) {
        if !matches!(self.stage, Stage::Form) {
            return;
        }
        let (Some(spec), Some(source), Some(target)) = (
            self.spec(cx),
            self.source.clone(),
            self.chosen().map(|chosen| chosen.session.clone()),
        ) else {
            return;
        };
        self.notice = None;

        // Both handles travel into the task and back out of the first step.
        // They are what keep the two sessions — and any tunnels under them —
        // alive for the length of the job; the job itself carries no claim on
        // either, and the target's is named only by an `i64` in the spec.
        let starting = cx.background_spawn(async move {
            let outcome = source.session().start_transfer(&spec);
            (source, target, outcome)
        });

        self._poll = Some(cx.spawn(async move |dialog, cx| {
            let (_source, _target, outcome) = starting.await;
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
            rows_skipped: 0,
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
                self.stage = Stage::Ended(Ended::Failed(SharedString::from(format!("{error}"))));
                cx.notify();
                return false;
            }
        };

        running.phase = SharedString::from(progress.phase.clone());
        running.rows_done = progress.rows_done;
        running.rows_skipped = progress.rows_skipped;

        if !progress.is_terminal() {
            cx.notify();
            return true;
        }

        self.stage = Stage::Ended(match progress.state {
            JobState::Done => Ended::Done {
                rows: progress.rows_done,
                skipped: progress.rows_skipped,
            },
            JobState::Cancelled => Ended::Cancelled {
                rows: progress.rows_done,
            },
            // `Running` cannot reach here — `is_terminal` excluded it — and a
            // failure without an error envelope is a bridge that broke its own
            // contract, so the fallback says so rather than showing nothing.
            JobState::Failed | JobState::Running => Ended::Failed(
                progress
                    .errors
                    .first()
                    .map(|error| SharedString::from(error.message.clone()))
                    .unwrap_or_else(|| ts!("transfer.failed_unknown")),
            ),
        });
        cx.notify();
        false
    }

    /// Asks the job to stop.
    ///
    /// The card stays up: a cancel reaches both statements — the source
    /// `SELECT` and the target batch — and the state that follows is still read
    /// by a poll.
    fn request_cancel(&mut self, cx: &mut Context<Self>) {
        let Stage::Running(running) = &mut self.stage else {
            return;
        };
        if running.cancelling {
            return;
        }
        running.cancelling = true;
        // Taken on the UI thread rather than handed to a task: the bridge's
        // cancel is detached and quick, and a task would arrive after the work
        // it was meant to stop.
        if let Err(error) = running.job.lock().cancel() {
            log::warn!("cancelling the transfer failed: {error}");
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

    /// The source object row, which every half of the dialog shows.
    fn render_object(&self, cx: &App) -> impl IntoElement + use<> {
        let chrome = theme(cx);
        let name = self
            .target
            .as_ref()
            .map(ObjectTarget::qualified)
            .unwrap_or_default();
        form_row(
            ts!("transfer.object"),
            div().text_color(chrome.text).child(name),
        )
    }

    /// The form half: everything the job is described by.
    fn render_form(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let this = cx.entity();

        let connection = Select::new("transfer-connection")
            .options(
                self.targets
                    .iter()
                    .map(|candidate| candidate.name.clone())
                    .collect::<Vec<_>>(),
            )
            .selected(self.chosen().map(|chosen| chosen.name.clone()))
            .open(self.connection_list_open)
            .tab_index(tab::TARGET_CONNECTION)
            .on_select({
                let this = this.clone();
                // By index: two profiles may legitimately share a name, and
                // what is being chosen is a session rather than a label.
                move |index, _label, _window, cx| {
                    this.update(cx, |dialog, cx| {
                        if index < dialog.targets.len() {
                            dialog.selected_target = index;
                            cx.notify();
                        }
                    });
                }
            })
            .on_open_change({
                let this = this.clone();
                move |open, _window, cx| {
                    this.update(cx, |dialog, cx| {
                        dialog.connection_list_open = open;
                        cx.notify();
                    });
                }
            });

        let mode = Select::new("transfer-mode")
            .options(MODES.iter().map(|mode| mode_label(*mode)))
            .selected(Some(mode_label(self.mode)))
            .open(self.mode_list_open)
            .tab_index(tab::MODE)
            .on_select({
                let this = this.clone();
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

        let on_error = Select::new("transfer-on-error")
            .options(POLICIES.iter().map(|policy| policy_label(*policy)))
            .selected(Some(policy_label(self.on_error)))
            .open(self.on_error_list_open)
            .tab_index(tab::ON_ERROR)
            .on_select({
                let this = this.clone();
                move |index, _label, _window, cx| {
                    let Some(policy) = POLICIES.get(index).copied() else {
                        return;
                    };
                    this.update(cx, |dialog, cx| {
                        dialog.on_error = policy;
                        cx.notify();
                    });
                }
            })
            .on_open_change({
                let this = this.clone();
                move |open, _window, cx| {
                    this.update(cx, |dialog, cx| {
                        dialog.on_error_list_open = open;
                        cx.notify();
                    });
                }
            });

        div()
            .flex()
            .flex_col()
            .gap(px(10.))
            .child(self.render_object(cx))
            .child(form_row(ts!("transfer.target_connection"), connection))
            .child(form_row(
                ts!("transfer.target_schema"),
                self.schema_input.clone(),
            ))
            .child(form_row(
                ts!("transfer.target_table"),
                self.table_input.clone(),
            ))
            .child(form_row(ts!("transfer.mode"), mode))
            .child(form_row(ts!("transfer.where"), self.where_input.clone()))
            .child(form_row(ts!("transfer.on_error"), on_error))
    }

    /// The progress half: what the job is doing, and how far it has got.
    fn render_running(
        &self,
        running: &Running,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let chrome = theme(cx);
        let phase = if running.phase.is_empty() {
            ts!("transfer.starting")
        } else {
            running.phase.clone()
        };
        // Only once something has actually been dropped: a permanent "0
        // skipped" beside the row count is a number that says nothing, and its
        // appearing is the signal.
        let skipped = (running.rows_skipped > 0).then(|| {
            div()
                .text_size(px(12.))
                .text_color(chrome.danger)
                .child(ts!("transfer.skipped", skipped = running.rows_skipped))
        });

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
                        ts!("transfer.cancelling")
                    } else {
                        phase
                    }),
            )
            .child(
                div()
                    .text_color(chrome.text)
                    .child(ts!("transfer.counters", rows = running.rows_done)),
            )
            .children(skipped)
    }

    /// The card a finished job leaves behind.
    fn render_ended(&self, ended: &Ended, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let chrome = theme(cx);
        let table = self.target_name(cx);

        let (message, color) = match ended {
            Ended::Done { rows, skipped } => {
                let mut message = ts!("transfer.done", rows = rows, table = table).to_string();
                if *skipped > 0 {
                    message.push(' ');
                    message.push_str(&ts!("transfer.done_skipped", skipped = skipped));
                }
                (SharedString::from(message), chrome.success)
            }
            // The committed rows are named because they are still there: a
            // cancelled transfer is not an undone one, and a user who does not
            // know how much landed cannot decide what to do next.
            Ended::Cancelled { rows } => (
                ts!("transfer.cancelled", rows = rows, table = table),
                chrome.text,
            ),
            Ended::Failed(message) => (
                ts!("transfer.failed", error = message.as_ref()),
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
                        Button::new("transfer-cancel", ts!("common.cancel"))
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
                        Button::new("transfer-start", ts!("transfer.start"))
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
                Button::new("transfer-stop", ts!("common.cancel"))
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
                Button::new("transfer-close", ts!("common.close"))
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

/// The label the dropdown lists one write mode under.
fn mode_label(mode: TransferMode) -> SharedString {
    match mode {
        TransferMode::Insert => ts!("transfer.mode_insert"),
        TransferMode::Upsert => ts!("transfer.mode_upsert"),
        TransferMode::TruncateInsert => ts!("transfer.mode_truncate"),
    }
}

/// The label the dropdown lists one error policy under.
fn policy_label(policy: OnError) -> SharedString {
    match policy {
        OnError::Abort => ts!("transfer.on_error_abort"),
        OnError::Skip => ts!("transfer.on_error_skip"),
        OnError::Log => ts!("transfer.on_error_log"),
    }
}

impl EventEmitter<TransferDialogEvent> for TransferDialog {}

impl Focusable for TransferDialog {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for TransferDialog {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if !self.open {
            return div().id("transfer-dialog");
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
            .id("transfer-dialog")
            .absolute()
            .inset_0()
            .size_full()
            .track_focus(&self.focus_handle)
            .child(modal(
                "transfer-modal",
                ts!("transfer.title"),
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
    use rudbman_jdbc::{StatementSpec, Value};

    use super::*;
    use crate::connection::{self, Connected};
    use crate::explorer::Folder;

    /// The source object every test below transfers.
    fn object(name: &str) -> ObjectTarget {
        ObjectTarget {
            connection: ConnectionId(1),
            catalog: None,
            schema: Some("PUBLIC".to_string()),
            folder: Folder::Tables,
            name: name.to_string(),
        }
    }

    /// A live H2 database with `setup` already run against it.
    ///
    /// Every call opens a database of its own — the profile carries a fresh
    /// name — which is what makes a transfer between two of these a transfer
    /// between two databases rather than one session talking to itself.
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

    /// How many rows one table holds, read on the calling thread.
    fn count(connected: &Connected, table: &str) -> i64 {
        let cursor = connected
            .session()
            .execute(&StatementSpec::new(format!("SELECT COUNT(*) FROM {table}")))
            .expect("the count runs");
        let batch = cursor.fetch(1).expect("the count has a row");
        match batch.value(0, 0) {
            Some(Value::I64(count)) => count,
            other => panic!("expected a count, got {other:?}"),
        }
    }

    /// A window holding nothing but the dialog, open over `name` on `source`
    /// and already pointed at `target`.
    fn dialog(
        source: &Connected,
        target: &Connected,
        name: &str,
        cx: &mut TestAppContext,
    ) -> WindowHandle<TransferDialog> {
        cx.update(|cx| {
            rudbman_ui::init(cx);
        });
        let candidates = vec![
            TransferTarget {
                connection: ConnectionId(1),
                name: SharedString::from("source"),
                session: source.handle(),
            },
            TransferTarget {
                connection: ConnectionId(2),
                name: SharedString::from("target"),
                session: target.handle(),
            },
        ];
        let object = object(name);
        let session = source.handle();
        let window = cx.add_window(|_window, cx| TransferDialog::new(cx));
        window
            .update(cx, |dialog, _window, cx| {
                dialog.open(object, session, candidates, cx);
                // The dropdown would be a click; the field behind it is what
                // the click sets.
                dialog.selected_target = 1;
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
    fn drive(window: &WindowHandle<TransferDialog>, cx: &mut TestAppContext) {
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
        let source = object("ORDERS");

        // The default form, which is what opening the dialog over an object
        // produces: the same name in the same schema, inserted, stopping at the
        // first row the target refuses.
        let plain = TransferForm {
            target_schema: "PUBLIC".to_string(),
            target_table: "ORDERS".to_string(),
            ..TransferForm::default()
        };
        let spec = build_spec(&source, &plain, 7).expect("a target table is all that is required");
        assert_eq!(spec.source_sql, "SELECT * FROM PUBLIC.ORDERS");
        assert_eq!(spec.target_session, 7);
        assert_eq!(spec.target_table.name, "ORDERS");
        assert_eq!(spec.target_table.schema.as_deref(), Some("PUBLIC"));
        // Never asked for and never sent: the form has no catalogue field.
        assert_eq!(spec.target_table.catalog, None);
        assert_eq!(spec.mode, TransferMode::Insert);
        assert_eq!(spec.on_error, OnError::Abort);
        // The throughput defaults the form deliberately does not offer.
        assert_eq!(spec.batch_size, 500);
        assert_eq!(spec.commit_every, 10_000);
        // Absent, which is what asks the bridge to use the source result set's
        // own column names — and `SELECT *` means those are the source
        // table's.
        assert!(spec.column_map.is_empty());

        // The WHERE joins the query with its keyword, trimmed the way the
        // extraction dialog trims its own.
        let narrowed = TransferForm {
            target_table: "ORDERS".to_string(),
            where_clause: "  state = 'open'  ".to_string(),
            ..plain.clone()
        };
        assert_eq!(
            build_spec(&source, &narrowed, 1)
                .expect("a target table is enough")
                .source_sql,
            "SELECT * FROM PUBLIC.ORDERS WHERE state = 'open'"
        );

        // A blank target schema is no schema, not an empty one: the rows go
        // wherever the target connection is pointed.
        let unscoped = TransferForm {
            target_schema: "   ".to_string(),
            ..plain.clone()
        };
        assert_eq!(
            build_spec(&source, &unscoped, 1)
                .expect("a target table is enough")
                .target_table
                .schema,
            None
        );

        // A source with no schema of its own queries the bare name.
        let mut bare = source.clone();
        bare.schema = None;
        assert_eq!(
            build_spec(&bare, &plain, 1)
                .expect("a target table is enough")
                .source_sql,
            "SELECT * FROM ORDERS"
        );

        // Every dropdown value reaches the wire, and the pairs are the ones
        // the labels claim.
        for (mode, policy) in [
            (TransferMode::Insert, OnError::Abort),
            (TransferMode::Upsert, OnError::Skip),
            (TransferMode::TruncateInsert, OnError::Log),
        ] {
            let form = TransferForm {
                mode,
                on_error: policy,
                ..plain.clone()
            };
            let spec = build_spec(&source, &form, 1).expect("a target table is enough");
            assert_eq!(spec.mode, mode);
            assert_eq!(spec.on_error, policy);
        }
    }

    #[test]
    fn without_a_target_table_there_is_nothing_to_send() {
        let source = object("ORDERS");

        // This is what disables the "Transfer" button. A schema alone names no
        // table, and a name of nothing but spaces is not a name.
        for table in ["", "   "] {
            assert!(
                build_spec(
                    &source,
                    &TransferForm {
                        target_schema: "PUBLIC".to_string(),
                        target_table: table.to_string(),
                        ..TransferForm::default()
                    },
                    1
                )
                .is_none(),
                "{table:?} was accepted as a table name"
            );
        }

        // And the name it does send is trimmed, so a stray space cannot become
        // part of an identifier the target has to quote.
        assert_eq!(
            build_spec(
                &source,
                &TransferForm {
                    target_table: "  ORDERS  ".to_string(),
                    ..TransferForm::default()
                },
                1
            )
            .expect("a name with spaces round it is still a name")
            .target_table
            .name,
            "ORDERS"
        );
    }

    /// Every half of the dialog lays out, in every shape it can take.
    ///
    /// A render test rather than an assertion about pixels: the three dropdowns
    /// and the skipped-row line are built conditionally, and a branch that is
    /// never drawn is a branch that can panic on the day it is.
    #[gpui::test]
    fn every_shape_of_the_dialog_lays_out(cx: &mut TestAppContext) {
        let source = h2("transfer-render-source", &["CREATE TABLE ORDERS (ID INT)"]);
        let target = h2("transfer-render-target", &["CREATE TABLE ORDERS (ID INT)"]);
        let window = dialog(&source, &target, "ORDERS", cx);
        let mut cx = gpui::VisualTestContext::from_window(window.into(), cx);

        for (mode, policy, list) in [
            (TransferMode::Insert, OnError::Abort, 0),
            (TransferMode::Upsert, OnError::Skip, 1),
            (TransferMode::TruncateInsert, OnError::Log, 2),
        ] {
            window
                .update(&mut cx, |dialog, _window, cx| {
                    dialog.mode = mode;
                    dialog.on_error = policy;
                    dialog.connection_list_open = list == 0;
                    dialog.mode_list_open = list == 1;
                    dialog.on_error_list_open = list == 2;
                    cx.notify();
                })
                .expect("the window is open");
            cx.run_until_parked();
        }

        // The progress card, which needs a real job to hold: `Running` owns
        // one and there is no way to build the card without it. The job is
        // started here rather than through `start`, so that nothing polls it
        // and the card can be posed in each of the shapes it draws. Outside the
        // update because the call blocks, which is the rule the shell itself
        // follows.
        let spec = build_spec(
            &object("ORDERS"),
            &TransferForm {
                target_schema: "PUBLIC".to_string(),
                target_table: "ORDERS".to_string(),
                ..TransferForm::default()
            },
            target.session().handle(),
        )
        .expect("a target table is enough");
        let job = source
            .session()
            .start_transfer(&spec)
            .expect("the specification is accepted");
        let job = Arc::new(Mutex::new(job));
        window
            .update(&mut cx, |dialog, _window, cx| {
                dialog.enter_progress(job, cx);
            })
            .expect("the window is open");
        cx.run_until_parked();

        for (skipped, cancelling) in [(0, false), (4, false), (4, true)] {
            window
                .update(&mut cx, |dialog, _window, cx| {
                    let Stage::Running(running) = &mut dialog.stage else {
                        panic!("the card was entered above");
                    };
                    running.rows_skipped = skipped;
                    running.cancelling = cancelling;
                    running.phase = SharedString::from("transfer");
                    cx.notify();
                })
                .expect("the window is open");
            cx.run_until_parked();
        }

        // And the three cards a job leaves behind, without running one.
        for ended in [
            Ended::Done {
                rows: 120,
                skipped: 0,
            },
            Ended::Done {
                rows: 118,
                skipped: 2,
            },
            Ended::Cancelled { rows: 40 },
            Ended::Failed(SharedString::from("the target table went away")),
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

    /// The whole path, against two real databases: the form becomes a job, the
    /// job is polled to its end, and the rows are in the other database.
    #[gpui::test]
    fn a_transfer_moves_the_rows_into_the_other_connection(cx: &mut TestAppContext) {
        let source = h2(
            "transfer-done-source",
            &[
                "CREATE TABLE ORDERS (ID INT PRIMARY KEY, STATE VARCHAR(16))",
                "INSERT INTO ORDERS VALUES (1, 'open'), (2, 'shipped'), (3, 'open')",
            ],
        );
        let target = h2(
            "transfer-done-target",
            &["CREATE TABLE ORDERS (ID INT PRIMARY KEY, STATE VARCHAR(16))"],
        );
        let window = dialog(&source, &target, "ORDERS", cx);

        window
            .update(cx, |dialog, _window, cx| dialog.start(cx))
            .expect("the window is open");
        drive(&window, cx);

        window
            .update(cx, |dialog, _window, _cx| {
                let Stage::Ended(Ended::Done { rows, skipped }) = &dialog.stage else {
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
                assert_eq!(*skipped, 0, "nothing was refused");
                assert!(
                    dialog.notice.is_none(),
                    "nothing was refused, so nothing is reported"
                );
            })
            .expect("the window is open");

        assert_eq!(count(&target, "ORDERS"), 3, "the rows are in the target");
        assert_eq!(count(&source, "ORDERS"), 3, "the source kept its own");
    }

    /// A specification the bridge will not accept is a refusal, not a job: the
    /// form stays up with the reason under it.
    #[gpui::test]
    fn a_refused_specification_keeps_the_form_up(cx: &mut TestAppContext) {
        let source = h2(
            "transfer-refused-source",
            &[
                "CREATE TABLE ORDERS (ID INT PRIMARY KEY, STATE VARCHAR(16))",
                "INSERT INTO ORDERS VALUES (1, 'open')",
            ],
        );
        // No primary key on the target, so an upsert has no conflict key to
        // read — the one refusal the form deliberately does not pre-empt,
        // because the bridge is the authority on what a malformed request is.
        let target = h2(
            "transfer-refused-target",
            &["CREATE TABLE ORDERS (ID INT, STATE VARCHAR(16))"],
        );
        let window = dialog(&source, &target, "ORDERS", cx);

        window
            .update(cx, |dialog, _window, cx| {
                dialog.mode = TransferMode::Upsert;
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
        assert_eq!(
            count(&target, "ORDERS"),
            0,
            "a specification that was never accepted moved nothing"
        );
    }

    /// `skip` survives the rows the target refuses, and the card counts them.
    #[gpui::test]
    fn skipped_rows_are_counted_and_shown(cx: &mut TestAppContext) {
        let source = h2(
            "transfer-skip-source",
            &[
                "CREATE TABLE ORDERS (ID INT PRIMARY KEY, STATE VARCHAR(16))",
                "INSERT INTO ORDERS VALUES (1, 'open'), (2, 'shipped'), (3, 'open')",
            ],
        );
        // Two of the three keys are already sitting in the target, so two rows
        // cannot go in.
        let target = h2(
            "transfer-skip-target",
            &[
                "CREATE TABLE ORDERS (ID INT PRIMARY KEY, STATE VARCHAR(16))",
                "INSERT INTO ORDERS VALUES (1, 'sitting'), (3, 'sitting')",
            ],
        );
        let window = dialog(&source, &target, "ORDERS", cx);

        window
            .update(cx, |dialog, _window, cx| {
                dialog.on_error = OnError::Skip;
                dialog.start(cx);
            })
            .expect("the window is open");
        drive(&window, cx);

        window
            .update(cx, |dialog, _window, _cx| {
                let Stage::Ended(Ended::Done { rows, skipped }) = &dialog.stage else {
                    panic!("`skip` means the job survives its bad rows");
                };
                assert_eq!(*rows, 1, "the one row that fitted");
                assert_eq!(*skipped, 2, "the two that clashed");
            })
            .expect("the window is open");

        // The rows already there were left alone, and the one that fitted went
        // in beside them.
        assert_eq!(count(&target, "ORDERS"), 3);
    }

    /// Cancelling ends the job as cancelled and leaves both sessions usable.
    #[gpui::test]
    fn cancelling_stops_the_transfer_and_leaves_the_sessions_usable(cx: &mut TestAppContext) {
        // Not a big table but a slow one, for the extraction dialog's reason: a
        // big table only loses the race on a fast enough disk, whereas a view
        // that sleeps a millisecond per row cannot reach the end inside the
        // test's lifetime.
        let source = h2(
            "transfer-cancel-source",
            &[
                "CREATE ALIAS NAP AS 'long nap(long ms) throws Exception { \
                     Thread.sleep(ms); return ms; }'",
                "CREATE VIEW BIG AS SELECT X AS ID, NAP(1) AS PAUSE \
                     FROM SYSTEM_RANGE(1, 400000)",
            ],
        );
        let target = h2(
            "transfer-cancel-target",
            &["CREATE TABLE BIG (ID BIGINT, PAUSE BIGINT)"],
        );
        let window = dialog(&source, &target, "BIG", cx);

        window
            .update(cx, |dialog, _window, cx| dialog.start(cx))
            .expect("the window is open");
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
                    matches!(dialog.stage, Stage::Ended(Ended::Cancelled { .. })),
                    "a cancelled job ends as cancelled, not as done or failed"
                );
            })
            .expect("the window is open");

        // Neither session is left holding its connection lock: the source had a
        // SELECT cancelled underneath it and the target a batch. Counting a
        // plain range on the source, not the sleeping view, so the check itself
        // is instant.
        assert_eq!(count(&source, "SYSTEM_RANGE(1, 10)"), 10);
        assert!(
            count(&target, "BIG") < 400_000,
            "a cancel that only landed after the last row proves nothing"
        );
    }
}
