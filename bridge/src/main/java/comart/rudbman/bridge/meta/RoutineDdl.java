package comart.rudbman.bridge.meta;

import com.google.gson.JsonArray;
import com.google.gson.JsonElement;
import com.google.gson.JsonObject;
import comart.rudbman.bridge.BridgeException;
import comart.rudbman.bridge.Json;

import java.sql.Connection;
import java.sql.DatabaseMetaData;
import java.sql.PreparedStatement;
import java.sql.ResultSet;
import java.sql.SQLException;
import java.sql.Statement;
import java.util.ArrayList;
import java.util.List;
import java.util.Locale;

/**
 * {@code DESCRIBE kind: "ddl"} with a {@code routine} member: the {@code CREATE}
 * text of one stored procedure or function.
 *
 * <p>The same two layers as {@link Ddl}, and for the same reason.
 *
 * <ol>
 *   <li><b>Native.</b> Every product that has routines keeps their source, and
 *       most will hand it back: {@code SHOW CREATE PROCEDURE} on MySQL and
 *       MariaDB, {@code pg_get_functiondef} on PostgreSQL, {@code ALL_SOURCE}
 *       on Oracle, {@code OBJECT_DEFINITION} on SQL Server,
 *       {@code SYSCAT.ROUTINES} on Db2 and {@code SCRIPT} on H2.</li>
 *   <li><b>Reconstruction.</b> Otherwise the standard
 *       {@code INFORMATION_SCHEMA.ROUTINES.ROUTINE_DEFINITION} supplies the body
 *       and the JDBC parameter list a header for it. The header carries the
 *       name, the parameters and the return type and nothing else: a routine's
 *       characteristics - determinism, security, language - are in no portable
 *       catalogue, so the response says {@code metadata} and the UI labels
 *       it.</li>
 * </ol>
 *
 * <p>A routine is looked up by name, not by specific name, so an overloaded
 * name answers every overload, one statement each. The explorer lists routines
 * by name too, and showing one overload at random would be worse than showing
 * them all.
 *
 * <p>Whether the request says {@code procedure} or {@code function} is the
 * explorer's folder speaking, and the folder is the driver's filing: H2 files
 * every alias under procedures. Only the products whose own syntax tells the
 * two apart look at it, and those try the other kind when the named one finds
 * nothing.
 */
public final class RoutineDdl {

    /**
     * H2's {@code SCRIPT} restricted to one schema still writes every object in
     * it; the routine is the row creating its alias.
     * Source: H2 2.x SQL grammar, "SCRIPT".
     */
    private static final String H2_SCRIPT = "SCRIPT NODATA NOPASSWORDS NOSETTINGS SCHEMA ";

    private RoutineDdl() {
    }

    /**
     * Builds the DDL for one routine.
     *
     * <p>Request members: {@code routine} ({@code procedure} or
     * {@code function}), {@code catalog}, {@code schema} and {@code name}
     * (required, exact). {@code source} has already been checked by
     * {@link Ddl#of} and means the same as there.
     *
     * @param conn the live connection
     * @param dbm  the connection metadata
     * @param req  the request body
     * @param want {@code auto}, {@code native} or {@code metadata}
     * @return an object carrying {@code ddl} and {@code source}
     * @throws SQLException if the driver fails on the portable path
     */
    static JsonObject of(Connection conn, DatabaseMetaData dbm, JsonObject req, String want)
            throws SQLException {
        String routine = Json.str(req, "routine");
        boolean function;
        if ("function".equals(routine)) {
            function = true;
        } else if ("procedure".equals(routine)) {
            function = false;
        } else {
            throw new BridgeException("protocol",
                    "describe kind 'ddl' accepts routine 'procedure' or 'function', not '"
                            + routine + "'");
        }
        String name = Json.str(req, "name");
        if (name == null || name.isEmpty()) {
            throw new BridgeException("protocol",
                    "describe kind 'ddl' for a routine requires an exact 'name'");
        }
        String catalog = Json.str(req, "catalog");
        String schema = Json.str(req, "schema");
        Dialect dialect = Dialect.of(dbm);
        Ident id = Ident.of(dbm);

        if (!"metadata".equals(want)) {
            String text = nativeDdl(conn, dialect, id, catalog, schema, name, function);
            if (text != null) {
                return Ddl.result(text, "native");
            }
            if ("native".equals(want)) {
                throw new BridgeException("sql",
                        "no native routine DDL source for " + dialect
                                + "; retry with source 'metadata'");
            }
        }
        String text = fromCatalog(conn, dbm, dialect, id, catalog, schema, name, function);
        if (text == null) {
            throw new BridgeException("sql", "the server keeps no readable source for "
                    + id.qualify(catalog, schema, name));
        }
        return Ddl.result(text, "metadata");
    }

