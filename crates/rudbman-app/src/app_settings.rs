//! The application-wide settings state.
//!
//! [`AppSettings`] loaded from disk lives in a gpui global so that every view
//! reads one consistent snapshot. The settings dialog replaces the global and
//! saves to disk when the user applies changes; everything else only reads.
//!
//! The window geometry is the one part that flows the other way: the shell
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
//! persisted settings rather than writing it into them is what makes cancelling
//! free: dropping the override is the revert, and a window closed mid-dialog
//! still saves the settings the user last committed to.

use std::sync::OnceLock;

use gpui::{App, Bounds, Global, Hsla, Pixels, Point, SharedString, Size, px};
use rudbman_core::{AppSettings, WindowState};

/// fontconfig's generic alias for a fixed-pitch face.
///
/// Only Linux resolves it. It is the last answer [`monospace_family`] gives,
/// and the only one it gives there.
const GENERIC_MONOSPACE: &str = "monospace";

/// Fixed-pitch families to look for on Windows, best first.
///
/// Cascadia Mono ships with Windows 11 and with the Terminal on 10; Cascadia
/// Code is the same face with programming ligatures and stands in when only the
/// Terminal's own install is present. Consolas has been in Windows since Vista
/// and Courier New since far earlier, so between them the list cannot come up
/// empty on a real machine.
#[cfg(target_os = "windows")]
const MONOSPACE_CANDIDATES: &[&str] =
    &["Cascadia Mono", "Cascadia Code", "Consolas", "Courier New"];

/// Fixed-pitch families to look for on macOS, best first.
///
/// SF Mono arrives with the Terminal and with Xcode and is what the system's
/// own developer tools draw code in; Menlo has shipped since 10.6 and Monaco
/// since long before that.
#[cfg(target_os = "macos")]
const MONOSPACE_CANDIDATES: &[&str] = &["SF Mono", "Menlo", "Monaco"];

/// No candidates anywhere else: see [`monospace_family`].
#[cfg(not(any(target_os = "windows", target_os = "macos")))]
const MONOSPACE_CANDIDATES: &[&str] = &[];

/// The family to draw fixed-pitch text with when no editor font is configured.
///
/// This is the app layer keeping the promise
/// [`rudbman_core::AppSettings::editor_font_family`] documents: a `None` there
/// means "the per-OS monospace default chosen by the app layer", and this is
/// that choice.
///
/// The naive answer — the literal `"monospace"` — is a *fontconfig* alias, so
/// it resolves to a real fixed-pitch face on Linux and nowhere else. Windows
/// DirectWrite has no such family: gpui logs `monospace not found` and falls
/// back to the system UI font, which is proportional, so SQL and `CREATE`
/// statements lose their columns. CoreText has no alias either. So on those two
/// platforms a family that actually exists has to be named, and the only way to
/// know which ones exist is to ask.
///
/// Off the two platforms that need it — Linux, and gpui's headless test
/// platform, whose font list is the fallback stack and nothing else — the
/// candidate list is empty or matches nothing and the alias is returned
/// unchanged, which is the behaviour that was there before.
///
/// Resolved once per process and cached: enumerating every installed family is
/// a platform call far too heavy for a render pass. A font installed while
/// rudbman is running is therefore not picked up until the next start, which is
/// a trade we make knowingly — the alternative is paying for the enumeration on
/// every frame that draws a line of SQL.
pub fn monospace_family(cx: &App) -> SharedString {
    static RESOLVED: OnceLock<SharedString> = OnceLock::new();
    RESOLVED
        .get_or_init(|| {
            pick(MONOSPACE_CANDIDATES, &cx.text_system().all_font_names())
                .map_or_else(|| SharedString::new_static(GENERIC_MONOSPACE), Into::into)
        })
        .clone()
}

/// The first of `candidates` that `installed` offers, spelled as `installed`
/// spells it.
///
/// Compared without ASCII case, and the *installed* spelling is what comes
/// back: the platforms report families in their own casing (and DirectWrite in
/// the system locale), and the name handed to the text system afterwards should
/// be one it has already said it has. Order is the candidate list's, not the
/// installed list's — this answers "the best face that is here", not "the first
/// face alphabetically".
fn pick(candidates: &[&str], installed: &[String]) -> Option<String> {
    candidates.iter().find_map(|candidate| {
        installed
            .iter()
            .find(|name| name.eq_ignore_ascii_case(candidate))
            .cloned()
    })
}

/// Global wrapper holding the current [`AppSettings`].
pub struct CurrentSettings(pub AppSettings);

impl Global for CurrentSettings {}

/// Global wrapper holding unsaved settings the window is drawn from.
///
/// Installed while the settings dialog previews an edit and removed again when
/// it closes; see [`set_preview`].
struct PreviewSettings(AppSettings);

