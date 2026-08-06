//! Platform-specific locations of the files rudbman persists.
//!
//! Every path is derived from a single [`directories::ProjectDirs`] instance
//! built from the bare application name, so the whole application agrees on
//! where its configuration lives:
//!
//! * Windows: `%APPDATA%\rudbman\config`
//! * macOS: `~/Library/Application Support/rudbman`
//! * Linux: `~/.config/rudbman`
//!
//! The qualifier and organization fields are deliberately left empty: they are
//! what would turn the macOS directory into `~/Library/Application
//! Support/com.example.rudbman`, and the architecture document pins the plain
//! `rudbman` form.
//!
//! Most of what rudbman persists is a single file in that directory —
//! [`settings_file`], [`connections_file`], [`drivers_file`],
//! [`known_hosts_file`]. What has no fixed number of members gets a
//! subdirectory instead: [`ui_themes_dir`], [`editor_themes_dir`],
//! [`snippets_dir`], [`drivers_dir`] and [`erd_layouts_dir`].

use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::{Context, Result};
use directories::ProjectDirs;

/// Application name, and the last path component of the config directory.
const APP_NAME: &str = "rudbman";

/// Name of the file holding the serialized [`crate::ConnectionStore`].
const CONNECTIONS_FILE_NAME: &str = "connections.json";

/// Name of the file holding the serialized [`crate::DriverStore`].
const DRIVERS_FILE_NAME: &str = "drivers.json";

/// Name of the file holding the serialized [`crate::AppSettings`].
const SETTINGS_FILE_NAME: &str = "settings.json";

/// Name of the file holding the trusted SSH host keys.
const KNOWN_HOSTS_FILE_NAME: &str = "known_hosts";

/// Name of the directory holding user-supplied UI theme files.
const UI_THEMES_DIR_NAME: &str = "themes";

/// Name of the directory holding user-supplied editor theme files.
const EDITOR_THEMES_DIR_NAME: &str = "editor-themes";

/// Name of the directory holding downloaded JDBC driver JARs.
const DRIVERS_DIR_NAME: &str = "drivers";

/// Name of the directory holding the user's saved SQL snippets.
const SNIPPETS_DIR_NAME: &str = "snippets";

/// Name of the directory holding saved ERD box positions.
const ERD_DIR_NAME: &str = "erd";

/// Byte order mark that Windows editors readily prepend to UTF-8 files.
const UTF8_BOM: &[u8] = b"\xEF\xBB\xBF";

/// Strip a leading UTF-8 byte order mark, if there is one.
///
/// Neither `serde_json` nor the `known_hosts` line parser tolerates a BOM: it
/// turns a perfectly valid file into a parse error, or silently glues itself to
/// the first host name. Since these files are meant to be editable by hand, and
/// several Windows editors add a BOM on save, every reader of one goes through
/// here — the theme and snippet files the app layer reads included.
pub fn strip_bom(bytes: &[u8]) -> &[u8] {
    bytes.strip_prefix(UTF8_BOM).unwrap_or(bytes)
}

/// Resolve the project directories for rudbman.
fn project_dirs() -> Result<ProjectDirs> {
    ProjectDirs::from("", "", APP_NAME)
        .context("could not determine a home directory for the current user")
}

/// Directory that holds every rudbman configuration file.
///
/// The directory is *not* created by this call; the writers in this crate create
/// it on demand.
///
/// # Errors
///
/// Fails when no home directory can be determined for the current user.
pub fn config_dir() -> Result<PathBuf> {
    Ok(project_dirs()?.config_dir().to_path_buf())
}

/// Full path of the application settings file (`settings.json`).
///
/// # Errors
///
/// Fails when no home directory can be determined for the current user.
pub fn settings_file() -> Result<PathBuf> {
    Ok(config_dir()?.join(SETTINGS_FILE_NAME))
}

