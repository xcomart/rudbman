package comart.rudbman.bridge;

import com.google.gson.JsonArray;
import com.google.gson.JsonObject;
import comart.rudbman.bridge.support.Batch;
import comart.rudbman.bridge.support.H2;
import comart.rudbman.bridge.support.Resp;
import org.junit.jupiter.api.AfterEach;
import org.junit.jupiter.api.BeforeEach;
import org.junit.jupiter.api.Test;

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertFalse;
import static org.junit.jupiter.api.Assertions.assertTrue;

/**
 * {@code JOB_START kind: "transfer"} end to end (architecture.md 6).
 *
 * <p>The point of a transfer is that no row crosses the JNI boundary, so the
 * assertions read the target back through its own session: what is in the target
 * table is the only evidence that anything happened. The rest pins the contract
 * the Rust side codes against - which failures are synchronous, what
 * {@code rows_skipped} counts, and that closing either session ends the job
 * instead of blocking on a lock.
 */
class TransferJobTest {

    private long source;
    private long target;

    @BeforeEach
    void setUp() {
        source = H2.open(H2.freshUrl());
        target = H2.open(H2.freshUrl());
    }

    @AfterEach
    void tearDown() {
        closeQuietly(source);
        closeQuietly(target);
    }

    // ------------------------------------------------------------------ modes

    @Test
    void rowsCrossFromOneSessionToAnother() throws Exception {
        H2.exec(source, "create table src (id int, note varchar(50), amount decimal(12,4))");
        H2.exec(source, "insert into src values (1, 'it''s fine', 1.5), "
                + "(2, '한글 テスト ☃', -2.25), (3, null, null)");
        H2.exec(target, "create table dst (id int, note varchar(50), amount decimal(12,4))");

        JsonObject done = await(start(spec("select * from src", "DST", "insert", null)));
        assertEquals("done", done.get("state").getAsString(), done.toString());
        assertEquals(3, done.get("rows_done").getAsLong(), done.toString());
        assertEquals(0, done.get("rows_skipped").getAsLong(), done.toString());
        // No file is written, so the byte counter stays where it started.
        assertEquals(0, done.get("bytes").getAsLong(), done.toString());
        assertEquals("done", done.get("phase").getAsString(), done.toString());
        assertTrue(done.get("errors").getAsJsonArray().isEmpty(), done.toString());

        assertEquals("3", one(target, "select count(*) from dst"));
        assertEquals("it's fine", one(target, "select note from dst where id = 1"));
        assertEquals("한글 テスト ☃", one(target, "select note from dst where id = 2"));
        assertEquals("-2.2500", one(target, "select amount from dst where id = 2"));
        assertEquals(null, one(target, "select note from dst where id = 3"));
    }

    /** The reentrant-lock case: a session transferring into itself. */
    @Test
    void aTransferIntoItsOwnSessionIsSafe() throws Exception {
        H2.exec(source, "create table src (id int, v varchar(20))");
        H2.exec(source, "insert into src values (1, 'a'), (2, 'b')");
        H2.exec(source, "create table dst (id int, v varchar(20))");

        JsonObject spec = spec("select * from src", "DST", "insert", null);
        spec.addProperty("target_session", source);
        JsonObject done = await(start(spec));
        assertEquals("done", done.get("state").getAsString(), done.toString());
        assertEquals(2, done.get("rows_done").getAsLong());
        assertEquals("2", one(source, "select count(*) from dst"));
        // And the session is still usable, which it would not be if the two
        // lock acquisitions had not been matched by two releases.
        assertEquals("2", one(source, "select count(*) from src"));
    }

    @Test
    void truncateInsertReplacesWhatWasThere() throws Exception {
        H2.exec(source, "create table src (id int, v varchar(20))");
        H2.exec(source, "insert into src values (1, 'new')");
        H2.exec(target, "create table dst (id int, v varchar(20))");
        H2.exec(target, "insert into dst values (9, 'old'), (8, 'older')");

        JsonObject done = await(start(spec("select * from src", "DST", "truncate_insert", null)));
        assertEquals("done", done.get("state").getAsString(), done.toString());
        assertEquals("1", one(target, "select count(*) from dst"));
        assertEquals("new", one(target, "select v from dst where id = 1"));
    }

    @Test
    void upsertUpdatesWhatIsThereAndInsertsWhatIsNot() throws Exception {
        H2.exec(source, "create table src (id int, v varchar(20))");
        H2.exec(source, "insert into src values (1, 'updated'), (2, 'inserted')");
        H2.exec(target, "create table dst (id int not null primary key, v varchar(20))");
        H2.exec(target, "insert into dst values (1, 'stale')");

        JsonObject done = await(start(spec("select * from src", "DST", "upsert", null)));
        assertEquals("done", done.get("state").getAsString(), done.toString());
        assertEquals(2, done.get("rows_done").getAsLong(), done.toString());
        assertEquals("2", one(target, "select count(*) from dst"));
        assertEquals("updated", one(target, "select v from dst where id = 1"));
        assertEquals("inserted", one(target, "select v from dst where id = 2"));
    }

