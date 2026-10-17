use super::*;

/// Opening things does not throw away what the pane already held, and
/// opening the *same* object twice does not open it twice.
#[gpui::test]
fn a_detail_and_a_query_are_two_tabs_of_one_pane(cx: &mut gpui::TestAppContext) {
    let (window, id) = workspace_over_h2("tabs", cx);
    let target = object(id, "ORDERS");
    let mut cx = gpui::VisualTestContext::from_window(window.into(), cx);

    let first = window
        .update(&mut cx, |workspace, window, cx| {
            workspace.open_object(target.clone(), window, cx);
            workspace.open_query("SELECT 1", window, cx);

            let pane = active_pane(workspace);
            // Both are open, in the order they were opened, and the query —
            // the one just asked for — is the one showing.
            assert_eq!(
                tab_titles(workspace, pane, cx),
                ["PUBLIC.ORDERS", "Query 1"]
            );
            assert_eq!(active_tab(workspace, pane), 1);
            assert!(workspace.active_query().is_some());

            // The same object again is a navigation, not a second copy.
            workspace.open_object(target.clone(), window, cx);
            assert_eq!(
                tab_titles(workspace, pane, cx),
                ["PUBLIC.ORDERS", "Query 1"]
            );
            assert_eq!(active_tab(workspace, pane), 0);
            assert!(
                workspace.active_query().is_none(),
                "the detail tab is the one showing, so no query pane is active"
            );
            pane
        })
        .expect("the window is open");
    cx.run_until_parked();

    window
        .update(&mut cx, |workspace, window, cx| {
            workspace.split_active(Axis::Horizontal, window, cx);
            let second = active_pane(workspace);
            assert_ne!(second, first, "the split produced a pane of its own");
            workspace.open_query("SELECT 2", window, cx);
            assert_eq!(
                tab_titles(workspace, second, cx),
                ["PUBLIC.ORDERS", "Query 2"]
            );

            // Asking for the object from the other pane jumps to the pane
            // already showing it rather than opening a copy beside it.
            workspace.open_object(target.clone(), window, cx);
            assert_eq!(active_pane(workspace), first);
            assert_eq!(active_tab(workspace, first), 0);
            assert_eq!(
                tab_titles(workspace, first, cx),
                ["PUBLIC.ORDERS", "Query 1"]
            );
            assert_eq!(
                tab_titles(workspace, second, cx),
                ["PUBLIC.ORDERS", "Query 2"]
            );
        })
        .expect("the window is open");
    cx.run_until_parked();
}

/// The rows of a table and its columns are two tabs over one object, and
/// each way in deduplicates against its own kind rather than against the
/// other (architecture document, §7.9).
#[gpui::test]
fn view_data_opens_one_tab_of_rows_per_table(cx: &mut gpui::TestAppContext) {
    let (window, id) = workspace_over_h2_with(
        "view-data",
        &[
            "create table CUSTOMER (ID int primary key, NAME varchar(20))",
            "insert into CUSTOMER values (1, 'a'), (2, 'b')",
        ],
        cx,
    );
    let target = object(id, "CUSTOMER");
    let mut cx = gpui::VisualTestContext::from_window(window.into(), cx);

    let pane = window
        .update(&mut cx, |workspace, window, cx| {
            workspace.open_data(target.clone(), window, cx);
            let pane = active_pane(workspace);
            assert_eq!(tab_titles(workspace, pane, cx), ["PUBLIC.CUSTOMER"]);

            // The detail panel of the same table is a second tab: the
            // columns and the rows are different questions.
            workspace.open_object(target.clone(), window, cx);
            assert_eq!(
                tab_titles(workspace, pane, cx),
                ["PUBLIC.CUSTOMER", "PUBLIC.CUSTOMER"]
            );
            assert_eq!(active_tab(workspace, pane), 1);

            // Asking for the rows again is a navigation, not a second copy.
            workspace.open_data(target.clone(), window, cx);
            assert_eq!(
                tab_titles(workspace, pane, cx).len(),
                2,
                "the rows were opened twice"
            );
            assert_eq!(active_tab(workspace, pane), 0);
            pane
        })
        .expect("the window is open");
    cx.run_until_parked();

    // And the pane really ran its own statement: nothing above it fetched
    // anything on its behalf.
    window
        .update(&mut cx, |workspace, _window, cx| {
            let Some(PaneItem::TableData(panel)) = area(workspace)
                .panes
                .get(pane)
                .expect("the pane is in the tree")
                .get(0)
            else {
                panic!("the first tab is the rows");
            };
            assert_eq!(panel.read(cx).row_count(cx), Some(2));
        })
        .expect("the window is open");
}

