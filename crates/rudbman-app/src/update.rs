//! The update check, and the self-update it now leads to.
//!
//! Two halves that share one HTTPS client and one notion of what a release is.
//!
//! The **check** is one request against the project's `releases/latest`
//! endpoint. It runs once per launch from the background executor, and it runs
//! again — with a different filter — whenever the user picks "Check for
//! updates" from the menu. Its whole visible outcome is the
//! [`UpdateDialog`][crate::update_dialog::UpdateDialog] appearing.
//!
//! The **install** is what "Update" now does: fetch the release asset built for
//! this exact target triple, verify it against what the API said it should be,
//! unpack it beside the installed copy, and move the new one into the old one's
//! place. The application then restarts itself into the build it just wrote.
//!
//! # Why the start-up check fails silently
//!
//! A workbench is opened to get work done, and an update check is the least
//! important thing happening at start-up. Every way it can go wrong — no
//! network, a captive portal answering HTML, GitHub rate-limiting the address,
//! a tag someone pushed by hand in a shape the parser does not recognise — has
//! the same correct response: say nothing and carry on. So [`check`] ends every
//! failure path in a `log::debug!` and a `None`.
//!
//! A *manual* check is the opposite: the user asked a question and is owed an
//! answer, including "I could not reach GitHub". That is why [`check_now`]
//! answers with a three-way [`Check`] instead, and why the manual path also
//! ignores the "never mention this version again" tag — the user has just
//! overruled it by asking.
//!
//! # Why `ureq` and not gpui's HTTP client
//!
//! `cx.http_client()` is a `NullHttpClient` unless the application installs a
//! real one, and this binary does not. `ureq` is already here for the driver
//! downloader, so the whole cost of the two requests this module makes is the
//! `gzip` feature the GitHub API's compressed JSON needs.
//!
//! # Why the swap moves three things and not one
//!
//! rudbman is not a single file. The executable resolves `lib/rudbman-bridge.jar`
//! and the bundled `runtime/` JRE *relative to itself* — see the JVM loader in
//! `rudbman-jdbc` — so a new binary beside an old bridge JAR is a mismatch that
//! only shows up at the first connection attempt. The three entries are
//! therefore replaced together, and if any one of them cannot be moved the
//! ones already moved are put back: a half-swapped installation is worse than
//! no swap at all. On macOS all three live inside `rudbman.app`, so there the
//! bundle directory is the one entry and the problem does not arise.
//!
//! The Linux archive also carries `icons/`, the `.desktop` file and
//! `install.sh`. Those are deliberately *not* swapped: nothing resolves them
//! relative to the executable — `install.sh` copied them into
//! `~/.local/share/applications` and `~/.local/share/icons` at install time —
//! so replacing the copies inside the application directory would update files
//! nobody reads while leaving the installed ones alone. Desktop integration is
//! the installer's business, not the updater's.
//!
//! # The JVM is a one-way door
//!
//! `Jvm::start` (see `crate::connection`) loads the JVM into *this* process at
//! the first database connection, and JNI offers no way to unload it again: it
//! stays until the process exits. While it is up, Windows holds an open handle
//! on `lib/rudbman-bridge.jar` and on the loaded images under `runtime/`, and
//! renaming either of them fails with a sharing violation.
//!
//! The main path is unaffected — the start-up announcement arrives long before
//! any connection is opened, so the swap runs against a process that has never
//! touched Java. Updating from the menu *after* connecting is the case that
//! cannot rename anything, and rather than fail there, [`install`] parks the
//! unpacked payload beside the installation as [`PENDING_DIR`] and reports
//! [`Installed::Staged`]. The next launch finds it and performs the same renames
//! from [`apply_pending`], before a window, a settings load or a connection
//! exists — a moment at which nothing Java has been touched and the only locked
//! file is the running executable, which Windows *does* allow to be renamed. The
//! user sees one flow either way: the dialog finishes and rudbman comes back up
//! on the new build.
//!
//! Three decisions inside that are worth writing down.
//!
//! **The fallback is chosen up front, not after a failure.** The question is
//! `cfg!(windows) && Jvm::get().is_some()`, asked before the first rename — see
//! [`must_defer`]. Trying the swap and staging on failure sounds more general
//! and is worse: Windows reports a sharing violation and a permissions problem
//! alike as `ERROR_ACCESS_DENIED`, so an installation directory the user cannot
//! write would be staged, and staged again, forever, instead of saying so. A
//! question with a knowable answer is both deterministic and testable.
//!
//! **Only Windows, and only with a JVM up.** Elsewhere a rename over a running
//! image succeeds, so a swap that fails there failed for a reason the next
//! launch will not change — a system package, a read-only mount, a `.app` opened
//! from a disk image. Deferring those would trade today's honest error dialog,
//! which names the problem and offers the release page, for a silent success
//! that quietly does nothing on the next launch.
//!
//! **A staged update that cannot be applied is discarded, quietly.** If the
//! pending directory turns out to be incomplete, or the swap fails anyway,
//! [`apply_pending`] logs a warning, removes the directory and lets the
//! installed build start normally. This is the fallback's own fallback, reached
//! where there is no window to put a dialog in; what the user needs at start-up
//! is a working rudbman, and keeping the directory would only turn one failure
//! into a failure on every launch — or, worse, apply a stale payload as a
//! downgrade months later.
//!
//! # What the install deliberately does not do
//!
//! No package manager is consulted, no installer is run, nothing is written
//! outside the directory rudbman is already installed in, and nothing is
//! elevated. A copy the user cannot overwrite — a system package, a read-only
//! mount, a `.app` opened from a disk image — fails the rename and lands in the
//! dialog's error state, whose one action is the browser fallback this module
//! used to be limited to. That is the honest outcome: an updater that starts
//! asking for administrator rights is a different program.

use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

#[cfg(windows)]
use std::os::windows::process::CommandExt;

use gpui::App;
use rudbman_core::AppSettings;

use crate::app_settings;

/// Version of the running binary, taken from its `Cargo.toml`.
const CURRENT_VERSION: &str = env!("CARGO_PKG_VERSION");

/// The GitHub API endpoint answering with the most recent non-draft,
/// non-prerelease release of the project.
const LATEST_RELEASE_API: &str = "https://api.github.com/repos/xcomart/rudbman/releases/latest";

/// Where "Update" goes when the API answered without an `html_url`.
///
/// The releases index rather than the project page: whatever the user came here
/// for, it is a download.
const RELEASES_PAGE: &str = "https://github.com/xcomart/rudbman/releases";

/// How long the whole *check* may take, connection included.
///
/// Short on purpose. Nothing waits on this — the window is already up — but a
/// background task blocked for minutes on a black-holed connection is a thread
/// of the executor pool held hostage for no possible benefit, and an answer
/// that arrives long after start-up would open a dialog over whatever the user
/// had started doing in the meantime.
///
/// Emphatically *not* reused for the download: see [`CONNECT_TIMEOUT`].
const TIMEOUT: Duration = Duration::from_secs(5);

/// How long the *download* may take to reach the server.
///
/// A global timeout is wrong for a download — a release archive on a slow line
/// legitimately takes minutes, and rudbman's carries a whole JRE, so it is
/// measured in tens of megabytes rather than in one. Killing it at any fixed
/// deadline would make the updater useless exactly where it is most wanted.
/// What can still be bounded is the handshake, so an unreachable host fails
/// quickly instead of leaving the dialog spinning at 0%.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(20);

/// Ceiling on what the download will write to disk.
///
/// The size the API reported is checked afterwards, but only a reader that
/// stops can do the checking; without a limit a server answering an endless
/// body would fill the volume first. An order of magnitude above any release
/// this project has published — the bundled runtime is most of one — so it can
/// only ever catch a fault.
const MAX_ASSET_BYTES: u64 = 512 * 1024 * 1024;

/// Copy buffer for the download.
const DOWNLOAD_BUFFER: usize = 64 * 1024;

/// How many bytes must land before the download reports progress again.
///
/// The read loop turns over hundreds of times a second; a report per turn would
/// wake the UI thread for a bar that has not moved a pixel.
const PROGRESS_STEP: u64 = 256 * 1024;

/// Name of the scratch directory the download and the unpacking happen in.
///
/// Created *beside the installed copy* rather than in the system temp
/// directory, and that placement is load-bearing: the last step of an install
/// is a `fs::rename` of the unpacked payload onto the installed one, and a
/// rename cannot cross a volume. Staging in `%TEMP%` or `/tmp` would work on
/// most machines and fail with `EXDEV` on exactly the ones where the
/// application lives on another disk.
const STAGING_DIR: &str = ".update";

/// Where the unpacked archive goes inside [`STAGING_DIR`].
const UNPACKED_DIR: &str = "unpacked";

