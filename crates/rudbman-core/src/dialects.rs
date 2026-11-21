//! The dialect profiles of `<config>/dialects`: loading them, and keeping the
//! ones the application ships there.
//!
//! `docs/dialect-profiles.md` fixes the rules — §7 for loading, §12 for the
//! shipped files. A profile is read by [`crate::dialect_profile`], resolved by
//! `rudbman_sql::dialect::profile`, and registered so that
//! [`Dialect::from_id`] finds it; what every file came to is kept as a
//! [`Report`] for the driver manager to show.

use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

use anyhow::{Context, Result};
use rudbman_sql::Dialect;
use rudbman_sql::dialect::profile::{self, ProfileSpec};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::DriverDef;
use crate::dialect_profile;
use crate::paths::{dialects_dir, strip_bom, write_atomic};

/// A profile file's extension.
const PROFILE_EXTENSION: &str = "yml";

/// What a script beside a profile is, when the profile names none.
const SCRIPT_EXTENSION: &str = "groovy";

/// Appended to a shipped file's name for its newer version, written beside a
/// file the user has changed.
const PENDING_SUFFIX: &str = ".new";

/// The record of the shipped files, in the profiles' directory.
const RECORD_FILE_NAME: &str = "shipped.json";

/// The scripts the user has allowed, in the profiles' directory (§6).
const ALLOWED_FILE_NAME: &str = "allowed.json";

/// The Groovy runtime a profile's script runs on (§5.3), as Maven Central
/// names it. It is not bundled: the application downloads it into the drivers
/// directory the first time a session needs it. `bridge/build.gradle` tests
/// the bridge against the same version.
pub const GROOVY_RUNTIME: &str = "org.apache.groovy:groovy:5.0.8";

// ------------------------------------------------------------------ loading

/// What one profile file came to.
#[derive(Debug, Clone)]
pub struct Entry {
    /// The profile's id, its file stem.
    pub id: String,
    /// The file.
    pub path: PathBuf,
    /// The name the file gives, once it reads.
    pub name: Option<String>,
    /// The base the file names, once it reads.
    pub base: Option<String>,
    /// Whether it loaded.
    pub state: State,
    /// The driver definition the profile brings, if it has a `driver`
    /// section.
    pub driver: Option<DriverDef>,
    /// The script `bridge.script` names, as written.
    pub script: Option<String>,
    /// The script whose hooks the profile's sessions run, once it has
    /// loaded: the one its own `bridge.script` names or, failing that, the
    /// nearest base profile's — and whether the user has allowed it.
    pub hooks: Option<Script>,
    /// Newer shipped versions of its files, written beside the user's own
    /// (`<file>.new`).
    pub pending: Vec<PathBuf>,
    /// Its own `bridge` half, as JSON, before its base's is merged under it.
    bridge: serde_json::Value,
    /// What a session opened with the profile hands the bridge, once it has
    /// loaded: its id, the built-in its chain of bases ends in, and its
    /// `bridge` half merged along that chain. See the bridge's `Profiles`.
    pub session: Option<serde_json::Value>,
}

/// A profile's script (§5), as it stands in the profiles' directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Script {
    /// Its name in the directory, as `bridge.script` gives it.
    pub name: String,
    /// The file.
    pub path: PathBuf,
    /// The SHA-256 of what the file holds, in lower-case hex: what
    /// `allowed.json` records of an allowed script.
    pub sha256: String,
    /// Whether sessions run it: the user has allowed this version of it, or
    /// the application wrote it as it is (§6, §12). Not allowed, the profile
    /// loads without it.
    pub allowed: bool,
}

impl Entry {
    /// The dialect, when the profile loaded.
    pub fn dialect(&self) -> Option<Dialect> {
        match self.state {
            State::Loaded(dialect) => Some(dialect),
            State::Failed(_) => None,
        }
    }

    /// Why the profile did not load, when it did not.
    pub fn failure(&self) -> Option<&str> {
        match &self.state {
            State::Loaded(_) => None,
            State::Failed(message) => Some(message),
        }
    }

    /// What the file is called in a list: its name, or its id until it reads.
    pub fn label(&self) -> &str {
        self.name.as_deref().unwrap_or(&self.id)
    }
}

