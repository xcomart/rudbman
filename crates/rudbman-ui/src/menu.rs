//! Dropdown menus: the toolbar application menu, and the menu a right-click
//! opens at the pointer.
//!
//! Windows and Linux get no native menu bar from gpui — [`gpui::App::set_menus`]
//! only builds one on macOS — so the shell draws its own. [`MenuButton`] is that
//! drawing: a compact glyph button which, while open, paints a list of
//! [`MenuEntry`] rows over the rest of the window. [`ContextMenu`] paints the
//! same list, without a trigger, wherever the caller says.
//!
//! Like every other widget here both are stateless: the parent view owns the
//! open flag — and, for a context menu, the position that goes with it — passes
//! it in on every render, and closes the menu from
//! [`MenuButton::on_open_change`] or [`ContextMenu::on_dismiss`].

use std::rc::Rc;

use gpui::{
    AnyElement, App, Corner, ElementId, Pixels, Point, SharedString, Size, Window, anchored,
    deferred, div, point, prelude::*, px, svg,
};

use crate::theme::{Theme, theme};
use crate::tooltip::tooltip_label;

/// Edge length of the trigger button.
const TRIGGER_SIZE: f32 = 28.;

/// Edge length of the icon inside the trigger, when it carries one.
///
/// Matches the toolbar's other icon buttons rather than the glyph it may stand
/// in for: a vector drawn at its own size sits in the row at the same weight as
/// the panel toggle beside it, which a font glyph scaled to the same box would
/// not.
const TRIGGER_ICON: f32 = 16.;

/// Style group of a trigger, so hovering the button recolours the icon in it.
///
/// Shared by every [`MenuButton`] rather than made unique per button: a
/// `group_hover` resolves against the nearest ancestor carrying the name, so
/// two triggers side by side each answer to their own.
const TRIGGER_GROUP: &str = "menu-trigger";

/// Vertical distance from the top of the trigger to the top of the dropdown, so
/// that the panel clears the button it hangs from.
const DROP_OFFSET: f32 = TRIGGER_SIZE + 4.;

/// Width of a [`MenuButton`] dropdown panel.
///
/// Wide enough for the longest row that menu has — the pane commands name the
/// thing they act on ("Split right of current tab") and carry a shortcut hint —
/// with room for a translation of it, since a row neither wraps nor ellipsises.
///
/// Fixed rather than content-sized because the button's menu is the same menu
/// every time it is opened: a width that followed the entries would make the
/// panel breathe as commands come and go, under a trigger that never moves.
const PANEL_WIDTH: f32 = 280.;

/// Narrowest a content-sized panel draws.
///
/// A context menu is built for one right-click and can be three short words
/// long; without a floor it would come out as a sliver that reads as a glitch
/// rather than a menu.
const PANEL_MIN_WIDTH: f32 = 180.;

/// Widest a content-sized panel draws.
///
/// Past this a row is truncated instead: the label is a command, not the text
/// of the object it acts on, and a panel wide enough for a fully qualified name
/// would cover the surface the menu was opened over.
const PANEL_MAX_WIDTH: f32 = 360.;

/// Width of the column a check mark sits in.
const CHECK_WIDTH: f32 = 16.;

/// What a checked row is marked with.
///
/// A glyph rather than an asset: the mark has to line up with a row of text and
/// take the row's colour, which is what a character does for free.
const CHECK_MARK: &str = "\u{2713}";

/// Key a test looks a drawn panel's bounds up by.
///
/// gpui compiles the call that records it away outside a test build, so this
/// costs the application nothing; it is here because the width of a panel is
/// now a decision rather than a constant, and a decision wants a test.
const PANEL_SELECTOR: &str = "rudbman-menu-panel";

/// Distance the dropdown keeps from the window edges when it would overflow.
const WINDOW_MARGIN: f32 = 6.;

/// Draw order of the click-catching backdrop, relative to other deferred draws.
const BACKDROP_PRIORITY: usize = 1;

/// Draw order of the dropdown panel; above [`BACKDROP_PRIORITY`] so that the
/// backdrop never eats clicks meant for a menu row.
const PANEL_PRIORITY: usize = 2;

