---
title: SQL Server grids read through a server cursor
author: Dennis
date: 2026-10-20
branch: dev
cost: { minutes: 29, tokens: 161354 }
touches:
  - bridge/README.md
  - bridge/src/main/java/comart/rudbman/bridge/Bridge.java
  - bridge/src/test/java/comart/rudbman/bridge/FetchRoundTripTest.java
  - crates/rudbman-app/src/data_pane.rs
  - crates/rudbman-app/src/query.rs
  - crates/rudbman-app/src/query/execution.rs
  - crates/rudbman-app/src/query/statements.rs
  - crates/rudbman-app/src/query/tests/statements.rs
  - crates/rudbman-jdbc/src/spec.rs
  - crates/rudbman-jdbc/tests/containers.rs
---

## Decided
- `StatementSpec::server_cursor` (wire `server_cursor`) asks the bridge for a forward-only read-only server cursor. Bridge.execute honours it only for Microsoft's driver (getDriverName starts with "Microsoft JDBC Driver"), as result set type 2004 (TYPE_SS_SERVER_CURSOR_FORWARD_ONLY); every other driver ignores it. jTDS also reports product "Microsoft SQL Server", hence the driver-name check.
- `Dialect::reads_by_server_cursor` (MSSQL only) decides it in the app: data pane cursor path always; query pane via `statements::server_cursor_read` (one plain SELECT/WITH read, no top-level FOR).
- Oracle, SQLite and the LIMIT/OFFSET dialects are unchanged.

## Tried and ruled out
- Connection-wide `selectMethod=cursor`: it puts every SELECT on the session (explorer, metadata helpers) through cursor round trips. Per statement keeps it to grid reads.

## Watch out
- SQL Server refuses FOR JSON / FOR XML / FOR BROWSE in a cursor ("not allowed in a CURSOR statement"); never set the flag on arbitrary SQL.
- A server cursor reports each column's table name where the default result set reports "", so MSSQL query results can now pass the query pane's edit gate (schema stays "" for the default schema).
- containers.rs proves it with sys.dm_exec_cursors(@@spid); cursor_memory.rs RUDBMAN_PROBE_SERVER_CURSOR=1 shows 4MB vs OOM at 1M rows.
