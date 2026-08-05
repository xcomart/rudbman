package comart.rudbman.bridge.meta;

import com.google.gson.JsonObject;
import comart.rudbman.bridge.BridgeException;
import comart.rudbman.bridge.Json;

import java.sql.Connection;
import java.sql.DatabaseMetaData;
import java.sql.ResultSet;
import java.sql.SQLException;
import java.sql.Statement;
import java.sql.Types;
import java.util.ArrayList;
import java.util.LinkedHashMap;
import java.util.List;
import java.util.Locale;
import java.util.Map;
import java.util.TreeMap;
import java.util.logging.Level;
import java.util.logging.Logger;

/**
 * {@code DESCRIBE kind: "ddl"}: the {@code CREATE} text of one table.
 *
 * <p>Answered in two layers, because no single layer is both faithful and
 * universal.
 *
 * <ol>
 *   <li><b>Native.</b> Some servers will quote their own DDL back
 *       ({@code SHOW CREATE TABLE} on MySQL, {@code SCRIPT TABLE} on H2). When
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
 *   <li>view, materialised view and typed-table definitions - a view reaches
 *       this path as a bare column list.</li>
 * </ul>
 *
 * <p>Indexes that merely back a declared key are dropped: an index whose column
 * list is exactly the primary key's, or exactly one foreign key's, is something
 * the server created for that key and will create again. Emitting it would make
 * the DDL fail to replay on the very servers it came from.
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
     * Builds the DDL for one table.
     *
     * <p>Request members: {@code catalog}, {@code schema}, {@code table}
     * (required) and {@code source}, one of {@code auto} (default, native then
     * reverse generation), {@code native} or {@code metadata}.
     *
     * @param conn    the live connection, for the native catalogue queries
     * @param dbm     the connection metadata
     * @param req     the request body
     * @return an object carrying {@code ddl} and {@code source}
     * @throws SQLException if the driver fails on the portable path
     */
    public static JsonObject of(Connection conn, DatabaseMetaData dbm, JsonObject req)
            throws SQLException {
        String table = Json.str(req, "table");
        if (table == null || table.isEmpty()) {
            throw new BridgeException("protocol", "describe kind 'ddl' requires an exact 'table'");
        }
        String want = Json.str(req, "source");
        if (want == null || want.isEmpty()) {
            want = "auto";
        }
        if (!"auto".equals(want) && !"native".equals(want) && !"metadata".equals(want)) {
            throw new BridgeException("protocol",
                    "describe kind 'ddl' accepts source 'auto', 'native' or 'metadata', not '"
                            + want + "'");
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

        Script(String source, String creates, List<String> alters) {
            this.source = source;
            this.creates = creates;
            this.alters = alters;
        }
    }

    /**
     * Builds one object's DDL for a script file rather than for display.
     *
     * <p>The difference from {@link #of} is the foreign keys. A script has to
     * replay into an empty database, and a schema whose tables reference each
     * other in a cycle - which is common enough that the design calls it out -
     * cannot be replayed in any creation order at all. So the keys are lifted out
     * of the {@code CREATE} and handed back separately, for the caller to write
     * after every {@code CREATE} in the whole script.
     *
     * <p><strong>Splitting forces the reconstruction path.</strong> The native
     * layer hands back the server's own text, and finding the foreign keys inside
     * that text means parsing vendor SQL - MySQL inlines them in the
     * {@code CREATE}, and a regular expression over a statement that may contain
     * the words {@code FOREIGN KEY} inside a comment or a default is not a
     * trade this bridge makes. The metadata layer knows structurally where the
     * keys are, so {@code splitForeignKeys} selects it. The cost is the
     * reconstruction's documented blind spots (check constraints, storage
     * clauses); the caller offers the user the choice.
     *
     * @param conn             the live connection
     * @param dbm              the connection metadata
     * @param catalog          catalog, may be {@code null}
     * @param schema           schema, may be {@code null}
     * @param table            the object name, required
     * @param splitForeignKeys whether foreign keys move to {@link Script#alters}
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
                return new Script("native", text, new ArrayList<>());
            }
            return new Script("metadata",
                    fromMetadata(dbm, dialect, id, catalog, schema, table, null),
                    new ArrayList<>());
        }
        List<String> alters = new ArrayList<>();
        String creates = fromMetadata(dbm, dialect, id, catalog, schema, table, alters);
        return new Script("metadata", creates, alters);
    }

    private static JsonObject result(String ddl, String source) {
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
        return null;
    }

    private static String mysqlShowCreate(Statement st, String qualified) throws SQLException {
        try (ResultSet rs = st.executeQuery(MYSQL_SHOW_CREATE + qualified)) {
            // Column 1 is the table name, column 2 the DDL. Read by position:
            // the label is "Create Table" for tables and "Create View" for
            // views, and MariaDB adds two more columns for views.
            if (rs.next() && rs.getMetaData().getColumnCount() >= 2) {
                String ddl = rs.getString(2);
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
        String ref = schema == null || schema.isEmpty()
                ? '"' + table + '"'
                : '"' + schema + "\".\"" + table + '"';
        StringBuilder sb = new StringBuilder();
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
                if (sb.length() > 0) {
                    sb.append('\n');
                }
                sb.append(stmt);
            }
        }
        // Without a CREATE statement the filter matched nothing useful - a
        // renamed schema, a quoting convention this code did not predict - and
        // half a script is worse than a reconstruction.
        return sawTable ? sb.toString() : null;
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
        List<Idx> indexes = readIndexes(dbm, catalog, schema, table, pkCols, fks);
        String tableRemarks = readTableRemarks(dbm, catalog, schema, table);

        String qualified = id.qualify(catalog, schema, table);
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
            pk.append("PRIMARY KEY (").append(columnList(pkCols, id)).append(')');
            body.add(pk.toString());
        }
        boolean catalogQualified = catalog != null && !catalog.isEmpty();
        for (Fk fk : fks) {
            if (alterOut == null) {
                body.add("    " + foreignKey(fk, id, catalogQualified));
            } else {
                alterOut.add("ALTER TABLE " + qualified + " ADD "
                        + foreignKey(fk, id, catalogQualified));
            }
        }
        sb.append(String.join(",\n", body)).append("\n)");

        // MySQL has no COMMENT ON; a table comment is part of the CREATE.
        if (dialect.isMySqlFamily() && tableRemarks != null && !tableRemarks.isEmpty()) {
            sb.append(" COMMENT=").append(Ident.literal(tableRemarks));
        }
        sb.append(";\n");

        for (Idx ix : indexes) {
            sb.append('\n').append(index(ix, qualified, id)).append(";\n");
        }

        appendComments(sb, dialect, id, qualified, tableRemarks, cols);
        return sb.toString();
    }

    private static String column(Col c, Dialect dialect, Ident id) {
        StringBuilder sb = new StringBuilder();
        sb.append(id.q(c.name)).append(' ').append(typeText(c));
        String identity = identityClause(dialect);
        if (dialect.isMySqlFamily()) {
            // MySQL's column grammar fixes this order:
            // type [NOT NULL] [DEFAULT] [AUTO_INCREMENT] [COMMENT].
            if (c.notNull) {
                sb.append(" NOT NULL");
            }
            appendDefault(sb, c);
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
                appendDefault(sb, c);
            }
            if (c.notNull) {
                sb.append(" NOT NULL");
            }
        }
        return sb.toString();
    }

    private static void appendDefault(StringBuilder sb, Col c) {
        if (!c.autoIncrement && c.def != null && !c.def.trim().isEmpty()) {
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
    private static String typeText(Col c) {
        String name = c.typeName == null ? "UNKNOWN" : c.typeName.trim();
        if (name.indexOf('(') >= 0) {
            // Some drivers hand back a fully parameterised name already.
            return name;
        }
        // MySQL appends attributes to TYPE_NAME ("DECIMAL UNSIGNED"), and the
        // parameters belong before them.
        String suffix = "";
        String upper = name.toUpperCase(Locale.ROOT);
        for (String attr : new String[]{" UNSIGNED ZEROFILL", " UNSIGNED", " ZEROFILL"}) {
            if (upper.endsWith(attr)) {
                suffix = name.substring(name.length() - attr.length());
                name = name.substring(0, name.length() - attr.length());
                break;
            }
        }
        return name + args(c) + suffix;
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
    private static String foreignKey(Fk fk, Ident id, boolean catalogQualified) {
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
                    // matches its neighbours too.
                    continue;
                }
                Col c = new Col();
                c.name = v.str("COLUMN_NAME");
                c.typeName = v.str("TYPE_NAME");
                c.dataType = v.i32("DATA_TYPE");
                c.size = v.i32("COLUMN_SIZE");
                c.digits = v.i32("DECIMAL_DIGITS");
                Integer nullable = v.i32("NULLABLE");
                Boolean isNullable = v.yesNo("IS_NULLABLE");
                c.notNull = isNullable != null ? !isNullable
                        : nullable != null && nullable == DatabaseMetaData.columnNoNulls;
                c.def = v.str("COLUMN_DEF");
                c.autoIncrement = Boolean.TRUE.equals(v.yesNo("IS_AUTOINCREMENT"));
                c.remarks = v.str("REMARKS");
                out.add(c);
            }
        }
        return out;
    }

    /** @return the primary key's constraint name; fills {@code into} with its columns in order */
    private static String readPrimaryKey(DatabaseMetaData dbm, String catalog, String schema,
                                         String table, List<String> into) throws SQLException {
        TreeMap<Integer, String> ordered = new TreeMap<>();
        String name = null;
        try (ResultSet rs = dbm.getPrimaryKeys(catalog, schema, table)) {
            RsView v = new RsView(rs);
            int fallbackSeq = 0;
            while (rs.next()) {
                Integer seq = v.i32("KEY_SEQ");
                ordered.put(seq != null ? seq : ++fallbackSeq, v.str("COLUMN_NAME"));
                if (name == null) {
                    name = v.str("PK_NAME");
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
                Integer seq = v.i32("KEY_SEQ");
                String fkName = v.str("FK_NAME");
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
                    fk.pkCatalog = v.str("PKTABLE_CAT");
                    fk.pkSchema = v.str("PKTABLE_SCHEM");
                    fk.pkTable = v.str("PKTABLE_NAME");
                    fk.updateRule = v.i32("UPDATE_RULE");
                    fk.deleteRule = v.i32("DELETE_RULE");
                }
                int at = seq != null ? seq : fk.fkCols.size() + 1;
                fk.fkCols.put(at, v.str("FKCOLUMN_NAME"));
                fk.pkCols.put(at, v.str("PKCOLUMN_NAME"));
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

    private static List<Idx> readIndexes(DatabaseMetaData dbm, String catalog, String schema,
                                         String table, List<String> pkCols, List<Fk> fks)
            throws SQLException {
        Map<String, Idx> byName = new LinkedHashMap<>();
        try (ResultSet rs = dbm.getIndexInfo(catalog, schema, table, false, true)) {
            RsView v = new RsView(rs);
            while (rs.next()) {
                Integer type = v.i32("TYPE");
                if (type != null && type == DatabaseMetaData.tableIndexStatistic) {
                    // A statistics row is a row count, not an index.
                    continue;
                }
                String name = v.str("INDEX_NAME");
                if (name == null || name.isEmpty()) {
                    continue;
                }
                Idx ix = byName.computeIfAbsent(name, k -> new Idx());
                ix.name = name;
                Boolean nonUnique = v.bool("NON_UNIQUE");
                ix.unique = nonUnique != null && !nonUnique;
                Integer ordinal = v.i32("ORDINAL_POSITION");
                String col = v.str("COLUMN_NAME");
                if (col == null) {
                    // A function-based or expression index; JDBC gives no
                    // expression text, so the index cannot be reproduced.
                    ix.usable = false;
                    continue;
                }
                int at = ordinal != null ? ordinal : ix.cols.size() + 1;
                ix.cols.put(at, col);
                String dir = v.str("ASC_OR_DESC");
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
            boolean backsForeignKey = false;
            for (Fk fk : fks) {
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
