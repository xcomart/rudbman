package comart.rudbman.bridge.meta;

import com.google.gson.JsonArray;
import com.google.gson.JsonElement;
import com.google.gson.JsonNull;
import com.google.gson.JsonObject;
import comart.rudbman.bridge.Json;

import java.sql.Connection;
import java.sql.DatabaseMetaData;
import java.sql.PreparedStatement;
import java.sql.ResultSet;
import java.sql.SQLException;
import java.sql.Statement;
import java.util.ArrayList;
import java.util.Collections;
import java.util.HashMap;
import java.util.List;
import java.util.Map;
import java.util.function.Function;

/**
 * Comment repair for {@code DESCRIBE kind: "tables"} and {@code kind: "columns"}:
 * the comments a driver hides are put back, and the one comment a driver invents
 * is taken away.
 *
 * <p>{@code REMARKS} is an optional metadata column, and on several products the
 * driver answers it with null while the server is holding the comment all along:
 *
 * <ul>
 *   <li><b>Oracle</b> reports comments only on a connection opened with
 *       {@code remarksReporting=true}, which makes the driver join
 *       {@code ALL_*_COMMENTS} into <em>every</em> metadata call, including the
 *       many that never show a comment. That is a bad trade for a schema browser
 *       and the sessions here are not opened that way, so the two comment views
 *       are read directly instead: {@code ALL_TAB_COMMENTS} and
 *       {@code ALL_COL_COMMENTS}.</li>
 *   <li><b>SQL Server</b> has no comment syntax at all. What every tool calls a
 *       comment there is an extended property named {@code MS_Description}, and
 *       no driver goes looking for it.</li>
 *   <li><b>MySQL and MariaDB</b> parse their metadata out of {@code SHOW} output
 *       by default, which carries column comments but not table comments;
 *       {@code information_schema.TABLES.TABLE_COMMENT} has those.</li>
 * </ul>
 *
 * <p>PostgreSQL and H2 fill {@code REMARKS} themselves and are never asked.
 * SQLite has no comments to find. Anything unrecognised gets the portable answer.
 *
 * <p>The subtraction is MySQL's and MariaDB's alone: they report the literal
 * string {@code VIEW} as a view's comment, and they do it through
 * <em>both</em> paths - the driver's own {@code SHOW} parsing puts it in
 * {@code REMARKS}, and {@code information_schema.TABLES} puts it in
 * {@code TABLE_COMMENT}. Blocking it in the enrichment query alone would leave
 * the driver's copy untouched, because filling only blanks means a placeholder
 * that is already there is never overwritten. So it is cleared from the rows
 * first, on the product, before anything else looks at them; see
 * {@link #unmarkViews}.
 *
 * <p>Three rules then keep this from making an answer worse than the driver's own.
 *
 * <ol>
 *   <li><b>The driver wins.</b> A comment is written only where the field is
 *       still blank, so a product that answers partly - MySQL, with its columns
 *       but not its tables - keeps every value it did give and only the holes are
 *       filled. The placeholder above is not an exception to this: it is removed
 *       before the rule applies, precisely because it is not a value.</li>
 *   <li><b>Nothing is asked when nothing is missing.</b> A result whose rows all
 *       carry a comment already, or that has no rows, costs no query at all.</li>
 *   <li><b>Failure is silent.</b> The lookup runs under {@link Attempt}: no
 *       privilege on {@code sys.extended_properties}, a server too old for a
 *       column, a shape this code did not predict - each degrades to the
 *       un-enriched result rather than to a failed {@code DESCRIBE}.</li>
 * </ol>
 *
 * <p>One query answers a whole request. It is scoped with the same filters the
 * request carried and its rows are overlaid onto the result by name; a lookup per
 * row would be a round trip per table, which on a tree's first expand is
 * thousands. The filter members are read the way {@link Describe} reads them -
 * {@code schema} exact, {@code schema_pattern} a JDBC search pattern - and a JDBC
 * search pattern is a SQL {@code LIKE} pattern, so it is passed to {@code LIKE}
 * unchanged.
 *
 * <p><b>Custom queries.</b> Four products are a list, not a rule: any driver may
 * hide comments, and the ones this file has never heard of are exactly the ones
 * nobody here can add a query for. So a driver definition may carry its own -
 * {@code table_comments_sql} and {@code column_comments_sql} on the session, from
 * jdbgen, where a driver definition has carried these two queries all along.
 * Where one is set it <em>replaces</em> the built-in query for that kind rather
 * than adding to it, and it does so on every product, the four recognised ones
 * included: the definition is the user's statement about their own database and
 * it outranks this file's guess. Everything around the query is unchanged - the
 * driver still wins, nothing is asked when nothing is missing, failure is still
 * silent.
 *
 * <p>The queries are written against one schema and say so themselves, through
 * the variables {@code ${catalog}} and {@code ${schema}}; see {@link #expand}.
 * Their result sets are read left to right and positionally, name and comment for
 * tables, table, name and comment for columns.
 */
