//! Global application settings.
//!
//! Everything here is persisted to `settings.json` next to the connection
//! database. The file is meant to be hand-editable, so loading is deliberately
//! forgiving: missing keys fall back to the documented defaults, out-of-range
//! numbers are clamped rather than rejected (see [`AppSettings::sanitize`]),
//! and keys this build does not know are kept as they were found — a file
//! written by a newer rudbman opens here and is saved back without losing the
//! settings that newer build cares about. See [`AppSettings::extra`].

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::paths::{settings_file, strip_bom, write_atomic};

/// Lowest window opacity the UI accepts; below this the chrome is unreadable.
const MIN_BACKGROUND_OPACITY: f32 = 0.5;
/// Fully opaque window, and the default.
const MAX_BACKGROUND_OPACITY: f32 = 1.0;
/// Smallest legible font size.
const MIN_FONT_SIZE: f32 = 6.0;
/// Largest font size worth offering.
const MAX_FONT_SIZE: f32 = 32.0;
/// Font size used by the interface chrome when none is configured.
const DEFAULT_UI_FONT_SIZE: f32 = 14.0;
/// Font size used by the SQL editor and the result grid when none is
/// configured.
const DEFAULT_EDITOR_FONT_SIZE: f32 = 14.0;
/// UI chrome theme used when none is configured.
const DEFAULT_THEME: &str = "one-dark";
/// Editor theme used when none is configured.
const DEFAULT_EDITOR_THEME: &str = "one-dark";
/// Java heap ceiling in megabytes when none is configured. See the
/// architecture document, §4.1.
const DEFAULT_JVM_HEAP_MB: u32 = 1024;
/// Smallest heap a JVM plus one JDBC driver can be expected to start in.
const MIN_JVM_HEAP_MB: u32 = 128;
/// Rows fetched per result batch when none is configured. See §7.5.
const DEFAULT_FETCH_BATCH_ROWS: u32 = 500;

/// Width of the explorer sidebar on a first run, in logical pixels.
const DEFAULT_EXPLORER_WIDTH: f32 = 260.0;

/// Narrowest the explorer may be dragged.
///
/// A schema name has to survive: below this the tree is a column of ellipses,
/// which is worse than the panel being closed.
const MIN_EXPLORER_WIDTH: f32 = 140.0;

/// Widest the explorer may be dragged.
///
/// Not a taste judgement — a sidebar wider than this on a small display leaves
/// no work area at all, and the drag has no other floor to stop at.
const MAX_EXPLORER_WIDTH: f32 = 720.0;
/// Upper bound on the batch size: one batch crosses the JNI boundary as a
/// single buffer, so an unbounded value is an out-of-memory waiting to happen.
const MAX_FETCH_BATCH_ROWS: u32 = 100_000;
/// Window width used on a first run.
const DEFAULT_WINDOW_WIDTH: u32 = 1440;
/// Window height used on a first run.
const DEFAULT_WINDOW_HEIGHT: u32 = 900;
/// Smallest window the layout still fits in.
const MIN_WINDOW_WIDTH: u32 = 640;
/// Smallest window height the layout still fits in.
const MIN_WINDOW_HEIGHT: u32 = 400;

/// Clamp `value` into `min ..= max`, replacing NaN with `fallback`.
fn clamp_f32(value: f32, min: f32, max: f32, fallback: f32) -> f32 {
    if value.is_nan() {
        fallback
    } else {
        value.clamp(min, max)
    }
}

/// Clamp a font size into the supported range.
fn clamp_font_size(value: f32, fallback: f32) -> f32 {
    clamp_f32(value, MIN_FONT_SIZE, MAX_FONT_SIZE, fallback)
}

/// Fall back to `default` when a hand-edited string is blank.
fn non_blank(value: &str, default: &str) -> String {
    if value.trim().is_empty() {
        default.to_string()
    } else {
        value.to_string()
    }
}

/// Replace a blank optional string with [`None`].
///
/// A user who clears a text field leaves `""` behind, which means the same
/// thing as "not set" everywhere in this file.
fn blank_to_none(value: &mut Option<String>) {
    if let Some(text) = value
        && text.trim().is_empty()
    {
        *value = None;
    }
}

/// Who draws the window's title bar.
///
/// Read once, when the window is created: the platforms decide at that point
/// whether the window has a caption at all, so a change only shows after a
/// restart. The UI is expected to say so.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum TitlebarStyle {
    /// rudbman draws it: the toolbar doubles as the title bar. The default.
    #[default]
    Custom,
    /// The operating system draws its own caption above the app's chrome.
    System,
}

