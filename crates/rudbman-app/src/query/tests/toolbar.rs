use super::*;

fn click(id: &'static str, cx: &mut gpui::VisualTestContext) {
    cx.run_until_parked();
    let bounds = cx.debug_bounds(id).expect("the toolbar button is visible");
    cx.simulate_click(bounds.center(), gpui::Modifiers::none());
    cx.run_until_parked();
}

#[gpui::test]
fn toolbar_edits_keep_the_selection_and_refresh_undo_redo(cx: &mut TestAppContext) {
    let (connected, profile) = h2("toolbar-edit", &[]);
    let window = pane(&connected, &profile, 500, cx);
    let mut cx = gpui::VisualTestContext::from_window(window.into(), cx);
    window
        .update(&mut cx, |pane, window, cx| {
            pane.editor
                .update(cx, |editor, cx| editor.set_text("select 1;", cx));
            pane.focus_editor(window, cx);
        })
        .unwrap();

    click("query-select-all", &mut cx);
    click("query-copy", &mut cx);
    assert_eq!(clipboard(&mut cx), "select 1;");
    click("query-cut", &mut cx);
    assert_eq!(
        window
            .update(&mut cx, |pane, _, cx| pane.editor_text(cx))
            .unwrap(),
        ""
    );
    click("query-undo", &mut cx);
    assert_eq!(
        window
            .update(&mut cx, |pane, _, cx| pane.editor_text(cx))
            .unwrap(),
        "select 1;"
    );
    click("query-redo", &mut cx);
    assert_eq!(
        window
            .update(&mut cx, |pane, _, cx| pane.editor_text(cx))
            .unwrap(),
        ""
    );
    click("query-paste", &mut cx);
    click("query-comment", &mut cx);
    assert_eq!(
        window
            .update(&mut cx, |pane, _, cx| pane.editor_text(cx))
            .unwrap(),
        "-- select 1;"
    );
    click("query-comment", &mut cx);
    assert_eq!(
        window
            .update(&mut cx, |pane, _, cx| pane.editor_text(cx))
            .unwrap(),
        "select 1;"
    );
    connected.close().expect("close");
}

#[gpui::test]
fn toolbar_runs_the_selection_script_and_current_statement(cx: &mut TestAppContext) {
    let (connected, profile) = h2("toolbar-run", &[]);
    let window = pane(&connected, &profile, 500, cx);
    let mut cx = gpui::VisualTestContext::from_window(window.into(), cx);
    window
        .update(&mut cx, |pane, window, cx| {
            pane.editor.update(cx, |editor, cx| {
                editor.set_text("select 1; select 2;", cx);
                editor.select_range(10..19, cx);
            });
            pane.focus_editor(window, cx);
        })
        .unwrap();

    click("query-run-selection", &mut cx);
    assert_eq!(column_zero(&window, 0, &mut cx), vec!["2"]);
    click("query-run-all", &mut cx);
    assert_eq!(column_zero(&window, 0, &mut cx), vec!["1"]);
    assert_eq!(column_zero(&window, 1, &mut cx), vec!["2"]);
    window
        .update(&mut cx, |pane, _, cx| {
            pane.editor
                .update(cx, |editor, cx| editor.select_range(0..0, cx));
        })
        .unwrap();
    click("query-run", &mut cx);
    assert_eq!(column_zero(&window, 0, &mut cx), vec!["1"]);
    assert_eq!(
        window
            .update(&mut cx, |pane, _, _| pane.results.len())
            .unwrap(),
        2
    );
    connected.close().expect("close");
}

#[gpui::test]
fn detached_toolbar_disables_execution_but_keeps_text_commands(cx: &mut TestAppContext) {
    let (connected, profile) = h2("toolbar-detach", &[]);
    let window = pane(&connected, &profile, 500, cx);
    let mut cx = gpui::VisualTestContext::from_window(window.into(), cx);
    window
        .update(&mut cx, |pane, _, cx| {
            pane.editor
                .update(cx, |editor, cx| editor.set_text("select 1;", cx));
            pane.detach(cx);
        })
        .unwrap();

    click("query-select-all", &mut cx);
    for id in [
        "query-run",
        "query-run-selection",
        "query-run-all",
        "query-cancel",
        "query-commit",
        "query-rollback",
    ] {
        click(id, &mut cx);
    }
    window
        .update(&mut cx, |pane, _, _| {
            assert!(pane.results.is_empty());
            assert!(
                pane.notice.is_none(),
                "disabled commands must not reach the execution path"
            );
        })
        .unwrap();
    click("query-copy", &mut cx);
    assert_eq!(clipboard(&mut cx), "select 1;");
    connected.close().expect("close");
}

