package comart.rudbman.bridge.meta;

import java.math.BigDecimal;
import java.sql.Connection;
import java.sql.PreparedStatement;
import java.sql.ResultSet;
import java.sql.Statement;
import java.util.ArrayList;
import java.util.HashSet;
import java.util.LinkedHashMap;
import java.util.List;
import java.util.Map;
import java.util.Set;

/**
 * What Db2's {@code SYSCAT} says about a table that JDBC metadata cannot, read
 * the way db2look writes a table: the column types as declared, a computed
 * column's expression, an identity's attributes, the {@code CHECK} and
 * {@code UNIQUE} constraints, and the indexes - over expressions, with
 * {@code INCLUDE} columns, clustering - each in its own schema.
 *
 * <p>The JCC driver reports a {@code VARGRAPHIC}'s size in bytes, so the type
 * it gives back grows with every restore, and has no place for any of the
 * rest. Every query is fenced by {@link Attempt}, a refused one leaving the
 * metadata's answer.
 */
public final class Db2Schema {

    private Db2Schema() {
    }

    private static String trim(String s) {
        return s == null ? null : s.trim();
    }

    /** Each non-computed column's type as it is declared. */
    static Map<String, String> columnTypes(Connection conn, Ident id, String schema,
                                           String table) {
        return Attempt.<Map<String, String>>on(conn, "SYSCAT.COLUMNS", c -> {
            Map<String, String> out = new LinkedHashMap<>();
            try (PreparedStatement ps = c.prepareStatement("SELECT COLNAME, TYPESCHEMA, TYPENAME,"
                    + " LENGTH, SCALE, CODEPAGE FROM SYSCAT.COLUMNS"
                    + " WHERE TABSCHEMA = ? AND TABNAME = ?")) {
                ps.setString(1, schema);
                ps.setString(2, table);
                try (ResultSet rs = ps.executeQuery()) {
                    while (rs.next()) {
                        out.put(rs.getString(1), type(id, trim(rs.getString(2)),
                                trim(rs.getString(3)), rs.getLong(4), rs.getInt(5), rs.getInt(6)));
                    }
                }
            }
            return out;
        }, new LinkedHashMap<>());
    }

    /** A type with the parameters it is declared with, as db2look writes it. */
    static String type(Ident id, String typeSchema, String type, long length, int scale,
                       int codepage) {
        if (typeSchema != null && !"SYSIBM".equals(typeSchema)) {
            return id.qualify(null, typeSchema, type);
        }
        switch (type) {
            case "CHARACTER":
                return "CHAR(" + length + ")" + (codepage == 0 ? " FOR BIT DATA" : "");
            case "VARCHAR":
                return "VARCHAR(" + length + ")" + (codepage == 0 ? " FOR BIT DATA" : "");
            case "GRAPHIC":
            case "VARGRAPHIC":
            case "CLOB":
            case "BLOB":
            case "DBCLOB":
            case "BINARY":
            case "VARBINARY":
                return type + "(" + length + ")";
            case "DECIMAL":
                return "DECIMAL(" + length + ", " + scale + ")";
            case "DECFLOAT":
                return length == 8 ? "DECFLOAT(16)" : "DECFLOAT(34)";
            case "TIMESTAMP":
                return "TIMESTAMP(" + scale + ")";
            default:
                return type;
        }
    }

