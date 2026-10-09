//! Render.

use super::*;

/// Formats an elapsed duration for the status bar.
pub(super) fn elapsed_label(elapsed: Duration) -> SharedString {
    ts!(
        "query.elapsed",
        seconds = format!("{:.1}", elapsed.as_secs_f64())
    )
}

/// The first [`PREVIEW_CHARS`] characters of a statement, with an ellipsis when
/// it was cut.
pub(super) fn preview(sql: &str) -> SharedString {
    let trimmed = sql.trim();
    match trimmed.char_indices().nth(PREVIEW_CHARS) {
        Some((at, _)) => SharedString::from(format!("{}…", &trimmed[..at])),
        None => SharedString::from(trimmed.to_string()),
    }
}

/// A centred line of text, for the states that have no rows to draw.
///
/// Shared with the data pane, which has the same four states to say something
/// in — loading, empty, failed, nothing run yet — and no reason to draw them
/// differently.
pub fn note(text: SharedString, color: Hsla) -> AnyElement {
    div()
        .flex()
        .flex_1()
        .min_w_0()
        .min_h_0()
        .items_center()
        .justify_center()
        .p(px(16.))
        .text_size(px(12.))
        .text_color(color)
        .child(text)
        .into_any_element()
}

/// The error envelope: what failed, its `SQLSTATE`, and a hint when the class
/// affords one.
///
/// Shared with the data pane for the reason [`note`] is: a driver's refusal
/// reads the same wherever the statement came from, and the hint is chosen from
/// the `SQLSTATE` class rather than from anything about the pane.
pub fn render_error(error: &QueryError, chrome: &Theme) -> AnyElement {
    error_lines(error, chrome)
        .flex_1()
        .min_w_0()
        .min_h_0()
        .p(px(16.))
        .into_any_element()
}

/// The lines an error envelope reads as, in a column and nothing else.
///
/// Split out of [`render_error`] because the data pane shows the same envelope
/// somewhere else. A failed load has no rows to draw, so its error stands where
/// they would have been; a failed *apply* leaves the rows — and everything
/// staged against them — exactly where they are, so its error is a strip above
/// them. Same three lines, two placements, one composition.
pub fn error_lines(error: &QueryError, chrome: &Theme) -> Div {
    let state = error
        .sql_state
        .clone()
        .map(|state| ts!("query.sql_state", state = state.to_string()));
    div()
        .flex()
        .flex_col()
        .gap(px(6.))
        .child(
            div()
                .text_size(px(12.))
                .text_color(chrome.danger)
                .child(error.message.clone()),
        )
        .children(state.map(|state| {
            div()
                .text_size(px(11.))
                .text_color(chrome.text_muted)
                .child(state)
        }))
        .children(error.hint().map(|hint| {
            div()
                .text_size(px(11.))
                .text_color(chrome.text_muted)
                .child(hint)
        }))
}

impl Render for QueryPane {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let chrome = theme(cx);
        let fonts = crate::app_settings::effective(cx);
        let share = self.editor_share.clamp(MIN_SHARE, 1. - MIN_SHARE);
        let id = cx.entity_id();
        let toolbar = self.render_toolbar(&chrome, cx);
        let banner = self.render_banner(&chrome);
        let failure = self
            .apply_error
            .as_ref()
            .map(|problem| render_apply_error(problem, &chrome));
        let results = self.render_results(&chrome);
        let context_menu = self.render_context_menu(cx);
        // Both modals are `row_apply`'s; what each button does is this pane's,
        // and it is said here rather than threaded through as an entity.
        let confirm = self
            .confirm_discard
            .then(|| self.counts(cx))
            .flatten()
            .zip(self.active_id())
            .map(|(counts, tab)| {
                let this = cx.entity();
                let discard = this.clone();
                render_discard_confirm(
                    counts,
                    cx,
                    move |_window, cx| {
                        this.update(cx, |pane, cx| {
                            pane.confirm_discard = false;
                            cx.notify();
                        });
                    },
                    move |_window, cx| {
                        discard.update(cx, |pane, cx| pane.discard_all(tab, cx));
                    },
                )
            });
        let preview = self.preview.as_ref().map(|preview| {
            let this = cx.entity();
            let run = this.clone();
            render_apply_preview(
                &preview.statements,
                self.transactional,
                cx,
                move |_window, cx| {
                    this.update(cx, |pane, cx| {
                        pane.preview = None;
                        cx.notify();
                    });
                },
                move |window, cx| {
                    run.update(cx, |pane, cx| pane.confirm_apply(window, cx));
                },
            )
        });

