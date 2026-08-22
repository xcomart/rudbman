//! Sending a staged edit set to the server, and the three surfaces around it.
//!
//! The half of the architecture document's §7.9 apply that a planner cannot do:
//! read the primary key off the catalogue, run the batch in one transaction,
//! say in the user's language why it did not happen, and draw the confirmation
//! that every write goes through. §7.9's "editing a query result" subsection is
//! why any of it is a module rather than a pane's private half — the data pane
//! and the query pane both apply row edits, and the transaction ordering below
//! is the last thing this codebase should hold two copies of.
//!
//! # Why not in [`crate::data_edit`]
//!
//! [`crate::data_edit`] holds the staging buffer, the source overlay and the
//! planner, and its charter is that it contains no gpui and no
//! [`Session`]: everything it decides is decidable from the buffer and the
//! column metadata alone, so it is testable without a window and without a JVM.
//! That property is worth keeping, and everything here would cost it —
//! [`apply_batch`] needs a live session, and the three renderers need gpui. So
//! the split is by what a thing needs rather than by what it is about: the pure
//! half is next door, the half that talks to a server or to a screen is here.
//!
//! # The one ordering rule
//!
//! On any failure the rollback goes out **before** autocommit is restored.
//! Restoring autocommit first is an implicit commit on several products, which
//! would commit the very half-applied batch the rollback is there to undo. See
//! [`apply_batch`] and [`unwind`].
//!
//! # No pane types in here
//!
//! The two modal renderers take plain callbacks rather than the entity that
//! hosts them. A type parameter that exists only to thread an entity through
//! would spread across every signature in this file and buy nothing: what each
//! button does is the caller's business, and a closure says it at the call site
//! where it can be read.

use std::rc::Rc;

use gpui::{
    AnyElement, App, IntoElement, ParentElement, SharedString, Styled, Window, div, prelude::*, px,
};
use rudbman_jdbc::{DescribeRequest, Error as JdbcError, Session, StatementSpec};
use rudbman_sql::DmlError;
use rudbman_ui::{Button, ButtonVariant, Theme, modal, theme};

use crate::data_edit::{EditCounts, PlanError, PlannedStatement};
use crate::i18n::ts;
use crate::query::{QueryError, error_lines};
use crate::table_detail::{number, text};

/// What a failed apply left behind, and what to say about it.
///
/// Three parts because the failures are three shapes: the driver refused a
/// statement, this side refused to send one, or — the one that needs saying
/// loudest — the unwind itself failed and the batch may be half in.
pub(crate) struct ApplyProblem {
    /// The driver's own envelope, when a statement is what failed.
    pub(crate) error: Option<Box<QueryError>>,
    /// This side's own words, when the refusal never reached the driver.
    pub(crate) message: Option<SharedString>,
    /// Whether the rollback failed too, so the table may hold part of the
    /// batch. The one case where the user has to go and look.
    pub(crate) half_applied: bool,
}

impl ApplyProblem {
    /// A refusal this side made, in words of its own.
    pub(crate) fn local(message: SharedString) -> Box<Self> {
        Box::new(Self {
            error: None,
            message: Some(message),
            half_applied: false,
        })
    }
}

