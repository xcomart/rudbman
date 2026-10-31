//! The query history dialog: what the connection on screen has run, to search,
//! copy, delete and open again.
//!
//! Opened with `Ctrl`/`Cmd`+`Shift`+`H`, the ☰ menu or a connection tab's
//! menu, over the connection tab on screen, and reading that tab's *profile*'s
//! history ([`crate::query_history`]) — so it lists what every tab on the
//! profile ran, this session and earlier ones. The search field takes the
//! keyboard; `↑`/`↓` move through the list without leaving it, `Enter` opens
//! the highlighted statement in a new query tab, and a right-click or
//! `Shift+F10` on a row offers the same as the buttons.
//!
//! A new query tab rather than the editor already open: a history entry is a
//! statement to look at again, and writing it into a tab that holds other work
//! would be an edit nobody asked for. Opening it runs nothing.

use super::*;
use crate::query_history::{HistoryEntry, Outcome, QueryHistory, local_time};
use rugpui::TextInput;

/// Height of one row of the list, before the interface size scales it.
const ROW_HEIGHT: f32 = 44.;

/// Rows the list shows before it scrolls.
const VISIBLE_ROWS: f32 = 8.;

/// The dialog, while it is open.
pub(super) struct HistoryView {
    /// The profile whose history this is.
    profile: Uuid,
    /// The connection tab it was opened over, whose session an opened entry
    /// runs on.
    connection: ConnectionId,
    /// The connection's name, for the title.
    name: SharedString,
    search: Entity<TextInput>,
    /// Every entry, newest first, as the store last answered.
    entries: Vec<HistoryEntry>,
    /// The highlighted row, as an index into what the search lets through.
    selected: usize,
    scroll: gpui::UniformListScrollHandle,
    /// Where the list was drawn last, for the menu `Shift+F10` opens over it.
    list_bounds: std::rc::Rc<std::cell::Cell<gpui::Bounds<Pixels>>>,
    /// Whether "delete the whole history" is waiting to be confirmed.
    confirm_clear: bool,
    /// Keeps the highlight on the first row whenever the search changes.
    _search_changed: Subscription,
}

impl HistoryView {
    /// The entries the search lets through, by index into
    /// [`HistoryView::entries`]: every one whose SQL holds what was typed,
    /// whatever the case.
    fn visible(&self, cx: &App) -> Vec<usize> {
        let needle = self.search.read(cx).content().trim().to_lowercase();
        self.entries
            .iter()
            .enumerate()
            .filter(|(_, entry)| needle.is_empty() || entry.sql.to_lowercase().contains(&needle))
            .map(|(index, _)| index)
            .collect()
    }

    /// The highlighted entry, if the search lets any through.
    fn selected_entry(&self, cx: &App) -> Option<&HistoryEntry> {
        let visible = self.visible(cx);
        let index = *visible.get(self.selected.min(visible.len().saturating_sub(1)))?;
        self.entries.get(index)
    }
}

impl Workspace {
    /// `Ctrl`/`Cmd`+`Shift`+`H` and the menu rows.
    pub(super) fn show_query_history_action(
        &mut self,
        _: &ShowQueryHistory,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.set_menu_open(false, cx);
        self.open_history(window, cx);
    }

    /// Opens the dialog over the connection tab on screen.
    pub(super) fn open_history(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(open) = self.active_connection() else {
            rugpui_shell::show_toast(rugpui_shell::Toast::info(ts!("toast.no_connection")), cx);
            return;
        };
        let profile = open.profile.id;
        let connection = open.id;
        let name = closing::connection_title(&open.profile);
        if !self.close_overlays(window, cx) {
            return;
        }
        let this = cx.weak_entity();
        let search = cx.new(|cx| {
            TextInput::new(cx)
                .placeholder(ts!("history.search"))
                .on_submit(move |_, window, cx| {
                    // The field is still borrowed while its submit handler
                    // runs, and opening a tab moves the focus out of it.
                    let this = this.clone();
                    window.defer(cx, move |window, cx| {
                        this.update(cx, |workspace, cx| {
                            workspace.open_history_entry(None, window, cx)
                        })
                        .ok();
                    });
                })
        });
        let search_changed = cx.observe(&search, |workspace, _, cx| {
            if let Some(history) = &mut workspace.history {
                history.selected = 0;
                history.scroll.scroll_to_item(0, gpui::ScrollStrategy::Top);
            }
            cx.notify();
        });
        window.focus(&search.read(cx).focus_handle(cx), cx);
        self.history = Some(HistoryView {
            profile,
            connection,
            name,
            search,
            entries: QueryHistory::entries(cx, profile),
            selected: 0,
            scroll: gpui::UniformListScrollHandle::new(),
            list_bounds: std::rc::Rc::default(),
            confirm_clear: false,
            _search_changed: search_changed,
        });
        cx.notify();
    }

