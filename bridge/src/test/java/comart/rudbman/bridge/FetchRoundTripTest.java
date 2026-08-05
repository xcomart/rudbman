package comart.rudbman.bridge;

import com.google.gson.JsonArray;
import com.google.gson.JsonObject;
import comart.rudbman.bridge.support.Batch;
import comart.rudbman.bridge.support.H2;
import comart.rudbman.bridge.support.Resp;
import org.junit.jupiter.api.AfterEach;
import org.junit.jupiter.api.BeforeEach;
import org.junit.jupiter.api.Test;

import java.math.BigDecimal;
import java.nio.charset.StandardCharsets;

import static org.junit.jupiter.api.Assertions.assertArrayEquals;
import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertFalse;
import static org.junit.jupiter.api.Assertions.assertNotEquals;
import static org.junit.jupiter.api.Assertions.assertNull;
import static org.junit.jupiter.api.Assertions.assertTrue;

/**
 * EXECUTE / FETCH round trip through the {@code RDB1} codec.
 *
 * <p>Every assertion here goes through {@link Batch}, a decoder written from the
 * format description rather than from the encoder. That is the only way this
 * suite proves anything about the contract the Rust decoder has to satisfy.
 */
class FetchRoundTripTest {

    /** Column indexes of the fixture table, in declaration order. */
    private static final int RN = 0;
    private static final int I_VAL = 1;
    private static final int BIG_VAL = 2;
    private static final int DBL_VAL = 3;
    private static final int BOOL_VAL = 4;
    private static final int TXT_VAL = 5;
    private static final int DEC_VAL = 6;
    private static final int DATE_VAL = 7;
    private static final int TS_VAL = 8;
    private static final int BIN_VAL = 9;
    private static final int CLOB_VAL = 10;
    private static final int NULL_VAL = 11;

    private static final String UNICODE = "hello ünïcode ✓ ctrl";
    private static final String DECIMAL_TEXT = "123456789012.12345678";

    private long session;

    @BeforeEach
    void setUp() {
        session = H2.open(H2.freshUrl());
        H2.exec(session, "create table t ("
                + "rn integer,"
                + "i_val integer,"
                + "big_val bigint,"
                + "dbl_val double precision,"
                + "bool_val boolean,"
                + "txt_val varchar(200),"
                + "dec_val numeric(20,8),"
                + "date_val date,"
                + "ts_val timestamp,"
                + "bin_val varbinary(64),"
                + "clob_val clob,"
                + "null_val varchar(10))");
        H2.exec(session, "insert into t values ("
                + "1, 42, 9223372036854775807, 3.5, true, '" + UNICODE.replace("'", "''") + "',"
                + DECIMAL_TEXT + ", DATE '2024-03-15', TIMESTAMP '2024-03-15 12:34:56.789',"
                + "X'DEADBEEF', 'clob body', null)");
        H2.exec(session, "insert into t values ("
                + "2, null, null, null, false, '', null, null, null, null, null, null)");
        H2.exec(session, "insert into t values ("
                + "3, -7, -9223372036854775808, -0.25, null, null, -0.00000001,"
                + "DATE '1999-12-31', TIMESTAMP '1999-12-31 23:59:59', X'00',"
                + "repeat('x', 1000), null)");
    }

    @AfterEach
    void tearDown() {
        H2.close(session);
    }

    private Batch fetchAll(String sql, int limit) {
        Resp exec = H2.query(session, sql);
        exec.assertOk();
        long cursor = exec.json().get("cursor").getAsLong();
        Batch b = Resp.of(Bridge.call(Ops.FETCH, cursor, limit, null)).batch();
        Resp.of(Bridge.call(Ops.CLOSE_CURSOR, cursor, 0, null)).assertOk();
        return b;
    }

