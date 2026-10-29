package comart.rudbman.bridge.meta;

import com.google.gson.JsonArray;
import com.google.gson.JsonObject;
import comart.rudbman.bridge.Json;

import java.sql.Connection;
import java.sql.DatabaseMetaData;
import java.sql.PreparedStatement;
import java.sql.ResultSet;
import java.util.ArrayList;
import java.util.LinkedHashMap;
import java.util.List;
import java.util.Locale;
import java.util.Map;
import java.util.regex.Matcher;
import java.util.regex.Pattern;

/**
 * {@code DESCRIBE kind: "triggers"}.
 *
 * <p>Like sequences, triggers have no {@link DatabaseMetaData} accessor, so
 * this is a catalog query per product: {@code pg_trigger},
 * {@code INFORMATION_SCHEMA.TRIGGERS} on MySQL, MariaDB, H2 and anything
 * unrecognized, {@code sys.triggers} on SQL Server, {@code ALL_TRIGGERS} on
 * Oracle, {@code SYSCAT.TRIGGERS} on Db2 and {@code sqlite_master} on SQLite. An empty list is a correct answer,
 * and so is a rejected query, via {@link Attempt}.
 *
 * <p>Each item: {@code catalog}, {@code schema}, {@code name}, {@code table} -
 * what it fires on - {@code timing} ({@code BEFORE}, {@code AFTER},
 * {@code INSTEAD OF}), {@code events} ({@code INSERT OR UPDATE}),
 * {@code orientation} ({@code ROW} or {@code STATEMENT}), {@code enabled} and
 * {@code remarks}, each null where the product does not say. The standard view
 * keeps one row per event, so rows are merged by trigger.
 *
 * <p>As everywhere else, MySQL's and MariaDB's database is the
 * {@code catalog}, and a request naming it as the schema is answered too.
 */
public final class Triggers {

    /**
     * PostgreSQL's own catalog rather than the standard view: it also lists
     * triggers on tables the user may only read, and says whether a trigger is
     * enabled. {@code tgtype} is a bit set - row 1, before 2, insert 4, delete
     * 8, update 16, truncate 32, instead 64.
     * Source: PostgreSQL source, {@code include/catalog/pg_trigger.h}.
     */
    private static final String POSTGRESQL_SQL = "SELECT current_database(), n.nspname,"
            + " t.tgname, c.relname,"
            + " CASE WHEN t.tgtype & 2 <> 0 THEN 'BEFORE' WHEN t.tgtype & 64 <> 0"
            + " THEN 'INSTEAD OF' ELSE 'AFTER' END,"
            + " concat_ws(' OR ', CASE WHEN t.tgtype & 4 <> 0 THEN 'INSERT' END,"
            + " CASE WHEN t.tgtype & 16 <> 0 THEN 'UPDATE' END,"
            + " CASE WHEN t.tgtype & 8 <> 0 THEN 'DELETE' END,"
            + " CASE WHEN t.tgtype & 32 <> 0 THEN 'TRUNCATE' END),"
            + " CASE WHEN t.tgtype & 1 <> 0 THEN 'ROW' ELSE 'STATEMENT' END,"
            + " CASE WHEN t.tgenabled = 'D' THEN 'N' ELSE 'Y' END,"
            + " pg_catalog.obj_description(t.oid, 'pg_trigger')"
            + " FROM pg_catalog.pg_trigger t"
            + " JOIN pg_catalog.pg_class c ON c.oid = t.tgrelid"
            + " JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace"
            + " WHERE NOT t.tgisinternal";

    /** MySQL and MariaDB: the database is the catalog, and every trigger is enabled. */
    private static final String MYSQL_SQL = "SELECT TRIGGER_SCHEMA, NULL, TRIGGER_NAME,"
            + " EVENT_OBJECT_TABLE, ACTION_TIMING, EVENT_MANIPULATION, ACTION_ORIENTATION,"
            + " NULL, NULL FROM information_schema.TRIGGERS WHERE 1 = 1";

