package comart.rudbman.bridge;

import com.google.gson.JsonArray;
import com.google.gson.JsonObject;
import comart.rudbman.bridge.codec.BatchWriter;
import comart.rudbman.bridge.codec.ColumnKind;
import comart.rudbman.bridge.codec.ColumnWriter;
import comart.rudbman.bridge.codec.ColumnWriters;
import comart.rudbman.bridge.codec.LobSink;
import comart.rudbman.bridge.meta.SqlTypes;

import java.sql.ResultSet;
import java.sql.ResultSetMetaData;
import java.sql.SQLException;
import java.sql.Statement;
import java.util.concurrent.ConcurrentHashMap;
import java.util.concurrent.atomic.AtomicLong;
import java.util.logging.Level;
import java.util.logging.Logger;

/**
 * One executed statement: the {@link Statement}, its current {@link ResultSet}
 * and the batch encoder that turns rows into {@code RDB1} payloads.
 *
 * <p>A cursor exists even for statements that produced only an update count, so
 * that {@code MORE_RESULTS} always has something to advance and
 * {@code CLOSE_CURSOR} always has something to close.
 */
public final class Cursor {

    private static final Logger LOG = Logger.getLogger(Cursor.class.getName());

    /** Default batch size when the caller passes no positive row limit. */
    public static final int DEFAULT_FETCH = 500;
    /** Upper bound on a single batch, to keep one FETCH from eating the heap. */
    public static final int MAX_FETCH = 1_000_000;

    private final Session session;
    private final Statement stmt;
    private final long handle;

    private ResultSet rs;
    private int[] colTypes = new int[0];
    private int[] colPrecision = new int[0];
    private JsonArray columnsJson = new JsonArray();
    private long updateCount = -1;
    private boolean exhausted;
    private long rowsEmitted;
    private volatile boolean closed;

    private final AtomicLong lobSeq = new AtomicLong(1);
    private final ConcurrentHashMap<Long, LobRef> lobs = new ConcurrentHashMap<>();

    /** A LOB referenced from a batch but not carried in it. */
    public static final class LobRef {
        /** Zero-based row index within the cursor. */
        public final long row;
        /** One-based JDBC column index. */
        public final int column;
        /** Octets for binary LOBs, characters for character LOBs, -1 unknown. */
        public final long size;
        /** Whether the LOB is binary. */
        public final boolean binary;

        LobRef(long row, int column, long size, boolean binary) {
            this.row = row;
            this.column = column;
            this.size = size;
            this.binary = binary;
        }
    }

    /**
     * @param session owning session
     * @param stmt    the statement, not yet executed
     */
    Cursor(Session session, Statement stmt) {
        this.session = session;
        this.stmt = stmt;
        this.handle = Registry.put(this);
        session.addCursor(this);
    }

    /** @return this cursor's registry handle. */
    public long handle() {
        return handle;
    }

    /** @return the owning session. */
    public Session session() {
        return session;
    }

    /**
     * Records the state produced by {@code execute} or {@code getMoreResults}.
     *
     * @param isResultSet what the statement reported
     * @throws SQLException if the driver fails
     */
    void afterExecute(boolean isResultSet) throws SQLException {
        closeResultSet();
        rowsEmitted = 0;
        lobs.clear();
        if (isResultSet) {
            rs = stmt.getResultSet();
            updateCount = -1;
            describeColumns();
        } else {
            rs = null;
            updateCount = stmt.getUpdateCount();
            colTypes = new int[0];
            colPrecision = new int[0];
            columnsJson = new JsonArray();
        }
        exhausted = rs == null;
    }