    @Test
    void columnMapReordersAndRenames() throws Exception {
        H2.exec(source, "create table src (a int, b varchar(20))");
        H2.exec(source, "insert into src values (7, 'seven')");
        H2.exec(target, "create table dst (v varchar(20), id int)");

        JsonArray map = new JsonArray();
        map.add(pair("B", "V"));
        map.add(pair("A", "ID"));
        JsonObject done = await(start(spec("select * from src", "DST", "insert", map)));
        assertEquals("done", done.get("state").getAsString(), done.toString());
        assertEquals("seven", one(target, "select v from dst where id = 7"));
    }

    /**
     * A {@code from} that names no source column can only be found once the
     * query has run, so it is the one specification error reported as a failed
     * job rather than as a rejected call.
     */
    @Test
    void aColumnMapNamingAnAbsentColumnFailsTheJob() throws Exception {
        H2.exec(source, "create table src (a int)");
        H2.exec(target, "create table dst (a int)");

        JsonArray map = new JsonArray();
        map.add(pair("NOPE", "A"));
        JsonObject end = await(start(spec("select * from src", "DST", "insert", map)));
        assertEquals("failed", end.get("state").getAsString(), end.toString());
        assertTrue(end.get("errors").getAsJsonArray().get(0).getAsJsonObject()
                .get("message").getAsString().contains("NOPE"), end.toString());
    }

    // --------------------------------------------------------------- on_error

    @Test
    void skipDropsTheOffendingRowsAndKeepsTheRest() throws Exception {
        H2.exec(source, "create table src (id int, v varchar(20))");
        H2.exec(source, "insert into src select x, 'row ' || x from system_range(1, 10)");
        H2.exec(target, "create table dst (id int not null primary key, v varchar(20))");
        H2.exec(target, "insert into dst values (3, 'taken'), (7, 'taken')");

        JsonObject spec = spec("select * from src", "DST", "insert", null);
        spec.addProperty("on_error", "skip");
        JsonObject done = await(start(spec));
        assertEquals("done", done.get("state").getAsString(), done.toString());
        assertEquals(8, done.get("rows_done").getAsLong(), done.toString());
        assertEquals(2, done.get("rows_skipped").getAsLong(), done.toString());
        // "skip" counts without recording; that is the whole difference from "log".
        assertTrue(done.get("errors").getAsJsonArray().isEmpty(), done.toString());

        assertEquals("10", one(target, "select count(*) from dst"));
        // The rows that were already there were not overwritten, and the eight
        // that could go in did.
        assertEquals("taken", one(target, "select v from dst where id = 3"));
        assertEquals("row 1", one(target, "select v from dst where id = 1"));
        assertEquals("row 10", one(target, "select v from dst where id = 10"));
    }

    @Test
    void logRecordsWhatSkipOnlyCounts() throws Exception {
        H2.exec(source, "create table src (id int, v varchar(20))");
        H2.exec(source, "insert into src values (1, 'a'), (2, 'b')");
        H2.exec(target, "create table dst (id int not null primary key, v varchar(20))");
        H2.exec(target, "insert into dst values (1, 'taken')");

        JsonObject spec = spec("select * from src", "DST", "insert", null);
        spec.addProperty("on_error", "log");
        JsonObject done = await(start(spec));
        assertEquals("done", done.get("state").getAsString(), done.toString());
        assertEquals(1, done.get("rows_done").getAsLong(), done.toString());
        assertEquals(1, done.get("rows_skipped").getAsLong(), done.toString());
        JsonArray errors = done.get("errors").getAsJsonArray();
        assertEquals(1, errors.size(), done.toString());
        assertEquals("sql", errors.get(0).getAsJsonObject().get("kind").getAsString(),
                done.toString());
    }

    @Test
    void abortIsTheDefaultAndFailsTheJob() throws Exception {
        H2.exec(source, "create table src (id int, v varchar(20))");
        H2.exec(source, "insert into src values (1, 'a')");
        H2.exec(target, "create table dst (id int not null primary key, v varchar(20))");
        H2.exec(target, "insert into dst values (1, 'taken')");

        JsonObject end = await(start(spec("select * from src", "DST", "insert", null)));
        assertEquals("failed", end.get("state").getAsString(), end.toString());
        assertEquals(0, end.get("rows_done").getAsLong(), end.toString());
        assertEquals("1", one(target, "select count(*) from dst"));
    }

    // -------------------------------------------------------- protocol errors

