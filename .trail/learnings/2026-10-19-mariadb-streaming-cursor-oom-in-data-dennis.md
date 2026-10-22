---
title: MariaDB streaming cursor OOM in data pane
author: Dennis
date: 2026-10-19
branch: dev
touches:
  - crates/rudbman-app/src/data_pane.rs
  - crates/rudbman-app/src/query_source.rs
  - crates/rudbman-sql/src/dialect.rs
  - AGENTS.md
  - opencode.json
  - bridge/build/libs/rudbman-bridge.jar
---

## Decided
- All panes, the explorer and metadata lookups share one JDBC session. An open streaming result set on it is unsafe with MariaDB Connector/J 3.x: any other command on the connection makes StandardClient.execute/readResponse call Statement.fetchRemaining(), which loads the rest of the result into the bridge heap (minute_bars OOM even at 4GB). Closing a stream calls skipRemaining() and reads the rest over the network, so sorts and closed tabs stall.
- pgjdbc ignores fetchSize under autocommit=on and buffers the whole result at EXECUTE.
- DataPane now pages by statement: Dialect::page_clause (rudbman-sql) gives LIMIT/OFFSET for H2/Postgres/MySql/MariaDb/Sqlite. Each page runs with no fetch size, is read whole and closed at once (data_pane.rs Select/query_page/next_page). ORDER BY is the user's column plus PK columns as tie-breakers.
- Oracle/MsSql/Generic keep the old open-cursor path, because their page syntax depends on version or ORDER BY.

## Tried and ruled out
- Relying on setFetchSize alone: MariaDB does stream with it, but the shared session forces fetchRemaining as soon as anything else runs.

## Watch out
- The query pane (query.rs, query/execution.rs) still keeps streaming cursors open and has the same OOM risk.
- Deep OFFSET re-scans the table on each page, and sorting by an unindexed column on a huge table makes every page slow.
- A stale bridge/build/libs/rudbman-bridge.jar fails the LOB test with "operation 0x25 is not implemented". Rebuild it with gradlew. If Gradle's cache lost the H2 jar, run `./gradlew test`.
