//! The restore dialog: a script file, run against the scope it was opened
//! over.
//!
//! Opened from the same places as the backup dialog — a scope in the
//! explorer — it collects the file and whether a failed statement ends the
//! run, then hands both to [`crate::restore`] and turns into a progress card.
//! The script is read the way the product's own client reads it, so a schema
//! backup, `mysqldump` or `pg_dump --inserts` output, an SSMS script or a SQL
//! Developer export all run as they would from their client; the module
//! documentation of [`rudbman_sql::script`] lists what each client changes.
//!
//! The halves of the dialog, closing as a cancel, and `Escape` over a running
//! restore asking first are the backup dialog's decisions, made here for the
//! same reasons.

use std::path::PathBuf;
use std::sync::Arc;

use gpui::{
    App, Context, EventEmitter, FocusHandle, Focusable, IntoElement, PathPromptOptions, Render,
    SharedString, Task, Window, div, prelude::*, px,
};
use rudbman_sql::Dialect;
use rugpui::{Button, ButtonVariant, Checkbox, form_row, modal, theme};

use crate::connection::SessionHandle;
use crate::explorer::Scope;
use crate::extract_dialog::{POLL_INTERVAL, format_bytes};
use crate::i18n::ts;
use crate::restore::{Failure, Outcome, Phase, Progress, Refusal, Restore, RestoreRequest};

/// Width of the dialog panel, matching the backup's.
const DIALOG_WIDTH: f32 = 520.;

/// How many passed failures the finished card names before it counts the rest.
const SHOWN_FAILURES: usize = 5;

/// Tab order of the form.
mod tab {
    /// "Browse…" beside the file.
    pub const INPUT: isize = 10;
    /// "Continue past failed statements".
    pub const CONTINUE: isize = 20;
    /// "Restore".
    pub const START: isize = 50;
    /// "Cancel" / "Close".
    pub const DISMISS: isize = 51;
}

/// Emitted by [`RestoreDialog`] when the user closes it.
pub enum RestoreDialogEvent {
    /// The dialog was dismissed; the shell should restore focus.
    Dismissed,
}

/// Which half of the dialog is on screen.
enum Stage {
    /// Collecting the form.
    Form,
    /// A restore is running.
    Running(Running),
    /// The restore ended, and the card says how.
    Ended(Outcome),
}

/// A restore in flight, and the last reading taken of it.
struct Running {
    restore: Arc<Restore>,
    progress: Progress,
    /// Whether a cancel has been issued and the restore has not stopped yet.
    canceling: bool,
}

/// Modal dialog that runs one script file against a scope.
///
/// Create it once with [`RestoreDialog::new`], keep the handle, subscribe to
/// [`RestoreDialogEvent`], and render it as the last child of a `relative()`
/// root. It renders nothing while [`RestoreDialog::is_open`] is `false`.
pub struct RestoreDialog {
    open: bool,
    focus_handle: FocusHandle,
    pending_focus: bool,
    /// The scope the dialog was opened over, captured at that moment.
    scope: Option<Scope>,
    /// The connection's session, which the restore opens its own beside.
    ///
    /// Held while the dialog is open, so that a connection tab closed
    /// mid-restore leaves the tunnel standing.
    session: Option<SessionHandle>,
    /// The connection's dialect, which the script is read in.
    dialect: Dialect,
    /// Whether the connection's profile is read-only, which refuses the form.
    read_only: bool,
    /// The script.
    input: Option<PathBuf>,
    /// Whether a failed statement is passed rather than the end of the run.
    continue_on_error: bool,
    stage: Stage,
    /// Whether `Escape` over a running restore asked "stop it?".
    asking_stop: bool,
    /// The task that polls the restore. Dropping it stops the polling; the
    /// restore itself is canceled by [`RestoreDialog::close`].
    _poll: Option<Task<()>>,
    label_width: gpui::Pixels,
}

