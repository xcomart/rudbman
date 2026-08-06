//! Request bodies: what a session is opened with, what a statement is executed
//! with, and what a job is started with.
//!
//! These are the Rust side of the JSON documented in `bridge/README.md`. They
//! serialise field for field; nothing here is renamed on the way out, so a
//! reader can compare this file with the bridge's `Session.open` and `Params`
//! and see the same names.
//!
//! **Secrets stay out of the rendering.** [`ConnectionSpec`] implements
//! [`Debug`] by hand: the password is replaced, every driver property value is
//! replaced, and credentials embedded in the JDBC URL are replaced. That is the
//! same discipline `rudbman-core` applies to a stored profile, repeated here
//! because a spec is built at connect time and is exactly what tends to end up
//! in a log line.

use std::collections::BTreeMap;
use std::fmt;
use std::path::PathBuf;

use serde::Serialize;

/// Everything needed to open one JDBC connection.
#[derive(Clone, Serialize)]
pub struct ConnectionSpec {
    /// JDBC URL. Required.
    pub url: String,
    /// Fully qualified driver class name, e.g. `org.postgresql.Driver`.
    /// Required.
    pub driver_class: String,
    /// Driver JARs, in classpath order.
    ///
    /// Order is part of the identity of the class loader the bridge caches:
    /// when two JARs ship the same class, the order decides which one wins, so
    /// two orderings really are two different class paths.
    ///
    /// Empty resolves the driver class from the bridge's own loader, which is
    /// how a driver baked into the jlink image is reached.
    pub jars: Vec<PathBuf>,
    /// User name, passed as the `user` connection property.
    pub username: Option<String>,
    /// Password, passed as the `password` connection property.
    pub password: Option<String>,
    /// Extra driver properties, verbatim.
    pub props: BTreeMap<String, String>,
    /// Ask the driver for a read-only connection.
    pub read_only: bool,
    /// Auto-commit state to set right after connecting.
    pub auto_commit: bool,
    /// Login timeout in seconds, `0` to leave it to the driver.
    ///
    /// Passed through as a `loginTimeout` connection property: `java.sql.Driver`
    /// has no login timeout of its own, and `DriverManager`'s is global mutable
    /// state the bridge stays away from. A driver with a better-known property
    /// name should get it through [`ConnectionSpec::props`].
    pub login_timeout_s: u32,
    /// Keep-alive query, run by a timer inside the bridge.
    pub keep_alive: Option<KeepAliveSpec>,
}

impl ConnectionSpec {
    /// A spec with the two required members and defaults for everything else.
    pub fn new(url: impl Into<String>, driver_class: impl Into<String>) -> Self {
        ConnectionSpec {
            url: url.into(),
            driver_class: driver_class.into(),
            jars: Vec::new(),
            username: None,
            password: None,
            props: BTreeMap::new(),
            read_only: false,
            auto_commit: true,
            login_timeout_s: 0,
            keep_alive: None,
        }
    }

    /// Sets the credentials.
    pub fn with_credentials(
        mut self,
        username: impl Into<String>,
        password: impl Into<String>,
    ) -> Self {
        self.username = Some(username.into());
        self.password = Some(password.into());
        self
    }

    /// Sets the driver JAR list, in classpath order.
    pub fn with_jars(mut self, jars: impl IntoIterator<Item = PathBuf>) -> Self {
        self.jars = jars.into_iter().collect();
        self
    }

    /// Adds one driver property.
    pub fn with_prop(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.props.insert(key.into(), value.into());
        self
    }
}

impl fmt::Debug for ConnectionSpec {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ConnectionSpec")
            .field("url", &MaskedUrl(&self.url))
            .field("driver_class", &self.driver_class)
            .field("jars", &self.jars)
            .field("username", &self.username)
            .field("password", &self.password.as_ref().map(|_| Redacted))
            // Values only: a driver property key says which driver this is,
            // and one of those keys is very often a password under a name this
            // crate has never heard of.
            .field(
                "props",
                &self
                    .props
                    .keys()
                    .map(|key| (key, Redacted))
                    .collect::<Vec<_>>(),
            )
            .field("read_only", &self.read_only)
            .field("auto_commit", &self.auto_commit)
            .field("login_timeout_s", &self.login_timeout_s)
            .field("keep_alive", &self.keep_alive)
            .finish()
    }
}

/// Keep-alive settings for a session.
///
/// The bridge runs the query on a timer that skips its round rather than
/// queueing when the connection is busy — a statement already in flight keeps
/// the connection just as alive.
#[derive(Clone, Debug, Serialize)]
pub struct KeepAliveSpec {
    /// Whether the timer runs at all.
    pub enabled: bool,
    /// Interval in seconds. Ignored when zero.
    pub interval_s: u32,
    /// The statement to run, e.g. `select 1`. Ignored when empty.
    pub query: String,
}

/// A statement to execute, with its bindings and limits.
#[derive(Clone, Debug, Serialize)]
pub struct StatementSpec {
    /// The SQL text. One statement.
    pub sql: String,
    /// Bound parameters. An empty list makes the bridge use a plain
    /// `Statement` instead of a `PreparedStatement`.
    pub params: Vec<Param>,
    /// JDBC fetch size hint, `0` to leave it to the driver.
    pub fetch_size: u32,
    /// `Statement.setMaxRows`, `0` for unlimited.
    pub max_rows: u32,
    /// Query timeout in seconds, `0` for none.
    pub timeout_s: u32,
}

impl StatementSpec {
    /// A statement with no parameters and no limits.
    pub fn new(sql: impl Into<String>) -> Self {
        StatementSpec {
            sql: sql.into(),
            params: Vec::new(),
            fetch_size: 0,
            max_rows: 0,
            timeout_s: 0,
        }
    }

    /// Sets the bound parameters.
    pub fn with_params(mut self, params: impl IntoIterator<Item = Param>) -> Self {
        self.params = params.into_iter().collect();
        self
    }

    /// Sets the fetch size hint.
    pub fn with_fetch_size(mut self, rows: u32) -> Self {
        self.fetch_size = rows;
        self
    }

    /// Sets the statement timeout in seconds.
    pub fn with_timeout_s(mut self, seconds: u32) -> Self {
        self.timeout_s = seconds;
        self
    }
}

/// One bound parameter.
///
/// Plain scalars travel as bare JSON. Everything JSON cannot express without
/// loss travels as `{"type": …, "value": …}`, and that is not a style choice:
/// a `DECIMAL(20,8)` sent as a JSON number goes through a double and arrives
/// rounded, which is exactly what the batch codec refuses to do in the other
/// direction.
#[derive(Clone, Debug, PartialEq)]
pub enum Param {
    /// SQL NULL.
    Null,
    /// A boolean.
    Bool(bool),
    /// A 64-bit integer.
    I64(i64),
    /// A double.
    F64(f64),
    /// Text.
    Str(String),
    /// An exact decimal, in its plain string form — never exponent notation.
    Decimal(String),
    /// A date, `YYYY-MM-DD` (what `java.sql.Date.valueOf` accepts).
    Date(String),
    /// A time, `HH:MM:SS`.
    Time(String),
    /// A timestamp, `YYYY-MM-DD HH:MM:SS[.fffffffff]`.
    Timestamp(String),
    /// Raw bytes; base64-encoded on the wire.
    Bytes(Vec<u8>),
}

