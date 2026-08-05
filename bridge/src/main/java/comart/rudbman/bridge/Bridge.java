package comart.rudbman.bridge;

import com.google.gson.JsonArray;
import com.google.gson.JsonObject;
import comart.rudbman.bridge.job.Jobs;
import comart.rudbman.bridge.meta.Describe;
import comart.rudbman.bridge.meta.SessionInfo;

import java.sql.Connection;
import java.sql.PreparedStatement;
import java.sql.SQLException;
import java.sql.Statement;

/**
 * The single JNI entry point of the bridge (architecture.md 4.3).
 *
 * <p>Rust caches exactly one {@code jmethodID} and dispatches everything through
 * {@link #call(int, long, long, byte[])}. Coarse granularity is the point: a
 * per-cell JNI round trip would be tens of millions of crossings for a 100k row
 * result.
 */
public final class Bridge {

    private Bridge() {
    }

    /**
     * Executes one bridge operation.
     *
     * <p><strong>Never throws and never returns null.</strong> Every failure,
     * including {@link Error}s, comes back as an ERROR envelope. That guarantee
     * is what lets the Rust side drop {@code ExceptionCheck} from the normal
     * path: an exception crossing this boundary would be a bridge bug, not a
     * database error.
     *
     * <p>{@link Throwable} is caught rather than {@link Exception} because
     * {@link NoClassDefFoundError} really does come out of an incomplete driver
     * jar, and that is a message for the user, not a reason to take the process
     * down.
     *
     * @param op     operation code, see {@link Ops}
     * @param handle session, cursor or job handle; 0 when the operation takes none
     * @param arg    integer argument for hot paths (the FETCH row limit, the
     *               SET_AUTOCOMMIT flag), so those calls parse no JSON at all
     * @param req    request body as UTF-8 JSON, or {@code null}
     * @return the response envelope of architecture.md 4.5
     */
    public static byte[] call(int op, long handle, long arg, byte[] req) {
        try {
            return dispatch(op, handle, arg, req);
        } catch (Throwable t) {
            try {
                return Envelope.error(t);
            } catch (Throwable fatal) {
                // Encoding the error failed too, which in practice means the
                // heap is gone. Hand back a constant that needed no allocation.
                return Envelope.lastResort();
            }
        }
    }

    private static byte[] dispatch(int op, long handle, long arg, byte[] req) throws Exception {
        switch (op) {
            case Ops.OPEN_SESSION:
                return openSession(req);
            case Ops.CLOSE_SESSION:
                return closeSession(handle);
            case Ops.PING:
                return ping(handle);
            case Ops.SESSION_INFO:
                return Envelope.ok(SessionInfo.of(Registry.session(handle)));
            case Ops.DESCRIBE:
                return Envelope.ok(Describe.run(Registry.session(handle), Json.request(req)));
            case Ops.EXECUTE:
                return execute(handle, Json.request(req));
            case Ops.FETCH:
                return Envelope.ok(Registry.cursor(handle).fetch(arg));
            case Ops.MORE_RESULTS:
                return moreResults(handle);
            case Ops.CLOSE_CURSOR:
                Registry.cursor(handle).close();
                return Envelope.ok();
            case Ops.CANCEL:
                return cancel(handle);
            case Ops.SET_AUTOCOMMIT:
                return setAutoCommit(handle, arg != 0);
            case Ops.COMMIT:
                return commit(handle);
            case Ops.ROLLBACK:
                return rollback(handle);
            case Ops.PROBE_DRIVER:
                return Envelope.ok(DriverProbe.probe(Json.request(req)));
            case Ops.JOB_START:
                return Envelope.ok(Jobs.start(Registry.session(handle), Json.request(req)));
            case Ops.JOB_POLL:
                return Envelope.ok(Jobs.poll(handle));
            case Ops.JOB_CANCEL:
                return Envelope.ok(Jobs.cancel(handle));
            case Ops.LOB_READ:
                throw new BridgeException("protocol",
                        "operation 0x" + Integer.toHexString(op)
                                + " is not implemented in this build");
            default:
                throw new BridgeException("protocol",
                        "unknown operation 0x" + Integer.toHexString(op));
        }
    }

