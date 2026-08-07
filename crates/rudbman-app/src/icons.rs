//! The vector icon set, embedded in the binary.
//!
//! gpui's [`svg`](gpui::svg) element resolves its `path` through the
//! [`AssetSource`] the application was built with — [`Icons`] here — and paints
//! the result as a *monochrome* sprite: resvg rasterises the file, only the
//! alpha channel survives, and the element's `text_color` supplies the colour.
//! Two things follow, and both are why these files look the way they do:
//!
//! * the colours written in an icon never reach the screen, only its coverage
//!   does, so a `fill-opacity` below `1` reads as a lighter shade of the tint;
//! * the tint is whatever the *element* asks for, and unlike text it is not
//!   inherited from a parent, so a hover that recolours a button has to reach
//!   the icon through [`group_hover`](gpui::InteractiveElement::group_hover).
//!
//! The bytes come from [`include_bytes!`], not from files read at run time: a
//! release then carries its icons wherever it is unpacked, and packaging has
//! nothing extra to ship. Cargo tracks the embedded files itself, so an edited
//! icon rebuilds the crate without help from `build.rs`.
//!
//! The set is deliberately the M0 shell's and nothing more: the window
//! controls, the two buttons of the tab strip and the application mark. The
//! database marks — driver badges, table and view and procedure glyphs — arrive
//! with the explorer tree in M1/M2, where there is something to put them on.

use std::borrow::Cow;

use gpui::{AssetSource, Hsla, Pixels, Result, SharedString, Styled, Svg, svg};

/// The button at the end of the tab strip that lists every open tab.
///
/// A plain chevron rather than a stack of lines: the strip's other end already
/// carries the application menu's `☰`, and two list-shaped glyphs facing each
/// other across one toolbar would read as the same control twice. A chevron
/// says "this opens downwards", which is the one thing the button does.
pub const TAB_LIST: &str = "icons/tab-list.svg";

/// The button at the end of the tab strip that opens a new connection.
///
/// Drawn with the stroke of [`TAB_LIST`] rather than a toolbar icon's: the two
/// sit shoulder to shoulder in the strip, and it is that pairing the glyph has
/// to match.
pub const NEW_TAB: &str = "icons/new-tab.svg";

/// The custom title bar's minimise button.
///
/// The four window-control glyphs are drawn edge to edge of the 24×24 box
/// rather than inset like the rest of the set: they are painted at half the
/// size of a toolbar icon, and a glyph that kept the usual margin would come
/// out thinner and smaller than the caption buttons of the platform they stand
/// in for.
///
/// They carry a heavier stroke than the rest of the set for the same reason —
/// `2.2` against the usual `1.8`. The caption strip renders them at 12 px
/// (`GLYPH_SIZE` in [`rudbman_ui::window_controls`]), which is half the
/// viewBox, so the stroke that reaches the screen is half what the file asks
/// for: `1.8` arrived as 0.9 px, a hairline no row of pixels could hold at full
/// coverage once it had been antialiased, and `2.2` arrives as 1.1 px instead.
/// All four share the value, including both rectangles of [`WINDOW_RESTORE`],
/// so that the strip reads as one set.
pub const WINDOW_MINIMIZE: &str = "icons/window-minimize.svg";

/// The custom title bar's maximise button, while the window is not maximised.
pub const WINDOW_MAXIMIZE: &str = "icons/window-maximize.svg";

/// The custom title bar's maximise button, while the window *is* maximised.
///
/// Two offset squares, the shape every desktop uses for "put it back": the
/// button keeps its place and only the glyph says which way it will go.
pub const WINDOW_RESTORE: &str = "icons/window-restore.svg";

/// The custom title bar's close button.
pub const WINDOW_CLOSE: &str = "icons/window-close.svg";

/// The application mark, drawn at the left end of the custom title bar.
///
/// This is the shipped `assets/icon.svg` itself, embedded under an asset path
/// so the title bar can draw it with [`img`](gpui::img) — which, unlike the
/// [`svg`](gpui::svg) element, keeps an SVG's own colours instead of reducing
/// it to a tintable alpha mask. The bar shows the very mark the taskbar and
/// Alt-Tab show — gold cap, blue barrel, embossed plate — and there is no
/// second drawing to keep in step with the master.
///
/// It was not always so: an earlier bar drew a monochrome outline stand-in
/// (`icons/logo.svg`), because the shipped icon's tile was then a near-flat
/// dark swatch that melted into dark chrome, leaving only the outline showing.
/// The plate now carries its own gradient, a legible ring and an embossed
/// edge, so it separates from the bar the way it separates from a taskbar,
/// and the stand-in went away with its reason.
pub const APP_ICON: &str = "icons/app-icon.svg";

// --- the explorer's object marks ------------------------------------------
//
// One glyph per kind of thing a database holds, because the tree is read by
// shape before it is read by word: a schema with two hundred tables and four
// views is only scannable if a view does not look like a table. They share the
// set's 1.8 stroke and 24×24 box, and they are drawn as outlines rather than
// filled shapes so that the theme's tint is what colours them.

/// A base table.
pub const TABLE: &str = "icons/table.svg";

