package comart.rudbman.bridge.meta;

import com.google.gson.JsonObject;
import comart.rudbman.bridge.BridgeException;
import comart.rudbman.bridge.Json;

import java.sql.Connection;
import java.sql.DatabaseMetaData;
import java.sql.PreparedStatement;
import java.sql.ResultSet;
import java.sql.SQLException;
import java.sql.Statement;
import java.sql.Types;
import java.util.ArrayList;
import java.util.HashSet;
import java.util.LinkedHashMap;
import java.util.List;
import java.util.Locale;
import java.util.Map;
import java.util.Set;
import java.util.TreeMap;
import java.util.regex.Pattern;
import java.util.logging.Level;
import java.util.logging.Logger;

/**
 * {@code DESCRIBE kind: "ddl"}: the {@code CREATE} text of one table, and the
 * dispatch to {@link ObjectDdl} and {@link RoutineDdl} for every other kind.
 *
 * <p>Answered in two layers, because no single layer is both faithful and
 * universal.
 *
 * <ol>
 *   <li><b>Native.</b> Some servers will quote their own DDL back
 *       ({@code SHOW CREATE TABLE} on MySQL, {@code SCRIPT TABLE} on H2,
 *       {@code DBMS_METADATA} on Oracle, {@code sqlite_master} on SQLite). When
 *       that is available it <em>is</em> the truth, down to storage clauses,
 *       {@code CHECK} constraints and vendor syntax, and nothing reconstructed
 *       here can improve on it.</li>
 *   <li><b>Reverse generation.</b> Otherwise the statement is assembled from
 *       {@link DatabaseMetaData} - {@code getColumns}, {@code getPrimaryKeys},
 *       {@code getImportedKeys}, {@code getIndexInfo}. This path works on every
 *       driver, which is the whole point of having it.</li>
 * </ol>
 *
 * <p>A native attempt that fails - no privilege, an unexpected result shape, a
 * server version that dropped the syntax - falls through to reverse generation
 * rather than failing the request. The response reports which layer answered in
 * its {@code source} member, so the UI can label reconstructed DDL as such.
 *
 * <h2>Limits of the reverse-generated form</h2>
 *
 * <p>It is <b>for display</b>. It is close enough to run in most cases, and the
 * test suite proves the H2 round trip, but it is not a migration tool and must
 * not be sold as one. JDBC metadata simply does not carry:
 *
 * <ul>
 *   <li>{@code CHECK} constraints - there is no accessor for them at all;</li>
 *   <li>triggers, rules and row-level security;</li>
 *   <li>partitioning, tablespaces, storage and compression clauses;</li>
 *   <li>collations, character sets and per-column storage options;</li>
 *   <li>generated / computed column expressions - {@code getColumns} says a
 *       column is generated but never says from what;</li>
 *   <li>{@code UNIQUE} constraints as constraints - they arrive as unique
 *       indexes and are emitted as {@code CREATE UNIQUE INDEX};</li>
 *   <li>view, materialized view and typed-table definitions - a view reaches
 *       this path as a bare column list.</li>
 * </ul>
 *
 * <p>PostgreSQL, SQL Server and Db2, which have no native table DDL, have
 * their own catalogs read for most of that list - declared types, {@code CHECK}
 * and {@code UNIQUE} constraints, computed columns, and the indexes as the
 * server spells them ({@link PgSchema}, {@link SqlServerSchema},
 * {@link Db2Schema}). What remains is partitioning, storage and security.
 *
 * <p>Indexes that merely back a declared key are dropped: an index whose column
 * list is exactly the primary key's is something the server created for that
 * key and will create again, and so is one exactly a foreign key's on MySQL,
 * MariaDB and H2, which make one for every key. Emitting it would make the DDL
 * fail to replay on the very servers it came from.
 */
public final class Ddl {

    private static final Logger LOG = Logger.getLogger(Ddl.class.getName());

    /**
     * MySQL and MariaDB return the server's own {@code CREATE TABLE} text in the
     * second column of a one-row result.
     * Source: MySQL 8.0 Reference Manual, "SHOW CREATE TABLE Statement".
     */
    private static final String MYSQL_SHOW_CREATE = "SHOW CREATE TABLE ";

    /**
     * H2 has no {@code SHOW CREATE TABLE} and, since 2.x, no {@code SQL} column
     * in {@code INFORMATION_SCHEMA.TABLES} either - that column existed in H2
     * 1.4 and was removed when the schema was aligned with the SQL standard.
     * {@code SCRIPT} is what is left. It emits one statement per row and
     * restricting it to a table still prefixes the script with database-wide
     * statements (the user, the schema, aliases, sequences), so the rows are
     * filtered down to the ones naming this table. {@code NODATA} keeps it from
     * reading a single row of the table.
     * Source: H2 1.4.200+/2.x SQL grammar, "SCRIPT".
     */
    private static final String H2_SCRIPT = "SCRIPT NODATA NOPASSWORDS NOSETTINGS TABLE ";

    private Ddl() {
    }

    /**
     * Builds the DDL for one object.
     *
     * <p>Request members: {@code catalog}, {@code schema}, {@code table}
     * (required) and {@code source}, one of {@code auto} (default, native then
     * reverse generation), {@code native} or {@code metadata}. {@code object}
     * says what kind of object is asked for - {@code table} when absent, or
     * {@code view}, {@code sequence}, {@code trigger}, {@code procedure},
     * {@code function} - and names it in {@code name} rather than
     * {@code table}. The kinds other than a table are answered by
     * {@link ObjectDdl} and {@link RoutineDdl}.
     *
     * @param conn    the live connection, for the native catalog queries
     * @param dbm     the connection metadata
     * @param req     the request body
     * @return an object carrying {@code ddl} and {@code source}
     * @throws SQLException if the driver fails on the portable path
     */
    public static JsonObject of(Connection conn, DatabaseMetaData dbm, JsonObject req)
            throws SQLException {
        String want = Json.str(req, "source");
        if (want == null || want.isEmpty()) {
            want = "auto";
        }
        if (!"auto".equals(want) && !"native".equals(want) && !"metadata".equals(want)) {
            throw new BridgeException("protocol",
                    "describe kind 'ddl' accepts source 'auto', 'native' or 'metadata', not '"
                            + want + "'");
        }
        String object = Json.str(req, "object");
        if (object != null && !"table".equals(object)) {
            switch (object) {
                case "view":      return ObjectDdl.view(conn, dbm, req, want);
                case "sequence":  return ObjectDdl.sequence(conn, dbm, req, want);
                case "trigger":   return ObjectDdl.trigger(conn, dbm, req, want);
                case "procedure": return RoutineDdl.of(conn, dbm, req, want, false);
                case "function":  return RoutineDdl.of(conn, dbm, req, want, true);
                default:
                    throw new BridgeException("protocol", "describe kind 'ddl' accepts object "
                            + "'table', 'view', 'sequence', 'trigger', 'procedure' or "
                            + "'function', not '" + object + "'");
            }
        }
        String table = Json.str(req, "table");
        if (table == null || table.isEmpty()) {
            throw new BridgeException("protocol", "describe kind 'ddl' requires an exact 'table'");
        }

        String catalog = Json.str(req, "catalog");
        String schema = Json.str(req, "schema");
        Dialect dialect = Dialect.of(dbm);
        Ident id = Ident.of(dbm);

        if (!"metadata".equals(want)) {
            String text = nativeDdl(conn, dialect, id, catalog, schema, table);
            if (text != null) {
                return result(text, "native");
            }
            if ("native".equals(want)) {
                throw new BridgeException("sql",
                        "no native DDL source for " + dialect + "; retry with source 'metadata'");
            }
        }
        return result(fromMetadata(dbm, dialect, id, catalog, schema, table, null), "metadata");
    }

    /**
     * One object's DDL, split into the statements that create it and the foreign
     * keys that have to wait for every other object to exist.
     */
    public static final class Script {

        /** Which layer answered: {@code native} or {@code metadata}. */
        public final String source;
        /**
         * The {@code CREATE TABLE}, its indexes and its comments, as one blob of
         * text with {@code ;} terminated statements - the same shape
         * {@code DESCRIBE kind: "ddl"} returns.
         */
        public final String creates;
        /** {@code ALTER TABLE … ADD CONSTRAINT … FOREIGN KEY …} statements, unterminated. */
        public final List<String> alters;
        /**
         * Whether {@link #creates} writes a generated (computed) column as one,
         * expression and all. A script must then leave such columns out of its
         * {@code INSERT}s - every product refuses a value for one - and must
         * keep them in where the column came out as a plain one, or the values
         * are lost.
         */
        public final boolean keepsGenerated;

        /**
         * The same script with its {@code CREATE}s replaced.
         *
         * @param replaced the new text
         * @return a copy that differs from this one in {@link #creates} alone
         */
        public Script withCreates(String replaced) {
            return new Script(source, replaced, alters, keepsGenerated);
        }

        Script(String source, String creates, List<String> alters, boolean keepsGenerated) {
            this.source = source;
            this.creates = creates;
            this.alters = alters;
            this.keepsGenerated = keepsGenerated;
        }
    }

    /**
     * An H2 {@code SCRIPT} row adding a foreign key: H2 writes every constraint
     * as a statement of its own, so telling this one apart is a look at the
     * statement's head, never a search inside a statement.
     */
    private static final Pattern H2_FOREIGN_KEY = Pattern.compile(
            "^ALTER TABLE (?:\"(?:[^\"]|\"\")*\"\\.)*\"(?:[^\"]|\"\")*\""
                    + " ADD CONSTRAINT (?:\"(?:[^\"]|\"\")*\"\\.)*\"(?:[^\"]|\"\")*\""
                    + " FOREIGN KEY\\(");

