---
title: "Page reads go through query_source::read_page"
author: Dennis
date: 2026-10-19
branch: dev
touches:
  - AGENTS.md
  - opencode.json
---

## Decided
- Every LIMIT/OFFSET page statement (data pane `open`/`next_page`, query pane) reads through `query_source::read_page`, which returns `ReadPage { columns, batch, rows }`. Don't add a per-pane page reader again; data_pane's old `query_page` was a parallel-written copy and was removed.
- `render_fetched_batch` is private to query_source.rs since only it renders fetched batches now.

## Tried and ruled out
- Nothing; the two readers were byte-for-byte equivalent, so the swap was mechanical with no behaviour change.

## Watch out
- `read_page` deliberately sets no fetch size and closes the cursor right after rendering (LOB locators die with the result set). Keep both properties: an open streaming cursor on the shared JDBC session is what triggers MariaDB Connector/J's fetchRemaining OOM.
- `rows < limit` is the only end-of-table signal; a full page says nothing about what follows.
