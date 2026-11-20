# Shipped dialect profiles

Each `<id>.yml` here is a dialect profile the application ships, with the
`<id>.groovy` its bridge needs beside it, if it needs one. They are compiled
into the binary and written into `<config>/dialects` at start-up, where a user
may change them; the rules are §12 of
[`docs/dialect-profiles.md`](../docs/dialect-profiles.md), and the format is
§4.

A database product supported from now on is added here rather than as a
built-in in code. The tests of `rudbman-core`'s `dialects` module load every
profile here, so one that does not load fails the build's tests.
