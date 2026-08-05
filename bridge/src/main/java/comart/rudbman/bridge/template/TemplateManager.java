package comart.rudbman.bridge.template;

import java.text.ParseException;
import java.text.SimpleDateFormat;
import java.util.ArrayList;
import java.util.Collections;
import java.util.Date;
import java.util.HashMap;
import java.util.HashSet;
import java.util.List;
import java.util.Map;
import java.util.Set;
import java.util.logging.Level;
import java.util.logging.Logger;
import java.util.regex.Pattern;

/**
 * The template engine, inherited from jdbgen's
 * {@code comart.tools.jdbgen.template.TemplateManager} (MIT, Dennis Soungjin
 * Park).
 *
 * <p>This class exists in Java rather than Rust for exactly one reason
 * (architecture.md 12.3): the template assets people already have must keep
 * rendering byte for byte. Every syntactic quirk below is therefore load
 * bearing, including the ones that look like accidents - the EUC-KR padding
 * width, {@code YYYY} reaching {@link SimpleDateFormat} as a week-year, the
 * quote pair that {@link Str#trim(String)} strips off attribute values. Do not
 * "fix" them without a migration story for the assets.
 *
 * <h2>Syntax</h2>
 *
 * <p>Everything outside <code>${...}</code> is copied through. Inside:
 *
 * <ul>
 *   <li><code>${name}</code> - shorthand for <code>${item:key=name}</code>,
 *       reads {@code name} off the model.</li>
 *   <li><code>${item:key=name, padSize=10, padDir=right, quote=", prepend=…,
 *       postpend=…}</code> - the same with formatting.</li>
 *   <li><code>${super:key=name}</code> - reads off the enclosing model instead,
 *       which inside a {@code for} is the object being looped over.</li>
 *   <li><code>${for:item=columns, inStr=",", indent=-1, skipList=A,B}</code> …
 *       <code>${endfor}</code> - repeats its body over a list member. Each item
 *       gets its 1-based position published as {@code no}.</li>
 *   <li><code>${if:key=x, equals=y}</code> … <code>${elif:…}</code> …
 *       <code>${else}</code> … <code>${endif}</code> - conditional. Comparisons:
 *       {@code equals}/{@code value}, {@code notEquals}, {@code contains},
 *       {@code notContains}, {@code startsWith}, {@code notStartsWith},
 *       {@code endsWith}, {@code notEndsWith}, {@code matches},
 *       {@code notMatches}. All are case-insensitive except the regex ones.</li>
 *   <li><code>${user}</code>, <code>${date:yyyy-MM-dd}</code>,
 *       <code>${author}</code> - the login user, the current date, and the
 *       {@code author} entry of the custom variables.</li>
 *   <li><code>${"text"}</code> or <code>${'text'}</code> - escapes literal text,
 *       which is how a template emits a <code>${</code> of its own.</li>
 * </ul>
 *
 * <p>A key may be followed by processors: <code>${name.camel}</code>,
 * <code>${name.suffix.pascal}</code>, <code>${x.replace("a","b")}</code>.
 * Available processors are {@code prefix}, {@code suffix}, {@code camel},
 * {@code pascal}, {@code snake}, {@code screaming}, {@code skewer}/{@code kebab},
 * {@code lower}, {@code upper}, {@code replace} and {@code abbr}.
 *
 * <h2>What changed from jdbgen</h2>
 *
 * <ul>
 *   <li>Lombok is gone; the one warning it logged goes to {@code java.util.logging}
 *       at {@code FINE}, matching the rest of the bridge.</li>
 *   <li>The abbreviation dictionary was a live read of jdbgen's configuration
 *       singleton. It is now static state set through
 *       {@link #configureAbbreviations}, empty by default, so the engine has no
 *       dependency on any configuration model.</li>
 *   <li>{@code ObjUtils}/{@code StrUtils} shrank to {@link Models} and
 *       {@link Str}; the only behavioural addition is that
 *       {@link Models#setValue} understands maps, so {@code ${no}} works inside
 *       a {@code for} over map-shaped models.</li>
 * </ul>
 *
 * <p>Instances are immutable after construction and rendering keeps no state, so
 * one parsed template can be applied to every row of an extract.
 */
public class TemplateManager {

    private static final Logger LOG = Logger.getLogger(TemplateManager.class.getName());

    private static final String USER_ID = System.getProperty("user.name");
    private static final String DEFAULT_DATE_FORMAT = "yyyy-MM-dd";

    private interface TemplateHandler {
        TemplateItem process(String extra, ParseContext ctx) throws ParseException;
    }

    private interface TemplateAppender {
        void append(StringBuilder sb, TemplateItem template, Object mapper, Object supr)
                throws Exception;
    }

