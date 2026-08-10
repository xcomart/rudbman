# Progress and handoff

This document exists so that another session — or another person — can pick
the work up. The design and the contracts all live in
[architecture.md](architecture.md); what is kept here is only **how far the
work has come, what is left, and how work is done in this repository**. It is
updated whenever a milestone ends.

Last updated: 2026-08-06 (just after the first release, **v0.1.0**).

## Where things stand

| Milestone | State | What went in |
|---|---|---|
| M0 | done | Workspace shell, gpui 0.2.2 vendored (six logman patches, kept byte-identical), 16 rudbman-ui widgets, theme/editor-theme/settings/i18n in eight languages, an icon of its own |
| M1 | done | The bridge JAR (a single JNI entry point, `Bridge.call`, an error envelope, Gson merging), the JVM bootstrap (-Xrs, a dedicated thread, no DestroyJavaVM), the session worker, the driver manager (Maven download, class auto-detection), the connection dialog, SSH tunnels (russh, a bastion without a PTY, loopback binds), OS-keychain secret storage, URL/property masking |
| M2 | done | The explorer tree (multiple roots, level-skipping rules), DESCRIBE for every kind, the four-tab table detail (columns, keys, references, DDL), DDL reconstruction (native and metadata, in that order) |
| M3 | done | rudbman-sql (an incremental lexer, seven dialects, statement splitting), rudbman-editor (ropey, IME — with the composition-caret bug in the gpui example fixed), rudbman-grid (virtualized on both axes, a million rows), the query pipeline (run/cancel/generation guard/NearEnd paging/multiple results/write confirmation), the RDB1 codec on both ends |
| M4 | done | Bridge job frames (0x40–42) and `ExtractJob` (DDL as every CREATE first, then every FK ALTER; insert/csv/template), the jdbgen template engine carried over (with an asset-compatibility canary test), the Rust job API, the extraction dialog, opening a SQL file (Ctrl+O) into the existing run pipeline |
| M5 | done | The rudbman-erd crate (model, grid and hand-written Sugiyama layouts, orthogonal routing, SVG export, canvas widget — pure and gpui modules kept apart), `PaneItem::Erd` plus `ErdPane` (loading, toolbar), the FK loader (imported_keys per table, deterministic ordering), arrangements saved to `erd/<uuid>.json` (once per gesture), `OpenErd` from the explorer (Ctrl/Cmd+E, per scope) |
| M6 | done | The bridge's `TransferJob` (two session locks taken in handle order, two cancel slots, a `uses(Session)` hook, three upsert dialect families with PK/OTHER sync refused, on_error abort/skip/log — batches isolated with savepoints, errors capped at 100), `BackupJob` (scope enumeration, INSERTs topologically sorted by FK, gzip) and `meta/Upsert`, `rows_skipped` in progress, the Rust `TransferSpec`/`BackupSpec` with `start_transfer`/`start_backup`, and the transfer and backup dialogs (the extraction polling pattern, a target-connection select) |
| M7 | done | The rudbman-erd canvas made shareable (a `canvas` module — viewport, gestures, per-column geometry, four paint layers, with the ERD's SVG bytes demonstrably unchanged) plus `BuilderView` (column clicks toggle, column-to-column drags join, state owned by the host), rudbman-sql `quote_ident`/`qualify` (quoting only when it must — keywords, non-identifier characters, per-dialect case folding), `PaneItem::QueryBuilder` plus `BuilderPane` (join/WHERE/GROUP BY/ORDER BY forms, a SQL preview, `open_query` into an editor), and the quoting defect in `open_query_for` fixed |
| Interim UI work | done | Mini-tabs (a tab list per pane, opening a duplicate moves to it), **per-connection work areas** (switching the top tab switches everything below it — the design the user settled), the focus-reclaim discipline (see "pitfalls" below), editor font settings wired through |

### After the milestones

| Work | State | What went in |
|---|---|---|
| Builder drag-and-drop and a welcome screen (PR #6) | done | An explorer row naming a table or a view can be dragged onto a query builder canvas — the same gate and the same one-round-trip column load as the menu action, except the drop lands on the builder under the pointer. With no connections, the explorer sidebar stays out of the frame (without touching the saved preference) and a welcome screen offers the connection dialog and the saved connections, one click to connect |
| Context menus (PR #7) | done | A right-click menu on every surface: the widgets learn only the gesture and emit an event, while the app owns the menu, the labels and the commands, because the widget layer carries no user-facing strings (§7.8). Rows that cannot run right now are drawn greyed rather than left out, so the menu doubles as the surface's documentation. Menus are described as MenuRow lists before they are drawn, and the tests read the description instead of clicking computed pixels. Escape closes a menu ahead of everything else — which uncovered and fixed the editor's find-bar binding swallowing the key with the bar shut |
| Monospace font fix | done | The literal family `monospace` is a fontconfig generic that only Linux resolves; on Windows every surface asking for it logged an error and fell back to the proportional system font. The app now resolves the first installed candidate per OS (Windows: Cascadia Mono through Courier New) and falls back to the alias where nothing matches |
| First release, v0.1.0 (PR #8) | done | `release.yml` (every build job runs Gradle, then jlink, then cargo — in that order, because rudbman-jdbc's build script refuses to compile without the bridge JAR — plus a smoke step over the staged tree before anything is published), `packaging/` (a Linux desktop entry and an `install.sh` that installs the whole tree and symlinks it, a macOS `Info.plist`), `<exe_dir>/runtime` added to the bundled-runtime search, `jdk.charsets` in the jlink module list (with `--compress=2`, the JDK 17 spelling), and a README brought fully up to date with three screenshots (captured through the temporary env-gated hook, reverted before the commit) |

- Repository: <https://github.com/xcomart/rudbman> (public, MIT).
- The branch flow is logman's: **work on dev, main takes PR merge commits
  only**. CI is a three-platform matrix and runs the bridge (Java) suite
  before the Rust one.
- Test count (2026-08-06): roughly 810 Rust plus 141 Java. That includes the
  integration tests, which boot a real JVM and a real H2.

## What is next

The planned milestones (M0–M7) are all finished, and the first release is
out. What remains can be taken in any order:

1. **Verification against real databases**: opt-in PostgreSQL/MySQL container
   tests (the transfer and backup paths), and confirmation of how
   Oracle, SQL Server and DB2 spell the upsert MERGE (a known gap, named in
   the bridge README).
2. **Open features**: the list below (the LOB viewer is the largest — §12.7
   needs a re-read strategy decided).

### Open items not tied to a milestone

- LOB_READ(0x25) is not implemented in the bridge, so there is no LOB viewer.
  Candidate re-read strategies are in §12.7.
- PL/SQL blocks and MySQL DELIMITER are not handled by statement splitting
  (the limit is pinned by rudbman-sql tests).
- A failed file read only reaches the log — the shell has no transient
  message strip. Wire it up once a shared notification UI exists.
- Mini-tab polish: scrolling the active tab into view, drag to reorder.
- Connection A's write-confirmation modal stays up after switching to
  connection B (it still responds and it is still correct, but it looks
  wrong — low priority).
- Settling the level of formality in the Spanish and German UI (waiting on
  the user's answer), plus other terminology flags.
- The macOS bundle is ad-hoc signed, so macOS 15's Local Network permission
  is often denied silently — no prompt, and the app is never listed in
  System Settings. The README documents the Terminal-launch workaround; the
  real fix is Developer ID signing.

## How work is done in this repository

- **Documents first.** Wire contracts and design decisions are changed in
  architecture.md before they are changed in code. The goal was for the
  bridge (Java) and the Rust codec and bindings to fit together even when
  written from the document alone by different hands, and that is how they
  were built.
- **Commit messages are English prose** that carry the "why". No
  Co-authored-by.
- **Merge with a merge commit once CI is green on all three platforms**
  (`gh pr merge --merge`).
- vendor/gpui stays **byte-identical** with logman's vendor tree, so patches
  can be exchanged as diffs. Do not edit it.
- Verification commands: `cargo test --workspace`, `cargo clippy --workspace
  --all-targets -- -D warnings`, `cargo fmt --check`, `cd bridge &&
  ./gradlew build`. The bridge JAR is regenerated with `cd bridge &&
  ./gradlew jar`.
- The JVM/H2 integration tests find the H2 driver JAR in the Gradle cache by
  themselves. When they cannot, point `RUDBMAN_TEST_H2_JAR` at it (this is
  what CI does — and the lesson of the first CI run was that on Windows the
  path has to be converted to its native spelling with `cygpath -w`). The
  automatic search locates the Gradle home through `HOME` (or
  `GRADLE_USER_HOME`), so it **fails under Windows PowerShell**, where there
  is no `HOME` — under Git Bash it simply works; under PowerShell, set
  `RUDBMAN_TEST_H2_JAR`.

## Development-environment pitfalls (machine-specific notes included)

- **A release build on Windows needs `fxc.exe` from the Windows SDK**, which
  gpui invokes for its shaders. This development machine has no SDK, so
  `cargo build --release` fails here; debug builds are unaffected, and the
  release binaries are produced by CI.
- **jlink `--compress`**: JDK 17 wants `--compress=2`. The `zip-N` syntax is
  JDK 21 and newer, and 17 errors on it.
- **On this X display a gpui app sometimes receives no keyboard or mouse
  input** (the pointer device disappears and XInput2 initialization fails).
  GUI checks are therefore done with (1) a temporary hook gated by an
  environment variable (`RUDBMAN_DEV_AUTOCONNECT` — auto-connect to an
  in-memory H2 and open the screens needed from code) and (2) screenshots.
  The hook is **always reverted before a commit, and the revert confirmed in
  the diff**. Runs isolate `XDG_CONFIG_HOME` so the real settings are not
  polluted.
- Screenshots: `xwd` dies on X_QueryColors on this display. Capture with a
  small C tool that uses XGetImage instead (see xshot.c in the scratchpad —
  link against libX11).
- **Never `pkill -f`** — it matches command lines and kills your own shell
  too. Always kill by PID.
- The user may be building something else, such as logman, on this machine at
  the same time — cargo lock contention can make a build slow.