    @Test
    void executeReportsColumnMetadata() {
        Resp exec = H2.query(session, "select * from t order by rn");
        JsonObject o = exec.json();
        assertTrue(o.get("cursor").getAsLong() != 0);
        assertEquals(-1, o.get("update_count").getAsInt());
        assertTrue(o.get("has_result_set").getAsBoolean());

        JsonArray cols = o.get("columns").getAsJsonArray();
        assertEquals(12, cols.size());
        JsonObject rn = cols.get(RN).getAsJsonObject();
        assertEquals("RN", rn.get("name").getAsString());
        assertEquals("INTEGER", rn.get("jdbc_type").getAsString());
        assertEquals(Batch.I64, rn.get("kind").getAsInt());
        // DECIMAL travels as text; anything else would lose the scale.
        assertEquals(Batch.STR, cols.get(DEC_VAL).getAsJsonObject().get("kind").getAsInt());
        assertEquals(Batch.STR, cols.get(TS_VAL).getAsJsonObject().get("kind").getAsInt());
        assertEquals(Batch.LOB, cols.get(CLOB_VAL).getAsJsonObject().get("kind").getAsInt());
        assertEquals(Batch.BIN, cols.get(BIN_VAL).getAsJsonObject().get("kind").getAsInt());
        assertEquals(Batch.BOOL, cols.get(BOOL_VAL).getAsJsonObject().get("kind").getAsInt());

        Resp.of(Bridge.call(Ops.CLOSE_CURSOR, o.get("cursor").getAsLong(), 0, null)).assertOk();
    }

    @Test
    void integersSurviveExactly() {
        Batch b = fetchAll("select * from t order by rn", 100);
        assertEquals(3, b.rowCount);
        assertTrue(b.last);

        Batch.Column rn = b.columns[RN];
        assertEquals(Batch.I64, rn.kind);
        assertArrayEquals(new long[]{1, 2, 3}, rn.i64);

        Batch.Column big = b.columns[BIG_VAL];
        assertEquals(Batch.I64, big.kind);
        // The two values a double would silently round.
        assertEquals(Long.MAX_VALUE, big.i64[0]);
        assertEquals(Long.MIN_VALUE, big.i64[2]);
        assertFalse(big.valid[1]);

        Batch.Column i = b.columns[I_VAL];
        assertEquals(42L, i.i64[0]);
        assertEquals(-7L, i.i64[2]);
    }

    @Test
    void doublesAndBooleansSurvive() {
        Batch b = fetchAll("select * from t order by rn", 100);

        Batch.Column d = b.columns[DBL_VAL];
        assertEquals(Batch.F64, d.kind);
        assertEquals(3.5d, d.f64[0]);
        assertEquals(-0.25d, d.f64[2]);
        assertFalse(d.valid[1]);

        Batch.Column flag = b.columns[BOOL_VAL];
        assertEquals(Batch.BOOL, flag.kind);
        assertTrue(flag.valid[0]);
        assertTrue(flag.bools[0]);
        assertTrue(flag.valid[1]);
        assertFalse(flag.bools[1]);
        assertFalse(flag.valid[2]);
    }

    @Test
    void decimalKeepsEveryDigit() {
        Batch b = fetchAll("select * from t order by rn", 100);
        Batch.Column dec = b.columns[DEC_VAL];
        assertEquals(Batch.STR, dec.kind);
        assertEquals(DECIMAL_TEXT, dec.str(0));
        // Exact, not approximately: a double round trip of this value differs in
        // the last digits, which is the whole reason for the text encoding.
        assertEquals(0, new BigDecimal(DECIMAL_TEXT).compareTo(new BigDecimal(dec.str(0))));
        assertNotEquals(DECIMAL_TEXT, Double.toString(Double.parseDouble(DECIMAL_TEXT)));
        assertNull(dec.str(1));
        assertEquals("-0.00000001", dec.str(2));
    }

    @Test
    void temporalValuesArriveAsDriverText() {
        Batch b = fetchAll("select * from t order by rn", 100);

        Batch.Column date = b.columns[DATE_VAL];
        assertEquals(Batch.STR, date.kind);
        assertEquals("2024-03-15", date.str(0));
        assertNull(date.str(1));
        assertEquals("1999-12-31", date.str(2));

        Batch.Column ts = b.columns[TS_VAL];
        assertEquals(Batch.STR, ts.kind);
        assertTrue(ts.str(0).startsWith("2024-03-15 12:34:56"), ts.str(0));
        assertTrue(ts.str(0).contains(".789"), ts.str(0));
    }

