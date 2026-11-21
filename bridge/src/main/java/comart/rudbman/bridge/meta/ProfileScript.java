package comart.rudbman.bridge.meta;

import com.google.gson.JsonObject;

import comart.rudbman.bridge.BridgeException;
import comart.rudbman.bridge.Json;

import java.lang.reflect.InvocationTargetException;
import java.lang.reflect.Method;
import java.lang.reflect.Modifier;
import java.lang.reflect.ParameterizedType;
import java.lang.reflect.Type;
import java.nio.charset.StandardCharsets;
import java.security.MessageDigest;
import java.security.NoSuchAlgorithmException;
import java.sql.SQLException;
import java.util.ArrayList;
import java.util.Collection;
import java.util.Collections;
import java.util.HashMap;
import java.util.LinkedHashMap;
import java.util.LinkedHashSet;
import java.util.List;
import java.util.Map;
import java.util.Set;
import java.util.StringJoiner;
import java.util.TreeMap;
import java.util.concurrent.ConcurrentHashMap;

/**
 * A dialect profile's Groovy script: compiled once per process, and the hooks
 * of the published API it defines ({@code docs/dialect-profiles.md}, §5).
 *
 * <p>A hook is a method of {@link Vendor} that a script may replace by
 * defining a method of the same name: {@link #API_1} lists them, and each is
 * the public {@code Vendor} method of that name, its parameters and return
 * type its contract. {@link ProfileVendor} asks the script first, then the
 * profile's data, then the base; the script's {@code base} is the profile
 * without its script, so that a hook can add to an answer rather than
 * rewrite it.
 *
 * <p>Errors carry the script's file and line. One that does not compile, or
 * defines a hook with the wrong number or kinds of parameters, fails the
 * session's open; a hook that throws, or answers something that is not of its
 * hook's type, fails the request it was serving. Both are a
 * {@link ScriptFailure}.
 */
final class ProfileScript {

    /**
     * The hooks of API version 1. Their names and signatures do not change
     * within the version; each is public in {@link Vendor} and has a contract
     * test in {@code ProfileVendorTest}.
     */
    static final List<String> API_1 = List.of(
            // literals
            "literal", "stringLiteral", "hexLiteral",
            // catalog reads
            "routineRemarks", "sequences", "triggers", "triggerRow", "tableComments",
            "columnComments",
            // backup plan
            "extensionMembers", "materializedViews", "tablesNotWritten", "indexViews", "packages",
            "backupPreamble", "ownedSequences",
            // backup objects
            "sequenceValue", "resumeSequence", "sqlMode", "overloadTexts", "routineDrops",
            // backup rows
            "textColumns", "textColumn", "rowFence", "counters");

    /** The {@link Vendor} method of each hook. */
    private static final Map<String, Method> HOOKS = hooks();

    /** Scripts compiled so far, by runtime, file name and content hash. */
    private static final Map<String, Compiled> COMPILED = new ConcurrentHashMap<>();

    /**
     * A script's class, the name it was compiled under - what its stack
     * frames carry - and the hooks it defines.
     */
    private record Compiled(Class<?> type, String name, Map<String, Method> defined) {
    }

    private final String profile;
    private final String path;
    private final String file;
    private final GroovyRuntime runtime;
    private final Compiled compiled;

    private ProfileScript(String profile, String path, String file, GroovyRuntime runtime,
                          Compiled compiled) {
        this.profile = profile;
        this.path = path;
        this.file = file;
        this.runtime = runtime;
        this.compiled = compiled;
    }

    private static Map<String, Method> hooks() {
        Map<String, Method> out = new LinkedHashMap<>();
        for (String name : API_1) {
            Method found = null;
            for (Method m : Vendor.class.getDeclaredMethods()) {
                if (m.getName().equals(name)) {
                    if (found != null || !Modifier.isPublic(m.getModifiers())) {
                        throw new IllegalStateException("hook " + name
                                + " is not one public method of Vendor");
                    }
                    found = m;
                }
            }
            if (found == null) {
                throw new IllegalStateException("hook " + name + " is no method of Vendor");
            }
            out.put(name, found);
        }
        return Collections.unmodifiableMap(out);
    }

