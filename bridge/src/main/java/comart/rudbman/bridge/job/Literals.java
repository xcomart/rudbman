package comart.rudbman.bridge.job;

import comart.rudbman.bridge.meta.Dialect;
import comart.rudbman.bridge.meta.Ident;

import java.math.BigDecimal;
import java.sql.ResultSet;
import java.sql.SQLException;
import java.sql.Types;
import java.time.LocalDateTime;
import java.time.ZoneOffset;
import java.time.ZonedDateTime;
import java.time.format.DateTimeFormatter;
import java.util.Locale;

/**
 * Renders one column of a result-set row as SQL text or as plain text.
 *
 * <p>Shared by {@link ExtractJob} and {@link BackupJob}: two implementations of
 * these rules would drift, and the drift would only show up as a script that
 * replays on one product and not on another.
 *
 * <p>Literals are rendered conservatively: text is single quoted with quotes
 * doubled, numbers go bare, dates go out as plain quoted strings rather than as
 * typed {@code DATE '…'} literals - every product accepts a string there, and
 * SQL Server rejects the typed form. Binary is the one place where a common form
 * does not exist, and the four spellings this bridge knows are listed on
 * {@link #hexLiteral}.
 */
final class Literals {

    private Literals() {
    }

    /**
     * Renders one column of the current row as a SQL literal.
     *
     * @param rs      the result set, positioned on a row
     * @param i       the one-based column index
     * @param type    the column's {@link Types} code
     * @param dialect the dialect the literal has to parse in
     * @return the literal text, or {@code NULL}
     * @throws SQLException if the driver fails
     */
    static String literal(ResultSet rs, int i, int type, Dialect dialect)
            throws SQLException {
        if (dialect == Dialect.ORACLE) {
            String oracle = oracleTemporal(rs, i, type);
            if (oracle != null) {
                return oracle;
            }
        }
        if (dialect == Dialect.H2) {
            String h2 = h2Literal(rs, i, type);
            if (h2 != null) {
                return h2;
            }
        }
        if (dialect == Dialect.SQLITE) {
            return sqliteLiteral(rs, i);
        }
        switch (type) {
            case Types.BINARY:
            case Types.VARBINARY:
            case Types.LONGVARBINARY:
            case Types.BLOB: {
                byte[] b = rs.getBytes(i);
                return b == null || rs.wasNull() ? "NULL" : hexLiteral(b, dialect);
            }
            case Types.TINYINT:
            case Types.SMALLINT:
            case Types.INTEGER:
            case Types.BIGINT: {
                // Read as text rather than as a long: an unsigned BIGINT does not
                // fit one, and the driver already knows how to spell its own.
                String s = rs.getString(i);
                return s == null || rs.wasNull() ? "NULL" : s;
            }
            case Types.DECIMAL:
            case Types.NUMERIC: {
                BigDecimal d = rs.getBigDecimal(i);
                // toPlainString, because toString switches to exponent notation
                // at some scales and not every parser accepts 1E+2 as a decimal.
                return d == null || rs.wasNull() ? "NULL" : d.toPlainString();
            }
            case Types.FLOAT:
            case Types.REAL:
            case Types.DOUBLE: {
                double d = rs.getDouble(i);
                if (rs.wasNull()) {
                    return "NULL";
                }
                if (Double.isNaN(d) || Double.isInfinite(d)) {
                    // No portable literal exists; a quoted form at least fails
                    // loudly instead of producing a wrong number silently.
                    return Ident.literal(Double.toString(d));
                }
                return Double.toString(d);
            }
            case Types.BIT:
            case Types.BOOLEAN: {
                if (type == Types.BIT && dialect.isMySqlFamily()
                        && rs.getMetaData().getPrecision(i) > 1) {
                    // MySQL's BIT(n) is n bits, not a truth value: BIT(8)
                    // holding 1010 is ten.
                    byte[] bits = rs.getBytes(i);
                    return bits == null || rs.wasNull() ? "NULL"
                            : "b'" + new java.math.BigInteger(1, bits).toString(2) + "'";
                }
                boolean b = rs.getBoolean(i);
                if (rs.wasNull()) {
                    return "NULL";
                }
                return booleanLiteral(b, dialect);
            }
            default: {
                String s = rs.getString(i);
                if (s == null || rs.wasNull()) {
                    return "NULL";
                }
                // A plain literal is in the database's code page on SQL Server,
                // and in its character set for Oracle's national types: text
                // outside it - Hangul in a Latin1 collation - would arrive as
                // question marks. N'...' is Unicode on both.
                boolean national = type == Types.NCHAR || type == Types.NVARCHAR
                        || type == Types.LONGNVARCHAR || type == Types.NCLOB;
                if (dialect == Dialect.SQLSERVER || (dialect == Dialect.ORACLE && national)) {
                    return "N" + Ident.literal(s);
                }
                if (dialect == Dialect.DB2 && (s.indexOf('\n') >= 0 || s.indexOf('\r') >= 0)) {
                    return db2Lines(s);
                }
                if (dialect.isMySqlFamily()) {
                    // The drivers hand a YEAR over as a DATE, January 1st of it,
                    // which a YEAR column refuses.
                    if (type == Types.DATE && s.length() >= 4
                            && "YEAR".equalsIgnoreCase(rs.getMetaData().getColumnTypeName(i))) {
                        return s.substring(0, 4);
                    }
                    return mysqlLiteral(s);
                }
                return Ident.literal(s);
            }
        }
    }

