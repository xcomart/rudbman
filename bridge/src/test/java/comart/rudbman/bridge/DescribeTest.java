package comart.rudbman.bridge;

import com.google.gson.JsonArray;
import com.google.gson.JsonElement;
import com.google.gson.JsonObject;
import comart.rudbman.bridge.support.H2;
import org.junit.jupiter.api.AfterEach;
import org.junit.jupiter.api.BeforeEach;
import org.junit.jupiter.api.Test;

import java.util.ArrayList;
import java.util.Arrays;
import java.util.List;

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertFalse;
import static org.junit.jupiter.api.Assertions.assertNotNull;
import static org.junit.jupiter.api.Assertions.assertNull;
import static org.junit.jupiter.api.Assertions.assertTrue;

/** DESCRIBE against a schema with keys, foreign keys and indexes. */
class DescribeTest {

    private long session;
    private String url;

    /** Extra sessions opened by a test on the same database, closed with it. */
    private final List<Long> extraSessions = new ArrayList<>();

    @BeforeEach
    void setUp() {
        url = H2.freshUrl();
        session = H2.open(url);
        H2.exec(session, "create schema app");
        H2.exec(session, "create table app.parent ("
                + "id int not null, code varchar(20) not null, "
                + "constraint pk_parent primary key (id))");
        H2.exec(session, "comment on table app.parent is 'parent rows'");
        H2.exec(session, "create table app.child ("
                + "id int not null primary key, "
                + "parent_id int not null, "
                + "note varchar(100), "
                + "constraint fk_child_parent foreign key (parent_id) references app.parent(id))");
        H2.exec(session, "create index idx_child_note on app.child(note)");
        H2.exec(session, "create unique index uq_parent_code on app.parent(code)");
        H2.exec(session, "create view app.v_child as select id from app.child");
    }

    @AfterEach
    void tearDown() {
        for (long s : extraSessions) {
            H2.close(s);
        }
        H2.close(session);
    }

    private JsonArray describe(String kind, String... kv) {
        return describe(session, kind, kv);
    }

    private JsonArray describe(long session, String kind, String... kv) {
        JsonObject req = new JsonObject();
        req.addProperty("kind", kind);
        for (int i = 0; i < kv.length; i += 2) {
            req.addProperty(kv[i], kv[i + 1]);
        }
        JsonObject resp = H2.call(Ops.DESCRIBE, session, 0, req).json();
        assertEquals(kind, resp.get("kind").getAsString());
        return resp.get("items").getAsJsonArray();
    }

    private static List<String> pluck(JsonArray arr, String member) {
        List<String> out = new ArrayList<>();
        for (JsonElement e : arr) {
            JsonElement v = e.getAsJsonObject().get(member);
            out.add(v == null || v.isJsonNull() ? null : v.getAsString());
        }
        return out;
    }

    @Test
    void catalogs() {
        JsonArray items = describe("catalogs");
        assertFalse(items.isEmpty());
        assertNotNull(items.get(0).getAsJsonObject().get("name").getAsString());
    }

    @Test
    void schemas() {
        List<String> names = pluck(describe("schemas"), "name");
        assertTrue(names.contains("APP"), names.toString());
        assertTrue(names.contains("PUBLIC"), names.toString());
        // Every schema row carries its catalog, even when the driver leaves it null.
        assertTrue(describe("schemas").get(0).getAsJsonObject().has("catalog"));
    }

    @Test
    void tablesAndViews() {
        JsonArray all = describe("schemas", "schema", "APP");
        assertFalse(all.isEmpty());

        List<String> names = pluck(describe("tables", "schema", "APP"), "name");
        assertTrue(names.contains("PARENT"), names.toString());
        assertTrue(names.contains("CHILD"), names.toString());
        assertTrue(names.contains("V_CHILD"), names.toString());

        JsonObject req = new JsonObject();
        req.addProperty("kind", "tables");
        req.addProperty("schema", "APP");
        JsonArray types = new JsonArray();
        types.add("VIEW");
        req.add("types", types);
        JsonArray views = H2.call(Ops.DESCRIBE, session, 0, req).json()
                .get("items").getAsJsonArray();
        assertEquals(List.of("V_CHILD"), pluck(views, "name"));
        assertEquals("VIEW", views.get(0).getAsJsonObject().get("type").getAsString());

        // Table comments come back verbatim; this is the arbitrary-driver-text
        // path Gson is here for.
        JsonArray parent = describe("tables", "schema", "APP", "table", "PARENT");
        assertEquals("parent rows", parent.get(0).getAsJsonObject().get("remarks").getAsString());
    }

    @Test
    void columns() {
        JsonArray items = describe("columns", "schema", "APP", "table", "CHILD");
        assertEquals(List.of("ID", "PARENT_ID", "NOTE"), pluck(items, "name"));

        JsonObject id = items.get(0).getAsJsonObject();
        assertEquals("INTEGER", id.get("jdbc_type").getAsString());
        assertEquals(1, id.get("ordinal").getAsInt());
        assertFalse(id.get("is_nullable").getAsBoolean());

        JsonObject note = items.get(2).getAsJsonObject();
        assertEquals("CHARACTER VARYING", note.get("type_name").getAsString());
        assertEquals(100, note.get("size").getAsInt());
        assertTrue(note.get("is_nullable").getAsBoolean());
        // Optional metadata columns are present as JSON null rather than absent.
        assertTrue(note.has("default"));
        assertTrue(note.has("remarks"));
    }

