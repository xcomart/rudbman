//! View.

use super::*;

/// Renders one pane's tab strip.
///
/// The same widget as the connection strip at the top of the window, minus the
/// dropdown and the "+": a pane's tabs are opened from the explorer and the
/// query commands, so a "new tab" button here would have nothing to open.
///
/// Every tab of the strip belongs to the connection whose tab is on top — the
/// work area is that connection's — so the dots all carry one colour, the one
/// the explorer marks that connection's root with. That is the point of keeping
/// them: with several connections open, a glance at a pane says which database
/// its panels are about without reading the strip above the window.
pub(super) fn render_tab_strip(
    id: PaneId,
    pane: &Pane,
    chrome: &PaneChrome,
    cx: &mut Context<Workspace>,
) -> TabBar {
    let this = cx.entity();
    let tabs: Vec<TabItem> = pane
        .items()
        .iter()
        .enumerate()
        .map(|(index, item)| {
            let tab = TabItem::new(("pane-tab", index), item.title(cx));
            match chrome.colors.get(&item.connection(cx)) {
                Some(color) => tab.dot(*color),
                None => tab,
            }
        })
        .collect();

    TabBar::new(("pane-tabs", id.as_u64()))
        .tabs(tabs)
        .active(pane.active_index())
        .scroll_handle(pane.scroll_handle())
        // Only the third slot can ever be read: this strip carries neither the
        // dropdown nor the "+" the first two label.
        .tooltips("", "", ts!("tab.close"))
        .on_select({
            let this = this.clone();
            move |index, window, cx| {
                this.update(cx, |workspace, cx| {
                    workspace.activate_tab(id, index, window, cx);
                });
            }
        })
        .on_close({
            let this = this.clone();
            move |index, window, cx| {
                this.update(cx, |workspace, cx| {
                    workspace.close_tab(id, index, window, cx);
                });
            }
        })
        // Only over a tab. The empty stretch of a strip is where a title bar
        // gesture would otherwise be — on Linux a right-click there is the
        // window menu — and the widget answers for tabs alone for exactly that
        // reason.
        .on_context_menu(move |index, position, _window, cx| {
            this.update(cx, |workspace, cx| {
                workspace.open_context_menu(
                    ContextTarget::PaneTab { pane: id, index },
                    position,
                    cx,
                );
            });
        })
}

/// The gpui axis a pane tree's own [`Axis`] stands for.
///
/// The tree's axis is the shell's, an enum of its own so that a saved layout
/// does not depend on a gpui type; the splitter wants gpui's. Two names for
/// one idea, and this is the one line that says so.
pub(super) fn gpui_axis(axis: Axis) -> gpui::Axis {
    match axis {
        Axis::Horizontal => gpui::Axis::Horizontal,
        Axis::Vertical => gpui::Axis::Vertical,
    }
}

/// Renders one node of a pane tree.
///
/// A split becomes a [`rugpui::Splitter`] along its axis, given the ratio the
/// tree holds and writing the dragged one back through
/// [`Workspace::move_split`]. Nothing about the layout is this function's
/// business any more: the widget owns the two flex bases, the grab band over
/// the seam and the guards on the number.
///
/// When `frame` is set — the work area holds more than one pane — every leaf is
/// framed with a hairline, accent coloured on the active one. The frames double
/// as the divider between neighbours, which is why there is no separate divider
/// element: a third hairline squeezed between two of them would only thicken the
/// seam. Every pane is framed, not just the active one, so that moving the
/// marker recolours a frame without shifting the layout by a pixel. It is a
/// border rather than a fill because a translucent window allows only one tinted
/// fill per pixel and the body already owns it. They are also why every
/// splitter here is `seamless`: a split only exists where there is more than
/// one pane, so the two frames meeting on the divider have already drawn the
/// line, and the widget's own would be a third hairline on the same seam.
pub(super) fn render_pane(
    node: &PaneNode<Pane>,
    chrome: &PaneChrome,
    cx: &mut Context<Workspace>,
) -> AnyElement {
    match node {
        PaneNode::Leaf { id, payload } => {
            let theme = &chrome.theme;
            let border = if *id == chrome.active {
                theme.accent
            } else {
                theme.border
            };
            let body = match payload.active() {
                Some(PaneItem::TableDetail(panel)) => panel.clone().into_any_element(),
                Some(PaneItem::Query { pane, .. }) => pane.clone().into_any_element(),
                Some(PaneItem::Erd(panel)) => panel.clone().into_any_element(),
                Some(PaneItem::TableData(panel)) => panel.clone().into_any_element(),
                Some(PaneItem::TableStruct(panel)) => panel.clone().into_any_element(),
                Some(PaneItem::QueryBuilder { pane, .. }) => pane.clone().into_any_element(),
                // A work area belongs to a connection, so a pane inside one is
                // empty because nothing that would fill it has been opened yet
                // — never because there is nothing to connect to.
                None => render_placeholder(theme),
            };
            div()
                .id(("pane", id.as_u64()))
                .flex()
                .flex_col()
                .size_full()
                .min_w_0()
                .min_h_0()
                .when(chrome.frame, |pane| pane.border_1().border_color(border))
                // No strip over an empty pane: a bar with nothing in it would
                // be a band of chrome saying nothing over the very words that
                // explain what the pane is for.
                .children((!payload.is_empty()).then(|| {
                    div()
                        .flex()
                        .flex_none()
                        .w_full()
                        .child(render_tab_strip(*id, payload, chrome, cx))
                }))
                .child(div().flex().flex_1().min_w_0().min_h_0().child(body))
                .into_any_element()
        }
        PaneNode::Split {
            id,
            axis,
            ratio,
            first,
            second,
        } => {
            let id = *id;
            // Both children are rendered up front because each one needs `cx`
            // for the splitters further down the tree, and a closure holding it
            // could not then be called twice.
            let first = render_pane(first, chrome, cx);
            let second = render_pane(second, chrome, cx);
            // A [`SplitId`] is unique within the tree and only one work area is
            // on screen at a time, which is what the splitter asks of an id:
            // nesting makes every enclosing divider hear an inner one's drag,
            // and the id is how each of them tells its own gesture apart.
            Splitter::new(("split", id.as_u64()), gpui_axis(*axis))
                .ratio(*ratio)
                .seamless()
                .first(first)
                .second(second)
                .on_change(cx.processor(move |workspace, ratio, _window, cx| {
                    workspace.move_split(id, ratio, cx);
                }))
                .into_any_element()
        }
    }
}

