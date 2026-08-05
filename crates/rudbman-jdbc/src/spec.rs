//! Request bodies: what a session is opened with, and what a statement is
//! executed with.
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