    /** @return the {@link Vendor} method of each hook of {@link #API_1} */
    static Map<String, Method> hookMethods() {
        return HOOKS;
    }

    /**
     * Compiles a profile's script, or takes it from the cache.
     *
     * @param profile the profile's id, for messages
     * @param script  the request's {@code script}: {@code path} and {@code text}
     * @param jars    the Groovy runtime's JARs
     * @return the script
     * @throws ScriptFailure if it does not compile, or defines a hook that is
     *                       not of its hook's shape
     */
    static ProfileScript load(String profile, JsonObject script, List<String> jars) {
        String path = Json.str(script, "path");
        String text = Json.str(script, "text");
        if (path == null || path.isEmpty() || text == null) {
            throw new BridgeException("protocol",
                    "dialect profile " + profile + ": its script needs a path and a text");
        }
        String file = new java.io.File(path).getName();
        GroovyRuntime runtime = GroovyRuntime.of(jars);
        String key = System.identityHashCode(runtime) + "\n" + file + "\n" + sha256(text);
        Compiled compiled = COMPILED.get(key);
        if (compiled == null) {
            compiled = compile(profile, path, file, text, runtime);
            Compiled raced = COMPILED.putIfAbsent(key, compiled);
            if (raced != null) {
                compiled = raced;
            }
        }
        return new ProfileScript(profile, path, file, runtime, compiled);
    }

    private static Compiled compile(String profile, String path, String file, String text,
                                    GroovyRuntime runtime) {
        // The class is named after the file, so the name is made one a class
        // can have: a profile's id may be a Java keyword, or hold a `-`.
        String stem = file.endsWith(".groovy") ? file.substring(0, file.length() - 7) : file;
        String name = "Profile_" + stem.replaceAll("[^A-Za-z0-9_]", "_") + ".groovy";
        Class<?> type;
        try {
            type = runtime.compile(name, text);
        } catch (Throwable t) {
            if (t instanceof VirtualMachineError) {
                throw (VirtualMachineError) t;
            }
            StringJoiner errors = new StringJoiner("; ");
            for (GroovyRuntime.CompileError e : GroovyRuntime.errors(t)) {
                String where = e.line() > 0 ? path + ":" + e.line()
                        + (e.column() > 0 ? ":" + e.column() : "") : path;
                errors.add(where + ": " + e.message());
            }
            throw new ScriptFailure("dialect profile " + profile + ": " + errors, t);
        }
        if (!runtime.isScript(type)) {
            throw new ScriptFailure("dialect profile " + profile + ": " + path
                    + " defines a class, not a script: write its hooks as methods of the file",
                    null);
        }
        Map<String, Method> defined = new HashMap<>();
        Map<String, List<Method>> byName = new TreeMap<>();
        for (Method m : type.getDeclaredMethods()) {
            if (HOOKS.containsKey(m.getName()) && Modifier.isPublic(m.getModifiers())
                    && !m.isSynthetic() && !m.isBridge()) {
                byName.computeIfAbsent(m.getName(), k -> new ArrayList<>()).add(m);
            }
        }
        for (Map.Entry<String, List<Method>> e : byName.entrySet()) {
            Method hook = HOOKS.get(e.getKey());
            Method match = null;
            for (Method m : e.getValue()) {
                if (m.getParameterCount() == hook.getParameterCount()) {
                    match = m;
                }
            }
            if (match == null) {
                throw new ScriptFailure("dialect profile " + profile + ": " + path + ": "
                        + e.getKey() + " takes " + signature(hook)
                        + " in API 1; the script's takes "
                        + e.getValue().get(0).getParameterCount() + " argument(s)", null);
            }
            String wrong = mismatch(match, hook);
            if (wrong != null) {
                throw new ScriptFailure("dialect profile " + profile + ": " + path + ": "
                        + e.getKey() + " " + wrong + "; API 1's is " + signature(hook), null);
            }
            match.setAccessible(true);
            defined.put(e.getKey(), match);
        }
        return new Compiled(type, name, Collections.unmodifiableMap(defined));
    }