    private interface ItemProcHandler {
        String process(String item, List<Object> params);
    }

    private interface IfCondHandler {
        boolean check(String key, String condVal, Object mapper, Map<String, String> customs)
                throws Exception;
    }

    private enum TemplateType {
        TEXT, ITEM, SUPER, FOR, IF, USER, DATE, AUTHOR
    }

    private static final class TemplateItem {
        final TemplateType type;
        final Object cont;

        TemplateItem(TemplateType type, Object cont) {
            this.type = type;
            this.cont = cont;
        }
    }

    /** Cursor over the template text, tracking the line number for error messages. */
    private static final class ParseContext {
        int curr;
        int len;
        int line;
        final String template;

        ParseContext(String template) {
            this.template = template;
            this.line = 0;
            this.curr = 0;
            this.len = template.length();
        }

        void updateLineCount(int end) {
            String text = template.substring(curr, end);
            line += text.split("\n").length - 1;
            curr = end;
        }

        int nextChar() {
            if (curr < len) {
                int res = template.charAt(curr++);
                if (res == '\n') {
                    line++;
                }
                return res;
            }
            return -1;
        }

        void skipSpace() {
            int c;
            while ((c = nextChar()) > -1) {
                if (!Str.isSpace(c)) {
                    curr--;
                    break;
                }
            }
        }

        int peek() {
            return curr < len ? template.charAt(curr) : -1;
        }

        /** @return the text just ahead of the cursor, to locate an error by eye */
        String near() {
            int length = 100;
            return curr + length < len
                    ? template.substring(curr, curr + length) + "..."
                    : template.substring(curr);
        }
    }

    // ----------------------------------------------------------------- parser

    /**
     * Advances to the next directive, pushing the text before it as a TEXT item.
     *
     * @return the directive body, or {@code null} at end of template
     */
    private static String next(ParseContext ctx, ArrayList<TemplateItem> items)
            throws ParseException {
        if (ctx.curr == ctx.len) {
            return null;
        }

        int sp = ctx.template.indexOf("${", ctx.curr);
        if (sp < 0) {
            sp = ctx.len;
        }

        if (sp > ctx.curr) {
            items.add(new TemplateItem(TemplateType.TEXT,
                    ctx.template.substring(ctx.curr, sp)));
        }

        if (sp + 1 < ctx.len) {
            sp += 2;    // skip "${"
        }

        ctx.updateLineCount(sp);
        if (sp == ctx.len) {
            return null;
        }

        ctx.skipSpace();
        int c = ctx.peek();
        StringBuilder sb = new StringBuilder();
        // A quoted directive is literal text, which is the only way a template
        // can emit a "${" of its own.
        if (c == '"' || c == '\'') {
            int openChar = ctx.nextChar();
            boolean isEscape = false;
            while ((c = ctx.nextChar()) > -1) {
                if (!isEscape) {
                    if (c == '\\') {
                        // The escape character itself is not part of the literal.
                        isEscape = true;
                        continue;
                    } else if (c == openChar) {
                        break;
                    }
                } else {
                    isEscape = false;
                }
                sb.append((char) c);
            }
            sp = ctx.curr;
        }
        int lst = ctx.template.indexOf("}", sp);
        if (lst < 0) {
            throw new ParseException("'}' not found, before: " + ctx.near(), ctx.line);
        }
        String res = sb.length() == 0 ? Str.trim(ctx.template.substring(sp, lst)) : sb.toString();
        ctx.updateLineCount(lst);
        ctx.nextChar(); // skip '}'
        if (sb.length() > 0) {
            items.add(new TemplateItem(TemplateType.TEXT, res));
            // That was an escape, not a directive, so keep looking.
            return next(ctx, items);
        }
        return res;
    }

