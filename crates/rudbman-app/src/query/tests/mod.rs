use gpui::{TestAppContext, WindowHandle};

use super::*;
use crate::app_settings;
use crate::connection::{self, Connected};

impl QueryPane {
    /// The grid of result tab `index`, for the assertions below.
    fn grid_at(&self, index: usize) -> &Entity<GridView<EditableSource>> {
        match &self.results[index].body {
            ResultBody::Rows(rows) => &rows.grid,
            ResultBody::Message(text) => panic!("result {index} is the message {text:?}"),
        }
    }

    /// The message of result tab `index`.
    fn message_at(&self, index: usize) -> &SharedString {
        match &self.results[index].body {
            ResultBody::Message(text) => text,
            ResultBody::Rows(_) => panic!("result {index} is a grid"),
        }
    }
}

/// A live H2 database with `setup` already run against it.
///
/// `DB_CLOSE_DELAY=-1` keeps it alive between connections, exactly as the
/// explorer's own fixture does.
fn h2(name: &str, setup: &[&str]) -> (Connected, ConnectionProfile) {
    let mut profile = connection::h2::profile(name);
    profile.url = format!("{};DB_CLOSE_DELAY=-1", profile.url);
    // The guards are switched on by the one test that is about them; the
    // rest run writes as setup and would only be asking themselves.
    profile.confirm_writes = false;
    profile.read_only = false;
    let connected = connection::connect(
        &profile,
        &connection::h2::driver(),
        &connection::Credentials::typed(Some(String::new()), None),
        &AppSettings::default(),
    )
    .expect("H2 opens an in-memory database without a server");

    for sql in setup {
        connected
            .session()
            .execute(&StatementSpec::new(*sql))
            .unwrap_or_else(|error| panic!("{sql}: {error}"));
    }
    (connected, profile)
}

/// A window whose whole content is one query pane over `connected`.
fn pane(
    connected: &Connected,
    profile: &ConnectionProfile,
    batch_rows: u32,
    cx: &mut TestAppContext,
) -> WindowHandle<QueryPane> {
    cx.update(|cx| {
        app_settings::init(cx);
        rugpui::init(cx);
        rugpui_editor::init(cx);
        rugpui_grid::init(cx);
    });
    let settings = AppSettings {
        fetch_batch_rows: batch_rows,
        ..AppSettings::default()
    };
    let session = connected.handle();
    let profile = profile.clone();
    cx.add_window(move |window, cx| {
        QueryPane::new(
            session,
            ConnectionId(1),
            &profile,
            "h2",
            &settings,
            "",
            window,
            cx,
        )
    })
}

/// Runs `sql` and waits for the whole pipeline to settle.
fn run(handle: &WindowHandle<QueryPane>, sql: &str, cx: &mut TestAppContext) {
    handle
        .update(cx, |pane, window, cx| {
            pane.request(vec![sql.to_string()], window, cx);
        })
        .expect("the window is open");
    cx.run_until_parked();
}

/// Column zero of the grid of result `index`, as text.
fn column_zero(
    window: &WindowHandle<QueryPane>,
    index: usize,
    cx: &mut TestAppContext,
) -> Vec<String> {
    window
        .update(cx, |pane, _window, cx| {
            let source = pane.grid_at(index).read(cx).source();
            (0..source.row_count())
                .map(|row| match source.cell(row, 0) {
                    GridCell::Text(text) => text.to_string(),
                    GridCell::Null => "NULL".to_string(),
                    // Unreachable over a query result, which stages
                    // nothing: only the data pane's overlay can leave a
                    // column to the server (§7.9).
                    GridCell::Default => "DEFAULT".to_string(),
                    GridCell::Lob { size } => format!("lob {size:?}"),
                })
                .collect()
        })
        .expect("the window is open")
}

/// The state of the grid of result `index`.
fn state(
    window: &WindowHandle<QueryPane>,
    index: usize,
    cx: &mut TestAppContext,
) -> GridSourceState {
    window
        .update(cx, |pane, _window, cx| {
            pane.grid_at(index).read(cx).source().state()
        })
        .expect("the window is open")
}

/// One column's source metadata, spelled exactly as given.
///
/// `None` writes a JSON `null` and `Some("")` writes an empty string,
/// because those are the two spellings §7.9's gate has to treat alike and a
/// builder that could not tell them apart would test neither.
fn described(
    name: &str,
    catalog: Option<&str>,
    schema: Option<&str>,
    table: Option<&str>,
) -> ColumnInfo {
    fn part(value: Option<&str>) -> String {
        value.map_or_else(|| "null".to_string(), |value| format!("\"{value}\""))
    }
    serde_json::from_str(&format!(
        r#"{{"index":1,"name":"{name}","label":"{name}","table":{},"schema":{},
                 "catalog":{},"type":4,"type_name":"T","jdbc_type":"T",
                 "class_name":null,"precision":0,"scale":0,"display_size":0,
                 "nullable":2,"auto_increment":false,"signed":true,"read_only":false,"kind":4}}"#,
        part(table),
        part(schema),
        part(catalog)
    ))
    .expect("parses")
}

/// A column of `USERS`, as a driver that answers every part reports one.
fn of_users(name: &str) -> ColumnInfo {
    described(name, Some("APP"), Some("PUBLIC"), Some("USERS"))
}

