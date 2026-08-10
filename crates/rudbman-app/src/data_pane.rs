//! The data pane: one table's rows, paged into a grid.
//!
//! What the architecture document's §7.9 draws, in the state this milestone
//! leaves it: browsing. The pane runs `SELECT * FROM <table>` of its own accord,
//! pages it as the grid nears the end, and re-runs it in a new order when a
//! heading is clicked. Editing — staging changes, generating the DML and
//! applying it in one transaction — is the work that comes next, and the shape
//! here is arranged so that it can be added without moving anything: the key
//! columns are already read before the rows are, the source under the grid is
//! already append-only and index-addressable, and every path that would replace
//! that source already asks [`DataPane::has_pending_edits`] first.
//!
//! # Why a pane and not a fifth detail tab
//!
//! The detail panel is one load of presentation and holds nothing of the
//! session; this holds a cursor, a generation counter and — soon — a staging
//! buffer, and a re-sort throws its whole result away. Those are different
//! lifetimes, and §7.9 keeps them in different tabs.
//!
//! # What it borrows and what it owns
//!
//! The fetch pipeline is the query pane's, moved into [`crate::query_source`]
//! so that both use one walk over a cursor rather than two. What is this pane's
//! own is the statement: it is assembled here from the target and the dialect,
//! never typed, which is why sorting can append an `ORDER BY` where the query
//! pane has to wrap what the user wrote in a derived table.

use std::sync::Arc;

use gpui::{
    AnyElement, App, Context, Entity, FocusHandle, Focusable, IntoElement, ParentElement, Render,
    SharedString, Styled, Subscription, Window, div, prelude::*, px,
};
use rudbman_core::{AppSettings, ConnectionProfile};
use rudbman_grid::{GridEvent, GridSource, GridSourceState, GridView, SortDirection};
use rudbman_jdbc::{
    ColumnInfo, Cursor, DescribeRequest, Error as JdbcError, Session, StatementSpec,
};
use rudbman_sql::Dialect;
use rudbman_ui::{Button, ButtonVariant, Theme, theme};

use crate::builder_sql;
use crate::connection::SessionHandle;
use crate::explorer::{ConnectionId, ObjectTarget};
use crate::i18n::ts;
use crate::query::{QueryError, note, render_error};
use crate::query_source::{Paged, RenderedBatch, ResultSource, Step, advance, page};
use crate::table_detail::{number, text};

/// One statement's worth of rows, as the background task hands them over.
struct Opened {
    /// The primary key's columns, in key order. Empty for a table that has
    /// none, which is the read-only case §7.9 describes.
    keys: Vec<String>,
    /// The result's logical column types.
    columns: Vec<ColumnInfo>,
    /// The first batch, already rendered. `None` for a driver that answered a
    /// statement with no result set at all, which a `SELECT` never does but a
    /// view over something exotic might.
    batch: Option<RenderedBatch>,
    /// Whether the driver had run out of rows.
    complete: bool,
    /// The cursor, while there is more to page. Closed on drop otherwise.
    cursor: Option<Cursor>,
}

/// The ordering the rows were asked for.
///
/// Both halves are needed across a re-run, and for different reasons: the name
/// writes the next `ORDER BY`, and the index puts the marker back on the grid
/// that comes back — the grid that was wearing it has been thrown away by then.
#[derive(Clone, Debug)]
struct Order {
    /// The source column, as the grid numbers them.
    column: usize,
    /// Its name, as the catalogue spells it.
    name: String,
    /// Which way.
    direction: SortDirection,
}

/// The rows on screen, and everything needed to go on reading them.
struct Rows {
    grid: Entity<GridView<ResultSource>>,
    /// The result's logical column types, for rendering later batches.
    columns: Arc<Vec<ColumnInfo>>,
    /// The cursor, while this result is still ours to page. `None` once the
    /// rows ran out, and while a fetch has it.
    cursor: Option<Cursor>,
    /// Whether the cursor is out with a background fetch.
    fetching: bool,
    /// Keeps the grid's subscription alive for as long as the rows.
    _events: Subscription,
}

/// Where the pane's rows have got to.
enum Load {
    /// A load is out.
    Running,
    /// It came back, with however many rows there were.
    Ready(Box<Rows>),
    /// The driver refused; its own words, through the query pane's envelope.
    Failed(Box<QueryError>),
}

