package comart.rudbman.bridge.meta;

import com.google.gson.JsonArray;
import com.google.gson.JsonElement;
import com.google.gson.JsonObject;

import comart.rudbman.bridge.BridgeException;
import comart.rudbman.bridge.Json;

import java.sql.Connection;
import java.sql.DatabaseMetaData;
import java.sql.PreparedStatement;
import java.sql.ResultSet;
import java.sql.SQLException;
import java.util.ArrayList;
import java.util.Collections;
import java.util.HashMap;
import java.util.HashSet;
import java.util.LinkedHashMap;
import java.util.List;
import java.util.Locale;
import java.util.Map;
import java.util.Set;

/**
 * A dialect profile's answers: the {@code bridge} half of its YAML, merged
 * along its chain of bases by the application, over the {@link Vendor} of the
 * built-in at the bottom of that chain.
 *
 * <p>Each key of {@code docs/dialect-profiles.md} §4.3 answers one method, and
 * a method whose key the profile leaves out is the base's answer - so an empty
 * profile is its base. A key that is {@code null} where the format allows it
 * says the product has none: no trigger catalog, no text of its own for a
 * view, no foreign key to take off before a drop.
 *
 * <p>The application checks every key, value and template hole when the
 * profile loads, so the data here is taken as well formed; a value of the
 * wrong shape still fails the session's open rather than a later request
 * (see {@link #ProfileVendor}).
 */
final class ProfileVendor extends Vendor {

    private final String id;
    private final Vendor base;
    private final JsonObject data;
    private final JsonObject comments;
    private final JsonObject texts;
    private final List<String> detect;

    /**
     * @param id   the profile's id
     * @param base the built-in vendor its chain of bases ends in
     * @param data its merged {@code bridge} half
     * @throws BridgeException if a value is not of its key's shape
     */
    ProfileVendor(String id, Vendor base, JsonObject data) {
        this.id = id;
        this.base = base;
        this.data = data == null ? new JsonObject() : data;
        this.comments = object(this.data, "comments");
        this.texts = object(this.data, "texts");
        List<String> words = new ArrayList<>();
        for (String word : listOr(this.data, "detect", List.of())) {
            words.add(word.toLowerCase(Locale.ROOT));
        }
        this.detect = words;
        check();
    }

    /** Reads every key once, so that a malformed one fails here. */
    private void check() {
        for (Map.Entry<String, JsonElement> e : data.entrySet()) {
            JsonElement v = e.getValue();
            if (v.isJsonNull() || v.isJsonObject() || v.isJsonArray()) {
                continue;
            }
            if (!v.isJsonPrimitive()) {
                throw shape(e.getKey());
            }
        }
        upsertKind();
        nationalStrings();
        profileClient();
    }

    private BridgeException shape(String key) {
        return new BridgeException("protocol",
                "dialect profile " + id + ": bridge." + key + " is not of its key's shape");
    }

    /** @return the profile's id */
    String id() {
        return id;
    }

    // ---------------------------------------------------------------- reading

    private static JsonObject object(JsonObject o, String key) {
        JsonElement e = o.get(key);
        return e != null && e.isJsonObject() ? e.getAsJsonObject() : new JsonObject();
    }

    private Boolean flag(JsonObject o, String key) {
        JsonElement e = o.get(key);
        if (e == null || e.isJsonNull()) {
            return null;
        }
        if (!e.isJsonPrimitive() || !e.getAsJsonPrimitive().isBoolean()) {
            throw shape(key);
        }
        return e.getAsBoolean();
    }

    private boolean flag(String key, boolean otherwise) {
        Boolean value = flag(data, key);
        return value != null ? value : otherwise;
    }

    private String text(JsonObject o, String key) {
        JsonElement e = o.get(key);
        if (e == null || e.isJsonNull()) {
            return null;
        }
        if (!e.isJsonPrimitive()) {
            throw shape(key);
        }
        return e.getAsString();
    }

