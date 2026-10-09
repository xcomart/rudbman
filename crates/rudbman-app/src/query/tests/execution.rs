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
    // same table in another catalogue: the whole triple has to agree.
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
        "an empty catalogue or schema was taken for a name"
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

    let id = window
        .update(cx, |pane, _window, cx| {
            assert!(pane.error.is_none(), "{:?}", pane.error);
            assert_eq!(pane.results.len(), 1);
            let source = pane.grid_at(0).read(cx).source();
            assert_eq!(source.row_count(), 1_000, "one batch, not the whole table");
            assert_eq!(source.state(), GridSourceState::HasMore);
            pane.results[0].id
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
        })
        .expect("the window is open");
}

/// A script of three statements: two grids and a row count, in the order
/// they were written.
#[gpui::test]
fn a_script_makes_one_result_per_statement(cx: &mut TestAppContext) {
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
                         select N from T order by ID;",
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
            assert_eq!(pane.results.len(), 3, "two grids and one row count");
            assert_eq!(
                pane.message_at(1),
                &ts!("query.rows_affected", count = 2),
                "the update count is the driver's, not a guess"
            );
        })
        .expect("the window is open");

    assert_eq!(column_zero(&window, 0, cx), ["1", "2", "3"]);
    assert_eq!(column_zero(&window, 2, cx), ["x", "x", "c"]);
}

/// A statement that would never finish, cancelled, and a session that is
/// still usable afterwards.
///
/// The cancel comes from a thread of its own rather than from the button:
/// the test executor runs the blocking `EXECUTE` on the very thread that
/// would otherwise deliver the click, and [`Canceller`] is `Send + Sync`
/// for exactly this reason.
#[gpui::test]
fn a_cancelled_statement_says_so_and_leaves_the_session_working(cx: &mut TestAppContext) {
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
    let reached = stopper.join().expect("the cancelling thread finished");

    window
        .update(cx, |pane, _window, _cx| {
            assert!(reached, "the cancel never reached a running statement");
            let error = pane.error.as_ref().expect("a cancel is a failure here");
            assert!(
                error.is_cancelled(),
                "kind {:?}, sqlstate {:?}: {}",
                error.kind,
                error.sql_state,
                error.message
            );
            assert_eq!(error.hint(), Some(ts!("query.hint_cancelled")));
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
            .update(cx, |pane, _window, _cx| pane.results[0].id)
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
                let ResultBody::Rows(rows) = &pane.results[0].body else {
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
            assert_eq!(pane.results.len(), 1);
            pane.results[0].id
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
            assert_eq!(pane.results.len(), 1, "the old tab went with its run");
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
            assert_eq!(pane.message_at(0), &ts!("query.rows_affected", count = 1));
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
                1,
                "the results of the last live run were replaced"
            );

            pane.notice = None;
            pane.reorder(
                pane.results[0].id,
                0,
                Some(SortDirection::Descending),
                dead,
                cx,
            );
            assert_eq!(pane.notice, Some(ts!("explorer.disconnected")));

            pane.notice = None;
            pane.fetch_more(pane.results[0].id, dead, cx);
            assert_eq!(pane.notice, Some(ts!("explorer.disconnected")));
        })
        .expect("the window is open");
    cx.run_until_parked();

    // The rows are still the ones the live run produced.
    assert_eq!(column_zero(&window, 0, cx), ["1", "2", "3"]);
    connected.close().expect("close");
}
