use super::*;

/// The end of the M1 thread, in one test: a real H2 session opens, a tab
/// appears carrying the profile's name and a "connected" dot, the status bar
/// names the product and version the driver reported, and closing the tab
/// takes the session with it.
///
/// The session is opened before the window is built so that the blocking
/// call is nowhere near a gpui update — which is also the rule the shell
/// itself follows, by way of `background_spawn`.
#[gpui::test]
fn a_real_connection_reaches_the_tab_strip_and_the_status_bar(cx: &mut gpui::TestAppContext) {
    let profile = connection::h2::profile("workspace");
    let connected = connection::connect(
        &profile,
        &connection::h2::driver(),
        &connection::Credentials::typed(Some(String::new()), None),
        &AppSettings::default(),
    )
    .expect("H2 opens an in-memory database without a server");
    let product = connected.product().expect("H2 names itself");

    cx.update(|cx| {
        app_settings::init(cx);
        rugpui::init(cx);
    });
    let window = cx.add_window(|window, cx| Workspace::new(TitlebarStyle::Custom, window, cx));

    window
        .update(cx, |workspace, window, cx| {
            // Nothing open yet: both cells say so.
            let (name, state) = workspace.status_cells();
            assert_eq!(name, ts!("statusbar.no_connection"));
            assert_eq!(state, ts!("statusbar.idle"));

            workspace.connections.push(Connection {
                id: next_connection_id(),
                profile: profile.clone(),
                state: ConnectionState::Open(Box::new(connected)),
                work: WorkArea::new(),
            });
            workspace.active_connection = 0;

            let connection = workspace.active_connection().expect("one tab");
            assert_eq!(connection.profile.name, "workspace");
            assert_eq!(connection.state.tab_status(), TabStatus::Connected);

            let (name, state) = workspace.status_cells();
            assert_eq!(name, SharedString::from(product.clone()));
            assert!(name.starts_with("H2 "), "{name}");
            assert_eq!(state, ts!("statusbar.connected"));

            // Closing the tab hands the session to a background task that
            // closes it; the tab is gone either way.
            workspace.close_connection(0, window, cx);
            assert!(workspace.connections.is_empty());
            let (name, state) = workspace.status_cells();
            assert_eq!(name, ts!("statusbar.no_connection"));
            assert_eq!(state, ts!("statusbar.idle"));
        })
        .expect("the window is open");
    cx.run_until_parked();
}

/// A failed attempt lands in the tab rather than in a log nobody reads.
#[gpui::test]
fn a_refused_connection_shows_the_drivers_own_message(cx: &mut gpui::TestAppContext) {
    let mut profile = connection::h2::profile("refused");
    profile.url = format!("{};DB_CLOSE_DELAY=-1", profile.url);
    let created = connection::connect(
        &profile,
        &connection::h2::driver(),
        &connection::Credentials::typed(Some("hunter2".into()), None),
        &AppSettings::default(),
    )
    .expect("the first connection creates the database");

    let error = connection::connect(
        &profile,
        &connection::h2::driver(),
        &connection::Credentials::typed(Some("s3cr3t-pa55w0rd".into()), None),
        &AppSettings::default(),
    )
    .expect_err("a wrong password must be refused");
    assert!(error.is_authentication(), "{error}");

    cx.update(|cx| {
        app_settings::init(cx);
        rugpui::init(cx);
    });
    let window = cx.add_window(|window, cx| Workspace::new(TitlebarStyle::Custom, window, cx));

    window
        .update(cx, |workspace, _window, _cx| {
            workspace.connections.push(Connection {
                id: next_connection_id(),
                profile,
                state: ConnectionState::Failed(error.message().into()),
                work: WorkArea::new(),
            });
            workspace.active_connection = 0;

            assert_eq!(
                workspace
                    .active_connection()
                    .expect("one tab")
                    .state
                    .tab_status(),
                TabStatus::Error
            );
            let (name, state) = workspace.status_cells();
            // The profile's name, since there is no product to report.
            assert_eq!(name, "refused");
            // The driver's own words, and no password in them.
            assert!(state.len() > ts!("statusbar.connected").len(), "{state}");
            assert!(
                !state.contains("s3cr3t-pa55w0rd") && !state.contains("hunter2"),
                "the refused password reached the status bar: {state}"
            );
        })
        .expect("the window is open");

    created.close().expect("close");
    cx.run_until_parked();
}

