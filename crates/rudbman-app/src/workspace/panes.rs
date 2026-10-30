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

    /// Closes the tab `index` of `pane`, through every guard a close passes;
    /// see [`Workspace::request_close`].
    pub(super) fn close_tab(
        &mut self,
        pane: PaneId,
        index: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.close_tabs(pane, &[index], window, cx);
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

    /// Closes several tabs of `pane` at once, through every guard a close
    /// passes; see [`Workspace::request_close`].
    ///
    /// `victims` are tab indices into the strip as it stands, in any order.
    /// They are turned into the tabs' identities before anything is asked, so
    /// a question answered a moment later still closes the tabs it was asked
    /// about.
    pub(super) fn close_tabs(
        &mut self,
        pane: PaneId,
        victims: &[usize],
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(target) = self.work_area().and_then(|area| area.panes.get(pane)) else {
            return;
        };
        let tabs: Vec<gpui::EntityId> = victims
            .iter()
            .filter_map(|index| target.items().get(*index).map(PaneItem::entity_id))
            .collect();
        if tabs.is_empty() {
            return;
        }
        self.request_close(
            closing::CloseRequest::Tabs { pane, tabs },
            closing::CloseAnswers::default(),
            window,
            cx,
        );
    }

    /// Closes several tabs of `pane`, now and unasked.
    ///
    /// They are removed from the highest index down so that the ones still to
    /// go do not shift out from under the list. Every query and builder tab
    /// among them is remembered for "Reopen closed tab" first.
    ///
    /// The keyboard follows the rule one close always followed, and is asked
    /// about once rather than once per tab: only the active tab is rendered,
    /// so only it can be holding the focus, and whatever is left on top
    /// afterwards takes it — which is what makes "close the other tabs" leave
    /// the user typing in the tab they kept, and closing a tab in the
    /// background leave the keyboard where it was. The neighbor that takes a
    /// closed tab's place inherits the keyboard when the closed tab had it,
    /// because the tab strip is a place a user closes several tabs in a row
    /// from, and a focus that fell back to the shell every time would swallow
    /// the editor shortcuts in between.
    pub(super) fn close_tabs_now(
        &mut self,
        pane: PaneId,
        victims: &[usize],
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let active = self
            .work_area()
            .and_then(|area| area.panes.get(pane))
            .map(Pane::active_index);
        let held = active.is_some_and(|active| victims.contains(&active))
            && self.pane_holds_focus(pane, window, cx);
        let mut order = victims.to_vec();
        order.sort_unstable();
        order.dedup();
        for index in &order {
            self.remember_closed(pane, *index, cx);
        }
        let Some(target) = self
            .work_area_mut()
            .and_then(|area| area.panes.get_mut(pane))
        else {
            return;
        };
        // Dropping the tabs drops the views in them, and with those whatever
        // cursor or fetch each was holding.
        let mut closed = Vec::new();
        for index in order.into_iter().rev() {
            closed.extend(target.close(index));
        }
        if closed.is_empty() {
            return;
        }
        if let Some(area) = self.work_area_mut() {
            for item in &closed {
                area.tab_names.remove(&item.entity_id());
            }
        }
        drop(closed);
        self.finish_tab_close(pane, held, window, cx);
        self.drop_stale_confirm(cx);
        cx.notify();
    }

    /// Moves the tab `src_index` of `src_pane` to before `dst_index` of
    /// `dst_pane`.
    ///
    /// A move within one pane reorders its strip; a move across panes carries
    /// the tab over and brings it to the top there. `dst_index` past the end
    /// clamps to the end, so a drop over a strip's empty stretch or a pane
    /// body names `usize::MAX` for "after the last tab". A move that would
    /// change nothing is a no-op.
    ///
    /// Unlike a close this destroys no work, so the dirty-tab guard stays out
    /// of it — and `tab_names` rides along untouched, being keyed by the view
    /// rather than by any index. An emptied source collapses exactly as a
    /// closed last tab leaves it: away while a neighbor grows into its space,
    /// standing while it is the only pane of the area. The keyboard follows
    /// the moved tab when it had it, and the marker follows the tab to its
    /// new pane either way.
    pub(super) fn move_tab(
        &mut self,
        src_pane: PaneId,
        src_index: usize,
        dst_pane: PaneId,
        dst_index: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.work_area().is_none() {
            return;
        }
        let in_tree = self
            .work_area()
            .is_some_and(|area| area.panes.contains(src_pane) && area.panes.contains(dst_pane));
        if !in_tree {
            return;
        }
        // Only the tab on top is rendered, so only it can be holding the
        // keyboard; the move rewrites which tab that is, hence asked before
        // anything moves.
        let moved_active = self.work_area().is_some_and(|area| {
            area.panes
                .get(src_pane)
                .is_some_and(|pane| pane.active_index() == src_index)
        });
        let held = moved_active && self.pane_holds_focus(src_pane, window, cx);
        if src_pane == dst_pane {
            let moved = self
                .work_area_mut()
                .and_then(|area| area.panes.get_mut(src_pane))
                .is_some_and(|pane| {
                    let len = pane.items().len();
                    pane.relocate(src_index, dst_index.min(len))
                });
            if !moved {
                return;
            }
            if let Some(area) = self.work_area_mut() {
                area.active_pane = dst_pane;
            }
            if held {
                self.focus_active_tab(dst_pane, window, cx);
            }
            cx.notify();
            return;
        }
        let Some(item) = self
            .work_area_mut()
            .and_then(|area| area.panes.get_mut(src_pane))
            .and_then(|pane| pane.close(src_index))
        else {
            return;
        };
        let Some(area) = self.work_area_mut() else {
            return;
        };
        let Some(dst) = area.panes.get_mut(dst_pane) else {
            return;
        };
        dst.insert(dst_index.min(dst.items().len()), item);
        area.active_pane = dst_pane;
        let empty = area.panes.get(src_pane).is_some_and(Pane::is_empty);
        if empty && area.panes.leaf_count() > 1 {
            area.panes.remove(src_pane);
        }
        if held {
            self.focus_active_tab(dst_pane, window, cx);
        }
        cx.notify();
    }

    /// Collapse an emptied pane while preserving the last leaf and the focus
    /// of a different pane when its tab was closed from a context menu.
    fn finish_tab_close(
        &mut self,
        pane: PaneId,
        held: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(area) = self.work_area_mut() else {
            return;
        };
        let empty = area.panes.get(pane).is_some_and(Pane::is_empty);
        let focus = if empty && area.panes.leaf_count() > 1 {
            let next = area.panes.next_leaf(pane).unwrap();
            area.panes.remove(pane);
            if area.active_pane == pane {
                area.active_pane = next;
            }
            area.active()
        } else {
            pane
        };
        if held {
            self.focus_active_tab(focus, window, cx);
        }
    }

    /// Splits `pane` along `axis`, whether or not the marker was on it.
    ///
    /// The menu row acts on the pane that was right-clicked, which is not
    /// necessarily the active one — a right-click moves no marker — so the
    /// marker is moved there first and the ordinary command run from where it
    /// then stands.
    pub(super) fn split_pane(
        &mut self,
        pane: PaneId,
        axis: Axis,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.mark_pane(pane) {
            return;
        }
        self.split_active(axis, window, cx);
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
                    let id = connection.id;
                    cx.spawn(async move |workspace, cx| {
                        let Ok(reason) = died.await else {
                            return;
                        };
                        workspace
                            .update(cx, |workspace, cx| {
                                if let Some(index) =
                                    workspace.connections.iter().position(|open| open.id == id)
                                {
                                    workspace.tunnel_died(index, reason, cx);
                                }
                            })
                            .ok();
                    })
                    .detach();
                }
                connection.state = ConnectionState::Open(Box::new(connected));
            }
            Err(error) => {
                log::warn!("connecting {} failed: {error}", connection.profile.name);
                connection.state =
                    ConnectionState::Failed(Failure::of(&error, &connection.profile));
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
    pub(super) fn split_active(&mut self, axis: Axis, window: &mut Window, cx: &mut Context<Self>) {
        // Recreating a tab needs a live session. Do not replace the requested
        // copy with an empty pane after the connection has died.
        if !self
            .active_connection()
            .is_some_and(|open| matches!(open.state, ConnectionState::Open(_)))
        {
            return;
        }
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
        self.duplicate_tab_into(target, new, window, cx);
        cx.notify();
    }

    /// Closes the active pane, unless it is the last one of its work area,
    /// through every guard a close passes; see [`Workspace::request_close`].
    ///
    /// The guards are the same ones a close of each of its tabs would meet:
    /// a pane is closed by `Alt+W` (`Cmd+W` on macOS) and by its strip's menu,
    /// and a pane holding a tab with staged edits, or a file with changes, is
    /// holding them whichever of the two gestures reaches for it.
    pub(super) fn close_active_pane(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(target) = self.active_pane() else {
            return;
        };
        self.request_close(
            closing::CloseRequest::Pane(target),
            closing::CloseAnswers::default(),
            window,
            cx,
        );
    }

    /// Closes `pane`, now and unasked, unless it is the last one of its work
    /// area.
    ///
    /// The marker moves to the pane that follows the closed one in layout
    /// order, which is the neighbor that grew into the freed space.
    pub(super) fn close_pane_now(
        &mut self,
        target: PaneId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(area) = self.work_area() else {
            return;
        };
        if area.panes.leaf_count() <= 1 {
            return;
        }
        let count = area.panes.get(target).map_or(0, |pane| pane.items().len());
        let next = area.panes.next_leaf(target).unwrap_or(target);
        // Asked before the pane goes, because afterwards there is nothing left
        // to ask whether its editor or its detail panel had the keyboard.
        let held = self.pane_holds_focus(target, window, cx);
        for index in 0..count {
            self.remember_closed(target, index, cx);
        }
        let Some(area) = self.work_area_mut() else {
            return;
        };
        let Some(closed) = area.panes.remove(target) else {
            return;
        };
        for item in closed.items() {
            area.tab_names.remove(&item.entity_id());
        }
        drop(closed);
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

    /// Moves the pane marker one step along the layout order, and the
    /// keyboard with it.
    ///
    /// The keyboard follows because the marker is where typing is expected to
    /// go: a frame drawn round one pane while the caret stayed in the other
    /// had the user typing into the pane they had just moved away from.
    pub(super) fn cycle_pane(
        &mut self,
        forward: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
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
            self.focus_active_tab(next, window, cx);
            cx.notify();
        }
    }

    /// Brings the tab one step along the active pane's strip to the front,
    /// wrapping at either end, and puts the keyboard in it.
    pub(super) fn cycle_tab(&mut self, forward: bool, window: &mut Window, cx: &mut Context<Self>) {
        let Some(pane) = self.active_pane() else {
            return;
        };
        let Some((count, active)) = self
            .work_area()
            .and_then(|area| area.panes.get(pane))
            .map(|pane| (pane.items().len(), pane.active_index()))
        else {
            return;
        };
        if count < 2 {
            return;
        }
        let next = if forward {
            (active + 1) % count
        } else {
            (active + count - 1) % count
        };
        self.show_tab(pane, next, window, cx);
    }

    /// Brings tab `index` of the active pane to the front — the last one for
    /// `8` — and puts the keyboard in it.
    pub(super) fn select_tab(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(pane) = self.active_pane() else {
            return;
        };
        let Some(count) = self
            .work_area()
            .and_then(|area| area.panes.get(pane))
            .map(|pane| pane.items().len())
        else {
            return;
        };
        if count == 0 {
            return;
        }
        let index = if index >= 8 { count - 1 } else { index };
        if index < count {
            self.show_tab(pane, index, window, cx);
        }
    }

    /// Brings tab `index` of `pane` to the front with the keyboard in it —
    /// what a tab chosen from the keyboard is for, where a click on a strip
    /// leaves the keyboard where it was.
    fn show_tab(
        &mut self,
        pane: PaneId,
        index: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.activate_tab(pane, index, window, cx);
        self.focus_active_tab(pane, window, cx);
    }

    /// Brings the connection tab one step along the strip to the front,
    /// wrapping, and the keyboard into its work area.
    pub(super) fn cycle_connection(
        &mut self,
        forward: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let count = self.connections.len();
        if count < 2 {
            return;
        }
        let next = if forward {
            (self.active_connection + 1) % count
        } else {
            (self.active_connection + count - 1) % count
        };
        self.select_connection(next, window, cx);
        match self.active_pane() {
            Some(pane) => self.focus_active_tab(pane, window, cx),
            None => self.focus_shell(window, cx),
        }
    }

    /// Moves the keyboard to the next region of the window — the explorer,
    /// the active tab, and a query tab's results — wrapping round, as `F6`
    /// does in every application with panes.
    ///
    /// The regions are the ones there are right now: no explorer while it is
    /// hidden, no results while the tab on top has none to show. From
    /// anywhere else — the shell, after a dialog — the first region is next.
    pub(super) fn cycle_region(
        &mut self,
        forward: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let query = self.active_query().cloned();
        let has_results = query
            .as_ref()
            .is_some_and(|pane| pane.read(cx).has_result_grid());
        let mut regions = Vec::new();
        if self.explorer_showing() {
            regions.push(Region::Explorer);
        }
        if self.active_pane().is_some() {
            regions.push(Region::Tab);
        }
        if has_results {
            regions.push(Region::Results);
        }
        if regions.is_empty() {
            return;
        }
        let explorer = self.explorer.read(cx).focus_handle(cx);
        let current = if explorer.contains_focused(window, cx) {
            Some(Region::Explorer)
        } else if query
            .as_ref()
            .is_some_and(|pane| pane.read(cx).results_hold_focus(window, cx))
        {
            Some(Region::Results)
        } else if self.area_holds_focus(window, cx) {
            Some(Region::Tab)
        } else {
            None
        };
        let at = current.and_then(|current| regions.iter().position(|region| *region == current));
        let next = match (at, forward) {
            (None, true) => 0,
            (None, false) => regions.len() - 1,
            (Some(at), true) => (at + 1) % regions.len(),
            (Some(at), false) => (at + regions.len() - 1) % regions.len(),
        };
        match regions[next] {
            Region::Explorer => self
                .explorer
                .update(cx, |explorer, cx| explorer.take_focus(window, cx)),
            Region::Tab => {
                if let Some(pane) = self.active_pane() {
                    self.focus_active_tab(pane, window, cx);
                }
            }
            Region::Results => {
                if let Some(query) = query {
                    query.update(cx, |pane, cx| pane.focus_results(window, cx));
                }
            }
        }
        cx.notify();
    }

    /// Where the keyboard is, as a region a dialog can give it back to.
    pub(super) fn focus_origin_now(&self, window: &Window, cx: &App) -> FocusOrigin {
        if self
            .explorer
            .read(cx)
            .focus_handle(cx)
            .contains_focused(window, cx)
        {
            FocusOrigin::Explorer
        } else if self.area_holds_focus(window, cx) {
            FocusOrigin::Area
        } else {
            FocusOrigin::Shell
        }
    }

    /// Gives the keyboard back to where it was before the dialog that has just
    /// closed took it: the explorer, the active tab — whatever it is showing
    /// now — or the shell.
    pub(super) fn restore_focus(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        match std::mem::take(&mut self.focus_origin) {
            FocusOrigin::Explorer if self.explorer_showing() => {
                self.explorer
                    .update(cx, |explorer, cx| explorer.take_focus(window, cx));
            }
            FocusOrigin::Area => match self.active_pane() {
                Some(pane) => self.focus_active_tab(pane, window, cx),
                None => self.focus_shell(window, cx),
            },
            _ => self.focus_shell(window, cx),
        }
        cx.notify();
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
    /// Two callers. The start-up update check announces itself only into an
    /// empty window — it is the one dialog nobody asked for, and it must not
    /// land on top of a half-typed connection form or a running backup. And
    /// [`Workspace::shortcuts_held`], below.
    pub(super) fn dialog_open(&self, cx: &App) -> bool {
        self.rename_tab.is_some()
            || self.confirm.is_some()
            || self.close_question.is_some()
            || self.about.read(cx).is_open()
            || self.connect.read(cx).is_open()
            || self.settings.read(cx).is_open()
            || self.extract.read(cx).is_open()
            || self.transfer.read(cx).is_open()
            || self.backup.read(cx).is_open()
            || self.update.read(cx).is_open()
    }

    /// Whether the window's own shortcuts are held back, because something
    /// modal is up in front of the panes they act on.
    ///
    /// Every dialog is a child of the workspace root, so a chord pressed inside
    /// one still walks up to the `Workspace` key context and matches there:
    /// `Ctrl+N` inside the connection dialog would close it and open it again
    /// over the first profile, `Ctrl+S` would save the query tab behind it, and
    /// `Alt+W` would close the pane under it. A dialog that wants one of those
    /// chords for itself binds it in its own key context, which gpui prefers
    /// for being deeper. The sheets a pane puts up over itself — an apply
    /// preview, a discard question, a value viewer — hold them back too, but
    /// only the active pane's: a sheet in a pane the marker is not on covers
    /// nothing the shortcuts would act on.
    pub(super) fn shortcuts_held(&self, cx: &App) -> bool {
        self.dialog_open(cx)
            || self
                .work_area()
                .and_then(|area| area.panes.get(area.active()))
                .and_then(Pane::active)
                .is_some_and(|item| item.modal_open(cx))
    }
}
