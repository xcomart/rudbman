# Contributing to rudbman

Contributions to SQL editing, database browsing, data grids, schema tools,
ER diagrams, translations, tests and documentation are welcome. Read the
[README](README.md) and [architecture](docs/architecture.md) before changing a
cross-cutting feature.

## Reporting a problem or proposing a change

Search the [issues](https://github.com/xcomart/rudbman/issues) first. A useful
report includes the application version or commit, OS, database and JDBC driver
versions, reproduction steps, expected behavior and actual behavior. Include a
minimal schema and SQL example for query, DDL, metadata or data-editing bugs.
Mention autocommit and transaction state when relevant.

Remove credentials, private data and identifying connection details from logs,
SQL and screenshots. Discuss substantial changes to SQL behavior, the bridge
protocol or the workspace UI before implementing them.

## Set up a development checkout

Use Git, stable Rust with `rustfmt` and `clippy` (CI and the release builds pin
Rust 1.98.0), a native compiler toolchain, and JDK 17 or newer. CI uses JDK 17.
Set `JAVA_HOME` and add its `bin` directory to `PATH`. Use the included Gradle
wrapper. Windows needs MSVC and the Windows SDK; macOS needs Xcode command line
tools.

On Debian or Ubuntu, install the native libraries used by CI:

```sh
sudo apt-get update
sudo apt-get install -y --no-install-recommends \
  libxkbcommon-dev libxkbcommon-x11-dev libwayland-dev libfontconfig1-dev
```

From a checkout of `dev`, build the Java bridge first:

```sh
rustup component add rustfmt clippy
cd bridge
./gradlew build --no-daemon
cd ..
cargo build --workspace --locked
cargo run -p rudbman-app --locked
```

In Windows PowerShell, use `.\gradlew.bat build --no-daemon` inside `bridge`.
Initial builds download Cargo and Maven dependencies. Running the UI needs a
graphical session. Windows release builds also require the SDK's `fxc.exe`;
set `GPUI_FXC_PATH` if it cannot be discovered.

Cargo checks for `bridge/build/libs/rudbman-bridge.jar` and never runs Gradle.
Rebuild the JAR after Java changes to avoid testing stale bridge code. Source
builds discover Java through `RUDBMAN_JAVA_HOME`, then `JAVA_HOME`, then system
locations; only release archives include a runtime.

## Where changes belong

| Location | Responsibility |
| --- | --- |
| `crates/rudbman-app` | Workspace, explorer, SQL editor integration, result and structure panes |
| `crates/rudbman-core` | Profiles, settings, paths and secret storage |
| `crates/rudbman-sql` | SQL analysis, statement handling, dialects and DDL generation |
| `crates/rudbman-erd` | ER diagram model, layout and views |
| `crates/rudbman-jdbc` | Rust bridge client, result decoding and JDBC integration tests |
| `bridge` | Java JDBC sessions, metadata, cursors, LOBs and database jobs |
| `crates/rudbman-ssh` | Compatibility facade over shared SSH transport |
| `crates/rudbman-app/locales` | Interface translations |

Reusable widgets, grids, editor and shell belong in
[rugpui](https://github.com/xcomart/rugpui/blob/dev/CONTRIBUTING.md). Connection
validation, tunnel ownership and JVM startup belong in
[rudbkit](https://github.com/xcomart/rudbkit/blob/dev/CONTRIBUTING.md).
Use their contribution guides for local Cargo patch examples and consumer
checks. Keep each shared repository's Git references on one revision, including
the four gpui patches from rugpui. Include intentional `Cargo.lock` updates;
remove temporary local overrides before submitting.

## Validate a change

Run the baseline checks from the repository root:

```sh
cd bridge
./gradlew build --no-daemon
cd ..
cargo fmt --all --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
```

The bridge and Rust integration suites use real in-memory H2 databases. The
Gradle build fetches H2; if Rust cannot find it in the cache, set
`RUDBMAN_TEST_H2_JAR` to the driver's absolute JAR path. Resolve JVM, bridge and
driver setup failures before treating the integration suite as validated.

Use focused checks while iterating:

```sh
cargo test -p rudbman-sql --locked
cargo test -p rudbman-erd --locked
cargo test -p rudbman-jdbc --test h2 --locked
cargo test -p rudbman-ssh --locked
```

### Database-specific checks

H2 alone cannot verify a vendor's SQL syntax or metadata behavior. The container
tests return without contacting a database when its `RUDBMAN_TEST_*_URL` is
unset. For a PostgreSQL change, use a disposable server:

```sh
cd bridge
./gradlew jar drivers --no-daemon
cd ..
docker compose -f docker/compose.yml up -d postgres
docker compose -f docker/compose.yml ps
# Wait for postgres to become healthy before continuing.
export RUDBMAN_TEST_PG_URL='jdbc:postgresql://127.0.0.1:55432/rudbman'
cargo test -p rudbman-jdbc --test containers --locked -- --nocapture
docker compose -f docker/compose.yml down
```

These are POSIX shell commands; PowerShell uses `$env:NAME = 'value'` for
environment variables. [docker/compose.yml](docker/compose.yml) documents the
MySQL, MariaDB, SQL Server, Oracle and Db2 settings. Db2's first start creates
its database and takes several minutes before the container reports healthy.
[CI](.github/workflows/ci.yml) shows the full matrix and driver overrides such
as `RUDBMAN_TEST_PG_JAR`. These tests create and drop objects; use test databases,
not a connection holding real data. The rudbman and rudbgen Compose stacks use
the same host ports and should run one at a time.

For SQLite, download the drivers, set `RUDBMAN_TEST_SQLITE_URL` to
`jdbc:sqlite:/absolute/path/to/a/test.db` in a disposable directory, and run
`cargo test -p rudbman-jdbc --test sqlite --locked -- --nocapture`.
Use `RUDBMAN_TEST_SQLITE_JAR` if cache discovery fails. No Docker server is needed.

OS-keychain, Maven Central and real-bastion SSH tests are ignored by default.
Read their setup requirements and select only the relevant tests with
`--ignored`. The normal SSH suites start loopback servers.

## Change guidelines

- Preserve SQL dialect differences. Cover identifier quoting, nulls, keys and
  the affected database's actual behavior when changing generated DDL or SQL.
- For data editing, test inserts, updates, deletes, commit/rollback, error paths
  and row identity as applicable. Use a temporary database for manual checks.
- Keep Java and Rust protocol changes in step. Update both sides' tests and the
  [bridge contract](bridge/README.md), and check whether the rudbgen bridge needs
  the same fix. Preserve the distinction between null, empty and large values.
- Keep database and file work off the UI thread. Preserve cancellation and
  resource cleanup for sessions, cursors, LOBs and background jobs.
- Keep secrets in the OS credential store and maintain compatibility with saved
  profiles and settings.
- Keep translation keys and placeholders aligned across all eight locale files.
  For UI changes, check keyboard navigation, focus after editing, long labels,
  light/dark themes and a non-English locale.
- Follow the surrounding style, format with `cargo fmt --all`, add regression
  tests for behavior changes and update affected documentation.

## Submit your contribution

`dev` is the integration branch. Fork if needed, branch from the latest upstream
`dev`, and open a focused pull request targeting `dev`. Maintainers working
directly in this workspace validate and commit on `dev`, then push to
`origin/dev`.

Use `type: summary` for new commit subjects, with a concise imperative summary.
Choose `feat`, `fix`, `docs`, `refactor`, `test`, `perf`, `build`, `ci` or `chore`
to describe the change, for example `fix: preserve inserted row selection`
or `docs: add contribution guide`. Release and merge commits also use a prefix,
such as `chore: release <version>` or `chore: merge dev into main`. This convention
applies across all five repositories; do not rewrite existing history to rename
older commits.

Describe the problem, resulting behavior, checks run and checks not run in the
pull request. Include database/driver versions for integration changes,
reproduction SQL for fixes and screenshots for visible changes. Review the diff
for unrelated formatting, credentials, build output and local configuration.

CI runs on pull requests and on pushes to `dev` and `main`. Run the local
checks before pushing. Routine work ends at
`origin/dev`. Releases are separate maintainer work: merge the `dev` → `main` PR
after required checks, fetch `origin/main`, then tag its verified latest commit
and publish. Do not move an existing release tag.

Contributions are covered by the [MIT license](LICENSE). Preserve third-party
notices when incorporating external code.