final class Comments {

    /**
     * Oracle's accessible-comment views. There is no information schema and no
     * {@code REMARKS} anywhere else; these two views <em>are</em> where a comment
     * is kept.
     * Source: Oracle Database Reference, "ALL_TAB_COMMENTS" / "ALL_COL_COMMENTS".
     */
    private static final String ORACLE_TABLES =
            "SELECT OWNER, TABLE_NAME, COMMENTS FROM ALL_TAB_COMMENTS WHERE ";
    private static final String ORACLE_COLUMNS =
            "SELECT OWNER, TABLE_NAME, COLUMN_NAME, COMMENTS FROM ALL_COL_COMMENTS WHERE ";

    /**
     * Without a schema filter the request asked for everything the session can
     * see, and on Oracle that is every owner in the database - a scan of both
     * comment views for a browser that is about to show one schema. The session's
     * effective schema is the useful subset, and {@code SYS_CONTEXT} names it
     * without a second round trip. Owners outside it simply keep the null the
     * driver gave, which is what they had before this class existed.
     */
    private static final String ORACLE_CURRENT_SCHEMA =
            "OWNER = SYS_CONTEXT('USERENV', 'CURRENT_SCHEMA')";

    /**
     * SQL Server keeps comments as extended properties: {@code class = 1} is the
     * object-or-column class, {@code minor_id = 0} means the object itself and
     * any other {@code minor_id} is a column ordinal. The name
     * {@code MS_Description} is only a convention, but it is the one SSMS writes
     * and reads, so it is the one a user means by "comment".
     *
     * <p>{@code sys.objects} filtered to {@code 'U'} and {@code 'V'} stands in
     * for {@code sys.tables} and {@code sys.views} together; the union of the two
     * is exactly what {@code getTables} returned. {@code ep.value} is
     * {@code sql_variant} and has to be cast before a driver will hand it over as
     * text.
     * Source: SQL Server documentation, "sys.extended_properties".
     */
    private static final String SQLSERVER_TABLES =
            "SELECT s.name, o.name, CAST(ep.value AS nvarchar(4000)) "
            + "FROM sys.extended_properties ep "
            + "JOIN sys.objects o ON o.object_id = ep.major_id "
            + "JOIN sys.schemas s ON s.schema_id = o.schema_id "
            + "WHERE ep.class = 1 AND ep.minor_id = 0 "
            + "AND ep.name = 'MS_Description' AND o.type IN ('U', 'V')";
    private static final String SQLSERVER_COLUMNS =
            "SELECT s.name, o.name, c.name, CAST(ep.value AS nvarchar(4000)) "
            + "FROM sys.extended_properties ep "
            + "JOIN sys.objects o ON o.object_id = ep.major_id "
            + "JOIN sys.schemas s ON s.schema_id = o.schema_id "
            + "JOIN sys.columns c ON c.object_id = ep.major_id AND c.column_id = ep.minor_id "
            + "WHERE ep.class = 1 AND ep.minor_id > 0 "
            + "AND ep.name = 'MS_Description' AND o.type IN ('U', 'V')";

    /**
     * The {@code CASE} is the same placeholder rule {@link #unmarkViews} applies
     * to the driver's own answer, enforced on the other path a {@code VIEW} can
     * arrive by. Both are needed: this one keeps the enrichment from writing the
     * placeholder into a row that had no comment, and {@code unmarkViews} clears
     * the one Connector/J already put there.
     *
     * <p>The catalog is the database on these drivers (Connector/J's default
     * {@code databaseTerm=CATALOG}), so the request's {@code catalog} is what
     * {@code TABLE_SCHEMA} holds.
     * Source: MySQL 8.0 Reference Manual, "The INFORMATION_SCHEMA TABLES Table".
     */
    private static final String MYSQL_TABLES =
            "SELECT TABLE_SCHEMA, TABLE_NAME, "
            + "CASE WHEN TABLE_TYPE = 'VIEW' THEN NULL ELSE TABLE_COMMENT END "
            + "FROM information_schema.TABLES WHERE TABLE_SCHEMA = ";
    private static final String MYSQL_COLUMNS =
            "SELECT TABLE_SCHEMA, TABLE_NAME, COLUMN_NAME, COLUMN_COMMENT "
            + "FROM information_schema.COLUMNS WHERE TABLE_SCHEMA = ";

