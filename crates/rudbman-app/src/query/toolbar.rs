//! Editor commands share the keyboard action path, including query guards.

use super::*;
use crate::icons;
use gpui::{MouseButton, Stateful, Svg};
use rugpui::tooltip_label;

fn tool_button(
    id: &'static str,
    icon: Svg,
    label: SharedString,
    enabled: bool,
    chrome: &Theme,
) -> Stateful<Div> {
    div()
        .id(id)
        .debug_selector(move || id.to_string())
        .flex()
        .flex_none()
        .size(px(26.))
        .items_center()
        .justify_center()
        .rounded_sm()
        .tooltip(tooltip_label(label))
        .when(enabled, |button| {
            button
                .cursor_pointer()
                .hover(|style| style.bg(chrome.surface_hover))
                .on_mouse_down(MouseButton::Left, |_, window, _| window.prevent_default())
        })
        .when(!enabled, |button| button.opacity(0.35))
        .child(icon)
}

impl QueryPane {
    pub(super) fn render_editor_toolbar(
        &self,
        chrome: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let editor = self.editor.read(cx);
        let handle = editor.focus_handle(cx);
        let selected = editor.has_selection();
        let writable = !editor.is_read_only();
        let running = self.is_running();
        let can_run =
            self.session.is_some() && !running && !self.applying && self.transaction.is_none();
        let cancelling = matches!(&self.run, RunState::Running(run) if run.cancelling);
        let modifier = SHORTCUT_MODIFIER;
        let button = |id, icon, label: SharedString, shortcut, enabled, action: Box<dyn Action>| {
            let handle = handle.clone();
            tool_button(
                id,
                icons::icon(icon, px(16.), chrome.icon),
                format!("{label} ({modifier}+{shortcut})").into(),
                enabled,
                chrome,
            )
            .when(enabled, |button| {
                button.on_click(move |_, window, cx| {
                    handle.focus(window, cx);
                    handle.dispatch_action(&*action, window, cx);
                })
            })
            .into_any_element()
        };
        let group = |buttons: Vec<AnyElement>| {
            div()
                .flex()
                .flex_none()
                .items_center()
                .gap(px(2.))
                .pr(px(4.))
                .border_r_1()
                .border_color(chrome.border)
                .children(buttons)
        };
        let this = cx.entity();
        let commit = tool_button(
            "query-commit",
            icons::icon(icons::COMMIT, px(16.), chrome.icon),
            if self.transaction == Some(TransactionAction::Commit) {
                ts!("query.committing")
            } else if self.restore_auto_commit {
                ts!("query.commit_auto")
            } else {
                ts!("query.commit")
            },
            self.can_finish_transaction(),
            chrome,
        )
        .when(self.can_finish_transaction(), |button| {
            button.on_click(move |_, _, cx| {
                this.update(cx, |pane, cx| {
                    pane.finish_transaction(TransactionAction::Commit, cx)
                });
            })
        });
        let this = cx.entity();
        let rollback = tool_button(
            "query-rollback",
            icons::icon(icons::ROLLBACK, px(16.), chrome.icon),
            if self.transaction == Some(TransactionAction::Rollback) {
                ts!("query.rolling_back")
            } else if self.restore_auto_commit {
                ts!("query.rollback_auto")
            } else {
                ts!("query.rollback")
            },
            self.can_finish_transaction(),
            chrome,
        )
        .when(self.can_finish_transaction(), |button| {
            button.on_click(move |_, _, cx| {
                this.update(cx, |pane, cx| {
                    pane.finish_transaction(TransactionAction::Rollback, cx)
                });
            })
        });
        let this = cx.entity();
        let cancel = tool_button(
            "query-cancel",
            icons::icon(icons::STOP, px(16.), chrome.danger),
            if cancelling {
                ts!("query.cancelling")
            } else {
                ts!("query.cancel")
            },
            running && !cancelling,
            chrome,
        )
        .when(running && !cancelling, |button| {
            button.on_click(move |_, _, cx| {
                this.update(cx, |pane, cx| pane.cancel(cx));
            })
        });

        div()
            .id("query-editor-toolbar")
            .flex()
            .flex_wrap()
            .flex_none()
            .items_center()
            .gap(px(4.))
            .px(px(6.))
            .py(px(3.))
            .bg(chrome.surface)
            .border_b_1()
            .border_color(chrome.border)
            .child(group(vec![
                button(
                    "query-run",
                    icons::RUN_STATEMENT,
                    ts!("context.run_statement"),
                    "Enter",
                    can_run,
                    Box::new(RunStatement),
                ),
                button(
                    "query-run-selection",
                    icons::RUN_SELECTION,
                    ts!("context.run_selection"),
                    "Alt+Enter",
                    can_run && selected,
                    Box::new(RunSelection),
                ),
                button(
                    "query-run-all",
                    icons::RUN_ALL,
                    ts!("context.run_all"),
                    "Shift+Enter",
                    can_run,
                    Box::new(RunAll),
                ),
                cancel.into_any_element(),
            ]))
            .child(group(vec![
                commit.into_any_element(),
                rollback.into_any_element(),
            ]))
            .child(group(vec![
                button(
                    "query-undo",
                    icons::UNDO,
                    ts!("context.undo"),
                    "Z",
                    writable && editor.can_undo(),
                    Box::new(Undo),
                ),
                button(
                    "query-redo",
                    icons::REDO,
                    ts!("context.redo"),
                    "Shift+Z",
                    writable && editor.can_redo(),
                    Box::new(Redo),
                ),
            ]))
            .child(group(vec![
                button(
                    "query-cut",
                    icons::CUT,
                    ts!("context.cut"),
                    "X",
                    writable && selected,
                    Box::new(Cut),
                ),
                button(
                    "query-copy",
                    icons::COPY,
                    ts!("context.copy"),
                    "C",
                    selected,
                    Box::new(Copy),
                ),
                button(
                    "query-paste",
                    icons::PASTE,
                    ts!("context.paste"),
                    "V",
                    writable,
                    Box::new(Paste),
                ),
                button(
                    "query-select-all",
                    icons::SELECT_ALL,
                    ts!("context.select_all"),
                    "A",
                    true,
                    Box::new(SelectAll),
                ),
            ]))
            .child(group(vec![
                button(
                    "query-comment",
                    icons::COMMENT,
                    ts!("context.toggle_comment"),
                    "/",
                    writable,
                    Box::new(ToggleComment),
                ),
                button(
                    "query-format",
                    icons::FORMAT,
                    ts!("context.format_sql"),
                    "Shift+F",
                    writable,
                    Box::new(super::format::FormatSql),
                ),
            ]))
            .child(button(
                "query-find",
                icons::FIND,
                ts!("context.find"),
                "F",
                true,
                Box::new(Find),
            ))
            .child(button(
                "query-replace",
                icons::REPLACE,
                ts!("context.replace"),
                "H",
                writable,
                Box::new(Replace),
            ))
            .into_any_element()
    }
}