impl Serialize for Param {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeStruct;

        /// Emits the `{"type": …, "value": …}` form.
        fn typed<S: serde::Serializer>(
            serializer: S,
            type_name: &str,
            value: &str,
        ) -> Result<S::Ok, S::Error> {
            let mut object = serializer.serialize_struct("Param", 2)?;
            object.serialize_field("type", type_name)?;
            object.serialize_field("value", value)?;
            object.end()
        }

        match self {
            // The bare forms the bridge reads by JSON type. Only these four:
            // anything else would have to survive a round trip through JSON's
            // single numeric type.
            Param::Null => serializer.serialize_none(),
            Param::Bool(value) => serializer.serialize_bool(*value),
            Param::I64(value) => serializer.serialize_i64(*value),
            Param::Str(value) => serializer.serialize_str(value),

            // f64 is tagged rather than bare: a whole-numbered double written
            // as a bare JSON number reads back as an integer and would be bound
            // with setLong.
            Param::F64(value) => {
                let mut object = serializer.serialize_struct("Param", 2)?;
                object.serialize_field("type", "f64")?;
                object.serialize_field("value", value)?;
                object.end()
            }
            Param::Decimal(text) => typed(serializer, "decimal", text),
            Param::Date(text) => typed(serializer, "date", text),
            Param::Time(text) => typed(serializer, "time", text),
            Param::Timestamp(text) => typed(serializer, "timestamp", text),
            Param::Bytes(bytes) => typed(serializer, "bytes", &base64(bytes)),
        }
    }
}

