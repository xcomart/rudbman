# rudbman architecture

A database workbench that wraps JDBC drivers in JNI and puts a Rust + gpui GUI
on top. Browsing databases, building and running queries, generating ERDs,
extracting and running scripts, backing up, and moving data from one database
to another all happen in a single window.

This document exists to fix the boundaries before any code is written. What it
does not decide, the implementer may decide; what it does decide never changes
without changing this document.

---

## 1. Decision summary

| # | Decision | Why |
|---|---|---|
| D1 | **Copy** logman's gpui widget kit into `rudbman-ui` and let it evolve on its own | Early speed comes first. Extracting a shared crate waits until both sides are stable |
| D2 | **Vendor logman's patched gpui** and use it as is | The Korean IME infinite loop, the X11 re-entrancy panic and the KWin blur patch are all just as necessary here |
| D3 | The JNI boundary is **one coarse-grained bridge JAR**, with exactly **one** static method as its entry point | A per-cell JNI round trip becomes tens of millions of calls at 100k rows × 20 columns. Unusable |
| D4 | The **data plane** of backup and DB-to-DB transfer **completes inside the JVM** | Gigabytes never get ferried across JNI. Rust only issues commands and polls progress |
| D5 | Every connection gets a **dedicated Rust worker thread** that stays attached to the JVM | JDBC connections are not thread-safe and the gpui UI thread must never block |
| D6 | Every driver is **isolated** in a child `URLClassLoader` | Prevents class conflicts between the Oracle, MySQL and MSSQL drivers. jdbgen already works this way |
| D7 | A **jlink bundled runtime** ships inside the platform package | Users are never asked to install a JRE |
| D8 | Result batches use a **custom columnar binary codec**; metadata is JSON | Arrow Java demands 40MB+ of dependencies and `--add-opens`. What the grid needs is far less than that |
| D9 | The **UI theme and the editor theme are separate** token sets | The 11 colors of the window chrome and the twenty-odd colors of syntax highlighting are different axes |
| D10 | **SSH local port forwarding is in scope for M1** | Production databases usually sit behind a bastion. Bolting it on later means fixing the connection profile schema and session lifetime management twice |

---

## 2. Inherited assets

### 2.1 logman (`~/Work/logman`) — the entire UI

What gets copied (paths relative to `crates/logman-app/src/`):

| Source | Destination | Notes |
|---|---|---|
| `ui/`, 14 files, 6.5k lines | `crates/rudbman-ui/src/` | Rename the `logman_input` action namespace to `rudbman_input` |
| `theme_store.rs` | `crates/rudbman-ui/src/theme_store.rs` | Extended to also load an editor-theme directory |
| `theme_editor.rs` | `crates/rudbman-app/src/theme_editor.rs` | Adds an editor-theme tab |
| `pane_tree.rs` | `crates/rudbman-app/src/pane_tree.rs` | Made generic so it holds editor/grid/ERD panes instead of terminals |
| `caption.rs` | `crates/rudbman-app/src/caption.rs` | As is |
| `i18n.rs` + `locales/*.yml` | same | Keys replaced |
| `icons.rs` | same | Database icons added |
| `about_dialog.rs` | same | Strings replaced |
| all of `logman-core/` | `crates/rudbman-core/` | `settings`/`profile`/`secrets`/`paths`. Only the profile schema is replaced |
| `logman-ssh/` | `crates/rudbman-ssh/` | M1. Only transport, auth and host keys are inherited; shell and SFTP dropped, forwarding added (§9) |
| `known_hosts.rs`, `verifier.rs` | `rudbman-core` / `rudbman-app` | Host key storage and the confirmation dialog |
| `vendor/gpui/` | same | **Leave the `LOGMAN PATCH` comments exactly where they are** — the two vendored trees stay byte-identical so they can be synced with `diff`. Do not remove them before an upstream release |

What is not brought over: `logman-pty`, `logman-term`, `terminal_view.rs`,
`file_panel.rs`, `connection.rs` (SSH shell only), `files.rs`, and
`logman-ssh`'s `sftp.rs`.

`ui/scheme_picker.rs` previews terminal color schemes, but its shape carries
over almost unchanged as an editor-theme preview. Copy it, then rework it.

### 2.2 jdbgen (`~/Work/jdbgen`) — the Java layer

This is where the bridge JAR starts. What is inherited:

- `types/db/DBMeta.java` (417 lines) — the per-driver `URLClassLoader`, the
  direct `Driver.connect` call that bypasses `DriverManager` (avoiding the
  global driver registry), `ReentrantLock`-based connection serialization, the
  keep-alive scheduler, and schema/table/column lookups
- `types/db/SqlTypes.java`, `DBColumn`, `DBTable`, `DBSchema`, `DBMetaModel`
- `utils/ClassUtils.java` — scans a JAR for classes implementing
  `java.sql.Driver` (driver class auto-detection in the driver registration UI)
- `utils/MavenREST.java`, `ui/MavenExplorer.java` — driver JAR downloads from
  Maven Central. **The logic is ported to Rust** (HTTP is better done in Rust)
- `template/TemplateManager.java` — the script extraction template engine. M4
  settled that it stays in Java and rides in the bridge (§12.3)
- `resources/icons/*.png`, 13 of them — driver icons

**What DBMeta does not have and has to be written fresh**: foreign keys
(`getImportedKeys`/`getExportedKeys`), indexes (`getIndexInfo`), primary keys
(`getPrimaryKeys`), views/procedures/sequences, DDL reconstruction, and query
result schemas from `ResultSetMetaData`.

---

## 3. Repository layout

```
rudbman/
├── Cargo.toml                  workspace. [patch.crates-io] gpui → vendor/gpui
├── docs/architecture.md        this document
├── vendor/gpui/                logman's patched copy
├── bridge/                     Gradle project → rudbman-bridge.jar
│   ├── build.gradle
│   └── src/main/java/comart/rudbman/bridge/
├── runtime/                    jlink output (produced at build time, .gitignore)
├── assets/                     icons
├── packaging/                  linux/macos/windows packaging
└── crates/
    ├── rudbman-core/           settings, profiles, secrets, paths, known_hosts
    ├── rudbman-ui/             gpui widget kit + UI theme + editor theme
    ├── rudbman-ssh/            SSH local port forwarding (M1)
    ├── rudbman-jdbc/           JNI. JVM bootstrap, session workers, wire codec
    ├── rudbman-sql/            SQL lexer, dialects, formatter, completion index
    ├── rudbman-editor/         multi-line code editor widget
    ├── rudbman-grid/           virtualized result grid widget
    ├── rudbman-erd/            ERD model, layout, canvas, SVG export
    └── rudbman-app/            the binary
```

### 3.1 Crate dependency direction

```
rudbman-app
 ├─→ rudbman-erd ─┐
 ├─→ rudbman-grid ┼─→ rudbman-ui ─→ gpui
 ├─→ rudbman-editor ─→ rudbman-sql
 ├─→ rudbman-jdbc ─→ rudbman-core
 ├─→ rudbman-ssh  ─→ rudbman-core
 └─→ rudbman-core
```

`rudbman-ssh` and `rudbman-jdbc` **know nothing about each other.** The tunnel
only opens a local port, and a JDBC session is an ordinary connection aimed at
that port. What ties them together is the session orchestration in
`rudbman-app` (§9.3).

There are no reverse dependencies. `rudbman-jdbc` **knows nothing about gpui**
— it exposes a purely synchronous API, and coupling it to the UI thread is
`rudbman-app`'s job. That boundary is what lets the JNI layer be unit-tested
without gpui.

`rudbman-ui` knows nothing about databases. Same discipline that kept logman's
`ui/` modules ignorant of SSH.

---

## 4. The JNI layer (`rudbman-jdbc`)

### 4.1 JVM lifecycle

- **One** JVM instance per process, held in a `OnceLock<JavaVM>`.
- `jni = { version = "0.22", features = ["invocation"] }`.
- **`DestroyJavaVM` is never called.** It cannot be trusted, and process exit
  cleans up anyway.
- The JVM is created on a **dedicated background thread**. On macOS gpui owns
  the main thread, and the creating thread has to stay alive.
- Runtime location is resolved in this order:
  1. The bundled runtime beside the executable — `<exe_dir>/runtime` (the flat
     distribution folder on Windows and Linux), then `<exe_dir>/../runtime`
     (`Contents/runtime` inside a macOS `.app`)
  2. The `RUDBMAN_JAVA_HOME` environment variable
  3. `JAVA_HOME`
  The resolved path is set as `JAVA_HOME` **before** `JavaVM::new()` is called.

JVM options (fixed):

```
-Djava.class.path=<bridge.jar>
-Djava.awt.headless=true
-Xrs                      # Keeps the JVM from intercepting SIGINT/SIGTERM handlers.
                          # Without it, Ctrl-C and closing the window get swallowed.
-Xss2m                    # Deep stacks in some drivers, Oracle above all
-XX:+UseSerialGC          # A desktop tool's heap is small. Parallel GC threads are waste
-Duser.language / -Duser.country   # Match the app locale so driver error messages match
```

`-Xmx` is exposed as a setting (default 1g). A large result-set fetch hits the
JVM heap first.

### 4.2 Thread model

```
gpui UI thread
    │  Command + oneshot reply channel
    ▼
Session worker thread  ──  stays attached after AttachCurrentThreadAsDaemon
    │  Bridge.call(op, handle, arg, req)
    ▼
Java: session object (one Connection + a child ClassLoader)
```

- One worker thread per connection. The worker drains its command queue
  serially. The thread-unsafety of a JDBC connection is structurally resolved
  right here.