    @Test
    void anUnknownModeIsRejectedBeforeAnyThreadStarts() {
        JsonObject spec = spec("select 1", "DST", "merge_all", null);
        JsonObject err = H2.call(Ops.JOB_START, source, 0, spec).error();
        assertEquals("protocol", err.get("kind").getAsString());
        assertTrue(err.get("message").getAsString().contains("mode must be"), err.toString());
    }

    @Test
    void anUnknownOnErrorPolicyIsRejected() {
        JsonObject spec = spec("select 1", "DST", "insert", null);
        spec.addProperty("on_error", "shrug");
        assertEquals("protocol",
                H2.call(Ops.JOB_START, source, 0, spec).error().get("kind").getAsString());
    }

    @Test
    void aMissingTargetSessionIsRejected() {
        JsonObject spec = spec("select 1", "DST", "insert", null);
        spec.remove("target_session");
        assertEquals("protocol",
                H2.call(Ops.JOB_START, source, 0, spec).error().get("kind").getAsString());
    }

    @Test
    void anUnknownTargetSessionHandleIsRejected() {
        JsonObject spec = spec("select 1", "DST", "insert", null);
        spec.addProperty("target_session", 999_999L);
        JsonObject err = H2.call(Ops.JOB_START, source, 0, spec).error();
        assertEquals("protocol", err.get("kind").getAsString());
        assertTrue(err.get("message").getAsString().contains("session"), err.toString());
    }

    @Test
    void upsertWithoutAPrimaryKeyOnTheTargetIsRejected() {
        H2.exec(target, "create table dst (id int, v varchar(20))");
        JsonObject spec = spec("select 1", "DST", "upsert", null);
        JsonObject err = H2.call(Ops.JOB_START, source, 0, spec).error();
        assertEquals("protocol", err.get("kind").getAsString());
        assertTrue(err.get("message").getAsString().contains("primary key"), err.toString());
    }

    @Test
    void aMalformedColumnMapEntryIsRejected() {
        JsonObject entry = new JsonObject();
        entry.addProperty("from", "A");
        JsonArray map = new JsonArray();
        map.add(entry);
        assertEquals("protocol",
                H2.call(Ops.JOB_START, source, 0, spec("select 1", "DST", "insert", map))
                        .error().get("kind").getAsString());
    }

    // -------------------------------------------------- cancel, session close

    @Test
    void cancellingMidFlightLeavesBothSessionsUsable() throws Exception {
        generateBigSource();
        H2.exec(target, "create table dst (id int, v varchar(80))");

        JsonObject spec = spec("select * from src", "DST", "insert", null);
        spec.addProperty("batch_size", 100);
        spec.addProperty("commit_every", 500);
        long job = start(spec);

        long seen = awaitRows(job, 1);
        assertTrue(seen > 0);
        assertTrue(H2.call(Ops.JOB_CANCEL, job, 0, null).json().get("cancelled").getAsBoolean());

        JsonObject end = await(job);
        assertEquals("cancelled", end.get("state").getAsString(), end.toString());
        assertTrue(end.get("rows_done").getAsLong() < BIG_ROWS,
                "a cancel that only arrived after the last row proves nothing: " + end);

        // Both connections survived having a statement cancelled underneath them.
        assertEquals(String.valueOf(BIG_ROWS), one(source, "select count(*) from src"));
        long left = Long.parseLong(one(target, "select count(*) from dst"));
        // Rows committed before the cancel stay: that is what rows_done reports,
        // and the uncommitted tail is what got rolled back.
        assertTrue(left < BIG_ROWS, "expected a partial transfer, got " + left);
        assertTrue(left <= end.get("rows_done").getAsLong(),
                "committed rows cannot exceed the rows the job counted: " + left + " / " + end);
    }

    /**
     * The half of the transaction rule that is easy to get backwards: a cancel
     * rolls back the uncommitted tail, and everything {@code commit_every}
     * already committed stays.
     */
    @Test
    void aCancelKeepsTheCommittedPrefix() throws Exception {
        generateBigSource();
        H2.exec(target, "create table dst (id int, v varchar(80))");

        JsonObject spec = spec("select * from src", "DST", "insert", null);
        spec.addProperty("batch_size", 100);
        spec.addProperty("commit_every", 100);
        long job = start(spec);

        // Far enough in that several commits have certainly happened, so that a
        // count of zero would mean the prefix was lost rather than never made.
        awaitRows(job, 5_000);
        H2.call(Ops.JOB_CANCEL, job, 0, null);
        JsonObject end = await(job);
        assertEquals("cancelled", end.get("state").getAsString(), end.toString());

        long left = Long.parseLong(one(target, "select count(*) from dst"));
        assertTrue(left >= 4_000, "the committed prefix should have survived, got " + left);
        assertTrue(left < BIG_ROWS, "the transfer should not have finished, got " + left);
    }

