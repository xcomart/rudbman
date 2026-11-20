package comart.rudbman.bridge.meta;

import com.google.gson.JsonObject;

import java.sql.Connection;
import java.sql.SQLException;
import java.util.ArrayList;
import java.util.Collections;
import java.util.List;
import java.util.Map;

/** Microsoft SQL Server. */
final class SqlServerVendor extends Vendor {

    @Override
    boolean names(String productName) {
        return productName.contains("microsoft sql server") || productName.contains("sql server");
    }

    @Override
    boolean numbersProcedures() {
        return true;
    }

    @Override
    String routineText(Connection conn, Ident id, String catalog, String schema, String name,
                       boolean function) {
        return DdlText.sqlServerDefinition(conn,
                id.qualify(catalog, schema, RoutineDdl.unnumbered(name)));
    }

    @Override
    List<String> sequenceQueries(Ident id, String catalog) {
        return List.of(Sequences.sqlServer(id, catalog));
    }

    /**
     * {@code sys.triggers} is per database; a catalog other than the session's
     * is read through its own three-part names.
     */
    @Override
    void triggerQuery(StringBuilder sql, List<String> args, Ident id, String catalog,
                      String schema, String name) {
        String db = catalog == null ? "" : id.q(catalog) + ".";
        sql.append("SELECT ").append(catalog == null ? "DB_NAME()" : "?")
                .append(", s.name, tr.name, o.name,")
                .append(" CASE WHEN tr.is_instead_of_trigger = 1 THEN 'INSTEAD OF'")
                .append(" ELSE 'AFTER' END, te.type_desc, 'STATEMENT',")
                .append(" CASE WHEN tr.is_disabled = 1 THEN 'N' ELSE 'Y' END, NULL")
                .append(" FROM ").append(db).append("sys.triggers tr")
                .append(" JOIN ").append(db).append("sys.objects o")
                .append(" ON o.object_id = tr.parent_id")
                .append(" JOIN ").append(db).append("sys.schemas s")
                .append(" ON s.schema_id = o.schema_id")
                .append(" JOIN ").append(db).append("sys.trigger_events te")
                .append(" ON te.object_id = tr.object_id")
                .append(" WHERE tr.parent_class = 1");
        if (catalog != null) {
            args.add(catalog);
        }
        Triggers.filter(sql, args, "s.name", schema);
        Triggers.filter(sql, args, "tr.name", name);
        sql.append(" ORDER BY 2, 3");
    }

    @Override
    boolean hidesComments() {
        return true;
    }

    @Override
    Map<String, String> tableComments(Connection conn, JsonObject req) throws SQLException {
        if (Comments.elsewhere(conn, req)) {
            return Collections.emptyMap();
        }
        Comments.Filter schema = Comments.Filter.of(req, "schema", "schema_pattern");
        Comments.Filter table = Comments.Filter.of(req, "table", "table_pattern");
        StringBuilder sql = new StringBuilder(Comments.SQLSERVER_TABLES);
        List<String> args = new ArrayList<>();
        schema.append(sql, args, "s.name", Comments.ANSI_ESCAPE);
        table.append(sql, args, "o.name", Comments.ANSI_ESCAPE);
        return Comments.read(conn, sql.toString(), args, 2);
    }

    @Override
    Map<String, String> columnComments(Connection conn, JsonObject req) throws SQLException {
        if (Comments.elsewhere(conn, req)) {
            return Collections.emptyMap();
        }
        Comments.Filter schema = Comments.Filter.of(req, "schema", "schema_pattern");
        Comments.Filter table = Comments.Filter.of(req, "table", "table_pattern");
        Comments.Filter column = Comments.Filter.of(req, "column", "column_pattern");
        StringBuilder sql = new StringBuilder(Comments.SQLSERVER_COLUMNS);
        List<String> args = new ArrayList<>();
        schema.append(sql, args, "s.name", Comments.ANSI_ESCAPE);
        table.append(sql, args, "o.name", Comments.ANSI_ESCAPE);
        column.append(sql, args, "c.name", Comments.ANSI_ESCAPE);
        return Comments.read(conn, sql.toString(), args, 3);
    }

    @Override
    boolean upserts() {
        return true;
    }

    /** {@code MERGE}, which SQL Server requires terminated. */
    @Override
    String upsert(Ident id, String qualified, List<String> columns, List<String> keys,
                  List<String> updates) {
        return Upsert.merge(id, qualified, columns, keys, updates, false, true);
    }
}