impl Global for PreviewSettings {}

/// Where a window is and how big it is, as `settings.json` records it.
///
/// A type of its own rather than a [`WindowState`], because only these five
/// values follow a live window. The opacity, the blur and the title bar style
/// sitting beside them in the settings are the user's choices and are never
/// written back from the window they were applied to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WindowGeometry {
    /// Left edge in screen coordinates.
    pub x: i32,
    /// Top edge in screen coordinates.
    pub y: i32,
    /// Width in logical pixels.
    pub width: u32,
    /// Height in logical pixels.
    pub height: u32,
    /// Whether the window was maximized. The bounds above are then the size it
    /// un-maximizes back to, which is what gpui hands out and what has to be
    /// restored alongside the maximized state.
    pub maximized: bool,
}

impl WindowGeometry {
    /// Reads a window's placement, rounded to whole logical pixels.
    ///
    /// The settings file is hand-editable, and a fractional window position is
    /// noise in it; a compositor that reports halves would otherwise write
    /// `1439.5` into a file a user is expected to read.
    pub fn of(bounds: Bounds<Pixels>, maximized: bool) -> Self {
        let value = |pixels: Pixels| f32::from(pixels).round();
        Self {
            x: value(bounds.origin.x) as i32,
            y: value(bounds.origin.y) as i32,
            width: value(bounds.size.width).max(0.) as u32,
            height: value(bounds.size.height).max(0.) as u32,
            maximized,
        }
    }

    /// The saved placement of `state`, or `None` when it carries no position.
    ///
    /// `None` is a first run, or a window that was never moved: the platform
    /// picks the placement then, and the caller centres the saved *size* on the
    /// active display rather than guessing at coordinates.
    pub fn saved(state: &WindowState) -> Option<Self> {
        Some(Self {
            x: state.x?,
            y: state.y?,
            width: state.width,
            height: state.height,
            maximized: state.maximized,
        })
    }

    /// The placement as gpui bounds.
    pub fn bounds(&self) -> Bounds<Pixels> {
        Bounds {
            origin: Point {
                x: px(self.x as f32),
                y: px(self.y as f32),
            },
            size: Size {
                width: px(self.width as f32),
                height: px(self.height as f32),
            },
        }
    }

    /// Writes the placement into `state`, leaving its appearance alone.
    fn apply_to(&self, state: &mut WindowState) {
        state.x = Some(self.x);
        state.y = Some(self.y);
        state.width = self.width;
        state.height = self.height;
        state.maximized = self.maximized;
    }

    /// Whether `state` already records this placement.
    fn matches(&self, state: &WindowState) -> bool {
        state.x == Some(self.x)
            && state.y == Some(self.y)
            && state.width == self.width
            && state.height == self.height
            && state.maximized == self.maximized
    }
}

/// Install the settings global from disk. Call once at start-up.
///
/// A file that cannot be read falls back to defaults; the app must start
/// regardless of what is on disk.
pub fn init(cx: &mut App) {
    let settings = AppSettings::load().unwrap_or_else(|err| {
        log::warn!("starting with default settings: {err:#}");
        AppSettings::default()
    });
    cx.set_global(CurrentSettings(settings));
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
    if geometry.matches(&settings.0.window) {
        return;
    }
    geometry.apply_to(&mut cx.global_mut::<CurrentSettings>().0.window);
}

/// Writes the settings global to `settings.json`.
///
/// Reports rather than propagates: the callers are shutdown paths, where there
/// is no longer a window to show a failure in and nothing useful to do about one
/// either.
pub fn save(cx: &App) {
    if let Err(error) = current(cx).save() {
        log::warn!("could not save the settings: {error:#}");
    }
}

/// Applies the configured window opacity to a background fill.
///
/// Only a fill that covers the window edge to edge may use this, and **at most
/// one such fill may cover any given pixel**. The window surface starts out
/// fully transparent, so a single translucent fill lets the desktop (or the
/// acrylic blur behind the window) show through. A second one on top does not:
/// gpui's Windows renderer blends the alpha channel additively
/// (`SrcBlendAlpha = ONE, DestBlendAlpha = ONE`), so two fills of, say, 0.75 and
/// 0.62 saturate the surface alpha at 1.0 and the window goes opaque. That is
/// why the toolbar and the status bar paint their surface untinted and only the
/// body — one fill, edge to edge — goes through here.
///
/// Reads [`current`] rather than [`effective`], and so does not follow a
/// preview. The fill is only half of what makes a window translucent: the other
/// half is the platform surface being told to permit alpha, which happens in
/// [`gpui::Window::set_background_appearance`] and only when the settings are
/// saved. Tinting ahead of that would compose against an opaque surface and
/// merely darken the window, which is a worse answer than not previewing at all.
pub fn window_tint(color: Hsla, cx: &App) -> Hsla {
    let opacity = current(cx).window.background_opacity;
    if opacity < 1.0 {
        Hsla {
            a: opacity,
            ..color
        }
    } else {
        color
    }
}

