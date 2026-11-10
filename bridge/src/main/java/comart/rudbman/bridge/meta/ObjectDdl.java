package comart.rudbman.bridge.meta;

import com.google.gson.JsonElement;
import com.google.gson.JsonObject;
import comart.rudbman.bridge.BridgeException;
import comart.rudbman.bridge.Json;

import java.sql.Connection;
import java.sql.DatabaseMetaData;
import java.sql.PreparedStatement;
import java.sql.ResultSet;
import java.sql.SQLException;
import java.util.ArrayList;
import java.util.LinkedHashMap;
import java.util.List;
import java.util.Locale;
import java.util.Map;

/**
 * {@code DESCRIBE kind: "ddl"} for a view, a sequence or a trigger.
 *
 * <p>The layers are {@link Ddl}'s - the server's own text first - but what
 * comes after them differs per kind, because what a catalog keeps differs:
 *
 * <ul>
 *   <li>A <b>view</b> is its query, and {@code INFORMATION_SCHEMA.VIEWS} keeps
 *       that on most products; the answer is then a {@code CREATE VIEW} around
 *       it, labeled {@code metadata}. A view is never answered with the
 *       {@code CREATE TABLE} that reconstructing its column list would give:
 *       that is a different object, and a reader would take it for this
 *       one.</li>
 *   <li>A <b>sequence</b> is nothing but its options, and the catalog every
 *       product has for sequences holds all of them, so the statement rebuilt
 *       from it is complete. It says {@code catalog}, which carries no
 *       warning.</li>
 *   <li>A <b>trigger</b> falls back to {@code INFORMATION_SCHEMA.TRIGGERS},
 *       whose {@code ACTION_STATEMENT} is the body, under a header rebuilt from
 *       its timing and events; {@code metadata}.</li>
 * </ul>
 *
 * <p>An object no layer can show is an error naming it, not an empty text.
 */
public final class ObjectDdl {

    private ObjectDdl() {
    }

    // ------------------------------------------------------------------ view

    /**
     * Builds the DDL for one view or materialized view.
     *
     * @param conn the live connection
     * @param dbm  the connection metadata
     * @param req  the request body: {@code catalog}, {@code schema}, {@code name}
     * @param want {@code auto}, {@code native} or {@code metadata}
     * @return an object carrying {@code ddl} and {@code source}
     */
    static JsonObject view(Connection conn, DatabaseMetaData dbm, JsonObject req, String want) {
        Target t = new Target(dbm, req);
        if (!"metadata".equals(want)) {
            String text = nativeView(conn, t);
            if (text != null) {
                return Ddl.result(text, "native");
            }
            noNative(want, t);
        }
        String body = Attempt.<String>on(conn, "INFORMATION_SCHEMA.VIEWS", c -> {
            try (PreparedStatement ps = t.standard(c, "SELECT VIEW_DEFINITION"
                    + " FROM INFORMATION_SCHEMA.VIEWS WHERE TABLE_NAME = ?", "TABLE_SCHEMA")) {
                try (ResultSet rs = ps.executeQuery()) {
                    return rs.next() ? DdlText.stripped(rs.getString(1)) : null;
                }
            }
        }, null);
        if (body == null) {
            throw nothing("definition", t);
        }
        return Ddl.result("CREATE VIEW " + t.qualified() + " AS\n" + DdlText.terminated(body),
                "metadata");
    }

