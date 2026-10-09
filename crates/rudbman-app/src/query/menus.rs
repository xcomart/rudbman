//! Menus.

use super::*;

impl QueryPane {
    /// Whether a menu is open in either half of the pane, for the shell's own
    /// tests.
    ///
    /// The workspace reaches every pane on `Escape` — a right click moves no
    /// pane marker, so the pane holding a menu is not necessarily the active
    /// one — and asserting that it did needs a way through.
    #[cfg(test)]
    pub(crate) fn has_context_menu(&self) -> bool {
        self.context_menu.is_some()
    }

    /// Opens the editor's menu, as a right click in it would.
    ///
    /// Test-only: the widget's own gesture is covered in `rugpui-editor`, and
    /// what the shell's tests need is a pane with a menu open on it.
    #[cfg(test)]
    pub(crate) fn open_editor_menu(&mut self, position: Point<Pixels>, cx: &mut Context<Self>) {
        self.context_menu = Some(PaneMenu::Editor { position });
        cx.notify();
    }

    /// Puts the pane's right-click menu away, and says whether there was one.
    ///
    /// What `Escape` reaches through the workspace, which closes the menu on
    /// top of everything before it closes anything else (architecture document,
    /// §7.8). The answer is what tells the workspace the key was spent here.
    pub fn close_context_menu(&mut self, cx: &mut Context<Self>) -> bool {
        let had = self.context_menu.take().is_some();
        if had {
            cx.notify();
        }
        had
    }

    /// The editor's right-click menu: everything a SQL buffer can be asked to
    /// do, in the order the keyboard already offers it.
    ///
    /// Every row is one of the editor's own actions, dispatched on the editor's
    /// focus handle — not called, because the editor exposes no method for any
    /// of them and should not have to: the menu and the chord are the same
    /// command reaching the same handler.
    ///
    /// What is greyed and why: the three clipboard rows follow the selection
    /// and whether the buffer may be written to, `Undo` and `Redo` follow the
    /// history, and the three run rows follow the session — a pane whose
    /// connection tab has been closed keeps its text and its rows and can run
    /// nothing (see [`QueryPane::detach`]).
    pub(super) fn editor_rows(&self, cx: &App) -> Vec<MenuRow> {
        let editor = self.editor.read(cx);
        let handle = editor.focus_handle(cx);
        let selected = editor.has_selection();
        let writable = !editor.is_read_only();
        let attached = self.session.is_some();
        let row =
            |label: SharedString, shortcut: String, enabled: bool, action: Box<dyn Action>| {
                let handle = handle.clone();
                MenuRow::new(label)
                    .shortcut(shortcut)
                    .enabled(enabled)
                    .on_activate(move |window, cx| handle.dispatch_action(&*action, window, cx))
            };
        let modifier = SHORTCUT_MODIFIER;

        vec![
            row(
                ts!("context.cut"),
                format!("{modifier}+X"),
                selected && writable,
                Box::new(Cut),
            ),
            row(
                ts!("context.copy"),
                format!("{modifier}+C"),
                selected,
                Box::new(Copy),
            ),
            row(
                ts!("context.paste"),
                format!("{modifier}+V"),
                writable,
                Box::new(Paste),
            ),
            MenuRow::separator(),
            row(
                ts!("context.select_all"),
                format!("{modifier}+A"),
                true,
                Box::new(SelectAll),
            ),
            MenuRow::separator(),
            row(
                ts!("context.undo"),
                format!("{modifier}+Z"),
                editor.can_undo(),
                Box::new(Undo),
            ),
            row(
                ts!("context.redo"),
                format!("{modifier}+Shift+Z"),
                editor.can_redo(),
                Box::new(Redo),
            ),
            MenuRow::separator(),
            row(
                ts!("context.toggle_comment"),
                format!("{modifier}+/"),
                writable,
                Box::new(ToggleComment),
            ),
            MenuRow::separator(),
            row(
                ts!("context.run_statement"),
                format!("{modifier}+Enter"),
                attached,
                Box::new(RunStatement),
            ),
            row(
                ts!("context.run_selection"),
                format!("{modifier}+Alt+Enter"),
                attached && selected,
                Box::new(RunSelection),
            ),
            row(
                ts!("context.run_all"),
                format!("{modifier}+Shift+Enter"),
                attached,
                Box::new(RunAll),
            ),
            MenuRow::separator(),
            row(
                ts!("context.find"),
                format!("{modifier}+F"),
                true,
                Box::new(Find),
            ),
            row(
                ts!("context.replace"),
                format!("{modifier}+H"),
                writable,
                Box::new(Replace),
            ),
        ]
    }

