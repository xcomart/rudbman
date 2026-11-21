package comart.rudbman.bridge.meta;

import java.io.File;
import java.io.IOException;
import java.lang.reflect.Array;
import java.lang.reflect.InvocationTargetException;
import java.net.MalformedURLException;
import java.net.URL;
import java.net.URLClassLoader;
import java.util.ArrayList;
import java.util.HashMap;
import java.util.List;
import java.util.Map;

/**
 * The Groovy runtime a dialect profile's script runs on
 * ({@code docs/dialect-profiles.md}, §5.3).
 *
 * <p>Not bundled: the application downloads {@code org.apache.groovy:groovy}
 * the first time a profile with a script is used, and sends its JARs with
 * {@code OPEN_SESSION}. They are loaded once per process on a class loader of
 * their own, whose parent is the bridge's: a script sees {@link Vendor},
 * {@link Ident}, JDBC and Groovy, and nothing of a driver's loader. The bridge
 * is compiled without Groovy, so nothing here names a Groovy type; everything
 * is reached by reflection.
 *
 * <p>A loader is kept for the life of the process, as the classes compiled on
 * it are: a script is compiled once and its class is cached.
 */
final class GroovyRuntime {

    /**
     * What Groovy's meta-class machinery reads every class with. It lives in
     * the {@code java.desktop} module, which a Java runtime cut down with
     * {@code jlink} may leave out.
     */
    private static final String BEANS = "java.beans.Introspector";

    /** The runtimes loaded so far, by their JARs' canonical paths. */
    private static final Map<List<String>, GroovyRuntime> RUNTIMES = new HashMap<>();

    private final ClassLoader loader;
    private final Class<?> script;
    private final Class<?> binding;

    private GroovyRuntime(ClassLoader loader) throws ReflectiveOperationException {
        this.loader = loader;
        this.script = loader.loadClass("groovy.lang.Script");
        this.binding = loader.loadClass("groovy.lang.Binding");
    }

    /**
     * The runtime of a set of JARs, loaded the first time it is asked for.
     *
     * <p>No JARs is the bridge's own class path, for a bridge that has Groovy
     * on it; one that has not fails, as a missing JAR does.
     *
     * @param jars the runtime's JARs, in class-path order
     * @return the runtime
     * @throws ScriptFailure if a JAR is missing, the JARs hold no Groovy, or
     *                       the Java runtime cannot run it
     */
    static GroovyRuntime of(List<String> jars) {
        try {
            Class.forName(BEANS, false, ClassLoader.getPlatformClassLoader());
        } catch (ClassNotFoundException | LinkageError e) {
            throw new ScriptFailure("the Groovy runtime needs the Java module java.desktop"
                    + " (java.beans), which this Java runtime does not have", e);
        }
        List<String> key = new ArrayList<>();
        List<URL> urls = new ArrayList<>();
        for (String jar : jars == null ? List.<String>of() : jars) {
            File f = new File(jar);
            if (!f.isFile()) {
                throw new ScriptFailure("the Groovy runtime is not at " + f.getAbsolutePath(),
                        null);
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
                throw new ScriptFailure("unusable Groovy runtime path: " + abs.getPath(), e);
            }
        }
        synchronized (RUNTIMES) {
            GroovyRuntime found = RUNTIMES.get(key);
            if (found != null) {
                return found;
            }
            ClassLoader parent = GroovyRuntime.class.getClassLoader();
            ClassLoader loader = urls.isEmpty() ? parent
                    : new URLClassLoader(urls.toArray(new URL[0]), parent);
            try {
                found = new GroovyRuntime(loader);
            } catch (ReflectiveOperationException | LinkageError e) {
                throw new ScriptFailure(urls.isEmpty()
                        ? "a dialect profile's script needs the Groovy runtime, and the session"
                                + " was opened without it"
                        : "no Groovy runtime in " + String.join(", ", key) + ": " + e, e);
            }
            RUNTIMES.put(key, found);
            return found;
        }
    }

