//! Bootstrap.

use super::*;

/// Where the two theme catalogues live, for `rugpui`'s theme store.
///
/// The widget kit is shared with applications that put their configuration
/// somewhere else, so it takes the directories rather than guessing at them:
/// [`rudbman_core`] resolves them, and this is the one place that turns the two
/// answers into the pair `rugpui` asks for.
///
/// # Errors
///
/// Fails when the platform's configuration directory cannot be resolved at
/// all, which is the same condition that used to fail a save or a delete.
pub(crate) fn theme_dirs() -> anyhow::Result<rugpui::ThemeDirs> {
    Ok(rugpui::ThemeDirs {
        ui_themes: rudbman_core::ui_themes_dir()?,
        editor_themes: Some(rudbman_core::editor_themes_dir()?),
    })
}

/// Reads both theme directories and installs what they hold.
///
/// Never fails, which is what every caller wants: a configuration directory
/// that cannot be resolved is a warning in the log and no themes of the user's
/// own, exactly as an unreadable directory has always been.
pub(super) fn reload_themes(cx: &mut App) {
    match theme_dirs() {
        Ok(dirs) => theme_store::reload(&dirs, cx),
        Err(err) => log::warn!("cannot locate the theme directories: {err:#}"),
    }
}

/// Installs both palettes the settings name.
///
/// The chrome theme comes straight from the configured id; the editor theme
/// goes through [`editor_theme_for`], which is where "follow the UI theme" is
/// decided. That decision lives here rather than in the settings dialog because
/// it has to hold whatever changed the inputs — a theme file appearing in the
/// user's directory moves the answer without anybody having opened a dialog.
///
/// An id nothing answers to — a theme file the user has since deleted — falls
/// back to the default rather than failing; see [`ThemeRegistry::resolve`].
pub(super) fn apply_themes(settings: &AppSettings, cx: &mut App) {
    let ui = ThemeRegistry::resolve(&settings.theme, cx);
    let editor_id = editor_theme_for(
        &settings.editor_theme,
        settings.editor_theme_follows_ui,
        &settings.theme,
        ui.dark,
        &EditorThemeRegistry::all(cx),
    );
    let editor = EditorThemeRegistry::resolve(&editor_id, cx);
    set_theme(ui, cx);
    set_editor_theme(editor, cx);
}

/// The editor theme to install, given the configured one and the chrome theme.
///
/// With the "follows the UI" switch off the configured id is used as it stands.
/// With it on the answer is the first of these that exists:
///
/// 1. the editor theme sharing the chrome theme's id, when its cast matches the
///    chrome — which is how the pairs that ship under one name stay together,
///    and, since every built-in chrome theme has an editor theme of the same id,
///    is the answer for every built-in;
/// 2. the configured theme, when its cast matches the chrome — the fallback for
///    a chrome theme of the user's own, which no editor theme is named after;
/// 3. any editor theme of the right cast;
/// 4. the configured id after all, when nothing of the right cast exists.
///
/// The namesake comes first because that is what the switch promises: its label
/// says the editor theme is *matched to* the interface theme, not merely kept on
/// the same side of light and dark, and while it is on the settings dialog
/// disables the editor theme dropdown outright — so there is no pick of the
/// user's here to preserve, and letting the configured id win would freeze the
/// editor on one palette however far the chrome moved.
///
/// The cast is still checked in rule 1, though, and deliberately: a user who has
/// written a *dark* editor theme under the id of a *light* chrome theme must not
/// have it dragged into a light window. Preventing that pairing is the whole
/// reason the switch exists, so it outranks the name match.
///
/// Pure and taking the theme list as an argument so that the rule can be tested
/// without an [`App`]; the caller supplies [`EditorThemeRegistry::all`].
pub(crate) fn editor_theme_for(
    configured: &str,
    follows_ui: bool,
    ui_theme_id: &str,
    ui_dark: bool,
    entries: &[EditorThemeEntry],
) -> String {
    if !follows_ui {
        return configured.to_string();
    }

    let matching = |id: &str| {
        entries
            .iter()
            .find(|entry| entry.id.eq_ignore_ascii_case(id) && entry.dark == ui_dark)
    };
    if let Some(entry) = matching(ui_theme_id).or_else(|| matching(configured)) {
        return entry.id.clone();
    }
    entries
        .iter()
        .find(|entry| entry.dark == ui_dark)
        .map(|entry| entry.id.clone())
        .unwrap_or_else(|| configured.to_string())
}

