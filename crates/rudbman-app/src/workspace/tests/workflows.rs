use super::*;

/// M7's own acceptance test: three tables joined in the builder, and the
/// statement it produces run in a query pane that answers with rows.
///
/// Everything up to the joins goes through the action the menu row
/// dispatches, so the path being asserted is the one a user takes. The two
/// joins are injected rather than dragged: the drag itself is the canvas
/// widget's, tested in `rudbman-erd` against real pointer events, and what
/// is at stake here is what the *shell* does with the result.
#[gpui::test]
fn three_tables_joined_in_the_builder_run_as_one_statement(cx: &mut gpui::TestAppContext) {
    let (window, id) = workspace_over_h2("builder-join", cx);
    let mut cx = gpui::VisualTestContext::from_window(window.into(), cx);

    // Outside the update, because the calls block; the same rule the ERD
    // test above follows.
    let session = window
        .update(&mut cx, |workspace, _window, _cx| {
            workspace.session_of(id).expect("the session is live")
        })
        .expect("the window is open");
    for sql in [
        "create schema if not exists BLD",
        "create table BLD.OFFICE (ID int primary key, CITY varchar(40))",
        "create table BLD.TEAM (ID int primary key, NAME varchar(40), OFFICE_ID int not null, \
                 constraint FK_BLD_TEAM_OFFICE foreign key (OFFICE_ID) references BLD.OFFICE(ID))",
        "create table BLD.PERSON (ID int primary key, NAME varchar(40), TEAM_ID int not null, \
                 constraint FK_BLD_PERSON_TEAM foreign key (TEAM_ID) references BLD.TEAM(ID))",
        "insert into BLD.OFFICE values (1, 'Seoul')",
        "insert into BLD.TEAM values (10, 'Core', 1)",
        "insert into BLD.PERSON values (100, 'Ada', 10)",
        "insert into BLD.PERSON values (101, 'Linus', 10)",
    ] {
        session
            .session()
            .execute(&rudbman_jdbc::StatementSpec::new(sql))
            .unwrap_or_else(|error| panic!("{sql}: {error}"));
    }
    drop(session);

    // The keyboard has to be somewhere inside the shell before an action is
    // dispatched: gpui resolves one against the focused element of the last
    // drawn frame, and the window the harness builds — unlike the one
    // `main` opens — starts with the focus nowhere.
    window
        .update(&mut cx, |workspace, window, cx| {
            workspace.focus_shell(window, cx);
        })
        .expect("the window is open");
    cx.run_until_parked();

    // Three tables, each through the explorer selection and the action —
    // the first of which has to open the builder tab as well.
    for name in ["PERSON", "TEAM", "OFFICE"] {
        select_table(&window, &mut cx, id, "BLD", name);
        cx.run_until_parked();
        cx.dispatch_action(AddToBuilder);
        cx.run_until_parked();
    }

    let (pane, panel) = window
        .update(&mut cx, |workspace, _window, cx| {
            let pane = active_pane(workspace);
            let panel = active_builder(workspace);
            // One tab, not three: every table lands on the builder already
            // in front.
            assert_eq!(
                tab_titles(workspace, pane, cx),
                [ts!("builder.tab", index = 1).to_string()]
            );
            assert_eq!(panel.read(cx).table_count(), 3);
            // A builder is not a query, so the status bar's own cells stay
            // empty over one.
            assert!(workspace.active_query().is_none());
            (pane, panel)
        })
        .expect("the window is open");

    // What drawing the two lines and clicking the three rows amounts to.
    window
        .update(&mut cx, |_workspace, _window, cx| {
            panel.update(cx, |panel, cx| {
                panel.add_join((0, 2), (1, 0), cx);
                panel.add_join((1, 2), (2, 0), cx);
                panel.toggle_column(0, 1, cx);
                panel.toggle_column(1, 1, cx);
                panel.toggle_column(2, 1, cx);
            });
        })
        .expect("the window is open");
    cx.run_until_parked();

    let sql = window
        .update(&mut cx, |_workspace, _window, cx| panel.read(cx).sql(cx))
        .expect("the window is open");
    assert_eq!(
        sql,
        "SELECT PERSON.NAME, TEAM.NAME, OFFICE.CITY\n\
             FROM BLD.PERSON\n\
             \x20 INNER JOIN BLD.TEAM ON PERSON.TEAM_ID = TEAM.ID\n\
             \x20 INNER JOIN BLD.OFFICE ON TEAM.OFFICE_ID = OFFICE.ID"
    );

    // "Open in editor": the panel's one message, the workspace's one gate.
    window
        .update(&mut cx, |_workspace, _window, cx| {
            panel.update(cx, |panel, cx| panel.open_in_editor(cx));
        })
        .expect("the window is open");
    cx.run_until_parked();

    let query = window
        .update(&mut cx, |workspace, _window, cx| {
            assert_eq!(
                tab_titles(workspace, pane, cx),
                [
                    ts!("builder.tab", index = 1).to_string(),
                    ts!("query.tab", index = 1).to_string()
                ]
            );
            let query = workspace
                .active_query()
                .expect("the query pane is the tab on top")
                .clone();
            assert_eq!(query.read(cx).editor_text(cx), sql);
            query
        })
        .expect("the window is open");

    // And it runs, through the editor's own chord: the builder produced a
    // statement the database accepts, which is the whole of the milestone.
    cx.simulate_keystrokes(RUN_ALL);
    cx.run_until_parked();

    window
        .update(&mut cx, |_workspace, _window, cx| {
            let query = query.read(cx);
            assert!(!query.is_running(), "the run never came back");
            assert_eq!(
                query.status_cells().0,
                ts!("query.row_count", count = 2),
                "the join did not produce the two people"
            );
        })
        .expect("the window is open");
}