    /// Closes the dialog and gives the keyboard back.
    pub(super) fn close_history(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.history.take().is_some() {
            if matches!(
                self.context_menu.as_ref().map(|menu| &menu.target),
                Some(ContextTarget::History(_))
            ) {
                self.context_menu = None;
            }
            self.restore_focus(window, cx);
            cx.notify();
        }
    }

    /// `Escape` over the dialog: the clear question first, then the dialog.
    pub(super) fn escape_history(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(history) = &mut self.history
            && history.confirm_clear
        {
            history.confirm_clear = false;
            cx.notify();
            return;
        }
        self.close_history(window, cx);
    }

    /// Moves the highlight `delta` rows, within what the search lets through.
    fn move_history_selection(&mut self, delta: isize, cx: &mut Context<Self>) {
        let Some(history) = &mut self.history else {
            return;
        };
        let count = history.visible(cx).len();
        if count == 0 {
            return;
        }
        let current = history.selected.min(count - 1) as isize;
        let next = (current + delta).clamp(0, count as isize - 1) as usize;
        history.selected = next;
        history
            .scroll
            .scroll_to_item(next, gpui::ScrollStrategy::Nearest);
        cx.notify();
    }

    /// Opens entry `id`, or the highlighted one, in a new query tab, and
    /// closes the dialog.
    ///
    /// Only over a live session — the new tab runs on it — and only on the
    /// connection the dialog was opened over, which nothing could have moved
    /// away from while it was up.
    pub(super) fn open_history_entry(
        &mut self,
        id: Option<u64>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(history) = &self.history else {
            return;
        };
        let entry = match id {
            Some(id) => history.entries.iter().find(|entry| entry.id == id),
            None => history.selected_entry(cx),
        };
        let Some(sql) = entry.map(|entry| entry.sql.clone()) else {
            return;
        };
        if self.session_of(history.connection).is_none() {
            self.say_not_live(cx);
            return;
        }
        self.history = None;
        self.context_menu = None;
        // Over the area the dialog came from: the new tab takes the keyboard
        // itself, so there is nothing to give back.
        self.focus_origin = FocusOrigin::Shell;
        self.open_query(&sql, window, cx);
        cx.notify();
    }

    /// Copies entry `id`'s SQL, or the highlighted entry's.
    fn copy_history_entry(&mut self, id: Option<u64>, cx: &mut Context<Self>) {
        let Some(history) = &self.history else {
            return;
        };
        let entry = match id {
            Some(id) => history.entries.iter().find(|entry| entry.id == id),
            None => history.selected_entry(cx),
        };
        if let Some(entry) = entry {
            cx.write_to_clipboard(gpui::ClipboardItem::new_string(entry.sql.clone()));
        }
    }

    /// Deletes entry `id`, or the highlighted one, from the history.
    fn delete_history_entry(&mut self, id: Option<u64>, cx: &mut Context<Self>) {
        let Some(history) = &self.history else {
            return;
        };
        let Some(id) = id.or_else(|| history.selected_entry(cx).map(|entry| entry.id)) else {
            return;
        };
        let profile = history.profile;
        QueryHistory::delete(cx, profile, id);
        let entries = QueryHistory::entries(cx, profile);
        if let Some(history) = &mut self.history {
            history.entries = entries;
            let count = history.visible(cx).len();
            history.selected = history.selected.min(count.saturating_sub(1));
        }
        cx.notify();
    }

    /// Deletes the profile's whole history, once confirmed.
    fn clear_history(&mut self, cx: &mut Context<Self>) {
        let Some(history) = &mut self.history else {
            return;
        };
        QueryHistory::clear(cx, history.profile);
        history.entries.clear();
        history.selected = 0;
        history.confirm_clear = false;
        cx.notify();
    }

