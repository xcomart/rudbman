//! Response bodies: what the bridge answers with.
//!
//! The bridge serialises nulls explicitly, so a member the driver had nothing
//! to say about arrives as JSON `null` rather than missing. That is why the
//! `Option` fields here need no `#[serde(default)]` — an absent member would be
//! a protocol change, not a driver quirk, and should fail loudly.
//!
//! Everything the explorer, the grid and the status bar read comes from here.

use serde::Deserialize;

use crate::codec::{ColumnKind, Value};
use crate::error::BridgeError;

/// `java.sql.Types.REAL`.
///
/// The one JDBC type constant this crate needs by number: a `REAL` is 32-bit,
/// but it crosses the boundary widened into an `f64`, and printing it at full
/// double precision turns `0.1` into `0.10000000149011612`. See
/// [`ColumnInfo::is_single_precision`].
pub const SQL_TYPE_REAL: i32 = 7;

/// `PING` (`0x03`).
#[derive(Clone, Debug, Deserialize)]
pub struct Ping {
    /// Whether the connection answered.
    pub ok: bool,
    /// Round trip time in milliseconds, measured inside the JVM.
    pub elapsed_ms: i64,
}

/// `SESSION_INFO` (`0x04`): product, driver and capability facts.
///
/// Every member is optional because `DatabaseMetaData` is where drivers are at
/// their least reliable — several throw from methods the specification says they
/// must implement — and the bridge answers `null` for each one it could not get
/// rather than failing the whole call.
#[derive(Clone, Debug, Deserialize)]
#[non_exhaustive]
pub struct SessionInfo {
    /// The URL the session was opened with.
    pub url: Option<String>,
    /// The driver class the session was opened with.
    pub driver_class: Option<String>,
    /// `DatabaseMetaData.getDatabaseProductName`.
    pub product_name: Option<String>,
    /// `DatabaseMetaData.getDatabaseProductVersion`.
    pub product_version: Option<String>,
    /// Database major version.
    pub database_major: Option<i32>,
    /// Database minor version.
    pub database_minor: Option<i32>,
    /// Driver name.
    pub driver_name: Option<String>,
    /// Driver version.
    pub driver_version: Option<String>,
    /// JDBC specification major version the driver implements.
    pub jdbc_major: Option<i32>,
    /// JDBC specification minor version.
    pub jdbc_minor: Option<i32>,
    /// The user the connection authenticated as.
    pub user_name: Option<String>,
    /// Current catalog.
    pub catalog: Option<String>,
    /// Current schema.
    pub schema: Option<String>,
    /// Whether the connection is read-only.
    pub read_only: Option<bool>,
    /// Whether auto-commit is on.
    pub auto_commit: Option<bool>,
    /// `Connection.getTransactionIsolation`.
    pub transaction_isolation: Option<i32>,
    /// Identifier quote string, e.g. `"`.
    pub identifier_quote: Option<String>,
    /// Catalog separator, e.g. `.`.
    pub catalog_separator: Option<String>,
    /// What this product calls a catalog.
    pub catalog_term: Option<String>,
    /// What this product calls a schema.
    pub schema_term: Option<String>,
    /// What this product calls a stored procedure.
    pub procedure_term: Option<String>,
    /// The escape character for `LIKE` patterns in metadata calls.
    pub search_string_escape: Option<String>,
    /// Extra characters allowed in unquoted identifiers.
    pub extra_name_characters: Option<String>,
    /// Comma-separated keywords that are not SQL92 keywords.
    pub sql_keywords: Option<String>,
    /// Whether unquoted identifiers are folded to upper case.
    pub stores_upper_case_identifiers: Option<bool>,
    /// Whether unquoted identifiers are folded to lower case.
    pub stores_lower_case_identifiers: Option<bool>,
    /// Whether unquoted identifiers keep their case.
    pub stores_mixed_case_identifiers: Option<bool>,
    /// Whether quoted identifiers keep their case.
    pub supports_mixed_case_quoted_identifiers: Option<bool>,
    /// Whether the product supports transactions.
    pub supports_transactions: Option<bool>,
    /// Whether the product supports savepoints.
    pub supports_savepoints: Option<bool>,
    /// Whether the driver supports batch updates.
    pub supports_batch_updates: Option<bool>,
    /// Whether a table definition may name a schema.
    pub supports_schemas_in_table_definitions: Option<bool>,
    /// Whether a table definition may name a catalog.
    pub supports_catalogs_in_table_definitions: Option<bool>,
    /// Whether the product supports stored procedures.
    pub supports_stored_procedures: Option<bool>,
    /// Whether generated keys can be retrieved.
    pub supports_get_generated_keys: Option<bool>,
    /// Whether one statement can produce several result sets.
    pub supports_multiple_result_sets: Option<bool>,
    /// Default transaction isolation level.
    pub default_transaction_isolation: Option<i32>,
    /// Maximum statement length in characters, `0` when unknown or unlimited.
    pub max_statement_length: Option<i32>,
}

