package comart.rudbman.bridge.meta;

import com.google.gson.JsonArray;
import com.google.gson.JsonObject;
import comart.rudbman.bridge.Json;

import java.sql.DatabaseMetaData;
import java.sql.ResultSet;
import java.sql.SQLException;
import java.util.HashMap;
import java.util.Map;
import java.util.logging.Level;
import java.util.logging.Logger;

/**
 * {@code DESCRIBE} kinds {@code procedures} and {@code functions}.
 *
 * <p>Each item carries its own {@code parameters} array rather than leaving the
 * caller to ask again per routine, because the explorer tree shows a signature -
 * {@code F_ADD(P1 INTEGER, P2 INTEGER) RETURNS INTEGER} - and a per-routine
 * round trip for a schema with two hundred procedures is two hundred round
 * trips. The parameter list is fetched once, with the same pattern arguments,
 * and joined in memory.
 *
 * <p>The join key is {@code SPECIFIC_NAME} when the driver supplies one on both
 * sides, and the routine name otherwise. That distinction is what keeps
 * overloads apart: two procedures may share a name and differ only in their
 * specific name, which is precisely why JDBC 4.0 added the column.
 *
 * <p>Parameter modes are reported both as the raw JDBC code and as text, and the
 * two families do <b>not</b> share codes. {@code procedureColumnOut} is 4 and
 * {@code procedureColumnResult} is 3, while {@code functionColumnOut} is 3 and
 * {@code functionColumnResult} is 5. Reading one table with the other's
 * constants silently mislabels every output parameter, so the two mappings are
 * kept separate here.
 *
 * <p>Products differ on which of the two lists a routine appears in. H2 2.x, for
 * one, returns an empty result from {@code getFunctions} unconditionally and
 * reports {@code CREATE ALIAS} functions through {@code getProcedures} with
 * {@code PROCEDURE_TYPE = procedureReturnsResult}. The UI should therefore treat
 * an empty list as "this server files them elsewhere", not as "there are none".
 */
public final class Routines {

    private static final Logger LOG = Logger.getLogger(Routines.class.getName());

    private Routines() {
    }

    /**
     * Lists stored procedures with their parameters.
     *
     * <p>Request members: {@code catalog}, {@code schema} / {@code schema_pattern},
     * {@code name} / {@code name_pattern}.
     *
     * @param dbm the connection metadata
     * @param req the request body
     * @return the item array
     * @throws SQLException if the driver fails on {@code getProcedures} itself
     */
    public static JsonArray procedures(DatabaseMetaData dbm, JsonObject req) throws SQLException {
        String catalog = Json.str(req, "catalog");
        String schema = pattern(req, "schema");
        String name = pattern(req, "name");

        Params params = collect("getProcedureColumns", () ->
                dbm.getProcedureColumns(catalog, schema, name, null), true);

        JsonArray arr = new JsonArray();
        try (ResultSet rs = dbm.getProcedures(catalog, schema, name)) {
            RsView v = new RsView(rs);
            while (rs.next()) {
                JsonObject o = new JsonObject();
                v.putStr(o, "catalog", "PROCEDURE_CAT");
                v.putStr(o, "schema", "PROCEDURE_SCHEM");
                v.putStr(o, "name", "PROCEDURE_NAME");
                v.putStr(o, "specific_name", "SPECIFIC_NAME");
                v.putStr(o, "remarks", "REMARKS");
                Integer type = v.i32("PROCEDURE_TYPE");
                o.addProperty("type", type);
                o.addProperty("type_name", procedureType(type));
                o.add("parameters", params.forRoutine(v.str("SPECIFIC_NAME"),
                        v.str("PROCEDURE_NAME")));
                arr.add(o);
            }
        }
        return arr;
    }

    /**
     * Lists user-defined functions with their parameters.
     *
     * <p>Request members are the same as {@link #procedures}.
     *
     * @param dbm the connection metadata
     * @param req the request body
     * @return the item array, possibly empty on a server that files functions
     *         under {@code getProcedures} instead
     * @throws SQLException if the driver fails on {@code getFunctions} itself
     */
    public static JsonArray functions(DatabaseMetaData dbm, JsonObject req) throws SQLException {
        String catalog = Json.str(req, "catalog");
        String schema = pattern(req, "schema");
        String name = pattern(req, "name");

        Params params = collect("getFunctionColumns", () ->
                dbm.getFunctionColumns(catalog, schema, name, null), false);

        JsonArray arr = new JsonArray();
        try (ResultSet rs = dbm.getFunctions(catalog, schema, name)) {
            RsView v = new RsView(rs);
            while (rs.next()) {
                JsonObject o = new JsonObject();
                v.putStr(o, "catalog", "FUNCTION_CAT");
                v.putStr(o, "schema", "FUNCTION_SCHEM");
                v.putStr(o, "name", "FUNCTION_NAME");
                v.putStr(o, "specific_name", "SPECIFIC_NAME");
                v.putStr(o, "remarks", "REMARKS");
                Integer type = v.i32("FUNCTION_TYPE");
                o.addProperty("type", type);
                o.addProperty("type_name", functionType(type));
                o.add("parameters", params.forRoutine(v.str("SPECIFIC_NAME"),
                        v.str("FUNCTION_NAME")));
                arr.add(o);
            }
        }
        return arr;
    }

