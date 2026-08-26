//! The application settings dialog.
//!
//! Edits [`AppSettings`] and nothing else: it reads the current snapshot from
//! [`crate::app_settings`] when it opens, writes the edited copy to disk when
//! the user saves, and replaces the global so the rest of the app picks the
//! change up. Range checking is deliberately *not* duplicated here — the form
//! collects whatever the user typed and [`AppSettings::sanitize`] clamps it once
//! on the way out, which keeps one definition of "valid" in `rudbman-core`.
//!
//! # Live preview
//!
//! Colours and fonts are shown before they are saved, because judging a palette
//! from a swatch is not the same as living in it. The dialog does that without
//! persisting anything: every change to one of those controls publishes the form
//! through [`crate::app_settings::set_preview`] and emits
//! [`SettingsDialogEvent::Previewed`], and the shell re-applies the palettes from
//! whatever that call left in place. Cancelling drops the preview, at which point
//! the same code path resolves back to the saved settings — the revert is the
//! absence of an override rather than a second copy of the settings kept around
//! to restore from.
//!
//! Only the palettes and the fonts work this way. The window's opacity, blur and
//! title bar style all end in a platform call on a live window, and running one
//! of those per keystroke is how gpui's X11 backend was made to panic
//! re-entrantly in the first place; they are applied once, on save, from the
//! shell's event handler.

use std::sync::{Arc, Once};

use gpui::{
    App, Context, DragMoveEvent, Entity, EventEmitter, FocusHandle, Focusable, Hsla, IntoElement,
    KeyBinding, KeyDownEvent, MouseButton, MouseUpEvent, Render, ScrollHandle, SharedString,
    Subscription, Window, actions, div, prelude::*, px,
};
use rudbman_core::{AppSettings, TitlebarStyle};
use rugpui::{
    Button, ButtonVariant, Checkbox, DraggedThumb, EditorTheme, EditorThemeRegistry, SchemePreview,
    SchemeSelect, SchemeSwatch, Scrollbar, ScrollbarAxis, ScrollbarState, Segmented, Select,
    TextInput, Theme, ThemeRegistry, form_row, hide_later, hide_now, modal, scroll_to, scrolled,
    theme,
};
use rugpui_shell::form::{
    format_number, hint, installed_fonts, parse_number, restrict_to_number, section, set_text,
    suffixed, text,
};
use rugpui_shell::{
    CatalogActionEvent, CatalogActions, CatalogFile, EditorThemeCatalog, ThemeCatalog, ThemeEditor,
    ThemeEditorEvent, UiThemeCatalog,
};

use crate::app_settings;
use crate::i18n::{self, ts};
use crate::icons;

/// The dialog's five scrolling surfaces, and the element id of each one's
/// overlay scroll indicator.
///
/// One drag listener answers all of them, so it has to be able to say which bar
/// a drag belongs to; these ids are how, and pairing each with the handle and
/// the state it goes with keeps them from being wired up crosswise.
const SCROLLBARS: [(&str, Surface); 5] = [
    ("settings-body-scrollbar", Surface::Body),
    ("settings-font-scrollbar", Surface::Font),
    ("settings-language-scrollbar", Surface::Language),
    ("settings-ui-theme-scrollbar", Surface::UiTheme),
    ("settings-editor-theme-scrollbar", Surface::EditorTheme),
];

/// Which of the dialog's scrolling surfaces is meant.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Surface {
    /// The dialog body, which scrolls behind the footer.
    Body,
    /// The open font list.
    Font,
    /// The open language list.
    Language,
    /// The open chrome theme list.
    UiTheme,
    /// The open editor theme list.
    EditorTheme,
}

/// Width of the dialog panel.
const DIALOG_WIDTH: f32 = 760.;

/// Height at which the form body starts scrolling.
const BODY_MAX_HEIGHT: f32 = 520.;

/// Segments of the title bar style picker, in [`TitlebarStyle`] order.
///
/// The first half of each pair is an element id and is never translated; only
/// the label is. Built per call rather than declared as a `const` because the
/// labels come out of the active locale.
fn titlebar_options() -> [(&'static str, SharedString); 2] {
    [
        ("custom", ts!("settings.titlebar_custom")),
        ("system", ts!("settings.titlebar_system")),
    ]
}

/// Label of the entry that hands the choice back to the operating system.
///
/// Heads both dropdowns in the dialog, and doubles as their placeholder so a
/// trigger reads the same whether or not its list is open.
fn system_default() -> SharedString {
    ts!("settings.system_default")
}

/// Key context the dialog's own shortcuts are scoped to.
///
/// `Tab` stays scoped here rather than bound globally: a global binding would
/// take the key away from every text field in the window.
const KEY_CONTEXT: &str = "SettingsDialog";

/// Guards the one-time registration of the dialog's key bindings.
static BIND_KEYS: Once = Once::new();

actions!(
    rudbman_settings,
    [
        /// Move focus to the next control in the dialog.
        FocusNext,
        /// Move focus to the previous control in the dialog.
        FocusPrev,
    ]
);

/// Tab order of the form, in visual order, spaced so controls can be inserted
/// later without renumbering.
mod tab {
    /// Chrome theme picker.
    pub const UI_THEME: isize = 10;
    /// First index of the management row under the chrome theme picker.
    pub const UI_THEME_ACTIONS: isize = 11;
    /// Editor theme picker.
    pub const EDITOR_THEME: isize = 20;
    /// First index of the management row under the editor theme picker.
    pub const EDITOR_THEME_ACTIONS: isize = 21;
    /// "The editor theme follows the UI theme" toggle.
    pub const FOLLOWS_UI: isize = 30;
    /// Interface font size.
    pub const UI_FONT_SIZE: isize = 40;
    /// Editor font family.
    pub const EDITOR_FONT_FAMILY: isize = 50;
    /// Editor font size.
    pub const EDITOR_FONT_SIZE: isize = 60;
    /// "Wrap long lines" toggle.
    pub const EDITOR_WORD_WRAP: isize = 65;
    /// Background opacity, in percent.
    pub const OPACITY: isize = 70;
    /// Background blur toggle.
    pub const BLUR: isize = 80;
    /// Title bar style picker.
    pub const TITLEBAR: isize = 90;
    /// Interface language picker.
    pub const LANGUAGE: isize = 100;
    /// Rows per result batch.
    pub const FETCH_BATCH: isize = 110;
    /// Statement timeout.
    pub const QUERY_TIMEOUT: isize = 120;
    /// Default of a new profile's "confirm writes" flag.
    pub const CONFIRM_WRITES: isize = 130;
    /// Java heap ceiling.
    pub const JVM_HEAP: isize = 140;
    /// Extra JVM arguments.
    pub const JVM_ARGS: isize = 150;
    /// Cancel.
    pub const CANCEL: isize = 200;
    /// Save.
    pub const SAVE: isize = 210;
}

/// Emitted by [`SettingsDialog`] when the user acts on it.
pub enum SettingsDialogEvent {
    /// The user saved: the settings global has been replaced and persisted.
    /// The shell should re-apply the settings to the window.
    Applied,
    /// What the form is showing changed in a way the rest of the window has to
    /// follow — a palette was picked, a font was chosen, or a theme file was
    /// written or removed while the dialog stayed open. Nothing has been saved;
    /// the shell re-applies the palettes from
    /// [`crate::app_settings::effective`] and repaints, without taking the focus
    /// off the dialog, which is still on screen.
    Previewed,
    /// The dialog was dismissed without saving.
    Dismissed,
}

/// Which of the dialog's two palette pickers is meant.
///
/// The catalogues themselves are [`rugpui_shell::catalog`]'s; what stays here is
/// the *form field* each one is attached to, because the selected id is a
/// setting and the dialog is what owns those.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Catalog {
    /// The chrome palette.
    UiTheme,
    /// The syntax palette.
    EditorTheme,
}

/// The chrome themes as dropdown entries.
///
/// The pill previews the page it paints, the text on it, and the three hues a
/// palette is actually argued about — the accent and the two status colors —
/// which is as much of a chrome theme as a line of a dropdown can honestly show.
fn ui_theme_swatches(cx: &App) -> Vec<SchemeSwatch> {
    ThemeRegistry::all(cx)
        .into_iter()
        .map(|entry| {
            let palette = ThemeRegistry::resolve(&entry.id, cx);
            SchemeSwatch::new(entry.id, entry.name).preview(SchemePreview {
                background: palette.background,
                foreground: palette.text,
                accents: vec![palette.accent, palette.success, palette.danger],
            })
        })
        .collect()
}

/// The editor themes as dropdown entries.
///
/// The four token colors a reader tells apart first — keyword, string, number
/// and comment — on the editor's own page. A syntax palette really wants to be
/// judged in arrangement, which is what [`rugpui::EditorThemePicker`] is
/// for and what the theme editor still shows; a settings row that has to fit
/// beside a dozen other settings gets the hues and the contrast, which is
/// enough to choose between themes by name.
fn editor_theme_swatches(cx: &App) -> Vec<SchemeSwatch> {
    EditorThemeRegistry::all(cx)
        .into_iter()
        .map(|entry| {
            let palette = EditorThemeRegistry::resolve(&entry.id, cx);
            SchemeSwatch::new(entry.id, entry.name).preview(editor_preview(&palette))
        })
        .collect()
}