/// Whether a profile loaded.
#[derive(Debug, Clone)]
pub enum State {
    /// Loaded, and registered under its id.
    Loaded(Dialect),
    /// Not loaded, for this reason. Its drivers get generic SQL until it is
    /// fixed.
    Failed(String),
}

/// What loading `<config>/dialects` came to.
#[derive(Debug, Clone, Default)]
pub struct Report {
    /// The directory read, when there is one.
    pub dir: Option<PathBuf>,
    /// Every profile file, by id.
    pub entries: Vec<Entry>,
    /// What could not be read or kept apart from any one profile: the
    /// directory itself, or the shipped files.
    pub problems: Vec<String>,
}

impl Report {
    /// Where among [`Self::entries`] the profile a driver's `dialect` names
    /// is: by its id or, once loaded, by an alias. ASCII case is ignored, as
    /// [`Dialect::from_id`] ignores it.
    pub fn find(&self, dialect: &str) -> Option<usize> {
        let dialect = dialect.trim();
        if let Some(at) = self
            .entries
            .iter()
            .position(|entry| entry.id.eq_ignore_ascii_case(dialect))
        {
            return Some(at);
        }
        let found = Dialect::find(dialect)?;
        self.entries
            .iter()
            .position(|entry| entry.dialect() == Some(found))
    }

    /// The profile a driver's `dialect` names; see [`Self::find`].
    pub fn entry_for(&self, dialect: &str) -> Option<&Entry> {
        self.find(dialect).map(|at| &self.entries[at])
    }

    /// The driver definitions the loaded profiles bring.
    pub fn drivers(&self) -> Vec<DriverDef> {
        self.entries
            .iter()
            .filter(|entry| entry.dialect().is_some())
            .filter_map(|entry| entry.driver.clone())
            .collect()
    }

    /// The dialects of the profiles that loaded, in file order.
    pub fn loaded(&self) -> Vec<Dialect> {
        self.entries.iter().filter_map(Entry::dialect).collect()
    }

    /// What a session whose driver names `dialect` opens with, when that is
    /// a profile that loaded.
    pub fn session_profile(&self, dialect: &str) -> Option<serde_json::Value> {
        self.entry_for(dialect)?.session.clone()
    }

    /// The loaded profiles that name products by their product name
    /// (`bridge.detect`), for a session whose driver names no profile.
    pub fn detect_profiles(&self) -> Vec<serde_json::Value> {
        self.entries
            .iter()
            .filter_map(|entry| entry.session.clone())
            .filter(|session| {
                session["bridge"]["detect"]
                    .as_array()
                    .is_some_and(|words| !words.is_empty())
            })
            .collect()
    }

    /// Reads the script of every loaded profile, and puts it in the
    /// profile's [`Entry::session`] where the user has allowed it.
    ///
    /// A script that cannot be read fails its profile: it is part of what
    /// the profile says, and its hooks silently gone would be settings
    /// silently lost.
    fn read_scripts(&mut self, dir: &Path, allowed: &Allowed) {
        for entry in &mut self.entries {
            let Some(name) = entry
                .session
                .as_ref()
                .and_then(|session| session["bridge"]["script"].as_str())
                .map(str::to_string)
            else {
                continue;
            };
            let path = dir.join(&name);
            let text = fs::read(&path)
                .map_err(|error| error.to_string())
                .and_then(|bytes| {
                    let sha256 = hash(&bytes);
                    String::from_utf8(strip_bom(&bytes).to_vec())
                        .map(|text| (sha256, text))
                        .map_err(|_| "the file is not UTF-8".to_string())
                });
            match text {
                Err(error) => {
                    entry.state = State::Failed(format!("bridge.script: {name}: {error}"));
                    entry.session = None;
                }
                Ok((sha256, text)) => {
                    let ok = allowed.scripts.get(&name) == Some(&sha256);
                    if ok && let Some(session) = entry.session.as_mut() {
                        session["script"] = serde_json::json!({
                            "path": path.display().to_string(),
                            "text": text,
                        });
                    }
                    entry.hooks = Some(Script {
                        name,
                        path,
                        sha256,
                        allowed: ok,
                    });
                }
            }
        }
    }

