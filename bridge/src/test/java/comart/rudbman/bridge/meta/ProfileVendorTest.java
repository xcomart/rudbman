package comart.rudbman.bridge.meta;

import com.google.gson.JsonArray;
import com.google.gson.JsonObject;
import com.google.gson.JsonParser;

import comart.rudbman.bridge.BridgeException;

import org.junit.jupiter.api.AfterAll;
import org.junit.jupiter.api.BeforeAll;
import org.junit.jupiter.api.Test;

import java.sql.Connection;
import java.sql.DriverManager;
import java.sql.SQLException;
import java.sql.Statement;
import java.sql.Types;
import java.util.ArrayList;
import java.util.List;
import java.util.Map;

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertFalse;
import static org.junit.jupiter.api.Assertions.assertNotNull;
import static org.junit.jupiter.api.Assertions.assertNull;
import static org.junit.jupiter.api.Assertions.assertSame;
import static org.junit.jupiter.api.Assertions.assertThrows;
import static org.junit.jupiter.api.Assertions.assertTrue;

/**
 * {@link ProfileVendor}: a dialect profile's {@code bridge} half over its base,
 * key by key, and what {@link Profiles} attaches to a session.
 */
class ProfileVendorTest {

    private static Connection h2;
    private static Ident id;

    @BeforeAll
    static void open() throws SQLException {
        h2 = DriverManager.getConnection("jdbc:h2:mem:profiles;DB_CLOSE_DELAY=-1");
        try (Statement st = h2.createStatement()) {
            st.execute("CREATE TABLE T (ID INT PRIMARY KEY, NOTE VARCHAR(20))");
            st.execute("CREATE SEQUENCE S START WITH 5");
            st.execute("CREATE TRIGGER TR BEFORE INSERT ON T FOR EACH ROW CALL \""
                    + Noop.class.getName() + "\"");
        }
        id = Ident.of(h2.getMetaData());
    }

    @AfterAll
    static void close() throws SQLException {
        h2.close();
    }

    /** A trigger that does nothing, for the catalog to list. */
    public static class Noop implements org.h2.api.Trigger {
        @Override
        public void fire(Connection conn, Object[] oldRow, Object[] newRow) {
        }
    }

    private static ProfileVendor over(Dialect base, String json) {
        JsonObject data = JsonParser.parseString(json).getAsJsonObject();
        return new ProfileVendor("test", base.vendor(), data);
    }

    /** Every answer that needs no connection, in one list to compare. */
    private static List<Object> answers(Vendor v) throws SQLException {
        List<Object> out = new ArrayList<>();
        out.add(v.catalogIsSchema());
        out.add(v.resultCatalogsResolve());
        out.add(v.seesOnlyItsCatalog());
        out.add(v.numbersProcedures());
        out.add(v.marksViewsInRemarks());
        out.add(v.hidesComments());
        out.add(v.upserts());
        out.add(v.upsert(id, "\"T\"", List.of("ID", "NOTE"), List.of("ID"), List.of("NOTE")));
        out.add(v.dataPrologue());
        out.add(v.dataEpilogue());
        out.add(v.scriptPrologue());
        out.add(v.scriptEpilogue());
        out.add(v.reconstructsGenerated());
        out.add(v.qualifiesByCatalog());
        out.add(v.defersKeys());
        out.add(v.commentsInline());
        out.add(v.commentsOn());
        out.add(v.computedHasNoType());
        out.add(v.serialColumns());
        out.add(v.identityClause());
        out.add(v.notNullFirst());
        out.add(v.maxType("VARCHAR", 100_000));
        out.add(v.indexesForeignKeys());
        out.add(v.createsWithoutCatalog());
        out.add(v.typesSequences());
        out.add(v.cacheOfOneIsNoCache());
        out.add(v.bitsAreNumbers());
        out.add(v.booleansAreNumbers());
        out.add(v.stringLiteral("it's", false, Types.VARCHAR, null, 1));
        out.add(v.stringLiteral("it's", true, Types.NVARCHAR, null, 1));
        out.add(v.hexLiteral("00FF"));
        out.add(v.scriptClient());
        out.add(v.dropsIfExists());
        out.add(v.dropTableCascade());
        out.add(v.dropForeignKey("s.t", "fk"));
        out.add(v.multiRowValues());
        out.add(v.packagesRoutines());
        out.add(v.backupPrologue(id, "db", "app", true));
        out.add(v.backupEpilogue(id, "db", "app", true, true));
        out.add(v.textColumn("\"c\""));
        out.add(v.overridesSystemValue());
        out.add(v.sequenceQueries(id, "db"));
        StringBuilder sql = new StringBuilder();
        List<String> args = new ArrayList<>();
        v.triggerQuery(sql, args, id, "db", "app", "tr");
        out.add(sql.toString());
        out.add(args);
        return out;
    }