/// `CANCEL` (`0x24`).
#[derive(Clone, Debug, Deserialize)]
pub struct Cancelled {
    /// How many running statements a cancel was issued for.
    ///
    /// Zero on an idle session, which is not an error: nothing was running.
    pub cancelled: u32,
}

/// `PROBE_DRIVER` (`0x50`): the JDBC drivers a set of JARs offers.
///
/// Answers the one question a driver manager has when the user picks a JAR —
/// "what is the class name?" — without making them open the archive.
///
/// Two lists, because they come from two different places and disagree in
/// useful ways:
///
/// * `services` is what the JAR *declares* through
///   `META-INF/services/java.sql.Driver`. When it is there it is authoritative:
///   the vendor named its own entry point.
/// * `classes` is what a scan of the archive *found* — every non-inner class
///   that implements `java.sql.Driver`. It is a superset, and it routinely
///   includes internal or deprecated drivers a vendor would not want picked.
///
/// [`DriverProbe::recommended`] applies that preference.
///
/// The scan never runs a static initialiser (`Class.forName(…, false, …)`): a
/// driver's can open sockets or load native libraries, and looking at a file
/// must not do either.
#[derive(Clone, Debug, Deserialize)]
pub struct DriverProbe {
    /// Every `java.sql.Driver` implementation found by scanning the archives,
    /// in the order they were encountered.
    pub classes: Vec<String>,
    /// The classes declared in `META-INF/services/java.sql.Driver`.
    pub services: Vec<String>,
}

impl DriverProbe {
    /// The class to offer the user first: the declared service if there is one,
    /// otherwise the first class the scan found.
    ///
    /// `None` means the JARs contain no driver at all — which is not an error
    /// and is worth saying out loud in the UI, because the usual cause is a
    /// sources or javadoc archive picked by mistake.
    pub fn recommended(&self) -> Option<&str> {
        self.services
            .first()
            .or_else(|| self.classes.first())
            .map(String::as_str)
    }

    /// Whether nothing was found, by either route.
    pub fn is_empty(&self) -> bool {
        self.classes.is_empty() && self.services.is_empty()
    }
}

