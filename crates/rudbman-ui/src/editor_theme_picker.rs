//! A grid of selectable cards, each previewing one editor theme.
//!
//! Where a chrome theme can be previewed with a few swatches — it is a set of
//! flat surfaces, and a strip of coloured blocks is an honest picture of one —
//! a syntax palette cannot. Its colors only mean anything in arrangement: what
//! a reader is choosing between is not "is this purple nice" but "can I tell a
//! keyword from a column name at a glance, and is the comment still legible".
//! Swatches answer neither question. So each card renders a *statement*
//! instead, in miniature, with a gutter, a current-line band, a selection and a
//! caret, painted entirely out of the theme it is offering.
//!
//! The snippet is hardcoded, spans and all: there is no lexer in this crate and
//! the preview does not need one. The real SQL lexer arrives with the editor,
//! and the preview will still not use it, because it never has to lex anything
//! but the one statement written below.

use std::rc::Rc;

use gpui::{App, ElementId, Hsla, SharedString, Window, div, prelude::*, px};

use crate::editor_theme::EditorTheme;
use crate::theme::theme;

/// Default number of cards per row.
///
/// Two rather than the three a swatch grid takes: a card here has a statement
/// in it, and a statement needs the width.
const DEFAULT_COLUMNS: usize = 2;

/// Font size of the previewed code, in pixels.
///
/// Small enough that a whole statement fits a card two to a row, large enough
/// that the hues are still hues rather than antialiasing.
const PREVIEW_TEXT_SIZE: f32 = 9.;

/// Height of one previewed line, in pixels.
const PREVIEW_LINE_HEIGHT: f32 = 13.;

/// Width of the previewed gutter, in pixels.
const PREVIEW_GUTTER_WIDTH: f32 = 13.;

/// Text drawn on a card that has no colors of its own.
///
/// English, because the widget layer has no locale of its own; every caller in
/// the application overrides it with [`EditorThemeSwatch::placeholder_label`].
const DEFAULT_PLACEHOLDER_LABEL: &str = "follows the app theme";

/// Callback fired with the id of the newly picked theme.
type SelectHandler = Rc<dyn Fn(&str, &mut Window, &mut App)>;

/// Which slot of an [`EditorTheme`] paints one run of the preview.
///
/// A subset of the palette: the slots a five-line `SELECT` can actually show.
/// [`EditorTheme::error`] and [`EditorTheme::warning`] are left out on purpose
/// — a card advertising a theme should not be drawing a broken statement — and
/// so are the frame slots, which the card renders directly.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PreviewSlot {
    /// See [`EditorTheme::keyword`].
    Keyword,
    /// See [`EditorTheme::identifier`].
    Identifier,
    /// See [`EditorTheme::number`].
    Number,
    /// See [`EditorTheme::string`].
    Str,
    /// See [`EditorTheme::comment`].
    Comment,
    /// See [`EditorTheme::operator`].
    Operator,
    /// See [`EditorTheme::punctuation`].
    Punctuation,
    /// See [`EditorTheme::foreground`]; the spaces between the rest.
    Plain,
}

impl PreviewSlot {
    /// The color `theme` paints this slot with.
    fn color(self, theme: &EditorTheme) -> Hsla {
        match self {
            Self::Keyword => theme.keyword,
            Self::Identifier => theme.identifier,
            Self::Number => theme.number,
            Self::Str => theme.string,
            Self::Comment => theme.comment,
            Self::Operator => theme.operator,
            Self::Punctuation => theme.punctuation,
            Self::Plain => theme.foreground,
        }
    }
}

/// One run of characters in the previewed snippet.
struct PreviewSpan {
    /// The characters themselves.
    text: &'static str,
    /// The slot that colors them.
    slot: PreviewSlot,
    /// Whether the run sits inside the previewed selection, and so is drawn on
    /// [`EditorTheme::selection`] rather than on the page.
    selected: bool,
}

/// A run outside the selection.
const fn span(text: &'static str, slot: PreviewSlot) -> PreviewSpan {
    PreviewSpan {
        text,
        slot,
        selected: false,
    }
}

/// A run inside the selection.
const fn picked(text: &'static str, slot: PreviewSlot) -> PreviewSpan {
    PreviewSpan {
        text,
        slot,
        selected: true,
    }
}

