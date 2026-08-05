//! Headless tests for the editor as a whole: the entity, its input handler and
//! its element, driven through a real gpui window.
//!
//! The unit tests of the pieces live beside them — the rope in
//! [`crate::buffer`], the syntax cache in [`crate::highlight`], the grouping
//! rule in [`crate::history`], the matcher in [`crate::find`], the windowing in
//! [`crate::syntax`]. What is here is everything that only exists once there is
//! a focused element and a platform input handler attached to it, which is the
//! whole of the IME story and the whole of the virtualisation story.

use std::cell::RefCell;
use std::ops::Deref;
use std::rc::Rc;

use gpui::{
    Context, Entity, EntityInputHandler, Focusable, IntoElement, Render, TestAppContext,
    VisualTestContext, Window, div, prelude::*,
};
use rudbman_sql::Dialect;

use crate::editor::{EditorEvent, EditorView};

/// A view that does nothing but hold the editor, as a pane would.
struct Harness {
    editor: Entity<EditorView>,
}

impl Render for Harness {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div().size_full().child(self.editor.clone())
    }
}

/// Forces a redraw and waits for it.
///
/// The test platform draws on the effect cycle, so a frame is a refresh plus a
/// turn of the executor; the tests that count shaping work need one to have
/// happened.
fn draw(cx: &mut VisualTestContext) {
    cx.refresh().expect("the window is open");
    cx.run_until_parked();
}

/// The editor under test and what it announced.
struct Handles {
    editor: Entity<EditorView>,
    events: Rc<RefCell<Vec<EditorEvent>>>,
}

impl Handles {
    /// Reads something off the editor.
    fn read<R>(&self, cx: &mut VisualTestContext, f: impl FnOnce(&EditorView) -> R) -> R {
        cx.update(|_, cx| f(self.editor.read(cx)))
    }

    /// Mutates the editor.
    fn update<R>(
        &self,
        cx: &mut VisualTestContext,
        f: impl FnOnce(&mut EditorView, &mut Context<EditorView>) -> R,
    ) -> R {
        cx.update(|_, cx| self.editor.update(cx, f))
    }

    /// Mutates the editor with a window in hand, which the input handler needs.
    fn with_window<R>(
        &self,
        cx: &mut VisualTestContext,
        f: impl FnOnce(&mut EditorView, &mut Window, &mut Context<EditorView>) -> R,
    ) -> R {
        cx.update(|window, cx| self.editor.update(cx, |editor, cx| f(editor, window, cx)))
    }

    /// The buffer's text.
    fn text(&self, cx: &mut VisualTestContext) -> String {
        self.read(cx, EditorView::text)
    }

    /// The caret's byte offset.
    fn caret(&self, cx: &mut VisualTestContext) -> usize {
        self.read(cx, EditorView::caret)
    }

    /// The events emitted so far, draining them.
    fn drain_events(&self) -> Vec<EditorEvent> {
        self.events.borrow_mut().drain(..).collect()
    }
}

/// Opens a window holding an editor over `text`, focused.
fn open(text: &str, cx: &mut TestAppContext) -> (Handles, VisualTestContext) {
    cx.update(rudbman_ui::init);
    cx.update(crate::init);

    let events: Rc<RefCell<Vec<EditorEvent>>> = Rc::new(RefCell::new(Vec::new()));
    let text = text.to_owned();
    let window = cx.add_window({
        let events = events.clone();
        move |_, cx| {
            let editor = cx.new(|cx| {
                let mut editor = EditorView::new(cx);
                editor.set_text(&text, cx);
                editor.mark_clean(cx);
                editor
            });
            cx.subscribe(
                &editor,
                move |_: &mut Harness, _, event: &EditorEvent, _| {
                    events.borrow_mut().push(event.clone());
                },
            )
            .detach();
            Harness { editor }
        }
    });
    let editor = window
        .update(cx, |harness, _, _| harness.editor.clone())
        .expect("the window is open");

    let mut cx = VisualTestContext::from_window(*window.deref(), cx);
    cx.update(|window, cx| {
        let handle = editor.read(cx).focus_handle(cx);
        handle.focus(window);
    });
    cx.run_until_parked();

    (Handles { editor, events }, cx)
}

// --- editing -----------------------------------------------------------------