/// Standard base64, with padding, as `java.util.Base64.getDecoder` expects.
///
/// Hand-rolled to keep a dependency out of the workspace for twenty lines of
/// table lookup; binary bind parameters are not a hot path.
fn base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let byte = |index: usize| *chunk.get(index).unwrap_or(&0) as u32;
        let group = (byte(0) << 16) | (byte(1) << 8) | byte(2);
        for offset in 0..4 {
            if offset <= chunk.len() {
                out.push(ALPHABET[(group >> (18 - 6 * offset)) as usize & 0x3f] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

/// A metadata query: the request body of `DESCRIBE`.
///
/// One operation with a `kind` rather than one operation per kind, because the
/// list of metadata kinds keeps growing and a new operation code every time is
/// how the Rust and Java tables drift apart. Metadata is called rarely enough
/// that the JSON parse costs nothing worth counting.
///
/// Every kind answers `{kind, items[]}` — **except `ddl`**, which answers one
/// document rather than a list of rows and therefore has its own path,
/// [`Session::describe_ddl`](crate::Session::describe_ddl). Asking for `ddl`
/// through [`Session::describe`](crate::Session::describe) fails to parse, on
/// purpose: the two answers are different shapes and the type system is the
/// right place to say so.
#[derive(Clone, Debug, Default, Serialize)]
pub struct DescribeRequest {
    /// One of `catalogs`, `schemas`, `tables`, `columns`, `primary_keys`,
    /// `imported_keys`, `exported_keys`, `indexes`, `type_info`, `procedures`,
    /// `functions`, `sequences`.
    ///
    /// `ddl` is the thirteenth kind and goes through
    /// [`Session::describe_ddl`](crate::Session::describe_ddl) instead.
    ///
    /// `procedures` and `functions` carry each routine's `parameters[]` inline,
    /// so a schema with two hundred routines costs one round trip rather than
    /// two hundred. Which of the two lists a routine appears in is the product's
    /// decision — H2 files `CREATE ALIAS` functions under `procedures` and
    /// answers `getFunctions` with nothing at all — so an empty list means
    /// "filed elsewhere", not "none".
    ///
    /// `sequences` is a vendor catalogue query, because JDBC never grew an
    /// accessor for sequences. An empty list is a correct answer on a product
    /// the bridge has no query for, and on one where the query was refused.
    pub kind: String,
    /// Exact catalog name.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub catalog: Option<String>,
    /// Exact schema name.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub schema: Option<String>,
    /// Schema name as a `LIKE` pattern.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub schema_pattern: Option<String>,
    /// Exact table name. Required by `primary_keys`, `imported_keys`,
    /// `exported_keys` and `indexes`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub table: Option<String>,
    /// Table name as a `LIKE` pattern.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub table_pattern: Option<String>,
    /// Exact column name.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub column: Option<String>,
    /// Column name as a `LIKE` pattern.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub column_pattern: Option<String>,
    /// `procedures`, `functions` and `sequences` only: exact routine or
    /// sequence name.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// `procedures` and `functions` only: routine name as a `LIKE` pattern.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name_pattern: Option<String>,
    /// `tables` only: the table types to list, e.g. `["TABLE", "VIEW"]`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub types: Option<Vec<String>>,
    /// `indexes` only: list unique indexes only.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unique_only: Option<bool>,
    /// `indexes` only: accept approximate statistics.
    ///
    /// The bridge defaults this to `true`, and that default is load bearing: a
    /// statistics refresh on a large schema is the difference between an
    /// instant answer and a minute of waiting.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub approximate: Option<bool>,
    /// `ddl` only: which layer should answer.
    ///
    /// Set for you by [`Session::describe_ddl`](crate::Session::describe_ddl);
    /// every other kind ignores it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<DdlSource>,
}

impl DescribeRequest {
    /// A request for one metadata kind.
    pub fn new(kind: impl Into<String>) -> Self {
        DescribeRequest {
            kind: kind.into(),
            ..Default::default()
        }
    }

    /// Sets the exact catalog name.
    pub fn with_catalog(mut self, catalog: impl Into<String>) -> Self {
        self.catalog = Some(catalog.into());
        self
    }

    /// Sets the exact schema name.
    pub fn with_schema(mut self, schema: impl Into<String>) -> Self {
        self.schema = Some(schema.into());
        self
    }

    /// Sets the exact table name.
    pub fn with_table(mut self, table: impl Into<String>) -> Self {
        self.table = Some(table.into());
        self
    }

    /// Sets the exact routine or sequence name.
    pub fn with_name(mut self, name: impl Into<String>) -> Self {
        self.name = Some(name.into());
        self
    }
}

/// Which layer should produce a table's DDL.
///
/// The two layers answer different questions, and neither is right for every
/// caller:
///
/// * the **native** path asks the server to quote its own `CREATE` text back
///   (MySQL's `SHOW CREATE TABLE`, H2's `SCRIPT`). Where it exists it *is* the
///   truth — storage clauses, `CHECK` constraints, vendor syntax and all;
/// * the **metadata** path reassembles the statement from `getColumns`,
///   `getPrimaryKeys`, `getImportedKeys` and `getIndexInfo`. It works on every
///   driver, which is why it exists, and it is **for display**: JDBC metadata
///   carries no `CHECK` constraints, triggers, partitioning or collations, and a
///   view arrives as a bare column list.
///
/// [`DdlResult::source`](crate::DdlResult::source) reports which one answered,
/// so a UI can label reconstructed DDL as reconstructed.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum DdlSource {
    /// Try the native path, fall back to reconstruction. The default, and what
    /// an explorer pane wants.
    #[default]
    Auto,
    /// Native only. Fails with a `sql` error on a product that has no native
    /// path, rather than quietly handing back something reconstructed — which
    /// is what a caller comparing DDL against a file needs.
    Native,
    /// Always reconstruct, even where a native path exists. Useful for
    /// comparing two products, and for exercising the fallback.
    Metadata,
}

/// The request body of `PROBE_DRIVER`: which archives to look inside.
///
/// Built for you by [`Jvm::probe_drivers`](crate::Jvm::probe_drivers); public
/// because a caller may want to inspect or log exactly what was asked.
#[derive(Clone, Debug, Serialize)]
pub struct ProbeRequest {
    /// The JARs to scan. Every path must exist — see
    /// [`Jvm::probe_drivers`](crate::Jvm::probe_drivers) for what happens when
    /// one does not.
    pub jars: Vec<PathBuf>,
}

impl ProbeRequest {
    /// A request for a set of JARs.
    pub fn new(jars: impl IntoIterator<Item = PathBuf>) -> Self {
        ProbeRequest {
            jars: jars.into_iter().collect(),
        }
    }
}

/// A script extraction: the request body of `JOB_START` with `kind: "extract"`
/// (architecture document, §6).
///
/// The job writes a file — `CREATE` statements, `INSERT` statements, CSV or
/// whatever a template makes of the rows — **inside the JVM**. No row of it
/// crosses the JNI boundary; what crosses is a handle and, every couple of
/// hundred milliseconds, a [`JobProgress`](crate::JobProgress). That is the
/// whole point of the data plane: the rows are already on the side that has the
/// file system.
///
/// Nothing here is validated in Rust. The bridge is the single authority on
/// what a malformed request is, and it answers one synchronously —
/// [`Session::start_job`](crate::Session::start_job) fails rather than handing
/// back a job that would fail on the first poll. In particular the bridge
/// rejects a spec that includes neither [`ddl`](ExtractSpec::ddl) nor
/// [`data`](ExtractSpec::data), which is what [`ExtractSpec::new`] alone
/// produces.
///
/// ```
/// use rudbman_jdbc::{DataMode, DataOptions, DdlOptions, ExtractSpec, ObjectRef};
///
/// let spec = ExtractSpec::new("/tmp/app.sql")
///     .with_object(ObjectRef::new("ORDERS").with_schema("APP"))
///     .with_ddl(DdlOptions::included())
///     .with_data(DataOptions::included(DataMode::Insert));
/// ```
#[derive(Clone, Debug, Serialize)]
pub struct ExtractSpec {
    /// Always `"extract"`. The bridge dispatches the job kinds on this member,
    /// and the other two — [`BackupSpec`] and [`TransferSpec`] — are separate
    /// types rather than modes of this one.
    kind: &'static str,
    /// The objects to extract, in the order they should appear in the file.
    ///
    /// Order is the caller's responsibility and it matters: `CREATE`s are
    /// written in this order, `DROP`s in the reverse of it. Foreign keys move
    /// to trailing `ALTER`s under [`Constraints::Alter`], so a dependency
    /// cycle needs no ordering at all.
    pub objects: Vec<ObjectRef>,
    /// Where the file goes and how it is encoded.
    pub output: OutputSpec,
    /// Whether and how the schema is written.
    pub ddl: DdlOptions,
    /// Whether and how the rows are written.
    pub data: DataOptions,
}

impl ExtractSpec {
    /// A spec that writes to `path` and, until something is added to it,
    /// extracts nothing at all.
    ///
    /// The path is resolved **by the JVM, on the machine the JVM runs on**, and
    /// its parent directories are created. A relative path is therefore
    /// relative to the process's working directory, which is rarely what a user
    /// picking a file in a dialogue means: pass an absolute one.
    pub fn new(path: impl Into<PathBuf>) -> Self {
        ExtractSpec {
            kind: "extract",
            objects: Vec::new(),
            output: OutputSpec::new(path),
            ddl: DdlOptions::default(),
            data: DataOptions::default(),
        }
    }

    /// Adds one object to the list.
    pub fn with_object(mut self, object: ObjectRef) -> Self {
        self.objects.push(object);
        self
    }

    /// Sets the whole object list, replacing whatever was there.
    pub fn with_objects(mut self, objects: impl IntoIterator<Item = ObjectRef>) -> Self {
        self.objects = objects.into_iter().collect();
        self
    }

    /// Sets the output encoding and record separator.
    pub fn with_output(mut self, output: OutputSpec) -> Self {
        self.output = output;
        self
    }

    /// Sets the DDL options.
    pub fn with_ddl(mut self, ddl: DdlOptions) -> Self {
        self.ddl = ddl;
        self
    }

    /// Sets the data options.
    pub fn with_data(mut self, data: DataOptions) -> Self {
        self.data = data;
        self
    }
}

/// One database object named for extraction.
///
/// The names are exact, never patterns: this is a list of objects the user
/// picked, not a query. `None` for catalog or schema means "wherever the
/// connection is pointed", the same reading [`DescribeRequest`] gives them.
#[derive(Clone, Debug, Serialize)]
pub struct ObjectRef {
    /// Exact catalog name.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub catalog: Option<String>,
    /// Exact schema name.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub schema: Option<String>,
    /// Exact object name. Required.
    pub name: String,
}

impl ObjectRef {
    /// An object named only by its own name.
    pub fn new(name: impl Into<String>) -> Self {
        ObjectRef {
            catalog: None,
            schema: None,
            name: name.into(),
        }
    }

    /// Sets the exact catalog name.
    pub fn with_catalog(mut self, catalog: impl Into<String>) -> Self {
        self.catalog = Some(catalog.into());
        self
    }

    /// Sets the exact schema name.
    pub fn with_schema(mut self, schema: impl Into<String>) -> Self {
        self.schema = Some(schema.into());
        self
    }
}

/// Where an extraction writes, and in what encoding.
#[derive(Clone, Debug, Serialize)]
pub struct OutputSpec {
    /// The file to write, interpreted by the JVM.
    ///
    /// A path that is not valid UTF-8 fails to serialise — the request body is
    /// JSON, and there is no lossless way to put such a path in it.
    pub path: PathBuf,
    /// The charset name, as `java.nio.charset.Charset.forName` reads it.
    ///
    /// A name the JVM does not know is a `protocol` error from `JOB_START`, not
    /// a silent fallback: a script written in the wrong encoding is a file that
    /// looks fine until someone replays it.
    pub charset: String,
    /// The record separator.
    pub newline: Newline,
}

impl OutputSpec {
    /// UTF-8 with Unix line endings, which is what a SQL script should be
    /// unless the user says otherwise.
    pub fn new(path: impl Into<PathBuf>) -> Self {
        OutputSpec {
            path: path.into(),
            charset: "UTF-8".to_string(),
            newline: Newline::Lf,
        }
    }

    /// Sets the charset.
    pub fn with_charset(mut self, charset: impl Into<String>) -> Self {
        self.charset = charset.into();
        self
    }

    /// Sets the record separator.
    pub fn with_newline(mut self, newline: Newline) -> Self {
        self.newline = newline;
        self
    }
}

/// The record separator of an extracted file.
///
/// It terminates records only. A line break *inside* a value is data and is
/// written through untouched, so a CSV file really can hold both spellings —
/// rewriting the one in the data would be data loss.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
pub enum Newline {
    /// `\n`. The default, and the only sensible one for a file that will be
    /// read back by a SQL client.
    #[default]
    #[serde(rename = "\n")]
    Lf,
    /// `\r\n`, for a file destined for a Windows editor that has not caught up.
    #[serde(rename = "\r\n")]
    Crlf,
}

/// Whether and how an extraction writes the schema.
#[derive(Clone, Copy, Debug, Default, Serialize)]
pub struct DdlOptions {
    /// Whether to write DDL at all. Off by default.
    pub include: bool,
    /// Whether to precede the `CREATE`s with `DROP`s, in reverse object order.
    ///
    /// `IF EXISTS` is used on the products that have it; Oracle and Db2 do not,
    /// so there the statement fails on a missing table and the script has to be
    /// run past that error. Constraints are not dropped, so a cyclic schema
    /// still needs a hand.
    pub include_drop: bool,
    /// Where foreign keys go.
    pub constraints: Constraints,
}

impl DdlOptions {
    /// DDL included, with the defaults: no `DROP`s, foreign keys moved to
    /// trailing `ALTER`s.
    pub fn included() -> Self {
        DdlOptions {
            include: true,
            ..DdlOptions::default()
        }
    }

    /// Sets whether `DROP` statements precede the `CREATE`s.
    pub fn with_drop(mut self, include_drop: bool) -> Self {
        self.include_drop = include_drop;
        self
    }

    /// Sets where foreign keys go.
    pub fn with_constraints(mut self, constraints: Constraints) -> Self {
        self.constraints = constraints;
        self
    }
}

/// Where an extraction puts foreign keys — and, as a consequence, which DDL
/// layer it uses.
///
/// The two are one decision, not two, and that is worth knowing before
/// choosing: pulling a foreign key out of a server's own `CREATE` text (MySQL's
/// `SHOW CREATE TABLE`) would mean parsing vendor SQL, so
/// [`Constraints::Alter`] forces the reconstructed path
/// ([`DdlSource::Metadata`]) instead.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Constraints {
    /// Every `CREATE` first, then every foreign key as `ALTER TABLE … ADD
    /// CONSTRAINT`. The default, because two tables that reference each other
    /// cannot be created in any order at all and such schemas exist.
    ///
    /// The price is the reconstructed DDL's known blind spots — `CHECK`
    /// constraints, storage clauses, triggers, partitioning — which is the
    /// trade a replayable script makes.
    #[default]
    Alter,
    /// Keys inline, native DDL first, like the DDL an explorer pane shows.
    /// Faithful to the server, and not replayable against a cycle.
    Inline,
}

/// Whether and how an extraction writes the rows.
#[derive(Clone, Debug, Serialize)]
pub struct DataOptions {
    /// Whether to write rows at all. Off by default.
    pub include: bool,
    /// The row format.
    pub mode: DataMode,
    /// [`DataMode::Template`] only: the template file, resolved by the JVM.
    ///
    /// The bridge has no idea where a configuration directory is, so the caller
    /// resolves `templates/<name>` before asking. Required by that mode and
    /// ignored by the others.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub template_path: Option<PathBuf>,
    /// How many rows one `INSERT` carries.
    ///
    /// `1` is the default and the only portable value: Oracle has no multi-row
    /// `VALUES` clause, so the bridge clamps this to `1` there rather than
    /// writing a file that cannot run.
    pub insert_batch_rows: u32,
    /// A `WHERE` clause, without the keyword.
    ///
    /// Valid only when [`ExtractSpec::objects`] holds exactly one entry — a
    /// predicate names columns, and columns belong to one table. The bridge
    /// rejects it otherwise instead of quietly emptying the other tables.
    #[serde(rename = "where", skip_serializing_if = "Option::is_none")]
    pub where_clause: Option<String>,
}

impl Default for DataOptions {
    /// No rows, and the batch size the bridge would have used anyway.
    ///
    /// Written by hand rather than derived: a derived `0` would travel on the
    /// wire as a row count nobody meant, and the bridge would silently clamp it
    /// back to one.
    fn default() -> Self {
        DataOptions {
            include: false,
            mode: DataMode::Insert,
            template_path: None,
            insert_batch_rows: 1,
            where_clause: None,
        }
    }
}

impl DataOptions {
    /// Rows included, in the given format, one row per statement.
    pub fn included(mode: DataMode) -> Self {
        DataOptions {
            include: true,
            mode,
            ..DataOptions::default()
        }
    }

    /// Sets the template file. Only [`DataMode::Template`] reads it.
    pub fn with_template_path(mut self, path: impl Into<PathBuf>) -> Self {
        self.template_path = Some(path.into());
        self
    }

    /// Sets how many rows one `INSERT` carries.
    pub fn with_insert_batch_rows(mut self, rows: u32) -> Self {
        self.insert_batch_rows = rows;
        self
    }

    /// Sets the `WHERE` clause, without the keyword.
    pub fn with_where(mut self, predicate: impl Into<String>) -> Self {
        self.where_clause = Some(predicate.into());
        self
    }
}

/// The row format of an extraction.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum DataMode {
    /// `INSERT` statements, quoted and escaped for the product the rows came
    /// from. The default.
    #[default]
    Insert,
    /// RFC 4180 CSV with a header row.
    ///
    /// NULL is an empty unquoted field and the empty string is `""` — the
    /// `COPY … CSV` convention, and the only way plain CSV can tell the two
    /// apart.
    Csv,
    /// One rendering of [`DataOptions::template_path`] per row, through the
    /// engine inherited from jdbgen.
    Template,
}