#[gpui::test]
fn toolbar_commit_persists_manual_transactions_and_keeps_the_results(cx: &mut TestAppContext) {
    let (connected, mut profile) = h2("toolbar-commit", &["create table entries (id int)"]);
    profile.auto_commit = false;
    connected.session().set_auto_commit(false).unwrap();
    let window = pane(&connected, &profile, 500, cx);
    run(&window, "insert into entries values (1)", cx);
    let mut cx = gpui::VisualTestContext::from_window(window.into(), cx);
    window
        .update(&mut cx, |pane, _, _| {
            assert!(pane.can_finish_transaction());
            assert_eq!(pane.results.len(), 1);
        })
        .unwrap();

    click("query-commit", &mut cx);
    window
        .update(&mut cx, |pane, _, _| {
            assert_eq!(pane.notice, Some(ts!("query.committed")));
            assert_eq!(
                pane.results.len(),
                1,
                "commit must keep the last query's results"
            );
            assert!(pane.can_finish_transaction());
        })
        .unwrap();
    connected.session().rollback().unwrap();
    assert_eq!(server_rows(&connected, "select id from entries"), vec!["1"]);
    connected.close().expect("close");
}

#[gpui::test]
fn toolbar_transactions_are_disabled_when_auto_commit_is_enabled(cx: &mut TestAppContext) {
    let (connected, profile) = h2("toolbar-auto-commit", &[]);
    assert!(profile.auto_commit);
    let window = pane(&connected, &profile, 500, cx);
    let mut cx = gpui::VisualTestContext::from_window(window.into(), cx);
    click("query-commit", &mut cx);
    click("query-rollback", &mut cx);
    window
        .update(&mut cx, |pane, _, _| {
            assert!(!pane.can_finish_transaction());
            assert!(
                pane.notice.is_none(),
                "automatic mode must not send a manual transaction command"
            );
        })
        .unwrap();
    connected.close().expect("close");
}

#[gpui::test]
fn transactions_block_new_queries_and_each_other_until_they_finish(cx: &mut TestAppContext) {
    let (connected, mut profile) = h2("toolbar-commit-busy", &[]);
    profile.auto_commit = false;
    connected.session().set_auto_commit(false).unwrap();
    let window = pane(&connected, &profile, 500, cx);
    for action in [TransactionAction::Commit, TransactionAction::Rollback] {
        window
            .update(cx, |pane, window, cx| {
                pane.finish_transaction(action, cx);
                assert_eq!(pane.transaction, Some(action));
                assert!(!pane.can_finish_transaction());
                pane.finish_transaction(TransactionAction::Commit, cx);
                pane.finish_transaction(TransactionAction::Rollback, cx);
                assert_eq!(pane.transaction, Some(action));
                pane.request(vec!["select 1".into()], window, cx);
                assert!(!pane.is_running());
                assert!(pane.results.is_empty());
            })
            .unwrap();
        cx.run_until_parked();
        window
            .update(cx, |pane, _, _| {
                assert!(pane.transaction.is_none());
                assert!(pane.can_finish_transaction());
            })
            .unwrap();
    }
    connected.close().expect("close");
}

