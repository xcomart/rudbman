//! What a close asks before it throws work away, and the tabs it remembers.
//!
//! Four gestures close things in this window — a tab, a pane, a connection
//! tab, the application — and every one of them goes through
//! [`Workspace::request_close`], so that what protects a tab protects the pane
//! and the connection holding it too. Three kinds of work are at stake, and
//! each is met the way its owner can best answer it:
//!
//! * **Staged edits** — rows or columns changed and not applied — are
//!   *refused* rather than asked about, exactly as closing one tab always was
//!   ([`Workspace::refuse_close`]): the tab comes forward and says what has to
//!   happen first, because the two answers the user needs, apply and discard,
//!   are already in the pane and neither of them is a dialog.
//! * **A query tab bound to a `.sql` file with changes the file does not
//!   have** is asked about — Save, Don't save, Cancel — one tab at a time,
//!   brought forward first so the question is about a tab the user can see.
//!   A tab bound to no file is not asked about: its SQL is the user's draft,
//!   and closing the tab is how one gets rid of a draft. Either kind can be
//!   brought back by "Reopen closed tab", which remembers the last few.
//! * **What only the connection holds** — an uncommitted transaction under
//!   manual commit, which the bridge rolls back when the session closes, and a
//!   statement still running — is asked about when the connection tab closes.
//!
//! The application's quit, the window's close (the caption's button, the
//! platform's own) and the restart an update ends in ask once, listing
//! everything at stake across every connection, because there is no tab left
//! to bring forward. That question is `rugpui_shell::CloseGuard`'s, fed by
//! [`Workspace::unsaved_work`] and able to save every file-bound tab first
//! ([`Workspace::save_all_files`]).
//!
//! # One question at a time, and the close is asked again after each
//!
//! A close that has to ask stops, keeps what it was asked to do and what has
//! been answered so far in a [`CloseQuestion`], and is run again from the top
//! once the answer is in. Running it again rather than resuming it is what
//! keeps the answer honest: a tab saved in the meantime is clean and is not
//! asked about, a tab that has gone is not closed twice, and a pane that
//! refused because of staged edits refuses again.

use super::*;
use std::collections::HashSet;

/// How many closed tabs each connection remembers for "Reopen closed tab".
///
/// A handful, as browsers keep: the command is for the tab closed by mistake a
/// moment ago, not an archive of every draft ever thrown away.
pub(super) const CLOSED_TABS_KEPT: usize = 10;

/// Something the user has asked to close.
#[derive(Clone, Debug)]
pub(super) enum CloseRequest {
    /// Some tabs of one pane of the work area on screen, by identity — an
    /// index would point at a different tab once the first of them is gone.
    Tabs {
        /// The pane they are in.
        pane: PaneId,
        /// The tabs.
        tabs: Vec<gpui::EntityId>,
    },
    /// One pane of the work area on screen, with every tab in it.
    Pane(PaneId),
    /// A connection tab, with its whole work area and its session.
    Connection(ConnectionId),
}

/// What has been answered so far for one [`CloseRequest`].
#[derive(Clone, Debug, Default)]
pub(super) struct CloseAnswers {
    /// File-bound query tabs the user said "Don't save" to.
    discard: HashSet<gpui::EntityId>,
    /// Whether the connection's own losses — a transaction, a running
    /// statement — were accepted.
    accepted: bool,
}

/// A close waiting on the user.
pub(super) struct CloseQuestion {
    /// What was asked for.
    pub(super) request: CloseRequest,
    /// What has been answered so far.
    pub(super) answers: CloseAnswers,
    /// What is being asked now.
    pub(super) ask: CloseAsk,
}

/// What a [`CloseQuestion`] is asking.
pub(super) enum CloseAsk {
    /// A query tab bound to a file holds changes the file does not.
    Unsaved {
        /// The tab.
        pane: Entity<QueryPane>,
        /// Its title, which is the file's name unless the user renamed it.
        name: SharedString,
        /// Whether "Save" has been pressed and the write is out.
        saving: bool,
    },
    /// Closing the connection would roll back or stop something.
    Connection {
        /// The connection's name.
        name: SharedString,
        /// One sentence per thing that would be lost.
        reasons: Vec<SharedString>,
    },
}