/// One table's rows.
pub struct DataPane {
    /// What is being shown. Also the tab's title, and the identity two "view
    /// data" gestures are deduplicated on.
    target: ObjectTarget,
    /// Which connection tab this pane belongs to.
    connection: ConnectionId,
    /// The session everything here runs on.
    ///
    /// `None` once [`DataPane::detach`] has run: the tab outlives its
    /// connection, because the rows already fetched are worth reading and
    /// copying, but nothing more can be asked for.
    session: Option<SessionHandle>,
    /// Writes the statement's identifiers, and nothing here writes one by hand.
    dialect: Dialect,
    /// The profile refuses writes outright (§8), which is one of the two
    /// reasons this pane can only ever browse.
    read_only: bool,
    /// Rows per `FETCH`, from the settings. Read once, when the pane opens.
    fetch_rows: u32,
    /// The primary key's columns, in key order.
    ///
    /// The other reason the pane may only browse — a table with none cannot be
    /// edited by primary key — and, from the next milestone, what the generated
    /// `WHERE` clause is written from.
    keys: Vec<String>,
    load: Load,
    /// The order the rows were asked in, when one was asked for.
    order: Option<Order>,
    /// A line the pane wants to say without it being a failure.
    notice: Option<SharedString>,
    /// The generation of the newest load. Every delivery carries one, and one
    /// that is not this is an answer a later load has already replaced.
    generation: u64,
    focus_handle: FocusHandle,
}

impl DataPane {
    /// A pane over `target`, before it has asked for anything.
    ///
    /// It does **not** load: the host has just created it inside `cx.new` and
    /// has a tab to open first, and a pane that started a fetch from its own
    /// constructor would be racing that. [`DataPane::refresh`] is the one way
    /// in, and it is also what the toolbar's button calls.
    pub fn new(
        session: SessionHandle,
        connection: ConnectionId,
        target: ObjectTarget,
        profile: &ConnectionProfile,
        driver_dialect: &str,
        settings: &AppSettings,
        cx: &mut Context<Self>,
    ) -> Self {
        Self {
            target,
            connection,
            session: Some(session),
            dialect: Dialect::from_id(driver_dialect),
            read_only: profile.read_only,
            fetch_rows: settings.fetch_batch_rows,
            keys: Vec::new(),
            load: Load::Running,
            order: None,
            notice: None,
            generation: 0,
            focus_handle: cx.focus_handle(),
        }
    }

    /// The object whose rows these are.
    pub fn target(&self) -> &ObjectTarget {
        &self.target
    }

    /// Which connection tab this pane runs against.
    pub fn connection(&self) -> ConnectionId {
        self.connection
    }

    /// Lets the session go, leaving the tab standing.
    ///
    /// The same bargain a query pane's detach strikes, and for the same reason
    /// (§9.3): holding a [`SessionHandle`] behind a dead connection would keep
    /// the session and its tunnel standing under a pane nobody can use. What
    /// stays is the rows already fetched, which are the user's to read.
    pub fn detach(&mut self, cx: &mut Context<Self>) {
        self.session = None;
        // Dropping the cursor closes it, and is what stops a scroll asking a
        // session that is being closed for another page.
        if let Load::Ready(rows) = &mut self.load {
            rows.cursor = None;
        }
        cx.notify();
    }

    /// Whether this pane still has a session behind it.
    #[cfg(test)]
    pub fn is_attached(&self) -> bool {
        self.session.is_some()
    }

    /// Whether the keyboard is anywhere inside this pane, as the last drawn
    /// frame had it.
    ///
    /// Two handles, for the reason a query pane has two: the grid takes the
    /// focus for itself when a cell is clicked, and a focus left on it would
    /// strand when the tab stops being rendered.
    pub fn contains_focus(&self, window: &Window, cx: &App) -> bool {
        if self.focus_handle.contains_focused(window, cx) {
            return true;
        }
        match &self.load {
            Load::Ready(rows) => rows
                .grid
                .read(cx)
                .focus_handle(cx)
                .contains_focused(window, cx),
            Load::Running | Load::Failed(_) => false,
        }
    }

    /// Puts the keyboard on the rows, or on the pane while there are none.
    pub fn take_focus(&self, window: &mut Window, cx: &mut Context<Self>) {
        match &self.load {
            Load::Ready(rows) => {
                let handle = rows.grid.read(cx).focus_handle(cx);
                window.focus(&handle);
            }
            Load::Running | Load::Failed(_) => window.focus(&self.focus_handle),
        }
    }

    /// Whether the pane is holding changes the user has not applied.
    ///
    /// Always false while the pane only browses. It is asked anyway, by every
    /// path that replaces the rows wholesale, because those paths are exactly
    /// the ones §7.9 says must ask once there is a staging buffer to ask about
    /// — and a guard added after the fact is a guard somebody has to remember
    /// to add in three places.
    fn has_pending_edits(&self) -> bool {
        false
    }