/// Renders the empty state of a pane with no tabs.
///
/// The wording is the opposite of the welcome screen's, and the difference
/// matters: here the connection is live and the pane is empty because nothing
/// has been opened into it yet, so the words point at the explorer beside it
/// rather than at the connection dialog. One wording for both states would have
/// a live tab sitting above the words "no connections".
///
/// Text only, and no button. There is nothing here a single command would do —
/// what fills a pane is whatever the user picks out of the tree — whereas the
/// window with no connection at all has exactly one next step and
/// [`Workspace::render_welcome`] offers it as a button.
///
/// It paints no fill of its own. The work area behind it already carries the
/// tinted fill for these pixels, and a second one here would compose back to
/// opaque; see [`app_settings::window_tint`].
pub(super) fn render_placeholder(theme: &Theme) -> AnyElement {
    let (title, hint) = (ts!("empty.connected_title"), ts!("empty.connected_hint"));
    div()
        .flex()
        .flex_col()
        .flex_grow_1()
        .min_w_0()
        .min_h_0()
        .items_center()
        .justify_center()
        .gap(px(8.))
        .child(div().text_size(px(18.)).text_color(theme.text).child(title))
        .child(
            div()
                .text_size(px(13.))
                .text_color(theme.text_muted)
                .child(hint),
        )
        .into_any_element()
}

/// A box that keeps `content` in the middle while it fits, and lets it be
/// scrolled from the top once it does not.
///
/// `justify_center` does the first half and ruins the second. With more content
/// than room, a centred column hangs off both ends of its box, and scrolling
/// only ever reaches what lies past the *end* of one — so the head of the column
/// goes off the top edge and stays there, unreachable. Automatic margins share
/// out whatever room is spare, which centres the column exactly as `justify_center`
/// would, and collapse to nothing when there is none, which leaves the column at
/// the top with all of it below the fold and so all of it reachable.
///
/// Three boxes. The outermost is what the overlay bar hangs off, because the
/// scrolling box cannot hold it — its children are what scroll away underneath
/// it — and it is what the caller styles. Inside it is the box that scrolls,
/// and inside that the one carrying the margins and the breathing room that
/// keeps either end of the scroll off the edge.
pub(super) fn centered_scroll(
    id: &'static str,
    scroll: &ScrollHandle,
    bar: Scrollbar,
    theme: &Theme,
    content: impl IntoElement,
) -> Div {
    div()
        .relative()
        .flex()
        .flex_col()
        .flex_grow_1()
        .min_h_0()
        .child(
            div()
                .id(id)
                .track_scroll(scroll)
                .flex()
                .flex_col()
                .flex_grow_1()
                .min_h_0()
                .items_center()
                .overflow_y_scroll()
                .restrict_scroll_to_axis()
                .child(
                    // `flex_none` so that a column taller than the box overflows
                    // it — and is scrolled to — rather than being squeezed into
                    // it, which is what a flex item does by default.
                    div()
                        .flex()
                        .flex_col()
                        .flex_none()
                        .items_center()
                        .my_auto()
                        .py(px(SCROLL_MARGIN))
                        .child(content),
                ),
        )
        .children(bar.render(theme))
}

impl Render for Workspace {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = theme(cx);
        // The one place the interface font size is read: everything below
        // inherits it unless it sets a size of its own, which is what makes the
        // setting — and the settings dialog's live preview of it — visible.
        let ui_font_size = app_settings::effective(cx).ui_font_size;
        self.watch_scroll(cx);
        let toolbar = self.render_toolbar(window, cx);
        let body = self.render_body(cx);
        let status_bar = self.render_status_bar(cx);
        let about = self
            .about
            .read(cx)
            .is_open()
            .then(|| div().absolute().inset_0().child(self.about.clone()));
        let connect = self
            .connect
            .read(cx)
            .is_open()
            .then(|| div().absolute().inset_0().child(self.connect.clone()));
        let settings = self
            .settings
            .read(cx)
            .is_open()
            .then(|| div().absolute().inset_0().child(self.settings.clone()));
        let extract = self
            .extract
            .read(cx)
            .is_open()
            .then(|| div().absolute().inset_0().child(self.extract.clone()));
        let transfer = self
            .transfer
            .read(cx)
            .is_open()
            .then(|| div().absolute().inset_0().child(self.transfer.clone()));
        let backup = self
            .backup
            .read(cx)
            .is_open()
            .then(|| div().absolute().inset_0().child(self.backup.clone()));
        let update = self
            .update
            .read(cx)
            .is_open()
            .then(|| div().absolute().inset_0().child(self.update.clone()));
        let confirm = self.render_confirm(cx);
        let context_menu = self.render_context_menu(cx);

        // With client-side decorations the compositor stops drawing the drop
        // shadow along with the frame, so the window has to bring its own: the
        // surface grows a transparent band all round, the content is inset by
        // it, and the shadow is painted into it. The inset call keeps
        // `_GTK_FRAME_EXTENTS` in step so the compositor treats the content
        // edge, not the surface edge, as the window.
        let tiling = client_tiling(window);
        if tiling.is_some() {
            window.set_client_inset(px(SHADOW_BAND));
        } else {
            // Clears the extents a client-side frame may have left behind when
            // the setting switches back to the system title bar on a live
            // window; a no-op under decorations that never set any.
            window.set_client_inset(px(0.));
        }

