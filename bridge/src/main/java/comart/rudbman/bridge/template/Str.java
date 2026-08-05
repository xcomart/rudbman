package comart.rudbman.bridge.template;

import java.util.ArrayList;
import java.util.Arrays;

/**
 * The string helpers the template engine needs, carried over from jdbgen's
 * {@code comart.utils.StrUtils} (MIT, Dennis Soungjin Park).
 *
 * <p>Only the methods {@link TemplateManager} actually calls are here. Dragging
 * the whole utility class across would have brought password encryption, file
 * helpers and a logging dependency into a jar whose whole design goal is to stay
 * small.
 *
 * <p>Every method reproduces jdbgen's behaviour exactly, quirks included,
 * because existing template assets depend on those quirks. {@link #trim(String)}
 * stripping a surrounding quote pair is the sharpest example: it is what makes
 * {@code inStr="\n,"} arrive at the renderer as a newline and a comma rather
 * than as a quoted string.
 */
public final class Str {

    /** The characters jdbgen treats as whitespace: space, tab, CR, LF. */
    private static final char[] SPACE_CHARS = " \t\r\n".toCharArray();

    private Str() {
    }

    /**
     * @param src a character, or -1 for end of input
     * @return whether it is one of the four whitespace characters
     */
    public static boolean isSpace(int src) {
        for (char c : SPACE_CHARS) {
            if (src == c) {
                return true;
            }
        }
        return false;
    }

    /**
     * @param seq the text, may be {@code null}
     * @return whether it is {@code null} or blank
     */
    public static boolean isEmpty(CharSequence seq) {
        return seq == null || seq.toString().trim().isEmpty();
    }

    /**
     * @param size the length
     * @param c    the fill character
     * @return a string of {@code size} copies of {@code c}
     */
    public static String space(int size, char c) {
        if (size <= 0) {
            return "";
        }
        char[] res = new char[size];
        Arrays.fill(res, c);
        return new String(res);
    }

    /**
     * Trims whitespace and, if what is left is wrapped in a matching pair of
     * quotes, removes those too.
     *
     * <p>The quote stripping is not incidental: the parser hands attribute
     * values through here, so {@code indent="-1"} and {@code indent=-1} mean the
     * same thing, and a quoted value can therefore carry commas and equals signs.
     *
     * @param input the text
     * @return the trimmed text, never {@code null}
     */
    public static String trim(String input) {
        int st = 0;
        int ed = input.length();
        while (st < ed && contains(SPACE_CHARS, input.charAt(st))) {
            st++;
        }
        if (st >= ed) {
            return "";
        }
        while (st < ed && contains(SPACE_CHARS, input.charAt(ed - 1))) {
            ed--;
        }
        String res = input.substring(st, ed);
        // A lone quote character is not an enclosing pair.
        if (res.length() > 1
                && contains(new char[]{'"', '\''}, res.charAt(0))
                && res.charAt(0) == res.charAt(res.length() - 1)) {
            res = res.substring(1, res.length() - 1);
        }
        return res;
    }

    /**
     * Splits on a literal delimiter, not a regular expression.
     *
     * @param src   the text, may be {@code null}
     * @param delim the literal delimiter
     * @param trim  whether to trim each part
     * @return the parts, or {@code null} when either argument was {@code null}
     */
    public static String[] split(String src, String delim, boolean trim) {
        if (src == null || delim == null) {
            return null;
        }
        ArrayList<String> res = new ArrayList<>();
        // A trailing delimiter is appended so the last part needs no special case.
        src = src + delim;
        int idx;
        int prevIdx = 0;
        int delimLen = delim.length();
        while ((idx = src.indexOf(delim, prevIdx)) > -1) {
            String item = src.substring(prevIdx, idx);
            res.add(trim ? item.trim() : item);
            prevIdx = idx + delimLen;
        }
        return res.toArray(new String[0]);
    }

    /**
     * Replaces every occurrence of a literal string.
     *
     * @param src  the text, may be {@code null}
     * @param find the literal to find, may be {@code null}
     * @param rep  the replacement
     * @return the result, empty when {@code src} or {@code find} was {@code null}
     */
    public static String replace(String src, String find, String rep) {
        StringBuilder res = new StringBuilder();
        if (src != null && find != null && !find.isEmpty()) {
            int idx;
            int prevIdx = 0;
            int findLen = find.length();
            while ((idx = src.indexOf(find, prevIdx)) > -1) {
                res.append(src, prevIdx, idx).append(rep);
                prevIdx = idx + findLen;
            }
            res.append(src.substring(prevIdx));
        }
        return res.toString();
    }

