//! Saving a query tab's SQL to a file, and the tab taking the file's name.

use super::*;

/// The title the strip draws for the tab on top: the user's name for it, or
/// the one it takes from what it shows — the same choice `render_tab_strip`
/// makes.
fn shown_title(workspace: &Workspace, cx: &App) -> String {
    let area = area(workspace);
    let item = area
        .panes
        .get(area.active())
        .and_then(|pane| pane.active())
        .expect("a tab is on top");
    area.tab_names
        .get(&item.entity_id())
        .cloned()
        .unwrap_or_else(|| item.title(cx).to_string())
}

/// Whether the tab on top wears the unsaved mark.
fn shown_unsaved(workspace: &Workspace, cx: &App) -> bool {
    let area = area(workspace);
    area.panes
        .get(area.active())
        .and_then(|pane| pane.active())
        .expect("a tab is on top")
        .unsaved(cx)
}

/// A query tab with no file asks where to save, opening in the home
/// directory, and the tab is named after the file chosen from then on — over
/// a name the user had given it, which is what the save replaced.
#[gpui::test]
fn saving_a_new_query_asks_where_and_names_the_tab_after_the_file(cx: &mut gpui::TestAppContext) {
    let (window, _) = workspace_over_h2("save-new", cx);
    let mut cx = gpui::VisualTestContext::from_window(window.into(), cx);
    let dir = tempfile::tempdir().expect("a temporary directory");
    let path = dir.path().join("매출 분석.sql");

    window
        .update(&mut cx, |workspace, window, cx| {
            workspace.open_query("select 1", window, cx);
            let tab = workspace.active_query().unwrap().entity_id();
            workspace
                .work_area_mut()
                .unwrap()
                .tab_names
                .insert(tab, "분석 초안".into());
            assert!(!shown_unsaved(workspace, cx), "an unbound tab has no mark");
            workspace.save_sql_file(false, window, cx);
        })
        .expect("the window is open");
    let home = crate::extract_dialog::default_directory();
    cx.simulate_new_path_selection(|directory| {
        assert_eq!(directory, home, "an unbound tab is saved from home");
        Some(path.clone())
    });
    cx.run_until_parked();

    assert_eq!(std::fs::read_to_string(&path).unwrap(), "select 1");
    window
        .update(&mut cx, |workspace, _window, cx| {
            let pane = workspace.active_query().unwrap().read(cx);
            assert_eq!(pane.file(), Some(path.as_path()));
            assert!(!pane.is_modified(cx));
            assert_eq!(shown_title(workspace, cx), "매출 분석.sql");
            assert!(!shown_unsaved(workspace, cx));
        })
        .expect("the window is open");
}

/// Once bound, an edit marks the tab and `Ctrl`/`Cmd`+`S` writes back to the same
/// file without asking — from the editor, where the keyboard is while typing,
/// which is what proves no binding in it takes the chord first. A name the
/// user gives the bound tab survives that save.
#[gpui::test]
fn a_bound_tab_is_marked_by_an_edit_and_saved_in_place_by_the_shortcut(
    cx: &mut gpui::TestAppContext,
) {
    let (window, _) = workspace_over_h2("save-in-place", cx);
    cx.update(bootstrap::bind_shortcuts);
    let mut cx = gpui::VisualTestContext::from_window(window.into(), cx);
    let dir = tempfile::tempdir().expect("a temporary directory");
    let path = dir.path().join("report.sql");
    std::fs::write(&path, "select 1").expect("the fixture is written");

    window
        .update(&mut cx, |workspace, window, cx| {
            workspace.load_sql_file(path.clone(), window, cx);
        })
        .expect("the window is open");
    cx.run_until_parked();
    cx.simulate_keystrokes(DOCUMENT_END);
    cx.simulate_input(" -- edited");
    window
        .update(&mut cx, |workspace, _window, cx| {
            assert_eq!(shown_title(workspace, cx), "report.sql");
            assert!(shown_unsaved(workspace, cx), "an edit marks the tab");
            let tab = workspace.active_query().unwrap().entity_id();
            workspace
                .work_area_mut()
                .unwrap()
                .tab_names
                .insert(tab, "야간 리포트".into());
        })
        .expect("the window is open");

    cx.simulate_keystrokes(SAVE);
    cx.run_until_parked();

    assert!(
        !cx.did_prompt_for_new_path(),
        "a bound tab is saved in place"
    );
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        "select 1 -- edited"
    );
    window
        .update(&mut cx, |workspace, _window, cx| {
            assert!(!shown_unsaved(workspace, cx), "the save took the mark away");
            assert_eq!(
                shown_title(workspace, cx),
                "야간 리포트",
                "saving to the same file keeps the user's name"
            );
        })
        .expect("the window is open");

    // Undoing past the save is a change the file does not hold.
    cx.simulate_keystrokes(UNDO);
    window
        .update(&mut cx, |workspace, _window, cx| {
            assert!(shown_unsaved(workspace, cx));
        })
        .expect("the window is open");
}