    /** Supplier of a metadata result set, allowed to throw. */
    private interface Rows {
        ResultSet get() throws SQLException;
    }

    /** Parameters grouped by specific name and by routine name. */
    private static final class Params {
        private final Map<String, JsonArray> bySpecific = new HashMap<>();
        private final Map<String, JsonArray> byName = new HashMap<>();

        JsonArray forRoutine(String specific, String name) {
            JsonArray a = specific == null ? null : bySpecific.get(specific);
            if (a == null && name != null) {
                a = byName.get(name);
            }
            // A fresh array per lookup would be wasteful, but the same array
            // instance cannot be attached to two JSON trees; Gson does not copy.
            return a == null ? new JsonArray() : a.deepCopy();
        }
    }

    /**
     * Reads a parameter result set into per-routine groups.
     *
     * <p>A driver that refuses this call costs the caller the signatures, not
     * the routine list: several drivers throw from {@code getFunctionColumns}
     * while answering {@code getFunctions} perfectly well, and an empty
     * parameter array is a far better answer than a failed request.
     */
    private static Params collect(String what, Rows rows, boolean procedure) {
        Params out = new Params();
        try (ResultSet rs = rows.get()) {
            RsView v = new RsView(rs);
            String catPrefix = procedure ? "PROCEDURE_" : "FUNCTION_";
            while (rs.next()) {
                JsonObject p = new JsonObject();
                v.putStr(p, "name", "COLUMN_NAME");
                Integer mode = v.i32("COLUMN_TYPE");
                p.addProperty("mode", mode);
                p.addProperty("mode_name", procedure ? procedureMode(mode) : functionMode(mode));
                Integer type = v.i32("DATA_TYPE");
                p.addProperty("data_type", type);
                p.addProperty("jdbc_type", type == null ? null : SqlTypes.name(type));
                v.putStr(p, "type_name", "TYPE_NAME");
                v.putI32(p, "precision", "PRECISION");
                v.putI32(p, "length", "LENGTH");
                v.putI32(p, "scale", "SCALE");
                v.putI32(p, "radix", "RADIX");
                v.putI32(p, "nullable", "NULLABLE");
                v.putYesNo(p, "is_nullable", "IS_NULLABLE");
                v.putStr(p, "remarks", "REMARKS");
                v.putStr(p, "default", "COLUMN_DEF");
                v.putI32(p, "ordinal", "ORDINAL_POSITION");

                String specific = v.str("SPECIFIC_NAME");
                String name = v.str(catPrefix + "NAME");
                if (specific != null) {
                    out.bySpecific.computeIfAbsent(specific, k -> new JsonArray()).add(p);
                }
                if (name != null) {
                    out.byName.computeIfAbsent(name, k -> new JsonArray()).add(p);
                }
            }
        } catch (SQLException | RuntimeException | AbstractMethodError e) {
            LOG.log(Level.FINE, "routine parameters unavailable via " + what, e);
        }
        return out;
    }

    private static String procedureType(Integer code) {
        if (code == null) {
            return null;
        }
        switch (code) {
            case DatabaseMetaData.procedureNoResult:      return "no_result";
            case DatabaseMetaData.procedureReturnsResult: return "returns_result";
            default:                                      return "unknown";
        }
    }

    private static String functionType(Integer code) {
        if (code == null) {
            return null;
        }
        switch (code) {
            case DatabaseMetaData.functionNoTable:      return "no_table";
            case DatabaseMetaData.functionReturnsTable: return "returns_table";
            default:                                    return "unknown";
        }
    }

    private static String procedureMode(Integer code) {
        if (code == null) {
            return null;
        }
        switch (code) {
            case DatabaseMetaData.procedureColumnIn:     return "IN";
            case DatabaseMetaData.procedureColumnInOut:  return "INOUT";
            case DatabaseMetaData.procedureColumnResult: return "RESULT";
            case DatabaseMetaData.procedureColumnOut:    return "OUT";
            case DatabaseMetaData.procedureColumnReturn: return "RETURN";
            default:                                     return "UNKNOWN";
        }
    }

    private static String functionMode(Integer code) {
        if (code == null) {
            return null;
        }
        switch (code) {
            case DatabaseMetaData.functionColumnIn:     return "IN";
            case DatabaseMetaData.functionColumnInOut:  return "INOUT";
            case DatabaseMetaData.functionColumnOut:    return "OUT";
            case DatabaseMetaData.functionReturn:       return "RETURN";
            case DatabaseMetaData.functionColumnResult: return "RESULT";
            default:                                    return "UNKNOWN";
        }
    }

    /** Exact member wins over its {@code _pattern} sibling, as elsewhere in DESCRIBE. */
    private static String pattern(JsonObject req, String key) {
        String exact = Json.str(req, key);
        return exact != null ? exact : Json.str(req, key + "_pattern");
    }
}