        // No background fill here on purpose. The three bands below — toolbar,
        // body and status bar — cover the window between them, and each paints
        // its own. A fill at this level would sit *under* the translucent body
        // fill and compose back to opaque, which is the mistake that makes
        // `window.background_opacity` and `background_blur` look as though they
        // did nothing at all.
        let content = div()
            .key_context(KEY_CONTEXT)
            .track_focus(&self.focus_handle)
            .relative()
            .size_full()
            .flex()
            .flex_col()
            .text_color(theme.text)
            .text_size(px(ui_font_size))
            // The overlay bars are answered from here rather than from the
            // surfaces they ride: gpui hands a drag move to every listener of
            // that type wherever it sits, and the root is the one element that
            // is always mounted while a drag of one is in flight.
            .on_drag_move::<DraggedThumb>(cx.listener(
                move |workspace, event: &DragMoveEvent<DraggedThumb>, _window, cx| {
                    workspace.drag_scrollbar(event, cx);
                },
            ))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|workspace, _: &MouseUpEvent, _window, cx| {
                    workspace.release_scrollbars(cx);
                    workspace.release_explorer(cx);
                }),
            )
            .on_mouse_up_out(
                MouseButton::Left,
                cx.listener(|workspace, _: &MouseUpEvent, _window, cx| {
                    workspace.release_scrollbars(cx);
                    workspace.release_explorer(cx);
                }),
            )
            .on_action(cx.listener(Self::new_connection_action))
            .on_action(cx.listener(Self::open_settings_action))
            .on_action(cx.listener(Self::show_about_action))
            .on_action(cx.listener(Self::check_updates_action))
            .on_action(cx.listener(Self::toggle_explorer_action))
            .on_action(cx.listener(Self::new_query_action))
            .on_action(cx.listener(Self::query_object_action))
            .on_action(cx.listener(Self::open_sql_file_action))
            .on_action(cx.listener(Self::extract_script_action))
            .on_action(cx.listener(Self::transfer_table_action))
            .on_action(cx.listener(Self::backup_schema_action))
            .on_action(cx.listener(Self::open_erd_action))
            .on_action(cx.listener(Self::add_to_builder_action))
            .on_action(cx.listener(Self::new_builder_action))
            .on_action(cx.listener(Self::close_pane_action))
            .on_action(cx.listener(Self::focus_next_pane_action))
            .on_action(cx.listener(Self::focus_prev_pane_action))
            .on_action(cx.listener(Self::split_right_action))
            .on_action(cx.listener(Self::split_below_action))
            .on_action(cx.listener(Self::dismiss_dialog_action))
            .child(toolbar)
            .child(body)
            .child(status_bar)
            .children(about)
            .children(connect)
            .children(settings)
            .children(extract)
            .children(transfer)
            .children(backup)
            .children(update)
            .children(confirm)
            // Last: it paints above the dialogs, as its own backdrop already
            // implies, and it takes no room in the column — the element is an
            // empty absolute box whose two halves are anchored to the window.
            .children(context_menu);

        let Some(tiling) = tiling else {
            // A server-decorated window: the compositor frames and shadows it,
            // and the content is the whole surface.
            return content.into_any_element();
        };
        render_client_frame(
            content,
            tiling,
            theme.surface,
            theme.border,
            window.is_window_active(),
        )
        .into_any_element()
    }
}

impl Workspace {
    /// The shell's own context menu, while one is open.
    ///
    /// One element for four surfaces; which rows it carries is
    /// [`ContextTarget`]. It is rendered from the workspace root rather than
    /// from the surface each menu belongs to because the tab strips and the
    /// welcome rows are built by free functions and `RenderOnce` widgets that
    /// have nowhere to keep the state — and because the root is the one box
    /// every one of those surfaces is inside of.
    pub(super) fn render_context_menu(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let menu = self.context_menu.as_ref()?;
        let this = cx.entity();
        let rows = match &menu.target {
            ContextTarget::Explorer(node) => self.explorer_rows(node, cx),
            ContextTarget::Connection(index) => self.connection_rows(*index, cx),
            ContextTarget::PaneTab { pane, index } => self.pane_tab_rows(*pane, *index, cx),
            ContextTarget::Profile(id) => self.profile_rows(*id, cx),
        };

        Some(
            rugpui::ContextMenu::new("workspace-context")
                .position(menu.position)
                .entries(context_menu::entries(rows))
                .on_dismiss(move |_window, cx| {
                    this.update(cx, |workspace, cx| {
                        workspace.context_menu = None;
                        cx.notify();
                    });
                })
                .into_any_element(),
        )
    }

