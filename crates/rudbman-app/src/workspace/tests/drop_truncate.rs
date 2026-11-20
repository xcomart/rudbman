//! The explorer's drop and truncate, end to end against H2: the menu row, the
//! typed confirmation, and what reaches the database.

use super::*;
use crate::context_menu::MenuRow;

/// How many rows `sql` reads, or the driver's refusal.
fn count(workspace: &Workspace, connection: ConnectionId, sql: &str) -> Result<usize, String> {
    let session = workspace
        .session_of(connection)
        .expect("the connection is open");
    let cursor = session
        .session()
        .execute(&rudbman_jdbc::StatementSpec::new(sql))
        .map_err(|error| error.to_string())?;
    cursor
        .fetch(0)
        .map(|batch| batch.rows())
        .map_err(|error| error.to_string())
}

fn table(connection: ConnectionId, name: &str) -> NodeId {
    NodeId::Object {
        connection,
        scope: public(),
        folder: Folder::Tables,
        name: name.to_string(),
    }
}

/// `node`'s explorer menu.
fn menu_rows(
    window: gpui::WindowHandle<Workspace>,
    node: &NodeId,
    cx: &mut gpui::VisualTestContext,
) -> Vec<MenuRow> {
    window
        .update(cx, |workspace, _window, cx| {
            workspace.explorer_rows(node, cx)
        })
        .expect("the window is open")
}

/// Activates the row labeled `key` and lets the statements be written.
fn activate(rows: &[MenuRow], key: &str, cx: &mut gpui::VisualTestContext) {
    let label = ts!(key);
    cx.update(|window, cx| context_menu::row(rows, &label).activate(window, cx));
    cx.run_until_parked();
}

/// Types `text` into the open question's field.
fn type_phrase(
    window: gpui::WindowHandle<Workspace>,
    text: &str,
    cx: &mut gpui::VisualTestContext,
) {
    window
        .update(cx, |workspace, _window, cx| {
            let question = workspace.removal.as_ref().expect("the question is open");
            question
                .input
                .update(cx, |input, cx| input.set_content(text.to_string(), cx));
        })
        .expect("the window is open");
}

fn answer(window: gpui::WindowHandle<Workspace>, run: bool, cx: &mut gpui::VisualTestContext) {
    window
        .update(cx, |workspace, window, cx| {
            workspace.answer_removal(run, window, cx)
        })
        .expect("the window is open");
    cx.run_until_parked();
}

/// The question shows the statement, and nothing runs until the name is
/// typed back exactly — not blank, not in another case.
#[gpui::test]
fn a_table_is_dropped_only_once_its_name_is_typed_back(cx: &mut gpui::TestAppContext) {
    let (window, connection) = workspace_over_h2_with(
        "removal-drop-table",
        &["create table PUBLIC.PERSON (ID integer)"],
        cx,
    );
    let mut cx = gpui::VisualTestContext::from_window(window.into(), cx);
    cx.run_until_parked();
    let person = table(connection, "PERSON");

    let rows = menu_rows(window, &person, &mut cx);
    activate(&rows, "menu.drop_table", &mut cx);
    window
        .update(&mut cx, |workspace, _window, cx| {
            let question = workspace.removal.as_ref().expect("the question is open");
            assert_eq!(question.statements, ["DROP TABLE PUBLIC.PERSON"]);
            assert_eq!(question.phrase, "PERSON");
            assert!(!question.answered(cx));
            assert!(workspace.dialog_open(cx));
        })
        .unwrap();

    for wrong in ["", "person", "PERSONS"] {
        type_phrase(window, wrong, &mut cx);
        answer(window, true, &mut cx);
        window
            .update(&mut cx, |workspace, _window, _cx| {
                assert!(
                    workspace.removal.is_some(),
                    "{wrong:?} was taken for the name"
                );
                assert_eq!(
                    count(workspace, connection, "select * from PUBLIC.PERSON"),
                    Ok(0)
                );
            })
            .unwrap();
    }

    type_phrase(window, "  PERSON ", &mut cx);
    answer(window, true, &mut cx);
    window
        .update(&mut cx, |workspace, _window, _cx| {
            assert!(workspace.removal.is_none());
            assert!(
                count(workspace, connection, "select * from PUBLIC.PERSON").is_err(),
                "the table is still there"
            );
        })
        .unwrap();
}