/// The pill colors of one editor palette.
fn editor_preview(palette: &EditorTheme) -> SchemePreview {
    SchemePreview {
        background: palette.background,
        foreground: palette.foreground,
        accents: vec![
            palette.keyword,
            palette.string,
            palette.number,
            palette.comment,
        ],
    }
}

/// Which of the dialog's dropdown lists is currently showing.
///
/// A single field rather than one flag per dropdown, so that no two can be open
/// at once — their lists are drawn deferred and would overlap.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OpenList {
    /// The interface language picker.
    Language,
    /// The editor font picker.
    Font,
    /// The chrome theme picker.
    UiTheme,
    /// The editor theme picker.
    EditorTheme,
}

/// Severity of the message strip at the bottom of the dialog.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StatusLevel {
    /// Something went wrong and the settings were not written.
    Error,
}

impl StatusLevel {
    /// Color of the message text under the active theme.
    fn color(self, theme: &Theme) -> Hsla {
        match self {
            Self::Error => theme.danger,
        }
    }
}

/// Modal dialog editing [`rudbman_core::AppSettings`].
///
/// Create it once with [`SettingsDialog::new`], keep the handle, subscribe to
/// [`SettingsDialogEvent`], and render it as the last child of a `relative()`
/// root. It renders nothing while [`SettingsDialog::is_open`] is `false`, so it
/// is safe to render unconditionally.
pub struct SettingsDialog {
    /// Whether the dialog is currently visible.
    open: bool,
    /// Chrome theme id currently selected in the form.
    ui_theme: SharedString,
    /// Editor theme id currently selected in the form.
    editor_theme: SharedString,
    /// Whether the editor theme is picked from the chrome theme's cast.
    editor_theme_follows_ui: bool,
    /// BCP 47 tag of the interface language; `None` follows the system locale.
    /// Holds the tag rather than the label, because the label is what the
    /// dropdown shows and the tag is what gets persisted.
    language: Option<String>,
    /// Whether the editor wraps a line too long for its width.
    editor_word_wrap: bool,
    /// Whether the window should be blurred behind.
    background_blur: bool,
    /// Title bar style currently selected in the form.
    titlebar: TitlebarStyle,
    /// What a newly created connection profile's "confirm writes" starts at.
    confirm_writes_default: bool,
    /// Editor font family; `None` means the per-OS default.
    font_family: Option<SharedString>,
    /// The chrome palettes, as the management row and the editor see them.
    ui_catalog: Arc<dyn ThemeCatalog>,
    /// The syntax palettes, likewise.
    editor_catalog: Arc<dyn ThemeCatalog>,
    /// The management row under the chrome theme picker.
    ui_theme_actions: Entity<CatalogActions>,
    /// The management row under the editor theme picker.
    editor_theme_actions: Entity<CatalogActions>,
    /// Keeps both rows' subscriptions alive.
    catalog_events: Vec<Subscription>,
    /// The colour editor, while one is open. The dialog renders it *instead of*
    /// the form rather than over it; see [`rugpui_shell::theme_editor`].
    editor: Option<Entity<ThemeEditor>>,
    /// Keeps the open editor's subscription alive.
    editor_events: Option<Subscription>,
    /// Message strip shown above the buttons.
    status: Option<SharedString>,
    /// Focus of the dialog root; also the anchor for the `Escape` handler.
    focus_handle: FocusHandle,
    /// Whether focus should move into the form on the next render.
    pending_focus: bool,
    /// Scroll position of the form body, so `Tab` can reveal the section it
    /// just moved into.
    body_scroll: ScrollHandle,
    /// Whether the body's overlay scroll indicator is on screen.
    body_scrollbar: ScrollbarState,
    /// Whether the font list's overlay scroll indicator is on screen.
    font_scrollbar: ScrollbarState,
    /// Whether the language list's overlay scroll indicator is on screen.
    language_scrollbar: ScrollbarState,
    /// Whether the chrome theme list's overlay scroll indicator is on screen.
    ui_theme_scrollbar: ScrollbarState,
    /// Whether the editor theme list's overlay scroll indicator is on screen.
    editor_theme_scrollbar: ScrollbarState,
    /// Index of the section currently scrolled into view. Kept so that tabbing
    /// between two controls of the same section does not re-scroll it.
    visible_section: usize,
    /// Which dropdown, if any, is showing its list.
    open_list: Option<OpenList>,
    /// Font families installed on the machine, read once per opening of the
    /// dialog rather than on every render.
    fonts: Vec<SharedString>,
    /// Scroll position of the font list, so opening it reveals the current
    /// font instead of the top of the alphabet.
    font_scroll: ScrollHandle,
    /// Scroll position of the language list, kept for the same reason.
    language_scroll: ScrollHandle,
    /// Scroll position of the chrome theme list, kept for the same reason.
    ui_theme_scroll: ScrollHandle,
    /// Scroll position of the editor theme list, kept for the same reason.
    editor_theme_scroll: ScrollHandle,
    /// Font size of the interface chrome.
    ui_font_size_input: Entity<TextInput>,
    /// Font size of the SQL editor and the result grid.
    editor_font_size_input: Entity<TextInput>,
    /// Window background opacity, in whole percent.
    opacity_input: Entity<TextInput>,
    /// Rows fetched per result batch.
    fetch_batch_input: Entity<TextInput>,
    /// Statement timeout in seconds; `0` disables it.
    query_timeout_input: Entity<TextInput>,
    /// Java heap ceiling in megabytes.
    jvm_heap_input: Entity<TextInput>,
    /// Extra JVM arguments, separated by spaces.
    jvm_args_input: Entity<TextInput>,
}

impl SettingsDialog {
    /// Build the dialog.
    pub fn new(cx: &mut Context<Self>) -> Self {
        let weak = cx.weak_entity();

        BIND_KEYS.call_once(|| {
            cx.bind_keys([
                KeyBinding::new("tab", FocusNext, Some(KEY_CONTEXT)),
                KeyBinding::new("shift-tab", FocusPrev, Some(KEY_CONTEXT)),
            ]);
        });

        // `Enter` saves from any field. The deferred call is load-bearing:
        // `on_submit` runs while gpui has the TextInput leased, and saving reads
        // every field back.
        let field = {
            let weak = weak.clone();
            move |cx: &mut Context<Self>, placeholder: SharedString, tab_index: isize| {
                let weak = weak.clone();
                cx.new(move |cx| {
                    TextInput::new(cx)
                        .placeholder(placeholder)
                        .tab_index(tab_index)
                        .on_submit(move |_, _window, cx| {
                            let weak = weak.clone();
                            cx.defer(move |cx| {
                                weak.update(cx, |this, cx| this.save(cx)).ok();
                            });
                        })
                })
            }
        };

        // Every placeholder is a sample *value* — a number, or a JVM flag — and
        // reads the same in every language, so none of them has to be revisited
        // when the language changes.
        let ui_font_size_input = field(cx, "14".into(), tab::UI_FONT_SIZE);
        let editor_font_size_input = field(cx, "14".into(), tab::EDITOR_FONT_SIZE);
        let opacity_input = field(cx, "100".into(), tab::OPACITY);
        let fetch_batch_input = field(cx, "500".into(), tab::FETCH_BATCH);
        let query_timeout_input = field(cx, "0".into(), tab::QUERY_TIMEOUT);
        let jvm_heap_input = field(cx, "1024".into(), tab::JVM_HEAP);
        let jvm_args_input = field(cx, "-Dfoo=bar".into(), tab::JVM_ARGS);

        // Numeric fields have no input filter of their own, so each one is
        // sanitised after the fact by an observer.
        restrict_to_number(cx, &ui_font_size_input, true, 5);
        restrict_to_number(cx, &editor_font_size_input, true, 5);
        restrict_to_number(cx, &opacity_input, false, 3);
        restrict_to_number(cx, &fetch_batch_input, false, 6);
        restrict_to_number(cx, &query_timeout_input, false, 6);
        restrict_to_number(cx, &jvm_heap_input, false, 6);

        // The two sizes are previewed as they are typed. Registered after the
        // filter above so that what reaches the preview is the filtered text.
        for input in [&ui_font_size_input, &editor_font_size_input] {
            cx.observe(input, |dialog, _input, cx| dialog.refresh_preview(cx))
                .detach();
        }

        let defaults = AppSettings::default();
        // One pair of catalogues for the life of the dialog: they hold the
        // directories and the id to fall back on, both of which are fixed.
        let dirs = crate::theme_dirs().unwrap_or_else(|err| {
            log::warn!("cannot locate the theme directories: {err:#}");
            rugpui::ThemeDirs {
                ui_themes: std::path::PathBuf::new(),
                editor_themes: None,
            }
        });
        let ui_catalog: Arc<dyn ThemeCatalog> = Arc::new(UiThemeCatalog::new(
            dirs.clone(),
            AppSettings::default().theme,
        ));
        let editor_catalog: Arc<dyn ThemeCatalog> = Arc::new(EditorThemeCatalog::new(
            dirs,
            AppSettings::default().editor_theme,
        ));
        let (ui_theme_actions, ui_theme_events) =
            Self::management_row(Catalog::UiTheme, ui_catalog.clone(), cx);
        let (editor_theme_actions, editor_theme_events) =
            Self::management_row(Catalog::EditorTheme, editor_catalog.clone(), cx);

        Self {
            open: false,
            ui_theme: defaults.theme.into(),
            editor_theme: defaults.editor_theme.into(),
            editor_theme_follows_ui: defaults.editor_theme_follows_ui,
            language: defaults.language,
            editor_word_wrap: defaults.editor_word_wrap,
            background_blur: defaults.window.background_blur,
            titlebar: defaults.window.titlebar,
            confirm_writes_default: defaults.confirm_writes_default,
            font_family: defaults.editor_font_family.map(SharedString::from),
            ui_catalog,
            editor_catalog,
            ui_theme_actions,
            editor_theme_actions,
            catalog_events: vec![ui_theme_events, editor_theme_events],
            editor: None,
            editor_events: None,
            status: None,
            focus_handle: cx.focus_handle(),
            pending_focus: false,
            body_scroll: ScrollHandle::new(),
            body_scrollbar: ScrollbarState::new(),
            font_scrollbar: ScrollbarState::new(),
            language_scrollbar: ScrollbarState::new(),
            ui_theme_scrollbar: ScrollbarState::new(),
            editor_theme_scrollbar: ScrollbarState::new(),
            visible_section: 0,
            open_list: None,
            fonts: Vec::new(),
            font_scroll: ScrollHandle::new(),
            language_scroll: ScrollHandle::new(),
            ui_theme_scroll: ScrollHandle::new(),
            editor_theme_scroll: ScrollHandle::new(),
            ui_font_size_input,
            editor_font_size_input,
            opacity_input,
            fetch_batch_input,
            query_timeout_input,
            jvm_heap_input,
            jvm_args_input,
        }
    }