    private static byte[] openSession(byte[] req) throws Exception {
        Session s = Session.open(Json.request(req));
        JsonObject o = new JsonObject();
        o.addProperty("session", s.handle());
        return Envelope.ok(o);
    }

    private static byte[] closeSession(long handle) {
        Session s = Registry.session(handle);
        // Removed first so a second CLOSE_SESSION reports a stale handle instead
        // of racing into the teardown.
        Registry.remove(handle);
        s.close();
        return Envelope.ok();
    }

    private static byte[] ping(long handle) throws SQLException {
        Session s = Registry.session(handle);
        long t0 = System.nanoTime();
        boolean ok;
        s.lock();
        try {
            Connection c = s.connection();
            try {
                ok = c.isValid(5);
            } catch (SQLException | AbstractMethodError e) {
                // isValid is JDBC 4.0 and a handful of drivers still do not
                // implement it; a trivial round trip proves the same thing.
                try (Statement st = c.createStatement()) {
                    st.execute("select 1");
                    ok = true;
                }
            }
        } finally {
            s.unlock();
        }
        JsonObject o = new JsonObject();
        o.addProperty("ok", ok);
        o.addProperty("elapsed_ms", (System.nanoTime() - t0) / 1_000_000L);
        return Envelope.ok(o);
    }

    private static byte[] execute(long handle, JsonObject req) throws SQLException {
        Session s = Registry.session(handle);
        String sql = Json.str(req, "sql");
        if (sql == null || sql.isEmpty()) {
            throw new BridgeException("protocol", "execute requires 'sql'");
        }
        JsonArray params = Json.arr(req, "params");
        int fetchSize = Json.i32(req, "fetch_size", 0);
        int maxRows = Json.i32(req, "max_rows", 0);
        int timeout = Json.i32(req, "timeout_s", 0);

        s.lock();
        Cursor cursor = null;
        try {
            Statement stmt;
            boolean prepared = params != null && params.size() > 0;
            if (prepared) {
                PreparedStatement ps = s.connection().prepareStatement(sql);
                Params.bind(ps, params);
                stmt = ps;
            } else {
                stmt = s.connection().createStatement();
            }
            if (fetchSize > 0) {
                stmt.setFetchSize(fetchSize);
            }
            if (maxRows > 0) {
                stmt.setMaxRows(maxRows);
            }
            if (timeout > 0) {
                stmt.setQueryTimeout(timeout);
            }

            // Registered before execute so that a CANCEL arriving mid-statement
            // finds the statement. This is the whole reason the cursor table is
            // concurrent.
            cursor = new Cursor(s, stmt);
            boolean isRs = prepared ? ((PreparedStatement) stmt).execute() : stmt.execute(sql);
            cursor.afterExecute(isRs);
            return Envelope.ok(cursor.describe());
        } catch (Throwable t) {
            if (cursor != null) {
                cursor.closeQuietly();
            }
            throw t;
        } finally {
            s.unlock();
        }
    }

    private static byte[] moreResults(long handle) throws SQLException {
        Cursor c = Registry.cursor(handle);
        c.session().lock();
        try {
            return Envelope.ok(c.moreResults());
        } finally {
            c.session().unlock();
        }
    }

    private static byte[] cancel(long handle) {
        // Deliberately takes no lock: the worker thread is holding it inside the
        // very statement this call is meant to interrupt.
        Session s = Registry.session(handle);
        JsonObject o = new JsonObject();
        o.addProperty("cancelled", s.cancel());
        return Envelope.ok(o);
    }

    private static byte[] setAutoCommit(long handle, boolean on) throws SQLException {
        Session s = Registry.session(handle);
        s.lock();
        try {
            s.connection().setAutoCommit(on);
        } finally {
            s.unlock();
        }
        return Envelope.ok();
    }

    private static byte[] commit(long handle) throws SQLException {
        Session s = Registry.session(handle);
        s.lock();
        try {
            s.connection().commit();
        } finally {
            s.unlock();
        }
        return Envelope.ok();
    }

    private static byte[] rollback(long handle) throws SQLException {
        Session s = Registry.session(handle);
        s.lock();
        try {
            s.connection().rollback();
        } finally {
            s.unlock();
        }
        return Envelope.ok();
    }
}