/// Name of the directory a deferred update waits in until the next launch.
///
/// A sibling of the installed copy for the same reason [`STAGING_DIR`] is — the
/// swap that eventually consumes it is a `fs::rename`, which cannot cross a
/// volume — but deliberately *not* inside it: [`install`] deletes the staging
/// directory on its way out, and a payload parked in there would go with it.
const PENDING_DIR: &str = ".update-pending";

/// Suffix a replaced entry is renamed to.
///
/// Windows will not let a running executable be deleted, but it will let it be
/// renamed, which is what makes an in-place swap possible at all. The leftovers
/// are removed by [`clean_leftovers`] on the next launch — one code path for all
/// three platforms, rather than an immediate unlink on unix and a deferred one
/// on Windows.
const OLD_SUFFIX: &str = ".old";

/// Fallback name for the downloaded archive.
///
/// Used when the asset name from the API is not a plain file name. It never is
/// in practice; the guard exists so a hostile response cannot steer the write
/// out of the staging directory.
const FALLBACK_ARCHIVE: &str = "rudbman-update";

/// `CREATE_NO_WINDOW`, so the `tar` this module shells out to does not flash a
/// console window over the progress dialog.
///
/// Spelled out rather than taken from the `windows` crate: that dependency is
/// scoped to `crate::caption` and pulling a whole feature of it in for one
/// constant would be the larger change.
#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// The release-asset target triple for the platform this binary was built for,
/// or `None` where the project publishes no build.
///
/// The three arms are exactly the three jobs of `.github/workflows/release.yml`.
/// An Intel Mac or an ARM Linux box runs a locally built rudbman, and there is
/// nothing to hand it: those fall through to `None`, which makes "Update" open
/// the release page the way it always did.
const TARGET: Option<&str> = if cfg!(all(target_os = "windows", target_arch = "x86_64")) {
    Some("x86_64-pc-windows-msvc")
} else if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
    Some("aarch64-apple-darwin")
} else if cfg!(all(target_os = "linux", target_arch = "x86_64")) {
    Some("x86_64-unknown-linux-gnu")
} else {
    None
};

/// What the archive holds that has to end up on disk, in install order.
///
/// The macOS archive carries one thing, the whole application bundle, and
/// everything rudbman loads at runtime is inside it. The Windows and Linux
/// archives carry the executable and the two directories it resolves relative
/// to itself; see the module docs for why all three move together.
///
/// The first entry is always the executable (or the bundle), because that is
/// the one whose *installed* name may differ from the published one — a binary
/// someone renamed still updates, and the directories beside it never are.
#[cfg(windows)]
const PAYLOAD: &[&str] = &["rudbman.exe", "lib", "runtime"];
/// See the Windows variant above.
#[cfg(target_os = "macos")]
const PAYLOAD: &[&str] = &["rudbman.app"];
/// See the Windows variant above.
#[cfg(all(unix, not(target_os = "macos")))]
const PAYLOAD: &[&str] = &["rudbman", "lib", "runtime"];

/// Where the executable sits inside the macOS bundle.
///
/// Needed only by [`apply_pending`], whose plan names the bundle and which has
/// to spawn something runnable out of it. Everywhere else the plan's first
/// target *is* the executable.
#[cfg(target_os = "macos")]
const BUNDLE_EXECUTABLE: &str = "Contents/MacOS/rudbman";

/// One thing the swap has to move: where it goes, and what goes there.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Entry {
    /// The installed path being replaced. It need not exist — a development
    /// tree has no `runtime/` beside the binary — in which case there is
    /// nothing to move aside and the new copy simply arrives.
    target: PathBuf,
    /// Name of the matching entry inside the unpacked payload directory.
    source: &'static str,
}

/// What one completed [`Entry`] left behind, so it can be undone.
#[derive(Debug)]
struct Done {
    /// Where the new copy now is.
    target: PathBuf,
    /// Where the displaced copy went, or `None` when there was nothing there.
    retired: Option<PathBuf>,
    /// Where in the payload directory the new copy came from, and where a
    /// rollback puts it back.
    source: PathBuf,
}

/// A downloadable build of a release, matched to this target triple.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Asset {
    /// File name as published, e.g.
    /// `rudbman-v0.2.0-x86_64-pc-windows-msvc.zip`.
    pub name: String,
    /// Direct download URL. Answers a redirect to storage, which `ureq`
    /// follows.
    pub url: String,
    /// Size in bytes, as the API reported it. Checked against what actually
    /// arrived, and used to drive the progress bar.
    pub size: u64,
    /// Lower-case hex SHA-256 of the asset, when the API supplied one.
    ///
    /// `digest` is a recent addition to the releases API, so an older GitHub
    /// Enterprise or a cached response may omit it; the size check still
    /// applies in that case.
    pub digest: Option<String>,
}

/// A release worth telling the user about.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Release {
    /// The git tag GitHub published it under, e.g. `"v0.2.0"`.
    ///
    /// Kept verbatim rather than normalised, because this is also what gets
    /// written to `settings.json` when the user ignores the version, and the two
    /// are compared as strings.
    pub tag: String,
    /// Human-readable version for display: [`Release::tag`] without its `v`.
    pub version: String,
    /// The release page to open in the browser.
    pub url: String,
    /// The build for this platform, when the release published one.
    ///
    /// `None` on a target the project does not ship — and on any release whose
    /// assets do not include the expected name — which is what decides whether
    /// "Update" installs or hands off to the browser.
    pub asset: Option<Asset>,
}

/// The answer to a check the user asked for.
///
/// Distinguishes the two outcomes the start-up check collapses into `None`:
/// "there is nothing newer" is a satisfying answer to a question, and "GitHub
/// could not be reached" is not the same thing at all.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Check {
    /// A newer release exists.
    Newer(Release),
    /// The running build is the latest one published.
    UpToDate,
    /// The check itself did not complete. Carries a short technical detail —
    /// untranslated on purpose, see [`install`].
    Failed(String),
}

/// How an [`install`] ended, both of which are successes.
///
/// The distinction is for the log and nothing else: the caller restarts either
/// way, and the user is told the same thing. See the module docs for when the
/// second one happens and why it is not an error.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Installed {
    /// The new build is in place. A restart comes up on it.
    Swapped,
    /// The new build is unpacked and waiting in [`PENDING_DIR`]. A restart
    /// applies it from [`apply_pending`] and re-executes once more, which is
    /// still one visible restart.
    Staged,
}

/// How far an [`install`] has got.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Progress {
    /// `done` of `total` bytes have been written to the staging directory.
    Downloading {
        /// Bytes received so far.
        done: u64,
        /// Bytes the API said the asset has. Zero when it said nothing.
        total: u64,
    },
    /// The download is complete; the archive is being unpacked and swapped in.
    Installing,
}

/// Ask GitHub whether a newer release exists, blocking until it answers.
///
/// **Call this from the background executor.** It performs a network request
/// and will block the calling thread for up to [`TIMEOUT`].
///
/// This is the *manual* check: it reports every outcome, and it knows nothing
/// about the ignore list. [`check`] is the start-up wrapper around it.
pub fn check_now() -> Check {
    let body = match fetch_latest() {
        Ok(body) => body,
        Err(err) => {
            log::debug!("update check: {err}");
            return Check::Failed(err.to_string());
        }
    };

    let Some(release) = parse_release(&body) else {
        return Check::Failed("GitHub answered with no readable release".to_string());
    };

    if is_newer(&release.tag, CURRENT_VERSION) {
        Check::Newer(release)
    } else {
        log::debug!(
            "update check: {} is not newer than {CURRENT_VERSION}",
            release.tag
        );
        Check::UpToDate
    }
}

/// The start-up check: answers `Some` only when there is something to say.
///
/// Answers `Some` only when all of the following hold, and `None` — silently —
/// otherwise:
///
/// * the request succeeded and the body parsed;
/// * the tag names a version strictly newer than the running one;
/// * that tag is not the one stored in `ignored`.
///
/// `ignored` is passed in rather than read from the settings global because the
/// global is only reachable from the UI thread, and this function runs off it.
///
/// Answers `None` immediately in a test build, and that guard lives here rather
/// than at the call site so the shell's start-up path stays one shape. gpui's
/// test executor runs background tasks inline whenever a test parks, so without
/// it every one of the dozens of `Workspace::new` calls in the suite would make
/// a real request to github.com — a test run that needs the network, and that
/// pays [`TIMEOUT`] per test without it. The manual [`check_now`] is untouched:
/// no test asks for one.
pub fn check(ignored: Option<&str>) -> Option<Release> {
    if cfg!(test) {
        return None;
    }
    match check_now() {
        Check::Newer(release) if ignored == Some(release.tag.as_str()) => {
            log::debug!("update check: {} is available but ignored", release.tag);
            None
        }
        Check::Newer(release) => Some(release),
        Check::UpToDate | Check::Failed(_) => None,
    }
}

/// The release page of `release`, or the releases index when it named none.
pub fn release_url(release: &Release) -> &str {
    if release.url.is_empty() {
        RELEASES_PAGE
    } else {
        &release.url
    }
}