impl RestoreDialog {
    /// Builds the dialog, closed.
    pub fn new(cx: &mut Context<Self>) -> Self {
        Self {
            open: false,
            focus_handle: cx.focus_handle(),
            pending_focus: false,
            scope: None,
            session: None,
            dialect: Dialect::GENERIC,
            read_only: false,
            input: None,
            continue_on_error: false,
            stage: Stage::Form,
            asking_stop: false,
            _poll: None,
            label_width: px(rugpui::modal::FORM_LABEL_WIDTH),
        }
    }

    /// Shows the dialog over `scope`, restoring through a session opened
    /// beside `session`, reading scripts in `dialect`.
    ///
    /// The form starts empty every time: a restore of yesterday's file into
    /// whatever is selected today is the mistake a remembered path makes easy.
    pub fn open(
        &mut self,
        scope: Scope,
        session: SessionHandle,
        dialect: Dialect,
        read_only: bool,
        cx: &mut Context<Self>,
    ) {
        self.scope = Some(scope);
        self.session = Some(session);
        self.dialect = dialect;
        self.read_only = read_only;
        self.input = None;
        self.continue_on_error = false;
        self.stage = Stage::Form;
        self.asking_stop = false;
        self._poll = None;
        self.open = true;
        self.pending_focus = true;
        cx.notify();
    }

    /// Whether the dialog is visible.
    pub fn is_open(&self) -> bool {
        self.open
    }

    /// Whether a restore is running under the card; see
    /// [`crate::backup_dialog::BackupDialog::is_running`].
    pub fn is_running(&self) -> bool {
        self.open && matches!(self.stage, Stage::Running(_))
    }

    /// What `Enter` means when no control keeps it: start from a form that
    /// can, close a card that ended, and nothing while a restore runs.
    pub fn confirm(&mut self, cx: &mut Context<Self>) {
        match &self.stage {
            Stage::Form if self.ready() => self.start(cx),
            Stage::Ended(_) => self.dismiss(cx),
            _ => {}
        }
    }

    /// Hides the dialog without emitting an event.
    ///
    /// **A restore still running is canceled.** What it already ran stays.
    pub fn close(&mut self, cx: &mut Context<Self>) {
        if let Stage::Running(running) = &self.stage {
            running.restore.cancel();
        }
        self.open = false;
        self.pending_focus = false;
        self.stage = Stage::Form;
        self.asking_stop = false;
        self._poll = None;
        self.session = None;
        self.scope = None;
        cx.notify();
    }

    /// Closes the dialog and reports it, so the shell can restore focus.
    pub fn dismiss(&mut self, cx: &mut Context<Self>) {
        cx.emit(RestoreDialogEvent::Dismissed);
        self.close(cx);
    }

    /// What `Escape` means here: over a running restore, the question
    /// whether to stop it; anywhere else, closing.
    pub fn escape(&mut self, cx: &mut Context<Self>) {
        if let Stage::Running(running) = &self.stage {
            if self.asking_stop {
                self.asking_stop = false;
            } else if !running.canceling {
                self.asking_stop = true;
            }
            cx.notify();
            return;
        }
        self.dismiss(cx);
    }

    /// Whether the form can start: a file chosen, on a connection that
    /// writes.
    fn ready(&self) -> bool {
        self.input.is_some() && !self.read_only && self.session.is_some()
    }

