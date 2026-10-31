//! What each connection profile has run: `history/<profile-uuid>.json`.
//!
//! Every statement a query tab sends because the user asked it to — run one,
//! run the selection, run everything — is written down with when it ran, how
//! long it took and how it ended, and the history dialog
//! ([`crate::workspace`]'s `history` module) reads it back to search and to
//! open again. What a tab sends on the user's behalf is not: a sort is a
//! wrapper around a statement already written down, an apply's batch is the
//! grid's and is shown before it runs, and a plan ran nothing. Neither is a
//! statement the profile refused to send.
//!
//! # One file per profile, kept in a global
//!
//! Keyed by the *profile*, as the workspace and the diagram layouts are: two
//! tabs on one profile are two views of one database, and closing a tab and
//! opening it again finds what was run there. The entries live in a gpui
//! global so that every tab on a profile writes into one list and the dialog
//! reads that same list; the file is read the first time a profile is asked
//! about, and written a little after each change, off the UI thread, with the
//! writes kept in the order they were queued — the workspace's scheme, see
//! `workspace::session`. Whatever is still queued when the application quits
//! is written then.
//!
//! # Bounded
//!
//! [`MAX_ENTRIES`] statements and [`MAX_TOTAL_BYTES`] of SQL per profile, the
//! oldest dropped first; a statement over [`MAX_STATEMENT_BYTES`] is not
//! written down at all. A script that size is a file the user has somewhere,
//! and a history that kept it would be rewriting it after every statement.
//!
//! # Secrets
//!
//! A statement can carry one — `CREATE USER … IDENTIFIED BY`, a literal in a
//! `WHERE` — and the file is plain JSON in the configuration directory. The
//! settings dialog turns recording off ([`AppSettings::query_history`]), and
//! the dialog deletes an entry or a profile's whole history.
//!
//! Loading is forgiving and writing is atomic, as everything persisted is: a
//! missing file is a first run, a byte order mark is stripped, and a file that
//! cannot be read is left exactly as it is — never overwritten by a history
//! that started empty because of it.
//!
//! [`AppSettings::query_history`]: rudbman_core::AppSettings::query_history

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use gpui::{App, BorrowAppContext, Global, Task};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Statements kept per profile.
pub const MAX_ENTRIES: usize = 1_000;

/// SQL kept per profile, in bytes, before the oldest entries go.
pub const MAX_TOTAL_BYTES: usize = 4 * 1024 * 1024;

/// The longest statement written down, in bytes.
pub const MAX_STATEMENT_BYTES: usize = 256 * 1024;

/// The longest failure message kept with an entry, in characters.
const MAX_MESSAGE_CHARS: usize = 400;

/// How long a change waits before it is written, so that a script of a
/// hundred statements is one write rather than a hundred.
const SAVE_DELAY: Duration = Duration::from_millis(500);

/// Schema version written by this build. Informational: every field
/// defaults, so a file carrying another number still loads.
const CURRENT_VERSION: u32 = 1;

/// The subdirectory of the configuration directory the files live in.
const DIRECTORY_NAME: &str = "history";

/// One statement that was run.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct HistoryEntry {
    /// Unique within its profile's history, and never reused: what a
    /// deletion names.
    pub id: u64,
    /// When it ran, in milliseconds since the Unix epoch.
    pub at: i64,
    /// The statement, as it was sent.
    pub sql: String,
    /// Execution through the first batch of rows, in milliseconds.
    pub elapsed_ms: u64,
    /// How it ended.
    pub outcome: Outcome,
}

/// How a statement ended.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Outcome {
    /// It answered with rows: as many as the first batch held, and whether
    /// there were more.
    Rows {
        /// Rows read.
        count: u64,
        /// Whether the result had more than were read.
        more: bool,
    },
    /// It changed rows.
    Affected {
        /// Rows the driver said it changed.
        count: u64,
    },
    /// It ran, and said nothing more: DDL, a write that changed nothing.
    Done,
    /// The database refused it.
    Failed {
        /// The driver's words, cut short.
        message: String,
    },
    /// The user stopped it.
    Canceled,
}