    /**
     * The regression this exists for: a transfer is registered against its source
     * session but holds the target's connection lock for its whole run, so
     * closing the <em>target</em> has to cancel it. Without that, this test hangs
     * inside {@code CLOSE_SESSION}.
     */
    @Test
    void closingTheTargetSessionCancelsTheTransfer() throws Exception {
        generateBigSource();
        H2.exec(target, "create table dst (id int, v varchar(80))");

        JsonObject spec = spec("select * from src", "DST", "insert", null);
        spec.addProperty("batch_size", 100);
        spec.addProperty("commit_every", 500);
        long job = start(spec);
        awaitRows(job, 1);

        H2.close(target);
        target = 0;

        // The handle went with the session, exactly as it does for the source.
        assertFalse(H2.call(Ops.JOB_POLL, job, 0, null).ok);
        // And the source session is still there and still usable.
        assertEquals(String.valueOf(BIG_ROWS), one(source, "select count(*) from src"));
    }

    @Test
    void closingTheSourceSessionCancelsTheTransfer() throws Exception {
        generateBigSource();
        H2.exec(target, "create table dst (id int, v varchar(80))");

        JsonObject spec = spec("select * from src", "DST", "insert", null);
        spec.addProperty("batch_size", 100);
        long job = start(spec);
        awaitRows(job, 1);

        H2.close(source);
        source = 0;
        assertFalse(H2.call(Ops.JOB_POLL, job, 0, null).ok);
        assertEquals("0", one(target, "select count(*) from dst where id < 0"));
    }

    // --------------------------------------------------------------- helpers

    private static final int BIG_ROWS = 200_000;

    private void generateBigSource() {
        H2.exec(source, "create table src (id int, v varchar(80))");
        H2.exec(source, "insert into src select x, 'row number ' || x"
                + " from system_range(1, " + BIG_ROWS + ")");
    }

    private JsonObject spec(String sql, String targetTable, String mode, JsonArray columnMap) {
        JsonObject spec = new JsonObject();
        spec.addProperty("kind", "transfer");
        spec.addProperty("source_sql", sql);
        spec.addProperty("target_session", target);
        JsonObject t = new JsonObject();
        t.addProperty("schema", "PUBLIC");
        t.addProperty("name", targetTable);
        spec.add("target_table", t);
        spec.addProperty("mode", mode);
        if (columnMap != null) {
            spec.add("column_map", columnMap);
        }
        return spec;
    }

    private static JsonObject pair(String from, String to) {
        JsonObject o = new JsonObject();
        o.addProperty("from", from);
        o.addProperty("to", to);
        return o;
    }

    private long start(JsonObject spec) {
        Resp r = H2.call(Ops.JOB_START, source, 0, spec);
        r.assertOk();
        return r.json().get("job").getAsLong();
    }

    /** Polls until the job leaves {@code running} and returns that one answer. */
    private JsonObject await(long job) throws Exception {
        for (int i = 0; i < 1200; i++) {
            JsonObject p = H2.call(Ops.JOB_POLL, job, 0, null).json();
            if (!"running".equals(p.get("state").getAsString())) {
                return p;
            }
            Thread.sleep(25);
        }
        throw new AssertionError("job " + job + " never finished");
    }

    /** @return the first {@code rows_done} at or above {@code atLeast} */
    private long awaitRows(long job, long atLeast) throws Exception {
        for (int i = 0; i < 1200; i++) {
            JsonObject p = H2.call(Ops.JOB_POLL, job, 0, null).json();
            long rows = p.get("rows_done").getAsLong();
            if (rows >= atLeast) {
                return rows;
            }
            if (!"running".equals(p.get("state").getAsString())) {
                throw new AssertionError("job finished before producing rows: " + p);
            }
            Thread.sleep(2);
        }
        throw new AssertionError("job " + job + " produced no rows");
    }

    private static void closeQuietly(long session) {
        if (session != 0) {
            H2.close(session);
        }
    }

    /**
     * @return the first column of the first row rendered as text, or {@code null}
     *         when it is NULL or the query returned nothing
     */
    private static String one(long session, String sql) {
        long cursor = H2.query(session, sql).json().get("cursor").getAsLong();
        try {
            Batch batch = Resp.of(Bridge.call(Ops.FETCH, cursor, 1, null)).batch();
            if (batch.rowCount == 0 || batch.colCount == 0) {
                return null;
            }
            Batch.Column col = batch.columns[0];
            if (!col.valid[0]) {
                return null;
            }
            switch (col.kind) {
                case Batch.I64:  return Long.toString(col.i64[0]);
                case Batch.F64:  return Double.toString(col.f64[0]);
                case Batch.BOOL: return Boolean.toString(col.bools[0]);
                default:         return col.str(0);
            }
        } finally {
            Resp.of(Bridge.call(Ops.CLOSE_CURSOR, cursor, 0, null)).assertOk();
        }
    }
}
