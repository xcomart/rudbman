//! Dropping and truncating what the explorer lists.
//!
//! The two commands no other gesture can take back, so they are asked about
//! the hard way. The menu row opens a question that shows every statement it
//! will send ([`drop_sql`] writes them) and does not run them until the user has
//! typed the object's name back — or, over a selection of several, the verb
//! itself. A click, an `Enter` or a habit is not enough to say yes: the danger
//! button stays disabled until the text matches, and `Enter` in the field is
//! refused the same way.
//!
//! The batch runs like the structure pane's (§7.10, [`apply_alter`]): autocommit
//! on, the first failure stops it, and what ran before the failure stays. It is
//! refused while another pane on the connection holds uncommitted work, because
//! the autocommit flip — and every product's implicit commit at DDL — would end
//! that transaction along with it.
//!
//! A routine or a trigger is looked up by its name before its statements are
//! written, because the tree holds the name and nothing else: the lookup is
//! what finds every overload of it, or every table a PostgreSQL trigger of that
//! name fires on, and each one becomes a statement of its own in the question.

use gpui::Subscription;
use rudbman_jdbc::DescribeRequest;
use rudbman_sql::{Dialect, DialectId};
use rugpui::TextInput;

use super::*;
use crate::drop_sql::{DropKind, DropTarget, Identity, drop_statement, truncate_statement};
use crate::query::QueryError;
use crate::struct_pane::apply_alter;

/// Which of the two commands a question is about.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Removal {
    /// `DROP`: the object goes.
    Drop,
    /// `TRUNCATE`: the rows go and the table stays.
    Truncate,
}

/// A drop or a truncate, waiting for the user to type their way to yes.
pub(super) struct RemovalQuestion {
    pub(super) removal: Removal,
    pub(super) connection: ConnectionId,
    /// What goes, in the order the statements name it.
    pub(super) nodes: Vec<NodeId>,
    /// Exactly what runs on a yes, and what the question shows.
    pub(super) statements: Vec<String>,
    /// What has to be typed: the one object's name, or the verb over several.
    pub(super) phrase: String,
    pub(super) input: Entity<TextInput>,
    scroll: ScrollHandle,
    _typing: Subscription,
}

impl RemovalQuestion {
    /// Whether the field holds the phrase, which is the whole of "yes".
    ///
    /// Exact, case included, and only the surrounding blanks forgiven: the
    /// names are the catalog's, and on most products `orders` and `ORDERS` are
    /// two tables.
    pub(super) fn answered(&self, cx: &App) -> bool {
        self.input.read(cx).content().trim() == self.phrase
    }
}

/// The statement that removes `node`, or `None` where `removal` does not apply
/// to it on this product.
///
/// `materialized` is what a lookup said about a view; see
/// [`Workspace::ask_removal`].
fn statement(
    dialect: &Dialect,
    removal: Removal,
    node: &NodeId,
    materialized: bool,
) -> Option<String> {
    match (removal, node) {
        (
            Removal::Truncate,
            NodeId::Object {
                folder: Folder::Tables,
                scope,
                name,
                ..
            },
        ) => Some(truncate_statement(
            dialect,
            scope.catalog.as_deref(),
            scope.schema.as_deref(),
            name,
        )),
        (
            Removal::Drop,
            NodeId::Object {
                folder,
                scope,
                name,
                ..
            },
        ) => {
            // Routines and triggers need a lookup first; see `identified`.
            let kind = match folder {
                Folder::Tables => DropKind::Table,
                Folder::Views if materialized => DropKind::MaterializedView,
                Folder::Views => DropKind::View,
                Folder::Sequences => DropKind::Sequence,
                Folder::Procedures | Folder::Functions | Folder::Triggers => return None,
            };
            drop_statement(
                dialect,
                &DropTarget::new(
                    kind,
                    scope.catalog.as_deref(),
                    scope.schema.as_deref(),
                    name,
                ),
            )
        }
        (Removal::Drop, NodeId::Schema { catalog, name, .. }) => drop_statement(
            dialect,
            &DropTarget::new(DropKind::Schema, catalog.as_deref(), None, name),
        ),
        (Removal::Drop, NodeId::Catalog { name, .. }) => drop_statement(
            dialect,
            &DropTarget::new(DropKind::Database, None, None, name),
        ),
        _ => None,
    }
}

