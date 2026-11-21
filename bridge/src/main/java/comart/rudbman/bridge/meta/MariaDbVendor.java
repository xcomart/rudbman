package comart.rudbman.bridge.meta;

import java.util.logging.Logger;
import java.util.logging.Level;
import java.util.ArrayList;
import java.sql.Statement;
import java.sql.SQLException;
import java.sql.ResultSet;
import java.sql.DatabaseMetaData;
import java.util.List;
import java.util.Map;
import com.google.gson.JsonArray;
import com.google.gson.JsonObject;

import java.sql.Connection;

/**
 * MariaDB: MySQL's answers, read from {@link MySqlVendor} rather than copied,
 * except where its catalog differs.
 */
final class MariaDbVendor extends MySqlVendor {

    /**
     * Tested before MySQL: the MariaDB server reports "MariaDB", but several
     * MySQL-compatible builds put both words in the string.
     */
    @Override
    boolean names(String productName) {
        return productName.contains("mariadb");
    }

    /** Only MariaDB of the two has sequences, and lists them as tables. */
    @Override
    public List<Map<String, String>> sequences(Connection conn, Ident id, String catalog,
                                               String schema, String name) {
        return Sequences.mariadb(conn, id, catalog, schema, name);
    }

    /** MariaDB's flag is right; only MySQL's needs confirming. */
    @Override
    void confirmGenerated(Connection conn, String catalog, String table, List<Ddl.Col> cols) {
    }

    @Override
    String sequenceText(Connection conn, ObjectDdl.Target t) {
        return ObjectDdl.show(conn, "SEQUENCE", t, 2);
    }

    @Override
    public String sequenceValue(Connection conn, Ident id, String catalog, String schema,
                                String name, String listed) {
        return nextNotCached(conn, id.qualify(catalog, null, name));
    }

    @Override
    public List<String> counters(DatabaseMetaData dbm, Ident id, List<Sequence> sequences,
                                 List<Table> tables, boolean data) {
        List<String> out = new ArrayList<>();
        for (Sequence s : sequences) {
            if (integer(s.value())) {
                out.add("SELECT SETVAL(" + id.qualify(s.catalog(), null, s.name()) + ", "
                        + s.value() + ", 0);");
            }
        }
        return out;
    }

    /**
     * MariaDB's next value not yet handed to a cache: where a restored
     * sequence has to continue from. {@code SHOW CREATE SEQUENCE} writes the
     * original start instead.
     */
    private static String nextNotCached(Connection conn, String qualified) {
        try (Statement st = conn.createStatement();
             ResultSet rs = st.executeQuery("SELECT next_not_cached_value FROM " + qualified)) {
            return rs.next() ? rs.getString(1) : null;
        } catch (SQLException e) {
            LOG.log(Level.FINE, "no sequence state for " + qualified, e);
            return null;
        }
    }

    private static final Logger LOG = Logger.getLogger(MariaDbVendor.class.getName());
}
