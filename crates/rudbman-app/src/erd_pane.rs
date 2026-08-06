//! The ERD panel: one schema's tables and the foreign keys between them.
//!
//! Opened over a scope — a catalogue and a schema — from the explorer, and
//! shown in a pane of the work area. A toolbar, and under it the canvas
//! [`rudbman_erd::ErdView`] draws.
//!
//! # The widget draws; the panel owns everything else
//!
//! `rudbman-erd` knows about boxes, lines, dragging, panning and zooming, and
//! nothing about a session, a loading state, a translated string or a file
//! (architecture document, §7.6). All four are here, exactly as [`QueryPane`]
//! wraps `GridView`: this panel asks the workspace to fetch, hands the result
//! over with [`ErdView::set_model`], and turns the widget's one event —
//! [`ErdEvent::LayoutChanged`] — into a request that the workspace write
//! `erd/<profile-uuid>.json`. The widget raises that event once per gesture, so
//! the file is written once per gesture rather than once per frame.
//!
//! # Why the fetch is shaped the way it is
//!
//! [`load_model`] is four kinds of `DESCRIBE`, and the count of each is the
//! whole design:
//!
//! * `tables` and `columns` are **one round trip each for the whole scope**.
//!   JDBC's `getColumns` takes a table *pattern*, so a schema of two hundred
//!   tables costs one call rather than two hundred.
//! * `primary_keys` and `imported_keys` are **one per table**, because JDBC
//!   offers no schema-wide form of either.
//! * `exported_keys` is **never called**. It is the same edge looked at from
//!   the other end, and asking for both would draw every foreign key twice.
//!
//! An edge pointing at a table outside the scope is dropped rather than drawn
//! to a box that is not there. The *column* keeps its foreign-key mark, because
//! "this column references something" is true whether or not the something is
//! on screen.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use gpui::{
    App, Context, Entity, EventEmitter, FocusHandle, Focusable, IntoElement, Render, SharedString,
    Subscription, Window, div, prelude::*, px,
};
use rudbman_erd::{ErdColumn, ErdEvent, ErdModel, ErdRelation, ErdTable, ErdView};
use rudbman_jdbc::{DescribeRequest, Session};
use rudbman_ui::{Button, ButtonVariant, Theme, theme};

use crate::explorer::{ConnectionId, Scope};
use crate::i18n::ts;
use crate::table_detail::{flag, items, number, text, type_of};

/// The one `TABLE_TYPE` a diagram is drawn from.
///
/// Views have columns but no declared foreign keys, and the system catalogue's
/// own tables are not what anyone opens a diagram to look at.
const TABLE_TYPE: &str = "TABLE";

/// One `DESCRIBE` item, as the bridge hands it over.
type Item = serde_json::Map<String, serde_json::Value>;

/// What one diagram is drawn over: a connection, and a scope inside it.
///
/// The scope is the explorer's own [`Scope`], so "open the ERD of what is
/// selected" is a projection of the selected node rather than a second notion
/// of where a table lives. Two panels of the same connection and scope would
/// show exactly the same thing, which is why the workspace moves to the open
/// tab instead of opening a second one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ErdTarget {
    /// Which connection to ask.
    pub connection: ConnectionId,
    /// The catalogue and schema whose tables are drawn.
    pub scope: Scope,
}

impl ErdTarget {
    /// The scope as it reads in a title: `catalog.schema`, or whichever half
    /// there is.
    pub fn qualified(&self) -> String {
        [self.scope.catalog.as_deref(), self.scope.schema.as_deref()]
            .into_iter()
            .flatten()
            .filter(|part| !part.is_empty())
            .collect::<Vec<_>>()
            .join(".")
    }

    /// The title its tab carries.
    ///
    /// A scope that names neither a catalogue nor a schema — SQLite, and any
    /// product whose levels were all skipped — gets the bare word, because
    /// "ERD — " with nothing after it reads as a bug.
    pub fn title(&self) -> SharedString {
        let scope = self.qualified();
        if scope.is_empty() {
            ts!("erd.tab_all")
        } else {
            ts!("erd.tab", scope = scope)
        }
    }

    /// The name an exported file is suggested under.
    ///
    /// Never translated and stripped of separators: it is a file name, and a
    /// `catalog.schema` with a dot in it would look like an extension.
    fn file_stem(&self) -> String {
        let scope = self.qualified().replace(['.', '/', '\\', ':'], "_");
        if scope.is_empty() {
            "diagram".to_string()
        } else {
            scope
        }
    }
}

