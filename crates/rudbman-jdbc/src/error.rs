//! Failures of the JNI layer, and the error envelope the Java bridge answers
//! with.
//!
//! Two kinds of failure meet here and they are worth keeping apart:
//!
//! * a [`BridgeError`] is the bridge telling us that *the database* said no —
//!   it crossed the boundary as a well-formed ERROR envelope (architecture
//!   document, §4.5) and carries a `SQLSTATE`, a vendor code and a cause chain;
//! * every other [`Error`] variant means the boundary itself misbehaved: the
//!   JVM would not start, a JNI call failed, the response was not a shape this
//!   version understands, or a worker thread died.
//!
//! The distinction is what the UI needs. The first kind is shown to the user as
//! a database error with the query that caused it; the second is a bug report.

use std::fmt;

use serde::Deserialize;

use crate::codec::CodecError;

/// Result alias for everything this crate returns.
pub type Result<T> = std::result::Result<T, Error>;

/// Anything that can go wrong between a Rust caller and the JDBC driver.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// The JVM could not be created, or the bridge class could not be found in
    /// it.
    #[error("cannot start the JVM: {0}")]
    JvmStart(String),

    /// A JNI call failed.
    ///
    /// Carried as text rather than as `jni::errors::Error`, because these
    /// values travel between threads through the worker's reply channel and the
    /// JNI error type is not guaranteed to be `Send + Sync`.
    #[error("JNI call failed: {0}")]
    Jni(String),

    /// The bridge answered with an ERROR envelope.
    #[error(transparent)]
    Bridge(#[from] BridgeError),

    /// A result batch could not be decoded.
    #[error(transparent)]
    Codec(#[from] CodecError),

    /// A response did not have the shape this version of the protocol expects.
    ///
    /// Always a version skew between this crate and the bridge JAR, never
    /// something the user did.
    #[error("malformed bridge response: {0}")]
    Protocol(String),

    /// A request body could not be encoded, or a response body could not be
    /// parsed.
    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),

    /// The session's worker thread has stopped, so no further command can be
    /// run on that connection.
    #[error("the session worker thread is gone; the session is no longer usable")]
    WorkerGone,

    /// A JNI call panicked inside the worker thread.
    ///
    /// The panic is caught at the worker boundary so that it takes the session
    /// down instead of the process (architecture document, §4.2). The session
    /// is dead afterwards.
    #[error("the session worker panicked: {0}")]
    WorkerPanic(String),
}

impl From<jni::errors::Error> for Error {
    fn from(error: jni::errors::Error) -> Self {
        Error::Jni(error.to_string())
    }
}

/// The category of a bridge failure — the `kind` member of the error envelope.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
#[non_exhaustive]
pub enum BridgeErrorKind {
    /// A `SQLException`: the database or the driver rejected the request.
    Sql,
    /// The driver itself is the problem — a missing class, an incomplete JAR, a
    /// linkage error, or a URL the driver says it does not understand.
    Driver,
    /// An I/O failure inside the bridge.
    Io,
    /// The request did not follow the protocol, or names an operation this
    /// build of the bridge does not implement. See
    /// [`BridgeError::is_not_implemented`].
    Protocol,
    /// A blocking call was interrupted.
    Interrupted,
    /// A bug in the bridge.
    Internal,
    /// A `kind` this version does not know, which means the JAR is newer than
    /// this crate.
    #[default]
    #[serde(other)]
    Unknown,
}

impl fmt::Display for BridgeErrorKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = match self {
            BridgeErrorKind::Sql => "sql",
            BridgeErrorKind::Driver => "driver",
            BridgeErrorKind::Io => "io",
            BridgeErrorKind::Protocol => "protocol",
            BridgeErrorKind::Interrupted => "interrupted",
            BridgeErrorKind::Internal => "internal",
            BridgeErrorKind::Unknown => "unknown",
        };
        f.write_str(name)
    }
}

/// The ERROR envelope of architecture document §4.5, as the bridge sends it.
///
/// Every member is always present on the wire — the bridge serialises nulls
/// explicitly — so no field here needs a serde default.
#[derive(Clone, Deserialize)]
pub struct BridgeError {
    /// Failure category.
    pub kind: BridgeErrorKind,
    /// `SQLSTATE`, when the failure came from a `SQLException` that carried one.
    ///
    /// **Branch on the class (the first two characters), not on the whole
    /// code** — see [`BridgeError::sql_state_class`].
    pub sql_state: Option<String>,
    /// Driver-specific error number, `0` when there was none.
    pub vendor_code: i32,
    /// The failure message, ready to show to the user.
    pub message: String,
    /// The flattened cause chain, walking both `getCause` and
    /// `getNextException`. Drivers routinely hide the real reason in the second.
    pub causes: Vec<String>,
    /// Java stack trace. Debug log only — never shown to the user, and left out
    /// of the [`Debug`] rendering because it is kilobytes long.
    stack: Option<String>,
}

