//! Color palette used by every widget in this crate.
//!
//! The theme is stored as a gpui [`Global`], so any widget that has access to an
//! [`App`] reference can read it without threading it through its constructor.
//!
//! This is the *chrome* palette — panels, tabs, buttons, the result grid. The
//! SQL editor has a palette of its own, with its own file format and its own
//! directory; see [`crate::editor_theme`].
//!
//! # Themes and their ids
//!
//! Six themes ship with rudbman. A theme can also come from a file:
//! [`ThemeFile`] is the on-disk form, [`crate::theme_store`] reads the files,
//! and [`ThemeRegistry`] is where the two kinds are listed and resolved
//! together.
//!
//! # Stored colors and derived ones
//!
//! Not every slot of a [`Theme`] is written down. A palette spells out the
//! colors an author chooses — that is [`Palette`], the one gate every theme is
//! built through — and the slots that have to *hold* for any palette whatever
//! are worked out from those. [`Theme::icon`] is the first of them: it is the
//! muted text bent until it clears [`MIN_ICON_CONTRAST`] on both backgrounds,
//! so that a theme nobody checked still draws icons that can be seen.
//!
//! The five grid slots are the second kind of derived color, and they are
//! derived for a different reason: they were added after the format was
//! published. Every theme file written against the eleven-slot format — the
//! hand-written ones, and the ones carried over from logman, whose theme files
//! rudbman reads unchanged — has to keep loading, so the grid slots are
//! optional on the way in and worked out from the eleven when they are absent.
//! Unlike [`Theme::icon`] they *can* be spelled out: a grid header is a design
//! choice, not merely a legibility floor, and the six built-in themes all make
//! that choice by hand.

use gpui::{App, Global, Hsla, Rgba, hsla};
use serde::{Deserialize, Serialize};

/// A flat set of semantic colors.
///
/// Widgets never hardcode colors; they always resolve them through a `Theme` so
/// that swapping [`Theme::dark`] for [`Theme::light`] restyles the whole app.
#[derive(Debug, Clone)]
pub struct Theme {
    /// Whether this is a dark palette.
    ///
    /// Nothing in the widget layer branches on it — every widget reads the
    /// colors below instead — but the platforms that draw their own window
    /// caption need to be told which side of light/dark the app is on, and the
    /// palette is the only thing that knows.
    pub dark: bool,
    /// Window / app background.
    pub background: Hsla,
    /// Background of raised chrome such as panels, toolbars and the tab bar.
    pub surface: Hsla,
    /// Surface color while the pointer hovers an interactive element.
    pub surface_hover: Hsla,
    /// Surface color while an interactive element is pressed or selected.
    pub surface_active: Hsla,
    /// Hairline separators and control outlines.
    pub border: Hsla,
    /// Primary foreground color.
    pub text: Hsla,
    /// Secondary foreground color for hints, placeholders and inactive labels.
    pub text_muted: Hsla,
    /// Resting foreground color of an *icon*, as opposed to muted text.
    ///
    /// Icons used to be painted in [`Theme::text_muted`], which is the right
    /// hierarchy for a hint or an inactive label but the wrong one for a mark:
    /// a glyph is a solid run of pixels, while an icon is a hairline — the
    /// caption buttons draw a 1.1 px stroke — and a stroke that thin never
    /// reaches full coverage once it has been antialiased, so the same color
    /// arrives on screen weaker than the text beside it. WCAG asks 3:1 of a
    /// graphical control for that reason, and several of the built-in dark
    /// palettes did not even reach that with their muted text.
    ///
    /// So this slot is *derived* rather than stored: [`Palette`] runs
    /// [`readable_icon`] over the theme's own muted text, keeping its hue and
    /// saturation and moving only its lightness away from the surfaces until it
    /// clears [`MIN_ICON_CONTRAST`] against both [`Theme::background`] and
    /// [`Theme::surface`]. Deriving it is what lets a theme a user wrote by
    /// hand — [`ThemeFile`] carries no `icon` key, and gains none — come out
    /// legible without the user having thought about contrast at all.
    pub icon: Hsla,
    /// Brand color used for the active tab, focus rings and primary buttons.
    pub accent: Hsla,
    /// Destructive actions and error states.
    pub danger: Hsla,
    /// Successful / connected states.
    pub success: Hsla,
    /// Translucent backdrop painted behind modal dialogs (includes alpha).
    pub overlay: Hsla,
    /// Background of the result grid's column header row.
    ///
    /// A band raised off [`Theme::surface`] rather than off the page, because
    /// the header is chrome that happens to sit inside the data: it scrolls
    /// horizontally with the columns but stays put vertically, and reads as
    /// part of the toolbar above it.
    pub grid_header: Hsla,
    /// Background of every other body row.
    ///
    /// Zebra striping is a *hint*, not a division — a stripe strong enough to
    /// notice on its own is strong enough to fight the selection — so the
    /// derived value moves only a few percent off [`Theme::background`].
    pub grid_row_alt: Hsla,
    /// Fill painted over the selected cells or rows (includes alpha).
    ///
    /// Translucent by design: a grid selection covers text, and an opaque fill
    /// would have to be paired with a foreground of its own to stay readable.
    /// Letting the row show through means the text keeps the color it already
    /// had, whichever row it lands on.
    pub grid_selection: Hsla,
    /// Foreground of the `NULL` marker drawn in an empty cell.
    ///
    /// A cell holding no value and a cell holding the *string* `NULL` have to
    /// be told apart at a glance, which is what this slot is for; it is dimmer
    /// than [`Theme::text`] but still held to [`MIN_GRID_TEXT_CONTRAST`], since
    /// "there is no value here" is information and not decoration.
    pub grid_null: Hsla,
    /// Foreground marking a primary-key column.
    ///
    /// Drawn on the header band and on the body rows alike — the key icon in
    /// the header, the values under it — so it is judged against both.
    pub grid_pk: Hsla,
}

impl Theme {
    /// The default dark theme, in the spirit of One Dark.
    pub fn dark() -> Self {
        Palette {
            dark: true,
            background: hsla(220. / 360., 0.13, 0.18, 1.0),
            surface: hsla(220. / 360., 0.13, 0.14, 1.0),
            surface_hover: hsla(220. / 360., 0.13, 0.23, 1.0),
            surface_active: hsla(220. / 360., 0.13, 0.28, 1.0),
            border: hsla(220. / 360., 0.13, 0.31, 1.0),
            text: hsla(219. / 360., 0.14, 0.78, 1.0),
            text_muted: hsla(220. / 360., 0.09, 0.55, 1.0),
            accent: hsla(207. / 360., 0.82, 0.66, 1.0),
            danger: hsla(355. / 360., 0.65, 0.65, 1.0),
            success: hsla(95. / 360., 0.38, 0.62, 1.0),
            overlay: hsla(220. / 360., 0.13, 0.06, 0.62),
            // The grid rides One Dark's own ramp: the header is `#333842`, one
            // step above the chrome, and the primary-key mark is the palette's
            // gold `#e5c07b`, which One Dark itself uses for class names.
            grid_header: Some(hsla(220. / 360., 0.13, 0.235, 1.0)),
            grid_row_alt: Some(hsla(220. / 360., 0.13, 0.215, 1.0)),
            grid_selection: Some(hsla(207. / 360., 0.82, 0.66, 0.28)),
            grid_null: Some(hsla(220. / 360., 0.10, 0.55, 1.0)),
            grid_pk: Some(hsla(39. / 360., 0.67, 0.69, 1.0)),
        }
        .into()
    }

