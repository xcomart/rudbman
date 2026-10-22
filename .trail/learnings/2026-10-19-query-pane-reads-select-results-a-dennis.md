---
title: "Query pane reads SELECT results a page at a time (LIMIT/OFFSET appended, not wrapped)"
author: Dennis
date: 2026-10-19
branch: dev
touches:
  - crates/rudbman-app/src/data_pane.rs
  - crates/rudbman-app/src/query.rs
  - crates/rudbman-app/src/query/execution.rs
  - crates/rudbman-app/src/query/results.rs
  - crates/rudbman-app/src/query/statements.rs
  - crates/rudbman-app/src/query/tests/execution.rs
  - crates/rudbman-app/src/query/tests/statements.rs
  - crates/rudbman-app/src/query_source.rs
  - crates/rudbman-sql/src/dialect.rs
  - AGENTS.md
---

## Decided
- The query pane runs a single reading SELECT/WITH as `<statement body> LIMIT n OFFSET m` on dialects with `Dialect::page_clause` (H2, Postgres, MySQL, MariaDB, SQLite). Each page is read whole with no fetch size and closed at once (`query_source::read_page`), so no result set stays open on the shared JDBC session between scrolls. State is `query::More::{Cursor, Offset{pager,next}}`; eligibility is `query/statements.rs::pageable_body`.
- The clause is appended to the user's SQL, not wrapped in a derived table: MariaDB drops an ORDER BY inside a derived table that has no LIMIT, MySQL/MariaDB/H2 reject derived tables with duplicate column names (any `SELECT *` join), and drivers may report the derived alias as the column's table, which closes the edit gate.
- Fallback to the old streaming cursor: no page clause (Oracle/MSSQL/Generic), not a single Read statement, first word not SELECT/WITH, or a top-level LIMIT/OFFSET/FETCH/TOP/FOR/LOCK/PROCEDURE/INTO.

## Tried and ruled out
- A separate metadata connection: it doesn't help pane-vs-pane, and the query pane's own `primary_key` lookup (`resolve_edits`, sent right after the first batch) is enough to make MariaDB Connector/J run `fetchRemaining` and OOM.
- Closing the cursor before other commands: MariaDB's close calls `skipRemaining`, which streams the whole table over the network.

## Watch out
- Every page re-runs the statement, so under autocommit each page sees a fresh snapshot, and without an ORDER BY the order across pages isn't guaranteed. Under manual commit, pages share the user's transaction.
- In gpui tests the window's first frames fire NearEnd and fetch extra pages, so assert "whole pages, next == rows on screen" rather than exactly one page.
- On Windows, Python `open(p,'w')` writes CRLF; pass `newline=''` when rewriting source files.
- data_pane.rs has its own private `query_page`, which duplicates `query_source::read_page`; dedupe later.
