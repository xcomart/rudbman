package comart.rudbman.bridge;

import com.google.gson.JsonArray;
import com.google.gson.JsonObject;

import java.io.File;
import java.io.FileInputStream;
import java.io.IOException;
import java.net.URL;
import java.net.URLClassLoader;
import java.sql.Driver;
import java.util.ArrayList;
import java.util.LinkedHashSet;
import java.util.List;
import java.util.Set;
import java.util.jar.JarEntry;
import java.util.jar.JarInputStream;

/**
 * {@code PROBE_DRIVER} (op {@code 0x50}): scans jars for
 * {@link java.sql.Driver} implementations so the driver manager UI can offer the
 * class name instead of making the user find it.
 *
 * <p>Derived from jdbgen's {@code comart.utils.ClassUtils} (MIT, Dennis Soungjin
 * Park).
 */
public final class DriverProbe {

    private DriverProbe() {
    }

    /**
     * Lists the driver classes found in a set of jars.
     *
     * @param req request body with a {@code jars[]} array of paths
     * @return {@code {classes: [...], services: [...]}} where {@code services}
     *         holds the classes declared through {@code META-INF/services}
     */
    public static JsonObject probe(JsonObject req) {
        List<String> jars = Json.strings(req, "jars");
        if (jars.isEmpty()) {
            throw new BridgeException("protocol", "probe_driver requires 'jars'");
        }
        List<URL> urls = new ArrayList<>();
        List<File> files = new ArrayList<>();
        for (String j : jars) {
            File f = new File(j);
            if (!f.exists()) {
                throw new BridgeException("driver", "driver jar not found: " + f.getAbsolutePath());
            }
            files.add(f);
            try {
                urls.add(f.toURI().toURL());
            } catch (IOException e) {
                throw new BridgeException("driver", "unusable driver jar path: " + j, e);
            }
        }

        Set<String> found = new LinkedHashSet<>();
        Set<String> services = new LinkedHashSet<>();
        // A throwaway loader, closed at the end: probing must not leave the jars
        // pinned in a cached loader, because the user may well be about to
        // replace the file they just probed.
        try (URLClassLoader probe = new URLClassLoader(
                urls.toArray(new URL[0]), DriverProbe.class.getClassLoader())) {
            for (File f : files) {
                scan(f, probe, found, services);
            }
        } catch (IOException e) {
            throw new BridgeException("io", "cannot read driver jar: " + e.getMessage(), e);
        }

        JsonObject out = new JsonObject();
        JsonArray classes = new JsonArray();
        found.forEach(classes::add);
        out.add("classes", classes);
        JsonArray svc = new JsonArray();
        services.forEach(svc::add);
        out.add("services", svc);
        return out;
    }

    private static void scan(File jar, ClassLoader loader, Set<String> found, Set<String> services)
            throws IOException {
        try (JarInputStream in = new JarInputStream(new FileInputStream(jar))) {
            JarEntry entry;
            while ((entry = in.getNextJarEntry()) != null) {
                String name = entry.getName();
                if ("META-INF/services/java.sql.Driver".equals(name)) {
                    readServiceFile(in, services);
                    continue;
                }
                if (!name.endsWith(".class") || name.contains("$")) {
                    continue;
                }
                String cls = name.substring(0, name.length() - 6).replace('/', '.');
                try {
                    // initialize=false: a driver's static initialiser can open
                    // sockets or load native libraries, and probing must not.
                    Class<?> c = Class.forName(cls, false, loader);
                    if (Driver.class.isAssignableFrom(c) && !c.isInterface()) {
                        found.add(cls);
                    }
                } catch (Throwable ignored) {
                    // Half the classes in a driver jar reference optional
                    // dependencies that are not present. Skipping them is the
                    // normal case, not an error.
                }
            }
        }
    }

    private static void readServiceFile(JarInputStream in, Set<String> services) throws IOException {
        StringBuilder sb = new StringBuilder();
        byte[] buf = new byte[512];
        int n;
        while ((n = in.read(buf)) > 0) {
            sb.append(new String(buf, 0, n, java.nio.charset.StandardCharsets.UTF_8));
        }
        for (String line : sb.toString().split("\\R")) {
            int hash = line.indexOf('#');
            if (hash >= 0) {
                line = line.substring(0, hash);
            }
            line = line.trim();
            if (!line.isEmpty()) {
                services.add(line);
            }
        }
    }
}
