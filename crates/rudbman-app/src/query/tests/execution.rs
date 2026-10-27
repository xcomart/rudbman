use super::*;

/// One table behind every column that names one is what the gate is for.
#[test]
fn one_table_behind_the_columns_is_the_table_a_result_writes_back_to() {
    assert_eq!(
        source_table(&[of_users("ID"), of_users("NAME")]),
        Some((
            Some("APP".to_string()),
            Some("PUBLIC".to_string()),
            "USERS".to_string()
        ))
    );
}

/// Two tables are a join, and an `UPDATE` names one.
#[test]
fn two_tables_among_the_columns_disqualify_the_result() {
    let columns = vec![
        of_users("ID"),
        described("NAME", Some("APP"), Some("PUBLIC"), Some("ORDERS")),
    ];
    assert_eq!(source_table(&columns), None);

    // The same table in another schema is another table, and so is the
    // same table in another catalog: the whole triple has to agree.
    for other in [
        described("N", Some("APP"), Some("OTHER"), Some("USERS")),
        described("N", Some("OTHER"), Some("PUBLIC"), Some("USERS")),
    ] {
        assert_eq!(source_table(&[of_users("ID"), other]), None);
    }
}

/// A result whose every column is computed has nothing to write back to.
#[test]
fn a_result_of_only_computed_columns_names_no_table() {
    // `SELECT 1, count(*) FROM …`, as the two drivers spell it: one
    // answers null and the other the empty string.
    let columns = vec![
        described("C1", None, None, None),
        described("C2", Some(""), Some(""), Some("")),
    ];
    assert_eq!(source_table(&columns), None);
}

/// The empty string is not a name, wherever it turns up.
#[test]
fn an_empty_name_part_reads_as_unknown_and_not_as_a_name() {
    // A driver that returns `""` for the qualifiers it will not report and
    // one that returns null are describing the same table, so the two
    // spellings have to agree rather than count as two tables.
    let columns = vec![
        described("ID", Some(""), Some(""), Some("USERS")),
        described("NAME", None, None, Some("USERS")),
    ];
    assert_eq!(
        source_table(&columns),
        Some((None, None, "USERS".to_string())),
        "an empty catalog or schema was taken for a name"
    );

    // And a column whose *table* is the empty string is computed, not a
    // column of the table called "".
    assert_eq!(source_table(&[described("C", None, None, Some(""))]), None);
}

/// §7.9's own example: one computed column does not refuse the two beside
/// it, and the computed one is read-only where they are not.
#[test]
fn a_computed_column_is_read_only_rather_than_disqualifying() {
    // `SELECT id, name, name || '!' FROM users`.
    let columns = vec![
        of_users("ID"),
        of_users("NAME"),
        described("C3", Some(""), Some(""), Some("")),
    ];
    assert_eq!(
        source_table(&columns),
        Some((
            Some("APP".to_string()),
            Some("PUBLIC".to_string()),
            "USERS".to_string()
        )),
        "one computed column refused a result two of whose columns are writable"
    );

    // And what the gate lets through, the column rules still hold back:
    // there is no column for an `UPDATE` to assign the third one to.
    let mut base = ResultSource::new(&columns);
    base.push(crate::query_source::render_batch(
        &crate::query_source::tests::batch(&[&[Some("1")], &[Some("a")], &[Some("a!")]]),
        &columns,
    ));
    let mut source = EditableSource::new(base, &columns, false, TableSource::Inferred);
    source.make_editable(&["ID".to_string()]);
    assert!(source.cell_editable(0, 0));
    assert!(source.cell_editable(0, 1));
    assert!(
        !source.cell_editable(0, 2),
        "a computed column took an edit"
    );
}

#[test]
fn sorting_wraps_the_statement_rather_than_appending_to_it() {
    // Appending would produce two `ORDER BY` clauses whenever the original
    // had one, which is a syntax error rather than a sort.
    let wrapped = order_by(
        "select id from t order by name;",
        "id",
        SortDirection::Descending,
        &Dialect::H2,
    );
    assert_eq!(
        wrapped,
        r#"SELECT * FROM (select id from t order by name) rudbman_sort ORDER BY "id" DESC"#
    );

    // MySQL spells a quoted identifier with backticks, because `"` opens a
    // string there.
    assert_eq!(
        order_by("select 1", "a b", SortDirection::Ascending, &Dialect::MYSQL),
        "SELECT * FROM (select 1) rudbman_sort ORDER BY `a b` ASC"
    );
    // And a quote inside the name is doubled, not dropped.
    assert_eq!(
        order_by(
            "select 1",
            "we\"ird",
            SortDirection::Ascending,
            &Dialect::H2
        ),
        "SELECT * FROM (select 1) rudbman_sort ORDER BY \"we\"\"ird\" ASC"
    );
}

