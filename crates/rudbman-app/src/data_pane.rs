//! The data pane: one table's rows, paged into a grid, and changed in place.
//!
//! What the architecture document's §7.9 draws. The pane runs
//! `SELECT * FROM <table>` of its own accord, pages it as the grid nears the
//! end, re-runs it in a new order when a heading is clicked, and holds whatever
//! the user has typed into it until they apply it or throw it away.
//!
//! # Why a pane and not a fifth detail tab
//!
//! The detail panel is one load of presentation and holds nothing of the
//! session; this holds a way to read on, a generation counter and a staging
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
//!
//! # Where an edit lives
//!
//! Not here. The staging buffer sits in [`crate::data_edit`]'s
//! [`EditableSource`], which is the source the grid is pointed at: a grid draws
//! what its source says, so the one way for a staged value to be on screen is
//! for the source to answer with it. That also puts the buffer inside the thing
//! a sort or a refresh throws away, which is exactly the lifetime it has —
//! §7.9 refuses to carry edits across a reload, because an edit that came back
//! attached to a different row than the one it was typed on is worse than being
//! asked to apply or discard.
//!
//! So the pane's own job is gestures and words: it turns a committed field into
//! a staged cell, draws the menu the grid asks for, counts what is staged for
//! the toolbar, and guards the three paths — sort, refresh and closing the tab
//! — that would take the indices out from under it.
//!
//! # The apply
//!
//! [`DataPane::apply`] plans, [`DataPane::confirm_apply`] sends. The planning is
//! [`crate::data_edit::plan_apply`]'s and happens on this thread, because it is
//! pure and because its failures are about a column the user can go and look at.
//! What comes back is a list of statements, and they are *shown* before any of
//! them runs: the preview is this pane's write confirmation, always drawn, which
//! is a superset of what the profile's `confirm_writes` asks for and therefore
//! answers it.
//!
//! Sending is [`crate::row_apply::apply_batch`], on the session's own worker
//! thread. Its one ordering rule is worth stating here as well as there: on any
//! failure the rollback goes out **before** autocommit is restored, because
//! restoring autocommit first is what commits the half-applied batch on several
//! products. That module holds the whole of the apply that needs a session or a
//! screen — the batch, the preview, the discard confirmation, the failure strip
//! — because §7.9's query-result editing gives it a second caller and the
//! transaction ordering is the last thing to keep two copies of.

use std::sync::Arc;

use gpui::{
    AnyElement, App, Context, Entity, FocusHandle, Focusable, IntoElement, ParentElement, Pixels,
    Point, Render, SharedString, Styled, Subscription, Window, div, prelude::*, px,
};
use rudbman_core::{AppSettings, ConnectionProfile};
use rudbman_jdbc::{ColumnInfo, Cursor, Error as JdbcError, Session, StatementSpec};
use rudbman_sql::Dialect;
use rugpui::{Button, ButtonVariant, ContextMenu, Theme, theme};
use rugpui_grid::{
    GridEvent, GridRequest, GridSource, GridSourceState, GridView, MenuTarget, RowStatus,
    SortDirection,
};

use crate::builder_sql;
use crate::connection::SessionHandle;
use crate::context_menu::{self, MenuRow};
use crate::data_edit::{EditableSource, PlannedStatement, RowRef, StagedCell, plan_apply};
use crate::explorer::{ConnectionId, ObjectTarget};
use crate::i18n::ts;
use crate::notice::{Notice, NoticeClock, render_notice};
use crate::query::{QueryError, note, render_error};
use crate::query_source::{
    ROW_LIMIT, ReadPage, RenderedBatch, ResultSource, Step, advance, page, read_page,
};
use crate::row_apply::{
    ApplyFailure, ApplyProblem, ApplyStop, apply_batch, plan_message, primary_key,
    render_apply_error, render_apply_preview, render_discard_confirm,
};

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
    /// How to read on, while there is more to page.
    more: Option<More>,
    /// The statement this load ran, kept so that every later page is cut
    /// from the same ordering as the first.
    select: Select,
}

/// How the rows after the ones on screen are to be had.
///
/// Two ways, and the dialect picks one for the life of a load (see
/// [`Select`]). The cursor is the way that works everywhere and costs the most:
/// the statement's result set stays open on the connection between one scroll
/// and the next. The offset is the way that leaves nothing open — each page is
/// a statement of its own, read to its end and closed before the page is
/// handed over.
enum More {
    /// The statement's own cursor, still open on the server and paged by
    /// `FETCH`. Dropping it closes it.
    Cursor(Cursor),
    /// Where the next page's statement starts: every row of the table fetched
    /// so far.
    Offset(u64),
}

/// One more page, as the background task hands it over.
struct Page {
    /// The rows that came back, already rendered.
    batch: RenderedBatch,
    /// Whether that was the last of them.
    complete: bool,
    /// How to read on from here, while there is more.
    more: Option<More>,
}

/// The statement a load runs, and what is needed to write each page of it.
///
/// Built on this thread and handed to the background, because the primary key
/// it may order by is only read there, inside [`open`].
///
/// # Why a page is a statement of its own
///
/// Every pane, the explorer and the metadata lookups share one JDBC connection,
/// and an open streaming result set is a poor neighbor on it. MariaDB's
/// Connector/J answers *any* other command sent while one is open by reading
/// the rest of it into the heap first (`fetchRemaining`), so the explorer
/// expanding a folder beside a huge table took the whole table into the
/// bridge's memory; and closing one reads the rest off the network to throw it
/// away (`skipRemaining`), so a sort or a closed tab stalled for as long as the
/// table took to stream. pgjdbc does not stream at all under autocommit and
/// reads everything at `EXECUTE`. So where the dialect has a
/// [`Dialect::page_clause`], each page is `… LIMIT n OFFSET m`, read whole and
/// closed at once, and nothing stays open between one page and the next. The
/// dialects without one keep the cursor, which is no worse than it was.
///
/// Pages cut by offset only line up if the order is total, so a paged
/// statement orders by the primary key — after the user's column, as a tie
/// breaker, when one was asked for. A table with no key has nothing to make
/// the order total with and is read in whatever order the server keeps; the
/// cursor never promised more than that either.
#[derive(Clone, Debug)]
struct Select {
    /// Writes the identifiers and decides the paging.
    dialect: Dialect,
    /// The table, qualified and quoted.
    table: String,
    /// The column the user ordered by, spelled as [`Order::name`] has it, and
    /// which way.
    order: Option<(String, SortDirection)>,
    /// Rows per page, and per `FETCH`.
    fetch_rows: u32,
}

impl Select {
    /// Rows per page. Never zero: `LIMIT 0` would page forever and never get
    /// anywhere, and the settings already refuse it — this is only the floor
    /// under that.
    fn limit(&self) -> u32 {
        self.fetch_rows.max(1)
    }

    /// Whether pages are statements of their own, rather than fetches from one
    /// open cursor.
    fn pages_by_offset(&self) -> bool {
        self.dialect.page_clause(1, 0).is_some()
    }

    /// The statement for the page that starts `offset` rows in.
    ///
    /// Where the dialect has no page clause there are no pages to write, only
    /// the one statement the cursor is opened on, and `keys` and `offset` are
    /// not used: the cursor's statement is exactly what it always was.
    fn statement(&self, keys: &[String], offset: u64) -> String {
        match self.dialect.page_clause(u64::from(self.limit()), offset) {
            Some(clause) => format!("{} {clause}", self.ordered(keys)),
            None => self.ordered(&[]),
        }
    }

    /// Every column, in the user's order and then the key's.
    ///
    /// An appended `ORDER BY` rather than the derived table the query pane
    /// wraps its statement in. That wrapper exists because the user's own SQL
    /// may already carry an ordering and two of them do not compose; this
    /// statement is written here, one clause at a time, so there is nothing
    /// underneath to collide with.
    ///
    /// A key column that is also the user's is not repeated. The comparison is
    /// exact, which on a mismatch of case costs a redundant term and nothing
    /// else, where a case-blind one could drop a different column.
    fn ordered(&self, keys: &[String]) -> String {
        let mut terms = Vec::new();
        if let Some((name, direction)) = &self.order {
            let direction = match direction {
                SortDirection::Ascending => "ASC",
                SortDirection::Descending => "DESC",
            };
            terms.push(format!("{} {direction}", self.dialect.quote_ident(name)));
        }
        let sorted = self.order.as_ref().map(|(name, _)| name.as_str());
        for key in keys.iter().filter(|key| Some(key.as_str()) != sorted) {
            terms.push(format!("{} ASC", self.dialect.quote_ident(key)));
        }

        let mut sql = format!("SELECT * FROM {}", self.table);
        if !terms.is_empty() {
            sql.push_str(" ORDER BY ");
            sql.push_str(&terms.join(", "));
        }
        sql
    }
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
    /// Its name, as the catalog spells it.
    name: String,
    /// Which way.
    direction: SortDirection,
}

/// A right-click in the grid, while the menu it asked for is open.
struct GridMenu {
    /// A cell or a heading, as the grid read the press.
    target: MenuTarget,
    /// Where the pointer was, in window coordinates.
    position: Point<Pixels>,
}

/// The rows on screen, and everything needed to go on reading them.
struct Rows {
    grid: Entity<GridView<EditableSource>>,
    /// The result's logical column types, for rendering later batches.
    columns: Arc<Vec<ColumnInfo>>,
    /// The statement these rows came from, which every later page repeats.
    ///
    /// Held here rather than rebuilt from the pane: a heading click records
    /// its order before the reload it asks for, and a reload can be refused,
    /// so the pane's order is not necessarily the one these rows are in.
    select: Select,
    /// How to read on, while this result is still ours to page. `None` once
    /// the rows ran out, and while a fetch has it.
    more: Option<More>,
    /// Whether a page is out with a background fetch.
    fetching: bool,
    /// Keeps the grid's subscriptions — its events and its key requests —
    /// alive for as long as the rows.
    _events: [Subscription; 2],
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

gpui::actions!(
    data_pane,
    [
        /// Put NULL into the cell under the cursor.
        SetNullCell,
        /// Add a row to type into.
        InsertRow,
        /// Show the staged changes as the statements that would apply them.
        ApplyChanges,
        /// Read the rows again.
        RefreshRows,
    ]
);

/// The key context the pane renders in.
const KEY_CONTEXT: &str = "DataPane";

/// Marks the pane's chords as registered.
struct Bindings;

impl gpui::Global for Bindings {}

/// The chord each command is bound to, as a menu row or a tooltip writes it.
///
/// One table for the bindings and the labels, so that a hint can never name
/// a key the pane does not answer. Never translated, for
/// [`SHORTCUT_MODIFIER`]'s reason.
pub(crate) mod keys {
    /// Strikes the row out.
    pub const DELETE_ROW: &str = "Delete";
    /// NULL into the cell.
    pub const SET_NULL: &str = if cfg!(target_os = "macos") {
        "Cmd+0"
    } else {
        "Ctrl+0"
    };
    /// A new row.
    pub const INSERT_ROW: &str = "Alt+Insert";
    /// The apply preview.
    pub const APPLY: &str = if cfg!(target_os = "macos") {
        "Cmd+S"
    } else {
        "Ctrl+S"
    };
    /// Reading the rows again.
    pub const REFRESH: &str = "F5";
}

/// A toolbar button with its chord as the hover text.
///
/// The button is the widget's and carries no tooltip of its own, so the chord
/// rides on a box around it.
fn hinted(id: &'static str, chord: &'static str, button: Button) -> impl IntoElement {
    div()
        .id(id)
        .flex_none()
        .tooltip(rugpui::tooltip_label(chord))
        .child(button)
}

/// Registers the pane's chords, once per application.
///
/// In the pane's own key context, so each answers wherever the keyboard is
/// inside the pane — on the grid, which is where it is while rows are being
/// edited — and wins over the window's `Ctrl+S`, which has nothing to save
/// over a data tab. The grid's cell editor is deeper still, so `Delete` typed
/// into a cell being edited deletes a character, not the row.
fn bind_keys(cx: &mut App) {
    if cx.has_global::<Bindings>() {
        return;
    }
    let modifier = if cfg!(target_os = "macos") {
        "cmd"
    } else {
        "ctrl"
    };
    cx.bind_keys([
        gpui::KeyBinding::new("shift-delete", SetNullCell, Some(KEY_CONTEXT)),
        gpui::KeyBinding::new(&format!("{modifier}-0"), SetNullCell, Some(KEY_CONTEXT)),
        gpui::KeyBinding::new("alt-insert", InsertRow, Some(KEY_CONTEXT)),
        gpui::KeyBinding::new(&format!("{modifier}-s"), ApplyChanges, Some(KEY_CONTEXT)),
        gpui::KeyBinding::new(
            &format!("{modifier}-enter"),
            ApplyChanges,
            Some(KEY_CONTEXT),
        ),
        gpui::KeyBinding::new("f5", RefreshRows, Some(KEY_CONTEXT)),
    ]);
    cx.set_global(Bindings);
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
    /// Whether the product behind this session has transactions.
    ///
    /// From `SessionInfo::supports_transactions`, and a driver that would not
    /// say is taken as having them: `set_auto_commit(false)` against a product
    /// that has none fails loudly and visibly, while skipping the transaction on
    /// one that has them gives up the guarantee in silence. False makes the
    /// apply run statement by statement under autocommit, and makes the
    /// confirmation say so.
    transactional: bool,
    /// The autocommit setting the session was opened with (§8).
    ///
    /// What the apply puts *back*, rather than a flat `true`: a profile opened
    /// with autocommit off is a session the user asked to be in a transaction,
    /// and an apply that handed it back in the other mode would be an edit to
    /// the connection nobody made.
    restore_auto_commit: bool,
    /// Rows per `FETCH`, from the settings. Read once, when the pane opens.
    fetch_rows: u32,
    /// The primary key's columns, in key order.
    ///
    /// The other reason the pane may only browse — a table with none cannot be
    /// edited by primary key — and, from the next milestone, what the generated
    /// `WHERE` clause is written from.
    keys: Vec<String>,
    /// Whether [`DataPane::keys`] is an answer rather than a starting value.
    ///
    /// A table whose key is still on its way has not said it has none, so the
    /// read-only banner and the writability of the grid both wait for this. It
    /// is a flag of its own rather than "the rows have arrived" because the
    /// grid is built — and must already know whether it is writable — inside
    /// the same delivery that sets the key.
    keys_read: bool,
    load: Load,
    /// The order the rows were asked in, when one was asked for.
    order: Option<Order>,
    /// A line the pane wants to say, at the level it matters; see
    /// [`crate::notice`].
    notice: Option<Notice>,
    /// Takes an informational notice away after a while.
    notice_clock: NoticeClock,
    /// The Cancel button of the apply preview and the discard question, which
    /// takes the keyboard when either opens.
    confirm_focus: FocusHandle,
    /// Reaches the driver under the load in flight, for its "Cancel"; `None`
    /// while nothing is loading.
    canceller: Option<rudbman_jdbc::Canceller>,
    /// The generation of the newest load. Every delivery carries one, and one
    /// that is not this is an answer a later load has already replaced.
    generation: u64,
    /// The grid's right-click menu, while one is open.
    context_menu: Option<GridMenu>,
    /// Whether "discard everything" is waiting to be confirmed.
    ///
    /// A modal of the pane's own rather than the shell's, and the one place
    /// this pane departs from how the query pane asks (`PendingConfirm` in
    /// `main`). The reason is what the confirmation is *about*: a query pane
    /// asks before touching the database and the shell is the thing that must
    /// not be usable meanwhile, while this asks before throwing away work that
    /// lives in one tab — so the sheet belongs over that tab, and the pane can
    /// be tested without a workspace around it.
    confirm_discard: bool,
    /// The statements an apply would send, while they are being shown.
    ///
    /// This pane's write confirmation, and the one modal it always raises: the
    /// question "are you sure" is worth very little next to the statements
    /// themselves, and a user who can read the `UPDATE` can see the `WHERE`
    /// clause that decides how much of their table it reaches.
    preview: Option<Vec<PlannedStatement>>,
    /// Whether a batch is out on the session.
    ///
    /// Not a state of [`DataPane::load`] because the rows are still there and
    /// still readable while it runs; what it gates is a second Apply on top of
    /// the first.
    applying: bool,
    /// Why the last apply did not happen.
    ///
    /// Held apart from [`DataPane::notice`] and drawn in the danger color: a
    /// failed apply leaves every staged change exactly where it was, and a line
    /// that faded in with the other passing remarks would be the wrong weight
    /// for "nothing you asked for has happened".
    apply_error: Option<Box<ApplyProblem>>,
    focus_handle: FocusHandle,
    lob_dialog: Option<Entity<crate::lob_dialog::LobDialog>>,
    lob_events: Option<Subscription>,
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
        bind_keys(cx);
        Self {
            target,
            connection,
            session: Some(session),
            dialect: Dialect::from_id(driver_dialect),
            read_only: profile.read_only,
            transactional: true,
            restore_auto_commit: profile.auto_commit,
            fetch_rows: settings.fetch_batch_rows,
            keys: Vec::new(),
            keys_read: false,
            load: Load::Running,
            order: None,
            notice: None,
            notice_clock: NoticeClock::default(),
            canceller: None,
            generation: 0,
            context_menu: None,
            confirm_discard: false,
            preview: None,
            applying: false,
            apply_error: None,
            focus_handle: cx.focus_handle(),
            confirm_focus: cx.focus_handle(),
            lob_dialog: None,
            lob_events: None,
        }
    }

