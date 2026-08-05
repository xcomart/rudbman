# <img src="assets/icon.svg" width="28" alt=""> rudbman

[![CI](https://github.com/xcomart/rudbman/actions/workflows/ci.yml/badge.svg?branch=main)](https://github.com/xcomart/rudbman/actions/workflows/ci.yml)
[![License](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)
![Platforms](https://img.shields.io/badge/platform-Windows%20%7C%20macOS%20%7C%20Linux-informational)

A multi-platform GUI database workbench written in Rust, built on
[gpui](https://gpui.rs) — the GPU-accelerated UI framework behind the Zed
editor — that talks to any database with a JDBC driver through an embedded
JVM bridge.

> **Status: under active development.** Connections, exploration, the query
> workbench and script extraction work today; ERD generation, backup and
> DB-to-DB transfer are on the roadmap. There is no release yet.

## What works today

- **Any JDBC driver.** Drivers are downloaded from Maven Central by
  coordinate or picked from disk, the driver class is auto-detected from the
  JAR, and each connection gets its own isolated class loader. Row data
  crosses the JNI boundary in a compact columnar batch format — not as one
  JSON string per row.
- **Connections as tabs**, each with its own work area: split panes, tabbed
  editors and table details, per-connection query numbering. Switching the
  connection tab switches the explorer and the whole work area with it.
- **SSH tunnels** with password or private-key auth, loopback binds only,
  and no PTY on the bastion — `nologin` jump hosts work. Secrets live in the
  OS keychain, never on disk, and credentials are masked out of logs and
  JDBC URLs.
- **A schema explorer** — catalogs, schemas, tables, views, sequences,
  routines — and a table detail panel with columns, keys, references in both
  directions, and DDL (the server's own text where the dialect offers it,
  reconstructed from metadata where it does not).
- **A SQL workbench**: an editor with incremental dialect-aware highlighting
  and IME support, statement splitting, run-one/run-selection/run-all,
  cancellation, multiple result sets, and a virtualized grid that scrolls a
  million rows without loading a million rows.
- **Script extraction**: any table to a replayable `.sql` (every `CREATE`
  first, every foreign-key `ALTER` after, so cyclic schemas replay), to CSV,
  or through a user template — streamed to the file inside the JVM with
  progress and cancellation. Script execution is the same editor pipeline:
  open a `.sql` file, run it, get per-statement errors.
- **Themes and languages**: UI and editor theme registries with live
  preview, import/export, and a user theme directory; eight UI languages
  (en, ko, ja, zh-CN, de, es, fr, ru).

## Building

Prerequisites: stable Rust, and a JDK (17+) with Gradle wrapper support for
the bridge.

```sh
# The Java half first: the Rust build refuses to proceed without the bridge JAR.
cd bridge && ./gradlew build && cd ..

# Then the workspace.
cargo build --release
```

Running the tests mirrors CI: the bridge suite first, then the Rust
workspace, whose integration tests boot a real JVM against a real in-memory
H2 (the driver JAR is found in the Gradle cache, or pointed at with
`RUDBMAN_TEST_H2_JAR`):

```sh
cd bridge && ./gradlew build && cd ..
cargo test --workspace
```

On Linux, gpui links against `libxkbcommon` (with its X11 half),
`wayland-client` and `fontconfig` development packages; see
[.github/workflows/ci.yml](.github/workflows/ci.yml) for the exact list.

## Architecture

The design document — the JNI wire protocol, the columnar batch codec, the
data-plane rule that row data never crosses the JNI boundary, the SSH tunnel
lifecycle, and the milestone plan — lives in
[docs/architecture.md](docs/architecture.md) (Korean). Current progress and
open items are tracked in [docs/status.md](docs/status.md).

`vendor/gpui` is a vendored copy of gpui 0.2.2 carrying six crash/hang
fixes, kept byte-identical with the same tree in
[logman](https://github.com/xcomart/logman) so fixes move between the two
projects as plain diffs.

## License

[MIT](LICENSE). The vendored gpui keeps its upstream license.