    /// Renders the toolbar: the application menu button and the tab strip.
    ///
    /// The button is left out on macOS, where [`app_menus`] puts the same
    /// commands in the system menu bar.
    ///
    /// In the custom title bar style this row *is* the title bar. It then marks
    /// itself as the window's drag area, takes over writing the application's
    /// name at its left end, and — off macOS, which keeps its native traffic
    /// lights — grows a set of caption buttons at its right end. Every *control*
    /// inside it occludes, so the drag area only ever answers for the gaps
    /// between them; see [`rugpui::window_controls`]. The name is not a
    /// control and deliberately does not.
    pub(super) fn render_toolbar(&self, window: &Window, cx: &mut Context<Self>) -> AnyElement {
        let theme = theme(cx);
        let custom = draws_own_titlebar(chrome_titlebar(self.titlebar), window);
        let titlebar_active = custom && window.is_window_active();
        let titlebar_text = if titlebar_active {
            theme.text
        } else {
            theme.text_muted
        };
        let menu = (!cfg!(target_os = "macos")).then(|| self.render_app_menu(cx));
        // Built before the row is assembled: both of these borrow the context to
        // register listeners, and the builders below borrow it to read the theme.
        let tab_bar = self.render_tab_bar(cx);

        // One cell for the leading controls, so the menu button shares the
        // toolbar's fill and bottom hairline with the strip.
        let leading = menu.map(|menu| {
            div()
                .flex()
                .flex_row()
                .flex_none()
                .items_center()
                .gap(px(2.))
                .h(px(TOOLBAR_HEIGHT))
                .px(px(4.))
                .bg(theme.surface)
                .border_b_1()
                .border_color(theme.border)
                .child(menu)
        });

        // Room for the traffic lights AppKit still draws over the transparent
        // title bar. Painted like the leading cell rather than left empty, so
        // the band reads as one strip. Fullscreen hides the buttons, and the
        // gap goes with them.
        let traffic_lights =
            (custom && cfg!(target_os = "macos") && !window.is_fullscreen()).then(|| {
                div()
                    .flex_none()
                    .w(px(TRAFFIC_LIGHT_GAP))
                    .h(px(TOOLBAR_HEIGHT))
                    .bg(theme.surface)
                    .border_b_1()
                    .border_color(theme.border)
            });

        // The application's own name, which only the custom style has to write:
        // a system title bar already carries it, and drawing it twice would put
        // it in two places at once.
        //
        // Windows and the GTK/KDE captions set an application icon beside the
        // title and macOS does not, so the mark follows that split.
        //
        // Nothing here is interactive, and — unlike every control in this row —
        // nothing here occludes either. The name and the mark are part of the
        // *empty* title bar as far as the window is concerned, so a press on
        // them has to reach the drag area underneath and move the window.
        let title = custom.then(|| {
            // Use the shipped PNG so the title bar preserves the icon's
            // colours on every platform instead of tinting its alpha mask.
            let icon = (!cfg!(target_os = "macos")).then(|| {
                let icon = img(icons::APP_ICON).size(px(16.)).flex_none();
                if !titlebar_active {
                    div()
                        .size(px(16.))
                        .flex_none()
                        .opacity(0.55)
                        .child(icon)
                        .into_any_element()
                } else {
                    icon.into_any_element()
                }
            });
            div()
                .flex()
                .flex_row()
                .flex_none()
                .items_center()
                .gap(px(6.))
                .h(px(TOOLBAR_HEIGHT))
                .px(px(10.))
                .bg(theme.surface)
                .border_b_1()
                .border_color(theme.border)
                // A shade quieter than a tab title, which is the one label in
                // this row that has to be read.
                .text_size(px(12.))
                .text_color(titlebar_text)
                .children(icon)
                .child(APP_NAME)
        });

        // The caption buttons the other two platforms have to draw themselves,
        // as the two strips a Linux desktop may ask for; see
        // [`rugpui_shell::chrome::window_control_strips`].
        let (leading_controls, trailing_controls) =
            window_control_strips(&window_control_icons(), custom, window, cx);

        div()
            .id("toolbar")
            .flex()
            .flex_row()
            .flex_none()
            .items_center()
            .w_full()
            .h(px(TOOLBAR_HEIGHT))
            .when(custom, |this| {
                // Occluding is load-bearing, not just hygiene: the workspace
                // root tracks focus, and gpui's focus transfer marks every
                // mouse down over it `default_prevented` — which the Windows
                // backend reads as "the app took this press", swallowing the
                // `HTCAPTION` down that would have started the system drag.
                // Cutting the root's hitbox out from under the strip keeps the
                // press unclaimed.
                titlebar_gestures(this.occlude().window_control_area(WindowControlArea::Drag))
            })
            // Ahead of the wordmark, which is where a desktop that asks for
            // left-hand caption buttons expects them: the buttons are the
            // window's, the name is the application's.
            .children(leading_controls)
            .children(traffic_lights)
            .children(title)
            .children(leading)
            .child(div().flex_1().min_w_0().child(tab_bar))
            .children(trailing_controls)
            .into_any_element()
    }

