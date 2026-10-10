//! Panes.

use super::*;

impl Workspace {
    /// Brings the tab `index` of `pane` to the front, and the pane marker with
    /// it.
    ///
    /// The keyboard follows only if it was inside the tab going off screen:
    /// activating a tab from the explorer, which is where a click that lands
    /// here comes from, must not pull the caret out of the sidebar.
    pub(super) fn activate_tab(
        &mut self,
        pane: PaneId,
        index: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(area) = self.work_area_mut() else {
            return;
        };
        if !area.panes.contains(pane) {
            return;
        }
        area.active_pane = pane;
        let held = self.pane_holds_focus(pane, window, cx);
        let moved = self
            .work_area_mut()
            .and_then(|area| area.panes.get_mut(pane))
            .is_some_and(|pane| pane.activate(index));
        if moved && held {
            self.focus_active_tab(pane, window, cx);
        }
        cx.notify();
    }

    /// Closes the tab `index` of `pane`.
    ///
    /// Dropping the tab drops the view in it, which closes whatever cursor or
    /// fetch it was holding. The neighbour that takes its place inherits the
    /// keyboard when the closed tab had it, because the tab strip is a place a
    /// user closes several tabs in a row from and a focus that fell back to the
    /// shell every time would swallow the editor shortcuts in between.
    pub(super) fn close_tab(
        &mut self,
        pane: PaneId,
        index: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.refuse_close(pane, &[index], window, cx) {
            return;
        }
        let active = self
            .work_area()
            .and_then(|area| area.panes.get(pane))
            .is_some_and(|pane| pane.active_index() == index);
        // Only the active tab is rendered, so only it can be holding the
        // keyboard; see [`Workspace::reclaim_focus`].
        let held = active && self.pane_holds_focus(pane, window, cx);
        let Some(closed) = self
            .work_area_mut()
            .and_then(|area| area.panes.get_mut(pane))
            .and_then(|pane| pane.close(index))
        else {
            return;
        };
        drop(closed);
        if held {
            self.focus_active_tab(pane, window, cx);
        }
        self.drop_stale_confirm(cx);
        cx.notify();
    }

    /// Whether any of `victims` is holding work a close would destroy, and —
    /// when one is — says so in the tab it is holding it in.
    ///
    /// The close guard §7.9 asks for, and the whole of it: a data pane's staged
    /// edits are keyed to the rows under its grid, so closing the tab throws
    /// them away with no way back. Refusing rather than asking, because the two
    /// answers the user needs are already in the pane and neither of them is a
    /// dialog: apply the changes, or discard them. So the tab is brought to the
    /// front, the pane says what has to happen first, and the close does not.
    ///
    /// A query pane answers the same question about a result grid that passed
    /// §7.9's gate, and for the same reason: what is staged there is keyed to
    /// the rows of one result, which the close would drop. A structure pane
    /// answers it about its own staging, keyed to indices into the snapshot it
    /// was read against (§7.10).
    ///
    /// All the victims are asked before any is refused, and the first blocker
    /// is the one shown — "close the other tabs" over three dirty panes should
    /// close none of them and land on one, rather than close two and stop.
    pub(super) fn refuse_close(
        &mut self,
        pane: PaneId,
        victims: &[usize],
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(target) = self.work_area().and_then(|area| area.panes.get(pane)) else {
            return false;
        };
        let mut blocked = None;
        for index in victims {
            match target.items().get(*index) {
                Some(PaneItem::TableData(panel)) if panel.read(cx).has_pending_edits(cx) => {
                    blocked = Some((*index, Pending::Data(panel.clone())));
                    break;
                }
                Some(PaneItem::TableStruct(panel)) if panel.read(cx).has_pending_edits() => {
                    blocked = Some((*index, Pending::Struct(panel.clone())));
                    break;
                }
                Some(PaneItem::Query { pane, .. }) if pane.read(cx).has_pending_edits(cx) => {
                    blocked = Some((*index, Pending::Query(pane.clone())));
                    break;
                }
                // Asked through the same seam every other tab answers, so that
                // a kind of tab that grows unsaved work later has one place to
                // say so.
                Some(item) => debug_assert!(!item.blocks_close(cx)),
                None => {}
            }
        }
        let Some((index, panel)) = blocked else {
            return false;
        };
        self.activate_tab(pane, index, window, cx);
        match panel {
            Pending::Data(panel) => panel.update(cx, |panel, cx| panel.warn_pending(cx)),
            Pending::Query(pane) => pane.update(cx, |pane, cx| pane.warn_pending(cx)),
            Pending::Struct(panel) => panel.update(cx, |panel, cx| panel.warn_pending(cx)),
        }
        true
    }