/// A drop lands on the builder it was let go of, not on the one the action
/// would have chosen.
///
/// Two builders, the second in front — which is where "add to builder"
/// puts everything. The table announced by the *first* has to arrive
/// there, because the pointer was over it, and nowhere else. The drop
/// gesture itself is `BuilderPane`'s, tested there against real pointer
/// events; what is at stake here is which panel the shell then loads for.
#[gpui::test]
fn a_dropped_table_lands_on_the_builder_it_was_dropped_on(cx: &mut gpui::TestAppContext) {
    let (window, id) = workspace_over_h2("builder-drop", cx);
    let mut cx = gpui::VisualTestContext::from_window(window.into(), cx);

    let (first, second) = window
        .update(&mut cx, |workspace, window, cx| {
            let first = workspace.open_builder(window, cx).expect("a builder opens");
            let second = workspace
                .open_builder(window, cx)
                .expect("a second builder opens");
            (first, second)
        })
        .expect("the window is open");
    cx.run_until_parked();

    window
        .update(&mut cx, |workspace, _window, cx| {
            // The one in front is the second, so the action's rule and the
            // drop's would disagree — which is what makes the assertion
            // below mean something.
            assert_eq!(
                workspace
                    .builder_tab()
                    .and_then(|(pane, index)| workspace.builder_at(pane, index)),
                Some(second.clone())
            );
            first.update(cx, |_panel, cx| {
                cx.emit(BuilderPaneEvent::TableDropped(object(id, "ORDERS")));
            });
        })
        .expect("the window is open");
    cx.run_until_parked();

    window
        .update(&mut cx, |_workspace, _window, cx| {
            assert_eq!(first.read(cx).table_count(), 1);
            assert_eq!(second.read(cx).table_count(), 0);
        })
        .expect("the window is open");
}