#[test]
fn sorting_strips_a_trailing_comment_rather_than_wrapping_it() {
    // A line comment left inside the parentheses would run to the end of the
    // line and take the `)` and the `ORDER BY` with it.
    let expected = r#"SELECT * FROM (select id from t) rudbman_sort ORDER BY "id" ASC"#;
    for sql in [
        "select id from t -- note",
        "select id from t; -- note",
        "select id from t -- note\n;",
        "select id from t /* note */",
        "select id from t /* note */ ;",
        "  select id from t\n-- one\n/* two */\n;\n",
    ] {
        assert_eq!(
            order_by(sql, "id", SortDirection::Ascending, &Dialect::H2),
            expected,
            "{sql:?}"
        );
    }

    // A comment marker inside a literal is the literal's, not a comment.
    assert_eq!(
        order_by(
            "select '-- not a comment' as id",
            "id",
            SortDirection::Ascending,
            &Dialect::H2
        ),
        r#"SELECT * FROM (select '-- not a comment' as id) rudbman_sort ORDER BY "id" ASC"#
    );
    // MySQL's `#` is a line comment too.
    assert_eq!(
        order_by(
            "select 1 as id # note",
            "id",
            SortDirection::Ascending,
            &Dialect::MYSQL
        ),
        "SELECT * FROM (select 1 as id) rudbman_sort ORDER BY `id` ASC"
    );
    // An executable comment is part of the statement, and stays.
    assert_eq!(
        order_by(
            "select 1 as id /*!50000 */",
            "id",
            SortDirection::Ascending,
            &Dialect::MYSQL
        ),
        "SELECT * FROM (select 1 as id /*!50000 */) rudbman_sort ORDER BY `id` ASC"
    );
}

/// The whole of infinite scrolling, against ten thousand real rows: the
/// first batch arrives on its own, the source says there are more, and each
/// page appends until the driver runs out and says so.
#[gpui::test]
fn ten_thousand_rows_arrive_one_batch_at_a_time(cx: &mut TestAppContext) {
    let (connected, profile) = h2(
        "paging",
        &[
            "create table BIG (ID int primary key)",
            "insert into BIG select X from system_range(1, 10000)",
        ],
    );
    let window = pane(&connected, &profile, 1_000, cx);
    run(&window, "select ID from BIG order by ID", cx);

    let (id, statement_elapsed, total_elapsed) = window
        .update(cx, |pane, _window, cx| {
            assert!(pane.error.is_none(), "{:?}", pane.error);
            assert_eq!(pane.results.len(), 2);
            let source = pane.grid_at(0).read(cx).source();
            assert_eq!(source.row_count(), 1_000, "one batch, not the whole table");
            assert_eq!(source.state(), GridSourceState::HasMore);
            (
                pane.row_result(0).id,
                pane.log_at(0)[0].elapsed,
                pane.finished.as_ref().unwrap().elapsed,
            )
        })
        .expect("the window is open");

    // What `GridEvent::NearEnd` does once the viewport reaches the end. The
    // event needs a laid-out window; the fetch it asks for does not.
    let mut pages = 0;
    while state(&window, 0, cx) != GridSourceState::Complete {
        window
            .update(cx, |pane, window, cx| pane.fetch_more(id, window, cx))
            .expect("the window is open");
        cx.run_until_parked();
        pages += 1;
        assert!(pages < 20, "the source never reached the end");
    }

    let rows = column_zero(&window, 0, cx);
    assert_eq!(rows.len(), 10_000);
    assert_eq!(rows[0], "1");
    assert_eq!(rows[9_999], "10000");
    window
        .update(cx, |pane, _window, _cx| {
            let (count, elapsed) = pane.status_cells();
            assert_eq!(count, ts!("query.row_count", count = 10_000));
            assert!(!elapsed.is_empty(), "the status bar reports a duration");
            assert_eq!(
                pane.log_at(0).len(),
                1,
                "paging must not log the statement again"
            );
            assert_eq!(pane.log_at(0)[0].elapsed, statement_elapsed);
            assert_eq!(pane.finished.as_ref().unwrap().elapsed, total_elapsed);
        })
        .expect("the window is open");
}

