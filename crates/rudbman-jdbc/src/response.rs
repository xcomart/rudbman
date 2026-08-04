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
}
