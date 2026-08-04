package comart.rudbman.bridge;

import java.util.concurrent.ConcurrentHashMap;
import java.util.concurrent.atomic.AtomicLong;

/**
 * Handle table mapping the opaque {@code long} handles of the JNI protocol to
 * live bridge objects.
 *
 * <p>A single number space is used for every object type. Handles are never
 * reused, so a stale handle from a closed session can never be mistaken for a
 * fresh cursor; it simply is not in the table any more.
 */
public final class Registry {

    /** Handle 0 is reserved to mean "no handle". */
    private static final AtomicLong SEQ = new AtomicLong(1);

    private static final ConcurrentHashMap<Long, Object> TABLE = new ConcurrentHashMap<>();

    private Registry() {
    }

    /**
     * Registers an object and allocates its handle.
     *
     * @param o the object
     * @return a fresh, non-zero handle
     */
    public static long put(Object o) {
        long h = SEQ.getAndIncrement();
        TABLE.put(h, o);
        return h;
    }

    /**
     * Removes a handle.
     *
     * @param handle the handle
     * @return the object that was registered, or {@code null}
     */
    public static Object remove(long handle) {
        return TABLE.remove(handle);
    }

    /**
     * Looks up a handle and checks its type.
     *
     * @param <T>    expected type
     * @param handle the handle
     * @param type   expected class
     * @param what   noun used in the error message
     * @return the registered object
     * @throws BridgeException with kind {@code protocol} when the handle is
     *         unknown or refers to a different kind of object
     */
    public static <T> T get(long handle, Class<T> type, String what) {
        Object o = TABLE.get(handle);
        if (o == null) {
            throw new BridgeException("protocol", "unknown or already closed " + what + " handle " + handle);
        }
        if (!type.isInstance(o)) {
            throw new BridgeException("protocol", "handle " + handle + " is not a " + what);
        }
        return type.cast(o);
    }

    /**
     * @param handle a session handle
     * @return the session
     * @throws BridgeException when the handle is not a live session
     */
    public static Session session(long handle) {
        return get(handle, Session.class, "session");
    }

    /**
     * @param handle a cursor handle
     * @return the cursor
     * @throws BridgeException when the handle is not a live cursor
     */
    public static Cursor cursor(long handle) {
        return get(handle, Cursor.class, "cursor");
    }

    /** @return the number of live handles. */
    public static int size() {
        return TABLE.size();
    }
}
