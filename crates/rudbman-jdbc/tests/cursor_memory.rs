//! Opt-in probe: does an open streaming cursor on the shared session blow up
//! the bridge heap once anything else runs on that session?
//!
//! This replays the pre-paging read path (`StatementSpec::with_fetch_size`,
//! one batch read, then a metadata lookup and another query on the same
//! session, then the next batch, then close) against a table several times the
//! JVM heap, and prints what each step cost. Every test is `#[ignore]`d — even
//! H2 and SQLite, which need no server, fill a table of 300 MB or more. Run one
//! product per process — an `OutOfMemoryError` leaves the process-wide JVM in
//! no state to go on:
//!
//! ```text
//! RUDBMAN_TEST_PG_URL=... cargo test -p rudbman-jdbc --test cursor_memory -- --ignored --exact postgres --nocapture
//! ```
//!
//! `RUDBMAN_PROBE_ROWS` (300000), `RUDBMAN_PROBE_HEAP_MB` (128),
//! `RUDBMAN_PROBE_FETCH` (1000), `RUDBMAN_PROBE_AUTOCOMMIT` (1),
//! `RUDBMAN_PROBE_OTHERS` (1; 0 skips the interleaved commands),
//! `RUDBMAN_PROBE_MODE` (`shared`, or `dedicated` for a reader session of the
//! cursor's own), `RUDBMAN_PROBE_CLOSE` (`close`, or `cancel` first),
//! `RUDBMAN_PROBE_SERVER_CURSOR` (0; 1 sets `StatementSpec::server_cursor`) and
//! `RUDBMAN_PROBE_PROPS` (`k=v;k=v`, extra driver properties) vary the run.

use std::path::PathBuf;
use std::process::Command;
use std::time::Instant;

use rudbman_jdbc::{
    ConnectionSpec, DescribeRequest, Jvm, JvmConfig, Session, StatementSpec, Value,
    default_bridge_jar,
};

const TABLE: &str = "RB_PROBE_BIG";

fn env_u32(name: &str, default: u32) -> u32 {
    std::env::var(name)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

fn jvm() -> &'static Jvm {
    Jvm::start(
        &JvmConfig::new(default_bridge_jar())
            .with_heap_mb(env_u32("RUDBMAN_PROBE_HEAP_MB", 128))
            // An embedded JVM never sees the SIGQUIT that would start the
            // attach listener, and `jcmd` is how the heap gets read.
            .with_extra_args(["-XX:+StartAttachListener".to_string()]),
    )
    .expect("the JVM must start; build the bridge with `cd bridge && ./gradlew jar`")
}