/// A DB-to-DB transfer: the request body of `JOB_START` with `kind:
/// "transfer"` (architecture document, §6).
///
/// `JOB_START` is called **on the source session**; the target is named by
/// handle in [`target_session`](TransferSpec::target_session). The rows go
/// source `ResultSet` → target `PreparedStatement.addBatch` → `executeBatch`
/// entirely inside the JVM, which is the whole point of the data plane: not one
/// of them crosses the JNI boundary. Binding is `getObject`/`setObject`, so
/// type coercion is the target driver's job, and an exotic value (an array, a
/// vendor struct) that will not make the trip is a known edge — that row takes
/// the [`on_error`](TransferSpec::on_error) policy.
///
/// **Both sessions are locked for the whole stream**, taken in ascending
/// [`Session::handle`](crate::Session::handle) order so two transfers cannot
/// deadlock against each other. A transfer into the session it reads from is
/// safe — the bridge's lock is reentrant. An
/// [`execute`](crate::Session::execute) on either session waits for the
/// duration, which is why a UI that has to keep querying opens a third session,
/// exactly as it does during an extraction.
///
/// Nothing here is validated in Rust. A malformed spec is rejected
/// synchronously by [`Session::start_transfer`](crate::Session::start_transfer)
/// — but only what can be judged without running anything. An error that
/// depends on the shape of the source result set, such as a
/// [`ColumnMapping::from`] naming a column the query does not return, is only
/// knowable once the query runs and therefore arrives as an early
/// [`failed`](crate::JobState::Failed) job rather than as a rejection.
///
/// # Progress
///
/// [`phase`](crate::JobProgress::phase) walks `"starting"` → `"transfer"` →
/// `"done"`. [`bytes`](crate::JobProgress::bytes) stays `0`: there is no file.
/// [`rows_total`](crate::JobProgress::rows_total) is `None` for the same reason
/// an extraction's is — no `COUNT(*)` is run up front.
///
/// ```
/// use rudbman_jdbc::{ObjectRef, TransferMode, TransferSpec};
///
/// # let target_handle = 1i64;
/// let spec = TransferSpec::new(
///     "select id, name from orders",
///     target_handle,
///     ObjectRef::new("ORDERS").with_schema("APP"),
/// )
/// .with_mode(TransferMode::TruncateInsert);
/// ```
#[derive(Clone, Debug, Serialize)]
pub struct TransferSpec {
    /// Always `"transfer"`.
    kind: &'static str,
    /// The query to run on the source session. Its result set is the input.
    pub source_sql: String,
    /// The target session's handle, as
    /// [`Session::handle`](crate::Session::handle) reports it.
    ///
    /// A handle the bridge does not know is a synchronous rejection. The target
    /// session must outlive the job: closing it cancels every job that uses it
    /// from either end, source or target.
    pub target_session: i64,
    /// The table the rows are written into, on the target session.
    pub target_table: ObjectRef,
    /// What writing a row means.
    pub mode: TransferMode,
    /// How many rows one `addBatch` run carries before `executeBatch`.
    pub batch_size: u32,
    /// How many rows between commits on the target; `0` commits once at the
    /// end.
    ///
    /// The target's auto-commit is turned off for the transfer and restored
    /// afterwards. A failure or a cancel rolls back the uncommitted tail, and
    /// **the rows committed before it stay** — [`rows_done`] says how many, so a
    /// resume can be built on it.
    ///
    /// [`rows_done`]: crate::JobProgress::rows_done
    pub commit_every: u64,
    /// Which source column feeds which target column.
    ///
    /// Empty — and then absent from the wire — means the source result set's own
    /// column names are used as the target column names, quoted by the target
    /// dialect's rules.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub column_map: Vec<ColumnMapping>,
    /// What a row that will not go in does to the job.
    pub on_error: OnError,
}

