# rudbman-bridge

The Java side of the JNI boundary. One JAR, one entry point, no framework.

Everything `rudbman-jdbc` can ask a database is routed through a single static
method, and every answer comes back as a length-prefixed byte array. This file is
the contract; `docs/architecture.md` §4–§6 is the design behind it.

## Build

```
cd bridge
./gradlew build          # compile + test
./gradlew jar            # just the artifact
```

Output: `bridge/build/libs/rudbman-bridge.jar`.

Requires an ambient JDK 17 or newer (`--release 17` pins the bytecode level).
The Gradle wrapper is pinned to 8.14.3. First build needs network access to
Maven Central for Gson, H2 and JUnit.

Gson is merged into the JAR rather than shipped beside it, because the JVM is
booted with `-Djava.class.path=<bridge.jar>` and nothing else. It stays under its
own `com.google.gson` package — it is not relocated. Driver JARs load through a
child loader whose parent is the bridge loader, so a driver that bundles its own
Gson will see the bridge's copy. No known JDBC driver does; if one ever does, the
fix is a shading plugin, not a classpath change.

`cargo build` never runs Gradle. `rudbman-jdbc/build.rs` only checks that the JAR
exists.

## Entry point

```java
package comart.rudbman.bridge;

public final class Bridge {
    public static byte[] call(int op, long handle, long arg, byte[] req);
}
```

- `op` — operation code, below
- `handle` — session, cursor or job handle; `0` when the operation takes none
- `arg` — integer argument for hot paths, so `FETCH` parses no JSON
- `req` — request body as UTF-8 JSON, or `null`
- returns — response envelope, never `null`

**`call` never throws.** Every failure, `Throwable` included, comes back as an
ERROR envelope. `NoClassDefFoundError` out of an incomplete driver JAR is a
message for the user, not a reason to take the process down. This is what lets
the Rust side drop `ExceptionCheck` from the normal path.

## Operations

| Code | Name | handle | arg | req | resp |
|---|---|---|---|---|---|
| `0x01` | `OPEN_SESSION` | — | — | JSON connection spec | JSON `{session}` |
| `0x02` | `CLOSE_SESSION` | session | — | — | — |
| `0x03` | `PING` | session | — | — | JSON `{ok, elapsed_ms}` |
| `0x04` | `SESSION_INFO` | session | — | — | JSON product / driver / capability facts |
| `0x10` | `DESCRIBE` | session | — | JSON `{kind, …}` | JSON `{kind, items[]}` (`ddl`: `{kind, ddl, source}`) |
| `0x20` | `EXECUTE` | session | — | JSON statement spec | JSON `{cursor, columns[], update_count, has_result_set, has_more}` |
| `0x21` | `FETCH` | cursor | max rows | — | binary `RDB1` batch |
| `0x22` | `MORE_RESULTS` | cursor | — | — | JSON, same shape as `EXECUTE` |
| `0x23` | `CLOSE_CURSOR` | cursor | — | — | — |
| `0x24` | `CANCEL` | session | — | — | JSON `{cancelled}` |
| `0x25` | `LOB_READ` | cursor | — | JSON | binary — **not implemented yet** |
| `0x30` | `SET_AUTOCOMMIT` | session | 0/1 | — | — |
| `0x31` | `COMMIT` | session | — | — | — |
| `0x32` | `ROLLBACK` | session | — | — | — |
| `0x40` | `JOB_START` | session | — | JSON `{kind, …}` | JSON `{job}` |
| `0x41` | `JOB_POLL` | job | — | — | JSON progress |
| `0x42` | `JOB_CANCEL` | job | — | — | JSON `{cancelled}` |
| `0x50` | `PROBE_DRIVER` | — | — | JSON `{jars[]}` | JSON `{classes[], services[]}` |

Unimplemented codes return an ERROR envelope with `kind: "protocol"` and a
message saying so, rather than an unknown-op error.

## Response envelope