    /**
     * Parses a directive's attribute list.
     *
     * <p>Quoting is tracked so that a value may hold the {@code ,} and {@code =}
     * that otherwise separate pairs, and backslash escapes are expanded, which is
     * how {@code inStr="\n,"} carries a real newline.
     */
    private static Map<String, Object> parseNVPairs(ParseContext ctx, String data)
            throws ParseException {
        int idx = 0;
        int openChar = -1;
        Map<String, Object> map = new HashMap<>();
        StringBuilder sb = new StringBuilder();
        String name = "";
        String value = "";
        while (idx < data.length()) {
            char c = data.charAt(idx);
            if (c == '\\') {
                idx++;
                if (idx >= data.length()) {
                    throw new ParseException("Dangling escape character at end of: " + data
                            + ". invalid syntax before: " + ctx.near(), idx);
                }
                c = data.charAt(idx);
                switch (c) {
                    case 'n': sb.append('\n'); break;
                    case 'r': sb.append('\r'); break;
                    case 't': sb.append('\t'); break;
                    default: sb.append(c);
                }
            } else {
                if (openChar < 0 && (c == '"' || c == '\'' || c == '(')) {
                    openChar = c == '(' ? ')' : c;
                    sb.append(c);
                } else if (c == openChar) {
                    sb.append(c);
                    openChar = -1;
                } else if (openChar < 0 && c == '=') {
                    if (!Str.isEmpty(name)) {
                        throw new ParseException("Name value pair not matched: " + data
                                + ". invalid syntax before: " + ctx.near(), idx);
                    }
                    name = Str.trim(sb.toString());
                    sb = new StringBuilder();
                } else if (openChar < 0 && c == ',') {
                    if (!Str.isEmpty(value)) {
                        throw new ParseException("Name value pair not matched: " + data
                                + ". invalid syntax before: " + ctx.near(), idx);
                    }
                    value = Str.trim(sb.toString());
                    if (Str.isEmpty(name)) {
                        throw new ParseException("Name value pair not matched: " + data
                                + ". invalid syntax before: " + ctx.near(), idx);
                    }
                    map.put(name.toLowerCase(), value);
                    name = "";
                    value = "";
                    sb = new StringBuilder();
                } else {
                    sb.append(c);
                }
            }
            idx++;
        }
        if (!Str.isEmpty(value)) {
            throw new ParseException("Name value pair not matched: " + data
                    + ". invalid syntax before: " + ctx.near(), idx);
        }
        value = Str.trim(sb.toString());
        if (!value.isEmpty()) {
            if (Str.isEmpty(name)) {
                throw new ParseException("Name value pair not matched: " + data
                        + ". invalid syntax before: " + ctx.near(), idx);
            }
            map.put(name.toLowerCase(), value);
        }
        return map;
    }

    private static TemplateItem parseOne(String itemString, ParseContext ctx)
            throws ParseException {
        if (Str.contains(new String[]{"user", "date", "author"}, itemString.toLowerCase())) {
            // These three take no attributes, so they arrive without a colon.
            itemString += ":";
        } else if (itemString.indexOf(':') < 0) {
            // The shorthand: ${name} is ${item:key=name}.
            itemString = "item:key=" + itemString;
        }
        int idx = itemString.indexOf(':');
        String type = Str.trim(itemString.substring(0, idx)).toLowerCase();
        String typeOptions = itemString.substring(idx + 1);
        TemplateHandler handler = HANDLERS.get(type);
        if (handler == null) {
            throw new ParseException("Unknown template: " + itemString + ", before: " + ctx.near(),
                    ctx.line);
        }
        return handler.process(typeOptions, ctx);
    }

    private static List<TemplateItem> parseTemplate(ParseContext ctx) throws ParseException {
        ArrayList<TemplateItem> res = new ArrayList<>();
        while (true) {
            String itemString = next(ctx, res);
            if (itemString == null) {
                break;
            }
            res.add(parseOne(itemString, ctx));
        }
        return res;
    }

    private static TemplateItem parseItem(String extra, ParseContext ctx) throws ParseException {
        return new TemplateItem(TemplateType.ITEM, parseNVPairs(ctx, extra));
    }

    private static TemplateItem parseSuper(String extra, ParseContext ctx) throws ParseException {
        return new TemplateItem(TemplateType.SUPER, parseNVPairs(ctx, extra));
    }

    private static void checkIfConditions(Map<String, Object> pairs, String extra, ParseContext ctx)
            throws ParseException {
        Set<String> available = new HashSet<>();
        available.add("key");
        available.add("item");
        available.addAll(IFCONDS.keySet());
        for (String key : pairs.keySet()) {
            if (!available.contains(key)) {
                throw new ParseException("Unknown if condition: " + extra + ", before: "
                        + ctx.near(), ctx.line);
            }
        }
    }