    /// Whether the rows may only be read.
    ///
    /// Two reasons, and the banner says which: the profile is marked read-only,
    /// or the table has no primary key to write a `WHERE` clause from. Both are
    /// permanent for the life of the pane, which is why this is a question and
    /// not a state.
    fn read_only_reason(&self) -> Option<SharedString> {
        if self.read_only {
            return Some(ts!("data.read_only"));
        }
        // Only once the metadata has answered: a table whose key is still on
        // its way has not said it has none.
        if matches!(self.load, Load::Ready(_)) && self.keys.is_empty() {
            return Some(ts!("data.no_primary_key"));
        }
        None
    }

    /// The table, qualified and quoted the way this dialect spells it.
    fn qualified(&self) -> String {
        builder_sql::table_ref(
            &self.dialect,
            self.target.catalog.as_deref(),
            self.target.schema.as_deref(),
            &self.target.name,
        )
    }

    /// The statement the pane runs: every column, in whatever order was asked
    /// for.
    ///
    /// An appended `ORDER BY` rather than the derived table the query pane
    /// wraps its statement in. That wrapper exists because the user's own SQL
    /// may already carry an ordering and two of them do not compose; this
    /// statement is written here, one clause at a time, so there is nothing
    /// underneath to collide with.
    fn select_sql(&self) -> String {
        let mut sql = format!("SELECT * FROM {}", self.qualified());
        if let Some(order) = &self.order {
            let direction = match order.direction {
                SortDirection::Ascending => "ASC",
                SortDirection::Descending => "DESC",
            };
            sql.push_str(" ORDER BY ");
            sql.push_str(&self.dialect.quote_ident(&order.name));
            sql.push(' ');
            sql.push_str(direction);
        }
        sql
    }

    /// Reads the key and the first batch again, from scratch.
    ///
    /// Both together on one background task: they are wanted at the same
    /// moment, and the key has to be known before the grid is built or the key
    /// columns would draw unmarked for a frame and then jump.
    pub fn refresh(&mut self, cx: &mut Context<Self>) {
        if self.has_pending_edits() {
            self.notice = Some(ts!("data.discard_first"));
            cx.notify();
            return;
        }
        let Some(session) = self.session.clone() else {
            self.notice = Some(ts!("explorer.disconnected"));
            cx.notify();
            return;
        };

        self.generation += 1;
        let generation = self.generation;
        // Dropping what was there drops its cursor, which closes it.
        self.load = Load::Running;
        self.notice = None;

        let target = self.target.clone();
        let sql = self.select_sql();
        let fetch_rows = self.fetch_rows;

        cx.spawn(async move |pane, cx| {
            let outcome = cx
                .background_spawn(async move { open(session.session(), &target, &sql, fetch_rows) })
                .await;
            pane.update(cx, |pane, cx| pane.deliver(generation, outcome, cx))
                .ok();
        })
        .detach();
        cx.notify();
    }

    /// Records what one load produced.
    fn deliver(
        &mut self,
        generation: u64,
        outcome: Result<Opened, JdbcError>,
        cx: &mut Context<Self>,
    ) {
        if generation != self.generation {
            // A superseded load's answer. Dropping it closes its cursor.
            return;
        }
        match outcome {
            Ok(opened) => {
                let Opened {
                    keys,
                    columns,
                    batch,
                    complete,
                    cursor,
                } = opened;
                self.keys = keys;

                let mut source = ResultSource::new(&columns);
                source.mark_primary_keys(&self.keys);
                if let Some(batch) = batch {
                    source.push(batch);
                }
                source.set_state(if complete && cursor.is_none() {
                    GridSourceState::Complete
                } else {
                    GridSourceState::HasMore
                });

                let table = self.qualified();
                let grid = cx.new(|cx| GridView::new(source, cx).insert_table(table));
                // The marker the sort that asked for this run was for: the grid
                // that was wearing it has just been replaced by this one.
                if let Some(order) = &self.order {
                    let sort = Some((order.column, order.direction));
                    grid.update(cx, |grid, cx| grid.set_sort(sort, cx));
                }
                let events = cx.subscribe(&grid, |pane, _grid, event, cx| match event {
                    GridEvent::NearEnd => pane.fetch_more(cx),
                    GridEvent::SortRequested { column, direction } => {
                        pane.reorder(*column, *direction, cx);
                    }
                    // Nothing yet, and nothing by accident: no cell of this
                    // pane is editable until a staging layer answers
                    // `GridSource::cell_editable`, so the grid opens no field
                    // and commits nothing. Activating a cell will open the
                    // editor or a LOB viewer, and the grid's own menu arrives
                    // with the editing rows that fill it — all of which is the
                    // milestone that makes this pane writable (§7.9).
                    GridEvent::CellActivated { .. }
                    | GridEvent::EditCommitted { .. }
                    | GridEvent::ContextMenu { .. } => {}
                });

                self.load = Load::Ready(Box::new(Rows {
                    grid,
                    columns: Arc::new(columns),
                    cursor,
                    fetching: false,
                    _events: events,
                }));
            }
            Err(error) => self.load = Load::Failed(Box::new(QueryError::new(error))),
        }
        cx.notify();
    }

