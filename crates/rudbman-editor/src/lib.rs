//! The multi-line SQL editor: a rope, an incremental syntax cache, and a gpui
//! element that draws only what fits on screen.
//!
//! `rudbman-ui`'s [`TextInput`](rudbman_ui::TextInput) is a single line by
//! construction — it replaces `\n` with a space — so the editor is a new
//! widget rather than an extension of it. What carries over is the discipline,
//! not the code: byte offsets everywhere, UTF-16 only at the platform boundary,
//! grapheme clusters for every caret step, and an `EntityInputHandler` that the
//! IME can drive without ever being handed an offset that is not on a character
//! boundary. [`mod@editor`] documents each departure and why it is one.
//!
//! # The three things that make it hold at 100MB
//!
//! * **The buffer is a rope.** An insert is O(log n), and so are
//!   `byte <-> line` and `byte <-> UTF-16 code unit`. [`mod@buffer`].
//! * **The syntax cache is one [`LineState`](rudbman_sql::LineState) per
//!   line**, and an edit re-lexes from the edited line down to the first line
//!   whose end state is unchanged — which for an ordinary keystroke is the line
//!   itself. [`mod@highlight`].
//! * **Only the visible lines are shaped.** The element works out the row range
//!   from the scroll offset and shapes those and no others. [`mod@element`].
//!
//! The things a whole-buffer `&str` would be needed for — "which statement is
//! the caret in", "which bracket matches this one" — are answered over a window
//! of the rope cut at positions where the lexer is known to be in its start
//! state, so they cost the length of a statement rather than the length of the
//! script. [`mod@syntax`].
//!
//! # Using it
//!
//! ```ignore
//! rudbman_editor::init(cx);            // once, after rudbman_ui::init
//!
//! let editor = cx.new(|cx| EditorView::new(cx).dialect(Dialect::POSTGRES));
//! cx.subscribe(&editor, |_, editor, event: &EditorEvent, cx| {
//!     if let EditorEvent::RunStatement { span } = event {
//!         let text = editor.read(cx).text();
//!         let sql = span.sql(&text).to_owned();
//!         // hand it to the session
//!     }
//! })
//! .detach();
//! ```
//!
//! # Out of scope, deliberately
//!
//! Four features of the architecture document's §7.4 are not here, and are not
//! half-here either. Multiple cursors would change the shape of every command
//! in [`mod@editor`], so they go in as a list of selections in one piece or not
//! at all. Code folding needs a row-to-line map between the buffer and the
//! renderer, which nothing else wants yet. The completion popup needs the
//! schema index, which arrives with the session layer; the hook for it is one
//! variant of [`EditorEvent`], marked with a `TODO` at its declaration. A
//! minimap needs a second, coarser shaping pass, and is the least valuable of
//! the four.

#![warn(missing_docs)]

pub mod buffer;
pub mod editor;
pub mod element;
pub mod find;
pub mod highlight;
pub mod history;
pub mod syntax;

pub use buffer::Buffer;
pub use editor::{EditorEvent, EditorView, init};
pub use element::EditorElement;
pub use find::{FindState, find_all};
pub use highlight::Highlighter;
pub use history::{Edit, EditKind, History, SelectionState, Transaction};

#[cfg(test)]
mod tests;