    private List<String> listOr(JsonObject o, String key, List<String> otherwise) {
        JsonElement e = o.get(key);
        if (e == null) {
            return otherwise;
        }
        if (e.isJsonNull()) {
            return List.of();
        }
        if (!e.isJsonArray()) {
            throw shape(key);
        }
        List<String> out = new ArrayList<>();
        for (JsonElement item : e.getAsJsonArray()) {
            out.add(item.getAsString());
        }
        return out;
    }

    /** @return the key's query object, or {@code null} where it is {@code null} */
    private JsonObject query(JsonObject o, String key) {
        JsonElement e = o.get(key);
        if (e == null || e.isJsonNull()) {
            return null;
        }
        if (!e.isJsonObject()) {
            throw shape(key);
        }
        return e.getAsJsonObject();
    }

    // -------------------------------------------------------------- detection

    @Override
    boolean names(String productName) {
        for (String word : detect) {
            if (productName.contains(word)) {
                return true;
            }
        }
        return false;
    }

    // ------------------------------------------------------- names, catalogs

    @Override
    public boolean catalogIsSchema() {
        return flag("catalog_is_schema", base.catalogIsSchema());
    }

    @Override
    public boolean resultCatalogsResolve() {
        return flag("result_catalogs_resolve", base.resultCatalogsResolve());
    }

    @Override
    boolean seesOnlyItsCatalog() {
        return flag("sees_only_its_catalog", base.seesOnlyItsCatalog());
    }

    // --------------------------------------------------------------- routines

    @Override
    boolean numbersProcedures() {
        return flag("numbers_procedures", base.numbersProcedures());
    }

    @Override
    JsonArray identifyOverloads(DatabaseMetaData dbm, JsonObject req, JsonArray items) {
        return base.identifyOverloads(dbm, req, items);
    }

    @Override
    String routineRemarks(String remarks) {
        return base.routineRemarks(remarks);
    }

    @Override
    String routineText(Connection conn, Ident id, String catalog, String schema, String name,
                       boolean function) {
        if (!texts.has("routine")) {
            return base.routineText(conn, id, catalog, schema, name, function);
        }
        return objectText(conn, "routine", id.qualify(catalog, schema, name), catalog, schema,
                name);
    }

    // -------------------------------------------------------------- sequences

    @Override
    JsonArray sequences(Connection conn, Ident id, JsonObject req) {
        // The profile's queries replace the base's own way of listing them.
        return data.has("sequences") ? null : base.sequences(conn, id, req);
    }

    @Override
    List<String> sequenceQueries(Ident id, String catalog) {
        return data.has("sequences") ? listOr(data, "sequences", List.of())
                : base.sequenceQueries(id, catalog);
    }

    // --------------------------------------------------------------- triggers

    @Override
    JsonArray triggers(Connection conn, String name) {
        if (!data.has("triggers")) {
            return base.triggers(conn, name);
        }
        // `null`: the product keeps no trigger catalog to read.
        return query(data, "triggers") == null ? new JsonArray() : null;
    }

    @Override
    void triggerQuery(StringBuilder sql, List<String> args, Ident id, String catalog,
                      String schema, String name) {
        JsonObject q = query(data, "triggers");
        if (q == null) {
            base.triggerQuery(sql, args, id, catalog, schema, name);
            return;
        }
        sql.append(text(q, "sql"));
        Map<String, String> values = new LinkedHashMap<>();
        values.put("catalog", catalog);
        values.put("schema", schema);
        values.put("name", name);
        exact(sql, args, object(q, "filters"), values);
        String order = text(q, "order_by");
        if (order != null && !order.isEmpty()) {
            sql.append(" ORDER BY ").append(order);
        }
    }

    @Override
    void triggerRow(String[] r) {
        // The base's rewriting is of the base's own query.
        if (!data.has("triggers")) {
            base.triggerRow(r);
        }
    }

