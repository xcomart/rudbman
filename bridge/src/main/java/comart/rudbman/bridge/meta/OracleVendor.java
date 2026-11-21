package comart.rudbman.bridge.meta;

import com.google.gson.JsonObject;

import java.util.logging.Logger;
import java.util.logging.Level;
import java.util.Set;
import java.util.HashSet;
import java.sql.Statement;
import java.sql.PreparedStatement;
import java.math.BigDecimal;
import java.util.Locale;
import java.time.format.DateTimeFormatter;
import java.time.ZonedDateTime;
import java.time.ZoneOffset;
import java.time.LocalDateTime;
import java.sql.Types;
import java.sql.ResultSet;
import java.sql.Connection;
import java.sql.SQLException;
import java.util.ArrayList;
import java.util.List;
import java.util.Map;
import java.util.regex.Pattern;

/** Oracle Database. */
final class OracleVendor extends Vendor {

    /**
     * What Oracle's driver writes into {@code REMARKS} for every routine -
     * "Standalone procedure or function", "Packaged function" - which says
     * nothing about the routine and is not a comment anyone wrote.
     */
    private static final Pattern ROUTINE_REMARKS = Routines.ORACLE_REMARKS;

    @Override
    boolean names(String productName) {
        return productName.contains("oracle");
    }

    @Override
    String routineRemarks(String remarks) {
        if (remarks != null && ROUTINE_REMARKS.matcher(remarks.trim()).matches()) {
            return null;
        }
        return remarks;
    }

    /** The source as listed first and, failing that, as the other kind. */
    @Override
    String routineText(Connection conn, Ident id, String catalog, String schema, String name,
                       boolean function) {
        String text = DdlText.oracleSource(conn, schema, name,
                function ? "FUNCTION" : "PROCEDURE");
        return text != null ? text : DdlText.oracleSource(conn, schema, name,
                function ? "PROCEDURE" : "FUNCTION");
    }

    @Override
    List<String> sequenceQueries(Ident id, String catalog) {
        return List.of(Sequences.ORACLE_OWN_SQL, Sequences.ORACLE_SQL);
    }

    @Override
    void triggerQuery(StringBuilder sql, List<String> args, Ident id, String catalog,
                      String schema, String name) {
        sql.append(Triggers.ORACLE_SQL).append(DdlText.owner(schema));
        if (schema != null) {
            args.add(schema);
        }
        Triggers.filter(sql, args, "TRIGGER_NAME", name);
        sql.append(" ORDER BY 3");
    }

    /** {@code TRIGGER_TYPE} holds timing and level together. */
    @Override
    void triggerRow(String[] r) {
        Triggers.oracleType(r);
    }

    @Override
    boolean hidesComments() {
        return true;
    }

    @Override
    Map<String, String> tableComments(Connection conn, JsonObject req) throws SQLException {
        Comments.Filter schema = Comments.Filter.of(req, "schema", "schema_pattern");
        Comments.Filter table = Comments.Filter.of(req, "table", "table_pattern");
        StringBuilder sql = new StringBuilder(Comments.ORACLE_TABLES);
        List<String> args = new ArrayList<>();
        Comments.oracleOwner(sql, args, schema);
        table.append(sql, args, "TABLE_NAME", Comments.ANSI_ESCAPE);
        return Comments.read(conn, sql.toString(), args, 2);
    }

    @Override
    Map<String, String> columnComments(Connection conn, JsonObject req) throws SQLException {
        Comments.Filter schema = Comments.Filter.of(req, "schema", "schema_pattern");
        Comments.Filter table = Comments.Filter.of(req, "table", "table_pattern");
        Comments.Filter column = Comments.Filter.of(req, "column", "column_pattern");
        StringBuilder sql = new StringBuilder(Comments.ORACLE_COLUMNS);
        List<String> args = new ArrayList<>();
        Comments.oracleOwner(sql, args, schema);
        table.append(sql, args, "TABLE_NAME", Comments.ANSI_ESCAPE);
        column.append(sql, args, "COLUMN_NAME", Comments.ANSI_ESCAPE);
        return Comments.read(conn, sql.toString(), args, 3);
    }

