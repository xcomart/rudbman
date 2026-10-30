//! The file a pane is bound to, and the two halves of a save around the write.

use super::*;
use crate::query::file::SaveOutcome;

/// A second save is refused, and said so, while the first is writing; a save
/// that comes back to an editor edited meanwhile binds the file but leaves the
/// pane marked — even once the edit is undone, since the editor's clean point
/// is still the text from before the save, which the file no longer holds.
#[gpui::test]
fn a_save_overtaken_by_an_edit_leaves_the_pane_marked(cx: &mut TestAppContext) {
    let (connected, profile) = h2("file-overtaken", &[]);
    let window = pane(&connected, &profile, 500, cx);
    let path = std::path::PathBuf::from("/nowhere/overtaken.sql");
    window
        .update(cx, |pane, _window, cx| {
            pane.editor
                .update(cx, |editor, cx| editor.set_text("select 1", cx));
        })
        .expect("the window is open");
    let job = window
        .update(cx, |pane, _window, cx| {
            let job = pane.begin_save(cx).expect("nothing else is saving");
            assert!(pane.begin_save(cx).is_none(), "one save at a time");
            assert!(pane.notice.is_some(), "the refusal is said");
            job
        })
        .expect("the window is open");
    assert_eq!(job.text, "select 1");
    window
        .update(cx, |pane, _window, cx| {
            pane.editor
                .update(cx, |editor, cx| editor.insert_at_caret("-- ", cx));
        })
        .expect("the window is open");
    window
        .update(cx, |pane, _window, cx| {
            let saved = pane.finish_save(path.clone(), job.edits, SaveOutcome::Written(None), cx);
            assert_eq!(saved, Saved::Rebound);
            assert_eq!(pane.file(), Some(path.as_path()));
            assert!(pane.is_modified(cx), "the edit is not in the file");
        })
        .expect("the window is open");
    let mut visual = gpui::VisualTestContext::from_window(window.into(), cx);
    window
        .update(&mut visual, |pane, window, cx| {
            pane.focus_editor(window, cx)
        })
        .expect("the window is open");
    visual.simulate_keystrokes(if cfg!(target_os = "macos") {
        "cmd-z"
    } else {
        "ctrl-z"
    });
    window
        .update(cx, |pane, _window, cx| {
            assert_eq!(pane.editor_text(cx), "select 1");
            assert!(
                pane.is_modified(cx),
                "back at the old clean point, which the file does not hold"
            );
            let again = pane.begin_save(cx).expect("the first save is done");
            let saved = pane.finish_save(path.clone(), again.edits, SaveOutcome::Written(None), cx);
            assert_eq!(saved, Saved::InPlace, "the same file again");
            assert!(!pane.is_modified(cx));
        })
        .expect("the window is open");
}

/// A failed save keeps the binding and says why. Failing on the bound file
/// marks the pane, since a broken-off write may have left the file holding
/// neither text; failing on another path leaves the bound file's mark alone.
/// Either way the next save may try again.
#[gpui::test]
fn a_failed_save_keeps_the_binding_and_says_why(cx: &mut TestAppContext) {
    let (connected, profile) = h2("file-failed", &[]);
    let window = pane(&connected, &profile, 500, cx);
    let bound = std::path::PathBuf::from("/nowhere/bound.sql");
    window
        .update(cx, |pane, _window, cx| {
            pane.bind_file(bound.clone(), None, cx);
            let job = pane.begin_save(cx).unwrap();
            let failure = SaveOutcome::Failed(std::io::Error::other("disk full"));
            let other = std::path::PathBuf::from("/nowhere/other.sql");
            assert_eq!(
                pane.finish_save(other, job.edits, failure, cx),
                Saved::Failed
            );
            assert_eq!(pane.file(), Some(bound.as_path()));
            assert!(!pane.is_modified(cx), "the bound file was not touched");
            let notice = pane.notice.clone().expect("the failure is said");
            assert!(notice.contains("disk full"), "{notice}");

            let job = pane.begin_save(cx).expect("the next save may try");
            let failure = SaveOutcome::Failed(std::io::Error::other("disk full"));
            assert_eq!(
                pane.finish_save(bound.clone(), job.edits, failure, cx),
                Saved::Failed
            );
            assert!(pane.is_modified(cx), "the bound file may be broken");
        })
        .expect("the window is open");
}