    /**
     * An H2 {@code SCRIPT TABLE} row creating one of the table's triggers. A
     * script writes triggers after the rows, so that loading them fires none,
     * and a trigger inside the table's own DDL would run first.
     */
    private static final Pattern H2_TRIGGER = Pattern.compile("^CREATE (?:FORCE )?TRIGGER ");

    /**
     * Builds one object's DDL for a script file rather than for display.
     *
     * <p>The difference from {@link #of} is the foreign keys. A script has to
     * replay into an empty database, and a schema whose tables reference each
     * other in a cycle - which is common enough that the design calls it out -
     * cannot be replayed in any creation order at all. So the keys are kept out
     * of the way of the creation order, for the caller to write after every
     * {@code CREATE} in the whole script.
     *
     * <p><strong>The server's own text is used wherever it can be split
     * without parsing it.</strong> Finding a foreign key inside vendor SQL by
     * pattern is not a trade this bridge makes, so each product gets the split
     * its own source makes possible:
     *
     * <ul>
     *   <li><b>H2</b> writes every constraint as a statement of its own, so the
     *       foreign keys are the {@code ALTER TABLE … FOREIGN KEY} rows.</li>
     *   <li><b>Oracle</b>'s {@code DBMS_METADATA} leaves them out when asked
     *       ({@code REF_CONSTRAINTS}) and writes each on request
     *       ({@code REF_CONSTRAINT}).</li>
     *   <li><b>MySQL, MariaDB and SQLite</b> keep them inline, where they do no
     *       harm: neither checks a reference while it is created - MySQL with
     *       {@code FOREIGN_KEY_CHECKS} off, which {@link #scriptPrologue} writes,
     *       and SQLite never.</li>
     * </ul>
     *
     * <p>Everything else is reconstructed from {@link DatabaseMetaData}, with
     * the keys lifted out as {@code ALTER}s.
     *
     * @param conn             the live connection
     * @param dbm              the connection metadata
     * @param catalog          catalog, may be {@code null}
     * @param schema           schema, may be {@code null}
     * @param table            the object name, required
     * @param splitForeignKeys whether foreign keys move to {@link Script#alters}
     *                         (or out of the creation order's way)
     * @return the object's script
     * @throws SQLException if the driver fails on the portable path
     */
    public static Script script(Connection conn, DatabaseMetaData dbm, String catalog,
                                String schema, String table, boolean splitForeignKeys)
            throws SQLException {
        Dialect dialect = Dialect.of(dbm);
        Ident id = Ident.of(dbm);
        if (!splitForeignKeys) {
            String text = nativeDdl(conn, dialect, id, catalog, schema, table);
            if (text != null) {
                return new Script("native", text, new ArrayList<>(), true);
            }
            return new Script("metadata",
                    fromMetadata(dbm, dialect, id, catalog, schema, table, null),
                    new ArrayList<>(), reconstructsGenerated(dialect));
        }
        Script split = nativeSplit(conn, dialect, id, catalog, schema, table);
        if (split != null) {
            return split;
        }
        List<String> alters = new ArrayList<>();
        String creates = fromMetadata(dbm, dialect, id, catalog, schema, table, alters);
        return new Script("metadata", creates, alters, reconstructsGenerated(dialect));
    }

    /**
     * What has to come before the rows of a script on this product, and
     * {@link #dataEpilogue} after them. MySQL and MariaDB read a backslash in a
     * string as an escape unless {@code NO_BACKSLASH_ESCAPES} is on, and an
     * {@code AUTO_INCREMENT} column handed a 0 draws a new key unless
     * {@code NO_AUTO_VALUE_ON_ZERO} is; rows go in under exactly the second,
     * as mysqldump has them, and the session's own mode comes back after.
     * Empty for every other product.
     *
     * @param dbm the connection metadata
     * @return the statements, terminated
     */
    public static List<String> dataPrologue(DatabaseMetaData dbm) throws SQLException {
        if (Dialect.of(dbm).isMySqlFamily()) {
            return List.of("SET @OLD_SQL_MODE = @@SESSION.SQL_MODE;",
                    "SET SESSION SQL_MODE = 'NO_AUTO_VALUE_ON_ZERO';");
        }
        return List.of();
    }

    /** @see #dataPrologue */
    public static List<String> dataEpilogue(DatabaseMetaData dbm) throws SQLException {
        if (Dialect.of(dbm).isMySqlFamily()) {
            return List.of("SET SESSION SQL_MODE = @OLD_SQL_MODE;");
        }
        return List.of();
    }

    /**
     * The statements a script of this product's DDL has to start with, so that
     * the {@code CREATE}s {@link #script} writes replay in any order: MySQL's
     * and MariaDB's foreign keys stay inline and need the server to stop
     * checking references. SQL Server's {@code sqlcmd} starts with
     * {@code QUOTED_IDENTIFIER} off, which refuses a persisted computed column
     * and is not what the objects were created under; SSMS writes the same
     * two settings at the head of its scripts. Empty for every other product.
     *
     * @param dbm the connection metadata
     * @return the statements, terminated
     */
    public static List<String> scriptPrologue(DatabaseMetaData dbm) {
        List<String> out = new ArrayList<>();
        Dialect dialect = Dialect.of(dbm);
        if (dialect.isMySqlFamily()) {
            out.add("SET FOREIGN_KEY_CHECKS = 0;");
        } else if (dialect == Dialect.SQLSERVER) {
            out.add("SET ANSI_NULLS ON;");
            out.add("SET QUOTED_IDENTIFIER ON;");
        }
        return out;
    }

    /**
     * What undoes {@link #scriptPrologue} at the end of the script.
     *
     * @param dbm the connection metadata
     * @return the statements, terminated
     */
    public static List<String> scriptEpilogue(DatabaseMetaData dbm) {
        List<String> out = new ArrayList<>();
        if (Dialect.of(dbm).isMySqlFamily()) {
            out.add("SET FOREIGN_KEY_CHECKS = 1;");
        }
        return out;
    }

    /**
     * Whether the reconstruction writes a generated column as one: only where
     * a catalog query below recovers the expression.
     */
    private static boolean reconstructsGenerated(Dialect dialect) {
        return dialect == Dialect.POSTGRESQL || dialect == Dialect.SQLSERVER
                || dialect == Dialect.DB2;
    }

    /**
     * The server's own text with the foreign keys out of the creation order's
     * way, or {@code null} for a product whose text cannot be split that way.
     */
    private static Script nativeSplit(Connection conn, Dialect dialect, Ident id,
                                      String catalog, String schema, String table) {
        if (dialect.isMySqlFamily() || dialect == Dialect.SQLITE) {
            String text = nativeDdl(conn, dialect, id, catalog, schema, table);
            return text == null ? null : new Script("native", text, new ArrayList<>(), true);
        }
        if (dialect == Dialect.H2) {
            List<String> rows = Attempt.<List<String>>run(conn, "SCRIPT TABLE",
                    st -> h2Statements(st, id.qualify(catalog, schema, table), schema, table),
                    null);
            if (rows == null) {
                return null;
            }
            List<String> creates = new ArrayList<>();
            List<String> alters = new ArrayList<>();
            for (String row : rows) {
                if (H2_TRIGGER.matcher(row).find()) {
                    continue;
                }
                if (H2_FOREIGN_KEY.matcher(row).find()) {
                    alters.add(row.endsWith(";") ? row.substring(0, row.length() - 1) : row);
                } else {
                    creates.add(row);
                }
            }
            return new Script("native", String.join("\n", creates), alters, true);
        }
        if (dialect == Dialect.ORACLE) {
            String text = oracle(conn, schema, table, false);
            if (text == null) {
                return null;
            }
            List<String> alters = new ArrayList<>();
            for (String fk : oracleForeignKeys(conn, schema, table)) {
                String one = DdlText.oracleMetadata(conn, "REF_CONSTRAINT", schema, fk);
                if (one == null) {
                    // A key the account may see but not script: the whole table
                    // goes the reconstruction's way rather than lose it.
                    return null;
                }
                alters.add(one.endsWith(";") ? one.substring(0, one.length() - 1).strip() : one);
            }
            return new Script("native", text, alters, true);
        }
        return null;
    }

    /** One column as a data script sees it. */
    public static final class DataColumn {
        /** The exact name. */
        public final String name;
        /** {@code IS_AUTOINCREMENT}: an identity, serial or auto-increment column. */
        public final boolean autoIncrement;
        /** {@code IS_GENERATEDCOLUMN}, and not merely an identity. */
        public final boolean generated;
        /**
         * A PostgreSQL {@code GENERATED ALWAYS} identity: an {@code INSERT} of
         * a value for it has to say {@code OVERRIDING SYSTEM VALUE}.
         */
        public final boolean identityAlways;

        DataColumn(String name, boolean autoIncrement, boolean generated,
                   boolean identityAlways) {
            this.name = name;
            this.autoIncrement = autoIncrement;
            this.generated = generated && !autoIncrement;
            this.identityAlways = identityAlways;
        }
    }