    /// The menu of one row: what the buttons under the list do, for that row.
    pub(super) fn history_rows(&self, id: u64, cx: &mut Context<Self>) -> Vec<MenuRow> {
        let this = cx.entity();
        let live = self
            .history
            .as_ref()
            .is_some_and(|history| self.session_of(history.connection).is_some());
        let act = |run: fn(&mut Workspace, u64, &mut Window, &mut Context<Workspace>)| {
            let this = this.clone();
            move |window: &mut Window, cx: &mut App| {
                this.update(cx, |workspace, cx| run(workspace, id, window, cx));
            }
        };
        vec![
            MenuRow::new(ts!("history.open"))
                .shortcut("Enter")
                .enabled(live)
                .on_activate(act(|workspace, id, window, cx| {
                    workspace.open_history_entry(Some(id), window, cx);
                })),
            MenuRow::new(ts!("context.copy_sql")).on_activate(act(|workspace, id, _, cx| {
                workspace.copy_history_entry(Some(id), cx);
            })),
            MenuRow::separator(),
            MenuRow::new(ts!("history.delete")).on_activate(act(|workspace, id, _, cx| {
                workspace.delete_history_entry(Some(id), cx);
            })),
        ]
    }

    /// Opens a row's menu, as a right click or `Shift+F10` asked.
    fn open_history_menu(
        &mut self,
        id: u64,
        position: Point<Pixels>,
        by_keyboard: bool,
        cx: &mut Context<Self>,
    ) {
        self.context_menu = Some(OpenContextMenu {
            target: ContextTarget::History(id),
            position,
            by_keyboard,
        });
        cx.notify();
    }

    /// The dialog, while it is open.
    pub(super) fn render_history(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let history = self.history.as_ref()?;
        let theme = theme(cx);
        let this = cx.entity();
        let visible = history.visible(cx);
        let selected = history.selected.min(visible.len().saturating_sub(1));
        let entry = visible
            .get(selected)
            .and_then(|index| history.entries.get(*index))
            .cloned();
        let live = self.session_of(history.connection).is_some();
        let row_height = rugpui::scaled(ROW_HEIGHT, cx);

        let list = if visible.is_empty() {
            div()
                .flex()
                .items_center()
                .justify_center()
                .h(row_height * VISIBLE_ROWS)
                .text_size(rugpui::TextSize::Sm.size(cx))
                .text_color(theme.text_muted)
                .child(if history.entries.is_empty() {
                    ts!("history.empty")
                } else {
                    ts!("history.no_match")
                })
                .into_any_element()
        } else {
            let rows_of = this.clone();
            let bounds = history.list_bounds.clone();
            div()
                .relative()
                .h(row_height * VISIBLE_ROWS)
                .border_1()
                .border_color(theme.border)
                .rounded_sm()
                .child(
                    gpui::uniform_list("history-list", visible.len(), move |range, _window, cx| {
                        let workspace = rows_of.read(cx);
                        let Some(history) = &workspace.history else {
                            return Vec::new();
                        };
                        let visible = history.visible(cx);
                        let theme = rugpui::theme(cx);
                        range
                            .filter_map(|row| {
                                let entry = history.entries.get(*visible.get(row)?)?;
                                Some(history_row(
                                    &rows_of,
                                    row,
                                    entry,
                                    row == selected,
                                    &theme,
                                    cx,
                                ))
                            })
                            .collect()
                    })
                    .track_scroll(&history.scroll)
                    .size_full(),
                )
                .child(
                    gpui::canvas(move |drawn, _, _| bounds.set(drawn), |_, _, _, _| {})
                        .absolute()
                        .size_full(),
                )
                .into_any_element()
        };

        // The whole statement, and how it ended when it did not end well:
        // the row shows one line of each.
        let preview = entry.as_ref().map(|entry| {
            let failure = match &entry.outcome {
                Outcome::Failed { message } => Some(message.clone()),
                _ => None,
            };
            div()
                .id("history-preview")
                .flex()
                .flex_col()
                .gap(px(4.))
                .max_h(rugpui::scaled(140., cx))
                .overflow_y_scroll()
                .p(px(8.))
                .rounded_sm()
                .bg(theme.surface)
                .text_size(rugpui::TextSize::Sm.size(cx))
                .child(
                    div()
                        .font_family(crate::app_settings::monospace_family(cx))
                        .text_color(theme.text)
                        .whitespace_normal()
                        .child(SharedString::from(entry.sql.clone())),
                )
                .children(failure.map(|message| {
                    div()
                        .text_color(theme.danger)
                        .child(SharedString::from(message))
                }))
        });

        let footer = if history.confirm_clear {
            div()
                .flex()
                .items_center()
                .gap(px(8.))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_size(rugpui::TextSize::Sm.size(cx))
                        .text_color(theme.danger)
                        .child(ts!("history.clear_question", count = history.entries.len())),
                )
                .child(
                    Button::new("history-clear-cancel", ts!("common.cancel"))
                        .variant(ButtonVariant::Secondary)
                        .on_click({
                            let this = this.clone();
                            move |_, _, cx| {
                                this.update(cx, |workspace, cx| {
                                    if let Some(history) = &mut workspace.history {
                                        history.confirm_clear = false;
                                    }
                                    cx.notify();
                                });
                            }
                        }),
                )
                .child(
                    Button::new("history-clear-confirm", ts!("history.clear_confirm"))
                        .variant(ButtonVariant::Danger)
                        .on_click({
                            let this = this.clone();
                            move |_, _, cx| {
                                this.update(cx, |workspace, cx| workspace.clear_history(cx));
                            }
                        }),
                )
        } else {
            let some = entry.is_some();
            div()
                .flex()
                .items_center()
                .gap(px(8.))
                .child(
                    Button::new("history-clear", ts!("history.clear"))
                        .variant(ButtonVariant::Secondary)
                        .disabled(history.entries.is_empty())
                        .on_click({
                            let this = this.clone();
                            move |_, _, cx| {
                                this.update(cx, |workspace, cx| {
                                    if let Some(history) = &mut workspace.history {
                                        history.confirm_clear = true;
                                    }
                                    cx.notify();
                                });
                            }
                        }),
                )
                .child(div().flex_1())
                .child(
                    Button::new("history-delete", ts!("history.delete"))
                        .variant(ButtonVariant::Secondary)
                        .disabled(!some)
                        .on_click({
                            let this = this.clone();
                            move |_, _, cx| {
                                this.update(cx, |workspace, cx| {
                                    workspace.delete_history_entry(None, cx);
                                });
                            }
                        }),
                )
                .child(
                    Button::new("history-copy", ts!("context.copy_sql"))
                        .variant(ButtonVariant::Secondary)
                        .disabled(!some)
                        .on_click({
                            let this = this.clone();
                            move |_, _, cx| {
                                this.update(cx, |workspace, cx| {
                                    workspace.copy_history_entry(None, cx);
                                });
                            }
                        }),
                )
                .child(
                    Button::new("history-close", ts!("common.close"))
                        .variant(ButtonVariant::Secondary)
                        .on_click({
                            let this = this.clone();
                            move |_, window, cx| {
                                this.update(cx, |workspace, cx| {
                                    workspace.close_history(window, cx)
                                });
                            }
                        }),
                )
                .child(
                    Button::new("history-open", ts!("history.open"))
                        .disabled(!(some && live))
                        .on_click({
                            let this = this.clone();
                            move |_, window, cx| {
                                this.update(cx, |workspace, cx| {
                                    workspace.open_history_entry(None, window, cx);
                                });
                            }
                        }),
                )
        };