    /// A light counterpart to [`Theme::dark`].
    pub fn light() -> Self {
        Palette {
            dark: false,
            background: hsla(0., 0.0, 1.0, 1.0),
            surface: hsla(220. / 360., 0.16, 0.96, 1.0),
            surface_hover: hsla(220. / 360., 0.16, 0.91, 1.0),
            surface_active: hsla(220. / 360., 0.16, 0.86, 1.0),
            border: hsla(220. / 360., 0.13, 0.80, 1.0),
            text: hsla(220. / 360., 0.16, 0.20, 1.0),
            text_muted: hsla(220. / 360., 0.10, 0.45, 1.0),
            accent: hsla(212. / 360., 0.76, 0.46, 1.0),
            danger: hsla(355. / 360., 0.66, 0.46, 1.0),
            success: hsla(120. / 360., 0.45, 0.33, 1.0),
            overlay: hsla(220. / 360., 0.13, 0.35, 0.40),
            // The light counterpart reads the same ramp downwards: the header
            // is a shade below the chrome surface and the stripe barely leaves
            // white. The key mark is One Light's amber `#c18401`, darkened
            // enough to hold its own on a white page.
            grid_header: Some(hsla(220. / 360., 0.16, 0.92, 1.0)),
            grid_row_alt: Some(hsla(220. / 360., 0.16, 0.975, 1.0)),
            grid_selection: Some(hsla(212. / 360., 0.76, 0.46, 0.22)),
            grid_null: Some(hsla(220. / 360., 0.10, 0.48, 1.0)),
            grid_pk: Some(hsla(39. / 360., 0.99, 0.33, 1.0)),
        }
        .into()
    }

    /// Chrome for Solarized Dark.
    ///
    /// The surfaces walk down Ethan Schoonover's base ramp from `base03`
    /// (`#002b36`, the terminal background) and the text back up it to `base0`
    /// and `base01`; the three status colors are the palette's own blue, red
    /// and green.
    pub fn solarized_dark() -> Self {
        Palette {
            dark: true,
            background: hsla(192. / 360., 1.00, 0.11, 1.0),
            surface: hsla(192. / 360., 1.00, 0.085, 1.0),
            surface_hover: hsla(192. / 360., 0.81, 0.16, 1.0),
            surface_active: hsla(192. / 360., 0.62, 0.21, 1.0),
            border: hsla(194. / 360., 0.25, 0.28, 1.0),
            text: hsla(186. / 360., 0.08, 0.55, 1.0),
            text_muted: hsla(194. / 360., 0.14, 0.40, 1.0),
            accent: hsla(205. / 360., 0.69, 0.49, 1.0),
            danger: hsla(1. / 360., 0.71, 0.52, 1.0),
            success: hsla(68. / 360., 1.00, 0.30, 1.0),
            overlay: hsla(192. / 360., 1.00, 0.04, 0.62),
            // Solarized's own furniture: the header is `base02` (`#073642`)
            // lifted a little so it clears `base03`, the `NULL` marker sits
            // between `base01` and `base1` — the two Schoonover reserves for
            // de-emphasised content, the lower of which does not clear
            // [`MIN_GRID_TEXT_CONTRAST`] on the stripe — and the key mark is
            // the palette's yellow.
            grid_header: Some(hsla(192. / 360., 0.65, 0.19, 1.0)),
            grid_row_alt: Some(hsla(192. / 360., 0.90, 0.145, 1.0)),
            grid_selection: Some(hsla(205. / 360., 0.69, 0.49, 0.30)),
            grid_null: Some(hsla(194. / 360., 0.14, 0.50, 1.0)),
            grid_pk: Some(hsla(45. / 360., 1.00, 0.38, 1.0)),
        }
        .into()
    }

    /// Chrome for Solarized Light.
    ///
    /// The same palette as [`Theme::solarized_dark`] read from the other end:
    /// the surfaces run down from `base3` (`#fdf6e3`) towards `base2`, and the
    /// text is `base01` over `base0`, which is the contrast pairing Solarized
    /// itself prescribes for a light background.
    pub fn solarized_light() -> Self {
        Palette {
            dark: false,
            background: hsla(44. / 360., 0.87, 0.94, 1.0),
            surface: hsla(46. / 360., 0.42, 0.88, 1.0),
            surface_hover: hsla(46. / 360., 0.35, 0.84, 1.0),
            surface_active: hsla(46. / 360., 0.28, 0.79, 1.0),
            border: hsla(46. / 360., 0.20, 0.72, 1.0),
            text: hsla(194. / 360., 0.14, 0.34, 1.0),
            text_muted: hsla(194. / 360., 0.11, 0.48, 1.0),
            accent: hsla(205. / 360., 0.69, 0.42, 1.0),
            danger: hsla(1. / 360., 0.71, 0.45, 1.0),
            success: hsla(68. / 360., 1.00, 0.26, 1.0),
            overlay: hsla(44. / 360., 0.30, 0.35, 0.40),
            // The same furniture read from the light end: `base2` for the
            // header, `base01` again for the `NULL` marker, and the yellow
            // taken down the ramp until it clears `base3`.
            grid_header: Some(hsla(46. / 360., 0.42, 0.85, 1.0)),
            grid_row_alt: Some(hsla(46. / 360., 0.60, 0.915, 1.0)),
            grid_selection: Some(hsla(205. / 360., 0.69, 0.42, 0.22)),
            grid_null: Some(hsla(194. / 360., 0.14, 0.40, 1.0)),
            grid_pk: Some(hsla(45. / 360., 1.00, 0.29, 1.0)),
        }
        .into()
    }

    /// Chrome for Gruvbox Dark.
    ///
    /// The surfaces are morhetz's `dark0` … `dark3` ramp, warm and barely
    /// saturated; the text is `light1` over `gray`, and the accents are the
    /// bright blue, red and green of the ANSI palette.
    pub fn gruvbox_dark() -> Self {
        Palette {
            dark: true,
            background: hsla(20. / 360., 0.03, 0.157, 1.0),
            surface: hsla(20. / 360., 0.03, 0.12, 1.0),
            surface_hover: hsla(20. / 360., 0.05, 0.224, 1.0),
            surface_active: hsla(22. / 360., 0.07, 0.29, 1.0),
            border: hsla(27. / 360., 0.10, 0.365, 1.0),
            text: hsla(43. / 360., 0.59, 0.81, 1.0),
            text_muted: hsla(30. / 360., 0.12, 0.514, 1.0),
            accent: hsla(157. / 360., 0.16, 0.58, 1.0),
            danger: hsla(6. / 360., 0.96, 0.59, 1.0),
            success: hsla(61. / 360., 0.66, 0.44, 1.0),
            overlay: hsla(20. / 360., 0.05, 0.06, 0.62),
            // `dark1` for the header and `gray` for the `NULL` marker, both
            // straight out of morhetz's palette; the key mark is the bright
            // yellow `#fabd2f`.
            grid_header: Some(hsla(20. / 360., 0.05, 0.225, 1.0)),
            grid_row_alt: Some(hsla(20. / 360., 0.04, 0.19, 1.0)),
            grid_selection: Some(hsla(157. / 360., 0.16, 0.58, 0.28)),
            grid_null: Some(hsla(30. / 360., 0.12, 0.55, 1.0)),
            grid_pk: Some(hsla(42. / 360., 0.95, 0.58, 1.0)),
        }
        .into()
    }

    /// Chrome for Dracula.
    ///
    /// Background and hover surface are the scheme's own `Background` and
    /// `Current Line`, the muted text is `Comment`, and the accent is the
    /// `Purple` that Dracula puts in the ANSI blue slot.
    pub fn dracula() -> Self {
        Palette {
            dark: true,
            background: hsla(231. / 360., 0.15, 0.184, 1.0),
            surface: hsla(231. / 360., 0.15, 0.14, 1.0),
            surface_hover: hsla(232. / 360., 0.14, 0.31, 1.0),
            surface_active: hsla(232. / 360., 0.15, 0.37, 1.0),
            border: hsla(226. / 360., 0.20, 0.42, 1.0),
            text: hsla(60. / 360., 0.30, 0.96, 1.0),
            text_muted: hsla(225. / 360., 0.27, 0.51, 1.0),
            accent: hsla(265. / 360., 0.89, 0.78, 1.0),
            danger: hsla(0., 1.00, 0.667, 1.0),
            success: hsla(135. / 360., 0.94, 0.65, 1.0),
            overlay: hsla(231. / 360., 0.15, 0.07, 0.62),
            // The header is the scheme's `Current Line` pulled a shade back
            // towards the background, the `NULL` marker is `Comment` lifted
            // until it clears it, and the key mark is Dracula's `Yellow`.
            grid_header: Some(hsla(232. / 360., 0.14, 0.26, 1.0)),
            grid_row_alt: Some(hsla(231. / 360., 0.15, 0.215, 1.0)),
            grid_selection: Some(hsla(265. / 360., 0.89, 0.78, 0.28)),
            grid_null: Some(hsla(225. / 360., 0.27, 0.58, 1.0)),
            grid_pk: Some(hsla(65. / 360., 0.92, 0.76, 1.0)),
        }
        .into()
    }
}