        div()
            .flex()
            .flex_col()
            .size_full()
            .min_w_0()
            .min_h_0()
            .relative()
            // Measured against this box rather than accumulated, exactly as the
            // workspace's own split dividers are: the seam follows the pointer
            // however far the gesture wandered.
            .on_drag_move::<DraggedQueryDivider>(cx.listener(
                |pane, event: &DragMoveEvent<DraggedQueryDivider>, _window, cx| {
                    pane.drag_divider(event, cx);
                },
            ))
            .child(
                // The editor draws with whatever text style it inherits, so
                // this wrapper is where the editor font settings take effect —
                // through `effective`, so the settings dialog's live preview
                // reaches the editor the same way it reaches the chrome. With
                // no family configured it falls back to the platform's
                // monospace default rather than the UI font: SQL is columnar
                // text, and the DDL tab already reads the same way.
                div()
                    .flex()
                    .flex_basis(relative(share))
                    .min_w_0()
                    .min_h_0()
                    .font_family(fonts.editor_font_family.clone().map_or_else(
                        || crate::app_settings::monospace_family(cx),
                        SharedString::from,
                    ))
                    .text_size(px(fonts.editor_font_size))
                    .child(self.editor.clone()),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_basis(relative(1. - share))
                    .min_w_0()
                    .min_h_0()
                    .border_t_1()
                    .border_color(chrome.border)
                    .child(toolbar)
                    .children(banner)
                    .children(failure)
                    .child(results),
            )
            .children(self.notice.clone().map(|notice| {
                div()
                    .absolute()
                    .bottom(px(6.))
                    .left(px(10.))
                    .right(px(10.))
                    .px(px(8.))
                    .py(px(4.))
                    .rounded_md()
                    .bg(chrome.surface)
                    .border_1()
                    .border_color(chrome.border)
                    .text_size(px(11.))
                    .text_color(chrome.text_muted)
                    .child(notice)
            }))
            // After both halves, so it wins the hit test against the two it
            // straddles. The band is centred on the seam by `at`, which pulls it
            // back half its own thickness for us, and it carries the accent bar
            // that fades in under the pointer — the same widget, and so the same
            // bar, that [`rugpui::Splitter`] gives the dividers in the pane tree
            // around this panel.
            //
            // The entity id rides along in the element id as well as in the drag
            // payload, and for a related reason: the payload keeps a nested
            // pane's drag from writing its neighbour's ratio, while the element
            // id is what the handle files its fade under, and two panels showing
            // at once would otherwise share one bar between them.
            .child(
                ResizeHandle::new(
                    ("query-divider", id),
                    Axis::Vertical,
                    DraggedQueryDivider(id),
                )
                .at(relative(share))
                .thickness(px(DIVIDER_GRAB)),
            )
            // All last, and the menu last of all: a context menu paints above
            // even a modal (architecture document, §7.8). The two modals never
            // stand at once — raising either puts the other away — so their
            // order between themselves decides nothing.
            .children(confirm)
            .children(preview)
            // Takes no room in the column — the element is an empty absolute
            // box whose two halves are anchored to the window — so it can
            // simply be the last child, above the divider it may cover.
            .children(context_menu)
    }
}

