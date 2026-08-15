package comart.rudbman.bridge;

import com.google.gson.JsonObject;
import comart.rudbman.bridge.job.Jobs;

import java.sql.Connection;
import java.sql.DatabaseMetaData;
import java.sql.Driver;
import java.sql.SQLException;
import java.sql.Statement;
import java.util.Map;
import java.util.Properties;
import java.util.concurrent.ConcurrentHashMap;
import java.util.concurrent.Executors;
import java.util.concurrent.ScheduledExecutorService;
import java.util.concurrent.TimeUnit;
import java.util.concurrent.locks.ReentrantLock;
import java.util.logging.Level;
import java.util.logging.Logger;

/**
 * One JDBC connection plus the class loader it was created from and its
 * keep-alive timer.
 *
 * <p>Derived from jdbgen's {@code comart.tools.jdbgen.types.db.DBMeta} (MIT,
 * Dennis Soungjin Park): the child class loader, the deliberate avoidance of
 * {@link java.sql.DriverManager}, the explicit null check on
 * {@link Driver#connect}, the connection lock and the keep-alive scheduler all
 * come from there.
 *
 * <p>The connection lock is kept even though the Rust side already serialises
 * commands per session: the keep-alive timer still runs concurrently with
 * whatever the worker is doing, and a JDBC connection is not thread safe.
 */
public final class Session implements AutoCloseable {

    private static final Logger LOG = Logger.getLogger(Session.class.getName());

    private final Connection conn;
    private final Loaders.Lease lease;
    private final ReentrantLock connLock = new ReentrantLock();
    private final String keepAliveQuery;
    private final String url;
    private final String driverClass;
    private final String tableCommentsSql;
    private final String columnCommentsSql;

    /** Attached after construction, because the timer task needs the session. */
    private ScheduledExecutorService keepAliveExec;

    /** Guarded by {@link #connLock}. */
    private DatabaseMetaData dbmeta;

    /**
     * Live cursors of this session.
     *
     * <p>Concurrent on purpose: {@code CANCEL} arrives on a different thread
     * while the worker thread is blocked inside {@code EXECUTE}, and it has to
     * reach the statement without taking {@link #connLock} - taking it would
     * mean waiting for the very statement it is trying to abort.
     */
    private final ConcurrentHashMap<Long, Cursor> cursors = new ConcurrentHashMap<>();

    private volatile boolean closed;
    private volatile long handle;

    private Session(Connection conn, Loaders.Lease lease, String url, String driverClass,
                    String keepAliveQuery, String tableCommentsSql, String columnCommentsSql) {
        this.conn = conn;
        this.lease = lease;
        this.url = url;
        this.driverClass = driverClass;
        this.keepAliveQuery = keepAliveQuery;
        this.tableCommentsSql = tableCommentsSql;
        this.columnCommentsSql = columnCommentsSql;
    }