/// The colors a theme *spells out*, before the derived ones are worked out.
///
/// Every `Theme` in the application is built from one of these — the six
/// built-in palettes above and [`ThemeFile::to_theme`] all end in
/// `Palette { … }.into()` — which is the point of the type: a slot that has to
/// hold for *any* palette can then be derived in the single [`From`] impl below
/// instead of being spelled out once per theme and forgotten by the seventh. It
/// is deliberately not public: a palette written outside this module could not
/// be held to that promise.
///
/// The eleven required slots are the ones no theme can do without.
/// [`Theme::icon`] has no field at all, since nothing may spell it out, and the
/// five grid slots are [`Option`]s: a palette that has an opinion states it, a
/// palette that has none — every theme file written before the grid existed —
/// gets one worked out.
struct Palette {
    /// See [`Theme::dark`](Theme#structfield.dark).
    dark: bool,
    /// See [`Theme::background`](Theme#structfield.background).
    background: Hsla,
    /// See [`Theme::surface`](Theme#structfield.surface).
    surface: Hsla,
    /// See [`Theme::surface_hover`](Theme#structfield.surface_hover).
    surface_hover: Hsla,
    /// See [`Theme::surface_active`](Theme#structfield.surface_active).
    surface_active: Hsla,
    /// See [`Theme::border`](Theme#structfield.border).
    border: Hsla,
    /// See [`Theme::text`](Theme#structfield.text).
    text: Hsla,
    /// See [`Theme::text_muted`](Theme#structfield.text_muted).
    text_muted: Hsla,
    /// See [`Theme::accent`](Theme#structfield.accent).
    accent: Hsla,
    /// See [`Theme::danger`](Theme#structfield.danger).
    danger: Hsla,
    /// See [`Theme::success`](Theme#structfield.success).
    success: Hsla,
    /// See [`Theme::overlay`](Theme#structfield.overlay).
    overlay: Hsla,
    /// See [`Theme::grid_header`](Theme#structfield.grid_header).
    grid_header: Option<Hsla>,
    /// See [`Theme::grid_row_alt`](Theme#structfield.grid_row_alt).
    grid_row_alt: Option<Hsla>,
    /// See [`Theme::grid_selection`](Theme#structfield.grid_selection).
    grid_selection: Option<Hsla>,
    /// See [`Theme::grid_null`](Theme#structfield.grid_null).
    grid_null: Option<Hsla>,
    /// See [`Theme::grid_pk`](Theme#structfield.grid_pk).
    grid_pk: Option<Hsla>,
}

impl From<Palette> for Theme {
    fn from(palette: Palette) -> Self {
        // The two grid backgrounds are settled first: the two grid foregrounds
        // are judged against them, so a theme that spells out an unusually
        // dark stripe still gets a `NULL` marker that can be read on it.
        let grid_header = palette
            .grid_header
            .unwrap_or_else(|| away_from_page(palette.surface, palette.dark, HEADER_STEP));
        let grid_row_alt = palette
            .grid_row_alt
            .unwrap_or_else(|| away_from_page(palette.background, palette.dark, STRIPE_STEP));

        Self {
            dark: palette.dark,
            background: palette.background,
            surface: palette.surface,
            surface_hover: palette.surface_hover,
            surface_active: palette.surface_active,
            border: palette.border,
            text: palette.text,
            text_muted: palette.text_muted,
            // The one slot no palette writes; see [`Theme::icon`].
            icon: readable_icon(palette.text_muted, palette.background, palette.surface),
            accent: palette.accent,
            danger: palette.danger,
            success: palette.success,
            overlay: palette.overlay,
            grid_header,
            grid_row_alt,
            // A tint of the theme's own brand color rather than a color of its
            // own: the selection has to be recognisable as "the app picked
            // this", which is the accent's job everywhere else too.
            grid_selection: palette.grid_selection.unwrap_or(Hsla {
                a: SELECTION_ALPHA,
                ..palette.accent
            }),
            // Dimmer than the muted text, then lifted back until it clears the
            // bar on both row backgrounds — the same shape as [`Theme::icon`],
            // and for the same reason: nobody checked.
            grid_null: palette.grid_null.unwrap_or_else(|| {
                readable(
                    towards(palette.text_muted, palette.background, NULL_FADE),
                    palette.background,
                    grid_row_alt,
                    MIN_GRID_TEXT_CONTRAST,
                )
            }),
            grid_pk: palette.grid_pk.unwrap_or_else(|| {
                readable(
                    palette.accent,
                    grid_header,
                    palette.background,
                    MIN_GRID_TEXT_CONTRAST,
                )
            }),
        }
    }
}

/// How far [`Theme::grid_header`] is lifted off the chrome surface.
///
/// Small: the header is a band, not a bar, and the columns it labels are the
/// thing being looked at.
const HEADER_STEP: f32 = 0.06;

/// How far [`Theme::grid_row_alt`] is lifted off the page.
///
/// Half the header's step, and deliberately near the limit of what can be seen
/// — the stripe only has to answer "am I still on the same row?" as the eye
/// tracks across twenty columns.
const STRIPE_STEP: f32 = 0.03;

/// Alpha of a derived [`Theme::grid_selection`].
///
/// Enough to read as a selection over either row background, little enough that
/// the values underneath keep their own color.
const SELECTION_ALPHA: f32 = 0.28;

/// How far a derived [`Theme::grid_null`] is faded towards the page before
/// being made legible again.
const NULL_FADE: f32 = 0.35;

/// Contrast the two grid foregrounds have to reach on the rows they sit on.
///
/// WCAG 2.1's 3:1 for large text and graphical objects rather than the 4.5:1
/// [`MIN_ICON_CONTRAST`] asks: both are *marks* on a dense surface — a `NULL`
/// placeholder and a key glyph — and holding them to body-text contrast would
/// leave them shouting over the values they annotate.
const MIN_GRID_TEXT_CONTRAST: f32 = 3.0;

/// Moves `color` `amount` away from the page: lighter on a dark palette, darker
/// on a light one.
///
/// The direction has to come from the palette rather than from the color, since
/// a light theme's surfaces sit near the top of the lightness axis where "add
/// a little" would run out of room and stop being visible at all.
fn away_from_page(color: Hsla, dark: bool, amount: f32) -> Hsla {
    shift_lightness(color, if dark { amount } else { -amount })
}

/// Mixes `color` `fraction` of the way towards `target` in HSL lightness.
///
/// Only the lightness moves, so the hue an author chose survives the fade.
fn towards(color: Hsla, target: Hsla, fraction: f32) -> Hsla {
    Hsla {
        l: color.l + (target.l - color.l) * fraction.clamp(0.0, 1.0),
        ..color
    }
}

/// Contrast an icon has to reach against the surfaces it is painted on.
///
/// WCAG 2.1 asks 3:1 of a graphical control and 4.5:1 of body text; icons are
/// held to the text figure here because they are drawn *thinner* than text —
/// a 12 px caption glyph is a stroke a little over a pixel wide, and an
/// antialiased stroke that narrow never reaches the full coverage the ratio
/// assumes. Aiming at 4.5 buys back roughly what the antialiasing gives away.
const MIN_ICON_CONTRAST: f32 = 4.5;

/// How many times [`readable`] halves the interval it searches.
///
/// Lightness is an `f32` in `[0, 1]`, so twenty-four halvings put the answer
/// far below the 1/255 that survives being written to a framebuffer: the
/// search stops well before precision does.
const CONTRAST_SEARCH_STEPS: u32 = 24;

