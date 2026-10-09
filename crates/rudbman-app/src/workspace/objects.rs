//! Objects.

use super::*;

impl Workspace {
    /// Opens `target`'s rows in a data pane, or brings the open one to the
    /// front.
    ///
    /// The same navigation rule a detail panel follows, and kept separate from
    /// it: the rows and the columns of one table are two tabs a user may
    /// perfectly well want at once, so each deduplicates against its own kind
    /// (architecture document, §7.9).
    ///
    /// Nothing happens without a live session. Unlike a detail panel, whose
    /// whole content is one fetch that can fail visibly, a data pane over a
    /// dead connection could not even name its columns.
    pub(super) fn open_data(
        &mut self,
        target: ObjectTarget,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some((pane, index)) = self.data_tab(&target, cx) {
            self.activate_tab(pane, index, window, cx);
            return;
        }
        let Some(session) = self.session_of(target.connection) else {
            return;
        };
        let Some(open) = self
            .connections
            .iter()
            .find(|open| open.id == target.connection)
        else {
            return;
        };
        let profile = open.profile.clone();
        let dialect = Self::dialect_of(&profile);
        let settings = app_settings::current(cx);
        let connection = open.id;
        // What `SESSION_INFO` said when this connection opened, rather than a
        // round trip of the pane's own. A driver that would not answer is taken
        // as having transactions: see `DataPane::with_transactions`.
        let transactional = match &open.state {
            ConnectionState::Open(connected) => connected.info.supports_transactions != Some(false),
            _ => true,
        };

        let panel = cx.new(|cx| {
            DataPane::new(
                session, connection, target, &profile, &dialect, &settings, cx,
            )
            .with_transactions(transactional)
        });
        // The pane owns its session, so nothing has to be subscribed for it to
        // load; it is asked *after* the tab exists, for the reason a detail
        // panel is asked after its subscription is registered — a fetch started
        // from inside `cx.new` would be racing the tab that is meant to show
        // it.
        panel.update(cx, |panel, cx| panel.refresh(window, cx));
        self.append_tab(PaneItem::TableData(panel.clone()), window, cx);
        // Opened with a keyboard gesture as often as with the mouse, and the
        // arrows and the copy chord are the grid's own.
        panel.update(cx, |panel, cx| panel.take_focus(window, cx));
    }

    /// Opens `target`'s structure in a pane of its own, or brings the open one
    /// to the front.
    ///
    /// The navigation rule the other two per-object tabs follow, and a third
    /// deduplication for the reason there are three: the shape of a table, its
    /// rows and its description are three things a user may want side by side
    /// (§7.10).
    ///
    /// Nothing happens without a live session, exactly as for a data pane: the
    /// whole content is a `DESCRIBE`, and there is nothing to edit the shape of
    /// over a dead connection.
    pub(super) fn open_structure(
        &mut self,
        target: ObjectTarget,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some((pane, index)) = self.struct_tab(&target, cx) {
            self.activate_tab(pane, index, window, cx);
            return;
        }
        let Some(session) = self.session_of(target.connection) else {
            return;
        };
        let Some(open) = self
            .connections
            .iter()
            .find(|open| open.id == target.connection)
        else {
            return;
        };
        let profile = open.profile.clone();
        let dialect = Self::dialect_of(&profile);
        let connection = open.id;

        let panel =
            cx.new(|cx| StructPane::new(session, connection, target, &profile, &dialect, cx));
        // Subscribed to before the first read, the way a detail panel is: the
        // pane redraws its own body, but the shell has to redraw around it —
        // the tab strip's title and its dot are the shell's — and an
        // observation registered after the load would miss the frame the
        // structure arrived in.
        cx.observe(&panel, |_workspace, _panel, cx| cx.notify())
            .detach();
        panel.update(cx, |panel, cx| panel.refresh(cx));
        self.append_tab(PaneItem::TableStruct(panel.clone()), window, cx);
        panel.update(cx, |panel, cx| panel.take_focus(window, cx));
    }

