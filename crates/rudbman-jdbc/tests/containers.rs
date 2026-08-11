//! Opt-in end-to-end tests against a real PostgreSQL and a real MySQL.
//!
//! `tests/h2.rs` proves that Rust reads what Java wrote, and its editing
//! section proves that the `UPDATE`s `rudbman-sql` plans and the parameters
//! this crate binds fit together against a real driver. This file is that
//! second sentence applied to **DDL**: `rudbman-sql::ddl` writes `CREATE TABLE`
//! and `ALTER TABLE` for six products, and until this file existed every one of
//! those statements had only ever been compared against a string somebody
//! reasoned out from documentation. A statement nobody has sent is a claim, not
//! a fact.
//!
//! So each test here drives `plan_create`/`plan_alter`, runs what comes back
//! through a real `Session`, and then **reads the catalogue back** with
//! `DescribeRequest`. The reading back is the point. A server that accepted a
//! statement has not thereby done what §7.10 says it did — MySQL's
//! `MODIFY COLUMN` accepts a definition that silently drops a `NOT NULL` just
//! as happily as one that keeps it, which is exactly what
//! [`mysql_modify_column_restates_the_whole_definition`] demonstrates by
//! sending both.
//!
//! Oracle and SQL Server are out of scope: their images are heavy and their
//! licensing is not a thing a test suite should assume. Four of §7.10's claims
//! are reachable from here — MySQL's restating, MySQL's four constraint-drop
//! spellings, PostgreSQL's independent clauses, and the statement order — and
//! they are the four this file settles.
//!
//! # Opt-in, and silent when it is out
//!
//! Every test here passes by doing nothing when its server's URL is unset, and
//! says so in one line. That is deliberately *not* what `h2_jar()` does: H2 is
//! a dependency the repository guarantees, so a missing H2 is a broken
//! checkout and a panic; a database server is a container the developer chose
//! to start, and CI has none. `cargo test --workspace` must stay green on a
//! machine with no Docker.
//!
//! ```text
//! docker compose -f docker/compose.yml up -d      # then wait for "healthy"
//! export RUDBMAN_TEST_PG_URL='jdbc:postgresql://127.0.0.1:55432/rudbman'
//! export RUDBMAN_TEST_MYSQL_URL='jdbc:mysql://127.0.0.1:33306/rudbman?allowPublicKeyRetrieval=true&useSSL=false'
//! cargo test -p rudbman-jdbc --test containers
//! ```
//!
//! The user and password default to `rudbman`/`rudbman`, which is what
//! `docker/compose.yml` sets up; `RUDBMAN_TEST_PG_USER`,
//! `RUDBMAN_TEST_PG_PASSWORD`, `RUDBMAN_TEST_MYSQL_USER` and
//! `RUDBMAN_TEST_MYSQL_PASSWORD` override them.
//!
//! # The driver JARs
//!
//! Found the way `tests/h2.rs` finds H2's, and for the same reason — the
//! Gradle cache is already the one place in this checkout where a driver
//! lives. What fills it is `cd bridge && ./gradlew drivers`, a task whose only
//! job is to resolve the two drivers; they are *not* `testImplementation`,
//! because no Java test in this project loads either of them.
//! `RUDBMAN_TEST_PG_JAR` and `RUDBMAN_TEST_MYSQL_JAR` override the search.
//!
//! A missing JAR **is** a panic, unlike a missing server: by the time one is
//! looked for the developer has already set a URL and asked for the test to
//! run, and a test that passes because it could not find its driver is the
//! thing this file exists to avoid.
//!
//! # Cleaning up
//!
//! Every table is named `rb_<what>_<pid>_<n>`, so two runs — or two developers
//! against one server — cannot collide, and every one of them is registered
//! with the [`Server`] that drops it however the test ends. Tables go in
//! reverse order of creation, so a child goes before the parent it references.

use std::cell::RefCell;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};

use rudbman_jdbc::{
    Batch, ConnectionSpec, DescribeRequest, Jvm, JvmConfig, Session, StatementSpec, Value,
    default_bridge_jar,
};
// The generator under test. `rudbman-sql` is already a dev-dependency of this
// crate for the editing tests in `h2.rs`, and for the same reason: writing the
// `ALTER TABLE`s out by hand here would prove something about a different
// string than the one the application sends.
use rudbman_sql::{
    ColumnChange, ColumnDef, ConstraintDrop, ConstraintKind, Dialect, TableAlter, TableConstraint,
    TableCreate, plan_alter, plan_create,
};

/// The process-wide JVM, started by whichever test needs it first.
fn jvm() -> &'static Jvm {
    Jvm::start(&JvmConfig::new(default_bridge_jar()).with_heap_mb(256))
        .expect("the JVM must start; build the bridge with `cd bridge && ./gradlew jar`")
}

// --- finding the two drivers -----------------------------------------------

/// Locates a driver JAR, or fails with instructions.
///
/// Only ever called once a server URL has been set, so a JAR that is not there
/// is a panic rather than a skip — see the module documentation.
fn driver_jar(env: &str, group: &str, artifact: &str) -> PathBuf {
    if let Some(path) = std::env::var_os(env) {
        let path = PathBuf::from(path);
        assert!(
            path.is_file(),
            "{env} points at {}, which is not a file",
            path.display()
        );
        return path;
    }
    find_in_gradle_cache(group, artifact).unwrap_or_else(|| {
        panic!(
            "the {group}:{artifact} driver JAR was not found.\n\
             \n\
             A server URL is set, so this test was asked to run, and it needs a driver.\n\
             Fetch both drivers into the Gradle cache with:\n\
             \n    cd bridge && ./gradlew drivers\n\
             \n\
             or point {env} at a JAR you already have."
        )
    })
}

/// Walks `<gradle home>/caches/modules-2/files-2.1/<group>/<artifact>/*/*/<artifact>-<version>.jar`.
///
/// The same two-level walk `tests/h2.rs` does, with the coordinates as
/// arguments rather than baked in: the shape of the cache is fixed, and one
/// crate with three copies of it would be three places for the same mistake.
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
        // Only the binary artefact. The same directory also holds
        // `-javadoc.jar` and `-sources.jar`, and picking one of those gets a
        // class loader with no classes in it — a ClassNotFoundException a long
        // way from its cause.
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

// --- the two products ------------------------------------------------------

/// Which server a test wants.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Product {
    Postgres,
    MySql,
}

