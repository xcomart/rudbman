package comart.rudbman.bridge.template;

import org.junit.jupiter.api.Test;

import java.io.InputStream;
import java.nio.charset.StandardCharsets;
import java.text.ParseException;
import java.util.ArrayList;
import java.util.Arrays;
import java.util.HashMap;
import java.util.LinkedHashMap;
import java.util.List;
import java.util.Map;

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertThrows;
import static org.junit.jupiter.api.Assertions.assertTrue;

/**
 * Pins the template syntax, construct by construct.
 *
 * <p>This engine was inherited rather than written, and the reason it was
 * inherited is that template assets in the wild must keep rendering the same
 * bytes (architecture.md 12.3). So these tests are not really about correctness
 * in the abstract: they are a record of what the syntax means today, so that a
 * later refactor cannot quietly change it.
 *
 * <p>{@link #jdbgenJavaModelTemplateStillRenders} is the other half of that:
 * a real jdbgen template asset, rendered against a hand-built model, asserting
 * on fragments that would move if any of the pieces drifted.
 */
class TemplateManagerTest {

    private static String render(String template, Object model) throws Exception {
        return new TemplateManager(template, new HashMap<>()).applyMapper(model);
    }

    private static String render(String template, Object model, Map<String, String> customs)
            throws Exception {
        return new TemplateManager(template, customs).applyMapper(model);
    }

    private static Map<String, Object> map(Object... kv) {
        Map<String, Object> m = new LinkedHashMap<>();
        for (int i = 0; i < kv.length; i += 2) {
            m.put((String) kv[i], kv[i + 1]);
        }
        return m;
    }

    // ------------------------------------------------------------------ text

    @Test
    void textWithoutDirectivesIsCopiedThrough() throws Exception {
        assertEquals("no directives here\nsecond line\n",
                render("no directives here\nsecond line\n", map()));
    }

    @Test
    void aQuotedDirectiveIsLiteralText() throws Exception {
        // The only way for a template to emit a "${" of its own.
        assertEquals("literal ${not.a.directive} here",
                render("literal ${\"${not.a.directive}\"} here", map()));
    }

    @Test
    void aQuotedDirectiveHonoursBackslashEscapes() throws Exception {
        assertEquals("it's", render("${'it\\'s'}", map()));
    }

    // ------------------------------------------------------------------ item

    @Test
    void theShorthandIsAnItemLookup() throws Exception {
        assertEquals("hello world",
                render("hello ${what}", map("what", "world")));
        assertEquals("hello world",
                render("hello ${item:key=what}", map("what", "world")));
    }

    @Test
    void itemAlsoReadsBeans() throws Exception {
        // jdbgen fed this engine beans; both shapes have to keep working.
        assertEquals("world", render("${what}", new Object() {
            @SuppressWarnings("unused")
            public String getWhat() {
                return "world";
            }
        }));
    }

    @Test
    void anAbsentKeyFallsBackToTheCustomVariablesThenToNothing() throws Exception {
        Map<String, String> customs = new HashMap<>();
        customs.put("project", "rudbman");
        assertEquals("rudbman/", render("${project}/${nowhere}", map(), customs));
    }

    @Test
    void aDotStartsAProcessorChainAndNeverANestedLookup() throws Exception {
        // Worth pinning because it reads like a path and is not one: everything
        // after the first dot has to name a processor. A nested model is reached
        // with 'for' or 'super', never with a dotted key.
        Exception e = assertThrows(Exception.class,
                () -> render("${outer.inner}", map("outer", map("inner", "x"))));
        assertTrue(e.getMessage().contains("string processors"), e.getMessage());
    }

    @Test
    void itemFormattingPadsAndQuotes() throws Exception {
        assertEquals("[ab      ]",
                render("[${item:key=v,padSize=8,padDir=right}]", map("v", "ab")));
        assertEquals("[      ab]",
                render("[${item:key=v,padSize=8,padDir=left}]", map("v", "ab")));
        assertEquals("'ab'", render("${item:key=v,quote=\"'\"}", map("v", "ab")));
        assertEquals("<ab>",
                render("${item:key=v,prepend=\"<\",postpend=\">\"}", map("v", "ab")));
    }