/// Persist `tag` as the version the user never wants to hear about again.
///
/// Replaces the global and writes the file immediately, which is the one place
/// the shell departs from its usual "save when the last window closes" rule:
/// this is a decision the user just made in a dialog, not a window position
/// drifting under a drag, and it has to survive a crash the same way a saved
/// setting does.
///
/// A failed write is logged and otherwise ignored — the tag still applies for
/// the rest of this run, and the worst case is that the same dialog appears once
/// more on the next launch, which is not worth an error message over.
pub fn remember_ignored(tag: &str, cx: &mut App) {
    let mut settings: AppSettings = app_settings::current(cx);
    settings.ignored_update = Some(tag.to_string());
    app_settings::replace(settings, cx);
    app_settings::save(cx);
}

/// Remove what a previous update left behind, if anything.
///
/// **Call this from the background executor**, early in the run: removing a
/// `.app` bundle or a bundled JRE is a recursive delete of thousands of files,
/// and nothing on screen depends on it.
///
/// The swap cannot delete the copies it replaces — on Windows because one of
/// them is the running process, on the others because there is no reason to
/// make the three platforms differ — so it renames them aside and leaves them
/// for the next launch. That is here. Every failure is a debug line: a leftover
/// costs disk space and nothing else, and the next update will try again.
pub fn clean_leftovers() {
    let Ok(plan) = install_plan() else {
        return;
    };
    for entry in plan {
        let Some(retired) = old_path(&entry.target) else {
            continue;
        };
        if !retired.exists() {
            continue;
        }
        match remove(&retired) {
            Ok(()) => log::debug!("removed the previous version at {}", retired.display()),
            Err(error) => log::debug!(
                "could not remove the previous version at {}: {error}",
                retired.display()
            ),
        }
    }
}

/// Download `release`, unpack it, and put it where the running copy is.
///
/// **Call this from the background executor.** It downloads tens of megabytes,
/// spawns `tar`, and renames files; none of that belongs on the UI thread.
/// `report` is called as the work proceeds, from this thread.
///
/// Returns [`Installed::Swapped`] only once the new build is fully in place, so
/// the caller may restart into it immediately, and [`Installed::Staged`] when
/// the swap had to be left to the next launch — see the module docs. Both are
/// successes and both are followed by a restart. On failure the staging
/// directory is gone, the installed copy is as it was, and the `Err` carries a
/// sentence for the dialog to show under its translated "the update failed"
/// heading.
///
/// # Why the error text is not translated
///
/// It is a technical detail — a `tar` message, an OS error, a byte count that
/// did not match — produced on a thread that has no business reaching into the
/// locale state, and shown beneath a heading that *is* translated. Translating
/// the detail would mean a key per failure mode and a per-locale copy of every
/// `io::Error` string, which is not what any of them say anyway.
pub fn install(release: &Release, report: &mut dyn FnMut(Progress)) -> Result<Installed, String> {
    let Some(asset) = release.asset.as_ref() else {
        return Err(format!(
            "{} publishes no build for this platform",
            release.tag
        ));
    };

    let plan = install_plan()?;
    let parent = install_dir(&plan)
        .ok_or_else(|| "the installed copy has no parent directory".to_string())?
        .to_path_buf();

    let staging = parent.join(STAGING_DIR);
    // A staging directory left by an interrupted run would otherwise poison
    // this one with a half-written archive under the same name.
    let _ = remove(&staging);
    fs::create_dir_all(&staging)
        .map_err(|error| format!("could not write to {}: {error}", parent.display()))?;

    let outcome = stage(asset, &plan, &staging, &parent.join(PENDING_DIR), report);
    // Best-effort on purpose: the update either happened or it did not, and a
    // scratch directory that outlives it is not worth turning a success into a
    // failure over. The next install removes it anyway.
    let _ = remove(&staging);
    outcome
}

/// The download-verify-unpack-swap sequence, with `staging` already prepared.
///
/// Split out from [`install`] purely so the staging directory has exactly one
/// removal site covering every way out of it.
fn stage(
    asset: &Asset,
    plan: &[Entry],
    staging: &Path,
    pending: &Path,
    report: &mut dyn FnMut(Progress),
) -> Result<Installed, String> {
    let archive = staging.join(archive_name(&asset.name));
    download(asset, &archive, report)?;

    report(Progress::Installing);

    let unpacked = staging.join(UNPACKED_DIR);
    fs::create_dir_all(&unpacked)
        .map_err(|error| format!("could not create {}: {error}", unpacked.display()))?;
    extract(&archive, &unpacked)?;

    let payload = find_payload(&unpacked, PAYLOAD).ok_or_else(|| {
        format!(
            "{} does not contain {}",
            asset.name,
            PAYLOAD.join(" beside ")
        )
    })?;

    if must_defer() {
        defer(&payload, pending)?;
        log::info!(
            "the update is staged at {} and will be applied on the next launch",
            pending.display()
        );
        return Ok(Installed::Staged);
    }

    swap(plan, &payload)?;

    // With the new bundle in place and the restart imminent, this is the last
    // moment to make sure Gatekeeper will let it open.
    #[cfg(target_os = "macos")]
    if let Some(entry) = plan.first() {
        clear_quarantine(&entry.target);
    }

    Ok(Installed::Swapped)
}

/// Strip the quarantine flag from the bundle just swapped in, best-effort.
///
/// A file this process downloads and unpacks should carry no quarantine of its
/// own — rudbman is not quarantine-aware, and `tar` restores none from the
/// CI-built archive — but Gatekeeper's rules have tightened release by release,
/// and the one unacceptable outcome here is an update that leaves the user with
/// an app macOS refuses to reopen. So the flag is cleared unconditionally: this
/// is the same `xattr -r -d com.apple.quarantine` the README walks a first-time
/// installer through, recursive because the flag lands on every file inside a
/// quarantined bundle, and best-effort because the attribute is usually not
/// there at all — a failure costs a debug line, never the update.
#[cfg(target_os = "macos")]
fn clear_quarantine(bundle: &Path) {
    match Command::new("xattr")
        .args(["-r", "-d", "com.apple.quarantine"])
        .arg(bundle)
        .output()
    {
        Ok(output) if output.status.success() => {}
        // The usual answer on a clean bundle: "No such xattr". Worth a debug
        // line and nothing more.
        Ok(output) => log::debug!(
            "xattr -r -d com.apple.quarantine {} exited with {}: {}",
            bundle.display(),
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        ),
        Err(error) => log::debug!("xattr could not be run: {error}"),
    }
}

/// Whether the renames have to be left to the next launch.
///
/// One line, and untested on purpose: no test can start a JVM into the test
/// process and then ask this. See the module docs for why the question is asked
/// here rather than inferred from a failed rename.
fn must_defer() -> bool {
    cfg!(windows) && rudbman_jdbc::Jvm::get().is_some()
}

/// Park the unpacked `payload` at `pending`, for the next launch to apply.
///
/// One rename, of a directory that is still inside the staging tree, onto a
/// sibling of the installed copy — so it survives the staging directory being
/// deleted and stays on the volume the eventual swap has to rename within. It
/// works equally for the wrapper directory every published archive carries and
/// for the unpacked root itself, which is what a flat archive resolves to.
///
/// Any earlier pending directory goes first. It can only be one of two things:
/// a payload this launch already failed to apply, or one staged and then
/// superseded before a restart happened. Neither is worth keeping over the copy
/// that was just downloaded and verified.
fn defer(payload: &Path, pending: &Path) -> Result<(), String> {
    let _ = remove(pending);
    fs::rename(payload, pending).map_err(|error| {
        format!(
            "could not stage the update at {}: {error}",
            pending.display()
        )
    })
}

/// Apply an update a previous run staged, and re-execute into it.
///
/// **Call this first thing in `main`**, before the gpui application exists and
/// long before anything can load the JVM: the whole point is to do the renames
/// in a process that holds no handle on `lib/` or `runtime/`.
///
/// Answers `true` when the caller should return from `main` immediately — the
/// new build is in place and a fresh process carrying this one's arguments has
/// been spawned into it. Answers `false` for every other case, including all the
/// failures, which means "carry on starting up normally"; there is no pending
/// directory left either way, so the next launch is an ordinary one and this can
/// never loop.
pub fn apply_pending() -> bool {
    let Ok(plan) = install_plan() else {
        return false;
    };
    let Some(pending) = install_dir(&plan).map(|parent| parent.join(PENDING_DIR)) else {
        return false;
    };
    // The overwhelmingly common case, and the only one that costs anything at
    // start-up: one `stat` that says there is nothing to do.
    if !pending.is_dir() {
        return false;
    }

    if !apply(&plan, &pending) {
        return false;
    }

    let Some(exe) = relaunch_target(&plan) else {
        log::warn!("the staged update was applied but there is nothing to restart");
        return false;
    };

    match Command::new(&exe).args(std::env::args_os().skip(1)).spawn() {
        Ok(_) => true,
        Err(error) => {
            // Vanishingly unlikely — the file was just renamed into place — and
            // there is nothing better to do than carry on. The build on disk is
            // now wholly the new one, so the running image is the only stale
            // part, and it is replaced by the next launch.
            log::warn!(
                "could not restart into the update at {}: {error}",
                exe.display()
            );
            false
        }
    }
}

