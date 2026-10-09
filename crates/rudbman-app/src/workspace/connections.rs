//! Connections.

use super::*;

impl Workspace {
    /// The connection the tab strip and the status bar are showing.
    pub(super) fn active_connection(&self) -> Option<&Connection> {
        self.connections.get(self.active_connection)
    }

    /// The work area on screen: the active connection's.
    ///
    /// `None` only while no connection is open at all. Every pane command, the
    /// status bar and the renderer go through this, which is what makes the
    /// connection tab select the whole window below it — and what makes a pane
    /// command over an empty window a no-op rather than an operation on a tree
    /// nobody can see.
    pub(super) fn work_area(&self) -> Option<&WorkArea> {
        self.active_connection().map(|open| &open.work)
    }

    /// The work area on screen, mutably.
    pub(super) fn work_area_mut(&mut self) -> Option<&mut WorkArea> {
        let index = self.active_connection;
        self.connections.get_mut(index).map(|open| &mut open.work)
    }

    /// Opens a session for `profile` in a tab of its own.
    ///
    /// The tab appears immediately, in [`ConnectionState::Connecting`]: the
    /// attempt can take as long as the network does, and a window that showed
    /// nothing until it finished would look frozen. Everything that blocks
    /// happens on a background task, because [`connection::connect`] opens an
    /// SSH channel and a JDBC connection and both of those wait on a socket.
    ///
    /// The new tab comes to the front, which takes the work area that was
    /// showing off screen — so the keyboard has to be asked about before
    /// anything moves; see [`Workspace::follow_work_area`].
    pub(super) fn open_connection(
        &mut self,
        profile: ConnectionProfile,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let held = self.area_holds_focus(window, cx);
        let drivers = match DriverStore::load() {
            Ok(drivers) => drivers,
            Err(error) => {
                log::error!("could not read drivers.json: {error:#}");
                DriverStore::default()
            }
        };
        let Some(driver) = drivers.get(&profile.driver_id).cloned() else {
            self.connections.push(Connection {
                id: next_connection_id(),
                state: ConnectionState::Failed(ts!(
                    "connect.no_driver",
                    driver = profile.driver_id.clone()
                )),
                profile,
                work: WorkArea::new(),
            });
            self.active_connection = self.connections.len() - 1;
            self.sync_explorer_root(self.active_connection, cx);
            self.sync_visible_root(cx);
            self.follow_work_area(held, window, cx);
            cx.notify();
            return;
        };

        let index = self.connections.len();
        let settings = app_settings::current(cx);
        let attempt = profile.clone();
        // Read here, on the UI thread, and moved straight into the task: the
        // secret exists as a value for the length of one connection attempt and
        // is written to nothing.
        let credentials = connection::Credentials::read(&profile);
        let opening = cx.background_spawn(async move {
            connection::connect(&attempt, &driver, &credentials, &settings)
        });

        let task = cx.spawn(async move |workspace, cx| {
            let outcome = opening.await;
            workspace
                .update(cx, |workspace, cx| workspace.connected(index, outcome, cx))
                .ok();
        });

        self.connections.push(Connection {
            id: next_connection_id(),
            profile,
            state: ConnectionState::Connecting { _task: task },
            work: WorkArea::new(),
        });
        self.active_connection = index;
        self.sync_explorer_root(index, cx);
        self.sync_visible_root(cx);
        self.follow_work_area(held, window, cx);
        cx.notify();
    }

    /// Opens a session on the saved profile `id`, with nothing asked first.
    ///
    /// What clicking a row of the welcome screen's list does. The profile has
    /// been saved already, so there is nothing to fill in: putting the dialog up
    /// over a profile the user has just picked would be a form to dismiss
    /// between them and the database.
    ///
    /// Nothing is checked ahead of the attempt either — not the driver, not the
    /// password. A profile whose driver has gone shows that in its tab, and a
    /// profile with no secret in the keychain is one the database is asked
    /// about: trust authentication is a perfectly ordinary way to be let in, and
    /// a dialog demanding a password first would lock those users out of their
    /// own connection.
    pub(super) fn open_profile(&mut self, id: Uuid, window: &mut Window, cx: &mut Context<Self>) {
        let Some(profile) = self.profiles.get(id).cloned() else {
            // The list is a snapshot; the file may have lost the profile since.
            return;
        };
        self.open_connection(profile, window, cx);
        // The tab takes the welcome screen off screen, and with it the very row
        // that was clicked — which is holding the keyboard, because the rows are
        // in the tab ring. Left there it would swallow every action from then
        // on; see [`Workspace::reclaim_focus`]. Nothing else can have it at this
        // point: there was no work area and no sidebar to hold it.
        self.focus_shell(window, cx);
    }

    /// The session of one connection, when it is open.
    pub(super) fn session_of(&self, connection: ConnectionId) -> Option<connection::SessionHandle> {
        self.connections
            .iter()
            .find(|open| open.id == connection)
            .and_then(|open| match &open.state {
                ConnectionState::Open(connected) => Some(connected.handle()),
                _ => None,
            })
    }

