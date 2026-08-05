package comart.rudbman.bridge.meta;

import java.sql.DatabaseMetaData;
import java.util.Arrays;
import java.util.HashSet;
import java.util.Locale;
import java.util.Set;

/**
 * Identifier quoting for generated SQL.
 *
 * <p>Quoting is decided per identifier rather than applied to all of them.
 * Quoting everything produces DDL nobody wants to read - {@code "APP"."ORDERS"
 * ("ID", "NAME")} - while quoting nothing breaks the moment a column is called
 * {@code order} or holds a space. The rule here is: quote only when leaving the
 * quotes off would change which object the name refers to, or would not parse.
 *
 * <p>Three things force quotes:
 * <ul>
 *   <li><b>Shape.</b> A character outside the portable identifier alphabet
 *       (letters, digits, underscore, plus whatever
 *       {@link DatabaseMetaData#getExtraNameCharacters()} adds), or a leading
 *       character that is not a letter.</li>
 *   <li><b>Case.</b> An unquoted identifier is folded to the storage case, so on
 *       a server that folds to upper case any name that is not already upper
 *       case has to be quoted to survive. This is what makes {@code "order"} on
 *       H2 and {@code "MixedCase"} on PostgreSQL come out quoted.</li>
 *   <li><b>Reserved words.</b> {@link DatabaseMetaData#getSQLKeywords()} lists
 *       only the vendor's additions <em>beyond</em> the SQL standard - H2
 *       answers {@code LIMIT,TOP,…} and not {@code ORDER} - so the standard
 *       reserved words are carried here as a constant and unioned with whatever
 *       the driver reports.</li>
 * </ul>
 *
 * <p>A server that answers a single space to
 * {@link DatabaseMetaData#getIdentifierQuoteString()} does not support quoted
 * identifiers at all; then nothing is ever quoted, because emitting a quote
 * character would be worse than emitting an ambiguous name.
 *
 * <p>Public rather than package private because the script extractor in
 * {@code job/} generates {@code SELECT} and {@code INSERT} statements that have
 * to agree with the generated DDL character for character; two implementations
 * of these rules would drift.
 */
public final class Ident {