    /**
     * A table's columns in order, with what an {@code INSERT} script needs to
     * know of each: whether the server computes it, and whether it is an
     * identity.
     *
     * @param dbm     the connection metadata
     * @param catalog catalog, may be {@code null}
     * @param schema  schema, may be {@code null}
     * @param table   the exact table name
     * @return the columns, empty when the driver lists none
     * @throws SQLException if the driver fails on {@code getColumns}
     */
    public static List<DataColumn> dataColumns(DatabaseMetaData dbm, String catalog,
                                               String schema, String table)
            throws SQLException {
        List<DataColumn> out = new ArrayList<>();
        List<Col> cols = readColumns(dbm, catalog, schema, table);
        boolean anyIdentity = false;
        for (Col c : cols) {
            anyIdentity |= c.autoIncrement;
        }
        Dialect dialect = Dialect.of(dbm);
        Set<String> always = !anyIdentity ? new HashSet<>()
                : dialect == Dialect.POSTGRESQL ? pgIdentityAlways(dbm.getConnection(), table, schema)
                : dialect == Dialect.H2 ? h2IdentityAlways(dbm.getConnection(), schema, table)
                : new HashSet<>();
        for (Col c : cols) {
            out.add(new DataColumn(c.name, c.autoIncrement, c.generated,
                    c.autoIncrement && always.contains(c.name)));
        }
        return out;
    }

    /**
     * An Oracle package, specification and body, as {@code ALL_SOURCE} keeps
     * it - {@code DBMS_METADATA} when that cannot be read - each part ending in
     * the {@code /} SQL*Plus runs a PL/SQL unit on.
     *
     * @param conn   the live connection
     * @param schema the owner, or {@code null} for the session's
     * @param name   the package name
     * @return the text, or {@code null} when the account may not read it
     */
    public static String oraclePackage(Connection conn, String schema, String name) {
        // ALL_SOURCE, as for a function: what DBMS_METADATA writes comes back
        // stored as PACKAGE "NAME", not as the package was written.
        String spec = DdlText.oracleSource(conn, schema, name, "PACKAGE");
        if (spec == null) {
            return DdlText.oracleMetadata(conn, "PACKAGE", schema, name);
        }
        String body = DdlText.oracleSource(conn, schema, name, "PACKAGE BODY");
        return spec + "\n/" + (body == null ? "" : "\n\n" + body + "\n/");
    }

    /** The names of a table's foreign keys, from {@code ALL_CONSTRAINTS}. */
    private static List<String> oracleForeignKeys(Connection conn, String schema, String table) {
        return Attempt.<List<String>>on(conn, "ALL_CONSTRAINTS", c -> {
            String sql = "SELECT CONSTRAINT_NAME FROM ALL_CONSTRAINTS WHERE OWNER = "
                    + DdlText.owner(schema) + " AND TABLE_NAME = ? AND CONSTRAINT_TYPE = 'R'"
                    + " ORDER BY CONSTRAINT_NAME";
            List<String> out = new ArrayList<>();
            try (PreparedStatement ps = c.prepareStatement(sql)) {
                int i = 1;
                if (schema != null && !schema.isEmpty()) {
                    ps.setString(i++, schema);
                }
                ps.setString(i, table);
                try (ResultSet rs = ps.executeQuery()) {
                    while (rs.next()) {
                        out.add(rs.getString(1));
                    }
                }
            }
            return out;
        }, new ArrayList<>());
    }

    /**
     * The exact object name of a request about something other than a table:
     * {@code name}, or {@code table} from a caller that still says that.
     */
    static String objectName(JsonObject req) {
        String name = Json.str(req, "name");
        if (name == null || name.isEmpty()) {
            name = Json.str(req, "table");
        }
        if (name == null || name.isEmpty()) {
            throw new BridgeException("protocol",
                    "describe kind 'ddl' for this object requires an exact 'name'");
        }
        return name;
    }

    static JsonObject result(String ddl, String source) {
        JsonObject o = new JsonObject();
        o.addProperty("ddl", ddl);
        o.addProperty("source", source);
        return o;
    }

    // ---------------------------------------------------------------- native

    /**
     * Asks the server for its own DDL.
     *
     * <p>Returns {@code null} rather than throwing whenever the attempt does not
     * produce usable text, because the caller has a working fallback and a
     * missing privilege is not worth failing a request over. The statement runs
     * on a plain {@link Statement} that is closed immediately; nothing about the
     * session state is touched.
     */
    private static String nativeDdl(Connection conn, Dialect dialect, Ident id,
                                    String catalog, String schema, String table) {
        String qualified = id.qualify(catalog, schema, table);
        if (dialect.isMySqlFamily()) {
            return Attempt.<String>run(conn, "SHOW CREATE TABLE",
                    st -> mysqlShowCreate(st, qualified), null);
        }
        if (dialect == Dialect.H2) {
            return Attempt.<String>run(conn, "SCRIPT TABLE",
                    st -> h2Script(st, qualified, schema, table), null);
        }
        if (dialect == Dialect.ORACLE) {
            return oracle(conn, schema, table, true);
        }
        if (dialect == Dialect.SQLITE) {
            return sqlite(conn, table);
        }
        return null;
    }

    /**
     * Oracle's own {@code CREATE TABLE}, check constraints and all, then the
     * indexes that do not merely back a key - the ones that do are created by
     * the table's own constraints - and the comments.
     *
     * @param refConstraints whether the foreign keys are written into the
     *                       {@code CREATE}
     */
    private static String oracle(Connection conn, String schema, String table,
                                 boolean refConstraints) {
        String create = DdlText.oracleMetadata(conn, "TABLE", schema, table, refConstraints);
        if (create == null) {
            return null;
        }
        List<String> parts = new ArrayList<>();
        parts.add(create);
        parts.addAll(oracleIndexes(conn, schema, table));
        String comments = DdlText.oracleDependent(conn, "COMMENT", schema, table);
        if (comments != null) {
            parts.add(comments);
        }
        return DdlText.joined(parts);
    }

    /**
     * The {@code CREATE INDEX} of each of a table's - or a materialized view's
     * - indexes that does not merely back a key, as {@code DBMS_METADATA}
     * writes it.
     */
    static List<String> oracleIndexes(Connection conn, String schema, String table) {
        List<String> parts = new ArrayList<>();
        List<String> indexes = Attempt.<List<String>>on(conn, "ALL_INDEXES", c -> {
            String sql = "SELECT i.INDEX_NAME FROM ALL_INDEXES i WHERE i.TABLE_OWNER = "
                    + DdlText.owner(schema) + " AND i.TABLE_NAME = ? AND i.INDEX_TYPE <> 'LOB'"
                    // The one a materialized view makes for its own refresh.
                    + " AND i.GENERATED = 'N' AND i.INDEX_NAME NOT LIKE 'I\\_SNAP$\\_%' ESCAPE '\\'"
                    + " AND NOT EXISTS (SELECT 1 FROM ALL_CONSTRAINTS k"
                    + " WHERE k.OWNER = i.OWNER AND k.INDEX_NAME = i.INDEX_NAME"
                    + " AND k.CONSTRAINT_TYPE IN ('P', 'U')) ORDER BY i.INDEX_NAME";
            List<String> out = new ArrayList<>();
            try (PreparedStatement ps = c.prepareStatement(sql)) {
                int i = 1;
                if (schema != null && !schema.isEmpty()) {
                    ps.setString(i++, schema);
                }
                ps.setString(i, table);
                try (ResultSet rs = ps.executeQuery()) {
                    while (rs.next()) {
                        out.add(rs.getString(1));
                    }
                }
            }
            return out;
        }, new ArrayList<>());
        for (String index : indexes) {
            String text = DdlText.oracleMetadata(conn, "INDEX", schema, index);
            if (text != null) {
                parts.add(text);
            }
        }
        return parts;
    }

    /**
     * SQLite keeps the {@code CREATE TABLE} exactly as typed, and each
     * {@code CREATE INDEX} beside it; an index with no text is the one the
     * table's own {@code UNIQUE} or key made.
     */
    private static String sqlite(Connection conn, String table) {
        return Attempt.<String>on(conn, "sqlite_master", c -> {
            List<String> parts = new ArrayList<>();
            try (PreparedStatement ps = c.prepareStatement("SELECT sql FROM sqlite_master"
                    + " WHERE tbl_name = ? AND type IN ('table', 'index') AND sql IS NOT NULL"
                    + " ORDER BY CASE type WHEN 'table' THEN 0 ELSE 1 END, name")) {
                ps.setString(1, table);
                try (ResultSet rs = ps.executeQuery()) {
                    while (rs.next()) {
                        String text = DdlText.terminated(rs.getString(1));
                        if (text != null) {
                            parts.add(text);
                        }
                    }
                }
            }
            return parts.isEmpty() || !parts.get(0).toUpperCase(Locale.ROOT)
                    .startsWith("CREATE") ? null : DdlText.joined(parts);
        }, null);
    }

    private static String mysqlShowCreate(Statement st, String qualified) throws SQLException {
        try (ResultSet rs = st.executeQuery(MYSQL_SHOW_CREATE + qualified)) {
            // Column 1 is the table name, column 2 the DDL. Read by position:
            // the label is "Create Table" for tables and "Create View" for
            // views, and MariaDB adds two more columns for views.
            if (rs.next() && rs.getMetaData().getColumnCount() >= 2) {
                String ddl = DdlText.withoutDefiner(rs.getString(2));
                if (ddl != null && !ddl.isBlank()) {
                    return ddl.endsWith(";") ? ddl : ddl + ";";
                }
            }
            return null;
        }
    }

