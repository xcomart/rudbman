//! The structure pane: one table's shape, and the `ALTER TABLE` batch that
//! would change it.
//!
//! What the architecture document's §7.10 draws, and a sibling of
//! [`crate::data_pane`] rather than a fifth tab of [`crate::table_detail`]: the
//! panel keeps its rows as display strings — a column's size folded into
//! `VARCHAR(255)`, its nullability into the word `NOT NULL` — so nothing an
//! editor needs survives in it. This pane issues its own
//! `DESCRIBE columns` / `primary_keys` / `imported_keys` / `indexes` and keeps
//! what it reads.
//!
//! # What it holds and what it shows
//!
//! One [`Structure`], read once and never modified, and one [`StructEdits`]
//! staged beside it. Everything drawn is the *effective* value —
//! [`StructEdits::draft`] — so a row the user has typed into shows what they
//! typed while the snapshot underneath still says what the catalog does, which
//! is what lets the plan carry both sides (§7.10).
//!
//! # Nothing here sends anything
//!
//! This is the first half of §7.10, the way `0919ae6` was the first half of
//! §7.9: the pane loads, stages, and shows the statements the staging would
//! produce. Running them is the other half. That is why the statements block is
//! read-only text and why there is no Apply button to disable — an apply that
//! is not written yet is better absent than greyed.
//!
//! # The form is one column at a time
//!
//! Three [`TextInput`]s and two checkboxes, refilled as the selection moves,
//! rather than an input per cell: a table of two hundred columns would
//! otherwise be six hundred entities, and a column's four fields are read
//! together anyway. Every keystroke stages immediately, so moving the selection
//! needs no commit step and nothing can be lost by clicking elsewhere.

use std::collections::BTreeMap;

use gpui::{
    AnyElement, App, Context, Div, DragMoveEvent, Entity, FocusHandle, Focusable, IntoElement,
    MouseButton, MouseUpEvent, Render, ScrollHandle, SharedString, Subscription, Window, div,
    prelude::*, px,
};
use rudbman_core::ConnectionProfile;
use rudbman_jdbc::{DescribeRequest, Session};
use rudbman_sql::{ConstraintDrop, ConstraintKind, Dialect, TableAlter, plan_alter};
use rudbman_ui::{
    Button, ButtonVariant, Checkbox, DraggedThumb, Scrollbar, ScrollbarAxis, ScrollbarState,
    TextInput, Theme, form_row, hide_later, hide_now, scroll_to, scrolled, theme,
};

use crate::builder_sql;
use crate::connection::SessionHandle;
use crate::explorer::{ConnectionId, ObjectTarget};
use crate::i18n::ts;
use crate::icons;
use crate::query::note;
use crate::struct_edit::{
    ColumnDraft, ColumnField, DraftValue, LoadedColumn, LoadedConstraint, PlanError, StructEdits,
    Structure,
};
use crate::table_detail::{NOTHING, flag, items, number, text, type_of};

/// Element id of the body's overlay scroll indicator.
const BODY_SCROLLBAR: &str = "struct-body-scrollbar";

/// One item of a `DESCRIBE` answer.
type Item = serde_json::Map<String, serde_json::Value>;

/// Where the pane's structure has got to.
///
/// [`crate::table_detail::Load`]'s shape: one fetch, and the two ways it can
/// end.
enum Load {
    /// A fetch is out.
    Running,
    /// It came back.
    Ready(Box<Structure>),
    /// It failed; the driver's own message.
    Failed(SharedString),
}

/// Which column the editor form is filled from.
///
/// Two spaces, because an added column has no snapshot row to be indexed
/// against — see [`StructEdits`]. Both are positions into collections that a
/// reload throws away, which is why nothing outside one frame holds a
/// [`Selection`] across a refresh.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Selection {
    /// A row of the snapshot, by index into [`Structure::columns`].
    Column(usize),
    /// A column being added, by position in [`StructEdits::added`].
    Added(usize),
}

/// One table's structure, and whatever is staged against it.
pub struct StructPane {
    /// What is being edited. Also the tab's title, and the identity two "edit
    /// structure" gestures are deduplicated on.
    target: ObjectTarget,
    /// Which connection tab this pane belongs to.
    connection: ConnectionId,
    /// The session the metadata is read over.
    ///
    /// `None` once [`StructPane::detach`] has run: the tab outlives its
    /// connection, because the structure already read is worth looking at, but
    /// nothing more can be asked for.
    session: Option<SessionHandle>,
    /// Writes the statements, and nothing here writes one by hand.
    dialect: Dialect,
    /// The profile refuses writes outright (§8), so nothing can be staged.
    read_only: bool,
    /// Where the fetch has got to.
    load: Load,
    /// Everything staged against [`Load::Ready`]'s structure.
    edits: StructEdits,
    /// The column the form below the table is filled from.
    selected: Option<Selection>,
    /// The form's three fields: name, type, default.
    name_input: Entity<TextInput>,
    type_input: Entity<TextInput>,
    default_input: Entity<TextInput>,
    /// The table's new bare name.
    rename_input: Entity<TextInput>,
    /// What was last *put into* the four fields, or last read out of them.
    ///
    /// The guard that keeps refilling the form from staging an edit nobody
    /// made. gpui delivers an observation after the update that caused it, so a
    /// flag set and cleared around [`TextInput::set_content`] would already be
    /// clear by the time the handler ran; comparing against the last value
    /// seen is decided in the handler itself and cannot race. Updated on the
    /// way out as well as on the way in, so a field typed over and typed back
    /// stages the value it was typed back to rather than being mistaken for a
    /// refill.
    filled: [String; 3],
    /// The same guard for the rename field.
    filled_rename: String,
    /// Keeps the four field observations alive for as long as the pane.
    _watch: [Subscription; 4],
    /// The generation of the newest load. Every delivery carries one, and one
    /// that is not this is an answer a later load has already replaced.
    ///
    /// One async path today and two from the commit that applies a batch: a
    /// successful apply reloads (§7.10), and its reload has to be able to
    /// supersede whatever a refresh had already asked for.
    generation: u64,
    /// A line the pane wants to say without it being a failure.
    notice: Option<SharedString>,
    focus_handle: FocusHandle,
    /// Scroll of everything below the header.
    body_scroll: ScrollHandle,
    /// Whether the body's overlay bar is on screen.
    body_scrollbar: ScrollbarState,
}

impl StructPane {
    /// A pane over `target`, before it has asked for anything.
    ///
    /// It does **not** load, for the reason [`crate::table_detail::TableDetail`]
    /// does not: the host is still inside `cx.new` and has a tab to open first.
    /// [`StructPane::refresh`] is the one way in, and it is also what the
    /// header's button calls.
    pub fn new(
        session: SessionHandle,
        connection: ConnectionId,
        target: ObjectTarget,
        profile: &ConnectionProfile,
        driver_dialect: &str,
        cx: &mut Context<Self>,
    ) -> Self {
        // Disabled once and for the pane's life rather than per render: both
        // halves of "this may not be written" — the profile's flag and the
        // absence of an apply — are settled before the first frame.
        let read_only = profile.read_only;
        let field = |cx: &mut Context<Self>| cx.new(|cx| TextInput::new(cx).disabled(read_only));
        let name_input = field(cx);
        let type_input = field(cx);
        let default_input = field(cx);
        let rename_input = field(cx);

        // Observed rather than left alone: a `TextInput` emits nothing, so the
        // only way a keystroke reaches the staging buffer is for the pane to
        // watch the entity and read it back.
        let watch = [
            Self::watch(&name_input, ColumnField::Name, cx),
            Self::watch(&type_input, ColumnField::Type, cx),
            Self::watch(&default_input, ColumnField::Default, cx),
            cx.observe(&rename_input, |pane, input, cx| {
                let typed = input.read(cx).content().to_string();
                pane.renamed(typed, cx);
            }),
        ];

        Self {
            target,
            connection,
            session: Some(session),
            dialect: Dialect::from_id(driver_dialect),
            read_only,
            load: Load::Running,
            edits: StructEdits::new(),
            selected: None,
            name_input,
            type_input,
            default_input,
            rename_input,
            filled: Default::default(),
            filled_rename: String::new(),
            _watch: watch,
            generation: 0,
            notice: None,
            focus_handle: cx.focus_handle(),
            body_scroll: ScrollHandle::new(),
            body_scrollbar: ScrollbarState::new(),
        }
    }

