//! The query pane: an editor above, its results below, and the pipeline
//! between them.
//!
//! This is the pane the architecture document's §7.1 draws — a SQL editor over
//! a result area, in one leaf of the pane tree — and the whole of what makes
//! rudbman a thing that runs queries rather than a thing that browses metadata.
//!
//! # Two entry points, and the tree keeps its old one
//!
//! * **New query** (`Ctrl`/`Cmd`+`T`, or the menu row) opens an empty pane on
//!   the connection whose tab is showing.
//! * **Query the selected object** (`Ctrl`/`Cmd`+`Enter` with the focus in the
//!   explorer, or the menu row) opens one pre-filled with
//!   `SELECT * FROM <qualified name>`.
//!
//! Double clicking a table in the tree still opens the detail panel it always
//! did. Overloading that gesture would have meant choosing which of the two
//! things a double click means, and there is no answer that is right both for
//! the user who wants the columns and for the user who wants the rows.
//!
//! # One statement at a time, and cancellation goes round the queue
//!
//! A session serialises everything on its own worker thread (architecture
//! document, §4.2), so a fetch, a schema load and a `DESCRIBE` on one
//! connection queue behind each other whether or not this module arranges it.
//! What this module does arrange is that a *second* run is refused while one is
//! in flight: it would queue behind the first and look like a hang.
//!
//! [`rudbman_jdbc::Canceller`] deliberately does not queue — that is the point
//! of it — so the cancel button reaches a statement blocked inside the driver.
//!
//! # Generations
//!
//! Every run takes the next generation number, and every delivery carries the
//! generation it belongs to. A cancelled statement's batch can still be in
//! flight when the next run starts, and the number is what stops it landing in
//! the new run's grid. Nothing here relies on a task being dropped in time.
//!
//! # One cursor per statement
//!
//! A script is split into statements first, so each statement gets a cursor of
//! its own and two `SELECT`s are two independently pageable grids. The
//! `MORE_RESULTS` walk — [`advance`], which the data pane shares and which is
//! therefore in [`crate::query_source`] — is what a stored procedure needs, not
//! what a script needs.
//!
//! # A result that can be written back
//!
//! A `SELECT` somebody wrote has no single table behind it in general, but the
//! cases that do are recognisable from metadata the wire already carries, and
//! the architecture document's §7.9 says such a result should be editable.
//! [`source_table`] is that gate, and it is the whole of the judgement: every
//! column that names a source table must name the same one, at least one must,
//! and then — asked of the catalogue, not guessed — the table's primary key
//! must be present in the result in full. A column that names no table is a
//! computed one: read-only, but not disqualifying.
//!
//! Everything under the gate is the data pane's, reached rather than copied.
//! The staging buffer and the planner are [`crate::data_edit`]'s and the
//! transaction, the preview and the update-count guard are
//! [`crate::row_apply`]'s. What is written here is the gate, the second round
//! trip that resolves it, and the gestures.
//!
//! Two things this pane does *not* offer, both §7.9's:
//!
//! * **No inserts.** A result carries the columns the user selected, not the
//!   columns the table requires, so a row typed into `SELECT id, name FROM
//!   users` is missing every `NOT NULL` column that was not selected — and a
//!   row that did insert need not satisfy the query's own `WHERE`, so the
//!   re-run would not show it and the apply would look as though it failed.
//!   Inserting is what the data pane on that table is for.
//! * **No edit carried across a re-run.** A sort or a re-run replaces the
//!   source a staged edit is keyed to, so both ask first while anything is
//!   staged.

use std::sync::Arc;
use std::time::{Duration, Instant};

use gpui::{
    Action, AnyElement, App, Context, Div, DragMoveEvent, Entity, EntityId, EventEmitter,
    FocusHandle, Focusable, Hsla, IntoElement, ParentElement, Pixels, Point, Render, SharedString,
    Styled, Subscription, Window, div, prelude::*, px, relative,
};
use rudbman_core::{AppSettings, ConnectionProfile};
use rudbman_editor::editor::{
    Copy, Cut, Find, Paste, Redo, Replace, RunAll, RunSelection, RunStatement, SelectAll,
    ToggleComment, Undo,
};
use rudbman_editor::{EditorEvent, EditorView};
use rudbman_grid::{
    GridCell, GridEvent, GridSource, GridSourceState, GridView, MenuTarget, RowStatus,
    SortDirection,
};
use rudbman_jdbc::{
    BridgeErrorKind, Canceller, ColumnInfo, Cursor, Error as JdbcError, StatementSpec,
};
use rudbman_sql::{Dialect, TokenKind, lex, split_statements};
use rudbman_ui::{Button, ButtonVariant, ContextMenu, Theme, theme};

use crate::SHORTCUT_MODIFIER;
use crate::builder_sql;
use crate::connection::{ConnectError, SessionHandle};
use crate::context_menu::{self, MenuRow};
use crate::data_edit::{
    EditCounts, EditableSource, PlannedStatement, StagedCell, TableSource, plan_apply,
};
use crate::explorer::ConnectionId;
use crate::i18n::ts;
use crate::query_source::{
    Paged, RenderedBatch, ResultSource, Step, advance, column_name, key_index, page,
};
use crate::row_apply::{
    ApplyFailure, ApplyProblem, ApplyStop, apply_batch, plan_message, primary_key,
    render_apply_error, render_apply_preview, render_discard_confirm,
};

/// The statement keywords that read rather than write.
///
/// The judgement `confirm_writes` turns on, and deliberately a short list: a
/// first word that is not one of these is treated as a write, because being
/// asked about a harmless statement costs a keystroke and the other mistake
/// costs a table. Read off the lexer rather than off `trim().starts_with`, so
/// that a leading comment — which is where a script's explanation lives — is
/// skipped for free.
const READING_KEYWORDS: [&str; 5] = ["SELECT", "WITH", "SHOW", "EXPLAIN", "DESCRIBE"];

/// Alias the sort round trip wraps the original statement under.
///
/// A derived table needs a name in most dialects; this one is unlikely enough
/// not to collide with anything the user wrote.
const SORT_ALIAS: &str = "rudbman_sort";

/// Share of the pane the editor gets before anyone drags the divider.
const DEFAULT_EDITOR_SHARE: f32 = 0.45;

/// Smallest share either half of the pane may be dragged to.
const MIN_SHARE: f32 = 0.12;

/// Thickness of the invisible grab strip over the divider, in pixels.
const DIVIDER_GRAB: f32 = 6.;

/// How often the elapsed clock redraws while a statement runs.
const CLOCK_TICK: Duration = Duration::from_millis(100);

/// Longest statement preview the write confirmation shows, in characters.
const PREVIEW_CHARS: usize = 400;

/// The divider between the editor and the results, while it is being dragged.
///
/// Carries the pane it belongs to because gpui delivers drag moves to every
/// ancestor of the element the drag started on, and a pane inside another
/// pane's subtree would otherwise write its neighbour's ratio.
pub struct DraggedQueryDivider(EntityId);

/// The first token of `sql` that is neither whitespace nor a comment.
fn first_word<'a>(sql: &'a str, dialect: &Dialect) -> Option<&'a str> {
    lex(sql, dialect)
        .into_iter()
        .find(|token| !matches!(token.kind, TokenKind::Whitespace | TokenKind::Comment))
        .map(|token| &sql[token.start..token.end])
}

/// Whether `sql` is a statement `confirm_writes` should ask about.
///
/// Blank input and a buffer holding nothing but comments are neither reads nor
/// writes: there is no statement, and nothing will be sent.
pub fn is_write_statement(sql: &str, dialect: &Dialect) -> bool {
    first_word(sql, dialect).is_some_and(|word| {
        !READING_KEYWORDS
            .iter()
            .any(|keyword| word.eq_ignore_ascii_case(keyword))
    })
}

/// Quotes an identifier the way `dialect` spells a quoted identifier.
///
/// MySQL is the one that has to be told apart: `"` opens a *string* there
/// unless the server runs in `ANSI_QUOTES`, which no client can see, so a
/// column name in double quotes would be compared against its own text. The
/// backtick is what MySQL, SQLite and H2 all accept.
fn quote_identifier(name: &str, dialect: &Dialect) -> String {
    if dialect.syntax().double_quoted_strings {
        format!("`{}`", name.replace('`', "``"))
    } else {
        format!("\"{}\"", name.replace('"', "\"\""))
    }
}

/// Wraps `sql` so the server returns it in `column` order.
///
/// A derived table rather than an appended `ORDER BY`, because the original may
/// already have one and two of them do not compose. That is also this
/// approach's limit, and the limit is left visible on purpose: a statement a
/// dialect will not accept inside `FROM (…)` — SQL Server with an inner
/// `ORDER BY` and no `TOP`, a product that will not nest a `WITH` — fails, and
/// the driver's own refusal is shown rather than swallowed. Sorting is offered
/// only for reading statements, so nothing is ever re-executed for its side
/// effects.
pub fn order_by(sql: &str, column: &str, direction: SortDirection, dialect: &Dialect) -> String {
    let inner = sql.trim().trim_end_matches(';').trim_end();
    let order = match direction {
        SortDirection::Ascending => "ASC",
        SortDirection::Descending => "DESC",
    };
    format!(
        "SELECT * FROM ({inner}) {SORT_ALIAS} ORDER BY {} {order}",
        quote_identifier(column, dialect)
    )
}

/// A table's three name parts, each `None` where the driver would not say.
///
/// The order is the catalogue's own — catalog, schema, table — which is also
/// the order [`crate::row_apply::primary_key`] takes them in.
type TableName = (Option<String>, Option<String>, String);

/// One name part of a column's source, or `None` where the driver would not
/// say.
///
/// The filter is the first thing any of §7.9's gate does, and getting it wrong
/// is the one mistake that would matter: JDBC answers the **empty string**, not
/// null, for a column with no source table, so both spellings mean "unknown"
/// and a comparison that treated `""` as a name would have two computed columns
/// agreeing on a table nobody has.
fn name_part(part: &Option<String>) -> Option<&str> {
    part.as_deref().filter(|part| !part.is_empty())
}

/// The one table a result's columns were all read from, when there is one.
///
/// The first two clauses of §7.9's gate, and nothing else — the third, that the
/// table's primary key is present in the result, needs the catalogue and is
/// asked afterwards ([`QueryPane::keyed`]). Answers the table's `(catalog,
/// schema, table)` parts, each already normalised by [`name_part`].
///
/// * A column that names **no** table takes no part in the vote and is not
///   disqualifying. It is a computed column — an expression, a literal, an
///   aggregate — and refusing the whole result because of one would refuse
///   `SELECT id, name, name || '!' FROM users`, where the first two columns are
///   perfectly writable. [`crate::data_edit`]'s column rules are what keep the
///   third read-only.
/// * Every column that **does** name one must agree on the whole triple. Two
///   tables mean a join, and an `UPDATE` names one table.
/// * At least one column must name a table, or there is nothing to write to.
///
/// The metadata is a hint and is allowed to be (§7.9): a driver that reports an
/// alias where the table was asked for, or `""` for a schema it knows perfectly
/// well, only ever costs an editing offer — a wrong table name finds no primary
/// key and the result stays read-only, and a right name over rows that are not
/// its own is caught by the update count of exactly one that every generated
/// statement is checked against. The hint may offer editing; it can never make
/// a statement safe.
fn source_table(columns: &[ColumnInfo]) -> Option<TableName> {
    let mut found: Option<(Option<&str>, Option<&str>, &str)> = None;
    for column in columns {
        let Some(table) = name_part(&column.table) else {
            continue;
        };
        let candidate = (name_part(&column.catalog), name_part(&column.schema), table);
        match found {
            None => found = Some(candidate),
            Some(agreed) if agreed == candidate => {}
            // Two tables: a join, and there is no one row for a `WHERE` clause
            // over one key to name.
            Some(_) => return None,
        }
    }
    found.map(|(catalog, schema, table)| {
        (
            catalog.map(str::to_string),
            schema.map(str::to_string),
            table.to_string(),
        )
    })
}

/// The failure of one run, as the pane draws it.
///
/// Built from the bridge's own envelope (architecture document, §4.5) through
/// [`ConnectError`], which is where the message flattening and the `SQLSTATE`
/// class rule already live.
#[derive(Clone, Debug)]
pub struct QueryError {
    /// The driver's own words, with the first cause appended when there is one.
    pub message: SharedString,
    /// The `SQLSTATE`, exactly as it arrived.
    pub sql_state: Option<SharedString>,
    /// The failure category the bridge assigned.
    pub kind: BridgeErrorKind,
}

impl QueryError {
    /// Wraps whatever came back from the JNI layer.
    pub fn new(error: JdbcError) -> Self {
        let (sql_state, kind) = match &error {
            JdbcError::Bridge(bridge) => (
                bridge
                    .sql_state
                    .clone()
                    .map(SharedString::from)
                    .filter(|state| !state.is_empty()),
                bridge.kind,
            ),
            _ => (None, BridgeErrorKind::Unknown),
        };
        // `ConnectError::message` is the flattening that already exists: the
        // envelope's `Display` plus the first cause, and never the Java stack.
        let message = ConnectError::from(error).message();
        Self {
            message: message.into(),
            sql_state,
            kind,
        }
    }

    /// The leading two characters of the `SQLSTATE`, which is the only part
    /// drivers agree on (architecture document, §4.5).
    pub fn sql_state_class(&self) -> Option<&str> {
        self.sql_state
            .as_deref()
            .filter(|state| state.len() >= 2)
            .map(|state| &state[..2])
    }

    /// Whether this failure is the cancel button rather than the statement.
    ///
    /// Class `57` is "operator intervention", which is what a driver raises for
    /// a statement someone cancelled; `interrupted` is what the bridge reports
    /// when the JVM call itself was interrupted.
    pub fn is_cancelled(&self) -> bool {
        self.kind == BridgeErrorKind::Interrupted || self.sql_state_class() == Some("57")
    }

    /// A one-line suggestion, for the classes worth one.
    pub fn hint(&self) -> Option<SharedString> {
        if self.is_cancelled() {
            return Some(ts!("query.hint_cancelled"));
        }
        match self.sql_state_class() {
            // Syntax error or access rule violation: the two the class cannot
            // separate, and the two the user fixes in the same place.
            Some("42") => Some(ts!("query.hint_syntax")),
            Some("23") => Some(ts!("query.hint_constraint")),
            Some("28") => Some(ts!("query.hint_auth")),
            Some("08") => Some(ts!("query.hint_network")),
            _ => None,
        }
    }
}

/// One executed statement, handed back from the background thread.
struct Executed {
    /// The SQL, kept for the sort round trip.
    sql: String,
    /// The cursor. Still ours to page when `pageable`; closed on drop
    /// otherwise.
    cursor: Cursor,
    steps: Vec<Step>,
    /// Whether the cursor is parked on a result set that can still be paged.
    pageable: bool,
}

/// One result of one statement, as a tab of the result area.
struct ResultTab {
    /// Stable identity, so a grid's subscription finds its own tab however the
    /// list has been rebuilt since.
    id: u64,
    label: SharedString,
    body: ResultBody,
}

/// What a result tab shows.
enum ResultBody {
    /// Rows, in a grid.
    Rows(Box<RowsTab>),
    /// An update count, or a bare success.
    Message(SharedString),
}

/// The table one result's rows may be written back to.
///
/// Only ever built by [`QueryPane::keyed`], which is where all three of §7.9's
/// clauses have been answered: a result that has one of these has passed the
/// gate, and one that has not simply has `None`.
struct EditTarget {
    /// The table's name parts, most significant first, as
    /// [`builder_sql::table_parts`] writes them — the same shape the data
    /// pane's apply hands `rudbman_sql::plan_edits`.
    parts: Vec<String>,
    /// The primary key's columns, in key order.
    keys: Vec<String>,
}

/// A result set and everything needed to go on reading it.
struct RowsTab {
    /// The rows, under the staging overlay.
    ///
    /// An [`EditableSource`] whether or not the result turned out editable: a
    /// result that fails §7.9's gate is one whose `writable` stayed false, not
    /// a second kind of grid. That is what lets the paging, the sorting and the
    /// menus be written once.
    grid: Entity<GridView<EditableSource>>,
    /// The statement the sort round trip wraps.
    ///
    /// The one the *user* wrote, not the one that produced these rows: a
    /// re-sort of an already-sorted result wraps the original, so clicking a
    /// header ten times sends ten statements of the same size rather than one
    /// nested ten deep.
    sql: String,
    /// The statement that actually produced these rows.
    ///
    /// [`RowsTab::sql`] with whatever ordering was asked for wrapped around it,
    /// and what an applied batch re-runs: the rows have to come back in the
    /// order they went away in, or the reload would look like a second sort.
    executed: String,
    /// The result's logical column types, for rendering later batches.
    columns: Arc<Vec<ColumnInfo>>,
    /// Where an apply would write, once the gate has been answered.
    ///
    /// `None` while the key lookup is out, and `None` for good on a result that
    /// did not pass.
    table: Option<EditTarget>,
    /// Why this result may only be read, in one line above the grid.
    ///
    /// `None` both while the answer is on its way and on a result that is
    /// editable — the difference between those two is not worth a word to
    /// anybody, since a lookup in flight lasts one round trip and says nothing
    /// either way.
    read_only: Option<SharedString>,
    /// The cursor, while this result is still ours to page. `None` once the
    /// rows ran out, and while a fetch has it.
    cursor: Option<Cursor>,
    /// Whether the cursor is out with a background fetch.
    fetching: bool,
    /// Keeps the grid's subscription alive for as long as the tab.
    _events: Subscription,
}

/// What the pane is doing.
enum RunState {
    /// Nothing is running.
    Idle,
    /// A statement is in flight.
    Running(Box<Running>),
}

/// A run in flight.
struct Running {
    generation: u64,
    started: Instant,
    /// Reaches the driver without queueing behind the statement it aborts.
    canceller: Canceller,
    /// Whether the cancel has been issued, so the button says so and cannot be
    /// pressed twice.
    cancelling: bool,
}

/// How a finished run is summarised in the status bar.
struct Finished {
    rows: usize,
    elapsed: Duration,
}