    /// Asks the platform for the script, and records the answer.
    fn pick_input(&mut self, cx: &mut Context<Self>) {
        let prompt = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some(ts!("restore.browse")),
        });
        cx.spawn(async move |dialog, cx| {
            let path = match prompt.await {
                Ok(Ok(Some(paths))) => paths.into_iter().next(),
                Ok(Ok(None)) | Err(_) => None,
                Ok(Err(error)) => {
                    log::warn!("the file picker could not be opened: {error:#}");
                    None
                }
            };
            let Some(path) = path else {
                return;
            };
            dialog
                .update(cx, |dialog, cx| {
                    dialog.input = Some(path);
                    cx.notify();
                })
                .ok();
        })
        .detach();
    }

    /// Starts the restore the form describes, and begins polling it.
    fn start(&mut self, cx: &mut Context<Self>) {
        if !matches!(self.stage, Stage::Form) || !self.ready() {
            return;
        }
        let (Some(path), Some(session)) = (self.input.clone(), self.session.clone()) else {
            return;
        };
        let request = RestoreRequest {
            path,
            dialect: self.dialect,
            continue_on_error: self.continue_on_error,
            enter: self
                .scope
                .as_ref()
                .and_then(|scope| crate::restore::enter_scope(&self.dialect, scope)),
        };
        let restore = Restore::start(session, request);
        self.stage = Stage::Running(Running {
            restore: Arc::clone(&restore),
            progress: Progress::default(),
            canceling: false,
        });
        self._poll = Some(cx.spawn(async move |dialog, cx| {
            loop {
                cx.background_executor().timer(POLL_INTERVAL).await;
                let carry_on = dialog
                    .update(cx, |dialog, cx| dialog.deliver(cx))
                    .unwrap_or(false);
                if !carry_on {
                    return;
                }
            }
        }));
        cx.notify();
    }

    /// Takes one reading. Answers whether the loop should take another.
    fn deliver(&mut self, cx: &mut Context<Self>) -> bool {
        let Stage::Running(running) = &mut self.stage else {
            return false;
        };
        running.progress = running.restore.progress();
        let Some(outcome) = running.restore.outcome() else {
            cx.notify();
            return true;
        };
        self.asking_stop = false;
        self.stage = Stage::Ended(outcome);
        cx.notify();
        false
    }

    /// Asks the restore to stop. The card stays up until it has.
    fn request_cancel(&mut self, cx: &mut Context<Self>) {
        self.asking_stop = false;
        let Stage::Running(running) = &mut self.stage else {
            return;
        };
        if running.canceling {
            return;
        }
        running.canceling = true;
        running.restore.cancel();
        cx.notify();
    }

    fn apply_pending_focus(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.pending_focus {
            return;
        }
        self.pending_focus = false;
        let handle = self.focus_handle.clone();
        window.focus(&handle, cx);
        cx.notify();
    }

    /// The scope as the target row names it.
    fn target_name(&self) -> SharedString {
        self.scope
            .as_ref()
            .map(|scope| {
                [scope.catalog.as_deref(), scope.schema.as_deref()]
                    .into_iter()
                    .flatten()
                    .filter(|part| !part.trim().is_empty())
                    .collect::<Vec<_>>()
                    .join(".")
            })
            .filter(|name| !name.is_empty())
            .map(SharedString::from)
            .unwrap_or_else(|| ts!("backup.scope_all"))
    }

    fn input_name(&self) -> String {
        self.input
            .as_ref()
            .map(|path| path.display().to_string())
            .unwrap_or_default()
    }

    /// The target row, which every half of the dialog shows.
    fn render_target(&self, cx: &App) -> impl IntoElement + use<> {
        let chrome = theme(cx);
        form_row(
            ts!("restore.target"),
            div().text_color(chrome.text).child(self.target_name()),
        )
        .label_width(self.label_width)
    }

    /// The file cell and the button that picks it.
    fn render_input(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let chrome = theme(cx);
        let this = cx.entity();
        let (text, color) = match &self.input {
            Some(path) => (path.display().to_string(), chrome.text),
            None => (ts!("restore.input_none").to_string(), chrome.text_muted),
        };
        form_row(
            ts!("restore.input"),
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
                        .text_size(rugpui::TextSize::Sm.size(cx))
                        .text_color(color)
                        .child(text),
                )
                .child(
                    Button::new("restore-input", ts!("backup.browse"))
                        .variant(ButtonVariant::Secondary)
                        .tab_index(tab::INPUT)
                        .on_click(move |_, _window, cx| {
                            this.update(cx, RestoreDialog::pick_input);
                        }),
                ),
        )
        .label_width(self.label_width)
    }

    /// The form half.
    fn render_form(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let chrome = theme(cx);
        let this = cx.entity();
        let note = if self.read_only {
            (ts!("restore.read_only"), chrome.danger)
        } else {
            (ts!("restore.warning"), chrome.text_muted)
        };
        div()
            .flex()
            .flex_col()
            .gap(px(10.))
            .child(self.render_target(cx))
            .child(self.render_input(cx))
            .child(
                Checkbox::new("restore-continue", ts!("restore.continue_on_error"))
                    .checked(self.continue_on_error)
                    .tab_index(tab::CONTINUE)
                    .on_toggle(move |checked, _window, cx| {
                        this.update(cx, |dialog, cx| {
                            dialog.continue_on_error = checked;
                            cx.notify();
                        });
                    }),
            )
            .child(
                div()
                    .text_size(rugpui::TextSize::Xs.size(cx))
                    .text_color(note.1)
                    .child(note.0),
            )
    }

    /// The progress half.
    fn render_running(
        &self,
        running: &Running,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let chrome = theme(cx);
        let progress = &running.progress;
        let phase = if running.canceling {
            ts!("restore.canceling")
        } else {
            match progress.phase {
                Phase::Checking => ts!("restore.checking"),
                Phase::Connecting => ts!("restore.connecting"),
                Phase::Running => ts!("restore.running"),
            }
        };
        let counters = match progress.phase {
            Phase::Checking => ts!(
                "restore.counted",
                statements = progress.statements_done,
                bytes = format_bytes(progress.bytes_done)
            ),
            Phase::Connecting | Phase::Running => ts!(
                "restore.counters",
                done = progress.statements_done,
                total = progress.statements_total,
                bytes = format_bytes(progress.bytes_done),
                size = format_bytes(progress.bytes_total)
            ),
        };
        let failed = (progress.failed > 0).then(|| {
            div()
                .text_size(rugpui::TextSize::Sm.size(cx))
                .text_color(chrome.danger)
                .child(ts!("restore.failed_so_far", count = progress.failed))
        });
        div()
            .flex()
            .flex_col()
            .gap(px(10.))
            .child(self.render_target(cx))
            .child(
                div()
                    .text_size(rugpui::TextSize::Sm.size(cx))
                    .text_color(chrome.text_muted)
                    .child(phase),
            )
            .child(div().text_color(chrome.text).child(counters))
            .children(failed)
    }

    /// The card a finished restore leaves behind.
    fn render_ended(&self, outcome: &Outcome, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let chrome = theme(cx);
        let path = self.input_name();
        let (message, color) = match outcome {
            Outcome::Done {
                statements, failed, ..
            } if *failed > 0 => (
                ts!(
                    "restore.done_with_failures",
                    count = statements,
                    failed = failed,
                    path = path.as_str()
                ),
                chrome.danger,
            ),
            Outcome::Done { statements, .. } => (
                ts!("restore.done", count = statements, path = path.as_str()),
                chrome.success,
            ),
            Outcome::Failed {
                statements,
                failure,
            } => (failed_message(*statements, failure), chrome.danger),
            Outcome::Refused(refusal) => (refusal_message(refusal), chrome.danger),
            Outcome::NoSession(error) => (
                ts!("restore.no_session", error = error.as_str()),
                chrome.danger,
            ),
            Outcome::Canceled { statements } => {
                (ts!("restore.canceled", count = statements), chrome.text)
            }
        };

        let statement = match outcome {
            Outcome::Failed { failure, .. } => Some(failure.sql.clone()),
            _ => None,
        };
        let failures = match outcome {
            Outcome::Done {
                failures, failed, ..
            } if !failures.is_empty() => Some((failures.clone(), *failed)),
            _ => None,
        };

        div()
            .flex()
            .flex_col()
            .gap(px(10.))
            .child(self.render_target(cx))
            .child(
                div()
                    .text_size(rugpui::TextSize::Sm.size(cx))
                    .text_color(color)
                    .child(message),
            )
            .children(statement.map(|sql| {
                div()
                    .text_size(rugpui::TextSize::Xs.size(cx))
                    .text_color(chrome.text_muted)
                    .overflow_hidden()
                    .child(sql)
            }))
            .children(failures.map(|(failures, failed)| {
                let more = (failed as usize).saturating_sub(SHOWN_FAILURES);
                div()
                    .flex()
                    .flex_col()
                    .gap(px(2.))
                    .text_size(rugpui::TextSize::Xs.size(cx))
                    .children(failures.into_iter().take(SHOWN_FAILURES).map(|failure| {
                        div()
                            .text_color(chrome.text_muted)
                            .overflow_hidden()
                            .child(ts!(
                                "restore.failure_line",
                                line = failure.line,
                                error = failure.message.as_str()
                            ))
                    }))
                    .children((more > 0).then(|| {
                        div()
                            .text_color(chrome.text_muted)
                            .child(ts!("backup.skipped_more", count = more))
                    }))
            }))
    }

    /// The button row.
    fn render_footer(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let this = cx.entity();
        let buttons = match &self.stage {
            Stage::Form => div()
                .flex()
                .flex_row()
                .gap(px(8.))
                .child(
                    Button::new("restore-cancel", ts!("common.cancel"))
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
                    Button::new("restore-start", ts!("restore.start"))
                        .variant(ButtonVariant::Primary)
                        .disabled(!self.ready())
                        .tab_index(tab::START)
                        .on_click({
                            let this = this.clone();
                            move |_, _window, cx| {
                                this.update(cx, |dialog, cx| dialog.start(cx));
                            }
                        }),
                ),
            Stage::Running(_) if self.asking_stop => {
                let keep = this.clone();
                let stop = this.clone();
                return crate::extract_dialog::render_stop_question(
                    "restore",
                    ts!("restore.stop_question"),
                    move |cx| {
                        keep.update(cx, |dialog, cx| {
                            dialog.asking_stop = false;
                            cx.notify();
                        });
                    },
                    move |cx| {
                        stop.update(cx, |dialog, cx| dialog.request_cancel(cx));
                    },
                    cx,
                );
            }
            Stage::Running(running) => div().flex().flex_row().gap(px(8.)).child(
                Button::new("restore-stop", ts!("common.cancel"))
                    .variant(ButtonVariant::Secondary)
                    .disabled(running.canceling)
                    .tab_index(tab::DISMISS)
                    .on_click({
                        let this = this.clone();
                        move |_, _window, cx| {
                            this.update(cx, |dialog, cx| dialog.request_cancel(cx));
                        }
                    }),
            ),
            Stage::Ended(_) => div().flex().flex_row().gap(px(8.)).child(
                Button::new("restore-close", ts!("common.close"))
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
            .child(div().flex_1())
            .child(buttons)
    }
}

/// What the card says about the statement a run stopped at.
fn failed_message(statements: u64, failure: &Failure) -> SharedString {
    if failure.line == 0 {
        ts!("restore.enter_failed", error = failure.message.as_str())
    } else {
        ts!(
            "restore.failed",
            line = failure.line,
            count = statements,
            error = failure.message.as_str()
        )
    }
}

/// What the card says about a script that was not run.
fn refusal_message(refusal: &Refusal) -> SharedString {
    let reason = match refusal {
        Refusal::Script(error) => crate::query::script_refusal(error),
        Refusal::NotText { line } => ts!("restore.not_text", line = line),
        Refusal::PgArchive => ts!("restore.pg_archive"),
        Refusal::Io(error) => ts!("restore.unreadable", error = error.as_str()),
    };
    ts!("restore.refused", reason = reason.as_ref())
}

impl EventEmitter<RestoreDialogEvent> for RestoreDialog {}

impl Focusable for RestoreDialog {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for RestoreDialog {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.label_width = rugpui::fit_label_width(
            vec![ts!("restore.target"), ts!("restore.input")],
            window,
            cx,
        );
        if !self.open {
            return div().id("restore-dialog");
        }
        self.apply_pending_focus(window, cx);

        let body = match &self.stage {
            Stage::Form => self.render_form(cx).into_any_element(),
            Stage::Running(running) => self.render_running(running, cx).into_any_element(),
            Stage::Ended(outcome) => self.render_ended(outcome, cx).into_any_element(),
        };
        let footer = self.render_footer(cx);
        let on_dismiss = {
            let this = cx.entity();
            move |_window: &mut Window, cx: &mut App| {
                this.update(cx, |dialog, cx| dialog.escape(cx));
            }
        };
        let confirm = {
            let this = cx.entity();
            move |_window: &mut Window, cx: &mut App| {
                this.update(cx, |dialog, cx| dialog.confirm(cx));
            }
        };
        div()
            .id("restore-dialog")
            .absolute()
            .inset_0()
            .size_full()
            .track_focus(&self.focus_handle)
            .child(
                modal(
                    "restore-modal",
                    ts!("restore.title"),
                    rugpui::scaled(DIALOG_WIDTH, cx),
                    div()
                        .flex()
                        .flex_col()
                        .gap(px(14.))
                        .child(body)
                        .child(footer),
                    on_dismiss,
                )
                .backdrop_dismisses(!self.is_running())
                .on_confirm(confirm),
            )
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use gpui::{TestAppContext, WindowHandle};
    use rudbman_jdbc::StatementSpec;

    use super::*;
    use crate::connection::Connected;

    fn dialog(
        connected: &Connected,
        schema: &str,
        input: Option<&Path>,
        cx: &mut TestAppContext,
    ) -> WindowHandle<RestoreDialog> {
        cx.update(|cx| {
            rugpui::init(cx);
        });
        let session = connected.handle();
        let scope = Scope {
            catalog: None,
            schema: Some(schema.to_string()),
        };
        let input = input.map(Path::to_path_buf);
        let window = cx.add_window(|_window, cx| RestoreDialog::new(cx));
        window
            .update(cx, |dialog, _window, cx| {
                dialog.open(scope, session, Dialect::H2, false, cx);
                dialog.input = input;
            })
            .expect("the window is open");
        window
    }

    fn drive(window: &WindowHandle<RestoreDialog>, cx: &mut TestAppContext) {
        for _ in 0..5_000 {
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
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        panic!("the restore never ended");
    }

    #[gpui::test]
    fn a_backup_is_restored_into_an_empty_schema(cx: &mut TestAppContext) {
        // The schema backed up by the backup dialog's own test, restored into
        // a database of its own.
        let source = crate::restore::tests::h2(
            "restore-dialog-source",
            &[
                "CREATE SCHEMA APP",
                "CREATE TABLE APP.PARENT (ID INT PRIMARY KEY, NAME VARCHAR(20))",
                "CREATE TABLE APP.CHILD (ID INT PRIMARY KEY, PARENT_ID INT NOT NULL, \
                     CONSTRAINT FK_CHILD_PARENT FOREIGN KEY (PARENT_ID) \
                     REFERENCES APP.PARENT(ID))",
                "INSERT INTO APP.PARENT VALUES (1, 'a; b'), (2, 'b')",
                "INSERT INTO APP.CHILD VALUES (10, 1), (11, 2), (12, 1)",
                "CREATE SEQUENCE APP.ORDER_NO START WITH 100",
                "CREATE VIEW APP.NAMES AS SELECT NAME FROM APP.PARENT",
            ],
        );
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("app.sql");
        let mut job = source
            .session()
            .start_backup(
                &rudbman_jdbc::BackupSpec::new(&path)
                    .with_scope(rudbman_jdbc::ScopeRef::new().with_schema("APP"))
                    .with_ddl(rudbman_jdbc::DdlOptions::included())
                    .with_objects(rudbman_jdbc::BackupObjects::all())
                    .with_data(rudbman_jdbc::BackupDataOptions::included()),
            )
            .expect("the backup starts");
        while !job.poll().expect("the backup reports").is_terminal() {
            std::thread::sleep(std::time::Duration::from_millis(5));
        }

        let target = crate::restore::tests::h2("restore-dialog-target", &["CREATE SCHEMA APP"]);
        let window = dialog(&target, "APP", Some(&path), cx);
        window
            .update(cx, |dialog, _window, cx| dialog.start(cx))
            .expect("the window is open");
        drive(&window, cx);
        window
            .update(cx, |dialog, _window, _cx| {
                let Stage::Ended(Outcome::Done {
                    statements, failed, ..
                }) = &dialog.stage
                else {
                    panic!(
                        "the restore did not finish: {:?}",
                        match &dialog.stage {
                            Stage::Ended(outcome) => format!("{outcome:?}"),
                            _ => "still running".to_string(),
                        }
                    );
                };
                assert!(*statements > 5, "{statements} statements");
                assert_eq!(*failed, 0);
            })
            .expect("the window is open");

        let rows = |sql: &str| {
            let cursor = target
                .session()
                .execute(&StatementSpec::new(sql))
                .unwrap_or_else(|error| panic!("{sql}: {error}"));
            match cursor.fetch(1).unwrap().value(0, 0) {
                Some(rudbman_jdbc::Value::I64(count)) => count,
                other => panic!("{sql}: {other:?}"),
            }
        };
        assert_eq!(rows("SELECT COUNT(*) FROM APP.PARENT"), 2);
        assert_eq!(rows("SELECT COUNT(*) FROM APP.CHILD"), 3);
        assert_eq!(
            rows("SELECT COUNT(*) FROM APP.NAMES WHERE NAME = 'a; b'"),
            1
        );
        assert_eq!(rows("SELECT NEXT VALUE FOR APP.ORDER_NO"), 100);
    }

    #[gpui::test]
    fn every_shape_of_the_dialog_lays_out(cx: &mut TestAppContext) {
        let connected = crate::restore::tests::h2("restore-dialog-shapes", &[]);
        let window = dialog(&connected, "PUBLIC", None, cx);
        cx.run_until_parked();
        let failure = Failure {
            line: 12,
            sql: "INSERT INTO T VALUES (1)".into(),
            message: "Table \"T\" not found".into(),
        };
        let outcomes = [
            Outcome::Done {
                statements: 3,
                failures: Vec::new(),
                failed: 0,
            },
            Outcome::Done {
                statements: 3,
                failures: vec![failure.clone(); 7],
                failed: 7,
            },
            Outcome::Failed {
                statements: 2,
                failure: failure.clone(),
            },
            Outcome::Failed {
                statements: 0,
                failure: Failure {
                    line: 0,
                    ..failure.clone()
                },
            },
            Outcome::Refused(Refusal::PgArchive),
            Outcome::Refused(Refusal::NotText { line: 3 }),
            Outcome::Refused(Refusal::Io("gone".into())),
            Outcome::Refused(Refusal::Script(rudbman_sql::ScriptError {
                line: 4,
                range: 0..8,
                kind: rudbman_sql::ScriptErrorKind::Unsupported("\\connect".into()),
            })),
            Outcome::NoSession("refused".into()),
            Outcome::Canceled { statements: 9 },
        ];
        for outcome in outcomes {
            window
                .update(cx, |dialog, _window, cx| {
                    dialog.stage = Stage::Ended(outcome);
                    cx.notify();
                })
                .unwrap();
            cx.run_until_parked();
        }
        window
            .update(cx, |dialog, _window, cx| {
                dialog.read_only = true;
                dialog.stage = Stage::Form;
                cx.notify();
            })
            .unwrap();
        cx.run_until_parked();
        window
            .update(cx, |dialog, _window, _cx| {
                assert!(!dialog.ready(), "a read-only connection does not restore");
            })
            .unwrap();
    }
}
