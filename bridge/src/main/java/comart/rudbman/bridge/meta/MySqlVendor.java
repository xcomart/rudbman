package comart.rudbman.bridge.meta;

import com.google.gson.JsonObject;

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
}