    @Test
    void anEmptyProfileIsItsBase() throws SQLException {
        for (Dialect base : Dialect.values()) {
            assertEquals(answers(base.vendor()), answers(over(base, "{}")), base.name());
        }
    }

    @Test
    void eachKeyAnswersFromTheProfile() throws SQLException {
        ProfileVendor v = over(Dialect.OTHER, "{"
                + "\"catalog_is_schema\": true, \"sees_only_its_catalog\": true,"
                + "\"comments_on\": false, \"comments_inline\": true,"
                + "\"identity_clause\": \"AUTO_INCREMENT\", \"not_null_first\": true,"
                + "\"bits_are_numbers\": true, \"booleans_are_numbers\": true,"
                + "\"script_client\": \"mysql\", \"drops_if_exists\": true,"
                + "\"drop_table_cascade\": \" CASCADE\", \"multi_row_values\": false,"
                + "\"script_prologue\": [\"SET FOREIGN_KEY_CHECKS = 0;\"],"
                + "\"script_epilogue\": [\"SET FOREIGN_KEY_CHECKS = 1;\"],"
                + "\"hex_literal\": \"0x{hex}\","
                + "\"drop_foreign_key\": \"ALTER TABLE {table} DROP FOREIGN KEY {key};\","
                + "\"overrides_system_value\": true"
                + "}");
        assertTrue(v.catalogIsSchema());
        assertTrue(v.seesOnlyItsCatalog());
        assertFalse(v.commentsOn());
        assertTrue(v.commentsInline());
        assertEquals("AUTO_INCREMENT", v.identityClause());
        assertTrue(v.notNullFirst());
        assertTrue(v.bitsAreNumbers());
        assertTrue(v.booleansAreNumbers());
        assertSame(Vendor.ScriptClient.MYSQL, v.scriptClient());
        assertTrue(v.dropsIfExists());
        assertEquals(" CASCADE", v.dropTableCascade());
        assertFalse(v.multiRowValues());
        assertEquals(List.of("SET FOREIGN_KEY_CHECKS = 0;"), v.scriptPrologue());
        assertEquals(List.of("SET FOREIGN_KEY_CHECKS = 1;"), v.scriptEpilogue());
        assertEquals("0x00FF", v.hexLiteral("00FF"));
        assertEquals("ALTER TABLE s.t DROP FOREIGN KEY fk;", v.dropForeignKey("s.t", "fk"));
        assertTrue(v.overridesSystemValue());
    }

    @Test
    void templatesFillTheirHolesAndKeepEscapedBraces() {
        assertEquals("{x} = 7", ProfileTemplate.render("{{x}} = {n}", Map.of("n", "7")));
        assertEquals("{unknown}", ProfileTemplate.render("{unknown}", Map.of()));
        Map<String, String> none = new java.util.HashMap<>();
        none.put("catalog", null);
        assertNull(ProfileTemplate.render("USE {catalog};", none), "no value, no statement");
    }

    @Test
    void nullSaysTheProductHasNone() throws SQLException {
        ProfileVendor v = over(Dialect.SQLSERVER, "{\"drop_foreign_key\": null,"
                + " \"identity_insert\": null, \"triggers\": null}");
        assertNull(v.dropForeignKey("s.t", "fk"));
        assertSame(Vendor.RowFence.NONE, v.rowFence(h2, id, "PUBLIC", "T", "\"T\"",
                List.of("ID")));
        assertEquals(new JsonArray(), v.triggers(h2, null));
    }

    @Test
    void stringsAreNationalByTheProfilesRule() throws SQLException {
        assertEquals("N'a'", over(Dialect.OTHER, "{\"national_strings\": \"always\"}")
                .stringLiteral("a", false, Types.VARCHAR, null, 1));
        ProfileVendor typed = over(Dialect.OTHER, "{\"national_strings\": \"national_types\"}");
        assertEquals("'a'", typed.stringLiteral("a", false, Types.VARCHAR, null, 1));
        assertEquals("N'a'", typed.stringLiteral("a", true, Types.NVARCHAR, null, 1));
        // On MySQL, `never` is the standard quoting rather than MySQL's escapes.
        assertEquals("'a\\b'", over(Dialect.MYSQL, "{\"national_strings\": \"never\"}")
                .stringLiteral("a\\b", false, Types.VARCHAR, null, 1));
    }