/// "Save as" asks even for a bound tab, opening where its file is, and moves
/// the binding — and the tab's name — to the file chosen. The old file is
/// left as it was.
#[gpui::test]
fn save_as_moves_the_binding_and_the_tab_name_to_the_new_file(cx: &mut gpui::TestAppContext) {
    let (window, _) = workspace_over_h2("save-as", cx);
    let mut cx = gpui::VisualTestContext::from_window(window.into(), cx);
    let dir = tempfile::tempdir().expect("a temporary directory");
    let old = dir.path().join("old.sql");
    let new = dir.path().join("new.sql");
    std::fs::write(&old, "select 1").expect("the fixture is written");

    window
        .update(&mut cx, |workspace, window, cx| {
            workspace.load_sql_file(old.clone(), window, cx);
        })
        .expect("the window is open");
    cx.run_until_parked();
    cx.simulate_keystrokes(DOCUMENT_END);
    cx.simulate_input(" + 1");
    window
        .update(&mut cx, |workspace, window, cx| {
            let tab = workspace.active_query().unwrap().entity_id();
            workspace
                .work_area_mut()
                .unwrap()
                .tab_names
                .insert(tab, "custom".into());
            workspace.save_sql_file(true, window, cx);
        })
        .expect("the window is open");
    cx.simulate_new_path_selection(|directory| {
        assert_eq!(directory, dir.path(), "save as opens beside the bound file");
        Some(new.clone())
    });
    cx.run_until_parked();

    assert_eq!(std::fs::read_to_string(&old).unwrap(), "select 1");
    assert_eq!(std::fs::read_to_string(&new).unwrap(), "select 1 + 1");
    window
        .update(&mut cx, |workspace, _window, cx| {
            let pane = workspace.active_query().unwrap().read(cx);
            assert_eq!(pane.file(), Some(new.as_path()));
            assert_eq!(shown_title(workspace, cx), "new.sql");
            assert!(!shown_unsaved(workspace, cx));
        })
        .expect("the window is open");
}

/// A canceled save dialog changes nothing, and a write that fails says why in
/// the pane and leaves the tab unbound and its name alone.
#[gpui::test]
fn a_canceled_or_failed_save_leaves_the_tab_as_it_was(cx: &mut gpui::TestAppContext) {
    let (window, _) = workspace_over_h2("save-fails", cx);
    let mut cx = gpui::VisualTestContext::from_window(window.into(), cx);
    let dir = tempfile::tempdir().expect("a temporary directory");
    let unwritable = dir.path().join("missing").join("draft.sql");

    let title = window
        .update(&mut cx, |workspace, window, cx| {
            workspace.open_query("select 1", window, cx);
            workspace.save_sql_file(false, window, cx);
            shown_title(workspace, cx)
        })
        .expect("the window is open");
    cx.simulate_new_path_selection(|_| None);
    cx.run_until_parked();
    window
        .update(&mut cx, |workspace, window, cx| {
            assert!(workspace.active_query().unwrap().read(cx).file().is_none());
            workspace.save_sql_file(false, window, cx);
        })
        .expect("the window is open");
    cx.simulate_new_path_selection(|_| Some(unwritable.clone()));
    cx.run_until_parked();

    assert!(!unwritable.exists());
    window
        .update(&mut cx, |workspace, _window, cx| {
            let pane = workspace.active_query().unwrap().read(cx);
            assert!(pane.file().is_none(), "a failed save binds nothing");
            assert!(pane.notice().is_some(), "the failure is said in the pane");
            assert_eq!(shown_title(workspace, cx), title);
        })
        .expect("the window is open");
}

/// With no query tab on top there is nothing to save: no dialog opens.
#[gpui::test]
fn saving_needs_a_query_tab_on_top(cx: &mut gpui::TestAppContext) {
    let (window, id) = workspace_over_h2_with(
        "save-nothing",
        &["create table SAVED (ID int primary key)"],
        cx,
    );
    let mut cx = gpui::VisualTestContext::from_window(window.into(), cx);
    window
        .update(&mut cx, |workspace, window, cx| {
            workspace.open_object(object(id, "SAVED"), window, cx);
            workspace.save_sql_file(false, window, cx);
            workspace.save_sql_file(true, window, cx);
        })
        .expect("the window is open");
    cx.run_until_parked();
    assert!(!cx.did_prompt_for_new_path());
}