/// Where the window was, how big it was, and how it was painted.
///
/// Geometry is restored on the next start so the app comes back where the user
/// left it. `None` for a coordinate means "no saved position" — a first run, or
/// a window that was never moved — and the platform picks the placement.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct WindowState {
    /// Left edge in screen coordinates; `None` lets the platform decide.
    pub x: Option<i32>,
    /// Top edge in screen coordinates; `None` lets the platform decide.
    pub y: Option<i32>,
    /// Window width in logical pixels.
    pub width: u32,
    /// Window height in logical pixels.
    pub height: u32,
    /// Whether the window was maximized when it was last closed.
    ///
    /// Kept beside the geometry rather than instead of it: restoring a
    /// maximized window still needs a size to un-maximize back to.
    pub maximized: bool,
    /// 0.5 ..= 1.0; values below the floor are clamped on load.
    pub background_opacity: f32,
    /// Acrylic/blur behind the window when the platform supports it.
    pub background_blur: bool,
    /// Who draws the title bar. Only read when a window is created.
    pub titlebar: TitlebarStyle,
}

impl Default for WindowState {
    fn default() -> Self {
        Self {
            x: None,
            y: None,
            width: DEFAULT_WINDOW_WIDTH,
            height: DEFAULT_WINDOW_HEIGHT,
            maximized: false,
            background_opacity: MAX_BACKGROUND_OPACITY,
            background_blur: false,
            titlebar: TitlebarStyle::default(),
        }
    }
}

impl WindowState {
    /// Force every field back into its supported range.
    fn sanitize(&mut self) {
        self.width = self.width.max(MIN_WINDOW_WIDTH);
        self.height = self.height.max(MIN_WINDOW_HEIGHT);
        self.background_opacity = clamp_f32(
            self.background_opacity,
            MIN_BACKGROUND_OPACITY,
            MAX_BACKGROUND_OPACITY,
            MAX_BACKGROUND_OPACITY,
        );
    }
}

/// Everything rudbman persists in `settings.json`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct AppSettings {
    /// Schema version of the file; see [`AppSettings::CURRENT_VERSION`].
    pub version: u32,
    /// UI chrome theme id, e.g. `"one-dark"`.
    ///
    /// Resolution lives in the UI layer, which also knows the themes loaded
    /// from [`ui_themes_dir`](crate::paths::ui_themes_dir), so an id this build
    /// cannot resolve is still kept verbatim.
    pub theme: String,
    /// Editor theme id, resolved against
    /// [`editor_themes_dir`](crate::paths::editor_themes_dir).
    ///
    /// Ignored while [`AppSettings::editor_theme_follows_ui`] is on, but not
    /// forgotten: turning the switch off has to give the user their pick back.
    pub editor_theme: String,
    /// Pick the editor theme from the UI theme's light/dark cast instead of
    /// [`AppSettings::editor_theme`].
    ///
    /// On by default, because a dark editor inside a light window (or the other
    /// way round) is the state nobody chooses on purpose.
    pub editor_theme_follows_ui: bool,
    /// BCP 47 tag of the interface language, e.g. `"ko"` or `"zh-CN"`.
    ///
    /// `None` — the default — means "follow the operating system". The list of
    /// tags rudbman actually ships translations for lives in the app layer, so
    /// nothing here validates the string: an unknown tag is resolved the same
    /// way `None` is, by falling back to the system locale and then to English.
    pub language: Option<String>,
    /// Font size of the interface chrome; clamped to 6.0 ..= 32.0 on load.
    pub ui_font_size: f32,
    /// Monospace family for the SQL editor and the result grid.
    ///
    /// `None` = the per-OS monospace default chosen by the app layer.
    pub editor_font_family: Option<String>,
    /// Font size of the SQL editor and the result grid; clamped like
    /// [`AppSettings::ui_font_size`].
    pub editor_font_size: f32,
    /// Maximum Java heap in megabytes, passed to the JVM as `-Xmx`.
    ///
    /// The JVM is started once per process and its heap cannot be resized
    /// afterwards, so a change here only takes effect on the next start. See
    /// the architecture document, §4.1.
    pub jvm_heap_mb: u32,
    /// Extra JVM arguments, appended after the ones rudbman sets itself.
    ///
    /// An escape hatch for `-D` properties a driver needs and for debugging
    /// flags; not validated, since anything rejected is reported by the JVM at
    /// start-up where the user can see it.
    pub jvm_extra_args: Vec<String>,
    /// Rows fetched per result batch; clamped to 1 ..= 100 000 on load.
    ///
    /// The grid streams: a query returns the first batch and asks for more as
    /// the user scrolls. See the architecture document, §7.5.
    pub fetch_batch_rows: u32,
    /// Statement timeout in seconds; `0` means no timeout.
    pub query_timeout_s: u32,
    /// Value [`ConnectionProfile::confirm_writes`](crate::ConnectionProfile)
    /// starts at for a newly created profile.
    pub confirm_writes_default: bool,
    /// Width of the explorer sidebar in logical pixels; clamped to
    /// 140.0 ..= 720.0 on load.
    ///
    /// Top level rather than inside [`WindowState`]: that struct is what a live
    /// window writes back as it is moved and resized, and the sidebar is a
    /// preference the user set once. Mixing the two would have a window drag
    /// rewriting a panel width.
    pub explorer_width: f32,
    /// Whether the explorer sidebar is showing.
    ///
    /// On by default: a workbench whose object tree is hidden until it is found
    /// in a menu is a workbench that looks like it has none.
    pub explorer_visible: bool,
    /// Window geometry and background treatment.
    pub window: WindowState,
    /// Release tag the user asked never to be told about again, e.g. `"v0.2.0"`.
    ///
    /// Written by the start-up update check when the user picks "ignore this
    /// version", and compared against the latest tag verbatim: only that exact
    /// release is suppressed, so the next one announces itself normally. `None`
    /// — the default — means nothing has been ignored.
    ///
    /// Stored as the tag rather than as a parsed version because the tag is what
    /// GitHub answers with and what the comparison already has in hand; nothing
    /// here validates it, since an unrecognisable value can only ever fail to
    /// match a real tag, which is the harmless direction.
    pub ignored_update: Option<String>,
    /// Top-level keys this build does not know, kept verbatim.
    ///
    /// Ignoring an unknown key would be enough to *load* a file from a newer
    /// build, but the first save from this build would then delete it. Round
    /// tripping them costs one map and makes running two versions against one
    /// config directory — a beta beside a release, say — non-destructive.
    #[serde(flatten)]
    pub extra: BTreeMap<String, serde_json::Value>,
}

