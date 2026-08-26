//! The vector icon set, embedded in the binary.
//!
//! gpui's [`svg`](gpui::svg) element resolves its `path` through the
//! [`AssetSource`](gpui::AssetSource) the application was built with —
//! [`ICONS`] here — and paints the result as a *monochrome* sprite: resvg
//! rasterises the file, only the alpha channel survives, and the element's
//! `text_color` supplies the colour. Two things follow, and both are why these
//! files look the way they do:
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
//! What is here is rudbman's own: the application mark, the two buttons of the
//! tab strip, the explorer's object glyphs and the panel controls. The four
//! caption glyphs of the custom title bar are not — they belong to every
//! self-drawn title bar rather than to this application, so they come from
//! [`rugpui_shell::icons`] and are concatenated into [`ICONS`] rather than copied
//! into `assets/`. Nor are the two disclosure carets: the widget layer draws
//! them from [`rugpui::ICONS`], and they are concatenated in for the same
//! reason — a tree, a fold-away section and a dropdown all wear the same mark,
//! and it is rugpui's rather than any one application's.

pub use rugpui_shell::icons::icon;
use rugpui_shell::icons::{IconSet, WINDOW_CONTROL_ICONS};

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

/// The explorer tree's disclosure mark on a closed node.
///
/// The tree used to draw `▸` and `▾` as text, and no font size rescued them:
/// those code points fill a fraction of their em square, so the mark that
/// reached the screen was a third of the space it was given and its direction
/// was a couple of antialiased pixels. A chevron drawn as geometry spans the
/// box it is handed, and the two directions differ by half the glyph.
///
/// Like the window controls, it carries a heavier stroke than the set's usual
/// `1.8` — `2.4` here. The tree renders it at 14 px (`ARROW_ICON_SIZE` in
/// [`rugpui::tree`]) out of the 24 of the viewBox, so what reaches the
/// screen is a little over half of what the file asks for: 1.4 px, which a row
/// of pixels can hold, where `1.8` would have arrived as the same hairline the
/// glyph was.
pub const CHEVRON_RIGHT: &str = "icons/chevron-right.svg";

/// The explorer tree's disclosure mark on an open node — [`CHEVRON_RIGHT`]
/// turned a quarter turn, so that opening a node reads as the mark tipping over
/// rather than as a different mark.
pub const CHEVRON_DOWN: &str = "icons/chevron-down.svg";

/// rudbman's own icons, paired with the bytes [`ICONS`] hands back for them.
const OWN: &[(&str, &[u8])] = &[
    (APP_ICON, include_bytes!("../../../assets/icon.svg")),
    (TAB_LIST, include_bytes!("../assets/icons/tab-list.svg")),
    (NEW_TAB, include_bytes!("../assets/icons/new-tab.svg")),
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
    (
        CHEVRON_RIGHT,
        include_bytes!("../assets/icons/chevron-right.svg"),
    ),
    (
        CHEVRON_DOWN,
        include_bytes!("../assets/icons/chevron-down.svg"),
    ),
];

/// The asset source backing every [`svg`](gpui::svg) element in the app.
///
/// Install it with [`Application::with_assets`](gpui::Application::with_assets);
/// without it gpui's default source answers every path with `None` and the
/// icons paint as nothing at all.
///
/// Three tables rather than one: rudbman's own above, then the shell's four
/// caption glyphs, then the widget layer's two disclosure carets — so that the
/// window controls stay one copy across the applications that draw their own
/// title bar, and the carets stay one copy across the widgets that draw them.
///
/// Leaving [`rugpui::ICONS`] out is not a build error but a silent one: gpui
/// answers the missing paths with `None` and paints nothing, so the explorer
/// tree grows a column of blank arrows and every dropdown loses its chevron.
/// The `rugpui/` segment in those paths keeps them clear of rudbman's own.
pub const ICONS: IconSet = IconSet::new(&[OWN, WINDOW_CONTROL_ICONS, rugpui::ICONS]);

#[cfg(test)]
mod tests {
    use gpui::AssetSource;

    use super::*;

    #[test]
    fn every_icon_loads_and_is_an_svg() {
        for (name, _) in OWN {
            let bytes = ICONS
                .load(name)
                .expect("loading an embedded icon cannot fail")
                .unwrap_or_else(|| panic!("{name} is missing from the asset source"));
            let text = std::str::from_utf8(&bytes).expect("an icon must be UTF-8");
            assert!(text.contains("<svg"), "{name} is not an SVG");
            // The glyph set shares one 24x24 box; the application icon is the
            // shipped 256 px mark, embedded whole rather than redrawn.
            let viewbox = if *name == APP_ICON {
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
            ICONS
                .load("icons/nothing.svg")
                .expect("a missing asset is not a failure")
                .is_none()
        );
    }

    #[test]
    fn the_caption_glyphs_come_from_the_shell_rather_than_from_a_copy_here() {
        // The four files live in `rugpui-shell/assets`, and this is the assertion
        // that would fail if a copy of them crept back into `assets/icons`.
        for path in [
            rugpui_shell::icons::WINDOW_MINIMIZE,
            rugpui_shell::icons::WINDOW_MAXIMIZE,
            rugpui_shell::icons::WINDOW_RESTORE,
            rugpui_shell::icons::WINDOW_CLOSE,
        ] {
            assert!(
                !OWN.iter().any(|(name, _)| *name == path),
                "{path} is in rudbman's own table as well as the shell's"
            );
            assert!(
                ICONS.load(path).expect("loading cannot fail").is_some(),
                "{path} is in neither"
            );
        }
        assert_eq!(
            ICONS.len(),
            OWN.len() + WINDOW_CONTROL_ICONS.len() + rugpui::ICONS.len()
        );
    }

    #[test]
    fn the_disclosure_carets_come_from_the_widget_layer() {
        // rugpui's widgets draw these two by default, and they are drawn
        // through *this* asset source: unchained, they would paint as nothing.
        for path in [rugpui::CARET_RIGHT, rugpui::CARET_DOWN] {
            assert!(
                ICONS.load(path).expect("loading cannot fail").is_some(),
                "{path} is not in the set rugpui's widgets draw through"
            );
        }
    }
}
