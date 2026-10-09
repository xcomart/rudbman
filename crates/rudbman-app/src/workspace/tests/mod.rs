use super::*;
use crate::connection_dialog;

/// A stand-in registry listing: the six built-in ids, each of which a
/// chrome theme shares a name with, plus one dark theme of the user's own
/// that none does — which is the case the rule below has to keep straight.
fn entries() -> Vec<EditorThemeEntry> {
    [
        ("one-dark", true, true),
        ("one-light", false, true),
        ("solarized-dark", true, true),
        ("solarized-light", false, true),
        ("gruvbox-dark", true, true),
        ("dracula", true, true),
        ("tokyo-night", true, false),
    ]
    .into_iter()
    .map(|(id, dark, builtin)| EditorThemeEntry {
        id: id.to_string(),
        name: id.to_string(),
        dark,
        builtin,
    })
    .collect()
}

/// The debug selector of one profile's row, as `debug_bounds` wants it.
///
/// That takes a `&'static str` and the id is only known at run time, so the
/// string is leaked: a handful of bytes for the length of a test process.
fn row_selector(profile: &ConnectionProfile) -> &'static str {
    Box::leak(format!("{}{}", connection_dialog::ROW_SELECTOR, profile.id).into_boxed_str())
}

/// A driver id no `drivers.json` defines, and none ever will.
///
/// Opening a profile that names it takes [`Workspace::open_connection`]'s
/// no-driver path, which is the one outcome that does not depend on what the
/// machine running the test has installed — and it still produces the tab
/// these tests are about, with the reason in it.
const MISSING_DRIVER: &str = "no-such-driver.welcome-test";

/// A saved profile that opens into a tab and no further.
fn unopenable_profile(name: &str) -> ConnectionProfile {
    ConnectionProfile::new(name, MISSING_DRIVER, "jdbc:rudbman:none", "sa")
}

/// A window showing the welcome screen, with `profiles` saved behind it.
///
/// The store is set directly rather than written to `connections.json`: the
/// file is the user's own, and what these tests are about is what the shell
/// does with the list once it has one.
fn workspace_over_welcome(
    profiles: &[ConnectionProfile],
    cx: &mut gpui::TestAppContext,
) -> gpui::WindowHandle<Workspace> {
    cx.update(|cx| {
        app_settings::init(cx);
        rugpui::init(cx);
    });
    let window = cx.add_window(|window, cx| Workspace::new(TitlebarStyle::Custom, window, cx));
    window
        .update(cx, |workspace, _window, cx| {
            let mut store = ConnectionStore::default();
            for profile in profiles {
                store.upsert(profile.clone());
            }
            workspace.profiles = store;
            cx.notify();
        })
        .expect("the window is open");
    window
}

/// A live H2 session and the profile it was opened from.
///
/// Opened outside every gpui update on purpose, as each test here does: the
/// call blocks, and the rule the shell itself follows is that nothing
/// blocking runs inside an update.
fn h2_connection(name: &str) -> (ConnectionProfile, Connected) {
    let profile = connection::h2::profile(name);
    let connected = connection::connect(
        &profile,
        &connection::h2::driver(),
        &connection::Credentials::typed(Some(String::new()), None),
        &AppSettings::default(),
    )
    .expect("H2 opens an in-memory database without a server");
    (profile, connected)
}

/// Pushes a live connection tab, brings it to the front, and puts the
/// explorer in step with it — everything [`Workspace::open_connection`] does
/// once the handshake is in, minus the handshake.
fn push_connection(
    workspace: &mut Workspace,
    profile: ConnectionProfile,
    connected: Connected,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) -> ConnectionId {
    let id = next_connection_id();
    let index = workspace.connections.len();
    workspace.connections.push(Connection {
        id,
        profile,
        state: ConnectionState::Open(Box::new(connected)),
        work: WorkArea::new(),
    });
    workspace.select_connection(index, window, cx);
    workspace.sync_explorer_root(index, cx);
    workspace.sync_visible_root(cx);
    id
}

/// A window with one live H2 connection in its tab strip, and the id that
/// connection was given.
fn workspace_over_h2(
    name: &str,
    cx: &mut gpui::TestAppContext,
) -> (gpui::WindowHandle<Workspace>, ConnectionId) {
    workspace_over_h2_with(name, &[], cx)
}

