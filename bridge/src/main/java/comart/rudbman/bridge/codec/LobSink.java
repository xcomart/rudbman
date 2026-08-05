package comart.rudbman.bridge.codec;

/**
 * Allocates LOB identifiers for values that are referenced from a batch instead
 * of being inlined into it.
 *
 * <p>Implemented by the cursor, because a LOB id is only meaningful relative to
 * the cursor that produced it.
 */
public interface LobSink {

    /** Size sentinel for a driver that refuses to report a LOB length. */
    long UNKNOWN_SIZE = -1L;

    /**
     * Records a LOB reference and returns the identifier written into the batch.
     *
     * @param rowInCursor zero-based row index counted from the first row this
     *                    cursor ever produced, not from the start of the batch
     * @param column      one-based JDBC column index
     * @param size        length in octets for binary LOBs, in characters for
     *                    character LOBs, or {@link #UNKNOWN_SIZE}
     * @param binary      {@code true} for BLOB-like values
     * @return a non-zero identifier, unique within the cursor
     */
    long registerLob(long rowInCursor, int column, long size, boolean binary);
}
