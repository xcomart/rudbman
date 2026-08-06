package comart.rudbman.bridge.job;

import com.google.gson.JsonArray;
import com.google.gson.JsonElement;
import com.google.gson.JsonObject;
import comart.rudbman.bridge.BridgeException;
import comart.rudbman.bridge.Json;
import comart.rudbman.bridge.Session;
import comart.rudbman.bridge.meta.Ddl;
import comart.rudbman.bridge.meta.Dialect;
import comart.rudbman.bridge.meta.Ident;
import comart.rudbman.bridge.template.TemplateManager;

import java.io.IOException;
import java.nio.charset.Charset;
import java.nio.charset.StandardCharsets;
import java.nio.file.Files;
import java.nio.file.Path;
import java.nio.file.Paths;
import java.sql.Connection;
import java.sql.DatabaseMetaData;
import java.sql.ResultSet;
import java.sql.ResultSetMetaData;
import java.sql.SQLException;
import java.sql.Statement;
import java.util.ArrayList;
import java.util.HashMap;
import java.util.LinkedHashMap;
import java.util.List;
import java.util.Map;

/**
 * {@code kind: "extract"} - a SQL script, a CSV file or a templated file built
 * from a list of database objects (architecture.md 6).
 *
 * <p>The file is written by the JVM, not handed to Rust. That is the whole point
 * of the data plane: the rows are already here, and moving them across JNI so
 * that the other side can write them to disk would be work done for nothing.
 *
 * <h2>Request</h2>
 *
 * <pre>
 * { kind: "extract",
 *   objects: [ {catalog?, schema?, name} ],
 *   output:  { path, charset: "UTF-8", newline: "\n" | "\r\n" },
 *   ddl:     { include: false, include_drop: false, constraints: "alter" | "inline" },
 *   data:    { include: false, mode: "insert" | "csv" | "template",
 *              template_path?, insert_batch_rows: 1, where? } }
 * </pre>
 *
 * <h2>Ordering</h2>
 *
 * <p>The script is written as: every {@code DROP} (in reverse object order),
 * then every {@code CREATE}, then every foreign key as
 * {@code ALTER TABLE … ADD CONSTRAINT}, then the data. Foreign keys go last
 * because two tables that reference each other cannot be created in any order at
 * all, and such schemas exist. Drops go first and in reverse for the mirror
 * reason: a dependency chain drops from the leaf up.
 *
 * <h2>What is dialect specific, and where it stops</h2>
 *
 * <p>Identifier quoting comes from {@link Ident}, so generated {@code INSERT}s
 * and generated DDL agree. Value rendering lives in {@link Literals} and the
 * statement shapes in {@link Scripts}, shared with {@link BackupJob} so that the
 * two produce the same bytes for the same rows.
 */
public final class ExtractJob extends Jobs.Job {

    /** Rows requested per round trip while streaming a table. */
    private static final int FETCH_SIZE = 1000;

    /** How often the byte counter is pulled out of the output stream. */
    private static final int BYTE_SYNC_ROWS = 256;

    /** One object to extract. */
    private static final class Obj {
        final String catalog;
        final String schema;
        final String name;

        Obj(String catalog, String schema, String name) {
            this.catalog = catalog;
            this.schema = schema;
            this.name = name;
        }

        /** @return a name for phase strings; not SQL, so it is never quoted */
        String display() {
            StringBuilder sb = new StringBuilder();
            if (schema != null && !schema.isEmpty()) {
                sb.append(schema).append('.');
            } else if (catalog != null && !catalog.isEmpty()) {
                sb.append(catalog).append('.');
            }
            return sb.append(name).toString();
        }
    }

    private final List<Obj> objects = new ArrayList<>();
    private final Path path;
    private final Charset charset;
    private final String newline;

    private final boolean ddlInclude;
    private final boolean includeDrop;
    private final boolean splitForeignKeys;

    private final boolean dataInclude;
    private final String mode;
    private final String templatePath;
    private final int insertBatchRows;
    private final String where;

