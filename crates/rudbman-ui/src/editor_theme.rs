//! Color palette used by the SQL editor, and by nothing else.
//!
//! A separate palette from [`crate::theme`], with its own file format and its
//! own directory, because the two answer different questions. The chrome
//! palette is eleven semantic slots — a surface, a border, a danger — and it
//! has to hold for buttons and tabs and dialogs alike. An editor palette is a
//! *syntax* palette: nineteen slots that only mean anything once a lexer has
//! said which run of characters is a keyword and which is a string, and the
//! published palettes people actually want — Dracula, Gruvbox, Monokai,
//! whatever their editor of choice ships — are written in those terms and no
//! other.
//!
//! Keeping them apart is what lets the two be picked independently — a light
//! chrome around a dark editor is a real preference, not a mistake — and what
//! lets an editor palette be copied out of somewhere else without first being
//! translated into chrome.
//!
//! The shape mirrors [`crate::theme`] exactly, so that whatever is true of one
//! is true of the other: [`EditorTheme`] is the resolved palette and a gpui
//! [`Global`], [`EditorThemeFile`] is the on-disk form,
//! [`crate::theme_store`] reads the files, and [`EditorThemeRegistry`] lists
//! and resolves the built-in and custom themes together. The colors are parsed
//! with the chrome palette's own [`parse_hex`], since `#RRGGBB` is `#RRGGBB`.
//!
//! Unlike the chrome palette, nothing here is derived. Every slot of a syntax
//! palette is a choice its author made about which token deserves which color,
//! and there is no "worked out from the others" answer to that: an operator is
//! not a shade of a keyword. What replaces derivation is forgiveness — a slot
//! that cannot be parsed falls back to the built-in theme of the same
//! *darkness*, so a typo in a light theme cannot drop a near-black keyword onto
//! a near-white page.

use gpui::{App, Global, Hsla};
use serde::{Deserialize, Serialize};

use crate::theme::{parse_hex, to_hex};

/// A syntax palette: the colors an editor paints one buffer with.
///
/// The nineteen slots split three ways. Four are the *canvas* —
/// [`background`](Self::background), [`foreground`](Self::foreground),
/// [`cursor`](Self::cursor), [`selection`](Self::selection) — three are the
/// *frame* around it — [`line_highlight`](Self::line_highlight),
/// [`gutter`](Self::gutter), [`gutter_active`](Self::gutter_active) — and the
/// remaining twelve are token classes a lexer hands out.
#[derive(Debug, Clone, PartialEq)]
pub struct EditorTheme {
    /// Whether this is a dark palette.
    ///
    /// Read by the settings option that keeps the editor on the same side of
    /// light/dark as the chrome, and by the picker, which groups by it.
    pub dark: bool,
    /// Background of the text area.
    pub background: Hsla,
    /// Default text color, and the color of anything the lexer did not classify.
    pub foreground: Hsla,
    /// The caret.
    pub cursor: Hsla,
    /// Background of the selected range.
    pub selection: Hsla,
    /// Background of the line the caret is on.
    pub line_highlight: Hsla,
    /// Line numbers in the gutter.
    pub gutter: Hsla,
    /// The line number of the line the caret is on.
    pub gutter_active: Hsla,
    /// Reserved words: `SELECT`, `FROM`, `WHERE`, `JOIN`.
    pub keyword: Hsla,
    /// String and date literals.
    pub string: Hsla,
    /// Numeric literals.
    pub number: Hsla,
    /// Line and block comments.
    pub comment: Hsla,
    /// Called functions: `COUNT`, `COALESCE`, and the user's own.
    pub function: Hsla,
    /// Type names in casts and DDL: `INTEGER`, `VARCHAR`, `TIMESTAMP`.
    pub r#type: Hsla,
    /// Operators: `=`, `<>`, `||`, `+`.
    pub operator: Hsla,
    /// Table, column and alias names.
    pub identifier: Hsla,
    /// Commas, semicolons, parentheses and dots.
    pub punctuation: Hsla,
    /// The bracket under the caret and its partner.
    pub bracket_match: Hsla,
    /// Squiggles and marks on a statement the parser rejected.
    pub error: Hsla,
    /// Squiggles and marks on a statement that parses but looks wrong.
    pub warning: Hsla,
}

impl EditorTheme {
    /// One Dark, the default.
    ///
    /// Atom's `one-dark-syntax`, which is where the chrome palette of the same
    /// id comes from too, so the default chrome and the default editor are one
    /// design rather than two. Plain identifiers are left at the foreground
    /// color, as One Dark itself leaves them: in SQL they outnumber everything
    /// else on the line, and a palette that tints them tints the whole buffer.
    pub fn one_dark() -> Self {
        Self {
            dark: true,
            background: hex("#282c34"),
            foreground: hex("#abb2bf"),
            cursor: hex("#528bff"),
            selection: hex("#3e4451"),
            line_highlight: hex("#2c313c"),
            gutter: hex("#4b5263"),
            gutter_active: hex("#abb2bf"),
            keyword: hex("#c678dd"),
            string: hex("#98c379"),
            number: hex("#d19a66"),
            comment: hex("#5c6370"),
            function: hex("#61afef"),
            r#type: hex("#e5c07b"),
            operator: hex("#56b6c2"),
            identifier: hex("#abb2bf"),
            punctuation: hex("#abb2bf"),
            bracket_match: hex("#56b6c2"),
            error: hex("#e06c75"),
            warning: hex("#e5c07b"),
        }
    }

