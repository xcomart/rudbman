//! The application-wide settings state.
//!
//! [`AppSettings`] loaded from disk lives in a gpui global so that every view
//! reads one consistent snapshot. The settings dialog replaces the global and
//! saves to disk when the user applies changes; everything else only reads.
//!
//! The window geometry is the one part that flows the other way: the workspace
//! records where the window is as it is moved and resized (see
//! [`record_window_geometry`]), and [`save`] writes the result out once, when
//! the last window closes. Writing it as it changes would put a file write in
//! the middle of a resize drag — a lot of syscalls to record a number nobody
//! reads until the next start.
//!
//! # Two snapshots
//!
//! [`current`] is what is on disk (or will be, at the next save); [`effective`]
//! is what the window is drawn from. They differ only while the settings dialog
//! is showing unsaved edits — a palette being tried on, a font being compared —
//! which it publishes through [`set_preview`]. Keeping the preview *beside* the
//! persisted settings rather than writing it into them is what makes canceling
//! free: dropping the override is the revert, and a window closed mid-dialog
//! still saves the settings the user last committed to.
//!
//! # A file that could not be read
//!
//! The app starts on defaults when `settings.json` cannot be read, but it never
//! writes them: they are not the user's settings, and saving them at shutdown
//! would replace a file the user may still be able to repair with nothing they
//! chose. [`load_error`] says why, the settings dialog shows it, and every
//! write refuses until a restart reads the file again.
//!
//! # What is not here
//!
//! The globals and the two snapshots are rudbman's, because the settings type
//! is: three applications share the shell and each spells its settings
//! differently. What is *not* rudbman's — where a window was, which fixed-pitch
//! family to fall back on, and how a translucent window tints a fill — comes
//! from [`rugpui_shell::settings`] and is re-exported here so that the call sites
//! go on reading as one module.

use anyhow::Result;
use gpui::{App, Global, SharedString};
use rudbman_core::{AppSettings, WindowState};
pub use rugpui_shell::settings::{WindowGeometry, monospace_family, window_tint};

/// Global wrapper holding the current [`AppSettings`].
pub struct CurrentSettings(pub AppSettings);

impl Global for CurrentSettings {}

/// Why `settings.json` could not be read at start-up; see [`load_error`].
struct UnreadableSettings(SharedString);

impl Global for UnreadableSettings {}

/// Global wrapper holding unsaved settings the window is drawn from.
///
/// Installed while the settings dialog previews an edit and removed again when
/// it closes; see [`set_preview`].
struct PreviewSettings(AppSettings);

impl Global for PreviewSettings {}

/// Writes `geometry` into `state`, leaving its appearance alone.
///
/// The two halves of a [`WindowState`] answer to different owners: the
/// placement follows the live window and is written back from it, while the
/// opacity, the blur and the title bar style are the user's choices and must
/// survive being recorded over.
fn apply_to(geometry: WindowGeometry, state: &mut WindowState) {
    state.x = Some(geometry.x);
    state.y = Some(geometry.y);
    state.width = geometry.width;
    state.height = geometry.height;
    state.maximized = geometry.maximized;
}

/// Whether `state` already records `geometry`.
fn matches(geometry: WindowGeometry, state: &WindowState) -> bool {
    state.x == Some(geometry.x)
        && state.y == Some(geometry.y)
        && state.width == geometry.width
        && state.height == geometry.height
        && state.maximized == geometry.maximized
}

/// The placement `state` records, or `None` when it carries no position.
///
/// `None` is a first run, or a window that was never moved: the platform picks
/// the placement then, and the caller centers the saved *size* on the active
/// display rather than guessing at coordinates.
pub fn saved_geometry(state: &WindowState) -> Option<WindowGeometry> {
    WindowGeometry::saved(state.x, state.y, state.width, state.height, state.maximized)
}

/// Install the settings global from disk. Call once at start-up.
///
/// A file that cannot be read falls back to defaults; the app must start
/// regardless of what is on disk. Those defaults are never written over it:
/// see [`load_error`].
pub fn init(cx: &mut App) {
    install(AppSettings::load(), cx);
}

