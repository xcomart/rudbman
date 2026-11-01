use super::*;
use crate::query_history::{Outcome, QueryHistory};

/// What the pane's runs wrote into its profile's history, newest first.
fn recorded(profile: uuid::Uuid, cx: &mut TestAppContext) -> Vec<(String, Outcome)> {
    cx.update(|cx| {
        QueryHistory::entries(cx, profile)
            .into_iter()
            .map(|entry| (entry.sql, entry.outcome))
            .collect()
    })
}

/// Every statement of a script is written down with how it ended, up to the
/// one it stopped at; a sort's re-run, a refused write and a statement after
/// the failure are not.
#[gpui::test]
fn the_statements_the_user_ran_are_written_down(cx: &mut TestAppContext) {
    let (connected, mut profile) = h2(
        "history",
        &[
            "create table H (ID int primary key)",
            "insert into H values (1), (2)",
        ],
    );
    profile.id = uuid::Uuid::new_v4();
    let window = pane(&connected, &profile, 500, cx);
    cx.update(|cx| crate::app_settings::replace(AppSettings::default(), cx));

    window
        .update(cx, |pane, _window, cx| {
            pane.editor.update(cx, |editor, cx| {
                editor.set_text(
                    "select ID from H;\nupdate H set ID = ID where ID = 1;\nselect nope from H;\nselect 1",
                    cx,
                );
                cx.emit(EditorEvent::RunAll);
            });
        })
        .expect("the window is open");
    cx.run_until_parked();
    let entries = recorded(profile.id, cx);
    assert_eq!(entries.len(), 3, "{entries:?}");
    assert_eq!(entries[0].0, "select nope from H");
    assert!(entries[0].1.is_failure());
    assert_eq!(entries[1].1, Outcome::Affected { count: 1 });
    assert_eq!(
        entries[2],
        (
            "select ID from H".to_string(),
            Outcome::Rows {
                count: 2,
                more: false
            }
        )
    );

    run(&window, "select ID from H", cx);
    let id = first_result(&window, cx);
    window
        .update(cx, |pane, window, cx| {
            pane.reorder(id, 0, Some(SortDirection::Descending), window, cx);
        })
        .expect("the window is open");
    cx.run_until_parked();
    assert_eq!(column_zero(&window, 0, cx), ["2", "1"], "the sort ran");
    assert_eq!(
        recorded(profile.id, cx).len(),
        4,
        "the sort was written down"
    );

    profile.read_only = true;
    let refusing = pane(&connected, &profile, 500, cx);
    run(&refusing, "delete from H", cx);
    assert_eq!(
        recorded(profile.id, cx).len(),
        4,
        "a write the profile refused was written down"
    );
    connected.close().expect("close");
}
