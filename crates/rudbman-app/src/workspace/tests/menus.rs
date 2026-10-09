use super::*;

/// A right-click on a tree row asks the shell for a menu, and what that
/// menu carries follows the kind of node it was raised over: a command a
/// node cannot answer is left out rather than greyed for ever, and the
/// commands that need a scope are greyed on the one node that names none.
#[gpui::test]
fn a_tree_menu_offers_what_its_node_can_answer(cx: &mut gpui::TestAppContext) {
    let (window, connection) = workspace_over_h2("tree-menu", cx);
    let mut cx = gpui::VisualTestContext::from_window(window.into(), cx);
    cx.run_until_parked();

    let table = NodeId::Object {
        connection,
        scope: public(),
        folder: explorer::Folder::Tables,
        name: "PERSON".to_string(),
    };
    let routine = NodeId::Object {
        connection,
        scope: public(),
        folder: explorer::Folder::Procedures,
        name: "DO_IT".to_string(),
    };
    let folder = NodeId::Folder {
        connection,
        scope: public(),
        folder: explorer::Folder::Tables,
    };
    let views = NodeId::Folder {
        connection,
        scope: public(),
        folder: explorer::Folder::Views,
    };
    let schema = NodeId::Schema {
        connection,
        catalog: None,
        name: "PUBLIC".to_string(),
    };
    let root = NodeId::Connection(connection);

    window
        .update(&mut cx, |workspace, _window, cx| {
            // A relation answers everything.
            let rows = workspace.explorer_rows(&table, cx);
            let mut expected = relation_labels();
            expected.extend(scope_labels());
            assert_eq!(context_menu::labels(&rows), expected);
            assert!(context_menu::greyed(&rows).is_empty());

            // A routine is an object and not a relation: `SELECT * FROM` a
            // procedure is not a statement, so those rows are absent rather
            // than greyed.
            assert_eq!(
                context_menu::labels(&workspace.explorer_rows(&routine, cx)),
                scope_labels(),
                "a table is not made inside a procedure either"
            );

            // A schema and the Tables folder under it are the places a
            // table would appear, and the only rows that offer to make one
            // (§7.10).
            let mut with_create = vec![ts!("menu.new_table").to_string()];
            with_create.extend(scope_labels());
            assert_eq!(
                context_menu::labels(&workspace.explorer_rows(&schema, cx)),
                with_create
            );
            assert_eq!(
                context_menu::labels(&workspace.explorer_rows(&folder, cx)),
                with_create
            );
            assert_eq!(
                context_menu::labels(&workspace.explorer_rows(&views, cx)),
                scope_labels(),
                "a CREATE TABLE under Views would put it where it is not listed"
            );

            // The connection root names no scope — a diagram of every
            // catalogue at once is not a diagram — so it keeps the rows and
            // greys them. It offers no new table for the same reason: there
            // is no schema to make one in.
            let rows = workspace.explorer_rows(&root, cx);
            assert_eq!(context_menu::labels(&rows), scope_labels());
            assert_eq!(context_menu::greyed(&rows), scope_labels());
        })
        .expect("the window is open");

    // A connection that never opened greys everything: the tree can still
    // be read, and none of these commands can run without a session.
    window
        .update(&mut cx, |workspace, _window, cx| {
            let id = next_connection_id();
            workspace.connections.push(Connection {
                id,
                profile: unopenable_profile("dead"),
                state: ConnectionState::Failed(SharedString::new_static("refused")),
                work: WorkArea::new(),
            });
            let node = NodeId::Object {
                connection: id,
                scope: public(),
                folder: explorer::Folder::Tables,
                name: "PERSON".to_string(),
            };
            let rows = workspace.explorer_rows(&node, cx);
            let mut expected = relation_labels();
            expected.pop();
            expected.extend(scope_labels());
            assert_eq!(
                context_menu::greyed(&rows),
                expected,
                "a dead connection offered a live command"
            );
        })
        .expect("the window is open");

    // The whole path, from the widget's event to a menu on screen: the
    // explorer promotes it, the shell keeps it, and the frame draws.
    let at = gpui::point(px(120.), px(90.));
    window
        .update(&mut cx, |workspace, _window, cx| {
            workspace.explorer.update(cx, |_explorer, cx| {
                cx.emit(ExplorerEvent::ContextMenu {
                    node: table.clone(),
                    position: at,
                });
            });
        })
        .expect("the window is open");
    cx.run_until_parked();
    window
        .update(&mut cx, |workspace, _window, _cx| {
            let menu = workspace.context_menu.as_ref().expect("a menu is open");
            assert_eq!(menu.position, at);
            assert!(matches!(&menu.target, ContextTarget::Explorer(node) if **node == table));
        })
        .expect("the window is open");

    // An error row names nothing — it is the sentence saying why its parent
    // could not be read — so it gets no menu at all, and the one that was
    // open is left where it is rather than replaced by an empty panel.
    window
        .update(&mut cx, |workspace, _window, cx| {
            workspace.context_menu = None;
            workspace.explorer.update(cx, |_explorer, cx| {
                cx.emit(ExplorerEvent::ContextMenu {
                    node: NodeId::Error(Box::new(table.clone())),
                    position: at,
                });
            });
        })
        .expect("the window is open");
    cx.run_until_parked();
    window
        .update(&mut cx, |workspace, _window, _cx| {
            assert!(
                workspace.context_menu.is_none(),
                "an error row was given a menu of greyed rows"
            );
        })
        .expect("the window is open");
}