/// Closing a tab is the same focus hazard as closing a pane: the view in it
/// A user's own reported order: the detail tab, the rows, a field opened
/// over a cell — and then a diagram of the schema asked for while all of
/// it is still open on the one session.
#[gpui::test]
fn a_diagram_loads_while_rows_are_open_for_editing(cx: &mut gpui::TestAppContext) {
    let (window, id) = workspace_over_h2_with(
        "erd-under-editing",
        &[
            "create table CUSTOMER (ID int primary key, NAME varchar(20))",
            "insert into CUSTOMER values (1, 'a'), (2, 'b')",
            "create table ORDERS (ID int primary key, \
                 CUSTOMER_ID int references CUSTOMER(ID))",
        ],
        cx,
    );
    let target = object(id, "CUSTOMER");
    let mut cx = gpui::VisualTestContext::from_window(window.into(), cx);

    let pane = window
        .update(&mut cx, |workspace, window, cx| {
            workspace.open_object(target.clone(), window, cx);
            workspace.open_data(target.clone(), window, cx);
            // A first diagram asked for while the detail's and the rows'
            // own fetches are still out, so all three interleave on the
            // one session worker...
            workspace.open_erd(erd_target(id), window, cx);
            active_pane(workspace)
        })
        .expect("the window is open");
    cx.run_until_parked();
    // ...and a second asked for the way the report had it, over rows that
    // have arrived and a field already open. Closed and reopened, because
    // asking again while the tab exists is a navigation, not a fetch.
    window
        .update(&mut cx, |workspace, window, cx| {
            let (erd_pane, tab) = workspace
                .erd_tab(&erd_target(id), cx)
                .expect("the diagram tab is open");
            if let Some(PaneItem::Erd(panel)) = area(workspace)
                .panes
                .get(erd_pane)
                .expect("the pane is in the tree")
                .get(tab)
            {
                assert_eq!(
                    panel.read(cx).failure(),
                    None,
                    "the diagram raced the other fetches and lost"
                );
            }
            workspace.close_tab(erd_pane, tab, window, cx);
        })
        .expect("the window is open");
    cx.run_until_parked();

    window
        .update(&mut cx, |workspace, window, cx| {
            let Some(PaneItem::TableData(panel)) = area(workspace)
                .panes
                .get(pane)
                .expect("the pane is in the tree")
                .get(1)
            else {
                panic!("the second tab is the rows");
            };
            panel.clone().update(cx, |panel, cx| {
                let grid = panel.grid().cloned().expect("the pane holds rows");
                let opened = grid.update(cx, |grid, cx| grid.begin_edit(0, 1, window, cx));
                assert!(opened, "no field opened over the cell");
            });
            workspace.open_erd(erd_target(id), window, cx);
        })
        .expect("the window is open");
    cx.run_until_parked();

    window
        .update(&mut cx, |workspace, _window, cx| {
            let Some(PaneItem::Erd(panel)) = area(workspace)
                .panes
                .get(pane)
                .expect("the pane is in the tree")
                .get(2)
            else {
                panic!("the third tab is the diagram");
            };
            let diagram = panel.read(cx);
            assert_eq!(diagram.failure(), None, "the diagram did not load");
            assert!(!diagram.is_loading(), "the fetch never came back");
        })
        .expect("the window is open");
}