```
u8  tag       0 = OK, 1 = ERROR
    payload   OK: operation body (JSON or binary), ERROR: JSON
```

Operations with no response body return the single byte `0x00`.

Error JSON, every member always present:

```json
{
  "kind": "sql | driver | io | protocol | interrupted | internal",
  "sql_state": "42S02",
  "vendor_code": 942,
  "message": "…",
  "causes": ["java.net.ConnectException: Connection refused", "…"],
  "stack": "…"
}
```

- `sql_state` / `vendor_code` come from `SQLException`. Both chains are walked —
  `getCause` and `getNextException` — because drivers routinely hide the real
  reason in the second exception. Both walks are cycle-guarded and depth-capped
  at 16.
- `causes[]` is that same flattened chain, excluding the root message.
- `stack` is for the debug log. Never show it to the user.
- `kind: "driver"` covers a missing driver class, a missing JAR, linkage errors
  and *"this driver does not accept this URL"* — the `null` return from
  `Driver.connect`, which the JDBC spec defines as "I do not understand this
  URL" and which is not an exception.

All responses use `serializeNulls`: a member the driver had nothing to say about
is JSON `null`, never absent. Rust `Option<T>` fields therefore need no
`#[serde(default)]`.

## Requests

### `OPEN_SESSION`

```json
{
  "url": "jdbc:postgresql://localhost:5432/app",
  "driver_class": "org.postgresql.Driver",
  "jars": ["/home/u/.config/rudbman/drivers/postgresql-42.7.4.jar"],
  "username": "app",
  "password": "…",
  "props": { "ApplicationName": "rudbman" },
  "read_only": false,
  "auto_commit": true,
  "login_timeout_s": 10,
  "keep_alive": { "enabled": true, "interval_s": 300, "query": "select 1" }
}
```

`url` and `driver_class` are required. An empty or absent `jars` resolves the
driver class from the bridge's own loader — that is how a driver baked into the
jlink image, or H2 on the test classpath, is reached.

`login_timeout_s` is passed through as a `loginTimeout` connection property.
`java.sql.Driver` has no login timeout of its own, and `DriverManager`'s is
global mutable state this bridge stays away from. A caller that knows its driver
should set the real property in `props`.

### `EXECUTE`

```json
{
  "sql": "select * from t where id = ? and amount > ?",
  "params": [42, { "type": "decimal", "value": "123456789012.12345678" }],
  "fetch_size": 500,
  "max_rows": 0,
  "timeout_s": 30
}
```

A parameter is either a bare JSON scalar (`null`, boolean, number, string) or an
object `{"type": …, "value": …}`. Types: `null`, `bool`, `i64`, `f64`,
`string`, `decimal`, `date`, `time`, `timestamp`, `bytes` (base64).

The typed form exists because JSON has one numeric type and no date type. A
`DECIMAL(20,8)` sent as a JSON number arrives rounded — the same mistake the
batch codec refuses to make in the other direction.

Omitting `params`, or sending an empty array, uses a plain `Statement` instead of
a `PreparedStatement`.

`EXECUTE` **always** returns a non-zero `cursor`, even for an `UPDATE` that
produced only a row count, so `MORE_RESULTS` always has something to advance and
`CLOSE_CURSOR` always has something to close. `FETCH` on such a cursor returns an
empty terminal batch rather than an error.

`has_more` is a **hint**, not a fact. JDBC offers no way to look ahead without
consuming the current result, so it means *"`MORE_RESULTS` may still return
something"*. Keep calling `MORE_RESULTS` until it answers `has_more: false`
(which comes with `cursor` set, `update_count: -1` and no columns).

### `DESCRIBE`

```json
{ "kind": "columns", "catalog": null, "schema": "APP", "table": "CHILD" }
```

