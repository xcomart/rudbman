use super::*;
use gpui::VisualTestContext;

fn open(handle: &WindowHandle<QueryPane>, row: usize, column: usize, cx: &mut VisualTestContext) {
    handle
        .update(cx, |pane, window, cx| {
            window.activate_window();
            pane.grid_at(0).update(cx, |_, cx| {
                cx.emit(GridEvent::CellActivated { row, column })
            });
        })
        .unwrap();
    cx.run_until_parked();
    handle
        .update(cx, |pane, _, _| assert!(pane.lob_dialog.is_some()))
        .unwrap();
}

fn replace(text: &str, cx: &mut VisualTestContext) {
    cx.simulate_keystrokes(if cfg!(target_os = "macos") {
        "cmd-a"
    } else {
        "ctrl-a"
    });
    cx.simulate_input(text);
    cx.run_until_parked();
    cx.simulate_keystrokes(if cfg!(target_os = "macos") {
        "cmd-enter"
    } else {
        "ctrl-enter"
    });
    cx.run_until_parked();
}

#[gpui::test]
fn multiline_query_text_uses_the_viewer_and_keeps_string_bindings(cx: &mut TestAppContext) {
    let original = "first\n한글🙂\n\nlast\n";
    let replacement = "changed\nsecond\n";
    let insert = format!("INSERT INTO NOTES VALUES (1, '{original}')");
    let (connected, profile) = h2(
        "query_multiline",
        &[
            "CREATE TABLE NOTES (ID INT PRIMARY KEY, BODY VARCHAR(1000))",
            &insert,
        ],
    );
    let handle = pane(&connected, &profile, 100, cx);
    run(&handle, "SELECT * FROM NOTES", cx);
    let mut cx = VisualTestContext::from_window(handle.into(), cx);
    handle
        .update(&mut cx, |pane, _, cx| {
            let source = pane.grid_at(0).read(cx).source();
            assert_eq!(source.cell(0, 1), GridCell::Text(original));
            assert!(source.uses_value_viewer(0, 1));
            assert!(source.lob_kind(1).is_none());
            assert_eq!(
                rugpui_grid::cell_label(&source.cell(0, 1)).text,
                "first\\n한글🙂\\n\\nlast\\n"
            );
        })
        .unwrap();
    open(&handle, 0, 1, &mut cx);
    cx.simulate_keystrokes(if cfg!(target_os = "macos") {
        "cmd-a cmd-c"
    } else {
        "ctrl-a ctrl-c"
    });
    cx.update(|_, cx| assert_eq!(cx.read_from_clipboard().unwrap().text().unwrap(), original));
    cx.simulate_keystrokes("escape");
    handle
        .update(&mut cx, |pane, _, cx| {
            assert!(pane.lob_dialog.is_none());
            assert!(!pane.has_pending_edits(cx));
        })
        .unwrap();
    open(&handle, 0, 1, &mut cx);
    replace(replacement, &mut cx);
    handle
        .update(&mut cx, |pane, window, cx| {
            assert!(pane.lob_dialog.is_none());
            assert_eq!(
                pane.grid_at(0).read(cx).source().cell(0, 1),
                GridCell::Text(replacement)
            );
            pane.apply(window, cx);
            assert_eq!(
                pane.preview.as_ref().unwrap().statements[0].params[0],
                rudbman_jdbc::Param::Str(replacement.into())
            );
            pane.confirm_apply(window, cx);
        })
        .unwrap();
    cx.run_until_parked();
    assert_eq!(
        server_rows(&connected, "SELECT BODY FROM NOTES"),
        [replacement]
    );
    handle
        .update(&mut cx, |pane, _, cx| {
            assert!(pane.apply_error.is_none());
            assert!(!pane.has_pending_edits(cx));
            pane.detach(cx);
        })
        .unwrap();
    open(&handle, 0, 1, &mut cx);
    replace("must not be saved", &mut cx);
    handle
        .update(&mut cx, |pane, _, cx| {
            assert!(!pane.has_pending_edits(cx));
            assert_eq!(
                pane.grid_at(0).read(cx).source().cell(0, 1),
                GridCell::Text(replacement)
            );
        })
        .unwrap();
}