    private void describeColumns() throws SQLException {
        ResultSetMetaData md = rs.getMetaData();
        int n = md.getColumnCount();
        colTypes = new int[n];
        colPrecision = new int[n];
        JsonArray arr = new JsonArray();
        for (int col = 1; col <= n; col++) {
            final int i = col;
            int type = safeInt(() -> md.getColumnType(i), java.sql.Types.OTHER);
            int precision = safeInt(() -> md.getPrecision(i), 0);
            colTypes[i - 1] = type;
            colPrecision[i - 1] = precision;

            JsonObject c = new JsonObject();
            c.addProperty("index", i);
            c.addProperty("name", safeStr(() -> md.getColumnName(i)));
            c.addProperty("label", safeStr(() -> md.getColumnLabel(i)));
            c.addProperty("table", safeStr(() -> md.getTableName(i)));
            c.addProperty("schema", safeStr(() -> md.getSchemaName(i)));
            c.addProperty("catalog", safeStr(() -> md.getCatalogName(i)));
            c.addProperty("type", type);
            c.addProperty("type_name", safeStr(() -> md.getColumnTypeName(i)));
            c.addProperty("jdbc_type", SqlTypes.name(type));
            c.addProperty("class_name", safeStr(() -> md.getColumnClassName(i)));
            c.addProperty("precision", precision);
            c.addProperty("scale", safeInt(() -> md.getScale(i), 0));
            c.addProperty("display_size", safeInt(() -> md.getColumnDisplaySize(i), 0));
            c.addProperty("nullable", safeInt(() -> md.isNullable(i), ResultSetMetaData.columnNullableUnknown));
            c.addProperty("auto_increment", safeBool(() -> md.isAutoIncrement(i)));
            c.addProperty("signed", safeBool(() -> md.isSigned(i)));
            c.addProperty("read_only", safeBool(() -> md.isReadOnly(i)));
            // The kind a full batch will use. Only a hint: a batch in which this
            // column is entirely NULL is emitted as kind 0 instead.
            c.addProperty("kind", ColumnKind.forSqlType(type, precision));
            arr.add(c);
        }
        columnsJson = arr;
    }

    /**
     * @return the {@code EXECUTE} / {@code MORE_RESULTS} response body for the
     *         current result of this statement
     */
    public JsonObject describe() {
        JsonObject o = new JsonObject();
        o.addProperty("cursor", handle);
        o.add("columns", columnsJson);
        o.addProperty("update_count", updateCount);
        o.addProperty("has_result_set", rs != null);
        // JDBC offers no way to look ahead without consuming the current
        // result, so this is a conservative hint: true means "MORE_RESULTS may
        // still return something", and the caller keeps calling until it is
        // false. See README, "has_more".
        o.addProperty("has_more", rs != null || updateCount >= 0);
        return o;
    }

    /**
     * Advances to the next result of the statement.
     *
     * @return the response body for the new result
     * @throws SQLException if the driver fails
     */
    public JsonObject moreResults() throws SQLException {
        boolean isRs = stmt.getMoreResults(Statement.CLOSE_CURRENT_RESULT);
        afterExecute(isRs);
        if (!isRs && updateCount < 0) {
            // Statement exhausted: no result set and no update count.
            JsonObject o = new JsonObject();
            o.addProperty("cursor", handle);
            o.add("columns", new JsonArray());
            o.addProperty("update_count", -1);
            o.addProperty("has_result_set", false);
            o.addProperty("has_more", false);
            return o;
        }
        return describe();
    }

    /**
     * Reads the next batch of rows.
     *
     * <p>Hot path: the row limit arrives as the raw {@code arg} of the JNI call,
     * so no JSON is parsed here.
     *
     * @param maxRows requested row count; values below 1 fall back to
     *                {@link #DEFAULT_FETCH}
     * @return an encoded {@code RDB1} batch
     * @throws SQLException if the driver fails
     */
    public byte[] fetch(long maxRows) throws SQLException {
        int want = maxRows <= 0 ? DEFAULT_FETCH : (int) Math.min(maxRows, MAX_FETCH);
        if (closed) {
            throw new BridgeException("protocol", "cursor " + handle + " is closed");
        }
        if (rs == null) {
            // A statement that produced only an update count has nothing to
            // fetch. An empty terminal batch is friendlier than an error and
            // lets the caller treat every cursor the same way.
            return BatchWriter.empty(true);
        }
        if (exhausted) {
            return new BatchWriter(newWriters(0)).finish(true);
        }

        session.lock();
        try {
            ColumnWriter[] writers = newWriters(want);
            BatchWriter batch = new BatchWriter(writers);
            boolean last = false;
            while (batch.rowCount() < want) {
                if (!rs.next()) {
                    exhausted = true;
                    last = true;
                    break;
                }
                batch.addRow(rs, rowsEmitted);
                rowsEmitted++;
            }
            return batch.finish(last);
        } finally {
            session.unlock();
        }
    }