impl Outcome {
    /// A failure, with its message cut to what an entry keeps.
    pub fn failed(message: &str) -> Self {
        let message = message.trim();
        let mut kept: String = message.chars().take(MAX_MESSAGE_CHARS).collect();
        if kept.len() < message.len() {
            kept.push('…');
        }
        Self::Failed { message: kept }
    }

    /// Whether the statement did not get to the end.
    pub fn is_failure(&self) -> bool {
        matches!(self, Self::Failed { .. } | Self::Canceled)
    }
}

/// What a file holds.
#[derive(Default, Serialize, Deserialize)]
struct HistoryFile {
    #[serde(default)]
    version: u32,
    /// Oldest first.
    #[serde(default)]
    entries: Vec<HistoryEntry>,
}

/// Every profile's history that has been asked about, and the writes still
/// owed to disk.
pub struct QueryHistory {
    /// Where the files go; `None` writes nothing, which is what the tests run
    /// under unless they name a directory of their own.
    directory: Option<PathBuf>,
    /// Each profile's entries, oldest first, once read.
    profiles: HashMap<Uuid, Vec<HistoryEntry>>,
    /// Profiles whose file could not be read, and which are therefore never
    /// written.
    blocked: HashSet<Uuid>,
    /// Profiles changed since the last write was queued.
    dirty: HashSet<Uuid>,
    /// Snapshots waiting for the writer.
    pending: Arc<Mutex<HashMap<Uuid, Vec<HistoryEntry>>>>,
    /// Keeps two writes in the order their snapshots were queued.
    writer: Arc<Mutex<()>>,
    /// The delayed write, while one is scheduled.
    save: Option<Task<()>>,
}

impl Global for QueryHistory {}

impl QueryHistory {
    fn new(directory: Option<PathBuf>) -> Self {
        Self {
            directory,
            profiles: HashMap::new(),
            blocked: HashSet::new(),
            dirty: HashSet::new(),
            pending: Arc::default(),
            writer: Arc::default(),
            save: None,
        }
    }

    /// Installs the global, once, and writes whatever is still owed when the
    /// application quits.
    ///
    /// Headless tests never read or write the user's history: under `cfg(test)`
    /// the store starts with no directory, and a test that wants files names
    /// one with [`QueryHistory::use_directory`].
    pub fn init(cx: &mut App) {
        if cx.has_global::<Self>() {
            return;
        }
        let directory = if cfg!(test) {
            None
        } else {
            rudbman_core::config_dir()
                .map(|dir| dir.join(DIRECTORY_NAME))
                .ok()
        };
        cx.set_global(Self::new(directory));
        cx.on_app_quit(|cx| {
            if cx.has_global::<Self>() {
                cx.update_global::<Self, _>(|history, _| history.flush());
            }
            async {}
        })
        .detach();
    }

    /// Points the store at `directory`, forgetting everything read so far.
    #[cfg(test)]
    pub fn use_directory(cx: &mut App, directory: &Path) {
        Self::init(cx);
        cx.set_global(Self::new(Some(directory.to_path_buf())));
    }

    /// Where `profile`'s history is kept, for the documentation and for the
    /// dialog's footer; `None` when there is nowhere to keep one.
    pub fn file_for(cx: &App, profile: Uuid) -> Option<PathBuf> {
        let history = cx.try_global::<Self>()?;
        Some(Self::path(history.directory.as_ref()?, profile))
    }

    fn path(directory: &Path, profile: Uuid) -> PathBuf {
        directory.join(format!("{profile}.json"))
    }

    /// `profile`'s history, newest first.
    pub fn entries(cx: &mut App, profile: Uuid) -> Vec<HistoryEntry> {
        Self::init(cx);
        cx.update_global::<Self, _>(|history, _| {
            history.loaded(profile).iter().rev().cloned().collect()
        })
    }