/// One result column's logical type, from the `EXECUTE` response.
///
/// This — not the batch's [`ColumnKind`] — is what presentation follows from.
/// The kind is transport: it says how the bytes were packed, and it changes
/// from batch to batch. The type below is stable for the life of the result.
#[derive(Clone, Debug, Deserialize)]
#[non_exhaustive]
pub struct ColumnInfo {
    /// One-based column index.
    pub index: i32,
    /// Column name.
    pub name: Option<String>,
    /// Column label — the `AS` alias when there is one.
    pub label: Option<String>,
    /// Source table, when the driver knows it.
    pub table: Option<String>,
    /// Source schema.
    pub schema: Option<String>,
    /// Source catalog.
    pub catalog: Option<String>,
    /// The `java.sql.Types` constant.
    #[serde(rename = "type")]
    pub sql_type: i32,
    /// The database's own name for the type, e.g. `NUMERIC`.
    pub type_name: Option<String>,
    /// The `java.sql.Types` constant's name, e.g. `DECIMAL`.
    pub jdbc_type: Option<String>,
    /// The Java class `ResultSet.getObject` would return.
    pub class_name: Option<String>,
    /// Precision, in digits or characters.
    pub precision: i32,
    /// Scale, for exact numeric types.
    pub scale: i32,
    /// The driver's suggested display width, in characters.
    pub display_size: i32,
    /// `ResultSetMetaData.isNullable`: 0 no, 1 yes, 2 unknown.
    pub nullable: i32,
    /// Whether the column is auto-incremented.
    pub auto_increment: bool,
    /// Whether the numeric type is signed.
    pub signed: bool,
    /// Whether the column is read-only.
    pub read_only: bool,
    /// The encoding a *full* batch of this column would use.
    ///
    /// A hint, and only a hint: any batch in which this column is entirely NULL
    /// arrives as [`ColumnKind::Nulls`] instead. Decode against the kind byte of
    /// the batch in hand, never against this.
    pub kind: u8,
}

impl ColumnInfo {
    /// The hinted physical encoding, or `None` when the bridge named a kind
    /// this version does not know.
    pub fn kind_hint(&self) -> Option<ColumnKind> {
        ColumnKind::from_byte(self.kind)
    }

    /// Whether the logical type is 32-bit `REAL`.
    ///
    /// The bridge sends `REAL` as an `F64`, because a batch has one float
    /// encoding. Nothing is lost — every `f32` is exactly representable as an
    /// `f64` — but the extra digits are noise from a widening, not precision the
    /// database has: a `REAL` holding `0.1` prints as `0.10000000149011612`
    /// unless it is narrowed back before rendering. [`Value::to_text`] does that.
    pub fn is_single_precision(&self) -> bool {
        self.sql_type == SQL_TYPE_REAL
    }

    /// The name to put in a column header: the label if there is one, else the
    /// name, else the one-based index.
    pub fn display_name(&self) -> String {
        self.label
            .as_deref()
            .filter(|label| !label.is_empty())
            .or(self.name.as_deref().filter(|name| !name.is_empty()))
            .map(str::to_string)
            .unwrap_or_else(|| self.index.to_string())
    }
}

/// The response to `EXECUTE` (`0x20`) and to `MORE_RESULTS` (`0x22`), which
/// have the same shape.
#[derive(Clone, Debug, Deserialize)]
#[non_exhaustive]
pub struct ExecuteResult {
    /// The cursor handle. Always non-zero, even for a statement that produced
    /// only a row count, so that `MORE_RESULTS` always has something to advance
    /// and `CLOSE_CURSOR` always has something to close.
    pub cursor: i64,
    /// The result columns; empty when the statement produced no result set.
    pub columns: Vec<ColumnInfo>,
    /// The update count, or `-1` when there was none.
    pub update_count: i64,
    /// Whether this result is a result set rather than an update count.
    pub has_result_set: bool,
    /// Whether `MORE_RESULTS` may still return something.
    ///
    /// **A hint, not a fact.** JDBC has no non-destructive lookahead: there is
    /// no way to learn whether another result exists without consuming the
    /// current one. Do not trust a single value — keep calling `MORE_RESULTS`
    /// until it answers `false`. Exhaustion is the three things together:
    /// this `false`, `update_count == -1`, and no columns.
    ///
    /// Named after the architecture document (§4.4); the wire member is
    /// `has_more`.
    #[serde(rename = "has_more", alias = "may_have_more")]
    pub may_have_more: bool,
}

impl ExecuteResult {
    /// Whether the statement is exhausted: no result set, no update count and
    /// no hint of anything further.
    pub fn is_exhausted(&self) -> bool {
        !self.may_have_more && self.update_count < 0 && self.columns.is_empty()
    }
}