    private ColumnWriter[] newWriters(int capacity) {
        ColumnWriter[] w = new ColumnWriter[colTypes.length];
        LobSink sink = this::registerLob;
        for (int i = 0; i < w.length; i++) {
            w[i] = ColumnWriters.forColumn(colTypes[i], colPrecision[i], capacity, sink);
        }
        return w;
    }

    private long registerLob(long row, int column, long size, boolean binary) {
        long id = lobSeq.getAndIncrement();
        lobs.put(id, new LobRef(row, column, size, binary));
        return id;
    }

    /**
     * @param id a LOB identifier previously written into a batch
     * @return the reference, or {@code null} when unknown
     */
    public LobRef lob(long id) {
        return lobs.get(id);
    }

    /**
     * Cancels the running statement.
     *
     * <p>Called from a thread other than the one executing the statement, and
     * without the session lock, which is the whole point.
     *
     * @return {@code true} when a cancel was issued
     */
    boolean cancel() {
        if (closed) {
            return false;
        }
        try {
            stmt.cancel();
            return true;
        } catch (SQLException | RuntimeException e) {
            // Not every driver supports cancel, and a statement that has already
            // finished may reject it. Neither is worth failing the CANCEL call.
            LOG.log(Level.FINE, "statement cancel failed", e);
            return false;
        }
    }

    private void closeResultSet() {
        if (rs != null) {
            try {
                rs.close();
            } catch (SQLException e) {
                LOG.log(Level.FINE, "cannot close result set", e);
            }
            rs = null;
        }
    }

    /** Closes the statement and drops the handle. */
    public void close() {
        if (closed) {
            return;
        }
        closed = true;
        session.removeCursor(this);
        Registry.remove(handle);
        session.lock();
        try {
            closeResultSet();
            try {
                stmt.close();
            } catch (SQLException e) {
                LOG.log(Level.FINE, "cannot close statement", e);
            }
        } finally {
            session.unlock();
        }
        lobs.clear();
    }

    /** Closes without ever throwing; used from session teardown. */
    void closeQuietly() {
        try {
            close();
        } catch (Throwable t) {
            LOG.log(Level.FINE, "cursor close failed", t);
        }
    }

    /** @return whether this cursor has been closed. */
    public boolean isClosed() {
        return closed;
    }

    // --- ResultSetMetaData is optional in practice ---------------------------
    // Plenty of drivers throw from one accessor while the rest work; losing a
    // display hint must not lose the whole result.

    private interface IntCall {
        int call() throws SQLException;
    }

    private interface StrCall {
        String call() throws SQLException;
    }

    private interface BoolCall {
        boolean call() throws SQLException;
    }

    private static int safeInt(IntCall c, int dflt) {
        try {
            return c.call();
        } catch (SQLException | RuntimeException | AbstractMethodError e) {
            return dflt;
        }
    }

    private static String safeStr(StrCall c) {
        try {
            return c.call();
        } catch (SQLException | RuntimeException | AbstractMethodError e) {
            return null;
        }
    }

    private static boolean safeBool(BoolCall c) {
        try {
            return c.call();
        } catch (SQLException | RuntimeException | AbstractMethodError e) {
            return false;
        }
    }
}
