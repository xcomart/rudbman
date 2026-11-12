//! The restore against the real products, opt-in like `rudbman-jdbc`'s
//! `tests/containers.rs`: a product runs when its `RUDBMAN_TEST_*_URL` is set,
//! and passes by doing nothing when it is not. H2 is left to the restore
//! dialog's own test: its fixture calls Java the bridge's tests alone load.
//!
//! ```text
//! docker compose -f docker/compose.yml up -d     # wait for "healthy"
//! export RUDBMAN_TEST_PG_URL=...                 # the URLs in compose.yml's header
//! cargo test -p rudbman-app restore::containers -- --nocapture --test-threads=1
//! ```
//!
//! Each product loads the bridge's round-trip fixture — every kind of object
//! the backup writes, `bridge/src/test/resources/roundtrip/<product>-setup.sql`
//! — into the schema `BackupRoundTripTest` works in, and then:
//!
//! 1. **A rudbman backup.** The scope is backed up, emptied, restored with
//!    [`Restore`], and backed up again. The two backups must be the same file,
//!    and the product's catalog (`<product>-catalog.sql`) must read the same
//!    before and after.
//! 2. **The product's own dump**, where its container has the tool:
//!    `pg_dump --inserts`, `mysqldump` and `mariadb-dump` with routines and
//!    triggers. The dump is restored the same way into an emptied scope and
//!    dumped again, and the two dumps must agree but for the lines the tool
//!    writes differently every time (its header and footer comments, and
//!    `pg_dump`'s `\restrict` key).
//!
//! The containers are reached with `docker exec`, by the names compose gives
//! them, or by `RUDBMAN_TEST_*_CONTAINER`.

use std::path::{Path, PathBuf};
use std::process::Command;

use rudbman_core::{AppSettings, ConnectionProfile, DriverDef};
use rudbman_jdbc::{
    BackupDataOptions, BackupObjects, BackupSpec, Constraints, DdlOptions, JobState, ScopeRef,
    StatementSpec, Value,
};

use super::*;
use crate::connection::{self, Connected};