    @Test
    void textIsUtf8AndEmptyIsNotNull() {
        Batch b = fetchAll("select * from t order by rn", 100);
        Batch.Column txt = b.columns[TXT_VAL];
        assertEquals(Batch.STR, txt.kind);
        assertEquals(UNICODE, txt.str(0));
        assertArrayEquals(UNICODE.getBytes(StandardCharsets.UTF_8), txt.bin(0));

        // The empty string and NULL both produce a zero-length slice; only the
        // validity bitmap tells them apart. Tools that get this wrong are
        // exactly why the grid needs to distinguish them.
        assertTrue(txt.valid[1]);
        assertEquals("", txt.str(1));
        assertFalse(txt.valid[2]);
        assertNull(txt.str(2));
    }

    @Test
    void binaryIsRaw() {
        Batch b = fetchAll("select * from t order by rn", 100);
        Batch.Column bin = b.columns[BIN_VAL];
        assertEquals(Batch.BIN, bin.kind);
        assertArrayEquals(new byte[]{(byte) 0xDE, (byte) 0xAD, (byte) 0xBE, (byte) 0xEF}, bin.bin(0));
        assertNull(bin.bin(1));
        assertArrayEquals(new byte[]{0}, bin.bin(2));
    }

    @Test
    void lobsCarryOnlyAHandleAndASize() {
        Batch b = fetchAll("select * from t order by rn", 100);
        Batch.Column lob = b.columns[CLOB_VAL];
        assertEquals(Batch.LOB, lob.kind);

        assertTrue(lob.valid[0]);
        assertEquals("clob body".length(), lob.lobSizes[0]);
        assertTrue(lob.lobIds[0] != 0);

        assertFalse(lob.valid[1]);

        assertTrue(lob.valid[2]);
        assertEquals(1000, lob.lobSizes[2]);
        assertNotEquals(lob.lobIds[0], lob.lobIds[2]);

        // A 1000-character CLOB contributes 16 bytes to the batch, not 1000.
        // The whole column is 3 rows x 16 bytes plus a one-byte bitmap.
        assertTrue(b.columns[CLOB_VAL].lobIds.length == 3);
    }

    @Test
    void validityBitmapCoversAllNullAndNoNullColumns() {
        Batch b = fetchAll("select * from t order by rn", 100);

        // Never null: every bit set, and the values are really there.
        for (boolean v : b.columns[RN].valid) {
            assertTrue(v);
        }

        // Always null: degenerates to kind NULLS with no value area at all.
        Batch.Column nulls = b.columns[NULL_VAL];
        assertEquals(Batch.NULLS, nulls.kind);
        for (boolean v : nulls.valid) {
            assertFalse(v);
        }

        // Mixed: exactly the rows that had values.
        assertArrayEquals(new boolean[]{true, false, true}, b.columns[I_VAL].valid);
        assertArrayEquals(new boolean[]{true, true, false}, b.columns[BOOL_VAL].valid);
    }

    @Test
    void bitmapHandlesMoreThanEightRows() {
        H2.exec(session, "create table wide as select x as n, "
                + "case when mod(x, 3) = 0 then null else x end as maybe "
                + "from system_range(1, 20)");
        Batch b = fetchAll("select n, maybe from wide order by n", 100);
        assertEquals(20, b.rowCount);
        for (int r = 0; r < 20; r++) {
            long n = b.columns[0].i64[r];
            assertEquals(r + 1L, n);
            assertEquals(n % 3 != 0, b.columns[1].valid[r], "row " + r);
            if (n % 3 != 0) {
                assertEquals(n, b.columns[1].i64[r]);
            }
        }
    }

    @Test
    void batchBoundariesAndTheLastFlag() {
        Resp exec = H2.query(session, "select rn from t order by rn");
        long cursor = exec.json().get("cursor").getAsLong();

        Batch b1 = Resp.of(Bridge.call(Ops.FETCH, cursor, 2, null)).batch();
        assertEquals(2, b1.rowCount);
        assertFalse(b1.last, "a full batch cannot know it is the last one");

        Batch b2 = Resp.of(Bridge.call(Ops.FETCH, cursor, 2, null)).batch();
        assertEquals(1, b2.rowCount);
        assertTrue(b2.last, "a short batch means the result set ran out");

        Batch b3 = Resp.of(Bridge.call(Ops.FETCH, cursor, 2, null)).batch();
        assertEquals(0, b3.rowCount);
        assertTrue(b3.last);
        assertEquals(1, b3.colCount);
        assertEquals(Batch.NULLS, b3.columns[0].kind);

        Resp.of(Bridge.call(Ops.CLOSE_CURSOR, cursor, 0, null)).assertOk();
    }