    /**
     * The SQL standard view, with H2's {@code REMARKS}; also the attempt made on
     * a product this bridge does not recognize.
     */
    private static final String STANDARD_SQL = "SELECT TRIGGER_CATALOG, TRIGGER_SCHEMA,"
            + " TRIGGER_NAME, EVENT_OBJECT_TABLE, ACTION_TIMING, EVENT_MANIPULATION,"
            + " ACTION_ORIENTATION, NULL, %s FROM INFORMATION_SCHEMA.TRIGGERS WHERE 1 = 1";

    /**
     * Oracle's {@code TRIGGER_TYPE} says timing and level in one phrase -
     * {@code BEFORE EACH ROW}, {@code AFTER STATEMENT} - which is taken apart
     * below. A trigger on a schema or the database has no table.
     * Source: Oracle Database Reference, "ALL_TRIGGERS".
     */
    private static final String ORACLE_SQL = "SELECT NULL, OWNER, TRIGGER_NAME, TABLE_NAME,"
            + " TRIGGER_TYPE, TRIGGERING_EVENT, NULL,"
            + " CASE WHEN STATUS = 'ENABLED' THEN 'Y' ELSE 'N' END, NULL"
            + " FROM ALL_TRIGGERS WHERE OWNER = ";

    /**
     * Db2 keeps one row per trigger with a letter each for timing, event and
     * level. A trigger on several events says {@code M} and sets a flag per
     * event; older releases have only single-event triggers. Whether one is
     * enabled is not asked: the column is recent, and a query that named it
     * would list nothing on a server without it. The schema is trimmed for the
     * reason {@code Sequences} gives: an implicitly created one is
     * blank-padded to eight characters.
     * Source: Db2 12.1 documentation, "SYSCAT.TRIGGERS catalog view".
     */
    private static final String DB2_SQL = "SELECT CAST(NULL AS VARCHAR(1)), RTRIM(TRIGSCHEMA),"
            + " TRIGNAME, TABNAME,"
            + " CASE TRIGTIME WHEN 'B' THEN 'BEFORE' WHEN 'I' THEN 'INSTEAD OF'"
            + " ELSE 'AFTER' END,"
            + " CASE TRIGEVENT WHEN 'I' THEN 'INSERT' WHEN 'U' THEN 'UPDATE'"
            + " WHEN 'D' THEN 'DELETE' ELSE SUBSTR("
            + "CASE WHEN EVENTINSERT = 'Y' THEN ' OR INSERT' ELSE '' END"
            + " || CASE WHEN EVENTUPDATE = 'Y' THEN ' OR UPDATE' ELSE '' END"
            + " || CASE WHEN EVENTDELETE = 'Y' THEN ' OR DELETE' ELSE '' END, 5) END,"
            + " CASE GRANULARITY WHEN 'R' THEN 'ROW' ELSE 'STATEMENT' END,"
            + " CAST(NULL AS CHAR(1)), REMARKS FROM SYSCAT.TRIGGERS WHERE 1 = 1";

    /** SQLite keeps a trigger's statement and its table, and nothing else. */
    private static final String SQLITE_SQL = "SELECT name, tbl_name, sql FROM sqlite_master"
            + " WHERE type = 'trigger'";

    private static final Pattern SQLITE_HEAD = Pattern.compile(
            "(?is)\\bTRIGGER\\s+(?:IF\\s+NOT\\s+EXISTS\\s+)?\\S+\\s+"
                    + "(BEFORE|AFTER|INSTEAD\\s+OF)?\\s*(INSERT|DELETE|UPDATE)\\b");

    private Triggers() {
    }