/// A tool's script, the counts that prove it ran, and whether it is saved as
/// UTF-16.
type ToolScript = (&'static str, &'static [(&'static str, i64)], bool);

/// Where one of the products is, and how it is reset.
struct Product {
    /// The fixture's file prefix: `pg`, `my`, `ma`, `ms`, `ora`, `h2`.
    name: &'static str,
    dialect: Dialect,
    driver: DriverDef,
    /// The scope's own session.
    url: String,
    user: String,
    password: String,
    /// The session the scope is dropped and recreated on, when it is not the
    /// scope's own.
    admin: Option<(String, String, String)>,
    /// What empties the scope, run on the admin session.
    reset: Vec<String>,
    /// What empties the scope when no fixed list of statements can: Db2,
    /// whose schema is emptied by hand.
    reset_by: Option<fn(&Connected)>,
    /// What runs on the scope's own session after the reset.
    after_reset: Vec<String>,
    scope: Scope,
    /// The container and the command that dumps the scope to stdout.
    dump: Option<(String, Vec<String>)>,
    /// The product's own client, reading a script on its standard input in
    /// the scope: what a dump's restore is held against.
    client: Vec<String>,
    /// A script in the shape a tool with no command line in the container
    /// writes — SSMS's "Generate Scripts", SQL Developer's export — with the
    /// counts that prove it ran, and whether it is saved as UTF-16.
    tool_script: Option<ToolScript>,
    /// How many of the `reset` statements run before a dump is restored:
    /// `pg_dump -n` creates the schema itself.
    dump_reset: usize,
    /// The lines of a script that differ between two faithful backups.
    normalize: fn(&str) -> Option<String>,
    /// The catalog rows that differ between two faithful copies: names the
    /// server makes up.
    snapshot: fn(&str) -> Option<String>,
}

fn env(key: &str, default: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| default.to_string())
}

/// The newest `<artifact>-<version>.jar` of `group` in the Gradle cache, or
/// what `RUDBMAN_TEST_<key>_JAR` names.
fn jar(key: &str, group: &str, artifact: &str) -> PathBuf {
    if let Some(path) = std::env::var_os(format!("RUDBMAN_TEST_{key}_JAR")) {
        return PathBuf::from(path);
    }
    let home = std::env::var_os("GRADLE_USER_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".gradle")))
        .expect("a home directory");
    let root = home
        .join("caches/modules-2/files-2.1")
        .join(group)
        .join(artifact);
    let mut found: Vec<PathBuf> = Vec::new();
    for version in std::fs::read_dir(&root)
        .unwrap_or_else(|_| {
            panic!(
                "no {artifact} in {}; run `cd bridge && ./gradlew drivers`",
                root.display()
            )
        })
        .flatten()
    {
        let wanted = format!("{artifact}-{}.jar", version.file_name().to_string_lossy());
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
                if file.file_name().to_string_lossy() == wanted {
                    found.push(file.path());
                }
            }
        }
    }
    found.sort();
    found
        .pop()
        .unwrap_or_else(|| panic!("no {artifact} JAR under {}", root.display()))
}

fn driver(id: &str, class: &str, jar: PathBuf, dialect: &str) -> DriverDef {
    DriverDef {
        id: id.to_string(),
        name: id.to_string(),
        class: class.to_string(),
        jars: vec![jar],
        dialect: dialect.to_string(),
        ..DriverDef::default()
    }
}

fn keep(line: &str) -> Option<String> {
    Some(line.to_string())
}

fn products() -> Vec<Product> {
    let mut out = Vec::new();
    if let Ok(url) = std::env::var("RUDBMAN_TEST_PG_URL") {
        let user = env("RUDBMAN_TEST_PG_USER", "rudbman");
        let database = env("RUDBMAN_TEST_PG_DATABASE", "rudbman");
        let container = env("RUDBMAN_TEST_PG_CONTAINER", "rudbman-test-postgres");
        out.push(Product {
            name: "pg",
            dialect: Dialect::POSTGRES,
            driver: driver(
                "postgresql",
                "org.postgresql.Driver",
                jar("PG", "org.postgresql", "postgresql"),
                "postgres",
            ),
            url,
            password: env("RUDBMAN_TEST_PG_PASSWORD", "rudbman"),
            user: user.clone(),
            admin: None,
            reset: vec![
                "drop schema if exists bk cascade".into(),
                "create schema bk".into(),
            ],
            reset_by: None,
            after_reset: Vec::new(),
            scope: Scope {
                catalog: None,
                schema: Some("bk".into()),
            },
            tool_script: None,
            client: [
                "psql",
                "-X",
                "-q",
                "-U",
                &user,
                "-d",
                &database,
                "-v",
                "ON_ERROR_STOP=1",
            ]
            .map(String::from)
            .to_vec(),
            dump_reset: 1,
            dump: Some((
                container,
                [
                    "pg_dump",
                    "-U",
                    &user,
                    "-d",
                    &database,
                    "--inserts",
                    "-n",
                    "bk",
                    "--extension=pgcrypto",
                ]
                .map(String::from)
                .to_vec(),
            )),
            normalize: |line| {
                (!line.starts_with("--")
                    && !line.starts_with("\\restrict")
                    && !line.starts_with("\\unrestrict"))
                .then(|| line.to_string())
            },
            snapshot: keep,
        });
    }
    for (key, name, client, container, class, group, artifact, dialect, collation) in [
        (
            "MYSQL",
            "my",
            "mysqldump",
            "rudbman-test-mysql",
            "com.mysql.cj.jdbc.Driver",
            "com.mysql",
            "mysql-connector-j",
            Dialect::MYSQL,
            "utf8mb4_0900_ai_ci",
        ),
        (
            "MARIADB",
            "ma",
            "mariadb-dump",
            "rudbman-test-mariadb",
            "org.mariadb.jdbc.Driver",
            "org.mariadb.jdbc",
            "mariadb-java-client",
            Dialect::MARIADB,
            "utf8mb4_uca1400_ai_ci",
        ),
    ] {
        let Ok(url) = std::env::var(format!("RUDBMAN_TEST_{key}_URL")) else {
            continue;
        };
        let password = env(&format!("RUDBMAN_TEST_{key}_ROOT_PASSWORD"), "rudbman");
        let container = env(&format!("RUDBMAN_TEST_{key}_CONTAINER"), container);
        out.push(Product {
            name,
            dialect,
            driver: driver(
                name,
                class,
                jar(key, group, artifact),
                dialect.id().as_str(),
            ),
            url,
            user: "root".into(),
            password: password.clone(),
            admin: None,
            reset: vec![
                "drop database if exists bk".into(),
                format!("create database bk character set utf8mb4 collate {collation}"),
            ],
            reset_by: None,
            after_reset: Vec::new(),
            scope: Scope {
                catalog: Some("bk".into()),
                schema: None,
            },
            tool_script: None,
            client: [
                "env",
                &format!("MYSQL_PWD={password}"),
                if name == "my" { "mysql" } else { "mariadb" },
                "-uroot",
                "--default-character-set=utf8mb4",
                "bk",
            ]
            .map(String::from)
            .to_vec(),
            dump_reset: 2,
            dump: Some((
                container,
                [
                    "env",
                    &format!("MYSQL_PWD={password}"),
                    client,
                    "-uroot",
                    "--default-character-set=utf8mb4",
                    "--routines",
                    "--triggers",
                    "--hex-blob",
                    "--skip-dump-date",
                    "bk",
                ]
                .map(String::from)
                .to_vec(),
            )),
            normalize: |line| (!line.starts_with("-- ")).then(|| line.to_string()),
            snapshot: keep,
        });
    }
    if let Ok(url) = std::env::var("RUDBMAN_TEST_MSSQL_URL") {
        let user = env("RUDBMAN_TEST_MSSQL_USER", "sa");
        let password = env("RUDBMAN_TEST_MSSQL_PASSWORD", "Rudbman!Passw0rd");
        out.push(Product {
            name: "ms",
            dialect: Dialect::MSSQL,
            driver: driver(
                "mssql",
                "com.microsoft.sqlserver.jdbc.SQLServerDriver",
                jar("MSSQL", "com.microsoft.sqlserver", "mssql-jdbc"),
                "mssql",
            ),
            url: format!("{url};databaseName=bkdb"),
            user: user.clone(),
            password: password.clone(),
            admin: Some((url, user, password)),
            reset: vec![
                "if db_id('bkdb') is not null begin alter database bkdb set single_user with \
                 rollback immediate; drop database bkdb; end"
                    .into(),
                "create database bkdb".into(),
            ],
            reset_by: None,
            after_reset: vec!["create schema bk".into()],
            scope: Scope {
                catalog: Some("bkdb".into()),
                schema: Some("bk".into()),
            },
            dump: None,
            client: Vec::new(),
            dump_reset: 0,
            tool_script: Some((
                SSMS,
                &[
                    ("select count(*) from bk.ssms_t", 2),
                    (
                        "select count(*) from bk.ssms_t where name = N'한글; semi'",
                        1,
                    ),
                    (
                        "select count(*) from sys.procedures where name = 'ssms_p'",
                        1,
                    ),
                ],
                true,
            )),
            normalize: keep,
            snapshot: keep,
        });
    }
    if let Ok(url) = std::env::var("RUDBMAN_TEST_ORACLE_URL") {
        let system = env("RUDBMAN_TEST_ORACLE_SYSTEM_PASSWORD", "rudbman");
        out.push(Product {
            name: "ora",
            dialect: Dialect::ORACLE,
            driver: driver(
                "oracle",
                "oracle.jdbc.OracleDriver",
                jar("ORACLE", "com.oracle.database.jdbc", "ojdbc11"),
                "oracle",
            ),
            url: url.clone(),
            user: "bk".into(),
            password: "bk".into(),
            admin: Some((url, "system".into(), system)),
            reset: vec![
                "begin execute immediate 'drop user bk cascade'; exception when others then \
                 if sqlcode != -1918 then raise; end if; end;"
                    .into(),
                "create user bk identified by bk quota unlimited on users".into(),
                "grant create session, create table, create view, create procedure, create \
                 trigger, create sequence, create materialized view to bk"
                    .into(),
            ],
            reset_by: None,
            after_reset: Vec::new(),
            scope: Scope {
                catalog: None,
                schema: Some("BK".into()),
            },
            dump: None,
            client: Vec::new(),
            dump_reset: 0,
            tool_script: Some((SQL_DEVELOPER, &[
                ("select count(*) from bk.sqldev_t", 2),
                ("select count(*) from bk.sqldev_t where name = 'A & B; c'", 1),
                ("select count(*) from all_procedures where owner = 'BK' and object_name = 'SQLDEV_P'", 1),
            ], false)),
            // A restored identity starts where its source left off, and
            // DBMS_METADATA says so.
            normalize: |line| {
                let mut out = String::with_capacity(line.len());
                let mut rest = line;
                while let Some(at) = rest.find("START WITH ") {
                    out.push_str(&rest[..at + "START WITH ".len()]);
                    rest = rest[at + "START WITH ".len()..].trim_start_matches(|c: char| c.is_ascii_digit());
                    out.push('n');
                }
                out.push_str(rest);
                Some(out)
            },
            // System-generated names, and the old tables the DROPs leave in
            // the recycle bin.
            snapshot: |line| {
                if line.contains("BIN$") {
                    return None;
                }
                let line = regex::Regex::new(r"ISEQ\$\$_\d+").unwrap().replace_all(line, "ISEQ$$$$_N");
                Some(regex::Regex::new(r"SYS_C\d+").unwrap().replace_all(&line, "SYS_C").into_owned())
            },
        });
    }
    if let Ok(url) = std::env::var("RUDBMAN_TEST_SQLITE_URL") {
        let file = url.strip_prefix("jdbc:sqlite:").unwrap_or(&url).to_string();
        // No `sqlite3` shell is assumed: Python's module dumps the way the
        // shell's `.dump` does and runs a script the way the shell splits one.
        let python = |code: &str, args: &[&str]| {
            let mut command = vec!["python3".to_string(), "-I".into(), "-c".into(), code.into()];
            command.extend(args.iter().map(|arg| arg.to_string()));
            command
        };
        out.push(Product {
            name: "sl",
            dialect: Dialect::SQLITE,
            driver: driver(
                "sqlite",
                "org.sqlite.JDBC",
                jar("SQLITE", "org.xerial", "sqlite-jdbc"),
                "sqlite",
            ),
            url,
            user: String::new(),
            password: String::new(),
            admin: None,
            reset: Vec::new(),
            reset_by: Some(empty_sqlite),
            after_reset: Vec::new(),
            scope: Scope::default(),
            dump: Some((
                String::new(),
                python(
                    "import sqlite3, sys; c = sqlite3.connect(sys.argv[1]); \
                     sys.stdout.write('\\n'.join(c.iterdump()) + '\\n')",
                    &[&file],
                ),
            )),
            client: python(
                "import sqlite3, sys; c = sqlite3.connect(sys.argv[1]); \
                 c.executescript(sys.stdin.read()); c.close()",
                &[&file],
            ),
            dump_reset: 0,
            tool_script: None,
            normalize: keep,
            snapshot: keep,
        });
    }
    if let Ok(url) = std::env::var("RUDBMAN_TEST_DB2_URL") {
        out.push(Product {
            name: "db2",
            dialect: Dialect::DB2,
            driver: driver(
                "db2",
                "com.ibm.db2.jcc.DB2Driver",
                jar("DB2", "com.ibm.db2", "jcc"),
                "db2",
            ),
            url,
            user: env("RUDBMAN_TEST_DB2_USER", "rudbman"),
            password: env("RUDBMAN_TEST_DB2_PASSWORD", "rudbman"),
            admin: None,
            reset: Vec::new(),
            reset_by: Some(empty_db2_schema),
            after_reset: Vec::new(),
            scope: Scope {
                catalog: None,
                schema: Some("BK".into()),
            },
            dump: None,
            client: Vec::new(),
            dump_reset: 0,
            tool_script: None,
            normalize: keep,
            snapshot: |line| {
                Some(
                    regex::Regex::new(r"SQL\d{15}")
                        .unwrap()
                        .replace_all(line, "SQLnnn")
                        .into_owned(),
                )
            },
        });
    }
    out
}

/// Drops everything a SQLite database holds, in passes.
fn empty_sqlite(connected: &Connected) {
    for _ in 0..5 {
        let cursor = connected
            .session()
            .execute(&StatementSpec::new(
                "select 'drop ' || type || ' \"' || name || '\"' from sqlite_master \
                 where type in ('table', 'view', 'trigger') and name not like 'sqlite_%'",
            ))
            .expect("the catalog reads");
        let batch = cursor.fetch(1000).expect("the catalog reads");
        let drops: Vec<String> = (0..batch.rows())
            .filter_map(|row| match batch.value(row, 0) {
                Some(Value::Str(drop)) => Some(drop.to_string()),
                _ => None,
            })
            .collect();
        drop(cursor);
        if drops.is_empty() {
            return;
        }
        for sql in drops {
            let _ = execute(connected.session(), &sql);
        }
    }
    panic!("could not empty the SQLite database");
}

/// Empties the schema `BK` the way `BackupRoundTripTest` does: by hand, in
/// passes, because `SYSPROC.ADMIN_DROP_SCHEMA` needs a table space a new
/// database does not have.
fn empty_db2_schema(connected: &Connected) {
    let cursor = connected
        .session()
        .execute(&StatementSpec::new(
            "select 'drop trigger BK.\"' || trigname || '\"' from syscat.triggers where trigschema = 'BK' \
             union all select 'drop view BK.\"' || viewname || '\"' from syscat.views v where viewschema = 'BK' \
             and not exists (select 1 from syscat.indexes i where i.viewschema = v.viewschema and i.viewname = v.viewname) \
             union all select 'drop specific ' || case routinetype when 'P' then 'procedure' else 'function' end \
             || ' BK.\"' || specificname || '\"' from syscat.routines where routineschema = 'BK' \
             union all select 'drop table BK.\"' || tabname || '\"' from syscat.tables where tabschema = 'BK' and type = 'T' \
             union all select 'drop sequence BK.\"' || seqname || '\"' from syscat.sequences where seqschema = 'BK' and seqtype = 'S'",
        ))
        .expect("the catalog reads");
    let mut drops = Vec::new();
    loop {
        let batch = cursor.fetch(1000).expect("the catalog reads");
        for row in 0..batch.rows() {
            if let Some(Value::Str(drop)) = batch.value(row, 0) {
                drops.push(drop.to_string());
            }
        }
        if batch.is_last() {
            break;
        }
    }
    drop(cursor);
    for _ in 0..5 {
        drops.retain(|sql| execute(connected.session(), sql).is_err());
        if drops.is_empty() {
            break;
        }
    }
    assert!(drops.is_empty(), "could not empty the schema BK: {drops:?}");
    let _ = execute(connected.session(), "create schema BK");
}

fn connect(
    driver: &DriverDef,
    dialect: &Dialect,
    url: &str,
    user: &str,
    password: &str,
) -> Connected {
    let mut profile = ConnectionProfile::new("restore-roundtrip", &driver.id, url, user);
    profile.read_only = false;
    profile.confirm_writes = false;
    let _ = dialect;
    connection::connect(
        &profile,
        driver,
        &connection::Credentials::typed(Some(password.to_string()), None),
        &AppSettings::default(),
    )
    .unwrap_or_else(|error| panic!("{url}: {error}"))
}

fn run_all(connected: &Connected, statements: &[String]) {
    for sql in statements {
        execute(connected.session(), sql).unwrap_or_else(|error| panic!("{sql}: {error}"));
    }
}

/// The scope's own session, after the scope has been emptied.
fn reset(product: &Product) -> Connected {
    reset_with(product, &product.reset)
}

fn reset_with(product: &Product, statements: &[String]) -> Connected {
    if let Some(reset_by) = product.reset_by {
        let own = connect(
            &product.driver,
            &product.dialect,
            &product.url,
            &product.user,
            &product.password,
        );
        reset_by(&own);
        return own;
    }
    match &product.admin {
        Some((url, user, password)) => {
            let admin = connect(&product.driver, &product.dialect, url, user, password);
            run_all(&admin, statements);
            admin.close().ok();
        }
        None => {
            let own = connect(
                &product.driver,
                &product.dialect,
                &product.url,
                &product.user,
                &product.password,
            );
            run_all(&own, statements);
            own.close().ok();
        }
    }
    let own = connect(
        &product.driver,
        &product.dialect,
        &product.url,
        &product.user,
        &product.password,
    );
    run_all(&own, &product.after_reset);
    own
}

fn fixture(name: &str, kind: &str) -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../bridge/src/test/resources/roundtrip")
        .join(format!("{name}-{kind}.sql"));
    std::fs::read_to_string(&path).unwrap_or_else(|error| panic!("{}: {error}", path.display()))
}