/// A view.
///
/// The table's frame, dashed — a view has the shape of a table without being
/// one — with an eye inside it. The dashes are what tells the two apart at a
/// glance down a long list, which is the only moment this distinction matters.
pub const VIEW: &str = "icons/view.svg";

/// One of the folders a schema is divided into.
pub const FOLDER: &str = "icons/folder.svg";

/// A schema, and — where a product has them — a catalogue.
///
/// The database cylinder, the same solid [`APP_ICON`] draws, because that is what
/// the level *is*: everything under it is one database's contents.
pub const SCHEMA: &str = "icons/schema.svg";

/// A stored procedure.
pub const PROCEDURE: &str = "icons/procedure.svg";

/// A stored function.
///
/// The mathematician's *f*, not the procedure's arrow: a function is asked for
/// a value and a procedure is told to do something, and the glyphs say which.
pub const FUNCTION: &str = "icons/function.svg";

/// A sequence.
pub const SEQUENCE: &str = "icons/sequence.svg";

/// Reloads whatever the panel is showing.
pub const REFRESH: &str = "icons/refresh.svg";

/// Copies the panel's text to the clipboard.
pub const COPY: &str = "icons/copy.svg";

/// Shows and hides the explorer.
pub const SIDEBAR: &str = "icons/sidebar.svg";

/// Every icon, paired with the bytes [`Icons`] hands back for it.
const ICONS: [(&str, &[u8]); 17] = [
    (APP_ICON, include_bytes!("../../../assets/icon.svg")),
    (TAB_LIST, include_bytes!("../assets/icons/tab-list.svg")),
    (NEW_TAB, include_bytes!("../assets/icons/new-tab.svg")),
    (
        WINDOW_MINIMIZE,
        include_bytes!("../assets/icons/window-minimize.svg"),
    ),
    (
        WINDOW_MAXIMIZE,
        include_bytes!("../assets/icons/window-maximize.svg"),
    ),
    (
        WINDOW_RESTORE,
        include_bytes!("../assets/icons/window-restore.svg"),
    ),
    (
        WINDOW_CLOSE,
        include_bytes!("../assets/icons/window-close.svg"),
    ),
    (TABLE, include_bytes!("../assets/icons/table.svg")),
    (VIEW, include_bytes!("../assets/icons/view.svg")),
    (FOLDER, include_bytes!("../assets/icons/folder.svg")),
    (SCHEMA, include_bytes!("../assets/icons/schema.svg")),
    (PROCEDURE, include_bytes!("../assets/icons/procedure.svg")),
    (FUNCTION, include_bytes!("../assets/icons/function.svg")),
    (SEQUENCE, include_bytes!("../assets/icons/sequence.svg")),
    (REFRESH, include_bytes!("../assets/icons/refresh.svg")),
    (COPY, include_bytes!("../assets/icons/copy.svg")),
    (SIDEBAR, include_bytes!("../assets/icons/sidebar.svg")),
];

/// The asset source backing every [`svg`](gpui::svg) element in the app.
///
/// Install it with [`Application::with_assets`](gpui::Application::with_assets);
/// without it gpui's default source answers every path with `None` and the
/// icons paint as nothing at all.
pub struct Icons;

impl AssetSource for Icons {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        Ok(ICONS
            .iter()
            .find(|(name, _)| *name == path)
            .map(|(_, bytes)| Cow::Borrowed(*bytes)))
    }

    fn list(&self, path: &str) -> Result<Vec<SharedString>> {
        Ok(ICONS
            .iter()
            .map(|(name, _)| *name)
            .filter(|name| name.starts_with(path))
            .map(SharedString::from)
            .collect())
    }
}

/// A square icon, sized and tinted.
///
/// The result is still an [`Svg`], so a caller can go on styling it — which is
/// what the hover states do.
pub fn icon(path: &'static str, size: Pixels, color: Hsla) -> Svg {
    svg().size(size).flex_none().path(path).text_color(color)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_icon_loads_and_is_an_svg() {
        for (name, _) in ICONS {
            let bytes = Icons
                .load(name)
                .expect("loading an embedded icon cannot fail")
                .unwrap_or_else(|| panic!("{name} is missing from the asset source"));
            let text = std::str::from_utf8(&bytes).expect("an icon must be UTF-8");
            assert!(text.contains("<svg"), "{name} is not an SVG");
            // The glyph set shares one 24×24 box; the application icon is the
            // shipped 256 px mark, embedded whole rather than redrawn.
            let viewbox = if name == APP_ICON {
                "viewBox=\"0 0 256 256\""
            } else {
                "viewBox=\"0 0 24 24\""
            };
            assert!(text.contains(viewbox), "{name} has the wrong viewBox");
        }
    }

    #[test]
    fn an_unknown_path_is_not_an_error() {
        assert!(
            Icons
                .load("icons/nothing.svg")
                .expect("a missing asset is not a failure")
                .is_none()
        );
    }

    #[test]
    fn listing_returns_the_whole_set() {
        assert_eq!(Icons.list("icons/").unwrap().len(), ICONS.len());
    }
}
