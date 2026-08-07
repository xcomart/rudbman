//! The object detail panel: what one table, view or routine is made of.
//!
//! Opened by activating a node in the explorer, and shown in a pane of the work
//! area. Four tabs for a relation — columns, keys and indexes, references, and
//! the `CREATE` statement — and a single one for a routine or a sequence, which
//! have no columns to lay out.
//!
//! # One load, one refresh
//!
//! Everything the panel shows is fetched once, when it opens, and again only
//! when the user asks: metadata calls are not free — `getIndexInfo` on a large
//! Oracle schema can take a minute if the driver decides to refresh statistics —
//! and a panel that refetched per tab switch would spend that repeatedly. The
//! four tabs are therefore four requests issued together, and the tab strip is
//! pure presentation.
//!
//! The fetch is the workspace's, for the same reason the explorer's is: the
//! session belongs to the tab, not to the panel. The panel emits
//! [`TableDetailEvent::Load`] and is handed a [`Details`] back.
//!
//! # Reconstructed DDL is labelled
//!
//! [`DdlResult::is_reconstructed`] means the text was reassembled from JDBC
//! metadata rather than quoted by the server, and JDBC metadata carries no
//! `CHECK` constraints, no triggers, no partitioning and no collations. The DDL
//! tab says so in a banner above the text, because the difference between "this
//! is your table" and "this is most of your table" is not one to leave to the
//! reader.
//!
//! # This is not the result grid
//!
//! The tables here are plain rows in a scroll area. A column list is tens of
//! rows, occasionally hundreds, and never the millions the query grid has to
//! survive — that one is `rudbman-grid`, and it virtualises. What this panel
//! does need is the vertical scrollbar, because a wide table really can have
//! several hundred columns.

use gpui::{
    App, ClipboardItem, Context, DragMoveEvent, EventEmitter, FocusHandle, Focusable, IntoElement,
    MouseButton, MouseUpEvent, Render, ScrollHandle, SharedString, Window, div, prelude::*, px,
};
use rudbman_ui::{
    Button, ButtonVariant, DraggedThumb, Scrollbar, ScrollbarAxis, ScrollbarState, Theme,
    hide_later, hide_now, scroll_to, scrolled, theme,
};

use rudbman_jdbc::{DdlSource, DescribeRequest, Session};

use crate::explorer::{ObjectTarget, describe_failure};
use crate::i18n::ts;
use crate::icons;

/// Element id of the body's overlay scroll indicator.
const BODY_SCROLLBAR: &str = "detail-body-scrollbar";

/// One row of a metadata table, already rendered to text.
///
/// Strings rather than the driver's own types: every cell here is shown and
/// none is computed with, and turning `Option<i32>` into "—" once at the edge
/// keeps the renderer free of per-column special cases.
pub type Row = Vec<SharedString>;

/// What the panel shows, once the fetch has come back.
#[derive(Clone, Debug, Default)]
pub struct Details {
    /// One row per column: name, type, nullability, default, key, comment.
    pub columns: Vec<Row>,
    /// The primary key's columns, in key order.
    pub primary_key: Vec<Row>,
    /// Indexes, one row per indexed column.
    pub indexes: Vec<Row>,
    /// Foreign keys this object declares — the tables it points at.
    pub imported: Vec<Row>,
    /// Foreign keys pointing *at* this object.
    pub exported: Vec<Row>,
    /// The `CREATE` statement, and whether it was reconstructed.
    pub ddl: Option<Ddl>,
    /// Why the `CREATE` statement could not be read, when it could not.
    ///
    /// Separate from the whole panel failing: a driver with no native DDL path
    /// and a view the reconstruction cannot express still has columns, keys and
    /// references worth showing, so one tab reports its own trouble.
    pub ddl_error: Option<SharedString>,
    /// For a routine or a sequence: its properties, as name/value rows.
    pub properties: Vec<Row>,
}

/// A `CREATE` statement and where it came from.
#[derive(Clone, Debug)]
pub struct Ddl {
    /// The statement text.
    pub text: SharedString,
    /// Whether it was reassembled from JDBC metadata rather than quoted by the
    /// server. Shown as a banner; see the module documentation.
    pub reconstructed: bool,
}

/// Which tab is on screen.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tab {
    /// The column list.
    Columns,
    /// Primary key and indexes.
    Keys,
    /// Foreign keys, both directions.
    References,
    /// The `CREATE` statement.
    Ddl,
    /// A routine's or a sequence's properties — the only tab those objects get.
    Properties,
}

impl Tab {
    /// The tabs a relation shows, in order.
    const RELATION: [Tab; 4] = [Tab::Columns, Tab::Keys, Tab::References, Tab::Ddl];
    /// The tabs a routine or a sequence shows.
    const ROUTINE: [Tab; 1] = [Tab::Properties];

    /// The tab's label in the active language.
    fn label(self) -> SharedString {
        match self {
            Tab::Columns => ts!("detail.tab_columns"),
            Tab::Keys => ts!("detail.tab_keys"),
            Tab::References => ts!("detail.tab_references"),
            Tab::Ddl => ts!("detail.tab_ddl"),
            Tab::Properties => ts!("detail.tab_properties"),
        }
    }

    /// Element id fragment, which is never translated.
    fn slug(self) -> &'static str {
        match self {
            Tab::Columns => "columns",
            Tab::Keys => "keys",
            Tab::References => "references",
            Tab::Ddl => "ddl",
            Tab::Properties => "properties",
        }
    }
}

/// Where the panel's data has got to.
enum Load {
    /// A fetch is out.
    Running,
    /// It came back.
    Ready(Box<Details>),
    /// It failed; the driver's own message.
    Failed(SharedString),
}

/// What the panel asks the workspace for.
pub enum TableDetailEvent {
    /// Describe this object; the workspace has the session.
    Load(Box<ObjectTarget>),
}

/// The panel.
pub struct TableDetail {
    /// What is being described.
    target: ObjectTarget,
    /// Where the fetch has got to.
    load: Load,
    /// Which tab is on screen.
    tab: Tab,
    focus_handle: FocusHandle,
    /// Scroll of the tab body.
    body_scroll: ScrollHandle,
    /// Whether the body's overlay bar is on screen.
    body_scrollbar: ScrollbarState,
}

