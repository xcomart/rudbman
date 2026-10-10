#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

//! rudbman application entry point. Window behavior lives in [`workspace`].

use gpui::actions;
use rugpui_shell::SHORTCUT_MODIFIER;
mod app_identity;
mod app_settings;
mod backup_dialog;
mod builder_pane;
mod builder_sql;
mod connection;
mod connection_dialog;
mod context_menu;
mod data_edit;
mod data_pane;
mod driver_manager;
mod erd_layout;
mod erd_pane;
mod explorer;
mod extract_dialog;
mod i18n;
mod icons;
mod maven;
// The pane tree is written as a self-contained data structure with its own
// tests rather than for the call sites the shell currently has, so it offers
// operations nothing reaches yet — merging a subtree, editing a payload — which
// inside a binary crate read as dead code.
#[allow(dead_code)]
mod pane_tree;
mod query;
mod query_source;
mod row_apply;
mod settings_dialog;
mod sql_highlight;
mod struct_edit;
mod struct_pane;
mod table_detail;
mod transfer_dialog;

// Compiles `locales/*.yml` into the binary and defines the machinery `t!`
// expands to, which is why it has to sit in the crate root. `fallback = "en"`
// is per key, not per locale: a string a translator has not got to yet shows
// in English while the rest of that language stays translated.
rust_i18n::i18n!("locales", fallback = "en");

actions!(
    rudbman,
    [
        /// Quit the application.
        Quit,
        /// Open the connection dialog with an empty form.
        NewConnection,
        /// Open the settings dialog.
        OpenSettings,
        /// Open the about dialog.
        ShowAbout,
        /// Ask GitHub whether a newer release exists, showing the answer either
        /// way. Unlike the start-up check, this one is not silent and does not
        /// respect the ignored-version tag.
        CheckUpdates,
        /// Close the active pane, unless it is the last one.
        ClosePane,
        /// Move keyboard focus to the next pane.
        FocusNextPane,
        /// Move keyboard focus to the previous pane.
        FocusPrevPane,
        /// Split the active pane, putting an empty pane to its right.
        SplitRight,
        /// Split the active pane, putting an empty pane below it.
        SplitBelow,
        /// Show or hide the explorer sidebar.
        ToggleExplorer,
        /// Open an empty query pane on the connection whose tab is showing.
        NewQuery,
        /// Open a query pane over the object selected in the explorer.
        QueryObject,
        /// Read a `.sql` file into a query pane on the connection whose tab is
        /// showing.
        OpenSqlFile,
        /// Open the extraction dialog over the object selected in the explorer.
        ExtractScript,
        /// Open the transfer dialog over the object selected in the explorer.
        TransferTable,
        /// Open the backup dialog over the scope the explorer's selection sits
        /// in.
        BackupSchema,
        /// Draw the ERD of the scope the explorer's selection sits in.
        OpenErd,
        /// Put the object selected in the explorer onto a query builder.
        AddToBuilder,
        /// Open an empty query builder on the connection whose tab is showing.
        NewBuilder,
        /// Close the open dialog or dropdown menu, if there is one.
        DismissDialog,
    ]
);

mod workspace;

use workspace::{editor_theme_for, theme_dirs};

fn main() {
    workspace::run();
}