#[gpui::test]
fn typing_inserts_at_the_caret(cx: &mut TestAppContext) {
    let (editor, mut cx) = open("", cx);
    cx.simulate_input("select 1");
    assert_eq!(editor.text(&mut cx), "select 1");
    assert_eq!(editor.caret(&mut cx), 8);
    assert!(editor.read(&mut cx, |editor| editor.is_dirty()));
}

#[gpui::test]
fn enter_carries_the_indent_of_the_line_above(cx: &mut TestAppContext) {
    let (editor, mut cx) = open("    select 1", cx);
    editor.update(&mut cx, |editor, cx| editor.move_to(12, cx));
    cx.simulate_keystrokes("enter");
    cx.simulate_input("from t");
    assert_eq!(editor.text(&mut cx), "    select 1\n    from t");
}

#[gpui::test]
fn backspace_and_delete_step_by_grapheme(cx: &mut TestAppContext) {
    // A Hangul syllable is three bytes, a joined emoji eleven; one press takes
    // one of each, not one byte of either.
    let family = "\u{1f468}\u{200d}\u{1f469}\u{200d}\u{1f467}";
    let (editor, mut cx) = open(&format!("한{family}글"), cx);
    editor.update(&mut cx, |editor, cx| {
        let end = editor.text().len();
        editor.move_to(end, cx);
    });

    cx.simulate_keystrokes("backspace");
    assert_eq!(editor.text(&mut cx), format!("한{family}"));
    cx.simulate_keystrokes("backspace");
    assert_eq!(editor.text(&mut cx), "한");

    editor.update(&mut cx, |editor, cx| editor.move_to(0, cx));
    cx.simulate_keystrokes("delete");
    assert_eq!(editor.text(&mut cx), "");
}

#[gpui::test]
fn the_arrows_move_by_grapheme_not_by_byte(cx: &mut TestAppContext) {
    let (editor, mut cx) = open("한글", cx);
    editor.update(&mut cx, |editor, cx| editor.move_to(0, cx));
    cx.simulate_keystrokes("right");
    assert_eq!(editor.caret(&mut cx), 3);
    cx.simulate_keystrokes("right");
    assert_eq!(editor.caret(&mut cx), 6);
    cx.simulate_keystrokes("left");
    assert_eq!(editor.caret(&mut cx), 3);
}

#[gpui::test]
fn up_and_down_keep_the_goal_column(cx: &mut TestAppContext) {
    let (editor, mut cx) = open("aaaaaaaa\nbb\ncccccccc", cx);
    editor.update(&mut cx, |editor, cx| editor.move_to(6, cx));

    cx.simulate_keystrokes("down");
    assert_eq!(editor.caret(&mut cx), 11, "clamped to the short line");
    cx.simulate_keystrokes("down");
    assert_eq!(editor.caret(&mut cx), 18, "and back out to column six");
    cx.simulate_keystrokes("up up");
    assert_eq!(editor.caret(&mut cx), 6);
}

#[gpui::test]
fn shift_arrows_extend_the_selection(cx: &mut TestAppContext) {
    let (editor, mut cx) = open("select 1", cx);
    editor.update(&mut cx, |editor, cx| editor.move_to(0, cx));
    cx.simulate_keystrokes("shift-right shift-right shift-right");
    assert_eq!(editor.read(&mut cx, EditorView::selection), 0..3);

    // Typing over a selection replaces it.
    cx.simulate_input("X");
    assert_eq!(editor.text(&mut cx), "Xect 1");
}

#[gpui::test]
fn select_all_then_typing_replaces_the_buffer(cx: &mut TestAppContext) {
    let (editor, mut cx) = open("select 1;\nselect 2;", cx);
    cx.simulate_keystrokes("cmd-a ctrl-a");
    cx.simulate_input("x");
    assert_eq!(editor.text(&mut cx), "x");
}

#[gpui::test]
fn cut_copy_and_paste_go_through_the_clipboard(cx: &mut TestAppContext) {
    let (editor, mut cx) = open("select 1;\nselect 2;", cx);
    editor.update(&mut cx, |editor, cx| editor.select_range(0..9, cx));
    cx.simulate_keystrokes("cmd-x ctrl-x");
    assert_eq!(editor.text(&mut cx), "\nselect 2;");

    editor.update(&mut cx, |editor, cx| {
        let end = editor.text().len();
        editor.move_to(end, cx);
    });
    cx.simulate_keystrokes("cmd-v ctrl-v");
    assert_eq!(editor.text(&mut cx), "\nselect 2;select 1;");
}