    /// Watches one of the three column fields.
    fn watch(
        input: &Entity<TextInput>,
        field: ColumnField,
        cx: &mut Context<Self>,
    ) -> Subscription {
        cx.observe(input, move |pane, input, cx| {
            let typed = input.read(cx).content().to_string();
            pane.typed(field, typed, cx);
        })
    }

    /// The object whose structure this is.
    pub fn target(&self) -> &ObjectTarget {
        &self.target
    }

    /// Which connection tab this pane runs against.
    pub fn connection(&self) -> ConnectionId {
        self.connection
    }

    /// Lets the session go, leaving the tab standing.
    ///
    /// The same bargain a data pane's detach strikes (§9.3): what stays is the
    /// structure already read, which is the user's to look at.
    pub fn detach(&mut self, cx: &mut Context<Self>) {
        self.session = None;
        cx.notify();
    }

    /// Whether the pane is holding edits nobody has sent anywhere.
    ///
    /// Asked by the tab strip before a close and by [`StructPane::refresh`]
    /// before a reload, for one reason: a staged edit is keyed to an index into
    /// the snapshot it was staged against, and a snapshot read again is a new
    /// set of indices (§7.10).
    pub fn has_pending_edits(&self) -> bool {
        !self.edits.is_empty()
    }

    /// Says, in the pane itself, why the gesture that was just tried is being
    /// refused while changes are staged.
    ///
    /// The counterpart of [`StructPane::has_pending_edits`] for the shell: a
    /// tab that simply would not close, with nothing said, would read as a bug.
    pub fn warn_pending(&mut self, cx: &mut Context<Self>) {
        self.notice = Some(ts!("struct.discard_first"));
        cx.notify();
    }

    /// Whether the keyboard is anywhere inside this pane, as the last drawn
    /// frame had it.
    ///
    /// The four fields as well as the pane's own handle, for the reason a data
    /// pane asks about its grid: a focus left on a field of a tab that has
    /// stopped being rendered swallows every shortcut in the window.
    pub fn contains_focus(&self, window: &Window, cx: &App) -> bool {
        if self.focus_handle.contains_focused(window, cx) {
            return true;
        }
        self.fields()
            .iter()
            .any(|input| input.read(cx).focus_handle(cx).contains_focused(window, cx))
    }

    /// Puts the keyboard on the pane.
    pub fn take_focus(&self, window: &mut Window, cx: &mut Context<Self>) {
        window.focus(&self.focus_handle);
        cx.notify();
    }

    /// The four fields, in the order they are drawn.
    fn fields(&self) -> [&Entity<TextInput>; 4] {
        [
            &self.name_input,
            &self.type_input,
            &self.default_input,
            &self.rename_input,
        ]
    }

    /// Why nothing here can be staged, when something says so.
    ///
    /// One reason today where the data pane has two, and stated in the same
    /// place and the same weight: a standing fact about what is being shown,
    /// not a failure, and the answer to a question the user has not asked yet.
    fn read_only_reason(&self) -> Option<SharedString> {
        self.read_only.then(|| ts!("struct.read_only"))
    }

    /// Reads the structure again, from scratch.
    ///
    /// Refuses while anything is staged, exactly as a data pane's refresh
    /// does: the indices the staging holds are the snapshot's, and a snapshot
    /// read again is a different one.
    pub fn refresh(&mut self, cx: &mut Context<Self>) {
        if self.has_pending_edits() {
            self.notice = Some(ts!("struct.discard_first"));
            cx.notify();
            return;
        }
        let Some(session) = self.session.clone() else {
            self.notice = Some(ts!("explorer.disconnected"));
            cx.notify();
            return;
        };

        self.generation += 1;
        let generation = self.generation;
        self.load = Load::Running;
        self.notice = None;
        self.select(None, cx);

        let target = self.target.clone();
        cx.spawn(async move |pane, cx| {
            let outcome = cx
                .background_spawn(async move { load_structure(session.session(), &target) })
                .await;
            pane.update(cx, |pane, cx| pane.deliver(generation, outcome, cx))
                .ok();
        })
        .detach();
        cx.notify();
    }

    /// Records what one load produced.
    fn deliver(
        &mut self,
        generation: u64,
        outcome: Result<Structure, String>,
        cx: &mut Context<Self>,
    ) {
        if generation != self.generation {
            // A superseded load's answer, and one nothing on screen is keyed to.
            return;
        }
        // Whatever was staged was staged against the structure being replaced.
        self.edits.clear();
        self.selected = None;
        self.load = match outcome {
            Ok(structure) => Load::Ready(Box::new(structure)),
            Err(message) => Load::Failed(SharedString::from(message)),
        };
        self.fill_rename(cx);
        self.fill_fields(cx);
        cx.notify();
    }

    /// The table's own bare name, as the catalog spells it.
    fn bare_name(&self) -> &str {
        &self.target.name
    }

    /// The structure, once it has been read.
    fn structure(&self) -> Option<&Structure> {
        match &self.load {
            Load::Ready(structure) => Some(structure),
            Load::Running | Load::Failed(_) => None,
        }
    }

    /// Whether a fetch is out.
    fn is_loading(&self) -> bool {
        matches!(self.load, Load::Running)
    }

    /// The effective draft of whichever column is selected.
    fn selected_draft(&self) -> Option<ColumnDraft> {
        match self.selected? {
            Selection::Column(index) => Some(self.edits.draft(self.structure()?, index)),
            Selection::Added(position) => self.edits.added().get(position).cloned(),
        }
    }

    /// Moves the form onto another column, and refills it.
    fn select(&mut self, selection: Option<Selection>, cx: &mut Context<Self>) {
        self.selected = selection;
        self.fill_fields(cx);
        cx.notify();
    }

    /// Puts the table's effective name into the rename field.
    ///
    /// The staged one where there is one and the catalog's own where there is
    /// not, which is [`StructPane::fill_fields`]'s rule for the other three
    /// fields. That it always answers the catalog today — a reload and a
    /// discard both clear the staging first — is a fact about its two callers
    /// and not about what the field is meant to show.
    fn fill_rename(&mut self, cx: &mut Context<Self>) {
        self.filled_rename = self
            .edits
            .rename_to()
            .unwrap_or_else(|| self.bare_name())
            .to_string();
        let name = self.filled_rename.clone();
        self.rename_input
            .update(cx, |input, cx| input.set_content(name, cx));
    }

    /// Puts the selected column's values into the three fields.
    ///
    /// `filled` is written *before* the fields are, so that the observation
    /// each `set_content` provokes reads its own value back and stages nothing.
    fn fill_fields(&mut self, cx: &mut Context<Self>) {
        let draft = self.selected_draft().unwrap_or_default();
        self.filled = [
            draft.name,
            draft.type_sql,
            draft.default_sql.unwrap_or_default(),
        ];
        let inputs = [
            self.name_input.clone(),
            self.type_input.clone(),
            self.default_input.clone(),
        ];
        for (input, value) in inputs.into_iter().zip(self.filled.clone()) {
            input.update(cx, |input, cx| input.set_content(value, cx));
        }
    }

    /// One of the three fields was typed into.
    fn typed(&mut self, field: ColumnField, value: String, cx: &mut Context<Self>) {
        let (slot, staged) = match field {
            ColumnField::Name => (0, DraftValue::Name(value.clone())),
            ColumnField::Type => (1, DraftValue::Type(value.clone())),
            // Typing into the default field is what gives a column a default;
            // the box beside it is how one is taken away. That is the whole
            // reason the pair exists: `None` and `Some("")` are two states the
            // server distinguishes, and one field cannot say both.
            ColumnField::Default => (2, DraftValue::Default(Some(value.clone()))),
            // Not a field: the nullability is a box, and it stages through
            // [`StructPane::set_field`] directly.
            ColumnField::NotNull => return,
        };
        // The value this field was last seen holding. Equal means the field is
        // being refilled from the selection rather than typed into, and staging
        // that would mark a row the user only clicked on as edited.
        if self.filled[slot] == value {
            return;
        }
        self.set_field(staged, cx);
    }

    /// The rename field was typed into.
    ///
    /// A field holding the name the table already has is not a rename — it is
    /// the pre-filled value, and leaving it alone is the commonest thing to do
    /// with it — so it stages nothing rather than staging an edit that would
    /// plan to nothing while counting as pending.
    fn renamed(&mut self, value: String, cx: &mut Context<Self>) {
        if self.filled_rename == value {
            return;
        }
        self.filled_rename = value.clone();
        if self.read_only {
            return;
        }
        self.edits
            .set_rename((value != self.bare_name()).then_some(value));
        cx.notify();
    }

