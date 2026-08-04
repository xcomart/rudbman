package comart.rudbman.bridge;

/**
 * Operation codes of the JNI protocol (architecture.md 4.4).
 *
 * <p>New capabilities are added as new codes; the JNI signature never changes.
 * The numbers are part of the wire contract with {@code rudbman-jdbc} and must
 * not be renumbered.
 */
public final class Ops {

    /** {@code req} JSON connection spec, {@code resp} JSON {@code {session}}. */
    public static final int OPEN_SESSION = 0x01;
    /** {@code handle} session. */
    public static final int CLOSE_SESSION = 0x02;
    /** {@code handle} session, {@code resp} JSON {@code {ok, elapsed_ms}}. */
    public static final int PING = 0x03;
    /** {@code handle} session, {@code resp} JSON product and capability facts. */
    public static final int SESSION_INFO = 0x04;
    /** {@code handle} session, {@code req} JSON {@code {kind, ...}}. */
    public static final int DESCRIBE = 0x10;
    /** {@code handle} session, {@code req} JSON statement spec. */
    public static final int EXECUTE = 0x20;
    /** {@code handle} cursor, {@code arg} max rows, {@code resp} RDB1 batch. */
    public static final int FETCH = 0x21;
    /** {@code handle} cursor, {@code resp} same shape as {@link #EXECUTE}. */
    public static final int MORE_RESULTS = 0x22;
    /** {@code handle} cursor. */
    public static final int CLOSE_CURSOR = 0x23;
    /** {@code handle} session. Arrives on a thread other than the worker. */
    public static final int CANCEL = 0x24;
    /** {@code handle} cursor, {@code req} JSON {@code {row, col, offset, len}}. */
    public static final int LOB_READ = 0x25;
    /** {@code handle} session, {@code arg} 0 or 1. */
    public static final int SET_AUTOCOMMIT = 0x30;
    /** {@code handle} session. */
    public static final int COMMIT = 0x31;
    /** {@code handle} session. */
    public static final int ROLLBACK = 0x32;
    /** {@code handle} session, {@code req} JSON job spec. */
    public static final int JOB_START = 0x40;
    /** {@code handle} job, {@code resp} JSON progress. */
    public static final int JOB_POLL = 0x41;
    /** {@code handle} job. */
    public static final int JOB_CANCEL = 0x42;
    /** {@code req} JSON {@code {jars[]}}, {@code resp} JSON {@code {classes[]}}. */
    public static final int PROBE_DRIVER = 0x50;

    private Ops() {
    }
}