/// The rows of one menu, taken out of the pane so that a row which acts on
/// the pane can be run without re-entering the update it was built in.
fn menu_rows(
    window: &WindowHandle<QueryPane>,
    menu: PaneMenu,
    cx: &mut TestAppContext,
) -> Vec<MenuRow> {
    window
        .update(cx, |pane, _window, cx| match menu {
            PaneMenu::Editor { .. } => pane.editor_rows(cx),
            PaneMenu::Grid { id, target, .. } => pane.grid_rows(id, target, cx),
        })
        .expect("the window is open")
}

/// A window position no menu is really raised at: the rows a menu carries
/// do not depend on where the pointer was.
fn anywhere() -> Point<Pixels> {
    gpui::point(px(0.), px(0.))
}

/// The id of the pane's first result tab.
fn first_result(window: &WindowHandle<QueryPane>, cx: &mut TestAppContext) -> u64 {
    window
        .update(cx, |pane, _window, _cx| pane.results[0].id)
        .expect("the window is open")
}

/// What one column of the clipboard test wants back.
fn clipboard(cx: &mut gpui::VisualTestContext) -> String {
    cx.update(|_window, cx| {
        cx.read_from_clipboard()
            .and_then(|item| item.text())
            .unwrap_or_default()
    })
}

/// A schema-less H2 fixture with one keyed table and one to join it to.
fn people(name: &str) -> (Connected, ConnectionProfile) {
    h2(
        name,
        &[
            "create table PERSON (ID int primary key, NAME varchar(20), NOTE varchar(20))",
            "create table PET (ID int primary key, OWNER int, NAME varchar(20))",
            "insert into PERSON values (1, 'a', null), (2, 'b', '')",
            "insert into PET values (1, 1, 'cat')",
        ],
    )
}

/// Column `column` of the grid of result `index`, as text.
fn column_at(
    window: &WindowHandle<QueryPane>,
    index: usize,
    column: usize,
    cx: &mut TestAppContext,
) -> Vec<String> {
    window
        .update(cx, |pane, _window, cx| {
            let source = pane.grid_at(index).read(cx).source();
            (0..source.row_count())
                .map(|row| match source.cell(row, column) {
                    GridCell::Text(text) => text.to_string(),
                    GridCell::Null => "NULL".to_string(),
                    // Only an inserted row can leave a column to the
                    // server, and §7.9 gives a query result no way to add
                    // one.
                    GridCell::Default => "DEFAULT".to_string(),
                    GridCell::Lob { size } => format!("lob {size:?}"),
                })
                .collect()
        })
        .expect("the window is open")
}

/// Whether the grid of result `index` may be written back through.
fn writable(window: &WindowHandle<QueryPane>, index: usize, cx: &mut TestAppContext) -> bool {
    window
        .update(cx, |pane, _window, cx| {
            pane.grid_at(index).read(cx).source().writable()
        })
        .expect("the window is open")
}

/// The line result `index` gives for why it may only be read.
fn read_only_reason(
    window: &WindowHandle<QueryPane>,
    index: usize,
    cx: &mut TestAppContext,
) -> Option<SharedString> {
    window
        .update(cx, |pane, _window, _cx| match &pane.results[index].body {
            ResultBody::Rows(rows) => rows.read_only.clone(),
            ResultBody::Message(text) => panic!("result {index} is the message {text:?}"),
        })
        .expect("the window is open")
}

/// Opens the field over a cell of result `index`, puts `text` in it and
/// closes it — the whole gesture, and the only route by which anything is
/// staged. Answers whether the field opened at all.
fn type_into(
    handle: &WindowHandle<QueryPane>,
    index: usize,
    row: usize,
    column: usize,
    text: &str,
    cx: &mut TestAppContext,
) -> bool {
    let opened = handle
        .update(cx, |pane, window, cx| {
            let grid = pane.grid_at(index).clone();
            grid.update(cx, |grid, cx| {
                if !grid.begin_edit(row, column, window, cx) {
                    return false;
                }
                let input = grid.editor().cloned().expect("the field is open");
                input.update(cx, |input: &mut rugpui::TextInput, cx| {
                    input.set_content(text.to_owned(), cx);
                });
                grid.commit_edit(cx);
                true
            })
        })
        .expect("the window is open");
    // `EditCommitted` is emitted, not called: the pane hears it on the next
    // turn of the loop.
    cx.run_until_parked();
    opened
}

/// The statements the pane's confirmation is showing.
fn planned(window: &WindowHandle<QueryPane>, cx: &mut TestAppContext) -> Vec<String> {
    window
        .update(cx, |pane, _window, _cx| {
            pane.preview
                .as_ref()
                .map(|preview| {
                    preview
                        .statements
                        .iter()
                        .map(|statement| statement.sql.clone())
                        .collect()
                })
                .unwrap_or_default()
        })
        .expect("the window is open")
}

/// The table as the server holds it, read straight off the session rather
/// than through the pane.
fn server_rows(connected: &Connected, sql: &str) -> Vec<String> {
    let cursor = connected
        .session()
        .execute(&StatementSpec::new(sql))
        .unwrap_or_else(|error| panic!("{sql}: {error}"));
    let batch = cursor.fetch(500).expect("the batch decodes");
    let described = crate::query_source::tests::info(1, "C", 12, 0);
    (0..batch.rows())
        .map(|row| {
            (0..batch.column_count())
                .map(|column| {
                    batch
                        .value(row, column)
                        .and_then(|value| value.to_text(&described))
                        .unwrap_or_else(|| "NULL".to_string())
                })
                .collect::<Vec<_>>()
                .join("|")
        })
        .collect()
}

mod editing;
mod execution;
mod menus;
mod statements;