/// Callback fired when a menu row is activated.
type ActivateHandler = Rc<dyn Fn(&mut Window, &mut App)>;

/// Callback fired when an open menu wants to close itself.
type DismissHandler = Rc<dyn Fn(&mut Window, &mut App)>;

/// Callback fired when the menu wants to open or close itself.
type OpenChangeHandler = Rc<dyn Fn(bool, &mut Window, &mut App)>;

/// One row of a [`MenuButton`] or [`ContextMenu`] dropdown.
///
/// A row is either a command — a label, an optional shortcut hint and a
/// callback — or a horizontal rule built with [`MenuEntry::separator`].
pub struct MenuEntry {
    /// Text shown on the left of the row.
    label: SharedString,
    /// Shortcut hint shown right-aligned and muted.
    shortcut: Option<SharedString>,
    /// Invoked when the row is clicked.
    on_activate: Option<ActivateHandler>,
    /// Whether the row is a rule rather than a command.
    separator: bool,
    /// Whether the row is shown but cannot be run.
    disabled: bool,
    /// Whether the row carries a check mark.
    checked: bool,
}

impl MenuEntry {
    /// Creates a command row with no shortcut hint and no callback.
    pub fn new(label: impl Into<SharedString>) -> Self {
        Self {
            label: label.into(),
            shortcut: None,
            on_activate: None,
            separator: false,
            disabled: false,
            checked: false,
        }
    }

    /// Creates a horizontal rule between two groups of commands.
    pub fn separator() -> Self {
        Self {
            label: SharedString::default(),
            shortcut: None,
            on_activate: None,
            separator: true,
            disabled: false,
            checked: false,
        }
    }

    /// Sets the shortcut hint shown at the right edge of the row.
    ///
    /// The hint is decoration only: the key binding itself is registered by the
    /// application, and the menu dispatches the same action the binding does.
    pub fn shortcut(mut self, shortcut: impl Into<SharedString>) -> Self {
        self.shortcut = Some(shortcut.into());
        self
    }

    /// Sets the callback run when the row is clicked.
    ///
    /// The menu closes itself afterwards, so the callback does not have to.
    pub fn on_activate(mut self, handler: impl Fn(&mut Window, &mut App) + 'static) -> Self {
        self.on_activate = Some(Rc::new(handler));
        self
    }

    /// Shows the row without letting it be run.
    ///
    /// A disabled row is drawn muted, takes no hover and no pointer cursor, and
    /// carries no click handler at all — clicking it runs nothing *and* leaves
    /// the menu open, since the panel occludes the backdrop that a press
    /// otherwise dismisses the menu from. That is the point of showing the row
    /// rather than dropping it: a command that is missing tells the reader
    /// nothing, while one that is greyed out says the surface has it and this
    /// is not the moment.
    pub fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }

    /// Marks the row as the one that is currently in effect.
    ///
    /// For the menus that show a choice rather than an action — which tab is
    /// showing, which sort a column is under. The mark sits in a column of its
    /// own before the label, and that column is laid out on *every* row of a
    /// menu that has a checked one, so the labels stay in a line; a menu with
    /// nothing checked never gets the column, and is drawn exactly as it was
    /// before the slot existed.
    pub fn checked(mut self, checked: bool) -> Self {
        self.checked = checked;
        self
    }
}

/// How wide a dropdown panel draws itself.
enum PanelWidth {
    /// One width whatever is in it, for a menu that hangs off a fixed trigger.
    Fixed(f32),
    /// As wide as the widest row, held between two bounds, for a menu built for
    /// a single right-click on a surface whose commands vary.
    Content {
        /// Floor, so a two-word menu is still a menu.
        min: f32,
        /// Ceiling, past which a label is truncated with an ellipsis.
        max: f32,
    },
}

/// Builds the full-window sheet that sits under an open menu.
///
/// A pointer press anywhere it can see dismisses the menu — either mouse button,
/// so that a right-click outside is not swallowed without effect. The panel is
/// drawn above it and occludes it, so presses on a row never reach here.
///
/// Callers wrap this in `anchored`, whose positions are window-relative by
/// default, so the sheet covers the window rather than the caller's own box.
fn menu_backdrop(
    id: ElementId,
    viewport: Size<Pixels>,
    on_dismiss: Option<DismissHandler>,
) -> AnyElement {
    div()
        .id(id)
        .w(viewport.width)
        .h(viewport.height)
        .occlude()
        .when_some(on_dismiss, |this, dismiss| {
            this.on_any_mouse_down(move |_, window, cx| dismiss(window, cx))
        })
        .into_any_element()
}