    /// The grid has come within sight of the last row it holds.
    fn fetch_more(&mut self, cx: &mut Context<Self>) {
        if self.session.is_none() {
            // `detach` has already dropped the cursor, so this is only reached
            // by a scroll that was already in flight; saying why the rows stop
            // is better than a grid that quietly ends early.
            self.notice = Some(ts!("explorer.disconnected"));
            cx.notify();
            return;
        }
        let generation = self.generation;
        let fetch_rows = self.fetch_rows;
        let Load::Ready(rows) = &mut self.load else {
            return;
        };
        if rows.fetching {
            return;
        }
        let Some(mut cursor) = rows.cursor.take() else {
            return;
        };
        rows.fetching = true;
        let columns = Arc::clone(&rows.columns);
        let grid = rows.grid.clone();
        // While a batch is on its way the source says so, which is what keeps a
        // fast scroll from asking once per frame.
        grid.update(cx, |grid, cx| {
            grid.source_mut(cx).set_state(GridSourceState::Loading);
        });

        cx.spawn(async move |pane, cx| {
            let outcome = cx
                .background_spawn(async move {
                    match page(&mut cursor, &columns, fetch_rows) {
                        Ok(paged) => Ok((cursor, paged)),
                        Err(error) => Err(error),
                    }
                })
                .await;
            pane.update(cx, |pane, cx| pane.paged(generation, outcome, cx))
                .ok();
        })
        .detach();
    }

    /// Records a page.
    fn paged(
        &mut self,
        generation: u64,
        outcome: Result<(Cursor, Paged), JdbcError>,
        cx: &mut Context<Self>,
    ) {
        if generation != self.generation {
            // The load this belonged to has been superseded. Dropping the
            // outcome closes its cursor, and the rows never reach a grid.
            return;
        }
        // What the failure arm wants to say, kept until the borrow of the rows
        // is over: the notice is a field of the pane, and the rows are one too.
        let mut failure = None;
        let Load::Ready(rows) = &mut self.load else {
            return;
        };
        rows.fetching = false;

        match outcome {
            Ok((cursor, paged)) => {
                // A completed result set is behind the cursor now: the walk in
                // `page` has already moved past it. The further results
                // `Paged::steps` may carry are a stored procedure's, and a
                // `SELECT` over one table has none.
                rows.cursor = (!paged.complete).then_some(cursor);
                let state = if paged.complete {
                    GridSourceState::Complete
                } else {
                    GridSourceState::HasMore
                };
                let grid = rows.grid.clone();
                grid.update(cx, |grid, cx| {
                    let source = grid.source_mut(cx);
                    source.push(paged.batch);
                    source.set_state(state);
                });
            }
            Err(error) => {
                rows.cursor = None;
                let grid = rows.grid.clone();
                grid.update(cx, |grid, cx| {
                    grid.source_mut(cx).set_state(GridSourceState::Complete);
                });
                // The rows already fetched stay on screen: what failed is the
                // next page, not the result under it.
                failure = Some(QueryError::new(error).message);
            }
        }
        self.notice = failure;
        cx.notify();
    }

    /// A heading was clicked: run the same `SELECT` in that order.
    fn reorder(&mut self, column: usize, direction: Option<SortDirection>, cx: &mut Context<Self>) {
        if self.session.is_none() {
            self.notice = Some(ts!("explorer.disconnected"));
            cx.notify();
            return;
        }
        if self.has_pending_edits() {
            // A re-run replaces the source the edits are keyed to; §7.9.
            self.notice = Some(ts!("data.discard_first"));
            cx.notify();
            return;
        }
        let Load::Ready(rows) = &self.load else {
            return;
        };
        let name = rows
            .grid
            .read(cx)
            .source()
            .column(column)
            .name
            .trim()
            .to_string();
        if name.is_empty() {
            return;
        }
        self.order = direction.map(|direction| Order {
            column,
            name,
            direction,
        });
        self.refresh(cx);
    }

    /// How many rows are on screen, which is however many have been paged in.
    ///
    /// `None` while there is no result at all — a load in flight, or one that
    /// failed — because zero rows and no answer are different things and the
    /// toolbar says so by drawing nothing.
    pub fn row_count(&self, cx: &App) -> Option<usize> {
        match &self.load {
            Load::Ready(rows) => Some(rows.grid.read(cx).source().row_count()),
            Load::Running | Load::Failed(_) => None,
        }
    }