/// A mixed script keeps ordered messages and timings alongside its two grids.
#[gpui::test]
fn a_script_keeps_result_sets_separate_and_appends_messages_to_one_log(cx: &mut TestAppContext) {
    let (connected, profile) = h2(
        "script",
        &[
            "create table T (ID int primary key, N varchar(10))",
            "insert into T values (1, 'a'), (2, 'b'), (3, 'c')",
        ],
    );
    let window = pane(&connected, &profile, 500, cx);
    window
        .update(cx, |pane, _window, cx| {
            pane.editor.update(cx, |editor, cx| {
                editor.set_text(
                    "select ID from T order by ID;\n\
                         update T set N = 'x' where ID <= 2;\n\
                         select N from T order by ID;\n\
                         delete from T where ID = 99;\n\
                         create table EXTRA (ID int);",
                    cx,
                );
                // Exactly what the editor raises for "run everything", so
                // the split and the subscription are both under test.
                cx.emit(EditorEvent::RunAll);
            });
        })
        .expect("the window is open");
    cx.run_until_parked();

    window
        .update(cx, |pane, _window, _cx| {
            assert!(pane.error.is_none(), "{:?}", pane.error);
            assert_eq!(pane.results.len(), 3, "two grids and one shared log");
            assert_eq!(pane.results[0].label, ts!("query.result_log"));
            assert_eq!(pane.results[1].label, ts!("query.result", index = 1));
            assert_eq!(pane.results[2].label, ts!("query.result", index = 2));
            let log = pane.log_at(0);
            assert_eq!(log.len(), 5);
            assert_eq!(
                &log[1].message,
                &ts!("query.rows_affected", count = 2),
                "the update count is the driver's, not a guess"
            );
            assert_eq!(log[0].message, ts!("query.executed"));
            assert_eq!(log[2].message, ts!("query.executed"));
            assert_eq!(log[3].message, ts!("query.executed"));
            assert_eq!(log[4].message, ts!("query.executed"));
            assert!(log[0].sql.starts_with("select ID"));
            assert!(log[1].sql.starts_with("update T"));
            assert!(log[2].sql.starts_with("select N"));
            assert!(log[3].sql.starts_with("delete from T"));
            assert!(log[4].sql.starts_with("create table EXTRA"));
            assert!(log.iter().all(|entry| !entry.elapsed.is_zero()));
            let statement_total: Duration = log.iter().map(|entry| entry.elapsed).sum();
            assert!(
                pane.finished.as_ref().unwrap().elapsed >= statement_total,
                "the total covers every statement, without counting earlier ones twice"
            );
        })
        .expect("the window is open");

    assert_eq!(column_zero(&window, 0, cx), ["1", "2", "3"]);
    assert_eq!(column_zero(&window, 1, cx), ["x", "x", "c"]);
}

#[gpui::test]
fn a_write_only_script_has_one_log_and_the_next_run_starts_fresh(cx: &mut TestAppContext) {
    let (connected, profile) = h2("script-log", &[]);
    let window = pane(&connected, &profile, 500, cx);
    window
        .update(cx, |pane, window, cx| {
            pane.request(
                vec![
                    "create table T (ID int)".into(),
                    "insert into T values (1)".into(),
                    "update T set ID = 2".into(),
                ],
                window,
                cx,
            );
        })
        .unwrap();
    cx.run_until_parked();
    let old_id = window
        .update(cx, |pane, _, _| {
            assert!(pane.error.is_none(), "{:?}", pane.error);
            assert_eq!(pane.results.len(), 1);
            assert_eq!(pane.results[0].label, ts!("query.result_log"));
            let log = pane.log_at(0);
            assert_eq!(log.len(), 3);
            assert_eq!(log[0].message, ts!("query.executed"));
            assert_eq!(log[1].message, ts!("query.rows_affected", count = 1));
            assert_eq!(log[2].message, ts!("query.rows_affected", count = 1));
            pane.results[0].id
        })
        .unwrap();

    // An empty result set still has a grid as well as its execution log.
    run(&window, "select ID from T where 1 = 0", cx);
    assert!(column_zero(&window, 0, cx).is_empty());
    window
        .update(cx, |pane, _, _| {
            assert_eq!(pane.results.len(), 2);
            assert_eq!(pane.results[1].label, ts!("query.result", index = 1));
            assert_eq!(pane.log_at(0).len(), 1);
            assert!(pane.log_at(0)[0].sql.starts_with("select ID"));
        })
        .unwrap();

    run(&window, "delete from T", cx);
    window
        .update(cx, |pane, _, _| {
            assert_eq!(pane.results.len(), 1);
            assert_ne!(pane.results[0].id, old_id);
            assert_eq!(pane.log_at(0).len(), 1);
            assert!(pane.log_at(0)[0].sql.starts_with("delete from T"));
        })
        .unwrap();
    connected.close().expect("close");
}

