//! The explorer filters each connection profile has used:
//! `explorer-filters/<profile-uuid>.json`.
//!
//! What the history button beside the filter box lists. A filter is written
//! down when it was *used* — `Enter` in the box, the conditions panel closed,
//! the tab left for another, an entry picked from the list — rather than on
//! every keystroke, which would fill the list with the prefixes of one word.
//!
//! # One file per profile, kept in a global
//!
//! Keyed by the *profile*, as the query history and the diagram layouts are:
//! a [`ConnectionId`](crate::explorer::ConnectionId) lives for one run, and
//! two tabs on one profile are two views of one database, which should offer
//! the same filters. The entries live in a gpui global, read from the file the
//! first time a profile is asked about and written a little after each change,
//! off the UI thread, in the order the writes were queued — the query
//! history's scheme, and for its reasons. Whatever is still owed when the
//! application quits is written then.
//!
//! A file that cannot be read is left as it is and never written over, so a
//! history that started empty because of it cannot replace the real one.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use gpui::{App, BorrowAppContext, Global, Task};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::explorer_filter::FilterSnapshot;

/// Filters kept per profile, the oldest dropped first.
pub const MAX_ENTRIES: usize = 20;

/// How long after a change the file is written: a burst of changes is one
/// write.
const SAVE_DELAY: Duration = Duration::from_millis(500);

/// The version written into each file.
const CURRENT_VERSION: u32 = 1;

/// The directory under the configuration directory the files go in.
const DIRECTORY_NAME: &str = "explorer-filters";

/// What a file holds.
#[derive(Default, Serialize, Deserialize)]
struct HistoryFile {
    #[serde(default)]
    version: u32,
    /// Newest first.
    #[serde(default)]
    entries: Vec<FilterSnapshot>,
}

/// Every profile's filter history that has been asked about, and the writes
/// still owed to disk.
pub struct FilterHistory {
    /// Where the files go; `None` writes nothing, which is what the tests run
    /// under unless they name a directory of their own.
    directory: Option<PathBuf>,
    /// Each profile's entries, newest first, once read.
    profiles: HashMap<Uuid, Vec<FilterSnapshot>>,
    /// Profiles whose file could not be read, and which are therefore never
    /// written.
    blocked: HashSet<Uuid>,
    /// Profiles changed since the last write was queued.
    dirty: HashSet<Uuid>,
    /// Snapshots waiting for the writer.
    pending: Arc<Mutex<HashMap<Uuid, Vec<FilterSnapshot>>>>,
    /// Keeps two writes in the order their snapshots were queued.
    writer: Arc<Mutex<()>>,
    /// The delayed write, while one is scheduled.
    save: Option<Task<()>>,
}

impl Global for FilterHistory {}

impl FilterHistory {
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
    /// Headless tests never read or write the user's files: under `cfg(test)`
    /// the store starts with no directory, and a test that wants files names
    /// one with [`FilterHistory::use_directory`].
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

    fn path(directory: &Path, profile: Uuid) -> PathBuf {
        directory.join(format!("{profile}.json"))
    }

    /// `profile`'s filters, newest first.
    pub fn entries(cx: &mut App, profile: Uuid) -> Vec<FilterSnapshot> {
        Self::init(cx);
        cx.update_global::<Self, _>(|history, _| history.loaded(profile).clone())
    }

    /// Puts `snapshot` at the top of `profile`'s list — moving it there when
    /// it is already in it — unless it lets everything through.
    pub fn record(cx: &mut App, profile: Uuid, snapshot: FilterSnapshot) {
        if snapshot.is_empty() {
            return;
        }
        Self::init(cx);
        cx.update_global::<Self, _>(|history, cx| {
            let entries = history.loaded(profile);
            if entries.first() == Some(&snapshot) {
                return;
            }
            entries.retain(|entry| *entry != snapshot);
            entries.insert(0, snapshot);
            entries.truncate(MAX_ENTRIES);
            history.changed(profile, cx);
        });
    }