/// Verify `pending`, swap it in, and remove it whichever way that went.
///
/// Split from [`apply_pending`] so the part with the decisions in it takes its
/// paths as arguments: `current_exe` and `spawn` are not things a test can hold
/// still. Answers whether the installed copy is now the staged one.
///
/// The directory is removed on every path, and that is deliberate. A successful
/// swap renames the entries out of it and leaves only whatever else the archive
/// carried; a failed one leaves the whole payload. Keeping either would mean a
/// launch that failed once fails identically forever, and a payload left by a
/// version the user has since moved past would eventually be applied as a
/// downgrade.
fn apply(plan: &[Entry], pending: &Path) -> bool {
    // The plan's own source names rather than `PAYLOAD`: they are exactly what
    // the swap below will reach for, and a pending directory missing one of them
    // is a swap that fails halfway.
    let names: Vec<&str> = plan.iter().map(|entry| entry.source).collect();

    let applied = if holds_all(pending, &names) {
        match swap(plan, pending) {
            Ok(()) => {
                log::info!("applied the update staged at {}", pending.display());
                true
            }
            Err(error) => {
                log::warn!("could not apply the staged update: {error}");
                false
            }
        }
    } else {
        log::warn!(
            "the update staged at {} is incomplete and has been discarded",
            pending.display()
        );
        false
    };

    if let Err(error) = remove(pending) {
        log::warn!("could not remove {}: {error}", pending.display());
    }

    applied
}

/// The executable to start once a staged update has been applied.
///
/// Everywhere but macOS the plan's first target is the executable itself. On
/// macOS it is the bundle, so the path inside has to be rebuilt — correctness
/// for a case that in practice never arises, since only Windows ever stages.
fn relaunch_target(plan: &[Entry]) -> Option<PathBuf> {
    let target = plan.first()?.target.clone();

    #[cfg(target_os = "macos")]
    {
        Some(target.join(BUNDLE_EXECUTABLE))
    }

    #[cfg(not(target_os = "macos"))]
    {
        Some(target)
    }
}

/// The directory `plan` installs into: the parent of the entry it starts with.
fn install_dir(plan: &[Entry]) -> Option<&Path> {
    plan.first()?.target.parent()
}

/// Stream `asset` into `to`, checking it against what the API promised.
///
/// Uses an agent of its own rather than the check's: that one carries a global
/// five-second deadline, which would abort a release download on any connection
/// slower than a datacentre's. Here only the connect phase is bounded.
fn download(asset: &Asset, to: &Path, report: &mut dyn FnMut(Progress)) -> Result<(), String> {
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_connect(Some(CONNECT_TIMEOUT))
        .build()
        .into();

    let mut response = agent
        .get(&asset.url)
        .header("User-Agent", format!("rudbman/{CURRENT_VERSION}"))
        .header("Accept", "application/octet-stream")
        .call()
        .map_err(|error| format!("could not download {}: {error}", asset.name))?;

    let mut body = response
        .body_mut()
        .with_config()
        .limit(MAX_ASSET_BYTES)
        .reader();

    let mut file =
        File::create(to).map_err(|error| format!("could not create {}: {error}", to.display()))?;

    let mut digest = ring::digest::Context::new(&ring::digest::SHA256);
    let mut buffer = vec![0u8; DOWNLOAD_BUFFER];
    let mut done = 0u64;
    let mut reported = 0u64;

    loop {
        let read = body
            .read(&mut buffer)
            .map_err(|error| format!("could not download {}: {error}", asset.name))?;
        if read == 0 {
            break;
        }
        let chunk = &buffer[..read];
        digest.update(chunk);
        file.write_all(chunk)
            .map_err(|error| format!("could not write {}: {error}", to.display()))?;
        done = done.saturating_add(read as u64);
        if done - reported >= PROGRESS_STEP {
            reported = done;
            report(Progress::Downloading {
                done,
                total: asset.size,
            });
        }
    }

    file.flush()
        .map_err(|error| format!("could not write {}: {error}", to.display()))?;
    drop(file);

    report(Progress::Downloading {
        done,
        total: asset.size,
    });

    if asset.size != 0 && done != asset.size {
        return Err(format!(
            "{} is {done} bytes, but the release says {}",
            asset.name, asset.size
        ));
    }

    if let Some(expected) = &asset.digest {
        let actual = hex(digest.finish().as_ref());
        if &actual != expected {
            return Err(format!(
                "{} does not match its published checksum",
                asset.name
            ));
        }
    }

    Ok(())
}

/// Unpack `archive` into `into` using the system `tar`.
///
/// One extractor for three archive formats and three platforms, and no new
/// dependency: `tar` on macOS and Linux is bsdtar or GNU tar, both of which
/// autodetect gzip, and Windows has shipped bsdtar as `System32\tar.exe` since
/// 1803 — which also reads the `.zip` the Windows release is published as,
/// because libarchive sniffs the container rather than trusting the extension.
///
/// `CREATE_NO_WINDOW` on Windows because a GUI process starting a console
/// program flashes a black rectangle on screen otherwise, and here it would
/// flash over a progress dialog.
fn extract(archive: &Path, into: &Path) -> Result<(), String> {
    let mut command = Command::new("tar");
    command.arg("-xf").arg(archive).arg("-C").arg(into);
    #[cfg(windows)]
    command.creation_flags(CREATE_NO_WINDOW);

    let output = command
        .output()
        .map_err(|error| format!("could not run tar: {error}"))?;

    if !output.status.success() {
        let detail = String::from_utf8_lossy(&output.stderr);
        let detail = detail.trim();
        return Err(if detail.is_empty() {
            format!("tar could not unpack {}", archive.display())
        } else {
            format!("tar could not unpack {}: {detail}", archive.display())
        });
    }

    Ok(())
}

/// Move every entry of `plan` out of `payload` and into its installed place.
///
/// Each entry is two renames in the only order that leaves a working entry at
/// every intermediate point: the installed copy is renamed out of the way first
/// — the step Windows permits for a running image but a delete would not — and
/// the new one takes the freed name.
///
/// Across entries the sequence is a journal. Every entry that completed is
/// recorded, and the first one that fails undoes all of them in reverse, which
/// a rename can always do. That matters here in a way it does not for a
/// single-file program: an executable that arrived beside the bridge JAR it was
/// not built against would start and then fail at the first connection, with
/// nothing on screen to say why. Either all three move or none do.
fn swap(plan: &[Entry], payload: &Path) -> Result<(), String> {
    let mut done: Vec<Done> = Vec::new();

    for entry in plan {
        match swap_one(entry, payload) {
            Ok(step) => done.push(step),
            Err(error) => {
                return Err(match roll_back(done) {
                    None => error,
                    // The rollback itself failed, which means the directory is
                    // in a state no further attempt can reason about. Say so:
                    // the browser fallback is the only way out, and the user
                    // needs to know where the pieces are.
                    Some(detail) => format!("{error}; {detail}"),
                });
            }
        }
    }

    Ok(())
}

/// Move one entry into place, leaving the copy it displaced under
/// [`OLD_SUFFIX`].
///
/// A failure here is already undone: if the new copy could not be moved in, the
/// displaced one goes straight back, so this either completes or changes
/// nothing. The caller's journal only has to undo the entries that *succeeded*.
fn swap_one(entry: &Entry, payload: &Path) -> Result<Done, String> {
    let source = payload.join(entry.source);

    let retired = if entry.target.exists() {
        let retired = old_path(&entry.target)
            .ok_or_else(|| format!("{} has no file name", entry.target.display()))?;
        // A leftover from a previous update that start-up could not remove
        // would make the rename below fail on Windows, where renaming onto an
        // existing name is an error.
        let _ = remove(&retired);
        fs::rename(&entry.target, &retired)
            .map_err(|error| format!("could not move {} aside: {error}", entry.target.display()))?;
        Some(retired)
    } else {
        // Nothing installed under this name — a development tree with no
        // `runtime/` beside the binary, say. There is nothing to displace and
        // the new copy simply arrives.
        None
    };

    if let Err(error) = fs::rename(&source, &entry.target) {
        let mut message = format!("could not install {}: {error}", entry.target.display());
        if let Some(retired) = &retired
            && let Err(second) = fs::rename(retired, &entry.target)
        {
            message.push_str(&format!(
                "; the previous one is now at {} ({second})",
                retired.display()
            ));
        }
        return Err(message);
    }

    Ok(Done {
        target: entry.target.clone(),
        retired,
        source,
    })
}

