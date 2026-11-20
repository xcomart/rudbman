package comart.rudbman.bridge.meta;

import java.util.HashSet;
import java.util.ArrayList;
import java.sql.SQLException;
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

    /** An extension's own objects come back with {@code CREATE EXTENSION}. */
    @Override
    public Set<String> extensionMembers(Connection conn, String schema) {
        return PgSchema.extensionMembers(conn, schema);
    }

    @Override
    public List<String[]> backupPreamble(Connection conn, Ident id, String catalog,
                                         String schema) {
        return PgSchema.preamble(conn, id, schema);
    }

    @Override
    public List<String> ownedSequences(Connection conn, Ident id, String schema,
                                       Set<String> names) {
        return PgSchema.ownedSequences(conn, id, schema, names);
    }

    /** A routine's body is checked when it runs, not when the script makes it. */
    @Override
    public List<String> backupPrologue(Ident id, String catalog, boolean routines) {
        return routines ? List.of("SET check_function_bodies = false;") : List.of();
    }

    @Override
    public List<String> backupEpilogue(String schema, boolean routines, boolean code) {
        return routines ? List.of("RESET check_function_bodies;") : List.of();
    }

    /**
     * Every serial's and identity's counter as the source has it, as pg_dump
     * carries it, then each sequence's.
     */
    @Override
    public List<String> counters(DatabaseMetaData dbm, Ident id, List<Sequence> sequences,
                                 List<Table> tables, boolean data) throws SQLException {
        List<String> out = new ArrayList<>();
        if (data) {
            Set<String> written = new HashSet<>();
            for (Sequence s : sequences) {
                written.add(id.qualify(null, s.schema(), s.name()));
            }
            Connection conn = dbm.getConnection();
            for (Table t : tables) {
                String qualified = id.qualify(null, t.schema(), t.name());
                for (Ddl.DataColumn c : Ddl.dataColumns(dbm, t.catalog(), t.schema(), t.name())) {
                    if (!c.autoIncrement) {
                        continue;
                    }
                    String serial = "pg_catalog.pg_get_serial_sequence(" + Ident.literal(qualified)
                            + ", " + Ident.literal(c.name) + ")";
                    // The counter as the source has it, as pg_dump carries it: a
                    // key handed out and rolled back or deleted is not handed out
                    // again. A serial's own sequence is in the script already.
                    String[] state = PgSchema.sequenceState(conn, serial);
                    if (state != null && written.contains(state[0])) {
                        continue;
                    }
                    if (state != null && state[1] != null && state[1].matches("-?\\d+")) {
                        out.add("SELECT pg_catalog.setval(" + serial + ", " + state[1] + ", "
                                + state[2] + ");");
                        continue;
                    }
                    String col = id.q(c.name);
                    out.add("SELECT pg_catalog.setval(" + serial + ", MAX(" + col + ")) FROM "
                            + qualified + " HAVING MAX(" + col + ") IS NOT NULL;");
                }
            }
        }
        for (Sequence s : sequences) {
            if (integer(s.value())) {
                out.add("SELECT pg_catalog.setval("
                        + Ident.literal(id.qualify(null, s.schema(), s.name())) + ", " + s.value()
                        + ", true);");
            }
        }
        return out;
    }
}
