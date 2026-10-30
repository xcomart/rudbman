//! The guards between a gesture and the work it would throw away: shortcuts
//! held back behind a dialog, and a running job that nothing but its own card
//! may stop.

use super::*;

/// How many tabs the pane on screen holds.
fn tab_count(workspace: &Workspace) -> usize {
    area(workspace)
        .panes
        .get(active_pane(workspace))
        .expect("the pane is in the tree")
        .items()
        .len()
}

/// A chord pressed inside a dialog walks up to the workspace's key context,
/// and the workspace answers none of them until the dialog is gone.
#[gpui::test]
fn a_shortcut_inside_a_dialog_leaves_the_window_behind_it_alone(cx: &mut gpui::TestAppContext) {
    let (window, _) = workspace_over_h2("guards-held", cx);
    let mut cx = gpui::VisualTestContext::from_window(window.into(), cx);

    window
        .update(&mut cx, |workspace, window, cx| {
            workspace.open_query("", window, cx);
            workspace.open_settings(window, cx);
            assert!(workspace.settings.read(cx).is_open());
        })
        .expect("the window is open");
    cx.run_until_parked();

    cx.dispatch_action(NewQuery);
    cx.dispatch_action(SplitRight);
    cx.dispatch_action(NewConnection);
    cx.run_until_parked();
    window
        .update(&mut cx, |workspace, _window, cx| {
            assert_eq!(
                tab_count(workspace),
                1,
                "no query tab opened behind the dialog"
            );
            assert_eq!(area(workspace).panes.leaf_count(), 1, "nothing was split");
            assert!(
                workspace.settings.read(cx).is_open() && !workspace.connect.read(cx).is_open(),
                "Ctrl+N did not swap the dialog for another"
            );
        })
        .expect("the window is open");

    // With the dialog gone, the same chord does what it always did.
    window
        .update(&mut cx, |workspace, window, cx| {
            workspace.settings.update(cx, |dialog, cx| dialog.close(cx));
            workspace.focus_shell(window, cx);
        })
        .expect("the window is open");
    cx.run_until_parked();
    cx.dispatch_action(NewQuery);
    cx.run_until_parked();
    window
        .update(&mut cx, |workspace, _window, _cx| {
            assert_eq!(tab_count(workspace), 2);
        })
        .expect("the window is open");
}

/// A transfer that is running stays on screen through anything that would
/// open another dialog over it, and the job under it goes on.
#[gpui::test]
fn a_running_transfer_outlasts_every_other_dialog(cx: &mut gpui::TestAppContext) {
    // A view that sleeps per row, for the transfer dialog's own reason: a
    // job that cannot finish inside the test.
    let (window, source) = workspace_over_h2_with(
        "guards-running-source",
        &[
            "CREATE ALIAS NAP AS 'long nap(long ms) throws Exception { \
                 Thread.sleep(ms); return ms; }'",
            "CREATE VIEW BIG AS SELECT X AS ID, NAP(1) AS PAUSE FROM SYSTEM_RANGE(1, 400000)",
            "CREATE TABLE BIG_COPY (ID BIGINT, PAUSE BIGINT)",
        ],
        cx,
    );
    let mut cx = gpui::VisualTestContext::from_window(window.into(), cx);

    window
        .update(&mut cx, |workspace, window, cx| {
            workspace.open_transfer(object(source, "BIG"), window, cx);
            workspace.transfer.update(cx, |dialog, cx| {
                // The source's own connection, into the other table: the one
                // pairing a lone connection allows.
                dialog.point_at_for_test(0, "BIG_COPY", cx);
                dialog.press_start(cx);
            });
        })
        .expect("the window is open");
    cx.run_until_parked();

    window
        .update(&mut cx, |workspace, window, cx| {
            assert!(workspace.transfer.read(cx).is_running(), "the job started");
            assert!(
                !workspace.close_overlays(window, cx),
                "the screen is not clear"
            );
            workspace.open_settings(window, cx);
            workspace.open_connect(window, cx);
            assert!(!workspace.settings.read(cx).is_open());
            assert!(!workspace.connect.read(cx).is_open());
            assert!(
                workspace.transfer.read(cx).is_running(),
                "the job is still running under its card"
            );
            workspace.transfer.update(cx, |dialog, cx| dialog.close(cx));
        })
        .expect("the window is open");
}

/// The data pane on top of the active pane, which the staging tests write
/// into.
fn active_data(workspace: &Workspace) -> Entity<DataPane> {
    match area(workspace)
        .panes
        .get(active_pane(workspace))
        .and_then(Pane::active)
    {
        Some(PaneItem::TableData(panel)) => panel.clone(),
        other => panic!("the rows are not the tab on top: {other:?}"),
    }
}

