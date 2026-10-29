//! The other end of the comment story: a product that has none.
//!
//! `tests/containers.rs` proves that a comment written on any of the five
//! server products comes back as `remarks`. That leaves the shape of the
//! contract untested at its edge, because all five *can* be commented. SQLite
//! cannot: it has no `COMMENT ON`, no `COMMENT=` clause, no extended
//! properties, and nowhere in `sqlite_master` to keep a comment if somebody
//! invented a syntax for one. What a comment enrichment does on a product with
//! no comments is therefore a question only SQLite can ask, and there are two
//! answers a caller can live with and one it cannot: `null` is right, an empty
//! string is the same thing said differently, and an error — a vendor query
//! that ran anyway and was refused — is a product that cannot be browsed at
//! all. This file is here to keep the third from happening quietly.
//!
//! # Opt-in, and silent when it is out
//!
//! Like the container suite, and for the same reason: `cargo test --workspace`
//! has to stay green on a checkout that has set nothing up. Unlike it, what has
//! to be set up is a path rather than a server — SQLite is a file, so there is
//! no container in this and the URL can point anywhere writable.
//!
//! ```text
//! export RUDBMAN_TEST_SQLITE_URL="jdbc:sqlite:$(mktemp -u)/rudbman.db"
//! cargo test -p rudbman-jdbc --test sqlite
//! ```
//!
//! `jdbc:sqlite:` with no path is also a URL: it opens a private in-memory
//! database, which is enough for everything below.
//!
//! # The driver JAR
//!
//! Found the way the other two suites find theirs — the Gradle cache that
//! `cd bridge && ./gradlew drivers` fills, `org.xerial:sqlite-jdbc` this time —
//! and overridable with `RUDBMAN_TEST_SQLITE_JAR`. A missing JAR is a panic
//! rather than a skip: by the time it is looked for the developer has set a URL
//! and asked for this test to run, and a test that passes because it could not
//! find its driver is exactly the failure this file exists to catch.

use std::path::PathBuf;

use rudbman_jdbc::{
    ConnectionSpec, DdlSource, DescribeRequest, Jvm, JvmConfig, ObjectKind, Session, StatementSpec,
    default_bridge_jar,
};

/// The environment variable that both enables this test and says where the
/// database file is.
const URL_VAR: &str = "RUDBMAN_TEST_SQLITE_URL";

/// The process-wide JVM, started by whichever test needs it first.
fn jvm() -> &'static Jvm {
    Jvm::start(&JvmConfig::new(default_bridge_jar()).with_heap_mb(256))
        .expect("the JVM must start; build the bridge with `cd bridge && ./gradlew jar`")
}

/// Locates the SQLite driver JAR, or fails with instructions.
fn driver_jar() -> PathBuf {
    if let Some(path) = std::env::var_os("RUDBMAN_TEST_SQLITE_JAR") {
        let path = PathBuf::from(path);
        assert!(
            path.is_file(),
            "RUDBMAN_TEST_SQLITE_JAR points at {}, which is not a file",
            path.display()
        );
        return path;
    }
    find_in_gradle_cache("org.xerial", "sqlite-jdbc").unwrap_or_else(|| {
        panic!(
            "the org.xerial:sqlite-jdbc driver JAR was not found.\n\
             \n\
             {URL_VAR} is set, so this test was asked to run, and it needs a driver.\n\
             Fetch the drivers into the Gradle cache with:\n\
             \n    cd bridge && ./gradlew drivers\n\
             \n\
             or point RUDBMAN_TEST_SQLITE_JAR at a JAR you already have."
        )
    })
}

