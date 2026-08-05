//! A grid of selectable cards, each previewing one chrome theme.
//!
//! The counterpart of [`rudbman_ui::EditorThemePicker`], and deliberately not a
//! second copy of it: a syntax palette only means something in arrangement, so
//! its card renders a statement, while a chrome theme *is* a set of flat
//! surfaces and an honest picture of one is those surfaces laid out the way the
//! window lays them out — a page, a raised bar on it, text of both weights, the
//! three status hues, and the two grid bands.
//!
//! It lives in the application rather than in the widget crate for the same
//! reason the settings dialog does: nothing else needs it, and the widget crate
//! keeps no state that would make it worth generalising yet.

use std::rc::Rc;

use gpui::{App, ElementId, SharedString, Window, div, prelude::*, px};
use rudbman_ui::{Theme, theme};

/// Default number of cards per row.
///
/// Three, where the editor picker takes two: a chrome card carries no text
/// beyond a word or two, so it stays legible at a third of the dialog's width.
const DEFAULT_COLUMNS: usize = 3;

/// Height of the previewed page, in pixels.
const PREVIEW_HEIGHT: f32 = 46.;

/// Callback fired with the id of the newly picked theme.
type SelectHandler = Rc<dyn Fn(&str, &mut Window, &mut App)>;

/// One entry of a [`ThemePicker`].
#[derive(Debug, Clone)]
pub struct ThemeSwatch {
    /// Stable id reported to [`ThemePicker::on_select`].
    id: SharedString,
    /// Label shown under the preview.
    name: SharedString,
    /// The palette the card is painted with.
    preview: Theme,
}

impl ThemeSwatch {
    /// Creates an entry previewing `preview`.
    pub fn new(id: impl Into<SharedString>, name: impl Into<SharedString>, preview: Theme) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            preview,
        }
    }
}

/// A stateless grid of chrome-theme cards.
///
/// The picker owns no state: the parent view passes the entries and the
/// selected id on every render and reacts to [`ThemePicker::on_select`].
///
/// The grid takes a single tab stop. While focused, the arrow keys move the
/// selection within it — `Left`/`Right` by one card, `Up`/`Down` by one row —
/// without wrapping, which is how a grid of radio buttons behaves everywhere
/// else, and which is what makes browsing the themes a matter of holding an
/// arrow key down while the window repaints under each one.
#[derive(IntoElement)]
pub struct ThemePicker {
    id: ElementId,
    options: Vec<ThemeSwatch>,
    selected: Option<SharedString>,
    columns: usize,
    tab_index: Option<isize>,
    on_select: Option<SelectHandler>,
}

impl ThemePicker {
    /// Creates an empty picker.
    ///
    /// `id` must be unique among the siblings of the picker.
    pub fn new(id: impl Into<ElementId>) -> Self {
        Self {
            id: id.into(),
            options: Vec::new(),
            selected: None,
            columns: DEFAULT_COLUMNS,
            tab_index: None,
            on_select: None,
        }
    }

    /// Sets the entries, in display order.
    pub fn options(mut self, options: impl IntoIterator<Item = ThemeSwatch>) -> Self {
        self.options = options.into_iter().collect();
        self
    }

    /// Sets the id of the highlighted entry. An unknown id highlights nothing.
    pub fn selected(mut self, selected: Option<impl Into<SharedString>>) -> Self {
        self.selected = selected.map(Into::into);
        self
    }

    /// Sets how many cards share a row. Zero is treated as one.
    pub fn columns(mut self, columns: usize) -> Self {
        self.columns = columns.max(1);
        self
    }

    /// Places the grid at `index` in the window's tab order.
    pub fn tab_index(mut self, index: isize) -> Self {
        self.tab_index = Some(index);
        self
    }

    /// Sets the callback invoked with the id of the picked entry.
    ///
    /// Never fired for the entry that is already selected.
    pub fn on_select(mut self, handler: impl Fn(&str, &mut Window, &mut App) + 'static) -> Self {
        self.on_select = Some(Rc::new(handler));
        self
    }
}

/// The miniature window drawn inside one card.
///
/// Every colour comes from `preview` and none from the surrounding chrome,
/// which is the whole point: the card has to look like the window will look,
/// not like the dialog it is sitting in.
fn miniature(preview: &Theme) -> impl IntoElement {
    // The three hues a palette is argued about, as chips.
    let chip = |color: gpui::Hsla| div().flex_none().size(px(7.)).rounded_full().bg(color);
    // Two body rows of the result grid under its header, which is the only
    // place the five grid slots are ever visible at this size.
    let band = |color: gpui::Hsla| div().w_full().h(px(5.)).bg(color);

    div()
        .flex()
        .flex_col()
        .w_full()
        .h(px(PREVIEW_HEIGHT))
        .overflow_hidden()
        .rounded_sm()
        .bg(preview.background)
        .child(
            // The toolbar band: a raised surface with the theme's own text on
            // it, and one active cell standing in for the selected tab.
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap(px(4.))
                .w_full()
                .h(px(13.))
                .px(px(4.))
                .bg(preview.surface)
                .border_b_1()
                .border_color(preview.border)
                .child(
                    div()
                        .flex_none()
                        .w(px(18.))
                        .h(px(8.))
                        .rounded_sm()
                        .bg(preview.surface_active),
                )
                .child(
                    div()
                        .flex_none()
                        .w(px(12.))
                        .h(px(8.))
                        .rounded_sm()
                        .bg(preview.surface_hover),
                )
                .child(div().flex_1().min_w_0())
                .child(chip(preview.accent)),
        )
        .child(
            div()
                .flex()
                .flex_col()
                .flex_1()
                .min_h_0()
                .gap(px(3.))
                .p(px(4.))
                .child(
                    div()
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap(px(4.))
                        .child(chip(preview.success))
                        .child(chip(preview.danger))
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .h(px(4.))
                                .rounded_full()
                                .bg(preview.text),
                        )
                        .child(
                            div()
                                .flex_none()
                                .w(px(14.))
                                .h(px(4.))
                                .rounded_full()
                                .bg(preview.text_muted),
                        ),
                )
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .w_full()
                        .overflow_hidden()
                        .rounded_sm()
                        .child(band(preview.grid_header))
                        .child(band(preview.background))
                        .child(band(preview.grid_row_alt)),
                ),
        )
}