    /// The handle and bar state of one scrolling surface.
    fn surface(&mut self, surface: Surface) -> (&ScrollHandle, &mut ScrollbarState) {
        match surface {
            Surface::Body => (&self.body_scroll, &mut self.body_scrollbar),
            Surface::Font => (&self.font_scroll, &mut self.font_scrollbar),
            Surface::Language => (&self.language_scroll, &mut self.language_scrollbar),
            Surface::UiTheme => (&self.ui_theme_scroll, &mut self.ui_theme_scrollbar),
            Surface::EditorTheme => (&self.editor_theme_scroll, &mut self.editor_theme_scrollbar),
        }
    }

    /// The same pair, for the renders that only read them.
    fn surface_ref(&self, surface: Surface) -> (&ScrollHandle, &ScrollbarState) {
        match surface {
            Surface::Body => (&self.body_scroll, &self.body_scrollbar),
            Surface::Font => (&self.font_scroll, &self.font_scrollbar),
            Surface::Language => (&self.language_scroll, &self.language_scrollbar),
            Surface::UiTheme => (&self.ui_theme_scroll, &self.ui_theme_scrollbar),
            Surface::EditorTheme => (&self.editor_theme_scroll, &self.editor_theme_scrollbar),
        }
    }

    /// The overlay scroll indicator of one surface, as it stands.
    fn scrollbar(&self, id: &'static str, surface: Surface) -> Scrollbar {
        let (handle, state) = self.surface_ref(surface);
        Scrollbar::for_handle(id, ScrollbarAxis::Vertical, handle).fade(state.fade())
    }

    /// The same bar, listening for the pointer reaching the edge it rides.
    ///
    /// Only the bars that are drawn need it: the one the drag path builds is
    /// there to be measured, and never reaches an element tree.
    fn hovering_scrollbar(
        &self,
        id: &'static str,
        surface: Surface,
        cx: &mut Context<Self>,
    ) -> Scrollbar {
        self.scrollbar(id, surface).on_hover(cx.listener(
            move |dialog, hovered: &bool, _window, cx| {
                dialog.hover_scrollbar(surface, *hovered, cx);
            },
        ))
    }

    /// Puts each surface's bar up whenever it has been scrolled, and starts the
    /// clock that takes it down again.
    fn watch_scroll(&mut self, cx: &mut Context<Self>) {
        for (_, surface) in SCROLLBARS {
            let (handle, state) = self.surface(surface);
            let scrolled = scrolled(handle, ScrollbarAxis::Vertical);
            if let Some(epoch) = state.moved(scrolled) {
                hide_later(epoch, cx, move |dialog| Some(dialog.surface(surface).1));
            }
        }
    }

    /// Scrolls whichever surface's thumb has been dragged.
    fn drag_scrollbar(&mut self, event: &DragMoveEvent<DraggedThumb>, cx: &mut Context<Self>) {
        for (id, surface) in SCROLLBARS {
            let Some(progress) = self.scrollbar(id, surface).dragged(event, cx) else {
                continue;
            };

            let (handle, state) = self.surface(surface);
            state.hold();
            scroll_to(handle, ScrollbarAxis::Vertical, progress);
            cx.notify();
            return;
        }
    }

    /// Lets go of whichever thumb was being held, and starts its clock again.
    fn release_scrollbars(&mut self, cx: &mut Context<Self>) {
        for (_, surface) in SCROLLBARS {
            if let Some(epoch) = self.surface(surface).1.release() {
                hide_later(epoch, cx, move |dialog| Some(dialog.surface(surface).1));
                cx.notify();
            }
        }
    }

    /// Puts one surface's bar up while the pointer rests on the edge it rides,
    /// and starts it going the moment the pointer leaves.
    ///
    /// Told which surface rather than asked to work it out: three bars are on
    /// screen at once at most, and each strip knows only its own.
    fn hover_scrollbar(&mut self, surface: Surface, hovered: bool, cx: &mut Context<Self>) {
        let state = self.surface(surface).1;
        if hovered {
            if state.hover_enter() {
                cx.notify();
            }
            return;
        }

        let Some(epoch) = state.hover_leave() else {
            return;
        };
        hide_now(self, epoch, cx, move |dialog| {
            Some(dialog.surface(surface).1)
        });
    }

    /// Show the dialog, re-reading the current settings into the form.
    pub fn open(&mut self, cx: &mut Context<Self>) {
        let settings = app_settings::current(cx);
        self.fonts = installed_fonts(cx);
        self.fill_form(&settings, cx);
        self.status = None;
        // Fresh rows rather than reset ones: a delete waiting to be confirmed,
        // and whatever the last action had to report, both belong to the
        // session of the dialog that raised them.
        let (ui_theme_actions, ui_theme_events) =
            Self::management_row(Catalog::UiTheme, self.ui_catalog.clone(), cx);
        let (editor_theme_actions, editor_theme_events) =
            Self::management_row(Catalog::EditorTheme, self.editor_catalog.clone(), cx);
        self.ui_theme_actions = ui_theme_actions;
        self.editor_theme_actions = editor_theme_actions;
        self.catalog_events = vec![ui_theme_events, editor_theme_events];
        self.push_selections(cx);
        self.editor = None;
        self.editor_events = None;
        self.open = true;
        self.open_list = None;
        self.pending_focus = true;
        self.visible_section = 0;
        self.body_scroll.scroll_to_item(0);
        cx.notify();
    }

    /// Whether the dialog is visible.
    pub fn is_open(&self) -> bool {
        self.open
    }

    /// Hide the dialog without saving.
    ///
    /// Drops the live preview along with the form, so that whatever the shell
    /// re-applies next resolves to the saved settings again.
    pub fn close(&mut self, cx: &mut Context<Self>) {
        self.open = false;
        self.open_list = None;
        self.pending_focus = false;
        self.status = None;
        self.editor = None;
        self.editor_events = None;
        app_settings::clear_preview(cx);
        cx.notify();
    }

    /// Publish the form as the settings the window should be drawn from, and
    /// ask the shell to re-apply them.
    ///
    /// Called after every change that shows on screen before it is saved, and
    /// after every change to the theme files themselves, since the ids in the
    /// form then resolve to something new.
    fn refresh_preview(&mut self, cx: &mut Context<Self>) {
        let settings = self.collect(cx);
        app_settings::set_preview(settings, cx);
        cx.emit(SettingsDialogEvent::Previewed);
    }

    /// Builds one picker's management row and the subscription that answers it.
    ///
    /// The row owns the five buttons, the delete confirmation and the status
    /// line; what it deliberately does not own is the selection — that is a
    /// form field, saved with the rest of the settings — nor the editor, which
    /// replaces the form rather than standing beside it. Both come back here as
    /// events.
    fn management_row(
        which: Catalog,
        catalog: Arc<dyn ThemeCatalog>,
        cx: &mut Context<Self>,
    ) -> (Entity<CatalogActions>, Subscription) {
        let base = match which {
            Catalog::UiTheme => tab::UI_THEME_ACTIONS,
            Catalog::EditorTheme => tab::EDITOR_THEME_ACTIONS,
        };
        let row = cx.new(|_cx| CatalogActions::new(catalog.clone(), base));
        let events = cx.subscribe(&row, move |dialog, _row, event, cx| match event {
            CatalogActionEvent::Edit { id, file } => {
                dialog.open_editor(catalog.clone(), id.clone(), file, cx);
            }
            CatalogActionEvent::Select(id) => dialog.select(which, id.clone(), cx),
            // Files were written or removed and the registries reloaded, so a
            // palette already in use has to repaint under its new colours
            // without the settings themselves having been saved.
            CatalogActionEvent::Changed => dialog.refresh_preview(cx),
        });
        (row, events)
    }