    /**
     * Oracle and SQL Server give {@code LIKE} no escape character unless one is
     * declared, while a JDBC search pattern always has one. MySQL's {@code LIKE}
     * already escapes with a backslash and rejects nothing, so it needs no clause
     * - and writing one would mean escaping the backslash for MySQL's own string
     * parser as well.
     */
    private static final String ANSI_ESCAPE = " ESCAPE '\\'";
    private static final String NO_ESCAPE = "";

    /** The JDBC table type, and the placeholder MySQL reports as a view's comment. */
    private static final String VIEW = "VIEW";

    /**
     * Separator for composite lookup keys. NUL, because a quoted identifier may
     * hold a space, a dot or anything else printable, and a separator that can
     * occur inside a name lets two different rows collide on one key.
     */
    private static final char SEP = '\0';

    private Comments() {
    }

    /**
     * Clears the placeholder comment, then fills in the missing ones, in place.
     *
     * @param conn   the live connection
     * @param dbm    the connection metadata, for the product name
     * @param req    the request body, for the filters to scope the query with
     * @param items  the rows built from {@code getTables}
     * @param custom the session's {@code table_comments_sql}, or {@code null}
     */
    static void tables(Connection conn, DatabaseMetaData dbm, JsonObject req, JsonArray items,
                       String custom) {
        Dialect dialect = Dialect.of(dbm);
        if (dialect.isMySqlFamily()) {
            unmarkViews(items);
        }
        if (!anyMissing(items)) {
            return;
        }
        if (custom != null) {
            String sql = expand(custom, req);
            if (sql != null) {
                overlay(conn, "custom table comments", sql, items, 1,
                        o -> key(Json.str(o, "name")));
            }
            return;
        }
        if (!enriches(dialect)) {
            return;
        }
        Attempt.on(conn, "table comments", c -> {
            Map<String, String> found = tableComments(c, dialect, req);
            for (JsonObject o : missing(items)) {
                fill(o, found.get(key(owner(dialect, o), Json.str(o, "name"))));
            }
            return null;
        }, null);
    }

    /**
     * Fills in missing column comments, in place.
     *
     * @param conn   the live connection
     * @param dbm    the connection metadata, for the product name
     * @param req    the request body, for the filters to scope the query with
     * @param items  the rows built from {@code getColumns}
     * @param custom the session's {@code column_comments_sql}, or {@code null}
     */
    static void columns(Connection conn, DatabaseMetaData dbm, JsonObject req, JsonArray items,
                        String custom) {
        Dialect dialect = Dialect.of(dbm);
        if (!anyMissing(items)) {
            return;
        }
        if (custom != null) {
            String sql = expand(custom, req);
            if (sql != null) {
                overlay(conn, "custom column comments", sql, items, 2,
                        o -> key(Json.str(o, "table"), Json.str(o, "name")));
            }
            return;
        }
        if (!enriches(dialect)) {
            return;
        }
        Attempt.on(conn, "column comments", c -> {
            Map<String, String> found = columnComments(c, dialect, req);
            for (JsonObject o : missing(items)) {
                fill(o, found.get(key(owner(dialect, o),
                        Json.str(o, "table"), Json.str(o, "name"))));
            }
            return null;
        }, null);
    }

