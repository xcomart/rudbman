//! The query builder panel: a canvas of tables, a form under it, and the
//! `SELECT` the two of them describe.
//!
//! Opened per connection and numbered like a query pane, because several of
//! them at once is the ordinary case: one builder per question being asked.
//! Tables arrive from the explorer, either through "add to builder" or by
//! dragging the row onto the canvas — two gestures, one path: both end in the
//! workspace reading the column list and calling [`BuilderPane::add_table`].
//! There is no reverse direction, and §7.7 says there never will be: parsing
//! SQL back into a picture is a larger program than this whole tool.
//!
//! A drop says only *what* was dropped ([`BuilderPaneEvent::TableDropped`]) and
//! on which panel, because the session that would answer for the columns is the
//! workspace's; what a drop settles that the action cannot is which builder the
//! table belongs on, since the pointer named one.
//!
//! # The widget draws; the panel owns the query
//!
//! `rudbman_erd::BuilderView` knows about boxes, rows, drags and joins as
//! *lines*, and nothing about what a join means. Everything that makes a
//! statement — which columns are picked and in what order, what type each join
//! is, what the `WHERE` rows say, what is grouped and what is sorted — is here,
//! and the canvas is handed a projection of it. That is the same division
//! [`ErdPane`](crate::erd_pane::ErdPane) has with `ErdView`, and it is what
//! lets the generator ([`crate::builder_sql`]) be tested without a window.
//!
//! The canvas answers gestures with three events. A column click toggles that
//! column in the select list; a drag from one column to another adds an
//! `INNER` join, which the form can then change or delete; a box moved says
//! nothing this panel has to act on, because a builder saves no layout — its
//! output is SQL, and the editor and the filesystem already know how to keep
//! that.
//!
//! # Why the panel does not run anything
//!
//! "Open in editor" emits [`BuilderPaneEvent::OpenSql`] and the workspace puts
//! the text through `open_query`, the one gate every new query pane comes
//! through. Running, cancelling, paging and the write confirmation are that
//! pane's, already built and already tested; a second execution path inside the
//! builder would be a second set of all of them.

use std::collections::HashSet;

use gpui::{
    App, Context, Entity, EventEmitter, FocusHandle, Focusable, IntoElement, Render, ScrollHandle,
    SharedString, Subscription, Window, div, prelude::*, px,
};
use rudbman_erd::{BuilderEdge, BuilderEvent, BuilderView, ErdColumn, ErdTable};
use rudbman_jdbc::{DescribeRequest, Session};
use rudbman_sql::Dialect;
use rudbman_ui::{Button, ButtonVariant, Checkbox, Select, TextInput, Theme, theme};

use crate::builder_sql::{
    BuilderQuery, BuilderTable, Join, JoinKind, SortDir, generate, unique_alias,
};
use crate::explorer::{ConnectionId, DraggedObject, ObjectTarget};
use crate::i18n::ts;
use crate::table_detail::{items, number, text, type_of};

/// Height of the form under the canvas, in logical pixels.
///
/// Fixed rather than a share of the pane: the canvas is what grows when the
/// window does, because a form of six rows does not read better at twice the
/// height while a diagram always does.
const FORM_HEIGHT: f32 = 260.;

/// Placeholder in a `WHERE` row.
///
/// A sample condition rather than a sentence, so it is not translated — the
/// same choice the extraction dialog makes for its own `WHERE` box.
const WHERE_PLACEHOLDER: &str = "id > 1000";

/// What the panel asks the workspace for.
pub enum BuilderPaneEvent {
    /// Put this statement in a new query pane.
    OpenSql(String),
    /// An explorer row was dropped on this panel's canvas.
    ///
    /// The panel asks rather than loads: reading the column list needs the
    /// session, and the session belongs to the workspace. What it does say is
    /// *which* builder the table belongs on — the one the pointer was over,
    /// which is this one.
    TableDropped(ObjectTarget),
}

/// The panel.
pub struct BuilderPane {
    /// Which connection the tables come from, and which one the statement will
    /// be run against.
    connection: ConnectionId,
    /// The dialect every identifier is quoted for: the driver's, resolved once
    /// when the tab is opened.
    dialect: Dialect,
    /// The canvas.
    view: Entity<BuilderView>,
    /// The tables, in the order they were added.
    tables: Vec<BuilderTable>,
    /// The same tables as the canvas draws them, keyed by alias so that the
    /// same table added twice is two boxes.
    boxes: Vec<ErdTable>,
    /// The joins, in the order they were drawn.
    joins: Vec<Join>,
    /// The picked columns, in the order they were picked.
    selected: Vec<(usize, usize)>,
    /// One text field per `WHERE` row.
    where_rows: Vec<Entity<TextInput>>,
    /// The columns grouped by, a subset of [`BuilderPane::selected`].
    group_by: Vec<(usize, usize)>,
    /// The columns sorted by, likewise.
    order_by: Vec<((usize, usize), SortDir)>,
    /// Which join row's type dropdown is showing its list.
    open_join: Option<usize>,
    /// Which column row's sort dropdown is.
    open_order: Option<usize>,
    /// Vertical scroll of the form.
    body_scroll: ScrollHandle,
    focus_handle: FocusHandle,
    /// Keeps the canvas subscription alive.
    _events: Subscription,
    /// Keeps one observation per `WHERE` field alive, so that typing in one
    /// redraws the preview. Parallel to [`BuilderPane::where_rows`].
    condition_events: Vec<Subscription>,
}

