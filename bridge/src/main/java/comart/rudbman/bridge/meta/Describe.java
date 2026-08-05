package comart.rudbman.bridge.meta;

import com.google.gson.JsonArray;
import com.google.gson.JsonElement;
import com.google.gson.JsonObject;
import comart.rudbman.bridge.BridgeException;
import comart.rudbman.bridge.Json;
import comart.rudbman.bridge.Session;

import java.sql.DatabaseMetaData;
import java.sql.ResultSet;
import java.sql.SQLException;
import java.util.List;
import java.util.Map;

/**
 * {@code DESCRIBE} (op {@code 0x10}): {@link DatabaseMetaData} queries rendered
 * as JSON.
 *
 * <p>The request selects the query with a {@code kind} member instead of an
 * operation code, because the set of metadata queries keeps growing and a
 * per-kind operation code would mean two tables - one in Rust, one in Java -
 * drifting apart. Metadata calls are rare enough that JSON parsing does not
 * matter.
 *
 * <p>Every response has the same shape:
 * <pre>{ "kind": "...", "items": [ { ... }, ... ] }</pre>
 * Key names are fixed snake_case, not the driver's metadata labels, so that the
 * Rust structs stay stable across drivers.
 *
 * <p>The single exception is {@code kind: "ddl"}, which answers
 * <pre>{ "kind": "ddl", "ddl": "CREATE TABLE …", "source": "native" | "metadata" }</pre>
 * because a table's DDL is one document, not a list of rows, and wrapping it in
 * a one-element array would only make the Rust side unwrap it again. See
 * {@link Ddl}.
 *
 * <p>Three kinds reach past {@link DatabaseMetaData}: {@code ddl} and
 * {@code sequences} need vendor catalogue queries ({@link Ddl},
 * {@link Sequences}), and both are written so that an unsupported or forbidden
 * query degrades to a reconstruction or an empty list rather than to an error.
 */
public final class Describe {

    private Describe() {
    }

    /**
     * Runs a metadata query.
     *
     * @param session the session to query
     * @param req     the request body
     * @return the response body
     * @throws SQLException if the driver fails
     */
    public static JsonObject run(Session session, JsonObject req) throws SQLException {
        String kind = Json.str(req, "kind");
        if (kind == null) {
            throw new BridgeException("protocol", "describe requires 'kind'");
        }

        session.lock();
        try {
            DatabaseMetaData dbm = session.metaData();
            if ("ddl".equals(kind)) {
                // The one kind that answers with a document rather than a list.
                JsonObject out = new JsonObject();
                out.addProperty("kind", kind);
                for (Map.Entry<String, JsonElement> e
                        : Ddl.of(session.connection(), dbm, req).entrySet()) {
                    out.add(e.getKey(), e.getValue());
                }
                return out;
            }
            JsonArray items;
            switch (kind) {
                case "catalogs":       items = catalogs(dbm); break;
                case "schemas":        items = schemas(dbm, req); break;
                case "tables":         items = tables(dbm, req); break;
                case "columns":        items = columns(dbm, req); break;
                case "primary_keys":   items = primaryKeys(dbm, req); break;
                case "imported_keys":  items = importedKeys(dbm, req); break;
                case "exported_keys":  items = exportedKeys(dbm, req); break;
                case "indexes":        items = indexes(dbm, req); break;
                case "type_info":      items = typeInfo(dbm); break;
                case "procedures":     items = Routines.procedures(dbm, req); break;
                case "functions":      items = Routines.functions(dbm, req); break;
                case "sequences":      items = Sequences.of(session.connection(), dbm, req); break;
                default:
                    throw new BridgeException("protocol", "unknown describe kind: " + kind);
            }
            JsonObject out = new JsonObject();
            out.addProperty("kind", kind);
            out.add("items", items);
            return out;
        } finally {
            session.unlock();
        }
    }

    private static JsonArray catalogs(DatabaseMetaData dbm) throws SQLException {
        JsonArray arr = new JsonArray();
        try (ResultSet rs = dbm.getCatalogs()) {
            RsView v = new RsView(rs);
            while (rs.next()) {
                JsonObject o = new JsonObject();
                v.putStr(o, "name", "TABLE_CAT");
                arr.add(o);
            }
        }
        return arr;
    }

    private static JsonArray schemas(DatabaseMetaData dbm, JsonObject req) throws SQLException {
        String catalog = Json.str(req, "catalog");
        String pattern = pattern(req);
        JsonArray arr = new JsonArray();
        try (ResultSet rs = openSchemas(dbm, catalog, pattern)) {
            RsView v = new RsView(rs);
            while (rs.next()) {
                JsonObject o = new JsonObject();
                v.putStr(o, "name", "TABLE_SCHEM");
                v.putStr(o, "catalog", "TABLE_CATALOG");
                arr.add(o);
            }
        }
        return arr;
    }