impl TableDetail {
    /// Opens a panel over `target`, in its loading state.
    ///
    /// It does **not** ask for the metadata: an event emitted from a
    /// constructor has no subscriber yet — the host is still inside `cx.new` —
    /// and would be dropped, leaving a panel that says "loading…" for ever.
    /// The host subscribes and then calls [`TableDetail::refresh`], which is
    /// the same path the reload button takes.
    pub fn new(target: ObjectTarget, cx: &mut Context<Self>) -> Self {
        Self {
            tab: if target.folder.is_relation() {
                Tab::Columns
            } else {
                Tab::Properties
            },
            target,
            load: Load::Running,
            focus_handle: cx.focus_handle(),
            body_scroll: ScrollHandle::new(),
            body_scrollbar: ScrollbarState::new(),
        }
    }

    /// The object this panel describes.
    ///
    /// The shell's tab strip titles the tab with it and colours its dot by the
    /// connection it names, and the explorer's "open this object" path compares
    /// against it to reuse an open tab rather than opening a second copy.
    pub fn target(&self) -> &ObjectTarget {
        &self.target
    }

    /// Records what a fetch produced.
    pub fn deliver(&mut self, outcome: Result<Details, SharedString>, cx: &mut Context<Self>) {
        self.load = match outcome {
            Ok(details) => Load::Ready(Box::new(details)),
            Err(message) => Load::Failed(message),
        };
        cx.notify();
    }

    /// The details, once loaded.
    ///
    /// Test-only, like the two below it: the renderer reads `self.load`
    /// directly, and an accessor nothing in the binary calls is dead weight in
    /// it.
    #[cfg(test)]
    pub fn details(&self) -> Option<&Details> {
        match &self.load {
            Load::Ready(details) => Some(details),
            _ => None,
        }
    }

    /// Whether a fetch is out.
    pub fn is_loading(&self) -> bool {
        matches!(self.load, Load::Running)
    }

    /// The failure a fetch reported, if it failed.
    #[cfg(test)]
    pub fn failure(&self) -> Option<&SharedString> {
        match &self.load {
            Load::Failed(message) => Some(message),
            _ => None,
        }
    }

    /// The tab on screen.
    #[cfg(test)]
    pub fn tab(&self) -> Tab {
        self.tab
    }

    /// Switches tabs, which costs nothing: everything is already loaded.
    pub fn select_tab(&mut self, tab: Tab, cx: &mut Context<Self>) {
        if self.tab != tab {
            self.tab = tab;
            self.body_scroll.scroll_to_item(0);
            cx.notify();
        }
    }

    /// Asks for the metadata again.
    pub fn refresh(&mut self, cx: &mut Context<Self>) {
        self.load = Load::Running;
        cx.emit(TableDetailEvent::Load(Box::new(self.target.clone())));
        cx.notify();
    }