impl BuilderPane {
    /// An empty builder over `connection`, quoting for `driver_dialect`.
    ///
    /// The dialect is the *driver's*, as it is for a query pane: a profile
    /// names a driver and a driver names a dialect.
    pub fn new(connection: ConnectionId, driver_dialect: &str, cx: &mut Context<Self>) -> Self {
        let view = cx.new(BuilderView::new);
        let events = cx.subscribe(&view, |pane, _view, event, cx| match event {
            BuilderEvent::ColumnToggled { table, column } => {
                pane.toggle_column(*table, *column, cx)
            }
            BuilderEvent::JoinDrawn { from, to } => pane.add_join(*from, *to, cx),
            // A box that moved changes the picture and not the statement, and
            // the picture is not saved.
            BuilderEvent::LayoutChanged => {}
        });

        Self {
            connection,
            dialect: Dialect::from_id(driver_dialect),
            view,
            tables: Vec::new(),
            boxes: Vec::new(),
            joins: Vec::new(),
            selected: Vec::new(),
            where_rows: Vec::new(),
            group_by: Vec::new(),
            order_by: Vec::new(),
            open_join: None,
            open_order: None,
            body_scroll: ScrollHandle::new(),
            focus_handle: cx.focus_handle(),
            _events: events,
            condition_events: Vec::new(),
        }
    }

    /// Which connection this builder belongs to.
    pub fn connection(&self) -> ConnectionId {
        self.connection
    }

    /// How many tables are on the canvas.
    #[cfg(test)]
    pub fn table_count(&self) -> usize {
        self.tables.len()
    }

    /// Puts `target` on the canvas with `columns` in it.
    ///
    /// The alias is the table's own name, or `name_2` when that is taken —
    /// which is what makes adding the same table twice a self-join rather than
    /// a collision. The canvas keys its boxes by name and requires them to be
    /// unique, so it is the alias that is handed over as the box's name.
    pub fn add_table(
        &mut self,
        target: &ObjectTarget,
        columns: Vec<ErdColumn>,
        cx: &mut Context<Self>,
    ) {
        let taken: Vec<String> = self
            .tables
            .iter()
            .map(|table| table.alias.clone())
            .collect();
        let alias = unique_alias(&target.name, &taken);

        self.tables.push(BuilderTable {
            catalog: target.catalog.clone(),
            schema: target.schema.clone(),
            name: target.name.clone(),
            alias: alias.clone(),
            columns: columns.iter().map(|column| column.name.clone()).collect(),
        });
        self.boxes.push(ErdTable {
            name: alias,
            columns,
        });

        let boxes = self.boxes.clone();
        self.view.update(cx, |view, cx| view.set_tables(boxes, cx));
        // The canvas remaps its own selection and edges by name when the table
        // list is replaced; handing both back keeps its view of them the same
        // as this panel's rather than merely equivalent.
        self.sync_selection(cx);
        self.sync_edges(cx);
        cx.notify();
    }

    /// Adds a join between two columns, or does nothing when one is already
    /// there.
    ///
    /// Either direction counts as the same join: the edge says which columns
    /// match, and drawing it back the other way is the same statement.
    pub fn add_join(&mut self, from: (usize, usize), to: (usize, usize), cx: &mut Context<Self>) {
        let known = self.joins.iter().any(|join| {
            (join.from == from && join.to == to) || (join.from == to && join.to == from)
        });
        if known {
            return;
        }
        self.joins.push(Join {
            from,
            to,
            kind: JoinKind::Inner,
        });
        self.sync_edges(cx);
        cx.notify();
    }

    /// Picks a column, or takes it out of the select list again.
    ///
    /// A column that leaves the list takes its `GROUP BY` and `ORDER BY` with
    /// it: both are edited per picked column, and a term over a column the
    /// statement no longer selects is one the user cannot see or undo.
    pub fn toggle_column(&mut self, table: usize, column: usize, cx: &mut Context<Self>) {
        let at = (table, column);
        match self.selected.iter().position(|picked| *picked == at) {
            Some(index) => {
                self.selected.remove(index);
                self.group_by.retain(|grouped| *grouped != at);
                self.order_by.retain(|(sorted, _)| *sorted != at);
                self.open_order = None;
            }
            None => self.selected.push(at),
        }
        self.sync_selection(cx);
        cx.notify();
    }

    /// Changes one join's type.
    pub fn set_join_kind(&mut self, index: usize, kind: JoinKind, cx: &mut Context<Self>) {
        let Some(join) = self.joins.get_mut(index) else {
            return;
        };
        join.kind = kind;
        cx.notify();
    }