    /// Writes one statement down, unless recording is off or the statement is
    /// too long to keep.
    pub fn record(cx: &mut App, profile: Uuid, sql: &str, elapsed: Duration, outcome: Outcome) {
        if !crate::app_settings::current(cx).query_history {
            return;
        }
        let sql = sql.trim();
        if sql.is_empty() || sql.len() > MAX_STATEMENT_BYTES {
            return;
        }
        Self::init(cx);
        let at = chrono::Utc::now().timestamp_millis();
        cx.update_global::<Self, _>(|history, cx| {
            let entries = history.loaded(profile);
            let id = entries.last().map_or(1, |entry| entry.id + 1);
            entries.push(HistoryEntry {
                id,
                at,
                sql: sql.to_string(),
                elapsed_ms: u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX),
                outcome,
            });
            trim(entries);
            history.changed(profile, cx);
        });
    }

    /// Deletes entry `id` from `profile`'s history.
    pub fn delete(cx: &mut App, profile: Uuid, id: u64) {
        Self::init(cx);
        cx.update_global::<Self, _>(|history, cx| {
            let entries = history.loaded(profile);
            let before = entries.len();
            entries.retain(|entry| entry.id != id);
            if entries.len() != before {
                history.changed(profile, cx);
            }
        });
    }

    /// Deletes `profile`'s whole history.
    pub fn clear(cx: &mut App, profile: Uuid) {
        Self::init(cx);
        cx.update_global::<Self, _>(|history, cx| {
            let entries = history.loaded(profile);
            if !entries.is_empty() {
                entries.clear();
                history.changed(profile, cx);
            }
        });
    }

    /// `profile`'s entries, read from its file the first time they are asked
    /// for.
    fn loaded(&mut self, profile: Uuid) -> &mut Vec<HistoryEntry> {
        if !self.profiles.contains_key(&profile) {
            let entries = match &self.directory {
                Some(directory) => match read(&Self::path(directory, profile)) {
                    Ok(entries) => entries,
                    Err(error) => {
                        // Left as it is: a history that started empty because
                        // the file was unreadable must not be written over it.
                        log::error!("could not read the query history of {profile}: {error:#}");
                        self.blocked.insert(profile);
                        Vec::new()
                    }
                },
                None => Vec::new(),
            };
            self.profiles.insert(profile, entries);
        }
        self.profiles.get_mut(&profile).expect("inserted above")
    }

    /// Notes that `profile` changed, and writes it a little later.
    fn changed(&mut self, profile: Uuid, cx: &mut App) {
        if self.directory.is_none() || self.blocked.contains(&profile) {
            return;
        }
        self.dirty.insert(profile);
        if self.save.is_some() {
            return;
        }
        self.save = Some(cx.spawn(async move |cx| {
            cx.background_executor().timer(SAVE_DELAY).await;
            cx.update(|cx| {
                cx.update_global::<Self, _>(|history, cx| {
                    history.save = None;
                    history.queue();
                    let (directory, pending, writer) = (
                        history.directory.clone(),
                        history.pending.clone(),
                        history.writer.clone(),
                    );
                    // Serializing a history is a pass over every entry in it,
                    // so it happens here, off the UI thread, with the write.
                    cx.background_executor()
                        .spawn(async move {
                            if let Some(directory) = directory {
                                write_pending(&directory, &pending, &writer);
                            }
                        })
                        .detach();
                });
            });
        }));
    }

    /// Moves a snapshot of every changed profile to the writer's queue.
    fn queue(&mut self) {
        let mut pending = self.pending.lock();
        for profile in self.dirty.drain() {
            if let Some(entries) = self.profiles.get(&profile) {
                pending.insert(profile, entries.clone());
            }
        }
    }

    /// Writes everything still owed, on this thread.
    fn flush(&mut self) {
        self.save = None;
        self.queue();
        if let Some(directory) = &self.directory {
            write_pending(directory, &self.pending, &self.writer);
        }
    }
}

/// Drops the oldest entries until the history is within both bounds.
fn trim(entries: &mut Vec<HistoryEntry>) {
    let mut bytes: usize = entries.iter().map(|entry| entry.sql.len()).sum();
    let mut dropped = 0;
    while dropped < entries.len()
        && (entries.len() - dropped > MAX_ENTRIES || bytes > MAX_TOTAL_BYTES)
    {
        bytes -= entries[dropped].sql.len();
        dropped += 1;
    }
    entries.drain(..dropped);
}