    @Override
    boolean upserts() {
        return true;
    }

    /** {@code MERGE} from a one-row {@code SELECT ... FROM dual}, with no {@code AS}. */
    @Override
    String upsert(Ident id, String qualified, List<String> columns, List<String> keys,
                  List<String> updates) {
        return Upsert.merge(id, qualified, columns, keys, updates, true, false);
    }

    @Override
    String tableText(Connection conn, Ident id, String catalog, String schema, String table) {
        return Ddl.oracle(conn, schema, table, true);
    }

    @Override
    Ddl.Script nativeSplit(Connection conn, Ident id, String catalog, String schema,
                           String table) {
        return Ddl.oracleSplit(conn, schema, table);
    }

    @Override
    boolean indexesForeignKeys() {
        return false;
    }

    /**
     * {@code DBMS_METADATA}'s text of the view or, failing that, of the
     * materialized view and the indexes of the table its rows sit in, then
     * the comments.
     */
    @Override
    String viewText(Connection conn, ObjectDdl.Target t) {
        String text = DdlText.oracleMetadata(conn, "VIEW", t.schema, t.name);
        if (text == null) {
            text = DdlText.oracleMetadata(conn, "MATERIALIZED_VIEW", t.schema, t.name);
            if (text != null) {
                // A materialized view's rows sit in a table of its name,
                // and that table's indexes are the view's.
                List<String> parts = new ArrayList<>();
                parts.add(text);
                parts.addAll(Ddl.oracleIndexes(conn, t.schema, t.name));
                text = DdlText.joined(parts);
            }
        }
        if (text == null) {
            return null;
        }
        String comments = DdlText.oracleDependent(conn, "COMMENT", t.schema, t.name);
        return comments == null ? text : text + "\n\n" + comments;
    }

    @Override
    String sequenceText(Connection conn, ObjectDdl.Target t) {
        return DdlText.oracleMetadata(conn, "SEQUENCE", t.schema, t.name);
    }

    @Override
    String triggerText(Connection conn, ObjectDdl.Target t) {
        return DdlText.oracleSource(conn, t.schema, t.name, "TRIGGER");
    }

    /** {@code CREATE SEQUENCE} takes no {@code AS} type. */
    @Override
    boolean typesSequences() {
        return false;
    }

    @Override
    public String literal(ResultSet rs, int i, int type) throws SQLException {
        return temporal(rs, i, type);
    }

    @Override
    public boolean booleansAreNumbers() {
        return true;
    }

    /** {@code N'…'} for the national types, whose character set may differ. */
    @Override
    public String stringLiteral(String s, boolean national, int type, ResultSet rs, int i) {
        return national ? "N" + Ident.literal(s) : Ident.literal(s);
    }

    @Override
    public String hexLiteral(String hex) {
        return "HEXTORAW('" + hex + "')";
    }

    @Override
    public ScriptClient scriptClient() {
        return ScriptClient.SQLPLUS;
    }

    /** Oracle's own way past the foreign keys that point at a table. */
    @Override
    public String dropTableCascade() {
        return " CASCADE CONSTRAINTS";
    }

    @Override
    public String dropForeignKey(String qualified, String key) {
        return null;
    }

    /** No multi-row {@code VALUES} clause. */
    @Override
    public boolean multiRowValues() {
        return false;
    }

    /** ojdbc's own type codes, which {@link Types} has no names for. */
    private static final int ORACLE_TIMESTAMPTZ = -101;
    private static final int ORACLE_TIMESTAMPLTZ = -102;
    private static final int ORACLE_INTERVALYM = -103;
    private static final int ORACLE_INTERVALDS = -104;
    private static final int ORACLE_BINARY_FLOAT = 100;
    private static final int ORACLE_BINARY_DOUBLE = 101;

    private static final DateTimeFormatter ORACLE_DATE =
            DateTimeFormatter.ofPattern("uuuu-MM-dd HH:mm:ss", Locale.ROOT);
    private static final DateTimeFormatter ORACLE_TIMESTAMP =
            DateTimeFormatter.ofPattern("uuuu-MM-dd HH:mm:ss.SSSSSSSSS", Locale.ROOT);