impl Product {
    /// The environment variable that both enables the product and says where
    /// it is.
    fn url_var(self) -> &'static str {
        match self {
            Product::Postgres => "RUDBMAN_TEST_PG_URL",
            Product::MySql => "RUDBMAN_TEST_MYSQL_URL",
        }
    }

    fn driver_class(self) -> &'static str {
        match self {
            Product::Postgres => "org.postgresql.Driver",
            Product::MySql => "com.mysql.cj.jdbc.Driver",
        }
    }

    fn jar(self) -> PathBuf {
        match self {
            Product::Postgres => driver_jar("RUDBMAN_TEST_PG_JAR", "org.postgresql", "postgresql"),
            Product::MySql => {
                driver_jar("RUDBMAN_TEST_MYSQL_JAR", "com.mysql", "mysql-connector-j")
            }
        }
    }

    /// The dialect `rudbman-sql` writes for this product.
    fn dialect(self) -> &'static Dialect {
        match self {
            Product::Postgres => &Dialect::POSTGRES,
            Product::MySql => &Dialect::MYSQL,
        }
    }

    /// `(user, password)`, from the environment or the compose file's defaults.
    fn credentials(self) -> (String, String) {
        let (user_var, password_var) = match self {
            Product::Postgres => ("RUDBMAN_TEST_PG_USER", "RUDBMAN_TEST_PG_PASSWORD"),
            Product::MySql => ("RUDBMAN_TEST_MYSQL_USER", "RUDBMAN_TEST_MYSQL_PASSWORD"),
        };
        (
            std::env::var(user_var).unwrap_or_else(|_| "rudbman".to_string()),
            std::env::var(password_var).unwrap_or_else(|_| "rudbman".to_string()),
        )
    }

    /// What the catalogue calls the type a user spells `bigint`.
    ///
    /// The two answers are `int8` and `bigint`, which is the whole reason
    /// `ddl` has no type model: the string a user types and the string the
    /// catalogue reports it back as are not even the same on one product.
    fn wider_int_catalog(self) -> &'static str {
        match self {
            Product::Postgres => "int8",
            Product::MySql => "bigint",
        }
    }
}

/// The type text a retyping test asks for. `bigint` happens to be spelled the
/// same on both products; what it comes back as does not.
const WIDER_INT: &str = "bigint";

/// A connected server, with the two names a `DescribeRequest` has to be
/// narrowed by.
///
/// PostgreSQL puts a table in a *schema* of a catalog; MySQL's JDBC driver
/// reports the database as the *catalog* and no schema at all. Every describe
/// below goes through [`Server::describe`] so that difference is written once.
struct Server {
    product: Product,
    session: Session,
    catalog: Option<String>,
    schema: Option<String>,
    /// Tables to drop when the test ends, in creation order.
    tables: RefCell<Vec<String>>,
}

impl Drop for Server {
    fn drop(&mut self) {
        // Reverse order, so a child that references a parent goes first. Every
        // drop is `IF EXISTS` and every failure is ignored: this runs on the
        // way out of a panicking test too, and a cleanup that panicked would
        // replace the real failure with its own.
        for table in self.tables.borrow().iter().rev() {
            let _ = self
                .session
                .execute(&StatementSpec::new(format!("DROP TABLE IF EXISTS {table}")));
        }
    }
}

impl Server {
    /// Opens the product's server, or answers `None` and says how to get one.
    fn open(product: Product) -> Option<Server> {
        let Ok(url) = std::env::var(product.url_var()) else {
            println!(
                "skipped: no {product:?} server. Start one with \
                 `docker compose -f docker/compose.yml up -d` and set {} \
                 (the URL is in that file's header).",
                product.url_var()
            );
            return None;
        };
        let session = connect(product, &url);

        // Asked of the server rather than parsed out of the URL: the URL may
        // leave the database to the driver's default, and a catalogue request
        // has to name the one the session really landed in.
        let (catalog, schema) = match product {
            Product::Postgres => (None, Some(scalar(&session, "select current_schema()"))),
            Product::MySql => (Some(scalar(&session, "select database()")), None),
        };
        Some(Server {
            product,
            session,
            catalog,
            schema,
            tables: RefCell::new(Vec::new()),
        })
    }