/// A save that found its file changed binds nothing new and says so.
#[gpui::test]
fn a_changed_file_is_reported_and_not_rebound(cx: &mut TestAppContext) {
    let (connected, profile) = h2("file-changed", &[]);
    let window = pane(&connected, &profile, 500, cx);
    let bound = std::path::PathBuf::from("/nowhere/bound.sql");
    window
        .update(cx, |pane, _window, cx| {
            pane.bind_file(bound.clone(), None, cx);
            let job = pane.begin_save(cx).unwrap();
            assert_eq!(
                pane.finish_save(bound.clone(), job.edits, SaveOutcome::Changed, cx),
                Saved::Changed
            );
            assert_eq!(pane.file(), Some(bound.as_path()));
            assert!(pane.notice.is_some());
            assert!(pane.begin_save(cx).is_some(), "the save dialog may write");
        })
        .expect("the window is open");
}

/// Filling a restored pane from its file: a clean draft gives way to what the
/// file holds now, saves wait for the read, and what was typed meanwhile is
/// kept, still bound but with the file forgotten so a save asks first.
#[gpui::test]
fn a_reload_shows_the_file_unless_the_draft_was_typed_into(cx: &mut TestAppContext) {
    let (connected, profile) = h2("file-reload", &[]);
    let window = pane(&connected, &profile, 500, cx);
    let bound = std::path::PathBuf::from("/nowhere/large.sql");
    let stamped = |bytes: &[u8]| Ok((bytes.to_vec(), None));
    window
        .update(cx, |pane, _window, cx| {
            pane.editor
                .update(cx, |editor, cx| editor.set_text("select 1", cx));
            pane.restore_file(bound.clone(), false, None, cx);
            pane.begin_reload();
            assert!(pane.begin_save(cx).is_none(), "saves wait for the read");
            pane.reload_file("select 1", stamped(b"select 2"), cx);
            assert_eq!(pane.editor_text(cx), "select 2", "the file moved on");
            assert!(!pane.is_modified(cx));
            assert!(pane.begin_save(cx).is_some());
        })
        .expect("the window is open");

    let window = pane(&connected, &profile, 500, cx);
    window
        .update(cx, |pane, _window, cx| {
            pane.restore_file(bound.clone(), false, None, cx);
            pane.begin_reload();
            pane.editor
                .update(cx, |editor, cx| editor.set_text("typed", cx));
            pane.reload_file("", stamped(b"select 42"), cx);
            assert_eq!(pane.editor_text(cx), "typed", "typed text wins");
            assert_eq!(pane.file(), Some(bound.as_path()));
            assert!(pane.is_modified(cx));
            assert!(pane.file_stamp().is_none(), "a save has to ask");
        })
        .expect("the window is open");
}

/// Bytes that are no longer UTF-8 are shown as opening them would show them,
/// and unbind the pane; a file that has gone keeps the binding.
#[gpui::test]
fn a_reload_unbinds_on_bad_bytes_and_keeps_a_gone_file(cx: &mut TestAppContext) {
    let (connected, profile) = h2("file-reload-bytes", &[]);
    let window = pane(&connected, &profile, 500, cx);
    let bound = std::path::PathBuf::from("/nowhere/latin1.sql");
    window
        .update(cx, |pane, _window, cx| {
            let stamp = Some(FileStamp::of_parts(9, None));
            pane.restore_file(bound.clone(), true, stamp, cx);
            pane.begin_reload();
            let gone = std::io::Error::from(std::io::ErrorKind::NotFound);
            pane.reload_file("", Err(gone), cx);
            assert_eq!(pane.file(), Some(bound.as_path()));
            assert!(
                pane.file_stamp().is_none(),
                "a file that comes back is not one this pane has shown"
            );

            pane.begin_reload();
            pane.reload_file("", Ok((vec![b'x', 0xE9], None)), cx);
            assert!(pane.editor_text(cx).contains('\u{fffd}'));
            assert!(pane.file().is_none(), "replaced bytes unbind the pane");
            assert!(!pane.is_modified(cx));
        })
        .expect("the window is open");
}
