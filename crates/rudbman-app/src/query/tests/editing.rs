use super::*;

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