#[gpui::test]
fn a_paste_keeps_its_line_breaks(cx: &mut TestAppContext) {
    let (editor, mut cx) = open("a\nb", cx);
    editor.update(&mut cx, |editor, cx| editor.select_range(0..3, cx));
    cx.simulate_keystrokes("cmd-c ctrl-c");
    editor.update(&mut cx, |editor, cx| editor.move_to(3, cx));
    cx.simulate_keystrokes("cmd-v ctrl-v");
    assert_eq!(editor.text(&mut cx), "a\nba\nb");
}

#[gpui::test]
fn tab_indents_a_block_and_shift_tab_takes_it_back(cx: &mut TestAppContext) {
    let (editor, mut cx) = open("select 1\nfrom t\nwhere x", cx);
    editor.update(&mut cx, |editor, cx| editor.select_range(0..20, cx));

    cx.simulate_keystrokes("tab");
    assert_eq!(
        editor.text(&mut cx),
        "    select 1\n    from t\n    where x"
    );
    cx.simulate_keystrokes("shift-tab");
    assert_eq!(editor.text(&mut cx), "select 1\nfrom t\nwhere x");
}

#[gpui::test]
fn tab_on_a_caret_is_an_indent_not_a_command(cx: &mut TestAppContext) {
    let (editor, mut cx) = open("ab", cx);
    editor.update(&mut cx, |editor, cx| editor.move_to(1, cx));
    cx.simulate_keystrokes("tab");
    assert_eq!(editor.text(&mut cx), "a    b");
}

#[gpui::test]
fn the_comment_toggle_takes_the_whole_selection(cx: &mut TestAppContext) {
    let (editor, mut cx) = open("select 1\n  from t\n\nwhere x", cx);
    editor.update(&mut cx, |editor, cx| editor.select_range(0..17, cx));

    cx.simulate_keystrokes("cmd-/ ctrl-/");
    assert_eq!(editor.text(&mut cx), "-- select 1\n--   from t\n\nwhere x");

    editor.update(&mut cx, |editor, cx| editor.select_range(0..23, cx));
    cx.simulate_keystrokes("cmd-/ ctrl-/");
    assert_eq!(editor.text(&mut cx), "select 1\n  from t\n\nwhere x");
}

#[gpui::test]
fn a_read_only_editor_refuses_every_change(cx: &mut TestAppContext) {
    let (editor, mut cx) = open("select 1", cx);
    editor.update(&mut cx, |editor, cx| editor.set_read_only(true, cx));

    cx.simulate_input("x");
    cx.simulate_keystrokes("backspace enter tab");
    assert_eq!(editor.text(&mut cx), "select 1");
    assert!(!editor.read(&mut cx, |editor| editor.is_dirty()));

    // Moving about still works, which is what makes it readable.
    cx.simulate_keystrokes("right right");
    assert_eq!(editor.caret(&mut cx), 2);
}

// --- undo and redo -----------------------------------------------------------

#[gpui::test]
fn a_run_of_typing_undoes_in_one_press(cx: &mut TestAppContext) {
    let (editor, mut cx) = open("", cx);
    cx.simulate_input("select");
    cx.simulate_keystrokes("cmd-z ctrl-z");
    assert_eq!(editor.text(&mut cx), "");
    cx.simulate_keystrokes("cmd-shift-z ctrl-shift-z");
    assert_eq!(editor.text(&mut cx), "select");
}

#[gpui::test]
fn a_caret_move_is_an_undo_boundary(cx: &mut TestAppContext) {
    let (editor, mut cx) = open("", cx);
    cx.simulate_input("sel");
    cx.simulate_keystrokes("left right");
    cx.simulate_input("ect");
    assert_eq!(editor.text(&mut cx), "select");

    cx.simulate_keystrokes("cmd-z ctrl-z");
    assert_eq!(editor.text(&mut cx), "sel");
    cx.simulate_keystrokes("cmd-z ctrl-z");
    assert_eq!(editor.text(&mut cx), "");
}