    /// Closes several tabs of `pane` at once.
    ///
    /// `victims` are tab indices into the strip as it stands, in any order.
    /// They are removed from the highest down so that the ones still to go do
    /// not shift out from under the list — which is the whole reason this is
    /// not a loop over [`Workspace::close_tab`].
    ///
    /// The keyboard follows the same rule one close does, and is asked about
    /// once rather than once per tab: only the active tab is rendered, so only
    /// it can be holding the focus, and whatever is left on top afterwards
    /// takes it. That is what makes "close the other tabs" leave the user
    /// typing in the tab they kept.
    pub(super) fn close_tabs(
        &mut self,
        pane: PaneId,
        victims: &[usize],
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if victims.is_empty() {
            return;
        }
        if self.refuse_close(pane, victims, window, cx) {
            return;
        }
        let held = self.pane_holds_focus(pane, window, cx);
        let Some(target) = self
            .work_area_mut()
            .and_then(|area| area.panes.get_mut(pane))
        else {
            return;
        };

        let mut order = victims.to_vec();
        order.sort_unstable();
        order.dedup();
        // Dropping the tabs drops the views in them, and with those whatever
        // cursor or fetch each was holding.
        let mut closed = Vec::new();
        for index in order.into_iter().rev() {
            closed.extend(target.close(index));
        }
        drop(closed);

        if held {
            self.focus_active_tab(pane, window, cx);
        }
        self.drop_stale_confirm(cx);
        cx.notify();
    }

    /// Splits `pane` along `axis`, whether or not the marker was on it.
    ///
    /// The menu row acts on the pane that was right-clicked, which is not
    /// necessarily the active one — a right-click moves no marker — so the
    /// marker is moved there first and the ordinary command run from where it
    /// then stands.
    pub(super) fn split_pane(&mut self, pane: PaneId, axis: Axis, cx: &mut Context<Self>) {
        if !self.mark_pane(pane) {
            return;
        }
        self.split_active(axis, cx);
    }

    /// Closes `pane`, whether or not the marker was on it.
    ///
    /// The marker is moved for the reason [`Workspace::split_pane`] moves it,
    /// and moved *before* the pane goes so that the focus question
    /// [`Workspace::close_active_pane`] asks is asked about the right one.
    pub(super) fn close_pane(&mut self, pane: PaneId, window: &mut Window, cx: &mut Context<Self>) {
        if !self.mark_pane(pane) {
            return;
        }
        self.close_active_pane(window, cx);
    }

    /// Puts the pane marker on `pane`, and says whether that pane exists.
    pub(super) fn mark_pane(&mut self, pane: PaneId) -> bool {
        let Some(area) = self.work_area_mut() else {
            return false;
        };
        if !area.panes.contains(pane) {
            return false;
        }
        area.active_pane = pane;
        true
    }

    /// Puts the keyboard on the active tab of `pane`, or on the shell when that
    /// tab has nothing to type into.
    ///
    /// A query pane takes the caret into its editor, which is what makes closing
    /// the tab in front of one leave the user typing where they were. An ERD
    /// takes the keyboard onto its canvas, where the zoom and auto-arrange
    /// chords are bound, and a data pane onto its grid, where the arrows and
    /// the copy chord are. A detail panel and an empty pane fall back to the
    /// shell, whose handlers are what keep the menu rows and the shortcuts
    /// alive.
    pub(super) fn focus_active_tab(
        &mut self,
        pane: PaneId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let target = match self
            .work_area()
            .and_then(|area| area.panes.get(pane))
            .and_then(Pane::active)
        {
            Some(PaneItem::Query { pane, .. }) => FocusTarget::Query(pane.clone()),
            Some(PaneItem::Erd(panel)) => FocusTarget::Erd(panel.clone()),
            Some(PaneItem::TableData(panel)) => FocusTarget::Data(panel.clone()),
            Some(PaneItem::TableStruct(panel)) => FocusTarget::Struct(panel.clone()),
            Some(PaneItem::QueryBuilder { pane, .. }) => FocusTarget::Builder(pane.clone()),
            Some(PaneItem::TableDetail(_)) | None => FocusTarget::Shell,
        };
        match target {
            FocusTarget::Query(pane) => pane.update(cx, |pane, cx| pane.focus_editor(window, cx)),
            FocusTarget::Erd(panel) => panel.update(cx, |panel, cx| panel.take_focus(window, cx)),
            FocusTarget::Data(panel) => {
                panel.update(cx, |panel, cx| panel.take_focus(window, cx));
            }
            FocusTarget::Builder(panel) => {
                panel.update(cx, |panel, cx| panel.take_focus(window, cx));
            }
            FocusTarget::Struct(panel) => {
                panel.update(cx, |panel, cx| panel.take_focus(window, cx));
            }
            FocusTarget::Shell => self.focus_shell(window, cx),
        }
    }

