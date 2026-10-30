//! A shared BLOB/CLOB and multiline text viewer for table data and query results.
//! The dialog stages replacements; only the existing row Apply writes to the DB.

use std::path::PathBuf;

use gpui::{
    Action, App, Context, Entity, EventEmitter, FocusHandle, Focusable, IntoElement, KeyBinding,
    PathPromptOptions, Pixels, Point, Render, SharedString, Subscription, Window, actions, div,
    prelude::*, px,
};
use rudbman_jdbc::LobFile;
use rugpui::{Button, ButtonVariant, ContextMenu, modal, theme};
use rugpui_editor::editor::{
    CloseFind, Copy, Cut, Find, Paste, Redo, RunStatement, SelectAll, Undo,
};
use rugpui_editor::{EditorEvent, EditorView};
use rugpui_grid::{GridCell, GridSource, GridView};
use rugpui_shell::menu_rows::{self, MenuRow};

use crate::data_edit::{EditableSource, StagedCell};
use crate::i18n::ts;

/// Larger values remain fully exportable and replaceable from a file. Only
/// their on-screen prefix is bounded, so truncated text can never overwrite them.
const PREVIEW_BYTES: usize = 1024 * 1024;

actions!(lob_dialog, [StageLob]);

pub(crate) struct Closed;

enum Contents {
    File(LobFile),
    Text { text: String, binary: bool },
}

impl Contents {
    fn stage(self, scalar: bool) -> Result<StagedCell, String> {
        if scalar {
            let text = match self {
                Self::File(file) => {
                    std::fs::read_to_string(file.path()).map_err(|error| error.to_string())?
                }
                Self::Text { text, .. } => text,
            };
            Ok(StagedCell::Text(text))
        } else {
            self.snapshot().map(StagedCell::Lob)
        }
    }

    fn snapshot(self) -> Result<LobFile, String> {
        match self {
            Self::File(file) => Ok(file),
            Self::Text { text, binary } => {
                let bytes = if binary {
                    decode_hex(&text)?
                } else {
                    text.into_bytes()
                };
                LobFile::from_bytes(&bytes, binary).map_err(|error| error.to_string())
            }
        }
    }
}

pub(crate) struct LobDialog {
    grid: Entity<GridView<EditableSource>>,
    row: usize,
    column: usize,
    title: SharedString,
    binary: bool,
    scalar: bool,
    writable: bool,
    nullable: bool,
    file: Option<LobFile>,
    editor: Entity<EditorView>,
    changed: bool,
    text_changed: bool,
    is_null: bool,
    truncated: bool,
    busy: bool,
    error: Option<SharedString>,
    message: Option<SharedString>,
    confirm_close: bool,
    context_menu: Option<Point<Pixels>>,
    /// Whether the text the dialog was opened on differs from the cell's, and
    /// so has to count as a change once it has loaded — see
    /// [`LobDialog::seeded`].
    seeded_change: bool,
    _events: Subscription,
}

impl EventEmitter<Closed> for LobDialog {}

impl LobDialog {
    pub(crate) fn new(
        grid: Entity<GridView<EditableSource>>,
        row: usize,
        column: usize,
        attached: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        Self::open(grid, row, column, attached, None, window, cx)
    }

    /// The dialog over a text cell, starting from `text` rather than from the
    /// value the cell holds: what the grid's one-line field had in it when its
    /// expand button was pressed.
    ///
    /// A text that differs from the cell's is an edit already, so the dialog
    /// opens changed — `Add to changes` stages it, and closing asks first. An
    /// empty text over a null cell is the field left as it was found, and the
    /// cell stays null.
    pub(crate) fn seeded(
        grid: Entity<GridView<EditableSource>>,
        row: usize,
        column: usize,
        attached: bool,
        text: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        Self::open(grid, row, column, attached, Some(text), window, cx)
    }

