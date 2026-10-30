//! Execution.

use super::*;

impl QueryPane {
    pub(super) fn can_finish_transaction(&self) -> bool {
        self.session.is_some()
            && self.transactional
            && !self.restore_auto_commit
            && !self.is_running()
            && !self.applying
            && self.transaction.is_none()
    }

    /// Finish the transaction through JDBC without replacing the editor or results.
    pub(super) fn finish_transaction(&mut self, action: TransactionAction, cx: &mut Context<Self>) {
        if !self.can_finish_transaction() {
            return;
        }
        let Some(session) = self.session.clone() else {
            return;
        };
        self.transaction = Some(action);
        self.notice = Some(Notice::warn(match action {
            TransactionAction::Commit => ts!("query.committing"),
            TransactionAction::Rollback => ts!("query.rolling_back"),
        }));
        cx.spawn(async move |pane, cx| {
            let outcome = cx
                .background_spawn({
                    let session = session.clone();
                    async move {
                        match action {
                            TransactionAction::Commit => session.session().commit(),
                            TransactionAction::Rollback => session.session().rollback(),
                        }
                    }
                })
                .await;
            pane.update(cx, |pane, cx| {
                pane.transaction = None;
                if outcome.is_ok() {
                    session.clear_uncommitted();
                }
                if pane.session.is_some() {
                    pane.notice = Some(match (action, outcome) {
                        (TransactionAction::Commit, Ok(())) => Notice::info(ts!("query.committed")),
                        (TransactionAction::Rollback, Ok(())) => {
                            Notice::info(ts!("query.rolled_back"))
                        }
                        // In the danger color, never the gray a commit that
                        // went through is said in: the transaction is still
                        // open, and the user has to know it is.
                        (TransactionAction::Commit, Err(error)) => {
                            Notice::error(ts!("query.commit_failed", message = error.to_string()))
                        }
                        (TransactionAction::Rollback, Err(error)) => {
                            Notice::error(ts!("query.rollback_failed", message = error.to_string()))
                        }
                    });
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    /// The write confirmation was accepted.
    pub fn confirmed(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(statements) = self.pending.take() else {
            return;
        };
        self.start(statements, None, window, cx);
    }

    /// The write confirmation was declined.
    pub fn declined(&mut self, cx: &mut Context<Self>) {
        self.pending = None;
        self.run_spans.clear();
        cx.notify();
    }

    /// Starts a run: clears the last one, takes the next generation, and hands
    /// the statements to a background pipeline.
    /// `sort_base` is the statement a later sort should wrap, for the runs
    /// whose own SQL is already a wrapper; `None` means "whatever was run".
    pub(super) fn start(
        &mut self,
        statements: Vec<String>,
        sort_base: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.transaction.is_some() {
            return;
        }
        // The one gate every run goes through, whichever way it was asked for:
        // a sort round trip and an accepted write confirmation arrive here
        // without passing `request` again.
        let Some(session) = self.session.clone() else {
            self.notice = Some(Notice::warn(ts!("explorer.disconnected")));
            cx.notify();
            return;
        };

        self.generation += 1;
        let generation = self.generation;
        // Dropping the old tabs drops their cursors, and `Cursor::drop` closes
        // them — which is the `CLOSE_CURSOR` a canceled run still owes.
        self.results.clear();
        self.active_result = 0;
        self.error = None;
        self.notice = None;
        self.finished = None;
        self.affected = 0;
        self.ran = true;
        // A menu naming a result tab that is about to be dropped would act on
        // whichever tab took its place, or on none at all.
        self.context_menu = None;
        // The rows a plan was made against are on their way out, so the plan
        // and the failure of the last attempt to send one go with them.
        self.preview = None;
        self.apply_error = None;
        self.confirm_discard = false;
        if sort_base.is_none() {
            // Not a sort round trip, so whatever order the last one left is
            // not the order this one comes back in.
            self.pending_sort = None;
        }
        self.sort_base = sort_base;
        // Whatever the editor's events left for this run; a sort round trip
        // and an apply's re-run leave nothing, and the spans then go unused.
        self.active_spans = (std::mem::take(&mut self.run_spans), self.edits);
        self.run = RunState::Running(Box::new(Running {
            generation,
            started: Instant::now(),
            canceller: session.session().canceller(),
            canceling: false,
        }));

        let fetch_rows = self.fetch_rows;
        let writer = self.tab_label().map(|label| label.to_string());
        // What each statement does to the books [`SessionHandle::has_uncommitted`]
        // keeps, worked out here because the dialect stays with the pane. Only a
        // session that is really in a transaction of the user's — manual commit,
        // on a product that has transactions — has anything to keep.
        //
        // [`SessionHandle::has_uncommitted`]: crate::connection::SessionHandle::has_uncommitted
        let manual = self.transactional && !self.restore_auto_commit;
        let statements: Vec<(String, Effect, Option<Pager>, bool)> = statements
            .into_iter()
            .map(|sql| {
                let effect = if !manual {
                    Effect::None
                } else if ends_transaction(&sql, &self.dialect) {
                    Effect::Ends
                } else if is_write_statement(&sql, &self.dialect) {
                    Effect::Opens
                } else {
                    Effect::None
                };
                // Decided here, beside the dialect, for the same reason: and
                // whether a statement is read a page at a time or through its
                // own cursor is settled for the life of its result (see
                // [`More`]).
                let pager = Pager::new(&sql, &self.dialect, fetch_rows);
                let server_cursor = statements::server_cursor_read(&sql, &self.dialect);
                (sql, effect, pager, server_cursor)
            })
            .collect();

        // `spawn_in` rather than `spawn`: the grids this run ends in are
        // subscribed to with a window, because answering a double click with
        // [`GridView::begin_edit`] means putting the keyboard in a field and
        // there is no window to be had inside a plain `cx.subscribe` (§7.9).
        let total = statements.len();
        cx.spawn_in(window, async move |pane, cx| {
            for (position, (sql, effect, pager, server_cursor)) in
                statements.into_iter().enumerate()
            {
                let failed_sql = sql.clone();
                let writer = writer.clone();
                let outcome = cx
                    .background_spawn({
                        let session = session.clone();
                        async move {
                            let started = Instant::now();
                            let outcome = execute(
                                &session,
                                sql,
                                Statement {
                                    effect,
                                    pager,
                                    server_cursor,
                                    writer: writer.as_deref(),
                                },
                                fetch_rows,
                            );
                            (outcome, started.elapsed())
                        }
                    })
                    .await;

                let at = Position {
                    index: position,
                    total,
                    sql: failed_sql,
                };
                let carry_on = pane
                    .update_in(cx, |pane, window, cx| {
                        pane.deliver(generation, at, outcome, window, cx)
                    })
                    .unwrap_or(false);
                if !carry_on {
                    return;
                }
            }
            pane.update(cx, |pane, cx| pane.finish(generation, cx)).ok();
        })
        .detach();

        // The elapsed clock. A task of its own rather than something the render
        // works out, because nothing else would make the window redraw while
        // the driver is blocked.
        cx.spawn(async move |pane, cx| {
            loop {
                cx.background_executor().timer(CLOCK_TICK).await;
                let ticking = pane
                    .update(cx, |pane, cx| {
                        let ticking = matches!(
                            &pane.run,
                            RunState::Running(running) if running.generation == generation
                        );
                        if ticking {
                            cx.notify();
                        }
                        ticking
                    })
                    .unwrap_or(false);
                if !ticking {
                    return;
                }
            }
        })
        .detach();

        cx.notify();
    }

    /// Records what one statement produced. Answers whether the run goes on.
    ///
    /// A failure of a run of several statements becomes a row of the
    /// execution log — which statement of how many, its SQL, and the error —
    /// and the log comes to the front, while the results of the statements
    /// before it stay where they are and can still be looked at: a script that
    /// stops at its thirtieth statement has run the first twenty-nine, and
    /// the user needs to see both halves of that. A run of one statement keeps
    /// the whole error view it always had. Either way the failed statement is
    /// selected in the editor, when the editor still holds the text it was cut
    /// from.
    pub(super) fn deliver(
        &mut self,
        generation: u64,
        at: Position,
        (outcome, elapsed): (Result<Executed, JdbcError>, Duration),
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if generation != self.generation {
            // A superseded run's answer. Dropping `outcome` closes its cursor.
            return false;
        }
        match outcome {
            Ok(executed) => {
                let Executed {
                    sql,
                    steps,
                    elapsed,
                    more,
                } = executed;
                self.append(&sql, steps, elapsed, more, window, cx);
                self.recount(cx);
                cx.notify();
                true
            }
            Err(error) => {
                let error = QueryError::new(error);
                if at.total > 1 {
                    let mut message = ts!(
                        "query.statement_failed",
                        index = at.index + 1,
                        total = at.total,
                        error = error.message.to_string()
                    )
                    .to_string();
                    if let Some(state) = &error.sql_state {
                        message.push_str(&format!(" ({state})"));
                    }
                    if let Some(hint) = error.hint() {
                        message.push_str(" — ");
                        message.push_str(&hint);
                    }
                    self.append_log_entry(&at.sql, message.into(), elapsed, true);
                    if let Some(log) = self
                        .results
                        .iter()
                        .position(|tab| matches!(tab.body, ResultBody::Log(_)))
                    {
                        self.active_result = log;
                    }
                }
                self.select_failed(at.index, cx);
                self.error = Some(error);
                self.finish(generation, cx);
                false
            }
        }
    }

    /// Selects statement `index` of the run in the editor, when the editor
    /// still holds the text it was cut from.
    fn select_failed(&mut self, index: usize, cx: &mut Context<Self>) {
        let (spans, edits) = &self.active_spans;
        if *edits != self.edits {
            return;
        }
        let Some(Some(range)) = spans.get(index).cloned() else {
            return;
        };
        self.editor
            .update(cx, |editor, cx| editor.select_range(range, cx));
    }

    /// Turns one statement's results into tabs.
    ///
    /// `more` is how to read on when the last result set still has rows to
    /// come: an open cursor when the walk stopped on one, which only the last
    /// grid tab can have since `MORE_RESULTS` has closed the others, or the
    /// next page of a statement read a page at a time, which has only the one
    /// result.
    pub(super) fn append(
        &mut self,
        sql: &str,
        steps: Vec<Step>,
        elapsed: Duration,
        more: Option<More>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let mut more = more;
        // A run the sort round trip started executes a wrapper; the statement
        // the *next* sort has to wrap is the one underneath it.
        let base = self.sort_base.clone().unwrap_or_else(|| sql.to_string());
        let last_rows = steps
            .iter()
            .rposition(|step| matches!(step, Step::Rows { .. }));

        for (index, step) in steps.into_iter().enumerate() {
            match step {
                Step::Rows {
                    columns,
                    batch,
                    complete,
                } => {
                    self.append_log(sql, ts!("query.executed"), elapsed);
                    let carried = (Some(index) == last_rows).then(|| more.take()).flatten();
                    let state = if complete && carried.is_none() {
                        GridSourceState::Complete
                    } else {
                        GridSourceState::HasMore
                    };
                    self.push_rows(
                        base.clone(),
                        sql.to_string(),
                        elapsed,
                        columns,
                        batch,
                        state,
                        carried,
                        window,
                        cx,
                    );
                }
                Step::Message { update_count } => {
                    self.affected += u64::try_from(update_count).unwrap_or(0);
                    let text = if update_count > 0 {
                        ts!("query.rows_affected", count = update_count)
                    } else {
                        ts!("query.executed")
                    };
                    self.append_log(sql, text, elapsed);
                }
            }
        }
    }

    /// Records every result's execution in the same log, including result sets.
    fn append_log(&mut self, sql: &str, message: SharedString, elapsed: Duration) {
        self.append_log_entry(sql, message, elapsed, false);
    }

    /// The same, saying whether the entry is the failure the run stopped at.
    fn append_log_entry(
        &mut self,
        sql: &str,
        message: SharedString,
        elapsed: Duration,
        failed: bool,
    ) {
        let index = self
            .results
            .iter()
            .position(|tab| matches!(tab.body, ResultBody::Log(_)))
            .unwrap_or_else(|| {
                let id = self.mint_id();
                self.results.push(ResultTab {
                    id,
                    label: ts!("query.result_log"),
                    body: ResultBody::Log(ResultLog::default()),
                });
                self.results.len() - 1
            });
        let ResultBody::Log(log) = &mut self.results[index].body else {
            unreachable!("the index belongs to the execution log");
        };
        let follow = log.scroll.is_scrolled_to_end().unwrap_or(true);
        log.entries.push(ResultLogEntry {
            sql: preview(sql).replace(['\r', '\n'], " ").into(),
            message,
            elapsed,
            failed,
        });
        if follow {
            log.scroll.scroll_to_bottom();
        }
    }

    /// Re-reads the row count the status bar shows.
    pub(super) fn recount(&mut self, cx: &App) {
        let mut rows = 0;
        let mut more = false;
        for tab in &self.results {
            if let ResultBody::Rows(grid) = &tab.body {
                let source = grid.grid.read(cx).source();
                rows += source.row_count();
                more |= source.state() == GridSourceState::HasMore;
            }
        }
        if let Some(finished) = &mut self.finished {
            finished.rows = rows;
            finished.more = more;
        }
    }

    /// Asks the driver to abandon whatever is running.
    pub(super) fn cancel(&mut self, cx: &mut Context<Self>) {
        let RunState::Running(running) = &mut self.run else {
            return;
        };
        if running.canceling {
            return;
        }
        running.canceling = true;
        let canceller = running.canceller.clone();
        // On a thread of its own: `Canceller::cancel` attaches to the JVM and
        // blocks, and the thread it must not block is the one drawing the
        // button that was just pressed.
        cx.background_spawn(async move {
            if let Err(error) = canceller.cancel() {
                log::warn!("canceling the statement failed: {error}");
            }
        })
        .detach();
        cx.notify();
    }
}

/// Runs one statement of a run and reads its first page. **Blocks**; only
/// ever called from a background thread.
///
/// A statement with a [`Pager`] is read as its first page and closed; any other
/// is executed with a fetch size and walked by [`advance`] to the first result
/// that still has rows, whose cursor stays open for the grid to page — through
/// a server cursor when `server_cursor` says it is one
/// ([`statements::server_cursor_read`]), since that cursor shares the session
/// with everything else.
fn execute(
    session: &SessionHandle,
    sql: String,
    statement: Statement<'_>,
    fetch_rows: u32,
) -> Result<Executed, JdbcError> {
    let Statement {
        effect,
        pager,
        server_cursor,
        writer,
    } = statement;
    let started = Instant::now();
    if effect == Effect::Opens {
        match writer {
            Some(writer) => session.mark_uncommitted_by(writer),
            None => session.mark_uncommitted(),
        }
    }
    let (steps, more) = match pager {
        Some(pager) => match pager.read(session.session(), 0)? {
            Some((step, more)) => (vec![step], more),
            // No result set from a `SELECT`: nothing to show but that it ran.
            None => (vec![Step::Message { update_count: 0 }], None),
        },
        None => {
            let spec = StatementSpec::new(sql.clone())
                .with_fetch_size(fetch_rows)
                .with_server_cursor(server_cursor);
            let mut cursor = session.session().execute(&spec)?;
            if effect == Effect::Ends {
                session.clear_uncommitted();
            }
            let mut steps = Vec::new();
            let pageable = advance(&mut cursor, true, fetch_rows, &mut steps)?;
            // A cursor that cannot be paged is dropped here, which closes it;
            // `then_some` is what does the dropping.
            (steps, pageable.then_some(More::Cursor(cursor)))
        }
    };
    Ok(Executed {
        sql,
        steps,
        elapsed: started.elapsed(),
        more,
    })
}

/// How one statement of a run is to be sent, worked out on the UI thread
/// where the dialect is.
struct Statement<'a> {
    /// What it does to the transaction.
    effect: Effect,
    /// How it is paged, when it is read a page at a time.
    pager: Option<Pager>,
    /// Whether it keeps a server cursor.
    server_cursor: bool,
    /// The title of the tab sending it, which a write under manual commit
    /// records against the session.
    writer: Option<&'a str>,
}

/// Which statement of a run an answer is for, and what it was.
pub(super) struct Position {
    /// Its place in the run, from zero.
    index: usize,
    /// How many statements the run has.
    total: usize,
    /// The statement, as it was sent.
    sql: String,
}

/// What one statement of a run does to the transaction its session keeps
/// account of.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Effect {
    /// Neither opens nor ends one: a read, or a session in autocommit.
    None,
    /// A write under manual commit, which leaves the transaction open.
    Opens,
    /// A `COMMIT` or a full `ROLLBACK` typed into the editor.
    Ends,
}
