package comart.rudbman.bridge.meta;

import com.google.gson.JsonObject;

import java.util.logging.Logger;
import java.util.logging.Level;
import java.util.Locale;
import java.sql.PreparedStatement;
import java.sql.Types;
import java.sql.ResultSet;
import java.sql.Connection;
import java.sql.SQLException;
import java.util.ArrayList;
import java.util.List;
import java.util.Map;

/**
 * MySQL. {@link MariaDbVendor} extends it and overrides where the fork parts
 * company.
 */
class MySqlVendor extends Vendor {

    @Override
    boolean names(String productName) {
        return productName.contains("mysql");
    }

    @Override
    public boolean catalogIsSchema() {
        return true;
    }

    /**
     * {@code SHOW CREATE}, asked as the kind the routine was listed as first
     * and as the other one after.
     */
    @Override
    String routineText(Connection conn, Ident id, String catalog, String schema, String name,
                       boolean function) {
        String qualified = id.qualify(catalog, schema, name);
        String first = function ? "FUNCTION" : "PROCEDURE";
        String other = function ? "PROCEDURE" : "FUNCTION";
        String text = Attempt.<String>run(conn, "SHOW CREATE " + first,
                st -> RoutineDdl.mysqlShowCreate(st, first, qualified), null);
        return text != null ? text : Attempt.<String>run(conn, "SHOW CREATE " + other,
                st -> RoutineDdl.mysqlShowCreate(st, other, qualified), null);
    }

    @Override
    void triggerQuery(StringBuilder sql, List<String> args, Ident id, String catalog,
                      String schema, String name) {
        sql.append(Triggers.MYSQL_SQL);
        Triggers.filter(sql, args, "TRIGGER_SCHEMA", catalog != null ? catalog : schema);
        Triggers.filter(sql, args, "TRIGGER_NAME", name);
        sql.append(" ORDER BY 1, 3");
    }

    /** Connector/J puts {@code VIEW} in a view's {@code REMARKS}. */
    @Override
    boolean marksViewsInRemarks() {
        return true;
    }

    @Override
    boolean hidesComments() {
        return true;
    }

    @Override
    Map<String, String> tableComments(Connection conn, JsonObject req) throws SQLException {
        Comments.Filter table = Comments.Filter.of(req, "table", "table_pattern");
        StringBuilder sql = new StringBuilder(Comments.MYSQL_TABLES);
        List<String> args = new ArrayList<>();
        Comments.mysqlDatabase(sql, args, req);
        table.append(sql, args, "TABLE_NAME", Comments.NO_ESCAPE);
        return Comments.read(conn, sql.toString(), args, 2);
    }

    @Override
    Map<String, String> columnComments(Connection conn, JsonObject req) throws SQLException {
        Comments.Filter table = Comments.Filter.of(req, "table", "table_pattern");
        Comments.Filter column = Comments.Filter.of(req, "column", "column_pattern");
        StringBuilder sql = new StringBuilder(Comments.MYSQL_COLUMNS);
        List<String> args = new ArrayList<>();
        Comments.mysqlDatabase(sql, args, req);
        table.append(sql, args, "TABLE_NAME", Comments.NO_ESCAPE);
        column.append(sql, args, "COLUMN_NAME", Comments.NO_ESCAPE);
        return Comments.read(conn, sql.toString(), args, 3);
    }

    @Override
    boolean upserts() {
        return true;
    }

    @Override
    String upsert(Ident id, String qualified, List<String> columns, List<String> keys,
                  List<String> updates) {
        return Upsert.onDuplicateKey(id, qualified, columns, keys, updates);
    }

    /**
     * Rows go in under {@code NO_AUTO_VALUE_ON_ZERO}, as mysqldump has them,
     * and the session's own mode comes back after.
     */
    @Override
    List<String> dataPrologue() {
        return List.of("SET @OLD_SQL_MODE = @@SESSION.SQL_MODE;",
                "SET SESSION SQL_MODE = 'NO_AUTO_VALUE_ON_ZERO';");
    }

    @Override
    List<String> dataEpilogue() {
        return List.of("SET SESSION SQL_MODE = @OLD_SQL_MODE;");
    }

    /** Foreign keys stay inline, so the server stops checking references. */
    @Override
    List<String> scriptPrologue() {
        return List.of("SET FOREIGN_KEY_CHECKS = 0;");
    }

    @Override
    List<String> scriptEpilogue() {
        return List.of("SET FOREIGN_KEY_CHECKS = 1;");
    }

    @Override
    String tableText(Connection conn, Ident id, String catalog, String schema, String table) {
        String qualified = id.qualify(catalog, schema, table);
        return Attempt.<String>run(conn, "SHOW CREATE TABLE",
                st -> Ddl.mysqlShowCreate(st, qualified), null);
    }