    /**
     * Comments are topped up from the vendor catalogue only on the products
     * whose drivers hide them - Oracle, SQL Server, MySQL, MariaDB. H2 reports
     * REMARKS itself, so DESCRIBE has to answer exactly what the driver said,
     * both where there is a comment and where there is none.
     */
    @Test
    void driverSuppliedRemarksAreLeftAlone() {
        H2.exec(session, "comment on column app.parent.code is 'lookup code'");

        JsonArray tables = describe("tables", "schema", "APP");
        assertEquals("parent rows", remarksOf(tables, "PARENT"));
        // No comment was ever put on CHILD, and none is invented for it.
        assertNull(remarksOf(tables, "CHILD"));

        JsonArray cols = describe("columns", "schema", "APP", "table", "PARENT");
        assertEquals(List.of("ID", "CODE"), pluck(cols, "name"));
        assertEquals(Arrays.asList(null, "lookup code"), pluck(cols, "remarks"));
    }

    /** @return the remarks of the row named {@code name}, or null */
    private static String remarksOf(JsonArray items, String name) {
        for (JsonElement e : items) {
            JsonObject o = e.getAsJsonObject();
            if (name.equals(o.get("name").getAsString())) {
                JsonElement r = o.get("remarks");
                return r == null || r.isJsonNull() ? null : r.getAsString();
            }
        }
        throw new AssertionError("no row named " + name + " in " + items);
    }

    /**
     * A driver definition may bring its own comment queries, and where it does
     * they replace the built-in lookup on every product - H2 included, whose
     * driver reports {@code REMARKS} perfectly well. Nothing around the query
     * changes: the driver still wins wherever it gave an answer, and only the
     * holes are filled.
     */
    @Test
    void aCustomTableQueryFillsTheHolesOnAnyProduct() {
        long s = custom("select TABLE_NAME, 'custom:' || TABLE_NAME "
                + "from INFORMATION_SCHEMA.TABLES where TABLE_SCHEMA = '${schema}'", null);

        JsonArray items = describe(s, "tables", "schema", "APP");
        // PARENT has a comment of its own, so the custom query does not get to
        // overwrite it - the same rule the built-in queries live under.
        assertEquals("parent rows", remarksOf(items, "PARENT"));
        assertEquals("custom:CHILD", remarksOf(items, "CHILD"));
        assertEquals("custom:V_CHILD", remarksOf(items, "V_CHILD"));
    }

    /**
     * The column query is scoped to a schema and knows nothing of the table the
     * request asked for; the overlay does that narrowing, which is why one query
     * can answer a describe of any single table in the schema.
     */
    @Test
    void aCustomColumnQueryIsScopedBySchemaAndNarrowedByTheOverlay() {
        H2.exec(session, "comment on column app.parent.code is 'lookup code'");
        long s = custom(null, "select TABLE_NAME, COLUMN_NAME, "
                + "'custom:' || TABLE_NAME || '.' || COLUMN_NAME "
                + "from INFORMATION_SCHEMA.COLUMNS where TABLE_SCHEMA = '${schema}'");

        JsonArray items = describe(s, "columns", "schema", "APP", "table", "PARENT");
        assertEquals(List.of("ID", "CODE"), pluck(items, "name"));
        assertEquals(List.of("custom:PARENT.ID", "lookup code"), pluck(items, "remarks"));
    }

    /**
     * A pattern filter does not describe the single scope these queries assume,
     * so none is run - and the built-in path is not reached for it either. The
     * answer is the driver's own, untouched.
     */
    @Test
    void aPatternFilterLeavesACustomQueryUnrun() {
        long s = custom("select TABLE_NAME, 'custom:' || TABLE_NAME "
                + "from INFORMATION_SCHEMA.TABLES where TABLE_SCHEMA = '${schema}'", null);

        JsonArray items = describe(s, "tables", "schema_pattern", "APP");
        assertEquals("parent rows", remarksOf(items, "PARENT"));
        assertNull(remarksOf(items, "CHILD"));
    }

    /** Same for a variable the request carries no exact filter for. */
    @Test
    void aVariableTheRequestCannotFillLeavesACustomQueryUnrun() {
        long s = custom("select TABLE_NAME, 'custom:' || TABLE_NAME "
                + "from INFORMATION_SCHEMA.TABLES where TABLE_CATALOG = '${catalog}'", null);

        // The request names a schema but no catalog, so there is nothing to put
        // where ${catalog} stands.
        JsonArray items = describe(s, "tables", "schema", "APP");
        assertEquals("parent rows", remarksOf(items, "PARENT"));
        assertNull(remarksOf(items, "CHILD"));
    }