    /**
     * Oracle's dates, times and intervals, and its binary floating-point
     * numbers, spelled so that no session setting changes what they mean: a
     * plain string goes through {@code NLS_DATE_FORMAT},
     * {@code NLS_TIMESTAMP_FORMAT} and {@code NLS_NUMERIC_CHARACTERS}, which
     * differ between one client and the next. {@code null} for every other
     * column, which the general rules write.
     */
    private static String temporal(ResultSet rs, int i, int type) throws SQLException {
        switch (type) {
            case Types.DATE:
            case Types.TIMESTAMP: {
                LocalDateTime v = rs.getObject(i, LocalDateTime.class);
                if (v == null || rs.wasNull()) {
                    return "NULL";
                }
                // ojdbc reports a DATE, which holds a time of day, as a TIMESTAMP.
                if ("DATE".equalsIgnoreCase(rs.getMetaData().getColumnTypeName(i))) {
                    return "TO_DATE('" + ORACLE_DATE.format(v) + "', 'YYYY-MM-DD HH24:MI:SS')";
                }
                return "TIMESTAMP '" + ORACLE_TIMESTAMP.format(v) + "'";
            }
            case Types.TIMESTAMP_WITH_TIMEZONE:
            case ORACLE_TIMESTAMPTZ: {
                ZonedDateTime v = rs.getObject(i, ZonedDateTime.class);
                if (v == null || rs.wasNull()) {
                    return "NULL";
                }
                String zone = v.getZone() instanceof ZoneOffset
                        ? ((ZoneOffset) v.getZone()).getId().replace("Z", "+00:00")
                        : v.getZone().getId();
                return "TIMESTAMP '" + ORACLE_TIMESTAMP.format(v) + " " + zone + "'";
            }
            case ORACLE_TIMESTAMPLTZ: {
                // The session's time zone is the reader's and the writer's
                // both: the instant goes over, written in UTC.
                java.sql.Timestamp v = rs.getTimestamp(i);
                if (v == null || rs.wasNull()) {
                    return "NULL";
                }
                return "TIMESTAMP '" + ORACLE_TIMESTAMP.format(
                        v.toInstant().atOffset(ZoneOffset.UTC)) + " +00:00'";
            }
            case ORACLE_INTERVALYM: {
                String v = rs.getString(i);
                return v == null || rs.wasNull() ? "NULL"
                        : "INTERVAL " + Ident.literal(v.strip()) + " YEAR(9) TO MONTH";
            }
            case ORACLE_INTERVALDS: {
                String v = rs.getString(i);
                return v == null || rs.wasNull() ? "NULL"
                        : "INTERVAL " + Ident.literal(v.strip()) + " DAY(9) TO SECOND(9)";
            }
            case ORACLE_BINARY_FLOAT:
            case ORACLE_BINARY_DOUBLE: {
                double d = rs.getDouble(i);
                if (rs.wasNull()) {
                    return "NULL";
                }
                if (Double.isNaN(d)) {
                    return type == ORACLE_BINARY_FLOAT ? "BINARY_FLOAT_NAN" : "BINARY_DOUBLE_NAN";
                }
                if (Double.isInfinite(d)) {
                    String inf = type == ORACLE_BINARY_FLOAT ? "BINARY_FLOAT_INFINITY"
                            : "BINARY_DOUBLE_INFINITY";
                    return d < 0 ? "-" + inf : inf;
                }
                // The f/d suffix keeps a float's value from rounding through NUMBER.
                return (type == ORACLE_BINARY_FLOAT ? Float.toString((float) d)
                        : Double.toString(d)) + (type == ORACLE_BINARY_FLOAT ? "f" : "d");
            }
            default:
                return null;
        }
    }

    /**
     * ojdbc lists a materialized view as the table that holds its rows; the
     * view comes back with its own DDL and fills itself.
     */
    @Override
    public Set<String> materializedViews(Connection conn, String schema) {
        return ownerNames(conn, schema, "SELECT MVIEW_NAME FROM ALL_MVIEWS WHERE OWNER = %s");
    }