#[gpui::test]
fn a_paste_is_its_own_undo_step(cx: &mut TestAppContext) {
    let (editor, mut cx) = open("pasted", cx);
    editor.update(&mut cx, |editor, cx| editor.select_range(0..6, cx));
    cx.simulate_keystrokes("cmd-c ctrl-c");
    editor.update(&mut cx, |editor, cx| editor.move_to(6, cx));

    cx.simulate_input("ab");
    cx.simulate_keystrokes("cmd-v ctrl-v");
    cx.simulate_input("cd");
    assert_eq!(editor.text(&mut cx), "pastedabpastedcd");

    cx.simulate_keystrokes("cmd-z ctrl-z");
    assert_eq!(editor.text(&mut cx), "pastedabpasted");
    cx.simulate_keystrokes("cmd-z ctrl-z");
    assert_eq!(editor.text(&mut cx), "pastedab");
    cx.simulate_keystrokes("cmd-z ctrl-z");
    assert_eq!(editor.text(&mut cx), "pasted");
}

#[gpui::test]
fn undoing_a_block_indent_takes_every_line_back(cx: &mut TestAppContext) {
    let (editor, mut cx) = open("a\nb\nc", cx);
    editor.update(&mut cx, |editor, cx| editor.select_range(0..5, cx));
    cx.simulate_keystrokes("tab");
    assert_eq!(editor.text(&mut cx), "    a\n    b\n    c");
    cx.simulate_keystrokes("cmd-z ctrl-z");
    assert_eq!(editor.text(&mut cx), "a\nb\nc");
    cx.simulate_keystrokes("cmd-shift-z ctrl-shift-z");
    assert_eq!(editor.text(&mut cx), "    a\n    b\n    c");
}

#[gpui::test]
fn undo_puts_the_caret_back_where_the_typing_started(cx: &mut TestAppContext) {
    let (editor, mut cx) = open("select  from t", cx);
    editor.update(&mut cx, |editor, cx| editor.move_to(7, cx));
    cx.simulate_input("x");
    cx.simulate_keystrokes("cmd-z ctrl-z");
    assert_eq!(editor.caret(&mut cx), 7);
}

// --- the IME -----------------------------------------------------------------

/// Runs one composition step, the way a platform IME does.
fn compose(editor: &Handles, cx: &mut VisualTestContext, preview: &str) {
    editor.with_window(cx, |editor, window, cx| {
        editor.replace_and_mark_text_in_range(None, preview, None, window, cx);
    });
}

/// Commits a composition.
fn commit(editor: &Handles, cx: &mut VisualTestContext, text: &str) {
    editor.with_window(cx, |editor, window, cx| {
        editor.replace_text_in_range(None, text, window, cx);
    });
}

/// The marked range, in UTF-16 code units.
fn marked(editor: &Handles, cx: &mut VisualTestContext) -> Option<std::ops::Range<usize>> {
    editor.with_window(cx, |editor, window, cx| {
        editor.marked_text_range(window, cx)
    })
}

#[gpui::test]
fn a_hangul_syllable_composes_in_place(cx: &mut TestAppContext) {
    let (editor, mut cx) = open("", cx);

    // ㅎ -> 하 -> 한: three previews of the same syllable, each replacing the
    // last, then the commit.
    compose(&editor, &mut cx, "ㅎ");
    assert_eq!(editor.text(&mut cx), "ㅎ");
    assert_eq!(marked(&editor, &mut cx), Some(0..1));
    assert_eq!(editor.caret(&mut cx), 3, "in bytes, past the syllable");

    compose(&editor, &mut cx, "하");
    assert_eq!(editor.text(&mut cx), "하", "the preview replaced itself");
    assert_eq!(marked(&editor, &mut cx), Some(0..1));

    compose(&editor, &mut cx, "한");
    assert_eq!(editor.text(&mut cx), "한");
    assert_eq!(marked(&editor, &mut cx), Some(0..1));

    commit(&editor, &mut cx, "한");
    assert_eq!(editor.text(&mut cx), "한");
    assert_eq!(marked(&editor, &mut cx), None);
    assert_eq!(editor.caret(&mut cx), 3);
}

#[gpui::test]
fn a_composition_after_text_starts_where_the_caret_is(cx: &mut TestAppContext) {
    let (editor, mut cx) = open("select ", cx);
    editor.update(&mut cx, |editor, cx| editor.move_to(7, cx));

    compose(&editor, &mut cx, "ㅎ");
    // Seven ASCII bytes are seven UTF-16 units; the syllable is one.
    assert_eq!(marked(&editor, &mut cx), Some(7..8));
    compose(&editor, &mut cx, "한");
    commit(&editor, &mut cx, "한");
    assert_eq!(editor.text(&mut cx), "select 한");
    assert_eq!(editor.caret(&mut cx), 10);
}