/// What the pane asks the workspace for.
pub enum QueryPaneEvent {
    /// A write is about to run and the profile asks first.
    ///
    /// The workspace owns the modal: a dialog centred inside a pane is centred
    /// in the wrong box, and every other dialog in the application is already
    /// rendered at the window's root.
    ConfirmWrites(Box<ConfirmRequest>),
}

/// What the write confirmation shows.
pub struct ConfirmRequest {
    /// How many of the statements about to run are writes.
    pub count: usize,
    /// The first of them, trimmed to something a dialog can hold.
    pub preview: SharedString,
}

/// A right-click inside the pane, while the menu it asked for is open.
///
/// One field for both halves of the pane, which is what keeps them mutually
/// exclusive: a right-click in the grid puts away a menu the editor had open,
/// the way the backdrop under either of them would have.
enum PaneMenu {
    /// In the SQL editor. The caret and the selection are wherever they were —
    /// the menu is nearly always raised *over* a selection in order to copy or
    /// run it — so the rows read the editor rather than the press.
    Editor {
        /// Where the pointer was, in window coordinates.
        position: Point<Pixels>,
    },
    /// In the grid of one result tab.
    Grid {
        /// The tab, by [`ResultTab::id`] rather than by position: a menu is
        /// open across at most one frame, but the id is what every other path
        /// into a result already names, and a tab index is not stable under a
        /// re-run.
        id: u64,
        /// A cell or a heading, as the grid read the press.
        target: MenuTarget,
        /// Where the pointer was, in window coordinates.
        position: Point<Pixels>,
    },
}

/// The editor, the results, and the pipeline between them.
pub struct QueryPane {
    editor: Entity<EditorView>,
    /// The session everything here runs on, until the connection is closed.
    ///
    /// `None` once [`QueryPane::detach`] has run: the tab outlives its
    /// connection, because the SQL in the editor is the user's and closing a
    /// connection tab must not take it away, but nothing in it can be run any
    /// more.
    session: Option<SessionHandle>,
    /// Which connection tab this pane belongs to.
    connection: ConnectionId,
    dialect: Dialect,
    /// The profile refuses writes outright rather than confirming them.
    ///
    /// Both kinds of write: a statement the user typed, and an apply the result
    /// grid would send (§7.9 gives the profile the same veto over the second).
    read_only: bool,
    /// The profile asks before a write.
    confirm_writes: bool,
    /// Whether the product behind this session has transactions.
    ///
    /// What an apply's batch runs under; see [`DataPane::with_transactions`]
    /// for why a driver that would not say is taken as having them.
    ///
    /// [`DataPane::with_transactions`]: crate::data_pane::DataPane::with_transactions
    transactional: bool,
    /// The autocommit setting the session was opened with (§8).
    ///
    /// What an apply puts *back* rather than a flat `true`: a profile opened
    /// with autocommit off is a session the user asked to be in a transaction.
    restore_auto_commit: bool,
    /// Rows per `FETCH`, from the settings.
    fetch_rows: u32,
    /// The editor's share of the pane's height.
    editor_share: f32,
    results: Vec<ResultTab>,
    active_result: usize,
    error: Option<QueryError>,
    /// A line the pane wants to say without it being a failure — "already
    /// running", "the LOB viewer is not built yet".
    notice: Option<SharedString>,
    run: RunState,
    /// The generation of the newest run. Every delivery carries one, and one
    /// that is not this is an answer the user has already moved on from.
    generation: u64,
    /// Whether anything has been run in this pane at all.
    ran: bool,
    finished: Option<Finished>,
    /// The statements the write confirmation is holding up.
    pending: Option<Vec<String>>,
    /// The statement a sort round trip should keep offering to wrap.
    ///
    /// Set while a run was started by [`QueryPane::reorder`], whose SQL is a
    /// wrapper around what the user wrote. Without it every sort would wrap the
    /// last wrapper.
    sort_base: Option<String>,
    /// The marker the grid of the run in flight should wear when it arrives.
    ///
    /// A sort is a re-run, and a re-run replaces the grid — so the marker the
    /// header click moved would be thrown away with the grid that carried it,
    /// leaving the rows ordered and nothing saying so. Set by
    /// [`QueryPane::reorder`], taken by [`QueryPane::push_rows`], and cleared
    /// by any run that is not a sort.
    pending_sort: Option<(usize, SortDirection)>,
    /// Mints [`ResultTab::id`].
    next_tab_id: u64,
    /// The right-click menu of one half of the pane, while one is open.
    context_menu: Option<PaneMenu>,
    /// Whether "throw the staged edits away" is waiting to be confirmed.
    ///
    /// A modal of the pane's own, exactly as the data pane's is and for the
    /// same reason: it asks about work that lives in one tab, so the sheet
    /// belongs over that tab rather than over the workspace.
    confirm_discard: bool,
    /// The statements an apply would send, while they are being shown.
    ///
    /// The write confirmation §7.9 always raises, which is a superset of what
    /// the profile's `confirm_writes` asks for and therefore answers it — so
    /// the batch never goes through [`QueryPane::request`]'s question as well.
    preview: Option<ApplyPreview>,
    /// Whether a batch is out on the session.
    applying: bool,
    /// Why the last apply did not happen.
    ///
    /// Held apart from [`QueryPane::notice`] and drawn in the danger colour: a
    /// failed apply leaves every staged change where it was, and a line that
    /// faded in with the other passing remarks would be the wrong weight for
    /// "nothing you asked for has happened".
    apply_error: Option<Box<ApplyProblem>>,
    _editor_events: Subscription,
}

/// One planned batch, and the result tab it was planned against.
///
/// The tab is carried rather than assumed to still be the active one: a
/// confirmation is answered a frame or several later, and a batch that landed
/// on the wrong result would write rows nobody looked at.
struct ApplyPreview {
    /// [`ResultTab::id`] of the tab the statements were planned from.
    tab: u64,
    /// The statements, in the order they will run.
    statements: Vec<PlannedStatement>,
}

impl EventEmitter<QueryPaneEvent> for QueryPane {}

impl QueryPane {
    /// A pane over `session`, with `sql` already in the editor.
    ///
    /// `settings` supplies the batch size; the profile supplies the two write
    /// guards. Both are read once, when the pane opens: a pane that re-read
    /// them per statement would change behaviour under a run already in flight.
    ///
    /// The pane holds a [`SessionHandle`], which is what that type is for — it
    /// keeps the session, and the tunnel under it, alive while a fetch is out.
    /// Closing the connection tab therefore leaves a pane holding an open
    /// cursor working until the pane itself goes.
    ///
    /// `window` is threaded in for the editor's subscription, which
    /// [`cx.subscribe_in`] registers: a run ends in a result grid, and answering
    /// a double click in one with [`GridView::begin_edit`] means putting the
    /// keyboard in a field — which there is no window to do inside a plain
    /// `cx.subscribe` (§7.9).
    ///
    /// [`cx.subscribe_in`]: gpui::Context::subscribe_in
    // Eight arguments, and each one is a fact the pane cannot work out: the
    // session, which connection it belongs to, the two profile guards, the
    // batch size, the SQL to open with, and the window its subscriptions are
    // registered against. Gathering them into a struct would move the same
    // eight values one line up the call site.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        session: SessionHandle,
        connection: ConnectionId,
        profile: &ConnectionProfile,
        driver_dialect: &str,
        settings: &AppSettings,
        sql: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let dialect = Dialect::from_id(driver_dialect);
        let editor = cx.new(|cx| {
            let mut editor = EditorView::new(cx).dialect(dialect);
            if !sql.is_empty() {
                editor.set_text(sql, cx);
            }
            editor
        });
        let editor_events =
            cx.subscribe_in(
                &editor,
                window,
                |pane, editor, event, window, cx| match event {
                    EditorEvent::RunStatement { span } => {
                        let text = editor.read(cx).text();
                        let sql = span.sql(&text).to_string();
                        pane.request(vec![sql], window, cx);
                    }
                    EditorEvent::RunSelection { span } => {
                        let text = editor.read(cx).text();
                        let selected = text.get(span.clone()).unwrap_or_default().to_string();
                        pane.request(vec![selected], window, cx);
                    }
                    EditorEvent::RunAll => {
                        let text = editor.read(cx).text();
                        // Split rather than sent whole: one cursor per statement is
                        // what makes two `SELECT`s two independently pageable grids.
                        let statements: Vec<String> = split_statements(&text, &pane.dialect)
                            .into_iter()
                            .map(|span| span.sql(&text).to_string())
                            .collect();
                        pane.request(statements, window, cx);
                    }
                    // The editor holds no strings, so its menu is drawn here
                    // (architecture document, §7.8).
                    EditorEvent::ContextMenu { position } => {
                        pane.context_menu = Some(PaneMenu::Editor {
                            position: *position,
                        });
                        cx.notify();
                    }
                    EditorEvent::Changed | EditorEvent::SelectionChanged => {}
                },
            );

