//! User-assigned tab names. The dialog keeps identities so another connection
//! finishing its handshake cannot redirect a rename to a different tab.

use super::*;
use rugpui::TextInput;

pub(super) struct RenameTab {
    connection: ConnectionId,
    tab: gpui::EntityId,
    input: Entity<TextInput>,
}

impl Workspace {
    pub(super) fn begin_tab_name(
        &mut self,
        pane: PaneId,
        index: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(open) = self.active_connection() else {
            return;
        };
        let Some(item) = open.work.panes.get(pane).and_then(|pane| pane.get(index)) else {
            return;
        };
        let tab = item.entity_id();
        let connection = open.id;
        let title = open
            .work
            .tab_names
            .get(&tab)
            .cloned()
            .unwrap_or_else(|| item.title(cx).to_string());
        if !self.close_overlays(window, cx) {
            return;
        }
        let this = cx.weak_entity();
        let input = cx.new(|cx| {
            let mut input = TextInput::new(cx).on_submit(move |_, window, cx| {
                // TextInput is still borrowed while its submit handler runs.
                let this = this.clone();
                window.defer(cx, move |window, cx| {
                    this.update(cx, |workspace, cx| {
                        workspace.finish_tab_name(true, window, cx)
                    })
                    .ok();
                });
            });
            input.set_content(title, cx);
            input
        });
        window.focus(&input.read(cx).focus_handle(cx), cx);
        self.rename_tab = Some(RenameTab {
            connection,
            tab,
            input,
        });
        cx.notify();
    }

    pub(super) fn finish_tab_name(
        &mut self,
        save: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(rename) = self.rename_tab.take() else {
            return;
        };
        if save {
            let name = rename.input.read(cx).content().trim().to_string();
            if let Some(open) = self
                .connections
                .iter_mut()
                .find(|open| open.id == rename.connection)
                && open.work.panes.leaves().iter().any(|(_, pane)| {
                    pane.items()
                        .iter()
                        .any(|item| item.entity_id() == rename.tab)
                })
            {
                if name.is_empty() {
                    open.work.tab_names.remove(&rename.tab);
                } else {
                    open.work.tab_names.insert(rename.tab, name.clone());
                }
                // A query pane keeps a copy, for naming itself as the tab a
                // transaction was opened from.
                let query = open
                    .work
                    .queries()
                    .into_iter()
                    .find(|pane| pane.entity_id() == rename.tab);
                if let Some(query) = query {
                    query.update(cx, |pane, _| pane.set_tab_name(Some(name)));
                }
            }
        }
        self.restore_focus(window, cx);
        cx.notify();
    }

    pub(super) fn render_rename_tab(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let rename = self.rename_tab.as_ref()?;
        let this = cx.entity();
        let dismiss = this.clone();
        let theme = theme(cx);
        let body = div()
            .flex()
            .flex_col()
            .gap(px(12.))
            .child(rename.input.clone())
            .child(
                div()
                    .text_size(px(12.))
                    .text_color(theme.text_muted)
                    .child(ts!("tab_name.hint")),
            )
            .child(
                div()
                    .flex()
                    .justify_end()
                    .gap(px(8.))
                    .child(
                        Button::new("tab-name-cancel", ts!("common.cancel"))
                            .variant(ButtonVariant::Secondary)
                            .on_click({
                                let this = this.clone();
                                move |_, window, cx| {
                                    this.update(cx, |workspace, cx| {
                                        workspace.finish_tab_name(false, window, cx)
                                    });
                                }
                            }),
                    )
                    .child(Button::new("tab-name-save", ts!("common.save")).on_click(
                        move |_, window, cx| {
                            this.update(cx, |workspace, cx| {
                                workspace.finish_tab_name(true, window, cx)
                            });
                        },
                    )),
            );
        Some(
            modal(
                "tab-name-dialog",
                ts!("tab_name.title"),
                px(400.),
                body,
                move |window, cx| {
                    dismiss.update(cx, |workspace, cx| {
                        workspace.finish_tab_name(false, window, cx)
                    });
                },
            )
            .into_any_element(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::super::tests::workspace_over_h2;

    #[gpui::test]
    fn enter_saves_a_name_and_cancel_and_blank_have_distinct_meanings(
        cx: &mut gpui::TestAppContext,
    ) {
        let (window, _) = workspace_over_h2("tab-name-dialog", cx);
        let mut cx = gpui::VisualTestContext::from_window(window.into(), cx);
        let tab = window
            .update(&mut cx, |workspace, window, cx| {
                workspace.open_query("select 1", window, cx);
                let tab = workspace.active_query().unwrap().entity_id();
                workspace.begin_tab_name(workspace.active_pane().unwrap(), 0, window, cx);
                workspace
                    .rename_tab
                    .as_ref()
                    .unwrap()
                    .input
                    .update(cx, |input, cx| input.set_content("  매출 분석  ", cx));
                tab
            })
            .unwrap();
        cx.run_until_parked();
        cx.simulate_keystrokes("enter");
        cx.run_until_parked();
        window
            .update(&mut cx, |workspace, window, cx| {
                assert!(workspace.rename_tab.is_none());
                assert_eq!(workspace.work_area().unwrap().tab_names[&tab], "매출 분석");
                workspace.begin_tab_name(workspace.active_pane().unwrap(), 0, window, cx);
                workspace
                    .rename_tab
                    .as_ref()
                    .unwrap()
                    .input
                    .update(cx, |input, cx| input.set_content("discard", cx));
                workspace.finish_tab_name(false, window, cx);
                assert_eq!(workspace.work_area().unwrap().tab_names[&tab], "매출 분석");
                workspace.begin_tab_name(workspace.active_pane().unwrap(), 0, window, cx);
                workspace
                    .rename_tab
                    .as_ref()
                    .unwrap()
                    .input
                    .update(cx, |input, cx| input.set_content("  ", cx));
                workspace.finish_tab_name(true, window, cx);
                assert!(!workspace.work_area().unwrap().tab_names.contains_key(&tab));
            })
            .unwrap();
    }
}
