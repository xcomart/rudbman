package comart.rudbman.bridge.job;

import com.google.gson.JsonObject;
import comart.rudbman.bridge.BridgeException;
import comart.rudbman.bridge.Json;
import comart.rudbman.bridge.Session;
import comart.rudbman.bridge.meta.Ddl;
import comart.rudbman.bridge.meta.Dialect;
import comart.rudbman.bridge.meta.Ident;

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
import java.util.Collections;
import java.util.Comparator;
import java.util.LinkedHashMap;
import java.util.LinkedHashSet;
import java.util.List;
import java.util.Map;
import java.util.Set;
import java.util.logging.Level;
import java.util.logging.Logger;

/**
 * {@code kind: "backup"} - every table of a scope written to one replayable
 * script file (architecture.md 6).
 *
 * <h2>Request</h2>
 *
 * <pre>
 * { kind: "backup",
 *   scope:    {catalog?, schema?},
 *   output:   {path, charset, newline},
 *   compress: "none" | "gzip",
 *   ddl:      {include, include_drop, constraints},
 *   data:     {include, insert_batch_rows} }
 * </pre>
 *
 * <h2>What a backup is, and is not</h2>
 *
 * <p>A backup is <strong>an extract without the object list</strong>: the scope's
 * {@code TABLE} entries are enumerated in name order and written through the same
 * core - every {@code CREATE}, then every foreign key as {@code ALTER}, then the
 * data. Views and routines are not written, because the goal is a data backup
 * that replays, not a schema dump.
 *
 * <p>The data format is {@code INSERT} and only {@code INSERT}. Several tables
 * share one file, and CSV has no notion of a table boundary while a template
 * means something different for every table. Those formats are the extract's
 * job, one table at a time.
 *
 * <p>With {@code compress: "gzip"} the byte counter sits under the compressor,
 * so the reported {@code bytes} is the file's size. It is only exact once the
 * stream is closed, which is why this job takes its final reading after closing
 * rather than before.
 */
public final class BackupJob extends Jobs.Job {

    private static final Logger LOG = Logger.getLogger(BackupJob.class.getName());

    /** Rows requested per round trip while streaming a table. */
    private static final int FETCH_SIZE = 1000;

    /** One table in the scope. */
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

    private final String scopeCatalog;
    private final String scopeSchema;

    private final Path path;
    private final Charset charset;
    private final String newline;
    private final boolean gzip;

    private final boolean ddlInclude;
    private final boolean includeDrop;
    private final boolean splitForeignKeys;

    private final boolean dataInclude;
    private final int insertBatchRows;

    /**
     * Validates a job specification.
     *
     * <p>Everything that can be decided without the database is decided here, on
     * the caller's thread, so that {@code JOB_START} can answer with an ERROR
     * envelope rather than handing back a job that fails on its first poll.
     *
     * @param session the session to run on
     * @param spec    the {@code JOB_START} body
     */
    BackupJob(Session session, JsonObject spec) {
        super(session, "backup");

        JsonObject scope = Json.obj(spec, "scope");
        scopeCatalog = scope == null ? null : Json.str(scope, "catalog");
        scopeSchema = scope == null ? null : Json.str(scope, "schema");

        JsonObject output = Json.obj(spec, "output");
        String p = output == null ? null : Json.str(output, "path");
        if (p == null || p.isEmpty()) {
            throw new BridgeException("protocol", "backup requires 'output.path'");
        }
        path = Paths.get(p);
        String cs = Json.str(output, "charset");
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

        String compress = Json.str(spec, "compress");
        if (compress == null || compress.isEmpty()) {
            compress = "none";
        }
        if (!"none".equals(compress) && !"gzip".equals(compress)) {
            throw new BridgeException("protocol",
                    "compress must be 'none' or 'gzip', not '" + compress + "'");
        }
        gzip = "gzip".equals(compress);

        JsonObject ddl = Json.obj(spec, "ddl");
        ddlInclude = ddl != null && Json.bool(ddl, "include", false);
        includeDrop = ddl != null && Json.bool(ddl, "include_drop", false);
        String constraints = ddl == null ? null : Json.str(ddl, "constraints");
        if (constraints == null || constraints.isEmpty()) {
            constraints = "alter";
        }
        if (!"alter".equals(constraints) && !"inline".equals(constraints)) {
            throw new BridgeException("protocol",
                    "ddl.constraints must be 'alter' or 'inline', not '" + constraints + "'");
        }
        splitForeignKeys = "alter".equals(constraints);

        JsonObject data = Json.obj(spec, "data");
        dataInclude = data != null && Json.bool(data, "include", false);
        insertBatchRows = Math.max(1, data == null ? 1 : Json.i32(data, "insert_batch_rows", 1));

        if (!ddlInclude && !dataInclude) {
            throw new BridgeException("protocol",
                    "backup must include at least one of ddl or data");
        }
    }