/// Builds the floating panel listing `entries`.
///
/// Opaque on purpose: a translucent window allows only one tinted fill per
/// pixel, and the terminal surface underneath already owns it.
fn menu_panel(
    id: ElementId,
    entries: Vec<MenuEntry>,
    width: PanelWidth,
    on_dismiss: Option<DismissHandler>,
    theme: &Theme,
) -> AnyElement {
    // The check column is decided for the menu, not for the row: a mark that
    // only indented its own row would leave the labels stepping sideways down
    // the list.
    let checkable = entries.iter().any(|entry| entry.checked);
    // Only a panel that may run out of room truncates. A fixed-width one was
    // measured for its own rows, and an ellipsis there would be a regression
    // rather than a fallback.
    let truncates = matches!(width, PanelWidth::Content { .. });

    let row_theme = theme.clone();
    let rows = entries.into_iter().enumerate().map(move |(index, entry)| {
        let theme = &row_theme;
        if entry.separator {
            return div()
                .id(ElementId::from(("menu-separator", index)))
                .flex_none()
                .h(px(1.))
                .my(px(4.))
                .mx(px(6.))
                .bg(theme.border);
        }

        let on_dismiss = on_dismiss.clone();
        let MenuEntry {
            label,
            shortcut,
            on_activate,
            disabled,
            checked,
            ..
        } = entry;

        div()
            .id(ElementId::from(("menu-entry", index)))
            .flex()
            .flex_row()
            .flex_none()
            .items_center()
            .gap(px(16.))
            .h(px(28.))
            .px(px(10.))
            .mx(px(4.))
            .rounded_sm()
            .text_size(px(13.))
            .text_color(if disabled {
                theme.text_muted
            } else {
                theme.text
            })
            // Everything that makes a row look and behave like a control hangs
            // off this one condition, so a disabled row is inert by having no
            // handler rather than by having one that thinks better of it.
            .when(!disabled, |this| {
                this.cursor_pointer()
                    .hover(|style| style.bg(theme.surface_hover))
                    .on_click(move |_, window, cx| {
                        if let Some(activate) = on_activate.clone() {
                            activate(window, cx);
                        }
                        if let Some(dismiss) = on_dismiss.clone() {
                            dismiss(window, cx);
                        }
                    })
            })
            .when(checkable, |this| {
                this.child(
                    div()
                        .flex_none()
                        .w(px(CHECK_WIDTH))
                        .whitespace_nowrap()
                        .child(if checked { CHECK_MARK } else { "" }),
                )
            })
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .whitespace_nowrap()
                    .when(truncates, |this| this.text_ellipsis())
                    .child(label),
            )
            .children(shortcut.map(|shortcut| {
                div()
                    .flex_none()
                    .text_size(px(11.))
                    .text_color(theme.text_muted)
                    .whitespace_nowrap()
                    .child(shortcut)
            }))
    });

    let panel = div()
        .id(id)
        .occlude()
        .flex()
        .flex_col()
        .flex_none()
        .py(px(4.))
        .bg(theme.background)
        .border_1()
        .border_color(theme.border)
        .rounded_lg()
        .shadow_lg()
        .text_color(theme.text)
        .debug_selector(|| PANEL_SELECTOR.to_string());

    match width {
        PanelWidth::Fixed(width) => panel.w(px(width)),
        PanelWidth::Content { min, max } => panel.min_w(px(min)).max_w(px(max)),
    }
    .children(rows)
    .into_any_element()
}

