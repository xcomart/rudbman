//! Reusable gpui widgets shared by every rudbman view.
//!
//! The crate is deliberately free of database concepts: it knows nothing of
//! connections, catalogues, statements or result sets, and only about colors
//! ([`theme`], [`editor_theme`]), text entry ([`text_input`]), buttons
//! ([`button`]), session tabs ([`tab_bar`]), dropdown menus ([`menu`]), hover
//! tooltips ([`tooltip`]), dialogs ([`modal`]), overlay scroll indicators
//! ([`scrollbar`]), lazily filled trees ([`tree`]) and the caption buttons of a
//! self-drawn title bar ([`window_controls`]). A widget that would need a result
//! set to draw itself belongs a layer up, not here — the tree included: it
//! knows about ids the host invents and rows the host draws, and nothing about
//! what a catalogue or a schema is.
//!
//! Two palettes live side by side and are chosen independently: [`theme`] is
//! the chrome, the result grid included, and [`editor_theme`] is the SQL editor
//! alone — a different file, a different directory and a different set of
//! tokens. [`theme_store`] reads both from the user's configuration directory.
//!
//! Call [`init`] once during application start-up so the widgets that need key
//! bindings get them.

#![warn(missing_docs)]

pub mod button;
pub mod checkbox;
pub mod editor_theme;
pub mod editor_theme_picker;
pub mod menu;
pub mod modal;
pub mod scrollbar;
pub mod segmented;
pub mod select;
pub mod tab_bar;
pub mod text_input;
pub mod theme;
pub mod theme_store;
pub mod tooltip;
pub mod tree;
pub mod window_controls;

pub use button::{Button, ButtonVariant};
pub use checkbox::Checkbox;
pub use editor_theme::{
    CustomEditorTheme, EditorTheme, EditorThemeColors, EditorThemeEntry, EditorThemeFile,
    EditorThemeRegistry, editor_theme, set_editor_theme,
};
pub use editor_theme_picker::{EditorThemePicker, EditorThemeSwatch};
pub use menu::{ContextMenu, MenuButton, MenuEntry};
pub use modal::{form_row, modal};
pub use scrollbar::{
    DraggedThumb, Scrollbar, ScrollbarAxis, ScrollbarState, hide_later, scroll_to, scrolled,
};
pub use segmented::Segmented;
pub use select::Select;
pub use tab_bar::{TabBar, TabItem, TabStatus};
pub use text_input::TextInput;
pub use theme::{
    CustomUiTheme, Theme, ThemeColors, ThemeEntry, ThemeFile, ThemeRegistry, parse_hex, set_theme,
    theme, to_hex,
};
pub use tooltip::tooltip_label;
pub use tree::{ChildState, TreeEvent, TreeRow, TreeRowInfo, TreeSource, TreeView};
pub use window_controls::{WindowControlIcons, WindowControls};

use gpui::App;

/// Registers everything the widget layer needs before the first window opens.
///
/// Both registries are installed empty and both palettes are set to their
/// defaults, so that a view rendered before [`theme_store::reload`] has read the
/// user's files still has colors to draw with.
pub fn init(cx: &mut App) {
    ThemeRegistry::init(cx);
    EditorThemeRegistry::init(cx);
    set_theme(Theme::dark(), cx);
    set_editor_theme(EditorTheme::default(), cx);
    TextInput::init(cx);
    tree::init(cx);
}