    fn dialect(&self) -> &'static Dialect {
        self.product.dialect()
    }

    /// Runs a statement that returns no rows.
    fn exec(&self, sql: &str) {
        self.session
            .execute(&StatementSpec::new(sql.to_string()))
            .unwrap_or_else(|error| panic!("{:?}: {sql}: {error}", self.product));
    }

    /// Runs a planned batch in order, the way the structure pane does: under
    /// autocommit, stopping at the first failure (§7.10).
    fn run(&self, batch: &[String]) {
        for (index, sql) in batch.iter().enumerate() {
            self.session
                .execute(&StatementSpec::new(sql.clone()))
                .unwrap_or_else(|error| {
                    panic!(
                        "{:?}: statement {index} of the batch was refused.\n{sql}\n{error}",
                        self.product
                    )
                });
        }
    }

    /// A name no other test and no other run of this one uses, registered for
    /// cleanup.
    fn table(&self, what: &str) -> String {
        static SEQ: AtomicU32 = AtomicU32::new(0);
        let name = format!(
            "rb_{what}_{}_{}",
            std::process::id(),
            SEQ.fetch_add(1, Ordering::Relaxed)
        );
        self.tables.borrow_mut().push(name.clone());
        name
    }

    /// Registers a name the test creates out of band — the target of a table
    /// rename, which the fixture would otherwise never hear about.
    fn also_drop(&self, name: &str) {
        self.tables.borrow_mut().push(name.to_string());
    }

    /// A describe request for `table`, narrowed the way this product needs.
    fn describe(&self, kind: &str, table: &str) -> Vec<serde_json::Map<String, serde_json::Value>> {
        let mut request = DescribeRequest::new(kind).with_table(table);
        if let Some(catalog) = &self.catalog {
            request = request.with_catalog(catalog);
        }
        if let Some(schema) = &self.schema {
            request = request.with_schema(schema);
        }
        self.session
            .describe(&request)
            .unwrap_or_else(|error| {
                panic!("{:?}: describe {kind} of {table}: {error}", self.product)
            })
            .items
    }

    /// One table's columns, as the catalogue holds them.
    fn columns(&self, table: &str) -> Vec<CatalogColumn> {
        self.describe("columns", table)
            .iter()
            .map(|item| CatalogColumn {
                name: string(item, "name"),
                type_name: string(item, "type_name").to_lowercase(),
                nullable: item.get("is_nullable").and_then(serde_json::Value::as_bool),
                default: item
                    .get("default")
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_string),
            })
            .collect()
    }

    /// The one column named `name`, or a panic naming what was there instead.
    fn column(&self, table: &str, name: &str) -> CatalogColumn {
        let columns = self.columns(table);
        columns
            .iter()
            .find(|column| column.name == name)
            .cloned()
            .unwrap_or_else(|| panic!("{table} has no column {name}: {columns:?}"))
    }

    /// Just the column names, in the order the catalogue lists them.
    fn column_names(&self, table: &str) -> Vec<String> {
        self.columns(table)
            .into_iter()
            .map(|column| column.name)
            .collect()
    }

    /// The primary key's columns, in key order.
    fn primary_key(&self, table: &str) -> Vec<String> {
        let mut rows: Vec<(i64, String)> = self
            .describe("primary_keys", table)
            .iter()
            .map(|item| {
                (
                    item.get("seq")
                        .and_then(serde_json::Value::as_i64)
                        .unwrap_or(0),
                    string(item, "column"),
                )
            })
            .collect();
        rows.sort();
        rows.into_iter().map(|(_, column)| column).collect()
    }

    /// The primary key's name as the server holds it, which is not always the
    /// name it was given.
    fn primary_key_name(&self, table: &str) -> Option<String> {
        self.describe("primary_keys", table)
            .first()
            .map(|item| string(item, "name"))
    }

    /// Every foreign key of `table`, as `(this column, referenced table,
    /// referenced column)`, sorted.
    fn references(&self, table: &str) -> Vec<(String, String, String)> {
        let mut rows: Vec<_> = self
            .describe("imported_keys", table)
            .iter()
            .map(|item| {
                (
                    string(item, "fk_column"),
                    string(item, "pk_table"),
                    string(item, "pk_column"),
                )
            })
            .collect();
        rows.sort();
        rows
    }

    /// Whether `table` carries a unique index called `name`.
    fn has_unique_index(&self, table: &str, name: &str) -> bool {
        self.describe("indexes", table).iter().any(|item| {
            item.get("name").and_then(serde_json::Value::as_str) == Some(name)
                && item.get("non_unique").and_then(serde_json::Value::as_bool) == Some(false)
        })
    }

    /// How many check constraints called `name` the server holds.
    ///
    /// The one thing here that JDBC cannot answer: `DatabaseMetaData` has no
    /// accessor for check constraints at all, which is also why §7.10 has the
    /// kind travel with the name rather than looking it up. Both products
    /// carry the SQL-standard `information_schema.check_constraints` — MySQL
    /// since 8.0.16, the same version that introduced the `DROP CHECK` this
    /// test is here to confirm.
    fn check_constraints(&self, name: &str) -> i64 {
        let sql = format!(
            "select count(*) from information_schema.check_constraints \
             where constraint_name = '{name}'"
        );
        scalar(&self.session, &sql)
            .parse()
            .expect("count(*) is a number")
    }
}

/// Opens one connection to `product` at `url`.
fn connect(product: Product, url: &str) -> Session {
    let (user, password) = product.credentials();
    let spec = ConnectionSpec::new(url.to_string(), product.driver_class())
        .with_credentials(user, password)
        .with_jars([product.jar()]);
    Session::open(jvm(), &spec).unwrap_or_else(|error| {
        panic!("{} is set but does not connect: {error}", product.url_var())
    })
}

/// A column as the catalogue reports it, which is the only reading these tests
/// trust.
#[derive(Debug, Clone, PartialEq, Eq)]
struct CatalogColumn {
    name: String,
    /// Lower-cased: PostgreSQL says `int4` and MySQL says `INT`, and the case
    /// is the driver's habit rather than anything worth asserting.
    type_name: String,
    /// `IS_NULLABLE`, as JDBC's tri-state. `None` is "the driver does not
    /// know", which neither of these two ever answers.
    nullable: Option<bool>,
    /// `COLUMN_DEF`, verbatim. The products disagree about how to spell one
    /// back — PostgreSQL answers `'x'::character varying` where MySQL answers
    /// `x` — so tests ask whether what they wrote survived rather than what it
    /// now looks like.
    default: Option<String>,
}

impl CatalogColumn {
    /// Whether the reported default mentions `wanted`.
    fn defaults_to(&self, wanted: &str) -> bool {
        self.default
            .as_deref()
            .is_some_and(|text| text.contains(wanted))
    }
}

/// A string field of a describe item, or a panic.
fn string(item: &serde_json::Map<String, serde_json::Value>, key: &str) -> String {
    item.get(key)
        .and_then(serde_json::Value::as_str)
        .unwrap_or_else(|| panic!("no `{key}` in {item:?}"))
        .to_string()
}

/// The first column of the first row of `sql`, as text.
fn scalar(session: &Session, sql: &str) -> String {
    let batch = fetch(session, sql);
    text(&batch, 0, 0).unwrap_or_else(|| panic!("{sql} answered NULL"))
}

/// Runs a query and reads one batch of up to 200 rows.
fn fetch(session: &Session, sql: &str) -> Batch {
    session
        .execute(&StatementSpec::new(sql.to_string()))
        .unwrap_or_else(|error| panic!("{sql}: {error}"))
        .fetch(200)
        .expect("the batch decodes")
}

/// One cell, as text.
fn text(batch: &Batch, row: usize, column: usize) -> Option<String> {
    match batch.value(row, column)? {
        Value::Null => None,
        Value::Str(text) => Some(text.to_string()),
        Value::I64(value) => Some(value.to_string()),
        other => Some(format!("{other:?}")),
    }
}

/// Opens the product's server, or returns from the test having said why not.
///
/// A macro rather than a function because the `return` has to happen in the
/// test body: a skipped test is one that passed by doing nothing, and that is
/// what keeps this file out of the way of a checkout with no Docker.
macro_rules! server {
    ($product:expr) => {
        match Server::open($product) {
            Some(server) => server,
            None => return,
        }
    };
}

// --- CREATE TABLE ----------------------------------------------------------

#[test]
fn postgres_creates_a_table_with_its_keys_and_references() {
    let server = server!(Product::Postgres);
    create_with_constraints(&server);
}

#[test]
fn mysql_creates_a_table_with_its_keys_and_references() {
    let server = server!(Product::MySql);
    create_with_constraints(&server);
}

