//! Operation codes and the response envelope (architecture document, §4.4 and
//! §4.5).
//!
//! The JNI signature never changes; new capabilities arrive as new operation
//! codes. The numbers are part of the wire contract with the bridge JAR and
//! must not be renumbered.

use crate::error::{BridgeError, Error, Result};

/// Response tag: the call succeeded and the rest is its body.
const TAG_OK: u8 = 0;
/// Response tag: the rest is an error JSON object.
const TAG_ERROR: u8 = 1;

/// A bridge operation.
///
/// The `handle` argument means a session, a cursor or a job depending on the
/// operation; see each variant.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Op {
    /// Open a connection. Request: a connection spec. Response: `{session}`.
    OpenSession,
    /// Close a session. Handle: session.
    CloseSession,
    /// Check a connection. Handle: session. Response: `{ok, elapsed_ms}`.
    Ping,
    /// Product, driver and capability facts. Handle: session.
    SessionInfo,
    /// Metadata query. Handle: session. Request: `{kind, …}`.
    Describe,
    /// Run a statement. Handle: session. Request: a statement spec.
    Execute,
    /// Read the next batch. Handle: cursor. `arg`: the row limit. Response: an
    /// `RDB1` batch, which is why this path parses no JSON at all.
    Fetch,
    /// Advance to the statement's next result. Handle: cursor.
    MoreResults,
    /// Close a cursor. Handle: cursor.
    CloseCursor,
    /// Cancel whatever is running on a session. Handle: session.
    ///
    /// The one operation that arrives on a thread other than the session
    /// worker — the worker is blocked inside the statement being cancelled.
    Cancel,
    /// Read part of a LOB. Handle: cursor. Not implemented by the bridge yet.
    LobRead,
    /// Set auto-commit. Handle: session. `arg`: 0 or 1.
    SetAutoCommit,
    /// Commit. Handle: session.
    Commit,
    /// Roll back. Handle: session.
    Rollback,
    /// Start a job. Handle: session. Request: `{kind, …}` — `extract` today,
    /// `backup` and `transfer` later. Response: `{job}`.
    JobStart,
    /// Poll a job's progress. Handle: job.
    ///
    /// The first poll that reports a terminal state unregisters the job in the
    /// same call, so a later poll or cancel is a `protocol` error.
    JobPoll,
    /// Cancel a job. Handle: job. Response: `{cancelled}`.
    ///
    /// The second operation that may arrive on a thread other than the session
    /// worker, for the same reason [`Op::Cancel`] does.
    JobCancel,
    /// Inspect driver JARs without initialising anything in them.
    ProbeDriver,
}

impl Op {
    /// The wire code.
    pub fn code(self) -> u8 {
        match self {
            Op::OpenSession => 0x01,
            Op::CloseSession => 0x02,
            Op::Ping => 0x03,
            Op::SessionInfo => 0x04,
            Op::Describe => 0x10,
            Op::Execute => 0x20,
            Op::Fetch => 0x21,
            Op::MoreResults => 0x22,
            Op::CloseCursor => 0x23,
            Op::Cancel => 0x24,
            Op::LobRead => 0x25,
            Op::SetAutoCommit => 0x30,
            Op::Commit => 0x31,
            Op::Rollback => 0x32,
            Op::JobStart => 0x40,
            Op::JobPoll => 0x41,
            Op::JobCancel => 0x42,
            Op::ProbeDriver => 0x50,
        }
    }
}

/// Strips the response envelope, turning an ERROR envelope into an [`Error`].
///
/// The payload is moved out of `response` rather than copied: a batch is the
/// largest thing that crosses this boundary and it crosses it on every scroll.
pub(crate) fn take_payload(mut response: Vec<u8>) -> Result<Vec<u8>> {
    let Some(&tag) = response.first() else {
        return Err(Error::Protocol(
            "the bridge returned an empty response; it always writes at least a tag byte".into(),
        ));
    };
    response.drain(..1);
    match tag {
        TAG_OK => Ok(response),
        TAG_ERROR => {
            let error: BridgeError = serde_json::from_slice(&response).map_err(|source| {
                Error::Protocol(format!("cannot parse the error envelope: {source}"))
            })?;
            Err(Error::Bridge(error))
        }
        other => Err(Error::Protocol(format!(
            "unexpected response tag {other:#04x}; expected 0 (ok) or 1 (error)"
        ))),
    }
}

/// Parses an OK payload as JSON.
pub(crate) fn parse_json<T: serde::de::DeserializeOwned>(payload: &[u8]) -> Result<T> {
    serde_json::from_slice(payload).map_err(|source| {
        Error::Protocol(format!(
            "cannot parse the response body as {}: {source}",
            std::any::type_name::<T>()
        ))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::BridgeErrorKind;

    #[test]
    fn operation_codes_match_the_table() {
        // Straight from architecture document §4.4; a renumbering here would be
        // a silent protocol break, so the table is asserted rather than trusted.
        assert_eq!(Op::OpenSession.code(), 0x01);
        assert_eq!(Op::CloseSession.code(), 0x02);
        assert_eq!(Op::Ping.code(), 0x03);
        assert_eq!(Op::SessionInfo.code(), 0x04);
        assert_eq!(Op::Describe.code(), 0x10);
        assert_eq!(Op::Execute.code(), 0x20);
        assert_eq!(Op::Fetch.code(), 0x21);
        assert_eq!(Op::MoreResults.code(), 0x22);
        assert_eq!(Op::CloseCursor.code(), 0x23);
        assert_eq!(Op::Cancel.code(), 0x24);
        assert_eq!(Op::LobRead.code(), 0x25);
        assert_eq!(Op::SetAutoCommit.code(), 0x30);
        assert_eq!(Op::Commit.code(), 0x31);
        assert_eq!(Op::Rollback.code(), 0x32);
        assert_eq!(Op::JobStart.code(), 0x40);
        assert_eq!(Op::JobPoll.code(), 0x41);
        assert_eq!(Op::JobCancel.code(), 0x42);
        assert_eq!(Op::ProbeDriver.code(), 0x50);
    }

    #[test]
    fn an_ok_envelope_yields_its_body() {
        let payload = take_payload(vec![0, b'{', b'}']).expect("ok envelope");
        assert_eq!(payload, b"{}");
        assert!(take_payload(vec![0]).expect("empty body").is_empty());
    }

    #[test]
    fn an_error_envelope_becomes_an_error() {
        let mut bytes = vec![1u8];
        bytes.extend_from_slice(
            br#"{"kind":"sql","sql_state":"42S04","vendor_code":42102,
                 "message":"table not found","causes":[],"stack":"at ..."}"#,
        );
        let Err(Error::Bridge(error)) = take_payload(bytes) else {
            panic!("an ERROR envelope must not read as success")
        };
        assert_eq!(error.kind, BridgeErrorKind::Sql);
        assert_eq!(error.sql_state_class(), Some("42"));
        assert_eq!(error.vendor_code, 42102);
    }

    #[test]
    fn anything_else_is_a_protocol_error() {
        assert!(matches!(take_payload(vec![]), Err(Error::Protocol(_))));
        assert!(matches!(take_payload(vec![2, 3]), Err(Error::Protocol(_))));
        // An ERROR envelope whose body is not the documented JSON.
        assert!(matches!(
            take_payload(vec![1, b'n', b'o']),
            Err(Error::Protocol(_))
        ));
    }
}