- The worker stays attached. Attaching and detaching per command makes the JVM
  build a thread structure every time.
- **Cancellation is the one exception**: `CANCEL` has to arrive on another
  thread while the worker is blocked. The cancellation path attaches and
  detaches per call — it is a rare event.
- The UI awaits worker replies via `cx.background_spawn`. The UI thread never
  blocks.
- A worker thread panic kills the session but not the process. JNI calls happen
  inside a `catch_unwind` boundary.

### 4.3 The bridge entry point

The Java side has **exactly one static method**:

```java
package comart.rudbman.bridge;

public final class Bridge {
    /**
     * @param op     operation code (§4.4)
     * @param handle session/cursor/job handle. Its meaning depends on the operation. 0 = none
     * @param arg    integer argument for hot paths (FETCH's max rows, etc). Avoids JSON parsing
     * @param req    request body. JSON UTF-8, or null
     * @return       response envelope (§4.5). Never null, never throws
     */
    public static byte[] call(int op, long handle, long arg, byte[] req);
}
```

Rust caches a single `jmethodID`. The Java side wraps every body in try/catch
and turns exceptions into the error tag of the response envelope — **which
removes JNI exception checks from the happy path entirely.** `ExceptionCheck`
only fires in fatal situations such as OOM.

New features arrive as new operation codes. The JNI signature stays the same
forever.

### 4.4 Operation codes

| Code | Name | handle | arg | req | resp |
|---|---|---|---|---|---|
| `0x01` | `OPEN_SESSION` | — | — | JSON connection spec | JSON `{session}` |
| `0x02` | `CLOSE_SESSION` | session | — | — | — |
| `0x03` | `PING` | session | — | — | JSON `{ok, elapsed_ms}` |
| `0x04` | `SESSION_INFO` | session | — | — | JSON DB product, version, feature flags |
| `0x10` | `DESCRIBE` | session | — | JSON `{kind, …}` | JSON |
| `0x20` | `EXECUTE` | session | — | JSON `{sql, params, fetch_size, max_rows, timeout_s}` | JSON `{cursor, columns[], update_count, has_result_set, has_more}` |
| `0x21` | `FETCH` | cursor | max rows | — | **binary batch** (§4.6) |
| `0x22` | `MORE_RESULTS` | cursor | — | — | JSON, same shape as `EXECUTE` |
| `0x23` | `CLOSE_CURSOR` | cursor | — | — | — |
| `0x24` | `CANCEL` | session | — | — | JSON `{cancelled}` |
| `0x25` | `LOB_READ` | cursor | — | JSON `{lob_id, offset, len}` | binary |
| `0x30` | `SET_AUTOCOMMIT` | session | 0/1 | — | — |
| `0x31` | `COMMIT` | session | — | — | — |
| `0x32` | `ROLLBACK` | session | — | — | — |
| `0x40` | `JOB_START` | session | — | JSON job spec (§6) | JSON `{job}` |
| `0x41` | `JOB_POLL` | job | — | — | JSON progress |
| `0x42` | `JOB_CANCEL` | job | — | — | — |
| `0x50` | `PROBE_DRIVER` | — | — | JSON `{jars[]}` | JSON `{classes[]}` |

`DESCRIBE`'s `kind`: `catalogs`, `schemas`, `tables`, `columns`,
`primary_keys`, `imported_keys`, `exported_keys`, `indexes`, `procedures`,
`functions`, `sequences`, `ddl`, `type_info`.

The default response shape is `{kind, items[]}`, but **`ddl` alone is
`{kind, ddl, source}`** — the DDL of one table is one document, and a
one-element array only adds an unwrap on the receiving side. A `ddl` request
takes `source: auto|native|metadata`: `native` is the path where the database
hands over its own DDL (MySQL's `SHOW CREATE TABLE`, H2's `SCRIPT`),
`metadata` is the fallback that reconstructs it from JDBC metadata, and `auto`
(the default) tries native and falls back. Reconstructed DDL is a display aid —
CHECK constraints, triggers and partitions are not in JDBC metadata and do not
come out. Items of `procedures`/`functions` carry `parameters[]` inline (so a
schema with 200 routines does not cost 200 round trips). `sequences` has no
standard JDBC API, so it is a per-dialect catalog query, and a database that
is not known yields an empty list rather than an error.

Why `DESCRIBE` branches on request JSON: the kinds of metadata will keep
growing, and adding an operation code for each one lets the Rust and Java
tables drift apart. Metadata is called rarely, so the JSON parsing cost is
negligible.

Operations and `kind`s that are not implemented yet answer with a
`kind: "protocol"` error saying **"not implemented"**. **That has to be
distinguishable from "unknown"** — the first is something you can wait for, the
second means the two tables have drifted apart.

#### `has_more` is a hint

JDBC has no non-destructive lookahead. There is **no** way to learn whether
another result exists without consuming the current one. So `has_more` on the
wire, despite its name, is only a conservative hint that "`MORE_RESULTS` might
return something". The Rust side therefore reads the field under the name
`may_have_more` — a name that promises a guarantee is a name callers will
believe.

**Do not trust a single value; loop until it comes back `false`.** Exhaustion
is a response where all three hold at once: `has_more: false`,
`update_count: -1`, and no `columns`.

#### `EXECUTE`'s `params`

A bind parameter takes one of two forms:

```json
"params": [42, "text", true, null,
           {"type": "decimal",   "value": "123456789012.12345678"},
           {"type": "timestamp", "value": "2026-08-04T09:30:00"},
           {"type": "bytes",     "value": "<base64>"}]
```

Bare JSON scalars are used only for integers, strings, booleans and null.
`decimal`, `date`, `time`, `timestamp` and `bytes` **must be sent in the typed
form.** The reason is the same one §4.6 gives for forbidding it in the other
direction — send a `DECIMAL(20,8)` as a JSON number and it arrives rounded, and
that cannot be undone.

### 4.5 Response envelope

```
u8  tag       0 = OK, 1 = ERROR
    payload   on OK, the per-operation body (JSON or binary); on ERROR, JSON
```

Error JSON:

```json
{
  "kind": "sql | driver | io | protocol | interrupted | internal",
  "sql_state": "42S02",
  "vendor_code": 942,
  "message": "ORA-00942: table or view does not exist",
  "causes": ["…"],
  "stack": "…"
}
```

`sql_state` and `vendor_code` have to be there for the UI to tell "no such
table" from "no permission" and guide the user differently. `stack` goes only
into the debug log and is never shown to the user.

**Branch on the first two characters of `sql_state` — the class — not on the
whole code.** Even standards-abiding drivers disagree on the last two: for a
missing table H2 says `42S04` while others say `42S02`. Class `42` (syntax
error or access rule violation) is as far as the reliable part goes.

### 4.6 The result batch binary codec (`RDB1`)

Little-endian throughout.

```
Batch  := Header Column*
Header := "RDB1"(4B) | u32 col_count | u32 row_count | u8 flags
          flags bit0 = this is the last batch
Column := u8 kind | u32 payload_len | payload
```

`payload` always starts with a **validity bitmap**: `ceil(row_count/8)` bytes,
where a set bit means non-null. The per-kind values follow.

| kind | Name | Value layout |
|---|---|---|
| 0 | `NULLS` | none. every row is NULL |
| 1 | `I64` | `row_count × i64` |
| 2 | `F64` | `row_count × f64` |
| 3 | `BOOL` | packed bits |
| 4 | `STR` | `u32 offsets[row_count+1]` + UTF-8 bytes |
| 5 | `BIN` | same layout as `STR`, raw bytes |
| 6 | `LOB` | `row_count × (u64 lob_id, u64 size)`. The body comes from `LOB_READ` |

**`DECIMAL`, `DATE`, `TIME`, `TIMESTAMP`, `UUID`, `INTERVAL`, arrays and other
vendor types all travel as `STR`, carrying their canonical text
representation.** Because:

- what the grid ultimately does is display text
- flattening a `BigDecimal`'s precision and scale into an f64 cannot be undone
- time zone hell does not get moved into Rust. The text the driver gave is the
  truth
- sorting is the server's job via `ORDER BY`. No client-side numeric sort is
  needed

Logical types (the `java.sql.Types` value, type name, precision, scale,
nullability, auto-increment) ride in `columns[]` of the `EXECUTE` response
JSON. The codec's `kind` is **physical encoding only**; presentation decisions
such as right alignment, NULL rendering and copy format come from the logical
type.

LOBs are never inlined into a batch. Scrolling past a row holding a 100MB BLOB
cannot mean pushing 100MB across JNI. When the cell is opened, `LOB_READ`
pulls it in chunks. **The address is `lob_id` and nothing else** — it has to be
an opaque identifier independent of cursor position. Addressing by `{row, col}`
would point at a row the result set has already passed.

#### Codec norms — what the encoder and the decoder must agree on

These are the places the spec leaves open. Diverge on any of them and it
silently draws the wrong data.

- **Bitmap bit order is LSB first.** Row `i` → byte `i >> 3`, bit `i & 7`.
  Packed `BOOL` values follow the same rule.
- **The validity bitmap is always present.** Even `NULLS` (kind 0) carries an
  all-zero bitmap; what is omitted is only the value area.
- **The kind of a given column may differ from batch to batch.** Any column
  that is all NULL within a batch collapses to `NULLS`. **The decoder must
  re-read the kind on every batch, not once per cursor.** `columns[].kind` in
  the `EXECUTE` response is only a hint.
- **NULL rows still occupy their slot in a fixed-width value area** (zero
  filled). No rank computation counting preceding non-nulls is needed.