/// A diagram and the arrangement to draw it in.
///
/// The two travel together because they are fetched together — one background
/// task reads the catalogue and the saved layout — and because handing them to
/// [`ErdView::set_model`] separately would draw the grid layout for one frame
/// and then jump.
pub struct ErdDiagram {
    /// The tables and the foreign keys between them.
    pub model: ErdModel,
    /// Where the boxes were left, keyed by table name; empty on a first open.
    pub saved: HashMap<String, (f32, f32)>,
}

/// Where the panel's data has got to.
enum Load {
    /// A fetch is out.
    Running,
    /// It came back, with this many tables in it.
    Ready(usize),
    /// It failed; the driver's own message.
    Failed(SharedString),
}

/// What an export last reported.
struct Notice {
    /// The sentence shown in the toolbar.
    message: SharedString,
    /// Whether it is a failure, and so drawn in the danger colour.
    error: bool,
}

/// What the panel asks the workspace for.
pub enum ErdPaneEvent {
    /// Read this scope's tables and keys; the workspace has the session.
    Load(Box<ErdTarget>),
    /// The boxes have been moved and the arrangement is worth saving.
    ///
    /// Carries the target rather than the positions: the workspace reads those
    /// back through [`ErdPane::positions`] in the same update, and a message
    /// carrying a hundred entries per gesture would copy them for nothing.
    LayoutChanged(Box<ErdTarget>),
}

/// The panel.
pub struct ErdPane {
    /// What is being drawn.
    target: ErdTarget,
    /// Where the fetch has got to.
    load: Load,
    /// The canvas.
    view: Entity<ErdView>,
    focus_handle: FocusHandle,
    /// What the last export said, until the next one.
    notice: Option<Notice>,
    /// Keeps the canvas subscription alive.
    _events: Subscription,
}

impl ErdPane {
    /// Opens a panel over `target`, in its loading state.
    ///
    /// It does **not** ask for the model, for the reason
    /// [`TableDetail::new`](crate::table_detail::TableDetail::new) does not:
    /// an event emitted from a constructor has no subscriber yet — the host is
    /// still inside `cx.new` — and would be dropped, leaving a panel that says
    /// "loading…" for ever. The host subscribes and then calls
    /// [`ErdPane::refresh`].
    pub fn new(target: ErdTarget, cx: &mut Context<Self>) -> Self {
        let view = cx.new(ErdView::new);
        let events = cx.subscribe(&view, |pane, _view, event, cx| match event {
            ErdEvent::LayoutChanged => {
                cx.emit(ErdPaneEvent::LayoutChanged(Box::new(pane.target.clone())));
            }
        });

        Self {
            target,
            load: Load::Running,
            view,
            focus_handle: cx.focus_handle(),
            notice: None,
            _events: events,
        }
    }

    /// The scope this panel draws.
    pub fn target(&self) -> &ErdTarget {
        &self.target
    }

    /// Where every box is, which is what the workspace persists.
    pub fn positions(&self, cx: &App) -> HashMap<String, (f32, f32)> {
        self.view.read(cx).positions()
    }

    /// Asks for the model again.
    pub fn refresh(&mut self, cx: &mut Context<Self>) {
        self.load = Load::Running;
        self.notice = None;
        cx.emit(ErdPaneEvent::Load(Box::new(self.target.clone())));
        cx.notify();
    }

    /// Records what a fetch produced.
    pub fn deliver(&mut self, outcome: Result<ErdDiagram, SharedString>, cx: &mut Context<Self>) {
        match outcome {
            Ok(diagram) => {
                self.load = Load::Ready(diagram.model.tables.len());
                self.view.update(cx, |view, cx| {
                    view.set_model(diagram.model, diagram.saved, cx);
                });
            }
            Err(message) => self.load = Load::Failed(message),
        }
        cx.notify();
    }

    /// Whether a fetch is out.
    pub fn is_loading(&self) -> bool {
        matches!(self.load, Load::Running)
    }

    /// The failure a fetch reported, if it failed.
    #[cfg(test)]
    pub fn failure(&self) -> Option<&SharedString> {
        match &self.load {
            Load::Failed(message) => Some(message),
            _ => None,
        }
    }

    /// How many boxes are on the canvas, once it has loaded.
    #[cfg(test)]
    pub fn table_count(&self) -> Option<usize> {
        match self.load {
            Load::Ready(count) => Some(count),
            _ => None,
        }
    }