    @Test
    void paddingCountsCjkCharactersAsTwoColumns() throws Exception {
        // Inherited on purpose: it is what lines generated code up in a
        // fixed-width font, and templates were written against it.
        assertEquals("[사용자    ]",
                render("[${item:key=v,padSize=10,padDir=right}]", map("v", "사용자")));
    }

    // ------------------------------------------------------------ processors

    @Test
    void everyStringProcessorKeepsItsMeaning() throws Exception {
        Map<String, Object> m = map("v", "USER_INFO_TABLE");
        assertEquals("USER_INFO", render("${v.prefix}", m));
        assertEquals("INFO_TABLE", render("${v.suffix}", m));
        assertEquals("userInfoTable", render("${v.camel}", m));
        assertEquals("UserInfoTable", render("${v.pascal}", m));
        assertEquals("user_info_table", render("${v.snake}", m));
        assertEquals("USER_INFO_TABLE", render("${v.screaming}", m));
        assertEquals("user-info-table", render("${v.skewer}", m));
        assertEquals("user-info-table", render("${v.kebab}", m));
        assertEquals("user_info_table", render("${v.lower}", m));
        assertEquals("USER_INFO_TABLE", render("${v.upper}", m));
    }

    @Test
    void processorsChainLeftToRight() throws Exception {
        assertEquals("infoTable", render("${v.suffix.camel}", map("v", "USER_INFO_TABLE")));
    }

    @Test
    void replaceTakesItsTwoArgumentsQuoted() throws Exception {
        assertEquals("a-b-c", render("${v.replace(\"_\",\"-\")}", map("v", "a_b_c")));
    }

    @Test
    void anUnknownProcessorIsReported() {
        Exception e = assertThrows(Exception.class,
                () -> render("${v.nosuchproc}", map("v", "x")));
        assertTrue(e.getMessage().contains("nosuchproc"), e.getMessage());
    }

    // ------------------------------------------------------------------- for

    @Test
    void forRepeatsItsBodyOverAListMember() throws Exception {
        Map<String, Object> m = map("cols",
                list(map("name", "A"), map("name", "B"), map("name", "C")));
        assertEquals("A|B|C|", render("${for:item=cols}${name}|${endfor}", m));
    }

    @Test
    void forPublishesTheOneBasedPositionAsNo() throws Exception {
        Map<String, Object> m = map("cols", list(map("name", "A"), map("name", "B")));
        assertEquals("1:A 2:B ", render("${for:item=cols}${no}:${name} ${endfor}", m));
    }

    @Test
    void forReIndentsTheSeparatorToTheColumnItStartedAt() throws Exception {
        // This is what produces a comma-first column list lined up under SELECT,
        // and 'indent' is negative for exactly that reason.
        Map<String, Object> m = map("cols",
                list(map("name", "A"), map("name", "B"), map("name", "C")));
        String out = render("SELECT ${for:item=cols,inStr=\"\\n,\",indent=-1}"
                + "${name}${endfor}\n  FROM T\n", m);
        assertEquals("SELECT A\n      ,B\n      ,C\n  FROM T\n", out);
    }

    @Test
    void forSkipsTheNamesInSkipList() throws Exception {
        Map<String, Object> m = map("cols",
                list(map("name", "A"), map("name", "B"), map("name", "C")));
        assertEquals("A|C|", render("${for:item=cols,skipList=\"A2,B\"}${name}|${endfor}", m));
    }

    @Test
    void superReadsTheEnclosingModel() throws Exception {
        Map<String, Object> m = map("table", "T", "cols", list(map("name", "A"), map("name", "B")));
        assertEquals("T.A T.B ",
                render("${for:item=cols}${super:key=table}.${name} ${endfor}", m));
    }