/// A bound file written by something else since the tab last read or saved
/// it — here another tab bound to the same file, which a split copy is — is
/// not overwritten: the save says so and asks where instead, starting at that
/// file, and the tab stays marked until it lands.
#[gpui::test]
fn a_file_changed_on_disk_is_not_overwritten_in_place(cx: &mut gpui::TestAppContext) {
    let (window, _) = workspace_over_h2("save-changed", cx);
    let mut cx = gpui::VisualTestContext::from_window(window.into(), cx);
    let dir = tempfile::tempdir().expect("a temporary directory");
    let path = dir.path().join("shared.sql");
    std::fs::write(&path, "select 1").expect("the fixture is written");

    window
        .update(&mut cx, |workspace, window, cx| {
            workspace.load_sql_file(path.clone(), window, cx);
        })
        .expect("the window is open");
    cx.run_until_parked();
    window
        .update(&mut cx, |workspace, window, cx| {
            workspace.split_active(Axis::Horizontal, window, cx);
        })
        .expect("the window is open");
    cx.run_until_parked();
    let (left, right) = window
        .update(&mut cx, |workspace, _window, cx| {
            let queries = area(workspace).queries();
            let [left, right] = queries.as_slice() else {
                panic!("the split copied the query tab");
            };
            assert_eq!(right.read(cx).file(), Some(path.as_path()));
            assert_eq!(shown_title(workspace, cx), "shared.sql");
            (left.clone(), right.clone())
        })
        .expect("the window is open");

    // The copy on the right, which has the keyboard, saves first …
    cx.simulate_keystrokes(DOCUMENT_END);
    cx.simulate_input(" -- right");
    window
        .update(&mut cx, |workspace, window, cx| {
            workspace.save_sql_file(false, window, cx)
        })
        .expect("the window is open");
    cx.run_until_parked();
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "select 1 -- right");

    // … so the original on the left no longer knows the file.
    window
        .update(&mut cx, |workspace, window, cx| {
            let first = area(workspace).panes.leaf_ids()[0];
            workspace.activate_tab(first, 0, window, cx);
            assert_eq!(workspace.active_query(), Some(&left));
            left.update(cx, |pane, cx| pane.focus_editor(window, cx));
        })
        .expect("the window is open");
    cx.simulate_keystrokes(DOCUMENT_END);
    cx.simulate_input(" -- left");
    window
        .update(&mut cx, |workspace, window, cx| {
            workspace.save_sql_file(false, window, cx)
        })
        .expect("the window is open");
    cx.run_until_parked();

    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        "select 1 -- right",
        "the other tab's save is left standing"
    );
    window
        .update(&mut cx, |_workspace, _window, cx| {
            assert!(left.read(cx).notice().is_some(), "the pane says why");
            assert!(left.read(cx).is_modified(cx));
            assert!(!right.read(cx).is_modified(cx));
        })
        .expect("the window is open");
    let other = dir.path().join("left.sql");
    cx.simulate_new_path_selection(|directory| {
        assert_eq!(directory, dir.path(), "the dialog starts at the file");
        Some(other.clone())
    });
    cx.run_until_parked();
    assert_eq!(std::fs::read_to_string(&other).unwrap(), "select 1 -- left");
    window
        .update(&mut cx, |workspace, _window, cx| {
            assert_eq!(left.read(cx).file(), Some(other.as_path()));
            assert!(!left.read(cx).is_modified(cx));
            assert_eq!(shown_title(workspace, cx), "left.sql");
        })
        .expect("the window is open");
}

/// A bound file that has gone is written anew in place: there is nothing of
/// anyone's left there to overwrite.
#[gpui::test]
fn a_bound_file_that_has_gone_is_written_anew(cx: &mut gpui::TestAppContext) {
    let (window, _) = workspace_over_h2("save-gone", cx);
    let mut cx = gpui::VisualTestContext::from_window(window.into(), cx);
    let dir = tempfile::tempdir().expect("a temporary directory");
    let path = dir.path().join("gone.sql");
    std::fs::write(&path, "select 1").expect("the fixture is written");

    window
        .update(&mut cx, |workspace, window, cx| {
            workspace.load_sql_file(path.clone(), window, cx);
        })
        .expect("the window is open");
    cx.run_until_parked();
    std::fs::remove_file(&path).expect("the fixture is removed");
    window
        .update(&mut cx, |workspace, window, cx| {
            workspace.save_sql_file(false, window, cx)
        })
        .expect("the window is open");
    cx.run_until_parked();

    assert!(!cx.did_prompt_for_new_path());
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "select 1");
}
