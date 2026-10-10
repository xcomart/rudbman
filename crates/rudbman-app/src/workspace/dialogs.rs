//! Dialogs.

use super::*;

impl Workspace {
    /// Closes every dialog and the dropdown menu.
    ///
    /// Every `open_*` method starts here, which is what keeps the modals
    /// mutually exclusive: only one of them can be on screen at a time, and
    /// opening one always puts the menu away.
    ///
    /// Closing the settings dialog drops its live preview, so the palettes are
    /// re-applied on the way out; without that the window would keep wearing a
    /// theme that nothing in the settings names any more.
    ///
    /// The update dialog is closed here like the rest, so a user who reaches
    /// for a command instead of one of its buttons is not left with a stale
    /// announcement floating over the window — except while it is installing,
    /// when its own `close` refuses and the swap is allowed to finish; see
    /// [`UpdateDialog::close`].
    pub(super) fn close_overlays(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.menu_open = false;
        self.close_context_menus(cx);
        if self.confirm.is_some() {
            // Declining is the only safe reading of "the dialog went away".
            self.answer_confirm(false, window, cx);
        }
        if self.about.read(cx).is_open() {
            self.about.update(cx, |dialog, cx| dialog.close(cx));
        }
        if self.connect.read(cx).is_open() {
            self.connect.update(cx, |dialog, cx| dialog.close(cx));
        }
        if self.settings.read(cx).is_open() {
            self.settings.update(cx, |dialog, cx| dialog.close(cx));
            self.apply_preview(window, cx);
        }
        if self.extract.read(cx).is_open() {
            // A job still running is cancelled by the close, through the drop
            // chain `extract_dialog` documents. That is the honest reading of
            // "the card is gone": a job whose progress nobody can see is one
            // nobody can stop either.
            self.extract.update(cx, |dialog, cx| dialog.close(cx));
        }
        if self.transfer.read(cx).is_open() {
            // Same reading, and the same drop chain — a transfer's cancel rolls
            // back the uncommitted tail and leaves what was committed, which is
            // §6's contract and not something closing a window can change.
            self.transfer.update(cx, |dialog, cx| dialog.close(cx));
        }
        if self.backup.read(cx).is_open() {
            self.backup.update(cx, |dialog, cx| dialog.close(cx));
        }
        if self.update.read(cx).is_open() {
            self.update.update(cx, |dialog, cx| dialog.close(cx));
        }
    }

    /// Shows or hides the application dropdown menu.
    ///
    /// Opening it puts every context menu away first. Both lay a full-window
    /// backdrop that dismisses on any press, so two of them on screen at once
    /// would be two sheets arguing over one click.
    pub(super) fn set_menu_open(&mut self, open: bool, cx: &mut Context<Self>) {
        if open {
            self.close_context_menus(cx);
        }
        if self.menu_open != open {
            self.menu_open = open;
            cx.notify();
        }
    }

    /// Opens the shell's own context menu over `target`.
    ///
    /// The application dropdown goes away for the reason
    /// [`Workspace::set_menu_open`] gives, and the one slot means the menu that
    /// was open — whichever surface it belonged to — goes with it.
    pub(super) fn open_context_menu(
        &mut self,
        target: ContextTarget,
        position: Point<Pixels>,
        cx: &mut Context<Self>,
    ) {
        self.menu_open = false;
        self.close_pane_context_menus(cx);
        self.context_menu = Some(OpenContextMenu { target, position });
        cx.notify();
    }

    /// Puts away every context menu anywhere in the window, and says whether
    /// there was one.
    ///
    /// Both halves: the shell's own, and the ones the panes draw for their
    /// editor, grid and canvases. What `Escape` runs first of all, and what the
    /// application dropdown runs before it opens.
    pub(super) fn close_context_menus(&mut self, cx: &mut Context<Self>) -> bool {
        let mine = self.context_menu.take().is_some();
        if mine {
            cx.notify();
        }
        // Both halves, always: `|` rather than `||`, because a pane menu left
        // open behind a dismissed shell menu is exactly the state this exists
        // to prevent.
        self.close_pane_context_menus(cx) | mine
    }