    /**
     * Runs H2's {@code SCRIPT} and keeps only the statements about this table.
     *
     * <p>{@code SCRIPT ... TABLE t} still emits the surrounding database - the
     * user, the schema, every alias and sequence - because those are what the
     * table needs to be restored into. For "show me this table's DDL" they are
     * noise. H2 always writes fully quoted, fully qualified names in a script,
     * so a row belongs to this table exactly when it contains
     * {@code "SCHEMA"."TABLE"}. Rows starting with {@code --} are H2's row-count
     * comments and are dropped by the same test only accidentally, so they are
     * excluded explicitly.
     */
    private static String h2Script(Statement st, String qualified, String schema, String table)
            throws SQLException {
        List<String> rows = h2Statements(st, qualified, schema, table);
        return rows == null ? null : String.join("\n", rows);
    }

    /** {@link #h2Script}'s statements one by one, or {@code null}. */
    /**
     * The schema's H2 domains, which its columns may be declared with, as
     * {@code SCRIPT} writes them: each {@code CREATE DOMAIN}, then the
     * {@code ALTER DOMAIN}s that add its checks.
     *
     * @param conn   the live connection
     * @param id     the quoting rules
     * @param schema the schema, which must be named
     * @return each statement, terminated, with the {@code DROP} that removes
     *         what it creates, or {@code null} beside an {@code ALTER}
     */
    public static List<String[]> h2Domains(Connection conn, Ident id, String schema) {
        if (schema == null || schema.isEmpty()) {
            return new ArrayList<>();
        }
        return Attempt.<List<String[]>>run(conn, "SCRIPT SCHEMA", st -> {
            String ref = "\"" + schema.replace("\"", "\"\"") + "\".";
            List<String[]> creates = new ArrayList<>();
            List<String[]> alters = new ArrayList<>();
            try (ResultSet rs = st.executeQuery("SCRIPT NODATA NOPASSWORDS NOSETTINGS NOVERSION"
                    + " SCHEMA " + id.q(schema))) {
                while (rs.next()) {
                    String stmt = rs.getString(1) == null ? "" : rs.getString(1).trim();
                    String upper = stmt.toUpperCase(Locale.ROOT);
                    if (upper.startsWith("CREATE DOMAIN " + ref.toUpperCase(Locale.ROOT))) {
                        String name = stmt.substring("CREATE DOMAIN ".length()).split("\\s")[0];
                        creates.add(new String[]{DdlText.terminated(stmt),
                                "DROP DOMAIN IF EXISTS " + name + ";"});
                    } else if (upper.startsWith("ALTER DOMAIN " + ref.toUpperCase(Locale.ROOT))) {
                        alters.add(new String[]{DdlText.terminated(stmt), null});
                    }
                }
            }
            creates.addAll(alters);
            return creates;
        }, new ArrayList<>());
    }

    private static List<String> h2Statements(Statement st, String qualified, String schema,
                                             String table) throws SQLException {
        String ref = schema == null || schema.isEmpty()
                ? '"' + table + '"'
                : '"' + schema + "\".\"" + table + '"';
        List<String> out = new ArrayList<>();
        boolean sawTable = false;
        try (ResultSet rs = st.executeQuery(H2_SCRIPT + qualified)) {
            while (rs.next()) {
                String stmt = rs.getString(1);
                if (stmt == null) {
                    continue;
                }
                stmt = stmt.trim();
                if (stmt.isEmpty() || stmt.startsWith("--") || !stmt.contains(ref)) {
                    continue;
                }
                String upper = stmt.toUpperCase(Locale.ROOT);
                if (upper.startsWith("INSERT ")) {
                    continue;
                }
                if (upper.startsWith("CREATE ") && upper.contains("TABLE ")) {
                    sawTable = true;
                }
                out.add(stmt);
            }
        }
        // Without a CREATE statement the filter matched nothing useful - a
        // renamed schema, a quoting convention this code did not predict - and
        // half a script is worse than a reconstruction.
        return sawTable ? out : null;
    }

    // -------------------------------------------------------------- metadata

    /** One column of the table being rendered. */
    private static final class Col {
        String name;
        String typeName;
        Integer dataType;
        Integer size;
        Integer digits;
        boolean notNull;
        String def;
        boolean autoIncrement;
        /** A PostgreSQL {@code GENERATED ALWAYS} identity, which refuses a value. */
        boolean identityAlways;
        /** SQL Server's {@code IDENTITY(seed, increment)}, when the catalog says. */
        String identitySpec;
        /** {@code IS_GENERATEDCOLUMN}: a computed column, its value not stored by hand. */
        boolean generated;
        /**
         * What follows the name when the product's own catalog says how the
         * column is computed - the whole rest of its definition - or
         * {@code null} for an ordinary column.
         */
        String computed;
        /**
         * The type as the product's own catalog spells it, parameters and all,
         * where JDBC's {@code TYPE_NAME} and sizes cannot say it: PostgreSQL's
         * {@code format_type}, which keeps {@code bit varying(8)} and a
         * {@code timestamp} declared without a precision.
         */
        String nativeType;
        /** SQL Server: the name of the column's default constraint. */
        String defaultName;
        String remarks;
    }

    /** One foreign key, with its columns already ordered by {@code KEY_SEQ}. */
    private static final class Fk {
        String name;
        String pkCatalog;
        String pkSchema;
        String pkTable;
        final TreeMap<Integer, String> fkCols = new TreeMap<>();
        final TreeMap<Integer, String> pkCols = new TreeMap<>();
        Integer updateRule;
        Integer deleteRule;
        /** {@code DEFERRABILITY}, one of {@code DatabaseMetaData.importedKey*}. */
        Integer deferrability;
    }

    /** One index, with its columns already ordered by {@code ORDINAL_POSITION}. */
    private static final class Idx {
        String name;
        boolean unique;
        boolean usable = true;
        final TreeMap<Integer, String> cols = new TreeMap<>();
        final TreeMap<Integer, String> order = new TreeMap<>();
    }

    /**
     * @param alterOut when non-{@code null}, foreign keys are appended here as
     *                 {@code ALTER TABLE} statements instead of being written
     *                 into the {@code CREATE} body
     */
    private static String fromMetadata(DatabaseMetaData dbm, Dialect dialect, Ident id,
                                       String catalog, String schema, String table,
                                       List<String> alterOut)
            throws SQLException {
        List<Col> cols = readColumns(dbm, catalog, schema, table);
        if (cols.isEmpty()) {
            throw new BridgeException("sql",
                    "no such table, or no columns visible: "
                            + id.qualify(catalog, schema, table));
        }
        List<String> pkCols = new ArrayList<>();
        String pkName = readPrimaryKey(dbm, catalog, schema, table, pkCols);
        List<Fk> fks = readForeignKeys(dbm, catalog, schema, table);
        List<Idx> indexes = readIndexes(dbm, dialect, catalog, schema, table, pkCols, fks);
        String tableRemarks = readTableRemarks(dbm, catalog, schema, table);

        // A PostgreSQL connection sees one database, and a name qualified by it
        // reads as a cross-database reference, which PostgreSQL does not have.
        String nameCatalog = dialect == Dialect.POSTGRESQL ? null : catalog;
        String qualified = id.qualify(nameCatalog, schema, table);
        List<String> checks = catalogExtras(dbm.getConnection(), dialect, id, catalog, schema,
                table, qualified, cols);
        StringBuilder sb = new StringBuilder();
        sb.append("CREATE TABLE ").append(qualified).append(" (\n");

        List<String> body = new ArrayList<>();
        for (Col c : cols) {
            body.add("    " + column(c, dialect, id));
        }
        if (!pkCols.isEmpty()) {
            StringBuilder pk = new StringBuilder("    ");
            if (pkName != null && !pkName.isEmpty()) {
                pk.append("CONSTRAINT ").append(id.q(pkName)).append(' ');
            }
            String kind = dialect == Dialect.SQLSERVER ? SqlServerSchema.primaryKeyKind(
                    dbm.getConnection(), id, catalog, id.qualify(catalog, schema, table)) : null;
            pk.append("PRIMARY KEY ").append(kind == null ? "" : kind + " ")
                    .append('(').append(columnList(pkCols, id)).append(')');
            body.add(pk.toString());
        }
        for (String check : checks) {
            body.add("    " + check);
        }
        boolean catalogQualified = nameCatalog != null && !nameCatalog.isEmpty();
        // Only PostgreSQL among the products this path serves can defer a key.
        boolean deferrable = dialect == Dialect.POSTGRESQL;
        for (Fk fk : fks) {
            if (alterOut == null) {
                body.add("    " + foreignKey(fk, id, catalogQualified, deferrable));
            } else {
                alterOut.add("ALTER TABLE " + qualified + " ADD "
                        + foreignKey(fk, id, catalogQualified, deferrable));
            }
        }
        sb.append(String.join(",\n", body)).append("\n)");

        // MySQL has no COMMENT ON; a table comment is part of the CREATE.
        if (dialect.isMySqlFamily() && tableRemarks != null && !tableRemarks.isEmpty()) {
            sb.append(" COMMENT=").append(Ident.literal(tableRemarks));
        }
        sb.append(";\n");

        List<String> nativeIndexes = dialect == Dialect.POSTGRESQL
                ? pgIndexes(dbm.getConnection(), schema, table, qualified)
                : dialect == Dialect.SQLSERVER
                ? SqlServerSchema.indexes(dbm.getConnection(), id, catalog,
                        id.qualify(catalog, schema, table), qualified)
                : dialect == Dialect.DB2 && schema != null && !schema.isEmpty()
                ? Db2Schema.indexes(dbm.getConnection(), id, schema, table, qualified)
                : null;
        if (nativeIndexes != null) {
            for (String ix : nativeIndexes) {
                sb.append('\n').append(ix).append(";\n");
            }
        } else {
            for (Idx ix : indexes) {
                sb.append('\n').append(index(ix, qualified, id)).append(";\n");
            }
        }

        appendComments(sb, dialect, id, qualified, tableRemarks, cols);
        if (dialect == Dialect.SQLSERVER) {
            List<String> props = SqlServerSchema.extendedProperties(dbm.getConnection(), id,
                    catalog, id.qualify(catalog, schema, table), schema, table);
            if (!props.isEmpty()) {
                sb.append('\n').append(String.join("\n", props)).append('\n');
            }
        }
        return sb.toString();
    }