/// Installs what reading the settings file came to.
pub(crate) fn install(loaded: Result<AppSettings>, cx: &mut App) {
    match loaded {
        Ok(settings) => {
            cx.set_global(CurrentSettings(settings));
            if cx.has_global::<UnreadableSettings>() {
                cx.remove_global::<UnreadableSettings>();
            }
        }
        Err(err) => {
            log::warn!("starting with default settings: {err:#}");
            cx.set_global(CurrentSettings(AppSettings::default()));
            cx.set_global(UnreadableSettings(format!("{err:#}").into()));
        }
    }
}

/// Why `settings.json` could not be read at start-up, if it could not.
///
/// While this answers, the settings are defaults standing in for a file that
/// is still there, and nothing writes them: [`save`] skips the write and the
/// settings dialog refuses to apply, showing this instead.
pub fn load_error(cx: &App) -> Option<SharedString> {
    cx.try_global::<UnreadableSettings>()
        .map(|unreadable| unreadable.0.clone())
}

/// A snapshot of the current settings.
pub fn current(cx: &App) -> AppSettings {
    cx.try_global::<CurrentSettings>()
        .map(|g| g.0.clone())
        .unwrap_or_default()
}

/// Replace the settings global. The caller is responsible for persistence and
/// for re-applying the settings to open windows.
pub fn replace(settings: AppSettings, cx: &mut App) {
    cx.set_global(CurrentSettings(settings));
}

/// A snapshot of the settings the interface should currently be drawn from.
///
/// The preview, while the settings dialog is showing one, and otherwise
/// [`current`]. Everything that *renders* from the settings reads this;
/// everything that *persists* them reads [`current`].
pub fn effective(cx: &App) -> AppSettings {
    cx.try_global::<PreviewSettings>()
        .map(|preview| preview.0.clone())
        .unwrap_or_else(|| current(cx))
}

/// Show `settings` without saving them.
///
/// The settings dialog calls this on every edit that is visible before it is
/// committed. Nothing is written to disk and [`current`] is untouched, so
/// [`clear_preview`] is all it takes to put the window back.
pub fn set_preview(settings: AppSettings, cx: &mut App) {
    cx.set_global(PreviewSettings(settings));
}

/// Drop the preview, if there is one, so [`effective`] answers [`current`]
/// again.
///
/// Idempotent: the dialog closes by more paths than it opens by, and every one
/// of them ends here.
pub fn clear_preview(cx: &mut App) {
    if cx.has_global::<PreviewSettings>() {
        cx.remove_global::<PreviewSettings>();
    }
}

/// Records where the window is, without touching the disk.
///
/// Called from the shell's window-bounds observer, so it runs on every move and
/// every step of a resize drag. Nothing is written and no global is marked dirty
/// unless a value actually changed: the observer fires far more often than the
/// rounded geometry differs, and a dirty global would schedule a repaint of a
/// window that is already repainting itself.
pub fn record_window_geometry(geometry: WindowGeometry, cx: &mut App) {
    let Some(settings) = cx.try_global::<CurrentSettings>() else {
        return;
    };
    if matches(geometry, &settings.0.window) {
        return;
    }
    apply_to(geometry, &mut cx.global_mut::<CurrentSettings>().0.window);
}

/// Writes the settings global to `settings.json`.
///
/// Reports rather than propagates: the callers are shutdown paths, where there
/// is no longer a window to show a failure in and nothing useful to do about one
/// either. Writes nothing while [`load_error`] answers.
pub fn save(cx: &App) {
    if let Err(error) = write(cx, AppSettings::save) {
        log::warn!("could not save the settings: {error:#}");
    }
}

/// Hands the settings global to `save`, unless it stands in for a file that
/// could not be read.
fn write(cx: &App, save: impl FnOnce(&AppSettings) -> Result<()>) -> Result<()> {
    if let Some(error) = load_error(cx) {
        anyhow::bail!("settings.json could not be read at start-up and is left as it is: {error}");
    }
    save(&current(cx))
}

#[cfg(test)]
mod tests {
    use rudbman_core::TitlebarStyle;

    use super::*;

    /// A placement that is nothing like the defaults, so a value left behind by
    /// mistake shows up as itself.
    fn geometry() -> WindowGeometry {
        WindowGeometry {
            x: 120,
            y: 60,
            width: 1600,
            height: 1000,
            maximized: true,
        }
    }