/// The statement every card renders.
///
/// Chosen so that seven slots land on five short lines and no line runs past
/// seventeen characters: a comment, two flavours of literal, two operators, the
/// punctuation between the columns, and identifiers on both sides of a `FROM`.
/// The reader is meant to be able to answer "can I tell these apart" without
/// reading the statement at all.
const PREVIEW_LINES: &[&[PreviewSpan]] = &[
    &[span("-- recent", PreviewSlot::Comment)],
    &[
        span("SELECT", PreviewSlot::Keyword),
        span(" ", PreviewSlot::Plain),
        span("id", PreviewSlot::Identifier),
        span(",", PreviewSlot::Punctuation),
        span(" ", PreviewSlot::Plain),
        picked("name", PreviewSlot::Identifier),
    ],
    &[
        span("FROM", PreviewSlot::Keyword),
        span(" ", PreviewSlot::Plain),
        span("users", PreviewSlot::Identifier),
    ],
    &[
        span("WHERE", PreviewSlot::Keyword),
        span(" ", PreviewSlot::Plain),
        span("age", PreviewSlot::Identifier),
        span(" ", PreviewSlot::Plain),
        span(">", PreviewSlot::Operator),
        span(" ", PreviewSlot::Plain),
        span("21", PreviewSlot::Number),
    ],
    &[
        span("  ", PreviewSlot::Plain),
        span("AND", PreviewSlot::Keyword),
        span(" ", PreviewSlot::Plain),
        span("tag", PreviewSlot::Identifier),
        span(" ", PreviewSlot::Plain),
        span("=", PreviewSlot::Operator),
        span(" ", PreviewSlot::Plain),
        span("'new'", PreviewSlot::Str),
        span(";", PreviewSlot::Punctuation),
    ],
];

/// Which line of [`PREVIEW_LINES`] carries the caret.
///
/// The line the selection is on, so that the current-line band, the selection
/// and the caret are all visible at once and can be seen not to swallow one
/// another — the three that most often do in a palette nobody checked.
const PREVIEW_CARET_LINE: usize = 1;

/// One entry of an [`EditorThemePicker`].
#[derive(Debug, Clone)]
pub struct EditorThemeSwatch {
    /// Stable id reported to [`EditorThemePicker::on_select`].
    id: SharedString,
    /// Label shown under the preview.
    name: SharedString,
    /// Palette to preview. `None` renders a muted placeholder card instead,
    /// which is how the picker offers "follow the app theme".
    preview: Option<EditorTheme>,
    /// Text drawn on that placeholder card. Taken from the caller so the widget
    /// needs no translations of its own.
    placeholder_label: SharedString,
}

impl EditorThemeSwatch {
    /// Creates an entry with no preview, drawn as a muted placeholder card.
    pub fn new(id: impl Into<SharedString>, name: impl Into<SharedString>) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            preview: None,
            placeholder_label: SharedString::new_static(DEFAULT_PLACEHOLDER_LABEL),
        }
    }

    /// Attaches the palette this entry's snippet is painted with.
    pub fn preview(mut self, preview: EditorTheme) -> Self {
        self.preview = Some(preview);
        self
    }

    /// Sets the text of the placeholder card shown when there is no preview.
    ///
    /// Callers pass a translated string; the built-in default is English.
    pub fn placeholder_label(mut self, label: impl Into<SharedString>) -> Self {
        self.placeholder_label = label.into();
        self
    }
}

/// A stateless grid of editor-theme cards.
///
/// The picker owns no state: the parent view passes the entries and the
/// selected id on every render and reacts to [`EditorThemePicker::on_select`].
///
/// The grid takes a single tab stop. While focused, the arrow keys move the
/// selection within the grid — `Left`/`Right` by one card, `Up`/`Down` by one
/// row — without wrapping, which is how a grid of radio buttons behaves
/// everywhere else.
///
/// ```ignore
/// EditorThemePicker::new("editor-theme")
///     .options(swatches)
///     .selected(Some(self.editor_theme.clone()))
///     .font_family(self.editor_font.clone())
///     .columns(2)
///     .on_select(cx.listener(..))
/// ```
#[derive(IntoElement)]
pub struct EditorThemePicker {
    id: ElementId,
    options: Vec<EditorThemeSwatch>,
    selected: Option<SharedString>,
    columns: usize,
    font_family: Option<SharedString>,
    tab_index: Option<isize>,
    on_select: Option<SelectHandler>,
}

