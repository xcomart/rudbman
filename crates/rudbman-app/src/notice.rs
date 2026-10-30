//! The one line a pane says about what just happened, and how it is drawn.
//!
//! The query, data and structure panes each have a line of their own for the
//! things that are not failures of a statement — "transaction committed",
//! "already running", "discard the staged changes first". It used to float
//! over the bottom of the grid in one muted gray whatever it said, covering the
//! last row and the horizontal scrollbar until something else replaced it, and
//! a commit that failed read exactly like one that succeeded.
//!
//! So a notice has a [`NoticeLevel`], and the level decides both its color and
//! how long it stays. It is drawn as a band between the toolbar and the rows
//! rather than over them, with a close button; an [`NoticeLevel::Info`] — a
//! report that something the user asked for has happened — goes on its own
//! after [`INFO_LINGER`], while a warning or an error stays until it is closed
//! or replaced, because it is about something that has *not* happened.

use std::time::Duration;

use gpui::{App, Div, SharedString, Task, div, prelude::*, px};
use rugpui::Theme;

use crate::icons;

/// How long an informational notice stays before it goes on its own.
pub const INFO_LINGER: Duration = Duration::from_secs(6);

/// How much a notice matters.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NoticeLevel {
    /// Something asked for has happened: committed, applied, saved.
    Info,
    /// Something asked for was refused, and the notice says what to do first.
    Warning,
    /// Something asked for failed.
    Error,
}

/// One line a pane is saying.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Notice {
    /// What it says.
    pub text: SharedString,
    /// How much it matters.
    pub level: NoticeLevel,
}

impl Notice {
    /// A report that something has happened.
    pub fn info(text: impl Into<SharedString>) -> Self {
        Self {
            text: text.into(),
            level: NoticeLevel::Info,
        }
    }

    /// A refusal, saying what to do first.
    pub fn warn(text: impl Into<SharedString>) -> Self {
        Self {
            text: text.into(),
            level: NoticeLevel::Warning,
        }
    }

    /// A failure.
    pub fn error(text: impl Into<SharedString>) -> Self {
        Self {
            text: text.into(),
            level: NoticeLevel::Error,
        }
    }
}

/// The band a notice is drawn as, with its close button.
///
/// `id` keeps the button's element apart between the panes on screen;
/// `on_close` is what the button does, which is the pane's to say.
pub fn render_notice(
    id: impl Into<gpui::ElementId>,
    notice: &Notice,
    chrome: &Theme,
    on_close: impl Fn(&mut gpui::Window, &mut App) + 'static,
    cx: &App,
) -> Div {
    let color = match notice.level {
        NoticeLevel::Info => chrome.text_muted,
        NoticeLevel::Warning => chrome.text,
        NoticeLevel::Error => chrome.danger,
    };
    div()
        .flex()
        .flex_row()
        .flex_none()
        .items_center()
        .gap(px(8.))
        .px(px(10.))
        .py(px(4.))
        .border_b_1()
        .border_color(chrome.border)
        .bg(chrome.surface)
        .text_size(rugpui::TextSize::Xs.size(cx))
        .text_color(color)
        .child(div().flex_1().min_w_0().child(notice.text.clone()))
        .child(
            div()
                .id(id)
                .flex_none()
                .size(px(18.))
                .flex()
                .items_center()
                .justify_center()
                .rounded_sm()
                .cursor_pointer()
                .hover(|style| style.bg(chrome.surface_hover))
                .child(icons::icon(
                    rugpui_shell::icons::WINDOW_CLOSE,
                    px(12.),
                    chrome.icon,
                ))
                .on_click(move |_, window, cx| on_close(window, cx)),
        )
}

/// The clock that takes an informational notice away.
///
/// Kept by each pane beside its notice and asked on every frame: a new info
/// notice starts the clock, and when it runs out the pane is told — through
/// `clear` — to drop the notice only if it is still that one. A warning or an
/// error starts nothing.
#[derive(Default)]
pub struct NoticeClock {
    /// The notice the clock is running for.
    timing: Option<Notice>,
    /// The clock itself; dropping it stops it.
    _task: Option<Task<()>>,
}

impl NoticeClock {
    /// Starts the clock for `notice` when it is an info notice the clock is
    /// not already running for. `clear` runs when it ends.
    pub fn watch<V: 'static>(
        &mut self,
        notice: Option<&Notice>,
        cx: &mut gpui::Context<V>,
        clear: fn(&mut V, &Notice) -> bool,
    ) {
        let Some(notice) = notice.filter(|notice| notice.level == NoticeLevel::Info) else {
            self.timing = None;
            self._task = None;
            return;
        };
        if self.timing.as_ref() == Some(notice) {
            return;
        }
        self.timing = Some(notice.clone());
        let notice = notice.clone();
        self._task = Some(cx.spawn(async move |view, cx| {
            cx.background_executor().timer(INFO_LINGER).await;
            view.update(cx, |view, cx| {
                if clear(view, &notice) {
                    cx.notify();
                }
            })
            .ok();
        }));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A view holding nothing but a notice and its clock.
    struct Holder {
        notice: Option<Notice>,
        clock: NoticeClock,
    }

    impl Holder {
        fn watch(&mut self, cx: &mut gpui::Context<Self>) {
            let notice = self.notice.clone();
            self.clock.watch(notice.as_ref(), cx, |holder, shown| {
                let still = holder.notice.as_ref() == Some(shown);
                if still {
                    holder.notice = None;
                }
                still
            });
        }
    }

    /// A report goes on its own after a while; a refusal and a failure stay
    /// until something replaces them, because they are about what did not
    /// happen.
    #[gpui::test]
    fn only_an_informational_notice_goes_on_its_own(cx: &mut gpui::TestAppContext) {
        for (notice, stays) in [
            (Notice::info("committed"), false),
            (Notice::warn("discard the changes first"), true),
            (Notice::error("commit failed"), true),
        ] {
            let holder = cx.new(|_| Holder {
                notice: Some(notice.clone()),
                clock: NoticeClock::default(),
            });
            holder.update(cx, |holder, cx| holder.watch(cx));
            cx.executor()
                .advance_clock(INFO_LINGER + Duration::from_millis(1));
            cx.run_until_parked();
            holder.update(cx, |holder, _cx| {
                assert_eq!(holder.notice.is_some(), stays, "{notice:?}");
            });
        }
    }

    /// A notice replaced before its time is not taken away by the clock the
    /// one it replaced started.
    #[gpui::test]
    fn a_replaced_notice_outlives_the_clock_of_the_one_before(cx: &mut gpui::TestAppContext) {
        let holder = cx.new(|_| Holder {
            notice: Some(Notice::info("committed")),
            clock: NoticeClock::default(),
        });
        holder.update(cx, |holder, cx| holder.watch(cx));
        cx.executor().advance_clock(INFO_LINGER / 2);
        holder.update(cx, |holder, cx| {
            holder.notice = Some(Notice::error("the next commit failed"));
            holder.watch(cx);
        });
        cx.executor().advance_clock(INFO_LINGER);
        cx.run_until_parked();
        holder.update(cx, |holder, _cx| {
            assert_eq!(holder.notice, Some(Notice::error("the next commit failed")));
        });
    }
}