    /**
     * Parses an if/elif/else/endif chain.
     *
     * <p>The chain is flattened into nested two-branch ifs while parsing: each
     * {@code elif} becomes the {@code false} branch of the one before it, so the
     * renderer only ever has to know about one shape.
     */
    private static TemplateItem parseIf(String extra, ParseContext ctx) throws ParseException {
        Map<String, Object> pairs = parseNVPairs(ctx, extra);
        checkIfConditions(pairs, extra, ctx);
        TemplateItem res = new TemplateItem(TemplateType.IF, pairs);
        ArrayList<TemplateItem> items = new ArrayList<>();
        pairs.put("true", items);
        while (true) {
            String itemString = next(ctx, items);
            if (itemString == null) {
                throw new ParseException("if statements not closed, before: " + ctx.near(),
                        ctx.line);
            }
            if (itemString.startsWith("elif:")) {
                extra = Str.trim(itemString.substring(5));
                Map<String, Object> npairs = parseNVPairs(ctx, extra);
                checkIfConditions(npairs, extra, ctx);
                TemplateItem curr = new TemplateItem(TemplateType.IF, npairs);
                items = new ArrayList<>();
                npairs.put("true", items);
                pairs.put("false", curr);
                pairs = npairs;
            } else if ("else".equals(itemString)) {
                items = new ArrayList<>();
                pairs.put("false", items);
            } else if ("endif".equals(itemString)) {
                break;
            } else {
                items.add(parseOne(itemString, ctx));
            }
        }
        return res;
    }

    private static TemplateItem parseFor(String extra, ParseContext ctx) throws ParseException {
        Map<String, Object> pairs = parseNVPairs(ctx, extra);
        TemplateItem res = new TemplateItem(TemplateType.FOR, pairs);
        ArrayList<TemplateItem> items = new ArrayList<>();
        pairs.put("items", items);
        while (true) {
            String itemString = next(ctx, items);
            if (itemString == null) {
                throw new ParseException("for statements not closed. before: " + ctx.near(),
                        ctx.line);
            }
            if ("endfor".equals(itemString)) {
                break;
            }
            items.add(parseOne(itemString, ctx));
        }
        return res;
    }

    private static TemplateItem parseDate(String extra, ParseContext ctx) throws ParseException {
        if (!extra.contains("=")) {
            // ${date:yyyy-MM-dd} is shorthand for ${date:format=yyyy-MM-dd}.
            extra = "format=" + extra;
        }
        return new TemplateItem(TemplateType.DATE, parseNVPairs(ctx, extra));
    }

    private static TemplateItem parseUser(String extra, ParseContext ctx) throws ParseException {
        return new TemplateItem(TemplateType.USER, parseNVPairs(ctx, extra));
    }

    private static TemplateItem parseAuthor(String extra, ParseContext ctx) throws ParseException {
        return new TemplateItem(TemplateType.AUTHOR, parseNVPairs(ctx, extra));
    }

    // ------------------------------------------------------------- processors

    private static String procPrefix(String item, List<Object> params) {
        int idx = item.lastIndexOf("_");
        return idx > -1 ? item.substring(0, idx) : item;
    }

    private static String procSuffix(String item, List<Object> params) {
        int idx = item.indexOf("_");
        return idx > -1 ? item.substring(idx + 1) : item;
    }

    private static String procCamel(String item, List<Object> params) {
        return Str.toCamelCase(item);
    }

    private static String procPascal(String item, List<Object> params) {
        return Str.toPascalCase(item);
    }

    private static String procSnake(String item, List<Object> params) {
        return Str.toSnakeCase(item);
    }

    private static String procScreaming(String item, List<Object> params) {
        return Str.toScreamingSnakeCase(item);
    }

    private static String procSkewer(String item, List<Object> params) {
        return Str.toSkewerCase(item);
    }

    private static String procLower(String item, List<Object> params) {
        return item.toLowerCase();
    }

    private static String procUpper(String item, List<Object> params) {
        return item.toUpperCase();
    }

    private static String procReplace(String item, List<Object> params) {
        if (params.size() < 2) {
            throw new IllegalArgumentException(
                    "'replace' processor requires 2 arguments - replace(find, replacement), but got "
                            + params.size() + ": " + params);
        }
        return Str.replace(item, params.get(0).toString(), params.get(1).toString());
    }

    /**
     * Expands the abbreviation dictionary over an identifier: the whole name if
     * it is listed as one, otherwise word by word between {@code _} and
     * {@code -}.
     */
    private static String procAbbr(String item, List<Object> params) {
        Map<String, String> whole = abbrWholeNames;
        Map<String, String> words = abbrWords;
        if (whole.containsKey(item.toLowerCase())) {
            return whole.get(item.toLowerCase());
        }
        // A trailing separator is appended so the last word needs no special case.
        item = item + "_";
        StringBuilder res = new StringBuilder();
        StringBuilder buf = new StringBuilder();
        for (char c : item.toCharArray()) {
            if (c == '_' || c == '-') {
                String word = buf.toString();
                if (words.containsKey(word)) {
                    word = words.get(word);
                }
                res.append(word).append(c);
                buf = new StringBuilder();
            } else {
                buf.append(c);
            }
        }
        res.deleteCharAt(res.length() - 1);
        return res.toString();
    }