#[gpui::test]
fn a_composition_past_the_basic_plane_counts_surrogates(cx: &mut TestAppContext) {
    // Four bytes, two UTF-16 units: an offset conversion that counted
    // characters would put the mark in the wrong place from here on.
    let (editor, mut cx) = open("\u{1f600}", cx);
    editor.update(&mut cx, |editor, cx| editor.move_to(4, cx));

    compose(&editor, &mut cx, "한");
    assert_eq!(marked(&editor, &mut cx), Some(2..3));
    commit(&editor, &mut cx, "한");
    assert_eq!(editor.text(&mut cx), "\u{1f600}한");
}

#[gpui::test]
fn a_whole_composition_is_one_undo_step(cx: &mut TestAppContext) {
    let (editor, mut cx) = open("", cx);
    compose(&editor, &mut cx, "ㅎ");
    compose(&editor, &mut cx, "하");
    compose(&editor, &mut cx, "한");
    commit(&editor, &mut cx, "한");
    compose(&editor, &mut cx, "ㄱ");
    compose(&editor, &mut cx, "글");
    commit(&editor, &mut cx, "글");
    assert_eq!(editor.text(&mut cx), "한글");

    cx.simulate_keystrokes("cmd-z ctrl-z");
    assert_eq!(editor.text(&mut cx), "한", "one syllable, not one jamo");
    cx.simulate_keystrokes("cmd-z ctrl-z");
    assert_eq!(editor.text(&mut cx), "");
}

#[gpui::test]
fn a_composition_over_a_selection_replaces_it(cx: &mut TestAppContext) {
    let (editor, mut cx) = open("select xx", cx);
    editor.update(&mut cx, |editor, cx| editor.select_range(7..9, cx));

    compose(&editor, &mut cx, "ㅎ");
    assert_eq!(editor.text(&mut cx), "select ㅎ");
    commit(&editor, &mut cx, "한");
    assert_eq!(editor.text(&mut cx), "select 한");

    cx.simulate_keystrokes("cmd-z ctrl-z");
    assert_eq!(editor.text(&mut cx), "select xx");
}

#[gpui::test]
fn an_empty_preview_cancels_the_composition(cx: &mut TestAppContext) {
    let (editor, mut cx) = open("ab", cx);
    editor.update(&mut cx, |editor, cx| editor.move_to(2, cx));

    compose(&editor, &mut cx, "ㅎ");
    assert_eq!(editor.text(&mut cx), "abㅎ");
    compose(&editor, &mut cx, "");
    assert_eq!(editor.text(&mut cx), "ab");
    assert_eq!(marked(&editor, &mut cx), None);
    assert_eq!(editor.caret(&mut cx), 2);
}

#[gpui::test]
fn a_caret_inside_a_preview_is_a_caret_and_not_a_selection(cx: &mut TestAppContext) {
    // This is the case gpui's own example gets wrong: a preview replacing a
    // preview, with a caret position inside it, as Windows sends.
    let (editor, mut cx) = open("select ", cx);
    editor.update(&mut cx, |editor, cx| editor.move_to(7, cx));

    compose(&editor, &mut cx, "ㅎ");
    editor.with_window(&mut cx, |editor, window, cx| {
        editor.replace_and_mark_text_in_range(None, "한", Some(1..1), window, cx);
    });
    assert_eq!(
        editor.read(&mut cx, EditorView::selection),
        10..10,
        "past the syllable, not across it"
    );
}

#[gpui::test]
fn the_selection_is_reported_in_utf16(cx: &mut TestAppContext) {
    let (editor, mut cx) = open("한글 sql", cx);
    editor.update(&mut cx, |editor, cx| editor.select_range(0..7, cx));

    let reported = editor.with_window(&mut cx, |editor, window, cx| {
        editor.selected_text_range(false, window, cx)
    });
    // Two syllables and a space: three units, not seven bytes.
    assert_eq!(reported.expect("a selection").range, 0..3);
}

