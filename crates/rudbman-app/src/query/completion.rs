//! Query editor popup and asynchronous, lazily fetched JDBC metadata.
use gpui::{
    Anchor, AnchoredPositionMode, Global, KeyBinding, MouseButton, Task, actions, anchored,
    deferred, point,
};
use rudbman_jdbc::DescribeRequest;
use rudbman_sql::completion::{self, Candidate, Kind, Relation, Request};
use rugpui_editor::NavKey;

use super::*;

const ROWS: usize = 8;
const CACHE_AGE: Duration = Duration::from_secs(30);
actions!(query_completion, [CompleteSql]);
struct Bindings;
impl Global for Bindings {}

pub(super) fn init(cx: &mut App) {
    if !cx.has_global::<Bindings>() {
        cx.bind_keys([KeyBinding::new(
            "ctrl-space",
            CompleteSql,
            Some("QueryEditor"),
        )]);
        cx.set_global(Bindings);
    }
}

struct Popup {
    request: Request,
    items: Vec<Candidate>,
    selected: usize,
}

/// Reuse the editor's cached statement boundaries so a keystroke does not copy
/// and lex an entire SQL script. Offsets are translated back before insertion.
#[derive(PartialEq, Eq)]
struct Snapshot {
    start: usize,
    text: String,
    selection: std::ops::Range<usize>,
}

impl Snapshot {
    fn read(editor: &EditorView) -> Self {
        let selection = editor.selection();
        let caret = editor.caret();
        let range = editor.statement_at_caret().map_or_else(
            || caret.saturating_sub(editor.line_before_caret().len())..caret,
            |span| span.start.min(selection.start)..span.end.max(selection.end),
        );
        Self {
            start: range.start,
            text: editor.text_in(range),
            selection,
        }
    }

    fn request(&self, dialect: Dialect, explicit: bool) -> Option<Request> {
        let selection = self.selection.start.checked_sub(self.start)?
            ..self.selection.end.checked_sub(self.start)?;
        let mut request = completion::request(&self.text, selection, dialect, explicit)?;
        request.replace.start += self.start;
        request.replace.end += self.start;
        Some(request)
    }
}

#[derive(Default)]
pub(super) struct CompletionState {
    popup: Option<Popup>,
    pub(super) tables: Vec<Relation>,
    loaded: Option<Instant>,
    loading: bool,
    pub(super) wanted: bool,
    pub(super) explicit: bool,
    dismissed: Option<Snapshot>,
    debounce: Option<Task<()>>,
}

impl QueryPane {
    pub(super) fn trigger_completion(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.context_menu = None;
        self.completion.dismissed = None;
        self.refresh_completion(true, window, cx);
    }

    pub(super) fn refresh_completion(
        &mut self,
        explicit: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let composing = self.editor.update(cx, |editor, cx| {
            gpui::EntityInputHandler::marked_text_range(editor, window, cx).is_some()
        });
        if composing {
            self.close_completion(cx);
            // Committing IME text can keep the exact same bytes and caret.
            // That commit is eligible even though its marked form was not.
            self.completion.dismissed = None;
            return;
        }
        let editor = self.editor.read(cx);
        let snapshot = Snapshot::read(editor);
        if !editor.focus_handle(cx).is_focused(window)
            || self.context_menu.is_some()
            || self
                .completion
                .dismissed
                .as_ref()
                .is_some_and(|old| old == &snapshot)
        {
            self.close_completion(cx);
            return;
        }
        let Some(request) = snapshot.request(self.dialect, explicit) else {
            self.close_completion(cx);
            return;
        };
        self.completion.wanted = true;
        self.completion.explicit = explicit;
        let items = request.candidates(self.dialect, &self.completion.tables);
        let selected = self
            .completion
            .popup
            .as_ref()
            .and_then(|old| old.items.get(old.selected))
            .and_then(|old| items.iter().position(|item| item == old))
            .unwrap_or(0);
        let needs_metadata = self.session.is_some()
            && (self
                .completion
                .loaded
                .is_none_or(|loaded| loaded.elapsed() >= CACHE_AGE)
                || request
                    .column_tables(&self.completion.tables)
                    .iter()
                    .any(|&i| self.completion.tables[i].columns.is_none()));
        self.completion.popup = (!items.is_empty()).then_some(Popup {
            request,
            items,
            selected,
        });
        self.editor.update(cx, |editor, _cx| {
            editor.set_intercept(self.completion.popup.is_some())
        });
        if needs_metadata && !self.completion.loading {
            self.completion.debounce = Some(cx.spawn_in(window, async move |pane, cx| {
                cx.background_executor()
                    .timer(Duration::from_millis(150))
                    .await;
                pane.update_in(cx, |pane, window, cx| {
                    pane.load_completion_metadata(window, cx)
                })
                .ok();
            }));
        }
        cx.notify();
    }