    private static String nativeView(Connection conn, Target t) {
        switch (t.dialect) {
            case MYSQL:
            case MARIADB:
                return show(conn, "VIEW", t, 2);
            case H2:
                return DdlText.h2Script(conn, t.id, t.schema, "VIEW", t.name);
            case POSTGRESQL:
                return Attempt.<String>on(conn, "pg_get_viewdef", c -> postgresView(c, t), null);
            case ORACLE: {
                String text = DdlText.oracleMetadata(conn, "VIEW", t.schema, t.name);
                if (text == null) {
                    text = DdlText.oracleMetadata(conn, "MATERIALIZED_VIEW", t.schema, t.name);
                    if (text != null) {
                        // A materialized view's rows sit in a table of its name,
                        // and that table's indexes are the view's.
                        List<String> parts = new ArrayList<>();
                        parts.add(text);
                        parts.addAll(Ddl.oracleIndexes(conn, t.schema, t.name));
                        text = DdlText.joined(parts);
                    }
                }
                if (text == null) {
                    return null;
                }
                String comments = DdlText.oracleDependent(conn, "COMMENT", t.schema, t.name);
                return comments == null ? text : text + "\n\n" + comments;
            }
            case SQLSERVER: {
                String text = DdlText.sqlServerDefinition(conn, t.qualified());
                List<String> indexes = text == null ? null : SqlServerSchema.indexes(conn, t.id,
                        t.catalog, t.qualified(), t.qualified());
                if (indexes == null || indexes.isEmpty()) {
                    return text;
                }
                // An indexed view's indexes, each a batch of its own after the
                // view's, which has to be alone in its batch.
                StringBuilder sb = new StringBuilder(text.strip());
                for (String ix : indexes) {
                    sb.append("\nGO\n").append(ix).append(';');
                }
                return sb.toString();
            }
            case SQLITE:
                return DdlText.sqlite(conn, "view", t.name);
            case DB2:
                return DdlText.db2Text(conn, "SYSCAT.VIEWS", "VIEWSCHEMA", "VIEWNAME",
                        t.schema, t.name);
            default:
                return null;
        }
    }

    /**
     * {@code pg_get_viewdef} is the query alone; whether it is a view or a
     * materialized view is {@code relkind}, the comment is the class's, and
     * the rest pg_dump writes too: a view's options and its
     * {@code CHECK OPTION} (both kept in {@code reloptions}), a materialized
     * view left unpopulated, and a materialized view's indexes.
     */
    private static String postgresView(Connection c, Target t) throws SQLException {
        String sql = "SELECT c.relkind, pg_catalog.pg_get_viewdef(c.oid, true),"
                + " pg_catalog.obj_description(c.oid, 'pg_class'),"
                + " (SELECT pg_catalog.string_agg(pg_catalog.quote_ident(o.option_name) || '='"
                + " || pg_catalog.quote_literal(o.option_value), ', ')"
                + " FROM pg_catalog.pg_options_to_table(c.reloptions) o"
                + " WHERE o.option_name <> 'check_option'),"
                + " (SELECT o.option_value FROM pg_catalog.pg_options_to_table(c.reloptions) o"
                + " WHERE o.option_name = 'check_option'),"
                + " c.relispopulated"
                + " FROM pg_catalog.pg_class c"
                + " JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace"
                + " WHERE c.relname = ? AND c.relkind IN ('v', 'm')"
                + (t.hasSchema() ? " AND n.nspname = ?" : " AND pg_catalog.pg_table_is_visible(c.oid)");
        try (PreparedStatement ps = c.prepareStatement(sql)) {
            ps.setString(1, t.name);
            if (t.hasSchema()) {
                ps.setString(2, t.schema);
            }
            try (ResultSet rs = ps.executeQuery()) {
                if (!rs.next()) {
                    return null;
                }
                boolean materialized = "m".equals(rs.getString(1));
                String query = DdlText.stripped(rs.getString(2));
                String comment = rs.getString(3);
                String options = rs.getString(4);
                String checkOption = rs.getString(5);
                boolean populated = rs.getBoolean(6);
                if (query == null) {
                    return null;
                }
                while (query.endsWith(";")) {
                    query = query.substring(0, query.length() - 1).stripTrailing();
                }
                String qualified = t.qualified();
                StringBuilder sb = new StringBuilder("CREATE ")
                        .append(materialized ? "MATERIALIZED VIEW " : "VIEW ")
                        .append(qualified);
                if (options != null && !options.isEmpty()) {
                    sb.append(" WITH (").append(options).append(')');
                }
                sb.append(" AS\n").append(query);
                if (checkOption != null && !checkOption.isEmpty()) {
                    sb.append("\n  WITH ").append(checkOption.toUpperCase(Locale.ROOT))
                            .append(" CHECK OPTION");
                }
                if (materialized && !populated) {
                    sb.append("\n  WITH NO DATA");
                }
                sb.append(';');
                if (materialized) {
                    List<String> indexes = Ddl.pgIndexes(c, t.schema, t.name, qualified);
                    for (String ix : indexes == null ? List.<String>of() : indexes) {
                        sb.append("\n\n").append(ix).append(';');
                    }
                }
                if (comment != null && !comment.isEmpty()) {
                    sb.append("\n\nCOMMENT ON ").append(materialized ? "MATERIALIZED VIEW " : "VIEW ")
                            .append(qualified).append(" IS ").append(Ident.literal(comment))
                            .append(';');
                }
                return sb.toString();
            }
        }
    }

