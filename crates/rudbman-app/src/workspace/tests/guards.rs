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