/// The answers a [`CloseQuestion`] offers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum CloseAnswer {
    /// Close nothing.
    Cancel,
    /// Close without saving, or accept what the connection loses.
    Discard,
    /// Save the file first, then close.
    Save,
}

impl Workspace {
    /// Closes what `request` names, once nothing in it holds work the close
    /// would throw away unasked.
    ///
    /// See the module documentation for what is refused, what is asked and in
    /// which order. `answers` is what the user has said so far for this same
    /// request; every gesture starts from [`CloseAnswers::default`].
    pub(super) fn request_close(
        &mut self,
        request: CloseRequest,
        answers: CloseAnswers,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match &request {
            CloseRequest::Tabs { pane, tabs } => {
                let pane = *pane;
                let indices = self.indices_of(pane, tabs);
                if indices.is_empty() || self.refuse_close(pane, &indices, window, cx) {
                    return;
                }
                if self.ask_unsaved(&request, &answers, pane, &indices, window, cx) {
                    return;
                }
                self.close_tabs_now(pane, &indices, window, cx);
            }
            CloseRequest::Pane(pane) => {
                let pane = *pane;
                let Some(count) = self.lone_pane_aside(pane) else {
                    return;
                };
                let indices: Vec<usize> = (0..count).collect();
                if self.refuse_close(pane, &indices, window, cx) {
                    return;
                }
                if self.ask_unsaved(&request, &answers, pane, &indices, window, cx) {
                    return;
                }
                self.close_pane_now(pane, window, cx);
            }
            CloseRequest::Connection(id) => {
                let Some(index) = self.connections.iter().position(|open| open.id == *id) else {
                    return;
                };
                if let Some(pane) = self.pane_with_staged_edits(index, cx) {
                    // Brought to the front first: the refusal lands on the tab
                    // holding the edits, and that tab has to be on screen to
                    // say so.
                    self.select_connection(index, window, cx);
                    let count = self
                        .work_area()
                        .and_then(|area| area.panes.get(pane))
                        .map_or(0, |pane| pane.items().len());
                    let indices: Vec<usize> = (0..count).collect();
                    self.refuse_close(pane, &indices, window, cx);
                    return;
                }
                if !answers.accepted {
                    let reasons = self.connection_risks(index, cx);
                    if !reasons.is_empty() {
                        let name = self.connection_name(index);
                        self.raise_close_question(
                            request,
                            answers,
                            CloseAsk::Connection { name, reasons },
                            cx,
                        );
                        return;
                    }
                }
                let leaves: Vec<(PaneId, usize)> = self.connections[index]
                    .work
                    .panes
                    .leaves()
                    .into_iter()
                    .map(|(id, pane)| (id, pane.items().len()))
                    .collect();
                for (pane, count) in leaves {
                    let indices: Vec<usize> = (0..count).collect();
                    if self
                        .first_unsaved_in(index, pane, &indices, &answers, cx)
                        .is_some()
                    {
                        self.select_connection(index, window, cx);
                        self.ask_unsaved(&request, &answers, pane, &indices, window, cx);
                        return;
                    }
                }
                self.close_connection_now(index, window, cx);
            }
        }
    }

    /// The indices `tabs` stand at in `pane` of the work area on screen, for
    /// the ones still there.
    fn indices_of(&self, pane: PaneId, tabs: &[gpui::EntityId]) -> Vec<usize> {
        let Some(pane) = self.work_area().and_then(|area| area.panes.get(pane)) else {
            return Vec::new();
        };
        pane.items()
            .iter()
            .enumerate()
            .filter(|(_, item)| tabs.contains(&item.entity_id()))
            .map(|(index, _)| index)
            .collect()
    }

    /// How many tabs `pane` holds, when it is a pane that can be closed: one
    /// of the work area on screen, and not its last.
    fn lone_pane_aside(&self, pane: PaneId) -> Option<usize> {
        let area = self.work_area()?;
        if area.panes.leaf_count() <= 1 {
            return None;
        }
        area.panes.get(pane).map(|pane| pane.items().len())
    }

