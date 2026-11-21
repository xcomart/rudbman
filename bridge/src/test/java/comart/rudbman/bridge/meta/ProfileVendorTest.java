package comart.rudbman.bridge.meta;

import com.google.gson.JsonArray;
import com.google.gson.JsonObject;
import com.google.gson.JsonParser;

import comart.rudbman.bridge.BridgeException;

import org.junit.jupiter.api.AfterAll;
import org.junit.jupiter.api.BeforeAll;
import org.junit.jupiter.api.Test;

import java.lang.reflect.InvocationTargetException;
import java.lang.reflect.Method;
import java.sql.Connection;
import java.sql.DriverManager;
import java.sql.ResultSet;
import java.sql.SQLException;
import java.sql.Statement;
import java.sql.Types;
import java.util.ArrayList;
import java.util.List;
import java.util.Map;
import java.util.Set;
import java.util.StringJoiner;
import java.util.concurrent.ConcurrentHashMap;

import static org.junit.jupiter.api.Assertions.assertArrayEquals;
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
        assertEquals(List.of(), v.triggers(h2, null, null, null));
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
            assertTrue(v.tableComments(h2, Comments.filters(table)).stream()
                    .anyMatch(row -> "noted".equals(row[2])));

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

    // ------------------------------------------------------- scripts (§5)

    /** @return the Groovy runtime's JARs, as the test task hands them over */
    static List<String> groovy() {
        String path = System.getProperty("rudbman.test.groovy");
        assertNotNull(path, "the test task sets rudbman.test.groovy");
        return List.of(path.split(java.io.File.pathSeparator));
    }

    /** What the contract tests' scripts were last called with, by hook. */
    private static final Map<String, Object[]> CALLED = new ConcurrentHashMap<>();

    /**
     * Called by the contract tests' scripts.
     *
     * @param hook the hook the script is
     * @param args what it was called with
     */
    public static void called(String hook, Object[] args) {
        CALLED.put(hook, args);
    }

    /** @return a profile of the request's shape, with a script */
    static JsonObject profile(String base, String bridge, String file, String text) {
        JsonObject profile = new JsonObject();
        profile.addProperty("id", "scripted");
        profile.addProperty("base", base);
        profile.add("bridge", JsonParser.parseString(bridge).getAsJsonObject());
        if (text != null) {
            JsonObject script = new JsonObject();
            script.addProperty("path", "/profiles/" + file);
            script.addProperty("text", text);
            profile.add("script", script);
        }
        return profile;
    }

    private static Vendor scripted(String base, String bridge, String file, String text) {
        return Profiles.dialect(profile(base, bridge, file, text), groovy()).vendor();
    }

    /** @return a method named for {@code hook}, with its parameters, and {@code body} */
    private static String define(String hook, String body) {
        Method m = ProfileScript.hookMethods().get(hook);
        StringJoiner params = new StringJoiner(", ");
        for (int i = 0; i < m.getParameterCount(); i++) {
            params.add(m.getParameterTypes()[i].getCanonicalName() + " a" + i);
        }
        return "def " + hook + "(" + params + ") {\n" + body + "\n}\n";
    }

    /** @return the documented arguments a hook is called with */
    private static Object[] arguments(String hook, ResultSet rs) throws SQLException {
        Vendor.Sequence seq = new Vendor.Sequence("db", "app", "s1", "41", "1", "1");
        Vendor.Table table = new Vendor.Table("db", "app", "t");
        switch (hook) {
            case "literal":
                return new Object[]{rs, 1, Types.VARCHAR};
            case "stringLiteral":
                return new Object[]{"it's", true, Types.NVARCHAR, rs, 1};
            case "hexLiteral":
                return new Object[]{"00FF"};
            case "routineRemarks":
                return new Object[]{"Standalone procedure or function"};
            case "routineText":
                return new Object[]{h2, id, "db", "app", "f", true};
            case "sequences":
                return new Object[]{h2, id, "db", "app", "s1"};
            case "triggers":
                return new Object[]{h2, "db", "app", "tr"};
            case "triggerRow":
                return new Object[]{new String[9]};
            case "tableComments":
                return new Object[]{h2, Map.of("schema", "PUBLIC", "table_pattern", "T%")};
            case "columnComments":
                return new Object[]{h2, Map.of("table", "T", "column", "NOTE")};
            case "extensionMembers":
            case "materializedViews":
            case "tablesNotWritten":
            case "indexViews":
            case "packages":
                return new Object[]{h2, "app"};
            case "backupPreamble":
                return new Object[]{h2, id, "db", "app"};
            case "ownedSequences":
                return new Object[]{h2, id, "app", Set.of("s1")};
            case "sequenceValue":
                return new Object[]{h2, id, "db", "app", "s1", "41"};
            case "resumeSequence":
                return new Object[]{h2, "app", "s1", "CREATE SEQUENCE s1"};
            case "sqlMode":
                return new Object[]{h2, "db", "f", "function"};
            case "overloadTexts":
                return new Object[]{h2, "app", "f"};
            case "routineDrops":
                return new Object[]{h2, id, "app", "f", true};
            case "textColumns":
                return new Object[]{h2, "app", "t"};
            case "textColumn":
                return new Object[]{"\"c\""};
            case "rowFence":
                return new Object[]{h2, id, "app", "t", "\"t\"", List.of("ID")};
            case "counters":
                return new Object[]{h2.getMetaData(), id, List.of(seq), List.of(table), true};
            default:
                throw new AssertionError("no contract test for the hook " + hook);
        }
    }

    /** What a contract script answers, by its hook's type, and that answer as the hook's. */
    private static String answer(Method hook) {
        String type = hook.getGenericReturnType().getTypeName();
        switch (type) {
            case "java.lang.String":
                return "\"${'from'} the script\"";
            case "java.util.Set<java.lang.String>":
                return "['from the script']";
            case "java.util.List<java.lang.String>":
                return "[\"${'from'} the script\"]";
            case "java.util.List<java.lang.String[]>":
                return "[['from', 'the script']]";
            case "java.util.List<java.util.Map<java.lang.String, java.lang.String>>":
                return "[[SEQUENCE_NAME: 'from the script']]";
            case "comart.rudbman.bridge.meta.Vendor$RowFence":
                return "[before: { ['from the script'] }] as RowFence";
            case "void":
                return "a0[8] = 'from the script'";
            default:
                throw new AssertionError("no answer for a hook of type " + type);
        }
    }

    /** @return {@code out} as it compares: arrays as lists, a row fence as its statements */
    private static Object compared(Object out) throws SQLException {
        if (out instanceof Vendor.RowFence) {
            return compared(((Vendor.RowFence) out).before());
        }
        if (out instanceof Object[]) {
            List<Object> items = new ArrayList<>();
            for (Object o : (Object[]) out) {
                items.add(compared(o));
            }
            return items;
        }
        if (out instanceof List) {
            List<Object> items = new ArrayList<>();
            for (Object o : (List<?>) out) {
                items.add(compared(o));
            }
            return items;
        }
        return out;
    }

    private static Object expected(Method hook) {
        switch (hook.getGenericReturnType().getTypeName()) {
            case "java.util.Set<java.lang.String>":
                return Set.of("from the script");
            case "java.util.List<java.lang.String>":
            case "comart.rudbman.bridge.meta.Vendor$RowFence":
                return List.of("from the script");
            case "java.util.List<java.lang.String[]>":
                return List.of(List.of("from", "the script"));
            case "java.util.List<java.util.Map<java.lang.String, java.lang.String>>":
                return List.of(Map.of("SEQUENCE_NAME", "from the script"));
            default:
                return "from the script";
        }
    }

    private static Object invoke(Vendor v, Method hook, Object[] args) throws Throwable {
        try {
            return hook.invoke(v, args);
        } catch (InvocationTargetException e) {
            throw e.getCause();
        }
    }

    @Test
    void everyPublishedHookIsCalledWithItsArgumentsAndAnsweredFrom() throws Throwable {
        try (Statement st = h2.createStatement(); ResultSet rs = st.executeQuery("SELECT 'x'")) {
            assertTrue(rs.next());
            for (Map.Entry<String, Method> e : ProfileScript.hookMethods().entrySet()) {
                String hook = e.getKey();
                Method m = e.getValue();
                String body = "comart.rudbman.bridge.meta.ProfileVendorTest.called('" + hook
                        + "', [" + params(m) + "] as Object[])\n" + answer(m);
                Vendor v = scripted("h2", "{}", "answers.groovy", define(hook, body));
                Object[] args = arguments(hook, rs);
                CALLED.remove(hook);
                Object out = invoke(v, m, args);
                assertArrayEquals(args, CALLED.get(hook), hook + " is called as documented");
                if (m.getReturnType() == void.class) {
                    assertEquals("from the script", ((String[]) args[0])[8], hook);
                } else {
                    assertEquals(expected(m), compared(out), hook + "'s answer is the script's");
                    assertTrue(m.getReturnType().isInstance(out), hook + " answers its type");
                }
            }
        }
    }

    private static String params(Method m) {
        StringJoiner names = new StringJoiner(", ");
        for (int i = 0; i < m.getParameterCount(); i++) {
            names.add("a" + i);
        }
        return names.toString();
    }

    @Test
    void everyPublishedHookThatThrowsFailsWithTheScriptsLine() throws Throwable {
        try (Statement st = h2.createStatement(); ResultSet rs = st.executeQuery("SELECT 'x'")) {
            assertTrue(rs.next());
            for (Map.Entry<String, Method> e : ProfileScript.hookMethods().entrySet()) {
                String hook = e.getKey();
                // The throw is the method's second line.
                Vendor v = scripted("h2", "{}", "two-words.groovy",
                        define(hook, "throw new IllegalStateException('broken " + hook + "')"));
                ScriptFailure failure = assertThrows(ScriptFailure.class,
                        () -> invoke(v, e.getValue(), arguments(hook, rs)), hook);
                assertTrue(failure.getMessage().contains("/profiles/two-words.groovy:2: " + hook
                        + " threw java.lang.IllegalStateException: broken " + hook),
                        failure.getMessage());
                assertEquals("script", failure.kind());
            }
        }
    }

    @Test
    void theScriptComesFirstAndItsBaseIsTheProfilesData() {
        Vendor v = scripted("h2", "{\"hex_literal\": \"0x{hex}\", \"identity_clause\": \"AUTO\"}",
                "layered.groovy", "String hexLiteral(String hex) { base.hexLiteral(hex) + ' -- ' + hex }");
        assertEquals("0x00FF -- 00FF", v.hexLiteral("00FF"));
        // What the script does not define is the data's, then H2's.
        assertEquals("AUTO", v.identityClause());
        assertEquals(Dialect.H2.vendor().textColumn("\"c\""), v.textColumn("\"c\""));
    }

    @Test
    void theImportsAndTheRecordsAreThereForAScript() throws SQLException {
        Vendor v = scripted("generic", "{}", "imports.groovy", ""
                + "String hexLiteral(String hex) { Ident.literal(hex) + ' ' + Types.VARCHAR }\n"
                + "List<String> counters(DatabaseMetaData dbm, Ident id, List<Sequence> seqs,\n"
                + "        List<Table> tables, boolean data) {\n"
                + "    seqs.collect { it.name() } + tables.collect { it.name() }\n"
                + "}\n");
        assertEquals("'00' " + Types.VARCHAR, v.hexLiteral("00"));
        assertEquals(List.of("s1", "t"), v.counters(h2.getMetaData(), id,
                List.of(new Vendor.Sequence(null, null, "s1", null, null, null)),
                List.of(new Vendor.Table(null, null, "t")), true));
    }

    @Test
    void aScriptThatDoesNotCompileFailsTheOpenWithItsLine() {
        ScriptFailure failure = assertThrows(ScriptFailure.class, () -> scripted("h2", "{}",
                "broken.groovy", "String hexLiteral(String hex) {\n    return 'x' +\n}\n"));
        assertTrue(failure.getMessage().startsWith("dialect profile scripted: /profiles/broken.groovy:"),
                failure.getMessage());
        assertTrue(failure.getMessage().matches("(?s).*broken\\.groovy:3(:\\d+)?: .*"),
                failure.getMessage());
    }

    @Test
    void aHookOfTheWrongShapeFailsTheOpen() {
        String arity = assertThrows(ScriptFailure.class, () -> scripted("h2", "{}", "arity.groovy",
                "String hexLiteral(String a, String b) { a }")).getMessage();
        assertTrue(arity.contains("hexLiteral takes String hexLiteral(String) in API 1"), arity);
        String type = assertThrows(ScriptFailure.class, () -> scripted("h2", "{}", "type.groovy",
                "String hexLiteral(int hex) { '' }")).getMessage();
        assertTrue(type.contains("takes a int as argument 1"), type);
        String nothing = assertThrows(ScriptFailure.class, () -> scripted("h2", "{}",
                "void.groovy", "void textColumn(String quoted) { }")).getMessage();
        assertTrue(nothing.contains("textColumn answers nothing"), nothing);
        String file = assertThrows(ScriptFailure.class, () -> scripted("h2", "{}", "class.groovy",
                "class Hooks { String hexLiteral(String hex) { hex } }")).getMessage();
        assertTrue(file.contains("defines a class, not a script"), file);
    }

    @Test
    void anAnswerOfTheWrongTypeFailsTheRequest() {
        Vendor v = scripted("h2", "{}", "answer.groovy",
                "def textColumns(Connection conn, String schema, String table) { 42 }");
        String message = assertThrows(ScriptFailure.class,
                () -> v.textColumns(h2, "app", "t")).getMessage();
        assertTrue(message.contains("textColumns answered a java.lang.Integer where API 1 wants"
                + " Set<String>"), message);
    }

    @Test
    void aRowFencesStatementsFailWithTheScriptsLine() throws SQLException {
        Vendor v = scripted("h2", "{}", "fence.groovy", ""
                + "RowFence rowFence(Connection c, Ident id, String s, String t, String q,\n"
                + "        List<String> identities) {\n"
                + "    [before: { [\"SET IDENTITY_INSERT ${q} ON;\"] },\n"
                + "     after: { throw new IllegalStateException('no after') }] as RowFence\n"
                + "}\n");
        Vendor.RowFence fence = v.rowFence(h2, id, "app", "t", "dbo.t", List.of("ID"));
        assertEquals(List.of("SET IDENTITY_INSERT dbo.t ON;"), fence.before());
        String message = assertThrows(ScriptFailure.class, fence::after).getMessage();
        assertTrue(message.contains("/profiles/fence.groovy:4: rowFence's after threw"), message);
    }

    @Test
    void aScriptIsCompiledOncePerProcess() {
        JsonObject script = new JsonObject();
        script.addProperty("path", "/profiles/once.groovy");
        script.addProperty("text", "String hexLiteral(String hex) { hex }");
        ProfileScript first = ProfileScript.load("a", script, groovy());
        ProfileScript second = ProfileScript.load("b", script, groovy());
        assertSame(first.type(), second.type());
        assertEquals(Set.of("hexLiteral"), first.defined());
        script.addProperty("text", "String hexLiteral(String hex) { hex + hex }");
        assertFalse(first.type() == ProfileScript.load("a", script, groovy()).type(),
                "a changed script is compiled again");
    }

    @Test
    void groovyIsNotOnTheBridgesClassPathAndAScriptNeedsItsRuntime() {
        assertThrows(ClassNotFoundException.class, () -> Class.forName("groovy.lang.Script"));
        ScriptFailure failure = assertThrows(ScriptFailure.class, () -> Profiles.dialect(
                profile("h2", "{}", "alone.groovy", "String hexLiteral(String hex) { hex }"),
                List.of()));
        assertTrue(failure.getMessage().contains("opened without it"), failure.getMessage());
    }

    @Test
    void aHookThatThrowsInsideAnOptionalQueryIsNotSwallowed() {
        Dialect dialect = Profiles.dialect(profile("h2", "{\"comments\": {\"hides_comments\": true}}",
                "comments.groovy", "List<String[]> tableComments(Connection c, Map<String, String> req) {\n"
                        + "    throw new java.sql.SQLException('no comments here')\n}\n"), groovy());
        Dialect.attach(h2, dialect);
        try {
            JsonArray items = JsonParser.parseString("[{\"name\": \"T\", \"remarks\": null}]")
                    .getAsJsonArray();
            JsonObject req = JsonParser.parseString("{\"schema\": \"PUBLIC\"}").getAsJsonObject();
            String message = assertThrows(ScriptFailure.class,
                    () -> Comments.tables(h2, h2.getMetaData(), req, items, null)).getMessage();
            assertTrue(message.contains("comments.groovy:2: tableComments threw"
                    + " java.sql.SQLException: no comments here"), message);
        } finally {
            Dialect.detach(h2);
        }
    }
}
