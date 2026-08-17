//! Editing one chrome theme or one editor theme, colour by colour.
//!
//! # Where the editor is drawn
//!
//! Not as a modal of its own. [`crate::settings_dialog::SettingsDialog`] is
//! already a modal, and stacking a second one on top of it would leave the form
//! underneath rendered — which is to say still in the window's tab ring, so
//! `Tab` would walk out of the editor and into controls nobody can see. The
//! settings dialog therefore swaps its *body* for this view while an editor is
//! open: one modal, one set of tab stops, and `Escape` has a single obvious
//! meaning at every moment. The view returned by [`ThemeEditor`]'s `Render` is
//! consequently a plain panel, not a dialog; the frame around it belongs to the
//! settings dialog.
//!
//! # What it edits
//!
//! One component for both catalogues, because they differ only in which slots
//! they carry: a chrome theme is a name, a dark/light flag, eleven required
//! colours and five optional grid ones, and an editor theme is a name, a
//! dark/light flag and nineteen required token colours. Everything else — the
//! hex fields, the live preview, the refusal of a malformed colour, saving under
//! an id that never changes — is the same work, and [`Catalog`] is the one place
//! the two part ways.
//!
//! # Automatic slots
//!
//! The five grid slots of a chrome theme may be left out of the file, in which
//! case [`ThemeFile::to_theme`] derives them from the rest of the palette; that
//! is what lets a theme written against the eleven-slot format keep loading. The
//! editor has to be able to say which of the two a slot is in, so an *empty*
//! field means "derive it": the swatch then shows the derived colour, the
//! placeholder spells its hex out, and a button beside the field puts a slot
//! that has been given a colour back to automatic. A required slot has neither.
//!
//! # Files in and out
//!
//! [`Catalog`] also owns the two ends of the exchange the settings dialog's
//! management row drives: [`Catalog::read`] turns a file the user picked
//! anywhere on the disk into a [`CatalogFile`], and [`CatalogFile::write`] puts
//! one back out anywhere. Reading is where this module is *stricter* than
//! `rudbman-ui`'s loader: the loader is forgiving because a broken file in the
//! configuration directory must not take the others down with it, whereas an
//! import is a single deliberate act with a person waiting on the answer, and
//! silently installing a theme with half its slots quietly substituted would be
//! the worse outcome. Every refusal is an [`ImportError`], which carries a
//! sentence the user can act on — above all "that is a file of the other kind",
//! since the two formats look alike enough that picking the wrong row is the
//! mistake most likely to be made.

use std::path::{Path, PathBuf};

use anyhow::Result;
use gpui::{
    App, Context, Div, DragMoveEvent, Entity, EventEmitter, FocusHandle, Focusable, Hsla,
    IntoElement, MouseButton, MouseUpEvent, Render, ScrollHandle, SharedString, Window, div,
    prelude::*, px,
};
use rudbman_core::AppSettings;
use rudbman_ui::{
    Button, ButtonVariant, Checkbox, DraggedThumb, EditorThemeColors, EditorThemeFile,
    EditorThemePicker, EditorThemeRegistry, EditorThemeSwatch, Scrollbar, ScrollbarAxis,
    ScrollbarState, TextInput, ThemeColors, ThemeFile, ThemeRegistry, form_row, hide_later,
    hide_now, parse_hex, scroll_to, scrolled, theme, theme_store, to_hex,
};

use crate::i18n::ts;

/// Element id of the editor's overlay scroll indicator.
const SCROLLBAR_ID: &str = "theme-editor-scrollbar";

/// Height at which the editor's field list starts scrolling.
///
/// The same cap the settings form uses, so the modal keeps its size as the
/// dialog swaps one body for the other.
const BODY_MAX_HEIGHT: f32 = 520.;

/// Colour fields per row.
///
/// Two, for both catalogues: at the dialog's width a row of two leaves each
/// label enough room to be read in every language.
const FIELD_COLUMNS: usize = 2;

/// Width of a colour field's label, in pixels.
const LABEL_WIDTH: f32 = 118.;

/// Side of the swatch drawn beside a colour field.
const SWATCH_SIZE: f32 = 26.;

/// Index of the first of the chrome palette's five optional grid slots.
///
/// Load-bearing: it is where [`ui_slots`] stops being required and where
/// [`derived_ui_color`] starts counting the grid slots from.
const UI_GRID_FIRST: usize = 11;

/// Tab order inside the editor, spaced so slots can be inserted later.
///
/// A ring of its own rather than a continuation of the settings form's: while
/// the editor is open the form is not rendered at all, so there is nothing for
/// these indices to collide with.
mod tab {
    /// The name field.
    pub const NAME: isize = 10;
    /// The dark/light checkbox.
    pub const DARK: isize = 20;
    /// The first colour field; the rest follow two apart, because an optional
    /// slot puts its "automatic" button in the odd index behind its field.
    pub const FIRST_COLOR: isize = 30;
    /// Cancel. Far enough past the colours that no catalogue can reach it.
    pub const CANCEL: isize = 900;
    /// Save.
    pub const SAVE: isize = 910;
}

/// Which of the two colour catalogues is meant.
///
/// The catalogues are parallel in every way that matters to the settings dialog
/// — each is a list of built-in entries plus a directory of files, each is
/// picked from a grid of cards — so every management action is written once
/// against this enum instead of twice against the two registries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Catalog {
    /// The chrome themes of [`ThemeRegistry`].
    UiTheme,
    /// The syntax palettes of [`EditorThemeRegistry`].
    EditorTheme,
}

/// One entry of a catalogue, as the management row needs to see it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CatalogEntry {
    /// Stable id, which is also the stem of the file a custom entry lives in.
    pub id: String,
    /// Human-readable name.
    pub name: String,
    /// Whether the entry ships with rudbman rather than coming from a file.
    pub builtin: bool,
}

/// One theme file, whichever of the two formats the catalogue holds.
#[derive(Debug, Clone)]
pub enum CatalogFile {
    /// A chrome theme.
    UiTheme(Box<ThemeFile>),
    /// An editor theme.
    EditorTheme(Box<EditorThemeFile>),
}

impl CatalogFile {
    /// The name the file carries.
    pub fn name(&self) -> &str {
        match self {
            Self::UiTheme(file) => &file.name,
            Self::EditorTheme(file) => &file.name,
        }
    }

    /// Replaces the name the file carries.
    pub fn set_name(&mut self, name: impl Into<String>) {
        match self {
            Self::UiTheme(file) => file.name = name.into(),
            Self::EditorTheme(file) => file.name = name.into(),
        }
    }

    /// Which catalogue the file belongs to.
    pub fn catalog(&self) -> Catalog {
        match self {
            Self::UiTheme(_) => Catalog::UiTheme,
            Self::EditorTheme(_) => Catalog::EditorTheme,
        }
    }

    /// Writes the file into the configuration directory under `id`.
    ///
    /// # Errors
    ///
    /// Fails for the reasons [`theme_store::save_ui_theme`] does: an unusable
    /// id, one belonging to a built-in entry, or a write that does not go
    /// through.
    pub fn save(&self, id: &str) -> Result<PathBuf> {
        match self {
            Self::UiTheme(file) => theme_store::save_ui_theme(id, file),
            Self::EditorTheme(file) => theme_store::save_editor_theme(id, file),
        }
    }

    /// Writes the file to `path`, wherever on the disk that is.
    ///
    /// The counterpart of [`CatalogFile::save`], which decides the path itself
    /// from an id: an export goes where the user pointed the save dialog, which
    /// is usually somewhere rudbman will never read from again.
    ///
    /// # Errors
    ///
    /// Fails when the file cannot be serialized or cannot be written — a
    /// directory that is gone, or one that is not writable.
    pub fn write(&self, path: &Path) -> Result<()> {
        match self {
            Self::UiTheme(file) => theme_store::write_file(path, file.as_ref()),
            Self::EditorTheme(file) => theme_store::write_file(path, file.as_ref()),
        }
    }