/// Records the window's placement in the settings global.
///
/// Nothing is written to disk here; the file is saved once, when the last
/// window closes. See [`app_settings::record_window_geometry`].
pub(super) fn record_window_geometry(window: &Window, cx: &mut App) {
    app_settings::record_window_geometry(window_geometry(window), cx);
}

/// The placement to open the window at.
pub(super) fn opening_bounds(state: &WindowState, cx: &mut App) -> WindowBounds {
    window_bounds(
        app_settings::saved_geometry(state),
        state.width,
        state.height,
        state.maximized,
        cx,
    )
}

/// The title bar style as the shell's chrome spells it.
///
/// Two identical enums, deliberately: `rudbman-core` reads and writes
/// `settings.json` and is free of gpui, which the shell's chrome is not, so
/// neither can be the other's. Both serialise to the same two `snake_case`
/// words — the test below is what says so — and this is the one line between
/// them.
pub(super) fn chrome_titlebar(style: TitlebarStyle) -> rugpui_shell::TitlebarStyle {
    match style {
        TitlebarStyle::Custom => rugpui_shell::TitlebarStyle::Custom,
        TitlebarStyle::System => rugpui_shell::TitlebarStyle::System,
    }
}

/// The application menu bar, in macOS layout.
///
/// gpui only turns this into a real menu bar on macOS — the Windows and Linux
/// backends store it and draw nothing — so the other platforms get the same
/// commands from the in-app dropdown built by [`Workspace::render_app_menu`].
/// Every item dispatches an action that is also bound to a shortcut in
/// [`bind_shortcuts`], which is what lets the macOS backend label the items with
/// their key equivalents; register the bindings first so the keymap it reads is
/// already populated.
///
/// About, Check for updates, Settings and Quit live in the application menu
/// because that is where macOS users look for them.
///
/// The item labels are translated, but the application menu's own name is the
/// "rudbman" wordmark and stays as it is. Rebuilt and re-installed whenever the
/// language changes, because gpui takes the menu bar by value.
pub(super) fn app_menus() -> Vec<Menu> {
    vec![
        Menu {
            name: APP_NAME.into(),
            items: vec![
                MenuItem::action(ts!("menu.about"), ShowAbout),
                MenuItem::action(ts!("menu.check_updates"), CheckUpdates),
                MenuItem::separator(),
                MenuItem::action(ts!("menu.settings"), OpenSettings),
                MenuItem::separator(),
                MenuItem::action(ts!("menu.mac.quit"), Quit),
            ],
            disabled: false,
        },
        Menu {
            name: ts!("menu.connection"),
            items: vec![
                MenuItem::action(ts!("menu.mac.new_connection"), NewConnection),
                MenuItem::separator(),
                MenuItem::action(ts!("menu.new_query"), NewQuery),
                MenuItem::action(ts!("menu.query_object"), QueryObject),
                MenuItem::action(ts!("menu.open_sql_file"), OpenSqlFile),
                MenuItem::action(ts!("menu.extract_script"), ExtractScript),
                MenuItem::action(ts!("menu.transfer_table"), TransferTable),
                MenuItem::action(ts!("menu.backup_schema"), BackupSchema),
                MenuItem::action(ts!("menu.erd"), OpenErd),
                MenuItem::action(ts!("menu.new_builder"), NewBuilder),
                MenuItem::action(ts!("menu.add_to_builder"), AddToBuilder),
            ],
            disabled: false,
        },
        Menu {
            name: ts!("menu.view"),
            items: vec![MenuItem::action(
                ts!("menu.toggle_explorer"),
                ToggleExplorer,
            )],
            disabled: false,
        },
    ]
}

