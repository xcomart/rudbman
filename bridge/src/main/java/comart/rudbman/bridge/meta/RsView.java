package comart.rudbman.bridge.meta;

import com.google.gson.JsonObject;

import java.sql.ResultSet;
import java.sql.ResultSetMetaData;
import java.sql.SQLException;
import java.util.HashSet;
import java.util.Locale;
import java.util.Set;

/**
 * Reader for {@link java.sql.DatabaseMetaData} result sets.
 *
 * <p>The JDBC spec marks many metadata columns optional, and real drivers do
 * omit them - H2 2.x dropped several that H2 1.x had. Asking such a result set
 * for a missing label throws, so the available labels are collected once and
 * only those are read. Exception-driven control flow per cell would be both
 * slower and noisier in the driver's own logs.
 */
final class RsView {

    private final ResultSet rs;
    private final Set<String> labels = new HashSet<>();

    RsView(ResultSet rs) throws SQLException {
        this.rs = rs;
        ResultSetMetaData md = rs.getMetaData();
        int n = md.getColumnCount();
        for (int i = 1; i <= n; i++) {
            labels.add(md.getColumnLabel(i).toUpperCase(Locale.ROOT));
        }
    }

    private boolean has(String label) {
        return labels.contains(label);
    }

    /** @param label metadata column label
     *  @return the string value, or {@code null} when the column is absent */
    String str(String label) throws SQLException {
        return has(label) ? rs.getString(label) : null;
    }

    /** @param label metadata column label
     *  @return the boxed int value, or {@code null} when absent or SQL NULL */
    Integer i32(String label) throws SQLException {
        if (!has(label)) {
            return null;
        }
        int v = rs.getInt(label);
        return rs.wasNull() ? null : v;
    }

    /** @param label metadata column label
     *  @return the boxed long value, or {@code null} when absent or SQL NULL */
    Long i64(String label) throws SQLException {
        if (!has(label)) {
            return null;
        }
        long v = rs.getLong(label);
        return rs.wasNull() ? null : v;
    }

    /** @param label metadata column label
     *  @return the boxed boolean value, or {@code null} when absent or SQL NULL */
    Boolean bool(String label) throws SQLException {
        if (!has(label)) {
            return null;
        }
        boolean v = rs.getBoolean(label);
        return rs.wasNull() ? null : v;
    }

    /**
     * Reads a JDBC {@code "YES"} / {@code "NO"} / {@code ""} tri-state.
     *
     * @param label metadata column label
     * @return {@code true}, {@code false}, or {@code null} when the driver says
     *         it does not know
     */
    Boolean yesNo(String label) throws SQLException {
        String s = str(label);
        if (s == null) {
            return null;
        }
        s = s.trim();
        if ("YES".equalsIgnoreCase(s)) {
            return Boolean.TRUE;
        }
        if ("NO".equalsIgnoreCase(s)) {
            return Boolean.FALSE;
        }
        return null;
    }

    /** Copies a string column into a JSON object under a snake_case key. */
    void putStr(JsonObject o, String key, String label) throws SQLException {
        o.addProperty(key, str(label));
    }

    /** Copies an int column into a JSON object under a snake_case key. */
    void putI32(JsonObject o, String key, String label) throws SQLException {
        o.addProperty(key, i32(label));
    }

    /** Copies a long column into a JSON object under a snake_case key. */
    void putI64(JsonObject o, String key, String label) throws SQLException {
        o.addProperty(key, i64(label));
    }

    /** Copies a boolean column into a JSON object under a snake_case key. */
    void putBool(JsonObject o, String key, String label) throws SQLException {
        o.addProperty(key, bool(label));
    }

    /** Copies a YES/NO column into a JSON object under a snake_case key. */
    void putYesNo(JsonObject o, String key, String label) throws SQLException {
        o.addProperty(key, yesNo(label));
    }
}