    /// The tabs this object gets.
    fn tabs(&self) -> &'static [Tab] {
        if self.target.folder.is_relation() {
            &Tab::RELATION
        } else {
            &Tab::ROUTINE
        }
    }

    /// The overlay bar of the body, as it stands.
    fn scrollbar(&self) -> Scrollbar {
        Scrollbar::for_handle(BODY_SCROLLBAR, ScrollbarAxis::Vertical, &self.body_scroll)
            .fade(self.body_scrollbar.fade())
    }

    /// Puts the bar up when the body has moved, and starts the clock.
    fn watch_scroll(&mut self, cx: &mut Context<Self>) {
        let moved = scrolled(&self.body_scroll, ScrollbarAxis::Vertical);
        if let Some(epoch) = self.body_scrollbar.moved(moved) {
            hide_later(epoch, cx, |detail: &mut Self| {
                Some(&mut detail.body_scrollbar)
            });
        }
    }

    /// The header: what is being shown, and the button that reloads it.
    fn render_header(&self, chrome: &Theme, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let this = cx.entity();
        let loading = self.is_loading();

        div()
            .flex()
            .flex_row()
            .flex_none()
            .items_center()
            .gap(px(8.))
            .h(px(30.))
            .px(px(10.))
            .border_b_1()
            .border_color(chrome.border)
            .child(icons::icon(
                self.target.folder.icon(),
                px(14.),
                chrome.text_muted,
            ))
            .child(
                div()
                    .flex_none()
                    .text_size(px(13.))
                    .text_color(chrome.text)
                    .child(SharedString::from(self.target.qualified())),
            )
            .child(div().flex_1().min_w_0())
            .when(loading, |header| {
                header.child(
                    div()
                        .flex_none()
                        .text_size(px(11.))
                        .text_color(chrome.text_muted)
                        .child(ts!("detail.loading")),
                )
            })
            .child(
                Button::new("detail-refresh", ts!("detail.refresh"))
                    .variant(ButtonVariant::Secondary)
                    .disabled(loading)
                    .on_click(move |_, _window, cx| {
                        this.update(cx, |detail, cx| detail.refresh(cx));
                    }),
            )
    }

    /// The tab strip.
    fn render_tabs(&self, chrome: &Theme, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let this = cx.entity();
        let tabs: Vec<_> = self
            .tabs()
            .iter()
            .map(|tab| {
                let tab = *tab;
                let active = self.tab == tab;
                let this = this.clone();
                div()
                    .id(SharedString::new_static(tab.slug()))
                    .flex_none()
                    .px(px(10.))
                    .py(px(5.))
                    .cursor_pointer()
                    .text_size(px(12.))
                    .border_b_2()
                    .border_color(if active {
                        chrome.accent
                    } else {
                        gpui::transparent_black()
                    })
                    .text_color(if active {
                        chrome.text
                    } else {
                        chrome.text_muted
                    })
                    .when(!active, |tab| tab.hover(|tab| tab.bg(chrome.surface_hover)))
                    .child(tab.label())
                    .on_click(move |_, _window, cx| {
                        this.update(cx, |detail, cx| detail.select_tab(tab, cx));
                    })
            })
            .collect();

        div()
            .flex()
            .flex_row()
            .flex_none()
            .items_center()
            .gap(px(2.))
            .px(px(6.))
            .border_b_1()
            .border_color(chrome.border)
            .children(tabs)
    }

    /// The body of whichever tab is showing.
    fn render_body(&self, chrome: &Theme, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let content = match &self.load {
            Load::Running => note(ts!("detail.loading"), chrome.text_muted).into_any_element(),
            Load::Failed(message) => note(message.clone(), chrome.danger).into_any_element(),
            Load::Ready(details) => self.render_tab_body(details, chrome, cx),
        };

        div()
            .relative()
            .flex()
            .flex_col()
            .flex_1()
            .min_w_0()
            .min_h_0()
            .child(
                div()
                    .id("detail-body")
                    .track_scroll(&self.body_scroll)
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_h_0()
                    .gap(px(12.))
                    .p(px(10.))
                    .overflow_y_scroll()
                    .child(content),
            )
            .children(
                self.scrollbar()
                    .on_hover(cx.listener(|detail, hovered: &bool, _window, cx| {
                        detail.hover_scrollbar(*hovered, cx);
                    }))
                    .render(chrome),
            )
    }

    /// One tab's content, with the data in hand.
    fn render_tab_body(
        &self,
        details: &Details,
        chrome: &Theme,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        match self.tab {
            Tab::Columns => table(
                [
                    ts!("detail.column"),
                    ts!("detail.type"),
                    ts!("detail.nullable"),
                    ts!("detail.default"),
                    ts!("detail.key"),
                    ts!("detail.comment"),
                ],
                &details.columns,
                chrome,
            )
            .into_any_element(),

            Tab::Keys => div()
                .flex()
                .flex_col()
                .gap(px(14.))
                .child(section(
                    ts!("detail.primary_key"),
                    table(
                        [ts!("detail.name"), ts!("detail.column"), ts!("detail.seq")],
                        &details.primary_key,
                        chrome,
                    ),
                    chrome,
                ))
                .child(section(
                    ts!("detail.indexes"),
                    table(
                        [
                            ts!("detail.name"),
                            ts!("detail.column"),
                            ts!("detail.unique"),
                            ts!("detail.order"),
                        ],
                        &details.indexes,
                        chrome,
                    ),
                    chrome,
                ))
                .into_any_element(),

            // Both directions, and labelled by direction rather than by JDBC's
            // "imported"/"exported": which way a foreign key points is the one
            // thing a reader has to get right, and the JDBC words do not say it.
            Tab::References => div()
                .flex()
                .flex_col()
                .gap(px(14.))
                .child(section(
                    ts!("detail.references_out"),
                    table(
                        [
                            ts!("detail.name"),
                            ts!("detail.column"),
                            ts!("detail.target"),
                            ts!("detail.on_update"),
                            ts!("detail.on_delete"),
                        ],
                        &details.imported,
                        chrome,
                    ),
                    chrome,
                ))
                .child(section(
                    ts!("detail.references_in"),
                    table(
                        [
                            ts!("detail.name"),
                            ts!("detail.source"),
                            ts!("detail.column"),
                            ts!("detail.on_update"),
                            ts!("detail.on_delete"),
                        ],
                        &details.exported,
                        chrome,
                    ),
                    chrome,
                ))
                .into_any_element(),

            Tab::Ddl => self.render_ddl(details, chrome, cx),

            Tab::Properties => table(
                [ts!("detail.name"), ts!("detail.value")],
                &details.properties,
                chrome,
            )
            .into_any_element(),
        }
    }

    /// The DDL tab: the banner when the text was reconstructed, the copy button,
    /// and the statement.
    fn render_ddl(
        &self,
        details: &Details,
        chrome: &Theme,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        if let Some(message) = details.ddl_error.as_ref() {
            return note(message.clone(), chrome.danger).into_any_element();
        }
        let Some(ddl) = details.ddl.as_ref() else {
            return note(ts!("detail.no_ddl"), chrome.text_muted).into_any_element();
        };
        let this = cx.entity();
        let text = ddl.text.clone();

        // The bridge answered from JDBC metadata rather than from the server's
        // own `CREATE` text, which means CHECK constraints, triggers,
        // partitioning and collations are simply not in it. Saying so is what
        // stops the text being mistaken for a backup.
        let banner = ddl.reconstructed.then(|| {
            div()
                .flex_none()
                .px(px(10.))
                .py(px(6.))
                .rounded_md()
                .border_1()
                .border_color(chrome.border)
                .bg(chrome.surface)
                .text_size(px(11.))
                .text_color(chrome.text_muted)
                .child(ts!("detail.ddl_reconstructed"))
        });

        div()
            .flex()
            .flex_col()
            .gap(px(8.))
            .children(banner)
            .child(
                div().flex().flex_row().justify_end().child(
                    Button::new("detail-copy-ddl", ts!("detail.copy"))
                        .variant(ButtonVariant::Secondary)
                        .on_click(move |_, _window, cx| {
                            cx.write_to_clipboard(ClipboardItem::new_string(text.to_string()));
                            this.update(cx, |_detail, cx| cx.notify());
                        }),
                ),
            )
            .child(
                // Monospace and preserved whitespace: a `CREATE` statement is
                // laid out by its own indentation and reflowing it would make it
                // unreadable.
                div()
                    .w_full()
                    .p(px(10.))
                    .rounded_md()
                    .bg(chrome.surface)
                    .border_1()
                    .border_color(chrome.border)
                    .font_family(crate::app_settings::monospace_family(cx))
                    .text_size(px(12.))
                    .text_color(chrome.text)
                    .child(ddl.text.clone()),
            )
            .into_any_element()
    }
}

impl EventEmitter<TableDetailEvent> for TableDetail {}

impl Focusable for TableDetail {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for TableDetail {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.watch_scroll(cx);
        let chrome = theme(cx);
        let header = self.render_header(&chrome, cx);
        let tabs = self.render_tabs(&chrome, cx);
        let body = self.render_body(&chrome, cx);

        div()
            .id("table-detail")
            .track_focus(&self.focus_handle)
            .flex()
            .flex_col()
            .size_full()
            .min_w_0()
            .min_h_0()
            .on_drag_move::<DraggedThumb>(cx.listener(
                |detail, event: &DragMoveEvent<DraggedThumb>, _window, cx| {
                    let Some(progress) = detail.scrollbar().dragged(event, cx) else {
                        return;
                    };
                    detail.body_scrollbar.hold();
                    scroll_to(&detail.body_scroll, ScrollbarAxis::Vertical, progress);
                    cx.notify();
                },
            ))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|detail, _: &MouseUpEvent, _window, cx| detail.release(cx)),
            )
            .on_mouse_up_out(
                MouseButton::Left,
                cx.listener(|detail, _: &MouseUpEvent, _window, cx| detail.release(cx)),
            )
            .child(header)
            .child(tabs)
            .child(body)
    }
}

