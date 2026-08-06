package comart.rudbman.bridge.job;

import com.google.gson.JsonArray;
import com.google.gson.JsonObject;
import comart.rudbman.bridge.BridgeException;
import comart.rudbman.bridge.Envelope;
import comart.rudbman.bridge.Json;
import comart.rudbman.bridge.Registry;
import comart.rudbman.bridge.Session;

import java.sql.SQLException;
import java.sql.Statement;
import java.util.ArrayList;
import java.util.List;
import java.util.concurrent.ConcurrentHashMap;
import java.util.concurrent.atomic.AtomicReferenceArray;
import java.util.logging.Level;
import java.util.logging.Logger;

/**
 * The worker-thread, progress and cancellation frame behind {@code JOB_START},
 * {@code JOB_POLL} and {@code JOB_CANCEL} (architecture.md 6).
 *
 * <p>A job is the answer to the question the data plane asks: what happens when
 * an operation moves more rows than anyone wants to carry across the JNI
 * boundary, and takes long enough that the UI has to stay alive while it runs.
 * The row data never leaves the JVM; what crosses is a handle and, every couple
 * of hundred milliseconds, a progress object.
 *
 * <h2>Threading</h2>
 *
 * <p>Each job owns a daemon thread. That thread talks to the database through
 * the session's own {@link java.sql.Connection}, which is not thread safe and is
 * shared with whatever the user does in the UI meanwhile, so it takes
 * {@link Session#lock()} around each phase of work - see
 * {@link Job#runLocked}. A phase is the unit, not a statement, because a phase
 * streams one {@link java.sql.ResultSet} and a result set cannot survive another
 * statement running on the same connection.
 *
 * <p>The consequence is deliberate and has to be understood by the caller: while
 * a job is streaming a table, an {@code EXECUTE} on the same session blocks.
 * A UI that wants to keep querying during a long extract has to open a second
 * session.
 *
 * <h2>Cancellation</h2>
 *
 * <p>{@code JOB_CANCEL} sets a flag and calls {@link Statement#cancel()} on
 * whatever statement is in flight, exactly as {@code CANCEL} does for a cursor,
 * and for the same reason it takes no lock: the thread being cancelled is
 * holding it. The worker notices the flag at the next row or phase boundary and
 * finishes in state {@code cancelled}, leaving a partial output file behind
 * rather than deleting work the user may still want.
 *
 * <h2>Handle lifetime</h2>
 *
 * <p><strong>A job handle dies on the first poll that reports a terminal
 * state.</strong> {@code JOB_POLL} answers {@code done}, {@code failed} or
 * {@code cancelled} exactly once and unregisters the job in the same call; a
 * second poll gets "unknown or already closed job handle". This is the rule the
 * Rust side has to code against: stop polling as soon as a terminal state
 * arrives, and never cancel a job whose terminal state has already been read.
 * The alternative - keeping terminated jobs around - has no natural end, because
 * nothing obliges a client to poll at all.
 *
 * <p>A session being closed cancels and unregisters its jobs, so a client that
 * walks away from a job leaks nothing beyond that session's lifetime.
 */
public final class Jobs {

    private static final Logger LOG = Logger.getLogger(Jobs.class.getName());

    /**
     * Live jobs, so that closing a session can find the jobs running on it.
     * Concurrent for the same reason the cursor table is: cancellation arrives on
     * another thread.
     */
    private static final ConcurrentHashMap<Long, Job> LIVE = new ConcurrentHashMap<>();

    private Jobs() {
    }

    /**
     * Starts a job from a {@code JOB_START} request body.
     *
     * <p>The specification is validated on the calling thread, before the worker
     * exists, so a malformed request comes back as an ERROR envelope from
     * {@code JOB_START} rather than as a job that immediately fails. A client
     * should not have to poll to learn that it sent nonsense.
     *
     * @param session the session the job runs on
     * @param spec    the job specification, {@code kind} plus whatever that kind
     *                needs
     * @return an object carrying the new job's {@code job} handle
     */
    public static JsonObject start(Session session, JsonObject spec) {
        String kind = Json.str(spec, "kind");
        if (kind == null || kind.isEmpty()) {
            throw new BridgeException("protocol", "job_start requires 'kind'");
        }
        Job job;
        switch (kind) {
            case "extract":
                job = new ExtractJob(session, spec);
                break;
            case "transfer":
                job = new TransferJob(session, spec);
                break;
            case "backup":
                job = new BackupJob(session, spec);
                break;
            default:
                throw new BridgeException("protocol", "unknown job kind '" + kind + "'");
        }
        job.register();
        JsonObject o = new JsonObject();
        o.addProperty("job", job.handle());
        return o;
    }