    private static final Map<String, TemplateHandler> HANDLERS = new HashMap<>();
    private static final Map<String, ItemProcHandler> PROCS = new HashMap<>();
    private static final Map<String, IfCondHandler> IFCONDS = new HashMap<>();

    static {
        HANDLERS.put("item", TemplateManager::parseItem);
        HANDLERS.put("super", TemplateManager::parseSuper);
        HANDLERS.put("if", TemplateManager::parseIf);
        HANDLERS.put("for", TemplateManager::parseFor);
        HANDLERS.put("date", TemplateManager::parseDate);
        HANDLERS.put("user", TemplateManager::parseUser);
        HANDLERS.put("author", TemplateManager::parseAuthor);

        PROCS.put("prefix", TemplateManager::procPrefix);
        PROCS.put("suffix", TemplateManager::procSuffix);
        PROCS.put("camel", TemplateManager::procCamel);
        PROCS.put("pascal", TemplateManager::procPascal);
        PROCS.put("snake", TemplateManager::procSnake);
        PROCS.put("screaming", TemplateManager::procScreaming);
        PROCS.put("skewer", TemplateManager::procSkewer);
        PROCS.put("kebab", TemplateManager::procSkewer);
        PROCS.put("lower", TemplateManager::procLower);
        PROCS.put("upper", TemplateManager::procUpper);
        PROCS.put("replace", TemplateManager::procReplace);
        PROCS.put("abbr", TemplateManager::procAbbr);

        IFCONDS.put("equals", TemplateManager::condEquals);
        IFCONDS.put("value", TemplateManager::condEquals);
        IFCONDS.put("notequals", TemplateManager::condNotEquals);
        IFCONDS.put("contains", TemplateManager::condContains);
        IFCONDS.put("notcontains", TemplateManager::condNotContains);
        IFCONDS.put("startswith", TemplateManager::condStartsWith);
        IFCONDS.put("notstartswith", TemplateManager::condNotStartsWith);
        IFCONDS.put("endswith", TemplateManager::condEndsWith);
        IFCONDS.put("notendswith", TemplateManager::condNotEndsWith);
        IFCONDS.put("matches", TemplateManager::condMatches);
        IFCONDS.put("notmatches", TemplateManager::condNotMatches);
    }

    // ---------------------------------------------------- abbreviation config

    private static volatile Map<String, String> abbrWords = Collections.emptyMap();
    private static volatile Map<String, String> abbrWholeNames = Collections.emptyMap();
    private static volatile boolean applyAbbrToName;

    /**
     * Installs the abbreviation dictionary used by the {@code abbr} processor.
     *
     * <p>In jdbgen this was read live out of the configuration singleton. Here it
     * is static state the host sets once, because the alternative was to make the
     * engine depend on a configuration model that lives on the Rust side.
     *
     * @param applyToName  whether {@code ${name}} implies {@code ${name.abbr}},
     *                     jdbgen's "apply abbreviations" switch
     * @param words        per-word replacements, keys lower case
     * @param wholeNames   whole-identifier replacements, keys lower case
     */
    public static void configureAbbreviations(boolean applyToName, Map<String, String> words,
                                              Map<String, String> wholeNames) {
        applyAbbrToName = applyToName;
        abbrWords = words == null ? Collections.emptyMap() : new HashMap<>(words);
        abbrWholeNames = wholeNames == null ? Collections.emptyMap() : new HashMap<>(wholeNames);
    }

    // --------------------------------------------------------------- renderer

    private final List<TemplateItem> items;
    private String lineEnd = System.lineSeparator();
    private final Map<TemplateType, TemplateAppender> appenders = new HashMap<>();
    private final Map<String, String> customs;

    /**
     * Parses a template.
     *
     * @param template the template text
     * @param customs  variables the template may read when the model has no such
     *                 member, and the source of {@code ${author}}; may be
     *                 {@code null}
     * @throws ParseException when the template is malformed, carrying the line
     *                        number and the text that follows the error
     */
    public TemplateManager(String template, Map<String, String> customs) throws ParseException {
        appenders.put(TemplateType.TEXT, this::appendText);
        appenders.put(TemplateType.ITEM, this::appendItem);
        appenders.put(TemplateType.SUPER, this::appendSuper);
        appenders.put(TemplateType.IF, this::appendIf);
        appenders.put(TemplateType.FOR, this::appendFor);
        appenders.put(TemplateType.DATE, this::appendDate);
        appenders.put(TemplateType.USER, this::appendUser);
        appenders.put(TemplateType.AUTHOR, this::appendAuthor);

        // The line ending the template itself uses is the one the 'for'
        // directive re-indents with, so a CRLF template stays CRLF.
        int idx = template.indexOf("\n");
        if (idx >= 0) {
            lineEnd = idx > 0 && template.charAt(idx - 1) == '\r' ? "\r\n" : "\n";
        }
        this.customs = customs;
        items = parseTemplate(new ParseContext(template));
    }