    /** @return a hook's signature as a message names it */
    private static String signature(Method hook) {
        StringJoiner params = new StringJoiner(", ", "(", ")");
        for (Class<?> p : hook.getParameterTypes()) {
            params.add(p.getSimpleName());
        }
        return hook.getReturnType().getSimpleName() + " " + hook.getName() + params;
    }

    /**
     * @return what is wrong with a script's method as the hook it is named
     *         for, or {@code null} where it takes what the hook is called
     *         with and answers something the hook can be
     */
    private static String mismatch(Method script, Method hook) {
        Class<?>[] declared = script.getParameterTypes();
        Class<?>[] passed = hook.getParameterTypes();
        for (int i = 0; i < passed.length; i++) {
            if (!accepts(declared[i], passed[i])) {
                return "takes a " + declared[i].getSimpleName() + " as argument " + (i + 1)
                        + " where the hook passes a " + passed[i].getSimpleName();
            }
        }
        Class<?> answer = script.getReturnType();
        Class<?> wanted = hook.getReturnType();
        if (wanted != void.class && answer == void.class) {
            return "answers nothing";
        }
        if (wanted != void.class && answer != Object.class && !answers(answer, wanted)) {
            return "answers a " + answer.getSimpleName();
        }
        return null;
    }

    private static boolean accepts(Class<?> declared, Class<?> passed) {
        if (declared.isPrimitive() || passed.isPrimitive()) {
            return box(declared).isAssignableFrom(box(passed));
        }
        return declared.isAssignableFrom(passed);
    }

    private static boolean answers(Class<?> answer, Class<?> wanted) {
        if (box(wanted).isAssignableFrom(box(answer))) {
            return true;
        }
        if (wanted == String.class) {
            return CharSequence.class.isAssignableFrom(answer);
        }
        if (Collection.class.isAssignableFrom(wanted) || wanted.isArray()) {
            return Collection.class.isAssignableFrom(answer) || answer.isArray();
        }
        return Map.class.isAssignableFrom(wanted) && Map.class.isAssignableFrom(answer);
    }

    private static Class<?> box(Class<?> type) {
        if (!type.isPrimitive()) {
            return type;
        }
        if (type == int.class) {
            return Integer.class;
        }
        if (type == boolean.class) {
            return Boolean.class;
        }
        if (type == long.class) {
            return Long.class;
        }
        if (type == double.class) {
            return Double.class;
        }
        if (type == float.class) {
            return Float.class;
        }
        if (type == short.class) {
            return Short.class;
        }
        if (type == byte.class) {
            return Byte.class;
        }
        if (type == char.class) {
            return Character.class;
        }
        return Void.class;
    }

    private static String sha256(String text) {
        try {
            byte[] digest = MessageDigest.getInstance("SHA-256")
                    .digest(text.getBytes(StandardCharsets.UTF_8));
            StringBuilder sb = new StringBuilder();
            for (byte b : digest) {
                sb.append(String.format("%02x", b));
            }
            return sb.toString();
        } catch (NoSuchAlgorithmException e) {
            throw new IllegalStateException("every Java runtime has SHA-256", e);
        }
    }

    /** @return the names of the hooks the script defines */
    Set<String> defined() {
        return compiled.defined.keySet();
    }

    /** @return the script's compiled class, which the cache shares */
    Class<?> type() {
        return compiled.type;
    }

