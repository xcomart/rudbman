package comart.rudbman.bridge.job;

import com.google.gson.JsonArray;
import com.google.gson.JsonElement;
import com.google.gson.JsonObject;
import comart.rudbman.bridge.BridgeException;
import comart.rudbman.bridge.Json;
import comart.rudbman.bridge.Registry;
import comart.rudbman.bridge.Session;
import comart.rudbman.bridge.meta.Ddl;
import comart.rudbman.bridge.meta.Dialect;
import comart.rudbman.bridge.meta.Ident;
import comart.rudbman.bridge.meta.Upsert;

import java.sql.Connection;
import java.sql.DatabaseMetaData;
import java.sql.PreparedStatement;
import java.sql.ResultSet;
import java.sql.ResultSetMetaData;
import java.sql.SQLException;
import java.sql.Savepoint;
import java.sql.Statement;
import java.util.ArrayList;
import java.util.List;
import java.util.logging.Level;
import java.util.logging.Logger;

/**
 * {@code kind: "transfer"} - rows from a query on one session into a table on
 * another, without a single row crossing the JNI boundary (architecture.md 6).
 *
 * <h2>Request</h2>
 *
 * <pre>
 * { kind: "transfer",
 *   source_sql:     "SELECT …",
 *   target_session: &lt;handle&gt;,
 *   target_table:   {catalog?, schema?, name},
 *   mode:           "insert" | "upsert" | "truncate_insert",
 *   batch_size:     500,
 *   commit_every:   10000,
 *   column_map:     [{from, to}],
 *   on_error:       "abort" | "skip" | "log" }
 * </pre>
 *
 * <p>{@code JOB_START} is called with the <em>source</em> session's handle; the
 * target arrives as a handle in the body.
 *
 * <h2>Locking</h2>
 *
 * <p>Both connection locks are held for the whole stream, taken in ascending
 * {@link Session#handle()} order so that two transfers running in opposite
 * directions cannot deadlock. Letting go between batches is not an option: the
 * source result set and the target transaction both die if another statement
 * runs on their connection. A transfer into the same session it reads from is
 * safe because the lock is reentrant.
 *
 * <p>The consequence for callers is the extract's, doubled: {@code EXECUTE}
 * blocks on <em>both</em> sessions while a transfer runs, so a UI that wants to
 * keep querying opens a third session. It also means {@link #uses(Session)} has
 * to name the target - {@code CLOSE_SESSION} takes the connection lock, and only
 * a cancel will make the worker let go of it.
 *
 * <h2>Transactions</h2>
 *
 * <p>The target's auto-commit is turned off and restored at the end. A commit
 * happens every {@code commit_every} rows and once more on success; a failure or
 * a cancel rolls back the uncommitted tail and <strong>leaves everything already
 * committed in place</strong>, which is what {@code rows_done} reports.
 */
public final class TransferJob extends Jobs.Job {

    private static final Logger LOG = Logger.getLogger(TransferJob.class.getName());

    /** Rows requested per round trip from the source. */
    private static final int FETCH_SIZE = 1000;

    /** Rows per {@code executeBatch} when the request does not say. */
    private static final int DEFAULT_BATCH_SIZE = 500;

    /** Rows per target commit when the request does not say. */
    private static final long DEFAULT_COMMIT_EVERY = 10_000;

    private final String sourceSql;
    private final Session target;

    private final String catalog;
    private final String schema;
    private final String table;

    private final String mode;
    private final String onError;
    private final int batchSize;
    private final long commitEvery;

    /** Source column names in {@code column_map} order; empty means "all of them". */
    private final List<String> mapFrom = new ArrayList<>();
    /** Target column names, index-aligned with {@link #mapFrom}. */
    private final List<String> mapTo = new ArrayList<>();

    /** The upsert conflict key, read from the target's metadata up front. */
    private final List<String> keyColumns = new ArrayList<>();

