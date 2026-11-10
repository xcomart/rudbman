package comart.rudbman.bridge.support;

import java.nio.charset.StandardCharsets;
import java.sql.Connection;
import java.sql.DatabaseMetaData;
import java.sql.ResultSet;
import java.sql.ResultSetMetaData;
import java.sql.SQLException;
import java.sql.Statement;
import java.sql.Types;
import java.util.ArrayList;
import java.util.Collections;
import java.util.Comparator;
import java.util.List;
import java.util.Map;
import java.util.TreeMap;
import java.util.regex.Pattern;

/**
 * What a schema holds, as sorted lines that two schemas can be compared by:
 * the JDBC metadata of every table, the product's own catalog rows a catalog
 * file asks for, every row of every table, and last the probes - the next
 * value a sequence or an identity hands out.
 *
 * <p>A catalog file is blocks split by a line {@code --;;}, each starting with
 * a {@code -- label} line:
 *
 * <ul>
 *   <li>{@code -- exec ...}: a statement run first, for its effect;</li>
 *   <li>{@code -- probe ...}: a query of {@code (key, sql)} pairs, each
 *       {@code sql} run after the rows are read and its first value kept;</li>
 *   <li>{@code -- probe-rollback ...}: the same, each {@code sql} run in a
 *       transaction that is rolled back - an insert that draws a key;</li>
 *   <li>anything else: a query whose rows are kept under the label.</li>
 * </ul>
 *
 * <p>Plain JDBC on purpose: the snapshot has to see the schema the way the
 * product does, not the way the bridge under test does.
 */
public final class SchemaSnapshot {

    private SchemaSnapshot() {
    }

    /**
     * @param conn    a connection to the schema
     * @param catalog the catalog, or {@code null}
     * @param schema  the schema, or {@code null}
     * @param queries the catalog file's text
     * @param skip    the table names to leave out
     * @return the lines
     */
    public static List<String> of(Connection conn, String catalog, String schema, String queries,
                                  Pattern skip) throws SQLException {
        List<String> out = new ArrayList<>();
        DatabaseMetaData m = conn.getMetaData();
        String q = m.getIdentifierQuoteString().trim();
        List<String> tables = new ArrayList<>();
        try (ResultSet rs = m.getTables(catalog, schema, "%", new String[]{"TABLE"})) {
            while (rs.next()) {
                if (!skip.matcher(rs.getString(3)).find()) {
                    tables.add(rs.getString(3));
                }
            }
        }
        Collections.sort(tables);
        for (String t : tables) {
            out.addAll(table(m, catalog, schema, t));
        }
        List<String> views = new ArrayList<>();
        try (ResultSet rs = m.getTables(catalog, schema, "%", new String[]{"VIEW"})) {
            while (rs.next()) {
                views.add("VIEW " + rs.getString(3));
            }
        }
        Collections.sort(views);
        out.addAll(views);

        List<String[]> probes = new ArrayList<>();
        try (Statement s = conn.createStatement()) {
            for (String block : queries.split("\n--;;\n")) {
                block = block.strip();
                if (block.isEmpty()) {
                    continue;
                }
                int eol = block.indexOf('\n');
                String label = block.substring(3, eol).strip();
                String sql = block.substring(eol + 1).strip();
                if (label.startsWith("probe")) {
                    probes.add(new String[]{label, sql});
                    continue;
                }
                if (label.startsWith("exec")) {
                    s.execute(sql);
                    continue;
                }
                List<String> lines = new ArrayList<>();
                try (ResultSet rs = s.executeQuery(sql)) {
                    while (rs.next()) {
                        lines.add(label + ": " + row(rs));
                    }
                }
                Collections.sort(lines);
                out.addAll(lines);
            }
            boolean postgres = m.getDatabaseProductName().startsWith("PostgreSQL");
            for (String t : tables) {
                String qualified = (catalog != null && !postgres ? q + catalog + q + "." : "")
                        + (schema != null ? q + schema + q + "." : "") + q + t + q;
                List<String> rows = new ArrayList<>();
                try (ResultSet rs = s.executeQuery("SELECT * FROM " + qualified)) {
                    while (rs.next()) {
                        rows.add("ROW " + t + ": " + row(rs));
                    }
                }
                Collections.sort(rows);
                out.add("DATA " + t + " count=" + rows.size());
                out.addAll(rows);
            }
        }
        for (String[] p : probes) {
            out.addAll(probe(conn, p[0], p[1]));
        }
        return out;
    }