    /// Stages one field of the selected column.
    ///
    /// The guard is brought into step with whatever is being staged, and here
    /// rather than in [`StructPane::typed`] because two of the four controls
    /// are boxes: a default the box has just taken away would otherwise leave
    /// the field's guard still holding the old text, and the next keystroke in
    /// that field would read as a refill and stage nothing.
    fn set_field(&mut self, value: DraftValue, cx: &mut Context<Self>) {
        if self.read_only {
            return;
        }
        let Some(selected) = self.selected else {
            return;
        };
        match (value.field(), &value) {
            (ColumnField::Name, DraftValue::Name(typed)) => self.filled[0] = typed.clone(),
            (ColumnField::Type, DraftValue::Type(typed)) => self.filled[1] = typed.clone(),
            (ColumnField::Default, DraftValue::Default(typed)) => {
                self.filled[2] = typed.clone().unwrap_or_default();
            }
            // The nullability, which lands in no field at all.
            _ => {}
        }
        match selected {
            Selection::Column(index) => {
                let Load::Ready(structure) = &self.load else {
                    return;
                };
                self.edits.set_column(structure, index, value);
            }
            Selection::Added(position) => self.edits.set_added(position, value),
        }
        cx.notify();
    }

    /// Turns a column's default on or off.
    ///
    /// The fields are refilled afterwards rather than left as they were: with
    /// the default dropped the text beside the box says nothing true, and a
    /// form that disagrees with the row above it is worse than one that forgot
    /// what was typed.
    fn set_has_default(&mut self, has_default: bool, cx: &mut Context<Self>) {
        let value = has_default.then(|| self.filled[2].clone());
        self.set_field(DraftValue::Default(value), cx);
        self.fill_fields(cx);
    }

    /// Appends a column and starts filling it in.
    fn add_column(&mut self, cx: &mut Context<Self>) {
        if self.read_only || self.structure().is_none() {
            return;
        }
        let position = self.edits.add_column();
        self.select(Some(Selection::Added(position)), cx);
    }

    /// Takes an added column back off the list.
    ///
    /// The selection goes with it whatever it pointed at: the positions after
    /// this one all move up, and a form left filled from the old numbering
    /// would be editing a different column than the one on screen.
    fn remove_added(&mut self, position: usize, cx: &mut Context<Self>) {
        if self.read_only {
            return;
        }
        self.edits.remove_added(position);
        self.select(None, cx);
    }

    /// Marks a column to be dropped, or takes the mark off.
    ///
    /// Marking one discards whatever was staged against it — that is
    /// [`StructEdits::toggle_column_drop`]'s rule, not this one — so the form
    /// lets go of it too rather than going on offering fields for a column that
    /// is going.
    fn toggle_drop(&mut self, index: usize, cx: &mut Context<Self>) {
        if self.read_only {
            return;
        }
        let dropped = self.edits.toggle_column_drop(index);
        if dropped && self.selected == Some(Selection::Column(index)) {
            self.select(None, cx);
            return;
        }
        self.fill_fields(cx);
        cx.notify();
    }

    /// Marks a constraint to be dropped, or takes the mark off.
    fn toggle_constraint_drop(&mut self, index: usize, cx: &mut Context<Self>) {
        if self.read_only {
            return;
        }
        self.edits.toggle_constraint_drop(index);
        cx.notify();
    }

    /// Throws away everything staged.
    fn discard(&mut self, cx: &mut Context<Self>) {
        self.edits.clear();
        self.notice = None;
        self.fill_rename(cx);
        self.select(None, cx);
    }