/// Hiding the sidebar has to bring the keyboard back with it.
///
/// The explorer's tree focuses itself when a row is clicked, and gpui never
/// clears the window focus when the focused element stops being rendered:
/// it resolves a dispatched action against the focused element of the last
/// drawn frame and falls back to the window root, which carries none of the
/// workspace's handlers, when that element has gone. So without
/// [`Workspace::reclaim_focus`] the first `ToggleExplorer` hides the panel
/// and every action after it — the menu rows and the shortcuts alike — is
/// dropped without a trace. The second dispatch below is that bug.
#[gpui::test]
fn hiding_the_explorer_takes_the_focus_back(cx: &mut gpui::TestAppContext) {
    cx.update(|cx| {
        app_settings::init(cx);
        rugpui::init(cx);
    });
    let window = cx.add_window(|window, cx| Workspace::new(TitlebarStyle::Custom, window, cx));
    // The setting on disk decides how the sidebar starts, and this is about
    // hiding one that is showing — which takes a connection as well as the
    // preference, since the welcome screen has the window to itself; see
    // [`Workspace::explorer_showing`]. The tab needs no session behind it:
    // what is being tested is the panel, and a failed connection renders one
    // exactly as a live one does.
    window
        .update(cx, |workspace, _window, cx| {
            workspace.connections.push(Connection {
                id: next_connection_id(),
                profile: connection::h2::profile("explorer-focus"),
                state: ConnectionState::Failed("no driver".into()),
                work: WorkArea::new(),
            });
            workspace.active_connection = 0;
            workspace.explorer_visible = true;
            cx.notify();
        })
        .expect("the window is open");

    let mut cx = gpui::VisualTestContext::from_window(window.into(), cx);
    cx.run_until_parked();

    // What clicking a row in the tree amounts to, without the mouse.
    window
        .update(&mut cx, |workspace, window, cx| {
            let handle = workspace.explorer.read(cx).focus_handle(cx);
            handle.focus(window, cx);
        })
        .expect("the window is open");
    cx.run_until_parked();

    // Through `Window::dispatch_action`, which is the path both the menu row
    // and the keyboard shortcut take.
    cx.dispatch_action(ToggleExplorer);
    window
        .update(&mut cx, |workspace, window, _cx| {
            assert!(!workspace.explorer_visible, "the sidebar is still showing");
            assert!(
                workspace.focus_handle.is_focused(window),
                "the focus is still on the hidden sidebar"
            );
        })
        .expect("the window is open");

    // The regression: with the focus stranded, this one never arrives.
    cx.dispatch_action(ToggleExplorer);
    window
        .update(&mut cx, |workspace, _window, _cx| {
            assert!(
                workspace.explorer_visible,
                "the second toggle was dropped: the sidebar cannot be brought back"
            );
        })
        .expect("the window is open");
}

/// With nothing open the welcome screen has the window to itself: no
/// sidebar beside it, whatever the stored preference says — and the
/// preference untouched, so the first connection brings the panel back
/// without the user having to ask for it again.
#[gpui::test]
fn the_welcome_screen_has_the_window_to_itself(cx: &mut gpui::TestAppContext) {
    let saved = unopenable_profile("saved");
    let window = workspace_over_welcome(std::slice::from_ref(&saved), cx);
    window
        .update(cx, |workspace, _window, cx| {
            workspace.explorer_visible = true;
            cx.notify();
        })
        .expect("the window is open");
    let mut cx = gpui::VisualTestContext::from_window(window.into(), cx);
    cx.run_until_parked();

    // Nothing but the welcome screen draws a saved profile, so the row's
    // bounds are the assertion that the welcome screen is what the body
    // drew — and it is this test's own profile, not whatever the machine
    // running it happens to have in `connections.json`.
    assert!(
        cx.debug_bounds(row_selector(&saved)).is_some(),
        "the saved connections were not drawn"
    );

    window
        .update(&mut cx, |workspace, _window, cx| {
            assert!(
                !workspace.explorer_showing(),
                "the sidebar was drawn beside the welcome screen"
            );
            assert!(
                workspace.explorer_visible,
                "the welcome screen wrote to the user's preference"
            );

            // A tab, and the panel comes back on the preference that was
            // never touched.
            workspace.connections.push(Connection {
                id: next_connection_id(),
                profile: unopenable_profile("open"),
                state: ConnectionState::Failed("no driver".into()),
                work: WorkArea::new(),
            });
            workspace.active_connection = 0;
            workspace.sync_visible_root(cx);
            assert!(
                workspace.explorer_showing(),
                "the sidebar did not come back with the first connection"
            );
            // And the welcome screen is not what the body draws any more:
            // there is a work area now, and that is what stands in its
            // place.
            assert!(workspace.work_area().is_some());
        })
        .expect("the window is open");
    cx.run_until_parked();
}