    @Override
    protected void run() throws Exception {
        Path parent = path.toAbsolutePath().getParent();
        if (parent != null) {
            Files.createDirectories(parent);
        }
        ScriptOut out = new ScriptOut(Files.newOutputStream(path), charset, newline, gzip);
        boolean closed = false;
        try {
            List<Obj> tables = runLocked(this::listTables);
            if (ddlInclude && !shouldStop()) {
                writeDdl(out, tables);
            }
            if (dataInclude && !shouldStop()) {
                writeData(out, runLocked(() -> dependencyOrder(tables)));
            }
            // Closed inside the try so that a failure to write the last buffer -
            // or the gzip trailer - is reported as the job's failure rather than
            // swallowed by the cleanup below.
            out.close();
            closed = true;
        } finally {
            if (!closed) {
                try {
                    out.close();
                } catch (IOException ignored) {
                    // The interesting failure is the one already propagating; a
                    // cancelled or failed job still keeps its partial file.
                }
            }
            // After the close, not before: a compressed stream only knows its
            // final size once the trailer is out.
            addBytes(out.unreported());
        }
    }

    /**
     * Enumerates the scope's tables.
     *
     * <p>Columns are read by position rather than by label: the three name
     * columns are the first three of {@code getTables} in every JDBC version,
     * and a driver that omits one of the optional labels would otherwise fail
     * the whole job.
     *
     * <p><strong>The catalog written into the script is the one the request
     * asked for, not the one the driver reports.</strong> A driver that answers
     * with the live database's name - H2 does - would otherwise produce a script
     * that only restores into a database of that same name, which defeats the
     * purpose. A caller whose product puts the schema in the catalog slot (MySQL)
     * names it in {@code scope.catalog} and gets it back.
     */
    private List<Obj> listTables() throws SQLException {
        List<Obj> tables = new ArrayList<>();
        DatabaseMetaData dbm = session().metaData();
        try (ResultSet rs = dbm.getTables(scopeCatalog, scopeSchema, "%",
                new String[]{"TABLE"})) {
            while (rs.next()) {
                tables.add(new Obj(scopeCatalog, rs.getString(2), rs.getString(3)));
            }
        }
        tables.sort(Comparator
                .comparing((Obj o) -> o.name == null ? "" : o.name)
                .thenComparing(o -> o.schema == null ? "" : o.schema));
        return tables;
    }