    private static String column(Col c, Dialect dialect, Ident id) {
        StringBuilder sb = new StringBuilder();
        if (c.computed != null) {
            // SQL Server's computed column has no type of its own to write.
            if (dialect == Dialect.SQLSERVER) {
                return sb.append(id.q(c.name)).append(' ').append(c.computed).toString();
            }
            sb.append(id.q(c.name)).append(' ').append(typeText(c, dialect))
                    .append(' ').append(c.computed);
            if (c.notNull) {
                sb.append(" NOT NULL");
            }
            return sb.toString();
        }
        if (dialect == Dialect.POSTGRESQL && c.autoIncrement && c.def != null
                && c.def.trim().startsWith("nextval(")) {
            // A serial column: pgjdbc names its type "serial", which would make
            // the server create the sequence again under the same name, and
            // calls it auto-increment, which would make it an identity. What the
            // catalog holds is an integer whose default draws on a sequence of
            // its own - written that way, as pg_dump does, the sequence being
            // created first.
            sb.append(id.q(c.name)).append(' ').append(serialBase(c.typeName))
                    .append(" DEFAULT ").append(c.def.trim());
            if (c.notNull) {
                sb.append(" NOT NULL");
            }
            return sb.toString();
        }
        sb.append(id.q(c.name)).append(' ').append(typeText(c, dialect));
        String identity = c.identityAlways ? "GENERATED ALWAYS AS IDENTITY"
                : c.identitySpec != null ? c.identitySpec : identityClause(dialect);
        if (dialect.isMySqlFamily()) {
            // MySQL's column grammar fixes this order:
            // type [NOT NULL] [DEFAULT] [AUTO_INCREMENT] [COMMENT].
            if (c.notNull) {
                sb.append(" NOT NULL");
            }
            appendDefault(sb, c, id);
            if (c.autoIncrement) {
                sb.append(' ').append(identity);
            }
            if (c.remarks != null && !c.remarks.isEmpty()) {
                sb.append(" COMMENT ").append(Ident.literal(c.remarks));
            }
        } else {
            // Standard SQL puts identity and DEFAULT in the same slot; they are
            // alternatives, never both, which is also why an auto-increment
            // column's COLUMN_DEF (PostgreSQL reports nextval(...)) is dropped.
            if (c.autoIncrement) {
                sb.append(' ').append(identity);
            } else {
                appendDefault(sb, c, id);
            }
            if (c.notNull) {
                sb.append(" NOT NULL");
            }
        }
        return sb.toString();
    }

    /** The integer a PostgreSQL serial pseudo-type stands for. */
    private static String serialBase(String typeName) {
        String t = typeName == null ? "" : typeName.trim().toLowerCase(Locale.ROOT);
        switch (t) {
            case "bigserial":
            case "serial8":
                return "bigint";
            case "smallserial":
            case "serial2":
                return "smallint";
            case "serial":
            case "serial4":
                return "integer";
            default:
                return typeName == null ? "integer" : typeName.trim();
        }
    }

    /**
     * What the product's own catalog knows and JDBC metadata does not: the
     * {@code CHECK} constraints, returned as table-body lines, and the
     * expressions of generated columns, set on the columns. Only PostgreSQL
     * and SQL Server are asked; every other product's reconstruction stays
     * what JDBC alone gives. A query that fails - an old server, a missing
     * privilege - leaves the reconstruction as it would have been.
     */
    private static List<String> catalogExtras(Connection conn, Dialect dialect, Ident id,
                                              String catalog, String schema, String table,
                                              String qualified, List<Col> cols) {
        List<String> checks = new ArrayList<>();
        boolean scoped = schema != null && !schema.isEmpty();
        if (dialect == Dialect.POSTGRESQL) {
            String rel = " JOIN pg_catalog.pg_class c ON c.oid = %s"
                    + " JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace"
                    + " WHERE c.relname = ?"
                    + (scoped ? " AND n.nspname = ?" : " AND pg_catalog.pg_table_is_visible(c.oid)");
            checks.addAll(Attempt.<List<String>>on(conn, "pg_constraint", c -> {
                List<String> out = new ArrayList<>();
                try (PreparedStatement ps = c.prepareStatement("SELECT k.conname,"
                        + " pg_catalog.pg_get_constraintdef(k.oid, true)"
                        + " FROM pg_catalog.pg_constraint k"
                        + String.format(Locale.ROOT, rel, "k.conrelid")
                        // UNIQUE and EXCLUDE too: written as the constraints
                        // they are, not as the indexes that back them.
                        + " AND k.contype IN ('u', 'x', 'c')"
                        + " ORDER BY k.contype DESC, k.conname")) {
                    bindRelation(ps, table, schema);
                    try (ResultSet rs = ps.executeQuery()) {
                        while (rs.next()) {
                            out.add("CONSTRAINT " + id.q(rs.getString(1)) + " " + rs.getString(2));
                        }
                    }
                }
                return out;
            }, new ArrayList<>()));
            Map<String, String> types = Attempt.<Map<String, String>>on(conn, "format_type", c -> {
                Map<String, String> out = new LinkedHashMap<>();
                try (PreparedStatement ps = c.prepareStatement("SELECT a.attname,"
                        + " pg_catalog.format_type(a.atttypid, a.atttypmod)"
                        + " FROM pg_catalog.pg_attribute a"
                        + String.format(Locale.ROOT, rel, "a.attrelid")
                        + " AND a.attnum > 0 AND NOT a.attisdropped")) {
                    bindRelation(ps, table, schema);
                    try (ResultSet rs = ps.executeQuery()) {
                        while (rs.next()) {
                            out.put(rs.getString(1), rs.getString(2));
                        }
                    }
                }
                return out;
            }, new LinkedHashMap<>());
            for (Col col : cols) {
                col.nativeType = types.get(col.name);
            }
            Set<String> always = pgIdentityAlways(conn, table, schema);
            for (Col col : cols) {
                col.identityAlways = col.autoIncrement && always.contains(col.name);
            }
            boolean anyGenerated = false;
            for (Col col : cols) {
                anyGenerated |= col.generated && !col.autoIncrement;
            }
            if (anyGenerated) {
                // attgenerated: 's' stored, 'v' virtual (PostgreSQL 18). The
                // expression is COLUMN_DEF, which pgjdbc reads from the same
                // place a default lives.
                Map<String, String> kinds = Attempt.<Map<String, String>>on(conn, "pg_attribute",
                        c -> {
                            Map<String, String> out = new LinkedHashMap<>();
                            try (PreparedStatement ps = c.prepareStatement("SELECT a.attname,"
                                    + " a.attgenerated::text FROM pg_catalog.pg_attribute a"
                                    + String.format(Locale.ROOT, rel, "a.attrelid")
                                    + " AND a.attgenerated <> ''")) {
                                bindRelation(ps, table, schema);
                                try (ResultSet rs = ps.executeQuery()) {
                                    while (rs.next()) {
                                        out.put(rs.getString(1), rs.getString(2));
                                    }
                                }
                            }
                            return out;
                        }, new LinkedHashMap<>());
                for (Col col : cols) {
                    String kind = kinds.get(col.name);
                    if (kind != null && col.def != null && !col.def.trim().isEmpty()) {
                        col.computed = "GENERATED ALWAYS AS (" + col.def.trim() + ") "
                                + ("v".equals(kind) ? "VIRTUAL" : "STORED");
                    }
                }
            }
        } else if (dialect == Dialect.DB2 && scoped) {
            Map<String, String> types = Db2Schema.columnTypes(conn, id, schema, table);
            Map<String, String> identities = new LinkedHashMap<>();
            Map<String, String> computed = Db2Schema.generated(conn, schema, table, identities);
            for (Col col : cols) {
                col.nativeType = types.get(col.name);
                col.computed = computed.get(col.name);
                String identity = identities.get(col.name);
                if (identity != null) {
                    col.autoIncrement = true;
                    col.identitySpec = identity;
                }
            }
            checks.addAll(Db2Schema.constraints(conn, id, schema, table));
        } else if (dialect == Dialect.SQLSERVER) {
            String object = id.qualify(catalog, schema, table);
            Map<String, String> types = SqlServerSchema.columnTypes(conn, id, catalog, object);
            Map<String, String> defaults = SqlServerSchema.defaultNames(conn, id, catalog, object);
            for (Col col : cols) {
                col.nativeType = types.get(col.name);
                col.defaultName = defaults.get(col.name);
            }
            checks.addAll(SqlServerSchema.uniqueConstraints(conn, id, catalog, object));
            checks.addAll(Attempt.<List<String>>on(conn, "sys.check_constraints", c -> {
                List<String> out = new ArrayList<>();
                try (PreparedStatement ps = c.prepareStatement("SELECT name, definition"
                        + " FROM " + SqlServerSchema.sys(id, catalog, "check_constraints")
                        + " WHERE parent_object_id = OBJECT_ID(?)"
                        + " ORDER BY name")) {
                    ps.setString(1, object);
                    try (ResultSet rs = ps.executeQuery()) {
                        while (rs.next()) {
                            out.add("CONSTRAINT " + id.q(rs.getString(1)) + " CHECK "
                                    + rs.getString(2));
                        }
                    }
                }
                return out;
            }, new ArrayList<>()));
            Map<String, String> computed = Attempt.<Map<String, String>>on(conn,
                    "sys.computed_columns", c -> {
                        Map<String, String> out = new LinkedHashMap<>();
                        try (PreparedStatement ps = c.prepareStatement("SELECT name, definition,"
                                + " is_persisted FROM "
                                + SqlServerSchema.sys(id, catalog, "computed_columns")
                                + " WHERE object_id = OBJECT_ID(?)")) {
                            ps.setString(1, object);
                            try (ResultSet rs = ps.executeQuery()) {
                                while (rs.next()) {
                                    String name = rs.getString(1);
                                    String definition = rs.getString(2);
                                    boolean persisted = rs.getBoolean(3);
                                    out.put(name, "AS " + definition
                                            + (persisted ? " PERSISTED" : ""));
                                }
                            }
                        }
                        return out;
                    }, new LinkedHashMap<>());
            for (Col col : cols) {
                col.computed = computed.get(col.name);
            }
            Map<String, String> identities = Attempt.<Map<String, String>>on(conn,
                    "sys.identity_columns", c -> {
                        Map<String, String> out = new LinkedHashMap<>();
                        try (PreparedStatement ps = c.prepareStatement("SELECT name,"
                                + " CAST(seed_value AS VARCHAR(40)),"
                                + " CAST(increment_value AS VARCHAR(40))"
                                + " FROM " + SqlServerSchema.sys(id, catalog, "identity_columns")
                                + " WHERE object_id = OBJECT_ID(?)")) {
                            ps.setString(1, object);
                            try (ResultSet rs = ps.executeQuery()) {
                                while (rs.next()) {
                                    out.put(rs.getString(1), "IDENTITY(" + rs.getString(2) + ", "
                                            + rs.getString(3) + ")");
                                }
                            }
                        }
                        return out;
                    }, new LinkedHashMap<>());
            for (Col col : cols) {
                if (col.autoIncrement) {
                    col.identitySpec = identities.get(col.name);
                }
            }
        }
        return checks;
    }

