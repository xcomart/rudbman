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
//! from a card is not the same as living in it. The dialog does that without
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

use std::sync::Once;

use gpui::{
    App, Context, DragMoveEvent, Entity, EventEmitter, FocusHandle, Focusable, Hsla, IntoElement,
    KeyBinding, KeyDownEvent, MouseButton, MouseUpEvent, Render, ScrollHandle, SharedString,
    Subscription, Window, actions, div, prelude::*, px,
};
use rudbman_core::{AppSettings, TitlebarStyle};
use rudbman_ui::{
    Button, ButtonVariant, Checkbox, DraggedThumb, EditorThemePicker, EditorThemeRegistry,
    EditorThemeSwatch, Scrollbar, ScrollbarAxis, ScrollbarState, Segmented, Select, TextInput,
    Theme, ThemeRegistry, form_row, hide_later, modal, scroll_to, scrolled, theme, theme_store,
};

use crate::app_settings;
use crate::i18n::{self, ts};
use crate::theme_editor::{Catalog, CatalogFile, ThemeEditor, ThemeEditorEvent};
use crate::theme_picker::{ThemePicker, ThemeSwatch};

/// The dialog's three scrolling surfaces, and the element id of each one's
/// overlay scroll indicator.
///
/// One drag listener answers all three, so it has to be able to say which bar a
/// drag belongs to; these ids are how, and pairing each with the handle and the
/// state it goes with keeps the three from being wired up crosswise.
const SCROLLBARS: [(&str, Surface); 3] = [
    ("settings-body-scrollbar", Surface::Body),
    ("settings-font-scrollbar", Surface::Font),
    ("settings-language-scrollbar", Surface::Language),
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
}

/// Width of the dialog panel.
const DIALOG_WIDTH: f32 = 760.;

/// Height at which the form body starts scrolling.
const BODY_MAX_HEIGHT: f32 = 520.;

/// Cards per row in the chrome theme picker.
const THEME_COLUMNS: usize = 3;

/// Cards per row in the editor theme picker.
///
/// Two rather than three: each card carries a whole statement, and a statement
/// needs the width. The widget's own default, spelled out here so that the two
/// pickers in one section read as a deliberate pair.
const EDITOR_THEME_COLUMNS: usize = 2;

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

/// What a management row under a picker can be asked to do.
///
/// Shared by both catalogues; which of them a given selection permits is worked
/// out in [`SettingsDialog::render_actions`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Action {
    /// Copy the selected entry into a new file and open it for editing.
    ///
    /// Also the "save under another name" of the editor: a built-in palette is
    /// duplicated to be edited, and a custom one is duplicated to be varied.
    Duplicate,
    /// Open the selected custom entry for editing.
    Edit,
    /// Remove the selected custom entry's file, once confirmed.
    Delete,
}

/// The three actions in the order they are drawn.
const ACTIONS: [Action; 3] = [Action::Duplicate, Action::Edit, Action::Delete];

/// Element id fragment and tab offset of the confirmation's "cancel".
const SLOT_CONFIRM_CANCEL: usize = 5;

/// Element id fragment and tab offset of the confirmation's "delete".
const SLOT_CONFIRM_DELETE: usize = 6;

impl Action {
    /// Position of the action within its row, used for both the element id and
    /// the tab index so the two can never drift apart.
    fn slot(self) -> usize {
        match self {
            Self::Duplicate => 0,
            Self::Edit => 1,
            Self::Delete => 2,
        }
    }

    /// The button's label in the active language.
    fn label(self) -> SharedString {
        match self {
            Self::Duplicate => ts!("settings.manage.duplicate"),
            Self::Edit => ts!("settings.manage.edit"),
            Self::Delete => ts!("settings.manage.delete"),
        }
    }

    /// Whether the action applies to the entry currently selected.
    ///
    /// `known` is whether the selected id resolves at all — a hand-edited
    /// `settings.json` can name one that does not — and `custom` whether what it
    /// resolves to came from a file, which is the only kind rudbman may rewrite
    /// or remove.
    fn enabled(self, known: bool, custom: bool) -> bool {
        match self {
            Self::Duplicate => known,
            Self::Edit | Self::Delete => custom,
        }
    }
}