    @Test
    void eachUpsertKindWritesItsStatement() {
        List<String> cols = List.of("ID", "NOTE");
        List<String> keys = List.of("ID");
        List<String> updates = List.of("NOTE");
        assertFalse(over(Dialect.POSTGRESQL, "{\"upsert\": \"none\"}").upserts());
        assertEquals(Upsert.onDuplicateKey(id, "t", cols, keys, updates),
                over(Dialect.OTHER, "{\"upsert\": \"on_duplicate_key\"}")
                        .upsert(id, "t", cols, keys, updates));
        assertEquals(Upsert.onConflict(id, "t", cols, keys, updates),
                over(Dialect.OTHER, "{\"upsert\": \"on_conflict\"}")
                        .upsert(id, "t", cols, keys, updates));
        assertEquals(Upsert.h2Merge(id, "t", cols, keys),
                over(Dialect.OTHER, "{\"upsert\": \"h2_merge\"}")
                        .upsert(id, "t", cols, keys, updates));
        ProfileVendor merge = over(Dialect.OTHER,
                "{\"upsert\": {\"merge\": {\"from_dual\": true, \"terminated\": false}}}");
        assertTrue(merge.upserts());
        assertEquals(Upsert.merge(id, "t", cols, keys, updates, true, false),
                merge.upsert(id, "t", cols, keys, updates));
    }

    @Test
    void backupLinesFollowTheirWhenAndTheirHoles() {
        ProfileVendor v = over(Dialect.OTHER, "{"
                + "\"backup_prologue\": [\"USE {catalog};\","
                + " {\"sql\": \"SET check_function_bodies = false;\", \"when\": \"routines\"}],"
                + "\"backup_epilogue\": [{\"sql\": \"CALL recompile({schema});\","
                + " \"when\": \"code\"}]"
                + "}");
        assertEquals(List.of("USE \"db\";", "SET check_function_bodies = false;"),
                v.backupPrologue(id, "db", null, true));
        assertEquals(List.of(), v.backupPrologue(id, null, null, false),
                "no catalog, no USE; no routines, no SET");
        assertEquals(List.of("CALL recompile(\"app\");"),
                v.backupEpilogue(id, null, "app", false, true));
        assertEquals(List.of(), v.backupEpilogue(id, null, "app", false, false));
    }

    @Test
    void identityInsertFencesOnlyATableWithIdentities() throws SQLException {
        ProfileVendor v = over(Dialect.OTHER, "{\"identity_insert\": {"
                + "\"before\": \"SET IDENTITY_INSERT {table} ON;\","
                + "\"after\": \"SET IDENTITY_INSERT {table} OFF;\"}}");
        Vendor.RowFence fence = v.rowFence(h2, id, "PUBLIC", "T", "dbo.t", List.of("ID"));
        assertEquals(List.of("SET IDENTITY_INSERT dbo.t ON;"), fence.before());
        assertEquals(List.of("SET IDENTITY_INSERT dbo.t OFF;"), fence.after());
        assertSame(Vendor.RowFence.NONE, v.rowFence(h2, id, "PUBLIC", "T", "dbo.t", List.of()));
    }

    @Test
    void aValueOfTheWrongShapeFailsTheOpen() {
        assertThrows(BridgeException.class, () -> over(Dialect.OTHER, "{\"upsert\": \"maybe\"}"));
        assertThrows(BridgeException.class,
                () -> over(Dialect.OTHER, "{\"national_strings\": \"sometimes\"}"));
        assertThrows(BridgeException.class,
                () -> over(Dialect.OTHER, "{\"script_client\": \"psql\"}"));
        assertThrows(BridgeException.class, () -> Profiles.dialect(JsonParser.parseString(
                "{\"id\": \"x\", \"base\": \"tidb\", \"bridge\": {}}").getAsJsonObject()));
    }

    // ------------------------------------------------- against a connection

    /** The request {@code OPEN_SESSION} would carry, with one profile. */
    private static JsonObject request(String member, String profile) {
        return JsonParser.parseString("{\"" + member + "\": " + profile + "}")
                .getAsJsonObject();
    }