fn find_in_gradle_cache(group: &str, artifact: &str) -> PathBuf {
    let home = std::env::var_os("GRADLE_USER_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(std::env::var_os("HOME").unwrap()).join(".gradle"));
    let root = home
        .join("caches/modules-2/files-2.1")
        .join(group)
        .join(artifact);
    let mut newest: Option<(String, PathBuf)> = None;
    for version in std::fs::read_dir(&root).into_iter().flatten().flatten() {
        let number = version.file_name().to_string_lossy().into_owned();
        let wanted = format!("{artifact}-{number}.jar");
        for hash in std::fs::read_dir(version.path())
            .into_iter()
            .flatten()
            .flatten()
        {
            for file in std::fs::read_dir(hash.path())
                .into_iter()
                .flatten()
                .flatten()
            {
                if file.file_name().to_string_lossy() == wanted
                    && newest.as_ref().is_none_or(|(best, _)| best < &number)
                {
                    newest = Some((number.clone(), file.path()));
                }
            }
        }
    }
    newest
        .unwrap_or_else(|| {
            panic!(
                "{group}:{artifact} not in the Gradle cache; run `cd bridge && ./gradlew drivers`"
            )
        })
        .1
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Product {
    Postgres,
    MySql,
    MariaDb,
    MsSql,
    Oracle,
    H2,
    Sqlite,
}

impl Product {
    fn url(self) -> Option<String> {
        let var = match self {
            Product::Postgres => "RUDBMAN_TEST_PG_URL",
            Product::MySql => "RUDBMAN_TEST_MYSQL_URL",
            Product::MariaDb => "RUDBMAN_TEST_MARIADB_URL",
            Product::MsSql => "RUDBMAN_TEST_MSSQL_URL",
            Product::Oracle => "RUDBMAN_TEST_ORACLE_URL",
            // Embedded: a file under target/, so the probe needs no server.
            Product::H2 | Product::Sqlite => {
                let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR"));
                return Some(match self {
                    Product::H2 => format!("jdbc:h2:file:{}/probe_h2", dir.display()),
                    _ => format!("jdbc:sqlite:{}/probe.sqlite", dir.display()),
                });
            }
        };
        std::env::var(var).ok()
    }

    fn driver(self) -> (&'static str, PathBuf) {
        match self {
            Product::Postgres => (
                "org.postgresql.Driver",
                find_in_gradle_cache("org.postgresql", "postgresql"),
            ),
            Product::MySql => (
                "com.mysql.cj.jdbc.Driver",
                find_in_gradle_cache("com.mysql", "mysql-connector-j"),
            ),
            Product::MariaDb => (
                "org.mariadb.jdbc.Driver",
                find_in_gradle_cache("org.mariadb.jdbc", "mariadb-java-client"),
            ),
            Product::MsSql => (
                "com.microsoft.sqlserver.jdbc.SQLServerDriver",
                find_in_gradle_cache("com.microsoft.sqlserver", "mssql-jdbc"),
            ),
            Product::Oracle => (
                "oracle.jdbc.OracleDriver",
                find_in_gradle_cache("com.oracle.database.jdbc", "ojdbc11"),
            ),
            Product::H2 => (
                "org.h2.Driver",
                find_in_gradle_cache("com.h2database", "h2"),
            ),
            Product::Sqlite => (
                "org.sqlite.JDBC",
                find_in_gradle_cache("org.xerial", "sqlite-jdbc"),
            ),
        }
    }

    fn credentials(self) -> (&'static str, &'static str) {
        match self {
            Product::MsSql => ("sa", "Rudbman!Passw0rd"),
            Product::H2 => ("sa", ""),
            _ => ("rudbman", "rudbman"),
        }
    }

    /// Statements that create and fill the probe table with `rows` rows of a
    /// 1000-character payload, server side.
    fn fill(self, rows: u32) -> Vec<String> {
        let create = match self {
            Product::Oracle => {
                format!("CREATE TABLE {TABLE} (ID NUMBER(10) PRIMARY KEY, PAD VARCHAR2(1000))")
            }
            _ => format!("CREATE TABLE {TABLE} (ID INT PRIMARY KEY, PAD VARCHAR(1000))"),
        };
        let digits = "(SELECT 0 d UNION ALL SELECT 1 UNION ALL SELECT 2 UNION ALL SELECT 3 \
                      UNION ALL SELECT 4 UNION ALL SELECT 5 UNION ALL SELECT 6 UNION ALL SELECT 7 \
                      UNION ALL SELECT 8 UNION ALL SELECT 9)";
        let insert = match self {
            Product::Postgres => format!(
                "INSERT INTO {TABLE} SELECT g, rpad(g::text, 1000, 'x') FROM generate_series(1, {rows}) g"
            ),
            Product::MySql | Product::MariaDb => format!(
                "INSERT INTO {TABLE} SELECT n, RPAD(n, 1000, 'x') FROM (SELECT \
                 a.d + b.d*10 + c.d*100 + e.d*1000 + f.d*10000 + g.d*100000 + 1 n \
                 FROM {digits} a, {digits} b, {digits} c, {digits} e, {digits} f, {digits} g) s \
                 WHERE n <= {rows}"
            ),
            Product::MsSql => format!(
                "INSERT INTO {TABLE} SELECT TOP ({rows}) \
                 ROW_NUMBER() OVER (ORDER BY (SELECT NULL)), REPLICATE('x', 1000) \
                 FROM sys.all_objects a CROSS JOIN sys.all_objects b CROSS JOIN sys.all_objects c"
            ),
            Product::Oracle => format!(
                "INSERT INTO {TABLE} SELECT LEVEL, RPAD('x', 1000, 'x') FROM DUAL CONNECT BY LEVEL <= {rows}"
            ),
            Product::H2 => format!(
                "INSERT INTO {TABLE} SELECT X, REPEAT('x', 1000) FROM SYSTEM_RANGE(1, {rows})"
            ),
            Product::Sqlite => format!(
                "WITH RECURSIVE s(n) AS (SELECT 1 UNION ALL SELECT n + 1 FROM s WHERE n < {rows}) \
                 INSERT INTO {TABLE} SELECT n, substr(hex(zeroblob(500)), 1, 1000) FROM s"
            ),
        };
        vec![create, insert]
    }

    fn one(self) -> &'static str {
        match self {
            Product::Oracle => "SELECT 1 FROM DUAL",
            _ => "SELECT 1",
        }
    }
}

fn connect(product: Product, url: &str, auto_commit: bool, props: &[(String, String)]) -> Session {
    let (class, jar) = product.driver();
    let (user, password) = product.credentials();
    let mut spec = ConnectionSpec::new(url.to_string(), class)
        .with_credentials(user, password)
        .with_jars([jar]);
    spec.auto_commit = auto_commit;
    for (k, v) in props {
        spec = spec.with_prop(k, v);
    }
    Session::open(jvm(), &spec).unwrap_or_else(|e| panic!("{product:?} does not connect: {e}"))
}

/// `RUDBMAN_PROBE_PROPS`, as `k=v;k=v`.
fn env_props() -> Vec<(String, String)> {
    std::env::var("RUDBMAN_PROBE_PROPS")
        .unwrap_or_default()
        .split(';')
        .filter(|p| !p.is_empty())
        .map(|pair| {
            let (k, v) = pair
                .split_once('=')
                .expect("RUDBMAN_PROBE_PROPS is k=v;k=v");
            (k.to_string(), v.to_string())
        })
        .collect()
}

/// How a connection that holds nothing but one open result streams it:
/// `(auto_commit, driver props, fetch size)`.
///
/// Driver configuration, not SQL: pgjdbc only uses a portal with autocommit
/// off, and Connector/J only streams row by row when the fetch size is
/// `Integer.MIN_VALUE` (here through `defaultFetchSize`, with no explicit
/// fetch size to override it). Everything else streams with a plain hint.
fn reader_knobs(product: Product, fetch: u32) -> (bool, Vec<(String, String)>, u32) {
    match product {
        Product::Postgres => (false, Vec::new(), fetch),
        Product::MySql => (
            true,
            vec![("defaultFetchSize".into(), i32::MIN.to_string())],
            0,
        ),
        _ => (true, Vec::new(), fetch),
    }
}

fn count(session: &Session) -> Option<i64> {
    let cursor = session
        .execute(&StatementSpec::new(format!("SELECT COUNT(*) FROM {TABLE}")))
        .ok()?;
    let batch = cursor.fetch(1).ok()?;
    match batch.value(0, 0)? {
        Value::I64(n) => Some(n),
        Value::Str(s) => s.parse().ok(),
        other => format!("{other:?}")
            .chars()
            .filter(char::is_ascii_digit)
            .collect::<String>()
            .parse()
            .ok(),
    }
}

/// Live heap after a full GC, in MB, read with `jcmd` from this very process.
fn live_heap_mb() -> String {
    let pid = std::process::id().to_string();
    let _ = Command::new("jcmd").args([&pid, "GC.run"]).output();
    let Ok(out) = Command::new("jcmd").args([&pid, "GC.heap_info"]).output() else {
        return "n/a".into();
    };
    let text = String::from_utf8_lossy(&out.stdout);
    // Serial GC on a small heap: one "generation ... used NK" line per generation.
    let used_k: Vec<u64> = text
        .lines()
        .filter(|line| line.contains(" heap ") || line.contains(" generation "))
        .filter_map(|line| {
            line.split("used ")
                .nth(1)?
                .split('K')
                .next()?
                .trim()
                .parse()
                .ok()
        })
        .collect();
    if used_k.is_empty() {
        return "n/a".into();
    }
    format!("{}MB", used_k.iter().sum::<u64>() / 1024)
}

fn step<T>(name: &str, f: impl FnOnce() -> Result<T, String>) -> Option<T> {
    let started = Instant::now();
    let result = f();
    let elapsed = started.elapsed();
    match result {
        Ok(value) => {
            println!(
                "PROBE  {name:<28} ok    {:>8.0?}  heap {}",
                elapsed,
                live_heap_mb()
            );
            Some(value)
        }
        Err(error) => {
            let first = error.lines().next().unwrap_or_default();
            println!("PROBE  {name:<28} FAIL  {:>8.0?}  {first}", elapsed);
            None
        }
    }
}

fn probe(product: Product) {
    let Some(url) = product.url() else {
        println!("skipped: no {product:?} server");
        return;
    };
    let rows = env_u32("RUDBMAN_PROBE_ROWS", 300_000);
    let fetch = env_u32("RUDBMAN_PROBE_FETCH", 1_000);
    let auto_commit = env_u32("RUDBMAN_PROBE_AUTOCOMMIT", 1) == 1;
    println!(
        "PROBE  {product:?} rows={rows} heap={}MB fetch={fetch} autocommit={auto_commit} props={:?}",
        env_u32("RUDBMAN_PROBE_HEAP_MB", 128),
        std::env::var("RUDBMAN_PROBE_PROPS").unwrap_or_default()
    );

    // Fill on a session of its own, autocommit on, then close it.
    {
        let setup = connect(product, &url, true, &[]);
        if count(&setup) != Some(rows as i64) {
            let _ = setup.execute(&StatementSpec::new(format!("DROP TABLE {TABLE}")));
            for sql in product.fill(rows) {
                setup
                    .execute(&StatementSpec::new(sql.clone()))
                    .unwrap_or_else(|e| panic!("{sql}: {e}"));
            }
            assert_eq!(
                count(&setup),
                Some(rows as i64),
                "the fill must produce {rows} rows"
            );
        }
        // A run that died mid-probe may have left its structure change behind.
        let _ = setup.execute(&StatementSpec::new(format!(
            "ALTER TABLE {TABLE} DROP COLUMN PROBE_X"
        )));
        setup.close().expect("setup session closes");
    }

    // `shared`: the pre-paging path, cursor and every other command on one
    // session. `dedicated`: the cursor alone on a reader session of its own,
    // configured by `reader_knobs`, while the commands go to the main one.
    let dedicated = std::env::var("RUDBMAN_PROBE_MODE").as_deref() == Ok("dedicated");
    let server_cursor = env_u32("RUDBMAN_PROBE_SERVER_CURSOR", 0) == 1;
    let session = connect(product, &url, auto_commit, &env_props());
    let (reader, fetch_size) = if dedicated {
        let (reader_auto_commit, mut props, fetch_size) = reader_knobs(product, fetch);
        props.extend(env_props());
        println!(
            "PROBE  reader autocommit={reader_auto_commit} props={props:?} fetch_size={fetch_size}"
        );
        (
            Some(connect(product, &url, reader_auto_commit, &props)),
            fetch_size,
        )
    } else {
        (None, fetch)
    };
    let reading = reader.as_ref().unwrap_or(&session);
    step("baseline", || Ok(()));

    let sql = format!("SELECT * FROM {TABLE}");
    let Some(cursor) = step("execute", || {
        reading
            .execute(
                &StatementSpec::new(sql.clone())
                    .with_fetch_size(fetch_size)
                    .with_server_cursor(server_cursor),
            )
            .map_err(|e| e.to_string())
    }) else {
        return;
    };
    step("fetch batch 1", || {
        cursor
            .fetch(fetch)
            .map(|b| b.rows())
            .map_err(|e| e.to_string())
    });
    let others = env_u32("RUDBMAN_PROBE_OTHERS", 1) == 1;
    if others {
        step("describe primary_keys", || {
            let mut request = DescribeRequest::new("primary_keys").with_table(TABLE);
            if product == Product::Postgres {
                request = request.with_table(TABLE.to_lowercase());
            }
            session
                .describe(&request)
                .map(|_| ())
                .map_err(|e| e.to_string())
        });
        step("other query on session", || {
            let other = session
                .execute(&StatementSpec::new(product.one()))
                .map_err(|e| e.to_string())?;
            other.fetch(10).map(|_| ()).map_err(|e| e.to_string())
        });
    }
    step("fetch batch 2", || {
        cursor
            .fetch(fetch)
            .map(|b| b.rows())
            .map_err(|e| e.to_string())
    });
    step("fetch 50 more batches", || {
        let mut rows = 0;
        for _ in 0..50 {
            rows += cursor.fetch(fetch).map_err(|e| e.to_string())?.rows();
        }
        Ok(rows)
    });
    // Rows a little past what has been read, where a server still holding the
    // scan's locks would make this wait. 10 s is the verdict.
    let read = 52 * fetch;
    step("update ahead of cursor", || {
        session
            .execute(
                &StatementSpec::new(format!(
                    "UPDATE {TABLE} SET PAD = PAD WHERE ID > {read} AND ID <= {}",
                    read + 20_000
                ))
                .with_timeout_s(10),
            )
            .map(|c| c.result().update_count)
            .map_err(|e| e.to_string())
    });
    // A structure change from another tab while the grid stays open.
    let (add, drop) = match product {
        Product::Oracle => (
            format!("ALTER TABLE {TABLE} ADD (PROBE_X NUMBER)"),
            format!("ALTER TABLE {TABLE} DROP COLUMN PROBE_X"),
        ),
        Product::MsSql => (
            format!("ALTER TABLE {TABLE} ADD PROBE_X INT"),
            format!("ALTER TABLE {TABLE} DROP COLUMN PROBE_X"),
        ),
        _ => (
            format!("ALTER TABLE {TABLE} ADD COLUMN PROBE_X INT"),
            format!("ALTER TABLE {TABLE} DROP COLUMN PROBE_X"),
        ),
    };
    if step("alter table under cursor", || {
        session
            .execute(&StatementSpec::new(add.clone()).with_timeout_s(10))
            .map(|_| ())
            .map_err(|e| e.to_string())
    })
    .is_some()
    {
        let _ = session.execute(&StatementSpec::new(drop.clone()).with_timeout_s(30));
    }
    if std::env::var("RUDBMAN_PROBE_CLOSE").as_deref() == Ok("cancel") {
        step("cancel statement", || {
            reading.canceller().cancel().map_err(|e| e.to_string())
        });
    }
    step("close cursor", || cursor.close().map_err(|e| e.to_string()));
    step("session still usable", || {
        count(reading)
            .map(|_| ())
            .ok_or_else(|| "count failed".to_string())
    });
    if let Some(reader) = reader {
        step("close reader session", || {
            reader.close().map_err(|e| e.to_string())
        });
    }
}

#[test]
#[ignore = "fills a large table; run by hand, see the module documentation"]
fn postgres() {
    probe(Product::Postgres);
}

#[test]
#[ignore = "fills a large table; run by hand, see the module documentation"]
fn mysql() {
    probe(Product::MySql);
}

#[test]
#[ignore = "fills a large table; run by hand, see the module documentation"]
fn mariadb() {
    probe(Product::MariaDb);
}

#[test]
#[ignore = "fills a large table; run by hand, see the module documentation"]
fn mssql() {
    probe(Product::MsSql);
}

#[test]
#[ignore = "fills a large table; run by hand, see the module documentation"]
fn oracle() {
    probe(Product::Oracle);
}

#[test]
#[ignore = "fills a large table; run by hand, see the module documentation"]
fn h2() {
    probe(Product::H2);
}

#[test]
#[ignore = "fills a large table; run by hand, see the module documentation"]
fn sqlite() {
    probe(Product::Sqlite);
}
