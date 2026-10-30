//! Per-profile workspaces. Only drafts and navigation are persisted: sessions,
//! query results, staged database edits and transactions never cross a restart.

use super::*;
use crate::builder_pane::BuilderWorkspace;
use crate::explorer::Scope;
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::sync::Arc;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(super) struct SavedWorkspace {
    version: u32,
    layout: SavedNode,
    active_pane: usize,
    next_query: u64,
    next_builder: u64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum SavedNode {
    Pane {
        tabs: Vec<SavedTab>,
        active: usize,
    },
    Split {
        horizontal: bool,
        ratio: f32,
        first: Box<SavedNode>,
        second: Box<SavedNode>,
    },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(super) struct SavedTab {
    #[serde(default)]
    name: Option<String>,
    content: SavedContent,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum SavedContent {
    Query {
        number: u64,
        /// Shared with the pane's own copy; see [`QueryPane::workspace_sql`].
        /// Serialized as the plain string it always was.
        sql: Arc<str>,
        selection: std::ops::Range<usize>,
        editor_share: f32,
        /// The file the pane is bound to. Left out for a pane bound to none,
        /// so a workspace written before files were saved reads the same.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        file: Option<PathBuf>,
        /// Whether `sql` held changes that file did not.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        modified: bool,
        /// What that file looked like when the pane last read or wrote it.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        stamp: Option<FileStamp>,
    },
    Detail {
        target: SavedObject,
        tab: crate::table_detail::Tab,
    },
    Data {
        target: SavedObject,
        order: Option<(usize, String, bool)>,
    },
    Structure {
        target: SavedObject,
    },
    Erd {
        scope: Scope,
    },
    Builder {
        number: u64,
        state: BuilderWorkspace,
    },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct SavedObject {
    catalog: Option<String>,
    schema: Option<String>,
    folder: Folder,
    name: String,
}

impl SavedObject {
    fn of(target: &ObjectTarget) -> Self {
        Self {
            catalog: target.catalog.clone(),
            schema: target.schema.clone(),
            folder: target.folder,
            name: target.name.clone(),
        }
    }

    fn target(&self, connection: ConnectionId) -> ObjectTarget {
        ObjectTarget {
            connection,
            catalog: self.catalog.clone(),
            schema: self.schema.clone(),
            folder: self.folder,
            name: self.name.clone(),
        }
    }
}

impl WorkArea {
    pub(super) fn snapshot(&self, cx: &App) -> SavedWorkspace {
        SavedWorkspace {
            version: 1,
            layout: self.snapshot_node(self.panes.root(), cx),
            active_pane: self
                .panes
                .leaf_ids()
                .iter()
                .position(|id| *id == self.active())
                .unwrap_or(0),
            next_query: self.next_query,
            next_builder: self.next_builder,
        }
    }

    fn snapshot_node(&self, node: &PaneNode<Pane>, cx: &App) -> SavedNode {
        match node {
            PaneNode::Split {
                axis,
                ratio,
                first,
                second,
                ..
            } => SavedNode::Split {
                horizontal: *axis == Axis::Horizontal,
                ratio: *ratio,
                first: Box::new(self.snapshot_node(first, cx)),
                second: Box::new(self.snapshot_node(second, cx)),
            },
            PaneNode::Leaf { payload, .. } => SavedNode::Pane {
                active: payload.active_index(),
                tabs: payload
                    .items()
                    .iter()
                    .map(|item| self.snapshot_tab(item, cx))
                    .collect(),
            },
        }
    }
    pub(super) fn snapshot_tab(&self, item: &PaneItem, cx: &App) -> SavedTab {
        SavedTab {
            name: self.tab_names.get(&item.entity_id()).cloned(),
            content: match item {
                PaneItem::Query { pane, number } => {
                    let query = pane.read(cx);
                    // A path that is not UTF-8 cannot be written into the JSON
                    // at all, and would fail the whole workspace's save; that
                    // one tab comes back unbound instead.
                    let file = query.file().filter(|path| path.to_str().is_some());
                    SavedContent::Query {
                        number: *number,
                        sql: query.workspace_sql(cx),
                        selection: query.workspace_selection(cx),
                        editor_share: query.workspace_editor_share(),
                        file: file.map(Path::to_path_buf),
                        modified: file.is_some() && query.is_modified(cx),
                        stamp: file.and(query.file_stamp()),
                    }
                }
                PaneItem::TableDetail(pane) => SavedContent::Detail {
                    target: SavedObject::of(pane.read(cx).target()),
                    tab: pane.read(cx).tab(),
                },
                PaneItem::TableData(pane) => SavedContent::Data {
                    target: SavedObject::of(pane.read(cx).target()),
                    order: pane.read(cx).workspace_order(),
                },
                PaneItem::TableStruct(pane) => SavedContent::Structure {
                    target: SavedObject::of(pane.read(cx).target()),
                },
                PaneItem::Erd(pane) => SavedContent::Erd {
                    scope: pane.read(cx).target().scope.clone(),
                },
                PaneItem::QueryBuilder { pane, number } => SavedContent::Builder {
                    number: *number,
                    state: pane.read(cx).workspace_state(cx),
                },
            },
        }
    }
}

/// Disk writes are serialized. A final close waits for any in-flight autosave
/// before writing the newest snapshot, so an old write cannot win a race.
///
/// The queue and the writer are two locks. A write takes what is queued and
/// lets go of the queue before it serializes anything, so the UI thread queuing
/// the next snapshot never waits behind a file being written; the writer lock
/// is what keeps two writes in the order their snapshots were taken.
pub(super) struct SessionStorage {
    directory: Option<PathBuf>,
    pending: Arc<Mutex<HashMap<Uuid, SavedWorkspace>>>,
    writer: Arc<Mutex<()>>,
    blocked: HashSet<Uuid>,
}

impl Default for SessionStorage {
    fn default() -> Self {
        // Headless tests must never read or write the user's workspaces.
        let directory = if cfg!(test) {
            None
        } else {
            rudbman_core::paths::config_dir()
                .map(|dir| dir.join("workspaces"))
                .ok()
        };
        Self {
            directory,
            pending: Arc::default(),
            writer: Arc::default(),
            blocked: HashSet::new(),
        }
    }
}

impl SessionStorage {
    fn load(&mut self, profile: Uuid) -> Option<SavedWorkspace> {
        // A snapshot taken off the queue by a write still in flight is in
        // neither place until that write lands.
        let _writing = self.writer.lock();
        if let Some(state) = self.pending.lock().get(&profile) {
            return Some(state.clone());
        }
        let path = self.directory.as_ref()?.join(format!("{profile}.json"));
        let read = || -> anyhow::Result<Option<SavedWorkspace>> {
            let bytes = match std::fs::read(&path) {
                Ok(bytes) => bytes,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
                Err(error) => return Err(error.into()),
            };
            let state: SavedWorkspace =
                serde_json::from_slice(rudbman_core::paths::strip_bom(&bytes))?;
            anyhow::ensure!(
                state.version == 1,
                "unsupported workspace version {}",
                state.version
            );
            Ok(Some(state))
        };
        match read() {
            Ok(state) => state,
            Err(error) => {
                // Leave an unreadable or newer-version file intact.
                self.blocked.insert(profile);
                log::error!("could not restore workspace {}: {error:#}", path.display());
                None
            }
        }
    }

    fn queue(&self, profile: Uuid, state: SavedWorkspace) {
        if self.directory.is_some() && !self.blocked.contains(&profile) {
            self.pending.lock().insert(profile, state);
        }
    }

    fn flush(&self) {
        if let Some(directory) = &self.directory {
            write_pending(directory, &self.pending, &self.writer);
        }
    }
}

fn write_pending(
    directory: &std::path::Path,
    pending: &Mutex<HashMap<Uuid, SavedWorkspace>>,
    writer: &Mutex<()>,
) {
    let _writing = writer.lock();
    let states = std::mem::take(&mut *pending.lock());
    for (profile, state) in states {
        let path = directory.join(format!("{profile}.json"));
        let result = serde_json::to_vec_pretty(&state)
            .map_err(anyhow::Error::from)
            .and_then(|bytes| rudbman_core::paths::write_atomic(&path, &bytes));
        if let Err(error) = result {
            log::error!("could not save workspace {}: {error:#}", path.display());
            // Retry on the next save, including the final close — unless a
            // newer snapshot was queued meanwhile, which that save writes instead.
            pending.lock().entry(profile).or_insert(state);
        }
    }
}

impl Workspace {
    fn checkpoint_connection(&mut self, index: usize, cx: &App) {
        let Some(open) = self.connections.get_mut(index) else {
            return;
        };
        if !matches!(
            open.state,
            ConnectionState::Open(_) | ConnectionState::Dead(_)
        ) {
            return;
        }
        let snapshot = open.work.snapshot(cx);
        if open.work.checkpoint.as_ref() != Some(&snapshot) {
            self.sessions.queue(open.profile.id, snapshot.clone());
            open.work.checkpoint = Some(snapshot);
        }
    }

    fn checkpoint_workspaces(&mut self, cx: &App) {
        if self.sessions.directory.is_none() {
            return;
        }
        for index in 0..self.connections.len() {
            if index != self.active_connection {
                self.checkpoint_connection(index, cx);
            }
        }
        // With two live sessions on one profile, the foreground workspace wins
        // when both changed. Unchanged older sessions never overwrite it.
        self.checkpoint_connection(self.active_connection, cx);
    }

    pub(super) fn save_connection_workspace(&mut self, index: usize, cx: &App) {
        if self.sessions.directory.is_none() {
            return;
        }
        self.checkpoint_connection(index, cx);
        self.sessions.flush();
    }

    pub(super) fn flush_workspaces(&mut self, cx: &App) {
        self.checkpoint_workspaces(cx);
        self.sessions.flush();
    }

    pub(super) fn schedule_workspace_save(&mut self, cx: &mut Context<Self>) {
        if self.autosave.is_some() || self.sessions.directory.is_none() {
            return;
        }
        // Throttle instead of restarting a debounce on every frame: a running
        // query's elapsed clock must not postpone saving a SQL draft forever.
        self.autosave = Some(cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(std::time::Duration::from_millis(750))
                .await;
            this.update(cx, |workspace, cx| {
                workspace.checkpoint_workspaces(cx);
                workspace.warn_oversized_drafts(cx);
                let directory = workspace.sessions.directory.clone();
                let pending = workspace.sessions.pending.clone();
                let writer = workspace.sessions.writer.clone();
                // Serializing a snapshot is a pass over every draft in it, so
                // it happens here, off the UI thread, with the file write.
                cx.background_spawn(async move {
                    if let Some(directory) = directory {
                        write_pending(&directory, &pending, &writer);
                    }
                })
                .detach();
                workspace.autosave = None;
            })
            .ok();
        }));
    }

    /// Lets each query pane whose draft the last checkpoint found too large to
    /// keep say so; see [`QueryPane::warn_oversized_draft`].
    ///
    /// Not a pane whose file already holds its text: that one comes back from
    /// its file after a restart, so there is nothing for it to warn about until
    /// it is edited — and the warning, still owed, is said then.
    fn warn_oversized_drafts(&mut self, cx: &mut Context<Self>) {
        for open in &self.connections {
            for pane in open.work.queries() {
                let query = pane.read(cx);
                if query.owes_draft_warning() && (query.file().is_none() || query.is_modified(cx)) {
                    pane.update(cx, |pane, cx| pane.warn_oversized_draft(cx));
                }
            }
        }
    }

    /// Remembers tab `index` of `pane` (on screen) for "Reopen closed tab",
    /// when it is a query or a builder; see [`WorkArea::closed`].
    ///
    /// Taken through the same snapshot the workspace is saved from, so a tab
    /// comes back exactly as a restart would bring it back — draft, selection,
    /// name, file binding and unsaved mark.
    pub(super) fn remember_closed(&mut self, pane: PaneId, index: usize, cx: &App) {
        let Some(area) = self.work_area() else {
            return;
        };
        let Some(item) = area
            .panes
            .get(pane)
            .and_then(|pane| pane.items().get(index))
        else {
            return;
        };
        if !matches!(item, PaneItem::Query { .. } | PaneItem::QueryBuilder { .. }) {
            return;
        }
        let tab = area.snapshot_tab(item, cx);
        let Some(area) = self.work_area_mut() else {
            return;
        };
        area.closed.push(tab);
        if area.closed.len() > closing::CLOSED_TABS_KEPT {
            area.closed.remove(0);
        }
    }

    pub(super) fn duplicate_tab_into(
        &mut self,
        source: PaneId,
        destination: PaneId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(open) = self.active_connection() else {
            return;
        };
        let Some(item) = open.work.panes.get(source).and_then(|pane| pane.active()) else {
            return;
        };
        let mut tab = open.work.snapshot_tab(item, cx);
        match &mut tab.content {
            SavedContent::Query { number, .. } => *number = open.work.next_query,
            SavedContent::Builder { number, .. } => *number = open.work.next_builder,
            _ => {}
        }
        let id = open.id;
        self.work_area_mut().unwrap().active_pane = destination;
        self.restore_tab(&tab, id, window, cx);
        self.focus_active_tab(destination, window, cx);
    }

    /// Restores connection `index`'s work area from the profile's saved
    /// workspace; see [`Workspace::restore_connection_workspace_from`].
    #[cfg(test)]
    pub(super) fn restore_connection_workspace(
        &mut self,
        index: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.restore_connection_workspace_from(index, None, window, cx);
    }

    /// Restores connection `index`'s work area from `saved`, or from the
    /// profile's saved workspace when that is `None`.
    ///
    /// The snapshot is handed in by a reconnect of a tab whose session died
    /// with tabs open: what it showed a moment ago is what it should show
    /// again, whether or not the workspace directory could be written.
    pub(super) fn restore_connection_workspace_from(
        &mut self,
        index: usize,
        saved: Option<SavedWorkspace>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(open) = self.connections.get(index) else {
            return;
        };
        if !matches!(open.state, ConnectionState::Open(_)) {
            return;
        }
        let id = open.id;
        let profile = open.profile.id;
        let Some(saved) = saved.or_else(|| self.sessions.load(profile)) else {
            return;
        };
        let previous = self.active_connection;
        let focus = window.focused(cx);
        self.active_connection = index;
        let mut tree = PaneTree::single(Pane::new());
        let first = tree.first_leaf().0;
        let mut leaves = Vec::new();
        restore_layout(&saved.layout, &mut tree, first, &mut leaves);
        // What was closed before the session died can still be reopened.
        let closed = std::mem::take(&mut self.connections[index].work.closed);
        self.connections[index].work = WorkArea {
            active_pane: first,
            panes: tree,
            tab_names: HashMap::new(),
            checkpoint: None,
            next_query: 1,
            next_builder: 1,
            closed,
        };
        for (pane, tabs, active) in leaves {
            self.connections[index].work.active_pane = pane;
            for tab in tabs {
                self.restore_tab(tab, id, window, cx);
            }
            let pane = self.connections[index].work.panes.get_mut(pane).unwrap();
            pane.activate(active.min(pane.items().len().saturating_sub(1)));
        }
        let area = &mut self.connections[index].work;
        area.active_pane = area
            .panes
            .leaf_ids()
            .get(saved.active_pane)
            .copied()
            .unwrap_or(first);
        area.next_query = area.next_query.max(saved.next_query);
        area.next_builder = area.next_builder.max(saved.next_builder);
        area.checkpoint = Some(area.snapshot(cx));
        self.active_connection = previous;
        if index == previous && !self.dialog_open(cx) {
            self.focus_active_tab(self.connections[index].work.active(), window, cx);
        } else if let Some(focus) = focus {
            window.focus(&focus, cx);
        }
        cx.notify();
    }

    pub(super) fn restore_tab(
        &mut self,
        tab: &SavedTab,
        connection: ConnectionId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match &tab.content {
            SavedContent::Query {
                number,
                sql,
                selection,
                editor_share,
                file,
                modified,
                stamp,
            } => {
                self.work_area_mut().unwrap().next_query = *number;
                if let Some(pane) = self.open_query(sql, window, cx) {
                    pane.update(cx, |pane, cx| {
                        pane.restore_workspace_editor(selection.clone(), *editor_share, cx);
                        if let Some(path) = file {
                            pane.restore_file(path.clone(), *modified, *stamp, cx);
                        }
                    });
                    // A draft saved clean is the file's text as it was then, and
                    // the file may have moved on since; an empty one over a file
                    // is nearly always a draft too large to have been kept (see
                    // `DRAFT_LIMIT`). Either way the file is the copy to show.
                    if let Some(path) = file.clone().filter(|_| sql.is_empty() || !*modified) {
                        pane.update(cx, |pane, _| pane.begin_reload());
                        reload_from_file(pane, path, sql.clone(), cx);
                    }
                }
            }
            SavedContent::Detail { target, tab } => {
                self.append_object(target.target(connection), window, cx);
                if let Some(PaneItem::TableDetail(panel)) = self
                    .work_area()
                    .and_then(|area| area.panes.get(area.active()))
                    .and_then(|pane| pane.active())
                {
                    panel
                        .clone()
                        .update(cx, |panel, cx| panel.select_tab(*tab, cx));
                }
            }
            SavedContent::Data { target, order } => {
                self.open_data_with_order(target.target(connection), order.clone(), window, cx)
            }
            SavedContent::Structure { target } if target.name.is_empty() => self.open_new_table(
                connection,
                Scope {
                    catalog: target.catalog.clone(),
                    schema: target.schema.clone(),
                },
                window,
                cx,
            ),
            SavedContent::Structure { target } => {
                self.append_structure(target.target(connection), window, cx)
            }
            SavedContent::Erd { scope } => self.append_erd(
                ErdTarget {
                    connection,
                    scope: scope.clone(),
                },
                window,
                cx,
            ),
            SavedContent::Builder { number, state } => {
                self.work_area_mut().unwrap().next_builder = *number;
                if let Some(panel) = self.open_builder(window, cx) {
                    panel.update(cx, |panel, cx| panel.restore_workspace(state.clone(), cx));
                }
            }
        }
        if let Some(name) = tab.name.as_ref().filter(|name| !name.trim().is_empty()) {
            let area = self.work_area_mut().unwrap();
            if let Some(item) = area.panes.get(area.active()).and_then(|pane| pane.active()) {
                area.tab_names.insert(item.entity_id(), name.clone());
                if let PaneItem::Query { pane, .. } = item {
                    let (pane, name) = (pane.clone(), name.clone());
                    pane.update(cx, |pane, _| pane.set_tab_name(Some(name)));
                }
            }
        }
    }
}

/// Reads `path` on a background task and hands what came of it to `pane`,
/// restored with `restored` as its draft; see [`QueryPane::reload_file`].
fn reload_from_file(
    pane: Entity<QueryPane>,
    path: PathBuf,
    restored: Arc<str>,
    cx: &mut Context<Workspace>,
) {
    let reading = cx.background_spawn(async move { read_sql_file(&path) });
    cx.spawn(async move |_, cx| {
        let read = reading.await;
        pane.update(cx, |pane, cx| pane.reload_file(&restored, read, cx));
    })
    .detach();
}

fn restore_layout<'a>(
    node: &'a SavedNode,
    tree: &mut PaneTree<Pane>,
    pane: PaneId,
    leaves: &mut Vec<(PaneId, &'a [SavedTab], usize)>,
) {
    match node {
        SavedNode::Pane { tabs, active } => leaves.push((pane, tabs, *active)),
        SavedNode::Split {
            horizontal,
            ratio,
            first,
            second,
        } => {
            let axis = if *horizontal {
                Axis::Horizontal
            } else {
                Axis::Vertical
            };
            let other = tree.split(pane, axis, Pane::new()).unwrap();
            let split = split_above(tree.root(), pane).unwrap();
            tree.set_ratio(split, if ratio.is_finite() { *ratio } else { 0.5 });
            restore_layout(first, tree, pane, leaves);
            restore_layout(second, tree, other, leaves);
        }
    }
}

fn split_above(node: &PaneNode<Pane>, pane: PaneId) -> Option<SplitId> {
    match node {
        PaneNode::Split {
            id, first, second, ..
        } => {
            if matches!(first.as_ref(), PaneNode::Leaf { id, .. } if *id == pane) {
                Some(*id)
            } else {
                split_above(first, pane).or_else(|| split_above(second, pane))
            }
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::super::tests::{
        DOCUMENT_END, h2_connection, push_connection, workspace_over_h2, workspace_over_h2_with,
    };
    use super::*;

    fn storage(directory: &std::path::Path) -> SessionStorage {
        SessionStorage {
            directory: Some(directory.to_path_buf()),
            ..SessionStorage::default()
        }
    }

    fn object(connection: ConnectionId) -> ObjectTarget {
        ObjectTarget {
            connection,
            catalog: None,
            schema: Some("PUBLIC".into()),
            folder: Folder::Tables,
            name: "CUSTOMER".into(),
        }
    }

    #[gpui::test]
    fn all_tab_kinds_names_and_nested_splits_survive_reopening(cx: &mut gpui::TestAppContext) {
        let directory = tempfile::tempdir().unwrap();
        let (window, id) = workspace_over_h2_with(
            "workspace-round-trip",
            &[
                "set db_close_delay -1",
                "create table CUSTOMER (ID int primary key, NAME varchar(40))",
            ],
            cx,
        );
        let profile = window
            .update(cx, |workspace, window, cx| {
                workspace.sessions = storage(directory.path());
                workspace.open_query(
                    "insert into CUSTOMER values (1, 'must not run');\n-- 매출 초안",
                    window,
                    cx,
                );
                workspace
                    .active_query()
                    .unwrap()
                    .clone()
                    .update(cx, |pane, cx| {
                        pane.restore_workspace_editor(0..6, 0.65, cx);
                    });
                let first = workspace.active_pane().unwrap();
                let query = workspace.active_query().unwrap().entity_id();
                workspace
                    .work_area_mut()
                    .unwrap()
                    .tab_names
                    .insert(query, "매출 분석".into());
                workspace.open_object(object(id), window, cx);
                if let Some(PaneItem::TableDetail(panel)) = workspace
                    .work_area()
                    .unwrap()
                    .panes
                    .get(first)
                    .unwrap()
                    .active()
                {
                    panel.clone().update(cx, |panel, cx| {
                        panel.select_tab(crate::table_detail::Tab::Ddl, cx)
                    });
                }
                // A detail clone must remain a separate tab, including after a restart.
                workspace.split_active(Axis::Horizontal, window, cx);
                let second = workspace.active_pane().unwrap();
                let split =
                    split_above(workspace.work_area().unwrap().panes.root(), first).unwrap();
                workspace
                    .work_area_mut()
                    .unwrap()
                    .panes
                    .set_ratio(split, 0.32);
                workspace.open_data_with_order(
                    object(id),
                    Some((0, "ID".into(), false)),
                    window,
                    cx,
                );
                workspace.open_structure(object(id), window, cx);
                workspace.split_active(Axis::Vertical, window, cx);
                let third = workspace.active_pane().unwrap();
                let split =
                    split_above(workspace.work_area().unwrap().panes.root(), second).unwrap();
                workspace
                    .work_area_mut()
                    .unwrap()
                    .panes
                    .set_ratio(split, 0.61);
                let builder = workspace.open_builder(window, cx).unwrap();
                builder.update(cx, |builder, cx| {
                    builder.add_table(
                        &object(id),
                        vec![rudbman_erd::ErdColumn {
                            name: "ID".into(),
                            type_name: "INTEGER".into(),
                            primary_key: true,
                            ..Default::default()
                        }],
                        cx,
                    );
                    builder.toggle_column(0, 0, cx);
                    let mut state = serde_json::to_value(builder.workspace_state(cx)).unwrap();
                    state["query"]["where_clauses"] = serde_json::json!(["CUSTOMER.ID > 10"]);
                    state["positions"]["CUSTOMER"] = serde_json::json!([143.0, 87.0]);
                    builder.restore_workspace(serde_json::from_value(state).unwrap(), cx);
                });
                workspace.open_erd(
                    ErdTarget {
                        connection: id,
                        scope: Scope {
                            schema: Some("PUBLIC".into()),
                            catalog: None,
                        },
                    },
                    window,
                    cx,
                );
                workspace.open_new_table(
                    id,
                    Scope {
                        schema: Some("PUBLIC".into()),
                        catalog: None,
                    },
                    window,
                    cx,
                );
                workspace.activate_tab(third, 1, window, cx);
                workspace.connections[0].profile.clone()
            })
            .unwrap();
        cx.run_until_parked();
        let expected = window
            .update(cx, |workspace, window, cx| {
                let expected = workspace.connections[0].work.snapshot(cx);
                workspace.close_connection(0, window, cx);
                expected
            })
            .unwrap();
        assert_eq!(
            storage(directory.path()).load(profile.id),
            Some(expected.clone())
        );
        cx.run_until_parked();

        let connected = connection::connect(
            &profile,
            &connection::h2::driver(),
            &connection::Credentials::typed(Some(String::new()), None),
            &AppSettings::default(),
        )
        .unwrap();
        let reopened =
            cx.add_window(|window, cx| Workspace::new(TitlebarStyle::Custom, window, cx));
        reopened
            .update(cx, |workspace, window, cx| {
                workspace.sessions = storage(directory.path());
                let mut renamed_profile = profile;
                renamed_profile.name = "A renamed connection".into();
                let new_id = push_connection(workspace, renamed_profile, connected, window, cx);
                assert_ne!(new_id, id);
                workspace.restore_connection_workspace(0, window, cx);
                assert_eq!(workspace.connections[0].work.snapshot(cx), expected);
                for (_, pane) in workspace.connections[0].work.panes.leaves() {
                    for tab in pane.items() {
                        assert_eq!(tab.connection(cx), new_id);
                    }
                }
                // Restoring a SQL draft must not execute its INSERT.
                workspace.open_data(object(new_id), window, cx);
            })
            .unwrap();
        cx.run_until_parked();
        reopened
            .update(cx, |workspace, _window, cx| {
                let pane = workspace.work_area().unwrap().data_panes().pop().unwrap();
                assert_eq!(pane.read(cx).row_count(cx), Some(0));
            })
            .unwrap();
    }

    #[gpui::test]
    fn saves_are_profile_scoped_and_failed_connections_do_not_erase_them(
        cx: &mut gpui::TestAppContext,
    ) {
        let directory = tempfile::tempdir().unwrap();
        let (window, _) = workspace_over_h2("workspace-profiles-a", cx);
        let (mut other, connected) = h2_connection("workspace-profiles-b");
        let (first, second) = window
            .update(cx, |workspace, window, cx| {
                workspace.sessions = storage(directory.path());
                workspace.open_query("select 'first'", window, cx);
                let first = workspace.connections[0].profile.id;
                other.name = workspace.connections[0].profile.name.clone();
                let second = other.id;
                push_connection(workspace, other, connected, window, cx);
                workspace.open_query("select 'second'", window, cx);
                workspace.flush_workspaces(cx);
                (first, second)
            })
            .unwrap();
        let mut saved = storage(directory.path());
        let first_state = saved.load(first).unwrap();
        let second_state = saved.load(second).unwrap();
        assert_ne!(first_state, second_state);
        window
            .update(cx, |workspace, _window, cx| {
                workspace.connections[0].state = ConnectionState::Failed("offline".into());
                workspace.connections[0].work = WorkArea::new();
                workspace.flush_workspaces(cx);
            })
            .unwrap();
        assert_eq!(saved.load(first), Some(first_state));
        assert_eq!(saved.load(second), Some(second_state));
    }

    #[gpui::test]
    fn closing_the_last_tab_saves_an_empty_workspace(cx: &mut gpui::TestAppContext) {
        let directory = tempfile::tempdir().unwrap();
        let (window, _) = workspace_over_h2("workspace-empty", cx);
        let profile = window
            .update(cx, |workspace, window, cx| {
                workspace.sessions = storage(directory.path());
                workspace.open_query("select 1", window, cx);
                workspace.flush_workspaces(cx);
                workspace.close_tab(workspace.active_pane().unwrap(), 0, window, cx);
                workspace.flush_workspaces(cx);
                workspace.connections[0].profile.id
            })
            .unwrap();
        assert!(
            matches!(storage(directory.path()).load(profile).unwrap().layout, SavedNode::Pane { tabs, .. } if tabs.is_empty())
        );
    }

    #[gpui::test]
    fn autosave_and_window_close_preserve_the_latest_editor_text(cx: &mut gpui::TestAppContext) {
        let directory = tempfile::tempdir().unwrap();
        let (window, _) = workspace_over_h2("workspace-autosave", cx);
        let profile = window
            .update(cx, |workspace, window, cx| {
                workspace.sessions = storage(directory.path());
                workspace.open_query("select 1", window, cx);
                workspace.connections[0].profile.id
            })
            .unwrap();
        cx.run_until_parked();
        cx.background_executor
            .advance_clock(std::time::Duration::from_secs(1));
        cx.run_until_parked();
        let before = storage(directory.path())
            .load(profile)
            .expect("the autosave ran");
        assert!(
            matches!(&before.layout, SavedNode::Pane { tabs, .. } if matches!(&tabs[0].content, SavedContent::Query { sql, .. } if &**sql == "select 1"))
        );
        cx.simulate_input(window.into(), "-- last edit\n");
        let expected = window
            .update(cx, |workspace, window, cx| {
                let expected = workspace.connections[0].work.snapshot(cx);
                window.remove_window();
                expected
            })
            .unwrap();
        cx.run_until_parked();
        assert_ne!(before, expected);
        assert_eq!(storage(directory.path()).load(profile), Some(expected));
    }

    #[test]
    fn corrupt_and_newer_snapshots_are_preserved() {
        let directory = tempfile::tempdir().unwrap();
        let id = Uuid::new_v4();
        let path = directory.path().join(format!("{id}.json"));
        let state = SavedWorkspace {
            version: 1,
            layout: SavedNode::Pane {
                tabs: vec![],
                active: 0,
            },
            active_pane: 0,
            next_query: 1,
            next_builder: 1,
        };
        let mut future = state.clone();
        future.version = 2;
        for bytes in [
            b"broken json".to_vec(),
            serde_json::to_vec(&future).unwrap(),
        ] {
            std::fs::write(&path, &bytes).unwrap();
            let mut saved = storage(directory.path());
            assert!(saved.load(id).is_none());
            saved.queue(id, state.clone());
            saved.flush();
            assert_eq!(std::fs::read(&path).unwrap(), bytes);
        }
    }

    /// A query tab bound to a file comes back bound to it after a restart,
    /// still named after it and still marked when its draft held unsaved
    /// changes. One whose draft was clean, or came back empty, shows what its
    /// file holds now.
    #[gpui::test]
    fn a_bound_query_keeps_its_file_and_its_mark_across_reopening(cx: &mut gpui::TestAppContext) {
        let directory = tempfile::tempdir().unwrap();
        let files = tempfile::tempdir().unwrap();
        let edited = files.path().join("edited.sql");
        let large = files.path().join("large.sql");
        let clean = files.path().join("clean.sql");
        std::fs::write(&edited, "select 1").unwrap();
        std::fs::write(&large, "select 42").unwrap();
        std::fs::write(&clean, "select 7").unwrap();
        let (window, _) = workspace_over_h2("workspace-files", cx);
        let mut visual = gpui::VisualTestContext::from_window(window.into(), cx);
        window
            .update(&mut visual, |workspace, window, cx| {
                workspace.sessions = storage(directory.path());
                // Standing in for a draft too large to keep: saved empty, over
                // a file that has the text.
                let pane = workspace.open_query("", window, cx).unwrap();
                pane.update(cx, |pane, cx| pane.bind_file(large.clone(), None, cx));
                let pane = workspace.open_query("select 7", window, cx).unwrap();
                pane.update(cx, |pane, cx| pane.bind_file(clean.clone(), None, cx));
                let pane = workspace.open_query("select 1", window, cx).unwrap();
                pane.update(cx, |pane, cx| pane.bind_file(edited.clone(), None, cx));
            })
            .unwrap();
        visual.simulate_keystrokes(DOCUMENT_END);
        visual.simulate_input(" -- unsaved");
        let (profile, expected) = window
            .update(&mut visual, |workspace, window, cx| {
                assert!(workspace.active_query().unwrap().read(cx).is_modified(cx));
                let expected = workspace.connections[0].work.snapshot(cx);
                let profile = workspace.connections[0].profile.clone();
                workspace.close_connection(0, window, cx);
                // The tab whose file is behind its text is asked about first,
                // and "Don't save" is what leaves the draft for the workspace.
                assert!(matches!(
                    workspace
                        .close_question
                        .as_ref()
                        .map(|question| &question.ask),
                    Some(closing::CloseAsk::Unsaved { .. })
                ));
                workspace.answer_close(closing::CloseAnswer::Discard, window, cx);
                assert!(workspace.connections.is_empty(), "the close went ahead");
                (profile, expected)
            })
            .unwrap();
        assert_eq!(storage(directory.path()).load(profile.id), Some(expected));
        cx.run_until_parked();
        // Written by something else while the app was closed.
        std::fs::write(&clean, "select 8").unwrap();

        let connected = connection::connect(
            &profile,
            &connection::h2::driver(),
            &connection::Credentials::typed(Some(String::new()), None),
            &AppSettings::default(),
        )
        .unwrap();
        let reopened =
            cx.add_window(|window, cx| Workspace::new(TitlebarStyle::Custom, window, cx));
        reopened
            .update(cx, |workspace, window, cx| {
                workspace.sessions = storage(directory.path());
                push_connection(workspace, profile, connected, window, cx);
                workspace.restore_connection_workspace(0, window, cx);
            })
            .unwrap();
        cx.run_until_parked();
        reopened
            .update(cx, |workspace, _window, cx| {
                let queries = workspace.connections[0].work.queries();
                let [large_pane, clean_pane, edited_pane] = queries.as_slice() else {
                    panic!("every query tab came back");
                };
                let clean_pane = clean_pane.read(cx);
                assert_eq!(clean_pane.file(), Some(clean.as_path()));
                assert_eq!(
                    clean_pane.editor_text(cx),
                    "select 8",
                    "a clean draft gives way to what the file holds now"
                );
                assert!(!clean_pane.is_modified(cx));
                let edited_pane = edited_pane.read(cx);
                assert_eq!(edited_pane.file(), Some(edited.as_path()));
                assert_eq!(edited_pane.editor_text(cx), "select 1 -- unsaved");
                assert!(edited_pane.is_modified(cx), "the mark survives the restart");
                assert_eq!(edited_pane.file_title().unwrap(), "edited.sql");
                let large_pane = large_pane.read(cx);
                assert_eq!(large_pane.file(), Some(large.as_path()));
                assert_eq!(large_pane.editor_text(cx), "select 42");
                assert!(!large_pane.is_modified(cx));
            })
            .unwrap();
    }

    /// A file name that is not UTF-8 cannot go into the JSON; the tab is saved
    /// unbound rather than failing the whole workspace's save.
    #[cfg(unix)]
    #[gpui::test]
    fn a_path_that_is_not_utf8_does_not_stop_the_workspace_saving(cx: &mut gpui::TestAppContext) {
        use std::os::unix::ffi::OsStrExt;

        let directory = tempfile::tempdir().unwrap();
        let (window, _) = workspace_over_h2("workspace-latin1-path", cx);
        let profile = window
            .update(cx, |workspace, window, cx| {
                workspace.sessions = storage(directory.path());
                let pane = workspace.open_query("select 1", window, cx).unwrap();
                let path = PathBuf::from(std::ffi::OsStr::from_bytes(b"/nowhere/caf\xe9.sql"));
                pane.update(cx, |pane, cx| pane.bind_file(path, None, cx));
                workspace.flush_workspaces(cx);
                workspace.connections[0].profile.id
            })
            .unwrap();
        let saved = storage(directory.path())
            .load(profile)
            .expect("the workspace was still saved");
        assert!(matches!(
            &saved.layout,
            SavedNode::Pane { tabs, .. } if matches!(
                &tabs[0].content,
                SavedContent::Query { file: None, modified: false, stamp: None, .. }
            )
        ));
    }

    /// A draft too large to keep is not warned about while its file holds it
    /// — it comes back from the file — and is once it has been edited.
    #[gpui::test]
    fn a_large_draft_its_file_holds_is_not_warned_about(cx: &mut gpui::TestAppContext) {
        let directory = tempfile::tempdir().unwrap();
        let files = tempfile::tempdir().unwrap();
        let path = files.path().join("large.sql");
        std::fs::write(&path, "-".repeat(crate::query::DRAFT_LIMIT + 1)).unwrap();
        let (window, _) = workspace_over_h2("workspace-large-file", cx);
        let mut visual = gpui::VisualTestContext::from_window(window.into(), cx);
        window
            .update(&mut visual, |workspace, window, cx| {
                workspace.sessions = storage(directory.path());
                workspace.load_sql_file(path.clone(), window, cx);
            })
            .unwrap();
        visual.run_until_parked();
        window
            .update(&mut visual, |workspace, _window, cx| {
                workspace.checkpoint_workspaces(cx);
                workspace.warn_oversized_drafts(cx);
                let pane = workspace.active_query().unwrap().read(cx);
                assert_eq!(pane.file(), Some(path.as_path()));
                assert!(pane.notice().is_none(), "the file keeps this draft");
            })
            .unwrap();
        visual.simulate_input("x");
        window
            .update(&mut visual, |workspace, _window, cx| {
                workspace.checkpoint_workspaces(cx);
                workspace.warn_oversized_drafts(cx);
                let pane = workspace.active_query().unwrap().read(cx);
                assert!(pane.notice().is_some(), "the edit is kept nowhere");
            })
            .unwrap();
    }

    /// A tab bound to no file is written exactly as before files were saved,
    /// and a workspace written then still reads.
    #[test]
    fn an_unbound_query_tab_reads_and_writes_as_it_always_did() {
        let content = SavedContent::Query {
            number: 3,
            sql: "select 1".into(),
            selection: 0..0,
            editor_share: 0.5,
            file: None,
            modified: false,
            stamp: None,
        };
        let json = serde_json::to_value(&content).unwrap();
        assert_eq!(
            json,
            serde_json::json!({
                "kind": "query",
                "number": 3,
                "sql": "select 1",
                "selection": {"start": 0, "end": 0},
                "editor_share": 0.5,
            })
        );
        assert_eq!(
            serde_json::from_value::<SavedContent>(json).unwrap(),
            content
        );
    }

    #[gpui::test]
    fn background_restore_preserves_the_foreground_connection_and_focus(
        cx: &mut gpui::TestAppContext,
    ) {
        let directory = tempfile::tempdir().unwrap();
        let (window, _) = workspace_over_h2("workspace-background-a", cx);
        let (other, connected) = h2_connection("workspace-background-b");
        window
            .update(cx, |workspace, window, cx| {
                workspace.sessions = storage(directory.path());
                workspace.open_query("select 'background'", window, cx);
                workspace.flush_workspaces(cx);
                workspace.connections[0].work = WorkArea::new();
                push_connection(workspace, other, connected, window, cx);
                workspace.open_query("select 'foreground'", window, cx);
                let focused = window.focused(cx);
                workspace.restore_connection_workspace(0, window, cx);
                assert_eq!(workspace.active_connection, 1);
                assert_eq!(window.focused(cx), focused);
                assert_eq!(
                    workspace.active_query().unwrap().read(cx).editor_text(cx),
                    "select 'foreground'"
                );
                assert_eq!(
                    workspace.connections[0].work.queries()[0]
                        .read(cx)
                        .editor_text(cx),
                    "select 'background'"
                );
            })
            .unwrap();
    }
}