    /// The first pane of connection `index` holding a tab whose staged edits
    /// a close would destroy.
    fn pane_with_staged_edits(&self, index: usize, cx: &App) -> Option<PaneId> {
        self.connections
            .get(index)?
            .work
            .panes
            .leaves()
            .into_iter()
            .find(|(_, pane)| pane.items().iter().any(|item| item.blocks_close(cx)))
            .map(|(id, _)| id)
    }

    /// The first of `indices` of `pane` in connection `index`'s area that is a
    /// query tab with changes its file does not have, and that has not been
    /// answered "Don't save" already.
    fn first_unsaved_in(
        &self,
        index: usize,
        pane: PaneId,
        indices: &[usize],
        answers: &CloseAnswers,
        cx: &App,
    ) -> Option<(usize, Entity<QueryPane>)> {
        let pane = self.connections.get(index)?.work.panes.get(pane)?;
        indices
            .iter()
            .find_map(|index| match pane.items().get(*index) {
                Some(PaneItem::Query { pane, .. })
                    if pane.read(cx).is_modified(cx)
                        && !answers.discard.contains(&pane.entity_id()) =>
                {
                    Some((*index, pane.clone()))
                }
                _ => None,
            })
    }

    /// Asks about the first of `indices` of `pane` (on screen) whose file is
    /// behind its text, and says whether it asked.
    ///
    /// The tab comes to the front before the question goes up, which is the
    /// same move [`Workspace::refuse_close`] makes and for the same reason:
    /// over a close of several tabs, the question has to be about one the user
    /// can see.
    fn ask_unsaved(
        &mut self,
        request: &CloseRequest,
        answers: &CloseAnswers,
        pane: PaneId,
        indices: &[usize],
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some((index, query)) =
            self.first_unsaved_in(self.active_connection, pane, indices, answers, cx)
        else {
            return false;
        };
        self.activate_tab(pane, index, window, cx);
        let name = self.tab_title(pane, index, cx);
        self.raise_close_question(
            request.clone(),
            answers.clone(),
            CloseAsk::Unsaved {
                pane: query,
                name,
                saving: false,
            },
            cx,
        );
        true
    }

    /// The title tab `index` of `pane` (on screen) carries.
    fn tab_title(&self, pane: PaneId, index: usize, cx: &App) -> SharedString {
        let Some(area) = self.work_area() else {
            return SharedString::default();
        };
        let Some(item) = area
            .panes
            .get(pane)
            .and_then(|pane| pane.items().get(index))
        else {
            return SharedString::default();
        };
        title_in(area, item, cx)
    }

    /// The name connection `index`'s tab carries.
    fn connection_name(&self, index: usize) -> SharedString {
        self.connections
            .get(index)
            .map(|open| connection_title(&open.profile))
            .unwrap_or_default()
    }

    /// Puts a question up, putting away the menus and the other questions a
    /// close could be raised under.
    fn raise_close_question(
        &mut self,
        request: CloseRequest,
        answers: CloseAnswers,
        ask: CloseAsk,
        cx: &mut Context<Self>,
    ) {
        self.menu_open = false;
        self.close_context_menus(cx);
        self.close_question = Some(CloseQuestion {
            request,
            answers,
            ask,
        });
        cx.notify();
    }

    /// What closing connection `index` would lose that only the connection
    /// holds, one sentence each.
    fn connection_risks(&self, index: usize, cx: &App) -> Vec<SharedString> {
        let Some(open) = self.connections.get(index) else {
            return Vec::new();
        };
        let mut reasons = Vec::new();
        if let ConnectionState::Open(connected) = &open.state
            && connected.handle().has_uncommitted()
        {
            reasons.push(ts!("closing.risk_uncommitted"));
        }
        for (_, pane) in open.work.panes.leaves() {
            for item in pane.items() {
                if let PaneItem::Query { pane: query, .. } = item
                    && query.read(cx).is_running()
                {
                    reasons.push(ts!(
                        "closing.risk_running",
                        tab = title_in(&open.work, item, cx).to_string()
                    ));
                }
            }
        }
        reasons
    }