/// State of one picker's management row.
///
/// One per catalogue, since the two rows ask and report independently: a delete
/// waiting to be confirmed under the chrome themes must not disappear because
/// something went wrong under the editor themes.
#[derive(Debug, Default)]
struct CatalogActions {
    /// Whether the delete confirmation is showing.
    confirming: bool,
    /// What went wrong the last time this row was used, if anything.
    status: Option<SharedString>,
}

/// The chrome themes as picker entries, each previewing its own window.
fn ui_theme_swatches(cx: &App) -> Vec<ThemeSwatch> {
    ThemeRegistry::all(cx)
        .into_iter()
        .map(|entry| {
            let palette = ThemeRegistry::resolve(&entry.id, cx);
            ThemeSwatch::new(entry.id, entry.name, palette)
        })
        .collect()
}

/// The editor themes as picker entries, each previewing its own statement.
fn editor_theme_swatches(cx: &App) -> Vec<EditorThemeSwatch> {
    EditorThemeRegistry::all(cx)
        .into_iter()
        .map(|entry| {
            let palette = EditorThemeRegistry::resolve(&entry.id, cx);
            EditorThemeSwatch::new(entry.id, entry.name).preview(palette)
        })
        .collect()
}

/// Which of the dialog's dropdown lists is currently showing.
///
/// A single field rather than one flag per dropdown, so that the two cannot be
/// open at once — their lists are drawn deferred and would overlap.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OpenList {
    /// The interface language picker.
    Language,
    /// The editor font picker.
    Font,
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
    /// Whether the window should be blurred behind.
    background_blur: bool,
    /// Title bar style currently selected in the form.
    titlebar: TitlebarStyle,
    /// What a newly created connection profile's "confirm writes" starts at.
    confirm_writes_default: bool,
    /// Editor font family; `None` means the per-OS default.
    font_family: Option<SharedString>,
    /// State of the management row under the chrome theme picker.
    ui_theme_actions: CatalogActions,
    /// State of the management row under the editor theme picker.
    editor_theme_actions: CatalogActions,
    /// The colour editor, while one is open. The dialog renders it *instead of*
    /// the form rather than over it; see [`crate::theme_editor`].
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
        Self {
            open: false,
            ui_theme: defaults.theme.into(),
            editor_theme: defaults.editor_theme.into(),
            editor_theme_follows_ui: defaults.editor_theme_follows_ui,
            language: defaults.language,
            background_blur: defaults.window.background_blur,
            titlebar: defaults.window.titlebar,
            confirm_writes_default: defaults.confirm_writes_default,
            font_family: defaults.editor_font_family.map(SharedString::from),
            ui_theme_actions: CatalogActions::default(),
            editor_theme_actions: CatalogActions::default(),
            editor: None,
            editor_events: None,
            status: None,
            focus_handle: cx.focus_handle(),
            pending_focus: false,
            body_scroll: ScrollHandle::new(),
            body_scrollbar: ScrollbarState::new(),
            font_scrollbar: ScrollbarState::new(),
            language_scrollbar: ScrollbarState::new(),
            visible_section: 0,
            open_list: None,
            fonts: Vec::new(),
            font_scroll: ScrollHandle::new(),
            language_scroll: ScrollHandle::new(),
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
        }
    }

    /// The same pair, for the renders that only read them.
    fn surface_ref(&self, surface: Surface) -> (&ScrollHandle, &ScrollbarState) {
        match surface {
            Surface::Body => (&self.body_scroll, &self.body_scrollbar),
            Surface::Font => (&self.font_scroll, &self.font_scrollbar),
            Surface::Language => (&self.language_scroll, &self.language_scrollbar),
        }
    }

    /// The overlay scroll indicator of one surface, as it stands.
    fn scrollbar(&self, id: &'static str, surface: Surface) -> Scrollbar {
        let (handle, state) = self.surface_ref(surface);
        Scrollbar::for_handle(id, ScrollbarAxis::Vertical, handle).fade(state.fade())
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

    /// Show the dialog, re-reading the current settings into the form.
    pub fn open(&mut self, cx: &mut Context<Self>) {
        let settings = app_settings::current(cx);
        self.fonts = installed_fonts(cx);
        self.fill_form(&settings, cx);
        self.status = None;
        self.ui_theme_actions = CatalogActions::default();
        self.editor_theme_actions = CatalogActions::default();
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

    /// The management state of one catalogue.
    fn actions(&self, catalog: Catalog) -> &CatalogActions {
        match catalog {
            Catalog::UiTheme => &self.ui_theme_actions,
            Catalog::EditorTheme => &self.editor_theme_actions,
        }
    }

    /// The same, for the callers that change it.
    fn actions_mut(&mut self, catalog: Catalog) -> &mut CatalogActions {
        match catalog {
            Catalog::UiTheme => &mut self.ui_theme_actions,
            Catalog::EditorTheme => &mut self.editor_theme_actions,
        }
    }

    /// The id currently highlighted in one catalogue's picker.
    fn selection(&self, catalog: Catalog) -> SharedString {
        match catalog {
            Catalog::UiTheme => self.ui_theme.clone(),
            Catalog::EditorTheme => self.editor_theme.clone(),
        }
    }

    /// Highlights `id` in one catalogue's picker and previews it.
    ///
    /// Nothing is persisted; the preview is dropped again if the dialog is
    /// cancelled, exactly as when the user clicks a card.
    fn select(&mut self, catalog: Catalog, id: impl Into<SharedString>, cx: &mut Context<Self>) {
        match catalog {
            Catalog::UiTheme => self.ui_theme = id.into(),
            Catalog::EditorTheme => self.editor_theme = id.into(),
        }
        self.refresh_preview(cx);
        cx.notify();
    }

    /// Runs one of the management actions against `catalog`.
    ///
    /// Every one of them starts by clearing whatever the last one had to
    /// report, so a message never outlives the situation it described.
    fn run(&mut self, catalog: Catalog, action: Action, cx: &mut Context<Self>) {
        self.actions_mut(catalog).status = None;
        match action {
            Action::Duplicate => self.duplicate(catalog, cx),
            Action::Edit => self.edit(catalog, cx),
            Action::Delete => {
                // Deleting is the one action here that cannot be undone by
                // doing it again, so it asks first.
                self.actions_mut(catalog).confirming = true;
                cx.notify();
            }
        }
    }

    /// Reports why an action could not be carried out.
    fn report(&mut self, catalog: Catalog, message: SharedString, cx: &mut Context<Self>) {
        self.actions_mut(catalog).status = Some(message);
        cx.notify();
    }

    /// Copies the selected entry into a file of its own and opens it.
    ///
    /// Works on a built-in entry as readily as on a custom one — that is the
    /// point of it, since the built-in palettes are where a user's own theme
    /// usually starts, and rudbman refuses to write over a built-in id.
    fn duplicate(&mut self, catalog: Catalog, cx: &mut Context<Self>) {
        let selection = self.selection(catalog);
        let Some(mut file) = catalog.file_for(&selection, cx) else {
            return;
        };

        let name = ts!("settings.manage.copy_name", name = file.name().to_owned()).to_string();
        let id = theme_store::unique_id(
            &[name.as_str()],
            catalog.generated_id_prefix(),
            &catalog.taken_ids(cx),
        );
        file.set_name(name);

        if let Err(err) = file.save(&id) {
            log::error!("could not write the duplicated {id}: {err:#}");
            let message = ts!("settings.manage.write_failed", error = format!("{err:#}"));
            self.report(catalog, message, cx);
            return;
        }

        theme_store::reload(cx);
        self.select(catalog, id.clone(), cx);
        self.open_editor(id, file, cx);
    }

    /// Opens the selected custom entry in the editor.
    fn edit(&mut self, catalog: Catalog, cx: &mut Context<Self>) {
        let selection = self.selection(catalog);
        let Some((id, file)) = catalog
            .entry(&selection, cx)
            .filter(|entry| !entry.builtin)
            .and_then(|entry| catalog.file_for(&entry.id, cx).map(|file| (entry.id, file)))
        else {
            return;
        };
        self.open_editor(id, file, cx);
    }

    /// Puts the editor in front of the form, over `file`.
    fn open_editor(&mut self, id: String, file: CatalogFile, cx: &mut Context<Self>) {
        let editor = cx.new(|cx| ThemeEditor::new(id, &file, cx));
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

    /// Drops the delete confirmation without acting on it.
    fn cancel_confirm(&mut self, catalog: Catalog, cx: &mut Context<Self>) {
        if self.actions_mut(catalog).confirming {
            self.actions_mut(catalog).confirming = false;
            cx.notify();
        }
    }

    /// Removes the selected custom entry's file.
    ///
    /// The selection then moves to the default id, because the one it held no
    /// longer resolves; the *setting* still names it until the dialog is saved,
    /// which is why the preview is refreshed — the running window falls back to
    /// the default palette in the same breath as the picker does.
    fn delete(&mut self, catalog: Catalog, cx: &mut Context<Self>) {
        self.actions_mut(catalog).confirming = false;
        let selection = self.selection(catalog);
        let Some(entry) = catalog.entry(&selection, cx).filter(|entry| !entry.builtin) else {
            cx.notify();
            return;
        };

        if let Err(err) = catalog.delete(&entry.id) {
            log::error!("could not remove {}: {err:#}", entry.id);
            let message = ts!("settings.manage.delete_failed", error = format!("{err:#}"));
            self.report(catalog, message, cx);
            return;
        }

        theme_store::reload(cx);
        self.select(catalog, catalog.default_id(), cx);
        cx.notify();
    }

    /// Copy `settings` into every control.
    fn fill_form(&mut self, settings: &AppSettings, cx: &mut Context<Self>) {
        self.ui_theme = settings.theme.clone().into();
        self.editor_theme = settings.editor_theme.clone().into();
        self.editor_theme_follows_ui = settings.editor_theme_follows_ui;
        self.language = settings.language.clone();
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
        window.focus_next();
        self.reveal_focused(window, cx);
    }

    /// `Shift+Tab`: move focus to the previous control, wrapping to the last.
    fn focus_prev(&mut self, _: &FocusPrev, window: &mut Window, cx: &mut Context<Self>) {
        self.close_lists(cx);
        window.focus_prev();
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
    /// itself, so that backing out of a list, a question or the colour editor
    /// does not also throw away the whole form. The editor is checked before the
    /// dropdowns because it replaces the form outright: while it is up there is
    /// no list to close.
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
            if self.actions(catalog).confirming {
                self.cancel_confirm(catalog, cx);
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
        window.focus(&handle);
    }

    /// The row of management buttons drawn under one picker.
    ///
    /// `base` is the first of the [`Action::slot`] consecutive tab indices the
    /// row takes; the confirmation's two buttons continue from there, so a row
    /// occupies seven indices whether or not it is currently asking anything.
    fn render_actions(
        &self,
        catalog: Catalog,
        base: isize,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let chrome = theme(cx);
        let this = cx.entity();
        let prefix = catalog.element_prefix();

        let entry = catalog.entry(&self.selection(catalog), cx);
        let known = entry.is_some();
        let custom = entry.as_ref().is_some_and(|entry| !entry.builtin);
        let confirming = self.actions(catalog).confirming;

        let buttons = ACTIONS.map(|action| {
            Button::new((prefix, action.slot()), action.label())
                .variant(ButtonVariant::Secondary)
                // Everything is held while the confirmation is up, so that the
                // question can only be answered, not walked away from.
                .disabled(confirming || !action.enabled(known, custom))
                .tab_index(base + action.slot() as isize)
                .on_click({
                    let this = this.clone();
                    move |_, _window, cx| {
                        this.update(cx, |dialog, cx| dialog.run(catalog, action, cx));
                    }
                })
                .into_any_element()
        });

        let confirm = confirming.then(|| {
            let name = entry.map(|entry| entry.name).unwrap_or_default();
            let question = match catalog {
                Catalog::UiTheme => ts!("settings.manage.delete_theme_confirm", name = name),
                Catalog::EditorTheme => {
                    ts!("settings.manage.delete_editor_theme_confirm", name = name)
                }
            };

            div()
                .flex()
                .flex_row()
                // Wraps rather than overflowing: a locale that spells the
                // question out at length would otherwise push a button past the
                // edge of the section.
                .flex_wrap()
                .items_center()
                .justify_end()
                .gap(px(8.))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .text_size(px(12.))
                        .text_color(chrome.text)
                        .child(question),
                )
                .child(
                    Button::new((prefix, SLOT_CONFIRM_CANCEL), ts!("common.cancel"))
                        .variant(ButtonVariant::Secondary)
                        .tab_index(base + SLOT_CONFIRM_CANCEL as isize)
                        .on_click({
                            let this = this.clone();
                            move |_, _window, cx| {
                                this.update(cx, |dialog, cx| dialog.cancel_confirm(catalog, cx));
                            }
                        }),
                )
                .child(
                    Button::new((prefix, SLOT_CONFIRM_DELETE), ts!("settings.manage.delete"))
                        .variant(ButtonVariant::Danger)
                        .tab_index(base + SLOT_CONFIRM_DELETE as isize)
                        .on_click({
                            let this = this.clone();
                            move |_, _window, cx| {
                                this.update(cx, |dialog, cx| dialog.delete(catalog, cx));
                            }
                        }),
                )
        });

        let status = self.actions(catalog).status.clone().map(|message| {
            div()
                .text_size(px(11.))
                .text_color(StatusLevel::Error.color(&chrome))
                .child(message)
        });

        div()
            .flex()
            .flex_col()
            .gap(px(6.))
            .child(
                div()
                    .flex()
                    .flex_row()
                    .flex_wrap()
                    .gap(px(6.))
                    .children(buttons),
            )
            .children(confirm)
            .children(status)
    }

    /// The editor theme picker, or — while the choice follows the chrome theme —
    /// a single card showing what the app picked instead.
    ///
    /// Disabled rather than merely ignored: the setting says the app decides, so
    /// offering a grid whose every click would be silently discarded would be a
    /// lie. Showing the resolved theme keeps the section informative, since that
    /// answer moves as the chrome theme does.
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

            return EditorThemePicker::new("settings-editor-theme-followed")
                .options([EditorThemeSwatch::new(resolved.clone(), name).preview(palette)])
                .selected(Some(resolved))
                .columns(1)
                .when_some(self.font_family.clone(), |picker, family| {
                    picker.font_family(family)
                })
                .into_any_element();
        }

        EditorThemePicker::new("settings-editor-theme")
            .options(editor_theme_swatches(cx))
            .selected(Some(self.editor_theme.clone()))
            .columns(EDITOR_THEME_COLUMNS)
            .tab_index(tab::EDITOR_THEME)
            .when_some(self.font_family.clone(), |picker, family| {
                picker.font_family(family)
            })
            .on_select(move |id, _window, cx| {
                let id = SharedString::from(id.to_owned());
                this.update(cx, |dialog, cx| {
                    dialog.select(Catalog::EditorTheme, id, cx);
                });
            })
            .into_any_element()
    }

    /// The "Appearance" section: both palettes and both fonts.
    fn render_appearance(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let this = cx.entity();
        let font_bar = self.scrollbar(SCROLLBARS[1].0, Surface::Font);
        // Built before the section is assembled, because `section` borrows the
        // context to read the theme and these borrow it mutably to listen.
        let theme_actions = self.render_actions(Catalog::UiTheme, tab::UI_THEME_ACTIONS, cx);
        let editor_theme_actions =
            self.render_actions(Catalog::EditorTheme, tab::EDITOR_THEME_ACTIONS, cx);
        let editor_theme = self.render_editor_theme(cx);

        let theme_picker = ThemePicker::new("settings-ui-theme")
            .options(ui_theme_swatches(cx))
            .selected(Some(self.ui_theme.clone()))
            .columns(THEME_COLUMNS)
            .tab_index(tab::UI_THEME)
            .on_select({
                let this = this.clone();
                move |id, _window, cx| {
                    let id = SharedString::from(id.to_owned());
                    this.update(cx, |dialog, cx| dialog.select(Catalog::UiTheme, id, cx));
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

        let font = Select::new("settings-editor-font")
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
                )),
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
        let language_bar = self.scrollbar(SCROLLBARS[2].0, Surface::Language);

        let language = Select::new("settings-language")
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
        let body_bar = self.scrollbar(SCROLLBARS[0].0, Surface::Body);

        // While a colour is being edited the form steps aside entirely rather
        // than being covered up, so that the window's tab ring holds only the
        // controls that are actually on screen; see [`crate::theme_editor`]. The
        // form is not even built in that case — it would be built afresh on
        // every keystroke in the editor and thrown away again.
        let (title, body) = match self.editor.clone() {
            Some(editor) => (editor.read(cx).title(), editor.into_any_element()),
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
        index if index <= tab::EDITOR_FONT_SIZE => 0,
        index if index <= tab::TITLEBAR => 1,
        index if index <= tab::LANGUAGE => 2,
        index if index <= tab::CONFIRM_WRITES => 3,
        _ => 4,
    }
}

/// Wraps `body` in a titled card.
fn section<E: IntoElement>(title: SharedString, cx: &App, body: E) -> impl IntoElement + use<E> {
    let chrome = theme(cx);
    div()
        .flex()
        .flex_col()
        .gap(px(10.))
        .p(px(12.))
        .rounded_lg()
        .border_1()
        .border_color(chrome.border)
        .bg(chrome.surface)
        .child(
            div()
                .text_size(px(11.))
                .text_color(chrome.text_muted)
                .child(title),
        )
        .child(body)
}

/// A muted paragraph explaining something a form row cannot say on its own.
fn hint(text: SharedString, cx: &App) -> impl IntoElement + use<> {
    let chrome = theme(cx);
    div()
        .text_size(px(11.))
        .text_color(chrome.text_muted)
        .child(text)
}

/// Lays a short unit hint out to the right of a narrow control.
fn suffixed<E: IntoElement>(control: E, hint: SharedString, cx: &App) -> impl IntoElement + use<E> {
    let chrome = theme(cx);
    div()
        .flex()
        .flex_row()
        .items_center()
        .gap(px(8.))
        .w_full()
        .child(div().flex_none().w(px(96.)).child(control))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .text_size(px(11.))
                .text_color(chrome.text_muted)
                .child(hint),
        )
}

/// The font families the platform offers, in the order gpui reports them —
/// sorted and deduplicated already.
///
/// Names starting with a dot are dropped: those are the platform's private
/// aliases, such as `.SystemUIFont` on macOS, which are not meant to be chosen
/// by name.
fn installed_fonts(cx: &App) -> Vec<SharedString> {
    cx.text_system()
        .all_font_names()
        .into_iter()
        .filter(|name| !name.starts_with('.'))
        .map(SharedString::from)
        .collect()
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

/// Trimmed content of `input`.
fn text(input: &Entity<TextInput>, cx: &App) -> String {
    input.read(cx).content().trim().to_owned()
}

/// Parses `input` into `T`, or `None` when it is blank or malformed.
fn parse_number<T: std::str::FromStr>(input: &Entity<TextInput>, cx: &App) -> Option<T> {
    text(input, cx).parse::<T>().ok()
}

/// Replaces the contents of `input`.
fn set_text(input: &Entity<TextInput>, value: impl Into<SharedString>, cx: &mut App) {
    input.update(cx, |input, cx| input.set_content(value, cx));
}

/// Renders `value` without a trailing `.0`, so 14.0 shows as "14".
fn format_number(value: f32) -> String {
    if value.fract() == 0.0 {
        format!("{value:.0}")
    } else {
        format!("{value}")
    }
}

/// Installs an observer that keeps `input` numeric.
///
/// The text field has no input filter, so the content is rewritten after every
/// edit. Rewriting only when the text actually changes stops the observer from
/// re-triggering itself.
fn restrict_to_number(
    cx: &mut Context<SettingsDialog>,
    input: &Entity<TextInput>,
    decimals: bool,
    max_len: usize,
) {
    cx.observe(input, move |_this, input, cx| {
        let content = input.read(cx).content().to_owned();
        let mut seen_dot = false;
        let filtered: String = content
            .chars()
            .filter(|c| {
                if c.is_ascii_digit() {
                    true
                } else if decimals && *c == '.' && !seen_dot {
                    seen_dot = true;
                    true
                } else {
                    false
                }
            })
            .take(max_len)
            .collect();
        if filtered != content {
            input.update(cx, |input, cx| input.set_content(filtered, cx));
        }
    })
    .detach();
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
                // What clicking a card does.
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

    #[test]
    fn every_label_the_form_draws_has_a_translation() {
        // `t!` answers with the key path itself when no such key exists, so a
        // mistyped key reaches the screen as "settings.ui_thme". Catching it
        // here is cheaper than opening the dialog in eight languages.
        let translated = |label: SharedString| {
            assert!(!label.is_empty(), "empty label");
            assert!(!label.contains("settings."), "untranslated label {label:?}");
        };

        for action in ACTIONS {
            translated(action.label());
        }
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
        // Each row takes one index per action plus the two the confirmation
        // adds, and has to stay clear of the control that follows it.
        let last = |base: isize| base + SLOT_CONFIRM_DELETE as isize;
        assert!(last(tab::UI_THEME_ACTIONS) < tab::EDITOR_THEME);
        assert!(last(tab::EDITOR_THEME_ACTIONS) < tab::FOLLOWS_UI);
        // Each row follows the picker it belongs to.
        const { assert!(tab::UI_THEME < tab::UI_THEME_ACTIONS) };
        const { assert!(tab::EDITOR_THEME < tab::EDITOR_THEME_ACTIONS) };

        let mut slots: Vec<usize> = ACTIONS.iter().map(|action| action.slot()).collect();
        slots.extend([SLOT_CONFIRM_CANCEL, SLOT_CONFIRM_DELETE]);
        let unique = slots.len();
        slots.sort_unstable();
        slots.dedup();
        assert_eq!(slots.len(), unique, "two actions share a slot");
    }

    #[test]
    fn only_a_custom_entry_may_be_edited_or_removed() {
        for action in ACTIONS {
            // Nothing at all is selected — a settings file naming a theme whose
            // file has since gone — so nothing may be done to it.
            assert!(!action.enabled(false, false), "{action:?}");
        }
        // A built-in entry can be copied, but not rewritten.
        assert!(Action::Duplicate.enabled(true, false));
        assert!(!Action::Edit.enabled(true, false));
        assert!(!Action::Delete.enabled(true, false));
        assert!(Action::Edit.enabled(true, true));
        assert!(Action::Delete.enabled(true, true));
    }

    #[test]
    fn every_control_lands_in_the_section_that_holds_it() {
        // The body scrolls by section index, so a control whose tab index falls
        // on the wrong side of a boundary would scroll the form away from the
        // ring it just moved into.
        for index in [tab::UI_THEME, tab::FOLLOWS_UI, tab::EDITOR_FONT_SIZE] {
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
        assert_eq!(section_of(tab::UI_THEME_ACTIONS + 6), 0);
        assert_eq!(section_of(tab::EDITOR_THEME_ACTIONS + 6), 0);
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
