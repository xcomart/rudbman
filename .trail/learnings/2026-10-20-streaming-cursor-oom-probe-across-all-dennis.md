---
title: Streaming-cursor OOM probe across all supported DBs
author: Dennis
date: 2026-10-20
branch: dev
cost: { minutes: 10, tokens: 117077 }
touches:
  - crates/rudbman-jdbc/tests/cursor_memory.rs
  - docker/compose.yml
---

## Decided
- Probed the pre-paging read path (fetch size set, one batch, then a primary_keys describe and another query on the same session) with a 128MB bridge heap and a 300k x 1000-char table, via the opt-in crates/rudbman-jdbc/tests/cursor_memory.rs (one product per process; docker/compose.yml servers).
- Results with default driver props: PostgreSQL (autocommit on) and MySQL Connector/J OOM at EXECUTE (whole result buffered); MariaDB and SQL Server (mssql-jdbc, adaptive buffering) stream until another command runs, then buffer the rest (~295MB) and OOM; Oracle, H2 and SQLite stay at a few MB.
- SQL Server therefore has the same problem as MariaDB but is still on the cursor path. Closing an mssql stream without another command reads the rest off the network (1.5M rows: 3s locally).

## Tried and ruled out
- Driver props as a fix: useCursorFetch=true (MySQL) and selectMethod=cursor (mssql) both keep the heap flat; pgjdbc streams with autocommit off. Not adopted, only measured.

## Watch out
- jcmd cannot attach to the JNI-embedded JVM unless it starts with -XX:+StartAttachListener.
- After a MariaDB OOM the session is unusable; after an mssql OOM it recovers.
