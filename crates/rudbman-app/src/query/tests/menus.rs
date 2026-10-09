use super::*;

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