- **In `STR`/`BIN`, NULL and the empty string are both zero-length slices.**
  Only the bitmap tells them apart.
- **When `row_count == 0`, every column is `NULLS`.** That makes the edge case
  where `STR` would need `offsets[1]` disappear.
- **The last-batch flag (bit0) is set only when the driver has exhausted the
  rows.** A batch that fills the requested maximum exactly has `flags = 0`, and
  the next `FETCH` returns 0 rows with bit0 set.
- **`payload_len` covers the bitmap and the value area together.**
- **The unit of a LOB's `size`** is octets for a binary LOB and characters for
  a character LOB. `0xFFFFFFFFFFFFFFFF` means the driver did not report a size.

#### Where type mapping took a judgment call

- **`BIT` splits on precision.** `≤1` becomes `BOOL`, anything larger becomes
  `BIN` — MySQL's `BIT(n)` returns a byte string.
- **`LONGVARCHAR`/`LONGVARBINARY` are inlined as `STR`/`BIN`, not `LOB`.**
  MySQL's `LONGTEXT` landing here is a known sharp edge.
- **`REAL` travels as `F64`.** A 32-bit float is widened to 64 bits, so unless
  Rust displays it at f32 precision, `0.1` shows up as `0.10000000149011612`.

### 4.7 Driver isolation

One child `URLClassLoader` per driver (parent = the bridge's own loader).
`DriverManager` is not used; `Class.forName(cls, true, child)` →
`Driver.connect(url, props)` is called directly. `DriverManager` is a global
registry, so when two drivers both claim the same URL prefix there is no
telling which one wins.

When `Driver.connect` returns `null`, the JDBC spec means "I do not understand
this URL". That is not an exception, so it is checked explicitly and reported
to the user as "the driver does not accept this URL" (jdbgen already does this).

Sessions that use the same driver share a loader. Building a new loader per
session repeats the driver's static initialization and leaks memory. Loaders
are cached keyed by their set of JAR paths, and closed when the last session
using them closes.

---

## 5. The Java bridge JAR

```
comart.rudbman.bridge/
├── Bridge.java          the sole JNI entry point. op dispatch + exception → error envelope
├── Registry.java        handle ↔ object table (AtomicLong issuance, ConcurrentHashMap)
├── Session.java         Connection + ClassLoader + keep-alive  (inherited from DBMeta)
├── Loaders.java         URLClassLoader cache
├── Cursor.java          Statement + ResultSet + batch encoder
├── codec/BatchWriter    the §4.6 encoder
├── meta/Describe.java   DatabaseMetaData lookups → JSON
├── meta/Ddl.java        DDL reconstruction (per dialect)
├── job/Jobs.java        shared frame for job threads, progress and cancellation (§6). Introduced in M4
├── job/ExtractJob.java  script extraction (§6, M4)
├── job/BackupJob.java   §6, M6
├── job/TransferJob.java §6, M6
├── template/            jdbgen's TemplateManager, inherited (§12.3 resolved — it stays in Java)
└── Json.java            Gson wrapper (§12.1 resolved — Gson is merged into the JAR)
```

Dependencies stay minimal. The heavier the bridge JAR gets, the heavier the
jlink image and the startup time get with it. The only candidate so far is one
JSON serializer.

`Session` **keeps** jdbgen `DBMeta`'s connection lock. The Rust worker already
serializes commands, but the keep-alive timer still runs concurrently.

---

## 6. The data plane — backup and DB-to-DB transfer (D4)

This is the most important design decision. **Row data never crosses the JNI
boundary.**

```
JOB_START { kind: "transfer", ... }   (handle = the source session)
   ↓
Java: source ResultSet → target PreparedStatement.addBatch → executeBatch
      the whole thing completes inside the JVM, on a separate job thread
   ↓
JOB_POLL → { state, rows_done, rows_skipped, rows_total, bytes, phase,
             errors[], eta_s }
```

Rust calls `JOB_POLL` periodically (around 200ms) and draws the progress bar.
Cancellation goes through `JOB_CANCEL`, which sets the job thread's interrupt
flag and calls `Statement.cancel()`.

The transfer spec (settled in M6 — `JOB_START` is called with the source
session handle):

```
JOB_START { kind: "transfer",
            source_sql: "SELECT …",            // the query to run on the source session
            target_session: <i64>,             // a handle issued by OPEN_SESSION
            target_table: {catalog?, schema?, name},
            mode: "insert|upsert|truncate_insert",
            batch_size: 500,                   // the addBatch → executeBatch unit
            commit_every: 10000,               // target commit interval in rows. 0 = once at the end
            column_map: [{from, to}],          // omitted = source result column names verbatim
            on_error: "abort|skip|log" }
```

Transfer semantics (settled in M6):

- **Locking**: the source and target session locks are taken **in ascending
  `Session.handle()` order** and held for the whole stream. Releasing them
  mid-stream breaks the source ResultSet and the target transaction. The same
  session on both sides (a transfer into itself) is safe through
  `ReentrantLock` re-entrancy. `EXECUTE` on those sessions waits for the
  duration, so the UI opens a second session under the same rule as
  extraction. `CLOSE_SESSION` first cancels any job that **uses that session on
  either side** — a job has to answer "do you use this session?" (looking only
  at the source makes closing the target session wait forever).
- **There are two things to cancel**: the source SELECT statement and the
  target batch statement are alive at once. Cancellation calls
  `Statement.cancel()` on both.
- **Transactions**: auto-commit is turned off on the target and a commit
  happens every `commit_every` rows, plus once on normal completion. The
  original auto-commit state is restored on exit. On failure or cancellation
  the uncommitted tail is rolled back, and **the rows already committed stay
  committed** — `rows_done` shows that fact.
- **`truncate_insert` empties the table with `DELETE FROM`.** TRUNCATE is a
  minefield of dialect, privilege and transactionality. DELETE means the same
  thing everywhere and rolls back in the same transaction.
- **upsert**: the conflict key is read from the target table's PK metadata. If
  there is no PK, `JOB_START` rejects synchronously. It branches by dialect —
  `ON CONFLICT … DO UPDATE` for PostgreSQL/SQLite, `ON DUPLICATE KEY UPDATE`
  for MySQL/MariaDB, `MERGE` for H2/Oracle/SQL Server/DB2. Any other (OTHER)
  dialect has no portable upsert, so it is rejected synchronously — better than
  quietly building a wrong statement.
- **`column_map`**: when omitted, the source result set's column names are used
  verbatim as target column names (quoted by the target dialect's rules).
  Malformed specs are rejected synchronously, but **errors that depend on the
  source result structure** (a map `from` that is not in the result, and the
  like) can only be known once the source query runs, so they are reported as
  a `failed` state early in execution.
- **`on_error`**: `abort` (the default) fails the job on the first row error.
  `skip` drops the row and counts it without recording it. `log` records the
  dropped row's error in `errors[]` — but **capped at 100** (past that it only
  counts; the `errors[]` of a job where a million rows all fail cannot come
  across JNI). Either way the number of dropped rows is reported as
  `rows_skipped` in the progress (always 0 for extraction and backup).
- **Binding uses `getObject`/`setObject`.** Type coercion is the target
  driver's business, and exotic types (arrays, vendor structs) failing to cross
  is a known edge — those rows take the `on_error` policy.
- **phase**: `"starting"` → `"transfer"` → `"done"`. `bytes` stays at 0 since
  there is no file. `rows_total` is `null` for the same reason as extraction.

Backup uses the same frame. `kind: "backup"` writes every table in a scope to
a file. Java does the file I/O too — on the side where the data is. The backup
spec (settled in M6):

```
JOB_START { kind: "backup",
            scope:    {catalog?, schema?},     // every TABLE-type table, name-sorted
            output:   {path, charset, newline},
            compress: "none|gzip",
            ddl:      {include, include_drop, constraints},
            data:     {include, insert_batch_rows} }
```

- A backup is **an extraction without object enumeration**: the `TABLE`-type
  tables in the scope are enumerated in name order and written by the same core
  as extraction (all CREATEs → all FK ALTERs, then INSERT scripts). Views and
  procedures are not written — the point is a replayable data backup.
- **The INSERT section alone is topologically sorted by FK** (cycles fall back
  to name order). Unlike DDL, data cannot be deferred with an ALTER — if name
  order puts `CHILD` before `PARENT`, the keys are already in place and the
  replay is rejected. Enumeration and DDL order stay alphabetical.
- The catalog written into the script is not the one the driver reported but
  **the request's `scope.catalog`.** H2 answers with the live database name,
  and using it nails the script to a database of that name and gets in the way
  of restoring.
- The data mode is **INSERT only**. Many tables go into one file, CSV has no
  table boundary, and a template means something different per table. That use
  case is already served by extraction.
- With `compress: "gzip"` the output stream is wrapped in gzip. Progress
  `bytes` is **bytes written to the file after compression** (it has to match
  the file size).
- The phase, cancellation and partial-file rules are the same as extraction.

**Script extraction (M4) is the first tenant of the same frame.** It is a job
where row data flows into a file, so per §12.3's conclusion it lives on the JVM
side together with the template engine:

```
JOB_START { kind: "extract",
            objects: [{catalog?, schema?, name}],
            output:  { path, charset: "UTF-8", newline: "\n|\r\n" },
            ddl:     { include: true|false, include_drop: false,
                       constraints: "inline|alter" },   // FKs are always pushed to the end as ALTERs
            data:    { include: true|false, mode: "insert|csv|template",
                       template_path?, insert_batch_rows: 1,
                       where?: "…" },                   // valid only for a single object
          } → { job }
```

- DDL applies `meta/Ddl` over the object list, but writes it in the order
  **all CREATEs → all FK ALTERs**. Schemas exist whose circular references
  cannot be untangled by creation order.
- `mode: "insert"` follows the dialect's identifier quoting and literal
  escaping, and with `insert_batch_rows > 1` it groups rows into multi-VALUES.
  `mode: "template"` runs rows through the `template/` engine inherited from
  jdbgen — template files come from `templates/` in the config directory (the
  built-in defaults are bridge resources).
- Progress and cancellation are `JOB_POLL`/`JOB_CANCEL`, exactly as for
  transfer.

Semantics the implementation settled (M4 — the contract the Rust side codes
against):

- **Handle lifetime**: the `JOB_POLL` that first reports a terminal state
  (`done|failed|cancelled`) unregisters the handle within that same call.
  Polls and cancels after that are `protocol` errors — once you have read a
  terminal state, stop polling. `CLOSE_SESSION` first cancels and releases that
  session's jobs (the job thread holds the connection lock, so without
  cancelling, closing is blocked).
