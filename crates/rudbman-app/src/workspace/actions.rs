//! Actions.

use super::*;

/// `handler`, held back while [`Workspace::shortcuts_held`] says so.
///
/// Wrapped around the workspace's own commands where the root registers them
/// rather than written into each, so that a command added later is held back
/// by being registered the same way — and the one handler that must *not* be,
/// [`Workspace::dismiss_dialog_action`], is the one registered without it.
pub(super) fn unless_held<A: gpui::Action>(
    handler: fn(&mut Workspace, &A, &mut Window, &mut Context<Workspace>),
) -> impl Fn(&mut Workspace, &A, &mut Window, &mut Context<Workspace>) + 'static {
    move |workspace, action, window, cx| {
        if workspace.shortcuts_held(cx) {
            return;
        }
        handler(workspace, action, window, cx);
    }
}

impl Workspace {
    /// Opens the connection dialog.
    pub(super) fn new_connection_action(
        &mut self,
        _: &NewConnection,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.set_menu_open(false, cx);
        self.open_connect(window, cx);
    }

    /// `F10`: the application menu, on its first row.
    ///
    /// Off macOS only, which is where the ☰ menu is: on macOS the commands are
    /// the menu bar's, and the key is left to whatever it means there. Held
    /// like the other shell commands while a dialog or a sheet is up.
    pub(super) fn open_app_menu_action(
        &mut self,
        _: &rugpui::OpenAppMenu,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if cfg!(target_os = "macos") || self.shortcuts_held(cx) {
            cx.propagate();
            return;
        }
        self.set_menu_open(true, cx);
        self.menu_by_keyboard = true;
        cx.notify();
    }

    /// Opens the settings dialog.
    pub(super) fn open_settings_action(
        &mut self,
        _: &OpenSettings,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_settings(window, cx);
    }

    /// Opens the about dialog.
    pub(super) fn show_about_action(
        &mut self,
        _: &ShowAbout,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_about(window, cx);
    }

    /// Handles the "Check for updates" menu item.
    pub(super) fn check_updates_action(
        &mut self,
        _: &CheckUpdates,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.check_updates(window, cx);
    }

    /// Shows or hides the explorer sidebar.
    pub(super) fn toggle_explorer_action(
        &mut self,
        _: &ToggleExplorer,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.set_menu_open(false, cx);
        self.toggle_explorer(window, cx);
    }

    /// Opens an empty query pane on the connection whose tab is showing.
    pub(super) fn new_query_action(
        &mut self,
        _: &NewQuery,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.set_menu_open(false, cx);
        if self.say_not_live(cx) {
            self.open_query("", window, cx);
        }
    }

    /// Opens a query pane over the object selected in the explorer.
    ///
    /// Scoped to the sidebar's key context, so the same chord means "run the
    /// statement" once the focus is in a SQL editor.
    pub(super) fn query_object_action(
        &mut self,
        _: &QueryObject,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.set_menu_open(false, cx);
        let Some(target) = self.explorer.read(cx).selected_relation(cx) else {
            return;
        };
        self.open_query_for(&target, window, cx);
    }

    /// Reads a `.sql` file into a query pane.
    pub(super) fn open_sql_file_action(
        &mut self,
        _: &OpenSqlFile,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.set_menu_open(false, cx);
        self.open_sql_file(window, cx);
    }

    /// Writes the query tab on screen to its file.
    pub(super) fn save_sql_file_action(
        &mut self,
        _: &SaveSqlFile,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.set_menu_open(false, cx);
        self.save_sql_file(false, window, cx);
    }

    /// Writes the query tab on screen to a file chosen anew.
    pub(super) fn save_sql_file_as_action(
        &mut self,
        _: &SaveSqlFileAs,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.set_menu_open(false, cx);
        self.save_sql_file(true, window, cx);
    }

    /// Opens the extraction dialog over the object selected in the explorer.
    ///
    /// Gated exactly as [`Workspace::query_object_action`] is: without a
    /// relation selected there is nothing to extract, and a dialog that opened
    /// on no object would have to invent one.
    pub(super) fn extract_script_action(
        &mut self,
        _: &ExtractScript,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.set_menu_open(false, cx);
        let Some(target) = self.explorer.read(cx).selected_relation(cx) else {
            return;
        };
        self.open_extract(target, window, cx);
    }