    /**
     * Validates a job specification.
     *
     * <p>Runs on the caller's thread so that a malformed request comes back from
     * {@code JOB_START} itself. That includes the one check that needs the
     * database: an {@code upsert} without a primary key on the target has no
     * conflict key and can never work, so it is rejected here rather than
     * discovered by a poll.
     *
     * @param session the source session, the one {@code JOB_START} was called on
     * @param spec    the {@code JOB_START} body
     */
    TransferJob(Session session, JsonObject spec) {
        super(session, "transfer");

        String sql = Json.str(spec, "source_sql");
        if (sql == null || sql.trim().isEmpty()) {
            throw new BridgeException("protocol", "transfer requires 'source_sql'");
        }
        sourceSql = sql;

        long handle = Json.i64(spec, "target_session", 0);
        if (handle == 0) {
            throw new BridgeException("protocol", "transfer requires 'target_session'");
        }
        // Resolves, or fails with the protocol error the registry already writes
        // for an unknown handle or one that names a cursor.
        target = Registry.session(handle);

        JsonObject t = Json.obj(spec, "target_table");
        String name = t == null ? null : Json.str(t, "name");
        if (name == null || name.isEmpty()) {
            throw new BridgeException("protocol", "transfer requires 'target_table.name'");
        }
        catalog = Json.str(t, "catalog");
        schema = Json.str(t, "schema");
        table = name;

        String m = Json.str(spec, "mode");
        mode = m == null || m.isEmpty() ? "insert" : m;
        if (!"insert".equals(mode) && !"upsert".equals(mode)
                && !"truncate_insert".equals(mode)) {
            throw new BridgeException("protocol",
                    "mode must be 'insert', 'upsert' or 'truncate_insert', not '" + mode + "'");
        }

        String oe = Json.str(spec, "on_error");
        onError = oe == null || oe.isEmpty() ? "abort" : oe;
        if (!"abort".equals(onError) && !"skip".equals(onError) && !"log".equals(onError)) {
            throw new BridgeException("protocol",
                    "on_error must be 'abort', 'skip' or 'log', not '" + onError + "'");
        }

        batchSize = Math.max(1, Json.i32(spec, "batch_size", DEFAULT_BATCH_SIZE));
        commitEvery = Math.max(0, Json.i64(spec, "commit_every", DEFAULT_COMMIT_EVERY));

        JsonArray map = Json.arr(spec, "column_map");
        if (map != null) {
            for (JsonElement e : map) {
                if (!e.isJsonObject()) {
                    throw new BridgeException("protocol",
                            "each 'column_map' entry must be an object");
                }
                JsonObject o = e.getAsJsonObject();
                String from = Json.str(o, "from");
                String to = Json.str(o, "to");
                if (from == null || from.isEmpty() || to == null || to.isEmpty()) {
                    throw new BridgeException("protocol",
                            "each 'column_map' entry requires 'from' and 'to'");
                }
                mapFrom.add(from);
                mapTo.add(to);
            }
        }

        if ("upsert".equals(mode)) {
            readConflictKey();
        }
    }

    /**
     * Reads the target's primary key, briefly holding the target's lock.
     *
     * <p>Short and on the calling thread on purpose: this is the one piece of
     * validation that has to ask the database, and the answer decides whether
     * {@code JOB_START} succeeds at all.
     */
    private void readConflictKey() {
        target.lock();
        try {
            DatabaseMetaData dbm = target.metaData();
            if (!Upsert.supported(Dialect.of(dbm))) {
                throw new BridgeException("protocol",
                        "mode 'upsert' has no portable form on this product; "
                                + "use 'insert' or 'truncate_insert'");
            }
            Ddl.readPrimaryKey(dbm, catalog, schema, table, keyColumns);
        } catch (SQLException e) {
            throw new BridgeException("sql",
                    "cannot read the primary key of the transfer target", e);
        } finally {
            target.unlock();
        }
        if (keyColumns.isEmpty()) {
            throw new BridgeException("protocol",
                    "mode 'upsert' needs a primary key on the target table, and "
                            + table + " has none");
        }
    }

    @Override
    public boolean uses(Session s) {
        return session() == s || target == s;
    }

    @Override
    protected void run() throws Exception {
        // Ascending handle order, so that a transfer A→B and a transfer B→A
        // cannot each hold what the other is waiting for. Handles are unique and
        // monotonic, which is all a lock order needs.
        Session first = session().handle() <= target.handle() ? session() : target;
        Session second = first == session() ? target : session();
        first.lock();
        try {
            // When source and target are the same session this is the same lock
            // again; ReentrantLock counts, and the unlocks below match.
            second.lock();
            try {
                transfer();
            } finally {
                second.unlock();
            }
        } finally {
            first.unlock();
        }
    }

    private void transfer() throws Exception {
        phase("transfer");
        try (Statement st = session().connection().createStatement()) {
            try {
                st.setFetchSize(FETCH_SIZE);
            } catch (SQLException ignored) {
                // A driver entitled to refuse the hint; nothing is lost.
            }
            inFlight(SOURCE, st);
            // A cancel that raced the statement's creation may have been
            // answered by a Statement.cancel on a statement that was not yet
            // executing, which a driver may treat as a no-op. The flag is the
            // durable half of the request, so it gets one last look.
            if (shouldStop()) {
                return;
            }
            try (ResultSet rs = st.executeQuery(sourceSql)) {
                write(rs);
            } finally {
                inFlight(SOURCE, null);
            }
        }
    }

    // ---------------------------------------------------------------- writing

