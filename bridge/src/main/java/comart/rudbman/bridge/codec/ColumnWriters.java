package comart.rudbman.bridge.codec;

import java.math.BigDecimal;
import java.nio.charset.StandardCharsets;
import java.sql.Blob;
import java.sql.Clob;
import java.sql.ResultSet;
import java.sql.SQLException;
import java.sql.Types;
import java.util.Arrays;

/**
 * Concrete {@link ColumnWriter} implementations, one per physical kind.
 *
 * <p>They are grouped here rather than spread over seven files because they only
 * make sense together: the set of writers <em>is</em> the codec's value layer.
 */
public final class ColumnWriters {

    private ColumnWriters() {
    }

    /**
     * Builds the writer for a result column.
     *
     * @param sqlType   a {@link java.sql.Types} constant from the result metadata
     * @param precision column precision from the result metadata
     * @param capacity  expected row count, used to presize the value arrays
     * @param lobs      sink for LOB references
     * @return a writer matching {@link ColumnKind#forSqlType(int, int)}
     */
    public static ColumnWriter forColumn(int sqlType, int precision, int capacity, LobSink lobs) {
        int kind = ColumnKind.forSqlType(sqlType, precision);
        switch (kind) {
            case ColumnKind.I64:
                return new I64Writer(capacity);
            case ColumnKind.F64:
                return new F64Writer(capacity);
            case ColumnKind.BOOL:
                return new BoolWriter(capacity);
            case ColumnKind.BIN:
                return new BinWriter(capacity);
            case ColumnKind.LOB:
                return new LobWriter(capacity, lobs, sqlType == Types.BLOB);
            case ColumnKind.STR:
            default:
                boolean decimal = sqlType == Types.DECIMAL || sqlType == Types.NUMERIC;
                return new StrWriter(capacity, decimal);
        }
    }

    /** Signed 64-bit integer column. */
    static final class I64Writer extends ColumnWriter {
        private long[] vals;

        I64Writer(int capacity) {
            vals = new long[Math.max(8, capacity)];
        }

        @Override
        protected boolean readValue(ResultSet rs, int column, long rowInCursor) throws SQLException {
            long v = rs.getLong(column);
            if (rows() >= vals.length) {
                vals = Arrays.copyOf(vals, vals.length * 2);
            }
            if (rs.wasNull()) {
                vals[rows()] = 0L;
                return false;
            }
            vals[rows()] = v;
            return true;
        }

        @Override
        protected void writeValues(LeBuf out, int rowCount) {
            for (int i = 0; i < rowCount; i++) {
                out.i64(vals[i]);
            }
        }

        @Override
        protected int kind() {
            return ColumnKind.I64;
        }
    }

    /** IEEE-754 double column. NaN and the infinities survive as raw bits. */
    static final class F64Writer extends ColumnWriter {
        private double[] vals;

        F64Writer(int capacity) {
            vals = new double[Math.max(8, capacity)];
        }

        @Override
        protected boolean readValue(ResultSet rs, int column, long rowInCursor) throws SQLException {
            double v = rs.getDouble(column);
            if (rows() >= vals.length) {
                vals = Arrays.copyOf(vals, vals.length * 2);
            }
            if (rs.wasNull()) {
                vals[rows()] = 0d;
                return false;
            }
            vals[rows()] = v;
            return true;
        }

        @Override
        protected void writeValues(LeBuf out, int rowCount) {
            for (int i = 0; i < rowCount; i++) {
                out.f64(vals[i]);
            }
        }

        @Override
        protected int kind() {
            return ColumnKind.F64;
        }
    }

    /** Boolean column, packed LSB-first exactly like the validity bitmap. */
    static final class BoolWriter extends ColumnWriter {
        private byte[] bits;

        BoolWriter(int capacity) {
            bits = new byte[Math.max(8, (capacity + 7) >> 3)];
        }

        @Override
        protected boolean readValue(ResultSet rs, int column, long rowInCursor) throws SQLException {
            boolean v = rs.getBoolean(column);
            int idx = rows();
            if ((idx >> 3) >= bits.length) {
                bits = Arrays.copyOf(bits, bits.length * 2);
            }
            if (rs.wasNull()) {
                return false;
            }
            if (v) {
                bits[idx >> 3] |= (byte) (1 << (idx & 7));
            }
            return true;
        }

        @Override
        protected void writeValues(LeBuf out, int rowCount) {
            out.bytes(bits, 0, (rowCount + 7) >> 3);
        }

        @Override
        protected int kind() {
            return ColumnKind.BOOL;
        }
    }

    /** Base for the two variable-length kinds, which share the offsets layout. */
    abstract static class VarWriter extends ColumnWriter {
        private int[] offsets;
        private final LeBuf data;

        VarWriter(int capacity, int dataHint) {
            offsets = new int[Math.max(9, capacity + 1)];
            offsets[0] = 0;
            data = new LeBuf(dataHint);
        }

        final void append(byte[] value) {
            int idx = rows();
            growOffsets(idx);
            if (value != null) {
                data.bytes(value);
            }
            offsets[idx + 1] = data.size();
        }

        private void growOffsets(int idx) {
            if (idx + 1 >= offsets.length) {
                offsets = Arrays.copyOf(offsets, offsets.length * 2);
            }
        }