    /**
     * Opens a session from an {@code OPEN_SESSION} request body.
     *
     * <p>Recognised members: {@code url} (required), {@code driver_class}
     * (required), {@code jars[]}, {@code username}, {@code password},
     * {@code props{}}, {@code read_only}, {@code auto_commit},
     * {@code login_timeout_s}, {@code keep_alive{enabled, interval_s, query}},
     * {@code table_comments_sql} and {@code column_comments_sql}.
     *
     * <p>The last two belong to the driver definition rather than to the
     * connection: they are the queries this product answers comments with, for
     * a product whose driver does not, and they are carried on the session
     * because {@code DESCRIBE} is where they are used. See
     * {@code comart.rudbman.bridge.meta.Comments}.
     *
     * @param req the parsed request body
     * @return a live session, already registered in the {@link Registry}
     * @throws Exception whatever the driver threw, unwrapped
     */
    public static Session open(JsonObject req) throws Exception {
        String url = Json.str(req, "url");
        String driverClass = Json.str(req, "driver_class");
        if (url == null || url.isEmpty()) {
            throw new BridgeException("protocol", "open_session requires 'url'");
        }
        if (driverClass == null || driverClass.isEmpty()) {
            throw new BridgeException("protocol", "open_session requires 'driver_class'");
        }

        Loaders.Lease lease = Loaders.acquire(Json.strings(req, "jars"));
        Connection conn = null;
        try {
            Driver driver = instantiate(driverClass, lease);

            Properties props = new Properties();
            String user = Json.str(req, "username");
            String pass = Json.str(req, "password");
            if (user != null) {
                props.setProperty("user", user);
            }
            if (pass != null) {
                props.setProperty("password", pass);
            }
            JsonObject extra = Json.obj(req, "props");
            if (extra != null) {
                for (Map.Entry<String, com.google.gson.JsonElement> e : extra.entrySet()) {
                    if (!e.getValue().isJsonNull()) {
                        props.setProperty(e.getKey(), e.getValue().getAsString());
                    }
                }
            }

            int loginTimeout = Json.i32(req, "login_timeout_s", 0);
            if (loginTimeout > 0) {
                // java.sql.Driver has no login timeout; only DriverManager's
                // global one and per-driver properties exist, and the global one
                // is the shared mutable state this bridge stays away from. A
                // property is the best portable approximation, and a caller that
                // knows its driver can always set the real key in 'props'.
                props.putIfAbsent("loginTimeout", Integer.toString(loginTimeout));
            }

            conn = driver.connect(url, props);
            if (conn == null) {
                // Per the JDBC spec a null return means "this driver does not
                // understand this URL". It is not an exception, and silently
                // dereferencing it would surface as a confusing NPE.
                throw new BridgeException("driver",
                        "driver '" + driverClass + "' does not accept the connection URL: " + url);
            }

            if (Json.bool(req, "read_only", false)) {
                conn.setReadOnly(true);
            }
            if (req.has("auto_commit")) {
                conn.setAutoCommit(Json.bool(req, "auto_commit", true));
            }

            JsonObject ka = Json.obj(req, "keep_alive");
            String kaQuery = null;
            int kaInterval = 0;
            if (ka != null && Json.bool(ka, "enabled", false)) {
                String q = Json.str(ka, "query");
                int interval = Json.i32(ka, "interval_s", 0);
                if (interval > 0 && q != null && !q.isEmpty()) {
                    kaQuery = q;
                    kaInterval = interval;
                }
            }

            Session s = new Session(conn, lease, url, driverClass, kaQuery,
                    sql(req, "table_comments_sql"), sql(req, "column_comments_sql"));
            s.handle = Registry.put(s);
            if (kaInterval > 0) {
                try {
                    s.keepAliveExec = s.startKeepAlive(kaInterval);
                } catch (Exception e) {
                    // A keep-alive that cannot be started must not take down an
                    // otherwise healthy connection.
                    LOG.log(Level.WARNING, "cannot start keep-alive", e);
                }
            }
            return s;
        } catch (Throwable t) {
            if (conn != null) {
                try {
                    conn.close();
                } catch (SQLException ignored) {
                    // The real failure is 't'; a close failure on an already
                    // broken connection adds nothing.
                }
            }
            lease.release();
            throw t;
        }
    }

    /**
     * @return an optional SQL member of the open request, or {@code null} when
     *         it is absent or holds nothing but whitespace. A driver definition
     *         with an empty text box has to mean the same thing as one where the
     *         box was never filled in, and the Rust side cannot tell the
     *         difference either.
     */
    private static String sql(JsonObject req, String member) {
        String s = Json.str(req, member);
        return s == null || s.isBlank() ? null : s;
    }

    private static Driver instantiate(String driverClass, Loaders.Lease lease) throws Exception {
        Class<?> cls;
        try {
            cls = Class.forName(driverClass, true, lease.loader());
        } catch (ClassNotFoundException e) {
            throw new BridgeException("driver",
                    "driver class not found: " + driverClass
                            + " (check the driver jar list)", e);
        }
        if (!Driver.class.isAssignableFrom(cls)) {
            throw new BridgeException("driver",
                    "class " + driverClass + " does not implement java.sql.Driver");
        }
        return (Driver) cls.getDeclaredConstructor().newInstance();
    }

    private ScheduledExecutorService startKeepAlive(int intervalSeconds) {
        ScheduledExecutorService exec = Executors.newSingleThreadScheduledExecutor(r -> {
            Thread t = new Thread(r, "rudbman-keepalive-" + handle);
            // Must never hold the JVM up; the process exits without a JVM teardown.
            t.setDaemon(true);
            return t;
        });
        // scheduleAtFixedRate silently stops rescheduling as soon as a task
        // throws, so nothing may escape the task body.
        exec.scheduleAtFixedRate(() -> {
            try {
                keepAlive();
            } catch (Throwable t) {
                LOG.log(Level.WARNING, "keep-alive failed", t);
            }
        }, intervalSeconds, intervalSeconds, TimeUnit.SECONDS);
        return exec;
    }