impl TransferSpec {
    /// A transfer of `source_sql`'s rows into `target_table` on
    /// `target_session`, inserting, in batches of 500, committing every 10 000
    /// rows, aborting on the first bad row.
    pub fn new(
        source_sql: impl Into<String>,
        target_session: i64,
        target_table: ObjectRef,
    ) -> Self {
        TransferSpec {
            kind: "transfer",
            source_sql: source_sql.into(),
            target_session,
            target_table,
            mode: TransferMode::default(),
            batch_size: 500,
            commit_every: 10_000,
            column_map: Vec::new(),
            on_error: OnError::default(),
        }
    }

    /// Sets what writing a row means.
    pub fn with_mode(mut self, mode: TransferMode) -> Self {
        self.mode = mode;
        self
    }

    /// Sets how many rows one batch carries.
    pub fn with_batch_size(mut self, rows: u32) -> Self {
        self.batch_size = rows;
        self
    }

    /// Sets how many rows pass between commits; `0` commits once at the end.
    pub fn with_commit_every(mut self, rows: u64) -> Self {
        self.commit_every = rows;
        self
    }

    /// Adds one column mapping.
    pub fn with_column(mut self, from: impl Into<String>, to: impl Into<String>) -> Self {
        self.column_map.push(ColumnMapping::new(from, to));
        self
    }

    /// Sets the whole column map, replacing whatever was there. Empty restores
    /// "use the source's own column names".
    pub fn with_column_map(mut self, map: impl IntoIterator<Item = ColumnMapping>) -> Self {
        self.column_map = map.into_iter().collect();
        self
    }

    /// Sets what a row that will not go in does to the job.
    pub fn with_on_error(mut self, on_error: OnError) -> Self {
        self.on_error = on_error;
        self
    }
}

/// What a transfer does with each row it has read.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TransferMode {
    /// Plain `INSERT`. The default, and the only mode that needs nothing of the
    /// target beyond the columns.
    #[default]
    Insert,
    /// Insert, or update the row already there.
    ///
    /// The conflict key comes from the target table's primary key metadata, so
    /// **a target without a primary key is a synchronous rejection** rather
    /// than a job that fails later. The statement is dialect-specific —
    /// `ON CONFLICT … DO UPDATE` on PostgreSQL and SQLite, `ON DUPLICATE KEY
    /// UPDATE` on MySQL and MariaDB, `MERGE` on H2, Oracle, SQL Server and Db2
    /// — and a product the bridge does not recognise is rejected too: there is
    /// no portable upsert, and a quietly wrong statement is worse than a
    /// refusal.
    Upsert,
    /// Empty the target table, then insert.
    ///
    /// **The emptying is `DELETE FROM`, not `TRUNCATE`.** `TRUNCATE` differs
    /// per product in syntax, privileges and whether it can be rolled back at
    /// all; `DELETE` means the same thing everywhere and rolls back with the
    /// rest of the transfer, so a failed run does not leave the target empty.
    TruncateInsert,
}

/// One source column wired to one target column.
#[derive(Clone, Debug, Serialize)]
pub struct ColumnMapping {
    /// The column name in the source result set.
    ///
    /// A name the query does not return cannot be caught before the query runs,
    /// so it surfaces as a [`failed`](crate::JobState::Failed) job in the first
    /// moments of the transfer, not as a rejected start.
    pub from: String,
    /// The column name in the target table.
    pub to: String,
}

impl ColumnMapping {
    /// A mapping from one name to another.
    pub fn new(from: impl Into<String>, to: impl Into<String>) -> Self {
        ColumnMapping {
            from: from.into(),
            to: to.into(),
        }
    }
}

/// What a transfer does about a row the target will not take.
///
/// Whichever policy drops the row, the count of dropped rows is reported as
/// [`rows_skipped`](crate::JobProgress::rows_skipped).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum OnError {
    /// The first bad row fails the job. The default: a half-copied table nobody
    /// was told about is the worse outcome.
    #[default]
    Abort,
    /// Drop the row and count it, silently.
    Skip,
    /// Drop the row, count it, and record why.
    ///
    /// The errors land in [`JobProgress::errors`](crate::JobProgress::errors),
    /// **capped at 100**; beyond that they are only counted. A job where a
    /// million rows fail cannot carry a million error envelopes across JNI.
    Log,
}

