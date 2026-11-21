package comart.rudbman.bridge.meta;

import com.google.gson.JsonArray;
import com.google.gson.JsonElement;
import com.google.gson.JsonObject;

import comart.rudbman.bridge.BridgeException;
import comart.rudbman.bridge.Json;

import java.sql.Connection;
import java.util.List;
import java.util.Locale;

/**
 * The dialect profile an {@code OPEN_SESSION} brings, tied to the session's
 * connection (see {@link Dialect#attach}).
 *
 * <p>Three members of the request, all optional:
 * <ul>
 *   <li>{@code profile} - the profile the connection's driver names:
 *       {@code id}, {@code base} (the built-in dialect id its chain of bases
 *       ends in, as {@code rudbman-sql} spells it), {@code bridge} (its
 *       {@code bridge} half, merged along that chain) and, where the profile
 *       has a script the user has allowed, {@code script}: its {@code path}
 *       and {@code text} ({@code docs/dialect-profiles.md}, §5).</li>
 *   <li>{@code detect_profiles} - for a connection with no profile of its
 *       own, the profiles that name products by their product name
 *       ({@code bridge.detect}), each in the same shape. The first whose
 *       words the server's product name contains is the session's; none, and
 *       the product name picks a built-in as it always has.</li>
 *   <li>{@code groovy} - the JARs of the Groovy runtime a script runs on,
 *       sent when a profile of the request has one.</li>
 * </ul>
 */
public final class Profiles {

    private Profiles() {
    }

    /**
     * Attaches the profile {@code req} names or detects to {@code conn}.
     *
     * @param conn the session's connection, just opened
     * @param req  the {@code OPEN_SESSION} request
     * @throws BridgeException if a profile's base is no built-in or a value
     *                         is not of its key's shape
     * @throws ScriptFailure   if the profile's script does not compile or
     *                         defines a hook of the wrong shape
     */
    public static void attach(Connection conn, JsonObject req) {
        List<String> groovy = Json.strings(req, "groovy");
        JsonObject own = Json.obj(req, "profile");
        if (own != null) {
            Dialect.attach(conn, dialect(own, groovy));
            return;
        }
        JsonElement listed = req.get("detect_profiles");
        if (listed == null || !listed.isJsonArray() || listed.getAsJsonArray().isEmpty()) {
            return;
        }
        String product;
        try {
            product = conn.getMetaData().getDatabaseProductName();
        } catch (Exception | AbstractMethodError e) {
            return;
        }
        if (product == null) {
            return;
        }
        String name = product.toLowerCase(Locale.ROOT);
        JsonArray candidates = listed.getAsJsonArray();
        for (JsonElement candidate : candidates) {
            if (!candidate.isJsonObject()) {
                continue;
            }
            // The words are read before anything is built, so that only the
            // profile the session takes has its script compiled.
            JsonObject profile = candidate.getAsJsonObject();
            if (names(profile, name)) {
                Dialect.attach(conn, dialect(profile, groovy));
                return;
            }
        }
    }

    /** @return whether a profile's {@code bridge.detect} words name the product */
    private static boolean names(JsonObject profile, String productName) {
        JsonObject bridge = Json.obj(profile, "bridge");
        JsonElement words = bridge == null ? null : bridge.get("detect");
        if (words == null || !words.isJsonArray()) {
            return false;
        }
        for (JsonElement word : words.getAsJsonArray()) {
            if (word.isJsonPrimitive()
                    && productName.contains(word.getAsString().toLowerCase(Locale.ROOT))) {
                return true;
            }
        }
        return false;
    }

    /** @return the dialect of one profile of the request, with no script runtime */
    static Dialect dialect(JsonObject profile) {
        return dialect(profile, List.of());
    }

    /**
     * @param profile one profile of the request
     * @param groovy  the Groovy runtime's JARs, for a profile with a script
     * @return its dialect: its data over its base and, with a script, the
     *         script's hooks over both
     */
    static Dialect dialect(JsonObject profile, List<String> groovy) {
        String id = Json.str(profile, "id");
        String base = Json.str(profile, "base");
        Dialect builtIn = Dialect.byId(base);
        if (builtIn == null) {
            throw new BridgeException("protocol",
                    "dialect profile " + id + ": its base '" + base + "' is no built-in dialect");
        }
        JsonObject bridge = Json.obj(profile, "bridge");
        ProfileVendor data = new ProfileVendor(id, builtIn.vendor(), bridge);
        JsonObject script = Json.obj(profile, "script");
        if (script == null) {
            return Dialect.profile(id, data);
        }
        ProfileScript.Hooks hooks = ProfileScript.load(id, script, groovy).bind(data);
        return Dialect.profile(id, new ProfileVendor(id, builtIn.vendor(), bridge, hooks));
    }
}