    fn open(
        grid: Entity<GridView<EditableSource>>,
        row: usize,
        column: usize,
        attached: bool,
        seed: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        cx.bind_keys([KeyBinding::new(
            if cfg!(target_os = "macos") {
                "cmd-enter"
            } else {
                "ctrl-enter"
            },
            StageLob,
            Some("LobEditor"),
        )]);
        let source = grid.read(cx).source();
        let kind = source.lob_kind(column);
        let binary = kind.unwrap_or(false);
        let scalar = kind.is_none();
        let original = if scalar {
            match source.cell(row, column) {
                GridCell::Text(text) => Some(text.to_owned()),
                _ => None,
            }
        } else {
            None
        };
        let seed = seed.filter(|seed| scalar && (original.is_some() || !seed.is_empty()));
        let seeded_change = seed
            .as_ref()
            .is_some_and(|seed| original.as_deref() != Some(seed.as_str()));
        let text = seed.or(original);
        let writable = attached && source.cell_editable(row, column);
        let nullable = source.nullable(column);
        let file = source.lob(row, column).cloned();
        let title = format!(
            "{} · {}",
            if scalar {
                ts!("lob.text").to_string()
            } else if binary {
                "BLOB".into()
            } else {
                "CLOB".into()
            },
            source.column(column).name
        );
        let editor = cx.new(|cx| EditorView::new(cx).read_only(true).word_wrap(!binary));
        let events = cx.subscribe(&editor, |dialog, editor, event, cx| match event {
            EditorEvent::Changed
                if editor.read(cx).is_dirty()
                    && dialog.writable
                    && !dialog.busy
                    && !dialog.truncated =>
            {
                dialog.changed = true;
                dialog.text_changed = true;
                dialog.is_null = false;
                dialog.error = None;
                dialog.message = None;
                cx.notify();
            }
            EditorEvent::ContextMenu { position } => {
                dialog.context_menu = Some(*position);
                cx.notify();
            }
            _ => {}
        });
        window.focus(&editor.read(cx).focus_handle(cx), cx);
        let mut dialog = Self {
            grid,
            row,
            column,
            title: title.into(),
            binary,
            scalar,
            writable,
            nullable,
            is_null: file.is_none() && text.is_none(),
            file,
            editor,
            changed: false,
            text_changed: false,
            truncated: false,
            busy: false,
            error: None,
            message: None,
            confirm_close: false,
            context_menu: None,
            seeded_change,
            _events: events,
        };
        if let Some(text) = text {
            dialog.load_text(text, cx);
        } else {
            dialog.load_preview(cx);
        }
        dialog
    }

    pub(crate) fn has_changes(&self) -> bool {
        self.changed
    }

    pub(crate) fn detach(&mut self, cx: &mut Context<Self>) {
        self.writable = false;
        self.editor
            .update(cx, |editor, cx| editor.set_read_only(true, cx));
        cx.notify();
    }

    fn load_preview(&mut self, cx: &mut Context<Self>) {
        let Some(file) = self.file.clone() else {
            self.editor
                .update(cx, |editor, cx| editor.set_read_only(!self.writable, cx));
            return;
        };
        self.busy = true;
        self.editor
            .update(cx, |editor, cx| editor.set_read_only(true, cx));
        cx.spawn(async move |dialog, cx| {
            let result = cx.background_spawn(async move { preview(&file) }).await;
            dialog
                .update(cx, |dialog, cx| {
                    dialog.busy = false;
                    match result {
                        Ok((text, truncated)) => {
                            dialog.truncated = truncated;
                            // Loaded from a seed that is already an edit: what is
                            // in the editor now is the value to stage.
                            let seeded = std::mem::take(&mut dialog.seeded_change)
                                && dialog.writable
                                && !truncated;
                            dialog.changed |= seeded;
                            dialog.text_changed = seeded;
                            dialog.editor.update(cx, |editor, cx| {
                                editor.set_text(&text, cx);
                                editor.set_read_only(!dialog.writable || truncated, cx);
                            });
                        }
                        Err(error) => dialog.error = Some(error.into()),
                    }
                    cx.notify();
                })
                .ok();
        })
        .detach();
    }