    /// One Light, the light counterpart of [`EditorTheme::one_dark`].
    ///
    /// Atom's `one-light-syntax`, hue for hue: the same twelve token classes
    /// pointed at the light end of the same palette.
    pub fn one_light() -> Self {
        Self {
            dark: false,
            background: hex("#fafafa"),
            foreground: hex("#383a42"),
            cursor: hex("#526fff"),
            selection: hex("#e5e5e6"),
            line_highlight: hex("#f0f0f1"),
            gutter: hex("#9d9d9f"),
            gutter_active: hex("#383a42"),
            keyword: hex("#a626a4"),
            string: hex("#50a14f"),
            number: hex("#986801"),
            comment: hex("#a0a1a7"),
            function: hex("#4078f2"),
            r#type: hex("#c18401"),
            operator: hex("#0184bc"),
            identifier: hex("#383a42"),
            punctuation: hex("#383a42"),
            bracket_match: hex("#0184bc"),
            error: hex("#e45649"),
            warning: hex("#986801"),
        }
    }

    /// Solarized Dark, the mirror of [`EditorTheme::solarized_light`].
    ///
    /// Ethan Schoonover's palette read from the other end, with the tones named
    /// as his official `solarized.xresources` names them: `base03` is the page,
    /// `base0` the body text, `base1` the caret and `base01` the de-emphasised
    /// gutter and comments — the same four roles the light variant fills with
    /// `base3`, `base00`, `base01` and `base1`. The accents are unchanged from
    /// the light variant, down to violet standing in for operators, because
    /// holding the eight accents fixed while only the monotones swap is the
    /// whole design of Solarized; a token that changed hue between the two
    /// would be a different palette wearing the name.
    ///
    /// The two slots Solarized does not name are extrapolated the way the light
    /// variant extrapolates them, in the opposite direction. The current-line
    /// band is `base02`, which Schoonover documents as "background highlights"
    /// and which is exactly what a current-line stripe is. The selection is one
    /// further step along the same ramp — `base03` to `base02` is `+07 +0b +0c`,
    /// and taking that step again lands on `#0e414e` — since a selection drawn
    /// *in* the highlight band would vanish on the line the caret is on, and the
    /// next tone Solarized actually names, `base01`, is the comment color and
    /// would swallow a commented-out line the moment it were selected.
    ///
    /// Worth noting against the sibling project: rulogman's terminal scheme
    /// spends `base02` on the *selection*, because a terminal palette has four
    /// slots and no current-line band at all, so the highlight tone has nowhere
    /// else to go. An editor palette has both bands, and here `base02` goes to
    /// the one Solarized named it for.
    pub fn solarized_dark() -> Self {
        Self {
            dark: true,
            background: hex("#002b36"),
            foreground: hex("#839496"),
            cursor: hex("#93a1a1"),
            selection: hex("#0e414e"),
            line_highlight: hex("#073642"),
            gutter: hex("#586e75"),
            gutter_active: hex("#93a1a1"),
            keyword: hex("#859900"),
            string: hex("#2aa198"),
            number: hex("#d33682"),
            comment: hex("#586e75"),
            function: hex("#268bd2"),
            r#type: hex("#b58900"),
            // Violet, the accent Solarized keeps spare, as in the light variant.
            operator: hex("#6c71c4"),
            identifier: hex("#839496"),
            punctuation: hex("#839496"),
            bracket_match: hex("#cb4b16"),
            error: hex("#dc322f"),
            warning: hex("#cb4b16"),
        }
    }

    /// Solarized Light.
    ///
    /// Ethan Schoonover's palette, with `base3` as the page, `base00` as the
    /// body text and `base1` as the de-emphasised gutter and comments. Two
    /// slots Solarized does not spell out are extrapolated from the ones it
    /// does: the current-line band is `base2`, the standard highlight
    /// background, and the selection is a step past it, since a selection drawn
    /// *in* the highlight band would vanish on the line the caret is on.
    pub fn solarized_light() -> Self {
        Self {
            dark: false,
            background: hex("#fdf6e3"),
            foreground: hex("#657b83"),
            cursor: hex("#586e75"),
            selection: hex("#dfd8c3"),
            line_highlight: hex("#eee8d5"),
            gutter: hex("#93a1a1"),
            gutter_active: hex("#586e75"),
            keyword: hex("#859900"),
            string: hex("#2aa198"),
            number: hex("#d33682"),
            comment: hex("#93a1a1"),
            function: hex("#268bd2"),
            r#type: hex("#b58900"),
            // Solarized reserves violet for whatever a language needs a fifth
            // accent for; operators are what SQL needs one for.
            operator: hex("#6c71c4"),
            identifier: hex("#657b83"),
            punctuation: hex("#657b83"),
            bracket_match: hex("#cb4b16"),
            error: hex("#dc322f"),
            warning: hex("#cb4b16"),
        }
    }