impl QueryPane {
    /// Moves the divider between the editor and the results.
    pub(super) fn drag_divider(
        &mut self,
        event: &DragMoveEvent<DraggedQueryDivider>,
        cx: &mut Context<Self>,
    ) {
        if event.drag(cx).0 != cx.entity_id() {
            return;
        }
        let height = event.bounds.size.height;
        let share = f32::from(event.event.position.y - event.bounds.top()) / f32::from(height);
        if !share.is_finite() {
            return;
        }
        self.editor_share = share.clamp(MIN_SHARE, 1. - MIN_SHARE);
        cx.notify();
    }

    /// The strip above the results: the tabs, the clock, and the run controls.
    pub(super) fn render_toolbar(&self, chrome: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let this = cx.entity();
        let running = self.is_running();
        let cancelling = matches!(&self.run, RunState::Running(running) if running.cancelling);

        let tabs: Vec<_> = self
            .results
            .iter()
            .enumerate()
            .map(|(index, tab)| {
                let active = index == self.active_result;
                let this = this.clone();
                div()
                    .id(("result-tab", tab.id))
                    .flex_none()
                    .px(px(10.))
                    .py(px(4.))
                    .cursor_pointer()
                    .text_size(px(12.))
                    .border_b_2()
                    .border_color(if active {
                        chrome.accent
                    } else {
                        gpui::transparent_black()
                    })
                    .text_color(if active {
                        chrome.text
                    } else {
                        chrome.text_muted
                    })
                    .when(!active, |tab| tab.hover(|tab| tab.bg(chrome.surface_hover)))
                    .child(tab.label.clone())
                    .on_click(move |_, _window, cx| {
                        this.update(cx, |pane, cx| {
                            pane.active_result = index;
                            cx.notify();
                        });
                    })
            })
            .collect();

        let run = {
            let this = this.clone();
            Button::new("query-run", ts!("query.run"))
                .variant(ButtonVariant::Primary)
                .disabled(running)
                .on_click(move |_, window, cx| {
                    this.update(cx, |pane, cx| {
                        let text = pane.editor.read(cx).text();
                        let statements = match pane.editor.read(cx).statement_at_caret() {
                            Some(span) => vec![span.sql(&text).to_string()],
                            None => Vec::new(),
                        };
                        pane.request(statements, window, cx);
                    });
                })
        };
        let cancel = running.then(|| {
            let this = this.clone();
            Button::new(
                "query-cancel",
                if cancelling {
                    ts!("query.cancelling")
                } else {
                    ts!("query.cancel")
                },
            )
            .variant(ButtonVariant::Danger)
            .disabled(cancelling)
            .on_click(move |_, _window, cx| {
                this.update(cx, |pane, cx| pane.cancel(cx));
            })
        });

        // Drawn only over a result that passed §7.9's gate. On one that did
        // not, nothing can be staged, so an Apply that could never light up
        // would be furniture — and the line under the toolbar has already said
        // why.
        let writable = self
            .active_rows()
            .is_some_and(|rows| rows.grid.read(cx).source().writable());
        let counts = self.counts(cx);
        let staged = counts.is_some();
        let pending = counts.map(|counts| {
            ts!(
                "data.pending",
                changed = counts.changed,
                inserted = counts.inserted,
                deleted = counts.deleted
            )
        });
        let apply = writable.then(|| {
            let this = this.clone();
            Button::new("query-apply", ts!("data.apply"))
                .variant(ButtonVariant::Primary)
                .disabled(!staged || running || self.applying)
                .on_click(move |_, window, cx| {
                    this.update(cx, |pane, cx| pane.apply(window, cx));
                })
        });
        let discard = writable.then(|| {
            let this = this.clone();
            Button::new("query-discard", ts!("data.discard"))
                .variant(ButtonVariant::Secondary)
                .disabled(!staged || running || self.applying)
                .on_click(move |_, _window, cx| {
                    this.update(cx, |pane, cx| {
                        pane.confirm_discard = true;
                        pane.preview = None;
                        cx.notify();
                    });
                })
        });

        div()
            .flex()
            .flex_row()
            .flex_none()
            .items_center()
            .gap(px(8.))
            .px(px(6.))
            .h(px(30.))
            .border_b_1()
            .border_color(chrome.border)
            .child(div().flex().flex_row().flex_1().min_w_0().children(tabs))
            .children(pending.map(|pending| {
                div()
                    .flex_none()
                    .whitespace_nowrap()
                    .text_size(px(11.))
                    // In the accent the grid marks a changed row with, so that
                    // the line and the markers under it read as one thing.
                    .text_color(chrome.accent)
                    .child(pending)
            }))
            .when(running, |bar| {
                bar.child(
                    div()
                        .flex_none()
                        .whitespace_nowrap()
                        .text_size(px(11.))
                        .text_color(chrome.text_muted)
                        .child(elapsed_label(self.elapsed())),
                )
            })
            .children(apply)
            .children(discard)
            .children(cancel)
            .child(run)
            .into_any_element()
    }