/// A statement that would never finish, canceled, and a session that is
/// still usable afterwards.
///
/// The cancel comes from a thread of its own rather than from the button:
/// the test executor runs the blocking `EXECUTE` on the very thread that
/// would otherwise deliver the click, and [`Canceller`] is `Send + Sync`
/// for exactly this reason.
#[gpui::test]
fn a_canceled_statement_says_so_and_leaves_the_session_working(cx: &mut TestAppContext) {
    let (connected, profile) = h2("cancel", &[]);
    let canceller = connected.session().canceller();
    let window = pane(&connected, &profile, 500, cx);

    let stopper = std::thread::spawn(move || {
        for _ in 0..60 {
            std::thread::sleep(Duration::from_millis(200));
            if canceller.cancel().unwrap_or(0) > 0 {
                return true;
            }
        }
        false
    });

    run(
        &window,
        "select count(*) from system_range(1, 200000) a, system_range(1, 200000) b \
             where a.X <> b.X",
        cx,
    );
    let reached = stopper.join().expect("the canceling thread finished");

    window
        .update(cx, |pane, _window, _cx| {
            assert!(reached, "the cancel never reached a running statement");
            let error = pane.error.as_ref().expect("a cancel is a failure here");
            assert!(
                error.is_canceled(),
                "kind {:?}, sqlstate {:?}: {}",
                error.kind,
                error.sql_state,
                error.message
            );
            assert_eq!(error.hint(), Some(ts!("query.hint_canceled")));
            assert!(!pane.is_running(), "the run ended with the cancel");
        })
        .expect("the window is open");

    // The session is the interesting part: a cancel that left the
    // connection unusable would be worse than no cancel at all.
    run(&window, "select 42", cx);
    window
        .update(cx, |pane, _window, _cx| {
            assert!(pane.error.is_none(), "{:?}", pane.error);
        })
        .expect("the window is open");
    assert_eq!(column_zero(&window, 0, cx), ["42"]);
}

/// The sort round trip really reorders, because the server does it.
#[gpui::test]
fn sorting_comes_back_in_the_new_order(cx: &mut TestAppContext) {
    let (connected, profile) = h2(
        "sorting",
        &[
            "create table S (ID int primary key)",
            "insert into S values (3), (1), (2)",
        ],
    );
    let window = pane(&connected, &profile, 500, cx);
    run(&window, "select ID from S", cx);

    // What the statement answers with no ordering at all. Not asserted as a
    // literal: an unordered `SELECT` may come back in any order, and here it
    // happens to arrive from the primary key index.
    let unsorted = column_zero(&window, 0, cx);

    for (direction, expected) in [
        (
            Some(SortDirection::Descending),
            vec!["3".to_string(), "2".to_string(), "1".to_string()],
        ),
        (
            Some(SortDirection::Ascending),
            vec!["1".to_string(), "2".to_string(), "3".to_string()],
        ),
        // The third click drops the ordering, which is the original query.
        (None, unsorted.clone()),
    ] {
        // A re-run replaces the tab, so the id has to be read afresh — the
        // same thing the grid's own subscription does when it fires again.
        let id = window
            .update(cx, |pane, _window, _cx| pane.row_result(0).id)
            .expect("the window is open");
        window
            .update(cx, |pane, sorted, cx| {
                pane.reorder(id, 0, direction, sorted, cx);
            })
            .expect("the window is open");
        cx.run_until_parked();
        window
            .update(cx, |pane, _window, _cx| {
                assert!(pane.error.is_none(), "{:?}", pane.error);
                // Every sort wraps the statement the user wrote, never the
                // wrapper the last one produced.
                let ResultBody::Rows(rows) = &pane.row_result(0).body else {
                    panic!("a grid");
                };
                assert_eq!(rows.sql, "select ID from S");
            })
            .expect("the window is open");
        assert_eq!(column_zero(&window, 0, cx), expected, "{direction:?}");
    }
}