#[gpui::test]
fn text_for_range_answers_in_utf16_too(cx: &mut TestAppContext) {
    let (editor, mut cx) = open("한글", cx);
    let mut actual = None;
    let text = editor.with_window(&mut cx, |editor, window, cx| {
        editor.text_for_range(1..2, &mut actual, window, cx)
    });
    assert_eq!(text.as_deref(), Some("글"));
    assert_eq!(actual, Some(1..2));
}

// --- syntax ------------------------------------------------------------------

#[gpui::test]
fn a_block_comment_opened_mid_buffer_propagates_downwards(cx: &mut TestAppContext) {
    let (editor, mut cx) = open("select 1;\nselect 2;\nselect 3;\n", cx);
    editor.update(&mut cx, |editor, cx| editor.move_to(0, cx));

    let inside =
        |editor: &EditorView, line: usize| !editor.highlighter().end_state(line).is_start();
    assert!(editor.read(&mut cx, |editor| !inside(editor, 1)));

    cx.simulate_input("/*");
    assert!(editor.read(&mut cx, |editor| inside(editor, 0)));
    assert!(editor.read(&mut cx, |editor| inside(editor, 2)));

    cx.simulate_input("*/");
    assert!(editor.read(&mut cx, |editor| !inside(editor, 0)));
    assert!(editor.read(&mut cx, |editor| !inside(editor, 2)));
}

#[gpui::test]
fn the_dialect_can_be_changed_under_the_buffer(cx: &mut TestAppContext) {
    let (editor, mut cx) = open("select `x` from t", cx);
    editor.update(&mut cx, |editor, cx| {
        editor.set_dialect(Dialect::MYSQL, cx);
    });
    assert_eq!(
        editor.read(&mut cx, EditorView::current_dialect).id(),
        Dialect::MYSQL.id()
    );
    // A backtick is a quoted identifier in MySQL and an error everywhere else;
    // either way the tokens still tile the line.
    let tokens = editor.read(&mut cx, |editor| {
        editor.highlighter().tokens(editor.buffer(), 0)
    });
    assert_eq!(tokens.last().expect("tokens").end, 17);
}

// --- running -----------------------------------------------------------------

#[gpui::test]
fn run_statement_emits_the_span_statement_at_would_cut(cx: &mut TestAppContext) {
    let script = "select 1;\n\nselect 2;\n";
    let (editor, mut cx) = open(script, cx);

    for offset in [3, 9, 10, 12, 19] {
        editor.update(&mut cx, |editor, cx| editor.move_to(offset, cx));
        editor.drain_events();
        cx.dispatch_action(crate::editor::RunStatement);
        cx.run_until_parked();

        let expected = rudbman_sql::statement_at(script, offset, &Dialect::GENERIC);
        let emitted = editor
            .drain_events()
            .into_iter()
            .find_map(|event| match event {
                EditorEvent::RunStatement { span } => Some(span),
                _ => None,
            });
        assert_eq!(emitted, expected, "at {offset}");
    }
}

#[gpui::test]
fn the_statement_under_the_caret_changes_at_a_semicolon(cx: &mut TestAppContext) {
    let script = "select 1;\nselect 2;\n";
    let (editor, mut cx) = open(script, cx);

    let sql = |editor: &Handles, cx: &mut VisualTestContext, at: usize| {
        editor.update(cx, |editor, cx| editor.move_to(at, cx));
        editor
            .read(cx, EditorView::statement_at_caret)
            .map(|span| span.sql(script).to_owned())
    };
    assert_eq!(sql(&editor, &mut cx, 3).as_deref(), Some("select 1"));
    assert_eq!(sql(&editor, &mut cx, 9).as_deref(), Some("select 1"));
    assert_eq!(sql(&editor, &mut cx, 13).as_deref(), Some("select 2"));
}

#[gpui::test]
fn run_selection_falls_back_to_the_statement(cx: &mut TestAppContext) {
    let (editor, mut cx) = open("select 1;\nselect 2;", cx);

    editor.update(&mut cx, |editor, cx| editor.select_range(0..8, cx));
    editor.drain_events();
    cx.dispatch_action(crate::editor::RunSelection);
    cx.run_until_parked();
    assert!(
        editor
            .drain_events()
            .contains(&EditorEvent::RunSelection { span: 0..8 })
    );

    editor.update(&mut cx, |editor, cx| editor.move_to(3, cx));
    editor.drain_events();
    cx.dispatch_action(crate::editor::RunSelection);
    cx.run_until_parked();
    assert!(
        editor
            .drain_events()
            .iter()
            .any(|event| matches!(event, EditorEvent::RunStatement { .. })),
        "an empty selection means the statement under the caret"
    );
}

