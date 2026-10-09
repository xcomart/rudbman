//! Results.

use super::*;

impl QueryPane {
    /// Mints a tab id.
    pub(super) fn mint_id(&mut self) -> u64 {
        let id = self.next_tab_id;
        self.next_tab_id += 1;
        id
    }

    /// Adds a grid tab over one result set's first batch.
    ///
    /// `sql` is the statement a later sort should wrap and `executed` the one
    /// that produced these rows; see [`RowsTab::executed`].
    ///
    /// The grid is built **read-only** and the rows go on screen at once. What
    /// makes it editable is a second round trip — the table's primary key — and
    /// §7.9 will not have the rows wait for one: [`QueryPane::resolve_edits`]
    /// asks, and the answer either opens the grid or says why it stays shut.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn push_rows(
        &mut self,
        sql: String,
        executed: String,
        columns: Vec<ColumnInfo>,
        batch: RenderedBatch,
        state: GridSourceState,
        cursor: Option<Cursor>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let id = self.mint_id();
        let mut source = ResultSource::new(&columns);
        source.push(batch);
        source.set_state(state);
        // `Inferred`: the table this result writes back to is worked out from
        // the columns themselves, so a column that named none is computed and
        // takes no edit (see [`TableSource`]).
        let source = EditableSource::new(source, &columns, false, TableSource::Inferred);

        let grid = cx.new(|cx| GridView::new(source, cx));
        // The marker the sort that asked for this run was for, if it was one:
        // the grid it was set on has just been replaced by this one.
        if let Some(sort) = self.pending_sort.take() {
            grid.update(cx, |grid, cx| grid.set_sort(Some(sort), cx));
        }
        let events = cx.subscribe_in(&grid, window, move |pane, _grid, event, window, cx| {
            match event {
                GridEvent::NearEnd => pane.fetch_more(id, window, cx),
                GridEvent::SortRequested { column, direction } => {
                    pane.reorder(id, *column, *direction, window, cx);
                }
                GridEvent::CellActivated { row, column } => {
                    pane.open_cell(id, *row, *column, window, cx);
                }
                // Reachable now that a result can be written back to (§7.9).
                // The grid refuses to open a field over a cell its source calls
                // read-only, so anything that arrives here is a value the user
                // meant to change.
                GridEvent::EditCommitted { row, column, value } => {
                    // An emptied field is the empty string — that is how one is
                    // typed — so only the clearing gesture asks for `NULL`, and
                    // it stages what the grid menu's "set null" stages.
                    let staged = match value {
                        rugpui_grid::EditValue::Text(text) => StagedCell::Text(text.clone()),
                        rugpui_grid::EditValue::Null => StagedCell::Null,
                    };
                    pane.stage(id, *row, *column, staged, cx);
                }
                // The grid holds no strings, so its menu is drawn here
                // (architecture document, §7.8).
                GridEvent::ContextMenu { target, position } => {
                    pane.context_menu = Some(PaneMenu::Grid {
                        id,
                        target: *target,
                        position: *position,
                    });
                    cx.notify();
                }
            }
        });