    /**
     * Reorders the tables so that a table's data follows the data of the tables
     * it references.
     *
     * <p>The foreign keys are added before the data, exactly as in an extract,
     * so the rows have to arrive in an order the constraints accept. An extract
     * gets that ordering for free - the caller listed the objects - but a backup
     * enumerates them, and enumeration is alphabetical: {@code CHILD} comes
     * before {@code PARENT} and its rows would be rejected. Nothing else about
     * the file changes; the {@code CREATE}s stay in name order and the keys stay
     * where they were.
     *
     * <p>A reference cycle has no such order at all. Those tables are emitted in
     * name order and the script has to be run past the errors, which is the same
     * limitation {@code include_drop} already carries.
     */
    private List<Obj> dependencyOrder(List<Obj> tables) throws SQLException {
        DatabaseMetaData dbm = session().metaData();
        Map<String, Obj> byKey = new LinkedHashMap<>();
        for (Obj o : tables) {
            byKey.put(key(o.schema, o.name), o);
        }
        Map<String, List<String>> references = new LinkedHashMap<>();
        for (Obj o : tables) {
            String self = key(o.schema, o.name);
            List<String> refs = new ArrayList<>();
            try (ResultSet rs = dbm.getImportedKeys(o.catalog, o.schema, o.name)) {
                while (rs.next()) {
                    // Columns 2 and 3 are PKTABLE_SCHEM and PKTABLE_NAME.
                    String ref = key(rs.getString(2), rs.getString(3));
                    if (!ref.equals(self) && byKey.containsKey(ref) && !refs.contains(ref)) {
                        refs.add(ref);
                    }
                }
            } catch (SQLException e) {
                // A driver that has no foreign key metadata for a table leaves
                // it unconstrained here; name order is then as good as any.
                LOG.log(Level.FINE, "no imported key metadata for " + self, e);
            }
            references.put(self, refs);
        }

        List<Obj> ordered = new ArrayList<>(tables.size());
        Set<String> done = new LinkedHashSet<>();
        Set<String> onPath = new LinkedHashSet<>();
        for (String k : byKey.keySet()) {
            visit(k, byKey, references, done, onPath, ordered);
        }
        return ordered;
    }

    private static void visit(String k, Map<String, Obj> byKey,
                              Map<String, List<String>> references, Set<String> done,
                              Set<String> onPath, List<Obj> ordered) {
        if (done.contains(k) || !onPath.add(k)) {
            // Already placed, or this is the edge that closes a cycle: stop
            // rather than recurse forever, and let name order decide.
            return;
        }
        for (String ref : references.getOrDefault(k, Collections.emptyList())) {
            visit(ref, byKey, references, done, onPath, ordered);
        }
        onPath.remove(k);
        if (done.add(k)) {
            ordered.add(byKey.get(k));
        }
    }

    private static String key(String schema, String name) {
        return (schema == null ? "" : schema) + '.' + (name == null ? "" : name);
    }

    // ------------------------------------------------------------------- ddl

    private void writeDdl(ScriptOut out, List<Obj> tables) throws Exception {
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
                for (int i = tables.size() - 1; i >= 0; i--) {
                    Obj o = tables.get(i);
                    out.line(Scripts.dropStatement(dialect,
                            id.qualify(o.catalog, o.schema, o.name)));
                }
                out.line("");
            }

            List<String> alters = new ArrayList<>();
            for (Obj o : tables) {
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
        addBytes(out.unreported());
    }

    // ------------------------------------------------------------------ data

    private void writeData(ScriptOut out, List<Obj> tables) throws Exception {
        for (Obj o : tables) {
            if (shouldStop()) {
                return;
            }
            phase("data:" + o.display());
            runLocked(() -> {
                writeTable(out, o);
                return null;
            });
            addBytes(out.unreported());
        }
    }

    private void writeTable(ScriptOut out, Obj o) throws Exception {
        DatabaseMetaData dbm = session().metaData();
        Dialect dialect = Dialect.of(dbm);
        Ident id = Ident.of(dbm);
        String qualified = id.qualify(o.catalog, o.schema, o.name);

        try (Statement st = session().connection().createStatement()) {
            try {
                st.setFetchSize(FETCH_SIZE);
            } catch (SQLException ignored) {
                // A driver entitled to refuse the hint; nothing is lost.
            }
            inFlight(st);
            // The flag is the durable half of a cancel: a Statement.cancel that
            // landed before this statement began executing may have been a no-op.
            if (shouldStop()) {
                return;
            }
            try (ResultSet rs = st.executeQuery("SELECT * FROM " + qualified)) {
                ResultSetMetaData md = rs.getMetaData();
                int n = md.getColumnCount();
                String[] names = new String[n];
                int[] types = new int[n];
                for (int i = 1; i <= n; i++) {
                    names[i - 1] = md.getColumnLabel(i);
                    types[i - 1] = md.getColumnType(i);
                }
                Scripts.writeInserts(this, out, rs, qualified, names, types, dialect, id,
                        insertBatchRows);
            } finally {
                inFlight(null);
            }
        }
    }
}
