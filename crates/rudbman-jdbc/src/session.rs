//! Sessions, cursors and the worker thread behind each connection
//! (architecture document, §4.2).
//!
//! # One thread per connection
//!
//! A JDBC `Connection` is not thread safe, and no amount of locking in the
//! bridge makes it so. The structural answer is here: every session owns one
//! worker thread, that thread attaches to the JVM once and stays attached, and
//! every command for that connection is handed to it and executed in order.
//! Two callers can hold a [`Session`] and neither can interleave with the other.
//!
//! Staying attached is not an optimisation detail. Attaching per command makes
//! the JVM build a thread structure every time, and the fetch path runs once
//! per screenful of scrolling.
//!
//! # Except cancellation
//!
//! [`Session::cancel`] deliberately does **not** go through the queue. It is
//! called precisely when the worker is blocked inside the statement it is meant
//! to interrupt, so queueing it behind that statement would mean waiting for
//! the thing being cancelled. It attaches the calling thread for the duration
//! of the call and detaches again — expensive, and rare.
//!
//! [`Canceller`] is the `Send + Sync` handle for that, so the cancel button can
//! hold one while the query it aborts is still running.
//!
//! # Panics stay inside
//!
//! Every JNI call runs inside `catch_unwind`. A panic there kills the session —
//! the worker stops and every later command answers [`Error::WorkerGone`] — but
//! it does not unwind into the JVM and does not take the process with it.
//!
//! # This crate does not know gpui
//!
//! Every method here blocks. Binding them to a UI thread — `background_spawn`,
//! a channel, whatever the application prefers — is `rudbman-app`'s job, and
//! keeping that out of here is what makes this layer testable without a window
//! (architecture document, §3.1).

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Arc;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::thread::JoinHandle;

use parking_lot::Mutex;
use serde::Deserialize;

use crate::codec::Batch;
use crate::error::{Error, Result};
use crate::jvm::Jvm;
use crate::protocol::{Op, parse_json, take_payload};
use crate::response::{
    Cancelled, ColumnInfo, DdlResult, DescribeResult, ExecuteResult, Ping, SessionInfo,
};
use crate::spec::{ConnectionSpec, DdlSource, DescribeRequest, StatementSpec};

/// The `OPEN_SESSION` response body.
#[derive(Deserialize)]
struct Opened {
    session: i64,
}

/// One JDBC connection, its worker thread, and everything that runs on it.
///
/// Dropping a session closes it. Prefer [`Session::close`] when the failure
/// matters: `Drop` can only log.
pub struct Session {
    jvm: &'static Jvm,
    worker: Arc<Worker>,
    handle: i64,
    closed: bool,
}

impl std::fmt::Debug for Session {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Session")
            .field("handle", &self.handle)
            .field("closed", &self.closed)
            .finish()
    }
}

impl Session {
    /// Opens a connection and starts its worker thread.
    ///
    /// Blocks until the driver has connected or refused.
    pub fn open(jvm: &'static Jvm, spec: &ConnectionSpec) -> Result<Session> {
        // The spec's Debug is the masked one; this line must stay loggable.
        log::debug!("opening a session: {spec:?}");

        let worker = Worker::start(jvm)?;
        let request = serde_json::to_vec(spec)?;
        let opened: Opened = match worker
            .call(Op::OpenSession, 0, 0, Some(request))
            .and_then(take_payload)
            .and_then(|payload| parse_json(&payload))
        {
            Ok(opened) => opened,
            Err(error) => {
                // No connection means no session to close, but the thread is
                // already running and would otherwise be left behind.
                worker.shutdown();
                return Err(error);
            }
        };

        Ok(Session {
            jvm,
            worker,
            handle: opened.session,
            closed: false,
        })
    }

    /// The bridge-side session handle.
    pub fn handle(&self) -> i64 {
        self.handle
    }

    /// Checks that the connection is alive.
    pub fn ping(&self) -> Result<Ping> {
        self.json_call(Op::Ping, self.handle, 0, None)
    }

    /// Product, driver and capability facts about this connection.
    pub fn info(&self) -> Result<SessionInfo> {
        self.json_call(Op::SessionInfo, self.handle, 0, None)
    }

    /// Runs a metadata query.
    ///
    /// Every kind but `ddl` comes back here as `{kind, items[]}`. `ddl` answers
    /// a document instead and has [`Session::describe_ddl`] to itself; asking
    /// for it through this method fails with a protocol error rather than
    /// half-parsing.
    pub fn describe(&self, request: &DescribeRequest) -> Result<DescribeResult> {
        let body = serde_json::to_vec(request)?;
        self.json_call(Op::Describe, self.handle, 0, Some(body))
    }

