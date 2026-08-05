package comart.rudbman.bridge.support;

import com.google.gson.JsonObject;
import comart.rudbman.bridge.Bridge;
import comart.rudbman.bridge.Json;
import comart.rudbman.bridge.Ops;

import java.nio.charset.StandardCharsets;
import java.util.concurrent.atomic.AtomicInteger;

/** H2 in-memory helpers shared by the bridge tests. */
public final class H2 {

    /** H2's JDBC driver class name. */
    public static final String DRIVER = "org.h2.Driver";

    private static final AtomicInteger SEQ = new AtomicInteger();

    private H2() {
    }

    /** @return a URL for a fresh, uniquely named in-memory database. */
    public static String freshUrl() {
        return "jdbc:h2:mem:rudbman" + SEQ.incrementAndGet() + ";DB_CLOSE_DELAY=-1";
    }

    /**
     * Opens a session against a fresh in-memory database.
     *
     * <p>The jar list is left empty on purpose: that is the path where the
     * driver is resolved from the bridge's own class loader, which is how a
     * driver bundled into the jlink image would be reached too.
     *
     * @param url the JDBC URL
     * @return the session handle
     */
    public static long open(String url) {
        JsonObject req = new JsonObject();
        req.addProperty("url", url);
        req.addProperty("driver_class", DRIVER);
        req.addProperty("username", "sa");
        req.addProperty("password", "");
        Resp r = call(Ops.OPEN_SESSION, 0, 0, req);
        r.assertOk();
        return r.num("session");
    }

    /**
     * Runs a statement and discards the cursor.
     *
     * @param session session handle
     * @param sql     the statement
     */
    public static void exec(long session, String sql) {
        JsonObject req = new JsonObject();
        req.addProperty("sql", sql);
        Resp r = call(Ops.EXECUTE, session, 0, req);
        r.assertOk();
        long cursor = r.json().get("cursor").getAsLong();
        Resp.of(Bridge.call(Ops.CLOSE_CURSOR, cursor, 0, null)).assertOk();
    }

    /**
     * Runs a query and leaves the cursor open.
     *
     * @param session session handle
     * @param sql     the query
     * @return the EXECUTE response
     */
    public static Resp query(long session, String sql) {
        JsonObject req = new JsonObject();
        req.addProperty("sql", sql);
        return call(Ops.EXECUTE, session, 0, req);
    }

    /**
     * Invokes the bridge with a JSON request body.
     *
     * @param op     operation code
     * @param handle handle argument
     * @param arg    integer argument
     * @param req    request body, may be {@code null}
     * @return the decoded response
     */
    public static Resp call(int op, long handle, long arg, JsonObject req) {
        byte[] body = req == null ? null : Json.bytes(req);
        return Resp.of(Bridge.call(op, handle, arg, body));
    }

    /**
     * Invokes the bridge with a raw JSON string, for malformed-input tests.
     *
     * @param op     operation code
     * @param handle handle argument
     * @param json   request body as text
     * @return the decoded response
     */
    public static Resp callRaw(int op, long handle, String json) {
        return Resp.of(Bridge.call(op, handle, 0,
                json == null ? null : json.getBytes(StandardCharsets.UTF_8)));
    }

    /**
     * Closes a session handle.
     *
     * @param session session handle
     */
    public static void close(long session) {
        Resp.of(Bridge.call(Ops.CLOSE_SESSION, session, 0, null)).assertOk();
    }
}
