//! "Format SQL": the selection, or the whole script, laid out again.
//!
//! The layout itself is [`rudbman_sql::format_sql`]'s; what this module adds is
//! the editor side of it — which text, one undo step, and where the caret and
//! selection land afterwards.
use gpui::{Global, KeyBinding, actions};

use super::*;

actions!(query_format, [FormatSql]);
struct Bindings;
impl Global for Bindings {}

/// Binds `Ctrl+Shift+F` (`Cmd+Shift+F` on macOS) in the query editor.
///
/// Free in both the editor widget, whose find bar is the unshifted chord, and
/// the shell.
pub(super) fn init(cx: &mut App) {
    if !cx.has_global::<Bindings>() {
        let modifier = if cfg!(target_os = "macos") {
            "cmd"
        } else {
            "ctrl"
        };
        cx.bind_keys([KeyBinding::new(
            &format!("{modifier}-shift-f"),
            FormatSql,
            Some("QueryEditor"),
        )]);
        cx.set_global(Bindings);
    }
}

impl QueryPane {
    /// Formats the selection, or the whole buffer when nothing is selected, as
    /// one undo step.
    ///
    /// A formatted selection stays selected. Without one the caret keeps its
    /// place in the SQL: formatting changes only whitespace and letter case, so
    /// the count of other characters before it identifies the same spot.
    pub(super) fn format_sql(&mut self, cx: &mut Context<Self>) {
        let dialect = self.dialect;
        self.editor.update(cx, |editor, cx| {
            if editor.is_read_only() {
                return;
            }
            let selection = editor.selection();
            if selection.is_empty() {
                let source = editor.text();
                let formatted = rudbman_sql::format_sql(&source, &dialect);
                if formatted != source {
                    let caret = same_spot(&source, &formatted, editor.caret());
                    editor.replace_range(0..source.len(), &formatted, cx);
                    editor.move_to(caret, cx);
                }
            } else {
                let source = editor.text_in(selection.clone());
                let formatted = rudbman_sql::format_sql(&source, &dialect);
                if formatted != source {
                    editor.replace_range(selection.clone(), &formatted, cx);
                    editor.select_range(selection.start..selection.start + formatted.len(), cx);
                }
            }
        });
    }
}

/// The offset in `formatted` that `offset` in `source` corresponds to.
///
/// Both strings hold the same characters once whitespace is set aside. A caret
/// right after one of them stays right after it; a caret in whitespace moves to
/// just before the next one.
fn same_spot(source: &str, formatted: &str, offset: usize) -> usize {
    let before = &source[..offset.min(source.len())];
    let wanted = before.chars().filter(|c| !c.is_whitespace()).count();
    let after_char = before
        .chars()
        .next_back()
        .is_some_and(|c| !c.is_whitespace());
    let mut seen = 0;
    for (i, c) in formatted.char_indices() {
        if c.is_whitespace() {
            continue;
        }
        if seen == wanted {
            return i;
        }
        seen += 1;
        if after_char && seen == wanted {
            return i + c.len_utf8();
        }
    }
    formatted.len()
}

#[cfg(test)]
mod tests {
    use super::same_spot;

    #[test]
    fn the_caret_keeps_its_place_in_the_sql() {
        let source = "select a, b from t";
        let formatted = "SELECT a,\n    b\nFROM t";
        assert_eq!(same_spot(source, formatted, 0), 0);
        let at_b = source.find('b').unwrap();
        assert_eq!(
            &formatted[same_spot(source, formatted, at_b)..],
            "b\nFROM t"
        );
        assert_eq!(same_spot(source, formatted, source.len()), formatted.len());
        let after_comma = source.find(',').unwrap() + 1;
        assert_eq!(same_spot(source, formatted, after_comma), "SELECT a,".len());
        let in_space = source.find(" from").unwrap() + 1;
        assert_eq!(
            &formatted[same_spot(source, formatted, in_space)..],
            "FROM t"
        );
    }
}
