package comart.rudbman.bridge.meta;

import java.util.Map;

/**
 * A dialect profile's statement template: text with {@code {name}} holes, and
 * {@code {{} and {@code }}} for a brace of its own - the format
 * {@code rudbman-sql}'s templates have, so that one rule reads both halves of
 * a profile.
 *
 * <p>The holes a key may name are checked when the profile loads, by the
 * application; a hole this side does not know is left as it is written rather
 * than refused, since nothing here can say which file it came from.
 */
final class ProfileTemplate {

    private ProfileTemplate() {
    }

    /**
     * Fills {@code template}'s holes.
     *
     * @param template the text
     * @param values   each hole's value; a hole with a {@code null} value, or
     *                 none, has nothing to say
     * @return the statement, or {@code null} when a hole has no value - the
     *         profile's way of saying "no statement for this call", as a
     *         built-in answers nothing for a scope without a catalog
     */
    static String render(String template, Map<String, String> values) {
        StringBuilder out = new StringBuilder(template.length() + 16);
        int i = 0;
        int n = template.length();
        while (i < n) {
            char c = template.charAt(i);
            if (c == '{' && i + 1 < n && template.charAt(i + 1) == '{') {
                out.append('{');
                i += 2;
                continue;
            }
            if (c == '}' && i + 1 < n && template.charAt(i + 1) == '}') {
                out.append('}');
                i += 2;
                continue;
            }
            if (c == '{') {
                int close = template.indexOf('}', i + 1);
                if (close > i + 1) {
                    String hole = template.substring(i + 1, close);
                    if (values.containsKey(hole)) {
                        String value = values.get(hole);
                        if (value == null) {
                            return null;
                        }
                        out.append(value);
                        i = close + 1;
                        continue;
                    }
                }
            }
            out.append(c);
            i++;
        }
        return out.toString();
    }
}