    // ---------------------------------------------------------------- native

    /**
     * Asks the server for the routine's own text, or answers {@code null}.
     *
     * <p>Every attempt is fenced by {@link Attempt}: a missing privilege or a
     * server version without the catalogue column is an answer here ("no
     * native text"), not a failure.
     */
    private static String nativeDdl(Connection conn, Dialect dialect, Ident id, String catalog,
                                    String schema, String name, boolean function) {
        switch (dialect) {
            case MYSQL:
            case MARIADB: {
                String qualified = id.qualify(catalog, schema, name);
                String first = function ? "FUNCTION" : "PROCEDURE";
                String other = function ? "PROCEDURE" : "FUNCTION";
                String text = Attempt.<String>run(conn, "SHOW CREATE " + first,
                        st -> mysqlShowCreate(st, first, qualified), null);
                return text != null ? text : Attempt.<String>run(conn, "SHOW CREATE " + other,
                        st -> mysqlShowCreate(st, other, qualified), null);
            }
            case POSTGRESQL:
                return Attempt.<String>on(conn, "pg_get_functiondef",
                        c -> postgres(c, schema, name), null);
            case ORACLE: {
                String first = function ? "FUNCTION" : "PROCEDURE";
                String other = function ? "PROCEDURE" : "FUNCTION";
                String text = Attempt.<String>on(conn, "ALL_SOURCE " + first,
                        c -> oracle(c, schema, name, first), null);
                return text != null ? text : Attempt.<String>on(conn, "ALL_SOURCE " + other,
                        c -> oracle(c, schema, name, other), null);
            }
            case SQLSERVER:
                return Attempt.<String>on(conn, "OBJECT_DEFINITION",
                        c -> sqlServer(c, id.qualify(catalog, schema, unnumbered(name))), null);
            case DB2:
                return Attempt.<String>on(conn, "SYSCAT.ROUTINES",
                        c -> db2(c, schema, name), null);
            case H2:
                if (schema == null || schema.isEmpty()) {
                    return null;
                }
                return Attempt.<String>run(conn, "SCRIPT SCHEMA",
                        st -> h2Script(st, id, schema, name), null);
            default:
                return null;
        }
    }

    private static String mysqlShowCreate(Statement st, String keyword, String qualified)
            throws SQLException {
        try (ResultSet rs = st.executeQuery("SHOW CREATE " + keyword + " " + qualified)) {
            // Column 1 is the name, 2 the sql_mode and 3 the CREATE text, under
            // a label that differs per kind. It is NULL for an account that may
            // run the routine but not read it, which is "no native text".
            if (rs.next() && rs.getMetaData().getColumnCount() >= 3) {
                return terminated(rs.getString(3));
            }
            return null;
        }
    }

    /**
     * Every overload's {@code pg_get_functiondef}, which writes the whole
     * {@code CREATE OR REPLACE} statement. Aggregates are left out: the function
     * refuses them, and {@code prokind} - PostgreSQL 11 and newer - is what says
     * which they are. An older server fails the query and falls through to the
     * catalogue layer.
     */
    private static String postgres(Connection conn, String schema, String name)
            throws SQLException {
        boolean scoped = schema != null && !schema.isEmpty();
        String sql = "SELECT pg_catalog.pg_get_functiondef(p.oid)"
                + " FROM pg_catalog.pg_proc p"
                + " JOIN pg_catalog.pg_namespace n ON n.oid = p.pronamespace"
                + " WHERE p.proname = ? AND p.prokind <> 'a'"
                + (scoped ? " AND n.nspname = ?" : " AND pg_catalog.pg_function_is_visible(p.oid)")
                + " ORDER BY p.oid";
        try (PreparedStatement ps = conn.prepareStatement(sql)) {
            ps.setString(1, name);
            if (scoped) {
                ps.setString(2, schema);
            }
            List<String> texts = new ArrayList<>();
            try (ResultSet rs = ps.executeQuery()) {
                while (rs.next()) {
                    String text = terminated(rs.getString(1));
                    if (text != null) {
                        texts.add(text);
                    }
                }
            }
            return texts.isEmpty() ? null : String.join("\n\n", texts);
        }
    }