/// `plan_create` with every clause it can write, read back out of the
/// catalogue.
///
/// The two foreign keys are the pair worth having: one names the referenced
/// columns and one leaves them out. An omitted list is how most products spell
/// "that table's own primary key" — PostgreSQL among them, MySQL not — so the
/// form the omitting key takes here is per product, and
/// [`a_foreign_key_with_no_referenced_columns_is_refused_before_mysql_sees_it`]
/// is where the difference is pinned down.
fn create_with_constraints(server: &Server) {
    let parent = server.table("parent");
    let child = server.table("child");

    let mut create = TableCreate::new([parent.as_str()]);
    create
        .columns
        .push(ColumnDef::new("id", "integer").with_not_null(true));
    create
        .columns
        .push(ColumnDef::new("code", "varchar(10)").with_not_null(true));
    create
        .constraints
        .push(TableConstraint::primary_key(["id"]).with_name(format!("{parent}_pk")));
    create
        .constraints
        .push(TableConstraint::unique(["code"]).with_name(format!("{parent}_uq")));
    server.run(&plan_create(&create, server.dialect()).expect("the parent plans"));

    let mut create = TableCreate::new([child.as_str()]);
    create
        .columns
        .push(ColumnDef::new("id", "integer").with_not_null(true));
    create.columns.push(ColumnDef::new("parent_id", "integer"));
    create
        .columns
        .push(ColumnDef::new("parent_code", "varchar(10)"));
    create
        .columns
        .push(ColumnDef::new("note", "varchar(80)").with_default("'x'"));
    create.columns.push(
        ColumnDef::new("qty", "integer")
            .with_not_null(true)
            .with_default("0"),
    );
    // Unnamed: the server chooses, and what it chooses is read back below.
    create
        .constraints
        .push(TableConstraint::primary_key(["id"]));
    create.constraints.push(
        match server.product {
            // PostgreSQL resolves the omitted list to the parent's own key,
            // which is the form that stays correct when that key is later
            // re-ordered.
            Product::Postgres => TableConstraint::foreign_key(["parent_id"], [parent.as_str()]),
            // MySQL resolves nothing, so `plan_create` refuses the shape for
            // it. The columns are spelled out here so that the rest of this
            // test is about what it is about.
            Product::MySql => {
                TableConstraint::foreign_key_to(["parent_id"], [parent.as_str()], ["id"])
            }
        }
        .with_name(format!("{child}_fk")),
    );
    create.constraints.push(TableConstraint::foreign_key_to(
        ["parent_code"],
        [parent.as_str()],
        ["code"],
    ));
    server.run(&plan_create(&create, server.dialect()).expect("the child plans"));

    // The columns arrived in the order they were written, with their
    // nullability and their defaults.
    assert_eq!(
        server.column_names(&child),
        ["id", "parent_id", "parent_code", "note", "qty"],
        "{:?}",
        server.product
    );
    let columns = server.columns(&child);
    assert_eq!(columns[0].nullable, Some(false), "id is NOT NULL");
    assert_eq!(columns[1].nullable, Some(true), "parent_id is not");
    assert!(columns[3].defaults_to("x"), "note: {:?}", columns[3]);
    assert_eq!(columns[4].nullable, Some(false), "qty is NOT NULL");
    assert!(columns[4].defaults_to("0"), "qty: {:?}", columns[4]);

    // Both primary keys are real, and the *name* is the interesting half.
    assert_eq!(server.primary_key(&parent), ["id"]);
    assert_eq!(server.primary_key(&child), ["id"]);
    match server.product {
        // PostgreSQL keeps the name it was given, and invents `<table>_pkey`
        // for the one that was left unnamed.
        Product::Postgres => {
            assert_eq!(
                server.primary_key_name(&parent).as_deref(),
                Some(format!("{parent}_pk").as_str())
            );
            assert_eq!(
                server.primary_key_name(&child).as_deref(),
                Some(format!("{child}_pkey").as_str())
            );
        }
        // MySQL does not: a primary key is always called `PRIMARY`, whatever
        // the `CONSTRAINT <name>` prefix asked for. Which is exactly why
        // `ConstraintKind::PrimaryKey` drops it by no name at all on MySQL —
        // there is no name to drop it by.
        Product::MySql => {
            assert_eq!(server.primary_key_name(&parent).as_deref(), Some("PRIMARY"));
            assert_eq!(server.primary_key_name(&child).as_deref(), Some("PRIMARY"));
        }
    }

    // The unique constraint is backed by a unique index of its own name on
    // both products, which is what makes MySQL's `DROP INDEX` spelling work.
    assert!(
        server.has_unique_index(&parent, &format!("{parent}_uq")),
        "no unique index for the UNIQUE constraint: {:?}",
        server.describe("indexes", &parent)
    );

    // The two references, one of which never named a referenced column.
    assert_eq!(
        server.references(&child),
        [
            (
                "parent_code".to_string(),
                parent.clone(),
                "code".to_string()
            ),
            ("parent_id".to_string(), parent.clone(), "id".to_string()),
        ],
        "on PostgreSQL, the omitted column list resolved to the parent's own key"
    );

    // And they are constraints rather than documentation.
    server.exec(&format!("insert into {parent} values (1, 'a')"));
    server.exec(&format!(
        "insert into {child} (id, parent_id, parent_code) values (1, 1, 'a')"
    ));
    server
        .session
        .execute(&StatementSpec::new(format!(
            "insert into {child} (id, parent_id) values (2, 999)"
        )))
        .expect_err("the foreign key refuses a parent that is not there");
}