    // -------------------------------------------------------------- sequence

    /**
     * Builds the DDL for one sequence.
     *
     * @param conn the live connection
     * @param dbm  the connection metadata
     * @param req  the request body: {@code catalog}, {@code schema}, {@code name}
     * @param want {@code auto}, {@code native} or {@code metadata}
     * @return an object carrying {@code ddl} and {@code source}
     */
    static JsonObject sequence(Connection conn, DatabaseMetaData dbm, JsonObject req,
                               String want) {
        Target t = new Target(dbm, req);
        if (!"metadata".equals(want)) {
            String text;
            switch (t.dialect) {
                case MARIADB:
                    text = show(conn, "SEQUENCE", t, 2);
                    break;
                case H2:
                    text = DdlText.h2Script(conn, t.id, t.schema, "SEQUENCE", t.name);
                    break;
                case ORACLE:
                    text = DdlText.oracleMetadata(conn, "SEQUENCE", t.schema, t.name);
                    break;
                default:
                    text = null;
            }
            if (text != null) {
                return Ddl.result(text, "native");
            }
            noNative(want, t);
        }

        JsonObject lookup = new JsonObject();
        lookup.addProperty("catalog", t.catalog);
        lookup.addProperty("schema", t.schema);
        lookup.addProperty("name", t.name);
        JsonObject found = null;
        for (JsonElement e : Sequences.of(conn, dbm, lookup)) {
            if (t.name.equals(Json.str(e.getAsJsonObject(), "name"))) {
                found = e.getAsJsonObject();
                break;
            }
        }
        if (found == null) {
            throw nothing("options", t);
        }
        // SQL Server names a sequence by schema alone when creating it: the
        // database prefix is refused (Msg 166), where a lookup accepts it.
        StringBuilder sb = new StringBuilder("CREATE SEQUENCE ").append(
                t.dialect == Dialect.SQLSERVER ? t.id.qualify(null, t.schema, t.name)
                        : t.qualified());
        String type = Json.str(found, "data_type");
        if (type != null && t.dialect != Dialect.ORACLE) {
            sb.append(" AS ").append(type);
        }
        option(sb, "START WITH", Json.str(found, "start_value"));
        option(sb, "INCREMENT BY", Json.str(found, "increment"));
        option(sb, "MINVALUE", Json.str(found, "min_value"));
        option(sb, "MAXVALUE", Json.str(found, "max_value"));
        String cache = Json.str(found, "cache");
        if (t.dialect == Dialect.DB2 && cache != null && cache.matches("\\d+")
                && Long.parseLong(cache) < 2) {
            // Db2 keeps NO CACHE as a cache of 1, and refuses CACHE 1.
            sb.append(" NO CACHE");
        } else if (cache != null && !"0".equals(cache)) {
            option(sb, "CACHE", cache);
        }
        if (found.has("cycle") && !found.get("cycle").isJsonNull()) {
            sb.append(found.get("cycle").getAsBoolean() ? " CYCLE" : " NO CYCLE");
        }
        return Ddl.result(sb.append(';').toString(), "catalog");
    }