    fn load_text(&mut self, text: String, cx: &mut Context<Self>) {
        self.busy = true;
        cx.spawn(async move |dialog, cx| {
            let result = cx
                .background_spawn(async move { LobFile::from_bytes(text.as_bytes(), false) })
                .await;
            dialog
                .update(cx, |dialog, cx| {
                    dialog.busy = false;
                    match result {
                        Ok(file) => {
                            dialog.file = Some(file);
                            dialog.load_preview(cx);
                        }
                        Err(error) => dialog.error = Some(error.to_string().into()),
                    }
                    cx.notify();
                })
                .ok();
        })
        .detach();
    }

    fn contents(&self, cx: &App) -> Option<Contents> {
        if self.is_null {
            return None;
        }
        if !self.text_changed
            && let Some(file) = &self.file
        {
            Some(Contents::File(file.clone()))
        } else {
            Some(Contents::Text {
                text: self.editor.read(cx).text(),
                binary: self.binary,
            })
        }
    }

    fn empty(&mut self, null: bool, cx: &mut Context<Self>) {
        if !self.writable || self.busy || (null && !self.nullable) {
            return;
        }
        self.file = None;
        self.is_null = null;
        self.changed = true;
        self.text_changed = true;
        self.truncated = false;
        self.error = None;
        self.message = None;
        self.editor.update(cx, |editor, cx| {
            editor.set_text("", cx);
            editor.set_read_only(false, cx);
        });
        cx.notify();
    }