    /// Whether the keyboard is anywhere in the panel.
    ///
    /// Both handles, because the canvas takes the focus for itself when a box
    /// is pressed and [`Focusable`] can only name one of them — the same
    /// reason [`QueryPane::contains_focus`](crate::query::QueryPane::contains_focus)
    /// exists. A focus left on the canvas of a tab that stopped being rendered
    /// swallows every action from then on.
    pub fn contains_focus(&self, window: &Window, cx: &App) -> bool {
        self.focus_handle.contains_focused(window, cx)
            || self
                .view
                .read(cx)
                .focus_handle(cx)
                .contains_focused(window, cx)
    }

    /// Puts the keyboard in the panel: on the canvas when there is one.
    ///
    /// The canvas is where the zoom and auto-arrange chords are bound, so it is
    /// what the keyboard wants — but it is only in the element tree once a
    /// model has arrived, and gpui resolves actions against the focused element
    /// of the last drawn frame. Focusing it while the panel still says
    /// "loading…" would swallow every action until the fetch came back, which
    /// is exactly the hazard `Workspace::reclaim_focus` describes. So the
    /// panel's own root takes it in the meantime; it is always drawn.
    pub fn take_focus(&self, window: &mut Window, cx: &mut App) {
        let handle = match self.load {
            Load::Ready(count) if count > 0 => self.view.read(cx).focus_handle(cx),
            _ => self.focus_handle.clone(),
        };
        handle.focus(window);
    }

    /// Re-runs the automatic layout.
    ///
    /// The canvas announces the result itself, so the arrangement is saved
    /// without this having to say so.
    fn auto_arrange(&mut self, cx: &mut Context<Self>) {
        self.view.update(cx, |view, cx| view.auto_arrange(cx));
    }

    /// Asks where to write an SVG of the diagram, and writes it there.
    ///
    /// Nothing waits on the prompt, for the reason no picker in this
    /// application does: on X11 that call is the one gpui had to be patched
    /// around. The document itself is produced on the UI thread — it is a
    /// string built from the model, and it reads the theme — and only the file
    /// write goes to a background task.
    fn export(&mut self, cx: &mut Context<Self>) {
        let suggested = format!("{}.svg", self.target.file_stem());
        let prompt = cx.prompt_for_new_path(&default_directory(), Some(&suggested));

        cx.spawn(async move |pane, cx| {
            let path = match prompt.await {
                Ok(Ok(Some(path))) => path,
                Ok(Ok(None)) | Err(_) => return,
                Ok(Err(error)) => {
                    log::warn!("the save dialog could not be opened: {error:#}");
                    return;
                }
            };
            let Ok(svg) = pane.update(cx, |pane, cx| pane.view.read(cx).export_svg(cx)) else {
                return;
            };
            let written = cx
                .background_spawn(async move { std::fs::write(&path, svg).map(|()| path) })
                .await;
            pane.update(cx, |pane, cx| pane.exported(written, cx)).ok();
        })
        .detach();
    }

    /// Records how the export went.
    fn exported(&mut self, outcome: std::io::Result<PathBuf>, cx: &mut Context<Self>) {
        self.notice = Some(match outcome {
            Ok(path) => Notice {
                message: ts!("erd.export_done", path = path.display().to_string()),
                error: false,
            },
            Err(error) => {
                // Logged as well as shown: the panel's strip is one line, and
                // an I/O failure is worth the detail in the log.
                log::error!("the diagram could not be written: {error}");
                Notice {
                    message: ts!("erd.export_failed", error = error.to_string()),
                    error: true,
                }
            }
        });
        cx.notify();
    }

    /// The toolbar: what is being drawn, what it reports, and the three
    /// buttons.
    fn render_toolbar(&self, chrome: &Theme, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let this = cx.entity();
        let loading = self.is_loading();
        let ready = matches!(self.load, Load::Ready(_));

        div()
            .flex()
            .flex_row()
            .flex_none()
            .items_center()
            .gap(px(8.))
            .h(px(30.))
            .px(px(10.))
            .border_b_1()
            .border_color(chrome.border)
            .child(
                div()
                    .flex_none()
                    .text_size(px(13.))
                    .text_color(chrome.text)
                    .child(self.target.title()),
            )
            .child(div().flex_1().min_w_0())
            .when(loading, |bar| {
                bar.child(
                    div()
                        .flex_none()
                        .text_size(px(11.))
                        .text_color(chrome.text_muted)
                        .child(ts!("erd.loading")),
                )
            })
            .children(self.notice.as_ref().map(|notice| {
                div()
                    .flex_none()
                    .max_w(px(360.))
                    .truncate()
                    .text_size(px(11.))
                    .text_color(if notice.error {
                        chrome.danger
                    } else {
                        chrome.text_muted
                    })
                    .child(notice.message.clone())
            }))
            .child({
                let this = this.clone();
                Button::new("erd-arrange", ts!("erd.auto_arrange"))
                    .variant(ButtonVariant::Secondary)
                    .disabled(!ready)
                    .on_click(move |_, _window, cx| {
                        this.update(cx, |pane, cx| pane.auto_arrange(cx));
                    })
            })
            .child({
                let this = this.clone();
                Button::new("erd-export", ts!("erd.export_svg"))
                    .variant(ButtonVariant::Secondary)
                    .disabled(!ready)
                    .on_click(move |_, _window, cx| {
                        this.update(cx, |pane, cx| pane.export(cx));
                    })
            })
            .child(
                Button::new("erd-refresh", ts!("erd.refresh"))
                    .variant(ButtonVariant::Secondary)
                    .disabled(loading)
                    .on_click(move |_, _window, cx| {
                        this.update(cx, |pane, cx| pane.refresh(cx));
                    }),
            )
    }

