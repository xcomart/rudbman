# User dialect profiles — design

**Status: phases 1 to 3 implemented; the rest settled before the code.**
Profiles resolve and export in `rudbman-sql`, read and write as YAML in
`rudbman-core`, load from `<config>/dialects` at start-up, are picked and
exported in the driver manager, and the bridge answers a session from its
profile's `bridge` half; scripts are read but not yet run. This document
fixes the file
format, the resolution rules, the scripting surface, the trust model and how
the application ships profiles of its own, so that the phases at the end can
land one at a time without reopening them.

## 1. Goal

A user can teach rudbman (and rudbgen) a database product it does not know, or
adjust one it does, by dropping files into the configuration directory — the
way a locale file adds a language — without rebuilding either application.

Today everything a product does differently lives in code: one file per
product in `crates/rudbman-sql/src/dialect/` (a `Vendor` implementation) and
one in the bridge's `meta/<Product>Vendor.java`. A profile is the same set of
answers written as data, on top of one of those built-in implementations.

### Non-goals

- **The built-ins that exist stay in code.** They are checked against real
  servers by the container suites, and a profile cannot replace or shadow one.
  A product supported from now on ships as a profile instead (§12).
- **No scripting in the editor's hot path.** The lexer and statement splitter
  run on every keystroke in Rust; a profile only chooses among the rule sets
  they already have.
- **No sandbox.** A script runs with the application's rights (§6). The trust
  model makes that visible; it does not pretend to contain it.

## 2. Overview

```
~/.config/rudbman/dialects/
├── tidb.yml          # a profile: data, plus an optional script
└── tidb.groovy       # bridge hooks the data cannot express
```

- A **profile** is a YAML file. Its file stem is its **id**, the string a
  driver definition's `dialect` names (`"dialect": "tidb"` in `drivers.json`).
- Every profile has a **`base`**: a built-in dialect id (`postgres`, `mysql`,
  `oracle`, ...) or another profile's id. Anything the profile does not say is
  the base's answer, all the way down the chain to a built-in.
- A profile has two halves: **`sql`** for rudbman-sql (the editor, DDL and DML
  writers, explorer statements) and **`bridge`** for the JDBC bridge (catalog
  reads, DDL reconstruction, literals, scripts, backup).
- Where data is not enough, **`bridge.script`** names a Groovy file whose
  methods override bridge hooks by name (§5).

Precedence for any one answer: script method > profile value > the base's
answer.

## 3. Files, ids and matching