    /**
     * SQL:2011 reserved words, plus the handful from SQL-92 that vendors still
     * reject. Drivers report only their own extensions, so without this list a
     * column named {@code ORDER} or {@code USER} would be emitted bare.
     */
    private static final Set<String> RESERVED = new HashSet<>(Arrays.asList(
            "ABS", "ALL", "ALLOCATE", "ALTER", "AND", "ANY", "ARE", "ARRAY", "AS", "ASENSITIVE",
            "ASYMMETRIC", "AT", "ATOMIC", "AUTHORIZATION", "AVG", "BEGIN", "BETWEEN", "BIGINT",
            "BINARY", "BLOB", "BOOLEAN", "BOTH", "BY", "CALL", "CALLED", "CASCADED", "CASE",
            "CAST", "CEIL", "CEILING", "CHAR", "CHARACTER", "CHECK", "CLOB", "CLOSE", "COALESCE",
            "COLLATE", "COLLECT", "COLUMN", "COMMIT", "CONDITION", "CONNECT", "CONSTRAINT",
            "CONVERT", "CORR", "CORRESPONDING", "COUNT", "COVAR_POP", "COVAR_SAMP", "CREATE",
            "CROSS", "CUBE", "CUME_DIST", "CURRENT", "CURRENT_CATALOG", "CURRENT_DATE",
            "CURRENT_DEFAULT_TRANSFORM_GROUP", "CURRENT_PATH", "CURRENT_ROLE", "CURRENT_SCHEMA",
            "CURRENT_TIME", "CURRENT_TIMESTAMP", "CURRENT_USER", "CURSOR", "CYCLE", "DATE",
            "DAY", "DEALLOCATE", "DEC", "DECIMAL", "DECLARE", "DEFAULT", "DELETE", "DENSE_RANK",
            "DEREF", "DESCRIBE", "DETERMINISTIC", "DISCONNECT", "DISTINCT", "DOUBLE", "DROP",
            "DYNAMIC", "EACH", "ELEMENT", "ELSE", "END", "END-EXEC", "ESCAPE", "EVERY", "EXCEPT",
            "EXEC", "EXECUTE", "EXISTS", "EXP", "EXTERNAL", "EXTRACT", "FALSE", "FETCH", "FILTER",
            "FIRST_VALUE", "FLOAT", "FLOOR", "FOR", "FOREIGN", "FREE", "FROM", "FULL", "FUNCTION",
            "FUSION", "GET", "GLOBAL", "GRANT", "GROUP", "GROUPING", "HAVING", "HOLD", "HOUR",
            "IDENTITY", "IN", "INDICATOR", "INNER", "INOUT", "INSENSITIVE", "INSERT", "INT",
            "INTEGER", "INTERSECT", "INTERSECTION", "INTERVAL", "INTO", "IS", "JOIN", "LAG",
            "LANGUAGE", "LARGE", "LAST_VALUE", "LATERAL", "LEAD", "LEADING", "LEFT", "LIKE",
            "LN", "LOCAL", "LOCALTIME", "LOCALTIMESTAMP", "LOWER", "MATCH", "MAX", "MEMBER",
            "MERGE", "METHOD", "MIN", "MINUTE", "MOD", "MODIFIES", "MODULE", "MONTH",
            "MULTISET", "NATIONAL", "NATURAL", "NCHAR", "NCLOB", "NEW", "NO", "NONE", "NORMALIZE",
            "NOT", "NTH_VALUE", "NTILE", "NULL", "NULLIF", "NUMERIC", "OCTET_LENGTH", "OF",
            "OFFSET", "OLD", "ON", "ONLY", "OPEN", "OR", "ORDER", "OUT", "OUTER", "OVER",
            "OVERLAPS", "OVERLAY", "PARAMETER", "PARTITION", "PERCENT_RANK", "PERCENTILE_CONT",
            "PERCENTILE_DISC", "POSITION", "POWER", "PRECISION", "PREPARE", "PRIMARY",
            "PROCEDURE", "RANGE", "RANK", "READS", "REAL", "RECURSIVE", "REF", "REFERENCES",
            "REFERENCING", "RELEASE", "RESULT", "RETURN", "RETURNS", "REVOKE", "RIGHT",
            "ROLLBACK", "ROLLUP", "ROW", "ROW_NUMBER", "ROWS", "SAVEPOINT", "SCOPE", "SCROLL",
            "SEARCH", "SECOND", "SELECT", "SENSITIVE", "SESSION_USER", "SET", "SIMILAR",
            "SMALLINT", "SOME", "SPECIFIC", "SPECIFICTYPE", "SQL", "SQLEXCEPTION", "SQLSTATE",
            "SQLWARNING", "SQRT", "START", "STATIC", "STDDEV_POP", "STDDEV_SAMP", "SUBMULTISET",
            "SUBSTRING", "SUM", "SYMMETRIC", "SYSTEM", "SYSTEM_USER", "TABLE", "TABLESAMPLE",
            "THEN", "TIME", "TIMESTAMP", "TIMEZONE_HOUR", "TIMEZONE_MINUTE", "TO", "TRAILING",
            "TRANSLATE", "TRANSLATION", "TREAT", "TRIGGER", "TRIM", "TRUE", "TRUNCATE", "UESCAPE",
            "UNION", "UNIQUE", "UNKNOWN", "UNNEST", "UPDATE", "UPPER", "USER", "USING", "VALUE",
            "VALUES", "VAR_POP", "VAR_SAMP", "VARBINARY", "VARCHAR", "VARYING", "WHEN",
            "WHENEVER", "WHERE", "WIDTH_BUCKET", "WINDOW", "WITH", "WITHIN", "WITHOUT", "YEAR"));

    private final String quote;
    private final boolean quotingSupported;
    private final boolean foldsUpper;
    private final boolean foldsLower;
    private final String extraChars;
    private final Set<String> keywords;

