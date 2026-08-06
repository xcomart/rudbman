package comart.rudbman.bridge.job;

import comart.rudbman.bridge.meta.Dialect;
import comart.rudbman.bridge.meta.Ident;

import java.math.BigDecimal;
import java.sql.ResultSet;
import java.sql.SQLException;
import java.sql.Types;
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
                boolean b = rs.getBoolean(i);
                if (rs.wasNull()) {
                    return "NULL";
                }
                return booleanLiteral(b, dialect);
            }
            default: {
                String s = rs.getString(i);
                return s == null || rs.wasNull() ? "NULL" : Ident.literal(s);
            }
        }
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
     * <p>There is no common spelling, so four are known:
     * {@code 0x…} for SQL Server, {@code '\x…'} for PostgreSQL's bytea input,
     * {@code HEXTORAW('…')} for Oracle and the standard {@code X'…'} for
     * everything else, which is what H2, MySQL, SQLite and Db2 accept. A product
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
