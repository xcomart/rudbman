# Progress and handoff

This document exists so that another session — or another person — can pick
the work up. The design and the contracts all live in
[architecture.md](architecture.md); what is kept here is only **how far the
work has come, what is left, and how work is done in this repository**. It is
updated whenever a milestone ends.

Last updated: 2026-08-14 (after container verification against all five
products).

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
| Table data editing, phase 1 (§7.9) | done | `PaneItem::TableData` plus `DataPane` — `SELECT *` at the settings' fetch size, paged by the query pane's own cursor walk (moved into `query_source` so both use one), sorted by an appended `ORDER BY`, opened from the explorer row and the detail header. `rudbman-sql::dml` (`plan_edits`: one `UPDATE`/`DELETE`/`INSERT` per row, deletes then updates then inserts, every value a bind parameter and every name through `quote_ident`). `data_edit` — the staging buffer keyed by base row index, the overlay the grid draws through, the `java.sql.Types` → bind-form table, and the planner that turns one into the other. The apply: the whole batch shown before any of it runs (which is the write confirmation, by superset), then autocommit off, one `execute` each, a row count of exactly 1 required of every `UPDATE` and `DELETE`, commit, reload — and on any failure a rollback *before* autocommit is restored. Proved end to end against H2 on both sides: the pane's own suite, and `rudbman-jdbc`'s `tests/h2.rs` for the wire mechanics |
| Container verification, all five products | done | `docker/compose.yml` grew from two services to five — `postgres:17`, `mysql:8.4`, `mariadb:11`, `mcr.microsoft.com/mssql/server:2022-latest` and `gvenzl/oracle-free:23-slim-faststart`, on 55432/33306/53306/51433/51521 so a developer's own servers are never shadowed — and `crates/rudbman-jdbc/tests/containers.rs` grew with it, from 15 tests to 38, every §7.10 DDL claim now checked against all five by writing the statement and reading the catalogue back. CI's `containers` job runs all five services. **Two bugs turned up and were fixed, not just documented**: MariaDB rejects `ALTER TABLE ... DROP CHECK` outright (error 1064) where MySQL accepts it, so the generator gained a `DialectId::MariaDb` distinct from MySQL — `DropStyle::PerKind { check }` now carries a MariaDB row that spells the same drop `DROP CONSTRAINT` instead (`ddl.rs`). Separately, the bridge was reading JDBC metadata `ResultSet`s out of column order, which every driver tried so far tolerated except Oracle's: describing a table with a defaulted column threw `ORA-17027`. Eight read paths in `bridge/src/main/java/comart/rudbman/bridge/meta/` (`Describe`'s columns, three in `Routines`, four in `Ddl`, and `Sequences`) now read every `ResultSet` by ascending ordinal. §7.9's driver-metadata picture is now complete rather than PostgreSQL/MySQL-only: pgjdbc answers `""` for schema *and* catalog and reports a column's alias as `getColumnName`; Connector/J does *not* report a table alias by default; MariaDB Connector/J behaves like Connector/J, not like pgjdbc; and mssql-jdbc and ojdbc11 answer `""` for the **table** itself on an ordinary column, so query-result editing's gate (§7.9) can never offer editing at all on SQL Server or Oracle — which is the gate's read-only default working as designed, not a hole in it |
| Editing a query result (§7.9, "Editing a query result") | done | The apply machinery lifted out of `data_pane` into `row_apply` first, so the staging buffer, the planner, the batch, the preview and the transaction ordering (rollback *before* autocommit is restored) exist once rather than twice — `apply_batch` and `unwind` moved byte for byte. Then the gate in `query.rs`: a pure `source_table(&[ColumnInfo])` over metadata normalised for JDBC's empty-string-means-unknown, every naming column agreeing on the whole `(catalog, schema, table)` triple, at least one naming, and the primary key present in full through the same `key_index` the statement writer uses. Rows draw immediately; the key is a second round trip that flips the grid writable, guarded by the generation *and* the result tab's id. Updates and deletes only. Whether a column the driver named no table for is writable became `TableSource::{Known, Inferred}` — the data pane names its catalogue table in the statement whatever the columns say, so a nameless column stays writable there, and only an inferred table makes one a computed column. Fixed on the way: `mark_primary_keys` matched the heading where `plan_apply` matched the catalogue name, so `SELECT id AS pk` drew the key unmarked and wrote a `WHERE` from it anyway; they now ask one function |
| Table creation (§7.10, "Creating a table") | done | `plan_create` beside `plan_alter` — one statement, multi-line because it is read before it is run, and the only place a `PRIMARY KEY`, `UNIQUE` or `FOREIGN KEY` is born, since `plan_alter` drops constraints and never adds one. A table-level constraint turned out byte-identical on all seven dialects, so `AlterStyle` gained no row. No auto-increment flag and no `IF NOT EXISTS` (Oracle has neither). The pane takes a second mode rather than a second surface — a table being created is a table whose current shape is empty, so the column list, the form, the live batch and the apply are the ones already there, diverging in eight named places. Two calls worth knowing: the "no name"/"no columns" refusals are held until the apply is asked for, because a pane just opened is in both states; and a refused create *keeps* what was typed, where a refused alter discards it — the discard rule is about what a committed statement invalidates, and a create that failed committed nothing. On success the pane becomes the editor for the table it made |
| Structure editing (§7.10) | done | `rudbman-sql::ddl` — `plan_alter` over a diff that carries **both sides** of every changed column, because MySQL's `MODIFY`/`CHANGE` restates a whole definition and SQL Server's `ALTER COLUMN` resets nullability when the clause is omitted. Statements are plain strings (no server takes a `?` in DDL) and a type or a default is the user's own SQL, passed through unread. The per-product spellings are one flat record per dialect in the shape `Syntax` is one; what a product cannot express is refused by name and reason (SQLite's type/nullability/default and constraint drops, SQL Server's defaults) rather than generated and rejected. `struct_edit` — the staging model, pure and unit-tested: a draft equal to its snapshot is dropped rather than refused, and a dropped column discards any change staged against it. `PaneItem::TableStruct` plus `StructPane` — its own four `DESCRIBE`s (the panel keeps display strings, so nothing an editor needs survives in it), the PK's own backing index matched away by name *and* by covering the key's columns, one column edited at a time in a form rather than an input per cell, and the batch shown live and read-only before it can be run. The apply forces autocommit on and never calls rollback — MySQL and Oracle commit at every DDL statement — stops at the first refusal, and on **either** outcome discards and reloads, saying how far it got |
| First release, v0.1.0 (PR #8) | done | `release.yml` (every build job runs Gradle, then jlink, then cargo — in that order, because rudbman-jdbc's build script refuses to compile without the bridge JAR — plus a smoke step over the staged tree before anything is published), `packaging/` (a Linux desktop entry and an `install.sh` that installs the whole tree and symlinks it, a macOS `Info.plist`), `<exe_dir>/runtime` added to the bundled-runtime search, `jdk.charsets` in the jlink module list (with `--compress=2`, the JDK 17 spelling), and a README brought fully up to date with three screenshots (captured through the temporary env-gated hook, reverted before the commit) |

- Repository: <https://github.com/xcomart/rudbman> (public, MIT).
- The branch flow is logman's: **work on dev, main takes PR merge commits
  only**. CI is a three-platform matrix and runs the bridge (Java) suite
  before the Rust one.
- Test count (2026-08-14): roughly 1080 Rust plus 141 Java. That includes the
  integration tests, which boot a real JVM and a real H2, and the 38 opt-in
  container tests (`crates/rudbman-jdbc/tests/containers.rs`, up from 15).

## What is next

The planned milestones (M0–M7) are all finished, and the first release is
out. What remains can be taken in any order:

1. **Verification against real databases**: **all five products the workspace
   targets are now done for the DDL generator** — PostgreSQL, MySQL, MariaDB,
   SQL Server and Oracle, not just the original two. `docker/compose.yml`
   plus `crates/rudbman-jdbc/tests/containers.rs` (38 tests, opt-in through
   `RUDBMAN_TEST_PG_URL`/`RUDBMAN_TEST_MYSQL_URL`/`RUDBMAN_TEST_MARIADB_URL`/
   `RUDBMAN_TEST_MSSQL_URL`/`RUDBMAN_TEST_ORACLE_URL` — unset, every one of
   them passes by doing nothing, which is what keeps `cargo test --workspace`
   green without Docker) run continuously by CI's Linux-only `containers` job.
   They send what `rudbman-sql` writes to a real server and read the catalogue
   back, and they earned their keep: see the container-verification entry
   above for the two bugs that surfaced only once Oracle and MariaDB joined
   the suite — a syntax error MariaDB throws that MySQL does not, and a
   metadata-ordering bug in the bridge that only Oracle's driver was strict
   enough to expose. What is still unverified against a live server: the
   **transfer and backup** paths (no container test drives `TransferJob` or
   `BackupJob` against any of the five yet), and **DB2**, which has no
   container in the suite and no built-in driver definition at all — how it,
   Oracle and SQL Server spell the upsert `MERGE` remains a known gap, named
   in the bridge README.
2. **Open features**: the list below (the LOB viewer is the largest — §12.7
   needs a re-read strategy decided).

### Open items not tied to a milestone

- **Editing a query result is updates and deletes, never inserts**, and only
  where the result maps to one table. A row typed into a `SELECT id, name` is
  missing every `NOT NULL` column that was not selected, and one that did insert
  need not satisfy the query's own `WHERE`, so the reload would not show it —
  inserting is what the data pane on that table is for. The gate itself is a
  heuristic on purpose (§7.9): a driver can report an alias where the table was
  asked for, and several report `""` for schema and catalog unconditionally, so
  what makes it safe is that every way out of a wrong hint is a refusal rather
  than a wrong statement. If a product turns up where the offer appears and the
  apply then fails, the gate is where to look.
- **Structure editing covers one table at a time and nothing around it.** A
  table can be created — columns, a primary key, unique constraints and foreign
  keys — and an existing one can have a column added, retyped, renamed, made
  null or not, given or denied a default and dropped, a reported constraint
  dropped, and itself renamed. What is *not* there: **adding** a constraint or
  an index to a table that already exists (creation is the only place one is
  born, which is why the create path carries them), dropping a table,
  reordering columns, and check constraints — JDBC's `DatabaseMetaData` does
  not report them, so the pane has no source for one even though the generator
  can write the drop. Auto-increment is deliberately unmodelled: a column's
  type is the user's own SQL passed through unread, so `SERIAL`, `INT
  AUTO_INCREMENT` and `GENERATED BY DEFAULT AS IDENTITY` all reach the server
  as typed. Two refusals
  are per-product and would each need real work rather than a spelling: SQL
  Server's defaults are separately named constraints whose names the catalog
  does not give, and SQLite needs a table rebuild (new table, copy, drop,
  rename) for anything but add/drop/rename, which is a data-moving job wearing
  a schema job's clothes and belongs with the §6 data plane if it is ever done.
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
- **GUI checks** are done with (1) a temporary hook gated by an environment
  variable (`RUDBMAN_DEV_AUTOCONNECT` — auto-connect to an in-memory H2 and
  open the screens needed from code) and (2) screenshots. The hook is **always
  reverted before a commit, and the revert confirmed in the diff**. This is
  worth doing rather than skipping: driving the panes for real is what turned
  up §7.10's partial-failure path behaving exactly as designed (statement 1 of
  2 committed, statement 2 refused by the server, the strip naming both), which
  no unit test had shown end to end. **On Windows** launch from Git Bash — the
  hook finds the H2 JAR through `HOME` — and capture with PowerShell plus
  `System.Drawing`; the app takes synthetic clicks through `user32`
  `mouse_event` normally. Two harness traps, neither an app bug: `SendKeys`
  treats `(` and `)` as grouping and needs `{(}`/`{)}`, and a negative mouse
  wheel delta has to be passed as its unsigned two's complement.
- **On the Linux/X11 development machine** — *not* this Windows one — a gpui
  app sometimes receives no keyboard or mouse input (the pointer device
  disappears and XInput2 initialization fails), which is why the hook above
  exists at all. Runs there isolate `XDG_CONFIG_HOME` so the real settings are
  not polluted.
- Screenshots on that X display: `xwd` dies on X_QueryColors. Capture with a
  small C tool that uses XGetImage instead (see xshot.c in the scratchpad —
  link against libX11).
- **Never `pkill -f`** — it matches command lines and kills your own shell
  too. Always kill by PID.
- The user may be building something else, such as logman, on this machine at
  the same time — cargo lock contention can make a build slow.