    /**
     * A Db2 string holding line breaks, as pieces joined around
     * {@code CHR(10)} and {@code CHR(13)}: the command line processor reads a
     * statement over several lines as one, a blank where each break was, and
     * would put that blank into the value.
     */
    static String db2Lines(String s) {
        StringBuilder sb = new StringBuilder();
        StringBuilder piece = new StringBuilder();
        for (int k = 0; k < s.length(); k++) {
            char ch = s.charAt(k);
            if (ch == '\n' || ch == '\r') {
                sb.append(Ident.literal(piece.toString())).append(" || CHR(")
                        .append(ch == '\n' ? 10 : 13).append(") || ");
                piece.setLength(0);
            } else {
                piece.append(ch);
            }
        }
        return sb.append(Ident.literal(piece.toString())).toString();
    }

    /**
     * H2's arrays and JSON, whose text a plain string does not turn back
     * into: an array reads as {@code [1, 2]}, which H2 does not parse, and a
     * string assigned to a JSON column becomes a JSON string holding that
     * text. {@code null} for every other column.
     */
    private static String h2Literal(ResultSet rs, int i, int type) throws SQLException {
        if (type == Types.ARRAY) {
            java.sql.Array a = rs.getArray(i);
            if (a == null || rs.wasNull()) {
                return "NULL";
            }
            StringBuilder sb = new StringBuilder("ARRAY[");
            Object[] items = (Object[]) a.getArray();
            for (int k = 0; k < items.length; k++) {
                if (k > 0) {
                    sb.append(", ");
                }
                Object v = items[k];
                if (v == null) {
                    sb.append("NULL");
                } else if (v instanceof Number || v instanceof Boolean) {
                    sb.append(v instanceof BigDecimal ? ((BigDecimal) v).toPlainString()
                            : v.toString().toUpperCase(Locale.ROOT));
                } else if (v instanceof byte[]) {
                    sb.append("X'").append(hex((byte[]) v)).append('\'');
                } else {
                    sb.append(Ident.literal(v.toString()));
                }
            }
            return sb.append(']').toString();
        }
        if ("JSON".equalsIgnoreCase(rs.getMetaData().getColumnTypeName(i))) {
            byte[] json = rs.getBytes(i);
            return json == null || rs.wasNull() ? "NULL"
                    : "JSON " + Ident.literal(new String(json, java.nio.charset.StandardCharsets.UTF_8));
        }
        return null;
    }

    /**
     * A SQLite value by what it is stored as, not by the column's declared
     * type, which SQLite does not hold its values to: an INTEGER column may
     * keep text, and writing that text as a number - or a number as text -
     * would change the value, or break the statement.
     */
    private static String sqliteLiteral(ResultSet rs, int i) throws SQLException {
        Object v = rs.getObject(i);
        if (v == null || rs.wasNull()) {
            return "NULL";
        }
        if (v instanceof Integer || v instanceof Long) {
            return v.toString();
        }
        if (v instanceof Double || v instanceof Float) {
            double d = ((Number) v).doubleValue();
            if (Double.isNaN(d) || Double.isInfinite(d)) {
                // SQLite has no literal for these; 9e999 is its own infinity.
                return Double.isNaN(d) ? "NULL" : d > 0 ? "9e999" : "-9e999";
            }
            return Double.toString(d);
        }
        if (v instanceof byte[]) {
            return hexLiteral((byte[]) v, Dialect.SQLITE);
        }
        return Ident.literal(v.toString());
    }