impl BridgeError {
    /// The `SQLSTATE` class: the first two characters of the code.
    ///
    /// This is the level to branch on. Standard-conforming drivers still
    /// disagree on the last three characters — a missing table is `42S04` on H2
    /// and `42S02` elsewhere — so only the class (`42`, syntax error or access
    /// rule violation) is worth testing against.
    pub fn sql_state_class(&self) -> Option<&str> {
        self.sql_state
            .as_deref()
            .filter(|state| state.len() >= 2 && state.is_char_boundary(2))
            .map(|state| &state[..2])
    }

    /// The Java stack trace, for the debug log only.
    ///
    /// Never render this to the user: it names bridge internals and says
    /// nothing they can act on.
    pub fn stack(&self) -> Option<&str> {
        self.stack.as_deref()
    }

    /// Whether this is the bridge saying "that operation exists, but this build
    /// does not implement it yet".
    ///
    /// Worth telling apart from an unknown operation
    /// ([`BridgeError::is_unknown_operation`]): this one means *wait for the
    /// next milestone*, the other means the two operation tables have drifted
    /// apart and something is seriously wrong.
    pub fn is_not_implemented(&self) -> bool {
        self.kind == BridgeErrorKind::Protocol && self.message.contains("not implemented")
    }

    /// Whether the bridge did not recognise the operation code at all.
    ///
    /// A build mismatch between this crate and the bridge JAR.
    pub fn is_unknown_operation(&self) -> bool {
        self.kind == BridgeErrorKind::Protocol && self.message.contains("unknown operation")
    }
}

impl fmt::Display for BridgeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} error", self.kind)?;
        if let Some(state) = &self.sql_state {
            write!(f, " [SQLSTATE {state}")?;
            if self.vendor_code != 0 {
                write!(f, ", code {}", self.vendor_code)?;
            }
            f.write_str("]")?;
        }
        write!(f, ": {}", self.message)
    }
}

impl fmt::Debug for BridgeError {
    /// Renders everything except the stack trace.
    ///
    /// A derived `Debug` would paste kilobytes of Java frames into every log
    /// line and every `unwrap` message that ever touches one of these.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("BridgeError")
            .field("kind", &self.kind)
            .field("sql_state", &self.sql_state)
            .field("vendor_code", &self.vendor_code)
            .field("message", &self.message)
            .field("causes", &self.causes)
            .field("stack", &self.stack.as_ref().map(|_| "<omitted>"))
            .finish()
    }
}

impl std::error::Error for BridgeError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sql_state_is_split_at_the_class() {
        let error = error_with_state(Some("42S04"));
        assert_eq!(error.sql_state_class(), Some("42"));
        assert_eq!(error_with_state(None).sql_state_class(), None);
        // A driver that answers something too short must not panic the caller.
        assert_eq!(error_with_state(Some("4")).sql_state_class(), None);
    }

    #[test]
    fn debug_leaves_the_stack_trace_out() {
        let mut error = error_with_state(Some("42S04"));
        error.stack = Some("java.lang.RuntimeException\n\tat …".to_string());
        let rendered = format!("{error:?}");
        assert!(rendered.contains("<omitted>"), "{rendered}");
        assert!(
            !rendered.contains("java.lang.RuntimeException"),
            "{rendered}"
        );
    }

    #[test]
    fn not_implemented_and_unknown_operation_are_different_answers() {
        let not_implemented = BridgeError {
            kind: BridgeErrorKind::Protocol,
            sql_state: None,
            vendor_code: 0,
            message: "operation 0x25 is not implemented in this build".to_string(),
            causes: Vec::new(),
            stack: None,
        };
        assert!(not_implemented.is_not_implemented());
        assert!(!not_implemented.is_unknown_operation());

        let unknown = BridgeError {
            message: "unknown operation 0x7f".to_string(),
            ..not_implemented.clone()
        };
        assert!(unknown.is_unknown_operation());
        assert!(!unknown.is_not_implemented());
    }

    #[test]
    fn an_unrecognised_kind_deserialises_instead_of_failing() {
        let error: BridgeError = serde_json::from_str(
            r#"{"kind":"quantum","sql_state":null,"vendor_code":0,
                "message":"m","causes":[],"stack":null}"#,
        )
        .expect("an unknown kind is not a parse failure");
        assert_eq!(error.kind, BridgeErrorKind::Unknown);
    }

    fn error_with_state(state: Option<&str>) -> BridgeError {
        BridgeError {
            kind: BridgeErrorKind::Sql,
            sql_state: state.map(str::to_string),
            vendor_code: 42102,
            message: "table not found".to_string(),
            causes: Vec::new(),
            stack: None,
        }
    }
}