    /**
     * Answers {@code JOB_POLL}.
     *
     * @param handle a job handle
     * @return the progress object of architecture.md 6
     * @throws BridgeException when the handle is not a live job, which after a
     *                         terminal state has been reported once it no longer is
     */
    public static JsonObject poll(long handle) {
        Job job = Registry.get(handle, Job.class, "job");
        JsonObject progress = job.progress();
        if (job.isTerminal()) {
            // The client has now been told; the handle has done its work.
            job.unregister();
        }
        return progress;
    }

    /**
     * Answers {@code JOB_CANCEL}.
     *
     * @param handle a job handle
     * @return an object carrying {@code cancelled}, whether the job was still
     *         running when the request arrived
     */
    public static JsonObject cancel(long handle) {
        Job job = Registry.get(handle, Job.class, "job");
        boolean was = job.cancel();
        JsonObject o = new JsonObject();
        o.addProperty("cancelled", was);
        return o;
    }

    /**
     * Cancels and forgets every job running on a session.
     *
     * <p>Called at the top of {@link Session#close()}, before the connection lock
     * is taken: the worker is holding that lock and only a statement cancel will
     * make it let go.
     *
     * <p>The test is {@link Job#uses(Session)}, not "is this the session the job
     * was started on". A transfer holds the target session's lock for its whole
     * run while being registered against the source, so a filter on the owning
     * session alone would let {@code CLOSE_SESSION} on the target wait forever
     * for a lock only a cancel can release.
     *
     * @param session the session being closed
     */
    public static void cancelAll(Session session) {
        for (Job job : new ArrayList<>(LIVE.values())) {
            if (job.uses(session)) {
                job.cancel();
                job.unregister();
            }
        }
    }

    /** @return the number of jobs that have not yet been unregistered. */
    public static int liveCount() {
        return LIVE.size();
    }

    // ------------------------------------------------------------------- job

    /**
     * One background operation: its thread, its progress and its cancellation
     * flag.
     *
     * <p>A subclass implements {@link #run()} and nothing else. It reports
     * progress through {@link #addRows}, {@link #addBytes} and {@link #phase},
     * checks {@link #cancelled()} at every row boundary, and wraps every piece of
     * work that touches the connection in {@link #runLocked}.
     */
    public abstract static class Job {

        /** Progress state names, as they appear on the wire. */
        private static final String RUNNING = "running";
        private static final String DONE = "done";
        private static final String FAILED = "failed";
        private static final String CANCELLED = "cancelled";

        /**
         * Cancellation slot for the statement a job reads from - the only one an
         * extract or a backup has.
         */
        protected static final int SOURCE = 0;

        /** Cancellation slot for the statement a transfer writes through. */
        protected static final int TARGET = 1;

        /** How many statements one job can have in flight at once. */
        private static final int SLOTS = 2;

        /**
         * How many failures {@code errors[]} carries before it stops growing.
         *
         * <p>A million-row transfer with {@code on_error: "log"} and a broken
         * target can fail every row, and that array crosses the JNI boundary on
         * every poll. Past the cap the failures are only counted, which is what
         * {@code rows_skipped} already reports.
         */
        private static final int MAX_ERRORS = 100;

        private final Session session;
        private final String kind;
        private long handle;

        private volatile String state = RUNNING;
        private volatile String phase = "starting";
        private volatile boolean cancelRequested;
        private volatile long rowsDone;
        private volatile long rowsSkipped;
        private volatile long rowsTotal = -1;
        private volatile long bytes;
        private final long startedNanos = System.nanoTime();

        /**
         * The statements a cancel has to interrupt, by slot. Each entry is
         * cleared as soon as its statement is done with, so that a cancel
         * arriving a moment late does not touch a closed statement.
         *
         * <p>There is more than one because a transfer streams a source result
         * set into a target batch: both statements are alive at the same time and
         * a cancel that only reached one of them would leave the other blocked.
         */
        private final AtomicReferenceArray<Statement> inFlight =
                new AtomicReferenceArray<>(SLOTS);

        /** Errors collected so far, in the ERROR envelope's shape. */
        private final List<JsonObject> errors = new ArrayList<>();

        private Thread thread;

        /**
         * @param session the session this job runs on
         * @param kind    the job kind, used to name the thread
         */
        protected Job(Session session, String kind) {
            this.session = session;
            this.kind = kind;
        }

        /** @return the session this job runs on. */
        public final Session session() {
            return session;
        }

