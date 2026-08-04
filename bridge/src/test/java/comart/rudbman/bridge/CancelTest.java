package comart.rudbman.bridge;

import com.google.gson.JsonObject;
import comart.rudbman.bridge.support.H2;
import comart.rudbman.bridge.support.Resp;
import org.junit.jupiter.api.Test;

import java.util.concurrent.CountDownLatch;
import java.util.concurrent.TimeUnit;
import java.util.concurrent.atomic.AtomicReference;

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertFalse;
import static org.junit.jupiter.api.Assertions.assertNotNull;
import static org.junit.jupiter.api.Assertions.assertTrue;

/**
 * CANCEL while a statement is running.
 *
 * <p>This is the one operation that arrives on a thread other than the session
 * worker, and the one that must not take the connection lock: the worker is
 * holding it inside the very statement being cancelled.
 */
class CancelTest {

    /**
     * A cross join of two ranges. H2 checks its cancellation flag between rows,
     * and 4x10^10 iterations leave a wide enough window that the test is not
     * racing the query.
     */
    private static final String LONG_QUERY =
            "select count(*) from system_range(1, 200000) a, system_range(1, 200000) b "
                    + "where a.x <> b.x";

    @Test
    void cancelInterruptsARunningStatement() throws Exception {
        long session = H2.open(H2.freshUrl());
        CountDownLatch started = new CountDownLatch(1);
        CountDownLatch done = new CountDownLatch(1);
        AtomicReference<Resp> result = new AtomicReference<>();

        Thread worker = new Thread(() -> {
            started.countDown();
            result.set(H2.query(session, LONG_QUERY));
            done.countDown();
        }, "rudbman-test-worker");
        worker.setDaemon(true);
        worker.start();

        assertTrue(started.await(5, TimeUnit.SECONDS));

        // The statement has to reach the driver before cancel can bite, and
        // there is no way to observe that from outside, so keep asking.
        boolean finished = false;
        for (int i = 0; i < 150 && !finished; i++) {
            Thread.sleep(100);
            H2.call(Ops.CANCEL, session, 0, null).assertOk();
            finished = done.await(100, TimeUnit.MILLISECONDS);
        }
        assertTrue(finished, "the cancelled statement never returned");

        Resp r = result.get();
        assertNotNull(r);
        assertFalse(r.ok, "a cancelled statement must come back as an error envelope");
        JsonObject err = r.error();
        assertEquals("sql", err.get("kind").getAsString());
        assertNotNull(err.get("sql_state"));
        assertFalse(err.get("sql_state").isJsonNull(),
                "a cancelled statement still carries a SQLSTATE: " + err);

        H2.close(session);
    }

    @Test
    void cancelOnAnIdleSessionIsHarmless() {
        long session = H2.open(H2.freshUrl());
        JsonObject o = H2.call(Ops.CANCEL, session, 0, null).json();
        assertEquals(0, o.get("cancelled").getAsInt());
        assertTrue(H2.call(Ops.PING, session, 0, null).json().get("ok").getAsBoolean());
        H2.close(session);
    }
}
