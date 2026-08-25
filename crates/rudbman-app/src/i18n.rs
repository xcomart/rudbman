//! Interface language: which locales exist, and which one is active.
//!
//! The translations themselves live in `crates/rudbman-app/locales/<tag>.yml`
//! and are compiled into the binary by `rust_i18n::i18n!` in [`crate`]'s root,
//! so nothing here touches the filesystem. `rust-i18n` compiles a crate's
//! locale files into *that* crate and keeps the active locale in a process
//! global, which is why the table cannot move into the shell and why
//! [`crate::AppStrings`] exists to lend it out. What this module does is decide
//! *which* locale `t!` should read from, and offer [`ts!`] for the one thing
//! the widget layer needs that `t!` does not give:
//! a [`SharedString`][gpui::SharedString].
//!
//! The arithmetic behind that decision is [`rugpui_shell::locale`]'s — matching a
//! platform's spelling of a tag against the ones an application ships is the
//! same problem in every application — and what is left here is the two ends of
//! it: which tags rudbman ships, and telling `rust-i18n` the answer.
//!
//! Resolution order, applied by [`apply`] at start-up and again whenever the
//! settings dialog saves:
//!
//! 1. the tag stored in `settings.json`, when rudbman ships that language;
//! 2. the operating system's locale, matched loosely;
//! 3. English.
//!
//! Step 3 is also `rust-i18n`'s compile-time `fallback`, so a key missing from
//! a translation falls back per-key rather than switching the whole UI.
//!
//! # Adding a language
//!
//! Drop a `<BCP 47 tag>.yml` next to the others, translate every key of
//! `en.yml` — `language.name` included, since that is the endonym the settings
//! dialog lists the language under — and rebuild. No source file mentions the
//! set of languages, so none needs editing.

use std::sync::OnceLock;

use gpui::SharedString;

#[cfg(test)]
pub use rugpui_shell::locale::FALLBACK;

/// Translates a key and hands the result back as a [`SharedString`].
///
/// `rust-i18n` yields a `Cow<str>`, which no gpui builder accepts; every call
/// site would otherwise repeat the same conversion. Takes exactly the arguments
/// [`rust_i18n::t`] takes, interpolation included:
///
/// ```ignore
/// ts!("about.title")
/// ts!("about.version", version = VERSION)
/// ```
///
/// [`SharedString`]: gpui::SharedString
macro_rules! ts {
    ($($args:tt)*) => {
        ::gpui::SharedString::from(::rust_i18n::t!($($args)*).into_owned())
    };
}

pub(crate) use ts;

/// The tags of the locale files compiled into the binary, sorted.
///
/// `available_locales!` hands back `Cow`s; owning them once in a `OnceLock`
/// turns them into the `&'static str`s the rest of the module passes around.
fn tags() -> &'static [String] {
    static TAGS: OnceLock<Vec<String>> = OnceLock::new();
    TAGS.get_or_init(|| {
        let mut tags: Vec<String> = rust_i18n::available_locales!()
            .into_iter()
            .map(std::borrow::Cow::into_owned)
            .collect();
        tags.sort();
        tags
    })
}

/// The same tags as [`rugpui_shell::locale`] wants them: a sorted slice of
/// `&'static str`, which is the order that makes its primary-subtag rule
/// deterministic.
pub fn shipped() -> &'static [&'static str] {
    static SHIPPED: OnceLock<Vec<&'static str>> = OnceLock::new();
    SHIPPED.get_or_init(|| tags().iter().map(String::as_str).collect())
}

/// The locales rudbman ships translations for, as `(BCP 47 tag, endonym)`,
/// ordered by tag.
///
/// Derived from the locale files themselves rather than from a list kept in
/// this module, so shipping one more language is a matter of adding one more
/// file. The endonym comes from that file's `language.name`; it is written in
/// the language it names and is deliberately not translated, so caching it is
/// safe — unlike most lookups it does not depend on the active locale.
pub fn supported() -> &'static [(&'static str, SharedString)] {
    static SUPPORTED: OnceLock<Vec<(&'static str, SharedString)>> = OnceLock::new();
    SUPPORTED.get_or_init(|| {
        shipped()
            .iter()
            .map(|tag| (*tag, ts!("language.name", locale = tag)))
            .collect()
    })
}

/// The endonym of `tag`, or `None` when rudbman ships no such translation.
pub fn display_name(tag: &str) -> Option<&'static str> {
    rugpui_shell::locale::display_name(supported(), tag)
}