/// Full path of the connection profile database (`connections.json`).
///
/// Passwords are never part of this file; they live in the OS keychain, see
/// [`crate::SecretStore`].
///
/// # Errors
///
/// Fails when no home directory can be determined for the current user.
pub fn connections_file() -> Result<PathBuf> {
    Ok(config_dir()?.join(CONNECTIONS_FILE_NAME))
}

/// Full path of the driver definition file (`drivers.json`).
///
/// # Errors
///
/// Fails when no home directory can be determined for the current user.
pub fn drivers_file() -> Result<PathBuf> {
    Ok(config_dir()?.join(DRIVERS_FILE_NAME))
}

/// Full path of the trusted host key database (`known_hosts`).
///
/// # Errors
///
/// Fails when no home directory can be determined for the current user.
pub fn known_hosts_file() -> Result<PathBuf> {
    Ok(config_dir()?.join(KNOWN_HOSTS_FILE_NAME))
}

/// Directory holding the user's own UI theme files (`themes`).
///
/// One `*.json` file per theme, whose stem is the id the theme is selected by.
/// Like [`config_dir`], the directory is not created by this call; a user who
/// has never added a theme simply has none.
///
/// # Errors
///
/// Fails when no home directory can be determined for the current user.
pub fn ui_themes_dir() -> Result<PathBuf> {
    Ok(config_dir()?.join(UI_THEMES_DIR_NAME))
}

/// Directory holding the user's own editor theme files (`editor-themes`).
///
/// Laid out exactly like [`ui_themes_dir`], but kept apart from it: the two
/// kinds of theme have different shapes (chrome colors versus syntax token
/// colors) and are selected independently, so an id collision between them
/// must not be possible.
///
/// # Errors
///
/// Fails when no home directory can be determined for the current user.
pub fn editor_themes_dir() -> Result<PathBuf> {
    Ok(config_dir()?.join(EDITOR_THEMES_DIR_NAME))
}

/// Directory holding downloaded JDBC driver JARs (`drivers`).
///
/// This is where the driver downloader puts what it fetches from Maven. A
/// [`DriverDef`](crate::DriverDef) may just as well point at JARs anywhere else
/// on disk; nothing forces a driver to live here.
///
/// # Errors
///
/// Fails when no home directory can be determined for the current user.
pub fn drivers_dir() -> Result<PathBuf> {
    Ok(config_dir()?.join(DRIVERS_DIR_NAME))
}

/// Directory holding the user's saved SQL snippets (`snippets`).
///
/// One `*.sql` file per snippet, named after the snippet.
///
/// # Errors
///
/// Fails when no home directory can be determined for the current user.
pub fn snippets_dir() -> Result<PathBuf> {
    Ok(config_dir()?.join(SNIPPETS_DIR_NAME))
}

/// Directory holding the saved layouts of the ERD canvas (`erd`).
///
/// One `<profile-uuid>.json` per connection profile, holding a table's box
/// position per scope (architecture document, §8). Keyed by the *profile* and
/// not by the open connection, because a diagram arranged on staging is worth
/// keeping when that tab is closed and opened again — and two tabs on one
/// profile are two views of the same schema, so they share an arrangement.
///
/// Like [`config_dir`], the directory is not created by this call; a user who
/// has never opened a diagram simply has none.
///
/// # Errors
///
/// Fails when no home directory can be determined for the current user.
pub fn erd_layouts_dir() -> Result<PathBuf> {
    Ok(config_dir()?.join(ERD_DIR_NAME))
}

/// Build a unique temporary path next to `path`.
///
/// Keeping the temporary file in the same directory guarantees that the final
/// rename stays inside one filesystem, which is what makes it atomic.
fn temp_sibling(path: &Path) -> PathBuf {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let seq = COUNTER.fetch_add(1, Ordering::Relaxed);
    let mut name = path
        .file_name()
        .map(OsString::from)
        .unwrap_or_else(|| OsString::from(APP_NAME));
    name.push(format!(".{}.{seq}.tmp", std::process::id()));
    path.with_file_name(name)
}

