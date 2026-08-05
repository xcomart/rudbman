package comart.rudbman.bridge;

import com.google.gson.JsonArray;
import com.google.gson.JsonElement;
import com.google.gson.JsonObject;

import java.io.IOException;
import java.io.PrintWriter;
import java.io.StringWriter;
import java.io.UncheckedIOException;
import java.nio.charset.StandardCharsets;
import java.sql.SQLException;
import java.sql.SQLTimeoutException;
import java.sql.SQLTransientConnectionException;
import java.util.IdentityHashMap;
import java.util.Map;

/**
 * Builds the response envelope of architecture.md 4.5.
 *
 * <pre>
 * u8  tag       0 = OK, 1 = ERROR
 *     payload   OK: operation specific body (JSON or binary), ERROR: JSON
 * </pre>
 */
public final class Envelope {

    /** Response tag for a successful call. */
    public static final byte TAG_OK = 0;
    /** Response tag for a failed call. */
    public static final byte TAG_ERROR = 1;

    /** Depth cap for the cause chain; buggy drivers have been seen to cycle. */
    private static final int MAX_CAUSES = 16;
    /** Stack traces go to the debug log only, so they do not need to be complete. */
    private static final int MAX_STACK_CHARS = 16 * 1024;

    /**
     * Envelope of last resort, pre-encoded so that reporting a failure never
     * needs to allocate. Used when even building the real envelope throws,
     * which in practice means {@link OutOfMemoryError}.
     */
    private static final byte[] LAST_RESORT =
            ("{\"kind\":\"internal\",\"sql_state\":null,\"vendor_code\":0,"
                    + "\"message\":\"bridge failed to encode the error envelope\","
                    + "\"causes\":[],\"stack\":null}").getBytes(StandardCharsets.US_ASCII);

    private Envelope() {
    }

    /** @return an OK envelope with no body. */
    public static byte[] ok() {
        return new byte[]{TAG_OK};
    }

    /**
     * @param body JSON tree to serialise
     * @return an OK envelope carrying {@code body}
     */
    public static byte[] ok(JsonElement body) {
        return ok(Json.bytes(body));
    }

    /**
     * @param body raw payload bytes, JSON or binary depending on the operation
     * @return an OK envelope carrying {@code body}
     */
    public static byte[] ok(byte[] body) {
        byte[] out = new byte[body.length + 1];
        out[0] = TAG_OK;
        System.arraycopy(body, 0, out, 1, body.length);
        return out;
    }

    /** @return the pre-encoded envelope used when error encoding itself fails. */
    public static byte[] lastResort() {
        return LAST_RESORT.clone();
    }

    /**
     * Converts any throwable into an ERROR envelope.
     *
     * @param t the failure
     * @return an ERROR envelope
     */
    public static byte[] error(Throwable t) {
        byte[] json = Json.bytes(describe(t));
        byte[] out = new byte[json.length + 1];
        out[0] = TAG_ERROR;
        System.arraycopy(json, 0, out, 1, json.length);
        return out;
    }

    /**
     * Describes a failure in the same shape an ERROR envelope carries.
     *
     * <p>Split out of {@link #error} because a job reports its failures inside an
     * OK envelope - {@code JOB_POLL} succeeded, the job did not - and the two
     * must not describe the same exception differently.
     *
     * @param t the failure
     * @return an object with {@code kind}, {@code sql_state}, {@code vendor_code},
     *         {@code message}, {@code causes} and {@code stack}
     */
    public static JsonObject describe(Throwable t) {
        JsonObject o = new JsonObject();
        o.addProperty("kind", kindOf(t));

        SQLException sql = firstSqlException(t);
        o.addProperty("sql_state", sql == null ? null : sqlStateOf(sql));
        o.addProperty("vendor_code", sql == null ? 0 : vendorCodeOf(sql));
        o.addProperty("message", messageOf(t));
        o.add("causes", causes(t));
        o.addProperty("stack", stack(t));
        return o;
    }

    private static String messageOf(Throwable t) {
        String m = t.getMessage();
        if (m == null || m.isEmpty()) {
            // NoClassDefFoundError and friends often carry only the class name;
            // an empty message in the UI is worse than a bare type name.
            m = t.getClass().getName();
        }
        return m;
    }

