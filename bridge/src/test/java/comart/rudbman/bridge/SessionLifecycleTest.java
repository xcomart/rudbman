package comart.rudbman.bridge;

import com.google.gson.JsonObject;
import comart.rudbman.bridge.support.H2;
import comart.rudbman.bridge.support.Resp;
import org.junit.jupiter.api.Test;

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertFalse;
import static org.junit.jupiter.api.Assertions.assertNotNull;
import static org.junit.jupiter.api.Assertions.assertTrue;

/** Open, ping, describe, close. */
class SessionLifecycleTest {

    @Test
    void openPingInfoClose() {
        long s = H2.open(H2.freshUrl());

        Resp ping = H2.call(Ops.PING, s, 0, null);
        ping.assertOk();
        assertTrue(ping.json().get("ok").getAsBoolean());
        assertTrue(ping.json().get("elapsed_ms").getAsLong() >= 0);

        JsonObject info = H2.call(Ops.SESSION_INFO, s, 0, null).json();
        assertEquals("H2", info.get("product_name").getAsString());
        assertNotNull(info.get("product_version").getAsString());
        assertNotNull(info.get("driver_name").getAsString());
        assertEquals("\"", info.get("identifier_quote").getAsString());
        assertTrue(info.get("auto_commit").getAsBoolean());
        assertFalse(info.get("read_only").getAsBoolean());
        assertEquals(H2.DRIVER, info.get("driver_class").getAsString());
        // serializeNulls is on: every documented member is present even when the
        // driver had nothing to say, so the Rust structs stay uniform.
        assertTrue(info.has("sql_keywords"));

        H2.close(s);

        // The handle is gone, and using it again is a protocol error, not a crash.
        JsonObject err = H2.call(Ops.PING, s, 0, null).error();
        assertEquals("protocol", err.get("kind").getAsString());
    }

    @Test
    void readOnlyAndAutoCommitAreHonoured() {
        JsonObject req = new JsonObject();
        req.addProperty("url", H2.freshUrl());
        req.addProperty("driver_class", H2.DRIVER);
        req.addProperty("username", "sa");
        req.addProperty("password", "");
        req.addProperty("auto_commit", false);
        long s = H2.call(Ops.OPEN_SESSION, 0, 0, req).num("session");

        JsonObject info = H2.call(Ops.SESSION_INFO, s, 0, null).json();
        assertFalse(info.get("auto_commit").getAsBoolean());

        H2.exec(s, "create table t(id int)");
        H2.exec(s, "insert into t values (1)");
        H2.call(Ops.ROLLBACK, s, 0, null).assertOk();

        Resp q = H2.query(s, "select count(*) from t");
        long cursor = q.json().get("cursor").getAsLong();
        assertEquals(0L, Resp.of(Bridge.call(Ops.FETCH, cursor, 10, null))
                .batch().columns[0].i64[0]);

        H2.exec(s, "insert into t values (2)");
        H2.call(Ops.COMMIT, s, 0, null).assertOk();
        H2.call(Ops.SET_AUTOCOMMIT, s, 1, null).assertOk();
        assertTrue(H2.call(Ops.SESSION_INFO, s, 0, null).json().get("auto_commit").getAsBoolean());

        H2.close(s);
    }

    @Test
    void keepAliveDoesNotBreakTheSession() throws Exception {
        JsonObject ka = new JsonObject();
        ka.addProperty("enabled", true);
        ka.addProperty("interval_s", 1);
        ka.addProperty("query", "select 1");

        JsonObject req = new JsonObject();
        req.addProperty("url", H2.freshUrl());
        req.addProperty("driver_class", H2.DRIVER);
        req.addProperty("username", "sa");
        req.addProperty("password", "");
        req.add("keep_alive", ka);

        long s = H2.call(Ops.OPEN_SESSION, 0, 0, req).num("session");
        Thread.sleep(1500);
        assertTrue(H2.call(Ops.PING, s, 0, null).json().get("ok").getAsBoolean());
        H2.close(s);
    }

    @Test
    void closingASessionDropsItsCursorHandles() {
        int base = Registry.size();
        long s = H2.open(H2.freshUrl());
        H2.exec(s, "create table t(id int)");

        long c1 = H2.query(s, "select * from t").json().get("cursor").getAsLong();
        long c2 = H2.query(s, "select * from t").json().get("cursor").getAsLong();
        assertEquals(base + 3, Registry.size(), "one session plus two cursors");

        // Closing the session takes its cursors with it; leaking them would keep
        // statements and result sets alive for the life of the process.
        H2.close(s);
        assertEquals(base, Registry.size());
        assertFalse(Resp.of(Bridge.call(Ops.FETCH, c1, 10, null)).ok);
        assertFalse(Resp.of(Bridge.call(Ops.FETCH, c2, 10, null)).ok);
    }

    @Test
    void unknownOpIsAProtocolError() {
        JsonObject err = Resp.of(Bridge.call(0x7F, 0, 0, null)).error();
        assertEquals("protocol", err.get("kind").getAsString());
        assertTrue(err.get("message").getAsString().contains("unknown operation"));
    }

    @Test
    void deferredOpsReportThemselvesAsUnimplemented() {
        // The job operations arrived in M4; LOB_READ is what is left.
        for (int op : new int[]{Ops.LOB_READ}) {
            JsonObject err = Resp.of(Bridge.call(op, 0, 0, null)).error();
            assertEquals("protocol", err.get("kind").getAsString());
            assertTrue(err.get("message").getAsString().contains("not implemented"),
                    "op 0x" + Integer.toHexString(op) + ": " + err);
        }
    }
}
