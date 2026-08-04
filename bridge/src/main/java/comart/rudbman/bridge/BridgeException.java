package comart.rudbman.bridge;

/**
 * Failure raised by the bridge itself rather than by a driver.
 *
 * <p>Carries the {@code kind} discriminant of the error envelope directly, so
 * that protocol violations ("unknown op", "stale handle") do not get filed under
 * {@code internal} where they would look like bridge bugs.
 */
public class BridgeException extends RuntimeException {

    /** Serialised form version; the bridge never serialises these, but the compiler asks. */
    private static final long serialVersionUID = 1L;

    /** Envelope discriminant this failure should be reported under. */
    private final String kind;

    /**
     * @param kind    one of {@code sql}, {@code driver}, {@code io},
     *                {@code protocol}, {@code interrupted}, {@code internal}
     * @param message human readable description
     */
    public BridgeException(String kind, String message) {
        super(message);
        this.kind = kind;
    }

    /**
     * @param kind    error kind, see {@link #BridgeException(String, String)}
     * @param message human readable description
     * @param cause   underlying failure
     */
    public BridgeException(String kind, String message, Throwable cause) {
        super(message, cause);
        this.kind = kind;
    }

    /** @return the error envelope {@code kind}. */
    public String kind() {
        return kind;
    }
}