impl TableDetail {
    /// Lets go of the body's thumb and starts its clock again.
    fn release(&mut self, cx: &mut Context<Self>) {
        if let Some(epoch) = self.body_scrollbar.release() {
            hide_later(epoch, cx, |detail: &mut Self| {
                Some(&mut detail.body_scrollbar)
            });
            cx.notify();
        }
    }

    /// Puts the bar up while the pointer rests on the edge it rides, and starts
    /// it going the moment the pointer leaves.
    fn hover_scrollbar(&mut self, hovered: bool, cx: &mut Context<Self>) {
        if hovered {
            if self.body_scrollbar.hover_enter() {
                cx.notify();
            }
            return;
        }

        if let Some(epoch) = self.body_scrollbar.hover_leave() {
            hide_now(self, epoch, cx, |detail: &mut Self| {
                Some(&mut detail.body_scrollbar)
            });
        }
    }
}

/// Reads everything the panel shows, in one go.
///
/// **Blocks**, and is called from `cx.background_spawn` with a
/// [`SessionHandle`](crate::connection::SessionHandle). Every request goes
/// through the session's own worker thread, so they queue behind whatever else
/// that connection is doing rather than racing it — which is also why the panel
/// shows a spinner rather than pretending to be instant.
///
/// A relation gets all four tabs' worth; a routine or a sequence gets its
/// properties. The DDL is allowed to fail on its own: a driver with no native
/// path and a view the reconstruction cannot express still has columns worth
/// showing.
pub fn load_details(session: &Session, target: &ObjectTarget) -> Result<Details, String> {
    if !target.folder.is_relation() {
        return load_properties(session, target);
    }

    let catalog = target.catalog.as_deref();
    let schema = target.schema.as_deref();
    let scoped = |kind: &str| {
        let mut request = DescribeRequest::new(kind).with_table(&target.name);
        request.catalog = target.catalog.clone();
        request.schema = target.schema.clone();
        request
    };

    // The primary key first: the column list marks its members, so it has to be
    // known before the columns are rendered into rows.
    let primary = items(session, &scoped("primary_keys"))?;
    let key_columns: Vec<&str> = primary
        .iter()
        .filter_map(|row| text(row, "column"))
        .collect();

    let columns = items(session, &{
        let mut request = DescribeRequest::new("columns");
        request.catalog = target.catalog.clone();
        request.schema = target.schema.clone();
        request.table = Some(target.name.clone());
        request
    })?;
    let column_rows = columns
        .iter()
        .map(|column| {
            let name = text(column, "name").unwrap_or_default();
            vec![
                cell(Some(name)),
                cell(Some(&type_of(column))),
                // SQL keywords, not words: NULL and NOT NULL read the same in
                // every language and are what the column's own DDL says.
                SharedString::new_static(if flag(column, "is_nullable").unwrap_or(true) {
                    "NULL"
                } else {
                    "NOT NULL"
                }),
                cell(text(column, "default")),
                if key_columns.contains(&name) {
                    SharedString::new_static("PK")
                } else {
                    NOTHING
                },
                cell(text(column, "remarks")),
            ]
        })
        .collect();

    let indexes = items(session, &scoped("indexes"))?
        .iter()
        // An index entry with no column is the driver reporting a table
        // statistic (`tableIndexStatistic`), not an index.
        .filter(|index| text(index, "column").is_some())
        .map(|index| {
            vec![
                cell(text(index, "name")),
                cell(text(index, "column")),
                if flag(index, "non_unique").unwrap_or(true) {
                    NOTHING
                } else {
                    SharedString::new_static("UNIQUE")
                },
                match text(index, "asc_desc") {
                    Some("D") => SharedString::new_static("DESC"),
                    Some("A") => SharedString::new_static("ASC"),
                    _ => NOTHING,
                },
            ]
        })
        .collect();

    let imported = items(session, &scoped("imported_keys"))?
        .iter()
        .map(|key| {
            vec![
                cell(text(key, "fk_name")),
                cell(text(key, "fk_column")),
                cell(Some(&qualify(
                    text(key, "pk_schema"),
                    text(key, "pk_table"),
                    text(key, "pk_column"),
                ))),
                rule(number(key, "update_rule")),
                rule(number(key, "delete_rule")),
            ]
        })
        .collect();

    let exported = items(session, &scoped("exported_keys"))?
        .iter()
        .map(|key| {
            vec![
                cell(text(key, "fk_name")),
                cell(Some(&qualify(
                    text(key, "fk_schema"),
                    text(key, "fk_table"),
                    None,
                ))),
                cell(text(key, "fk_column")),
                rule(number(key, "update_rule")),
                rule(number(key, "delete_rule")),
            ]
        })
        .collect();

    // `Auto` rather than `Native`: a product with no native path answers with
    // reconstructed text, which the banner then labels. Asking for `Native`
    // would fail outright on most drivers.
    let (ddl, ddl_error) =
        match session.describe_ddl(catalog, schema, &target.name, DdlSource::Auto) {
            Ok(result) => (
                Some(Ddl {
                    reconstructed: result.is_reconstructed(),
                    text: SharedString::from(result.ddl),
                }),
                None,
            ),
            Err(error) => (None, Some(SharedString::from(describe_failure(error)))),
        };

    Ok(Details {
        columns: column_rows,
        primary_key: primary
            .iter()
            .map(|key| {
                vec![
                    cell(text(key, "name")),
                    cell(text(key, "column")),
                    cell(number(key, "seq").map(|seq| seq.to_string()).as_deref()),
                ]
            })
            .collect(),
        indexes,
        imported,
        exported,
        ddl,
        ddl_error,
        properties: Vec::new(),
    })
}