    /// The strip above the rows: what is being shown, how much of it, and the
    /// button that reads it again.
    fn render_toolbar(&self, chrome: &Theme, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let this = cx.entity();
        let loading = matches!(self.load, Load::Running);
        let count = self
            .row_count(cx)
            .map(|count| ts!("data.row_count", count = count));

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
            .child(crate::icons::icon(
                self.target.folder.icon(),
                px(14.),
                chrome.text_muted,
            ))
            .child(
                div()
                    .flex_none()
                    .text_size(px(13.))
                    .text_color(chrome.text)
                    .child(SharedString::from(self.target.qualified())),
            )
            .child(div().flex_1().min_w_0())
            .children(count.map(|count| {
                div()
                    .flex_none()
                    .whitespace_nowrap()
                    .text_size(px(11.))
                    .text_color(chrome.text_muted)
                    .child(count)
            }))
            .child(
                Button::new("data-refresh", ts!("data.refresh"))
                    .variant(ButtonVariant::Secondary)
                    .disabled(loading)
                    .on_click(move |_, _window, cx| {
                        this.update(cx, |pane, cx| pane.refresh(cx));
                    }),
            )
    }

    /// The one line that says why nothing here can be edited, when something
    /// says so.
    ///
    /// Understated on purpose: it is a standing fact about the object, not a
    /// failure, and it has to live above the grid rather than in a dialog
    /// because it is the answer to a question the user has not asked yet.
    fn render_banner(&self, chrome: &Theme) -> Option<impl IntoElement + use<>> {
        let reason = self.read_only_reason()?;
        Some(
            div()
                .flex_none()
                .px(px(10.))
                .py(px(4.))
                .border_b_1()
                .border_color(chrome.border)
                .bg(chrome.surface)
                .text_size(px(11.))
                .text_color(chrome.text_muted)
                .child(reason),
        )
    }

    /// The rows, or the one line that stands in for them.
    fn render_body(&self, chrome: &Theme, cx: &App) -> AnyElement {
        match &self.load {
            Load::Running => note(ts!("data.loading"), chrome.text_muted),
            Load::Failed(error) => render_error(error, chrome),
            Load::Ready(rows) if rows.grid.read(cx).source().row_count() == 0 => {
                note(ts!("data.empty"), chrome.text_muted)
            }
            Load::Ready(rows) => div()
                .flex()
                .flex_1()
                .min_w_0()
                .min_h_0()
                .child(rows.grid.clone())
                .into_any_element(),
        }
    }
}

impl Focusable for DataPane {
    /// The pane's own handle. The grid takes the keyboard when a cell is
    /// clicked, which is why [`DataPane::contains_focus`] exists beside this.
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for DataPane {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let chrome = theme(cx);
        let toolbar = self.render_toolbar(&chrome, cx);
        let banner = self.render_banner(&chrome);
        let body = self.render_body(&chrome, cx);

        div()
            .id("data-pane")
            .track_focus(&self.focus_handle)
            .flex()
            .flex_col()
            .size_full()
            .min_w_0()
            .min_h_0()
            .relative()
            .child(toolbar)
            .children(banner)
            .child(body)
            .children(self.notice.clone().map(|notice| {
                div()
                    .absolute()
                    .bottom(px(6.))
                    .left(px(10.))
                    .right(px(10.))
                    .px(px(8.))
                    .py(px(4.))
                    .rounded_md()
                    .bg(chrome.surface)
                    .border_1()
                    .border_color(chrome.border)
                    .text_size(px(11.))
                    .text_color(chrome.text_muted)
                    .child(notice)
            }))
    }
}

/// Reads one table's key and its first batch of rows.
///
/// **Blocks**, and is called from `cx.background_spawn` with a
/// [`SessionHandle`]. Both halves go through the session's own worker thread,
/// so they queue behind whatever else that connection is doing rather than
/// racing it.
///
/// The key first, because the grid marks its columns and the source has to be
/// told before it is built.
fn open(
    session: &Session,
    target: &ObjectTarget,
    sql: &str,
    fetch_rows: u32,
) -> Result<Opened, JdbcError> {
    let keys = primary_key(session, target)?;

    let spec = StatementSpec::new(sql.to_string()).with_fetch_size(fetch_rows);
    let mut cursor = session.execute(&spec)?;
    let mut steps = Vec::new();
    let pageable = advance(&mut cursor, true, fetch_rows, &mut steps)?;

    // The first — and, for a `SELECT` over one table, the only — result set.
    // Anything else the walk picked up is dropped rather than drawn: this pane
    // shows one table, and a second grid inside it would have nowhere to go.
    let rows = steps.into_iter().find_map(|step| match step {
        Step::Rows {
            columns,
            batch,
            complete,
        } => Some((columns, batch, complete)),
        Step::Message { .. } => None,
    });

    Ok(match rows {
        Some((columns, batch, complete)) => Opened {
            keys,
            columns,
            batch: Some(batch),
            complete,
            // `then_some` is what drops — and so closes — a cursor that is
            // parked on nothing pageable.
            cursor: pageable.then_some(cursor),
        },
        None => Opened {
            keys,
            columns: Vec::new(),
            batch: None,
            complete: true,
            cursor: None,
        },
    })
}

