package comart.rudbman.bridge.support;

import com.google.gson.JsonArray;
import com.google.gson.JsonObject;
import com.google.gson.JsonParser;

import java.nio.charset.StandardCharsets;
import java.util.Arrays;

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertTrue;

/** Decoded response envelope. */
public final class Resp {

    /** Whether the tag byte was 0. */
    public final boolean ok;
    /** The payload after the tag byte. */
    public final byte[] body;

    private Resp(boolean ok, byte[] body) {
        this.ok = ok;
        this.body = body;
    }

    /**
     * @param env a raw response envelope
     * @return the decoded envelope
     */
    public static Resp of(byte[] env) {
        assertTrue(env != null && env.length >= 1, "envelope must never be null or empty");
        return new Resp(env[0] == 0, Arrays.copyOfRange(env, 1, env.length));
    }

    /** @return the body parsed as a JSON object, failing the test if the call errored. */
    public JsonObject json() {
        assertOk();
        return JsonParser.parseString(new String(body, StandardCharsets.UTF_8)).getAsJsonObject();
    }

    /** @return the error body parsed as JSON, failing the test if the call succeeded. */
    public JsonObject error() {
        assertEquals(false, ok, "expected an ERROR envelope but got OK: " + text());
        return JsonParser.parseString(new String(body, StandardCharsets.UTF_8)).getAsJsonObject();
    }

    /** @return the body as text, for assertion messages. */
    public String text() {
        return new String(body, StandardCharsets.UTF_8);
    }

    /** Fails the test unless the call succeeded. */
    public void assertOk() {
        assertTrue(ok, "expected an OK envelope but got: " + text());
    }

    /** @return the body decoded as an RDB1 batch. */
    public Batch batch() {
        assertOk();
        return Batch.decode(body);
    }

    /**
     * @param member member name
     * @return that member of the JSON body as a string
     */
    public String str(String member) {
        return json().get(member).getAsString();
    }

    /**
     * @param member member name
     * @return that member of the JSON body as a long
     */
    public long num(String member) {
        return json().get(member).getAsLong();
    }

    /**
     * @param member member name
     * @return that member of the JSON body as an array
     */
    public JsonArray arr(String member) {
        return json().get(member).getAsJsonArray();
    }
}