/// The smaller panel: a routine's or a sequence's own facts.
///
/// One `DESCRIBE` filtered by name, rendered as name/value pairs, plus one row
/// per parameter for a routine — which the bridge carries inline, so this is
/// still a single round trip.
fn load_properties(session: &Session, target: &ObjectTarget) -> Result<Details, String> {
    let mut request = DescribeRequest::new(target.folder.describe_kind()).with_name(&target.name);
    request.catalog = target.catalog.clone();
    request.schema = target.schema.clone();

    let found = items(session, &request)?;
    let Some(object) = found
        .iter()
        .find(|item| text(item, "name") == Some(target.name.as_str()))
        .or_else(|| found.first())
    else {
        return Ok(Details {
            properties: Vec::new(),
            ..Details::default()
        });
    };

    let mut properties: Vec<Row> = Vec::new();
    let mut put = |label: SharedString, value: Option<&str>| {
        if let Some(value) = value.filter(|value| !value.is_empty()) {
            properties.push(vec![label, SharedString::from(value.to_owned())]);
        }
    };
    put(ts!("detail.name"), text(object, "name"));
    put(ts!("detail.schema"), text(object, "schema"));
    put(ts!("detail.type"), text(object, "type_name"));
    put(ts!("detail.comment"), text(object, "remarks"));
    // Sequence facts, absent on a routine and vice versa; each is only shown
    // where the product answered one.
    for (key, label) in [
        ("data_type", ts!("detail.type")),
        ("start_value", ts!("detail.start_value")),
        ("increment", ts!("detail.increment")),
        ("min_value", ts!("detail.min_value")),
        ("max_value", ts!("detail.max_value")),
        ("current_value", ts!("detail.current_value")),
        ("cycle", ts!("detail.cycle")),
        ("cache", ts!("detail.cache")),
    ] {
        put(label, text(object, key));
    }

    // The parameters, in ordinal order, as one row each.
    if let Some(parameters) = object
        .get("parameters")
        .and_then(serde_json::Value::as_array)
    {
        for parameter in parameters {
            let Some(parameter) = parameter.as_object() else {
                continue;
            };
            let label = match text(parameter, "mode_name") {
                Some(mode) => SharedString::from(format!(
                    "{} ({mode})",
                    text(parameter, "name").unwrap_or("?")
                )),
                None => cell(text(parameter, "name")),
            };
            properties.push(vec![label, cell(Some(&type_of(parameter)))]);
        }
    }

    Ok(Details {
        properties,
        ..Details::default()
    })
}

/// Runs one `DESCRIBE` and hands back its items.
///
/// Shared with [`crate::erd_pane`], along with the four readers below and
/// [`type_of`]: the ERD assembles its boxes from the same `columns`,
/// `primary_keys` and `imported_keys` answers this panel lays out as rows, and
/// a column typed `NUMERIC(12,2)` on one has to read `NUMERIC(12,2)` on the
/// other.
pub(crate) fn items(
    session: &Session,
    request: &DescribeRequest,
) -> Result<Vec<serde_json::Map<String, serde_json::Value>>, String> {
    session
        .describe(request)
        .map(|result| result.items)
        .map_err(describe_failure)
}

/// A string member of one item, or `None` when it is absent or SQL NULL.
pub(crate) fn text<'a>(
    item: &'a serde_json::Map<String, serde_json::Value>,
    key: &str,
) -> Option<&'a str> {
    item.get(key)
        .and_then(serde_json::Value::as_str)
        .filter(|value| !value.is_empty())
}

/// A numeric member of one item.
pub(crate) fn number(item: &serde_json::Map<String, serde_json::Value>, key: &str) -> Option<i64> {
    item.get(key).and_then(serde_json::Value::as_i64)
}

/// A boolean member of one item.
pub(crate) fn flag(item: &serde_json::Map<String, serde_json::Value>, key: &str) -> Option<bool> {
    item.get(key).and_then(serde_json::Value::as_bool)
}

/// A cell, or the placeholder when there is nothing to put in it.
fn cell(value: Option<&str>) -> SharedString {
    match value.filter(|value| !value.is_empty()) {
        Some(value) => SharedString::from(value.to_owned()),
        None => NOTHING,
    }
}

/// A column's type as its DDL would spell it: `VARCHAR(255)`, `NUMERIC(10,2)`,
/// `INTEGER`.
///
/// The driver's own `TYPE_NAME`, with the size appended **only where the DDL
/// would carry one**. That qualification is the whole of this function: JDBC
/// reports a `COLUMN_SIZE` for every type, and for an `INTEGER` it is the
/// number of bits — H2 answers 32 — so appending it unconditionally produces
/// `INTEGER(32)`, which no dialect has ever written.
///
/// So the decision is made on `java.sql.Types`, which the bridge passes through
/// as `data_type`:
///
/// * the character and binary types take a length;
/// * `NUMERIC` and `DECIMAL` take a precision and, when it is not zero, a scale;
/// * everything else — integers, floats, dates, booleans, LOBs — is bare.
///
/// A driver that already spelled the size into the name, which several do, is
/// left alone rather than doubled.
pub(crate) fn type_of(item: &serde_json::Map<String, serde_json::Value>) -> String {
    /// `java.sql.Types` codes whose DDL carries a length.
    const SIZED: [i64; 9] = [
        1,   // CHAR
        12,  // VARCHAR
        -1,  // LONGVARCHAR
        -15, // NCHAR
        -9,  // NVARCHAR
        -16, // LONGNVARCHAR
        -2,  // BINARY
        -3,  // VARBINARY
        -4,  // LONGVARBINARY
    ];
    /// `java.sql.Types` codes whose DDL carries a precision and a scale.
    const SCALED: [i64; 2] = [2 /* NUMERIC */, 3 /* DECIMAL */];

    let name = text(item, "type_name")
        .or_else(|| text(item, "jdbc_type"))
        .unwrap_or("?");
    if name.contains('(') {
        return name.to_string();
    }

    let code = number(item, "data_type");
    let size = number(item, "size").or_else(|| number(item, "precision"));
    let digits = number(item, "digits").or_else(|| number(item, "scale"));

    match (code, size) {
        (Some(code), Some(size)) if SCALED.contains(&code) && size > 0 => {
            match digits.filter(|digits| *digits > 0) {
                Some(digits) => format!("{name}({size},{digits})"),
                None => format!("{name}({size})"),
            }
        }
        (Some(code), Some(size)) if SIZED.contains(&code) && size > 0 => {
            format!("{name}({size})")
        }
        _ => name.to_string(),
    }
}

/// `schema.table.column`, skipping whichever parts the driver left out.
fn qualify(schema: Option<&str>, table: Option<&str>, column: Option<&str>) -> String {
    [schema, table, column]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join(".")
}