| | |
| --- | --- |
| Location | `<config>/dialects/*.yml`, where `<config>` is the application's configuration directory (`~/.config/rudbman` on Linux). Scripts sit beside them and are named relative to the YAML file. |
| Id | The file stem, lower-case ASCII letters, digits, `-` and `_`. |
| Reserved ids | Every built-in id and alias (`generic`, `h2`, `postgres`, `postgresql`, `pg`, `mysql`, `mariadb`, `sqlite`, `oracle`, `mssql`, `sqlserver`, `tsql`, `db2`, ...). A profile with one of them is refused: to change a built-in, write a profile with that `base` and point the driver at it. |
| Matching | By the driver definition's `dialect` field, not by its display name. A display name is the user's to change, and one product can have several definitions (rudbgen's H2 embedded and server). The driver manager's dialect picker lists the built-ins and then every profile by its `name` and file, one that failed marked as such. |
| Fallback | A `dialect` naming no built-in and no loaded profile is generic SQL, as today (`Dialect::from_id`). A profile that fails to load is reported (§7) and its drivers get generic SQL until it is fixed. |

`api: 1` is required in every profile. A later format raises it; an
application refuses a profile whose `api` it does not know, rather than
reading half of it.

## 4. The profile format

Every key is optional except `api`, `name` and `base`. Values merge over the
base's: a record (`syntax`, `alter`, `drops`, ...) field by field, a word list
by adding to the base's, a template or a list of statements by replacing it.
`null` where a value is allowed to be absent means "this product has none" —
for example `page_clause: null` makes the data pane keep a cursor open, as on
Oracle.

Templates use `{name}` holes. The holes each key takes are listed with it; an
unknown hole is an error at load time, not text sent to a server. A template
whose hole has no value for the call gives no statement, the way a built-in
answers `None`: `enter_scope: "USE {catalog}"` enters nothing for a scope with
no catalog.

### 4.1 Identity

```yaml
api: 1
name: TiDB                 # shown in the dialect picker and in messages
base: mysql                # a built-in id or another profile's id
aliases: [pingcap]         # other spellings a driver's dialect may use
```

### 4.2 `sql`: what rudbman-sql answers

Each key corresponds to one method of `rudbman_sql::dialect::Vendor`.

```yaml
sql:
  syntax:                       # Syntax: the lexer's rules, merged field by field
    hash_line_comment: true
    dash_dash_needs_space: true
    nested_block_comments: false
    backslash_escapes: true
    e_strings: false
    double_quoted_strings: true
    backtick_identifiers: true
    bracket_identifiers: false
    dollar_quotes: false
    hex_literals: true
    numbered_parameters: false
    colon_parameters: false
    at_parameters: true
    hash_identifiers: false
  words:
    keywords: [AUTO_RANDOM, SHARD_ROW_ID_BITS]   # added to the base's tables
    types: []
  functions: [TIDB_VERSION]                      # added to completion's list

  folding: preserve             # upper | lower | preserve
  case_insensitive_names: true
  catalog_first: true
  uses_catalogs: true

  blocks:                       # merged field by field
    language: psm               # plsql | tsql | psm
    begin: not_atomic           # psm only: transaction | not_atomic | unless_transaction
    routines: psm               # psm only: psm | plsql | unknown
  routine_layout: mysql         # mysql | oracle | mssql | db2 | other
  client: mysql                 # psql | mysql | sqlcmd | sqlplus | clp | sqlite | plain

  page_clause: "LIMIT {limit} OFFSET {offset}"   # or null; holes: limit, offset
  reads_by_server_cursor: false
  explain:                      # or null; holes: sql
    before: []
    plan: "EXPLAIN {sql}"
    after: []
    shown: "EXPLAIN {sql}"

  empty_insert: "INSERT INTO {table} () VALUES ()"   # or null; holes: table, first_column
  alter:                        # AlterStyle, merged field by field
    add: ADD COLUMN
    default_first: false
    named_reference_columns: true
    column_rename: change       # rename_column | alter_rename_to | change | sp_rename
    attrs: restate              # clauses | restate | modify_changed | type_with_null | rebuild
    constraints: per_kind       # named | per_kind | rebuild
    check_drop: CHECK           # per_kind only: the word a check constraint's DROP takes
    table_rename: rename_to     # rename_to | rename_table | sp_rename
    reorg: false
  unsupported_reasons:          # Unsupported -> the sentence a refused edit gives
    reference_without_columns: "..."
  drops:                        # DropSpelling, merged field by field
    schema: database            # none | schema | schema_restrict | user | database
    database: true
    truncate: truncate          # truncate | immediate | delete_all
    routines: named             # none | named | alias | overloaded | specific | packaged
    triggers: named             # named | on_table
    in_catalog: false

  enter_scope: "USE {catalog}"  # or null; holes: catalog, schema (quoted)
  connect_timeout_property: connectTimeout     # or null
```

The enumerated values (`blocks`, `routine_layout`, `client`, the `alter` and
`drops` spellings) select behaviour that is code in rudbman-sql.
A product whose grammar needs a rule none of them has is a built-in to add,
not a profile.

### 4.3 `bridge`: what the JDBC bridge answers

Each key corresponds to a method of the bridge's `meta/Vendor.java`, and
`meta/ProfileVendor.java` answers it: from the key where the profile gives
it, from the base where it does not. Along a chain of profile bases a key
replaces the base profile's, except in `comments` and `texts`, whose own keys
replace one by one. `null` where a key allows it is "this product has none".

```yaml
bridge:
  detect: [tidb]                # substrings of getDatabaseProductName, lower-cased;
                                # used when a session is opened without a profile
  # names and catalogs
  catalog_is_schema: true
  result_catalogs_resolve: true
  sees_only_its_catalog: false
  numbers_procedures: false

  # catalog queries; binds are ? in the order given by `binds`
  sequences: []                 # queries tried in order; columns as Sequences reads them
  triggers:
    sql: "SELECT TRIGGER_SCHEMA, NULL, TRIGGER_NAME, ... FROM information_schema.TRIGGERS WHERE 1 = 1"
    filters: { catalog: TRIGGER_SCHEMA, schema: TRIGGER_SCHEMA, name: TRIGGER_NAME }
    order_by: "1, 3"
  comments:
    hides_comments: true
    marks_views_in_remarks: true
    escape: none                # ansi (ESCAPE '\') | none, after a pattern's LIKE
    tables: { sql: "...", filters: { catalog: TABLE_SCHEMA, table: TABLE_NAME } }
    columns: { sql: "...", filters: { ..., column: COLUMN_NAME } }
  texts:                        # the server's own text of an object, or null
    table: { sql: "SHOW CREATE TABLE {qualified}", column: 2 }
    view: { sql: "...", binds: [catalog, name], column: 1 }
    routine: null
    trigger: null
    sequence: null

  # DDL reconstruction
  qualifies_by_catalog: true
  defers_keys: false
  comments_inline: true
  comments_on: false
  computed_has_no_type: false
  serial_columns: false
  identity_clause: AUTO_INCREMENT
  not_null_first: true
  indexes_foreign_keys: true
  creates_without_catalog: false
  types_sequences: true
  cache_of_one_is_no_cache: false
  script_prologue: ["SET FOREIGN_KEY_CHECKS = 0;"]
  script_epilogue: ["SET FOREIGN_KEY_CHECKS = 1;"]
  data_prologue: []
  data_epilogue: []

  # literals and scripts
  bits_are_numbers: true
  booleans_are_numbers: true
  national_strings: never       # always | national_types | never
  hex_literal: "X'{hex}'"
  script_client: mysql          # plain | mysql | sqlplus | sqlcmd | clp
  drops_if_exists: true
  drop_table_cascade: ""
  drop_foreign_key: null        # or a template; holes: table, key
  multi_row_values: true
  upsert: on_duplicate_key      # none | on_conflict | on_duplicate_key | h2_merge |
                                #   merge: { from_dual, terminated }

  # backup
  backup_prologue: []           # holes: catalog, schema; entries may carry `when: routines`
  backup_epilogue: []
  identity_insert:              # statements around a table's rows, or null
    before: "SET IDENTITY_INSERT {table} ON;"
    after: "SET IDENTITY_INSERT {table} OFF;"
  overrides_system_value: false

  script: tidb.groovy           # optional, §5
```

What the queries return, and when they run:

- `sequences` columns are read by label, as the built-ins' are:
  `SEQUENCE_CATALOG`, `SEQUENCE_SCHEMA`, `SEQUENCE_NAME`, `DATA_TYPE`,
  `START_VALUE`, `MINIMUM_VALUE`, `MAXIMUM_VALUE`, `INCREMENT`,
  `CYCLE_OPTION`, `CACHE`, `CURRENT_VALUE`. Given, they replace the base's own
  way of listing sequences; `[]` lists none.
- `triggers` returns nine columns by position: catalog, schema, name, table,
  timing, event, level, enabled (`Y`/`N`), comment. Its `sql` ends where
  `AND <column> = ?` may follow; each filter with a value in the request adds
  one, and a column two filters name is compared once, with the first that
  has a value — the MySQL family's database is both catalog and schema.
- `comments.tables` returns owner, table, comment; `comments.columns` owner,
  table, column, comment — the owner being the catalog where
  `catalog_is_schema` holds and the schema otherwise. `schema`, `table` and
  `column` filters compare by `LIKE` where the request has a pattern.
- `texts.*` binds the object's `catalog`, `schema` or `name` in the order of
  `binds`, and reads `column` (from 1) of the first row. `{qualified}` is the
  object's name as a statement writes it. A query that fails or answers no row
  is no text, and the bridge rebuilds the object as it does for any product
  without one. `texts.table`, given, also stands for the base's own way of
  reading a table whole.
- `national_strings`, given, replaces the base's spelling of a character
  literal with the standard quoting, prefixed `N` as it says — MySQL's
  backslash escapes included, on a profile based on `mysql`.
- Templates: `hex_literal` takes `{hex}`; `drop_foreign_key` `{table}` and
  `{key}`; `backup_prologue` and `backup_epilogue` `{catalog}` and
  `{schema}`, quoted, a line naming one the backup has not being left out;
  `identity_insert` `{table}`. A `when: code` line is the epilogue's only:
  the prologue is written before it is known.

What a key cannot say — a literal computed from a JDBC type, a counter
derived from the source's catalog, a catalog row rewritten in place — is a
hook for the script, or the base's.

### 4.4 `driver`: the definition it brings

```yaml
driver:
  name: TiDB                    # optional; the profile's name otherwise
  class: com.mysql.cj.jdbc.Driver
  maven: com.mysql:mysql-connector-j:9.1.0   # optional
  url_template: "jdbc:mysql://{host}:{port}/{database}"
  default_port: 4000            # optional; none for a file-backed database
  icon: mysql                   # optional; an icon the application has
```

A driver definition of the profile's product, its id and `dialect` the
profile's id. It is offered to `drivers.json` once, the way a built-in
definition released since the file was written is (§12): the user may change
it or delete it, and a deleted one is not offered again. Only a profile that
loads offers one.

## 5. Groovy hooks

### 5.1 Where they run

Only in the bridge, which already runs in a JVM. Each method a script defines
with the name of a hook below replaces that hook for the profile's sessions;
every other hook is the profile's data or the base's. The script gets `base`,
the base's own `Vendor`, so that it can add to an answer rather than rewrite it:

```groovy
// tidb.groovy
String literal(ResultSet rs, int i, int type) {
    if (type == java.sql.Types.OTHER && rs.getMetaData().getColumnTypeName(i) == 'VECTOR') {
        def v = rs.getString(i)
        return v == null ? 'NULL' : "VEC_FROM_TEXT(${Ident.literal(v)})"
    }
    base.literal(rs, i, type)
}
```

### 5.2 The API, version 1

The hooks a script may define are a published API: their names and
signatures do not change within an `api` version. Version 1 takes only hooks
whose types are JDBC's, Java's, or the bridge's public `Ident`,
`Vendor.Sequence`, `Vendor.Table` and `Vendor.RowFence`:

| Area | Hooks |
| --- | --- |
| Literals | `literal`, `stringLiteral`, `hexLiteral` |
| Catalog reads | `routineRemarks`, `sequences`, `triggers`, `triggerRow`, `tableComments`, `columnComments` |
| Backup plan | `extensionMembers`, `materializedViews`, `tablesNotWritten`, `indexViews`, `packages`, `backupPreamble`, `ownedSequences` |
| Backup objects | `sequenceValue`, `resumeSequence`, `sqlMode`, `overloadTexts`, `routineDrops` |
| Backup rows | `textColumns`, `textColumn`, `rowFence`, `counters` |

The hooks that take the bridge's internal types — `catalogExtras` and
`confirmGenerated` (`Ddl.Col`), `nativeSplit` and `resumeIdentity`
(`Ddl.Script`), `viewText` and its siblings (`ObjectDdl.Target`) — stay out of
version 1. A profile reaches them through its base, or through the data keys
of §4.3. Publishing them means giving those types a stable public shape
first.

Every hook in the table becomes `public` in `Vendor` and gains a contract
test (§8) before it is published.

### 5.3 Groovy itself

- **Delivery.** The Groovy runtime (`org.apache.groovy:groovy`, pinned) is not
  bundled. The first session of a profile with a script downloads it the way a
  JDBC driver is downloaded (`maven.rs`, checked against Central's `.sha1`),
  into the drivers directory. A profile without a script never needs it.
- **Loading.** The bridge compiles a script once per content hash with a
  `GroovyClassLoader` whose parent is the bridge's own loader, so the script
  sees `Vendor`, `Ident` and JDBC and nothing of a driver's loader. `java.sql.*`,
  `Ident` and `Vendor`'s public records are imported into every script. The
  compiled class is cached for the life of the process.
- **Errors.** A script that does not compile fails the session's open with
  the file, the line and Groovy's message. A hook that throws fails the request
  it was serving with the same detail; it is never swallowed into the base's
  answer, which would make a broken script look like a working one.

## 6. Trust

A script is code with the application's rights, in the JVM that holds every
open session and its credentials. So:

1. Profiles and scripts are read only from the application's own
   configuration directory, never from a project, a URL or an imported file.
2. The application records the SHA-256 of every script the user has allowed
   (`<config>/dialects/allowed.json`). A script that is new or has changed is
   not run until the user allows it: the dialog names the profile, the file and
   its path, and says that it runs with rudbman's rights. Refused, the profile
   loads without its script — its data keys still apply — and says so wherever
   it is listed.
3. The dialect picker and the connection's details show which profiles are
   active and which have a script.

## 7. Loading and errors

- Profiles load at start-up and again when the driver manager is opened, so an
  edit does not need a restart. Loading never blocks the window: a profile
  that fails is reported and skipped.
- Unknown keys are errors, with the file and line: a typo silently ignored is a
  setting silently lost. So are an unknown template hole, an enumerated value
  outside its list, a `base` that names nothing, and a cycle of `base`s.
- The driver manager lists each profile's state — loaded, failed with its
  message, script allowed or not — and a definition whose dialect is a failed
  profile shows the failure on its row.
- **Export.** The driver manager can write any dialect, built-in or profile,
  out as a profile with every key filled in — the starting point for a new one,
  and the clearest statement of what its base answers.

## 8. Changes by component

### rudbman-sql and rudbman-core (phase 1, implemented)

- What a profile *means* is `rudbman_sql::dialect::profile`, which keeps the
  crate's rule of no dependencies: `ProfileSpec` is a parsed profile,
  `resolve` turns a set of them into dialects (bases among the set or the
  built-ins, cycles, ids, aliases, templates and words checked), and
  `ProfileSpec::of` exports any dialect. `ProfileVendor` answers from the
  profile and asks its base otherwise.
- How a profile is *spelled* is `rudbman_core::dialect_profile`: `parse` and
  `to_yaml` over `serde_norway`, every key checked and an unknown one reported
  with its line. The `bridge` half is `dialect_profile::bridge::BridgeSpec`
  since phase 3, checked the same way and its template holes too.
- The registry has a runtime half. `Dialect::ALL` stays the built-ins;
  `profile::install` registers resolved profiles, leaked to `'static`, and
  `Dialect::from_id` searches built-ins first, then them.
- `DialectId` gains `Profile`, and `Dialect`'s identity becomes its canonical
  name rather than its id. `DdlError::Unsupported` and
  `DmlError::NoEmptyInsert` carry the `Dialect` rather than a `DialectId`, so a
  profile's messages name its own product.
- **Export** serialises any `Dialect` to a profile. The contract test is the
  round trip: every built-in, exported and loaded back as a profile on
  `generic`, answers every `Vendor` method the way the built-in does.

### rudbman-core and rudbman-app (phase 2, implemented)

- `rudbman_core::dialects` loads `<config>/dialects` — `load_from` reads and
  resolves, `reload` registers what loaded and keeps a `Report` of every
  file's state — at start-up (`start_up`, before the first window) and again
  when the driver manager or its dialect list opens. A reload whose directory
  has not changed since the last keeps the last, so opening the list leaks
  nothing.
- `start_up` first brings in the shipped files by §12's rules
  (`sync_shipped`). The build script of `rudbman-core` compiles in the
  repository's `dialects/` as `dialects::SHIPPED`.
- A loaded profile's `driver` section is offered to `drivers.json` with the
  built-ins, recorded in the same `offered_builtins` list: once, and a
  definition deleted stays deleted.
- The driver manager's dialect field is a picker of the built-ins and every
  profile, which keeps a value that is neither as its own row. Under it: the
  profile's base and file, why it did not load, a `.new` waiting beside it,
  or that the value names nothing. A definition on a failed profile says so
  on its row. **Export…** writes the chosen dialect as a profile (the
  profiles' folder suggested), and **Open folder** opens that folder. Every
  string is in all eight locales.
- Still to come: the trust dialog (phase 4), and sessions opened with the
  resolved `bridge` half of the profile and the path of the Groovy runtime
  when it has a script (§5.3, phase 3).

### Bridge (phase 3, implemented)

- `OPEN_SESSION` takes an optional `profile`: `{ id, base, bridge }`, the
  base being the built-in the chain of bases ends in and `bridge` the half
  merged along it (`rudbman_core::dialects::Report::session_profile`, sent by
  the app's `build_spec`). The script's text and path join it in phase 4.
- Where the design first said `Vendor.of(dbm)`, the bridge keeps every
  caller's `Dialect.of(dbm).vendor()`: `Dialect` became a class of constants
  rather than an enum, and `Dialect.attach` ties a profile's dialect to the
  session's connection (a weak map), which `Dialect.of` asks before the
  product name. Nothing but the open and the close changed.
- `ProfileVendor` extends `Vendor` itself rather than any product's subclass:
  it holds the base's `Vendor` and the profile's data, and answers each
  method from the data or the base. The script comes in phase 4.
- `detect_profiles` lets product-name detection find a profile for a
  connection opened without one — a driver whose dialect is generic SQL, or
  names a profile that did not load; the app sends the loaded profiles that
  have a `bridge.detect`. rudbgen's sample database is phase 5.

### rudbgen and rudbkit

- The profile format, its parsing and its validation move to `rudbkit-core`
  once rudbman's are settled, so both applications read the same file. A file
  written for one works in the other.
- rudbgen reads only the `bridge` half (its bridge answers comments), so a
  profile there needs no `sql` half; one that has it is accepted and ignored.
  A driver definition's own custom comment queries, which rudbgen already lets
  a user write, win over the profile's: they are the more specific of the two.
- Each application reads its own configuration directory. Sharing one is
  §10's open question.

## 9. Testing

- **Round trip** (rudbman-sql): every built-in exported and re-loaded answers
  as itself (§8).
- **Merge rules**: field-wise records, additive word lists, replaced
  templates, `null` versus absent, chains of `base`, and every load error of
  §7 with its file and line.
- **Data keys** (bridge, phase 3): `ProfileVendorTest` checks that an empty
  profile answers every connection-free method as each built-in base does,
  that each key answers from the profile, and, on an in-memory H2, that the
  query keys are run; `profile_answers` in `rudbman-jdbc`'s container suite
  opens MySQL and MariaDB sessions with a profile and by detection.
- **Contract tests** (bridge): for each published hook, a test that a script
  defining it is called with the documented arguments, that its answer is
  used, and that one which throws fails the request with the script's line.
- **Container suites**: a profile that re-describes MariaDB on `base: mysql`
  — its `DROP CONSTRAINT` and sequences as data and script — runs the shared
  suites as `on_every_product!` runs them for the built-in.

## 10. Phases

1. **rudbman-sql profiles** — `ProfileVendor`, the `sql` half and its YAML,
   `base` chains, export and the round-trip tests. Done; no application change.
2. **Loading, shipped profiles and the driver manager** — the directory, the
   start-up sync of the profiles the application ships (§12) and of their
   driver definitions, the picker, states and errors, export, eight locales.
3. **Bridge profiles** — `OPEN_SESSION`'s `profile`, the session's dialect
   (`Dialect.attach`), the `bridge` half's data keys. Done.
4. **Groovy hooks** — API version 1, the runtime download, the allow list and
   its dialog, compile cache and errors, contract tests.
5. **rudbgen and rudbkit** — the shared format and loader, rudbgen's `bridge`
   half.

Each phase is verified as this repository's other changes are, including the
container suites from phase 3 on.

## 11. Open questions

- **One directory or two.** Each application reading its own
  `<config>/dialects` keeps them independent; a shared one would let a profile
  written once serve both. A shared directory needs a home that is neither
  application's.
- **A profile that changes a built-in in place.** Reserved ids (§3) rule it
  out for now; a profile with the built-in as its base, and the driver pointed
  at it, does the same thing without changing what every other definition of
  that product gets.
- **Script hooks for internal types.** §5.2 leaves out the DDL-reconstruction
  hooks. Whether to publish them depends on how often a profile needs them,
  which phase 4 will show.

## 12. Products the application ships as profiles

A database product supported from now on is not a new `dialect/<product>.rs`
and `<Product>Vendor.java`: it is a profile, and a Groovy script where its
bridge needs one, shipped with the application. The built-ins of today stay in
code (§1).

- **In the repository** the shipped profiles sit in `dialects/` at the root,
  each `<id>.yml` with its `<id>.groovy` beside it, and are compiled into the
  binary, the way the locales are. There are none yet; `rudbman-core`'s tests
  load every one there is. A shipped profile is tested like a built-in:
  the round trip, and the container suites against a server of its product.
- **A driver definition.** A shipped profile may carry a `driver` section —
  its class, Maven coordinate, URL template, port and icon — so that a product
  reaches the driver manager without a built-in of its own in `profile.rs`.
- **Reaching the configuration directory.** Installers differ by platform and
  package manager, so the application does this itself, on every start:
  1. `<config>/dialects/shipped.json` records, for each shipped file, the hash
     of the content the application last wrote there.
  2. A shipped file the directory has never been offered is written, and
     recorded. Its driver definition, if it has one, is offered to
     `drivers.json` the same way, once — `offered_builtins`' rule.
  3. A shipped file the user deleted is not written again: it is recorded, and
     absent.
  4. A shipped file the user has not changed — its hash is the recorded one —
     is replaced by a newer shipped version, and the record moves with it.
  5. A shipped file the user has changed is left as the user has it; the
     newer shipped version is written beside it as `<id>.yml.new` (or
     `.groovy.new`) and the driver manager says so.
- **Trust.** A script the application wrote itself is allowed by its hash as
  it is written; one the user then edits is the user's script, and asks (§6).