    /**
     * Appends {@code AND column = ?} for each filter with a value, in the
     * order of {@code values}; a column two filters name is filtered once, by
     * the first with a value - the MySQL family's database answers both as
     * catalog and as schema.
     */
    private void exact(StringBuilder sql, List<String> args, JsonObject filters,
                       Map<String, String> values) {
        Set<String> used = new HashSet<>();
        for (Map.Entry<String, String> v : values.entrySet()) {
            String column = text(filters, v.getKey());
            String value = v.getValue();
            if (column == null || value == null || value.isEmpty() || !used.add(column)) {
                continue;
            }
            sql.append(" AND ").append(column).append(" = ?");
            args.add(value);
        }
    }

    // --------------------------------------------------------------- comments

    @Override
    boolean marksViewsInRemarks() {
        Boolean value = flag(comments, "marks_views_in_remarks");
        return value != null ? value : base.marksViewsInRemarks();
    }

    @Override
    boolean hidesComments() {
        Boolean value = flag(comments, "hides_comments");
        return value != null ? value : base.hidesComments();
    }

    @Override
    Map<String, String> tableComments(Connection conn, JsonObject req) throws SQLException {
        if (!comments.has("tables")) {
            return base.tableComments(conn, req);
        }
        return comments(conn, query(comments, "tables"), req, 2, false);
    }

    @Override
    Map<String, String> columnComments(Connection conn, JsonObject req) throws SQLException {
        if (!comments.has("columns")) {
            return base.columnComments(conn, req);
        }
        return comments(conn, query(comments, "columns"), req, 3, true);
    }

    /**
     * Runs a comment query: its owner, table and, for columns, column, then
     * the comment, the way {@link Comments#read} reads the built-ins'.
     */
    private Map<String, String> comments(Connection conn, JsonObject q, JsonObject req,
                                         int keys, boolean columns) throws SQLException {
        if (q == null) {
            return Collections.emptyMap();
        }
        JsonObject filters = object(q, "filters");
        String escape = "none".equals(text(comments, "escape")) ? Comments.NO_ESCAPE
                : Comments.ANSI_ESCAPE;
        StringBuilder sql = new StringBuilder(text(q, "sql"));
        List<String> args = new ArrayList<>();
        Set<String> used = new HashSet<>();
        String catalog = Json.str(req, "catalog");
        String catalogColumn = text(filters, "catalog");
        if (catalogColumn != null && catalog != null && !catalog.isEmpty()) {
            sql.append(" AND ").append(catalogColumn).append(" = ?");
            args.add(catalog);
            used.add(catalogColumn);
        }
        pattern(sql, args, used, text(filters, "schema"),
                Comments.Filter.of(req, "schema", "schema_pattern"), escape);
        pattern(sql, args, used, text(filters, "table"),
                Comments.Filter.of(req, "table", "table_pattern"), escape);
        if (columns) {
            pattern(sql, args, used, text(filters, "column"),
                    Comments.Filter.of(req, "column", "column_pattern"), escape);
        }
        return Comments.read(conn, sql.toString(), args, keys);
    }

    private static void pattern(StringBuilder sql, List<String> args, Set<String> used,
                                String column, Comments.Filter filter, String escape) {
        if (column == null || filter.absent() || !used.add(column)) {
            return;
        }
        filter.append(sql, args, column, escape);
    }

    // ----------------------------------------------------------------- upsert

    /** @return the {@code upsert} key's kind, {@code null} where it is left out */
    private String upsertKind() {
        JsonElement e = data.get("upsert");
        if (e == null || e.isJsonNull()) {
            return null;
        }
        if (e.isJsonObject() && e.getAsJsonObject().has("merge")) {
            return "merge";
        }
        if (e.isJsonPrimitive()) {
            String kind = e.getAsString();
            switch (kind) {
                case "none":
                case "on_conflict":
                case "on_duplicate_key":
                case "h2_merge":
                    return kind;
                default:
                    break;
            }
        }
        throw shape("upsert");
    }

    @Override
    boolean upserts() {
        String kind = upsertKind();
        return kind == null ? base.upserts() : !"none".equals(kind);
    }