    /// Fills in [`Entry::session`] of every loaded profile.
    fn merge_sessions(&mut self) {
        for at in 0..self.entries.len() {
            if self.entries[at].dialect().is_none() {
                continue;
            }
            // Down the chain of bases, which `resolve` found free of cycles
            // and ending in a built-in, collecting each profile's half.
            let mut halves = vec![self.entries[at].bridge.clone()];
            let mut base = self.entries[at].base.clone().unwrap_or_default();
            let mut steps = 0;
            let builtin = loop {
                if let Some(found) = Dialect::find(&base)
                    && Dialect::ALL.contains(&found)
                {
                    break Some(found);
                }
                let Some(next) = self.find(&base).filter(|_| steps < self.entries.len()) else {
                    break None;
                };
                steps += 1;
                halves.push(self.entries[next].bridge.clone());
                base = self.entries[next].base.clone().unwrap_or_default();
            };
            let Some(builtin) = builtin else {
                continue;
            };
            let bridge = halves
                .iter()
                .rev()
                .fold(serde_json::json!({}), |under, over| {
                    dialect_profile::bridge::merged(&under, over)
                });
            self.entries[at].session = Some(serde_json::json!({
                "id": self.entries[at].id,
                "base": builtin.name(),
                "bridge": bridge,
            }));
        }
    }
}

/// Reads every profile in `dir` and resolves them together.
///
/// Nothing is registered: see [`reload`]. A directory that does not exist
/// holds no profiles; one that cannot be read is a problem of the report.
pub fn load_from(dir: &Path) -> Report {
    let mut report = Report {
        dir: Some(dir.to_path_buf()),
        ..Report::default()
    };
    let files = match profile_files(dir) {
        Ok(files) => files,
        Err(error) => {
            if error.kind() != io::ErrorKind::NotFound {
                report.problems.push(format!("{}: {error}", dir.display()));
            }
            return report;
        }
    };

    // Each read profile's place among the entries, with its spec.
    let mut read: Vec<(usize, String, ProfileSpec)> = Vec::new();
    for (id, path) in files {
        let mut entry = Entry {
            id: id.clone(),
            path: path.clone(),
            name: None,
            base: None,
            state: State::Failed(String::new()),
            driver: None,
            script: None,
            hooks: None,
            pending: Vec::new(),
            bridge: serde_json::json!({}),
            session: None,
        };
        match fs::read(&path) {
            Err(error) => entry.state = State::Failed(error.to_string()),
            Ok(bytes) => match String::from_utf8(strip_bom(&bytes).to_vec()) {
                Err(_) => entry.state = State::Failed("the file is not UTF-8".to_string()),
                Ok(text) => match dialect_profile::parse(&id, &text) {
                    Err(error) => entry.state = State::Failed(error.message),
                    Ok(file) => {
                        entry.name = Some(file.spec.name.clone());
                        entry.base = Some(file.spec.base.clone());
                        entry.script = file.script().map(str::to_string);
                        if let Some(bridge) = &file.bridge {
                            entry.bridge = serde_json::to_value(bridge)
                                .unwrap_or_else(|_| serde_json::json!({}));
                        }
                        entry.driver = file
                            .driver
                            .as_ref()
                            .map(|driver| driver.definition(&id, &file.spec.name));
                        read.push((report.entries.len(), id.clone(), file.spec));
                    }
                },
            },
        }
        entry.pending = pending_beside(dir, &entry);
        report.entries.push(entry);
    }

    let unread: Vec<String> = report
        .entries
        .iter()
        .filter(|entry| entry.name.is_none())
        .map(|entry| entry.id.clone())
        .collect();
    let places: Vec<usize> = read.iter().map(|(at, _, _)| *at).collect();
    let resolved = profile::resolve(read.into_iter().map(|(_, id, spec)| (id, spec)).collect());
    for (at, (_, outcome)) in places.into_iter().zip(resolved) {
        let entry = &mut report.entries[at];
        entry.state = match outcome {
            Ok(dialect) => State::Loaded(dialect),
            Err(error) => {
                let base = entry.base.as_deref().unwrap_or_default();
                // To resolve, a base that did not read is no profile at all;
                // to the person reading the list, it is the other failure.
                let message = if error.key.as_deref() == Some("base")
                    && unread.iter().any(|id| id.eq_ignore_ascii_case(base))
                {
                    format!("`{base}` did not load")
                } else {
                    match error.key {
                        Some(key) => format!("{key}: {}", error.message),
                        None => error.message,
                    }
                };
                State::Failed(message)
            }
        };
    }
    report.merge_sessions();
    let allowed = match read_allowed(dir) {
        Ok(allowed) => allowed,
        Err(error) => {
            // Nothing is allowed by a list that cannot be read.
            report.problems.push(format!("{error:#}"));
            Allowed::default()
        }
    };
    report.read_scripts(dir, &allowed);
    report
}

