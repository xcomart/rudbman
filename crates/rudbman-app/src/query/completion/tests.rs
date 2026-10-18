use gpui::{VisualTestContext, WindowHandle};

use super::*;
use crate::query::tests::{h2, pane};

fn source(handle: &WindowHandle<QueryPane>, sql: &str, cx: &mut VisualTestContext) {
    let caret = sql.find('|').unwrap_or(sql.len());
    let text = sql.replacen('|', "", 1);
    handle
        .update(cx, |pane, window, cx| {
            pane.editor.update(cx, |editor, cx| {
                editor.set_text(&text, cx);
                editor.select_range(caret..caret, cx);
            });
            window.activate_window();
            pane.focus_editor(window, cx);
        })
        .unwrap();
    cx.run_until_parked();
}

#[gpui::test]
fn completion_keyboard_accepts_undoes_dismisses_and_preserves_execution(
    cx: &mut gpui::TestAppContext,
) {
    let (connected, profile) = h2("completion_keys", &[]);
    let handle = pane(&connected, &profile, 100, cx);
    let mut cx = VisualTestContext::from_window(handle.into(), cx);
    source(&handle, "SELECT 1; sel|ect", &mut cx);
    cx.simulate_keystrokes("ctrl-space");
    handle
        .update(&mut cx, |pane, _, _| {
            assert!(pane.completion.popup.is_some());
            assert_eq!(
                pane.completion.popup.as_ref().unwrap().items[0].insert,
                "SELECT"
            );
        })
        .unwrap();
    cx.simulate_keystrokes("tab");
    handle
        .update(&mut cx, |pane, _, cx| {
            assert_eq!(pane.editor_text(cx), "SELECT 1; SELECT");
            assert!(pane.completion.popup.is_none());
        })
        .unwrap();
    cx.simulate_keystrokes(if cfg!(target_os = "macos") {
        "cmd-z"
    } else {
        "ctrl-z"
    });
    handle
        .update(&mut cx, |pane, _, cx| {
            assert_eq!(pane.editor_text(cx), "SELECT 1; select")
        })
        .unwrap();
    cx.simulate_keystrokes("ctrl-space escape");
    handle
        .update(&mut cx, |pane, _, _| {
            assert!(pane.completion.popup.is_none())
        })
        .unwrap();
    // With the popup dismissed the editor owns Enter again.
    cx.simulate_keystrokes("enter");
    handle
        .update(&mut cx, |pane, _, cx| {
            assert!(pane.editor_text(cx).contains('\n'))
        })
        .unwrap();

    source(&handle, "SELECT 1|", &mut cx);
    cx.simulate_keystrokes(if cfg!(target_os = "macos") {
        "cmd-enter"
    } else {
        "ctrl-enter"
    });
    cx.run_until_parked();
    handle
        .update(&mut cx, |pane, _, _| assert!(pane.ran))
        .unwrap();
}