    @Override
    String upsert(Ident id, String qualified, List<String> columns, List<String> keys,
                  List<String> updates) {
        String kind = upsertKind();
        if (kind == null) {
            return base.upsert(id, qualified, columns, keys, updates);
        }
        switch (kind) {
            case "on_conflict":
                return Upsert.onConflict(id, qualified, columns, keys, updates);
            case "on_duplicate_key":
                return Upsert.onDuplicateKey(id, qualified, columns, keys, updates);
            case "h2_merge":
                return Upsert.h2Merge(id, qualified, columns, keys);
            case "merge": {
                JsonObject merge = object(data.getAsJsonObject("upsert"), "merge");
                Boolean dual = flag(merge, "from_dual");
                Boolean terminated = flag(merge, "terminated");
                return Upsert.merge(id, qualified, columns, keys, updates,
                        dual != null && dual, terminated != null && terminated);
            }
            default:
                return null;
        }
    }

    // -------------------------------------------------------------- table DDL

    @Override
    List<String> dataPrologue() {
        return listOr(data, "data_prologue", base.dataPrologue());
    }

    @Override
    List<String> dataEpilogue() {
        return listOr(data, "data_epilogue", base.dataEpilogue());
    }

    @Override
    List<String> scriptPrologue() {
        return listOr(data, "script_prologue", base.scriptPrologue());
    }

    @Override
    List<String> scriptEpilogue() {
        return listOr(data, "script_epilogue", base.scriptEpilogue());
    }

    @Override
    String tableText(Connection conn, Ident id, String catalog, String schema, String table) {
        if (!texts.has("table")) {
            return base.tableText(conn, id, catalog, schema, table);
        }
        return objectText(conn, "table", id.qualify(catalog, schema, table), catalog, schema,
                table);
    }

    @Override
    Ddl.Script nativeSplit(Connection conn, Ident id, String catalog, String schema,
                           String table) {
        if (!texts.has("table")) {
            return base.nativeSplit(conn, id, catalog, schema, table);
        }
        // The profile's text, as the server writes it: whole, its foreign
        // keys where the server put them.
        return Ddl.whole(tableText(conn, id, catalog, schema, table));
    }

    @Override
    boolean reconstructsGenerated() {
        return base.reconstructsGenerated();
    }

    @Override
    List<String> catalogExtras(Connection conn, Ident id, String catalog, String schema,
                               String table, List<Ddl.Col> cols) {
        return base.catalogExtras(conn, id, catalog, schema, table, cols);
    }

    @Override
    void confirmGenerated(Connection conn, String catalog, String table, List<Ddl.Col> cols) {
        base.confirmGenerated(conn, catalog, table, cols);
    }

    @Override
    Set<String> alwaysIdentities(Connection conn, String schema, String table) {
        return base.alwaysIdentities(conn, schema, table);
    }

    @Override
    boolean qualifiesByCatalog() {
        return flag("qualifies_by_catalog", base.qualifiesByCatalog());
    }

    @Override
    String primaryKeyKind(Connection conn, Ident id, String catalog, String object) {
        return base.primaryKeyKind(conn, id, catalog, object);
    }

    @Override
    boolean defersKeys() {
        return flag("defers_keys", base.defersKeys());
    }

    @Override
    boolean commentsInline() {
        return flag("comments_inline", base.commentsInline());
    }

    @Override
    boolean commentsOn() {
        return flag("comments_on", base.commentsOn());
    }

    @Override
    List<String> nativeIndexes(Connection conn, Ident id, String catalog, String schema,
                               String table, String qualified) {
        return base.nativeIndexes(conn, id, catalog, schema, table, qualified);
    }

    @Override
    List<String> extendedProperties(Connection conn, Ident id, String catalog, String schema,
                                    String table) {
        return base.extendedProperties(conn, id, catalog, schema, table);
    }

    @Override
    boolean computedHasNoType() {
        return flag("computed_has_no_type", base.computedHasNoType());
    }

    @Override
    boolean serialColumns() {
        return flag("serial_columns", base.serialColumns());
    }