/// A backup: the request body of `JOB_START` with `kind: "backup"`
/// (architecture document, §6).
///
/// A backup is **an extraction with no object list**: the bridge enumerates the
/// `TABLE`-typed tables of [`scope`](BackupSpec::scope), sorted by name, and
/// writes them through the same core — every `CREATE`, then every foreign-key
/// `ALTER`, then the rows. Views and routines are not written; the goal is a
/// replayable data backup, and a scope of one schema is the unit a user
/// actually restores.
///
/// The row format is `INSERT` only, with no choice to make. Several tables share
/// one file: CSV has no table boundary in it, and a template means something
/// different per table. That job is [`ExtractSpec`]'s.
///
/// Phases, cancellation and the partial file left behind by a cancel are an
/// extraction's exactly.
///
/// ```
/// use rudbman_jdbc::{BackupDataOptions, BackupSpec, Compression, DdlOptions};
///
/// let spec = BackupSpec::new("/tmp/app-backup.sql.gz")
///     .with_schema("APP")
///     .with_compress(Compression::Gzip)
///     .with_ddl(DdlOptions::included())
///     .with_data(BackupDataOptions::included());
/// ```
#[derive(Clone, Debug, Serialize)]
pub struct BackupSpec {
    /// Always `"backup"`.
    kind: &'static str,
    /// Which catalog and schema to enumerate.
    pub scope: ScopeRef,
    /// Where the file goes and how it is encoded.
    ///
    /// The charset applies to the text before compression, so a gzip backup is
    /// still a file in the charset that was asked for once it is unpacked.
    pub output: OutputSpec,
    /// Whether the output stream is wrapped in gzip.
    pub compress: Compression,
    /// Whether and how the schema is written.
    pub ddl: DdlOptions,
    /// Whether and how the rows are written.
    pub data: BackupDataOptions,
}

impl BackupSpec {
    /// A backup of the connection's current catalog and schema, writing to
    /// `path`, uncompressed and — until something is switched on — holding
    /// neither schema nor rows.
    ///
    /// The path is resolved by the JVM, on the machine the JVM runs on. Pass an
    /// absolute one.
    pub fn new(path: impl Into<PathBuf>) -> Self {
        BackupSpec {
            kind: "backup",
            scope: ScopeRef::default(),
            output: OutputSpec::new(path),
            compress: Compression::default(),
            ddl: DdlOptions::default(),
            data: BackupDataOptions::default(),
        }
    }

    /// Sets the whole scope, replacing whatever was there.
    pub fn with_scope(mut self, scope: ScopeRef) -> Self {
        self.scope = scope;
        self
    }

    /// Sets the exact catalog name of the scope.
    pub fn with_catalog(mut self, catalog: impl Into<String>) -> Self {
        self.scope.catalog = Some(catalog.into());
        self
    }

    /// Sets the exact schema name of the scope.
    pub fn with_schema(mut self, schema: impl Into<String>) -> Self {
        self.scope.schema = Some(schema.into());
        self
    }

    /// Sets the output path, encoding and record separator.
    pub fn with_output(mut self, output: OutputSpec) -> Self {
        self.output = output;
        self
    }

    /// Sets the compression.
    pub fn with_compress(mut self, compress: Compression) -> Self {
        self.compress = compress;
        self
    }

    /// Sets the DDL options.
    pub fn with_ddl(mut self, ddl: DdlOptions) -> Self {
        self.ddl = ddl;
        self
    }

    /// Sets the data options.
    pub fn with_data(mut self, data: BackupDataOptions) -> Self {
        self.data = data;
        self
    }
}

/// The catalog and schema a backup enumerates.
///
/// `None` for either means "wherever the connection is pointed", the same
/// reading [`ObjectRef`] and [`DescribeRequest`] give them — and, as there,
/// absent and null are different things on the wire, so an unset member is not
/// sent at all.
#[derive(Clone, Debug, Default, Serialize)]
pub struct ScopeRef {
    /// Exact catalog name.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub catalog: Option<String>,
    /// Exact schema name.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub schema: Option<String>,
}

impl ScopeRef {
    /// The connection's current catalog and schema.
    pub fn new() -> Self {
        ScopeRef::default()
    }

    /// Sets the exact catalog name.
    pub fn with_catalog(mut self, catalog: impl Into<String>) -> Self {
        self.catalog = Some(catalog.into());
        self
    }

    /// Sets the exact schema name.
    pub fn with_schema(mut self, schema: impl Into<String>) -> Self {
        self.schema = Some(schema.into());
        self
    }
}

/// How a backup file is compressed.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Compression {
    /// Plain text. The default.
    #[default]
    None,
    /// The output stream is wrapped in gzip.
    ///
    /// [`JobProgress::bytes`](crate::JobProgress::bytes) then counts the bytes
    /// written to the file — **after** compression — so it still matches the
    /// file's size on disc when the job ends.
    Gzip,
}

/// Whether and how a backup writes the rows.
///
/// Deliberately not [`DataOptions`]: a backup has no `mode` to choose and no
/// `where` to apply, because the file holds many tables. Sharing the type would
/// mean two members that are silently ignored here.
#[derive(Clone, Copy, Debug, Serialize)]
pub struct BackupDataOptions {
    /// Whether to write rows at all. Off by default, which makes a
    /// schema-only backup.
    pub include: bool,
    /// How many rows one `INSERT` carries.
    ///
    /// `1` is the default and the only portable value; Oracle has no multi-row
    /// `VALUES` clause and the bridge clamps it back to `1` there.
    pub insert_batch_rows: u32,
}

impl Default for BackupDataOptions {
    /// No rows, and the batch size the bridge would have used anyway.
    ///
    /// Written by hand for [`DataOptions`]'s reason: a derived `0` would travel
    /// as a row count nobody meant, and the bridge would silently clamp it back
    /// to one.
    fn default() -> Self {
        BackupDataOptions {
            include: false,
            insert_batch_rows: 1,
        }
    }
}

impl BackupDataOptions {
    /// Rows included, one row per `INSERT`.
    pub fn included() -> Self {
        BackupDataOptions {
            include: true,
            ..BackupDataOptions::default()
        }
    }

    /// Sets how many rows one `INSERT` carries.
    pub fn with_insert_batch_rows(mut self, rows: u32) -> Self {
        self.insert_batch_rows = rows;
        self
    }
}

/// Placeholder rendered in place of a secret.
struct Redacted;

impl fmt::Debug for Redacted {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("<redacted>")
    }
}

/// A JDBC URL with any embedded credentials and parameter values replaced.
///
/// Mirrors the rules `rudbman-core` applies to a stored profile — that
/// implementation is private to its crate, and duplicating twenty lines is
/// cheaper than publishing masking as API:
///
/// * everything after the first `?` or `;` is split on `&` and `;`, and the
///   value of every `key=value` is replaced while the key survives. Values are
///   not filtered by name, because one missed spelling of `password` is a
///   leaked credential and a hidden `ssl=true` is an inconvenience;
/// * an `@` means credentials precede it, and everything from the last `:`
///   before it is replaced. That one rule covers both `//user:pass@host` (the
///   user name survives) and Oracle's `thin:user/pass@//host` (the whole
///   credential goes). An empty span, as in `thin:@//host`, is left alone.
struct MaskedUrl<'a>(&'a str);