    /// Refuses a file that parses but holds something which is not a colour.
    ///
    /// Checked against the very table the editor's fields are built from, so a
    /// value the import accepts is one the editor would also let be saved
    /// again. Only the first offending slot is reported: a hand-written file
    /// with one typo is the common case, and listing nineteen slots would bury
    /// it.
    fn validate(&self) -> Result<(), ImportError> {
        let (slots, values) = match self {
            Self::UiTheme(file) => (ui_slots(), ui_values(&file.colors)),
            Self::EditorTheme(file) => (editor_slots(), editor_values(&file.colors)),
        };
        for (slot, value) in slots.iter().zip(&values) {
            if !valid_hex(value, slot.alpha, slot.optional) {
                return Err(ImportError::BadColor(slot.label.clone()));
            }
        }
        Ok(())
    }
}

/// Why a file the user picked could not be imported.
///
/// A type rather than a bare [`anyhow::Error`] because the three cases read
/// differently to the person who picked the file: one is "this is not a theme
/// file", one is "this is a theme file, but of the other kind", and one is
/// "this is a theme file of the right kind with a bad colour in it". Only the
/// first has anything to gain from the underlying error text.
#[derive(Debug)]
pub enum ImportError {
    /// The file could not be read, or does not parse as either format.
    Unreadable(anyhow::Error),
    /// It parses — as the catalogue named here, which is not the one it was
    /// offered to.
    WrongCatalog(Catalog),
    /// A slot holds something that is not an `#RRGGBB` colour; the slot's own
    /// label, already translated.
    BadColor(SharedString),
}

impl ImportError {
    /// The sentence shown under the management row, naming `file`.
    ///
    /// `file` is the file's name rather than its whole path: the path is
    /// already in the log, and a management row is not wide enough to print one
    /// without pushing everything else off the edge.
    pub fn message(&self, file: &str) -> SharedString {
        match self {
            Self::Unreadable(error) => ts!(
                "settings.manage.import_unreadable",
                file = file,
                error = format!("{error:#}")
            ),
            // Named by what the file *is*, not by where it was dropped: the
            // user is being told which row to try instead.
            Self::WrongCatalog(Catalog::EditorTheme) => {
                ts!("settings.manage.import_not_a_theme", file = file)
            }
            Self::WrongCatalog(Catalog::UiTheme) => {
                ts!("settings.manage.import_not_an_editor_theme", file = file)
            }
            Self::BadColor(slot) => ts!(
                "settings.manage.import_bad_color",
                file = file,
                slot = slot.clone()
            ),
        }
    }
}

impl Catalog {
    /// Every entry, the built-in ones first and then the user's own.
    pub fn entries(self, cx: &App) -> Vec<CatalogEntry> {
        match self {
            Self::UiTheme => ThemeRegistry::all(cx)
                .into_iter()
                .map(|entry| CatalogEntry {
                    id: entry.id,
                    name: entry.name,
                    builtin: entry.builtin,
                })
                .collect(),
            Self::EditorTheme => EditorThemeRegistry::all(cx)
                .into_iter()
                .map(|entry| CatalogEntry {
                    id: entry.id,
                    name: entry.name,
                    builtin: entry.builtin,
                })
                .collect(),
        }
    }

    /// The entry `id` names, or `None` when nothing answers to it.
    pub fn entry(self, id: &str, cx: &App) -> Option<CatalogEntry> {
        self.entries(cx)
            .into_iter()
            .find(|entry| entry.id.eq_ignore_ascii_case(id))
    }

    /// Every id already spoken for, which is what a new one has to dodge.
    pub fn taken_ids(self, cx: &App) -> Vec<String> {
        self.entries(cx).into_iter().map(|entry| entry.id).collect()
    }

    /// The other of the two catalogues.
    ///
    /// Used by the import to work out whether a file that would not parse is a
    /// theme file after all, only of the other kind.
    pub fn other(self) -> Self {
        match self {
            Self::UiTheme => Self::EditorTheme,
            Self::EditorTheme => Self::UiTheme,
        }
    }

    /// Directory this catalogue's user files live in.
    ///
    /// Not created by this call: a user who has added no theme of their own has
    /// no such directory, and both callers cope with that.
    ///
    /// # Errors
    ///
    /// Fails when no home directory can be determined for the current user.
    pub fn directory(self) -> Result<PathBuf> {
        match self {
            Self::UiTheme => rudbman_core::ui_themes_dir(),
            Self::EditorTheme => rudbman_core::editor_themes_dir(),
        }
    }

    /// Parses `path` as one of this catalogue's files.
    ///
    /// The refusal is what this is for; see the module documentation. A file
    /// that does not parse is tried once more against the *other* catalogue,
    /// which costs one more read of a file already in the page cache and is the
    /// difference between "this file is broken" and "this file belongs under the
    /// other picker". The two formats can always be told apart because neither
    /// one's required keys are a subset of the other's: a chrome palette has to
    /// carry `surface`, an editor palette `foreground`.
    ///
    /// # Errors
    ///
    /// Fails when the file cannot be read, does not parse as either format,
    /// parses as the other one, or holds a value that is not a colour.
    pub fn read(self, path: &Path) -> Result<CatalogFile, ImportError> {
        let parsed = match self {
            Self::UiTheme => theme_store::read_file::<ThemeFile>(path)
                .map(|file| CatalogFile::UiTheme(Box::new(file))),
            Self::EditorTheme => theme_store::read_file::<EditorThemeFile>(path)
                .map(|file| CatalogFile::EditorTheme(Box::new(file))),
        };

        let file = match parsed {
            Ok(file) => file,
            Err(_) if self.other().parses(path) => {
                return Err(ImportError::WrongCatalog(self.other()));
            }
            Err(error) => return Err(ImportError::Unreadable(error)),
        };
        file.validate()?;
        Ok(file)
    }

    /// Whether `path` parses as one of this catalogue's files.
    ///
    /// Only the shape is asked about; a file whose colours are nonsense still
    /// answers `true`, because the question this exists to settle is which of
    /// the two formats the user handed over.
    fn parses(self, path: &Path) -> bool {
        match self {
            Self::UiTheme => theme_store::read_file::<ThemeFile>(path).is_ok(),
            Self::EditorTheme => theme_store::read_file::<EditorThemeFile>(path).is_ok(),
        }
    }

    /// Prefix of the ids made up for an entry whose name yields no slug.
    pub fn generated_id_prefix(self) -> &'static str {
        match self {
            Self::UiTheme => theme_store::GENERATED_THEME_ID,
            Self::EditorTheme => theme_store::GENERATED_EDITOR_THEME_ID,
        }
    }

    /// The id selected when the one in hand has just been deleted.
    pub fn default_id(self) -> String {
        let defaults = AppSettings::default();
        match self {
            Self::UiTheme => defaults.theme,
            Self::EditorTheme => defaults.editor_theme,
        }
    }

    /// Removes the file `id` lives in.
    ///
    /// # Errors
    ///
    /// Fails when `id` has no usable slug or the file cannot be removed.
    pub fn delete(self, id: &str) -> Result<()> {
        match self {
            Self::UiTheme => theme_store::delete_ui_theme(id),
            Self::EditorTheme => theme_store::delete_editor_theme(id),
        }
    }

    /// The file that would reproduce the entry `id` names.
    ///
    /// Resolved through the registries rather than read back off the disk, so a
    /// built-in entry — which has no file — duplicates exactly like one of the
    /// user's own. That is also why a duplicated chrome theme arrives with its
    /// five grid slots spelled out: [`ThemeFile::from_theme`] writes the values
    /// that were derived on the way in, and a copy the user is about to edit
    /// should show what it is actually wearing.
    pub fn file_for(self, id: &str, cx: &App) -> Option<CatalogFile> {
        let entry = self.entry(id, cx)?;
        Some(match self {
            Self::UiTheme => CatalogFile::UiTheme(Box::new(ThemeFile::from_theme(
                entry.name,
                &ThemeRegistry::resolve(id, cx),
            ))),
            Self::EditorTheme => CatalogFile::EditorTheme(Box::new(EditorThemeFile::from_theme(
                entry.name,
                &EditorThemeRegistry::resolve(id, cx),
            ))),
        })
    }

    /// Prefix of the element ids of this catalogue's management row.
    ///
    /// Static, and never translated: gpui element ids only have to be unique
    /// among their siblings, and the two rows are siblings within one form.
    pub fn element_prefix(self) -> &'static str {
        match self {
            Self::UiTheme => "settings-ui-theme-action",
            Self::EditorTheme => "settings-editor-theme-action",
        }
    }

    /// Heading shown over the editor while one of this catalogue's entries is
    /// being edited.
    pub fn editor_title(self) -> SharedString {
        match self {
            Self::UiTheme => ts!("settings.editor.theme_title"),
            Self::EditorTheme => ts!("settings.editor.editor_theme_title"),
        }
    }
}