/// A routine is looked up before it is asked about, and dropped the way its
/// product names one: on H2 every routine is an alias.
#[gpui::test]
fn a_routine_is_looked_up_and_dropped_the_way_its_product_names_it(cx: &mut gpui::TestAppContext) {
    let (window, connection) = workspace_over_h2_with(
        "removal-drop-routine",
        &["create alias PUBLIC.F_ABS for 'java.lang.Math.abs(int)'"],
        cx,
    );
    let mut cx = gpui::VisualTestContext::from_window(window.into(), cx);
    cx.run_until_parked();
    let routine = NodeId::Object {
        connection,
        scope: public(),
        folder: Folder::Procedures,
        name: "F_ABS".to_string(),
    };
    window
        .update(&mut cx, |workspace, _window, _cx| {
            assert_eq!(count(workspace, connection, "call PUBLIC.F_ABS(-1)"), Ok(1));
        })
        .unwrap();

    let rows = menu_rows(window, &routine, &mut cx);
    activate(&rows, "menu.drop_procedure", &mut cx);
    window
        .update(&mut cx, |workspace, _window, _cx| {
            let question = workspace.removal.as_ref().expect("the question is open");
            assert_eq!(question.statements, ["DROP ALIAS PUBLIC.F_ABS"]);
            assert_eq!(question.phrase, "F_ABS");
        })
        .unwrap();

    type_phrase(window, "F_ABS", &mut cx);
    answer(window, true, &mut cx);
    window
        .update(&mut cx, |workspace, _window, _cx| {
            assert!(workspace.removal.is_none());
            assert!(
                count(workspace, connection, "call PUBLIC.F_ABS(-1)").is_err(),
                "the alias is still there"
            );
        })
        .unwrap();
}

/// Cancel and `Escape` both put the question away, and nothing is sent.
#[gpui::test]
fn turning_the_question_down_sends_nothing(cx: &mut gpui::TestAppContext) {
    let (window, connection) = workspace_over_h2_with(
        "removal-decline",
        &[
            "create table PUBLIC.PERSON (ID integer)",
            "insert into PUBLIC.PERSON values (1)",
        ],
        cx,
    );
    let mut cx = gpui::VisualTestContext::from_window(window.into(), cx);
    cx.run_until_parked();
    let person = table(connection, "PERSON");

    let rows = menu_rows(window, &person, &mut cx);
    activate(&rows, "menu.truncate_table", &mut cx);
    type_phrase(window, "PERSON", &mut cx);
    answer(window, false, &mut cx);

    activate(&rows, "menu.truncate_table", &mut cx);
    type_phrase(window, "PERSON", &mut cx);
    window
        .update(&mut cx, |_workspace, window, cx| {
            window.dispatch_action(Box::new(crate::DismissDialog), cx);
        })
        .unwrap();
    cx.run_until_parked();

    window
        .update(&mut cx, |workspace, _window, _cx| {
            assert!(workspace.removal.is_none());
            assert_eq!(
                count(workspace, connection, "select * from PUBLIC.PERSON"),
                Ok(1)
            );
        })
        .unwrap();
}

/// A truncate empties the table and keeps it.
#[gpui::test]
fn a_truncate_empties_the_table_and_keeps_it(cx: &mut gpui::TestAppContext) {
    let (window, connection) = workspace_over_h2_with(
        "removal-truncate",
        &[
            "create table PUBLIC.PERSON (ID integer)",
            "insert into PUBLIC.PERSON values (1)",
            "insert into PUBLIC.PERSON values (2)",
        ],
        cx,
    );
    let mut cx = gpui::VisualTestContext::from_window(window.into(), cx);
    cx.run_until_parked();

    let rows = menu_rows(window, &table(connection, "PERSON"), &mut cx);
    activate(&rows, "menu.truncate_table", &mut cx);
    window
        .update(&mut cx, |workspace, _window, _cx| {
            let question = workspace.removal.as_ref().expect("the question is open");
            assert_eq!(question.statements, ["TRUNCATE TABLE PUBLIC.PERSON"]);
        })
        .unwrap();
    type_phrase(window, "PERSON", &mut cx);
    answer(window, true, &mut cx);
    window
        .update(&mut cx, |workspace, _window, _cx| {
            assert_eq!(
                count(workspace, connection, "select * from PUBLIC.PERSON"),
                Ok(0)
            );
        })
        .unwrap();
}