#[test]
fn a_foreign_key_with_no_referenced_columns_is_refused_before_mysql_sees_it() {
    let server = server!(Product::MySql);

    // The rule this test guards was found here rather than in a manual. An
    // earlier version of it sent `FOREIGN KEY (parent_id) REFERENCES parent`
    // to this server and got
    //
    //     [SQLSTATE 42000, code 1239]: Incorrect foreign key definition for
    //     'rb_nofk_child_..._fk': Key reference and table reference don't match
    //
    // — a message that names neither the omission nor the fix, which is
    // exactly the case §7.10 says to refuse ahead of the server. `plan_create`
    // now does, so what is checked below is the refusal and its words; the
    // evidence behind the rule is the paragraph you are reading.
    let parent = server.table("nofk_parent");
    let child = server.table("nofk_child");

    let mut create = TableCreate::new([parent.as_str()]);
    create
        .columns
        .push(ColumnDef::new("id", "integer").with_not_null(true));
    create
        .constraints
        .push(TableConstraint::primary_key(["id"]));
    server.run(&plan_create(&create, server.dialect()).expect("plans"));

    let mut create = TableCreate::new([child.as_str()]);
    create
        .columns
        .push(ColumnDef::new("id", "integer").with_not_null(true));
    create.columns.push(ColumnDef::new("parent_id", "integer"));
    create
        .constraints
        .push(TableConstraint::primary_key(["id"]));
    create.constraints.push(TableConstraint::foreign_key(
        ["parent_id"],
        [parent.as_str()],
    ));

    let error = plan_create(&create, server.dialect())
        .expect_err("the generator refuses the shape rather than writing it");
    let message = error.to_string();
    assert!(message.contains("MySQL"), "{message}");
    assert!(
        message.contains("spelled out"),
        "the refusal has to say what to do about it: {message}"
    );

    // And the same shape with the column named is accepted by the server, so
    // what is being refused is the omission and nothing else about the
    // statement. This half is why the test needs a real MySQL at all.
    create.constraints.pop();
    create.constraints.push(TableConstraint::foreign_key_to(
        ["parent_id"],
        [parent.as_str()],
        ["id"],
    ));
    server.run(&plan_create(&create, server.dialect()).expect("plans"));
    assert_eq!(
        server.references(&child),
        [("parent_id".to_string(), parent.clone(), "id".to_string())]
    );
}

// --- adding, dropping and renaming a column --------------------------------

#[test]
fn postgres_adds_drops_and_renames_a_column() {
    let server = server!(Product::Postgres);
    add_drop_rename(&server);
}

#[test]
fn mysql_adds_drops_and_renames_a_column() {
    let server = server!(Product::MySql);
    add_drop_rename(&server);
}

/// One batch that adds, renames and drops, in the order §7.10 fixes.
fn add_drop_rename(server: &Server) {
    let t = server.table("cols");
    let mut create = TableCreate::new([t.as_str()]);
    create
        .columns
        .push(ColumnDef::new("id", "integer").with_not_null(true));
    create.columns.push(ColumnDef::new("gone", "varchar(10)"));
    create.columns.push(ColumnDef::new("old_name", "integer"));
    create
        .constraints
        .push(TableConstraint::primary_key(["id"]));
    server.run(&plan_create(&create, server.dialect()).expect("plans"));

    let mut alter = TableAlter::new([t.as_str()]);
    alter.adds.push(
        ColumnDef::new("added", "varchar(20)")
            .with_not_null(true)
            .with_default("'z'"),
    );
    alter.changes.push(ColumnChange {
        from: ColumnDef::new("old_name", "integer"),
        to: ColumnDef::new("new_name", "integer"),
    });
    alter.drops.push("gone".to_string());
    let batch = plan_alter(&alter, server.dialect()).expect("plans");

    // The spellings §7.10 claims, before they are sent. The add comes first,
    // the drop last, and the rename in between names the column the way the
    // catalogue still holds it.
    let expected = match server.product {
        Product::Postgres => [
            format!("ALTER TABLE {t} ADD COLUMN added varchar(20) NOT NULL DEFAULT 'z'"),
            format!("ALTER TABLE {t} RENAME COLUMN old_name TO new_name"),
            format!("ALTER TABLE {t} DROP COLUMN gone"),
        ],
        // `CHANGE COLUMN old new <definition>`, the form that works before
        // MySQL 8.0 as well as after — which is why it restates the type.
        Product::MySql => [
            format!("ALTER TABLE {t} ADD COLUMN added varchar(20) NOT NULL DEFAULT 'z'"),
            format!("ALTER TABLE {t} CHANGE COLUMN old_name new_name integer"),
            format!("ALTER TABLE {t} DROP COLUMN gone"),
        ],
    };
    assert_eq!(batch, expected, "{:?}", server.product);
    server.run(&batch);

    assert_eq!(
        server.column_names(&t),
        ["id", "new_name", "added"],
        "{:?}: the dropped column is gone, the renamed one kept its place, and \
         the added one went to the end however it was written",
        server.product
    );
    let added = server.column(&t, "added");
    assert_eq!(added.nullable, Some(false));
    assert!(added.defaults_to("z"), "{added:?}");
}

// --- type, nullability and default -----------------------------------------

#[test]
fn postgres_changes_a_type_a_nullability_and_a_default() {
    let server = server!(Product::Postgres);
    attribute_changes(&server);
}

#[test]
fn mysql_changes_a_type_a_nullability_and_a_default() {
    let server = server!(Product::MySql);
    attribute_changes(&server);
}

/// Every attribute change the generator can write, in one batch: nullability
/// both ways, a default set and a default dropped, and a type widened.
fn attribute_changes(server: &Server) {
    let t = server.table("attrs");
    let mut create = TableCreate::new([t.as_str()]);
    create
        .columns
        .push(ColumnDef::new("id", "integer").with_not_null(true));
    create
        .columns
        .push(ColumnDef::new("to_not_null", "integer"));
    create
        .columns
        .push(ColumnDef::new("to_nullable", "integer").with_not_null(true));
    create
        .columns
        .push(ColumnDef::new("gains_default", "varchar(10)"));
    create.columns.push(
        ColumnDef::new("loses_default", "integer")
            .with_not_null(true)
            .with_default("1"),
    );
    create.columns.push(ColumnDef::new("widens", "integer"));
    server.run(&plan_create(&create, server.dialect()).expect("plans"));

    let mut alter = TableAlter::new([t.as_str()]);
    alter.changes.push(ColumnChange {
        from: ColumnDef::new("to_not_null", "integer"),
        to: ColumnDef::new("to_not_null", "integer").with_not_null(true),
    });
    alter.changes.push(ColumnChange {
        from: ColumnDef::new("to_nullable", "integer").with_not_null(true),
        to: ColumnDef::new("to_nullable", "integer"),
    });
    alter.changes.push(ColumnChange {
        from: ColumnDef::new("gains_default", "varchar(10)"),
        to: ColumnDef::new("gains_default", "varchar(10)").with_default("'d'"),
    });
    alter.changes.push(ColumnChange {
        from: ColumnDef::new("loses_default", "integer")
            .with_not_null(true)
            .with_default("1"),
        to: ColumnDef::new("loses_default", "integer").with_not_null(true),
    });
    alter.changes.push(ColumnChange {
        from: ColumnDef::new("widens", "integer"),
        to: ColumnDef::new("widens", WIDER_INT),
    });
    let batch = plan_alter(&alter, server.dialect()).expect("plans");

    let expected = match server.product {
        // The standard family: an independent clause per attribute.
        Product::Postgres => vec![
            format!("ALTER TABLE {t} ALTER COLUMN to_not_null SET NOT NULL"),
            format!("ALTER TABLE {t} ALTER COLUMN to_nullable DROP NOT NULL"),
            format!("ALTER TABLE {t} ALTER COLUMN gains_default SET DEFAULT 'd'"),
            format!("ALTER TABLE {t} ALTER COLUMN loses_default DROP DEFAULT"),
            format!("ALTER TABLE {t} ALTER COLUMN widens SET DATA TYPE bigint"),
        ],
        // MySQL restates — except for a default on its own, which `ALTER
        // COLUMN` changes as metadata where `MODIFY` would rewrite the table.
        // Both spellings are sent here, which is the point of listing them.
        Product::MySql => vec![
            format!("ALTER TABLE {t} MODIFY COLUMN to_not_null integer NOT NULL"),
            format!("ALTER TABLE {t} MODIFY COLUMN to_nullable integer"),
            format!("ALTER TABLE {t} ALTER COLUMN gains_default SET DEFAULT 'd'"),
            format!("ALTER TABLE {t} ALTER COLUMN loses_default DROP DEFAULT"),
            format!("ALTER TABLE {t} MODIFY COLUMN widens bigint"),
        ],
    };
    assert_eq!(batch, expected, "{:?}", server.product);
    server.run(&batch);

    assert_eq!(
        server.column(&t, "to_not_null").nullable,
        Some(false),
        "{:?}",
        server.product
    );
    assert_eq!(
        server.column(&t, "to_nullable").nullable,
        Some(true),
        "{:?}",
        server.product
    );
    let gains = server.column(&t, "gains_default");
    assert!(gains.defaults_to("d"), "{gains:?}");
    assert_eq!(
        server.column(&t, "loses_default").default,
        None,
        "the default was dropped, not set to something"
    );
    assert_eq!(
        server.column(&t, "widens").type_name,
        server.product.wider_int_catalog()
    );
}