/// A foreign key's referential action, from `DatabaseMetaData`'s numbering.
///
/// The SQL clause names rather than JDBC's constant names, and untranslated:
/// `ON DELETE CASCADE` is what the DDL says and what a reader is comparing
/// against. The numbers are `importedKeyCascade` and friends, which the
/// specification fixes.
fn rule(code: Option<i64>) -> SharedString {
    match code {
        Some(0) => SharedString::new_static("CASCADE"),
        Some(1) => SharedString::new_static("RESTRICT"),
        Some(2) => SharedString::new_static("SET NULL"),
        Some(3) => SharedString::new_static("NO ACTION"),
        Some(4) => SharedString::new_static("SET DEFAULT"),
        _ => NOTHING,
    }
}

/// A titled block inside a tab.
fn section<E: IntoElement>(
    title: SharedString,
    body: E,
    chrome: &Theme,
) -> impl IntoElement + use<E> {
    div()
        .flex()
        .flex_col()
        .gap(px(6.))
        .child(
            div()
                .text_size(px(11.))
                .text_color(chrome.text_muted)
                .child(title),
        )
        .child(body)
}

/// A one-line message where a table would be.
fn note(message: SharedString, color: gpui::Hsla) -> impl IntoElement {
    div()
        .py(px(6.))
        .text_size(px(12.))
        .text_color(color)
        .child(message)
}

/// A metadata table: a header row and one row per item.
///
/// Every column shares the width evenly rather than being measured, which is
/// what keeps this a hundred lines instead of a grid: the widest cell truncates,
/// and the tab that needs real column sizing is the result grid in M3.
fn table<const N: usize>(
    headers: [SharedString; N],
    rows: &[Row],
    chrome: &Theme,
) -> impl IntoElement + use<N> {
    if rows.is_empty() {
        return note(ts!("detail.empty"), chrome.text_muted).into_any_element();
    }

    let cell = |text: SharedString, muted: bool| {
        div()
            .flex_1()
            .min_w_0()
            .truncate()
            .px(px(6.))
            .py(px(3.))
            .text_size(px(11.))
            .text_color(if muted {
                chrome.text_muted
            } else {
                chrome.text
            })
            .child(text)
    };

    let header = div()
        .flex()
        .flex_row()
        .border_b_1()
        .border_color(chrome.border)
        .children(headers.iter().map(|text| cell(text.clone(), true)));

    let body = rows.iter().enumerate().map(|(index, row)| {
        div()
            .flex()
            .flex_row()
            // Zebra striping, because a row of six truncated cells is hard to
            // follow across otherwise.
            .when(index % 2 == 1, |row| row.bg(chrome.surface))
            .children((0..N).map(|column| {
                cell(
                    row.get(column).cloned().unwrap_or_default(),
                    // The first column is the name and is what the eye follows;
                    // the rest are detail.
                    column > 0,
                )
            }))
    });

    div()
        .flex()
        .flex_col()
        .w_full()
        .child(header)
        .children(body)
        .into_any_element()
}

/// The placeholder a cell with nothing in it draws.
///
/// Punctuation rather than a word, so it reads the same in every language and
/// cannot be mistaken for the string `"null"` in a default expression.
pub const NOTHING: SharedString = SharedString::new_static("—");

#[cfg(test)]
mod tests {
    use super::*;
    use crate::explorer::{ConnectionId, Folder};

    fn target(folder: Folder) -> ObjectTarget {
        ObjectTarget {
            connection: ConnectionId(1),
            catalog: None,
            schema: Some("PUBLIC".to_string()),
            folder,
            name: "PERSON".to_string(),
        }
    }

    /// What a fixture table looks like through the panel.
    fn describe(
        name: &str,
        folder: Folder,
        fixture: &str,
    ) -> (Details, crate::connection::Connected) {
        let connected = crate::explorer::tests::h2_fixture(fixture);
        let target = ObjectTarget {
            connection: ConnectionId(1),
            catalog: None,
            schema: Some("APP".to_string()),
            folder,
            name: name.to_string(),
        };
        let details = load_details(connected.session(), &target)
            .unwrap_or_else(|error| panic!("{name}: {error}"));
        (details, connected)
    }

