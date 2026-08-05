package comart.rudbman.bridge.codec;

import java.sql.ResultSet;
import java.sql.SQLException;

/**
 * Encoder for the {@code RDB1} result batch format (architecture.md 4.6).
 *
 * <pre>
 * Batch  := Header Column*
 * Header := "RDB1"(4B) | u32 col_count | u32 row_count | u8 flags
 *           flags bit0 = this is the last batch
 * Column := u8 kind | u32 payload_len | payload
 * </pre>
 *
 * <p>Every integer is little-endian. Every {@code payload} starts with a
 * validity bitmap of {@code ceil(row_count/8)} bytes in which a set bit means
 * non-null, LSB-first.
 */
public final class BatchWriter {

    private static final byte[] MAGIC = {'R', 'D', 'B', '1'};

    /** Header flag: no further batches follow on this cursor. */
    public static final int FLAG_LAST = 0x01;

    private final ColumnWriter[] columns;
    private int rows;

    /**
     * @param columns one writer per result column, in result order
     */
    public BatchWriter(ColumnWriter[] columns) {
        this.columns = columns;
    }

    /**
     * Appends the current row of {@code rs}.
     *
     * @param rs          result set positioned on a row
     * @param rowInCursor absolute row index within the cursor
     * @throws SQLException if the driver fails to produce a value
     */
    public void addRow(ResultSet rs, long rowInCursor) throws SQLException {
        for (int i = 0; i < columns.length; i++) {
            columns[i].read(rs, i + 1, rowInCursor);
        }
        rows++;
    }

    /** @return the number of rows appended so far. */
    public int rowCount() {
        return rows;
    }

    /**
     * Serialises the batch.
     *
     * @param last whether this is the final batch of the cursor
     * @return the encoded batch
     */
    public byte[] finish(boolean last) {
        LeBuf out = new LeBuf(1024 + rows * Math.max(1, columns.length) * 8);
        out.bytes(MAGIC);
        out.u32(columns.length);
        out.u32(rows);
        out.u8(last ? FLAG_LAST : 0);
        for (ColumnWriter c : columns) {
            c.emit(out);
        }
        return out.toArray();
    }

    /**
     * Produces a batch with no columns and no rows.
     *
     * <p>Used when a cursor has no result set to read from at all, so that the
     * caller always gets a well-formed batch instead of an error envelope.
     *
     * @param last whether this is the final batch of the cursor
     * @return the encoded batch
     */
    public static byte[] empty(boolean last) {
        return new BatchWriter(new ColumnWriter[0]).finish(last);
    }
}