    /**
     * {@code ALL_SOURCE} holds the text after {@code CREATE [OR REPLACE]}, one
     * row per line with the line break kept, so the rows are joined as they
     * stand. Its last line already ends in {@code END;}.
     */
    private static String oracle(Connection conn, String schema, String name, String type)
            throws SQLException {
        boolean scoped = schema != null && !schema.isEmpty();
        String sql = "SELECT TEXT FROM ALL_SOURCE WHERE OWNER = "
                + (scoped ? "?" : "SYS_CONTEXT('USERENV', 'CURRENT_SCHEMA')")
                + " AND NAME = ? AND TYPE = ? ORDER BY LINE";
        try (PreparedStatement ps = conn.prepareStatement(sql)) {
            int i = 1;
            if (scoped) {
                ps.setString(i++, schema);
            }
            ps.setString(i++, name);
            ps.setString(i, type);
            StringBuilder sb = new StringBuilder();
            try (ResultSet rs = ps.executeQuery()) {
                while (rs.next()) {
                    String line = rs.getString(1);
                    if (line != null) {
                        sb.append(line);
                    }
                }
            }
            String text = sb.toString().strip();
            return text.isEmpty() ? null : "CREATE OR REPLACE " + text;
        }
    }

    /** {@code OBJECT_DEFINITION} is the batch the routine was created with. */
    private static String sqlServer(Connection conn, String qualified) throws SQLException {
        try (PreparedStatement ps = conn.prepareStatement(
                "SELECT OBJECT_DEFINITION(OBJECT_ID(?))")) {
            ps.setString(1, qualified);
            try (ResultSet rs = ps.executeQuery()) {
                return rs.next() ? stripped(rs.getString(1)) : null;
            }
        }
    }

    /** {@code SYSCAT.ROUTINES.TEXT} is the whole {@code CREATE} statement. */
    private static String db2(Connection conn, String schema, String name) throws SQLException {
        boolean scoped = schema != null && !schema.isEmpty();
        String sql = "SELECT TEXT FROM SYSCAT.ROUTINES WHERE ROUTINESCHEMA = "
                + (scoped ? "?" : "CURRENT SCHEMA") + " AND ROUTINENAME = ? ORDER BY SPECIFICNAME";
        try (PreparedStatement ps = conn.prepareStatement(sql)) {
            int i = 1;
            if (scoped) {
                ps.setString(i++, schema);
            }
            ps.setString(i, name);
            List<String> texts = new ArrayList<>();
            try (ResultSet rs = ps.executeQuery()) {
                while (rs.next()) {
                    String text = stripped(rs.getString(1));
                    if (text != null) {
                        texts.add(text);
                    }
                }
            }
            return texts.isEmpty() ? null : String.join("\n\n", texts);
        }
    }

    /**
     * Runs H2's {@code SCRIPT} over the schema and keeps the alias.
     *
     * <p>H2 writes fully quoted, fully qualified names in a script, so the row
     * that creates this routine is the one with {@code ALIAS "SCHEMA"."NAME"}
     * in it - followed by a space, so that {@code F} does not also pick
     * {@code F_2}.
     */
    private static String h2Script(Statement st, Ident id, String schema, String name)
            throws SQLException {
        String ref = "ALIAS \"" + schema + "\".\"" + name + "\" ";
        List<String> texts = new ArrayList<>();
        try (ResultSet rs = st.executeQuery(H2_SCRIPT + id.q(schema))) {
            while (rs.next()) {
                String stmt = stripped(rs.getString(1));
                if (stmt != null && stmt.toUpperCase(Locale.ROOT).startsWith("CREATE ")
                        && stmt.contains(ref)) {
                    texts.add(stmt);
                }
            }
        }
        return texts.isEmpty() ? null : String.join("\n\n", texts);
    }

