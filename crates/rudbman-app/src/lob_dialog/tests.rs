use super::*;
use crate::data_edit::TableSource;
use crate::query_source::{
    ResultSource, render_batch,
    tests::{batch, info},
};
use gpui::{TestAppContext, WindowHandle};

fn window(file: LobFile, writable: bool, cx: &mut TestAppContext) -> WindowHandle<LobDialog> {
    cx.update(|cx| {
        crate::app_settings::init(cx);
        rugpui::init(cx);
        rugpui_editor::init(cx);
        rugpui_grid::init(cx);
    });
    let columns = vec![info(
        1,
        "BODY",
        if file.is_binary() { 2004 } else { 2005 },
        0,
    )];
    let mut base = ResultSource::new(&columns);
    base.push(render_batch(&batch(&[&[None]]), &columns));
    let mut source = EditableSource::new(base, &columns, writable, TableSource::Known);
    source.stage(0, 0, StagedCell::Lob(file));
    let grid = cx.new(|cx| GridView::new(source, cx));
    let window = cx.add_window(move |window, cx| {
        window.activate_window();
        LobDialog::new(grid, 0, 0, true, window, cx)
    });
    cx.run_until_parked();
    window
}

#[test]
fn lob_previews_never_split_utf8_or_accept_incomplete_hex() {
    let text = format!("{}🙂끝", "x".repeat(PREVIEW_BYTES - 1));
    let file = LobFile::from_bytes(text.as_bytes(), false).unwrap();
    let (display, truncated) = preview(&file).unwrap();
    assert!(truncated);
    assert_eq!(display, "x".repeat(PREVIEW_BYTES - 1));
    assert_eq!(decode_hex("00 fF\n80\t01").unwrap(), [0, 255, 128, 1]);
    assert!(decode_hex("0").is_err());
    assert!(decode_hex("GG").is_err());
    assert_eq!(decode_hex("").unwrap(), Vec::<u8>::new());
    for key in [
        "open",
        "open_text",
        "text",
        "export",
        "import",
        "empty",
        "loading",
        "modified",
        "size",
        "truncated",
        "read_only",
        "hex_hint",
        "text_hint",
        "import_error",
        "saved",
        "stage",
        "discard_question",
        "keep_editing",
        "invalid_hex",
    ] {
        for locale in ["en", "ko", "ja", "zh-CN", "de", "fr", "es", "ru"] {
            let key = format!("lob.{key}");
            assert_ne!(rust_i18n::t!(&key, locale = locale).as_ref(), key);
        }
    }
}

#[test]
fn multiline_text_files_stage_as_strings_and_preserve_line_endings() {
    let text = "first\r\n\r\n한🙂\nlast\r";
    let file = LobFile::from_bytes(text.as_bytes(), false).unwrap();
    assert_eq!(
        Contents::File(file).stage(true).unwrap(),
        StagedCell::Text(text.into())
    );
    assert_eq!(
        Contents::Text {
            text: text.into(),
            binary: false
        }
        .stage(true)
        .unwrap(),
        StagedCell::Text(text.into())
    );
}

#[gpui::test]
fn lob_file_picker_exports_the_complete_value_and_imports_an_immutable_replacement(
    cx: &mut TestAppContext,
) {
    let bytes = vec![0xab; PREVIEW_BYTES + 65537];
    let file = LobFile::from_bytes(&bytes, true).unwrap();
    let handle = window(file, true, cx);
    handle
        .update(cx, |dialog, _, cx| {
            assert!(dialog.truncated);
            assert!(
                dialog.editor.read(cx).is_read_only(),
                "the partial preview cannot overwrite the full value"
            );
            assert!(!dialog.changed);
            dialog.export(cx);
        })
        .unwrap();
    let directory = tempfile::tempdir().unwrap();
    let destination = directory.path().join("all.bin");
    cx.simulate_new_path_selection(|_| Some(destination.clone()));
    cx.run_until_parked();
    assert_eq!(std::fs::read(destination).unwrap(), bytes);
    handle
        .update(cx, |dialog, _, cx| {
            assert!(dialog.error.is_none());
            dialog.import(cx);
        })
        .unwrap();
    cx.simulate_path_prompt_response(|_| None);
    cx.run_until_parked();
    handle
        .update(cx, |dialog, _, cx| {
            assert!(
                !dialog.changed,
                "cancelled picker leaves the original intact"
            );
            dialog.import(cx);
        })
        .unwrap();
    let input = directory.path().join("replacement.bin");
    std::fs::write(&input, [0, 1, 128, 255]).unwrap();
    cx.simulate_path_prompt_response(|_| Some(vec![input.clone()]));
    cx.run_until_parked();
    std::fs::write(&input, b"changed outside").unwrap();
    handle
        .update(cx, |dialog, _, cx| {
            assert!(!dialog.truncated);
            assert_eq!(dialog.editor.read(cx).text(), "00 01 80 FF");
            assert!(!dialog.editor.read(cx).is_read_only());
            assert!(dialog.changed);
            dialog.stage(cx);
        })
        .unwrap();
    cx.run_until_parked();
    handle
        .update(cx, |dialog, _, cx| {
            assert!(dialog.error.is_none());
            let source = dialog.grid.read(cx).source();
            assert_eq!(
                source.lob(0, 0).unwrap().prefix(100).unwrap(),
                [0, 1, 128, 255]
            );
        })
        .unwrap();
}

#[gpui::test]
fn lob_character_imports_are_strict_and_empty_values_differ_from_null(cx: &mut TestAppContext) {
    let file = LobFile::from_bytes("처음🙂".as_bytes(), false).unwrap();
    let handle = window(file, true, cx);
    let directory = tempfile::tempdir().unwrap();
    let invalid = directory.path().join("invalid.txt");
    std::fs::write(&invalid, [0xff, 0x00]).unwrap();
    handle
        .update(cx, |dialog, _, cx| dialog.import_path(invalid, cx))
        .unwrap();
    cx.run_until_parked();
    handle
        .update(cx, |dialog, _, cx| {
            assert!(dialog.error.is_some());
            assert!(!dialog.changed);
            assert_eq!(dialog.editor.read(cx).text(), "처음🙂");
            dialog.empty(false, cx);
            assert!(!dialog.is_null);
            dialog.stage(cx);
        })
        .unwrap();
    cx.run_until_parked();
    handle
        .update(cx, |dialog, _, cx| {
            assert!(dialog.grid.read(cx).source().lob(0, 0).unwrap().is_empty());
            dialog.empty(true, cx);
            dialog.stage(cx);
        })
        .unwrap();
    cx.run_until_parked();
    handle
        .update(cx, |dialog, _, cx| {
            assert_eq!(
                dialog.grid.read(cx).source().cell(0, 0),
                rugpui_grid::GridCell::Null
            );
            dialog.detach(cx);
            assert!(dialog.editor.read(cx).is_read_only());
            dialog.empty(false, cx);
            assert!(dialog.is_null);
            dialog.import(cx);
        })
        .unwrap();
    assert!(
        !cx.did_prompt_for_paths(),
        "read-only values cannot be replaced"
    );
}