    private void write(ResultSet rs) throws Exception {
        int[] sourceIndex = resolveSourceColumns(rs.getMetaData());
        List<String> columns = targetColumns(rs.getMetaData(), sourceIndex);

        Connection conn = target.connection();
        DatabaseMetaData dbm = target.metaData();
        Dialect dialect = Dialect.of(dbm);
        Ident id = Ident.of(dbm);
        String qualified = id.qualify(catalog, schema, table);
        String sql = statement(dialect, id, qualified, columns);

        boolean autoCommit = conn.getAutoCommit();
        if (autoCommit) {
            conn.setAutoCommit(false);
        }
        boolean pending = true;
        try {
            if ("truncate_insert".equals(mode)) {
                emptyTarget(conn, qualified);
            }
            try (PreparedStatement ps = conn.prepareStatement(sql)) {
                inFlight(TARGET, ps);
                try {
                    stream(rs, ps, conn, sourceIndex, savepointsUsable(dbm));
                } finally {
                    inFlight(TARGET, null);
                }
            }
            if (!shouldStop()) {
                conn.commit();
                pending = false;
            }
        } finally {
            // Order matters: setAutoCommit(true) commits the open transaction on
            // most drivers, so the tail has to be rolled back before it.
            if (pending) {
                quietly(conn::rollback, "cannot roll back the transfer's tail");
            }
            if (autoCommit) {
                quietly(() -> conn.setAutoCommit(true), "cannot restore auto-commit");
            }
        }
    }

    /**
     * {@code DELETE FROM}, not {@code TRUNCATE}: truncation is a dialect,
     * privilege and transactionality minefield, while a delete means the same
     * thing everywhere and rolls back with the rest of the transfer.
     */
    private void emptyTarget(Connection conn, String qualified) throws SQLException {
        try (Statement st = conn.createStatement()) {
            inFlight(TARGET, st);
            try {
                if (shouldStop()) {
                    return;
                }
                st.executeUpdate("DELETE FROM " + qualified);
            } finally {
                inFlight(TARGET, null);
            }
        }
    }

    private void stream(ResultSet rs, PreparedStatement ps, Connection conn, int[] sourceIndex,
                        boolean savepoints) throws Exception {
        int n = sourceIndex.length;
        // With no savepoint to undo a half-applied batch, a failed batch cannot
        // be replayed row by row without risking duplicates, so a row at a time
        // is the only safe unit. Only the forgiving on_error policies pay this.
        int batch = abortOnError() || savepoints ? batchSize : 1;
        List<Object[]> buffered = new ArrayList<>(batch);
        long sinceCommit = 0;

        while (rs.next()) {
            if (shouldStop()) {
                return;
            }
            Object[] values = new Object[n];
            for (int i = 0; i < n; i++) {
                values[i] = rs.getObject(sourceIndex[i]);
            }
            bind(ps, values);
            ps.addBatch();
            buffered.add(values);

            if (buffered.size() >= batch) {
                sinceCommit += flush(ps, conn, buffered, savepoints);
                if (commitEvery > 0 && sinceCommit >= commitEvery) {
                    conn.commit();
                    sinceCommit = 0;
                }
            }
        }
        if (!shouldStop()) {
            flush(ps, conn, buffered, savepoints);
        }
    }

    /**
     * Executes one batch and reports how many rows reached the target.
     *
     * <p>Under {@code on_error: "abort"} the driver's exception simply
     * propagates. Under {@code skip} or {@code log} the batch is undone to its
     * savepoint and replayed a row at a time, so that one poisoned row costs
     * only itself - the rest of the batch is genuinely written, not lost with
     * it.
     */
    private long flush(PreparedStatement ps, Connection conn, List<Object[]> buffered,
                       boolean savepoints) throws SQLException {
        if (buffered.isEmpty()) {
            return 0;
        }
        Savepoint before = savepoints && !abortOnError() ? conn.setSavepoint() : null;
        try {
            ps.executeBatch();
            long done = buffered.size();
            addRows(done);
            release(conn, before);
            buffered.clear();
            return done;
        } catch (SQLException e) {
            if (abortOnError()) {
                throw e;
            }
            clearBatch(ps);
            long done;
            if (before != null) {
                conn.rollback(before);
                done = replay(ps, conn, buffered);
            } else {
                // Batch size is 1 in this branch, so the failure is this row's.
                addSkipped(buffered.size());
                logDropped(e);
                done = 0;
            }
            buffered.clear();
            return done;
        }
    }