        /**
         * Answers whether this job touches a session, in either direction.
         *
         * <p>{@code CLOSE_SESSION} asks this before it takes the connection lock.
         * The default is the session the job was started on; a job that also
         * holds a second connection - a transfer's target - has to say so, or
         * closing that second session waits on a lock nothing will release.
         *
         * @param s a session being closed
         * @return whether this job holds or uses {@code s}
         */
        public boolean uses(Session s) {
            return session == s;
        }

        /** @return this job's registry handle. */
        public final long handle() {
            return handle;
        }

        /**
         * Does the work.
         *
         * <p>Returning normally means {@code done}; throwing means {@code failed}
         * unless a cancel was requested, in which case the exception is assumed
         * to be the cancel taking effect and the state is {@code cancelled}.
         *
         * @throws Exception whatever went wrong
         */
        protected abstract void run() throws Exception;

        /**
         * Allocates the handle and starts the worker thread.
         *
         * <p>Split from the constructor so that a subclass is fully built before
         * a thread can observe it.
         */
        final void register() {
            handle = Registry.put(this);
            LIVE.put(handle, this);
            thread = new Thread(this::body, "rudbman-job-" + kind + "-" + handle);
            // Daemon: the process exits without a JVM shutdown, and a job must
            // never be the reason it hangs.
            thread.setDaemon(true);
            thread.start();
        }

        final void unregister() {
            LIVE.remove(handle);
            Registry.remove(handle);
        }

        private void body() {
            try {
                run();
                synchronized (this) {
                    state = cancelRequested ? CANCELLED : DONE;
                    if (DONE.equals(state)) {
                        phase = "done";
                    }
                }
            } catch (Throwable t) {
                synchronized (this) {
                    if (cancelRequested) {
                        // The driver threw because the statement was cancelled.
                        // That is the cancel working, not a failure, but the
                        // detail is kept so a confused user has something to read.
                        state = CANCELLED;
                        errors.add(Envelope.describe(t));
                    } else {
                        state = FAILED;
                        errors.add(Envelope.describe(t));
                    }
                }
                LOG.log(Level.FINE, "job " + handle + " ended abnormally", t);
            } finally {
                for (int i = 0; i < SLOTS; i++) {
                    inFlight.set(i, null);
                }
            }
        }

        /**
         * Runs one phase of work with the session's connection lock held.
         *
         * @param <T>  the result type
         * @param work the work
         * @return whatever {@code work} returned
         * @throws Exception whatever {@code work} threw
         */
        protected final <T> T runLocked(LockedWork<T> work) throws Exception {
            session.lock();
            try {
                return work.call();
            } finally {
                session.unlock();
            }
        }

        /** Work that needs the session's connection lock. */
        @FunctionalInterface
        protected interface LockedWork<T> {
            /**
             * @return the result
             * @throws Exception on failure
             */
            T call() throws Exception;
        }

        /**
         * Publishes the statement a cancel should interrupt.
         *
         * <p>Called with the statement before it is executed and with
         * {@code null} once it is closed. A cancel that arrived before this call
         * is applied immediately, so that the window between "cancel requested"
         * and "statement created" cannot swallow a cancel.
         *
         * @param stmt the statement, or {@code null} to clear
         */
        protected final void inFlight(Statement stmt) {
            inFlight(SOURCE, stmt);
        }

        /**
         * Publishes one of the statements a cancel should interrupt.
         *
         * @param slot {@link #SOURCE} or {@link #TARGET}; the two are
         *             independent, so clearing one leaves the other armed
         * @param stmt the statement, or {@code null} to clear
         */
        protected final void inFlight(int slot, Statement stmt) {
            inFlight.set(slot, stmt);
            if (stmt != null && cancelRequested) {
                cancelStatement(stmt);
            }
        }

        /**
         * Requests cancellation.
         *
         * <p>Takes no lock on purpose: the worker holds the session lock inside
         * the very statement this is meant to interrupt, and
         * {@link Statement#cancel()} is the one JDBC method documented to be
         * callable from another thread.
         *
         * @return whether the job was still running
         */
        public final boolean cancel() {
            if (isTerminal()) {
                return false;
            }
            cancelRequested = true;
            cancelInFlight();
            return true;
        }

        /** Issues {@link Statement#cancel()} on every statement still armed. */
        private void cancelInFlight() {
            for (int i = 0; i < SLOTS; i++) {
                Statement stmt = inFlight.get(i);
                if (stmt != null) {
                    cancelStatement(stmt);
                }
            }
        }