/// The relative luminance of `color`, as WCAG 2.1 defines it.
///
/// Alpha plays no part: every foreground slot of a theme is opaque, and a
/// translucent one would have to be composited against a specific background
/// before its luminance meant anything at all.
fn relative_luminance(color: Hsla) -> f32 {
    let rgba = Rgba::from(color);
    // sRGB's transfer function, undone: the stored channel is gamma-encoded,
    // and luminance is a sum of *linear* light.
    let linear = |channel: f32| {
        let channel = channel.clamp(0.0, 1.0);
        if channel <= 0.03928 {
            channel / 12.92
        } else {
            ((channel + 0.055) / 1.055).powf(2.4)
        }
    };
    0.2126 * linear(rgba.r) + 0.7152 * linear(rgba.g) + 0.0722 * linear(rgba.b)
}

/// The WCAG contrast ratio between two colors, from `1.0` to `21.0`.
///
/// Symmetric in its arguments, so a caller need not know which of the two is
/// the foreground.
pub fn contrast_ratio(left: Hsla, right: Hsla) -> f32 {
    let (left, right) = (relative_luminance(left), relative_luminance(right));
    let (lighter, darker) = if left >= right {
        (left, right)
    } else {
        (right, left)
    };
    (lighter + 0.05) / (darker + 0.05)
}

/// The icon tint a theme whose muted text is `muted` should use.
///
/// Icons sit on both of a theme's two backgrounds — the app background and the
/// raised chrome of toolbars, panels and the tab strip — so the color is judged
/// against both and has to clear [`MIN_ICON_CONTRAST`] on the worse of them.
fn readable_icon(muted: Hsla, background: Hsla, surface: Hsla) -> Hsla {
    readable(muted, background, surface, MIN_ICON_CONTRAST)
}

/// `color`, moved as little as it takes to clear `target` on both backgrounds.
///
/// A color that already does is left exactly as it is, which is what keeps a
/// well-judged theme looking like itself; only the ones that fall short are
/// moved, and then only in lightness, so that the hue and saturation the
/// theme's author chose survive.
///
/// The direction is whichever end of the lightness axis is further from the two
/// backgrounds — away from them, in other words, which for a dark theme means
/// brighter and for a light one darker — and the amount is the smallest that
/// reaches the target, found by bisection. Relative luminance rises
/// monotonically with HSL lightness at a fixed hue and saturation, so beyond
/// the backgrounds the contrast does too and the bisection is well-founded.
///
/// One background always leaves an end of the axis that clears 4.5:1, so the
/// only palettes the search cannot satisfy are the ones whose two backgrounds
/// sit far apart on the ramp — black chrome on a white page, which no theme
/// here or on disk is — and those are given the better end anyway rather than
/// left where they were.
fn readable(color: Hsla, first: Hsla, second: Hsla, target: f32) -> Hsla {
    let at = |lightness: f32| Hsla {
        l: lightness,
        ..color
    };
    let worst =
        |candidate: Hsla| contrast_ratio(candidate, first).min(contrast_ratio(candidate, second));

    if worst(color) >= target {
        return color;
    }

    let end = if worst(at(1.0)) >= worst(at(0.0)) {
        1.0
    } else {
        0.0
    };
    if worst(at(end)) < target {
        return at(end);
    }

    // The invariant carried through the halvings: `short` fails the target and
    // `enough` meets it, so the answer is always the endpoint that meets it.
    let (mut short, mut enough) = (color.l, end);
    for _ in 0..CONTRAST_SEARCH_STEPS {
        let middle = (short + enough) / 2.0;
        if worst(at(middle)) >= target {
            enough = middle;
        } else {
            short = middle;
        }
    }
    at(enough)
}

impl Default for Theme {
    fn default() -> Self {
        Self::dark()
    }
}

impl Global for Theme {}

/// Id of the default theme.
const ID_ONE_DARK: &str = "one-dark";
/// Id of the light counterpart of [`ID_ONE_DARK`].
const ID_ONE_LIGHT: &str = "one-light";
/// Id of the dark Solarized theme.
const ID_SOLARIZED_DARK: &str = "solarized-dark";
/// Id of the light Solarized theme.
const ID_SOLARIZED_LIGHT: &str = "solarized-light";
/// Id of the dark Gruvbox theme.
const ID_GRUVBOX_DARK: &str = "gruvbox-dark";
/// Id of the Dracula theme.
const ID_DRACULA: &str = "dracula";

/// What [`ID_ONE_DARK`] was called before the themes had ids.
const LEGACY_ID_DARK: &str = "dark";
/// What [`ID_ONE_LIGHT`] was called before the themes had ids.
const LEGACY_ID_LIGHT: &str = "light";

/// One entry of the built-in theme table.
struct BuiltinTheme {
    /// Stable id stored in settings.
    id: &'static str,
    /// Human-readable name, shown in the picker.
    name: &'static str,
    /// Whether the palette is a dark one.
    dark: bool,
    /// Builds the palette. A function rather than a value because [`Hsla`] is
    /// not constructible in a `const`.
    build: fn() -> Theme,
}

/// Every built-in theme, in presentation order: the two defaults first, then
/// the borrowed palettes, dark before light where a family has both.
const BUILTIN_THEMES: [BuiltinTheme; 6] = [
    BuiltinTheme {
        id: ID_ONE_DARK,
        name: "One Dark",
        dark: true,
        build: Theme::dark,
    },
    BuiltinTheme {
        id: ID_ONE_LIGHT,
        name: "One Light",
        dark: false,
        build: Theme::light,
    },
    BuiltinTheme {
        id: ID_SOLARIZED_DARK,
        name: "Solarized Dark",
        dark: true,
        build: Theme::solarized_dark,
    },
    BuiltinTheme {
        id: ID_SOLARIZED_LIGHT,
        name: "Solarized Light",
        dark: false,
        build: Theme::solarized_light,
    },
    BuiltinTheme {
        id: ID_GRUVBOX_DARK,
        name: "Gruvbox Dark",
        dark: true,
        build: Theme::gruvbox_dark,
    },
    BuiltinTheme {
        id: ID_DRACULA,
        name: "Dracula",
        dark: true,
        build: Theme::dracula,
    },
];

/// A theme loaded from a file rather than compiled in.
#[derive(Debug, Clone)]
pub struct CustomUiTheme {
    /// Stable id stored in settings, taken from the file name.
    pub id: String,
    /// Human-readable name, taken from the file's `name` key.
    pub name: String,
    /// The palette itself.
    pub theme: Theme,
}

/// One entry of the combined built-in + custom theme listing.
///
/// What a picker needs to draw a row, and nothing more; the colors are fetched
/// with [`ThemeRegistry::resolve`] only for the entries that end up on screen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ThemeEntry {
    /// Stable id stored in settings.
    pub id: String,
    /// Human-readable name.
    pub name: String,
    /// Whether the palette is a dark one.
    pub dark: bool,
    /// Whether the theme ships with rudbman rather than coming from a file.
    pub builtin: bool,
}

/// The themes read from the user's `themes` directory.
///
/// A gpui [`Global`] rather than a process-wide static, because every reader of
/// a UI theme already holds an [`App`].
#[derive(Debug, Default)]
pub struct ThemeRegistry {
    /// The custom themes, in the order the loader found them.
    custom: Vec<CustomUiTheme>,
}

impl Global for ThemeRegistry {}

impl ThemeRegistry {
    /// Installs an empty registry, if none has been installed yet.
    ///
    /// Called from [`crate::init`], so that resolving an id before the
    /// theme files have been read answers the built-in themes rather than
    /// panicking on a missing global.
    pub fn init(cx: &mut App) {
        if cx.try_global::<Self>().is_none() {
            cx.set_global(Self::default());
        }
    }

    /// Replaces the themes loaded from the user's `themes` directory.
    ///
    /// The whole list is swapped at once, so re-scanning the directory cannot
    /// leave behind a theme its file no longer defines.
    pub fn set_custom(themes: Vec<CustomUiTheme>, cx: &mut App) {
        cx.set_global(Self { custom: themes });
    }