    private static ResultSet openSchemas(DatabaseMetaData dbm, String catalog, String pattern)
            throws SQLException {
        try {
            return dbm.getSchemas(catalog, pattern);
        } catch (SQLException | AbstractMethodError e) {
            // getSchemas(String,String) is JDBC 4.0; drivers predating it, and a
            // few that simply never implemented it, still answer the no-arg form.
            if (catalog == null && pattern == null) {
                return dbm.getSchemas();
            }
            throw e instanceof SQLException ? (SQLException) e
                    : new SQLException("driver does not support getSchemas(catalog, pattern)");
        }
    }

    private static JsonArray tables(DatabaseMetaData dbm, JsonObject req) throws SQLException {
        List<String> types = Json.strings(req, "types");
        String[] typeArr = types.isEmpty() ? null : types.toArray(new String[0]);
        JsonArray arr = new JsonArray();
        try (ResultSet rs = dbm.getTables(Json.str(req, "catalog"), pattern(req),
                tablePattern(req), typeArr)) {
            RsView v = new RsView(rs);
            while (rs.next()) {
                JsonObject o = new JsonObject();
                v.putStr(o, "catalog", "TABLE_CAT");
                v.putStr(o, "schema", "TABLE_SCHEM");
                v.putStr(o, "name", "TABLE_NAME");
                v.putStr(o, "type", "TABLE_TYPE");
                v.putStr(o, "remarks", "REMARKS");
                v.putStr(o, "type_catalog", "TYPE_CAT");
                v.putStr(o, "type_schema", "TYPE_SCHEM");
                v.putStr(o, "type_name", "TYPE_NAME");
                v.putStr(o, "self_ref_column", "SELF_REFERENCING_COL_NAME");
                v.putStr(o, "ref_generation", "REF_GENERATION");
                arr.add(o);
            }
        }
        return arr;
    }

    private static JsonArray columns(DatabaseMetaData dbm, JsonObject req) throws SQLException {
        JsonArray arr = new JsonArray();
        try (ResultSet rs = dbm.getColumns(Json.str(req, "catalog"), pattern(req),
                tablePattern(req), columnPattern(req))) {
            RsView v = new RsView(rs);
            while (rs.next()) {
                JsonObject o = new JsonObject();
                v.putStr(o, "catalog", "TABLE_CAT");
                v.putStr(o, "schema", "TABLE_SCHEM");
                v.putStr(o, "table", "TABLE_NAME");
                v.putStr(o, "name", "COLUMN_NAME");
                Integer type = v.i32("DATA_TYPE");
                o.addProperty("data_type", type);
                o.addProperty("jdbc_type", type == null ? null : SqlTypes.name(type));
                v.putStr(o, "type_name", "TYPE_NAME");
                v.putI32(o, "size", "COLUMN_SIZE");
                v.putI32(o, "digits", "DECIMAL_DIGITS");
                v.putI32(o, "radix", "NUM_PREC_RADIX");
                v.putI32(o, "nullable", "NULLABLE");
                v.putYesNo(o, "is_nullable", "IS_NULLABLE");
                v.putStr(o, "remarks", "REMARKS");
                v.putStr(o, "default", "COLUMN_DEF");
                v.putI32(o, "char_octet_length", "CHAR_OCTET_LENGTH");
                v.putI32(o, "ordinal", "ORDINAL_POSITION");
                v.putYesNo(o, "auto_increment", "IS_AUTOINCREMENT");
                v.putYesNo(o, "generated", "IS_GENERATEDCOLUMN");
                arr.add(o);
            }
        }
        return arr;
    }

    private static JsonArray primaryKeys(DatabaseMetaData dbm, JsonObject req) throws SQLException {
        JsonArray arr = new JsonArray();
        try (ResultSet rs = dbm.getPrimaryKeys(Json.str(req, "catalog"),
                Json.str(req, "schema"), requireTable(req))) {
            RsView v = new RsView(rs);
            while (rs.next()) {
                JsonObject o = new JsonObject();
                v.putStr(o, "catalog", "TABLE_CAT");
                v.putStr(o, "schema", "TABLE_SCHEM");
                v.putStr(o, "table", "TABLE_NAME");
                v.putStr(o, "column", "COLUMN_NAME");
                v.putI32(o, "seq", "KEY_SEQ");
                v.putStr(o, "name", "PK_NAME");
                arr.add(o);
            }
        }
        return arr;
    }

    private static JsonArray importedKeys(DatabaseMetaData dbm, JsonObject req) throws SQLException {
        try (ResultSet rs = dbm.getImportedKeys(Json.str(req, "catalog"),
                Json.str(req, "schema"), requireTable(req))) {
            return foreignKeys(rs);
        }
    }

    private static JsonArray exportedKeys(DatabaseMetaData dbm, JsonObject req) throws SQLException {
        try (ResultSet rs = dbm.getExportedKeys(Json.str(req, "catalog"),
                Json.str(req, "schema"), requireTable(req))) {
            return foreignKeys(rs);
        }
    }

