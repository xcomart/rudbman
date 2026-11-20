package comart.rudbman.bridge.meta;

import java.util.List;
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
    JsonArray sequences(Connection conn, Ident id, JsonObject req) {
        return Sequences.mariadb(conn, id, req);
    }

    /** MariaDB's flag is right; only MySQL's needs confirming. */
    @Override
    void confirmGenerated(Connection conn, String catalog, String table, List<Ddl.Col> cols) {
    }

    @Override
    String sequenceText(Connection conn, ObjectDdl.Target t) {
        return ObjectDdl.show(conn, "SEQUENCE", t, 2);
    }
}
