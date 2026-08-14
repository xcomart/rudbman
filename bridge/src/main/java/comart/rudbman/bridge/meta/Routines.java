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
                // getProcedures column order, which the reads have to follow -
                // see RsView. The specific name is column 9, so it cannot be
                // fetched next to the name at 3 the way the output lists them,
                // and both go into locals because the join below wants them a
                // second time and a column may only be read once.
                v.putStr(o, "catalog", "PROCEDURE_CAT");   // 1
                v.putStr(o, "schema", "PROCEDURE_SCHEM");  // 2
                String routine = v.str("PROCEDURE_NAME");  // 3
                String remarks = v.str("REMARKS");         // 7
                Integer type = v.i32("PROCEDURE_TYPE");    // 8
                String specific = v.str("SPECIFIC_NAME");  // 9
                o.addProperty("name", routine);
                o.addProperty("specific_name", specific);
                o.addProperty("remarks", remarks);
                o.addProperty("type", type);
                o.addProperty("type_name", procedureType(type));
                o.add("parameters", params.forRoutine(specific, routine));
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
                // getFunctions column order, as in procedures above.
                v.putStr(o, "catalog", "FUNCTION_CAT");    // 1
                v.putStr(o, "schema", "FUNCTION_SCHEM");   // 2
                String routine = v.str("FUNCTION_NAME");   // 3
                String remarks = v.str("REMARKS");         // 4
                Integer type = v.i32("FUNCTION_TYPE");     // 5
                String specific = v.str("SPECIFIC_NAME");  // 6
                o.addProperty("name", routine);
                o.addProperty("specific_name", specific);
                o.addProperty("remarks", remarks);
                o.addProperty("type", type);
                o.addProperty("type_name", functionType(type));
                o.add("parameters", params.forRoutine(specific, routine));
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
                // Both parameter result sets have to be read in column order -
                // see RsView - and the two number their columns differently
                // past the remarks, so the ordinals below are given as
                // procedures / functions. The routine name is column 3, left of
                // everything else, so it is taken first even though it is only
                // wanted for the grouping at the end. getFunctionColumns has no
                // COLUMN_DEF at all, which RsView reports as an absent label.
                String routine = v.str(catPrefix + "NAME");   // 3
                v.putStr(p, "name", "COLUMN_NAME");           // 4
                Integer mode = v.i32("COLUMN_TYPE");          // 5
                p.addProperty("mode", mode);
                p.addProperty("mode_name", procedure ? procedureMode(mode) : functionMode(mode));
                Integer type = v.i32("DATA_TYPE");            // 6
                p.addProperty("data_type", type);
                p.addProperty("jdbc_type", type == null ? null : SqlTypes.name(type));
                v.putStr(p, "type_name", "TYPE_NAME");        // 7
                v.putI32(p, "precision", "PRECISION");        // 8
                v.putI32(p, "length", "LENGTH");              // 9
                v.putI32(p, "scale", "SCALE");                // 10
                v.putI32(p, "radix", "RADIX");                // 11
                v.putI32(p, "nullable", "NULLABLE");          // 12
                v.putStr(p, "remarks", "REMARKS");            // 13
                v.putStr(p, "default", "COLUMN_DEF");         // 14 / absent
                v.putI32(p, "ordinal", "ORDINAL_POSITION");   // 18 / 15
                v.putYesNo(p, "is_nullable", "IS_NULLABLE");  // 19 / 16
                String specific = v.str("SPECIFIC_NAME");     // 20 / 17

                if (specific != null) {
                    out.bySpecific.computeIfAbsent(specific, k -> new JsonArray()).add(p);
                }
                if (routine != null) {
                    out.byName.computeIfAbsent(routine, k -> new JsonArray()).add(p);
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
