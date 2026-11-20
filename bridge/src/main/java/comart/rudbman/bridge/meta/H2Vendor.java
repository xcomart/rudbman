package comart.rudbman.bridge.meta;

import java.util.Locale;
import java.sql.Types;
import java.sql.SQLException;
import java.sql.ResultSet;
import java.math.BigDecimal;
import java.util.Set;
import java.sql.Connection;
import java.util.List;

/** H2, product name {@code "H2"}. */
final class H2Vendor extends Vendor {

    @Override
    boolean names(String productName) {
        return productName.startsWith("h2");
    }

    @Override
    String routineText(Connection conn, Ident id, String catalog, String schema, String name,
                       boolean function) {
        return DdlText.h2Script(conn, id, schema, "ALIAS", name);
    }

    @Override
    List<String> sequenceQueries(Ident id, String catalog) {
        return List.of(Sequences.H2_SQL);
    }

    /** The standard trigger view, with H2's comment column. */
    @Override
    void triggerQuery(StringBuilder sql, List<String> args, Ident id, String catalog,
                      String schema, String name) {
        sql.append(String.format(Triggers.STANDARD_SQL, "REMARKS"));
        Triggers.filter(sql, args, "TRIGGER_SCHEMA", schema);
        Triggers.filter(sql, args, "TRIGGER_NAME", name);
        sql.append(" ORDER BY 2, 3");
    }

    @Override
    boolean upserts() {
        return true;
    }

    @Override
    String upsert(Ident id, String qualified, List<String> columns, List<String> keys,
                  List<String> updates) {
        return Upsert.h2Merge(id, qualified, columns, keys);
    }

    @Override
    public boolean overridesSystemValue() {
        return true;
    }

    @Override
    String tableText(Connection conn, Ident id, String catalog, String schema, String table) {
        String qualified = id.qualify(catalog, schema, table);
        return Attempt.<String>run(conn, "SCRIPT TABLE",
                st -> Ddl.h2Script(st, qualified, schema, table), null);
    }

    @Override
    Ddl.Script nativeSplit(Connection conn, Ident id, String catalog, String schema,
                           String table) {
        return Ddl.h2Split(conn, id, catalog, schema, table);
    }

    @Override
    Set<String> alwaysIdentities(Connection conn, String schema, String table) {
        return Ddl.h2IdentityAlways(conn, schema, table);
    }

    @Override
    String viewText(Connection conn, ObjectDdl.Target t) {
        return DdlText.h2Script(conn, t.id, t.schema, "VIEW", t.name);
    }

    @Override
    String sequenceText(Connection conn, ObjectDdl.Target t) {
        return DdlText.h2Script(conn, t.id, t.schema, "SEQUENCE", t.name);
    }

    @Override
    String triggerText(Connection conn, ObjectDdl.Target t) {
        return DdlText.h2Script(conn, t.id, t.schema, "TRIGGER", t.name);
    }

    @Override
    public String literal(ResultSet rs, int i, int type) throws SQLException {
        return arraysAndJson(rs, i, type);
    }

    @Override
    public boolean dropsIfExists() {
        return true;
    }

    @Override
    public String dropForeignKey(String qualified, String key) {
        return "ALTER TABLE IF EXISTS " + qualified + " DROP CONSTRAINT IF EXISTS " + key + ";";
    }

    /**
     * H2's arrays and JSON, whose text a plain string does not turn back
     * into: an array reads as {@code [1, 2]}, which H2 does not parse, and a
     * string assigned to a JSON column becomes a JSON string holding that
     * text. {@code null} for every other column.
     */
    private static String arraysAndJson(ResultSet rs, int i, int type) throws SQLException {
        if (type == Types.ARRAY) {
            java.sql.Array a = rs.getArray(i);
            if (a == null || rs.wasNull()) {
                return "NULL";
            }
            StringBuilder sb = new StringBuilder("ARRAY[");
            Object[] items = (Object[]) a.getArray();
            for (int k = 0; k < items.length; k++) {
                if (k > 0) {
                    sb.append(", ");
                }
                Object v = items[k];
                if (v == null) {
                    sb.append("NULL");
                } else if (v instanceof Number || v instanceof Boolean) {
                    sb.append(v instanceof BigDecimal ? ((BigDecimal) v).toPlainString()
                            : v.toString().toUpperCase(Locale.ROOT));
                } else if (v instanceof byte[]) {
                    sb.append("X'").append(Ident.hex((byte[]) v)).append('\'');
                } else {
                    sb.append(Ident.literal(v.toString()));
                }
            }
            return sb.append(']').toString();
        }
        if ("JSON".equalsIgnoreCase(rs.getMetaData().getColumnTypeName(i))) {
            byte[] json = rs.getBytes(i);
            return json == null || rs.wasNull() ? "NULL"
                    : "JSON " + Ident.literal(new String(json, java.nio.charset.StandardCharsets.UTF_8));
        }
        return null;
    }
}
