//! Editing.

use super::*;

impl QueryPane {
    /// Says, in the pane itself, why the gesture that was just tried is being
    /// refused while changes are staged.
    ///
    /// The counterpart of [`QueryPane::has_pending_edits`] for the shell: a tab
    /// that simply would not close, with nothing said, would read as a bug.
    pub fn warn_pending(&mut self, cx: &mut Context<Self>) {
        self.notice = Some(ts!("data.discard_first"));
        cx.notify();
    }

    /// How much is staged against the result showing, or `None` while nothing
    /// is.
    pub(super) fn counts(&self, cx: &App) -> Option<EditCounts> {
        let counts = self.active_rows()?.grid.read(cx).source().edits().counts();
        (counts != EditCounts::default()).then_some(counts)
    }

    /// Records `value` against a cell of result tab `id`.
    ///
    /// The one way anything is staged. A value equal to the one the server gave
    /// un-stages the cell instead, which is why this goes through
    /// [`EditableSource::stage`] rather than writing into the buffer directly.
    pub(super) fn stage(
        &mut self,
        id: u64,
        row: usize,
        column: usize,
        value: StagedCell,
        cx: &mut Context<Self>,
    ) {
        let Some(grid) = self.grid_of(id).cloned() else {
            return;
        };
        grid.update(cx, |grid, cx| {
            grid.source_mut(cx).stage(row, column, value);
        });
        cx.notify();
    }

    /// Puts NULL into a cell, deliberately.
    ///
    /// The gesture the inline editor cannot offer: an empty field over a null
    /// cell commits nothing, so clearing a cell has to be a command of its own.
    pub(super) fn set_null(&mut self, id: u64, row: usize, column: usize, cx: &mut Context<Self>) {
        self.stage(id, row, column, StagedCell::Null, cx);
    }

    /// Marks a row to be deleted, or takes the mark off.
    pub(super) fn toggle_delete(&mut self, id: u64, row: usize, cx: &mut Context<Self>) {
        let Some(grid) = self.grid_of(id).cloned() else {
            return;
        };
        grid.update(cx, |grid, cx| {
            let source = grid.source_mut(cx);
            // Nothing to delete outside the rows the server sent, and this pane
            // offers no way to add one: §7.9 gives a query result updates and
            // deletes and no inserts, because a result carries the columns the
            // user selected rather than the columns the table requires.
            if source.writable() && row < source.base_rows() {
                source.edits_mut().toggle_deleted(row);
            }
        });
        cx.notify();
    }

    /// Throws away everything staged against one row.
    pub(super) fn discard_row(&mut self, id: u64, row: usize, cx: &mut Context<Self>) {
        let Some(grid) = self.grid_of(id).cloned() else {
            return;
        };
        grid.update(cx, |grid, cx| {
            let source = grid.source_mut(cx);
            let base = source.base_rows();
            source.edits_mut().discard_row(row, base);
        });
        cx.notify();
    }

    /// Throws away everything staged against one result, and puts the
    /// confirmation away with it.
    pub(super) fn discard_all(&mut self, id: u64, cx: &mut Context<Self>) {
        self.confirm_discard = false;
        // A plan describes changes that are about to stop existing, and a
        // failure describes an attempt to send them.
        self.preview = None;
        self.apply_error = None;
        self.notice = None;
        let Some(grid) = self.grid_of(id).cloned() else {
            cx.notify();
            return;
        };
        grid.update(cx, |grid, cx| {
            grid.source_mut(cx).edits_mut().discard_all();
        });
        cx.notify();
    }

    /// The cell the grid's menu should act on: wherever the caret was left.
    ///
    /// The answer is a *source* column, because the selection is kept in
    /// display positions and everything on this side of the grid is addressed
    /// in source ones.
    pub(super) fn menu_cell(&self, id: u64, cx: &App) -> Option<(usize, usize)> {
        let grid = self.grid_of(id)?.read(cx);
        let cursor = grid.selection().cursor()?;
        let column = grid.visible_column_indices().get(cursor.column).copied()?;
        Some((cursor.row, column))
    }