impl EditorThemePicker {
    /// Creates an empty picker.
    ///
    /// `id` must be unique among the siblings of the picker.
    pub fn new(id: impl Into<ElementId>) -> Self {
        Self {
            id: id.into(),
            options: Vec::new(),
            selected: None,
            columns: DEFAULT_COLUMNS,
            font_family: None,
            tab_index: None,
            on_select: None,
        }
    }

    /// Sets the entries, in display order.
    pub fn options(mut self, options: impl IntoIterator<Item = EditorThemeSwatch>) -> Self {
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

    /// Renders the snippet in `family` rather than in the inherited font.
    ///
    /// Passed in rather than resolved here: which monospace family is present
    /// on a machine, and which one the user has pointed the editor at, are both
    /// questions this crate has no business answering. A caller that says
    /// nothing gets the surrounding font, which previews the colors correctly
    /// and only the metrics wrongly.
    pub fn font_family(mut self, family: impl Into<SharedString>) -> Self {
        self.font_family = Some(family.into());
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

/// The miniature editor drawn inside one card.
///
/// Every color comes from `preview` and none from the surrounding chrome, which
/// is the whole point: the card has to look like the editor will look, not like
/// the dialog it is sitting in.
fn snippet(preview: &EditorTheme, font_family: Option<SharedString>) -> impl IntoElement {
    let lines = PREVIEW_LINES.iter().enumerate().map(|(index, spans)| {
        let current = index == PREVIEW_CARET_LINE;

        div()
            .flex()
            .flex_row()
            .items_center()
            .w_full()
            .h(px(PREVIEW_LINE_HEIGHT))
            // The current-line band spans the gutter too, as it does in the
            // editor: a band that stops at the text reads as a selection.
            .when(current, |this| this.bg(preview.line_highlight))
            .child(
                div()
                    .flex_none()
                    .w(px(PREVIEW_GUTTER_WIDTH))
                    .pr(px(3.))
                    .text_right()
                    .text_color(if current {
                        preview.gutter_active
                    } else {
                        preview.gutter
                    })
                    .child(SharedString::from((index + 1).to_string())),
            )
            .children(spans.iter().map(|span| {
                div()
                    .flex_none()
                    .whitespace_nowrap()
                    .text_color(span.slot.color(preview))
                    .when(span.selected, |this| this.bg(preview.selection))
                    .child(span.text)
            }))
            // The caret, drawn as the hairline the editor draws rather than as
            // a block: a block caret would hide the character under it and take
            // the selection's color with it.
            .when(current, |this| {
                this.child(div().flex_none().w(px(1.)).h(px(10.)).bg(preview.cursor))
            })
    });

    div()
        .flex()
        .flex_col()
        .w_full()
        .overflow_hidden()
        .py(px(3.))
        .rounded_sm()
        .bg(preview.background)
        .text_size(px(PREVIEW_TEXT_SIZE))
        .text_color(preview.foreground)
        .when_some(font_family, |this, family| this.font_family(family))
        .children(lines)
}

/// Height of a card body, so that a placeholder card is the size of a preview.
fn body_height() -> f32 {
    PREVIEW_LINE_HEIGHT * PREVIEW_LINES.len() as f32 + 6.
}

impl RenderOnce for EditorThemePicker {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = theme(cx);
        let columns = self.columns;
        let selected = self.selected;
        let on_select = self.on_select;
        let font_family = self.font_family;
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

                        let body = match &entry.preview {
                            Some(preview) => {
                                snippet(preview, font_family.clone()).into_any_element()
                            }
                            None => div()
                                .flex()
                                .flex_row()
                                .items_center()
                                .justify_center()
                                .h(px(body_height()))
                                .px(px(6.))
                                .rounded_sm()
                                .border_1()
                                .border_color(theme.border)
                                .text_size(px(11.))
                                .text_color(theme.text_muted)
                                .child(entry.placeholder_label.clone())
                                .into_any_element(),
                        };

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
                                theme.accent
                            } else {
                                theme.border
                            })
                            .bg(if is_selected {
                                theme.surface_active
                            } else {
                                theme.surface
                            })
                            .when(!is_selected, |this| {
                                this.cursor_pointer()
                                    .hover(|style| style.bg(theme.surface_hover))
                            })
                            .when_some(handler, |this, handler| {
                                this.on_click(move |_, window, cx| handler(&id, window, cx))
                            })
                            .child(body)
                            .child(
                                div()
                                    .truncate()
                                    .text_size(px(11.))
                                    .text_color(if is_selected {
                                        theme.text
                                    } else {
                                        theme.text_muted
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
            .border_1()
            .border_color(gpui::transparent_black())
            .when_some(tab_index.filter(|_| !ids.is_empty()), |this, index| {
                let accent = theme.accent;
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

    /// Every slot the snippet actually reaches for.
    fn slots_used() -> Vec<PreviewSlot> {
        PREVIEW_LINES
            .iter()
            .flat_map(|line| line.iter().map(|span| span.slot))
            .collect()
    }

    /// A preview that quietly stopped drawing a class would still look fine,
    /// which is exactly why it is worth asserting that it draws them all.
    #[test]
    fn the_snippet_shows_every_class_it_claims_to() {
        let used = slots_used();
        for slot in [
            PreviewSlot::Keyword,
            PreviewSlot::Identifier,
            PreviewSlot::Number,
            PreviewSlot::Str,
            PreviewSlot::Comment,
            PreviewSlot::Operator,
            PreviewSlot::Punctuation,
        ] {
            assert!(used.contains(&slot), "{slot:?} is never drawn");
        }
    }

    /// The distinctions the preview is *for*: a reader picking a theme from
    /// these cards is deciding whether they can tell these pairs apart, so a
    /// built-in theme that paints two of them the same makes the card a lie.
    #[test]
    fn every_builtin_theme_keeps_the_previewed_classes_apart() {
        for theme in [
            EditorTheme::one_dark(),
            EditorTheme::one_light(),
            EditorTheme::solarized_dark(),
            EditorTheme::solarized_light(),
            EditorTheme::gruvbox_dark(),
            EditorTheme::dracula(),
        ] {
            for (left, right) in [
                (PreviewSlot::Keyword, PreviewSlot::Identifier),
                (PreviewSlot::Keyword, PreviewSlot::Number),
                (PreviewSlot::Keyword, PreviewSlot::Str),
                (PreviewSlot::Comment, PreviewSlot::Identifier),
                (PreviewSlot::Number, PreviewSlot::Str),
                (PreviewSlot::Operator, PreviewSlot::Identifier),
            ] {
                assert_ne!(
                    left.color(&theme),
                    right.color(&theme),
                    "{left:?} and {right:?} are the same color"
                );
            }
        }
    }

    /// The caret sits on a line that exists, and on the line the selection is
    /// on — the three bands have to be on screen together to be judged.
    #[test]
    fn the_caret_line_carries_the_selection() {
        assert!(PREVIEW_CARET_LINE < PREVIEW_LINES.len());
        assert!(
            PREVIEW_LINES[PREVIEW_CARET_LINE]
                .iter()
                .any(|span| span.selected),
            "the caret line has nothing selected on it"
        );
        // And nothing outside it does: one selection, not two.
        for (index, line) in PREVIEW_LINES.iter().enumerate() {
            if index != PREVIEW_CARET_LINE {
                assert!(line.iter().all(|span| !span.selected), "line {index}");
            }
        }
    }

    /// A swatch with no palette is the "follow the app theme" card, and it has
    /// a label of its own even when the caller supplies none.
    #[test]
    fn a_swatch_without_a_preview_is_a_placeholder() {
        let swatch = EditorThemeSwatch::new("inherit", "Follow");
        assert!(swatch.preview.is_none());
        assert_eq!(swatch.placeholder_label, DEFAULT_PLACEHOLDER_LABEL);

        let swatch = swatch.placeholder_label("앱 테마를 따름");
        assert_eq!(swatch.placeholder_label, "앱 테마를 따름");

        let swatch =
            EditorThemeSwatch::new("one-dark", "One Dark").preview(EditorTheme::one_dark());
        assert!(swatch.preview.is_some());
    }

    /// Zero columns would divide by zero in the row chunking.
    #[test]
    fn a_picker_always_has_at_least_one_column() {
        assert_eq!(EditorThemePicker::new("p").columns(0).columns, 1);
        assert_eq!(EditorThemePicker::new("p").columns(3).columns, 3);
        assert_eq!(EditorThemePicker::new("p").columns, DEFAULT_COLUMNS);
    }
}