    /// Opens the transfer dialog over the object selected in the explorer.
    ///
    /// Gated exactly as [`Workspace::extract_script_action`] is: a transfer
    /// reads one relation's rows, so without one selected there is nothing to
    /// copy.
    pub(super) fn transfer_table_action(
        &mut self,
        _: &TransferTable,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.set_menu_open(false, cx);
        let Some(target) = self.explorer.read(cx).selected_relation(cx) else {
            return;
        };
        self.open_transfer(target, window, cx);
    }

    /// Opens the backup dialog over the scope the explorer's selection sits in.
    ///
    /// Gated one level wider than the transfer, exactly as
    /// [`Workspace::open_erd_action`] is: a backup is of a *scope*, so a
    /// schema, a folder and a table all name one and the connection root does
    /// not.
    pub(super) fn backup_schema_action(
        &mut self,
        _: &BackupSchema,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.set_menu_open(false, cx);
        let Some((connection, scope)) = self.explorer.read(cx).selected_scope(cx) else {
            return;
        };
        self.open_backup(connection, scope, window, cx);
    }

    /// Draws the ERD of the scope the explorer's selection sits in.
    ///
    /// Wired exactly as [`Workspace::extract_script_action`] is, and gated one
    /// level wider: a diagram is of a *scope*, so a schema, a folder and a
    /// table all name one and the connection root does not. The panel opens on
    /// the connection the selected node belongs to rather than on the tab
    /// showing, which is the same thing today and would not be if the sidebar
    /// ever drew more than one root.
    pub(super) fn open_erd_action(
        &mut self,
        _: &OpenErd,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.set_menu_open(false, cx);
        let Some((connection, scope)) = self.explorer.read(cx).selected_scope(cx) else {
            return;
        };
        self.open_erd(ErdTarget { connection, scope }, window, cx);
    }

    /// Puts the object selected in the explorer onto a query builder.
    ///
    /// Gated exactly as [`Workspace::query_object_action`] is, and on the same
    /// selection: a builder holds relations, so a routine or a sequence names
    /// nothing it could add.
    pub(super) fn add_to_builder_action(
        &mut self,
        _: &AddToBuilder,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.set_menu_open(false, cx);
        let Some(target) = self.explorer.read(cx).selected_relation(cx) else {
            return;
        };
        self.add_to_builder(target, window, cx);
    }

    /// Opens an empty query builder on the connection whose tab is showing.
    pub(super) fn new_builder_action(
        &mut self,
        _: &NewBuilder,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.set_menu_open(false, cx);
        if self.say_not_live(cx) {
            self.open_builder(window, cx);
        }
    }

    /// Closes the tab on top of the active pane, through every guard a close
    /// passes.
    pub(super) fn close_tab_action(
        &mut self,
        _: &CloseTab,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(pane) = self.active_pane() else {
            return;
        };
        let Some(index) = self
            .work_area()
            .and_then(|area| area.panes.get(pane))
            .filter(|pane| !pane.is_empty())
            .map(Pane::active_index)
        else {
            return;
        };
        self.close_tab(pane, index, window, cx);
    }

    /// Brings back the query or builder tab closed most recently.
    pub(super) fn reopen_closed_tab_action(
        &mut self,
        _: &ReopenClosedTab,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.reopen_closed_tab(window, cx);
    }

    /// Closes the active pane.
    pub(super) fn close_pane_action(
        &mut self,
        _: &ClosePane,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.close_active_pane(window, cx);
    }

    /// Moves the pane marker forwards.
    pub(super) fn focus_next_pane_action(
        &mut self,
        _: &FocusNextPane,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.cycle_pane(true, window, cx);
    }

    /// Moves the pane marker backwards.
    pub(super) fn focus_prev_pane_action(
        &mut self,
        _: &FocusPrevPane,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.cycle_pane(false, window, cx);
    }

    /// The next tab of the active pane.
    pub(super) fn next_tab_action(
        &mut self,
        _: &NextTab,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.cycle_tab(true, window, cx);
    }

    /// The previous tab of the active pane.
    pub(super) fn prev_tab_action(
        &mut self,
        _: &PrevTab,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.cycle_tab(false, window, cx);
    }

