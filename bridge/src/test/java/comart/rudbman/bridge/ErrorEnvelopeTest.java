package comart.rudbman.bridge;

import com.google.gson.JsonObject;
import comart.rudbman.bridge.support.H2;
import comart.rudbman.bridge.support.Resp;
import org.junit.jupiter.api.Test;

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertFalse;
import static org.junit.jupiter.api.Assertions.assertNotNull;
import static org.junit.jupiter.api.Assertions.assertTrue;

/**
 * Failure paths.
 *
 * <p>The point of these is not that the calls fail but that they fail
 * <em>informatively</em>: the UI has to be able to tell "no such table" from
 * "no permission", and that distinction lives in {@code sql_state} and
 * {@code vendor_code}.
 */
class ErrorEnvelopeTest {

    private static JsonObject openReq(String url, String driverClass, String password) {
        JsonObject req = new JsonObject();
        req.addProperty("url", url);
        req.addProperty("driver_class", driverClass);
        req.addProperty("username", "sa");
        req.addProperty("password", password);
        return req;
    }

    @Test
    void urlTheDriverDoesNotUnderstandIsReportedAsSuch() {
        // H2's Driver.connect returns null here rather than throwing. The JDBC
        // spec calls that "I do not understand this URL", and swallowing it
        // would surface later as an unexplained NullPointerException.
        JsonObject err = H2.call(Ops.OPEN_SESSION, 0, 0,
                openReq("jdbc:postgresql://localhost:5432/nope", H2.DRIVER, "")).error();
        assertEquals("driver", err.get("kind").getAsString());
        assertTrue(err.get("message").getAsString().contains("does not accept"),
                err.get("message").getAsString());
    }

    @Test
    void malformedUrlTheDriverDoesUnderstandCarriesASqlState() {
        JsonObject err = H2.call(Ops.OPEN_SESSION, 0, 0,
                openReq("jdbc:h2:file:/nonexistent/rudbman/nope;IFEXISTS=TRUE", H2.DRIVER, ""))
                .error();
        assertEquals("sql", err.get("kind").getAsString());
        String state = err.get("sql_state").getAsString();
        assertNotNull(state);
        assertFalse(state.isEmpty(), "sql_state must be filled in for driver failures");
        assertTrue(err.get("vendor_code").getAsInt() != 0, "vendor_code must be filled in");
    }

    @Test
    void missingDriverClassIsADriverError() {
        JsonObject err = H2.call(Ops.OPEN_SESSION, 0, 0,
                openReq(H2.freshUrl(), "com.example.NoSuchDriver", "")).error();
        assertEquals("driver", err.get("kind").getAsString());
        assertTrue(err.get("message").getAsString().contains("com.example.NoSuchDriver"));
    }

    @Test
    void wrongPasswordCarriesSqlStateAndVendorCode() {
        // The first connection creates the in-memory database and fixes its
        // credentials; DB_CLOSE_DELAY=-1 keeps it alive after we disconnect.
        String url = H2.freshUrl();
        JsonObject create = openReq(url, H2.DRIVER, "correct-horse");
        long s = H2.call(Ops.OPEN_SESSION, 0, 0, create).num("session");
        H2.close(s);

        JsonObject err = H2.call(Ops.OPEN_SESSION, 0, 0,
                openReq(url, H2.DRIVER, "wrong")).error();
        assertEquals("sql", err.get("kind").getAsString());
        assertEquals("28000", err.get("sql_state").getAsString());
        assertTrue(err.get("vendor_code").getAsInt() != 0);
        assertTrue(err.has("causes"));
        assertTrue(err.has("stack"));
    }

    @Test
    void unknownTableCarriesItsOwnSqlState() {
        long s = H2.open(H2.freshUrl());
        JsonObject err = H2.query(s, "select * from no_such_table").error();
        assertEquals("sql", err.get("kind").getAsString());
        // Class 42 is "syntax error or access rule violation". The subclass is
        // vendor specific - H2 says 42S04 where the JDBC examples say 42S02 -
        // so the UI has to match on the class, and the test does the same.
        String state = err.get("sql_state").getAsString();
        assertTrue(state.startsWith("42"), "unexpected sql_state " + state);
        assertTrue(err.get("vendor_code").getAsInt() != 0);
        H2.close(s);
    }

    @Test
    void malformedRequestJsonIsAProtocolError() {
        JsonObject err = H2.callRaw(Ops.OPEN_SESSION, 0, "{not json").error();
        assertEquals("protocol", err.get("kind").getAsString());
    }

    @Test
    void missingRequiredMembersAreProtocolErrors() {
        JsonObject err = H2.callRaw(Ops.OPEN_SESSION, 0, "{\"url\":\"jdbc:h2:mem:x\"}").error();
        assertEquals("protocol", err.get("kind").getAsString());
        assertTrue(err.get("message").getAsString().contains("driver_class"));
    }

    @Test
    void staleCursorHandleIsAProtocolError() {
        long s = H2.open(H2.freshUrl());
        H2.exec(s, "create table t(id int)");
        Resp q = H2.query(s, "select * from t");
        long cursor = q.json().get("cursor").getAsLong();
        Resp.of(Bridge.call(Ops.CLOSE_CURSOR, cursor, 0, null)).assertOk();

        JsonObject err = Resp.of(Bridge.call(Ops.FETCH, cursor, 10, null)).error();
        assertEquals("protocol", err.get("kind").getAsString());
        H2.close(s);
    }

    @Test
    void errorEnvelopeAlwaysHasEveryDocumentedMember() {
        JsonObject err = H2.call(Ops.OPEN_SESSION, 0, 0,
                openReq(H2.freshUrl(), "com.example.Missing", "")).error();
        for (String k : new String[]{"kind", "sql_state", "vendor_code", "message", "causes", "stack"}) {
            assertTrue(err.has(k), "error envelope is missing '" + k + "'");
        }
    }
}
