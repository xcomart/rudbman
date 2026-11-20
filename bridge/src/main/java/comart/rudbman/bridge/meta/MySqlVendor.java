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
}