    /// Reads one table's `CREATE` statement.
    ///
    /// `catalog` and `schema` are exact names, `None` meaning "wherever the
    /// connection is pointed"; `table` is exact and required.
    ///
    /// [`DdlSource`] picks the layer, and [`DdlResult::source`] reports which
    /// one actually answered — with [`DdlSource::Auto`] a server that has no
    /// native path, or refuses it, silently yields reconstructed text, and the
    /// UI should say so.
    ///
    /// # Errors
    ///
    /// * [`DdlSource::Native`] against a product with no native path is a `sql`
    ///   error suggesting `metadata`, not a fallback.
    /// * An unknown table is whatever the driver says — usually a `sql` error of
    ///   class `42`, and on some drivers an empty `CREATE TABLE` shell, because
    ///   `getColumns` for a table that does not exist is an empty result rather
    ///   than a failure.
    pub fn describe_ddl(
        &self,
        catalog: Option<&str>,
        schema: Option<&str>,
        table: &str,
        source: DdlSource,
    ) -> Result<DdlResult> {
        let mut request = DescribeRequest::new("ddl").with_table(table);
        request.catalog = catalog.map(str::to_string);
        request.schema = schema.map(str::to_string);
        request.source = Some(source);
        let body = serde_json::to_vec(&request)?;
        self.json_call(Op::Describe, self.handle, 0, Some(body))
    }

    /// Executes a statement and returns its cursor.
    ///
    /// A cursor comes back even for an `UPDATE` that produced only a row count,
    /// so that [`Cursor::more_results`] always has something to advance and
    /// [`Cursor::close`] always has something to close.
    pub fn execute(&self, statement: &StatementSpec) -> Result<Cursor> {
        let body = serde_json::to_vec(statement)?;
        let result: ExecuteResult = self.json_call(Op::Execute, self.handle, 0, Some(body))?;
        Ok(Cursor {
            worker: Arc::clone(&self.worker),
            handle: result.cursor,
            result,
            closed: false,
        })
    }

    /// Turns auto-commit on or off.
    pub fn set_auto_commit(&self, on: bool) -> Result<()> {
        self.unit_call(Op::SetAutoCommit, self.handle, i64::from(on))
    }

    /// Commits the open transaction.
    pub fn commit(&self) -> Result<()> {
        self.unit_call(Op::Commit, self.handle, 0)
    }

    /// Rolls the open transaction back.
    pub fn rollback(&self) -> Result<()> {
        self.unit_call(Op::Rollback, self.handle, 0)
    }

    /// Cancels whatever is running on this connection.
    ///
    /// Returns how many statements a cancel was issued for; zero on an idle
    /// session, which is not an error. Runs on the calling thread rather than
    /// on the worker — see the module documentation.
    pub fn cancel(&self) -> Result<u32> {
        self.canceller().cancel()
    }

    /// A handle that can cancel this session from any thread.
    pub fn canceller(&self) -> Canceller {
        Canceller {
            jvm: self.jvm,
            session: self.handle,
        }
    }

    /// Calls an operation this crate does not model yet.
    ///
    /// The escape hatch for `LOB_READ` and the job operations, which the bridge
    /// answers with a `protocol` /
    /// [not implemented](crate::BridgeError::is_not_implemented) error until
    /// their request shapes are settled. Goes through the worker like every
    /// other command, so it stays serialised against the connection.
    pub fn call_raw(
        &self,
        op: Op,
        handle: i64,
        arg: i64,
        request: Option<Vec<u8>>,
    ) -> Result<Vec<u8>> {
        take_payload(self.worker.call(op, handle, arg, request)?)
    }

    /// Closes the connection and stops the worker thread.
    pub fn close(mut self) -> Result<()> {
        self.close_inner()
    }

    fn close_inner(&mut self) -> Result<()> {
        if self.closed {
            return Ok(());
        }
        self.closed = true;
        let result = self.unit_call(Op::CloseSession, self.handle, 0);
        self.worker.shutdown();
        result
    }

    /// Runs an operation whose response is JSON.
    fn json_call<T: serde::de::DeserializeOwned>(
        &self,
        op: Op,
        handle: i64,
        arg: i64,
        request: Option<Vec<u8>>,
    ) -> Result<T> {
        let payload = take_payload(self.worker.call(op, handle, arg, request)?)?;
        parse_json(&payload)
    }