/// Whether a session's profile, as [`Report::session_profile`] or
/// [`Report::detect_profiles`] gives it, runs a script: what makes the
/// session need the Groovy runtime.
pub fn runs_script(session: &serde_json::Value) -> bool {
    session
        .get("script")
        .is_some_and(|script| !script.is_null())
}

// ------------------------------------------------------------------- trust

/// `allowed.json`: each script the user has allowed, by its name in the
/// directory, with the SHA-256 of the version allowed (§6).
#[derive(Debug, Default, Serialize, Deserialize)]
struct Allowed {
    #[serde(default)]
    scripts: BTreeMap<String, String>,
}

/// `dir`'s `allowed.json`; none allows nothing.
fn read_allowed(dir: &Path) -> Result<Allowed> {
    let path = dir.join(ALLOWED_FILE_NAME);
    match read_optional(&path)? {
        None => Ok(Allowed::default()),
        Some(bytes) => serde_json::from_slice(strip_bom(&bytes))
            .with_context(|| format!("failed to parse {}", path.display())),
    }
}

fn write_allowed(dir: &Path, allowed: &Allowed) -> Result<()> {
    let json = serde_json::to_vec_pretty(allowed).context("failed to serialize the allow list")?;
    write_atomic(&dir.join(ALLOWED_FILE_NAME), &json)
}

/// Allows `script` as it was read — by the hash it had then, so that a file
/// changed since is still not run — in the profiles' directory `dir`.
///
/// # Errors
///
/// Fails when `allowed.json` cannot be read or written.
pub fn allow_in(dir: &Path, script: &Script) -> Result<()> {
    let mut allowed = read_allowed(dir)?;
    allowed
        .scripts
        .insert(script.name.clone(), script.sha256.clone());
    write_allowed(dir, &allowed)
}

/// Takes the script `name` off the allow list of `dir`: sessions open
/// without it until it is allowed again.
///
/// # Errors
///
/// Fails when `allowed.json` cannot be read or written.
pub fn disallow_in(dir: &Path, name: &str) -> Result<()> {
    let mut allowed = read_allowed(dir)?;
    if allowed.scripts.remove(name).is_some() {
        write_allowed(dir, &allowed)?;
    }
    Ok(())
}
/// The `*.yml` files of `dir` by id, in id order.
fn profile_files(dir: &Path) -> io::Result<Vec<(String, PathBuf)>> {
    let mut files = Vec::new();
    for entry in fs::read_dir(dir)? {
        let path = entry?.path();
        if path.extension().and_then(|ext| ext.to_str()) != Some(PROFILE_EXTENSION) {
            continue;
        }
        let Some(id) = path.file_stem().and_then(|stem| stem.to_str()) else {
            continue;
        };
        if path.is_file() {
            files.push((id.to_string(), path.clone()));
        }
    }
    files.sort();
    Ok(files)
}

/// The `.new` files beside `entry`'s profile and its script.
fn pending_beside(dir: &Path, entry: &Entry) -> Vec<PathBuf> {
    let script = entry
        .script
        .clone()
        .unwrap_or_else(|| format!("{}.{SCRIPT_EXTENSION}", entry.id));
    let mut pending = Vec::new();
    for file in [entry.path.clone(), dir.join(script)] {
        let candidate = pending_path(&file);
        if candidate.is_file() && !pending.contains(&candidate) {
            pending.push(candidate);
        }
    }
    pending
}

/// `<file>.new`.
fn pending_path(file: &Path) -> PathBuf {
    let mut name = file.as_os_str().to_os_string();
    name.push(PENDING_SUFFIX);
    PathBuf::from(name)
}

/// What the last [`reload`] came to, and the directory as it stood then.
static CURRENT: RwLock<Option<(Arc<Report>, Listing)>> = RwLock::new(None);

/// Every file of the profiles' directory with its size and time: what tells a
/// [`reload`] that nothing changed since the last.
type Listing = Vec<(PathBuf, u64, Option<std::time::SystemTime>)>;