    private void keepAlive() {
        if (closed || keepAliveQuery == null) {
            return;
        }
        // A statement already in flight keeps the connection busy, which is all
        // the keep-alive wanted to achieve. Skip this round rather than queue.
        if (!connLock.tryLock()) {
            return;
        }
        try {
            if (closed || conn.isClosed()) {
                return;
            }
            try (Statement stmt = conn.createStatement()) {
                stmt.execute(keepAliveQuery);
            }
        } catch (Exception e) {
            LOG.log(Level.WARNING, "keep-alive statement failed", e);
        } finally {
            connLock.unlock();
        }
    }

    /** @return this session's registry handle. */
    public long handle() {
        return handle;
    }

    /** @return the JDBC URL this session was opened with. */
    public String url() {
        return url;
    }

    /** @return the driver class name this session was opened with. */
    public String driverClass() {
        return driverClass;
    }

    /**
     * @return the driver-defined table comment query, or {@code null} when this
     *         session was opened without one
     */
    public String tableCommentsSql() {
        return tableCommentsSql;
    }

    /**
     * @return the driver-defined column comment query, or {@code null} when this
     *         session was opened without one
     */
    public String columnCommentsSql() {
        return columnCommentsSql;
    }

    /**
     * @return the underlying connection; callers must hold {@link #lock()}
     */
    public Connection connection() {
        return conn;
    }

    /** Acquires the connection lock. */
    public void lock() {
        connLock.lock();
    }

    /** Releases the connection lock. */
    public void unlock() {
        connLock.unlock();
    }

    /**
     * @return the connection metadata; callers must hold {@link #lock()}
     * @throws SQLException if the driver fails
     */
    public DatabaseMetaData metaData() throws SQLException {
        // Cached like jdbgen did: some drivers build a fresh object on every
        // call, and the explorer tree asks for metadata constantly.
        if (dbmeta == null) {
            dbmeta = conn.getMetaData();
        }
        return dbmeta;
    }

    /**
     * Registers a cursor so that {@code CANCEL} can find its statement.
     *
     * @param c the cursor
     */
    void addCursor(Cursor c) {
        cursors.put(c.handle(), c);
    }

    /**
     * @param c the cursor to forget
     */
    void removeCursor(Cursor c) {
        cursors.remove(c.handle());
    }

    /**
     * Cancels every statement currently running on this session.
     *
     * <p>Deliberately does not take {@link #connLock}: it is called from another
     * thread precisely while the worker thread holds that lock inside a blocking
     * {@code execute}. {@link Statement#cancel()} is the one JDBC method
     * documented to be callable from a different thread.
     *
     * @return the number of statements a cancel was issued for
     */
    public int cancel() {
        int n = 0;
        for (Cursor c : cursors.values()) {
            if (c.cancel()) {
                n++;
            }
        }
        return n;
    }

    /** Closes every cursor, the connection, the keep-alive timer and the loader lease. */
    @Override
    public void close() {
        closed = true;
        // First, and deliberately before any lock is taken: a job worker is
        // holding the connection lock inside a streaming statement, and only a
        // statement cancel will make it let go. Closing the session behind a
        // running extract would otherwise block here until the extract finished.
        Jobs.cancelAll(this);
        if (keepAliveExec != null) {
            try {
                keepAliveExec.shutdownNow();
            } catch (Exception e) {
                LOG.log(Level.WARNING, "cannot stop keep-alive scheduler", e);
            }
        }
        for (Cursor c : cursors.values()) {
            c.closeQuietly();
        }
        cursors.clear();
        try {
            connLock.lock();
            try {
                conn.close();
            } catch (SQLException e) {
                LOG.log(Level.WARNING, "cannot close connection", e);
            } finally {
                connLock.unlock();
            }
        } finally {
            lease.release();
        }
    }

    /** @return whether {@link #close()} has been called. */
    public boolean isClosed() {
        return closed;
    }
}