| kind | needs | notes |
|---|---|---|
| `catalogs` | — | |
| `schemas` | — | |
| `tables` | — | `types[]` filters by `TABLE`, `VIEW`, … |
| `columns` | — | |
| `primary_keys` | exact `table` | |
| `imported_keys` | exact `table` | |
| `exported_keys` | exact `table` | |
| `indexes` | exact `table` | `unique_only`, `approximate` |
| `type_info` | — | |
| `procedures` | — | each item carries its `parameters[]` |
| `functions` | — | same shape as `procedures` |
| `sequences` | — | vendor catalogue query; empty list on products without sequences |
| `ddl` | exact `table` | answers `{ddl, source}`, not `items[]` |

Every response except `ddl` is `{ "kind": "...", "items": [ … ] }`. Item keys are
fixed snake_case chosen here, **not** the driver's metadata labels, so the Rust
structs stay stable across drivers. Optional metadata columns that a driver omits
come back as `null`; the reader collects the available labels once instead of
asking for a missing one and catching the exception per cell.

- `schema` is an exact name, `schema_pattern` a LIKE pattern; likewise
  `table` / `table_pattern`, `column` / `column_pattern` and, for `procedures`
  and `functions`, `name` / `name_pattern`.
- `imported_keys` and `exported_keys` share one item shape with `pk_`/`fk_`
  prefixes; only the direction of the query differs.
- `indexes` accepts `unique_only` (default false) and `approximate`
  (default **true** — a statistics refresh on a large schema is the difference
  between instant and a minute).

#### `procedures` and `functions`

Each item carries `catalog`, `schema`, `name`, `specific_name`, `remarks`, `type`
(the raw JDBC code), `type_name` and a `parameters[]` array of
`{name, mode, mode_name, data_type, jdbc_type, type_name, precision, length,
scale, radix, nullable, is_nullable, remarks, default, ordinal}`.

The parameter list travels with the routine instead of needing a call per
routine: the explorer tree draws a signature, and a schema with two hundred
procedures would otherwise cost two hundred round trips. The join key is
`SPECIFIC_NAME` when the driver supplies one on both sides — that is what keeps
overloads apart — and the routine name otherwise. A driver that refuses
`getProcedureColumns` costs the signatures, not the routine list.

`mode` codes are **not** shared between the two families. `procedureColumnOut` is
4 and `procedureColumnResult` is 3, while `functionColumnOut` is 3 and
`functionColumnResult` is 5; `mode_name` is the resolved text, use it.

Which of the two lists a routine appears in is a per-product decision. H2 2.x
returns an empty result from `getFunctions` unconditionally and reports
`CREATE ALIAS` functions through `getProcedures`. An empty list means "this
server files them elsewhere", not "there are none".

#### `sequences`

JDBC has no sequence accessor — sequences were standardised in SQL:2003, long
after `DatabaseMetaData` was fixed — so this kind is a vendor catalogue query.

| product | source |
|---|---|
| H2 | `INFORMATION_SCHEMA.SEQUENCES` |
| PostgreSQL | `information_schema.sequences` (only what the user may see) |
| Oracle | `ALL_SEQUENCES` — no `START WITH` column exists, so `start_value` is null |
| MariaDB | `information_schema.SEQUENCES`, probed; empty list where the build has no such view |
| everything else | empty list |

**An empty list is a correct answer.** MySQL, SQLite and any unrecognised product
are simply not asked, and a query that is attempted and rejected — no privilege,
no such view — lands in the same place. Items are
`{catalog, schema, name, data_type, start_value, min_value, max_value, increment,
cycle, cache, current_value, remarks}`; every value but `cycle` is a string,
because an Oracle sequence maximum is `NUMBER(28)` and does not fit a `long`.

`schema` and `name` filter exactly, in Java rather than in a `WHERE` clause,
because the column holding the schema name differs per product.

#### `ddl`

```json
{ "kind": "ddl", "schema": "APP", "table": "CHILD", "source": "auto" }
```

Answers `{ "kind": "ddl", "ddl": "CREATE TABLE …", "source": "native" | "metadata" }`
— one document, not a list of rows.