    /// Drops a write confirmation whose pane is no longer open anywhere.
    ///
    /// The dialog asks on behalf of one query pane and sends the answer back to
    /// it; a pane that has been closed has nobody to answer.
    ///
    /// Every connection's work area is searched, not just the one on screen: a
    /// pane of another connection is merely hidden, and a confirmation it is
    /// waiting on is still live. Only closing that connection — which discards
    /// its whole area — makes the question unanswerable.
    pub(super) fn drop_stale_confirm(&mut self, cx: &mut Context<Self>) {
        let Some(pending) = &self.confirm else {
            return;
        };
        let open = self.connections.iter().any(|open| {
            open.work.panes.leaves().into_iter().any(|(_, pane)| {
                pane.items().iter().any(
                    |item| matches!(item, PaneItem::Query { pane, .. } if pane == &pending.pane),
                )
            })
        });
        if !open {
            self.confirm = None;
            cx.notify();
        }
    }

    /// Puts the explorer's root for one connection in step with its tab.
    pub(super) fn sync_explorer_root(&mut self, index: usize, cx: &mut Context<Self>) {
        let Some(open) = self.connections.get(index) else {
            return;
        };
        let info = RootInfo {
            name: if open.profile.name.trim().is_empty() {
                ts!("connect.unnamed")
            } else {
                SharedString::from(open.profile.name.clone())
            },
            color: open.profile.color.clone().map(SharedString::from),
            live: matches!(open.state, ConnectionState::Open(_)),
        };
        let id = open.id;
        let live = info.live;
        self.explorer.update(cx, |explorer, cx| {
            explorer.update_source(cx, |source| source.upsert_root(id, info));
            // A root opened while the handshake was still out answered "the
            // connection is closed"; now that there is a session, that row has
            // to go rather than stay until the tab does.
            if live {
                explorer.reload(&NodeId::Connection(id), cx);
            }
        });
    }

    /// Points the explorer at the connection whose tab is on top.
    ///
    /// Called from everywhere [`Workspace::active_connection`] changes, and from
    /// nowhere else: the tree keeps every root it has ever been given, and this
    /// is the whole of what makes it show one of them. `None` — no connection
    /// open at all — leaves it with an empty root level and its own empty
    /// wording.
    pub(super) fn sync_visible_root(&mut self, cx: &mut Context<Self>) {
        let visible = self.active_connection().map(|open| open.id);
        self.explorer.update(cx, |explorer, cx| {
            explorer.update_source(cx, |source| source.set_visible_root(visible));
        });
    }

    /// Whether the sidebar is actually on screen.
    ///
    /// Two conditions, and only one of them is the user's: the panel is drawn
    /// when they have asked for it *and* there is a connection for it to show.
    /// A tree of nothing beside a welcome screen is a column of chrome with no
    /// content, so the welcome screen takes the whole width — and because the
    /// preference itself is left alone, the sidebar comes straight back with the
    /// first connection rather than having to be asked for again.
    pub(super) fn explorer_showing(&self) -> bool {
        !self.connections.is_empty() && self.explorer_visible
    }