/// The response to `DESCRIBE` (`0x10`).
///
/// Item shapes differ per kind and are documented in `bridge/README.md`. They
/// are left as JSON objects here on purpose: the explorer tree maps the two or
/// three members it needs per kind, and modelling fourteen metadata shapes in
/// Rust would be a second copy of the same table to keep in step.
#[derive(Clone, Debug, Deserialize)]
pub struct DescribeResult {
    /// The kind that was asked for, echoed back.
    pub kind: String,
    /// The items, with the bridge's fixed snake_case keys — not the driver's
    /// metadata labels, so the key names stay stable across drivers.
    pub items: Vec<serde_json::Map<String, serde_json::Value>>,
}

/// The response to `DESCRIBE` with `kind: "ddl"`: one table's `CREATE` text.
///
/// The one metadata kind that answers with a document instead of a list, which
/// is why it has [`Session::describe_ddl`](crate::Session::describe_ddl) to
/// itself. A one-element array would only have added an unwrap at every call
/// site.
#[derive(Clone, Debug, Deserialize)]
pub struct DdlResult {
    /// The statement text, ready to show. One `CREATE TABLE`, possibly followed
    /// by `CREATE INDEX` statements for indexes that do not merely back a
    /// declared key.
    pub ddl: String,
    /// Which layer produced it: `"native"` or `"metadata"`.
    ///
    /// Kept as a string rather than an enum so that a bridge which learns a
    /// third path does not fail to parse here. Use [`DdlResult::is_native`] and
    /// [`DdlResult::is_reconstructed`] instead of matching on the text.
    pub source: String,
}

impl DdlResult {
    /// Whether the server quoted its own DDL back.
    ///
    /// When this is true the text is authoritative — everything the server
    /// stores, including what JDBC metadata cannot see.
    pub fn is_native(&self) -> bool {
        self.source == "native"
    }

    /// Whether the text was reassembled from JDBC metadata.
    ///
    /// **Label it in the UI.** It is close enough to read and usually close
    /// enough to run, but `CHECK` constraints, triggers, partitioning,
    /// collations and generated-column expressions are not in JDBC metadata at
    /// all and are therefore not in this text either. It is not a migration
    /// artefact.
    pub fn is_reconstructed(&self) -> bool {
        self.source == "metadata"
    }
}

/// The response to `JOB_POLL` (`0x41`): one reading of a running job.
///
/// Every member is a snapshot taken without a lock, so a reading of a *running*
/// job may mix a counter from one instant with a phase from the next. That is
/// harmless for a progress bar, and the one reading that matters is exact: the
/// job writes every counter before it writes its terminal state, so a poll that
/// sees a terminal [`state`](JobProgress::state) sees the final numbers with it.
#[derive(Clone, Debug, Deserialize)]
#[non_exhaustive]
pub struct JobProgress {
    /// What the job is doing, or what it ended as.
    pub state: JobState,
    /// Rows processed so far.
    pub rows_done: u64,
    /// The total, when the caller asked for it to be counted up front.
    ///
    /// **Normally `None`**, because no `COUNT(*)` is run before the work: on a
    /// large table it costs as much as the extraction. A UI that wants a
    /// determinate progress bar has to ask for the count and pay for it.
    pub rows_total: Option<u64>,
    /// Bytes written to the output file.
    ///
    /// Lags by up to one 64KB buffer while the job runs — the count is read
    /// without flushing — and is exact once the state is terminal.
    pub bytes: u64,
    /// What the job is working on: `starting`, `ddl`, `data:<schema>.<table>`,
    /// then `done`.
    ///
    /// Free text meant for a status line. Do not branch on it: the table part
    /// is a display name, not a parseable qualified identifier.
    pub phase: String,
    /// Failures that did not stop the job, and the one that did.
    ///
    /// Full error envelopes, the same ones a failed call answers with, so a
    /// job's failure can be rendered by whatever already renders those. A
    /// `failed` state always leaves one here; a `cancelled` one often does,
    /// carrying the driver's account of the statement being aborted, which is
    /// the cancel working rather than a fault.
    pub errors: Vec<BridgeError>,
    /// Seconds remaining, when there is a row total to extrapolate from.
    ///
    /// `None` whenever it would be a guess — which, with no row total, is
    /// almost always.
    pub eta_s: Option<f64>,
}