// --- find --------------------------------------------------------------------

#[gpui::test]
fn find_is_case_insensitive_until_it_is_not(cx: &mut TestAppContext) {
    let (editor, mut cx) = open("Select select SELECT", cx);
    editor.update(&mut cx, |editor, cx| editor.set_find_query("select", cx));
    assert_eq!(editor.read(&mut cx, |editor| editor.matches().len()), 3);

    editor.update(&mut cx, |editor, cx| {
        editor.set_find_case_sensitive(true, cx);
    });
    assert_eq!(
        editor.read(&mut cx, |editor| editor.matches().to_vec()),
        vec![7..13]
    );
}

#[gpui::test]
fn f3_walks_the_matches_and_wraps(cx: &mut TestAppContext) {
    let (editor, mut cx) = open("aa bb aa bb aa", cx);
    editor.update(&mut cx, |editor, cx| {
        editor.move_to(0, cx);
        editor.set_find_query("aa", cx);
    });

    cx.dispatch_action(crate::editor::FindNext);
    assert_eq!(editor.read(&mut cx, EditorView::selection), 6..8);
    cx.dispatch_action(crate::editor::FindNext);
    assert_eq!(editor.read(&mut cx, EditorView::selection), 12..14);
    cx.dispatch_action(crate::editor::FindNext);
    assert_eq!(editor.read(&mut cx, EditorView::selection), 0..2);
    cx.dispatch_action(crate::editor::FindPrev);
    assert_eq!(editor.read(&mut cx, EditorView::selection), 12..14);
}

#[gpui::test]
fn replacing_one_corrects_the_offsets_of_the_rest(cx: &mut TestAppContext) {
    let (editor, mut cx) = open("aa bb aa bb aa", cx);
    editor.update(&mut cx, |editor, cx| {
        editor.move_to(0, cx);
        editor.set_find_query("aa", cx);
        editor.set_find_replacement("xxxxx", cx);
    });

    cx.dispatch_action(crate::editor::ReplaceNext);
    assert_eq!(editor.text(&mut cx), "xxxxx bb aa bb aa");
    // The two matches left have to have moved by three bytes each.
    assert_eq!(
        editor.read(&mut cx, |editor| editor.matches().to_vec()),
        vec![9..11, 15..17]
    );
}

#[gpui::test]
fn replace_all_rewrites_every_match_in_one_undo_step(cx: &mut TestAppContext) {
    let (editor, mut cx) = open("aa bb aa bb aa", cx);
    editor.update(&mut cx, |editor, cx| {
        editor.set_find_query("aa", cx);
        editor.set_find_replacement("z", cx);
    });

    cx.dispatch_action(crate::editor::ReplaceAll);
    assert_eq!(editor.text(&mut cx), "z bb z bb z");
    cx.simulate_keystrokes("cmd-z ctrl-z");
    assert_eq!(editor.text(&mut cx), "aa bb aa bb aa");
}

// --- virtualisation and cost -------------------------------------------------

/// A script of `lines` statements, one per line.
fn long_script(lines: usize) -> String {
    let mut text = String::with_capacity(lines * 20);
    for line in 0..lines {
        text.push_str("select ");
        text.push_str(&line.to_string());
        text.push_str(" from t;\n");
    }
    text
}

#[gpui::test]
fn drawing_a_hundred_thousand_lines_styles_only_the_visible_ones(cx: &mut TestAppContext) {
    let (editor, mut cx) = open(&long_script(100_000), cx);
    editor.update(&mut cx, |editor, cx| editor.move_to(0, cx));
    draw(&mut cx);

    let before = editor.read(&mut cx, |editor| editor.highlighter().lex_calls());
    draw(&mut cx);
    let per_frame = editor.read(&mut cx, |editor| editor.highlighter().lex_calls()) - before;

    // One call per visible line, plus the bounded windows the statement
    // highlight and the bracket scan cost. A frame that touched the buffer
    // would be a hundred thousand.
    assert!(
        per_frame > 0 && per_frame < 200,
        "one frame lexed {per_frame} lines of a hundred thousand"
    );
}