    /// The statements the staged edits would send, as the block under the
    /// tables shows them.
    ///
    /// `None` while nothing is staged or nothing has been read — an empty
    /// staging plans to an empty batch, and a box with nothing in it says less
    /// than no box at all. Computed on every render rather than kept in a
    /// field: it is a function of the snapshot, the staging and the dialect,
    /// and a cached copy would be one more thing that can be stale.
    fn plan(&self) -> Option<Result<Vec<String>, PlanError>> {
        let structure = self.structure()?;
        if self.edits.is_empty() {
            return None;
        }
        Some(self.edits.plan(structure, &self.dialect))
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
            hide_later(epoch, cx, |pane: &mut Self| Some(&mut pane.body_scrollbar));
        }
    }

    /// Lets go of the body's thumb and starts its clock again.
    fn release(&mut self, cx: &mut Context<Self>) {
        if let Some(epoch) = self.body_scrollbar.release() {
            hide_later(epoch, cx, |pane: &mut Self| Some(&mut pane.body_scrollbar));
            cx.notify();
        }
    }

    /// Puts the bar up while the pointer rests on the edge it rides.
    fn hover_scrollbar(&mut self, hovered: bool, cx: &mut Context<Self>) {
        if hovered {
            if self.body_scrollbar.hover_enter() {
                cx.notify();
            }
            return;
        }
        if let Some(epoch) = self.body_scrollbar.hover_leave() {
            hide_now(self, epoch, cx, |pane: &mut Self| {
                Some(&mut pane.body_scrollbar)
            });
        }
    }

    /// The header: what is being edited, how much is staged, and the two
    /// buttons that act on all of it.
    fn render_header(&self, chrome: &Theme, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let this = cx.entity();
        let loading = self.is_loading();
        let staged = self.has_pending_edits();
        let pending = staged.then(|| ts!("struct.pending", count = self.edits.pending_count()));

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
                        .child(ts!("struct.loading")),
                )
            })
            .children(pending.map(|pending| {
                div()
                    .flex_none()
                    .whitespace_nowrap()
                    .text_size(px(11.))
                    .text_color(chrome.accent)
                    .child(pending)
            }))
            .child({
                let this = this.clone();
                Button::new("struct-discard", ts!("struct.discard"))
                    .variant(ButtonVariant::Secondary)
                    .disabled(!staged)
                    .on_click(move |_, _window, cx| {
                        this.update(cx, |pane, cx| pane.discard(cx));
                    })
            })
            .child(
                Button::new("struct-refresh", ts!("struct.refresh"))
                    .variant(ButtonVariant::Secondary)
                    .disabled(loading)
                    .on_click(move |_, _window, cx| {
                        this.update(cx, |pane, cx| pane.refresh(cx));
                    }),
            )
    }

    /// The one line that says why nothing here can be staged.
    fn render_banner(&self, chrome: &Theme) -> Option<impl IntoElement + use<>> {
        let reason = self.read_only_reason()?;
        Some(
            div()
                .flex_none()
                .px(px(10.))
                .py(px(4.))
                .border_b_1()
                .border_color(chrome.border)
                .bg(chrome.surface)
                .text_size(px(11.))
                .text_color(chrome.text_muted)
                .child(reason),
        )
    }

    /// The structure, or the one line that stands in for it.
    fn render_body(&self, chrome: &Theme, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let content = match &self.load {
            Load::Running => note(ts!("struct.loading"), chrome.text_muted),
            Load::Failed(message) => note(message.clone(), chrome.danger),
            Load::Ready(structure) => self.render_structure(structure, chrome, cx),
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
                    .id("struct-body")
                    .track_scroll(&self.body_scroll)
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_h_0()
                    .gap(px(14.))
                    .p(px(10.))
                    .overflow_y_scroll()
                    .child(content),
            )
            .children(
                self.scrollbar()
                    .on_hover(cx.listener(|pane, hovered: &bool, _window, cx| {
                        pane.hover_scrollbar(*hovered, cx);
                    }))
                    .render(chrome),
            )
    }

    /// The four blocks: the columns, the form, the constraints, the rename —
    /// and the statements they add up to.
    fn render_structure(
        &self,
        structure: &Structure,
        chrome: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        div()
            .flex()
            .flex_col()
            .gap(px(14.))
            .child(self.render_columns(structure, chrome, cx))
            .child(self.render_editor(chrome, cx))
            .child(self.render_constraints(structure, chrome, cx))
            .child(self.render_rename(chrome))
            .children(self.render_statements(chrome, cx))
            .into_any_element()
    }

    /// The columns: one row per snapshot column, then one per added column.
    ///
    /// A dropped row is struck through and stays where it is rather than
    /// vanishing, for §7.9's reason: a row that disappears when it is marked
    /// takes with it the one thing that would let the mark be taken off again.
    fn render_columns(
        &self,
        structure: &Structure,
        chrome: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let this = cx.entity();
        let writable = self.read_only_reason().is_none();

        let rows = (0..structure.columns.len()).map(|index| {
            let draft = self.edits.draft(structure, index);
            let dropped = self.edits.is_column_dropped(index);
            let state = if dropped {
                ts!("struct.state_dropped")
            } else if self.edits.is_column_changed(index) {
                ts!("struct.state_changed")
            } else {
                NOTHING
            };
            let label = if dropped {
                ts!("struct.keep")
            } else {
                ts!("struct.drop")
            };
            let selector = this.clone();
            let dropper = this.clone();
            // The row's *index* in the id, never its name: an element id that
            // changed while a name was being retyped would make gpui treat the
            // row as a new one on every keystroke.
            self.row(
                ("struct-column", index),
                &draft,
                state,
                dropped,
                self.selected == Some(Selection::Column(index)),
                index % 2 == 1,
                chrome,
                move |_window, cx| {
                    selector.update(cx, |pane, cx| {
                        pane.select(Some(Selection::Column(index)), cx);
                    });
                },
            )
            .child(
                div()
                    .flex_none()
                    .w(px(84.))
                    .px(px(4.))
                    .children(writable.then(|| {
                        Button::new(("struct-drop", index), label)
                            .variant(ButtonVariant::Secondary)
                            .on_click(move |_, _window, cx| {
                                dropper.update(cx, |pane, cx| pane.toggle_drop(index, cx));
                            })
                    })),
            )
        });

        let added = self
            .edits
            .added()
            .iter()
            .enumerate()
            .map(|(position, draft)| {
                let selector = this.clone();
                let remover = this.clone();
                self.row(
                    ("struct-added", position),
                    draft,
                    ts!("struct.state_added"),
                    false,
                    self.selected == Some(Selection::Added(position)),
                    (structure.columns.len() + position) % 2 == 1,
                    chrome,
                    move |_window, cx| {
                        selector.update(cx, |pane, cx| {
                            pane.select(Some(Selection::Added(position)), cx);
                        });
                    },
                )
                .child(
                    div()
                        .flex_none()
                        .w(px(84.))
                        .px(px(4.))
                        .children(writable.then(|| {
                            Button::new(("struct-remove", position), ts!("struct.remove"))
                                .variant(ButtonVariant::Secondary)
                                .on_click(move |_, _window, cx| {
                                    remover.update(cx, |pane, cx| pane.remove_added(position, cx));
                                })
                        })),
                )
            });

        let adder = this.clone();
        let add = writable.then(|| {
            Button::new("struct-add-column", ts!("struct.add_column"))
                .variant(ButtonVariant::Secondary)
                .on_click(move |_, _window, cx| {
                    adder.update(cx, |pane, cx| pane.add_column(cx));
                })
        });

        section(
            ts!("struct.columns"),
            div()
                .flex()
                .flex_col()
                .w_full()
                .child(
                    headings(
                        [
                            ts!("struct.column"),
                            ts!("struct.type"),
                            ts!("struct.nullable"),
                            ts!("struct.default"),
                            ts!("struct.state"),
                        ],
                        chrome,
                    )
                    .child(div().flex_none().w(px(84.))),
                )
                .children(rows)
                .children(added)
                .child(div().flex().flex_row().pt(px(6.)).children(add)),
            chrome,
        )
        .into_any_element()
    }

    /// One column row, up to the button at the end of it.
    #[allow(clippy::too_many_arguments)]
    fn row(
        &self,
        id: (&'static str, usize),
        draft: &ColumnDraft,
        state: SharedString,
        struck: bool,
        selected: bool,
        zebra: bool,
        chrome: &Theme,
        click: impl Fn(&mut Window, &mut App) + 'static,
    ) -> gpui::Stateful<Div> {
        let cells = [
            (SharedString::from(draft.name.clone()), false),
            (SharedString::from(draft.type_sql.clone()), true),
            // SQL keywords, not words: NULL and NOT NULL read the same in every
            // language and are what the column's own DDL says.
            (
                SharedString::new_static(if draft.not_null { "NOT NULL" } else { "NULL" }),
                true,
            ),
            (
                draft
                    .default_sql
                    .clone()
                    .map_or(NOTHING, SharedString::from),
                true,
            ),
            (state, true),
        ];

        div()
            .id(id)
            .flex()
            .flex_row()
            .items_center()
            .cursor_pointer()
            // Zebra striping, because a row of five truncated cells is hard to
            // follow across otherwise — and the selected row instead of it,
            // because which row the form is filled from matters more.
            .when(zebra && !selected, |row| row.bg(chrome.surface))
            .when(selected, |row| row.bg(chrome.surface_hover))
            .children(cells.into_iter().map(|(text, muted)| {
                cell(text, muted, chrome).when(struck, |cell| cell.line_through())
            }))
            .on_click(move |_, window, cx| click(window, cx))
    }

    /// The form: the selected column's four fields, one column at a time.
    fn render_editor(&self, chrome: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let Some(draft) = self.selected_draft() else {
            return section(
                ts!("struct.edit_column"),
                div()
                    .text_size(px(11.))
                    .text_color(chrome.text_muted)
                    .child(ts!("struct.no_selection")),
                chrome,
            )
            .into_any_element();
        };
        let this = cx.entity();
        let nulls = this.clone();
        let writable = self.read_only_reason().is_none();

        section(
            ts!("struct.edit_column"),
            div()
                .flex()
                .flex_col()
                .gap(px(8.))
                .child(form_row(ts!("struct.name"), self.name_input.clone()))
                .child(form_row(ts!("struct.type"), self.type_input.clone()))
                .child(form_row(ts!("struct.default"), self.default_input.clone()))
                .child(
                    div()
                        .flex()
                        .flex_row()
                        .gap(px(16.))
                        .child(
                            // The SQL keyword, untranslated: it is what the
                            // column's own DDL says.
                            Checkbox::new("struct-not-null", SharedString::new_static("NOT NULL"))
                                .checked(draft.not_null)
                                .on_toggle(move |checked, _window, cx| {
                                    nulls.update(cx, |pane, cx| {
                                        pane.set_field(DraftValue::NotNull(checked), cx);
                                    });
                                }),
                        )
                        .child(
                            Checkbox::new("struct-has-default", ts!("struct.has_default"))
                                .checked(draft.default_sql.is_some())
                                .on_toggle(move |checked, _window, cx| {
                                    this.update(cx, |pane, cx| pane.set_has_default(checked, cx));
                                }),
                        ),
                )
                .when(!writable, |form| form.opacity(0.6)),
            chrome,
        )
        .into_any_element()
    }

    /// The constraints, and the toggle that drops one.
    ///
    /// The toggle is drawn disabled where the dialect cannot drop a constraint
    /// at all rather than left out, and a line underneath says which product
    /// and why: §7.8's rule that a surface documents itself, and the same
    /// sentence the generator would have refused with.
    fn render_constraints(
        &self,
        structure: &Structure,
        chrome: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let this = cx.entity();
        let writable = self.read_only_reason().is_none();
        let droppable = drops_constraints(&self.dialect);

        let body =
            if structure.constraints.is_empty() {
                div()
                    .text_size(px(11.))
                    .text_color(chrome.text_muted)
                    .child(ts!("struct.empty"))
            } else {
                let rows =
                    structure
                        .constraints
                        .iter()
                        .enumerate()
                        .map(|(index, constraint)| {
                            let dropped = self.edits.is_constraint_dropped(index);
                            let this = this.clone();
                            let label = if dropped {
                                ts!("struct.keep")
                            } else {
                                ts!("struct.drop")
                            };
                            div()
                                .flex()
                                .flex_row()
                                .items_center()
                                .when(index % 2 == 1, |row| row.bg(chrome.surface))
                                .children(
                                    [
                                        (kind_name(constraint.kind), false),
                                        (
                                            if constraint.name.is_empty() {
                                                NOTHING
                                            } else {
                                                SharedString::from(constraint.name.clone())
                                            },
                                            true,
                                        ),
                                        (SharedString::from(constraint.columns.join(", ")), true),
                                    ]
                                    .into_iter()
                                    .map(|(text, muted)| {
                                        cell(text, muted, chrome)
                                            .when(dropped, |cell| cell.line_through())
                                    }),
                                )
                                .child(div().flex_none().w(px(84.)).px(px(4.)).children(
                                    writable.then(|| {
                                        Button::new(("struct-drop-constraint", index), label)
                                            .variant(ButtonVariant::Secondary)
                                            .disabled(!droppable)
                                            .on_click(move |_, _window, cx| {
                                                this.update(cx, |pane, cx| {
                                                    pane.toggle_constraint_drop(index, cx);
                                                });
                                            })
                                    }),
                                ))
                        });

                div()
                    .flex()
                    .flex_col()
                    .w_full()
                    .child(
                        headings(
                            [
                                ts!("struct.kind"),
                                ts!("struct.name"),
                                ts!("struct.columns"),
                            ],
                            chrome,
                        )
                        .child(div().flex_none().w(px(84.))),
                    )
                    .children(rows)
                    .children((!droppable).then(|| {
                        div()
                            .pt(px(6.))
                            .text_size(px(11.))
                            .text_color(chrome.text_muted)
                            .child(ts!(
                                "struct.no_constraint_drop",
                                dialect = self.dialect.name()
                            ))
                    }))
            };

        section(ts!("struct.constraints"), body, chrome).into_any_element()
    }

    /// The rename field, pre-filled with the table's bare name.
    fn render_rename(&self, chrome: &Theme) -> AnyElement {
        section(
            ts!("struct.rename_table"),
            form_row(ts!("struct.new_name"), self.rename_input.clone()),
            chrome,
        )
        .into_any_element()
    }

    /// The statements the staged edits would send, or the refusal they earned.
    fn render_statements(&self, chrome: &Theme, cx: &mut Context<Self>) -> Option<AnyElement> {
        let mono = crate::app_settings::monospace_family(cx);
        let body = match self.plan()? {
            // Nothing to send. A box with nothing in it would say less than no
            // box at all.
            Ok(statements) if statements.is_empty() => return None,
            Ok(statements) => div()
                .flex()
                .flex_col()
                .gap(px(2.))
                .font_family(mono)
                .text_size(px(11.))
                .text_color(chrome.text)
                .children(statements.into_iter().map(SharedString::from)),
            // The generator's own sentence, which already names the product and
            // the reason it cannot do this — see `struct_edit::PlanError`.
            Err(error) => div()
                .text_size(px(11.))
                .text_color(chrome.danger)
                .child(SharedString::from(error.to_string())),
        };

        Some(
            section(
                ts!("struct.statements"),
                div()
                    .w_full()
                    .p(px(8.))
                    .rounded_md()
                    .bg(chrome.surface)
                    .border_1()
                    .border_color(chrome.border)
                    .child(body),
                chrome,
            )
            .into_any_element(),
        )
    }
}