    /// Gruvbox Dark.
    ///
    /// morhetz's palette at its default (medium) contrast, with the tone names
    /// and the accent values of `gruvbox-contrib`'s `gruvbox-dark.xresources`:
    /// `dark0` (`#282828`) is the page, `light1` (`#ebdbb2`) the body text and
    /// the caret, and the accents are that file's *bright* ramp — red
    /// `#fb4934`, green `#b8bb26`, yellow `#fabd2f`, purple `#d3869b`, aqua
    /// `#8ec07c` — which is the ramp gruvbox itself highlights code with, the
    /// neutral one being reserved for the terminal's normal colors.
    ///
    /// The three bands behind the text walk up the `dark0`/`dark1`/`dark2` ramp
    /// gruvbox already defines rather than inventing anything, which puts the
    /// selection on the `dark2` the sibling terminal scheme uses. The gutter
    /// takes `dark4`, the last step of that ramp, leaving line numbers quieter
    /// than the gray comments and quieter than every token class — which is
    /// what a gutter is for.
    ///
    /// The token classes follow the vim theme's own highlight links — red
    /// keywords, green strings, purple constants, yellow types, orange
    /// operators and delimiters — with one departure. `Function` there is
    /// green, the same green as a string, and in SQL that puts `COUNT(` and
    /// `'...'` in one color on nearly every line; aqua, which gruvbox spends on
    /// preprocessor directives that SQL has none of, takes functions instead.
    ///
    /// The gray comments are the dimmest thing here at 4.02:1 against the page,
    /// which is a narrow margin over the red keywords at 4.29:1 — narrow, but
    /// the right way round, and both are gruvbox's own values, so neither is
    /// nudged to widen it.
    pub fn gruvbox_dark() -> Self {
        Self {
            dark: true,
            background: hex("#282828"),
            foreground: hex("#ebdbb2"),
            cursor: hex("#ebdbb2"),
            selection: hex("#504945"),
            line_highlight: hex("#3c3836"),
            gutter: hex("#7c6f64"),
            gutter_active: hex("#ebdbb2"),
            keyword: hex("#fb4934"),
            string: hex("#b8bb26"),
            number: hex("#d3869b"),
            comment: hex("#928374"),
            function: hex("#8ec07c"),
            r#type: hex("#fabd2f"),
            operator: hex("#fe8019"),
            identifier: hex("#ebdbb2"),
            punctuation: hex("#ebdbb2"),
            bracket_match: hex("#fe8019"),
            error: hex("#fb4934"),
            warning: hex("#fabd2f"),
        }
    }

    /// Dracula.
    ///
    /// The specification published at `draculatheme.com`, slot for slot:
    /// `Background` `#282a36` for the page, `Foreground` `#f8f8f2` for the text
    /// and the caret, `Comment` `#6272a4` for comments and the gutter, and the
    /// named accents spent the way Dracula's own language definitions spend
    /// them — `Pink` keywords, `Yellow` strings, `Purple` numbers, `Green`
    /// functions, `Cyan` type names, `Orange` for the matched bracket and the
    /// warning mark, `Red` for the error mark.
    ///
    /// That the strings are yellow rather than green is where this parts from
    /// the sibling terminal scheme, which derives its editor colors out of the
    /// sixteen ANSI slots of `dracula/xresources` and so has to take green for
    /// strings the way every terminal palette does. Nothing here is derived
    /// from ANSI, so Dracula's own answer wins.
    ///
    /// One extrapolation. Dracula publishes a single `#44475a` and names it
    /// both "Current Line" and "Selection", so taking it at its word would
    /// paint two of the three bands behind the text identically and leave a
    /// selection invisible on the one line a reader is guaranteed to be
    /// selecting on. The selection keeps the published value — that is the band
    /// being looked at, and it is what the terminal scheme uses too — and the
    /// current-line stripe is pulled halfway back toward the page: `#363848`,
    /// the midpoint of `#282a36` and `#44475a`, so the caret's line reads as a
    /// hint beneath the selection rather than as a second one.
    pub fn dracula() -> Self {
        Self {
            dark: true,
            background: hex("#282a36"),
            foreground: hex("#f8f8f2"),
            cursor: hex("#f8f8f2"),
            selection: hex("#44475a"),
            line_highlight: hex("#363848"),
            gutter: hex("#6272a4"),
            gutter_active: hex("#f8f8f2"),
            keyword: hex("#ff79c6"),
            string: hex("#f1fa8c"),
            number: hex("#bd93f9"),
            comment: hex("#6272a4"),
            function: hex("#50fa7b"),
            r#type: hex("#8be9fd"),
            // Pink again: Dracula tints operators with the keyword color, which
            // is the one pairing its spec spells out for punctuation-like runs.
            operator: hex("#ff79c6"),
            identifier: hex("#f8f8f2"),
            punctuation: hex("#f8f8f2"),
            bracket_match: hex("#ffb86c"),
            error: hex("#ff5555"),
            warning: hex("#ffb86c"),
        }
    }
}

/// Parses a color a built-in palette spelled out.
///
/// Panicking is right here and only here: the argument is a literal in this
/// file, so a failure is a typo caught by the first test that touches the
/// theme, not anything a user can reach. Every color that comes from *outside*
/// goes through [`parse_hex`] and falls back instead.
fn hex(value: &str) -> Hsla {
    parse_hex(value).expect("a built-in editor theme color is a valid hex string")
}

