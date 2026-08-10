//! The rows of a right-click menu, before they become widget entries.
//!
//! Every surface of the window has a menu and every one of them is built here
//! (architecture document, §7.8) — but not as [`MenuEntry`], which is
//! write-only: a row of it carries a boxed callback and a private label, so
//! nothing can be read back out of one. What a menu offers on a given node, a
//! given tab or a given cell is exactly the decision worth testing, and a test
//! that had to click at a computed pixel to find out would be testing the
//! menu's line height.
//!
//! So each surface builds a [`MenuRow`] list — label, hint, whether it is live,
//! whether it is ticked, and what it does — and [`entries`] turns the list into
//! the widget's own rows on the way to being drawn. The description is what the
//! tests read, and it is the same list the user sees, not a second account of
//! it.

use std::rc::Rc;

use gpui::{App, ClipboardItem, Entity, SharedString, Window};
use rudbman_grid::{CopyFormat, GridSource, GridView, SortDirection};
use rudbman_ui::MenuEntry;

use crate::SHORTCUT_MODIFIER;
use crate::i18n::ts;

/// What a row does when it is run.
type Activate = Rc<dyn Fn(&mut Window, &mut App)>;

/// What one row of a context menu says and does.
///
/// A separator is a row with no label, for the same reason [`MenuEntry`] makes
/// it one: a menu is a list, and the rule between two groups of it holds a
/// place in that list rather than being a property of its neighbours.
pub(crate) struct MenuRow {
    /// The row's words, or `None` for a separator.
    label: Option<SharedString>,
    /// The chord that runs the same command, when there is one.
    shortcut: Option<SharedString>,
    /// Whether the row can be run at all.
    enabled: bool,
    /// Whether the row is the choice currently in effect.
    checked: bool,
    /// What running it does.
    run: Option<Activate>,
}

impl MenuRow {
    /// A command row: live, unticked, and doing nothing until told what.
    pub(crate) fn new(label: impl Into<SharedString>) -> Self {
        Self {
            label: Some(label.into()),
            shortcut: None,
            enabled: true,
            checked: false,
            run: None,
        }
    }

    /// A rule between two groups of commands.
    pub(crate) fn separator() -> Self {
        Self {
            label: None,
            shortcut: None,
            enabled: true,
            checked: false,
            run: None,
        }
    }

    /// Names the chord that runs the same command.
    ///
    /// Decoration only, exactly as [`MenuEntry::shortcut`] is: the binding is
    /// registered elsewhere and the row dispatches the same thing it does.
    pub(crate) fn shortcut(mut self, shortcut: impl Into<SharedString>) -> Self {
        self.shortcut = Some(shortcut.into());
        self
    }

    /// Says whether the row can be run, greying it out when it cannot.
    pub(crate) fn enabled(mut self, enabled: bool) -> Self {
        self.enabled = enabled;
        self
    }

    /// Ticks the row as the choice in effect.
    pub(crate) fn checked(mut self, checked: bool) -> Self {
        self.checked = checked;
        self
    }

    /// Sets what the row does.
    pub(crate) fn on_activate(mut self, run: impl Fn(&mut Window, &mut App) + 'static) -> Self {
        self.run = Some(Rc::new(run));
        self
    }

    /// The row's words, or the empty string for a separator.
    #[cfg(test)]
    pub(crate) fn label(&self) -> &str {
        self.label.as_ref().map_or("", SharedString::as_ref)
    }

    /// Whether the row can be run.
    #[cfg(test)]
    pub(crate) fn is_enabled(&self) -> bool {
        self.enabled
    }

    /// Whether the row is ticked.
    #[cfg(test)]
    pub(crate) fn is_checked(&self) -> bool {
        self.checked
    }

    /// Runs the row, as clicking it would.
    ///
    /// Panics on a row the menu would not have let the user click: a test that
    /// activated a greyed row would be asserting about a path the interface
    /// does not have.
    #[cfg(test)]
    pub(crate) fn activate(&self, window: &mut Window, cx: &mut App) {
        assert!(self.enabled, "the row {:?} is greyed out", self.label());
        if let Some(run) = &self.run {
            run(window, cx);
        }
    }
}

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

/// The rows as the widget wants them.
pub(crate) fn entries(rows: Vec<MenuRow>) -> Vec<MenuEntry> {
    rows.into_iter()
        .map(|row| {
            let Some(label) = row.label else {
                return MenuEntry::separator();
            };
            let mut entry = MenuEntry::new(label)
                .disabled(!row.enabled)
                .checked(row.checked);
            if let Some(shortcut) = row.shortcut {
                entry = entry.shortcut(shortcut);
            }
            if let Some(run) = row.run {
                entry = entry.on_activate(move |window, cx| run(window, cx));
            }
            entry
        })
        .collect()
}

/// The rows' words, in order, for a test asserting what a surface offers.
///
/// Separators come out as empty strings, so that the groups a menu is divided
/// into are part of what the assertion pins down.
#[cfg(test)]
pub(crate) fn labels(rows: &[MenuRow]) -> Vec<String> {
    rows.iter().map(|row| row.label().to_owned()).collect()
}

/// The words of the rows that are greyed out, in order.
///
/// Separators are not among them however they are built: a rule is not a
/// command that happens to be unavailable, and a test asserting "nothing here
/// is greyed" should not have to say so.
#[cfg(test)]
pub(crate) fn greyed(rows: &[MenuRow]) -> Vec<String> {
    rows.iter()
        .filter(|row| row.label.is_some() && !row.enabled)
        .map(|row| row.label().to_owned())
        .collect()
}

/// The row whose label is `label`, for a test that means to run one.
///
/// By label rather than by index so that inserting a row above the one a test
/// is about does not silently point it at a different command.
#[cfg(test)]
pub(crate) fn row<'a>(rows: &'a [MenuRow], label: &str) -> &'a MenuRow {
    rows.iter()
        .find(|row| row.label() == label)
        .unwrap_or_else(|| panic!("no {label:?} row in {:?}", labels(rows)))
}
