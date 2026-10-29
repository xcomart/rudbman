package comart.rudbman.bridge.meta;

import com.google.gson.JsonArray;
import com.google.gson.JsonObject;
import comart.rudbman.bridge.Json;

import java.sql.Connection;
import java.sql.DatabaseMetaData;
import java.sql.PreparedStatement;
import java.sql.ResultSet;
import java.sql.SQLException;
import java.util.ArrayList;
import java.util.HashMap;
import java.util.List;
import java.util.Locale;
import java.util.Map;

/**
 * {@code DESCRIBE kind: "sequences"}.
 *
 * <p>The one metadata kind with no JDBC behind it. {@link DatabaseMetaData} has
 * accessors for tables, columns, keys, indexes, procedures, functions and user
 * types, and none at all for sequences - they were standardized in SQL:2003,
 * years after the JDBC metadata interface was fixed, and no later JDBC revision
 * went back for them. So this is a vendor catalog query per product.
 *
 * <p><b>An empty list is a correct answer.</b> Many databases have no
 * sequences: MySQL has none at all, SQLite has none, and a server whose
 * product name is not recognized is simply not asked. None of that is an error
 * condition, and the explorer tree should show an empty branch, not a failure.
 * A query that is attempted and rejected - the object is not there, or the user
 * cannot read it - lands in the same place, via {@link Attempt}.
 *
 * <p>Row shape is normalized across products by reading each output key from the
 * first metadata label that is present, so that one reader serves the SQL:2003
 * {@code INFORMATION_SCHEMA.SEQUENCES} layout and Oracle's older
 * {@code ALL_SEQUENCES} layout alike, and the catalogs of SQL Server and Db2
 * are relabeled into the first. MariaDB is read its own way; see
 * {@link #mariadb}. Numbers stay strings: an Oracle sequence
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
     * PostgreSQL 10's {@code pg_sequences}, which also has the cache size and
     * the last value handed out - the standard view has neither - with the
     * columns named as the standard view names them. A sequence an identity
     * column owns is left out, as the standard view leaves it out: it is part of
     * its table. Before 10 the query fails and the standard view answers.
     * Source: PostgreSQL documentation, "pg_sequences".
     */
    private static final String POSTGRESQL_CATALOG_SQL = "SELECT current_database()"
            + " AS sequence_catalog, s.schemaname AS sequence_schema,"
            + " s.sequencename AS sequence_name, s.data_type::text AS data_type,"
            + " s.start_value, s.min_value AS minimum_value, s.max_value AS maximum_value,"
            + " s.increment_by AS increment,"
            + " CASE WHEN s.cycle THEN 'YES' ELSE 'NO' END AS cycle_option,"
            + " s.cache_size AS cache, s.last_value AS current_value,"
            + " pg_catalog.obj_description(c.oid, 'pg_class') AS remarks"
            + " FROM pg_catalog.pg_sequences s"
            + " JOIN pg_catalog.pg_namespace n ON n.nspname = s.schemaname"
            + " JOIN pg_catalog.pg_class c ON c.relnamespace = n.oid AND c.relname = s.sequencename"
            + " WHERE NOT EXISTS (SELECT 1 FROM pg_catalog.pg_depend d"
            + " WHERE d.classid = 'pg_catalog.pg_class'::regclass AND d.objid = c.oid"
            + " AND d.deptype = 'i')";

    /**
     * Oracle has no information schema; {@code ALL_SEQUENCES} is the accessible
     * subset of {@code DBA_SEQUENCES}. It has no {@code START WITH} column - the
     * original start value is not retained - so {@code start_value} comes back
     * null and {@code LAST_NUMBER} feeds {@code current_value}.
     * Source: Oracle Database Reference, "ALL_SEQUENCES".
     */
    private static final String ORACLE_SQL = "SELECT * FROM ALL_SEQUENCES";

    /**
     * The same, less the {@code ISEQ$$} sequences Oracle 12c makes for identity
     * columns: they are part of their table, as PostgreSQL's are. Before 12c
     * there is no {@code ALL_TAB_IDENTITY_COLS}, the query fails, and the plain
     * one answers.
     * Source: Oracle Database Reference, "ALL_TAB_IDENTITY_COLS".
     */
    private static final String ORACLE_OWN_SQL = "SELECT s.* FROM ALL_SEQUENCES s"
            + " WHERE NOT EXISTS (SELECT 1 FROM ALL_TAB_IDENTITY_COLS i"
            + " WHERE i.OWNER = s.SEQUENCE_OWNER AND i.SEQUENCE_NAME = s.SEQUENCE_NAME)";

    /**
     * Db2's {@code SYSCAT.SEQUENCES}, under the standard view's labels.
     * {@code SEQTYPE} {@code 'I'} is the sequence behind an identity column,
     * left out as PostgreSQL's and Oracle's are. A {@code DECIMAL} sequence
     * keeps its precision in a column of its own, and is written back with it.
     * {@code NEXTCACHEFIRSTVALUE} is where the next cache block starts, not the
     * value last handed out, so there is no current value to report. A schema
     * the server created implicitly is stored blank-padded to eight characters
     * - {@code 'RUDBMAN '} - which SQL compares equal to the bare name and Java
     * does not, so it is trimmed here, where the filter below reads it.
     * Source: Db2 12.1 documentation, "SYSCAT.SEQUENCES catalog view".
     */
    private static final String DB2_SQL = "SELECT RTRIM(s.SEQSCHEMA) AS SEQUENCE_SCHEMA,"
            + " s.SEQNAME AS SEQUENCE_NAME,"
            + " CASE WHEN d.TYPENAME = 'DECIMAL'"
            + " THEN 'DECIMAL(' || VARCHAR(s.PRECISION) || ', 0)' ELSE d.TYPENAME END"
            + " AS DATA_TYPE,"
            + " s.START AS START_VALUE, s.MINVALUE AS MINIMUM_VALUE,"
            + " s.MAXVALUE AS MAXIMUM_VALUE, s.INCREMENT AS INCREMENT,"
            + " s.CYCLE AS CYCLE_OPTION, s.CACHE AS CACHE, s.REMARKS AS REMARKS"
            + " FROM SYSCAT.SEQUENCES s"
            + " LEFT JOIN SYSCAT.DATATYPES d ON d.TYPEID = s.DATATYPEID"
            + " WHERE s.SEQTYPE = 'S'";

    /**
     * MariaDB keeps a sequence as a one-row table, and every release since 10.3
     * - the one that added {@code CREATE SEQUENCE} - lists it in
     * {@code INFORMATION_SCHEMA.TABLES} with {@code TABLE_TYPE = 'SEQUENCE'},
     * its comment beside it. The {@code SEQUENCES} view only arrived in 11.5,
     * carries no cache size and no comment, and names {@code def} as the catalog
     * where JDBC - and so the explorer - names the database; so the list comes
     * from {@code TABLES} and the options from the sequence itself.
     * Source: MariaDB Server documentation, "SEQUENCE Overview" and
     * "Information Schema SEQUENCES Table".
     */
    private static final String MARIADB_SQL = "SELECT TABLE_SCHEMA, TABLE_NAME, TABLE_COMMENT"
            + " FROM information_schema.TABLES WHERE TABLE_TYPE = 'SEQUENCE'";

    /**
     * How many MariaDB sequences a listing reads the options of. Each costs a
     * query of its own; a schema with more is listed by name and comment, and
     * the options of one are read when it is asked for by name.
     */
    private static final int MARIADB_DETAIL_LIMIT = 100;

    /** The option columns of a MariaDB sequence's own row, in their order. */
    private static final String[] MARIADB_OPTIONS = {
        "MINIMUM_VALUE", "MAXIMUM_VALUE", "START_VALUE", "INCREMENT", "CACHE_SIZE",
        "CYCLE_OPTION",
    };

    private Sequences() {
    }

    /**
     * Lists sequences.
     *
     * <p>Request members: {@code catalog}, {@code schema} and {@code name}, all
     * optional and all matched exactly. Filtering happens here rather than in a
     * {@code WHERE} clause because the column holding the schema name differs
     * per product, and sequence catalogs are small.
     *
     * @param conn the live connection
     * @param dbm  the connection metadata, for the product name
     * @param req  the request body
     * @return the sequence list, empty when the product has no sequences
     */
    public static JsonArray of(Connection conn, DatabaseMetaData dbm, JsonObject req) {
        Dialect dialect = Dialect.of(dbm);
        if (dialect == Dialect.MARIADB) {
            return mariadb(conn, Ident.of(dbm), req);
        }
        String catalog = Json.str(req, "catalog");
        String schema = Json.str(req, "schema");
        String name = Json.str(req, "name");
        // The first query the server answers is the one used; a later one is
        // the older catalog a server without the first still has.
        for (String sql : sqlFor(dialect, Ident.of(dbm), catalog)) {
            JsonArray answered = Attempt.<JsonArray>run(conn, sql, st -> {
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
            }, null);
            if (answered != null) {
                return answered;
            }
        }
        return new JsonArray();
    }

    private static List<String> sqlFor(Dialect dialect, Ident id, String catalog) {
        switch (dialect) {
            case H2:         return List.of(H2_SQL);
            case POSTGRESQL: return List.of(POSTGRESQL_CATALOG_SQL, POSTGRESQL_SQL);
            case ORACLE:     return List.of(ORACLE_OWN_SQL, ORACLE_SQL);
            case SQLSERVER:  return List.of(sqlServer(id, catalog));
            case DB2:        return List.of(DB2_SQL);
            default:         return List.of();
        }
    }

    /**
     * SQL Server's {@code sys.sequences}, which is per database: a catalog other
     * than the session's is read through its three-part name. The values are
     * {@code sql_variant}, cast to text for the driver to hand over.
     * Source: SQL Server documentation, "sys.sequences".
     */
    private static String sqlServer(Ident id, String catalog) {
        boolean other = catalog != null && !catalog.isEmpty();
        String db = other ? id.q(catalog) + "." : "";
        return "SELECT " + (other ? Ident.literal(catalog) : "DB_NAME()") + " AS SEQUENCE_CATALOG,"
                + " sc.name AS SEQUENCE_SCHEMA, s.name AS SEQUENCE_NAME,"
                + " TYPE_NAME(s.user_type_id) AS DATA_TYPE,"
                + " CAST(s.start_value AS VARCHAR(40)) AS START_VALUE,"
                + " CAST(s.minimum_value AS VARCHAR(40)) AS MINIMUM_VALUE,"
                + " CAST(s.maximum_value AS VARCHAR(40)) AS MAXIMUM_VALUE,"
                + " CAST(s.increment AS VARCHAR(40)) AS INCREMENT,"
                + " CASE WHEN s.is_cycling = 1 THEN 'YES' ELSE 'NO' END AS CYCLE_OPTION,"
                + " CASE WHEN s.is_cached = 0 THEN '0'"
                + " ELSE CAST(s.cache_size AS VARCHAR(20)) END AS CACHE,"
                + " CAST(s.current_value AS VARCHAR(40)) AS CURRENT_VALUE"
                + " FROM " + db + "sys.sequences s"
                + " JOIN " + db + "sys.schemas sc ON sc.schema_id = s.schema_id";
    }

    /**
     * MariaDB's sequences, shaped as the other products' are.
     *
     * <p>The database is the {@code catalog}, as in every other JDBC answer
     * about MariaDB, and {@code schema} is null; a request that names the
     * database as its schema is answered too. A sequence whose own row cannot be
     * read - a privilege granted on the schema listing but not on the sequence -
     * is still listed, with its options null.
     */
    private static JsonArray mariadb(Connection conn, Ident id, JsonObject req) {
        String database = Json.str(req, "catalog");
        if (database == null || database.isEmpty()) {
            database = Json.str(req, "schema");
        }
        String db = database == null || database.isEmpty() ? null : database;
        String name = Json.str(req, "name");
        String one = name == null || name.isEmpty() ? null : name;

        List<String[]> found = Attempt.<List<String[]>>on(conn, MARIADB_SQL, c -> {
            String sql = MARIADB_SQL + (db != null ? " AND TABLE_SCHEMA = ?" : "")
                    + (one != null ? " AND TABLE_NAME = ?" : "")
                    + " ORDER BY TABLE_SCHEMA, TABLE_NAME";
            List<String[]> out = new ArrayList<>();
            try (PreparedStatement ps = c.prepareStatement(sql)) {
                int i = 1;
                if (db != null) {
                    ps.setString(i++, db);
                }
                if (one != null) {
                    ps.setString(i, one);
                }
                try (ResultSet rs = ps.executeQuery()) {
                    while (rs.next()) {
                        out.add(new String[]{rs.getString(1), rs.getString(2), rs.getString(3)});
                    }
                }
            }
            return out;
        }, new ArrayList<>());

        // AS <type> arrived with the SEQUENCES view, in 11.5; before it every
        // sequence is a BIGINT.
        Map<String, String> types = Attempt.<Map<String, String>>run(conn,
                "information_schema.SEQUENCES", st -> {
                    Map<String, String> out = new HashMap<>();
                    try (ResultSet rs = st.executeQuery("SELECT SEQUENCE_SCHEMA, SEQUENCE_NAME,"
                            + " DATA_TYPE FROM information_schema.SEQUENCES")) {
                        while (rs.next()) {
                            out.put(rs.getString(1) + '.' + rs.getString(2), rs.getString(3));
                        }
                    }
                    return out;
                }, new HashMap<>());

        boolean details = one != null || found.size() <= MARIADB_DETAIL_LIMIT;
        JsonArray arr = new JsonArray();
        for (String[] f : found) {
            String qualified = id.qualify(f[0], f[1]);
            Map<String, String> r = !details ? new HashMap<>()
                    : Attempt.<Map<String, String>>run(conn, "SELECT FROM " + qualified, st -> {
                        try (ResultSet rs = st.executeQuery("SELECT * FROM " + qualified)) {
                            return rs.next() ? new RsView(rs).strs(MARIADB_OPTIONS)
                                    : new HashMap<>();
                        }
                    }, new HashMap<>());
            JsonObject o = new JsonObject();
            o.addProperty("catalog", f[0]);
            o.addProperty("schema", (String) null);
            o.addProperty("name", f[1]);
            o.addProperty("data_type", types.getOrDefault(f[0] + '.' + f[1], "bigint"));
            o.addProperty("start_value", r.get("START_VALUE"));
            o.addProperty("min_value", r.get("MINIMUM_VALUE"));
            o.addProperty("max_value", r.get("MAXIMUM_VALUE"));
            o.addProperty("increment", r.get("INCREMENT"));
            o.addProperty("cycle", yesNo(r.get("CYCLE_OPTION")));
            o.addProperty("cache", r.get("CACHE_SIZE"));
            // next_not_cached_value is where the next cache block starts, not
            // the value last handed out, and nothing else says what that was.
            o.addProperty("current_value", (String) null);
            o.addProperty("remarks", f[2] == null || f[2].isEmpty() ? null : f[2]);
            arr.add(o);
        }
        return arr;
    }

    /**
     * Every label this reader knows, across all four catalog layouts.
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