    // -------------------------------------------------------------- catalog

    /**
     * Rebuilds the statement from {@code INFORMATION_SCHEMA.ROUTINES} and the
     * JDBC parameter list, or answers {@code null} when the catalogue has no
     * body to give - no such view, or a body the account may not read.
     *
     * <p>MySQL calls its databases catalogs in JDBC and schemas in
     * {@code INFORMATION_SCHEMA}, so the catalog is what is matched there.
     */
    private static String fromCatalog(Connection conn, DatabaseMetaData dbm, Dialect dialect,
                                      Ident id, String catalog, String schema, String name,
                                      boolean function) throws SQLException {
        String owner = dialect.isMySqlFamily() ? catalog : schema;
        boolean scoped = owner != null && !owner.isEmpty();
        String body = Attempt.<String>on(conn, "INFORMATION_SCHEMA.ROUTINES", c -> {
            String sql = "SELECT ROUTINE_DEFINITION FROM INFORMATION_SCHEMA.ROUTINES"
                    + " WHERE ROUTINE_NAME = ?" + (scoped ? " AND ROUTINE_SCHEMA = ?" : "");
            try (PreparedStatement ps = c.prepareStatement(sql)) {
                ps.setString(1, name);
                if (scoped) {
                    ps.setString(2, owner);
                }
                try (ResultSet rs = ps.executeQuery()) {
                    return rs.next() ? stripped(rs.getString(1)) : null;
                }
            }
        }, null);
        if (body == null) {
            return null;
        }

        JsonObject lookup = new JsonObject();
        lookup.addProperty("catalog", catalog);
        lookup.addProperty("schema", schema);
        lookup.addProperty("name", name);
        JsonArray found = function ? Routines.functions(dbm, lookup)
                : Routines.procedures(dbm, lookup);
        if (found.isEmpty()) {
            // Filed under the other kind - H2's aliases, a function some
            // driver lists only as a procedure.
            found = function ? Routines.procedures(dbm, lookup) : Routines.functions(dbm, lookup);
        }
        JsonArray params = new JsonArray();
        for (JsonElement e : found) {
            JsonObject o = e.getAsJsonObject();
            JsonArray listed = Json.arr(o, "parameters");
            if (name.equals(Json.str(o, "name")) && listed != null) {
                params = listed;
                break;
            }
        }

        List<String> args = new ArrayList<>();
        String returns = null;
        for (JsonElement e : params) {
            JsonObject p = e.getAsJsonObject();
            String mode = Json.str(p, "mode_name");
            String type = Json.str(p, "type_name");
            if ("RETURN".equals(mode)) {
                returns = type;
                continue;
            }
            if ("RESULT".equals(mode)) {
                continue;
            }
            StringBuilder arg = new StringBuilder();
            if (!function && mode != null && !"UNKNOWN".equals(mode)) {
                arg.append(mode).append(' ');
            }
            String pname = Json.str(p, "name");
            if (pname != null && !pname.isEmpty()) {
                arg.append(id.q(pname)).append(' ');
            }
            args.add(arg.append(type == null ? "" : type).toString().strip());
        }

        StringBuilder sb = new StringBuilder("CREATE ")
                .append(function ? "FUNCTION " : "PROCEDURE ")
                .append(id.qualify(catalog, schema, name))
                .append('(').append(String.join(", ", args)).append(')');
        if (returns != null) {
            sb.append(" RETURNS ").append(returns);
        }
        return sb.append('\n').append(terminated(body)).toString();
    }

    // ---------------------------------------------------------------- text

    /** SQL Server's procedure numbering: {@code getProcedures} reports {@code P;1}. */
    private static String unnumbered(String name) {
        return name.replaceFirst(";\\d+$", "");
    }

    /** The text with its trailing blanks gone, or {@code null} when nothing is left. */
    private static String stripped(String text) {
        if (text == null) {
            return null;
        }
        String out = text.strip();
        return out.isEmpty() ? null : out;
    }

    /** {@link #stripped}, ending in a {@code ;} as a table's DDL does. */
    private static String terminated(String text) {
        String out = stripped(text);
        if (out == null) {
            return null;
        }
        return out.endsWith(";") ? out : out + ";";
    }
}
