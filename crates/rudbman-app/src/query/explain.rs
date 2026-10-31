//! Reading a statement's plan instead of running it.
//!
//! `Ctrl`/`Cmd`+`Shift`+`E`, the editor's menu row and the toolbar button all
//! come here. The statement is the selection, or the one the caret is in, as
//! for running it; what is sent is whatever [`rudbman_sql::explain`] writes
//! for the dialect, and the plan comes back as read-only grids titled "Plan"
//! in the result area the pane's runs already use — with its log, its elapsed
//! clock and its cancel button.
//!
//! # Not a run of the statement
//!
//! So neither of the profile's write guards applies: a plan of a `DELETE` is
//! not a `DELETE`, and refusing or confirming one would be refusing to say what
//! a statement *would* do on exactly the connection where that matters most.
//! Nothing is marked as uncommitted either. The one product where reading a
//! plan writes anything is Oracle, whose `EXPLAIN PLAN` puts rows into the
//! session's own temporary `PLAN_TABLE`: under manual commit those rows join
//! the open transaction and go with its commit or rollback, and on a
//! connection opened read-only the database may refuse the statement, which
//! is then reported as it is.
//!
//! # Putting the session back
//!
//! SQL Server reads a plan by switching `SHOWPLAN_TEXT` on, and while it is on
//! the server executes nothing on that connection. The statements that switch
//! it off ([`Explain::after`]) are therefore sent whatever happened before
//! them — a failed plan, a canceled one — and a failure to send them is said
//! in the danger color, because every statement after it would come back as a
//! plan rather than run.

use rudbman_sql::{Explain, explain};

use super::execution::Position;

use super::*;

/// Rows read from a plan's result sets: one batch, never paged.
///
/// A plan is short — tens of rows for a hard query — and the session that
/// read it may have to be put back right after, which a cursor left open for
/// paging would be in the way of. Rows past this are not shown.
const PLAN_ROWS: u32 = 10_000;

impl QueryPane {
    /// Reads the plan of the selection, or of the statement under the caret.
    pub(crate) fn explain(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.close_completion(cx);
        if self.lob_dialog.is_some() || self.transaction.is_some() {
            return;
        }
        self.notice = None;
        let Some((sql, span)) = self.explain_target(cx) else {
            self.notice = Some(Notice::warn(ts!("query.no_statement")));
            cx.notify();
            return;
        };
        let statements = split_statements(&sql, &self.dialect);
        let statement = match statements.as_slice() {
            [] => {
                self.notice = Some(Notice::warn(ts!("query.no_statement")));
                cx.notify();
                return;
            }
            [only] => only.sql(&sql).to_string(),
            _ => {
                // A plan is of one statement. Which of several was meant is
                // not something to guess at.
                self.notice = Some(Notice::warn(ts!("query.explain_one")));
                cx.notify();
                return;
            }
        };
        if self.has_pending_edits(cx) {
            // The plan replaces every result, and a staged edit is keyed to a
            // row of the result it was typed on (§7.9).
            self.notice = Some(Notice::warn(ts!("data.discard_first")));
            cx.notify();
            return;
        }
        if self.session.is_none() {
            self.notice = Some(Notice::warn(ts!("explorer.disconnected")));
            cx.notify();
            return;
        }
        if self.is_running() {
            self.notice = Some(Notice::warn(ts!("query.busy")));
            cx.notify();
            return;
        }
        let Some(plan) = explain(self.dialect, &statement) else {
            self.notice = Some(Notice::warn(ts!(
                "query.explain_unsupported",
                product = self.dialect.name()
            )));
            cx.notify();
            return;
        };

        self.run_spans = vec![span];
        let Some((session, generation)) = self.begin_run(None, cx) else {
            return;
        };
        self.explaining = true;
        self.recording = false;

        cx.spawn_in(window, async move |pane, cx| {
            let shown = plan.shown.clone();
            let (outcome, elapsed, restore) = cx
                .background_spawn(async move {
                    let started = Instant::now();
                    let (outcome, restore) = read_plan(&session, &plan);
                    (outcome, started.elapsed(), restore)
                })
                .await;
            pane.update_in(cx, |pane, window, cx| {
                let at = Position {
                    index: 0,
                    total: 1,
                    sql: shown,
                };
                if pane.deliver(generation, at, (outcome, elapsed), window, cx) {
                    pane.finish(generation, cx);
                }
                if let Some(error) = restore
                    && generation == pane.generation
                {
                    pane.notice = Some(Notice::error(ts!(
                        "query.explain_restore_failed",
                        message = QueryError::new(error).message.to_string()
                    )));
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    /// The text a plan is asked for, and where in the editor it was cut from:
    /// the selection when there is one, the statement under the caret
    /// otherwise.
    fn explain_target(&self, cx: &App) -> Option<(String, Option<std::ops::Range<usize>>)> {
        let editor = self.editor.read(cx);
        let text = editor.text();
        let selection = editor.selection();
        if !selection.is_empty() {
            let selected = text.get(selection.clone())?.to_string();
            return Some((selected, Some(selection)));
        }
        let span = editor.statement_at_caret()?;
        Some((span.sql(&text).to_string(), Some(span.sql_range())))
    }
}

/// Sends `plan`'s statements and reads the plan's rows. **Blocks**; only ever
/// called from a background thread.
///
/// Answers what the plan statement produced, and — apart from it — whatever
/// went wrong putting the session back, since a plan that was read is worth
/// showing even when the statement after it failed.
fn read_plan(
    session: &SessionHandle,
    plan: &Explain,
) -> (Result<Executed, JdbcError>, Option<JdbcError>) {
    let started = Instant::now();
    let session_ref = session.session();
    let outcome = plan
        .before
        .iter()
        .try_for_each(|sql| {
            // Dropped at once, which closes it: these answer nothing worth
            // reading.
            session_ref
                .execute(&StatementSpec::new(sql.clone()))
                .map(drop)
        })
        .and_then(|()| {
            let spec = StatementSpec::new(plan.plan.clone()).with_fetch_size(PLAN_ROWS);
            let mut cursor = session_ref.execute(&spec)?;
            let mut steps = Vec::new();
            advance(&mut cursor, true, PLAN_ROWS, &mut steps)?;
            // Closed before anything is put back, and the rows that were read
            // are all there will be.
            drop(cursor);
            for step in &mut steps {
                if let Step::Rows { complete, .. } = step {
                    *complete = true;
                }
            }
            Ok(steps)
        });
    // Whatever happened above: a session left with SQL Server's plan switch
    // on runs nothing from here on.
    let mut restore = None;
    for sql in &plan.after {
        if let Err(error) = session_ref.execute(&StatementSpec::new(sql.clone())) {
            restore.get_or_insert(error);
        }
    }
    let outcome = outcome.map(|steps| Executed {
        sql: plan.shown.clone(),
        steps,
        elapsed: started.elapsed(),
        more: None,
    });
    (outcome, restore)
}