fn chunks(text: &str) -> Vec<String> {
    text.split("\n--;;\n")
        .map(str::trim)
        .filter(|chunk| !chunk.is_empty())
        .map(String::from)
        .collect()
}

/// Every row the product's catalog queries answer, as text, sorted.
///
/// None on SQLite, whose `PRAGMA` answers the bridge cannot read as a result
/// set; its backups and dumps, which hold every object and row, are compared
/// instead.
fn catalog(product: &Product, connected: &Connected) -> Vec<String> {
    let mut out = Vec::new();
    if product.dialect.id() == rudbman_sql::DialectId::Sqlite {
        return out;
    }
    for query in chunks(&fixture(product.name, "catalog")) {
        let sql: String = query
            .lines()
            .filter(|line| !line.trim_start().starts_with("--"))
            .collect::<Vec<_>>()
            .join("\n");
        if sql.trim().is_empty() {
            continue;
        }
        let cursor = connected
            .session()
            .execute(&StatementSpec::new(sql.as_str()))
            .unwrap_or_else(|error| panic!("{sql}: {error}"));
        loop {
            let batch = cursor.fetch(1000).expect("the catalog reads");
            for row in 0..batch.rows() {
                let cells: Vec<String> = (0..batch.column_count())
                    .map(|column| match batch.value(row, column) {
                        Some(Value::Str(text)) => text.to_string(),
                        // MySQL's catalog answers in LONGTEXT, which the
                        // bridge leaves on its side.
                        Some(Value::Lob { id, .. }) => {
                            let file = cursor.cache_lob(id, false).expect("the LOB reads");
                            let bytes = file
                                .prefix(file.byte_len() as usize)
                                .expect("the LOB reads");
                            String::from_utf8_lossy(&bytes).into_owned()
                        }
                        Some(other) => format!("{other:?}"),
                        None => "?".into(),
                    })
                    .collect();
                let line = format!(
                    "{}: {}",
                    query.lines().next().unwrap_or(""),
                    cells.join(" | ")
                );
                if let Some(line) = (product.snapshot)(&line) {
                    out.push(line);
                }
            }
            if batch.is_last() {
                break;
            }
        }
    }
    out.sort();
    out
}