`source` in the request selects the layer: `auto` (default) tries the native path
and falls back, `native` fails if there is none, `metadata` always reconstructs.

1. **Native.** MySQL and MariaDB answer `SHOW CREATE TABLE`; H2 answers `SCRIPT
   NODATA NOPASSWORDS NOSETTINGS TABLE`, filtered down to the rows naming this
   table (a script still carries the user, the schema and every alias and
   sequence around it). H2 2.x has no `SQL` column in `INFORMATION_SCHEMA.TABLES`
   — that was an H2 1.4 thing — so `SCRIPT` is what is left. Where this works it
   *is* the truth, storage clauses and `CHECK` constraints included.
2. **Reverse generation.** Everything else is assembled from `getColumns`,
   `getPrimaryKeys`, `getImportedKeys` and `getIndexInfo`. This path works on
   every driver, which is why it exists.

A native attempt that fails — no privilege, an unexpected result shape — falls
through to reverse generation rather than failing the request, and when the
session is not in auto-commit the attempt is fenced with a savepoint so that a
rejected probe cannot poison an open transaction (PostgreSQL aborts the whole
transaction on any statement error).

Identifiers are quoted **only when leaving the quotes off would change meaning**:
a character outside the portable alphabet, a case that would not survive the
server's folding, or a reserved word. `getSQLKeywords()` lists only the vendor's
additions beyond the standard — H2 answers `LIMIT,TOP,…` and not `ORDER` — so the
SQL:2011 reserved words are carried in the bridge and unioned with it.

The reconstruction is **for display**. It replays on H2 in the test suite, but it
is not a migration tool: JDBC metadata carries no `CHECK` constraints, triggers,
partitioning, tablespaces, collations or generated-column expressions, `UNIQUE`
constraints arrive as unique indexes and are emitted as `CREATE UNIQUE INDEX`,
and a view reaches this path as a bare column list. Indexes that merely back the
primary key or a foreign key are dropped, because the server creates them again
and emitting them would make the statement fail to replay.

### `FETCH`

`arg` is the maximum row count. `arg <= 0` means the default 500; values above
1,000,000 are clamped. No JSON is parsed on this path.

### `JOB_START`, `JOB_POLL`, `JOB_CANCEL`

A job is work that moves more rows than anyone wants to carry across JNI and
takes long enough that the UI has to stay alive while it runs. **The rows never
leave the JVM**; what crosses is a handle and, every couple of hundred
milliseconds, a progress object. Files are written here too, on the side the data
is on.

Three kinds: `extract` (objects → script, CSV or template file), `backup` (a
scope's tables → one script file) and `transfer` (a query on one session →
a table on another).

```json
{ "kind": "extract",
  "objects": [{ "catalog": null, "schema": "APP", "name": "ORDERS" }],
  "output":  { "path": "/tmp/orders.sql", "charset": "UTF-8", "newline": "\n" },
  "ddl":     { "include": true, "include_drop": false, "constraints": "alter" },
  "data":    { "include": true, "mode": "insert", "insert_batch_rows": 1,
               "template_path": null, "where": null } }
```

```json
{ "kind": "backup",
  "scope":    { "catalog": null, "schema": "APP" },
  "output":   { "path": "/tmp/app.sql.gz", "charset": "UTF-8", "newline": "\n" },
  "compress": "gzip",
  "ddl":      { "include": true, "include_drop": false, "constraints": "alter" },
  "data":     { "include": true, "insert_batch_rows": 1 } }
```

```json
{ "kind": "transfer",
  "source_sql":     "select * from orders where created_at > '2026-01-01'",
  "target_session":  7,
  "target_table":   { "catalog": null, "schema": "APP", "name": "ORDERS" },
  "mode":           "insert",
  "batch_size":     500,
  "commit_every":   10000,
  "column_map":     [{ "from": "ID", "to": "ORDER_ID" }],
  "on_error":       "abort" }
```

`JOB_START` answers `{"job": <handle>}`. `JOB_POLL` answers:

```json
{ "state": "running | done | failed | cancelled",
  "rows_done": 12000, "rows_skipped": 0, "rows_total": null,
  "bytes": 918273, "phase": "data:APP.ORDERS", "errors": [], "eta_s": null }
```

- **A job handle dies on the first poll that reports a terminal state.** That
  poll unregisters it in the same call; a second poll is a `protocol` error. Stop
  polling as soon as a terminal state arrives, and never cancel afterwards.
- **Specification errors are synchronous.** A bad `mode`, `charset`,
  `on_error`, an unknown `target_session`, or an `upsert` whose target has no
  primary key comes back as an ERROR envelope from `JOB_START` itself. The one
  exception is an error that needs the query's result shape — a `column_map`
  naming a column the source did not return — which fails the job instead.
- `rows_total` and `eta_s` are `null`: no `COUNT(*)` is run up front.
  `rows_skipped` is always present and is zero for everything but a transfer
  under `on_error: skip | log`. `bytes` is zero for a transfer, which writes no
  file, and lags by up to one buffer for the ones that do, exact at the end.
- `errors[]` holds error-envelope objects and is **capped at 100**; past that,
  dropped rows are only counted. `rows_skipped` is the total.
- Cancellation sets a flag and calls `Statement.cancel()` on every statement the
  job has in flight — two of them for a transfer. Each poll re-delivers the
  cancel until the worker acknowledges it, because a cancel that lands in the
  sliver before the driver enters execution cancels nothing. A cancelled job
  keeps its partial output file.
- `CLOSE_SESSION` cancels and unregisters the jobs that use that session, **in
  either direction** — a transfer holds the target's connection lock too, and
  closing it would otherwise wait on a lock only a cancel releases.

**Extract and backup** write every `CREATE`, then every foreign key as
`ALTER TABLE … ADD CONSTRAINT`, then the data. Foreign keys go last because two
tables that reference each other cannot be created in any order at all.
`ddl.constraints: "alter"` (the default) forces the reverse-generated DDL path,
with the blind spots listed under `DESCRIBE`/`ddl`: lifting the keys out of a
server's own `CREATE` text would mean parsing vendor SQL.

A backup is an extract without the object list: the scope's `TABLE` entries are
enumerated in name order — no views, no routines. The data section is reordered
so that a table follows the tables it references, because the keys are already in
place by then and alphabetical order would have `CHILD` rejected before `PARENT`
exists. A reference cycle has no such order and is left in name order. The
catalog written into the script is the one `scope.catalog` asked for, never the
one the driver reports, so a backup does not pin itself to the source database's
name. `compress: "gzip"` wraps the output; `bytes` then counts the compressed
file, matching its size.

**Transfer** holds both connection locks for the whole stream, taken in ascending
session-handle order so that two transfers in opposite directions cannot
deadlock; a transfer into its own session is safe on the reentrant lock. Values
move by `getObject` / `setObject`, so type coercion is the target driver's
problem and an exotic vendor type is a known edge that takes the `on_error` path.
The target's auto-commit is turned off and restored at the end; a commit happens
every `commit_every` rows (`0` = once at the end) and once on success. **A cancel
or a failure rolls back the uncommitted tail and leaves what was committed**,
which is what `rows_done` reports.

- `truncate_insert` empties the target with `DELETE FROM`. `TRUNCATE` is a
  dialect, privilege and transactionality minefield; `DELETE` means the same
  thing everywhere and rolls back with the rest.
- `upsert` reads its conflict key from the target's primary key and spells the
  statement per product: `ON CONFLICT` (PostgreSQL, SQLite), `ON DUPLICATE KEY`
  (MySQL, MariaDB), H2's `MERGE … KEY (…)`, standard `MERGE` (Oracle, SQL Server,
  Db2). An unrecognised product has no portable form and is rejected rather than
  guessed at.