        let label = ts!("query.result", index = self.results.len() + 1);
        self.results.push(ResultTab {
            id,
            label,
            body: ResultBody::Rows(Box::new(RowsTab {
                grid,
                sql,
                executed,
                columns: Arc::new(columns),
                table: None,
                read_only: None,
                cursor,
                fetching: false,
                _events: events,
            })),
        });
        self.resolve_edits(id, cx);
    }

    /// Works out whether result tab `id` may be written back to, and opens it
    /// when it may.
    ///
    /// The two clauses of §7.9's gate that need no server are answered here;
    /// the third — the table's primary key, in full, among the columns that
    /// were read — is a `DESCRIBE` and goes out in the background, because the
    /// rows are already on screen and must not wait for it.
    pub(super) fn resolve_edits(&mut self, id: u64, cx: &mut Context<Self>) {
        if self.read_only {
            // §8's veto, in the same words the data pane says it in: it is the
            // same fact about the same connection.
            self.mark_read_only(id, ts!("data.read_only"), cx);
            return;
        }
        // A detached pane can still be read; it cannot ask the catalogue
        // anything, so its results stay as they came.
        let Some(session) = self.session.clone() else {
            self.mark_read_only(id, ts!("query.not_editable"), cx);
            return;
        };
        let Some(tab) = self.rows_tab(id) else {
            return;
        };
        let Some((catalog, schema, table)) = source_table(&tab.columns) else {
            self.mark_read_only(id, ts!("query.not_editable"), cx);
            return;
        };
        let generation = self.generation;

        cx.spawn(async move |pane, cx| {
            let outcome = cx
                .background_spawn(async move {
                    let keys = primary_key(
                        session.session(),
                        catalog.as_deref(),
                        schema.as_deref(),
                        &table,
                    )?;
                    Ok::<_, JdbcError>(((catalog, schema, table), keys))
                })
                .await;
            pane.update(cx, |pane, cx| pane.keyed(generation, id, outcome, cx))
                .ok();
        })
        .detach();
    }

    /// Records what the key lookup for result tab `id` answered.
    ///
    /// Guarded by the generation **and** by the tab id, and it needs both: a
    /// script produces several result tabs from one run, so the generation
    /// alone would let one statement's key open another statement's grid.
    pub(super) fn keyed(
        &mut self,
        generation: u64,
        id: u64,
        outcome: Result<(TableName, Vec<String>), JdbcError>,
        cx: &mut Context<Self>,
    ) {
        if generation != self.generation {
            // The rows this was asked about have been replaced.
            return;
        }
        let ((catalog, schema, table), keys) = match outcome {
            Ok(answer) => answer,
            Err(error) => {
                // Not a failure of the query: the rows are there and readable,
                // and all that is lost is an offer. Logged rather than shown,
                // for the same reason the offer is never explained in detail —
                // §7.9's hint is only ever allowed to *offer* editing.
                log::warn!("reading a query result's primary key failed: {error}");
                self.mark_read_only(id, ts!("query.not_editable"), cx);
                return;
            }
        };
        let parts = builder_sql::table_parts(catalog.as_deref(), schema.as_deref(), &table);
        let qualified = self.dialect.qualify(parts.iter().map(String::as_str));

        let Some(tab) = self.rows_tab(id) else {
            return;
        };
        // The third clause: every key column has to be among the columns that
        // were read, or the `WHERE` clause has no value to find the row by —
        // which is also what disqualifies most aggregates, since
        // `SELECT dept, COUNT(*) FROM emp GROUP BY dept` names `emp` and
        // carries none of its key. Matched through `key_index` over
        // `column_name`, which is the same matching `mark_primary_keys` and
        // `plan_apply` use, so an aliased key column is still found and no
        // fourth rule exists to drift from the other three.
        let names: Vec<String> = tab.columns.iter().map(column_name).collect();
        let complete = !keys.is_empty() && keys.iter().all(|key| key_index(&names, key).is_some());
        if !complete {
            self.mark_read_only(id, ts!("query.not_editable"), cx);
            return;
        }

        let grid = tab.grid.clone();
        tab.table = Some(EditTarget {
            parts,
            keys: keys.clone(),
        });
        grid.update(cx, |grid, cx| {
            // The table a copied `INSERT` names, now that one is known.
            grid.set_insert_table(Some(SharedString::from(qualified)));
            grid.source_mut(cx).make_editable(&keys);
        });
        cx.notify();
    }

    /// Records why result tab `id` may only be read.
    pub(super) fn mark_read_only(&mut self, id: u64, reason: SharedString, cx: &mut Context<Self>) {
        if let Some(tab) = self.rows_tab(id) {
            tab.read_only = Some(reason);
        }
        cx.notify();
    }

    /// The rows tab with this id, if it is still there.
    pub(super) fn rows_tab(&mut self, id: u64) -> Option<&mut RowsTab> {
        self.results
            .iter_mut()
            .find(|tab| tab.id == id)
            .and_then(|tab| match &mut tab.body {
                ResultBody::Rows(rows) => Some(&mut **rows),
                ResultBody::Message(_) => None,
            })
    }

    /// The grid has come within sight of the last row it holds.
    pub(super) fn fetch_more(&mut self, id: u64, window: &mut Window, cx: &mut Context<Self>) {
        if self.session.is_none() {
            // `detach` has already dropped the cursors, so this is only reached
            // by a scroll that was already in flight; saying why the rows stop
            // is better than a grid that quietly ends early.
            self.notice = Some(ts!("explorer.disconnected"));
            cx.notify();
            return;
        }
        let generation = self.generation;
        let fetch_rows = self.fetch_rows;
        let Some(tab) = self.rows_tab(id) else {
            return;
        };
        if tab.fetching {
            return;
        }
        let Some(mut cursor) = tab.cursor.take() else {
            return;
        };
        tab.fetching = true;
        let columns = Arc::clone(&tab.columns);
        let grid = tab.grid.clone();
        // While a batch is on its way the source says so, which is what keeps a
        // fast scroll from asking once per frame.
        grid.update(cx, |grid, cx| {
            grid.source_mut(cx).set_state(GridSourceState::Loading);
        });

        cx.spawn_in(window, async move |pane, cx| {
            let outcome = cx
                .background_spawn(async move {
                    match page(&mut cursor, &columns, fetch_rows) {
                        Ok(paged) => Ok((cursor, paged)),
                        Err(error) => Err(error),
                    }
                })
                .await;
            pane.update_in(cx, |pane, window, cx| {
                pane.paged(id, generation, outcome, window, cx);
            })
            .ok();
        })
        .detach();
    }

    /// Records a page fetched for tab `id`.
    pub(super) fn paged(
        &mut self,
        id: u64,
        generation: u64,
        outcome: Result<(Cursor, Paged), JdbcError>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if generation != self.generation {
            // The run this belonged to has been superseded. Dropping the
            // outcome closes its cursor, and the rows never reach a grid.
            return;
        }
        match outcome {
            Ok((cursor, paged)) => {
                let Paged {
                    batch,
                    complete,
                    steps,
                    pageable,
                } = paged;
                let mut cursor = Some(cursor);
                // The statement that ran, not the one a sort would wrap:
                // `append` works the second out of `sort_base` for itself.
                let sql = self.rows_tab(id).map(|tab| tab.executed.clone());

                if let Some(tab) = self.rows_tab(id) {
                    tab.fetching = false;
                    // A completed result set is behind the cursor now: the walk
                    // in `page` has already moved past it.
                    tab.cursor = if complete { None } else { cursor.take() };
                    let state = if complete {
                        GridSourceState::Complete
                    } else {
                        GridSourceState::HasMore
                    };
                    let grid = tab.grid.clone();
                    grid.update(cx, |grid, cx| {
                        let source = grid.source_mut(cx);
                        // Into the rows the server sent, under the overlay: the
                        // staged edits are keyed to base row indices, and
                        // appending moves none of them.
                        source.base_mut().push(batch);
                        source.set_state(state);
                    });
                }

                // Results the `MORE_RESULTS` walk picked up once this one ended.
                if !steps.is_empty()
                    && let Some(sql) = sql
                {
                    let carried = if pageable { cursor.take() } else { None };
                    self.append(&sql, steps, carried, window, cx);
                }
                self.recount(cx);
            }
            Err(error) => {
                if let Some(tab) = self.rows_tab(id) {
                    tab.fetching = false;
                    tab.cursor = None;
                    let grid = tab.grid.clone();
                    grid.update(cx, |grid, cx| {
                        grid.source_mut(cx).set_state(GridSourceState::Complete);
                    });
                }
                self.error = Some(QueryError::new(error));
            }
        }
        cx.notify();
    }

    /// A header was clicked: re-run the statement in that order.
    ///
    /// Only for reading statements. Re-executing an `INSERT` because a column
    /// heading was clicked is not a thing this program will do.
    pub(super) fn reorder(
        &mut self,
        id: u64,
        column: usize,
        direction: Option<SortDirection>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.session.is_none() {
            // Sorting is a re-run, and there is nothing left to run it on. Said
            // here rather than left to `start`, so the grid's own header click
            // gets an answer even when the statement turns out unsortable.
            self.notice = Some(ts!("explorer.disconnected"));
            cx.notify();
            return;
        }
        if self.is_running() {
            self.notice = Some(ts!("query.busy"));
            cx.notify();
            return;
        }
        if self.has_pending_edits(cx) {
            // A re-run replaces the source the edits are keyed to; §7.9.
            self.notice = Some(ts!("data.discard_first"));
            cx.notify();
            return;
        }
        let dialect = self.dialect;
        let Some(tab) = self.rows_tab(id) else {
            return;
        };
        let base = tab.sql.clone();
        let name = tab
            .grid
            .read(cx)
            .source()
            .column(column)
            .name
            .trim()
            .to_string();
        if name.is_empty() || is_write_statement(&base, &dialect) {
            return;
        }
        let sql = match direction {
            Some(direction) => order_by(&base, &name, direction, &dialect),
            // The third click drops the ordering, which is the original query.
            None => base.clone(),
        };
        // Carried across the re-run, so the grid that comes back wears the
        // marker for the order it is actually in; see
        // [`QueryPane::pending_sort`].
        self.pending_sort = direction.map(|direction| (column, direction));
        self.start(vec![sql], Some(base), window, cx);
    }

    /// A cell was opened: a field over it, or a word about why not.
    ///
    /// A LOB has no body in the grid — only its size travelled — and reading
    /// one needs `LOB_READ` (0x25), which the bridge answers "not implemented"
    /// (architecture document, §12, open question 7). Saying so is better than
    /// a viewer that shows nothing.
    ///
    /// Anything else goes to [`GridView::begin_edit`], which refuses on its own
    /// for a cell that cannot take a field — a result that failed §7.9's gate,
    /// a computed column, a row on its way out — so there is nothing to check
    /// here that is not already checked where it is known.
    // TODO(M4): open a chunked LOB viewer once `LOB_READ` lands in the bridge.
    pub(super) fn open_cell(
        &mut self,
        id: u64,
        row: usize,
        column: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(grid) = self.grid_of(id).cloned() else {
            return;
        };
        if matches!(
            grid.read(cx).source().cell(row, column),
            GridCell::Lob { .. }
        ) {
            self.notice = Some(ts!("query.lob_unsupported"));
            cx.notify();
            return;
        }
        grid.update(cx, |grid, cx| {
            grid.begin_edit(row, column, window, cx);
        });
    }

    /// Ends the run, whether it succeeded or not.
    pub(super) fn finish(&mut self, generation: u64, cx: &mut Context<Self>) {
        if generation != self.generation {
            return;
        }
        let elapsed = match &self.run {
            RunState::Running(running) => running.started.elapsed(),
            RunState::Idle => Duration::ZERO,
        };
        self.run = RunState::Idle;
        self.finished = Some(Finished { rows: 0, elapsed });
        self.recount(cx);
        cx.notify();
    }
}