/// The explorer's own "query this object", which now writes its `FROM`
/// through the same quoting the builder uses.
///
/// The assertion is that an ordinary name is *unchanged*: quoting only
/// where it is needed is the whole point of the new API, and a statement
/// that suddenly came out as `SELECT * FROM "PUBLIC"."ORDERS"` would be a
/// regression rather than a fix.
#[gpui::test]
fn querying_an_object_leaves_an_ordinary_name_bare(cx: &mut gpui::TestAppContext) {
    let (window, id) = workspace_over_h2("query-for", cx);
    let mut cx = gpui::VisualTestContext::from_window(window.into(), cx);

    window
        .update(&mut cx, |workspace, window, cx| {
            workspace.open_query_for(&object(id, "ORDERS"), window, cx);
            let query = workspace
                .active_query()
                .expect("the query pane is the tab on top");
            assert_eq!(
                query.read(cx).editor_text(cx),
                "SELECT * FROM PUBLIC.ORDERS"
            );

            // A name that would not survive being written bare is quoted,
            // and the schema is still in front of it.
            let mut awkward = object(id, "Order Details");
            awkward.schema = Some("PUBLIC".to_string());
            workspace.open_query_for(&awkward, window, cx);
            let query = workspace
                .active_query()
                .expect("the second query pane is the tab on top");
            assert_eq!(
                query.read(cx).editor_text(cx),
                "SELECT * FROM PUBLIC.\"Order Details\""
            );
        })
        .expect("the window is open");
    cx.run_until_parked();
}

/// Closing a connection tab takes its whole work area with it.
///
/// The tabs belonged to that connection and nothing else, so keeping them
/// would leave the window carrying panels of a database nobody can ask —
/// and, worse, editors still holding a session handle each. A write
/// confirmation waiting on one of those panes has nobody left to answer it
/// and goes too.
#[gpui::test]
fn closing_a_connection_discards_its_work_area(cx: &mut gpui::TestAppContext) {
    let (window, id) = workspace_over_h2("abandon", cx);
    let mut cx = gpui::VisualTestContext::from_window(window.into(), cx);

    window
        .update(&mut cx, |workspace, window, cx| {
            workspace.open_object(object(id, "ORDERS"), window, cx);
            workspace.open_query("SELECT 1", window, cx);
            let pane = active_pane(workspace);
            assert_eq!(
                tab_titles(workspace, pane, cx),
                ["PUBLIC.ORDERS", "Query 1"]
            );
            // As if the editor had asked before running a `DELETE`.
            let query = workspace
                .active_query()
                .expect("the query is the tab on top")
                .clone();
            workspace.confirm = Some(PendingConfirm {
                pane: query,
                request: Box::new(ConfirmRequest {
                    count: 1,
                    preview: "DELETE FROM PUBLIC.ORDERS".into(),
                }),
            });
        })
        .expect("the window is open");
    cx.run_until_parked();

    window
        .update(&mut cx, |workspace, window, cx| {
            workspace.close_connection(0, window, cx);

            assert!(workspace.connections.is_empty());
            // No connection, so no work area at all: the body renders the
            // empty state and every pane command is a no-op.
            assert!(workspace.work_area().is_none());
            assert!(workspace.active_pane().is_none());
            assert!(workspace.active_query().is_none());
            assert!(
                workspace.confirm.is_none(),
                "the confirmation outlived the pane that asked"
            );
            assert!(
                workspace.explorer.read(cx).visible_roots(cx).is_empty(),
                "the sidebar is still showing a closed connection"
            );

            // And the pane commands, which have no tree to act on.
            workspace.split_active(Axis::Horizontal, cx);
            workspace.close_active_pane(window, cx);
            workspace.cycle_pane(true, cx);
            assert!(workspace.work_area().is_none());
        })
        .expect("the window is open");
    cx.run_until_parked();
}