/// The primary key's columns, in key order.
///
/// `KEY_SEQ` decides the order rather than the order the driver listed them in:
/// a composite key's `WHERE` clause is written from this, and a key read in the
/// wrong order would still work while reading rather differently from the
/// table's own DDL.
///
/// A driver that answers nothing — a view, a table with no key — gives an empty
/// list, which is the read-only case rather than a failure.
fn primary_key(session: &Session, target: &ObjectTarget) -> Result<Vec<String>, JdbcError> {
    let mut request = DescribeRequest::new("primary_keys").with_table(&target.name);
    request.catalog = target.catalog.clone();
    request.schema = target.schema.clone();

    let mut found: Vec<(i64, String)> = session
        .describe(&request)?
        .items
        .iter()
        .filter_map(|item| {
            let column = text(item, "column")?;
            Some((number(item, "seq").unwrap_or(0), column.to_string()))
        })
        .collect();
    found.sort_by_key(|(seq, _)| *seq);
    Ok(found.into_iter().map(|(_, column)| column).collect())
}

#[cfg(test)]
mod tests {
    use gpui::{TestAppContext, WindowHandle};
    use rudbman_grid::GridCell;

    use super::*;
    use crate::app_settings;
    use crate::connection::{self, Connected};
    use crate::explorer::Folder;

    /// A live H2 database with `setup` already run against it.
    ///
    /// The same fixture the query pane's own tests build, for the same reason:
    /// everything under test here is a round trip, and a faked one would be
    /// testing the fake.
    fn h2(name: &str, setup: &[&str]) -> (Connected, ConnectionProfile) {
        let mut profile = connection::h2::profile(name);
        profile.url = format!("{};DB_CLOSE_DELAY=-1", profile.url);
        profile.read_only = false;
        let connected = connection::connect(
            &profile,
            &connection::h2::driver(),
            &connection::Credentials::typed(Some(String::new()), None),
            &AppSettings::default(),
        )
        .expect("H2 opens an in-memory database without a server");

        for sql in setup {
            connected
                .session()
                .execute(&StatementSpec::new(*sql))
                .unwrap_or_else(|error| panic!("{sql}: {error}"));
        }
        (connected, profile)
    }

    /// A table of the fixture's `APP` schema.
    fn target(name: &str) -> ObjectTarget {
        ObjectTarget {
            connection: ConnectionId(1),
            catalog: None,
            schema: Some("APP".to_string()),
            folder: Folder::Tables,
            name: name.to_string(),
        }
    }

    /// A window whose whole content is one data pane, already loading.
    fn pane(
        connected: &Connected,
        profile: &ConnectionProfile,
        target: ObjectTarget,
        batch_rows: u32,
        cx: &mut TestAppContext,
    ) -> WindowHandle<DataPane> {
        cx.update(|cx| {
            app_settings::init(cx);
            rudbman_ui::init(cx);
            rudbman_grid::init(cx);
        });
        let settings = AppSettings {
            fetch_batch_rows: batch_rows,
            ..AppSettings::default()
        };
        let session = connected.handle();
        let profile = profile.clone();
        let window = cx.add_window(move |_window, cx| {
            DataPane::new(
                session,
                ConnectionId(1),
                target,
                &profile,
                "h2",
                &settings,
                cx,
            )
        });
        window
            .update(cx, |pane, _window, cx| pane.refresh(cx))
            .expect("the window is open");
        cx.run_until_parked();
        window
    }

    /// Column `column` of the grid, as text.
    fn column(
        window: &WindowHandle<DataPane>,
        column: usize,
        cx: &mut TestAppContext,
    ) -> Vec<String> {
        window
            .update(cx, |pane, _window, cx| {
                let Load::Ready(rows) = &pane.load else {
                    panic!("the pane holds no rows: {:?}", pane.failure());
                };
                let grid = rows.grid.read(cx);
                let source = grid.source();
                (0..source.row_count())
                    .map(|row| match source.cell(row, column) {
                        GridCell::Text(text) => text.to_string(),
                        GridCell::Null => "NULL".to_string(),
                        GridCell::Lob { size } => format!("lob {size:?}"),
                    })
                    .collect()
            })
            .expect("the window is open")
    }