    /**
     * Applies the shared formatting attributes: {@code prepend}/{@code postpend}
     * (or {@code quote} for both), then padding to {@code padSize} on the side
     * {@code padDir} names.
     *
     * <p>A {@code null} value renders as nothing at all, not as the string
     * "null", and not even the padding is emitted.
     */
    private void appendBase(StringBuilder sb, Map<String, Object> map, Object val) {
        String spadsz = (String) map.get("padsize");
        int padsz = spadsz == null ? 0 : Integer.parseInt(spadsz);
        String spaddr = (String) map.get("paddir");
        boolean padLeft = "left".equalsIgnoreCase(spaddr);
        String quote = (String) map.get("quote");
        String qpre = (String) map.get("prepend");
        String qpos = (String) map.get("postpend");
        if (qpre == null) {
            qpre = quote;
        }
        if (qpos == null) {
            qpos = quote;
        }
        if (val == null) {
            return;
        }
        String valstr = String.valueOf(val);
        if (qpre != null) {
            valstr = qpre + valstr;
        }
        if (qpos != null) {
            valstr = valstr + qpos;
        }
        if (!padLeft) {
            sb.append(valstr);
        }
        if (padsz > 0) {
            int vsize = padsz - Str.displayWidth(valstr);
            if (vsize < 0) {
                vsize = 0;
            }
            sb.append(Str.space(vsize, ' '));
        }
        if (padLeft) {
            sb.append(valstr);
        }
    }

    private String getKey(Map<String, Object> props) throws ParseException {
        String mkey = (String) props.get("key");
        if (mkey == null) {
            mkey = (String) props.get("item");
        }
        if (mkey == null) {
            throw new ParseException(
                    "'key' or 'item' is required, but none given in: " + props.keySet(), 0);
        }
        return mkey;
    }

    private void appendText(StringBuilder sb, TemplateItem template, Object mapper, Object supr) {
        sb.append(template.cont.toString());
    }

    /** One step of a dotted key: a name and, for processors, its arguments. */
    private static final class ItemKey {
        String key;
        final List<Object> params = new ArrayList<>();

        ItemKey() {
        }

        ItemKey(String key) {
            this.key = key;
        }
    }

    /**
     * Splits {@code name.suffix.replace("a","b")} into its steps.
     *
     * <p>Written as a character scanner rather than a split on {@code .} because
     * a processor argument may itself hold dots, quotes and commas.
     */
    private static List<ItemKey> parseKeys(String mkey) {
        if (!mkey.endsWith(".")) {
            mkey = mkey + ".";
        }

        int i = 0;
        int len = mkey.length();
        List<ItemKey> res = new ArrayList<>();
        StringBuilder sb = new StringBuilder();
        ItemKey curr = new ItemKey();
        boolean isParam = false;
        boolean isOpen = false;
        int openchar = -1;
        while (i < len) {
            char c = mkey.charAt(i);
            if (isOpen) {
                if (c == openchar) {
                    curr.params.add(sb.toString());
                    sb = new StringBuilder();
                    openchar = -1;
                    isOpen = false;
                } else {
                    sb.append(c);
                }
            } else if (Str.contains(new char[]{'\'', '"'}, c)) {
                openchar = c;
                isOpen = true;
                sb = new StringBuilder();
            } else if (c == '.') {
                if (curr.key == null) {
                    curr.key = sb.toString();
                }
                res.add(curr);
                curr = new ItemKey();
                sb = new StringBuilder();
            } else if (c == '(') {
                curr.key = sb.toString();
                sb = new StringBuilder();
                isParam = true;
            } else if (isParam) {
                if (c == ')' || c == ',') {
                    // Collect the accumulated unquoted argument, if any. Quoted
                    // arguments were already collected by the isOpen branch,
                    // which leaves sb empty here.
                    String param = sb.toString();
                    if (!param.isEmpty()) {
                        curr.params.add(param);
                    }
                    sb = new StringBuilder();
                    if (c == ')') {
                        isParam = false;
                    }
                } else if (!Str.isSpace(c)) {
                    sb.append(c);
                }
            } else if (!Str.isSpace(c)) {
                sb.append(c);
            }
            i++;
        }

        if (applyAbbrToName && !res.isEmpty() && "name".equalsIgnoreCase(res.get(0).key)) {
            res.add(1, new ItemKey("abbr"));
        }

        return res;
    }

