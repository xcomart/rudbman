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
        1
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
            assert!(pane.can_commit());
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
            assert!(pane.can_commit());
        })
        .unwrap();
    connected.session().rollback().unwrap();
    assert_eq!(server_rows(&connected, "select id from entries"), vec!["1"]);
    connected.close().expect("close");
}

#[gpui::test]
fn toolbar_commit_is_disabled_when_auto_commit_is_enabled(cx: &mut TestAppContext) {
    let (connected, profile) = h2("toolbar-auto-commit", &[]);
    assert!(profile.auto_commit);
    let window = pane(&connected, &profile, 500, cx);
    let mut cx = gpui::VisualTestContext::from_window(window.into(), cx);
    click("query-commit", &mut cx);
    window
        .update(&mut cx, |pane, _, _| {
            assert!(!pane.can_commit());
            assert!(
                pane.notice.is_none(),
                "automatic mode must not send a manual commit"
            );
        })
        .unwrap();
    connected.close().expect("close");
}

#[gpui::test]
fn a_commit_blocks_new_queries_until_it_finishes(cx: &mut TestAppContext) {
    let (connected, mut profile) = h2("toolbar-commit-busy", &[]);
    profile.auto_commit = false;
    connected.session().set_auto_commit(false).unwrap();
    let window = pane(&connected, &profile, 500, cx);
    window
        .update(cx, |pane, window, cx| {
            pane.commit(cx);
            assert!(pane.committing);
            assert!(!pane.can_commit());
            pane.request(vec!["select 1".into()], window, cx);
            assert!(!pane.is_running());
            assert!(pane.results.is_empty());
        })
        .unwrap();
    cx.run_until_parked();
    window
        .update(cx, |pane, _, _| {
            assert!(!pane.committing);
            assert!(pane.can_commit());
        })
        .unwrap();
    connected.close().expect("close");
}