#[gpui::test]
fn lob_query_values_are_viewed_edited_staged_and_applied(cx: &mut TestAppContext) {
    let large = "한🙂".repeat(200);
    let insert = format!(
        "INSERT INTO FILES VALUES (1, X'00FF', 'original'), (2, NULL, NULL), \
         (3, X'', ''), (4, X'{}', '{large}')",
        "AB".repeat(100)
    );
    let (connected, profile) = h2(
        "query_lob_editor",
        &[
            "CREATE TABLE FILES (ID INT PRIMARY KEY, B BLOB, C CLOB)",
            &insert,
        ],
    );
    let handle = pane(&connected, &profile, 100, cx);
    run(&handle, "SELECT * FROM FILES ORDER BY ID", cx);
    let mut cx = VisualTestContext::from_window(handle.into(), cx);
    handle
        .update(&mut cx, |pane, _, cx| {
            let source = pane.grid_at(0).read(cx).source();
            assert_eq!(source.lob(0, 1).unwrap().prefix(10).unwrap(), [0, 255]);
            assert_eq!(source.lob(0, 2).unwrap().prefix(100).unwrap(), b"original");
            for (row, column, expected) in [
                (0, 1, "00FF".to_owned()),
                (0, 2, "original".to_owned()),
                (2, 1, String::new()),
                (2, 2, String::new()),
                (3, 1, format!("{}…", "AB".repeat(64))),
                (3, 2, format!("{}…", "한🙂".repeat(128))),
            ] {
                assert_eq!(
                    rugpui_grid::cell_label(&source.cell(row, column)).text,
                    expected
                );
                assert!(source.uses_value_viewer(row, column));
            }
            assert_eq!(source.cell(1, 1), GridCell::Null);
            assert_eq!(source.cell(1, 2), GridCell::Null);
            assert_eq!(
                source.lob(3, 2).unwrap().prefix(large.len()).unwrap(),
                large.as_bytes()
            );
            assert!(source.writable());
            assert_eq!(source.state(), GridSourceState::Complete);
        })
        .unwrap();
    open(&handle, 0, 2, &mut cx);
    replace("새 본문🙂\nsecond line", &mut cx);
    handle
        .update(&mut cx, |pane, _, cx| {
            assert!(pane.lob_dialog.is_none(), "stage closes the editor");
            let source = pane.grid_at(0).read(cx).source();
            assert_eq!(
                source.lob(0, 2).unwrap().prefix(100).unwrap(),
                "새 본문🙂\nsecond line".as_bytes()
            );
            assert_eq!(source.edits().counts().changed, 1);
            assert_eq!(
                rugpui_grid::cell_label(&source.cell(0, 2)).text,
                "새 본문🙂\\nsecond line"
            );
        })
        .unwrap();
    assert_eq!(
        server_rows(
            &connected,
            "SELECT CAST(C AS VARCHAR) FROM FILES WHERE ID = 1"
        ),
        ["original"]
    );
    open(&handle, 0, 1, &mut cx);
    replace("00 80 FF\n01", &mut cx);
    handle
        .update(&mut cx, |pane, window, cx| {
            assert!(pane.lob_dialog.is_none());
            pane.apply(window, cx);
            let preview = &pane.preview.as_ref().unwrap().statements;
            assert_eq!(preview.len(), 1);
            assert_eq!(
                preview[0].params.len(),
                3,
                "two LOB values, then primary key"
            );
            assert!(matches!(preview[0].params[0], rudbman_jdbc::Param::Lob(_)));
            assert!(matches!(preview[0].params[1], rudbman_jdbc::Param::Lob(_)));
            assert_eq!(preview[0].params[2], rudbman_jdbc::Param::I64(1));
            pane.confirm_apply(window, cx);
        })
        .unwrap();
    cx.run_until_parked();
    handle
        .update(&mut cx, |pane, _, cx| {
            assert!(pane.apply_error.is_none());
            assert!(!pane.has_pending_edits(cx));
            let source = pane.grid_at(0).read(cx).source();
            assert_eq!(rugpui_grid::cell_label(&source.cell(0, 1)).text, "0080FF01");
            assert_eq!(
                source.lob(0, 1).unwrap().prefix(10).unwrap(),
                [0, 128, 255, 1]
            );
            assert_eq!(
                source.lob(0, 2).unwrap().prefix(100).unwrap(),
                "새 본문🙂\nsecond line".as_bytes()
            );
        })
        .unwrap();
    // NULL LOB cells also open the viewer and can become non-NULL values.
    open(&handle, 1, 1, &mut cx);
    replace("DE AD BE EF", &mut cx);
    handle
        .update(&mut cx, |pane, _, cx| {
            assert!(pane.lob_dialog.is_none());
            assert_eq!(
                pane.grid_at(0)
                    .read(cx)
                    .source()
                    .lob(1, 1)
                    .unwrap()
                    .prefix(10)
                    .unwrap(),
                [0xde, 0xad, 0xbe, 0xef]
            );
            let id = pane.row_result(0).id;
            pane.set_null(id, 0, 2, cx);
            assert_eq!(pane.grid_at(0).read(cx).source().cell(0, 2), GridCell::Null);
            assert!(
                pane.grid_at(0)
                    .read(cx)
                    .source()
                    .edits()
                    .staged(0, 2)
                    .is_some(),
                "a non-NULL LOB is not confused with NULL"
            );
        })
        .unwrap();
}