        let where_kept = QueryHistory::file_for(cx, history.profile).map(|path| {
            div()
                .text_size(rugpui::TextSize::Xs.size(cx))
                .text_color(theme.text_muted)
                .child(ts!("history.kept_in", path = path.display().to_string()))
        });
        let recording = crate::app_settings::current(cx).query_history;

        let body = div()
            .id("history-body")
            .key_context("HistoryDialog")
            .flex()
            .flex_col()
            .gap(px(10.))
            .on_action(cx.listener(|workspace, _: &rugpui::text_input::Up, _, cx| {
                workspace.move_history_selection(-1, cx);
            }))
            .on_action(
                cx.listener(|workspace, _: &rugpui::text_input::Down, _, cx| {
                    workspace.move_history_selection(1, cx);
                }),
            )
            .on_action(
                cx.listener(|workspace, _: &rugpui::OpenContextMenu, _, cx| {
                    let Some(history) = &workspace.history else {
                        return;
                    };
                    let Some(id) = history.selected_entry(cx).map(|entry| entry.id) else {
                        return;
                    };
                    let position = history.list_bounds.get().origin + gpui::point(px(16.), px(16.));
                    workspace.open_history_menu(id, position, true, cx);
                }),
            )
            .child(history.search.clone())
            .child(list)
            .children(preview)
            .children((!recording).then(|| {
                div()
                    .text_size(rugpui::TextSize::Xs.size(cx))
                    .text_color(theme.text_muted)
                    .child(ts!("history.recording_off"))
            }))
            .children(where_kept)
            .child(footer);

