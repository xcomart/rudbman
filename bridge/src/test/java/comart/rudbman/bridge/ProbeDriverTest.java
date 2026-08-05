package comart.rudbman.bridge;

import com.google.gson.JsonArray;
import com.google.gson.JsonElement;
import com.google.gson.JsonObject;
import comart.rudbman.bridge.support.H2;
import comart.rudbman.bridge.support.Resp;
import org.junit.jupiter.api.Test;

import java.io.File;
import java.net.URISyntaxException;
import java.net.URL;
import java.util.ArrayList;
import java.util.List;

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertNotNull;
import static org.junit.jupiter.api.Assertions.assertTrue;

/** PROBE_DRIVER against the real H2 jar. */
class ProbeDriverTest {

    /** @return the jar the H2 driver was loaded from. */
    private static File h2Jar() throws ClassNotFoundException, URISyntaxException {
        Class<?> driver = Class.forName("org.h2.Driver");
        URL loc = driver.getProtectionDomain().getCodeSource().getLocation();
        assertNotNull(loc, "H2 must be on the test classpath as a jar");
        File f = new File(loc.toURI());
        assertTrue(f.isFile() && f.getName().endsWith(".jar"),
                "expected a jar file, got " + f);
        return f;
    }

    private static List<String> strings(JsonArray a) {
        List<String> out = new ArrayList<>();
        for (JsonElement e : a) {
            out.add(e.getAsString());
        }
        return out;
    }

    @Test
    void findsH2Driver() throws Exception {
        JsonObject req = new JsonObject();
        JsonArray jars = new JsonArray();
        jars.add(h2Jar().getAbsolutePath());
        req.add("jars", jars);

        JsonObject resp = H2.call(Ops.PROBE_DRIVER, 0, 0, req).json();
        List<String> classes = strings(resp.get("classes").getAsJsonArray());
        assertTrue(classes.contains("org.h2.Driver"), classes.toString());
        // The service declaration is the authoritative one when it is present.
        assertTrue(strings(resp.get("services").getAsJsonArray()).contains("org.h2.Driver"));
    }

    @Test
    void missingJarIsADriverError() {
        JsonObject req = new JsonObject();
        JsonArray jars = new JsonArray();
        jars.add("/nonexistent/rudbman/nope.jar");
        req.add("jars", jars);
        assertEquals("driver",
                H2.call(Ops.PROBE_DRIVER, 0, 0, req).error().get("kind").getAsString());
    }

    @Test
    void probeWithoutJarsIsAProtocolError() {
        assertEquals("protocol",
                Resp.of(Bridge.call(Ops.PROBE_DRIVER, 0, 0, null)).error()
                        .get("kind").getAsString());
    }

    @Test
    void aProbedDriverCanThenBeUsedFromItsJar() throws Exception {
        // The end-to-end shape of driver registration: probe a jar, then open a
        // session with the class it reported, loaded from that same jar through
        // a child loader rather than from the bridge classpath.
        JsonObject req = new JsonObject();
        JsonArray jars = new JsonArray();
        jars.add(h2Jar().getAbsolutePath());
        req.add("jars", jars);
        req.addProperty("url", H2.freshUrl());
        req.addProperty("driver_class", "org.h2.Driver");
        req.addProperty("username", "sa");
        req.addProperty("password", "");

        long s = H2.call(Ops.OPEN_SESSION, 0, 0, req).num("session");
        assertTrue(H2.call(Ops.PING, s, 0, null).json().get("ok").getAsBoolean());
        H2.close(s);
    }
}