        private static void cancelStatement(Statement stmt) {
            try {
                stmt.cancel();
            } catch (SQLException | RuntimeException e) {
                // Not every driver supports cancel and a finished statement may
                // reject it; the flag alone still stops the worker at the next
                // row boundary.
                LOG.log(Level.FINE, "job statement cancel failed", e);
            }
        }

        /** @return whether cancellation has been requested. */
        protected final boolean cancelled() {
            return cancelRequested;
        }

        /**
         * @return whether the worker should stop: cancelled, or the session it
         *         runs on has been closed underneath it
         */
        protected final boolean shouldStop() {
            return cancelRequested || session.isClosed();
        }

        /** @return whether the job has finished, one way or another. */
        public final boolean isTerminal() {
            return !RUNNING.equals(state);
        }

        /**
         * @param name the phase name, e.g. {@code ddl} or {@code data:PUBLIC.T}
         */
        protected final void phase(String name) {
            phase = name;
        }

        /**
         * @param n rows processed since the last call
         */
        protected final void addRows(long n) {
            rowsDone += n;
        }

        /**
         * @param n rows dropped since the last call, under a transfer's
         *          {@code on_error} policy of {@code skip} or {@code log}. An
         *          extract or a backup never calls this and reports zero.
         */
        protected final void addSkipped(long n) {
            rowsSkipped += n;
        }

        /**
         * @param n bytes written since the last call
         */
        protected final void addBytes(long n) {
            bytes += n;
        }

        /**
         * @param n the total row count, when the caller asked for it to be
         *         counted up front; left at -1 otherwise
         */
        protected final void rowsTotal(long n) {
            rowsTotal = n;
        }

        /**
         * Records a failure that did not stop the job.
         *
         * <p>Capped at {@value #MAX_ERRORS} entries. Past the cap the failure is
         * dropped rather than appended: the array travels across JNI on every
         * poll, and the count the caller actually needs is already in
         * {@code rows_skipped}.
         *
         * @param t the failure
         */
        protected final void addError(Throwable t) {
            synchronized (this) {
                if (errors.size() < MAX_ERRORS) {
                    errors.add(Envelope.describe(t));
                }
            }
        }

        /**
         * Takes a progress reading.
         *
         * <p>The counters are not read under one lock, so a poll of a running
         * job may mix a counter from one instant with a phase from the next.
         * That is harmless for a progress bar and the one reading that matters is
         * exact: the worker writes every counter before it writes the terminal
         * state, and both are volatile, so a caller that sees a terminal state
         * sees the final numbers with it.
         *
         * @return the progress object of architecture.md 6:
         *         {@code {state, rows_done, rows_skipped, rows_total, bytes,
         *         phase, errors[], eta_s}}
         */
        public final JsonObject progress() {
            // A requested cancel is re-armed on every reading until the worker
            // acknowledges it. Statement.cancel only bites while a command is
            // actually executing, so a cancel that landed in the sliver between
            // the worker's last flag check and the driver entering execution
            // cancelled nothing — and no flag check can close that sliver from
            // the worker's side, because the worker is then blocked inside the
            // very call that needed cancelling. The poller is already knocking
            // every 200ms; each knock re-delivers the cancel until it lands.
            if (cancelRequested && !isTerminal()) {
                cancelInFlight();
            }
            JsonObject o = new JsonObject();
            o.addProperty("state", state);
            o.addProperty("rows_done", rowsDone);
            // Always present, zero for the jobs that cannot skip a row, so that
            // one progress shape serves every kind.
            o.addProperty("rows_skipped", rowsSkipped);
            // Absent rather than zero: a client that draws a determinate progress
            // bar has to be able to tell "no rows yet" from "no idea how many".
            o.addProperty("rows_total", rowsTotal < 0 ? null : rowsTotal);
            o.addProperty("bytes", bytes);
            o.addProperty("phase", phase);
            JsonArray arr = new JsonArray();
            synchronized (this) {
                for (JsonObject e : errors) {
                    arr.add(e);
                }
            }
            o.add("errors", arr);
            o.addProperty("eta_s", eta());
            return o;
        }

        /**
         * @return seconds remaining, or {@code null} whenever that is a guess -
         *         which, with no row total, is almost always
         */
        private Long eta() {
            long total = rowsTotal;
            long done = rowsDone;
            if (total <= 0 || done <= 0 || done >= total) {
                return null;
            }
            long elapsed = System.nanoTime() - startedNanos;
            if (elapsed <= 0) {
                return null;
            }
            double perRow = (double) elapsed / done;
            return (long) (perRow * (total - done) / 1_000_000_000d);
        }
    }
}
