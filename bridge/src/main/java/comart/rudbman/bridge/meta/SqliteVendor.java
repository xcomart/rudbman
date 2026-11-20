package comart.rudbman.bridge.meta;

import com.google.gson.JsonArray;

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
}