    /// The themes currently loaded from the user's `themes` directory.
    pub fn custom(cx: &App) -> Vec<CustomUiTheme> {
        cx.try_global::<Self>()
            .map(|registry| registry.custom.clone())
            .unwrap_or_default()
    }

    /// Whether `id` names a theme that ships with rudbman.
    pub fn is_builtin(id: &str) -> bool {
        BUILTIN_THEMES
            .iter()
            .any(|theme| theme.id.eq_ignore_ascii_case(id))
    }

    /// Every selectable theme: the built-in ones in presentation order, then
    /// the custom ones sorted by name.
    ///
    /// A custom theme whose id shadows a built-in one is left out, since
    /// [`ThemeRegistry::resolve`] would never hand it back anyway.
    pub fn all(cx: &App) -> Vec<ThemeEntry> {
        let mut entries: Vec<ThemeEntry> = BUILTIN_THEMES
            .iter()
            .map(|theme| ThemeEntry {
                id: theme.id.to_string(),
                name: theme.name.to_string(),
                dark: theme.dark,
                builtin: true,
            })
            .collect();

        let mut custom: Vec<ThemeEntry> = Self::custom(cx)
            .into_iter()
            .filter(|theme| !Self::is_builtin(&theme.id))
            .map(|theme| ThemeEntry {
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

    /// The palette `id` names, falling back to [`Theme::dark`].
    ///
    /// Ids are case-insensitive, built-in themes win over custom ones, and the
    /// two names the default themes went by before they had ids — `dark` and
    /// `light` — still resolve. An id nothing answers to falls back rather than
    /// failing: a settings file naming a theme whose file has been deleted has
    /// to keep opening the app.
    pub fn resolve(id: &str, cx: &App) -> Theme {
        if id.eq_ignore_ascii_case(LEGACY_ID_DARK) {
            return Theme::dark();
        }
        if id.eq_ignore_ascii_case(LEGACY_ID_LIGHT) {
            return Theme::light();
        }
        if let Some(builtin) = BUILTIN_THEMES
            .iter()
            .find(|theme| theme.id.eq_ignore_ascii_case(id))
        {
            return (builtin.build)();
        }
        Self::custom(cx)
            .into_iter()
            .find(|theme| theme.id.eq_ignore_ascii_case(id))
            .map(|theme| theme.theme)
            .unwrap_or_else(Theme::dark)
    }
}

/// Schema version written into a [`ThemeFile`] by this build.
const THEME_FILE_VERSION: u32 = 1;

/// Version assumed for a file that does not carry one.
fn default_theme_file_version() -> u32 {
    THEME_FILE_VERSION
}

/// One UI theme as it is written to disk.
///
/// Hand-editable by design, and read the same way `settings.json` is: keys
/// rudbman does not know are ignored, and a color it cannot parse falls back to
/// the corresponding slot of [`Theme::dark`] instead of failing the file.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ThemeFile {
    /// Schema version of the file; informational until a migration is needed.
    #[serde(default = "default_theme_file_version")]
    pub version: u32,
    /// Human-readable name, shown in the picker.
    pub name: String,
    /// Whether the palette is a dark one; drives the native window caption.
    #[serde(default)]
    pub dark: bool,
    /// The palette itself.
    pub colors: ThemeColors,
}

/// The color slots of a [`ThemeFile`].
///
/// Each value is `#RRGGBB`, or `#RRGGBBAA` where the slot carries alpha —
/// which, of the eleven required ones, only `overlay` meaningfully does, and of
/// the five grid ones, `grid_selection`.
///
/// The eleven are required and the five grid slots optional, which is what lets
/// a theme file written before the grid existed — including one lifted straight
/// out of logman — load unchanged; see the module documentation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ThemeColors {
    /// Window / app background.
    pub background: String,
    /// Background of raised chrome.
    pub surface: String,
    /// Surface color under the pointer.
    pub surface_hover: String,
    /// Surface color while pressed or selected.
    pub surface_active: String,
    /// Hairline separators and control outlines.
    pub border: String,
    /// Primary foreground color.
    pub text: String,
    /// Secondary foreground color.
    pub text_muted: String,
    /// Brand color.
    pub accent: String,
    /// Destructive actions and error states.
    pub danger: String,
    /// Successful / connected states.
    pub success: String,
    /// Translucent modal backdrop.
    pub overlay: String,
    /// Background of the result grid's header row. Derived when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub grid_header: Option<String>,
    /// Background of every other body row. Derived when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub grid_row_alt: Option<String>,
    /// Fill over the selected cells, usually with alpha. Derived when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub grid_selection: Option<String>,
    /// Foreground of the `NULL` marker. Derived when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub grid_null: Option<String>,
    /// Foreground marking a primary-key column. Derived when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub grid_pk: Option<String>,
}

impl ThemeFile {
    /// The file for a name, a darkness and a set of already-written colors.
    ///
    /// The counterpart of [`ThemeFile::from_theme`] for the theme editor, which
    /// holds each slot as the string the user typed rather than as a resolved
    /// color and has to write those strings back untouched — a `#ABCDEF` the
    /// user prefers in capitals stays in capitals.
    pub fn new(name: impl Into<String>, dark: bool, colors: ThemeColors) -> Self {
        Self {
            version: THEME_FILE_VERSION,
            name: name.into(),
            dark,
            colors,
        }
    }

    /// Turn the file into a palette the widgets can use.
    ///
    /// A color that is not a `#RRGGBB` or `#RRGGBBAA` value keeps the default
    /// theme's color for that slot, which is the same forgiveness the settings
    /// loader shows a hand-edited value. For the five optional grid slots the
    /// same typo means the same thing as leaving the key out — the value is
    /// derived from the rest of the palette — which is the better answer there:
    /// falling back to [`Theme::dark`]'s header on a *light* theme would put a
    /// near-black band over a white grid.
    pub fn to_theme(&self) -> Theme {
        let fallback = Theme::dark();
        let color = |value: &str, fallback: Hsla| parse_hex(value).unwrap_or(fallback);
        let optional = |value: &Option<String>| value.as_deref().and_then(parse_hex);

        Palette {
            dark: self.dark,
            background: color(&self.colors.background, fallback.background),
            surface: color(&self.colors.surface, fallback.surface),
            surface_hover: color(&self.colors.surface_hover, fallback.surface_hover),
            surface_active: color(&self.colors.surface_active, fallback.surface_active),
            border: color(&self.colors.border, fallback.border),
            text: color(&self.colors.text, fallback.text),
            text_muted: color(&self.colors.text_muted, fallback.text_muted),
            accent: color(&self.colors.accent, fallback.accent),
            danger: color(&self.colors.danger, fallback.danger),
            success: color(&self.colors.success, fallback.success),
            overlay: color(&self.colors.overlay, fallback.overlay),
            grid_header: optional(&self.colors.grid_header),
            grid_row_alt: optional(&self.colors.grid_row_alt),
            grid_selection: optional(&self.colors.grid_selection),
            grid_null: optional(&self.colors.grid_null),
            grid_pk: optional(&self.colors.grid_pk),
        }
        // Which also settles [`Theme::icon`] and any grid slot the file left
        // out, for a file that never mentions them: a hand-written theme is
        // legible whether or not its author thought about the icons, and it
        // has a grid whether or not it has heard of one.
        .into()
    }