impl Default for AppSettings {
    fn default() -> Self {
        Self {
            version: Self::CURRENT_VERSION,
            theme: DEFAULT_THEME.to_string(),
            editor_theme: DEFAULT_EDITOR_THEME.to_string(),
            editor_theme_follows_ui: true,
            language: None,
            ui_font_size: DEFAULT_UI_FONT_SIZE,
            editor_font_family: None,
            editor_font_size: DEFAULT_EDITOR_FONT_SIZE,
            jvm_heap_mb: DEFAULT_JVM_HEAP_MB,
            jvm_extra_args: Vec::new(),
            fetch_batch_rows: DEFAULT_FETCH_BATCH_ROWS,
            query_timeout_s: 0,
            confirm_writes_default: true,
            explorer_width: DEFAULT_EXPLORER_WIDTH,
            explorer_visible: true,
            window: WindowState::default(),
            ignored_update: None,
            extra: BTreeMap::new(),
        }
    }
}

impl AppSettings {
    /// Schema version written by this build.
    ///
    /// A file carrying a different number still loads: unknown keys are kept
    /// and missing ones default, so the version is informational until a real
    /// migration is needed.
    pub const CURRENT_VERSION: u32 = 1;

    /// Load the settings from the default configuration file.
    ///
    /// A missing file yields [`AppSettings::default`], and the result is always
    /// passed through [`AppSettings::sanitize`].
    ///
    /// # Errors
    ///
    /// Fails when the configuration directory cannot be determined, the file
    /// cannot be read, or its contents are not valid JSON.
    pub fn load() -> Result<Self> {
        Self::load_from(&settings_file()?)
    }

    /// Load the settings from an explicit path.
    ///
    /// A missing file yields [`AppSettings::default`]. A leading UTF-8 byte
    /// order mark is tolerated, unknown keys are preserved, and every value is
    /// clamped by [`AppSettings::sanitize`] before being returned.
    ///
    /// # Errors
    ///
    /// Fails when the file cannot be read or does not contain valid JSON.
    pub fn load_from(path: &Path) -> Result<Self> {
        let data = match fs::read(path) {
            Ok(data) => data,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(Self::default()),
            Err(err) => {
                return Err(err).with_context(|| format!("failed to read {}", path.display()));
            }
        };
        let mut settings: Self = serde_json::from_slice(strip_bom(&data))
            .with_context(|| format!("failed to parse settings from {}", path.display()))?;
        settings.sanitize();
        Ok(settings)
    }