    fn import(&mut self, cx: &mut Context<Self>) {
        if !self.writable || self.busy {
            return;
        }
        let prompt = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some(ts!("lob.import")),
        });
        cx.spawn(async move |dialog, cx| {
            let path = match prompt.await {
                Ok(Ok(Some(paths))) => paths.into_iter().next(),
                Ok(Err(error)) => {
                    dialog
                        .update(cx, |dialog, cx| {
                            dialog.error = Some(error.to_string().into());
                            cx.notify();
                        })
                        .ok();
                    None
                }
                _ => None,
            };
            if let Some(path) = path {
                dialog
                    .update(cx, |dialog, cx| dialog.import_path(path, cx))
                    .ok();
            }
        })
        .detach();
    }

    fn import_path(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        if !self.writable || self.busy {
            return;
        }
        self.busy = true;
        self.error = None;
        self.message = None;
        self.editor
            .update(cx, |editor, cx| editor.set_read_only(true, cx));
        let binary = self.binary;
        cx.spawn(async move |dialog, cx| {
            let result = cx
                .background_spawn(async move { LobFile::from_path(&path, binary) })
                .await;
            dialog
                .update(cx, |dialog, cx| {
                    dialog.busy = false;
                    match result {
                        Ok(file) => {
                            dialog.file = Some(file);
                            dialog.is_null = false;
                            dialog.changed = true;
                            dialog.load_preview(cx);
                        }
                        Err(error) => {
                            dialog.error = Some(ts!("lob.import_error", error = error.to_string()));
                            dialog.editor.update(cx, |editor, cx| {
                                editor.set_read_only(!dialog.writable || dialog.truncated, cx)
                            });
                        }
                    }
                    cx.notify();
                })
                .ok();
        })
        .detach();
        cx.notify();
    }

    fn export(&mut self, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        let Some(contents) = self.contents(cx) else {
            return;
        };
        let name = if self.binary {
            "value.bin"
        } else {
            "value.txt"
        };
        let directory = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        let prompt = cx.prompt_for_new_path(&directory, Some(name));
        cx.spawn(async move |dialog, cx| {
            let path = match prompt.await {
                Ok(Ok(Some(path))) => path,
                Ok(Err(error)) => {
                    dialog
                        .update(cx, |dialog, cx| {
                            dialog.error = Some(error.to_string().into());
                            cx.notify();
                        })
                        .ok();
                    return;
                }
                _ => return,
            };
            let result = cx
                .background_spawn(async move {
                    contents
                        .snapshot()?
                        .export_to(&path)
                        .map_err(|error| error.to_string())
                })
                .await;
            dialog
                .update(cx, |dialog, cx| {
                    match result {
                        Ok(()) => {
                            dialog.error = None;
                            dialog.message = Some(ts!("lob.saved"));
                        }
                        Err(error) => dialog.error = Some(error.into()),
                    }
                    cx.notify();
                })
                .ok();
        })
        .detach();
    }

    fn stage(&mut self, cx: &mut Context<Self>) {
        if !self.writable || self.busy || !self.changed {
            return;
        }
        let contents = self.contents(cx);
        let scalar = self.scalar;
        self.busy = true;
        self.editor
            .update(cx, |editor, cx| editor.set_read_only(true, cx));
        cx.spawn(async move |dialog, cx| {
            let result = cx
                .background_spawn(async move {
                    contents
                        .map(|contents| contents.stage(scalar))
                        .transpose()
                        .map(|value| value.unwrap_or(StagedCell::Null))
                })
                .await;
            dialog
                .update(cx, |dialog, cx| {
                    dialog.busy = false;
                    match result {
                        Ok(value)
                            if dialog.writable
                                && dialog
                                    .grid
                                    .read(cx)
                                    .source()
                                    .cell_editable(dialog.row, dialog.column) =>
                        {
                            dialog.grid.update(cx, |grid, cx| {
                                grid.source_mut(cx).stage(dialog.row, dialog.column, value)
                            });
                            dialog.changed = false;
                            cx.emit(Closed);
                        }
                        Ok(_) => dialog.error = Some(ts!("lob.read_only")),
                        Err(error) => dialog.error = Some(error.into()),
                    }
                    dialog.editor.update(cx, |editor, cx| {
                        editor.set_read_only(!dialog.writable || dialog.truncated, cx)
                    });
                    cx.notify();
                })
                .ok();
        })
        .detach();
        cx.notify();
    }

    pub(crate) fn request_close(&mut self, cx: &mut Context<Self>) {
        if self.context_menu.take().is_some() {
            cx.notify();
            return;
        }
        if self.changed {
            self.confirm_close = true;
            cx.notify();
        } else {
            cx.emit(Closed);
        }
    }

    fn render_menu(&self, cx: &mut Context<Self>) -> Option<impl IntoElement + use<>> {
        let position = self.context_menu?;
        let editor = self.editor.read(cx);
        let focus = editor.focus_handle(cx);
        let writable = !editor.is_read_only();
        let row = |label, action: Box<dyn Action>, enabled| {
            let focus = focus.clone();
            MenuRow::new(label)
                .enabled(enabled)
                .on_activate(move |window, cx| focus.dispatch_action(action.as_ref(), window, cx))
        };
        let rows = vec![
            row(
                ts!("input.menu_cut"),
                Box::new(Cut),
                writable && editor.has_selection(),
            ),
            row(
                ts!("input.menu_copy"),
                Box::new(Copy),
                editor.has_selection(),
            ),
            row(ts!("input.menu_paste"), Box::new(Paste), writable),
            row(ts!("input.menu_select_all"), Box::new(SelectAll), true),
            MenuRow::separator(),
            row(
                ts!("context.undo"),
                Box::new(Undo),
                writable && editor.can_undo(),
            ),
            row(
                ts!("context.redo"),
                Box::new(Redo),
                writable && editor.can_redo(),
            ),
            row(ts!("context.find"), Box::new(Find), true),
        ];
        let this = cx.entity();
        Some(
            ContextMenu::new("lob-editor-menu")
                .position(position)
                .entries(menu_rows::entries(rows))
                .on_dismiss(move |_, cx| {
                    this.update(cx, |dialog, cx| {
                        dialog.context_menu = None;
                        cx.notify();
                    })
                }),
        )
    }
}