    private static void option(StringBuilder sb, String keyword, String value) {
        if (value != null && !value.isEmpty()) {
            sb.append(' ').append(keyword).append(' ').append(value);
        }
    }

    // --------------------------------------------------------------- trigger

    /**
     * Builds the DDL for one trigger.
     *
     * @param conn the live connection
     * @param dbm  the connection metadata
     * @param req  the request body: {@code catalog}, {@code schema}, {@code name}
     * @param want {@code auto}, {@code native} or {@code metadata}
     * @return an object carrying {@code ddl} and {@code source}
     */
    static JsonObject trigger(Connection conn, DatabaseMetaData dbm, JsonObject req,
                              String want) {
        Target t = new Target(dbm, req);
        if (!"metadata".equals(want)) {
            String text = nativeTrigger(conn, t);
            if (text != null) {
                return Ddl.result(text, "native");
            }
            noNative(want, t);
        }
        String text = Attempt.<String>on(conn, "INFORMATION_SCHEMA.TRIGGERS",
                c -> standardTrigger(c, t), null);
        if (text == null) {
            throw nothing("definition", t);
        }
        return Ddl.result(text, "metadata");
    }

    private static String nativeTrigger(Connection conn, Target t) {
        switch (t.dialect) {
            case MYSQL:
            case MARIADB:
                return show(conn, "TRIGGER", t, 3);
            case POSTGRESQL:
                return Attempt.<String>on(conn, "pg_get_triggerdef", c -> {
                    String sql = "SELECT pg_catalog.pg_get_triggerdef(t.oid, true)"
                            + " FROM pg_catalog.pg_trigger t"
                            + " JOIN pg_catalog.pg_class c ON c.oid = t.tgrelid"
                            + " JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace"
                            + " WHERE t.tgname = ? AND NOT t.tgisinternal"
                            + (t.hasSchema() ? " AND n.nspname = ?" : "")
                            + " ORDER BY c.relname";
                    List<String> texts = new ArrayList<>();
                    try (PreparedStatement ps = c.prepareStatement(sql)) {
                        ps.setString(1, t.name);
                        if (t.hasSchema()) {
                            ps.setString(2, t.schema);
                        }
                        try (ResultSet rs = ps.executeQuery()) {
                            while (rs.next()) {
                                String text = DdlText.terminated(rs.getString(1));
                                if (text != null) {
                                    texts.add(text);
                                }
                            }
                        }
                    }
                    return DdlText.joined(texts);
                }, null);
            case ORACLE:
                return DdlText.oracleSource(conn, t.schema, t.name, "TRIGGER");
            case SQLSERVER:
                return DdlText.sqlServerDefinition(conn, t.qualified());
            case SQLITE:
                return DdlText.sqlite(conn, "trigger", t.name);
            case H2:
                return DdlText.h2Script(conn, t.id, t.schema, "TRIGGER", t.name);
            case DB2:
                return DdlText.db2Text(conn, "SYSCAT.TRIGGERS", "TRIGSCHEMA", "TRIGNAME",
                        t.schema, t.name);
            default:
                return null;
        }
    }