/// A page of a superseded run never lands in the run that replaced it.
#[gpui::test]
fn a_superseded_page_never_reaches_the_new_grid(cx: &mut TestAppContext) {
    let (connected, profile) = h2(
        "generations",
        &[
            "create table G (ID int primary key)",
            "insert into G select X from system_range(1, 3000)",
        ],
    );
    let window = pane(&connected, &profile, 1_000, cx);
    run(&window, "select ID from G order by ID", cx);

    let id = window
        .update(cx, |pane, _window, _cx| {
            assert_eq!(pane.results.len(), 2);
            pane.row_result(0).id
        })
        .expect("the window is open");

    // A page goes out and, before it is answered, the user runs something
    // else. Neither task has run yet: nothing here is parked.
    window
        .update(cx, |pane, live, cx| {
            pane.fetch_more(id, live, cx);
            pane.request(vec!["select 7".to_string()], live, cx);
        })
        .expect("the window is open");
    cx.run_until_parked();

    window
        .update(cx, |pane, _window, _cx| {
            assert!(pane.error.is_none(), "{:?}", pane.error);
            assert_eq!(pane.results.len(), 2, "the old tab went with its run");
        })
        .expect("the window is open");
    assert_eq!(
        column_zero(&window, 0, cx),
        ["7"],
        "the old run's thousand rows must not be in the new grid"
    );
}

/// The two write guards: one refuses, the other asks.
#[gpui::test]
fn a_write_is_refused_outright_or_asked_about(cx: &mut TestAppContext) {
    let (connected, mut profile) = h2(
        "guards",
        &["create table W (ID int primary key, N varchar(10))"],
    );

    profile.read_only = true;
    let refusing = pane(&connected, &profile, 500, cx);
    run(&refusing, "insert into W values (1, 'a')", cx);
    refusing
        .update(cx, |pane, _window, _cx| {
            assert!(!pane.ran, "a read-only profile never sends the statement");
            assert_eq!(pane.notice, Some(ts!("query.read_only")));
            assert!(pane.pending.is_none(), "a refusal is not a question");
        })
        .expect("the window is open");
    // And a read goes through on the same pane.
    run(&refusing, "select count(*) from W", cx);
    assert_eq!(column_zero(&refusing, 0, cx), ["0"]);

    profile.read_only = false;
    profile.confirm_writes = true;
    let asking = pane(&connected, &profile, 500, cx);
    run(&asking, "-- add one\ninsert into W values (2, 'b')", cx);
    asking
        .update(cx, |pane, _window, _cx| {
            assert!(!pane.ran, "nothing runs until the question is answered");
            assert!(pane.pending.is_some());
        })
        .expect("the window is open");

    asking
        .update(cx, |pane, window, cx| pane.confirmed(window, cx))
        .expect("the window is open");
    cx.run_until_parked();
    asking
        .update(cx, |pane, _window, _cx| {
            assert!(pane.error.is_none(), "{:?}", pane.error);
            assert_eq!(
                pane.log_at(0)[0].message,
                ts!("query.rows_affected", count = 1)
            );
        })
        .expect("the window is open");
}