    /// Shows or hides the sidebar, and remembers which.
    pub(super) fn toggle_explorer(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.explorer_visible = !self.explorer_visible;
        if !self.explorer_visible {
            // The tree takes the focus when a row is clicked, and hiding the
            // sidebar leaves it holding a focus nothing renders any more; see
            // [`Workspace::reclaim_focus`].
            let explorer = self.explorer.read(cx).focus_handle(cx);
            self.reclaim_focus(&explorer, window, cx);
        }
        let mut settings = app_settings::current(cx);
        settings.explorer_visible = self.explorer_visible;
        app_settings::replace(settings, cx);
        cx.notify();
    }

    /// Records what a connection attempt produced.
    pub(super) fn connected(
        &mut self,
        index: usize,
        outcome: Result<Connected, ConnectError>,
        cx: &mut Context<Self>,
    ) {
        let Some(connection) = self.connections.get_mut(index) else {
            // The tab was closed while the attempt was in flight; the session,
            // if one opened, is closed by `Connected`'s own drop.
            return;
        };

        match outcome {
            Ok(connected) => {
                // A tunnel that dies takes the session above it with it, and the
                // tab has to say so rather than going quiet: the transaction the
                // user was in the middle of is gone (§9.3).
                if let Some(lease) = connected.lease() {
                    let died = lease.watch();
                    cx.spawn(async move |workspace, cx| {
                        let Ok(reason) = died.await else {
                            return;
                        };
                        workspace
                            .update(cx, |workspace, cx| {
                                workspace.tunnel_died(index, reason, cx);
                            })
                            .ok();
                    })
                    .detach();
                }
                connection.state = ConnectionState::Open(Box::new(connected));
            }
            Err(error) => {
                log::warn!("connecting {} failed: {error}", connection.profile.name);
                connection.state = ConnectionState::Failed(error.message().into());
            }
        }
        self.sync_explorer_root(index, cx);
        cx.notify();
    }

    /// Moves the keyboard onto the work area now on screen, when the one that
    /// left had it.
    ///
    /// `held` is what [`Workspace::area_holds_focus`] answered before the
    /// switch. The incoming area's active tab takes the caret if it has one to
    /// take, and the shell takes it otherwise — including when no connection is
    /// left at all. Doing nothing instead would leave the focus on an editor
    /// nothing renders, which swallows every action from then on.
    pub(super) fn follow_work_area(
        &mut self,
        held: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !held {
            return;
        }
        match self.active_pane() {
            Some(pane) => self.focus_active_tab(pane, window, cx),
            None => self.focus_shell(window, cx),
        }
    }

    /// The active pane of the work area on screen.
    ///
    /// `None` with no connection open, which is what makes every pane command
    /// below a no-op rather than an operation on a tree nobody can see.
    pub(super) fn active_pane(&self) -> Option<PaneId> {
        self.work_area().map(WorkArea::active)
    }

    /// Splits the active pane along `axis` and moves the marker to the new one.
    pub(super) fn split_active(&mut self, axis: Axis, cx: &mut Context<Self>) {
        let Some(target) = self.active_pane() else {
            return;
        };
        let Some(area) = self.work_area_mut() else {
            return;
        };
        let Some(new) = area.panes.split(target, axis, Pane::new()) else {
            return;
        };
        area.active_pane = new;
        cx.notify();
    }

    /// Closes the active pane, unless it is the last one of its work area.
    ///
    /// The marker moves to the pane that follows the closed one in layout order,
    /// which is the neighbour that grew into the freed space.
    pub(super) fn close_active_pane(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(target) = self.active_pane() else {
            return;
        };
        let Some(area) = self.work_area() else {
            return;
        };
        let next = area.panes.next_leaf(target).unwrap_or(target);
        // Asked before the pane goes, because afterwards there is nothing left
        // to ask whether its editor or its detail panel had the keyboard.
        let held = self.pane_holds_focus(target, window, cx);
        let Some(area) = self.work_area_mut() else {
            return;
        };
        if area.panes.remove(target).is_none() {
            return;
        }
        area.active_pane = if area.panes.contains(next) {
            next
        } else {
            area.panes.first_leaf().0
        };
        if held {
            self.focus_shell(window, cx);
        }
        self.drop_stale_confirm(cx);
        cx.notify();
    }