/// Write `contents` to `path`, replacing any previous file atomically.
///
/// Missing parent directories are created first. The data is written to a
/// temporary sibling file and then renamed over the destination, so a crash
/// mid-write can never leave a half-written configuration behind.
///
/// # Errors
///
/// Fails when the parent directory cannot be created, the temporary file cannot
/// be written, or the rename onto `path` does not go through.
pub fn write_atomic(path: &Path, contents: &[u8]) -> Result<()> {
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create directory {}", parent.display()))?;
    }

    let temp = temp_sibling(path);
    fs::write(&temp, contents)
        .with_context(|| format!("failed to write temporary file {}", temp.display()))?;

    // `rename` replaces the destination on Unix and on Windows (`MoveFileEx`
    // with `MOVEFILE_REPLACE_EXISTING`). Should a platform ever refuse to
    // clobber an existing file, fall back to removing it first.
    if let Err(first) = fs::rename(&temp, path) {
        let _ = fs::remove_file(path);
        if let Err(second) = fs::rename(&temp, path) {
            let _ = fs::remove_file(&temp);
            return Err(second).with_context(|| {
                format!(
                    "failed to move {} onto {} (first attempt: {first})",
                    temp.display(),
                    path.display()
                )
            });
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_paths_share_the_config_directory() {
        let dir = config_dir().expect("config dir");
        let cases: [(PathBuf, &str); 9] = [
            (settings_file().expect("settings"), SETTINGS_FILE_NAME),
            (
                connections_file().expect("connections"),
                CONNECTIONS_FILE_NAME,
            ),
            (drivers_file().expect("drivers"), DRIVERS_FILE_NAME),
            (
                known_hosts_file().expect("known hosts"),
                KNOWN_HOSTS_FILE_NAME,
            ),
            (ui_themes_dir().expect("ui themes"), UI_THEMES_DIR_NAME),
            (
                editor_themes_dir().expect("editor themes"),
                EDITOR_THEMES_DIR_NAME,
            ),
            (drivers_dir().expect("driver jars"), DRIVERS_DIR_NAME),
            (snippets_dir().expect("snippets"), SNIPPETS_DIR_NAME),
            (erd_layouts_dir().expect("erd layouts"), ERD_DIR_NAME),
        ];
        for (path, name) in cases {
            assert_eq!(
                path.parent(),
                Some(dir.as_path()),
                "{name} escaped the config dir"
            );
            assert_eq!(path.file_name().unwrap(), name);
        }
    }

    #[test]
    fn the_config_directory_is_named_after_the_app() {
        let dir = config_dir().expect("config dir");
        assert!(
            dir.components()
                .any(|c| c.as_os_str().to_string_lossy().contains(APP_NAME)),
            "{} does not mention {APP_NAME}",
            dir.display()
        );
    }

    #[test]
    fn strip_bom_removes_only_a_leading_mark() {
        assert_eq!(strip_bom(b"\xEF\xBB\xBF{}"), b"{}");
        assert_eq!(strip_bom(b"{}"), b"{}");
        // A mark in the middle of the file is data, not a BOM.
        assert_eq!(strip_bom(b"{\xEF\xBB\xBF}"), b"{\xEF\xBB\xBF}");
    }

    #[test]
    fn write_atomic_creates_parents_and_overwrites() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("nested").join("deeper").join("data.txt");

        write_atomic(&path, b"first").expect("initial write");
        assert_eq!(fs::read_to_string(&path).unwrap(), "first");

        // Overwriting an existing destination must work on every platform.
        write_atomic(&path, b"second").expect("overwrite");
        assert_eq!(fs::read_to_string(&path).unwrap(), "second");

        // No temporary leftovers.
        let stray: Vec<_> = fs::read_dir(path.parent().unwrap())
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name())
            .filter(|name| name.to_string_lossy().ends_with(".tmp"))
            .collect();
        assert!(stray.is_empty(), "temporary files left behind: {stray:?}");
    }
}