#[gpui::test]
fn completion_reads_real_table_and_alias_columns_and_stays_closed_after_blur(
    cx: &mut gpui::TestAppContext,
) {
    let (connected, profile) = h2(
        "completion_metadata",
        &[
            "CREATE TABLE PEOPLE (ID INT, NAME VARCHAR(40), \"display name\" VARCHAR(40))",
            "CREATE VIEW PEOPLE_VIEW AS SELECT ID, NAME FROM PEOPLE",
        ],
    );
    let handle = pane(&connected, &profile, 100, cx);
    let mut cx = VisualTestContext::from_window(handle.into(), cx);
    source(&handle, "SELECT p.na| FROM PUBLIC.PEOPLE p", &mut cx);
    cx.simulate_keystrokes("ctrl-space");
    // Bypass only the debounce clock; this is the actual JDBC metadata path.
    handle
        .update(&mut cx, |pane, window, cx| {
            pane.load_completion_metadata(window, cx)
        })
        .unwrap();
    cx.run_until_parked();
    handle
        .update(&mut cx, |pane, _, _| {
            let open = pane.completion.popup.as_ref().expect("column popup");
            assert_eq!(open.items[0].insert, "NAME");
            assert!(
                pane.completion
                    .tables
                    .iter()
                    .any(|t| t.name == "PEOPLE_VIEW")
            );
            assert!(
                pane.completion
                    .tables
                    .iter()
                    .find(|t| t.name == "PEOPLE_VIEW")
                    .unwrap()
                    .columns
                    .is_none(),
                "unreferenced columns stay lazy"
            );
        })
        .unwrap();
    cx.simulate_keystrokes("enter");
    handle
        .update(&mut cx, |pane, _, cx| {
            assert_eq!(pane.editor_text(cx), "SELECT p.NAME FROM PUBLIC.PEOPLE p");
            assert!(!pane.ran, "completion must not run SQL");
            assert!(pane.completion.popup.is_none());
        })
        .unwrap();
    source(&handle, "SELECT * FROM peo|", &mut cx);
    cx.simulate_keystrokes("ctrl-space down tab");
    handle
        .update(&mut cx, |pane, _, cx| {
            assert_eq!(pane.editor_text(cx), "SELECT * FROM PUBLIC.PEOPLE_VIEW")
        })
        .unwrap();

    source(&handle, "SELECT p.| FROM PEOPLE p", &mut cx);
    cx.simulate_keystrokes("ctrl-space");
    handle
        .update(&mut cx, |pane, window, cx| {
            assert!(pane.completion.popup.is_some());
            window.blur(cx);
        })
        .unwrap();
    cx.run_until_parked();
    cx.update(|window, cx| window.draw(cx).clear(cx));
    handle
        .update(&mut cx, |pane, _, _| {
            assert!(!pane.completion.wanted && pane.completion.popup.is_none())
        })
        .unwrap();
}

#[gpui::test]
fn completion_automatic_typing_skips_literals_and_selection_and_detach_keeps_keywords(
    cx: &mut gpui::TestAppContext,
) {
    let (connected, profile) = h2("completion_automatic", &[]);
    let handle = pane(&connected, &profile, 100, cx);
    let mut cx = VisualTestContext::from_window(handle.into(), cx);
    source(&handle, "|", &mut cx);
    cx.simulate_keystrokes("s e l");
    handle
        .update(&mut cx, |pane, _, _| {
            assert!(pane.completion.popup.is_some(), "typing opens suggestions")
        })
        .unwrap();
    cx.simulate_keystrokes("escape");
    for sql in ["-- sel|", "SELECT 'sel|", "/* sel|"] {
        source(&handle, sql, &mut cx);
        cx.simulate_keystrokes("ctrl-space");
        handle
            .update(&mut cx, |pane, _, _| {
                assert!(pane.completion.popup.is_none())
            })
            .unwrap();
    }
    source(&handle, "sel|", &mut cx);
    cx.simulate_keystrokes("shift-left ctrl-space");
    handle
        .update(&mut cx, |pane, _, cx| {
            assert!(pane.completion.popup.is_none());
            pane.detach(cx);
            assert!(pane.completion.tables.is_empty());
        })
        .unwrap();
    cx.simulate_keystrokes("right ctrl-space enter");
    handle
        .update(&mut cx, |pane, _, cx| {
            assert_eq!(pane.editor_text(cx), "SELECT");
            assert!(!pane.ran);
        })
        .unwrap();
}