#[gpui::test]
fn lob_editor_rejects_invalid_hex_and_preserves_values_when_read_only(cx: &mut TestAppContext) {
    let (connected, profile) = h2(
        "query_lob_invalid",
        &[
            "CREATE TABLE FILES (ID INT PRIMARY KEY, B BLOB)",
            "INSERT INTO FILES VALUES (1, X'00FF')",
        ],
    );
    let handle = pane(&connected, &profile, 100, cx);
    run(&handle, "SELECT * FROM FILES", cx);
    let mut cx = VisualTestContext::from_window(handle.into(), cx);
    open(&handle, 0, 1, &mut cx);
    replace("0Z1", &mut cx);
    handle
        .update(&mut cx, |pane, _, cx| {
            assert!(pane.lob_dialog.is_some());
            assert!(
                pane.has_pending_edits(cx),
                "unfinished editor changes block a destructive rerun"
            );
            assert!(pane.grid_at(0).read(cx).source().edits().is_empty());
            assert_eq!(
                pane.grid_at(0)
                    .read(cx)
                    .source()
                    .lob(0, 1)
                    .unwrap()
                    .prefix(10)
                    .unwrap(),
                [0, 255]
            );
        })
        .unwrap();
    cx.simulate_keystrokes("escape");
    handle
        .update(&mut cx, |pane, _, _| {
            assert!(
                pane.lob_dialog.is_some(),
                "dirty Escape must ask before discarding"
            )
        })
        .unwrap();
    replace("AB CD", &mut cx);
    handle
        .update(&mut cx, |pane, _, cx| {
            assert!(pane.lob_dialog.is_none());
            let id = pane.row_result(0).id;
            pane.discard_all(id, cx);
            pane.detach(cx);
        })
        .unwrap();
    open(&handle, 0, 1, &mut cx);
    replace("00", &mut cx);
    handle
        .update(&mut cx, |pane, _, cx| {
            assert!(pane.lob_dialog.is_some());
            assert!(!pane.has_pending_edits(cx));
            assert_eq!(
                pane.grid_at(0)
                    .read(cx)
                    .source()
                    .lob(0, 1)
                    .unwrap()
                    .prefix(10)
                    .unwrap(),
                [0, 255]
            );
        })
        .unwrap();
    cx.simulate_keystrokes("escape");
    handle
        .update(&mut cx, |pane, _, _| assert!(pane.lob_dialog.is_none()))
        .unwrap();
}
