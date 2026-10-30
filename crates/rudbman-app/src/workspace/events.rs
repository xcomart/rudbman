//! Routes child events to workspace operations.
use super::*;

impl Workspace {
    pub(super) fn on_about_event(
        &mut self,
        dialog: &Entity<AboutDialog>,
        event: &AboutDialogEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            AboutDialogEvent::Dismissed => {
                dialog.update(cx, |dialog, cx| dialog.close(cx));
                self.restore_focus(window, cx);
            }
        }
    }

    pub(super) fn on_connect_event(
        &mut self,
        dialog: &Entity<ConnectionDialog>,
        event: &ConnectionDialogEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            // The dialog has already saved the profile and closed itself;
            // opening the session is the shell's half of the workflow,
            // because the tab it produces belongs here.
            //
            // The welcome screen is re-read on the way out of the dialog by
            // either door: whichever one was taken, the file behind the
            // list may have been saved, renamed or deleted since it was
            // last read, and the list is what the user comes back to when
            // this tab is closed again.
            //
            // A tab that asked for a password to reconnect gets the session in
            // place; anything else opens a tab of its own.
            ConnectionDialogEvent::Connect { profile, typed } => {
                self.profiles = load_profiles();
                let typed = typed.borrow_mut().take();
                let waiting = self.pending_reconnect.take().and_then(|(tab, id)| {
                    (id == profile.id)
                        .then(|| self.connections.iter().position(|open| open.id == tab))
                        .flatten()
                });
                match waiting {
                    Some(index) => self.reconnect_with(index, typed, window, cx),
                    None => self.open_connection_with((**profile).clone(), typed, window, cx),
                }
                // The dialog has closed itself, and the keyboard was in it.
                if !dialog.read(cx).is_open() {
                    self.restore_focus(window, cx);
                }
            }
            ConnectionDialogEvent::Dismissed => {
                dialog.update(cx, |dialog, cx| dialog.close(cx));
                self.pending_reconnect = None;
                self.profiles = load_profiles();
                self.restore_focus(window, cx);
            }
        }
    }

    pub(super) fn on_explorer_event(
        &mut self,
        _explorer: &Entity<Explorer>,
        event: &ExplorerEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            // The explorer has no session of its own — see its module docs —
            // so every fetch comes back here, where the tabs live.
            ExplorerEvent::Load { node, generation } => {
                self.load_node(node.clone(), *generation, cx)
            }
            ExplorerEvent::Activated(target) => {
                self.open_object((**target).clone(), window, cx);
            }
            // The tree holds no strings and none of the five object
            // commands, so its menu is drawn here (architecture document,
            // §7.8).
            //
            // An error row is the one node with no menu at all: it names
            // nothing — it is the sentence saying why its parent could not
            // be read — so every command would be grayed, and a panel of
            // nothing but grayed rows says less than no panel.
            ExplorerEvent::ContextMenu { node, position } => {
                if !matches!(node, NodeId::Error(_)) {
                    self.open_context_menu(
                        ContextTarget::Explorer(Box::new(node.clone())),
                        *position,
                        cx,
                    );
                }
            }
        }
    }

    pub(super) fn on_settings_event(
        &mut self,
        dialog: &Entity<SettingsDialog>,
        event: &SettingsDialogEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            // The dialog has already replaced and persisted the settings
            // global by the time it emits this; the shell re-applies the
            // parts that touch the live window.
            SettingsDialogEvent::Applied => {
                self.apply_settings(window, cx);
                // The dialog closes itself after applying; without a refocus
                // the window focus dangles on its unrendered controls and
                // macOS disables every menu item validated through it.
                self.restore_focus(window, cx);
            }
            // The user is still in the dialog and nothing has been saved, so
            // only the palettes and the fonts follow — and the focus stays
            // where it is, since taking it back now would pull it out from
            // under whoever is typing.
            SettingsDialogEvent::Previewed => self.apply_preview(window, cx),
            // Closing dropped the preview, so re-applying now resolves back
            // to the settings on disk. That is the whole of the undo.
            SettingsDialogEvent::Dismissed => {
                dialog.update(cx, |dialog, cx| dialog.close(cx));
                self.apply_preview(window, cx);
                self.restore_focus(window, cx);
            }
        }
    }

    pub(super) fn on_extract_event(
        &mut self,
        dialog: &Entity<ExtractDialog>,
        event: &ExtractDialogEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            ExtractDialogEvent::Dismissed => {
                dialog.update(cx, |dialog, cx| dialog.close(cx));
                self.restore_focus(window, cx);
            }
        }
    }

    pub(super) fn on_transfer_event(
        &mut self,
        dialog: &Entity<TransferDialog>,
        event: &TransferDialogEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            TransferDialogEvent::Dismissed => {
                dialog.update(cx, |dialog, cx| dialog.close(cx));
                self.restore_focus(window, cx);
            }
        }
    }

    pub(super) fn on_backup_event(
        &mut self,
        dialog: &Entity<BackupDialog>,
        event: &BackupDialogEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            BackupDialogEvent::Dismissed => {
                dialog.update(cx, |dialog, cx| dialog.close(cx));
                self.restore_focus(window, cx);
            }
        }
    }

    pub(super) fn on_update_event(
        &mut self,
        dialog: &Entity<UpdateDialog>,
        event: &UpdateDialogEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            UpdateDialogEvent::Ignored { tag } => {
                // The dialog has already closed itself; writing the file is
                // this workspace's job, through the policy `app_identity`
                // installed, because the settings are rudbman's.
                update::remember_ignored(tag, cx);
                self.restore_focus(window, cx);
            }
            UpdateDialogEvent::Installed(_) => {
                // The new build is on disk and the dialog is still on
                // screen, deliberately: the restart is imminent and a
                // dialog that closed itself first would flash the window
                // back into view for a fraction of a second. Restarting is
                // the application's call and not the shell's — a staged
                // update merely spends its restart applying itself and
                // re-executing once more.
                //
                // The path comes from the shell because only it knows that
                // the install just renamed the running image aside:
                // `restart_path` is the `current_exe()` of the moment
                // `init_process_identity` ran, before any of that happened.
                // `None` — the platform never said, or no identity was
                // installed — is left unset, which is gpui's own default
                // and therefore exactly the same thing.
                if let Some(path) = rugpui_shell::restart_path() {
                    cx.set_restart_path(path);
                }
                cx.restart();
            }
            UpdateDialogEvent::Dismissed => {
                dialog.update(cx, |dialog, cx| dialog.close(cx));
                self.restore_focus(window, cx);
            }
        }
    }
}