    impl DataPane {
        /// The failure the load reported, if it failed.
        fn failure(&self) -> Option<&SharedString> {
            match &self.load {
                Load::Failed(error) => Some(&error.message),
                _ => None,
            }
        }

        /// The state of the source under the grid.
        fn state(&self, cx: &App) -> GridSourceState {
            match &self.load {
                Load::Ready(rows) => rows.grid.read(cx).source().state(),
                _ => panic!("the pane holds no rows"),
            }
        }
    }

    /// The statement is assembled, not typed: the name is qualified and quoted
    /// by the dialect, and the ordering is a clause of the pane's own.
    #[gpui::test]
    fn the_statement_names_the_table_the_way_the_dialect_spells_it(cx: &mut TestAppContext) {
        let (connected, profile) = h2(
            "data-sql",
            &[
                "create schema if not exists APP",
                // Quoted in the fixture too: `ORDER` is a keyword, which is
                // exactly why the pane must not spell it bare either.
                "create table APP.\"ORDER\" (ID int primary key)",
            ],
        );
        let window = pane(&connected, &profile, target("ORDER"), 500, cx);

        window
            .update(cx, |pane, _window, _cx| {
                // `ORDER` is a keyword, so it is quoted — which is the whole
                // reason nothing here formats an identifier by hand.
                assert_eq!(pane.select_sql(), "SELECT * FROM APP.\"ORDER\"");
                pane.order = Some(Order {
                    column: 0,
                    name: "ID".to_string(),
                    direction: SortDirection::Descending,
                });
                assert_eq!(
                    pane.select_sql(),
                    "SELECT * FROM APP.\"ORDER\" ORDER BY ID DESC"
                );
            })
            .expect("the window is open");
        connected.close().expect("close");
    }

    /// The pane opens, draws the rows, and marks the key column.
    #[gpui::test]
    fn opening_a_table_shows_its_rows(cx: &mut TestAppContext) {
        let (connected, profile) = h2(
            "data-open",
            &[
                "create schema if not exists APP",
                "create table APP.PERSON (ID int primary key, NAME varchar(20))",
                "insert into APP.PERSON values (1, 'a'), (2, 'b'), (3, 'c')",
            ],
        );
        let window = pane(&connected, &profile, target("PERSON"), 500, cx);

        assert_eq!(column(&window, 0, cx), ["1", "2", "3"]);
        assert_eq!(column(&window, 1, cx), ["a", "b", "c"]);
        window
            .update(cx, |pane, _window, cx| {
                assert_eq!(pane.keys, ["ID"], "the key was read before the rows");
                assert!(
                    pane.read_only_reason().is_none(),
                    "a keyed table is edit-able"
                );
                let Load::Ready(rows) = &pane.load else {
                    panic!("the rows arrived");
                };
                let source = rows.grid.read(cx).source();
                assert!(source.column(0).primary_key, "the key column is unmarked");
                assert!(!source.column(1).primary_key);
                assert_eq!(pane.state(cx), GridSourceState::Complete);
            })
            .expect("the window is open");
        connected.close().expect("close");
    }

    /// A table with no primary key is browsed and says why it cannot be edited.
    #[gpui::test]
    fn a_table_without_a_primary_key_says_it_can_only_be_read(cx: &mut TestAppContext) {
        let (connected, profile) = h2(
            "data-no-key",
            &[
                "create schema if not exists APP",
                "create table APP.LOG (LINE varchar(20))",
                "insert into APP.LOG values ('one'), ('two')",
            ],
        );
        let window = pane(&connected, &profile, target("LOG"), 500, cx);

        assert_eq!(column(&window, 0, cx), ["one", "two"]);
        window
            .update(cx, |pane, _window, _cx| {
                assert!(
                    pane.keys.is_empty(),
                    "H2 reported a key for a keyless table"
                );
                assert_eq!(pane.read_only_reason(), Some(ts!("data.no_primary_key")));
            })
            .expect("the window is open");
        connected.close().expect("close");
    }

    /// A read-only profile says so instead, and says it about a table that has
    /// a key — so the two reasons cannot be confused for each other.
    #[gpui::test]
    fn a_read_only_profile_says_so_over_a_keyed_table(cx: &mut TestAppContext) {
        let (connected, mut profile) = h2(
            "data-read-only",
            &[
                "create schema if not exists APP",
                "create table APP.PERSON (ID int primary key)",
            ],
        );
        profile.read_only = true;
        let window = pane(&connected, &profile, target("PERSON"), 500, cx);

        window
            .update(cx, |pane, _window, _cx| {
                assert_eq!(pane.keys, ["ID"]);
                assert_eq!(pane.read_only_reason(), Some(ts!("data.read_only")));
            })
            .expect("the window is open");
        connected.close().expect("close");
    }