/// A menu opened at a point of the caller's choosing, with no trigger of its
/// own.
///
/// Rendered by the view that owns the pointer position — typically from an
/// `on_mouse_down(MouseButton::Right, …)` handler that stored the event's
/// window-space position. The element takes no space in its parent's layout, so
/// it can be dropped in anywhere the view already renders:
///
/// ```ignore
/// ContextMenu::new("tab-context")
///     .position(position)
///     .entries(vec![
///         MenuEntry::new("Close tab"),
///         MenuEntry::new("Close others").disabled(only_tab),
///     ])
///     .on_dismiss(|_window, cx| { /* clear the stored position */ })
/// ```
///
/// The panel is as wide as its widest row, held between a floor and a ceiling,
/// because the commands a right-click offers depend on what was under the
/// pointer: two surfaces of the same window can want very differently sized
/// menus, and neither of them is a fixed trigger's own.
#[derive(IntoElement)]
pub struct ContextMenu {
    id: ElementId,
    position: Point<Pixels>,
    entries: Vec<MenuEntry>,
    on_dismiss: Option<DismissHandler>,
}

impl ContextMenu {
    /// Creates an empty menu anchored at the window's top-left corner.
    ///
    /// `id` must be unique among the siblings of the menu.
    pub fn new(id: impl Into<ElementId>) -> Self {
        Self {
            id: id.into(),
            position: point(px(0.), px(0.)),
            entries: Vec::new(),
            on_dismiss: None,
        }
    }

    /// Puts the top-left corner of the panel at `position`, in window
    /// coordinates.
    ///
    /// A panel that would hang off an edge is pulled back inside the window
    /// instead.
    pub fn position(mut self, position: Point<Pixels>) -> Self {
        self.position = position;
        self
    }

    /// Sets the rows of the menu, in display order.
    pub fn entries(mut self, entries: Vec<MenuEntry>) -> Self {
        self.entries = entries;
        self
    }

    /// Called when the menu should go away: after a row is activated, or when
    /// the pointer goes down outside the panel.
    pub fn on_dismiss(mut self, handler: impl Fn(&mut Window, &mut App) + 'static) -> Self {
        self.on_dismiss = Some(Rc::new(handler));
        self
    }
}

impl RenderOnce for ContextMenu {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = theme(cx);
        let viewport = window.viewport_size();
        let backdrop = menu_backdrop(
            ElementId::from((self.id.clone(), "backdrop")),
            viewport,
            self.on_dismiss.clone(),
        );
        let panel = menu_panel(
            ElementId::from((self.id.clone(), "panel")),
            self.entries,
            PanelWidth::Content {
                min: PANEL_MIN_WIDTH,
                max: PANEL_MAX_WIDTH,
            },
            self.on_dismiss,
            &theme,
        );

        // Absolutely positioned and zero-sized: both children are `anchored` in
        // window coordinates, so this box only has to stay out of the way of the
        // layout it is dropped into.
        div()
            .id(self.id)
            .absolute()
            .w(px(0.))
            .h(px(0.))
            .child(
                deferred(anchored().position(point(px(0.), px(0.))).child(backdrop))
                    .with_priority(BACKDROP_PRIORITY),
            )
            .child(
                deferred(
                    anchored()
                        .anchor(Corner::TopLeft)
                        .position(self.position)
                        .snap_to_window_with_margin(px(WINDOW_MARGIN))
                        .child(panel),
                )
                .with_priority(PANEL_PRIORITY),
            )
    }
}

/// A toolbar button with a dropdown menu.
///
/// ```ignore
/// MenuButton::new("app-menu")
///     .open(self.menu_open)
///     .entries(vec![MenuEntry::new("New session").shortcut("Ctrl+T")])
///     .on_open_change(|open, _window, cx| { /* store `open` */ })
/// ```
#[derive(IntoElement)]
pub struct MenuButton {
    id: ElementId,
    glyph: SharedString,
    icon: Option<SharedString>,
    tooltip: Option<SharedString>,
    open: bool,
    entries: Vec<MenuEntry>,
    on_open_change: Option<OpenChangeHandler>,
}

impl MenuButton {
    /// Creates a closed menu button showing the default hamburger glyph.
    ///
    /// `id` must be unique among the siblings of the button.
    pub fn new(id: impl Into<ElementId>) -> Self {
        Self {
            id: id.into(),
            glyph: SharedString::new_static("\u{2630}"),
            icon: None,
            tooltip: None,
            open: false,
            entries: Vec::new(),
            on_open_change: None,
        }
    }