#[cfg(test)]
mod tests {
    use gpui::size;
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

    #[test]
    fn a_placement_survives_the_trip_through_the_settings() {
        let mut state = WindowState {
            background_opacity: 0.8,
            background_blur: true,
            titlebar: TitlebarStyle::System,
            ..WindowState::default()
        };
        geometry().apply_to(&mut state);

        assert_eq!(WindowGeometry::saved(&state), Some(geometry()));
        // The appearance is the user's and must not have been touched.
        assert_eq!(state.background_opacity, 0.8);
        assert!(state.background_blur);
        assert_eq!(state.titlebar, TitlebarStyle::System);
    }

    #[test]
    fn a_placement_survives_the_trip_through_gpui_bounds() {
        let geometry = geometry();
        assert_eq!(
            WindowGeometry::of(geometry.bounds(), geometry.maximized),
            geometry
        );
    }

    #[test]
    fn a_fractional_placement_is_rounded_to_whole_pixels() {
        let bounds = Bounds {
            origin: Point {
                x: px(1439.5),
                y: px(-0.4),
            },
            size: size(px(1280.6), px(719.2)),
        };
        assert_eq!(
            WindowGeometry::of(bounds, false),
            WindowGeometry {
                x: 1440,
                y: 0,
                width: 1281,
                height: 719,
                maximized: false,
            }
        );
    }

    #[test]
    fn a_state_without_a_position_has_no_saved_placement() {
        // A first run: the size is known, the coordinates are not, and the
        // caller has to centre rather than place.
        let state = WindowState::default();
        assert_eq!(state.x, None);
        assert_eq!(WindowGeometry::saved(&state), None);

        let half_placed = WindowState {
            x: Some(10),
            ..WindowState::default()
        };
        assert_eq!(WindowGeometry::saved(&half_placed), None);
    }

    #[test]
    fn recording_the_same_placement_twice_changes_nothing() {
        // The guard `record_window_geometry` relies on, tested without an `App`:
        // a window that is repainting but has not moved must not dirty the
        // settings global.
        let mut state = WindowState::default();
        geometry().apply_to(&mut state);
        assert!(geometry().matches(&state));

        let moved = WindowGeometry {
            x: 121,
            ..geometry()
        };
        assert!(!moved.matches(&state));
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

            // Cancelling is the absence of the override, not a second copy.
            clear_preview(cx);
            assert_eq!(effective(cx).theme, "one-dark");
            assert_eq!(effective(cx).ui_font_size, 14.0);
            // Every path that closes the dialog ends here, so it has to be safe
            // to run twice.
            clear_preview(cx);
            assert_eq!(effective(cx).theme, "one-dark");
        });
    }

    /// The candidate list decides, not the installed list's order: a machine
    /// with both Consolas and Cascadia Mono has to get the better of the two.
    #[test]
    fn the_best_installed_monospace_family_wins() {
        let installed = vec![
            "Arial".to_string(),
            "Consolas".to_string(),
            "Cascadia Mono".to_string(),
        ];
        assert_eq!(
            pick(&["Cascadia Mono", "Consolas"], &installed),
            Some("Cascadia Mono".to_string())
        );
        // And the next one down when the first is missing.
        assert_eq!(
            pick(&["Cascadia Code", "Consolas"], &installed),
            Some("Consolas".to_string())
        );
    }

    /// Matched without case, returned in the platform's spelling — that name is
    /// what the text system is asked for afterwards.
    #[test]
    fn a_candidate_is_matched_without_case_and_answered_in_the_installed_spelling() {
        let installed = vec!["CONSOLAS".to_string()];
        assert_eq!(
            pick(&["Consolas"], &installed),
            Some("CONSOLAS".to_string())
        );
    }

    /// Nothing installed, or nothing worth having: the caller falls back to the
    /// fontconfig alias, which is what Linux and the headless test platform get
    /// and what the app drew with before there was a candidate list at all.
    #[test]
    fn nothing_to_pick_is_left_to_the_generic_alias() {
        assert_eq!(pick(&["Consolas"], &["Arial".to_string()]), None);
        assert_eq!(pick(&["Consolas"], &[]), None);
        // The empty candidate list every other platform carries.
        assert_eq!(pick(&[], &["Consolas".to_string()]), None);
    }

    /// The headless test platform offers no fixed-pitch family, so the helper
    /// has to hand back the alias — the string every render test was written
    /// against.
    #[gpui::test]
    fn the_test_platform_falls_back_to_the_generic_alias(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| {
            assert_eq!(monospace_family(cx), GENERIC_MONOSPACE);
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