    /**
     * Lists triggers.
     *
     * @param conn the live connection
     * @param dbm  the connection metadata, for the product name
     * @param req  the request body: {@code catalog}, {@code schema} and
     *             {@code name}, all optional and exact
     * @return the trigger list, empty where the product has none or says nothing
     */
    public static JsonArray of(Connection conn, DatabaseMetaData dbm, JsonObject req) {
        Dialect dialect = Dialect.of(dbm);
        Ident id = Ident.of(dbm);
        String catalog = blank(Json.str(req, "catalog"));
        String schema = blank(Json.str(req, "schema"));
        String name = blank(Json.str(req, "name"));
        if (dialect == Dialect.SQLITE) {
            return sqlite(conn, name);
        }

        StringBuilder sql = new StringBuilder();
        List<String> args = new ArrayList<>();
        switch (dialect) {
            case POSTGRESQL:
                sql.append(POSTGRESQL_SQL);
                filter(sql, args, "n.nspname", schema);
                filter(sql, args, "t.tgname", name);
                sql.append(" ORDER BY 2, 3");
                break;
            case MYSQL:
            case MARIADB:
                sql.append(MYSQL_SQL);
                filter(sql, args, "TRIGGER_SCHEMA", catalog != null ? catalog : schema);
                filter(sql, args, "TRIGGER_NAME", name);
                sql.append(" ORDER BY 1, 3");
                break;
            case SQLSERVER: {
                // sys.triggers is per database; a catalog other than the
                // session's is read through its own three-part names.
                String db = catalog == null ? "" : id.q(catalog) + ".";
                sql.append("SELECT ").append(catalog == null ? "DB_NAME()" : "?")
                        .append(", s.name, tr.name, o.name,")
                        .append(" CASE WHEN tr.is_instead_of_trigger = 1 THEN 'INSTEAD OF'")
                        .append(" ELSE 'AFTER' END, te.type_desc, 'STATEMENT',")
                        .append(" CASE WHEN tr.is_disabled = 1 THEN 'N' ELSE 'Y' END, NULL")
                        .append(" FROM ").append(db).append("sys.triggers tr")
                        .append(" JOIN ").append(db).append("sys.objects o")
                        .append(" ON o.object_id = tr.parent_id")
                        .append(" JOIN ").append(db).append("sys.schemas s")
                        .append(" ON s.schema_id = o.schema_id")
                        .append(" JOIN ").append(db).append("sys.trigger_events te")
                        .append(" ON te.object_id = tr.object_id")
                        .append(" WHERE tr.parent_class = 1");
                if (catalog != null) {
                    args.add(catalog);
                }
                filter(sql, args, "s.name", schema);
                filter(sql, args, "tr.name", name);
                sql.append(" ORDER BY 2, 3");
                break;
            }
            case DB2:
                sql.append(DB2_SQL);
                filter(sql, args, "TRIGSCHEMA", schema);
                filter(sql, args, "TRIGNAME", name);
                sql.append(" ORDER BY 2, 3");
                break;
            case ORACLE:
                sql.append(ORACLE_SQL).append(DdlText.owner(schema));
                if (schema != null) {
                    args.add(schema);
                }
                filter(sql, args, "TRIGGER_NAME", name);
                sql.append(" ORDER BY 3");
                break;
            default:
                sql.append(String.format(STANDARD_SQL,
                        dialect == Dialect.H2 ? "REMARKS" : "NULL"));
                filter(sql, args, "TRIGGER_SCHEMA", schema);
                filter(sql, args, "TRIGGER_NAME", name);
                sql.append(" ORDER BY 2, 3");
        }

        boolean oracle = dialect == Dialect.ORACLE;
        return Attempt.on(conn, "triggers", c -> {
            Map<String, JsonObject> merged = new LinkedHashMap<>();
            try (PreparedStatement ps = c.prepareStatement(sql.toString())) {
                for (int i = 0; i < args.size(); i++) {
                    ps.setString(i + 1, args.get(i));
                }
                try (ResultSet rs = ps.executeQuery()) {
                    while (rs.next()) {
                        String[] r = new String[9];
                        for (int i = 0; i < r.length; i++) {
                            r[i] = rs.getString(i + 1);
                        }
                        if (oracle) {
                            oracleType(r);
                        }
                        String key = r[0] + '\0' + r[1] + '\0' + r[2] + '\0' + r[3];
                        JsonObject o = merged.get(key);
                        if (o == null) {
                            merged.put(key, item(r[0], r[1], r[2], r[3], r[4], r[5], r[6],
                                    r[7] == null ? null : "Y".equals(r[7]), r[8]));
                        } else if (r[5] != null) {
                            String events = Json.str(o, "events");
                            if (events == null || !(" OR " + events + " OR ").contains(
                                    " OR " + r[5] + " OR ")) {
                                o.addProperty("events", events == null ? r[5]
                                        : events + " OR " + r[5]);
                            }
                        }
                    }
                }
            }
            JsonArray arr = new JsonArray();
            merged.values().forEach(arr::add);
            return arr;
        }, new JsonArray());
    }

