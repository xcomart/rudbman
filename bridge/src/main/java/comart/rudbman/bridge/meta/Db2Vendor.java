package comart.rudbman.bridge.meta;

import java.sql.ResultSet;
import java.sql.Connection;
import java.util.List;

/** IBM Db2. */
final class Db2Vendor extends Vendor {

    @Override
    boolean names(String productName) {
        return productName.contains("db2");
    }

    /**
     * JCC answers {@code getCatalogName} with the database's name, while its
     * {@code getCatalogs} lists no catalog at all and a metadata call handed
     * that name answers nothing.
     */
    @Override
    public boolean resultCatalogsResolve() {
        return false;
    }

    @Override
    String routineText(Connection conn, Ident id, String catalog, String schema, String name,
                       boolean function) {
        return Attempt.<String>on(conn, "SYSCAT.ROUTINES",
                c -> RoutineDdl.db2(c, schema, name), null);
    }

    @Override
    List<String> sequenceQueries(Ident id, String catalog) {
        return List.of(Sequences.DB2_SQL);
    }

    @Override
    void triggerQuery(StringBuilder sql, List<String> args, Ident id, String catalog,
                      String schema, String name) {
        sql.append(Triggers.DB2_SQL);
        Triggers.filter(sql, args, "TRIGSCHEMA", schema);
        Triggers.filter(sql, args, "TRIGNAME", name);
        sql.append(" ORDER BY 2, 3");
    }

    @Override
    boolean upserts() {
        return true;
    }

    @Override
    String upsert(Ident id, String qualified, List<String> columns, List<String> keys,
                  List<String> updates) {
        return Upsert.merge(id, qualified, columns, keys, updates, false, false);
    }

    @Override
    boolean reconstructsGenerated() {
        return true;
    }

    @Override
    List<String> catalogExtras(Connection conn, Ident id, String catalog, String schema,
                               String table, List<Ddl.Col> cols) {
        return Ddl.db2Extras(conn, id, catalog, schema, table, cols);
    }

    /** {@code SYSCAT.INDEXES}, for a table named by its schema. */
    @Override
    List<String> nativeIndexes(Connection conn, Ident id, String catalog, String schema,
                               String table, String qualified) {
        return schema != null && !schema.isEmpty()
                ? Db2Schema.indexes(conn, id, schema, table, qualified) : null;
    }

    @Override
    boolean indexesForeignKeys() {
        return false;
    }

    @Override
    String viewText(Connection conn, ObjectDdl.Target t) {
        return DdlText.db2Text(conn, "SYSCAT.VIEWS", "VIEWSCHEMA", "VIEWNAME", t.schema, t.name);
    }

    @Override
    String triggerText(Connection conn, ObjectDdl.Target t) {
        return DdlText.db2Text(conn, "SYSCAT.TRIGGERS", "TRIGSCHEMA", "TRIGNAME", t.schema,
                t.name);
    }

    /** Db2 keeps {@code NO CACHE} as a cache of 1, and refuses {@code CACHE 1}. */
    @Override
    boolean cacheOfOneIsNoCache() {
        return true;
    }

    /** A string holding line breaks is written in {@link #lines pieces}. */
    @Override
    public String stringLiteral(String s, boolean national, int type, ResultSet rs, int i) {
        if (s.indexOf('\n') >= 0 || s.indexOf('\r') >= 0) {
            return lines(s);
        }
        return Ident.literal(s);
    }

    /**
     * {@code X'…'} is a character string {@code FOR BIT DATA} there, which a
     * {@code BINARY}, {@code VARBINARY} or {@code BLOB} column refuses;
     * {@code BX'…'} is a binary string.
     */
    @Override
    public String hexLiteral(String hex) {
        return "BX'" + hex + "'";
    }

    @Override
    public ScriptClient scriptClient() {
        return ScriptClient.CLP;
    }

    /**
     * A Db2 string holding line breaks, as pieces joined around
     * {@code CHR(10)} and {@code CHR(13)}: the command line processor reads a
     * statement over several lines as one, a blank where each break was, and
     * would put that blank into the value.
     */
    private static String lines(String s) {
        StringBuilder sb = new StringBuilder();
        StringBuilder piece = new StringBuilder();
        for (int k = 0; k < s.length(); k++) {
            char ch = s.charAt(k);
            if (ch == '\n' || ch == '\r') {
                sb.append(Ident.literal(piece.toString())).append(" || CHR(")
                        .append(ch == '\n' ? 10 : 13).append(") || ");
                piece.setLength(0);
            } else {
                piece.append(ch);
            }
        }
        return sb.append(Ident.literal(piece.toString())).toString();
    }
}
