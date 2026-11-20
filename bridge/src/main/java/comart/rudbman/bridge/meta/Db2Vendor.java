package comart.rudbman.bridge.meta;

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
}
