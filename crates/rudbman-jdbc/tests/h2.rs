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
    Batch, BridgeErrorKind, ColumnKind, ConnectionSpec, DdlSource, DescribeRequest, Error, Jvm,
    JvmConfig, Op, Param, Session, StatementSpec, Value, default_bridge_jar,
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
