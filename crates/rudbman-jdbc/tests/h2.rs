//! End-to-end tests: a real JVM, the real bridge JAR, a real H2 database.
//!
//! Unit tests can prove that the decoder reads what this crate's own encoder
//! writes. They cannot prove the thing this crate exists for — that Rust reads
//! what *Java* wrote. Everything here goes through `JNI_CreateJavaVM`, the
//! bridge's `Bridge.call`, the H2 JDBC driver and back.
//!
//! # One JVM, many test threads
//!
//! JNI allows exactly one VM per process, and `cargo test` runs every test in
//! this file as a thread of one process. [`Jvm::start`] is therefore idempotent:
//! whichever test gets there first creates the VM and the rest find it. Each
//! test then opens its own session, which brings its own worker thread and its
//! own attachment. Getting this wrong is how the second test onwards starts
//! failing for reasons that have nothing to do with what it tests.
//!
//! # The H2 driver
//!
//! Looked up in this order, and **not** silently skipped when it is missing: a
//! test that passes because it could not find the thing it tests is worse than
//! no test at all.
//!
//! 1. `RUDBMAN_TEST_H2_JAR`
//! 2. the Gradle cache the bridge's own test suite fills:
//!    `~/.gradle/caches/modules-2/files-2.1/com.h2database/h2/**/h2-*.jar`

use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use rudbman_jdbc::{
    BackupDataOptions, BackupSpec, Batch, BridgeErrorKind, ColumnKind, Compression, ConnectionSpec,
    DataMode, DataOptions, DdlOptions, DdlSource, DescribeRequest, Error, ExtractSpec, Job,
    JobProgress, JobState, Jvm, JvmConfig, ObjectRef, OnError, Op, Param, Session, StatementSpec,
    TransferMode, TransferSpec, Value, default_bridge_jar,
};
// A dev-dependency, for the editing section at the bottom of this file: what it
// tests is that the statements one crate plans and the parameters this one binds
// fit together against a real driver.
use rudbman_sql::{
    Dialect, DmlKind, DmlStatement, DmlValue, InsertCell, RowUpdate, TableEdits, plan_edits,
};

/// The process-wide JVM, started by whichever test needs it first.
fn jvm() -> &'static Jvm {
    Jvm::start(&JvmConfig::new(default_bridge_jar()).with_heap_mb(256))
        .expect("the JVM must start; build the bridge with `cd bridge && ./gradlew jar`")
}

/// Locates the H2 driver JAR, or fails with instructions.
fn h2_jar() -> PathBuf {
    if let Some(path) = std::env::var_os("RUDBMAN_TEST_H2_JAR") {
        let path = PathBuf::from(path);
        assert!(
            path.is_file(),
            "RUDBMAN_TEST_H2_JAR points at {}, which is not a file",
            path.display()
        );
        return path;
    }
    find_in_gradle_cache().unwrap_or_else(|| {
        panic!(
            "the H2 driver JAR was not found.\n\
             \n\
             These tests need it: they connect to a real database through a real driver.\n\
             Fetch it into the Gradle cache by running the bridge's own suite:\n\
             \n    cd bridge && ./gradlew test\n\
             \n\
             or point RUDBMAN_TEST_H2_JAR at an h2-*.jar you already have."
        )
    })
}