    @Override
    String identityClause() {
        String clause = text(data, "identity_clause");
        return clause != null ? clause : base.identityClause();
    }

    @Override
    boolean notNullFirst() {
        return flag("not_null_first", base.notNullFirst());
    }

    @Override
    String maxType(String name, Integer size) {
        return base.maxType(name, size);
    }

    @Override
    boolean indexesForeignKeys() {
        return flag("indexes_foreign_keys", base.indexesForeignKeys());
    }

    // ------------------------------------------------------------- object DDL

    @Override
    String viewText(Connection conn, ObjectDdl.Target t) {
        if (!texts.has("view")) {
            return base.viewText(conn, t);
        }
        return objectText(conn, "view", t.qualified(), t.catalog, t.schema, t.name);
    }

    @Override
    String sequenceText(Connection conn, ObjectDdl.Target t) {
        if (!texts.has("sequence")) {
            return base.sequenceText(conn, t);
        }
        return objectText(conn, "sequence", t.qualified(), t.catalog, t.schema, t.name);
    }

    @Override
    String triggerText(Connection conn, ObjectDdl.Target t) {
        if (!texts.has("trigger")) {
            return base.triggerText(conn, t);
        }
        return objectText(conn, "trigger", t.qualified(), t.catalog, t.schema, t.name);
    }

    /**
     * Runs one of the {@code texts} queries: {@code {qualified}} in its text,
     * the object's catalog, schema and name bound in the order {@code binds}
     * gives, and its text read from {@code column} of the first row.
     *
     * @return the text, terminated, or {@code null} where the key is
     *         {@code null}, the query fails or answers no row
     */
    private String objectText(Connection conn, String key, String qualified, String catalog,
                              String schema, String name) {
        JsonObject q = query(texts, key);
        if (q == null) {
            return null;
        }
        Map<String, String> holes = new HashMap<>();
        holes.put("qualified", qualified);
        String sql = ProfileTemplate.render(text(q, "sql"), holes);
        if (sql == null) {
            return null;
        }
        Map<String, String> values = new HashMap<>();
        values.put("catalog", catalog);
        values.put("schema", schema);
        values.put("name", name);
        List<String> binds = new ArrayList<>();
        for (String bind : listOr(q, "binds", List.of())) {
            binds.add(values.get(bind));
        }
        JsonElement at = q.get("column");
        int column = at != null && at.isJsonPrimitive() ? at.getAsInt() : 1;
        String text = Attempt.<String>on(conn, "profile " + key + " text", c -> {
            try (PreparedStatement ps = c.prepareStatement(sql)) {
                for (int i = 0; i < binds.size(); i++) {
                    ps.setString(i + 1, binds.get(i));
                }
                try (ResultSet rs = ps.executeQuery()) {
                    if (rs.next() && rs.getMetaData().getColumnCount() >= column) {
                        return rs.getString(column);
                    }
                    return null;
                }
            }
        }, null);
        return DdlText.terminated(text);
    }

    @Override
    public boolean createsWithoutCatalog() {
        return flag("creates_without_catalog", base.createsWithoutCatalog());
    }

    @Override
    boolean typesSequences() {
        return flag("types_sequences", base.typesSequences());
    }

    @Override
    boolean cacheOfOneIsNoCache() {
        return flag("cache_of_one_is_no_cache", base.cacheOfOneIsNoCache());
    }

    // --------------------------------------------------------------- literals

    @Override
    public String literal(ResultSet rs, int i, int type) throws SQLException {
        return base.literal(rs, i, type);
    }

    @Override
    public boolean bitsAreNumbers() {
        return flag("bits_are_numbers", base.bitsAreNumbers());
    }

    @Override
    public boolean booleansAreNumbers() {
        return flag("booleans_are_numbers", base.booleansAreNumbers());
    }

    /** @return the {@code national_strings} key, {@code null} where it is left out */
    private String nationalStrings() {
        String value = text(data, "national_strings");
        if (value == null || value.equals("always") || value.equals("national_types")
                || value.equals("never")) {
            return value;
        }
        throw shape("national_strings");
    }