/// The pane has two focusable halves and [`Focusable`] can name only one of
/// them, which is why the shell asks [`QueryPane::contains_focus`] instead
/// before it stops rendering a tab: a click in the grid puts the keyboard
/// somewhere the editor's handle knows nothing about.
#[gpui::test]
fn a_focused_result_grid_counts_as_focus_inside_the_pane(cx: &mut TestAppContext) {
    let (connected, profile) = h2("grid-focus", &["create table G (ID int primary key)"]);
    let window = pane(&connected, &profile, 500, cx);
    run(&window, "select ID from G", cx);

    window
        .update(cx, |pane, window, cx| {
            // Nothing is focused yet, so neither answer can be true by
            // accident.
            assert!(!pane.contains_focus(window, cx));

            // What clicking a cell amounts to, without the mouse.
            let handle = pane.grid_at(0).read(cx).focus_handle(cx);
            handle.focus(window, cx);
            assert!(
                !pane.focus_handle(cx).contains_focused(window, cx),
                "the editor's handle answered for a focus that is not inside it"
            );
            assert!(
                pane.contains_focus(window, cx),
                "a focus in the grid would strand when the tab stops rendering"
            );

            pane.focus_editor(window, cx);
            assert!(pane.contains_focus(window, cx));
        })
        .expect("the window is open");
    connected.close().expect("close");
}

/// Closing the connection tab leaves the SQL and the rows where they are
/// and takes the session away, which every path that would use one has to
/// notice rather than reach for a handle that is gone.
#[gpui::test]
fn a_detached_pane_keeps_its_rows_and_refuses_to_run(cx: &mut TestAppContext) {
    let (connected, profile) = h2(
        "detached",
        &[
            "create table D (ID int primary key)",
            "insert into D values (1), (2), (3)",
        ],
    );
    let window = pane(&connected, &profile, 500, cx);
    run(&window, "select ID from D order by ID", cx);
    assert_eq!(column_zero(&window, 0, cx), ["1", "2", "3"]);

    window
        .update(cx, |pane, _window, cx| pane.detach(cx))
        .expect("the window is open");

    // The rows the user already has are theirs to read; the status bar is
    // what says they are a snapshot of a connection that has gone.
    assert_eq!(column_zero(&window, 0, cx), ["1", "2", "3"]);
    window
        .update(cx, |pane, _window, _cx| {
            assert!(!pane.is_attached());
            assert_eq!(pane.status_cells().0, ts!("statusbar.disconnected"));
        })
        .expect("the window is open");

    // Every way in refuses with the same wording: a run, a sort round trip,
    // and a page the grid asks for as it nears the last row it holds.
    run(&window, "select ID from D", cx);
    window
        .update(cx, |pane, dead, cx| {
            assert!(!pane.is_running(), "a detached pane sent a statement");
            assert_eq!(pane.notice, Some(ts!("explorer.disconnected")));
            assert_eq!(
                pane.results.len(),
                2,
                "the results of the last live run were replaced"
            );

            pane.notice = None;
            pane.reorder(
                pane.row_result(0).id,
                0,
                Some(SortDirection::Descending),
                dead,
                cx,
            );
            assert_eq!(pane.notice, Some(ts!("explorer.disconnected")));

            pane.notice = None;
            pane.fetch_more(pane.row_result(0).id, dead, cx);
            assert_eq!(pane.notice, Some(ts!("explorer.disconnected")));
        })
        .expect("the window is open");
    cx.run_until_parked();

    // The rows are still the ones the live run produced.
    assert_eq!(column_zero(&window, 0, cx), ["1", "2", "3"]);
    connected.close().expect("close");
}

/// How result `index` would read on, as the assertions below need it: the
/// offset of its next page, `Err(())` for a cursor left open, `Ok(None)` for
/// none.
fn way_on(
    window: &WindowHandle<QueryPane>,
    index: usize,
    cx: &mut TestAppContext,
) -> Result<Option<u64>, ()> {
    window
        .update(cx, |pane, _window, _cx| {
            let ResultBody::Rows(rows) = &pane.row_result(index).body else {
                panic!("result {index} is the execution log");
            };
            assert!(!rows.fetching, "a page is still out while idle");
            match &rows.more {
                None => Ok(None),
                Some(More::Offset { next, .. }) => Ok(Some(*next)),
                Some(More::Cursor(_)) => Err(()),
            }
        })
        .expect("the window is open")
}

