//! Opt-in end-to-end tests against real PostgreSQL, MySQL, MariaDB, SQL Server
//! and Oracle servers.
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
//! Five of the six products are reachable from here, which is to say all of
//! them but SQLite, whose `ALTER` the generator refuses outright and whose
//! refusals the unit tests already fix. So all four of §7.10's attribute-change
//! families are settled against a server: the standard independent clauses
//! (PostgreSQL), MySQL's restating (MySQL and MariaDB), Oracle's
//! `MODIFY (...)` of only what changed, and SQL Server's `ALTER COLUMN`
//! carrying type and nullability together. So are the two constraint-drop
//! styles, the four column-rename spellings, and both table renames.
//!
//! # Opt-in, and silent when it is out
//!
//! Every test here passes by doing nothing when its server's URL is unset, and
//! says so in one line. That is deliberately *not* what `h2_jar()` does: H2 is
//! a dependency the repository guarantees, so a missing H2 is a broken
//! checkout and a panic; a database server is a container the developer chose
//! to start, and CI has none. `cargo test --workspace` must stay green on a
//! machine with no Docker — and a developer who started one container out of
//! five gets that container's tests and four skips.
//!
//! ```text
//! docker compose -f docker/compose.yml up -d      # then wait for "healthy"
//! export RUDBMAN_TEST_PG_URL='jdbc:postgresql://127.0.0.1:55432/rudbman'
//! export RUDBMAN_TEST_MYSQL_URL='jdbc:mysql://127.0.0.1:33306/rudbman?allowPublicKeyRetrieval=true&useSSL=false'
//! export RUDBMAN_TEST_MARIADB_URL='jdbc:mariadb://127.0.0.1:53306/rudbman'
//! export RUDBMAN_TEST_MSSQL_URL='jdbc:sqlserver://127.0.0.1:51433;encrypt=false'
//! export RUDBMAN_TEST_ORACLE_URL='jdbc:oracle:thin:@//127.0.0.1:51521/FREEPDB1'
//! cargo test -p rudbman-jdbc --test containers
//! ```
//!
//! The user and password default to `rudbman`/`rudbman`, which is what
//! `docker/compose.yml` sets up for four of the five; `RUDBMAN_TEST_PG_USER`,
//! `RUDBMAN_TEST_PG_PASSWORD` and their `MYSQL`, `MARIADB`, `MSSQL` and
//! `ORACLE` counterparts override them. SQL Server is the exception, and not by
//! choice: its `sa` password has a complexity rule that `rudbman` fails, so the
//! default there is `sa`/`Rudbman!Passw0rd` and the URL names no database at
//! all — the image ships none but `master`, and every table here is named
//! uniquely enough to live in it.
//!
//! # The driver JARs
//!
//! Found the way `tests/h2.rs` finds H2's, and for the same reason — the
//! Gradle cache is already the one place in this checkout where a driver
//! lives. What fills it is `cd bridge && ./gradlew drivers`, a task whose only
//! job is to resolve the five drivers; they are *not* `testImplementation`,
//! because no Java test in this project loads any of them.
//! `RUDBMAN_TEST_PG_JAR` and its four counterparts override the search.
//!
//! A missing JAR **is** a panic, unlike a missing server: by the time one is
//! looked for the developer has already set a URL and asked for the test to
//! run, and a test that passes because it could not find its driver is the
//! thing this file exists to avoid.
//!
//! # Case folding, and why Oracle's tables are shouted at
//!
//! [`Dialect::quote_ident`](rudbman_sql::Dialect::quote_ident) quotes a name
//! the server would otherwise change, so on Oracle a lower-case `note` is
//! emitted as `"note"` and stored in that case. That is the right behaviour and
//! these tests keep it — every **column** below is named in lower case on every
//! product, so Oracle's generated DDL really does go out quoted and the
//! catalogue really is read back at `note` rather than `NOTE`.
//!
//! The **table** names are the exception: [`Server::table`] shouts them on
//! Oracle. A table name is the one identifier this file also has to write into
//! raw SQL of its own — the `insert` that proves a foreign key bites, the
//! `DROP TABLE IF EXISTS` that cleans up — and hand-written SQL is unquoted, so
//! a lower-case table would be looked up folded and never found. Shouting it
//! makes the two spellings agree without any quoting on either side, and takes
//! the constraint names with it, since every one of them is built from the
//! table's. The few places that do have to name a *column* in raw SQL go
//! through [`Server::col`], which quotes it on Oracle and nowhere else.
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

// --- the five products -----------------------------------------------------

/// Which server a test wants.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Product {
    Postgres,
    MySql,
    /// MariaDB, which `rudbman-sql` wrote the MySQL dialect for until this
    /// file asked a server and got a syntax error back. It has a dialect of
    /// its own now, and the tests below are what keeps the two rows honest
    /// about which spellings they still share.
    MariaDb,
    MsSql,
    Oracle,
}

