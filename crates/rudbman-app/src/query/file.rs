//! The file a query pane's SQL was read from or last saved to.
//!
//! A pane is bound to at most one file, and only once its text and that file's
//! bytes are known to agree: after opening a file that read as UTF-8, or after
//! a save. From then on the tab is named after the file, "Save" writes back to
//! it without asking, and the tab wears a mark while the editor holds changes
//! the file does not.
//!
//! A binding also remembers what the file looked like on disk at that moment,
//! as a [`FileStamp`], and "Save" writes in place only while it still looks
//! that way. A file written since — by another program, or by another tab
//! bound to it: a split copy, or the same script open on a second connection —
//! is left alone and the save asks where instead, so that one buffer never
//! silently overwrites what another wrote.
//!
//! The prompt is the workspace's — it owns the tab names a new binding
//! replaces — so what is here is the binding itself, the write, and the two
//! halves of a save around it.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::*;

/// What a file looked like on disk: enough to tell that something has
/// written it since.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct FileStamp {
    len: u64,
    /// Seconds and nanoseconds since the Unix epoch, or `None` where the
    /// platform does not keep the time.
    modified: Option<(u64, u32)>,
}

impl FileStamp {
    fn of(metadata: &std::fs::Metadata) -> Self {
        let modified = metadata
            .modified()
            .ok()
            .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|since| (since.as_secs(), since.subsec_nanos()));
        Self {
            len: metadata.len(),
            modified,
        }
    }

    /// A stamp made up for a test, standing for a file nobody has to write.
    #[cfg(test)]
    pub(crate) fn of_parts(len: u64, modified: Option<(u64, u32)>) -> Self {
        Self { len, modified }
    }

    /// What the file at `path` looks like now.
    ///
    /// Asked of an opened file rather than of the path: on NFS an open is what
    /// revalidates the attributes, where a bare `stat` may answer from a cache
    /// that has not yet heard of another machine's write.
    fn read(path: &Path) -> std::io::Result<Self> {
        std::fs::File::open(path)?
            .metadata()
            .map(|metadata| Self::of(&metadata))
    }
}

/// How a save may treat a file already at its path.
pub(crate) enum Overwrite {
    /// The user picked the path in the save dialog, which asked about
    /// replacing a file there.
    Chosen,
    /// The pane's own file, replaced only while it still looks the way it did
    /// when the pane last read or wrote it — `None` when the pane cannot say.
    IfUnchanged(Option<FileStamp>),
}

/// How a write went.
pub(crate) enum SaveOutcome {
    /// Written, and what the file looks like now, when that could be read back.
    Written(Option<FileStamp>),
    /// Not written: the bound file is not the one the pane last saw.
    Changed,
    Failed(std::io::Error),
}

/// What a save came to, as far as the workspace is concerned.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Saved {
    /// Written to the file the pane was already bound to.
    InPlace,
    /// Written to a file the pane was not bound to, and is now.
    Rebound,
    /// Not written, because the bound file has changed; the workspace asks
    /// where to save instead.
    Changed,
    /// Not written, and the pane says why.
    Failed,
}

/// The text a save is writing, and the edit it was taken at.
pub(crate) struct SaveJob {
    /// The editor's whole text when the save began.
    pub text: String,
    /// [`QueryPane::edits`] when the text was taken. A save that comes back to
    /// an editor edited since leaves it dirty rather than marking a state the
    /// file does not hold as saved.
    pub edits: u64,
}

/// Reads a SQL file, and what it looked like on disk as it was read.
/// **Blocks.**
pub(crate) fn read_sql_file(path: &Path) -> std::io::Result<(Vec<u8>, Option<FileStamp>)> {
    let mut file = std::fs::File::open(path)?;
    let stamp = file
        .metadata()
        .ok()
        .map(|metadata| FileStamp::of(&metadata));
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)?;
    Ok((bytes, stamp))
}

