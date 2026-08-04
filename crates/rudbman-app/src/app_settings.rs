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

use gpui::{App, Bounds, Global, Hsla, Pixels, Point, Size, px};
use rudbman_core::{AppSettings, WindowState};

/// Global wrapper holding the current [`AppSettings`].
pub struct CurrentSettings(pub AppSettings);

impl Global for CurrentSettings {}

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
}