#[gpui::test]
fn toolbar_rollback_reverts_only_uncommitted_changes(cx: &mut TestAppContext) {
    let (connected, mut profile) = h2(
        "toolbar-rollback",
        &[
            "create table entries (id int)",
            "insert into entries values (0)",
        ],
    );
    profile.auto_commit = false;
    connected.session().set_auto_commit(false).unwrap();
    let window = pane(&connected, &profile, 500, cx);
    run(&window, "insert into entries values (1)", cx);
    assert_eq!(
        server_rows(&connected, "select id from entries order by id"),
        vec!["0", "1"]
    );
    let mut cx = gpui::VisualTestContext::from_window(window.into(), cx);
    click("query-rollback", &mut cx);
    window
        .update(&mut cx, |pane, _, _| {
            assert_eq!(pane.notice, Some(ts!("query.rolled_back")));
            assert!(pane.transaction.is_none());
            assert!(pane.can_finish_transaction());
            assert_eq!(pane.results.len(), 1);
        })
        .unwrap();
    connected.session().commit().unwrap();
    assert_eq!(server_rows(&connected, "select id from entries"), vec!["0"]);
    connected.close().expect("close");
}

#[gpui::test]
fn the_books_follow_writes_commits_and_rollbacks_under_manual_commit(cx: &mut TestAppContext) {
    let (connected, mut profile) = h2("toolbar-uncommitted", &["create table entries (id int)"]);
    profile.auto_commit = false;
    connected.session().set_auto_commit(false).unwrap();
    let handle = connected.handle();
    let window = pane(&connected, &profile, 500, cx);

    run(&window, "select id from entries", cx);
    assert!(!handle.has_uncommitted(), "a read opened a transaction");
    run(&window, "insert into entries values (1)", cx);
    assert!(handle.has_uncommitted());
    run(&window, "commit", cx);
    assert!(!handle.has_uncommitted(), "a typed COMMIT left it open");
    run(&window, "insert into entries values (2)", cx);
    assert!(handle.has_uncommitted());

    let mut cx = gpui::VisualTestContext::from_window(window.into(), cx);
    click("query-rollback", &mut cx);
    assert!(
        !handle.has_uncommitted(),
        "the rollback button left it open"
    );
    assert_eq!(server_rows(&connected, "select id from entries"), vec!["1"]);
    connected.close().expect("close");
}

#[gpui::test]
fn writes_under_autocommit_open_nothing(cx: &mut TestAppContext) {
    let (connected, profile) = h2("toolbar-autocommit", &["create table entries (id int)"]);
    assert!(profile.auto_commit);
    let handle = connected.handle();
    let window = pane(&connected, &profile, 500, cx);
    run(&window, "insert into entries values (1)", cx);
    assert!(!handle.has_uncommitted());
    connected.close().expect("close");
}

#[gpui::test]
fn format_sql_formats_the_selection_or_the_script_as_one_undo_step(cx: &mut TestAppContext) {
    let (connected, profile) = h2("toolbar-format", &[]);
    let window = pane(&connected, &profile, 500, cx);
    let mut cx = gpui::VisualTestContext::from_window(window.into(), cx);
    let text = |cx: &mut gpui::VisualTestContext| {
        window
            .update(cx, |pane, _, cx| pane.editor_text(cx))
            .unwrap()
    };
    let script = "select 1;\nselect a, 'x  y' from t where a = 1 and b = 2;";
    window
        .update(&mut cx, |pane, window, cx| {
            pane.editor.update(cx, |editor, cx| {
                editor.set_text(script, cx);
                editor.select_range(10..script.len(), cx);
            });
            pane.focus_editor(window, cx);
        })
        .unwrap();

    // The selection alone, which stays selected.
    click("query-format", &mut cx);
    let formatted = "SELECT a,\n       'x  y'\n  FROM t\n WHERE a = 1\n   AND b = 2;";
    assert_eq!(text(&mut cx), format!("select 1;\n{formatted}"));
    window
        .update(&mut cx, |pane, _, cx| {
            assert_eq!(pane.editor.read(cx).selection(), 10..10 + formatted.len());
        })
        .unwrap();
    click("query-undo", &mut cx);
    assert_eq!(text(&mut cx), script);

    // Nothing selected: the whole script, through the keyboard chord.
    window
        .update(&mut cx, |pane, _, cx| {
            pane.editor.update(cx, |editor, cx| editor.move_to(0, cx));
        })
        .unwrap();
    cx.dispatch_action(super::super::format::FormatSql);
    cx.run_until_parked();
    assert_eq!(text(&mut cx), format!("SELECT 1;\n\n{formatted}"));
    click("query-undo", &mut cx);
    assert_eq!(text(&mut cx), script);
    connected.close().expect("close");
}