/// Undo completed entries, newest first, and report what could not be undone.
///
/// `None` means the installation is exactly as it was before the swap started.
/// Each step frees the installed name — by moving the new copy back into the
/// payload directory, or failing that by deleting it, since the whole staging
/// tree is about to go anyway — and then puts the displaced copy back.
fn roll_back(done: Vec<Done>) -> Option<String> {
    let mut stuck: Vec<String> = Vec::new();

    for step in done.into_iter().rev() {
        let freed = fs::rename(&step.target, &step.source).is_ok() || remove(&step.target).is_ok();
        let Some(retired) = step.retired else {
            // Nothing was displaced, so freeing the name is the whole undo.
            if !freed {
                stuck.push(format!(
                    "{} could not be removed again",
                    step.target.display()
                ));
            }
            continue;
        };
        if !freed {
            stuck.push(format!(
                "{} is the new version and {} is the previous one",
                step.target.display(),
                retired.display()
            ));
            continue;
        }
        if fs::rename(&retired, &step.target).is_err() {
            stuck.push(format!(
                "the previous {} is now at {}",
                step.target.display(),
                retired.display()
            ));
        }
    }

    if stuck.is_empty() {
        None
    } else {
        Some(format!(
            "and the rollback was incomplete: {}",
            stuck.join("; ")
        ))
    }
}

/// Everything this run of rudbman would replace, in the order it replaces them.
///
/// On macOS that is the one bundle the executable lives inside. Everywhere else
/// it is the executable plus the two directories it resolves relative to
/// itself; see the module docs.
///
/// The macOS arm is the one that can refuse. A `cargo run` build, or a bare
/// binary someone copied out of a bundle, has no `.app` to swap and no sensible
/// thing to do with an archive that contains one, so it reports that rather than
/// scattering a bundle into whatever directory it happens to sit in.
///
/// `current_exe()` resolves symlinks, which is what makes the Linux layout work:
/// `install.sh` puts the tree in `~/.local/share/rudbman` and links
/// `~/.local/bin/rudbman` at it, and this answers the real directory rather
/// than the link's.
fn install_plan() -> Result<Vec<Entry>, String> {
    let exe = std::env::current_exe()
        .map_err(|error| format!("could not locate the running program: {error}"))?;

    #[cfg(target_os = "macos")]
    {
        let bundle = bundle_root(&exe)
            .ok_or_else(|| "rudbman is not running from an application bundle".to_string())?;
        Ok(vec![Entry {
            target: bundle,
            source: PAYLOAD[0],
        }])
    }

    #[cfg(not(target_os = "macos"))]
    {
        let parent = exe
            .parent()
            .ok_or_else(|| format!("{} has no parent directory", exe.display()))?
            .to_path_buf();
        // The executable keeps whatever name it is installed under; the
        // companions never differ from the published ones, because the loader
        // looks them up by name.
        let mut plan = vec![Entry {
            target: exe.clone(),
            source: PAYLOAD[0],
        }];
        plan.extend(PAYLOAD[1..].iter().map(|name| Entry {
            target: parent.join(name),
            source: name,
        }));
        Ok(plan)
    }
}

/// The `.app` directory `exe` lives inside, if any.
///
/// `current_exe()` in a bundle is `<name>.app/Contents/MacOS/rudbman`, but the
/// depth is not worth relying on: the ancestor chain is walked until a component
/// wears the `app` extension.
#[cfg(any(target_os = "macos", test))]
fn bundle_root(exe: &Path) -> Option<PathBuf> {
    exe.ancestors()
        .find(|path| {
            path.extension()
                .is_some_and(|extension| extension.eq_ignore_ascii_case("app"))
        })
        .map(Path::to_path_buf)
}

/// `path` with [`OLD_SUFFIX`] appended to its file name.
///
/// Appended to the whole name rather than swapped for the extension, so
/// `rudbman.exe` becomes `rudbman.exe.old` and not `rudbman.old`: the second
/// would collide with a directory listing's idea of a different program, and on
/// Windows it would stop being an executable.
fn old_path(path: &Path) -> Option<PathBuf> {
    let mut name = path.file_name()?.to_os_string();
    name.push(OLD_SUFFIX);
    Some(path.with_file_name(name))
}

/// The directory inside the unpacked archive that holds the whole payload.
///
/// Every published archive wraps its contents in one directory named after the
/// asset, so the payload is one level down — but an archive that ever stops
/// doing that should still install, hence the direct hit is tried first and the
/// immediate subdirectories after it. Nothing deeper: a match further down would
/// be a different tree that happens to share the names.
///
/// A directory qualifies only if it holds *every* name, which is what keeps the
/// Windows and Linux archives from matching on the executable alone and then
/// failing halfway through the swap.
fn find_payload(root: &Path, names: &[&str]) -> Option<PathBuf> {
    if holds_all(root, names) {
        return Some(root.to_path_buf());
    }

    let mut found: Vec<PathBuf> = fs::read_dir(root)
        .ok()?
        .flatten()
        .map(|entry| entry.path())
        .filter(|candidate| candidate.is_dir() && holds_all(candidate, names))
        .collect();
    // Sorted so a two-directory archive picks the same one on every filesystem,
    // rather than whatever order the directory happened to be read in.
    found.sort();
    found.into_iter().next()
}

/// Whether `dir` holds every one of `names`.
///
/// An empty `names` answers `false`: no directory is the payload of an archive
/// that carries nothing, and answering `true` would make the first directory
/// read win.
fn holds_all(dir: &Path, names: &[&str]) -> bool {
    !names.is_empty() && names.iter().all(|name| dir.join(name).exists())
}

/// A file name for the downloaded archive that cannot escape the staging
/// directory.
///
/// The published names are plain, so this returns them unchanged; a name
/// carrying a separator — which only a compromised or confused API could send —
/// is replaced wholesale rather than sanitised, because there is no correct
/// guess at what it was meant to be.
fn archive_name(asset: &str) -> &str {
    let plain = !asset.is_empty()
        && asset != "."
        && asset != ".."
        && !asset.contains('/')
        && !asset.contains('\\');
    if plain { asset } else { FALLBACK_ARCHIVE }
}

/// Delete `path`, whichever kind of thing it is.
fn remove(path: &Path) -> std::io::Result<()> {
    if path.is_dir() {
        fs::remove_dir_all(path)
    } else if path.exists() {
        fs::remove_file(path)
    } else {
        Ok(())
    }
}

/// Lower-case hex, for comparing against the API's `sha256:` field.
fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    bytes.iter().fold(String::new(), |mut out, byte| {
        // Writing to a `String` cannot fail.
        let _ = write!(out, "{byte:02x}");
        out
    })
}

/// Fetch the raw JSON body of the latest-release endpoint.
///
/// The `User-Agent` is not optional politeness: the GitHub API rejects requests
/// without one. `Accept` pins the response to the current API media type so a
/// future default cannot silently change the field names underneath the parser.
fn fetch_latest() -> Result<String, ureq::Error> {
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(TIMEOUT))
        .build()
        .into();

    agent
        .get(LATEST_RELEASE_API)
        .header("User-Agent", format!("rudbman/{CURRENT_VERSION}"))
        .header("Accept", "application/vnd.github+json")
        .call()?
        .body_mut()
        .read_to_string()
}

/// Pick the tag, the release page and this platform's asset out of a
/// latest-release response.
///
/// `None` when the body is not an object, or carries no usable `tag_name`; a
/// missing `html_url` is tolerated and leaves [`Release::url`] empty, because
/// [`release_url`] has a sensible destination for that case and a release with
/// no page is still worth announcing. A missing asset is tolerated for the same
/// reason, and means the same thing to the dialog: hand off to the browser.
fn parse_release(body: &str) -> Option<Release> {
    let value: serde_json::Value = match serde_json::from_str(body) {
        Ok(value) => value,
        Err(err) => {
            log::debug!("update check: unreadable response: {err}");
            return None;
        }
    };

    let tag = value.get("tag_name")?.as_str()?.trim();
    if tag.is_empty() {
        return None;
    }

    let url = value
        .get("html_url")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default();

    let asset = TARGET
        .map(|target| asset_name(tag, target))
        .and_then(|name| find_asset(&value, &name));

    Some(Release {
        tag: tag.to_string(),
        version: strip_v(tag).to_string(),
        url: url.to_string(),
        asset,
    })
}

/// The file name the release workflow publishes for `tag` on `target`.
///
/// Mirrors `.github/workflows/release.yml`; the two have to be changed together,
/// and a mismatch degrades to the browser fallback rather than to a wrong
/// download, because nothing in the response would match this name.
fn asset_name(tag: &str, target: &str) -> String {
    let extension = if target.contains("windows") {
        "zip"
    } else {
        "tar.gz"
    };
    format!("rudbman-{tag}-{target}.{extension}")
}