/// Registers every shortcut the workspace listens for.
///
/// A binding here beats the focused view: gpui matches key bindings along the
/// whole dispatch path before it delivers the key event itself, so every chord
/// bound in this function is taken away from the SQL editor that will one day
/// be inside a pane. That is what decides [`PANE_SHORTCUT_MODIFIER`].
pub(super) fn bind_shortcuts(cx: &mut App) {
    let modifier = if cfg!(target_os = "macos") {
        "cmd"
    } else {
        "ctrl"
    };
    let pane = PANE_SHORTCUT_MODIFIER;

    cx.bind_keys(vec![
        KeyBinding::new(&format!("{modifier}-q"), Quit, None),
        KeyBinding::new(&format!("{modifier}-n"), NewConnection, Some(KEY_CONTEXT)),
        KeyBinding::new(&format!("{modifier}-,"), OpenSettings, Some(KEY_CONTEXT)),
        // `Ctrl+B` is what every editor with a sidebar binds it to, and unlike
        // the pane chords it has no contender inside a SQL editor.
        KeyBinding::new(&format!("{modifier}-b"), ToggleExplorer, Some(KEY_CONTEXT)),
        // `Ctrl+T` is free: the SQL editor binds no `T` chord, and the shell has
        // no tab-cycling gesture to clash with.
        KeyBinding::new(&format!("{modifier}-t"), NewQuery, Some(KEY_CONTEXT)),
        // `Ctrl+O` is "open a file" everywhere, and the SQL editor binds no `O`
        // chord of its own for it to be taken away from.
        KeyBinding::new(&format!("{modifier}-o"), OpenSqlFile, Some(KEY_CONTEXT)),
        // Scoped to the sidebar rather than the window, because the same chord
        // is the editor's "run the statement" — see `explorer::KEY_CONTEXT`.
        KeyBinding::new(
            &format!("{modifier}-enter"),
            QueryObject,
            Some(explorer::KEY_CONTEXT),
        ),
        // Scoped to the sidebar for the same reason: it acts on what is
        // selected there, and `Ctrl+E` is a line command in several editors.
        KeyBinding::new(
            &format!("{modifier}-e"),
            OpenErd,
            Some(explorer::KEY_CONTEXT),
        ),
        KeyBinding::new("escape", DismissDialog, Some(KEY_CONTEXT)),
        KeyBinding::new(&format!("{pane}-w"), ClosePane, Some(KEY_CONTEXT)),
        KeyBinding::new(&format!("{pane}-]"), FocusNextPane, Some(KEY_CONTEXT)),
        KeyBinding::new(&format!("{pane}-["), FocusPrevPane, Some(KEY_CONTEXT)),
        // Shifted because off macOS the pane modifier is `alt`, and a bare
        // `Alt+D`/`Alt+S` is a menu mnemonic on Windows. The bracket keys above
        // stay unshifted on purpose: macOS and Windows both report a shifted
        // bracket as `}` with the shift flag already consumed, so a `shift-]`
        // binding would never match.
        KeyBinding::new(&format!("{pane}-shift-d"), SplitRight, Some(KEY_CONTEXT)),
        KeyBinding::new(&format!("{pane}-shift-s"), SplitBelow, Some(KEY_CONTEXT)),
    ]);
}