    /// Opens a structure pane over a table that does not exist yet, in `scope`.
    ///
    /// The create half of §7.10, and deliberately **not** deduplicated the way
    /// [`Workspace::open_structure`] is: a pane in that mode names no table —
    /// its name is a field somebody is typing into — so there is nothing to
    /// deduplicate on, and two of them are two tables being drafted rather than
    /// one tab opened twice. The moment one of them becomes a real table it
    /// takes that table's name, and the ordinary rule applies to it again.
    ///
    /// Nothing happens without a live session, for [`Workspace::open_structure`]'s
    /// reason turned around: the statement has to be sent somewhere.
    pub(super) fn open_new_table(
        &mut self,
        connection: ConnectionId,
        scope: explorer::Scope,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(session) = self.session_of(connection) else {
            return;
        };
        let Some(open) = self.connections.iter().find(|open| open.id == connection) else {
            return;
        };
        let profile = open.profile.clone();
        let dialect = Self::dialect_of(&profile);
        let target = ObjectTarget {
            connection,
            catalog: scope.catalog,
            schema: scope.schema,
            folder: Folder::Tables,
            name: String::new(),
        };

        let panel =
            cx.new(|cx| StructPane::creating(session, connection, target, &profile, &dialect, cx));
        // Observed for the reason `open_structure`'s is: the tab strip's title
        // and its dot are the shell's to redraw.
        cx.observe(&panel, |_workspace, _panel, cx| cx.notify())
            .detach();
        // Not asked to load: a table being created *is* loaded — its current
        // shape is the empty one — and there is no server to ask for it.
        self.append_tab(PaneItem::TableStruct(panel.clone()), window, cx);
        // The pane puts the keyboard on its name field, which §7.10 opens a new
        // table with.
        panel.update(cx, |panel, cx| panel.take_focus(window, cx));
    }

    /// Draws the ERD of one scope, or brings the open one to the front.
    ///
    /// The same navigation rule the detail panels follow: a second diagram of
    /// one scope would show exactly what the first one does, and — unlike a
    /// second query pane — there is nothing of the user's in it to keep apart.
    ///
    /// Nothing happens without a session behind the scope. The panel's whole
    /// content is a fetch, so a diagram over a dead connection would be a tab
    /// that can only ever say "the connection is closed".
    pub(super) fn open_erd(
        &mut self,
        target: ErdTarget,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some((pane, index)) = self.erd_tab(&target, cx) {
            self.activate_tab(pane, index, window, cx);
            return;
        }
        if self.session_of(target.connection).is_none() {
            return;
        }

        let panel = cx.new(|cx| ErdPane::new(target, cx));
        // Subscribe first, *then* ask, for the reason `ErdPane::new` does not
        // emit: a request from a constructor reaches nobody.
        cx.subscribe_in(&panel, window, |workspace, panel, event, window, cx| {
            match event {
                ErdPaneEvent::Load(target) => {
                    workspace.load_erd(panel.clone(), (**target).clone(), cx);
                }
                ErdPaneEvent::LayoutChanged(target) => {
                    let positions = panel.read(cx).positions(cx);
                    workspace.save_erd_layout(target, positions, cx);
                }
                // Through the same gate the explorer's own double click goes
                // through, so a table reached from a diagram lands on the tab
                // that is already open for it.
                ErdPaneEvent::OpenTable(target) => {
                    workspace.open_object((**target).clone(), window, cx);
                }
            }
        })
        .detach();
        panel.update(cx, |panel, cx| panel.refresh(cx));
        self.append_tab(PaneItem::Erd(panel.clone()), window, cx);
        // A diagram opened with the keyboard should answer the keyboard: the
        // zoom and auto-arrange chords are the canvas's, and until the fetch
        // comes back the panel's own root stands in for it.
        panel.update(cx, |panel, cx| panel.take_focus(window, cx));
    }

