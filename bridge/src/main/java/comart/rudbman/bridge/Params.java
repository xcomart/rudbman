package comart.rudbman.bridge;

import com.google.gson.JsonArray;
import com.google.gson.JsonElement;
import com.google.gson.JsonObject;
import com.google.gson.JsonPrimitive;

import java.math.BigDecimal;
import java.sql.Date;
import java.sql.PreparedStatement;
import java.sql.SQLException;
import java.sql.Time;
import java.sql.Timestamp;
import java.sql.Types;
import java.util.Base64;

/**
 * Binds the {@code params} array of an {@code EXECUTE} request onto a
 * {@link PreparedStatement}.
 *
 * <p>Two forms are accepted per parameter:
 * <ul>
 *   <li>a bare JSON scalar - {@code null}, boolean, number or string - bound by
 *       its JSON type;</li>
 *   <li>an object {@code {"type": "...", "value": ...}} for everything JSON
 *       cannot express without loss.</li>
 * </ul>
 *
 * <p>The typed form exists because JSON has exactly one numeric type and no date
 * type. A {@code DECIMAL(20,8)} sent as a JSON number would be routed through a
 * double and arrive rounded, which is the same mistake the batch codec refuses
 * to make in the other direction.
 *
 * <p>Recognised type names: {@code null}, {@code bool}, {@code i64},
 * {@code f64}, {@code string}, {@code decimal}, {@code date}, {@code time},
 * {@code timestamp}, {@code bytes} (base64).
 */
public final class Params {

    private Params() {
    }

    /**
     * @param ps     the statement
     * @param params the request's {@code params} array
     * @throws SQLException if the driver rejects a binding
     */
    public static void bind(PreparedStatement ps, JsonArray params) throws SQLException {
        for (int i = 0; i < params.size(); i++) {
            bindOne(ps, i + 1, params.get(i));
        }
    }

    private static void bindOne(PreparedStatement ps, int idx, JsonElement e) throws SQLException {
        if (e == null || e.isJsonNull()) {
            setNull(ps, idx);
            return;
        }
        if (e.isJsonObject()) {
            bindTyped(ps, idx, e.getAsJsonObject());
            return;
        }
        if (!e.isJsonPrimitive()) {
            throw new BridgeException("protocol", "parameter " + idx + " is not a scalar");
        }
        JsonPrimitive p = e.getAsJsonPrimitive();
        if (p.isBoolean()) {
            ps.setBoolean(idx, p.getAsBoolean());
        } else if (p.isNumber()) {
            String raw = p.getAsString();
            if (raw.indexOf('.') < 0 && raw.indexOf('e') < 0 && raw.indexOf('E') < 0) {
                ps.setLong(idx, p.getAsLong());
            } else {
                ps.setDouble(idx, p.getAsDouble());
            }
        } else {
            ps.setString(idx, p.getAsString());
        }
    }

    private static void bindTyped(PreparedStatement ps, int idx, JsonObject o) throws SQLException {
        String type = Json.str(o, "type");
        JsonElement v = o.get("value");
        if (type == null) {
            throw new BridgeException("protocol", "parameter " + idx + " object needs a 'type'");
        }
        if (v == null || v.isJsonNull()) {
            setNull(ps, idx);
            return;
        }
        switch (type) {
            case "null":
                setNull(ps, idx);
                break;
            case "bool":
                ps.setBoolean(idx, v.getAsBoolean());
                break;
            case "i64":
                ps.setLong(idx, v.getAsLong());
                break;
            case "f64":
                ps.setDouble(idx, v.getAsDouble());
                break;
            case "string":
                ps.setString(idx, v.getAsString());
                break;
            case "decimal":
                // Always carried as text, never as a JSON number.
                ps.setBigDecimal(idx, new BigDecimal(v.getAsString()));
                break;
            case "date":
                ps.setDate(idx, Date.valueOf(v.getAsString()));
                break;
            case "time":
                ps.setTime(idx, Time.valueOf(v.getAsString()));
                break;
            case "timestamp":
                ps.setTimestamp(idx, Timestamp.valueOf(v.getAsString()));
                break;
            case "bytes":
                ps.setBytes(idx, Base64.getDecoder().decode(v.getAsString()));
                break;
            default:
                throw new BridgeException("protocol",
                        "parameter " + idx + " has unknown type '" + type + "'");
        }
    }

    private static void setNull(PreparedStatement ps, int idx) throws SQLException {
        try {
            ps.setNull(idx, Types.NULL);
        } catch (SQLException e) {
            // Types.NULL is legal but several drivers insist on a concrete type
            // code; VARCHAR is the one they all accept for an untyped null.
            ps.setNull(idx, Types.VARCHAR);
        }
    }
}