impl fmt::Debug for MaskedUrl<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let (base, params) = match self.0.find(['?', ';']) {
            Some(index) => (&self.0[..index], Some(&self.0[index..])),
            None => (self.0, None),
        };

        f.write_str("\"")?;
        match base.find('@') {
            // Byte indices are safe: `@` and `:` are ASCII, so a split at one of
            // them never lands inside a multi-byte character.
            Some(at) => {
                let start = base[..at].rfind(':').map_or(0, |colon| colon + 1);
                if start == at {
                    f.write_str(base)?;
                } else {
                    write!(f, "{}<redacted>{}", &base[..start], &base[at..])?;
                }
            }
            None => f.write_str(base)?,
        }

        if let Some(params) = params {
            // The leading `?` or `;` and every separator between parameters are
            // kept as they were: the shape of the URL is what says which driver
            // it belongs to, and that has to survive the masking.
            f.write_str(&params[..1])?;
            let mut rest = &params[1..];
            loop {
                let (token, separator, tail) = match rest.find(['&', ';']) {
                    Some(index) => (
                        &rest[..index],
                        Some(&rest[index..index + 1]),
                        &rest[index + 1..],
                    ),
                    None => (rest, None, ""),
                };
                match token.find('=') {
                    Some(equals) => write!(f, "{}<redacted>", &token[..=equals])?,
                    // A token with no `=` carries no value to hide.
                    None => f.write_str(token)?,
                }
                let Some(separator) = separator else { break };
                f.write_str(separator)?;
                rest = tail;
            }
        }
        f.write_str("\"")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bare_scalars_stay_bare_and_everything_else_is_tagged() {
        let json = serde_json::to_string(&vec![
            Param::I64(42),
            Param::Str("text".into()),
            Param::Bool(true),
            Param::Null,
            Param::Decimal("123456789012.12345678".into()),
            Param::Timestamp("2026-08-04 09:30:00".into()),
            Param::Bytes(vec![0xde, 0xad, 0xbe, 0xef]),
        ])
        .expect("serialises");
        assert_eq!(
            json,
            r#"[42,"text",true,null,"#.to_string()
                + r#"{"type":"decimal","value":"123456789012.12345678"},"#
                + r#"{"type":"timestamp","value":"2026-08-04 09:30:00"},"#
                + r#"{"type":"bytes","value":"3q2+7w=="}]"#
        );
    }

    #[test]
    fn a_whole_numbered_double_stays_a_double() {
        // Bare, this would serialise as `3` and be bound with setLong.
        let json = serde_json::to_string(&Param::F64(3.0)).expect("serialises");
        assert_eq!(json, r#"{"type":"f64","value":3.0}"#);
    }

    #[test]
    fn the_ddl_source_words_are_the_ones_the_bridge_accepts() {
        // The bridge rejects anything but these three by name, so a rename here
        // is a protocol break rather than a refactor.
        assert_eq!(
            serde_json::to_string(&DdlSource::Auto).expect("serialises"),
            r#""auto""#
        );
        assert_eq!(
            serde_json::to_string(&DdlSource::Native).expect("serialises"),
            r#""native""#
        );
        assert_eq!(
            serde_json::to_string(&DdlSource::Metadata).expect("serialises"),
            r#""metadata""#
        );
        assert_eq!(DdlSource::default(), DdlSource::Auto);
    }

    #[test]
    fn a_describe_request_only_sends_what_was_set() {
        // Absent and null are not the same thing to the bridge: a `schema` of
        // null is "any schema", and sending one where the caller said nothing
        // would silently widen the query.
        let json = serde_json::to_string(&DescribeRequest::new("tables").with_schema("APP"))
            .expect("serialises");
        assert_eq!(json, r#"{"kind":"tables","schema":"APP"}"#);
    }

    #[test]
    fn an_extract_spec_serialises_to_the_documented_wire_shape() {
        // The wire pin for architecture document §6. Every name here is read by
        // `ExtractJob`'s constructor by hand, so a rename on either side is a
        // protocol break that only a comparison like this one catches.
        let spec = ExtractSpec::new("/tmp/out.sql")
            .with_object(ObjectRef::new("T").with_schema("PUBLIC"))
            .with_ddl(DdlOptions::included())
            .with_data(DataOptions::included(DataMode::Insert));
        assert_eq!(
            serde_json::to_string(&spec).expect("serialises"),
            r#"{"kind":"extract","objects":[{"schema":"PUBLIC","name":"T"}],"#.to_string()
                + r#""output":{"path":"/tmp/out.sql","charset":"UTF-8","newline":"\n"},"#
                + r#""ddl":{"include":true,"include_drop":false,"constraints":"alter"},"#
                // No `template_path` and no `where`: absent leaves the bridge's
                // own defaults in place, and a null would not.
                + r#""data":{"include":true,"mode":"insert","insert_batch_rows":1}}"#
        );
    }

    #[test]
    fn the_optional_members_of_an_extract_spec_appear_only_once_set() {
        let spec = ExtractSpec::new("/tmp/out.csv")
            .with_object(ObjectRef::new("T").with_catalog("APP").with_schema("S"))
            .with_output(
                OutputSpec::new("/tmp/out.csv")
                    .with_charset("EUC-KR")
                    .with_newline(Newline::Crlf),
            )
            .with_ddl(
                DdlOptions::included()
                    .with_drop(true)
                    .with_constraints(Constraints::Inline),
            )
            .with_data(
                DataOptions::included(DataMode::Template)
                    .with_template_path("/etc/rudbman/templates/model.tpl")
                    .with_insert_batch_rows(50)
                    .with_where("id > 10"),
            );
        let json = serde_json::to_string(&spec).expect("serialises");
        assert!(
            json.contains(r#""catalog":"APP","schema":"S","name":"T""#),
            "{json}"
        );
        assert!(json.contains(r#""charset":"EUC-KR""#), "{json}");
        // The record separator travels as the characters themselves, not as a
        // name: the bridge compares against "\n" and "\r\n" and takes nothing
        // else.
        assert!(json.contains(r#""newline":"\r\n""#), "{json}");
        assert!(json.contains(r#""constraints":"inline""#), "{json}");
        assert!(json.contains(r#""include_drop":true"#), "{json}");
        assert!(json.contains(r#""mode":"template""#), "{json}");
        assert!(
            json.contains(r#""template_path":"/etc/rudbman/templates/model.tpl""#),
            "{json}"
        );
        assert!(json.contains(r#""insert_batch_rows":50"#), "{json}");
        // `where` is a Rust keyword and the wire name all the same.
        assert!(json.contains(r#""where":"id > 10""#), "{json}");
    }

    #[test]
    fn a_transfer_spec_serialises_to_the_documented_wire_shape() {
        // The wire pin for architecture document §6. The bridge reads every one
        // of these names by hand, and the defaults are pinned too: 500 and
        // 10 000 are what the document says a caller who says nothing gets.
        let spec = TransferSpec::new(
            "select id, txt from t",
            7,
            ObjectRef::new("T").with_schema("PUBLIC"),
        );
        assert_eq!(
            serde_json::to_string(&spec).expect("serialises"),
            r#"{"kind":"transfer","source_sql":"select id, txt from t","#.to_string()
                + r#""target_session":7,"target_table":{"schema":"PUBLIC","name":"T"},"#
                + r#""mode":"insert","batch_size":500,"commit_every":10000,"#
                // No `column_map`: absent means "use the source result set's own
                // column names", which is not the same request as an empty map.
                + r#""on_error":"abort"}"#
        );
    }

    #[test]
    fn the_transfer_words_are_the_ones_the_bridge_accepts() {
        // `truncate_insert` is snake_case while every other enum on this wire is
        // one lowercase word, so it is the one a rename would quietly break.
        assert_eq!(
            serde_json::to_string(&TransferMode::Insert).expect("serialises"),
            r#""insert""#
        );
        assert_eq!(
            serde_json::to_string(&TransferMode::Upsert).expect("serialises"),
            r#""upsert""#
        );
        assert_eq!(
            serde_json::to_string(&TransferMode::TruncateInsert).expect("serialises"),
            r#""truncate_insert""#
        );
        assert_eq!(TransferMode::default(), TransferMode::Insert);

        assert_eq!(
            serde_json::to_string(&OnError::Abort).expect("serialises"),
            r#""abort""#
        );
        assert_eq!(
            serde_json::to_string(&OnError::Skip).expect("serialises"),
            r#""skip""#
        );
        assert_eq!(
            serde_json::to_string(&OnError::Log).expect("serialises"),
            r#""log""#
        );
        assert_eq!(OnError::default(), OnError::Abort);
    }

    #[test]
    fn a_column_map_appears_only_once_it_holds_something() {
        let spec = TransferSpec::new("select 1", 3, ObjectRef::new("T"))
            .with_mode(TransferMode::Upsert)
            .with_batch_size(1)
            .with_commit_every(0)
            .with_column("SRC_ID", "ID")
            .with_column("SRC_TXT", "TXT")
            .with_on_error(OnError::Log);
        assert_eq!(
            serde_json::to_string(&spec).expect("serialises"),
            r#"{"kind":"transfer","source_sql":"select 1","target_session":3,"#.to_string()
                // The target table names neither catalog nor schema, and neither
                // travels as a null.
                + r#""target_table":{"name":"T"},"mode":"upsert","batch_size":1,"#
                + r#""commit_every":0,"column_map":[{"from":"SRC_ID","to":"ID"},"#
                + r#"{"from":"SRC_TXT","to":"TXT"}],"on_error":"log"}"#
        );

        // And setting the map back to nothing takes it off the wire again.
        let cleared = spec.with_column_map([]);
        assert!(
            !serde_json::to_string(&cleared)
                .expect("serialises")
                .contains("column_map")
        );
    }

    #[test]
    fn a_backup_spec_serialises_to_the_documented_wire_shape() {
        // An unset scope is an empty object, not a pair of nulls: the bridge
        // reads absent as "wherever the connection is pointed".
        let spec = BackupSpec::new("/tmp/backup.sql");
        assert_eq!(
            serde_json::to_string(&spec).expect("serialises"),
            r#"{"kind":"backup","scope":{},"#.to_string()
                + r#""output":{"path":"/tmp/backup.sql","charset":"UTF-8","newline":"\n"},"#
                + r#""compress":"none","#
                + r#""ddl":{"include":false,"include_drop":false,"constraints":"alter"},"#
                // No `mode` and no `where`: a backup writes many tables to one
                // file and INSERT is the only format that survives that.
                + r#""data":{"include":false,"insert_batch_rows":1}}"#
        );
    }

    #[test]
    fn a_scoped_compressed_backup_sends_every_member_it_was_given() {
        let spec = BackupSpec::new("/tmp/app.sql.gz")
            .with_scope(ScopeRef::new().with_catalog("APP").with_schema("PUBLIC"))
            .with_compress(Compression::Gzip)
            .with_output(
                OutputSpec::new("/tmp/app.sql.gz")
                    .with_charset("EUC-KR")
                    .with_newline(Newline::Crlf),
            )
            .with_ddl(DdlOptions::included().with_drop(true))
            .with_data(BackupDataOptions::included().with_insert_batch_rows(100));
        assert_eq!(
            serde_json::to_string(&spec).expect("serialises"),
            r#"{"kind":"backup","scope":{"catalog":"APP","schema":"PUBLIC"},"#.to_string()
                + r#""output":{"path":"/tmp/app.sql.gz","charset":"EUC-KR","newline":"\r\n"},"#
                + r#""compress":"gzip","#
                + r#""ddl":{"include":true,"include_drop":true,"constraints":"alter"},"#
                + r#""data":{"include":true,"insert_batch_rows":100}}"#
        );

        // A scope with only a schema leaves the catalog off entirely.
        let json = serde_json::to_string(&BackupSpec::new("/tmp/s.sql").with_schema("APP"))
            .expect("serialises");
        assert!(json.contains(r#""scope":{"schema":"APP"}"#), "{json}");
        assert_eq!(Compression::default(), Compression::None);
    }

    #[test]
    fn base64_matches_the_reference_vectors() {
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64(b"foob"), "Zm9vYg==");
        assert_eq!(base64(b"fooba"), "Zm9vYmE=");
        assert_eq!(base64(b"foobar"), "Zm9vYmFy");
    }

    #[test]
    fn debug_hides_the_password_the_props_and_the_url_credentials() {
        let spec = ConnectionSpec::new("jdbc:postgresql://db:5432/app?password=hunter2", "org.pg")
            .with_credentials("alice", "hunter2")
            .with_prop("ApplicationName", "rudbman");
        let rendered = format!("{spec:?}");
        assert!(!rendered.contains("hunter2"), "{rendered}");
        assert!(rendered.contains("alice"), "the user name is not a secret");
        assert!(
            rendered.contains("ApplicationName"),
            "property keys survive: {rendered}"
        );
        assert!(
            rendered.contains("jdbc:postgresql://db:5432/app"),
            "{rendered}"
        );
    }

    #[test]
    fn debug_hides_credentials_in_every_url_shape_a_driver_accepts() {
        let cases = [
            (
                "jdbc:postgresql://alice:hunter2@db:5432/app",
                "jdbc:postgresql://alice:<redacted>@db:5432/app",
            ),
            // The separator is a slash, not a colon, so the fallback to the
            // scheme's own colon hides user name and password together.
            (
                "jdbc:oracle:thin:scott/tiger@//host:1521/orcl",
                "jdbc:oracle:thin:<redacted>@//host:1521/orcl",
            ),
            // Nothing between the colon and the `@`: printing <redacted> for an
            // empty span would be noise.
            (
                "jdbc:oracle:thin:@//host:1521/orcl",
                "jdbc:oracle:thin:@//host:1521/orcl",
            ),
            (
                "jdbc:sqlserver://host;user=sa;password=s3cret",
                "jdbc:sqlserver://host;user=<redacted>;password=<redacted>",
            ),
            // Most URLs carry no secret at all and must stay readable.
            ("jdbc:h2:mem:test", "jdbc:h2:mem:test"),
        ];
        for (url, expected) in cases {
            assert_eq!(format!("{:?}", MaskedUrl(url)), format!("{expected:?}"));
        }
    }
}