impl Product {
    /// The environment variable that both enables the product and says where
    /// it is.
    fn url_var(self) -> &'static str {
        match self {
            Product::Postgres => "RUDBMAN_TEST_PG_URL",
            Product::MySql => "RUDBMAN_TEST_MYSQL_URL",
            Product::MariaDb => "RUDBMAN_TEST_MARIADB_URL",
            Product::MsSql => "RUDBMAN_TEST_MSSQL_URL",
            Product::Oracle => "RUDBMAN_TEST_ORACLE_URL",
        }
    }

    fn driver_class(self) -> &'static str {
        match self {
            Product::Postgres => "org.postgresql.Driver",
            Product::MySql => "com.mysql.cj.jdbc.Driver",
            Product::MariaDb => "org.mariadb.jdbc.Driver",
            Product::MsSql => "com.microsoft.sqlserver.jdbc.SQLServerDriver",
            Product::Oracle => "oracle.jdbc.OracleDriver",
        }
    }

    fn jar(self) -> PathBuf {
        match self {
            Product::Postgres => driver_jar("RUDBMAN_TEST_PG_JAR", "org.postgresql", "postgresql"),
            Product::MySql => {
                driver_jar("RUDBMAN_TEST_MYSQL_JAR", "com.mysql", "mysql-connector-j")
            }
            Product::MariaDb => driver_jar(
                "RUDBMAN_TEST_MARIADB_JAR",
                "org.mariadb.jdbc",
                "mariadb-java-client",
            ),
            Product::MsSql => driver_jar(
                "RUDBMAN_TEST_MSSQL_JAR",
                "com.microsoft.sqlserver",
                "mssql-jdbc",
            ),
            Product::Oracle => driver_jar(
                "RUDBMAN_TEST_ORACLE_JAR",
                "com.oracle.database.jdbc",
                "ojdbc11",
            ),
        }
    }

    /// The dialect `rudbman-sql` writes for this product.
    fn dialect(self) -> &'static Dialect {
        match self {
            Product::Postgres => &Dialect::POSTGRES,
            // Two rows for two servers. They were one until the check-drop
            // test below sent MySQL's spelling to MariaDB and got 1064; what
            // separates them now is that one word, and every other assertion
            // in this file still expects the two to answer alike.
            Product::MySql => &Dialect::MYSQL,
            Product::MariaDb => &Dialect::MARIADB,
            Product::MsSql => &Dialect::MSSQL,
            Product::Oracle => &Dialect::ORACLE,
        }
    }

    /// Whether this product folds an unquoted identifier to upper case, and so
    /// wants its table names shouted — see the module documentation.
    fn folds_upper(self) -> bool {
        self == Product::Oracle
    }

    /// `(user, password)`, from the environment or the compose file's defaults.
    fn credentials(self) -> (String, String) {
        let (user_var, password_var) = match self {
            Product::Postgres => ("RUDBMAN_TEST_PG_USER", "RUDBMAN_TEST_PG_PASSWORD"),
            Product::MySql => ("RUDBMAN_TEST_MYSQL_USER", "RUDBMAN_TEST_MYSQL_PASSWORD"),
            Product::MariaDb => ("RUDBMAN_TEST_MARIADB_USER", "RUDBMAN_TEST_MARIADB_PASSWORD"),
            Product::MsSql => ("RUDBMAN_TEST_MSSQL_USER", "RUDBMAN_TEST_MSSQL_PASSWORD"),
            Product::Oracle => ("RUDBMAN_TEST_ORACLE_USER", "RUDBMAN_TEST_ORACLE_PASSWORD"),
        };
        // SQL Server's `sa` password has a complexity rule `rudbman` fails, so
        // that one image gets a login of its own; `docker/compose.yml` says the
        // same thing in its header.
        let (user, password) = match self {
            Product::MsSql => ("sa", "Rudbman!Passw0rd"),
            _ => ("rudbman", "rudbman"),
        };
        (
            std::env::var(user_var).unwrap_or_else(|_| user.to_string()),
            std::env::var(password_var).unwrap_or_else(|_| password.to_string()),
        )
    }

    /// The type a retyping test changes a column *to*, and what the catalogue
    /// calls it afterwards.
    ///
    /// Four products take `bigint` and answer three different names for it,
    /// which is the whole reason `ddl` has no type model: the string a user
    /// types and the string the catalogue reports it back as are not even the
    /// same on one product.
    ///
    /// Oracle has no `bigint` at all, and the obvious substitute is worse than
    /// no test: `integer` and `number(19)` are both `NUMBER` to
    /// `getColumns`, so a retype between them would be a green assertion about
    /// a statement that could have done nothing. The retype there is to
    /// `varchar(30)` — a change the catalogue cannot fail to show — and the
    /// answer is `VARCHAR2`, Oracle's own name for the type it was asked for by
    /// a synonym.
    fn widened(self) -> (&'static str, &'static str) {
        match self {
            Product::Postgres => ("bigint", "int8"),
            Product::MySql | Product::MariaDb | Product::MsSql => ("bigint", "bigint"),
            Product::Oracle => ("varchar(30)", "varchar2"),
        }
    }
}

/// The type text the MySQL-family retyping tests ask for. `bigint` happens to
/// be spelled the same on every product that has it; what it comes back as
/// does not.
const WIDER_INT: &str = "bigint";