/// Staged rows are refused at every door, not only at the tab's own: the
/// pane holding them and the connection holding that pane stay open, and the
/// tab with the rows comes forward to say why.
#[gpui::test]
fn staged_rows_hold_their_pane_and_their_connection_open(cx: &mut gpui::TestAppContext) {
    let (window, id) = workspace_over_h2_with(
        "guards-staged",
        &[
            "create table PERSON (ID int primary key, NAME varchar(20))",
            "insert into PERSON values (1, 'a'), (2, 'b')",
        ],
        cx,
    );
    let mut cx = gpui::VisualTestContext::from_window(window.into(), cx);

    let right = window
        .update(&mut cx, |workspace, window, cx| {
            workspace.open_query("select 1", window, cx);
            workspace.split_active(Axis::Horizontal, window, cx);
            let right = active_pane(workspace);
            workspace.open_data(object(id, "PERSON"), window, cx);
            right
        })
        .expect("the window is open");
    cx.run_until_parked();
    window
        .update(&mut cx, |workspace, window, cx| {
            let data = active_data(workspace);
            data.update(cx, |pane, cx| pane.null_for_test(0, 1, cx));
            assert!(data.read(cx).has_pending_edits(cx));
            // Somewhere else first, so the refusal has a tab to bring back.
            workspace.activate_tab(right, 0, window, cx);

            workspace.close_pane(right, window, cx);
            assert_eq!(area(workspace).panes.leaf_count(), 2, "the pane stayed");
            assert!(matches!(
                area(workspace).panes.get(right).and_then(Pane::active),
                Some(PaneItem::TableData(_))
            ));

            workspace.close_connection(0, window, cx);
            assert_eq!(workspace.connections.len(), 1, "the connection stayed");
            assert!(workspace.close_question.is_none(), "refused, not asked");
        })
        .expect("the window is open");
}

/// A query tab bound to a file with changes the file does not have asks
/// before it goes, at the tab, at the pane and at the connection, and every
/// answer does what it says; an unbound tab goes unasked as it always did.
#[gpui::test]
fn a_file_with_changes_asks_save_dont_save_or_cancel(cx: &mut gpui::TestAppContext) {
    let (window, _) = workspace_over_h2("guards-unsaved", cx);
    let mut cx = gpui::VisualTestContext::from_window(window.into(), cx);
    let dir = tempfile::tempdir().expect("a temporary directory");
    let path = dir.path().join("report.sql");
    std::fs::write(&path, "select 1").expect("the fixture is written");

    // An unbound draft closes with no question.
    window
        .update(&mut cx, |workspace, window, cx| {
            workspace.open_query("select 'draft'", window, cx);
            let pane = active_pane(workspace);
            workspace.close_tab(pane, 0, window, cx);
            assert!(workspace.close_question.is_none());
            assert_eq!(tab_count(workspace), 0);
            workspace.load_sql_file(path.clone(), window, cx);
        })
        .expect("the window is open");
    cx.run_until_parked();
    cx.simulate_keystrokes(DOCUMENT_END);
    cx.simulate_input(" -- edited");

    let pane = window
        .update(&mut cx, |workspace, window, cx| {
            let pane = active_pane(workspace);
            // Cancel: nothing closes.
            workspace.close_tab(pane, 0, window, cx);
            assert!(matches!(
                workspace
                    .close_question
                    .as_ref()
                    .map(|question| &question.ask),
                Some(closing::CloseAsk::Unsaved { .. })
            ));
            pane
        })
        .expect("the window is open");
    // Drawn, as each kind of question is in these tests.
    cx.run_until_parked();
    window
        .update(&mut cx, |workspace, window, cx| {
            workspace.answer_close(closing::CloseAnswer::Cancel, window, cx);
            assert_eq!(tab_count(workspace), 1);
            // Ctrl+W asks too, and so does the connection's own close.
            workspace.close_tab_action(&CloseTab, window, cx);
            assert!(workspace.close_question.is_some());
            workspace.answer_close(closing::CloseAnswer::Cancel, window, cx);
            workspace.close_connection(0, window, cx);
            assert!(workspace.close_question.is_some());
            workspace.answer_close(closing::CloseAnswer::Cancel, window, cx);
            assert_eq!(workspace.connections.len(), 1);
            // Save: the file is written, then the tab goes.
            workspace.close_tab(pane, 0, window, cx);
            workspace.answer_close(closing::CloseAnswer::Save, window, cx);
        })
        .expect("the window is open");
    cx.run_until_parked();
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        "select 1 -- edited"
    );
    window
        .update(&mut cx, |workspace, window, cx| {
            assert!(workspace.close_question.is_none());
            assert_eq!(tab_count(workspace), 0, "saved, then closed");
            // Reopened, it is the file it was — and clean, since it was saved.
            workspace.reopen_closed_tab(window, cx);
            let query = workspace.active_query().expect("it came back").clone();
            assert_eq!(query.read(cx).file(), Some(path.as_path()));
            assert!(!query.read(cx).is_modified(cx));
        })
        .expect("the window is open");
    cx.run_until_parked();

    // Don't save, from a pane close: the pane goes, the file keeps what it had,
    // and "Reopen closed tab" brings the changes back with their mark.
    cx.simulate_keystrokes(DOCUMENT_END);
    cx.simulate_input(" -- again");
    window
        .update(&mut cx, |workspace, window, cx| {
            workspace.split_active(Axis::Horizontal, window, cx);
            let left = area(workspace).panes.first_leaf().0;
            workspace.close_pane(left, window, cx);
            assert!(workspace.close_question.is_some(), "the pane asked");
            workspace.answer_close(closing::CloseAnswer::Discard, window, cx);
            assert_eq!(area(workspace).panes.leaf_count(), 1, "the pane went");
            assert!(workspace.close_question.is_none());
            // The copy the split made is still open and still marked; the
            // one that was closed comes back beside it, marked too.
            workspace.reopen_closed_tab(window, cx);
            let query = workspace.active_query().expect("it came back").clone();
            assert!(query.read(cx).is_modified(cx));
            assert_eq!(
                query.read(cx).editor_text(cx),
                "select 1 -- edited -- again"
            );
        })
        .expect("the window is open");
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        "select 1 -- edited"
    );
}