        Self {
            editor,
            session: Some(session),
            connection,
            dialect,
            read_only: profile.read_only,
            confirm_writes: profile.confirm_writes,
            transactional: true,
            restore_auto_commit: profile.auto_commit,
            fetch_rows: settings.fetch_batch_rows,
            editor_share: DEFAULT_EDITOR_SHARE,
            results: Vec::new(),
            active_result: 0,
            error: None,
            notice: None,
            run: RunState::Idle,
            generation: 0,
            ran: false,
            finished: None,
            pending: None,
            sort_base: None,
            pending_sort: None,
            next_tab_id: 1,
            context_menu: None,
            confirm_discard: false,
            preview: None,
            applying: false,
            apply_error: None,
            _editor_events: editor_events,
        }
    }

    /// Records whether the product behind the session has transactions.
    ///
    /// A builder step for the same reason the data pane's is: everything
    /// [`QueryPane::new`] takes is a fact about the profile or the settings,
    /// and this is a fact about the *product* — one the host already has from
    /// the `SESSION_INFO` the connection was opened with, so the pane spends no
    /// round trip on it.
    pub fn with_transactions(mut self, supported: bool) -> Self {
        self.transactional = supported;
        self
    }

    /// Which connection tab this pane runs against.
    pub fn connection(&self) -> ConnectionId {
        self.connection
    }

    /// Lets the session go, leaving the tab standing.
    ///
    /// The connection tab this pane belongs to has been closed. The
    /// [`SessionHandle`] is what keeps the session — and the SSH tunnel under it
    /// — alive while a fetch is out, so holding on to it would keep both open
    /// behind a pane nobody can run anything in, and §9.3's rule that a tunnel
    /// dies with its session would hold only until someone had opened an editor.
    ///
    /// What stays is everything that is the user's: the statement they wrote and
    /// the rows already fetched. Every path that would talk to the database
    /// refuses from here on, and the status bar says the pane is disconnected
    /// rather than idle.
    pub fn detach(&mut self, cx: &mut Context<Self>) {
        self.session = None;
        // A confirmation still on screen would run nothing if it were answered.
        self.pending = None;
        // The cursors of the results are answered by a session that is being
        // closed; dropping them is what stops a scroll asking it for a page.
        for tab in &mut self.results {
            if let ResultBody::Rows(rows) = &mut tab.body {
                rows.cursor = None;
            }
        }
        cx.notify();
    }

    /// Whether this pane still has a session behind it.
    #[cfg(test)]
    pub fn is_attached(&self) -> bool {
        self.session.is_some()
    }

    /// What is in the editor, for the shell's tests.
    ///
    /// The editor is the pane's own field and the workspace's tests are in
    /// another module, so opening a file into a pane can only be asserted
    /// through an accessor. Test-only: nothing on screen reads the buffer this
    /// way — the run pipeline goes through the editor's own events.
    #[cfg(test)]
    pub fn editor_text(&self, cx: &App) -> String {
        self.editor.read(cx).text()
    }

    /// Puts the keyboard in the editor.
    pub fn focus_editor(&self, window: &mut Window, cx: &mut Context<Self>) {
        let handle = self.editor.read(cx).focus_handle(cx);
        window.focus(&handle);
    }

    /// Whether the keyboard is anywhere inside this pane, as the last drawn
    /// frame had it.
    ///
    /// The pane has two focusable halves — the editor and the grid of the active
    /// result, which takes the focus when a cell is clicked — and
    /// [`Focusable::focus_handle`] can only name one of them. Deciding whether a
    /// tab about to stop being rendered is holding the keyboard has to ask about
    /// both, or a focus left on a grid strands exactly the way the workspace's
    /// `reclaim_focus` exists to prevent.
    pub fn contains_focus(&self, window: &Window, cx: &App) -> bool {
        if self
            .editor
            .read(cx)
            .focus_handle(cx)
            .contains_focused(window, cx)
        {
            return true;
        }
        self.results.iter().any(|tab| match &tab.body {
            ResultBody::Rows(rows) => rows
                .grid
                .read(cx)
                .focus_handle(cx)
                .contains_focused(window, cx),
            ResultBody::Message(_) => false,
        })
    }

    /// Whether a statement is in flight.
    pub fn is_running(&self) -> bool {
        matches!(self.run, RunState::Running(_))
    }

    /// The two right-hand status bar cells: rows, and elapsed time.
    ///
    /// Both blank while nothing has run, so the bar never carries a count from
    /// a pane whose results the user has already replaced.
    ///
    /// A detached pane says so instead of counting: its rows are a snapshot of a
    /// connection that is gone, and "idle" would read as a pane that is merely
    /// waiting to be told what to run.
    pub fn status_cells(&self) -> (SharedString, SharedString) {
        if self.session.is_none() {
            return (ts!("statusbar.disconnected"), SharedString::default());
        }
        match (&self.run, &self.finished) {
            (RunState::Running(running), _) => (
                ts!("query.running"),
                elapsed_label(running.started.elapsed()),
            ),
            (RunState::Idle, Some(finished)) => (
                ts!("query.row_count", count = finished.rows),
                elapsed_label(finished.elapsed),
            ),
            (RunState::Idle, None) => (SharedString::default(), SharedString::default()),
        }
    }

    /// Runs `statements`, once the profile's guards allow it.
    fn request(&mut self, statements: Vec<String>, window: &mut Window, cx: &mut Context<Self>) {
        let statements: Vec<String> = statements
            .into_iter()
            .filter(|sql| first_word(sql, &self.dialect).is_some())
            .collect();
        self.notice = None;

        if statements.is_empty() {
            self.notice = Some(ts!("query.no_statement"));
            cx.notify();
            return;
        }
        if self.has_pending_edits(cx) {
            // A run replaces every result, and a staged edit is keyed to a row
            // index of the source it was typed on and nothing else (§7.9).
            self.notice = Some(ts!("data.discard_first"));
            cx.notify();
            return;
        }
        if self.session.is_none() {
            // Said before the write confirmation rather than after it: being
            // asked whether to run something that cannot be run is worse than
            // being told there is nothing to run it on.
            self.notice = Some(ts!("explorer.disconnected"));
            cx.notify();
            return;
        }
        if self.is_running() {
            // The session would queue it behind the statement already running,
            // which looks like a hang rather than like a queue.
            self.notice = Some(ts!("query.busy"));
            cx.notify();
            return;
        }

        let writes = statements
            .iter()
            .filter(|sql| is_write_statement(sql, &self.dialect))
            .count();
        if writes > 0 {
            if self.read_only {
                // A refusal, not a question: the profile says this connection
                // does not write, and asking would offer something it will not
                // do anyway.
                self.error = None;
                self.notice = Some(ts!("query.read_only"));
                cx.notify();
                return;
            }
            if self.confirm_writes {
                let first = statements
                    .iter()
                    .find(|sql| is_write_statement(sql, &self.dialect))
                    .map(String::as_str)
                    .unwrap_or_default();
                let request = ConfirmRequest {
                    count: writes,
                    preview: preview(first),
                };
                self.pending = Some(statements);
                cx.emit(QueryPaneEvent::ConfirmWrites(Box::new(request)));
                cx.notify();
                return;
            }
        }

        self.start(statements, None, window, cx);
    }

    /// The write confirmation was accepted.
    pub fn confirmed(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(statements) = self.pending.take() else {
            return;
        };
        self.start(statements, None, window, cx);
    }

    /// The write confirmation was declined.
    pub fn declined(&mut self, cx: &mut Context<Self>) {
        self.pending = None;
        cx.notify();
    }

    /// Starts a run: clears the last one, takes the next generation, and hands
    /// the statements to a background pipeline.
    /// `sort_base` is the statement a later sort should wrap, for the runs
    /// whose own SQL is already a wrapper; `None` means "whatever was run".
    fn start(
        &mut self,
        statements: Vec<String>,
        sort_base: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // The one gate every run goes through, whichever way it was asked for:
        // a sort round trip and an accepted write confirmation arrive here
        // without passing `request` again.
        let Some(session) = self.session.clone() else {
            self.notice = Some(ts!("explorer.disconnected"));
            cx.notify();
            return;
        };

        self.generation += 1;
        let generation = self.generation;
        // Dropping the old tabs drops their cursors, and `Cursor::drop` closes
        // them — which is the `CLOSE_CURSOR` a cancelled run still owes.
        self.results.clear();
        self.active_result = 0;
        self.error = None;
        self.notice = None;
        self.finished = None;
        self.ran = true;
        // A menu naming a result tab that is about to be dropped would act on
        // whichever tab took its place, or on none at all.
        self.context_menu = None;
        // The rows a plan was made against are on their way out, so the plan
        // and the failure of the last attempt to send one go with them.
        self.preview = None;
        self.apply_error = None;
        self.confirm_discard = false;
        if sort_base.is_none() {
            // Not a sort round trip, so whatever order the last one left is
            // not the order this one comes back in.
            self.pending_sort = None;
        }
        self.sort_base = sort_base;
        self.run = RunState::Running(Box::new(Running {
            generation,
            started: Instant::now(),
            canceller: session.session().canceller(),
            cancelling: false,
        }));

        let fetch_rows = self.fetch_rows;

        // `spawn_in` rather than `spawn`: the grids this run ends in are
        // subscribed to with a window, because answering a double click with
        // [`GridView::begin_edit`] means putting the keyboard in a field and
        // there is no window to be had inside a plain `cx.subscribe` (§7.9).
        cx.spawn_in(window, async move |pane, cx| {
            for sql in statements {
                let outcome = cx
                    .background_spawn({
                        let session = session.clone();
                        async move {
                            let spec = StatementSpec::new(sql.clone()).with_fetch_size(fetch_rows);
                            let mut cursor = session.session().execute(&spec)?;
                            let mut steps = Vec::new();
                            let pageable = advance(&mut cursor, true, fetch_rows, &mut steps)?;
                            Ok::<Executed, JdbcError>(Executed {
                                sql,
                                cursor,
                                steps,
                                pageable,
                            })
                        }
                    })
                    .await;

                let carry_on = pane
                    .update_in(cx, |pane, window, cx| {
                        pane.deliver(generation, outcome, window, cx)
                    })
                    .unwrap_or(false);
                if !carry_on {
                    return;
                }
            }
            pane.update(cx, |pane, cx| pane.finish(generation, cx)).ok();
        })
        .detach();

        // The elapsed clock. A task of its own rather than something the render
        // works out, because nothing else would make the window redraw while
        // the driver is blocked.
        cx.spawn(async move |pane, cx| {
            loop {
                cx.background_executor().timer(CLOCK_TICK).await;
                let ticking = pane
                    .update(cx, |pane, cx| {
                        let ticking = matches!(
                            &pane.run,
                            RunState::Running(running) if running.generation == generation
                        );
                        if ticking {
                            cx.notify();
                        }
                        ticking
                    })
                    .unwrap_or(false);
                if !ticking {
                    return;
                }
            }
        })
        .detach();

        cx.notify();
    }

    /// Records what one statement produced. Answers whether the run goes on.
    fn deliver(
        &mut self,
        generation: u64,
        outcome: Result<Executed, JdbcError>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if generation != self.generation {
            // A superseded run's answer. Dropping `outcome` closes its cursor.
            return false;
        }
        match outcome {
            Ok(executed) => {
                let Executed {
                    sql,
                    cursor,
                    steps,
                    pageable,
                } = executed;
                // A cursor that cannot be paged is dropped here, which closes
                // it; `then_some` is what does the dropping.
                self.append(&sql, steps, pageable.then_some(cursor), window, cx);
                self.recount(cx);
                cx.notify();
                true
            }
            Err(error) => {
                self.error = Some(QueryError::new(error));
                self.finish(generation, cx);
                false
            }
        }
    }

    /// Turns one statement's results into tabs.
    ///
    /// `cursor` is the open cursor when the walk stopped on a result set with
    /// rows still to come; it goes to the last grid tab, which is the only one
    /// `MORE_RESULTS` has not already closed.
    fn append(
        &mut self,
        sql: &str,
        steps: Vec<Step>,
        cursor: Option<Cursor>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let mut cursor = cursor;
        // A run the sort round trip started executes a wrapper; the statement
        // the *next* sort has to wrap is the one underneath it.
        let base = self.sort_base.clone().unwrap_or_else(|| sql.to_string());
        let last_rows = steps
            .iter()
            .rposition(|step| matches!(step, Step::Rows { .. }));

        for (index, step) in steps.into_iter().enumerate() {
            match step {
                Step::Rows {
                    columns,
                    batch,
                    complete,
                } => {
                    let carried = (Some(index) == last_rows).then(|| cursor.take()).flatten();
                    let state = if complete && carried.is_none() {
                        GridSourceState::Complete
                    } else {
                        GridSourceState::HasMore
                    };
                    self.push_rows(
                        base.clone(),
                        sql.to_string(),
                        columns,
                        batch,
                        state,
                        carried,
                        window,
                        cx,
                    );
                }
                Step::Message { update_count } => {
                    let text = if update_count > 0 {
                        ts!("query.rows_affected", count = update_count)
                    } else {
                        ts!("query.executed")
                    };
                    let id = self.mint_id();
                    let label = ts!("query.result", index = self.results.len() + 1);
                    self.results.push(ResultTab {
                        id,
                        label,
                        body: ResultBody::Message(text),
                    });
                }
            }
        }
    }

    /// Mints a tab id.
    fn mint_id(&mut self) -> u64 {
        let id = self.next_tab_id;
        self.next_tab_id += 1;
        id
    }

    /// Adds a grid tab over one result set's first batch.
    ///
    /// `sql` is the statement a later sort should wrap and `executed` the one
    /// that produced these rows; see [`RowsTab::executed`].
    ///
    /// The grid is built **read-only** and the rows go on screen at once. What
    /// makes it editable is a second round trip — the table's primary key — and
    /// §7.9 will not have the rows wait for one: [`QueryPane::resolve_edits`]
    /// asks, and the answer either opens the grid or says why it stays shut.
    #[allow(clippy::too_many_arguments)]
    fn push_rows(
        &mut self,
        sql: String,
        executed: String,
        columns: Vec<ColumnInfo>,
        batch: RenderedBatch,
        state: GridSourceState,
        cursor: Option<Cursor>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let id = self.mint_id();
        let mut source = ResultSource::new(&columns);
        source.push(batch);
        source.set_state(state);
        // `Inferred`: the table this result writes back to is worked out from
        // the columns themselves, so a column that named none is computed and
        // takes no edit (see [`TableSource`]).
        let source = EditableSource::new(source, &columns, false, TableSource::Inferred);

        let grid = cx.new(|cx| GridView::new(source, cx));
        // The marker the sort that asked for this run was for, if it was one:
        // the grid it was set on has just been replaced by this one.
        if let Some(sort) = self.pending_sort.take() {
            grid.update(cx, |grid, cx| grid.set_sort(Some(sort), cx));
        }
        let events = cx.subscribe_in(&grid, window, move |pane, _grid, event, window, cx| {
            match event {
                GridEvent::NearEnd => pane.fetch_more(id, window, cx),
                GridEvent::SortRequested { column, direction } => {
                    pane.reorder(id, *column, *direction, window, cx);
                }
                GridEvent::CellActivated { row, column } => {
                    pane.open_cell(id, *row, *column, window, cx);
                }
                // Reachable now that a result can be written back to (§7.9).
                // The grid refuses to open a field over a cell its source calls
                // read-only, so anything that arrives here is a value the user
                // meant to change.
                GridEvent::EditCommitted { row, column, value } => {
                    let rudbman_grid::EditValue::Text(text) = value;
                    pane.stage(id, *row, *column, StagedCell::Text(text.clone()), cx);
                }
                // The grid holds no strings, so its menu is drawn here
                // (architecture document, §7.8).
                GridEvent::ContextMenu { target, position } => {
                    pane.context_menu = Some(PaneMenu::Grid {
                        id,
                        target: *target,
                        position: *position,
                    });
                    cx.notify();
                }
            }
        });

        let label = ts!("query.result", index = self.results.len() + 1);
        self.results.push(ResultTab {
            id,
            label,
            body: ResultBody::Rows(Box::new(RowsTab {
                grid,
                sql,
                executed,
                columns: Arc::new(columns),
                table: None,
                read_only: None,
                cursor,
                fetching: false,
                _events: events,
            })),
        });
        self.resolve_edits(id, cx);
    }

    /// Works out whether result tab `id` may be written back to, and opens it
    /// when it may.
    ///
    /// The two clauses of §7.9's gate that need no server are answered here;
    /// the third — the table's primary key, in full, among the columns that
    /// were read — is a `DESCRIBE` and goes out in the background, because the
    /// rows are already on screen and must not wait for it.
    fn resolve_edits(&mut self, id: u64, cx: &mut Context<Self>) {
        if self.read_only {
            // §8's veto, in the same words the data pane says it in: it is the
            // same fact about the same connection.
            self.mark_read_only(id, ts!("data.read_only"), cx);
            return;
        }
        // A detached pane can still be read; it cannot ask the catalogue
        // anything, so its results stay as they came.
        let Some(session) = self.session.clone() else {
            self.mark_read_only(id, ts!("query.not_editable"), cx);
            return;
        };
        let Some(tab) = self.rows_tab(id) else {
            return;
        };
        let Some((catalog, schema, table)) = source_table(&tab.columns) else {
            self.mark_read_only(id, ts!("query.not_editable"), cx);
            return;
        };
        let generation = self.generation;

        cx.spawn(async move |pane, cx| {
            let outcome = cx
                .background_spawn(async move {
                    let keys = primary_key(
                        session.session(),
                        catalog.as_deref(),
                        schema.as_deref(),
                        &table,
                    )?;
                    Ok::<_, JdbcError>(((catalog, schema, table), keys))
                })
                .await;
            pane.update(cx, |pane, cx| pane.keyed(generation, id, outcome, cx))
                .ok();
        })
        .detach();
    }

    /// Records what the key lookup for result tab `id` answered.
    ///
    /// Guarded by the generation **and** by the tab id, and it needs both: a
    /// script produces several result tabs from one run, so the generation
    /// alone would let one statement's key open another statement's grid.
    fn keyed(
        &mut self,
        generation: u64,
        id: u64,
        outcome: Result<(TableName, Vec<String>), JdbcError>,
        cx: &mut Context<Self>,
    ) {
        if generation != self.generation {
            // The rows this was asked about have been replaced.
            return;
        }
        let ((catalog, schema, table), keys) = match outcome {
            Ok(answer) => answer,
            Err(error) => {
                // Not a failure of the query: the rows are there and readable,
                // and all that is lost is an offer. Logged rather than shown,
                // for the same reason the offer is never explained in detail —
                // §7.9's hint is only ever allowed to *offer* editing.
                log::warn!("reading a query result's primary key failed: {error}");
                self.mark_read_only(id, ts!("query.not_editable"), cx);
                return;
            }
        };
        let parts = builder_sql::table_parts(catalog.as_deref(), schema.as_deref(), &table);
        let qualified = self.dialect.qualify(parts.iter().map(String::as_str));

        let Some(tab) = self.rows_tab(id) else {
            return;
        };
        // The third clause: every key column has to be among the columns that
        // were read, or the `WHERE` clause has no value to find the row by —
        // which is also what disqualifies most aggregates, since
        // `SELECT dept, COUNT(*) FROM emp GROUP BY dept` names `emp` and
        // carries none of its key. Matched through `key_index` over
        // `column_name`, which is the same matching `mark_primary_keys` and
        // `plan_apply` use, so an aliased key column is still found and no
        // fourth rule exists to drift from the other three.
        let names: Vec<String> = tab.columns.iter().map(column_name).collect();
        let complete = !keys.is_empty() && keys.iter().all(|key| key_index(&names, key).is_some());
        if !complete {
            self.mark_read_only(id, ts!("query.not_editable"), cx);
            return;
        }

        let grid = tab.grid.clone();
        tab.table = Some(EditTarget {
            parts,
            keys: keys.clone(),
        });
        grid.update(cx, |grid, cx| {
            // The table a copied `INSERT` names, now that one is known.
            grid.set_insert_table(Some(SharedString::from(qualified)));
            grid.source_mut(cx).make_editable(&keys);
        });
        cx.notify();
    }

    /// Records why result tab `id` may only be read.
    fn mark_read_only(&mut self, id: u64, reason: SharedString, cx: &mut Context<Self>) {
        if let Some(tab) = self.rows_tab(id) {
            tab.read_only = Some(reason);
        }
        cx.notify();
    }

    /// The rows tab with this id, if it is still there.
    fn rows_tab(&mut self, id: u64) -> Option<&mut RowsTab> {
        self.results
            .iter_mut()
            .find(|tab| tab.id == id)
            .and_then(|tab| match &mut tab.body {
                ResultBody::Rows(rows) => Some(&mut **rows),
                ResultBody::Message(_) => None,
            })
    }

    /// The grid has come within sight of the last row it holds.
    fn fetch_more(&mut self, id: u64, window: &mut Window, cx: &mut Context<Self>) {
        if self.session.is_none() {
            // `detach` has already dropped the cursors, so this is only reached
            // by a scroll that was already in flight; saying why the rows stop
            // is better than a grid that quietly ends early.
            self.notice = Some(ts!("explorer.disconnected"));
            cx.notify();
            return;
        }
        let generation = self.generation;
        let fetch_rows = self.fetch_rows;
        let Some(tab) = self.rows_tab(id) else {
            return;
        };
        if tab.fetching {
            return;
        }
        let Some(mut cursor) = tab.cursor.take() else {
            return;
        };
        tab.fetching = true;
        let columns = Arc::clone(&tab.columns);
        let grid = tab.grid.clone();
        // While a batch is on its way the source says so, which is what keeps a
        // fast scroll from asking once per frame.
        grid.update(cx, |grid, cx| {
            grid.source_mut(cx).set_state(GridSourceState::Loading);
        });

        cx.spawn_in(window, async move |pane, cx| {
            let outcome = cx
                .background_spawn(async move {
                    match page(&mut cursor, &columns, fetch_rows) {
                        Ok(paged) => Ok((cursor, paged)),
                        Err(error) => Err(error),
                    }
                })
                .await;
            pane.update_in(cx, |pane, window, cx| {
                pane.paged(id, generation, outcome, window, cx);
            })
            .ok();
        })
        .detach();
    }

    /// Records a page fetched for tab `id`.
    fn paged(
        &mut self,
        id: u64,
        generation: u64,
        outcome: Result<(Cursor, Paged), JdbcError>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if generation != self.generation {
            // The run this belonged to has been superseded. Dropping the
            // outcome closes its cursor, and the rows never reach a grid.
            return;
        }
        match outcome {
            Ok((cursor, paged)) => {
                let Paged {
                    batch,
                    complete,
                    steps,
                    pageable,
                } = paged;
                let mut cursor = Some(cursor);
                // The statement that ran, not the one a sort would wrap:
                // `append` works the second out of `sort_base` for itself.
                let sql = self.rows_tab(id).map(|tab| tab.executed.clone());

                if let Some(tab) = self.rows_tab(id) {
                    tab.fetching = false;
                    // A completed result set is behind the cursor now: the walk
                    // in `page` has already moved past it.
                    tab.cursor = if complete { None } else { cursor.take() };
                    let state = if complete {
                        GridSourceState::Complete
                    } else {
                        GridSourceState::HasMore
                    };
                    let grid = tab.grid.clone();
                    grid.update(cx, |grid, cx| {
                        let source = grid.source_mut(cx);
                        // Into the rows the server sent, under the overlay: the
                        // staged edits are keyed to base row indices, and
                        // appending moves none of them.
                        source.base_mut().push(batch);
                        source.set_state(state);
                    });
                }

                // Results the `MORE_RESULTS` walk picked up once this one ended.
                if !steps.is_empty()
                    && let Some(sql) = sql
                {
                    let carried = if pageable { cursor.take() } else { None };
                    self.append(&sql, steps, carried, window, cx);
                }
                self.recount(cx);
            }
            Err(error) => {
                if let Some(tab) = self.rows_tab(id) {
                    tab.fetching = false;
                    tab.cursor = None;
                    let grid = tab.grid.clone();
                    grid.update(cx, |grid, cx| {
                        grid.source_mut(cx).set_state(GridSourceState::Complete);
                    });
                }
                self.error = Some(QueryError::new(error));
            }
        }
        cx.notify();
    }

    /// A header was clicked: re-run the statement in that order.
    ///
    /// Only for reading statements. Re-executing an `INSERT` because a column
    /// heading was clicked is not a thing this program will do.
    fn reorder(
        &mut self,
        id: u64,
        column: usize,
        direction: Option<SortDirection>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.session.is_none() {
            // Sorting is a re-run, and there is nothing left to run it on. Said
            // here rather than left to `start`, so the grid's own header click
            // gets an answer even when the statement turns out unsortable.
            self.notice = Some(ts!("explorer.disconnected"));
            cx.notify();
            return;
        }
        if self.is_running() {
            self.notice = Some(ts!("query.busy"));
            cx.notify();
            return;
        }
        if self.has_pending_edits(cx) {
            // A re-run replaces the source the edits are keyed to; §7.9.
            self.notice = Some(ts!("data.discard_first"));
            cx.notify();
            return;
        }
        let dialect = self.dialect;
        let Some(tab) = self.rows_tab(id) else {
            return;
        };
        let base = tab.sql.clone();
        let name = tab
            .grid
            .read(cx)
            .source()
            .column(column)
            .name
            .trim()
            .to_string();
        if name.is_empty() || is_write_statement(&base, &dialect) {
            return;
        }
        let sql = match direction {
            Some(direction) => order_by(&base, &name, direction, &dialect),
            // The third click drops the ordering, which is the original query.
            None => base.clone(),
        };
        // Carried across the re-run, so the grid that comes back wears the
        // marker for the order it is actually in; see
        // [`QueryPane::pending_sort`].
        self.pending_sort = direction.map(|direction| (column, direction));
        self.start(vec![sql], Some(base), window, cx);
    }

    /// A cell was opened: a field over it, or a word about why not.
    ///
    /// A LOB has no body in the grid — only its size travelled — and reading
    /// one needs `LOB_READ` (0x25), which the bridge answers "not implemented"
    /// (architecture document, §12, open question 7). Saying so is better than
    /// a viewer that shows nothing.
    ///
    /// Anything else goes to [`GridView::begin_edit`], which refuses on its own
    /// for a cell that cannot take a field — a result that failed §7.9's gate,
    /// a computed column, a row on its way out — so there is nothing to check
    /// here that is not already checked where it is known.
    // TODO(M4): open a chunked LOB viewer once `LOB_READ` lands in the bridge.
    fn open_cell(
        &mut self,
        id: u64,
        row: usize,
        column: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(grid) = self.grid_of(id).cloned() else {
            return;
        };
        if matches!(
            grid.read(cx).source().cell(row, column),
            GridCell::Lob { .. }
        ) {
            self.notice = Some(ts!("query.lob_unsupported"));
            cx.notify();
            return;
        }
        grid.update(cx, |grid, cx| {
            grid.begin_edit(row, column, window, cx);
        });
    }

    /// Ends the run, whether it succeeded or not.
    fn finish(&mut self, generation: u64, cx: &mut Context<Self>) {
        if generation != self.generation {
            return;
        }
        let elapsed = match &self.run {
            RunState::Running(running) => running.started.elapsed(),
            RunState::Idle => Duration::ZERO,
        };
        self.run = RunState::Idle;
        self.finished = Some(Finished { rows: 0, elapsed });
        self.recount(cx);
        cx.notify();
    }

    /// Re-reads the row count the status bar shows.
    fn recount(&mut self, cx: &App) {
        let rows: usize = self
            .results
            .iter()
            .filter_map(|tab| match &tab.body {
                ResultBody::Rows(rows) => Some(rows.grid.read(cx).source().row_count()),
                ResultBody::Message(_) => None,
            })
            .sum();
        if let Some(finished) = &mut self.finished {
            finished.rows = rows;
        }
    }

    /// Asks the driver to abandon whatever is running.
    fn cancel(&mut self, cx: &mut Context<Self>) {
        let RunState::Running(running) = &mut self.run else {
            return;
        };
        if running.cancelling {
            return;
        }
        running.cancelling = true;
        let canceller = running.canceller.clone();
        // On a thread of its own: `Canceller::cancel` attaches to the JVM and
        // blocks, and the thread it must not block is the one drawing the
        // button that was just pressed.
        cx.background_spawn(async move {
            if let Err(error) = canceller.cancel() {
                log::warn!("cancelling the statement failed: {error}");
            }
        })
        .detach();
        cx.notify();
    }

    /// Moves the divider between the editor and the results.
    fn drag_divider(&mut self, event: &DragMoveEvent<DraggedQueryDivider>, cx: &mut Context<Self>) {
        if event.drag(cx).0 != cx.entity_id() {
            return;
        }
        let height = event.bounds.size.height;
        let share = f32::from(event.event.position.y - event.bounds.top()) / f32::from(height);
        if !share.is_finite() {
            return;
        }
        self.editor_share = share.clamp(MIN_SHARE, 1. - MIN_SHARE);
        cx.notify();
    }

    /// The strip above the results: the tabs, the clock, and the run controls.
    fn render_toolbar(&self, chrome: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let this = cx.entity();
        let running = self.is_running();
        let cancelling = matches!(&self.run, RunState::Running(running) if running.cancelling);

        let tabs: Vec<_> = self
            .results
            .iter()
            .enumerate()
            .map(|(index, tab)| {
                let active = index == self.active_result;
                let this = this.clone();
                div()
                    .id(("result-tab", tab.id))
                    .flex_none()
                    .px(px(10.))
                    .py(px(4.))
                    .cursor_pointer()
                    .text_size(px(12.))
                    .border_b_2()
                    .border_color(if active {
                        chrome.accent
                    } else {
                        gpui::transparent_black()
                    })
                    .text_color(if active {
                        chrome.text
                    } else {
                        chrome.text_muted
                    })
                    .when(!active, |tab| tab.hover(|tab| tab.bg(chrome.surface_hover)))
                    .child(tab.label.clone())
                    .on_click(move |_, _window, cx| {
                        this.update(cx, |pane, cx| {
                            pane.active_result = index;
                            cx.notify();
                        });
                    })
            })
            .collect();

        let run = {
            let this = this.clone();
            Button::new("query-run", ts!("query.run"))
                .variant(ButtonVariant::Primary)
                .disabled(running)
                .on_click(move |_, window, cx| {
                    this.update(cx, |pane, cx| {
                        let text = pane.editor.read(cx).text();
                        let statements = match pane.editor.read(cx).statement_at_caret() {
                            Some(span) => vec![span.sql(&text).to_string()],
                            None => Vec::new(),
                        };
                        pane.request(statements, window, cx);
                    });
                })
        };
        let cancel = running.then(|| {
            let this = this.clone();
            Button::new(
                "query-cancel",
                if cancelling {
                    ts!("query.cancelling")
                } else {
                    ts!("query.cancel")
                },
            )
            .variant(ButtonVariant::Danger)
            .disabled(cancelling)
            .on_click(move |_, _window, cx| {
                this.update(cx, |pane, cx| pane.cancel(cx));
            })
        });

        // Drawn only over a result that passed §7.9's gate. On one that did
        // not, nothing can be staged, so an Apply that could never light up
        // would be furniture — and the line under the toolbar has already said
        // why.
        let writable = self
            .active_rows()
            .is_some_and(|rows| rows.grid.read(cx).source().writable());
        let counts = self.counts(cx);
        let staged = counts.is_some();
        let pending = counts.map(|counts| {
            ts!(
                "data.pending",
                changed = counts.changed,
                inserted = counts.inserted,
                deleted = counts.deleted
            )
        });
        let apply = writable.then(|| {
            let this = this.clone();
            Button::new("query-apply", ts!("data.apply"))
                .variant(ButtonVariant::Primary)
                .disabled(!staged || running || self.applying)
                .on_click(move |_, window, cx| {
                    this.update(cx, |pane, cx| pane.apply(window, cx));
                })
        });
        let discard = writable.then(|| {
            let this = this.clone();
            Button::new("query-discard", ts!("data.discard"))
                .variant(ButtonVariant::Secondary)
                .disabled(!staged || running || self.applying)
                .on_click(move |_, _window, cx| {
                    this.update(cx, |pane, cx| {
                        pane.confirm_discard = true;
                        pane.preview = None;
                        cx.notify();
                    });
                })
        });

        div()
            .flex()
            .flex_row()
            .flex_none()
            .items_center()
            .gap(px(8.))
            .px(px(6.))
            .h(px(30.))
            .border_b_1()
            .border_color(chrome.border)
            .child(div().flex().flex_row().flex_1().min_w_0().children(tabs))
            .children(pending.map(|pending| {
                div()
                    .flex_none()
                    .whitespace_nowrap()
                    .text_size(px(11.))
                    // In the accent the grid marks a changed row with, so that
                    // the line and the markers under it read as one thing.
                    .text_color(chrome.accent)
                    .child(pending)
            }))
            .when(running, |bar| {
                bar.child(
                    div()
                        .flex_none()
                        .whitespace_nowrap()
                        .text_size(px(11.))
                        .text_color(chrome.text_muted)
                        .child(elapsed_label(self.elapsed())),
                )
            })
            .children(apply)
            .children(discard)
            .children(cancel)
            .child(run)
            .into_any_element()
    }

    /// The one line that says why the result showing cannot be written back to.
    ///
    /// Understated on purpose, and above the grid rather than in a dialog: it
    /// is the answer to a question the user has not asked yet, and §7.9 will
    /// not have it delivered by refusing a keystroke.
    fn render_banner(&self, chrome: &Theme) -> Option<impl IntoElement + use<>> {
        let reason = self.active_rows()?.read_only.clone()?;
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

    /// How long the run in flight has been going, or the last one took.
    fn elapsed(&self) -> Duration {
        match &self.run {
            RunState::Running(running) => running.started.elapsed(),
            RunState::Idle => self
                .finished
                .as_ref()
                .map_or(Duration::ZERO, |finished| finished.elapsed),
        }
    }

    /// The grid of result tab `id`, if that tab is still open and holds rows.
    fn grid_of(&self, id: u64) -> Option<&Entity<GridView<EditableSource>>> {
        self.results
            .iter()
            .find(|tab| tab.id == id)
            .and_then(|tab| match &tab.body {
                ResultBody::Rows(rows) => Some(&rows.grid),
                ResultBody::Message(_) => None,
            })
    }

    /// The result tab showing, when it holds rows.
    fn active_rows(&self) -> Option<&RowsTab> {
        match self.results.get(self.active_result) {
            Some(ResultTab {
                body: ResultBody::Rows(rows),
                ..
            }) => Some(rows),
            _ => None,
        }
    }

    /// The id of the result tab showing.
    fn active_id(&self) -> Option<u64> {
        self.results.get(self.active_result).map(|tab| tab.id)
    }

    /// Whether this pane is holding changes the user has not applied.
    ///
    /// Asked by every path that replaces a result wholesale — a run, a sort,
    /// and closing the tab — because a staged edit is keyed to a row index of
    /// the source it was typed on and nothing else (§7.9). Over *every* result
    /// and not only the one showing: a run replaces them all, and edits staged
    /// in a tab the user has scrolled away from are still edits.
    ///
    /// Public because the last of those three paths is the shell's: the tab
    /// strip is what closes a tab, so the tab strip is what has to ask.
    pub fn has_pending_edits(&self, cx: &App) -> bool {
        self.results.iter().any(|tab| match &tab.body {
            ResultBody::Rows(rows) => !rows.grid.read(cx).source().edits().is_empty(),
            ResultBody::Message(_) => false,
        })
    }

    /// Says, in the pane itself, why the gesture that was just tried is being
    /// refused while changes are staged.
    ///
    /// The counterpart of [`QueryPane::has_pending_edits`] for the shell: a tab
    /// that simply would not close, with nothing said, would read as a bug.
    pub fn warn_pending(&mut self, cx: &mut Context<Self>) {
        self.notice = Some(ts!("data.discard_first"));
        cx.notify();
    }

    /// How much is staged against the result showing, or `None` while nothing
    /// is.
    fn counts(&self, cx: &App) -> Option<EditCounts> {
        let counts = self.active_rows()?.grid.read(cx).source().edits().counts();
        (counts != EditCounts::default()).then_some(counts)
    }

    /// Records `value` against a cell of result tab `id`.
    ///
    /// The one way anything is staged. A value equal to the one the server gave
    /// un-stages the cell instead, which is why this goes through
    /// [`EditableSource::stage`] rather than writing into the buffer directly.
    fn stage(
        &mut self,
        id: u64,
        row: usize,
        column: usize,
        value: StagedCell,
        cx: &mut Context<Self>,
    ) {
        let Some(grid) = self.grid_of(id).cloned() else {
            return;
        };
        grid.update(cx, |grid, cx| {
            grid.source_mut(cx).stage(row, column, value);
        });
        cx.notify();
    }

    /// Puts NULL into a cell, deliberately.
    ///
    /// The gesture the inline editor cannot offer: an empty field over a null
    /// cell commits nothing, so clearing a cell has to be a command of its own.
    fn set_null(&mut self, id: u64, row: usize, column: usize, cx: &mut Context<Self>) {
        self.stage(id, row, column, StagedCell::Null, cx);
    }

    /// Marks a row to be deleted, or takes the mark off.
    fn toggle_delete(&mut self, id: u64, row: usize, cx: &mut Context<Self>) {
        let Some(grid) = self.grid_of(id).cloned() else {
            return;
        };
        grid.update(cx, |grid, cx| {
            let source = grid.source_mut(cx);
            // Nothing to delete outside the rows the server sent, and this pane
            // offers no way to add one: §7.9 gives a query result updates and
            // deletes and no inserts, because a result carries the columns the
            // user selected rather than the columns the table requires.
            if source.writable() && row < source.base_rows() {
                source.edits_mut().toggle_deleted(row);
            }
        });
        cx.notify();
    }

    /// Throws away everything staged against one row.
    fn discard_row(&mut self, id: u64, row: usize, cx: &mut Context<Self>) {
        let Some(grid) = self.grid_of(id).cloned() else {
            return;
        };
        grid.update(cx, |grid, cx| {
            let source = grid.source_mut(cx);
            let base = source.base_rows();
            source.edits_mut().discard_row(row, base);
        });
        cx.notify();
    }

    /// Throws away everything staged against one result, and puts the
    /// confirmation away with it.
    fn discard_all(&mut self, id: u64, cx: &mut Context<Self>) {
        self.confirm_discard = false;
        // A plan describes changes that are about to stop existing, and a
        // failure describes an attempt to send them.
        self.preview = None;
        self.apply_error = None;
        self.notice = None;
        let Some(grid) = self.grid_of(id).cloned() else {
            cx.notify();
            return;
        };
        grid.update(cx, |grid, cx| {
            grid.source_mut(cx).edits_mut().discard_all();
        });
        cx.notify();
    }

    /// The cell the grid's menu should act on: wherever the caret was left.
    ///
    /// The answer is a *source* column, because the selection is kept in
    /// display positions and everything on this side of the grid is addressed
    /// in source ones.
    fn menu_cell(&self, id: u64, cx: &App) -> Option<(usize, usize)> {
        let grid = self.grid_of(id)?.read(cx);
        let cursor = grid.selection().cursor()?;
        let column = grid.visible_column_indices().get(cursor.column).copied()?;
        Some((cursor.row, column))
    }

    /// Plans the staged changes of the result showing and puts the statements
    /// up for confirmation.
    ///
    /// Half of §7.9's apply, and the half that sends nothing.
    /// [`QueryPane::confirm_apply`] is what runs them.
    ///
    /// The read-only check is made again here even though the button that calls
    /// it is not drawn on a result that has one: this is the only function in
    /// the pane that leads to a generated write, and it is worth its two lines
    /// to have the refusal stated where the write is rather than only where the
    /// button is.
    fn apply(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        if self.read_only || self.applying || self.preview.is_some() {
            return;
        }
        // One modal at a time: the two are irreversible in opposite directions
        // and stacking them would leave the user answering the wrong one.
        self.confirm_discard = false;
        self.apply_error = None;

        let Some(id) = self.active_id() else {
            return;
        };
        let dialect = self.dialect;
        let planned = {
            let Some(rows) = self.active_rows() else {
                return;
            };
            let Some(target) = rows.table.as_ref() else {
                return;
            };
            let source = rows.grid.read(cx).source();
            if source.edits().is_empty() {
                return;
            }
            plan_apply(
                source,
                &rows.columns,
                target.parts.clone(),
                &target.keys,
                &dialect,
            )
        };

        match planned {
            // Nothing to send, which only a buffer that counted as non-empty
            // and generated no statement can produce.
            Ok(statements) if statements.is_empty() => {}
            Ok(statements) => {
                self.preview = Some(ApplyPreview {
                    tab: id,
                    statements,
                })
            }
            Err(error) => self.apply_error = Some(ApplyProblem::local(plan_message(&error))),
        }
        cx.notify();
    }

    /// Runs the statements the confirmation showed.
    ///
    /// The whole transaction — the autocommit flip, every statement, the row
    /// counts, the commit or the rollback — happens inside one background call
    /// on the session's own worker thread. Splitting it across awaits would
    /// leave the connection mid-transaction between them, which is a state no
    /// other pane on this session could safely be allowed to find.
    fn confirm_apply(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(ApplyPreview { tab, statements }) = self.preview.take() else {
            return;
        };
        if self.read_only {
            return;
        }
        let Some(session) = self.session.clone() else {
            self.notice = Some(ts!("explorer.disconnected"));
            cx.notify();
            return;
        };

        self.applying = true;
        self.apply_error = None;
        self.notice = Some(ts!("data.applying"));
        let generation = self.generation;
        let transactional = self.transactional;
        let restore = self.restore_auto_commit;

        cx.spawn_in(window, async move |pane, cx| {
            let outcome = cx
                .background_spawn(async move {
                    apply_batch(session.session(), &statements, transactional, restore)
                })
                .await;
            pane.update_in(cx, |pane, window, cx| {
                pane.applied(generation, tab, outcome, window, cx);
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    /// Records what one apply did.
    ///
    /// Success throws the staging buffer away and re-runs the statement that
    /// produced the rows, which is §7.9's answer to triggers and to anything
    /// else the server did on the way: what it now holds is a question only it
    /// can answer. Failure keeps every staged change exactly where it was —
    /// the rollback means nothing reached the table, so there is nothing to
    /// reconcile — and says why.
    fn applied(
        &mut self,
        generation: u64,
        id: u64,
        outcome: Result<usize, Box<ApplyFailure>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.applying = false;
        if generation != self.generation {
            // The rows this batch was staged against have been replaced.
            cx.notify();
            return;
        }
        match outcome {
            Ok(applied) => {
                // The pane's own re-run path and not a new one: the statement
                // that produced these rows, wrapped in whatever ordering was
                // asked for, with the user's own statement carried along as the
                // thing the *next* sort wraps.
                let rerun = self
                    .rows_tab(id)
                    .map(|tab| (tab.executed.clone(), tab.sql.clone()));
                let sort = self.grid_of(id).and_then(|grid| grid.read(cx).sort());
                // Cleared before the re-run, not after: `request` and `reorder`
                // refuse while anything is staged, and there is nothing left
                // worth keeping — the server has all of it.
                self.discard_all(id, cx);
                if let Some((executed, base)) = rerun {
                    // So the grid that comes back wears the order it is in; the
                    // one that was wearing it is about to be dropped.
                    self.pending_sort = sort;
                    self.start(vec![executed], Some(base), window, cx);
                }
                self.notice = Some(ts!("data.applied", count = applied));
            }
            Err(failure) => {
                self.notice = None;
                let half_applied = failure.half_applied;
                if let Some(error) = failure.rollback {
                    // The user is told that the batch may be half in; the
                    // driver's account of *why the unwind failed* is a second
                    // envelope with nowhere to go on screen.
                    log::error!("rolling back a failed apply failed: {error}");
                }
                let (error, message) = match failure.stop {
                    ApplyStop::Driver(error) => (Some(Box::new(QueryError::new(error))), None),
                    // §7.9's own words rather than a driver's: no statement
                    // failed, and what happened — a row somebody else has since
                    // moved — is not something a `SQLSTATE` describes.
                    ApplyStop::Stale => (None, Some(ts!("data.apply_stale"))),
                };
                self.apply_error = Some(Box::new(ApplyProblem {
                    error,
                    message,
                    half_applied,
                }));
            }
        }
        cx.notify();
    }

    /// Whether a menu is open in either half of the pane, for the shell's own
    /// tests.
    ///
    /// The workspace reaches every pane on `Escape` — a right click moves no
    /// pane marker, so the pane holding a menu is not necessarily the active
    /// one — and asserting that it did needs a way through.
    #[cfg(test)]
    pub(crate) fn has_context_menu(&self) -> bool {
        self.context_menu.is_some()
    }

    /// Opens the editor's menu, as a right click in it would.
    ///
    /// Test-only: the widget's own gesture is covered in `rudbman-editor`, and
    /// what the shell's tests need is a pane with a menu open on it.
    #[cfg(test)]
    pub(crate) fn open_editor_menu(&mut self, position: Point<Pixels>, cx: &mut Context<Self>) {
        self.context_menu = Some(PaneMenu::Editor { position });
        cx.notify();
    }

    /// Puts the pane's right-click menu away, and says whether there was one.
    ///
    /// What `Escape` reaches through the workspace, which closes the menu on
    /// top of everything before it closes anything else (architecture document,
    /// §7.8). The answer is what tells the workspace the key was spent here.
    pub fn close_context_menu(&mut self, cx: &mut Context<Self>) -> bool {
        let had = self.context_menu.take().is_some();
        if had {
            cx.notify();
        }
        had
    }

    /// The editor's right-click menu: everything a SQL buffer can be asked to
    /// do, in the order the keyboard already offers it.
    ///
    /// Every row is one of the editor's own actions, dispatched on the editor's
    /// focus handle — not called, because the editor exposes no method for any
    /// of them and should not have to: the menu and the chord are the same
    /// command reaching the same handler.
    ///
    /// What is greyed and why: the three clipboard rows follow the selection
    /// and whether the buffer may be written to, `Undo` and `Redo` follow the
    /// history, and the three run rows follow the session — a pane whose
    /// connection tab has been closed keeps its text and its rows and can run
    /// nothing (see [`QueryPane::detach`]).
    fn editor_rows(&self, cx: &App) -> Vec<MenuRow> {
        let editor = self.editor.read(cx);
        let handle = editor.focus_handle(cx);
        let selected = editor.has_selection();
        let writable = !editor.is_read_only();
        let attached = self.session.is_some();
        let row =
            |label: SharedString, shortcut: String, enabled: bool, action: Box<dyn Action>| {
                let handle = handle.clone();
                MenuRow::new(label)
                    .shortcut(shortcut)
                    .enabled(enabled)
                    .on_activate(move |window, cx| handle.dispatch_action(&*action, window, cx))
            };
        let modifier = SHORTCUT_MODIFIER;

        vec![
            row(
                ts!("context.cut"),
                format!("{modifier}+X"),
                selected && writable,
                Box::new(Cut),
            ),
            row(
                ts!("context.copy"),
                format!("{modifier}+C"),
                selected,
                Box::new(Copy),
            ),
            row(
                ts!("context.paste"),
                format!("{modifier}+V"),
                writable,
                Box::new(Paste),
            ),
            MenuRow::separator(),
            row(
                ts!("context.select_all"),
                format!("{modifier}+A"),
                true,
                Box::new(SelectAll),
            ),
            MenuRow::separator(),
            row(
                ts!("context.undo"),
                format!("{modifier}+Z"),
                editor.can_undo(),
                Box::new(Undo),
            ),
            row(
                ts!("context.redo"),
                format!("{modifier}+Shift+Z"),
                editor.can_redo(),
                Box::new(Redo),
            ),
            MenuRow::separator(),
            row(
                ts!("context.toggle_comment"),
                format!("{modifier}+/"),
                writable,
                Box::new(ToggleComment),
            ),
            MenuRow::separator(),
            row(
                ts!("context.run_statement"),
                format!("{modifier}+Enter"),
                attached,
                Box::new(RunStatement),
            ),
            row(
                ts!("context.run_selection"),
                format!("{modifier}+Alt+Enter"),
                attached && selected,
                Box::new(RunSelection),
            ),
            row(
                ts!("context.run_all"),
                format!("{modifier}+Shift+Enter"),
                attached,
                Box::new(RunAll),
            ),
            MenuRow::separator(),
            row(
                ts!("context.find"),
                format!("{modifier}+F"),
                true,
                Box::new(Find),
            ),
            row(
                ts!("context.replace"),
                format!("{modifier}+H"),
                writable,
                Box::new(Replace),
            ),
        ]
    }

    /// A result grid's right-click menu: the cell menu, or the heading one.
    ///
    /// Both lists start as [`crate::context_menu`]'s, because both are the same
    /// lists the data pane's grid draws (architecture document, §7.8) — the
    /// cell menu is about the *selection* rather than about the cell that was
    /// pressed, and the heading menu is about one column. What is this pane's
    /// own is where a sort goes: [`QueryPane::reorder`] wraps whatever the user
    /// wrote in a derived table, which no other pane has to do.
    ///
    /// The cell menu then carries the editing commands, which are the data
    /// pane's **minus its insert row** (§7.9: a result carries the columns the
    /// user selected, not the columns the table requires). They are drawn even
    /// where they cannot be run, greyed: a menu that changed shape between an
    /// editable result and a read-only one would make the user work out which
    /// of the two they were looking at, and the line above the grid has already
    /// said.
    fn grid_rows(&self, id: u64, target: MenuTarget, cx: &mut Context<Self>) -> Vec<MenuRow> {
        let Some(grid) = self.grid_of(id) else {
            return Vec::new();
        };
        let this = cx.entity();
        let grid = grid.clone();

        let MenuTarget::Cell = target else {
            let MenuTarget::Header { column } = target else {
                unreachable!("the grid has two menu targets");
            };
            return context_menu::grid_header_rows(
                &grid,
                column,
                cx,
                move |direction, window, cx| {
                    this.update(cx, |pane, cx| {
                        pane.reorder(id, column, direction, window, cx)
                    });
                },
            );
        };

        let mut rows = context_menu::grid_copy_rows(&grid, cx);
        let cell = self.menu_cell(id, cx);
        let source = grid.read(cx).source();
        let writable = source.writable();
        let deleted = cell.is_some_and(|(row, _)| source.row_status(row) == RowStatus::Deleted);
        // A cell may take a NULL when it may take anything at all *and* the
        // catalogue lets the column hold one. Both halves are the source's:
        // this side only asks.
        let nullable = cell.is_some_and(|(row, column)| {
            source.cell_editable(row, column) && source.nullable(column)
        });
        let staged = cell.is_some_and(|(row, _)| source.row_status(row) != RowStatus::Unchanged);
        let anything = !source.edits().is_empty();

        rows.push(MenuRow::separator());
        rows.push({
            let this = this.clone();
            MenuRow::new(ts!("data.set_null"))
                .enabled(writable && nullable)
                .on_activate(move |_window, cx| {
                    let Some((row, column)) = cell else {
                        return;
                    };
                    this.update(cx, |pane, cx| pane.set_null(id, row, column, cx));
                })
        });
        rows.push({
            let this = this.clone();
            // One row, two words: the label says what the command will do, so
            // it flips on a row that is already struck out.
            let label = if deleted {
                ts!("data.undelete_row")
            } else {
                ts!("data.delete_row")
            };
            MenuRow::new(label)
                .enabled(writable && cell.is_some())
                .on_activate(move |_window, cx| {
                    let Some((row, _)) = cell else {
                        return;
                    };
                    this.update(cx, |pane, cx| pane.toggle_delete(id, row, cx));
                })
        });
        rows.push(MenuRow::separator());
        rows.push({
            let this = this.clone();
            MenuRow::new(ts!("data.discard_row"))
                .enabled(staged)
                .on_activate(move |_window, cx| {
                    let Some((row, _)) = cell else {
                        return;
                    };
                    this.update(cx, |pane, cx| pane.discard_row(id, row, cx));
                })
        });
        rows.push(
            // The same command the toolbar's Discard is, confirmation and all:
            // it throws away work in both directions at once, so it is asked
            // about wherever it is offered from.
            MenuRow::new(ts!("data.discard"))
                .enabled(anything)
                .on_activate(move |_window, cx| {
                    this.update(cx, |pane, cx| {
                        pane.confirm_discard = true;
                        pane.preview = None;
                        cx.notify();
                    });
                }),
        );
        rows
    }

    /// The pane's right-click menu, while one is open.
    fn render_context_menu(&self, cx: &mut Context<Self>) -> Option<impl IntoElement + use<>> {
        let (position, rows) = match self.context_menu.as_ref()? {
            PaneMenu::Editor { position } => (*position, self.editor_rows(cx)),
            PaneMenu::Grid {
                id,
                target,
                position,
            } => (*position, self.grid_rows(*id, *target, cx)),
        };
        let this = cx.entity();

        Some(
            ContextMenu::new("query-context")
                .position(position)
                .entries(context_menu::entries(rows))
                .on_dismiss(move |_window, cx| {
                    this.update(cx, |pane, cx| {
                        pane.close_context_menu(cx);
                    });
                }),
        )
    }

    /// The result area's body: a grid, a message, a failure, or an empty state.
    fn render_results(&self, chrome: &Theme) -> AnyElement {
        if let Some(error) = &self.error {
            return render_error(error, chrome);
        }
        if self.results.is_empty() {
            let text = if self.is_running() {
                ts!("query.running")
            } else if self.ran {
                ts!("query.no_results")
            } else {
                ts!("query.empty")
            };
            return note(text, chrome.text_muted);
        }
        match self.results.get(self.active_result) {
            Some(ResultTab {
                body: ResultBody::Rows(rows),
                ..
            }) => div()
                .flex()
                .flex_1()
                .min_w_0()
                .min_h_0()
                .child(rows.grid.clone())
                .into_any_element(),
            Some(ResultTab {
                body: ResultBody::Message(text),
                ..
            }) => note(text.clone(), chrome.text),
            None => note(ts!("query.empty"), chrome.text_muted),
        }
    }
}

/// Formats an elapsed duration for the status bar.
fn elapsed_label(elapsed: Duration) -> SharedString {
    ts!(
        "query.elapsed",
        seconds = format!("{:.1}", elapsed.as_secs_f64())
    )
}

/// The first [`PREVIEW_CHARS`] characters of a statement, with an ellipsis when
/// it was cut.
fn preview(sql: &str) -> SharedString {
    let trimmed = sql.trim();
    match trimmed.char_indices().nth(PREVIEW_CHARS) {
        Some((at, _)) => SharedString::from(format!("{}…", &trimmed[..at])),
        None => SharedString::from(trimmed.to_string()),
    }
}

/// A centred line of text, for the states that have no rows to draw.
///
/// Shared with the data pane, which has the same four states to say something
/// in — loading, empty, failed, nothing run yet — and no reason to draw them
/// differently.
pub fn note(text: SharedString, color: Hsla) -> AnyElement {
    div()
        .flex()
        .flex_1()
        .min_w_0()
        .min_h_0()
        .items_center()
        .justify_center()
        .p(px(16.))
        .text_size(px(12.))
        .text_color(color)
        .child(text)
        .into_any_element()
}

/// The error envelope: what failed, its `SQLSTATE`, and a hint when the class
/// affords one.
///
/// Shared with the data pane for the reason [`note`] is: a driver's refusal
/// reads the same wherever the statement came from, and the hint is chosen from
/// the `SQLSTATE` class rather than from anything about the pane.
pub fn render_error(error: &QueryError, chrome: &Theme) -> AnyElement {
    error_lines(error, chrome)
        .flex_1()
        .min_w_0()
        .min_h_0()
        .p(px(16.))
        .into_any_element()
}

/// The lines an error envelope reads as, in a column and nothing else.
///
/// Split out of [`render_error`] because the data pane shows the same envelope
/// somewhere else. A failed load has no rows to draw, so its error stands where
/// they would have been; a failed *apply* leaves the rows — and everything
/// staged against them — exactly where they are, so its error is a strip above
/// them. Same three lines, two placements, one composition.
pub fn error_lines(error: &QueryError, chrome: &Theme) -> Div {
    let state = error
        .sql_state
        .clone()
        .map(|state| ts!("query.sql_state", state = state.to_string()));
    div()
        .flex()
        .flex_col()
        .gap(px(6.))
        .child(
            div()
                .text_size(px(12.))
                .text_color(chrome.danger)
                .child(error.message.clone()),
        )
        .children(state.map(|state| {
            div()
                .text_size(px(11.))
                .text_color(chrome.text_muted)
                .child(state)
        }))
        .children(error.hint().map(|hint| {
            div()
                .text_size(px(11.))
                .text_color(chrome.text_muted)
                .child(hint)
        }))
}

impl Focusable for QueryPane {
    /// The editor's handle: focusing the pane means putting the caret in the
    /// SQL, which is the only thing in here anyone types into.
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.editor.read(cx).focus_handle(cx)
    }
}

impl Render for QueryPane {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let chrome = theme(cx);
        let fonts = crate::app_settings::effective(cx);
        let share = self.editor_share.clamp(MIN_SHARE, 1. - MIN_SHARE);
        let id = cx.entity_id();
        let toolbar = self.render_toolbar(&chrome, cx);
        let banner = self.render_banner(&chrome);
        let failure = self
            .apply_error
            .as_ref()
            .map(|problem| render_apply_error(problem, &chrome));
        let results = self.render_results(&chrome);
        let context_menu = self.render_context_menu(cx);
        // Both modals are `row_apply`'s; what each button does is this pane's,
        // and it is said here rather than threaded through as an entity.
        let confirm = self
            .confirm_discard
            .then(|| self.counts(cx))
            .flatten()
            .zip(self.active_id())
            .map(|(counts, tab)| {
                let this = cx.entity();
                let discard = this.clone();
                render_discard_confirm(
                    counts,
                    cx,
                    move |_window, cx| {
                        this.update(cx, |pane, cx| {
                            pane.confirm_discard = false;
                            cx.notify();
                        });
                    },
                    move |_window, cx| {
                        discard.update(cx, |pane, cx| pane.discard_all(tab, cx));
                    },
                )
            });
        let preview = self.preview.as_ref().map(|preview| {
            let this = cx.entity();
            let run = this.clone();
            render_apply_preview(
                &preview.statements,
                self.transactional,
                cx,
                move |_window, cx| {
                    this.update(cx, |pane, cx| {
                        pane.preview = None;
                        cx.notify();
                    });
                },
                move |window, cx| {
                    run.update(cx, |pane, cx| pane.confirm_apply(window, cx));
                },
            )
        });

        div()
            .flex()
            .flex_col()
            .size_full()
            .min_w_0()
            .min_h_0()
            .relative()
            // Measured against this box rather than accumulated, exactly as the
            // workspace's own split dividers are: the seam follows the pointer
            // however far the gesture wandered.
            .on_drag_move::<DraggedQueryDivider>(cx.listener(
                |pane, event: &DragMoveEvent<DraggedQueryDivider>, _window, cx| {
                    pane.drag_divider(event, cx);
                },
            ))
            .child(
                // The editor draws with whatever text style it inherits, so
                // this wrapper is where the editor font settings take effect —
                // through `effective`, so the settings dialog's live preview
                // reaches the editor the same way it reaches the chrome. With
                // no family configured it falls back to the platform's
                // monospace default rather than the UI font: SQL is columnar
                // text, and the DDL tab already reads the same way.
                div()
                    .flex()
                    .flex_basis(relative(share))
                    .min_w_0()
                    .min_h_0()
                    .font_family(fonts.editor_font_family.clone().map_or_else(
                        || crate::app_settings::monospace_family(cx),
                        SharedString::from,
                    ))
                    .text_size(px(fonts.editor_font_size))
                    .child(self.editor.clone()),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_basis(relative(1. - share))
                    .min_w_0()
                    .min_h_0()
                    .border_t_1()
                    .border_color(chrome.border)
                    .child(toolbar)
                    .children(banner)
                    .children(failure)
                    .child(results),
            )
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
            // Last, so it wins the hit test against both halves it straddles.
            .child(
                div()
                    .id("query-divider")
                    .absolute()
                    .occlude()
                    .left_0()
                    .right_0()
                    .top(relative(share))
                    .mt(px(-DIVIDER_GRAB / 2.))
                    .h(px(DIVIDER_GRAB))
                    .cursor_ns_resize()
                    .on_drag(DraggedQueryDivider(id), |_, _, _, cx| {
                        cx.new(|_| gpui::Empty)
                    }),
            )
            // All last, and the menu last of all: a context menu paints above
            // even a modal (architecture document, §7.8). The two modals never
            // stand at once — raising either puts the other away — so their
            // order between themselves decides nothing.
            .children(confirm)
            .children(preview)
            // Takes no room in the column — the element is an empty absolute
            // box whose two halves are anchored to the window — so it can
            // simply be the last child, above the divider it may cover.
            .children(context_menu)
    }
}

#[cfg(test)]
mod tests {
    use gpui::{TestAppContext, WindowHandle};

    use super::*;
    use crate::app_settings;
    use crate::connection::{self, Connected};

    impl QueryPane {
        /// The grid of result tab `index`, for the assertions below.
        fn grid_at(&self, index: usize) -> &Entity<GridView<EditableSource>> {
            match &self.results[index].body {
                ResultBody::Rows(rows) => &rows.grid,
                ResultBody::Message(text) => panic!("result {index} is the message {text:?}"),
            }
        }

        /// The message of result tab `index`.
        fn message_at(&self, index: usize) -> &SharedString {
            match &self.results[index].body {
                ResultBody::Message(text) => text,
                ResultBody::Rows(_) => panic!("result {index} is a grid"),
            }
        }
    }

    /// A live H2 database with `setup` already run against it.
    ///
    /// `DB_CLOSE_DELAY=-1` keeps it alive between connections, exactly as the
    /// explorer's own fixture does.
    fn h2(name: &str, setup: &[&str]) -> (Connected, ConnectionProfile) {
        let mut profile = connection::h2::profile(name);
        profile.url = format!("{};DB_CLOSE_DELAY=-1", profile.url);
        // The guards are switched on by the one test that is about them; the
        // rest run writes as setup and would only be asking themselves.
        profile.confirm_writes = false;
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

    /// A window whose whole content is one query pane over `connected`.
    fn pane(
        connected: &Connected,
        profile: &ConnectionProfile,
        batch_rows: u32,
        cx: &mut TestAppContext,
    ) -> WindowHandle<QueryPane> {
        cx.update(|cx| {
            app_settings::init(cx);
            rudbman_ui::init(cx);
            rudbman_editor::init(cx);
            rudbman_grid::init(cx);
        });
        let settings = AppSettings {
            fetch_batch_rows: batch_rows,
            ..AppSettings::default()
        };
        let session = connected.handle();
        let profile = profile.clone();
        cx.add_window(move |window, cx| {
            QueryPane::new(
                session,
                ConnectionId(1),
                &profile,
                "h2",
                &settings,
                "",
                window,
                cx,
            )
        })
    }

    /// Runs `sql` and waits for the whole pipeline to settle.
    fn run(handle: &WindowHandle<QueryPane>, sql: &str, cx: &mut TestAppContext) {
        handle
            .update(cx, |pane, window, cx| {
                pane.request(vec![sql.to_string()], window, cx);
            })
            .expect("the window is open");
        cx.run_until_parked();
    }

    /// Column zero of the grid of result `index`, as text.
    fn column_zero(
        window: &WindowHandle<QueryPane>,
        index: usize,
        cx: &mut TestAppContext,
    ) -> Vec<String> {
        window
            .update(cx, |pane, _window, cx| {
                let source = pane.grid_at(index).read(cx).source();
                (0..source.row_count())
                    .map(|row| match source.cell(row, 0) {
                        GridCell::Text(text) => text.to_string(),
                        GridCell::Null => "NULL".to_string(),
                        // Unreachable over a query result, which stages
                        // nothing: only the data pane's overlay can leave a
                        // column to the server (§7.9).
                        GridCell::Default => "DEFAULT".to_string(),
                        GridCell::Lob { size } => format!("lob {size:?}"),
                    })
                    .collect()
            })
            .expect("the window is open")
    }

    /// The state of the grid of result `index`.
    fn state(
        window: &WindowHandle<QueryPane>,
        index: usize,
        cx: &mut TestAppContext,
    ) -> GridSourceState {
        window
            .update(cx, |pane, _window, cx| {
                pane.grid_at(index).read(cx).source().state()
            })
            .expect("the window is open")
    }

    #[test]
    fn a_comment_before_a_write_does_not_hide_it() {
        let h2 = Dialect::H2;
        // The whole reason the judgement goes through the lexer: a script's
        // explanation sits above the statement it explains.
        assert!(is_write_statement(
            "-- nightly clean-up\nUPDATE orders SET state = 'x'",
            &h2
        ));
        assert!(is_write_statement(
            "/* two\n   lines */ delete from orders",
            &h2
        ));
        assert!(is_write_statement("  \n\tinsert into t values (1)", &h2));
        assert!(is_write_statement("drop table t", &h2));
        assert!(is_write_statement("call do_something()", &h2));

        // And a `WITH` that only selects is a read, comment and all.
        assert!(!is_write_statement(
            "-- who ordered what\nWITH recent AS (SELECT * FROM orders) SELECT * FROM recent",
            &h2
        ));
        assert!(!is_write_statement("select 1", &h2));
        assert!(!is_write_statement("EXPLAIN select 1", &h2));
        assert!(!is_write_statement("show tables", &h2));

        // Nothing to send is neither: no statement, so no question.
        assert!(!is_write_statement("", &h2));
        assert!(!is_write_statement("   \n  ", &h2));
        assert!(!is_write_statement("-- only a comment", &h2));
    }

    /// One column's source metadata, spelled exactly as given.
    ///
    /// `None` writes a JSON `null` and `Some("")` writes an empty string,
    /// because those are the two spellings §7.9's gate has to treat alike and a
    /// builder that could not tell them apart would test neither.
    fn described(
        name: &str,
        catalog: Option<&str>,
        schema: Option<&str>,
        table: Option<&str>,
    ) -> ColumnInfo {
        fn part(value: Option<&str>) -> String {
            value.map_or_else(|| "null".to_string(), |value| format!("\"{value}\""))
        }
        serde_json::from_str(&format!(
            r#"{{"index":1,"name":"{name}","label":"{name}","table":{},"schema":{},
                 "catalog":{},"type":4,"type_name":"T","jdbc_type":"T",
                 "class_name":null,"precision":0,"scale":0,"display_size":0,
                 "nullable":2,"auto_increment":false,"signed":true,"read_only":false,"kind":4}}"#,
            part(table),
            part(schema),
            part(catalog)
        ))
        .expect("parses")
    }

    /// A column of `USERS`, as a driver that answers every part reports one.
    fn of_users(name: &str) -> ColumnInfo {
        described(name, Some("APP"), Some("PUBLIC"), Some("USERS"))
    }

    /// One table behind every column that names one is what the gate is for.
    #[test]
    fn one_table_behind_the_columns_is_the_table_a_result_writes_back_to() {
        assert_eq!(
            source_table(&[of_users("ID"), of_users("NAME")]),
            Some((
                Some("APP".to_string()),
                Some("PUBLIC".to_string()),
                "USERS".to_string()
            ))
        );
    }

    /// Two tables are a join, and an `UPDATE` names one.
    #[test]
    fn two_tables_among_the_columns_disqualify_the_result() {
        let columns = vec![
            of_users("ID"),
            described("NAME", Some("APP"), Some("PUBLIC"), Some("ORDERS")),
        ];
        assert_eq!(source_table(&columns), None);

        // The same table in another schema is another table, and so is the
        // same table in another catalogue: the whole triple has to agree.
        for other in [
            described("N", Some("APP"), Some("OTHER"), Some("USERS")),
            described("N", Some("OTHER"), Some("PUBLIC"), Some("USERS")),
        ] {
            assert_eq!(source_table(&[of_users("ID"), other]), None);
        }
    }

    /// A result whose every column is computed has nothing to write back to.
    #[test]
    fn a_result_of_only_computed_columns_names_no_table() {
        // `SELECT 1, count(*) FROM …`, as the two drivers spell it: one
        // answers null and the other the empty string.
        let columns = vec![
            described("C1", None, None, None),
            described("C2", Some(""), Some(""), Some("")),
        ];
        assert_eq!(source_table(&columns), None);
    }

    /// The empty string is not a name, wherever it turns up.
    #[test]
    fn an_empty_name_part_reads_as_unknown_and_not_as_a_name() {
        // A driver that returns `""` for the qualifiers it will not report and
        // one that returns null are describing the same table, so the two
        // spellings have to agree rather than count as two tables.
        let columns = vec![
            described("ID", Some(""), Some(""), Some("USERS")),
            described("NAME", None, None, Some("USERS")),
        ];
        assert_eq!(
            source_table(&columns),
            Some((None, None, "USERS".to_string())),
            "an empty catalogue or schema was taken for a name"
        );

        // And a column whose *table* is the empty string is computed, not a
        // column of the table called "".
        assert_eq!(source_table(&[described("C", None, None, Some(""))]), None);
    }

    /// §7.9's own example: one computed column does not refuse the two beside
    /// it, and the computed one is read-only where they are not.
    #[test]
    fn a_computed_column_is_read_only_rather_than_disqualifying() {
        // `SELECT id, name, name || '!' FROM users`.
        let columns = vec![
            of_users("ID"),
            of_users("NAME"),
            described("C3", Some(""), Some(""), Some("")),
        ];
        assert_eq!(
            source_table(&columns),
            Some((
                Some("APP".to_string()),
                Some("PUBLIC".to_string()),
                "USERS".to_string()
            )),
            "one computed column refused a result two of whose columns are writable"
        );

        // And what the gate lets through, the column rules still hold back:
        // there is no column for an `UPDATE` to assign the third one to.
        let mut base = ResultSource::new(&columns);
        base.push(crate::query_source::render_batch(
            &crate::query_source::tests::batch(&[&[Some("1")], &[Some("a")], &[Some("a!")]]),
            &columns,
        ));
        let mut source = EditableSource::new(base, &columns, false, TableSource::Inferred);
        source.make_editable(&["ID".to_string()]);
        assert!(source.cell_editable(0, 0));
        assert!(source.cell_editable(0, 1));
        assert!(
            !source.cell_editable(0, 2),
            "a computed column took an edit"
        );
    }

    #[test]
    fn sorting_wraps_the_statement_rather_than_appending_to_it() {
        // Appending would produce two `ORDER BY` clauses whenever the original
        // had one, which is a syntax error rather than a sort.
        let wrapped = order_by(
            "select id from t order by name;",
            "id",
            SortDirection::Descending,
            &Dialect::H2,
        );
        assert_eq!(
            wrapped,
            r#"SELECT * FROM (select id from t order by name) rudbman_sort ORDER BY "id" DESC"#
        );

        // MySQL spells a quoted identifier with backticks, because `"` opens a
        // string there.
        assert_eq!(
            order_by("select 1", "a b", SortDirection::Ascending, &Dialect::MYSQL),
            "SELECT * FROM (select 1) rudbman_sort ORDER BY `a b` ASC"
        );
        // And a quote inside the name is doubled, not dropped.
        assert_eq!(
            order_by(
                "select 1",
                "we\"ird",
                SortDirection::Ascending,
                &Dialect::H2
            ),
            "SELECT * FROM (select 1) rudbman_sort ORDER BY \"we\"\"ird\" ASC"
        );
    }

    /// The whole of infinite scrolling, against ten thousand real rows: the
    /// first batch arrives on its own, the source says there are more, and each
    /// page appends until the driver runs out and says so.
    #[gpui::test]
    fn ten_thousand_rows_arrive_one_batch_at_a_time(cx: &mut TestAppContext) {
        let (connected, profile) = h2(
            "paging",
            &[
                "create table BIG (ID int primary key)",
                "insert into BIG select X from system_range(1, 10000)",
            ],
        );
        let window = pane(&connected, &profile, 1_000, cx);
        run(&window, "select ID from BIG order by ID", cx);

        let id = window
            .update(cx, |pane, _window, cx| {
                assert!(pane.error.is_none(), "{:?}", pane.error);
                assert_eq!(pane.results.len(), 1);
                let source = pane.grid_at(0).read(cx).source();
                assert_eq!(source.row_count(), 1_000, "one batch, not the whole table");
                assert_eq!(source.state(), GridSourceState::HasMore);
                pane.results[0].id
            })
            .expect("the window is open");

        // What `GridEvent::NearEnd` does once the viewport reaches the end. The
        // event needs a laid-out window; the fetch it asks for does not.
        let mut pages = 0;
        while state(&window, 0, cx) != GridSourceState::Complete {
            window
                .update(cx, |pane, window, cx| pane.fetch_more(id, window, cx))
                .expect("the window is open");
            cx.run_until_parked();
            pages += 1;
            assert!(pages < 20, "the source never reached the end");
        }

        let rows = column_zero(&window, 0, cx);
        assert_eq!(rows.len(), 10_000);
        assert_eq!(rows[0], "1");
        assert_eq!(rows[9_999], "10000");
        window
            .update(cx, |pane, _window, _cx| {
                let (count, elapsed) = pane.status_cells();
                assert_eq!(count, ts!("query.row_count", count = 10_000));
                assert!(!elapsed.is_empty(), "the status bar reports a duration");
            })
            .expect("the window is open");
    }

    /// A script of three statements: two grids and a row count, in the order
    /// they were written.
    #[gpui::test]
    fn a_script_makes_one_result_per_statement(cx: &mut TestAppContext) {
        let (connected, profile) = h2(
            "script",
            &[
                "create table T (ID int primary key, N varchar(10))",
                "insert into T values (1, 'a'), (2, 'b'), (3, 'c')",
            ],
        );
        let window = pane(&connected, &profile, 500, cx);
        window
            .update(cx, |pane, _window, cx| {
                pane.editor.update(cx, |editor, cx| {
                    editor.set_text(
                        "select ID from T order by ID;\n\
                         update T set N = 'x' where ID <= 2;\n\
                         select N from T order by ID;",
                        cx,
                    );
                    // Exactly what the editor raises for "run everything", so
                    // the split and the subscription are both under test.
                    cx.emit(EditorEvent::RunAll);
                });
            })
            .expect("the window is open");
        cx.run_until_parked();

        window
            .update(cx, |pane, _window, _cx| {
                assert!(pane.error.is_none(), "{:?}", pane.error);
                assert_eq!(pane.results.len(), 3, "two grids and one row count");
                assert_eq!(
                    pane.message_at(1),
                    &ts!("query.rows_affected", count = 2),
                    "the update count is the driver's, not a guess"
                );
            })
            .expect("the window is open");

        assert_eq!(column_zero(&window, 0, cx), ["1", "2", "3"]);
        assert_eq!(column_zero(&window, 2, cx), ["x", "x", "c"]);
    }

    /// A statement that would never finish, cancelled, and a session that is
    /// still usable afterwards.
    ///
    /// The cancel comes from a thread of its own rather than from the button:
    /// the test executor runs the blocking `EXECUTE` on the very thread that
    /// would otherwise deliver the click, and [`Canceller`] is `Send + Sync`
    /// for exactly this reason.
    #[gpui::test]
    fn a_cancelled_statement_says_so_and_leaves_the_session_working(cx: &mut TestAppContext) {
        let (connected, profile) = h2("cancel", &[]);
        let canceller = connected.session().canceller();
        let window = pane(&connected, &profile, 500, cx);

        let stopper = std::thread::spawn(move || {
            for _ in 0..60 {
                std::thread::sleep(Duration::from_millis(200));
                if canceller.cancel().unwrap_or(0) > 0 {
                    return true;
                }
            }
            false
        });

        run(
            &window,
            "select count(*) from system_range(1, 200000) a, system_range(1, 200000) b \
             where a.X <> b.X",
            cx,
        );
        let reached = stopper.join().expect("the cancelling thread finished");

        window
            .update(cx, |pane, _window, _cx| {
                assert!(reached, "the cancel never reached a running statement");
                let error = pane.error.as_ref().expect("a cancel is a failure here");
                assert!(
                    error.is_cancelled(),
                    "kind {:?}, sqlstate {:?}: {}",
                    error.kind,
                    error.sql_state,
                    error.message
                );
                assert_eq!(error.hint(), Some(ts!("query.hint_cancelled")));
                assert!(!pane.is_running(), "the run ended with the cancel");
            })
            .expect("the window is open");

        // The session is the interesting part: a cancel that left the
        // connection unusable would be worse than no cancel at all.
        run(&window, "select 42", cx);
        window
            .update(cx, |pane, _window, _cx| {
                assert!(pane.error.is_none(), "{:?}", pane.error);
            })
            .expect("the window is open");
        assert_eq!(column_zero(&window, 0, cx), ["42"]);
    }

    /// The sort round trip really reorders, because the server does it.
    #[gpui::test]
    fn sorting_comes_back_in_the_new_order(cx: &mut TestAppContext) {
        let (connected, profile) = h2(
            "sorting",
            &[
                "create table S (ID int primary key)",
                "insert into S values (3), (1), (2)",
            ],
        );
        let window = pane(&connected, &profile, 500, cx);
        run(&window, "select ID from S", cx);

        // What the statement answers with no ordering at all. Not asserted as a
        // literal: an unordered `SELECT` may come back in any order, and here it
        // happens to arrive from the primary key index.
        let unsorted = column_zero(&window, 0, cx);

        for (direction, expected) in [
            (
                Some(SortDirection::Descending),
                vec!["3".to_string(), "2".to_string(), "1".to_string()],
            ),
            (
                Some(SortDirection::Ascending),
                vec!["1".to_string(), "2".to_string(), "3".to_string()],
            ),
            // The third click drops the ordering, which is the original query.
            (None, unsorted.clone()),
        ] {
            // A re-run replaces the tab, so the id has to be read afresh — the
            // same thing the grid's own subscription does when it fires again.
            let id = window
                .update(cx, |pane, _window, _cx| pane.results[0].id)
                .expect("the window is open");
            window
                .update(cx, |pane, sorted, cx| {
                    pane.reorder(id, 0, direction, sorted, cx);
                })
                .expect("the window is open");
            cx.run_until_parked();
            window
                .update(cx, |pane, _window, _cx| {
                    assert!(pane.error.is_none(), "{:?}", pane.error);
                    // Every sort wraps the statement the user wrote, never the
                    // wrapper the last one produced.
                    let ResultBody::Rows(rows) = &pane.results[0].body else {
                        panic!("a grid");
                    };
                    assert_eq!(rows.sql, "select ID from S");
                })
                .expect("the window is open");
            assert_eq!(column_zero(&window, 0, cx), expected, "{direction:?}");
        }
    }

    /// A page of a superseded run never lands in the run that replaced it.
    #[gpui::test]
    fn a_superseded_page_never_reaches_the_new_grid(cx: &mut TestAppContext) {
        let (connected, profile) = h2(
            "generations",
            &[
                "create table G (ID int primary key)",
                "insert into G select X from system_range(1, 3000)",
            ],
        );
        let window = pane(&connected, &profile, 1_000, cx);
        run(&window, "select ID from G order by ID", cx);

        let id = window
            .update(cx, |pane, _window, _cx| {
                assert_eq!(pane.results.len(), 1);
                pane.results[0].id
            })
            .expect("the window is open");

        // A page goes out and, before it is answered, the user runs something
        // else. Neither task has run yet: nothing here is parked.
        window
            .update(cx, |pane, live, cx| {
                pane.fetch_more(id, live, cx);
                pane.request(vec!["select 7".to_string()], live, cx);
            })
            .expect("the window is open");
        cx.run_until_parked();

        window
            .update(cx, |pane, _window, _cx| {
                assert!(pane.error.is_none(), "{:?}", pane.error);
                assert_eq!(pane.results.len(), 1, "the old tab went with its run");
            })
            .expect("the window is open");
        assert_eq!(
            column_zero(&window, 0, cx),
            ["7"],
            "the old run's thousand rows must not be in the new grid"
        );
    }

    /// The two write guards: one refuses, the other asks.
    #[gpui::test]
    fn a_write_is_refused_outright_or_asked_about(cx: &mut TestAppContext) {
        let (connected, mut profile) = h2(
            "guards",
            &["create table W (ID int primary key, N varchar(10))"],
        );

        profile.read_only = true;
        let refusing = pane(&connected, &profile, 500, cx);
        run(&refusing, "insert into W values (1, 'a')", cx);
        refusing
            .update(cx, |pane, _window, _cx| {
                assert!(!pane.ran, "a read-only profile never sends the statement");
                assert_eq!(pane.notice, Some(ts!("query.read_only")));
                assert!(pane.pending.is_none(), "a refusal is not a question");
            })
            .expect("the window is open");
        // And a read goes through on the same pane.
        run(&refusing, "select count(*) from W", cx);
        assert_eq!(column_zero(&refusing, 0, cx), ["0"]);

        profile.read_only = false;
        profile.confirm_writes = true;
        let asking = pane(&connected, &profile, 500, cx);
        run(&asking, "-- add one\ninsert into W values (2, 'b')", cx);
        asking
            .update(cx, |pane, _window, _cx| {
                assert!(!pane.ran, "nothing runs until the question is answered");
                assert!(pane.pending.is_some());
            })
            .expect("the window is open");

        asking
            .update(cx, |pane, window, cx| pane.confirmed(window, cx))
            .expect("the window is open");
        cx.run_until_parked();
        asking
            .update(cx, |pane, _window, _cx| {
                assert!(pane.error.is_none(), "{:?}", pane.error);
                assert_eq!(pane.message_at(0), &ts!("query.rows_affected", count = 1));
            })
            .expect("the window is open");
    }

    /// The pane has two focusable halves and [`Focusable`] can name only one of
    /// them, which is why the shell asks [`QueryPane::contains_focus`] instead
    /// before it stops rendering a tab: a click in the grid puts the keyboard
    /// somewhere the editor's handle knows nothing about.
    #[gpui::test]
    fn a_focused_result_grid_counts_as_focus_inside_the_pane(cx: &mut TestAppContext) {
        let (connected, profile) = h2("grid-focus", &["create table G (ID int primary key)"]);
        let window = pane(&connected, &profile, 500, cx);
        run(&window, "select ID from G", cx);

        window
            .update(cx, |pane, window, cx| {
                // Nothing is focused yet, so neither answer can be true by
                // accident.
                assert!(!pane.contains_focus(window, cx));

                // What clicking a cell amounts to, without the mouse.
                pane.grid_at(0).read(cx).focus_handle(cx).focus(window);
                assert!(
                    !pane.focus_handle(cx).contains_focused(window, cx),
                    "the editor's handle answered for a focus that is not inside it"
                );
                assert!(
                    pane.contains_focus(window, cx),
                    "a focus in the grid would strand when the tab stops rendering"
                );

                pane.focus_editor(window, cx);
                assert!(pane.contains_focus(window, cx));
            })
            .expect("the window is open");
        connected.close().expect("close");
    }

    /// Closing the connection tab leaves the SQL and the rows where they are
    /// and takes the session away, which every path that would use one has to
    /// notice rather than reach for a handle that is gone.
    #[gpui::test]
    fn a_detached_pane_keeps_its_rows_and_refuses_to_run(cx: &mut TestAppContext) {
        let (connected, profile) = h2(
            "detached",
            &[
                "create table D (ID int primary key)",
                "insert into D values (1), (2), (3)",
            ],
        );
        let window = pane(&connected, &profile, 500, cx);
        run(&window, "select ID from D order by ID", cx);
        assert_eq!(column_zero(&window, 0, cx), ["1", "2", "3"]);

        window
            .update(cx, |pane, _window, cx| pane.detach(cx))
            .expect("the window is open");

        // The rows the user already has are theirs to read; the status bar is
        // what says they are a snapshot of a connection that has gone.
        assert_eq!(column_zero(&window, 0, cx), ["1", "2", "3"]);
        window
            .update(cx, |pane, _window, _cx| {
                assert!(!pane.is_attached());
                assert_eq!(pane.status_cells().0, ts!("statusbar.disconnected"));
            })
            .expect("the window is open");

        // Every way in refuses with the same wording: a run, a sort round trip,
        // and a page the grid asks for as it nears the last row it holds.
        run(&window, "select ID from D", cx);
        window
            .update(cx, |pane, dead, cx| {
                assert!(!pane.is_running(), "a detached pane sent a statement");
                assert_eq!(pane.notice, Some(ts!("explorer.disconnected")));
                assert_eq!(
                    pane.results.len(),
                    1,
                    "the results of the last live run were replaced"
                );

                pane.notice = None;
                pane.reorder(
                    pane.results[0].id,
                    0,
                    Some(SortDirection::Descending),
                    dead,
                    cx,
                );
                assert_eq!(pane.notice, Some(ts!("explorer.disconnected")));

                pane.notice = None;
                pane.fetch_more(pane.results[0].id, dead, cx);
                assert_eq!(pane.notice, Some(ts!("explorer.disconnected")));
            })
            .expect("the window is open");
        cx.run_until_parked();

        // The rows are still the ones the live run produced.
        assert_eq!(column_zero(&window, 0, cx), ["1", "2", "3"]);
        connected.close().expect("close");
    }

    /// The rows of one menu, taken out of the pane so that a row which acts on
    /// the pane can be run without re-entering the update it was built in.
    fn menu_rows(
        window: &WindowHandle<QueryPane>,
        menu: PaneMenu,
        cx: &mut TestAppContext,
    ) -> Vec<MenuRow> {
        window
            .update(cx, |pane, _window, cx| match menu {
                PaneMenu::Editor { .. } => pane.editor_rows(cx),
                PaneMenu::Grid { id, target, .. } => pane.grid_rows(id, target, cx),
            })
            .expect("the window is open")
    }

    /// A window position no menu is really raised at: the rows a menu carries
    /// do not depend on where the pointer was.
    fn anywhere() -> Point<Pixels> {
        gpui::point(px(0.), px(0.))
    }

    /// The id of the pane's first result tab.
    fn first_result(window: &WindowHandle<QueryPane>, cx: &mut TestAppContext) -> u64 {
        window
            .update(cx, |pane, _window, _cx| pane.results[0].id)
            .expect("the window is open")
    }

    /// What one column of the clipboard test wants back.
    fn clipboard(cx: &mut gpui::VisualTestContext) -> String {
        cx.update(|_window, cx| {
            cx.read_from_clipboard()
                .and_then(|item| item.text())
                .unwrap_or_default()
        })
    }

    /// Hiding a column and putting every one of them back, through the header
    /// menu — which is the only route to the second half of that: a hidden
    /// column has no heading left to right-click.
    #[gpui::test]
    fn a_header_menu_hides_a_column_and_puts_them_all_back(cx: &mut TestAppContext) {
        let (connected, profile) = h2(
            "grid-menu",
            &[
                "create table G (A int, B int, C int)",
                "insert into G values (1, 2, 3)",
            ],
        );
        let window = pane(&connected, &profile, 500, cx);
        run(&window, "select A, B, C from G", cx);
        let id = first_result(&window, cx);
        let mut vcx = gpui::VisualTestContext::from_window(window.into(), cx);

        let header = |column| PaneMenu::Grid {
            id,
            target: MenuTarget::Header { column },
            position: anywhere(),
        };
        let rows = menu_rows(&window, header(1), &mut vcx);
        assert_eq!(
            context_menu::labels(&rows),
            [
                ts!("context.sort_asc").to_string(),
                ts!("context.sort_desc").to_string(),
                ts!("context.sort_clear").to_string(),
                String::new(),
                ts!("context.autofit").to_string(),
                ts!("context.hide_column").to_string(),
                ts!("context.show_columns").to_string(),
                String::new(),
                ts!("context.copy_column_name").to_string(),
            ]
        );
        assert!(
            !context_menu::row(&rows, &ts!("context.show_columns")).is_enabled(),
            "the way back was offered with nothing to come back from"
        );
        assert!(
            !context_menu::row(&rows, &ts!("context.sort_clear")).is_enabled(),
            "an unsorted result offered to unsort itself"
        );

        vcx.update(|window, cx| {
            context_menu::row(&rows, &ts!("context.hide_column")).activate(window, cx);
        });
        window
            .update(&mut vcx, |pane, _window, cx| {
                let grid = pane.grid_at(0).read(cx);
                assert!(grid.is_column_hidden(1));
                assert_eq!(grid.visible_column_indices(), vec![0, 2]);
            })
            .expect("the window is open");

        // The way back lives on the menu of a heading that is still there, and
        // it is live now that something is hidden.
        let rows = menu_rows(&window, header(0), &mut vcx);
        let show = context_menu::row(&rows, &ts!("context.show_columns"));
        assert!(show.is_enabled());
        vcx.update(|window, cx| show.activate(window, cx));

        window
            .update(&mut vcx, |pane, _window, cx| {
                let grid = pane.grid_at(0).read(cx);
                assert_eq!(grid.hidden_column_count(), 0);
                assert_eq!(grid.visible_column_indices(), vec![0, 1, 2]);
            })
            .expect("the window is open");
        connected.close().expect("close");
    }

    /// The editor menu's copy row writes the selection to the real clipboard,
    /// through the editor's own action rather than through anything invented
    /// here — and it is greyed out when there is nothing selected to copy.
    #[gpui::test]
    fn the_editor_menu_copies_the_selection(cx: &mut TestAppContext) {
        let (connected, profile) = h2("editor-menu", &[]);
        let window = pane(&connected, &profile, 500, cx);
        let mut vcx = gpui::VisualTestContext::from_window(window.into(), cx);

        window
            .update(&mut vcx, |pane, window, cx| {
                pane.editor.update(cx, |editor, cx| {
                    editor.set_text("select 1 from dual", cx);
                });
                pane.focus_editor(window, cx);
            })
            .expect("the window is open");
        vcx.run_until_parked();

        // With only a caret there is nothing to cut or copy, and the two rows
        // say so rather than quietly copying nothing.
        let editor_menu = || PaneMenu::Editor {
            position: anywhere(),
        };
        let rows = menu_rows(&window, editor_menu(), &mut vcx);
        assert!(!context_menu::row(&rows, &ts!("context.copy")).is_enabled());
        assert!(!context_menu::row(&rows, &ts!("context.cut")).is_enabled());
        assert!(
            context_menu::row(&rows, &ts!("context.run_all")).is_enabled(),
            "the pane still has its session"
        );

        window
            .update(&mut vcx, |pane, _window, cx| {
                pane.editor
                    .update(cx, |editor, cx| editor.select_range(0..8, cx));
            })
            .expect("the window is open");
        vcx.run_until_parked();

        let rows = menu_rows(&window, editor_menu(), &mut vcx);
        let copy = context_menu::row(&rows, &ts!("context.copy"));
        assert!(copy.is_enabled());
        vcx.update(|window, cx| copy.activate(window, cx));

        assert_eq!(
            clipboard(&mut vcx),
            "select 1",
            "the menu row did not reach the editor"
        );
        connected.close().expect("close");
    }

    /// A detached pane keeps its editor and refuses to run from the menu too:
    /// the three run rows are greyed, and the rows that only touch text are
    /// not.
    #[gpui::test]
    fn a_detached_pane_greys_the_menus_run_rows(cx: &mut TestAppContext) {
        let (connected, profile) = h2("editor-menu-detached", &[]);
        let window = pane(&connected, &profile, 500, cx);

        window
            .update(cx, |pane, _window, cx| {
                pane.editor
                    .update(cx, |editor, cx| editor.set_text("select 1", cx));
                pane.detach(cx);
            })
            .expect("the window is open");

        let rows = menu_rows(
            &window,
            PaneMenu::Editor {
                position: anywhere(),
            },
            cx,
        );
        for label in [
            ts!("context.run_statement"),
            ts!("context.run_selection"),
            ts!("context.run_all"),
        ] {
            assert!(
                !context_menu::row(&rows, &label).is_enabled(),
                "{label} was offered on a pane with no session"
            );
        }
        assert!(context_menu::row(&rows, &ts!("context.select_all")).is_enabled());
        assert!(context_menu::row(&rows, &ts!("context.paste")).is_enabled());
        connected.close().expect("close");
    }

    /// A sort is a re-run, so the grid whose marker was moved is thrown away —
    /// and the one that replaces it has to come back wearing the order it is
    /// actually in, or both the header and the menu would call it unsorted.
    #[gpui::test]
    fn a_sort_from_the_header_menu_marks_the_grid_it_comes_back_in(cx: &mut TestAppContext) {
        let (connected, profile) = h2(
            "grid-menu-sort",
            &[
                "create table S (N int)",
                "insert into S values (2), (3), (1)",
            ],
        );
        let window = pane(&connected, &profile, 500, cx);
        run(&window, "select N from S", cx);
        let mut vcx = gpui::VisualTestContext::from_window(window.into(), cx);

        let id = first_result(&window, &mut vcx);
        let rows = menu_rows(
            &window,
            PaneMenu::Grid {
                id,
                target: MenuTarget::Header { column: 0 },
                position: anywhere(),
            },
            &mut vcx,
        );
        assert!(
            rows.iter().all(|row| !row.is_checked()),
            "an unsorted result had a direction ticked"
        );
        vcx.update(|window, cx| {
            context_menu::row(&rows, &ts!("context.sort_desc")).activate(window, cx);
        });
        vcx.run_until_parked();
        assert_eq!(column_zero(&window, 0, &mut vcx), ["3", "2", "1"]);

        // The menu of that column now says which direction is in effect, and
        // offers the way out of it.
        let id = first_result(&window, &mut vcx);
        let rows = menu_rows(
            &window,
            PaneMenu::Grid {
                id,
                target: MenuTarget::Header { column: 0 },
                position: anywhere(),
            },
            &mut vcx,
        );
        assert!(context_menu::row(&rows, &ts!("context.sort_desc")).is_checked());
        assert!(!context_menu::row(&rows, &ts!("context.sort_asc")).is_checked());
        assert!(context_menu::row(&rows, &ts!("context.sort_clear")).is_enabled());

        vcx.update(|window, cx| {
            context_menu::row(&rows, &ts!("context.sort_clear")).activate(window, cx);
        });
        vcx.run_until_parked();
        assert_eq!(column_zero(&window, 0, &mut vcx), ["2", "3", "1"]);
        connected.close().expect("close");
    }

    /// A cell menu is about the selection rather than about the cell that was
    /// pressed, and says so by greying every row that would act on nothing.
    #[gpui::test]
    fn a_cell_menu_follows_the_selection(cx: &mut TestAppContext) {
        let (connected, profile) = h2(
            "grid-menu-cells",
            &["create table C (N int)", "insert into C values (7)"],
        );
        let window = pane(&connected, &profile, 500, cx);
        run(&window, "select N from C", cx);
        let id = first_result(&window, cx);
        let mut vcx = gpui::VisualTestContext::from_window(window.into(), cx);

        let cells = || PaneMenu::Grid {
            id,
            target: MenuTarget::Cell,
            position: anywhere(),
        };
        let rows = menu_rows(&window, cells(), &mut vcx);
        assert_eq!(
            context_menu::labels(&rows),
            [
                ts!("context.copy_as", format = "TSV").to_string(),
                ts!("context.copy_as", format = "CSV").to_string(),
                ts!("context.copy_as", format = "JSON").to_string(),
                ts!("context.copy_as", format = "INSERT").to_string(),
                String::new(),
                ts!("context.select_all").to_string(),
                ts!("context.clear_selection").to_string(),
                String::new(),
                ts!("data.set_null").to_string(),
                ts!("data.delete_row").to_string(),
                String::new(),
                ts!("data.discard_row").to_string(),
                ts!("data.discard").to_string(),
            ],
            "the editing rows are drawn greyed rather than left out (§7.8)"
        );
        assert!(
            !context_menu::labels(&rows).contains(&ts!("data.insert_row").to_string()),
            "a query result offered an insert (§7.9)"
        );
        for label in [
            ts!("context.copy_as", format = "TSV"),
            ts!("context.clear_selection"),
        ] {
            assert!(
                !context_menu::row(&rows, &label).is_enabled(),
                "{label} was offered over an empty selection"
            );
        }
        // `C` has no primary key, so the result failed §7.9's gate: every
        // editing row is there and every one of them is greyed.
        for label in [
            ts!("data.set_null"),
            ts!("data.delete_row"),
            ts!("data.discard_row"),
            ts!("data.discard"),
        ] {
            assert!(
                !context_menu::row(&rows, &label).is_enabled(),
                "{label} was offered over a result nothing can be written back through"
            );
        }

        vcx.update(|window, cx| {
            context_menu::row(&rows, &ts!("context.select_all")).activate(window, cx);
        });
        let rows = menu_rows(&window, cells(), &mut vcx);
        vcx.update(|window, cx| {
            context_menu::row(&rows, &ts!("context.copy_as", format = "CSV")).activate(window, cx);
        });
        assert_eq!(clipboard(&mut vcx).trim(), "7");
        connected.close().expect("close");
    }

    /// A schema-less H2 fixture with one keyed table and one to join it to.
    fn people(name: &str) -> (Connected, ConnectionProfile) {
        h2(
            name,
            &[
                "create table PERSON (ID int primary key, NAME varchar(20), NOTE varchar(20))",
                "create table PET (ID int primary key, OWNER int, NAME varchar(20))",
                "insert into PERSON values (1, 'a', null), (2, 'b', '')",
                "insert into PET values (1, 1, 'cat')",
            ],
        )
    }

    /// Column `column` of the grid of result `index`, as text.
    fn column_at(
        window: &WindowHandle<QueryPane>,
        index: usize,
        column: usize,
        cx: &mut TestAppContext,
    ) -> Vec<String> {
        window
            .update(cx, |pane, _window, cx| {
                let source = pane.grid_at(index).read(cx).source();
                (0..source.row_count())
                    .map(|row| match source.cell(row, column) {
                        GridCell::Text(text) => text.to_string(),
                        GridCell::Null => "NULL".to_string(),
                        // Only an inserted row can leave a column to the
                        // server, and §7.9 gives a query result no way to add
                        // one.
                        GridCell::Default => "DEFAULT".to_string(),
                        GridCell::Lob { size } => format!("lob {size:?}"),
                    })
                    .collect()
            })
            .expect("the window is open")
    }

    /// Whether the grid of result `index` may be written back through.
    fn writable(window: &WindowHandle<QueryPane>, index: usize, cx: &mut TestAppContext) -> bool {
        window
            .update(cx, |pane, _window, cx| {
                pane.grid_at(index).read(cx).source().writable()
            })
            .expect("the window is open")
    }

    /// The line result `index` gives for why it may only be read.
    fn read_only_reason(
        window: &WindowHandle<QueryPane>,
        index: usize,
        cx: &mut TestAppContext,
    ) -> Option<SharedString> {
        window
            .update(cx, |pane, _window, _cx| match &pane.results[index].body {
                ResultBody::Rows(rows) => rows.read_only.clone(),
                ResultBody::Message(text) => panic!("result {index} is the message {text:?}"),
            })
            .expect("the window is open")
    }

    /// Opens the field over a cell of result `index`, puts `text` in it and
    /// closes it — the whole gesture, and the only route by which anything is
    /// staged. Answers whether the field opened at all.
    fn type_into(
        handle: &WindowHandle<QueryPane>,
        index: usize,
        row: usize,
        column: usize,
        text: &str,
        cx: &mut TestAppContext,
    ) -> bool {
        let opened = handle
            .update(cx, |pane, window, cx| {
                let grid = pane.grid_at(index).clone();
                grid.update(cx, |grid, cx| {
                    if !grid.begin_edit(row, column, window, cx) {
                        return false;
                    }
                    let input = grid.editor().cloned().expect("the field is open");
                    input.update(cx, |input: &mut rudbman_ui::TextInput, cx| {
                        input.set_content(text.to_owned(), cx);
                    });
                    grid.commit_edit(cx);
                    true
                })
            })
            .expect("the window is open");
        // `EditCommitted` is emitted, not called: the pane hears it on the next
        // turn of the loop.
        cx.run_until_parked();
        opened
    }

    /// The statements the pane's confirmation is showing.
    fn planned(window: &WindowHandle<QueryPane>, cx: &mut TestAppContext) -> Vec<String> {
        window
            .update(cx, |pane, _window, _cx| {
                pane.preview
                    .as_ref()
                    .map(|preview| {
                        preview
                            .statements
                            .iter()
                            .map(|statement| statement.sql.clone())
                            .collect()
                    })
                    .unwrap_or_default()
            })
            .expect("the window is open")
    }

    /// The table as the server holds it, read straight off the session rather
    /// than through the pane.
    fn server_rows(connected: &Connected, sql: &str) -> Vec<String> {
        let cursor = connected
            .session()
            .execute(&StatementSpec::new(sql))
            .unwrap_or_else(|error| panic!("{sql}: {error}"));
        let batch = cursor.fetch(500).expect("the batch decodes");
        let described = crate::query_source::tests::info(1, "C", 12, 0);
        (0..batch.rows())
            .map(|row| {
                (0..batch.column_count())
                    .map(|column| {
                        batch
                            .value(row, column)
                            .and_then(|value| value.to_text(&described))
                            .unwrap_or_else(|| "NULL".to_string())
                    })
                    .collect::<Vec<_>>()
                    .join("|")
            })
            .collect()
    }

    /// A `SELECT` of one keyed table becomes editable, and an `UPDATE` typed
    /// into it is planned, shown, sent and read back.
    ///
    /// The whole of §7.9's query-result editing end to end: the gate opens the
    /// grid, the preview is the confirmation, the batch is one transaction, and
    /// the re-run is what puts the server's own answer back on screen.
    #[gpui::test]
    fn a_select_of_one_keyed_table_is_edited_and_applied(cx: &mut TestAppContext) {
        let (connected, profile) = people("query-edit-apply");
        let window = pane(&connected, &profile, 500, cx);
        run(&window, "select ID, NAME from PERSON order by ID", cx);

        assert!(
            writable(&window, 0, cx),
            "a keyed table's own columns did not open the grid"
        );
        assert_eq!(read_only_reason(&window, 0, cx), None);
        window
            .update(cx, |pane, _window, cx| {
                assert!(
                    pane.grid_at(0).read(cx).source().column(0).primary_key,
                    "the key column is unmarked"
                );
            })
            .expect("the window is open");

        assert!(type_into(&window, 0, 0, 1, "A", cx), "the field refused");
        assert_eq!(column_at(&window, 0, 1, cx), ["A", "b"]);
        window
            .update(cx, |pane, _window, cx| {
                assert!(pane.has_pending_edits(cx));
                assert_eq!(pane.counts(cx).expect("staged").changed, 1);
            })
            .expect("the window is open");

        // Planned and shown; nothing has gone out yet.
        window
            .update(cx, |pane, window, cx| pane.apply(window, cx))
            .expect("the window is open");
        assert_eq!(
            planned(&window, cx),
            ["UPDATE PUBLIC.PERSON SET NAME = ? WHERE ID = ?"],
            "the statement is qualified from the columns' own metadata"
        );
        assert_eq!(
            server_rows(&connected, "select NAME from PERSON order by ID"),
            ["a", "b"],
            "the preview sent something"
        );

        window
            .update(cx, |pane, window, cx| pane.confirm_apply(window, cx))
            .expect("the window is open");
        cx.run_until_parked();

        window
            .update(cx, |pane, _window, cx| {
                assert!(!pane.has_pending_edits(cx), "the buffer outlived the apply");
                assert!(pane.apply_error.is_none());
                assert!(pane.preview.is_none());
                assert_eq!(pane.notice, Some(ts!("data.applied", count = 1)));
                assert_eq!(pane.results.len(), 1, "the statement was re-run");
            })
            .expect("the window is open");
        // Read back off the server by the re-run, not left over from the
        // overlay: the staging buffer was thrown away before it happened.
        assert_eq!(column_at(&window, 0, 1, cx), ["A", "b"]);
        assert_eq!(
            server_rows(&connected, "select NAME from PERSON order by ID"),
            ["A", "b"]
        );
        assert!(
            writable(&window, 0, cx),
            "the result the re-run produced is not editable"
        );
        connected.close().expect("close");
    }

    /// A statement the server refuses rolls the batch back and leaves every
    /// staged change exactly where it was.
    ///
    /// §7.9's rule, and the opposite of the structure pane's: the rollback
    /// means nothing was written, so there is nothing to reconcile and no
    /// reason to make the user type it again.
    #[gpui::test]
    fn a_refused_apply_rolls_back_and_keeps_the_staging(cx: &mut TestAppContext) {
        let (connected, profile) = people("query-edit-refused");
        let window = pane(&connected, &profile, 500, cx);
        run(&window, "select ID, NAME from PERSON order by ID", cx);

        // Row 0's key becomes row 1's, which the primary key forbids.
        assert!(type_into(&window, 0, 0, 0, "2", cx));
        window
            .update(cx, |pane, window, cx| {
                pane.apply(window, cx);
                pane.confirm_apply(window, cx);
            })
            .expect("the window is open");
        cx.run_until_parked();

        window
            .update(cx, |pane, _window, cx| {
                let problem = pane.apply_error.as_ref().expect("the apply failed");
                assert!(problem.error.is_some(), "the driver said nothing");
                assert!(!problem.half_applied, "the rollback went through");
                assert!(
                    pane.has_pending_edits(cx),
                    "a failed apply threw the staging away"
                );
                assert_eq!(pane.results.len(), 1, "a failed apply re-ran the statement");
            })
            .expect("the window is open");
        assert_eq!(
            column_at(&window, 0, 0, cx),
            ["2", "2"],
            "the overlay still holds what was typed"
        );
        assert_eq!(
            server_rows(&connected, "select ID from PERSON order by ID"),
            ["1", "2"],
            "the table took the refused statement"
        );
        connected.close().expect("close");
    }

    /// An alias is a heading and not a column name, so a key selected under one
    /// is still the key.
    #[gpui::test]
    fn an_aliased_key_column_still_opens_the_grid(cx: &mut TestAppContext) {
        let (connected, profile) = people("query-edit-alias");
        let window = pane(&connected, &profile, 500, cx);
        run(&window, "select ID as PK, NAME from PERSON order by ID", cx);

        assert!(
            writable(&window, 0, cx),
            "the alias hid the key: {:?}",
            read_only_reason(&window, 0, cx)
        );
        assert!(type_into(&window, 0, 0, 1, "A", cx));
        window
            .update(cx, |pane, window, cx| pane.apply(window, cx))
            .expect("the window is open");
        // The `WHERE` clause names the catalogue's column and not the heading:
        // `PK` is what the grid draws, `ID` is what the table has.
        assert_eq!(
            planned(&window, cx),
            ["UPDATE PUBLIC.PERSON SET NAME = ? WHERE ID = ?"],
            "the statement is qualified from the columns' own metadata"
        );
        connected.close().expect("close");
    }

    /// The three ways a result misses the gate, each read-only and each saying
    /// so in the one line §7.9 asks for.
    #[gpui::test]
    fn a_result_that_is_not_one_keyed_table_stays_read_only(cx: &mut TestAppContext) {
        let (connected, profile) = people("query-edit-gate");
        let window = pane(&connected, &profile, 500, cx);

        for (why, sql) in [
            (
                "a join names two tables",
                "select PERSON.ID, PET.NAME from PERSON join PET on PET.OWNER = PERSON.ID",
            ),
            (
                "the key was not selected",
                "select NAME from PERSON order by NAME",
            ),
            ("every column is computed", "select count(*) from PERSON"),
            (
                "a grouped aggregate names the table and carries none of its key",
                "select NAME, count(*) from PERSON group by NAME",
            ),
        ] {
            run(&window, sql, cx);
            assert!(!writable(&window, 0, cx), "{why}: {sql}");
            assert_eq!(
                read_only_reason(&window, 0, cx),
                Some(ts!("query.not_editable")),
                "{why}: {sql}"
            );
            assert!(
                !type_into(&window, 0, 0, 0, "x", cx),
                "{why}: a field opened over {sql}"
            );
        }
        connected.close().expect("close");
    }

    /// A read-only profile refuses an apply as flatly as it refuses a typed
    /// write, over a result that would otherwise be editable.
    #[gpui::test]
    fn a_read_only_profile_keeps_even_a_keyed_result_shut(cx: &mut TestAppContext) {
        let (connected, mut profile) = people("query-edit-read-only");
        profile.read_only = true;
        let window = pane(&connected, &profile, 500, cx);
        run(&window, "select ID, NAME from PERSON order by ID", cx);

        assert!(!writable(&window, 0, cx));
        assert_eq!(
            read_only_reason(&window, 0, cx),
            Some(ts!("data.read_only")),
            "the two reasons must not be confused for each other"
        );
        assert!(!type_into(&window, 0, 0, 1, "A", cx));

        // And the half of the apply that a gesture cannot reach refuses too.
        window
            .update(cx, |pane, window, cx| {
                let grid = pane.grid_at(0).clone();
                grid.update(cx, |grid, cx| {
                    grid.source_mut(cx).edits_mut().toggle_deleted(0);
                });
                pane.apply(window, cx);
                assert!(pane.preview.is_none(), "a read-only pane planned a batch");
            })
            .expect("the window is open");
        cx.run_until_parked();
        assert_eq!(
            server_rows(&connected, "select NAME from PERSON order by ID"),
            ["a", "b"]
        );
        connected.close().expect("close");
    }

    /// A sort and a run both replace the source a staged edit is keyed to, so
    /// both ask first — and the tab strip does too.
    #[gpui::test]
    fn a_sort_or_a_run_waits_for_the_staged_edits_to_be_dealt_with(cx: &mut TestAppContext) {
        let (connected, profile) = people("query-edit-guard");
        let window = pane(&connected, &profile, 500, cx);
        run(&window, "select ID, NAME from PERSON order by ID", cx);
        assert!(type_into(&window, 0, 0, 1, "A", cx));

        let id = first_result(&window, cx);
        window
            .update(cx, |pane, window, cx| {
                pane.reorder(id, 0, Some(SortDirection::Descending), window, cx);
                assert_eq!(pane.notice, Some(ts!("data.discard_first")));

                pane.notice = None;
                pane.request(vec!["select 7".to_string()], window, cx);
                assert_eq!(pane.notice, Some(ts!("data.discard_first")));

                // What the tab strip asks before it closes the tab, and what
                // the pane says about a yes.
                assert!(pane.has_pending_edits(cx));
                pane.notice = None;
                pane.warn_pending(cx);
                assert_eq!(pane.notice, Some(ts!("data.discard_first")));
            })
            .expect("the window is open");
        cx.run_until_parked();
        assert_eq!(
            column_at(&window, 0, 1, cx),
            ["A", "b"],
            "the edit did not survive the refusals"
        );

        // Discarding lets both through.
        window
            .update(cx, |pane, _window, cx| pane.discard_all(id, cx))
            .expect("the window is open");
        assert_eq!(column_at(&window, 0, 1, cx), ["a", "b"]);
        window
            .update(cx, |pane, window, cx| {
                assert!(!pane.has_pending_edits(cx));
                pane.reorder(id, 0, Some(SortDirection::Descending), window, cx);
            })
            .expect("the window is open");
        cx.run_until_parked();
        assert_eq!(column_at(&window, 0, 0, cx), ["2", "1"], "the sort ran");
        connected.close().expect("close");
    }

    /// A result every column of which refuses a field is still editable, and
    /// the silent banner is telling the truth about it.
    ///
    /// The one case where "no banner" and "no cell takes a keystroke" come
    /// apart: `SELECT ID FROM TICKET` over an auto-increment key passes the
    /// gate — one table, key present — while its only column is one the driver
    /// says not to type into. What makes the silence honest is that deleting a
    /// row needs the key and nothing else, so the surface can still write back;
    /// the banner claims "these rows can be changed", not "every cell can".
    /// The data pane has the same property over a table of only generated
    /// columns, and says nothing there either.
    #[gpui::test]
    fn a_result_of_only_generated_columns_can_still_delete_its_rows(cx: &mut TestAppContext) {
        let (connected, profile) = h2(
            "query-edit-generated",
            &[
                "create table TICKET (ID int auto_increment primary key)",
                "insert into TICKET values (default), (default)",
            ],
        );
        let window = pane(&connected, &profile, 500, cx);
        run(&window, "select ID from TICKET order by ID", cx);

        assert!(writable(&window, 0, cx), "the gate refused a keyed table");
        assert_eq!(read_only_reason(&window, 0, cx), None);
        assert!(
            !type_into(&window, 0, 0, 0, "9", cx),
            "an auto-increment column took a keystroke"
        );

        // And the row goes, which is the writing back the silence promised.
        let id = first_result(&window, cx);
        let mut vcx = gpui::VisualTestContext::from_window(window.into(), cx);
        window
            .update(&mut vcx, |pane, _window, cx| {
                let grid = pane.grid_at(0).clone();
                grid.update(cx, |grid, cx| grid.select_cell(0, 0, cx));
            })
            .expect("the window is open");
        let rows = menu_rows(
            &window,
            PaneMenu::Grid {
                id,
                target: MenuTarget::Cell,
                position: anywhere(),
            },
            &mut vcx,
        );
        assert!(
            context_menu::row(&rows, &ts!("data.delete_row")).is_enabled(),
            "a result nothing can be typed into could not delete either"
        );
        vcx.update(|window, cx| {
            context_menu::row(&rows, &ts!("data.delete_row")).activate(window, cx);
        });

        window
            .update(&mut vcx, |pane, window, cx| {
                assert!(pane.has_pending_edits(cx));
                pane.apply(window, cx);
            })
            .expect("the window is open");
        assert_eq!(
            planned(&window, &mut vcx),
            ["DELETE FROM PUBLIC.TICKET WHERE ID = ?"]
        );
        window
            .update(&mut vcx, |pane, window, cx| pane.confirm_apply(window, cx))
            .expect("the window is open");
        vcx.run_until_parked();
        assert_eq!(
            server_rows(&connected, "select ID from TICKET order by ID"),
            ["2"]
        );
        connected.close().expect("close");
    }

    /// Nothing anywhere offers to add a row to a query result (§7.9).
    #[gpui::test]
    fn an_editable_result_offers_no_insert(cx: &mut TestAppContext) {
        let (connected, profile) = people("query-edit-no-insert");
        let window = pane(&connected, &profile, 500, cx);
        run(&window, "select ID, NAME, NOTE from PERSON order by ID", cx);
        assert!(writable(&window, 0, cx));

        let id = first_result(&window, cx);
        let mut vcx = gpui::VisualTestContext::from_window(window.into(), cx);
        window
            .update(&mut vcx, |pane, _window, cx| {
                let grid = pane.grid_at(0).clone();
                grid.update(cx, |grid, cx| grid.select_cell(0, 2, cx));
            })
            .expect("the window is open");

        let rows = menu_rows(
            &window,
            PaneMenu::Grid {
                id,
                target: MenuTarget::Cell,
                position: anywhere(),
            },
            &mut vcx,
        );
        assert!(
            !context_menu::labels(&rows).contains(&ts!("data.insert_row").to_string()),
            "an editable query result offered an insert"
        );
        // The editing rows that *are* offered are live over a writable cell.
        for label in [ts!("data.set_null"), ts!("data.delete_row")] {
            assert!(
                context_menu::row(&rows, &label).is_enabled(),
                "{label} was greyed over an editable result"
            );
        }

        // And the grid itself has no row past the ones the server sent.
        window
            .update(&mut vcx, |pane, _window, cx| {
                let source = pane.grid_at(0).read(cx).source();
                assert_eq!(source.row_count(), source.base_rows());
            })
            .expect("the window is open");
        connected.close().expect("close");
    }

    #[test]
    fn every_label_the_pane_menus_draw_has_a_translation() {
        for label in [
            ts!("context.copy_as", format = "TSV"),
            ts!("context.select_all"),
            ts!("context.clear_selection"),
            ts!("context.sort_asc"),
            ts!("context.sort_desc"),
            ts!("context.sort_clear"),
            ts!("context.autofit"),
            ts!("context.hide_column"),
            ts!("context.show_columns"),
            ts!("context.copy_column_name"),
            ts!("context.cut"),
            ts!("context.copy"),
            ts!("context.paste"),
            ts!("context.undo"),
            ts!("context.redo"),
            ts!("context.toggle_comment"),
            ts!("context.run_statement"),
            ts!("context.run_selection"),
            ts!("context.run_all"),
            ts!("context.find"),
            ts!("context.replace"),
            // The editing rows §7.9 gives a query result, which reuse the data
            // pane's wording: they are the same operations on the same kind of
            // thing, and a second set of strings would be a second thing to
            // keep in step.
            ts!("data.set_null"),
            ts!("data.delete_row"),
            ts!("data.undelete_row"),
            ts!("data.discard_row"),
            ts!("data.apply"),
            ts!("data.discard"),
            ts!("data.discard_first"),
            ts!("data.applying"),
            ts!("data.applied", count = 1),
            ts!("data.pending", changed = 1, inserted = 0, deleted = 2),
            ts!("data.read_only"),
            // The one line that is this pane's own.
            ts!("query.not_editable"),
        ] {
            assert!(!label.is_empty(), "empty label");
            assert!(!label.starts_with("context."), "untranslated {label:?}");
            assert!(!label.starts_with("data."), "untranslated {label:?}");
            assert!(!label.starts_with("query."), "untranslated {label:?}");
        }
    }

    /// Every locale answers the pane's own new key with something of its own.
    ///
    /// The per-key fallback in `rust-i18n` makes a missing key look like a
    /// working lookup, so the only thing that catches one is a translation that
    /// differs from the English.
    #[test]
    fn the_reason_a_result_is_not_editable_is_translated_everywhere() {
        for (tag, _) in crate::i18n::supported()
            .iter()
            .filter(|(tag, _)| *tag != "en")
        {
            let translated = rust_i18n::t!("query.not_editable", locale = *tag);
            assert!(!translated.is_empty(), "{tag} says nothing");
            assert_ne!(
                translated,
                rust_i18n::t!("query.not_editable", locale = "en"),
                "query.not_editable is untranslated in {tag}"
            );
        }
    }
}
