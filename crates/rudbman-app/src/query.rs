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

mod editing;
mod execution;
mod menus;
mod render;
use render::{elapsed_label, preview};
pub use render::{error_lines, note, render_error};
mod results;
mod statements;
use statements::source_table;
pub use statements::{is_write_statement, order_by};
use std::sync::Arc;
use std::time::{Duration, Instant};

use gpui::{
    Action, AnyElement, App, Axis, Context, Div, DragMoveEvent, Entity, EntityId, EventEmitter,
    FocusHandle, Focusable, Hsla, IntoElement, ParentElement, Pixels, Point, Render, SharedString,
    Styled, Subscription, Window, div, prelude::*, px, relative,
};
use rudbman_core::{AppSettings, ConnectionProfile};
use rudbman_jdbc::{
    BridgeErrorKind, Canceller, ColumnInfo, Cursor, Error as JdbcError, StatementSpec,
};
use rudbman_sql::{Dialect, StatementAccess, split_statements, statement_access};
use rugpui::{Button, ButtonVariant, ContextMenu, ResizeHandle, Theme, theme};
use rugpui_editor::editor::{
    Copy, Cut, Find, Paste, Redo, Replace, RunAll, RunSelection, RunStatement, SelectAll,
    ToggleComment, Undo,
};
use rugpui_editor::{EditorEvent, EditorView};
use rugpui_grid::{
    GridCell, GridEvent, GridSource, GridSourceState, GridView, MenuTarget, RowStatus,
    SortDirection,
};

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
use crate::sql_highlight::DialectHighlighter;

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
///
/// The strip is what answers the press; the accent bar
/// [`rugpui::ResizeHandle`] fades in inside it is thinner, and is the only part
/// of the divider the eye ever sees. Six pixels is what the workspace gives its
/// own dividers, so every seam in the window is equally easy to hit.
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

/// A table's three name parts, each `None` where the driver would not say.
///
/// The order is the catalogue's own — catalog, schema, table — which is also
/// the order [`crate::row_apply::primary_key`] takes them in.
type TableName = (Option<String>, Option<String>, String);

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
            let mut editor = EditorView::new(cx)
                .highlighter(DialectHighlighter::shared(dialect))
                // Unlike the fonts, wrapping is state the editor carries rather
                // than something it reads back from the settings each frame, so
                // a pane opened while the switch is on has to be built with it
                // and a pane already open is handed the change by the shell.
                .word_wrap(settings.editor_word_wrap);
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
                    // `Changed` and `SelectionChanged` are drawn from the
                    // editor's own state on the next frame; `Intercepted` is a
                    // navigation key handed back by an editor that was asked to
                    // hand them back, which this pane never asks for.
                    EditorEvent::Changed
                    | EditorEvent::SelectionChanged
                    | EditorEvent::Intercepted(_) => {}
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

    /// Wraps — or stops wrapping — the lines in this pane's editor.
    ///
    /// The settings dialog's counterpart of the `word_wrap` the constructor
    /// applies: [`crate::workspace::Workspace::apply_settings`] walks every open pane so a
    /// change reaches the editors already on screen, not only the next one.
    pub fn set_word_wrap(&mut self, wrap: bool, cx: &mut Context<Self>) {
        self.editor
            .update(cx, |editor, cx| editor.set_word_wrap(wrap, cx));
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
        window.focus(&handle, cx);
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
            .filter(|sql| {
                !split_statements(sql, &self.dialect).is_empty()
                    || sql.contains("/*!")
                    || sql.contains("/*M!")
            })
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
}

impl Focusable for QueryPane {
    /// The editor's handle: focusing the pane means putting the caret in the
    /// SQL, which is the only thing in here anyone types into.
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.editor.read(cx).focus_handle(cx)
    }
}

#[cfg(test)]
mod tests;