    /// Fetches one diagram: the catalogue, and the arrangement it was left in.
    ///
    /// Both on one background task. They are wanted at the same moment and
    /// handing them to [`ErdPane::deliver`] separately would draw the grid
    /// layout for a frame and then jump.
    ///
    /// A layout file that cannot be read is logged and treated as absent: the
    /// diagram is worth drawing in its default arrangement, and a schema the
    /// user can see beats an error about a file they did not write.
    pub(super) fn load_erd(
        &mut self,
        panel: Entity<ErdPane>,
        target: ErdTarget,
        cx: &mut Context<Self>,
    ) {
        let Some(session) = self.session_of(target.connection) else {
            let message = ts!("explorer.disconnected");
            cx.defer(move |cx| {
                panel.update(cx, |panel, cx| panel.deliver(Err(message), cx));
            });
            return;
        };
        let profile = self.profile_of(target.connection);

        let fetch = cx.background_spawn(async move {
            let model = erd_pane::load_model(session.session(), &target)?;
            let saved = profile
                .map(|profile| match ErdLayouts::load(profile) {
                    Ok(layouts) => layouts.positions(&target.scope),
                    Err(error) => {
                        log::warn!("the saved ERD layout could not be read: {error:#}");
                        HashMap::new()
                    }
                })
                .unwrap_or_default();
            Ok::<ErdDiagram, String>(ErdDiagram { model, saved })
        });
        cx.spawn(async move |_workspace, cx| {
            let outcome = fetch.await.map_err(SharedString::from);
            panel.update(cx, |panel, cx| panel.deliver(outcome, cx));
        })
        .detach();
    }

    /// Writes one scope's box positions to `erd/<profile-uuid>.json`.
    ///
    /// Once per gesture, because [`ErdPaneEvent::LayoutChanged`] arrives once
    /// per gesture — the same discipline the sidebar's width follows, and for
    /// the same reason: a file written per frame would be the only thing in the
    /// frame doing work.
    ///
    /// Read, edit and write happen together on one background task. Two
    /// gestures finishing at once therefore settle as last writer wins, which
    /// for one user dragging one box is the only outcome there is.
    pub(super) fn save_erd_layout(
        &mut self,
        target: &ErdTarget,
        positions: HashMap<String, (f32, f32)>,
        cx: &mut Context<Self>,
    ) {
        let Some(profile) = self.profile_of(target.connection) else {
            return;
        };
        let scope = target.scope.clone();
        cx.background_spawn(async move {
            let mut layouts = ErdLayouts::load(profile).unwrap_or_else(|error| {
                log::warn!("the saved ERD layout could not be read: {error:#}");
                ErdLayouts::default()
            });
            layouts.set_positions(&scope, positions);
            if let Err(error) = layouts.save(profile) {
                log::error!("the ERD layout could not be saved: {error:#}");
            }
        })
        .detach();
    }

    /// The profile one open connection was created from.
    ///
    /// The layout file is keyed by it rather than by [`ConnectionId`], which
    /// lives only as long as the tab does.
    pub(super) fn profile_of(&self, connection: ConnectionId) -> Option<uuid::Uuid> {
        self.connections
            .iter()
            .find(|open| open.id == connection)
            .map(|open| open.profile.id)
    }

