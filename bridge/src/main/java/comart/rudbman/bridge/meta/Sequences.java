package comart.rudbman.bridge.meta;

import com.google.gson.JsonArray;
import com.google.gson.JsonObject;
import comart.rudbman.bridge.Json;

import java.sql.Connection;
import java.sql.DatabaseMetaData;
import java.sql.ResultSet;
import java.sql.SQLException;
import java.util.Locale;
import java.util.Map;

/**
 * {@code DESCRIBE kind: "sequences"}.
 *
 * <p>The one metadata kind with no JDBC behind it. {@link DatabaseMetaData} has
 * accessors for tables, columns, keys, indexes, procedures, functions and user
 * types, and none at all for sequences - they were standardised in SQL:2003,
 * years after the JDBC metadata interface was fixed, and no later JDBC revision
 * went back for them. So this is a vendor catalogue query per product.
 *
 * <p><b>An empty list is a correct answer.</b> Most of the world's databases
 * have no sequences: MySQL has none at all, SQLite has none, and a server whose
 * product name is not recognised is simply not asked. None of that is an error
 * condition, and the explorer tree should show an empty branch, not a failure.
 * A query that is attempted and rejected - the object is not there, or the user
 * cannot read it - lands in the same place, via {@link Attempt}.
 *
 * <p>Row shape is normalised across products by reading each output key from the
 * first metadata label that is present, so that one reader serves the SQL:2003
 * {@code INFORMATION_SCHEMA.SEQUENCES} layout and Oracle's older
 * {@code ALL_SEQUENCES} layout alike. Numbers stay strings: an Oracle sequence
 * maximum is {@code NUMBER(28)}, which does not fit a {@code long}, and the UI
 * only prints it.
 */
public final class Sequences {

    /**
     * H2 2.x, which follows SQL:2003 and adds {@code BASE_VALUE},
     * {@code CACHE} and {@code REMARKS}.
     * Source: H2 2.x documentation, "Information Schema - SEQUENCES".
     */
    private static final String H2_SQL = "SELECT * FROM INFORMATION_SCHEMA.SEQUENCES";

    /**
     * PostgreSQL exposes the standard view. It lists only sequences the current
     * user owns or has a privilege on, so a short answer here is a permission
     * fact rather than a missing one.
     * Source: PostgreSQL documentation, "The Information Schema - sequences".
     */
    private static final String POSTGRESQL_SQL = "SELECT * FROM information_schema.sequences";

    /**
     * Oracle has no information schema; {@code ALL_SEQUENCES} is the accessible
     * subset of {@code DBA_SEQUENCES}. It has no {@code START WITH} column - the
     * original start value is not retained - so {@code start_value} comes back
     * null and {@code LAST_NUMBER} feeds {@code current_value}.
     * Source: Oracle Database Reference, "ALL_SEQUENCES".
     */
    private static final String ORACLE_SQL = "SELECT * FROM ALL_SEQUENCES";

    /**
     * MariaDB 10.3 added {@code CREATE SEQUENCE}, but how sequences are exposed
     * to the information schema has moved between releases - some builds list
     * them only in {@code INFORMATION_SCHEMA.TABLES} with
     * {@code TABLE_TYPE = 'SEQUENCE'}. This is therefore a probe: if the view is
     * there it answers, and if it is not, {@link Attempt} turns the rejection
     * into the empty list, which is the right answer for a build without it.
     * Source: MariaDB Server documentation, "SEQUENCE Overview".
     */
    private static final String MARIADB_SQL = "SELECT * FROM information_schema.SEQUENCES";

    private Sequences() {
    }

    /**
     * Lists sequences.
     *
     * <p>Request members: {@code catalog}, {@code schema} and {@code name}, all
     * optional and all matched exactly. Filtering happens here rather than in a
     * {@code WHERE} clause because the column holding the schema name differs
     * per product, and sequence catalogues are small.
     *
     * @param conn the live connection
     * @param dbm  the connection metadata, for the product name
     * @param req  the request body
     * @return the sequence list, empty when the product has no sequences
     */
    public static JsonArray of(Connection conn, DatabaseMetaData dbm, JsonObject req) {
        String sql = sqlFor(Dialect.of(dbm));
        if (sql == null) {
            return new JsonArray();
        }
        String catalog = Json.str(req, "catalog");
        String schema = Json.str(req, "schema");
        String name = Json.str(req, "name");
        return Attempt.run(conn, sql, st -> {
            JsonArray arr = new JsonArray();
            try (ResultSet rs = st.executeQuery(sql)) {
                RsView v = new RsView(rs);
                while (rs.next()) {
                    JsonObject o = row(v);
                    if (matches(o, "catalog", catalog) && matches(o, "schema", schema)
                            && matches(o, "name", name)) {
                        arr.add(o);
                    }
                }
            }
            return arr;
        }, new JsonArray());
    }