    /** A materialized view log is listed as a table, and comes back with its view. */
    @Override
    public Set<String> tablesNotWritten(Connection conn, String schema) {
        return ownerNames(conn, schema,
                "SELECT LOG_TABLE FROM ALL_MVIEW_LOGS WHERE LOG_OWNER = %s");
    }

    /** The driver lists a package's members as routines whose catalog is the package. */
    @Override
    public boolean packagesRoutines() {
        return true;
    }

    @Override
    public List<String> packages(Connection conn, String schema) {
        List<String> out = new ArrayList<>();
        boolean scoped = schema != null && !schema.isEmpty();
        String sql = "SELECT OBJECT_NAME FROM ALL_OBJECTS WHERE OBJECT_TYPE = 'PACKAGE' AND OWNER = "
                + (scoped ? "?" : "SYS_CONTEXT('USERENV', 'CURRENT_SCHEMA')")
                + " ORDER BY OBJECT_NAME";
        try (PreparedStatement ps = conn.prepareStatement(sql)) {
            if (scoped) {
                ps.setString(1, schema);
            }
            try (ResultSet rs = ps.executeQuery()) {
                while (rs.next()) {
                    out.add(rs.getString(1));
                }
            }
        } catch (SQLException e) {
            LOG.log(Level.FINE, "no package listing", e);
        }
        return out;
    }

    /**
     * The script is SQL*Plus's already - its PL/SQL ends in slashes - and
     * SQL*Plus would read an ampersand in a row as a variable to prompt for,
     * and end a statement at its first empty line.
     */
    @Override
    public List<String> backupPrologue(Ident id, String catalog, String schema,
                                       boolean routines) {
        return List.of("SET DEFINE OFF", "SET SQLBLANKLINES ON");
    }

    /**
     * An identity switched for the rows and back, like any DDL on a table,
     * leaves the views and code over it invalid until their next use; compiled
     * here they are as valid as in the source.
     */
    @Override
    public List<String> backupEpilogue(Ident id, String catalog, String schema,
                                       boolean routines, boolean code) {
        if (!code) {
            return List.of();
        }
        return List.of("BEGIN DBMS_UTILITY.COMPILE_SCHEMA(" + (schema == null
                || schema.isEmpty() ? "SYS_CONTEXT('USERENV', 'CURRENT_SCHEMA')"
                : Ident.literal(schema)) + ", FALSE); END;", "/");
    }

    /**
     * Asked of every table rather than of the ones whose columns say
     * auto-increment: not every ojdbc release says so of an identity. An
     * {@code ALWAYS} identity refuses a value even from its owner; it is
     * relaxed for the rows and put back after them, every identity starting
     * where the source's continues.
     */
    @Override
    public RowFence rowFence(Connection conn, Ident id, String schema, String table,
                             String qualified, List<String> autoIncrement) {
        List<String[]> identities = identities(conn, schema, table);
        return new RowFence() {
            @Override
            public List<String> before() {
                List<String> out = new ArrayList<>();
                for (String[] ident : identities) {
                    if ("ALWAYS".equals(ident[1])) {
                        out.add("ALTER TABLE " + qualified + " MODIFY (" + id.q(ident[0])
                                + " GENERATED BY DEFAULT AS IDENTITY);");
                    }
                }
                return out;
            }

            @Override
            public List<String> after() {
                List<String> out = new ArrayList<>();
                for (String[] ident : identities) {
                    out.add("ALTER TABLE " + qualified + " MODIFY (" + id.q(ident[0])
                            + " GENERATED " + ("ALWAYS".equals(ident[1]) ? "ALWAYS"
                            : "YES".equals(ident[2]) ? "BY DEFAULT ON NULL" : "BY DEFAULT")
                            + " AS IDENTITY (START WITH "
                            + identityStart(conn, qualified, ident, id) + "));");
                }
                return out;
            }
        };
    }

