package comart.rudbman.bridge.meta;

import com.google.gson.JsonArray;
import com.google.gson.JsonElement;
import com.google.gson.JsonObject;

import comart.rudbman.bridge.BridgeException;
import comart.rudbman.bridge.Json;

import java.sql.Connection;
import java.util.Locale;

/**
 * The dialect profile an {@code OPEN_SESSION} brings, tied to the session's
 * connection (see {@link Dialect#attach}).
 *
 * <p>Two members of the request, both optional:
 * <ul>
 *   <li>{@code profile} - the profile the connection's driver names:
 *       {@code id}, {@code base} (the built-in dialect id its chain of bases
 *       ends in, as {@code rudbman-sql} spells it) and {@code bridge} (its
 *       {@code bridge} half, merged along that chain).</li>
 *   <li>{@code detect_profiles} - for a connection with no profile of its
 *       own, the profiles that name products by their product name
 *       ({@code bridge.detect}), each in the same shape. The first whose
 *       words the server's product name contains is the session's; none, and
 *       the product name picks a built-in as it always has.</li>
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
     */
    public static void attach(Connection conn, JsonObject req) {
        JsonObject own = Json.obj(req, "profile");
        if (own != null) {
            Dialect.attach(conn, dialect(own));
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
            Dialect dialect = dialect(candidate.getAsJsonObject());
            if (dialect.vendor().names(name)) {
                Dialect.attach(conn, dialect);
                return;
            }
        }
    }

    /** @return the dialect of one profile of the request */
    static Dialect dialect(JsonObject profile) {
        String id = Json.str(profile, "id");
        String base = Json.str(profile, "base");
        Dialect builtIn = Dialect.byId(base);
        if (builtIn == null) {
            throw new BridgeException("protocol",
                    "dialect profile " + id + ": its base '" + base + "' is no built-in dialect");
        }
        return Dialect.profile(id, new ProfileVendor(id, builtIn.vendor(),
                Json.obj(profile, "bridge")));
    }
}