    #[gpui::test]
    fn settings_that_could_not_be_read_are_not_saved_over(cx: &mut gpui::TestAppContext) {
        // Saving the defaults the app started on — at shutdown, with the
        // window's placement in them — would replace every setting the user
        // has with nothing they chose.
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("settings.json");
        let damaged = b"{ \"theme\": \"dracula\", ";
        std::fs::write(&path, damaged).unwrap();

        cx.update(|cx| {
            install(AppSettings::load_from(&path), cx);
            assert!(load_error(cx).is_some());
            record_window_geometry(geometry(), cx);
            assert!(write(cx, |settings| settings.save_to(&path)).is_err());
        });
        assert_eq!(std::fs::read(&path).unwrap(), damaged);

        // A file that reads — here, none at all — is written as it always was.
        std::fs::remove_file(&path).unwrap();
        cx.update(|cx| {
            install(AppSettings::load_from(&path), cx);
            assert!(load_error(cx).is_none());
            write(cx, |settings| settings.save_to(&path)).expect("the file is written");
        });
        assert!(path.exists());
    }

    #[test]
    fn a_placement_survives_the_trip_through_the_settings() {
        let mut state = WindowState {
            background_opacity: 0.8,
            background_blur: true,
            titlebar: TitlebarStyle::System,
            ..WindowState::default()
        };
        apply_to(geometry(), &mut state);

        assert_eq!(saved_geometry(&state), Some(geometry()));
        // The appearance is the user's and must not have been touched.
        assert_eq!(state.background_opacity, 0.8);
        assert!(state.background_blur);
        assert_eq!(state.titlebar, TitlebarStyle::System);
    }

    #[test]
    fn a_state_without_a_position_has_no_saved_placement() {
        // A first run: the size is known, the coordinates are not, and the
        // caller has to center rather than place.
        let state = WindowState::default();
        assert_eq!(state.x, None);
        assert_eq!(saved_geometry(&state), None);

        let half_placed = WindowState {
            x: Some(10),
            ..WindowState::default()
        };
        assert_eq!(saved_geometry(&half_placed), None);
    }

    #[test]
    fn recording_the_same_placement_twice_changes_nothing() {
        // The guard `record_window_geometry` relies on, tested without an `App`:
        // a window that is repainting but has not moved must not dirty the
        // settings global.
        let mut state = WindowState::default();
        apply_to(geometry(), &mut state);
        assert!(matches(geometry(), &state));

        let moved = WindowGeometry {
            x: 121,
            ..geometry()
        };
        assert!(!matches(moved, &state));
    }

    /// The whole of the settings dialog's live preview, and its undo: an
    /// override that hides the saved settings from everything that draws, and
    /// nothing at all from what saves.
    #[gpui::test]
    fn a_preview_hides_the_saved_settings_until_it_is_dropped(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| {
            cx.set_global(CurrentSettings(AppSettings::default()));
            assert_eq!(effective(cx).theme, current(cx).theme);

            let previewed = AppSettings {
                theme: "dracula".to_string(),
                ui_font_size: 20.0,
                ..current(cx)
            };
            set_preview(previewed, cx);
            assert_eq!(effective(cx).theme, "dracula");
            assert_eq!(effective(cx).ui_font_size, 20.0);
            // And nothing of it reached what would be written to disk.
            assert_eq!(current(cx).theme, "one-dark");
            assert_eq!(current(cx).ui_font_size, 14.0);

            // Canceling is the absence of the override, not a second copy.
            clear_preview(cx);
            assert_eq!(effective(cx).theme, "one-dark");
            assert_eq!(effective(cx).ui_font_size, 14.0);
            // Every path that closes the dialog ends here, so it has to be safe
            // to run twice.
            clear_preview(cx);
            assert_eq!(effective(cx).theme, "one-dark");
        });
    }

    /// A preview must not survive the settings being replaced under it either:
    /// saving replaces the global and the dialog drops the override, and the
    /// two together have to leave one answer.
    #[gpui::test]
    fn saving_and_dropping_the_preview_agree(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| {
            cx.set_global(CurrentSettings(AppSettings::default()));
            let edited = AppSettings {
                theme: "gruvbox-dark".to_string(),
                ..current(cx)
            };
            set_preview(edited.clone(), cx);
            replace(edited, cx);
            clear_preview(cx);
            assert_eq!(effective(cx).theme, "gruvbox-dark");
            assert_eq!(current(cx).theme, "gruvbox-dark");
        });
    }
}