/// The directory's [`Listing`], empty where it cannot be read.
fn listing(dir: &Path) -> Listing {
    let mut listing: Listing = fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|entry| {
            let meta = entry.metadata().ok()?;
            Some((entry.path(), meta.len(), meta.modified().ok()))
        })
        .collect();
    listing.sort();
    listing
}

/// What the last [`reload`] came to; an empty report before the first.
pub fn current() -> Arc<Report> {
    CURRENT
        .read()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .as_ref()
        .map(|(report, _)| report.clone())
        .unwrap_or_default()
}

/// Loads `<config>/dialects` and registers what loaded, in place of what was
/// registered before — unless no file of the directory has changed since the
/// last load, when that load's report stands (a reload leaks what it
/// registers; see `profile::install`).
///
/// The problems of the last [`start_up`]'s sync are kept: they are about the
/// directory, not about this read of it.
pub fn reload() -> Arc<Report> {
    let dir = dialects_dir();
    let now = dir.as_deref().map(listing).unwrap_or_default();
    if let Some((report, then)) = CURRENT
        .read()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .as_ref()
        && *then == now
    {
        return report.clone();
    }
    let mut report = match dir {
        Ok(dir) => load_from(&dir),
        Err(error) => Report {
            problems: vec![format!("{error:#}")],
            ..Report::default()
        },
    };
    report.problems.extend(
        SYNC_PROBLEMS
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .iter()
            .cloned(),
    );
    profile::install(report.loaded());
    for entry in &report.entries {
        if let Some(message) = entry.failure() {
            log::warn!("dialect profile {} did not load: {message}", entry.id);
        }
    }
    let report = Arc::new(report);
    *CURRENT
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some((report.clone(), now));
    report
}

/// What [`start_up`]'s sync could not do, kept for every later [`reload`].
static SYNC_PROBLEMS: RwLock<Vec<String>> = RwLock::new(Vec::new());

/// Brings the shipped profiles into `<config>/dialects` and loads the
/// directory: what the application does once, before anything asks for a
/// dialect.
pub fn start_up() -> Arc<Report> {
    let problem = dialects_dir().and_then(|dir| sync_shipped(&dir, SHIPPED));
    let problems = match problem {
        Ok(synced) => {
            for name in &synced.written {
                log::info!("dialect profile file {name} written");
            }
            for name in &synced.replaced {
                log::info!("dialect profile file {name} updated");
            }
            for name in &synced.beside {
                log::info!(
                    "dialect profile file {name} changed by its user; its update is beside it"
                );
            }
            Vec::new()
        }
        Err(error) => {
            log::warn!("shipped dialect profiles not kept: {error:#}");
            vec![format!("{error:#}")]
        }
    };
    *SYNC_PROBLEMS
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = problems;
    *CURRENT
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = None;
    reload()
}

// ----------------------------------------------------------- shipped files

/// A file the application ships into `<config>/dialects`.
#[derive(Debug, Clone, Copy)]
pub struct Shipped {
    /// Its name in the directory, `<id>.yml` or a script beside one.
    pub name: &'static str,
    /// What the application writes.
    pub contents: &'static [u8],
}

/// The files of the repository's `dialects/` folder, compiled in by the build
/// script.
pub const SHIPPED: &[Shipped] = include!(concat!(env!("OUT_DIR"), "/shipped_dialects.rs"));

/// `shipped.json`: for each shipped file, the hash of the version last
/// offered to the directory.
#[derive(Debug, Default, Serialize, Deserialize)]
struct Record {
    #[serde(default)]
    files: BTreeMap<String, String>,
}

/// What [`sync_shipped`] did.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Synced {
    /// Files written where there was none.
    pub written: Vec<String>,
    /// Files the user had not changed, replaced by a newer version.
    pub replaced: Vec<String>,
    /// Files the user has changed, whose newer version was written beside
    /// them as `<file>.new`.
    pub beside: Vec<String>,
}

