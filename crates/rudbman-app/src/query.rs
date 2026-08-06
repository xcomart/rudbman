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
//! # `may_have_more` is a contract, not a field
//!
//! JDBC has no lookahead: asking whether another result exists consumes the
//! current one (architecture document, §4.4). [`advance`] therefore keeps
//! calling `MORE_RESULTS` until the three-part exhaustion holds, and stops the
//! moment a result set still has rows in it — because advancing past it would
//! close the very `ResultSet` the grid is paging. Paging that result to its end
//! resumes the walk, so the later results of a multi-result statement appear
//! when the earlier one is finished with, and never before.
//!
//! A script is split into statements first, so each statement gets a cursor of
//! its own and two `SELECT`s are two independently pageable grids. The
//! `MORE_RESULTS` walk is what a stored procedure needs, not what a script
//! needs.

use std::sync::Arc;
use std::time::{Duration, Instant};

use gpui::{
    Action, AnyElement, App, ClipboardItem, Context, DragMoveEvent, Entity, EntityId, EventEmitter,
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
    CopyFormat, GridCell, GridEvent, GridSource, GridSourceState, GridView, MenuTarget,
    SortDirection,
};
use rudbman_jdbc::{
    BridgeErrorKind, Canceller, ColumnInfo, Cursor, Error as JdbcError, StatementSpec,
};
use rudbman_sql::{Dialect, TokenKind, lex, split_statements};
use rudbman_ui::{Button, ButtonVariant, ContextMenu, Theme, theme};

use crate::SHORTCUT_MODIFIER;
use crate::connection::{ConnectError, SessionHandle};
use crate::context_menu::{self, MenuRow};
use crate::explorer::ConnectionId;
use crate::i18n::ts;
use crate::query_source::{RenderedBatch, ResultSource, render_batch};

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