/// One slot of a palette, as the editor has to know it.
struct Slot {
    /// Element id fragment; never translated.
    key: &'static str,
    /// Label shown to the left of the field.
    label: SharedString,
    /// Whether this slot accepts an `#RRGGBBAA` value as well as `#RRGGBB`.
    alpha: bool,
    /// Whether the file may leave the slot out and have it derived.
    optional: bool,
}

/// A required slot.
fn slot(key: &'static str, label: SharedString, alpha: bool) -> Slot {
    Slot {
        key,
        label,
        alpha,
        optional: false,
    }
}

/// A slot the file may omit, in which case it is derived.
fn derived_slot(key: &'static str, label: SharedString, alpha: bool) -> Slot {
    Slot {
        key,
        label,
        alpha,
        optional: true,
    }
}

/// One editable colour: what it is called, and what has been typed into it.
struct ColorField {
    /// Label shown to the left of the field.
    label: SharedString,
    /// Element id fragment; never translated.
    key: &'static str,
    /// Whether this slot accepts an `#RRGGBBAA` value.
    alpha: bool,
    /// Whether an empty field means "derive it" rather than "not a colour".
    optional: bool,
    /// The field itself.
    input: Entity<TextInput>,
}

/// Whether `value` is a colour the file format accepts.
///
/// Stricter than [`parse_hex`] on purpose: that helper takes an alpha channel
/// wherever it finds one, while only a handful of slots are ever *drawn* with
/// one, and a stray eighth digit on an opaque slot is a mistake worth pointing
/// at. An `optional` slot also accepts nothing at all, which is how the file
/// says "derive this one".
fn valid_hex(value: &str, alpha: bool, optional: bool) -> bool {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return optional;
    }
    let digits = trimmed.strip_prefix('#').unwrap_or(trimmed);
    let length_ok = digits.len() == 6 || (alpha && digits.len() == 8);
    length_ok && parse_hex(trimmed).is_some()
}

/// The sixteen chrome slots, in the order [`ThemeColors`] declares them.
///
/// The order is load-bearing: [`ui_colors`] reads the fields back by position,
/// and everything from [`UI_GRID_FIRST`] on is optional.
fn ui_slots() -> Vec<Slot> {
    vec![
        slot("background", ts!("settings.editor.slot.background"), false),
        slot("surface", ts!("settings.editor.slot.surface"), false),
        slot(
            "surface_hover",
            ts!("settings.editor.slot.surface_hover"),
            false,
        ),
        slot(
            "surface_active",
            ts!("settings.editor.slot.surface_active"),
            false,
        ),
        slot("border", ts!("settings.editor.slot.border"), false),
        slot("text", ts!("settings.editor.slot.text"), false),
        slot("text_muted", ts!("settings.editor.slot.text_muted"), false),
        slot("accent", ts!("settings.editor.slot.accent"), false),
        slot("danger", ts!("settings.editor.slot.danger"), false),
        slot("success", ts!("settings.editor.slot.success"), false),
        // The one required slot that is drawn translucent, and so the one that
        // may carry an eighth and ninth hex digit.
        slot("overlay", ts!("settings.editor.slot.overlay"), true),
        // The five the format made optional, so that a theme file written
        // before the result grid existed still loads; see the module docs.
        derived_slot(
            "grid_header",
            ts!("settings.editor.slot.grid_header"),
            false,
        ),
        derived_slot(
            "grid_row_alt",
            ts!("settings.editor.slot.grid_row_alt"),
            false,
        ),
        derived_slot(
            "grid_selection",
            ts!("settings.editor.slot.grid_selection"),
            true,
        ),
        derived_slot("grid_null", ts!("settings.editor.slot.grid_null"), false),
        derived_slot("grid_pk", ts!("settings.editor.slot.grid_pk"), false),
    ]
}

/// The current value of every chrome slot, in [`ui_slots`] order.
///
/// An omitted grid slot becomes an empty field, which is what the editor reads
/// as "derive it".
fn ui_values(colors: &ThemeColors) -> Vec<String> {
    let optional = |value: &Option<String>| value.clone().unwrap_or_default();
    vec![
        colors.background.clone(),
        colors.surface.clone(),
        colors.surface_hover.clone(),
        colors.surface_active.clone(),
        colors.border.clone(),
        colors.text.clone(),
        colors.text_muted.clone(),
        colors.accent.clone(),
        colors.danger.clone(),
        colors.success.clone(),
        colors.overlay.clone(),
        optional(&colors.grid_header),
        optional(&colors.grid_row_alt),
        optional(&colors.grid_selection),
        optional(&colors.grid_null),
        optional(&colors.grid_pk),
    ]
}

/// The chrome slots, read back out of the fields in [`ui_slots`] order.
///
/// An empty optional field is written back as an absent key rather than as an
/// empty string: the loader treats a blank the same way it treats a typo, and
/// the whole point of clearing a grid slot is to get the derivation back.
fn ui_colors(values: &[String]) -> ThemeColors {
    let at = |index: usize| values.get(index).cloned().unwrap_or_default();
    let optional = |index: usize| {
        let value = at(index);
        (!value.trim().is_empty()).then_some(value)
    };
    ThemeColors {
        background: at(0),
        surface: at(1),
        surface_hover: at(2),
        surface_active: at(3),
        border: at(4),
        text: at(5),
        text_muted: at(6),
        accent: at(7),
        danger: at(8),
        success: at(9),
        overlay: at(10),
        grid_header: optional(11),
        grid_row_alt: optional(12),
        grid_selection: optional(13),
        grid_null: optional(14),
        grid_pk: optional(15),
    }
}

/// The colour the palette would derive for the grid slot at `index`.
///
/// Worked out by asking [`ThemeFile::to_theme`] the same question the loader
/// asks: the slot is blanked out, the rest of the fields are left as they are,
/// and whatever comes back is what the file would resolve to without that key.
/// `None` for an index that is not one of the five.
fn derived_ui_color(index: usize, dark: bool, values: &[String]) -> Option<Hsla> {
    let grid_slot = index.checked_sub(UI_GRID_FIRST)?;
    let mut values = values.to_vec();
    *values.get_mut(index)? = String::new();
    let palette = ThemeFile::new("", dark, ui_colors(&values)).to_theme();
    match grid_slot {
        0 => Some(palette.grid_header),
        1 => Some(palette.grid_row_alt),
        2 => Some(palette.grid_selection),
        3 => Some(palette.grid_null),
        4 => Some(palette.grid_pk),
        _ => None,
    }
}

/// The nineteen editor slots, in the order [`EditorThemeColors`] declares them.
///
/// As with [`ui_slots`], the order is what [`editor_colors`] reads back. The
/// two bands drawn *behind* text are the ones that may carry alpha: a selection
/// and a current-line highlight both have to let the glyph under them show.
fn editor_slots() -> Vec<Slot> {
    vec![
        slot("background", ts!("settings.editor.slot.background"), false),
        slot("foreground", ts!("settings.editor.code.foreground"), false),
        slot("cursor", ts!("settings.editor.code.cursor"), false),
        slot("selection", ts!("settings.editor.code.selection"), true),
        slot(
            "line_highlight",
            ts!("settings.editor.code.line_highlight"),
            true,
        ),
        slot("gutter", ts!("settings.editor.code.gutter"), false),
        slot(
            "gutter_active",
            ts!("settings.editor.code.gutter_active"),
            false,
        ),
        slot("keyword", ts!("settings.editor.code.keyword"), false),
        slot("string", ts!("settings.editor.code.string"), false),
        slot("number", ts!("settings.editor.code.number"), false),
        slot("comment", ts!("settings.editor.code.comment"), false),
        slot("function", ts!("settings.editor.code.function"), false),
        slot("type", ts!("settings.editor.code.type"), false),
        slot("operator", ts!("settings.editor.code.operator"), false),
        slot("identifier", ts!("settings.editor.code.identifier"), false),
        slot(
            "punctuation",
            ts!("settings.editor.code.punctuation"),
            false,
        ),
        slot(
            "bracket_match",
            ts!("settings.editor.code.bracket_match"),
            false,
        ),
        slot("error", ts!("settings.editor.code.error"), false),
        slot("warning", ts!("settings.editor.code.warning"), false),
    ]
}