    @Test
    void emptyResultSetIsAWellFormedTerminalBatch() {
        Batch b = fetchAll("select * from t where 1 = 0", 100);
        assertEquals(0, b.rowCount);
        assertEquals(12, b.colCount);
        assertTrue(b.last);
        for (Batch.Column c : b.columns) {
            assertEquals(Batch.NULLS, c.kind);
            assertEquals(0, c.valid.length);
        }
    }

    @Test
    void updateStatementsStillYieldACursor() {
        Resp exec = H2.query(session, "update t set i_val = 0 where rn = 2");
        JsonObject o = exec.json();
        long cursor = o.get("cursor").getAsLong();
        assertTrue(cursor != 0, "a cursor is needed so MORE_RESULTS has something to advance");
        assertEquals(1, o.get("update_count").getAsInt());
        assertFalse(o.get("has_result_set").getAsBoolean());

        // Fetching from an update is not an error; it yields nothing.
        Batch b = Resp.of(Bridge.call(Ops.FETCH, cursor, 10, null)).batch();
        assertEquals(0, b.rowCount);
        assertEquals(0, b.colCount);
        assertTrue(b.last);

        JsonObject more = Resp.of(Bridge.call(Ops.MORE_RESULTS, cursor, 0, null)).json();
        assertFalse(more.get("has_more").getAsBoolean());
        assertEquals(-1, more.get("update_count").getAsInt());

        Resp.of(Bridge.call(Ops.CLOSE_CURSOR, cursor, 0, null)).assertOk();
    }

    @Test
    void fetchWithoutARowLimitUsesTheDefaultBatchSize() {
        H2.exec(session, "create table big as select x as n from system_range(1, 1200)");
        Resp exec = H2.query(session, "select n from big order by n");
        long cursor = exec.json().get("cursor").getAsLong();

        Batch b = Resp.of(Bridge.call(Ops.FETCH, cursor, 0, null)).batch();
        assertEquals(Cursor.DEFAULT_FETCH, b.rowCount);
        assertFalse(b.last);

        Resp.of(Bridge.call(Ops.CLOSE_CURSOR, cursor, 0, null)).assertOk();
    }

    @Test
    void boundParametersKeepDecimalPrecision() {
        JsonObject dec = new JsonObject();
        dec.addProperty("type", "decimal");
        dec.addProperty("value", "99999999999.99999999");

        JsonArray params = new JsonArray();
        params.add(4);
        params.add(dec);

        JsonObject req = new JsonObject();
        req.addProperty("sql", "insert into t (rn, dec_val) values (?, ?)");
        req.add("params", params);
        Resp r = H2.call(Ops.EXECUTE, session, 0, req);
        r.assertOk();
        Resp.of(Bridge.call(Ops.CLOSE_CURSOR, r.json().get("cursor").getAsLong(), 0, null))
                .assertOk();

        Batch b = fetchAll("select dec_val from t where rn = 4", 10);
        assertEquals("99999999999.99999999", b.columns[0].str(0));
    }

    @Test
    void multipleResultsAreWalkedWithMoreResults() {
        Resp exec = H2.query(session,
                "select rn from t order by rn; select count(*) from t");
        JsonObject first = exec.json();
        long cursor = first.get("cursor").getAsLong();

        // H2 executes only the first statement of a batch through execute(),
        // so this asserts the walk terminates cleanly rather than that H2
        // produces two results.
        Batch b = Resp.of(Bridge.call(Ops.FETCH, cursor, 100, null)).batch();
        assertTrue(b.rowCount > 0);

        JsonObject more = Resp.of(Bridge.call(Ops.MORE_RESULTS, cursor, 0, null)).json();
        assertTrue(more.has("has_more"));
        Resp.of(Bridge.call(Ops.CLOSE_CURSOR, cursor, 0, null)).assertOk();
    }
}
