package comart.rudbman.bridge.meta;

import com.google.gson.JsonObject;

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
}