/// Reads one profile's file; a missing one is an empty history.
fn read(path: &Path) -> anyhow::Result<Vec<HistoryEntry>> {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error.into()),
    };
    let file: HistoryFile = serde_json::from_slice(rudbman_core::paths::strip_bom(&bytes))?;
    Ok(file.entries)
}

/// Writes every queued snapshot, in the order the writer lock is taken.
fn write_pending(
    directory: &Path,
    pending: &Mutex<HashMap<Uuid, Vec<HistoryEntry>>>,
    writer: &Mutex<()>,
) {
    let _writing = writer.lock();
    let queued = std::mem::take(&mut *pending.lock());
    for (profile, entries) in queued {
        let path = QueryHistory::path(directory, profile);
        let file = HistoryFile {
            version: CURRENT_VERSION,
            entries,
        };
        let result = serde_json::to_vec(&file)
            .map_err(anyhow::Error::from)
            .and_then(|bytes| rudbman_core::paths::write_atomic(&path, &bytes));
        if let Err(error) = result {
            log::error!(
                "could not save the query history {}: {error:#}",
                path.display()
            );
            // Retried with the next write, unless a newer snapshot was queued
            // meanwhile, which that write carries instead.
            pending.lock().entry(profile).or_insert(file.entries);
        }
    }
}