impl Focusable for LobDialog {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.editor.read(cx).focus_handle(cx)
    }
}

impl Render for LobDialog {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let chrome = theme(cx);
        let menu = self.render_menu(cx);
        let enabled = self.writable && !self.busy;
        let status = if self.busy {
            ts!("lob.loading")
        } else if self.is_null {
            "NULL".into()
        } else if self.text_changed {
            ts!("lob.modified")
        } else {
            ts!(
                "lob.size",
                bytes = self.file.as_ref().map_or(0, LobFile::byte_len)
            )
        };
        let body = div()
            .key_context("LobEditor")
            .on_action(cx.listener(|dialog, _: &StageLob, _, cx| dialog.stage(cx)))
            .flex()
            .flex_col()
            .min_h_0()
            .gap(px(10.))
            // In the bubble phase, after the editor has had it: with its find
            // bar open the editor closes the bar and the key goes no further,
            // and only with the bar shut does it pass the action on to here,
            // where `Escape` means the viewer. Capturing it first closed the
            // whole viewer under a user who only meant to put the bar away.
            .on_action(cx.listener(|dialog, _: &CloseFind, _, cx| {
                dialog.request_close(cx);
                cx.stop_propagation();
            }))
            // The editor's deeper binding wins Ctrl/Cmd+Enter, but a plain
            // LOB has no SQL statement to run. Capture the action before it.
            .capture_action(cx.listener(|dialog, _: &RunStatement, _, cx| {
                dialog.stage(cx);
                cx.stop_propagation();
            }))
            .on_action(
                cx.listener(|dialog, _: &crate::DismissDialog, _, cx| dialog.request_close(cx)),
            )
            .child(
                div()
                    .flex()
                    .gap(px(8.))
                    .flex_wrap()
                    .child(
                        Button::new("lob-export", ts!("lob.export"))
                            .disabled(self.busy || self.is_null)
                            .on_click(cx.listener(|dialog, _, _, cx| dialog.export(cx))),
                    )
                    .child(
                        Button::new("lob-import", ts!("lob.import"))
                            .disabled(!enabled)
                            .on_click(cx.listener(|dialog, _, _, cx| dialog.import(cx))),
                    )
                    .child(
                        Button::new("lob-empty", ts!("lob.empty"))
                            .disabled(!enabled)
                            .on_click(cx.listener(|dialog, _, _, cx| dialog.empty(false, cx))),
                    )
                    .child(
                        Button::new("lob-null", ts!("data.set_null"))
                            .disabled(!enabled || !self.nullable)
                            .on_click(cx.listener(|dialog, _, _, cx| dialog.empty(true, cx))),
                    ),
            )
            .child(
                div()
                    .text_size(px(11.))
                    .text_color(chrome.text_muted)
                    .child(status),
            )
            .children(self.truncated.then(|| {
                div()
                    .text_size(px(11.))
                    .text_color(chrome.text_muted)
                    .child(ts!("lob.truncated"))
            }))
            .child(
                div()
                    .id("lob-value-editor")
                    .debug_selector(|| "lob-value-editor".into())
                    .h(px(340.))
                    .min_h(px(100.))
                    .flex()
                    .min_w_0()
                    .border_1()
                    .border_color(chrome.border)
                    .font_family(crate::app_settings::monospace_family(cx))
                    .text_size(px(12.))
                    .child(self.editor.clone()),
            )
            .child(
                div()
                    .text_size(px(11.))
                    .text_color(chrome.text_muted)
                    .child(if !self.writable {
                        ts!("lob.read_only")
                    } else if self.binary {
                        ts!("lob.hex_hint")
                    } else {
                        ts!("lob.text_hint")
                    }),
            )
            .children(self.error.clone().map(|error| {
                div()
                    .text_size(px(11.))
                    .text_color(chrome.danger)
                    .child(error)
            }))
            .children(self.message.clone().map(|message| {
                div()
                    .text_size(px(11.))
                    .text_color(chrome.text_muted)
                    .child(message)
            }))
            .children(self.confirm_close.then(|| {
                div()
                    .flex()
                    .flex_col()
                    .gap(px(8.))
                    .child(div().text_size(px(12.)).child(ts!("lob.discard_question")))
                    .child(
                        div()
                            .flex()
                            .gap(px(8.))
                            .justify_end()
                            .child(
                                Button::new("lob-keep-editing", ts!("lob.keep_editing")).on_click(
                                    cx.listener(|dialog, _, _, cx| {
                                        dialog.confirm_close = false;
                                        cx.notify();
                                    }),
                                ),
                            )
                            .child(
                                Button::new("lob-discard", ts!("data.discard"))
                                    .variant(ButtonVariant::Danger)
                                    .on_click(cx.listener(|_, _, _, cx| cx.emit(Closed))),
                            ),
                    )
            }))
            .child(
                div()
                    .flex()
                    .gap(px(8.))
                    .justify_end()
                    .child(
                        Button::new("lob-close", ts!("common.close"))
                            .on_click(cx.listener(|dialog, _, _, cx| dialog.request_close(cx))),
                    )
                    .child(
                        Button::new("lob-stage", ts!("lob.stage"))
                            .variant(ButtonVariant::Primary)
                            .disabled(!enabled || !self.changed)
                            .on_click(cx.listener(|dialog, _, _, cx| dialog.stage(cx))),
                    ),
            );
        let this = cx.entity();
        div()
            .absolute()
            .inset_0()
            .child(modal(
                "lob-dialog",
                self.title.clone(),
                px(760.),
                body,
                move |_, cx| this.update(cx, |dialog, cx| dialog.request_close(cx)),
            ))
            .children(menu)
    }
}