    /**
     * Drops the {@code VIEW} that MySQL and MariaDB put in a view's comment.
     *
     * <p>It is a type marker left over from the days before {@code TABLE_TYPE},
     * not a comment anyone wrote - neither product even has syntax to comment a
     * view - and left alone it prints the word "VIEW" in the description column
     * of every view in the tree. Connector/J hands it over in {@code REMARKS}
     * without any help from this class, so the correction cannot live in the
     * enrichment query and cannot be conditional on enrichment running: it is a
     * normalisation of what the driver said, and it happens whether or not any
     * comment is missing.
     *
     * <p>The test is on {@code TABLE_TYPE} rather than on the text, so a
     * <em>table</em> genuinely commented "VIEW" keeps its comment. The value
     * becomes JSON null rather than vanishing, which is how every other absent
     * optional metadata value is reported.
     */
    private static void unmarkViews(JsonArray items) {
        for (JsonElement e : items) {
            JsonObject o = e.getAsJsonObject();
            if (VIEW.equals(Json.str(o, "type")) && VIEW.equals(Json.str(o, "remarks"))) {
                o.add("remarks", JsonNull.INSTANCE);
            }
        }
    }

    /** @return whether this product hides comments the server is holding */
    private static boolean enriches(Dialect dialect) {
        return dialect == Dialect.ORACLE || dialect == Dialect.SQLSERVER
                || dialect.isMySqlFamily();
    }

    // ----------------------------------------------------------------- queries

    /** @return table comments, keyed by owner and table name */
    private static Map<String, String> tableComments(Connection conn, Dialect dialect,
                                                     JsonObject req) throws SQLException {
        Filter schema = Filter.of(req, "schema", "schema_pattern");
        Filter table = Filter.of(req, "table", "table_pattern");
        StringBuilder sql = new StringBuilder();
        List<String> args = new ArrayList<>();
        switch (dialect) {
            case ORACLE:
                sql.append(ORACLE_TABLES);
                owner(sql, args, schema);
                table.append(sql, args, "TABLE_NAME", ANSI_ESCAPE);
                break;
            case SQLSERVER:
                if (elsewhere(conn, req)) {
                    return Collections.emptyMap();
                }
                sql.append(SQLSERVER_TABLES);
                schema.append(sql, args, "s.name", ANSI_ESCAPE);
                table.append(sql, args, "o.name", ANSI_ESCAPE);
                break;
            case MYSQL:
            case MARIADB:
                sql.append(MYSQL_TABLES);
                database(sql, args, req);
                table.append(sql, args, "TABLE_NAME", NO_ESCAPE);
                break;
            default:
                return Collections.emptyMap();
        }
        return read(conn, sql.toString(), args, 2);
    }

    /** @return column comments, keyed by owner, table name and column name */
    private static Map<String, String> columnComments(Connection conn, Dialect dialect,
                                                      JsonObject req) throws SQLException {
        Filter schema = Filter.of(req, "schema", "schema_pattern");
        Filter table = Filter.of(req, "table", "table_pattern");
        Filter column = Filter.of(req, "column", "column_pattern");
        StringBuilder sql = new StringBuilder();
        List<String> args = new ArrayList<>();
        switch (dialect) {
            case ORACLE:
                sql.append(ORACLE_COLUMNS);
                owner(sql, args, schema);
                table.append(sql, args, "TABLE_NAME", ANSI_ESCAPE);
                column.append(sql, args, "COLUMN_NAME", ANSI_ESCAPE);
                break;
            case SQLSERVER:
                if (elsewhere(conn, req)) {
                    return Collections.emptyMap();
                }
                sql.append(SQLSERVER_COLUMNS);
                schema.append(sql, args, "s.name", ANSI_ESCAPE);
                table.append(sql, args, "o.name", ANSI_ESCAPE);
                column.append(sql, args, "c.name", ANSI_ESCAPE);
                break;
            case MYSQL:
            case MARIADB:
                sql.append(MYSQL_COLUMNS);
                database(sql, args, req);
                table.append(sql, args, "TABLE_NAME", NO_ESCAPE);
                column.append(sql, args, "COLUMN_NAME", NO_ESCAPE);
                break;
            default:
                return Collections.emptyMap();
        }
        return read(conn, sql.toString(), args, 3);
    }

    /** Appends Oracle's mandatory owner predicate. */
    private static void owner(StringBuilder sql, List<String> args, Filter schema) {
        if (schema.absent()) {
            sql.append(ORACLE_CURRENT_SCHEMA);
        } else {
            sql.append("OWNER").append(schema.test(ANSI_ESCAPE));
            args.add(schema.value);
        }
    }

    /** Appends the MySQL database predicate the two queries open with. */
    private static void database(StringBuilder sql, List<String> args, JsonObject req) {
        String catalog = Json.str(req, "catalog");
        if (catalog == null || catalog.isEmpty()) {
            // No catalog filter means "whatever this session is connected to",
            // which is the same thing the driver's own metadata call resolved.
            sql.append("DATABASE()");
        } else {
            sql.append('?');
            args.add(catalog);
        }
    }