    /// Deletes `profile`'s whole list.
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
    fn loaded(&mut self, profile: Uuid) -> &mut Vec<FilterSnapshot> {
        if !self.profiles.contains_key(&profile) {
            let entries = match &self.directory {
                Some(directory) => match read(&Self::path(directory, profile)) {
                    Ok(entries) => entries,
                    Err(error) => {
                        log::error!("could not read the explorer filters of {profile}: {error:#}");
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

/// Reads one profile's file; a missing one is an empty history.
fn read(path: &Path) -> anyhow::Result<Vec<FilterSnapshot>> {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error.into()),
    };
    let file: HistoryFile = serde_json::from_slice(rudbman_core::paths::strip_bom(&bytes))?;
    let mut entries = file.entries;
    entries.truncate(MAX_ENTRIES);
    Ok(entries)
}

/// Writes every queued snapshot, in the order the writer lock is taken.
fn write_pending(
    directory: &Path,
    pending: &Mutex<HashMap<Uuid, Vec<FilterSnapshot>>>,
    writer: &Mutex<()>,
) {
    let _writing = writer.lock();
    let queued = std::mem::take(&mut *pending.lock());
    for (profile, entries) in queued {
        let path = FilterHistory::path(directory, profile);
        let file = HistoryFile {
            version: CURRENT_VERSION,
            entries,
        };
        let result = serde_json::to_vec_pretty(&file)
            .map_err(anyhow::Error::from)
            .and_then(|bytes| rudbman_core::paths::write_atomic(&path, &bytes));
        if let Err(error) = result {
            log::error!(
                "could not save the explorer filters {}: {error:#}",
                path.display()
            );
            // Retried with the next write, unless a newer snapshot was queued
            // meanwhile, which that write carries instead.
            pending.lock().entry(profile).or_insert(file.entries);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn named(text: &str) -> FilterSnapshot {
        FilterSnapshot {
            text: text.to_string(),
            ..FilterSnapshot::default()
        }
    }

    #[gpui::test]
    fn a_used_filter_goes_to_the_top_once_and_the_list_stays_short(cx: &mut gpui::TestAppContext) {
        let profile = Uuid::new_v4();
        let other = Uuid::new_v4();
        cx.update(|cx| {
            FilterHistory::record(cx, profile, named("cust"));
            FilterHistory::record(cx, profile, named("*_seq"));
            FilterHistory::record(cx, profile, named("cust"));
            // A filter that lets everything through is not worth an entry.
            FilterHistory::record(cx, profile, FilterSnapshot::default());
            FilterHistory::record(cx, profile, named(" , ! "));
            assert_eq!(
                FilterHistory::entries(cx, profile),
                vec![named("cust"), named("*_seq")]
            );
            // Another profile's list is its own.
            assert!(FilterHistory::entries(cx, other).is_empty());

            for index in 0..MAX_ENTRIES + 5 {
                FilterHistory::record(cx, profile, named(&format!("t{index}")));
            }
            let entries = FilterHistory::entries(cx, profile);
            assert_eq!(entries.len(), MAX_ENTRIES);
            assert_eq!(entries[0], named(&format!("t{}", MAX_ENTRIES + 4)));

            FilterHistory::clear(cx, profile);
            assert!(FilterHistory::entries(cx, profile).is_empty());
        });
    }

    #[gpui::test]
    fn a_history_is_written_and_read_back(cx: &mut gpui::TestAppContext) {
        let directory = tempfile::tempdir().expect("a temporary directory");
        let profile = Uuid::new_v4();
        cx.update(|cx| {
            FilterHistory::use_directory(cx, directory.path());
            FilterHistory::record(cx, profile, named("orders"));
            cx.update_global::<FilterHistory, _>(|history, _| history.flush());
        });
        let path = FilterHistory::path(directory.path(), profile);
        assert_eq!(read(&path).expect("readable"), vec![named("orders")]);

        // Read again by a store that starts cold, as the next run does.
        cx.update(|cx| {
            FilterHistory::use_directory(cx, directory.path());
            assert_eq!(FilterHistory::entries(cx, profile), vec![named("orders")]);
        });
    }

    #[gpui::test]
    fn an_unreadable_file_is_never_written_over(cx: &mut gpui::TestAppContext) {
        let directory = tempfile::tempdir().expect("a temporary directory");
        let profile = Uuid::new_v4();
        let path = FilterHistory::path(directory.path(), profile);
        std::fs::write(&path, b"{ not json").expect("written");
        cx.update(|cx| {
            FilterHistory::use_directory(cx, directory.path());
            FilterHistory::record(cx, profile, named("orders"));
            cx.update_global::<FilterHistory, _>(|history, _| history.flush());
        });
        assert_eq!(std::fs::read(&path).expect("still there"), b"{ not json");
    }
}