    /**
     * Reads a key off the model and runs the processors chained behind it.
     *
     * <p>A key the model does not have falls back to the custom variables and
     * then to the empty string: a template that mentions a column some tables do
     * not have still has to render.
     */
    private static Object getItemProcessed(String mkey, Object mapper, Map<String, String> customs)
            throws Exception {
        List<ItemKey> keys = parseKeys(mkey);
        String key = Str.trim(keys.get(0).key);
        Object val = Models.getValue(mapper, key);
        if (val == null) {
            val = Models.getValue(customs, key);
        }
        if (val == null) {
            LOG.log(Level.FINE, "template key not found in model or custom variables: {0}", key);
            val = "";
        }
        for (int i = 1; i < keys.size(); i++) {
            ItemKey ikey = keys.get(i);
            String proc = Str.trim(ikey.key).toLowerCase();
            ItemProcHandler handler = PROCS.get(proc);
            if (handler == null) {
                throw new IllegalArgumentException("cannot find '" + proc
                        + "' in string processors, valid values are: " + PROCS.keySet());
            }
            val = handler.process(val.toString(), ikey.params);
        }
        return val;
    }

    @SuppressWarnings("unchecked")
    private void appendItemBase(StringBuilder sb, TemplateItem template, Object mapper)
            throws Exception {
        Map<String, Object> map = (Map<String, Object>) template.cont;
        appendBase(sb, map, getItemProcessed(getKey(map), mapper, customs));
    }

    private void appendItem(StringBuilder sb, TemplateItem template, Object mapper, Object supr)
            throws Exception {
        appendItemBase(sb, template, mapper);
    }

    private void appendSuper(StringBuilder sb, TemplateItem template, Object mapper, Object supr)
            throws Exception {
        appendItemBase(sb, template, supr);
    }

    // ------------------------------------------------------------- conditions

    private static boolean condEquals(String key, String condVal, Object mapper,
                                      Map<String, String> customs) throws Exception {
        return condVal.equalsIgnoreCase(String.valueOf(getItemProcessed(key, mapper, customs)));
    }

    private static boolean condNotEquals(String key, String condVal, Object mapper,
                                         Map<String, String> customs) throws Exception {
        return !condEquals(key, condVal, mapper, customs);
    }

    /**
     * True when the value is a list holding an item whose {@code name} matches,
     * or a string equal to one of the comma-separated alternatives.
     */
    @SuppressWarnings("unchecked")
    private static boolean condContains(String key, String condVal, Object mapper,
                                        Map<String, String> customs) throws Exception {
        Object objValue = getItemProcessed(key, mapper, customs);
        boolean contains = false;
        if (objValue instanceof List) {
            for (Object o : (List<Object>) objValue) {
                if (String.valueOf(Models.getValue(o, "name")).equalsIgnoreCase(condVal)) {
                    contains = true;
                    break;
                }
            }
        } else if (objValue instanceof CharSequence) {
            String strValue = String.valueOf(objValue);
            for (String item : Str.split(condVal, ",", true)) {
                if (strValue.equalsIgnoreCase(item)) {
                    contains = true;
                    break;
                }
            }
        } else {
            throw new IllegalArgumentException("contains/notcontains in if statement item must be "
                    + "a collection object or a ',' separated string.");
        }
        return contains;
    }

    private static boolean condNotContains(String key, String condVal, Object mapper,
                                           Map<String, String> customs) throws Exception {
        return !condContains(key, condVal, mapper, customs);
    }

    private static boolean condStartsWith(String key, String condVal, Object mapper,
                                          Map<String, String> customs) throws Exception {
        String oval = String.valueOf(getItemProcessed(key, mapper, customs));
        return oval.toLowerCase().startsWith(condVal.toLowerCase());
    }

    private static boolean condNotStartsWith(String key, String condVal, Object mapper,
                                             Map<String, String> customs) throws Exception {
        return !condStartsWith(key, condVal, mapper, customs);
    }

    private static boolean condEndsWith(String key, String condVal, Object mapper,
                                        Map<String, String> customs) throws Exception {
        String oval = String.valueOf(getItemProcessed(key, mapper, customs));
        return oval.toLowerCase().endsWith(condVal.toLowerCase());
    }

    private static boolean condNotEndsWith(String key, String condVal, Object mapper,
                                           Map<String, String> customs) throws Exception {
        return !condEndsWith(key, condVal, mapper, customs);
    }

    private static boolean condMatches(String key, String condVal, Object mapper,
                                       Map<String, String> customs) throws Exception {
        return Pattern.matches(condVal,
                String.valueOf(getItemProcessed(key, mapper, customs)));
    }

    private static boolean condNotMatches(String key, String condVal, Object mapper,
                                          Map<String, String> customs) throws Exception {
        return !condMatches(key, condVal, mapper, customs);
    }