// --- the claim §7.10 leans on hardest --------------------------------------

#[test]
fn mysql_modify_column_restates_the_whole_definition() {
    let server = server!(Product::MySql);

    // Two tables of identical shape. The first gets the statement the
    // generator writes; the second gets the statement a generator that only
    // named the new type would have written. The pair is the test: one of them
    // proves the rule is kept, and the other proves the rule is needed.
    let kept = server.table("restate_kept");
    let lost = server.table("restate_lost");
    for table in [&kept, &lost] {
        let mut create = TableCreate::new([table.as_str()]);
        create
            .columns
            .push(ColumnDef::new("id", "integer").with_not_null(true));
        create.columns.push(
            ColumnDef::new("qty", "integer")
                .with_not_null(true)
                .with_default("7"),
        );
        server.run(&plan_create(&create, server.dialect()).expect("plans"));
    }

    // Only the type changes. Nothing in the diff mentions nullability or the
    // default.
    let mut alter = TableAlter::new([kept.as_str()]);
    alter.changes.push(ColumnChange {
        from: ColumnDef::new("qty", "integer")
            .with_not_null(true)
            .with_default("7"),
        to: ColumnDef::new("qty", WIDER_INT)
            .with_not_null(true)
            .with_default("7"),
    });
    let batch = plan_alter(&alter, server.dialect()).expect("plans");
    assert_eq!(
        batch,
        [format!(
            "ALTER TABLE {kept} MODIFY COLUMN qty bigint NOT NULL DEFAULT 7"
        )],
        "the whole definition, restated, from a diff whose only difference is \
         the type"
    );
    server.run(&batch);

    let qty = server.column(&kept, "qty");
    assert_eq!(qty.type_name, "bigint", "{qty:?}");
    assert_eq!(
        qty.nullable,
        Some(false),
        "the restated NOT NULL survived the retype: {qty:?}"
    );
    assert!(
        qty.defaults_to("7"),
        "the restated default survived the retype: {qty:?}"
    );

    // And now the counterfactual, sent by hand: exactly what MySQL does with a
    // `MODIFY COLUMN` that names only the type. It is accepted — there is no
    // error to notice — and it silently takes both attributes away.
    server.exec(&format!("ALTER TABLE {lost} MODIFY COLUMN qty bigint"));
    let qty = server.column(&lost, "qty");
    assert_eq!(qty.type_name, "bigint");
    assert_eq!(
        qty.nullable,
        Some(true),
        "MySQL dropped the NOT NULL, exactly as §7.10 warns: {qty:?}"
    );
    assert_eq!(
        qty.default, None,
        "and the default with it, without a word: {qty:?}"
    );
}

#[test]
fn postgres_set_data_type_leaves_nullability_and_default_alone() {
    let server = server!(Product::Postgres);

    let t = server.table("independent");
    let mut create = TableCreate::new([t.as_str()]);
    create
        .columns
        .push(ColumnDef::new("id", "integer").with_not_null(true));
    create.columns.push(
        ColumnDef::new("qty", "integer")
            .with_not_null(true)
            .with_default("7"),
    );
    server.run(&plan_create(&create, server.dialect()).expect("plans"));

    let mut alter = TableAlter::new([t.as_str()]);
    alter.changes.push(ColumnChange {
        from: ColumnDef::new("qty", "integer")
            .with_not_null(true)
            .with_default("7"),
        to: ColumnDef::new("qty", WIDER_INT)
            .with_not_null(true)
            .with_default("7"),
    });
    let batch = plan_alter(&alter, server.dialect()).expect("plans");
    // The mirror of MySQL's restating: one clause, naming one attribute, and
    // nothing said about the other two.
    assert_eq!(
        batch,
        [format!(
            "ALTER TABLE {t} ALTER COLUMN qty SET DATA TYPE bigint"
        )]
    );
    server.run(&batch);

    let qty = server.column(&t, "qty");
    assert_eq!(qty.type_name, "int8", "{qty:?}");
    assert_eq!(
        qty.nullable,
        Some(false),
        "PostgreSQL left the NOT NULL where it was: {qty:?}"
    );
    assert!(qty.defaults_to("7"), "and the default: {qty:?}");
}

// --- dropping constraints --------------------------------------------------

#[test]
fn postgres_drops_a_constraint_of_every_kind() {
    let server = server!(Product::Postgres);
    drop_every_constraint(&server);
}

#[test]
fn mysql_drops_a_constraint_of_every_kind() {
    let server = server!(Product::MySql);
    drop_every_constraint(&server);
}