- `on_error: "abort"` (the default) fails the job on the first bad row. `skip`
  and `log` roll the failed batch back to a savepoint and replay it a row at a
  time, so one poisoned row costs only itself; `log` also records the failure.
  Where the driver has no savepoints, those two policies fall back to one row per
  batch, which is slower but cannot double-insert.

## `RDB1` batch codec

All integers little-endian.

```
Batch  := Header Column*
Header := "RDB1"(4B) | u32 col_count | u32 row_count | u8 flags
          flags bit0 = this is the last batch
Column := u8 kind | u32 payload_len | payload
```

`payload` **always** starts with a validity bitmap of `ceil(row_count/8)` bytes.
Bits are **LSB-first**: row `i` is byte `i >> 3`, bit `i & 7`. A set bit means
**non-null**. `payload_len` covers the bitmap and the values together.

| kind | name | value area |
|---|---|---|
| 0 | `NULLS` | none — every row is NULL |
| 1 | `I64` | `row_count × i64` |
| 2 | `F64` | `row_count × f64` (raw IEEE-754 bits, so NaN and ±∞ survive) |
| 3 | `BOOL` | packed bits, LSB-first, `ceil(row_count/8)` bytes |
| 4 | `STR` | `u32 offsets[row_count+1]` then UTF-8 bytes |
| 5 | `BIN` | same layout, raw bytes |
| 6 | `LOB` | `row_count × (u64 lob_id, u64 size)` |

Points a decoder has to get right:

- **The kind of a column can change between batches of the same cursor.** A
  batch in which a column is entirely NULL is emitted as kind 0 regardless of the
  column's declared type, so a 500-row all-null string column costs 63 bytes
  instead of kilobytes of zero offsets. Switch on the kind byte per batch, not
  once per cursor. The stable type is the logical one in `columns[]`.
- **Kind 0 still carries the bitmap** (all zeros), because the payload always
  starts with one. Only the value area is omitted.
- **NULL rows still occupy a slot** in every fixed-width value area, filled with
  zero, so indexes line up with the bitmap without a rank computation.
- **A NULL and an empty string produce the same zero-length slice.** Only the
  bitmap tells them apart. The grid must distinguish them; too many tools do not.
- **`row_count` may be 0.** The bitmap is then 0 bytes, and every column is
  kind 0 with an empty payload. A cursor that produced no result set at all
  yields `col_count: 0` too.
- The last batch is only recognised as such when the driver runs out of rows. A
  batch that filled its row limit exactly reports `flags = 0`; the next `FETCH`
  returns a 0-row batch with bit0 set.

### Type mapping

| JDBC type | kind |
|---|---|
| `TINYINT` `SMALLINT` `INTEGER` `BIGINT` | `I64` |
| `REAL` `FLOAT` `DOUBLE` | `F64` |
| `BOOLEAN`, `BIT` with precision ≤ 1 | `BOOL` |
| `BINARY` `VARBINARY` `LONGVARBINARY`, `BIT` with precision > 1 | `BIN` |
| `BLOB` `CLOB` `NCLOB` | `LOB` |
| everything else | `STR` |

`DECIMAL`, `NUMERIC`, `DATE`, `TIME`, `TIMESTAMP`, `UUID`, `INTERVAL`, arrays,
`SQLXML` and vendor types all travel as `STR`:

- the grid displays text in the end;
- flattening a `BigDecimal` into an `f64` cannot be undone — `DECIMAL` goes
  through `BigDecimal.toPlainString()`, never through a double and never through
  exponent notation;
- the driver's own text is the only authority on which time zone was applied, so
  time-zone handling stays on this side of the boundary;
- sorting is the server's job via `ORDER BY`.

`BIT` is split by precision because MySQL reports `BIT(n>1)` as `Types.BIT` while
handing back a byte string, not a boolean.

Presentation — right alignment, NULL rendering, copy format — is decided by the
**logical** type in `columns[]` (`type`, `jdbc_type`, `type_name`, `precision`,
`scale`, `nullable`, `auto_increment`). The `kind` is transport only. Each entry
in `columns[]` also carries a `kind` hint: the encoding a full batch would use.