    @Test
    void anUnclosedForIsAParseError() {
        assertThrows(ParseException.class, () -> render("${for:item=cols}${name}", map()));
    }

    // -------------------------------------------------------------------- if

    @Test
    void ifTakesTheTrueBranchAndElseTheOther() throws Exception {
        assertEquals("yes", render("${if:key=v,equals=1}yes${else}no${endif}", map("v", "1")));
        assertEquals("no", render("${if:key=v,equals=1}yes${else}no${endif}", map("v", "2")));
    }

    @Test
    void elifChainsAreFlattenedInOrder() throws Exception {
        String t = "${if:key=v,equals=1}one${elif:key=v,equals=2}two"
                + "${elif:key=v,equals=3}three${else}many${endif}";
        assertEquals("one", render(t, map("v", "1")));
        assertEquals("two", render(t, map("v", "2")));
        assertEquals("three", render(t, map("v", "3")));
        assertEquals("many", render(t, map("v", "9")));
    }

    @Test
    void everyComparisonFormIsAvailable() throws Exception {
        Map<String, Object> m = map("v", "character varying", "list",
                list(map("name", "ID"), map("name", "NAME")));
        assertEquals("y", render("${if:key=v,value=\"character varying\"}y${else}n${endif}", m));
        assertEquals("y", render("${if:key=v,notEquals=int}y${else}n${endif}", m));
        assertEquals("y", render("${if:key=v,startsWith=char}y${else}n${endif}", m));
        assertEquals("y", render("${if:key=v,notStartsWith=int}y${else}n${endif}", m));
        assertEquals("y", render("${if:key=v,endsWith=varying}y${else}n${endif}", m));
        assertEquals("y", render("${if:key=v,notEndsWith=int}y${else}n${endif}", m));
        assertEquals("y", render("${if:key=v,matches=\"char.*\"}y${else}n${endif}", m));
        assertEquals("y", render("${if:key=v,notMatches=\"int.*\"}y${else}n${endif}", m));
        // 'contains' over a list matches on each item's own 'name'.
        assertEquals("y", render("${if:key=list,contains=NAME}y${else}n${endif}", m));
        assertEquals("y", render("${if:key=list,notContains=NOPE}y${else}n${endif}", m));
        // and over a string it matches one of the comma separated alternatives.
        assertEquals("y", render("${if:key=v,contains=\"int,character varying\"}y${else}n${endif}",
                m));
    }

    @Test
    void comparisonsIgnoreCaseExceptTheRegularExpressions() throws Exception {
        assertEquals("y", render("${if:key=v,equals=ABC}y${else}n${endif}", map("v", "abc")));
        assertEquals("n", render("${if:key=v,matches=ABC}y${else}n${endif}", map("v", "abc")));
    }

    @Test
    void severalConditionsOnOneIfAreAllRequired() throws Exception {
        Map<String, Object> m = map("v", "charvar");
        assertEquals("y", render("${if:key=v,startsWith=char,endsWith=var}y${else}n${endif}", m));
        assertEquals("n", render("${if:key=v,startsWith=char,endsWith=zzz}y${else}n${endif}", m));
    }

    @Test
    void anUnknownConditionIsAParseError() {
        assertThrows(ParseException.class,
                () -> render("${if:key=v,soundsLike=x}y${endif}", map("v", "x")));
    }

    @Test
    void anUnclosedIfIsAParseError() {
        assertThrows(ParseException.class, () -> render("${if:key=v,equals=1}y", map("v", "1")));
    }

    // ----------------------------------------------------- user, date, author

    @Test
    void userIsTheLoginUser() throws Exception {
        assertEquals(System.getProperty("user.name"), render("${user}", map()));
    }

    @Test
    void authorComesFromTheCustomVariables() throws Exception {
        Map<String, String> customs = new HashMap<>();
        customs.put("author", "Dennis");
        assertEquals("Dennis", render("${author}", map(), customs));
    }