    /// Everything closing the window now would lose, one line each, across
    /// every connection; empty when closing loses nothing.
    ///
    /// The list the close guard shows before a quit, a close of the window or
    /// the restart an update ends in, and so the whole of what "unsaved work"
    /// means in this window: rows or columns staged and not applied, a
    /// query tab whose file is behind its text, a statement still running, a
    /// transaction not yet committed, and a job of the three dialogs still
    /// writing. A draft bound to no file is not on it — the workspace keeps
    /// drafts across a restart — and neither is anything the database itself
    /// still has.
    pub(super) fn unsaved_work(&self, cx: &App) -> Vec<SharedString> {
        let mut lines = Vec::new();
        for open in &self.connections {
            let connection = connection_title(&open.profile).to_string();
            for (_, pane) in open.work.panes.leaves() {
                for item in pane.items() {
                    let tab = title_in(&open.work, item, cx).to_string();
                    if item.blocks_close(cx) {
                        lines.push(ts!(
                            "closing.work_staged",
                            tab = tab.clone(),
                            connection = connection.clone()
                        ));
                    }
                    if item.unsaved(cx) {
                        lines.push(ts!("closing.work_unsaved", tab = tab.clone()));
                    }
                    if let PaneItem::Query { pane, .. } = item
                        && pane.read(cx).is_running()
                    {
                        lines.push(ts!(
                            "closing.work_running",
                            tab = tab.clone(),
                            connection = connection.clone()
                        ));
                    }
                }
            }
            if let ConnectionState::Open(connected) = &open.state
                && connected.handle().has_uncommitted()
            {
                lines.push(ts!("closing.work_uncommitted", connection = connection));
            }
        }
        if self.extract.read(cx).is_running() {
            lines.push(ts!("closing.work_extract"));
        }
        if self.transfer.read(cx).is_running() {
            lines.push(ts!("closing.work_transfer"));
        }
        if self.backup.read(cx).is_running() {
            lines.push(ts!("closing.work_backup"));
        }
        lines
    }

    /// Answers the question a close is waiting on.
    ///
    /// "Cancel" puts it away and closes nothing. "Don't save" — and "close
    /// anyway" over a connection — records the answer and runs the
    /// close again from the top. "Save" writes the tab to its file and runs
    /// the close again once the write is in ([`Workspace::after_close_save`]);
    /// a save that fails or is abandoned cancels the close, and the tab says
    /// why itself.
    pub(super) fn answer_close(
        &mut self,
        answer: CloseAnswer,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(mut question) = self.close_question.take() else {
            return;
        };
        match answer {
            CloseAnswer::Cancel => {
                cx.notify();
            }
            CloseAnswer::Discard => {
                match &question.ask {
                    CloseAsk::Unsaved { pane, .. } => {
                        question.answers.discard.insert(pane.entity_id());
                    }
                    CloseAsk::Connection { .. } => {
                        question.answers.accepted = true;
                    }
                }
                self.request_close(question.request, question.answers, window, cx);
            }
            CloseAnswer::Save => {
                let CloseAsk::Unsaved { pane, saving, .. } = &mut question.ask else {
                    return;
                };
                if *saving {
                    self.close_question = Some(question);
                    return;
                }
                *saving = true;
                let pane = pane.clone();
                self.close_question = Some(question);
                self.save_query_pane(pane, window, cx);
                cx.notify();
            }
        }
    }

    /// Hears how a save went, and carries on the close that asked for it.
    ///
    /// Only a save the close question is waiting on counts: one pressed from
    /// the menu while no question is up is none of this method's business.
    pub(super) fn after_close_save(
        &mut self,
        pane: &Entity<QueryPane>,
        saved: Saved,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let waiting = matches!(
            &self.close_question,
            Some(CloseQuestion { ask: CloseAsk::Unsaved { pane: asked, saving: true, .. }, .. })
                if asked == pane
        );
        if !waiting {
            return;
        }
        match saved {
            // The file changed on disk, so the save dialog is asking where to
            // write instead; the close waits for that answer.
            Saved::Changed => {}
            Saved::InPlace | Saved::Rebound if !pane.read(cx).is_modified(cx) => {
                if let Some(question) = self.close_question.take() {
                    self.request_close(question.request, question.answers, window, cx);
                }
            }
            // Failed, or typed into while it was writing: either way the file
            // is not what the tab holds, and the tab is not closed.
            _ => {
                self.close_question = None;
                cx.notify();
            }
        }
    }

