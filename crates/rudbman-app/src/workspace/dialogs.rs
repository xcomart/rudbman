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
    ///
    /// The three job dialogs are left standing while their job runs, for the
    /// update dialog's reason and a stronger one: closing one cancels its job,
    /// and minutes of transfer are not something a command reached for
    /// elsewhere may throw away. So this answers whether the screen is clear,
    /// and every `open_*` that would put a dialog up opens nothing when it is
    /// not — two modals at once would be two sheets fighting over one press.
    #[must_use]
    pub(super) fn close_overlays(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        // Every dialog opens through here, so this is where the keyboard is
        // remembered — once, by the first of a run of dialogs, so that one
        // opened over another still gives the keyboard back to where it was
        // before either.
        if !self.dialog_open(cx) {
            self.focus_origin = self.focus_origin_now(window, cx);
        }
        if self.rename_tab.is_some() {
            self.finish_tab_name(false, window, cx);
        }
        self.menu_open = false;
        self.close_context_menus(cx);
        if self.confirm.is_some() {
            // Declining is the only safe reading of "the dialog went away".
            self.answer_confirm(false, window, cx);
        }
        // And for a close, the only safe reading is that nothing closes.
        if self.close_question.is_some() {
            self.answer_close(closing::CloseAnswer::Cancel, window, cx);
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
        // A job dialog that is merely a form or a finished card goes like any
        // other. One with a job under it stays: the close would cancel the job
        // through the drop chain `extract_dialog` documents.
        if self.extract.read(cx).is_open() && !self.extract.read(cx).is_running() {
            self.extract.update(cx, |dialog, cx| dialog.close(cx));
        }
        if self.transfer.read(cx).is_open() && !self.transfer.read(cx).is_running() {
            self.transfer.update(cx, |dialog, cx| dialog.close(cx));
        }
        if self.backup.read(cx).is_open() && !self.backup.read(cx).is_running() {
            self.backup.update(cx, |dialog, cx| dialog.close(cx));
        }
        if self.update.read(cx).is_open() {
            self.update.update(cx, |dialog, cx| dialog.close(cx));
        }
        !self.job_running(cx) && !self.update.read(cx).is_open()
    }

    /// Whether one of the three job dialogs has a job running under it.
    pub(super) fn job_running(&self, cx: &App) -> bool {
        self.extract.read(cx).is_running()
            || self.transfer.read(cx).is_running()
            || self.backup.read(cx).is_running()
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
        // `F10` sets it again after this; every other way in is the pointer's
        // or the trigger's own, which highlights by itself.
        self.menu_by_keyboard = false;
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
        self.context_menu = Some(OpenContextMenu {
            target,
            position,
            by_keyboard: false,
        });
        cx.notify();
    }

    /// Marks the shell's open context menu as raised from the keyboard, so it
    /// opens on its first row; see [`Workspace::open_context_menu`].
    pub(super) fn context_by_keyboard(&mut self, by_keyboard: bool) {
        if let Some(menu) = &mut self.context_menu {
            menu.by_keyboard = by_keyboard;
        }
    }

    /// Puts away every context menu anywhere in the window.
    ///
    /// Both halves: the shell's own, and the ones the panes draw for their
    /// editor, grid and canvases. What a dialog and the application dropdown
    /// run before they open.
    pub(super) fn close_context_menus(&mut self, cx: &mut Context<Self>) {
        if self.context_menu.take().is_some() {
            cx.notify();
        }
        self.close_pane_context_menus(cx);
    }

    /// Puts away the context menu of every pane of the work area on screen.
    ///
    /// Every leaf and every tab of each, rather than the active pane alone:
    /// only the active tab is rendered, so only it can have a menu open — but
    /// a right-click does not move the pane marker, so the pane holding one is
    /// not necessarily the active one, and asking them all costs a walk of a
    /// tree with a handful of leaves in it.
    pub(super) fn close_pane_context_menus(&mut self, cx: &mut Context<Self>) {
        let Some(area) = self.work_area() else {
            return;
        };
        // Gathered before anything is updated: reading the tree borrows the
        // application immutably and closing a menu borrows it mutably.
        let mut queries = Vec::new();
        let mut diagrams = Vec::new();
        let mut builders = Vec::new();
        let mut data = Vec::new();
        let mut details = Vec::new();
        for (_, pane) in area.panes.leaves() {
            for item in pane.items() {
                match item {
                    PaneItem::Query { pane, .. } => queries.push(pane.clone()),
                    PaneItem::Erd(panel) => diagrams.push(panel.clone()),
                    PaneItem::QueryBuilder { pane, .. } => builders.push(pane.clone()),
                    PaneItem::TableData(panel) => data.push(panel.clone()),
                    // The detail panel's only menu is its DDL editor's.
                    PaneItem::TableDetail(panel) => details.push(panel.clone()),
                    // The structure pane's commands are all buttons of its own.
                    PaneItem::TableStruct(_) => {}
                }
            }
        }

        for pane in queries {
            pane.update(cx, |pane, cx| pane.close_context_menu(cx));
        }
        for panel in diagrams {
            panel.update(cx, |panel, cx| panel.close_context_menu(cx));
        }
        for panel in builders {
            panel.update(cx, |panel, cx| panel.close_context_menu(cx));
        }
        for panel in data {
            panel.update(cx, |panel, cx| panel.close_context_menu(cx));
        }
        for panel in details {
            panel.update(cx, |panel, cx| panel.close_context_menu(cx));
        }
    }

    /// Opens the about dialog, closing whatever else was showing.
    pub(super) fn open_about(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.close_overlays(window, cx) {
            return;
        }
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
        if !self.close_overlays(window, cx) {
            return;
        }
        self.update.update(cx, |dialog, cx| dialog.start_check(cx));
        cx.notify();
    }

    /// Opens the settings dialog, closing whatever else was showing.
    pub(super) fn open_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.close_overlays(window, cx) {
            return;
        }
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
        if !self.close_overlays(window, cx) {
            return;
        }
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
        if !self.close_overlays(window, cx) {
            return;
        }
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
        if !self.close_overlays(window, cx) {
            return;
        }
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
    ///
    /// A file that read as UTF-8 binds the pane to it, which names the tab
    /// after the file and makes "Save" write back to it. One that did not stays
    /// unbound, so that a save asks where to write instead of putting the
    /// replacement characters over bytes the editor could not show.
    pub(super) fn load_sql_file(
        &mut self,
        path: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.has_live_connection() {
            return;
        }
        let file = path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.display().to_string());
        let reading = cx.background_spawn(async move {
            read_sql_file(&path).map(|(bytes, stamp)| (path, bytes, stamp))
        });

        cx.spawn_in(window, async move |workspace, cx| {
            match reading.await {
                Ok((path, bytes, stamp)) => {
                    let (text, exact) = decode_sql_file(bytes);
                    workspace
                        .update_in(cx, |workspace, window, cx| {
                            log::debug!("opened {}", path.display());
                            let Some(pane) = workspace.open_query(&text, window, cx) else {
                                return;
                            };
                            if exact {
                                pane.update(cx, |pane, cx| pane.bind_file(path, stamp, cx));
                            }
                        })
                        .ok();
                }
                // A toast: the file the user just picked has gone away or
                // will not be read, and no tab opened to say so in.
                Err(error) => {
                    log::error!("could not read the SQL file: {error}");
                    cx.update(|_window, cx| {
                        rugpui_shell::show_toast(
                            rugpui_shell::Toast::error(ts!(
                                "toast.open_failed",
                                file = file.clone(),
                                error = format!("{error:#}")
                            )),
                            cx,
                        );
                    })
                    .ok();
                }
            }
        })
        .detach();
    }

    /// Writes the SQL of the query tab on screen to its file.
    ///
    /// Straight to the bound file, unless there is none or `choose` asks for a
    /// new one; then the platform's save dialog asks first. Writing in place
    /// holds only while the file still looks the way the pane last saw it (see
    /// [`crate::query`]'s `file` module); otherwise the pane says so and the
    /// dialog asks after all.
    ///
    /// Not gated on the session: what is written is the editor's text, which a
    /// pane whose connection has gone still holds.
    pub(super) fn save_sql_file(
        &mut self,
        choose: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(area) = self.work_area() else {
            return;
        };
        let Some(item) = area.panes.get(area.active()).and_then(|pane| pane.active()) else {
            return;
        };
        let PaneItem::Query { pane, .. } = item else {
            return;
        };
        let pane = pane.clone();
        let title = area
            .tab_names
            .get(&item.entity_id())
            .cloned()
            .unwrap_or_else(|| item.title(cx).to_string());
        let (bound, stamp) = {
            let pane = pane.read(cx);
            (pane.file().map(Path::to_path_buf), pane.file_stamp())
        };
        match bound {
            Some(path) if !choose => {
                self.save_pane_to(pane, path, Overwrite::IfUnchanged(stamp), window, cx)
            }
            _ => self.ask_where_to_save(pane, &title, window, cx),
        }
    }

    /// Opens the platform's save dialog for `pane`, and writes where it says.
    ///
    /// It opens where the bound file is, suggesting its name; a pane bound to
    /// none starts from the home directory, with the tab's `title` made into a
    /// file name. Nothing waits on the prompt, for the reason
    /// [`Workspace::open_sql_file`] gives.
    pub(super) fn ask_where_to_save(
        &mut self,
        pane: Entity<QueryPane>,
        title: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let bound = pane.read(cx).file().map(Path::to_path_buf);
        let directory = bound
            .as_deref()
            .and_then(Path::parent)
            .map(Path::to_path_buf)
            .unwrap_or_else(crate::extract_dialog::default_directory);
        let suggested = match bound.as_deref().and_then(Path::file_name) {
            Some(name) => name.to_string_lossy().into_owned(),
            None => suggested_file_name(title),
        };
        let prompt = cx.prompt_for_new_path(&directory, Some(&suggested));

        cx.spawn_in(window, async move |workspace, cx| {
            let path = match prompt.await {
                Ok(Ok(Some(path))) => path,
                outcome => {
                    if let Ok(Err(error)) = outcome {
                        log::warn!("the save dialog could not be opened: {error:#}");
                    }
                    // A close that asked for this save closes nothing now.
                    workspace
                        .update(cx, |workspace, cx| {
                            workspace.close_save_abandoned(&pane, cx)
                        })
                        .ok();
                    return;
                }
            };
            workspace
                .update_in(cx, |workspace, window, cx| {
                    workspace.save_pane_to(pane, path, Overwrite::Chosen, window, cx)
                })
                .ok();
        })
        .detach();
    }

    /// Writes `pane`'s text to `path` on a background task, as `overwrite`
    /// allows, and tells the pane how it went.
    ///
    /// A pane that ends up bound to a file it was not bound to before gives up
    /// any name the user had given its tab, so that the tab reads as the file
    /// it now is; saving again to the same file leaves a name alone. A bound
    /// file that has changed is left as it is, and the save dialog asks.
    pub(super) fn save_pane_to(
        &mut self,
        pane: Entity<QueryPane>,
        path: PathBuf,
        overwrite: Overwrite,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(SaveJob { text, edits }) = pane.update(cx, |pane, cx| pane.begin_save(cx)) else {
            return;
        };
        let writing = cx.background_spawn(async move {
            let outcome = write_sql_file(&path, &text, overwrite);
            (path, outcome)
        });

        cx.spawn_in(window, async move |workspace, cx| {
            let (path, outcome) = writing.await;
            workspace
                .update_in(cx, |workspace, window, cx| {
                    let saved =
                        pane.update(cx, |pane, cx| pane.finish_save(path, edits, outcome, cx));
                    match saved {
                        Saved::Rebound => {
                            let tab = pane.entity_id();
                            for open in &mut workspace.connections {
                                open.work.tab_names.remove(&tab);
                            }
                            pane.update(cx, |pane, _| pane.set_tab_name(None));
                        }
                        // The pane is bound, so the dialog suggests its own file
                        // and the title goes unused.
                        Saved::Changed => workspace.ask_where_to_save(pane.clone(), "", window, cx),
                        Saved::InPlace | Saved::Failed => {}
                    }
                    // A close that asked for this save carries on, or stops.
                    workspace.after_close_save(&pane, saved, window, cx);
                    cx.notify();
                })
                .ok();
        })
        .detach();
    }

    /// Whether the tab on screen has a session behind it.
    pub(super) fn has_live_connection(&self) -> bool {
        self.active_connection()
            .is_some_and(|open| matches!(open.state, ConnectionState::Open(_)))
    }

    /// [`Workspace::has_live_connection`] for a command the user just gave —
    /// `Ctrl+T`, `Ctrl+O`, `Ctrl+Shift+T`, a new builder — saying in a toast
    /// why nothing opens when there is no session: no connection at all, one
    /// still connecting, or one that failed or ended. Those chords used to end
    /// without a word, which reads as a key that does nothing.
    pub(super) fn say_not_live(&self, cx: &mut App) -> bool {
        let message = match self.active_connection() {
            None => ts!("toast.no_connection"),
            Some(open) => {
                let name = closing::connection_title(&open.profile).to_string();
                match open.state {
                    ConnectionState::Open(_) => return true,
                    ConnectionState::Connecting { .. } => ts!("toast.connecting", name = name),
                    ConnectionState::Failed(_) | ConnectionState::Dead(_) => {
                        ts!("toast.not_connected", name = name)
                    }
                }
            }
        };
        rugpui_shell::show_toast(rugpui_shell::Toast::info(message), cx);
        false
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
        if !self.say_not_live(cx) {
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
    /// smaller: only the two palettes and the fonts — the interface size's
    /// scale among them — are previewed, so this
    /// touches no platform state beyond the native caption's colors, which have
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
        if !self.close_overlays(window, cx) {
            return;
        }
        self.connect.update(cx, |dialog, cx| dialog.open(cx));
        cx.notify();
    }

    /// Opens the connection dialog on one saved profile.
    ///
    /// What the welcome list's "edit…" row does, as against its "connect" row:
    /// the same dialog the button beside the list opens, showing the profile
    /// that was right-clicked rather than the first one saved.
    pub(super) fn edit_profile(&mut self, id: Uuid, window: &mut Window, cx: &mut Context<Self>) {
        if !self.close_overlays(window, cx) {
            return;
        }
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
        // Back to the editor either way: the question interrupted someone
        // running SQL, and that is where they go on.
        pending
            .pane
            .update(cx, |pane, cx| pane.focus_editor(window, cx));
        cx.notify();
    }
}