/// Brings `shipped` into `dir` by the rules of §12.
///
/// For each file, `shipped.json` holds the hash of the version last offered:
///
/// 1. A file never offered is written. If the directory already holds a file
///    of that name — the user's own — it is left alone and the shipped one is
///    written beside it.
/// 2. A file offered and since deleted stays deleted.
/// 3. A file as it was offered is replaced by a newer version.
/// 4. A file the user has changed is left as it is, and the newer version is
///    written beside it as `<file>.new`.
///
/// A `.new` is removed once the file beside it is the shipped version or the
/// `.new` itself — the user has taken it.
///
/// A script the application writes is allowed by its hash as it is written
/// (§12): one written where there was none, and the newer version of one the
/// user had allowed as it was. A script the user has turned off stays off, and
/// one the user has changed is the user's, which asks (§6).
///
/// # Errors
///
/// Fails when the directory cannot be read or written, or `shipped.json` or
/// `allowed.json` cannot be read; nothing is decided from a record that
/// could not be read.
pub fn sync_shipped(dir: &Path, shipped: &[Shipped]) -> Result<Synced> {
    let record_path = dir.join(RECORD_FILE_NAME);
    let mut record: Record = match read_optional(&record_path)? {
        None => Record::default(),
        Some(bytes) => serde_json::from_slice(strip_bom(&bytes))
            .with_context(|| format!("failed to parse {}", record_path.display()))?,
    };
    let mut allowed = read_allowed(dir)?;
    let mut synced = Synced::default();
    let mut changed = false;
    let mut allow_changed = false;
    for file in shipped {
        let path = dir.join(file.name);
        let ours = hash(file.contents);
        let mut on_disk = read_optional(&path)?;
        let theirs = on_disk.as_deref().map(hash);
        let offered = record.files.get(file.name);
        let name = file.name.to_string();
        if offered == Some(&ours) {
            // Nothing newer than what the directory was offered.
        } else {
            match (offered, theirs) {
                (_, Some(theirs)) if theirs == ours => {}
                (None, None) => {
                    write_atomic(&path, file.contents)?;
                    on_disk = Some(file.contents.to_vec());
                    if is_script(file.name) {
                        allowed.scripts.insert(name.clone(), ours.clone());
                        allow_changed = true;
                    }
                    synced.written.push(name);
                }
                (Some(_), None) => {}
                (Some(offered), Some(theirs)) if theirs == *offered => {
                    write_atomic(&path, file.contents)?;
                    on_disk = Some(file.contents.to_vec());
                    if is_script(file.name) && allowed.scripts.get(&name) == Some(offered) {
                        allowed.scripts.insert(name.clone(), ours.clone());
                        allow_changed = true;
                    }
                    synced.replaced.push(name);
                }
                (_, Some(_)) => {
                    write_atomic(&pending_path(&path), file.contents)?;
                    synced.beside.push(name);
                }
            }
            record.files.insert(file.name.to_string(), ours.clone());
            changed = true;
        }
        if let Some(on_disk) = on_disk {
            tidy_pending(&path, &on_disk, file.contents)?;
        }
    }
    if allow_changed {
        write_allowed(dir, &allowed)?;
    }
    if changed {
        let json = serde_json::to_vec_pretty(&record).context("failed to serialize the record")?;
        write_atomic(&record_path, &json)?;
    }
    Ok(synced)
}

/// Whether a shipped file is a script rather than a profile.
fn is_script(name: &str) -> bool {
    !name.ends_with(&format!(".{PROFILE_EXTENSION}"))
}

/// Removes `<path>.new` once `path` holds the shipped version or the `.new`
/// itself.
fn tidy_pending(path: &Path, on_disk: &[u8], shipped: &[u8]) -> Result<()> {
    let pending = pending_path(path);
    let Some(waiting) = read_optional(&pending)? else {
        return Ok(());
    };
    if on_disk == shipped || on_disk == waiting.as_slice() {
        fs::remove_file(&pending)
            .with_context(|| format!("failed to remove {}", pending.display()))?;
    }
    Ok(())
}

/// The SHA-256 of `bytes`, in lower-case hex.
fn hash(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// A file's contents, or `None` when there is no file.
fn read_optional(path: &Path) -> Result<Option<Vec<u8>>> {
    match fs::read(path) {
        Ok(data) => Ok(Some(data)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error).with_context(|| format!("failed to read {}", path.display())),
    }
}

/// `dialect` written as a profile file with every key filled in — the
/// starting point for a profile of a product like it.
pub fn export(dialect: Dialect) -> String {
    dialect_profile::to_yaml(&ProfileSpec::of(dialect))
}

#[cfg(test)]
mod tests;