impl Focusable for StructPane {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for StructPane {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.watch_scroll(cx);
        let chrome = theme(cx);
        let header = self.render_header(&chrome, cx);
        let banner = self.render_banner(&chrome);
        let body = self.render_body(&chrome, cx);

        div()
            .id("struct-pane")
            .track_focus(&self.focus_handle)
            .flex()
            .flex_col()
            .size_full()
            .min_w_0()
            .min_h_0()
            .relative()
            .on_drag_move::<DraggedThumb>(cx.listener(
                |pane, event: &DragMoveEvent<DraggedThumb>, _window, cx| {
                    let Some(progress) = pane.scrollbar().dragged(event, cx) else {
                        return;
                    };
                    pane.body_scrollbar.hold();
                    scroll_to(&pane.body_scroll, ScrollbarAxis::Vertical, progress);
                    cx.notify();
                },
            ))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|pane, _: &MouseUpEvent, _window, cx| pane.release(cx)),
            )
            .on_mouse_up_out(
                MouseButton::Left,
                cx.listener(|pane, _: &MouseUpEvent, _window, cx| pane.release(cx)),
            )
            .child(header)
            .children(banner)
            .child(body)
            .children(self.notice.clone().map(|notice| {
                div()
                    .absolute()
                    .bottom(px(6.))
                    .left(px(10.))
                    .right(px(10.))
                    .px(px(8.))
                    .py(px(4.))
                    .rounded_md()
                    .bg(chrome.surface)
                    .border_1()
                    .border_color(chrome.border)
                    .text_size(px(11.))
                    .text_color(chrome.text_muted)
                    .child(notice)
            }))
    }
}

/// A titled block of the body.
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

/// One cell of one of the pane's tables.
///
/// Every column shares the width evenly rather than being measured, and the
/// widest cell truncates: [`crate::table_detail`]'s idiom, and for its reason —
/// a column list is tens of rows and never the millions the result grid has to
/// survive.
fn cell(text: SharedString, muted: bool, chrome: &Theme) -> Div {
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
}

/// The heading row of one of the pane's tables.
fn headings<const N: usize>(headers: [SharedString; N], chrome: &Theme) -> Div {
    div()
        .flex()
        .flex_row()
        .border_b_1()
        .border_color(chrome.border)
        .children(headers.into_iter().map(|text| cell(text, true, chrome)))
}

/// A constraint's kind, as SQL spells it.
///
/// Untranslated for the reason `NOT NULL` is: these are the words the table's
/// own DDL carries, and a localised `PRIMARY KEY` would stop matching the
/// statement the block below shows.
fn kind_name(kind: ConstraintKind) -> SharedString {
    SharedString::new_static(match kind {
        ConstraintKind::PrimaryKey => "PRIMARY KEY",
        ConstraintKind::ForeignKey => "FOREIGN KEY",
        ConstraintKind::Unique => "UNIQUE",
        ConstraintKind::Check => "CHECK",
    })
}

/// Whether this dialect can drop a constraint at all.
///
/// Asked of the generator rather than answered here: SQLite is the product that
/// cannot, and a second list of which products those are would be a list that
/// can disagree with the one that writes the statements. The probe costs a
/// handful of string operations and reaches no database.
fn drops_constraints(dialect: &Dialect) -> bool {
    let mut probe = TableAlter::new(["t"]);
    probe.drop_constraints.push(ConstraintDrop {
        kind: ConstraintKind::PrimaryKey,
        name: "c".to_string(),
    });
    plan_alter(&probe, dialect).is_ok()
}

/// Reads one table's structure: its columns, and the constraints that can be
/// dropped.
///
/// **Blocks**, and is called from `cx.background_spawn` with a
/// [`SessionHandle`]. Every request goes through the session's own worker
/// thread, so they queue behind whatever else that connection is doing rather
/// than racing it.
///
/// Four `DESCRIBE`s and no more, because four is what JDBC's
/// `DatabaseMetaData` answers: there is no call for check constraints at all,
/// so a [`Structure`] read this way never holds one (§7.10).
pub fn load_structure(session: &Session, target: &ObjectTarget) -> Result<Structure, String> {
    let scoped = |kind: &str| {
        let mut request = DescribeRequest::new(kind).with_table(&target.name);
        request.catalog = target.catalog.clone();
        request.schema = target.schema.clone();
        request
    };

    let columns = items(session, &{
        let mut request = DescribeRequest::new("columns");
        request.catalog = target.catalog.clone();
        request.schema = target.schema.clone();
        request.table = Some(target.name.clone());
        request
    })?;
    let primary = items(session, &scoped("primary_keys"))?;
    let imported = items(session, &scoped("imported_keys"))?;
    let indexes = items(session, &scoped("indexes"))?;

    Ok(Structure {
        table: builder_sql::table_parts(
            target.catalog.as_deref(),
            target.schema.as_deref(),
            &target.name,
        ),
        columns: columns_of(&columns),
        constraints: constraints_of(&primary, &imported, &indexes),
    })
}

/// The columns of one `DESCRIBE columns` answer, in ordinal order.
///
/// Sorted rather than taken as they came: a reload that reshuffled the rows
/// would move every index the staging buffer holds, and drivers are not
/// required to answer in any order at all. The sort is stable, so a driver that
/// reported no ordinal keeps the order it gave.
fn columns_of(items: &[Item]) -> Vec<LoadedColumn> {
    let mut columns: Vec<(i64, LoadedColumn)> = items
        .iter()
        .map(|column| {
            (
                number(column, "ordinal").unwrap_or(i64::MAX),
                LoadedColumn {
                    name: text(column, "name").unwrap_or_default().to_string(),
                    // The panel's own folding: `VARCHAR(255)`, `NUMERIC(12,2)`.
                    // That is the form the user retypes and the only form the
                    // generator passes on.
                    type_sql: type_of(column),
                    not_null: !flag(column, "is_nullable").unwrap_or(true),
                    // `text` answers `None` for an absent *and* for an empty
                    // value, which is exactly the distinction the generator
                    // needs: no default at all, rather than a default of the
                    // empty string.
                    default_sql: text(column, "default").map(str::to_owned),
                },
            )
        })
        .collect();
    columns.sort_by_key(|(ordinal, _)| *ordinal);
    columns.into_iter().map(|(_, column)| column).collect()
}