    private static List<String> table(DatabaseMetaData m, String catalog, String schema, String t)
            throws SQLException {
        List<String> lines = new ArrayList<>();
        try (ResultSet rs = m.getColumns(catalog, schema, t, "%")) {
            int n = rs.getMetaData().getColumnCount();
            while (rs.next()) {
                // In column order: ojdbc streams COLUMN_DEF (a LONG) and closes
                // the stream once a later column is read.
                String[] v = new String[Math.max(n, 24) + 1];
                for (int k = 1; k <= n; k++) {
                    try {
                        v[k] = rs.getString(k);
                    } catch (SQLException e) {
                        v[k] = "?";
                    }
                }
                lines.add(String.format("COL %s.%s #%s %s(%s,%s) null=%s def=%s auto=%s gen=%s"
                                + " rem=%s", t, v[4], v[17], v[6], v[7], v[9], v[18],
                        v[13] == null ? null : v[13].strip(), v[23], v[24], v[12]));
            }
        }
        Map<String, TreeMap<Integer, String>> pk = new TreeMap<>();
        try (ResultSet rs = m.getPrimaryKeys(catalog, schema, t)) {
            while (rs.next()) {
                pk.computeIfAbsent(String.valueOf(rs.getString("PK_NAME")), k -> new TreeMap<>())
                        .put(rs.getInt("KEY_SEQ"), rs.getString("COLUMN_NAME"));
            }
        }
        pk.forEach((k, v) -> lines.add("PK " + t + " " + k + " " + v.values()));
        Map<String, TreeMap<Integer, String>> fk = new TreeMap<>();
        try (ResultSet rs = m.getImportedKeys(catalog, schema, t)) {
            while (rs.next()) {
                String k = rs.getString("FK_NAME") + " -> " + rs.getString("PKTABLE_NAME")
                        + " upd=" + rs.getInt("UPDATE_RULE") + " del=" + rs.getInt("DELETE_RULE")
                        + " defer=" + rs.getInt("DEFERRABILITY");
                fk.computeIfAbsent(k, x -> new TreeMap<>()).put(rs.getInt("KEY_SEQ"),
                        rs.getString("FKCOLUMN_NAME") + "=" + rs.getString("PKCOLUMN_NAME"));
            }
        }
        fk.forEach((k, v) -> lines.add("FK " + t + " " + k + " " + v.values()));
        Map<String, TreeMap<Integer, String>> ix = new TreeMap<>();
        // approximate: ojdbc analyzes the table for an exact answer, and fails
        // to on a quoted name.
        try (ResultSet rs = m.getIndexInfo(catalog, schema, t, false, true)) {
            while (rs.next()) {
                if (rs.getShort("TYPE") == DatabaseMetaData.tableIndexStatistic) {
                    continue;
                }
                String k = rs.getString("INDEX_NAME") + " unique=" + !rs.getBoolean("NON_UNIQUE")
                        + " filter=" + rs.getString("FILTER_CONDITION");
                ix.computeIfAbsent(k, x -> new TreeMap<>()).put(rs.getInt("ORDINAL_POSITION"),
                        rs.getString("COLUMN_NAME") + " " + rs.getString("ASC_OR_DESC"));
            }
        }
        ix.forEach((k, v) -> lines.add("IDX " + t + " " + k + " " + v.values()));
        Collections.sort(lines);
        lines.add(0, "TABLE " + t);
        return lines;
    }

    private static List<String> probe(Connection conn, String label, String sql)
            throws SQLException {
        List<String[]> pairs = new ArrayList<>();
        try (Statement s = conn.createStatement(); ResultSet rs = s.executeQuery(sql)) {
            while (rs.next()) {
                pairs.add(new String[]{rs.getString(1), rs.getString(2)});
            }
        }
        pairs.sort(Comparator.comparing(x -> x[0]));
        boolean rollback = label.startsWith("probe-rollback");
        List<String> out = new ArrayList<>();
        for (String[] kv : pairs) {
            String v;
            if (rollback) {
                conn.setAutoCommit(false);
            }
            try (Statement s = conn.createStatement(); ResultSet rs = s.executeQuery(kv[1])) {
                v = rs.next() ? rs.getString(1) : "(no row)";
            } finally {
                if (rollback) {
                    conn.rollback();
                    conn.setAutoCommit(true);
                }
            }
            out.add(label + " " + kv[0] + " = " + v);
        }
        return out;
    }

    private static String row(ResultSet rs) throws SQLException {
        ResultSetMetaData md = rs.getMetaData();
        StringBuilder sb = new StringBuilder();
        for (int i = 1; i <= md.getColumnCount(); i++) {
            if (i > 1) {
                sb.append(" | ");
            }
            switch (md.getColumnType(i)) {
                case Types.BINARY:
                case Types.VARBINARY:
                case Types.LONGVARBINARY:
                case Types.BLOB: {
                    byte[] b = rs.getBytes(i);
                    if (b == null) {
                        sb.append("NULL");
                    } else {
                        sb.append("0x");
                        for (byte x : b) {
                            sb.append(String.format("%02x", x));
                        }
                    }
                    break;
                }
                default: {
                    String s = rs.getString(i);
                    sb.append(s == null ? "NULL" : "'" + s.replace("\\", "\\\\")
                            .replace("\n", "\\n").replace("\r", "\\r").replace("\t", "\\t") + "'");
                }
            }
        }
        return sb.toString();
    }

    /** The text of a classpath resource. */
    public static String resource(String path) {
        try (var in = SchemaSnapshot.class.getResourceAsStream(path)) {
            if (in == null) {
                throw new IllegalArgumentException("no resource " + path);
            }
            return new String(in.readAllBytes(), StandardCharsets.UTF_8);
        } catch (java.io.IOException e) {
            throw new IllegalStateException(e);
        }
    }
}
