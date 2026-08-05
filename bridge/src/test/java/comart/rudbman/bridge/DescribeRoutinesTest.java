package comart.rudbman.bridge;

import com.google.gson.JsonArray;
import com.google.gson.JsonElement;
import com.google.gson.JsonObject;
import comart.rudbman.bridge.support.H2;
import org.junit.jupiter.api.AfterEach;
import org.junit.jupiter.api.BeforeEach;
import org.junit.jupiter.api.Test;

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertFalse;
import static org.junit.jupiter.api.Assertions.assertNotNull;
import static org.junit.jupiter.api.Assertions.assertTrue;

/** DESCRIBE kinds {@code procedures}, {@code functions} and {@code sequences}. */
class DescribeRoutinesTest {

    private long session;

    /** Body of the H2 alias created below; H2 calls it through reflection. */
    public static int add(int x, int y) {
        return x + y;
    }

    @BeforeEach
    void setUp() {
        session = H2.open(H2.freshUrl());
        H2.exec(session, "create schema app");
        H2.exec(session, "create alias app.f_add for "
                + "'comart.rudbman.bridge.DescribeRoutinesTest.add'");
        H2.exec(session, "create sequence app.seq_order start with 100 increment by 5 "
                + "minvalue 10 maxvalue 100000 cycle cache 20");
        H2.exec(session, "create sequence app.seq_plain");
    }

    @AfterEach
    void tearDown() {
        H2.close(session);
    }

    private JsonArray describe(String kind, String... kv) {
        JsonObject req = new JsonObject();
        req.addProperty("kind", kind);
        for (int i = 0; i < kv.length; i += 2) {
            req.addProperty(kv[i], kv[i + 1]);
        }
        JsonObject resp = H2.call(Ops.DESCRIBE, session, 0, req).json();
        assertEquals(kind, resp.get("kind").getAsString());
        return resp.get("items").getAsJsonArray();
    }

    private static JsonObject pick(JsonArray arr, String name) {
        for (JsonElement e : arr) {
            if (name.equals(e.getAsJsonObject().get("name").getAsString())) {
                return e.getAsJsonObject();
            }
        }
        throw new AssertionError("no item named " + name + " in " + arr);
    }

    @Test
    void proceduresCarryTheirSignature() {
        JsonObject p = pick(describe("procedures", "schema", "APP"), "F_ADD");
        assertEquals("APP", p.get("schema").getAsString());
        assertEquals("returns_result", p.get("type_name").getAsString());
        assertNotNull(p.get("specific_name").getAsString());

        // The signature is what the explorer tree draws, so it travels with the
        // routine rather than costing a second round trip per routine.
        JsonArray params = p.get("parameters").getAsJsonArray();
        assertEquals(3, params.size(), params.toString());

        JsonObject result = params.get(0).getAsJsonObject();
        assertEquals("RESULT", result.get("name").getAsString());
        // procedureColumnReturn is 5 - and functionReturn is 4. Reading one
        // table with the other's constants is the mistake this asserts against.
        assertEquals("RETURN", result.get("mode_name").getAsString());
        assertEquals("INTEGER", result.get("jdbc_type").getAsString());

        JsonObject first = params.get(1).getAsJsonObject();
        assertEquals("P1", first.get("name").getAsString());
        assertEquals("IN", first.get("mode_name").getAsString());
        assertEquals("INTEGER", first.get("jdbc_type").getAsString());
        assertEquals(1, first.get("ordinal").getAsInt());

        assertEquals("P2", params.get(2).getAsJsonObject().get("name").getAsString());
    }

    @Test
    void proceduresCanBeNarrowedToOneName() {
        assertEquals(1, describe("procedures", "schema", "APP", "name", "F_ADD").size());
        assertTrue(describe("procedures", "schema", "APP", "name", "NOPE").isEmpty());
    }

    /**
     * H2 2.x returns an empty result from {@code getFunctions} unconditionally -
     * {@code DatabaseMetaLocalBase.getFunctions} is final and answers nothing -
     * and files {@code CREATE ALIAS} routines under {@code getProcedures}
     * instead. So the assertion here is that the kind answers cleanly, not that
     * it finds something; if a future H2 starts populating the call, this test
     * is where that shows up.
     */
    @Test
    void functionsAnswerEvenWhenTheDriverFilesThemElsewhere() {
        JsonArray items = describe("functions", "schema", "APP");
        assertTrue(items.isEmpty(), "H2 2.x is expected to report no functions: " + items);
        assertFalse(describe("procedures", "schema", "APP").isEmpty(),
                "the same routine must be reachable through 'procedures'");
    }

    @Test
    void sequencesCarryTheirOptions() {
        JsonArray items = describe("sequences", "schema", "APP");
        assertEquals(2, items.size(), items.toString());

        JsonObject s = pick(items, "SEQ_ORDER");
        assertEquals("APP", s.get("schema").getAsString());
        assertEquals("100", s.get("start_value").getAsString());
        assertEquals("5", s.get("increment").getAsString());
        assertEquals("10", s.get("min_value").getAsString());
        assertEquals("100000", s.get("max_value").getAsString());
        assertEquals("20", s.get("cache").getAsString());
        assertTrue(s.get("cycle").getAsBoolean());
        assertEquals("BIGINT", s.get("data_type").getAsString());

        // A sequence taking every default still fills every member, as JSON
        // null where the product has nothing to say.
        JsonObject plain = pick(items, "SEQ_PLAIN");
        assertFalse(plain.get("cycle").getAsBoolean());
        assertTrue(plain.has("remarks"));
        assertTrue(plain.has("current_value"));
    }

    @Test
    void sequencesCanBeNarrowedToOneName() {
        JsonArray one = describe("sequences", "schema", "APP", "name", "SEQ_PLAIN");
        assertEquals(1, one.size());
        assertEquals("SEQ_PLAIN", one.get(0).getAsJsonObject().get("name").getAsString());
        assertTrue(describe("sequences", "schema", "NO_SUCH_SCHEMA").isEmpty());
    }

    /**
     * The four kinds this milestone added must not answer the deferred-feature
     * error any more. An empty list is a valid answer; an error is not.
     */
    @Test
    void formerlyDeferredKindsAreImplemented() {
        for (String kind : new String[]{"procedures", "functions", "sequences"}) {
            JsonObject req = new JsonObject();
            req.addProperty("kind", kind);
            req.addProperty("schema", "APP");
            JsonObject resp = H2.call(Ops.DESCRIBE, session, 0, req).json();
            assertEquals(kind, resp.get("kind").getAsString());
            assertTrue(resp.get("items").isJsonArray(), kind);
        }
        JsonObject req = new JsonObject();
        req.addProperty("kind", "ddl");
        req.addProperty("schema", "APP");
        req.addProperty("table", "NOPE");
        // 'ddl' needs a real table, but the failure must be about the table and
        // never about the kind being unavailable.
        JsonObject err = H2.call(Ops.DESCRIBE, session, 0, req).error();
        assertFalse(err.get("message").getAsString().contains("not implemented"), err.toString());
    }
}