/// The same, with `setup` already run against the database.
///
/// For the tabs that fetch something of their own: a pane over a table
/// that does not exist would be asserting about an error message rather
/// than about the rows.
fn workspace_over_h2_with(
    name: &str,
    setup: &[&str],
    cx: &mut gpui::TestAppContext,
) -> (gpui::WindowHandle<Workspace>, ConnectionId) {
    let (profile, connected) = h2_connection(name);
    for sql in setup {
        connected
            .session()
            .execute(&rudbman_jdbc::StatementSpec::new(*sql))
            .unwrap_or_else(|error| panic!("{sql}: {error}"));
    }

    cx.update(|cx| {
        app_settings::init(cx);
        rugpui::init(cx);
        rugpui_editor::init(cx);
        rugpui_grid::init(cx);
    });
    let window = cx.add_window(|window, cx| Workspace::new(TitlebarStyle::Custom, window, cx));
    let id = window
        .update(cx, |workspace, window, cx| {
            push_connection(workspace, profile, connected, window, cx)
        })
        .expect("the window is open");
    (window, id)
}

/// The work area on screen, which every assertion about panes goes through.
fn area(workspace: &Workspace) -> &WorkArea {
    workspace.work_area().expect("a connection tab is open")
}

/// The scope of `connection`'s public schema, which is what a diagram is
/// drawn over.
fn erd_target(connection: ConnectionId) -> ErdTarget {
    ErdTarget {
        connection,
        scope: explorer::Scope {
            catalog: None,
            schema: Some("PUBLIC".to_string()),
        },
    }
}

/// An object of `connection` in the public schema.
fn object(connection: ConnectionId, name: &str) -> ObjectTarget {
    ObjectTarget {
        connection,
        catalog: None,
        schema: Some("PUBLIC".to_string()),
        folder: explorer::Folder::Tables,
        name: name.to_string(),
    }
}

/// The titles of one pane's tabs, in strip order.
fn tab_titles(workspace: &Workspace, pane: PaneId, cx: &App) -> Vec<String> {
    area(workspace)
        .panes
        .get(pane)
        .expect("the pane is in the tree")
        .items()
        .iter()
        .map(|item| item.title(cx).to_string())
        .collect()
}

/// Which tab of one pane is on top.
fn active_tab(workspace: &Workspace, pane: PaneId) -> usize {
    area(workspace)
        .panes
        .get(pane)
        .expect("the pane is in the tree")
        .active_index()
}

/// The active pane of the work area on screen.
fn active_pane(workspace: &Workspace) -> PaneId {
    workspace.active_pane().expect("a connection tab is open")
}

/// The chord the SQL editor binds "run everything" to.
///
/// Follows `rugpui_editor::init`, which is what the test harness
/// registers; the action itself is that crate's and is not exported.
const RUN_ALL: &str = if cfg!(target_os = "macos") {
    "cmd-shift-enter"
} else {
    "ctrl-shift-enter"
};

/// Selects one table in the explorer, which is what the builder's action
/// reads.
fn select_table(
    window: &gpui::WindowHandle<Workspace>,
    cx: &mut gpui::VisualTestContext,
    connection: ConnectionId,
    schema: &str,
    name: &str,
) {
    window
        .update(cx, |workspace, _window, cx| {
            workspace.explorer.update(cx, |explorer, cx| {
                explorer.select(
                    NodeId::Object {
                        connection,
                        scope: explorer::Scope {
                            catalog: None,
                            schema: Some(schema.to_string()),
                        },
                        folder: explorer::Folder::Tables,
                        name: name.to_string(),
                    },
                    cx,
                );
            });
        })
        .expect("the window is open");
}

/// The builder that is the tab on top, if one is.
fn active_builder(workspace: &Workspace) -> Entity<BuilderPane> {
    let pane = active_pane(workspace);
    match area(workspace)
        .panes
        .get(pane)
        .expect("the pane is in the tree")
        .active()
    {
        Some(PaneItem::QueryBuilder { pane, .. }) => pane.clone(),
        other => panic!("the builder is not the tab on top: {other:?}"),
    }
}

/// The scope every explorer node in these tests sits in.
fn public() -> explorer::Scope {
    explorer::Scope {
        catalog: None,
        schema: Some("PUBLIC".to_string()),
    }
}

/// The two rows every explorer node offers, whatever kind it is.
fn scope_labels() -> Vec<String> {
    vec![
        ts!("menu.erd").to_string(),
        ts!("menu.backup_schema").to_string(),
    ]
}

/// The four rows only a table or a view offers, and the rule under them.
fn relation_labels() -> Vec<String> {
    vec![
        ts!("menu.view_data").to_string(),
        ts!("menu.view_structure").to_string(),
        ts!("menu.query_object").to_string(),
        ts!("menu.add_to_builder").to_string(),
        ts!("menu.extract_script").to_string(),
        ts!("menu.transfer_table").to_string(),
        String::new(),
    ]
}

mod connections;
mod menus;
mod panes;
mod settings;
mod workflows;