/// The current value of every editor slot, in [`editor_slots`] order.
fn editor_values(colors: &EditorThemeColors) -> Vec<String> {
    vec![
        colors.background.clone(),
        colors.foreground.clone(),
        colors.cursor.clone(),
        colors.selection.clone(),
        colors.line_highlight.clone(),
        colors.gutter.clone(),
        colors.gutter_active.clone(),
        colors.keyword.clone(),
        colors.string.clone(),
        colors.number.clone(),
        colors.comment.clone(),
        colors.function.clone(),
        colors.r#type.clone(),
        colors.operator.clone(),
        colors.identifier.clone(),
        colors.punctuation.clone(),
        colors.bracket_match.clone(),
        colors.error.clone(),
        colors.warning.clone(),
    ]
}

/// The editor slots, read back out of the fields in [`editor_slots`] order.
fn editor_colors(values: &[String]) -> EditorThemeColors {
    let at = |index: usize| values.get(index).cloned().unwrap_or_default();
    EditorThemeColors {
        background: at(0),
        foreground: at(1),
        cursor: at(2),
        selection: at(3),
        line_highlight: at(4),
        gutter: at(5),
        gutter_active: at(6),
        keyword: at(7),
        string: at(8),
        number: at(9),
        comment: at(10),
        function: at(11),
        r#type: at(12),
        operator: at(13),
        identifier: at(14),
        punctuation: at(15),
        bracket_match: at(16),
        error: at(17),
        warning: at(18),
    }
}

/// Emitted by [`ThemeEditor`] when the user is done with it.
pub enum ThemeEditorEvent {
    /// The entry has been written and both registries reloaded. The host has to
    /// repaint whatever was already wearing it.
    Saved,
    /// The user backed out; nothing was written.
    Cancelled,
}

/// Editor for one chrome theme or one editor theme.
///
/// Built with [`ThemeEditor::new`] from the file that is to be edited, rendered
/// as the body of the settings dialog, and finished by one of
/// [`ThemeEditorEvent`]'s two variants. The id it saves under is fixed at
/// construction and never follows the name: renaming a theme must not orphan the
/// settings entry that selected it.
pub struct ThemeEditor {
    /// Which catalogue the entry belongs to.
    catalog: Catalog,
    /// The id it is saved under, from construction to save.
    id: String,
    /// The name, which is the only thing about it that is free text.
    name_input: Entity<TextInput>,
    /// Whether the palette is a dark one.
    dark: bool,
    /// One field per colour slot, in the catalogue's own order.
    fields: Vec<ColorField>,
    /// Why the last save did not go through, if it did not.
    status: Option<SharedString>,
    /// Focus of the editor root; the anchor the host's `Escape` handler sits on.
    focus_handle: FocusHandle,
    /// Whether focus should move into the name field on the next render.
    pending_focus: bool,
    /// Scroll position of the field list.
    scroll: ScrollHandle,
    /// Whether the field list's overlay scroll indicator is on screen.
    scrollbar: ScrollbarState,
}

impl ThemeEditor {
    /// Builds an editor over `file`, which will be saved back under `id`.
    pub fn new(id: impl Into<String>, file: &CatalogFile, cx: &mut Context<Self>) -> Self {
        let catalog = file.catalog();
        let (slots, values, dark) = match file {
            CatalogFile::UiTheme(theme) => (ui_slots(), ui_values(&theme.colors), theme.dark),
            CatalogFile::EditorTheme(theme) => {
                (editor_slots(), editor_values(&theme.colors), theme.dark)
            }
        };

        let name_input = cx.new(|cx| {
            let mut input = TextInput::new(cx).tab_index(tab::NAME);
            input.set_content(file.name().to_owned(), cx);
            input
        });
        // The name is not validated, but it *is* previewed, so the editor has
        // to hear about it changing just as it hears about the colours.
        cx.observe(&name_input, |_editor, _input, cx| cx.notify())
            .detach();

        let mut fields = Vec::with_capacity(slots.len());
        for (index, slot) in slots.into_iter().enumerate() {
            let value = values.get(index).cloned().unwrap_or_default();
            // Marked as it opens, not only once it is typed into: a file edited
            // by hand can arrive with a slot that is not a colour, and the
            // editor is exactly where that has to be visible.
            let valid = valid_hex(&value, slot.alpha, slot.optional);
            let input = cx.new(|cx| {
                let mut input = TextInput::new(cx)
                    .placeholder("#000000")
                    .tab_index(tab::FIRST_COLOR + 2 * index as isize);
                input.set_content(value, cx);
                input.set_invalid(!valid, cx);
                input
            });
            // gpui does not re-render a parent when a child entity notifies, so
            // without this the live preview would only follow the typing at the
            // next unrelated repaint — and the refusal of a malformed colour
            // would never appear at all.
            cx.observe(&input, |editor, _input, cx| editor.revalidate(cx))
                .detach();
            fields.push(ColorField {
                label: slot.label,
                key: slot.key,
                alpha: slot.alpha,
                optional: slot.optional,
                input,
            });
        }

        Self {
            catalog,
            id: id.into(),
            name_input,
            dark,
            fields,
            status: None,
            focus_handle: cx.focus_handle(),
            pending_focus: true,
            scroll: ScrollHandle::new(),
            scrollbar: ScrollbarState::new(),
        }
    }

    /// Heading the host draws over the editor.
    pub fn title(&self) -> SharedString {
        self.catalog.editor_title()
    }

    /// Discards the edits and tells the host to put its own body back.
    pub fn cancel(&mut self, cx: &mut Context<Self>) {
        cx.emit(ThemeEditorEvent::Cancelled);
    }

    /// Re-marks every field that does not hold a colour, and repaints.
    fn revalidate(&mut self, cx: &mut Context<Self>) {
        for field in &self.fields {
            let valid = valid_hex(field.input.read(cx).content(), field.alpha, field.optional);
            field
                .input
                .update(cx, |input, cx| input.set_invalid(!valid, cx));
        }
        cx.notify();
    }

    /// Whether every field holds a colour the format accepts.
    fn is_valid(&self, cx: &App) -> bool {
        self.fields
            .iter()
            .all(|field| valid_hex(field.input.read(cx).content(), field.alpha, field.optional))
    }

    /// What has been typed into every field, in the catalogue's own order.
    fn values(&self, cx: &App) -> Vec<String> {
        self.fields
            .iter()
            .map(|field| field.input.read(cx).content().trim().to_owned())
            .collect()
    }

    /// Puts an optional slot back to automatic by emptying its field.
    fn clear_field(&mut self, index: usize, cx: &mut Context<Self>) {
        let Some(field) = self.fields.get(index) else {
            return;
        };
        field.input.update(cx, |input, cx| input.clear(cx));
        self.revalidate(cx);
    }

    /// The file the fields currently describe.
    fn collect(&self, cx: &App) -> CatalogFile {
        let name = self.name_input.read(cx).content().trim().to_owned();
        let values = self.values(cx);
        match self.catalog {
            Catalog::UiTheme => CatalogFile::UiTheme(Box::new(ThemeFile::new(
                name,
                self.dark,
                ui_colors(&values),
            ))),
            Catalog::EditorTheme => CatalogFile::EditorTheme(Box::new(EditorThemeFile::new(
                name,
                self.dark,
                editor_colors(&values),
            ))),
        }
    }

