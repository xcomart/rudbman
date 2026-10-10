use super::*;

#[test]
fn a_pinned_editor_theme_is_left_alone() {
    // The switch is off, so nothing about the chrome may reach the editor —
    // not even a cast that clashes with it.
    assert_eq!(
        editor_theme_for("tokyo-night", false, "one-light", false, &entries()),
        "tokyo-night"
    );
}

#[test]
fn following_the_ui_prefers_the_chrome_themes_namesake() {
    // The rule the switch is named for: the editor theme sharing the chrome
    // theme's id wins, whatever the settings file still carries. A dark
    // editor under a light window is dropped for the light namesake…
    assert_eq!(
        editor_theme_for("tokyo-night", true, "one-light", false, &entries()),
        "one-light"
    );
    // …and so is a dark editor under a *different* dark window, which is
    // the half that used to be skipped: the cast already matched, so the
    // configured id won and the editor never moved off One Dark however far
    // the chrome went. Every built-in chrome theme has a namesake here, so
    // this is the path every built-in takes.
    assert_eq!(
        editor_theme_for("one-dark", true, "dracula", true, &entries()),
        "dracula"
    );
    assert_eq!(
        editor_theme_for("dracula", true, "gruvbox-dark", true, &entries()),
        "gruvbox-dark"
    );
}

#[test]
fn following_the_ui_keeps_the_configured_pick_when_the_chrome_has_no_namesake() {
    // A chrome theme of the user's own, which no editor theme is named
    // after: there is no pair to honour, so the configured editor theme
    // stands as long as its cast fits the window.
    assert_eq!(
        editor_theme_for("tokyo-night", true, "my-chrome", true, &entries()),
        "tokyo-night"
    );
}

#[test]
fn following_the_ui_refuses_a_namesake_of_the_wrong_cast() {
    // The one thing that outranks the name match. A user has written a dark
    // editor theme under the id of a light chrome theme; pairing them would
    // put a dark editor in a light window, which is the accident this switch
    // exists to prevent, so the namesake is passed over.
    let mut entries = entries();
    entries.push(EditorThemeEntry {
        id: "my-chrome".to_string(),
        name: "Mine".to_string(),
        dark: true,
        builtin: false,
    });
    assert_eq!(
        editor_theme_for("solarized-light", true, "my-chrome", false, &entries),
        "solarized-light"
    );
}

#[test]
fn following_the_ui_falls_back_to_any_theme_of_the_right_cast() {
    // A chrome theme with no editor theme of its name — a palette the user
    // wrote themselves, say — still has to produce a light editor.
    assert_eq!(
        editor_theme_for("one-dark", true, "my-light-theme", false, &entries()),
        "one-light"
    );
}

#[test]
fn following_the_ui_keeps_the_configured_id_when_nothing_matches() {
    // Nothing of the right cast exists, so there is no better answer than
    // the id the settings already carry; resolving it falls back on its own.
    let only_dark = vec![EditorThemeEntry {
        id: "one-dark".to_string(),
        name: "One Dark".to_string(),
        dark: true,
        builtin: true,
    }];
    assert_eq!(
        editor_theme_for("one-dark", true, "one-light", false, &only_dark),
        "one-dark"
    );
    // And an empty registry cannot make one up either.
    assert_eq!(
        editor_theme_for("whatever", true, "one-light", false, &[]),
        "whatever"
    );
}

#[test]
fn ids_are_matched_case_insensitively() {
    // `settings.json` is hand-editable and the registries resolve ids
    // case-insensitively, so this rule has to as well.
    assert_eq!(
        editor_theme_for("One-Dark", true, "irrelevant", true, &entries()),
        "one-dark"
    );
}

#[test]
fn every_empty_state_wording_is_translated() {
    // `t!` answers with the key path when a key is missing, so a typo
    // reaches the screen as "empty.connected_ttle". The two hints have to
    // differ, or the connected state would still read like the welcome
    // screen's.
    for label in [
        ts!("welcome.hint", shortcut = "Ctrl+N"),
        ts!("welcome.new_connection"),
        ts!("welcome.saved"),
        ts!("empty.hint"),
        ts!("empty.connected_title"),
        ts!("empty.connected_hint"),
        ts!("statusbar.no_connection"),
        ts!("statusbar.idle"),
        ts!("statusbar.connecting"),
        ts!("statusbar.connected"),
        ts!("statusbar.disconnected"),
        ts!("statusbar.failed", error = "e"),
        ts!("statusbar.tunnel_lost", reason = "r"),
    ] {
        assert!(!label.is_empty(), "empty label");
        for namespace in ["welcome.", "empty.", "statusbar."] {
            assert!(
                !label.starts_with(namespace),
                "untranslated label {label:?}"
            );
        }
    }
    assert_ne!(ts!("empty.hint"), ts!("empty.connected_hint"));
    // The welcome screen shows one hint line or the other, never both, so
    // a shared wording would make the two states indistinguishable.
    assert_ne!(ts!("welcome.hint", shortcut = "Ctrl+N"), ts!("empty.hint"));
}

#[test]
fn every_label_the_shell_menus_draw_has_a_translation() {
    for label in [
        ts!("context.close_tab"),
        ts!("context.close_others"),
        ts!("context.close_right"),
        ts!("context.split_right"),
        ts!("context.split_below"),
        ts!("context.close_pane"),
        ts!("context.connect"),
        ts!("context.edit"),
    ] {
        assert!(!label.is_empty(), "empty label");
        assert!(!label.starts_with("context."), "untranslated {label:?}");
    }
}

#[test]
fn a_maximized_window_is_restored_maximized() {
    // The bounds a maximized window carries are its *restore* size, so both
    // halves have to survive: the state, and the size to un-maximize to.
    let state = WindowState {
        x: Some(10),
        y: Some(20),
        width: 1280,
        height: 720,
        maximized: true,
        ..WindowState::default()
    };
    let geometry = app_settings::saved_geometry(&state).expect("the position is set");
    assert_eq!(geometry.bounds().size.width, px(1280.));
    assert_eq!(geometry.bounds().origin.x, px(10.));
    assert!(state.maximized);
}

#[test]
fn the_two_titlebar_spellings_are_one_setting() {
    // `rudbman-core` and `rugpui-shell` each declare a `TitlebarStyle`,
    // because one has to stay free of gpui and the other cannot. What makes
    // `chrome_titlebar` a conversion rather than a translation is that both
    // write the same two words into `settings.json`.
    for (mine, theirs) in [
        (TitlebarStyle::Custom, rugpui_shell::TitlebarStyle::Custom),
        (TitlebarStyle::System, rugpui_shell::TitlebarStyle::System),
    ] {
        assert_eq!(chrome_titlebar(mine), theirs);
        assert_eq!(
            serde_json::to_string(&mine).expect("a style serialises"),
            serde_json::to_string(&theirs).expect("a style serialises"),
        );
    }
    // And the default is the same one on both sides, which is what a
    // settings file with no `titlebar` key gets.
    assert_eq!(
        chrome_titlebar(TitlebarStyle::default()),
        rugpui_shell::TitlebarStyle::default()
    );
}
