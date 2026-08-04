package comart.rudbman.bridge.codec;

/**
 * Growable little-endian byte buffer.
 *
 * <p>The {@code RDB1} codec is entirely little-endian and is written on the hot
 * fetch path, so this avoids {@link java.nio.ByteBuffer} reallocation dances and
 * the bounds checks that come with a fixed-capacity buffer.
 */
public final class LeBuf {

    private byte[] buf;
    private int len;

    /**
     * Creates a buffer with the given initial capacity.
     *
     * @param capacity initial capacity in bytes; values below 16 are raised to 16
     */
    public LeBuf(int capacity) {
        this.buf = new byte[Math.max(16, capacity)];
        this.len = 0;
    }

    /** @return the number of bytes written so far. */
    public int size() {
        return len;
    }

    private void ensure(int extra) {
        int need = len + extra;
        if (need <= buf.length) {
            return;
        }
        int cap = buf.length;
        while (cap < need) {
            // Doubling keeps amortised append cost constant; a 500-row batch
            // reaches its final size in a handful of copies.
            cap = cap + (cap >> 1) + 16;
        }
        byte[] nb = new byte[cap];
        System.arraycopy(buf, 0, nb, 0, len);
        buf = nb;
    }

    /**
     * Appends one byte.
     *
     * @param v value, low 8 bits used
     */
    public void u8(int v) {
        ensure(1);
        buf[len++] = (byte) v;
    }

    /**
     * Appends a 32-bit little-endian value.
     *
     * @param v value, low 32 bits used
     */
    public void u32(long v) {
        ensure(4);
        buf[len++] = (byte) v;
        buf[len++] = (byte) (v >>> 8);
        buf[len++] = (byte) (v >>> 16);
        buf[len++] = (byte) (v >>> 24);
    }

    /**
     * Appends a 64-bit little-endian value.
     *
     * @param v value
     */
    public void i64(long v) {
        ensure(8);
        buf[len++] = (byte) v;
        buf[len++] = (byte) (v >>> 8);
        buf[len++] = (byte) (v >>> 16);
        buf[len++] = (byte) (v >>> 24);
        buf[len++] = (byte) (v >>> 32);
        buf[len++] = (byte) (v >>> 40);
        buf[len++] = (byte) (v >>> 48);
        buf[len++] = (byte) (v >>> 56);
    }

    /**
     * Appends an IEEE-754 double in little-endian order.
     *
     * @param v value
     */
    public void f64(double v) {
        i64(Double.doubleToRawLongBits(v));
    }

    /**
     * Appends raw bytes.
     *
     * @param b source array
     */
    public void bytes(byte[] b) {
        bytes(b, 0, b.length);
    }

    /**
     * Appends a slice of raw bytes.
     *
     * @param b   source array
     * @param off start offset
     * @param n   byte count
     */
    public void bytes(byte[] b, int off, int n) {
        ensure(n);
        System.arraycopy(b, off, buf, len, n);
        len += n;
    }

    /**
     * Reserves four bytes for a length that is only known once the payload has
     * been written, and returns the position to patch later.
     *
     * @return the offset of the reserved slot
     */
    public int reserveU32() {
        int pos = len;
        u32(0);
        return pos;
    }

    /**
     * Patches a previously reserved 32-bit slot.
     *
     * @param pos offset returned by {@link #reserveU32()}
     * @param v   value, low 32 bits used
     */
    public void patchU32(int pos, long v) {
        buf[pos] = (byte) v;
        buf[pos + 1] = (byte) (v >>> 8);
        buf[pos + 2] = (byte) (v >>> 16);
        buf[pos + 3] = (byte) (v >>> 24);
    }

    /** @return a trimmed copy of the written bytes. */
    public byte[] toArray() {
        byte[] out = new byte[len];
        System.arraycopy(buf, 0, out, 0, len);
        return out;
    }
}