/// A connection that dies under the user keeps its tabs and lets go of the
/// session: what the database said stays readable, what would ask it again
/// refuses.
#[gpui::test]
fn a_dead_connection_detaches_its_query_panes(cx: &mut gpui::TestAppContext) {
    let (window, id) = workspace_over_h2("tunnel-death", cx);
    let mut cx = gpui::VisualTestContext::from_window(window.into(), cx);

    let pane = window
        .update(&mut cx, |workspace, window, cx| {
            workspace.open_object(object(id, "ORDERS"), window, cx);
            workspace.open_query("SELECT 1", window, cx);
            active_pane(workspace)
        })
        .expect("the window is open");
    cx.run_until_parked();

    window
        .update(&mut cx, |workspace, _window, cx| {
            // What the tunnel watcher does when the SSH channel closes.
            workspace.tunnel_died(0, "the channel closed".to_string(), cx);

            // Both tabs stay: the layout is the user's, and a session dying
            // must not rearrange their window.
            assert_eq!(
                tab_titles(workspace, pane, cx),
                ["PUBLIC.ORDERS", "Query 1"]
            );
            let query = workspace
                .active_query()
                .expect("the query tab survived the death")
                .read(cx);
            assert!(!query.is_attached(), "the session was not let go of");
            assert_eq!(query.status_cells().0, ts!("statusbar.disconnected"));

            // The tab and its area are still reachable, and the strip says
            // what happened.
            let connection = workspace
                .active_connection()
                .expect("the tab is still open");
            assert!(matches!(connection.state, ConnectionState::Dead(_)));
            assert_eq!(connection.state.tab_status(), TabStatus::Error);
        })
        .expect("the window is open");
    cx.run_until_parked();
}

/// The whole point of the design: the connection tab selects the window.
///
/// Tabs opened under one connection are not visible under another, the
/// explorer's root follows the same switch, and coming back finds the area
/// exactly as it was left — same tabs, same one on top.
#[gpui::test]
fn switching_the_connection_tab_switches_the_work_area(cx: &mut gpui::TestAppContext) {
    let (window, first) = workspace_over_h2("switch-a", cx);
    let (profile, connected) = h2_connection("switch-b");
    let mut cx = gpui::VisualTestContext::from_window(window.into(), cx);

    let pane_a = window
        .update(&mut cx, |workspace, window, cx| {
            workspace.open_object(object(first, "ORDERS"), window, cx);
            workspace.open_query("SELECT 1", window, cx);
            let pane = active_pane(workspace);
            assert_eq!(
                tab_titles(workspace, pane, cx),
                ["PUBLIC.ORDERS", "Query 1"]
            );
            assert_eq!(active_tab(workspace, pane), 1);
            assert_eq!(workspace.explorer.read(cx).visible_roots(cx), [first]);
            pane
        })
        .expect("the window is open");
    cx.run_until_parked();

    let second = window
        .update(&mut cx, |workspace, window, cx| {
            let second = push_connection(workspace, profile, connected, window, cx);

            // A work area of its own: nothing of the first connection is in
            // it, and its numbering starts again at one.
            let pane = active_pane(workspace);
            assert_ne!(pane, pane_a, "the second tab brought its own pane tree");
            assert!(tab_titles(workspace, pane, cx).is_empty());
            workspace.open_query("SELECT 2", window, cx);
            assert_eq!(
                tab_titles(workspace, pane, cx),
                ["Query 1"],
                "query numbering is per connection"
            );
            assert_eq!(workspace.explorer.read(cx).visible_roots(cx), [second]);
            second
        })
        .expect("the window is open");
    cx.run_until_parked();

    window
        .update(&mut cx, |workspace, window, cx| {
            workspace.select_connection(0, window, cx);

            // Back where the first connection was left, tab on top and all.
            assert_eq!(active_pane(workspace), pane_a);
            assert_eq!(
                tab_titles(workspace, pane_a, cx),
                ["PUBLIC.ORDERS", "Query 1"]
            );
            assert_eq!(active_tab(workspace, pane_a), 1);
            assert_eq!(workspace.explorer.read(cx).visible_roots(cx), [first]);
            assert_ne!(first, second);
        })
        .expect("the window is open");
    cx.run_until_parked();
}