### LOBs

`BLOB`, `CLOB` and `NCLOB` never inline. A 100MB BLOB must not cross JNI because
a user scrolled past its row. Each cell contributes 16 bytes: an id and a size.

- `size` is **octets** for binary LOBs and **characters** for character LOBs —
  that is what `Clob.length()` means, and what a `LOB_READ` offset would be
  counted in.
- `size` of `0xFFFFFFFFFFFFFFFF` (`-1` as i64) means the driver would not report
  a length.
- Ids are unique within a cursor and are reset whenever the cursor advances to a
  new result.
- `LOB_READ` (`0x25`) is a later milestone. See "Known gaps" below.

`LONGVARCHAR` and `LONGVARBINARY` are **not** treated as LOBs; they inline as
`STR` / `BIN`. On MySQL a `LONGTEXT` reaches 4GB under `Types.LONGVARCHAR`, so
this is a known sharp edge, deferred with `LOB_READ`.

## Concurrency

- One JDBC connection per session, one Rust worker thread per session. The worker
  serialises commands, which is what makes a non-thread-safe `Connection` safe.
- The session still holds a `ReentrantLock` around every use of the connection,
  because the keep-alive timer runs concurrently with the worker. The timer uses
  `tryLock` and skips its round rather than queueing — a statement already in
  flight keeps the connection just as alive.
- **`CANCEL` deliberately takes no lock.** It arrives on a different thread while
  the worker holds the lock inside the blocking `execute` it is meant to
  interrupt. The cursor table is a `ConcurrentHashMap` for exactly this, and
  cursors are registered *before* the statement executes. `Statement.cancel()` is
  the one JDBC method documented as callable from another thread.
- Handles are never reused, so a stale handle is always reported as a stale
  handle and never mistaken for a live object.
- A job runs on its own daemon thread and takes the session's connection lock for
  each phase of work — a whole table stream, not a statement, because a result
  set cannot survive another statement on the same connection. `EXECUTE` on that
  session blocks meanwhile, so a UI that wants to keep querying during a long job
  opens a second session. A transfer holds two such locks, taken in ascending
  session-handle order.

## Driver isolation

One child `URLClassLoader` per set of driver JARs, parented to the bridge loader.
`Class.forName(cls, true, child)` then `Driver.connect(url, props)` directly —
**never `DriverManager`**, which is a global registry where two drivers claiming
the same URL prefix produce an undefined winner.

Loaders are cached by the JAR path list and reference counted; the loader closes
when its last session does. A fresh loader per session re-runs the driver's
static initialisers and leaks everything they loaded.

The cache key preserves the caller's JAR order, because classpath order decides
which JAR wins when two ship the same class — two orderings are genuinely two
different class paths.

`PROBE_DRIVER` uses a throwaway loader and `Class.forName(cls, false, …)`:
probing must not run a driver's static initialiser, which may open sockets or
load native libraries, and must not pin a JAR the user is about to replace.

## Layout