    /// The file that would reproduce `theme` under the name `name`.
    ///
    /// The grid slots are always written, even the ones that were derived on
    /// the way in. They are optional because *old files* do not have them, not
    /// because they are unknowable: once a theme has been through the editor,
    /// spelling them out is what makes them editable next time. [`Theme::icon`]
    /// is the opposite case and stays absent — nothing may spell it out.
    pub fn from_theme(name: impl Into<String>, theme: &Theme) -> Self {
        Self {
            version: THEME_FILE_VERSION,
            name: name.into(),
            dark: theme.dark,
            colors: ThemeColors {
                background: to_hex(theme.background),
                surface: to_hex(theme.surface),
                surface_hover: to_hex(theme.surface_hover),
                surface_active: to_hex(theme.surface_active),
                border: to_hex(theme.border),
                text: to_hex(theme.text),
                text_muted: to_hex(theme.text_muted),
                accent: to_hex(theme.accent),
                danger: to_hex(theme.danger),
                success: to_hex(theme.success),
                overlay: to_hex(theme.overlay),
                grid_header: Some(to_hex(theme.grid_header)),
                grid_row_alt: Some(to_hex(theme.grid_row_alt)),
                grid_selection: Some(to_hex(theme.grid_selection)),
                grid_null: Some(to_hex(theme.grid_null)),
                grid_pk: Some(to_hex(theme.grid_pk)),
            },
        }
    }
}

/// Parse a `#RRGGBB` or `#RRGGBBAA` string into a color.
///
/// The leading `#` is optional and the digits are case-insensitive; anything
/// else — a short `#rgb`, a color name — answers `None`.
pub fn parse_hex(value: &str) -> Option<Hsla> {
    let value = value.trim();
    let digits = value.strip_prefix('#').unwrap_or(value);
    if !digits.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return None;
    }
    let channel = |index: usize| {
        u8::from_str_radix(digits.get(index..index + 2)?, 16)
            .ok()
            .map(|value| value as f32 / 255.0)
    };
    let alpha = match digits.len() {
        6 => 1.0,
        8 => channel(6)?,
        _ => return None,
    };
    Some(
        Rgba {
            r: channel(0)?,
            g: channel(2)?,
            b: channel(4)?,
            a: alpha,
        }
        .into(),
    )
}

/// Format a color as the `#rrggbb` a theme file expects.
///
/// The alpha channel is only written when the color has one to write, so the
/// ten opaque slots of a theme file stay readable six-digit values.
pub fn to_hex(color: Hsla) -> String {
    let rgba = Rgba::from(color);
    let channel = |value: f32| (value.clamp(0.0, 1.0) * 255.0).round() as u8;
    let (r, g, b) = (channel(rgba.r), channel(rgba.g), channel(rgba.b));
    if rgba.a >= 1.0 {
        format!("#{r:02x}{g:02x}{b:02x}")
    } else {
        format!("#{r:02x}{g:02x}{b:02x}{:02x}", channel(rgba.a))
    }
}

/// Returns the active theme, falling back to [`Theme::dark`] when the app has
/// not installed one yet.
///
/// A clone is returned rather than a borrow so that callers can keep using the
/// [`App`] mutably while styling their elements.
pub fn theme(cx: &App) -> Theme {
    cx.try_global::<Theme>().cloned().unwrap_or_default()
}

/// Installs `theme` as the active [`Theme`] global.
pub fn set_theme(theme: Theme, cx: &mut App) {
    cx.set_global(theme);
}

/// The window's configured background opacity, `1.0` meaning fully opaque.
///
/// A [`Global`] for the same reason [`Theme`] is one, and for one more: the
/// readers that need it most are leaves. The result grid and the ERD and
/// query-builder canvases each paint a full-bleed background and each has to know
/// whether the window is translucent before doing so, and not one of them knows
/// what a settings file is. They already reach for [`theme`] exactly this way.
/// (The SQL editor is not among them: it paints its background opaque either way,
/// for legibility.)
#[derive(Debug, Clone, Copy)]
struct WindowTint(f32);

impl Global for WindowTint {}

/// The installed window opacity, or `1.0` when the shell has not set one.
fn window_opacity(cx: &App) -> f32 {
    cx.try_global::<WindowTint>().map_or(1.0, |tint| tint.0)
}

/// Installs the window's background opacity.
///
/// The shell calls this at exactly the moments it hands the platform surface a
/// new background appearance — start-up, and a settings *save*. Never for a
/// preview; [`window_tint`] says why.
pub fn set_window_tint(opacity: f32, cx: &mut App) {
    cx.set_global(WindowTint(opacity));
}

/// Whether the window is drawn with a translucent or blurred background.
///
/// What a full-bleed content fill asks before painting itself *at all*. The body
/// behind such a fill already carries the one tinted fill the window permits, so
/// anything covering the same pixels again — tinted or opaque — is what would
/// hide the desktop or the blur behind it. See [`window_tint`] for why a second
/// tinted fill is no answer either.
///
/// A view that would rather be legible than see-through simply does not ask, and
/// paints itself opaque; the SQL editor is the one that made that choice.
pub fn window_translucent(cx: &App) -> bool {
    window_opacity(cx) < 1.0
}

/// Applies the window's background opacity to a background fill.
///
/// **At most one such fill may cover any given pixel**, and between them they
/// must leave no pixel of the body uncovered. The window surface starts out
/// fully transparent, so a single translucent fill lets the desktop (or the
/// acrylic blur behind the window) show through. A second one on top does not:
/// gpui's Windows renderer blends the alpha channel additively
/// (`SrcBlendAlpha = ONE, DestBlendAlpha = ONE`), so two fills of, say, 0.75 and
/// 0.62 saturate the surface alpha at 1.0 and the window goes opaque again. That
/// is why chrome bands paint their surface untinted, and why a content fill over
/// the body stops painting itself rather than tinting itself — see
/// [`window_translucent`].
///
/// Only the shell may call this, and only for a fill it has reasoned about; the
/// wrapper `app_settings::window_tint` is that call site and carries the rest of
/// the argument. Widgets in this crate ask [`window_translucent`] instead.
pub fn window_tint(color: Hsla, cx: &App) -> Hsla {
    let opacity = window_opacity(cx);
    if opacity < 1.0 {
        Hsla {
            a: opacity,
            ..color
        }
    } else {
        color
    }
}