    /// Puts away the context menu of every pane of the work area on screen.
    ///
    /// Every leaf and every tab of each, rather than the active pane alone:
    /// only the active tab is rendered, so only it can have a menu open — but
    /// a right-click does not move the pane marker, so the pane holding one is
    /// not necessarily the active one, and asking them all costs a walk of a
    /// tree with a handful of leaves in it.
    pub(super) fn close_pane_context_menus(&mut self, cx: &mut Context<Self>) -> bool {
        let Some(area) = self.work_area() else {
            return false;
        };
        // Gathered before anything is updated: reading the tree borrows the
        // application immutably and closing a menu borrows it mutably.
        let mut queries = Vec::new();
        let mut diagrams = Vec::new();
        let mut builders = Vec::new();
        let mut data = Vec::new();
        for (_, pane) in area.panes.leaves() {
            for item in pane.items() {
                match item {
                    PaneItem::Query { pane, .. } => queries.push(pane.clone()),
                    PaneItem::Erd(panel) => diagrams.push(panel.clone()),
                    PaneItem::QueryBuilder { pane, .. } => builders.push(pane.clone()),
                    PaneItem::TableData(panel) => data.push(panel.clone()),
                    // The two surfaces with nothing to act on: the detail panel
                    // is four tabs of read-only presentation, and the structure
                    // pane's commands are all buttons of its own.
                    PaneItem::TableDetail(_) | PaneItem::TableStruct(_) => {}
                }
            }
        }

        let mut closed = false;
        for pane in queries {
            closed |= pane.update(cx, |pane, cx| pane.close_context_menu(cx));
        }
        for panel in diagrams {
            closed |= panel.update(cx, |panel, cx| panel.close_context_menu(cx));
        }
        for panel in builders {
            closed |= panel.update(cx, |panel, cx| panel.close_context_menu(cx));
        }
        for panel in data {
            closed |= panel.update(cx, |panel, cx| panel.close_context_menu(cx));
        }
        closed
    }

    /// Opens the about dialog, closing whatever else was showing.
    pub(super) fn open_about(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.close_overlays(window, cx);
        self.about.update(cx, |dialog, cx| dialog.open(cx));
        cx.notify();
    }

    /// Asks GitHub for the latest release and shows the answer.
    ///
    /// Goes through `close_overlays` where the start-up check pointedly does
    /// not: this dialog was asked for, so it is entitled to the screen the way
    /// every other menu command is.
    ///
    /// Refuses while an install is already running, which is the one case where
    /// the update dialog cannot be closed and so must not be reopened into a
    /// different state.
    pub(super) fn check_updates(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.update.read(cx).is_busy() {
            return;
        }
        self.close_overlays(window, cx);
        self.update.update(cx, |dialog, cx| dialog.start_check(cx));
        cx.notify();
    }