    /// Records whether the product behind the session has transactions.
    ///
    /// A builder step rather than another constructor argument, and for a
    /// reason of its own: everything [`DataPane::new`] takes is a fact about the
    /// *object* being browsed or about the settings it is browsed under, and
    /// this is a fact about the product. It is passed in rather than asked for
    /// because the host already has the `SESSION_INFO` this pane would otherwise
    /// spend a round trip on.
    pub fn with_transactions(mut self, supported: bool) -> Self {
        self.transactional = supported;
        self
    }

    /// The object whose rows these are.
    pub fn target(&self) -> &ObjectTarget {
        &self.target
    }

    pub(crate) fn workspace_order(&self) -> Option<(usize, String, bool)> {
        self.order.as_ref().map(|order| {
            (
                order.column,
                order.name.clone(),
                order.direction == SortDirection::Ascending,
            )
        })
    }

    pub(crate) fn restore_workspace_order(&mut self, order: Option<(usize, String, bool)>) {
        self.order = order.map(|(column, name, ascending)| Order {
            column,
            name,
            direction: if ascending {
                SortDirection::Ascending
            } else {
                SortDirection::Descending
            },
        });
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
        if let Some(dialog) = &self.lob_dialog {
            dialog.update(cx, |dialog, cx| dialog.detach(cx));
        }
        self.session = None;
        // Dropping the way on closes a cursor, if that is what it was, and is
        // what stops a scroll asking a session that is being closed for
        // another page.
        if let Load::Ready(rows) = &mut self.load {
            rows.more = None;
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
        if self.lob_dialog.as_ref().is_some_and(|dialog| {
            dialog
                .read(cx)
                .focus_handle(cx)
                .contains_focused(window, cx)
        }) {
            return true;
        }
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
        if let Some(dialog) = &self.lob_dialog {
            window.focus(&dialog.read(cx).focus_handle(cx), cx);
            return;
        }
        match &self.load {
            Load::Ready(rows) => {
                let handle = rows.grid.read(cx).focus_handle(cx);
                window.focus(&handle, cx);
            }
            Load::Running | Load::Failed(_) => window.focus(&self.focus_handle, cx),
        }
    }

    /// Whether the pane is holding changes the user has not applied.
    ///
    /// Asked by every path that replaces the rows wholesale — a sort, a
    /// refresh, and closing the tab — because a staged edit is keyed to a row
    /// index of the source it was typed on and nothing else (§7.9). Public
    /// because the last of those three is the shell's: the tab strip is what
    /// closes a tab, so the tab strip is what has to ask.
    pub fn has_pending_edits(&self, cx: &App) -> bool {
        if self
            .lob_dialog
            .as_ref()
            .is_some_and(|dialog| dialog.read(cx).has_changes())
        {
            return true;
        }
        match &self.load {
            Load::Ready(rows) => !rows.grid.read(cx).source().edits().is_empty(),
            Load::Running | Load::Failed(_) => false,
        }
    }

    /// Whether the pane has a sheet of its own up — the batch preview, the
    /// discard question or a value viewer — which the window's shortcuts must
    /// not act behind.
    pub fn modal_open(&self) -> bool {
        self.preview.is_some() || self.confirm_discard || self.lob_dialog.is_some()
    }

    /// Says, in the pane itself, why the gesture that was just tried is being
    /// refused while changes are staged.
    ///
    /// The counterpart of [`DataPane::has_pending_edits`] for the shell: a tab
    /// that simply would not close, with nothing said, would read as a bug.
    pub fn warn_pending(&mut self, cx: &mut Context<Self>) {
        self.notice = Some(Notice::warn(ts!("data.discard_first")));
        cx.notify();
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
        if self.keys_read && self.keys.is_empty() {
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
    /// for. [`Select`] writes it, once the key it pages by is known.
    fn select(&self) -> Select {
        Select {
            dialect: self.dialect,
            table: self.qualified(),
            order: self
                .order
                .as_ref()
                .map(|order| (order.name.clone(), order.direction)),
            fetch_rows: self.fetch_rows,
        }
    }

    /// Reads the key and the first batch again, from scratch.
    ///
    /// Both together on one background task: they are wanted at the same
    /// moment, and the key has to be known before the grid is built or the key
    /// columns would draw unmarked for a frame and then jump.
    pub fn refresh(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.lob_dialog.is_some() {
            return;
        }
        if self.has_pending_edits(cx) {
            self.notice = Some(Notice::warn(ts!("data.discard_first")));
            cx.notify();
            return;
        }
        let Some(session) = self.session.clone() else {
            self.notice = Some(Notice::warn(ts!("explorer.disconnected")));
            cx.notify();
            return;
        };

        self.generation += 1;
        let generation = self.generation;
        // A read of a big table sorted by a column with no index can take as
        // long as the server needs to sort it; the canceller reaches the
        // statement without queueing behind it, as the query pane's does.
        self.canceller = Some(session.session().canceller());
        // Dropping what was there drops its cursor, which closes it.
        self.load = Load::Running;
        self.keys_read = false;
        self.notice = None;
        self.context_menu = None;
        self.confirm_discard = false;
        // The rows a plan was made against are on their way out, so the plan
        // goes with them: every statement in it is keyed to a row of the source
        // being replaced.
        self.preview = None;
        self.apply_error = None;

        let target = self.target.clone();
        let select = self.select();

        // `spawn_in` rather than `spawn`: the grid this load ends in is
        // subscribed to with a window, because answering a double click with
        // [`GridView::begin_edit`] means putting the keyboard in a field and
        // there is no window to be had inside a plain `cx.subscribe`.
        cx.spawn_in(window, async move |pane, cx| {
            let outcome = cx
                .background_spawn(async move { open(session.session(), &target, select) })
                .await;
            pane.update_in(cx, |pane, window, cx| {
                pane.deliver(generation, outcome, window, cx);
            })
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
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if generation != self.generation {
            // A superseded load's answer. Dropping it closes its cursor, if it
            // kept one open.
            return;
        }
        self.canceller = None;
        match outcome {
            Ok(opened) => {
                let Opened {
                    keys,
                    columns,
                    batch,
                    complete,
                    more,
                    select,
                } = opened;
                self.keys = keys;
                self.keys_read = true;

                let mut source = ResultSource::new(&columns);
                source.mark_primary_keys(&self.keys);
                if let Some(batch) = batch {
                    source.push(batch);
                }
                source.set_state(if complete && more.is_none() {
                    GridSourceState::Complete
                } else {
                    GridSourceState::HasMore
                });
                // Whether anything here may be written is settled now and not
                // again: both of §7.9's reasons — a read-only profile and a
                // table with no key — are facts about the object, and `keys`
                // has just been set from the metadata this same load read.
                let writable = self.read_only_reason().is_none();
                let source = EditableSource::new(
                    source,
                    &columns,
                    writable,
                    crate::data_edit::TableSource::Known,
                );

                let table = self.qualified();
                // `Delete`, `F2`, typing and `Ctrl+V` over the grid come back
                // as requests (see `answer_grid`), because what each one means
                // — a delete mark, a field, a staged paste — is this pane's.
                let grid = cx.new(|cx| {
                    GridView::new(source, cx)
                        .insert_table(table)
                        .expand_tooltip(ts!("lob.open_text"))
                        .key_requests(true)
                });
                // The marker the sort that asked for this run was for: the grid
                // that was wearing it has just been replaced by this one.
                if let Some(order) = &self.order {
                    let sort = Some((order.column, order.direction));
                    grid.update(cx, |grid, cx| grid.set_sort(sort, cx));
                }
                let events =
                    cx.subscribe_in(
                        &grid,
                        window,
                        |pane, _grid, event, window, cx| match event {
                            GridEvent::NearEnd => pane.fetch_more(cx),
                            GridEvent::SortRequested { column, direction } => {
                                pane.reorder(*column, *direction, window, cx);
                            }
                            // LOB columns use the shared viewer; other cells
                            // keep the grid's inline field and writability checks.
                            GridEvent::CellActivated { row, column } => {
                                pane.open_cell(*row, *column, window, cx);
                            }
                            // The one-line field's expand button: the same
                            // viewer, started from what had been typed.
                            GridEvent::ExpandRequested { row, column, text } => {
                                pane.open_value_editor(
                                    *row,
                                    *column,
                                    Some(text.clone()),
                                    window,
                                    cx,
                                );
                            }
                            GridEvent::EditCommitted { row, column, value } => {
                                // The two ways a field ends. An emptied field is
                                // the empty string — that is how one is typed —
                                // and only the clearing gesture asks for `NULL`,
                                // which is the same thing the grid's own "set
                                // null" row stages.
                                let staged = match value {
                                    rugpui_grid::EditValue::Text(text) => {
                                        StagedCell::Text(text.clone())
                                    }
                                    rugpui_grid::EditValue::Null => StagedCell::Null,
                                };
                                pane.stage(*row, *column, staged, cx);
                            }
                            // The grid holds no strings, so its menu is drawn here
                            // (architecture document, §7.8).
                            GridEvent::ContextMenu { target, position } => {
                                pane.context_menu = Some(GridMenu {
                                    target: *target,
                                    position: *position,
                                });
                                cx.notify();
                            }
                        },
                    );

                let requests = cx.subscribe_in(
                    &grid,
                    window,
                    |pane, _grid, request: &GridRequest, window, cx| {
                        pane.answer_grid(request, window, cx);
                    },
                );

                self.load = Load::Ready(Box::new(Rows {
                    grid,
                    columns: Arc::new(columns),
                    select,
                    more,
                    fetching: false,
                    _events: [events, requests],
                }));
            }
            Err(error) => self.load = Load::Failed(Box::new(QueryError::new(error))),
        }
        cx.notify();
    }

    /// The grid has come within sight of the last row it holds.
    fn fetch_more(&mut self, cx: &mut Context<Self>) {
        let Some(session) = self.session.clone() else {
            // `detach` has already dropped the way on, so this is only reached
            // by a scroll that was already in flight; saying why the rows stop
            // is better than a grid that quietly ends early.
            self.notice = Some(Notice::warn(ts!("explorer.disconnected")));
            cx.notify();
            return;
        };
        let generation = self.generation;
        let fetch_rows = self.fetch_rows;
        // The key the first page was ordered by: it was read by the same load
        // that built these rows, and only a later load replaces it.
        let keys = self.keys.clone();
        let Load::Ready(rows) = &mut self.load else {
            return;
        };
        if rows.fetching {
            return;
        }
        let Some(more) = rows.more.take() else {
            return;
        };
        rows.fetching = true;
        let columns = Arc::clone(&rows.columns);
        let select = rows.select.clone();
        let grid = rows.grid.clone();
        // While a batch is on its way the source says so, which is what keeps a
        // fast scroll from asking once per frame.
        grid.update(cx, |grid, cx| {
            grid.source_mut(cx).set_state(GridSourceState::Loading);
        });

        cx.spawn(async move |pane, cx| {
            let outcome = cx
                .background_spawn(async move {
                    match more {
                        More::Cursor(mut cursor) => {
                            page(&mut cursor, &columns, fetch_rows).map(|paged| Page {
                                // A completed result set is behind the cursor
                                // now: the walk in `page` has already moved
                                // past it. The further results `Paged::steps`
                                // may carry are a stored procedure's, and a
                                // `SELECT` over one table has none.
                                more: (!paged.complete).then_some(More::Cursor(cursor)),
                                batch: paged.batch,
                                complete: paged.complete,
                            })
                        }
                        More::Offset(offset) => {
                            next_page(session.session(), &select, &keys, offset)
                        }
                    }
                })
                .await;
            pane.update(cx, |pane, cx| pane.paged(generation, outcome, cx))
                .ok();
        })
        .detach();
    }

    /// Records a page.
    fn paged(&mut self, generation: u64, outcome: Result<Page, JdbcError>, cx: &mut Context<Self>) {
        if generation != self.generation {
            // The load this belonged to has been superseded. Dropping the
            // outcome closes its cursor, if it held one, and the rows never
            // reach a grid.
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
            Ok(paged) => {
                rows.more = paged.more;
                let grid = rows.grid.clone();
                let limited = grid.update(cx, |grid, cx| {
                    // Into the rows the server sent, under the overlay: the
                    // staged edits are keyed to base row indices, and appending
                    // moves none of them.
                    grid.source_mut(cx)
                        .base_mut()
                        .push_page(paged.batch, paged.complete)
                });
                if limited {
                    failure = Some(ts!("query.row_limit", limit = ROW_LIMIT));
                }
            }
            Err(error) => {
                rows.more = None;
                let grid = rows.grid.clone();
                grid.update(cx, |grid, cx| {
                    grid.source_mut(cx).set_state(GridSourceState::Complete);
                });
                // The rows already fetched stay on screen: what failed is the
                // next page, not the result under it.
                failure = Some(QueryError::new(error).message);
            }
        }
        self.notice = failure.map(Notice::error);
        cx.notify();
    }

    /// A heading was clicked: run the same `SELECT` in that order.
    fn reorder(
        &mut self,
        column: usize,
        direction: Option<SortDirection>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.session.is_none() {
            self.notice = Some(Notice::warn(ts!("explorer.disconnected")));
            cx.notify();
            return;
        }
        if self.has_pending_edits(cx) {
            // A re-run replaces the source the edits are keyed to; §7.9.
            self.notice = Some(Notice::warn(ts!("data.discard_first")));
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
        self.refresh(window, cx);
    }

    /// How many rows are on screen, which is however many have been paged in.
    ///
    /// `None` while there is no result at all — a load in flight, or one that
    /// failed — because zero rows and no answer are different things and the
    /// toolbar says so by drawing nothing.
    pub fn row_count(&self, cx: &App) -> Option<usize> {
        match &self.load {
            // The rows the *server* has, not the ones the grid draws: a row the
            // user is adding is not yet a row of the table, and counting it
            // here would have the toolbar report a table one row longer than
            // the one the next refresh reads back.
            Load::Ready(rows) => Some(rows.grid.read(cx).source().base_rows()),
            Load::Running | Load::Failed(_) => None,
        }
    }

    /// The grid, while there is one.
    pub(crate) fn grid(&self) -> Option<&Entity<GridView<EditableSource>>> {
        match &self.load {
            Load::Ready(rows) => Some(&rows.grid),
            Load::Running | Load::Failed(_) => None,
        }
    }

    /// Records `value` against a cell, through the overlay that knows which row
    /// space the index falls in.
    ///
    /// The one way anything is staged. A value equal to the one the server gave
    /// un-stages the cell instead, which is why this goes through
    /// [`EditableSource::stage`] rather than writing into the buffer directly.
    fn stage(&mut self, row: usize, column: usize, value: StagedCell, cx: &mut Context<Self>) {
        let Some(grid) = self.grid().cloned() else {
            return;
        };
        grid.update(cx, |grid, cx| {
            grid.source_mut(cx).stage(row, column, value);
        });
        cx.notify();
    }

    /// The cell the grid's menu should act on: wherever the caret was left.
    ///
    /// A right click has already moved the selection onto the pressed cell
    /// unless it landed inside one, so the cursor is the cell the user means.
    /// The answer is a *source* column: the selection is kept in display
    /// positions, and everything on this side of the grid — the staging buffer,
    /// the column rules, [`GridSource::cell`] — is addressed in source ones.
    fn menu_cell(&self, cx: &App) -> Option<(usize, usize)> {
        let grid = self.grid()?.read(cx);
        let cursor = grid.selection().cursor()?;
        let column = grid.visible_column_indices().get(cursor.column).copied()?;
        Some((cursor.row, column))
    }

    /// Puts NULL into a cell, deliberately.
    ///
    /// The gesture the inline editor cannot offer: an empty field over a null
    /// cell commits nothing (that is what keeps opening one and thinking better
    /// of it from writing `''`), so clearing a cell has to be a command of its
    /// own. Doing it to a cell that is already NULL stages nothing, by the same
    /// rule that un-stages an edit that walked back to where it started.
    fn set_null(&mut self, row: usize, column: usize, cx: &mut Context<Self>) {
        self.stage(row, column, StagedCell::Null, cx);
    }

    /// What the grid asked for from the keyboard.
    ///
    /// * `Delete` (and `Backspace` on macOS) strikes the selected rows out, or
    ///   takes the marks off once every one of them carries one —
    ///   [`EditableSource::toggle_rows_deleted`]. A row added here is passed
    ///   over; its menu offers Discard.
    /// * `F2` or a typed character opens the cell the way a double click does,
    ///   with the character already in the field — or, for a LOB or a
    ///   multiline value, the viewer, which is where those are typed.
    /// * `Ctrl+V` stages tab-separated clipboard text into the cells from the
    ///   cursor on, or one value into every selected cell
    ///   ([`crate::data_edit::paste_targets`]), passing over cells that take no
    ///   typing.
    ///
    /// The grid only asks over an editable cell or a selection, so a request
    /// that finds the pane read-only — a profile, a table with no key, a load
    /// or apply running — is simply not acted on.
    fn answer_grid(&mut self, request: &GridRequest, window: &mut Window, cx: &mut Context<Self>) {
        let Some(grid) = self.grid().cloned() else {
            return;
        };
        if self.read_only_reason().is_some() || self.applying {
            return;
        }
        match request {
            GridRequest::Delete => {
                grid.update(cx, |grid, cx| {
                    let rows = crate::data_edit::selected_rows(grid.selection());
                    grid.source_mut(cx).toggle_rows_deleted(rows);
                });
                cx.notify();
            }
            GridRequest::Edit { row, column, text } => {
                if grid.read(cx).source().uses_value_viewer(*row, *column) {
                    self.open_value_editor(*row, *column, text.clone(), window, cx);
                    return;
                }
                grid.update(cx, |grid, cx| match text {
                    Some(text) => grid.begin_edit_with(*row, *column, text, window, cx),
                    None => grid.begin_edit(*row, *column, window, cx),
                });
            }
            GridRequest::Paste { text } => {
                grid.update(cx, |grid, cx| {
                    let cells = crate::data_edit::paste_targets(
                        grid.selection(),
                        &grid.visible_column_indices(),
                        crate::data_edit::parse_tsv(text),
                    );
                    grid.source_mut(cx).paste(cells);
                });
                cx.notify();
            }
        }
    }

    /// `Ctrl+0`: NULL into the cell under the cursor, when it may hold one.
    fn key_set_null(&mut self, cx: &mut Context<Self>) {
        let Some(grid) = self.grid() else {
            return;
        };
        let Some((row, column)) = self.menu_cell(cx) else {
            return;
        };
        let source = grid.read(cx).source();
        if source.writable() && source.cell_editable(row, column) && source.nullable(column) {
            self.set_null(row, column, cx);
        }
    }

    /// Puts NULL into a cell, for the shell's tests, which stage an edit to
    /// see what a close does about it without laying out a grid to click.
    #[cfg(test)]
    pub(crate) fn null_for_test(&mut self, row: usize, column: usize, cx: &mut Context<Self>) {
        self.set_null(row, column, cx);
    }

    fn open_cell(
        &mut self,
        row: usize,
        column: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(grid) = self.grid().cloned() else {
            return;
        };
        if grid.read(cx).source().uses_value_viewer(row, column) {
            self.open_value_editor(row, column, None, window, cx);
        } else {
            grid.update(cx, |grid, cx| grid.begin_edit(row, column, window, cx));
        }
    }

    /// Opens the shared LOB and multiline text viewer over one cell.
    ///
    /// `seed` is what the grid's one-line field held when its expand button
    /// was pressed, which the viewer starts from instead of the cell's value;
    /// see [`LobDialog::seeded`](crate::lob_dialog::LobDialog::seeded).
    fn open_value_editor(
        &mut self,
        row: usize,
        column: usize,
        seed: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(grid) = self.grid().cloned() else {
            return;
        };
        if self.lob_dialog.is_some() || self.applying || self.preview.is_some() {
            return;
        }
        self.context_menu = None;
        let attached = self.session.is_some();
        let dialog = cx.new(|cx| match seed {
            Some(text) => crate::lob_dialog::LobDialog::seeded(
                grid.clone(),
                row,
                column,
                attached,
                text,
                window,
                cx,
            ),
            None => {
                crate::lob_dialog::LobDialog::new(grid.clone(), row, column, attached, window, cx)
            }
        });
        self.lob_events = Some(cx.subscribe_in(
            &dialog,
            window,
            move |pane, _, _: &crate::lob_dialog::Closed, window, cx| {
                pane.lob_dialog = None;
                pane.lob_events = None;
                window.focus(&grid.read(cx).focus_handle(cx), cx);
                cx.notify();
            },
        ));
        self.lob_dialog = Some(dialog);
        cx.notify();
    }

    /// Adds a row above the fetched rows, and starts typing into it.
    /// Paging can then append without moving the new row or its editor.
    fn insert_row(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.read_only_reason().is_some() {
            return;
        }
        let Some(grid) = self.grid().cloned() else {
            return;
        };
        // Commit events use display indices. Let the current field's event
        // reach the staging buffer before prepending shifts those indices.
        grid.update(cx, |grid, cx| grid.commit_edit(cx));
        cx.defer_in(window, move |pane, window, cx| {
            if pane.grid() != Some(&grid) || pane.read_only_reason().is_some() {
                return;
            }
            let first = grid.update(cx, |grid, cx| {
                let source = grid.source_mut(cx);
                let columns = source.column_count();
                source.edits_mut().add_insert(columns);
                grid.scroll_to_row(0, cx);
                // A hidden column has nowhere to draw the input field.
                (0..columns).find(|column| {
                    grid.source().cell_editable(0, *column) && !grid.is_column_hidden(*column)
                })
            });
            if let Some(column) = first {
                // The scroll handle is applied during layout. Opening the
                // field before that frame would place it at the old offset
                // and let the grid dismiss it as off screen.
                cx.on_next_frame(window, move |pane, window, cx| {
                    if pane.grid() == Some(&grid)
                        && grid.read(cx).source().row_status(0) == RowStatus::Inserted
                    {
                        pane.open_cell(0, column, window, cx);
                    }
                });
            }
            cx.notify();
        });
    }

    /// Marks a row to be deleted, or takes the mark off.
    ///
    /// Only a base row: an inserted row is not in the table, so there is
    /// nothing to delete — discarding it is what takes it away.
    fn toggle_delete(&mut self, row: usize, cx: &mut Context<Self>) {
        if self.read_only_reason().is_some() {
            return;
        }
        let Some(grid) = self.grid().cloned() else {
            return;
        };
        grid.update(cx, |grid, cx| {
            let source = grid.source_mut(cx);
            if let Some(RowRef::Base(row)) = source.edits().locate(row, source.base_rows()) {
                source.edits_mut().toggle_deleted(row);
            }
        });
        cx.notify();
    }

    /// Throws away everything staged against one row.
    fn discard_row(&mut self, row: usize, cx: &mut Context<Self>) {
        let Some(grid) = self.grid().cloned() else {
            return;
        };
        grid.update(cx, |grid, cx| {
            if grid.editing().is_some_and(|(editing, _)| editing == row) {
                grid.cancel_edit(cx);
            } else {
                grid.commit_edit(cx);
            }
        });
        let pane = cx.entity();
        cx.defer(move |cx| {
            pane.update(cx, |pane, cx| {
                if pane.grid() != Some(&grid) {
                    return;
                }
                // Any other field commits before removing an inserted row
                // changes the display indices below it.
                grid.update(cx, |grid, cx| {
                    let source = grid.source_mut(cx);
                    let base = source.base_rows();
                    source.edits_mut().discard_row(row, base);
                    grid.clear_selection(cx);
                });
                cx.notify();
            });
        });
    }

    /// Throws away everything staged, and puts the confirmation away with it.
    fn discard_all(&mut self, cx: &mut Context<Self>) {
        self.confirm_discard = false;
        // A plan describes changes that are about to stop existing, and a
        // failure describes an attempt to send them.
        self.preview = None;
        self.apply_error = None;
        let Some(grid) = self.grid().cloned() else {
            return;
        };
        grid.update(cx, |grid, cx| {
            grid.cancel_edit(cx);
            grid.source_mut(cx).edits_mut().discard_all();
            grid.clear_selection(cx);
        });
        self.notice = None;
        cx.notify();
    }

    /// Plans the staged changes and puts the statements up for confirmation.
    ///
    /// Half of §7.9's apply, and the half that sends nothing: what it produces
    /// is a list of statements and a modal over them. [`DataPane::confirm_apply`]
    /// is what runs them.
    ///
    /// The read-only check is made again here even though the button that calls
    /// it is not drawn on a pane that has one. Both of §7.9's reasons are
    /// permanent, so this cannot fire — which is the point: this is the only
    /// function in the pane that leads to a write, and it is worth its two lines
    /// to have the refusal stated where the write is rather than only where the
    /// button is.
    fn apply(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        if self.lob_dialog.is_some() {
            return;
        }
        if self.read_only_reason().is_some() || self.applying || self.preview.is_some() {
            return;
        }
        // The batch ends with a commit or a rollback, or for an `ALTER` with an
        // autocommit flip, and on a session every pane shares that would end a
        // query editor's open transaction along with it.
        if self
            .session
            .as_ref()
            .is_some_and(SessionHandle::has_uncommitted)
        {
            self.notice = Some(Notice::warn(
                self.session
                    .as_ref()
                    .map(SessionHandle::uncommitted_refusal)
                    .unwrap_or_else(|| ts!("query.uncommitted_pending")),
            ));
            cx.notify();
            return;
        }
        // One modal at a time: the two are irreversible in opposite directions
        // and stacking them would leave the user answering the wrong one.
        self.confirm_discard = false;
        self.apply_error = None;

        let planned = {
            let Load::Ready(rows) = &self.load else {
                return;
            };
            let source = rows.grid.read(cx).source();
            if source.edits().is_empty() {
                return;
            }
            let table = builder_sql::table_parts(
                self.target.catalog.as_deref(),
                self.target.schema.as_deref(),
                &self.target.name,
            );
            plan_apply(source, &rows.columns, table, &self.keys, &self.dialect)
        };

        match planned {
            // Nothing to send, which only a buffer that counted as non-empty
            // and generated no statement can produce. Saying nothing is right:
            // there is no failure and no change.
            Ok(statements) if statements.is_empty() => {}
            Ok(statements) => self.preview = Some(statements),
            Err(error) => self.apply_error = Some(ApplyProblem::local(plan_message(&error))),
        }
        cx.notify();
    }

    /// Runs the statements the confirmation showed.
    ///
    /// The batch goes out on the session's own worker thread, so it queues
    /// behind whatever else this connection is doing rather than racing it, and
    /// the whole of the transaction — the autocommit flip, every statement, the
    /// row counts, the commit or the rollback — happens inside that one
    /// background call. Splitting it across awaits would leave the connection
    /// mid-transaction between them, which is a state no other pane on this
    /// session could safely be allowed to find.
    fn confirm_apply(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // Asked again, since an editor may have written in the meantime; the
        // preview stays up so the apply can be confirmed once that is settled.
        if self
            .session
            .as_ref()
            .is_some_and(SessionHandle::has_uncommitted)
        {
            self.notice = Some(Notice::warn(
                self.session
                    .as_ref()
                    .map(SessionHandle::uncommitted_refusal)
                    .unwrap_or_else(|| ts!("query.uncommitted_pending")),
            ));
            cx.notify();
            return;
        }
        let Some(statements) = self.preview.take() else {
            return;
        };
        if self.read_only_reason().is_some() {
            return;
        }
        let Some(session) = self.session.clone() else {
            self.notice = Some(Notice::warn(ts!("explorer.disconnected")));
            cx.notify();
            return;
        };

        self.applying = true;
        self.apply_error = None;
        self.notice = Some(Notice::warn(ts!("data.applying")));
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
                pane.applied(generation, outcome, window, cx);
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    /// Records what one apply did.
    ///
    /// Success throws the staging buffer away and reloads, which is §7.9's
    /// answer to generated keys and triggers: what the server now holds is a
    /// question only the server can answer, and a full reload is the one reading
    /// that cannot be wrong. Failure keeps every staged change exactly where it
    /// was — nothing reached the table, so there is nothing to reconcile — and
    /// says why.
    fn applied(
        &mut self,
        generation: u64,
        outcome: Result<usize, Box<ApplyFailure>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.applying = false;
        if generation != self.generation {
            // The rows this batch was staged against have been replaced. A
            // refresh refuses while anything is staged, so this is only
            // reachable through a discard that raced the batch; either way the
            // indices the buffer holds are not these rows'.
            cx.notify();
            return;
        }
        match outcome {
            Ok(applied) => {
                // Cleared before the reload, not after: `refresh` refuses while
                // anything is staged, and there is nothing left worth keeping —
                // the server has all of it.
                self.discard_all(cx);
                self.refresh(window, cx);
                self.notice = Some(Notice::info(ts!("data.applied", count = applied)));
            }
            Err(failure) => {
                self.notice = None;
                let half_applied = failure.half_applied;
                let reconnect_required = failure.rollback.is_some();
                if let Some(error) = failure.rollback {
                    // The user is told that the batch may be half in; the
                    // driver's account of *why the unwind failed* is a second
                    // envelope with nowhere to go on screen, and losing it
                    // silently would make the report unanswerable.
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
                    reconnect_required,
                }));
            }
        }
        cx.notify();
    }

    /// Puts the grid's right-click menu away.
    ///
    /// What the workspace runs before it opens another overlay, so that only
    /// one full-window backdrop is ever down at a time.
    pub fn close_context_menu(&mut self, cx: &mut Context<Self>) {
        if self.context_menu.take().is_some() {
            cx.notify();
        }
    }

    /// The grid's right-click menu: the cell menu, or the heading one.
    ///
    /// The heading half is [`crate::context_menu`]'s, shared with the query
    /// pane; only where a sort goes differs, and here it is the pane's own
    /// `ORDER BY` rather than a derived table.
    ///
    /// The cell half is that crate's copy rows plus the five commands only a
    /// writable grid has. They are drawn even where they cannot be run, grayed:
    /// a menu that changed shape between a keyed table and a keyless one would
    /// make the user work out which of the two they were looking at, and the
    /// banner above the grid has already said.
    fn grid_rows(&self, target: MenuTarget, cx: &mut Context<Self>) -> Vec<MenuRow> {
        let Some(grid) = self.grid().cloned() else {
            return Vec::new();
        };
        let this = cx.entity();

        let MenuTarget::Cell = target else {
            let MenuTarget::Header { column } = target else {
                unreachable!("the grid has two menu targets");
            };
            return context_menu::grid_header_rows(
                &grid,
                column,
                cx,
                move |direction, window, cx| {
                    this.update(cx, |pane, cx| pane.reorder(column, direction, window, cx));
                },
            );
        };

        let mut rows = context_menu::grid_copy_rows(&grid, cx);
        let cell = self.menu_cell(cx);
        let source = grid.read(cx).source();
        let writable = source.writable();
        let deleted = cell.is_some_and(|(row, _)| source.row_status(row) == RowStatus::Deleted);
        let inserted = cell.is_some_and(|(row, _)| source.row_status(row) == RowStatus::Inserted);
        // A cell may take a NULL when it may take anything at all *and* the
        // catalog lets the column hold one. Both halves are the source's:
        // this side only asks.
        let nullable = cell.is_some_and(|(row, column)| {
            source.cell_editable(row, column) && source.nullable(column)
        });
        let staged = cell.is_some_and(|(row, _)| source.row_status(row) != RowStatus::Unchanged);

        if cell.is_some_and(|(row, column)| source.uses_value_viewer(row, column)) {
            let label = if cell.is_some_and(|(_, column)| source.lob_kind(column).is_some()) {
                ts!("lob.open")
            } else {
                ts!("lob.open_text")
            };
            let this = this.clone();
            rows.push(MenuRow::separator());
            rows.push(MenuRow::new(label).on_activate(move |window, cx| {
                if let Some((row, column)) = cell {
                    this.update(cx, |pane, cx| pane.open_cell(row, column, window, cx));
                }
            }));
        }

        rows.push(MenuRow::separator());
        rows.push({
            let this = this.clone();
            MenuRow::new(ts!("data.set_null"))
                .shortcut(keys::SET_NULL)
                .enabled(writable && nullable)
                .on_activate(move |_window, cx| {
                    let Some((row, column)) = cell else {
                        return;
                    };
                    this.update(cx, |pane, cx| pane.set_null(row, column, cx));
                })
        });
        rows.push(MenuRow::separator());
        rows.push({
            let this = this.clone();
            MenuRow::new(ts!("data.insert_row"))
                .shortcut(keys::INSERT_ROW)
                .enabled(writable)
                .on_activate(move |window, cx| {
                    this.update(cx, |pane, cx| pane.insert_row(window, cx));
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
                .shortcut(keys::DELETE_ROW)
                .enabled(writable && cell.is_some() && !inserted)
                .on_activate(move |_window, cx| {
                    let Some((row, _)) = cell else {
                        return;
                    };
                    this.update(cx, |pane, cx| pane.toggle_delete(row, cx));
                })
        });
        rows.push(MenuRow::separator());
        rows.push(
            MenuRow::new(ts!("data.discard_row"))
                .enabled(staged)
                .on_activate(move |_window, cx| {
                    let Some((row, _)) = cell else {
                        return;
                    };
                    this.update(cx, |pane, cx| pane.discard_row(row, cx));
                }),
        );
        rows
    }

    /// The grid's right-click menu, while one is open.
    fn render_context_menu(&self, cx: &mut Context<Self>) -> Option<impl IntoElement + use<>> {
        let menu = self.context_menu.as_ref()?;
        let position = menu.position;
        let rows = self.grid_rows(menu.target, cx);
        let this = cx.entity();

        Some(
            ContextMenu::new("data-context")
                .position(position)
                .entries(context_menu::entries(rows))
                .on_dismiss(move |_window, cx| {
                    this.update(cx, |pane, cx| {
                        pane.close_context_menu(cx);
                    });
                }),
        )
    }

    /// How much is staged, or `None` while nothing is.
    fn counts(&self, cx: &App) -> Option<crate::data_edit::EditCounts> {
        let counts = self.grid()?.read(cx).source().edits().counts();
        (counts != crate::data_edit::EditCounts::default()).then_some(counts)
    }

    /// The strip above the rows: what is being shown, how much of it, what is
    /// staged against it, and the three buttons that act on all of that.
    ///
    /// The two editing buttons are drawn only on a pane that can be written to.
    /// A read-only one cannot stage anything, so an "Apply" that could never
    /// light up would be furniture — the banner underneath is where that pane
    /// says what it is.
    fn render_toolbar(&self, chrome: &Theme, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let this = cx.entity();
        let loading = matches!(self.load, Load::Running);
        // "500+ rows" while the table has more than have been paged in: the
        // number is how far the grid has read, and with nothing after it read
        // as the size of the table.
        let more = matches!(
            &self.load,
            Load::Ready(rows) if rows.grid.read(cx).source().state() == GridSourceState::HasMore
        );
        let count = self.row_count(cx).map(|count| {
            if more {
                ts!("data.row_count_more", count = count)
            } else {
                ts!("data.row_count", count = count)
            }
        });
        let writable = self.read_only_reason().is_none();
        let counts = self.counts(cx);
        let pending = counts.map(|counts| {
            ts!(
                "data.pending",
                changed = counts.changed,
                inserted = counts.inserted,
                deleted = counts.deleted
            )
        });
        let staged = counts.is_some();

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
            .children(count.map(|count| {
                div()
                    .flex_none()
                    .whitespace_nowrap()
                    .text_size(px(11.))
                    .text_color(chrome.text_muted)
                    .child(count)
            }))
            .children(writable.then(|| {
                let this = this.clone();
                // Also in the grid's own menu, and here as well for the one
                // case that menu cannot reach: a table with no rows in it has
                // no cell to right-click, and a table nobody can put the first
                // row into would be a strange thing to ship.
                hinted(
                    "data-insert-hint",
                    keys::INSERT_ROW,
                    Button::new("data-insert", ts!("data.insert_row"))
                        .compact()
                        .variant(ButtonVariant::Secondary)
                        .disabled(loading)
                        .on_click(move |_, window, cx| {
                            this.update(cx, |pane, cx| pane.insert_row(window, cx));
                        }),
                )
            }))
            .children(writable.then(|| {
                let this = this.clone();
                hinted(
                    "data-apply-hint",
                    keys::APPLY,
                    Button::new("data-apply", ts!("data.apply"))
                        .compact()
                        .variant(ButtonVariant::Primary)
                        .disabled(!staged || loading || self.applying)
                        .on_click(move |_, window, cx| {
                            this.update(cx, |pane, cx| pane.apply(window, cx));
                        }),
                )
            }))
            .children(writable.then(|| {
                let this = this.clone();
                Button::new("data-discard", ts!("data.discard"))
                    .compact()
                    .variant(ButtonVariant::Secondary)
                    .disabled(!staged || loading || self.applying)
                    .on_click(move |_, _window, cx| {
                        this.update(cx, |pane, cx| {
                            pane.confirm_discard = true;
                            pane.preview = None;
                            cx.notify();
                        });
                    })
            }))
            .child(hinted(
                "data-refresh-hint",
                keys::REFRESH,
                Button::new("data-refresh", ts!("data.refresh"))
                    .compact()
                    .variant(ButtonVariant::Secondary)
                    .disabled(loading || self.applying)
                    .on_click(move |_, window, cx| {
                        this.update(cx, |pane, cx| pane.refresh(window, cx));
                    }),
            ))
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

    /// "Cancel" under a load: stops the statement and stops waiting for it.
    ///
    /// The answer is dropped by the generation, whichever way it ends — the
    /// rows the driver read before the cancel took are not worth showing half
    /// of — and the pane says the load was canceled, with "Refresh" beside it.
    fn cancel_load(&mut self, cx: &mut Context<Self>) {
        let Some(canceller) = self.canceller.take() else {
            return;
        };
        if !matches!(self.load, Load::Running) {
            return;
        }
        self.generation += 1;
        cx.background_spawn(async move {
            if let Err(error) = canceller.cancel() {
                log::warn!("canceling the data load failed: {error}");
            }
        })
        .detach();
        self.load = Load::Failed(Box::new(QueryError {
            message: ts!("data.load_canceled"),
            sql_state: None,
            kind: rudbman_jdbc::BridgeErrorKind::Interrupted,
        }));
        cx.notify();
    }

    /// The rows, or the one line that stands in for them.
    fn render_body(&self, chrome: &Theme, cx: &mut Context<Self>) -> AnyElement {
        match &self.load {
            Load::Running => {
                let this = cx.entity();
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .items_center()
                    .justify_center()
                    .gap(px(10.))
                    .child(note(ts!("data.loading"), chrome.text_muted))
                    .child(
                        Button::new("data-cancel-load", ts!("common.cancel"))
                            .compact()
                            .variant(ButtonVariant::Secondary)
                            .on_click(move |_, _window, cx| {
                                this.update(cx, |pane, cx| pane.cancel_load(cx));
                            }),
                    )
                    .into_any_element()
            }
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
        self.notice_clock
            .watch(self.notice.as_ref(), cx, |pane, shown| {
                let still = pane.notice.as_ref() == Some(shown);
                if still {
                    pane.notice = None;
                }
                still
            });
        let notice = self.notice.as_ref().map(|notice| {
            let this = cx.entity();
            render_notice(
                ("data-notice", cx.entity_id()),
                notice,
                &chrome,
                move |_, cx| {
                    this.update(cx, |pane, cx| {
                        pane.notice = None;
                        cx.notify();
                    });
                },
            )
        });
        let failure = self
            .apply_error
            .as_ref()
            .map(|problem| render_apply_error(problem, &chrome));
        let body = self.render_body(&chrome, cx);
        let menu = self.render_context_menu(cx);
        // Both modals are `row_apply`'s; what each button does is this pane's,
        // and it is said here rather than threaded through as an entity.
        let confirm = self
            .confirm_discard
            .then(|| self.counts(cx))
            .flatten()
            .map(|counts| {
                let this = cx.entity();
                let discard = this.clone();
                render_discard_confirm(
                    counts,
                    &self.confirm_focus,
                    cx,
                    move |_window, cx| {
                        this.update(cx, |pane, cx| {
                            pane.confirm_discard = false;
                            cx.notify();
                        });
                    },
                    move |_window, cx| {
                        discard.update(cx, |pane, cx| pane.discard_all(cx));
                    },
                )
            });
        let preview = self.preview.as_ref().map(|statements| {
            let this = cx.entity();
            let run = this.clone();
            render_apply_preview(
                statements,
                self.transactional,
                &self.confirm_focus,
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
            .id("data-pane")
            .key_context(KEY_CONTEXT)
            .on_action(cx.listener(|pane, _: &SetNullCell, _window, cx| {
                pane.key_set_null(cx);
            }))
            .on_action(cx.listener(|pane, _: &InsertRow, window, cx| {
                if pane.read_only_reason().is_none() && !matches!(pane.load, Load::Running) {
                    pane.insert_row(window, cx);
                }
            }))
            .on_action(cx.listener(|pane, _: &ApplyChanges, window, cx| {
                if pane.has_pending_edits(cx) && !pane.applying {
                    pane.apply(window, cx);
                }
            }))
            .on_action(cx.listener(|pane, _: &RefreshRows, window, cx| {
                if !matches!(pane.load, Load::Running) && !pane.applying {
                    pane.refresh(window, cx);
                }
            }))
            .track_focus(&self.focus_handle)
            .flex()
            .flex_col()
            .size_full()
            .min_w_0()
            .min_h_0()
            .relative()
            .child(toolbar)
            .children(notice)
            .children(banner)
            .children(failure)
            .child(body)
            // All last, and the menu last of all: a context menu paints above
            // even a modal (architecture document, §7.8). The two modals never
            // stand at once — raising either puts the other away — so their
            // order between themselves decides nothing.
            .children(confirm)
            .children(preview)
            .children(menu)
            .children(self.lob_dialog.clone())
    }
}

/// Reads one table's key and its first batch of rows.
///
/// **Blocks**, and is called from `cx.background_spawn` with a
/// [`SessionHandle`]. Both halves go through the session's own worker thread,
/// so they queue behind whatever else that connection is doing rather than
/// racing it.
///
/// The key first, for two reasons: the grid marks its columns and the source
/// has to be told before it is built, and a statement paged by offset orders
/// by it, so it is part of what the first page's statement says.
fn open(session: &Session, target: &ObjectTarget, select: Select) -> Result<Opened, JdbcError> {
    let keys = primary_key(
        session,
        target.catalog.as_deref(),
        target.schema.as_deref(),
        &target.name,
    )?;
    let sql = select.statement(&keys, 0);

    if select.pages_by_offset() {
        let limit = select.limit();
        return Ok(match read_page(session, &sql, limit)? {
            Some(ReadPage {
                columns,
                batch,
                rows,
            }) => {
                let complete = rows < limit as usize;
                Opened {
                    keys,
                    columns,
                    batch: Some(batch),
                    complete,
                    more: (!complete).then_some(More::Offset(rows as u64)),
                    select,
                }
            }
            None => Opened {
                keys,
                columns: Vec::new(),
                batch: None,
                complete: true,
                more: None,
                select,
            },
        });
    }

    // The cursor stays open between scrolls on the session every pane shares,
    // so on SQL Server it has to be a server cursor: a default result set
    // there is read whole into the bridge heap as soon as anything else runs.
    // This statement is the plain `SELECT` written above, which a cursor
    // always accepts.
    let fetch_rows = select.fetch_rows;
    let spec = StatementSpec::new(sql)
        .with_fetch_size(fetch_rows)
        .with_server_cursor(select.dialect.reads_by_server_cursor());
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
            more: pageable.then_some(More::Cursor(cursor)),
            select,
        },
        None => Opened {
            keys,
            columns: Vec::new(),
            batch: None,
            complete: true,
            more: None,
            select,
        },
    })
}

/// Reads the page of a [`Select`] that starts `offset` rows in.
///
/// **Blocks**, like [`open`], and for the same reason runs on a background
/// task. The statement is a new one, written with the key the first page was
/// ordered by, so the rows it returns are the ones that follow the rows
/// already on screen.
fn next_page(
    session: &Session,
    select: &Select,
    keys: &[String],
    offset: u64,
) -> Result<Page, JdbcError> {
    let limit = select.limit();
    let sql = select.statement(keys, offset);
    Ok(match read_page(session, &sql, limit)? {
        Some(ReadPage { batch, rows, .. }) => {
            let complete = rows < limit as usize;
            Page {
                batch,
                complete,
                more: (!complete).then_some(More::Offset(offset + rows as u64)),
            }
        }
        // A `SELECT` that answered with no result set at all, which the first
        // page of the same statement did not do. There is nothing to read on
        // from.
        None => Page {
            batch: RenderedBatch::default(),
            complete: true,
            more: None,
        },
    })
}

#[cfg(test)]
mod tests {
    use gpui::{TestAppContext, WindowHandle};
    use rugpui::TextInput;
    use rugpui_grid::GridCell;

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
            rugpui::init(cx);
            rugpui_editor::init(cx);
            rugpui_grid::init(cx);
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
            .update(cx, |pane, window, cx| pane.refresh(window, cx))
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
                        GridCell::Default => "DEFAULT".to_string(),
                        GridCell::Lob { size, .. } => format!("lob {size:?}"),
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
    /// by the dialect, and the ordering is a clause of the pane's own — with
    /// the key behind it and a page clause after it, since H2 pages by offset.
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
                assert_eq!(
                    pane.select().statement(&pane.keys, 0),
                    "SELECT * FROM APP.\"ORDER\" ORDER BY ID ASC LIMIT 500 OFFSET 0"
                );
                pane.order = Some(Order {
                    column: 0,
                    name: "ID".to_string(),
                    direction: SortDirection::Descending,
                });
                // The user's column is the key, so the key is not said twice.
                assert_eq!(
                    pane.select().statement(&pane.keys, 1_000),
                    "SELECT * FROM APP.\"ORDER\" ORDER BY ID DESC LIMIT 500 OFFSET 1000"
                );
            })
            .expect("the window is open");
        connected.close().expect("close");
    }

    /// The order a paged statement is written in: the user's column first, the
    /// key behind it as a tie breaker, and nothing at all for a keyless table
    /// nobody sorted.
    #[test]
    fn a_paged_statement_is_ordered_by_the_key_behind_the_users_column() {
        let select = |order: Option<(&str, SortDirection)>| Select {
            dialect: Dialect::H2,
            table: "APP.T".to_string(),
            order: order.map(|(name, direction)| (name.to_string(), direction)),
            fetch_rows: 100,
        };
        let keys = ["A".to_string(), "B".to_string()];

        assert_eq!(
            select(None).statement(&keys, 0),
            "SELECT * FROM APP.T ORDER BY A ASC, B ASC LIMIT 100 OFFSET 0"
        );
        assert_eq!(
            select(Some(("PRICE", SortDirection::Descending))).statement(&keys, 200),
            "SELECT * FROM APP.T ORDER BY PRICE DESC, A ASC, B ASC LIMIT 100 OFFSET 200"
        );
        assert_eq!(
            select(Some(("B", SortDirection::Descending))).statement(&keys, 0),
            "SELECT * FROM APP.T ORDER BY B DESC, A ASC LIMIT 100 OFFSET 0",
            "a key column the user sorted by is not repeated"
        );
        assert_eq!(
            select(None).statement(&[], 300),
            "SELECT * FROM APP.T LIMIT 100 OFFSET 300"
        );
        assert_eq!(
            select(Some(("PRICE", SortDirection::Ascending))).statement(&[], 0),
            "SELECT * FROM APP.T ORDER BY PRICE ASC LIMIT 100 OFFSET 0"
        );
        assert!(select(None).pages_by_offset());
    }

    /// A dialect with no page clause keeps the one statement the cursor is
    /// opened on, exactly as it was written before pages were statements.
    #[test]
    fn a_dialect_without_a_page_clause_keeps_its_cursor_statement() {
        for dialect in [Dialect::ORACLE, Dialect::MSSQL, Dialect::GENERIC] {
            let select = Select {
                dialect,
                table: "T".to_string(),
                order: Some(("PRICE".to_string(), SortDirection::Descending)),
                fetch_rows: 100,
            };
            assert!(!select.pages_by_offset(), "{}", dialect.name());
            assert_eq!(
                select.statement(&["ID".to_string()], 500),
                "SELECT * FROM T ORDER BY PRICE DESC",
                "{}",
                dialect.name()
            );
        }
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
            .update(cx, |pane, window, cx| {
                pane.reorder(0, Some(SortDirection::Descending), window, cx);
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

    /// Scrolls to the end the way `GridEvent::NearEnd` would, a page at a
    /// time, and answers the first column of every row.
    ///
    /// Between pages — whenever the pane is idle — nothing may be held open:
    /// the way on is the offset of the next page, and that offset is exactly
    /// the rows already fetched. A cursor there would be a result set left
    /// standing on the shared connection, which is what paging by statement
    /// exists to avoid.
    fn scroll_to_end(window: &WindowHandle<DataPane>, cx: &mut TestAppContext) -> Vec<String> {
        let mut pages = 0;
        loop {
            let complete = window
                .update(cx, |pane, _window, cx| {
                    let Load::Ready(rows) = &pane.load else {
                        panic!("the pane holds no rows: {:?}", pane.failure());
                    };
                    assert!(!rows.fetching, "a page is still out while idle");
                    let fetched = rows.grid.read(cx).source().base_rows() as u64;
                    match &rows.more {
                        Some(More::Offset(next)) => assert_eq!(
                            *next, fetched,
                            "the next page does not start after the rows on screen"
                        ),
                        Some(More::Cursor(_)) => {
                            panic!("a cursor was left open between pages")
                        }
                        None => {}
                    }
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
            assert!(pages < 100, "the source never reached the end");
        }
        window
            .update(cx, |pane, _window, _cx| {
                assert!(pane.notice.is_none(), "paging said {:?}", pane.notice);
            })
            .expect("the window is open");
        column(window, 0, cx)
    }

    /// Small pages over a keyed table come back in key order, every row once:
    /// each page is a statement of its own, ordered by the key, so the pages
    /// line up end to end.
    #[gpui::test]
    fn small_pages_read_a_keyed_table_in_key_order_without_gaps(cx: &mut TestAppContext) {
        let (connected, profile) = h2(
            "data-small-pages",
            &[
                "create schema if not exists APP",
                "create table APP.BIG (ID int primary key, NAME varchar(20))",
                // Inserted out of key order, so that the order on screen is the
                // statement's and not an accident of how the rows were stored.
                "insert into APP.BIG select 2501 - X, 'row' from system_range(1, 2500)",
            ],
        );
        let window = pane(&connected, &profile, target("BIG"), 100, cx);
        window
            .update(cx, |pane, _window, cx| {
                // A page, or the few the window's own first frames asked for,
                // and not the table.
                let loaded = pane.row_count(cx).expect("the rows arrived");
                assert!(loaded < 2_500 && loaded % 100 == 0, "{loaded} rows");
                assert_eq!(pane.state(cx), GridSourceState::HasMore);
            })
            .expect("the window is open");

        let rows = scroll_to_end(&window, cx);
        let expected: Vec<String> = (1..=2_500).map(|id| id.to_string()).collect();
        assert_eq!(rows, expected);
        window
            .update(cx, |pane, _window, _cx| {
                let Load::Ready(rows) = &pane.load else {
                    panic!("the rows arrived");
                };
                assert!(rows.more.is_none(), "the end still has a way on");
            })
            .expect("the window is open");
        connected.close().expect("close");
    }

    /// A descending sort pages too, and a sort on a column full of ties still
    /// lines its pages up, because the key breaks every tie the same way on
    /// every page.
    #[gpui::test]
    fn a_sorted_table_pages_in_the_order_asked_for(cx: &mut TestAppContext) {
        let (connected, profile) = h2(
            "data-sorted-pages",
            &[
                "create schema if not exists APP",
                "create table APP.BIG (ID int primary key, GRP int)",
                "insert into APP.BIG select X, mod(X, 7) from system_range(1, 2500)",
            ],
        );
        let window = pane(&connected, &profile, target("BIG"), 100, cx);

        window
            .update(cx, |pane, window, cx| {
                pane.reorder(0, Some(SortDirection::Descending), window, cx);
            })
            .expect("the window is open");
        cx.run_until_parked();
        let rows = scroll_to_end(&window, cx);
        let expected: Vec<String> = (1..=2_500).rev().map(|id| id.to_string()).collect();
        assert_eq!(rows, expected);

        window
            .update(cx, |pane, window, cx| {
                pane.reorder(1, Some(SortDirection::Descending), window, cx);
            })
            .expect("the window is open");
        cx.run_until_parked();
        let ids = scroll_to_end(&window, cx);
        let groups = column(&window, 1, cx);
        let pairs: Vec<(i64, i64)> = groups
            .iter()
            .zip(&ids)
            .map(|(group, id)| (group.parse().unwrap(), id.parse().unwrap()))
            .collect();
        let mut expected: Vec<(i64, i64)> = (1..=2_500).map(|id| (id % 7, id)).collect();
        // The group descending, and the key ascending inside each group.
        expected.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
        assert_eq!(pairs, expected);
        connected.close().expect("close");
    }

    /// A table with no key has nothing to order its pages by, and is still
    /// read whole: every row, once.
    #[gpui::test]
    fn a_table_without_a_key_is_paged_whole(cx: &mut TestAppContext) {
        let (connected, profile) = h2(
            "data-keyless-pages",
            &[
                "create schema if not exists APP",
                "create table APP.HEAP (V int)",
                "insert into APP.HEAP select X from system_range(1, 2500)",
            ],
        );
        let window = pane(&connected, &profile, target("HEAP"), 100, cx);
        window
            .update(cx, |pane, _window, _cx| assert!(pane.keys.is_empty()))
            .expect("the window is open");

        let mut values: Vec<i64> = scroll_to_end(&window, cx)
            .iter()
            .map(|value| value.parse().unwrap())
            .collect();
        values.sort_unstable();
        assert_eq!(values, (1..=2_500).collect::<Vec<i64>>());
        connected.close().expect("close");
    }

    /// A table that is an exact number of pages long ends on an empty page
    /// rather than asking forever.
    #[gpui::test]
    fn a_table_of_whole_pages_ends(cx: &mut TestAppContext) {
        let (connected, profile) = h2(
            "data-whole-pages",
            &[
                "create schema if not exists APP",
                "create table APP.EVEN (ID int primary key)",
                "insert into APP.EVEN select X from system_range(1, 300)",
            ],
        );
        let window = pane(&connected, &profile, target("EVEN"), 100, cx);
        let rows = scroll_to_end(&window, cx);
        assert_eq!(rows.len(), 300);
        assert_eq!(rows.last().map(String::as_str), Some("300"));
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
            .update(cx, |pane, window, cx| {
                pane.detach(cx);
                assert!(!pane.is_attached());

                pane.fetch_more(cx);
                assert_eq!(
                    pane.notice.as_ref().map(|notice| notice.text.clone()),
                    Some(ts!("explorer.disconnected"))
                );

                pane.notice = None;
                pane.reorder(0, Some(SortDirection::Ascending), window, cx);
                assert_eq!(
                    pane.notice.as_ref().map(|notice| notice.text.clone()),
                    Some(ts!("explorer.disconnected"))
                );

                pane.notice = None;
                pane.refresh(window, cx);
                assert_eq!(
                    pane.notice.as_ref().map(|notice| notice.text.clone()),
                    Some(ts!("explorer.disconnected"))
                );
            })
            .expect("the window is open");
        cx.run_until_parked();

        // The rows the live load produced are still there to read.
        assert_eq!(column(&window, 0, cx), ["1", "2"]);
        connected.close().expect("close");
    }

    /// Opens the field over a cell, puts `text` in it and closes it — which is
    /// the whole gesture, and the only route by which anything is staged.
    ///
    /// Answers whether the field opened at all, so that the refusals can be
    /// asserted through the same helper the successes go through.
    fn type_into(
        window: &WindowHandle<DataPane>,
        row: usize,
        column: usize,
        text: &str,
        cx: &mut TestAppContext,
    ) -> bool {
        let opened = window
            .update(cx, |pane, window, cx| {
                let grid = pane.grid().cloned().expect("the pane holds rows");
                grid.update(cx, |grid, cx| {
                    if !grid.begin_edit(row, column, window, cx) {
                        return false;
                    }
                    let input = grid.editor().cloned().expect("the field is open");
                    input.update(cx, |input: &mut TextInput, cx| {
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

    /// Puts the caret on a cell, which is where the right-click menu reads the
    /// row and column it acts on from.
    fn select(window: &WindowHandle<DataPane>, row: usize, column: usize, cx: &mut TestAppContext) {
        window
            .update(cx, |pane, _window, cx| {
                let grid = pane.grid().cloned().expect("the pane holds rows");
                grid.update(cx, |grid, cx| grid.select_cell(row, column, cx));
            })
            .expect("the window is open");
    }

    /// The pane's cell menu, over whatever the caret is on.
    fn cell_menu(window: &WindowHandle<DataPane>, cx: &mut TestAppContext) -> Vec<MenuRow> {
        window
            .update(cx, |pane, _window, cx| pane.grid_rows(MenuTarget::Cell, cx))
            .expect("the window is open")
    }

    /// Runs the cell menu's row labeled `label`, as clicking it would.
    ///
    /// From outside the pane's own update, because that is where the click
    /// happens: every row's callback reaches back into the pane, and running
    /// one from inside a borrow of it would be a re-entry the interface cannot
    /// produce.
    fn run(window: &WindowHandle<DataPane>, label: &str, cx: &mut gpui::VisualTestContext) {
        let rows = cell_menu(window, cx);
        cx.update(|window, cx| context_menu::row(&rows, label).activate(window, cx));
    }

    /// How a row is marked.
    fn status(window: &WindowHandle<DataPane>, row: usize, cx: &mut TestAppContext) -> RowStatus {
        window
            .update(cx, |pane, _window, cx| {
                pane.grid()
                    .expect("the pane holds rows")
                    .read(cx)
                    .source()
                    .row_status(row)
            })
            .expect("the window is open")
    }

    /// A table with two rows to type into, one NULL and one empty string among
    /// them so that the two can be told apart in every assertion below.
    fn person(name: &str) -> (Connected, ConnectionProfile) {
        h2(
            name,
            &[
                "create schema if not exists APP",
                "create table APP.PERSON (ID int primary key, NAME varchar(20), NOTE varchar(20))",
                "insert into APP.PERSON values (1, 'a', null), (2, 'b', '')",
            ],
        )
    }

    /// The grid answers the editing commands from the keyboard: `Delete`
    /// strikes the row under the caret out and back, `Ctrl+0` puts NULL in its
    /// cell, `Alt+Insert` adds a row, and `Ctrl+S` raises the apply preview.
    #[gpui::test]
    fn the_editing_commands_answer_the_keyboard(cx: &mut TestAppContext) {
        let (connected, profile) = person("data-keys");
        let window = pane(&connected, &profile, target("PERSON"), 500, cx);
        let mut visual = gpui::VisualTestContext::from_window(window.into(), cx);
        window
            .update(&mut visual, |pane, window, cx| pane.take_focus(window, cx))
            .expect("the window is open");
        visual.run_until_parked();
        let modifier = if cfg!(target_os = "macos") {
            "cmd"
        } else {
            "ctrl"
        };

        select(&window, 0, 1, &mut visual);
        visual.simulate_keystrokes("delete");
        visual.run_until_parked();
        assert_eq!(status(&window, 0, &mut visual), RowStatus::Deleted);
        visual.simulate_keystrokes("delete");
        visual.run_until_parked();
        assert_eq!(status(&window, 0, &mut visual), RowStatus::Unchanged);

        select(&window, 1, 1, &mut visual);
        visual.simulate_keystrokes(&format!("{modifier}-0"));
        visual.run_until_parked();
        assert_eq!(status(&window, 1, &mut visual), RowStatus::Modified);

        // A new row goes in at the top, where its first field opens.
        visual.simulate_keystrokes("alt-insert");
        visual.run_until_parked();
        assert_eq!(status(&window, 0, &mut visual), RowStatus::Inserted);

        // Back on the grid, where the new row's field took the keyboard.
        window
            .update(&mut visual, |pane, window, cx| pane.take_focus(window, cx))
            .expect("the window is open");
        visual.run_until_parked();
        visual.simulate_keystrokes(&format!("{modifier}-s"));
        visual.run_until_parked();
        window
            .update(&mut visual, |pane, _window, _cx| {
                assert!(pane.preview.is_some(), "the apply preview was raised");
            })
            .expect("the window is open");
        connected.close().expect("close");
    }

    /// The grid's key requests reach the pane: `Delete` over two rows marks
    /// both, a typed character opens the cell holding it, and `Ctrl+V` stages
    /// a tab-separated block from the cursor on.
    #[gpui::test]
    fn the_grid_asks_the_pane_to_delete_type_and_paste(cx: &mut TestAppContext) {
        let (connected, profile) = person("data-requests");
        let window = pane(&connected, &profile, target("PERSON"), 500, cx);
        let mut visual = gpui::VisualTestContext::from_window(window.into(), cx);
        window
            .update(&mut visual, |pane, window, cx| pane.take_focus(window, cx))
            .expect("the window is open");
        visual.run_until_parked();
        let grid = window
            .update(&mut visual, |pane, _window, _cx| {
                pane.grid().cloned().expect("the pane holds rows")
            })
            .expect("the window is open");

        select(&window, 0, 1, &mut visual);
        grid.update(&mut visual, |grid, cx| {
            grid.extend_selection(1, 1, cx);
        });
        visual.simulate_keystrokes("delete");
        visual.run_until_parked();
        assert_eq!(status(&window, 0, &mut visual), RowStatus::Deleted);
        assert_eq!(status(&window, 1, &mut visual), RowStatus::Deleted);
        visual.simulate_keystrokes("delete");
        visual.run_until_parked();
        assert_eq!(status(&window, 0, &mut visual), RowStatus::Unchanged);
        assert_eq!(status(&window, 1, &mut visual), RowStatus::Unchanged);

        select(&window, 0, 1, &mut visual);
        visual.simulate_keystrokes("q");
        visual.run_until_parked();
        grid.update(&mut visual, |grid, cx| {
            assert_eq!(grid.editing(), Some((0, 1)), "typing opened the cell");
            grid.cancel_edit(cx);
        });

        let modifier = if cfg!(target_os = "macos") {
            "cmd"
        } else {
            "ctrl"
        };
        visual.write_to_clipboard(gpui::ClipboardItem::new_string("x\ty\n".into()));
        select(&window, 1, 1, &mut visual);
        visual.simulate_keystrokes(&format!("{modifier}-v"));
        visual.run_until_parked();
        grid.update(&mut visual, |grid, _cx| {
            let edits = grid.source().edits();
            assert_eq!(edits.staged(1, 1), Some(&StagedCell::Text("x".into())));
            assert_eq!(edits.staged(1, 2), Some(&StagedCell::Text("y".into())));
        });
        connected.close().expect("close");
    }

    /// "Cancel" under a load stops waiting for it at once: the pane says the
    /// load was canceled, and a late answer is dropped by the generation.
    #[gpui::test]
    fn a_canceled_load_says_so_and_drops_its_answer(cx: &mut TestAppContext) {
        let (connected, profile) = person("data-cancel");
        let window = pane(&connected, &profile, target("PERSON"), 500, cx);
        let canceller = connected.session().canceller();
        let mut visual = gpui::VisualTestContext::from_window(window.into(), cx);
        window
            .update(&mut visual, |pane, _window, cx| {
                // Standing in for a load in flight.
                pane.load = Load::Running;
                pane.canceller = Some(canceller);
                cx.notify();
            })
            .expect("the window is open");
        // Drawn with its Cancel.
        visual.run_until_parked();
        window
            .update(&mut visual, |pane, _window, cx| {
                let generation = pane.generation;
                pane.cancel_load(cx);
                assert_ne!(pane.generation, generation, "a late answer would be taken");
                let Load::Failed(error) = &pane.load else {
                    panic!("the pane is still loading");
                };
                assert!(error.is_canceled());
                assert_eq!(error.message, ts!("data.load_canceled"));
            })
            .expect("the window is open");
        visual.run_until_parked();
        connected.close().expect("close");
    }

    /// Typing into a cell stages it, marks the row, and says so in the toolbar
    /// — and typing the old value back in takes all of that away again.
    #[gpui::test]
    fn a_committed_field_is_staged_and_marks_its_row(cx: &mut TestAppContext) {
        let (connected, profile) = person("data-stage");
        let window = pane(&connected, &profile, target("PERSON"), 500, cx);

        assert!(type_into(&window, 0, 1, "A", cx), "the field refused");
        assert_eq!(column(&window, 1, cx), ["A", "b"], "the overlay is on top");
        assert_eq!(status(&window, 0, cx), RowStatus::Modified);
        assert_eq!(status(&window, 1, cx), RowStatus::Unchanged);
        window
            .update(cx, |pane, _window, cx| {
                assert!(pane.has_pending_edits(cx));
                let counts = pane.counts(cx).expect("something is staged");
                assert_eq!(counts.changed, 1);
                assert_eq!(counts.inserted, 0);
                assert_eq!(counts.deleted, 0);
                assert_eq!(
                    pane.row_count(cx),
                    Some(2),
                    "the toolbar counts the table's rows"
                );
            })
            .expect("the window is open");

        // A to B to A. The grid raises nothing at all for a field left as it
        // was found, so the second edit has to be a real one that happens to
        // spell what the server gave.
        assert!(type_into(&window, 0, 1, "a", cx));
        window
            .update(cx, |pane, _window, cx| {
                assert!(!pane.has_pending_edits(cx), "the round trip left a change");
                assert!(pane.counts(cx).is_none());
            })
            .expect("the window is open");
        assert_eq!(status(&window, 0, cx), RowStatus::Unchanged);
        connected.close().expect("close");
    }

    /// NULL is a command rather than an empty field, and running it on a cell
    /// that is already NULL stages nothing.
    #[gpui::test]
    fn setting_null_is_a_command_and_a_null_cell_is_left_alone(cx: &mut TestAppContext) {
        let (connected, profile) = person("data-null");
        let window = pane(&connected, &profile, target("PERSON"), 500, cx);
        // Row 0's NOTE is NULL, row 1's is the empty string. Neither is the
        // other, here or anywhere else.
        assert_eq!(column(&window, 2, cx), ["NULL", ""]);
        let mut cx = gpui::VisualTestContext::from_window(window.into(), cx);

        select(&window, 0, 2, &mut cx);
        run(&window, &ts!("data.set_null"), &mut cx);
        window
            .update(&mut cx, |pane, _window, cx| {
                assert!(
                    !pane.has_pending_edits(cx),
                    "NULL over NULL was staged as a change"
                );
            })
            .expect("the window is open");

        // The empty string is a value, so clearing it is a change — and it
        // becomes the marker rather than staying empty.
        select(&window, 1, 2, &mut cx);
        run(&window, &ts!("data.set_null"), &mut cx);
        window
            .update(&mut cx, |pane, _window, cx| {
                assert!(pane.has_pending_edits(cx));
            })
            .expect("the window is open");
        assert_eq!(column(&window, 2, &mut cx), ["NULL", "NULL"]);
        assert_eq!(status(&window, 1, &mut cx), RowStatus::Modified);
        connected.close().expect("close");
    }

    /// Deleting keeps the row where it is, flips the label that would put it
    /// back, and takes the row's cells out of reach while it stands.
    #[gpui::test]
    fn a_deleted_row_stays_on_screen_and_can_be_put_back(cx: &mut TestAppContext) {
        let (connected, profile) = person("data-delete");
        let window = pane(&connected, &profile, target("PERSON"), 500, cx);
        let mut cx = gpui::VisualTestContext::from_window(window.into(), cx);

        select(&window, 0, 0, &mut cx);
        run(&window, &ts!("data.delete_row"), &mut cx);

        assert_eq!(status(&window, 0, &mut cx), RowStatus::Deleted);
        assert_eq!(
            column(&window, 0, &mut cx),
            ["1", "2"],
            "still in its place"
        );
        assert!(
            !type_into(&window, 0, 1, "A", &mut cx),
            "a row on its way out took an edit"
        );

        // The label is the command, so it now says the opposite.
        let rows = cell_menu(&window, &mut cx);
        assert!(!context_menu::labels(&rows).contains(&ts!("data.delete_row").to_string()));
        run(&window, &ts!("data.undelete_row"), &mut cx);
        window
            .update(&mut cx, |pane, _window, cx| {
                assert!(!pane.has_pending_edits(cx));
            })
            .expect("the window is open");
        assert_eq!(status(&window, 0, &mut cx), RowStatus::Unchanged);
        connected.close().expect("close");
    }

    fn next_frame(window: WindowHandle<DataPane>, cx: &mut TestAppContext) {
        cx.update_window(window.into(), |_, window, cx| {
            window.simulate_next_frame(cx);
        })
        .unwrap();
        cx.run_until_parked();
    }

    /// A new row appears above the fetched rows, is scrolled to, and is already
    /// taking a value — with the columns nobody typed into left to the server.
    #[gpui::test]
    fn an_inserted_row_appears_at_the_top_and_opens_a_field(cx: &mut TestAppContext) {
        let (connected, profile) = person("data-insert");
        let window = pane(&connected, &profile, target("PERSON"), 500, cx);

        window
            .update(cx, |pane, window, cx| {
                pane.insert_row(window, cx);
            })
            .expect("the window is open");
        cx.run_until_parked();
        next_frame(window, cx);
        window
            .update(cx, |pane, _window, cx| {
                let grid = pane.grid().expect("the pane holds rows").read(cx);
                assert_eq!(grid.source().row_count(), 3, "the row is not there");
                assert_eq!(
                    grid.editing(),
                    Some((0, 0)),
                    "the field did not open on the new row's first column"
                );
            })
            .expect("the window is open");

        assert_eq!(status(&window, 0, cx), RowStatus::Inserted);
        // Every column of it is the server's until something is typed in, and
        // that is not the same thing as NULL.
        assert_eq!(column(&window, 0, cx), ["DEFAULT", "1", "2"]);
        assert_eq!(column(&window, 2, cx), ["DEFAULT", "NULL", ""]);

        assert!(type_into(&window, 0, 1, "c", cx));
        assert_eq!(column(&window, 1, cx), ["c", "a", "b"]);
        assert_eq!(
            column(&window, 0, cx),
            ["DEFAULT", "1", "2"],
            "an untyped column is still the server's"
        );
        window
            .update(cx, |pane, _window, cx| {
                let counts = pane.counts(cx).expect("something is staged");
                assert_eq!(counts.inserted, 1);
                assert_eq!(counts.changed, 0, "a new row is not a changed one");
                assert_eq!(
                    pane.row_count(cx),
                    Some(2),
                    "a row nobody has sent yet is not a row of the table"
                );
            })
            .expect("the window is open");

        // Discarding an inserted row takes the row itself: it *is* the change.
        let mut cx = gpui::VisualTestContext::from_window(window.into(), cx);
        select(&window, 0, 0, &mut cx);
        let rows = cell_menu(&window, &mut cx);
        assert!(
            !context_menu::row(&rows, &ts!("data.delete_row")).is_enabled(),
            "a row the server has never seen was offered a DELETE"
        );
        run(&window, &ts!("data.discard_row"), &mut cx);
        window
            .update(&mut cx, |pane, _window, cx| {
                assert!(!pane.has_pending_edits(cx));
            })
            .expect("the window is open");
        assert_eq!(column(&window, 1, &mut cx), ["a", "b"]);
        connected.close().expect("close");
    }

    #[gpui::test]
    fn a_new_row_is_visible_above_a_large_paged_table(cx: &mut TestAppContext) {
        let (connected, profile) = h2(
            "data-insert-paging",
            &[
                "create schema if not exists APP",
                "create table APP.BIG (ID int auto_increment primary key, NAME varchar(20))",
                "insert into APP.BIG (NAME) select 'row' from system_range(1, 2500)",
            ],
        );
        let window = pane(&connected, &profile, target("BIG"), 1_000, cx);
        let mut cx = gpui::VisualTestContext::from_window(window.into(), cx);
        window
            .update(&mut cx, |pane, _window, cx| {
                pane.grid()
                    .unwrap()
                    .update(cx, |grid, cx| grid.scroll_to_row(900, cx));
            })
            .unwrap();
        cx.run_until_parked();

        let loaded = window
            .update(&mut cx, |pane, _window, cx| pane.row_count(cx).unwrap())
            .unwrap();
        assert!(
            loaded < 2_500,
            "the test must still have another page to fetch"
        );
        window
            .update(&mut cx, |pane, window, cx| pane.insert_row(window, cx))
            .unwrap();
        cx.run_until_parked();
        next_frame(window, &mut cx);
        window
            .update(&mut cx, |pane, _window, cx| {
                let grid = pane.grid().unwrap().read(cx);
                assert_eq!(grid.source().base_rows(), loaded);
                assert_eq!(grid.source().row_status(0), RowStatus::Inserted);
                assert!(
                    grid.visible_rows().contains(&0),
                    "the new row is off screen"
                );
                assert_eq!(grid.editing(), Some((0, 1)));
            })
            .unwrap();
        cx.simulate_input("new row");
        // A page already on its way must not reassign the typed value to a
        // fetched row. The insert stays at index zero after the page arrives.
        window
            .update(&mut cx, |pane, _window, cx| pane.fetch_more(cx))
            .unwrap();
        cx.run_until_parked();
        window
            .update(&mut cx, |pane, _window, cx| {
                let grid = pane.grid().unwrap().read(cx);
                assert_eq!(grid.source().base_rows(), (loaded + 1_000).min(2_500));
                assert_eq!(grid.source().row_status(0), RowStatus::Inserted);
                assert_eq!(grid.source().cell(0, 1), GridCell::Text("new row"));
                assert_eq!(grid.source().cell(1, 1), GridCell::Text("row"));
                assert_eq!(grid.source().edits().counts().changed, 0);
            })
            .unwrap();
        assert!(type_into(&window, 0, 1, "still editable", &mut cx));
        connected.close().expect("close");
    }

    #[gpui::test]
    fn prepending_rows_keeps_pending_typing_and_existing_row_keys(cx: &mut TestAppContext) {
        let (connected, profile) = person("data-prepend-edits");
        let window = pane(&connected, &profile, target("PERSON"), 500, cx);
        window
            .update(cx, |pane, window, cx| {
                let grid = pane.grid().cloned().unwrap();
                grid.update(cx, |grid, cx| {
                    assert!(grid.begin_edit(1, 1, window, cx));
                    grid.editor().cloned().unwrap().update(cx, |input, cx| {
                        input.set_content("existing", cx);
                    });
                });
                pane.insert_row(window, cx);
            })
            .unwrap();
        cx.run_until_parked();
        next_frame(window, cx);
        assert_eq!(column(&window, 1, cx), ["DEFAULT", "a", "existing"]);
        assert!(type_into(&window, 0, 0, "3", cx));
        window
            .update(cx, |pane, window, cx| {
                let grid = pane.grid().cloned().unwrap();
                grid.update(cx, |grid, cx| {
                    assert!(grid.begin_edit(0, 1, window, cx));
                    grid.editor().cloned().unwrap().update(cx, |input, cx| {
                        input.set_content("first", cx);
                    });
                });
                pane.insert_row(window, cx);
            })
            .unwrap();
        cx.run_until_parked();
        next_frame(window, cx);
        assert_eq!(
            column(&window, 1, cx),
            ["DEFAULT", "first", "a", "existing"]
        );
        assert!(type_into(&window, 0, 0, "4", cx));
        assert!(type_into(&window, 0, 1, "second", cx));
        window
            .update(cx, |pane, window, cx| {
                pane.toggle_delete(2, cx);
                pane.apply(window, cx);
                pane.confirm_apply(window, cx);
            })
            .unwrap();
        cx.run_until_parked();
        next_frame(window, cx);
        assert_eq!(
            server_rows(&connected, "select NAME from APP.PERSON order by ID"),
            ["existing", "first", "second"]
        );
        connected.close().expect("close");
    }

    #[gpui::test]
    fn discarding_inserted_rows_keeps_pending_edits_on_their_own_rows(cx: &mut TestAppContext) {
        let (connected, profile) = person("data-discard-insert");
        let window = pane(&connected, &profile, target("PERSON"), 500, cx);
        window
            .update(cx, |pane, window, cx| pane.insert_row(window, cx))
            .unwrap();
        cx.run_until_parked();
        next_frame(window, cx);
        window
            .update(cx, |pane, _window, cx| {
                let input = pane.grid().unwrap().read(cx).editor().cloned().unwrap();
                input.update(cx, |input, cx| input.set_content("999", cx));
                pane.discard_row(0, cx);
            })
            .unwrap();
        cx.run_until_parked();
        next_frame(window, cx);
        assert_eq!(column(&window, 0, cx), ["1", "2"]);
        window
            .update(cx, |pane, window, cx| {
                assert!(!pane.has_pending_edits(cx));
                pane.insert_row(window, cx);
            })
            .unwrap();
        window
            .update(cx, |pane, window, cx| pane.insert_row(window, cx))
            .unwrap();
        cx.run_until_parked();
        next_frame(window, cx);
        window
            .update(cx, |pane, window, cx| {
                pane.grid().unwrap().update(cx, |grid, cx| {
                    assert!(grid.begin_edit(3, 1, window, cx));
                    grid.editor().cloned().unwrap().update(cx, |input, cx| {
                        input.set_content("kept", cx);
                    });
                });
                pane.discard_row(1, cx);
            })
            .unwrap();
        cx.run_until_parked();
        next_frame(window, cx);
        assert_eq!(column(&window, 1, cx), ["DEFAULT", "a", "kept"]);
        window
            .update(cx, |pane, window, cx| {
                pane.grid().unwrap().update(cx, |grid, cx| {
                    assert!(grid.begin_edit(0, 1, window, cx));
                    grid.editor().cloned().unwrap().update(cx, |input, cx| {
                        input.set_content("discard me", cx);
                    });
                });
                pane.discard_all(cx);
            })
            .unwrap();
        cx.run_until_parked();
        next_frame(window, cx);
        assert_eq!(column(&window, 1, cx), ["a", "b"]);
        window
            .update(cx, |pane, _window, cx| assert!(!pane.has_pending_edits(cx)))
            .unwrap();
        connected.close().expect("close");
    }

    /// A table with nothing in it can still be typed into.
    ///
    /// The one case the grid's own menu cannot reach — there is no cell to
    /// right-click — which is why the toolbar carries the command too.
    #[gpui::test]
    fn the_first_row_of_an_empty_table_can_be_added(cx: &mut TestAppContext) {
        let (connected, profile) = h2(
            "data-empty-insert",
            &[
                "create schema if not exists APP",
                "create table APP.PERSON (ID int primary key, NAME varchar(20))",
            ],
        );
        let window = pane(&connected, &profile, target("PERSON"), 500, cx);
        window
            .update(cx, |pane, _window, cx| {
                assert_eq!(pane.row_count(cx), Some(0));
                assert!(pane.read_only_reason().is_none());
            })
            .expect("the window is open");

        window
            .update(cx, |pane, window, cx| pane.insert_row(window, cx))
            .expect("the window is open");
        assert_eq!(status(&window, 0, cx), RowStatus::Inserted);
        assert!(type_into(&window, 0, 1, "first", cx));
        assert_eq!(column(&window, 1, cx), ["first"]);
        window
            .update(cx, |pane, _window, cx| {
                assert_eq!(pane.counts(cx).expect("staged").inserted, 1);
                assert_eq!(pane.row_count(cx), Some(0), "the server still has none");
            })
            .expect("the window is open");
        connected.close().expect("close");
    }

    /// A sort and a refresh both throw the rows away, so both refuse while
    /// anything is staged — and both go through once it is discarded.
    #[gpui::test]
    fn a_sort_and_a_refresh_wait_for_the_edits_to_be_dealt_with(cx: &mut TestAppContext) {
        let (connected, profile) = person("data-guard");
        let window = pane(&connected, &profile, target("PERSON"), 500, cx);
        assert!(type_into(&window, 0, 1, "A", cx));

        window
            .update(cx, |pane, window, cx| {
                pane.reorder(0, Some(SortDirection::Descending), window, cx);
                assert_eq!(
                    pane.notice.as_ref().map(|notice| notice.text.clone()),
                    Some(ts!("data.discard_first"))
                );
                assert!(pane.order.is_none(), "the ordering was taken anyway");

                pane.notice = None;
                pane.refresh(window, cx);
                assert_eq!(
                    pane.notice.as_ref().map(|notice| notice.text.clone()),
                    Some(ts!("data.discard_first"))
                );
                assert!(
                    matches!(pane.load, Load::Ready(_)),
                    "the rows were thrown away anyway"
                );
            })
            .expect("the window is open");
        cx.run_until_parked();
        assert_eq!(column(&window, 1, cx), ["A", "b"], "the edit survived");

        // Discarding everything puts the rows back as the server has them and
        // lets the guarded paths through.
        window
            .update(cx, |pane, _window, cx| pane.discard_all(cx))
            .expect("the window is open");
        assert_eq!(column(&window, 1, cx), ["a", "b"]);
        window
            .update(cx, |pane, window, cx| {
                assert!(!pane.has_pending_edits(cx));
                pane.reorder(0, Some(SortDirection::Descending), window, cx);
            })
            .expect("the window is open");
        cx.run_until_parked();
        assert_eq!(column(&window, 0, cx), ["2", "1"], "the sort ran");
        connected.close().expect("close");
    }

    /// Neither of §7.9's two read-only reasons lets a keystroke through, and
    /// the menu that would go round the field is grayed for both.
    #[gpui::test]
    fn a_pane_that_may_not_be_written_to_opens_no_field(cx: &mut TestAppContext) {
        for (name, table, setup, read_only) in [
            (
                "data-ro-nokey",
                "LOG",
                "create table APP.LOG (LINE varchar(20), NOTE varchar(20))",
                false,
            ),
            (
                "data-ro-profile",
                "PERSON",
                "create table APP.PERSON (ID int primary key, NAME varchar(20))",
                true,
            ),
        ] {
            let (connected, mut profile) = h2(name, &["create schema if not exists APP", setup]);
            profile.read_only = read_only;
            let window = pane(&connected, &profile, target(table), 500, cx);

            window
                .update(cx, |pane, _window, _cx| {
                    assert!(pane.read_only_reason().is_some(), "{name} is writable");
                })
                .expect("the window is open");

            // Through the event a double click raises, so that the pane's own
            // handler is what is under test, and then directly — the refusal
            // has to happen before a field opens, not after the user has typed.
            let opened = window
                .update(cx, |pane, window, cx| {
                    let grid = pane.grid().cloned().expect("the pane holds rows");
                    grid.update(cx, |_grid, cx| {
                        cx.emit(GridEvent::CellActivated { row: 0, column: 0 });
                    });
                    grid.update(cx, |grid, cx| grid.begin_edit(0, 0, window, cx))
                })
                .expect("the window is open");
            cx.run_until_parked();
            assert!(!opened, "{name} opened a field");
            window
                .update(cx, |pane, _window, cx| {
                    assert!(
                        pane.grid().expect("rows").read(cx).editing().is_none(),
                        "{name} opened a field from the activation"
                    );
                    assert!(!pane.has_pending_edits(cx));
                })
                .expect("the window is open");

            select(&window, 0, 0, cx);
            let rows = cell_menu(&window, cx);
            for label in [
                ts!("data.set_null"),
                ts!("data.insert_row"),
                ts!("data.delete_row"),
                ts!("data.discard_row"),
            ] {
                assert!(
                    !context_menu::row(&rows, &label).is_enabled(),
                    "{name} offered {label}"
                );
            }
            connected.close().expect("close");
        }
    }

    /// Closing a tab holding staged changes is refused, and the pane says why.
    ///
    /// Asserted at the seam rather than through the workspace: what the shell
    /// asks is `PaneItem::blocks_close`, and what it does about a yes is to
    /// bring the tab forward and call this.
    #[gpui::test]
    fn a_tab_holding_changes_refuses_to_close(cx: &mut TestAppContext) {
        let (connected, profile) = person("data-close");
        let window = pane(&connected, &profile, target("PERSON"), 500, cx);
        assert!(type_into(&window, 0, 1, "A", cx));

        window
            .update(cx, |pane, _window, cx| {
                assert!(pane.has_pending_edits(cx));
                pane.warn_pending(cx);
                assert_eq!(
                    pane.notice.as_ref().map(|notice| notice.text.clone()),
                    Some(ts!("data.discard_first"))
                );

                pane.discard_all(cx);
                assert!(!pane.has_pending_edits(cx));
                assert!(pane.notice.is_none(), "the refusal outlived the reason");
            })
            .expect("the window is open");
        connected.close().expect("close");
    }

    /// The table as the server holds it, read straight off the session and not
    /// through the pane.
    ///
    /// Every apply assertion needs both readings: what the pane shows is the
    /// staging buffer over whatever it last fetched, and the question a rollback
    /// test asks is about the table itself.
    fn server_rows(connected: &Connected, sql: &str) -> Vec<String> {
        let cursor = connected
            .session()
            .execute(&StatementSpec::new(sql))
            .unwrap_or_else(|error| panic!("{sql}: {error}"));
        let batch = cursor.fetch(500).expect("the batch decodes");
        // `to_text` needs a column to know whether a float is single precision,
        // and nothing read here is one.
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

    /// The statements the pane's confirmation is showing.
    fn preview(window: &WindowHandle<DataPane>, cx: &mut TestAppContext) -> Vec<String> {
        window
            .update(cx, |pane, _window, _cx| {
                pane.preview
                    .as_ref()
                    .map(|statements| {
                        statements
                            .iter()
                            .map(|statement| statement.sql.clone())
                            .collect()
                    })
                    .unwrap_or_default()
            })
            .expect("the window is open")
    }

    /// A mixed batch — one changed cell, one deleted row, one new row whose
    /// key the server generates — goes out as three statements, and the pane
    /// comes back showing what the server now holds.
    #[gpui::test]
    fn a_mixed_edit_set_is_applied_and_the_rows_are_read_back(cx: &mut TestAppContext) {
        let (connected, profile) = h2(
            "data-apply",
            &[
                "create schema if not exists APP",
                "create table APP.PERSON (ID int auto_increment primary key, NAME varchar(20))",
                "insert into APP.PERSON (NAME) values ('a'), ('b'), ('c')",
            ],
        );
        let window = pane(&connected, &profile, target("PERSON"), 500, cx);
        assert_eq!(column(&window, 1, cx), ["a", "b", "c"]);

        // A cell, a deletion and a new row. The key column is auto-increment,
        // so the grid refuses to type into it and the `INSERT` leaves it out —
        // which is what makes the server generate one.
        assert!(type_into(&window, 0, 1, "A", cx));
        window
            .update(cx, |pane, window, cx| {
                pane.toggle_delete(1, cx);
                pane.insert_row(window, cx);
            })
            .expect("the window is open");
        assert!(type_into(&window, 0, 1, "d", cx));

        // Apply plans and shows; nothing has gone out yet.
        window
            .update(cx, |pane, window, cx| pane.apply(window, cx))
            .expect("the window is open");
        assert_eq!(
            preview(&window, cx),
            [
                "DELETE FROM APP.PERSON WHERE ID = ?",
                "UPDATE APP.PERSON SET NAME = ? WHERE ID = ?",
                "INSERT INTO APP.PERSON (NAME) VALUES (?)",
            ]
        );
        assert_eq!(
            server_rows(&connected, "select NAME from APP.PERSON order by ID"),
            ["a", "b", "c"],
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
                assert_eq!(
                    pane.notice.as_ref().map(|notice| notice.text.clone()),
                    Some(ts!("data.applied", count = 3))
                );
            })
            .expect("the window is open");
        // The reload is what makes the generated key visible: it is 4, and
        // nothing on this side could have known that.
        assert_eq!(column(&window, 0, cx), ["1", "3", "4"]);
        assert_eq!(column(&window, 1, cx), ["A", "c", "d"]);
        connected.close().expect("close");
    }

    /// Canceling the confirmation sends nothing and keeps everything staged.
    #[gpui::test]
    fn the_confirmation_can_be_turned_down(cx: &mut TestAppContext) {
        let (connected, profile) = person("data-apply-cancel");
        let window = pane(&connected, &profile, target("PERSON"), 500, cx);
        assert!(type_into(&window, 0, 1, "A", cx));

        window
            .update(cx, |pane, window, cx| {
                pane.apply(window, cx);
                assert_eq!(pane.preview.as_ref().map(Vec::len), Some(1));
                // What the modal's Cancel button and its dismiss both do.
                pane.preview = None;
            })
            .expect("the window is open");
        cx.run_until_parked();

        window
            .update(cx, |pane, _window, cx| {
                assert!(pane.has_pending_edits(cx), "the edit was thrown away");
            })
            .expect("the window is open");
        assert_eq!(
            column(&window, 1, cx),
            ["A", "b"],
            "the overlay still holds"
        );
        assert_eq!(
            server_rows(&connected, "select NAME from APP.PERSON order by ID"),
            ["a", "b"]
        );
        connected.close().expect("close");
    }

    /// A row somebody else moved stops the whole batch, and the transaction
    /// takes the statements that had already run back out with it.
    #[gpui::test]
    fn a_row_that_moved_underneath_stops_the_apply(cx: &mut TestAppContext) {
        let (connected, profile) = h2(
            "data-apply-stale",
            &[
                "create schema if not exists APP",
                "create table APP.PERSON (ID int primary key, NAME varchar(20))",
                "insert into APP.PERSON values (1, 'a'), (2, 'b')",
            ],
        );
        let window = pane(&connected, &profile, target("PERSON"), 500, cx);
        assert!(type_into(&window, 0, 1, "A", cx));
        assert!(type_into(&window, 1, 1, "B", cx));

        // Behind the pane's back, between the read and the apply. The pane's
        // second `UPDATE` will find nothing, and the first has already run.
        connected
            .session()
            .execute(&StatementSpec::new("delete from APP.PERSON where ID = 2"))
            .expect("the row goes");

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
                assert_eq!(problem.message, Some(ts!("data.apply_stale")));
                assert!(problem.error.is_none(), "no driver refused anything");
                assert!(!problem.half_applied, "the rollback went through");
                assert!(pane.has_pending_edits(cx), "the staging was thrown away");
            })
            .expect("the window is open");
        // The `UPDATE` that did reach its row was rolled back with the rest.
        assert_eq!(
            server_rows(&connected, "select NAME from APP.PERSON order by ID"),
            ["a"]
        );
        assert_eq!(
            column(&window, 1, cx),
            ["A", "B"],
            "the edits are still here"
        );
        connected.close().expect("close");
    }

    /// A statement the server refuses rolls the batch back and says why in the
    /// driver's own words.
    #[gpui::test]
    fn a_driver_refusal_rolls_the_whole_batch_back(cx: &mut TestAppContext) {
        let (connected, profile) = h2(
            "data-apply-refused",
            &[
                "create schema if not exists APP",
                "create table APP.PERSON (ID int primary key, NAME varchar(20) not null)",
                "insert into APP.PERSON values (1, 'a'), (2, 'b')",
            ],
        );
        let window = pane(&connected, &profile, target("PERSON"), 500, cx);

        // The first row's `UPDATE` is fine; the second sets a NOT NULL column
        // to NULL. Row order is the buffer's, so the good one runs first.
        assert!(type_into(&window, 0, 1, "A", cx));
        window
            .update(cx, |pane, _window, cx| pane.set_null(1, 1, cx))
            .expect("the window is open");

        window
            .update(cx, |pane, window, cx| {
                pane.apply(window, cx);
                assert_eq!(pane.preview.as_ref().map(Vec::len), Some(2));
                pane.confirm_apply(window, cx);
            })
            .expect("the window is open");
        cx.run_until_parked();

        window
            .update(cx, |pane, _window, cx| {
                let problem = pane.apply_error.as_ref().expect("the apply failed");
                assert!(problem.error.is_some(), "the driver said nothing");
                assert!(problem.message.is_none(), "this is not the staleness");
                assert!(!problem.half_applied);
                assert!(pane.has_pending_edits(cx));
            })
            .expect("the window is open");
        assert_eq!(
            server_rows(&connected, "select NAME from APP.PERSON order by ID"),
            ["a", "b"],
            "the statement before the refused one stayed"
        );

        // And autocommit is back on: a statement run now needs no commit to be
        // seen by the next reader.
        connected
            .session()
            .execute(&StatementSpec::new(
                "insert into APP.PERSON values (3, 'c')",
            ))
            .expect("the insert runs");
        assert_eq!(
            server_rows(&connected, "select NAME from APP.PERSON order by ID"),
            ["a", "b", "c"]
        );
        connected.close().expect("close");
    }

    /// A value the column's type cannot take is refused before anything is
    /// sent, and the refusal names the column.
    #[gpui::test]
    fn a_value_the_column_cannot_take_never_reaches_a_statement(cx: &mut TestAppContext) {
        let (connected, profile) = h2(
            "data-apply-bad",
            &[
                "create schema if not exists APP",
                "create table APP.PERSON (ID int primary key, AGE int)",
                "insert into APP.PERSON values (1, 30)",
            ],
        );
        let window = pane(&connected, &profile, target("PERSON"), 500, cx);
        assert!(type_into(&window, 0, 1, "thirty", cx));

        window
            .update(cx, |pane, window, cx| {
                pane.apply(window, cx);
                assert!(pane.preview.is_none(), "an unbindable value was planned");
                let problem = pane.apply_error.as_ref().expect("the plan failed");
                assert_eq!(
                    problem.message,
                    Some(ts!(
                        "data.apply_bad_value",
                        column = "AGE",
                        value = "thirty"
                    ))
                );
                assert!(pane.has_pending_edits(cx));
            })
            .expect("the window is open");
        connected.close().expect("close");
    }

    /// A pane that may not be written to refuses an apply even with something
    /// staged in it, which is only reachable by going round the grid.
    #[gpui::test]
    fn a_read_only_pane_refuses_to_apply(cx: &mut TestAppContext) {
        let (connected, mut profile) = person("data-apply-read-only");
        profile.read_only = true;
        let window = pane(&connected, &profile, target("PERSON"), 500, cx);

        window
            .update(cx, |pane, window, cx| {
                assert_eq!(pane.read_only_reason(), Some(ts!("data.read_only")));
                // Past the grid, which would refuse: what is under test is the
                // check `apply` makes for itself.
                let grid = pane.grid().cloned().expect("the pane holds rows");
                grid.update(cx, |grid, cx| {
                    grid.source_mut(cx).edits_mut().toggle_deleted(0);
                });
                assert!(pane.has_pending_edits(cx));

                pane.apply(window, cx);
                assert!(pane.preview.is_none(), "a read-only pane planned a batch");
                assert!(pane.apply_error.is_none());

                // And so does the second half, in case the first were ever
                // reached with a plan already in hand.
                pane.preview = Some(Vec::new());
                pane.confirm_apply(window, cx);
                assert!(!pane.applying);
            })
            .expect("the window is open");
        cx.run_until_parked();
        assert_eq!(
            server_rows(&connected, "select NAME from APP.PERSON order by ID"),
            ["a", "b"]
        );
        connected.close().expect("close");
    }

    /// The batch ends in a commit or a rollback on the session every pane of the
    /// connection shares, so it waits for a query editor's open transaction to
    /// be settled rather than ending it along the way.
    #[gpui::test]
    fn an_open_editor_transaction_holds_the_apply_back(cx: &mut TestAppContext) {
        let (connected, profile) = person("data-apply-uncommitted");
        let window = pane(&connected, &profile, target("PERSON"), 500, cx);
        let handle = connected.handle();
        handle.mark_uncommitted();

        assert!(type_into(&window, 0, 1, "A", cx));
        window
            .update(cx, |pane, window, cx| {
                pane.apply(window, cx);
                assert!(pane.preview.is_none(), "the confirmation was raised");
                assert_eq!(
                    pane.notice.as_ref().map(|notice| notice.text.clone()),
                    Some(ts!("query.uncommitted_pending"))
                );

                handle.clear_uncommitted();
                pane.apply(window, cx);
                assert!(pane.preview.is_some());

                // Opened again between the confirmation and its answer: the
                // modal stays up and nothing is sent.
                handle.mark_uncommitted();
                pane.confirm_apply(window, cx);
                assert!(pane.preview.is_some(), "the confirmation went away");
                assert!(!pane.applying);
            })
            .expect("the window is open");
        cx.run_until_parked();
        assert_eq!(
            server_rows(&connected, "select NAME from APP.PERSON order by ID"),
            ["a", "b"],
            "the batch went out over an open transaction"
        );
        connected.close().expect("close");
    }

    /// A product with no transactions says so in the confirmation, because the
    /// guarantee the rest of it implies is the one thing that does not hold.
    #[gpui::test]
    fn a_product_without_transactions_warns_in_the_confirmation(cx: &mut TestAppContext) {
        let (connected, profile) = person("data-apply-no-txn");
        let window = pane(&connected, &profile, target("PERSON"), 500, cx);

        window
            .update(cx, |pane, _window, _cx| {
                assert!(pane.transactional, "H2 has transactions");
                pane.transactional = false;
            })
            .expect("the window is open");
        assert!(type_into(&window, 0, 1, "A", cx));
        window
            .update(cx, |pane, window, cx| {
                pane.apply(window, cx);
                assert!(pane.preview.is_some());
                // The line the modal draws for exactly this case.
                assert!(!ts!("data.apply_no_rollback").is_empty());
            })
            .expect("the window is open");
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
            ts!("data.pending", changed = 1, inserted = 2, deleted = 3),
            ts!("data.apply"),
            ts!("data.discard"),
            ts!("data.discard_title"),
            ts!("data.discard_body", changed = 1, inserted = 2, deleted = 3),
            ts!("data.set_null"),
            ts!("data.insert_row"),
            ts!("data.delete_row"),
            ts!("data.undelete_row"),
            ts!("data.discard_row"),
            ts!("data.apply_title", count = 3),
            ts!("data.apply_no_rollback"),
            ts!("data.applying"),
            ts!("data.applied", count = 3),
            ts!("data.apply_stale"),
            ts!("data.apply_half_applied"),
            ts!("data.apply_bad_value", column = "AGE", value = "x"),
            ts!("data.apply_null_key", column = "ID"),
            ts!("data.apply_unknown_key", column = "ID"),
            ts!("data.apply_not_planned", detail = "why"),
            ts!("menu.view_data"),
        ] {
            assert!(!label.is_empty(), "empty label");
            assert!(!label.starts_with("data."), "untranslated {label:?}");
            assert!(!label.starts_with("menu."), "untranslated {label:?}");
        }
    }
    #[gpui::test]
    fn multiline_data_cells_open_the_viewer_when_tabbing(cx: &mut TestAppContext) {
        let (connected, profile) = h2(
            "data-multiline",
            &[
                "CREATE SCHEMA APP",
                "CREATE TABLE APP.NOTES (ID INT PRIMARY KEY, NAME VARCHAR(30), BODY VARCHAR(1000))",
                "INSERT INTO APP.NOTES VALUES (1, 'name', 'first\nsecond\n')",
            ],
        );
        let handle = pane(&connected, &profile, target("NOTES"), 100, cx);
        let mut cx = gpui::VisualTestContext::from_window(handle.into(), cx);
        handle
            .update(&mut cx, |pane, window, cx| {
                window.activate_window();
                pane.grid().unwrap().update(cx, |grid, cx| {
                    assert!(grid.begin_edit(0, 1, window, cx));
                });
            })
            .unwrap();
        cx.simulate_keystrokes("tab");
        cx.run_until_parked();
        handle
            .update(&mut cx, |pane, _, cx| {
                assert!(pane.lob_dialog.is_some());
                assert!(pane.grid().unwrap().read(cx).editing().is_none());
                assert!(!pane.has_pending_edits(cx));
            })
            .unwrap();
        cx.simulate_keystrokes(if cfg!(target_os = "macos") {
            "cmd-a"
        } else {
            "ctrl-a"
        });
        cx.simulate_input("new first\n새 줄🙂\n");
        cx.simulate_keystrokes(if cfg!(target_os = "macos") {
            "cmd-enter"
        } else {
            "ctrl-enter"
        });
        cx.run_until_parked();
        handle
            .update(&mut cx, |pane, window, cx| {
                assert!(pane.lob_dialog.is_none());
                assert!(pane.has_pending_edits(cx));
                pane.apply(window, cx);
                assert_eq!(
                    pane.preview.as_ref().unwrap()[0].params[0],
                    rudbman_jdbc::Param::Str("new first\n새 줄🙂\n".into())
                );
                pane.confirm_apply(window, cx);
            })
            .unwrap();
        cx.run_until_parked();
        assert_eq!(
            server_rows(&connected, "SELECT BODY FROM APP.NOTES"),
            ["new first\n새 줄🙂\n"]
        );
        handle
            .update(&mut cx, |pane, _, cx| {
                assert!(pane.apply_error.is_none());
                assert!(!pane.has_pending_edits(cx));
            })
            .unwrap();
    }

    /// A one-line text cell's field carries the expand button: pressing it
    /// opens the shared viewer on what had been typed, and stages nothing
    /// until the viewer does. A number column's field has no button.
    #[gpui::test]
    fn the_expand_button_opens_a_one_line_cell_in_the_viewer(cx: &mut TestAppContext) {
        let (connected, profile) = h2(
            "data-expand",
            &[
                "CREATE SCHEMA APP",
                "CREATE TABLE APP.NOTES (ID INT PRIMARY KEY, NAME VARCHAR(30), QTY INT)",
                "INSERT INTO APP.NOTES VALUES (1, 'name', 3)",
            ],
        );
        let handle = pane(&connected, &profile, target("NOTES"), 100, cx);
        let mut cx = gpui::VisualTestContext::from_window(handle.into(), cx);
        handle
            .update(&mut cx, |pane, window, cx| {
                window.activate_window();
                pane.grid().unwrap().update(cx, |grid, cx| {
                    assert!(grid.begin_edit(0, 2, window, cx));
                    assert!(!grid.is_expandable(), "a number has no use for a paragraph");
                    grid.cancel_edit(cx);
                    assert!(grid.begin_edit(0, 1, window, cx));
                    assert!(grid.is_expandable());
                });
            })
            .unwrap();
        cx.run_until_parked();
        cx.simulate_input("!");
        let button = cx
            .debug_bounds("grid-cell-expand")
            .expect("a text cell's field draws the expand button");
        cx.simulate_click(button.center(), gpui::Modifiers::none());
        cx.run_until_parked();
        handle
            .update(&mut cx, |pane, _, cx| {
                assert!(pane.lob_dialog.is_some());
                assert!(pane.grid().unwrap().read(cx).editing().is_none());
                assert!(
                    pane.grid().unwrap().read(cx).source().edits().is_empty(),
                    "the button stages nothing itself"
                );
                // What was typed is already an edit, held by the viewer.
                assert!(pane.has_pending_edits(cx));
            })
            .unwrap();
        // And `Add to changes` stages it as it is.
        cx.simulate_keystrokes(if cfg!(target_os = "macos") {
            "cmd-enter"
        } else {
            "ctrl-enter"
        });
        cx.run_until_parked();
        handle
            .update(&mut cx, |pane, window, cx| {
                assert!(pane.lob_dialog.is_none());
                assert!(pane.has_pending_edits(cx));
                pane.apply(window, cx);
                assert_eq!(
                    pane.preview.as_ref().unwrap()[0].params[0],
                    rudbman_jdbc::Param::Str("name!".into())
                );
                pane.confirm_apply(window, cx);
            })
            .unwrap();
        cx.run_until_parked();
        assert_eq!(
            server_rows(&connected, "SELECT NAME FROM APP.NOTES"),
            ["name!"]
        );
    }

    #[gpui::test]
    fn lob_data_cells_open_the_shared_editor_and_apply_streaming_values(cx: &mut TestAppContext) {
        let (connected, profile) = h2(
            "data-lob",
            &[
                "CREATE SCHEMA APP",
                "CREATE TABLE APP.FILES (ID INT PRIMARY KEY, B BLOB, C CLOB)",
                "INSERT INTO APP.FILES VALUES (1, X'00FF', 'before')",
            ],
        );
        let handle = pane(&connected, &profile, target("FILES"), 100, cx);
        let mut cx = gpui::VisualTestContext::from_window(handle.into(), cx);
        handle
            .update(&mut cx, |pane, window, cx| {
                window.activate_window();
                let grid = pane.grid().unwrap().clone();
                assert_eq!(
                    rugpui_grid::cell_label(&grid.read(cx).source().cell(0, 1)).text,
                    "00FF"
                );
                assert_eq!(
                    rugpui_grid::cell_label(&grid.read(cx).source().cell(0, 2)).text,
                    "before"
                );
                assert_eq!(
                    grid.read(cx)
                        .source()
                        .lob(0, 1)
                        .unwrap()
                        .prefix(10)
                        .unwrap(),
                    [0, 255]
                );
                grid.update(cx, |_, cx| {
                    cx.emit(GridEvent::CellActivated { row: 0, column: 1 })
                });
            })
            .unwrap();
        cx.run_until_parked();
        handle
            .update(&mut cx, |pane, _, _| assert!(pane.lob_dialog.is_some()))
            .unwrap();
        cx.simulate_keystrokes(if cfg!(target_os = "macos") {
            "cmd-a"
        } else {
            "ctrl-a"
        });
        cx.simulate_input("DE AD 00 FF");
        cx.simulate_keystrokes(if cfg!(target_os = "macos") {
            "cmd-enter"
        } else {
            "ctrl-enter"
        });
        cx.run_until_parked();
        handle
            .update(&mut cx, |pane, window, cx| {
                assert!(pane.lob_dialog.is_none());
                assert!(pane.has_pending_edits(cx));
                assert_eq!(
                    pane.grid()
                        .unwrap()
                        .read(cx)
                        .source()
                        .lob(0, 1)
                        .unwrap()
                        .prefix(10)
                        .unwrap(),
                    [0xde, 0xad, 0, 255]
                );
                pane.apply(window, cx);
                assert!(matches!(
                    pane.preview.as_ref().unwrap()[0].params[0],
                    rudbman_jdbc::Param::Lob(_)
                ));
                pane.confirm_apply(window, cx);
            })
            .unwrap();
        cx.run_until_parked();
        handle
            .update(&mut cx, |pane, _, cx| {
                assert!(pane.apply_error.is_none());
                assert!(!pane.has_pending_edits(cx));
                assert_eq!(
                    pane.grid()
                        .unwrap()
                        .read(cx)
                        .source()
                        .lob(0, 1)
                        .unwrap()
                        .prefix(10)
                        .unwrap(),
                    [0xde, 0xad, 0, 255]
                );
            })
            .unwrap();
        assert_eq!(
            server_rows(&connected, "SELECT RAWTOHEX(B) FROM APP.FILES"),
            ["dead00ff"]
        );
    }
}