#[gpui::test]
fn one_keystroke_in_a_hundred_thousand_lines_relexes_a_constant_number(cx: &mut TestAppContext) {
    let (editor, mut cx) = open(&long_script(100_000), cx);
    editor.update(&mut cx, |editor, cx| {
        let at = editor.buffer().line_start(50_000) + 7;
        editor.move_to(at, cx);
    });
    draw(&mut cx);

    // Two runs of different lengths with the same surroundings, so that the
    // frame the harness draws around them cancels out and what is left is the
    // marginal cost of one keystroke.
    let mut count = |presses: usize| {
        let before = editor.read(&mut cx, |editor| editor.highlighter().lex_calls());
        editor.with_window(&mut cx, |editor, window, cx| {
            for _ in 0..presses {
                editor.replace_text_in_range(None, "x", window, cx);
            }
        });
        editor.read(&mut cx, |editor| editor.highlighter().lex_calls()) - before
    };
    let short = count(100);
    let long = count(1_000);
    let per_keystroke = (long - short) / 900;

    assert!(
        per_keystroke <= 3,
        "one keystroke re-lexed {per_keystroke} lines of a hundred thousand"
    );
}

#[gpui::test]
fn typing_in_a_hundred_thousand_lines_stays_quick(cx: &mut TestAppContext) {
    let (editor, mut cx) = open(&long_script(100_000), cx);
    editor.update(&mut cx, |editor, cx| {
        let at = editor.buffer().line_start(50_000) + 7;
        editor.move_to(at, cx);
    });

    let started = std::time::Instant::now();
    editor.with_window(&mut cx, |editor, window, cx| {
        for _ in 0..500 {
            editor.replace_text_in_range(None, "x", window, cx);
        }
    });
    let each = started.elapsed() / 500;

    // Generous by two orders of magnitude against what it measures, because
    // this runs on whatever machine CI has; what it is really holding down is
    // that nothing on the edit path is linear in the buffer.
    assert!(
        each < std::time::Duration::from_millis(1),
        "a keystroke took {each:?}"
    );
}

#[gpui::test]
fn setting_the_text_clears_the_history_and_the_dirty_flag(cx: &mut TestAppContext) {
    let (editor, mut cx) = open("select 1", cx);
    cx.simulate_input("x");
    assert!(editor.read(&mut cx, |editor| editor.is_dirty()));

    editor.update(&mut cx, |editor, cx| editor.set_text("select 2", cx));
    assert!(!editor.read(&mut cx, |editor| editor.is_dirty()));
    cx.simulate_keystrokes("cmd-z ctrl-z");
    assert_eq!(editor.text(&mut cx), "select 2", "undo does not cross it");
}

// --- the mouse ---------------------------------------------------------------

#[gpui::test]
fn a_double_click_selects_a_word_and_a_triple_click_a_line(cx: &mut TestAppContext) {
    use gpui::{Modifiers, MouseButton, MouseDownEvent, MouseUpEvent, Point, px};

    let (editor, mut cx) = open("select count from t\nsecond line", cx);
    draw(&mut cx);

    let line_height = editor.read(&mut cx, |editor| editor.layout.line_height);
    let gutter = editor.read(&mut cx, |editor| editor.layout.gutter);
    let position = Point {
        // Somewhere inside `count`, which starts at column seven.
        x: gutter + px(70.),
        y: line_height / 2.,
    };
    let click = |cx: &mut VisualTestContext, count: usize| {
        cx.simulate_event(MouseDownEvent {
            position,
            modifiers: Modifiers::none(),
            button: MouseButton::Left,
            click_count: count,
            first_mouse: false,
        });
        cx.simulate_event(MouseUpEvent {
            position,
            modifiers: Modifiers::none(),
            button: MouseButton::Left,
            click_count: count,
        });
        cx.run_until_parked();
    };

    click(&mut cx, 2);
    let word = editor.read(&mut cx, EditorView::selection);
    assert!(!word.is_empty(), "a double click selects something");
    assert!(
        word.start >= 7 && word.end <= 19,
        "and it is on the first line: {word:?}"
    );

    click(&mut cx, 3);
    assert_eq!(
        editor.read(&mut cx, EditorView::selection),
        0..20,
        "a triple click takes the whole line"
    );
}