impl Default for EditorTheme {
    fn default() -> Self {
        Self::one_dark()
    }
}

impl Global for EditorTheme {}

/// Id of the default editor theme.
const ID_ONE_DARK: &str = "one-dark";
/// Id of the light counterpart of [`ID_ONE_DARK`].
const ID_ONE_LIGHT: &str = "one-light";
/// Id of the Solarized Dark editor theme.
const ID_SOLARIZED_DARK: &str = "solarized-dark";
/// Id of the Solarized Light editor theme.
const ID_SOLARIZED_LIGHT: &str = "solarized-light";
/// Id of the Gruvbox Dark editor theme.
const ID_GRUVBOX_DARK: &str = "gruvbox-dark";
/// Id of the Dracula editor theme.
const ID_DRACULA: &str = "dracula";

/// One entry of the built-in editor theme table.
struct BuiltinEditorTheme {
    /// Stable id stored in settings.
    id: &'static str,
    /// Human-readable name.
    name: &'static str,
    /// Whether the palette is a dark one.
    dark: bool,
    /// Builds the palette. A function rather than a value because [`Hsla`] is
    /// not constructible in a `const`.
    build: fn() -> EditorTheme,
}

/// Every built-in editor theme, in the order [`crate::theme`] lists its own.
///
/// The six ids are exactly the six ids of the built-in *chrome* themes, name
/// for name and in the same order, which is what the settings option that keeps
/// the editor in step with the chrome is built on: it resolves the chrome
/// theme's id in this table and always finds the syntax palette drawn by
/// whoever drew the window around it. Picking a built-in chrome theme therefore
/// never leaves the editor on a palette from somewhere else.
///
/// That the two tables agree is a property of the built-ins and not of the
/// design — see [`EditorThemeRegistry::is_builtin`] — since a theme of the
/// user's own can exist on one side and not the other, and the two settings
/// stay independently selectable.
const BUILTIN_EDITOR_THEMES: [BuiltinEditorTheme; 6] = [
    BuiltinEditorTheme {
        id: ID_ONE_DARK,
        name: "One Dark",
        dark: true,
        build: EditorTheme::one_dark,
    },
    BuiltinEditorTheme {
        id: ID_ONE_LIGHT,
        name: "One Light",
        dark: false,
        build: EditorTheme::one_light,
    },
    BuiltinEditorTheme {
        id: ID_SOLARIZED_DARK,
        name: "Solarized Dark",
        dark: true,
        build: EditorTheme::solarized_dark,
    },
    BuiltinEditorTheme {
        id: ID_SOLARIZED_LIGHT,
        name: "Solarized Light",
        dark: false,
        build: EditorTheme::solarized_light,
    },
    BuiltinEditorTheme {
        id: ID_GRUVBOX_DARK,
        name: "Gruvbox Dark",
        dark: true,
        build: EditorTheme::gruvbox_dark,
    },
    BuiltinEditorTheme {
        id: ID_DRACULA,
        name: "Dracula",
        dark: true,
        build: EditorTheme::dracula,
    },
];

/// An editor theme loaded from a file rather than compiled in.
#[derive(Debug, Clone)]
pub struct CustomEditorTheme {
    /// Stable id stored in settings, taken from the file name.
    pub id: String,
    /// Human-readable name, taken from the file's `name` key.
    pub name: String,
    /// The palette itself.
    pub theme: EditorTheme,
}

/// One entry of the combined built-in + custom editor theme listing.
///
/// What a picker needs to draw a row, and nothing more; the colors are fetched
/// with [`EditorThemeRegistry::resolve`] only for the entries that end up on
/// screen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EditorThemeEntry {
    /// Stable id stored in settings.
    pub id: String,
    /// Human-readable name.
    pub name: String,
    /// Whether the palette is a dark one.
    pub dark: bool,
    /// Whether the theme ships with rudbman rather than coming from a file.
    pub builtin: bool,
}

/// The editor themes read from the user's `editor-themes` directory.
#[derive(Debug, Default)]
pub struct EditorThemeRegistry {
    /// The custom themes, in the order the loader found them.
    custom: Vec<CustomEditorTheme>,
}

impl Global for EditorThemeRegistry {}

impl EditorThemeRegistry {
    /// Installs an empty registry, if none has been installed yet.
    ///
    /// Called from [`crate::init`], so that resolving an id before the theme
    /// files have been read answers the built-in themes rather than panicking
    /// on a missing global.
    pub fn init(cx: &mut App) {
        if cx.try_global::<Self>().is_none() {
            cx.set_global(Self::default());
        }
    }

    /// Replaces the themes loaded from the user's `editor-themes` directory.
    ///
    /// The whole list is swapped at once, so re-scanning the directory cannot
    /// leave behind a theme its file no longer defines.
    pub fn set_custom(themes: Vec<CustomEditorTheme>, cx: &mut App) {
        cx.set_global(Self { custom: themes });
    }

    /// The themes currently loaded from the user's `editor-themes` directory.
    pub fn custom(cx: &App) -> Vec<CustomEditorTheme> {
        cx.try_global::<Self>()
            .map(|registry| registry.custom.clone())
            .unwrap_or_default()
    }