/// The constraints three `DESCRIBE` answers add up to.
///
/// In kind order and then in name order — the primary key, the foreign keys,
/// the unique indexes — so that a reload does not reshuffle the table and the
/// indices the staging buffer holds go on meaning what they meant.
///
/// Three rules worth stating:
///
/// * **An unnamed primary key is kept.** A driver that reports no `PK_NAME`
///   still leaves a droppable key on MySQL, whose `DROP PRIMARY KEY` names no
///   constraint because a table has at most one. An unnamed *foreign key* is
///   skipped instead: every product's spelling of that drop needs the name.
/// * **An index row with no column is a table statistic** — JDBC's
///   `tableIndexStatistic` — and not an index, exactly as the detail panel has
///   it.
/// * **The primary key's own index is not offered twice.** It arrives once from
///   `getPrimaryKeys` and again from `getIndexInfo`, and the second copy would
///   offer a `DROP CONSTRAINT` against a name the server does not know as a
///   constraint. Matched by name and, for the drivers that name the index and
///   the key differently, by covering exactly the key's columns.
fn constraints_of(primary: &[Item], imported: &[Item], indexes: &[Item]) -> Vec<LoadedConstraint> {
    let mut constraints = Vec::new();

    let key_columns = ordered(primary, "column", "seq");
    let key_name = primary
        .iter()
        .find_map(|row| text(row, "name"))
        .unwrap_or_default()
        .to_string();
    if !primary.is_empty() {
        constraints.push(LoadedConstraint {
            kind: ConstraintKind::PrimaryKey,
            name: key_name.clone(),
            columns: key_columns.clone(),
        });
    }

    let mut foreign: BTreeMap<&str, Vec<(i64, String)>> = BTreeMap::new();
    for row in imported {
        let (Some(name), Some(column)) = (text(row, "fk_name"), text(row, "fk_column")) else {
            continue;
        };
        foreign
            .entry(name)
            .or_default()
            .push((number(row, "seq").unwrap_or(0), column.to_string()));
    }
    for (name, mut members) in foreign {
        members.sort_by_key(|(seq, _)| *seq);
        constraints.push(LoadedConstraint {
            kind: ConstraintKind::ForeignKey,
            name: name.to_string(),
            columns: members.into_iter().map(|(_, column)| column).collect(),
        });
    }

    let mut unique: BTreeMap<&str, Vec<(i64, String)>> = BTreeMap::new();
    for row in indexes {
        let (Some(name), Some(column)) = (text(row, "name"), text(row, "column")) else {
            continue;
        };
        if flag(row, "non_unique").unwrap_or(true) || name == key_name {
            continue;
        }
        unique
            .entry(name)
            .or_default()
            .push((number(row, "ordinal").unwrap_or(0), column.to_string()));
    }
    for (name, mut members) in unique {
        members.sort_by_key(|(ordinal, _)| *ordinal);
        let columns: Vec<String> = members.into_iter().map(|(_, column)| column).collect();
        if !key_columns.is_empty() && columns == key_columns {
            continue;
        }
        constraints.push(LoadedConstraint {
            kind: ConstraintKind::Unique,
            name: name.to_string(),
            columns,
        });
    }

    constraints
}

/// One key's columns, in the order the sequence member gives them.
fn ordered(rows: &[Item], column: &str, sequence: &str) -> Vec<String> {
    let mut members: Vec<(i64, String)> = rows
        .iter()
        .filter_map(|row| {
            Some((
                number(row, sequence).unwrap_or(0),
                text(row, column)?.to_string(),
            ))
        })
        .collect();
    members.sort_by_key(|(seq, _)| *seq);
    members.into_iter().map(|(_, column)| column).collect()
}

#[cfg(test)]
mod tests {
    use gpui::{TestAppContext, WindowHandle};

    use super::*;
    use crate::app_settings;
    use crate::connection::Connected;
    use crate::explorer::Folder;

    /// Parses one `DESCRIBE` item out of JSON.
    fn item(json: &str) -> Item {
        serde_json::from_str(json).expect("the fixture parses")
    }

    /// Parses a list of them.
    fn list(rows: &[&str]) -> Vec<Item> {
        rows.iter().copied().map(item).collect()
    }

    /// A table of the fixture's `APP` schema.
    fn target(name: &str) -> ObjectTarget {
        ObjectTarget {
            connection: ConnectionId(1),
            catalog: None,
            schema: Some("APP".to_string()),
            folder: Folder::Tables,
            name: name.to_string(),
        }
    }

    /// A window whose whole content is one structure pane, already loaded.
    fn pane(
        connected: &Connected,
        target: ObjectTarget,
        cx: &mut TestAppContext,
    ) -> WindowHandle<StructPane> {
        cx.update(|cx| {
            app_settings::init(cx);
            rudbman_ui::init(cx);
        });
        let session = connected.handle();
        let profile = crate::connection::h2::profile("struct-pane");
        let window = cx.add_window(move |_window, cx| {
            StructPane::new(session, ConnectionId(1), target, &profile, "h2", cx)
        });
        window
            .update(cx, |pane, _window, cx| pane.refresh(cx))
            .expect("the window is open");
        cx.run_until_parked();
        window
    }

    /// The fixture table, read through the pane's own loader.
    #[test]
    fn a_table_is_read_as_its_columns_and_its_droppable_constraints() {
        let connected = crate::explorer::tests::h2_fixture("struct-load");
        let structure = load_structure(connected.session(), &target("PERSON"))
            .unwrap_or_else(|error| panic!("PERSON: {error}"));

        assert_eq!(structure.table, ["APP", "PERSON"]);
        let names: Vec<&str> = structure
            .columns
            .iter()
            .map(|column| column.name.as_str())
            .collect();
        assert_eq!(names, ["ID", "TEAM_ID", "EMAIL", "SALARY"], "ordinal order");

        let id = &structure.columns[0];
        assert_eq!(id.type_sql, "INTEGER", "{id:?}");
        assert!(id.not_null, "a primary key column refuses NULL");
        // No default, rather than a default of the empty string: the two are
        // different statements.
        assert_eq!(id.default_sql, None);
        // The size folded in, exactly as the detail panel spells it.
        assert_eq!(structure.columns[2].type_sql, "CHARACTER VARYING(200)");
        assert_eq!(structure.columns[3].type_sql, "NUMERIC(12,2)");
        assert!(!structure.columns[2].not_null);

        // One primary key, one foreign key, one unique index — and the index
        // behind the primary key is *not* a fourth.
        let kinds: Vec<ConstraintKind> = structure
            .constraints
            .iter()
            .map(|constraint| constraint.kind)
            .collect();
        assert_eq!(
            kinds,
            [
                ConstraintKind::PrimaryKey,
                ConstraintKind::ForeignKey,
                ConstraintKind::Unique
            ],
            "{:?}",
            structure.constraints
        );
        assert_eq!(structure.constraints[0].columns, ["ID"]);
        assert_eq!(structure.constraints[1].name, "FK_PERSON_TEAM");
        assert_eq!(structure.constraints[1].columns, ["TEAM_ID"]);
        assert_eq!(structure.constraints[2].name, "UX_PERSON_EMAIL");
        assert_eq!(structure.constraints[2].columns, ["EMAIL"]);
    }

    /// The primary key's own index is the same constraint under two names, and
    /// offering it twice would offer a drop that fails.
    #[test]
    fn the_primary_keys_index_is_not_offered_as_a_second_constraint() {
        let primary = list(&[
            r#"{"column":"ID","seq":1,"name":"PK_ORDERS"}"#,
            r#"{"column":"LINE","seq":2,"name":"PK_ORDERS"}"#,
        ]);
        let indexes = list(&[
            // The key's index, reported under the key's own name.
            r#"{"name":"PK_ORDERS","column":"ID","ordinal":1,"non_unique":false}"#,
            r#"{"name":"PK_ORDERS","column":"LINE","ordinal":2,"non_unique":false}"#,
            // The same index under a name of its own, which several drivers do.
            r#"{"name":"PRIMARY_KEY_4","column":"ID","ordinal":1,"non_unique":false}"#,
            r#"{"name":"PRIMARY_KEY_4","column":"LINE","ordinal":2,"non_unique":false}"#,
            // A statistic row: no column, and not an index at all.
            r#"{"name":"STATS","non_unique":true}"#,
            // A non-unique index is not a constraint and cannot be dropped as
            // one.
            r#"{"name":"IX_NOTE","column":"NOTE","ordinal":1,"non_unique":true}"#,
            // And a genuine unique index, which is.
            r#"{"name":"UX_CODE","column":"CODE","ordinal":1,"non_unique":false}"#,
        ]);

        let constraints = constraints_of(&primary, &[], &indexes);
        assert_eq!(constraints.len(), 2, "{constraints:?}");
        assert_eq!(constraints[0].kind, ConstraintKind::PrimaryKey);
        assert_eq!(constraints[0].columns, ["ID", "LINE"], "in KEY_SEQ order");
        assert_eq!(constraints[1].name, "UX_CODE");
        assert_eq!(constraints[1].kind, ConstraintKind::Unique);
    }