    /// Builds the dropdown menu shown on the platforms without a native one.
    ///
    /// Every row dispatches the action its keyboard shortcut dispatches, so the
    /// menu adds a way in rather than a second implementation.
    ///
    /// The pane commands are deliberately absent. They act on panes that hold
    /// nothing yet, so a row offering to split the empty state would promise
    /// something it cannot deliver; the shortcuts stay bound so that the layout
    /// code is exercised, and the rows arrive with the panels in M2.
    ///
    /// Every other row is greyed exactly when the action behind it would return
    /// without doing anything — the welcome screen has no session, so all but
    /// the connection, settings and about rows arrive muted. Drawn rather than
    /// dropped, for the reason the explorer's menu draws its own: a command that
    /// is missing tells the reader nothing about what the surface can do.
    pub(super) fn render_app_menu(&self, cx: &mut Context<Self>) -> MenuButton {
        let this = cx.entity();
        // The tab on screen, which is what the panes and the file picker open
        // over, and the explorer's selection, which is what the object commands
        // act on. The selection carries its own connection, so each of those is
        // gated on *that* session rather than on the active one.
        let live = self.has_live_connection();
        let relation = self.explorer.read(cx).selected_relation(cx);
        let scope = self.explorer.read(cx).selected_scope(cx);
        let relation_live = relation
            .as_ref()
            .is_some_and(|target| self.session_of(target.connection).is_some());
        let scope_live = scope
            .as_ref()
            .is_some_and(|(connection, _)| self.session_of(*connection).is_some());
        // A builder holds only its own connection's tables, so the target has to
        // belong to the tab in front as well as to a live session.
        let addable = relation_live
            && self.active_connection().map(|open| open.id)
                == relation.as_ref().map(|target| target.connection);
        let entries = vec![
            MenuEntry::new(ts!("menu.new_connection"))
                .shortcut(format!("{SHORTCUT_MODIFIER}+N"))
                .on_activate(|window, cx| window.dispatch_action(Box::new(NewConnection), cx)),
            MenuEntry::new(ts!("menu.new_query"))
                .shortcut(format!("{SHORTCUT_MODIFIER}+T"))
                .disabled(!live)
                .on_activate(|window, cx| window.dispatch_action(Box::new(NewQuery), cx)),
            MenuEntry::new(ts!("menu.query_object"))
                .shortcut(format!("{SHORTCUT_MODIFIER}+Enter"))
                .disabled(!(live && relation.is_some()))
                .on_activate(|window, cx| window.dispatch_action(Box::new(QueryObject), cx)),
            MenuEntry::new(ts!("menu.open_sql_file"))
                .shortcut(format!("{SHORTCUT_MODIFIER}+O"))
                .disabled(!live)
                .on_activate(|window, cx| window.dispatch_action(Box::new(OpenSqlFile), cx)),
            MenuEntry::new(ts!("menu.extract_script"))
                .disabled(!relation_live)
                .on_activate(|window, cx| window.dispatch_action(Box::new(ExtractScript), cx)),
            MenuEntry::new(ts!("menu.transfer_table"))
                .disabled(!relation_live)
                .on_activate(|window, cx| window.dispatch_action(Box::new(TransferTable), cx)),
            MenuEntry::new(ts!("menu.backup_schema"))
                .disabled(!scope_live)
                .on_activate(|window, cx| window.dispatch_action(Box::new(BackupSchema), cx)),
            MenuEntry::new(ts!("menu.erd"))
                .shortcut(format!("{SHORTCUT_MODIFIER}+E"))
                .disabled(!scope_live)
                .on_activate(|window, cx| window.dispatch_action(Box::new(OpenErd), cx)),
            MenuEntry::new(ts!("menu.new_builder"))
                .disabled(!live)
                .on_activate(|window, cx| window.dispatch_action(Box::new(NewBuilder), cx)),
            MenuEntry::new(ts!("menu.add_to_builder"))
                .disabled(!addable)
                .on_activate(|window, cx| window.dispatch_action(Box::new(AddToBuilder), cx)),
            MenuEntry::new(ts!("menu.toggle_explorer"))
                .shortcut(format!("{SHORTCUT_MODIFIER}+B"))
                .disabled(self.connections.is_empty())
                .on_activate(|window, cx| window.dispatch_action(Box::new(ToggleExplorer), cx)),
            MenuEntry::new(ts!("menu.settings"))
                .shortcut(format!("{SHORTCUT_MODIFIER}+,"))
                .on_activate(|window, cx| window.dispatch_action(Box::new(OpenSettings), cx)),
            MenuEntry::separator(),
            // Next to About, where a Help menu would put it and where users of
            // every other desktop application look for it.
            MenuEntry::new(ts!("menu.check_updates"))
                .on_activate(|window, cx| window.dispatch_action(Box::new(CheckUpdates), cx)),
            MenuEntry::new(ts!("menu.about"))
                .on_activate(|window, cx| window.dispatch_action(Box::new(ShowAbout), cx)),
            MenuEntry::separator(),
            MenuEntry::new(ts!("menu.quit"))
                .shortcut(format!("{SHORTCUT_MODIFIER}+Q"))
                .on_activate(|window, cx| window.dispatch_action(Box::new(Quit), cx)),
        ];

        MenuButton::new("app-menu")
            .tooltip(ts!("menu.tip_menu"))
            .open(self.menu_open)
            .entries(entries)
            .on_open_change(move |open, _window, cx| {
                this.update(cx, |workspace, cx| workspace.set_menu_open(open, cx));
            })
    }

    /// Renders the tab strip: one tab per open connection.
    ///
    /// The title is the profile's name and the dot is where the session has got
    /// to, so a tab that is still connecting, one that is live and one whose
    /// tunnel died are told apart without opening any of them.
    ///
    /// Selecting a tab is the window's one mode switch: it swaps the whole work
    /// area below and the explorer's root along with it.
    pub(super) fn render_tab_bar(&self, cx: &mut Context<Self>) -> TabBar {
        let this = cx.entity();
        let tabs: Vec<TabItem> = self
            .connections
            .iter()
            .enumerate()
            .map(|(index, connection)| {
                let title = if connection.profile.name.trim().is_empty() {
                    ts!("connect.unnamed")
                } else {
                    SharedString::from(connection.profile.name.clone())
                };
                TabItem::new(("connection", index), title).status(connection.state.tab_status())
            })
            .collect();

        TabBar::new("connection-tabs")
            .tabs(tabs)
            .active(self.active_connection)
            .on_select({
                let this = this.clone();
                move |index, window, cx| {
                    this.update(cx, |workspace, cx| {
                        workspace.select_connection(index, window, cx);
                    });
                }
            })
            .on_close({
                let this = this.clone();
                move |index, window, cx| {
                    this.update(cx, |workspace, cx| {
                        workspace.close_connection(index, window, cx)
                    });
                }
            })
            .on_context_menu({
                let this = this.clone();
                move |index, position, _window, cx| {
                    this.update(cx, |workspace, cx| {
                        workspace.open_context_menu(ContextTarget::Connection(index), position, cx);
                    });
                }
            })
            .scroll_handle(&self.tab_scroll)
            .scrollbar(self.hovering_scrollbar(SCROLLBARS[0].0, Surface::Tabs, cx))
            .menu_icon(icons::TAB_LIST)
            .new_icon(icons::NEW_TAB)
            // The close button reuses the tab menu's own row: it is the same
            // command, worded the same way, and neither takes an ellipsis.
            .tooltips(
                ts!("tab.tip_list"),
                ts!("tab.tip_new", shortcut = format!("{SHORTCUT_MODIFIER}+N")),
                ts!("tab.close"),
            )
            .on_new(|window, cx| window.dispatch_action(Box::new(NewConnection), cx))
    }

