package comart.rudbman.bridge.support;

import java.nio.ByteBuffer;
import java.nio.ByteOrder;
import java.nio.charset.StandardCharsets;

/**
 * Independent decoder for the {@code RDB1} batch format.
 *
 * <p>Written from the format description rather than from the encoder, on
 * purpose: it plays the role the Rust decoder will play, so a round trip through
 * it is the only thing that actually proves both sides agree.
 */
public final class Batch {

    /** Physical kind of a decoded column. */
    public static final int NULLS = 0;
    /** @see comart.rudbman.bridge.codec.ColumnKind */
    public static final int I64 = 1;
    /** @see comart.rudbman.bridge.codec.ColumnKind */
    public static final int F64 = 2;
    /** @see comart.rudbman.bridge.codec.ColumnKind */
    public static final int BOOL = 3;
    /** @see comart.rudbman.bridge.codec.ColumnKind */
    public static final int STR = 4;
    /** @see comart.rudbman.bridge.codec.ColumnKind */
    public static final int BIN = 5;
    /** @see comart.rudbman.bridge.codec.ColumnKind */
    public static final int LOB = 6;

    /** Number of columns declared in the header. */
    public final int colCount;
    /** Number of rows declared in the header. */
    public final int rowCount;
    /** Whether flags bit0 was set. */
    public final boolean last;
    /** Decoded columns. */
    public final Column[] columns;

    private Batch(int colCount, int rowCount, boolean last, Column[] columns) {
        this.colCount = colCount;
        this.rowCount = rowCount;
        this.last = last;
        this.columns = columns;
    }

    /** One decoded column. */
    public static final class Column {
        /** Physical kind byte. */
        public final int kind;
        /** Per-row non-null flags decoded from the validity bitmap. */
        public final boolean[] valid;
        /** Values for {@link #I64}. */
        public long[] i64;
        /** Values for {@link #F64}. */
        public double[] f64;
        /** Values for {@link #BOOL}. */
        public boolean[] bools;
        /** Raw slices for {@link #STR} and {@link #BIN}. */
        public byte[][] bytes;
        /** Identifiers for {@link #LOB}. */
        public long[] lobIds;
        /** Sizes for {@link #LOB}. */
        public long[] lobSizes;

        Column(int kind, boolean[] valid) {
            this.kind = kind;
            this.valid = valid;
        }

        /**
         * @param row row index
         * @return the UTF-8 decoded value, or {@code null} when the row is NULL
         */
        public String str(int row) {
            if (!valid[row]) {
                return null;
            }
            return new String(bytes[row], StandardCharsets.UTF_8);
        }

        /**
         * @param row row index
         * @return the raw bytes, or {@code null} when the row is NULL
         */
        public byte[] bin(int row) {
            return valid[row] ? bytes[row] : null;
        }
    }

    /**
     * Decodes a batch.
     *
     * @param data the encoded batch
     * @return the decoded batch
     */
    public static Batch decode(byte[] data) {
        ByteBuffer b = ByteBuffer.wrap(data).order(ByteOrder.LITTLE_ENDIAN);
        byte[] magic = new byte[4];
        b.get(magic);
        if (magic[0] != 'R' || magic[1] != 'D' || magic[2] != 'B' || magic[3] != '1') {
            throw new IllegalArgumentException("not an RDB1 batch");
        }
        int cols = b.getInt();
        int rows = b.getInt();
        int flags = b.get() & 0xFF;

        Column[] out = new Column[cols];
        for (int c = 0; c < cols; c++) {
            int kind = b.get() & 0xFF;
            int payloadLen = b.getInt();
            int end = b.position() + payloadLen;

            int bitmapBytes = (rows + 7) >> 3;
            byte[] bitmap = new byte[bitmapBytes];
            b.get(bitmap);
            boolean[] valid = new boolean[rows];
            for (int r = 0; r < rows; r++) {
                valid[r] = (bitmap[r >> 3] & (1 << (r & 7))) != 0;
            }
            Column col = new Column(kind, valid);

            switch (kind) {
                case NULLS:
                    break;
                case I64:
                    col.i64 = new long[rows];
                    for (int r = 0; r < rows; r++) {
                        col.i64[r] = b.getLong();
                    }
                    break;
                case F64:
                    col.f64 = new double[rows];
                    for (int r = 0; r < rows; r++) {
                        col.f64[r] = b.getDouble();
                    }
                    break;
                case BOOL: {
                    byte[] bits = new byte[(rows + 7) >> 3];
                    b.get(bits);
                    col.bools = new boolean[rows];
                    for (int r = 0; r < rows; r++) {
                        col.bools[r] = (bits[r >> 3] & (1 << (r & 7))) != 0;
                    }
                    break;
                }
                case STR:
                case BIN: {
                    int[] offsets = new int[rows + 1];
                    for (int r = 0; r <= rows; r++) {
                        offsets[r] = b.getInt();
                    }
                    int dataStart = b.position();
                    col.bytes = new byte[rows][];
                    for (int r = 0; r < rows; r++) {
                        int len = offsets[r + 1] - offsets[r];
                        byte[] slice = new byte[len];
                        System.arraycopy(data, dataStart + offsets[r], slice, 0, len);
                        col.bytes[r] = slice;
                    }
                    break;
                }
                case LOB:
                    col.lobIds = new long[rows];
                    col.lobSizes = new long[rows];
                    for (int r = 0; r < rows; r++) {
                        col.lobIds[r] = b.getLong();
                        col.lobSizes[r] = b.getLong();
                    }
                    break;
                default:
                    throw new IllegalArgumentException("unknown column kind " + kind);
            }
            if (b.position() > end) {
                throw new IllegalStateException(
                        "column " + c + " read past its payload (" + b.position() + " > " + end + ")");
            }
            // Trust payload_len, so a decoder bug shows up as a mismatch here
            // rather than as garbage in the next column.
            b.position(end);
            out[c] = col;
        }
        if (b.hasRemaining()) {
            throw new IllegalStateException(b.remaining() + " trailing bytes after the last column");
        }
        return new Batch(cols, rows, (flags & 1) != 0, out);
    }
}