/// When an entry ran, in the computer's own time zone, as the dialog lists it.
pub fn local_time(at: i64) -> String {
    use chrono::TimeZone;
    match chrono::Local.timestamp_millis_opt(at) {
        chrono::LocalResult::Single(time) | chrono::LocalResult::Ambiguous(time, _) => {
            time.format("%Y-%m-%d %H:%M:%S").to_string()
        }
        chrono::LocalResult::None => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(id: u64, sql: &str) -> HistoryEntry {
        HistoryEntry {
            id,
            at: 0,
            sql: sql.to_string(),
            elapsed_ms: 1,
            outcome: Outcome::Done,
        }
    }

    /// The oldest go first, whichever bound is crossed.
    #[test]
    fn a_history_is_kept_within_both_bounds() {
        let mut entries: Vec<HistoryEntry> = (0..MAX_ENTRIES as u64 + 5)
            .map(|id| entry(id, "select 1"))
            .collect();
        trim(&mut entries);
        assert_eq!(entries.len(), MAX_ENTRIES);
        assert_eq!(entries[0].id, 5, "the oldest were not the ones dropped");

        let big = "x".repeat(MAX_STATEMENT_BYTES);
        let mut entries: Vec<HistoryEntry> = (0..20).map(|id| entry(id, &big)).collect();
        trim(&mut entries);
        let bytes: usize = entries.iter().map(|entry| entry.sql.len()).sum();
        assert!(bytes <= MAX_TOTAL_BYTES, "{bytes}");
        assert_eq!(entries.len(), MAX_TOTAL_BYTES / MAX_STATEMENT_BYTES);
        assert_eq!(entries.last().unwrap().id, 19, "the newest went");

        let mut entries = vec![entry(1, "a"), entry(2, "b")];
        trim(&mut entries);
        assert_eq!(entries.len(), 2, "a small history lost entries");
    }

    /// A failure keeps the start of the driver's words, marked as cut when it
    /// was.
    #[test]
    fn a_failure_message_is_cut_short() {
        let Outcome::Failed { message } = Outcome::failed("short") else {
            unreachable!()
        };
        assert_eq!(message, "short");
        let Outcome::Failed { message } = Outcome::failed(&"가".repeat(1_000)) else {
            unreachable!()
        };
        assert_eq!(message.chars().count(), MAX_MESSAGE_CHARS + 1);
        assert!(message.ends_with('…'));
    }

    /// A file that is not there is a first run, a byte order mark is
    /// tolerated, and what is written reads back the same.
    #[test]
    fn a_history_file_round_trips() {
        let directory = tempfile::tempdir().unwrap();
        let profile = Uuid::new_v4();
        let path = QueryHistory::path(directory.path(), profile);
        assert!(read(&path).unwrap().is_empty());

        let entries = vec![
            entry(1, "select 1"),
            HistoryEntry {
                outcome: Outcome::Rows {
                    count: 3,
                    more: true,
                },
                ..entry(2, "select * from t")
            },
            HistoryEntry {
                outcome: Outcome::failed("no such table"),
                ..entry(3, "select * from nope")
            },
        ];
        let pending = Mutex::new(HashMap::from([(profile, entries.clone())]));
        write_pending(directory.path(), &pending, &Mutex::new(()));
        assert_eq!(read(&path).unwrap(), entries);

        let mut bytes = b"\xEF\xBB\xBF".to_vec();
        bytes.extend(std::fs::read(&path).unwrap());
        std::fs::write(&path, bytes).unwrap();
        assert_eq!(read(&path).unwrap(), entries);
    }

    /// Recording, deleting and clearing go through the global, newest first,
    /// and land in the profile's file; an unreadable file is never written.
    #[gpui::test]
    fn the_store_records_deletes_and_clears(cx: &mut gpui::TestAppContext) {
        let directory = tempfile::tempdir().unwrap();
        let profile = Uuid::new_v4();
        let broken = Uuid::new_v4();
        std::fs::write(QueryHistory::path(directory.path(), broken), b"{ nope").unwrap();
        cx.update(|cx| {
            crate::app_settings::replace(rudbman_core::AppSettings::default(), cx);
            QueryHistory::use_directory(cx, directory.path());
            QueryHistory::record(
                cx,
                profile,
                "select 1",
                Duration::from_millis(3),
                Outcome::Done,
            );
            QueryHistory::record(
                cx,
                profile,
                "  select 2;  ",
                Duration::from_millis(4),
                Outcome::Done,
            );
            QueryHistory::record(cx, profile, "   ", Duration::ZERO, Outcome::Done);
            QueryHistory::record(cx, broken, "select 3", Duration::ZERO, Outcome::Done);
        });
        let entries = cx.update(|cx| QueryHistory::entries(cx, profile));
        assert_eq!(
            entries
                .iter()
                .map(|entry| entry.sql.as_str())
                .collect::<Vec<_>>(),
            ["select 2;", "select 1"],
            "newest first, trimmed, and nothing for a blank statement"
        );
        cx.executor().advance_clock(SAVE_DELAY * 2);
        cx.run_until_parked();
        let on_disk = read(&QueryHistory::path(directory.path(), profile)).unwrap();
        assert_eq!(on_disk.len(), 2);
        assert_eq!(
            std::fs::read(QueryHistory::path(directory.path(), broken)).unwrap(),
            b"{ nope",
            "an unreadable history was written over"
        );

        cx.update(|cx| QueryHistory::delete(cx, profile, entries[1].id));
        assert_eq!(cx.update(|cx| QueryHistory::entries(cx, profile)).len(), 1);
        cx.update(|cx| QueryHistory::clear(cx, profile));
        assert!(
            cx.update(|cx| QueryHistory::entries(cx, profile))
                .is_empty()
        );
        cx.executor().advance_clock(SAVE_DELAY * 2);
        cx.run_until_parked();
        assert!(
            read(&QueryHistory::path(directory.path(), profile))
                .unwrap()
                .is_empty()
        );
    }

    /// Off in the settings, nothing is written down.
    #[gpui::test]
    fn recording_can_be_turned_off(cx: &mut gpui::TestAppContext) {
        let profile = Uuid::new_v4();
        cx.update(|cx| {
            QueryHistory::init(cx);
            crate::app_settings::replace(
                rudbman_core::AppSettings {
                    query_history: false,
                    ..rudbman_core::AppSettings::default()
                },
                cx,
            );
            QueryHistory::record(cx, profile, "select 1", Duration::ZERO, Outcome::Done);
        });
        assert!(
            cx.update(|cx| QueryHistory::entries(cx, profile))
                .is_empty()
        );
    }
}