    /// Renders the work area: the sidebar, and the active connection's panes.
    ///
    /// With no connection open there is no work area to draw, so the empty state
    /// stands in for the whole of it — the same words a pane with no tabs shows,
    /// in the wording that says there is nothing to connect to.
    pub(super) fn render_body(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = theme(cx);
        let work = self.work_area().map(|area| {
            let chrome = PaneChrome {
                active: area.active(),
                // A lone pane with nothing beside it needs no frame: there is
                // only one thing on screen, so nothing has to be said about
                // which one is active.
                frame: area.panes.leaf_count() > 1,
                theme: theme.clone(),
                colors: self
                    .connections
                    .iter()
                    .filter_map(|open| {
                        let color = rugpui::parse_hex(open.profile.color.as_deref()?)?;
                        Some((open.id, color))
                    })
                    .collect(),
            };
            (area.panes.root(), chrome)
        });

        // The sidebar and, inside its own trailing edge, the handle that resizes
        // it — both left out entirely when the panel is hidden, because a
        // zero-width flex child would still take the divider's hit area with it.
        //
        // The handle is a child of the sidebar rather than a sibling of it
        // because [`rugpui::ResizeHandle`] is an absolutely positioned band and
        // so has to be measured against a `relative` box. Inside the sidebar it
        // covers the sidebar's own last six pixels, which is exactly where the
        // negative-margin sibling this replaced sat: the grab area straddles the
        // seam over the panel's own border rather than pushing the work area
        // across. Last child, so it wins the hit test against whatever the
        // explorer has drawn under it — and the widget brings the accent bar
        // that fades in under the pointer, the same one the pane tree's
        // [`rugpui::Splitter`] dividers show, so the two resize gestures of one
        // window not only feel alike but look alike.
        let sidebar = self.explorer_showing().then(|| {
            div()
                .flex()
                .relative()
                .flex_none()
                .w(px(self.explorer_width))
                .min_h_0()
                .child(self.explorer.clone())
                .child(
                    ResizeHandle::new("explorer-divider", gpui::Axis::Horizontal, DraggedExplorer)
                        .at_end()
                        .thickness(px(SPLIT_HANDLE)),
                )
        });

        // The row paints no fill of its own. Its children tile it, and each of
        // them tints its own share: the explorer's surface under the sidebar, the
        // background below over the work area. Side by side rather than stacked
        // is exactly what [`app_settings::window_tint`] requires, and it is what
        // lets the blur behind the window carry on under the sidebar too.
        div()
            .flex()
            .flex_row()
            .flex_grow_1()
            .min_w_0()
            .min_h_0()
            // Measured against this box rather than tracked as a delta, exactly
            // as a split divider is: the width follows the pointer however far
            // the gesture wandered.
            .on_drag_move::<DraggedExplorer>(cx.listener(
                |workspace, event: &DragMoveEvent<DraggedExplorer>, _window, cx| {
                    workspace.drag_explorer(event, cx);
                },
            ))
            .children(sidebar)
            .child(
                div()
                    .flex()
                    .flex_1()
                    .min_w_0()
                    .min_h_0()
                    // Everything the row is not already covering with the
                    // sidebar's own fill, and so the one fill over these pixels;
                    // see [`app_settings::window_tint`].
                    .bg(app_settings::window_tint(theme.background, cx))
                    .child(match work {
                        Some((root, chrome)) => render_pane(root, &chrome, cx),
                        None => self.render_welcome(&theme, cx),
                    }),
            )
            .into_any_element()
    }

    /// Renders the welcome screen: what the window is, with nothing open.
    ///
    /// The first screen of a first run, and the one a user comes back to every
    /// time they close their last tab, so it carries the two things there are to
    /// do from here rather than describing them: the button that makes a
    /// connection, and the connections already saved. A row of that list opens
    /// straight away — see [`Workspace::open_profile`] — which is what makes the
    /// list a way in rather than a reminder that the dialog exists.
    ///
    /// Laid out the way logman lays out its own empty state, deliberately —
    /// the two are the same author's tools and greet an empty window the same
    /// way: the application's name over one line of hint, then a fixed-width
    /// column carrying the button and, under its own small heading, the saved
    /// list. The column sits centred while it fits and scrolls from the top
    /// once it does not — [`centered_scroll`] says why those are one
    /// arrangement — under the same overlay bar the tab strip wears.
    ///
    /// It paints no fill of its own. The work area behind it already carries the
    /// tinted fill for these pixels, and a second one here would compose back
    /// to opaque; see [`app_settings::window_tint`].
    pub(super) fn render_welcome(&self, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let this = cx.entity();
        let profiles = self.profiles.connections();
        let rows = profile_rows(
            profiles,
            None,
            theme,
            {
                let this = this.clone();
                move |id, window, cx| {
                    this.update(cx, |workspace, cx| workspace.open_profile(id, window, cx));
                }
            },
            Some(std::rc::Rc::new(move |id, position, _window, cx| {
                this.update(cx, |workspace, cx| {
                    workspace.open_context_menu(ContextTarget::Profile(id), position, cx);
                });
            })),
        );

        // A first run has nothing saved and no habit of the chord yet, so the
        // line under the name says what a connection is for; once something is
        // saved it carries the shortcut that skips the button instead.
        let hint = if profiles.is_empty() {
            ts!("empty.hint")
        } else {
            ts!("welcome.hint", shortcut = format!("{SHORTCUT_MODIFIER}+N"))
        };

        let saved = (!rows.is_empty()).then(|| {
            div()
                .flex()
                .flex_col()
                .gap(px(6.))
                .w(px(WELCOME_WIDTH))
                .child(
                    div()
                        .text_size(px(11.))
                        .text_color(theme.text_muted)
                        .child(ts!("welcome.saved")),
                )
                .child(div().flex().flex_col().gap(px(1.)).children(rows))
        });

        let bar = self.hovering_scrollbar(SCROLLBARS[1].0, Surface::Welcome, cx);

        let content = div()
            .flex()
            .flex_col()
            .items_center()
            .gap(px(14.))
            .child(
                div()
                    .text_size(px(30.))
                    .text_color(theme.text)
                    .child(APP_NAME),
            )
            .child(
                div()
                    .text_size(px(13.))
                    .text_color(theme.text_muted)
                    .child(hint),
            )
            .child(
                div()
                    .w(px(WELCOME_WIDTH))
                    .debug_selector(|| WELCOME_NEW_SELECTOR.to_string())
                    .child(
                        Button::new("welcome-new", ts!("welcome.new_connection"))
                            .variant(ButtonVariant::Primary)
                            .full_width(true)
                            .tab_index(WELCOME_NEW_TAB)
                            // The same action the menu row, the tab strip's
                            // plus and Ctrl+N dispatch: one command, however
                            // it is reached.
                            .on_click(|_, window, cx| {
                                window.dispatch_action(Box::new(NewConnection), cx);
                            }),
                    ),
            )
            .children(saved);

        centered_scroll(WELCOME_STATE, &self.welcome_scroll, bar, theme, content).into_any_element()
    }

