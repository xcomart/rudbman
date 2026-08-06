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

use gpui::{App, SharedString, Window};
use rudbman_ui::MenuEntry;

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