    /**
     * @return whether the request names a database other than the one this
     *         session is in. The {@code sys} views are per database and there is
     *         no portable way to reach another one - {@code USE} would move the
     *         user's session - so a cross-database request is answered without
     *         enrichment rather than with the wrong database's comments.
     */
    private static boolean elsewhere(Connection conn, JsonObject req) throws SQLException {
        String catalog = Json.str(req, "catalog");
        return catalog != null && !catalog.isEmpty() && !catalog.equals(conn.getCatalog());
    }

    /**
     * Runs one comment query.
     *
     * @param keys how many leading columns make up the lookup key; the comment is
     *             the column after them
     * @return the non-blank comments, keyed by those columns
     */
    private static Map<String, String> read(Connection conn, String sql, List<String> args,
                                            int keys) throws SQLException {
        try (PreparedStatement ps = conn.prepareStatement(sql)) {
            for (int i = 0; i < args.size(); i++) {
                ps.setString(i + 1, args.get(i));
            }
            try (ResultSet rs = ps.executeQuery()) {
                return collect(rs, keys);
            }
        }
    }

    /**
     * Collects one comment result set.
     *
     * @param keys how many leading columns make up the lookup key; the comment is
     *             the column after them
     * @return the non-blank comments, keyed by those columns
     */
    private static Map<String, String> collect(ResultSet rs, int keys) throws SQLException {
        Map<String, String> out = new HashMap<>();
        while (rs.next()) {
            // Left to right, the house rule from RsView. For the built-in
            // queries the column order is this file's own, so following it costs
            // nothing; for a custom query it is the only order there is, since
            // nobody can predict what an author will label their columns. Any
            // further column is the author's business and is not read.
            StringBuilder k = new StringBuilder();
            for (int i = 1; i <= keys; i++) {
                append(k, rs.getString(i));
            }
            String comment = rs.getString(keys + 1);
            if (!blank(comment)) {
                out.put(k.toString(), comment);
            }
        }
        return out;
    }

    // ---------------------------------------------------------- custom queries

    /**
     * Substitutes the request's filters into a driver-defined query.
     *
     * <p>{@code ${catalog}} and {@code ${schema}} are replaced with the exact
     * {@code catalog} and {@code schema} members of the request, verbatim and
     * unquoted, which is jdbgen's rule and the only one that can work: the text
     * around the variable is the author's, so only the author knows whether it
     * wants {@code '${schema}'}, {@code "${schema}"} or a bare identifier. The
     * queries come from a driver definition the same user wrote, alongside a JDBC
     * URL and a password, so this is not a place where an attacker's string
     * arrives.
     *
     * <p>Two situations produce no query at all rather than a wrong one:
     *
     * <ul>
     *   <li>The query names a variable the request has no exact filter for. There
     *       is nothing to put there, and leaving the text as it stands would run
     *       a query with a literal {@code ${schema}} in it.</li>
     *   <li>The request filters by pattern - {@code schema_pattern},
     *       {@code table_pattern}, {@code column_pattern} - and so does not
     *       describe a single scope the way these queries assume.</li>
     * </ul>
     *
     * <p>Neither case falls back to the built-in query. A definition that names
     * its own comment query has said this product is not what this file thinks it
     * is; answering with the built-in guess anyway would be worse than answering
     * with what the driver said.
     *
     * <p>There is deliberately no {@code ${table}} or {@code ${column}}: one
     * query per describe is the whole point, the scope of that query is a schema,
     * and the narrowing to the rows actually asked for is the overlay's job.
     *
     * @return the query to run, or {@code null} to run none
     */
    private static String expand(String sql, JsonObject req) {
        if (patterned(req, "schema_pattern") || patterned(req, "table_pattern")
                || patterned(req, "column_pattern")) {
            return null;
        }
        String out = sql;
        for (String var : new String[] {"catalog", "schema"}) {
            String token = "${" + var + "}";
            if (!out.contains(token)) {
                continue;
            }
            String value = Json.str(req, var);
            if (value == null || value.isEmpty()) {
                return null;
            }
            // String.replace on two CharSequences is literal, not a regex, so a
            // schema name full of punctuation needs no escaping here.
            out = out.replace(token, value);
        }
        return out;
    }

