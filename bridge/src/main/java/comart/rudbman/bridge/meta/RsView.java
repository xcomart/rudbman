package comart.rudbman.bridge.meta;

import com.google.gson.JsonObject;

import java.sql.ResultSet;
import java.sql.ResultSetMetaData;
import java.sql.SQLException;
import java.util.ArrayList;
import java.util.HashMap;
import java.util.LinkedHashMap;
import java.util.List;
import java.util.Locale;
import java.util.Map;

/**
 * Reader for {@link java.sql.DatabaseMetaData} result sets.
 *
 * <p>The JDBC spec marks many metadata columns optional, and real drivers do
 * omit them - H2 2.x dropped several that H2 1.x had. Asking such a result set
 * for a missing label throws, so the available labels are collected once and
 * only those are read. Exception-driven control flow per cell would be both
 * slower and noisier in the driver's own logs.
 *
 * <h2>Read a row's columns left to right</h2>
 *
 * <p>Reading by label makes the order of the calls look free. It is not. JDBC
 * lets a driver stream a column's value instead of buffering it, and requires
 * the application to read each column at most once and in ascending column
 * order; a streaming driver is free to discard everything to the left of the
 * column just fetched. Oracle's driver does exactly that: {@code getColumns}
 * hands back {@code COLUMN_DEF} (column 13) as a LONG stream, so touching
 * {@code IS_NULLABLE} (column 18) first closes it and the later read of the
 * default fails with {@code ORA-17027, stream has already been closed}. Any
 * table with a column default was unreadable while the reads here ran out of
 * order.
 *
 * <p>So every caller lists its reads in the order the JDBC javadoc numbers the
 * columns, even where the JSON it builds would read better in another order,
 * and reads each column exactly once - a value needed twice goes into a local.
 * The ordinals are written into the comments beside the reads, because the
 * labels alone do not reveal the order they have to be in.
 */
final class RsView {

    private final ResultSet rs;
    /** Label to 1-based column position, so reads can be sorted into column order. */
    private final Map<String, Integer> labels = new HashMap<>();

    RsView(ResultSet rs) throws SQLException {
        this.rs = rs;
        ResultSetMetaData md = rs.getMetaData();
        int n = md.getColumnCount();
        for (int i = 1; i <= n; i++) {
            labels.putIfAbsent(md.getColumnLabel(i).toUpperCase(Locale.ROOT), i);
        }
    }

    private boolean has(String label) {
        return labels.containsKey(label);
    }

    /**
     * Reads several string columns of the current row in ascending column order.
     *
     * <p>For a result set whose column order is not fixed by JDBC - a vendor
     * catalogue view, where the same output field lives at a different position
     * per product - the caller cannot put its reads in a safe order by hand.
     * This does it for them: the labels this result set actually has are sorted
     * by position and read left to right, and the caller composes its output
     * from the returned values in whatever order suits it.
     *
     * @param wanted candidate labels, absent ones ignored
     * @return the values that were present, keyed by label
     */
    Map<String, String> strs(String... wanted) throws SQLException {
        List<String> present = new ArrayList<>();
        for (String label : wanted) {
            if (has(label) && !present.contains(label)) {
                present.add(label);
            }
        }
        present.sort((a, b) -> Integer.compare(labels.get(a), labels.get(b)));
        Map<String, String> out = new LinkedHashMap<>();
        for (String label : present) {
            out.put(label, rs.getString(label));
        }
        return out;
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