    /// The body: the canvas, or the one line that stands in for it.
    fn render_body(&self, chrome: &Theme) -> gpui::AnyElement {
        match &self.load {
            Load::Running => note(ts!("erd.loading"), chrome.text_muted).into_any_element(),
            Load::Failed(message) => note(
                ts!("erd.load_failed", error = message.to_string()),
                chrome.danger,
            )
            .into_any_element(),
            // A schema with no tables draws an empty canvas, which reads as a
            // diagram that failed rather than as a schema with nothing in it.
            Load::Ready(0) => note(ts!("erd.empty"), chrome.text_muted).into_any_element(),
            Load::Ready(_) => self.view.clone().into_any_element(),
        }
    }
}

impl EventEmitter<ErdPaneEvent> for ErdPane {}

impl Focusable for ErdPane {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for ErdPane {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let chrome = theme(cx);
        let toolbar = self.render_toolbar(&chrome, cx);
        let body = self.render_body(&chrome);

        div()
            .id("erd-pane")
            .track_focus(&self.focus_handle)
            .flex()
            .flex_col()
            .size_full()
            .min_w_0()
            .min_h_0()
            .child(toolbar)
            .child(div().flex().flex_1().min_w_0().min_h_0().child(body))
    }
}

/// A one-line message where the canvas would be.
fn note(message: SharedString, color: gpui::Hsla) -> impl IntoElement {
    div()
        .p(px(10.))
        .text_size(px(12.))
        .text_color(color)
        .child(message)
}

/// Where the save dialog opens when nothing has been exported yet.
///
/// The user's home directory, and the working directory when even that cannot
/// be resolved — the same choice the extraction dialog makes, and for the same
/// reason: a relative path is resolved against something the user did not pick.
fn default_directory() -> PathBuf {
    directories::UserDirs::new()
        .map(|dirs| dirs.home_dir().to_path_buf())
        .unwrap_or_else(|| PathBuf::from("."))
}