    /// Runs an operation with no response body.
    fn unit_call(&self, op: Op, handle: i64, arg: i64) -> Result<()> {
        take_payload(self.worker.call(op, handle, arg, None)?).map(|_| ())
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        if let Err(error) = self.close_inner() {
            log::warn!("closing session {} failed: {error}", self.handle);
        }
    }
}

/// Cancels a session's running statement from another thread.
///
/// Cheap to clone and safe to keep: a stale handle is reported as a stale
/// handle, because the bridge never reuses one.
#[derive(Clone, Debug)]
pub struct Canceller {
    jvm: &'static Jvm,
    session: i64,
}

impl Canceller {
    /// Issues the cancel. Returns how many statements it reached.
    pub fn cancel(&self) -> Result<u32> {
        let response = self.jvm.call_detached(Op::Cancel, self.session, 0, None)?;
        let payload = take_payload(response)?;
        let cancelled: Cancelled = parse_json(&payload)?;
        Ok(cancelled.cancelled)
    }

    /// The session this handle cancels.
    pub fn session(&self) -> i64 {
        self.session
    }
}

/// One executed statement: its result metadata and its rows.
///
/// A cursor runs on the session's worker thread and does not keep the session
/// alive: once the session is closed, every call here answers
/// [`Error::WorkerGone`], which is the same answer the bridge would give for a
/// handle whose connection is gone.
///
/// Dropping a cursor closes it. Prefer [`Cursor::close`] when the failure
/// matters.
pub struct Cursor {
    worker: Arc<Worker>,
    handle: i64,
    result: ExecuteResult,
    closed: bool,
}

impl std::fmt::Debug for Cursor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Cursor")
            .field("handle", &self.handle)
            .field("columns", &self.result.columns.len())
            .field("update_count", &self.result.update_count)
            .field("closed", &self.closed)
            .finish()
    }
}

impl Cursor {
    /// The bridge-side cursor handle.
    pub fn handle(&self) -> i64 {
        self.handle
    }

    /// The current result's metadata.
    ///
    /// Replaced by [`Cursor::more_results`] when the statement produced several
    /// results.
    pub fn result(&self) -> &ExecuteResult {
        &self.result
    }

    /// The current result's columns.
    pub fn columns(&self) -> &[ColumnInfo] {
        &self.result.columns
    }

    /// Reads the next batch of rows.
    ///
    /// `max_rows` is the row limit; `0` asks the bridge for its default of 500.
    ///
    /// A batch that fills the limit exactly does **not** carry the last-batch
    /// flag — the driver had not run out of rows yet, and there is no way to
    /// find out without asking. Keep fetching until
    /// [`Batch::is_last`](crate::Batch::is_last).
    pub fn fetch(&self, max_rows: u32) -> Result<Batch> {
        let payload =
            take_payload(
                self.worker
                    .call(Op::Fetch, self.handle, i64::from(max_rows), None)?,
            )?;
        Ok(Batch::decode(&payload)?)
    }

    /// Advances to the statement's next result.
    ///
    /// Returns the new result's metadata, which also replaces
    /// [`Cursor::result`]. Keep calling until
    /// [`ExecuteResult::is_exhausted`] holds: `may_have_more` is a hint, and a
    /// single reading of it proves nothing.
    pub fn more_results(&mut self) -> Result<&ExecuteResult> {
        let payload = take_payload(self.worker.call(Op::MoreResults, self.handle, 0, None)?)?;
        self.result = parse_json(&payload)?;
        Ok(&self.result)
    }

    /// Closes the cursor and its statement.
    pub fn close(mut self) -> Result<()> {
        self.close_inner()
    }

    fn close_inner(&mut self) -> Result<()> {
        if self.closed {
            return Ok(());
        }
        self.closed = true;
        take_payload(self.worker.call(Op::CloseCursor, self.handle, 0, None)?).map(|_| ())
    }
}

impl Drop for Cursor {
    fn drop(&mut self) {
        if let Err(error) = self.close_inner() {
            log::debug!("closing cursor {} failed: {error}", self.handle);
        }
    }
}

/// One command for a session worker.
enum Job {
    /// Invoke `Bridge.call` and send the raw response envelope back.
    Call {
        op: Op,
        handle: i64,
        arg: i64,
        request: Option<Vec<u8>>,
        reply: Sender<Result<Vec<u8>>>,
    },
    /// Leave the loop, detach and end the thread.
    Stop,
}