    /**
     * Each computed column's definition after its type -
     * {@code GENERATED ALWAYS AS (expr)} - and each identity column's, with
     * its attributes. The map holds the computed columns, {@code identities}
     * the identity ones.
     */
    static Map<String, String> generated(Connection conn, String schema, String table,
                                         Map<String, String> identities) {
        return Attempt.<Map<String, String>>on(conn, "SYSCAT.COLUMNS generated", c -> {
            Map<String, String> out = new LinkedHashMap<>();
            try (PreparedStatement ps = c.prepareStatement("SELECT c.COLNAME, c.GENERATED,"
                    + " c.IDENTITY, c.TEXT, VARCHAR(i.START), VARCHAR(i.INCREMENT),"
                    + " VARCHAR(i.MINVALUE), VARCHAR(i.MAXVALUE), i.CYCLE, i.CACHE, i.ORDER"
                    + " FROM SYSCAT.COLUMNS c LEFT JOIN SYSCAT.COLIDENTATTRIBUTES i"
                    + " ON i.TABSCHEMA = c.TABSCHEMA AND i.TABNAME = c.TABNAME"
                    + " AND i.COLNAME = c.COLNAME"
                    + " WHERE c.TABSCHEMA = ? AND c.TABNAME = ? AND c.GENERATED <> ' '")) {
                ps.setString(1, schema);
                ps.setString(2, table);
                try (ResultSet rs = ps.executeQuery()) {
                    while (rs.next()) {
                        String name = rs.getString(1);
                        String kind = "A".equals(trim(rs.getString(2))) ? "ALWAYS" : "BY DEFAULT";
                        if ("Y".equals(rs.getString(3))) {
                            int cache = rs.getInt(10);
                            identities.put(name, "GENERATED " + kind + " AS IDENTITY (START WITH "
                                    + number(rs.getString(5)) + " INCREMENT BY "
                                    + number(rs.getString(6)) + " MINVALUE "
                                    + number(rs.getString(7)) + " MAXVALUE "
                                    + number(rs.getString(8))
                                    + ("Y".equals(rs.getString(9)) ? " CYCLE" : " NO CYCLE")
                                    + (cache < 2 ? " NO CACHE" : " CACHE " + cache)
                                    + ("Y".equals(rs.getString(11)) ? " ORDER" : " NO ORDER")
                                    + ")");
                        } else {
                            String text = trim(rs.getString(4));
                            if (text != null && !text.isEmpty()) {
                                out.put(name, "GENERATED " + kind + " " + text);
                            }
                        }
                    }
                }
            }
            return out;
        }, new LinkedHashMap<>());
    }

    /** Db2 writes a DECIMAL(31) attribute as {@code 10.}; the dot goes. */
    private static String number(String s) {
        String t = trim(s);
        return t != null && t.endsWith(".") ? t.substring(0, t.length() - 1) : t;
    }

    /** The table's {@code CHECK} and {@code UNIQUE} constraints, as lines of its body. */
    static List<String> constraints(Connection conn, Ident id, String schema, String table) {
        return Attempt.<List<String>>on(conn, "SYSCAT.TABCONST", c -> {
            List<String> out = new ArrayList<>();
            try (PreparedStatement ps = c.prepareStatement("SELECT k.CONSTNAME,"
                    + " LISTAGG(CAST(u.COLNAME AS VARCHAR(128)), X'1F') WITHIN GROUP (ORDER BY u.COLSEQ)"
                    + " FROM SYSCAT.TABCONST k JOIN SYSCAT.KEYCOLUSE u"
                    + " ON u.TABSCHEMA = k.TABSCHEMA AND u.TABNAME = k.TABNAME"
                    + " AND u.CONSTNAME = k.CONSTNAME"
                    + " WHERE k.TABSCHEMA = ? AND k.TABNAME = ? AND k.TYPE = 'U'"
                    + " GROUP BY k.CONSTNAME ORDER BY k.CONSTNAME")) {
                ps.setString(1, schema);
                ps.setString(2, table);
                try (ResultSet rs = ps.executeQuery()) {
                    while (rs.next()) {
                        StringBuilder cols = new StringBuilder();
                        for (String col : rs.getString(2).split("\u001f")) {
                            if (cols.length() > 0) {
                                cols.append(", ");
                            }
                            cols.append(id.q(col));
                        }
                        out.add("CONSTRAINT " + id.q(rs.getString(1)) + " UNIQUE (" + cols + ")");
                    }
                }
            }
            // TYPE 'C' alone: a computed column carries a check of its own
            // that the column definition brings back.
            try (PreparedStatement ps = c.prepareStatement("SELECT CONSTNAME, TEXT"
                    + " FROM SYSCAT.CHECKS WHERE TABSCHEMA = ? AND TABNAME = ? AND TYPE = 'C'"
                    + " ORDER BY CONSTNAME")) {
                ps.setString(1, schema);
                ps.setString(2, table);
                try (ResultSet rs = ps.executeQuery()) {
                    while (rs.next()) {
                        out.add("CONSTRAINT " + id.q(rs.getString(1)) + " CHECK ("
                                + trim(rs.getString(2)) + ")");
                    }
                }
            }
            return out;
        }, new ArrayList<>());
    }

