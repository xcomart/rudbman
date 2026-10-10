//! Menus.

use super::*;

impl Workspace {
    /// The menu of one explorer row.
    ///
    /// Node-kind driven, and the rows a kind cannot answer are left *out*
    /// rather than greyed: a schema has no "extract script" the way a table
    /// with no rows selected has no "copy", and offering one greyed for ever
    /// would be describing a command that never applies here. What *is* greyed
    /// is everything on a connection that is not open — the tree can still be
    /// read after a session dies, and none of these commands can run without
    /// one.
    ///
    /// The commands are the workspace's own methods rather than dispatched
    /// actions, and they are handed the node the menu was opened over rather
    /// than reading the selection: the two are the same thing today, because
    /// the tree moves the selection before it asks for a menu, and a row that
    /// acted on the selection would be one refactor away from acting on
    /// something else.
    pub(super) fn explorer_rows(&self, node: &NodeId, cx: &mut Context<Self>) -> Vec<MenuRow> {
        let this = cx.entity();
        let connection = node.connection();
        let live = self
            .connections
            .iter()
            .any(|open| open.id == connection && matches!(open.state, ConnectionState::Open(_)));
        let relation = node
            .as_target()
            .filter(|target| target.folder.is_relation());
        let scope = node.as_scope();
        let mut rows = Vec::new();

        if let Some(target) = relation {
            let object =
                |run: fn(&mut Workspace, ObjectTarget, &mut Window, &mut Context<Self>)| {
                    let this = this.clone();
                    let target = target.clone();
                    move |window: &mut Window, cx: &mut App| {
                        let target = target.clone();
                        this.update(cx, |workspace, cx| run(workspace, target, window, cx));
                    }
                };
            // First, because it is the shortest question a table can be asked:
            // what is in it. Only relations get it — a routine has no rows —
            // which the `relation` filter above has already settled.
            rows.push(
                MenuRow::new(ts!("menu.view_data"))
                    .enabled(live)
                    .on_activate(object(Workspace::open_data)),
            );
            // Beside it, because the second shortest question a table can be
            // asked is what it is made of — and unlike the detail panel, this
            // one can answer by changing it (§7.10).
            rows.push(
                MenuRow::new(ts!("menu.view_structure"))
                    .enabled(live)
                    .on_activate(object(Workspace::open_structure)),
            );
            rows.push(
                MenuRow::new(ts!("menu.query_object"))
                    .shortcut(format!("{SHORTCUT_MODIFIER}+Enter"))
                    .enabled(live)
                    .on_activate(object(|workspace, target, window, cx| {
                        workspace.open_query_for(&target, window, cx);
                    })),
            );
            rows.push(
                MenuRow::new(ts!("menu.add_to_builder"))
                    .enabled(live)
                    .on_activate(object(Workspace::add_to_builder)),
            );
            rows.push(
                MenuRow::new(ts!("menu.extract_script"))
                    .enabled(live)
                    .on_activate(object(Workspace::open_extract)),
            );
            rows.push(
                MenuRow::new(ts!("menu.transfer_table"))
                    .enabled(live)
                    .on_activate(object(Workspace::open_transfer)),
            );
            rows.push(MenuRow::separator());
        }

        // The places a table would appear, which is where §7.10's create path
        // is offered: a schema, a catalogue on a product whose schema level was
        // skipped, and the Tables folder under either. Left out entirely on the
        // rest — a table is not made inside another table, and the connection
        // root names no scope to make one in — because a row a kind cannot
        // answer is left out rather than greyed.
        let creatable = match node {
            NodeId::Schema { .. } | NodeId::Catalog { .. } => scope.clone(),
            NodeId::Folder {
                folder: Folder::Tables,
                scope,
                ..
            } => Some(scope.clone()),
            _ => None,
        };
        if let Some(scope) = creatable {
            let this = this.clone();
            rows.push(
                MenuRow::new(ts!("menu.new_table"))
                    .enabled(live)
                    .on_activate(move |window: &mut Window, cx: &mut App| {
                        let scope = scope.clone();
                        this.update(cx, |workspace, cx| {
                            workspace.open_new_table(connection, scope, window, cx);
                        });
                    }),
            );
        }

        // The connection root names no scope — a diagram of every catalogue at
        // once is not a diagram — so its rows are drawn and greyed: the menu of
        // a row that offered nothing at all would read as a broken right-click.
        let scoped = |run: fn(
            &mut Workspace,
            ConnectionId,
            explorer::Scope,
            &mut Window,
            &mut Context<Self>,
        )| {
            let this = this.clone();
            let scope = scope.clone();
            move |window: &mut Window, cx: &mut App| {
                let Some(scope) = scope.clone() else {
                    return;
                };
                this.update(cx, |workspace, cx| {
                    run(workspace, connection, scope, window, cx)
                });
            }
        };
        rows.push(
            MenuRow::new(ts!("menu.erd"))
                .shortcut(format!("{SHORTCUT_MODIFIER}+E"))
                .enabled(live && scope.is_some())
                .on_activate(scoped(|workspace, connection, scope, window, cx| {
                    workspace.open_erd(ErdTarget { connection, scope }, window, cx);
                })),
        );
        rows.push(
            MenuRow::new(ts!("menu.backup_schema"))
                .enabled(live && scope.is_some())
                .on_activate(scoped(Workspace::open_backup)),
        );
        rows
    }