        let dismiss = this.clone();
        Some(
            modal(
                "history-dialog",
                ts!("history.title", name = history.name.to_string()),
                rugpui::scaled(680., cx),
                body,
                move |window, cx| {
                    dismiss.update(cx, |workspace, cx| workspace.close_history(window, cx));
                },
            )
            .into_any_element(),
        )
    }
}

/// One row of the list: when it ran, how it ended and how long it took above,
/// the statement's first line below.
fn history_row(
    workspace: &Entity<Workspace>,
    row: usize,
    entry: &HistoryEntry,
    selected: bool,
    theme: &Theme,
    cx: &App,
) -> AnyElement {
    let status = match &entry.outcome {
        Outcome::Rows { count, more: true } => ts!("query.row_count_more", count = count),
        Outcome::Rows { count, more: false } => ts!("query.row_count", count = count),
        Outcome::Affected { count } => ts!("query.rows_affected", count = count),
        Outcome::Done => ts!("history.done"),
        Outcome::Failed { .. } => ts!("history.failed"),
        Outcome::Canceled => ts!("history.canceled"),
    };
    let color = entry.outcome.is_failure().then_some(theme.danger);
    let first_line: String = entry
        .sql
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or_default()
        .chars()
        .take(300)
        .collect();
    let id = entry.id;
    let select = workspace.clone();
    let menu = workspace.clone();
    div()
        .id(("history-row", id))
        .flex()
        .flex_col()
        .justify_center()
        .gap(px(2.))
        .h(rugpui::scaled(ROW_HEIGHT, cx))
        .px(px(10.))
        .border_b_1()
        .border_color(theme.border)
        .cursor_pointer()
        .when(selected, |row| row.bg(theme.accent.opacity(0.18)))
        .when(!selected, |row| {
            row.hover(|style| style.bg(theme.surface_hover))
        })
        .on_click(move |event, window, cx| {
            select.update(cx, |workspace, cx| {
                if let Some(history) = &mut workspace.history {
                    history.selected = row;
                }
                if event.click_count() >= 2 {
                    workspace.open_history_entry(Some(id), window, cx);
                }
                cx.notify();
            });
        })
        .on_mouse_down(gpui::MouseButton::Right, move |event, _window, cx| {
            let position = event.position;
            menu.update(cx, |workspace, cx| {
                if let Some(history) = &mut workspace.history {
                    history.selected = row;
                }
                workspace.open_history_menu(id, position, false, cx);
            });
        })
        .child(
            div()
                .flex()
                .items_center()
                .gap(px(10.))
                .text_size(rugpui::TextSize::Xs.size(cx))
                .text_color(theme.text_muted)
                .child(SharedString::from(local_time(entry.at)))
                .child(
                    div()
                        .when_some(color, |status, color| status.text_color(color))
                        .child(status),
                )
                .child(elapsed(entry.elapsed_ms)),
        )
        .child(
            div()
                .overflow_hidden()
                .whitespace_nowrap()
                .text_ellipsis()
                .font_family(crate::app_settings::monospace_family(cx))
                .text_size(rugpui::TextSize::Sm.size(cx))
                .text_color(theme.text)
                .child(SharedString::from(first_line)),
        )
        .into_any_element()
}