/// One thing a statement produced.
#[derive(Debug)]
enum Step {
    /// A result set, and its first batch.
    Rows {
        /// The result's logical column types.
        columns: Vec<ColumnInfo>,
        /// The first batch, already rendered.
        batch: RenderedBatch,
        /// Whether the driver had run out of rows.
        complete: bool,
    },
    /// An update count.
    Message {
        /// Rows the statement changed, or zero for a statement that changes
        /// none — a `CREATE TABLE`, say.
        update_count: i64,
    },
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

/// One page of an already-open result.
struct Paged {
    batch: RenderedBatch,
    /// Whether that batch was the last of this result set.
    complete: bool,
    /// Further results of the same statement, picked up once this one ended.
    steps: Vec<Step>,
    /// Whether the cursor is parked on a result set that can still be paged.
    pageable: bool,
}

/// Walks a cursor's results, stopping at the first one that can still be paged.
///
/// `fresh` says whether the cursor is already sitting on a result nobody has
/// read — true straight after `EXECUTE`, false when resuming after a result set
/// ran out. Blocks; only ever called from a background thread.
fn advance(
    cursor: &mut Cursor,
    mut fresh: bool,
    fetch_rows: u32,
    steps: &mut Vec<Step>,
) -> Result<bool, JdbcError> {
    loop {
        if !fresh && cursor.more_results()?.is_exhausted() {
            return Ok(false);
        }
        fresh = false;

        let (has_result_set, update_count, exhausted, columns) = {
            let result = cursor.result();
            (
                result.has_result_set,
                result.update_count,
                result.is_exhausted(),
                result.columns.clone(),
            )
        };

        if has_result_set {
            let raw = cursor.fetch(fetch_rows)?;
            let complete = raw.is_last();
            let batch = render_batch(&raw, &columns);
            steps.push(Step::Rows {
                columns,
                batch,
                complete,
            });
            if !complete {
                // Advancing now would close the `ResultSet` the grid is about
                // to page. The walk resumes when the rows run out.
                return Ok(true);
            }
        } else if update_count >= 0 {
            steps.push(Step::Message { update_count });
        }

        if exhausted {
            return Ok(false);
        }
    }
}

/// Fetches one more batch of an open result, and walks on if it was the last.
fn page(cursor: &mut Cursor, columns: &[ColumnInfo], fetch_rows: u32) -> Result<Paged, JdbcError> {
    let raw = cursor.fetch(fetch_rows)?;
    let complete = raw.is_last();
    let batch = render_batch(&raw, columns);
    let mut steps = Vec::new();
    let pageable = if complete {
        advance(cursor, false, fetch_rows, &mut steps)?
    } else {
        true
    };
    Ok(Paged {
        batch,
        complete,
        steps,
        pageable,
    })
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

/// A result set and everything needed to go on reading it.
struct RowsTab {
    grid: Entity<GridView<ResultSource>>,
    /// The statement the sort round trip wraps.
    ///
    /// The one the *user* wrote, not the one that produced these rows: a
    /// re-sort of an already-sorted result wraps the original, so clicking a
    /// header ten times sends ten statements of the same size rather than one
    /// nested ten deep.
    sql: String,
    /// The result's logical column types, for rendering later batches.
    columns: Arc<Vec<ColumnInfo>>,
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
    read_only: bool,
    /// The profile asks before a write.
    confirm_writes: bool,
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
    _editor_events: Subscription,
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
    pub fn new(
        session: SessionHandle,
        connection: ConnectionId,
        profile: &ConnectionProfile,
        driver_dialect: &str,
        settings: &AppSettings,
        sql: &str,
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
        let editor_events = cx.subscribe(&editor, |pane, editor, event, cx| match event {
            EditorEvent::RunStatement { span } => {
                let text = editor.read(cx).text();
                let sql = span.sql(&text).to_string();
                pane.request(vec![sql], cx);
            }
            EditorEvent::RunSelection { span } => {
                let text = editor.read(cx).text();
                let selected = text.get(span.clone()).unwrap_or_default().to_string();
                pane.request(vec![selected], cx);
            }
            EditorEvent::RunAll => {
                let text = editor.read(cx).text();
                // Split rather than sent whole: one cursor per statement is
                // what makes two `SELECT`s two independently pageable grids.
                let statements: Vec<String> = split_statements(&text, &pane.dialect)
                    .into_iter()
                    .map(|span| span.sql(&text).to_string())
                    .collect();
                pane.request(statements, cx);
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
        });

        Self {
            editor,
            session: Some(session),
            connection,
            dialect,
            read_only: profile.read_only,
            confirm_writes: profile.confirm_writes,
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
            _editor_events: editor_events,
        }
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
    fn request(&mut self, statements: Vec<String>, cx: &mut Context<Self>) {
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

        self.start(statements, None, cx);
    }

    /// The write confirmation was accepted.
    pub fn confirmed(&mut self, cx: &mut Context<Self>) {
        let Some(statements) = self.pending.take() else {
            return;
        };
        self.start(statements, None, cx);
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

        cx.spawn(async move |pane, cx| {
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
                    .update(cx, |pane, cx| pane.deliver(generation, outcome, cx))
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
                self.append(&sql, steps, pageable.then_some(cursor), cx);
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
                    self.push_rows(base.clone(), columns, batch, state, carried, cx);
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
    fn push_rows(
        &mut self,
        sql: String,
        columns: Vec<ColumnInfo>,
        batch: RenderedBatch,
        state: GridSourceState,
        cursor: Option<Cursor>,
        cx: &mut Context<Self>,
    ) {
        let id = self.mint_id();
        let mut source = ResultSource::new(&columns);
        source.push(batch);
        source.set_state(state);

        let grid = cx.new(|cx| GridView::new(source, cx));
        // The marker the sort that asked for this run was for, if it was one:
        // the grid it was set on has just been replaced by this one.
        if let Some(sort) = self.pending_sort.take() {
            grid.update(cx, |grid, cx| grid.set_sort(Some(sort), cx));
        }
        let events = cx.subscribe(&grid, move |pane, _grid, event, cx| match event {
            GridEvent::NearEnd => pane.fetch_more(id, cx),
            GridEvent::SortRequested { column, direction } => {
                pane.reorder(id, *column, *direction, cx);
            }
            GridEvent::CellActivated { row, column } => pane.open_cell(id, *row, *column, cx),
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
        });

        let label = ts!("query.result", index = self.results.len() + 1);
        self.results.push(ResultTab {
            id,
            label,
            body: ResultBody::Rows(Box::new(RowsTab {
                grid,
                sql,
                columns: Arc::new(columns),
                cursor,
                fetching: false,
                _events: events,
            })),
        });
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
    fn fetch_more(&mut self, id: u64, cx: &mut Context<Self>) {
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

        cx.spawn(async move |pane, cx| {
            let outcome = cx
                .background_spawn(async move {
                    match page(&mut cursor, &columns, fetch_rows) {
                        Ok(paged) => Ok((cursor, paged)),
                        Err(error) => Err(error),
                    }
                })
                .await;
            pane.update(cx, |pane, cx| pane.paged(id, generation, outcome, cx))
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
                let sql = self.rows_tab(id).map(|tab| tab.sql.clone());

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
                        source.push(batch);
                        source.set_state(state);
                    });
                }

                // Results the `MORE_RESULTS` walk picked up once this one ended.
                if !steps.is_empty()
                    && let Some(sql) = sql
                {
                    let carried = if pageable { cursor.take() } else { None };
                    self.append(&sql, steps, carried, cx);
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
        self.start(vec![sql], Some(base), cx);
    }

    /// A cell was opened.
    ///
    /// A LOB has no body in the grid — only its size travelled — and reading
    /// one needs `LOB_READ` (0x25), which the bridge answers "not implemented"
    /// (architecture document, §12, open question 7). Saying so is better than
    /// a viewer that shows nothing.
    // TODO(M4): open a chunked LOB viewer once `LOB_READ` lands in the bridge.
    fn open_cell(&mut self, id: u64, row: usize, column: usize, cx: &mut Context<Self>) {
        let Some(tab) = self.rows_tab(id) else {
            return;
        };
        let is_lob = matches!(
            tab.grid.read(cx).source().cell(row, column),
            GridCell::Lob { .. }
        );
        if is_lob {
            self.notice = Some(ts!("query.lob_unsupported"));
            cx.notify();
        }
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
                .on_click(move |_, _window, cx| {
                    this.update(cx, |pane, cx| {
                        let text = pane.editor.read(cx).text();
                        let statements = match pane.editor.read(cx).statement_at_caret() {
                            Some(span) => vec![span.sql(&text).to_string()],
                            None => Vec::new(),
                        };
                        pane.request(statements, cx);
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
            .children(cancel)
            .child(run)
            .into_any_element()
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
    fn grid_of(&self, id: u64) -> Option<&Entity<GridView<ResultSource>>> {
        self.results
            .iter()
            .find(|tab| tab.id == id)
            .and_then(|tab| match &tab.body {
                ResultBody::Rows(rows) => Some(&rows.grid),
                ResultBody::Message(_) => None,
            })
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
    /// The cell menu is about the *selection*, not about the cell that was
    /// pressed — the grid has already moved the selection onto it unless the
    /// press landed inside one — so the four copy formats and "clear" all read
    /// the same block the user can see.
    ///
    /// The heading menu is about one column, and its sort rows go through
    /// [`QueryPane::reorder`] rather than through the grid: the grid holds only
    /// the first n rows of an answer the server has all of, so ordering it is a
    /// re-run and not a shuffle. "Show every column" is the one row here that
    /// no other gesture offers — a hidden column has no heading left to
    /// right-click.
    fn grid_rows(&self, id: u64, target: MenuTarget, cx: &mut Context<Self>) -> Vec<MenuRow> {
        let Some(grid) = self.grid_of(id) else {
            return Vec::new();
        };
        let this = cx.entity();
        let grid = grid.clone();

        match target {
            MenuTarget::Cell => {
                let empty = grid.read(cx).selection().is_empty();
                let mut rows: Vec<MenuRow> = CopyFormat::ALL
                    .into_iter()
                    .map(|format| {
                        let grid = grid.clone();
                        let row = MenuRow::new(ts!("context.copy_as", format = format.label()))
                            .enabled(!empty)
                            .on_activate(move |_window, cx| {
                                grid.update(cx, |grid, cx| grid.copy(format, cx));
                            });
                        // Only the default format carries the hint: `Ctrl+C` is
                        // one chord and copies TSV, and repeating it on four
                        // rows would say it does all four.
                        if format == CopyFormat::default() {
                            row.shortcut(format!("{SHORTCUT_MODIFIER}+C"))
                        } else {
                            row
                        }
                    })
                    .collect();
                rows.push(MenuRow::separator());
                rows.push({
                    let grid = grid.clone();
                    MenuRow::new(ts!("context.select_all"))
                        .shortcut(format!("{SHORTCUT_MODIFIER}+A"))
                        .on_activate(move |_window, cx| {
                            grid.update(cx, |grid, cx| grid.select_all(cx));
                        })
                });
                rows.push(
                    MenuRow::new(ts!("context.clear_selection"))
                        .enabled(!empty)
                        .on_activate(move |_window, cx| {
                            grid.update(cx, |grid, cx| grid.clear_selection(cx));
                        }),
                );
                rows
            }
            MenuTarget::Header { column } => {
                let sort = grid.read(cx).sort();
                let nothing_hidden = grid.read(cx).hidden_column_count() == 0;
                let name = grid.read(cx).column_name(column).map(str::to_owned);
                let sorted = |direction: SortDirection| sort == Some((column, direction));
                let order = |direction: Option<SortDirection>| {
                    let this = this.clone();
                    move |_window: &mut Window, cx: &mut App| {
                        this.update(cx, |pane, cx| pane.reorder(id, column, direction, cx));
                    }
                };

                vec![
                    MenuRow::new(ts!("context.sort_asc"))
                        .checked(sorted(SortDirection::Ascending))
                        .on_activate(order(Some(SortDirection::Ascending))),
                    MenuRow::new(ts!("context.sort_desc"))
                        .checked(sorted(SortDirection::Descending))
                        .on_activate(order(Some(SortDirection::Descending))),
                    MenuRow::new(ts!("context.sort_clear"))
                        .enabled(sort.is_some())
                        .on_activate(order(None)),
                    MenuRow::separator(),
                    MenuRow::new(ts!("context.autofit")).on_activate({
                        let grid = grid.clone();
                        move |_window, cx| {
                            grid.update(cx, |grid, cx| grid.autofit_column(column, cx));
                        }
                    }),
                    MenuRow::new(ts!("context.hide_column")).on_activate({
                        let grid = grid.clone();
                        move |_window, cx| {
                            grid.update(cx, |grid, cx| grid.set_column_hidden(column, true, cx));
                        }
                    }),
                    MenuRow::new(ts!("context.show_columns"))
                        .enabled(!nothing_hidden)
                        .on_activate({
                            let grid = grid.clone();
                            move |_window, cx| {
                                grid.update(cx, |grid, cx| grid.show_all_columns(cx));
                            }
                        }),
                    MenuRow::separator(),
                    MenuRow::new(ts!("context.copy_column_name"))
                        .enabled(name.is_some())
                        .on_activate(move |_window, cx| {
                            if let Some(name) = name.clone() {
                                cx.write_to_clipboard(ClipboardItem::new_string(name));
                            }
                        }),
                ]
            }
        }
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
fn note(text: SharedString, color: Hsla) -> AnyElement {
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
fn render_error(error: &QueryError, chrome: &Theme) -> AnyElement {
    let state = error
        .sql_state
        .clone()
        .map(|state| ts!("query.sql_state", state = state.to_string()));
    div()
        .flex()
        .flex_col()
        .flex_1()
        .min_w_0()
        .min_h_0()
        .gap(px(6.))
        .p(px(16.))
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
        .into_any_element()
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
        let results = self.render_results(&chrome);
        let context_menu = self.render_context_menu(cx);

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
        fn grid_at(&self, index: usize) -> &Entity<GridView<ResultSource>> {
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
        cx.add_window(move |_window, cx| {
            QueryPane::new(session, ConnectionId(1), &profile, "h2", &settings, "", cx)
        })
    }

    /// Runs `sql` and waits for the whole pipeline to settle.
    fn run(window: &WindowHandle<QueryPane>, sql: &str, cx: &mut TestAppContext) {
        window
            .update(cx, |pane, _window, cx| {
                pane.request(vec![sql.to_string()], cx);
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
                .update(cx, |pane, _window, cx| pane.fetch_more(id, cx))
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
                .update(cx, |pane, _window, cx| {
                    pane.reorder(id, 0, direction, cx);
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
            .update(cx, |pane, _window, cx| {
                pane.fetch_more(id, cx);
                pane.request(vec!["select 7".to_string()], cx);
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
            .update(cx, |pane, _window, cx| pane.confirmed(cx))
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
            .update(cx, |pane, _window, cx| {
                assert!(!pane.is_running(), "a detached pane sent a statement");
                assert_eq!(pane.notice, Some(ts!("explorer.disconnected")));
                assert_eq!(
                    pane.results.len(),
                    1,
                    "the results of the last live run were replaced"
                );

                pane.notice = None;
                pane.reorder(pane.results[0].id, 0, Some(SortDirection::Descending), cx);
                assert_eq!(pane.notice, Some(ts!("explorer.disconnected")));

                pane.notice = None;
                pane.fetch_more(pane.results[0].id, cx);
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
            ]
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
        ] {
            assert!(!label.is_empty(), "empty label");
            assert!(!label.starts_with("context."), "untranslated {label:?}");
        }
    }
}