    /// The row under one catalogue's picker.
    fn actions(&self, catalog: Catalog) -> &Entity<CatalogActions> {
        match catalog {
            Catalog::UiTheme => &self.ui_theme_actions,
            Catalog::EditorTheme => &self.editor_theme_actions,
        }
    }

    /// The id currently highlighted in one catalogue's picker.
    fn selection(&self, catalog: Catalog) -> SharedString {
        match catalog {
            Catalog::UiTheme => self.ui_theme.clone(),
            Catalog::EditorTheme => self.editor_theme.clone(),
        }
    }

    /// Tells both rows which entry their picker is showing.
    ///
    /// Everything a row offers is about the selection, so a row that had not
    /// been told would grey the wrong buttons out.
    fn push_selections(&mut self, cx: &mut Context<Self>) {
        for catalog in [Catalog::UiTheme, Catalog::EditorTheme] {
            let id = self.selection(catalog).to_string();
            self.actions(catalog)
                .clone()
                .update(cx, |row, cx| row.set_selection(id, cx));
        }
    }

    /// Highlights `id` in one catalogue's picker and previews it.
    ///
    /// Nothing is persisted; the preview is dropped again if the dialog is
    /// cancelled, exactly as when the user picks a row of the dropdown.
    fn select(&mut self, catalog: Catalog, id: impl Into<SharedString>, cx: &mut Context<Self>) {
        match catalog {
            Catalog::UiTheme => self.ui_theme = id.into(),
            Catalog::EditorTheme => self.editor_theme = id.into(),
        }
        self.push_selections(cx);
        self.refresh_preview(cx);
        cx.notify();
    }

    /// Puts the editor in front of the form, over `file`.
    fn open_editor(
        &mut self,
        catalog: Arc<dyn ThemeCatalog>,
        id: String,
        file: &CatalogFile,
        cx: &mut Context<Self>,
    ) {
        let editor = cx.new(|cx| ThemeEditor::new(catalog, id, file, cx));
        self.editor_events = Some(cx.subscribe(&editor, |dialog, _editor, event, cx| {
            let saved = matches!(event, ThemeEditorEvent::Saved);
            dialog.close_editor(saved, cx);
        }));
        self.editor = Some(editor);
        self.close_lists(cx);
        cx.notify();
    }

    /// Takes the editor back down and returns to the form.
    ///
    /// When something was written the preview is refreshed, so that a palette
    /// already in use repaints under its new colours without the settings
    /// themselves having to be saved.
    fn close_editor(&mut self, saved: bool, cx: &mut Context<Self>) {
        self.editor = None;
        self.editor_events = None;
        self.pending_focus = true;
        if saved {
            self.refresh_preview(cx);
        }
        cx.notify();
    }

    /// Copy `settings` into every control.
    fn fill_form(&mut self, settings: &AppSettings, cx: &mut Context<Self>) {
        self.ui_theme = settings.theme.clone().into();
        self.editor_theme = settings.editor_theme.clone().into();
        self.editor_theme_follows_ui = settings.editor_theme_follows_ui;
        self.language = settings.language.clone();
        self.editor_word_wrap = settings.editor_word_wrap;
        self.background_blur = settings.window.background_blur;
        self.titlebar = settings.window.titlebar;
        self.confirm_writes_default = settings.confirm_writes_default;
        self.font_family = settings.editor_font_family.clone().map(SharedString::from);

        set_text(
            &self.ui_font_size_input,
            format_number(settings.ui_font_size),
            cx,
        );
        set_text(
            &self.editor_font_size_input,
            format_number(settings.editor_font_size),
            cx,
        );
        let percent = (settings.window.background_opacity * 100.0).round() as i32;
        set_text(&self.opacity_input, percent.to_string(), cx);
        set_text(
            &self.fetch_batch_input,
            settings.fetch_batch_rows.to_string(),
            cx,
        );
        set_text(
            &self.query_timeout_input,
            settings.query_timeout_s.to_string(),
            cx,
        );
        set_text(&self.jvm_heap_input, settings.jvm_heap_mb.to_string(), cx);
        set_text(&self.jvm_args_input, settings.jvm_extra_args.join(" "), cx);
        self.push_selections(cx);
    }

    /// Assemble the form into settings, starting from the persisted snapshot so
    /// that everything the dialog does not edit survives.
    ///
    /// The window's geometry is the reason it starts from the *current* settings
    /// rather than from the ones the form was filled with: the shell records
    /// where the window is as it moves, and a dialog left open across a resize
    /// would otherwise write the old placement back.
    ///
    /// A field the user emptied or made unparseable keeps the value it already
    /// had; nothing here clamps, because [`AppSettings::sanitize`] does that once
    /// for the whole struct.
    fn collect(&self, cx: &App) -> AppSettings {
        let mut settings = app_settings::current(cx);

        settings.theme = self.ui_theme.to_string();
        settings.editor_theme = self.editor_theme.to_string();
        settings.editor_theme_follows_ui = self.editor_theme_follows_ui;
        settings.language = self.language.clone();
        settings.editor_font_family = self.font_family.as_ref().map(ToString::to_string);
        settings.editor_word_wrap = self.editor_word_wrap;
        settings.confirm_writes_default = self.confirm_writes_default;
        settings.window.titlebar = self.titlebar;
        settings.window.background_blur = self.background_blur;

        if let Some(size) = parse_number::<f32>(&self.ui_font_size_input, cx) {
            settings.ui_font_size = size;
        }
        if let Some(size) = parse_number::<f32>(&self.editor_font_size_input, cx) {
            settings.editor_font_size = size;
        }
        if let Some(percent) = parse_number::<f32>(&self.opacity_input, cx) {
            settings.window.background_opacity = percent / 100.0;
        }
        if let Some(rows) = parse_number::<u32>(&self.fetch_batch_input, cx) {
            settings.fetch_batch_rows = rows;
        }
        if let Some(seconds) = parse_number::<u32>(&self.query_timeout_input, cx) {
            settings.query_timeout_s = seconds;
        }
        if let Some(heap) = parse_number::<u32>(&self.jvm_heap_input, cx) {
            settings.jvm_heap_mb = heap;
        }
        settings.jvm_extra_args = split_arguments(text(&self.jvm_args_input, cx).as_str());

        settings
    }

    /// Persist the form and apply it, or report why it could not be written.
    ///
    /// A failed write leaves the dialog open with the message showing, so the
    /// user never believes a setting took effect when it did not.
    fn save(&mut self, cx: &mut Context<Self>) {
        let mut settings = self.collect(cx);
        settings.sanitize();

        if let Err(err) = settings.save() {
            log::error!("could not write settings.json: {err:#}");
            self.status = Some(ts!("settings.save_failed", error = format!("{err:#}")));
            // Show the clamped values so the user sees what would be stored.
            self.fill_form(&settings, cx);
            cx.notify();
            return;
        }

        app_settings::replace(settings, cx);
        cx.emit(SettingsDialogEvent::Applied);
        self.close(cx);
    }

    /// Close the dialog and report that nothing was saved.
    fn dismiss(&mut self, cx: &mut Context<Self>) {
        cx.emit(SettingsDialogEvent::Dismissed);
        self.close(cx);
    }

    /// `Tab`: move focus to the next control. gpui's tab ring wraps on its own.
    fn focus_next(&mut self, _: &FocusNext, window: &mut Window, cx: &mut Context<Self>) {
        self.close_lists(cx);
        window.focus_next(cx);
        self.reveal_focused(window, cx);
    }

    /// `Shift+Tab`: move focus to the previous control, wrapping to the last.
    fn focus_prev(&mut self, _: &FocusPrev, window: &mut Window, cx: &mut Context<Self>) {
        self.close_lists(cx);
        window.focus_prev(cx);
        self.reveal_focused(window, cx);
    }

    /// Scroll the section holding the focused control into view.
    ///
    /// Without this a focus ring below the fold would be invisible, which is the
    /// same as having no focus indicator at all. The section is derived from the
    /// focused handle's tab index, so no per-control bookkeeping is needed for
    /// the controls whose focus handles gpui creates itself.
    ///
    /// Silent while the editor is up: the tab indices then belong to *its* ring,
    /// and reading them as sections would scroll a form nobody can see to
    /// wherever the editor's last field happened to land.
    fn reveal_focused(&mut self, window: &Window, cx: &mut Context<Self>) {
        if self.editor.is_some() {
            return;
        }
        let Some(handle) = window.focused(cx) else {
            return;
        };
        if section_of(handle.tab_index) != self.visible_section {
            self.visible_section = section_of(handle.tab_index);
            self.body_scroll.scroll_to_item(self.visible_section);
            cx.notify();
        }
    }