    /**
     * The columns of a PostgreSQL table that are {@code GENERATED ALWAYS AS
     * IDENTITY}, which pgjdbc reports as auto-increment and no more. Empty for
     * an old server or a refused query.
     */
    private static Set<String> pgIdentityAlways(Connection conn, String table, String schema) {
        boolean scoped = schema != null && !schema.isEmpty();
        return Attempt.<Set<String>>on(conn, "pg_attribute attidentity", c -> {
            Set<String> out = new HashSet<>();
            try (PreparedStatement ps = c.prepareStatement("SELECT a.attname"
                    + " FROM pg_catalog.pg_attribute a"
                    + " JOIN pg_catalog.pg_class c ON c.oid = a.attrelid"
                    + " JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace"
                    + " WHERE c.relname = ?"
                    + (scoped ? " AND n.nspname = ?" : " AND pg_catalog.pg_table_is_visible(c.oid)")
                    + " AND a.attidentity = 'a'")) {
                bindRelation(ps, table, schema);
                try (ResultSet rs = ps.executeQuery()) {
                    while (rs.next()) {
                        out.add(rs.getString(1));
                    }
                }
            }
            return out;
        }, new HashSet<>());
    }

    /**
     * A PostgreSQL table's indexes as the server writes them, less the ones a
     * key or a {@code UNIQUE}/{@code EXCLUDE} constraint owns, which the
     * {@code CREATE TABLE} declares. JDBC metadata cannot carry what an index
     * is over beyond plain columns: pgjdbc puts an expression where the column
     * name goes, and has no place for the method, the {@code WHERE} of a
     * partial index, {@code INCLUDE}, an operator class or a collation.
     *
     * <p>{@code pg_get_indexdef} names the table schema-qualified or not by the
     * session's search path; the name is swapped for the script's own.
     *
     * @return the statements, unterminated, or {@code null} when the catalog
     *         could not be read and the metadata indexes have to do
     */
    static List<String> pgIndexes(Connection conn, String schema, String table,
                                  String qualified) {
        boolean scoped = schema != null && !schema.isEmpty();
        return Attempt.<List<String>>on(conn, "pg_index", c -> {
            List<String> out = new ArrayList<>();
            try (PreparedStatement ps = c.prepareStatement("SELECT"
                    + " pg_catalog.pg_get_indexdef(x.indexrelid),"
                    + " pg_catalog.quote_ident(i.relname), x.indrelid::pg_catalog.regclass::text"
                    + " FROM pg_catalog.pg_index x"
                    + " JOIN pg_catalog.pg_class i ON i.oid = x.indexrelid"
                    + " JOIN pg_catalog.pg_class c ON c.oid = x.indrelid"
                    + " JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace"
                    + " WHERE c.relname = ?"
                    + (scoped ? " AND n.nspname = ?" : " AND pg_catalog.pg_table_is_visible(c.oid)")
                    + " AND NOT EXISTS (SELECT 1 FROM pg_catalog.pg_constraint k"
                    + " WHERE k.conindid = x.indexrelid AND k.conrelid = x.indrelid"
                    + " AND k.contype IN ('p', 'u', 'x'))"
                    + " ORDER BY i.relname")) {
                bindRelation(ps, table, schema);
                try (ResultSet rs = ps.executeQuery()) {
                    while (rs.next()) {
                        String def = rs.getString(1);
                        String name = rs.getString(2);
                        String rel = rs.getString(3);
                        for (String head : new String[]{"CREATE INDEX ", "CREATE UNIQUE INDEX "}) {
                            for (String only : new String[]{"", "ONLY "}) {
                                String prefix = head + name + " ON " + only + rel + " ";
                                if (def.startsWith(prefix)) {
                                    def = head + name + " ON " + only + qualified + " "
                                            + def.substring(prefix.length());
                                }
                            }
                        }
                        out.add(def);
                    }
                }
            }
            return out;
        }, null);
    }

    /**
     * The columns of an H2 table that are {@code GENERATED ALWAYS AS
     * IDENTITY}, which take a value only with {@code OVERRIDING SYSTEM VALUE}.
     */
    private static Set<String> h2IdentityAlways(Connection conn, String schema, String table) {
        return Attempt.<Set<String>>on(conn, "INFORMATION_SCHEMA.COLUMNS identity", c -> {
            Set<String> out = new HashSet<>();
            try (PreparedStatement ps = c.prepareStatement("SELECT COLUMN_NAME"
                    + " FROM INFORMATION_SCHEMA.COLUMNS WHERE TABLE_NAME = ?"
                    + (schema == null || schema.isEmpty() ? " AND TABLE_SCHEMA = SCHEMA()"
                    : " AND TABLE_SCHEMA = ?")
                    + " AND IDENTITY_GENERATION = 'ALWAYS'")) {
                ps.setString(1, table);
                if (schema != null && !schema.isEmpty()) {
                    ps.setString(2, schema);
                }
                try (ResultSet rs = ps.executeQuery()) {
                    while (rs.next()) {
                        out.add(rs.getString(1));
                    }
                }
            }
            return out;
        }, new HashSet<>());
    }

    private static void bindRelation(PreparedStatement ps, String table, String schema)
            throws SQLException {
        ps.setString(1, table);
        if (schema != null && !schema.isEmpty()) {
            ps.setString(2, schema);
        }
    }

    private static void appendDefault(StringBuilder sb, Col c, Ident id) {
        if (!c.autoIncrement && c.def != null && !c.def.trim().isEmpty()) {
            if (c.defaultName != null) {
                sb.append(" CONSTRAINT ").append(id.q(c.defaultName));
            }
            // COLUMN_DEF is already SQL text, quoted the way the server wants
            // it; re-quoting it would turn DEFAULT 0 into DEFAULT '0'.
            sb.append(" DEFAULT ").append(c.def.trim());
        }
    }

    private static String identityClause(Dialect dialect) {
        switch (dialect) {
            case MYSQL:
            case MARIADB:  return "AUTO_INCREMENT";
            case SQLSERVER: return "IDENTITY";
            case SQLITE:   return "AUTOINCREMENT";
            default:       return "GENERATED BY DEFAULT AS IDENTITY";
        }
    }