/// The primary key's columns, in key order.
///
/// `KEY_SEQ` decides the order rather than the order the driver listed them in:
/// a composite key's `WHERE` clause is written from this, and a key read in the
/// wrong order would still work while reading rather differently from the
/// table's own DDL.
///
/// The three name parts are taken loose rather than as an [`ObjectTarget`] so
/// that a caller who has them from somewhere else — a query result's
/// [`ColumnInfo`], which carries a `catalog`, `schema` and `table` per column —
/// can ask the same question. An **empty** catalog or schema is normalised to
/// `None`, and that is not tidying: JDBC's `getPrimaryKeys` reads an exact `""`
/// as "objects that have no catalog/schema at all", so passing the `""` several
/// drivers report unconditionally would ask about a table nobody has and get a
/// silent empty answer. `None` is the wildcard, and unknown is what `""` means.
///
/// A driver that answers nothing — a view, a table with no key — gives an empty
/// list, which is the read-only case rather than a failure.
///
/// [`ObjectTarget`]: crate::explorer::ObjectTarget
/// [`ColumnInfo`]: rudbman_jdbc::ColumnInfo
pub(crate) fn primary_key(
    session: &Session,
    catalog: Option<&str>,
    schema: Option<&str>,
    table: &str,
) -> Result<Vec<String>, JdbcError> {
    let mut request = DescribeRequest::new("primary_keys").with_table(table);
    request.catalog = catalog.filter(|name| !name.is_empty()).map(str::to_string);
    request.schema = schema.filter(|name| !name.is_empty()).map(str::to_string);

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

/// One statement's bound values, as the confirmation lists them.
///
/// `None` for a statement that binds nothing — an `INSERT ... DEFAULT VALUES` —
/// so that the line is left out rather than drawn empty. NULL is spelled `NULL`
/// and not translated: it is the value's SQL name, and a localised one would
/// stop matching the statement above it.
fn render_values(values: &[Option<String>]) -> Option<SharedString> {
    if values.is_empty() {
        return None;
    }
    // A middle dot rather than a comma, because a comma is a character the
    // values themselves very often contain.
    let joined = values
        .iter()
        .map(|value| value.as_deref().unwrap_or("NULL"))
        .collect::<Vec<_>>()
        .join(" · ");
    Some(SharedString::from(joined))
}

/// Why a staged buffer could not be turned into statements, in the user's
/// language.
///
/// Three of the four cases name a column, which is the whole reason the planning
/// checks values before anything is generated. The fourth carries
/// `rudbman-sql`'s own sentence: its remaining variants describe shapes the row
/// editor does not build, so a translated line per variant would be eight
/// strings nobody can reach in exchange for one that would then say nothing
/// useful.
///
/// The keys stay in the `data.*` namespace now that this is shared: they are
/// about editing rows, which is what both callers are doing, and renaming them
/// to match a module would churn eight locale files for no reader's benefit.
pub(crate) fn plan_message(error: &PlanError) -> SharedString {
    match error {
        PlanError::NoKey => ts!("data.no_primary_key"),
        PlanError::UnknownKeyColumn { column } => {
            ts!("data.apply_unknown_key", column = column.clone())
        }
        PlanError::BadValue { column, text, .. } => ts!(
            "data.apply_bad_value",
            column = column.clone(),
            value = text.clone()
        ),
        PlanError::Dml(DmlError::NullKey { column }) => {
            ts!("data.apply_null_key", column = column.clone())
        }
        PlanError::Dml(other) => ts!("data.apply_not_planned", detail = other.to_string()),
    }
}

/// The "throw it all away" confirmation.
///
/// Asked rather than done, because the button is next to the one that applies
/// and the two are irreversible in opposite directions.
///
/// `on_cancel` is what both ways out — the button and the modal's own dismiss —
/// call, so the caller writes the retraction once.
pub(crate) fn render_discard_confirm(
    counts: EditCounts,
    cx: &mut App,
    on_cancel: impl Fn(&mut Window, &mut App) + 'static,
    on_discard: impl Fn(&mut Window, &mut App) + 'static,
) -> AnyElement {
    let chrome = theme(cx);
    let on_cancel = Rc::new(on_cancel);
    let dismiss = on_cancel.clone();

    let body = div()
        .flex()
        .flex_col()
        .gap(px(12.))
        .child(div().text_size(px(12.)).text_color(chrome.text).child(ts!(
            "data.discard_body",
            changed = counts.changed,
            inserted = counts.inserted,
            deleted = counts.deleted
        )))
        .child(
            div()
                .flex()
                .flex_row()
                .justify_end()
                .gap(px(8.))
                .child(
                    Button::new("data-discard-cancel", ts!("common.cancel"))
                        .variant(ButtonVariant::Secondary)
                        .on_click({
                            let on_cancel = on_cancel.clone();
                            move |_, window, cx| on_cancel(window, cx)
                        }),
                )
                .child(
                    Button::new("data-discard-confirm", ts!("data.discard"))
                        .variant(ButtonVariant::Danger)
                        .on_click(move |_, window, cx| on_discard(window, cx)),
                ),
        );

    modal(
        "data-discard",
        ts!("data.discard_title"),
        px(420.),
        body,
        move |window, cx| dismiss(window, cx),
    )
    .into_any_element()
}

/// The statements an apply is about to send.
///
/// The whole batch, in the order it will run, each statement over the values
/// its `?`s will take. That is a deliberately literal confirmation: the
/// question worth asking before a write is not "are you sure" but "is this
/// what you meant", and only the `WHERE` clause can answer it.
///
/// A product with no transactions says so here, in red, because the guarantee
/// the rest of this modal implies is the one thing that does not hold for it: a
/// batch that fails half way stays half applied.
///
/// `on_cancel` is shared by the button and the modal's dismiss, as above;
/// `on_run` is what sends the batch.
pub(crate) fn render_apply_preview(
    statements: &[PlannedStatement],
    transactional: bool,
    cx: &mut App,
    on_cancel: impl Fn(&mut Window, &mut App) + 'static,
    on_run: impl Fn(&mut Window, &mut App) + 'static,
) -> AnyElement {
    let chrome = theme(cx);
    let mono = crate::app_settings::monospace_family(cx);
    let on_cancel = Rc::new(on_cancel);
    let dismiss = on_cancel.clone();

    let listing = statements.iter().enumerate().map(|(index, statement)| {
        let values = render_values(&statement.values);
        div()
            .flex()
            .flex_col()
            .gap(px(2.))
            .when(index > 0, |row| row.mt(px(8.)))
            .child(
                div()
                    .font_family(mono.clone())
                    .text_size(px(11.))
                    .text_color(chrome.text)
                    .child(SharedString::from(statement.sql.clone())),
            )
            .children(values.map(|values| {
                div()
                    .text_size(px(11.))
                    .text_color(chrome.text_muted)
                    .child(values)
            }))
    });

    let body = div()
        .flex()
        .flex_col()
        .gap(px(12.))
        .children((!transactional).then(|| {
            div()
                .text_size(px(11.))
                .text_color(chrome.danger)
                .child(ts!("data.apply_no_rollback"))
        }))
        .child(
            div()
                .id("data-apply-preview")
                .max_h(px(260.))
                .overflow_y_scroll()
                .restrict_scroll_to_axis()
                .p(px(8.))
                .rounded_md()
                .bg(chrome.surface)
                .border_1()
                .border_color(chrome.border)
                .children(listing),
        )
        .child(
            div()
                .flex()
                .flex_row()
                .justify_end()
                .gap(px(8.))
                .child(
                    Button::new("data-apply-cancel", ts!("common.cancel"))
                        .variant(ButtonVariant::Secondary)
                        .on_click({
                            let on_cancel = on_cancel.clone();
                            move |_, window, cx| on_cancel(window, cx)
                        }),
                )
                .child(
                    Button::new("data-apply-run", ts!("data.apply"))
                        .variant(ButtonVariant::Primary)
                        .on_click(move |_, window, cx| on_run(window, cx)),
                ),
        );

    modal(
        "data-apply",
        ts!("data.apply_title", count = statements.len()),
        px(560.),
        body,
        move |window, cx| dismiss(window, cx),
    )
    .into_any_element()
}

/// Why the last apply did not happen, above the rows it did not change.
///
/// A strip and not the body: [`crate::query::render_error`] draws a failure *in
/// place of* the rows, which is right for a load that produced none and wrong
/// here — the rows are still on screen and still hold everything the user
/// staged against them.
pub(crate) fn render_apply_error(
    problem: &ApplyProblem,
    chrome: &Theme,
) -> impl IntoElement + use<> {
    div()
        .flex()
        .flex_col()
        .flex_none()
        .gap(px(4.))
        .px(px(10.))
        .py(px(6.))
        .border_b_1()
        .border_color(chrome.border)
        .bg(chrome.surface)
        .children(problem.message.clone().map(|message| {
            div()
                .text_size(px(12.))
                .text_color(chrome.danger)
                .child(message)
        }))
        .children(
            problem
                .error
                .as_ref()
                .map(|error| error_lines(error, chrome)),
        )
        .children(problem.half_applied.then(|| {
            div()
                .text_size(px(11.))
                .text_color(chrome.danger)
                .child(ts!("data.apply_half_applied"))
        }))
}

/// Why an apply stopped.
pub(crate) enum ApplyStop {
    /// The driver refused a statement, in its own words.
    Driver(JdbcError),
    /// An `UPDATE` or a `DELETE` reached a number of rows that was not one, so
    /// the row it named is not the row that was read (§7.9).
    Stale,
}

/// A failed apply: what stopped it, and what it left in the table.
pub(crate) struct ApplyFailure {
    pub(crate) stop: ApplyStop,
    /// The rollback's own failure, when even that did not go through. Logged
    /// rather than shown: what the user needs is [`ApplyFailure::half_applied`],
    /// and a second driver envelope beside the first says nothing they can act
    /// on.
    pub(crate) rollback: Option<JdbcError>,
    /// Whether part of the batch may already be in the table.
    ///
    /// Two ways to get here, and the user's next move is the same for both: a
    /// rollback that failed, and a product with no transactions to roll back,
    /// where every statement before the one that failed is simply in.
    pub(crate) half_applied: bool,
}

/// Runs one batch of statements, in one transaction, and reports what happened.
///
/// **Blocks**, and is called from `cx.background_spawn` with a
/// [`SessionHandle`]: every call in here goes through the session's own worker
/// thread, so the transaction is opened, filled and closed without anything else
/// on this connection getting in between.
///
/// The shape is §7.9's:
///
/// * `set_auto_commit(false)`, then one `execute` per statement in the order
///   they were planned — deletes, updates, inserts — then `commit`, then
///   autocommit back to what the session was opened with.
/// * Every `UPDATE` and `DELETE` has to report exactly one row. A count that is
///   not one means the `WHERE` clause reached a row somebody else has already
///   moved, and the whole apply is abandoned; that is what makes a `WHERE` over
///   the primary key alone safe.
/// * On **any** failure the rollback goes out first and autocommit is restored
///   second. The order is not cosmetic: several products treat
///   `setAutoCommit(true)` as an implicit commit, so restoring it first is
///   exactly what would commit the half-applied batch this is trying to undo.
///   The bridge's `TransferJob` documents the same trap.
///
/// `transactional == false` — a product that has no transactions — runs the
/// statements under autocommit, count checks and all. The row counts still say
/// whether each statement reached what it meant to; what is missing is any way
/// to put the earlier ones back, which is why the confirmation says so before
/// the user reaches this.
///
/// [`SessionHandle`]: crate::connection::SessionHandle
pub(crate) fn apply_batch(
    session: &Session,
    statements: &[PlannedStatement],
    transactional: bool,
    restore: bool,
) -> Result<usize, Box<ApplyFailure>> {
    if transactional {
        // Nothing has run, so a failure here needs no unwind: the session is
        // still in whatever mode it was opened in.
        session.set_auto_commit(false).map_err(|error| {
            Box::new(ApplyFailure {
                stop: ApplyStop::Driver(error),
                rollback: None,
                half_applied: false,
            })
        })?;
    }

    for (done, statement) in statements.iter().enumerate() {
        if let Err(stop) = run_one(session, statement) {
            return Err(unwind(session, stop, transactional, restore, done));
        }
    }

    if transactional {
        if let Err(error) = session.commit() {
            // A driver that failed a commit has usually rolled back already,
            // but "usually" is not a guarantee worth resting a table on, and a
            // rollback of nothing costs a round trip.
            return Err(unwind(
                session,
                ApplyStop::Driver(error),
                transactional,
                restore,
                0,
            ));
        }
        if let Err(error) = session.set_auto_commit(restore) {
            // The work is committed and the user's changes are in. Leaving the
            // session in the wrong mode is a real problem, but it is not this
            // apply's failure and reporting it as one would have the pane keep
            // changes the server already took.
            log::warn!("restoring auto-commit after an apply failed: {error}");
        }
    }
    Ok(statements.len())
}

/// Runs one statement and checks what it reached.
fn run_one(session: &Session, statement: &PlannedStatement) -> Result<(), ApplyStop> {
    let spec =
        StatementSpec::new(statement.sql.clone()).with_params(statement.params.iter().cloned());
    let cursor = session.execute(&spec).map_err(ApplyStop::Driver)?;
    // Dropping the cursor closes it; a DML statement holds nothing worth paging.
    if statement.checked && cursor.result().update_count != 1 {
        return Err(ApplyStop::Stale);
    }
    Ok(())
}

/// Puts the connection back after a failure: rollback first, autocommit second.
///
/// See [`apply_batch`] for why that order is the whole point of this function
/// existing rather than the two calls being written inline. `done` is how many
/// statements had already gone through, which decides nothing under a
/// transaction and decides everything without one.
fn unwind(
    session: &Session,
    stop: ApplyStop,
    transactional: bool,
    restore: bool,
    done: usize,
) -> Box<ApplyFailure> {
    if !transactional {
        // Nothing was held back, so there is nothing to put back: whatever ran
        // before the statement that failed is in the table.
        return Box::new(ApplyFailure {
            stop,
            rollback: None,
            half_applied: done > 0,
        });
    }
    let rollback = session.rollback().err();
    // Attempted whatever the rollback did: a session left with autocommit off
    // is a session every later statement on this connection silently joins an
    // open transaction on.
    if let Err(error) = session.set_auto_commit(restore) {
        log::warn!("restoring auto-commit after a failed apply failed: {error}");
    }
    Box::new(ApplyFailure {
        half_applied: rollback.is_some(),
        stop,
        rollback,
    })
}