/// The `assets` entry called `name`, read into an [`Asset`].
///
/// An entry without a download URL is no asset at all, so it answers `None` and
/// the release announces itself without one.
fn find_asset(value: &serde_json::Value, name: &str) -> Option<Asset> {
    let entry = value
        .get("assets")?
        .as_array()?
        .iter()
        .find(|asset| asset.get("name").and_then(serde_json::Value::as_str) == Some(name))?;

    let url = entry
        .get("browser_download_url")
        .and_then(serde_json::Value::as_str)?;
    if url.is_empty() {
        return None;
    }

    Some(Asset {
        name: name.to_string(),
        url: url.to_string(),
        size: entry
            .get("size")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0),
        digest: entry
            .get("digest")
            .and_then(serde_json::Value::as_str)
            .and_then(parse_digest),
    })
}

/// Read the API's `digest` field, which is `"<algorithm>:<hex>"`.
///
/// Only SHA-256 is accepted, and only as exactly 64 hex digits. Anything else —
/// a future algorithm, a truncated value, a field that changed shape — answers
/// `None` and leaves the size check as the only verification, which is the
/// behaviour on the many responses that carry no digest at all.
fn parse_digest(raw: &str) -> Option<String> {
    let (algorithm, hex) = raw.trim().split_once(':')?;
    if !algorithm.eq_ignore_ascii_case("sha256") {
        return None;
    }
    if hex.len() != 64 || !hex.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return None;
    }
    Some(hex.to_ascii_lowercase())
}

/// Whether `latest` names a strictly newer version than `current`.
///
/// Both sides are read by [`parse_version`], and anything it cannot read
/// compares as *not* newer. That asymmetry is the point: the only consequence of
/// answering `false` is that a dialog does not appear, while answering `true` on
/// a tag nobody can interpret would nag the user about a release that may not
/// exist. A hand-pushed `nightly` tag, a release named after a branch, an API
/// answering something unexpected — all of them stay quiet.
fn is_newer(latest: &str, current: &str) -> bool {
    let (Some(latest), Some(current)) = (parse_version(latest), parse_version(current)) else {
        return false;
    };

    // Compared position by position rather than as vectors, so that a tag with
    // fewer components than the running version — `v1` against `0.1.2` — is read
    // as `1.0.0` and wins, instead of being cut short by the shorter length.
    let len = latest.len().max(current.len());
    for index in 0..len {
        let left = latest.get(index).copied().unwrap_or(0);
        let right = current.get(index).copied().unwrap_or(0);
        if left != right {
            return left > right;
        }
    }
    false
}

/// Split a version string into its numeric components.
///
/// Accepts the `v` prefix the project's tags carry, in either case, and nothing
/// else: every dot-separated component must be a plain non-negative integer.
/// Pre-release and build suffixes (`1.2.3-rc1`, `1.2.3+build`) are therefore
/// rejected rather than truncated — rudbman does not publish them, so a tag
/// wearing one is a surprise, and a surprise should not open a dialog.
fn parse_version(version: &str) -> Option<Vec<u64>> {
    let version = strip_v(version.trim());
    if version.is_empty() {
        return None;
    }
    version
        .split('.')
        .map(|part| part.parse::<u64>().ok())
        .collect()
}