    /// Removes one join.
    pub fn remove_join(&mut self, index: usize, cx: &mut Context<Self>) {
        if index >= self.joins.len() {
            return;
        }
        self.joins.remove(index);
        self.open_join = None;
        self.sync_edges(cx);
        cx.notify();
    }

    /// Appends an empty `WHERE` row.
    pub fn add_condition(&mut self, cx: &mut Context<Self>) {
        let input = cx.new(|cx| TextInput::new(cx).placeholder(WHERE_PLACEHOLDER));
        // Observed rather than left alone: the preview is rebuilt from the
        // fields every frame, and without this the panel would not redraw when
        // one of them is typed into.
        let watch = cx.observe(&input, |_pane, _input, cx| cx.notify());
        self.where_rows.push(input);
        self.condition_events.push(watch);
        cx.notify();
    }

    /// Removes one `WHERE` row.
    pub fn remove_condition(&mut self, index: usize, cx: &mut Context<Self>) {
        if index >= self.where_rows.len() {
            return;
        }
        self.where_rows.remove(index);
        // Dropping the observation is the point: the field it watched is gone.
        drop(self.condition_events.remove(index));
        cx.notify();
    }

    /// Sets one `WHERE` row's text.
    ///
    /// The panel's own way in for callers with no keyboard — the workspace's
    /// tests — and the same path typing takes.
    #[cfg(test)]
    pub fn set_condition(&mut self, index: usize, text: &str, cx: &mut Context<Self>) {
        let Some(input) = self.where_rows.get(index).cloned() else {
            return;
        };
        input.update(cx, |input, cx| input.set_content(text.to_owned(), cx));
        cx.notify();
    }

    /// Groups by one picked column, or stops doing so.
    pub fn toggle_group_by(&mut self, at: (usize, usize), cx: &mut Context<Self>) {
        match self.group_by.iter().position(|grouped| *grouped == at) {
            Some(index) => {
                self.group_by.remove(index);
            }
            None => self.group_by.push(at),
        }
        cx.notify();
    }

    /// Sorts by one picked column, or stops doing so when `direction` is
    /// `None`.
    pub fn set_order_by(
        &mut self,
        at: (usize, usize),
        direction: Option<SortDir>,
        cx: &mut Context<Self>,
    ) {
        self.order_by.retain(|(sorted, _)| *sorted != at);
        if let Some(direction) = direction {
            self.order_by.push((at, direction));
        }
        cx.notify();
    }

    /// The statement the current state describes.
    pub fn sql(&self, cx: &App) -> String {
        generate(&self.query(cx), &self.dialect)
    }

    /// Asks the workspace to open the statement in a query pane.
    ///
    /// Nothing happens over an empty canvas: there is no statement, and a query
    /// pane holding an empty buffer is what "new query" is for.
    pub fn open_in_editor(&mut self, cx: &mut Context<Self>) {
        let sql = self.sql(cx);
        if sql.is_empty() {
            return;
        }
        cx.emit(BuilderPaneEvent::OpenSql(sql));
    }

    /// Whether the keyboard is anywhere in the panel.
    ///
    /// Both handles, for the reason [`ErdPane::contains_focus`] takes both: the
    /// canvas focuses itself when a box is pressed, and [`Focusable`] can only
    /// name one of them. A focus left on the canvas of a tab that stopped being
    /// rendered swallows every action from then on.
    ///
    /// [`ErdPane::contains_focus`]: crate::erd_pane::ErdPane::contains_focus
    pub fn contains_focus(&self, window: &Window, cx: &App) -> bool {
        self.focus_handle.contains_focused(window, cx)
            || self
                .view
                .read(cx)
                .focus_handle(cx)
                .contains_focused(window, cx)
    }

    /// Puts the keyboard in the panel: on the canvas once there is one.
    ///
    /// The canvas is where the zoom chords are bound, but it is only in the
    /// element tree once a table has been added — so until then the panel's own
    /// root takes the keyboard, which is always drawn. Focusing something
    /// unrendered is the hazard `Workspace::reclaim_focus` describes.
    pub fn take_focus(&self, window: &mut Window, cx: &mut App) {
        let handle = if self.tables.is_empty() {
            self.focus_handle.clone()
        } else {
            self.view.read(cx).focus_handle(cx)
        };
        handle.focus(window);
    }

    /// The state as the generator wants it.
    fn query(&self, cx: &App) -> BuilderQuery {
        BuilderQuery {
            tables: self.tables.clone(),
            joins: self.joins.clone(),
            selected: self.selected.clone(),
            where_clauses: self
                .where_rows
                .iter()
                .map(|input| input.read(cx).content().to_string())
                .collect(),
            group_by: self.group_by.clone(),
            order_by: self.order_by.clone(),
        }
    }

    /// Hands the canvas the highlighted columns.
    fn sync_selection(&mut self, cx: &mut Context<Self>) {
        let selected: HashSet<(usize, usize)> = self.selected.iter().copied().collect();
        self.view
            .update(cx, |view, cx| view.set_selected(selected, cx));
    }

