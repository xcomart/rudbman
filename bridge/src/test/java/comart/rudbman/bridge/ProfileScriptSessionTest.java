package comart.rudbman.bridge;

import com.google.gson.JsonArray;
import com.google.gson.JsonObject;
import comart.rudbman.bridge.support.H2;
import comart.rudbman.bridge.support.Resp;
import org.junit.jupiter.api.Test;

import java.io.File;

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertNotNull;
import static org.junit.jupiter.api.Assertions.assertTrue;

/**
 * A dialect profile's script over a whole session, on H2: the script and the
 * Groovy runtime arrive with {@code OPEN_SESSION}, its hooks answer
 * {@code DESCRIBE}, and what goes wrong in it is reported with its line
 * ({@code docs/dialect-profiles.md}, §5).
 */
class ProfileScriptSessionTest {

    private static final String PATH = "/home/someone/.config/rudbman/dialects/scripted.groovy";

    /** @return the Groovy runtime's JARs, as the test task hands them over */
    private static JsonArray groovy() {
        String path = System.getProperty("rudbman.test.groovy");
        assertNotNull(path, "the test task sets rudbman.test.groovy");
        JsonArray jars = new JsonArray();
        for (String jar : path.split(File.pathSeparator)) {
            jars.add(jar);
        }
        return jars;
    }

    /** @return what OPEN_SESSION gets beyond the connection: the profile and the runtime */
    private static JsonObject withScript(String text) {
        JsonObject script = new JsonObject();
        script.addProperty("path", PATH);
        script.addProperty("text", text);
        JsonObject profile = new JsonObject();
        profile.addProperty("id", "scripted");
        profile.addProperty("base", "h2");
        profile.add("bridge", new JsonObject());
        profile.add("script", script);
        JsonObject extra = new JsonObject();
        extra.add("profile", profile);
        extra.add("groovy", groovy());
        return extra;
    }

    private static Resp describe(long session, String kind) {
        JsonObject req = new JsonObject();
        req.addProperty("kind", kind);
        req.addProperty("schema", "APP");
        return H2.call(Ops.DESCRIBE, session, 0, req);
    }

    @Test
    void aSessionAnswersFromItsScript() {
        String url = H2.freshUrl();
        long s = H2.open(url, withScript(""
                + "List<String[]> triggers(Connection c, String catalog, String schema,\n"
                + "        String name) {\n"
                + "    [[null, schema, 'AUDIT', 'T', 'AFTER', 'INSERT', 'ROW', 'Y',\n"
                + "      \"on ${c.metaData.databaseProductName}\"] as String[]]\n"
                + "}\n"
                + "def sequences(Connection c, Ident id, String catalog, String schema, String name) {\n"
                + "    [[SEQUENCE_SCHEMA: schema, SEQUENCE_NAME: 'ORDERS_SEQ', start_value: '100']]\n"
                + "}\n"));
        JsonArray triggers = describe(s, "triggers").json().get("items").getAsJsonArray();
        assertEquals(1, triggers.size(), triggers.toString());
        JsonObject trigger = triggers.get(0).getAsJsonObject();
        assertEquals("AUDIT", trigger.get("name").getAsString());
        assertEquals("APP", trigger.get("schema").getAsString());
        assertEquals("on H2", trigger.get("remarks").getAsString());
        assertTrue(trigger.get("enabled").getAsBoolean());

        JsonArray sequences = describe(s, "sequences").json().get("items").getAsJsonArray();
        assertEquals(1, sequences.size(), sequences.toString());
        assertEquals("ORDERS_SEQ", sequences.get(0).getAsJsonObject().get("name").getAsString());
        assertEquals("100", sequences.get(0).getAsJsonObject().get("start_value").getAsString());
        H2.close(s);

        // The same database without the profile is H2's, which has neither.
        long plain = H2.open(url);
        assertEquals(0, describe(plain, "triggers").json().get("items").getAsJsonArray().size());
        H2.close(plain);
    }

    @Test
    void aHookThatThrowsFailsItsRequestAndNotTheSession() {
        long s = H2.open(H2.freshUrl(), withScript(""
                + "List<String[]> triggers(Connection c, String catalog, String schema, String name) {\n"
                + "    throw new IllegalStateException('no trigger catalog today')\n"
                + "}\n"));
        JsonObject error = describe(s, "triggers").error();
        assertEquals("script", error.get("kind").getAsString(), error.toString());
        assertEquals("dialect profile scripted: " + PATH + ":2: triggers threw"
                + " java.lang.IllegalStateException: no trigger catalog today",
                error.get("message").getAsString());
        // Only that request failed.
        H2.call(Ops.PING, s, 0, null).assertOk();
        describe(s, "sequences").assertOk();
        H2.close(s);
    }

    @Test
    void aScriptThatDoesNotCompileFailsTheOpen() {
        JsonObject req = withScript("String hexLiteral(String hex) {\n    hex +\n}\n");
        req.addProperty("url", H2.freshUrl());
        req.addProperty("driver_class", H2.DRIVER);
        req.addProperty("username", "sa");
        req.addProperty("password", "");
        JsonObject error = H2.call(Ops.OPEN_SESSION, 0, 0, req).error();
        assertEquals("script", error.get("kind").getAsString(), error.toString());
        String message = error.get("message").getAsString();
        assertTrue(message.startsWith("dialect profile scripted: " + PATH + ":3"), message);
    }
}
