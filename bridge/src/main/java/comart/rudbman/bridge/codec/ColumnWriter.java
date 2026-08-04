package comart.rudbman.bridge.codec;

import java.sql.ResultSet;
import java.sql.SQLException;

/**
 * Accumulates the values of one result column for one batch and emits its
 * {@code Column} record.
 *
 * <p>The base class owns the validity bitmap so every subclass gets NULL
 * handling for free and cannot get the bit order wrong. Bits are LSB-first:
 * row {@code i} lives in byte {@code i >> 3} at bit {@code i & 7}, and a set bit
 * means <em>non-null</em>.
 */
public abstract class ColumnWriter {

    private byte[] validity = new byte[64];
    private int rows;
    private int validCount;

    /**
     * Reads the value of this column from the current row of {@code rs}.
     *
     * @param rs          result set positioned on a row
     * @param column      one-based column index
     * @param rowInCursor absolute row index within the cursor, needed by LOB
     *                    columns to make their references addressable later
     * @throws SQLException if the driver fails to produce the value
     */
    public final void read(ResultSet rs, int column, long rowInCursor) throws SQLException {
        int byteIndex = rows >> 3;
        if (byteIndex >= validity.length) {
            byte[] nb = new byte[validity.length * 2];
            System.arraycopy(validity, 0, nb, 0, validity.length);
            validity = nb;
        }
        boolean nonNull = readValue(rs, column, rowInCursor);
        if (nonNull) {
            validity[byteIndex] |= (byte) (1 << (rows & 7));
            validCount++;
        }
        rows++;
    }

    /** @return the number of rows appended so far. */
    public final int rows() {
        return rows;
    }

    /**
     * Writes this column's {@code u8 kind | u32 payload_len | payload} record.
     *
     * <p>A column whose every value in this batch is NULL degenerates to
     * {@link ColumnKind#NULLS}: the bitmap is still emitted, per the "payload
     * always starts with the validity bitmap" rule, but the value area is
     * omitted. That turns an all-null 500-row string column from kilobytes of
     * offsets into 63 bytes.
     *
     * @param out destination buffer
     */
    public final void emit(LeBuf out) {
        boolean allNull = validCount == 0;
        out.u8(allNull ? ColumnKind.NULLS : kind());
        int lenPos = out.reserveU32();
        int start = out.size();
        out.bytes(validity, 0, (rows + 7) >> 3);
        if (!allNull) {
            writeValues(out, rows);
        }
        out.patchU32(lenPos, out.size() - start);
    }

    /**
     * Reads one value and stores it, appending a placeholder when the value is
     * NULL so that fixed-width value arrays stay index-aligned with the bitmap.
     *
     * @param rs          result set positioned on a row
     * @param column      one-based column index
     * @param rowInCursor absolute row index within the cursor
     * @return {@code true} when the value was non-null
     * @throws SQLException if the driver fails to produce the value
     */
    protected abstract boolean readValue(ResultSet rs, int column, long rowInCursor)
            throws SQLException;

    /**
     * Appends the value area for this column.
     *
     * @param out      destination buffer
     * @param rowCount number of rows in the batch
     */
    protected abstract void writeValues(LeBuf out, int rowCount);

    /** @return the {@link ColumnKind} constant this writer produces. */
    protected abstract int kind();
}