    /// Fetches the children of one explorer node.
    ///
    /// The session's own worker thread serialises this against everything else
    /// that connection is doing, which is the reason the tree draws a
    /// placeholder rather than pretending to be instant: a schema opened while a
    /// statement is running waits for it.
    pub(super) fn load_node(&mut self, node: NodeId, cx: &mut Context<Self>) {
        let Some(session) = self.session_of(node.connection()) else {
            // The tab was closed, or its session died, between the tree asking
            // and this running. The node keeps its placeholder; there is nobody
            // to ask.
            let explorer = self.explorer.clone();
            let message = ts!("explorer.disconnected");
            cx.defer(move |cx| {
                explorer.update(cx, |explorer, cx| {
                    explorer.deliver(node, Err(message), cx);
                });
            });
            return;
        };

        let fetch = cx.background_spawn({
            let node = node.clone();
            async move { explorer::load_children(session.session(), &node) }
        });
        let explorer = self.explorer.clone();
        cx.spawn(async move |_workspace, cx| {
            let outcome = fetch.await.map_err(SharedString::from);
            explorer.update(cx, |explorer, cx| explorer.deliver(node, outcome, cx));
        })
        .detach();
    }

    /// Opens a detail panel for `target`, or brings the open one to the front.
    ///
    /// Activating the same object twice is a navigation, not a request for a
    /// second copy: the panel that is already open shows exactly what a new one
    /// would, and a strip filling up with duplicates of one table is nobody's
    /// idea of a workspace. The search covers every pane of the work area on
    /// screen, so activating an object from one half of a split jumps to the
    /// pane already showing it — and it need cover no more than that, because
    /// the explorer only offers objects of the connection whose area this is.
    pub(super) fn open_object(
        &mut self,
        target: ObjectTarget,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some((pane, index)) = self.detail_tab(&target, cx) {
            self.activate_tab(pane, index, window, cx);
            return;
        }

        let panel = cx.new(|cx| TableDetail::new(target, cx));
        // Subscribe first, *then* ask. A panel that requested its own metadata
        // from its constructor would emit into an empty room and sit at
        // "loading…" for ever; see `TableDetail::new`.
        cx.subscribe_in(&panel, window, |workspace, panel, event, window, cx| {
            match event {
                TableDetailEvent::Load(target) => {
                    workspace.load_details(panel.clone(), (**target).clone(), cx);
                }
                // Through the same gate the explorer's own row goes through, so
                // the rows of a table whose columns are already open land on
                // the data tab that is already open for it.
                TableDetailEvent::ViewData(target) => {
                    workspace.open_data((**target).clone(), window, cx);
                }
                // Through the same gate as the explorer's own row, for the
                // reason the one above it is: a table whose columns are already
                // open lands on the structure tab already open for it.
                TableDetailEvent::ViewStructure(target) => {
                    workspace.open_structure((**target).clone(), window, cx);
                }
            }
        })
        .detach();
        panel.update(cx, |panel, cx| panel.refresh(cx));
        self.append_tab(PaneItem::TableDetail(panel), window, cx);
    }

    /// Marks a connection dead because the tunnel under it closed, and lets its
    /// editors go of the session.
    ///
    /// A query pane holds a [`connection::SessionHandle`], which is what keeps
    /// the session — and the tunnel under it — alive while a fetch is out. That
    /// is right for a fetch and wrong for a session that has ended: without the
    /// detach below, a dead connection's editors would each hold a handle to a
    /// session nobody can use, and §9.3's rule that a tunnel dies with its
    /// session would hold only until someone had opened an editor.
    ///
    /// The kinds of tab part company here, because what the user would lose
    /// differs. A query pane holds the statement they typed, so its tab stays
    /// and is detached: the SQL and the rows already fetched remain readable and
    /// copyable, and every path that would talk to the database refuses. A data
    /// pane is detached for the same reason — it holds rows and a cursor, and
    /// the rows are worth keeping on screen. A detail panel shows what the
    /// database already said and nothing the user wrote, so it is simply left
    /// alone — a refresh of one finds no session and says so, the same way the
    /// explorer does.
    ///
    /// Nothing is closed and no pane is removed. The connection's tab and its
    /// whole work area stay reachable until the user closes the tab themselves;
    /// a session dying under them must not rearrange their window.
    pub(super) fn tunnel_died(&mut self, index: usize, reason: String, cx: &mut Context<Self>) {
        let Some(connection) = self.connections.get(index) else {
            return;
        };
        if !matches!(connection.state, ConnectionState::Open(_)) {
            return;
        }
        log::warn!(
            "the tunnel under {} closed: {reason}",
            connection.profile.name
        );

        // Before the state is replaced, so that the handles the editors hold are
        // gone by the time the `Connected` below is dropped and the session can
        // actually close rather than outliving its own tab.
        for pane in connection.work.queries() {
            pane.update(cx, |pane, cx| pane.detach(cx));
        }
        for panel in connection.work.data_panes() {
            panel.update(cx, |panel, cx| panel.detach(cx));
        }
        for panel in connection.work.struct_panes() {
            panel.update(cx, |panel, cx| panel.detach(cx));
        }

        let Some(connection) = self.connections.get_mut(index) else {
            return;
        };
        // Replacing the state drops the `Connected`, which closes the session
        // and releases the lease in that order.
        connection.state = ConnectionState::Dead(ts!("statusbar.tunnel_lost", reason = reason));
        self.sync_explorer_root(index, cx);
        cx.notify();
    }