    /** Re-runs a failed batch one row at a time, fencing each row with a savepoint. */
    private long replay(PreparedStatement ps, Connection conn, List<Object[]> buffered)
            throws SQLException {
        long done = 0;
        for (Object[] values : buffered) {
            if (shouldStop()) {
                break;
            }
            Savepoint row = conn.setSavepoint();
            try {
                bind(ps, values);
                ps.executeUpdate();
                done++;
                addRows(1);
                release(conn, row);
            } catch (SQLException e) {
                // Without the rollback a product that aborts the whole
                // transaction on any statement error - PostgreSQL does - would
                // fail every remaining row of the transfer too.
                try {
                    conn.rollback(row);
                } catch (SQLException ignored) {
                    LOG.log(Level.FINE, "cannot roll back a skipped row", ignored);
                }
                addSkipped(1);
                logDropped(e);
            }
        }
        return done;
    }

    private void logDropped(SQLException e) {
        if ("log".equals(onError)) {
            addError(e);
        }
    }

    private boolean abortOnError() {
        return "abort".equals(onError);
    }

    private static void bind(PreparedStatement ps, Object[] values) throws SQLException {
        // getObject / setObject throughout: type coercion is the target driver's
        // job, and an exotic value that will not cross takes the on_error path.
        for (int i = 0; i < values.length; i++) {
            ps.setObject(i + 1, values[i]);
        }
    }

    private static void clearBatch(PreparedStatement ps) {
        try {
            ps.clearBatch();
        } catch (SQLException e) {
            // Several drivers clear the batch themselves on failure and reject
            // the call; the following bind sequence rebuilds it either way.
            LOG.log(Level.FINE, "cannot clear a failed batch", e);
        }
    }

    private static void release(Connection conn, Savepoint sp) {
        if (sp == null) {
            return;
        }
        try {
            conn.releaseSavepoint(sp);
        } catch (SQLException | RuntimeException e) {
            // Optional in JDBC and refused by several drivers. A savepoint that
            // is not released is freed by the next commit anyway.
            LOG.log(Level.FINE, "cannot release a savepoint", e);
        }
    }

    private static boolean savepointsUsable(DatabaseMetaData dbm) {
        try {
            return dbm.supportsSavepoints();
        } catch (Exception | AbstractMethodError e) {
            return false;
        }
    }

    private static void quietly(SqlAction action, String message) {
        try {
            action.run();
        } catch (SQLException | RuntimeException e) {
            LOG.log(Level.WARNING, message, e);
        }
    }

    /** Cleanup that must not replace the failure already on its way out. */
    @FunctionalInterface
    private interface SqlAction {
        /** @throws SQLException if the driver fails */
        void run() throws SQLException;
    }

    // ---------------------------------------------------------------- columns

    /**
     * Maps target column positions onto source result columns.
     *
     * <p>A {@code column_map} whose {@code from} names a column the query did
     * not return is only discoverable once the query has run, so unlike the rest
     * of the specification it fails the job instead of the {@code JOB_START}
     * call.
     */
    private int[] resolveSourceColumns(ResultSetMetaData md) throws SQLException {
        int n = md.getColumnCount();
        if (mapFrom.isEmpty()) {
            int[] all = new int[n];
            for (int i = 0; i < n; i++) {
                all[i] = i + 1;
            }
            return all;
        }
        int[] picked = new int[mapFrom.size()];
        for (int i = 0; i < mapFrom.size(); i++) {
            picked[i] = -1;
            for (int c = 1; c <= n; c++) {
                if (mapFrom.get(i).equalsIgnoreCase(md.getColumnLabel(c))) {
                    picked[i] = c;
                    break;
                }
            }
            if (picked[i] < 0) {
                throw new BridgeException("protocol",
                        "column_map 'from' column '" + mapFrom.get(i)
                                + "' is not in the source result");
            }
        }
        return picked;
    }

    private List<String> targetColumns(ResultSetMetaData md, int[] sourceIndex)
            throws SQLException {
        if (!mapTo.isEmpty()) {
            return mapTo;
        }
        List<String> names = new ArrayList<>(sourceIndex.length);
        for (int c : sourceIndex) {
            names.add(md.getColumnLabel(c));
        }
        return names;
    }

    private String statement(Dialect dialect, Ident id, String qualified, List<String> columns) {
        if (!"upsert".equals(mode)) {
            StringBuilder cols = new StringBuilder();
            StringBuilder marks = new StringBuilder();
            for (String c : columns) {
                if (cols.length() > 0) {
                    cols.append(", ");
                    marks.append(", ");
                }
                cols.append(id.q(c));
                marks.append('?');
            }
            return "INSERT INTO " + qualified + " (" + cols + ") VALUES (" + marks + ")";
        }
        if (!Upsert.covers(columns, keyColumns)) {
            throw new BridgeException("protocol",
                    "mode 'upsert' needs every key column written; "
                            + keyColumns + " is not covered by " + columns);
        }
        return Upsert.sql(dialect, id, qualified, columns, keyColumns);
    }
}