/// Writes `text` to `path`, as `overwrite` allows. **Blocks.**
///
/// A file that has gone is written anew under either rule: there is nothing
/// of anyone's left there to overwrite. One that cannot be looked at counts as
/// changed — the save then asks, which costs a dialog rather than a file.
///
/// Written in place rather than through a temporary file and a rename, as the
/// application's other exports are: a rename would turn a symbolic link into a
/// plain file and drop the file's own permissions. The write is synced before
/// it counts: a network filesystem may report a full disk or a quota only
/// when the data is flushed, and a save that said "done" over a file it had
/// already truncated would be the worst way to find that out.
pub(crate) fn write_sql_file(path: &Path, text: &str, overwrite: Overwrite) -> SaveOutcome {
    if let Overwrite::IfUnchanged(expected) = overwrite {
        match FileStamp::read(path) {
            Ok(stamp) if Some(stamp) == expected => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            _ => return SaveOutcome::Changed,
        }
    }
    let written = std::fs::File::create(path).and_then(|mut file| {
        file.write_all(text.as_bytes())?;
        file.sync_all()?;
        file.metadata()
    });
    match written {
        Ok(metadata) => SaveOutcome::Written(Some(FileStamp::of(&metadata))),
        Err(error) => SaveOutcome::Failed(error),
    }
}

impl QueryPane {
    /// The file this pane's SQL was read from or last saved to.
    pub fn file(&self) -> Option<&Path> {
        self.file.as_deref()
    }

    /// What the bound file looked like when the pane last read or wrote it.
    pub(crate) fn file_stamp(&self) -> Option<FileStamp> {
        self.stamp
    }

    /// The name the bound file gives the tab: its file name, as it reads.
    pub fn file_title(&self) -> Option<SharedString> {
        let name = self.file.as_deref()?.file_name()?;
        Some(SharedString::from(name.to_string_lossy().into_owned()))
    }

    /// The line the pane is showing, for the shell's tests of a failed save.
    #[cfg(test)]
    pub(crate) fn notice(&self) -> Option<&SharedString> {
        self.notice.as_ref()
    }

    /// Whether the editor holds changes the bound file does not.
    ///
    /// Always `false` for a pane bound to no file: its draft is kept by the
    /// workspace, and there is nothing to have saved it to.
    pub fn is_modified(&self, cx: &App) -> bool {
        self.file.is_some() && (self.draft_unsaved || self.editor.read(cx).is_dirty())
    }

    /// Binds the pane to `path`, whose bytes the editor now holds and which
    /// looked like `stamp` as they were read.
    ///
    /// What opening a file ends in. The editor's clean point moves here too,
    /// so the mark appears with the first edit after it.
    pub(crate) fn bind_file(
        &mut self,
        path: PathBuf,
        stamp: Option<FileStamp>,
        cx: &mut Context<Self>,
    ) {
        self.file = Some(path);
        self.stamp = stamp;
        self.draft_unsaved = false;
        self.editor.update(cx, |editor, cx| editor.mark_clean(cx));
        cx.notify();
    }

    /// Binds a pane the workspace has just restored to the file it was bound
    /// to, saying whether the restored draft held changes that file did not
    /// and what the file looked like then.
    ///
    /// Restoring sets the editor's text, which leaves it clean, and the undo
    /// history that could walk back to the saved state did not survive the
    /// restart either — so an unsaved draft stays marked until the next save.
    pub(crate) fn restore_file(
        &mut self,
        path: PathBuf,
        modified: bool,
        stamp: Option<FileStamp>,
        cx: &mut Context<Self>,
    ) {
        self.file = Some(path);
        self.stamp = stamp;
        self.draft_unsaved = modified;
        cx.notify();
    }

    /// Holds saves back until [`QueryPane::reload_file`] has had the file:
    /// until then the editor shows a draft the file may no longer hold, or
    /// nothing at all.
    pub(crate) fn begin_reload(&mut self) {
        self.reloading = true;
    }