    fn load_completion_metadata(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.completion.wanted || self.completion.loading {
            return;
        }
        let Some(session) = self.session.clone() else {
            return;
        };
        let editor = self.editor.read(cx);
        let Some(request) = Snapshot::read(editor).request(self.dialect, self.completion.explicit)
        else {
            return;
        };
        let mut tables = self.completion.tables.clone();
        let reload = self
            .completion
            .loaded
            .is_none_or(|loaded| loaded.elapsed() >= CACHE_AGE);
        self.completion.loading = true;
        cx.spawn_in(window, async move |pane, cx| {
            let tables = cx
                .background_spawn(async move {
                    if reload
                        && let Ok(result) =
                            session.session().describe(&DescribeRequest::new("tables"))
                    {
                        tables = result
                            .items
                            .into_iter()
                            .filter_map(|item| {
                                Some(Relation {
                                    catalog: string(&item, "catalog"),
                                    schema: string(&item, "schema"),
                                    name: string(&item, "name")?,
                                    columns: None,
                                })
                            })
                            .collect();
                    }
                    for i in request.column_tables(&tables) {
                        let table = &mut tables[i];
                        if table.columns.is_some() {
                            continue;
                        }
                        let mut describe = DescribeRequest::new("columns").with_table(&table.name);
                        describe.catalog = table.catalog.clone();
                        describe.schema = table.schema.clone();
                        table.columns = Some(
                            session
                                .session()
                                .describe(&describe)
                                .map(|result| {
                                    result
                                        .items
                                        .iter()
                                        .filter_map(|item| string(item, "name"))
                                        .collect()
                                })
                                .unwrap_or_default(),
                        );
                    }
                    tables
                })
                .await;
            pane.update_in(cx, |pane, window, cx| {
                pane.completion.loading = false;
                if pane.session.is_none() {
                    return;
                }
                pane.completion.tables = tables;
                pane.completion.loaded = Some(Instant::now());
                // Closing the popup or moving focus while JDBC was answering
                // must never reopen it. Recompute against the current text.
                if pane.completion.wanted {
                    pane.refresh_completion(pane.completion.explicit, window, cx);
                }
            })
            .ok();
        })
        .detach();
    }

    pub(super) fn close_completion(&mut self, cx: &mut Context<Self>) {
        let editor = self.editor.read(cx);
        self.completion.dismissed = Some(Snapshot::read(editor));
        self.completion.wanted = false;
        self.completion.debounce = None;
        if self.completion.popup.take().is_some() {
            self.editor
                .update(cx, |editor, _cx| editor.set_intercept(false));
            cx.notify();
        }
    }

    pub(super) fn completion_key(&mut self, key: NavKey, cx: &mut Context<Self>) {
        let Some(open) = &mut self.completion.popup else {
            return;
        };
        match key {
            NavKey::Up => {
                open.selected = open.selected.checked_sub(1).unwrap_or(open.items.len() - 1)
            }
            NavKey::Down => open.selected = (open.selected + 1) % open.items.len(),
            NavKey::Enter | NavKey::Tab => {
                let selected = open.selected;
                self.accept_completion(selected, cx);
            }
            NavKey::Escape => self.close_completion(cx),
        }
        cx.notify();
    }

    fn accept_completion(&mut self, index: usize, cx: &mut Context<Self>) {
        let Some(open) = &self.completion.popup else {
            return;
        };
        let Some(item) = open.items.get(index) else {
            return;
        };
        let range = open.request.replace.clone();
        let insert = item.insert.clone();
        self.editor
            .update(cx, |editor, cx| editor.replace_range(range, &insert, cx));
        self.close_completion(cx);
    }

    pub(super) fn render_completion(
        &self,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Option<impl IntoElement + use<>> {
        if !self.editor.read(cx).focus_handle(cx).is_focused(window) {
            return None;
        }
        let open = self.completion.popup.as_ref()?;
        let anchor = self.editor.read(cx).caret_bounds()?;
        let chrome = theme(cx);
        let first = open.selected.saturating_sub(ROWS - 1);
        let rows = open
            .items
            .iter()
            .enumerate()
            .skip(first)
            .take(ROWS)
            .map(|(i, item)| {
                let kind = match item.kind {
                    Kind::Column => ts!("completion.column"),
                    Kind::Table => ts!("completion.table"),
                    Kind::Schema => ts!("completion.schema"),
                    Kind::Keyword => ts!("completion.keyword"),
                };
                div()
                    .id(("sql-completion", i))
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .h(px(26.))
                    .px(px(8.))
                    .cursor_pointer()
                    .when(i == open.selected, |row| row.bg(chrome.surface_hover))
                    .hover(|style| style.bg(chrome.surface_hover))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |pane, _, _window, cx| {
                            cx.stop_propagation();
                            pane.accept_completion(i, cx);
                        }),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_color(chrome.text)
                            .child(item.label.clone()),
                    )
                    .child(
                        div()
                            .max_w(px(130.))
                            .truncate()
                            .text_color(chrome.text_muted)
                            .child(item.detail.clone()),
                    )
                    .child(div().text_color(chrome.text_muted).child(kind))
            })
            .collect::<Vec<_>>();
        let list = div()
            .id("sql-completion-popup")
            .debug_selector(|| "sql-completion-popup".to_owned())
            .occlude()
            .flex()
            .flex_col()
            .w(px(380.))
            .py(px(3.))
            .rounded_md()
            .bg(chrome.surface)
            .border_1()
            .border_color(chrome.border)
            .shadow_md()
            .text_size(px(11.))
            .on_mouse_down_out(cx.listener(|pane, _, _window, cx| pane.close_completion(cx)))
            .children(rows)
            .child(
                div()
                    .px(px(8.))
                    .pt(px(4.))
                    .border_t_1()
                    .border_color(chrome.border)
                    .text_color(chrome.text_muted)
                    .text_size(px(10.))
                    .child(ts!("completion.hint")),
            );
        Some(
            div().absolute().size_0().child(
                deferred(
                    anchored()
                        .position(point(anchor.left(), anchor.bottom()))
                        .position_mode(AnchoredPositionMode::Window)
                        .anchor(Anchor::TopLeft)
                        .snap_to_window_with_margin(px(8.))
                        .child(list),
                )
                .with_priority(2),
            ),
        )
    }
}

#[cfg(test)]
mod tests;

fn string(item: &serde_json::Map<String, serde_json::Value>, key: &str) -> Option<String> {
    item.get(key)
        .and_then(serde_json::Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
}