    /**
     * The table's indexes that no key or {@code UNIQUE} constraint requires,
     * each named in its own schema, with expression keys, {@code INCLUDE}
     * columns and clustering. XML and other special indexes are not written.
     *
     * @return the statements, unterminated, or {@code null} when the catalog
     *         could not be read
     */
    static List<String> indexes(Connection conn, Ident id, String schema, String table,
                                String qualified) {
        return Attempt.<List<String>>on(conn, "SYSCAT.INDEXES", c -> {
            List<String[]> found = new ArrayList<>();
            try (PreparedStatement ps = c.prepareStatement("SELECT INDSCHEMA, INDNAME, UNIQUERULE,"
                    + " INDEXTYPE, REVERSE_SCANS FROM SYSCAT.INDEXES"
                    + " WHERE TABSCHEMA = ? AND TABNAME = ? AND SYSTEM_REQUIRED = 0"
                    + " AND INDEXTYPE IN ('REG', 'CLUS') AND UNIQUERULE <> 'P'"
                    + " ORDER BY INDNAME")) {
                ps.setString(1, schema);
                ps.setString(2, table);
                try (ResultSet rs = ps.executeQuery()) {
                    while (rs.next()) {
                        found.add(new String[]{trim(rs.getString(1)), rs.getString(2),
                                rs.getString(3), rs.getString(4), rs.getString(5)});
                    }
                }
            }
            List<String> out = new ArrayList<>();
            for (String[] f : found) {
                StringBuilder keys = new StringBuilder();
                StringBuilder included = new StringBuilder();
                try (PreparedStatement ps = c.prepareStatement("SELECT COLNAME, COLORDER, TEXT"
                        + " FROM SYSCAT.INDEXCOLUSE WHERE INDSCHEMA = ? AND INDNAME = ?"
                        + " ORDER BY COLSEQ")) {
                    ps.setString(1, f[0]);
                    ps.setString(2, f[1]);
                    try (ResultSet rs = ps.executeQuery()) {
                        while (rs.next()) {
                            String order = trim(rs.getString(2));
                            String text = trim(rs.getString(3));
                            StringBuilder sb = "I".equals(order) ? included : keys;
                            if (sb.length() > 0) {
                                sb.append(", ");
                            }
                            sb.append(text != null && !text.isEmpty() ? text : id.q(rs.getString(1)));
                            if ("D".equals(order)) {
                                sb.append(" DESC");
                            }
                        }
                    }
                }
                StringBuilder sb = new StringBuilder("CREATE ");
                if ("U".equals(f[2])) {
                    sb.append("UNIQUE ");
                }
                sb.append("INDEX ").append(id.qualify(null, f[0], f[1])).append(" ON ")
                        .append(qualified).append(" (").append(keys).append(')');
                if (included.length() > 0) {
                    sb.append(" INCLUDE (").append(included).append(')');
                }
                if ("CLUS".equals(f[3])) {
                    sb.append(" CLUSTER");
                }
                if ("N".equals(trim(f[4]))) {
                    sb.append(" DISALLOW REVERSE SCANS");
                }
                out.add(sb.toString());
            }
            return out;
        }, null);
    }

    /**
     * The statistical views Db2 makes for its expression-based indexes, which
     * come back with the index and are not views to write.
     *
     * @param conn   the live connection
     * @param schema the schema, or {@code null} for every one
     * @return their names
     */
    public static Set<String> indexViews(Connection conn, String schema) {
        return Attempt.<Set<String>>on(conn, "SYSCAT.INDEXES VIEWNAME", c -> {
            Set<String> out = new HashSet<>();
            try (PreparedStatement ps = c.prepareStatement("SELECT VIEWNAME FROM SYSCAT.INDEXES"
                    + " WHERE VIEWNAME IS NOT NULL"
                    + (schema == null || schema.isEmpty() ? "" : " AND VIEWSCHEMA = ?"))) {
                if (schema != null && !schema.isEmpty()) {
                    ps.setString(1, schema);
                }
                try (ResultSet rs = ps.executeQuery()) {
                    while (rs.next()) {
                        out.add(rs.getString(1));
                    }
                }
            }
            return out;
        }, new HashSet<>());
    }