/// The kind of drop a routine's or a trigger's folder stands for, or `None` for
/// everything that is dropped without a lookup.
fn looked_up_kind(removal: Removal, node: &NodeId) -> Option<DropKind> {
    match (removal, node) {
        (Removal::Drop, NodeId::Object { folder, .. }) => match folder {
            Folder::Procedures => Some(DropKind::Procedure),
            Folder::Functions => Some(DropKind::Function),
            Folder::Triggers => Some(DropKind::Trigger),
            Folder::Tables | Folder::Views | Folder::Sequences => None,
        },
        _ => None,
    }
}

/// Whether `removal` is offered over `node` on this product at all, before any
/// lookup: SQLite is the one product with no routines to drop.
fn applies(dialect: &Dialect, removal: Removal, node: &NodeId) -> bool {
    match looked_up_kind(removal, node) {
        Some(DropKind::Procedure | DropKind::Function) => dialect.id() != DialectId::Sqlite,
        Some(_) => true,
        None => statement(dialect, removal, node, false).is_some(),
    }
}

/// One routine or trigger a lookup found under a name, with what tells it
/// apart from its namesakes. See [`drop_sql::Identity`](crate::drop_sql::Identity).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct Found {
    /// The catalog the driver filed it under: Oracle's package, for a routine.
    catalog: Option<String>,
    /// PostgreSQL's identity argument list; empty for none.
    arguments: Option<String>,
    /// PostgreSQL's `prokind`.
    routine_kind: Option<String>,
    /// The driver's specific name.
    specific_name: Option<String>,
    /// The table a trigger fires on.
    table: Option<String>,
}

/// Every routine or trigger listed under `node`'s exact name, as its folder's
/// own `DESCRIBE` reports it.
///
/// **Blocks**, on the session's worker thread. A failed lookup finds nothing,
/// and [`identified`] then drops by the name alone, so that the server says
/// for itself whatever it thinks of that.
fn look_up(session: &rudbman_jdbc::Session, node: &NodeId) -> Vec<Found> {
    let NodeId::Object {
        folder,
        scope,
        name,
        ..
    } = node
    else {
        return Vec::new();
    };
    let mut request = DescribeRequest::new(folder.describe_kind()).with_name(name);
    request.catalog = scope.catalog.clone();
    request.schema = scope.schema.clone();
    let Ok(result) = session.describe(&request) else {
        return Vec::new();
    };
    let text = |item: &serde_json::Map<String, serde_json::Value>, key: &str| {
        item.get(key)
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned)
    };
    result
        .items
        .iter()
        // The name goes to the driver as a pattern, where `_` matches anything.
        .filter(|item| text(item, "name").as_deref() == Some(name.as_str()))
        .map(|item| {
            let present = |key| text(item, key).filter(|value| !value.is_empty());
            Found {
                catalog: present("catalog"),
                // Empty is an answer here: a routine with no arguments.
                arguments: text(item, "arguments"),
                routine_kind: present("routine_kind"),
                specific_name: present("specific_name"),
                table: present("table"),
            }
        })
        .collect()
}

/// The statements that drop the routine or trigger `node` names, one for each
/// of `found` the product tells apart, in the order found.
///
/// Products that need nothing but the name get one statement whatever the
/// lookup found, even nothing. Empty where nothing found can be dropped: an
/// Oracle routine that lives in a package, a PostgreSQL trigger whose table the
/// lookup did not find.
fn identified(dialect: &Dialect, removal: Removal, node: &NodeId, found: &[Found]) -> Vec<String> {
    let (Some(kind), NodeId::Object { scope, name, .. }) = (looked_up_kind(removal, node), node)
    else {
        return Vec::new();
    };
    let target = DropTarget::new(
        kind,
        scope.catalog.as_deref(),
        scope.schema.as_deref(),
        name,
    );
    if found.is_empty() {
        return drop_statement(dialect, &target).into_iter().collect();
    }
    let oracle = dialect.id() == DialectId::Oracle;
    let mut statements: Vec<String> = Vec::new();
    for found in found {
        let identity = Identity {
            arguments: found.arguments.as_deref(),
            routine_kind: found.routine_kind.as_deref(),
            specific_name: found.specific_name.as_deref(),
            package: found.catalog.as_deref().filter(|_| oracle),
            table: found.table.as_deref(),
        };
        if let Some(statement) = drop_statement(dialect, &DropTarget { identity, ..target })
            && !statements.contains(&statement)
        {
            statements.push(statement);
        }
    }
    statements
}

