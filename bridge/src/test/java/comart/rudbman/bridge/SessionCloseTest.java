package comart.rudbman.bridge;

import com.google.gson.JsonObject;
import comart.rudbman.bridge.support.H2;
import comart.rudbman.bridge.support.Resp;
import java.lang.reflect.Proxy;
import java.sql.Connection;
import java.sql.SQLFeatureNotSupportedException;
import java.util.ArrayList;
import java.util.List;
import org.junit.jupiter.api.Test;

import static org.junit.jupiter.api.Assertions.assertEquals;

/**
 * Closing a session never leaves an open transaction to the driver's own close,
 * which on Oracle commits it.
 */
class SessionCloseTest {

    /** A connection that records the methods called on it. */
    private static Connection recording(List<String> calls, boolean autoCommit, boolean canAbort) {
        return (Connection) Proxy.newProxyInstance(
                SessionCloseTest.class.getClassLoader(),
                new Class<?>[]{Connection.class},
                (proxy, method, args) -> {
                    calls.add(method.getName());
                    switch (method.getName()) {
                        case "getAutoCommit":
                            return autoCommit;
                        case "abort":
                            if (!canAbort) {
                                throw new SQLFeatureNotSupportedException("no abort");
                            }
                            return null;
                        default:
                            return null;
                    }
                });
    }

    @Test
    void anOpenTransactionIsRolledBackBeforeTheClose() {
        List<String> calls = new ArrayList<>();
        Session.shutDown(recording(calls, false, true), false);
        assertEquals(List.of("getAutoCommit", "rollback", "close"), calls);
    }

    @Test
    void anAutoCommitConnectionIsSimplyClosed() {
        List<String> calls = new ArrayList<>();
        Session.shutDown(recording(calls, true, true), false);
        assertEquals(List.of("getAutoCommit", "close"), calls);
    }

    @Test
    void anUnusableSessionIsAbortedRatherThanClosed() {
        List<String> calls = new ArrayList<>();
        Session.shutDown(recording(calls, false, true), true);
        assertEquals(List.of("abort"), calls);
    }

    @Test
    void aDriverThatCannotAbortIsRolledBackAndClosed() {
        List<String> calls = new ArrayList<>();
        Session.shutDown(recording(calls, false, false), true);
        assertEquals(List.of("abort", "getAutoCommit", "rollback", "close"), calls);
    }

    /** Both paths end a real session and leave its uncommitted row behind. */
    @Test
    void uncommittedRowsDoNotSurviveEitherClose() {
        String url = H2.freshUrl();
        long setup = H2.open(url);
        H2.exec(setup, "create table t(id int)");
        H2.close(setup);

        for (boolean unusable : new boolean[]{false, true}) {
            JsonObject manual = new JsonObject();
            manual.addProperty("auto_commit", false);
            long s = H2.open(url, manual);
            H2.exec(s, "insert into t values (1)");
            if (unusable) {
                Registry.session(s).markUnusable("test");
            }
            H2.close(s);

            long check = H2.open(url);
            Resp q = H2.query(check, "select count(*) from t");
            long cursor = q.json().get("cursor").getAsLong();
            assertEquals(0L, Resp.of(Bridge.call(Ops.FETCH, cursor, 10, null))
                    .batch().columns[0].i64[0], "unusable=" + unusable);
            H2.close(check);
        }
    }
}
