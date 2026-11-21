# Shipped dialect profiles

Each `<id>.yml` here is a dialect profile the application ships, with the
`<id>.groovy` its bridge needs beside it, if it needs one. They are compiled
into the binary and written into `<config>/dialects` at start-up, where a user
may change them; the rules are §12 of
[`docs/dialect-profiles.md`](../docs/dialect-profiles.md), and the format is
§4.

| Profile | Product |
| --- | --- |
| `tidb.yml`, `tidb.groovy` | TiDB, on `mysql` |
| `cockroachdb.yml` | CockroachDB, on `postgres` |
| `firebird.yml`, `firebird.groovy` | Firebird 3 and later, on `generic` |

What each rests on, and what its product does not do, is §12.1.

A script shipped here is allowed to run as it is written (§12): the user is
asked only about a script they have changed. A database product supported
from now on is added here rather than as a built-in in code. The tests of `rudbman-core`'s `dialects` module load every
profile here, so one that does not load fails the build's tests.