    /// Hands the canvas the lines, which carry no type.
    fn sync_edges(&mut self, cx: &mut Context<Self>) {
        let edges: Vec<BuilderEdge> = self
            .joins
            .iter()
            .map(|join| BuilderEdge {
                from: join.from,
                to: join.to,
            })
            .collect();
        self.view.update(cx, |view, cx| view.set_edges(edges, cx));
    }

    /// One column as the form names it: `alias.column`, unquoted.
    ///
    /// A label rather than SQL — the statement below it is where the quoting
    /// shows — so a missing index reads as a placeholder instead of panicking a
    /// frame.
    fn label_of(&self, (table, column): (usize, usize)) -> SharedString {
        let Some(table) = self.tables.get(table) else {
            return SharedString::new_static("?");
        };
        match table.columns.get(column) {
            Some(column) => SharedString::from(format!("{}.{column}", table.alias)),
            None => SharedString::from(table.alias.clone()),
        }
    }

    /// The join list: one row per line drawn, with its type and its delete
    /// button.
    fn render_joins(&self, chrome: &Theme, cx: &mut Context<Self>) -> Option<gpui::AnyElement> {
        if self.joins.is_empty() {
            return None;
        }
        let this = cx.entity();
        let rows: Vec<_> = self
            .joins
            .iter()
            .enumerate()
            .map(|(index, join)| {
                let kind = Select::new(("builder-join", index))
                    .options(JoinKind::ALL.iter().map(|kind| join_label(*kind)))
                    .selected(Some(join_label(join.kind)))
                    .open(self.open_join == Some(index))
                    .width(px(120.))
                    .on_select({
                        let this = this.clone();
                        // By index: the labels are translated and say nothing
                        // about which `JoinKind` they are.
                        move |chosen, _label, _window, cx| {
                            let Some(kind) = JoinKind::ALL.get(chosen).copied() else {
                                return;
                            };
                            this.update(cx, |pane, cx| pane.set_join_kind(index, kind, cx));
                        }
                    })
                    .on_open_change({
                        let this = this.clone();
                        move |open, _window, cx| {
                            this.update(cx, |pane, cx| {
                                pane.open_join = open.then_some(index);
                                cx.notify();
                            });
                        }
                    });

                let remove = {
                    let this = this.clone();
                    Button::new(("builder-join-remove", index), ts!("builder.remove"))
                        .variant(ButtonVariant::Secondary)
                        .on_click(move |_, _window, cx| {
                            this.update(cx, |pane, cx| pane.remove_join(index, cx));
                        })
                };

                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(8.))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_size(px(12.))
                            .text_color(chrome.text)
                            .child(SharedString::from(format!(
                                "{} = {}",
                                self.label_of(join.from),
                                self.label_of(join.to)
                            ))),
                    )
                    .child(kind)
                    .child(remove)
            })
            .collect();

        Some(
            section(ts!("builder.joins"), chrome)
                .children(rows)
                .into_any_element(),
        )
    }

    /// The `WHERE` rows and the button that adds one.
    fn render_conditions(&self, chrome: &Theme, cx: &mut Context<Self>) -> gpui::AnyElement {
        let this = cx.entity();
        let rows: Vec<_> = self
            .where_rows
            .iter()
            .enumerate()
            .map(|(index, input)| {
                let this = this.clone();
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(8.))
                    .child(div().flex_1().min_w_0().child(input.clone()))
                    .child(
                        Button::new(("builder-where-remove", index), ts!("builder.remove"))
                            .variant(ButtonVariant::Secondary)
                            .on_click(move |_, _window, cx| {
                                this.update(cx, |pane, cx| pane.remove_condition(index, cx));
                            }),
                    )
            })
            .collect();

        let add = {
            let this = this.clone();
            Button::new("builder-where-add", ts!("builder.add_condition"))
                .variant(ButtonVariant::Secondary)
                .on_click(move |_, _window, cx| {
                    this.update(cx, |pane, cx| pane.add_condition(cx));
                })
        };

        section(ts!("builder.where"), chrome)
            .children(rows)
            .child(div().flex().flex_row().child(add))
            .into_any_element()
    }

    /// The picked columns, each with its `GROUP BY` toggle and sort dropdown.
    fn render_columns(&self, chrome: &Theme, cx: &mut Context<Self>) -> Option<gpui::AnyElement> {
        if self.selected.is_empty() {
            return None;
        }
        let this = cx.entity();
        let rows: Vec<_> = self
            .selected
            .iter()
            .enumerate()
            .map(|(index, at)| {
                let at = *at;
                let grouped = self.group_by.contains(&at);
                let sorted = self
                    .order_by
                    .iter()
                    .find(|(column, _)| *column == at)
                    .map(|(_, direction)| *direction);

                let group = {
                    let this = this.clone();
                    Checkbox::new(("builder-group", index), ts!("builder.group_by"))
                        .checked(grouped)
                        .on_toggle(move |_checked, _window, cx| {
                            this.update(cx, |pane, cx| pane.toggle_group_by(at, cx));
                        })
                };

                let order = Select::new(("builder-order", index))
                    .options(SORTS.iter().map(|sort| sort_label(*sort)))
                    .selected(Some(sort_label(sorted)))
                    .open(self.open_order == Some(index))
                    .width(px(130.))
                    .on_select({
                        let this = this.clone();
                        move |chosen, _label, _window, cx| {
                            let Some(direction) = SORTS.get(chosen).copied() else {
                                return;
                            };
                            this.update(cx, |pane, cx| pane.set_order_by(at, direction, cx));
                        }
                    })
                    .on_open_change({
                        let this = this.clone();
                        move |open, _window, cx| {
                            this.update(cx, |pane, cx| {
                                pane.open_order = open.then_some(index);
                                cx.notify();
                            });
                        }
                    });

                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(8.))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_size(px(12.))
                            .text_color(chrome.text)
                            .child(self.label_of(at)),
                    )
                    .child(group)
                    .child(order)
            })
            .collect();

        Some(
            section(ts!("builder.columns"), chrome)
                .children(rows)
                .into_any_element(),
        )
    }

    /// The statement, as it stands.
    fn render_preview(&self, chrome: &Theme, cx: &mut Context<Self>) -> gpui::AnyElement {
        let this = cx.entity();
        let sql = self.sql(cx);
        let empty = sql.is_empty();

        let open = Button::new("builder-open", ts!("builder.open_editor"))
            .disabled(empty)
            .on_click(move |_, _window, cx| {
                this.update(cx, |pane, cx| pane.open_in_editor(cx));
            });

        section(ts!("builder.sql"), chrome)
            .child(
                div()
                    .w_full()
                    .p(px(8.))
                    .rounded(px(4.))
                    .bg(chrome.surface)
                    .border_1()
                    .border_color(chrome.border)
                    // SQL is columnar text, and the editor it is about to be
                    // put into draws it the same way.
                    .font_family(crate::app_settings::monospace_family(cx))
                    .text_size(px(12.))
                    .text_color(if empty {
                        chrome.text_muted
                    } else {
                        chrome.text
                    })
                    .child(if empty {
                        ts!("builder.no_sql")
                    } else {
                        SharedString::from(sql)
                    }),
            )
            .child(div().flex().flex_row().child(open))
            .into_any_element()
    }
}