/// Every statement `removal` sends for `node`, looked up where it needs to be.
///
/// **Blocks**, on the session's worker thread: a view asks whether it is a
/// materialized one, and a routine or a trigger asks what it is.
fn plan(
    session: &rudbman_jdbc::Session,
    dialect: &Dialect,
    removal: Removal,
    node: &NodeId,
) -> Vec<String> {
    if looked_up_kind(removal, node).is_some() {
        return identified(dialect, removal, node, &look_up(session, node));
    }
    let materialized = removal == Removal::Drop && is_materialized(session, node);
    statement(dialect, removal, node, materialized)
        .into_iter()
        .collect()
}

/// What a removal over `node` acts on, in the order it runs; empty when the
/// command does not apply to it.
///
/// Every selected object it applies to when `node` is one of them — the copies
/// do the same, and a right-click on one of several selected tables is how a
/// selection is acted on — and `node` alone otherwise. A schema or a database
/// is always alone: dropping several of those at once is not something one
/// right-click should be able to ask for.
///
/// Triggers go first, then views, tables and routines, and sequences last, the
/// order dependents have to go in: a trigger rides on a table and calls a
/// routine, a view reads the tables and routines beside it, and a table's
/// default may call a routine or draw on a sequence.
fn removal_nodes(
    removal: Removal,
    dialect: &Dialect,
    node: &NodeId,
    selection: &[NodeId],
) -> Vec<NodeId> {
    let applies = |node: &NodeId| applies(dialect, removal, node);
    if !applies(node) {
        return Vec::new();
    }
    let mut nodes: Vec<NodeId> =
        if matches!(node, NodeId::Object { .. }) && selection.contains(node) {
            selection
                .iter()
                .filter(|other| {
                    matches!(other, NodeId::Object { .. })
                        && other.connection() == node.connection()
                        && applies(other)
                })
                .cloned()
                .collect()
        } else {
            vec![node.clone()]
        };
    nodes.sort_by_key(|node| match node {
        NodeId::Object { folder, .. } => match folder {
            Folder::Triggers => 0,
            Folder::Views => 1,
            Folder::Tables => 2,
            Folder::Procedures | Folder::Functions => 3,
            Folder::Sequences => 4,
        },
        _ => 5,
    });
    nodes
}

/// The name a node's removal asks to have typed back.
fn name_of(node: &NodeId) -> &str {
    match node {
        NodeId::Object { name, .. }
        | NodeId::Schema { name, .. }
        | NodeId::Catalog { name, .. } => name,
        NodeId::Connection(_) | NodeId::Folder { .. } | NodeId::Error(_) => "",
    }
}

/// The menu row's label: what goes, by kind when it is one thing.
fn menu_label(removal: Removal, nodes: &[NodeId]) -> SharedString {
    match (removal, nodes) {
        (Removal::Truncate, [_]) => ts!("menu.truncate_table"),
        (Removal::Truncate, _) => ts!("menu.truncate_tables", count = nodes.len()),
        (Removal::Drop, [node]) => match node {
            NodeId::Object {
                folder: Folder::Views,
                ..
            } => ts!("menu.drop_view"),
            NodeId::Object {
                folder: Folder::Sequences,
                ..
            } => ts!("menu.drop_sequence"),
            NodeId::Object {
                folder: Folder::Procedures,
                ..
            } => ts!("menu.drop_procedure"),
            NodeId::Object {
                folder: Folder::Functions,
                ..
            } => ts!("menu.drop_function"),
            NodeId::Object {
                folder: Folder::Triggers,
                ..
            } => ts!("menu.drop_trigger"),
            NodeId::Schema { .. } => ts!("menu.drop_schema"),
            NodeId::Catalog { .. } => ts!("menu.drop_database"),
            _ => ts!("menu.drop_table"),
        },
        (Removal::Drop, _) => ts!("menu.drop_objects", count = nodes.len()),
    }
}

