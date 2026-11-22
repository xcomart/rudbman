# Driver marks

The products' logos drawn beside each driver in the connection dialog's
picker and the driver manager's list (`src/icons.rs`, `DRIVER_MARKS`). A
driver definition names its mark by stem in its `icon`; one without a mark of
its own gets its dialect's, and one with neither gets `generic.png`. An `icon`
may instead name an image file of the user's — see
`rudbkit_core::dialects::icon_ref` — which is read from disk and falls back
the same way when it cannot be read.

| File | Product | Source |
| --- | --- | --- |
| `altibase.png`, `cubrid.png`, `generic.png`, `h2.png`, `mariadb.png`, `mongodb.png`, `mssql.png`, `mysql.png`, `oracle.png`, `postgresql.png`, `sqlite.png` | as named; `generic` is the fallback cylinder | jdbgen's driver icon set |
| `tidb.png` | TiDB | [Simple Icons](https://simpleicons.org) `tidb` (CC0), in its brand color `#DC150B` |
| `cockroachdb.png` | CockroachDB | Simple Icons `cockroachlabs` (CC0), in its brand color `#6933FF` |
| `firebird.png` | Firebird | [Devicon](https://devicon.dev) `firebird-original` (MIT) |
| `db2.png` | IBM Db2 | Wikimedia Commons, *Logo of IBM DB2 (2005).svg* (public domain; trademarked) |

Each is a 64 px square PNG: the mark rendered or scaled down, its transparent
margin trimmed, and centered with 2 px to spare. The logos are their owners'
trademarks, shown only to say which product a driver connects to.

rudbgen ships the same files in its `assets/drivers`; add a mark to both.