    /// Writes the edits and reloads both registries.
    ///
    /// A failed write leaves the editor open with the reason showing, so the
    /// user never believes a colour took effect when it did not.
    fn save(&mut self, cx: &mut Context<Self>) {
        if !self.is_valid(cx) {
            self.status = Some(ts!("settings.editor.invalid"));
            cx.notify();
            return;
        }

        let file = self.collect(cx);
        if let Err(err) = file.save(&self.id) {
            log::error!("could not write the {} file: {err:#}", self.id);
            self.status = Some(ts!(
                "settings.manage.write_failed",
                error = format!("{err:#}")
            ));
            cx.notify();
            return;
        }

        theme_store::reload(cx);
        cx.emit(ThemeEditorEvent::Saved);
    }

    /// Moves focus into the name field the first time the editor is drawn.
    fn apply_pending_focus(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.pending_focus {
            return;
        }
        self.pending_focus = false;
        let handle = self.name_input.read(cx).focus_handle(cx);
        window.focus(&handle);
    }

    /// The overlay scroll indicator of the field list, as it stands.
    fn scrollbar(&self) -> Scrollbar {
        Scrollbar::for_handle(SCROLLBAR_ID, ScrollbarAxis::Vertical, &self.scroll)
            .fade(self.scrollbar.fade())
    }

    /// Puts the bar up whenever the list has been scrolled, and starts the
    /// clock that takes it down again.
    fn watch_scroll(&mut self, cx: &mut Context<Self>) {
        let scrolled = scrolled(&self.scroll, ScrollbarAxis::Vertical);
        if let Some(epoch) = self.scrollbar.moved(scrolled) {
            hide_later(epoch, cx, |editor| Some(&mut editor.scrollbar));
        }
    }

    /// Scrolls the list while its thumb is dragged.
    fn drag_scrollbar(&mut self, event: &DragMoveEvent<DraggedThumb>, cx: &mut Context<Self>) {
        let Some(progress) = self.scrollbar().dragged(event, cx) else {
            return;
        };
        self.scrollbar.hold();
        scroll_to(&self.scroll, ScrollbarAxis::Vertical, progress);
        cx.notify();
    }

    /// Lets go of the thumb, and starts its clock again.
    fn release_scrollbar(&mut self, cx: &mut Context<Self>) {
        if let Some(epoch) = self.scrollbar.release() {
            hide_later(epoch, cx, |editor| Some(&mut editor.scrollbar));
            cx.notify();
        }
    }

    /// Puts the bar up while the pointer rests on the edge it rides, and starts
    /// it going the moment the pointer leaves.
    fn hover_scrollbar(&mut self, hovered: bool, cx: &mut Context<Self>) {
        if hovered {
            if self.scrollbar.hover_enter() {
                cx.notify();
            }
            return;
        }

        if let Some(epoch) = self.scrollbar.hover_leave() {
            hide_now(self, epoch, cx, |editor| Some(&mut editor.scrollbar));
        }
    }

    /// The colour a field currently describes.
    ///
    /// For an automatic slot — an optional one left empty — that is the colour
    /// the palette derives, so the swatch never goes blank and the user can see
    /// what "automatic" actually resolved to. `None` only for a field that
    /// holds something which is not a colour at all.
    fn color_of(&self, index: usize, cx: &App) -> Option<Hsla> {
        let field = self.fields.get(index)?;
        let value = field.input.read(cx).content();
        if value.trim().is_empty() {
            return field
                .optional
                .then(|| derived_ui_color(index, self.dark, &self.values(cx)))
                .flatten();
        }
        valid_hex(value, field.alpha, field.optional)
            .then(|| parse_hex(value))
            .flatten()
    }

    /// One labelled colour field: the slot's name, the hex value, the swatch,
    /// and — for an optional slot — the button that puts it back to automatic.
    ///
    /// The swatch is what turns a hex value back into something a person can
    /// judge, and it doubles as the refusal: a field holding anything but a
    /// colour has nothing to draw, so the swatch shows an outline instead — next
    /// to the field, which is itself already outlined in the danger colour.
    ///
    /// An automatic slot is told apart from an explicit one by its label, which
    /// gains the word, and by the muted hex printed after it, which spells out
    /// what the derivation produced; the swatch alone could not say which of the
    /// two a colour came from. That hex is text rather than the field's own
    /// placeholder, because a placeholder rewritten from `render` would notify
    /// the field, which notifies this view, which renders again.
    fn render_field(&self, index: usize, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let chrome = theme(cx);
        let field = &self.fields[index];
        let color = self.color_of(index, cx);
        let automatic = field.optional && field.input.read(cx).content().trim().is_empty();
        let this = cx.entity();

        let swatch = div()
            .flex_none()
            .size(px(SWATCH_SIZE))
            .rounded_md()
            .border_1()
            .border_color(match color {
                Some(_) => chrome.border,
                None => chrome.danger,
            })
            .when_some(color, |this, color| this.bg(color));

        // The two states of an optional slot are mutually exclusive, so they
        // share one cell: an automatic slot shows the hex it resolved to, and
        // an explicit one shows the button that gives the derivation back. A
        // required slot has neither, and no cell.
        let trailing = field.optional.then(|| {
            if automatic {
                div()
                    .flex_none()
                    .w(px(64.))
                    .truncate()
                    .text_size(px(11.))
                    .text_color(chrome.text_muted)
                    .child(SharedString::from(color.map(to_hex).unwrap_or_default()))
                    .into_any_element()
            } else {
                Button::new(
                    ("theme-editor-auto", index),
                    ts!("settings.editor.automatic"),
                )
                .variant(ButtonVariant::Secondary)
                .tab_index(tab::FIRST_COLOR + 2 * index as isize + 1)
                .on_click(move |_, _window, cx| {
                    this.update(cx, |editor, cx| editor.clear_field(index, cx));
                })
                .into_any_element()
            }
        });

        let label = if automatic {
            ts!("settings.editor.automatic_slot", name = field.label.clone())
        } else {
            field.label.clone()
        };

        div()
            // Named after the slot rather than numbered, so that the element
            // keeps its identity as the two catalogues swap field lists.
            .id(field.key)
            .flex()
            .flex_row()
            .items_center()
            .gap(px(8.))
            .flex_1()
            .min_w_0()
            .child(
                div()
                    .flex_none()
                    .w(px(LABEL_WIDTH))
                    .truncate()
                    .text_size(px(12.))
                    .text_color(if automatic {
                        chrome.text_muted
                    } else {
                        chrome.text
                    })
                    .child(label),
            )
            .child(div().flex_1().min_w_0().child(field.input.clone()))
            .child(swatch)
            .children(trailing)
    }

    /// The colour fields, laid out [`FIELD_COLUMNS`] to a row.
    fn render_fields(&self, range: std::ops::Range<usize>, cx: &mut Context<Self>) -> Vec<Div> {
        range
            .collect::<Vec<_>>()
            .chunks(FIELD_COLUMNS)
            .map(|row| {
                let cells: Vec<_> = row
                    .iter()
                    .map(|index| self.render_field(*index, cx).into_any_element())
                    .collect();
                // Pad a short last row so its fields keep the width they have
                // in every other row rather than stretching to fill it.
                let padding = (FIELD_COLUMNS - row.len()) % FIELD_COLUMNS;
                div()
                    .flex()
                    .flex_row()
                    .w_full()
                    .gap(px(12.))
                    .children(cells)
                    .children((0..padding).map(|_| div().flex_1().min_w_0().into_any_element()))
            })
            .collect()
    }

    /// A miniature of the chrome the edited theme would draw.
    ///
    /// The colours a chrome theme is actually judged by: a window background
    /// with a raised surface on it, primary and muted text, a chip each for the
    /// accent and the two status colours, and — because the five grid slots are
    /// invisible anywhere else in this dialog — three rows of a result grid,
    /// header included, with a `NULL` cell and a primary-key column in it.
    fn render_theme_preview(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let values = self.values(cx);
        let palette = ThemeFile::new("", self.dark, ui_colors(&values)).to_theme();
        let name = SharedString::from(self.name_input.read(cx).content().to_owned());

        let chip = |color: Hsla| div().flex_none().size(px(12.)).rounded_full().bg(color);
        let cell = |color: Hsla, text: &'static str| {
            div()
                .flex_1()
                .min_w_0()
                .truncate()
                .px(px(4.))
                .text_color(color)
                .child(text)
        };
        let row = |background: Option<Hsla>, selected: bool| {
            div()
                .flex()
                .flex_row()
                .w_full()
                .py(px(1.))
                .when_some(background, |this, color| this.bg(color))
                .when(selected, |this| this.bg(palette.grid_selection))
                .child(cell(palette.grid_pk, "id"))
                .child(cell(palette.text, "name"))
                .child(cell(palette.grid_null, "NULL"))
        };