/// Closing the tab of a connection with a transaction open asks, saying the
/// transaction will be rolled back; "Cancel" keeps everything as it was.
#[gpui::test]
fn closing_a_connection_with_an_open_transaction_asks(cx: &mut gpui::TestAppContext) {
    let (window, _) = workspace_over_h2("guards-transaction", cx);
    let mut cx = gpui::VisualTestContext::from_window(window.into(), cx);

    window
        .update(&mut cx, |workspace, window, cx| {
            let ConnectionState::Open(connected) = &workspace.connections[0].state else {
                panic!("the connection is open");
            };
            // What a write under manual commit leaves behind.
            assert!(workspace.transaction_cell().is_none());
            connected.handle().mark_uncommitted_by("Query 7");
            // Said in the status bar, naming the tab to commit from.
            let (label, tip) = workspace.transaction_cell().expect("the cell shows");
            assert_eq!(label, ts!("statusbar.uncommitted"));
            assert_eq!(tip, ts!("statusbar.uncommitted_in", tab = "Query 7"));

            workspace.close_connection(0, window, cx);
            match workspace
                .close_question
                .as_ref()
                .map(|question| &question.ask)
            {
                Some(closing::CloseAsk::Connection { reasons, .. }) => {
                    assert_eq!(reasons, &[ts!("closing.risk_uncommitted")]);
                }
                _ => panic!("the close did not ask about the transaction"),
            }
        })
        .expect("the window is open");
    cx.run_until_parked();
    window
        .update(&mut cx, |workspace, window, cx| {
            workspace.answer_close(closing::CloseAnswer::Cancel, window, cx);
            assert_eq!(workspace.connections.len(), 1);

            // The quit list says the same, in its own words.
            let lines = workspace.unsaved_work(cx);
            assert_eq!(lines.len(), 1);
            workspace.quit_action(&Quit, window, cx);
            assert!(matches!(
                workspace
                    .close_question
                    .as_ref()
                    .map(|question| &question.ask),
                Some(closing::CloseAsk::Quit { .. })
            ));
        })
        .expect("the window is open");
    cx.run_until_parked();
    window
        .update(&mut cx, |workspace, window, cx| {
            workspace.answer_close(closing::CloseAnswer::Cancel, window, cx);

            workspace.close_connection(0, window, cx);
            workspace.answer_close(closing::CloseAnswer::Discard, window, cx);
            assert!(workspace.connections.is_empty(), "closed once accepted");
            assert!(workspace.unsaved_work(cx).is_empty());
        })
        .expect("the window is open");
}

/// "Reopen closed tab" brings back the last query tab closed — its text, its
/// name and its number — and the next new tab carries on counting.
#[gpui::test]
fn a_closed_query_tab_comes_back_as_it_went(cx: &mut gpui::TestAppContext) {
    let (window, _) = workspace_over_h2("guards-reopen", cx);
    let mut cx = gpui::VisualTestContext::from_window(window.into(), cx);

    window
        .update(&mut cx, |workspace, window, cx| {
            workspace.open_query("select 1", window, cx);
            workspace.open_query("select 42", window, cx);
            let tab = workspace.active_query().unwrap().entity_id();
            workspace
                .work_area_mut()
                .unwrap()
                .tab_names
                .insert(tab, "mine".into());
            workspace.close_tab_action(&CloseTab, window, cx);
            assert_eq!(tab_count(workspace), 1);

            workspace.reopen_closed_tab_action(&ReopenClosedTab, window, cx);
            assert_eq!(tab_count(workspace), 2);
            let query = workspace.active_query().unwrap().clone();
            assert_eq!(query.read(cx).editor_text(cx), "select 42");
            let pane = active_pane(workspace);
            assert_eq!(
                area(workspace)
                    .tab_names
                    .get(&query.entity_id())
                    .map(String::as_str),
                Some("mine")
            );
            assert_eq!(
                tab_titles(workspace, pane, cx)[1],
                ts!("query.tab", index = 2).to_string()
            );
            assert!(area(workspace).closed.is_empty(), "reopened once");

            workspace.open_query("", window, cx);
            assert_eq!(
                tab_titles(workspace, pane, cx)[2],
                ts!("query.tab", index = 3).to_string(),
                "the count carried on past the reopened tab"
            );
        })
        .expect("the window is open");
}