    /// Plans the staged changes of the result showing and puts the statements
    /// up for confirmation.
    ///
    /// Half of §7.9's apply, and the half that sends nothing.
    /// [`QueryPane::confirm_apply`] is what runs them.
    ///
    /// The read-only check is made again here even though the button that calls
    /// it is not drawn on a result that has one: this is the only function in
    /// the pane that leads to a generated write, and it is worth its two lines
    /// to have the refusal stated where the write is rather than only where the
    /// button is.
    pub(super) fn apply(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        if self.read_only || self.applying || self.preview.is_some() {
            return;
        }
        // One modal at a time: the two are irreversible in opposite directions
        // and stacking them would leave the user answering the wrong one.
        self.confirm_discard = false;
        self.apply_error = None;

        let Some(id) = self.active_id() else {
            return;
        };
        let dialect = self.dialect;
        let planned = {
            let Some(rows) = self.active_rows() else {
                return;
            };
            let Some(target) = rows.table.as_ref() else {
                return;
            };
            let source = rows.grid.read(cx).source();
            if source.edits().is_empty() {
                return;
            }
            plan_apply(
                source,
                &rows.columns,
                target.parts.clone(),
                &target.keys,
                &dialect,
            )
        };

        match planned {
            // Nothing to send, which only a buffer that counted as non-empty
            // and generated no statement can produce.
            Ok(statements) if statements.is_empty() => {}
            Ok(statements) => {
                self.preview = Some(ApplyPreview {
                    tab: id,
                    statements,
                })
            }
            Err(error) => self.apply_error = Some(ApplyProblem::local(plan_message(&error))),
        }
        cx.notify();
    }

    /// Runs the statements the confirmation showed.
    ///
    /// The whole transaction — the autocommit flip, every statement, the row
    /// counts, the commit or the rollback — happens inside one background call
    /// on the session's own worker thread. Splitting it across awaits would
    /// leave the connection mid-transaction between them, which is a state no
    /// other pane on this session could safely be allowed to find.
    pub(super) fn confirm_apply(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(ApplyPreview { tab, statements }) = self.preview.take() else {
            return;
        };
        if self.read_only {
            return;
        }
        let Some(session) = self.session.clone() else {
            self.notice = Some(ts!("explorer.disconnected"));
            cx.notify();
            return;
        };

        self.applying = true;
        self.apply_error = None;
        self.notice = Some(ts!("data.applying"));
        let generation = self.generation;
        let transactional = self.transactional;
        let restore = self.restore_auto_commit;

        cx.spawn_in(window, async move |pane, cx| {
            let outcome = cx
                .background_spawn(async move {
                    apply_batch(session.session(), &statements, transactional, restore)
                })
                .await;
            pane.update_in(cx, |pane, window, cx| {
                pane.applied(generation, tab, outcome, window, cx);
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    /// Records what one apply did.
    ///
    /// Success throws the staging buffer away and re-runs the statement that
    /// produced the rows, which is §7.9's answer to triggers and to anything
    /// else the server did on the way: what it now holds is a question only it
    /// can answer. Failure keeps every staged change exactly where it was —
    /// the rollback means nothing reached the table, so there is nothing to
    /// reconcile — and says why.
    pub(super) fn applied(
        &mut self,
        generation: u64,
        id: u64,
        outcome: Result<usize, Box<ApplyFailure>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.applying = false;
        if generation != self.generation {
            // The rows this batch was staged against have been replaced.
            cx.notify();
            return;
        }
        match outcome {
            Ok(applied) => {
                // The pane's own re-run path and not a new one: the statement
                // that produced these rows, wrapped in whatever ordering was
                // asked for, with the user's own statement carried along as the
                // thing the *next* sort wraps.
                let rerun = self
                    .rows_tab(id)
                    .map(|tab| (tab.executed.clone(), tab.sql.clone()));
                let sort = self.grid_of(id).and_then(|grid| grid.read(cx).sort());
                // Cleared before the re-run, not after: `request` and `reorder`
                // refuse while anything is staged, and there is nothing left
                // worth keeping — the server has all of it.
                self.discard_all(id, cx);
                if let Some((executed, base)) = rerun {
                    // So the grid that comes back wears the order it is in; the
                    // one that was wearing it is about to be dropped.
                    self.pending_sort = sort;
                    self.start(vec![executed], Some(base), window, cx);
                }
                self.notice = Some(ts!("data.applied", count = applied));
            }
            Err(failure) => {
                self.notice = None;
                let half_applied = failure.half_applied;
                let reconnect_required = failure.rollback.is_some();
                if let Some(error) = failure.rollback {
                    // The user is told that the batch may be half in; the
                    // driver's account of *why the unwind failed* is a second
                    // envelope with nowhere to go on screen.
                    log::error!("rolling back a failed apply failed: {error}");
                }
                let (error, message) = match failure.stop {
                    ApplyStop::Driver(error) => (Some(Box::new(QueryError::new(error))), None),
                    // §7.9's own words rather than a driver's: no statement
                    // failed, and what happened — a row somebody else has since
                    // moved — is not something a `SQLSTATE` describes.
                    ApplyStop::Stale => (None, Some(ts!("data.apply_stale"))),
                };
                self.apply_error = Some(Box::new(ApplyProblem {
                    error,
                    message,
                    half_applied,
                    reconnect_required,
                }));
            }
        }
        cx.notify();
    }
}