impl EventEmitter<BuilderPaneEvent> for BuilderPane {}

impl Focusable for BuilderPane {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for BuilderPane {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let chrome = theme(cx);

        // One element in both states, because an explorer row is dropped on
        // both: the empty canvas is a drop target above all, since the hint it
        // draws is what the *first* table lands on. `on_drop` asks for no id —
        // gpui gives any element with a drop listener a hitbox — and the
        // canvas widget's own mouse handling is undisturbed, because the
        // release that ends a drag has no press of its own behind it.
        //
        // The resting border is the window's own background rather than no
        // border at all, so that lighting it up while something is held over
        // the canvas moves no pixel of what is inside.
        let empty = self.tables.is_empty();
        let accent = chrome.accent;
        let canvas = div()
            .flex()
            .flex_1()
            .min_w_0()
            .min_h_0()
            .border_1()
            .border_color(chrome.background)
            .on_drop::<DraggedObject>(cx.listener(|_pane, dragged: &DraggedObject, _window, cx| {
                cx.emit(BuilderPaneEvent::TableDropped(dragged.0.clone()));
            }))
            .drag_over::<DraggedObject>(move |style, _dragged, _window, _cx| {
                style.border_color(accent)
            })
            .map(|canvas| {
                if empty {
                    canvas
                        .p(px(10.))
                        .text_size(px(12.))
                        .text_color(chrome.text_muted)
                        .child(ts!("builder.canvas_hint"))
                } else {
                    canvas.child(self.view.clone())
                }
            });

        let joins = self.render_joins(&chrome, cx);
        let conditions = self.render_conditions(&chrome, cx);
        let columns = self.render_columns(&chrome, cx);
        let preview = self.render_preview(&chrome, cx);

        div()
            .id("builder-pane")
            .track_focus(&self.focus_handle)
            .flex()
            .flex_col()
            .size_full()
            .min_w_0()
            .min_h_0()
            .child(canvas)
            .child(
                div()
                    .id("builder-form")
                    .track_scroll(&self.body_scroll)
                    .flex()
                    .flex_col()
                    .flex_none()
                    .h(px(FORM_HEIGHT))
                    .min_w_0()
                    .gap(px(12.))
                    .p(px(10.))
                    .border_t_1()
                    .border_color(chrome.border)
                    .overflow_y_scroll()
                    .children(joins)
                    .child(conditions)
                    .children(columns)
                    .child(preview),
            )
    }
}

/// The three states the sort dropdown offers, in the order it lists them.
const SORTS: [Option<SortDir>; 3] = [None, Some(SortDir::Asc), Some(SortDir::Desc)];