    /// Whether `id` names an editor theme that ships with rudbman.
    ///
    /// Answering `true` here does *not* imply the chrome theme of the same id
    /// exists, or the other way round: the two tables are independent, and that
    /// the built-ins currently line up id for id is a fact about the palettes
    /// rudbman happens to ship rather than a rule. A `tokyo-night.json` the user
    /// drops into `editor-themes` is a built-in of neither table, and a chrome
    /// theme they wrote is not an editor theme however it is named.
    pub fn is_builtin(id: &str) -> bool {
        BUILTIN_EDITOR_THEMES
            .iter()
            .any(|theme| theme.id.eq_ignore_ascii_case(id))
    }

    /// Every selectable editor theme: the built-in ones in presentation order,
    /// then the custom ones sorted by name.
    ///
    /// A custom theme whose id shadows a built-in one is left out, since
    /// [`EditorThemeRegistry::resolve`] would never hand it back anyway.
    pub fn all(cx: &App) -> Vec<EditorThemeEntry> {
        let mut entries: Vec<EditorThemeEntry> = BUILTIN_EDITOR_THEMES
            .iter()
            .map(|theme| EditorThemeEntry {
                id: theme.id.to_string(),
                name: theme.name.to_string(),
                dark: theme.dark,
                builtin: true,
            })
            .collect();

        let mut custom: Vec<EditorThemeEntry> = Self::custom(cx)
            .into_iter()
            .filter(|theme| !Self::is_builtin(&theme.id))
            .map(|theme| EditorThemeEntry {
                dark: theme.theme.dark,
                id: theme.id,
                name: theme.name,
                builtin: false,
            })
            .collect();
        custom.sort_by(|a, b| a.name.cmp(&b.name));

        entries.append(&mut custom);
        entries
    }

    /// The palette `id` names, falling back to [`EditorTheme::one_dark`].
    ///
    /// Ids are case-insensitive and built-in themes win over custom ones. An id
    /// nothing answers to falls back rather than failing: a settings file
    /// naming a theme whose file has been deleted has to keep opening the app.
    pub fn resolve(id: &str, cx: &App) -> EditorTheme {
        if let Some(builtin) = BUILTIN_EDITOR_THEMES
            .iter()
            .find(|theme| theme.id.eq_ignore_ascii_case(id))
        {
            return (builtin.build)();
        }
        Self::custom(cx)
            .into_iter()
            .find(|theme| theme.id.eq_ignore_ascii_case(id))
            .map(|theme| theme.theme)
            .unwrap_or_else(EditorTheme::one_dark)
    }
}

/// Schema version written into an [`EditorThemeFile`] by this build.
const EDITOR_THEME_FILE_VERSION: u32 = 1;

/// Version assumed for a file that does not carry one.
fn default_editor_theme_file_version() -> u32 {
    EDITOR_THEME_FILE_VERSION
}

/// One editor theme as it is written to disk.
///
/// Hand-editable by design, and read the same way `settings.json` is: keys
/// rudbman does not know are ignored, and a color it cannot parse falls back to
/// the corresponding slot of a built-in theme instead of failing the file.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EditorThemeFile {
    /// Schema version of the file; informational until a migration is needed.
    #[serde(default = "default_editor_theme_file_version")]
    pub version: u32,
    /// Human-readable name, shown in the picker.
    pub name: String,
    /// Whether the palette is a dark one.
    ///
    /// Load-bearing beyond the picker: it also picks which built-in theme an
    /// unparseable color falls back to. See [`EditorThemeFile::to_theme`].
    #[serde(default)]
    pub dark: bool,
    /// The palette itself.
    pub colors: EditorThemeColors,
}

/// The color slots of an [`EditorThemeFile`].
///
/// Each value is `#RRGGBB`, or `#RRGGBBAA` where the author wants alpha —
/// which, of the nineteen, only the two background bands and the selection
/// have much use for.
///
/// All nineteen are required. Unlike the chrome palette's grid slots there is
/// nothing here to derive a missing one *from*, and a file missing a token
/// class is more likely a truncated copy than a deliberate omission, so it is
/// better reported — [`crate::theme_store`] logs it and skips the file — than
/// silently half-applied.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EditorThemeColors {
    /// Background of the text area.
    pub background: String,
    /// Default text color.
    pub foreground: String,
    /// The caret.
    pub cursor: String,
    /// Background of the selected range.
    pub selection: String,
    /// Background of the line the caret is on.
    pub line_highlight: String,
    /// Line numbers in the gutter.
    pub gutter: String,
    /// The line number of the line the caret is on.
    pub gutter_active: String,
    /// Reserved words.
    pub keyword: String,
    /// String and date literals.
    pub string: String,
    /// Numeric literals.
    pub number: String,
    /// Line and block comments.
    pub comment: String,
    /// Called functions.
    pub function: String,
    /// Type names.
    pub r#type: String,
    /// Operators.
    pub operator: String,
    /// Table, column and alias names.
    pub identifier: String,
    /// Commas, semicolons, parentheses and dots.
    pub punctuation: String,
    /// The bracket under the caret and its partner.
    pub bracket_match: String,
    /// Marks on a statement the parser rejected.
    pub error: String,
    /// Marks on a statement that parses but looks wrong.
    pub warning: String,
}

