package comart.rudbman.bridge;

import com.google.gson.Gson;
import com.google.gson.GsonBuilder;
import com.google.gson.JsonArray;
import com.google.gson.JsonElement;
import com.google.gson.JsonObject;
import com.google.gson.JsonParser;

import java.nio.charset.StandardCharsets;
import java.util.ArrayList;
import java.util.List;

/**
 * JSON plumbing for the bridge.
 *
 * <p>Gson is used as a tree builder, never as a reflective object mapper: the
 * key names of the wire protocol are written out by hand so they cannot drift
 * when a field is renamed, and no reflective access is needed, which matters
 * under a stripped jlink image.
 */
public final class Json {

    private static final Gson GSON = new GsonBuilder()
            // Absent and null are two different things for a Rust deserialiser.
            // Emitting nulls explicitly keeps every response shape uniform so
            // Option<T> fields need no #[serde(default)].
            .serializeNulls()
            // <, >, & and = are legal in table comments and SQL text; escaping
            // them to < only bloats the payload.
            .disableHtmlEscaping()
            .create();

    private Json() {
    }

    /**
     * Parses a request body into an object.
     *
     * @param req UTF-8 JSON, may be {@code null} or empty
     * @return the parsed object, never {@code null}; an empty object when the
     *         body was absent
     * @throws BridgeException if the body is not a JSON object
     */
    public static JsonObject request(byte[] req) {
        if (req == null || req.length == 0) {
            return new JsonObject();
        }
        JsonElement e;
        try {
            e = JsonParser.parseString(new String(req, StandardCharsets.UTF_8));
        } catch (RuntimeException ex) {
            throw new BridgeException("protocol", "malformed request JSON: " + ex.getMessage(), ex);
        }
        if (!e.isJsonObject()) {
            throw new BridgeException("protocol", "request body must be a JSON object");
        }
        return e.getAsJsonObject();
    }

    /**
     * @param elem a JSON tree
     * @return the UTF-8 serialisation of {@code elem}
     */
    public static byte[] bytes(JsonElement elem) {
        return GSON.toJson(elem).getBytes(StandardCharsets.UTF_8);
    }

    /**
     * @param o    an object
     * @param name a member name
     * @return the member's string value, or {@code null} when absent or JSON null
     */
    public static String str(JsonObject o, String name) {
        JsonElement e = o.get(name);
        return e == null || e.isJsonNull() ? null : e.getAsString();
    }

    /**
     * @param o    an object
     * @param name a member name
     * @param dflt fallback value
     * @return the member's int value, or {@code dflt} when absent or JSON null
     */
    public static int i32(JsonObject o, String name, int dflt) {
        JsonElement e = o.get(name);
        return e == null || e.isJsonNull() ? dflt : e.getAsInt();
    }

    /**
     * Reads a member as a {@code long}.
     *
     * <p>Handles are {@code long} on the wire and a handle read through
     * {@link #i32} would start truncating silently at 2^31; anything carrying a
     * handle or a row count comes through here.
     *
     * @param o    an object
     * @param name a member name
     * @param dflt fallback value
     * @return the member's long value, or {@code dflt} when absent or JSON null
     */
    public static long i64(JsonObject o, String name, long dflt) {
        JsonElement e = o.get(name);
        return e == null || e.isJsonNull() ? dflt : e.getAsLong();
    }

    /**
     * @param o    an object
     * @param name a member name
     * @param dflt fallback value
     * @return the member's boolean value, or {@code dflt} when absent or JSON null
     */
    public static boolean bool(JsonObject o, String name, boolean dflt) {
        JsonElement e = o.get(name);
        return e == null || e.isJsonNull() ? dflt : e.getAsBoolean();
    }

    /**
     * @param o    an object
     * @param name a member name
     * @return the member as an array, or {@code null} when absent or JSON null
     */
    public static JsonArray arr(JsonObject o, String name) {
        JsonElement e = o.get(name);
        return e == null || e.isJsonNull() ? null : e.getAsJsonArray();
    }

    /**
     * @param o    an object
     * @param name a member name
     * @return the member as an object, or {@code null} when absent or JSON null
     */
    public static JsonObject obj(JsonObject o, String name) {
        JsonElement e = o.get(name);
        return e == null || e.isJsonNull() ? null : e.getAsJsonObject();
    }

    /**
     * @param o    an object
     * @param name a member name
     * @return the member as a list of strings, empty when absent
     */
    public static List<String> strings(JsonObject o, String name) {
        List<String> out = new ArrayList<>();
        JsonArray a = arr(o, name);
        if (a != null) {
            for (JsonElement e : a) {
                if (!e.isJsonNull()) {
                    out.add(e.getAsString());
                }
            }
        }
        return out;
    }
}