/// What the dropdown calls one join type.
fn join_label(kind: JoinKind) -> SharedString {
    match kind {
        JoinKind::Inner => ts!("builder.join_inner"),
        JoinKind::Left => ts!("builder.join_left"),
        JoinKind::Right => ts!("builder.join_right"),
        JoinKind::Full => ts!("builder.join_full"),
    }
}

/// What the dropdown calls one sort state.
fn sort_label(direction: Option<SortDir>) -> SharedString {
    match direction {
        None => ts!("builder.order_none"),
        Some(SortDir::Asc) => ts!("builder.order_asc"),
        Some(SortDir::Desc) => ts!("builder.order_desc"),
    }
}

/// One labelled block of the form.
fn section(title: SharedString, chrome: &Theme) -> gpui::Div {
    div().flex().flex_col().gap(px(6.)).child(
        div()
            .text_size(px(11.))
            .text_color(chrome.text_muted)
            .child(title),
    )
}

/// Reads one table's columns, in catalogue order.
///
/// **Blocks**, and is called from `cx.background_spawn` with a
/// [`SessionHandle`](crate::connection::SessionHandle).
///
/// One round trip, as §7.7 asks for: a builder is used by adding several tables
/// in a row, and a key lookup per table would make each of those additions
/// three calls instead of one. The primary-key and foreign-key marks a diagram
/// draws are therefore absent here, which costs a colour on a row and nothing
/// about the statement.
pub fn load_columns(session: &Session, target: &ObjectTarget) -> Result<Vec<ErdColumn>, String> {
    let mut request = DescribeRequest::new("columns");
    request.catalog = target.catalog.clone();
    request.schema = target.schema.clone();
    request.table = Some(target.name.clone());

    let mut columns = items(session, &request)?;
    columns.sort_by(|left, right| {
        let ordinal = |column: &serde_json::Map<String, serde_json::Value>| {
            number(column, "ordinal").unwrap_or(i64::MAX)
        };
        ordinal(left)
            .cmp(&ordinal(right))
            // A driver that reported no ordinal still has to produce the same
            // order twice running, or the indices a join names would move.
            .then_with(|| text(left, "name").cmp(&text(right, "name")))
    });

    Ok(columns
        .iter()
        .filter_map(|column| Some(ErdColumn::new(text(column, "name")?, type_of(column))))
        .collect())
}

#[cfg(test)]
mod tests {
    use gpui::{
        Modifiers, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, Pixels, Point,
        TestAppContext, VisualTestContext, point,
    };

    use super::*;
    use crate::explorer::Folder;

    /// A table of `columns` in the public schema of `connection`.
    fn target(name: &str) -> ObjectTarget {
        ObjectTarget {
            connection: ConnectionId(1),
            catalog: None,
            schema: Some("APP".to_string()),
            folder: Folder::Tables,
            name: name.to_string(),
        }
    }

    /// Columns under the names a test reads best.
    fn columns(names: &[&str]) -> Vec<ErdColumn> {
        names
            .iter()
            .map(|name| ErdColumn::new(*name, "INTEGER"))
            .collect()
    }

    /// A panel in a window of its own.
    fn open(cx: &mut TestAppContext) -> gpui::WindowHandle<BuilderPane> {
        cx.update(rudbman_ui::init);
        cx.add_window(|_window, cx| BuilderPane::new(ConnectionId(1), "h2", cx))
    }

    #[gpui::test]
    fn an_empty_builder_has_no_statement(cx: &mut TestAppContext) {
        let window = open(cx);
        window
            .update(cx, |pane, _window, cx| {
                assert_eq!(pane.table_count(), 0);
                assert_eq!(pane.sql(cx), "");
                // And "open in editor" over one says nothing rather than
                // opening an empty pane.
                pane.open_in_editor(cx);
            })
            .expect("the window is open");
    }

