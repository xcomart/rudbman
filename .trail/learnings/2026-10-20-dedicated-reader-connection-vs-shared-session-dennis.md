---
title: "Dedicated reader connection vs shared session, measured on all DBs"
author: Dennis
date: 2026-10-20
branch: dev
cost: { minutes: 19, tokens: 63803 }
touches:
  - crates/rudbman-jdbc/tests/cursor_memory.rs
---

## Decided
- Measured with crates/rudbman-jdbc/tests/cursor_memory.rs (RUDBMAN_PROBE_MODE=shared|dedicated, 1M x 1000-char rows, 128MB heap): no one mechanism fits every product; the variation is driver configuration, not SQL.
- Shared session is already safe for Oracle and SQLite. H2 is safe but materializes the whole result at EXECUTE (12s for 1M rows) unless LAZY_QUERY_EXECUTION=TRUE (15ms).
- SQL Server: selectMethod=cursor on the shared session removes the OOM, the write/ALTER blocking and the close stall. Without it a dedicated connection keeps the heap flat but its suspended scan holds shared locks: UPDATE ahead of the cursor and ALTER time out.
- PG (reader autocommit off), MySQL (defaultFetchSize=Integer.MIN_VALUE, fetch_size 0) and MariaDB stream at 2-6MB on a dedicated reader connection, and writes from the main session pass.

## Tried and ruled out
- Statement cancel before close: MySQL/MariaDB ignore it once execute returned (close still 25s/3s). Connection.abort() ends it in 1-11ms (rs.close afterwards throws; swallow it).
- Dedicated reader for SQLite: the reader's SHARED lock makes main-session UPDATE/ALTER fail with SQLITE_BUSY.

## Watch out
- Any open streaming cursor blocks ALTER TABLE on PG, MySQL, MariaDB, H2 (10s timeout in the probe); LIMIT/OFFSET paging does not. A grid left open would hang the structure pane in the same app.
- A dedicated reader cannot see uncommitted work of the main session under manual commit.