/// Whether a lookup says `node` is a materialized view, which PostgreSQL and
/// Oracle refuse to drop as a plain one.
///
/// **Blocks**, on the session's worker thread. Asks for the one name with both
/// view types and reads the type back, rather than asking for materialized
/// views alone: a driver that ignores a type it does not know would otherwise
/// answer every plain view as one. A failed lookup is a plain view — the drop
/// then says for itself what the server thinks.
fn is_materialized(session: &rudbman_jdbc::Session, node: &NodeId) -> bool {
    let NodeId::Object {
        folder: Folder::Views,
        scope,
        name,
        ..
    } = node
    else {
        return false;
    };
    let mut request = DescribeRequest::new("tables").with_table(name);
    request.catalog = scope.catalog.clone();
    request.schema = scope.schema.clone();
    request.types = Some(vec!["VIEW".into(), "MATERIALIZED VIEW".into()]);
    session.describe(&request).is_ok_and(|result| {
        result.items.iter().any(|item| {
            item.get("name").and_then(serde_json::Value::as_str) == Some(name.as_str())
                && item
                    .get("type")
                    .and_then(serde_json::Value::as_str)
                    .is_some_and(|kind| kind.eq_ignore_ascii_case("MATERIALIZED VIEW"))
        })
    })
}

impl Workspace {
    /// The explorer menu's drop and truncate rows for `node`, in that order;
    /// empty where neither applies.
    ///
    /// Grayed on a connection that is not open, like every command that needs
    /// a session, and on a read-only profile, whose session would refuse them.
    pub(super) fn removal_rows(
        &self,
        node: &NodeId,
        live: bool,
        cx: &mut Context<Self>,
    ) -> Vec<MenuRow> {
        let Some(open) = self
            .connections
            .iter()
            .find(|open| open.id == node.connection())
        else {
            return Vec::new();
        };
        let dialect = Dialect::from_id(&Self::dialect_of(&open.profile));
        let writable = !open.profile.read_only;
        let selection = self.explorer.read(cx).selected_nodes(cx);
        let this = cx.entity();
        [Removal::Truncate, Removal::Drop]
            .into_iter()
            .filter_map(|removal| {
                let nodes = removal_nodes(removal, &dialect, node, &selection);
                if nodes.is_empty() {
                    return None;
                }
                let this = this.clone();
                Some(
                    MenuRow::new(menu_label(removal, &nodes))
                        .enabled(live && writable)
                        .on_activate(move |window: &mut Window, cx: &mut App| {
                            let nodes = nodes.clone();
                            this.update(cx, |workspace, cx| {
                                workspace.ask_removal(removal, nodes, window, cx);
                            });
                        }),
                )
            })
            .collect()
    }

    /// The refusal that stands between a removal and its session, if any.
    ///
    /// Asked when the question opens and again when it is answered, because
    /// another pane may have written in between.
    fn removal_refusal(
        &self,
        connection: ConnectionId,
    ) -> Result<connection::SessionHandle, SharedString> {
        let Some(open) = self.connections.iter().find(|open| open.id == connection) else {
            return Err(ts!("explorer.disconnected"));
        };
        if open.profile.read_only {
            return Err(ts!("removal.read_only"));
        }
        let Some(session) = self.session_of(connection) else {
            return Err(ts!("explorer.disconnected"));
        };
        if session.has_uncommitted() {
            return Err(session.uncommitted_refusal());
        }
        Ok(session)
    }

    /// Writes the statements for `nodes` and puts them up as a question.
    ///
    /// The statements are written on the session's thread rather than here,
    /// because some need a lookup first; see [`plan`]. When one of `nodes`
    /// turns out to have nothing that can be dropped, nothing is asked and a
    /// notice names it instead.
    pub(super) fn ask_removal(
        &mut self,
        removal: Removal,
        nodes: Vec<NodeId>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(connection) = nodes.first().map(NodeId::connection) else {
            return;
        };
        let session = match self.removal_refusal(connection) {
            Ok(session) => session,
            Err(refusal) => {
                rugpui_shell::show_toast(rugpui_shell::Toast::warning(refusal), cx);
                return;
            }
        };
        let Some(open) = self.connections.iter().find(|open| open.id == connection) else {
            return;
        };
        let dialect = Dialect::from_id(&Self::dialect_of(&open.profile));
        let planning = cx.background_spawn(async move {
            let mut statements = Vec::new();
            let mut missing = Vec::new();
            for node in &nodes {
                let planned = plan(session.session(), &dialect, removal, node);
                if planned.is_empty() {
                    missing.push(name_of(node).to_string());
                }
                statements.extend(planned);
            }
            (nodes, statements, missing)
        });
        cx.spawn_in(window, async move |workspace, cx| {
            let (nodes, statements, missing) = planning.await;
            workspace
                .update_in(cx, |workspace, window, cx| {
                    if missing.is_empty() {
                        workspace.open_removal(removal, connection, nodes, statements, window, cx);
                    } else {
                        rugpui_shell::show_toast(
                            rugpui_shell::Toast::warning(ts!(
                                "removal.not_droppable",
                                names = missing.join(", ")
                            )),
                            cx,
                        );
                    }
                })
                .ok();
        })
        .detach();
    }

