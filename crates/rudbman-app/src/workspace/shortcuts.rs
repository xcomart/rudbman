//! Shortcuts.
//!
//! The application menu's *Keyboard shortcuts* sheet, and its *User guide*
//! row. The sheet is written from the same constants the bindings and the menu
//! hints are wherever there is one, so the three cannot drift apart, and its
//! descriptions are the menu's and the context menus' own words wherever those
//! have a row for the command.

use super::*;
use crate::query::{COMMIT_KEY, EXPLAIN_KEY, ROLLBACK_KEY};
use rugpui::TextSize;

/// Where *User guide* goes: the manual, as the repository publishes it.
const USER_GUIDE_URL: &str = "https://github.com/xcomart/rudbman#readme";

/// Width of the sheet.
const SHEET_WIDTH: f32 = 600.;

/// Height at which the sheet's list starts scrolling.
const SHEET_MAX_HEIGHT: f32 = 460.;

/// Width of the key column.
const KEY_COLUMN: f32 = 220.;

/// One heading of the sheet and its rows of key and command.
pub(super) type ShortcutGroup = (SharedString, Vec<(SharedString, SharedString)>);

/// The sheet's groups, in the order the guide's own table lists them.
///
/// A chord that only exists on some platforms — `F10`, the close-tab and
/// save-as chords, the connection strip's — is left out where it is not
/// bound, rather than listed with a note.
pub(super) fn shortcut_groups() -> Vec<ShortcutGroup> {
    let key = |text: &str| SharedString::from(text.to_owned());
    let chord = |text: &str| key(&format!("{SHORTCUT_MODIFIER}+{text}"));
    let pane = |text: &str| key(&format!("{PANE_SHORTCUT_LABEL}+{text}"));
    let mac = cfg!(target_os = "macos");

    let mut window = vec![
        (chord("N"), ts!("menu.new_connection")),
        (chord(","), ts!("menu.settings")),
        (chord("Shift+H"), ts!("menu.query_history")),
        (chord("B"), ts!("menu.toggle_explorer")),
    ];
    if !mac {
        window.push((key("F10"), ts!("shortcuts.open_menu")));
    }
    window.extend([
        (key("Shift+F10"), ts!("shortcuts.context_menu")),
        (key("Esc"), ts!("shortcuts.dismiss")),
        (chord("Q"), ts!("menu.quit")),
    ]);

    let mut tabs = vec![(chord("T"), ts!("menu.new_query"))];
    if let Some((_, label)) = CLOSE_TAB_SHORTCUT {
        tabs.push((key(label), ts!("context.close_tab")));
    }
    tabs.extend([
        (chord("Shift+T"), ts!("context.reopen_tab")),
        (
            key(if mac {
                "Cmd+Alt+\u{2192} / \u{2190}"
            } else {
                "Ctrl+PgDn / PgUp"
            }),
            ts!("shortcuts.next_prev_tab"),
        ),
        (
            key("Ctrl+Tab / Ctrl+Shift+Tab"),
            ts!("shortcuts.next_prev_tab"),
        ),
        (chord("1\u{2026}9"), ts!("shortcuts.select_tab")),
    ]);
    if !mac {
        tabs.push((
            key("Ctrl+Alt+PgDn / PgUp"),
            ts!("shortcuts.next_prev_connection"),
        ));
    }
    tabs.extend([
        (pane("Shift+D"), ts!("context.split_right")),
        (pane("Shift+S"), ts!("context.split_below")),
        (pane("W"), ts!("context.close_pane")),
        (pane("]"), ts!("menu.next_pane")),
        (pane("["), ts!("menu.prev_pane")),
        (key("F6 / Shift+F6"), ts!("shortcuts.regions")),
    ]);

    let explorer = vec![
        (chord("Enter"), ts!("menu.query_object")),
        (chord("E"), ts!("menu.erd")),
        (chord("C"), ts!("menu.copy_names")),
        (
            key("Shift+\u{2191} / \u{2193}"),
            ts!("shortcuts.select_range"),
        ),
        (chord("F"), ts!("shortcuts.filter_tree")),
        (key("Alt+R"), ts!("shortcuts.filter_regex")),
    ];

    let mut editor = vec![
        (chord("O"), ts!("menu.open_sql_file")),
        (chord("S"), ts!("menu.save_sql_file")),
    ];
    if let Some((_, label)) = SAVE_AS_SHORTCUT {
        editor.push((key(label), ts!("menu.save_sql_file_as")));
    }
    editor.extend([
        (chord("Enter"), ts!("context.run_statement")),
        (chord("Alt+Enter"), ts!("context.run_selection")),
        (chord("Shift+Enter"), ts!("context.run_all")),
        (key("Esc"), ts!("shortcuts.stop_run")),
        (key(COMMIT_KEY), ts!("query.commit")),
        (key(ROLLBACK_KEY), ts!("query.rollback")),
        (chord("Shift+F"), ts!("context.format_sql")),
        (key(EXPLAIN_KEY), ts!("context.explain")),
        (key("Ctrl+Space"), ts!("completion.trigger")),
        (chord("F"), ts!("context.find")),
        (chord("H"), ts!("context.replace")),
        (chord("/"), ts!("context.toggle_comment")),
    ]);

    let grid = vec![
        (
            key(if mac { "Delete / Backspace" } else { "Delete" }),
            ts!("shortcuts.mark_delete"),
        ),
        (key("F2"), ts!("shortcuts.edit_cell")),
        (chord("V"), ts!("context.paste")),
        (chord("0"), ts!("data.set_null")),
        (key("Shift+Delete"), ts!("data.set_null")),
        (key("Alt+Insert"), ts!("data.insert_row")),
        (chord("S"), ts!("data.apply")),
        (key("F5"), ts!("data.refresh")),
        (chord("F"), ts!("shortcuts.filter_rows")),
        (chord("C"), ts!("context.copy")),
    ];

    let erd = vec![
        (chord("="), ts!("context.zoom_in")),
        (chord("-"), ts!("context.zoom_out")),
        (chord("0"), ts!("context.zoom_reset")),
        (chord("Shift+A"), ts!("erd.auto_arrange")),
        (chord("Shift+L"), ts!("shortcuts.toggle_names")),
    ];

    vec![
        (ts!("shortcuts.group_window"), window),
        (ts!("shortcuts.group_tabs"), tabs),
        (ts!("shortcuts.group_explorer"), explorer),
        (ts!("shortcuts.group_editor"), editor),
        (ts!("shortcuts.group_grid"), grid),
        (ts!("shortcuts.group_erd"), erd),
    ]
}