    /// A result grid's right-click menu: the cell menu, or the heading one.
    ///
    /// Both lists start as [`crate::context_menu`]'s, because both are the same
    /// lists the data pane's grid draws (architecture document, §7.8) — the
    /// cell menu is about the *selection* rather than about the cell that was
    /// pressed, and the heading menu is about one column. What is this pane's
    /// own is where a sort goes: [`QueryPane::reorder`] wraps whatever the user
    /// wrote in a derived table, which no other pane has to do.
    ///
    /// The cell menu then carries the editing commands, which are the data
    /// pane's **minus its insert row** (§7.9: a result carries the columns the
    /// user selected, not the columns the table requires). They are drawn even
    /// where they cannot be run, greyed: a menu that changed shape between an
    /// editable result and a read-only one would make the user work out which
    /// of the two they were looking at, and the line above the grid has already
    /// said.
    pub(super) fn grid_rows(
        &self,
        id: u64,
        target: MenuTarget,
        cx: &mut Context<Self>,
    ) -> Vec<MenuRow> {
        let Some(grid) = self.grid_of(id) else {
            return Vec::new();
        };
        let this = cx.entity();
        let grid = grid.clone();

        let MenuTarget::Cell = target else {
            let MenuTarget::Header { column } = target else {
                unreachable!("the grid has two menu targets");
            };
            return context_menu::grid_header_rows(
                &grid,
                column,
                cx,
                move |direction, window, cx| {
                    this.update(cx, |pane, cx| {
                        pane.reorder(id, column, direction, window, cx)
                    });
                },
            );
        };

        let mut rows = context_menu::grid_copy_rows(&grid, cx);
        let cell = self.menu_cell(id, cx);
        let source = grid.read(cx).source();
        let writable = source.writable();
        let deleted = cell.is_some_and(|(row, _)| source.row_status(row) == RowStatus::Deleted);
        // A cell may take a NULL when it may take anything at all *and* the
        // catalogue lets the column hold one. Both halves are the source's:
        // this side only asks.
        let nullable = cell.is_some_and(|(row, column)| {
            source.cell_editable(row, column) && source.nullable(column)
        });
        let staged = cell.is_some_and(|(row, _)| source.row_status(row) != RowStatus::Unchanged);
        let anything = !source.edits().is_empty();

        rows.push(MenuRow::separator());
        rows.push({
            let this = this.clone();
            MenuRow::new(ts!("data.set_null"))
                .enabled(writable && nullable)
                .on_activate(move |_window, cx| {
                    let Some((row, column)) = cell else {
                        return;
                    };
                    this.update(cx, |pane, cx| pane.set_null(id, row, column, cx));
                })
        });
        rows.push({
            let this = this.clone();
            // One row, two words: the label says what the command will do, so
            // it flips on a row that is already struck out.
            let label = if deleted {
                ts!("data.undelete_row")
            } else {
                ts!("data.delete_row")
            };
            MenuRow::new(label)
                .enabled(writable && cell.is_some())
                .on_activate(move |_window, cx| {
                    let Some((row, _)) = cell else {
                        return;
                    };
                    this.update(cx, |pane, cx| pane.toggle_delete(id, row, cx));
                })
        });
        rows.push(MenuRow::separator());
        rows.push({
            let this = this.clone();
            MenuRow::new(ts!("data.discard_row"))
                .enabled(staged)
                .on_activate(move |_window, cx| {
                    let Some((row, _)) = cell else {
                        return;
                    };
                    this.update(cx, |pane, cx| pane.discard_row(id, row, cx));
                })
        });
        rows.push(
            // The same command the toolbar's Discard is, confirmation and all:
            // it throws away work in both directions at once, so it is asked
            // about wherever it is offered from.
            MenuRow::new(ts!("data.discard"))
                .enabled(anything)
                .on_activate(move |_window, cx| {
                    this.update(cx, |pane, cx| {
                        pane.confirm_discard = true;
                        pane.preview = None;
                        cx.notify();
                    });
                }),
        );
        rows
    }
}