/// The worker thread of one session.
struct Worker {
    /// `None` once the worker has been told to stop.
    ///
    /// The mutex is what makes an `Arc<Worker>` usable from several threads —
    /// an `mpsc::Sender` is `Send` but not `Sync` — and holding it across the
    /// reply is deliberate: commands for one connection are serialised at the
    /// door rather than piling up in a queue whose depth nobody bounds.
    sender: Mutex<Option<Sender<Job>>>,
    thread: Mutex<Option<JoinHandle<()>>>,
}

impl Worker {
    /// Spawns the thread and waits for it to attach to the JVM.
    fn start(jvm: &'static Jvm) -> Result<Arc<Worker>> {
        let (sender, receiver) = channel();
        let (ready_tx, ready_rx) = channel();

        let thread = std::thread::Builder::new()
            .name("rudbman-session".to_string())
            .spawn(move || run(jvm, receiver, ready_tx))
            .map_err(|source| Error::Jni(format!("cannot spawn the session thread: {source}")))?;

        match ready_rx.recv() {
            Ok(Ok(())) => {}
            Ok(Err(error)) => return Err(error),
            Err(_) => return Err(Error::WorkerGone),
        }

        Ok(Arc::new(Worker {
            sender: Mutex::new(Some(sender)),
            thread: Mutex::new(Some(thread)),
        }))
    }

    /// Hands one command to the worker and waits for its answer.
    fn call(&self, op: Op, handle: i64, arg: i64, request: Option<Vec<u8>>) -> Result<Vec<u8>> {
        let (reply, answer) = channel();
        let guard = self.sender.lock();
        let sender = guard.as_ref().ok_or(Error::WorkerGone)?;
        sender
            .send(Job::Call {
                op,
                handle,
                arg,
                request,
                reply,
            })
            .map_err(|_| Error::WorkerGone)?;
        // A dropped reply channel means the worker stopped mid-command, which
        // is what a panic in a JNI call looks like from here.
        answer.recv().map_err(|_| Error::WorkerGone)?
    }

    /// Stops the worker and waits for the thread to end.
    fn shutdown(&self) {
        if let Some(sender) = self.sender.lock().take() {
            let _ = sender.send(Job::Stop);
        }
        if let Some(thread) = self.thread.lock().take()
            && thread.join().is_err()
        {
            // `run` catches panics around the JNI calls, so this is only
            // reachable if the thread body itself came apart.
            log::error!("the session worker thread panicked");
        }
    }
}

/// The worker thread body: attach once, then serve the queue until told to stop.
fn run(jvm: &'static Jvm, receiver: Receiver<Job>, ready: Sender<Result<()>>) {
    // The attachment lasts for the whole closure, which is the whole life of
    // the session. `jni` 0.22 has no `AttachCurrentThreadAsDaemon` of its own;
    // it attaches permanently and detaches when the thread ends, and since this
    // process never calls `DestroyJavaVM` — the one thing daemon status changes
    // — the two are equivalent here.
    let attached: Result<()> = jvm.vm().attach_current_thread(|env| {
        if ready.send(Ok(())).is_err() {
            // Nobody is waiting for this worker any more.
            return Ok(());
        }
        while let Ok(job) = receiver.recv() {
            let Job::Call {
                op,
                handle,
                arg,
                request,
                reply,
            } = job
            else {
                break;
            };

            // A panic here must take the session down, not the process
            // (architecture document, §4.2). `AssertUnwindSafe` is honest: the
            // only state that survives the panic is the JVM's, and the session
            // is abandoned right after.
            let outcome = catch_unwind(AssertUnwindSafe(|| {
                jvm.call_attached(env, op, handle, arg, request.as_deref())
            }));

            match outcome {
                Ok(result) => {
                    // A caller that gave up before the answer arrived is normal.
                    let _ = reply.send(result);
                }
                Err(panic) => {
                    let message = panic_message(panic.as_ref());
                    log::error!("session worker panicked in {op:?}: {message}");
                    let _ = reply.send(Err(Error::WorkerPanic(message)));
                    // The connection's state is unknown from here on.
                    break;
                }
            }
        }
        Ok(())
    });

    if let Err(error) = attached {
        log::error!("session worker could not attach to the JVM: {error}");
        let _ = ready.send(Err(error));
    }
}

/// Best effort rendering of a panic payload.
fn panic_message(panic: &(dyn std::any::Any + Send)) -> String {
    if let Some(message) = panic.downcast_ref::<&str>() {
        (*message).to_string()
    } else if let Some(message) = panic.downcast_ref::<String>() {
        message.clone()
    } else {
        "a panic with no message".to_string()
    }
}