    /**
     * The script's object for one session.
     *
     * @param base what {@code base} is to the script: the profile without it
     * @return the hooks, bound to that object
     * @throws ScriptFailure if the script cannot be made
     */
    Hooks bind(Vendor base) {
        Object instance;
        try {
            instance = runtime.instantiate(compiled.type, Map.of("base", base));
        } catch (Throwable t) {
            if (t instanceof VirtualMachineError) {
                throw (VirtualMachineError) t;
            }
            throw failure("its creation", t);
        }
        return new Hooks(this, instance);
    }

    /** @return the failure of a hook, with the script's line where it threw */
    private ScriptFailure failure(String what, Throwable t) {
        Throwable cause = unwrapped(t);
        if (cause instanceof ScriptFailure) {
            return (ScriptFailure) cause;
        }
        int line = lineOf(cause);
        String where = line > 0 ? path + ":" + line : path;
        return new ScriptFailure("dialect profile " + profile + ": " + where + ": " + what
                + " threw " + cause, cause);
    }

    /** @return {@code t} without the wrappers reflection and Groovy put around it */
    private static Throwable unwrapped(Throwable t) {
        Throwable cur = t;
        for (int guard = 0; guard < 16 && cur.getCause() != null; guard++) {
            boolean wrapper = cur instanceof InvocationTargetException
                    || cur.getClass().getName().equals(
                            "org.codehaus.groovy.runtime.InvokerInvocationException");
            if (!wrapper) {
                break;
            }
            cur = cur.getCause();
        }
        return cur;
    }

    /** @return the script's line nearest the failure, or {@code -1} */
    private int lineOf(Throwable t) {
        Throwable cur = t;
        for (int guard = 0; guard < 16 && cur != null; guard++) {
            for (StackTraceElement frame : cur.getStackTrace()) {
                if (compiled.name.equals(frame.getFileName()) && frame.getLineNumber() > 0) {
                    return frame.getLineNumber();
                }
            }
            cur = cur.getCause();
        }
        return -1;
    }

    /** A script's object for one session: the hooks it defines, ready to call. */
    static final class Hooks {

        /** A profile with no script. */
        static final Hooks NONE = new Hooks(null, null);

        private final ProfileScript script;
        private final Object instance;

        private Hooks(ProfileScript script, Object instance) {
            this.script = script;
            this.instance = instance;
        }

        /**
         * @param hook a hook of {@link #API_1}
         * @return whether the script defines it
         */
        boolean defines(String hook) {
            return script != null && script.compiled.defined.containsKey(hook);
        }

        /**
         * Calls a hook the script defines.
         *
         * @param hook a hook {@link #defines} says the script has
         * @param args its arguments, as {@link Vendor}'s method takes them
         * @param <T>  the hook's type
         * @return its answer, as the hook's type
         * @throws ScriptFailure if the hook throws or answers something else
         */
        @SuppressWarnings("unchecked")
        <T> T call(String hook, Object... args) {
            Method m = script.compiled.defined.get(hook);
            Object answer;
            try {
                answer = m.invoke(instance, args);
            } catch (InvocationTargetException e) {
                throw script.failure(hook, e.getCause());
            } catch (IllegalAccessException | IllegalArgumentException e) {
                throw script.failure(hook, e);
            }
            Method contract = HOOKS.get(hook);
            Object out = script.coerce(hook, answer, contract.getGenericReturnType());
            if (out instanceof Vendor.RowFence && out != Vendor.RowFence.NONE) {
                out = script.guarded(hook, (Vendor.RowFence) out);
            }
            return (T) out;
        }
    }