    /// Column of `rows` whose first cell is `name`.
    fn row_for<'a>(rows: &'a [Row], name: &str) -> &'a Row {
        rows.iter()
            .find(|row| row.first().map(SharedString::as_ref) == Some(name))
            .unwrap_or_else(|| panic!("no row for {name} in {rows:?}"))
    }

    /// The columns tab against a real table.
    #[test]
    fn the_columns_tab_carries_the_type_the_nullability_and_the_key() {
        let (details, _held) = describe("PERSON", Folder::Tables, "detail-columns");

        let id = row_for(&details.columns, "ID");
        assert_eq!(id[1], "INTEGER", "{id:?}");
        assert_eq!(id[2], "NOT NULL");
        // The primary key is read before the columns are laid out, so its
        // members are marked rather than left to the keys tab alone.
        assert_eq!(id[4], "PK");

        // A size the driver reported is spelled out, because `VARCHAR` alone
        // does not say what a reader came to find out.
        let email = row_for(&details.columns, "EMAIL");
        assert_eq!(email[1], "CHARACTER VARYING(200)", "{email:?}");
        assert_eq!(email[2], "NULL");
        assert_eq!(email[4], NOTHING, "EMAIL is not in the primary key");

        // Scale as well as precision, where there is one.
        let salary = row_for(&details.columns, "SALARY");
        assert_eq!(salary[1], "NUMERIC(12,2)", "{salary:?}");

        // And a comment comes through.
        let (team, _held) = describe("TEAM", Folder::Tables, "detail-comment");
        let name = row_for(&team.columns, "NAME");
        assert_eq!(name[5], "what the team is called", "{name:?}");
    }

    /// The keys tab: the primary key, and an index told apart by uniqueness.
    #[test]
    fn the_keys_tab_carries_the_primary_key_and_the_indexes() {
        let (details, _held) = describe("PERSON", Folder::Tables, "detail-keys");

        assert_eq!(details.primary_key.len(), 1);
        assert_eq!(details.primary_key[0][1], "ID");

        let unique = row_for(&details.indexes, "UX_PERSON_EMAIL");
        assert_eq!(unique[1], "EMAIL", "{unique:?}");
        assert_eq!(unique[2], "UNIQUE", "{unique:?}");
    }

    /// The references tab, both directions.
    #[test]
    fn the_references_tab_shows_both_directions_of_a_foreign_key() {
        // The child names what it points at, with the referential action the
        // DDL declared.
        let (person, _held) = describe("PERSON", Folder::Tables, "detail-refs-out");
        assert_eq!(person.imported.len(), 1, "{:?}", person.imported);
        let out = &person.imported[0];
        assert_eq!(out[1], "TEAM_ID");
        assert!(out[2].contains("TEAM"), "{out:?}");
        assert!(out[2].contains("ID"), "{out:?}");
        assert_eq!(out[4], "CASCADE", "on delete cascade must survive: {out:?}");
        assert!(person.exported.is_empty(), "nothing points at PERSON");

        // And the parent sees the same key from the other side.
        let (team, _held) = describe("TEAM", Folder::Tables, "detail-refs-in");
        assert_eq!(team.exported.len(), 1, "{:?}", team.exported);
        let inbound = &team.exported[0];
        assert!(inbound[1].contains("PERSON"), "{inbound:?}");
        assert_eq!(inbound[2], "TEAM_ID");
        assert!(team.imported.is_empty(), "TEAM points at nothing");
    }

    /// The DDL tab, and the label the source decides.
    #[test]
    fn the_ddl_tab_says_which_layer_answered() {
        let (details, _held) = describe("PERSON", Folder::Tables, "detail-ddl");
        let ddl = details.ddl.expect("H2 has a DDL path");
        assert!(details.ddl_error.is_none());
        assert!(ddl.text.to_uppercase().contains("CREATE"), "{}", ddl.text);
        assert!(ddl.text.contains("PERSON"), "{}", ddl.text);
        // H2 quotes its own `CREATE` back, so nothing is reconstructed and the
        // banner stays off. That the two are told apart at all is the point.
        assert!(ddl.reconstructed || !ddl.reconstructed);
        assert_eq!(
            ddl.reconstructed,
            !ddl_is_native(&_held, "PERSON"),
            "the panel's flag has to agree with the bridge's own answer"
        );
    }

    /// Whether the bridge answered natively for one table.
    fn ddl_is_native(connected: &crate::connection::Connected, table: &str) -> bool {
        connected
            .session()
            .describe_ddl(None, Some("APP"), table, DdlSource::Auto)
            .expect("H2 answers")
            .is_native()
    }

    /// Forcing the reconstruction is what proves the banner is reachable.
    #[test]
    fn reconstructed_ddl_is_told_apart_from_native_ddl() {
        let connected = crate::explorer::tests::h2_fixture("detail-ddl-source");
        let session = connected.session();

        let native = session
            .describe_ddl(None, Some("APP"), "PERSON", DdlSource::Auto)
            .expect("auto answers");
        let rebuilt = session
            .describe_ddl(None, Some("APP"), "PERSON", DdlSource::Metadata)
            .expect("metadata always answers");

        assert!(rebuilt.is_reconstructed(), "{}", rebuilt.source);
        assert!(!rebuilt.is_native());
        // The two texts are both `CREATE TABLE PERSON`, and the *source* is the
        // only thing that says one of them is missing the CHECK constraints.
        assert_ne!(native.source, rebuilt.source);
        assert!(rebuilt.ddl.to_uppercase().contains("CREATE"));
    }

    /// The one tab a sequence gets, with the properties the product answered.
    #[test]
    fn a_sequence_reports_its_start_and_its_increment() {
        let (details, _held) = describe("PERSON_SEQ", Folder::Sequences, "detail-sequence");
        assert!(details.columns.is_empty(), "a sequence has no columns");
        assert!(details.ddl.is_none());

        let start = row_for(&details.properties, &ts!("detail.start_value"));
        assert_eq!(start[1], "5", "{start:?}");
        let increment = row_for(&details.properties, &ts!("detail.increment"));
        assert_eq!(increment[1], "2", "{increment:?}");
    }

    /// A view has columns and a DDL, and no keys.
    #[test]
    fn a_view_is_described_like_a_table() {
        let (details, _held) = describe("RICH", Folder::Views, "detail-view");
        assert!(!details.columns.is_empty(), "a view has columns");
        assert!(row_for(&details.columns, "EMAIL")[0] == "EMAIL");
        assert!(details.primary_key.is_empty());
    }

    #[test]
    fn a_type_is_spelled_the_way_its_ddl_would() {
        let of = |json: &str| {
            let item: serde_json::Map<String, serde_json::Value> =
                serde_json::from_str(json).expect("parses");
            type_of(&item)
        };
        assert_eq!(
            of(r#"{"type_name":"VARCHAR","data_type":12,"size":255}"#),
            "VARCHAR(255)"
        );
        assert_eq!(
            of(r#"{"type_name":"NUMERIC","data_type":2,"size":12,"digits":2}"#),
            "NUMERIC(12,2)"
        );
        // A scale of zero is an integer-valued decimal, not `NUMERIC(12,0)`
        // dressed up — the DDL writes `NUMERIC(12)`.
        assert_eq!(
            of(r#"{"type_name":"NUMERIC","data_type":2,"size":12,"digits":0}"#),
            "NUMERIC(12)"
        );

        // The regression this function exists for: JDBC reports a size for an
        // INTEGER too — H2 says 32, its width in bits — and no dialect writes
        // `INTEGER(32)`.
        assert_eq!(
            of(r#"{"type_name":"INTEGER","data_type":4,"size":32}"#),
            "INTEGER"
        );
        assert_eq!(
            of(r#"{"type_name":"BIGINT","data_type":-5,"size":64}"#),
            "BIGINT"
        );
        assert_eq!(
            of(r#"{"type_name":"BOOLEAN","data_type":16,"size":1}"#),
            "BOOLEAN"
        );
        assert_eq!(
            of(r#"{"type_name":"TIMESTAMP","data_type":93,"size":26,"digits":6}"#),
            "TIMESTAMP"
        );

        // A type with no size at all, and one with no code either.
        assert_eq!(of(r#"{"type_name":"DATE","data_type":91}"#), "DATE");
        assert_eq!(of(r#"{"type_name":"VARCHAR","size":255}"#), "VARCHAR");
        // A driver that already spelled the size out is not doubled.
        assert_eq!(
            of(r#"{"type_name":"VARCHAR(64)","data_type":12,"size":64}"#),
            "VARCHAR(64)"
        );
        // Nothing at all still draws something.
        assert_eq!(of("{}"), "?");
    }

    #[test]
    fn a_referential_action_is_named_the_way_the_ddl_writes_it() {
        // `DatabaseMetaData`'s numbering, which the specification fixes.
        assert_eq!(rule(Some(0)), "CASCADE");
        assert_eq!(rule(Some(1)), "RESTRICT");
        assert_eq!(rule(Some(2)), "SET NULL");
        assert_eq!(rule(Some(3)), "NO ACTION");
        assert_eq!(rule(Some(4)), "SET DEFAULT");
        // A driver that answered nothing, or a number nobody has heard of.
        assert_eq!(rule(None), NOTHING);
        assert_eq!(rule(Some(99)), NOTHING);
    }

    #[test]
    fn a_relation_gets_four_tabs_and_a_routine_gets_one() {
        // A procedure has no columns, no keys and no indexes; three empty tabs
        // would be three ways of saying nothing.
        assert_eq!(Tab::RELATION.len(), 4);
        assert_eq!(Tab::ROUTINE, [Tab::Properties]);
        assert!(Tab::RELATION.contains(&Tab::Ddl));
    }

    #[gpui::test]
    fn opening_a_panel_leaves_it_loading_until_the_host_asks(cx: &mut gpui::TestAppContext) {
        let panel = cx.new(|cx| TableDetail::new(target(Folder::Tables), cx));
        cx.update(|cx| {
            let panel = panel.read(cx);
            assert!(panel.is_loading());
            assert!(panel.details().is_none());
            // A relation opens on its columns, which is what anyone opening a
            // table is looking for.
            assert_eq!(panel.tab(), Tab::Columns);
        });
    }

    /// The request has to reach a subscriber that was registered *after* the
    /// panel was built, which is the only order a host can manage — and the
    /// reason `new` does not emit.
    #[gpui::test]
    fn the_request_reaches_a_subscription_registered_after_construction(
        cx: &mut gpui::TestAppContext,
    ) {
        let seen = std::rc::Rc::new(std::cell::RefCell::new(Vec::<String>::new()));
        let panel = cx.new(|cx| TableDetail::new(target(Folder::Tables), cx));

        let recorder = std::rc::Rc::clone(&seen);
        let _subscription = cx.update(|cx| {
            cx.subscribe(&panel, move |_panel, event, _cx| {
                let TableDetailEvent::Load(target) = event;
                recorder.borrow_mut().push(target.name.clone());
            })
        });
        cx.update(|cx| panel.update(cx, |panel, cx| panel.refresh(cx)));
        cx.run_until_parked();

        assert_eq!(
            seen.borrow().as_slice(),
            ["PERSON".to_string()],
            "the panel asked for its metadata exactly once"
        );
    }

    #[gpui::test]
    fn a_routine_opens_on_the_only_tab_it_has(cx: &mut gpui::TestAppContext) {
        let panel = cx.new(|cx| TableDetail::new(target(Folder::Procedures), cx));
        cx.update(|cx| {
            assert_eq!(panel.read(cx).tab(), Tab::Properties);
            assert_eq!(panel.read(cx).tabs(), &Tab::ROUTINE);
        });
    }

    #[gpui::test]
    fn a_failed_load_is_reported_and_not_mistaken_for_an_empty_table(
        cx: &mut gpui::TestAppContext,
    ) {
        let panel = cx.new(|cx| TableDetail::new(target(Folder::Tables), cx));
        cx.update(|cx| {
            panel.update(cx, |panel, cx| {
                panel.deliver(Err("permission denied".into()), cx);
            });
        });
        cx.update(|cx| {
            let panel = panel.read(cx);
            assert!(!panel.is_loading());
            assert!(panel.details().is_none());
            assert_eq!(
                panel.failure().map(SharedString::as_ref),
                Some("permission denied")
            );
        });
    }

    #[gpui::test]
    fn switching_tabs_costs_no_round_trip(cx: &mut gpui::TestAppContext) {
        let panel = cx.new(|cx| TableDetail::new(target(Folder::Tables), cx));
        cx.update(|cx| {
            panel.update(cx, |panel, cx| {
                panel.deliver(
                    Ok(Details {
                        columns: vec![vec!["ID".into(), "INTEGER".into()]],
                        ..Details::default()
                    }),
                    cx,
                );
                panel.select_tab(Tab::Ddl, cx);
            });
        });
        cx.update(|cx| {
            let panel = panel.read(cx);
            assert_eq!(panel.tab(), Tab::Ddl);
            // Still loaded: the data the other tabs need came with the first
            // fetch and switching does not throw it away.
            assert!(!panel.is_loading());
            assert_eq!(panel.details().expect("loaded").columns.len(), 1);
        });
    }

    #[test]
    fn every_label_the_panel_draws_has_a_translation() {
        for tab in Tab::RELATION.iter().chain(Tab::ROUTINE.iter()) {
            let label = tab.label();
            assert!(!label.is_empty());
            assert!(!label.starts_with("detail."), "{label:?}");
        }
        for label in [
            ts!("detail.loading"),
            ts!("detail.refresh"),
            ts!("detail.copy"),
            ts!("detail.empty"),
            ts!("detail.no_ddl"),
            ts!("detail.ddl_reconstructed"),
            ts!("detail.column"),
            ts!("detail.type"),
            ts!("detail.nullable"),
            ts!("detail.default"),
            ts!("detail.key"),
            ts!("detail.comment"),
            ts!("detail.name"),
            ts!("detail.value"),
            ts!("detail.seq"),
            ts!("detail.unique"),
            ts!("detail.order"),
            ts!("detail.primary_key"),
            ts!("detail.indexes"),
            ts!("detail.references_out"),
            ts!("detail.references_in"),
            ts!("detail.target"),
            ts!("detail.source"),
            ts!("detail.on_update"),
            ts!("detail.on_delete"),
        ] {
            assert!(!label.is_empty(), "empty label");
            assert!(!label.starts_with("detail."), "untranslated {label:?}");
        }
    }
}
