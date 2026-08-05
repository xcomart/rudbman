//! The JNI layer of rudbman: a JVM, a Java bridge, and one JDBC connection per
//! worker thread.
//!
//! Every database rudbman talks to, it talks to through a JDBC driver running
//! inside an embedded JVM. This crate owns that JVM, the threads that call into
//! it, and the decoder for the binary result batches that come back.
//!
//! ```no_run
//! use rudbman_jdbc::{ConnectionSpec, Jvm, JvmConfig, Session, StatementSpec, Value};
//!
//! # fn main() -> anyhow::Result<()> {
//! // One JVM per process; the first call builds it, later calls find it.
//! let jvm = Jvm::start(&JvmConfig::new(rudbman_jdbc::default_bridge_jar()))?;
//!
//! let spec = ConnectionSpec::new("jdbc:h2:mem:demo", "org.h2.Driver")
//!     .with_credentials("sa", "")
//!     .with_jars(["/opt/drivers/h2-2.3.232.jar".into()]);
//! let session = Session::open(jvm, &spec)?;
//!
//! let cursor = session.execute(&StatementSpec::new("select id, name from person"))?;
//! loop {
//!     let batch = cursor.fetch(500)?;
//!     for row in 0..batch.rows() {
//!         if let Some(Value::Str(name)) = batch.value(row, 1) {
//!             println!("{name}");
//!         }
//!     }
//!     // Not `rows() < 500`: a batch that fills its limit exactly still is not
//!     // the last one.
//!     if batch.is_last() {
//!         break;
//!     }
//! }
//! # Ok(())
//! # }
//! ```
//!
//! # The shape of it
//!
//! ```text
//! caller thread                    session worker thread          JVM
//!     │  Session::execute(…)              │                        │
//!     ├──────────── command queue ───────►│  Bridge.call(op, …)    │
//!     │                                   ├───────────────────────►│
//!     │◄─────────── response bytes ───────┤                        │
//!     │  decode here, not there           │                        │
//! ```
//!
//! * [`Jvm`] starts the one VM the process gets, on a thread of its own, and
//!   caches the single `jmethodID` the whole protocol goes through. It also
//!   carries [`Jvm::probe_drivers`], the one question that has to be answerable
//!   before any connection exists: what driver does this JAR contain?
//! * [`Session`] owns a connection and the worker thread that serialises every
//!   command for it. [`Canceller`] is the one path that does not queue.
//! * [`Cursor`] executes and fetches; [`Batch`] decodes what comes back.
//! * [`Job`] is the data plane: a script extraction ([`ExtractSpec`]) whose rows
//!   never cross the boundary at all, watched through [`JobProgress`].
//! * Requests are [`ConnectionSpec`], [`StatementSpec`], [`DescribeRequest`] and
//!   [`ExtractSpec`]; responses are [`ExecuteResult`], [`SessionInfo`],
//!   [`DescribeResult`] and friends.
//!
//! # Four things worth knowing before using it
//!
//! * **This crate does not know gpui.** Every call blocks; binding them to a UI
//!   thread belongs to `rudbman-app`. That boundary is what lets the whole JNI
//!   layer be tested without a window (architecture document, §3.1).
//! * **`may_have_more` is a hint.** JDBC has no non-destructive lookahead, so a
//!   single reading of [`ExecuteResult::may_have_more`] proves nothing. Keep
//!   calling [`Cursor::more_results`] until [`ExecuteResult::is_exhausted`].
//! * **Branch on the `SQLSTATE` class**, not the whole code — see
//!   [`BridgeError::sql_state_class`]. A missing table is `42S04` on H2 and
//!   `42S02` elsewhere; only the leading `42` is portable.
//! * **A column's physical [`ColumnKind`] changes between batches.** Any column
//!   that is entirely NULL in a batch arrives as [`ColumnKind::Nulls`] whatever
//!   its declared type, so decode against the batch in hand. The stable type is
//!   the logical one in [`ColumnInfo`].
//!
//! # Secrets
//!
//! Nothing here renders a credential. [`ConnectionSpec`]'s [`Debug`] masks the
//! password, every driver property value, and credentials embedded in the JDBC
//! URL; [`BridgeError`]'s leaves out the Java stack trace. Logging a spec or an
//! error is safe, and it needs to stay that way.
//!
//! # Building the bridge JAR
//!
//! The Java side is a separate artefact:
//!
//! ```text
//! cd bridge && ./gradlew jar
//! ```
//!
//! `cargo build` never runs Gradle — the build script only checks that the JAR
//! is there, because a JVM start-up per Rust edit is not a trade worth making.

#![warn(missing_docs)]

pub mod codec;
pub mod error;
pub mod jvm;
pub mod protocol;
pub mod response;
pub mod session;
pub mod spec;

pub use codec::{Batch, CodecError, Column, ColumnKind, Value};
pub use error::{BridgeError, BridgeErrorKind, Error, Result};
pub use jvm::{BRIDGE_JAR_ENV, JAVA_HOME_ENV, Jvm, JvmConfig, default_bridge_jar};
pub use protocol::Op;
pub use response::{
    Cancelled, ColumnInfo, DdlResult, DescribeResult, DriverProbe, ExecuteResult, JobProgress,
    JobState, Ping, SQL_TYPE_REAL, SessionInfo,
};
pub use session::{Canceller, Cursor, Job, Session};
pub use spec::{
    ConnectionSpec, Constraints, DataMode, DataOptions, DdlOptions, DdlSource, DescribeRequest,
    ExtractSpec, KeepAliveSpec, Newline, ObjectRef, OutputSpec, Param, ProbeRequest, StatementSpec,
};