/// Over several selected objects the verb is what is typed, the view goes
/// before the table it reads, and a schema is dropped by its own name.
#[gpui::test]
fn a_selection_and_a_schema_are_dropped_in_an_order_that_works(cx: &mut gpui::TestAppContext) {
    let (window, connection) = workspace_over_h2_with(
        "removal-selection",
        &[
            "create table PUBLIC.PERSON (ID integer)",
            "create view PUBLIC.PEOPLE as select * from PUBLIC.PERSON",
            "create schema EMPTY_ONE",
        ],
        cx,
    );
    let mut cx = gpui::VisualTestContext::from_window(window.into(), cx);
    cx.run_until_parked();
    let person = table(connection, "PERSON");
    let people = NodeId::Object {
        connection,
        scope: public(),
        folder: Folder::Views,
        name: "PEOPLE".to_string(),
    };

    // What the row hands over for a selection of the two (the order is
    // `removal_nodes`', tested beside it).
    window
        .update(&mut cx, |workspace, window, cx| {
            workspace.ask_removal(
                removal::Removal::Drop,
                vec![people.clone(), person.clone()],
                window,
                cx,
            );
        })
        .unwrap();
    cx.run_until_parked();
    window
        .update(&mut cx, |workspace, _window, _cx| {
            let question = workspace.removal.as_ref().expect("the question is open");
            assert_eq!(
                question.statements,
                ["DROP VIEW PUBLIC.PEOPLE", "DROP TABLE PUBLIC.PERSON"]
            );
            assert_eq!(question.phrase, "DROP");
        })
        .unwrap();
    type_phrase(window, "PERSON", &mut cx);
    answer(window, true, &mut cx);
    window
        .update(&mut cx, |workspace, _window, _cx| {
            assert!(workspace.removal.is_some(), "a name is not the verb");
        })
        .unwrap();
    type_phrase(window, "DROP", &mut cx);
    answer(window, true, &mut cx);
    window
        .update(&mut cx, |workspace, _window, _cx| {
            assert!(count(workspace, connection, "select * from PUBLIC.PEOPLE").is_err());
            assert!(count(workspace, connection, "select * from PUBLIC.PERSON").is_err());
        })
        .unwrap();

    let schema = NodeId::Schema {
        connection,
        catalog: None,
        name: "EMPTY_ONE".to_string(),
    };
    let rows = menu_rows(window, &schema, &mut cx);
    activate(&rows, "menu.drop_schema", &mut cx);
    window
        .update(&mut cx, |workspace, _window, _cx| {
            let question = workspace.removal.as_ref().expect("the question is open");
            assert_eq!(question.statements, ["DROP SCHEMA EMPTY_ONE"]);
            assert_eq!(question.phrase, "EMPTY_ONE");
        })
        .unwrap();
    type_phrase(window, "EMPTY_ONE", &mut cx);
    answer(window, true, &mut cx);
    window
        .update(&mut cx, |workspace, _window, _cx| {
            assert!(
                count(
                    workspace,
                    connection,
                    "select * from INFORMATION_SCHEMA.SCHEMATA where SCHEMA_NAME = 'EMPTY_ONE'"
                ) == Ok(0)
            );
        })
        .unwrap();
}

/// A read-only profile grays both rows, and an editor's open transaction
/// keeps the question from opening at all: the autocommit flip would end it.
#[gpui::test]
fn a_read_only_profile_and_an_open_transaction_refuse(cx: &mut gpui::TestAppContext) {
    let (window, connection) = workspace_over_h2_with(
        "removal-refusals",
        &["create table PUBLIC.PERSON (ID integer)"],
        cx,
    );
    let mut cx = gpui::VisualTestContext::from_window(window.into(), cx);
    cx.run_until_parked();
    let person = table(connection, "PERSON");

    window
        .update(&mut cx, |workspace, _window, cx| {
            let open = workspace
                .connections
                .iter_mut()
                .find(|open| open.id == connection)
                .unwrap();
            open.profile.read_only = true;
            let rows = workspace.explorer_rows(&person, cx);
            let grayed = context_menu::grayed(&rows);
            assert!(grayed.contains(&ts!("menu.drop_table").to_string()));
            assert!(grayed.contains(&ts!("menu.truncate_table").to_string()));
            assert!(!grayed.contains(&ts!("menu.view_data").to_string()));
            workspace
                .connections
                .iter_mut()
                .find(|open| open.id == connection)
                .unwrap()
                .profile
                .read_only = false;
            workspace.session_of(connection).unwrap().mark_uncommitted();
        })
        .unwrap();

    let rows = menu_rows(window, &person, &mut cx);
    activate(&rows, "menu.drop_table", &mut cx);
    window
        .update(&mut cx, |workspace, _window, _cx| {
            assert!(
                workspace.removal.is_none(),
                "it asked over an open transaction"
            );
            workspace
                .session_of(connection)
                .unwrap()
                .clear_uncommitted();
            assert_eq!(
                count(workspace, connection, "select * from PUBLIC.PERSON"),
                Ok(0)
            );
        })
        .unwrap();
}