    /// Tab `n` of the active pane.
    pub(super) fn select_tab_action(
        &mut self,
        action: &SelectTab,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.select_tab(action.0, window, cx);
    }

    /// The next connection tab.
    pub(super) fn next_connection_action(
        &mut self,
        _: &NextConnection,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.cycle_connection(true, window, cx);
    }

    /// The previous connection tab.
    pub(super) fn prev_connection_action(
        &mut self,
        _: &PrevConnection,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.cycle_connection(false, window, cx);
    }

    /// The next region, `F6`.
    pub(super) fn next_region_action(
        &mut self,
        _: &NextRegion,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.cycle_region(true, window, cx);
    }

    /// The previous region, `Shift+F6`.
    pub(super) fn prev_region_action(
        &mut self,
        _: &PrevRegion,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.cycle_region(false, window, cx);
    }

    /// Splits the active pane to the right.
    pub(super) fn split_right_action(
        &mut self,
        _: &SplitRight,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.split_active(Axis::Horizontal, window, cx);
    }

    /// Splits the active pane downwards.
    pub(super) fn split_below_action(
        &mut self,
        _: &SplitBelow,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.split_active(Axis::Vertical, window, cx);
    }

    /// Closes whatever overlay is on top, in the order they are stacked.
    ///
    /// Context menus and the application dropdown are not among them: an open
    /// one takes `Escape` itself, ahead of every binding, and this never sees
    /// the key (architecture document, §7.8).
    pub(super) fn dismiss_dialog_action(
        &mut self,
        _: &DismissDialog,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.rename_tab.is_some() {
            self.finish_tab_name(false, window, cx);
            return;
        }
        if self.close_question.is_some() {
            // "Cancel": nothing closes, and the tab the question brought
            // forward keeps the keyboard if it had it.
            self.answer_close(closing::CloseAnswer::Cancel, window, cx);
            return;
        }
        if self.confirm.is_some() {
            self.answer_confirm(false, window, cx);
            return;
        }
        if self.update.read(cx).is_open() {
            // Swallowed rather than propagated while an install runs: the key
            // must not reach a pane, but nothing may take the screen from a
            // swap either, so `Escape` simply does nothing until it is over.
            if !self.update.read(cx).is_busy() {
                self.update.update(cx, |dialog, cx| dialog.close(cx));
                self.restore_focus(window, cx);
            }
            return;
        }
        if self.about.read(cx).is_open() {
            self.about.update(cx, |dialog, cx| dialog.close(cx));
            self.restore_focus(window, cx);
            return;
        }
        if self.connect.read(cx).is_open() {
            // The card answers `Escape` itself while the focus is inside it
            // (its modal's guard peels the same layers); this is the way in
            // for a focus on the dialog's own handle, outside the card. Routed
            // through the dialog for the settings one's reason: it stacks a
            // driver manager, a dropdown and a delete confirmation, and each
            // of those has to be able to take `Escape` for itself before the
            // whole form is thrown away.
            self.connect.update(cx, |dialog, cx| dialog.escape(cx));
            return;
        }
        if self.settings.read(cx).is_open() {
            // Routed through the dialog rather than closed from here: it stacks
            // a color editor, two dropdowns and a delete confirmation of its
            // own, and each of those has to be able to take `Escape` for itself
            // before the whole form is thrown away. gpui matches key bindings
            // ahead of key listeners, so this handler — not the dialog's own —
            // is where the key actually lands.
            self.settings.update(cx, |dialog, cx| dialog.escape(cx));
            return;
        }
        if self.extract.read(cx).is_open() {
            // Routed through the dialog: it stacks a dropdown, and while a job
            // is running `Escape` asks whether to stop it rather than closing
            // anything — dismissing the card would cancel a job the user may
            // have waited minutes on.
            self.extract.update(cx, |dialog, cx| dialog.escape(cx));
            return;
        }
        if self.transfer.read(cx).is_open() {
            // Routed through the dialog for the extraction's reasons, and it
            // stacks three dropdowns rather than one.
            self.transfer.update(cx, |dialog, cx| dialog.escape(cx));
            return;
        }
        if self.backup.read(cx).is_open() {
            // No dropdown of its own, but a running job still has to take
            // `Escape` as its question rather than as a close.
            self.backup.update(cx, |dialog, cx| dialog.escape(cx));
            return;
        }
        cx.propagate();
    }
}