    /// Paging, against more rows than one batch holds, and then a sort that
    /// really reorders because the server did it.
    #[gpui::test]
    fn rows_are_paged_and_a_sort_is_a_re_run(cx: &mut TestAppContext) {
        let (connected, profile) = h2(
            "data-paging",
            &[
                "create schema if not exists APP",
                "create table APP.BIG (ID int primary key)",
                "insert into APP.BIG select X from system_range(1, 2500)",
            ],
        );
        let window = pane(&connected, &profile, target("BIG"), 1_000, cx);

        window
            .update(cx, |pane, _window, cx| {
                assert_eq!(
                    pane.row_count(cx),
                    Some(1_000),
                    "one batch, not the whole table"
                );
                assert_eq!(pane.state(cx), GridSourceState::HasMore);
            })
            .expect("the window is open");

        // What `GridEvent::NearEnd` does once the viewport reaches the end. The
        // event needs a laid-out window; the fetch it asks for does not.
        let mut pages = 0;
        loop {
            let complete = window
                .update(cx, |pane, _window, cx| {
                    pane.state(cx) == GridSourceState::Complete
                })
                .expect("the window is open");
            if complete {
                break;
            }
            window
                .update(cx, |pane, _window, cx| pane.fetch_more(cx))
                .expect("the window is open");
            cx.run_until_parked();
            pages += 1;
            assert!(pages < 10, "the source never reached the end");
        }
        let rows = column(&window, 0, cx);
        assert_eq!(rows.len(), 2_500);

        // Descending is a fresh statement, so the whole result comes back — and
        // the marker comes back with it, on the grid that replaced the one the
        // header click was on.
        window
            .update(cx, |pane, _window, cx| {
                pane.reorder(0, Some(SortDirection::Descending), cx);
            })
            .expect("the window is open");
        cx.run_until_parked();
        window
            .update(cx, |pane, _window, cx| {
                let Load::Ready(rows) = &pane.load else {
                    panic!("the rows arrived");
                };
                assert_eq!(
                    rows.grid.read(cx).sort(),
                    Some((0, SortDirection::Descending)),
                    "the new grid does not wear the order it is in"
                );
            })
            .expect("the window is open");
        assert_eq!(column(&window, 0, cx)[0], "2500");
        connected.close().expect("close");
    }

    /// Closing the connection tab leaves the rows where they are and takes the
    /// session away, which every path that would use one has to notice.
    #[gpui::test]
    fn a_detached_pane_keeps_its_rows_and_asks_for_nothing_more(cx: &mut TestAppContext) {
        let (connected, profile) = h2(
            "data-detached",
            &[
                "create schema if not exists APP",
                "create table APP.PERSON (ID int primary key)",
                "insert into APP.PERSON values (1), (2)",
            ],
        );
        let window = pane(&connected, &profile, target("PERSON"), 500, cx);
        assert_eq!(column(&window, 0, cx), ["1", "2"]);

        window
            .update(cx, |pane, _window, cx| {
                pane.detach(cx);
                assert!(!pane.is_attached());

                pane.fetch_more(cx);
                assert_eq!(pane.notice, Some(ts!("explorer.disconnected")));

                pane.notice = None;
                pane.reorder(0, Some(SortDirection::Ascending), cx);
                assert_eq!(pane.notice, Some(ts!("explorer.disconnected")));

                pane.notice = None;
                pane.refresh(cx);
                assert_eq!(pane.notice, Some(ts!("explorer.disconnected")));
            })
            .expect("the window is open");
        cx.run_until_parked();

        // The rows the live load produced are still there to read.
        assert_eq!(column(&window, 0, cx), ["1", "2"]);
        connected.close().expect("close");
    }

    #[test]
    fn every_label_the_pane_draws_has_a_translation() {
        for label in [
            ts!("data.loading"),
            ts!("data.refresh"),
            ts!("data.empty"),
            ts!("data.row_count", count = 2),
            ts!("data.no_primary_key"),
            ts!("data.read_only"),
            ts!("data.discard_first"),
            ts!("menu.view_data"),
        ] {
            assert!(!label.is_empty(), "empty label");
            assert!(!label.starts_with("data."), "untranslated {label:?}");
            assert!(!label.starts_with("menu."), "untranslated {label:?}");
        }
    }
}