    /**
     * Validates a job specification.
     *
     * <p>Everything that can be decided without the database is decided here, on
     * the caller's thread, so that {@code JOB_START} can answer with an ERROR
     * envelope. A client should learn that its request is malformed from the call
     * that made it, not from a poll two hundred milliseconds later.
     *
     * @param session the session to run on
     * @param spec    the {@code JOB_START} body
     */
    ExtractJob(Session session, JsonObject spec) {
        super(session, "extract");

        JsonArray objs = Json.arr(spec, "objects");
        if (objs == null || objs.size() == 0) {
            throw new BridgeException("protocol", "extract requires a non-empty 'objects'");
        }
        for (JsonElement e : objs) {
            if (!e.isJsonObject()) {
                throw new BridgeException("protocol", "each 'objects' entry must be an object");
            }
            JsonObject o = e.getAsJsonObject();
            String name = Json.str(o, "name");
            if (name == null || name.isEmpty()) {
                throw new BridgeException("protocol", "each 'objects' entry requires a 'name'");
            }
            objects.add(new Obj(Json.str(o, "catalog"), Json.str(o, "schema"), name));
        }

        JsonObject output = Json.obj(spec, "output");
        String p = output == null ? null : Json.str(output, "path");
        if (p == null || p.isEmpty()) {
            throw new BridgeException("protocol", "extract requires 'output.path'");
        }
        path = Paths.get(p);
        String cs = output == null ? null : Json.str(output, "charset");
        try {
            charset = cs == null || cs.isEmpty() ? StandardCharsets.UTF_8 : Charset.forName(cs);
        } catch (RuntimeException ex) {
            throw new BridgeException("protocol", "unsupported output charset: " + cs, ex);
        }
        String nl = Json.str(output, "newline");
        if (nl == null || nl.isEmpty()) {
            nl = "\n";
        }
        if (!"\n".equals(nl) && !"\r\n".equals(nl)) {
            throw new BridgeException("protocol",
                    "output.newline must be \"\\n\" or \"\\r\\n\"");
        }
        newline = nl;

        JsonObject ddl = Json.obj(spec, "ddl");
        ddlInclude = ddl != null && Json.bool(ddl, "include", false);
        includeDrop = ddl != null && Json.bool(ddl, "include_drop", false);
        String constraints = ddl == null ? null : Json.str(ddl, "constraints");
        if (constraints == null || constraints.isEmpty()) {
            // The design's rule is that foreign keys always move to the end, so
            // that is the default; "inline" exists for the user who wants the
            // server's own verbatim DDL and knows the schema has no cycles.
            constraints = "alter";
        }
        if (!"alter".equals(constraints) && !"inline".equals(constraints)) {
            throw new BridgeException("protocol",
                    "ddl.constraints must be 'alter' or 'inline', not '" + constraints + "'");
        }
        splitForeignKeys = "alter".equals(constraints);

        JsonObject data = Json.obj(spec, "data");
        dataInclude = data != null && Json.bool(data, "include", false);
        String m = data == null ? null : Json.str(data, "mode");
        mode = m == null || m.isEmpty() ? "insert" : m;
        if (!"insert".equals(mode) && !"csv".equals(mode) && !"template".equals(mode)) {
            throw new BridgeException("protocol",
                    "data.mode must be 'insert', 'csv' or 'template', not '" + mode + "'");
        }
        templatePath = data == null ? null : Json.str(data, "template_path");
        if (dataInclude && "template".equals(mode)
                && (templatePath == null || templatePath.isEmpty())) {
            throw new BridgeException("protocol",
                    "data.mode 'template' requires 'data.template_path'");
        }
        int batch = data == null ? 1 : Json.i32(data, "insert_batch_rows", 1);
        insertBatchRows = Math.max(1, batch);

        where = data == null ? null : Json.str(data, "where");
        if (where != null && !where.isEmpty() && objects.size() != 1) {
            // A WHERE clause names columns, and columns belong to one table. The
            // alternative reading - apply it to all of them - is a way to write a
            // half-empty script without noticing.
            throw new BridgeException("protocol",
                    "data.where is only valid when 'objects' holds exactly one entry");
        }

        if (!ddlInclude && !dataInclude) {
            throw new BridgeException("protocol",
                    "extract must include at least one of ddl or data");
        }
    }

    @Override
    protected void run() throws Exception {
        Path parent = path.toAbsolutePath().getParent();
        if (parent != null) {
            Files.createDirectories(parent);
        }
        try (ScriptOut out = new ScriptOut(Files.newOutputStream(path), charset, newline, false)) {
            try {
                if (ddlInclude) {
                    writeDdl(out);
                }
                if (dataInclude && !shouldStop()) {
                    writeData(out);
                }
            } finally {
                // A cancelled or failed job still reports how much of the file it
                // managed to write, because the file is still there.
                try {
                    out.flush();
                } catch (IOException ignored) {
                    // The interesting failure is the one already propagating.
                }
                syncBytes(out);
            }
        }
    }