    /** ojdbc's own type codes, which {@link Types} has no names for. */
    private static final int ORACLE_TIMESTAMPTZ = -101;
    private static final int ORACLE_TIMESTAMPLTZ = -102;
    private static final int ORACLE_INTERVALYM = -103;
    private static final int ORACLE_INTERVALDS = -104;
    private static final int ORACLE_BINARY_FLOAT = 100;
    private static final int ORACLE_BINARY_DOUBLE = 101;

    private static final DateTimeFormatter ORACLE_DATE =
            DateTimeFormatter.ofPattern("uuuu-MM-dd HH:mm:ss", Locale.ROOT);
    private static final DateTimeFormatter ORACLE_TIMESTAMP =
            DateTimeFormatter.ofPattern("uuuu-MM-dd HH:mm:ss.SSSSSSSSS", Locale.ROOT);

    /**
     * Oracle's dates, times and intervals, and its binary floating-point
     * numbers, spelled so that no session setting changes what they mean: a
     * plain string goes through {@code NLS_DATE_FORMAT},
     * {@code NLS_TIMESTAMP_FORMAT} and {@code NLS_NUMERIC_CHARACTERS}, which
     * differ between one client and the next. {@code null} for every other
     * column, which the general rules write.
     */
    private static String oracleTemporal(ResultSet rs, int i, int type) throws SQLException {
        switch (type) {
            case Types.DATE:
            case Types.TIMESTAMP: {
                LocalDateTime v = rs.getObject(i, LocalDateTime.class);
                if (v == null || rs.wasNull()) {
                    return "NULL";
                }
                // ojdbc reports a DATE, which holds a time of day, as a TIMESTAMP.
                if ("DATE".equalsIgnoreCase(rs.getMetaData().getColumnTypeName(i))) {
                    return "TO_DATE('" + ORACLE_DATE.format(v) + "', 'YYYY-MM-DD HH24:MI:SS')";
                }
                return "TIMESTAMP '" + ORACLE_TIMESTAMP.format(v) + "'";
            }
            case Types.TIMESTAMP_WITH_TIMEZONE:
            case ORACLE_TIMESTAMPTZ: {
                ZonedDateTime v = rs.getObject(i, ZonedDateTime.class);
                if (v == null || rs.wasNull()) {
                    return "NULL";
                }
                String zone = v.getZone() instanceof ZoneOffset
                        ? ((ZoneOffset) v.getZone()).getId().replace("Z", "+00:00")
                        : v.getZone().getId();
                return "TIMESTAMP '" + ORACLE_TIMESTAMP.format(v) + " " + zone + "'";
            }
            case ORACLE_TIMESTAMPLTZ: {
                // The session's time zone is the reader's and the writer's
                // both: the instant goes over, written in UTC.
                java.sql.Timestamp v = rs.getTimestamp(i);
                if (v == null || rs.wasNull()) {
                    return "NULL";
                }
                return "TIMESTAMP '" + ORACLE_TIMESTAMP.format(
                        v.toInstant().atOffset(ZoneOffset.UTC)) + " +00:00'";
            }
            case ORACLE_INTERVALYM: {
                String v = rs.getString(i);
                return v == null || rs.wasNull() ? "NULL"
                        : "INTERVAL " + Ident.literal(v.strip()) + " YEAR(9) TO MONTH";
            }
            case ORACLE_INTERVALDS: {
                String v = rs.getString(i);
                return v == null || rs.wasNull() ? "NULL"
                        : "INTERVAL " + Ident.literal(v.strip()) + " DAY(9) TO SECOND(9)";
            }
            case ORACLE_BINARY_FLOAT:
            case ORACLE_BINARY_DOUBLE: {
                double d = rs.getDouble(i);
                if (rs.wasNull()) {
                    return "NULL";
                }
                if (Double.isNaN(d)) {
                    return type == ORACLE_BINARY_FLOAT ? "BINARY_FLOAT_NAN" : "BINARY_DOUBLE_NAN";
                }
                if (Double.isInfinite(d)) {
                    String inf = type == ORACLE_BINARY_FLOAT ? "BINARY_FLOAT_INFINITY"
                            : "BINARY_DOUBLE_INFINITY";
                    return d < 0 ? "-" + inf : inf;
                }
                // The f/d suffix keeps a float's value from rounding through NUMBER.
                return (type == ORACLE_BINARY_FLOAT ? Float.toString((float) d)
                        : Double.toString(d)) + (type == ORACLE_BINARY_FLOAT ? "f" : "d");
            }
            default:
                return null;
        }
    }