/// Returns `color` with its lightness shifted by `delta`, clamped to `[0, 1]`.
///
/// Used by widgets to derive hover / pressed shades from a base color without
/// having to store one entry per state in [`Theme`].
pub fn shift_lightness(color: Hsla, delta: f32) -> Hsla {
    Hsla {
        l: (color.l + delta).clamp(0.0, 1.0),
        ..color
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Largest difference two colors may show and still count as the same one
    /// after a round trip through eight bits per channel.
    const CHANNEL_EPSILON: f32 = 1.0 / 255.0;

    /// Asserts that two colors survive a round trip as the same color.
    fn assert_same(left: Hsla, right: Hsla) {
        let left = Rgba::from(left);
        let right = Rgba::from(right);
        for (a, b) in [
            (left.r, right.r),
            (left.g, right.g),
            (left.b, right.b),
            (left.a, right.a),
        ] {
            assert!((a - b).abs() <= CHANNEL_EPSILON, "{left:?} != {right:?}");
        }
    }

    #[test]
    fn hex_round_trips() {
        assert_same(
            parse_hex("#ff0000").expect("red"),
            gpui::rgb(0xff0000).into(),
        );
        assert_eq!(parse_hex("AABBCC"), parse_hex("#aabbcc"));
        assert_eq!(to_hex(gpui::rgb(0x00ff7f).into()), "#00ff7f");

        for theme in [Theme::dark(), Theme::light(), Theme::dracula()] {
            assert_same(
                parse_hex(&to_hex(theme.accent)).expect("accent"),
                theme.accent,
            );
            assert_same(
                parse_hex(&to_hex(theme.overlay)).expect("overlay"),
                theme.overlay,
            );
        }
    }

    #[test]
    fn hex_writes_alpha_only_when_there_is_some() {
        assert_eq!(to_hex(hsla(0., 0., 0., 1.0)), "#000000");
        assert_eq!(to_hex(hsla(0., 0., 0., 0.5)), "#00000080");
        assert_eq!(parse_hex("#00000080").expect("alpha").a, 128.0 / 255.0);
    }

    #[test]
    fn hex_rejects_everything_else() {
        for value in ["", "#", "#abc", "#abcde", "#gghhii", "rebeccapurple"] {
            assert!(parse_hex(value).is_none(), "accepted {value:?}");
        }
    }

    #[test]
    fn theme_file_round_trips_through_json() {
        let theme = Theme::solarized_light();
        let file = ThemeFile::from_theme("Solarized Light", &theme);
        let json = serde_json::to_string(&file).expect("serialize");
        assert!(json.contains("\"surface_hover\""), "{json}");

        let parsed: ThemeFile = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(parsed, file);
        assert_eq!(parsed.version, 1);
        assert!(!parsed.dark);

        let restored = parsed.to_theme();
        assert_eq!(restored.dark, theme.dark);
        for (left, right) in [
            (restored.background, theme.background),
            (restored.surface, theme.surface),
            (restored.surface_hover, theme.surface_hover),
            (restored.surface_active, theme.surface_active),
            (restored.border, theme.border),
            (restored.text, theme.text),
            (restored.text_muted, theme.text_muted),
            (restored.accent, theme.accent),
            (restored.danger, theme.danger),
            (restored.success, theme.success),
            (restored.overlay, theme.overlay),
            (restored.grid_header, theme.grid_header),
            (restored.grid_row_alt, theme.grid_row_alt),
            (restored.grid_selection, theme.grid_selection),
            (restored.grid_null, theme.grid_null),
            (restored.grid_pk, theme.grid_pk),
        ] {
            assert_same(left, right);
        }
    }

    #[test]
    fn a_theme_file_tolerates_missing_and_unknown_keys() {
        let json = r##"{
            "name": "Sparse",
            "future_key": {"anything": [1, 2, 3]},
            "colors": {
                "background": "#101010",
                "surface": "#151515",
                "surface_hover": "not a color",
                "surface_active": "#252525",
                "border": "#303030",
                "text": "#e0e0e0",
                "text_muted": "#909090",
                "accent": "#3080f0",
                "danger": "#f04040",
                "success": "#40c060",
                "overlay": "#0000009e"
            }
        }"##;

        let file: ThemeFile = serde_json::from_str(json).expect("parse");
        // Both defaults apply: the version this build writes, and a light theme
        // only if the file says so.
        assert_eq!(file.version, 1);
        assert!(!file.dark);

        let theme = file.to_theme();
        assert_same(theme.background, gpui::rgb(0x101010).into());
        // The unparseable slot keeps the default theme's color.
        assert_same(theme.surface_hover, Theme::dark().surface_hover);
        assert_same(theme.overlay, gpui::rgba(0x0000009e).into());
        // And the five slots the file has never heard of are worked out from
        // the ones it does carry rather than left at the default theme's. The
        // direction follows the file's `dark` key — which this fixture does not
        // set, so the stripe goes *down* from a background that happens to be
        // dark. A palette that lies about which side of light/dark it is on is
        // already telling the window caption the same lie.
        assert_ne!(theme.grid_header, Theme::dark().grid_header);
        assert_same(theme.grid_row_alt, shift_lightness(theme.background, -0.03));
    }

    #[test]
    fn every_builtin_id_resolves_and_is_listed_once() {
        let mut ids: Vec<&str> = BUILTIN_THEMES.iter().map(|theme| theme.id).collect();
        assert_eq!(ids.len(), 6);
        ids.sort_unstable();
        let unique = ids.len();
        ids.dedup();
        assert_eq!(ids.len(), unique, "duplicate theme id");

        for theme in &BUILTIN_THEMES {
            assert!(ThemeRegistry::is_builtin(theme.id));
            assert_eq!((theme.build)().dark, theme.dark, "{}", theme.id);
        }
        assert!(!ThemeRegistry::is_builtin("nonsense"));
    }

    /// The worst contrast an icon shows on the two backgrounds it is painted
    /// on, which is the figure [`readable_icon`] is judged by.
    fn icon_contrast(theme: &Theme) -> f32 {
        contrast_ratio(theme.icon, theme.background).min(contrast_ratio(theme.icon, theme.surface))
    }

    /// A palette built around one background and one muted text, for the cases
    /// no shipped theme covers.
    fn palette(background: Hsla, surface: Hsla, text_muted: Hsla) -> Theme {
        Palette {
            background,
            surface,
            text_muted,
            ..dark_palette()
        }
        .into()
    }

    /// The default palette, as a starting point for [`palette`].
    fn dark_palette() -> Palette {
        let theme = Theme::dark();
        Palette {
            dark: theme.dark,
            background: theme.background,
            surface: theme.surface,
            surface_hover: theme.surface_hover,
            surface_active: theme.surface_active,
            border: theme.border,
            text: theme.text,
            text_muted: theme.text_muted,
            accent: theme.accent,
            danger: theme.danger,
            success: theme.success,
            overlay: theme.overlay,
            // Left unstated rather than copied back off the built-in theme, so
            // that a palette built through this helper exercises the derivation
            // instead of the values One Dark happens to spell out.
            grid_header: None,
            grid_row_alt: None,
            grid_selection: None,
            grid_null: None,
            grid_pk: None,
        }
    }

    #[test]
    fn contrast_is_symmetric_and_spans_the_whole_range() {
        let black = hsla(0., 0., 0., 1.0);
        let white = hsla(0., 0., 1.0, 1.0);
        assert!((contrast_ratio(black, white) - 21.0).abs() < 0.01);
        assert_eq!(contrast_ratio(black, white), contrast_ratio(white, black));
        assert_eq!(contrast_ratio(white, white), 1.0);
    }

    /// The reason [`Theme::icon`] exists: every theme that ships has to clear
    /// the bar on *both* of the backgrounds an icon is drawn on, which several
    /// of them did not when the icons were painted in the muted text — and the
    /// bar is read off the implementation rather than hardcoded here, so that
    /// raising it can never leave this test agreeing with the old figure.
    #[test]
    fn every_builtin_theme_gives_its_icons_enough_contrast() {
        for builtin in &BUILTIN_THEMES {
            let theme = (builtin.build)();
            assert!(
                icon_contrast(&theme) >= MIN_ICON_CONTRAST,
                "{}: icons at {:.2}:1",
                builtin.id,
                icon_contrast(&theme)
            );
        }
    }

    /// And a palette whose muted text is already legible keeps it: the derived
    /// slot is a floor under a theme, not a restyling of one.
    #[test]
    fn a_muted_text_that_is_already_legible_is_left_alone() {
        let theme = palette(
            hsla(0., 0., 0.05, 1.0),
            hsla(0., 0., 0.10, 1.0),
            hsla(210. / 360., 0.20, 0.80, 1.0),
        );
        assert_eq!(theme.icon, theme.text_muted);
    }

    /// A palette that has to be moved keeps its hue and saturation, and moves
    /// no further than it must.
    #[test]
    fn an_illegible_muted_text_is_moved_only_in_lightness() {
        let muted = hsla(225. / 360., 0.27, 0.51, 1.0);
        let theme = palette(hsla(231. / 360., 0.15, 0.184, 1.0), muted, muted);
        assert_ne!(theme.icon, theme.text_muted);
        assert_eq!(theme.icon.h, muted.h);
        assert_eq!(theme.icon.s, muted.s);
        assert_eq!(theme.icon.a, muted.a);
        assert!(icon_contrast(&theme) >= MIN_ICON_CONTRAST);
        // The smallest move that works: a hair darker would not have.
        let short = Hsla {
            l: theme.icon.l - 0.01,
            ..theme.icon
        };
        assert!(
            contrast_ratio(short, theme.background).min(contrast_ratio(short, theme.surface))
                < MIN_ICON_CONTRAST
        );
    }

    /// The extremes answer rather than panicking, and they answer with the most
    /// legible colour on offer: a mid-grey background can still be cleared, by
    /// going dark rather than light.
    #[test]
    fn an_extreme_palette_still_gets_an_answer() {
        let grey = hsla(0., 0., 0.5, 1.0);
        let theme = palette(grey, grey, grey);
        assert!(theme.icon.l < grey.l, "the icon went the wrong way");
        assert!(icon_contrast(&theme) >= MIN_ICON_CONTRAST);

        // And a palette that no colour can satisfy — one background at each end
        // of the ramp — is pushed to an end of the axis instead of being left
        // where it was.
        let split = palette(
            hsla(0., 0., 0.0, 1.0),
            hsla(0., 0., 1.0, 1.0),
            hsla(0., 0., 0.5, 1.0),
        );
        assert!(split.icon.l == 0.0 || split.icon.l == 1.0);
    }

    /// The guarantee reaches themes rudbman never saw: a file carries no `icon`
    /// key — adding one would break every theme already written — so the slot
    /// is derived on the way in, for a hand-written palette as much as for a
    /// built-in one.
    #[test]
    fn a_theme_from_a_file_gets_the_same_guarantee() {
        let json = r##"{
            "name": "Murky",
            "dark": true,
            "colors": {
                "background": "#202430",
                "surface": "#1a1e28",
                "surface_hover": "#2a2f3c",
                "surface_active": "#333949",
                "border": "#3d4455",
                "text": "#c8ccd8",
                "text_muted": "#4a5064",
                "accent": "#6ea8fe",
                "danger": "#e05561",
                "success": "#8cc265",
                "overlay": "#0a0c129e"
            }
        }"##;

        let file: ThemeFile = serde_json::from_str(json).expect("parse");
        let muted = parse_hex("#4a5064").expect("muted");
        let theme = file.to_theme();
        assert!(
            contrast_ratio(muted, theme.background).min(contrast_ratio(muted, theme.surface))
                < MIN_ICON_CONTRAST,
            "the fixture was not the illegible palette this test needs"
        );
        assert!(icon_contrast(&theme) >= MIN_ICON_CONTRAST);

        // The eleven the file spelled out come back untouched, and `icon` is
        // still not a key: nothing may spell it out, so a round trip cannot
        // invent one. The grid slots *are* keys by now — see
        // [`ThemeFile::from_theme`] — which is why they are dropped here before
        // the comparison rather than expected to be absent.
        let written = ThemeFile::from_theme("Murky", &theme);
        assert_eq!(
            ThemeColors {
                grid_header: None,
                grid_row_alt: None,
                grid_selection: None,
                grid_null: None,
                grid_pk: None,
                ..written.colors.clone()
            },
            file.colors
        );
        let json = serde_json::to_string(&written).expect("serialize");
        assert!(!json.contains("icon"), "{json}");
    }

    /// The worst contrast a grid foreground shows on the two backgrounds it is
    /// drawn on, which is the figure the derivation is judged by.
    fn grid_null_contrast(theme: &Theme) -> f32 {
        contrast_ratio(theme.grid_null, theme.background)
            .min(contrast_ratio(theme.grid_null, theme.grid_row_alt))
    }

    /// The same, for the primary-key mark, which sits on the header instead.
    fn grid_pk_contrast(theme: &Theme) -> f32 {
        contrast_ratio(theme.grid_pk, theme.grid_header)
            .min(contrast_ratio(theme.grid_pk, theme.background))
    }

    /// The six palettes spell their grid colors out by hand, so the derivation
    /// is not what makes them legible — this is the check that the hand-picked
    /// values were picked well, on the light themes as much as the dark ones.
    #[test]
    fn every_builtin_theme_gives_its_grid_marks_enough_contrast() {
        for builtin in &BUILTIN_THEMES {
            let theme = (builtin.build)();
            assert!(
                grid_null_contrast(&theme) >= MIN_GRID_TEXT_CONTRAST,
                "{}: NULL marker at {:.2}:1",
                builtin.id,
                grid_null_contrast(&theme)
            );
            assert!(
                grid_pk_contrast(&theme) >= MIN_GRID_TEXT_CONTRAST,
                "{}: key mark at {:.2}:1",
                builtin.id,
                grid_pk_contrast(&theme)
            );
        }
    }

    /// And the two grid backgrounds have to be *told apart* from the page and
    /// from each other, or the header stops reading as a header.
    #[test]
    fn every_builtin_theme_separates_its_grid_backgrounds() {
        for builtin in &BUILTIN_THEMES {
            let theme = (builtin.build)();
            for (name, color) in [
                ("header", theme.grid_header),
                ("stripe", theme.grid_row_alt),
            ] {
                assert!(
                    (color.l - theme.background.l).abs() > 0.005,
                    "{}: the {name} is the page",
                    builtin.id
                );
            }
            assert!(
                theme.grid_header.l != theme.grid_row_alt.l,
                "{}: the header is the stripe",
                builtin.id
            );
            // And the selection has to let the row show through.
            assert!(theme.grid_selection.a < 1.0, "{}", builtin.id);
        }
    }

    /// A theme that says nothing about the grid still gets one, and gets one
    /// that points the right way: away from the page, whichever page it is.
    #[test]
    fn a_palette_with_no_grid_colors_gets_a_derived_grid() {
        for (dark, theme) in [
            (true, Theme::dark()),
            (false, Theme::light()),
            (true, Theme::dracula()),
            (false, Theme::solarized_light()),
        ] {
            // Rebuilt through `Palette` with the grid slots dropped, which is
            // exactly the shape an eleven-slot file arrives in.
            let bare: Theme = Palette {
                dark,
                background: theme.background,
                surface: theme.surface,
                surface_hover: theme.surface_hover,
                surface_active: theme.surface_active,
                border: theme.border,
                text: theme.text,
                text_muted: theme.text_muted,
                accent: theme.accent,
                danger: theme.danger,
                success: theme.success,
                overlay: theme.overlay,
                grid_header: None,
                grid_row_alt: None,
                grid_selection: None,
                grid_null: None,
                grid_pk: None,
            }
            .into();

            let further = |derived: Hsla, from: Hsla| {
                if dark {
                    derived.l > from.l
                } else {
                    derived.l < from.l
                }
            };
            assert!(further(bare.grid_header, bare.surface), "header");
            assert!(further(bare.grid_row_alt, bare.background), "stripe");
            // The stripe is a hint: it must move, but hardly.
            assert!((bare.grid_row_alt.l - bare.background.l).abs() < 0.05);
            assert_eq!(bare.grid_selection.h, bare.accent.h);
            assert!(bare.grid_selection.a < 1.0);
            assert!(grid_null_contrast(&bare) >= MIN_GRID_TEXT_CONTRAST);
            assert!(grid_pk_contrast(&bare) >= MIN_GRID_TEXT_CONTRAST);
        }
    }

    /// A grid color a file *does* spell out is used as it stands, even one the
    /// derivation would never have chosen: the fallback is for the files that
    /// predate the slots, not a house style imposed on the ones that do not.
    #[test]
    fn a_grid_color_that_is_spelled_out_wins() {
        let json = r##"{
            "name": "Loud",
            "dark": true,
            "colors": {
                "background": "#202430",
                "surface": "#1a1e28",
                "surface_hover": "#2a2f3c",
                "surface_active": "#333949",
                "border": "#3d4455",
                "text": "#c8ccd8",
                "text_muted": "#8a90a4",
                "accent": "#6ea8fe",
                "danger": "#e05561",
                "success": "#8cc265",
                "overlay": "#0a0c129e",
                "grid_header": "#ff00ff",
                "grid_selection": "#00ff0040",
                "grid_null": "#111111"
            }
        }"##;

        let file: ThemeFile = serde_json::from_str(json).expect("parse");
        let theme = file.to_theme();
        assert_same(theme.grid_header, gpui::rgb(0xff00ff).into());
        assert_same(theme.grid_selection, gpui::rgba(0x00ff0040).into());
        // Even one that fails the contrast bar the derivation would have held
        // itself to: an author who writes a color has asked for that color.
        assert_same(theme.grid_null, gpui::rgb(0x111111).into());
        assert!(grid_null_contrast(&theme) < MIN_GRID_TEXT_CONTRAST);
        // The two the file left out are still derived.
        assert_ne!(theme.grid_row_alt, theme.background);
        assert_eq!(theme.grid_pk.h, theme.accent.h);
    }

    /// A grid color that is present but unreadable is treated as absent rather
    /// than falling back to [`Theme::dark`]'s: a near-black header on a light
    /// theme would be far worse than a derived one.
    #[test]
    fn an_unparseable_grid_color_is_derived_rather_than_borrowed() {
        let mut file = ThemeFile::from_theme("Light-ish", &Theme::light());
        file.colors.grid_header = Some("chartreuse".into());

        let theme = file.to_theme();
        assert_ne!(theme.grid_header, Theme::dark().grid_header);
        assert!(theme.grid_header.l < theme.surface.l, "went the wrong way");
    }
}