    /// What `Escape` means, one layer at a time.
    ///
    /// Anything layered on top of the form takes the key first and only undoes
    /// itself, so that backing out of a list, a delete confirmation or the
    /// colour editor does not also throw away the whole form. The editor is
    /// checked before the dropdowns because it replaces the form outright:
    /// while it is up there is no list to close. A management row's delete
    /// confirmation is checked last, ahead of dismissing the dialog outright,
    /// through [`CatalogActions::is_confirming`] and
    /// [`CatalogActions::cancel_confirm`] — the row itself offers no other way
    /// to ask whether one is showing.
    ///
    /// Public because the key does not actually arrive here: gpui matches key
    /// bindings before it delivers key events, so the shell's `Escape` binding
    /// wins and calls this. [`SettingsDialog::on_key_down`] is the fallback for
    /// a dispatch that lets the key through instead.
    pub fn escape(&mut self, cx: &mut Context<Self>) {
        if let Some(editor) = self.editor.clone() {
            editor.update(cx, |editor, cx| editor.cancel(cx));
            return;
        }
        if self.open_list.is_some() {
            self.close_lists(cx);
            return;
        }
        for catalog in [Catalog::UiTheme, Catalog::EditorTheme] {
            let row = self.actions(catalog).clone();
            if row.read(cx).is_confirming() {
                row.update(cx, |row, cx| row.cancel_confirm(cx));
                return;
            }
        }
        self.dismiss(cx);
    }

    /// `Escape` dismisses the dialog from anywhere inside it.
    ///
    /// See [`SettingsDialog::escape`] for why this rarely runs.
    fn on_key_down(&mut self, event: &KeyDownEvent, _window: &mut Window, cx: &mut Context<Self>) {
        if !self.open || event.keystroke.key != "escape" {
            return;
        }
        cx.stop_propagation();
        self.escape(cx);
    }

    /// Hide whichever dropdown list is showing.
    ///
    /// Called whenever focus leaves a dropdown, so that a list nobody is driving
    /// any more does not stay painted over the rest of the form.
    fn close_lists(&mut self, cx: &mut Context<Self>) {
        if self.open_list.take().is_some() {
            cx.notify();
        }
    }

    /// The entries of the font dropdown: the "leave it to the OS" row first,
    /// then every installed family.
    ///
    /// A saved font that is not installed — a hand-edited `settings.json`, or a
    /// family that has since been removed — is spliced in after the first row,
    /// so the trigger keeps showing it instead of silently falling back.
    fn font_options(&self) -> Vec<SharedString> {
        let mut options = Vec::with_capacity(self.fonts.len() + 2);
        options.push(system_default());
        options.extend(
            self.font_family
                .clone()
                .filter(|family| !self.fonts.contains(family)),
        );
        options.extend(self.fonts.iter().cloned());
        options
    }

    /// The entries of the language dropdown: "follow the system" first, then
    /// every shipped translation named in its own language.
    fn language_options() -> Vec<SharedString> {
        let supported = i18n::supported();
        let mut options = Vec::with_capacity(supported.len() + 1);
        options.push(system_default());
        options.extend(supported.iter().map(|(_, name)| name.clone()));
        options
    }

    /// Show or hide `list`, revealing the current entry as it opens.
    ///
    /// Opening one list closes the other, since both are drawn deferred and two
    /// open at once would paint over each other.
    fn set_list_open(&mut self, list: OpenList, open: bool, cx: &mut Context<Self>) {
        self.open_list = open.then_some(list);
        if open {
            let (scroll, current) = match list {
                OpenList::Font => {
                    let options = self.font_options();
                    let current = self
                        .font_family
                        .as_ref()
                        .and_then(|family| options.iter().position(|option| option == family));
                    (&self.font_scroll, current)
                }
                OpenList::Language => (&self.language_scroll, self.language_index()),
                // Positions are read off the registries rather than off the
                // swatches, which keep their ids to themselves; the dropdowns
                // are built from those same lists, in that same order.
                OpenList::UiTheme => {
                    let current = ThemeRegistry::all(cx)
                        .iter()
                        .position(|entry| *entry.id == *self.ui_theme);
                    (&self.ui_theme_scroll, current)
                }
                OpenList::EditorTheme => {
                    let current = EditorThemeRegistry::all(cx)
                        .iter()
                        .position(|entry| *entry.id == *self.editor_theme);
                    (&self.editor_theme_scroll, current)
                }
            };
            scroll.scroll_to_item(current.unwrap_or(0));
        }
        cx.notify();
    }

    /// Position of the selected language in [`Self::language_options`], or
    /// `None` while the language follows the system — or names a tag rudbman has
    /// no translation for, which the app treats the same way.
    fn language_index(&self) -> Option<usize> {
        let tag = self.language.as_deref()?;
        let index = i18n::supported()
            .iter()
            .position(|(code, _)| *code == tag)?;
        Some(index + 1)
    }

    /// Move focus into the first control when the dialog opens.
    ///
    /// Skipped while an editor is up: the editor moves focus into its own name
    /// field, and two views claiming the focus in one frame would leave it
    /// wherever the second one happened to run.
    fn apply_pending_focus(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.pending_focus || self.editor.is_some() {
            return;
        }
        self.pending_focus = false;
        let handle = self.ui_font_size_input.read(cx).focus_handle(cx);
        window.focus(&handle, cx);
    }

    /// The editor theme dropdown, or — while the choice follows the chrome theme
    /// — a dead trigger showing what the app picked instead.
    ///
    /// Disabled rather than merely ignored: the setting says the app decides, so
    /// offering a working dropdown whose every choice would be silently
    /// discarded would be a lie. It is also why
    /// [`crate::editor_theme_for`] prefers the chrome theme's namesake over the
    /// configured id — there is no live pick here to preserve. Showing the
    /// resolved theme keeps the row informative, since that answer moves with
    /// the chrome theme picked in the row above — and the disabled trigger is
    /// the same line in the same place as the live one, so ticking the box
    /// shifts nothing on screen but the colours.
    fn render_editor_theme(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let this = cx.entity();

        if self.editor_theme_follows_ui {
            let resolved = crate::editor_theme_for(
                &self.editor_theme,
                true,
                &self.ui_theme,
                ThemeRegistry::resolve(&self.ui_theme, cx).dark,
                &EditorThemeRegistry::all(cx),
            );
            let name = EditorThemeRegistry::all(cx)
                .into_iter()
                .find(|entry| entry.id == resolved)
                .map(|entry| entry.name)
                .unwrap_or_else(|| resolved.clone());
            let palette = EditorThemeRegistry::resolve(&resolved, cx);

            return SchemeSelect::new("settings-editor-theme-followed")
                .chevron_icon(icons::CHEVRON_DOWN)
                .options([
                    SchemeSwatch::new(resolved.clone(), name).preview(editor_preview(&palette))
                ])
                .selected(Some(resolved))
                .disabled(true)
                .into_any_element();
        }

        let bar = self.hovering_scrollbar(SCROLLBARS[4].0, Surface::EditorTheme, cx);

        SchemeSelect::new("settings-editor-theme")
            .chevron_icon(icons::CHEVRON_DOWN)
            .options(editor_theme_swatches(cx))
            .selected(Some(self.editor_theme.clone()))
            .open(self.open_list == Some(OpenList::EditorTheme))
            .tab_index(tab::EDITOR_THEME)
            .scroll_handle(self.editor_theme_scroll.clone())
            .scrollbar(bar)
            .on_select({
                let this = this.clone();
                move |id, _window, cx| {
                    let id = SharedString::from(id.to_owned());
                    this.update(cx, |dialog, cx| {
                        dialog.select(Catalog::EditorTheme, id, cx);
                    });
                }
            })
            .on_open_change(move |open, _window, cx| {
                this.update(cx, |dialog, cx| {
                    dialog.set_list_open(OpenList::EditorTheme, open, cx);
                });
            })
            .into_any_element()
    }