    /**
     * A hook's answer as its hook's type: a {@code GString} as a
     * {@code String}, any collection or array as the list or set the hook
     * answers, element by element.
     */
    private Object coerce(String hook, Object value, Type type) {
        if (value == null) {
            return null;
        }
        Class<?> raw = raw(type);
        if (raw == void.class || raw == Void.class) {
            return null;
        }
        if (raw == String.class) {
            if (value instanceof CharSequence) {
                return value.toString();
            }
            throw wrongAnswer(hook, value, type);
        }
        if (raw == String[].class) {
            List<Object> items = items(value);
            if (items == null) {
                throw wrongAnswer(hook, value, type);
            }
            String[] out = new String[items.size()];
            for (int i = 0; i < out.length; i++) {
                out[i] = (String) coerce(hook, items.get(i), String.class);
            }
            return out;
        }
        if (raw == List.class || raw == Set.class) {
            List<Object> items = items(value);
            if (items == null) {
                throw wrongAnswer(hook, value, type);
            }
            Type element = argument(type, 0);
            Collection<Object> out = raw == List.class ? new ArrayList<>() : new LinkedHashSet<>();
            for (Object item : items) {
                out.add(coerce(hook, item, element));
            }
            return out;
        }
        if (raw == Map.class) {
            if (!(value instanceof Map)) {
                throw wrongAnswer(hook, value, type);
            }
            Map<Object, Object> out = new LinkedHashMap<>();
            for (Map.Entry<?, ?> e : ((Map<?, ?>) value).entrySet()) {
                out.put(coerce(hook, e.getKey(), argument(type, 0)),
                        coerce(hook, e.getValue(), argument(type, 1)));
            }
            return out;
        }
        if (raw.isInstance(value)) {
            return value;
        }
        throw wrongAnswer(hook, value, type);
    }

    private static List<Object> items(Object value) {
        if (value instanceof Collection) {
            return new ArrayList<>((Collection<?>) value);
        }
        if (value.getClass().isArray() && !value.getClass().getComponentType().isPrimitive()) {
            List<Object> out = new ArrayList<>();
            Collections.addAll(out, (Object[]) value);
            return out;
        }
        return null;
    }

    private static Class<?> raw(Type type) {
        if (type instanceof Class) {
            return (Class<?>) type;
        }
        if (type instanceof ParameterizedType) {
            return (Class<?>) ((ParameterizedType) type).getRawType();
        }
        return Object.class;
    }

    private static Type argument(Type type, int at) {
        if (type instanceof ParameterizedType) {
            return ((ParameterizedType) type).getActualTypeArguments()[at];
        }
        return Object.class;
    }

    private ScriptFailure wrongAnswer(String hook, Object value, Type type) {
        return new ScriptFailure("dialect profile " + profile + ": " + path + ": " + hook
                + " answered a " + value.getClass().getName() + " where API 1 wants "
                + type.getTypeName().replace("java.lang.", "").replace("java.util.", "")
                .replace("comart.rudbman.bridge.meta.", ""), null);
    }

    /**
     * A script's row fence, whose statements are asked for later: what they
     * throw is the hook's failure, and what they answer a list of strings.
     */
    private Vendor.RowFence guarded(String hook, Vendor.RowFence fence) {
        Type statements = new ParameterizedType() {
            @Override
            public Type[] getActualTypeArguments() {
                return new Type[]{String.class};
            }

            @Override
            public Type getRawType() {
                return List.class;
            }

            @Override
            public Type getOwnerType() {
                return null;
            }
        };
        return new Vendor.RowFence() {
            @Override
            @SuppressWarnings("unchecked")
            public List<String> before() {
                try {
                    return (List<String>) coerce(hook, fence.before(), statements);
                } catch (ScriptFailure e) {
                    throw e;
                } catch (RuntimeException e) {
                    throw failure(hook + "'s before", e);
                }
            }

            @Override
            @SuppressWarnings("unchecked")
            public List<String> after() throws SQLException {
                try {
                    return (List<String>) coerce(hook, fence.after(), statements);
                } catch (ScriptFailure e) {
                    throw e;
                } catch (SQLException | RuntimeException e) {
                    throw failure(hook + "'s after", e);
                }
            }
        };
    }
}