/// "Close the other tabs" is a command with no gesture behind it, and the
/// keyboard has to end up in the tab that stays: the strip is a place a
/// user closes several tabs in a row from, and a focus that fell back to
/// the shell would swallow the editor shortcuts in between.
#[gpui::test]
fn closing_the_other_tabs_leaves_the_keyboard_in_the_one_that_stays(cx: &mut gpui::TestAppContext) {
    let (window, _connection) = workspace_over_h2("tab-menu", cx);
    let mut cx = gpui::VisualTestContext::from_window(window.into(), cx);

    let pane = window
        .update(&mut cx, |workspace, window, cx| {
            for sql in ["select 1", "select 2", "select 3"] {
                workspace.open_query(sql, window, cx);
            }
            workspace.active_pane().expect("a work area is open")
        })
        .expect("the window is open");
    cx.run_until_parked();

    window
        .update(&mut cx, |workspace, _window, cx| {
            assert_eq!(tab_titles(workspace, pane, cx).len(), 3);
            assert_eq!(active_tab(workspace, pane), 2);
        })
        .expect("the window is open");

    // Raised over the *first* tab, which is not the one on top: a right
    // click selects no tab, so the menu has to act on what was pressed.
    let rows = window
        .update(&mut cx, |workspace, _window, cx| {
            workspace.pane_tab_rows(pane, 0, cx)
        })
        .expect("the window is open");
    assert_eq!(
        context_menu::labels(&rows),
        [
            ts!("context.close_tab").to_string(),
            ts!("context.close_others").to_string(),
            ts!("context.close_right").to_string(),
            String::new(),
            ts!("context.split_right").to_string(),
            ts!("context.split_below").to_string(),
            ts!("context.close_pane").to_string(),
        ]
    );
    assert!(
        !context_menu::row(&rows, &ts!("context.close_pane")).is_enabled(),
        "the last pane of a work area was offered for closing"
    );

    cx.update(|window, cx| {
        context_menu::row(&rows, &ts!("context.close_others")).activate(window, cx);
    });
    cx.run_until_parked();

    window
        .update(&mut cx, |workspace, window, cx| {
            assert_eq!(tab_titles(workspace, pane, cx).len(), 1);
            assert_eq!(active_tab(workspace, pane), 0);
            let editor = workspace
                .active_query()
                .expect("the tab that stayed is the query pane")
                .read(cx)
                .focus_handle(cx);
            assert!(
                editor.is_focused(window),
                "the keyboard was left on an editor nothing renders"
            );

            // And with one tab left, both of the multi-tab rows say so.
            let rows = workspace.pane_tab_rows(pane, 0, cx);
            assert!(!context_menu::row(&rows, &ts!("context.close_others")).is_enabled());
            assert!(!context_menu::row(&rows, &ts!("context.close_right")).is_enabled());
        })
        .expect("the window is open");
}