    /// Opens a query pane in the active pane, with `sql` already in it.
    ///
    /// Nothing happens without a live session: a SQL editor with no connection
    /// behind it can be typed into and never run, which is worse than a pane
    /// that says there is nothing to connect to.
    pub(super) fn open_query(&mut self, sql: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(open) = self.active_connection() else {
            return;
        };
        let ConnectionState::Open(connected) = &open.state else {
            return;
        };
        let session = connected.handle();
        let id = open.id;
        let profile = open.profile.clone();
        let dialect = Self::dialect_of(&profile);
        let settings = app_settings::current(cx);

        // What `SESSION_INFO` said when this connection opened, rather than a
        // round trip of the pane's own: an editable result's apply runs in one
        // transaction, and a product with none has to be told about (§7.9).
        let transactional = connected.info.supports_transactions != Some(false);
        let pane = cx.new(|cx| {
            QueryPane::new(session, id, &profile, &dialect, &settings, sql, window, cx)
                .with_transactions(transactional)
        });
        // The elapsed clock and the row count live in the pane; the status bar
        // that draws them is here, so the shell redraws whenever the pane does.
        cx.observe(&pane, |_workspace, _pane, cx| cx.notify())
            .detach();
        cx.subscribe(&pane, |workspace, pane, event, cx| {
            let QueryPaneEvent::ConfirmWrites(request) = event;
            workspace.confirm = Some(PendingConfirm {
                pane: pane.clone(),
                request: Box::new(ConfirmRequest {
                    count: request.count,
                    preview: request.preview.clone(),
                }),
            });
            cx.notify();
        })
        .detach();

        // Numbered within this connection's own area, which the checks above
        // have already established exists.
        let Some(area) = self.work_area_mut() else {
            return;
        };
        let number = area.next_query;
        area.next_query += 1;
        self.append_tab(
            PaneItem::Query {
                pane: pane.clone(),
                number,
            },
            window,
            cx,
        );
        pane.update(cx, |pane, cx| pane.focus_editor(window, cx));
    }

    /// The dialect one profile's statements are written for.
    ///
    /// The *driver's*, not the profile's: a profile names a driver and a driver
    /// names a dialect. Read from `drivers.json` each time rather than cached,
    /// because the driver manager can rewrite that file while the window is
    /// open, and a driver that has gone falls back to the generic profile
    /// rather than to nothing.
    pub(super) fn dialect_of(profile: &ConnectionProfile) -> String {
        DriverStore::load()
            .ok()
            .and_then(|store| {
                store
                    .get(&profile.driver_id)
                    .map(|driver| driver.dialect.clone())
            })
            .unwrap_or_else(|| "generic".to_string())
    }

    /// Opens a query pane over one explorer object, pre-filled with a `SELECT`.
    ///
    /// The name is written by [`builder_sql::table_ref`], which is also what
    /// the query builder's `FROM` goes through: a name that needs quoting gets
    /// it, a catalogue is not dropped on a product that has no schemas, and an
    /// ordinary name in the catalogue's own case comes out exactly as before.
    pub(super) fn open_query_for(
        &mut self,
        target: &ObjectTarget,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let dialect = rudbman_sql::Dialect::from_id(&self.active_dialect());
        let name = builder_sql::table_ref(
            &dialect,
            target.catalog.as_deref(),
            target.schema.as_deref(),
            &target.name,
        );
        self.open_query(&format!("SELECT * FROM {name}"), window, cx);
    }

    /// The dialect of the connection whose tab is showing.
    pub(super) fn active_dialect(&self) -> String {
        self.active_connection()
            .map(|open| Self::dialect_of(&open.profile))
            .unwrap_or_else(|| "generic".to_string())
    }

    /// The query pane the status bar and the run commands act on: the active
    /// tab of the active pane of the work area on screen, when that tab is a
    /// query.
    ///
    /// `None` with no connection open, because then there is no work area to
    /// have an active pane at all.
    pub(super) fn active_query(&self) -> Option<&Entity<QueryPane>> {
        let area = self.work_area()?;
        match area.panes.get(area.active())?.active()? {
            PaneItem::Query { pane, .. } => Some(pane),
            PaneItem::TableDetail(_)
            | PaneItem::Erd(_)
            | PaneItem::TableData(_)
            | PaneItem::TableStruct(_)
            | PaneItem::QueryBuilder { .. } => None,
        }
    }