    @Test
    void aSessionsProfileIsWhatItsConnectionAnswers() throws SQLException {
        Profiles.attach(h2, request("profile",
                "{\"id\": \"mine\", \"base\": \"h2\", \"bridge\": {\"hex_literal\": \"0x{hex}\"}}"));
        try {
            Dialect dialect = Dialect.of(h2.getMetaData());
            assertEquals("mine", dialect.name());
            assertEquals("0x00", dialect.vendor().hexLiteral("00"));
            // What the profile leaves out is H2's.
            assertEquals(Dialect.H2.vendor().scriptPrologue(), dialect.vendor().scriptPrologue());
        } finally {
            Dialect.detach(h2);
        }
        assertSame(Dialect.H2, Dialect.of(h2.getMetaData()), "detached, H2 again");
    }

    @Test
    void aProfileThatNamesTheProductIsDetected() throws SQLException {
        Profiles.attach(h2, request("detect_profiles", "["
                + "{\"id\": \"other\", \"base\": \"generic\", \"bridge\": {\"detect\": [\"tidb\"]}},"
                + "{\"id\": \"h2ish\", \"base\": \"h2\", \"bridge\": {\"detect\": [\"H2\"]}}]"));
        try {
            assertEquals("h2ish", Dialect.of(h2.getMetaData()).name());
        } finally {
            Dialect.detach(h2);
        }
        Profiles.attach(h2, request("detect_profiles",
                "[{\"id\": \"other\", \"base\": \"generic\", \"bridge\": {\"detect\": [\"tidb\"]}}]"));
        assertSame(Dialect.H2, Dialect.of(h2.getMetaData()), "no profile names H2");
    }

    @Test
    void theQueriesOfAProfileAreRun() throws SQLException {
        String bridge = "{"
                + "\"sequences\": [],"
                + "\"triggers\": {\"sql\": \"SELECT TRIGGER_CATALOG, TRIGGER_SCHEMA,"
                + " TRIGGER_NAME, EVENT_OBJECT_TABLE, ACTION_TIMING, EVENT_MANIPULATION,"
                + " ACTION_ORIENTATION, NULL, 'from the profile'"
                + " FROM INFORMATION_SCHEMA.TRIGGERS WHERE 1 = 1\","
                + " \"filters\": {\"schema\": \"TRIGGER_SCHEMA\", \"name\": \"TRIGGER_NAME\"},"
                + " \"order_by\": \"3\"},"
                + "\"comments\": {\"hides_comments\": true, \"tables\": {"
                + " \"sql\": \"SELECT TABLE_SCHEMA, TABLE_NAME, 'noted'"
                + " FROM INFORMATION_SCHEMA.TABLES WHERE 1 = 1\","
                + " \"filters\": {\"schema\": \"TABLE_SCHEMA\", \"table\": \"TABLE_NAME\"}}},"
                + "\"texts\": {\"table\": {\"sql\": \"SELECT 'CREATE TABLE {qualified} (' || ? || ')'\","
                + " \"binds\": [\"name\"]}, \"view\": null}"
                + "}";
        ProfileVendor v = over(Dialect.H2, bridge);
        Profiles.attach(h2, request("profile",
                "{\"id\": \"q\", \"base\": \"h2\", \"bridge\": " + bridge + "}"));
        try {
            JsonObject req = JsonParser.parseString("{\"schema\": \"PUBLIC\"}").getAsJsonObject();
            assertEquals(0, Sequences.of(h2, h2.getMetaData(), req).size(),
                    "an empty list of queries lists none");
            JsonArray triggers = Triggers.of(h2, h2.getMetaData(), req);
            assertEquals(1, triggers.size(), triggers.toString());
            assertEquals("from the profile",
                    triggers.get(0).getAsJsonObject().get("remarks").getAsString());

            JsonObject table = JsonParser.parseString(
                    "{\"schema\": \"PUBLIC\", \"table\": \"T\"}").getAsJsonObject();
            assertTrue(v.hidesComments());
            assertTrue(v.tableComments(h2, table).containsValue("noted"));

            assertEquals("CREATE TABLE PUBLIC.T (T);",
                    v.tableText(h2, id, null, "PUBLIC", "T"));
            assertNotNull(v.nativeSplit(h2, id, null, "PUBLIC", "T"));
        } finally {
            Dialect.detach(h2);
        }
        // Without the profile, H2 lists its sequence.
        JsonObject req = JsonParser.parseString("{\"schema\": \"PUBLIC\"}").getAsJsonObject();
        assertEquals(1, Sequences.of(h2, h2.getMetaData(), req).size());
    }
}