    /// Replaces the glyph drawn on the trigger button.
    pub fn glyph(mut self, glyph: impl Into<SharedString>) -> Self {
        self.glyph = glyph.into();
        self
    }

    /// Draws the asset at `path` on the trigger instead of the glyph.
    ///
    /// A second way to dress the same button rather than a replacement for
    /// [`MenuButton::glyph`], because the two triggers in the application want
    /// different things: the application menu's `☰` is a character every font
    /// has and needs no asset, while a chevron drawn as text lands at whatever
    /// size and baseline the font feels like. Callers hand over the path rather
    /// than an element, so this module keeps knowing nothing about the icon set.
    pub fn icon(mut self, path: impl Into<SharedString>) -> Self {
        self.icon = Some(path.into());
        self
    }

    /// Sets the label shown when the pointer rests on the trigger.
    ///
    /// Taken as text rather than looked up here: this layer holds no strings of
    /// its own, so the localised sentence comes from the view that builds the
    /// button.
    pub fn tooltip(mut self, tooltip: impl Into<SharedString>) -> Self {
        self.tooltip = Some(tooltip.into());
        self
    }

    /// Sets whether the dropdown is currently shown.
    pub fn open(mut self, open: bool) -> Self {
        self.open = open;
        self
    }

    /// Sets the rows of the dropdown, in display order.
    pub fn entries(mut self, entries: Vec<MenuEntry>) -> Self {
        self.entries = entries;
        self
    }

    /// Called with the open state the menu would like to be in.
    ///
    /// Fires with `true` when the trigger is clicked while closed, and with
    /// `false` when the trigger is clicked again, when a row is activated, or
    /// when the pointer goes down anywhere outside the panel.
    pub fn on_open_change(
        mut self,
        handler: impl Fn(bool, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_open_change = Some(Rc::new(handler));
        self
    }
}

impl RenderOnce for MenuButton {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = theme(cx);
        let viewport = window.viewport_size();
        let open = self.open;
        let on_open_change = self.on_open_change;

        let on_dismiss: Option<DismissHandler> = on_open_change.clone().map(|handler| {
            Rc::new(move |window: &mut Window, cx: &mut App| handler(false, window, cx))
                as DismissHandler
        });

        // A trigger only ever wears a mark — an icon, or the hamburger glyph
        // standing in for one — never a word, so its resting colour is the
        // theme's icon tint rather than the muted text a label would take.
        let tint = if open { theme.text } else { theme.icon };
        let hover_tint = theme.text;
        // An SVG takes its colour from its own `text_color`, which — unlike a
        // glyph's — does not inherit from the button, so the open and hover
        // shades have to be handed to it directly.
        let face = match self.icon.clone() {
            Some(path) => svg()
                .size(px(TRIGGER_ICON))
                .flex_none()
                .path(path)
                .text_color(tint)
                .group_hover(TRIGGER_GROUP, move |style| style.text_color(hover_tint))
                .into_any_element(),
            None => self.glyph.clone().into_any_element(),
        };

        let trigger = div()
            .id(ElementId::from((self.id.clone(), "trigger")))
            // The trigger may sit inside a window drag area — the toolbar
            // doubles as the title bar in the custom style — and occluding is
            // what keeps a click on it from being read as "move the window".
            .occlude()
            .group(TRIGGER_GROUP)
            .flex()
            .flex_none()
            .items_center()
            .justify_center()
            .size(px(TRIGGER_SIZE))
            .rounded_md()
            .text_size(px(14.))
            .text_color(tint)
            .bg(if open {
                theme.surface_active
            } else {
                gpui::transparent_black()
            })
            .cursor_pointer()
            .hover(|style| style.bg(theme.surface_hover).text_color(theme.text))
            .when_some(self.tooltip.clone(), |this, tooltip| {
                this.tooltip(tooltip_label(tooltip))
            })
            .when_some(on_open_change.clone(), |this, handler| {
                this.on_click(move |_, window, cx| handler(!open, window, cx))
            })
            .child(face);

        // A full-window sheet under the panel, deferred so that it covers the
        // whole window rather than just the toolbar row this button sits in.
        let backdrop = menu_backdrop(
            ElementId::from((self.id.clone(), "backdrop")),
            viewport,
            on_dismiss.clone(),
        );