/// Clicking a saved connection opens it, with no form in between.
///
/// The profile was filled in when it was saved; putting the dialog up over
/// one the user has just picked would be a form to dismiss between them and
/// their database. Nothing is checked ahead of the attempt either — the tab
/// is where a missing driver is reported, exactly as it is for a refused
/// password.
#[gpui::test]
fn clicking_a_saved_connection_opens_it_with_nothing_in_between(cx: &mut gpui::TestAppContext) {
    let profile = unopenable_profile("staging");
    let window = workspace_over_welcome(&[unopenable_profile("production"), profile.clone()], cx);
    let mut cx = gpui::VisualTestContext::from_window(window.into(), cx);
    cx.run_until_parked();

    // The second row, so that a handler wired to the wrong profile shows up
    // as the wrong tab rather than passing by luck.
    let row = cx
        .debug_bounds(row_selector(&profile))
        .expect("both saved connections are drawn");
    cx.simulate_click(row.center(), gpui::Modifiers::none());
    cx.run_until_parked();

    window
        .update(&mut cx, |workspace, window, cx| {
            assert_eq!(workspace.connections.len(), 1, "one click, one tab");
            let open = workspace.active_connection().expect("the tab is on top");
            assert_eq!(open.profile.id, profile.id, "another profile was opened");
            assert!(
                !workspace.connect.read(cx).is_open(),
                "a dialog came up between the click and the connection"
            );
            match &open.state {
                ConnectionState::Failed(message) => assert!(
                    message.contains(MISSING_DRIVER),
                    "the attempt stopped before the driver lookup: {message}"
                ),
                _ => panic!("the attempt did not reach the driver lookup"),
            }
            // The row that was clicked is not rendered any more, so the
            // keyboard must not still be on it.
            assert!(
                workspace.focus_handle.is_focused(window),
                "the focus was left on the welcome screen"
            );
        })
        .expect("the window is open");

    // The regression that rule exists for: with the focus stranded, this
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

/// The welcome screen's button is the same command as the menu row.
#[gpui::test]
fn the_welcome_button_opens_the_connection_dialog(cx: &mut gpui::TestAppContext) {
    let window = workspace_over_welcome(&[], cx);
    let mut cx = gpui::VisualTestContext::from_window(window.into(), cx);
    cx.run_until_parked();

    // Nothing saved, so the screen is the heading, the button, and a line of
    // words where the list would be.
    let button = cx
        .debug_bounds(WELCOME_NEW_SELECTOR)
        .expect("the button is drawn");
    cx.simulate_click(button.center(), gpui::Modifiers::none());
    cx.run_until_parked();

    window
        .update(&mut cx, |workspace, _window, cx| {
            assert!(
                workspace.connect.read(cx).is_open(),
                "the button did not open the connection dialog"
            );
        })
        .expect("the window is open");
}

/// Closing the last tab comes back to the welcome screen, and the keyboard
/// has to come back with it.
///
/// The sidebar goes off screen the moment the last connection does — see
/// [`Workspace::explorer_showing`] — which is the same hazard as hiding it
/// by hand: a focus left on the tree resolves to the window root, which
/// carries none of the workspace's handlers, and every action after it is
/// dropped without a trace.
#[gpui::test]
fn closing_the_last_tab_takes_the_focus_back_from_the_sidebar(cx: &mut gpui::TestAppContext) {
    let window = workspace_over_welcome(&[], cx);
    window
        .update(cx, |workspace, _window, cx| {
            workspace.connections.push(Connection {
                id: next_connection_id(),
                profile: unopenable_profile("last"),
                state: ConnectionState::Failed("no driver".into()),
                work: WorkArea::new(),
            });
            workspace.active_connection = 0;
            workspace.explorer_visible = true;
            workspace.sync_explorer_root(0, cx);
            workspace.sync_visible_root(cx);
        })
        .expect("the window is open");
    let mut cx = gpui::VisualTestContext::from_window(window.into(), cx);
    cx.run_until_parked();

    // What clicking a row in the tree amounts to, without the mouse.
    window
        .update(&mut cx, |workspace, window, cx| {
            let handle = workspace.explorer.read(cx).focus_handle(cx);
            handle.focus(window, cx);
        })
        .expect("the window is open");
    cx.run_until_parked();

    window
        .update(&mut cx, |workspace, window, cx| {
            workspace.close_connection(0, window, cx);
            assert!(workspace.connections.is_empty());
            assert!(
                !workspace.explorer_showing(),
                "the sidebar outlived the last connection"
            );
            assert!(
                workspace.focus_handle.is_focused(window),
                "the focus was left on a sidebar nothing renders"
            );
        })
        .expect("the window is open");
    cx.run_until_parked();

    // The regression: with the focus stranded, this never arrives, and the
    // window is inert from the welcome screen on.
    cx.dispatch_action(NewConnection);
    window
        .update(&mut cx, |workspace, _window, cx| {
            assert!(
                workspace.connect.read(cx).is_open(),
                "the action was dropped: the focus is on something unrendered"
            );
        })
        .expect("the window is open");
}

/// The welcome list is re-read when the dialog closes.
///
/// The dialog is the only thing that edits `connections.json`, and it may
/// have saved, renamed or deleted a profile while it was up; a list left as
/// it was would offer a profile that is gone, or hide one just made.
#[gpui::test]
fn the_welcome_list_follows_what_the_dialog_did(cx: &mut gpui::TestAppContext) {
    let stale = unopenable_profile("stale");
    let window = workspace_over_welcome(std::slice::from_ref(&stale), cx);
    let mut cx = gpui::VisualTestContext::from_window(window.into(), cx);
    cx.run_until_parked();

    window
        .update(&mut cx, |workspace, _window, cx| {
            // Emitted by the dialog on its way out, whichever way it went.
            workspace
                .connect
                .update(cx, |_dialog, cx| cx.emit(ConnectionDialogEvent::Dismissed));
        })
        .expect("the window is open");
    cx.run_until_parked();

    window
        .update(&mut cx, |workspace, _window, _cx| {
            // Whatever is on disk — an empty store on a machine that has
            // never saved a profile — but never the list from before.
            let ids: Vec<_> = workspace
                .profiles
                .connections()
                .iter()
                .map(|profile| profile.id)
                .collect();
            let disk: Vec<_> = load_profiles()
                .connections()
                .iter()
                .map(|profile| profile.id)
                .collect();
            assert_eq!(ids, disk, "the list was not re-read");
            assert!(
                workspace.profiles.get(stale.id).is_none(),
                "the list the dialog opened over survived it"
            );
        })
        .expect("the window is open");
}

/// The same hazard one pane down: closing a pane whose editor has the
/// keyboard must not leave the focus on an editor nothing renders.
#[gpui::test]
fn closing_a_focused_pane_takes_the_focus_back(cx: &mut gpui::TestAppContext) {
    let profile = connection::h2::profile("panes");
    let connected = connection::connect(
        &profile,
        &connection::h2::driver(),
        &connection::Credentials::typed(Some(String::new()), None),
        &AppSettings::default(),
    )
    .expect("H2 opens an in-memory database without a server");

    cx.update(|cx| {
        app_settings::init(cx);
        rugpui::init(cx);
        rugpui_editor::init(cx);
    });
    let window = cx.add_window(|window, cx| Workspace::new(TitlebarStyle::Custom, window, cx));
    window
        .update(cx, |workspace, window, cx| {
            workspace.connections.push(Connection {
                id: next_connection_id(),
                profile: profile.clone(),
                state: ConnectionState::Open(Box::new(connected)),
                work: WorkArea::new(),
            });
            workspace.active_connection = 0;
            // Two panes, because the last one is never closed.
            workspace.split_active(Axis::Horizontal, cx);
            // Opens in the new pane and puts the caret in its editor.
            workspace.open_query("SELECT 1", window, cx);
        })
        .expect("the window is open");

    let mut cx = gpui::VisualTestContext::from_window(window.into(), cx);
    cx.run_until_parked();

    window
        .update(&mut cx, |workspace, window, cx| {
            let editor = workspace
                .active_query()
                .expect("the new pane holds the editor")
                .read(cx)
                .focus_handle(cx);
            assert!(editor.is_focused(window), "the editor did not take focus");
            workspace.close_active_pane(window, cx);
            assert!(
                workspace.focus_handle.is_focused(window),
                "the focus stayed on the editor of the closed pane"
            );
        })
        .expect("the window is open");
    cx.run_until_parked();
}