- **Spec errors are synchronous**: a bad `objects`/`mode`/`charset` and the
  like are rejected by `JOB_START` immediately with an error envelope. They do
  not become a failed job you have to poll for.
- **Locking**: a running job holds the session's connection lock for the
  duration of each section (one table's stream). `EXECUTE` on the same session
  waits meanwhile, so a UI that needs to query during an extraction opens a
  second session.
- **Progress**: `phase` goes `"starting"` → `"ddl"` →
  `"data:<schema>.<table>"` → `"done"`. `rows_total` and `eta_s` are `null`
  (there is no COUNT). `bytes` lags by up to a buffer (≤64KB) while running and
  is exact at the end. Elements of `errors[]` are §4.4 error envelope objects.
- **`ddl.constraints`**: `"alter"` (the default) **forces the metadata
  reconstruction path** — pulling FKs out of native DDL (MySQL's
  `SHOW CREATE`) would require parsing vendor SQL, which is not done.
  `"inline"` prefers native, like display DDL does. In other words a replayable
  script accepts the known blind spots of reconstructed DDL (check constraints,
  storage clauses).
- **CSV**: NULL is an empty unquoted field, the empty string is `""`
  (PostgreSQL's `COPY … CSV` convention — the two are distinguishable). A
  column-name header row is written. Newlines inside values pass through, and
  `output.newline` applies only to the record terminator.
- **Literals**: dates and times are quoted strings (the `DATE '…'` form is
  rejected by SQL Server). Booleans are `1/0` on
  Oracle/SQL Server/SQLite/MySQL/MariaDB and `TRUE/FALSE` elsewhere. Binaries
  use per-dialect hex (`0x…`/`'\x…'`/`HEXTORAW`/`X'…'`). Oracle has no
  multi-VALUES, so `insert_batch_rows` is forced to 1 there.
- **`include_drop`**: all DROPs come first, in reverse order, with `IF EXISTS`
  only on dialects that support it (Oracle and DB2 excluded). Constraints are
  not dropped, so re-dropping a circular schema is a manual job.
- **The template model (per row)**: `table`/`schema`/`catalog`/`qualified`/
  `row_no`/`columns[]` (`name`/`value`/`literal`/`type_name`/`jdbc_type`), plus
  each column directly under its own name. `${a.b}` is not a nested path but a
  processor chain (jdbgen's rule).
- **Packaging note**: the template engine's EUC-KR padding width calculation is
  only correct when the jlink image contains the `jdk.charsets` module (without
  it, a heuristic fallback takes over).

The execute direction (script file → DB) needs no new operation. The file is
opened in the editor, run through `rudbman-sql`'s statement splitting, and
executed sequentially through the existing `EXECUTE` pipeline; per-statement
error reporting is already what the query pane's multi-result handling does.

`rows_total` is usually unknown. An up-front `COUNT(*)` is optional (a
checkbox); the default is an indeterminate progress bar plus a processed-row
count.

---

## 7. The UI layer

### 7.1 Window structure

logman's self-drawn title bar and `pane_tree` are inherited as they are.

```
┌ Title bar (tabs = connection sessions) ─────┬ Window buttons ┐
├─────────────┬────────────────────────────────────────────────┤
│ Explorer    │  pane_tree (splittable)                        │
│ tree        │  ┌────────────────────────────────┐            │
│             │  │ SQL editor                     │            │
│ Server      │  ├────────────────────────────────┤            │
│ └ Schema    │  │ Result grid / messages / plan  │            │
│   ├ Tables  │  └────────────────────────────────┘            │
│   ├ Views   │  or table detail / ERD canvas / query builder  │
│   └ Procs   │                                                │
├─────────────┴────────────────────────────────────────────────┤
│ Status bar (connection, transaction state, rows, elapsed)    │
└──────────────────────────────────────────────────────────────┘
```

The shape the implementation settled on (M3–M4): the work area is **a document
per connection**. Every connection tab along the top carries its own
`WorkArea` (pane tree, split ratios, active pane, query numbering), and
switching tabs swaps the entire document. Each pane holds not a single piece of
content but **a list of mini-tabs** (`PaneItem::TableDetail | Query | Erd |
QueryBuilder`), and activating the same object again moves to the open tab
instead of adding a new one. The explorer keeps tree data for every connection
but draws only the active connection's roots — switching is a pure filter, so
there is no round trip. **Closing** a connection tab tears the whole document
down (panes releasing their session handles and cursors is the cleanup path of
§9.3), and when a connection **drops**, the tab stays but query panes detach:
the SQL and the rows already received remain readable, only execution is
refused.

One discipline runs through this whole structure: gpui does not clean up focus
when the focused element leaves the render tree, and it resolves actions and
key bindings against the focused element of the **last frame it drew**. Every
path that hides or removes a subtree (sidebar toggle, closing a tab or pane,
switching connections) has to take focus back within the same update, or every
menu and shortcut afterwards is silently dropped.

### 7.2 The UI theme

logman's 11 color tokens (`background`, `surface`, `surface_hover`,
`surface_active`, `border`, `text`, `text_muted`, `accent`, `danger`,
`success`, `overlay`) are used unchanged. The file format, registry, editor and
user-directory loading are all inherited.

A few grid tokens are added: `grid_header`, `grid_row_alt`, `grid_selection`,
`grid_null` (the dimmed rendering of a NULL cell) and `grid_pk` (primary key
column emphasis). Their defaults derive from the existing tokens so that a
hand-written theme file that knows nothing about them still loads — exactly how
logman's `icon` slot works.

### 7.3 The editor theme (D9)

A **different file, a different directory and a different token set** from the
UI theme. `~/.config/rudbman/editor-themes/<id>.json`.

```json
{
  "version": 1,
  "name": "Tokyo Night",
  "dark": true,
  "colors": {
    "background": "#1a1b26",  "foreground": "#a9b1d6",
    "cursor": "#c0caf5",      "selection": "#33467c",
    "line_highlight": "#1f2335",
    "gutter": "#3b4261",      "gutter_active": "#737aa2",
    "keyword": "#bb9af7",     "string": "#9ece6a",
    "number": "#ff9e64",      "comment": "#565f89",
    "function": "#7aa2f7",    "type": "#2ac3de",
    "operator": "#89ddff",    "identifier": "#c0caf5",
    "punctuation": "#a9b1d6", "bracket_match": "#f7768e",
    "error": "#f7768e",       "warning": "#e0af68"
  }
}
```

Loading reuses `theme_store.rs`'s `load_dir` generic verbatim (a third
directory, differing only in format). The theme editor gets one more tab.

The UI theme and the editor theme are chosen independently, but a "follow the
UI theme" option in the settings prevents the accident of a light UI dragging a
dark editor along with it.

### 7.4 The SQL editor (`rudbman-editor`)

logman's `TextInput` is single-line only (it replaces `\n` with a space). This
is written fresh.

- Buffer: `ropey`. Editing stays O(log n) even with a 100MB script open
- Rendering: virtualized on gpui's `uniform_list`. Only the lines on screen get
  shaped
- Input: implements `EntityInputHandler`. **IME composition follows exactly
  what logman's `text_input.rs` already solved** — byte offset to UTF-16 offset
  conversion, and caret handling during composition
- Syntax highlighting: `rudbman-sql`'s lexer. Tree-sitter is not adopted (SQL
  grammar splits per dialect, and what highlighting needs is the token level)
- Features: line numbers, current-line highlight, bracket matching, multiple
  cursors, statement-at-cursor execution (detecting the statement under the
  caret), folding, find and replace, auto-indent, comment toggling
- Completion: suggests tables, columns and aliases from the connected session's
  schema index. The index is filled in the background right after connecting
  and kept in memory

### 7.5 The result grid (`rudbman-grid`)

- `uniform_list` virtualization plus horizontal virtualization (tables with
  hundreds of columns exist)
- Column resize, pin, hide and reorder; sorting is a server round trip
  (re-running with `ORDER BY`)
- Cell selection, range selection, copy (TSV/CSV/INSERT statements/JSON)
- NULL and the empty string are visually distinguished. Far too many tools
  cannot do this
- Cell editing lives in the table data pane (§7.9), which is the one place the
  single table and its primary key are known. The query result grid stays
  read-only: it carries no key metadata and no table to attribute a column to,
  and editing one is deferred
- A LOB cell shows only its size and loads in chunks in a viewer when clicked
- Infinite scroll: hitting the bottom fetches another `FETCH` batch. Default
  batch size 500 rows

### 7.6 ERD (`rudbman-erd`)