/// Walks `<gradle home>/caches/modules-2/files-2.1/com.h2database/h2/*/*/h2-*.jar`.
///
/// A hand-rolled two-level walk rather than a glob dependency: the shape is
/// fixed and this is the only place in the crate that needs it.
fn find_in_gradle_cache() -> Option<PathBuf> {
    let gradle_home = std::env::var_os("GRADLE_USER_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".gradle")))?;
    let root = gradle_home.join("caches/modules-2/files-2.1/com.h2database/h2");

    let mut newest: Option<(String, PathBuf)> = None;
    for version in std::fs::read_dir(&root).ok()?.flatten() {
        let number = version.file_name().to_string_lossy().into_owned();
        // Only the binary artefact. The same version directory also holds
        // `h2-<version>-javadoc.jar` and `-sources.jar`, and picking one of
        // those gets a class loader with no classes in it — which surfaces as a
        // ClassNotFoundException for the driver, a long way from the cause.
        let wanted = format!("h2-{number}.jar");
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

/// A URL for a database no other test shares.
fn fresh_url() -> String {
    static SEQ: AtomicU32 = AtomicU32::new(0);
    // DB_CLOSE_DELAY=-1 keeps the database alive between connections, which is
    // what lets a test reconnect to one it created.
    format!(
        "jdbc:h2:mem:rudbman{};DB_CLOSE_DELAY=-1",
        SEQ.fetch_add(1, Ordering::Relaxed)
    )
}

/// A spec for an H2 in-memory database, with the driver loaded from its JAR.
///
/// Note the JAR list: the JVM's own class path holds nothing but the bridge, so
/// the driver really does come up in a child class loader, the way a
/// user-installed driver will.
fn spec(url: &str, user: &str, password: &str) -> ConnectionSpec {
    ConnectionSpec::new(url, "org.h2.Driver")
        .with_credentials(user, password)
        .with_jars([h2_jar()])
}

/// Opens a session against a fresh database.
fn session() -> Session {
    Session::open(jvm(), &spec(&fresh_url(), "sa", "")).expect("H2 accepts the connection")
}

/// Runs a statement that returns no rows.
fn exec(session: &Session, sql: &str) {
    session
        .execute(&StatementSpec::new(sql))
        .unwrap_or_else(|error| panic!("{sql}: {error}"));
}

/// Runs a query and reads a single batch of up to `limit` rows.
fn fetch_one(session: &Session, sql: &str, limit: u32) -> Batch {
    let cursor = session
        .execute(&StatementSpec::new(sql))
        .unwrap_or_else(|error| panic!("{sql}: {error}"));
    cursor.fetch(limit).expect("the batch decodes")
}

// --- session lifecycle -----------------------------------------------------

#[test]
fn boot_connect_ping_describe_the_session_and_close() {
    let session = Session::open(jvm(), &spec(&fresh_url(), "sa", "")).expect("connects");

    let ping = session.ping().expect("pings");
    assert!(ping.ok, "H2 answered a ping with not-ok");
    assert!(ping.elapsed_ms >= 0);

    let info = session.info().expect("describes itself");
    assert_eq!(info.product_name.as_deref(), Some("H2"));
    assert!(info.driver_version.is_some());
    assert_eq!(info.auto_commit, Some(true));
    assert_eq!(info.identifier_quote.as_deref(), Some("\""));

    session.close().expect("closes");
}

#[test]
fn the_jvm_is_one_per_process_however_often_it_is_asked_for() {
    let first = jvm();
    // A second start with a different configuration hands back the running VM
    // rather than failing or, worse, trying to build a second one: JNI allows
    // exactly one per process. Every test in this file depends on that.
    let second = Jvm::start(&JvmConfig::new("/nonexistent/rudbman-bridge.jar").with_heap_mb(64))
        .expect("the running VM is returned, and its JAR is not re-checked");
    assert!(std::ptr::eq(first, second));
    assert!(std::ptr::eq(first, Jvm::get().expect("started")));
}

#[test]
fn two_sessions_work_at_the_same_time_on_their_own_threads() {
    // One worker thread per connection is the whole thread model; this is what
    // it buys. Both sessions are used concurrently and neither serialises
    // against the other.
    std::thread::scope(|scope| {
        let handles: Vec<_> = (0..2)
            .map(|n| {
                scope.spawn(move || {
                    let session = session();
                    exec(&session, "create table t (id integer)");
                    for row in 0..20 {
                        exec(&session, &format!("insert into t values ({row})"));
                    }
                    let batch = fetch_one(&session, "select count(*) from t", 1);
                    assert_eq!(batch.value(0, 0), Some(Value::I64(20)), "session {n}");
                })
            })
            .collect();
        for handle in handles {
            handle.join().expect("both sessions finished");
        }
    });
}

#[test]
fn transaction_control_reaches_the_connection() {
    let session = session();
    exec(&session, "create table t (id integer)");

    session.set_auto_commit(false).expect("auto-commit off");
    exec(&session, "insert into t values (1)");
    session.rollback().expect("rolls back");

    let batch = fetch_one(&session, "select count(*) from t", 10);
    assert_eq!(
        batch.value(0, 0),
        Some(Value::I64(0)),
        "the insert survived a rollback"
    );

    exec(&session, "insert into t values (2)");
    session.commit().expect("commits");
    session.set_auto_commit(true).expect("auto-commit on");

    let batch = fetch_one(&session, "select count(*) from t", 10);
    assert_eq!(batch.value(0, 0), Some(Value::I64(1)));
}

// --- error envelopes -------------------------------------------------------

#[test]
fn a_url_the_driver_refuses_is_a_driver_error() {
    // Not an exception on the Java side: `Driver.connect` returns null, which
    // the JDBC specification defines as "I do not understand this URL".
    let error = Session::open(jvm(), &spec("jdbc:postgresql://nowhere/db", "sa", ""))
        .expect_err("H2 must not accept a PostgreSQL URL");
    let Error::Bridge(error) = error else {
        panic!("expected an error envelope, got {error:?}")
    };
    assert_eq!(error.kind, BridgeErrorKind::Driver);
    assert!(
        error.message.contains("does not accept"),
        "unhelpful message: {}",
        error.message
    );
}

#[test]
fn a_missing_driver_class_is_a_driver_error() {
    let spec = ConnectionSpec::new(fresh_url(), "com.example.NoSuchDriver").with_jars([h2_jar()]);
    let error = Session::open(jvm(), &spec).expect_err("there is no such class");
    let Error::Bridge(error) = error else {
        panic!("expected an error envelope, got {error:?}")
    };
    assert_eq!(error.kind, BridgeErrorKind::Driver);
    assert!(
        error.message.contains("com.example.NoSuchDriver"),
        "the message must name the class: {}",
        error.message
    );
}

#[test]
fn a_wrong_password_is_a_sql_error_carrying_a_sqlstate() {
    let url = fresh_url();
    // The first connection creates the in-memory database with these
    // credentials; DB_CLOSE_DELAY keeps it around for the second attempt.
    let created = Session::open(jvm(), &spec(&url, "sa", "secret")).expect("creates the database");

    let error = Session::open(jvm(), &spec(&url, "sa", "wrong")).expect_err("wrong password");
    let Error::Bridge(error) = error else {
        panic!("expected an error envelope, got {error:?}")
    };
    assert_eq!(error.kind, BridgeErrorKind::Sql);
    assert_eq!(
        error.sql_state_class(),
        Some("28"),
        "invalid authorisation specification: {error:?}"
    );
    assert_ne!(error.vendor_code, 0, "H2 numbers this one: {error:?}");

    drop(created);
}

#[test]
fn a_missing_table_is_a_sql_error_whose_class_is_42() {
    let session = session();
    let error = session
        .execute(&StatementSpec::new("select * from no_such_table"))
        .expect_err("there is no such table");
    let Error::Bridge(error) = error else {
        panic!("expected an error envelope, got {error:?}")
    };
    assert_eq!(error.kind, BridgeErrorKind::Sql);
    // H2 says 42S04 where other drivers say 42S02. Only the class is portable,
    // which is exactly why this asserts the class and not the code.
    assert_eq!(error.sql_state_class(), Some("42"), "{error:?}");
    assert!(
        error.stack().is_some(),
        "the stack is kept for the debug log"
    );
}

#[test]
fn an_unimplemented_operation_says_so_rather_than_reading_as_unknown() {
    let session = session();
    let error = session
        .call_raw(Op::LobRead, session.handle(), 0, Some(b"{}".to_vec()))
        .expect_err("LOB_READ is a later milestone");
    let Error::Bridge(error) = error else {
        panic!("expected an error envelope, got {error:?}")
    };
    assert_eq!(error.kind, BridgeErrorKind::Protocol);
    assert!(
        error.is_not_implemented(),
        "waiting for a milestone, not a table mismatch: {error:?}"
    );
    assert!(!error.is_unknown_operation());
}

// --- metadata --------------------------------------------------------------

#[test]
fn describe_answers_every_implemented_kind() {
    let session = session();
    exec(
        &session,
        "create table parent (id integer primary key, code varchar(10) unique)",
    );
    exec(
        &session,
        "create table child (id integer primary key, parent_id integer references parent(id))",
    );

    for kind in ["catalogs", "schemas", "tables", "type_info"] {
        let result = session
            .describe(&DescribeRequest::new(kind))
            .unwrap_or_else(|error| panic!("describe {kind}: {error}"));
        assert_eq!(result.kind, kind);
        assert!(!result.items.is_empty(), "{kind} came back empty");
    }

    let columns = session
        .describe(&DescribeRequest::new("columns").with_table("CHILD"))
        .expect("describes columns");
    let names: Vec<&str> = columns
        .items
        .iter()
        .filter_map(|item| item.get("name")?.as_str())
        .collect();
    assert_eq!(names, ["ID", "PARENT_ID"]);

    for (kind, table) in [
        ("primary_keys", "CHILD"),
        ("imported_keys", "CHILD"),
        ("exported_keys", "PARENT"),
        ("indexes", "PARENT"),
    ] {
        let result = session
            .describe(&DescribeRequest::new(kind).with_table(table))
            .unwrap_or_else(|error| panic!("describe {kind}: {error}"));
        assert!(
            !result.items.is_empty(),
            "{kind} on {table} came back empty"
        );
    }

    // A kind nobody has heard of is a protocol error, and it is *not* the
    // "not implemented" one — that distinction is what tells a caller whether to
    // wait for a milestone or to go and fix a table mismatch.
    let error = session
        .describe(&DescribeRequest::new("phlogiston"))
        .expect_err("there is no such kind");
    let Error::Bridge(error) = error else {
        panic!("expected an error envelope, got {error:?}")
    };
    assert_eq!(error.kind, BridgeErrorKind::Protocol);
    assert!(!error.is_not_implemented(), "{error:?}");
    assert!(error.message.contains("unknown describe kind"), "{error:?}");
}

/// The driver definition's own comment queries, inherited from jdbgen.
///
/// H2 answers `getTables` and `getColumns` with an empty `REMARKS` for a table
/// nobody commented, which is exactly the gap these two statements exist to
/// fill — and H2 is not one of the products the bridge has a built-in comment
/// query for, so anything that arrives here came from the spec.
///
/// `${schema}` travels **unsubstituted**: filling it in on this side would be
/// doing the bridge's job and testing none of it.
#[test]
fn the_comment_queries_on_the_session_fill_in_what_the_driver_left_blank() {
    let mut spec = spec(&fresh_url(), "sa", "");
    spec.table_comments_sql = Some(
        "SELECT TABLE_NAME, 'custom:' || TABLE_NAME FROM INFORMATION_SCHEMA.TABLES \
         WHERE TABLE_SCHEMA = '${schema}'"
            .to_string(),
    );
    spec.column_comments_sql = Some(
        "SELECT TABLE_NAME, COLUMN_NAME, 'custom:' || COLUMN_NAME \
         FROM INFORMATION_SCHEMA.COLUMNS WHERE TABLE_SCHEMA = '${schema}'"
            .to_string(),
    );
    let session = Session::open(jvm(), &spec).expect("H2 accepts the connection");
    exec(&session, "create table plain (id integer primary key)");

    let tables = session
        .describe(&DescribeRequest::new("tables").with_schema("PUBLIC"))
        .expect("describes tables");
    let remarks = |items: &[serde_json::Map<String, serde_json::Value>], name: &str| {
        items
            .iter()
            .find(|item| item.get("name").and_then(serde_json::Value::as_str) == Some(name))
            .and_then(|item| item.get("remarks")?.as_str())
            .map(str::to_owned)
    };
    assert_eq!(
        remarks(&tables.items, "PLAIN").as_deref(),
        Some("custom:PLAIN"),
        "the table comment query did not reach the describe: {:?}",
        tables.items
    );

    let columns = session
        .describe(
            &DescribeRequest::new("columns")
                .with_schema("PUBLIC")
                .with_table("PLAIN"),
        )
        .expect("describes columns");
    assert_eq!(
        remarks(&columns.items, "ID").as_deref(),
        Some("custom:ID"),
        "the column comment query did not reach the describe: {:?}",
        columns.items
    );

    session.close().expect("closes");
}

#[test]
fn routines_arrive_with_their_signatures_attached() {
    let session = session();
    exec(&session, "create schema app");
    // A method that is on every JVM's boot classpath and has exactly one
    // overload, so the alias resolves without a classpath of our own.
    exec(
        &session,
        "create alias app.f_sqrt for 'java.lang.Math.sqrt'",
    );

    let procedures = session
        .describe(&DescribeRequest::new("procedures").with_schema("APP"))
        .expect("describes procedures");
    assert_eq!(procedures.kind, "procedures");
    let routine = procedures
        .items
        .iter()
        .find(|item| item.get("name").and_then(|v| v.as_str()) == Some("F_SQRT"))
        .unwrap_or_else(|| panic!("F_SQRT is missing from {:?}", procedures.items));
    assert_eq!(routine.get("schema").and_then(|v| v.as_str()), Some("APP"));

    // The signature travels with the routine: a schema of two hundred of these
    // would otherwise be two hundred round trips before the tree could draw one
    // of them.
    let parameters = routine
        .get("parameters")
        .and_then(|v| v.as_array())
        .expect("parameters are inlined");
    let modes: Vec<&str> = parameters
        .iter()
        .filter_map(|p| p.get("mode_name")?.as_str())
        .collect();
    assert!(
        modes.contains(&"RETURN") && modes.contains(&"IN"),
        "expected a return value and an argument: {parameters:?}"
    );

    // H2 2.x answers getFunctions with nothing at all and files CREATE ALIAS
    // routines under getProcedures. An empty list is "filed elsewhere", not
    // "none" — and it is an answer, not an error.
    let functions = session
        .describe(&DescribeRequest::new("functions").with_schema("APP"))
        .expect("describes functions");
    assert!(
        functions.items.is_empty(),
        "H2 is expected to file this alias under procedures: {:?}",
        functions.items
    );

    // Narrowing by exact name.
    let one = session
        .describe(
            &DescribeRequest::new("procedures")
                .with_schema("APP")
                .with_name("F_SQRT"),
        )
        .expect("narrows to one routine");
    assert_eq!(one.items.len(), 1);
}

#[test]
fn sequences_come_from_the_vendor_catalogue() {
    let session = session();
    exec(&session, "create schema app");
    exec(
        &session,
        "create sequence app.seq_order start with 100 increment by 5 \
         minvalue 10 maxvalue 100000 cycle cache 20",
    );

    let sequences = session
        .describe(&DescribeRequest::new("sequences").with_schema("APP"))
        .expect("describes sequences");
    let sequence = sequences
        .items
        .iter()
        .find(|item| item.get("name").and_then(|v| v.as_str()) == Some("SEQ_ORDER"))
        .unwrap_or_else(|| panic!("SEQ_ORDER is missing from {:?}", sequences.items));

    // Every value but `cycle` is a string: an Oracle sequence maximum is
    // NUMBER(28) and does not fit an i64.
    assert_eq!(
        sequence.get("start_value").and_then(|v| v.as_str()),
        Some("100")
    );
    assert_eq!(
        sequence.get("increment").and_then(|v| v.as_str()),
        Some("5")
    );
    assert_eq!(sequence.get("cycle").and_then(|v| v.as_bool()), Some(true));

    // A product the bridge has no catalogue query for answers an empty list,
    // and so does a schema that holds no sequences. Neither is an error.
    let none = session
        .describe(&DescribeRequest::new("sequences").with_schema("NO_SUCH_SCHEMA"))
        .expect("an empty list is a correct answer");
    assert!(none.items.is_empty());
}

// --- DDL: the one kind that answers with a document ------------------------

#[test]
fn describe_ddl_asks_the_server_for_its_own_create_text() {
    let session = session();
    exec(
        &session,
        "create table parent (id integer primary key, code varchar(10) not null)",
    );

    let ddl = session
        .describe_ddl(None, Some("PUBLIC"), "PARENT", DdlSource::Auto)
        .expect("H2 can quote its own DDL back");
    assert!(
        ddl.is_native(),
        "H2 has SCRIPT, so auto should not have fallen back: {ddl:?}"
    );
    assert!(!ddl.is_reconstructed());
    let text = ddl.ddl.to_uppercase();
    assert!(text.contains("CREATE"), "{}", ddl.ddl);
    assert!(text.contains("PARENT"), "{}", ddl.ddl);
    assert!(text.contains("CODE"), "{}", ddl.ddl);

    // Asking for the native path explicitly gets the same answer here; on a
    // product without one it would be a sql error rather than a silent
    // downgrade.
    let native = session
        .describe_ddl(None, Some("PUBLIC"), "PARENT", DdlSource::Native)
        .expect("H2 has a native path");
    assert!(native.is_native());
}

#[test]
fn describe_ddl_can_be_forced_through_the_reverse_generated_path() {
    let session = session();
    exec(
        &session,
        "create table child (
             id        integer primary key,
             parent_id integer not null,
             note      varchar(40))",
    );
    exec(&session, "create index idx_child_note on child (note)");

    let ddl = session
        .describe_ddl(None, Some("PUBLIC"), "CHILD", DdlSource::Metadata)
        .expect("the fallback works on every driver, which is why it exists");
    assert!(
        ddl.is_reconstructed(),
        "metadata was demanded, so the native path must not have answered: {ddl:?}"
    );

    let text = ddl.ddl.to_uppercase();
    assert!(text.contains("CREATE TABLE"), "{}", ddl.ddl);
    for column in ["ID", "PARENT_ID", "NOTE"] {
        assert!(text.contains(column), "{column} missing from {}", ddl.ddl);
    }
    assert!(text.contains("PRIMARY KEY"), "{}", ddl.ddl);
    // An index that does not merely back a declared key is emitted separately.
    assert!(text.contains("IDX_CHILD_NOTE"), "{}", ddl.ddl);

    // For display, but close enough to run: replaying it in a database that has
    // never seen the table is the strongest cheap check there is.
    let elsewhere = Session::open(jvm(), &spec(&fresh_url(), "sa", "")).expect("connects");
    elsewhere
        .execute(&StatementSpec::new(&ddl.ddl))
        .expect("the reconstructed DDL replays");
}

#[test]
fn ddl_is_not_reachable_through_the_list_shaped_path() {
    // The reason `describe_ddl` exists: the answer is a document, and a caller
    // that asks for it through `describe` gets a parse failure rather than a
    // silently empty list.
    let session = session();
    exec(&session, "create table t (id integer)");

    let error = session
        .describe(&DescribeRequest::new("ddl").with_table("T"))
        .expect_err("the shapes differ");
    assert!(
        matches!(error, Error::Protocol(_)),
        "expected a shape mismatch, got {error:?}"
    );
}

// --- the round trip that matters -------------------------------------------

/// Creates the fixture table used by the type round-trip tests.
fn create_fixture(session: &Session) {
    exec(
        session,
        "create table t (
             rn        integer,
             i_val     integer,
             big_val   bigint,
             dbl_val   double precision,
             real_val  real,
             bool_val  boolean,
             txt_val   varchar(200),
             dec_val   numeric(20,8),
             date_val  date,
             ts_val    timestamp,
             bin_val   varbinary(64),
             clob_val  clob,
             null_val  varchar(10))",
    );
}

#[test]
fn every_type_survives_execute_fetch_and_decode() {
    let session = session();
    create_fixture(&session);

    // Bound parameters, not literals: the typed parameter forms are half the
    // contract and a DECIMAL(20,8) sent as a JSON number would arrive rounded.
    let insert = StatementSpec::new("insert into t values (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)")
        .with_params([
            Param::I64(1),
            Param::I64(42),
            Param::I64(i64::MAX),
            Param::F64(3.5),
            Param::F64(0.1),
            Param::Bool(true),
            Param::Str("hello ünïcode ✓".to_string()),
            Param::Decimal("123456789012.12345678".to_string()),
            Param::Date("2024-03-15".to_string()),
            Param::Timestamp("2024-03-15 12:34:56.789".to_string()),
            Param::Bytes(vec![0xde, 0xad, 0xbe, 0xef]),
            Param::Str("clob body".to_string()),
            Param::Null,
        ]);
    session.execute(&insert).expect("the insert binds");
    exec(
        &session,
        "insert into t values (2, null, null, null, null, false, '', null, null, null, null, null, null)",
    );

    let cursor = session
        .execute(&StatementSpec::new("select * from t order by rn"))
        .expect("selects");

    // The logical types are stable for the life of the result; the physical
    // kinds below are per batch.
    let columns = cursor.columns().to_vec();
    assert_eq!(columns.len(), 13);
    assert_eq!(columns[1].jdbc_type.as_deref(), Some("INTEGER"));
    // DECIMAL and NUMERIC are the same thing under two `java.sql.Types`
    // constants, and which one a driver reports is its own business. What is
    // not: an exact decimal travels as text.
    assert!(
        matches!(columns[7].jdbc_type.as_deref(), Some("DECIMAL" | "NUMERIC")),
        "{:?}",
        columns[7].jdbc_type
    );
    assert_eq!(columns[7].kind_hint(), Some(ColumnKind::Str));
    assert_eq!(columns[7].scale, 8);
    assert!(
        columns[4].is_single_precision(),
        "real_val is a 32-bit REAL"
    );

    let batch = cursor.fetch(100).expect("the batch decodes");
    assert_eq!(batch.rows(), 2);
    assert!(batch.is_last(), "two rows out of two: the driver ran dry");

    // Row 0: every value present.
    assert_eq!(batch.value(0, 1), Some(Value::I64(42)));
    assert_eq!(batch.value(0, 2), Some(Value::I64(i64::MAX)));
    assert_eq!(batch.value(0, 3), Some(Value::F64(3.5)));
    assert_eq!(batch.value(0, 5), Some(Value::Bool(true)));
    assert_eq!(batch.value(0, 6), Some(Value::Str("hello ünïcode ✓")));
    assert_eq!(
        batch.value(0, 7),
        Some(Value::Str("123456789012.12345678")),
        "a DECIMAL(20,8) has to arrive digit for digit; through an f64 it could not"
    );
    assert_eq!(batch.value(0, 8), Some(Value::Str("2024-03-15")));
    assert_eq!(
        batch.value(0, 9),
        Some(Value::Str("2024-03-15 12:34:56.789")),
        "the driver's own text is the authority on the time zone"
    );
    assert_eq!(
        batch.value(0, 10),
        Some(Value::Bin(&[0xde, 0xad, 0xbe, 0xef]))
    );
    assert!(
        matches!(batch.value(0, 11), Some(Value::Lob { id, size }) if id != 0 && size == Some(9)),
        "a CLOB is a reference plus a length in characters, never inlined: {:?}",
        batch.value(0, 11)
    );

    // A REAL crosses widened to f64; printed as one it would read
    // 0.10000000149011612.
    let Some(real) = batch.value(0, 4) else {
        panic!("real_val is missing")
    };
    assert_eq!(real.to_text(&columns[4]).as_deref(), Some("0.1"));

    // Row 1: the NULLs, and the empty string that is not one.
    for column in [1, 2, 3, 4, 7, 8, 9, 10, 11] {
        assert_eq!(
            batch.value(1, column),
            Some(Value::Null),
            "column {column} of row 1 should be NULL"
        );
    }
    assert_eq!(
        batch.value(1, 6),
        Some(Value::Str("")),
        "an empty string is not a NULL, and only the bitmap says so"
    );
    assert!(!batch.columns()[6].is_null(1));

    // The all-NULL column is shortened, whatever its declared type.
    assert_eq!(batch.columns()[12].kind(), ColumnKind::Nulls);
}

#[test]
fn a_columns_kind_changes_from_batch_to_batch() {
    let session = session();
    exec(&session, "create table t (rn integer, v integer)");
    exec(&session, "insert into t values (1, 100)");
    exec(&session, "insert into t values (2, null)");

    let cursor = session
        .execute(&StatementSpec::new("select rn, v from t order by rn"))
        .expect("selects");

    // The EXECUTE hint says what a full batch would look like...
    assert_eq!(cursor.columns()[1].kind_hint(), Some(ColumnKind::I64));

    // ...but one row at a time, the second batch is all NULL and arrives
    // shortened. A decoder that read the kind once per cursor would read the
    // bitmap of batch two as an I64 value area.
    let first = cursor.fetch(1).expect("first batch");
    assert_eq!(first.rows(), 1);
    assert_eq!(first.columns()[1].kind(), ColumnKind::I64);
    assert_eq!(first.value(0, 1), Some(Value::I64(100)));

    let second = cursor.fetch(1).expect("second batch");
    assert_eq!(second.rows(), 1);
    assert_eq!(
        second.columns()[1].kind(),
        ColumnKind::Nulls,
        "an all-NULL batch is shortened regardless of the declared type"
    );
    assert_eq!(second.value(0, 1), Some(Value::Null));
    assert_eq!(
        second.value(0, 0),
        Some(Value::I64(2)),
        "the other column is untouched"
    );
}

#[test]
fn a_batch_that_fills_its_limit_exactly_is_not_the_last_one() {
    let session = session();
    exec(&session, "create table t (id integer)");
    exec(&session, "insert into t select x from system_range(1, 10)");

    let cursor = session
        .execute(&StatementSpec::new("select id from t order by id"))
        .expect("selects");

    let full = cursor.fetch(10).expect("ten rows");
    assert_eq!(full.rows(), 10);
    assert!(
        !full.is_last(),
        "the driver had not run out of rows yet, and there is no way to know it would"
    );

    let terminal = cursor.fetch(10).expect("the terminal batch");
    assert_eq!(terminal.rows(), 0);
    assert!(terminal.is_last());
    // Every column of a zero-row batch is NULLS with an empty payload, which is
    // what saves STR from needing an offsets[1].
    assert_eq!(terminal.columns()[0].kind(), ColumnKind::Nulls);
}

#[test]
fn update_counts_and_the_exhaustion_of_a_statement() {
    let session = session();
    exec(&session, "create table t (id integer)");

    let mut cursor = session
        .execute(&StatementSpec::new(
            "insert into t select x from system_range(1, 3)",
        ))
        .expect("inserts");
    assert_eq!(cursor.result().update_count, 3);
    assert!(!cursor.result().has_result_set);
    assert_ne!(cursor.handle(), 0, "even an update gets a cursor");

    // FETCH on a cursor with no result set is an empty terminal batch, not an
    // error, so every cursor can be treated the same way.
    let batch = cursor.fetch(10).expect("an empty batch");
    assert_eq!(batch.rows(), 0);
    assert!(batch.is_last());

    // `may_have_more` is a hint: keep asking until the three exhaustion
    // conditions hold together.
    let mut rounds = 0;
    while !cursor.result().is_exhausted() && rounds < 8 {
        cursor.more_results().expect("advances");
        rounds += 1;
    }
    assert!(cursor.result().is_exhausted(), "after {rounds} rounds");
    assert!(!cursor.result().may_have_more);
    assert_eq!(cursor.result().update_count, -1);
    assert!(cursor.result().columns.is_empty());
}

// --- cancellation ----------------------------------------------------------

#[test]
fn cancel_reaches_a_statement_the_worker_is_blocked_in() {
    // A cross join of two ranges: H2 checks its cancellation flag between rows,
    // and 4x10^10 of them leave a window wide enough not to race the query.
    const LONG_QUERY: &str = "select count(*) from system_range(1, 200000) a, \
                              system_range(1, 200000) b where a.x <> b.x";

    let session = session();
    let canceller = session.canceller();

    std::thread::scope(|scope| {
        let running = scope.spawn(|| session.execute(&StatementSpec::new(LONG_QUERY)));

        // The statement has to reach the driver before a cancel can bite, and
        // there is no way to observe that from outside — so keep asking.
        let mut cancelled = 0;
        for _ in 0..150 {
            if running.is_finished() {
                break;
            }
            std::thread::sleep(Duration::from_millis(100));
            cancelled += canceller.cancel().expect("cancel is answered");
        }

        let result = running.join().expect("the worker thread survived");
        assert!(cancelled > 0, "no statement was ever reached by a cancel");

        let error = result.expect_err("a cancelled statement comes back as an error");
        let Error::Bridge(error) = error else {
            panic!("expected an error envelope, got {error:?}")
        };
        assert_eq!(error.kind, BridgeErrorKind::Sql);
        assert!(
            error.sql_state.is_some(),
            "a cancelled statement still carries a SQLSTATE: {error:?}"
        );
    });

    // The session is still usable: cancelling aborts a statement, not a
    // connection.
    assert!(session.ping().expect("still alive").ok);
    assert_eq!(
        session.cancel().expect("cancel on an idle session"),
        0,
        "nothing was running, which is not an error"
    );
}

// --- jobs: the data plane --------------------------------------------------

/// Polls until the job stops running and returns that one reading.
///
/// The reading that reports a terminal state is also the one that retires the
/// handle, so it is the last thing anybody may ask this job for — which is why
/// it is returned rather than merely observed.
fn drain(job: &mut Job) -> JobProgress {
    for _ in 0..1200 {
        let progress = job.poll().expect("the job answers a poll");
        if progress.is_terminal() {
            return progress;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    panic!("the job never reached a terminal state");
}

#[test]
fn an_extract_job_writes_a_script_without_a_row_crossing_the_boundary() {
    let session = session();
    exec(
        &session,
        "create table t (id integer not null primary key, txt varchar(40))",
    );
    exec(
        &session,
        "insert into t values (1, 'plain'), (2, 'it''s quoted'), (3, null)",
    );

    let dir = tempfile::tempdir().expect("a temp directory");
    let path = dir.path().join("extract.sql");
    let mut job = session
        .start_job(
            &ExtractSpec::new(&path)
                .with_object(ObjectRef::new("T").with_schema("PUBLIC"))
                .with_ddl(DdlOptions::included())
                .with_data(DataOptions::included(DataMode::Insert)),
        )
        .expect("the specification is accepted");

    let end = drain(&mut job);
    assert_eq!(end.state, JobState::Done, "{end:?}");
    assert_eq!(end.rows_done, 3, "{end:?}");
    assert_eq!(end.phase, "done");
    assert!(end.errors.is_empty(), "{:?}", end.errors);
    // No COUNT(*) is run up front, so neither of these is ever known here.
    assert_eq!(end.rows_total, None);
    assert_eq!(end.eta_s, None);

    // The file was written by the JVM: nothing in this test ever saw a row.
    let script = std::fs::read_to_string(&path).expect("the script is where it was asked for");
    assert_eq!(
        end.bytes as usize,
        script.len(),
        "the byte count lags while a job runs and is exact once it has ended"
    );
    assert!(script.contains("CREATE TABLE"), "{script}");
    assert!(script.contains("INSERT INTO"), "{script}");
    assert!(
        script.contains("'it''s quoted'"),
        "an apostrophe has to be doubled, or the script does not run: {script}"
    );
    assert!(
        script.contains("NULL"),
        "the third row is a NULL and has to say so: {script}"
    );
}

#[test]
fn the_row_format_and_the_predicate_reach_the_bridge_by_name() {
    // The serde test pins the JSON this crate writes; this one pins that the
    // bridge reads those names. A member silently ignored on the far side looks
    // exactly like a member that worked.
    let session = session();
    exec(&session, "create table t (id integer, txt varchar(20))");
    exec(
        &session,
        "insert into t values (1, 'keep'), (2, ''), (3, null)",
    );

    let dir = tempfile::tempdir().expect("a temp directory");
    let path = dir.path().join("rows.csv");
    let mut job = session
        .start_job(
            &ExtractSpec::new(&path)
                .with_object(ObjectRef::new("T").with_schema("PUBLIC"))
                .with_data(DataOptions::included(DataMode::Csv).with_where("id <> 1")),
        )
        .expect("the specification is accepted");

    let end = drain(&mut job);
    assert_eq!(end.state, JobState::Done, "{end:?}");
    assert_eq!(end.rows_done, 2, "the predicate was applied: {end:?}");

    let csv = std::fs::read_to_string(&path).expect("the file is there");
    let lines: Vec<&str> = csv.lines().collect();
    assert_eq!(lines[0], "ID,TXT", "a header row of column names: {csv}");
    // The convention that makes plain CSV lossless enough: NULL is nothing at
    // all, the empty string is a pair of quotes.
    assert_eq!(lines[1], "2,\"\"", "{csv}");
    assert_eq!(lines[2], "3,", "{csv}");
}

#[test]
fn a_malformed_specification_fails_the_start_rather_than_becoming_a_failed_job() {
    let session = session();
    exec(&session, "create table t (id integer)");
    exec(&session, "create table u (id integer)");
    let dir = tempfile::tempdir().expect("a temp directory");

    // Nothing to extract. Not short-circuited on the Rust side: the bridge is
    // the single authority on what a malformed request is.
    let error = session
        .start_job(
            &ExtractSpec::new(dir.path().join("nothing.sql"))
                .with_data(DataOptions::included(DataMode::Insert)),
        )
        .expect_err("an empty object list is not a job");
    let Error::Bridge(error) = error else {
        panic!("expected an error envelope, got {error:?}")
    };
    assert_eq!(error.kind, BridgeErrorKind::Protocol);
    assert!(!error.is_not_implemented(), "{error:?}");
    assert!(error.message.contains("objects"), "{}", error.message);

    // A predicate names columns, and columns belong to one table.
    let error = session
        .start_job(
            &ExtractSpec::new(dir.path().join("ambiguous.sql"))
                .with_objects([
                    ObjectRef::new("T").with_schema("PUBLIC"),
                    ObjectRef::new("U").with_schema("PUBLIC"),
                ])
                .with_data(DataOptions::included(DataMode::Insert).with_where("id > 1")),
        )
        .expect_err("one predicate cannot serve two tables");
    let Error::Bridge(error) = error else {
        panic!("expected an error envelope, got {error:?}")
    };
    assert_eq!(error.kind, BridgeErrorKind::Protocol);
    assert!(error.message.contains("where"), "{}", error.message);

    // A rejected specification leaves nothing behind: no file, no job, and a
    // session that is still good for the next attempt.
    assert!(!dir.path().join("nothing.sql").exists());
    assert!(session.ping().expect("still alive").ok);
}

#[test]
fn a_terminal_reading_retires_the_job_handle() {
    // The rule the whole poll loop is built on. Losing it means either a poller
    // that spins forever on a handle the bridge has forgotten, or a bridge that
    // keeps every job it ever ran.
    let session = session();
    exec(&session, "create table t (id integer)");

    let dir = tempfile::tempdir().expect("a temp directory");
    let mut job = session
        .start_job(
            &ExtractSpec::new(dir.path().join("once.sql"))
                .with_object(ObjectRef::new("T").with_schema("PUBLIC"))
                .with_ddl(DdlOptions::included()),
        )
        .expect("the specification is accepted");

    assert_eq!(drain(&mut job).state, JobState::Done);
    assert!(job.is_terminal(), "the crate knows the handle is spent");

    let error = job
        .poll()
        .expect_err("the handle died in the call that reported the end");
    let Error::Bridge(error) = error else {
        panic!("expected an error envelope, got {error:?}")
    };
    assert_eq!(error.kind, BridgeErrorKind::Protocol, "{error:?}");

    // A cancel after that is answered locally and does not go near the bridge:
    // a button clicked in the instant the job finished is a race, not a fault.
    assert!(
        !job.cancel().expect("answered without a round trip"),
        "there was nothing left to cancel"
    );
}

/// How many rows the cancellation tests give themselves to catch a job in.
const BIG_ROWS: u64 = 200_000;

/// Creates a table big enough that extracting it cannot finish instantly.
fn create_big_table(session: &Session) {
    exec(session, "create table big (id integer, v varchar(80))");
    exec(
        session,
        &format!("insert into big select x, 'row number ' || x from system_range(1, {BIG_ROWS})"),
    );
}

#[test]
fn cancelling_a_job_mid_flight_keeps_the_partial_file_and_the_session() {
    let session = session();
    create_big_table(&session);

    let dir = tempfile::tempdir().expect("a temp directory");
    let path = dir.path().join("cancelled.sql");
    let mut job = session
        .start_job(
            &ExtractSpec::new(&path)
                .with_object(ObjectRef::new("BIG").with_schema("PUBLIC"))
                .with_data(DataOptions::included(DataMode::Insert)),
        )
        .expect("the specification is accepted");

    // Wait for the job to be moving rows before cancelling it: a cancel that
    // arrived after the last row would prove nothing.
    let mut seen = 0;
    for _ in 0..2000 {
        let progress = job.poll().expect("the job answers a poll");
        assert_eq!(
            progress.state,
            JobState::Running,
            "the job outran the sampler: {progress:?}"
        );
        seen = progress.rows_done;
        if seen > 0 {
            assert_eq!(progress.phase, "data:PUBLIC.BIG");
            break;
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    assert!(seen > 0, "no rows were ever reported");

    assert!(
        job.cancel().expect("the cancel is answered"),
        "the job was still running when the cancel arrived"
    );

    let end = drain(&mut job);
    assert_eq!(end.state, JobState::Cancelled, "{end:?}");
    assert!(
        end.rows_done < BIG_ROWS,
        "a cancel that only landed after the last row proves nothing: {end:?}"
    );
    // Partial output is kept rather than deleted: it is work the user may still
    // want, and deleting it is a decision this layer does not get to make.
    let written = std::fs::metadata(&path)
        .expect("the partial file is there")
        .len();
    assert!(
        written > 0,
        "the file should hold what was written before the cancel"
    );

    // And the session survived having a statement cancelled underneath it.
    let batch = fetch_one(&session, "select count(*) from big", 1);
    assert_eq!(batch.value(0, 0), Some(Value::I64(BIG_ROWS as i64)));
}

#[test]
fn dropping_a_job_nobody_polled_stops_it_instead_of_leaving_it_writing() {
    let session = session();
    create_big_table(&session);

    let dir = tempfile::tempdir().expect("a temp directory");
    let path = dir.path().join("abandoned.sql");
    let handle = {
        let job = session
            .start_job(
                &ExtractSpec::new(&path)
                    .with_object(ObjectRef::new("BIG").with_schema("PUBLIC"))
                    .with_data(DataOptions::included(DataMode::Insert)),
            )
            .expect("the specification is accepted");
        job.handle()
        // Dropped here, without a single poll: a best-effort cancel and one
        // reading to retire the handle.
    };

    // Read through `call_raw`, because the typed path needs a live `Job` and
    // this test is about one that no longer exists. Two answers are correct:
    // the drop's own reading retired the handle, or the job had not stopped in
    // time and the next reading finds it cancelled. What must not happen is a
    // job that keeps writing a file nobody is waiting for.
    for _ in 0..200 {
        match session.call_raw(Op::JobPoll, handle, 0, None) {
            Err(Error::Bridge(error)) => {
                assert_eq!(error.kind, BridgeErrorKind::Protocol, "{error:?}");
                return;
            }
            Err(error) => panic!("expected an error envelope, got {error:?}"),
            Ok(payload) => {
                let progress: JobProgress =
                    serde_json::from_slice(&payload).expect("a progress object");
                assert_ne!(
                    progress.state,
                    JobState::Done,
                    "an abandoned job ran to completion: {progress:?}"
                );
                if progress.state == JobState::Cancelled {
                    assert!(progress.rows_done < BIG_ROWS, "{progress:?}");
                    return;
                }
                std::thread::sleep(Duration::from_millis(25));
            }
        }
    }
    panic!("the abandoned job was still running long after it was dropped");
}

// --- jobs: DB-to-DB transfers ----------------------------------------------

/// Opens a second session against a database of its own.
///
/// A transfer needs two, and they have to be two *sessions*: the bridge holds
/// both connection locks for the whole stream, so the target cannot be the
/// handle this test is also reading with.
fn other_session() -> Session {
    Session::open(jvm(), &spec(&fresh_url(), "sa", "")).expect("H2 accepts a second connection")
}

/// `id integer primary key, txt varchar(40)`, the fixture both ends of the
/// transfer tests share.
const PAIR_DDL: &str = "create table t (id integer not null primary key, txt varchar(40))";

/// Reads the whole of a small table as `(id, txt)` pairs, ordered.
fn rows_of(session: &Session, table: &str) -> Vec<(i64, Option<String>)> {
    let batch = fetch_one(
        session,
        &format!("select id, txt from {table} order by id"),
        100,
    );
    (0..batch.rows())
        .map(|row| {
            let id = match batch.value(row, 0) {
                Some(Value::I64(id)) => id,
                other => panic!("row {row} has a non-integer id: {other:?}"),
            };
            let txt = match batch.value(row, 1) {
                Some(Value::Str(text)) => Some(text.to_string()),
                Some(Value::Null) | None => None,
                other => panic!("row {row} has a non-text txt: {other:?}"),
            };
            (id, txt)
        })
        .collect()
}

#[test]
fn a_transfer_moves_rows_between_two_sessions_without_one_crossing_the_boundary() {
    let source = session();
    exec(&source, PAIR_DDL);
    exec(
        &source,
        "insert into t values (1, 'one'), (2, 'it''s two'), (3, null)",
    );

    let target = other_session();
    exec(&target, PAIR_DDL);

    let mut job = source
        .start_transfer(&TransferSpec::new(
            "select id, txt from t order by id",
            target.handle(),
            ObjectRef::new("T").with_schema("PUBLIC"),
        ))
        .expect("the specification is accepted");

    let end = drain(&mut job);
    assert_eq!(end.state, JobState::Done, "{end:?}");
    assert_eq!(end.rows_done, 3, "{end:?}");
    assert_eq!(end.rows_skipped, 0, "nothing was refused: {end:?}");
    assert_eq!(end.bytes, 0, "a transfer writes no file: {end:?}");
    assert_eq!(end.phase, "done");
    assert_eq!(end.rows_total, None, "no COUNT(*) is run up front: {end:?}");
    assert!(end.errors.is_empty(), "{:?}", end.errors);

    // The rows are in the other database, and this test never saw one of them.
    assert_eq!(
        rows_of(&target, "t"),
        vec![
            (1, Some("one".to_string())),
            (2, Some("it's two".to_string())),
            (3, None),
        ]
    );
}

#[test]
fn truncate_insert_empties_the_target_first() {
    let source = session();
    exec(&source, PAIR_DDL);
    exec(
        &source,
        "insert into t values (1, 'fresh'), (2, 'also fresh')",
    );

    let target = other_session();
    exec(&target, PAIR_DDL);
    exec(
        &target,
        "insert into t values (7, 'stale'), (8, 'stale too')",
    );

    let mut job = source
        .start_transfer(
            &TransferSpec::new(
                "select id, txt from t",
                target.handle(),
                ObjectRef::new("T").with_schema("PUBLIC"),
            )
            .with_mode(TransferMode::TruncateInsert),
        )
        .expect("the specification is accepted");

    let end = drain(&mut job);
    assert_eq!(end.state, JobState::Done, "{end:?}");
    assert_eq!(end.rows_done, 2, "{end:?}");

    // The emptying is a DELETE, so it happened in the transfer's own
    // transaction — but the transaction committed, so nothing of the old rows
    // is left.
    assert_eq!(
        rows_of(&target, "t"),
        vec![
            (1, Some("fresh".to_string())),
            (2, Some("also fresh".to_string())),
        ]
    );
}

#[test]
fn an_upsert_updates_what_is_there_and_inserts_what_is_not() {
    let source = session();
    exec(&source, PAIR_DDL);
    exec(
        &source,
        "insert into t values (1, 'updated'), (2, 'inserted')",
    );

    let target = other_session();
    exec(&target, PAIR_DDL);
    exec(&target, "insert into t values (1, 'old'), (9, 'untouched')");

    let mut job = source
        .start_transfer(
            &TransferSpec::new(
                "select id, txt from t",
                target.handle(),
                ObjectRef::new("T").with_schema("PUBLIC"),
            )
            .with_mode(TransferMode::Upsert),
        )
        .expect("H2 has a MERGE and the target has a primary key");

    let end = drain(&mut job);
    assert_eq!(end.state, JobState::Done, "{end:?}");
    assert_eq!(end.rows_done, 2, "{end:?}");
    assert_eq!(end.rows_skipped, 0, "an upsert refuses nothing: {end:?}");

    // The conflict key came from the target's primary key metadata: row 1 was
    // updated rather than duplicated or refused, row 2 inserted, row 9 left
    // alone because the source said nothing about it.
    assert_eq!(
        rows_of(&target, "t"),
        vec![
            (1, Some("updated".to_string())),
            (2, Some("inserted".to_string())),
            (9, Some("untouched".to_string())),
        ]
    );
}

#[test]
fn on_error_skip_drops_the_bad_rows_and_counts_them() {
    let source = session();
    exec(&source, PAIR_DDL);
    exec(
        &source,
        "insert into t values (1, 'clash'), (2, 'fine'), (3, 'clash too'), (4, 'fine too')",
    );

    let target = other_session();
    exec(&target, PAIR_DDL);
    exec(
        &target,
        "insert into t values (1, 'sitting'), (3, 'sitting')",
    );

    // One row per batch, so a refused row is a refused row rather than a batch
    // the driver failed as a unit: which rows of a failed batch went in is a
    // driver-by-driver answer, and `skip` is only meaningful when it is not.
    let mut job = source
        .start_transfer(
            &TransferSpec::new(
                "select id, txt from t order by id",
                target.handle(),
                ObjectRef::new("T").with_schema("PUBLIC"),
            )
            .with_batch_size(1)
            .with_on_error(OnError::Skip),
        )
        .expect("the specification is accepted");

    let end = drain(&mut job);
    assert_eq!(
        end.state,
        JobState::Done,
        "`skip` means the job survives its bad rows: {end:?}"
    );
    assert_eq!(end.rows_done, 2, "the two rows that fitted: {end:?}");
    assert_eq!(end.rows_skipped, 2, "the two that clashed: {end:?}");

    assert_eq!(
        rows_of(&target, "t"),
        vec![
            (1, Some("sitting".to_string())),
            (2, Some("fine".to_string())),
            (3, Some("sitting".to_string())),
            (4, Some("fine too".to_string())),
        ],
        "the rows that could go in went in, and the ones already there stayed"
    );
}

#[test]
fn a_transfer_that_cannot_work_is_refused_at_the_start_rather_than_run() {
    let source = session();
    exec(&source, PAIR_DDL);
    exec(&source, "insert into t values (1, 'a')");

    // A handle the bridge has never issued. Not short-circuited in Rust: the
    // bridge is the single authority on what a malformed request is.
    let error = source
        .start_transfer(&TransferSpec::new(
            "select id, txt from t",
            i64::MAX,
            ObjectRef::new("T").with_schema("PUBLIC"),
        ))
        .expect_err("there is no such target session");
    let Error::Bridge(error) = error else {
        panic!("expected an error envelope, got {error:?}")
    };
    assert_eq!(error.kind, BridgeErrorKind::Protocol, "{error:?}");
    assert!(!error.is_not_implemented(), "{error:?}");

    // An upsert needs a conflict key, and it reads one from the target's
    // primary key metadata. Without one there is no correct statement to write,
    // so the start fails instead of the job.
    let target = other_session();
    exec(&target, "create table t (id integer, txt varchar(40))");
    let error = source
        .start_transfer(
            &TransferSpec::new(
                "select id, txt from t",
                target.handle(),
                ObjectRef::new("T").with_schema("PUBLIC"),
            )
            .with_mode(TransferMode::Upsert),
        )
        .expect_err("a keyless target cannot be upserted into");
    let Error::Bridge(error) = error else {
        panic!("expected an error envelope, got {error:?}")
    };
    assert_eq!(error.kind, BridgeErrorKind::Protocol, "{error:?}");

    // A refused start moved nothing and left both sessions good for the next
    // attempt.
    assert!(rows_of(&target, "t").is_empty(), "no row was written");
    assert!(source.ping().expect("still alive").ok);
    assert!(target.ping().expect("still alive").ok);
}

#[test]
fn cancelling_a_transfer_stops_it_and_leaves_both_sessions_usable() {
    let source = session();
    create_big_table(&source);

    let target = other_session();
    exec(&target, "create table big (id integer, v varchar(80))");

    let mut job = source
        .start_transfer(&TransferSpec::new(
            "select id, v from big",
            target.handle(),
            ObjectRef::new("BIG").with_schema("PUBLIC"),
        ))
        .expect("the specification is accepted");

    // Catch it moving rows: a cancel that arrived after the last one would
    // prove nothing.
    let mut seen = 0;
    for _ in 0..2000 {
        let progress = job.poll().expect("the job answers a poll");
        assert_eq!(
            progress.state,
            JobState::Running,
            "the job outran the sampler: {progress:?}"
        );
        seen = progress.rows_done;
        if seen > 0 {
            break;
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    assert!(seen > 0, "no rows were ever reported");

    assert!(
        job.cancel().expect("the cancel is answered"),
        "the job was still running when the cancel arrived"
    );

    let end = drain(&mut job);
    assert_eq!(end.state, JobState::Cancelled, "{end:?}");
    assert!(
        end.rows_done < BIG_ROWS,
        "a cancel that only landed after the last row proves nothing: {end:?}"
    );

    // Both sessions survived: the source had a SELECT cancelled underneath it
    // and the target a batch, and the job let go of both connection locks.
    let batch = fetch_one(&source, "select count(*) from big", 1);
    assert_eq!(batch.value(0, 0), Some(Value::I64(BIG_ROWS as i64)));
    let batch = fetch_one(&target, "select count(*) from big", 1);
    let landed = match batch.value(0, 0) {
        Some(Value::I64(count)) => count,
        other => panic!("expected a count, got {other:?}"),
    };
    // Whatever had been committed stays — that is the documented outcome, not
    // an accident — and the uncommitted tail was rolled back.
    assert!(
        landed <= BIG_ROWS as i64,
        "the target holds more rows than the source had: {landed}"
    );
}

#[test]
fn a_terminal_reading_retires_a_transfer_handle_too() {
    let source = session();
    exec(&source, PAIR_DDL);
    exec(&source, "insert into t values (1, 'only')");

    let target = other_session();
    exec(&target, PAIR_DDL);

    let mut job = source
        .start_transfer(&TransferSpec::new(
            "select id, txt from t",
            target.handle(),
            ObjectRef::new("T").with_schema("PUBLIC"),
        ))
        .expect("the specification is accepted");

    assert_eq!(drain(&mut job).state, JobState::Done);
    assert!(job.is_terminal(), "the crate knows the handle is spent");

    let error = job
        .poll()
        .expect_err("the handle died in the call that reported the end");
    let Error::Bridge(error) = error else {
        panic!("expected an error envelope, got {error:?}")
    };
    assert_eq!(error.kind, BridgeErrorKind::Protocol, "{error:?}");
    assert!(
        !job.cancel().expect("answered without a round trip"),
        "there was nothing left to cancel"
    );
}

// --- jobs: backups ----------------------------------------------------------

/// Creates two tables joined by a foreign key, and fills them.
///
/// The pair is the point: a backup writes every `CREATE` first and every
/// foreign key afterwards as an `ALTER`, and only a script built that way
/// replays into an empty database whatever order the tables come out in.
fn create_related_tables(session: &Session) {
    exec(
        session,
        "create table parent (id integer not null primary key, name varchar(20))",
    );
    exec(
        session,
        "create table child (id integer not null primary key,
             parent_id integer not null,
             constraint fk_child_parent foreign key (parent_id) references parent(id))",
    );
    exec(session, "insert into parent values (1, 'a'), (2, 'b')");
    exec(
        session,
        "insert into child values (10, 1), (11, 2), (12, 1)",
    );
}

/// Replays a script statement by statement into a database that has never seen
/// it.
///
/// Statements end at a semicolon that ends a line — enough for what the bridge
/// writes, and deliberately not a SQL parser. `rudbman-sql` is where a real
/// splitter lives.
fn replay(session: &Session, script: &str) {
    let mut statement = String::new();
    for line in script.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with("--") {
            continue;
        }
        statement.push_str(line);
        statement.push('\n');
        if trimmed.ends_with(';') {
            exec(session, statement.trim().trim_end_matches(';'));
            statement.clear();
        }
    }
    assert!(
        statement.trim().is_empty(),
        "the script ended mid-statement: {statement}"
    );
}

#[test]
fn a_backup_of_a_schema_replays_into_an_empty_database() {
    let source = session();
    create_related_tables(&source);

    let dir = tempfile::tempdir().expect("a temp directory");
    let path = dir.path().join("schema.sql");
    let mut job = source
        .start_backup(
            &BackupSpec::new(&path)
                .with_schema("PUBLIC")
                .with_ddl(DdlOptions::included())
                .with_data(BackupDataOptions::included()),
        )
        .expect("the specification is accepted");

    let end = drain(&mut job);
    assert_eq!(end.state, JobState::Done, "{end:?}");
    // No object list was given: the bridge enumerated the schema's tables and
    // wrote every row of both.
    assert_eq!(end.rows_done, 5, "two parents and three children: {end:?}");
    assert_eq!(end.rows_skipped, 0, "a backup refuses nothing: {end:?}");
    assert!(end.errors.is_empty(), "{:?}", end.errors);

    let script = std::fs::read_to_string(&path).expect("the file is where it was asked for");
    assert_eq!(
        end.bytes as usize,
        script.len(),
        "the byte count is exact once the job has ended"
    );

    let elsewhere = other_session();
    replay(&elsewhere, &script);

    let parents = fetch_one(&elsewhere, "select id, name from parent order by id", 10);
    assert_eq!(parents.rows(), 2);
    assert_eq!(parents.value(0, 0), Some(Value::I64(1)));
    assert_eq!(parents.value(0, 1), Some(Value::Str("a")));
    assert_eq!(parents.value(1, 1), Some(Value::Str("b")));
    let batch = fetch_one(&elsewhere, "select count(*) from child", 1);
    assert_eq!(batch.value(0, 0), Some(Value::I64(3)));

    // The foreign key came across as well, which is what makes this a backup
    // rather than a heap of rows.
    elsewhere
        .execute(&StatementSpec::new("insert into child values (13, 999)"))
        .expect_err("the replayed foreign key is a real constraint");
}

#[test]
fn a_gzip_backup_writes_a_gzip_file_and_counts_the_compressed_bytes() {
    let source = session();
    create_related_tables(&source);

    let dir = tempfile::tempdir().expect("a temp directory");
    let path = dir.path().join("schema.sql.gz");
    let mut job = source
        .start_backup(
            &BackupSpec::new(&path)
                .with_schema("PUBLIC")
                .with_compress(Compression::Gzip)
                .with_ddl(DdlOptions::included())
                .with_data(BackupDataOptions::included()),
        )
        .expect("the specification is accepted");

    let end = drain(&mut job);
    assert_eq!(end.state, JobState::Done, "{end:?}");
    assert_eq!(end.rows_done, 5, "{end:?}");

    let bytes = std::fs::read(&path).expect("the file is there");
    assert_eq!(
        &bytes[..2],
        &[0x1f, 0x8b],
        "a gzip member starts with its magic number, or nothing will unpack it"
    );
    // The count is what was written to the file, after compression — otherwise
    // a progress bar for a compressed backup would be measuring the wrong
    // thing and would never agree with the file on disc.
    assert_eq!(
        end.bytes,
        std::fs::metadata(&path).expect("the file is there").len(),
        "the byte count is the compressed size"
    );
    assert!(
        bytes.len() < 4096,
        "a handful of rows should not compress to {} bytes",
        bytes.len()
    );
}

// --- driver probing, before any session exists -----------------------------

/// Writes a throwaway file under the temp directory and hands back its path.
///
/// Named after the test that made it so a failure says which one left it
/// behind, and removed by [`TempFile`]'s `Drop`.
struct TempFile(PathBuf);

impl TempFile {
    fn new(name: &str, contents: &[u8]) -> TempFile {
        let path = std::env::temp_dir().join(format!("rudbman-probe-{name}"));
        std::fs::write(&path, contents).expect("the temp directory is writable");
        TempFile(path)
    }

    fn path(&self) -> &PathBuf {
        &self.0
    }
}

impl Drop for TempFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

/// The 22 bytes of an archive with no entries: end-of-central-directory and
/// nothing else. A valid JAR that contains no driver — which is what a sources
/// or javadoc archive looks like to this operation.
const EMPTY_ZIP: [u8; 22] = [
    0x50, 0x4b, 0x05, 0x06, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
];

#[test]
fn probing_a_driver_jar_names_its_driver_class() {
    // No session anywhere in this test: probing is what happens *before* one
    // exists, which is the whole reason it lives on the JVM.
    let probe = jvm()
        .probe_drivers(&[h2_jar()])
        .expect("the H2 jar is readable");

    assert!(
        probe.classes.iter().any(|class| class == "org.h2.Driver"),
        "the scan should have found the driver: {:?}",
        probe.classes
    );
    assert!(
        probe.services.iter().any(|class| class == "org.h2.Driver"),
        "H2 declares itself in META-INF/services: {:?}",
        probe.services
    );
    assert_eq!(
        probe.recommended(),
        Some("org.h2.Driver"),
        "the declared service is the one to offer first"
    );
    assert!(!probe.is_empty());

    // The end-to-end shape of driver registration: what was probed can then be
    // connected with.
    let spec = ConnectionSpec::new(fresh_url(), probe.recommended().expect("a driver"))
        .with_credentials("sa", "")
        .with_jars([h2_jar()]);
    let session = Session::open(jvm(), &spec).expect("the probed class connects");
    assert!(session.ping().expect("pings").ok);
}

#[test]
fn probing_an_archive_with_no_driver_is_an_empty_answer_not_an_error() {
    let jar = TempFile::new("empty.jar", &EMPTY_ZIP);
    let probe = jvm()
        .probe_drivers(&[jar.path().clone()])
        .expect("an archive without a driver is not a failure");
    assert!(probe.is_empty(), "expected nothing to be found: {probe:?}");
    assert_eq!(probe.recommended(), None);
}

#[test]
fn probing_a_file_that_is_not_an_archive_finds_nothing_and_says_so_quietly() {
    // Documented in `Jvm::probe_drivers`: the entry stream simply yields no
    // entries, so there is nothing to report and nothing to fail about.
    let jar = TempFile::new("garbage.jar", b"this is not a zip archive at all");
    let probe = jvm()
        .probe_drivers(&[jar.path().clone()])
        .expect("a file that is not an archive is not an error");
    assert!(probe.is_empty(), "{probe:?}");
}

#[test]
fn probing_a_damaged_archive_is_an_io_error() {
    // Half of a real jar: enough of a zip header to start reading entries, not
    // enough to finish one. Unlike the garbage file above, this one fails part
    // way through, and the two cases are worth telling apart in the UI —
    // "nothing in it" is a wrong file, "cannot read it" is a broken download.
    let whole = std::fs::read(h2_jar()).expect("the H2 jar is readable");
    let jar = TempFile::new("truncated.jar", &whole[..whole.len() / 2]);

    let error = jvm()
        .probe_drivers(&[jar.path().clone()])
        .expect_err("a half-written jar cannot be scanned");
    let Error::Bridge(error) = error else {
        panic!("expected an error envelope, got {error:?}")
    };
    assert_eq!(error.kind, BridgeErrorKind::Io, "{error:?}");
}

#[test]
fn probing_a_path_that_does_not_exist_is_a_driver_error() {
    let missing = PathBuf::from("/nonexistent/rudbman/nope.jar");
    let error = jvm()
        .probe_drivers(&[missing])
        .expect_err("there is no such file");
    let Error::Bridge(error) = error else {
        panic!("expected an error envelope, got {error:?}")
    };
    assert_eq!(error.kind, BridgeErrorKind::Driver);
    assert!(
        error.message.contains("nope.jar"),
        "the message must name the file: {}",
        error.message
    );
}

#[test]
fn probing_nothing_at_all_is_a_protocol_error() {
    // Not short-circuited on the Rust side: the bridge is the single authority
    // on what a malformed request is.
    let error = jvm().probe_drivers(&[]).expect_err("no jars, no answer");
    let Error::Bridge(error) = error else {
        panic!("expected an error envelope, got {error:?}")
    };
    assert_eq!(error.kind, BridgeErrorKind::Protocol);
    assert!(!error.is_not_implemented(), "{error:?}");
}

// --- corruption, on bytes Java really wrote --------------------------------

#[test]
fn a_damaged_batch_is_an_error_and_never_a_panic() {
    let session = session();
    exec(&session, "create table t (id integer, txt varchar(20))");
    exec(&session, "insert into t values (1, 'one'), (2, 'two')");

    let cursor = session
        .execute(&StatementSpec::new("select id, txt from t order by id"))
        .expect("selects");
    // The raw batch, exactly as the bridge encoded it.
    let bytes = session
        .call_raw(Op::Fetch, cursor.handle(), 10, None)
        .expect("fetches");
    assert!(Batch::decode(&bytes).is_ok(), "the intact batch decodes");

    // Every truncation of it.
    for cut in 0..bytes.len() {
        assert!(
            Batch::decode(&bytes[..cut]).is_err(),
            "a batch cut to {cut} bytes decoded"
        );
    }

    // An offset pushed past the end of the value area: the STR column's first
    // offset lives right after the header, its kind byte, its payload length
    // and its one-byte bitmap.
    let mut damaged = bytes.clone();
    let string_column_at = 13 + 5 + 1 + 8 * 2 + 5 + 1;
    damaged[string_column_at..string_column_at + 4].copy_from_slice(&u32::MAX.to_le_bytes());
    assert!(
        Batch::decode(&damaged).is_err(),
        "an offset outside the buffer has to be refused, not indexed with"
    );
}

// --- editing: planned DML applied in one transaction -----------------------
//
// The data pane stages a user's edits, `rudbman-sql` turns them into statements
// with bind parameters, and this crate runs them. Each half has tests of its
// own and neither can prove the seam: the planner never sees a driver, and this
// crate never sees an edit. What follows drives the whole way through against
// real H2 — including the two failures the design turns on, a statement the
// server refuses and a row that moved underneath the batch.
//
// The apply loop below is the application's, reproduced rather than called: it
// lives in `rudbman-app`, which depends on this crate and cannot be depended on
// back. What is under test here is therefore the wire mechanics — that
// `update_count` really comes back as the row count, that a rollback really
// undoes the statements before the failure, that auto-commit really returns —
// and the *decision* those mechanics feed is tested where it lives, in
// `rudbman-app`'s own suite.

/// The bind parameter one planned value becomes.
///
/// The application's own mapping, in miniature. None of these fixtures has a
/// binary column, so `Bytes` shares the text arm rather than carrying a hex
/// decoder nothing here would exercise.
fn bind(value: &DmlValue) -> Param {
    let Some(text) = value.text() else {
        return Param::Null;
    };
    match value.kind() {
        DmlKind::I64 => Param::I64(text.parse().expect("the planner was given an integer")),
        DmlKind::Decimal => Param::Decimal(text.to_string()),
        DmlKind::Bool => Param::Bool(text == "true"),
        DmlKind::Date => Param::Date(text.to_string()),
        DmlKind::Time => Param::Time(text.to_string()),
        DmlKind::Timestamp => Param::Timestamp(text.to_string()),
        DmlKind::Str | DmlKind::Bytes => Param::Str(text.to_string()),
    }
}

/// Runs a planned batch the way the data pane does, and says why if it stopped.
///
/// `checked` is how many leading statements have to report exactly one changed
/// row — the deletes and the updates, which `plan_edits` puts first. The
/// ordering rule in [`unwind`] is the point of the whole function.
fn apply(session: &Session, batch: &[DmlStatement], checked: usize) -> Result<(), String> {
    session.set_auto_commit(false).expect("auto-commit off");

    for (index, statement) in batch.iter().enumerate() {
        let spec = StatementSpec::new(statement.sql.clone())
            .with_params(statement.values.iter().map(bind));
        let stopped = match session.execute(&spec) {
            Err(error) => Some(error.to_string()),
            Ok(cursor) if index < checked && cursor.result().update_count != 1 => Some(format!(
                "statement {index} reached {} rows",
                cursor.result().update_count
            )),
            Ok(_) => None,
        };
        if let Some(why) = stopped {
            return Err(unwind(session, why));
        }
    }

    if let Err(error) = session.commit() {
        return Err(unwind(session, error.to_string()));
    }
    session.set_auto_commit(true).expect("auto-commit back on");
    Ok(())
}

/// Rolls back, **then** restores auto-commit, and hands the reason back.
///
/// The order is the whole reason this is a function. Several products treat
/// `setAutoCommit(true)` as an implicit commit, so putting it back first is
/// exactly what would commit the half-applied batch — the trap the bridge's
/// `TransferJob` documents on its own path.
fn unwind(session: &Session, why: String) -> String {
    session.rollback().expect("the rollback goes through");
    session.set_auto_commit(true).expect("auto-commit back on");
    why
}

/// Every column of `sql`'s rows, as text, one joined string per row.
fn read_back(session: &Session, sql: &str) -> Vec<String> {
    let batch = fetch_one(session, sql, 100);
    (0..batch.rows())
        .map(|row| {
            (0..batch.column_count())
                .map(|column| match batch.value(row, column) {
                    None | Some(Value::Null) => "NULL".to_string(),
                    Some(Value::I64(value)) => value.to_string(),
                    Some(Value::Str(text)) => text.to_string(),
                    Some(other) => format!("{other:?}"),
                })
                .collect::<Vec<_>>()
                .join("|")
        })
        .collect()
}

#[test]
fn a_planned_batch_of_edits_applies_in_one_transaction() {
    let session = session();
    exec(
        &session,
        "create table t (id integer auto_increment primary key,
                         name varchar(20),
                         qty integer)",
    );
    exec(
        &session,
        "insert into t (name, qty) values ('a', 1), ('b', 2), ('c', 3)",
    );

    // One of each: the row `b` goes, `c`'s quantity changes, and a row is added
    // whose key the server generates — which is what `InsertCell::Unset` on the
    // auto-increment column means.
    let mut edits = TableEdits::new(["T"], ["ID", "NAME", "QTY"]).with_key([0]);
    edits.deletes.push(vec![DmlValue::new(DmlKind::I64, "2")]);
    edits.updates.push(RowUpdate {
        key: vec![DmlValue::new(DmlKind::I64, "3")],
        set: vec![(2, DmlValue::new(DmlKind::I64, "30"))],
    });
    edits.inserts.push(vec![
        InsertCell::Unset,
        InsertCell::Set(DmlValue::new(DmlKind::Str, "d")),
        InsertCell::Set(DmlValue::new(DmlKind::I64, "4")),
    ]);

    let batch = plan_edits(&edits, &Dialect::H2).expect("the edits plan");
    assert_eq!(
        batch.iter().map(|s| s.sql.as_str()).collect::<Vec<_>>(),
        [
            "DELETE FROM T WHERE ID = ?",
            "UPDATE T SET QTY = ? WHERE ID = ?",
            "INSERT INTO T (NAME, QTY) VALUES (?, ?)",
        ],
        "no value was spliced into the SQL"
    );

    apply(&session, &batch, 2).expect("the batch applies");

    // The generated key is 4, and nothing on the planning side could have known
    // that: it is exactly why an apply is followed by a full reload.
    assert_eq!(
        read_back(&session, "select id, name, qty from t order by id"),
        ["1|a|1", "3|c|30", "4|d|4"]
    );
}

#[test]
fn a_batch_that_fails_part_way_leaves_the_table_as_it_was() {
    let url = fresh_url();
    let session = Session::open(jvm(), &spec(&url, "sa", "")).expect("connects");
    exec(
        &session,
        "create table t (id integer primary key, name varchar(20) not null)",
    );
    exec(&session, "insert into t values (1, 'a'), (2, 'b')");

    // The first update is fine; the second sets a NOT NULL column to NULL.
    let mut edits = TableEdits::new(["T"], ["ID", "NAME"]).with_key([0]);
    for (id, name) in [
        (1, DmlValue::new(DmlKind::Str, "A")),
        (2, DmlValue::null(DmlKind::Str)),
    ] {
        edits.updates.push(RowUpdate {
            key: vec![DmlValue::new(DmlKind::I64, id.to_string())],
            set: vec![(1, name)],
        });
    }
    let batch = plan_edits(&edits, &Dialect::H2).expect("the edits plan");

    let why = apply(&session, &batch, batch.len()).expect_err("the second update is refused");
    assert!(
        why.to_lowercase().contains("null"),
        "the driver's own words are what surfaces: {why}"
    );
    assert_eq!(
        read_back(&session, "select id, name from t order by id"),
        ["1|a", "2|b"],
        "the update that did go through was not rolled back"
    );

    // And auto-commit really is back on, which is the half of the unwind a
    // reading of this connection alone cannot prove: a second connection sees
    // the row only if nothing is holding it in an open transaction.
    exec(&session, "insert into t values (3, 'c')");
    let observer = Session::open(jvm(), &spec(&url, "sa", "")).expect("connects");
    assert_eq!(
        read_back(&observer, "select id, name from t order by id"),
        ["1|a", "2|b", "3|c"],
        "the insert after the failed batch was still inside a transaction"
    );
}

#[test]
fn an_update_that_reaches_no_row_is_reported_as_a_row_count_of_zero() {
    let session = session();
    exec(
        &session,
        "create table t (id integer primary key, name varchar(20))",
    );
    exec(&session, "insert into t values (1, 'a'), (2, 'b')");

    let mut edits = TableEdits::new(["T"], ["ID", "NAME"]).with_key([0]);
    for (id, name) in [(1, "A"), (2, "B")] {
        edits.updates.push(RowUpdate {
            key: vec![DmlValue::new(DmlKind::I64, id.to_string())],
            set: vec![(1, DmlValue::new(DmlKind::Str, name))],
        });
    }
    let batch = plan_edits(&edits, &Dialect::H2).expect("the edits plan");

    // Behind the batch's back, after the rows were read and before they are
    // written. This is the whole reason a `WHERE` clause naming the key alone is
    // safe: the row count is what notices.
    exec(&session, "delete from t where id = 2");

    let why = apply(&session, &batch, batch.len()).expect_err("the second update reaches nothing");
    assert_eq!(why, "statement 1 reached 0 rows");
    assert_eq!(
        read_back(&session, "select id, name from t order by id"),
        ["1|a"],
        "the first update stood after a row-count abort"
    );
}
