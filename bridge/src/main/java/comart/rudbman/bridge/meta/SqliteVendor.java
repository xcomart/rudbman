package comart.rudbman.bridge.meta;

import com.google.gson.JsonArray;

import java.util.logging.Logger;
import java.util.logging.Level;
import java.util.Set;
import java.util.HashSet;
import java.util.ArrayList;
import java.sql.Statement;
import java.sql.DatabaseMetaData;
import java.sql.SQLException;
import java.sql.ResultSet;
import java.sql.Connection;
import java.util.List;

/** SQLite. */
final class SqliteVendor extends Vendor {

    @Override
    boolean names(String productName) {
        return productName.contains("sqlite");
    }

    /** No trigger catalog: each trigger is read off its statement. */
    @Override
    JsonArray triggers(Connection conn, String name) {
        return Triggers.sqlite(conn, name);
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
    String tableText(Connection conn, Ident id, String catalog, String schema, String table) {
        return Ddl.sqlite(conn, table);
    }

    @Override
    Ddl.Script nativeSplit(Connection conn, Ident id, String catalog, String schema,
                           String table) {
        return Ddl.whole(tableText(conn, id, catalog, schema, table));
    }

    @Override
    String identityClause() {
        return "AUTOINCREMENT";
    }

    @Override
    boolean indexesForeignKeys() {
        return false;
    }

    @Override
    String viewText(Connection conn, ObjectDdl.Target t) {
        return DdlText.sqlite(conn, "view", t.name);
    }

    @Override
    String triggerText(Connection conn, ObjectDdl.Target t) {
        return DdlText.sqlite(conn, "trigger", t.name);
    }

    /** Every value by what it is stored as; see {@link #stored}. */
    @Override
    public String literal(ResultSet rs, int i, int type) throws SQLException {
        return stored(rs, i);
    }

    @Override
    public boolean booleansAreNumbers() {
        return true;
    }

    @Override
    public boolean dropsIfExists() {
        return true;
    }

    @Override
    public String dropForeignKey(String qualified, String key) {
        return null;
    }

    /**
     * A SQLite value by what it is stored as, not by the column's declared
     * type, which SQLite does not hold its values to: an INTEGER column may
     * keep text, and writing that text as a number - or a number as text -
     * would change the value, or break the statement.
     */
    private static String stored(ResultSet rs, int i) throws SQLException {
        Object v = rs.getObject(i);
        if (v == null || rs.wasNull()) {
            return "NULL";
        }
        if (v instanceof Integer || v instanceof Long) {
            return v.toString();
        }
        if (v instanceof Double || v instanceof Float) {
            double d = ((Number) v).doubleValue();
            if (Double.isNaN(d) || Double.isInfinite(d)) {
                // SQLite has no literal for these; 9e999 is its own infinity.
                return Double.isNaN(d) ? "NULL" : d > 0 ? "9e999" : "-9e999";
            }
            return Double.toString(d);
        }
        if (v instanceof byte[]) {
            return "X'" + Ident.hex((byte[]) v) + "'";
        }
        return Ident.literal(v.toString());
    }

    /**
     * An {@code AUTOINCREMENT} table's high-water mark, which the rows alone
     * put back no higher than the largest key left, as {@code .dump} has it.
     */
    @Override
    public List<String> counters(DatabaseMetaData dbm, Ident id, List<Sequence> sequences,
                                 List<Table> tables, boolean data) {
        List<String> out = new ArrayList<>();
        if (!data) {
            return out;
        }
        Set<String> names = new HashSet<>();
        for (Table t : tables) {
            names.add(t.name());
        }
        try (Statement st = dbm.getConnection().createStatement();
             ResultSet rs = st.executeQuery("SELECT name, seq FROM sqlite_sequence")) {
            while (rs.next()) {
                String name = rs.getString(1);
                if (names.contains(name)) {
                    out.add("DELETE FROM sqlite_sequence WHERE name = " + Ident.literal(name)
                            + ";");
                    out.add("INSERT INTO sqlite_sequence (name, seq) VALUES ("
                            + Ident.literal(name) + ", " + rs.getLong(2) + ");");
                }
            }
        } catch (SQLException e) {
            // No AUTOINCREMENT table in the file: no sqlite_sequence either.
            LOG.log(Level.FINE, "no sqlite_sequence", e);
        }
        return out;
    }

    private static final Logger LOG = Logger.getLogger(SqliteVendor.class.getName());
}