- Model: a graph over the results of `DESCRIBE imported_keys`. exported_keys is
  only the same edge queried in the other direction, so it is not called. FKs
  are collected with one `imported_keys` call per table (JDBC has no
  schema-wide bulk lookup). Columns take one call per schema. Edges pointing at
  tables outside the scope are not drawn
- Layout: grid placement by default, manual dragging with saved positions, and
  a hand-written Sugiyama for auto-layout (§12.4 resolved). All of it is pure
  modules — tested without a window
- Rendering: gpui `canvas`. Entity boxes, orthogonally routed relationship
  lines and cardinality notation. Hit testing is arithmetic, not listeners
  (the same call as in rudbman-grid)
- Widget/pane separation: `rudbman-erd`'s `ErdView` is a widget that knows only
  drawing, dragging, zooming and panning, while loading state, the toolbar,
  i18n and persistence are wrapped by `rudbman-app`'s `ErdPane` — the same
  discipline as `GridView`/`QueryPane`. It emits a `LayoutChanged` event when a
  drag ends, and saving is the host's business
- Layout persistence: `erd/<profile-uuid>.json` (§8). Table name → position,
  per scope (catalog and schema). Written once per drag gesture — not per event
- Entry point: select a node that has a scope in the explorer and use the
  `OpenErd` action (wired the same way as `ExtractScript`). Opening the same
  scope again moves to the open tab
- Export: SVG is generated directly (gpui's offscreen render path is rough
  going). It uses the current theme's colors. PNG goes through SVG or waits
- The canvas, drag, zoom and pan code is shared with the query builder (§7.7)

### 7.7 The query builder

Drop tables on the ERD canvas, draw joins, and SQL comes out. Column selection,
WHERE condition rows, GROUP BY and ordering are edited in a form, and the
result is sent to the editor. The reverse direction (SQL → builder) is not
done — the parser complexity would exceed the entire rest of the tool.

The implemented shape (settled in M7):

- **Sharing the canvas means sharing code.** `rudbman-erd` extracts the
  viewport (pan, zoom, coordinate transforms), gestures and box render assembly
  into an internal `canvas` module, and a second widget, `BuilderView`, stands
  on top of it. The ERD side's behavior and SVG output must remain
  byte-identical. The newly needed geometry is per-column: a row's y
  coordinate, the reverse y→row hit test, the row anchor a join line attaches
  to, and a generalization of `route` that takes anchor y values.
- **The view is a projection; the state belongs to the host.** `BuilderView`
  receives a table list (reusing `ErdTable`), a set of selected columns and
  join edges, draws them, and hands gestures back as events: clicking a column
  → `ColumnToggled`, dragging from column to column → `JoinDrawn`, moving a box
  → `LayoutChanged`. Editing a join's type and deleting it happen in the row
  list of the panel below, not on the canvas — simpler than hit-testing line
  clicks, and testable without a window.
- **A builder document is a per-connection pane**: `PaneItem::QueryBuilder`
  (numbered like a query pane, several allowed). Tables get onto it either by
  selecting one in the explorer and using the "add to builder" action or by
  **dragging the row onto the builder canvas** (same gate, same load path — a
  drop goes into whichever builder the pointer is over). The column list is
  filled with a single `DESCRIBE columns` round trip. Adding the same table
  twice attaches a `name_2` alias (self-joins). Builder state is not persisted —
  the product is SQL, and SQL is already preserved by the editor and by files.
- **The form**: the join list (type INNER/LEFT/RIGHT/FULL plus delete), WHERE
  condition rows (free text, ANDed together), GROUP BY (a toggle per selected
  column), ORDER BY (none/ASC/DESC per selected column). The SQL preview always
  reflects the current state, and "open in editor" opens a new query pane
  through the existing `open_query` gate. Execution, cancellation and results
  are handled by that pane's existing pipeline.
- **Identifier quoting is done by a new `rudbman-sql` API** that quotes only
  when it must: non-identifier characters, a leading digit, a keyword clash, or
  a name that disagrees with the dialect's unquoted case folding (Oracle and H2
  upper, PostgreSQL lower, MySQL/SQL Server/SQLite preserving). The quote
  character is a backtick on the MySQL family and a double quote elsewhere. The
  always-quoting helper and the unquoted `qualified()` SQL assembly that used to
  live in the app converge on this API.

### 7.8 Context menus

The right-click menu exists on every major surface: explorer tree rows,
connection tabs, pane mini-tabs, the result grid (cells and headers
separately), the SQL editor, the ERD and builder canvases, and the welcome
list. The discipline:

- **Widgets detect the right click and emit an event, nothing more** (window
  coordinates, plus hit information). Rendering the menu, its labels and
  running the commands are the host's business — an extension of the standing
  rule that the widget layer holds no strings. Menu state (open, coordinates)
  is owned by the host view that receives the event.
- Presentation comes from `rudbman-ui`'s `ContextMenu` (deferred + anchored,
  anchored to the pointer and snapped inside the window). Items support
  disabled and checked states, and the width follows the content.
- **A right click moves the selection but does not select tabs**: tree rows and
  the grid move the selection to the right-clicked target (the grid keeps it if
  the click lands inside the existing selection), while tab strips leave the
  selection alone (the menu differs for active and inactive tabs — a decision
  documented on TabBar).
- `Escape` (DismissDialog) closes an open context menu **first of all** — ahead
  even of the app dropdown. Context menus and the app dropdown are mutually
  exclusive.
