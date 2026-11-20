package comart.rudbman.bridge.meta;

import com.google.gson.JsonArray;
import com.google.gson.JsonObject;

import java.sql.Connection;
import java.sql.DatabaseMetaData;
import java.util.List;

/** PostgreSQL and wire-compatible forks that keep the product name. */
final class PostgresVendor extends Vendor {

    @Override
    boolean names(String productName) {
        return productName.contains("postgresql") || productName.contains("postgres");
    }

    /**
     * pgjdbc lists every database on the server, but a PostgreSQL session sees
     * only the one it connected to: every metadata call answers about that one
     * whatever catalog it is given.
     */
    @Override
    boolean seesOnlyItsCatalog() {
        return true;
    }

    /** A routine name is overloaded, and each overload is told by its arguments. */
    @Override
    JsonArray identifyOverloads(DatabaseMetaData dbm, JsonObject req, JsonArray items) {
        return Routines.postgresOverloads(dbm, req, items);
    }

    @Override
    String routineText(Connection conn, Ident id, String catalog, String schema, String name,
                       boolean function) {
        return Attempt.<String>on(conn, "pg_get_functiondef",
                c -> RoutineDdl.postgres(c, schema, name), null);
    }

    @Override
    List<String> sequenceQueries(Ident id, String catalog) {
        return List.of(Sequences.POSTGRESQL_CATALOG_SQL, Sequences.POSTGRESQL_SQL);
    }

    @Override
    void triggerQuery(StringBuilder sql, List<String> args, Ident id, String catalog,
                      String schema, String name) {
        sql.append(Triggers.POSTGRESQL_SQL);
        Triggers.filter(sql, args, "n.nspname", schema);
        Triggers.filter(sql, args, "t.tgname", name);
        sql.append(" ORDER BY 2, 3");
    }

    @Override
    boolean upserts() {
        return true;
    }

    @Override
    String upsert(Ident id, String qualified, List<String> columns, List<String> keys,
                  List<String> updates) {
        return Upsert.onConflict(id, qualified, columns, keys, updates);
    }

    @Override
    public boolean overridesSystemValue() {
        return true;
    }
}
