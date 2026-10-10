//! Execution.

use super::*;

impl QueryPane {
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
        // The one gate every run goes through, whichever way it was asked for:
        // a sort round trip and an accepted write confirmation arrive here
        // without passing `request` again.
        let Some(session) = self.session.clone() else {
            self.notice = Some(ts!("explorer.disconnected"));
            cx.notify();
            return;
        };

        self.generation += 1;
        let generation = self.generation;
        // Dropping the old tabs drops their cursors, and `Cursor::drop` closes
        // them — which is the `CLOSE_CURSOR` a cancelled run still owes.
        self.results.clear();
        self.active_result = 0;
        self.error = None;
        self.notice = None;
        self.finished = None;
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
        self.run = RunState::Running(Box::new(Running {
            generation,
            started: Instant::now(),
            canceller: session.session().canceller(),
            cancelling: false,
        }));

        let fetch_rows = self.fetch_rows;

        // `spawn_in` rather than `spawn`: the grids this run ends in are
        // subscribed to with a window, because answering a double click with
        // [`GridView::begin_edit`] means putting the keyboard in a field and
        // there is no window to be had inside a plain `cx.subscribe` (§7.9).
        cx.spawn_in(window, async move |pane, cx| {
            for sql in statements {
                let outcome = cx
                    .background_spawn({
                        let session = session.clone();
                        async move {
                            let spec = StatementSpec::new(sql.clone()).with_fetch_size(fetch_rows);
                            let mut cursor = session.session().execute(&spec)?;
                            let mut steps = Vec::new();
                            let pageable = advance(&mut cursor, true, fetch_rows, &mut steps)?;
                            Ok::<Executed, JdbcError>(Executed {
                                sql,
                                cursor,
                                steps,
                                pageable,
                            })
                        }
                    })
                    .await;

                let carry_on = pane
                    .update_in(cx, |pane, window, cx| {
                        pane.deliver(generation, outcome, window, cx)
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
    pub(super) fn deliver(
        &mut self,
        generation: u64,
        outcome: Result<Executed, JdbcError>,
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
                    cursor,
                    steps,
                    pageable,
                } = executed;
                // A cursor that cannot be paged is dropped here, which closes
                // it; `then_some` is what does the dropping.
                self.append(&sql, steps, pageable.then_some(cursor), window, cx);
                self.recount(cx);
                cx.notify();
                true
            }
            Err(error) => {
                self.error = Some(QueryError::new(error));
                self.finish(generation, cx);
                false
            }
        }
    }

    /// Turns one statement's results into tabs.
    ///
    /// `cursor` is the open cursor when the walk stopped on a result set with
    /// rows still to come; it goes to the last grid tab, which is the only one
    /// `MORE_RESULTS` has not already closed.
    pub(super) fn append(
        &mut self,
        sql: &str,
        steps: Vec<Step>,
        cursor: Option<Cursor>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let mut cursor = cursor;
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
                    let carried = (Some(index) == last_rows).then(|| cursor.take()).flatten();
                    let state = if complete && carried.is_none() {
                        GridSourceState::Complete
                    } else {
                        GridSourceState::HasMore
                    };
                    self.push_rows(
                        base.clone(),
                        sql.to_string(),
                        columns,
                        batch,
                        state,
                        carried,
                        window,
                        cx,
                    );
                }
                Step::Message { update_count } => {
                    let text = if update_count > 0 {
                        ts!("query.rows_affected", count = update_count)
                    } else {
                        ts!("query.executed")
                    };
                    let id = self.mint_id();
                    let label = ts!("query.result", index = self.results.len() + 1);
                    self.results.push(ResultTab {
                        id,
                        label,
                        body: ResultBody::Message(text),
                    });
                }
            }
        }
    }

    /// Re-reads the row count the status bar shows.
    pub(super) fn recount(&mut self, cx: &App) {
        let rows: usize = self
            .results
            .iter()
            .filter_map(|tab| match &tab.body {
                ResultBody::Rows(rows) => Some(rows.grid.read(cx).source().row_count()),
                ResultBody::Message(_) => None,
            })
            .sum();
        if let Some(finished) = &mut self.finished {
            finished.rows = rows;
        }
    }

    /// Asks the driver to abandon whatever is running.
    pub(super) fn cancel(&mut self, cx: &mut Context<Self>) {
        let RunState::Running(running) = &mut self.run else {
            return;
        };
        if running.cancelling {
            return;
        }
        running.cancelling = true;
        let canceller = running.canceller.clone();
        // On a thread of its own: `Canceller::cancel` attaches to the JVM and
        // blocks, and the thread it must not block is the one drawing the
        // button that was just pressed.
        cx.background_spawn(async move {
            if let Err(error) = canceller.cancel() {
                log::warn!("cancelling the statement failed: {error}");
            }
        })
        .detach();
        cx.notify();
    }
}