- The menu items are everything that surface can do: what already exists as an
  action or a public API (the four copy formats, sorting, column hide and
  auto-fit, edit and execute, zoom/arrange/export, the explorer's five actions)
  is exposed, and what only the menu needs (close other tabs, close tabs to the
  right, remove a builder table, show all columns) is added to that surface's
  API.

### 7.9 Table data editing

Reading a table's rows, and changing them in place, is **a pane of its own** —
`PaneItem::TableData`, a sibling of the query pane — rather than a fifth tab of
the detail panel. The detail panel is one load and one refresh of presentation:
it fetches everything it shows the moment it opens, holds no cursor, and owns
nothing of the session (§7.5's grid is where rows live). A surface that pages a
result set, re-runs it to sort it, keeps edits the user has not applied yet and
then runs a transaction is none of those things, and hanging it off the panel
would give every metadata tab the lifetime of an open `ResultSet`. The ways in
are the explorer's "view data" row on a table or a view and the same row on the
detail panel's header; opening the same object twice moves to the tab already
open, exactly as activating an object does.

The rows arrive through the pipeline that already exists: `SELECT * FROM
<qualified name>` at the settings' fetch size, paged on the grid's `NearEnd`
event by the same cursor walk the query pane uses — which is why that walk lives
in `query_source` rather than in `query`. Every identifier is written by
`rudbman-sql`'s quoting API (§7.7) and never by hand. Sorting appends an
`ORDER BY` instead of wrapping the statement in a derived table the way the
query pane must: this statement is the pane's own, so there is no `ORDER BY`
underneath for a second one to collide with.

- **Staging is keyed by the base row index.** The source under the grid is
  append-only — a page adds a batch and nothing already in it moves — so a row's
  index is stable for as long as that source is, and an edit is recorded as
  (row, column) → value *beside* the rows rather than written into them. Rows
  the user inserts are a list of their own after the last fetched row, and a
  deleted row is a marker rather than a hole: the grid goes on drawing it,
  struck through, until the change is applied or discarded.
- **A sort or a refresh replaces that source wholesale**, which is the one thing
  those indices cannot survive, so both ask first while anything is staged.
  Edits are not carried across a reload by primary key: an edit that comes back
  attached to a different row than the one it was typed on is worse than being
  asked to apply or discard.
- **Staleness is treated optimistically, and the update count is the guard.**
  The `WHERE` clause of a generated `UPDATE` or `DELETE` names the primary key
  and nothing else. The alternative — repeating every column the row was read
  with — cannot be written for a NULL or a LOB and is a different statement on
  every product. What makes the short clause safe is that each statement is
  checked as it runs: an `UPDATE` or a `DELETE` whose update count is not
  exactly 1 has reached a row somebody else has already moved, and the whole
  apply is abandoned. That case is said in a line of its own, apart from any
  driver's refusal, because nothing was written wrongly — the row simply is not
  the row that was read, and the answer is to refresh.
- **The apply is one transaction**, over the session calls that already exist:
  `set_auto_commit(false)`, one `execute` per statement, then `commit`, then
  autocommit back to whatever the profile opened the session with — *restored*
  rather than set to `true`, since a profile with `auto_commit` off asked for a
  connection that stays in a transaction. Any failure — a driver error, or a
  count that is not 1 — rolls back **before** autocommit is restored, because
  putting autocommit back first is what commits the half-applied batch on
  several products. A product whose `SESSION_INFO` reports no transaction
  support runs the same statements under autocommit with the same count checks:
  what is missing is the undo, and the confirmation says so before the user
  reaches it. Success clears the staging buffer and re-runs the `SELECT` in full
  — which is how a generated key and a trigger's work become visible — while a
  failure leaves every staged change exactly where it was.
- **Every statement is shown before any of them runs**, each over the values its
  `?`s will take. That preview *is* the write confirmation: it is always raised,
  which satisfies the profile's `confirm_writes` (§8) by superset, and it is
  deliberately literal. The question worth asking before a write is not "are you
  sure" but "is this what you meant", and only the `WHERE` clause can answer it.
- **Statements are generated with bind parameters** by a new `rudbman-sql::dml`
  module: one typed value per column (§4.4's `params`, with `decimal`, `date`,
  `time`, `timestamp` and `bytes` in their typed form), never a literal spliced
  into the text. The bridge's `Literals.java` already writes literals for the
  extract and backup jobs; a second copy of that judgement in Rust would be a
  second place for a quoting bug to live, and the wire already carries the form
  that does not ask the question. Which form a column takes is resolved once from
  its `java.sql.Types` constant, and conservatively: anything not in the table is
  bound as text, and anything numeric that is not an integer is a decimal, since
  routing a typed `0.1` through a double would round it on the way out.
- **A value is checked against its column's bind form before anything is
  planned**, so that a refusal can name the column: by the time a batch exists a
  value is a `?` in a string, and "the third parameter of statement two" is not
  something to show anybody. Only whether the text can *become* the bound form
  is checked — an integer that parses, hex of an even length. Whether the server
  will accept it is the server's judgement, and its refusal says more than a
  guess made here would.
- **A table with no primary key stays read-only**, and says so in one line above
  the grid rather than by refusing a keystroke later: with no key there is no
  `WHERE` clause that names exactly one row. A profile marked `read_only` (§8)
  refuses in the same words and in the same place. A view is browsed like a
  table and edited only where the driver reports a key for it.

#### Editing a query result

A `SELECT` somebody wrote has no single table behind it in general, which is
why the pane above is the one that edits. But the cases that *do* have one are
recognisable from metadata the wire already carries — `ColumnInfo` holds a
`table`, `schema` and `catalog` per column — and a result grid that can fix the
row it is showing is worth having. So a query result becomes editable, under a
gate with three clauses, and the machinery underneath is the data pane's own,
**moved rather than copied**.

- **The gate.** Every column that names a source table must name the *same*
  one; at least one must; and the table's primary key must be present in the
  result, every column of it. Each clause earns its place. One table, because an
  `UPDATE` names one. The key in full, because the `WHERE` is built out of the
  row's own values and a key column that was never selected has no value to be
  found by — which is also what quietly disqualifies most aggregates, since
  `SELECT dept, COUNT(*) FROM emp GROUP BY dept` names `emp` in one column but
  does not carry `emp`'s key. A column that names no table is **read-only rather
  than disqualifying**: refusing the whole result because one column was
  computed would refuse `SELECT id, name, name || '!' FROM users`, where the
  first two are perfectly writable.
- **The metadata is a hint, and is allowed to be.** JDBC answers the *empty
  string*, not null, for a column with no source table, so both spellings mean
  "unknown" and a filter for them is the first thing any of this does. Beyond
  that the answer is a particular driver's rather than a fact: several return
  `""` for schema and catalog unconditionally, and MySQL can report an alias
  where the table was asked for. None of that has to be trusted, because every
  way out of a wrong hint is a refusal — a table name that is wrong finds no
  primary key and the result stays read-only, and a name that is right over rows
  that are not is caught by the update count of exactly 1 that every generated
  `UPDATE` and `DELETE` is already checked against. The hint is only ever
  allowed to *offer* editing, never to make a statement safe.
- **Updates and deletes only; no inserts.** A result carries the columns the
  user selected, not the columns the table requires, so a row typed into a
  `SELECT id, name FROM users` is missing every `NOT NULL` column that was not
  selected and is refused by the server every time. And a row that did insert
  need not satisfy the query's own `WHERE`, so the reload would not show it —
  an apply that looks as though it failed. Inserting is what the data pane on
  that table is for; this is the surface for changing rows you are already
  looking at.
- **One copy of the apply.** The staging buffer, the planner, the batch, the
  transaction, the preview that is the confirmation and the update-count guard
  are all §7.9's, and they now live where both panes reach them rather than
  being written twice. The transaction ordering in particular — the rollback
  *before* autocommit is restored, which is what stops several products
  committing the half-applied batch — is the last thing this codebase should
  hold two copies of.
- **A sort or a re-run replaces the source**, exactly as above, and the query
  pane's sort wraps the statement in a derived table rather than appending an
  `ORDER BY`, so it re-runs. The rule is unchanged: ask first while anything is
  staged, and never carry an edit across by key.

### 7.10 Structure editing (`ALTER TABLE`)

Changing a table's *shape* — adding a column, retyping one, dropping a
constraint — is a second generator (`rudbman-sql::ddl`, a sibling of §7.9's
`dml`) and a second surface. It is not an extension of the row editor, and the
reason is that almost nothing carries over. Row editing has one statement
grammar that every product spells the same way and a value model the wire
already carries; structure editing has six grammars that disagree on every
clause, no bind parameters anywhere, and no undo on two of the six.

- **A statement is a string, and there are no parameters.** No server accepts a
  `?` in a DDL statement — not for a type, not for a default, not for a name —
  so `plan_alter` returns `Vec<String>` where `plan_edits` returns SQL plus
  values. §7.9's rule that a value is never spliced into text does not lapse
  here so much as it does not apply: there are no values, only names (through
  `Dialect::quote_ident`, as everywhere) and fragments the user wrote.
- **Types and defaults are the user's own SQL, passed through unread.** This
  crate has no type model and is not getting one: `VARCHAR2(30)`,
  `character varying(30)`, `NVARCHAR(30)` and `TEXT` are four products' answers
  to one question, and a mapping table between them would be a guess made in
  the one place that cannot see the server. So a column's type is a string the
  user typed into a field pre-filled from the catalog, a default is a string in
  the same shape, and the generator's whole contribution is deciding which
  clause they land in. What makes that safe is the same thing that makes §7.9's
  short `WHERE` clause safe — the batch is shown in full before any of it runs,
  and a type nobody can parse is visible there.
- **The input is a diff that carries both sides**, not a target state: every
  changed column arrives as the definition that was read *and* the definition
  that is wanted. Two dialects need the old side. MySQL's `MODIFY COLUMN` and
  `CHANGE COLUMN` restate the entire definition, so a change of type that did
  not also restate `NOT NULL` would quietly drop it; SQL Server's `ALTER COLUMN`
  restates the type even when only nullability changed, and — the trap — resets
  the column to nullable when the clause is omitted. The old side is also what
  lets a column rename be spelled `CHANGE a b <definition>` on MySQL, which is
  the form that works before 8.0 as well as after, since the client cannot see
  the server's version.
- **Where the products differ is a table, not a pile of branches.** Four
  families: the standard one (PostgreSQL, H2, generic) with an independent
  clause per attribute — `SET DATA TYPE`, `SET`/`DROP NOT NULL`,
  `SET`/`DROP DEFAULT`; MySQL, which restates the definition; Oracle's
  `MODIFY (...)`, which restates *only what changed*, because naming `NOT NULL`
  on a column that already has it is ORA-01442; and SQL Server, whose
  `ALTER COLUMN` carries type and nullability together. Even the spellings that
  look universal are not — `ADD COLUMN` is a syntax error on Oracle and SQL
  Server, which want a bare `ADD`, and the order of `DEFAULT` and `NOT NULL`
  inside one definition is a per-dialect field rather than a constant, since
  Oracle takes the default first and MySQL documents the reverse.
- **A refusal names the product and the reason.** SQLite can add, drop and
  rename a column and rename a table, and has no `ALTER` for anything else: a
  type change there is a table rebuild — new table, copy, drop, rename — which
  is a data-moving operation wearing a schema operation's clothes, and it is not
  what a user who typed a new type asked for. SQL Server keeps defaults as named
  constraints rather than column attributes, so changing one is a drop and an
  add of a constraint whose name JDBC's `getColumns` does not report. Both are
  refused, in a line that says which product and why, before anything is
  planned. A generated statement that fails on the server would say less: the
  driver's message names a syntax error, not the fact that the product cannot do
  this at all.
- **Dropping a constraint needs to know its kind.** Everywhere but MySQL it is
  `DROP CONSTRAINT <name>`; MySQL has no generic form and spells each kind
  separately (`DROP PRIMARY KEY`, `DROP FOREIGN KEY`, `DROP INDEX`,
  `DROP CHECK`). The kind travels with the name, which costs the caller nothing:
  the detail panel's keys and references tabs already know which is which.
- **Order within a batch**: constraint drops, then column adds, then column
  changes, then column drops, then the table rename. Constraints go first
  because one naming a column blocks that column's drop; the renames go last —
  both a column's and the table's — so that every statement before them names
  its target the way the catalog still holds it, which is the same rule twice.
- **No transaction is pretended.** MySQL and Oracle commit implicitly at every
  DDL statement, so a batch cannot be rolled back there, and wrapping one in
  `setAutoCommit(false)` would produce a rollback that silently does nothing on
  a third of the products rudbman supports. The batch therefore runs under
  autocommit on every product alike, stops at the first failure, and reports
  **how many statements were committed before it stopped** — which is a fact the
  user can act on, where "the transaction was rolled back" would have been a
  guess. §7.9's model is the opposite one for the opposite reason: DML is
  transactional everywhere, so there a rollback is a promise that can be kept.
- **The surface is a pane of its own** — `PaneItem::TableStruct`, a sibling of
  `TableData` — and the detail panel stays read-only. The panel is one load and
  one refresh of presentation, and it keeps its rows as display strings: a
  column's type, size and scale are folded into `VARCHAR(255)` and its
  nullability into the word `NOT NULL` by the time they are stored, so nothing
  an editor needs survives in it. The structure pane issues its own
  `DESCRIBE columns` / `primary_keys` / `imported_keys` and keeps what it reads.
  The way in is the detail header and the explorer row, beside "view data".
- **Success reloads rather than patches.** What a server did with a DDL
  statement is not always what it was asked — a type widened, a default
  normalized, a column added at the end regardless of where it was typed — so
  the pane re-reads the catalog and shows that, exactly as §7.9's apply re-runs
  its `SELECT`.
- **A failure reloads too, and discards what was staged**, which is where this
  parts company with §7.9. There a failed apply leaves every staged change
  where it was, because the rollback means nothing was written; here the
  statements before the one that failed are committed and cannot be taken back,
  so the catalog has moved and the staging — keyed, like the row editor's, to
  indices into the reading it was staged against — is describing a table that no
  longer exists in that shape. Replaying it would apply the committed half
  twice. What the user is told instead is exactly how far the batch got: which
  statement failed, the driver's reason, and how many before it were committed.
  The whole batch was on screen a moment earlier, so that number locates the
  work still to do.

#### Creating a table

A table being created is a table whose current shape is empty, so it is **the
same pane in a second mode** rather than a surface of its own. The column list,
the one-column-at-a-time form, the live batch preview and the apply are already
what a person filling in a new table wants, and duplicating them into a dialog
would be duplicating the largest part of the pane to change the verb. The way in
is the explorer's context menu on a schema — the place a table would appear —
and the pane opens with nothing staged but its name field focused.

- **The generator is a second entry point, not a special case of the first.**
  `plan_create` takes a `TableCreate` and answers with one statement. Sharing
  `ColumnDef` with `plan_alter` is the whole of what they have in common: an
  `ALTER` is a batch of amendments to something that exists and has to be
  ordered against itself, and a `CREATE` is one sentence.
- **This is where constraints are born, and the only place.** `plan_alter`
  drops constraints and never adds one, so without table-level clauses here a
  table made by rudbman could never be given a key or a reference at all. The
  create path therefore carries `PRIMARY KEY`, `UNIQUE` and `FOREIGN KEY`
  clauses. They are also the easy half of the problem: unlike `ALTER`, whose
  every clause is spelled differently per product, a table-level constraint in
  a `CREATE TABLE` is standard everywhere rudbman speaks.
- **Auto-increment is not modelled.** It is spelled at least five ways —
  `SERIAL`, `AUTO_INCREMENT` after the type, `IDENTITY`, `GENERATED BY DEFAULT
  AS IDENTITY`, `AUTOINCREMENT` — and some of those are a type, some a column
  attribute and some a clause. Since a column's type here is already the user's
  own SQL passed through unread, all five reach the server exactly as typed and
  the module needs to know about none of them. That is the same rule that keeps
  a type table out of §7.10 generally, applied where it would have been most
  tempting to break it.
- **No `IF NOT EXISTS`.** Oracle has no such form, so it would be a per-product
  refusal for a clause whose whole purpose is to say nothing when it matters.
  A table that already exists is a message the server phrases better than a
  statement that silently did nothing.
- **Success turns the pane into the editor for the table it just made.** The
  reload that follows every apply (above) finds a table where there was none,
  and the pane goes on as the structure editor for it. That is also what proves
  the server accepted the shape rather than a shape near it, and it means the
  common sequence — create a table, then notice a column needs a default — is
  one surface and no re-navigation.
- **A refused create keeps what was typed**, which is the one place the discard
  rule above does not reach — and it does not reach it for the same reason it
  exists. That rule discards because the statements before the failure are
  committed and the snapshot's indices no longer describe anything. A create is
  one statement: when it is refused nothing was committed, the snapshot is the
  empty one it always was, and the staging was never keyed to it — a table
  being defined is all additions. So a mistyped type in a thirty-column
  definition costs the line it is on rather than the definition. The rule is
  about what a committed statement invalidates, not about failure as such.

---

## 8. Settings, profiles and secrets

logman-core's structure is inherited and only the schema changes.

```
~/.config/rudbman/            (macOS: ~/Library/Application Support/rudbman)
├── settings.json             app settings
├── connections.json          connection profiles (passwords excluded)
├── drivers.json              driver definitions
├── themes/*.json             UI themes
├── editor-themes/*.json      editor themes
├── snippets/*.sql            user SQL snippets
├── erd/<uuid>.json           ERD layout (per connection profile, per scope table positions)
├── history.db                query execution history (SQLite? open, §12)
└── drivers/                  downloaded driver JARs
```

A connection profile:

```json
{
  "id": "uuid",  "name": "Production Oracle",  "folder": "Production",
  "color": "#e06c75",
  "driver_id": "oracle-thin",
  "url": "jdbc:oracle:thin:@//host:1521/ORCLPDB",
  "username": "app",
  "props": { "oracle.jdbc.ReadTimeout": "30000" },
  "keep_alive": { "enabled": true, "interval_s": 300, "query": "select 1 from dual" },
  "read_only": false,
  "auto_commit": true,
  "confirm_writes": true
}
```

Passwords are **never stored in a file.** They go into the `keyring`
(logman-core's `secrets.rs`) under `rudbman:<profile-id>`.

`read_only` and `confirm_writes` guard against accidents on production
databases. With `read_only`, the session is opened with
`Connection.setReadOnly(true)` and DDL/DML is blocked before it runs.

A driver definition:

```json
{
  "id": "oracle-thin",  "name": "Oracle Thin",  "icon": "oracle",
  "class": "oracle.jdbc.OracleDriver",
  "jars": ["~/.config/rudbman/drivers/ojdbc11-23.4.0.24.05.jar"],
  "maven": "com.oracle.database.jdbc:ojdbc11:23.4.0.24.05",
  "url_template": "jdbc:oracle:thin:@//{host}:{port}/{service}",
  "default_port": 1521,
  "dialect": "oracle"
}
```

`dialect` selects `rudbman-sql`'s keyword set, identifier quoting rules, DDL
generator and paging syntax (`ROWNUM` vs `LIMIT` vs `FETCH FIRST`).

---

## 9. SSH tunnels (`rudbman-ssh`)

Production databases usually sit behind a bastion host. A connection profile
optionally carries a tunnel, and JDBC attaches to the local port that results.

### 9.1 What is inherited from logman-ssh and what is written fresh

**Inherited**: `SshConfig`/`SshAuth` (password, key and agent auth, with
secrets auto-masked in `Debug`), the dedicated thread plus own Tokio runtime
structure (the design that lets the GUI thread hold a handle safely), the
`HostKeyVerifier` trait and `known_hosts` storage, the `SshEvent` stream, and
the russh configuration (ring backend, keepalive, connection timeout).

**Written fresh**: logman-ssh only opens shell channels and SFTP through
`channel_open_session`. There is no port forwarding. What is needed:

- `channel_open_direct_tcpip(remote_host, remote_port, origin_host, origin_port)`
- A local `TcpListener` accept loop. Each accept opens one channel and copies
  bidirectionally
- A connection path that requests no PTY. A tunnel does not need a shell, and
  bastions that allow forwarding only, with shell-less accounts, are common
- Channel multiplexing: when a connection pool opens several sockets at once,
  that means several channels

`sftp.rs` is not brought over.

### 9.2 The profile schema

A connection profile (§8) gains an optional `tunnel` block:

```json
"tunnel": {
  "enabled": true,
  "host": "bastion.example.com", "port": 22,
  "username": "ops",
  "auth": "agent | key | password",
  "key_path": "~/.ssh/id_ed25519",
  "remote_host": "db.internal", "remote_port": 5432,
  "local_port": 0
}
```

With `local_port: 0` the OS picks a free port — that is the default. A fixed
port collides as soon as two sessions ask for the same one. The port that was
actually bound is substituted into the `{host}:{port}` slot of the JDBC URL.

The tunnel's password and key passphrase also go into the `keyring`
(`rudbman:<profile-id>:tunnel`).

### 9.3 Lifetime management

The tunnel and the JDBC session are two resources that know nothing about each
other, and `rudbman-app` is responsible for the order.

```
connect    → establish tunnel → read the bound port → substitute the URL → OPEN_SESSION
disconnect → CLOSE_SESSION → tear the tunnel down
```

- **The tunnel stands up first and lies down last.** If the tunnel closes while
  a JDBC session is still alive, the driver sees nothing but an unexplained
  socket error
- Several sessions share one tunnel (same bastion, same target). It is
  reference counted and torn down when the last session closes
- If the tunnel drops mid-flight, a `kind: "io"` error is propagated to every
  JDBC session over it and those sessions are marked dead. It never silently
  reconnects — there may have been a transaction in flight, and the user needs
  to know
- If the host key has never been seen before, the fingerprint is shown and
  confirmation is required. logman's `verifier.rs` dialog is used as is

---

## 10. Build and packaging

### 10.1 The bridge JAR

Gradle builds `bridge/` into `bridge/build/libs/rudbman-bridge.jar`.

`cargo build` does not invoke Gradle. `rudbman-jdbc/build.rs` only checks that
the JAR exists and, if it does not, produces a clear error telling you to run
`./gradlew :bridge:jar`. Having a JVM start every time you fix a line of Rust
would be intolerable.

The full build is orchestrated in one place, `just`/`xtask`:
`gradlew :bridge:jar` → `jlink` → `cargo build --release` → packaging.

### 10.2 The jlink runtime

```
jlink --add-modules \
    java.base,java.sql,java.sql.rowset,java.naming,java.transaction.xa,\
    java.security.jgss,java.security.sasl,java.management,java.logging,\
    jdk.charsets,jdk.crypto.ec,jdk.crypto.cryptoki,jdk.unsupported,jdk.net \
    --strip-debug --no-header-files --no-man-pages --compress=2 \
    --output runtime/
```

`--compress=2` is JDK 17's spelling (zip compression). The `zip-6` syntax
arrived in JDK 21 and errors out on the JDK 17 the release pins.

- `jdk.unsupported` is **required**. A great many drivers use `sun.misc.Unsafe`
- `jdk.charsets` is **required** too. The template engine's EUC-KR padding
  width calculation needs the extended charsets (§6, packaging note) — without
  it the code silently falls back to a heuristic and fixed-width Korean output
  comes out misaligned
- `java.naming` is JNDI/LDAP authentication
- `java.security.jgss`/`sasl` are Kerberos integrated authentication
- `java.transaction.xa` is referenced at load time by XA-capable drivers

A missing module only shows up as a `NoClassDefFoundError` at connect time.
The module list is verified by a smoke test per driver.

Target output size: around 50MB per platform.

### 10.3 Distribution shapes

logman's `packaging/` is inherited. Linux gets a tar plus `install.sh` plus a
`.desktop` file, macOS gets an `.app` bundle (`Contents/runtime/`), and Windows
gets a zipped folder — the executable is the launcher.

The archive layout is dictated by §4.1's search order. Relative to the
executable, the bridge JAR goes to `lib/rudbman-bridge.jar` (`<exe_dir>/lib`,
or `<exe_dir>/../lib` inside the macOS bundle) and the runtime goes beside it
(`<exe_dir>/runtime`, or `<exe_dir>/../runtime` on macOS):

```
rudbman-vX.Y.Z-<target>/          # Windows and Linux
├── rudbman(.exe)
├── lib/rudbman-bridge.jar
├── runtime/                      # jlink output (§10.2)
└── README.md (+ Linux: install.sh, .desktop, icons/)

rudbman.app/                      # macOS
└── Contents/
    ├── MacOS/rudbman
    ├── lib/rudbman-bridge.jar
    ├── runtime/
    ├── Resources/rudbman.icns
    └── Info.plist
```

On macOS the JAR must not sit under `Contents/MacOS/`: everything in that
directory is nested code to `codesign`, and sealing the bundle fails on an
unsigned JAR. `Contents/lib/` is sealed as a plain resource instead — the
first v0.1.0 release run failed exactly this way.

Unlike logman's, the Linux `install.sh` installs not a single binary but **the
whole tree** into `~/.local/share/rudbman/`, then creates a `~/.local/bin/rudbman`
symlink. Because `current_exe()` resolves symlinks to their real path, the
relative search for the runtime and the JAR keeps working.

CI is a three-platform matrix. Each job runs JDK setup → Gradle → jlink →
cargo, in that order. The release job packages and then smoke-checks that
`runtime/bin/java --list-modules` contains `jdk.charsets` and
`jdk.unsupported`, and that the JAR and the executable are in their places —
because a missing module does not surface until connect time (§10.2).

### 10.4 Branches and releases

logman's flow is followed exactly.

- Work happens on `dev`. `main` moves **only through a merge commit** via
  `gh pr merge --merge`, after CI has passed on all three platforms
- A release is cut by pushing an annotated tag `vX.Y.Z` pointing at a merge
  commit on `main`. `.github/workflows/release.yml` builds the artifacts and
  the GitHub release from that tag
- Before pushing the tag, bump `[workspace.package] version` and `Cargo.lock`
  (`cargo update --workspace`) in a separate `chore:` commit
- **The release notes are the body of the annotated tag** (`release.yml` reads
  `%(contents:body)`). The subject is `rudbman vX.Y.Z`; the body is a one-line
  introduction followed by **one bullet per user-visible change**. Prose
  paragraphs become a wall on the release page. Write from the user's point of
  view, not the implementation's, and wrap at 72 columns
- Folding a late change into an already-published release: merge to `main` →
  delete the remote tag → re-create the tag on the new merge commit → push.
  Note that the release action **replaces only the artifacts of an existing
  release and leaves the body alone.** After a re-release, the notes have to be
  pushed by hand with `gh release edit vX.Y.Z --notes-file <file>`, and any
  leftover draft has to be found and deleted

`.github/workflows/ci.yml` and `release.yml` are taken from logman with the
Gradle and jlink steps added.

---

## 11. Milestones

Status (2026-08-06): **every planned milestone, M0 through M7, is complete and
merged**, and the first release, v0.1.0, is tagged. Handoff across sessions is
the job of [status.md](status.md).

| | Scope | Done when |
|---|---|---|
| **M0** | Workspace, vendored gpui, `rudbman-ui`/`rudbman-core` ported, theme/settings/i18n | An empty window opens, theme switching and settings persistence work. `cargo test` passes |
| **M1** | Minimal bridge JAR, JVM bootstrap, session workers, driver manager, connection dialog, **SSH tunnel** | Connect/disconnect/PING round trips on H2, PostgreSQL and MySQL. The error envelope shows up in the UI. PostgreSQL over a bastion |
| **M2** | Explorer tree, every kind of `DESCRIBE`, table detail (columns, keys, indexes, FKs, DDL) | Schema browsing and DDL display on all three databases |
| **M3** | `rudbman-sql`, `rudbman-editor`, `rudbman-grid`, execution/cancellation/multiple results | A million-row result scrolls without a stutter. Cancelling mid-execution works. IME input behaves |
| **M4** | Script extraction (DDL/DML), script execution, the template engine | Table → CREATE/INSERT script, running a script file with error reporting |
| **M5** | ERD model, layout, canvas, SVG export | An ERD of a schema with FKs, saved drag layout, SVG output |
| **M6** | Backup, DB-to-DB transfer (§6) | A million rows moved from PostgreSQL to MySQL, with progress, cancellation and error reporting |
| **M7** | The visual query builder | A three-table join query built in the GUI and executed |

M3 is somewhere around 40% of the total work. M0 through M3 is the minimum bar
for "a usable tool".

### Testing strategy

- `rudbman-jdbc` does not depend on gpui, so pure integration tests are
  possible. **H2 in-memory is the reference database** — jdbgen already has a
  `sample_h2.db.mv.db`
- The codec gets round-trip property tests across the Java encoder and the Rust
  decoder
- Widgets use `gpui/test-support`'s headless platform, as in logman
- PostgreSQL, MySQL and Oracle get optional container-backed tests locally
  (excluded from CI by default)

---

## 12. Open questions

1. **The JSON library in the bridge JAR** — **resolved (M1): Gson.** A proven
   230KB is cheaper than hand-writing escaping and introducing bugs with it.
   It is merged into the JAR and ships with it.
2. **Query history storage** — SQLite (rusqlite, one more native dependency)
   vs JSON Lines (simple, slow to search). To be decided once there is an
   estimate of history volume.
3. **The script extraction template engine** — **resolved (M4): it stays in
   Java and ships in the bridge.** Extraction is a §6 data plane job, so it has
   to run on the JVM side where the rows flow, and compatibility with jdbgen's
   template assets comes for free when the engine (885 lines, same author, MIT)
   is inherited as is. Porting it to Rust would only add the risk of subtle
   parser incompatibilities.
4. **The ERD auto-layout algorithm** — **resolved (M5): a small hand-written
   implementation.** ERD nodes are boxes whose size varies with column count,
   and FK graphs commonly contain cycles and self-references; the crates
   surveyed (rust-sugiyama centers on uniform vertex coordinates, dagre-rs is a
   young port) add failure modes outside our control under those conditions.
   The quality needed is "a starting point for manual dragging", so the
   standard four-stage heuristic (greedy cycle removal → longest-path ranking →
   median crossing reduction → per-rank coordinate assignment) is enough, and
   it fits the minimal-dependency principle (the same call as D8). It lives as
   a pure module in `rudbman-erd`.
5. **PNG export** — gpui's offscreen render path needs checking. Start with SVG
   only.
6. **SSH agent forwarding and multi-hop jump hosts** — environments exist where
   two or more bastions stack. M1 supports a single hop, and this expands once
   the need is confirmed.
7. **The LOB re-read strategy** — the address is settled as `lob_id` (§4.6),
   but **most drivers invalidate a `Blob`/`Clob` handle the moment the row
   changes.** After the grid has fetched 500 rows and the user opens the BLOB
   in the third row, that handle is already dead. The candidates:
   (a) spill to a temp file at fetch time — accurate, but writes disk for LOBs
   nobody will open, (b) read immediately up to an inline cap (around 4KB) and
   re-query the single row by primary key beyond that — which has to be refused
   on result sets with no key, (c) allow it only within the current batch.
   No viewer exists through M3 and M4, so this is still undecided — it gets
   decided in the milestone that adds a LOB viewer. The bridge already records
   `lob_id → (row, column, size, is-binary)`, so either option can be bolted on
   afterwards.

---

## Appendix A. Traps to respect

Things logman and jdbgen already paid for.

- **Deleting the gpui vendor patches** → typing with a Korean IME pins the CPU
  at 100% and freezes. Closing the last window on X11 panics. Do not delete or
  rename the six `LOGMAN PATCH` comments in `vendor/gpui` (taffy 1, x11 client
  1, x11 window 5, windows 3) — they have to stay byte-identical to logman's
  vendored copy so upstream patches can be exchanged with `diff`
- **Using `DriverManager`** → when two drivers claim the same URL prefix there
  is no telling who wins. Call `Driver.connect` directly
- **Ignoring a `null` return from `Driver.connect`** → per the spec that means
  "I do not understand the URL". It is not an exception
- **Starting the JVM without `-Xrs`** → the JVM intercepts SIGINT/SIGTERM and
  closing the window stops working
- **A fresh `URLClassLoader` per session** → repeated driver static
  initialization and a memory leak
- **Fetching results on the UI thread** → the window freezes. Do not cross the
  worker thread boundary
- **`DECIMAL` as `f64`** → cannot be undone. Send it as text
- **Inlining LOBs into a batch** → one row with a 100MB BLOB crosses JNI
- **Calling another callback while holding a `RefCell` inside a gpui callback**
  → a re-entrancy panic on the X11 backend. logman hit it twice