    /**
     * A query the server rejects is the whole reason the lookup is fenced: the
     * describe still answers, with exactly what the driver said.
     */
    @Test
    void aBrokenCustomQueryCostsOnlyTheComments() {
        long s = custom("select NOPE from NO_SUCH_TABLE", "this is not sql at all");

        JsonArray tables = describe(s, "tables", "schema", "APP");
        assertEquals("parent rows", remarksOf(tables, "PARENT"));
        assertNull(remarksOf(tables, "CHILD"));

        JsonArray cols = describe(s, "columns", "schema", "APP", "table", "CHILD");
        assertEquals(List.of("ID", "PARENT_ID", "NOTE"), pluck(cols, "name"));
        assertEquals(Arrays.asList(null, null, null), pluck(cols, "remarks"));
    }

    /**
     * @return a second session on the same database, carrying the driver-defined
     *         comment queries. Either may be {@code null}, which is how a
     *         definition that names only one of them arrives.
     */
    private long custom(String tableSql, String columnSql) {
        JsonObject spec = new JsonObject();
        if (tableSql != null) {
            spec.addProperty("table_comments_sql", tableSql);
        }
        if (columnSql != null) {
            spec.addProperty("column_comments_sql", columnSql);
        }
        long s = H2.open(url, spec);
        extraSessions.add(s);
        return s;
    }

    @Test
    void primaryKeys() {
        JsonArray items = describe("primary_keys", "schema", "APP", "table", "PARENT");
        assertEquals(1, items.size());
        JsonObject pk = items.get(0).getAsJsonObject();
        assertEquals("ID", pk.get("column").getAsString());
        assertEquals(1, pk.get("seq").getAsInt());
        assertEquals("PK_PARENT", pk.get("name").getAsString());
    }

    @Test
    void importedAndExportedKeys() {
        JsonArray imported = describe("imported_keys", "schema", "APP", "table", "CHILD");
        assertEquals(1, imported.size());
        JsonObject fk = imported.get(0).getAsJsonObject();
        assertEquals("PARENT", fk.get("pk_table").getAsString());
        assertEquals("ID", fk.get("pk_column").getAsString());
        assertEquals("CHILD", fk.get("fk_table").getAsString());
        assertEquals("PARENT_ID", fk.get("fk_column").getAsString());
        assertEquals("FK_CHILD_PARENT", fk.get("fk_name").getAsString());
        assertEquals(1, fk.get("seq").getAsInt());

        // Same relationship seen from the other end, same key names.
        JsonArray exported = describe("exported_keys", "schema", "APP", "table", "PARENT");
        assertEquals(1, exported.size());
        JsonObject ex = exported.get(0).getAsJsonObject();
        assertEquals("CHILD", ex.get("fk_table").getAsString());
        assertEquals("PARENT", ex.get("pk_table").getAsString());
    }

    @Test
    void indexes() {
        JsonArray items = describe("indexes", "schema", "APP", "table", "PARENT");
        List<String> names = pluck(items, "name");
        assertTrue(names.contains("UQ_PARENT_CODE"), names.toString());

        boolean sawUnique = false;
        for (JsonElement e : items) {
            JsonObject o = e.getAsJsonObject();
            if ("UQ_PARENT_CODE".equals(o.get("name").getAsString())) {
                assertFalse(o.get("non_unique").getAsBoolean());
                assertEquals("CODE", o.get("column").getAsString());
                assertEquals(1, o.get("ordinal").getAsInt());
                sawUnique = true;
            }
        }
        assertTrue(sawUnique);

        JsonObject req = new JsonObject();
        req.addProperty("kind", "indexes");
        req.addProperty("schema", "APP");
        req.addProperty("table", "CHILD");
        req.addProperty("unique_only", true);
        JsonArray uniqueOnly = H2.call(Ops.DESCRIBE, session, 0, req).json()
                .get("items").getAsJsonArray();
        assertFalse(pluck(uniqueOnly, "name").contains("IDX_CHILD_NOTE"));
    }

    @Test
    void typeInfo() {
        JsonArray items = describe("type_info");
        assertFalse(items.isEmpty());
        List<String> names = pluck(items, "name");
        assertTrue(names.stream().anyMatch(n -> n.contains("INTEGER")), names.toString());
        JsonObject first = items.get(0).getAsJsonObject();
        assertTrue(first.has("jdbc_type"));
        assertTrue(first.has("precision"));
    }

    @Test
    void kindsRequiringATableSaySoWhenItIsMissing() {
        JsonObject req = new JsonObject();
        req.addProperty("kind", "primary_keys");
        req.addProperty("schema", "APP");
        JsonObject err = H2.call(Ops.DESCRIBE, session, 0, req).error();
        assertEquals("protocol", err.get("kind").getAsString());
    }

    @Test
    void unknownKindIsAProtocolError() {
        JsonObject req = new JsonObject();
        req.addProperty("kind", "nonsense");
        assertEquals("protocol",
                H2.call(Ops.DESCRIBE, session, 0, req).error().get("kind").getAsString());
    }
}