    private static String sqlFor(Dialect dialect) {
        switch (dialect) {
            case H2:         return H2_SQL;
            case POSTGRESQL: return POSTGRESQL_SQL;
            case ORACLE:     return ORACLE_SQL;
            case MARIADB:    return MARIADB_SQL;
            default:         return null;
        }
    }

    /**
     * Every label this reader knows, across all four catalogue layouts.
     *
     * <p>The row is fetched as a set rather than field by field because a row
     * has to be read left to right - see {@link RsView} - and these products do
     * not agree on what that order is: H2 puts {@code BASE_VALUE} before
     * {@code CACHE} and Oracle puts {@code CACHE_SIZE} before
     * {@code LAST_NUMBER}, so the same two output fields want opposite read
     * orders. {@link RsView#strs} sorts by whatever positions this result set
     * turned out to have, which leaves the composition below free to list the
     * fields in the order the UI wants them.
     */
    private static final String[] LABELS = {
        "SEQUENCE_CATALOG", "SEQUENCE_SCHEMA", "SEQUENCE_OWNER", "DB_NAME",
        "SEQUENCE_NAME", "DATA_TYPE", "START_VALUE", "START_WITH",
        "MINIMUM_VALUE", "MIN_VALUE", "MAXIMUM_VALUE", "MAX_VALUE",
        "INCREMENT", "INCREMENT_BY", "CYCLE_OPTION", "CYCLE_FLAG", "CYCLE",
        "CACHE", "CACHE_SIZE", "BASE_VALUE", "LAST_NUMBER", "CURRENT_VALUE",
        "REMARKS", "COMMENT",
    };

    private static JsonObject row(RsView v) throws SQLException {
        Map<String, String> r = v.strs(LABELS);
        JsonObject o = new JsonObject();
        o.addProperty("catalog", first(r, "SEQUENCE_CATALOG"));
        o.addProperty("schema", first(r, "SEQUENCE_SCHEMA", "SEQUENCE_OWNER", "DB_NAME"));
        o.addProperty("name", first(r, "SEQUENCE_NAME"));
        o.addProperty("data_type", first(r, "DATA_TYPE"));
        o.addProperty("start_value", first(r, "START_VALUE", "START_WITH"));
        o.addProperty("min_value", first(r, "MINIMUM_VALUE", "MIN_VALUE"));
        o.addProperty("max_value", first(r, "MAXIMUM_VALUE", "MAX_VALUE"));
        o.addProperty("increment", first(r, "INCREMENT", "INCREMENT_BY"));
        o.addProperty("cycle", yesNo(first(r, "CYCLE_OPTION", "CYCLE_FLAG", "CYCLE")));
        o.addProperty("cache", first(r, "CACHE", "CACHE_SIZE"));
        o.addProperty("current_value", first(r, "BASE_VALUE", "LAST_NUMBER", "CURRENT_VALUE"));
        o.addProperty("remarks", first(r, "REMARKS", "COMMENT"));
        return o;
    }

    /** @return the value of the first label this row actually carried */
    private static String first(Map<String, String> row, String... labels) {
        for (String label : labels) {
            String s = row.get(label);
            if (s != null) {
                return s;
            }
        }
        return null;
    }

    /**
     * Reads the several spellings of a cycle flag: {@code YES}/{@code NO} in the
     * information schema, {@code Y}/{@code N} in Oracle, {@code 0}/{@code 1} in
     * a few drivers that render a boolean as a number.
     */
    private static Boolean yesNo(String s) {
        if (s == null) {
            return null;
        }
        String t = s.trim().toUpperCase(Locale.ROOT);
        if (t.equals("YES") || t.equals("Y") || t.equals("TRUE") || t.equals("1")) {
            return Boolean.TRUE;
        }
        if (t.equals("NO") || t.equals("N") || t.equals("FALSE") || t.equals("0")) {
            return Boolean.FALSE;
        }
        return null;
    }

    private static boolean matches(JsonObject o, String key, String want) {
        if (want == null || want.isEmpty()) {
            return true;
        }
        com.google.gson.JsonElement e = o.get(key);
        return e != null && !e.isJsonNull() && want.equals(e.getAsString());
    }

}