    /**
     * Compiles a script, with {@code java.sql.*}, {@link Ident}, {@link Vendor}
     * and its public records imported.
     *
     * @param fileName the name its classes and stack frames carry
     * @param text     the source
     * @return the script's class
     * @throws Throwable what Groovy threw: a compilation failure, which
     *                   {@link #errors} reads
     */
    Class<?> compile(String fileName, String text) throws Throwable {
        try {
            Class<?> configType = loader.loadClass(
                    "org.codehaus.groovy.control.CompilerConfiguration");
            Object config = configType.getConstructor().newInstance();
            configType.getMethod("setSourceEncoding", String.class).invoke(config, "UTF-8");

            Class<?> importsType = loader.loadClass(
                    "org.codehaus.groovy.control.customizers.ImportCustomizer");
            Object imports = importsType.getConstructor().newInstance();
            importsType.getMethod("addStarImports", String[].class)
                    .invoke(imports, (Object) new String[]{"java.sql"});
            importsType.getMethod("addImports", String[].class).invoke(imports, (Object)
                    new String[]{Ident.class.getName(), Vendor.class.getName()});
            for (Class<?> record : new Class<?>[]{
                Vendor.Sequence.class, Vendor.Table.class, Vendor.RowFence.class,
            }) {
                importsType.getMethod("addImport", String.class, String.class)
                        .invoke(imports, record.getSimpleName(), record.getName());
            }
            Class<?> customizer = loader.loadClass(
                    "org.codehaus.groovy.control.customizers.CompilationCustomizer");
            Object customizers = Array.newInstance(customizer, 1);
            Array.set(customizers, 0, imports);
            configType.getMethod("addCompilationCustomizers", customizers.getClass())
                    .invoke(config, customizers);

            // A class loader per script: two versions of one file are two
            // classes of the same name.
            Class<?> gclType = loader.loadClass("groovy.lang.GroovyClassLoader");
            Object gcl = gclType.getConstructor(ClassLoader.class, configType)
                    .newInstance(loader, config);
            return (Class<?>) gclType.getMethod("parseClass", String.class, String.class)
                    .invoke(gcl, text, fileName);
        } catch (InvocationTargetException e) {
            throw e.getCause();
        }
    }

    /** @return whether {@code type} is a script rather than a class a file defines */
    boolean isScript(Class<?> type) {
        return script.isAssignableFrom(type);
    }

    /**
     * Makes a script's object, its binding holding {@code variables}. The
     * script's own top-level statements are not run: its hooks are its
     * methods.
     *
     * @param type      a class {@link #isScript} says is a script
     * @param variables what the script sees as properties of its own
     * @return the object
     * @throws Throwable what the script's construction threw
     */
    Object instantiate(Class<?> type, Map<String, Object> variables) throws Throwable {
        try {
            Object vars = binding.getConstructor().newInstance();
            for (Map.Entry<String, Object> v : variables.entrySet()) {
                binding.getMethod("setVariable", String.class, Object.class)
                        .invoke(vars, v.getKey(), v.getValue());
            }
            return type.getConstructor(binding).newInstance(vars);
        } catch (InvocationTargetException e) {
            throw e.getCause();
        }
    }

    /** Where a compilation failed, and why, as Groovy says it. */
    record CompileError(int line, int column, String message) {
    }

    /**
     * Reads the errors out of what {@link #compile} threw.
     *
     * @param failure a {@code MultipleCompilationErrorsException}, or anything
     *                else Groovy threw while compiling
     * @return the errors, at least one
     */
    static List<CompileError> errors(Throwable failure) {
        List<CompileError> out = new ArrayList<>();
        try {
            Object collector = failure.getClass().getMethod("getErrorCollector").invoke(failure);
            List<?> messages = (List<?>) collector.getClass().getMethod("getErrors")
                    .invoke(collector);
            for (Object message : messages) {
                Object cause = message.getClass().getMethod("getCause").invoke(message);
                if (cause == null) {
                    continue;
                }
                int line = -1;
                int column = -1;
                String text;
                try {
                    line = (Integer) cause.getClass().getMethod("getLine").invoke(cause);
                    column = (Integer) cause.getClass().getMethod("getStartColumn").invoke(cause);
                    text = (String) cause.getClass().getMethod("getOriginalMessage")
                            .invoke(cause);
                } catch (ReflectiveOperationException | ClassCastException e) {
                    text = cause.toString();
                }
                out.add(new CompileError(line, column, text));
            }
        } catch (ReflectiveOperationException | ClassCastException e) {
            // Not a compilation failure Groovy collected: its message is all.
        }
        if (out.isEmpty()) {
            String message = failure.getMessage();
            out.add(new CompileError(-1, -1, message == null ? failure.toString() : message));
        }
        return out;
    }
}
