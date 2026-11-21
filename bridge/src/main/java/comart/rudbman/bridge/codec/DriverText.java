package comart.rudbman.bridge.codec;

import java.sql.ResultSet;
import java.sql.SQLException;
import java.sql.Timestamp;
import java.sql.Types;
import java.time.Duration;

/**
 * A column's value as text, as {@link ResultSet#getString} gives it, mended
 * where a driver is known to get it wrong.
 *
 * <p>One driver so far. MariaDB Connector/J (3.4 through 4.0.0 at least,
 * against MariaDB and MySQL servers alike; upstream CONJ-1385) drops the
 * leading zeros of a fraction of a second below six digits of precision:
 * <ul>
 *   <li>{@code DATETIME(n)} and {@code TIMESTAMP(n)}, over either protocol: a
 *       {@code DATETIME(3)} holding {@code .064} reads as {@code ….64000},
 *       {@code .006} as {@code ….6000}, a {@code DATETIME(5)}'s {@code .00006}
 *       as {@code ….00060}. {@code getTimestamp} has the value right.</li>
 *   <li>{@code TIME(n)} over the binary protocol
 *       ({@code useServerPrepStmts=true}): {@code .005} as {@code ….5000}. Over
 *       the text protocol the driver hands on the server's own text, which is
 *       right. The value is read as a {@link Duration}, which a {@code TIME}
 *       beyond a day or below zero fits and a {@code LocalTime} does not.</li>
 * </ul>
 * Every reader of a value as text goes through here - the grid's batches, a
 * script's literals, an extract's text - because each would hand the wrong
 * time on: shown in the grid and saved back by an edit, written into a backup
 * that restores another time.
 *
 * <p>Only the fraction is mended, and only where it does not say what the
 * value holds: the date and the time of day are the driver's text as it is,
 * and so is a fraction it got right.
 */
public final class DriverText {

    /** The package every MariaDB Connector/J result set lives in. */
    private static final String MARIADB = "org.mariadb.jdbc.";

    /** {@code 10^n} for the digits a fraction of a second can have. */
    private static final long[] POW10 = {
        1L, 10L, 100L, 1_000L, 10_000L, 100_000L, 1_000_000L, 10_000_000L, 100_000_000L,
        1_000_000_000L,
    };

    private DriverText() {
    }

    /**
     * @param rs      the result set
     * @param sqlType the column's {@link Types} code
     * @return whether values of this type from this result set are mended
     */
    public static boolean mends(ResultSet rs, int sqlType) {
        return (sqlType == Types.TIMESTAMP || sqlType == Types.TIME)
                && rs.getClass().getName().startsWith(MARIADB);
    }

    /**
     * Reads one column of the current row as text.
     *
     * @param rs      the result set, positioned on a row
     * @param column  the one-based column index
     * @param sqlType the column's {@link Types} code
     * @return the text, or {@code null} for SQL NULL; {@link ResultSet#wasNull}
     *         says the same thing it would after {@code getString}
     * @throws SQLException if the driver fails
     */
    public static String of(ResultSet rs, int column, int sqlType) throws SQLException {
        String text = rs.getString(column);
        if (text == null || !mends(rs, sqlType) || text.indexOf('.') < 0) {
            return text;
        }
        Integer nanos;
        try {
            nanos = sqlType == Types.TIME ? timeNanos(rs, column) : timestampNanos(rs, column);
        } catch (SQLException | RuntimeException e) {
            // A zero date and the like: the driver's text is all there is.
            return text;
        }
        return nanos == null ? text : mended(text, nanos);
    }

    /** @return the fraction of a second a TIMESTAMP holds, in nanoseconds */
    private static Integer timestampNanos(ResultSet rs, int column) throws SQLException {
        Timestamp value = rs.getTimestamp(column);
        return value == null ? null : value.getNanos();
    }

    /**
     * @return the fraction of a second a TIME holds, in nanoseconds, of its
     *         magnitude: {@code -00:00:01.5} is written {@code .5}
     */
    private static Integer timeNanos(ResultSet rs, int column) throws SQLException {
        Duration value = rs.getObject(column, Duration.class);
        return value == null ? null : value.abs().getNano();
    }

    /**
     * @param text  a timestamp as a driver wrote it
     * @param nanos the fraction of a second the value holds, in nanoseconds
     * @return {@code text}, its fraction rewritten where it is not
     *         {@code nanos}: as microseconds where that is exact, six digits
     *         as the driver writes a fraction it gets right, else nine
     */
    static String mended(String text, int nanos) {
        int dot = text.indexOf('.');
        if (dot < 0) {
            return text;
        }
        int end = dot + 1;
        while (end < text.length() && Character.isDigit(text.charAt(end))) {
            end++;
        }
        int digits = end - dot - 1;
        if (digits == 0 || digits > 9) {
            return text;
        }
        long written = Long.parseLong(text.substring(dot + 1, end)) * POW10[9 - digits];
        if (written == nanos) {
            return text;
        }
        String fraction = nanos % 1000 == 0 ? String.format("%06d", nanos / 1000)
                : String.format("%09d", nanos);
        return text.substring(0, dot + 1) + fraction + text.substring(end);
    }
}