    /**
     * Renders a column's type with its precision and scale.
     *
     * <p>Only the type families where the parameters are part of the type get
     * them. {@code getColumns} reports {@code COLUMN_SIZE} for every numeric
     * type - 32 for an {@code INTEGER}, meaning bits - and {@code INTEGER(32)}
     * is not a type.
     */
    private static String typeText(Col c, Dialect dialect) {
        if (c.nativeType != null) {
            return c.nativeType;
        }
        String name = c.typeName == null ? "UNKNOWN" : c.typeName.trim();
        if (name.indexOf('(') >= 0) {
            // Some drivers hand back a fully parameterized name already.
            return name;
        }
        String max = dialect == Dialect.SQLSERVER ? sqlServerMax(name, c.size) : null;
        if (max != null) {
            return max;
        }
        // MySQL appends attributes to TYPE_NAME ("DECIMAL UNSIGNED"), and the
        // parameters belong before them.
        String suffix = "";
        String upper = name.toUpperCase(Locale.ROOT);
        // SQL Server reports an identity column's type as "int identity"; the
        // IDENTITY clause is written once, where the column says it is one.
        if (upper.endsWith(" IDENTITY")) {
            name = name.substring(0, name.length() - " IDENTITY".length());
            upper = upper.substring(0, upper.length() - " IDENTITY".length());
        }
        for (String attr : new String[]{" UNSIGNED ZEROFILL", " UNSIGNED", " ZEROFILL"}) {
            if (upper.endsWith(attr)) {
                suffix = name.substring(name.length() - attr.length());
                name = name.substring(0, name.length() - attr.length());
                break;
            }
        }
        return name + args(c) + suffix;
    }

    /**
     * SQL Server's {@code (max)} types, whose size mssql-jdbc reports as the
     * largest it can - 2^31-1 or 2^30-1 - and which read back as a length of
     * one when written without it. {@code text}, {@code ntext} and
     * {@code image} take no length at all. {@code null} for anything else.
     */
    private static String sqlServerMax(String name, Integer size) {
        String lower = name.toLowerCase(Locale.ROOT);
        switch (lower) {
            case "varchar":
            case "varbinary":
                return size != null && size > 8000 ? name + "(max)" : null;
            case "nvarchar":
                return size != null && size > 4000 ? name + "(max)" : null;
            case "ntext":
            case "text":
            case "image":
                return name;
            default:
                return null;
        }
    }

    private static String args(Col c) {
        int type = c.dataType == null ? Types.OTHER : c.dataType;
        Integer size = c.size;
        Integer digits = c.digits;
        // A driver reporting Integer.MAX_VALUE means "unbounded", not a length.
        boolean sized = size != null && size > 0 && size != Integer.MAX_VALUE;
        switch (type) {
            case Types.CHAR:
            case Types.VARCHAR:
            case Types.LONGVARCHAR:
            case Types.NCHAR:
            case Types.NVARCHAR:
            case Types.LONGNVARCHAR:
            case Types.BINARY:
            case Types.VARBINARY:
            case Types.LONGVARBINARY:
                return sized ? "(" + size + ")" : "";
            case Types.DECIMAL:
            case Types.NUMERIC:
                if (!sized) {
                    return "";
                }
                return digits != null && digits > 0 ? "(" + size + ", " + digits + ")"
                        : "(" + size + ")";
            case Types.TIME:
            case Types.TIME_WITH_TIMEZONE:
            case Types.TIMESTAMP:
            case Types.TIMESTAMP_WITH_TIMEZONE:
                // For the datetime family DECIMAL_DIGITS is the fractional
                // seconds precision, which is part of the type.
                return digits != null && digits > 0 ? "(" + digits + ")" : "";
            default:
                return "";
        }
    }

    /**
     * @param catalogQualified whether the request named a catalog, in which case
     *                         the referenced table is named the same way. Without
     *                         it the reference stays at schema depth, so that a
     *                         statement qualified {@code APP.CHILD} does not
     *                         suddenly point at {@code MYDB.APP.PARENT}. A
     *                         product with no schemas (MySQL, where the catalog
     *                         <em>is</em> the namespace) keeps the catalog either
     *                         way, because dropping it would leave a bare name.
     */
    private static String foreignKey(Fk fk, Ident id, boolean catalogQualified,
                                     boolean deferrable) {
        StringBuilder sb = new StringBuilder();
        if (fk.name != null && !fk.name.isEmpty()) {
            sb.append("CONSTRAINT ").append(id.q(fk.name)).append(' ');
        }
        boolean keepCatalog = catalogQualified || fk.pkSchema == null || fk.pkSchema.isEmpty();
        sb.append("FOREIGN KEY (").append(columnList(fk.fkCols.values(), id)).append(')');
        sb.append(" REFERENCES ")
                .append(id.qualify(keepCatalog ? fk.pkCatalog : null, fk.pkSchema, fk.pkTable))
                .append(" (").append(columnList(fk.pkCols.values(), id)).append(')');
        String del = rule(fk.deleteRule);
        if (del != null) {
            sb.append(" ON DELETE ").append(del);
        }
        String upd = rule(fk.updateRule);
        if (upd != null) {
            sb.append(" ON UPDATE ").append(upd);
        }
        if (deferrable && fk.deferrability != null) {
            if (fk.deferrability == DatabaseMetaData.importedKeyInitiallyDeferred) {
                sb.append(" DEFERRABLE INITIALLY DEFERRED");
            } else if (fk.deferrability == DatabaseMetaData.importedKeyInitiallyImmediate) {
                sb.append(" DEFERRABLE");
            }
        }
        return sb.toString();
    }

    /**
     * @return the referential action text, or {@code null} for the default
     *         {@code NO ACTION}, which is left implicit
     */
    private static String rule(Integer code) {
        if (code == null) {
            return null;
        }
        switch (code) {
            case DatabaseMetaData.importedKeyCascade:    return "CASCADE";
            case DatabaseMetaData.importedKeyRestrict:   return "RESTRICT";
            case DatabaseMetaData.importedKeySetNull:    return "SET NULL";
            case DatabaseMetaData.importedKeySetDefault: return "SET DEFAULT";
            default:                                     return null;
        }
    }

    private static String index(Idx ix, String qualified, Ident id) {
        StringBuilder sb = new StringBuilder("CREATE ");
        if (ix.unique) {
            sb.append("UNIQUE ");
        }
        sb.append("INDEX ").append(id.q(ix.name)).append(" ON ").append(qualified).append(" (");
        boolean first = true;
        for (Map.Entry<Integer, String> e : ix.cols.entrySet()) {
            if (!first) {
                sb.append(", ");
            }
            first = false;
            sb.append(id.q(e.getValue()));
            if ("D".equalsIgnoreCase(ix.order.get(e.getKey()))) {
                sb.append(" DESC");
            }
        }
        return sb.append(')').toString();
    }

    private static void appendComments(StringBuilder sb, Dialect dialect, Ident id,
                                       String qualified, String tableRemarks, List<Col> cols) {
        // MySQL carried its comments inline; SQL Server has no COMMENT ON at all
        // (extended properties are a stored-procedure call, not DDL), and
        // emitting standard syntax there would produce text that cannot run.
        if (dialect.isMySqlFamily() || dialect == Dialect.SQLSERVER) {
            return;
        }
        StringBuilder out = new StringBuilder();
        if (tableRemarks != null && !tableRemarks.isEmpty()) {
            out.append("COMMENT ON TABLE ").append(qualified)
                    .append(" IS ").append(Ident.literal(tableRemarks)).append(";\n");
        }
        for (Col c : cols) {
            if (c.remarks != null && !c.remarks.isEmpty()) {
                out.append("COMMENT ON COLUMN ").append(qualified).append('.').append(id.q(c.name))
                        .append(" IS ").append(Ident.literal(c.remarks)).append(";\n");
            }
        }
        if (out.length() > 0) {
            sb.append('\n').append(out);
        }
    }

    private static String columnList(Iterable<String> names, Ident id) {
        StringBuilder sb = new StringBuilder();
        for (String n : names) {
            if (sb.length() > 0) {
                sb.append(", ");
            }
            sb.append(id.q(n));
        }
        return sb.toString();
    }

    // ------------------------------------------------------------ collection

    private static List<Col> readColumns(DatabaseMetaData dbm, String catalog, String schema,
                                         String table) throws SQLException {
        List<Col> out = new ArrayList<>();
        try (ResultSet rs = dbm.getColumns(catalog, schema, table, null)) {
            RsView v = new RsView(rs);
            while (rs.next()) {
                if (!table.equals(v.str("TABLE_NAME"))) {
                    // TABLE_NAME is a pattern argument: a name holding _ or %
                    // matches its neighbors too.
                    continue;
                }
                // getColumns column order, which the reads have to follow - see
                // RsView. NULLABLE (11) and IS_NULLABLE (18) sit either side of
                // the remarks and the default, so both are read where they fall
                // and the flag they agree on is worked out afterwards.
                Col c = new Col();
                c.name = v.str("COLUMN_NAME");                  // 4
                c.dataType = v.i32("DATA_TYPE");                // 5
                c.typeName = v.str("TYPE_NAME");                // 6
                c.size = v.i32("COLUMN_SIZE");                  // 7
                c.digits = v.i32("DECIMAL_DIGITS");             // 9
                Integer nullable = v.i32("NULLABLE");           // 11
                c.remarks = v.str("REMARKS");                   // 12
                c.def = v.str("COLUMN_DEF");                    // 13
                Boolean isNullable = v.yesNo("IS_NULLABLE");    // 18
                c.autoIncrement = Boolean.TRUE.equals(v.yesNo("IS_AUTOINCREMENT"));  // 23
                c.generated = Boolean.TRUE.equals(v.yesNo("IS_GENERATEDCOLUMN"));   // 24
                c.notNull = isNullable != null ? !isNullable
                        : nullable != null && nullable == DatabaseMetaData.columnNoNulls;
                out.add(c);
            }
        }
        if (Dialect.of(dbm) == Dialect.MYSQL) {
            confirmMysqlGenerated(dbm.getConnection(), catalog, table, out);
        }
        return out;
    }

