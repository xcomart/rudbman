package comart.rudbman.bridge.meta;

import java.util.Set;
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

    @Override
    boolean reconstructsGenerated() {
        return true;
    }

    @Override
    List<String> catalogExtras(Connection conn, Ident id, String catalog, String schema,
                               String table, List<Ddl.Col> cols) {
        return Ddl.postgresExtras(conn, id, catalog, schema, table, cols);
    }

    @Override
    Set<String> alwaysIdentities(Connection conn, String schema, String table) {
        return Ddl.pgIdentityAlways(conn, table, schema);
    }

    /** A catalog-qualified name reads as a cross-database reference. */
    @Override
    boolean qualifiesByCatalog() {
        return false;
    }

    @Override
    boolean defersKeys() {
        return true;
    }

    @Override
    List<String> nativeIndexes(Connection conn, Ident id, String catalog, String schema,
                               String table, String qualified) {
        return Ddl.pgIndexes(conn, schema, table, qualified);
    }

    /**
     * pgjdbc names a serial column's type "serial", which would make the
     * server create the sequence again, and calls it auto-increment, which
     * would make it an identity.
     */
    @Override
    boolean serialColumns() {
        return true;
    }

    @Override
    boolean indexesForeignKeys() {
        return false;
    }

    @Override
    String viewText(Connection conn, ObjectDdl.Target t) {
        return Attempt.<String>on(conn, "pg_get_viewdef", c -> ObjectDdl.postgresView(c, t),
                null);
    }

    @Override
    String triggerText(Connection conn, ObjectDdl.Target t) {
        return ObjectDdl.postgresTrigger(conn, t);
    }

    /** {@code bytea}'s hex input form. */
    @Override
    public String hexLiteral(String hex) {
        return "'\\x" + hex + "'";
    }

    @Override
    public boolean dropsIfExists() {
        return true;
    }

    @Override
    public String dropForeignKey(String qualified, String key) {
        return "ALTER TABLE IF EXISTS " + qualified + " DROP CONSTRAINT IF EXISTS " + key + ";";
    }
}
