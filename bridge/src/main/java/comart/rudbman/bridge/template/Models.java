package comart.rudbman.bridge.template;

import java.lang.reflect.Field;
import java.lang.reflect.Method;
import java.util.HashMap;
import java.util.Map;

/**
 * Property lookup on a template model, carried over from jdbgen's
 * {@code comart.utils.ObjUtils} (MIT, Dennis Soungjin Park).
 *
 * <p>A model is whatever the caller hands to
 * {@link TemplateManager#applyMapper(Object)}. jdbgen fed it beans and this
 * bridge feeds it maps, so both have to work.
 *
 * <p>{@link #getValue} understands a dotted path, but the template syntax does
 * not hand it one: a dot in <code>${a.b}</code> starts a processor chain, and
 * the key the engine looks up is always a single name. The path support is kept
 * because it is what jdbgen's helper did and a future directive may want it.
 *
 * <p>The bean path is deliberately forgiving about accessor shape - a public
 * field, {@code getX()}, {@code x()} and {@code isX()} are all accepted, walking
 * up the class hierarchy - because a template must not have to know how the
 * model class was written.
 */
public final class Models {

    private Models() {
    }

    /**
     * Reads a property, following {@code .} through nested models.
     *
     * @param obj      the model, may be {@code null}
     * @param property the property path
     * @return the value, or {@code null} when any step of the path is absent
     * @throws Exception whatever an accessor threw
     */
    public static Object getValue(Object obj, String property) throws Exception {
        int idx = property.indexOf('.');
        if (idx < 0) {
            return getOne(obj, property);
        }
        return getValue(getOne(obj, property.substring(0, idx)), property.substring(idx + 1));
    }

    private static Object getOne(Object obj, String property) throws Exception {
        if (obj == null) {
            return null;
        }
        if (obj instanceof Map) {
            return ((Map<?, ?>) obj).get(property);
        }
        Class<?> c = obj.getClass();
        try {
            Field f = c.getField(property);
            return f.get(obj);
        } catch (Throwable fieldNotVisible) {
            // No public field of that name; fall through to accessors. The catch
            // is on Throwable because a security manager answers with an Error.
            String capitalized = property.substring(0, 1).toUpperCase() + property.substring(1);
            String[] candidates = {"get" + capitalized, property, "is" + capitalized};
            Method m = null;
            while (c != null && m == null) {
                for (String candidate : candidates) {
                    try {
                        m = c.getMethod(candidate);
                        break;
                    } catch (Exception ignored) {
                        // Try the next accessor spelling.
                    }
                }
                if (m == null) {
                    c = c.getSuperclass();
                }
            }
            if (m == null) {
                return null;
            }
            return m.invoke(obj);
        }
    }

    /**
     * Boxed type to its primitive counterpart. A reflective lookup keyed on
     * {@code val.getClass()} finds the boxed type while a generated setter
     * usually declares the primitive one.
     */
    private static final Map<Class<?>, Class<?>> PRIMITIVES = new HashMap<>();

    static {
        PRIMITIVES.put(Integer.class, int.class);
        PRIMITIVES.put(Long.class, long.class);
        PRIMITIVES.put(Short.class, short.class);
        PRIMITIVES.put(Byte.class, byte.class);
        PRIMITIVES.put(Character.class, char.class);
        PRIMITIVES.put(Boolean.class, boolean.class);
        PRIMITIVES.put(Float.class, float.class);
        PRIMITIVES.put(Double.class, double.class);
    }

    /**
     * Writes a property, used by the {@code for} directive to publish the loop
     * counter as {@code no} on each item.
     *
     * <p>Silently does nothing when the model has no way to accept the value;
     * that is jdbgen's behaviour and a template that never reads {@code no}
     * must not fail because the model is immutable.
     *
     * <p>The {@link Map} case is an addition: jdbgen only ever looped over beans,
     * while this bridge builds its row models as maps, and without it
     * {@code ${no}} inside a {@code for} would render empty.
     *
     * @param obj      the model
     * @param property the property name
     * @param val      the value
     * @throws Exception whatever the setter threw
     */
    @SuppressWarnings("unchecked")
    public static void setValue(Object obj, String property, Object val) throws Exception {
        if (obj == null) {
            return;
        }
        if (obj instanceof Map) {
            ((Map<String, Object>) obj).put(property, val);
            return;
        }
        String setter = "set" + property.substring(0, 1).toUpperCase() + property.substring(1);
        Class<?> c = obj.getClass();
        Class<?>[] argTypes;
        if (val == null) {
            argTypes = new Class<?>[]{};
        } else if (PRIMITIVES.containsKey(val.getClass())) {
            argTypes = new Class<?>[]{val.getClass(), PRIMITIVES.get(val.getClass())};
        } else {
            argTypes = new Class<?>[]{val.getClass()};
        }
        String[] candidates = {setter, property};
        Method m = null;
        while (c != null && m == null) {
            outer:
            for (Class<?> argType : argTypes) {
                for (String candidate : candidates) {
                    try {
                        m = c.getMethod(candidate, argType);
                        break outer;
                    } catch (Exception ignored) {
                        // Try the next spelling or the primitive counterpart.
                    }
                }
            }
            if (m == null) {
                c = c.getSuperclass();
            }
        }
        if (m != null) {
            m.invoke(obj, val);
        }
    }
}