/// The four kinds, dropped in one batch.
///
/// This is the test MySQL was in scope for. §7.10 claims that everywhere but
/// MySQL a drop is `DROP CONSTRAINT <name>`, and that MySQL has no generic
/// form and spells each kind separately — `DROP PRIMARY KEY` with no name at
/// all, `DROP FOREIGN KEY`, `DROP INDEX` for a unique, `DROP CHECK`. Four
/// different statements where one would do, none of which a server had ever
/// confirmed.
fn drop_every_constraint(server: &Server) {
    let parent = server.table("dc_parent");
    let child = server.table("dc_child");

    let mut create = TableCreate::new([parent.as_str()]);
    create
        .columns
        .push(ColumnDef::new("id", "integer").with_not_null(true));
    create
        .constraints
        .push(TableConstraint::primary_key(["id"]));
    server.run(&plan_create(&create, server.dialect()).expect("plans"));

    let mut create = TableCreate::new([child.as_str()]);
    create
        .columns
        .push(ColumnDef::new("id", "integer").with_not_null(true));
    create.columns.push(ColumnDef::new("parent_id", "integer"));
    create.columns.push(ColumnDef::new("code", "varchar(10)"));
    create
        .columns
        .push(ColumnDef::new("qty", "integer").with_not_null(true));
    create
        .constraints
        .push(TableConstraint::primary_key(["id"]).with_name(format!("{child}_pk")));
    create
        .constraints
        .push(TableConstraint::unique(["code"]).with_name(format!("{child}_uq")));
    create.constraints.push(
        // The referenced column is named because MySQL insists on it; this
        // test is about the drop, not about the omitted list.
        TableConstraint::foreign_key_to(["parent_id"], [parent.as_str()], ["id"])
            .with_name(format!("{child}_fk")),
    );
    server.run(&plan_create(&create, server.dialect()).expect("plans"));

    // `plan_alter` drops constraints and never adds one (§7.10), so the check
    // constraint is put there by hand. It is the fixture, not the thing under
    // test — what is under test is the statement that takes it away.
    server.exec(&format!(
        "ALTER TABLE {child} ADD CONSTRAINT {child}_ck CHECK (qty >= 0)"
    ));

    assert_eq!(server.primary_key(&child), ["id"]);
    assert_eq!(server.references(&child).len(), 1);
    assert!(server.has_unique_index(&child, &format!("{child}_uq")));
    assert_eq!(server.check_constraints(&format!("{child}_ck")), 1);

    let mut alter = TableAlter::new([child.as_str()]);
    for (kind, suffix) in [
        // The foreign key goes first: on MySQL its index would otherwise be in
        // the way, and on both products a constraint naming a column is what
        // §7.10 puts ahead of everything else for.
        (ConstraintKind::ForeignKey, "_fk"),
        (ConstraintKind::PrimaryKey, "_pk"),
        (ConstraintKind::Unique, "_uq"),
        (ConstraintKind::Check, "_ck"),
    ] {
        alter.drop_constraints.push(ConstraintDrop {
            kind,
            name: format!("{child}{suffix}"),
        });
    }
    let batch = plan_alter(&alter, server.dialect()).expect("plans");

    let expected = match server.product {
        // One spelling, whatever the kind — the kind is carried only because
        // MySQL needs it.
        Product::Postgres => vec![
            format!("ALTER TABLE {child} DROP CONSTRAINT {child}_fk"),
            format!("ALTER TABLE {child} DROP CONSTRAINT {child}_pk"),
            format!("ALTER TABLE {child} DROP CONSTRAINT {child}_uq"),
            format!("ALTER TABLE {child} DROP CONSTRAINT {child}_ck"),
        ],
        // Four. Note that the primary key is dropped by no name at all, which
        // is just as well: MySQL called it `PRIMARY` rather than what the
        // `CREATE` asked for.
        Product::MySql => vec![
            format!("ALTER TABLE {child} DROP FOREIGN KEY {child}_fk"),
            format!("ALTER TABLE {child} DROP PRIMARY KEY"),
            format!("ALTER TABLE {child} DROP INDEX {child}_uq"),
            format!("ALTER TABLE {child} DROP CHECK {child}_ck"),
        ],
    };
    assert_eq!(batch, expected, "{:?}", server.product);
    server.run(&batch);

    assert!(
        server.primary_key(&child).is_empty(),
        "{:?}: the primary key is still there",
        server.product
    );
    assert!(
        server.references(&child).is_empty(),
        "{:?}: the foreign key is still there",
        server.product
    );
    assert!(
        !server.has_unique_index(&child, &format!("{child}_uq")),
        "{:?}: the unique index is still there: {:?}",
        server.product,
        server.describe("indexes", &child)
    );
    assert_eq!(
        server.check_constraints(&format!("{child}_ck")),
        0,
        "{:?}: the check constraint is still there",
        server.product
    );
}

// --- renaming the table ----------------------------------------------------

#[test]
fn postgres_renames_a_table() {
    let server = server!(Product::Postgres);
    rename_table(&server);
}

#[test]
fn mysql_renames_a_table() {
    let server = server!(Product::MySql);
    rename_table(&server);
}

/// `ALTER TABLE t RENAME TO n` — the same statement on both, and the one
/// §7.10 puts last in a batch so that everything before it names the table the
/// way the catalogue still holds it.
fn rename_table(server: &Server) {
    let before = server.table("rename");
    let after = format!("{before}_renamed");
    server.also_drop(&after);

    let mut create = TableCreate::new([before.as_str()]);
    create
        .columns
        .push(ColumnDef::new("id", "integer").with_not_null(true));
    create.columns.push(ColumnDef::new("note", "varchar(30)"));
    create
        .constraints
        .push(TableConstraint::primary_key(["id"]));
    server.run(&plan_create(&create, server.dialect()).expect("plans"));

    let mut alter = TableAlter::new([before.as_str()]);
    alter.rename_to = Some(after.clone());
    let batch = plan_alter(&alter, server.dialect()).expect("plans");
    assert_eq!(batch, [format!("ALTER TABLE {before} RENAME TO {after}")]);
    server.run(&batch);

    assert!(
        server.columns(&before).is_empty(),
        "{:?}: the old name still answers",
        server.product
    );
    assert_eq!(server.column_names(&after), ["id", "note"]);
    assert_eq!(
        server.primary_key(&after),
        ["id"],
        "the key came with the table"
    );
}