/// The tabs to the right go and the ones to the left stay, whichever tab is
/// on top.
#[gpui::test]
fn closing_the_tabs_to_the_right_keeps_the_ones_before_them(cx: &mut gpui::TestAppContext) {
    let (window, _connection) = workspace_over_h2("tab-menu-right", cx);
    let mut cx = gpui::VisualTestContext::from_window(window.into(), cx);

    let pane = window
        .update(&mut cx, |workspace, window, cx| {
            for sql in ["select 1", "select 2", "select 3"] {
                workspace.open_query(sql, window, cx);
            }
            workspace.active_pane().expect("a work area is open")
        })
        .expect("the window is open");
    cx.run_until_parked();

    let before = window
        .update(&mut cx, |workspace, _window, cx| {
            tab_titles(workspace, pane, cx)
        })
        .expect("the window is open");

    let rows = window
        .update(&mut cx, |workspace, _window, cx| {
            workspace.pane_tab_rows(pane, 1, cx)
        })
        .expect("the window is open");
    cx.update(|window, cx| {
        context_menu::row(&rows, &ts!("context.close_right")).activate(window, cx);
    });
    cx.run_until_parked();

    window
        .update(&mut cx, |workspace, _window, cx| {
            assert_eq!(tab_titles(workspace, pane, cx), before[..2]);
            assert_eq!(
                active_tab(workspace, pane),
                1,
                "the last tab left is on top"
            );
        })
        .expect("the window is open");
}

/// A connection tab's menu acts on the tab that was pressed rather than on
/// the one showing, and greys what needs a session.
#[gpui::test]
fn a_connection_tab_menu_acts_on_the_tab_it_was_raised_over(cx: &mut gpui::TestAppContext) {
    let (window, _connection) = workspace_over_h2("conn-menu", cx);
    let mut cx = gpui::VisualTestContext::from_window(window.into(), cx);

    window
        .update(&mut cx, |workspace, _window, cx| {
            workspace.connections.push(Connection {
                id: next_connection_id(),
                profile: unopenable_profile("refused"),
                state: ConnectionState::Failed(SharedString::new_static("no driver")),
                work: WorkArea::new(),
            });

            let live = workspace.connection_rows(0, cx);
            assert_eq!(
                context_menu::labels(&live),
                [
                    ts!("menu.new_query").to_string(),
                    ts!("menu.new_builder").to_string(),
                    String::new(),
                    ts!("tab.close").to_string(),
                ]
            );
            assert!(context_menu::greyed(&live).is_empty());

            let dead = workspace.connection_rows(1, cx);
            assert!(!context_menu::row(&dead, &ts!("menu.new_query")).is_enabled());
            assert!(!context_menu::row(&dead, &ts!("menu.new_builder")).is_enabled());
            assert!(
                context_menu::row(&dead, &ts!("tab.close")).is_enabled(),
                "a tab that failed to open still has to be closable"
            );
        })
        .expect("the window is open");

    // Closing acts on the pressed tab, not on the one on screen.
    let rows = window
        .update(&mut cx, |workspace, _window, cx| {
            workspace.connection_rows(1, cx)
        })
        .expect("the window is open");
    cx.update(|window, cx| {
        context_menu::row(&rows, &ts!("tab.close")).activate(window, cx);
    });
    cx.run_until_parked();
    window
        .update(&mut cx, |workspace, _window, _cx| {
            assert_eq!(workspace.connections.len(), 1);
            assert_eq!(workspace.active_connection, 0);
        })
        .expect("the window is open");
}

