//! Who rudbman is, as far as the shell is concerned.
//!
//! [`ruui_shell`] is written against an application it deliberately knows
//! nothing about: the about dialog draws a name it was handed, the updater asks
//! an endpoint it was handed, and every word either of them says is looked up
//! in a table it was handed. This module is the whole of what rudbman hands
//! over, and [`install`] is the one call that does it.
//!
//! Three things go across:
//!
//! * [`IDENTITY`] — the constants. The version above all: it is the
//!   *application's* `CARGO_PKG_VERSION` and can only be read here, because the
//!   shell has a version of its own and it is not this one.
//! * [`Strings`] — the words, looked up by the very keys `locales/*.yml`
//!   already carry, so adopting the shell changed no translation.
//! * [`UpdatePolicy`] — reading and writing the "never mention this release
//!   again" tag, which lives in rudbman's `settings.json` and is therefore
//!   rudbman's to persist.
//!
//! The Windows uninstall key is here for a different reason than the rest: it
//! is a *published identifier of this product*, one corner of a triangle with
//! `packaging/windows/rudbman.iss` and the winget manifests, and the test at
//! the bottom of this file is what keeps two of those three corners together.

use gpui::{App, SharedString};
use ruui_shell::{AppIdentity, Strings, UpdatePolicy};

use crate::app_settings;
use crate::i18n::ts;

/// Where "Update" goes when the API answered without an `html_url`.
///
/// The releases index rather than the project page: whatever the user came here
/// for, it is a download.
const RELEASES_PAGE: &str = "https://github.com/xcomart/rudbman/releases";

/// What a release archive carries that has to end up on disk, in install order.
///
/// The executable first, because it is the one whose *installed* name may
/// differ from the published one; then the bundled runtime, which is most of
/// the archive's weight and all of the reason an update is measured in tens of
/// megabytes.
#[cfg(windows)]
const PAYLOAD: &[&str] = &["rudbman.exe", "lib", "runtime"];

/// What a release archive carries; on macOS the bundle is the whole of it.
#[cfg(target_os = "macos")]
const PAYLOAD: &[&str] = &["rudbman.app"];

/// What a release archive carries, on Linux.
#[cfg(all(unix, not(target_os = "macos")))]
const PAYLOAD: &[&str] = &["rudbman", "lib", "runtime"];

/// The "Apps & features" entry the Windows installer leaves behind, relative to
/// `HKEY_CURRENT_USER` or `HKEY_LOCAL_MACHINE`.
///
/// The GUID in the middle of it is one corner of a triangle that has to agree,
/// and it is a published identifier rather than an implementation detail:
///
/// * `packaging/windows/rudbman.iss` sets it as Inno Setup's `AppId`, and Inno
///   derives this key's name from it by appending `_is1`;
/// * the manifests under `packaging/winget/*/` record the same string, braces
///   and suffix included, as the package's `ProductCode`;
/// * and the shell's updater uses it to find the entry again, so that a copy
///   which has updated itself still reports its real version to winget.
///
/// Move any one corner without the other two and winget stops recognising an
/// installed rudbman: `winget list` finds nothing, `winget upgrade` offers a
/// fresh install to sit beside the existing one, and `winget uninstall` has
/// nothing to remove — all silently, because a key that is not there is
/// indistinguishable from a copy that was never installed. None of the three
/// ever changes; see the README in `packaging/winget/`.
const ARP_KEY: &str = concat!(
    r"Software\Microsoft\Windows\CurrentVersion\Uninstall\",
    "{E09022E1-1203-4A82-A00E-6385C2594DEF}_is1"
);

/// Everything the shell has to be told about rudbman.
pub const IDENTITY: AppIdentity = AppIdentity {
    name: "rudbman",
    version: env!("CARGO_PKG_VERSION"),
    repository_url: "https://github.com/xcomart/rudbman",
    repository_label: "github.com/xcomart/rudbman",
    latest_release_api: "https://api.github.com/repos/xcomart/rudbman/releases/latest",
    releases_page: RELEASES_PAGE,
    fallback_archive: "rudbman-update",
    payload: PAYLOAD,
    bundle_executable: "Contents/MacOS/rudbman",
    windows_arp_key: ARP_KEY,
    // On Windows a JVM loaded into this process holds open handles on the very
    // files the swap renames, so the renames have to wait for the next launch.
    // A question only rudbman can answer, because the answer is about what
    // rudbman has loaded.
    must_defer: || cfg!(windows) && rudbman_jdbc::Jvm::get().is_some(),
};

/// rudbman's translations, as the shell reads them.
///
/// One line over `rust-i18n`, and that is the point: the shell names the key
/// and rudbman answers with whatever the active locale has for it.
/// Interpolation is not done here — a template comes back with its
/// `%{marker}`s intact and the shell fills them in, which is what lets it
/// substitute a value the key never mentioned.
struct AppStrings;

impl Strings for AppStrings {
    fn text(&self, key: &str) -> SharedString {
        ts!(key)
    }
}

/// Where the ignored release is kept: rudbman's own `settings.json`.
///
/// Written through immediately rather than at the next save. It is a decision
/// the user has just made in a dialog, and it should survive a crash the way a
/// saved setting does.
struct IgnoredUpdate;

impl UpdatePolicy for IgnoredUpdate {
    fn ignored(&self, cx: &App) -> Option<String> {
        app_settings::current(cx).ignored_update
    }

    fn set_ignored(&self, tag: Option<String>, cx: &mut App) {
        let mut settings = app_settings::current(cx);
        settings.ignored_update = tag;
        app_settings::replace(settings, cx);
        app_settings::save(cx);
    }
}

/// Hands the shell all three, once, before the first window opens.
///
/// Order matters only in that nothing may render or check for a release until
/// this has run: [`ruui_shell::identity`] panics without it, deliberately,
/// because reaching it unwired is a mistake in `main` and not a runtime
/// condition.
pub fn install(cx: &mut App) {
    ruui_shell::init(IDENTITY, cx);
    ruui_shell::set_strings(Box::new(AppStrings), cx);
    ruui_shell::set_update_policy(Box::new(IgnoredUpdate), cx);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_uninstall_key_is_the_one_the_installer_writes() {
        // The triangle from `ARP_KEY`'s docs, one side of it checked
        // mechanically. Inno Setup names its uninstall key by appending `_is1`
        // to `AppId`, so if these two ever drift the updater goes on running and
        // silently stops correcting the version — the exact failure mode that is
        // hardest to notice. The third side, the `ProductCode` in the winget
        // manifests, is not checked here only because that directory is named
        // after a release and would have to be found rather than named.
        let script = include_str!("../../../packaging/windows/rudbman.iss");
        // The doubled brace is Inno's escape for a literal one, so what follows
        // it is the GUID with its own closing brace still attached.
        let app_id = script
            .lines()
            .find_map(|line| line.trim().strip_prefix("AppId={{"))
            .expect("an AppId in rudbman.iss");
        assert!(
            IDENTITY
                .windows_arp_key
                .ends_with(&format!("{{{app_id}_is1")),
            "{} is not the key Inno derives from AppId={{{app_id}",
            IDENTITY.windows_arp_key
        );
    }

    #[test]
    fn the_payload_names_the_executable_first() {
        // The shell's install plan treats the first entry as the one whose
        // installed name may differ from the published one, and rolls the whole
        // swap back around it. A payload that led with `lib` would still
        // install and would restore the wrong thing on a failure.
        let first = IDENTITY.payload.first().expect("a payload");
        assert!(
            first.starts_with("rudbman"),
            "the payload leads with {first}, not the executable"
        );
    }
}