    /// Opens an empty query builder in the active pane and hands it back.
    ///
    /// Gated on a live session for the reason a query pane is: the builder's
    /// tables come from `DESCRIBE`, and a canvas over a dead connection could
    /// never have anything put on it.
    pub(super) fn open_builder(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<Entity<BuilderPane>> {
        let open = self.active_connection()?;
        if !matches!(open.state, ConnectionState::Open(_)) {
            return None;
        }
        let connection = open.id;
        let dialect = Self::dialect_of(&open.profile);

        let panel = cx.new(|cx| BuilderPane::new(connection, &dialect, cx));
        // Through `open_query`, which is the one gate every new query pane
        // comes through: running, cancelling and the write confirmation are its
        // pipeline, and the builder has no business owning a second one.
        cx.subscribe_in(
            &panel,
            window,
            |workspace, panel, event, window, cx| match event {
                BuilderPaneEvent::OpenSql(sql) => workspace.open_query(sql, window, cx),
                // On the panel the pointer was released over, which is the one
                // that emitted this — not whichever builder the action would have
                // picked. Dropping on a builder is aiming at it.
                BuilderPaneEvent::TableDropped(target) => {
                    workspace.add_to_builder_on(panel.clone(), target.clone(), cx);
                }
            },
        )
        .detach();

        let area = self.work_area_mut()?;
        let number = area.next_builder;
        area.next_builder += 1;
        self.append_tab(
            PaneItem::QueryBuilder {
                pane: panel.clone(),
                number,
            },
            window,
            cx,
        );
        // A builder opened with the keyboard should answer the keyboard: the
        // zoom chords are the canvas's, and until a table arrives the panel's
        // own root stands in for it.
        panel.update(cx, |panel, cx| panel.take_focus(window, cx));
        Some(panel)
    }

    /// Puts one explorer object on a query builder, opening one if there is
    /// none.
    ///
    /// The builder the object lands on is the one already in front when that is
    /// a builder, and otherwise the first one open anywhere in the work area —
    /// which is brought to the front so that the table can be seen arriving.
    /// A window with no builder at all gets one.
    ///
    /// Only ever this connection's own: a builder belongs to the connection its
    /// tab is under, and the explorer draws only the active connection's tree,
    /// so a target from anywhere else would be a table the statement could not
    /// name.
    pub(super) fn add_to_builder(
        &mut self,
        target: ObjectTarget,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // Checked before a tab is opened as well as inside
        // [`Workspace::add_to_builder_on`], so that a target from a connection
        // that is not the one on screen cannot leave an empty builder behind.
        if self.active_connection().map(|open| open.id) != Some(target.connection) {
            return;
        }
        let panel = match self.builder_tab() {
            Some((pane, index)) => {
                self.activate_tab(pane, index, window, cx);
                self.builder_at(pane, index)
            }
            None => self.open_builder(window, cx),
        };
        let Some(panel) = panel else {
            return;
        };
        self.add_to_builder_on(panel, target, cx);
    }

    /// Puts one explorer object on *this* builder.
    ///
    /// Split out from [`Workspace::add_to_builder`] because a drop has already
    /// chosen its builder — the one the pointer was over — and re-running the
    /// "which builder?" rule over it could move the table to a different tab
    /// than the one the user aimed at. What both paths share is everything
    /// after that choice: the same guards, the same one-round-trip column load
    /// and the same `add_table`.
    ///
    /// Only ever this connection's own: a builder belongs to the connection its
    /// tab is under, and the explorer draws only the active connection's tree,
    /// so a target from anywhere else would be a table the statement could not
    /// name.
    pub(super) fn add_to_builder_on(
        &mut self,
        panel: Entity<BuilderPane>,
        target: ObjectTarget,
        cx: &mut Context<Self>,
    ) {
        if self.active_connection().map(|open| open.id) != Some(target.connection) {
            return;
        }
        let Some(session) = self.session_of(target.connection) else {
            return;
        };

        let fetch = cx.background_spawn({
            let target = target.clone();
            async move { builder_pane::load_columns(session.session(), &target) }
        });
        cx.spawn(async move |_workspace, cx| {
            match fetch.await {
                Ok(columns) => {
                    panel.update(cx, |panel, cx| panel.add_table(&target, columns, cx));
                }
                // Nowhere to put this on screen: the builder has no message
                // strip, and a column list that could not be read is the same
                // failure the explorer already reports on the node itself.
                Err(error) => log::error!("the column list could not be read: {error}"),
            }
        })
        .detach();
    }

    /// Where a table added from the explorer should land.
    ///
    /// The tab in front when it is a builder, so that adding several tables in
    /// a row keeps putting them where the user is looking; otherwise the first
    /// builder in layout order.
    pub(super) fn builder_tab(&self) -> Option<(PaneId, usize)> {
        let area = self.work_area()?;
        let active = area.active();
        if let Some(pane) = area.panes.get(active)
            && matches!(pane.active(), Some(PaneItem::QueryBuilder { .. }))
        {
            return Some((active, pane.active_index()));
        }
        area.panes
            .leaves()
            .into_iter()
            .find_map(|(id, pane)| pane.first_builder().map(|index| (id, index)))
    }

    /// The builder in tab `index` of `pane`, when that tab is one.
    pub(super) fn builder_at(&self, pane: PaneId, index: usize) -> Option<Entity<BuilderPane>> {
        match self.work_area()?.panes.get(pane)?.get(index)? {
            PaneItem::QueryBuilder { pane, .. } => Some(pane.clone()),
            _ => None,
        }
    }

    /// Where `target` is already open, if it is: the pane and the tab in it.
    pub(super) fn detail_tab(&self, target: &ObjectTarget, cx: &App) -> Option<(PaneId, usize)> {
        self.work_area()?
            .panes
            .leaves()
            .into_iter()
            .find_map(|(id, pane)| pane.detail_of(target, cx).map(|index| (id, index)))
    }

    /// Where `target`'s rows are already open, if they are.
    pub(super) fn data_tab(&self, target: &ObjectTarget, cx: &App) -> Option<(PaneId, usize)> {
        self.work_area()?
            .panes
            .leaves()
            .into_iter()
            .find_map(|(id, pane)| pane.data_of(target, cx).map(|index| (id, index)))
    }

    /// Where `target`'s structure is already open, if it is.
    pub(super) fn struct_tab(&self, target: &ObjectTarget, cx: &App) -> Option<(PaneId, usize)> {
        self.work_area()?
            .panes
            .leaves()
            .into_iter()
            .find_map(|(id, pane)| pane.struct_of(target, cx).map(|index| (id, index)))
    }

    /// Where `target`'s diagram is already open, if it is.
    pub(super) fn erd_tab(&self, target: &ErdTarget, cx: &App) -> Option<(PaneId, usize)> {
        self.work_area()?
            .panes
            .leaves()
            .into_iter()
            .find_map(|(id, pane)| pane.erd_of(target, cx).map(|index| (id, index)))
    }

    /// Appends a tab to the active pane and brings it to the front.
    ///
    /// The tab that was showing stops being rendered the moment this returns,
    /// and it may be holding the keyboard; see [`Workspace::reclaim_focus`].
    /// Nothing is focused in its place here — the callers that open something
    /// typeable do that themselves — so the shell takes the keyboard back.
    pub(super) fn append_tab(
        &mut self,
        item: PaneItem,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(target) = self.active_pane() else {
            return;
        };
        if self.pane_holds_focus(target, window, cx) {
            self.focus_shell(window, cx);
        }
        if let Some(pane) = self
            .work_area_mut()
            .and_then(|area| area.panes.get_mut(target))
        {
            pane.push(item);
        }
        cx.notify();
    }

    /// Fetches everything one detail panel shows.
    pub(super) fn load_details(
        &mut self,
        panel: Entity<TableDetail>,
        target: ObjectTarget,
        cx: &mut Context<Self>,
    ) {
        let Some(session) = self.session_of(target.connection) else {
            let message = ts!("explorer.disconnected");
            cx.defer(move |cx| {
                panel.update(cx, |panel, cx| panel.deliver(Err(message), cx));
            });
            return;
        };

        let fetch = cx.background_spawn({
            let target = target.clone();
            async move { table_detail::load_details(session.session(), &target) }
        });
        cx.spawn(async move |_workspace, cx| {
            let outcome = fetch.await.map_err(SharedString::from);
            panel.update(cx, |panel, cx| panel.deliver(outcome, cx));
        })
        .detach();
    }
}