#[gpui::test]
fn completion_menu_and_pointer_insert_without_stealing_ime_composition(
    cx: &mut gpui::TestAppContext,
) {
    use gpui::EntityInputHandler;
    let (connected, profile) = h2("completion_pointer", &[]);
    let handle = pane(&connected, &profile, 100, cx);
    let mut cx = VisualTestContext::from_window(handle.into(), cx);
    source(&handle, "sel|", &mut cx);
    let rows = handle
        .update(&mut cx, |pane, _, cx| {
            pane.close_completion(cx);
            pane.context_menu = Some(PaneMenu::Editor {
                position: point(px(20.), px(20.)),
            });
            pane.editor_rows(cx)
        })
        .unwrap();
    let trigger = context_menu::row(&rows, &ts!("completion.trigger"));
    cx.update(|window, cx| trigger.activate(window, cx));
    cx.run_until_parked();
    cx.update(|window, cx| window.draw(cx).clear(cx));
    let bounds = cx
        .debug_bounds("sql-completion-popup")
        .expect("completion popup is visible");
    cx.simulate_click(
        point(bounds.left() + px(20.), bounds.top() + px(15.)),
        gpui::Modifiers::none(),
    );
    handle
        .update(&mut cx, |pane, _, cx| {
            assert_eq!(pane.editor_text(cx), "SELECT");
            assert!(pane.completion.popup.is_none());
        })
        .unwrap();

    source(&handle, "|", &mut cx);
    handle
        .update(&mut cx, |pane, window, cx| {
            pane.editor.update(cx, |editor, cx| {
                editor.replace_and_mark_text_in_range(None, "sel", Some(3..3), window, cx)
            });
        })
        .unwrap();
    cx.run_until_parked();
    cx.simulate_keystrokes("ctrl-space");
    handle
        .update(&mut cx, |pane, window, cx| {
            assert!(pane.completion.popup.is_none());
            assert!(!pane.editor.read(cx).intercepts());
            pane.editor.update(cx, |editor, cx| {
                editor.replace_text_in_range(None, "sel", window, cx)
            });
        })
        .unwrap();
    cx.run_until_parked();
    handle
        .update(&mut cx, |pane, _, _| {
            assert!(
                pane.completion.popup.is_some(),
                "committed input can complete"
            )
        })
        .unwrap();
}

#[gpui::test]
fn late_completion_metadata_does_not_reopen_a_dismissed_or_detached_pane(
    cx: &mut gpui::TestAppContext,
) {
    let (connected, profile) = h2("completion_late", &["CREATE TABLE LATE_TABLE (ID INT)"]);
    let handle = pane(&connected, &profile, 100, cx);
    let mut cx = VisualTestContext::from_window(handle.into(), cx);
    source(&handle, "SELECT * FROM LAT|", &mut cx);
    handle
        .update(&mut cx, |pane, window, cx| {
            pane.trigger_completion(window, cx);
            pane.load_completion_metadata(window, cx);
            assert!(pane.completion.loading);
            pane.close_completion(cx);
        })
        .unwrap();
    cx.run_until_parked();
    handle
        .update(&mut cx, |pane, window, cx| {
            assert!(!pane.completion.loading);
            assert!(pane.completion.popup.is_none());
            assert_eq!(pane.editor_text(cx), "SELECT * FROM LAT");
            pane.trigger_completion(window, cx);
            pane.load_completion_metadata(window, cx);
            pane.detach(cx);
        })
        .unwrap();
    cx.run_until_parked();
    handle
        .update(&mut cx, |pane, _, _| {
            assert!(pane.completion.tables.is_empty());
            assert!(pane.completion.popup.is_none());
        })
        .unwrap();
}

#[gpui::test]
fn function_completion_preserves_arguments_and_is_undoable(cx: &mut gpui::TestAppContext) {
    let (connected, profile) = h2("completion_functions", &[]);
    let handle = pane(&connected, &profile, 100, cx);
    let mut cx = VisualTestContext::from_window(handle.into(), cx);
    source(&handle, "SELECT cou|nt(*)", &mut cx);
    cx.simulate_keystrokes("ctrl-space");
    handle
        .update(&mut cx, |pane, _, _| {
            let open = pane.completion.popup.as_ref().expect("function popup");
            assert_eq!(open.items[open.selected].kind, Kind::Function);
            assert_eq!(open.items[open.selected].label, "COUNT");
        })
        .unwrap();
    cx.simulate_keystrokes("tab");
    handle
        .update(&mut cx, |pane, _, cx| {
            assert_eq!(pane.editor_text(cx), "SELECT COUNT(*)");
            assert!(!pane.ran);
            assert!(pane.completion.popup.is_none());
        })
        .unwrap();
    cx.simulate_keystrokes(if cfg!(target_os = "macos") {
        "cmd-z"
    } else {
        "ctrl-z"
    });
    handle
        .update(&mut cx, |pane, _, cx| {
            assert_eq!(pane.editor_text(cx), "SELECT count(*)")
        })
        .unwrap();
    source(&handle, "SELECT sum|", &mut cx);
    cx.simulate_keystrokes("ctrl-space enter");
    handle
        .update(&mut cx, |pane, _, cx| {
            assert_eq!(pane.editor_text(cx), "SELECT SUM")
        })
        .unwrap();
}