    /// The "Appearance" section: both palettes and both fonts.
    fn render_appearance(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let this = cx.entity();
        let font_bar = self.hovering_scrollbar(SCROLLBARS[1].0, Surface::Font, cx);
        let ui_theme_bar = self.hovering_scrollbar(SCROLLBARS[3].0, Surface::UiTheme, cx);
        // Built before the section is assembled, because `section` borrows the
        // context to read the theme and these borrow it mutably to listen.
        let theme_actions = self.ui_theme_actions.clone();
        let editor_theme_actions = self.editor_theme_actions.clone();
        let editor_theme = self.render_editor_theme(cx);

        let theme_picker = SchemeSelect::new("settings-ui-theme")
            .chevron_icon(icons::CHEVRON_DOWN)
            .options(ui_theme_swatches(cx))
            .selected(Some(self.ui_theme.clone()))
            .open(self.open_list == Some(OpenList::UiTheme))
            .tab_index(tab::UI_THEME)
            .scroll_handle(self.ui_theme_scroll.clone())
            .scrollbar(ui_theme_bar)
            .on_select({
                let this = this.clone();
                move |id, _window, cx| {
                    let id = SharedString::from(id.to_owned());
                    this.update(cx, |dialog, cx| dialog.select(Catalog::UiTheme, id, cx));
                }
            })
            .on_open_change({
                let this = this.clone();
                move |open, _window, cx| {
                    this.update(cx, |dialog, cx| {
                        dialog.set_list_open(OpenList::UiTheme, open, cx);
                    });
                }
            });

        let follows_ui = Checkbox::new("settings-follows-ui", ts!("settings.editor_follows_ui"))
            .checked(self.editor_theme_follows_ui)
            .tab_index(tab::FOLLOWS_UI)
            .on_toggle({
                let this = this.clone();
                move |checked, _window, cx| {
                    this.update(cx, |dialog, cx| {
                        dialog.editor_theme_follows_ui = checked;
                        dialog.refresh_preview(cx);
                        cx.notify();
                    });
                }
            });

        // No preview: wrapping is state the editors hold, and the shell only
        // hands it over on save, so previewing it would show a change the
        // Cancel button could not take back.
        let word_wrap = Checkbox::new(
            "settings-editor-word-wrap",
            ts!("settings.editor_word_wrap"),
        )
        .checked(self.editor_word_wrap)
        .tab_index(tab::EDITOR_WORD_WRAP)
        .on_toggle({
            let this = this.clone();
            move |checked, _window, cx| {
                this.update(cx, |dialog, cx| {
                    dialog.editor_word_wrap = checked;
                    cx.notify();
                });
            }
        });

        let font = Select::new("settings-editor-font")
            .chevron_icon(icons::CHEVRON_DOWN)
            .options(self.font_options())
            .selected(self.font_family.clone())
            .placeholder(system_default())
            .open(self.open_list == Some(OpenList::Font))
            .tab_index(tab::EDITOR_FONT_FAMILY)
            .scroll_handle(self.font_scroll.clone())
            .scrollbar(font_bar)
            .on_select({
                let this = this.clone();
                // Row 0 is the "leave it to the OS" entry; comparing its label
                // against the picked text would only work in one language.
                move |index, family, _window, cx| {
                    let family = (index > 0).then(|| SharedString::from(family.to_owned()));
                    this.update(cx, |dialog, cx| {
                        dialog.font_family = family;
                        dialog.refresh_preview(cx);
                        cx.notify();
                    });
                }
            })
            .on_open_change({
                let this = this.clone();
                move |open, _window, cx| {
                    this.update(cx, |dialog, cx| {
                        dialog.set_list_open(OpenList::Font, open, cx);
                    });
                }
            });

        section(
            ts!("settings.section.appearance"),
            cx,
            div()
                .flex()
                .flex_col()
                .gap(px(10.))
                .child(form_row(
                    ts!("settings.ui_theme"),
                    div()
                        .flex()
                        .flex_col()
                        .gap(px(6.))
                        .child(theme_picker)
                        .child(theme_actions),
                ))
                .child(form_row("", follows_ui))
                .child(form_row(
                    ts!("settings.editor_theme"),
                    div()
                        .flex()
                        .flex_col()
                        .gap(px(6.))
                        .child(editor_theme)
                        .child(editor_theme_actions),
                ))
                .child(form_row(
                    ts!("settings.ui_font_size"),
                    suffixed(
                        self.ui_font_size_input.clone(),
                        ts!("settings.font_size_hint"),
                        cx,
                    ),
                ))
                .child(form_row(ts!("settings.editor_font"), font))
                .child(form_row(
                    ts!("settings.editor_font_size"),
                    suffixed(
                        self.editor_font_size_input.clone(),
                        ts!("settings.font_size_hint"),
                        cx,
                    ),
                ))
                .child(form_row("", word_wrap)),
        )
    }

    /// The "Window" section.
    fn render_window(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let this = cx.entity();

        let blur = Checkbox::new("settings-blur", ts!("settings.blur"))
            .checked(self.background_blur)
            .tab_index(tab::BLUR)
            .on_toggle({
                let this = this.clone();
                move |checked, _window, cx| {
                    this.update(cx, |dialog, cx| {
                        dialog.background_blur = checked;
                        cx.notify();
                    });
                }
            });

        let titlebar = Segmented::new("settings-titlebar")
            .options(titlebar_options())
            .selected(match self.titlebar {
                TitlebarStyle::Custom => 0,
                TitlebarStyle::System => 1,
            })
            .tab_index(tab::TITLEBAR)
            .on_select({
                let this = this.clone();
                move |index, _window, cx| {
                    this.update(cx, |dialog, cx| {
                        dialog.titlebar = if index == 1 {
                            TitlebarStyle::System
                        } else {
                            TitlebarStyle::Custom
                        };
                        cx.notify();
                    });
                }
            });

        section(
            ts!("settings.section.window"),
            cx,
            div()
                .flex()
                .flex_col()
                .gap(px(10.))
                .child(form_row(
                    ts!("settings.opacity"),
                    suffixed(self.opacity_input.clone(), ts!("settings.opacity_hint"), cx),
                ))
                .child(form_row("", blur))
                .child(form_row(ts!("settings.titlebar"), titlebar)),
        )
    }

    /// The "Language" section.
    fn render_language(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let this = cx.entity();
        let language_bar = self.hovering_scrollbar(SCROLLBARS[2].0, Surface::Language, cx);

        let language = Select::new("settings-language")
            .chevron_icon(icons::CHEVRON_DOWN)
            .options(Self::language_options())
            .selected(self.language.as_deref().and_then(i18n::display_name))
            .placeholder(system_default())
            .open(self.open_list == Some(OpenList::Language))
            .tab_index(tab::LANGUAGE)
            .scroll_handle(self.language_scroll.clone())
            .scrollbar(language_bar)
            .on_select({
                let this = this.clone();
                // By index, not by label: row 0 is "follow the system" and the
                // rest line up with `i18n::supported`, whereas the labels are
                // endonyms that say nothing about their position.
                move |index, _label, _window, cx| {
                    let tag = index
                        .checked_sub(1)
                        .and_then(|index| i18n::supported().get(index))
                        .map(|(code, _)| (*code).to_owned());
                    this.update(cx, |dialog, cx| {
                        dialog.language = tag;
                        cx.notify();
                    });
                }
            })
            .on_open_change({
                let this = this.clone();
                move |open, _window, cx| {
                    this.update(cx, |dialog, cx| {
                        dialog.set_list_open(OpenList::Language, open, cx);
                    });
                }
            });

        section(
            ts!("settings.section.language"),
            cx,
            div()
                .flex()
                .flex_col()
                .gap(px(10.))
                .child(form_row(ts!("settings.language"), language))
                .child(hint(ts!("settings.language_hint"), cx)),
        )
    }

    /// The "Database" section.
    fn render_database(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let this = cx.entity();

        let confirm_writes = Checkbox::new(
            "settings-confirm-writes",
            ts!("settings.confirm_writes_default"),
        )
        .checked(self.confirm_writes_default)
        .tab_index(tab::CONFIRM_WRITES)
        .on_toggle(move |checked, _window, cx| {
            this.update(cx, |dialog, cx| {
                dialog.confirm_writes_default = checked;
                cx.notify();
            });
        });

        section(
            ts!("settings.section.database"),
            cx,
            div()
                .flex()
                .flex_col()
                .gap(px(10.))
                .child(form_row(
                    ts!("settings.fetch_batch_rows"),
                    suffixed(
                        self.fetch_batch_input.clone(),
                        ts!("settings.fetch_batch_rows_hint"),
                        cx,
                    ),
                ))
                .child(form_row(
                    ts!("settings.query_timeout"),
                    suffixed(
                        self.query_timeout_input.clone(),
                        ts!("settings.query_timeout_hint"),
                        cx,
                    ),
                ))
                .child(form_row("", confirm_writes)),
        )
    }

    /// The "Java virtual machine" section.
    fn render_jvm(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        section(
            ts!("settings.section.jvm"),
            cx,
            div()
                .flex()
                .flex_col()
                .gap(px(10.))
                .child(form_row(
                    ts!("settings.jvm_heap"),
                    suffixed(
                        self.jvm_heap_input.clone(),
                        ts!("settings.jvm_heap_hint"),
                        cx,
                    ),
                ))
                .child(form_row(
                    ts!("settings.jvm_args"),
                    self.jvm_args_input.clone(),
                ))
                .child(hint(ts!("settings.jvm_hint"), cx)),
        )
    }

    /// The scrolling form and the footer under it — the dialog's own body.
    ///
    /// Takes the body's overlay bar and the resolved theme rather than fetching
    /// them, because the caller has already had to work both out to decide
    /// whether this is what the modal is showing at all.
    fn render_form(
        &self,
        body_bar: Scrollbar,
        chrome: &Theme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        // The `min_h_0` chain lets the scroll area shrink below its cap when the
        // modal hits the window height, keeping the footer on screen.
        div()
            .flex()
            .flex_col()
            .min_h_0()
            .gap(px(12.))
            .child(
                // The middle box exists only to hold the overlay bar: a
                // scrolling box cannot, because its children are what scroll
                // away underneath it.
                div()
                    .relative()
                    .flex()
                    .flex_col()
                    .min_h_0()
                    .child(
                        div()
                            .id("settings-body")
                            .track_scroll(&self.body_scroll)
                            .flex()
                            .flex_col()
                            .min_h_0()
                            .gap(px(14.))
                            .max_h(px(BODY_MAX_HEIGHT))
                            .overflow_y_scroll()
                            .restrict_scroll_to_axis()
                            .child(self.render_appearance(cx))
                            .child(self.render_window(cx))
                            .child(self.render_language(cx))
                            .child(self.render_database(cx))
                            .child(self.render_jvm(cx)),
                    )
                    .children(body_bar.render(chrome)),
            )
            .child(self.render_footer(cx))
    }