    // ------------------------------------------------------------------- ddl

    private void writeDdl(ScriptOut out) throws Exception {
        phase("ddl");
        runLocked(() -> {
            Connection conn = session().connection();
            DatabaseMetaData dbm = session().metaData();
            Dialect dialect = Dialect.of(dbm);
            Ident id = Ident.of(dbm);

            if (includeDrop) {
                // Reverse order, because a dependency chain has to be dropped
                // from the leaf end. It is a heuristic, not a solution: a cycle
                // still needs the constraints dropped first, and nothing here
                // does that.
                for (int i = objects.size() - 1; i >= 0; i--) {
                    Obj o = objects.get(i);
                    out.line(Scripts.dropStatement(dialect,
                            id.qualify(o.catalog, o.schema, o.name)));
                }
                out.line("");
            }

            List<String> alters = new ArrayList<>();
            for (Obj o : objects) {
                if (shouldStop()) {
                    return null;
                }
                Ddl.Script sc = Ddl.script(conn, dbm, o.catalog, o.schema, o.name,
                        splitForeignKeys);
                out.block(sc.creates);
                out.line("");
                alters.addAll(sc.alters);
            }
            for (String alter : alters) {
                out.line(alter + ";");
            }
            if (!alters.isEmpty()) {
                out.line("");
            }
            return null;
        });
        syncBytes(out);
    }

    // ------------------------------------------------------------------ data

    private void writeData(ScriptOut out) throws Exception {
        TemplateManager template = null;
        if ("template".equals(mode)) {
            // Read once, parsed once, applied per row. The caller resolves the
            // path; the bridge has no idea where a configuration directory is.
            String text = new String(Files.readAllBytes(Paths.get(templatePath)),
                    StandardCharsets.UTF_8);
            template = new TemplateManager(text, new HashMap<>());
        }
        for (Obj o : objects) {
            if (shouldStop()) {
                return;
            }
            phase("data:" + o.display());
            final TemplateManager tpl = template;
            runLocked(() -> {
                writeTable(out, o, tpl);
                return null;
            });
            syncBytes(out);
        }
    }

    private void writeTable(ScriptOut out, Obj o, TemplateManager template) throws Exception {
        DatabaseMetaData dbm = session().metaData();
        Dialect dialect = Dialect.of(dbm);
        Ident id = Ident.of(dbm);
        String qualified = id.qualify(o.catalog, o.schema, o.name);
        String sql = "SELECT * FROM " + qualified
                + (where == null || where.isEmpty() ? "" : " WHERE " + where);

        try (Statement st = session().connection().createStatement()) {
            // A hint only: several drivers ignore it, and PostgreSQL honours it
            // only with auto-commit off. Without it, a driver that materialises
            // the whole result would decide the heap ceiling for this job.
            try {
                st.setFetchSize(FETCH_SIZE);
            } catch (SQLException ignored) {
                // A driver entitled to refuse the hint; nothing is lost.
            }
            inFlight(st);
            // A cancel that raced the statement's creation was answered by
            // Statement.cancel on a statement that was not yet executing, which
            // JDBC lets a driver treat as a no-op — H2 does. The flag is the
            // durable half of the request, so it gets one last look before
            // execution begins; without it, a query the driver materialises
            // (a view over a generator, say) would run to completion with the
            // cancel already lost.
            if (shouldStop()) {
                return;
            }
            try (ResultSet rs = st.executeQuery(sql)) {
                ResultSetMetaData md = rs.getMetaData();
                int n = md.getColumnCount();
                String[] names = new String[n];
                String[] typeNames = new String[n];
                int[] types = new int[n];
                for (int i = 1; i <= n; i++) {
                    names[i - 1] = md.getColumnLabel(i);
                    typeNames[i - 1] = md.getColumnTypeName(i);
                    types[i - 1] = md.getColumnType(i);
                }
                switch (mode) {
                    case "csv":
                        writeCsv(out, rs, names, types);
                        break;
                    case "template":
                        writeTemplate(out, rs, o, qualified, names, typeNames, types, dialect,
                                template);
                        break;
                    default:
                        Scripts.writeInserts(this, out, rs, qualified, names, types, dialect, id,
                                insertBatchRows);
                }
            } finally {
                inFlight(null);
            }
        }
    }