    /**
     * @param t a failure
     * @return the envelope {@code kind} discriminant
     */
    static String kindOf(Throwable t) {
        if (t instanceof BridgeException) {
            return ((BridgeException) t).kind();
        }
        if (t instanceof InterruptedException) {
            return "interrupted";
        }
        if (t instanceof SQLTimeoutException || t instanceof SQLTransientConnectionException) {
            return "sql";
        }
        if (t instanceof SQLException) {
            return "sql";
        }
        if (t instanceof ClassNotFoundException
                || t instanceof NoClassDefFoundError
                || t instanceof UnsupportedClassVersionError
                || t instanceof ExceptionInInitializerError
                || t instanceof LinkageError) {
            // An incomplete or mismatched driver jar. The user has to fix the
            // jar, so this must not read as an internal bridge failure.
            return "driver";
        }
        if (t instanceof IOException || t instanceof UncheckedIOException) {
            return "io";
        }
        return "internal";
    }

    /**
     * Finds the first {@link SQLException} anywhere in the chain.
     *
     * <p>Drivers routinely wrap the interesting exception, and just as routinely
     * hide the real one behind {@link SQLException#getNextException()}.
     */
    private static SQLException firstSqlException(Throwable t) {
        Map<Throwable, Boolean> seen = new IdentityHashMap<>();
        Throwable cur = t;
        int guard = 0;
        while (cur != null && guard++ < MAX_CAUSES && seen.put(cur, Boolean.TRUE) == null) {
            if (cur instanceof SQLException) {
                return (SQLException) cur;
            }
            cur = cur.getCause();
        }
        return null;
    }

    /** Walks the next-exception chain for the first non-empty SQLSTATE. */
    private static String sqlStateOf(SQLException e) {
        SQLException cur = e;
        int guard = 0;
        Map<Throwable, Boolean> seen = new IdentityHashMap<>();
        while (cur != null && guard++ < MAX_CAUSES && seen.put(cur, Boolean.TRUE) == null) {
            String s = cur.getSQLState();
            if (s != null && !s.isEmpty()) {
                return s;
            }
            cur = cur.getNextException();
        }
        return null;
    }

    /** Walks the next-exception chain for the first non-zero vendor code. */
    private static int vendorCodeOf(SQLException e) {
        SQLException cur = e;
        int guard = 0;
        Map<Throwable, Boolean> seen = new IdentityHashMap<>();
        while (cur != null && guard++ < MAX_CAUSES && seen.put(cur, Boolean.TRUE) == null) {
            int c = cur.getErrorCode();
            if (c != 0) {
                return c;
            }
            cur = cur.getNextException();
        }
        return 0;
    }

    /**
     * Flattens both chains a JDBC failure can hang off: {@code getCause} and,
     * for SQL exceptions, {@code getNextException}. The real reason a connection
     * was refused is very often in the second one.
     */
    private static JsonArray causes(Throwable t) {
        JsonArray arr = new JsonArray();
        Map<Throwable, Boolean> seen = new IdentityHashMap<>();
        seen.put(t, Boolean.TRUE);
        collect(t, arr, seen, true);
        return arr;
    }

    private static void collect(Throwable t, JsonArray arr, Map<Throwable, Boolean> seen, boolean root) {
        if (t == null || arr.size() >= MAX_CAUSES) {
            return;
        }
        if (!root) {
            arr.add(t.getClass().getName() + ": " + messageOf(t));
        }
        Throwable cause = t.getCause();
        if (cause != null && cause != t && seen.put(cause, Boolean.TRUE) == null) {
            collect(cause, arr, seen, false);
        }
        if (t instanceof SQLException) {
            SQLException next = ((SQLException) t).getNextException();
            if (next != null && next != t && seen.put(next, Boolean.TRUE) == null) {
                collect(next, arr, seen, false);
            }
        }
    }

    private static String stack(Throwable t) {
        StringWriter sw = new StringWriter();
        try (PrintWriter pw = new PrintWriter(sw)) {
            t.printStackTrace(pw);
        }
        String s = sw.toString();
        return s.length() > MAX_STACK_CHARS ? s.substring(0, MAX_STACK_CHARS) + "\n... truncated" : s;
    }
}