// --- what the drivers really say about a result's source table -------------
//
// §7.9's "Editing a query result" leans on `ColumnInfo`'s `table`, `schema` and
// `catalog` — the gate offers editing only where every column that names a
// source table names the same one. The document is careful to call the
// metadata a hint, and says two things about it that were until now unverified:
// that several drivers answer `""` for schema and catalog, and that MySQL "can
// report an alias where the table was asked for".
//
// Both tests below assert what these two drivers actually answered, for a
// plain column, an aliased column, a computed column and a column of an
// aliased table. The findings, in short:
//
//   * **Schema and catalog are all but useless.** pgjdbc 42.7.4 answers `""`
//     for *both*, on a plain column of an ordinary table in `public`.
//     Connector/J 9.1.0 answers `""` for schema and the database name for
//     catalog. So the gate can never lean on schema, and only MySQL offers a
//     catalog.
//   * **MySQL did not report an alias for the table**, on Connector/J 9.1.0
//     with its defaults: `select b.id from t b` reports `t`. The alias comes
//     back only with the legacy `useOldAliasMetadataBehavior=true`, which the
//     second half of the MySQL test sets on a connection of its own to show
//     that the document's "can" is a real behaviour and not the default one.
//   * **pgjdbc reports the column *alias* as the column name.** `getColumnName`
//     and `getColumnLabel` both answer `label` for `note AS label`, where
//     Connector/J distinguishes them. Nothing in §7.9 says otherwise, but it
//     bears on the gate: a result that aliases a key column looks, on
//     PostgreSQL, like a result that does not carry that key column — and the
//     gate's answer to that is to stay read-only, which is the safe direction.

/// The `(name, label, table, schema, catalog)` of every column of `sql`.
fn column_sources(session: &Session, sql: &str) -> Vec<(String, String, String, String, String)> {
    let cursor = session
        .execute(&StatementSpec::new(sql.to_string()))
        .unwrap_or_else(|error| panic!("{sql}: {error}"));
    cursor
        .columns()
        .iter()
        .map(|column| {
            let field = |value: &Option<String>| value.clone().unwrap_or_default();
            (
                field(&column.name),
                field(&column.label),
                field(&column.table),
                field(&column.schema),
                field(&column.catalog),
            )
        })
        .collect()
}

/// A table with two columns and a key, for the metadata tests.
fn metadata_fixture(server: &Server, what: &str) -> String {
    let t = server.table(what);
    let mut create = TableCreate::new([t.as_str()]);
    create
        .columns
        .push(ColumnDef::new("id", "integer").with_not_null(true));
    create.columns.push(ColumnDef::new("note", "varchar(30)"));
    create
        .constraints
        .push(TableConstraint::primary_key(["id"]));
    server.run(&plan_create(&create, server.dialect()).expect("plans"));
    t
}

#[test]
fn postgres_reports_no_schema_and_no_catalog_for_a_result_column() {
    let server = server!(Product::Postgres);
    let t = metadata_fixture(&server, "meta");

    let sources = column_sources(
        &server.session,
        &format!("select id, note as label, id + 1 as computed from {t}"),
    );
    assert_eq!(
        sources,
        [
            // A plain column: the source table is there, and nothing else is.
            (
                "id".into(),
                "id".into(),
                t.clone(),
                String::new(),
                String::new()
            ),
            // An aliased column. pgjdbc answers the *alias* for the column
            // name as well as for the label — it is `getBaseColumnName` that
            // does the lookup, and JDBC's `getColumnName` is not it.
            (
                "label".into(),
                "label".into(),
                t.clone(),
                String::new(),
                String::new()
            ),
            // A computed column names no table, which is the `""` §7.9 filters
            // for first and treats as "unknown" alongside null.
            (
                "computed".into(),
                "computed".into(),
                String::new(),
                String::new(),
                String::new()
            ),
        ],
        "pgjdbc's answers changed"
    );

    // An aliased *table* still reports the real table. The alias is nowhere in
    // the metadata.
    let sources = column_sources(&server.session, &format!("select b.id from {t} b"));
    assert_eq!(
        sources,
        [(
            "id".into(),
            "id".into(),
            t.clone(),
            String::new(),
            String::new()
        )],
        "pgjdbc reported the alias where the table was asked for"
    );
}

#[test]
fn mysql_reports_a_catalog_but_no_schema_for_a_result_column() {
    let server = server!(Product::MySql);
    let t = metadata_fixture(&server, "meta");
    let database = server.catalog.clone().expect("MySQL reports a catalog");

    let sources = column_sources(
        &server.session,
        &format!("select id, note as label, id + 1 as computed from {t}"),
    );
    assert_eq!(
        sources,
        [
            // The catalog is the database. The schema is `""` — Connector/J
            // treats the database as a catalog and has no schema to report.
            (
                "id".into(),
                "id".into(),
                t.clone(),
                String::new(),
                database.clone()
            ),
            // Connector/J keeps the name and the label apart, where pgjdbc
            // does not.
            (
                "note".into(),
                "label".into(),
                t.clone(),
                String::new(),
                database.clone()
            ),
            // A computed column: nothing at all, catalog included.
            (
                "computed".into(),
                "computed".into(),
                String::new(),
                String::new(),
                String::new()
            ),
        ],
        "Connector/J's answers changed"
    );

    // The claim in §7.9 — that MySQL "can report an alias where the table was
    // asked for" — is about a connection property rather than about MySQL.
    // With the defaults, an aliased table reports the real table:
    let aliased = format!("select b.id from {t} b");
    let sources = column_sources(&server.session, &aliased);
    assert_eq!(
        sources,
        [(
            "id".into(),
            "id".into(),
            t.clone(),
            String::new(),
            database.clone()
        )],
        "Connector/J 9.1.0 reports the real table by default"
    );

    // ...and with `useOldAliasMetadataBehavior=true`, the 5.x behaviour the
    // document is describing, it really does answer the alias. Which is why
    // §7.9 makes the metadata a hint that can only ever *offer* editing: a
    // connection property nobody in this codebase sets can change what the
    // gate is looking at.
    let url = std::env::var(Product::MySql.url_var()).expect("the test would have returned");
    let separator = if url.contains('?') { '&' } else { '?' };
    let legacy = connect(
        Product::MySql,
        &format!("{url}{separator}useOldAliasMetadataBehavior=true"),
    );
    let sources = column_sources(&legacy, &aliased);
    assert_eq!(
        sources[0].2, "b",
        "with the legacy property, the alias is what comes back"
    );
}