    @Test
    void dateTakesAFormatWithOrWithoutTheAttributeName() throws Exception {
        String a = render("${date:yyyy-MM-dd}", map());
        String b = render("${date:format=yyyy-MM-dd}", map());
        assertEquals(a, b);
        assertTrue(a.matches("\\d{4}-\\d{2}-\\d{2}"), a);
    }

    @Test
    void dateDefaultsToIsoWhenNoFormatIsGiven() throws Exception {
        assertTrue(render("${date}", map()).matches("\\d{4}-\\d{2}-\\d{2}"));
    }

    // ------------------------------------------------------- malformed input

    @Test
    void anUnclosedDirectiveIsAParseError() {
        assertThrows(ParseException.class, () -> render("${v", map("v", "x")));
    }

    @Test
    void anUnknownDirectiveTypeIsAParseError() {
        assertThrows(ParseException.class, () -> render("${nosuch:key=v}", map("v", "x")));
    }

    // -------------------------------------------------- asset compatibility

    /**
     * The canary: jdbgen's own {@code java_model.java} template, rendered against
     * a model shaped the way jdbgen shapes one. Every assertion below is a
     * composition of pieces - a processor chain, padding, a nested condition -
     * so a drift in any of them shows up here even if its own unit test was
     * updated to match the drift.
     */
    @Test
    void jdbgenJavaModelTemplateStillRenders() throws Exception {
        String template = resource("/templates/java_model.java");

        Map<String, Object> model = map(
                "name", "TB_USER_INFO",
                "remarks", "user info",
                "keys", list(map(
                        "name", "USER_ID",
                        "remarks", "user id",
                        "javaType", "String",
                        "nullable", "0",
                        "typeName", "character varying",
                        "length", "20")),
                "notKeys", list(
                        map("name", "USER_NAME",
                                "remarks", "user name",
                                "javaType", "String",
                                "nullable", "0",
                                "typeName", "character varying",
                                "length", "50"),
                        map("name", "LOGIN_COUNT",
                                "remarks", "login count",
                                "javaType", "Integer",
                                "nullable", "1",
                                "typeName", "integer",
                                "length", "10")));

        String out = render(template, model);

        // ${name.suffix.camel} and ${name.suffix.pascal}: suffix drops the first
        // underscore-separated word, then the case processors do their work.
        assertContains(out, "package com.abc.sample.userInfo;");
        assertContains(out, "public class UserInfoModel");
        assertContains(out, "@Alias(\"user_info\")");
        assertContains(out, "@author " + System.getProperty("user.name"));

        // The key loop: padSize=10 pads "String" out to ten columns.
        assertContains(out, "// user id");
        assertContains(out, "@NotBlank(message=\"user id: Required Item.\")");
        assertContains(out, "private String     userId;");

        // A not-null column gets @NotBlank, a char column gets @Size, and the
        // nullable integer gets neither.
        assertContains(out, "@NotBlank(message=\"user name: Required Item.\")");
        assertContains(out, "@Size(max=50, message=\"user name: Cannot exceeds 50.\")");
        assertContains(out, "private String     userName;");
        assertContains(out, "private Integer    loginCount;");
        assertTrue(!out.contains("login count: Required Item."), out);
        assertTrue(!out.contains("Cannot exceeds 10."), out);
    }

    // --------------------------------------------------------------- helpers

    private static List<Object> list(Object... items) {
        return new ArrayList<>(Arrays.asList(items));
    }

    private static String resource(String name) throws Exception {
        try (InputStream in = TemplateManagerTest.class.getResourceAsStream(name)) {
            if (in == null) {
                throw new AssertionError("missing test resource " + name);
            }
            return new String(in.readAllBytes(), StandardCharsets.UTF_8);
        }
    }

    private static void assertContains(String haystack, String needle) {
        assertTrue(haystack.contains(needle),
                "expected to find\n  " + needle + "\nin\n" + haystack);
    }
}
