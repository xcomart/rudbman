package comart.rudbman.bridge.meta;

import com.google.gson.JsonObject;

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
}