    /// Closes one connection tab, ending its session and discarding everything
    /// it had open.
    ///
    /// The tab's whole work area goes with it. That is the designed cleanup
    /// path: dropping the area drops every pane, every pane drops its tabs, and
    /// every query tab drops the [`connection::SessionHandle`] and the cursors
    /// it was holding — which is what lets the session below actually close
    /// rather than being kept alive by an editor nobody can run anything in
    /// (architecture document, §9.3). Panes and splits of *other* connections
    /// are untouched, because they were never in this tree.
    ///
    /// Closing the tab that is on top brings another area on screen, so the
    /// keyboard has to be asked about before anything is removed; see
    /// [`Workspace::follow_work_area`].
    pub(super) fn close_connection(
        &mut self,
        index: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if index >= self.connections.len() {
            return;
        }
        // Only the area on screen can be holding the keyboard, so only closing
        // that one moves it.
        let held = index == self.active_connection && self.area_holds_focus(window, cx);
        // Asked before the tab goes, for the same reason: it reads the frame
        // that still has the sidebar in it.
        let sidebar = self.explorer_showing();

        let connection = self.connections.remove(index);
        let closed = connection.id;
        self.explorer.update(cx, |explorer, cx| {
            explorer.update_source(cx, |source| source.remove_root(closed));
        });
        // Explicitly, and before the session is handed over below: the order is
        // the point.
        drop(connection.work);
        if let ConnectionState::Open(connected) = connection.state {
            // CLOSE_SESSION and then the tunnel, in that order, and off the UI
            // thread because both of them talk to a socket.
            cx.background_spawn(async move {
                if let Err(error) = connected.close() {
                    log::warn!("closing the session failed: {error}");
                }
            })
            .detach();
        }

        // Closing a tab to the left of the active one shifts it along; the tab
        // on top must stay the same one, or the work area would change under a
        // user who closed something else entirely. Closing the last tab of all
        // clamps to whatever is left, and to zero when nothing is.
        if index < self.active_connection {
            self.active_connection -= 1;
        }
        self.active_connection = self
            .active_connection
            .min(self.connections.len().saturating_sub(1));
        self.sync_visible_root(cx);
        self.follow_work_area(held, window, cx);
        // Closing the last tab takes the sidebar off screen with it — see
        // [`Workspace::explorer_showing`] — which is the same focus hazard as
        // hiding it by hand: a focus left on the tree would swallow every action
        // from then on, the `Ctrl+B` that would bring it back included.
        if sidebar && !self.explorer_showing() {
            let explorer = self.explorer.read(cx).focus_handle(cx);
            self.reclaim_focus(&explorer, window, cx);
        }
        self.drop_stale_confirm(cx);
        cx.notify();
    }

    /// Brings one connection tab to the front, and its work area with it.
    ///
    /// The outgoing area stops being rendered entirely, editors and all, so this
    /// is the same focus hazard as hiding the sidebar; see
    /// [`Workspace::follow_work_area`] and [`Workspace::reclaim_focus`].
    pub(super) fn select_connection(
        &mut self,
        index: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if index >= self.connections.len() || self.active_connection == index {
            return;
        }
        let held = self.area_holds_focus(window, cx);
        self.active_connection = index;
        self.sync_visible_root(cx);
        self.follow_work_area(held, window, cx);
        cx.notify();
    }

    /// Whether the work area on screen is holding the keyboard.
    ///
    /// Asked *before* a switch, because it reads the last drawn frame — the one
    /// that still holds the outgoing area.
    pub(super) fn area_holds_focus(&self, window: &Window, cx: &App) -> bool {
        self.active_pane()
            .is_some_and(|pane| self.pane_holds_focus(pane, window, cx))
    }

    /// Moves the sidebar's edge to wherever the pointer has dragged it.
    ///
    /// The width is persisted on release rather than per move: a drag is
    /// hundreds of events and `settings.json` is written once when the window
    /// closes, so writing the global on every one of them would be the only
    /// thing in the frame doing work.
    pub(super) fn drag_explorer(
        &mut self,
        event: &DragMoveEvent<DraggedExplorer>,
        cx: &mut Context<Self>,
    ) {
        let width = f32::from(event.event.position.x - event.bounds.left());
        if !width.is_finite() {
            return;
        }
        let width = width.clamp(MIN_EXPLORER_WIDTH, MAX_EXPLORER_WIDTH);
        if (self.explorer_width - width).abs() > f32::EPSILON {
            self.explorer_width = width;
            cx.notify();
        }
    }
}