impl EditorThemeFile {
    /// The file for a name, a darkness and a set of already-written colors.
    ///
    /// The counterpart of [`EditorThemeFile::from_theme`] for the theme editor,
    /// which holds each slot as the string the user typed rather than as a
    /// resolved color and has to write those strings back untouched.
    pub fn new(name: impl Into<String>, dark: bool, colors: EditorThemeColors) -> Self {
        Self {
            version: EDITOR_THEME_FILE_VERSION,
            name: name.into(),
            dark,
            colors,
        }
    }

    /// Turn the file into a palette the editor can use.
    ///
    /// A color that is not a `#RRGGBB` or `#RRGGBBAA` value keeps the fallback
    /// theme's color for that slot — and the fallback theme is chosen by the
    /// file's own `dark` key, [`EditorTheme::one_dark`] or
    /// [`EditorTheme::one_light`]. That choice matters more here than in the
    /// chrome palette: a syntax palette is nineteen colors on one page, so a
    /// single typo in a light theme borrowing from a dark one puts a
    /// near-invisible keyword in the middle of every statement. Borrowing from
    /// the same side of light/dark keeps a mistyped slot merely *wrong* rather
    /// than unreadable.
    pub fn to_theme(&self) -> EditorTheme {
        let fallback = if self.dark {
            EditorTheme::one_dark()
        } else {
            EditorTheme::one_light()
        };
        let color = |value: &str, fallback: Hsla| parse_hex(value).unwrap_or(fallback);

        EditorTheme {
            dark: self.dark,
            background: color(&self.colors.background, fallback.background),
            foreground: color(&self.colors.foreground, fallback.foreground),
            cursor: color(&self.colors.cursor, fallback.cursor),
            selection: color(&self.colors.selection, fallback.selection),
            line_highlight: color(&self.colors.line_highlight, fallback.line_highlight),
            gutter: color(&self.colors.gutter, fallback.gutter),
            gutter_active: color(&self.colors.gutter_active, fallback.gutter_active),
            keyword: color(&self.colors.keyword, fallback.keyword),
            string: color(&self.colors.string, fallback.string),
            number: color(&self.colors.number, fallback.number),
            comment: color(&self.colors.comment, fallback.comment),
            function: color(&self.colors.function, fallback.function),
            r#type: color(&self.colors.r#type, fallback.r#type),
            operator: color(&self.colors.operator, fallback.operator),
            identifier: color(&self.colors.identifier, fallback.identifier),
            punctuation: color(&self.colors.punctuation, fallback.punctuation),
            bracket_match: color(&self.colors.bracket_match, fallback.bracket_match),
            error: color(&self.colors.error, fallback.error),
            warning: color(&self.colors.warning, fallback.warning),
        }
    }

    /// The file that would reproduce `theme` under the name `name`.
    pub fn from_theme(name: impl Into<String>, theme: &EditorTheme) -> Self {
        Self {
            version: EDITOR_THEME_FILE_VERSION,
            name: name.into(),
            dark: theme.dark,
            colors: EditorThemeColors {
                background: to_hex(theme.background),
                foreground: to_hex(theme.foreground),
                cursor: to_hex(theme.cursor),
                selection: to_hex(theme.selection),
                line_highlight: to_hex(theme.line_highlight),
                gutter: to_hex(theme.gutter),
                gutter_active: to_hex(theme.gutter_active),
                keyword: to_hex(theme.keyword),
                string: to_hex(theme.string),
                number: to_hex(theme.number),
                comment: to_hex(theme.comment),
                function: to_hex(theme.function),
                r#type: to_hex(theme.r#type),
                operator: to_hex(theme.operator),
                identifier: to_hex(theme.identifier),
                punctuation: to_hex(theme.punctuation),
                bracket_match: to_hex(theme.bracket_match),
                error: to_hex(theme.error),
                warning: to_hex(theme.warning),
            },
        }
    }
}

/// Returns the active editor theme, falling back to [`EditorTheme::one_dark`]
/// when the app has not installed one yet.
///
/// A clone is returned rather than a borrow so that callers can keep using the
/// [`App`] mutably while styling their elements.
pub fn editor_theme(cx: &App) -> EditorTheme {
    cx.try_global::<EditorTheme>().cloned().unwrap_or_default()
}