    /// The message strip and the action buttons.
    fn render_footer(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let chrome = theme(cx);
        let this = cx.entity();

        let status = self.status.clone().map(|message| {
            div()
                .text_size(px(12.))
                .text_color(StatusLevel::Error.color(&chrome))
                .child(message)
        });

        div()
            .flex()
            .flex_col()
            .flex_none()
            .gap(px(10.))
            .child(div().h(px(1.)).w_full().flex_none().bg(chrome.border))
            .children(status)
            .child(
                div()
                    .flex()
                    .flex_row()
                    .justify_end()
                    .gap(px(8.))
                    .child(
                        Button::new("settings-cancel", ts!("common.cancel"))
                            .variant(ButtonVariant::Secondary)
                            .tab_index(tab::CANCEL)
                            .on_click({
                                let this = this.clone();
                                move |_, _window, cx| {
                                    this.update(cx, |dialog, cx| dialog.dismiss(cx));
                                }
                            }),
                    )
                    .child(
                        Button::new("settings-save", ts!("common.save"))
                            .variant(ButtonVariant::Primary)
                            .tab_index(tab::SAVE)
                            .on_click({
                                let this = this.clone();
                                move |_, _window, cx| {
                                    this.update(cx, |dialog, cx| dialog.save(cx));
                                }
                            }),
                    ),
            )
    }
}

impl EventEmitter<SettingsDialogEvent> for SettingsDialog {}

impl Focusable for SettingsDialog {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for SettingsDialog {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if !self.open {
            return div().id("settings-dialog");
        }

        self.apply_pending_focus(window, cx);
        self.watch_scroll(cx);
        let chrome = theme(cx);
        let body_bar = self.hovering_scrollbar(SCROLLBARS[0].0, Surface::Body, cx);

        // While a colour is being edited the form steps aside entirely rather
        // than being covered up, so that the window's tab ring holds only the
        // controls that are actually on screen; see
        // [`rugpui_shell::theme_editor`]. The form is not even built in that
        // case — it would be built afresh on every keystroke in the editor and
        // thrown away again.
        let (title, body) = match self.editor.clone() {
            Some(editor) => (editor.read(cx).title(cx), editor.into_any_element()),
            None => (
                ts!("settings.title"),
                self.render_form(body_bar, &chrome, cx).into_any_element(),
            ),
        };

        // A click on the backdrop backs out of whatever is in front: the editor
        // while one is open, otherwise the dialog itself. Anything else would
        // discard an unsaved palette by way of a stray click.
        let on_dismiss = {
            let this = cx.entity();
            move |_window: &mut Window, cx: &mut App| {
                this.update(cx, |dialog, cx| match dialog.editor.clone() {
                    Some(editor) => editor.update(cx, |editor, cx| editor.cancel(cx)),
                    None => dialog.dismiss(cx),
                });
            }
        };

        // Absolute and full-size for the same reason as the about dialog: an
        // absolutely positioned child is laid out against its direct parent.
        div()
            .id("settings-dialog")
            .key_context(KEY_CONTEXT)
            .absolute()
            .inset_0()
            .size_full()
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(Self::focus_next))
            .on_action(cx.listener(Self::focus_prev))
            .on_key_down(cx.listener(Self::on_key_down))
            // All three overlay bars are answered from here: gpui hands a drag
            // move to every listener of that type wherever it sits, and this is
            // the one element mounted for the whole of any of them — the open
            // list a thumb belongs to is torn down the moment the pointer picks
            // an option, and the body scrolls away under its own.
            .on_drag_move::<DraggedThumb>(cx.listener(
                |dialog, event: &DragMoveEvent<DraggedThumb>, _window, cx| {
                    dialog.drag_scrollbar(event, cx);
                },
            ))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|dialog, _: &MouseUpEvent, _window, cx| {
                    dialog.release_scrollbars(cx);
                }),
            )
            .on_mouse_up_out(
                MouseButton::Left,
                cx.listener(|dialog, _: &MouseUpEvent, _window, cx| {
                    dialog.release_scrollbars(cx);
                }),
            )
            .child(modal(
                "settings-modal",
                title,
                px(DIALOG_WIDTH),
                body,
                on_dismiss,
            ))
    }
}

/// Which section of the form a tab index belongs to.
///
/// The body scrolls by item index, and every section is one item, so this is
/// what turns "the focus moved" into "scroll this far". Kept beside the tab
/// table rather than inside the method that uses it so the two can be checked
/// against each other in a test.
fn section_of(tab_index: isize) -> usize {
    match tab_index {
        index if index <= tab::EDITOR_WORD_WRAP => 0,
        index if index <= tab::TITLEBAR => 1,
        index if index <= tab::LANGUAGE => 2,
        index if index <= tab::CONFIRM_WRITES => 3,
        _ => 4,
    }
}

/// Splits the extra JVM arguments field into the arguments it names.
///
/// Whitespace separated, which is what the field's one-line shape allows and
/// what a user typing `-Xss4m -Dfoo=bar` expects. An argument that has to
/// *contain* a space cannot be written here; `settings.json` takes a JSON array
/// and is the escape hatch for that. Empty runs are dropped rather than passed
/// on, since the JVM rejects an empty argument outright.
fn split_arguments(value: &str) -> Vec<String> {
    value.split_whitespace().map(ToOwned::to_owned).collect()
}

#[cfg(test)]
mod tests {
    use rudbman_core::WindowState;

    use super::*;

    /// Settings that are nothing like the defaults, so a field the form drops
    /// on the way through shows up as a default rather than as itself.
    fn edited() -> AppSettings {
        AppSettings {
            theme: "gruvbox-dark".to_string(),
            editor_theme: "tokyo-night".to_string(),
            editor_theme_follows_ui: false,
            language: Some("ko".to_string()),
            ui_font_size: 15.0,
            editor_font_family: Some("Cascadia Mono".to_string()),
            editor_font_size: 16.5,
            editor_word_wrap: true,
            jvm_heap_mb: 4096,
            jvm_extra_args: vec!["-Xss4m".to_string(), "-Dfoo=bar".to_string()],
            fetch_batch_rows: 1_000,
            query_timeout_s: 30,
            confirm_writes_default: false,
            window: WindowState {
                x: Some(120),
                y: Some(60),
                width: 1600,
                height: 1000,
                maximized: true,
                background_opacity: 0.8,
                background_blur: true,
                titlebar: TitlebarStyle::System,
            },
            ..AppSettings::default()
        }
    }

    /// The whole of what the dialog is for: every setting it edits has to
    /// survive being written into the form, read back out, saved and reloaded.
    /// A field that reaches the form but never comes back — a number formatted
    /// one way and parsed another, a percentage that loses its last digit — is
    /// invisible until someone notices their setting quietly reverting.
    #[gpui::test]
    fn the_form_round_trips_through_the_settings_file(cx: &mut gpui::TestAppContext) {
        let original = edited();
        let dialog = cx.update(|cx| {
            // `collect` starts from the persisted snapshot, so the geometry and
            // any unknown keys have to be in place for the comparison to mean
            // anything.
            app_settings::replace(original.clone(), cx);
            cx.new(SettingsDialog::new)
        });

        let collected = cx.update(|cx| {
            dialog.update(cx, |dialog, cx| {
                dialog.fill_form(&original, cx);
                let mut settings = dialog.collect(cx);
                settings.sanitize();
                settings
            })
        });
        assert_eq!(collected, original);

        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("settings.json");
        collected.save_to(&path).expect("save");
        assert_eq!(AppSettings::load_from(&path).expect("load"), original);
    }

    /// The word-wrap switch is the one appearance control the dialog does not
    /// preview — the editors are handed the change on save — so it has to reach
    /// `collect` on its own, and cancelling has to leave the saved answer alone.
    #[gpui::test]
    fn the_word_wrap_switch_reaches_the_settings_without_a_preview(cx: &mut gpui::TestAppContext) {
        let saved = AppSettings::default();
        assert!(
            !saved.editor_word_wrap,
            "the default is what is flipped below"
        );
        let dialog = cx.update(|cx| {
            app_settings::replace(saved.clone(), cx);
            let dialog = cx.new(SettingsDialog::new);
            dialog.update(cx, |dialog, cx| dialog.fill_form(&saved, cx));
            dialog
        });

        // What the checkbox's `on_toggle` does.
        cx.update(|cx| {
            dialog.update(cx, |dialog, cx| {
                dialog.editor_word_wrap = true;
                cx.notify();
            })
        });

        cx.update(|cx| {
            assert!(dialog.read(cx).collect(cx).editor_word_wrap);
            // Neither the saved settings nor the preview moved with it.
            assert!(!app_settings::current(cx).editor_word_wrap);
            assert!(!app_settings::effective(cx).editor_word_wrap);
        });

        // And filling the form again puts the saved answer back on the switch.
        cx.update(|cx| dialog.update(cx, |dialog, cx| dialog.fill_form(&saved, cx)));
        cx.update(|cx| assert!(!dialog.read(cx).editor_word_wrap));
    }

    /// Nothing typed into the form may reach the disk on its own, and closing
    /// the dialog has to leave the window drawn from the saved settings again.
    #[gpui::test]
    fn a_cancelled_edit_leaves_nothing_behind(cx: &mut gpui::TestAppContext) {
        let saved = AppSettings::default();
        let dialog = cx.update(|cx| {
            app_settings::replace(saved.clone(), cx);
            cx.new(SettingsDialog::new)
        });

        cx.update(|cx| {
            dialog.update(cx, |dialog, cx| {
                dialog.fill_form(&saved, cx);
                // What picking a row of the theme dropdown does.
                dialog.select(Catalog::UiTheme, "dracula", cx);
            });
        });
        cx.update(|cx| {
            assert_eq!(app_settings::effective(cx).theme, "dracula");
            assert_eq!(app_settings::current(cx).theme, saved.theme);
        });

        cx.update(|cx| dialog.update(cx, |dialog, cx| dialog.close(cx)));
        cx.update(|cx| {
            assert_eq!(app_settings::effective(cx).theme, saved.theme);
            assert_eq!(app_settings::current(cx), saved);
        });
    }