    /// Writes the sidebar's width into the settings global.
    ///
    /// Called when the drag ends; [`app_settings::save`] takes it to disk with
    /// everything else when the last window closes.
    pub(super) fn release_explorer(&mut self, cx: &mut Context<Self>) {
        let mut settings = app_settings::current(cx);
        if (settings.explorer_width - self.explorer_width).abs() > f32::EPSILON {
            settings.explorer_width = self.explorer_width;
            app_settings::replace(settings, cx);
        }
    }

    /// Writes the ratio a divider has been dragged to into the pane tree.
    ///
    /// [`rugpui::Splitter`] has already clamped it and already ruled out a
    /// `NaN`, so there is nothing left to check: the number arrives ready to
    /// store.
    pub(super) fn move_split(&mut self, split: SplitId, ratio: f32, cx: &mut Context<Self>) {
        if self
            .work_area_mut()
            .is_some_and(|area| area.panes.set_ratio(split, ratio))
        {
            cx.notify();
        }
    }

    /// One surface's scroll offset and the state of the bar over it.
    pub(super) fn surface(&mut self, surface: Surface) -> (&ScrollHandle, &mut ScrollbarState) {
        match surface {
            Surface::Tabs => (&self.tab_scroll, &mut self.tab_scrollbar),
            Surface::Welcome => (&self.welcome_scroll, &mut self.welcome_scrollbar),
        }
    }

    /// The same pair, for the paths that only read.
    pub(super) fn surface_ref(&self, surface: Surface) -> (&ScrollHandle, &ScrollbarState) {
        match surface {
            Surface::Tabs => (&self.tab_scroll, &self.tab_scrollbar),
            Surface::Welcome => (&self.welcome_scroll, &self.welcome_scrollbar),
        }
    }

    /// One surface's overlay scroll indicator, as it stands.
    ///
    /// Rebuilt on demand rather than kept, because everything it is made of —
    /// the surface's box, how far it overflows, where it sits — is measured
    /// afresh by gpui on every layout pass.
    pub(super) fn scrollbar(&self, id: &'static str, surface: Surface) -> Scrollbar {
        let (handle, state) = self.surface_ref(surface);
        Scrollbar::for_handle(id, surface.axis(), handle).fade(state.fade())
    }

    /// The same bar, listening for the pointer reaching the edge it rides.
    ///
    /// Only the bars that are drawn need it: the ones the drag path builds are
    /// there to be measured, and never reach an element tree.
    pub(super) fn hovering_scrollbar(
        &self,
        id: &'static str,
        surface: Surface,
        cx: &mut Context<Self>,
    ) -> Scrollbar {
        self.scrollbar(id, surface).on_hover(cx.listener(
            move |workspace, hovered: &bool, _window, cx| {
                workspace.hover_scrollbar(surface, *hovered, cx);
            },
        ))
    }

    /// Puts each surface's bar up whenever that surface has moved, and starts
    /// the clock that takes it down again.
    ///
    /// Called from `render` because that is where every way of scrolling them
    /// meets: a wheel over the tabs or the welcome screen, and the jump that
    /// brings a newly activated tab back into view.
    pub(super) fn watch_scroll(&mut self, cx: &mut Context<Self>) {
        for (_, surface) in SCROLLBARS {
            let (handle, state) = self.surface(surface);
            let scrolled = scrolled(handle, surface.axis());
            if let Some(epoch) = state.moved(scrolled) {
                hide_later(epoch, cx, move |workspace| {
                    Some(workspace.surface(surface).1)
                });
            }
        }
    }

    /// Scrolls whichever surface's thumb has been dragged.
    ///
    /// Every element listening for this drag type hears every such drag, so each
    /// bar checks that the one being dragged is its own before answering.
    pub(super) fn drag_scrollbar(
        &mut self,
        event: &DragMoveEvent<DraggedThumb>,
        cx: &mut Context<Self>,
    ) {
        for (id, surface) in SCROLLBARS {
            let Some(progress) = self.scrollbar(id, surface).dragged(event, cx) else {
                continue;
            };

            // Held even when the pointer moved along the other axis and the
            // surface has not budged: the bar has to stay up for as long as it
            // is being held, and a still pointer moves nothing to notice.
            let (handle, state) = self.surface(surface);
            state.hold();
            scroll_to(handle, surface.axis(), progress);
            cx.notify();
            return;
        }
    }

    /// Lets go of whichever thumb was being held, and starts its clock again.
    ///
    /// Every mouse release in the window arrives here; all but the one ending a
    /// drag of a bar find nothing to let go of.
    pub(super) fn release_scrollbars(&mut self, cx: &mut Context<Self>) {
        for (_, surface) in SCROLLBARS {
            if let Some(epoch) = self.surface(surface).1.release() {
                hide_later(epoch, cx, move |workspace| {
                    Some(workspace.surface(surface).1)
                });
                cx.notify();
            }
        }
    }

    /// Puts one surface's bar up while the pointer rests on the edge it rides,
    /// and starts it going the moment the pointer leaves.
    ///
    /// Told which surface rather than asked to work it out: each strip carries
    /// this listener already and knows only its own.
    pub(super) fn hover_scrollbar(
        &mut self,
        surface: Surface,
        hovered: bool,
        cx: &mut Context<Self>,
    ) {
        let state = self.surface(surface).1;
        if hovered {
            if state.hover_enter() {
                cx.notify();
            }
            return;
        }

        let Some(epoch) = state.hover_leave() else {
            return;
        };
        hide_now(self, epoch, cx, move |workspace| {
            Some(workspace.surface(surface).1)
        });
    }