/// A connected server, with the two names a `DescribeRequest` has to be
/// narrowed by.
///
/// The products disagree about which of the two a database even is. PostgreSQL
/// and SQL Server put a table in a *schema* of a catalog; MySQL's and
/// MariaDB's drivers report the database as the *catalog* and no schema at all;
/// Oracle has no catalog and calls the owning user the schema. Every describe
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
            Product::MySql | Product::MariaDb => {
                (Some(scalar(&session, "select database()")), None)
            }
            // The one product here that has both, and the only URL that names
            // no database: `master` is where a session with nothing else asked
            // for lands, and `db_name()` is how it says so.
            Product::MsSql => (
                Some(scalar(&session, "select db_name()")),
                Some(scalar(&session, "select schema_name()")),
            ),
            // Oracle's `getTables` takes no catalog at all, and the schema is
            // the connected user's — asked of the session rather than assumed
            // from the credentials, since `ALTER SESSION` can move it.
            Product::Oracle => (
                None,
                Some(scalar(
                    &session,
                    "select sys_context('userenv', 'current_schema') from dual",
                )),
            ),
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
    ///
    /// Shouted on a product that folds unquoted names up, so that the generated
    /// DDL and this file's own hand-written SQL spell the table the same way
    /// without either of them quoting it — see the module documentation.
    fn table(&self, what: &str) -> String {
        static SEQ: AtomicU32 = AtomicU32::new(0);
        let name = self.name(&format!(
            "rb_{what}_{}_{}",
            std::process::id(),
            SEQ.fetch_add(1, Ordering::Relaxed)
        ));
        self.tables.borrow_mut().push(name.clone());
        name
    }

    /// A table-shaped name in the case this product's catalogue will hold it,
    /// for the one name a test derives rather than asks for: a rename's target.
    fn name(&self, name: &str) -> String {
        if self.product.folds_upper() {
            name.to_uppercase()
        } else {
            name.to_string()
        }
    }

    /// A column named inside SQL this file wrote by hand, rather than inside
    /// SQL the generator wrote.
    ///
    /// Columns keep their lower-case names on every product, so on Oracle the
    /// generator quotes them and the catalogue holds them lower-cased. An
    /// unquoted `qty` in a hand-written `CHECK` would then be looked up as
    /// `QTY` and not found; this is the one place that has to say so.
    fn col(&self, name: &str) -> String {
        if self.product.folds_upper() {
            format!("\"{name}\"")
        } else {
            name.to_string()
        }
    }

    /// A constraint name built from `table`'s, in the case this product's
    /// catalogue will hold it.
    ///
    /// The suffix has to be shouted along with the table on Oracle, and not for
    /// tidiness: a mixed-case `RB_CHILD_1_2_ck` would be quoted by the
    /// generator and stored mixed, while the same name in the hand-written
    /// `ADD CONSTRAINT` that puts the check constraint there would be folded to
    /// `..._CK`, and the drop would then be looking for a constraint nobody
    /// created. Two spellings of one name is exactly the failure
    /// [`Dialect::quote_ident`](rudbman_sql::Dialect::quote_ident) exists to
    /// prevent, reintroduced by a test that quoted half its names.
    fn constraint(&self, table: &str, suffix: &str) -> String {
        self.name(&format!("{table}{suffix}"))
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
    ///
    /// All five products answer this through `DescribeRequest`. Oracle used to
    /// need a detour around it — see
    /// [`oracle_describe_columns_survives_a_column_default`] for the bug that
    /// forced that and the fix that retired it.
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
    /// kind travel with the name rather than looking it up. Four of the five
    /// products carry the SQL-standard `information_schema.check_constraints` —
    /// MySQL since 8.0.16 and MariaDB since 10.2, the versions that introduced
    /// the `DROP CHECK` these tests are here to confirm. Oracle has no
    /// `information_schema` at all and answers from `user_constraints`, where a
    /// check is `constraint_type = 'C'`.
    fn check_constraints(&self, name: &str) -> i64 {
        let sql = match self.product {
            Product::Oracle => format!(
                "select count(*) from user_constraints \
                 where constraint_type = 'C' and constraint_name = '{name}'"
            ),
            _ => format!(
                "select count(*) from information_schema.check_constraints \
                 where constraint_name = '{name}'"
            ),
        };
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

    /// Whether no default is in force.
    ///
    /// Four products answer `COLUMN_DEF` as NULL once a default is dropped.
    /// Oracle spells the removal `DEFAULT NULL` — it has no `DROP DEFAULT` —
    /// and stores that expression, so `getColumns` hands back the text `NULL`
    /// for a column that has no default just as surely as the others hand back
    /// nothing. The two are the same fact spelled twice, so both are accepted.
    fn has_no_default(&self) -> bool {
        match self.default.as_deref() {
            None => true,
            Some(text) => text.trim().eq_ignore_ascii_case("null"),
        }
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

#[test]
fn mariadb_creates_a_table_with_its_keys_and_references() {
    let server = server!(Product::MariaDb);
    create_with_constraints(&server);
}

#[test]
fn mssql_creates_a_table_with_its_keys_and_references() {
    let server = server!(Product::MsSql);
    create_with_constraints(&server);
}

#[test]
fn oracle_creates_a_table_with_its_keys_and_references() {
    let server = server!(Product::Oracle);
    create_with_constraints(&server);
}

/// `plan_create` with every clause it can write, read back out of the
/// catalogue.
///
/// The two foreign keys are the pair worth having: one names the referenced
/// columns and one leaves them out. An omitted list is how most products spell
/// "that table's own primary key" — PostgreSQL, SQL Server and Oracle among
/// them, MySQL and MariaDB not — so the form the omitting key takes here is per
/// product, and
/// [`a_foreign_key_with_no_referenced_columns_is_refused_before_mysql_sees_it`]
/// is where the difference is pinned down.
fn create_with_constraints(server: &Server) {
    let parent = server.table("parent");
    let child = server.table("child");
    let (parent_pk, parent_uq, child_fk) = (
        server.constraint(&parent, "_pk"),
        server.constraint(&parent, "_uq"),
        server.constraint(&child, "_fk"),
    );

    let mut create = TableCreate::new([parent.as_str()]);
    create
        .columns
        .push(ColumnDef::new("id", "integer").with_not_null(true));
    create
        .columns
        .push(ColumnDef::new("code", "varchar(10)").with_not_null(true));
    create
        .constraints
        .push(TableConstraint::primary_key(["id"]).with_name(parent_pk.clone()));
    create
        .constraints
        .push(TableConstraint::unique(["code"]).with_name(parent_uq.clone()));
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
            // PostgreSQL, SQL Server and Oracle resolve the omitted list to the
            // parent's own key, which is the form that stays correct when that
            // key is later re-ordered.
            Product::Postgres | Product::MsSql | Product::Oracle => {
                TableConstraint::foreign_key(["parent_id"], [parent.as_str()])
            }
            // MySQL and MariaDB resolve nothing, so `plan_create` refuses the
            // shape for them. The columns are spelled out here so that the rest
            // of this test is about what it is about.
            Product::MySql | Product::MariaDb => {
                TableConstraint::foreign_key_to(["parent_id"], [parent.as_str()], ["id"])
            }
        }
        .with_name(child_fk.clone()),
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
    let named = server.primary_key_name(&parent);
    let invented = server.primary_key_name(&child);
    match server.product {
        // Three products keep the name they were given and invent one for the
        // key that was left unnamed. What they invent differs — PostgreSQL's
        // `<table>_pkey` is the only one a person would have guessed — and none
        // of it is a name the user typed, which is why §7.10 has the structure
        // pane read a constraint's name out of the catalogue before offering to
        // drop it.
        Product::Postgres | Product::MsSql | Product::Oracle => {
            assert_eq!(named.as_deref(), Some(parent_pk.as_str()));
            let invented = invented.expect("the server named the unnamed key");
            let looks_generated = match server.product {
                Product::Postgres => invented == format!("{child}_pkey"),
                // `PK__rb_child__<16 hex digits>`, truncating the table name.
                Product::MsSql => invented.starts_with("PK__"),
                // `SYS_C0011234`, naming nothing at all.
                _ => invented.starts_with("SYS_C"),
            };
            assert!(
                looks_generated,
                "{:?} invented {invented}, which is not the shape this test recorded",
                server.product
            );
        }
        // The MySQL family does not: a primary key is always called `PRIMARY`,
        // whatever the `CONSTRAINT <name>` prefix asked for. Which is exactly
        // why `ConstraintKind::PrimaryKey` drops it by no name at all there —
        // there is no name to drop it by.
        Product::MySql | Product::MariaDb => {
            assert_eq!(named.as_deref(), Some("PRIMARY"));
            assert_eq!(invented.as_deref(), Some("PRIMARY"));
        }
    }

    // The unique constraint is backed by a unique index of its own name on
    // both products, which is what makes MySQL's `DROP INDEX` spelling work.
    assert!(
        server.has_unique_index(&parent, &parent_uq),
        "no unique index for the UNIQUE constraint: {:?}",
        server.describe("indexes", &parent)
    );

    // The two references, one of which never named a referenced column.
    let to_code = (
        "parent_code".to_string(),
        parent.clone(),
        "code".to_string(),
    );
    let to_id = ("parent_id".to_string(), parent.clone(), "id".to_string());
    match server.product {
        // Oracle is the one product here whose driver does not report both.
        // `getImportedKeys` in ojdbc11 answers only for a reference whose
        // target is a *primary* key, and `parent_code` points at the unique
        // one — so the constraint that took the trouble to name its referenced
        // column is the one JDBC hides. It is really there: the dictionary is
        // asked below, and answers two. This is a driver's reading of the
        // catalogue rather than anything §7.10 claims, but it is the reading
        // the detail panel's references tab shows, so it is worth having
        // written down.
        Product::Oracle => {
            assert_eq!(server.references(&child), [to_id]);
            assert_eq!(
                scalar(
                    &server.session,
                    &format!(
                        "select count(*) from user_constraints \
                         where table_name = '{child}' and constraint_type = 'R'"
                    )
                ),
                "2",
                "both references were created, whatever JDBC says"
            );
        }
        _ => assert_eq!(
            server.references(&child),
            [to_code, to_id],
            "the omitted column list resolved to the parent's own key"
        ),
    }

    // And they are constraints rather than documentation.
    let (id, parent_id, parent_code) = (
        server.col("id"),
        server.col("parent_id"),
        server.col("parent_code"),
    );
    server.exec(&format!("insert into {parent} values (1, 'a')"));
    server.exec(&format!(
        "insert into {child} ({id}, {parent_id}, {parent_code}) values (1, 1, 'a')"
    ));
    server
        .session
        .execute(&StatementSpec::new(format!(
            "insert into {child} ({id}, {parent_id}) values (2, 999)"
        )))
        .expect_err("the foreign key refuses a parent that is not there");
}

#[test]
fn a_foreign_key_with_no_referenced_columns_is_refused_before_mysql_sees_it() {
    let server = server!(Product::MySql);
    reference_without_columns(&server);
}

#[test]
fn a_foreign_key_with_no_referenced_columns_is_refused_before_mariadb_sees_it() {
    // MariaDB inherited the refusal along with the grammar —
    // `named_reference_columns` is the row MariaDB copies from MySQL and this
    // is the server that confirms it — but not the wording: now that it has a
    // `DialectId`, the message names MariaDB, which is what a user staring at
    // their own connection needs to read.
    let server = server!(Product::MariaDb);
    reference_without_columns(&server);
}

/// The refusal, and then the same shape with the columns named going through.
fn reference_without_columns(server: &Server) {
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
    // The refusal names the product the user connected to rather than the
    // family it belongs to, which is what having two `DialectId`s buys.
    let named = match server.product {
        Product::MariaDb => "MariaDB",
        _ => "MySQL",
    };
    assert!(message.contains(named), "{message}");
    assert!(
        message.contains("spelled out"),
        "the refusal has to say what to do about it: {message}"
    );

    // And the same shape with the column named is accepted by the server, so
    // what is being refused is the omission and nothing else about the
    // statement. This half is why the test needs a real server at all.
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

#[test]
fn mariadb_adds_drops_and_renames_a_column() {
    let server = server!(Product::MariaDb);
    add_drop_rename(&server);
}

#[test]
fn mssql_adds_drops_and_renames_a_column() {
    let server = server!(Product::MsSql);
    add_drop_rename(&server);
}

#[test]
fn oracle_adds_drops_and_renames_a_column() {
    let server = server!(Product::Oracle);
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
        Product::MySql | Product::MariaDb => [
            format!("ALTER TABLE {t} ADD COLUMN added varchar(20) NOT NULL DEFAULT 'z'"),
            format!("ALTER TABLE {t} CHANGE COLUMN old_name new_name integer"),
            format!("ALTER TABLE {t} DROP COLUMN gone"),
        ],
        // A bare `ADD`, and a rename that is not `ALTER TABLE` at all: SQL
        // Server's is a stored procedure taking the old column qualified by its
        // table and the new one bare, as a name rather than an identifier.
        Product::MsSql => [
            format!("ALTER TABLE {t} ADD added varchar(20) NOT NULL DEFAULT 'z'"),
            format!("EXEC sp_rename '{t}.old_name', 'new_name', 'COLUMN'"),
            format!("ALTER TABLE {t} DROP COLUMN gone"),
        ],
        // A bare `ADD` too, with the default ahead of `NOT NULL` — Oracle's
        // grammar puts it in the datatype clause — and every column quoted,
        // because a lower-case name reaching this server unquoted would be
        // stored shouted.
        Product::Oracle => [
            format!(r#"ALTER TABLE {t} ADD "added" varchar(20) DEFAULT 'z' NOT NULL"#),
            format!(r#"ALTER TABLE {t} RENAME COLUMN "old_name" TO "new_name""#),
            format!(r#"ALTER TABLE {t} DROP COLUMN "gone""#),
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

#[test]
fn mariadb_changes_a_type_a_nullability_and_a_default() {
    let server = server!(Product::MariaDb);
    attribute_changes(&server);
}

#[test]
fn oracle_changes_a_type_a_nullability_and_a_default() {
    let server = server!(Product::Oracle);
    attribute_changes(&server);
}

/// Every attribute change the generator can write, in one batch: nullability
/// both ways, a default set and a default dropped, and a type retyped.
///
/// SQL Server is not here, and cannot be: `plan_alter` refuses a default change
/// for it outright, so the batch below has no SQL Server form. What that
/// product does with the changes it *can* express is
/// [`mssql_changes_a_type_and_a_nullability_in_one_clause`], and the refusal
/// itself is [`mssql_refuses_to_change_a_default`].
fn attribute_changes(server: &Server) {
    let (wider, wider_catalog) = server.product.widened();
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
        to: ColumnDef::new("widens", wider),
    });
    let batch = plan_alter(&alter, server.dialect()).expect("plans");

    let expected = match server.product {
        // The standard family: an independent clause per attribute.
        Product::Postgres => vec![
            format!("ALTER TABLE {t} ALTER COLUMN to_not_null SET NOT NULL"),
            format!("ALTER TABLE {t} ALTER COLUMN to_nullable DROP NOT NULL"),
            format!("ALTER TABLE {t} ALTER COLUMN gains_default SET DEFAULT 'd'"),
            format!("ALTER TABLE {t} ALTER COLUMN loses_default DROP DEFAULT"),
            format!("ALTER TABLE {t} ALTER COLUMN widens SET DATA TYPE {wider}"),
        ],
        // MySQL restates — except for a default on its own, which `ALTER
        // COLUMN` changes as metadata where `MODIFY` would rewrite the table.
        // Both spellings are sent here, which is the point of listing them.
        Product::MySql | Product::MariaDb => vec![
            format!("ALTER TABLE {t} MODIFY COLUMN to_not_null integer NOT NULL"),
            format!("ALTER TABLE {t} MODIFY COLUMN to_nullable integer"),
            format!("ALTER TABLE {t} ALTER COLUMN gains_default SET DEFAULT 'd'"),
            format!("ALTER TABLE {t} ALTER COLUMN loses_default DROP DEFAULT"),
            format!("ALTER TABLE {t} MODIFY COLUMN widens {wider}"),
        ],
        // Oracle names only what changed, inside one `MODIFY (...)`. Restating
        // an attribute that did not change is not redundant there but an error:
        // `NOT NULL` on a column that already has it is ORA-01442. And a
        // default is removed by being set to `DEFAULT NULL`, which is the one
        // spelling in this table that reads like the opposite of what it does.
        Product::Oracle => vec![
            format!(r#"ALTER TABLE {t} MODIFY ("to_not_null" NOT NULL)"#),
            format!(r#"ALTER TABLE {t} MODIFY ("to_nullable" NULL)"#),
            format!(r#"ALTER TABLE {t} MODIFY ("gains_default" DEFAULT 'd')"#),
            format!(r#"ALTER TABLE {t} MODIFY ("loses_default" DEFAULT NULL)"#),
            format!(r#"ALTER TABLE {t} MODIFY ("widens" {wider})"#),
        ],
        Product::MsSql => unreachable!("SQL Server cannot express half of this batch"),
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
    let loses = server.column(&t, "loses_default");
    assert!(
        loses.has_no_default(),
        "the default was dropped, not set to something: {loses:?}"
    );
    assert_eq!(server.column(&t, "widens").type_name, wider_catalog);
}

// --- SQL Server, whose defaults are constraints ----------------------------

#[test]
fn mssql_changes_a_type_and_a_nullability_in_one_clause() {
    let server = server!(Product::MsSql);
    let (wider, wider_catalog) = Product::MsSql.widened();

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
        .push(ColumnDef::new("widens", "integer").with_not_null(true));
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
        from: ColumnDef::new("widens", "integer").with_not_null(true),
        to: ColumnDef::new("widens", wider).with_not_null(true),
    });
    let batch = plan_alter(&alter, server.dialect()).expect("plans");

    // The type is restated on the two statements that only changed a
    // nullability, and the nullability on the one that only changed a type.
    // Neither is padding: `ALTER COLUMN` takes a whole datatype clause, and
    // leaving the nullability out of it does not mean "as before" but "NULL".
    assert_eq!(
        batch,
        [
            format!("ALTER TABLE {t} ALTER COLUMN to_not_null integer NOT NULL"),
            format!("ALTER TABLE {t} ALTER COLUMN to_nullable integer NULL"),
            format!("ALTER TABLE {t} ALTER COLUMN widens {wider} NOT NULL"),
        ]
    );
    server.run(&batch);

    assert_eq!(server.column(&t, "to_not_null").nullable, Some(false));
    assert_eq!(server.column(&t, "to_nullable").nullable, Some(true));
    let widens = server.column(&t, "widens");
    assert_eq!(widens.type_name, wider_catalog);
    assert_eq!(
        widens.nullable,
        Some(false),
        "the restated NOT NULL survived the retype: {widens:?}"
    );

    // And the trap, sent by hand: the same statement with the nullability left
    // out. SQL Server accepts it and makes the column nullable, which is why
    // the generator writes that clause on every `ALTER COLUMN` it produces.
    server.exec(&format!("ALTER TABLE {t} ALTER COLUMN widens {wider}"));
    let widens = server.column(&t, "widens");
    assert_eq!(
        widens.nullable,
        Some(true),
        "SQL Server reset the column to nullable, exactly as §7.10 warns: {widens:?}"
    );
}

#[test]
fn mssql_refuses_to_change_a_default() {
    let server = server!(Product::MsSql);

    // Nothing is created: the refusal happens in the generator, before a
    // statement exists to send. What the server is here for is the other half —
    // that a default *can* be written by a `CREATE`, so what is being refused
    // is changing one rather than having one.
    let t = server.table("nodefault");
    let mut create = TableCreate::new([t.as_str()]);
    create
        .columns
        .push(ColumnDef::new("id", "integer").with_not_null(true));
    create.columns.push(
        ColumnDef::new("qty", "integer")
            .with_not_null(true)
            .with_default("1"),
    );
    server.run(&plan_create(&create, server.dialect()).expect("plans"));
    let qty = server.column(&t, "qty");
    assert!(qty.defaults_to("1"), "the CREATE's default took: {qty:?}");

    let mut alter = TableAlter::new([t.as_str()]);
    alter.changes.push(ColumnChange {
        from: ColumnDef::new("qty", "integer")
            .with_not_null(true)
            .with_default("1"),
        to: ColumnDef::new("qty", "integer")
            .with_not_null(true)
            .with_default("2"),
    });
    let error = plan_alter(&alter, server.dialect())
        .expect_err("the generator refuses rather than writing something the server would take");
    let message = error.to_string();
    assert!(message.contains("SQL Server"), "{message}");
    assert!(
        message.contains("qty"),
        "the refusal names the column it is about: {message}"
    );

    // The reason, which is what makes this a refusal rather than a gap: the
    // default is a constraint of its own, under a name the server invented and
    // `getColumns` never reports, so changing one is a drop and an add of
    // something the pane cannot name. The catalogue is asked here so the shape
    // of that name is on the record.
    let invented = scalar(
        &server.session,
        &format!(
            "select name from sys.default_constraints \
             where parent_object_id = object_id('{t}')"
        ),
    );
    assert!(
        invented.starts_with("DF__"),
        "SQL Server named the default constraint {invented}"
    );
    assert_eq!(
        qty.default.as_deref(),
        Some("((1))"),
        "and `getColumns` reports the expression, never that name"
    );
}

// --- the claim §7.10 leans on hardest --------------------------------------

#[test]
fn mysql_modify_column_restates_the_whole_definition() {
    let server = server!(Product::MySql);
    modify_column_restates(&server);
}

#[test]
fn mariadb_modify_column_restates_the_whole_definition() {
    // The claim §7.10 makes is about a grammar, and MariaDB speaks it. Whether
    // it also shares the *behaviour* the grammar's restating exists to guard
    // against — the silent loss of a `NOT NULL` — is not something a shared
    // dialect can promise, so the counterfactual half of this test is run
    // against MariaDB too rather than assumed from MySQL.
    let server = server!(Product::MariaDb);
    modify_column_restates(&server);
}

fn modify_column_restates(server: &Server) {
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
        "{:?} dropped the NOT NULL, exactly as §7.10 warns: {qty:?}",
        server.product
    );
    // "Without a word" is the point; the two servers do not even report the
    // absence the same way. MySQL answers `COLUMN_DEF` as NULL and MariaDB
    // answers the text `NULL`, which is that server's way of writing the
    // implicit default a nullable column gets — the same fact, spelled.
    assert!(
        qty.has_no_default(),
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

/// A column default no longer costs Oracle its `describe columns`.
///
/// `DescribeRequest::new("columns")` used to answer ORA-17027, *stream has
/// already been closed*, for any Oracle table carrying a column default — which
/// is to say for most real Oracle tables, and for every table the structure
/// pane would create with one. Nothing about DDL was involved: the two-column
/// table this test makes by hand was enough to provoke it.
///
/// The fault was `Describe.columns` in `bridge/src/main/java`, which read
/// `getColumns`' fields out of order: `IS_NULLABLE` is column 18 of that result
/// set and was fetched ahead of `REMARKS` (12) and `COLUMN_DEF` (13). Oracle
/// hands back `COLUMN_DEF` as a `LONG`, and a `LONG` is a stream the driver
/// closes the moment a column to its right is read, so the later reach for the
/// default found nothing left to read. The bridge now takes every metadata
/// result set in column order, the rule JDBC has always asked of callers.
///
/// So the assertion is the ordinary one: the default the `CREATE` wrote comes
/// back through the ordinary path.
#[test]
fn oracle_describe_columns_survives_a_column_default() {
    let server = server!(Product::Oracle);

    let t = server.table("longdef");
    let mut create = TableCreate::new([t.as_str()]);
    create
        .columns
        .push(ColumnDef::new("id", "integer").with_not_null(true));
    create
        .columns
        .push(ColumnDef::new("qty", "integer").with_default("7"));
    server.run(&plan_create(&create, server.dialect()).expect("plans"));

    let request = DescribeRequest::new("columns")
        .with_table(&t)
        .with_schema(server.schema.clone().expect("Oracle reports a schema"));
    let items = server
        .session
        .describe(&request)
        .unwrap_or_else(|error| panic!("describe columns of {t}: {error}"))
        .items;

    assert_eq!(items.len(), 2);
    let default = items[1]
        .get("default")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_else(|| panic!("the default is there to read: {:?}", items[1]));
    // Oracle keeps `data_default` as the source text of the expression, newline
    // and all, and streams it back the same way. The text either side is what
    // the CREATE wrote.
    assert_eq!(
        default.trim(),
        "7",
        "the default the CREATE wrote, read back through JDBC"
    );
}

#[test]
fn oracle_modify_names_only_what_changed_because_restating_is_an_error() {
    let server = server!(Product::Oracle);

    let t = server.table("only_changed");
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

    // Only the type changes, so only the type is named — and unlike MySQL's
    // restating, that is not a choice between two working spellings.
    let mut alter = TableAlter::new([t.as_str()]);
    alter.changes.push(ColumnChange {
        from: ColumnDef::new("qty", "integer")
            .with_not_null(true)
            .with_default("7"),
        to: ColumnDef::new("qty", "number(19)")
            .with_not_null(true)
            .with_default("7"),
    });
    let batch = plan_alter(&alter, server.dialect()).expect("plans");
    assert_eq!(
        batch,
        [format!(r#"ALTER TABLE {t} MODIFY ("qty" number(19))"#)]
    );
    server.run(&batch);

    let qty = server.column(&t, "qty");
    assert_eq!(
        qty.nullable,
        Some(false),
        "Oracle left the NOT NULL where it was: {qty:?}"
    );
    assert!(qty.defaults_to("7"), "and the default: {qty:?}");

    // The counterfactual, sent by hand: the same statement with the unchanged
    // `NOT NULL` restated the way MySQL's form would have restated it. MySQL
    // needs that clause and Oracle refuses it, which is why §7.10 has two
    // restating families rather than one.
    let error = server
        .session
        .execute(&StatementSpec::new(format!(
            r#"ALTER TABLE {t} MODIFY ("qty" number(19) NOT NULL)"#
        )))
        .expect_err("restating a NOT NULL that is already there is ORA-01442");
    assert!(
        error.to_string().contains("01442"),
        "the error should be ORA-01442, and was: {error}"
    );
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

/// The test that split the dialect in two, and the reason `DialectId::MariaDb`
/// exists.
///
/// Three of the four spellings `Dialect::MYSQL` writes are MariaDB's too. The
/// fourth is not: `ALTER TABLE t DROP CHECK c` is MySQL 8.0.16's syntax for
/// taking a check constraint away, and MariaDB 11 answers
///
/// ```text
/// [SQLSTATE 42000, code 1064]: You have an error in your SQL syntax; check the
/// manual that corresponds to your MariaDB server version for the right syntax
/// to use near 'CHECK rb_dc_child_..._ck' at line 1
/// ```
///
/// MariaDB spells it `DROP CONSTRAINT c`, which it has taken since 10.2.22 and
/// which this test confirmed against a running server. So the two products
/// disagree about exactly one row of `AlterStyle` — and a row is what
/// `AlterStyle` is for, which is why the fix was `Dialect::MARIADB`, a copy of
/// MySQL's row with `DropStyle::PerKind`'s `check` word replaced, rather than a
/// branch in the emitter. Everything else this file asserts about the pair is
/// unchanged, which is the other half of what the split had to prove.
#[test]
fn mariadb_drops_a_constraint_of_every_kind() {
    let server = server!(Product::MariaDb);
    drop_every_constraint(&server);
}

#[test]
fn mssql_drops_a_constraint_of_every_kind() {
    let server = server!(Product::MsSql);
    drop_every_constraint(&server);
}

#[test]
fn oracle_drops_a_constraint_of_every_kind() {
    let server = server!(Product::Oracle);
    drop_every_constraint(&server);
}

/// The four kinds, dropped in one batch.
///
/// This is the test MySQL was in scope for. §7.10 claims that everywhere but
/// MySQL a drop is `DROP CONSTRAINT <name>`, and that MySQL has no generic
/// form and spells each kind separately — `DROP PRIMARY KEY` with no name at
/// all, `DROP FOREIGN KEY`, `DROP INDEX` for a unique, `DROP CHECK`. Four
/// different statements where one would do, none of which a server had ever
/// confirmed. Running it against SQL Server and Oracle is what turns
/// "everywhere but MySQL" from a generalization about the standard into a
/// reading of two more servers. Running it against MariaDB is what found the
/// edge of the family: three of the four spellings are shared, and the check
/// drop is not — which is why the expectation below forks on the product for
/// one line and agrees on the other three.
fn drop_every_constraint(server: &Server) {
    let parent = server.table("dc_parent");
    let child = server.table("dc_child");
    let (fk, pk, uq, ck) = (
        server.constraint(&child, "_fk"),
        server.constraint(&child, "_pk"),
        server.constraint(&child, "_uq"),
        server.constraint(&child, "_ck"),
    );

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
        .push(TableConstraint::primary_key(["id"]).with_name(pk.clone()));
    create
        .constraints
        .push(TableConstraint::unique(["code"]).with_name(uq.clone()));
    create.constraints.push(
        // The referenced column is named because MySQL insists on it; this
        // test is about the drop, not about the omitted list.
        TableConstraint::foreign_key_to(["parent_id"], [parent.as_str()], ["id"])
            .with_name(fk.clone()),
    );
    server.run(&plan_create(&create, server.dialect()).expect("plans"));

    // `plan_alter` drops constraints and never adds one (§7.10), so the check
    // constraint is put there by hand. It is the fixture, not the thing under
    // test — what is under test is the statement that takes it away.
    server.exec(&format!(
        "ALTER TABLE {child} ADD CONSTRAINT {ck} CHECK ({} >= 0)",
        server.col("qty")
    ));

    assert_eq!(server.primary_key(&child), ["id"]);
    assert_eq!(server.references(&child).len(), 1);
    assert!(server.has_unique_index(&child, &uq));
    assert_eq!(server.check_constraints(&ck), 1);

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
            name: server.constraint(&child, suffix),
        });
    }
    let batch = plan_alter(&alter, server.dialect()).expect("plans");

    let expected = match server.product {
        // One spelling, whatever the kind — the kind is carried only because
        // MySQL needs it.
        Product::Postgres | Product::MsSql | Product::Oracle => vec![
            format!("ALTER TABLE {child} DROP CONSTRAINT {fk}"),
            format!("ALTER TABLE {child} DROP CONSTRAINT {pk}"),
            format!("ALTER TABLE {child} DROP CONSTRAINT {uq}"),
            format!("ALTER TABLE {child} DROP CONSTRAINT {ck}"),
        ],
        // Four. Note that the primary key is dropped by no name at all, which
        // is just as well: the MySQL family called it `PRIMARY` rather than
        // what the `CREATE` asked for.
        Product::MySql => vec![
            format!("ALTER TABLE {child} DROP FOREIGN KEY {fk}"),
            format!("ALTER TABLE {child} DROP PRIMARY KEY"),
            format!("ALTER TABLE {child} DROP INDEX {uq}"),
            format!("ALTER TABLE {child} DROP CHECK {ck}"),
        ],
        // The same four but the last, which is the whole of what separates the
        // two dialects: MariaDB answers 1064 to `DROP CHECK` and takes the
        // generic form instead. The three above are byte-for-byte MySQL's.
        Product::MariaDb => vec![
            format!("ALTER TABLE {child} DROP FOREIGN KEY {fk}"),
            format!("ALTER TABLE {child} DROP PRIMARY KEY"),
            format!("ALTER TABLE {child} DROP INDEX {uq}"),
            format!("ALTER TABLE {child} DROP CONSTRAINT {ck}"),
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
        !server.has_unique_index(&child, &uq),
        "{:?}: the unique index is still there: {:?}",
        server.product,
        server.describe("indexes", &child)
    );
    assert_eq!(
        server.check_constraints(&ck),
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

#[test]
fn mariadb_renames_a_table() {
    let server = server!(Product::MariaDb);
    rename_table(&server);
}

#[test]
fn mssql_renames_a_table() {
    let server = server!(Product::MsSql);
    rename_table(&server);
}

#[test]
fn oracle_renames_a_table() {
    let server = server!(Product::Oracle);
    rename_table(&server);
}

/// `ALTER TABLE t RENAME TO n` — the same statement on four of the five, and
/// `sp_rename` on the fifth. Whichever it is, §7.10 puts it last in a batch so
/// that everything before it names the table the way the catalogue still holds
/// it.
fn rename_table(server: &Server) {
    let before = server.table("rename");
    let after = server.name(&format!("{before}_renamed"));
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
    let expected = match server.product {
        // Not `ALTER TABLE` at all: the old name is a string literal and the
        // new one is a bare name, so neither goes through `quote_ident`.
        Product::MsSql => format!("EXEC sp_rename '{before}', '{after}'"),
        _ => format!("ALTER TABLE {before} RENAME TO {after}"),
    };
    assert_eq!(batch, [expected]);
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
// The five tests below assert what each driver actually answered, for a
// plain column, an aliased column, a computed column and a column of an
// aliased table. The findings, in short:
//
//   * **Schema and catalog are all but useless.** pgjdbc 42.7.4 answers `""`
//     for *both*, on a plain column of an ordinary table in `public`.
//     Connector/J 9.1.0 and MariaDB Connector/J 3.5.1 answer `""` for schema
//     and the database name for catalog. So the gate can never lean on schema,
//     and only the MySQL family offers a catalog.
//   * **Two drivers report no source table at all.** mssql-jdbc 12.8.1 and
//     ojdbc11 23.6 answer `""` for `getTableName` on a plain column of an
//     ordinary table — not a wrong table, not an alias, nothing. §7.9's gate
//     offers editing only where every column names the same source table, so on
//     SQL Server and Oracle it will never offer it at all. That is a bigger
//     claim than the document's "several drivers answer `""` for schema and
//     catalog", and it is measured rather than reasoned: see
//     [`mssql_reports_no_source_table_at_all`] and
//     [`oracle_reports_no_source_table_at_all`].
//   * **MySQL did not report an alias for the table**, on Connector/J 9.1.0
//     with its defaults: `select b.id from t b` reports `t`. The alias comes
//     back only with the legacy `useOldAliasMetadataBehavior=true`, which the
//     second half of the MySQL test sets on a connection of its own to show
//     that the document's "can" is a real behaviour and not the default one.
//     MariaDB's driver has no such property and always answers the real table.
//   * **pgjdbc reports the column *alias* as the column name.** `getColumnName`
//     and `getColumnLabel` both answer `label` for `note AS label`, where the
//     MySQL family distinguishes them. Nothing in §7.9 says otherwise, but it
//     bears on the gate: a result that aliases a key column looks, on
//     PostgreSQL, like a result that does not carry that key column — and the
//     gate's answer to that is to stay read-only, which is the safe direction.
//     Oracle does the same, and shouts the alias on top of it.

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

#[test]
fn mariadb_answers_exactly_what_connector_j_does() {
    let server = server!(Product::MariaDb);
    let t = metadata_fixture(&server, "meta");
    let database = server.catalog.clone().expect("MariaDB reports a catalog");

    // Column for column, this is [`mysql_reports_a_catalog_but_no_schema_for_a_result_column`]'s
    // answer: the database as the catalog, no schema, the name and the label
    // kept apart, and nothing at all for a computed column. Two drivers written
    // by different people agreeing is worth an assertion of its own, because
    // the moment one of them stops agreeing §7.9's gate behaves differently on
    // a product the dialect table says is the same one.
    let sources = column_sources(
        &server.session,
        &format!("select id, note as label, id + 1 as computed from {t}"),
    );
    assert_eq!(
        sources,
        [
            (
                "id".into(),
                "id".into(),
                t.clone(),
                String::new(),
                database.clone()
            ),
            (
                "note".into(),
                "label".into(),
                t.clone(),
                String::new(),
                database.clone()
            ),
            (
                "computed".into(),
                "computed".into(),
                String::new(),
                String::new(),
                String::new()
            ),
        ],
        "MariaDB Connector/J's answers changed"
    );

    // And the real table for an aliased one. MariaDB's driver has no
    // `useOldAliasMetadataBehavior`, so this is the only behaviour it has —
    // where Connector/J's is the default of two.
    let sources = column_sources(&server.session, &format!("select b.id from {t} b"));
    assert_eq!(
        sources,
        [(
            "id".into(),
            "id".into(),
            t.clone(),
            String::new(),
            database.clone()
        )],
        "MariaDB Connector/J reported the alias where the table was asked for"
    );
}

#[test]
fn mssql_reports_no_source_table_at_all() {
    let server = server!(Product::MsSql);
    let t = metadata_fixture(&server, "meta");

    // Not "no schema" or "no catalog", which is the most §7.9 warns about:
    // mssql-jdbc 12.8.1 answers `""` for the *table* of a plain column of an
    // ordinary table in `dbo`, on a connection that just created it. The name
    // and the label are all it will say.
    let sources = column_sources(
        &server.session,
        &format!("select id, note as label, id + 1 as computed from {t}"),
    );
    assert_eq!(
        sources,
        [
            (
                "id".into(),
                "id".into(),
                String::new(),
                String::new(),
                String::new()
            ),
            // The alias, for the name as well as the label, the way pgjdbc
            // does it.
            (
                "label".into(),
                "label".into(),
                String::new(),
                String::new(),
                String::new()
            ),
            (
                "computed".into(),
                "computed".into(),
                String::new(),
                String::new(),
                String::new()
            ),
        ],
        "mssql-jdbc's answers changed"
    );

    // Which settles what §7.9's gate does on this product: every column's
    // source table is unknown, so no two of them can be shown to name the same
    // one, and the result stays read-only. A driver that starts answering would
    // turn editing on by itself, which is the direction that is safe to be
    // surprised in.
    let sources = column_sources(&server.session, &format!("select b.id from {t} b"));
    assert_eq!(sources[0].2, "", "mssql-jdbc started naming a table");
}

#[test]
fn oracle_reports_no_source_table_at_all() {
    let server = server!(Product::Oracle);
    let t = metadata_fixture(&server, "meta");
    let (id, note) = (server.col("id"), server.col("note"));

    // ojdbc11 is the second driver here with nothing to say about where a
    // column came from. The other half of what this measures is case: the
    // quoted `"id"` comes back lower-cased because that is how the column was
    // stored, while the *alias* `label` was written unquoted and so comes back
    // shouted. Both halves are what §7.9's gate would have to compare, and it
    // is comparing strings the server folded on two different rules.
    let sources = column_sources(
        &server.session,
        &format!("select {id}, {note} as label, {id} + 1 as computed from {t}"),
    );
    assert_eq!(
        sources,
        [
            (
                "id".into(),
                "id".into(),
                String::new(),
                String::new(),
                String::new()
            ),
            (
                "LABEL".into(),
                "LABEL".into(),
                String::new(),
                String::new(),
                String::new()
            ),
            (
                "COMPUTED".into(),
                "COMPUTED".into(),
                String::new(),
                String::new(),
                String::new()
            ),
        ],
        "ojdbc11's answers changed"
    );

    let sources = column_sources(&server.session, &format!("select b.{id} from {t} b"));
    assert_eq!(sources[0].2, "", "ojdbc11 started naming a table");
}
