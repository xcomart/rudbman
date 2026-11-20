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
}
