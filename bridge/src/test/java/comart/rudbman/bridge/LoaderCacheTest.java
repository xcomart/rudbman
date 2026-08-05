package comart.rudbman.bridge;

import com.google.gson.JsonArray;
import com.google.gson.JsonObject;
import comart.rudbman.bridge.support.H2;
import org.junit.jupiter.api.Test;

import java.io.File;
import java.net.URL;

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertNotNull;
import static org.junit.jupiter.api.Assertions.assertTrue;

/**
 * The class loader cache.
 *
 * <p>A fresh {@link java.net.URLClassLoader} per session re-runs the driver's
 * static initialisers every time and leaks the old loader with everything it
 * ever loaded, so sessions sharing a jar list must share a loader, and the
 * loader must go away with the last of them.
 */
class LoaderCacheTest {

    private static String h2Jar() throws Exception {
        URL loc = Class.forName("org.h2.Driver")
                .getProtectionDomain().getCodeSource().getLocation();
        assertNotNull(loc);
        return new File(loc.toURI()).getAbsolutePath();
    }

    private static long openWithJar(String jar) throws Exception {
        JsonObject req = new JsonObject();
        JsonArray jars = new JsonArray();
        jars.add(jar);
        req.add("jars", jars);
        req.addProperty("url", H2.freshUrl());
        req.addProperty("driver_class", H2.DRIVER);
        req.addProperty("username", "sa");
        req.addProperty("password", "");
        return H2.call(Ops.OPEN_SESSION, 0, 0, req).num("session");
    }

    @Test
    void sessionsShareALoaderAndReleaseItWithTheLastOne() throws Exception {
        // Relative to a baseline: other test classes share this JVM and the
        // cache is process-wide.
        int base = Loaders.cachedCount();

        String jar = h2Jar();
        long a = openWithJar(jar);
        assertEquals(base + 1, Loaders.cachedCount());

        long b = openWithJar(jar);
        assertEquals(base + 1, Loaders.cachedCount(), "the second session must reuse the loader");

        H2.close(a);
        assertEquals(base + 1, Loaders.cachedCount(), "a loader still in use must stay open");

        H2.close(b);
        assertEquals(base, Loaders.cachedCount(), "the last session must close the loader");
    }

    @Test
    void anEmptyJarListUsesTheBridgeLoaderAndCachesNothing() {
        int base = Loaders.cachedCount();
        long s = H2.open(H2.freshUrl());
        assertEquals(base, Loaders.cachedCount(),
                "the bridge's own loader is not a cache entry and is never closed");
        assertTrue(H2.call(Ops.PING, s, 0, null).json().get("ok").getAsBoolean());
        H2.close(s);
    }
}
