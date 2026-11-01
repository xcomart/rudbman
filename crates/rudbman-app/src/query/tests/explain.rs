use super::*;

/// Puts `text` in the editor with the caret at its end, and asks for the plan
/// of the statement under it.
fn explain_text(window: &WindowHandle<QueryPane>, text: &str, cx: &mut TestAppContext) {
    window
        .update(cx, |pane, window, cx| {
            pane.editor.update(cx, |editor, cx| {
                editor.set_text(text, cx);
                editor.select_range(text.len()..text.len(), cx);
            });
            pane.explain(window, cx);
        })
        .expect("the window is open");
    cx.run_until_parked();
}

/// The plan of the statement under the caret comes back as a read-only grid
/// titled as a plan, the log says the statement was not run — and it was not:
/// the rows a `DELETE` would have taken are still there.
#[gpui::test]
fn a_plan_is_read_and_the_statement_is_not_run(cx: &mut TestAppContext) {
    let (connected, profile) = h2(
        "explain",
        &[
            "create table T (ID int primary key, N varchar(10))",
            "insert into T values (1, 'a'), (2, 'b')",
        ],
    );
    let window = pane(&connected, &profile, 500, cx);
    explain_text(&window, "select 1;\ndelete from T where ID = 1", cx);

    window
        .update(cx, |pane, _window, cx| {
            assert!(pane.error.is_none(), "{:?}", pane.error);
            assert_eq!(pane.results[0].label, ts!("query.result_log"));
            assert_eq!(pane.results[1].label, ts!("query.plan", index = 1));
            let log = pane.log_at(0);
            assert_eq!(log.len(), 1);
            assert_eq!(log[0].message, ts!("query.explained"));
            assert!(
                log[0].sql.starts_with("EXPLAIN delete from T"),
                "{}",
                log[0].sql
            );
            let ResultBody::Rows(rows) = &pane.row_result(0).body else {
                panic!("the plan is not a grid");
            };
            assert!(rows.plan);
            assert!(rows.read_only.is_none(), "a plan needs no read-only line");
            assert!(!rows.grid.read(cx).source().writable());
            assert!(rows.grid.read(cx).source().row_count() > 0);
        })
        .expect("the window is open");

    run(&window, "select count(*) from T", cx);
    assert_eq!(column_zero(&window, 0, cx), ["2"], "the DELETE ran");
    connected.close().expect("close");
}

/// A plan is not a write, so neither of the profile's write guards stands in
/// its way: a read-only profile explains a `DELETE`, and one that asks before
/// writes does not ask.
#[gpui::test]
fn the_write_guards_do_not_apply_to_a_plan(cx: &mut TestAppContext) {
    let (connected, mut profile) = h2("explain-guards", &["create table W (ID int primary key)"]);
    profile.read_only = true;
    profile.confirm_writes = true;
    let window = pane(&connected, &profile, 500, cx);
    explain_text(&window, "delete from W", cx);
    window
        .update(cx, |pane, _window, _cx| {
            assert!(pane.pending.is_none(), "a plan was asked about");
            assert!(pane.error.is_none(), "{:?}", pane.error);
            assert_eq!(pane.results[1].label, ts!("query.plan", index = 1));
        })
        .expect("the window is open");
    connected.close().expect("close");
}

/// A selection of two statements is refused rather than guessed at, and the
/// selection is what is explained when it holds one.
#[gpui::test]
fn a_selection_is_explained_when_it_is_one_statement(cx: &mut TestAppContext) {
    let (connected, profile) = h2("explain-selection", &["create table S (ID int)"]);
    let window = pane(&connected, &profile, 500, cx);
    window
        .update(cx, |pane, window, cx| {
            pane.editor.update(cx, |editor, cx| {
                editor.set_text("select ID from S; select 2", cx);
                editor.select_range(0..26, cx);
            });
            pane.explain(window, cx);
            assert_eq!(
                pane.notice.as_ref().map(|notice| notice.text.clone()),
                Some(ts!("query.explain_one"))
            );
            assert!(!pane.ran);

            pane.editor
                .update(cx, |editor, cx| editor.select_range(0..16, cx));
            pane.explain(window, cx);
        })
        .expect("the window is open");
    cx.run_until_parked();
    window
        .update(cx, |pane, _window, _cx| {
            assert!(pane.error.is_none(), "{:?}", pane.error);
            assert_eq!(pane.log_at(0)[0].sql, "EXPLAIN select ID from S");
        })
        .expect("the window is open");
    connected.close().expect("close");
}

/// A plan's rows are in the order the plan reads, and wrapping an `EXPLAIN`
/// in a derived table is a syntax error, so a header click starts nothing.
#[gpui::test]
fn a_plan_is_not_sorted(cx: &mut TestAppContext) {
    let (connected, profile) = h2("explain-sort", &["create table P (ID int)"]);
    let window = pane(&connected, &profile, 500, cx);
    explain_text(&window, "select ID from P", cx);
    let id = first_result(&window, cx);
    window
        .update(cx, |pane, window, cx| {
            let generation = pane.generation;
            pane.reorder(id, 0, Some(SortDirection::Ascending), window, cx);
            assert_eq!(pane.generation, generation, "the sort started a run");
            assert!(!pane.is_running());
        })
        .expect("the window is open");
    connected.close().expect("close");
}

/// Db2 is not asked at all, and says so.
#[gpui::test]
fn a_product_without_a_plan_statement_says_so(cx: &mut TestAppContext) {
    let (connected, profile) = h2("explain-db2", &[]);
    cx.update(|cx| {
        app_settings::init(cx);
        rugpui::init(cx);
        rugpui_editor::init(cx);
        rugpui_grid::init(cx);
    });
    let session = connected.handle();
    let window = cx.add_window(move |window, cx| {
        QueryPane::new(
            session,
            ConnectionId(1),
            &profile,
            "db2",
            &AppSettings::default(),
            "select 1 from sysibm.sysdummy1",
            window,
            cx,
        )
    });
    window
        .update(cx, |pane, window, cx| {
            pane.explain(window, cx);
            assert_eq!(
                pane.notice.as_ref().map(|notice| notice.text.clone()),
                Some(ts!("query.explain_unsupported", product = "db2"))
            );
            assert!(!pane.ran);
        })
        .expect("the window is open");
    connected.close().expect("close");
}

/// The editor's menu offers the plan beside the run rows, with its chord.
#[gpui::test]
fn the_editor_menu_offers_the_plan(cx: &mut TestAppContext) {
    let (connected, profile) = h2("explain-menu", &[]);
    let window = pane(&connected, &profile, 500, cx);
    let rows = menu_rows(
        &window,
        PaneMenu::Editor {
            position: anywhere(),
        },
        cx,
    );
    assert!(context_menu::row(&rows, &ts!("context.explain")).is_enabled());
    connected.close().expect("close");
}