impl JobProgress {
    /// Whether the job has finished, one way or another.
    ///
    /// **Stop polling when this is true.** The reading that reports a terminal
    /// state is also the one that retires the handle inside the bridge; a
    /// further poll is a `protocol` error. See [`Job::poll`](crate::Job::poll).
    pub fn is_terminal(&self) -> bool {
        self.state.is_terminal()
    }
}

/// What a job is doing.
///
/// A closed set on purpose, unlike [`BridgeErrorKind`](crate::BridgeErrorKind):
/// a state this version has not heard of would have to be guessed either
/// terminal — abandoning a running job — or running — polling a dead handle
/// forever. Failing to parse says out loud that the JAR is newer than the
/// crate.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum JobState {
    /// Still working.
    Running,
    /// Finished, having written everything that was asked for.
    Done,
    /// Stopped by a failure. [`JobProgress::errors`] says which.
    Failed,
    /// Stopped by a cancel. The partial output file is left where it is, on
    /// purpose: it is work the user may still want.
    Cancelled,
}

impl JobState {
    /// Whether this is an end state.
    pub fn is_terminal(self) -> bool {
        !matches!(self, JobState::Running)
    }
}

impl Value<'_> {
    /// Renders a cell as text, using the logical type to decide how.
    ///
    /// `None` means SQL NULL — the caller decides how a NULL looks, because a
    /// grid has to be able to tell it from the string `"NULL"`.
    ///
    /// The only interesting case is [`ColumnInfo::is_single_precision`]: a
    /// `REAL` arrives widened to `f64` and is narrowed back here, so `0.1`
    /// prints as `0.1`. Binary values become uppercase hex, and a LOB — whose
    /// body was deliberately left on the Java side — becomes a placeholder that
    /// a grid is expected to replace with something better.
    pub fn to_text(&self, column: &ColumnInfo) -> Option<String> {
        Some(match self {
            Value::Null => return None,
            Value::I64(value) => value.to_string(),
            Value::F64(value) => {
                if column.is_single_precision() {
                    (*value as f32).to_string()
                } else {
                    value.to_string()
                }
            }
            Value::Bool(value) => value.to_string(),
            Value::Str(text) => (*text).to_string(),
            Value::Bin(bytes) => {
                let mut out = String::with_capacity(bytes.len() * 2);
                for byte in *bytes {
                    out.push_str(&format!("{byte:02X}"));
                }
                out
            }
            Value::Lob { id, size } => match size {
                Some(size) => format!("<lob {id}, {size}>"),
                None => format!("<lob {id}>"),
            },
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn column(sql_type: i32) -> ColumnInfo {
        serde_json::from_str(&format!(
            r#"{{"index":1,"name":"C","label":"C","table":null,"schema":null,"catalog":null,
                 "type":{sql_type},"type_name":"T","jdbc_type":"T","class_name":null,
                 "precision":0,"scale":0,"display_size":0,"nullable":2,
                 "auto_increment":false,"signed":true,"read_only":false,"kind":2}}"#
        ))
        .expect("parses")
    }

    #[test]
    fn a_real_is_rendered_at_the_precision_it_actually_has() {
        let real = column(SQL_TYPE_REAL);
        // What a REAL holding 0.1 looks like after the widening to f64.
        let widened = 0.1f32 as f64;
        assert_eq!(
            Value::F64(widened).to_text(&real).as_deref(),
            Some("0.1"),
            "a REAL must not print the noise of its widening"
        );

        let double = column(8); // java.sql.Types.DOUBLE
        assert_eq!(
            Value::F64(widened).to_text(&double).as_deref(),
            Some("0.10000000149011612"),
            "a DOUBLE is not narrowed: those digits are the value"
        );
    }

    #[test]
    fn null_renders_as_nothing_rather_than_as_a_word() {
        assert_eq!(Value::Null.to_text(&column(8)), None);
    }

    #[test]
    fn binary_renders_as_hex() {
        assert_eq!(
            Value::Bin(&[0xde, 0xad, 0x00])
                .to_text(&column(-3))
                .as_deref(),
            Some("DEAD00")
        );
    }

    #[test]
    fn the_wire_name_of_the_lookahead_hint_is_has_more() {
        let result: ExecuteResult = serde_json::from_str(
            r#"{"cursor":7,"columns":[],"update_count":-1,
                "has_result_set":false,"has_more":false}"#,
        )
        .expect("parses");
        assert!(!result.may_have_more);
        assert!(result.is_exhausted());
    }

    #[test]
    fn a_running_job_reports_the_two_unknowns_as_null() {
        let progress: JobProgress = serde_json::from_str(
            r#"{"state":"running","rows_done":1024,"rows_total":null,"bytes":65536,
                "phase":"data:PUBLIC.T","errors":[],"eta_s":null}"#,
        )
        .expect("parses");
        assert_eq!(progress.state, JobState::Running);
        assert!(!progress.is_terminal());
        // Not zero: "no idea how many" and "none yet" are different answers and
        // a progress bar has to be able to tell them apart.
        assert_eq!(progress.rows_total, None);
        assert_eq!(progress.eta_s, None);
    }

    #[test]
    fn a_failed_job_carries_whole_error_envelopes() {
        let progress: JobProgress = serde_json::from_str(
            r#"{"state":"failed","rows_done":7,"rows_total":10,"bytes":128,"phase":"data:S.T",
                "errors":[{"kind":"sql","sql_state":"42S04","vendor_code":42102,
                           "message":"table not found","causes":["boom"],"stack":null}],
                "eta_s":3}"#,
        )
        .expect("parses");
        assert!(progress.is_terminal());
        assert_eq!(progress.rows_total, Some(10));
        // An integer on the wire: a JSON number is a JSON number, and the
        // seconds are a float here because that is what an estimate is.
        assert_eq!(progress.eta_s, Some(3.0));
        assert_eq!(progress.errors.len(), 1);
        assert_eq!(progress.errors[0].sql_state_class(), Some("42"));
        assert_eq!(progress.errors[0].causes, ["boom"]);
    }

    #[test]
    fn every_terminal_state_is_terminal_and_running_is_not() {
        for (word, state) in [
            ("running", JobState::Running),
            ("done", JobState::Done),
            ("failed", JobState::Failed),
            ("cancelled", JobState::Cancelled),
        ] {
            // The wire spelling is the bridge's; a rename here would silently
            // turn a finished job into one that is polled forever.
            assert_eq!(
                serde_json::from_str::<JobState>(&format!("\"{word}\"")).expect("parses"),
                state
            );
            assert_eq!(state.is_terminal(), word != "running");
        }
        assert!(
            serde_json::from_str::<JobState>("\"paused\"").is_err(),
            "an unknown state has to fail loudly rather than be guessed"
        );
    }

    #[test]
    fn a_ddl_answer_says_which_layer_produced_it() {
        let native: DdlResult =
            serde_json::from_str(r#"{"kind":"ddl","ddl":"CREATE TABLE t()","source":"native"}"#)
                .expect("parses");
        assert!(native.is_native());
        assert!(!native.is_reconstructed());

        let reconstructed: DdlResult =
            serde_json::from_str(r#"{"kind":"ddl","ddl":"CREATE TABLE t()","source":"metadata"}"#)
                .expect("parses");
        assert!(reconstructed.is_reconstructed());
        assert!(!reconstructed.is_native());

        // A layer this version has not heard of still parses — the text is the
        // wire's to name, and failing here would break on a bridge upgrade.
        let future: DdlResult =
            serde_json::from_str(r#"{"kind":"ddl","ddl":"x","source":"catalogue"}"#)
                .expect("an unknown source is not a parse failure");
        assert!(!future.is_native() && !future.is_reconstructed());
    }
}