        let panel = menu_panel(
            ElementId::from((self.id.clone(), "panel")),
            self.entries,
            PanelWidth::Fixed(PANEL_WIDTH),
            on_dismiss,
            &theme,
        );

        // The dropdown hangs off a zero-width box laid out *before* the
        // trigger, not off the trigger itself. An `anchored` element is
        // absolutely positioned, and an absolutely positioned box is aligned by
        // its parent's `align-items`; hanging it directly in the `items_center`
        // row would centre the whole panel on the 28px button instead of
        // starting it at the button's top-left corner. This box neither centres
        // its children nor takes up space, so the panel starts exactly one
        // [`DROP_OFFSET`] below the top-left of the trigger.
        let overlays = div()
            .flex()
            .flex_col()
            .flex_none()
            .w(px(0.))
            .h(px(TRIGGER_SIZE))
            .child(
                deferred(anchored().position(point(px(0.), px(0.))).child(backdrop))
                    .with_priority(BACKDROP_PRIORITY),
            )
            .child(
                deferred(
                    anchored()
                        .anchor(Corner::TopLeft)
                        .offset(point(px(0.), px(DROP_OFFSET)))
                        .snap_to_window_with_margin(px(WINDOW_MARGIN))
                        .child(panel),
                )
                .with_priority(PANEL_PRIORITY),
            );

        div()
            .id(self.id)
            .flex()
            .flex_row()
            .flex_none()
            .items_center()
            .children(open.then_some(overlays))
            .child(trigger)
    }
}

#[cfg(test)]
mod tests {
    use std::cell::{Cell, RefCell};
    use std::ops::Deref;

    use gpui::{
        Context, Modifiers, MouseButton, MouseDownEvent, MouseUpEvent, Render, TestAppContext,
        VisualTestContext,
    };

    use super::*;

    /// Where the harness anchors the context menu: far enough from every edge
    /// that nothing is snapped back inside, so the row arithmetic below holds.
    const MENU_X: f32 = 100.;

    /// Top of the context menu panel, for the same reason.
    const MENU_Y: f32 = 50.;

    /// Height of one command row, as `menu_panel` lays it out.
    const ROW_HEIGHT: f32 = 28.;

    /// What the panel puts above its first row: its border and its padding.
    const PANEL_TOP: f32 = 5.;

    /// Gap between the cells of a row, as `menu_panel` sets it.
    const ROW_GAP: f32 = 16.;

    /// A column inside every panel these tests draw, past the check column.
    const INSIDE_THE_PANEL: f32 = MENU_X + 60.;

    /// A point no panel covers.
    const OUTSIDE: f32 = 10.;

    /// A label short enough that the panel would rather be at its floor.
    const SHORT: &str = "Cut";

    /// A label long enough to push a panel past its floor and leave room for a
    /// check column before it reaches the ceiling.
    const MEDIUM: &str = "Copy as INSERT statement";

    /// A label no panel can hold, so the ceiling decides instead.
    const LONG: &str = "Copy the fully qualified name of every selected column";

    /// One row of the menu a test asks for.
    ///
    /// [`MenuEntry`] owns callbacks and cannot be cloned, so the harness keeps
    /// the description of a row and builds the entry again on every draw, the
    /// way a real view rebuilds its menu from its own state.
    #[derive(Clone)]
    struct Row {
        label: SharedString,
        disabled: bool,
        checked: bool,
    }

    impl Row {
        /// An ordinary command row.
        fn new(label: &'static str) -> Self {
            Self {
                label: SharedString::new_static(label),
                disabled: false,
                checked: false,
            }
        }

        /// The same row, greyed out.
        fn disabled(mut self) -> Self {
            self.disabled = true;
            self
        }

        /// The same row, marked as the one in effect.
        fn checked(mut self) -> Self {
            self.checked = true;
            self
        }
    }

    /// Which of the two menus a test is looking at. Both draw the same panel.
    #[derive(Clone, Copy, PartialEq)]
    enum Surface {
        /// A [`ContextMenu`] at a fixed point, always open.
        Context,
        /// A [`MenuButton`] whose dropdown is held open.
        Button,
    }