    @Override
    public String stringLiteral(String s, boolean national, int type, ResultSet rs, int i)
            throws SQLException {
        String rule = nationalStrings();
        if (rule == null) {
            return base.stringLiteral(s, national, type, rs, i);
        }
        boolean prefixed = rule.equals("always") || (rule.equals("national_types") && national);
        return prefixed ? "N" + Ident.literal(s) : Ident.literal(s);
    }

    @Override
    public String hexLiteral(String hex) {
        String template = text(data, "hex_literal");
        if (template == null) {
            return base.hexLiteral(hex);
        }
        return ProfileTemplate.render(template, Map.of("hex", hex));
    }

    // ---------------------------------------------------------------- scripts

    /** @return the {@code script_client} key, {@code null} where it is left out */
    private ScriptClient profileClient() {
        String value = text(data, "script_client");
        if (value == null) {
            return null;
        }
        try {
            return ScriptClient.valueOf(value.toUpperCase(Locale.ROOT));
        } catch (IllegalArgumentException e) {
            throw shape("script_client");
        }
    }

    @Override
    public ScriptClient scriptClient() {
        ScriptClient client = profileClient();
        return client != null ? client : base.scriptClient();
    }

    @Override
    public boolean dropsIfExists() {
        return flag("drops_if_exists", base.dropsIfExists());
    }

    @Override
    public String dropTableCascade() {
        String cascade = text(data, "drop_table_cascade");
        return cascade != null ? cascade : base.dropTableCascade();
    }

    @Override
    public String dropForeignKey(String qualified, String key) {
        if (!data.has("drop_foreign_key")) {
            return base.dropForeignKey(qualified, key);
        }
        String template = text(data, "drop_foreign_key");
        if (template == null) {
            return null;
        }
        Map<String, String> holes = new HashMap<>();
        holes.put("table", qualified);
        holes.put("key", key);
        return ProfileTemplate.render(template, holes);
    }

    @Override
    public boolean multiRowValues() {
        return flag("multi_row_values", base.multiRowValues());
    }

    // ----------------------------------------------------------------- backup

    @Override
    public Set<String> extensionMembers(Connection conn, String schema) {
        return base.extensionMembers(conn, schema);
    }

    @Override
    public Set<String> materializedViews(Connection conn, String schema) {
        return base.materializedViews(conn, schema);
    }

    @Override
    public Set<String> tablesNotWritten(Connection conn, String schema) {
        return base.tablesNotWritten(conn, schema);
    }

    @Override
    public List<String[]> backupPreamble(Connection conn, Ident id, String catalog,
                                         String schema) {
        return base.backupPreamble(conn, id, catalog, schema);
    }

    @Override
    public List<String> ownedSequences(Connection conn, Ident id, String schema,
                                       Set<String> names) {
        return base.ownedSequences(conn, id, schema, names);
    }

    @Override
    public Set<String> indexViews(Connection conn, String schema) {
        return base.indexViews(conn, schema);
    }

    @Override
    public boolean packagesRoutines() {
        return base.packagesRoutines();
    }

    @Override
    public List<String> packages(Connection conn, String schema) {
        return base.packages(conn, schema);
    }

    @Override
    public List<String> backupPrologue(Ident id, String catalog, String schema,
                                       boolean routines) {
        if (!data.has("backup_prologue")) {
            return base.backupPrologue(id, catalog, schema, routines);
        }
        return backupLines("backup_prologue", id, catalog, schema, routines, false);
    }

    @Override
    public List<String> backupEpilogue(Ident id, String catalog, String schema,
                                       boolean routines, boolean code) {
        if (!data.has("backup_epilogue")) {
            return base.backupEpilogue(id, catalog, schema, routines, code);
        }
        return backupLines("backup_epilogue", id, catalog, schema, routines, code);
    }