/// Reads one scope's tables, columns and foreign keys, and assembles a diagram.
///
/// **Blocks**, and is called from `cx.background_spawn` with a
/// [`SessionHandle`](crate::connection::SessionHandle). See the module
/// documentation for what is asked for and how often.
///
/// Everything about the answer is ordered: tables by name, columns by ordinal,
/// a composite key's pairs by `KEY_SEQ`, and the relations by the table they
/// leave from. That is not tidiness — the box positions saved in
/// `erd/<profile-uuid>.json` are keyed by table name, and a layout that came
/// out differently on the next fetch would make the saved file look wrong.
pub fn load_model(session: &Session, target: &ErdTarget) -> Result<ErdModel, String> {
    let scope = &target.scope;
    let scoped = |kind: &str| {
        let mut request = DescribeRequest::new(kind);
        request.catalog = scope.catalog.clone();
        request.schema = scope.schema.clone();
        request
    };

    let mut names: Vec<String> = {
        let mut request = scoped("tables");
        request.types = Some(vec![TABLE_TYPE.to_string()]);
        items(session, &request)?
            .iter()
            .filter_map(|table| text(table, "name"))
            .map(str::to_owned)
            .collect()
    };
    names.sort();
    // A driver that lists one table under two types would otherwise draw it
    // twice, and `ErdView::positions` is keyed by name.
    names.dedup();

    // One call for every column of the scope. `getColumns` takes a table
    // pattern, and leaving it unset is what makes this one round trip instead
    // of one per table.
    let columns = items(session, &scoped("columns"))?;
    let mut by_table: HashMap<&str, Vec<&Item>> = HashMap::new();
    for column in &columns {
        let Some(table) = text(column, "table") else {
            continue;
        };
        by_table.entry(table).or_default().push(column);
    }
    for group in by_table.values_mut() {
        group.sort_by(|left, right| {
            let ordinal = |column: &Item| number(column, "ordinal").unwrap_or(i64::MAX);
            ordinal(left)
                .cmp(&ordinal(right))
                // A driver that reported no ordinal at all still has to produce
                // the same order twice running.
                .then_with(|| text(left, "name").cmp(&text(right, "name")))
        });
    }

    let mut tables: Vec<ErdTable> = Vec::with_capacity(names.len());
    let mut edges: Vec<Edge> = Vec::new();
    for name in &names {
        let keyed = |kind: &str| {
            let mut request = scoped(kind);
            request.table = Some(name.clone());
            request
        };

        let primary: HashSet<String> = items(session, &keyed("primary_keys"))?
            .iter()
            .filter_map(|key| text(key, "column"))
            .map(str::to_owned)
            .collect();

        let imported = items(session, &keyed("imported_keys"))?;
        let fk_columns: HashSet<&str> = imported
            .iter()
            .filter_map(|key| text(key, "fk_column"))
            .collect();

        let columns = by_table
            .get(name.as_str())
            .map(|group| {
                group
                    .iter()
                    .filter_map(|column| {
                        let column_name = text(column, "name")?;
                        Some(ErdColumn {
                            name: column_name.to_string(),
                            type_name: type_of(column),
                            nullable: flag(column, "is_nullable").unwrap_or(true),
                            primary_key: primary.contains(column_name),
                            foreign_key: fk_columns.contains(column_name),
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();

        tables.push(ErdTable {
            name: name.clone(),
            columns,
        });
        edges.extend(edges_of(name, &imported, scope));
    }

    let model = ErdModel {
        tables,
        relations: Vec::new(),
    };
    let relations = edges
        .into_iter()
        .filter_map(|edge| {
            // An edge pointing out of the scope has no box to end on; §7.6 says
            // to drop it rather than draw a line into nothing.
            let from = model.index_of(&edge.from)?;
            let to = model.index_of(&edge.to)?;
            Some(ErdRelation {
                name: edge.name,
                from,
                to,
                columns: edge.columns,
            })
        })
        .collect();

    Ok(ErdModel { relations, ..model })
}

/// One foreign key of one table, before its endpoints have been resolved.
struct Edge {
    /// The constraint name, when the driver reported one.
    name: Option<String>,
    /// The table holding the key.
    from: String,
    /// The table it references.
    to: String,
    /// `(foreign key column, referenced column)`, in key order.
    columns: Vec<(String, String)>,
}

/// Groups one table's `imported_keys` rows into one edge per constraint.
///
/// JDBC answers one *row per column*, so a composite key arrives as several
/// rows that only `FK_NAME` and `KEY_SEQ` tie together. A driver that reports
/// no `FK_NAME` — several do — is grouped by the table pair instead, which is
/// right except for the rare schema declaring two separate keys between the
/// same two tables; those merge into one line, which draws correctly and
/// merely under-counts the constraints.
///
/// A reference into another schema is dropped here rather than left to the
/// index lookup: two schemas routinely hold a table of the same name, and
/// matching on the name alone would draw an edge to the wrong box.
fn edges_of(table: &str, rows: &[Item], scope: &Scope) -> Vec<Edge> {
    /// The rows of one constraint, before they have been put in key order.
    struct Group {
        /// `FK_NAME`, when the driver reported one.
        name: Option<String>,
        /// The table referenced, which is what groups a nameless key.
        to: String,
        /// `(KEY_SEQ, foreign key column, referenced column)`.
        pairs: Vec<(i64, String, String)>,
    }

    let mut groups: Vec<Group> = Vec::new();

    for row in rows {
        let (Some(pk_table), Some(pk_column), Some(fk_column)) = (
            text(row, "pk_table"),
            text(row, "pk_column"),
            text(row, "fk_column"),
        ) else {
            continue;
        };
        if !in_scope(row, scope) {
            continue;
        }

        let name = text(row, "fk_name").map(str::to_owned);
        let pair = (
            number(row, "seq").unwrap_or(i64::MAX),
            fk_column.to_string(),
            pk_column.to_string(),
        );
        match groups
            .iter_mut()
            .find(|group| group.name == name && group.to == pk_table)
        {
            Some(group) => group.pairs.push(pair),
            None => groups.push(Group {
                name,
                to: pk_table.to_string(),
                pairs: vec![pair],
            }),
        }
    }

    // By the table referenced and then by the constraint name, so that two runs
    // over the same schema produce the same list whatever order the driver
    // answered in.
    groups.sort_by(|left, right| {
        left.to
            .cmp(&right.to)
            .then_with(|| left.name.cmp(&right.name))
    });

    groups
        .into_iter()
        .map(|mut group| {
            group
                .pairs
                .sort_by(|left, right| left.0.cmp(&right.0).then_with(|| left.1.cmp(&right.1)));
            Edge {
                name: group.name,
                from: table.to_string(),
                to: group.to,
                columns: group
                    .pairs
                    .into_iter()
                    .map(|(_, fk_column, pk_column)| (fk_column, pk_column))
                    .collect(),
            }
        })
        .collect()
}

/// Whether the table an `imported_keys` row points at is inside `scope`.
///
/// A half the scope does not name is not compared: a product whose catalogue
/// level was skipped answers `PKTABLE_CAT` all the same, and refusing the edge
/// over that would empty the diagram of lines.
fn in_scope(row: &Item, scope: &Scope) -> bool {
    let matches = |wanted: Option<&String>, reported: Option<&str>| match (wanted, reported) {
        (Some(wanted), Some(reported)) => wanted == reported,
        _ => true,
    };
    matches(scope.catalog.as_ref(), text(row, "pk_catalog"))
        && matches(scope.schema.as_ref(), text(row, "pk_schema"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn target(schema: &str) -> ErdTarget {
        ErdTarget {
            connection: ConnectionId(1),
            scope: Scope {
                catalog: None,
                schema: Some(schema.to_string()),
            },
        }
    }

    /// The table of `model` called `name`.
    fn table<'a>(model: &'a ErdModel, name: &str) -> &'a ErdTable {
        model
            .tables
            .iter()
            .find(|table| table.name == name)
            .unwrap_or_else(|| panic!("no table {name} in {:?}", model.tables))
    }

    /// The column of `table` called `name`.
    fn column<'a>(table: &'a ErdTable, name: &str) -> &'a ErdColumn {
        table
            .columns
            .iter()
            .find(|column| column.name == name)
            .unwrap_or_else(|| panic!("no column {name} in {:?}", table.columns))
    }

    /// The whole loader against a real product: two tables, one foreign key
    /// between them, and the marks a box is drawn with.
    #[test]
    fn a_schema_with_a_foreign_key_becomes_two_boxes_and_one_line() {
        let connected = crate::explorer::tests::h2_fixture("erd-model");
        let model = load_model(connected.session(), &target("APP")).expect("H2 answers");

        // The view is not a table and the sequence is not either, so the
        // fixture's four objects come down to two boxes.
        let names: Vec<&str> = model
            .tables
            .iter()
            .map(|table| table.name.as_str())
            .collect();
        assert_eq!(names, ["PERSON", "TEAM"], "sorted, and tables only");

        let person = table(&model, "PERSON");
        // Columns in the order the catalogue reported them, typed the way the
        // detail panel types them.
        let columns: Vec<&str> = person
            .columns
            .iter()
            .map(|column| column.name.as_str())
            .collect();
        assert_eq!(columns, ["ID", "TEAM_ID", "EMAIL", "SALARY"]);
        assert_eq!(column(person, "SALARY").type_name, "NUMERIC(12,2)");
        assert_eq!(column(person, "EMAIL").type_name, "CHARACTER VARYING(200)");

        // The two marks the canvas colours a row by.
        assert!(column(person, "ID").primary_key);
        assert!(!column(person, "ID").foreign_key);
        assert!(column(person, "TEAM_ID").foreign_key);
        assert!(!column(person, "TEAM_ID").primary_key);
        assert!(!column(person, "TEAM_ID").nullable, "declared NOT NULL");
        assert!(column(person, "EMAIL").nullable);

        // One relation, and it points the way JDBC does: from the table that
        // holds the key to the table whose key is referenced.
        assert_eq!(model.relations.len(), 1, "{:?}", model.relations);
        let relation = &model.relations[0];
        assert_eq!(model.tables[relation.from].name, "PERSON");
        assert_eq!(model.tables[relation.to].name, "TEAM");
        assert_eq!(
            relation.columns,
            vec![("TEAM_ID".to_string(), "ID".to_string())]
        );
        assert_eq!(relation.name.as_deref(), Some("FK_PERSON_TEAM"));

        // And both ends are inside the model, which is what the canvas draws.
        assert_eq!(model.valid_relations().count(), 1);
    }

    /// The same fetch twice has to produce the same thing, or a saved layout
    /// would stop matching the diagram it was saved for.
    #[test]
    fn the_same_schema_loads_the_same_way_twice() {
        let connected = crate::explorer::tests::h2_fixture("erd-determinism");
        let once = load_model(connected.session(), &target("APP")).expect("H2 answers");
        let twice = load_model(connected.session(), &target("APP")).expect("H2 answers");
        assert_eq!(once, twice);
    }

    /// A schema with nothing in it is a diagram with no boxes, not a failure.
    #[test]
    fn an_unknown_schema_is_an_empty_diagram() {
        let connected = crate::explorer::tests::h2_fixture("erd-empty");
        let model =
            load_model(connected.session(), &target("NOSUCHSCHEMA")).expect("an empty scope loads");
        assert!(model.tables.is_empty(), "{:?}", model.tables);
        assert!(model.relations.is_empty());
    }

    #[test]
    fn an_edge_out_of_the_scope_is_dropped() {
        let scope = Scope {
            catalog: None,
            schema: Some("APP".to_string()),
        };
        let row = |pk_schema: &str| -> Item {
            serde_json::from_str(&format!(
                r#"{{"pk_schema":"{pk_schema}","pk_table":"TEAM","pk_column":"ID",
                    "fk_table":"PERSON","fk_column":"TEAM_ID","seq":1,
                    "fk_name":"FK_PERSON_TEAM"}}"#
            ))
            .expect("parses")
        };

        assert_eq!(edges_of("PERSON", &[row("APP")], &scope).len(), 1);
        assert!(
            edges_of("PERSON", &[row("OTHER")], &scope).is_empty(),
            "a key into another schema was kept"
        );
    }

    #[test]
    fn a_composite_key_is_one_edge_with_its_pairs_in_key_order() {
        let scope = Scope::default();
        let row = |seq: i64, fk: &str, pk: &str| -> Item {
            serde_json::from_str(&format!(
                r#"{{"pk_table":"PARENT","pk_column":"{pk}","fk_table":"CHILD",
                    "fk_column":"{fk}","seq":{seq},"fk_name":"FK_CHILD_PARENT"}}"#
            ))
            .expect("parses")
        };

        // Handed over in the wrong order on purpose: `KEY_SEQ` is what orders
        // the pairs, not the order the driver answered in.
        let edges = edges_of("CHILD", &[row(2, "B_ID", "B"), row(1, "A_ID", "A")], &scope);
        assert_eq!(edges.len(), 1, "a composite key became two lines");
        assert_eq!(
            edges[0].columns,
            vec![
                ("A_ID".to_string(), "A".to_string()),
                ("B_ID".to_string(), "B".to_string())
            ]
        );
    }

    #[test]
    fn keys_with_no_name_are_grouped_by_the_table_they_point_at() {
        let scope = Scope::default();
        let row = |pk_table: &str, fk: &str| -> Item {
            serde_json::from_str(&format!(
                r#"{{"pk_table":"{pk_table}","pk_column":"ID","fk_table":"CHILD",
                    "fk_column":"{fk}","seq":1}}"#
            ))
            .expect("parses")
        };

        let edges = edges_of(
            "CHILD",
            &[row("PARENT", "PARENT_ID"), row("OTHER", "OTHER_ID")],
            &scope,
        );
        assert_eq!(edges.len(), 2, "two targets must not merge into one line");
        // Sorted by the table referenced, which is what makes the list the same
        // on the next fetch.
        assert_eq!(edges[0].to, "OTHER");
        assert_eq!(edges[1].to, "PARENT");
    }

    #[test]
    fn a_scope_with_nothing_in_it_still_titles_its_tab() {
        let bare = ErdTarget {
            connection: ConnectionId(1),
            scope: Scope::default(),
        };
        assert_eq!(bare.qualified(), "");
        assert_eq!(bare.title(), ts!("erd.tab_all"));
        assert_eq!(bare.file_stem(), "diagram");

        let scoped = ErdTarget {
            connection: ConnectionId(1),
            scope: Scope {
                catalog: Some("app".to_string()),
                schema: Some("public".to_string()),
            },
        };
        assert_eq!(scoped.qualified(), "app.public");
        assert!(scoped.title().contains("app.public"), "{}", scoped.title());
        assert_eq!(scoped.file_stem(), "app_public");
    }

    #[gpui::test]
    fn opening_a_panel_leaves_it_loading_until_the_host_asks(cx: &mut gpui::TestAppContext) {
        cx.update(rudbman_ui::init);
        let panel = cx.new(|cx| ErdPane::new(target("PUBLIC"), cx));
        cx.update(|cx| {
            assert!(panel.read(cx).is_loading());
            assert!(panel.read(cx).table_count().is_none());
        });
    }

    /// The request has to reach a subscriber registered *after* the panel was
    /// built, which is the only order a host can manage — and the reason `new`
    /// does not emit.
    #[gpui::test]
    fn the_request_reaches_a_subscription_registered_after_construction(
        cx: &mut gpui::TestAppContext,
    ) {
        cx.update(rudbman_ui::init);
        let seen = std::rc::Rc::new(std::cell::RefCell::new(Vec::<String>::new()));
        let panel = cx.new(|cx| ErdPane::new(target("PUBLIC"), cx));

        let recorder = std::rc::Rc::clone(&seen);
        let _subscription = cx.update(|cx| {
            cx.subscribe(&panel, move |_panel, event, _cx| {
                if let ErdPaneEvent::Load(target) = event {
                    recorder.borrow_mut().push(target.qualified());
                }
            })
        });
        cx.update(|cx| panel.update(cx, |panel, cx| panel.refresh(cx)));
        cx.run_until_parked();

        assert_eq!(seen.borrow().as_slice(), ["PUBLIC".to_string()]);
    }

    /// The canvas raises one event per gesture and the panel passes it on, so
    /// the host writes the file once per gesture.
    #[gpui::test]
    fn a_rearrangement_is_passed_on_for_the_host_to_save(cx: &mut gpui::TestAppContext) {
        cx.update(rudbman_ui::init);
        let seen = std::rc::Rc::new(std::cell::RefCell::new(0usize));
        let panel = cx.new(|cx| ErdPane::new(target("APP"), cx));

        let recorder = std::rc::Rc::clone(&seen);
        let _subscription = cx.update(|cx| {
            cx.subscribe(&panel, move |_panel, event, _cx| {
                if let ErdPaneEvent::LayoutChanged(_) = event {
                    *recorder.borrow_mut() += 1;
                }
            })
        });

        cx.update(|cx| {
            panel.update(cx, |panel, cx| {
                panel.deliver(
                    Ok(ErdDiagram {
                        model: ErdModel {
                            tables: vec![
                                ErdTable::new("TEAM")
                                    .column(ErdColumn::new("ID", "INTEGER").primary_key()),
                                ErdTable::new("PERSON")
                                    .column(ErdColumn::new("TEAM_ID", "INTEGER").foreign_key()),
                            ],
                            relations: Vec::new(),
                        },
                        saved: HashMap::new(),
                    }),
                    cx,
                );
                panel.auto_arrange(cx);
            });
        });
        cx.run_until_parked();

        assert_eq!(*seen.borrow(), 1, "one gesture, one save");
        cx.update(|cx| {
            let panel = panel.read(cx);
            assert_eq!(panel.table_count(), Some(2));
            assert_eq!(panel.positions(cx).len(), 2);
        });
    }

    #[gpui::test]
    fn a_failed_load_is_reported_and_not_mistaken_for_an_empty_schema(
        cx: &mut gpui::TestAppContext,
    ) {
        cx.update(rudbman_ui::init);
        let panel = cx.new(|cx| ErdPane::new(target("PUBLIC"), cx));
        cx.update(|cx| {
            panel.update(cx, |panel, cx| {
                panel.deliver(Err("permission denied".into()), cx);
            });
        });
        cx.update(|cx| {
            let panel = panel.read(cx);
            assert!(!panel.is_loading());
            assert!(panel.table_count().is_none());
            assert_eq!(
                panel.failure().map(SharedString::as_ref),
                Some("permission denied")
            );
        });
    }

    #[test]
    fn every_label_the_panel_draws_has_a_translation() {
        for label in [
            ts!("erd.tab", scope = "APP"),
            ts!("erd.tab_all"),
            ts!("erd.loading"),
            ts!("erd.load_failed", error = "e"),
            ts!("erd.empty"),
            ts!("erd.auto_arrange"),
            ts!("erd.export_svg"),
            ts!("erd.export_done", path = "/tmp/a.svg"),
            ts!("erd.export_failed", error = "e"),
            ts!("erd.refresh"),
            ts!("menu.erd"),
        ] {
            assert!(!label.is_empty(), "empty label");
            assert!(!label.starts_with("erd."), "untranslated {label:?}");
            assert!(!label.starts_with("menu."), "untranslated {label:?}");
        }
    }
}