/// Scrolls result 0 to its end the way `GridEvent::NearEnd` would.
///
/// Between pages — whenever the pane is idle — nothing may be held open: the
/// way on is the offset of the next page, and that offset is exactly the rows
/// already on screen. A cursor there would be a result set left standing on
/// the shared connection, which is what reading a page at a time exists to
/// avoid.
fn scroll_paged_to_end(window: &WindowHandle<QueryPane>, cx: &mut TestAppContext) {
    let id = window
        .update(cx, |pane, _window, _cx| pane.row_result(0).id)
        .expect("the window is open");
    let mut pages = 0;
    loop {
        let rows = window
            .update(cx, |pane, _window, cx| {
                assert!(pane.error.is_none(), "{:?}", pane.error);
                pane.grid_at(0).read(cx).source().row_count() as u64
            })
            .expect("the window is open");
        match way_on(window, 0, cx) {
            Ok(Some(next)) => assert_eq!(next, rows, "the next page skips or repeats"),
            Ok(None) => break,
            Err(()) => panic!("a cursor was left open between pages"),
        }
        window
            .update(cx, |pane, window, cx| pane.fetch_more(id, window, cx))
            .expect("the window is open");
        cx.run_until_parked();
        pages += 1;
        assert!(pages < 100, "the source never reached the end");
    }
    assert_eq!(state(window, 0, cx), GridSourceState::Complete);
}

/// Asserts that result 0 is being read a page at a time and is not at its
/// end yet: some whole pages of `limit` rows on screen, and the next page due
/// right after them.
///
/// Some, not one: the window's own first frames lay the grid out and may ask
/// for a page or two before the test looks.
fn assert_part_paged(window: &WindowHandle<QueryPane>, limit: u64, cx: &mut TestAppContext) {
    let rows = window
        .update(cx, |pane, _window, cx| {
            assert!(pane.error.is_none(), "{:?}", pane.error);
            pane.grid_at(0).read(cx).source().row_count() as u64
        })
        .expect("the window is open");
    assert!(rows > 0 && rows.is_multiple_of(limit), "{rows} rows");
    assert_eq!(way_on(window, 0, cx), Ok(Some(rows)), "not read by page");
}

/// A `SELECT` is read a page at a time, each page a statement of its own:
/// nothing is left open between pages, the pages line up end to end, and the
/// execution log still names the statement once.
#[gpui::test]
fn a_select_is_read_a_page_at_a_time_with_nothing_left_open(cx: &mut TestAppContext) {
    let (connected, profile) = h2(
        "paged-select",
        &[
            "create table BIG (ID int primary key, NAME varchar(20))",
            // Inserted out of key order, so the order on screen is the
            // statement's and not an accident of how the rows were stored.
            "insert into BIG select 2501 - X, 'row' || X from system_range(1, 2500)",
        ],
    );
    let window = pane(&connected, &profile, 100, cx);
    run(
        &window,
        "select ID, NAME from BIG order by ID desc; -- newest first",
        cx,
    );

    assert_part_paged(&window, 100, cx);

    // The primary key lookup that decides editability went out on the same
    // session right after the first page — the very command that made
    // MariaDB's driver drain an open result — and the result is editable.
    assert!(writable(&window, 0, cx), "the edit gate did not open");

    scroll_paged_to_end(&window, cx);
    let rows = column_zero(&window, 0, cx);
    let expected: Vec<String> = (1..=2_500).rev().map(|id| id.to_string()).collect();
    assert_eq!(rows, expected);
    window
        .update(cx, |pane, _window, _cx| {
            assert_eq!(pane.log_at(0).len(), 1, "paging logged the statement again");
        })
        .expect("the window is open");
    connected.close().expect("close");
}

/// A result an exact number of pages long ends on an empty page rather than
/// asking forever.
#[gpui::test]
fn a_result_of_whole_pages_ends(cx: &mut TestAppContext) {
    let (connected, profile) = h2(
        "paged-whole",
        &[
            "create table EVEN (ID int primary key)",
            "insert into EVEN select X from system_range(1, 300)",
        ],
    );
    let window = pane(&connected, &profile, 100, cx);
    run(&window, "select ID from EVEN order by ID", cx);
    scroll_paged_to_end(&window, cx);
    let rows = column_zero(&window, 0, cx);
    assert_eq!(rows.len(), 300);
    assert_eq!(rows.last().map(String::as_str), Some("300"));

    // And one shorter than a page is complete at once, with no way on.
    run(&window, "select ID from EVEN where ID <= 42", cx);
    assert_eq!(way_on(&window, 0, cx), Ok(None));
    assert_eq!(state(&window, 0, cx), GridSourceState::Complete);
    assert_eq!(column_zero(&window, 0, cx).len(), 42);
    connected.close().expect("close");
}