    /// A view holding one open menu, as the surface that owns a right-click
    /// would.
    struct Harness {
        surface: Surface,
        rows: Vec<Row>,
        activated: Rc<RefCell<Vec<usize>>>,
        dismissed: Rc<Cell<usize>>,
    }

    impl Harness {
        /// The entries as the menu wants them, wired to the tallies.
        fn entries(&self) -> Vec<MenuEntry> {
            self.rows
                .iter()
                .enumerate()
                .map(|(index, row)| {
                    let activated = self.activated.clone();
                    MenuEntry::new(row.label.clone())
                        .disabled(row.disabled)
                        .checked(row.checked)
                        .on_activate(move |_, _| activated.borrow_mut().push(index))
                })
                .collect()
        }
    }

    impl Render for Harness {
        fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
            let dismissed = self.dismissed.clone();
            let entries = self.entries();

            div().size_full().child(match self.surface {
                Surface::Context => ContextMenu::new("menu")
                    .position(point(px(MENU_X), px(MENU_Y)))
                    .entries(entries)
                    .on_dismiss(move |_, _| dismissed.set(dismissed.get() + 1))
                    .into_any_element(),
                Surface::Button => MenuButton::new("menu")
                    .open(true)
                    .entries(entries)
                    .on_open_change(move |open, _, _| {
                        if !open {
                            dismissed.set(dismissed.get() + 1);
                        }
                    })
                    .into_any_element(),
            })
        }
    }

    /// What a test reads back out of a running harness.
    struct Handles {
        activated: Rc<RefCell<Vec<usize>>>,
        dismissed: Rc<Cell<usize>>,
    }

    impl Handles {
        /// The rows run since the last look.
        fn drain(&self) -> Vec<usize> {
            self.activated.borrow_mut().drain(..).collect()
        }

        /// How many times the menu has asked to close.
        fn dismissals(&self) -> usize {
            self.dismissed.get()
        }
    }

    /// Opens a window on `surface` showing `rows`.
    fn open(
        surface: Surface,
        rows: Vec<Row>,
        cx: &mut TestAppContext,
    ) -> (Handles, VisualTestContext) {
        cx.update(crate::init);

        let handles = Handles {
            activated: Rc::new(RefCell::new(Vec::new())),
            dismissed: Rc::new(Cell::new(0)),
        };
        let window = cx.add_window({
            let activated = handles.activated.clone();
            let dismissed = handles.dismissed.clone();
            move |_, _| Harness {
                surface,
                rows,
                activated,
                dismissed,
            }
        });
        let cx = VisualTestContext::from_window(*window.deref(), cx);
        cx.run_until_parked();

        (handles, cx)
    }

    /// The middle of row `index` of the context menu, on the label.
    fn row_middle(index: usize) -> Point<Pixels> {
        point(
            px(INSIDE_THE_PANEL),
            px(MENU_Y + PANEL_TOP + ROW_HEIGHT * index as f32 + ROW_HEIGHT / 2.),
        )
    }

    /// Presses and releases the left button over `position`.
    fn click(cx: &mut VisualTestContext, position: Point<Pixels>) {
        cx.simulate_event(MouseDownEvent {
            position,
            modifiers: Modifiers::none(),
            button: MouseButton::Left,
            click_count: 1,
            first_mouse: false,
        });
        cx.simulate_event(MouseUpEvent {
            position,
            modifiers: Modifiers::none(),
            button: MouseButton::Left,
            click_count: 1,
        });
        cx.run_until_parked();
    }

    /// How wide the panel came out.
    fn panel_width(cx: &mut VisualTestContext) -> f32 {
        cx.debug_bounds(PANEL_SELECTOR)
            .expect("the panel is drawn")
            .size
            .width
            .into()
    }

    /// The two halves of what disabled means: the callback never runs, and the
    /// menu is still there afterwards — a row that did nothing *and* closed the
    /// menu would read as a command that silently failed.
    #[gpui::test]
    fn a_disabled_row_runs_nothing_and_leaves_the_menu_open(cx: &mut TestAppContext) {
        let (menu, mut cx) = open(
            Surface::Context,
            vec![Row::new(MEDIUM), Row::new(MEDIUM).disabled()],
            cx,
        );

        click(&mut cx, row_middle(1));
        assert_eq!(menu.drain(), Vec::<usize>::new());
        assert_eq!(menu.dismissals(), 0, "the menu stays where it is");

        // The row above it, which is the same in every way but enabled, still
        // runs and still closes the menu.
        click(&mut cx, row_middle(0));
        assert_eq!(menu.drain(), vec![0]);
        assert_eq!(menu.dismissals(), 1);

        // And the backdrop under the panel still dismisses, as it did before
        // any of this: only the panel swallows presses.
        click(&mut cx, point(px(OUTSIDE), px(OUTSIDE)));
        assert_eq!(menu.drain(), Vec::<usize>::new());
        assert_eq!(menu.dismissals(), 2);
    }

    /// A checked row is still a command: the mark says which one is in effect,
    /// it does not stand in for the click.
    #[gpui::test]
    fn a_checked_row_still_runs(cx: &mut TestAppContext) {
        let (menu, mut cx) = open(
            Surface::Context,
            vec![Row::new(MEDIUM).checked(), Row::new(MEDIUM)],
            cx,
        );

        click(&mut cx, row_middle(0));
        assert_eq!(menu.drain(), vec![0]);
        assert_eq!(menu.dismissals(), 1);
    }

    /// The check column is the menu's, not the row's: one checked row widens
    /// every row by the column and the gap after it, and a menu with none is
    /// laid out exactly as it was before the slot existed.
    #[gpui::test]
    fn the_check_column_is_laid_out_for_the_whole_menu(cx: &mut TestAppContext) {
        let rows = vec![Row::new(MEDIUM), Row::new(MEDIUM)];
        let (_, mut bare_cx) = open(Surface::Context, rows.clone(), cx);
        let bare = panel_width(&mut bare_cx);

        let checked = vec![rows[0].clone(), rows[1].clone().checked()];
        let (_, mut checked_cx) = open(Surface::Context, checked, cx);

        assert_eq!(
            panel_width(&mut checked_cx) - bare,
            CHECK_WIDTH + ROW_GAP,
            "the column is laid out once for the menu, on the checked row and \
             the unchecked one alike"
        );
    }

    /// A context menu is measured by what is in it, because what is in it
    /// depends on what was right-clicked — with a floor and a ceiling, so that
    /// neither a two-word menu nor a sentence decides the whole layout.
    #[gpui::test]
    fn a_context_menu_follows_its_content_between_its_bounds(cx: &mut TestAppContext) {
        let (_, mut short_cx) = open(Surface::Context, vec![Row::new(SHORT)], cx);
        assert_eq!(panel_width(&mut short_cx), PANEL_MIN_WIDTH);

        let (_, mut medium_cx) = open(Surface::Context, vec![Row::new(MEDIUM)], cx);
        let medium = panel_width(&mut medium_cx);
        assert!(
            medium > PANEL_MIN_WIDTH && medium < PANEL_MAX_WIDTH,
            "a menu that fits is drawn at its own width, not at a bound: {medium}"
        );

        // The longest row decides, not the last one.
        let (_, mut mixed_cx) = open(
            Surface::Context,
            vec![Row::new(SHORT), Row::new(MEDIUM), Row::new(SHORT)],
            cx,
        );
        assert_eq!(panel_width(&mut mixed_cx), medium);

        let (_, mut long_cx) = open(Surface::Context, vec![Row::new(LONG)], cx);
        assert_eq!(panel_width(&mut long_cx), PANEL_MAX_WIDTH);
    }

    /// The button's dropdown is the same panel and takes none of that: it hangs
    /// under a trigger that never moves, and was measured for its own rows.
    #[gpui::test]
    fn a_menu_button_panel_keeps_its_fixed_width(cx: &mut TestAppContext) {
        let (_, mut short_cx) = open(Surface::Button, vec![Row::new(SHORT)], cx);
        assert_eq!(panel_width(&mut short_cx), PANEL_WIDTH);

        let (_, mut long_cx) = open(
            Surface::Button,
            vec![Row::new(LONG), Row::new(MEDIUM).checked()],
            cx,
        );
        assert_eq!(panel_width(&mut long_cx), PANEL_WIDTH);
    }
}