    /// A primary key a driver would not name is still a primary key: MySQL's
    /// `DROP PRIMARY KEY` writes no name. A foreign key without one is not,
    /// because every spelling of that drop needs it.
    #[test]
    fn an_unnamed_primary_key_is_kept_and_an_unnamed_foreign_key_is_not() {
        let primary = list(&[r#"{"column":"ID","seq":1}"#]);
        let imported = list(&[
            r#"{"fk_column":"TEAM_ID","seq":1}"#,
            r#"{"fk_name":"FK_B","fk_column":"B2","seq":2}"#,
            r#"{"fk_name":"FK_B","fk_column":"B1","seq":1}"#,
            r#"{"fk_name":"FK_A","fk_column":"A1","seq":1}"#,
        ]);

        let constraints = constraints_of(&primary, &imported, &[]);
        assert_eq!(constraints.len(), 3, "{constraints:?}");
        assert_eq!(constraints[0].kind, ConstraintKind::PrimaryKey);
        assert_eq!(constraints[0].name, "", "unnamed, and kept");
        // Name order among the foreign keys, and key-sequence order inside one.
        assert_eq!(constraints[1].name, "FK_A");
        assert_eq!(constraints[2].name, "FK_B");
        assert_eq!(constraints[2].columns, ["B1", "B2"]);
    }

    /// An absent or empty default is "no default", which is a different
    /// statement from a default of the empty string.
    #[test]
    fn an_empty_default_is_no_default_at_all() {
        let columns = columns_of(&list(&[
            r#"{"name":"A","type_name":"INTEGER","data_type":4,"ordinal":1,"is_nullable":true}"#,
            r#"{"name":"B","type_name":"INTEGER","data_type":4,"ordinal":2,"default":""}"#,
            r#"{"name":"C","type_name":"INTEGER","data_type":4,"ordinal":3,"default":"0"}"#,
        ]));

        assert_eq!(columns[0].default_sql, None, "absent");
        assert_eq!(columns[1].default_sql, None, "empty is not Some(\"\")");
        assert_eq!(columns[2].default_sql, Some("0".to_string()));
        // A driver that would not say is taken as nullable, which is JDBC's own
        // default and the safer of the two.
        assert!(!columns[0].not_null);
    }

    /// The columns come out in ordinal order however the driver listed them.
    #[test]
    fn columns_are_ordered_so_a_reload_does_not_reshuffle_them() {
        let columns = columns_of(&list(&[
            r#"{"name":"C","type_name":"INTEGER","data_type":4,"ordinal":3}"#,
            r#"{"name":"A","type_name":"INTEGER","data_type":4,"ordinal":1}"#,
            r#"{"name":"B","type_name":"INTEGER","data_type":4,"ordinal":2}"#,
        ]));
        let names: Vec<&str> = columns.iter().map(|column| column.name.as_str()).collect();
        assert_eq!(names, ["A", "B", "C"]);
    }

    /// SQLite has no `ALTER` that drops a constraint, and the pane asks the
    /// generator rather than keeping a list of its own.
    #[test]
    fn only_the_products_that_can_drop_a_constraint_offer_the_toggle() {
        assert!(drops_constraints(&Dialect::POSTGRES));
        assert!(drops_constraints(&Dialect::MYSQL));
        assert!(drops_constraints(&Dialect::ORACLE));
        assert!(!drops_constraints(&Dialect::SQLITE));
    }

    /// The block under the tables is the staged state turned into statements,
    /// recomputed and never cached.
    #[gpui::test]
    fn the_statements_follow_what_is_staged(cx: &mut TestAppContext) {
        let connected = crate::explorer::tests::h2_fixture("struct-statements");
        let window = pane(&connected, target("PERSON"), cx);

        window
            .update(cx, |pane, _window, cx| {
                assert!(pane.structure().is_some(), "the structure arrived");
                assert!(!pane.has_pending_edits());
                assert_eq!(pane.plan(), None, "nothing staged is nothing to show");

                // The type of the third column, retyped through the form: the
                // selection fills the fields, and the field stages.
                pane.select(Some(Selection::Column(2)), cx);
                assert_eq!(pane.filled[0], "EMAIL", "the form filled from the row");
                assert!(
                    !pane.has_pending_edits(),
                    "selecting a row is not an edit of it"
                );

                pane.typed(ColumnField::Type, "VARCHAR(300)".to_string(), cx);
                assert!(pane.edits.is_column_changed(2));
                assert_eq!(
                    pane.plan(),
                    Some(Ok(vec![
                        "ALTER TABLE APP.PERSON ALTER COLUMN EMAIL SET DATA TYPE VARCHAR(300)"
                            .to_string()
                    ]))
                );

                // A drop of the same column replaces the change rather than
                // joining it.
                pane.toggle_drop(2, cx);
                assert_eq!(
                    pane.plan(),
                    Some(Ok(vec![
                        "ALTER TABLE APP.PERSON DROP COLUMN EMAIL".to_string()
                    ]))
                );
                assert_eq!(pane.selected, None, "a dropped column is not editable");

                // And a discard puts it all back.
                pane.discard(cx);
                assert!(!pane.has_pending_edits());
                assert_eq!(pane.plan(), None);
            })
            .expect("the window is open");
        connected.close().expect("close");
    }

    /// An added column is typed from nothing, and its refusal names the row.
    #[gpui::test]
    fn an_added_column_is_staged_from_the_form(cx: &mut TestAppContext) {
        let connected = crate::explorer::tests::h2_fixture("struct-add");
        let window = pane(&connected, target("TEAM"), cx);

        window
            .update(cx, |pane, _window, cx| {
                pane.add_column(cx);
                assert_eq!(pane.selected, Some(Selection::Added(0)));
                // A column with no name cannot be added, and the refusal points
                // at the row rather than at a statement.
                assert_eq!(
                    pane.plan(),
                    Some(Err(PlanError::AddedHasNoName { position: 0 }))
                );

                pane.typed(ColumnField::Name, "MOTTO".to_string(), cx);
                pane.typed(ColumnField::Type, "VARCHAR(40)".to_string(), cx);
                assert_eq!(
                    pane.plan(),
                    Some(Ok(vec![
                        "ALTER TABLE APP.TEAM ADD COLUMN MOTTO VARCHAR(40)".to_string()
                    ]))
                );

                // The default pair: typing one sets it, and the box takes it
                // away again. `None` and `Some("")` stay two states.
                pane.set_has_default(true, cx);
                assert_eq!(
                    pane.edits.added()[0].default_sql,
                    Some(String::new()),
                    "a default of nothing is still a default"
                );
                pane.typed(ColumnField::Default, "'?'".to_string(), cx);
                assert_eq!(
                    pane.plan(),
                    Some(Ok(vec![
                        "ALTER TABLE APP.TEAM ADD COLUMN MOTTO VARCHAR(40) DEFAULT '?'".to_string()
                    ]))
                );
                pane.set_has_default(false, cx);
                assert_eq!(pane.edits.added()[0].default_sql, None);

                pane.remove_added(0, cx);
                assert!(!pane.has_pending_edits());
                assert_eq!(pane.selected, None);
            })
            .expect("the window is open");
        connected.close().expect("close");
    }

    /// The rename field is pre-filled from the catalog, so filling it is not a
    /// rename and typing the same name back is not one either.
    #[gpui::test]
    fn the_rename_field_only_counts_once_it_says_something_else(cx: &mut TestAppContext) {
        let connected = crate::explorer::tests::h2_fixture("struct-rename");
        let window = pane(&connected, target("TEAM"), cx);

        window
            .update(cx, |pane, _window, cx| {
                assert_eq!(pane.filled_rename, "TEAM");
                assert!(!pane.has_pending_edits(), "the pre-fill is not an edit");

                pane.renamed("SQUAD".to_string(), cx);
                assert_eq!(
                    pane.plan(),
                    Some(Ok(vec!["ALTER TABLE APP.TEAM RENAME TO SQUAD".to_string()]))
                );

                pane.renamed("TEAM".to_string(), cx);
                assert!(!pane.has_pending_edits(), "typed back is not a rename");
            })
            .expect("the window is open");
        connected.close().expect("close");
    }

    /// A read-only profile browses and stages nothing, and says so where the
    /// data pane says its own.
    #[gpui::test]
    fn a_read_only_profile_stages_nothing(cx: &mut TestAppContext) {
        let connected = crate::explorer::tests::h2_fixture("struct-read-only");
        cx.update(|cx| {
            app_settings::init(cx);
            rudbman_ui::init(cx);
        });
        let session = connected.handle();
        let mut profile = crate::connection::h2::profile("struct-read-only");
        profile.read_only = true;
        let window = cx.add_window(move |_window, cx| {
            StructPane::new(session, ConnectionId(1), target("TEAM"), &profile, "h2", cx)
        });
        window
            .update(cx, |pane, _window, cx| pane.refresh(cx))
            .expect("the window is open");
        cx.run_until_parked();

        window
            .update(cx, |pane, _window, cx| {
                assert_eq!(pane.read_only_reason(), Some(ts!("struct.read_only")));
                assert!(pane.structure().is_some(), "it still browses");

                pane.select(Some(Selection::Column(0)), cx);
                pane.typed(ColumnField::Type, "BIGINT".to_string(), cx);
                pane.toggle_drop(0, cx);
                pane.add_column(cx);
                pane.renamed("SQUAD".to_string(), cx);
                assert!(!pane.has_pending_edits(), "nothing may be staged");
            })
            .expect("the window is open");
        connected.close().expect("close");
    }

    /// A pane whose connection has gone keeps what it read and asks for nothing
    /// more.
    #[gpui::test]
    fn a_detached_pane_says_so_rather_than_asking(cx: &mut TestAppContext) {
        let connected = crate::explorer::tests::h2_fixture("struct-detach");
        let window = pane(&connected, target("TEAM"), cx);

        window
            .update(cx, |pane, _window, cx| {
                pane.detach(cx);
                pane.refresh(cx);
                assert_eq!(pane.notice, Some(ts!("explorer.disconnected")));
                assert!(pane.structure().is_some(), "what was read is still there");
            })
            .expect("the window is open");
        connected.close().expect("close");
    }

    /// A refresh refuses while anything is staged, for the reason a data pane's
    /// does: the indices the buffer holds are the snapshot's.
    #[gpui::test]
    fn a_reload_refuses_to_take_the_indices_out_from_under_the_staging(cx: &mut TestAppContext) {
        let connected = crate::explorer::tests::h2_fixture("struct-reload");
        let window = pane(&connected, target("TEAM"), cx);

        window
            .update(cx, |pane, _window, cx| {
                pane.select(Some(Selection::Column(1)), cx);
                pane.typed(ColumnField::Type, "VARCHAR(80)".to_string(), cx);
                assert!(pane.has_pending_edits());

                pane.refresh(cx);
                assert_eq!(pane.notice, Some(ts!("struct.discard_first")));
                assert!(pane.structure().is_some(), "nothing was thrown away");
                assert!(pane.has_pending_edits());

                pane.warn_pending(cx);
                assert_eq!(pane.notice, Some(ts!("struct.discard_first")));
            })
            .expect("the window is open");
        connected.close().expect("close");
    }

    /// Every state of the pane lays out.
    ///
    /// The one thing a headless test cannot answer on its own: a duplicated
    /// element id or an element the layout cannot place is a panic at paint
    /// time and nothing at all before it, and this pane draws two tables, a
    /// form and a block of statements out of one body.
    #[gpui::test]
    fn every_state_of_the_pane_draws(cx: &mut TestAppContext) {
        let connected = crate::explorer::tests::h2_fixture("struct-draw");
        let window = pane(&connected, target("PERSON"), cx);
        let mut vcx = gpui::VisualTestContext::from_window(window.into(), cx);
        vcx.run_until_parked();

        window
            .update(&mut vcx, |pane, _window, cx| {
                // A selected row, a dropped one, an added one, a dropped
                // constraint and a rename: every branch of the body at once.
                pane.select(Some(Selection::Column(1)), cx);
                pane.typed(ColumnField::Type, "BIGINT".to_string(), cx);
                pane.toggle_drop(2, cx);
                pane.toggle_constraint_drop(1, cx);
                pane.renamed("HUMAN".to_string(), cx);
                pane.add_column(cx);
                pane.typed(ColumnField::Name, "MEMO".to_string(), cx);
                pane.typed(ColumnField::Type, "VARCHAR(10)".to_string(), cx);
            })
            .expect("the window is open");
        vcx.run_until_parked();

        // The refusal branch of the statements block, which is a different
        // element from the listing.
        window
            .update(&mut vcx, |pane, _window, cx| {
                pane.select(Some(Selection::Column(0)), cx);
                pane.typed(ColumnField::Name, String::new(), cx);
                assert!(matches!(pane.plan(), Some(Err(_))));
            })
            .expect("the window is open");
        vcx.run_until_parked();

        // And the two bodies that stand in for a structure.
        window
            .update(&mut vcx, |pane, _window, cx| {
                pane.discard(cx);
                pane.load = Load::Failed("permission denied".into());
                cx.notify();
            })
            .expect("the window is open");
        vcx.run_until_parked();
        window
            .update(&mut vcx, |pane, _window, cx| {
                pane.load = Load::Running;
                cx.notify();
            })
            .expect("the window is open");
        vcx.run_until_parked();
        connected.close().expect("close");
    }

    #[test]
    fn every_label_the_pane_draws_has_a_translation() {
        for label in [
            ts!("struct.loading"),
            ts!("struct.refresh"),
            ts!("struct.discard"),
            ts!("struct.discard_first"),
            ts!("struct.read_only"),
            ts!("struct.pending", count = 2),
            ts!("struct.columns"),
            ts!("struct.column"),
            ts!("struct.type"),
            ts!("struct.nullable"),
            ts!("struct.default"),
            ts!("struct.state"),
            ts!("struct.state_changed"),
            ts!("struct.state_dropped"),
            ts!("struct.state_added"),
            ts!("struct.drop"),
            ts!("struct.keep"),
            ts!("struct.remove"),
            ts!("struct.add_column"),
            ts!("struct.edit_column"),
            ts!("struct.no_selection"),
            ts!("struct.name"),
            ts!("struct.has_default"),
            ts!("struct.constraints"),
            ts!("struct.kind"),
            ts!("struct.empty"),
            ts!("struct.no_constraint_drop", dialect = "SQLite"),
            ts!("struct.rename_table"),
            ts!("struct.new_name"),
            ts!("struct.statements"),
            ts!("menu.view_structure"),
        ] {
            assert!(!label.is_empty(), "empty label");
            assert!(!label.starts_with("struct."), "untranslated {label:?}");
            assert!(!label.starts_with("menu."), "untranslated {label:?}");
        }
        // The one setting the language changes nothing about: SQL keywords are
        // what the table's own DDL says.
        assert_eq!(kind_name(ConstraintKind::PrimaryKey), "PRIMARY KEY");
        assert_eq!(kind_name(ConstraintKind::ForeignKey), "FOREIGN KEY");
        assert_eq!(kind_name(ConstraintKind::Unique), "UNIQUE");
        assert_eq!(kind_name(ConstraintKind::Check), "CHECK");
    }

    /// Nothing is asked for from the constructor: the host has a tab to open
    /// first, and a fetch started inside `cx.new` would be racing it.
    #[gpui::test]
    fn opening_a_pane_leaves_it_loading_until_the_host_asks(cx: &mut TestAppContext) {
        let connected = crate::explorer::tests::h2_fixture("struct-open");
        cx.update(|cx| {
            app_settings::init(cx);
            rudbman_ui::init(cx);
        });
        let session = connected.handle();
        let profile = crate::connection::h2::profile("struct-open");
        let window = cx.add_window(move |_window, cx| {
            StructPane::new(
                session,
                ConnectionId(1),
                target("PERSON"),
                &profile,
                "h2",
                cx,
            )
        });
        cx.run_until_parked();

        window
            .update(cx, |pane, _window, _cx| {
                assert!(pane.is_loading());
                assert!(pane.structure().is_none());
                assert_eq!(pane.target().name, "PERSON");
                assert_eq!(pane.connection(), ConnectionId(1));
            })
            .expect("the window is open");
        connected.close().expect("close");
    }
}
