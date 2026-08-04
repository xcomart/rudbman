package comart.rudbman.bridge;

import java.io.File;
import java.io.IOException;
import java.net.MalformedURLException;
import java.net.URL;
import java.net.URLClassLoader;
import java.util.ArrayList;
import java.util.HashMap;
import java.util.List;
import java.util.Map;
import java.util.logging.Level;
import java.util.logging.Logger;

/**
 * Cache of the child {@link URLClassLoader}s that isolate JDBC drivers from each
 * other and from the bridge.
 *
 * <p>Derived from the loader handling in jdbgen's {@code DBMeta} (MIT, Dennis
 * Soungjin Park), extended with reference counting.
 *
 * <p>Sessions that use the same jars share one loader. Building a fresh loader
 * per session re-runs the driver's static initialisers every time and leaks the
 * old loader together with everything it ever loaded. The loader is closed when
 * its last session goes away.
 */
public final class Loaders {

    private static final Logger LOG = Logger.getLogger(Loaders.class.getName());

    private static final Map<List<String>, Entry> CACHE = new HashMap<>();

    private Loaders() {
    }

    /** A cached loader together with its live-session count. */
    private static final class Entry {
        final URLClassLoader loader;
        int refs;

        Entry(URLClassLoader loader) {
            this.loader = loader;
        }
    }

    /**
     * A borrowed class loader. Must be released exactly once.
     */
    public static final class Lease {
        private final List<String> key;
        private final ClassLoader loader;

        Lease(List<String> key, ClassLoader loader) {
            this.key = key;
            this.loader = loader;
        }

        /** @return the class loader to load the driver class from. */
        public ClassLoader loader() {
            return loader;
        }

        /** Releases the lease, closing the loader when it was the last one. */
        public void release() {
            if (key == null) {
                return;
            }
            Loaders.release(key);
        }
    }

    /**
     * Borrows the loader for a set of driver jars.
     *
     * <p>An empty jar list yields the bridge's own class loader. That is how a
     * driver that already sits on the bridge classpath (the embedded H2 used by
     * the tests, or a driver bundled into the jlink image) is reached.
     *
     * @param jars driver jar paths, in classpath order
     * @return a lease that must be released when the session closes
     * @throws BridgeException when a jar is missing or its path is unusable
     */
    public static Lease acquire(List<String> jars) {
        if (jars == null || jars.isEmpty()) {
            return new Lease(null, Loaders.class.getClassLoader());
        }
        // The key preserves the caller's order: classpath order decides which
        // jar wins when two of them ship the same class, so two orderings are
        // genuinely two different class paths and must not share a loader.
        List<String> key = new ArrayList<>(jars.size());
        List<URL> urls = new ArrayList<>(jars.size());
        for (String j : jars) {
            File f = new File(j);
            if (!f.exists()) {
                throw new BridgeException("driver", "driver jar not found: " + f.getAbsolutePath());
            }
            File abs;
            try {
                abs = f.getCanonicalFile();
            } catch (IOException e) {
                abs = f.getAbsoluteFile();
            }
            key.add(abs.getPath());
            try {
                urls.add(abs.toURI().toURL());
            } catch (MalformedURLException e) {
                throw new BridgeException("driver", "unusable driver jar path: " + abs.getPath(), e);
            }
        }

        synchronized (CACHE) {
            Entry e = CACHE.get(key);
            if (e == null) {
                e = new Entry(new URLClassLoader(
                        urls.toArray(new URL[0]), Loaders.class.getClassLoader()));
                CACHE.put(key, e);
            }
            e.refs++;
            return new Lease(key, e.loader);
        }
    }

    private static void release(List<String> key) {
        URLClassLoader toClose = null;
        synchronized (CACHE) {
            Entry e = CACHE.get(key);
            if (e == null) {
                return;
            }
            if (--e.refs <= 0) {
                CACHE.remove(key);
                toClose = e.loader;
            }
        }
        if (toClose != null) {
            try {
                toClose.close();
            } catch (IOException e) {
                LOG.log(Level.WARNING, "cannot close jdbc driver class loader", e);
            }
        }
    }

    /** @return the number of cached loaders; for tests. */
    public static int cachedCount() {
        synchronized (CACHE) {
            return CACHE.size();
        }
    }
}