    /// Renders the bottom status bar.
    ///
    /// The layout is the one the architecture document asks for — connection,
    /// transaction state, rows, elapsed time. The first cell names the database
    /// product and version the active session reported, and the second says
    /// what the session is doing; the last two stand empty until there is a
    /// statement behind them, which is M3.
    ///
    /// The second cell is where a failure is written out, which is why it is the
    /// one that shrinks: a driver's refusal is the longest text this row ever
    /// carries.
    ///
    /// The two texts come from [`Workspace::status_cells`], which is separate so
    /// that "the bar says H2 2.3.232" can be asserted without laying out a
    /// window.
    pub(super) fn status_cells(&self) -> (SharedString, SharedString) {
        let Some(connection) = self.active_connection() else {
            return (ts!("statusbar.no_connection"), ts!("statusbar.idle"));
        };
        // The product and its version once the session has answered
        // SESSION_INFO, and the profile's own name until then — a cell that went
        // blank while connecting would read as no connection at all.
        let label = match &connection.state {
            ConnectionState::Open(connected) => connected
                .product()
                .map(SharedString::from)
                .unwrap_or_else(|| SharedString::from(connection.profile.name.clone())),
            _ => SharedString::from(connection.profile.name.clone()),
        };
        let state = match &connection.state {
            ConnectionState::Connecting { .. } => ts!("statusbar.connecting"),
            ConnectionState::Open(_) => ts!("statusbar.connected"),
            ConnectionState::Failed(error) => ts!("statusbar.failed", error = error.to_string()),
            ConnectionState::Dead(reason) => reason.clone(),
        };
        (label, state)
    }

    /// The two right-hand status bar cells — rows and elapsed time — which the
    /// active query pane owns.
    ///
    /// Blank for every other kind of pane, because there is nothing running
    /// behind them to count.
    pub(super) fn query_cells(&self, cx: &App) -> (SharedString, SharedString) {
        self.active_query()
            .map(|pane| pane.read(cx).status_cells())
            .unwrap_or_default()
    }

    pub(super) fn render_status_bar(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = theme(cx);
        let (connection, state) = self.status_cells();
        let (rows, elapsed) = self.query_cells(cx);
        let state_color = match self.active_connection().map(|open| &open.state) {
            Some(ConnectionState::Open(_)) => Some(theme.success),
            Some(ConnectionState::Failed(_) | ConnectionState::Dead(_)) => Some(theme.danger),
            _ => None,
        };

        div()
            .flex()
            .flex_row()
            .flex_none()
            .items_center()
            .gap(px(14.))
            .h(px(STATUS_BAR_HEIGHT))
            .px(px(10.))
            // The bar is inert, so a press on it must not move the keyboard.
            // Without this the workspace root's `track_focus` would claim the
            // click.
            .on_any_mouse_down(|_, window, _cx| window.prevent_default())
            .bg(theme.surface)
            .border_t_1()
            .border_color(theme.border)
            .text_size(px(11.))
            .text_color(theme.text_muted)
            .child(div().flex_none().whitespace_nowrap().child(connection))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .when_some(state_color, |cell, color| cell.text_color(color))
                    .child(state),
            )
            .child(
                div()
                    .flex_none()
                    .whitespace_nowrap()
                    .child(if rows.is_empty() { NOTHING } else { rows }),
            )
            .child(
                div()
                    .flex_none()
                    .whitespace_nowrap()
                    .child(if elapsed.is_empty() { NOTHING } else { elapsed }),
            )
            .into_any_element()
    }

    /// The write confirmation, while a query pane is waiting on one.
    ///
    /// Two buttons and the statement itself: a dialog that asked "are you
    /// sure?" without showing what it is about would be asking the user to
    /// remember rather than to read.
    pub(super) fn render_confirm(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let pending = self.confirm.as_ref()?;
        let theme = theme(cx);
        let this = cx.entity();
        let dismiss = this.clone();

        let body = div()
            .flex()
            .flex_col()
            .gap(px(12.))
            .child(
                div()
                    .text_size(px(12.))
                    .text_color(theme.text)
                    .child(ts!("query.confirm_body", count = pending.request.count)),
            )
            .child(
                div()
                    .id("confirm-preview")
                    .max_h(px(200.))
                    .overflow_y_scroll()
                    .restrict_scroll_to_axis()
                    .p(px(8.))
                    .rounded_md()
                    .bg(theme.surface)
                    .border_1()
                    .border_color(theme.border)
                    .text_size(px(11.))
                    .text_color(theme.text_muted)
                    .child(pending.request.preview.clone()),
            )
            .child(
                div()
                    .flex()
                    .flex_row()
                    .justify_end()
                    .gap(px(8.))
                    .child(
                        Button::new("confirm-cancel", ts!("common.cancel"))
                            .variant(ButtonVariant::Secondary)
                            .on_click({
                                let this = this.clone();
                                move |_, window, cx| {
                                    this.update(cx, |workspace, cx| {
                                        workspace.answer_confirm(false, window, cx);
                                    });
                                }
                            }),
                    )
                    .child(
                        Button::new("confirm-run", ts!("query.confirm_run"))
                            .variant(ButtonVariant::Danger)
                            .on_click(move |_, window, cx| {
                                this.update(cx, |workspace, cx| {
                                    workspace.answer_confirm(true, window, cx);
                                });
                            }),
                    ),
            );

        Some(
            modal(
                "query-confirm",
                ts!("query.confirm_title"),
                px(460.),
                body,
                move |window, cx| {
                    dismiss.update(cx, |workspace, cx| {
                        workspace.answer_confirm(false, window, cx);
                    });
                },
            )
            .into_any_element(),
        )
    }
}
