//! The rows of a right-click menu that are rudbman's own.
//!
//! Every surface of the window has a menu and every one of them is built out of
//! [`MenuRow`] (architecture document, §7.8) — the shell's row type, because a
//! menu described as data rather than as [`MenuEntry`](rugpui::MenuEntry) is what
//! makes "what does this node offer" a testable question in every application
//! that asks it. [`entries`] turns a list of rows into the widget's own rows on
//! the way to being drawn, and [`labels`], [`greyed`] and [`row`] are what the
//! tests read them back with.
//!
//! What is left here is the two menus that are *about a result grid*: the cell
//! menu and the heading menu. They are generic over the grid's source rather
//! than over the application, but they are still rudbman's — the wording is its
//! own, the ordering row re-runs a statement, and `rugpui-grid` is not something
//! the shell depends on.

use gpui::{App, ClipboardItem, Entity, Window};
use rugpui_grid::{CopyFormat, GridSource, GridView, SortDirection};
pub(crate) use rugpui_shell::menu_rows::{MenuRow, entries};
#[cfg(test)]
pub(crate) use rugpui_shell::menu_rows::{greyed, labels, row};

use crate::SHORTCUT_MODIFIER;
use crate::i18n::ts;

/// What every grid's cell menu offers, whatever the grid is showing.
///
/// The four copy formats, then selecting and clearing — all of them things
/// [`GridView`] already does, and none of them dependent on where the rows came
/// from. Written once and generic over the source because two panes now draw a
/// grid menu: the query pane over a result nobody can write to, and the data
/// pane over one that stages edits (architecture document, §7.9). A second copy
/// of these seven rows would be a second place for the shortcut hint and the
/// greying rule to drift.
///
/// The rows act on [`GridView::selection`] rather than on the pressed cell: a
/// right click inside a selection leaves it alone, so the cell under the
/// pointer is not necessarily the interesting one.
pub(crate) fn grid_copy_rows<S: GridSource>(grid: &Entity<GridView<S>>, cx: &App) -> Vec<MenuRow> {
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
            // Only the default format carries the hint: `Ctrl+C` is one chord
            // and copies TSV, and repeating it on four rows would say it does
            // all four.
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
    rows.push({
        let grid = grid.clone();
        MenuRow::new(ts!("context.clear_selection"))
            .enabled(!empty)
            .on_activate(move |_window, cx| {
                grid.update(cx, |grid, cx| grid.clear_selection(cx));
            })
    });
    rows
}

/// What every grid's heading menu offers, for the heading of `column`.
///
/// Ordering is the one row here that does not go through the grid: it holds
/// only the first `n` rows of an answer the server has all of, so sorting is a
/// re-run and not a shuffle — and how the statement is re-run differs per pane.
/// `order` is that re-run, called with the direction asked for or `None` to
/// drop the ordering.
///
/// "Show every column" is the row no other gesture offers: a hidden column has
/// no heading left to right-click.
pub(crate) fn grid_header_rows<S: GridSource>(
    grid: &Entity<GridView<S>>,
    column: usize,
    cx: &App,
    order: impl Fn(Option<SortDirection>, &mut Window, &mut App) + Clone + 'static,
) -> Vec<MenuRow> {
    let sort = grid.read(cx).sort();
    let nothing_hidden = grid.read(cx).hidden_column_count() == 0;
    let name = grid.read(cx).column_name(column).map(str::to_owned);
    let sorted = |direction: SortDirection| sort == Some((column, direction));
    let order = |direction: Option<SortDirection>| {
        let order = order.clone();
        move |window: &mut Window, cx: &mut App| order(direction, window, cx)
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