    /**
     * The table's {@code GENERATED ALWAYS} identity columns, which refuse a
     * value until switched to {@code BY DEFAULT} for the rows.
     */
    public static List<String> alwaysIdentities(Connection conn, String schema, String table) {
        return Attempt.<List<String>>on(conn, "SYSCAT.COLUMNS identity", c -> {
            List<String> out = new ArrayList<>();
            try (PreparedStatement ps = c.prepareStatement("SELECT COLNAME FROM SYSCAT.COLUMNS"
                    + " WHERE TABSCHEMA = ? AND TABNAME = ? AND IDENTITY = 'Y'"
                    + " AND GENERATED = 'A'")) {
                ps.setString(1, schema);
                ps.setString(2, table);
                try (ResultSet rs = ps.executeQuery()) {
                    while (rs.next()) {
                        out.add(rs.getString(1));
                    }
                }
            }
            return out;
        }, new ArrayList<>());
    }

    /**
     * Where a restored table's identity has to continue: past every value the
     * source has handed out, which Db2 knows up to its cache
     * ({@code NEXTCACHEFIRSTVALUE}, where the source resumes after a restart
     * too), and past every value in the column, explicit ones included.
     *
     * <p>It goes into the {@code CREATE} as the identity's {@code START WITH},
     * as {@code DBMS_METADATA} has an Oracle sequence: a {@code RESTART WITH}
     * is not in the catalog until the next value is drawn, and a backup of the
     * restored table before that would read the old position.
     *
     * @param qualified the table's name in the script
     * @return the value, or {@code null} for a table with no identity
     */
    public static String identityNext(Connection conn, Ident id, String schema, String table,
                                      String qualified) {
        return Attempt.<String>on(conn, "SYSCAT.COLIDENTATTRIBUTES", c -> {
            String column;
            BigDecimal next;
            BigDecimal step;
            try (PreparedStatement ps = c.prepareStatement("SELECT COLNAME, NEXTCACHEFIRSTVALUE,"
                    + " INCREMENT FROM SYSCAT.COLIDENTATTRIBUTES"
                    + " WHERE TABSCHEMA = ? AND TABNAME = ?")) {
                ps.setString(1, schema);
                ps.setString(2, table);
                try (ResultSet rs = ps.executeQuery()) {
                    if (!rs.next()) {
                        return null;
                    }
                    column = rs.getString(1);
                    next = rs.getBigDecimal(2);
                    step = rs.getBigDecimal(3);
                }
            }
            BigDecimal max;
            try (Statement st = c.createStatement();
                 ResultSet rs = st.executeQuery("SELECT MAX(" + id.q(column) + ") FROM "
                         + qualified)) {
                max = rs.next() ? rs.getBigDecimal(1) : null;
            }
            BigDecimal restart = next;
            if (max != null && step != null && step.signum() != 0) {
                BigDecimal past = max.add(step);
                restart = restart == null ? past
                        : step.signum() > 0 ? restart.max(past) : restart.min(past);
            }
            return restart == null ? null : restart.stripTrailingZeros().toPlainString();
        }, null);
    }

    /**
     * Where a sequence of the source resumes - the start of its next cache
     * block, exact for one without a cache - or {@code null}.
     */
    public static String sequenceNext(Connection conn, String schema, String name) {
        return Attempt.<String>on(conn, "SYSCAT.SEQUENCES", c -> {
            try (PreparedStatement ps = c.prepareStatement("SELECT NEXTCACHEFIRSTVALUE"
                    + " FROM SYSCAT.SEQUENCES WHERE SEQSCHEMA = ? AND SEQNAME = ?")) {
                ps.setString(1, schema);
                ps.setString(2, name);
                try (ResultSet rs = ps.executeQuery()) {
                    BigDecimal v = rs.next() ? rs.getBigDecimal(1) : null;
                    return v == null ? null : v.stripTrailingZeros().toPlainString();
                }
            }
        }, null);
    }