    @Override
    Ddl.Script nativeSplit(Connection conn, Ident id, String catalog, String schema,
                           String table) {
        return Ddl.whole(tableText(conn, id, catalog, schema, table));
    }

    /** Connector/J reports a generated column's flag wrongly on some servers. */
    @Override
    void confirmGenerated(Connection conn, String catalog, String table, List<Ddl.Col> cols) {
        Ddl.confirmMysqlGenerated(conn, catalog, table, cols);
    }

    @Override
    boolean commentsInline() {
        return true;
    }

    /** MySQL has no {@code COMMENT ON}. */
    @Override
    boolean commentsOn() {
        return false;
    }

    @Override
    String identityClause() {
        return "AUTO_INCREMENT";
    }

    @Override
    boolean notNullFirst() {
        return true;
    }

    @Override
    String viewText(Connection conn, ObjectDdl.Target t) {
        return ObjectDdl.show(conn, "VIEW", t, 2);
    }

    @Override
    String triggerText(Connection conn, ObjectDdl.Target t) {
        return ObjectDdl.show(conn, "TRIGGER", t, 3);
    }

    /** {@code BIT(n)} is n bits: {@code BIT(8)} holding 1010 is ten. */
    @Override
    public boolean bitsAreNumbers() {
        return true;
    }

    @Override
    public boolean booleansAreNumbers() {
        return true;
    }

    /**
     * The drivers hand a {@code YEAR} over as a {@code DATE}, January 1st of
     * it, which a {@code YEAR} column refuses; every other string is
     * {@link #escaped}.
     */
    @Override
    public String stringLiteral(String s, boolean national, int type, ResultSet rs, int i)
            throws SQLException {
        if (type == Types.DATE && s.length() >= 4
                && "YEAR".equalsIgnoreCase(rs.getMetaData().getColumnTypeName(i))) {
            return s.substring(0, 4);
        }
        return escaped(s);
    }

    @Override
    public ScriptClient scriptClient() {
        return ScriptClient.MYSQL;
    }

    @Override
    public boolean dropsIfExists() {
        return true;
    }

    @Override
    public String dropForeignKey(String qualified, String key) {
        return null;
    }

    /**
     * A MySQL or MariaDB string literal: a backslash there escapes what follows
     * it - the rows are read under a mode that keeps it so, see
     * {@code Ddl.dataPrologue} - and a raw NUL is spelled out, as mysqldump
     * writes them.
     */
    private static String escaped(String s) {
        StringBuilder sb = new StringBuilder(s.length() + 2).append('\'');
        for (int k = 0; k < s.length(); k++) {
            char ch = s.charAt(k);
            switch (ch) {
                case '\\': sb.append("\\\\"); break;
                case '\'':  sb.append("''"); break;
                case '\0':  sb.append("\\0"); break;
                default:    sb.append(ch);
            }
        }
        return sb.append('\'').toString();
    }

    /**
     * {@code SHOW CREATE} writes names without their database, the rows are
     * qualified by it: the two have to land in the same place.
     */
    @Override
    public List<String> backupPrologue(Ident id, String catalog, String schema,
                                       boolean routines) {
        return catalog != null && !catalog.isEmpty() ? List.of("USE " + id.q(catalog) + ";")
                : List.of();
    }

    /**
     * The {@code sql_mode} a MySQL or MariaDB routine or trigger keeps, or
     * {@code null} for any other object or when it cannot be read.
     */
    @Override
    public String sqlMode(Connection conn, String catalog, String name, String kind) {
        String sql;
        switch (kind) {
            case "function":
            case "procedure":
                sql = "SELECT SQL_MODE FROM information_schema.ROUTINES WHERE ROUTINE_SCHEMA = ?"
                        + " AND ROUTINE_NAME = ? AND ROUTINE_TYPE = ?";
                break;
            case "trigger":
                sql = "SELECT SQL_MODE FROM information_schema.TRIGGERS WHERE TRIGGER_SCHEMA = ?"
                        + " AND TRIGGER_NAME = ?";
                break;
            default:
                return null;
        }
        try (PreparedStatement ps = conn.prepareStatement(sql)) {
            ps.setString(1, catalog);
            ps.setString(2, name);
            if (!"trigger".equals(kind)) {
                ps.setString(3, kind.toUpperCase(Locale.ROOT));
            }
            try (ResultSet rs = ps.executeQuery()) {
                return rs.next() ? rs.getString(1) : null;
            }
        } catch (SQLException e) {
            LOG.log(Level.FINE, "no sql_mode for " + name, e);
            return null;
        }
    }

    private static final Logger LOG = Logger.getLogger(MySqlVendor.class.getName());
}