fn preview(file: &LobFile) -> Result<(String, bool), String> {
    let bytes = file
        .prefix(PREVIEW_BYTES)
        .map_err(|error| error.to_string())?;
    let truncated = file.byte_len() > bytes.len() as u64;
    if file.is_binary() {
        use std::fmt::Write;
        let mut text = String::with_capacity(bytes.len() * 3);
        for (index, byte) in bytes.iter().enumerate() {
            if index > 0 {
                text.push(if index % 16 == 0 { '\n' } else { ' ' });
            }
            write!(&mut text, "{byte:02X}").expect("writing to a String");
        }
        Ok((text, truncated))
    } else {
        let end = match std::str::from_utf8(&bytes) {
            Ok(_) => bytes.len(),
            Err(error) if truncated && error.error_len().is_none() => error.valid_up_to(),
            Err(error) => return Err(error.to_string()),
        };
        Ok((
            std::str::from_utf8(&bytes[..end])
                .map_err(|error| error.to_string())?
                .to_owned(),
            truncated,
        ))
    }
}

fn decode_hex(text: &str) -> Result<Vec<u8>, String> {
    let digits: Vec<u8> = text
        .bytes()
        .filter(|byte| !byte.is_ascii_whitespace())
        .collect();
    if !digits.len().is_multiple_of(2) || !digits.iter().all(u8::is_ascii_hexdigit) {
        return Err(ts!("lob.invalid_hex").to_string());
    }
    Ok(digits
        .as_chunks::<2>()
        .0
        .iter()
        .map(|pair| {
            let digit = |byte: u8| (byte as char).to_digit(16).unwrap() as u8;
            digit(pair[0]) * 16 + digit(pair[1])
        })
        .collect())
}

#[cfg(test)]
mod tests;
