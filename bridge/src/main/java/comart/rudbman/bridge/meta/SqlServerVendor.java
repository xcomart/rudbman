package comart.rudbman.bridge.meta;

import com.google.gson.JsonObject;

import java.util.logging.Logger;
import java.util.logging.Level;
import java.sql.PreparedStatement;
import java.sql.DatabaseMetaData;
import java.math.BigInteger;
import java.sql.ResultSet;
import java.sql.Connection;
import java.sql.SQLException;
import java.util.ArrayList;
import java.util.Collections;
import java.util.List;
import java.util.Map;

/** Microsoft SQL Server. */
final class SqlServerVendor extends Vendor {

    @Override
    boolean names(String productName) {
        return productName.contains("microsoft sql server") || productName.contains("sql server");
    }

    @Override
    boolean numbersProcedures() {
        return true;
    }

    @Override
    public String routineText(Connection conn, Ident id, String catalog, String schema,
                              String name, boolean function) {
        return DdlText.sqlServerDefinition(conn,
                id.qualify(catalog, schema, RoutineDdl.unnumbered(name)));
    }

    @Override
    List<String> sequenceQueries(Ident id, String catalog) {
        return List.of(Sequences.sqlServer(id, catalog));
    }

    /**
     * {@code sys.triggers} is per database; a catalog other than the session's
     * is read through its own three-part names.
     */
    @Override
    void triggerQuery(StringBuilder sql, List<String> args, Ident id, String catalog,
                      String schema, String name) {
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
        Triggers.filter(sql, args, "s.name", schema);
        Triggers.filter(sql, args, "tr.name", name);
        sql.append(" ORDER BY 2, 3");
    }

    @Override
    boolean hidesComments() {
        return true;
    }

    @Override
    public List<String[]> tableComments(Connection conn, Map<String, String> req)
            throws SQLException {
        if (Comments.elsewhere(conn, req)) {
            return List.of();
        }
        Comments.Filter schema = Comments.Filter.of(req, "schema", "schema_pattern");
        Comments.Filter table = Comments.Filter.of(req, "table", "table_pattern");
        StringBuilder sql = new StringBuilder(Comments.SQLSERVER_TABLES);
        List<String> args = new ArrayList<>();
        schema.append(sql, args, "s.name", Comments.ANSI_ESCAPE);
        table.append(sql, args, "o.name", Comments.ANSI_ESCAPE);
        return Comments.rows(conn, sql.toString(), args, 3);
    }

    @Override
    public List<String[]> columnComments(Connection conn, Map<String, String> req)
            throws SQLException {
        if (Comments.elsewhere(conn, req)) {
            return List.of();
        }
        Comments.Filter schema = Comments.Filter.of(req, "schema", "schema_pattern");
        Comments.Filter table = Comments.Filter.of(req, "table", "table_pattern");
        Comments.Filter column = Comments.Filter.of(req, "column", "column_pattern");
        StringBuilder sql = new StringBuilder(Comments.SQLSERVER_COLUMNS);
        List<String> args = new ArrayList<>();
        schema.append(sql, args, "s.name", Comments.ANSI_ESCAPE);
        table.append(sql, args, "o.name", Comments.ANSI_ESCAPE);
        column.append(sql, args, "c.name", Comments.ANSI_ESCAPE);
        return Comments.rows(conn, sql.toString(), args, 4);
    }

    @Override
    boolean upserts() {
        return true;
    }

    /** {@code MERGE}, which SQL Server requires terminated. */
    @Override
    String upsert(Ident id, String qualified, List<String> columns, List<String> keys,
                  List<String> updates) {
        return Upsert.merge(id, qualified, columns, keys, updates, false, true);
    }

    /**
     * {@code sqlcmd} starts with {@code QUOTED_IDENTIFIER} off, which refuses
     * a persisted computed column and is not what the objects were created
     * under; SSMS writes the same two settings at the head of its scripts.
     */
    @Override
    List<String> scriptPrologue() {
        return List.of("SET ANSI_NULLS ON;", "SET QUOTED_IDENTIFIER ON;");
    }

    @Override
    boolean reconstructsGenerated() {
        return true;
    }

    @Override
    List<String> catalogExtras(Connection conn, Ident id, String catalog, String schema,
                               String table, List<Ddl.Col> cols) {
        return Ddl.sqlServerExtras(conn, id, catalog, schema, table, cols);
    }

    @Override
    String primaryKeyKind(Connection conn, Ident id, String catalog, String object) {
        return SqlServerSchema.primaryKeyKind(conn, id, catalog, object);
    }

    /**
     * No {@code COMMENT ON} at all: extended properties are a
     * stored-procedure call, written by {@link #extendedProperties}.
     */
    @Override
    boolean commentsOn() {
        return false;
    }

    @Override
    List<String> nativeIndexes(Connection conn, Ident id, String catalog, String schema,
                               String table, String qualified) {
        return SqlServerSchema.indexes(conn, id, catalog, id.qualify(catalog, schema, table),
                qualified);
    }

    @Override
    List<String> extendedProperties(Connection conn, Ident id, String catalog, String schema,
                                    String table) {
        return SqlServerSchema.extendedProperties(conn, id, catalog,
                id.qualify(catalog, schema, table), schema, table);
    }

    /** A computed column has no type of its own to write. */
    @Override
    boolean computedHasNoType() {
        return true;
    }

    @Override
    String identityClause() {
        return "IDENTITY";
    }

    @Override
    String maxType(String name, Integer size) {
        return Ddl.sqlServerMax(name, size);
    }

    @Override
    boolean indexesForeignKeys() {
        return false;
    }