        div()
            .flex()
            .flex_col()
            .gap(px(8.))
            .p(px(10.))
            .rounded_md()
            .border_1()
            .border_color(palette.border)
            .bg(palette.background)
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(8.))
                    .px(px(8.))
                    .py(px(6.))
                    .rounded_md()
                    .bg(palette.surface)
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_size(px(12.))
                            .text_color(palette.text)
                            .child(name),
                    )
                    .child(
                        div()
                            .flex_none()
                            .size(px(14.))
                            .rounded_sm()
                            .bg(palette.surface_active),
                    ),
            )
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(6.))
                    .child(chip(palette.accent))
                    .child(chip(palette.success))
                    .child(chip(palette.danger))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_size(px(11.))
                            .text_color(palette.text_muted)
                            .child("Aa Bb Cc 0123"),
                    ),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .w_full()
                    .overflow_hidden()
                    .rounded_sm()
                    .border_1()
                    .border_color(palette.border)
                    .text_size(px(10.))
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .w_full()
                            .py(px(1.))
                            .bg(palette.grid_header)
                            .text_color(palette.text_muted)
                            .child(cell(palette.text_muted, "id"))
                            .child(cell(palette.text_muted, "name"))
                            .child(cell(palette.text_muted, "note")),
                    )
                    .child(row(None, false))
                    .child(row(Some(palette.grid_row_alt), false))
                    .child(row(None, true)),
            )
    }

    /// The statement the edited editor theme would draw.
    ///
    /// Rendered by the same widget the settings dialog picks editor themes with,
    /// over a single card: a syntax palette is judged by whether its classes can
    /// be told apart in an actual statement, and building a second preview here
    /// would be a second chance to disagree with the picker about that.
    fn render_editor_theme_preview(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let values = self.values(cx);
        let palette = EditorThemeFile::new("", self.dark, editor_colors(&values)).to_theme();
        let name = SharedString::from(self.name_input.read(cx).content().to_owned());

        EditorThemePicker::new("theme-editor-preview")
            .options([EditorThemeSwatch::new(self.id.clone(), name).preview(palette)])
            .selected(Some(self.id.clone()))
            .columns(1)
    }

    /// The message strip and the two buttons that end the editor.
    fn render_footer(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let chrome = theme(cx);
        let this = cx.entity();
        let valid = self.is_valid(cx);

        // A refused colour explains itself the moment it is typed rather than
        // waiting for a Save that is already held back — otherwise the only
        // sign would be a greyed-out button with no reason attached.
        let status = self
            .status
            .clone()
            .or_else(|| (!valid).then(|| ts!("settings.editor.invalid")))
            .map(|message| {
                div()
                    .text_size(px(12.))
                    .text_color(chrome.danger)
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
                        Button::new("theme-editor-cancel", ts!("common.cancel"))
                            .variant(ButtonVariant::Secondary)
                            .tab_index(tab::CANCEL)
                            .on_click({
                                let this = this.clone();
                                move |_, _window, cx| {
                                    this.update(cx, |editor, cx| editor.cancel(cx));
                                }
                            }),
                    )
                    .child(
                        Button::new("theme-editor-save", ts!("common.save"))
                            .variant(ButtonVariant::Primary)
                            .disabled(!valid)
                            .tab_index(tab::SAVE)
                            .on_click({
                                let this = this.clone();
                                move |_, _window, cx| {
                                    this.update(cx, |editor, cx| editor.save(cx));
                                }
                            }),
                    ),
            )
    }
}

impl EventEmitter<ThemeEditorEvent> for ThemeEditor {}

impl Focusable for ThemeEditor {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for ThemeEditor {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.apply_pending_focus(window, cx);
        self.watch_scroll(cx);
        let chrome = theme(cx);
        let bar = self
            .scrollbar()
            .on_hover(cx.listener(|editor, hovered: &bool, _window, cx| {
                editor.hover_scrollbar(*hovered, cx);
            }));

        let preview = match self.catalog {
            Catalog::UiTheme => self.render_theme_preview(cx).into_any_element(),
            Catalog::EditorTheme => self.render_editor_theme_preview(cx).into_any_element(),
        };

        let this = cx.entity();
        let dark = Checkbox::new("theme-editor-dark", ts!("settings.editor.dark"))
            .checked(self.dark)
            .tab_index(tab::DARK)
            .on_toggle(move |checked, _window, cx| {
                this.update(cx, |editor, cx| {
                    editor.dark = checked;
                    cx.notify();
                });
            });

        // A chrome theme's five optional slots get a heading of their own:
        // without it they would run on from the eleven required ones with
        // nothing to say that these are the ones the file may leave out.
        let (required, derived) = match self.catalog {
            Catalog::UiTheme => (0..UI_GRID_FIRST, Some(UI_GRID_FIRST..self.fields.len())),
            Catalog::EditorTheme => (0..self.fields.len(), None),
        };

        let list = div()
            .id("theme-editor-fields")
            .track_scroll(&self.scroll)
            .flex()
            .flex_col()
            .min_h_0()
            .gap(px(8.))
            .max_h(px(BODY_MAX_HEIGHT))
            .overflow_y_scroll()
            .child(preview)
            .child(form_row(
                ts!("settings.editor.name"),
                self.name_input.clone(),
            ))
            .child(form_row("", dark))
            .children(self.render_fields(required, cx))
            .children(derived.map(|derived| {
                div()
                    .flex()
                    .flex_col()
                    .gap(px(8.))
                    .pt(px(4.))
                    .child(
                        div()
                            .text_size(px(11.))
                            .text_color(chrome.text_muted)
                            .child(ts!("settings.editor.grid_group")),
                    )
                    .children(self.render_fields(derived, cx))
            }));