    private Ident(String quote, boolean quotingSupported, boolean foldsUpper, boolean foldsLower,
                  String extraChars, Set<String> keywords) {
        this.quote = quote;
        this.quotingSupported = quotingSupported;
        this.foldsUpper = foldsUpper;
        this.foldsLower = foldsLower;
        this.extraChars = extraChars;
        this.keywords = keywords;
    }

    /**
     * Reads the quoting rules off a connection.
     *
     * <p>Every accessor is wrapped, for the same reason {@code SESSION_INFO}
     * wraps its own: a driver that throws from one capability method must not
     * cost the caller the whole answer. A missing answer degrades to the safe
     * side - unknown quote string means no quoting, unknown folding means no
     * case-driven quoting.
     *
     * @param dbm the connection metadata
     * @return the rules for this connection
     */
    public static Ident of(DatabaseMetaData dbm) {
        String q = str(dbm, 'q');
        boolean supported = q != null && !q.isEmpty() && !q.trim().isEmpty();
        String extra = str(dbm, 'e');
        Set<String> kw = new HashSet<>(RESERVED);
        String vendor = str(dbm, 'k');
        if (vendor != null) {
            for (String w : vendor.split(",")) {
                String t = w.trim();
                if (!t.isEmpty()) {
                    kw.add(t.toUpperCase(Locale.ROOT));
                }
            }
        }
        return new Ident(supported ? q.trim() : "\"", supported,
                flag(dbm, 'u'), flag(dbm, 'l'),
                extra == null ? "" : extra, kw);
    }

    private static String str(DatabaseMetaData dbm, char which) {
        try {
            switch (which) {
                case 'q': return dbm.getIdentifierQuoteString();
                case 'e': return dbm.getExtraNameCharacters();
                default:  return dbm.getSQLKeywords();
            }
        } catch (Exception | AbstractMethodError e) {
            return null;
        }
    }

    private static boolean flag(DatabaseMetaData dbm, char which) {
        try {
            return which == 'u' ? dbm.storesUpperCaseIdentifiers()
                    : dbm.storesLowerCaseIdentifiers();
        } catch (Exception | AbstractMethodError e) {
            return false;
        }
    }

    /**
     * Renders one identifier, quoted only if it has to be.
     *
     * @param id the identifier, may be {@code null}
     * @return the SQL text, or {@code null} when {@code id} was {@code null}
     */
    public String q(String id) {
        if (id == null) {
            return null;
        }
        if (!quotingSupported || !needsQuote(id)) {
            return id;
        }
        // Doubling the quote character is the escape in both the SQL standard
        // (") and in MySQL's backtick mode.
        return quote + id.replace(quote, quote + quote) + quote;
    }

    private boolean needsQuote(String id) {
        if (id.isEmpty()) {
            return true;
        }
        char first = id.charAt(0);
        if (!Character.isLetter(first)) {
            return true;
        }
        for (int i = 0; i < id.length(); i++) {
            char c = id.charAt(i);
            boolean plain = c == '_' || (c < 128 && (Character.isLetterOrDigit(c)))
                    || extraChars.indexOf(c) >= 0;
            if (!plain) {
                return true;
            }
        }
        if (foldsUpper && !id.equals(id.toUpperCase(Locale.ROOT))) {
            return true;
        }
        if (foldsLower && !id.equals(id.toLowerCase(Locale.ROOT))) {
            return true;
        }
        return keywords.contains(id.toUpperCase(Locale.ROOT));
    }

    /**
     * Renders a dotted name, skipping absent parts.
     *
     * @param parts name parts from outermost to innermost; {@code null} and
     *              empty parts are dropped
     * @return the qualified SQL name
     */
    public String qualify(String... parts) {
        StringBuilder sb = new StringBuilder();
        for (String p : parts) {
            if (p == null || p.isEmpty()) {
                continue;
            }
            if (sb.length() > 0) {
                sb.append('.');
            }
            sb.append(q(p));
        }
        return sb.toString();
    }

    /**
     * Renders a SQL string literal.
     *
     * @param s the text, must not be {@code null}
     * @return the quoted literal with embedded quotes doubled
     */
    public static String literal(String s) {
        return "'" + s.replace("'", "''") + "'";
    }
}