impl RenderOnce for ThemePicker {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let chrome = theme(cx);
        let columns = self.columns;
        let selected = self.selected;
        let on_select = self.on_select;
        let container_id = self.id;
        let outer_id = container_id.clone();
        let tab_index = self.tab_index;

        let ids: Vec<SharedString> = self.options.iter().map(|entry| entry.id.clone()).collect();
        let current = selected
            .as_ref()
            .and_then(|id| ids.iter().position(|candidate| candidate == id));

        let rows: Vec<_> = self
            .options
            .chunks(columns)
            .map(|entries| {
                let cards: Vec<_> = entries
                    .iter()
                    .map(|entry| {
                        let is_selected = Some(&entry.id) == selected.as_ref();
                        let handler = on_select.clone().filter(|_| !is_selected);
                        let id = entry.id.clone();

                        div()
                            .id(ElementId::from((container_id.clone(), entry.id.clone())))
                            .flex()
                            .flex_col()
                            .flex_1()
                            .min_w_0()
                            .gap(px(4.))
                            .p(px(4.))
                            .rounded_md()
                            .border_1()
                            .border_color(if is_selected {
                                chrome.accent
                            } else {
                                chrome.border
                            })
                            .bg(if is_selected {
                                chrome.surface_active
                            } else {
                                chrome.surface
                            })
                            .when(!is_selected, |this| {
                                this.cursor_pointer()
                                    .hover(|style| style.bg(chrome.surface_hover))
                            })
                            .when_some(handler, |this, handler| {
                                this.on_click(move |_, window, cx| handler(&id, window, cx))
                            })
                            .child(miniature(&entry.preview))
                            .child(
                                div()
                                    .truncate()
                                    .text_size(px(11.))
                                    .text_color(if is_selected {
                                        chrome.text
                                    } else {
                                        chrome.text_muted
                                    })
                                    .child(entry.name.clone()),
                            )
                            .into_any_element()
                    })
                    .collect();

                // Pad the last row so its cards keep the width of a full row
                // instead of stretching to fill it.
                let padding = (columns - entries.len()) % columns;
                div()
                    .flex()
                    .flex_row()
                    .w_full()
                    .gap(px(6.))
                    .children(cards)
                    .children((0..padding).map(|_| div().flex_1().min_w_0().into_any_element()))
            })
            .collect();

        div()
            .id(outer_id)
            .flex()
            .flex_col()
            .w_full()
            .gap(px(6.))
            .p(px(2.))
            .rounded_md()
            // A transparent outline reserves the room the focus ring needs, so
            // focusing the grid does not shift the form by a pixel.
            .border_1()
            .border_color(gpui::transparent_black())
            .when_some(tab_index.filter(|_| !ids.is_empty()), |this, index| {
                let accent = chrome.accent;
                let arrow_handler = on_select.clone();
                this.tab_index(index)
                    .focus(move |style| style.border_color(accent))
                    .on_key_down(move |event, window, cx| {
                        if event.keystroke.modifiers.modified() {
                            return;
                        }
                        let Some(current) = current else { return };
                        let last = ids.len() - 1;
                        let next = match event.keystroke.key.as_str() {
                            "left" => current.checked_sub(1),
                            "right" => (current < last).then(|| current + 1),
                            "up" => current.checked_sub(columns),
                            "down" => (current + columns <= last).then(|| current + columns),
                            _ => return,
                        };
                        let (Some(next), Some(handler)) = (next, arrow_handler.as_ref()) else {
                            return;
                        };
                        cx.stop_propagation();
                        handler(&ids[next], window, cx);
                    })
            })
            .children(rows)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_picker_always_has_at_least_one_column() {
        // Zero columns would divide by zero in the row chunking.
        assert_eq!(ThemePicker::new("p").columns(0).columns, 1);
        assert_eq!(ThemePicker::new("p").columns(4).columns, 4);
        assert_eq!(ThemePicker::new("p").columns, DEFAULT_COLUMNS);
    }

    #[test]
    fn a_swatch_keeps_the_palette_it_was_given() {
        let swatch = ThemeSwatch::new("one-light", "One Light", Theme::light());
        assert_eq!(swatch.id, "one-light");
        assert!(!swatch.preview.dark);
    }
}