/// The page clause goes on the end of the user's statement rather than round
/// it, so a join with two columns of one name — which a derived table refuses
/// — pages like anything else.
#[gpui::test]
fn a_join_with_repeated_column_names_pages(cx: &mut TestAppContext) {
    let (connected, profile) = h2(
        "paged-join",
        &[
            "create table A (ID int primary key, V int)",
            "create table B (ID int primary key, V int)",
            "insert into A select X, X from system_range(1, 250)",
            "insert into B select X, -X from system_range(1, 250)",
        ],
    );
    let window = pane(&connected, &profile, 100, cx);
    run(
        &window,
        "select * from A join B on A.ID = B.ID order by A.ID",
        cx,
    );
    assert_part_paged(&window, 100, cx);
    scroll_paged_to_end(&window, cx);
    let rows = column_zero(&window, 0, cx);
    assert_eq!(rows.len(), 250);
    assert_eq!(rows[249], "250");
    connected.close().expect("close");
}

/// A sort round trip pages too: its wrapper is a plain read, and the order it
/// asks for holds across every page.
#[gpui::test]
fn a_sorted_result_pages_in_the_new_order(cx: &mut TestAppContext) {
    let (connected, profile) = h2(
        "paged-sort",
        &[
            "create table S (ID int primary key)",
            "insert into S select X from system_range(1, 250)",
        ],
    );
    let window = pane(&connected, &profile, 100, cx);
    run(&window, "select ID from S order by ID", cx);
    let id = window
        .update(cx, |pane, _window, _cx| pane.row_result(0).id)
        .expect("the window is open");
    window
        .update(cx, |pane, sorted, cx| {
            pane.reorder(id, 0, Some(SortDirection::Descending), sorted, cx);
        })
        .expect("the window is open");
    cx.run_until_parked();
    assert_part_paged(&window, 100, cx);
    scroll_paged_to_end(&window, cx);
    let expected: Vec<String> = (1..=250).rev().map(|id| id.to_string()).collect();
    assert_eq!(column_zero(&window, 0, cx), expected);
    connected.close().expect("close");
}

/// A statement that cuts its own rows is the user's to cut: it keeps its own
/// cursor, which pages by `FETCH` exactly as before, and its `LIMIT` holds.
#[gpui::test]
fn a_statement_with_its_own_limit_keeps_its_cursor(cx: &mut TestAppContext) {
    let (connected, profile) = h2(
        "cursor-limit",
        &[
            "create table L (ID int primary key)",
            "insert into L select X from system_range(1, 1000)",
        ],
    );
    let window = pane(&connected, &profile, 100, cx);
    run(&window, "select ID from L order by ID limit 250", cx);
    assert_eq!(way_on(&window, 0, cx), Err(()), "expected the cursor");

    let id = window
        .update(cx, |pane, _window, _cx| pane.row_result(0).id)
        .expect("the window is open");
    let mut pages = 0;
    while state(&window, 0, cx) != GridSourceState::Complete {
        window
            .update(cx, |pane, window, cx| pane.fetch_more(id, window, cx))
            .expect("the window is open");
        cx.run_until_parked();
        pages += 1;
        assert!(pages < 20, "the source never reached the end");
    }
    let rows = column_zero(&window, 0, cx);
    assert_eq!(rows.len(), 250);
    assert_eq!(rows[249], "250");
    assert_eq!(way_on(&window, 0, cx), Ok(None));
    connected.close().expect("close");
}

/// Under manual commit the pages share the user's transaction: a row the
/// transaction inserted and has not committed is on the page that reaches it.
#[gpui::test]
fn pages_read_inside_the_users_transaction(cx: &mut TestAppContext) {
    let (connected, profile) = h2(
        "paged-transaction",
        &[
            "create table TX (ID int primary key)",
            "insert into TX select X from system_range(1, 150)",
        ],
    );
    connected
        .session()
        .set_auto_commit(false)
        .expect("manual commit");
    let window = pane(&connected, &profile, 100, cx);
    run(&window, "insert into TX values (151)", cx);
    run(&window, "select ID from TX order by ID", cx);
    scroll_paged_to_end(&window, cx);
    let rows = column_zero(&window, 0, cx);
    assert_eq!(rows.len(), 151);
    assert_eq!(rows[150], "151");
    connected.session().rollback().expect("rollback");
    connected.close().expect("close");
}