    /// Puts the question on screen.
    ///
    /// Nothing opens if something else did while the statements were being
    /// written: the user has moved on, and a question that pushed their new
    /// dialog away would be answering for them.
    fn open_removal(
        &mut self,
        removal: Removal,
        connection: ConnectionId,
        nodes: Vec<NodeId>,
        statements: Vec<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if statements.is_empty() || self.dialog_open(cx) {
            return;
        }
        if !self.close_overlays(window, cx) {
            return;
        }
        let phrase = match nodes.as_slice() {
            [node] => name_of(node).to_string(),
            _ => match removal {
                Removal::Drop => "DROP".to_string(),
                Removal::Truncate => "TRUNCATE".to_string(),
            },
        };
        let this = cx.weak_entity();
        let input = cx.new(|cx| {
            TextInput::new(cx).on_submit(move |_, window, cx| {
                // TextInput is still borrowed while its submit handler runs.
                let this = this.clone();
                window.defer(cx, move |window, cx| {
                    this.update(cx, |workspace, cx| {
                        workspace.answer_removal(true, window, cx)
                    })
                    .ok();
                });
            })
        });
        // The danger button follows the field, keystroke by keystroke.
        let typing = cx.observe(&input, |_workspace, _input, cx| cx.notify());
        window.focus(&input.read(cx).focus_handle(cx), cx);
        self.removal = Some(RemovalQuestion {
            removal,
            connection,
            nodes,
            statements,
            phrase,
            input,
            scroll: ScrollHandle::new(),
            _typing: typing,
        });
        cx.notify();
    }

    /// Answers the question: `run` is the danger button or `Enter`, and runs
    /// only once the phrase has been typed; anything else puts it away.
    pub(super) fn answer_removal(
        &mut self,
        run: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(question) = self.removal.take() else {
            return;
        };
        if !run {
            self.restore_focus(window, cx);
            cx.notify();
            return;
        }
        if !question.answered(cx) {
            self.removal = Some(question);
            return;
        }
        let session = match self.removal_refusal(question.connection) {
            Ok(session) => session,
            Err(refusal) => {
                rugpui_shell::show_toast(rugpui_shell::Toast::warning(refusal), cx);
                // Kept up while it can still be answered: a transaction can be
                // committed and the question asked again.
                if self.session_of(question.connection).is_some() {
                    self.removal = Some(question);
                } else {
                    self.restore_focus(window, cx);
                }
                cx.notify();
                return;
            }
        };
        let restore = self
            .connections
            .iter()
            .find(|open| open.id == question.connection)
            .is_some_and(|open| open.profile.auto_commit);
        self.restore_focus(window, cx);
        cx.notify();

        let RemovalQuestion {
            removal,
            connection,
            nodes,
            statements,
            ..
        } = question;
        let running = cx
            .background_spawn(async move { apply_alter(session.session(), &statements, restore) });
        cx.spawn(async move |workspace, cx| {
            let outcome = running.await;
            workspace
                .update(cx, |workspace, cx| {
                    workspace.removed(removal, connection, &nodes, outcome, cx);
                })
                .ok();
        })
        .detach();
    }