/// Walks `<gradle home>/caches/modules-2/files-2.1/<group>/<artifact>/*/*/<artifact>-<version>.jar`.
///
/// The same two-level walk `tests/containers.rs` does. Copied rather than
/// shared because integration tests are separate binaries with no module in
/// common, and a `mod` shared between them would be compiled into both anyway.
fn find_in_gradle_cache(group: &str, artifact: &str) -> Option<PathBuf> {
    let gradle_home = std::env::var_os("GRADLE_USER_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".gradle")))?;
    let root = gradle_home
        .join("caches/modules-2/files-2.1")
        .join(group)
        .join(artifact);

    let mut newest: Option<(String, PathBuf)> = None;
    for version in std::fs::read_dir(&root).ok()?.flatten() {
        let number = version.file_name().to_string_lossy().into_owned();
        // Only the binary artifact: the same directory holds `-javadoc.jar` and
        // `-sources.jar`, and picking one of those gets a class loader with no
        // classes in it.
        let wanted = format!("{artifact}-{number}.jar");
        for hash in std::fs::read_dir(version.path()).ok()?.flatten() {
            for file in std::fs::read_dir(hash.path()).ok()?.flatten() {
                if file.file_name().to_string_lossy() == wanted
                    && newest
                        .as_ref()
                        .is_none_or(|(best, _)| best.as_str() < number.as_str())
                {
                    newest = Some((number.clone(), file.path()));
                }
            }
        }
    }
    newest.map(|(_, path)| path)
}

/// An open SQLite database, with the tables it made registered for cleanup.
///
/// The file may well be a temporary one the runner throws away, but it may just
/// as well be a database a developer keeps, and a test suite that leaves its
/// fixtures behind in somebody's file is a test suite that fails the second
/// time it runs.
struct Db {
    session: Session,
    table: String,
}

impl Drop for Db {
    fn drop(&mut self) {
        // Ignored, like the container suite's: this runs on the way out of a
        // panicking test too, and a cleanup that panicked would replace the
        // real failure with its own.
        let _ = self.session.execute(&StatementSpec::new(format!(
            "DROP TABLE IF EXISTS {}",
            self.table
        )));
    }
}

impl Db {
    /// Opens the database `RUDBMAN_TEST_SQLITE_URL` names, or answers `None`
    /// and says how to ask for one.
    fn open() -> Option<Db> {
        let Ok(url) = std::env::var(URL_VAR) else {
            println!(
                "skipped: no SQLite database. Set {URL_VAR} to a `jdbc:sqlite:<path>` \
                 URL — any writable path will do, and an empty one is in-memory."
            );
            return None;
        };
        // No credentials: SQLite is a file, and the only authority over it is
        // the filesystem's.
        let spec = ConnectionSpec::new(url, "org.sqlite.JDBC").with_jars([driver_jar()]);
        let session = Session::open(jvm(), &spec)
            .unwrap_or_else(|error| panic!("{URL_VAR} is set but does not connect: {error}"));
        let table = format!("rb_remarks_{}", std::process::id());
        Some(Db { session, table })
    }

    fn exec(&self, sql: &str) {
        self.session
            .execute(&StatementSpec::new(sql.to_string()))
            .unwrap_or_else(|error| panic!("{sql}: {error}"));
    }

    /// A describe request for the fixture table.
    ///
    /// Narrowed by table alone: SQLite's driver reports neither a catalog nor a
    /// schema worth naming, and the two the container suite has to supply are
    /// exactly the products that keep a table somewhere.
    fn describe(&self, kind: &str) -> Vec<serde_json::Map<String, serde_json::Value>> {
        let request = DescribeRequest::new(kind).with_table(&self.table);
        self.session
            .describe(&request)
            .unwrap_or_else(|error| {
                panic!(
                    "describe {kind} of {} was refused, which is the failure this \
                     test exists to catch: a product with no comments must answer \
                     `remarks` as null, not fail: {error}",
                    self.table
                )
            })
            .items
    }
}

/// The wire contract, on a product that can never satisfy the interesting half
/// of it.
///
/// Every `tables` and `columns` item carries `remarks`, and on SQLite what it
/// carries is nothing — null, or the empty string, which is the same fact
/// spelled differently and is read as the same thing here. What it must not be
/// is absent, and what the describe must not be is an error.
#[test]
fn sqlite_reports_no_remarks_because_it_has_none() {
    let Some(db) = Db::open() else { return };

    // Left over from a run that ended badly, on a database file somebody keeps.
    db.exec(&format!("drop table if exists {}", db.table));
    // No comment syntax exists to write here. That is the point: the columns
    // below are as commented as a SQLite column can be.
    db.exec(&format!(
        "create table {} (id integer not null, note varchar(40))",
        db.table
    ));

    let tables = db.describe("tables");
    let table = tables
        .iter()
        .find(|item| item.get("name").and_then(serde_json::Value::as_str) == Some(&db.table))
        .unwrap_or_else(|| panic!("describe tables did not list {}: {tables:?}", db.table));
    assert!(
        table.contains_key("remarks"),
        "the key is part of the contract even where the value cannot be: {table:?}"
    );
    assert_eq!(
        remarks(table),
        None,
        "SQLite has no table comments, so it has none to report: {table:?}"
    );

    let columns = db.describe("columns");
    let names: Vec<_> = columns
        .iter()
        .map(|item| item.get("name").and_then(serde_json::Value::as_str))
        .collect();
    assert_eq!(
        names,
        [Some("id"), Some("note")],
        "the columns the CREATE wrote: {columns:?}"
    );
    for column in &columns {
        assert!(
            column.contains_key("remarks"),
            "no `remarks` in a columns item: {column:?}"
        );
        assert_eq!(
            remarks(column),
            None,
            "SQLite has no column comments either: {column:?}"
        );
    }
}

#[test]
fn sqlite_blob_file_parameters_and_stream_only_reads_round_trip() {
    use rudbman_jdbc::{LobFile, Param, Value};
    let Some(mut db) = Db::open() else { return };
    db.table.push_str("_lob");
    db.exec(&format!(
        "create table {} (id integer primary key, b blob, c text)",
        db.table
    ));
    let bytes: Vec<u8> = (0..(1024 * 1024 + 7)).map(|i| (i % 256) as u8).collect();
    let text = "한🙂\0끝".repeat(1000);
    let insert = format!("insert into {} values (?, ?, ?)", db.table);
    for (id, binary, character) in [(1, bytes.as_slice(), text.as_bytes()), (2, &[], &[])] {
        db.session
            .execute(&StatementSpec::new(&insert).with_params([
                Param::I64(id),
                Param::Lob(LobFile::from_bytes(binary, true).unwrap()),
                Param::Lob(LobFile::from_bytes(character, false).unwrap()),
            ]))
            .unwrap();
    }
    let cursor = db
        .session
        .execute(&StatementSpec::new(format!(
            "select b, c from {} order by id",
            db.table
        )))
        .unwrap();
    let batch = cursor.fetch(10).unwrap();
    for (row, expected) in [bytes.as_slice(), &[]].into_iter().enumerate() {
        let Value::Lob { id, .. } = batch.value(row, 0).unwrap() else {
            panic!("BLOB")
        };
        let file = cursor.cache_lob(id, true).unwrap();
        assert_eq!(file.prefix(bytes.len()).unwrap(), expected);
    }
    assert_eq!(batch.value(0, 1).unwrap(), Value::Str(&text));
    assert_eq!(batch.value(1, 1).unwrap(), Value::Str(""));
}

/// A describe item's `REMARKS`, with null and the empty string read alike —
/// the same normalization `tests/containers.rs` applies, for the same reason.
fn remarks(item: &serde_json::Map<String, serde_json::Value>) -> Option<String> {
    item.get("remarks")
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(str::to_string)
}

/// Every kind of object SQLite has — a table, its index, a view and a trigger —
/// each with the `CREATE` SQLite keeps for it, and the kinds it does not have
/// answered as empty rather than failed.
///
/// The table's text is SQLite's own: the reconstruction from metadata wrote
/// `AUTOINCREMENT` without its `PRIMARY KEY`, which SQLite refuses, and lost
/// the `CHECK`. The view's was a `CREATE TABLE`. The functions folder was an
/// error, because the driver refuses `getFunctions` outright.
#[test]
fn sqlite_answers_every_object_with_its_own_text() {
    let Some(mut db) = Db::open() else { return };
    // Its own table: the other test here runs beside it on the same file.
    db.table = format!("rb_objects_{}", std::process::id());
    let view = format!("{}_v", db.table);
    let trigger = format!("{}_t", db.table);
    let _ = db
        .session
        .execute(&StatementSpec::new(format!("drop view if exists {view}")));
    db.exec(&format!("drop table if exists {}", db.table));
    db.exec(&format!(
        "create table {} (id integer primary key autoincrement, \
         amount numeric default 0 check (amount >= 0), note varchar(20))",
        db.table
    ));
    db.exec(&format!(
        "create index {}_ix on {} (note)",
        db.table, db.table
    ));
    db.exec(&format!(
        "create view {view} as select id from {} where amount > 0",
        db.table
    ));
    db.exec(&format!(
        "create trigger {trigger} after insert on {} begin \
         update {} set note = upper(note) where id = new.id; end",
        db.table, db.table
    ));

    let ddl = |name: &str, kind: ObjectKind| {
        let answer = db
            .session
            .describe_object_ddl(None, None, name, kind, DdlSource::Auto)
            .unwrap_or_else(|error| panic!("ddl of {kind:?} {name}: {error}"));
        assert!(answer.is_native(), "{kind:?}: {}", answer.source);
        answer.ddl
    };
    let table = ddl(&db.table, ObjectKind::Table);
    assert!(table.contains("primary key autoincrement"), "{table}");
    assert!(table.contains("check (amount >= 0)"), "{table}");
    assert!(table.contains(&format!("{}_ix", db.table)), "{table}");
    let text = ddl(&view, ObjectKind::View);
    assert!(text.starts_with("CREATE VIEW"), "{text}");
    let text = ddl(&trigger, ObjectKind::Trigger);
    assert!(text.starts_with("CREATE TRIGGER"), "{text}");

    let triggers = db
        .session
        .describe(&DescribeRequest::new("triggers").with_name(&trigger))
        .expect("triggers are listed")
        .items;
    assert_eq!(triggers.len(), 1, "{triggers:?}");
    assert_eq!(triggers[0].get("table"), Some(&serde_json::json!(db.table)));
    assert_eq!(triggers[0].get("timing"), Some(&serde_json::json!("AFTER")));
    assert_eq!(
        triggers[0].get("events"),
        Some(&serde_json::json!("INSERT"))
    );

    for kind in ["functions", "procedures", "sequences"] {
        let answer = db
            .session
            .describe(&DescribeRequest::new(kind))
            .unwrap_or_else(|error| panic!("{kind} must be empty, not refused: {error}"));
        assert!(answer.items.is_empty(), "{kind}: {:?}", answer.items);
    }
    let _ = db
        .session
        .execute(&StatementSpec::new(format!("drop view if exists {view}")));
}