    /**
     * A {@code DROP} for each of a routine's overloads by its signature, the
     * one way Db2 drops a name with more than one - {@code VARCHAR()} with
     * empty parentheses standing for any length.
     *
     * @param procedure whether the routine is a procedure
     * @return the statements, terminated, or {@code null} when the catalog
     *         could not be read
     */
    public static List<String> routineDrops(Connection conn, Ident id, String schema,
                                            String name, boolean procedure) {
        return Attempt.<List<String>>on(conn, "SYSCAT.ROUTINEPARMS", c -> {
            Map<String, StringBuilder> bySpecific = new LinkedHashMap<>();
            try (PreparedStatement ps = c.prepareStatement("SELECT r.SPECIFICNAME, p.TYPESCHEMA,"
                    + " p.TYPENAME FROM SYSCAT.ROUTINES r LEFT JOIN SYSCAT.ROUTINEPARMS p"
                    + " ON p.ROUTINESCHEMA = r.ROUTINESCHEMA AND p.SPECIFICNAME = r.SPECIFICNAME"
                    + " AND p.ROWTYPE IN ('P', 'B', 'O')"
                    + " WHERE r.ROUTINESCHEMA = ? AND r.ROUTINENAME = ? AND r.ROUTINETYPE = ?"
                    + " ORDER BY r.SPECIFICNAME, p.ORDINAL")) {
                ps.setString(1, schema);
                ps.setString(2, name);
                ps.setString(3, procedure ? "P" : "F");
                try (ResultSet rs = ps.executeQuery()) {
                    while (rs.next()) {
                        StringBuilder sb = bySpecific.computeIfAbsent(rs.getString(1),
                                k -> new StringBuilder());
                        String type = trim(rs.getString(3));
                        if (type == null) {
                            continue;
                        }
                        if (sb.length() > 0) {
                            sb.append(", ");
                        }
                        String typeSchema = trim(rs.getString(2));
                        if (typeSchema != null && !"SYSIBM".equals(typeSchema)) {
                            sb.append(id.qualify(null, typeSchema, type));
                        } else {
                            sb.append(type);
                            switch (type) {
                                case "CHARACTER": case "VARCHAR": case "GRAPHIC":
                                case "VARGRAPHIC": case "CLOB": case "BLOB": case "DBCLOB":
                                case "DECIMAL": case "DECFLOAT": case "BINARY":
                                case "VARBINARY": case "TIMESTAMP":
                                    sb.append("()");
                                    break;
                                default:
                                    break;
                            }
                        }
                    }
                }
            }
            List<String> out = new ArrayList<>();
            for (StringBuilder params : bySpecific.values()) {
                out.add("DROP " + (procedure ? "PROCEDURE " : "FUNCTION ")
                        + id.qualify(null, schema, name) + " (" + params + ");");
            }
            return out;
        }, null);
    }

    /** The table's {@code TIMESTAMP} columns with more than nine digits of a second. */
    public static Set<String> fineTimestamps(Connection conn, String schema, String table) {
        return Attempt.<Set<String>>on(conn, "SYSCAT.COLUMNS timestamp", c -> {
            Set<String> out = new HashSet<>();
            try (PreparedStatement ps = c.prepareStatement("SELECT COLNAME FROM SYSCAT.COLUMNS"
                    + " WHERE TABSCHEMA = ? AND TABNAME = ? AND TYPENAME = 'TIMESTAMP'"
                    + " AND SCALE > 9")) {
                ps.setString(1, schema);
                ps.setString(2, table);
                try (ResultSet rs = ps.executeQuery()) {
                    while (rs.next()) {
                        out.add(rs.getString(1));
                    }
                }
            }
            return out;
        }, new HashSet<>());
    }

    /**
     * A routine's every overload, each its own statement, as it was written.
     *
     * @return the statements, unterminated, or an empty list
     */
    public static List<String> routineTexts(Connection conn, String schema, String name) {
        return Attempt.<List<String>>on(conn, "SYSCAT.ROUTINES", c -> {
            List<String> out = new ArrayList<>();
            try (PreparedStatement ps = c.prepareStatement("SELECT TEXT FROM SYSCAT.ROUTINES"
                    + " WHERE ROUTINESCHEMA = ? AND ROUTINENAME = ? ORDER BY SPECIFICNAME")) {
                ps.setString(1, schema);
                ps.setString(2, name);
                try (ResultSet rs = ps.executeQuery()) {
                    while (rs.next()) {
                        String text = DdlText.stripped(rs.getString(1));
                        if (text != null) {
                            out.add(text);
                        }
                    }
                }
            }
            return out;
        }, new ArrayList<>());
    }
}