    /// Moves the pane marker one step along the layout order.
    pub(super) fn cycle_pane(&mut self, forward: bool, cx: &mut Context<Self>) {
        let Some(from) = self.active_pane() else {
            return;
        };
        let Some(area) = self.work_area_mut() else {
            return;
        };
        let next = if forward {
            area.panes.next_leaf(from)
        } else {
            area.panes.prev_leaf(from)
        };
        if let Some(next) = next {
            area.active_pane = next;
            cx.notify();
        }
    }

    /// Puts the keyboard back on the shell after a dialog closes.
    pub(super) fn focus_shell(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        window.focus(&self.focus_handle, cx);
        cx.notify();
    }

    /// Takes the keyboard back from `subtree`, which is about to stop being
    /// rendered, if the focus is anywhere inside it.
    ///
    /// gpui never clears the window focus when the focused element leaves the
    /// tree, and it resolves both dispatched actions and key bindings against
    /// the focused element *of the frame that was last drawn*; an element that
    /// is no longer in it resolves to the window root, which the workspace's
    /// `on_action` handlers do not sit on. A focus left behind on a hidden
    /// sidebar or a closed pane therefore swallows every menu row and every
    /// shortcut, silently and for good.
    ///
    /// This has to run in the same update that removes the subtree, and reads
    /// the *previous* frame — the one that still holds it — which is exactly
    /// what [`FocusHandle::contains_focused`] answers from.
    pub(super) fn reclaim_focus(
        &mut self,
        subtree: &FocusHandle,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if subtree.contains_focused(window, cx) {
            self.focus_shell(window, cx);
        }
    }

    /// Whether the keyboard is inside what `pane` is showing, as the last drawn
    /// frame had it.
    ///
    /// Tab aware, and only the active tab needs asking: the others are not
    /// rendered, so nothing in them can hold the focus. A query pane answers for
    /// both of its focusable halves — the editor and the grid of the active
    /// result — through [`QueryPane::contains_focus`], because its
    /// [`Focusable`] impl names only the editor and a focus left on a grid would
    /// strand exactly as [`Workspace::reclaim_focus`] describes.
    pub(super) fn pane_holds_focus(&self, pane: PaneId, window: &Window, cx: &App) -> bool {
        match self
            .work_area()
            .and_then(|area| area.panes.get(pane))
            .and_then(Pane::active)
        {
            Some(PaneItem::Query { pane, .. }) => pane.read(cx).contains_focus(window, cx),
            Some(PaneItem::TableDetail(panel)) => {
                panel.read(cx).focus_handle(cx).contains_focused(window, cx)
            }
            // Two handles, for the reason a query pane has two: the canvas
            // takes the focus for itself when a box is pressed, and an ERD
            // whose canvas held the keyboard would strand it exactly as
            // [`Workspace::reclaim_focus`] describes.
            Some(PaneItem::Erd(panel)) => panel.read(cx).contains_focus(window, cx),
            // Two handles again, and for the same reason: the builder's canvas
            // takes the focus when a box or a column row is pressed, and the
            // data pane's grid when a cell is clicked.
            Some(PaneItem::QueryBuilder { pane, .. }) => pane.read(cx).contains_focus(window, cx),
            Some(PaneItem::TableData(panel)) => panel.read(cx).contains_focus(window, cx),
            // And a third: the structure pane's four fields take the keyboard
            // the moment one is clicked into.
            Some(PaneItem::TableStruct(panel)) => panel.read(cx).contains_focus(window, cx),
            None => false,
        }
    }

    /// Whether any modal is on screen.
    ///
    /// Exactly the set [`Workspace::close_overlays`] closes, minus the dropdown
    /// and the context menus: those are transient and dismiss themselves on the
    /// next press, so a dialog appearing over one takes nothing away.
    ///
    /// One caller, and the reason this exists at all: the start-up update check
    /// announces itself only into an empty window. It is the one dialog nobody
    /// asked for, and it must not land on top of a half-typed connection form
    /// or a running backup.
    pub(super) fn dialog_open(&self, cx: &App) -> bool {
        self.confirm.is_some()
            || self.about.read(cx).is_open()
            || self.connect.read(cx).is_open()
            || self.settings.read(cx).is_open()
            || self.extract.read(cx).is_open()
            || self.transfer.read(cx).is_open()
            || self.backup.read(cx).is_open()
            || self.update.read(cx).is_open()
    }
}