/// Switching the connection tab takes the outgoing area off screen, editors
/// and all, which is the hazard [`Workspace::reclaim_focus`] describes one
/// level up. The keyboard has to land on something the next frame renders.
#[gpui::test]
fn switching_the_connection_tab_does_not_strand_the_keyboard(cx: &mut gpui::TestAppContext) {
    let (window, _first) = workspace_over_h2("focus-a", cx);
    let (profile, connected) = h2_connection("focus-b");
    let mut cx = gpui::VisualTestContext::from_window(window.into(), cx);

    window
        .update(&mut cx, |workspace, window, cx| {
            workspace.open_query("SELECT 1", window, cx);
        })
        .expect("the window is open");
    cx.run_until_parked();

    let editor = window
        .update(&mut cx, |workspace, window, cx| {
            let editor = workspace
                .active_query()
                .expect("the editor is the tab on top")
                .read(cx)
                .focus_handle(cx);
            assert!(editor.is_focused(window), "the editor did not take focus");

            push_connection(workspace, profile, connected, window, cx);
            assert!(
                !editor.is_focused(window),
                "the keyboard stayed on an editor the new tab does not render"
            );
            assert!(
                workspace.focus_handle.is_focused(window),
                "the incoming area holds nothing typeable, so the shell takes over"
            );
            editor
        })
        .expect("the window is open");
    cx.run_until_parked();

    // The other direction, where the incoming area *does* have something to
    // type into: an editor going off screen hands the caret to the editor
    // coming on, so a user switching between two connections keeps typing.
    window
        .update(&mut cx, |workspace, window, cx| {
            workspace.open_query("SELECT 2", window, cx);
            let second = workspace
                .active_query()
                .expect("the new tab is the editor")
                .read(cx)
                .focus_handle(cx);
            assert!(second.is_focused(window), "the editor did not take focus");

            workspace.select_connection(0, window, cx);
            assert!(
                !second.is_focused(window),
                "the keyboard stayed on the editor of the tab switched away from"
            );
            assert!(
                editor.is_focused(window),
                "the editor coming back on screen did not take the caret"
            );
        })
        .expect("the window is open");
    cx.run_until_parked();

    // The regression the rule exists for: with the focus stranded, this
    // dispatch would never arrive.
    let showing = window
        .update(&mut cx, |workspace, _window, _cx| {
            workspace.explorer_visible
        })
        .expect("the window is open");
    cx.dispatch_action(ToggleExplorer);
    window
        .update(&mut cx, |workspace, _window, _cx| {
            assert_ne!(
                workspace.explorer_visible, showing,
                "the action was dropped: the focus is on something unrendered"
            );
        })
        .expect("the window is open");
}

/// A split is part of a work area, so it belongs to one connection: the tab
/// beside it keeps the single pane it started with.
#[gpui::test]
fn the_split_layout_is_per_connection(cx: &mut gpui::TestAppContext) {
    let (window, _first) = workspace_over_h2("split-a", cx);
    let (profile, connected) = h2_connection("split-b");
    let mut cx = gpui::VisualTestContext::from_window(window.into(), cx);

    let (left, right) = window
        .update(&mut cx, |workspace, window, cx| {
            let left = active_pane(workspace);
            workspace.split_active(Axis::Horizontal, cx);
            let right = active_pane(workspace);
            // A marker in the new pane, so that coming back can be told from
            // a layout that merely happens to have two panes.
            workspace.open_query("SELECT 1", window, cx);
            assert_eq!(area(workspace).panes.leaf_count(), 2);
            (left, right)
        })
        .expect("the window is open");
    cx.run_until_parked();

    window
        .update(&mut cx, |workspace, window, cx| {
            push_connection(workspace, profile, connected, window, cx);
            assert_eq!(
                area(workspace).panes.leaf_count(),
                1,
                "the new connection inherited a split it never asked for"
            );

            workspace.select_connection(0, window, cx);
            let area = area(workspace);
            assert_eq!(area.panes.leaf_count(), 2);
            assert_eq!(area.panes.leaf_ids(), vec![left, right]);
            assert_eq!(area.active(), right, "the marker moved while away");
            assert_eq!(tab_titles(workspace, right, cx), ["Query 1"]);
        })
        .expect("the window is open");
    cx.run_until_parked();
}