    /// Opens the settings dialog, closing whatever else was showing.
    pub(super) fn open_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.close_overlays(window, cx);
        self.settings.update(cx, |dialog, cx| dialog.open(cx));
        cx.notify();
    }

    /// Opens the extraction dialog over `target`, closing whatever else was
    /// showing.
    ///
    /// Nothing happens without a live session behind the object: the dialog's
    /// only button starts a job on one, so a card that could not have run
    /// anything is worse than no card. The session handle is passed in rather
    /// than looked up later — the tab may be closed while the dialog is up, and
    /// holding a [`connection::SessionHandle`] is what keeps the session and its
    /// tunnel standing until the job is done with them.
    pub(super) fn open_extract(
        &mut self,
        target: ObjectTarget,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(session) = self.session_of(target.connection) else {
            return;
        };
        self.close_overlays(window, cx);
        self.extract
            .update(cx, |dialog, cx| dialog.open(target, session, cx));
        cx.notify();
    }

    /// Opens the transfer dialog over `target`, closing whatever else was
    /// showing.
    ///
    /// Nothing happens without a live session behind the object: the source
    /// query runs on it. The dialog is handed every open connection as a
    /// candidate target, the source's own included — a transfer into another
    /// schema of the same database is a real one, and the bridge's lock is
    /// reentrant — and it holds a [`connection::SessionHandle`] for whichever
    /// it is pointed at, so that closing that tab mid-transfer leaves the
    /// session standing until the job is done with it.
    pub(super) fn open_transfer(
        &mut self,
        target: ObjectTarget,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(session) = self.session_of(target.connection) else {
            return;
        };
        let candidates = self.transfer_targets();
        self.close_overlays(window, cx);
        self.transfer.update(cx, |dialog, cx| {
            dialog.open(target, session, candidates, cx)
        });
        cx.notify();
    }

    /// Every connection a transfer could write into: the open ones, in tab
    /// order.
    pub(super) fn transfer_targets(&self) -> Vec<TransferTarget> {
        self.connections
            .iter()
            .filter_map(|open| {
                let ConnectionState::Open(connected) = &open.state else {
                    return None;
                };
                Some(TransferTarget {
                    connection: open.id,
                    name: if open.profile.name.trim().is_empty() {
                        ts!("connect.unnamed")
                    } else {
                        SharedString::from(open.profile.name.clone())
                    },
                    session: connected.handle(),
                })
            })
            .collect()
    }

    /// Opens the backup dialog over `scope`, closing whatever else was showing.
    ///
    /// Gated on the session the same way, and holding its handle for the same
    /// reason: the job writes a file for as long as it takes, and the tab it
    /// was started from may be closed in the meantime.
    pub(super) fn open_backup(
        &mut self,
        connection: ConnectionId,
        scope: explorer::Scope,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(session) = self.session_of(connection) else {
            return;
        };
        self.close_overlays(window, cx);
        self.backup
            .update(cx, |dialog, cx| dialog.open(scope, session, cx));
        cx.notify();
    }

    /// Reads `path` into a query pane on the connection whose tab is showing.
    ///
    /// The read is a background task because a script can be a hundred
    /// megabytes and the rope behind the editor is built for exactly that; the
    /// editor, the statement splitter and "run everything" then handle it like
    /// anything else that was typed.
    ///
    /// Invalid UTF-8 is replaced rather than refused. A `.sql` file in a legacy
    /// encoding is still mostly readable as UTF-8 — the ASCII half of it always
    /// is — and a user who can see their script can fix the part that came out
    /// wrong, which is more than an error message offers.
    pub(super) fn load_sql_file(
        &mut self,
        path: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.has_live_connection() {
            return;
        }
        let reading =
            cx.background_spawn(async move { std::fs::read(&path).map(|bytes| (path, bytes)) });

        cx.spawn_in(window, async move |workspace, cx| {
            match reading.await {
                Ok((path, bytes)) => {
                    let text = String::from_utf8_lossy(&bytes).into_owned();
                    workspace
                        .update_in(cx, |workspace, window, cx| {
                            workspace.open_query(&text, window, cx);
                            log::debug!("opened {}", path.display());
                        })
                        .ok();
                }
                // Nowhere to put this on screen: the shell has no transient
                // message strip, and inventing one for the case where a file
                // the user just picked has gone away is out of proportion.
                Err(error) => log::error!("could not read the SQL file: {error}"),
            }
        })
        .detach();
    }

    /// Whether the tab on screen has a session behind it.
    pub(super) fn has_live_connection(&self) -> bool {
        self.active_connection()
            .is_some_and(|open| matches!(open.state, ConnectionState::Open(_)))
    }

    /// Asks the platform for a `.sql` file and opens what it hands back.
    ///
    /// Nothing waits on the prompt, for the reason the other pickers in this
    /// application do not: on X11 that call is the one gpui had to be patched
    /// around.
    pub(super) fn open_sql_file(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // Asked before the prompt as well as after: opening a file picker over
        // a window that has nowhere to put the file is a dead end the user only
        // finds out about once they have chosen one.
        if !self.has_live_connection() {
            return;
        }
        let paths = cx.prompt_for_paths(gpui::PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some(ts!("query.open_file_select")),
        });

        cx.spawn_in(window, async move |workspace, cx| {
            let chosen = match paths.await {
                Ok(Ok(Some(paths))) => paths,
                Ok(Ok(None)) | Err(_) => return,
                Ok(Err(error)) => {
                    log::warn!("the file picker could not be opened: {error:#}");
                    return;
                }
            };
            let Some(path) = chosen.into_iter().next() else {
                return;
            };
            workspace
                .update_in(cx, |workspace, window, cx| {
                    workspace.load_sql_file(path, window, cx);
                })
                .ok();
        })
        .detach();
    }

    /// Re-applies the saved settings to the window.
    ///
    /// The counterpart of the start-up sequence in [`main`], and the only place
    /// a setting reaches a *live* window: the language the next frame is built
    /// in, the menu bar the platform owns, both palettes, the title bar style
    /// and the surface's background treatment.
    ///
    /// Deliberately does not move the focus — where the focus belongs after this
    /// depends on whether the dialog closed, which only the caller knows.
    ///
    /// Every platform call in here acts on the window, and one of them —
    /// `request_decorations` on X11 — is the call that used to re-enter gpui's
    /// window callbacks and panic. It is safe from this stack: the settings
    /// dialog emits its event, gpui delivers it after the button's own callback
    /// has returned and released every borrow, and this runs from there. It must
    /// stay that way; calling it from inside a widget callback would put the
    /// borrow back.
    pub(super) fn apply_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let settings = app_settings::current(cx);
        // The sidebar is not in the settings dialog, but the file it writes
        // carries the width and the visibility all the same, so the live window
        // follows what was actually saved.
        self.explorer_visible = settings.explorer_visible;
        self.explorer_width = settings.explorer_width;
        // The editor fonts reach the editors through the frame that draws them,
        // but wrapping is state each `EditorView` holds, so it has to be handed
        // over pane by pane — across every connection, not just the one on
        // screen, or a tab behind another would keep the old answer.
        let queries: Vec<_> = self
            .connections
            .iter()
            .flat_map(|open| open.work.queries())
            .collect();
        for query in queries {
            query.update(cx, |query, cx| {
                query.set_word_wrap(settings.editor_word_wrap, cx);
            });
        }
        // Before the repaint below, so the next frame is already drawn in the
        // newly chosen language.
        i18n::apply(settings.language.as_deref());
        // The native macOS menu bar is built once and owned by the platform, so
        // unlike the in-app menu it does not follow a repaint; it has to be
        // handed over again.
        cx.set_menus(app_menus());
        apply_themes(&settings, cx);
        // Ahead of the repaint, so the toolbar's next frame already knows
        // whether it has to stand in for a title bar; and ahead of the two calls
        // below, which leave the accent policy and the caption colors on the
        // window, so a caption that comes back here comes back already themed.
        //
        // The field follows the call rather than the stored setting: everything
        // that branches on it is asking what the window carries, not what was
        // last saved.
        if settings.window.titlebar != self.titlebar {
            self.titlebar = settings.window.titlebar;
            let custom = self.titlebar == TitlebarStyle::Custom;
            window.set_titlebar_transparent(custom, custom.then_some(TRAFFIC_LIGHT_ORIGIN));
            // The Linux counterpart of the call above, which only the Windows
            // and macOS backends implement: swap the compositor's frame for
            // client-side decorations (or back) on the live window.
            #[cfg(not(any(target_os = "windows", target_os = "macos")))]
            window.request_decorations(if custom {
                gpui::WindowDecorations::Client
            } else {
                gpui::WindowDecorations::Server
            });
        }
        // Paired with the call below, and never with a preview: the leaf crates
        // read this to decide whether to paint their own background, and the
        // answer is only right once the surface itself permits alpha. Ahead of
        // the repaint, so the next frame already draws under the new answer.
        set_window_tint(settings.window.background_opacity, cx);
        cx.refresh_windows();
        window.set_background_appearance(window_appearance(
            settings.window.background_blur,
            settings.window.background_opacity,
        ));
        // After the background appearance, never before: on Windows that call
        // re-arms the accent policy that would otherwise repaint the caption out
        // from under us.
        apply_caption_theme(window, &theme(cx), cx);
    }

    /// Re-applies the palettes the settings dialog is currently showing.
    ///
    /// The unsaved half of [`Workspace::apply_settings`], and deliberately much
    /// smaller: only the two palettes and the fonts are previewed, so this
    /// touches no platform state beyond the native caption's colours, which have
    /// to follow the chrome theme or the window would be half repainted.
    ///
    /// Reads [`app_settings::effective`], which answers the preview while one is
    /// installed and the saved settings once it is dropped — so the same call
    /// both applies a preview and undoes it.
    pub(super) fn apply_preview(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        apply_themes(&app_settings::effective(cx), cx);
        cx.refresh_windows();
        apply_caption_theme(window, &theme(cx), cx);
    }

    /// Opens the connection dialog, closing whatever else was showing.
    pub(super) fn open_connect(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.close_overlays(window, cx);
        self.connect.update(cx, |dialog, cx| dialog.open(cx));
        cx.notify();
    }

    /// Opens the connection dialog on one saved profile.
    ///
    /// What the welcome list's "edit…" row does, as against its "connect" row:
    /// the same dialog the button beside the list opens, showing the profile
    /// that was right-clicked rather than the first one saved.
    pub(super) fn edit_profile(&mut self, id: Uuid, window: &mut Window, cx: &mut Context<Self>) {
        self.close_overlays(window, cx);
        self.connect.update(cx, |dialog, cx| dialog.open_at(id, cx));
        cx.notify();
    }

    /// Answers the write confirmation, one way or the other.
    pub(super) fn answer_confirm(
        &mut self,
        run: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(pending) = self.confirm.take() else {
            return;
        };
        pending.pane.update(cx, |pane, cx| {
            if run {
                pane.confirmed(window, cx);
            } else {
                pane.declined(cx);
            }
        });
        if run {
            pending
                .pane
                .update(cx, |pane, cx| pane.focus_editor(window, cx));
        }
        cx.notify();
    }
}