    private static boolean patterned(JsonObject req, String member) {
        String p = Json.str(req, member);
        return p != null && !p.isEmpty();
    }

    /**
     * Runs a driver-defined comment query and overlays its rows.
     *
     * <p>The key is the table name, and for columns the column name after it,
     * with no namespace part - unlike the built-in path, which knows whether the
     * product calls that namespace a catalog or a schema and can key by it. A
     * custom query could be against any product, so nothing here knows which of
     * the two its rows belong to. Dropping the namespace from the key is safe
     * because the query is already scoped to one of them by its own
     * {@code ${...}} predicate: a collision would need two tables of the same
     * name in a single schema, which no product permits.
     *
     * <p>A plain {@link java.sql.Statement} runs it. There is nothing left to
     * bind after {@link #expand}, and preparing the text would hand the author's
     * every {@code ?} - inside a string literal, or as a PostgreSQL JSON operator
     * - to the driver as a parameter marker.
     *
     * @param what short description for the debug log
     * @param sql  the expanded query
     * @param keys how many leading result columns make up the key
     * @param of   the same key, built from a row of the result being enriched
     */
    private static void overlay(Connection conn, String what, String sql, JsonArray items,
                                int keys, Function<JsonObject, String> of) {
        Attempt.on(conn, what, c -> {
            Map<String, String> found;
            try (Statement st = c.createStatement();
                 ResultSet rs = st.executeQuery(sql)) {
                found = collect(rs, keys);
            }
            for (JsonObject o : missing(items)) {
                fill(o, found.get(of.apply(o)));
            }
            return null;
        }, null);
    }

    // ----------------------------------------------------------------- overlay

    /**
     * @return the row member holding the namespace a comment is keyed by. MySQL
     *         and MariaDB put the database in {@code TABLE_CAT} and leave
     *         {@code TABLE_SCHEM} null; Oracle and SQL Server name a schema.
     */
    private static String owner(Dialect dialect, JsonObject o) {
        return Json.str(o, dialect.isMySqlFamily() ? "catalog" : "schema");
    }

    /** Writes a comment into a row, leaving a blank one as the JSON null it was. */
    private static void fill(JsonObject o, String comment) {
        if (!blank(comment)) {
            o.addProperty("remarks", comment);
        }
    }

    private static List<JsonObject> missing(JsonArray items) {
        List<JsonObject> out = new ArrayList<>();
        for (JsonElement e : items) {
            JsonObject o = e.getAsJsonObject();
            if (blank(Json.str(o, "remarks"))) {
                out.add(o);
            }
        }
        return out;
    }

    private static boolean anyMissing(JsonArray items) {
        return !missing(items).isEmpty();
    }

    private static String key(String... parts) {
        StringBuilder sb = new StringBuilder();
        for (String p : parts) {
            append(sb, p);
        }
        return sb.toString();
    }

    private static void append(StringBuilder sb, String part) {
        sb.append(part == null ? "" : part).append(SEP);
    }

    private static boolean blank(String s) {
        return s == null || s.isBlank();
    }

    // ------------------------------------------------------------------ filter

    /**
     * One name filter from the request, kept together with whether it arrived as
     * an exact name or as a JDBC search pattern - the two need different SQL, and
     * by the time the value is a bare string the difference is gone.
     */
    private static final class Filter {

        private final String value;
        private final boolean pattern;

        private Filter(String value, boolean pattern) {
            this.value = value;
            this.pattern = pattern;
        }

        static Filter of(JsonObject req, String exactKey, String patternKey) {
            String exact = Json.str(req, exactKey);
            if (exact != null && !exact.isEmpty()) {
                return new Filter(exact, false);
            }
            return new Filter(Json.str(req, patternKey), true);
        }

        boolean absent() {
            return value == null || value.isEmpty();
        }

        /** @return the comparison operator and placeholder, {@code escape} included */
        String test(String escape) {
            return pattern ? " LIKE ?" + escape : " = ?";
        }

        /**
         * Appends this filter as a further {@code AND}, if it is set at all. A
         * filter that is not set is not an error: the predicates here only narrow
         * a scan, and the overlay matches on name regardless.
         */
        void append(StringBuilder sql, List<String> args, String column, String escape) {
            if (absent()) {
                return;
            }
            sql.append(" AND ").append(column).append(test(escape));
            args.add(value);
        }
    }
}