    /// Write the settings to the default configuration file.
    ///
    /// # Errors
    ///
    /// Fails when the configuration directory cannot be determined or created,
    /// or when the file cannot be written.
    pub fn save(&self) -> Result<()> {
        self.save_to(&settings_file()?)
    }

    /// Write the settings to an explicit path, creating parent directories.
    ///
    /// The write is atomic: the data lands in a temporary sibling file that is
    /// then renamed over `path`.
    ///
    /// # Errors
    ///
    /// Fails when the parent directory cannot be created or the file cannot be
    /// written.
    pub fn save_to(&self, path: &Path) -> Result<()> {
        let json = serde_json::to_vec_pretty(self).context("failed to serialize settings")?;
        write_atomic(path, &json)
    }

    /// Force every value into its supported range.
    ///
    /// Called on every load so a hand-edited `settings.json` cannot break the
    /// app: font sizes are clamped to 6.0 ..= 32.0 (NaN becomes the default),
    /// opacity to 0.5 ..= 1.0, the heap to at least 128 MB, the fetch batch to
    /// 1 ..= 100 000 rows, and blank strings fall back to their defaults. The
    /// UI should call it again after editing values.
    pub fn sanitize(&mut self) {
        self.theme = non_blank(&self.theme, DEFAULT_THEME);
        self.editor_theme = non_blank(&self.editor_theme, DEFAULT_EDITOR_THEME);
        blank_to_none(&mut self.language);
        self.ui_font_size = clamp_font_size(self.ui_font_size, DEFAULT_UI_FONT_SIZE);
        self.editor_font_size = clamp_font_size(self.editor_font_size, DEFAULT_EDITOR_FONT_SIZE);
        blank_to_none(&mut self.editor_font_family);
        self.jvm_heap_mb = self.jvm_heap_mb.max(MIN_JVM_HEAP_MB);
        self.explorer_width = clamp_f32(
            self.explorer_width,
            MIN_EXPLORER_WIDTH,
            MAX_EXPLORER_WIDTH,
            DEFAULT_EXPLORER_WIDTH,
        );
        // An empty or whitespace-only argument would reach the JVM as an empty
        // string, which it rejects with a message about the wrong argument.
        self.jvm_extra_args.retain(|arg| !arg.trim().is_empty());
        self.fetch_batch_rows = self.fetch_batch_rows.clamp(1, MAX_FETCH_BATCH_ROWS);
        // A blank tag would match no release and silence nothing, so it is the
        // same thing as having ignored none.
        blank_to_none(&mut self.ignored_update);
        self.window.sanitize();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_match_the_documented_values() {
        let settings = AppSettings::default();
        assert_eq!(settings.version, 1);
        assert_eq!(settings.theme, "one-dark");
        assert_eq!(settings.editor_theme, "one-dark");
        assert!(settings.editor_theme_follows_ui);
        assert_eq!(settings.language, None);
        assert_eq!(settings.ui_font_size, 14.0);
        assert_eq!(settings.editor_font_family, None);
        assert_eq!(settings.editor_font_size, 14.0);
        assert_eq!(settings.jvm_heap_mb, 1024);
        assert!(settings.jvm_extra_args.is_empty());
        assert_eq!(settings.fetch_batch_rows, 500);
        assert_eq!(settings.query_timeout_s, 0);
        assert!(settings.confirm_writes_default);
        assert_eq!(settings.window, WindowState::default());
        assert_eq!(settings.window.x, None);
        assert!(!settings.window.maximized);
        assert_eq!(settings.window.background_opacity, 1.0);
        assert_eq!(settings.window.titlebar, TitlebarStyle::Custom);
    }

    #[test]
    fn save_to_load_from_round_trip() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("cfg").join("settings.json");

        let settings = AppSettings {
            theme: "gruvbox-dark".to_string(),
            editor_theme: "solarized".to_string(),
            editor_theme_follows_ui: false,
            language: Some("ko".to_string()),
            ui_font_size: 15.0,
            editor_font_family: Some("Cascadia Mono".to_string()),
            editor_font_size: 16.5,
            jvm_heap_mb: 4096,
            jvm_extra_args: vec!["-Doracle.jdbc.timezoneAsRegion=false".to_string()],
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
        };

        settings.save_to(&path).expect("save");
        assert_eq!(AppSettings::load_from(&path).expect("load"), settings);

        // Saving over an existing file must work too.
        settings.save_to(&path).expect("overwrite");
        assert_eq!(AppSettings::load_from(&path).expect("reload"), settings);
    }