/// The locale to render the UI in, given the configured `language`.
///
/// `None`, a blank string, or a tag rudbman has no translation for all fall
/// through to the system locale, and from there to [`FALLBACK`].
pub fn resolve(language: Option<&str>) -> String {
    let system = sys_locale::get_locale();
    rugpui_shell::locale::resolve(shipped(), language, system.as_deref())
}

/// Make [`resolve`]'s answer the locale `t!` reads from.
pub fn apply(language: Option<&str>) {
    rust_i18n::set_locale(&resolve(language));
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use rust_i18n::t;

    use super::*;

    /// One key per top-level namespace of `en.yml`, chosen so that no
    /// translation of it legitimately coincides with the English wording.
    const PROBES: [&str; 11] = [
        "language.name",
        "common.close",
        "menu.new_connection",
        "tab.close",
        "empty.connected_title",
        "statusbar.no_connection",
        "about.title",
        "connect.title",
        "driver.class",
        "context.select_all",
        "update.ignore",
    ];

    /// The directory `i18n!` compiles the translations out of.
    fn locales() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("locales")
    }

    #[test]
    fn every_locale_file_is_sound() {
        // The whole of what can be checked by reading the *files* rather than
        // the compiled-in table: the same key set everywhere, and no value that
        // YAML swallows into something other than its text. Both are invisible
        // in a running app — the first because the per-key fallback answers in
        // English, the second because a swallowed value renders as a blank.
        rugpui_shell::locale::check_locale_dir(&locales(), shipped());
    }

    #[test]
    fn the_shipped_languages_are_the_compiled_in_locales_in_tag_order() {
        let mut expected = rust_i18n::available_locales!();
        expected.sort();
        assert_eq!(shipped(), expected.as_slice());
    }

    #[test]
    fn every_locale_translates_every_namespace() {
        // A key missing from a translation is answered in English by the
        // `fallback = "en"` of `i18n!`, so a silently mis-nested key would look
        // like a working lookup. Asserting that a non-English locale answers
        // with something *other* than English is what catches it.
        for (tag, _) in supported().iter().filter(|(tag, _)| *tag != FALLBACK) {
            for key in PROBES {
                assert_ne!(
                    t!(key, locale = *tag),
                    t!(key, locale = FALLBACK),
                    "{key} is untranslated in {tag}"
                );
            }
        }
    }

    #[test]
    fn every_language_names_itself_distinctly() {
        // `language.name` is what the settings dialog lists a language under,
        // so a file that omits it would show up as "English" — the per-key
        // fallback — and two entries would be indistinguishable. The
        // `every_locale_translates_every_namespace` probe already catches the
        // leak; this catches the collision, including one between two locales
        // that both spell out a name of their own.
        let mut seen: Vec<&str> = Vec::new();
        for (tag, name) in supported() {
            assert!(!name.is_empty(), "{tag} names itself with an empty string");
            assert!(
                !seen.contains(&name.as_str()),
                "{tag} shares the display name {name:?} with another locale"
            );
            seen.push(name);
        }
    }

    #[test]
    fn every_supported_tag_is_found_by_its_own_name() {
        for (tag, name) in supported() {
            assert_eq!(display_name(tag), Some(name.as_str()), "name of {tag}");
        }
    }

    #[test]
    fn a_configured_language_wins_over_the_system_locale() {
        // The only branch of `resolve` that can be asserted without controlling
        // the environment: a supported tag never consults `sys_locale`.
        assert_eq!(resolve(Some("ru")), "ru");
        assert_eq!(resolve(Some("zh_TW")), "zh-CN");
    }

    #[test]
    fn resolve_always_answers_with_a_supported_locale() {
        // Covers the system-locale and fallback branches without assuming what
        // the machine running the tests is set to.
        for language in [None, Some(""), Some("xx-YZ")] {
            let resolved = resolve(language);
            assert!(
                shipped().contains(&resolved.as_str()),
                "resolve({language:?}) returned {resolved}"
            );
        }
    }
}