/// A backup of the scope, as the bridge writes it.
fn backup(product: &Product, connected: &Connected, path: &Path) -> Vec<String> {
    let mut scope = ScopeRef::new();
    if let Some(catalog) = &product.scope.catalog {
        scope = scope.with_catalog(catalog);
    }
    if let Some(schema) = &product.scope.schema {
        scope = scope.with_schema(schema);
    }
    let spec = BackupSpec::new(path)
        .with_scope(scope)
        .with_ddl(DdlOptions::included().with_constraints(Constraints::Alter))
        .with_objects(BackupObjects::all())
        .with_data(BackupDataOptions::included().with_insert_batch_rows(1));
    let mut job = connected
        .session()
        .start_backup(&spec)
        .expect("the backup starts");
    let progress = loop {
        let progress = job.poll().expect("the backup reports");
        if progress.is_terminal() {
            break progress;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    };
    assert_eq!(
        progress.state,
        JobState::Done,
        "{}: {:?}",
        product.name,
        progress.errors
    );
    lines(
        product,
        &std::fs::read_to_string(path).expect("the backup was written"),
    )
}

fn lines(product: &Product, text: &str) -> Vec<String> {
    text.lines().filter_map(product.normalize).collect()
}

/// Runs `path` through the restore on `connected`, and insists it ran clean.
fn restore_file(product: &Product, connected: &Connected, path: &Path) -> u64 {
    let restore = Restore::start(
        connected.handle(),
        RestoreRequest {
            path: path.to_path_buf(),
            dialect: product.dialect,
            continue_on_error: false,
            enter: enter_scope(&product.dialect, &product.scope),
        },
    );
    let outcome = loop {
        if let Some(outcome) = restore.outcome() {
            break outcome;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    };
    match outcome {
        Outcome::Done {
            statements,
            failed: 0,
            ..
        } => statements,
        other => panic!(
            "{}: restoring {} ended {other:#?}",
            product.name,
            path.display()
        ),
    }
}

/// `command` in `container`, or on this machine when there is none.
fn in_container(container: &str, command: &[String]) -> Command {
    if container.is_empty() {
        let mut local = Command::new(&command[0]);
        local.args(&command[1..]);
        return local;
    }
    let mut exec = Command::new("docker");
    exec.args(["exec", "-i", container]).args(command);
    exec
}

fn dump(product: &Product, path: &Path) -> Option<Vec<String>> {
    let (container, command) = product.dump.as_ref()?;
    let output = in_container(container, command)
        .output()
        .expect("the dump runs");
    assert!(
        output.status.success(),
        "{}: {}",
        product.name,
        String::from_utf8_lossy(&output.stderr)
    );
    std::fs::write(path, &output.stdout).expect("the dump is written");
    Some(lines(
        product,
        &String::from_utf8(output.stdout).expect("the dump is UTF-8"),
    ))
}

/// Equal lists, or a failure naming the lines only one side has.
fn same(expected: &[String], actual: &[String], what: &str) {
    if expected == actual {
        return;
    }
    let only: Vec<_> = expected
        .iter()
        .filter(|line| !actual.contains(line))
        .take(30)
        .collect();
    let extra: Vec<_> = actual
        .iter()
        .filter(|line| !expected.contains(line))
        .take(30)
        .collect();
    panic!("{what}\n- {only:#?}\n+ {extra:#?}");
}

fn round_trip(product: &Product) {
    let dir = tempfile::tempdir().unwrap();
    let loading = reset(product);
    for chunk in chunks(&fixture(product.name, "setup")) {
        execute(loading.session(), &chunk).unwrap_or_else(|error| {
            panic!("{}: the fixture failed at {chunk}: {error}", product.name)
        });
    }
    loading.close().ok();
    // Read on a session of its own, as every later backup is: MariaDB's
    // driver spells a TIMESTAMP(3) with five decimals on the session that
    // inserted it and with six on any other.
    let source = connect(
        &product.driver,
        &product.dialect,
        &product.url,
        &product.user,
        &product.password,
    );
    let snapshot = catalog(product, &source);
    let first = backup(product, &source, &dir.path().join("backup.sql"));
    let dumped = dump(product, &dir.path().join("dump.sql"));
    // Without `--hex-blob`, the fixture's binary columns go into the dump as
    // raw bytes, which no statement sent as text can carry.
    let raw = dir.path().join("raw.sql");
    let raw_dumped = product.dump.as_ref().and_then(|(container, command)| {
        command.iter().any(|arg| arg == "--hex-blob").then(|| {
            let kept: Vec<String> = command
                .iter()
                .filter(|arg| *arg != "--hex-blob")
                .cloned()
                .collect();
            let output = in_container(container, &kept)
                .output()
                .expect("the dump runs");
            std::fs::write(&raw, &output.stdout).expect("the dump is written");
        })
    });
    source.close().ok();

    // 1. The backup, restored and backed up again.
    let target = reset(product);
    let ran = restore_file(product, &target, &dir.path().join("backup.sql"));
    same(
        &snapshot,
        &catalog(product, &target),
        &format!("{}: the catalog after the restore", product.name),
    );
    let again = backup(product, &target, &dir.path().join("again.sql"));
    same(
        &first,
        &again,
        &format!("{}: a backup of the restore", product.name),
    );
    target.close().ok();
    println!("{}: the backup restored, {ran} statements", product.name);

    // 1b. A script in the shape of a tool the container has no command line
    //     for.
    if let Some((script, counts, utf16)) = product.tool_script {
        let path = dir.path().join("tool.sql");
        let bytes = if utf16 {
            let mut bytes = vec![0xFF, 0xFE];
            for unit in script.encode_utf16() {
                bytes.extend_from_slice(&unit.to_le_bytes());
            }
            bytes
        } else {
            script.as_bytes().to_vec()
        };
        std::fs::write(&path, bytes).unwrap();
        let target = reset(product);
        let ran = restore_file(product, &target, &path);
        for (sql, expected) in counts {
            let cursor = target
                .session()
                .execute(&StatementSpec::new(*sql))
                .unwrap_or_else(|error| panic!("{sql}: {error}"));
            let count = match cursor.fetch(1).unwrap().value(0, 0) {
                Some(Value::I64(count)) => count,
                Some(Value::Str(text)) => text.trim().parse().unwrap(),
                other => panic!("{sql}: {other:?}"),
            };
            assert_eq!(count, *expected, "{}: {sql}", product.name);
        }
        target.close().ok();
        println!(
            "{}: a tool's script restored, {ran} statements",
            product.name
        );
    }

    // 2. The product's own dump, restored and dumped again, and held against
    //    the same dump restored by the product's own client: a dump does not
    //    always rebuild its source exactly (a MySQL view's columns come back
    //    NOT NULL), and what the restore has to match is the client.
    let Some(dumped) = dumped else {
        return;
    };
    let target = reset_with(product, &product.reset[..product.dump_reset]);
    let ran = restore_file(product, &target, &dir.path().join("dump.sql"));
    let restored = catalog(product, &target);
    target.close().ok();
    let redumped = dump(product, &dir.path().join("redump.sql")).expect("the tool is there");
    same(
        &dumped,
        &redumped,
        &format!("{}: a dump of the restored dump", product.name),
    );

    let (container, _) = product.dump.as_ref().expect("a dump has a container");
    reset_with(product, &product.reset[..product.dump_reset])
        .close()
        .ok();
    let output = in_container(container, &product.client)
        .stdin(std::fs::File::open(dir.path().join("dump.sql")).expect("the dump is there"))
        .output()
        .expect("the client runs");
    assert!(
        output.status.success(),
        "{}: the client could not restore its own dump: {}",
        product.name,
        String::from_utf8_lossy(&output.stderr)
    );
    let target = connect(
        &product.driver,
        &product.dialect,
        &product.url,
        &product.user,
        &product.password,
    );
    same(
        &catalog(product, &target),
        &restored,
        &format!(
            "{}: the dump restored by rudbman and by the client",
            product.name
        ),
    );
    target.close().ok();
    println!(
        "{}: the dump restored as the client restores it, {ran} statements",
        product.name
    );

    if raw_dumped.is_some() {
        let target = reset_with(product, &product.reset[..product.dump_reset]);
        let restore = Restore::start(
            target.handle(),
            RestoreRequest {
                path: raw.clone(),
                dialect: product.dialect,
                continue_on_error: false,
                enter: enter_scope(&product.dialect, &product.scope),
            },
        );
        let outcome = loop {
            if let Some(outcome) = restore.outcome() {
                break outcome;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        };
        assert!(
            matches!(outcome, Outcome::Refused(Refusal::NotText { .. })),
            "{}: a dump with raw binary was not refused: {outcome:?}",
            product.name
        );
        let cursor = target
            .session()
            .execute(&StatementSpec::new(
                "select count(*) from information_schema.tables where table_schema = 'bk'",
            ))
            .expect("the count runs");
        assert!(
            matches!(cursor.fetch(1).unwrap().value(0, 0), Some(Value::I64(0))),
            "{}: a refused dump ran something",
            product.name
        );
        drop(cursor);
        target.close().ok();
        println!(
            "{}: a dump without --hex-blob is refused, {outcome:?}",
            product.name
        );
    }
}

#[test]
fn backups_and_client_dumps_restore_on_real_servers() {
    for product in &products() {
        round_trip(product);
    }
}

/// What SSMS's "Generate Scripts" writes, data included: batches ended by
/// `GO`, statements without semicolons, a routine whose body runs to its
/// batch's end.
const SSMS: &str = "USE [bkdb]
GO
/****** Object:  Table [bk].[ssms_t]    Script Date: 11/12/2026 ******/
SET ANSI_NULLS ON
GO
SET QUOTED_IDENTIFIER ON
GO
CREATE TABLE [bk].[ssms_t](
\t[id] [int] IDENTITY(1,1) NOT NULL,
\t[name] [nvarchar](50) NULL,
 CONSTRAINT [PK_ssms_t] PRIMARY KEY CLUSTERED 
(
\t[id] ASC
)WITH (PAD_INDEX = OFF, STATISTICS_NORECOMPUTE = OFF) ON [PRIMARY]
) ON [PRIMARY]
GO
SET IDENTITY_INSERT [bk].[ssms_t] ON 
GO
INSERT [bk].[ssms_t] ([id], [name]) VALUES (1, N'한글; semi')
INSERT [bk].[ssms_t] ([id], [name]) VALUES (2, N'GO')
GO
SET IDENTITY_INSERT [bk].[ssms_t] OFF
GO
/****** Object:  StoredProcedure [bk].[ssms_p] ******/
SET ANSI_NULLS ON
GO
CREATE PROCEDURE [bk].[ssms_p]
AS
BEGIN
\tSET NOCOUNT ON;
\tSELECT COUNT(*) FROM [bk].[ssms_t];
END
GO
EXEC [bk].[ssms_p]
GO
";

/// What SQL Developer's "Database Export" writes: SQL\*Plus's `REM` and
/// `SET DEFINE OFF`, a unit ended by a slash after a blank line, and an
/// `exec`.
const SQL_DEVELOPER: &str = "--------------------------------------------------------
--  File created - Thursday-November-12-2026   
--------------------------------------------------------
--------------------------------------------------------
--  DDL for Table SQLDEV_T
--------------------------------------------------------

  CREATE TABLE \"BK\".\"SQLDEV_T\" 
   (\t\"ID\" NUMBER, 
\t\"NAME\" VARCHAR2(50 BYTE)
   ) ;
REM INSERTING into BK.SQLDEV_T
SET DEFINE OFF;
Insert into BK.SQLDEV_T (ID,NAME) values ('1','A & B; c');
Insert into BK.SQLDEV_T (ID,NAME) values ('2','한글');
--------------------------------------------------------
--  DDL for Procedure SQLDEV_P
--------------------------------------------------------
set define off;

  CREATE OR REPLACE EDITIONABLE PROCEDURE \"BK\".\"SQLDEV_P\" AS 
BEGIN
  UPDATE BK.SQLDEV_T SET NAME = NAME WHERE ID = 1;
END;

/
--------------------------------------------------------
--  Constraints for Table SQLDEV_T
--------------------------------------------------------

  ALTER TABLE \"BK\".\"SQLDEV_T\" MODIFY (\"ID\" NOT NULL ENABLE);
exec BK.SQLDEV_P;
";