    /// Shows what the bound file holds now, in a pane restored with `restored`
    /// as its draft — one saved clean, which the file may have moved on from
    /// since, or one saved empty because it was too large to keep.
    ///
    /// Only over the restored draft itself: the read is a background task, and
    /// what was typed into the pane while it was out is the user's newer word.
    /// Such a pane stays bound and marked, but forgets what the file looked
    /// like, so its next save asks rather than writing over a file it never
    /// showed. So does one whose file could not be read, or had gone: a save
    /// then writes it anew if it is still gone, and asks if it has come back —
    /// moved back, or on a share mounted late — looking just as it did. Bytes
    /// that are no longer UTF-8 are shown the way opening them would show them
    /// and unbind the pane, so that a save never writes replacement characters
    /// over them.
    pub(crate) fn reload_file(
        &mut self,
        restored: &str,
        read: std::io::Result<(Vec<u8>, Option<FileStamp>)>,
        cx: &mut Context<Self>,
    ) {
        self.reloading = false;
        let (bytes, stamp) = match read {
            Ok(read) => read,
            Err(error) => {
                if error.kind() != std::io::ErrorKind::NotFound {
                    log::warn!("could not reread the bound SQL file: {error}");
                }
                self.stamp = None;
                return;
            }
        };
        let (text, exact) = decode_sql_file(bytes);
        if exact && text == restored {
            // Typing on top of it since is an edit against this very file.
            self.stamp = stamp;
        } else if self.editor_text(cx) != restored {
            self.stamp = None;
            self.draft_unsaved = true;
        } else {
            self.editor
                .update(cx, |editor, cx| editor.set_text(&text, cx));
            self.draft_unsaved = false;
            if exact {
                self.stamp = stamp;
            } else {
                self.file = None;
                self.stamp = None;
            }
        }
        cx.notify();
    }

    /// The text a save should write, or `None` — and a line saying so — while
    /// another save of this pane is still writing or a restored pane is still
    /// rereading its file.
    ///
    /// Refused rather than queued: two writes in flight at once could land in
    /// either order, and the older text would win half the time.
    pub(crate) fn begin_save(&mut self, cx: &mut Context<Self>) -> Option<SaveJob> {
        if self.saving || self.reloading {
            self.notice = Some(ts!("query.save_busy"));
            cx.notify();
            return None;
        }
        self.saving = true;
        Some(SaveJob {
            text: self.editor_text(cx),
            edits: self.edits,
        })
    }

    /// Records how a save to `path` went.
    ///
    /// A write binds the pane to `path` and marks the editor clean — unless
    /// something was typed while it was out, in which case the pane stays
    /// marked until a save that holds it all, rather than leaving the editor's
    /// clean point on a text the file no longer has. Any line the pane was
    /// showing is put away: the one most likely to be up is a failed save's,
    /// or the warning that a draft is too large to be kept across a restart,
    /// and a file is what that warning asks for.
    ///
    /// A failure says why and keeps the binding; if it was the bound file that
    /// failed, the pane is marked, because a write that broke off may have left
    /// that file holding neither text.
    pub(crate) fn finish_save(
        &mut self,
        path: PathBuf,
        edits: u64,
        outcome: SaveOutcome,
        cx: &mut Context<Self>,
    ) -> Saved {
        self.saving = false;
        let saved = match outcome {
            SaveOutcome::Written(stamp) => {
                let saved = if self.file.as_deref() == Some(path.as_path()) {
                    Saved::InPlace
                } else {
                    Saved::Rebound
                };
                self.file = Some(path);
                self.stamp = stamp;
                self.draft_unsaved = edits != self.edits;
                if !self.draft_unsaved {
                    self.editor.update(cx, |editor, cx| editor.mark_clean(cx));
                }
                self.notice = None;
                saved
            }
            SaveOutcome::Changed => {
                self.notice = Some(ts!("query.file_changed"));
                Saved::Changed
            }
            SaveOutcome::Failed(error) => {
                log::error!("could not write {}: {error}", path.display());
                if self.file.as_deref() == Some(path.as_path()) {
                    self.draft_unsaved = true;
                }
                self.notice = Some(ts!("query.save_failed", error = error.to_string()));
                Saved::Failed
            }
        };
        cx.notify();
        saved
    }
}

/// A SQL file's bytes as text, and whether they were UTF-8 to begin with.
///
/// Invalid UTF-8 is replaced rather than refused, for the reason
/// [`Workspace::load_sql_file`](crate::workspace) gives; the flag is what keeps
/// such a file from being bound, since writing the replaced text back would
/// destroy the bytes the editor could not show.
pub(crate) fn decode_sql_file(bytes: Vec<u8>) -> (String, bool) {
    match String::from_utf8(bytes) {
        Ok(text) => (text, true),
        Err(error) => (
            String::from_utf8_lossy(error.as_bytes()).into_owned(),
            false,
        ),
    }
}