    /**
     * A MySQL or MariaDB string literal: a backslash there escapes what follows
     * it - the rows are read under a mode that keeps it so, see
     * {@code Ddl.dataPrologue} - and a raw NUL is spelled out, as mysqldump
     * writes them.
     */
    static String mysqlLiteral(String s) {
        StringBuilder sb = new StringBuilder(s.length() + 2).append('\'');
        for (int k = 0; k < s.length(); k++) {
            char ch = s.charAt(k);
            switch (ch) {
                case '\\': sb.append("\\\\"); break;
                case '\'':  sb.append("''"); break;
                case '\0':  sb.append("\\0"); break;
                default:    sb.append(ch);
            }
        }
        return sb.append('\'').toString();
    }

    /**
     * @param b       the value
     * @param dialect the target dialect
     * @return {@code TRUE}/{@code FALSE} where the product has a boolean type,
     *         {@code 1}/{@code 0} where it does not. Oracle before 23ai, SQL
     *         Server and SQLite are in the second group.
     */
    static String booleanLiteral(boolean b, Dialect dialect) {
        switch (dialect) {
            case ORACLE:
            case SQLSERVER:
            case SQLITE:
            case MYSQL:
            case MARIADB:
                return b ? "1" : "0";
            default:
                return b ? "TRUE" : "FALSE";
        }
    }

    /**
     * Renders binary data.
     *
     * <p>There is no common spelling, so five are known:
     * {@code 0x…} for SQL Server, {@code '\x…'} for PostgreSQL's bytea input,
     * {@code HEXTORAW('…')} for Oracle, {@code BX'…'} for Db2 and the standard
     * {@code X'…'} for everything else, which is what H2, MySQL and SQLite
     * accept. A product
     * that reaches {@link Dialect#OTHER} gets the standard form and may not take
     * it.
     *
     * @param b       the bytes
     * @param dialect the target dialect
     * @return the literal text
     */
    static String hexLiteral(byte[] b, Dialect dialect) {
        String hex = hex(b);
        switch (dialect) {
            case SQLSERVER:  return "0x" + hex;
            case POSTGRESQL: return "'\\x" + hex + "'";
            case ORACLE:     return "HEXTORAW('" + hex + "')";
            // X'…' is a character string FOR BIT DATA there, which a BINARY,
            // VARBINARY or BLOB column refuses; BX'…' is a binary string.
            case DB2:        return "BX'" + hex + "'";
            default:         return "X'" + hex + "'";
        }
    }

    /**
     * Renders one column of the current row as plain text, for CSV and templates.
     *
     * @param rs   the result set, positioned on a row
     * @param i    the one-based column index
     * @param type the column's {@link Types} code
     * @return the text, or {@code null} for SQL NULL
     * @throws SQLException if the driver fails
     */
    static String text(ResultSet rs, int i, int type) throws SQLException {
        switch (type) {
            case Types.BINARY:
            case Types.VARBINARY:
            case Types.LONGVARBINARY:
            case Types.BLOB: {
                byte[] b = rs.getBytes(i);
                return b == null || rs.wasNull() ? null : hex(b);
            }
            default: {
                String s = rs.getString(i);
                return s == null || rs.wasNull() ? null : s;
            }
        }
    }

    /**
     * @param b the bytes
     * @return their upper-case hexadecimal spelling, with no prefix
     */
    static String hex(byte[] b) {
        StringBuilder sb = new StringBuilder(b.length * 2);
        for (byte x : b) {
            sb.append(Character.forDigit((x >> 4) & 0xf, 16))
                    .append(Character.forDigit(x & 0xf, 16));
        }
        return sb.toString().toUpperCase(Locale.ROOT);
    }
}