    /**
     * The lines of {@code backup_prologue} or {@code backup_epilogue}: each a
     * template, or a template with the {@code when} it is written under
     * ({@code routines}, {@code code}); {@code {catalog}} and {@code {schema}}
     * are the backup's, quoted, and a line naming one the backup has not is
     * left out.
     */
    private List<String> backupLines(String key, Ident id, String catalog, String schema,
                                     boolean routines, boolean code) {
        JsonElement e = data.get(key);
        if (e == null || e.isJsonNull()) {
            return List.of();
        }
        if (!e.isJsonArray()) {
            throw shape(key);
        }
        Map<String, String> holes = new HashMap<>();
        holes.put("catalog", catalog == null || catalog.isEmpty() ? null : id.q(catalog));
        holes.put("schema", schema == null || schema.isEmpty() ? null : id.q(schema));
        List<String> out = new ArrayList<>();
        for (JsonElement line : e.getAsJsonArray()) {
            String template;
            if (line.isJsonObject()) {
                JsonObject o = line.getAsJsonObject();
                String when = text(o, "when");
                if ("routines".equals(when) && !routines || "code".equals(when) && !code) {
                    continue;
                }
                template = text(o, "sql");
            } else {
                template = line.getAsString();
            }
            String rendered = template == null ? null : ProfileTemplate.render(template, holes);
            if (rendered != null) {
                out.add(rendered);
            }
        }
        return out;
    }

    @Override
    public Ddl.Script resumeIdentity(Connection conn, Ident id, String catalog, String schema,
                                     String table, Ddl.Script script) {
        return base.resumeIdentity(conn, id, catalog, schema, table, script);
    }

    @Override
    public String sequenceValue(Connection conn, Ident id, String catalog, String schema,
                                String name, String listed) {
        return base.sequenceValue(conn, id, catalog, schema, name, listed);
    }

    @Override
    public String resumeSequence(Connection conn, String schema, String name, String ddl) {
        return base.resumeSequence(conn, schema, name, ddl);
    }

    @Override
    public String sqlMode(Connection conn, String catalog, String name, String kind) {
        return base.sqlMode(conn, catalog, name, kind);
    }

    @Override
    public List<String> overloadTexts(Connection conn, String schema, String name) {
        return base.overloadTexts(conn, schema, name);
    }

    @Override
    public List<String> routineDrops(Connection conn, Ident id, String schema, String name,
                                     boolean procedure) {
        return base.routineDrops(conn, id, schema, name, procedure);
    }

    @Override
    public List<String> counters(DatabaseMetaData dbm, Ident id, List<Sequence> sequences,
                                 List<Table> tables, boolean data) throws SQLException {
        return base.counters(dbm, id, sequences, tables, data);
    }

    @Override
    public Set<String> textColumns(Connection conn, String schema, String table) {
        return base.textColumns(conn, schema, table);
    }

    @Override
    public String textColumn(String quoted) {
        return base.textColumn(quoted);
    }

    @Override
    public RowFence rowFence(Connection conn, Ident id, String schema, String table,
                             String qualified, List<String> identities) throws SQLException {
        if (!data.has("identity_insert")) {
            return base.rowFence(conn, id, schema, table, qualified, identities);
        }
        JsonObject fence = query(data, "identity_insert");
        if (fence == null || identities.isEmpty()) {
            return RowFence.NONE;
        }
        Map<String, String> holes = Map.of("table", qualified);
        String before = text(fence, "before");
        String after = text(fence, "after");
        return new RowFence() {
            @Override
            public List<String> before() {
                return rendered(before, holes);
            }

            @Override
            public List<String> after() {
                return rendered(after, holes);
            }
        };
    }

    private static List<String> rendered(String template, Map<String, String> holes) {
        if (template == null) {
            return List.of();
        }
        String line = ProfileTemplate.render(template, holes);
        return line == null ? List.of() : List.of(line);
    }

    // ---------------------------------------------------------------- extract

    @Override
    public boolean overridesSystemValue() {
        return flag("overrides_system_value", base.overridesSystemValue());
    }
}