    /// `Escape` only backs the layer on top of the form out; it does not
    /// dismiss the dialog around it. The delete-confirmation branch is not
    /// exercised here — driving a management row's own "Delete" button needs a
    /// rendered window, and `rugpui_shell::catalog_ui` already proves
    /// `is_confirming`/`cancel_confirm` correct in isolation
    /// (`a_confirmation_can_be_asked_about_and_cancelled_from_outside_the_row`);
    /// what is left to check here is only that [`SettingsDialog::escape`] asks
    /// the editor and the open list first.
    #[gpui::test]
    fn escape_backs_out_of_a_layer_before_it_ever_dismisses(cx: &mut gpui::TestAppContext) {
        let saved = AppSettings::default();
        let dialog = cx.update(|cx| {
            app_settings::replace(saved.clone(), cx);
            let dialog = cx.new(SettingsDialog::new);
            dialog.update(cx, |dialog, cx| dialog.open(cx));
            dialog
        });

        // A dropdown open: `escape` closes it and leaves the dialog up.
        cx.update(|cx| {
            dialog.update(cx, |dialog, cx| {
                dialog.set_list_open(OpenList::Language, true, cx);
                dialog.escape(cx);
                assert!(dialog.open, "the dialog closed instead of just the list");
                assert!(dialog.open_list.is_none(), "the list is still open");
            });
        });

        // The colour editor up: `escape` cancels it in place. `cancel` reports
        // itself through an emitted event rather than a direct field write —
        // see `ThemeEditor::cancel` — so the close it causes only lands once
        // this update's effects flush, which is why the two checks below are
        // split across two `cx.update` calls rather than made inline.
        cx.update(|cx| {
            dialog.update(cx, |dialog, cx| {
                let catalog = dialog.ui_catalog.clone();
                // Built from scratch rather than resolved through the
                // registry, so the test needs no `rugpui::init` of its own.
                let file = catalog.file_from("Test Theme".to_string(), &[], false);
                dialog.open_editor(catalog, "test-theme".to_string(), &file, cx);
            });
        });
        cx.update(|cx| {
            assert!(dialog.read(cx).editor.is_some(), "the editor did not open");
        });

        cx.update(|cx| {
            dialog.update(cx, |dialog, cx| dialog.escape(cx));
        });
        cx.update(|cx| {
            let dialog = dialog.read(cx);
            assert!(dialog.open, "the dialog closed instead of just the editor");
            assert!(dialog.editor.is_none(), "the editor is still open");
        });
    }

    #[test]
    fn every_label_the_form_draws_has_a_translation() {
        // `t!` answers with the key path itself when no such key exists, so a
        // mistyped key reaches the screen as "settings.ui_thme". Catching it
        // here is cheaper than opening the dialog in eight languages.
        let translated = |label: SharedString| {
            assert!(!label.is_empty(), "empty label");
            assert!(!label.contains("settings."), "untranslated label {label:?}");
        };

        for (_, label) in titlebar_options() {
            translated(label);
        }
        for label in [
            ts!("settings.title"),
            ts!("settings.section.appearance"),
            ts!("settings.section.window"),
            ts!("settings.section.language"),
            ts!("settings.section.database"),
            ts!("settings.section.jvm"),
            ts!("settings.ui_theme"),
            ts!("settings.editor_theme"),
            ts!("settings.editor_follows_ui"),
            ts!("settings.ui_font_size"),
            ts!("settings.editor_font"),
            ts!("settings.editor_font_size"),
            ts!("settings.editor_word_wrap"),
            ts!("settings.font_size_hint"),
            ts!("settings.opacity"),
            ts!("settings.opacity_hint"),
            ts!("settings.blur"),
            ts!("settings.titlebar"),
            ts!("settings.language"),
            ts!("settings.language_hint"),
            ts!("settings.fetch_batch_rows"),
            ts!("settings.fetch_batch_rows_hint"),
            ts!("settings.query_timeout"),
            ts!("settings.query_timeout_hint"),
            ts!("settings.confirm_writes_default"),
            ts!("settings.jvm_heap"),
            ts!("settings.jvm_heap_hint"),
            ts!("settings.jvm_args"),
            ts!("settings.jvm_hint"),
            ts!("settings.system_default"),
            system_default(),
            ts!("settings.manage.delete_theme_confirm", name = "X"),
            ts!("settings.manage.delete_editor_theme_confirm", name = "X"),
            ts!("settings.manage.write_failed", error = "e"),
            ts!("settings.manage.delete_failed", error = "e"),
            ts!("settings.manage.import_select"),
            ts!("settings.manage.import_skipped", count = 2),
            // The five buttons of a management row and the two refusals it can
            // report. Drawn by `rugpui-shell`, looked up by these keys, and so
            // still rudbman's to translate.
            ts!("settings.manage.duplicate"),
            ts!("settings.manage.edit"),
            ts!("settings.manage.delete"),
            ts!("settings.manage.import"),
            ts!("settings.manage.export"),
            ts!("settings.manage.import_not_a_theme"),
            ts!("settings.manage.import_not_an_editor_theme"),
            ts!("settings.manage.import_bad_color"),
            ts!("settings.manage.import_unreadable", error = "e"),
            ts!("settings.save_failed", error = "e"),
        ] {
            translated(label);
        }

        // The copy's name has to carry the original's, or duplicating twice
        // would produce two entries that read identically.
        let copy = ts!("settings.manage.copy_name", name = "One Dark");
        assert!(copy.contains("One Dark"), "{copy:?}");
        assert_ne!(copy, "One Dark");
    }

    #[test]
    fn the_two_management_rows_never_share_a_tab_index() {
        // A row takes `TAB_SPAN` consecutive indices whether or not it is
        // currently asking anything, and has to stay clear of the control that
        // follows it.
        let last = |base: isize| base + CatalogActions::TAB_SPAN - 1;
        assert!(last(tab::UI_THEME_ACTIONS) < tab::EDITOR_THEME);
        assert!(last(tab::EDITOR_THEME_ACTIONS) < tab::FOLLOWS_UI);
        // Each row follows the picker it belongs to.
        const { assert!(tab::UI_THEME < tab::UI_THEME_ACTIONS) };
        const { assert!(tab::EDITOR_THEME < tab::EDITOR_THEME_ACTIONS) };
    }

    #[test]
    fn every_control_lands_in_the_section_that_holds_it() {
        // The body scrolls by section index, so a control whose tab index falls
        // on the wrong side of a boundary would scroll the form away from the
        // ring it just moved into.
        for index in [
            tab::UI_THEME,
            tab::FOLLOWS_UI,
            tab::EDITOR_FONT_SIZE,
            tab::EDITOR_WORD_WRAP,
        ] {
            assert_eq!(section_of(index), 0, "{index}");
        }
        for index in [tab::OPACITY, tab::BLUR, tab::TITLEBAR] {
            assert_eq!(section_of(index), 1, "{index}");
        }
        assert_eq!(section_of(tab::LANGUAGE), 2);
        for index in [tab::FETCH_BATCH, tab::QUERY_TIMEOUT, tab::CONFIRM_WRITES] {
            assert_eq!(section_of(index), 3, "{index}");
        }
        for index in [tab::JVM_HEAP, tab::JVM_ARGS, tab::CANCEL, tab::SAVE] {
            assert_eq!(section_of(index), 4, "{index}");
        }
        // And the management rows stay with their pickers.
        assert_eq!(
            section_of(tab::UI_THEME_ACTIONS + CatalogActions::TAB_SPAN - 1),
            0
        );
        assert_eq!(
            section_of(tab::EDITOR_THEME_ACTIONS + CatalogActions::TAB_SPAN - 1),
            0
        );
    }

    #[test]
    fn every_scrolling_surface_has_a_bar_of_its_own() {
        // One drag listener tells the bars apart by id and looks the surface up
        // in this table, so a duplicate on either side would scroll the wrong
        // thing — and a surface listed twice would give two lists one bar.
        let mut ids: Vec<&str> = SCROLLBARS.iter().map(|(id, _)| *id).collect();
        let count = ids.len();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), count, "two bars share an element id");

        for (index, (_, surface)) in SCROLLBARS.iter().enumerate() {
            assert!(
                !SCROLLBARS[..index]
                    .iter()
                    .any(|(_, earlier)| earlier == surface),
                "{surface:?} is listed twice"
            );
        }
    }

    #[test]
    fn the_extra_jvm_arguments_split_on_whitespace() {
        assert_eq!(
            split_arguments("  -Xss4m   -Dfoo=bar\t-Dbaz=1 "),
            vec![
                "-Xss4m".to_string(),
                "-Dfoo=bar".to_string(),
                "-Dbaz=1".to_string()
            ]
        );
        assert!(split_arguments("").is_empty());
        assert!(split_arguments("   ").is_empty());
    }

    #[test]
    fn a_font_size_is_shown_without_a_pointless_decimal() {
        assert_eq!(format_number(14.0), "14");
        assert_eq!(format_number(13.5), "13.5");
    }
}
