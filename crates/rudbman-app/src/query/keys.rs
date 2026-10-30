//! The pane's own chords: stopping a statement and ending a transaction.
//!
//! Bound in the `QueryPane` key context, which the whole pane — editor and
//! results — sits in, so each works wherever the keyboard is inside it.
//! `Escape` is shared with more than one layer above and below: the editor's
//! find bar and completion popup take it first (they are deeper), and when
//! nothing is running here the pane passes it on, so that it still reaches
//! the workspace's dialog and menu handling.
//!
//! `F11` and `F12` for commit and rollback are Oracle SQL Developer's, the one
//! convention among SQL workbenches that is not a letter chord: a `Ctrl+Alt`
//! letter is `AltGr` on a Windows keyboard with a national layout, and would
//! commit a transaction from under a user typing a `ć` into a string.

use gpui::{App, KeyBinding, actions};

actions!(
    query,
    [
        /// Stop the statement running in this pane, when one is.
        CancelRun,
        /// Commit the session's transaction from this pane.
        CommitTransaction,
        /// Roll the session's transaction back from this pane.
        RollbackTransaction,
    ]
);

/// The key context the whole pane renders in.
pub(super) const KEY_CONTEXT: &str = "QueryPane";

/// Marks the bindings as registered, so a second pane does not add them again.
struct Bindings;

impl gpui::Global for Bindings {}

/// Registers the pane's chords, once per application.
pub(super) fn init(cx: &mut App) {
    if cx.has_global::<Bindings>() {
        return;
    }
    cx.bind_keys([
        KeyBinding::new("escape", CancelRun, Some(KEY_CONTEXT)),
        KeyBinding::new("f11", CommitTransaction, Some(KEY_CONTEXT)),
        KeyBinding::new("f12", RollbackTransaction, Some(KEY_CONTEXT)),
    ]);
    cx.set_global(Bindings);
}