impl Workspace {
    /// Shows the shortcut sheet, closing whatever else was showing.
    pub(super) fn show_shortcuts(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.close_overlays(window, cx) {
            return;
        }
        self.shortcuts_open = true;
        cx.notify();
    }

    /// Puts the shortcut sheet away, and the keyboard back where it was.
    pub(super) fn close_shortcuts(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if std::mem::take(&mut self.shortcuts_open) {
            self.restore_focus(window, cx);
        }
    }

    /// Opens the user guide in the browser.
    pub(super) fn open_user_guide(&mut self, cx: &mut Context<Self>) {
        cx.open_url(USER_GUIDE_URL);
    }

    /// Renders the shortcut sheet, while it is open.
    pub(super) fn render_shortcuts(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        if !self.shortcuts_open {
            return None;
        }
        let theme = theme(cx);
        let this = cx.entity();
        let groups = shortcut_groups()
            .into_iter()
            .enumerate()
            .map(|(index, (heading, rows))| {
                div()
                    .flex()
                    .flex_col()
                    .gap(px(2.))
                    .when(index > 0, |group| group.pt(px(10.)))
                    .child(
                        div()
                            .pb(px(2.))
                            .text_size(TextSize::Xs.size(cx))
                            .text_color(theme.text_muted)
                            .child(heading),
                    )
                    .children(rows.into_iter().map(|(key, action)| {
                        div()
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap(px(12.))
                            .text_size(TextSize::Sm.size(cx))
                            .child(
                                div()
                                    .flex_none()
                                    .w(rugpui::scaled(KEY_COLUMN, cx))
                                    .text_color(theme.accent)
                                    .child(key),
                            )
                            .child(div().flex_1().min_w_0().child(action))
                    }))
            });
        // Shrinks below its cap in a short window, so the buttons under it
        // stay inside the dialog and the sheet scrolls instead.
        let body = div()
            .flex()
            .flex_col()
            .min_h_0()
            .gap(px(12.))
            .child(
                div()
                    .relative()
                    .flex()
                    .flex_col()
                    .min_h_0()
                    .child(
                        div()
                            .id("shortcut-sheet")
                            .track_scroll(&self.shortcuts_scroll)
                            .flex()
                            .flex_col()
                            .min_h_0()
                            .max_h(rugpui::scaled(SHEET_MAX_HEIGHT, cx))
                            .overflow_y_scroll()
                            .restrict_scroll_to_axis()
                            .children(groups),
                    )
                    .with_scrollbar("shortcut-sheet-scrollbar", &self.shortcuts_scroll),
            )
            .child(
                div()
                    .flex()
                    .flex_row()
                    .justify_end()
                    .gap(px(8.))
                    .child(
                        Button::new("shortcut-sheet-guide", ts!("menu.user_guide"))
                            .variant(ButtonVariant::Secondary)
                            .on_click(cx.listener(|workspace, _: &gpui::ClickEvent, _, cx| {
                                workspace.open_user_guide(cx);
                            })),
                    )
                    .child(
                        Button::new("shortcut-sheet-close", ts!("common.close"))
                            .variant(ButtonVariant::Primary)
                            .focus_handle(&self.shortcuts_focus)
                            .on_click(cx.listener(
                                |workspace, _: &gpui::ClickEvent, window, cx| {
                                    workspace.close_shortcuts(window, cx);
                                },
                            )),
                    ),
            );
        Some(
            modal(
                "shortcut-sheet-modal",
                ts!("menu.shortcuts"),
                rugpui::scaled(SHEET_WIDTH, cx),
                body,
                move |window, cx| {
                    this.update(cx, |workspace, cx| workspace.close_shortcuts(window, cx));
                },
            )
            .initial_focus(&self.shortcuts_focus)
            .escape_dismisses(true)
            .into_any_element(),
        )
    }
}