/// Installs `theme` as the active [`EditorTheme`] global.
pub fn set_editor_theme(theme: EditorTheme, cx: &mut App) {
    cx.set_global(theme);
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::theme::contrast_ratio;

    /// Floor every token class has to clear against the page it is drawn on.
    ///
    /// Not WCAG's 4.5:1, and the gap is worth being explicit about. The
    /// palettes here are published ones reproduced faithfully, and not one of
    /// them meets 4.5:1 across all twelve classes — Solarized Light's keyword
    /// is 2.97:1 against `base3` and its author knew it, One Dark's comment is
    /// 2.32:1. Holding them to the body-text figure would mean shipping
    /// palettes under those names that are not those palettes, which is a worse
    /// outcome than a dim comment: a reader who picks "Solarized Light" is
    /// asking for the thing Schoonover published.
    ///
    /// So the bar here is a *floor* rather than a standard — enough to catch
    /// the failure that actually happens, a slot left at or near the background
    /// and so invisible — and the legibility that can be insisted on is
    /// insisted on separately, in [`MIN_BODY_CONTRAST`].
    const MIN_TOKEN_CONTRAST: f32 = 2.0;

    /// Contrast the body text has to reach against the page.
    ///
    /// Identifiers and unclassified text are most of what is on screen in a SQL
    /// buffer — table and column names outnumber keywords several to one — so
    /// this pair is held to something close to WCAG's body-text figure even
    /// where the accents are not. Solarized Light sets the bar: `base00` on
    /// `base3` is 4.13:1, and a palette dimmer than that is one nobody could
    /// read a long statement in.
    const MIN_BODY_CONTRAST: f32 = 4.0;

    /// Every token class of a palette, with the name to blame in a failure.
    fn tokens(theme: &EditorTheme) -> Vec<(&'static str, Hsla)> {
        vec![
            ("foreground", theme.foreground),
            ("keyword", theme.keyword),
            ("string", theme.string),
            ("number", theme.number),
            ("comment", theme.comment),
            ("function", theme.function),
            ("type", theme.r#type),
            ("operator", theme.operator),
            ("identifier", theme.identifier),
            ("punctuation", theme.punctuation),
            ("bracket_match", theme.bracket_match),
            ("error", theme.error),
            ("warning", theme.warning),
        ]
    }

    #[test]
    fn every_builtin_id_resolves_and_is_listed_once() {
        let mut ids: Vec<&str> = BUILTIN_EDITOR_THEMES.iter().map(|theme| theme.id).collect();
        assert_eq!(ids.len(), 6);
        ids.sort_unstable();
        let unique = ids.len();
        ids.dedup();
        assert_eq!(ids.len(), unique, "duplicate editor theme id");

        for theme in &BUILTIN_EDITOR_THEMES {
            assert!(EditorThemeRegistry::is_builtin(theme.id));
            assert_eq!((theme.build)().dark, theme.dark, "{}", theme.id);
        }
        assert!(!EditorThemeRegistry::is_builtin("nonsense"));
    }

    /// Both sides are covered, so that neither is the only one anybody looked
    /// at — which is exactly how a palette that is unreadable on the other side
    /// gets shipped. The split is lopsided because the chrome table's is: the
    /// six built-ins are the same six on both sides, and four of those six
    /// published palettes are dark ones.
    #[test]
    fn the_builtin_editor_themes_cover_both_sides() {
        let dark = BUILTIN_EDITOR_THEMES
            .iter()
            .filter(|theme| theme.dark)
            .count();
        assert_eq!(dark, 4);
        assert_eq!(BUILTIN_EDITOR_THEMES.len() - dark, 2);
    }

    #[test]
    fn every_builtin_theme_keeps_its_tokens_readable() {
        for builtin in &BUILTIN_EDITOR_THEMES {
            let theme = (builtin.build)();
            for (name, color) in tokens(&theme) {
                let ratio = contrast_ratio(color, theme.background);
                assert!(
                    ratio >= MIN_TOKEN_CONTRAST,
                    "{}: {name} at {ratio:.2}:1",
                    builtin.id
                );
            }
            for (name, color) in [
                ("foreground", theme.foreground),
                ("identifier", theme.identifier),
            ] {
                let ratio = contrast_ratio(color, theme.background);
                assert!(
                    ratio >= MIN_BODY_CONTRAST,
                    "{}: {name} at {ratio:.2}:1",
                    builtin.id
                );
            }
        }
    }

    /// Comments are meant to be the quietest thing on the page — that is the
    /// one hierarchy every syntax palette agrees on — and a palette that let
    /// them shout over the code would be wrong in a way no absolute contrast
    /// figure would catch.
    #[test]
    fn a_comment_is_the_dimmest_class_in_every_builtin_theme() {
        for builtin in &BUILTIN_EDITOR_THEMES {
            let theme = (builtin.build)();
            let comment = contrast_ratio(theme.comment, theme.background);
            for (name, color) in tokens(&theme) {
                if name == "comment" {
                    continue;
                }
                let ratio = contrast_ratio(color, theme.background);
                assert!(
                    ratio > comment,
                    "{}: {name} at {ratio:.2}:1 is no louder than the comment at {comment:.2}:1",
                    builtin.id
                );
            }
        }
    }

    /// The three bands behind the text have to be told apart from the page and
    /// from each other, or the current line and the selection stop showing.
    #[test]
    fn every_builtin_theme_separates_its_bands() {
        for builtin in &BUILTIN_EDITOR_THEMES {
            let theme = (builtin.build)();
            assert_ne!(theme.line_highlight, theme.background, "{}", builtin.id);
            assert_ne!(theme.selection, theme.background, "{}", builtin.id);
            assert_ne!(theme.selection, theme.line_highlight, "{}", builtin.id);
            // And the caret has to be visible against the page it sits on.
            assert!(
                contrast_ratio(theme.cursor, theme.background) >= 3.0,
                "{}: the caret is invisible",
                builtin.id
            );
        }
    }

    #[test]
    fn editor_theme_file_round_trips_through_json() {
        let theme = EditorTheme::dracula();
        let file = EditorThemeFile::from_theme("Dracula", &theme);
        let json = serde_json::to_string(&file).expect("serialize");
        // The Rust field is `r#type`; the key on disk has to be plain `type`.
        assert!(json.contains("\"type\""), "{json}");
        assert!(json.contains("\"bracket_match\""), "{json}");

        let parsed: EditorThemeFile = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(parsed, file);
        assert_eq!(parsed.version, 1);
        assert!(parsed.dark);
        assert_eq!(parsed.to_theme(), theme);
    }

    /// The format in the architecture document, byte for byte, has to load: it
    /// is what a user copying an example out of the docs will type.
    ///
    /// The example is deliberately a palette rudbman does *not* ship — folke's
    /// Tokyo Night — because that is the situation the file format exists for,
    /// and because a file named after a built-in would be skipped by the loader
    /// rather than loaded. So there is nothing to compare it against slot for
    /// slot; what is checked is that every key it spells is a key the format
    /// knows, which is what a missing or misspelled one would show up as.
    #[test]
    fn the_documented_format_parses() {
        let json = r##"{
            "version": 1,
            "name": "Tokyo Night",
            "dark": true,
            "colors": {
                "background": "#1a1b26",  "foreground": "#a9b1d6",
                "cursor": "#c0caf5",      "selection": "#33467c",
                "line_highlight": "#1f2335",
                "gutter": "#3b4261",      "gutter_active": "#737aa2",
                "keyword": "#bb9af7",     "string": "#9ece6a",
                "number": "#ff9e64",      "comment": "#565f89",
                "function": "#7aa2f7",    "type": "#2ac3de",
                "operator": "#89ddff",    "identifier": "#c0caf5",
                "punctuation": "#a9b1d6", "bracket_match": "#f7768e",
                "error": "#f7768e",       "warning": "#e0af68"
            }
        }"##;

        let file: EditorThemeFile = serde_json::from_str(json).expect("parse");
        assert_eq!(file.name, "Tokyo Night");
        assert_eq!(file.version, 1);
        assert!(file.dark);

        // Every slot landed where it was written rather than on a fallback,
        // which is what a key the document spells differently from the format
        // would look like. `type` and `bracket_match` are checked by name
        // because they are the two the Rust field names do not match.
        let theme = file.to_theme();
        assert_eq!(to_hex(theme.background), "#1a1b26");
        assert_eq!(to_hex(theme.r#type), "#2ac3de");
        assert_eq!(to_hex(theme.bracket_match), "#f7768e");
        assert_eq!(EditorThemeFile::from_theme("Tokyo Night", &theme), file);
    }

    #[test]
    fn an_unknown_key_is_ignored_and_a_missing_version_defaults() {
        let mut file = EditorThemeFile::from_theme("Mine", &EditorTheme::one_light());
        let mut value = serde_json::to_value(&file).expect("serialize");
        value
            .as_object_mut()
            .expect("object")
            .remove("version")
            .expect("version was written");
        value
            .as_object_mut()
            .expect("object")
            .insert("future_key".into(), serde_json::json!([1, 2, 3]));

        let parsed: EditorThemeFile = serde_json::from_value(value).expect("parse");
        assert_eq!(parsed.version, 1);
        file.version = 1;
        assert_eq!(parsed, file);
    }

    /// A missing token class fails the file rather than being half-applied;
    /// see [`EditorThemeColors`].
    #[test]
    fn a_missing_color_fails_the_file() {
        let file = EditorThemeFile::from_theme("Truncated", &EditorTheme::one_dark());
        let mut value = serde_json::to_value(&file).expect("serialize");
        value["colors"]
            .as_object_mut()
            .expect("object")
            .remove("operator")
            .expect("operator was written");

        assert!(serde_json::from_value::<EditorThemeFile>(value).is_err());
    }

    /// The reason [`EditorThemeFile::to_theme`] picks its fallback by darkness:
    /// the same typo in a light theme and a dark one has to stay on the right
    /// side of the page both times.
    #[test]
    fn an_unparseable_color_falls_back_within_the_same_darkness() {
        for (dark, expected) in [
            (true, EditorTheme::one_dark()),
            (false, EditorTheme::one_light()),
        ] {
            let mut file = EditorThemeFile::from_theme(
                "Typo",
                &if dark {
                    EditorTheme::gruvbox_dark()
                } else {
                    EditorTheme::solarized_light()
                },
            );
            file.colors.keyword = "rebeccapurple".into();

            let theme = file.to_theme();
            assert_eq!(theme.dark, dark);
            assert_eq!(theme.keyword, expected.keyword);
            // Only the broken slot moves; the rest of the file is honoured.
            assert_ne!(theme.background, expected.background);
            assert!(
                contrast_ratio(theme.keyword, theme.background) >= MIN_TOKEN_CONTRAST,
                "the borrowed keyword vanished on the page it landed on"
            );
        }
    }

    /// A file whose `dark` key disagrees with its background is taken at its
    /// word — the key is the palette's own claim, and the picker and the
    /// "follow the chrome" setting both read it rather than guessing.
    #[test]
    fn the_dark_key_is_carried_through_untouched() {
        let mut file = EditorThemeFile::from_theme("Mislabelled", &EditorTheme::one_dark());
        file.dark = false;
        assert!(!file.to_theme().dark);
    }
}