    /// The whole form, without a mouse: three tables, two joins, picks, a
    /// condition, a group and a sort.
    #[gpui::test]
    fn the_form_builds_the_statement_the_generator_writes(cx: &mut TestAppContext) {
        let window = open(cx);
        window
            .update(cx, |pane, _window, cx| {
                pane.add_table(&target("PERSON"), columns(&["ID", "TEAM_ID", "NAME"]), cx);
                pane.add_table(&target("TEAM"), columns(&["ID", "NAME", "OFFICE_ID"]), cx);
                pane.add_table(&target("OFFICE"), columns(&["ID", "CITY"]), cx);
                assert_eq!(pane.table_count(), 3);
                // Nothing picked yet, and nothing joined: three tables and two
                // commas.
                assert_eq!(
                    pane.sql(cx),
                    "SELECT *\nFROM APP.PERSON,\n  APP.TEAM,\n  APP.OFFICE"
                );

                pane.add_join((0, 1), (1, 0), cx);
                pane.add_join((1, 2), (2, 0), cx);
                // The same edge again, drawn the other way, is the same join.
                pane.add_join((2, 0), (1, 2), cx);
                assert_eq!(pane.joins.len(), 2);
                pane.set_join_kind(1, JoinKind::Left, cx);

                pane.toggle_column(0, 2, cx);
                pane.toggle_column(2, 1, cx);
                pane.toggle_group_by((2, 1), cx);
                pane.set_order_by((0, 2), Some(SortDir::Desc), cx);

                pane.add_condition(cx);
                pane.set_condition(0, "PERSON.ID > 10", cx);

                assert_eq!(
                    pane.sql(cx),
                    "SELECT PERSON.NAME, OFFICE.CITY\n\
                     FROM APP.PERSON\n\
                     \x20 INNER JOIN APP.TEAM ON PERSON.TEAM_ID = TEAM.ID\n\
                     \x20 LEFT JOIN APP.OFFICE ON TEAM.OFFICE_ID = OFFICE.ID\n\
                     WHERE PERSON.ID > 10\n\
                     GROUP BY OFFICE.CITY\n\
                     ORDER BY PERSON.NAME DESC"
                );

                // Unpicking a column takes its group and its sort with it.
                pane.toggle_column(2, 1, cx);
                assert_eq!(
                    pane.sql(cx),
                    "SELECT PERSON.NAME\n\
                     FROM APP.PERSON\n\
                     \x20 INNER JOIN APP.TEAM ON PERSON.TEAM_ID = TEAM.ID\n\
                     \x20 LEFT JOIN APP.OFFICE ON TEAM.OFFICE_ID = OFFICE.ID\n\
                     WHERE PERSON.ID > 10\n\
                     ORDER BY PERSON.NAME DESC"
                );

                // A deleted join drops back to a comma.
                pane.remove_join(1, cx);
                pane.remove_condition(0, cx);
                assert_eq!(
                    pane.sql(cx),
                    "SELECT PERSON.NAME\n\
                     FROM APP.PERSON\n\
                     \x20 INNER JOIN APP.TEAM ON PERSON.TEAM_ID = TEAM.ID,\n\
                     \x20 APP.OFFICE\n\
                     ORDER BY PERSON.NAME DESC"
                );
            })
            .expect("the window is open");
    }

    /// The same table twice is two boxes under two aliases, which is what makes
    /// a self-join drawable at all.
    #[gpui::test]
    fn the_same_table_twice_gets_an_alias(cx: &mut TestAppContext) {
        let window = open(cx);
        window
            .update(cx, |pane, _window, cx| {
                pane.add_table(&target("PERSON"), columns(&["ID", "MANAGER_ID"]), cx);
                pane.add_table(&target("PERSON"), columns(&["ID", "MANAGER_ID"]), cx);
                pane.add_join((0, 1), (1, 0), cx);

                assert_eq!(pane.tables[1].alias, "PERSON_2");
                assert_eq!(
                    pane.sql(cx),
                    "SELECT *\n\
                     FROM APP.PERSON\n\
                     \x20 INNER JOIN APP.PERSON PERSON_2 ON PERSON.MANAGER_ID = PERSON_2.ID"
                );
            })
            .expect("the window is open");
    }

    /// The panel's one message reaches a subscriber, carrying the statement.
    #[gpui::test]
    fn opening_in_the_editor_hands_the_statement_over(cx: &mut TestAppContext) {
        cx.update(rudbman_ui::init);
        let seen = std::rc::Rc::new(std::cell::RefCell::new(Vec::<String>::new()));
        let pane = cx.new(|cx| BuilderPane::new(ConnectionId(1), "h2", cx));

        let recorder = std::rc::Rc::clone(&seen);
        let _subscription = cx.update(|cx| {
            cx.subscribe(&pane, move |_pane, event, _cx| {
                if let BuilderPaneEvent::OpenSql(sql) = event {
                    recorder.borrow_mut().push(sql.clone());
                }
            })
        });

        cx.update(|cx| {
            pane.update(cx, |pane, cx| {
                pane.add_table(&target("PERSON"), columns(&["ID"]), cx);
                pane.toggle_column(0, 0, cx);
                pane.open_in_editor(cx);
            });
        });
        cx.run_until_parked();

        assert_eq!(
            seen.borrow().as_slice(),
            ["SELECT PERSON.ID\nFROM APP.PERSON".to_string()]
        );
    }

    /// Height of the strip a drag starts on in [`DropHarness`], in logical
    /// pixels. Everything below it is the panel.
    const STRIP: f32 = 20.;

    /// Stands in for the chip the explorer draws under the pointer. What it
    /// looks like is the explorer's business; that a drag *has* one is gpui's
    /// requirement.
    struct Ghost;