    /// Reports how a removal ended, and reads again what it changed.
    ///
    /// The tree is read again whatever happened: the statements before a
    /// failed one ran. A dropped object's folder is what changed; a dropped
    /// schema or database moves the levels above the folders, so the whole
    /// connection is read again. A truncate changes nothing the tree shows.
    fn removed(
        &mut self,
        removal: Removal,
        connection: ConnectionId,
        nodes: &[NodeId],
        outcome: Result<usize, crate::struct_pane::AlterFailure>,
        cx: &mut Context<Self>,
    ) {
        let toast = match outcome {
            Ok(count) => rugpui_shell::Toast::info(match removal {
                Removal::Drop => ts!("removal.dropped", count = count),
                Removal::Truncate => ts!("removal.truncated", count = count),
            }),
            Err(failure) => {
                let (committed, total) = (failure.committed, failure.total);
                let error = QueryError::new(failure.error).message;
                log::error!("a drop or truncate failed: {error}");
                rugpui_shell::Toast::error(if committed == 0 {
                    ts!("removal.failed", error = error)
                } else {
                    ts!(
                        "removal.failed_partway",
                        number = committed + 1,
                        total = total,
                        committed = committed,
                        error = error
                    )
                })
            }
        };
        rugpui_shell::show_toast(toast, cx);

        if removal == Removal::Truncate {
            return;
        }
        let mut reload: Vec<NodeId> = Vec::new();
        for node in nodes {
            let node = match node {
                NodeId::Object { .. } => node.clone(),
                _ => NodeId::Connection(connection),
            };
            if !reload.contains(&node) {
                reload.push(node);
            }
        }
        self.explorer.update(cx, |explorer, cx| {
            for node in &reload {
                explorer.reload(node, cx);
            }
        });
    }