/// Opening a `.sql` file is a query pane with the file already in it.
///
/// Driven through [`Workspace::load_sql_file`] rather than the action: the
/// action's first step is a platform file picker, which a headless test has
/// no way to answer.
#[gpui::test]
fn a_sql_file_opens_as_a_query_pane_with_its_text_in_it(cx: &mut gpui::TestAppContext) {
    let (window, _id) = workspace_over_h2("open-file", cx);
    let mut cx = gpui::VisualTestContext::from_window(window.into(), cx);

    let dir = tempfile::tempdir().expect("a temporary directory");
    let path = dir.path().join("report.sql");
    let script = "-- nightly report\nSELECT 1;\nSELECT 2;\n";
    std::fs::write(&path, script).expect("the fixture is written");

    window
        .update(&mut cx, |workspace, window, cx| {
            workspace.load_sql_file(path.clone(), window, cx);
        })
        .expect("the window is open");
    cx.run_until_parked();

    window
        .update(&mut cx, |workspace, _window, cx| {
            let pane = workspace
                .active_query()
                .expect("the file opened a query pane and brought it to the front");
            assert_eq!(pane.read(cx).editor_text(cx), script);
        })
        .expect("the window is open");
}

/// A file whose bytes are not UTF-8 still opens: the invalid runs are
/// replaced rather than the whole file refused.
#[gpui::test]
fn a_file_that_is_not_utf8_opens_lossily(cx: &mut gpui::TestAppContext) {
    let (window, _id) = workspace_over_h2("open-latin1", cx);
    let mut cx = gpui::VisualTestContext::from_window(window.into(), cx);

    let dir = tempfile::tempdir().expect("a temporary directory");
    let path = dir.path().join("latin1.sql");
    // `SELECT 'é'` as Latin-1: the accented byte is not valid UTF-8.
    let mut bytes = b"SELECT '".to_vec();
    bytes.push(0xE9);
    bytes.extend_from_slice(b"'");
    std::fs::write(&path, &bytes).expect("the fixture is written");

    window
        .update(&mut cx, |workspace, window, cx| {
            workspace.load_sql_file(path.clone(), window, cx);
        })
        .expect("the window is open");
    cx.run_until_parked();

    window
        .update(&mut cx, |workspace, _window, cx| {
            let pane = workspace
                .active_query()
                .expect("the file opened a query pane");
            let text = pane.read(cx).editor_text(cx);
            assert!(
                text.starts_with("SELECT '"),
                "the readable part of the file was thrown away: {text:?}"
            );
            assert!(
                text.contains('\u{fffd}'),
                "the undecodable byte was dropped rather than replaced: {text:?}"
            );
        })
        .expect("the window is open");
}

/// With no connection open there is nowhere to put a file, so nothing is
/// opened — the same gate [`Workspace::open_query`] applies.
#[gpui::test]
fn a_sql_file_needs_a_connection_to_open_into(cx: &mut gpui::TestAppContext) {
    cx.update(|cx| {
        app_settings::init(cx);
        rugpui::init(cx);
        rugpui_editor::init(cx);
        rugpui_grid::init(cx);
    });
    let window = cx.add_window(|window, cx| Workspace::new(TitlebarStyle::Custom, window, cx));
    let mut cx = gpui::VisualTestContext::from_window(window.into(), cx);

    let dir = tempfile::tempdir().expect("a temporary directory");
    let path = dir.path().join("orphan.sql");
    std::fs::write(&path, "SELECT 1").expect("the fixture is written");

    window
        .update(&mut cx, |workspace, window, cx| {
            workspace.load_sql_file(path.clone(), window, cx);
        })
        .expect("the window is open");
    cx.run_until_parked();

    window
        .update(&mut cx, |workspace, _window, _cx| {
            assert!(workspace.work_area().is_none(), "no connection, no area");
            assert!(workspace.active_query().is_none());
        })
        .expect("the window is open");
}