/// Drop one leading `v` or `V`, if there is one.
fn strip_v(version: &str) -> &str {
    version
        .strip_prefix('v')
        .or_else(|| version.strip_prefix('V'))
        .unwrap_or(version)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_higher_component_anywhere_is_newer() {
        assert!(is_newer("0.1.3", "0.1.2"));
        assert!(is_newer("0.2.0", "0.1.2"));
        assert!(is_newer("1.0.0", "0.9.9"));
        assert!(is_newer("v0.1.3", "0.1.2"));
        assert!(is_newer("V0.1.3", "0.1.2"));
    }

    #[test]
    fn the_same_or_an_older_version_is_not_newer() {
        assert!(!is_newer("0.1.2", "0.1.2"));
        assert!(!is_newer("v0.1.2", "0.1.2"));
        assert!(!is_newer("0.1.1", "0.1.2"));
        assert!(!is_newer("0.0.9", "0.1.2"));
        assert!(!is_newer("0.1.2", "1.0.0"));
    }

    #[test]
    fn components_compare_numerically_and_not_as_text() {
        // The whole reason not to compare the strings: "10" sorts before "9".
        assert!(is_newer("0.10.0", "0.9.0"));
        assert!(!is_newer("0.9.0", "0.10.0"));
        assert!(is_newer("0.1.10", "0.1.9"));
    }

    #[test]
    fn a_missing_component_counts_as_zero() {
        assert!(!is_newer("0.1", "0.1.2"));
        assert!(!is_newer("0.1.2", "0.1.2.0"));
        assert!(!is_newer("0.1.2.0", "0.1.2"));
        assert!(is_newer("0.2", "0.1.2"));
        assert!(is_newer("1", "0.1.2"));
        assert!(is_newer("0.1.2.1", "0.1.2"));
    }

    #[test]
    fn an_unreadable_version_on_either_side_is_never_newer() {
        for tag in [
            "",
            "   ",
            "v",
            "nightly",
            "1.2.3-rc1",
            "1.2.3+build",
            "1..2",
            "1.2.",
            ".1.2",
            "1.-2",
            "0x10",
            "٩.٩",
            "99999999999999999999999",
        ] {
            assert!(!is_newer(tag, "0.1.2"), "{tag:?} must not read as newer");
            assert!(!is_newer("9.9.9", tag), "{tag:?} must not be compared to");
        }
    }

    #[test]
    fn the_shipped_version_is_one_this_module_can_read() {
        // A workspace version this parser cannot read would silence the check
        // permanently, and silently — exactly the failure this test exists to
        // notice the moment the version scheme changes.
        assert!(
            parse_version(CURRENT_VERSION).is_some(),
            "{CURRENT_VERSION} is not a version `parse_version` understands"
        );
        assert!(is_newer("999.0.0", CURRENT_VERSION));
        assert!(!is_newer(CURRENT_VERSION, CURRENT_VERSION));
    }

    #[test]
    fn a_release_response_yields_its_tag_and_page() {
        // Trimmed to the fields that matter; the real payload carries dozens
        // more, which is why the parser reaches for keys by name.
        let body = r#"{
            "tag_name": "v0.2.0",
            "name": "rudbman 0.2.0",
            "draft": false,
            "html_url": "https://github.com/xcomart/rudbman/releases/tag/v0.2.0",
            "assets": []
        }"#;
        let release = parse_release(body).expect("a well-formed release");
        assert_eq!(release.tag, "v0.2.0");
        assert_eq!(release.version, "0.2.0");
        assert_eq!(
            release.url,
            "https://github.com/xcomart/rudbman/releases/tag/v0.2.0"
        );
        assert_eq!(
            release_url(&release),
            "https://github.com/xcomart/rudbman/releases/tag/v0.2.0"
        );
        // No assets at all is the browser-fallback case on every platform.
        assert!(release.asset.is_none());
    }

    #[test]
    fn a_release_without_a_page_falls_back_to_the_releases_index() {
        let release = parse_release(r#"{"tag_name":"0.2.0"}"#).expect("a tag is enough");
        assert_eq!(release.tag, "0.2.0");
        assert_eq!(release.version, "0.2.0");
        assert!(release.url.is_empty());
        assert!(release.asset.is_none());
        assert_eq!(release_url(&release), RELEASES_PAGE);
    }

    #[test]
    fn a_response_without_a_usable_tag_is_no_release() {
        for body in [
            "",
            "not json at all",
            "<html>captive portal</html>",
            "null",
            "[]",
            r#"{"message":"API rate limit exceeded"}"#,
            r#"{"tag_name":null}"#,
            r#"{"tag_name":42}"#,
            r#"{"tag_name":""}"#,
            r#"{"tag_name":"   "}"#,
        ] {
            assert!(parse_release(body).is_none(), "{body:?} must yield nothing");
        }
    }

    #[test]
    fn a_surrounding_whitespace_only_differs_by_trimming() {
        let release = parse_release(r#"{"tag_name":"  v1.2.3  "}"#).expect("a padded tag");
        assert_eq!(release.tag, "v1.2.3");
        assert_eq!(release.version, "1.2.3");
    }

    #[test]
    fn an_asset_name_follows_the_release_workflow() {
        assert_eq!(
            asset_name("v0.2.0", "x86_64-pc-windows-msvc"),
            "rudbman-v0.2.0-x86_64-pc-windows-msvc.zip"
        );
        assert_eq!(
            asset_name("v0.2.0", "aarch64-apple-darwin"),
            "rudbman-v0.2.0-aarch64-apple-darwin.tar.gz"
        );
        assert_eq!(
            asset_name("v0.2.0", "x86_64-unknown-linux-gnu"),
            "rudbman-v0.2.0-x86_64-unknown-linux-gnu.tar.gz"
        );
    }

    /// A response carrying all three published assets, as the API shapes them.
    fn three_assets(tag: &str) -> String {
        let entries: Vec<String> = [
            "x86_64-pc-windows-msvc",
            "aarch64-apple-darwin",
            "x86_64-unknown-linux-gnu",
        ]
        .iter()
        .map(|target| {
            let name = asset_name(tag, target);
            format!(
                r#"{{"name":"{name}",
                    "size":1234,
                    "digest":"sha256:{hex}",
                    "browser_download_url":"https://example.invalid/{name}"}}"#,
                hex = "ab".repeat(32)
            )
        })
        .collect();
        format!(r#"{{"tag_name":"{tag}","assets":[{}]}}"#, entries.join(","))
    }

    #[test]
    fn the_asset_for_this_target_is_the_one_picked() {
        let release = parse_release(&three_assets("v9.9.9")).expect("a well-formed release");
        match TARGET {
            // The three targets the project publishes: exactly one entry of the
            // response is the right one, and it is chosen by name.
            Some(target) => {
                let asset = release.asset.expect("a build for a published target");
                assert_eq!(asset.name, asset_name("v9.9.9", target));
                assert!(asset.url.ends_with(&asset.name));
                assert_eq!(asset.size, 1234);
                assert_eq!(asset.digest.as_deref(), Some("ab".repeat(32).as_str()));
            }
            // Everything else — an Intel Mac, an ARM Linux box — has no build
            // to install and must fall back to the browser.
            None => assert!(release.asset.is_none()),
        }
    }

    #[test]
    fn an_asset_for_another_tag_is_not_this_release() {
        // The name carries the tag, so a response whose assets were built for a
        // different one matches nothing and degrades to the browser fallback.
        let body =
            three_assets("v9.9.9").replace("\"tag_name\":\"v9.9.9\"", "\"tag_name\":\"v8.8.8\"");
        let release = parse_release(&body).expect("a well-formed release");
        assert_eq!(release.tag, "v8.8.8");
        assert!(release.asset.is_none());
    }

    #[test]
    fn an_asset_without_a_download_url_is_no_asset() {
        let Some(target) = TARGET else { return };
        let name = asset_name("v9.9.9", target);
        for entry in [
            format!(r#"{{"name":"{name}","size":1}}"#),
            format!(r#"{{"name":"{name}","browser_download_url":""}}"#),
            format!(r#"{{"name":"{name}","browser_download_url":42}}"#),
        ] {
            let body = format!(r#"{{"tag_name":"v9.9.9","assets":[{entry}]}}"#);
            let release = parse_release(&body).expect("a well-formed release");
            assert!(release.asset.is_none(), "{entry} must not be usable");
        }
    }

    #[test]
    fn an_asset_may_arrive_without_a_size_or_a_digest() {
        let Some(target) = TARGET else { return };
        let name = asset_name("v9.9.9", target);
        let body = format!(
            r#"{{"tag_name":"v9.9.9","assets":[
                {{"name":"{name}","browser_download_url":"https://example.invalid/a"}}]}}"#
        );
        let asset = parse_release(&body)
            .and_then(|release| release.asset)
            .expect("a usable asset");
        // A zero size disables the byte-count check rather than failing it.
        assert_eq!(asset.size, 0);
        assert_eq!(asset.digest, None);
    }

    #[test]
    fn only_a_well_formed_sha256_digest_is_kept() {
        let sha = "ab".repeat(32);
        assert_eq!(parse_digest(&format!("sha256:{sha}")), Some(sha.clone()));
        assert_eq!(
            parse_digest(&format!("SHA256:{}", sha.to_uppercase())),
            Some(sha)
        );
        for raw in [
            "",
            "sha256",
            "sha256:",
            "sha512:{}",
            &format!("sha512:{}", "ab".repeat(32)),
            &format!("sha256:{}", "ab".repeat(31)),
            &format!("sha256:{}", "zz".repeat(32)),
        ] {
            assert_eq!(parse_digest(raw), None, "{raw:?} must not be accepted");
        }
    }

    #[test]
    fn a_digest_is_compared_as_lower_case_hex() {
        // The empty input's SHA-256, so the encoder is checked against a value
        // that is not of this codebase's making.
        let digest = ring::digest::digest(&ring::digest::SHA256, b"");
        assert_eq!(
            hex(digest.as_ref()),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }

    #[test]
    fn the_displaced_copy_keeps_its_whole_name() {
        for (path, expected) in [
            ("C:/Program Files/rudbman/rudbman.exe", "rudbman.exe.old"),
            ("/usr/local/share/rudbman/rudbman", "rudbman.old"),
            ("/usr/local/share/rudbman/runtime", "runtime.old"),
            ("/Applications/rudbman.app", "rudbman.app.old"),
        ] {
            let retired = old_path(Path::new(path)).expect("a path with a file name");
            assert_eq!(
                retired.file_name().and_then(|name| name.to_str()),
                Some(expected)
            );
            assert_eq!(retired.parent(), Path::new(path).parent());
        }
        assert_eq!(old_path(Path::new("/")), None);
    }

    #[test]
    fn a_bundle_is_found_however_deep_the_binary_sits() {
        assert_eq!(
            bundle_root(Path::new(
                "/Applications/rudbman.app/Contents/MacOS/rudbman"
            )),
            Some(PathBuf::from("/Applications/rudbman.app"))
        );
        // The extension is what identifies it, not the depth or the name.
        assert_eq!(
            bundle_root(Path::new("/tmp/x/Some Name.APP/Contents/MacOS/rudbman")),
            Some(PathBuf::from("/tmp/x/Some Name.APP"))
        );
        // A development build, and a binary copied out of its bundle: nothing
        // to swap, which is what makes the macOS install refuse.
        assert_eq!(
            bundle_root(Path::new("/work/rudbman/target/debug/rudbman")),
            None
        );
        assert_eq!(bundle_root(Path::new("/usr/local/bin/rudbman")), None);
    }

    #[test]
    fn an_archive_name_can_never_leave_the_staging_directory() {
        assert_eq!(
            archive_name("rudbman-v0.2.0-x86_64-pc-windows-msvc.zip"),
            "rudbman-v0.2.0-x86_64-pc-windows-msvc.zip"
        );
        for hostile in ["", ".", "..", "../evil", "a/b", "a\\b", "/etc/passwd"] {
            assert_eq!(archive_name(hostile), FALLBACK_ARCHIVE, "{hostile:?}");
        }
    }

    /// The three entries the non-macOS archives carry, as names.
    ///
    /// Spelled out rather than taken from [`PAYLOAD`] so that the tests below
    /// exercise the multi-entry shape on every platform, macOS included — the
    /// swap logic is the same code there and a one-entry `PAYLOAD` would leave
    /// the rollback untested on exactly the platform that cannot run it.
    const TRIPLE: [&str; 3] = ["rudbman", "lib", "runtime"];

    /// Builds a directory holding `names`, the file entries carrying `mark`.
    ///
    /// `lib` and `runtime` are made as directories with a file inside, because
    /// that is what they are on disk and because a rename of a directory is the
    /// operation the swap actually has to perform.
    fn tree(root: &Path, names: &[&str], mark: &str) {
        fs::create_dir_all(root).expect("a directory");
        for name in names {
            if *name == "lib" || *name == "runtime" {
                let dir = root.join(name);
                fs::create_dir_all(&dir).expect("a directory");
                fs::write(dir.join("payload.txt"), mark).expect("a file");
            } else {
                fs::write(root.join(name), mark).expect("a file");
            }
        }
    }

    /// What a file (or a directory's `payload.txt`) says.
    fn mark_of(root: &Path, name: &str) -> String {
        let path = root.join(name);
        let path = if path.is_dir() {
            path.join("payload.txt")
        } else {
            path
        };
        fs::read_to_string(path).expect("a readable entry")
    }

    /// A plan replacing every one of [`TRIPLE`] inside `install`.
    fn plan_over(install: &Path) -> Vec<Entry> {
        TRIPLE
            .iter()
            .map(|name| Entry {
                target: install.join(name),
                source: name,
            })
            .collect()
    }

    #[test]
    fn the_payload_is_found_at_the_root_or_one_level_down() {
        let root = tempfile::tempdir().expect("a temp directory");
        let root = root.path();

        // Nothing there yet.
        assert_eq!(find_payload(root, &TRIPLE), None);

        // The shape every published archive has: one wrapper directory.
        let wrapper = root.join("rudbman-v0.2.0-x86_64-unknown-linux-gnu");
        tree(&wrapper, &TRIPLE, "new");
        assert_eq!(find_payload(root, &TRIPLE), Some(wrapper.clone()));

        // A flat archive works too, and wins, because it is unambiguous.
        tree(root, &TRIPLE, "new");
        assert_eq!(find_payload(root, &TRIPLE), Some(root.to_path_buf()));

        // A directory counts as a payload: that is what the macOS bundle is,
        // and what `lib` and `runtime` are everywhere else.
        let bundles = tempfile::tempdir().expect("a temp directory");
        fs::create_dir_all(bundles.path().join("wrapper/rudbman.app/Contents")).expect("a bundle");
        assert_eq!(
            find_payload(bundles.path(), &["rudbman.app"]),
            Some(bundles.path().join("wrapper"))
        );
    }

    /// A wrapper that carries only part of the payload is not the payload.
    ///
    /// The case this guards is a Windows archive whose `runtime/` failed to
    /// pack: matching on the executable alone would move the new binary in and
    /// then fail on the directory, which is exactly the half-swapped state the
    /// rollback exists to prevent — better never to start.
    #[test]
    fn a_directory_missing_one_entry_is_not_the_payload() {
        let root = tempfile::tempdir().expect("a temp directory");
        let wrapper = root.path().join("rudbman-v0.2.0-x86_64-pc-windows-msvc");
        tree(&wrapper, &["rudbman", "lib"], "new");
        assert_eq!(find_payload(root.path(), &TRIPLE), None);
        // And with the last one in place it is.
        tree(&wrapper, &TRIPLE, "new");
        assert_eq!(find_payload(root.path(), &TRIPLE), Some(wrapper));
    }

    #[test]
    fn a_swap_replaces_every_entry_and_keeps_the_old_ones_aside() {
        let root = tempfile::tempdir().expect("a temp directory");
        let install = root.path().join("install");
        let payload = root.path().join("payload");
        tree(&install, &TRIPLE, "old");
        tree(&payload, &TRIPLE, "new");

        swap(&plan_over(&install), &payload).expect("every entry moves");

        for name in TRIPLE {
            assert_eq!(mark_of(&install, name), "new", "{name} was not replaced");
            let retired = format!("{name}{OLD_SUFFIX}");
            assert!(
                install.join(&retired).exists(),
                "{retired} should have been kept aside"
            );
            assert_eq!(mark_of(&install, &retired), "old");
        }
    }

    /// The whole reason the swap keeps a journal: one entry that cannot move
    /// must leave the installation exactly as it was.
    #[test]
    fn a_failed_entry_rolls_the_earlier_ones_back() {
        let root = tempfile::tempdir().expect("a temp directory");
        let install = root.path().join("install");
        let payload = root.path().join("payload");
        tree(&install, &TRIPLE, "old");
        // The payload is missing `runtime`, so the third rename fails after the
        // first two have already happened.
        tree(&payload, &["rudbman", "lib"], "new");

        let error = swap(&plan_over(&install), &payload).expect_err("the third entry cannot move");
        assert!(error.contains("runtime"), "{error}");
        // No mention of an incomplete rollback: this one had to succeed.
        assert!(!error.contains("rollback"), "{error}");

        for name in TRIPLE {
            assert_eq!(
                mark_of(&install, name),
                "old",
                "{name} should have been rolled back"
            );
            assert!(
                !install.join(format!("{name}{OLD_SUFFIX}")).exists(),
                "{name} should have no leftover after a rollback"
            );
        }
    }

    /// An entry with nothing installed under its name still installs, and a
    /// rollback removes it again rather than leaving half a tree.
    #[test]
    fn an_entry_with_nothing_to_displace_is_still_undone() {
        let root = tempfile::tempdir().expect("a temp directory");
        let install = root.path().join("install");
        let payload = root.path().join("payload");
        // A development tree: the binary is there, the two directories beside
        // it are not.
        tree(&install, &["rudbman"], "old");
        tree(&payload, &["rudbman", "lib"], "new");

        swap(&plan_over(&install), &payload).expect_err("`runtime` is missing from the payload");

        assert_eq!(mark_of(&install, "rudbman"), "old");
        assert!(
            !install.join("lib").exists(),
            "the freshly created lib/ should have been taken away again"
        );
        assert!(!install.join("rudbman.old").exists());
    }

    /// The staging half: the payload leaves the scratch tree in one rename.
    #[test]
    fn a_deferred_update_is_parked_beside_the_installation() {
        let root = tempfile::tempdir().expect("a temp directory");
        let staging = root.path().join(".update");
        let payload = staging.join("unpacked/rudbman-v0.2.0-x86_64-pc-windows-msvc");
        let pending = root.path().join(PENDING_DIR);
        tree(&payload, &TRIPLE, "new");

        defer(&payload, &pending).expect("the payload moves out of the staging tree");

        assert!(!payload.exists(), "the payload should have been moved");
        for name in TRIPLE {
            assert_eq!(mark_of(&pending, name), "new");
        }

        // The staging directory goes at the end of every install, and the
        // parked payload has to outlive it.
        remove(&staging).expect("the staging tree is removable");
        assert!(holds_all(&pending, &TRIPLE));

        // A payload staged and never applied is replaced, not merged into.
        let second = staging.join("unpacked");
        tree(&second, &["rudbman"], "newer");
        defer(&second, &pending).expect("the second payload takes the first one's place");
        assert_eq!(mark_of(&pending, "rudbman"), "newer");
        assert!(
            !pending.join("lib").exists(),
            "the superseded payload should be gone entirely"
        );
    }

    #[test]
    fn a_complete_staged_update_is_applied_and_then_cleared() {
        let root = tempfile::tempdir().expect("a temp directory");
        let install = root.path().join("install");
        let pending = install.join(PENDING_DIR);
        tree(&install, &TRIPLE, "old");
        tree(&pending, &TRIPLE, "new");

        assert!(apply(&plan_over(&install), &pending));

        for name in TRIPLE {
            assert_eq!(mark_of(&install, name), "new", "{name} was not replaced");
            assert_eq!(mark_of(&install, &format!("{name}{OLD_SUFFIX}")), "old");
        }
        assert!(
            !pending.exists(),
            "the pending directory must not survive its own application"
        );
    }

    /// An archive that unpacked badly, or a payload someone pruned: the swap
    /// must not start, and the directory must not be left to fail again on
    /// every launch from here on.
    #[test]
    fn an_incomplete_staged_update_is_discarded_without_touching_anything() {
        let root = tempfile::tempdir().expect("a temp directory");
        let install = root.path().join("install");
        let pending = install.join(PENDING_DIR);
        tree(&install, &TRIPLE, "old");
        tree(&pending, &["rudbman", "lib"], "new");

        assert!(!apply(&plan_over(&install), &pending));

        for name in TRIPLE {
            assert_eq!(mark_of(&install, name), "old", "{name} should be untouched");
            assert!(!install.join(format!("{name}{OLD_SUFFIX}")).exists());
        }
        assert!(!pending.exists(), "the pending directory should be gone");
    }

    #[test]
    fn a_staged_update_that_cannot_be_swapped_rolls_back_and_is_still_cleared() {
        let root = tempfile::tempdir().expect("a temp directory");
        let install = root.path().join("install");
        let pending = install.join(PENDING_DIR);
        tree(&install, &TRIPLE, "old");
        // A fourth entry whose target sits in a directory that does not exist,
        // so the pending directory passes the completeness check and the rename
        // fails anyway — after the first three have already happened.
        tree(&pending, &["rudbman", "lib", "runtime", "extra"], "new");
        let mut plan = plan_over(&install);
        plan.push(Entry {
            target: install.join("nowhere").join("extra"),
            source: "extra",
        });

        assert!(!apply(&plan, &pending));

        for name in TRIPLE {
            assert_eq!(
                mark_of(&install, name),
                "old",
                "{name} should have been rolled back"
            );
            assert!(!install.join(format!("{name}{OLD_SUFFIX}")).exists());
        }
        assert!(
            !pending.exists(),
            "a failed application must clear the directory too, or it fails forever"
        );
    }

    #[test]
    fn the_relaunch_target_is_something_that_can_be_executed() {
        let plan = vec![Entry {
            target: PathBuf::from(if cfg!(target_os = "macos") {
                "/Applications/rudbman.app"
            } else {
                "/opt/rudbman/rudbman"
            }),
            source: PAYLOAD[0],
        }];
        let exe = relaunch_target(&plan).expect("a plan with a first entry");
        #[cfg(target_os = "macos")]
        assert_eq!(
            exe,
            PathBuf::from("/Applications/rudbman.app/Contents/MacOS/rudbman")
        );
        #[cfg(not(target_os = "macos"))]
        assert_eq!(exe, PathBuf::from("/opt/rudbman/rudbman"));
        assert_eq!(relaunch_target(&[]), None);
    }

    #[test]
    fn a_release_with_no_asset_cannot_be_installed() {
        // The one `install` failure reachable without touching the network or
        // the filesystem, and the one that must never be a panic: it is what
        // an unpublished target reaches if the dialog ever routes it here.
        let release = Release {
            tag: "v9.9.9".to_string(),
            version: "9.9.9".to_string(),
            url: String::new(),
            asset: None,
        };
        let mut seen = Vec::new();
        let error = install(&release, &mut |progress| seen.push(progress))
            .expect_err("no asset, no install");
        assert!(error.contains("v9.9.9"), "{error}");
        assert!(seen.is_empty(), "nothing should have been reported");
    }
}