    /// The question, while one is up.
    ///
    /// The statements in full, and a field the name has to be typed into
    /// before the danger button wakes up: the question the write confirmation
    /// asks, made impossible to answer by reflex.
    pub(super) fn render_removal(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let question = self.removal.as_ref()?;
        let theme = theme(cx);
        let this = cx.entity();
        let dismiss = this.clone();
        let (title, warning, run_label) = match question.removal {
            Removal::Drop => (
                ts!("removal.drop_title"),
                ts!("removal.drop_body"),
                ts!("removal.drop_run"),
            ),
            Removal::Truncate => (
                ts!("removal.truncate_title"),
                ts!("removal.truncate_body"),
                ts!("removal.truncate_run"),
            ),
        };
        let answered = question.answered(cx);

        let body = div()
            .flex()
            .flex_col()
            .gap(px(12.))
            .child(
                div()
                    .text_size(rugpui::TextSize::Sm.size(cx))
                    .text_color(theme.danger)
                    .child(warning),
            )
            .child(
                div()
                    .relative()
                    .flex()
                    .flex_col()
                    .min_h_0()
                    .child(
                        div()
                            .id("removal-statements")
                            .track_scroll(&question.scroll)
                            .max_h(rugpui::scaled(200., cx))
                            .overflow_y_scroll()
                            .restrict_scroll_to_axis()
                            .p(px(8.))
                            .rounded_md()
                            .bg(theme.surface)
                            .border_1()
                            .border_color(theme.border)
                            .text_size(rugpui::TextSize::Xs.size(cx))
                            .text_color(theme.text_muted)
                            .child(question.statements.join("\n")),
                    )
                    .with_scrollbar("removal-statements-scrollbar", &question.scroll),
            )
            .child(
                div()
                    .text_size(rugpui::TextSize::Sm.size(cx))
                    .text_color(theme.text)
                    .child(ts!(
                        "removal.type_to_confirm",
                        phrase = question.phrase.as_str()
                    )),
            )
            .child(question.input.clone())
            .child(
                div()
                    .flex()
                    .flex_row()
                    .justify_end()
                    .gap(px(8.))
                    .child(
                        Button::new("removal-cancel", ts!("common.cancel"))
                            .variant(ButtonVariant::Secondary)
                            .on_click({
                                let this = this.clone();
                                move |_, window, cx| {
                                    this.update(cx, |workspace, cx| {
                                        workspace.answer_removal(false, window, cx);
                                    });
                                }
                            }),
                    )
                    .child(
                        Button::new("removal-run", run_label)
                            .variant(ButtonVariant::Danger)
                            .disabled(!answered)
                            .on_click(move |_, window, cx| {
                                this.update(cx, |workspace, cx| {
                                    workspace.answer_removal(true, window, cx);
                                });
                            }),
                    ),
            );

        Some(
            modal(
                "removal-confirm",
                title,
                rugpui::scaled(480., cx),
                body,
                move |window, cx| {
                    dismiss.update(cx, |workspace, cx| {
                        workspace.answer_removal(false, window, cx);
                    });
                },
            )
            .initial_focus(&question.input.read(cx).focus_handle(cx))
            .escape_dismisses(true)
            .into_any_element(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const C: ConnectionId = ConnectionId(1);

    fn object(folder: Folder, name: &str) -> NodeId {
        NodeId::Object {
            connection: C,
            scope: explorer::Scope {
                catalog: None,
                schema: Some("APP".into()),
            },
            folder,
            name: name.into(),
        }
    }

    #[test]
    fn each_kind_of_node_gets_the_removals_it_can_answer() {
        let h2 = Dialect::H2;
        let table = object(Folder::Tables, "T");
        let view = object(Folder::Views, "V");
        let sequence = object(Folder::Sequences, "S");
        let schema = NodeId::Schema {
            connection: C,
            catalog: None,
            name: "APP".into(),
        };
        let catalog = NodeId::Catalog {
            connection: C,
            name: "shop".into(),
        };
        let one = |removal, node: &NodeId| removal_nodes(removal, &h2, node, &[]);

        assert_eq!(one(Removal::Drop, &table), std::slice::from_ref(&table));
        assert_eq!(one(Removal::Truncate, &table), std::slice::from_ref(&table));
        assert_eq!(one(Removal::Drop, &view), std::slice::from_ref(&view));
        assert!(one(Removal::Truncate, &view).is_empty());
        assert_eq!(
            one(Removal::Drop, &sequence),
            std::slice::from_ref(&sequence)
        );
        assert_eq!(one(Removal::Drop, &schema), std::slice::from_ref(&schema));
        assert!(one(Removal::Truncate, &schema).is_empty());
        for folder in [Folder::Procedures, Folder::Functions, Folder::Triggers] {
            let routine = object(folder, "R");
            assert_eq!(one(Removal::Drop, &routine), std::slice::from_ref(&routine));
            assert!(one(Removal::Truncate, &routine).is_empty());
        }
        // SQLite has triggers and no routines.
        let sqlite = |node: &NodeId| removal_nodes(Removal::Drop, &Dialect::SQLITE, node, &[]);
        assert!(sqlite(&object(Folder::Functions, "F")).is_empty());
        assert!(sqlite(&object(Folder::Procedures, "P")).is_empty());
        assert_eq!(sqlite(&object(Folder::Triggers, "T")).len(), 1);
        // H2's catalog is the database file; MySQL's is a database it can drop.
        assert!(one(Removal::Drop, &catalog).is_empty());
        assert_eq!(
            removal_nodes(Removal::Drop, &Dialect::MYSQL, &catalog, &[]),
            std::slice::from_ref(&catalog)
        );
        for node in [
            NodeId::Connection(C),
            NodeId::Folder {
                connection: C,
                scope: explorer::Scope::default(),
                folder: Folder::Tables,
            },
        ] {
            assert!(one(Removal::Drop, &node).is_empty());
        }
    }

    #[test]
    fn a_selection_goes_dependents_first_and_only_where_it_applies() {
        let h2 = Dialect::H2;
        let table = object(Folder::Tables, "T");
        let view = object(Folder::Views, "V");
        let sequence = object(Folder::Sequences, "S");
        let routine = object(Folder::Procedures, "P");
        let trigger = object(Folder::Triggers, "TR");
        let schema = NodeId::Schema {
            connection: C,
            catalog: None,
            name: "APP".into(),
        };
        let selection = [
            sequence.clone(),
            table.clone(),
            routine.clone(),
            view.clone(),
            trigger.clone(),
            schema.clone(),
        ];
        assert_eq!(
            removal_nodes(Removal::Drop, &h2, &table, &selection),
            [
                trigger.clone(),
                view.clone(),
                table.clone(),
                routine.clone(),
                sequence.clone()
            ]
        );
        assert_eq!(
            removal_nodes(Removal::Truncate, &h2, &table, &selection),
            std::slice::from_ref(&table)
        );
        // A schema is never dropped with company.
        assert_eq!(
            removal_nodes(Removal::Drop, &h2, &schema, &selection),
            std::slice::from_ref(&schema)
        );
        // A right-click outside the selection acts on what it pressed.
        let other = object(Folder::Tables, "OTHER");
        assert_eq!(
            removal_nodes(Removal::Drop, &h2, &other, &selection),
            std::slice::from_ref(&other)
        );

        assert_eq!(
            menu_label(Removal::Drop, &[view.clone(), table.clone()]),
            ts!("menu.drop_objects", count = 2)
        );
        assert_eq!(menu_label(Removal::Drop, &[view]), ts!("menu.drop_view"));
        assert_eq!(
            menu_label(Removal::Drop, &[routine]),
            ts!("menu.drop_procedure")
        );
        assert_eq!(
            menu_label(Removal::Drop, &[object(Folder::Functions, "F")]),
            ts!("menu.drop_function")
        );
        assert_eq!(
            menu_label(Removal::Drop, &[trigger]),
            ts!("menu.drop_trigger")
        );
        assert_eq!(
            menu_label(Removal::Drop, &[schema]),
            ts!("menu.drop_schema")
        );
        assert_eq!(
            menu_label(Removal::Truncate, &[table.clone(), table]),
            ts!("menu.truncate_tables", count = 2)
        );
    }

    fn found(arguments: Option<&str>, kind: Option<&str>) -> Found {
        Found {
            arguments: arguments.map(str::to_owned),
            routine_kind: kind.map(str::to_owned),
            ..Found::default()
        }
    }

    #[test]
    fn every_overload_a_lookup_finds_is_a_statement_of_its_own() {
        let pg = Dialect::POSTGRES;
        let function = NodeId::Object {
            connection: C,
            scope: explorer::Scope {
                catalog: Some("db".into()),
                schema: Some("app".into()),
            },
            folder: Folder::Functions,
            name: "total".into(),
        };
        assert_eq!(
            identified(
                &pg,
                Removal::Drop,
                &function,
                &[
                    found(Some("a integer"), Some("f")),
                    found(Some("a integer, b integer"), Some("f")),
                ]
            ),
            [
                "DROP FUNCTION app.total(a integer)",
                "DROP FUNCTION app.total(a integer, b integer)"
            ]
        );
        // Nothing found: the name alone, for the server to judge.
        assert_eq!(
            identified(&pg, Removal::Drop, &function, &[]),
            ["DROP FUNCTION app.total"]
        );
        // A truncate never looks anything up.
        assert!(identified(&pg, Removal::Truncate, &function, &[found(None, None)]).is_empty());
    }

    #[test]
    fn a_trigger_is_dropped_on_every_table_it_is_found_on() {
        let trigger = object(Folder::Triggers, "AUDIT");
        let on = |table: &str| Found {
            table: Some(table.into()),
            ..Found::default()
        };
        assert_eq!(
            identified(
                &Dialect::POSTGRES,
                Removal::Drop,
                &trigger,
                &[on("orders"), on("items")]
            ),
            [
                "DROP TRIGGER \"AUDIT\" ON \"APP\".orders",
                "DROP TRIGGER \"AUDIT\" ON \"APP\".items"
            ]
        );
        // PostgreSQL cannot drop one without its table.
        assert!(identified(&Dialect::POSTGRES, Removal::Drop, &trigger, &[]).is_empty());
        // Elsewhere the name is enough, and said once.
        assert_eq!(
            identified(
                &Dialect::ORACLE,
                Removal::Drop,
                &trigger,
                &[on("ORDERS"), on("ORDERS")]
            ),
            ["DROP TRIGGER APP.AUDIT"]
        );
    }

    #[test]
    fn an_oracle_routine_inside_a_package_is_not_dropped_alone() {
        let procedure = object(Folder::Procedures, "POST");
        let standalone = Found::default();
        let packaged = Found {
            catalog: Some("BILLING".into()),
            ..Found::default()
        };
        assert_eq!(
            identified(
                &Dialect::ORACLE,
                Removal::Drop,
                &procedure,
                &[packaged.clone(), standalone]
            ),
            ["DROP PROCEDURE APP.POST"]
        );
        assert!(
            identified(
                &Dialect::ORACLE,
                Removal::Drop,
                &procedure,
                std::slice::from_ref(&packaged)
            )
            .is_empty()
        );
        // Only Oracle's catalog is a package: MySQL's is the database.
        assert_eq!(
            identified(&Dialect::MYSQL, Removal::Drop, &procedure, &[packaged]),
            ["DROP PROCEDURE APP.POST"]
        );
    }
}
