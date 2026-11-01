//! The keyboard's way round the window: tabs, connections, regions, and the
//! focus a dialog gives back.

use super::*;

/// Whether the editor of the query tab on top has the keyboard.
fn editor_focused(workspace: &Workspace, window: &Window, cx: &App) -> bool {
    workspace
        .active_query()
        .expect("a query tab is on top")
        .read(cx)
        .focus_handle(cx)
        .is_focused(window)
}

/// `Ctrl+PgDn`/`PgUp` walk the active pane's strip, wrapping, `Ctrl+1`–`9`
/// jump, and each puts the keyboard in the tab it brings forward.
#[gpui::test]
fn the_tab_keys_walk_the_strip_and_take_the_keyboard_along(cx: &mut gpui::TestAppContext) {
    let (window, _) = workspace_over_h2("keys-tabs", cx);
    let mut cx = gpui::VisualTestContext::from_window(window.into(), cx);
    let pane = window
        .update(&mut cx, |workspace, window, cx| {
            for sql in ["select 1", "select 2", "select 3"] {
                workspace.open_query(sql, window, cx);
            }
            active_pane(workspace)
        })
        .expect("the window is open");
    cx.run_until_parked();

    for (action, expected) in [
        (Box::new(NextTab) as Box<dyn gpui::Action>, 0),
        (Box::new(NextTab), 1),
        (Box::new(PrevTab), 0),
        (Box::new(PrevTab), 2),
        (Box::new(SelectTab(0)), 0),
        (Box::new(SelectTab(8)), 2),
        (Box::new(SelectTab(5)), 2),
    ] {
        let dispatched = action.boxed_clone();
        cx.update(|window, cx| window.dispatch_action(dispatched, cx));
        cx.run_until_parked();
        window
            .update(&mut cx, |workspace, window, cx| {
                assert_eq!(active_tab(workspace, pane), expected, "{action:?}");
                assert!(editor_focused(workspace, window, cx), "{action:?}");
            })
            .expect("the window is open");
    }
}

/// `Ctrl+Alt+PgDn`/`PgUp` walk the connection strip, wrapping.
#[gpui::test]
fn the_connection_keys_walk_the_connection_strip(cx: &mut gpui::TestAppContext) {
    let (window, _) = workspace_over_h2("keys-first", cx);
    let (profile, connected) = h2_connection("keys-second");
    window
        .update(cx, |workspace, window, cx| {
            push_connection(workspace, profile, connected, window, cx);
            assert_eq!(workspace.active_connection, 1);
            workspace.focus_shell(window, cx);
        })
        .expect("the window is open");
    let mut cx = gpui::VisualTestContext::from_window(window.into(), cx);
    for (forward, expected) in [(true, 0), (true, 1), (false, 0)] {
        if forward {
            cx.dispatch_action(NextConnection);
        } else {
            cx.dispatch_action(PrevConnection);
        }
        cx.run_until_parked();
        window
            .update(&mut cx, |workspace, _window, _cx| {
                assert_eq!(workspace.active_connection, expected);
            })
            .expect("the window is open");
    }
}

/// `F6` goes explorer → editor → results and round again, `Shift+F6` the
/// other way; and `Alt+]` takes the keyboard to the pane it moves the frame
/// to.
#[gpui::test]
fn f6_walks_the_regions_and_the_pane_keys_carry_the_keyboard(cx: &mut gpui::TestAppContext) {
    let (window, _) = workspace_over_h2("keys-regions", cx);
    let mut cx = gpui::VisualTestContext::from_window(window.into(), cx);
    let query = window
        .update(&mut cx, |workspace, window, cx| {
            workspace.explorer_visible = true;
            workspace
                .open_query("select 1", window, cx)
                .expect("the tab opens")
        })
        .expect("the window is open");
    cx.run_until_parked();
    window
        .update(&mut cx, |_workspace, window, cx| {
            query.update(cx, |pane, cx| {
                pane.focus_editor(window, cx);
            });
        })
        .expect("the window is open");
    cx.simulate_keystrokes(RUN_ALL);
    cx.run_until_parked();

    let region = |cx: &mut gpui::VisualTestContext| {
        window
            .update(cx, |workspace, window, cx| {
                if workspace
                    .explorer
                    .read(cx)
                    .focus_handle(cx)
                    .contains_focused(window, cx)
                {
                    "explorer"
                } else if query.read(cx).results_hold_focus(window, cx) {
                    "results"
                } else if editor_focused(workspace, window, cx) {
                    "editor"
                } else {
                    "elsewhere"
                }
            })
            .expect("the window is open")
    };
    assert_eq!(region(&mut cx), "editor");
    for expected in ["results", "explorer", "editor"] {
        cx.dispatch_action(NextRegion);
        cx.run_until_parked();
        assert_eq!(region(&mut cx), expected);
    }
    cx.dispatch_action(PrevRegion);
    cx.run_until_parked();
    assert_eq!(region(&mut cx), "explorer");

    // Split, and step back into the first pane: the frame and the keyboard
    // move together.
    window
        .update(&mut cx, |workspace, window, cx| {
            workspace.split_active(Axis::Horizontal, window, cx);
        })
        .expect("the window is open");
    cx.run_until_parked();
    cx.dispatch_action(FocusPrevPane);
    cx.run_until_parked();
    window
        .update(&mut cx, |workspace, window, cx| {
            let first = area(workspace).panes.first_leaf().0;
            assert_eq!(active_pane(workspace), first);
            assert!(
                workspace.pane_holds_focus(first, window, cx),
                "the keyboard stayed behind"
            );
        })
        .expect("the window is open");
}