    /// Hears that the save dialog a close's save opened was dismissed.
    pub(super) fn close_save_abandoned(
        &mut self,
        pane: &Entity<QueryPane>,
        cx: &mut Context<Self>,
    ) {
        if matches!(
            &self.close_question,
            Some(CloseQuestion { ask: CloseAsk::Unsaved { pane: asked, saving: true, .. }, .. })
                if asked == pane
        ) {
            self.close_question = None;
            cx.notify();
        }
    }

    /// Writes `pane` to its file, as "Save" does for the tab on screen.
    fn save_query_pane(
        &mut self,
        pane: Entity<QueryPane>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let (bound, stamp) = {
            let query = pane.read(cx);
            (query.file().map(Path::to_path_buf), query.file_stamp())
        };
        match bound {
            Some(path) => self.save_pane_to(pane, path, Overwrite::IfUnchanged(stamp), window, cx),
            None => self.ask_where_to_save(pane, "", window, cx),
        }
    }

    /// Reopens the query or builder tab closed most recently on the connection
    /// on screen, into the active pane.
    ///
    /// It comes back as it went: its text, its selection, its name and — for
    /// a tab bound to a file — the binding, with its unsaved mark if it left
    /// with changes. A tab closed with "Don't save" is how that answer is
    /// undone. Needs a live session, as opening any tab does.
    pub(super) fn reopen_closed_tab(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.say_not_live(cx) {
            return;
        }
        let Some(open) = self.active_connection() else {
            return;
        };
        let id = open.id;
        let (next_query, next_builder) = (open.work.next_query, open.work.next_builder);
        let Some(tab) = self.work_area_mut().and_then(|area| area.closed.pop()) else {
            return;
        };
        self.restore_tab(&tab, id, window, cx);
        // The tab takes its old number back; the next new tab carries on from
        // where the count had got to.
        if let Some(area) = self.work_area_mut() {
            area.next_query = area.next_query.max(next_query);
            area.next_builder = area.next_builder.max(next_builder);
        }
        if let Some(pane) = self.active_pane() {
            self.focus_active_tab(pane, window, cx);
        }
        cx.notify();
    }
}

/// The title `item` carries in `area`: the user's name for it, or its own.
pub(super) fn title_in(area: &WorkArea, item: &PaneItem, cx: &App) -> SharedString {
    area.tab_names
        .get(&item.entity_id())
        .map(|name| SharedString::from(name.clone()))
        .unwrap_or_else(|| item.title(cx))
}

/// The name a connection's tab carries.
pub(super) fn connection_title(profile: &ConnectionProfile) -> SharedString {
    if profile.name.trim().is_empty() {
        ts!("connect.unnamed")
    } else {
        SharedString::from(profile.name.clone())
    }
}