/// stops being rendered, and gpui resolves actions against the focused
/// element of the last drawn frame. The keyboard has to land on the
/// neighbour that took its place — or on the shell, when nothing did.
#[gpui::test]
fn closing_a_tab_hands_the_keyboard_to_the_tab_beside_it(cx: &mut gpui::TestAppContext) {
    let (window, _id) = workspace_over_h2("tab-focus", cx);
    let mut cx = gpui::VisualTestContext::from_window(window.into(), cx);

    let pane = window
        .update(&mut cx, |workspace, window, cx| {
            workspace.open_query("SELECT 1", window, cx);
            workspace.open_query("SELECT 2", window, cx);
            active_pane(workspace)
        })
        .expect("the window is open");
    cx.run_until_parked();

    window
        .update(&mut cx, |workspace, window, cx| {
            let editor = workspace
                .active_query()
                .expect("the second query is showing")
                .read(cx)
                .focus_handle(cx);
            assert!(editor.is_focused(window), "the editor did not take focus");

            workspace.close_tab(pane, 1, window, cx);
            assert_eq!(tab_titles(workspace, pane, cx), ["Query 1"]);
            assert_eq!(active_tab(workspace, pane), 0);
            let neighbour = workspace
                .active_query()
                .expect("the first query took its place")
                .read(cx)
                .focus_handle(cx);
            assert!(
                neighbour.is_focused(window),
                "the focus stayed on the editor of the closed tab"
            );
        })
        .expect("the window is open");
    cx.run_until_parked();

    window
        .update(&mut cx, |workspace, window, cx| {
            workspace.close_tab(pane, 0, window, cx);
            assert!(
                area(workspace)
                    .panes
                    .get(pane)
                    .expect("the pane outlives its tabs")
                    .is_empty(),
                "closing the last tab must leave the pane standing"
            );
            assert!(
                workspace.focus_handle.is_focused(window),
                "an empty pane has nothing to type into, so the shell takes over"
            );
        })
        .expect("the window is open");

    // The regression the rule exists for: with the focus stranded on an
    // editor nothing renders, this dispatch would never arrive.
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

/// The ERD's end of the tab discipline: one diagram per scope, opened by
/// the action the menu row dispatches, and a focus that comes back to the
/// shell when the tab is closed.
#[gpui::test]
fn an_erd_opens_once_per_scope_and_the_action_finds_it(cx: &mut gpui::TestAppContext) {
    let (window, id) = workspace_over_h2("erd-tabs", cx);
    let mut cx = gpui::VisualTestContext::from_window(window.into(), cx);

    let pane = window
        .update(&mut cx, |workspace, window, cx| {
            workspace.open_object(object(id, "ORDERS"), window, cx);
            workspace.open_erd(erd_target(id), window, cx);

            let pane = active_pane(workspace);
            let titles = tab_titles(workspace, pane, cx);
            assert_eq!(titles.len(), 2, "{titles:?}");
            assert_eq!(titles[1], ts!("erd.tab", scope = "PUBLIC").to_string());
            assert_eq!(active_tab(workspace, pane), 1);
            // A diagram is not a query, so the status bar's own cells stay
            // empty over one.
            assert!(workspace.active_query().is_none());
            pane
        })
        .expect("the window is open");
    cx.run_until_parked();

    window
        .update(&mut cx, |workspace, window, cx| {
            // Back to the detail tab, and then the same scope again: a
            // navigation, not a second copy.
            workspace.activate_tab(pane, 0, window, cx);
            assert_eq!(active_tab(workspace, pane), 0);

            workspace.open_erd(erd_target(id), window, cx);
            assert_eq!(
                tab_titles(workspace, pane, cx).len(),
                2,
                "the same scope opened a second tab"
            );
            assert_eq!(active_tab(workspace, pane), 1);
        })
        .expect("the window is open");
    cx.run_until_parked();

    // And through the action the menu row and the shortcut dispatch, which
    // reads the explorer's selection rather than being handed a target.
    window
        .update(&mut cx, |workspace, window, cx| {
            workspace.activate_tab(pane, 0, window, cx);
            workspace.explorer.update(cx, |explorer, cx| {
                explorer.select(
                    NodeId::Folder {
                        connection: id,
                        scope: explorer::Scope {
                            catalog: None,
                            schema: Some("PUBLIC".to_string()),
                        },
                        folder: explorer::Folder::Tables,
                    },
                    cx,
                );
            });
        })
        .expect("the window is open");
    cx.run_until_parked();

    cx.dispatch_action(OpenErd);
    window
        .update(&mut cx, |workspace, window, cx| {
            assert_eq!(
                tab_titles(workspace, pane, cx).len(),
                2,
                "the action opened a diagram beside the one already showing"
            );
            assert_eq!(active_tab(workspace, pane), 1);

            // With the keyboard inside the diagram, closing it has to hand
            // the keyboard on: the panel stops being rendered, and gpui
            // resolves actions against the last drawn frame.
            workspace.focus_active_tab(pane, window, cx);
            assert!(
                workspace.pane_holds_focus(pane, window, cx),
                "the diagram did not take the keyboard"
            );
            workspace.close_tab(pane, 1, window, cx);
            assert_eq!(tab_titles(workspace, pane, cx), ["PUBLIC.ORDERS"]);
            assert!(
                workspace.focus_handle.is_focused(window),
                "a detail panel has nothing to type into, so the shell takes over"
            );
        })
        .expect("the window is open");
    cx.run_until_parked();
}

/// The whole thread, over a real database: the loader reaches H2, the
/// panel is handed a model, and moving a box writes the layout file that
/// the next open reads back.
#[gpui::test]
fn a_diagram_loads_from_the_database_and_its_layout_survives(cx: &mut gpui::TestAppContext) {
    let (window, id) = workspace_over_h2("erd-load", cx);
    let mut cx = gpui::VisualTestContext::from_window(window.into(), cx);

    // A schema of this connection's own, so the diagram has a foreign key
    // in it whatever else the H2 instance holds. Outside the update: the
    // call blocks, and the rule the shell itself follows is that nothing
    // blocking runs inside one.
    let session = window
        .update(&mut cx, |workspace, _window, _cx| {
            workspace.session_of(id).expect("the session is live")
        })
        .expect("the window is open");
    for sql in [
        "create schema if not exists ERD",
        "create table ERD.TEAM (ID int primary key, NAME varchar(40))",
        "create table ERD.PERSON (ID int primary key, TEAM_ID int not null, \
                 constraint FK_ERD_PERSON_TEAM foreign key (TEAM_ID) references ERD.TEAM(ID))",
    ] {
        session
            .session()
            .execute(&rudbman_jdbc::StatementSpec::new(sql))
            .unwrap_or_else(|error| panic!("{sql}: {error}"));
    }
    drop(session);

    let target = ErdTarget {
        connection: id,
        scope: explorer::Scope {
            catalog: None,
            schema: Some("ERD".to_string()),
        },
    };
    let panel = window
        .update(&mut cx, |workspace, window, cx| {
            workspace.open_erd(target.clone(), window, cx);
            let pane = active_pane(workspace);
            match area(workspace)
                .panes
                .get(pane)
                .expect("the pane is in the tree")
                .active()
            {
                Some(PaneItem::Erd(panel)) => panel.clone(),
                other => panic!("the ERD is not the tab on top: {other:?}"),
            }
        })
        .expect("the window is open");
    cx.run_until_parked();

    let positions = window
        .update(&mut cx, |_workspace, _window, cx| {
            let panel = panel.read(cx);
            assert!(!panel.is_loading(), "the fetch never came back");
            assert_eq!(panel.failure(), None);
            assert_eq!(panel.table_count(), Some(2));
            panel.positions(cx)
        })
        .expect("the window is open");
    assert!(positions.contains_key("PERSON"), "{positions:?}");
    assert!(positions.contains_key("TEAM"), "{positions:?}");

    // What a drag amounts to, without the mouse: the file is written from
    // the event the canvas raises, and the workspace is what writes it.
    let dir = tempfile::tempdir().expect("a temporary directory");
    let path = dir.path().join("layout.json");
    let mut layouts = ErdLayouts::default();
    layouts.set_positions(&target.scope, positions);
    layouts.save_to(&path).expect("the layout is written");

    let read = ErdLayouts::load_from(&path).expect("the layout is read back");
    let saved = read.positions(&target.scope);
    assert_eq!(saved.len(), 2, "{saved:?}");
    assert!(saved.contains_key("PERSON"));
}

#[gpui::test]
fn splitting_copies_the_selected_sql_and_name_into_an_independent_editor(
    cx: &mut gpui::TestAppContext,
) {
    let (window, _) = workspace_over_h2("split-clones-query", cx);
    let mut cx = gpui::VisualTestContext::from_window(window.into(), cx);
    let (source, copy, first, second) = window
        .update(&mut cx, |workspace, window, cx| {
            workspace.open_query("select 'unselected'", window, cx);
            workspace.open_query("select 'selected'", window, cx);
            let source = workspace.active_query().unwrap().clone();
            let first = workspace.active_pane().unwrap();
            workspace
                .work_area_mut()
                .unwrap()
                .tab_names
                .insert(source.entity_id(), "분석".into());
            workspace.split_active(Axis::Horizontal, window, cx);
            let second = workspace.active_pane().unwrap();
            let copy = workspace.active_query().unwrap().clone();
            assert_ne!(source.entity_id(), copy.entity_id());
            assert_eq!(copy.read(cx).editor_text(cx), "select 'selected'");
            assert_eq!(
                workspace
                    .work_area()
                    .unwrap()
                    .panes
                    .get(second)
                    .unwrap()
                    .items()
                    .len(),
                1
            );
            assert_eq!(
                workspace.work_area().unwrap().tab_names[&copy.entity_id()],
                "분석"
            );
            assert_eq!(
                workspace
                    .work_area()
                    .unwrap()
                    .panes
                    .get(first)
                    .unwrap()
                    .items()
                    .len(),
                2
            );
            (source, copy, first, second)
        })
        .unwrap();
    cx.run_until_parked();
    cx.simulate_input("/* copy */ ");
    window
        .update(&mut cx, |workspace, window, cx| {
            assert_eq!(source.read(cx).editor_text(cx), "select 'selected'");
            assert_eq!(
                copy.read(cx).editor_text(cx),
                "/* copy */ select 'selected'"
            );
            workspace.close_tab(second, 0, window, cx);
            assert!(!workspace.work_area().unwrap().panes.contains(second));
            assert_eq!(workspace.active_pane(), Some(first));
            assert!(source.read(cx).focus_handle(cx).is_focused(window));
            assert!(
                !workspace
                    .work_area()
                    .unwrap()
                    .tab_names
                    .contains_key(&copy.entity_id())
            );
            workspace.close_tabs(first, &[0, 1], window, cx);
            assert_eq!(workspace.work_area().unwrap().panes.leaf_count(), 1);
            assert!(
                workspace
                    .work_area()
                    .unwrap()
                    .panes
                    .get(first)
                    .unwrap()
                    .is_empty()
            );
            assert!(workspace.focus_handle.is_focused(window));
        })
        .unwrap();
}

#[gpui::test]
fn closing_a_background_panes_last_tab_collapses_only_that_pane(cx: &mut gpui::TestAppContext) {
    let (window, _) = workspace_over_h2("collapse-background-pane", cx);
    let mut cx = gpui::VisualTestContext::from_window(window.into(), cx);
    let (first, second, third) = window
        .update(&mut cx, |workspace, window, cx| {
            workspace.open_query("select 1", window, cx);
            let first = workspace.active_pane().unwrap();
            workspace.split_active(Axis::Horizontal, window, cx);
            let second = workspace.active_pane().unwrap();
            workspace.split_active(Axis::Vertical, window, cx);
            let third = workspace.active_pane().unwrap();
            workspace.activate_tab(first, 0, window, cx);
            workspace.focus_active_tab(first, window, cx);
            (first, second, third)
        })
        .unwrap();
    cx.run_until_parked();
    window
        .update(&mut cx, |workspace, window, cx| {
            let focus = window.focused(cx);
            workspace.close_tab(second, 0, window, cx);
            assert_eq!(
                workspace.work_area().unwrap().panes.leaf_ids(),
                vec![first, third]
            );
            assert_eq!(workspace.active_pane(), Some(first));
            assert_eq!(window.focused(cx), focus);
            workspace.close_tabs(third, &[0], window, cx);
            assert_eq!(workspace.work_area().unwrap().panes.leaf_ids(), vec![first]);
            assert_eq!(window.focused(cx), focus);
        })
        .unwrap();
}

/// The drop bar follows a drag only over a strip that would take the tab:
/// a pane's strip refuses a connection tab, the connection strip refuses a
/// pane's, and a strip the pointer is not over never takes down the bar of
/// the one it is.
#[gpui::test]
fn the_drop_bar_shows_only_where_the_dragged_tab_would_be_taken(cx: &mut gpui::TestAppContext) {
    let (window, _) = workspace_over_h2("drop-bar", cx);
    let mut cx = gpui::VisualTestContext::from_window(window.into(), cx);
    window
        .update(&mut cx, |workspace, window, cx| {
            workspace.open_query("select 1", window, cx);
            let pane = active_pane(workspace);
            let strip = pane.as_u64();
            let from_pane = DraggedTab {
                source: strip,
                index: 0,
            };
            let from_connections = DraggedTab {
                source: CONNECTION_STRIP_TAG,
                index: 0,
            };

            workspace.drag_over_tab(strip, from_pane, Some(1), cx);
            assert_eq!(workspace.tab_drop.indicator(strip), Some(1));
            // Another strip reporting the pointer gone leaves this bar up…
            workspace.drag_over_tab(CONNECTION_STRIP_TAG, from_pane, None, cx);
            assert_eq!(workspace.tab_drop.indicator(strip), Some(1));
            // …and refuses the pane's tab outright.
            workspace.drag_over_tab(CONNECTION_STRIP_TAG, from_pane, Some(0), cx);
            assert_eq!(workspace.tab_drop.current(), Some((strip, 1)));
            // The pane's own `None` takes it down.
            workspace.drag_over_tab(strip, from_pane, None, cx);
            assert_eq!(workspace.tab_drop.current(), None);

            // A connection tab raises no bar over a pane…
            workspace.drag_over_tab(strip, from_connections, Some(0), cx);
            assert_eq!(workspace.tab_drop.current(), None);
            // …and does over its own strip, until the drag is let go.
            workspace.drag_over_tab(CONNECTION_STRIP_TAG, from_connections, Some(0), cx);
            assert_eq!(workspace.tab_drop.indicator(CONNECTION_STRIP_TAG), Some(0));
            workspace.clear_tab_drop(cx);
            assert_eq!(workspace.tab_drop.current(), None);

            // A pane of no work area on screen is no source a pane takes from.
            let stale = PaneTree::single(Pane::new()).first_leaf().0;
            let from_stale = DraggedTab {
                source: stale.as_u64(),
                index: 0,
            };
            workspace.drag_over_tab(strip, from_stale, Some(0), cx);
            assert_eq!(workspace.tab_drop.current(), None);
        })
        .unwrap();
    cx.run_until_parked();
}

/// A drag along the strip reorders it: the dropped tab tucks in before the
/// one it landed on, the tab on top stays the tab on top unless it was the
/// one moved, and a drop that would change nothing is a no-op.
#[gpui::test]
fn moving_a_tab_within_its_strip_reorders_it(cx: &mut gpui::TestAppContext) {
    let (window, _) = workspace_over_h2("move-reorder", cx);
    let mut cx = gpui::VisualTestContext::from_window(window.into(), cx);
    window
        .update(&mut cx, |workspace, window, cx| {
            workspace.open_query("select 1", window, cx);
            workspace.open_query("select 2", window, cx);
            workspace.open_query("select 3", window, cx);
            let pane = active_pane(workspace);
            assert_eq!(
                tab_titles(workspace, pane, cx),
                ["Query 1", "Query 2", "Query 3"]
            );
            assert_eq!(active_tab(workspace, pane), 2);

            // The first tab dropped on the third lands between second and third.
            workspace.move_tab(pane, 0, pane, 2, window, cx);
            assert_eq!(
                tab_titles(workspace, pane, cx),
                ["Query 2", "Query 1", "Query 3"]
            );
            assert_eq!(
                active_tab(workspace, pane),
                2,
                "the top tab did not move, so it stays on top"
            );
            assert_eq!(active_pane(workspace), pane);

            // Onto itself, and before its own neighbour, both change nothing.
            workspace.move_tab(pane, 1, pane, 1, window, cx);
            workspace.move_tab(pane, 1, pane, 2, window, cx);
            assert_eq!(
                tab_titles(workspace, pane, cx),
                ["Query 2", "Query 1", "Query 3"]
            );

            // The tab on top follows when it is the one moved.
            workspace.move_tab(pane, 2, pane, 0, window, cx);
            assert_eq!(
                tab_titles(workspace, pane, cx),
                ["Query 3", "Query 2", "Query 1"]
            );
            assert_eq!(active_tab(workspace, pane), 0);

            // Past the end appends; an unknown pane or tab is refused.
            workspace.move_tab(pane, 0, pane, usize::MAX, window, cx);
            assert_eq!(
                tab_titles(workspace, pane, cx),
                ["Query 2", "Query 1", "Query 3"]
            );
            let stale = PaneTree::single(Pane::new()).first_leaf().0;
            workspace.move_tab(stale, 0, pane, 0, window, cx);
            workspace.move_tab(pane, 0, stale, 0, window, cx);
            workspace.move_tab(pane, 9, pane, 0, window, cx);
            assert_eq!(
                tab_titles(workspace, pane, cx),
                ["Query 2", "Query 1", "Query 3"]
            );
        })
        .unwrap();
    cx.run_until_parked();
}

/// A drag across to another pane carries the tab over: it lands where it was
/// dropped and comes to the top there, the marker follows it, and an emptied
/// source collapses exactly as a closed last tab leaves it.
#[gpui::test]
fn moving_a_tab_to_another_pane_carries_it_over(cx: &mut gpui::TestAppContext) {
    let (window, _) = workspace_over_h2("move-cross-pane", cx);
    let mut cx = gpui::VisualTestContext::from_window(window.into(), cx);
    window
        .update(&mut cx, |workspace, window, cx| {
            workspace.open_query("select 1", window, cx);
            let first = active_pane(workspace);
            workspace.split_active(Axis::Horizontal, window, cx);
            let second = active_pane(workspace);
            assert_ne!(first, second);
            // The split duplicated the showing tab into the new pane.
            assert_eq!(tab_titles(workspace, first, cx), ["Query 1"]);
            assert_eq!(tab_titles(workspace, second, cx), ["Query 2"]);

            // To the front of the other strip, as a drop on its first tab asks.
            workspace.move_tab(first, 0, second, 0, window, cx);
            assert_eq!(tab_titles(workspace, second, cx), ["Query 1", "Query 2"]);
            assert_eq!(active_tab(workspace, second), 0);
            assert_eq!(active_pane(workspace), second);
            // The source held nothing else, so it collapsed away.
            assert_eq!(
                area(workspace).panes.leaf_ids(),
                vec![second],
                "the emptied source pane was not removed"
            );

            // And over the empty stretch — past the end — appends. The split
            // duplicated the showing tab into the new pane under a fresh
            // number, so the moved tab lands behind that copy.
            workspace.split_active(Axis::Horizontal, window, cx);
            let third = active_pane(workspace);
            assert_eq!(tab_titles(workspace, third, cx), ["Query 3"]);
            workspace.move_tab(second, 0, third, usize::MAX, window, cx);
            assert_eq!(tab_titles(workspace, third, cx), ["Query 3", "Query 1"]);
            assert_eq!(active_tab(workspace, third), 1);
            assert_eq!(active_pane(workspace), third);
            assert_eq!(
                tab_titles(workspace, second, cx),
                ["Query 2"],
                "only the moved tab left the source"
            );
        })
        .unwrap();
    cx.run_until_parked();
}

/// The keyboard follows the moved tab when it had it, and stays where it is
/// when a background tab is the one that moved.
#[gpui::test]
fn moving_a_tab_carries_only_the_focus_it_held(cx: &mut gpui::TestAppContext) {
    let (window, _) = workspace_over_h2("move-focus", cx);
    let mut cx = gpui::VisualTestContext::from_window(window.into(), cx);
    window
        .update(&mut cx, |workspace, window, cx| {
            workspace.open_query("select 1", window, cx);
            workspace.open_query("select 2", window, cx);
            let first = active_pane(workspace);
            workspace.split_active(Axis::Horizontal, window, cx);
            let second = active_pane(workspace);
            // Back to the first pane without taking the keyboard along: the
            // split left it on the copy.
            workspace.activate_tab(first, 1, window, cx);
            workspace.focus_active_tab(first, window, cx);
            let moved = match area(workspace)
                .panes
                .get(first)
                .expect("the pane is in the tree")
                .get(1)
            {
                Some(PaneItem::Query { pane, .. }) => pane.clone(),
                other => panic!("the second tab is a query: {other:?}"),
            };
            assert!(
                moved.read(cx).focus_handle(cx).is_focused(window),
                "the setup did not put the keyboard on the tab to be moved"
            );

            // The focused tab moves panes, and the keyboard moves with it.
            workspace.move_tab(first, 1, second, usize::MAX, window, cx);
            assert_eq!(active_pane(workspace), second);
            assert_eq!(active_tab(workspace, second), 1);
            assert!(
                moved.read(cx).focus_handle(cx).is_focused(window),
                "the keyboard did not follow the moved tab"
            );

            // A background tab moving out from under a focused one leaves the
            // keyboard where it was.
            workspace.move_tab(first, 0, second, 0, window, cx);
            assert!(
                moved.read(cx).focus_handle(cx).is_focused(window),
                "moving a background tab stole the keyboard"
            );
            assert_eq!(active_pane(workspace), second);
        })
        .unwrap();
    cx.run_until_parked();
}