/// The welcome list's menu offers the two things there are to do with a
/// saved connection, and "edit…" opens the dialog rather than a session.
#[gpui::test]
fn the_welcome_menu_opens_the_dialog_over_the_row(cx: &mut gpui::TestAppContext) {
    let saved = unopenable_profile("saved");
    let window = workspace_over_welcome(std::slice::from_ref(&saved), cx);
    let mut cx = gpui::VisualTestContext::from_window(window.into(), cx);
    cx.run_until_parked();

    let at = gpui::point(px(60.), px(200.));
    window
        .update(&mut cx, |workspace, _window, cx| {
            workspace.open_context_menu(ContextTarget::Profile(saved.id), at, cx);
        })
        .expect("the window is open");
    cx.run_until_parked();

    let rows = window
        .update(&mut cx, |workspace, _window, cx| {
            workspace.profile_rows(saved.id, cx)
        })
        .expect("the window is open");
    assert_eq!(
        context_menu::labels(&rows),
        [
            ts!("context.connect").to_string(),
            String::new(),
            ts!("context.edit").to_string(),
        ]
    );

    cx.update(|window, cx| {
        context_menu::row(&rows, &ts!("context.edit")).activate(window, cx);
    });
    cx.run_until_parked();

    window
        .update(&mut cx, |workspace, _window, cx| {
            assert!(
                workspace.connect.read(cx).is_open(),
                "the row did not open the dialog"
            );
            assert!(
                workspace.connections.is_empty(),
                "editing a profile opened a session"
            );
            // Opening a dialog closes the menu that led to it, the way
            // every other overlay does.
            assert!(workspace.context_menu.is_none());
        })
        .expect("the window is open");
}

/// `Escape` closes the context menu before it closes anything else — the
/// shell's own and the panes' alike — and leaves the dialog stack exactly
/// as it was, so the next press finds it.
#[gpui::test]
fn escape_closes_the_context_menu_before_the_dialog_under_it(cx: &mut gpui::TestAppContext) {
    let (window, connection) = workspace_over_h2("escape-menu", cx);
    let mut cx = gpui::VisualTestContext::from_window(window.into(), cx);

    let query = window
        .update(&mut cx, |workspace, window, cx| {
            workspace.open_query("select 1", window, cx);
            workspace.open_about(window, cx);
            workspace.open_context_menu(
                ContextTarget::Connection(0),
                gpui::point(px(30.), px(10.)),
                cx,
            );
            workspace.active_query().expect("the pane is open").clone()
        })
        .expect("the window is open");
    // A pane menu of its own, which the shell has to reach as well: a right
    // click moves no pane marker, so the pane holding one is not
    // necessarily the active one.
    window
        .update(&mut cx, |_workspace, _window, cx| {
            query.update(cx, |pane, cx| {
                pane.open_editor_menu(gpui::point(px(40.), px(80.)), cx);
            });
        })
        .expect("the window is open");
    cx.run_until_parked();

    cx.dispatch_action(DismissDialog);
    window
        .update(&mut cx, |workspace, _window, cx| {
            assert!(workspace.context_menu.is_none(), "the shell menu stayed");
            assert!(
                !query.read(cx).has_context_menu(),
                "the pane's own menu was left behind the one the shell owns"
            );
            assert!(
                workspace.about.read(cx).is_open(),
                "the dialog under the menu went with it"
            );
        })
        .expect("the window is open");

    // And the next press finds the dialog, which is the whole point of the
    // menu going first rather than instead.
    cx.dispatch_action(DismissDialog);
    window
        .update(&mut cx, |workspace, _window, cx| {
            assert!(!workspace.about.read(cx).is_open());
        })
        .expect("the window is open");

    // Opening the application dropdown puts a context menu away, because
    // both of them lay a full-window backdrop.
    window
        .update(&mut cx, |workspace, _window, cx| {
            workspace.open_context_menu(
                ContextTarget::Explorer(Box::new(NodeId::Connection(connection))),
                gpui::point(px(30.), px(10.)),
                cx,
            );
            workspace.set_menu_open(true, cx);
            assert!(workspace.context_menu.is_none());
            assert!(workspace.menu_open);
        })
        .expect("the window is open");
}