    #[test]
    fn load_from_missing_file_is_default() {
        let dir = tempfile::tempdir().expect("tempdir");
        let settings =
            AppSettings::load_from(&dir.path().join("absent.json")).expect("load missing");
        assert_eq!(settings, AppSettings::default());
    }

    #[test]
    fn empty_object_loads_as_default() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("settings.json");
        fs::write(&path, b"{}").expect("write");
        assert_eq!(
            AppSettings::load_from(&path).expect("load"),
            AppSettings::default()
        );
    }

    #[test]
    fn a_partial_file_fills_in_the_rest() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("settings.json");
        fs::write(
            &path,
            br#"{"theme":"my-theme","window":{"maximized":true}}"#,
        )
        .expect("write");

        let settings = AppSettings::load_from(&path).expect("load");
        assert_eq!(settings.theme, "my-theme");
        assert!(settings.window.maximized);
        // Unspecified keys of a partially specified section still default.
        assert_eq!(settings.window.width, DEFAULT_WINDOW_WIDTH);
        assert_eq!(settings.editor_font_size, 14.0);
        assert_eq!(settings.jvm_heap_mb, 1024);
    }

    #[test]
    fn a_font_size_typed_without_a_decimal_point_loads() {
        // Nobody hand-writes `14.0`, and the unknown-key map means these
        // numbers reach `f32` through serde's buffered path rather than
        // directly — worth pinning down.
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("settings.json");
        fs::write(&path, br#"{"ui_font_size": 16, "editor_font_size": 12}"#).expect("write");

        let settings = AppSettings::load_from(&path).expect("load");
        assert_eq!(settings.ui_font_size, 16.0);
        assert_eq!(settings.editor_font_size, 12.0);
    }

    #[test]
    fn load_from_tolerates_a_utf8_bom() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("settings.json");

        let mut with_bom = b"\xEF\xBB\xBF".to_vec();
        with_bom.extend_from_slice(br#"{"theme":"one-light"}"#);
        fs::write(&path, with_bom).expect("write");

        let settings = AppSettings::load_from(&path).expect("load");
        assert_eq!(settings.theme, "one-light");
        assert_eq!(settings.window, WindowState::default());
    }

    #[test]
    fn unknown_keys_survive_a_load_and_save() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("settings.json");
        fs::write(
            &path,
            br#"{
                "version": 99,
                "theme": "one-light",
                "from_the_future": {"anything": [1, 2, 3]}
            }"#,
        )
        .expect("write");

        let settings = AppSettings::load_from(&path).expect("load");
        assert_eq!(settings.version, 99);
        assert_eq!(settings.theme, "one-light");
        assert_eq!(
            settings.extra.get("from_the_future"),
            Some(&serde_json::json!({"anything": [1, 2, 3]}))
        );

        // The point of keeping them: a save from this build must not drop them.
        settings.save_to(&path).expect("save");
        let text = fs::read_to_string(&path).expect("read");
        assert!(text.contains("from_the_future"), "got {text}");
        assert_eq!(AppSettings::load_from(&path).expect("reload"), settings);
    }

    #[test]
    fn load_from_invalid_json_fails_without_touching_the_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("settings.json");
        fs::write(&path, b"{ nope").expect("write");

        let err = AppSettings::load_from(&path).expect_err("must be an error");
        assert!(
            err.to_string().contains("failed to parse settings"),
            "unhelpful error: {err:#}"
        );
        assert_eq!(fs::read_to_string(&path).unwrap(), "{ nope");
    }

    #[test]
    fn sanitize_clamps_font_sizes() {
        let mut settings = AppSettings {
            ui_font_size: 500.0,
            editor_font_size: 0.0,
            ..AppSettings::default()
        };
        settings.sanitize();
        assert_eq!(settings.ui_font_size, 32.0);
        assert_eq!(settings.editor_font_size, 6.0);

        settings.ui_font_size = f32::NAN;
        settings.editor_font_size = -20.0;
        settings.sanitize();
        assert_eq!(settings.ui_font_size, 14.0);
        assert_eq!(settings.editor_font_size, 6.0);
    }

    #[test]
    fn sanitize_clamps_background_opacity() {
        let mut settings = AppSettings {
            window: WindowState {
                background_opacity: 1.5,
                ..WindowState::default()
            },
            ..AppSettings::default()
        };
        settings.sanitize();
        assert_eq!(settings.window.background_opacity, 1.0);

        settings.window.background_opacity = 0.0;
        settings.sanitize();
        assert_eq!(settings.window.background_opacity, 0.5);

        settings.window.background_opacity = f32::NAN;
        settings.sanitize();
        assert_eq!(settings.window.background_opacity, 1.0);

        settings.window.background_opacity = 0.75;
        settings.sanitize();
        assert_eq!(settings.window.background_opacity, 0.75);
    }

    #[test]
    fn sanitize_bounds_the_jvm_heap_and_the_fetch_batch() {
        let mut settings = AppSettings {
            jvm_heap_mb: 0,
            fetch_batch_rows: 0,
            ..AppSettings::default()
        };
        settings.sanitize();
        assert_eq!(settings.jvm_heap_mb, 128, "a 0 MB heap cannot start a JVM");
        assert_eq!(settings.fetch_batch_rows, 1, "a batch must fetch something");

        settings.fetch_batch_rows = u32::MAX;
        settings.sanitize();
        assert_eq!(settings.fetch_batch_rows, 100_000);

        // A timeout of 0 is meaningful — it disables the timeout — and stays.
        settings.query_timeout_s = 0;
        settings.sanitize();
        assert_eq!(settings.query_timeout_s, 0);
    }

    #[test]
    fn sanitize_restores_blank_strings_and_drops_blank_arguments() {
        let mut settings = AppSettings {
            theme: "  ".to_string(),
            editor_theme: String::new(),
            language: Some("  ".to_string()),
            editor_font_family: Some(String::new()),
            jvm_extra_args: vec![String::new(), "-Xss2m".to_string(), "   ".to_string()],
            ..AppSettings::default()
        };
        settings.sanitize();

        assert_eq!(settings.theme, "one-dark");
        assert_eq!(settings.editor_theme, "one-dark");
        assert_eq!(settings.language, None);
        assert_eq!(settings.editor_font_family, None);
        assert_eq!(settings.jvm_extra_args, vec!["-Xss2m".to_string()]);
    }

    #[test]
    fn sanitize_keeps_ids_this_build_cannot_resolve() {
        // The UI layer owns the theme registry — the themes directories
        // included — and the app layer owns the list of shipped translations,
        // so core must not drop a value it happens not to know.
        let mut settings = AppSettings {
            theme: "my-theme".to_string(),
            editor_theme: "my-editor-theme".to_string(),
            language: Some("xx-YZ".to_string()),
            ..AppSettings::default()
        };
        settings.sanitize();
        assert_eq!(settings.theme, "my-theme");
        assert_eq!(settings.editor_theme, "my-editor-theme");
        assert_eq!(settings.language.as_deref(), Some("xx-YZ"));
    }

    #[test]
    fn sanitize_enforces_a_usable_window_size() {
        let mut settings = AppSettings::default();
        settings.window.width = 1;
        settings.window.height = 0;
        settings.sanitize();
        assert_eq!(settings.window.width, MIN_WINDOW_WIDTH);
        assert_eq!(settings.window.height, MIN_WINDOW_HEIGHT);
    }

    #[test]
    fn load_applies_sanitize() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("settings.json");
        fs::write(
            &path,
            br#"{
                "ui_font_size": 500.0,
                "jvm_heap_mb": 1,
                "fetch_batch_rows": 999999999,
                "window": {"background_opacity": 0.1}
            }"#,
        )
        .expect("write");

        let settings = AppSettings::load_from(&path).expect("load");
        assert_eq!(settings.ui_font_size, 32.0);
        assert_eq!(settings.jvm_heap_mb, 128);
        assert_eq!(settings.fetch_batch_rows, 100_000);
        assert_eq!(settings.window.background_opacity, 0.5);
    }

    #[test]
    fn titlebar_style_serializes_in_snake_case() {
        assert_eq!(
            serde_json::to_value(TitlebarStyle::System).unwrap(),
            serde_json::json!("system")
        );
        assert_eq!(
            serde_json::from_str::<TitlebarStyle>("\"custom\"").unwrap(),
            TitlebarStyle::Custom
        );
    }
}