    /**
     * @param arr the array, may be {@code null}
     * @param val the value, may be {@code null}
     * @return whether {@code arr} holds something equal to {@code val}
     */
    public static boolean contains(Object[] arr, Object val) {
        if (arr == null || val == null) {
            return false;
        }
        for (Object c : arr) {
            if (val.equals(c)) {
                return true;
            }
        }
        return false;
    }

    /**
     * @param charArr the array
     * @param c       the character
     * @return whether {@code charArr} holds {@code c}
     */
    public static boolean contains(char[] charArr, char c) {
        for (char ca : charArr) {
            if (ca == c) {
                return true;
            }
        }
        return false;
    }

    /**
     * @param str the text
     * @return whether it holds no ASCII lower-case letter
     */
    public static boolean isUpper(CharSequence str) {
        for (int i = 0; i < str.length(); i++) {
            char c = str.charAt(i);
            if (c >= 'a' && c <= 'z') {
                return false;
            }
        }
        return true;
    }

    /**
     * @param s an identifier
     * @return {@code s} in {@code camelCase}
     */
    public static String toCamelCase(String s) {
        if (s.contains("_") || s.contains("-")) {
            StringBuilder sb = new StringBuilder();
            boolean upper = false;
            for (int i = 0; i < s.length(); i++) {
                char c = s.charAt(i);
                if (c == '_' || c == '-') {
                    upper = true;
                } else if (upper) {
                    sb.append(s.substring(i, i + 1).toUpperCase());
                    upper = false;
                } else {
                    sb.append(s.substring(i, i + 1).toLowerCase());
                }
            }
            return sb.toString();
        } else if (isUpper(s)) {
            // An all-upper-case name carries no word boundaries at all, so the
            // only defensible reading is that the whole thing is one word.
            return s.toLowerCase();
        } else if (s.isEmpty()) {
            return s;
        } else {
            return s.substring(0, 1).toLowerCase() + s.substring(1);
        }
    }

    /**
     * @param s an identifier
     * @return {@code s} in {@code PascalCase}
     */
    public static String toPascalCase(String s) {
        String res = toCamelCase(s);
        return res.isEmpty() ? res : res.substring(0, 1).toUpperCase() + res.substring(1);
    }

    /**
     * @param s an identifier
     * @return {@code s} in {@code snake_case}
     */
    public static String toSnakeCase(String s) {
        if (s.contains("_") || s.contains("-")) {
            return replace(s, "-", "_").toLowerCase();
        }
        if (isUpper(s)) {
            s = s.toLowerCase();
        }
        StringBuilder sb = new StringBuilder();
        for (int i = 0; i < s.length(); i++) {
            char c = s.charAt(i);
            // No separator in front of a leading upper-case character.
            if (c >= 'A' && c <= 'Z' && sb.length() > 0) {
                sb.append('_');
            }
            sb.append(("" + c).toLowerCase());
        }
        return sb.toString();
    }

    /**
     * @param s an identifier
     * @return {@code s} in {@code SCREAMING_SNAKE_CASE}
     */
    public static String toScreamingSnakeCase(String s) {
        return toSnakeCase(s).toUpperCase();
    }

    /**
     * @param s an identifier
     * @return {@code s} in {@code skewer-case}
     */
    public static String toSkewerCase(String s) {
        return replace(toSnakeCase(s), "_", "-");
    }

    /**
     * Display width in the units jdbgen's padding was written against: the byte
     * length of the EUC-KR encoding, which is 1 for ASCII and 2 for the CJK
     * characters a Korean schema's comments are full of.
     *
     * <p>Reproducing this exactly matters, because it is what makes a template's
     * {@code padSize} line generated Java up in a fixed-width font. The charset
     * is looked up once and the calculation falls back to a width heuristic when
     * it is absent, which is what a jlink image without {@code jdk.charsets}
     * looks like.
     *
     * @param s the text
     * @return its display width
     */
    public static int displayWidth(String s) {
        if (EUC_KR != null) {
            return s.getBytes(EUC_KR).length;
        }
        int n = 0;
        for (int i = 0; i < s.length(); i++) {
            n += s.charAt(i) < 0x80 ? 1 : 2;
        }
        return n;
    }

    private static final java.nio.charset.Charset EUC_KR = lookupEucKr();

    private static java.nio.charset.Charset lookupEucKr() {
        try {
            return java.nio.charset.Charset.forName("EUC-KR");
        } catch (Exception e) {
            // A stripped runtime image without jdk.charsets. The heuristic is
            // close enough that no template breaks visibly.
            return null;
        }
    }
}