    /**
     * {@code CREATE TRIGGER} rebuilt from the standard view, which keeps one
     * row per triggering event and the body in {@code ACTION_STATEMENT}.
     */
    private static String standardTrigger(Connection c, Target t) throws SQLException {
        Map<String, String> head = new LinkedHashMap<>();
        List<String> events = new ArrayList<>();
        String body = null;
        try (PreparedStatement ps = t.standard(c, "SELECT ACTION_TIMING, EVENT_MANIPULATION,"
                + " EVENT_OBJECT_TABLE, ACTION_ORIENTATION, ACTION_STATEMENT"
                + " FROM INFORMATION_SCHEMA.TRIGGERS WHERE TRIGGER_NAME = ?", "TRIGGER_SCHEMA")) {
            try (ResultSet rs = ps.executeQuery()) {
                while (rs.next()) {
                    head.putIfAbsent("timing", rs.getString(1));
                    String event = rs.getString(2);
                    if (event != null && !events.contains(event)) {
                        events.add(event);
                    }
                    head.putIfAbsent("table", rs.getString(3));
                    head.putIfAbsent("orientation", rs.getString(4));
                    if (body == null) {
                        body = DdlText.stripped(rs.getString(5));
                    }
                }
            }
        }
        if (body == null) {
            return null;
        }
        StringBuilder sb = new StringBuilder("CREATE TRIGGER ").append(t.qualified());
        if (head.get("timing") != null) {
            sb.append(' ').append(head.get("timing"));
        }
        if (!events.isEmpty()) {
            sb.append(' ').append(String.join(" OR ", events));
        }
        if (head.get("table") != null) {
            sb.append(" ON ").append(t.id.qualify(t.dialect.isMySqlFamily() ? t.catalog : null,
                    t.dialect.isMySqlFamily() ? null : t.schema, head.get("table")));
        }
        if (head.get("orientation") != null) {
            sb.append(" FOR EACH ").append(head.get("orientation"));
        }
        return sb.append('\n').append(DdlText.terminated(body)).toString();
    }

    // ---------------------------------------------------------------- shared

    /**
     * MySQL's and MariaDB's {@code SHOW CREATE <what>}, whose text is in
     * {@code column}: the second for a view or a sequence, the third for a
     * trigger, without its {@code DEFINER} and the like - see
     * {@link DdlText#withoutDefiner}. NULL - an account that may use the object
     * but not read it - is "no native text".
     */
    private static String show(Connection conn, String what, Target t, int column) {
        String qualified = t.qualified();
        return Attempt.<String>run(conn, "SHOW CREATE " + what, st -> {
            try (ResultSet rs = st.executeQuery("SHOW CREATE " + what + " " + qualified)) {
                if (rs.next() && rs.getMetaData().getColumnCount() >= column) {
                    return DdlText.terminated(DdlText.withoutDefiner(rs.getString(column)));
                }
                return null;
            }
        }, null);
    }

    private static void noNative(String want, Target t) {
        if ("native".equals(want)) {
            throw new BridgeException("sql", "no native DDL source for " + t.dialect
                    + "; retry with source 'metadata'");
        }
    }

    private static BridgeException nothing(String what, Target t) {
        return new BridgeException("sql",
                "the server keeps no readable " + what + " for " + t.qualified());
    }

    /** The object a request names, and how to write its name. */
    private static final class Target {
        final Dialect dialect;
        final Ident id;
        final String catalog;
        final String schema;
        final String name;

        Target(DatabaseMetaData dbm, JsonObject req) {
            this.dialect = Dialect.of(dbm);
            this.id = Ident.of(dbm);
            this.catalog = Json.str(req, "catalog");
            this.schema = Json.str(req, "schema");
            this.name = Ddl.objectName(req);
        }

        boolean hasSchema() {
            return schema != null && !schema.isEmpty();
        }

        /**
         * The name as a statement writes it. A PostgreSQL connection sees one
         * database, so its name is left out there, as {@link Ddl} does.
         */
        String qualified() {
            return id.qualify(dialect == Dialect.POSTGRESQL ? null : catalog, schema, name);
        }

        /**
         * A query against a standard {@code INFORMATION_SCHEMA} view with the
         * name bound first and the owner second. MySQL calls its databases
         * catalogs in JDBC and schemas in {@code INFORMATION_SCHEMA}, so the
         * catalog is what is matched there.
         */
        PreparedStatement standard(Connection c, String sql, String ownerColumn)
                throws SQLException {
            String owner = dialect.isMySqlFamily() ? catalog : schema;
            boolean scoped = owner != null && !owner.isEmpty();
            PreparedStatement ps = c.prepareStatement(
                    sql + (scoped ? " AND " + ownerColumn + " = ?" : ""));
            ps.setString(1, name);
            if (scoped) {
                ps.setString(2, owner);
            }
            return ps;
        }
    }
}