    /** Splits Oracle's {@code TRIGGER_TYPE} into timing and level, in place. */
    private static void oracleType(String[] r) {
        String type = r[4] == null ? "" : r[4].trim().toUpperCase(Locale.ROOT);
        if (type.endsWith(" EACH ROW")) {
            r[4] = type.substring(0, type.length() - " EACH ROW".length());
            r[6] = "ROW";
        } else if (type.endsWith(" STATEMENT")) {
            r[4] = type.substring(0, type.length() - " STATEMENT".length());
            r[6] = "STATEMENT";
        } else if (type.startsWith("INSTEAD OF")) {
            r[4] = "INSTEAD OF";
            r[6] = "ROW";
        } else {
            r[4] = type.isEmpty() ? null : type;
        }
    }

    /** SQLite: timing and event read off the statement, row-level always. */
    private static JsonArray sqlite(Connection conn, String name) {
        return Attempt.on(conn, "sqlite_master triggers", c -> {
            JsonArray arr = new JsonArray();
            String sql = SQLITE_SQL + (name != null ? " AND name = ?" : "") + " ORDER BY name";
            try (PreparedStatement ps = c.prepareStatement(sql)) {
                if (name != null) {
                    ps.setString(1, name);
                }
                try (ResultSet rs = ps.executeQuery()) {
                    while (rs.next()) {
                        String text = rs.getString(3);
                        String timing = null;
                        String event = null;
                        Matcher m = SQLITE_HEAD.matcher(text == null ? "" : text);
                        if (m.find()) {
                            timing = m.group(1) == null ? "BEFORE"
                                    : m.group(1).toUpperCase(Locale.ROOT).replaceAll("\\s+", " ");
                            event = m.group(2).toUpperCase(Locale.ROOT);
                        }
                        arr.add(item(null, null, rs.getString(1), rs.getString(2), timing, event,
                                "ROW", null, null));
                    }
                }
            }
            return arr;
        }, new JsonArray());
    }

    private static JsonObject item(String catalog, String schema, String name, String table,
                                   String timing, String events, String orientation,
                                   Boolean enabled, String remarks) {
        JsonObject o = new JsonObject();
        o.addProperty("catalog", catalog);
        o.addProperty("schema", schema);
        o.addProperty("name", name);
        o.addProperty("table", table);
        o.addProperty("timing", timing);
        o.addProperty("events", events == null || events.isEmpty() ? null : events);
        o.addProperty("orientation", orientation);
        o.addProperty("enabled", enabled);
        o.addProperty("remarks", remarks == null || remarks.isEmpty() ? null : remarks);
        return o;
    }

    private static void filter(StringBuilder sql, List<String> args, String column, String value) {
        if (value != null) {
            sql.append(" AND ").append(column).append(" = ?");
            args.add(value);
        }
    }

    private static String blank(String s) {
        return s == null || s.isEmpty() ? null : s;
    }
}