    /// The menu of one connection tab.
    ///
    /// A right-click does not select the tab (see
    /// [`TabBar::on_context_menu`]), so every row here names the tab that was
    /// pressed rather than the one on screen. The two commands that open
    /// something into a work area bring that tab to the front first: they act
    /// on "the connection showing", and the alternative would be a new query
    /// pane appearing in a work area the user is not looking at.
    pub(super) fn connection_rows(&self, index: usize, cx: &mut Context<Self>) -> Vec<MenuRow> {
        let this = cx.entity();
        let live = self
            .connections
            .get(index)
            .is_some_and(|open| matches!(open.state, ConnectionState::Open(_)));
        let on_tab = |run: fn(&mut Workspace, &mut Window, &mut Context<Self>)| {
            let this = this.clone();
            move |window: &mut Window, cx: &mut App| {
                this.update(cx, |workspace, cx| {
                    workspace.select_connection(index, window, cx);
                    run(workspace, window, cx);
                });
            }
        };

        vec![
            MenuRow::new(ts!("menu.new_query"))
                .shortcut(format!("{SHORTCUT_MODIFIER}+T"))
                .enabled(live)
                .on_activate(on_tab(|workspace, window, cx| {
                    workspace.open_query("", window, cx);
                })),
            MenuRow::new(ts!("menu.new_builder"))
                .enabled(live)
                .on_activate(on_tab(|workspace, window, cx| {
                    workspace.open_builder(window, cx);
                })),
            MenuRow::separator(),
            MenuRow::new(ts!("tab.close")).on_activate({
                let this = this.clone();
                move |window, cx| {
                    this.update(cx, |workspace, cx| {
                        workspace.close_connection(index, window, cx);
                    });
                }
            }),
        ]
    }

    /// The menu of one tab of one pane's strip.
    ///
    /// The three closes are the strip's, the three pane commands are the
    /// layout's, and the split between them is the separator: above it the
    /// rows act on tabs, below it on the box holding them. "Close the other
    /// tabs" and "close the tabs to the right" exist nowhere else in the
    /// program — there is no gesture for them — which is exactly the case §7.8
    /// says a menu should grow an API for.
    pub(super) fn pane_tab_rows(
        &self,
        pane: PaneId,
        index: usize,
        cx: &mut Context<Self>,
    ) -> Vec<MenuRow> {
        let this = cx.entity();
        let Some(area) = self.work_area() else {
            return Vec::new();
        };
        let count = area.panes.get(pane).map_or(0, |pane| pane.items().len());
        let lone_pane = area.panes.leaf_count() <= 1;
        let close = |victims: Vec<usize>| {
            let this = this.clone();
            move |window: &mut Window, cx: &mut App| {
                this.update(cx, |workspace, cx| {
                    workspace.close_tabs(pane, &victims, window, cx);
                });
            }
        };
        let others: Vec<usize> = (0..count).filter(|other| *other != index).collect();
        let to_the_right: Vec<usize> = (index + 1..count).collect();

        vec![
            MenuRow::new(ts!("context.close_tab")).on_activate(close(vec![index])),
            MenuRow::new(ts!("context.close_others"))
                .enabled(!others.is_empty())
                .on_activate(close(others)),
            MenuRow::new(ts!("context.close_right"))
                .enabled(!to_the_right.is_empty())
                .on_activate(close(to_the_right)),
            MenuRow::separator(),
            MenuRow::new(ts!("context.split_right"))
                .shortcut(format!("{PANE_SHORTCUT_LABEL}+Shift+D"))
                .on_activate({
                    let this = this.clone();
                    move |_window, cx| {
                        this.update(cx, |workspace, cx| {
                            workspace.split_pane(pane, Axis::Horizontal, cx);
                        });
                    }
                }),
            MenuRow::new(ts!("context.split_below"))
                .shortcut(format!("{PANE_SHORTCUT_LABEL}+Shift+S"))
                .on_activate({
                    let this = this.clone();
                    move |_window, cx| {
                        this.update(cx, |workspace, cx| {
                            workspace.split_pane(pane, Axis::Vertical, cx);
                        });
                    }
                }),
            MenuRow::new(ts!("context.close_pane"))
                .shortcut(format!("{PANE_SHORTCUT_LABEL}+W"))
                .enabled(!lone_pane)
                .on_activate({
                    let this = this.clone();
                    move |window, cx| {
                        this.update(cx, |workspace, cx| workspace.close_pane(pane, window, cx));
                    }
                }),
        ]
    }

    /// The menu of one row of the welcome screen's saved list.
    ///
    /// The two things there are to do with a saved connection: open it, which
    /// is what clicking the row already does, and change it — which otherwise
    /// means opening the dialog from the button above and finding the profile
    /// again in a second copy of the same list.
    pub(super) fn profile_rows(&self, id: Uuid, cx: &mut Context<Self>) -> Vec<MenuRow> {
        let this = cx.entity();
        vec![
            MenuRow::new(ts!("context.connect")).on_activate({
                let this = this.clone();
                move |window, cx| {
                    this.update(cx, |workspace, cx| workspace.open_profile(id, window, cx));
                }
            }),
            MenuRow::separator(),
            MenuRow::new(ts!("context.edit")).on_activate(move |window, cx| {
                this.update(cx, |workspace, cx| workspace.edit_profile(id, window, cx));
            }),
        ]
    }
}
