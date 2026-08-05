package comart.rudbman.bridge.meta;

import java.sql.Connection;
import java.sql.SQLException;
import java.sql.Savepoint;
import java.sql.Statement;
import java.util.logging.Level;
import java.util.logging.Logger;

/**
 * Runs a vendor catalogue query that is allowed to fail.
 *
 * <p>Both {@code ddl} and {@code sequences} probe for syntax the server may not
 * have: {@code SHOW CREATE TABLE} on something that turns out not to be MySQL,
 * {@code information_schema.SEQUENCES} on a MariaDB build that never added it,
 * {@code ALL_SEQUENCES} without the privilege to read it. A failure there is an
 * answer ("no native DDL", "no sequences"), not an error to report.
 *
 * <p>The catch is that on some servers - PostgreSQL most strictly - a statement
 * that raises inside an open transaction poisons the whole transaction, and
 * every later statement fails with "current transaction is aborted" until a
 * rollback. A speculative query that costs the user their uncommitted work is
 * not an acceptable trade for a metadata guess, so when the session is not in
 * auto-commit the attempt is fenced with a savepoint and rolled back to it.
 *
 * <p>A driver that cannot make a savepoint is not a reason to refuse the query;
 * the attempt then simply runs unfenced, which is exactly where it stood before.
 */
final class Attempt {

    private static final Logger LOG = Logger.getLogger(Attempt.class.getName());

    /** What to do with a statement on the connection. */
    interface Body<T> {
        /**
         * @param st a statement, closed by the caller
         * @return the result
         * @throws SQLException if the server rejects the query
         */
        T run(Statement st) throws SQLException;
    }

    private Attempt() {
    }

    /**
     * Runs {@code body}, swallowing any failure.
     *
     * @param conn     the connection
     * @param what     short description for the debug log
     * @param body     the query
     * @param fallback returned when the attempt fails
     * @param <T>      result type
     * @return the body's result, or {@code fallback}
     */
    static <T> T run(Connection conn, String what, Body<T> body, T fallback) {
        Savepoint sp = fence(conn);
        try (Statement st = conn.createStatement()) {
            T out = body.run(st);
            release(conn, sp);
            return out;
        } catch (SQLException | RuntimeException e) {
            LOG.log(Level.FINE, "optional metadata query failed: " + what, e);
            rollback(conn, sp);
            return fallback;
        }
    }

    private static Savepoint fence(Connection conn) {
        try {
            return conn.getAutoCommit() ? null : conn.setSavepoint();
        } catch (SQLException | RuntimeException e) {
            LOG.log(Level.FINE, "cannot fence a speculative query with a savepoint", e);
            return null;
        }
    }

    private static void rollback(Connection conn, Savepoint sp) {
        if (sp == null) {
            return;
        }
        try {
            conn.rollback(sp);
        } catch (SQLException | RuntimeException e) {
            LOG.log(Level.FINE, "cannot roll back to savepoint", e);
        }
    }

    private static void release(Connection conn, Savepoint sp) {
        if (sp == null) {
            return;
        }
        try {
            conn.releaseSavepoint(sp);
        } catch (SQLException | RuntimeException e) {
            // Oracle has no releaseSavepoint; the savepoint just lives until the
            // transaction ends, which is harmless.
            LOG.log(Level.FINEST, "cannot release savepoint", e);
        }
    }
}