    impl Render for Ghost {
        fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
            div()
        }
    }

    /// The panel with something draggable above it, which is what the shell is
    /// to it: a sidebar row that carries a [`DraggedObject`], and a canvas
    /// underneath to let go of it over.
    struct DropHarness {
        /// The panel under test.
        pane: Entity<BuilderPane>,
        /// What the strip drags.
        dragged: ObjectTarget,
    }

    impl Render for DropHarness {
        fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
            div()
                .flex()
                .flex_col()
                .size_full()
                .child(div().id("row").flex_none().h(px(STRIP)).w_full().on_drag(
                    DraggedObject(self.dragged.clone()),
                    |_dragged, _at, _window, cx| cx.new(|_cx| Ghost),
                ))
                .child(div().flex().flex_1().min_h_0().child(self.pane.clone()))
        }
    }

    /// Presses the left button at `at`.
    fn press(cx: &mut VisualTestContext, at: Point<Pixels>) {
        cx.simulate_event(MouseDownEvent {
            position: at,
            modifiers: Modifiers::none(),
            button: MouseButton::Left,
            click_count: 1,
            first_mouse: false,
        });
        cx.run_until_parked();
    }

    /// Moves the pointer to `at` with the left button down.
    fn drag_to(cx: &mut VisualTestContext, at: Point<Pixels>) {
        cx.simulate_event(MouseMoveEvent {
            position: at,
            pressed_button: Some(MouseButton::Left),
            modifiers: Modifiers::none(),
        });
        cx.run_until_parked();
    }

    /// Releases the left button at `at`.
    fn release(cx: &mut VisualTestContext, at: Point<Pixels>) {
        cx.simulate_event(MouseUpEvent {
            position: at,
            modifiers: Modifiers::none(),
            button: MouseButton::Left,
            click_count: 1,
        });
        cx.run_until_parked();
    }

    /// A real pointer drag onto the canvas: press, past the threshold, over the
    /// canvas, let go. The panel asks the workspace for the table rather than
    /// loading it, because the session is not its to hold.
    #[gpui::test]
    fn a_table_dropped_on_the_canvas_is_asked_for(cx: &mut TestAppContext) {
        cx.update(rudbman_ui::init);
        let dropped = std::rc::Rc::new(std::cell::RefCell::new(Vec::<ObjectTarget>::new()));

        let pane = cx.new(|cx| BuilderPane::new(ConnectionId(1), "h2", cx));
        let recorder = std::rc::Rc::clone(&dropped);
        let _subscription = cx.update(|cx| {
            cx.subscribe(&pane, move |_pane, event, _cx| {
                if let BuilderPaneEvent::TableDropped(target) = event {
                    recorder.borrow_mut().push(target.clone());
                }
            })
        });

        let window = cx.add_window({
            let pane = pane.clone();
            move |_window, _cx| DropHarness {
                pane,
                dragged: target("PERSON"),
            }
        });
        let mut cx = VisualTestContext::from_window(window.into(), cx);
        cx.run_until_parked();

        // On the strip, then well past the 2 px gpui asks for before a press
        // counts as a drag, then over the empty canvas — which is the state the
        // *first* table is always dropped onto.
        press(&mut cx, point(px(100.), px(10.)));
        drag_to(&mut cx, point(px(140.), px(60.)));
        drag_to(&mut cx, point(px(400.), px(300.)));
        release(&mut cx, point(px(400.), px(300.)));

        assert_eq!(dropped.borrow().as_slice(), [target("PERSON")]);
        // And nothing was added behind the workspace's back: the columns are
        // still unread, so the canvas is still empty.
        cx.update(|_window, cx| assert_eq!(pane.read(cx).table_count(), 0));
    }

    /// The loader against a real product: the column list comes back in
    /// catalogue order, typed the way the diagram types it.
    #[test]
    fn one_tables_columns_load_in_catalogue_order() {
        let connected = crate::explorer::tests::h2_fixture("builder-columns");
        let mut wanted = target("PERSON");
        wanted.name = "PERSON".to_string();
        let columns = load_columns(connected.session(), &wanted).expect("H2 answers");

        let names: Vec<&str> = columns.iter().map(|column| column.name.as_str()).collect();
        assert_eq!(names, ["ID", "TEAM_ID", "EMAIL", "SALARY"]);
        assert_eq!(columns[3].type_name, "NUMERIC(12,2)");
    }

    #[test]
    fn every_label_the_panel_draws_has_a_translation() {
        for label in [
            ts!("builder.tab", index = 1),
            ts!("builder.canvas_hint"),
            ts!("builder.joins"),
            ts!("builder.join_inner"),
            ts!("builder.join_left"),
            ts!("builder.join_right"),
            ts!("builder.join_full"),
            ts!("builder.remove"),
            ts!("builder.where"),
            ts!("builder.add_condition"),
            ts!("builder.columns"),
            ts!("builder.group_by"),
            ts!("builder.order_none"),
            ts!("builder.order_asc"),
            ts!("builder.order_desc"),
            ts!("builder.sql"),
            ts!("builder.no_sql"),
            ts!("builder.open_editor"),
            ts!("menu.add_to_builder"),
            ts!("menu.new_builder"),
        ] {
            assert!(!label.is_empty(), "empty label");
            assert!(!label.starts_with("builder."), "untranslated {label:?}");
            assert!(!label.starts_with("menu."), "untranslated {label:?}");
        }
    }
}