    /**
     * Connector/J reports {@code IS_GENERATEDCOLUMN} as {@code EXTRA LIKE
     * '%GENERATED%'}, which MySQL 8's {@code DEFAULT_GENERATED} - the mark of
     * any expression default, {@code DEFAULT CURRENT_TIMESTAMP} among them -
     * matches too. Such a column holds values of its own, and taking it for a
     * computed one would leave it out of every row written. The generation
     * expression is what makes a column computed; a refused query leaves the
     * driver's answer.
     */
    private static void confirmMysqlGenerated(Connection conn, String catalog, String table,
                                              List<Col> cols) {
        boolean any = false;
        for (Col c : cols) {
            any |= c.generated;
        }
        if (!any || catalog == null || catalog.isEmpty()) {
            return;
        }
        Set<String> computed = Attempt.<Set<String>>on(conn, "information_schema.COLUMNS", c -> {
            Set<String> found = new HashSet<>();
            try (PreparedStatement ps = c.prepareStatement("SELECT COLUMN_NAME"
                    + " FROM information_schema.COLUMNS WHERE TABLE_SCHEMA = ?"
                    + " AND TABLE_NAME = ? AND GENERATION_EXPRESSION <> ''")) {
                ps.setString(1, catalog);
                ps.setString(2, table);
                try (ResultSet rs = ps.executeQuery()) {
                    while (rs.next()) {
                        found.add(rs.getString(1));
                    }
                }
            }
            return found;
        }, null);
        if (computed != null) {
            for (Col c : cols) {
                c.generated = c.generated && computed.contains(c.name);
            }
        }
    }

    /**
     * Reads a table's primary key.
     *
     * <p>Public because a transfer's {@code upsert} mode needs exactly this to
     * find its conflict key, and a second reader of {@code getPrimaryKeys} would
     * only differ from this one in its handling of the drivers that answer badly.
     *
     * @param dbm     the connection metadata
     * @param catalog catalog, may be {@code null}
     * @param schema  schema, may be {@code null}
     * @param table   the table name
     * @param into    filled with the key's columns, in {@code KEY_SEQ} order
     * @return the primary key's constraint name, or {@code null} when the driver
     *         does not name it or the table has no primary key
     * @throws SQLException if the driver fails
     */
    public static String readPrimaryKey(DatabaseMetaData dbm, String catalog, String schema,
                                        String table, List<String> into) throws SQLException {
        TreeMap<Integer, String> ordered = new TreeMap<>();
        String name = null;
        try (ResultSet rs = dbm.getPrimaryKeys(catalog, schema, table)) {
            RsView v = new RsView(rs);
            int fallbackSeq = 0;
            while (rs.next()) {
                // getPrimaryKeys column order - see RsView.
                String column = v.str("COLUMN_NAME");  // 4
                Integer seq = v.i32("KEY_SEQ");        // 5
                String pkName = v.str("PK_NAME");      // 6
                ordered.put(seq != null ? seq : ++fallbackSeq, column);
                if (name == null) {
                    name = pkName;
                }
            }
        } catch (SQLException e) {
            // Not every driver has primary keys for every table type; a view has
            // none and some drivers say so by throwing.
            LOG.log(Level.FINE, "no primary key metadata", e);
        }
        into.addAll(ordered.values());
        return name;
    }

    private static List<Fk> readForeignKeys(DatabaseMetaData dbm, String catalog, String schema,
                                            String table) throws SQLException {
        Map<String, Fk> byName = new LinkedHashMap<>();
        try (ResultSet rs = dbm.getImportedKeys(catalog, schema, table)) {
            RsView v = new RsView(rs);
            int anonymous = 0;
            String anonymousKey = null;
            while (rs.next()) {
                // getImportedKeys column order - see RsView. The whole row goes
                // into locals first because the grouping below reads the key
                // sequence and the constraint name before the columns they
                // order, and the result set will not be read backwards.
                String pkCatalog = v.str("PKTABLE_CAT");     // 1
                String pkSchema = v.str("PKTABLE_SCHEM");    // 2
                String pkTable = v.str("PKTABLE_NAME");      // 3
                String pkColumn = v.str("PKCOLUMN_NAME");    // 4
                String fkColumn = v.str("FKCOLUMN_NAME");    // 8
                Integer seq = v.i32("KEY_SEQ");              // 9
                Integer updateRule = v.i32("UPDATE_RULE");   // 10
                Integer deleteRule = v.i32("DELETE_RULE");   // 11
                String fkName = v.str("FK_NAME");            // 12
                Integer deferrability = v.i32("DEFERRABILITY"); // 14
                String key;
                if (fkName != null && !fkName.isEmpty()) {
                    key = fkName;
                } else {
                    // Unnamed keys still arrive as contiguous runs ordered by
                    // KEY_SEQ, so a new run starts at sequence 1.
                    if (seq == null || seq <= 1 || anonymousKey == null) {
                        anonymousKey = " fk" + (++anonymous);
                    }
                    key = anonymousKey;
                }
                Fk fk = byName.computeIfAbsent(key, k -> new Fk());
                if (fk.pkTable == null) {
                    fk.name = fkName;
                    fk.pkCatalog = pkCatalog;
                    fk.pkSchema = pkSchema;
                    fk.pkTable = pkTable;
                    fk.updateRule = updateRule;
                    fk.deleteRule = deleteRule;
                    fk.deferrability = deferrability;
                }
                int at = seq != null ? seq : fk.fkCols.size() + 1;
                fk.fkCols.put(at, fkColumn);
                fk.pkCols.put(at, pkColumn);
            }
        } catch (SQLException e) {
            LOG.log(Level.FINE, "no foreign key metadata", e);
        }
        List<Fk> out = new ArrayList<>();
        for (Fk fk : byName.values()) {
            if (fk.pkTable != null && !fk.fkCols.isEmpty()) {
                out.add(fk);
            }
        }
        return out;
    }

    private static List<Idx> readIndexes(DatabaseMetaData dbm, Dialect dialect, String catalog,
                                         String schema, String table, List<String> pkCols,
                                         List<Fk> fks)
            throws SQLException {
        Map<String, Idx> byName = new LinkedHashMap<>();
        try (ResultSet rs = dbm.getIndexInfo(catalog, schema, table, false, true)) {
            RsView v = new RsView(rs);
            while (rs.next()) {
                // getIndexInfo column order - see RsView. The row is taken whole
                // before any of it is judged: the statistics row and the unnamed
                // index are both rejected on columns that sit right of ones read
                // after them, and a rejected row costs nothing but the reads.
                Boolean nonUnique = v.bool("NON_UNIQUE");        // 4
                String name = v.str("INDEX_NAME");               // 6
                Integer type = v.i32("TYPE");                    // 7
                Integer ordinal = v.i32("ORDINAL_POSITION");     // 8
                String col = v.str("COLUMN_NAME");               // 9
                String dir = v.str("ASC_OR_DESC");               // 10
                if (type != null && type == DatabaseMetaData.tableIndexStatistic) {
                    // A statistics row is a row count, not an index.
                    continue;
                }
                if (name == null || name.isEmpty()) {
                    continue;
                }
                Idx ix = byName.computeIfAbsent(name, k -> new Idx());
                ix.name = name;
                ix.unique = nonUnique != null && !nonUnique;
                if (col == null) {
                    // A function-based or expression index; JDBC gives no
                    // expression text, so the index cannot be reproduced.
                    ix.usable = false;
                    continue;
                }
                int at = ordinal != null ? ordinal : ix.cols.size() + 1;
                ix.cols.put(at, col);
                if (dir != null) {
                    ix.order.put(at, dir);
                }
            }
        } catch (SQLException e) {
            LOG.log(Level.FINE, "no index metadata", e);
        }

        List<Idx> out = new ArrayList<>();
        for (Idx ix : byName.values()) {
            if (!ix.usable || ix.cols.isEmpty()) {
                continue;
            }
            List<String> cols = new ArrayList<>(ix.cols.values());
            if (cols.equals(pkCols)) {
                // The primary key's own index; PRIMARY KEY already declares it.
                continue;
            }
            // MySQL, MariaDB and H2 make an index for every foreign key that
            // has none and list it like any other: written out, it would be
            // made twice. Elsewhere such an index is one someone created.
            boolean automatic = dialect.isMySqlFamily() || dialect == Dialect.H2
                    || dialect == Dialect.OTHER;
            boolean backsForeignKey = false;
            for (Fk fk : automatic ? fks : List.<Fk>of()) {
                if (cols.equals(new ArrayList<>(fk.fkCols.values()))) {
                    backsForeignKey = true;
                    break;
                }
            }
            if (!backsForeignKey) {
                out.add(ix);
            }
        }
        return out;
    }

    private static String readTableRemarks(DatabaseMetaData dbm, String catalog, String schema,
                                           String table) {
        try (ResultSet rs = dbm.getTables(catalog, schema, table, null)) {
            RsView v = new RsView(rs);
            while (rs.next()) {
                if (table.equals(v.str("TABLE_NAME"))) {
                    return v.str("REMARKS");
                }
            }
        } catch (SQLException | RuntimeException e) {
            LOG.log(Level.FINE, "no table remarks", e);
        }
        return null;
    }
}