    /**
     * An Oracle table's identity columns: name, {@code GENERATION_TYPE} and
     * whether it is {@code ON NULL}. Before 12c there are none, and the query
     * fails into an empty answer.
     */
    private static List<String[]> identities(Connection conn, String schema,
                                             String table) {
        List<String[]> out = new ArrayList<>();
        boolean scoped = schema != null && !schema.isEmpty();
        String owner = scoped ? "?" : "SYS_CONTEXT('USERENV', 'CURRENT_SCHEMA')";
        String sql = "SELECT i.COLUMN_NAME, i.GENERATION_TYPE, c.DEFAULT_ON_NULL,"
                + " TO_CHAR(s.LAST_NUMBER), TO_CHAR(s.INCREMENT_BY)"
                + " FROM ALL_TAB_IDENTITY_COLS i JOIN ALL_TAB_COLUMNS c"
                + " ON c.OWNER = i.OWNER AND c.TABLE_NAME = i.TABLE_NAME"
                + " AND c.COLUMN_NAME = i.COLUMN_NAME"
                + " LEFT JOIN ALL_SEQUENCES s ON s.SEQUENCE_OWNER = i.OWNER"
                + " AND s.SEQUENCE_NAME = i.SEQUENCE_NAME"
                + " WHERE i.OWNER = " + owner + " AND i.TABLE_NAME = ?";
        try (PreparedStatement ps = conn.prepareStatement(sql)) {
            int i = 1;
            if (scoped) {
                ps.setString(i++, schema);
            }
            ps.setString(i, table);
            try (ResultSet rs = ps.executeQuery()) {
                while (rs.next()) {
                    out.add(new String[]{rs.getString(1), rs.getString(2), rs.getString(3),
                            rs.getString(4), rs.getString(5)});
                }
            }
        } catch (SQLException e) {
            LOG.log(Level.FINE, "no identity metadata for " + table, e);
        }
        return out;
    }

    /**
     * Where a restored Oracle identity has to continue: past every value its
     * source has handed out, which Oracle knows only up to its cache -
     * {@code LAST_NUMBER}, where a sequence of the source's resumes after a
     * restart too, as Data Pump carries a sequence - and past every value in
     * the column, explicit ones included. {@code LIMIT VALUE}, the second
     * alone, would hand out a deleted row's key again.
     */
    private static String identityStart(Connection conn, String qualified, String[] ident,
                                        Ident id) {
        if (ident[3] == null || ident[4] == null) {
            return "LIMIT VALUE";
        }
        try (Statement st = conn.createStatement();
             ResultSet rs = st.executeQuery("SELECT MAX(" + id.q(ident[0]) + ") FROM "
                     + qualified)) {
            BigDecimal last = new BigDecimal(ident[3]);
            BigDecimal step = new BigDecimal(ident[4]);
            BigDecimal max = rs.next() ? rs.getBigDecimal(1) : null;
            if (step.signum() == 0) {
                return "LIMIT VALUE";
            }
            if (max == null) {
                return last.toPlainString();
            }
            BigDecimal past = max.add(step);
            return (step.signum() > 0 ? last.max(past) : last.min(past)).toPlainString();
        } catch (SQLException | NumberFormatException e) {
            LOG.log(Level.FINE, "no identity state for " + qualified, e);
            return "LIMIT VALUE";
        }
    }

    /**
     * The first column of an Oracle catalog query over the scope's owner, its
     * {@code %s}; empty when the query is refused.
     */
    private static Set<String> ownerNames(Connection conn, String schema, String sqlWithOwner) {
        Set<String> out = new HashSet<>();
        boolean scoped = schema != null && !schema.isEmpty();
        String sql = String.format(Locale.ROOT, sqlWithOwner,
                scoped ? "?" : "SYS_CONTEXT('USERENV', 'CURRENT_SCHEMA')");
        try (PreparedStatement ps = conn.prepareStatement(sql)) {
            if (scoped) {
                ps.setString(1, schema);
            }
            try (ResultSet rs = ps.executeQuery()) {
                while (rs.next()) {
                    out.add(rs.getString(1));
                }
            }
        } catch (SQLException e) {
            LOG.log(Level.FINE, "no answer to " + sqlWithOwner, e);
        }
        return out;
    }

    private static final Logger LOG = Logger.getLogger(OracleVendor.class.getName());
}