```
src/main/java/comart/rudbman/bridge/
├── Bridge.java            the single JNI entry point; op dispatch, Throwable → envelope
├── Ops.java               operation codes
├── Envelope.java          response envelope and error mapping
├── Json.java              Gson tree helpers
├── BridgeException.java   failures that carry their own envelope kind
├── Registry.java          handle ↔ object table
├── Session.java           Connection + loader lease + keep-alive
├── Loaders.java           URLClassLoader cache
├── Cursor.java            Statement + ResultSet + batch encoding
├── Params.java            EXECUTE parameter binding
├── DriverProbe.java       PROBE_DRIVER
├── codec/                 RDB1 encoder
├── job/
│   ├── Jobs.java          worker thread, progress, cancellation, the job table
│   ├── ExtractJob.java    kind: "extract"
│   ├── BackupJob.java     kind: "backup"
│   ├── TransferJob.java   kind: "transfer"
│   ├── Scripts.java       DROP and batched INSERT text, shared by the two writers
│   ├── Literals.java      value → SQL literal or plain text, per dialect
│   └── ScriptOut.java     counting, charset-encoding, optionally gzipped output
├── template/              the jdbgen template engine, for extract's template mode
└── meta/
    ├── Describe.java      DESCRIBE dispatch, the DatabaseMetaData kinds
    ├── Routines.java      procedures and functions with their parameters
    ├── Sequences.java     sequences, per-product catalogue queries
    ├── Ddl.java           native DDL, and reverse generation as the fallback
    ├── Dialect.java       product name → the vendor paths that apply
    ├── Ident.java         identifier quoting, only where it is needed
    ├── Upsert.java        the per-product spelling of "insert or update"
    ├── Attempt.java       savepoint-fenced query that is allowed to fail
    ├── RsView.java        reader for metadata result sets with optional columns
    ├── SessionInfo.java   SESSION_INFO
    └── SqlTypes.java      java.sql.Types names
```

## Inherited code

From [jdbgen](https://github.com/comart/jdbgen) (MIT, Dennis Soungjin Park):

- `types/db/DBMeta.java` → `Session.java`, `Loaders.java` — the child class
  loader, the deliberate avoidance of `DriverManager`, the explicit `null` check
  on `Driver.connect`, the connection lock, the keep-alive scheduler
- `types/db/SqlTypes.java` → `meta/SqlTypes.java`
- `utils/ClassUtils.java` → `DriverProbe.java`

New here: primary keys, foreign keys, indexes, `type_info`, routines and
sequences, DDL generation, the `RDB1` codec, the handle registry, cancellation,
the error envelope and the job layer (extract, backup, transfer).

## Tests

```
./gradlew test
```

H2 in-memory is the reference database. Two of these earn their keep by round
tripping rather than by asserting on strings:

- `FetchRoundTripTest` asserts through `support/Batch.java`, a decoder written
  from the format description above rather than from the encoder. That round trip
  is the only thing that proves the format the Rust decoder has to read.
- `DdlTest.reverseGeneratedDdlReplays` executes the reconstructed DDL into a
  second schema and compares the two tables through the same metadata the
  reconstruction was built from. String assertions only prove that the expected
  words appear somewhere; replaying proves that what came out is the table that
  went in.
- `ExtractJobTest` and `BackupJobTest` run their generated scripts into a fresh
  database and read the rows back. `TransferJobTest` reads the target through its
  own session, which is the only evidence a transfer leaves — no file, and no row
  ever crossed JNI.

## Known gaps

- `LOB_READ` (`0x25`) is not implemented.
- The `upsert` statements for Oracle, SQL Server and Db2 are written from the
  products' documentation and are not covered by a test; only the H2 form is
  executed here.
- A transfer binds with `getObject` / `setObject`. Arrays and vendor structured
  types often do not survive that between two different products; those rows take
  the `on_error` path.
- The batch carries a `lob_id`, but §4.5 of the architecture document specifies
  `LOB_READ` as taking `{row, col, offset, len}`. These are two different
  addressing schemes; one of them has to go. The cursor currently records
  `lob_id → (row, column, size, binary)` so either can be served, but the
  request shape is not settled. Whichever wins, re-reading a LOB after the
  result set has advanced needs a forward-only-safe strategy — most drivers
  invalidate a `Blob` as soon as the row changes.
- `LONGVARCHAR` / `LONGVARBINARY` inline rather than becoming LOB references.
- Only MySQL, MariaDB and H2 have a native `ddl` path; every other product gets
  the reverse-generated form with the limits listed above.
- `sequences` covers H2, PostgreSQL, Oracle and MariaDB. SQL Server
  (`sys.sequences`) and Db2 (`SYSCAT.SEQUENCES`) have sequences and are not
  wired up; they return the empty list.
- Reverse-generated DDL emits a `CREATE TABLE` for a view, because JDBC metadata
  does not carry the view's query text.