        div()
            .id("theme-editor")
            .track_focus(&self.focus_handle)
            .flex()
            .flex_col()
            .min_h_0()
            .gap(px(12.))
            .on_drag_move::<DraggedThumb>(cx.listener(
                |editor, event: &DragMoveEvent<DraggedThumb>, _window, cx| {
                    editor.drag_scrollbar(event, cx);
                },
            ))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|editor, _: &MouseUpEvent, _window, cx| {
                    editor.release_scrollbar(cx);
                }),
            )
            .on_mouse_up_out(
                MouseButton::Left,
                cx.listener(|editor, _: &MouseUpEvent, _window, cx| {
                    editor.release_scrollbar(cx);
                }),
            )
            .child(
                // The middle box exists only to hold the overlay bar, as in the
                // settings form: a scrolling box cannot, because its children
                // are what scroll away underneath it.
                div()
                    .relative()
                    .flex()
                    .flex_col()
                    .min_h_0()
                    .child(list)
                    .children(bar.render(&chrome)),
            )
            .child(self.render_footer(cx))
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use rudbman_ui::{EditorTheme, Theme};

    use super::*;

    /// A chrome file worth round-tripping.
    fn chrome_file() -> CatalogFile {
        CatalogFile::UiTheme(Box::new(ThemeFile::from_theme("Mine", &Theme::dracula())))
    }

    /// An editor file worth round-tripping.
    fn editor_file() -> CatalogFile {
        CatalogFile::EditorTheme(Box::new(EditorThemeFile::from_theme(
            "Mine",
            &EditorTheme::one_dark(),
        )))
    }

    #[test]
    fn every_label_the_editor_draws_has_a_translation() {
        // `t!` answers with the key path itself when no such key exists, so a
        // typo in one of the forty-odd lookups above would reach the screen as
        // "settings.editor.slot.backgrund". Catching it here is cheaper than
        // opening the dialog in eight languages.
        let translated = |label: &SharedString| {
            assert!(!label.is_empty(), "empty label");
            assert!(!label.contains("settings."), "untranslated label {label:?}");
        };

        for slot in ui_slots().into_iter().chain(editor_slots()) {
            translated(&slot.label);
        }
        for label in [
            ts!("settings.editor.name"),
            ts!("settings.editor.dark"),
            ts!("settings.editor.invalid"),
            ts!("settings.editor.automatic"),
            ts!("settings.editor.grid_group"),
            Catalog::UiTheme.editor_title(),
            Catalog::EditorTheme.editor_title(),
        ] {
            translated(&label);
        }

        // The automatic label is interpolated, so it also has to have picked
        // the slot's own name up rather than left `%{name}` standing.
        let automatic = ts!("settings.editor.automatic_slot", name = "Header");
        assert!(automatic.contains("Header"), "{automatic:?}");
        assert_ne!(automatic, "Header");
    }

    #[test]
    fn a_six_digit_colour_is_accepted_everywhere() {
        for value in ["#ff0000", "ff0000", "  #AABBCC  "] {
            for alpha in [false, true] {
                for optional in [false, true] {
                    assert!(valid_hex(value, alpha, optional), "refused {value:?}");
                }
            }
        }
    }

    #[test]
    fn only_a_slot_with_alpha_takes_eight_digits() {
        assert!(valid_hex("#0000009e", true, false));
        assert!(!valid_hex("#0000009e", false, false));
    }

    #[test]
    fn only_an_optional_slot_may_be_left_empty() {
        for value in ["", "   "] {
            assert!(valid_hex(value, false, true), "refused {value:?}");
            assert!(!valid_hex(value, false, false), "accepted {value:?}");
        }
    }

    #[test]
    fn anything_that_is_not_a_colour_is_refused() {
        for value in ["#", "#abc", "#abcde", "#gghhii", "rebeccapurple"] {
            for alpha in [false, true] {
                for optional in [false, true] {
                    assert!(!valid_hex(value, alpha, optional), "accepted {value:?}");
                }
            }
        }
    }

    #[test]
    fn the_chrome_slots_round_trip_through_the_fields() {
        let file = ThemeFile::from_theme("Mine", &Theme::solarized_light());
        let values = ui_values(&file.colors);
        assert_eq!(values.len(), ui_slots().len());
        assert_eq!(values.len(), 16);
        assert_eq!(ui_colors(&values), file.colors);
    }

    #[test]
    fn an_omitted_grid_slot_is_an_empty_field_and_stays_omitted() {
        // The distinction the editor exists to show: a file that leaves the
        // grid out has to open with five empty fields, and saving it again
        // without touching them must not turn those into explicit colours.
        let mut file = ThemeFile::from_theme("Mine", &Theme::dracula());
        file.colors.grid_header = None;
        file.colors.grid_row_alt = None;
        file.colors.grid_selection = None;
        file.colors.grid_null = None;
        file.colors.grid_pk = None;

        let values = ui_values(&file.colors);
        assert!(values[UI_GRID_FIRST..].iter().all(String::is_empty));
        assert_eq!(ui_colors(&values), file.colors);

        // And a whitespace-only field means the same thing as an empty one.
        let mut typed = values.clone();
        typed[UI_GRID_FIRST] = "   ".to_string();
        assert_eq!(ui_colors(&typed).grid_header, None);
    }

    #[test]
    fn an_omitted_grid_slot_still_has_a_colour_to_show() {
        // The swatch must never go blank: an automatic slot shows whatever the
        // palette derived, which is what the loader would have used.
        let mut file = ThemeFile::from_theme("Mine", &Theme::light());
        file.colors.grid_header = None;
        let values = ui_values(&file.colors);

        let derived = derived_ui_color(UI_GRID_FIRST, false, &values).expect("a grid slot");
        assert_eq!(to_hex(derived), to_hex(file.to_theme().grid_header));
        // A required slot has nothing to derive.
        assert_eq!(derived_ui_color(0, false, &values), None);
    }

    #[test]
    fn a_spelled_out_grid_slot_wins_over_the_derivation() {
        let mut values = ui_values(&ThemeFile::from_theme("Mine", &Theme::dark()).colors);
        values[UI_GRID_FIRST] = "#123456".to_string();
        let palette = ThemeFile::new("", true, ui_colors(&values)).to_theme();
        assert_eq!(to_hex(palette.grid_header), "#123456");
        // And the derivation is still what clearing it would go back to.
        assert_ne!(
            to_hex(derived_ui_color(UI_GRID_FIRST, true, &values).expect("derived")),
            "#123456"
        );
    }

    #[test]
    fn the_editor_slots_round_trip_through_the_fields() {
        let file = EditorThemeFile::from_theme("Mine", &EditorTheme::dracula());
        let values = editor_values(&file.colors);
        assert_eq!(values.len(), editor_slots().len());
        assert_eq!(values.len(), 19);
        assert_eq!(editor_colors(&values), file.colors);
    }

    #[test]
    fn every_builtin_palette_writes_values_its_own_fields_accept() {
        // The alpha flags are a claim about which slots are drawn translucent.
        // A built-in whose file carries an eighth digit in a slot marked opaque
        // would open in the editor already refused, which is the one way this
        // table can be wrong without anybody noticing.
        for theme in [
            Theme::dark(),
            Theme::light(),
            Theme::solarized_dark(),
            Theme::solarized_light(),
            Theme::gruvbox_dark(),
            Theme::dracula(),
        ] {
            let values = ui_values(&ThemeFile::from_theme("X", &theme).colors);
            for (index, slot) in ui_slots().into_iter().enumerate() {
                assert!(
                    valid_hex(&values[index], slot.alpha, slot.optional),
                    "{} is refused by its own field: {:?}",
                    slot.key,
                    values[index]
                );
            }
        }

        for theme in [
            EditorTheme::one_dark(),
            EditorTheme::one_light(),
            EditorTheme::solarized_dark(),
            EditorTheme::solarized_light(),
            EditorTheme::gruvbox_dark(),
            EditorTheme::dracula(),
        ] {
            let values = editor_values(&EditorThemeFile::from_theme("X", &theme).colors);
            for (index, slot) in editor_slots().into_iter().enumerate() {
                assert!(
                    valid_hex(&values[index], slot.alpha, slot.optional),
                    "{} is refused by its own field: {:?}",
                    slot.key,
                    values[index]
                );
            }
        }
    }

    /// The one thing [`CatalogFile::save`] adds over the store it delegates to
    /// is picking the right directory and the right table of reserved ids, and
    /// getting that backwards would let an editor theme called `dracula` — a
    /// chrome id, and a free editor id — be refused.
    ///
    /// Asserted through the refusals alone, which is as far as this can go
    /// without writing into the *user's* configuration directory: the write and
    /// the delete themselves are round-tripped against a temporary directory by
    /// `rudbman_ui::theme_store`'s own tests, once per format.
    #[test]
    fn a_builtin_id_is_refused_by_the_catalogue_that_reserves_it() {
        let chrome = CatalogFile::UiTheme(Box::new(ThemeFile::from_theme("Mine", &Theme::dark())));
        let editor = CatalogFile::EditorTheme(Box::new(EditorThemeFile::from_theme(
            "Mine",
            &EditorTheme::one_dark(),
        )));

        assert!(chrome.save("dracula").is_err(), "a built-in chrome id");
        assert!(editor.save("one-dark").is_err(), "a built-in editor id");
        // A name with nothing to slug cannot become a file name either.
        assert!(chrome.save("   ").is_err());
        assert!(editor.save("테마").is_err());
    }

    #[test]
    fn an_exported_file_reads_back_as_the_entry_it_came_from() {
        // The whole point of the pair: what `Export` writes is what `Import`
        // takes, for both catalogues and without a trip through the settings
        // directory. A built-in is used on purpose — exporting one is how a
        // user gets a starting point, so it has to survive the round trip.
        let directory = tempfile::tempdir().expect("a temporary directory");

        for original in [chrome_file(), editor_file()] {
            let catalog = original.catalog();
            let path = directory.path().join(format!("{catalog:?}.json"));
            original.write(&path).expect("the export");

            let read = catalog.read(&path).expect("the import");
            assert_eq!(read.catalog(), catalog);
            assert_eq!(read.name(), original.name());
            match (&read, &original) {
                (CatalogFile::UiTheme(read), CatalogFile::UiTheme(original)) => {
                    assert_eq!(read, original);
                }
                (CatalogFile::EditorTheme(read), CatalogFile::EditorTheme(original)) => {
                    assert_eq!(read, original);
                }
                _ => panic!("the round trip changed catalogue"),
            }
        }
    }

    #[test]
    fn a_file_of_the_other_catalogue_is_refused_by_name() {
        // The refusal this module exists for: the two formats are close enough
        // to look interchangeable, and installing half a theme because the
        // absent keys defaulted would be the worst outcome of the two.
        let directory = tempfile::tempdir().expect("a temporary directory");

        for file in [chrome_file(), editor_file()] {
            let wrong = file.catalog().other();
            let path = directory.path().join("theme.json");
            file.write(&path).expect("the export");

            match wrong.read(&path) {
                Err(ImportError::WrongCatalog(actual)) => assert_eq!(actual, file.catalog()),
                other => panic!("{wrong:?} accepted a {:?} file: {other:?}", file.catalog()),
            }
        }
    }

    #[test]
    fn a_file_that_is_not_a_theme_at_all_is_refused_rather_than_panicking() {
        let directory = tempfile::tempdir().expect("a temporary directory");
        let path = directory.path().join("broken.json");

        // Not JSON, valid JSON of the wrong shape, JSON that stops halfway, an
        // empty file, and a path with nothing behind it at all.
        for contents in [
            "not json at all",
            "[]",
            "{\"name\": \"Mine\", \"colors\":",
            "{}",
            "",
        ] {
            fs::write(&path, contents).expect("the fixture");
            for catalog in [Catalog::UiTheme, Catalog::EditorTheme] {
                assert!(
                    matches!(catalog.read(&path), Err(ImportError::Unreadable(_))),
                    "{catalog:?} accepted {contents:?}"
                );
            }
        }

        let missing = directory.path().join("no-such-file.json");
        assert!(matches!(
            Catalog::UiTheme.read(&missing),
            Err(ImportError::Unreadable(_))
        ));
    }

    #[test]
    fn a_slot_that_is_not_a_colour_is_refused_by_the_slot_it_belongs_to() {
        // `ThemeFile::to_theme` would happily substitute its fallback here,
        // which is right for a directory scan and wrong for one deliberate
        // import: the user has to be told the file is not what it claims.
        let directory = tempfile::tempdir().expect("a temporary directory");
        let path = directory.path().join("theme.json");

        let CatalogFile::UiTheme(mut chrome) = chrome_file() else {
            unreachable!("a chrome file");
        };
        chrome.colors.accent = "rebeccapurple".to_string();
        CatalogFile::UiTheme(chrome).write(&path).expect("write");
        match Catalog::UiTheme.read(&path) {
            Err(ImportError::BadColor(slot)) => {
                assert_eq!(slot, ts!("settings.editor.slot.accent"));
            }
            other => panic!("accepted a colour that is not one: {other:?}"),
        }

        // An eighth digit on a slot that is never drawn translucent is refused
        // by the same table the editor's fields are built from.
        let CatalogFile::EditorTheme(mut editor) = editor_file() else {
            unreachable!("an editor file");
        };
        editor.colors.keyword = "#11223344".to_string();
        CatalogFile::EditorTheme(editor)
            .write(&path)
            .expect("write");
        assert!(matches!(
            Catalog::EditorTheme.read(&path),
            Err(ImportError::BadColor(_))
        ));

        // But a grid slot the file simply leaves out is not a bad colour; it is
        // the one thing an empty slot is allowed to mean.
        let CatalogFile::UiTheme(mut chrome) = chrome_file() else {
            unreachable!("a chrome file");
        };
        chrome.colors.grid_header = None;
        chrome.colors.grid_selection = None;
        CatalogFile::UiTheme(chrome).write(&path).expect("write");
        assert!(Catalog::UiTheme.read(&path).is_ok());
    }

    #[test]
    fn an_imported_id_that_is_taken_is_renamed_rather_than_written_over() {
        // The id the install picks, asserted through the very call it makes:
        // the file's own name first, its stem second, and a suffix until the id
        // is free — of built-ins and of the files installed earlier in the same
        // batch alike.
        let taken = |ids: &[&str]| ids.iter().map(|id| id.to_string()).collect::<Vec<_>>();
        let prefix = Catalog::EditorTheme.generated_id_prefix();

        assert_eq!(
            theme_store::unique_id(&["One Dark", "downloaded"], prefix, &taken(&[])),
            "one-dark"
        );
        // A built-in of the same name is never written over.
        assert_eq!(
            theme_store::unique_id(&["One Dark", "downloaded"], prefix, &taken(&["one-dark"])),
            "one-dark-2"
        );
        // Nor is the file installed a moment ago in the same batch.
        assert_eq!(
            theme_store::unique_id(
                &["One Dark", "downloaded"],
                prefix,
                &taken(&["one-dark", "one-dark-2"])
            ),
            "one-dark-3"
        );
        // A name with nothing to slug falls back to the file's own stem, and
        // then to a made-up id — never to an empty one.
        assert_eq!(
            theme_store::unique_id(&["테마", "downloaded"], prefix, &taken(&[])),
            "downloaded"
        );
        assert_eq!(
            theme_store::unique_id(&["테마", "테마"], prefix, &taken(&[])),
            format!("{prefix}-1")
        );
    }

    #[test]
    fn every_refusal_has_a_sentence_of_its_own() {
        let messages = [
            ImportError::Unreadable(anyhow::anyhow!("no such file")).message("mine.json"),
            ImportError::WrongCatalog(Catalog::EditorTheme).message("mine.json"),
            ImportError::WrongCatalog(Catalog::UiTheme).message("mine.json"),
            ImportError::BadColor(ts!("settings.editor.slot.accent")).message("mine.json"),
        ];

        for message in &messages {
            assert!(!message.contains("settings."), "untranslated {message:?}");
            // Every one of them names the file that was refused, which is what
            // makes a batch of them readable at all.
            assert!(message.contains("mine.json"), "unnamed file in {message:?}");
        }
        // And the two catalogues do not share a sentence, or the message would
        // be pointing at the row the user is already standing in.
        assert_ne!(messages[1], messages[2]);

        assert!(messages[0].contains("no such file"), "{:?}", messages[0]);
        let accent = ts!("settings.editor.slot.accent");
        assert!(messages[3].contains(accent.as_ref()), "{:?}", messages[3]);
    }

    #[test]
    fn the_two_catalogues_never_share_an_element_prefix() {
        assert_ne!(
            Catalog::UiTheme.element_prefix(),
            Catalog::EditorTheme.element_prefix()
        );
        assert_ne!(
            Catalog::UiTheme.generated_id_prefix(),
            Catalog::EditorTheme.generated_id_prefix()
        );
        // Both defaults resolve to something the registries actually know.
        assert!(ThemeRegistry::is_builtin(&Catalog::UiTheme.default_id()));
        assert!(EditorThemeRegistry::is_builtin(
            &Catalog::EditorTheme.default_id()
        ));

        assert_eq!(Catalog::UiTheme.other(), Catalog::EditorTheme);
        assert_eq!(Catalog::EditorTheme.other(), Catalog::UiTheme);
        // An import lands in the catalogue's own directory, so the two sharing
        // one would put every chrome theme in the editor theme picker. Only
        // asserted where a home directory could be found at all.
        if let (Ok(chrome), Ok(editor)) = (
            Catalog::UiTheme.directory(),
            Catalog::EditorTheme.directory(),
        ) {
            assert_ne!(chrome, editor);
        }
    }

    #[test]
    fn the_colour_fields_leave_room_for_their_own_revert_buttons() {
        // Every field takes two indices — its own and the button behind it —
        // so the last one has to stay clear of the footer.
        let last = tab::FIRST_COLOR + 2 * ui_slots().len().max(editor_slots().len()) as isize;
        assert!(last < tab::CANCEL);
        const { assert!(tab::NAME < tab::DARK && tab::DARK < tab::FIRST_COLOR) };
        const { assert!(tab::CANCEL < tab::SAVE) };
    }
}