impl Workspace {
    /// The question a close is waiting on, while one is.
    ///
    /// A modal at the window's root like the write confirmation, and for its
    /// reason: a dialog centers in its nearest positioned ancestor, and the
    /// question about a connection or a quit belongs to no pane. "Cancel" is
    /// what a press beside it and `Escape` answer — nothing is lost by not
    /// closing.
    pub(super) fn render_close_question(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let question = self.close_question.as_ref()?;
        let chrome = theme(cx);
        let this = cx.entity();
        let answer = move |answer: CloseAnswer| {
            let this = this.clone();
            move |_: &gpui::ClickEvent, window: &mut Window, cx: &mut App| {
                this.update(cx, |workspace, cx| {
                    workspace.answer_close(answer, window, cx)
                });
            }
        };
        let line = |text: SharedString| {
            div()
                .text_size(rugpui::TextSize::Sm.size(cx))
                .text_color(chrome.text)
                .child(text)
        };
        let (title, body, discard, save): (SharedString, Vec<SharedString>, SharedString, _) =
            match &question.ask {
                CloseAsk::Unsaved { name, saving, .. } => (
                    ts!("closing.unsaved_title"),
                    vec![ts!("closing.unsaved_body", name = name.to_string())],
                    ts!("closing.dont_save"),
                    Some(*saving),
                ),
                CloseAsk::Connection { name, reasons } => (
                    ts!("closing.connection_title", name = name.to_string()),
                    reasons.clone(),
                    ts!("closing.close_connection"),
                    None,
                ),
            };
        let saving = save == Some(true);

        let buttons = div()
            .flex()
            .flex_row()
            .justify_end()
            .gap(px(8.))
            .child(
                Button::new("close-question-cancel", ts!("common.cancel"))
                    .variant(ButtonVariant::Secondary)
                    .when(save.is_none(), |button| {
                        button.focus_handle(&self.close_focus)
                    })
                    .on_click(answer(CloseAnswer::Cancel)),
            )
            .child(
                Button::new("close-question-discard", discard)
                    .variant(if save.is_some() {
                        ButtonVariant::Secondary
                    } else {
                        ButtonVariant::Danger
                    })
                    .disabled(saving)
                    .on_click(answer(CloseAnswer::Discard)),
            )
            .children(save.map(|saving| {
                Button::new(
                    "close-question-save",
                    if saving {
                        ts!("closing.saving")
                    } else {
                        ts!("common.save")
                    },
                )
                .variant(ButtonVariant::Primary)
                .disabled(saving)
                .focus_handle(&self.close_focus)
                .on_click(answer(CloseAnswer::Save))
            }));

        let dismiss = cx.entity();
        Some(
            modal(
                "close-question",
                title,
                px(460.),
                div()
                    .flex()
                    .flex_col()
                    .gap(px(12.))
                    .child(
                        div()
                            .id("close-question-body")
                            .flex()
                            .flex_col()
                            .gap(px(4.))
                            .max_h(px(240.))
                            .overflow_y_scroll()
                            .children(body.into_iter().map(line)),
                    )
                    .child(buttons),
                move |window, cx| {
                    dismiss.update(cx, |workspace, cx| {
                        workspace.answer_close(CloseAnswer::Cancel, window, cx);
                    });
                },
            )
            // The default answer takes the keyboard: Save over a file — it
            // loses nothing — and Cancel over a connection, whose other answer
            // rolls a transaction back. `Escape` is Cancel either way.
            .initial_focus(&self.close_focus)
            .escape_dismisses(true)
            .into_any_element(),
        )
    }
}

impl Workspace {
    /// Saves every query tab bound to a file whose text the file does not
    /// have, across every connection — the close guard's "Save all" — and
    /// answers whether every one of them was written.
    ///
    /// Each goes to its own file only while that file is as the tab last saw
    /// it: one changed on disk since is not written over, the tab says so, and
    /// the answer is `false`, which leaves the guard's question up with the
    /// tab still on its list. A tab bound to no file is a draft, not on the
    /// list, and not asked where to go.
    pub(super) fn save_all_files(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Task<bool> {
        let panes: Vec<Entity<QueryPane>> = self
            .connections
            .iter()
            .flat_map(|open| open.work.queries())
            .filter(|pane| pane.read(cx).is_modified(cx))
            .collect();
        let mut writes = Vec::new();
        for pane in panes {
            let (path, stamp) = {
                let query = pane.read(cx);
                (query.file().map(Path::to_path_buf), query.file_stamp())
            };
            let Some(path) = path else {
                continue;
            };
            let Some(SaveJob { text, edits }) = pane.update(cx, |pane, cx| pane.begin_save(cx))
            else {
                // Already writing, or rereading its file: not saved by this.
                writes.push(None);
                continue;
            };
            let write = cx.background_spawn({
                let path = path.clone();
                async move { write_sql_file(&path, &text, Overwrite::IfUnchanged(stamp)) }
            });
            writes.push(Some((pane, path, edits, write)));
        }
        cx.spawn_in(window, async move |workspace, cx| {
            let mut all = true;
            for write in writes {
                let Some((pane, path, edits, write)) = write else {
                    all = false;
                    continue;
                };
                let outcome = write.await;
                let saved = workspace
                    .update(cx, |_, cx| {
                        pane.update(cx, |pane, cx| pane.finish_save(path, edits, outcome, cx))
                    })
                    .unwrap_or(Saved::Failed);
                all &= matches!(saved, Saved::InPlace | Saved::Rebound);
            }
            all
        })
    }
}
