//! The draft the workspace saves, as [`QueryPane::workspace_sql`] hands it out.

use super::*;

/// An autosave follows every redraw, so an editor nobody touched must not be
/// copied again, and the copy it keeps must be one the workspace can compare
/// by pointer.
#[gpui::test]
fn an_unchanged_draft_is_shared_and_an_edit_is_copied_once(cx: &mut TestAppContext) {
    let (connected, profile) = h2("draft-shared", &[]);
    let window = pane(&connected, &profile, 500, cx);
    let (first, second) = window
        .update(cx, |pane, _window, cx| {
            (pane.workspace_sql(cx), pane.workspace_sql(cx))
        })
        .expect("the window is open");
    assert!(Arc::ptr_eq(&first, &second));
    window
        .update(cx, |pane, _window, cx| {
            pane.editor
                .update(cx, |editor, cx| editor.set_text("select 1", cx));
        })
        .expect("the window is open");
    window
        .update(cx, |pane, _window, cx| {
            let edited = pane.workspace_sql(cx);
            assert_eq!(&*edited, "select 1");
            assert!(Arc::ptr_eq(&edited, &pane.workspace_sql(cx)));
        })
        .expect("the window is open");
}

/// Past [`DRAFT_LIMIT`] the draft is saved empty, and the pane says so once
/// per crossing rather than on every autosave.
#[gpui::test]
fn an_oversized_draft_is_saved_empty_and_said_once(cx: &mut TestAppContext) {
    let (connected, profile) = h2("draft-oversized", &[]);
    let window = pane(&connected, &profile, 500, cx);
    let large = "-".repeat(DRAFT_LIMIT + 1);
    window
        .update(cx, |pane, _window, cx| {
            pane.editor
                .update(cx, |editor, cx| editor.set_text(&large, cx));
        })
        .expect("the window is open");
    window
        .update(cx, |pane, _window, cx| {
            assert_eq!(&*pane.workspace_sql(cx), "");
            assert!(pane.owes_draft_warning());
            pane.warn_oversized_draft(cx);
            assert_eq!(
                pane.notice.as_ref().map(|notice| notice.text.clone()),
                Some(ts!(
                    "query.draft_too_large",
                    limit = DRAFT_LIMIT / (1024 * 1024)
                ))
            );
            assert!(!pane.owes_draft_warning());
            pane.editor
                .update(cx, |editor, cx| editor.insert_at_caret("-", cx));
        })
        .expect("the window is open");
    window
        .update(cx, |pane, _window, cx| {
            assert_eq!(&*pane.workspace_sql(cx), "");
            assert!(!pane.owes_draft_warning(), "warned twice for one crossing");
            pane.editor
                .update(cx, |editor, cx| editor.set_text("select 1", cx));
        })
        .expect("the window is open");
    window
        .update(cx, |pane, _window, cx| {
            assert_eq!(&*pane.workspace_sql(cx), "select 1");
            assert!(!pane.owes_draft_warning());
        })
        .expect("the window is open");
}