    /**
     * RFC 4180 shaped CSV: comma separated, a field quoted whenever it holds a
     * comma, a quote or a line break, embedded quotes doubled.
     *
     * <p>A header row of column names is written first.
     *
     * <p><strong>NULL is written as an empty unquoted field and the empty string
     * as {@code ""}.</strong> Plain CSV has no null, and the two would otherwise
     * be indistinguishable; this is the same convention PostgreSQL's
     * {@code COPY … WITH CSV} uses. A reader that strips quotes without tracking
     * whether they were there will still see both as empty, which is the best
     * plain CSV can do.
     *
     * <p>Line breaks inside a value are written through unchanged, so the file's
     * record separator and its data can differ. Rewriting them would be data
     * loss.
     */
    private void writeCsv(ScriptOut out, ResultSet rs, String[] names, int[] types)
            throws Exception {
        StringBuilder header = new StringBuilder();
        for (String name : names) {
            if (header.length() > 0) {
                header.append(',');
            }
            header.append(csvField(name));
        }
        out.line(header.toString());

        long sinceSync = 0;
        while (rs.next()) {
            if (shouldStop()) {
                break;
            }
            StringBuilder row = new StringBuilder();
            for (int i = 1; i <= names.length; i++) {
                if (i > 1) {
                    row.append(',');
                }
                String v = Literals.text(rs, i, types[i - 1]);
                if (v != null) {
                    row.append(csvField(v));
                }
            }
            out.line(row.toString());
            addRows(1);
            if (++sinceSync >= BYTE_SYNC_ROWS) {
                sinceSync = 0;
                syncBytes(out);
            }
        }
    }

    private static String csvField(String v) {
        // The empty string is quoted although nothing forces it to be: that is
        // the only mark left to tell it apart from NULL, which is written as
        // nothing at all.
        if (!v.isEmpty() && v.indexOf(',') < 0 && v.indexOf('"') < 0 && v.indexOf('\n') < 0
                && v.indexOf('\r') < 0) {
            return v;
        }
        return '"' + v.replace("\"", "\"\"") + '"';
    }

    /**
     * Renders one model per row through the inherited template engine.
     *
     * <p>The model is a map holding:
     *
     * <ul>
     *   <li>{@code table}, {@code schema}, {@code catalog}, {@code qualified}
     *       and {@code row_no} - where the row came from and which one it is;</li>
     *   <li>{@code columns} - a list for {@code ${for:item=columns}}, each entry
     *       carrying {@code name}, {@code value}, {@code literal},
     *       {@code type_name} and {@code jdbc_type};</li>
     *   <li>every column under its own name, so {@code ${CUSTOMER_ID}} works.</li>
     * </ul>
     *
     * <p>Columns are added last and therefore win a name clash: a table with a
     * column called {@code table} shadows the fixed key, and the loop is then the
     * only way to reach either. A dotted key is a processor chain in this syntax,
     * not a path, so there is no {@code ${meta.table}} to fall back on.
     */
    private void writeTemplate(ScriptOut out, ResultSet rs, Obj o, String qualified, String[] names,
                               String[] typeNames, int[] types, Dialect dialect,
                               TemplateManager template) throws Exception {
        long sinceSync = 0;
        long rowNo = 0;
        while (rs.next()) {
            if (shouldStop()) {
                break;
            }
            rowNo++;
            List<Map<String, Object>> columns = new ArrayList<>(names.length);
            Map<String, Object> model = new LinkedHashMap<>();
            model.put("table", o.name);
            model.put("schema", o.schema);
            model.put("catalog", o.catalog);
            model.put("qualified", qualified);
            model.put("row_no", rowNo);
            model.put("columns", columns);
            for (int i = 1; i <= names.length; i++) {
                String v = Literals.text(rs, i, types[i - 1]);
                Map<String, Object> col = new LinkedHashMap<>();
                col.put("name", names[i - 1]);
                col.put("value", v);
                col.put("literal", Literals.literal(rs, i, types[i - 1], dialect));
                col.put("type_name", typeNames[i - 1]);
                col.put("jdbc_type", types[i - 1]);
                columns.add(col);
                model.put(names[i - 1], v);
            }

            out.raw(template.applyMapper(model));
            addRows(1);
            if (++sinceSync >= BYTE_SYNC_ROWS) {
                sinceSync = 0;
                syncBytes(out);
            }
        }
    }

    // ---------------------------------------------------------------- output

    private void syncBytes(ScriptOut out) {
        addBytes(out.unreported());
    }
}