    /** Every condition attribute present has to hold; they are ANDed. */
    @SuppressWarnings("unchecked")
    private void appendIf(StringBuilder sb, TemplateItem template, Object mapper, Object supr)
            throws Exception {
        Map<String, Object> map = (Map<String, Object>) template.cont;
        List<TemplateItem> ttpls = (List<TemplateItem>) map.get("true");
        Object ftpls = map.get("false");
        String mkey = getKey(map);
        boolean condMet = true;

        for (String key : map.keySet()) {
            IfCondHandler cond = IFCONDS.get(key);
            if (cond != null && !cond.check(mkey, String.valueOf(map.get(key)), mapper, customs)) {
                condMet = false;
                break;
            }
        }

        if (condMet) {
            appendMapper(sb, ttpls, mapper, supr);
        } else if (ftpls instanceof TemplateItem) {
            appendIf(sb, (TemplateItem) ftpls, mapper, supr);
        } else if (ftpls instanceof List) {
            appendMapper(sb, (List<TemplateItem>) ftpls, mapper, supr);
        }
    }

    /**
     * Repeats the body over a list member of the model.
     *
     * <p>The separator {@code inStr} is re-indented to the column the loop
     * started at, which is what keeps a comma-first column list lined up under
     * {@code SELECT}. {@code indent} shifts that column, and it is routinely
     * negative for exactly that reason.
     */
    @SuppressWarnings("unchecked")
    private void appendFor(StringBuilder sb, TemplateItem template, Object mapper, Object supr)
            throws Exception {
        Map<String, Object> map = (Map<String, Object>) template.cont;
        String mkey = getKey(map);
        String instr = (String) map.get("instr");
        String indent = (String) map.get("indent");
        String[] skips = Str.split((String) map.get("skiplist"), ",", true);
        int idnt = indent == null ? 0 : Integer.parseInt(indent);
        List<TemplateItem> tpls = (List<TemplateItem>) map.get("items");
        List<Object> litems = (List<Object>) Models.getValue(mapper, mkey);
        if (litems == null) {
            throw new IllegalArgumentException("Model has no '" + mkey + "' member: " + mapper);
        }
        int stidx = sb.lastIndexOf("\n") + 1;
        int splen = Str.displayWidth(sb.substring(stidx)) + idnt;
        String prepend = Str.space(splen, ' ');
        boolean isFirst = true;
        for (int i = 0; i < litems.size(); i++) {
            Object o = litems.get(i);
            if (skips != null) {
                Object n = Models.getValue(o, "name");
                if (n != null && Str.contains(skips, n.toString())) {
                    continue;
                }
            }
            if (!isFirst && instr != null) {
                // Every line break inside 'instr' becomes the template's line end
                // and the fragment after it is re-indented.
                String[] parts = instr.split("\r?\n", -1);
                sb.append(parts[0]);
                for (int p = 1; p < parts.length; p++) {
                    sb.append(lineEnd).append(prepend).append(parts[p]);
                }
            }
            Models.setValue(o, "no", i + 1);
            appendMapper(sb, tpls, o, mapper);
            isFirst = false;
        }
    }

    @SuppressWarnings("unchecked")
    private void appendDate(StringBuilder sb, TemplateItem template, Object mapper, Object supr) {
        Map<String, Object> map = (Map<String, Object>) template.cont;
        String format = (String) map.getOrDefault("format", DEFAULT_DATE_FORMAT);
        appendBase(sb, map, new SimpleDateFormat(format).format(new Date()));
    }

    @SuppressWarnings("unchecked")
    private void appendUser(StringBuilder sb, TemplateItem template, Object mapper, Object supr) {
        appendBase(sb, (Map<String, Object>) template.cont, USER_ID);
    }

    @SuppressWarnings("unchecked")
    private void appendAuthor(StringBuilder sb, TemplateItem template, Object mapper, Object supr)
            throws Exception {
        appendBase(sb, (Map<String, Object>) template.cont, Models.getValue(customs, "author"));
    }

    private void appendMapper(StringBuilder sb, List<TemplateItem> templates, Object mapper,
                              Object supr) throws Exception {
        for (TemplateItem tpl : templates) {
            appenders.get(tpl.type).append(sb, tpl, mapper, supr);
        }
    }

    /**
     * Renders the template against one model.
     *
     * @param mapper the model: a map, a bean, or anything {@link Models} can read
     * @return the rendered text
     * @throws Exception whatever a model accessor or a processor threw
     */
    public String applyMapper(Object mapper) throws Exception {
        StringBuilder sb = new StringBuilder();
        appendMapper(sb, items, mapper, null);
        return sb.toString();
    }
}