/// How long an entry took, in the seconds the query pane's status bar uses.
fn elapsed(milliseconds: u64) -> SharedString {
    format!("{:.3} s", milliseconds as f64 / 1000.).into()
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::super::tests::workspace_over_h2;
    use super::*;

    /// Runs `sql` in a fresh query tab and waits for it.
    fn run(window: &gpui::WindowHandle<Workspace>, sql: &str, cx: &mut gpui::VisualTestContext) {
        window
            .update(cx, |workspace, window, cx| {
                let pane = workspace.open_query(sql, window, cx).unwrap();
                pane.update(cx, |pane, cx| {
                    pane.focus_editor(window, cx);
                });
            })
            .unwrap();
        cx.run_until_parked();
        cx.simulate_keystrokes(if cfg!(target_os = "macos") {
            "cmd-shift-enter"
        } else {
            "ctrl-shift-enter"
        });
        cx.run_until_parked();
    }

    /// What the user ran is in the dialog, newest first, searchable; `↓` and
    /// `Enter` open the highlighted statement in a new tab without running
    /// it; a deleted entry is gone and a cleared history is empty.
    #[gpui::test]
    fn the_dialog_lists_searches_opens_and_deletes(cx: &mut gpui::TestAppContext) {
        let directory = tempfile::tempdir().unwrap();
        let (window, _) = workspace_over_h2("history-dialog", cx);
        cx.update(|cx| {
            crate::app_settings::replace(rudbman_core::AppSettings::default(), cx);
            QueryHistory::use_directory(cx, directory.path());
        });
        let mut cx = gpui::VisualTestContext::from_window(window.into(), cx);
        let profile = profile_of(&window, &mut cx);
        run(&window, "create table HISTORY_T (ID int)", &mut cx);
        run(&window, "insert into HISTORY_T values (1)", &mut cx);
        run(&window, "select * from NO_SUCH_TABLE", &mut cx);

        window
            .update(&mut cx, |workspace, window, cx| {
                workspace.open_history(window, cx);
                let history = workspace.history.as_ref().unwrap();
                let sql: Vec<&str> = history.entries.iter().map(|e| e.sql.as_str()).collect();
                assert_eq!(
                    sql,
                    [
                        "select * from NO_SUCH_TABLE",
                        "insert into HISTORY_T values (1)",
                        "create table HISTORY_T (ID int)",
                    ]
                );
                assert!(history.entries[0].outcome.is_failure());
                assert_eq!(history.entries[1].outcome, Outcome::Affected { count: 1 });
                history
                    .search
                    .clone()
                    .update(cx, |input, cx| input.set_content("HISTORY_T", cx));
            })
            .unwrap();
        cx.run_until_parked();
        window
            .update(&mut cx, |workspace, _window, cx| {
                assert_eq!(workspace.history.as_ref().unwrap().visible(cx).len(), 2);
            })
            .unwrap();

        cx.simulate_keystrokes("down enter");
        cx.run_until_parked();
        window
            .update(&mut cx, |workspace, _window, cx| {
                assert!(workspace.history.is_none(), "the dialog stayed open");
                let pane = workspace.active_query().unwrap();
                assert_eq!(
                    pane.read(cx).editor_text(cx),
                    "create table HISTORY_T (ID int)"
                );
                assert!(!pane.read(cx).is_running(), "opening ran the statement");
            })
            .unwrap();

        window
            .update(&mut cx, |workspace, window, cx| {
                workspace.open_history(window, cx);
                workspace.delete_history_entry(None, cx);
                let history = workspace.history.as_ref().unwrap();
                assert_eq!(history.entries.len(), 2);
                assert_eq!(history.entries[0].sql, "insert into HISTORY_T values (1)");
                workspace.clear_history(cx);
                assert!(workspace.history.as_ref().unwrap().entries.is_empty());
                workspace.escape_history(window, cx);
                assert!(workspace.history.is_none());
            })
            .unwrap();
        cx.update(|_, cx| assert!(QueryHistory::entries(cx, profile).is_empty()));
    }

    /// A plan is not written down; the statement run before it is, once,
    /// with the rows it answered.
    #[gpui::test]
    fn only_what_the_user_ran_is_written_down(cx: &mut gpui::TestAppContext) {
        let (window, _) = workspace_over_h2("history-only-runs", cx);
        cx.update(|cx| {
            crate::app_settings::replace(rudbman_core::AppSettings::default(), cx);
            QueryHistory::init(cx);
        });
        let mut cx = gpui::VisualTestContext::from_window(window.into(), cx);
        let profile = profile_of(&window, &mut cx);
        run(&window, "select 2 as N union all select 1", &mut cx);
        window
            .update(&mut cx, |workspace, window, cx| {
                let pane = workspace.active_query().unwrap().clone();
                pane.update(cx, |pane, cx| pane.explain(window, cx));
            })
            .unwrap();
        cx.run_until_parked();
        cx.background_executor.advance_clock(Duration::from_secs(1));
        let entries = cx.update(|_, cx| QueryHistory::entries(cx, profile));
        assert_eq!(
            entries.iter().map(|e| e.sql.as_str()).collect::<Vec<_>>(),
            ["select 2 as N union all select 1"]
        );
        assert_eq!(
            entries[0].outcome,
            Outcome::Rows {
                count: 2,
                more: false
            }
        );
    }

    /// The connection's profile, with "ask before writes" turned off so
    /// that the statements the tests type run without a question.
    fn profile_of(
        window: &gpui::WindowHandle<Workspace>,
        cx: &mut gpui::VisualTestContext,
    ) -> Uuid {
        window
            .update(cx, |workspace, _, _| {
                workspace.connections[0].profile.confirm_writes = false;
                workspace.connections[0].profile.id
            })
            .unwrap()
    }
}
