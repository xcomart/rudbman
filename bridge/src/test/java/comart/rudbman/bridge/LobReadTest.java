package comart.rudbman.bridge;

import com.google.gson.JsonObject;
import comart.rudbman.bridge.support.Batch;
import comart.rudbman.bridge.support.H2;
import comart.rudbman.bridge.support.Resp;
import comart.rudbman.bridge.codec.ColumnKind;
import comart.rudbman.bridge.codec.ColumnWriters;
import org.junit.jupiter.api.Test;

import java.io.ByteArrayInputStream;
import java.io.StringReader;
import java.lang.reflect.Proxy;
import java.nio.charset.StandardCharsets;
import java.sql.ResultSet;
import java.sql.SQLFeatureNotSupportedException;
import java.sql.Types;
import java.util.Arrays;

import static org.junit.jupiter.api.Assertions.*;

class LobReadTest {
    @Test
    void longTypesAndDriversWithoutLobWrappersUseStreams() throws Exception {
        for (int type : new int[]{Types.LONGVARBINARY, Types.LONGVARCHAR,
                Types.LONGNVARCHAR, Types.BLOB, Types.CLOB, Types.NCLOB}) {
            assertEquals(ColumnKind.LOB, ColumnKind.forSqlType(type, 0));
            boolean binary = type == Types.BLOB || type == Types.LONGVARBINARY;
            byte[] expected = binary ? new byte[]{0, (byte) 255, 1}
                    : "한🙂\0끝".getBytes(StandardCharsets.UTF_8);
            ResultSet row = (ResultSet) Proxy.newProxyInstance(getClass().getClassLoader(),
                    new Class<?>[]{ResultSet.class}, (proxy, method, args) -> {
                        switch (method.getName()) {
                            case "getBlob":
                            case "getClob":
                                assertTrue(type == Types.BLOB || type == Types.CLOB || type == Types.NCLOB,
                                        "LONG types must use stream getters directly");
                                throw new SQLFeatureNotSupportedException();
                            case "getBinaryStream": return new ByteArrayInputStream(expected);
                            case "getCharacterStream": return new StringReader("한🙂\0끝");
                            case "wasNull": return false;
                            default: throw new AssertionError(method.getName());
                        }
                    });
            LobStore store = new LobStore();
            long[] id = {0};
            try {
                ColumnWriters.forColumn(type, 0, 1, (r, c, size, isBinary, value) -> {
                    assertEquals(binary, isBinary);
                    return id[0] = store.registerLob(r, c, size, isBinary, value);
                }).read(row, 1, 0);
                JsonObject request = new JsonObject();
                request.addProperty("id", id[0]);
                request.addProperty("offset", 0);
                request.addProperty("length", 100);
                assertArrayEquals(expected, store.read(request));
            } finally {
                store.clear();
            }
        }
    }

    private static Resp read(long cursor, long id, long offset, int length) {
        JsonObject request = new JsonObject();
        request.addProperty("id", id);
        request.addProperty("offset", offset);
        request.addProperty("length", length);
        return Resp.of(Bridge.call(Ops.LOB_READ, cursor, 0,
                request.toString().getBytes(StandardCharsets.UTF_8)));
    }

    @Test
    void snapshotsSurviveRowAdvanceAndReadsAreBounded() {
        long session = H2.open(H2.freshUrl());
        try {
            H2.exec(session, "create table lobs (id int, b blob, c clob)");
            H2.exec(session, "insert into lobs values (1, X'00FF0102', '한🙂끝'), (2, X'', ''), (3, null, null)");
            long cursor = H2.query(session, "select b, c from lobs order by id").num("cursor");
            Batch first = Resp.of(Bridge.call(Ops.FETCH, cursor, 1, null)).batch();
            long binary = first.columns[0].lobIds[0];
            long text = first.columns[1].lobIds[0];
            Batch rest = Resp.of(Bridge.call(Ops.FETCH, cursor, 10, null)).batch();
            assertArrayEquals(new byte[]{0, (byte) 255, 1, 2}, read(cursor, binary, 0, 100).body);
            byte[] utf8 = "한🙂끝".getBytes(StandardCharsets.UTF_8);
            assertArrayEquals(Arrays.copyOfRange(utf8, 2, 6), read(cursor, text, 2, 4).body);
            assertEquals(0, read(cursor, text, utf8.length, 100).body.length);
            assertEquals(0, read(cursor, rest.columns[0].lobIds[0], 0, 100).body.length);
            assertEquals(0, read(cursor, rest.columns[1].lobIds[0], 0, 100).body.length);
            assertFalse(rest.columns[0].valid[1]);
            assertFalse(rest.columns[1].valid[1]);
            assertFalse(read(cursor, text, -1, 10).ok);
            assertFalse(read(cursor, text, 1000, 10).ok);
            assertFalse(read(cursor, text, 0, 0).ok);
            assertFalse(read(cursor, text, 0, LobStore.MAX_CHUNK + 1).ok);
            assertFalse(read(cursor, Long.MAX_VALUE, 0, 1).ok);
            Resp.of(Bridge.call(Ops.MORE_RESULTS, cursor, 0, null)).assertOk();
            assertFalse(read(cursor, text, 0, 1).ok, "previous result snapshots are released");
            Resp.of(Bridge.call(Ops.CLOSE_CURSOR, cursor, 0, null)).assertOk();
            assertFalse(read(cursor, binary, 0, 1).ok);
        } finally {
            H2.close(session);
        }
    }
}
