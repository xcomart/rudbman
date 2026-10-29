package comart.rudbman.bridge;

import com.google.gson.JsonArray;
import com.google.gson.JsonObject;
import comart.rudbman.bridge.support.H2;
import org.h2.api.Trigger;
import org.junit.jupiter.api.AfterEach;
import org.junit.jupiter.api.BeforeEach;
import org.junit.jupiter.api.Test;

import java.sql.Connection;

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertFalse;
import static org.junit.jupiter.api.Assertions.assertTrue;

/**
 * DESCRIBE kind {@code triggers}, and kind {@code ddl} for every {@code object}
 * that is not a table: a view, a sequence, a trigger.
 */
class DescribeObjectsTest {

    private long session;

    /** A trigger that does nothing; H2 instantiates it by class name. */
    public static final class Quiet implements Trigger {
        @Override
        public void fire(Connection conn, Object[] oldRow, Object[] newRow) {
        }
    }

    @BeforeEach
    void setUp() {
        session = H2.open(H2.freshUrl());
        H2.exec(session, "create schema app");
        H2.exec(session, "create table app.person (id int primary key, note varchar(20))");
        H2.exec(session, "create view app.rich as select id from app.person where id > 1");
        H2.exec(session, "create sequence app.seq_order start with 100 increment by 5");
        H2.exec(session, "create trigger app.trg_person before insert, update on app.person "
                + "for each row call '" + Quiet.class.getName() + "'");
    }

    @AfterEach
    void tearDown() {
        H2.close(session);
    }

    private JsonObject call(String... kv) {
        JsonObject req = new JsonObject();
        for (int i = 0; i < kv.length; i += 2) {
            req.addProperty(kv[i], kv[i + 1]);
        }
        return H2.call(Ops.DESCRIBE, session, 0, req).json();
    }

    private JsonObject ddl(String object, String name, String source) {
        return call("kind", "ddl", "object", object, "schema", "APP", "name", name,
                "source", source);
    }

    @Test
    void triggersAreListedWithWhatTheyFireOn() {
        JsonArray items = call("kind", "triggers", "schema", "APP").get("items").getAsJsonArray();
        assertEquals(1, items.size(), items.toString());
        JsonObject t = items.get(0).getAsJsonObject();
        assertEquals("TRG_PERSON", t.get("name").getAsString());
        assertEquals("APP", t.get("schema").getAsString());
        assertEquals("PERSON", t.get("table").getAsString());
        assertEquals("BEFORE", t.get("timing").getAsString());
        // One row per event in the standard view, merged into one trigger.
        assertEquals("INSERT OR UPDATE", t.get("events").getAsString());
        assertEquals("ROW", t.get("orientation").getAsString());

        assertTrue(call("kind", "triggers", "schema", "APP", "name", "NOPE")
                .get("items").getAsJsonArray().isEmpty());
    }

    /**
     * A view is answered with its query, never with the {@code CREATE TABLE}
     * reconstructing its column list would write; and where the server's own
     * text is not asked for, the standard view's definition stands in.
     */
    @Test
    void aViewIsAnsweredWithItsQuery() {
        JsonObject own = ddl("view", "RICH", "auto");
        assertEquals("native", own.get("source").getAsString());
        String text = own.get("ddl").getAsString();
        assertTrue(text.startsWith("CREATE FORCE VIEW \"APP\".\"RICH\""), text);

        JsonObject rebuilt = ddl("view", "RICH", "metadata");
        assertEquals("metadata", rebuilt.get("source").getAsString());
        text = rebuilt.get("ddl").getAsString();
        assertTrue(text.startsWith("CREATE VIEW "), text);
        assertFalse(text.contains("CREATE TABLE"), text);
    }

    /** A sequence rebuilt from the catalog is complete, and says so. */
    @Test
    void aSequenceIsAnsweredWithItsOptions() {
        JsonObject own = ddl("sequence", "SEQ_ORDER", "auto");
        assertEquals("native", own.get("source").getAsString());
        assertTrue(own.get("ddl").getAsString()
                .startsWith("CREATE SEQUENCE \"APP\".\"SEQ_ORDER\""), own.toString());

        JsonObject rebuilt = ddl("sequence", "SEQ_ORDER", "metadata");
        assertEquals("catalog", rebuilt.get("source").getAsString());
        String text = rebuilt.get("ddl").getAsString();
        assertTrue(text.contains("START WITH 100"), text);
        assertTrue(text.contains("INCREMENT BY 5"), text);
    }

    @Test
    void aTriggerIsAnsweredWithItsOwnText() {
        JsonObject own = ddl("trigger", "TRG_PERSON", "auto");
        assertEquals("native", own.get("source").getAsString());
        String text = own.get("ddl").getAsString();
        assertTrue(text.contains("TRIGGER \"APP\".\"TRG_PERSON\""), text);
        assertTrue(text.contains(Quiet.class.getName()), text);
    }

    @Test
    void anUnknownObjectIsAProtocolErrorAndAMissingOneIsNamed() {
        JsonObject req = new JsonObject();
        req.addProperty("kind", "ddl");
        req.addProperty("object", "index");
        req.addProperty("name", "X");
        assertEquals("protocol",
                H2.call(Ops.DESCRIBE, session, 0, req).error().get("kind").getAsString());

        req = new JsonObject();
        req.addProperty("kind", "ddl");
        req.addProperty("object", "view");
        req.addProperty("schema", "APP");
        req.addProperty("name", "NOPE");
        JsonObject err = H2.call(Ops.DESCRIBE, session, 0, req).error();
        assertEquals("sql", err.get("kind").getAsString(), err.toString());
        assertTrue(err.get("message").getAsString().contains("NOPE"), err.toString());
    }
}