    /**
     * Shared shape for imported and exported keys: both metadata result sets
     * have identical columns, only the direction of the query differs, so both
     * are rendered with the same pk_/fk_ prefixed keys.
     */
    private static JsonArray foreignKeys(ResultSet rs) throws SQLException {
        JsonArray arr = new JsonArray();
        RsView v = new RsView(rs);
        while (rs.next()) {
            JsonObject o = new JsonObject();
            v.putStr(o, "pk_catalog", "PKTABLE_CAT");
            v.putStr(o, "pk_schema", "PKTABLE_SCHEM");
            v.putStr(o, "pk_table", "PKTABLE_NAME");
            v.putStr(o, "pk_column", "PKCOLUMN_NAME");
            v.putStr(o, "fk_catalog", "FKTABLE_CAT");
            v.putStr(o, "fk_schema", "FKTABLE_SCHEM");
            v.putStr(o, "fk_table", "FKTABLE_NAME");
            v.putStr(o, "fk_column", "FKCOLUMN_NAME");
            v.putI32(o, "seq", "KEY_SEQ");
            v.putI32(o, "update_rule", "UPDATE_RULE");
            v.putI32(o, "delete_rule", "DELETE_RULE");
            v.putStr(o, "fk_name", "FK_NAME");
            v.putStr(o, "pk_name", "PK_NAME");
            v.putI32(o, "deferrability", "DEFERRABILITY");
            arr.add(o);
        }
        return arr;
    }

    private static JsonArray indexes(DatabaseMetaData dbm, JsonObject req) throws SQLException {
        boolean uniqueOnly = Json.bool(req, "unique_only", false);
        // Approximate results skip a statistics refresh, which on a large Oracle
        // or SQL Server schema is the difference between instant and a minute.
        boolean approximate = Json.bool(req, "approximate", true);
        JsonArray arr = new JsonArray();
        try (ResultSet rs = dbm.getIndexInfo(Json.str(req, "catalog"), Json.str(req, "schema"),
                requireTable(req), uniqueOnly, approximate)) {
            RsView v = new RsView(rs);
            while (rs.next()) {
                JsonObject o = new JsonObject();
                v.putStr(o, "catalog", "TABLE_CAT");
                v.putStr(o, "schema", "TABLE_SCHEM");
                v.putStr(o, "table", "TABLE_NAME");
                v.putBool(o, "non_unique", "NON_UNIQUE");
                v.putStr(o, "qualifier", "INDEX_QUALIFIER");
                v.putStr(o, "name", "INDEX_NAME");
                v.putI32(o, "type", "TYPE");
                v.putI32(o, "ordinal", "ORDINAL_POSITION");
                v.putStr(o, "column", "COLUMN_NAME");
                v.putStr(o, "asc_desc", "ASC_OR_DESC");
                v.putI64(o, "cardinality", "CARDINALITY");
                v.putI64(o, "pages", "PAGES");
                v.putStr(o, "filter", "FILTER_CONDITION");
                arr.add(o);
            }
        }
        return arr;
    }

    private static JsonArray typeInfo(DatabaseMetaData dbm) throws SQLException {
        JsonArray arr = new JsonArray();
        try (ResultSet rs = dbm.getTypeInfo()) {
            RsView v = new RsView(rs);
            while (rs.next()) {
                JsonObject o = new JsonObject();
                v.putStr(o, "name", "TYPE_NAME");
                Integer type = v.i32("DATA_TYPE");
                o.addProperty("data_type", type);
                o.addProperty("jdbc_type", type == null ? null : SqlTypes.name(type));
                v.putI32(o, "precision", "PRECISION");
                v.putStr(o, "literal_prefix", "LITERAL_PREFIX");
                v.putStr(o, "literal_suffix", "LITERAL_SUFFIX");
                v.putStr(o, "create_params", "CREATE_PARAMS");
                v.putI32(o, "nullable", "NULLABLE");
                v.putBool(o, "case_sensitive", "CASE_SENSITIVE");
                v.putI32(o, "searchable", "SEARCHABLE");
                v.putBool(o, "unsigned", "UNSIGNED_ATTRIBUTE");
                v.putBool(o, "fixed_prec_scale", "FIXED_PREC_SCALE");
                v.putBool(o, "auto_increment", "AUTO_INCREMENT");
                v.putStr(o, "local_name", "LOCAL_TYPE_NAME");
                v.putI32(o, "min_scale", "MINIMUM_SCALE");
                v.putI32(o, "max_scale", "MAXIMUM_SCALE");
                v.putI32(o, "radix", "NUM_PREC_RADIX");
                arr.add(o);
            }
        }
        return arr;
    }

    /** Schema filter: {@code schema} is exact, {@code schema_pattern} allows wildcards. */
    private static String pattern(JsonObject req) {
        String exact = Json.str(req, "schema");
        return exact != null ? exact : Json.str(req, "schema_pattern");
    }

    private static String tablePattern(JsonObject req) {
        String exact = Json.str(req, "table");
        return exact != null ? exact : Json.str(req, "table_pattern");
    }

    private static String columnPattern(JsonObject req) {
        String exact = Json.str(req, "column");
        return exact != null ? exact : Json.str(req, "column_pattern");
    }

    private static String requireTable(JsonObject req) {
        String t = Json.str(req, "table");
        if (t == null || t.isEmpty()) {
            throw new BridgeException("protocol",
                    "this describe kind requires an exact 'table' name");
        }
        return t;
    }
}