pub(super) fn run() {
    env_logger::init();

    // The half of the identity that needs no `App`, installed first and ahead
    // of everything below: `apply_pending` and `clean_leftovers` both read it,
    // and both have to run before `gpui_platform::application()` does, because
    // that call is what loads a JVM into the process — the very thing an
    // applied update must not race. See `rugpui_shell::init_process_identity`.
    rugpui_shell::init_process_identity(app_identity::IDENTITY);

    // An update the previous run could only stage — because a JVM was loaded
    // into it and Windows will not let its files be renamed — is applied here,
    // synchronously, before a window, a settings load or a connection exists,
    // and therefore before anything can load a JVM into *this* process. It
    // answers `true` only when it has already spawned a fresh process on the
    // new build, at which point the one useful thing left to do is return
    // without ever building an `App`. See `update::apply_pending`.
    if update::apply_pending() {
        return;
    }

    // A self-update renames the copies it replaces aside instead of deleting
    // them — Windows cannot delete a running image, and one code path for
    // three platforms is worth more than an immediate unlink on the two that
    // could. This is the other half: the leftovers are swept up on the next
    // launch. Off the main thread, on a plain thread rather than
    // `cx.background_executor()` — there is no `App` yet to hand it to —
    // because removing a bundled JRE or a `.app` bundle is a recursive delete
    // of thousands of files and nothing this early depends on it.
    std::thread::spawn(update::clean_leftovers);

    // The icon set has to be installed before the app runs: `svg()` resolves
    // every path through this source, and the default one answers `None`.
    // `LastWindowClosed` rather than the default, which is this only away from
    // macOS: there an app whose last window closes stays in the Dock, and
    // rudbman has nothing to offer once its window is gone — no menu bar
    // command that opens a new one, no connection or query worth keeping alive
    // in the background. One rule on every platform is what the app has always
    // done.
    let app = gpui_platform::application()
        .with_assets(icons::ICONS)
        .with_quit_mode(QuitMode::LastWindowClosed);
    app.run(|cx: &mut App| {
        // The other half of the identity — the gpui global `identity`, `text`
        // and the rest of the shell's UI-facing paths read — plus the words
        // and the update policy. Safe alongside the `init_process_identity`
        // call above: `init` calls it again itself, and installing the same
        // identity twice leaves nothing different behind.
        app_identity::install(cx);

        if let Err(error) = rudbman_core::init_secrets() {
            log::warn!("the OS keychain is unavailable: {error}");
        }

        // Load the settings before the widget layer installs its default
        // palettes, then override those to match what the user configured.
        app_settings::init(cx);
        let settings = app_settings::current(cx);
        // Ahead of everything that renders a string — the menu bar included —
        // so nothing is ever built in the wrong language and then corrected.
        i18n::apply(settings.language.as_deref());

        rugpui::init(cx);
        // After the widget layer, because both scope their bindings to key
        // contexts the shell's own bindings have to be able to outrank.
        rugpui_editor::init(cx);
        rugpui_grid::init(cx);
        rudbman_erd::init(cx);
        bind_shortcuts(cx);
        cx.set_menus(app_menus());

        // Before the palettes are applied: the ids in the settings may well
        // name themes of the user's own.
        reload_themes(cx);
        apply_themes(&settings, cx);
        // The same value `window_appearance` below reads, handed to the widget
        // layer so the result grid and the ERD canvases know whether to paint a
        // background of their own; see [`app_settings::window_tint`].
        set_window_tint(settings.window.background_opacity, cx);

        cx.on_action(|_: &Quit, cx: &mut App| cx.quit());
        // The window's geometry is only in memory until here; this is the one
        // write of `settings.json` the shell performs. Nothing in the closure
        // re-enters gpui — the file write is the whole of it — which is what
        // keeps it clear of the X11 backend's re-entrancy trap, the one the
        // vendored `client.rs` patch exists for. Quitting is no longer this
        // closure's business: gpui does it, from the quit mode set on the
        // application below, and it runs these observers first — so the
        // settings are on disk before the process starts winding down.
        cx.on_window_closed(|cx, _closed| {
            if cx.windows().is_empty() {
                app_settings::save(cx);
            }
        })
        .detach();

        let bounds = opening_bounds(&settings.window, cx);
        // Read once, here: `appears_transparent` is what strips the platform
        // caption, and both Windows and macOS decide that when the window is
        // created. Changing the setting later cannot reach an open window,
        // which is why the settings dialog has to say a restart is needed.
        let titlebar = settings.window.titlebar;
        cx.open_window(
            WindowOptions {
                window_bounds: Some(bounds),
                titlebar: Some(TitlebarOptions {
                    title: Some(APP_NAME.into()),
                    appears_transparent: titlebar == TitlebarStyle::Custom,
                    // Ignored unless the caption is transparent; it moves the
                    // traffic lights AppKit keeps drawing into the toolbar band
                    // the app puts in the caption's place.
                    traffic_light_position: (titlebar == TitlebarStyle::Custom)
                        .then_some(TRAFFIC_LIGHT_ORIGIN),
                }),
                // Only the Linux backends read this. `appears_transparent`
                // above means nothing to X11 and Wayland: the caption stays the
                // compositor's until the window asks for client-side
                // decorations outright. gpui falls back to server decorations
                // on its own when no compositor is present, and
                // [`draws_own_titlebar`] follows what the window actually got.
                window_decorations: (titlebar == TitlebarStyle::Custom)
                    .then_some(gpui::WindowDecorations::Client),
                app_id: Some(APP_ID.into()),
                // A translucent or blurred window needs the platform surface to
                // permit alpha; the body then tints its own background.
                window_background: window_appearance(
                    settings.window.background_blur,
                    settings.window.background_opacity,
                ),
                ..Default::default()
            },
            |window, cx| {
                let workspace = cx.new(|cx| Workspace::new(titlebar, window, cx));
                let handle = workspace.read(cx).focus_handle.clone();
                window.focus(&handle, cx);
                apply_caption_theme(window, &theme(cx), cx);
                workspace
            },
        )
        .expect("failed to open the rudbman window");

        cx.activate(true);
    });
}