/// The file name a save dialog suggests for a tab titled `title`.
///
/// The title as it reads, with `.sql` after it unless it already ends that
/// way, and with every character a file name cannot hold on one of the three
/// platforms swapped for `_` — a tab the user named "매출/분석" is still a
/// name worth starting from, but the slash would turn it into a directory.
/// Trailing dots and spaces go too, which Windows would strip on its own.
fn suggested_file_name(title: &str) -> String {
    let cleaned: String = title
        .chars()
        .map(|c| match c {
            '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*' => '_',
            c if c.is_control() => '_',
            c => c,
        })
        .collect();
    let stem = cleaned.trim().trim_end_matches(['.', ' ']);
    let stem = if stem.is_empty() { "query" } else { stem };
    if stem.to_lowercase().ends_with(".sql") {
        stem.to_string()
    } else {
        format!("{stem}.sql")
    }
}

#[cfg(test)]
mod tests {
    use super::suggested_file_name;

    #[test]
    fn a_tab_title_becomes_a_file_name_a_save_dialog_can_take() {
        assert_eq!(suggested_file_name("쿼리 3"), "쿼리 3.sql");
        assert_eq!(suggested_file_name("매출/분석"), "매출_분석.sql");
        assert_eq!(suggested_file_name("a:b*c?"), "a_b_c_.sql");
        assert_eq!(suggested_file_name("report.sql"), "report.sql");
        assert_eq!(suggested_file_name("REPORT.SQL"), "REPORT.SQL");
        assert_eq!(suggested_file_name("draft. "), "draft.sql");
        assert_eq!(suggested_file_name("  "), "query.sql");
        assert_eq!(suggested_file_name("tab\tname"), "tab_name.sql");
    }
}