        @Override
        protected final void writeValues(LeBuf out, int rowCount) {
            for (int i = 0; i <= rowCount; i++) {
                out.u32(offsets[i]);
            }
            out.bytes(data.toArray());
        }
    }

    /**
     * Text column.
     *
     * <p>Also carries DECIMAL, DATE, TIME, TIMESTAMP, UUID, INTERVAL, arrays and
     * every vendor type the codec has no opinion about. DECIMAL goes through
     * {@link BigDecimal#toPlainString()} so a scale of 8 stays a scale of 8 and
     * never turns into exponent notation.
     */
    static final class StrWriter extends VarWriter {
        private final boolean decimal;

        StrWriter(int capacity, boolean decimal) {
            super(capacity, Math.max(64, capacity * 16));
            this.decimal = decimal;
        }

        @Override
        protected boolean readValue(ResultSet rs, int column, long rowInCursor) throws SQLException {
            String s;
            if (decimal) {
                BigDecimal bd = rs.getBigDecimal(column);
                s = bd == null ? null : bd.toPlainString();
            } else {
                s = text(rs, column);
            }
            if (s == null || rs.wasNull()) {
                append(null);
                return false;
            }
            append(s.getBytes(StandardCharsets.UTF_8));
            return true;
        }

        private static String text(ResultSet rs, int column) throws SQLException {
            try {
                return rs.getString(column);
            } catch (SQLException e) {
                // Some drivers refuse getString on structured or vendor types
                // (arrays, geometry, intervals). The grid still has to show
                // something, and toString of the driver's own object is the
                // closest thing to a canonical rendering we can get.
                Object o = rs.getObject(column);
                return o == null ? null : String.valueOf(o);
            }
        }

        @Override
        protected int kind() {
            return ColumnKind.STR;
        }
    }

    /** Raw byte column. */
    static final class BinWriter extends VarWriter {
        BinWriter(int capacity) {
            super(capacity, Math.max(64, capacity * 16));
        }

        @Override
        protected boolean readValue(ResultSet rs, int column, long rowInCursor) throws SQLException {
            byte[] b = rs.getBytes(column);
            if (b == null || rs.wasNull()) {
                append(null);
                return false;
            }
            append(b);
            return true;
        }

        @Override
        protected int kind() {
            return ColumnKind.BIN;
        }
    }

    /**
     * LOB reference column: {@code (u64 lob_id, u64 size)} per row.
     *
     * <p>Only the identifier and the length cross the boundary. The size of a
     * binary LOB is in octets, the size of a character LOB is in characters
     * (that is what {@link Clob#length()} means and what a later {@code LOB_READ}
     * offset would have to be counted in).
     */
    static final class LobWriter extends ColumnWriter {
        private long[] ids;
        private long[] sizes;
        private final LobSink sink;
        private final boolean binary;

        LobWriter(int capacity, LobSink sink, boolean binary) {
            this.ids = new long[Math.max(8, capacity)];
            this.sizes = new long[Math.max(8, capacity)];
            this.sink = sink;
            this.binary = binary;
        }

        @Override
        protected boolean readValue(ResultSet rs, int column, long rowInCursor) throws SQLException {
            int idx = rows();
            if (idx >= ids.length) {
                ids = Arrays.copyOf(ids, ids.length * 2);
                sizes = Arrays.copyOf(sizes, sizes.length * 2);
            }
            ids[idx] = 0L;
            sizes[idx] = 0L;

            long size = LobSink.UNKNOWN_SIZE;
            Object lob;
            if (binary) {
                Blob b = rs.getBlob(column);
                lob = b;
                if (b != null) {
                    try {
                        size = b.length();
                    } catch (SQLException | RuntimeException e) {
                        // Streaming drivers do not always know the length up
                        // front; the sentinel tells the UI to say "unknown".
                        size = LobSink.UNKNOWN_SIZE;
                    }
                }
            } else {
                Clob c = rs.getClob(column);
                lob = c;
                if (c != null) {
                    try {
                        size = c.length();
                    } catch (SQLException | RuntimeException e) {
                        size = LobSink.UNKNOWN_SIZE;
                    }
                }
            }
            if (lob == null || rs.wasNull()) {
                return false;
            }
            ids[idx] = sink.registerLob(rowInCursor, column, size, binary);
            sizes[idx] = size;
            free(lob);
            return true;
        }

        private static void free(Object lob) {
            // Temporary LOBs leak server-side on some drivers (Oracle in
            // particular) if they are never freed, and we have already taken
            // everything we need from the object.
            try {
                if (lob instanceof Blob) {
                    ((Blob) lob).free();
                } else if (lob instanceof Clob) {
                    ((Clob) lob).free();
                }
            } catch (SQLException | RuntimeException | AbstractMethodError ignored) {
                // free() is optional in practice; a driver that cannot do it is
                // not a reason to fail the fetch.
            }
        }

        @Override
        protected void writeValues(LeBuf out, int rowCount) {
            for (int i = 0; i < rowCount; i++) {
                out.i64(ids[i]);
                out.i64(sizes[i]);
            }
        }

        @Override
        protected int kind() {
            return ColumnKind.LOB;
        }
    }
}