/// A dialog closed with `Escape` gives the keyboard back to the editor it
/// was taken from, not to the shell.
#[gpui::test]
fn a_dialog_gives_the_keyboard_back_where_it_found_it(cx: &mut gpui::TestAppContext) {
    let (window, _) = workspace_over_h2("keys-restore", cx);
    let mut cx = gpui::VisualTestContext::from_window(window.into(), cx);
    window
        .update(&mut cx, |workspace, window, cx| {
            workspace.open_query("select 1", window, cx);
        })
        .expect("the window is open");
    cx.run_until_parked();
    window
        .update(&mut cx, |workspace, window, cx| {
            assert!(editor_focused(workspace, window, cx));
            workspace.open_about(window, cx);
        })
        .expect("the window is open");
    cx.run_until_parked();
    cx.dispatch_action(DismissDialog);
    cx.run_until_parked();
    window
        .update(&mut cx, |workspace, window, cx| {
            assert!(!workspace.about.read(cx).is_open());
            assert!(
                editor_focused(workspace, window, cx),
                "the keyboard went to the shell"
            );
        })
        .expect("the window is open");
}

/// The write confirmation takes the keyboard from the editor that asked:
/// `Enter` answers the focused "Cancel" rather than breaking the line it was
/// typed after, and `Escape` declines as well, handing the editor back.
#[gpui::test]
fn the_write_confirmation_takes_enter_and_escape_from_the_editor(cx: &mut gpui::TestAppContext) {
    let (window, _) = workspace_over_h2("keys-confirm", cx);
    let mut cx = gpui::VisualTestContext::from_window(window.into(), cx);
    let ask = |cx: &mut gpui::VisualTestContext| {
        window
            .update(cx, |workspace, window, cx| {
                let query = workspace
                    .active_query()
                    .expect("the query is the tab on top")
                    .clone();
                query.update(cx, |pane, cx| pane.focus_editor(window, cx));
                workspace.confirm = Some(PendingConfirm {
                    pane: query,
                    request: Box::new(ConfirmRequest {
                        count: 1,
                        preview: "DELETE FROM PUBLIC.ORDERS".into(),
                    }),
                });
                cx.notify();
            })
            .expect("the window is open");
        cx.run_until_parked();
        window
            .update(cx, |workspace, window, cx| {
                assert!(workspace.confirm_focus.is_focused(window));
                assert!(!editor_focused(workspace, window, cx));
            })
            .expect("the window is open");
    };
    window
        .update(&mut cx, |workspace, window, cx| {
            workspace.open_query("delete from orders", window, cx);
        })
        .expect("the window is open");
    cx.run_until_parked();

    for key in ["enter", "escape"] {
        ask(&mut cx);
        // Down and up, as a real press: a focused button clicks on the
        // release of `Enter`.
        let keystroke = gpui::Keystroke::parse(key).expect("a key");
        cx.simulate_event(gpui::KeyDownEvent {
            keystroke: keystroke.clone(),
            is_held: false,
            prefer_character_input: false,
        });
        cx.simulate_event(gpui::KeyUpEvent { keystroke });
        cx.run_until_parked();
        window
            .update(&mut cx, |workspace, window, cx| {
                assert!(workspace.confirm.is_none(), "{key} answered the question");
                assert!(editor_focused(workspace, window, cx), "{key}");
                let query = workspace.active_query().expect("still on top");
                assert_eq!(
                    query.read(cx).editor_text(cx),
                    "delete from orders",
                    "{key} reached the editor"
                );
            })
            .expect("the window is open");
    }
}

/// A chord that opens a tab says why nothing opens — no connection at all,
/// or one that is not live — rather than ending without a word.
#[gpui::test]
fn opening_a_tab_without_a_session_says_why(cx: &mut gpui::TestAppContext) {
    let window = workspace_over_welcome(&[], cx);
    let mut cx = gpui::VisualTestContext::from_window(window.into(), cx);
    let said = |cx: &mut gpui::VisualTestContext| {
        cx.update(|_, cx| {
            let said = rugpui_shell::shown_toasts(cx)
                .into_iter()
                .map(|(_, toast)| toast.message().to_string())
                .collect::<Vec<_>>();
            rugpui_shell::clear_toasts(cx);
            said
        })
    };
    window
        .update(&mut cx, |workspace, window, cx| {
            workspace.focus_shell(window, cx);
        })
        .expect("the window is open");
    cx.dispatch_action(NewQuery);
    cx.run_until_parked();
    assert_eq!(said(&mut cx), [ts!("toast.no_connection").to_string()]);

    window
        .update(&mut cx, |workspace, window, cx| {
            workspace.connections.push(Connection {
                id: next_connection_id(),
                profile: unopenable_profile("refused"),
                state: ConnectionState::Failed("no driver".into()),
                work: WorkArea::new(),
            });
            workspace.active_connection = 0;
            workspace.focus_shell(window, cx);
            cx.notify();
        })
        .expect("the window is open");
    cx.run_until_parked();
    for action in [
        Box::new(NewQuery) as Box<dyn gpui::Action>,
        Box::new(NewBuilder),
        Box::new(ReopenClosedTab),
    ] {
        let dispatched = action.boxed_clone();
        cx.update(|window, cx| window.dispatch_action(dispatched, cx));
        cx.run_until_parked();
        assert_eq!(
            said(&mut cx),
            [ts!("toast.not_connected", name = "refused").to_string()],
            "{action:?}"
        );
    }
}