    /// The one line that says why the result showing cannot be written back to.
    ///
    /// Understated on purpose, and above the grid rather than in a dialog: it
    /// is the answer to a question the user has not asked yet, and §7.9 will
    /// not have it delivered by refusing a keystroke.
    pub(super) fn render_banner(&self, chrome: &Theme) -> Option<impl IntoElement + use<>> {
        let reason = self.active_rows()?.read_only.clone()?;
        Some(
            div()
                .flex_none()
                .px(px(10.))
                .py(px(4.))
                .border_b_1()
                .border_color(chrome.border)
                .bg(chrome.surface)
                .text_size(px(11.))
                .text_color(chrome.text_muted)
                .child(reason),
        )
    }

    /// How long the run in flight has been going, or the last one took.
    pub(super) fn elapsed(&self) -> Duration {
        match &self.run {
            RunState::Running(running) => running.started.elapsed(),
            RunState::Idle => self
                .finished
                .as_ref()
                .map_or(Duration::ZERO, |finished| finished.elapsed),
        }
    }

    /// The pane's right-click menu, while one is open.
    pub(super) fn render_context_menu(
        &self,
        cx: &mut Context<Self>,
    ) -> Option<impl IntoElement + use<>> {
        let (position, rows) = match self.context_menu.as_ref()? {
            PaneMenu::Editor { position } => (*position, self.editor_rows(cx)),
            PaneMenu::Grid {
                id,
                target,
                position,
            } => (*position, self.grid_rows(*id, *target, cx)),
        };
        let this = cx.entity();

        Some(
            ContextMenu::new("query-context")
                .position(position)
                .entries(context_menu::entries(rows))
                .on_dismiss(move |_window, cx| {
                    this.update(cx, |pane, cx| {
                        pane.close_context_menu(cx);
                    });
                }),
        )
    }

    /// The result area's body: a grid, a message, a failure, or an empty state.
    pub(super) fn render_results(&self, chrome: &Theme) -> AnyElement {
        if let Some(error) = &self.error {
            return render_error(error, chrome);
        }
        if self.results.is_empty() {
            let text = if self.is_running() {
                ts!("query.running")
            } else if self.ran {
                ts!("query.no_results")
            } else {
                ts!("query.empty")
            };
            return note(text, chrome.text_muted);
        }
        match self.results.get(self.active_result) {
            Some(ResultTab {
                body: ResultBody::Rows(rows),
                ..
            }) => div()
                .flex()
                .flex_1()
                .min_w_0()
                .min_h_0()
                .child(rows.grid.clone())
                .into_any_element(),
            Some(ResultTab {
                body: ResultBody::Message(text),
                ..
            }) => note(text.clone(), chrome.text),
            None => note(ts!("query.empty"), chrome.text_muted),
        }
    }
}