    /**
     * The definition, then an indexed view's indexes, each a batch of its
     * own after the view's, which has to be alone in its batch.
     */
    @Override
    String viewText(Connection conn, ObjectDdl.Target t) {
        String text = DdlText.sqlServerDefinition(conn, t.qualified());
        List<String> indexes = text == null ? null : SqlServerSchema.indexes(conn, t.id,
                t.catalog, t.qualified(), t.qualified());
        if (indexes == null || indexes.isEmpty()) {
            return text;
        }
        StringBuilder sb = new StringBuilder(text.strip());
        for (String ix : indexes) {
            sb.append("\nGO\n").append(ix).append(';');
        }
        return sb.toString();
    }

    @Override
    String triggerText(Connection conn, ObjectDdl.Target t) {
        return DdlText.sqlServerDefinition(conn, t.qualified());
    }

    /** The database prefix is refused (Msg 166), where a lookup accepts it. */
    @Override
    public boolean createsWithoutCatalog() {
        return true;
    }

    @Override
    public boolean booleansAreNumbers() {
        return true;
    }

    /**
     * A plain literal is in the database's code page: text outside it -
     * Hangul in a Latin1 collation - would arrive as question marks.
     * {@code N'…'} is Unicode.
     */
    @Override
    public String stringLiteral(String s, boolean national, int type, ResultSet rs, int i) {
        return "N" + Ident.literal(s);
    }

    @Override
    public String hexLiteral(String hex) {
        return "0x" + hex;
    }

    @Override
    public ScriptClient scriptClient() {
        return ScriptClient.SQLCMD;
    }

    @Override
    public boolean dropsIfExists() {
        return true;
    }

    @Override
    public String dropForeignKey(String qualified, String key) {
        return "IF OBJECT_ID(" + Ident.literal(qualified) + ", 'U') IS NOT NULL"
                + " ALTER TABLE " + qualified + " DROP CONSTRAINT IF EXISTS " + key + ";";
    }

    @Override
    public List<String[]> backupPreamble(Connection conn, Ident id, String catalog,
                                         String schema) {
        return SqlServerSchema.aliasTypes(conn, id, catalog, schema);
    }

    /**
     * {@code current_value} is the start of a sequence never used, too;
     * {@code last_used_value} (2017 and newer) tells the two apart.
     */
    @Override
    public String sequenceValue(Connection conn, Ident id, String catalog, String schema,
                                String name, String listed) {
        return lastUsed(conn, id.qualify(catalog, schema, name));
    }

    /**
     * Each sequence drawn up to where the source has it, then every table's
     * identity reseeded.
     */
    @Override
    public List<String> counters(DatabaseMetaData dbm, Ident id, List<Sequence> sequences,
                                 List<Table> tables, boolean data) throws SQLException {
        List<String> out = new ArrayList<>();
        int ranges = 0;
        for (Sequence s : sequences) {
            String value = s.value();
            if (!integer(value) || s.increment() == null || !s.increment().matches("-?\\d+")) {
                continue;
            }
            BigInteger last = new BigInteger(value);
            BigInteger step = new BigInteger(s.increment());
            BigInteger taken = s.start() != null && s.start().matches("-?\\d+")
                    && step.signum() != 0 ? last.subtract(new BigInteger(s.start())) : null;
            if (taken != null && taken.signum() * step.signum() >= 0
                    && taken.mod(step.abs()).signum() == 0) {
                // Draw the values the source has handed out: RESTART WITH
                // would move the sequence's start along with it.
                String variable = "@range_first" + (++ranges);
                out.add("DECLARE " + variable + " sql_variant;"
                        + " EXEC sys.sp_sequence_get_range @sequence_name = N"
                        + Ident.literal(id.qualify(s.catalog(), s.schema(), s.name()))
                        + ", @range_size = " + taken.divide(step).add(BigInteger.ONE)
                        + ", @range_first_value = " + variable + " OUTPUT;");
            } else {
                out.add("ALTER SEQUENCE " + id.qualify(null, s.schema(), s.name())
                        + " RESTART WITH " + last.add(step) + ";");
            }
        }
        if (data) {
            Connection conn = dbm.getConnection();
            for (Table t : tables) {
                String reseed = SqlServerSchema.identityReseed(conn, id, t.catalog(),
                        id.qualify(t.catalog(), t.schema(), t.name()));
                if (reseed != null) {
                    out.add(reseed);
                }
            }
        }
        return out;
    }

    @Override
    public RowFence rowFence(Connection conn, Ident id, String schema, String table,
                             String qualified, List<String> identities) {
        if (identities.isEmpty()) {
            return RowFence.NONE;
        }
        return new RowFence() {
            @Override
            public List<String> before() {
                return List.of("SET IDENTITY_INSERT " + qualified + " ON;");
            }

            @Override
            public List<String> after() {
                return List.of("SET IDENTITY_INSERT " + qualified + " OFF;");
            }
        };
    }

    private static String lastUsed(Connection conn, String qualified) {
        try (PreparedStatement ps = conn.prepareStatement("SELECT CAST(last_used_value AS"
                + " VARCHAR(40)) FROM sys.sequences WHERE object_id = OBJECT_ID(?)")) {
            ps.setString(1, qualified);
            try (ResultSet rs = ps.executeQuery()) {
                return rs.next() ? rs.getString(1) : null;
            }
        } catch (SQLException e) {
            LOG.log(Level.FINE, "no last_used_value for " + qualified, e);
            return null;
        }
    }

    private static final Logger LOG = Logger.getLogger(SqlServerVendor.class.getName());
}
