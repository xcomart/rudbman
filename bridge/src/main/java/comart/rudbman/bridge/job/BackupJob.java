package comart.rudbman.bridge.job;

import com.google.gson.JsonElement;
import com.google.gson.JsonObject;
import comart.rudbman.bridge.BridgeException;
import comart.rudbman.bridge.Json;
import comart.rudbman.bridge.Session;
import comart.rudbman.bridge.meta.Ddl;
import comart.rudbman.bridge.meta.Dialect;
import comart.rudbman.bridge.meta.Ident;
import comart.rudbman.bridge.meta.PgSchema;
import comart.rudbman.bridge.meta.Routines;
import comart.rudbman.bridge.meta.ScriptFailure;
import comart.rudbman.bridge.meta.Sequences;
import comart.rudbman.bridge.meta.Triggers;
import comart.rudbman.bridge.meta.Vendor;

import java.io.IOException;
import java.nio.charset.Charset;
import java.nio.charset.StandardCharsets;
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
import java.util.HashSet;
import java.util.LinkedHashMap;
import java.util.LinkedHashSet;
import java.util.List;
import java.util.Locale;
import java.util.Map;
import java.util.Set;
import java.util.logging.Level;
import java.util.logging.Logger;
import java.util.regex.Pattern;

/**
 * {@code kind: "backup"} - a scope written to one replayable script file
 * (architecture.md 6).
 *
 * <h2>Request</h2>
 *
 * <pre>
 * { kind: "backup",
 *   scope:    {catalog?, schema?},
 *   output:   {path, charset, newline},
 *   compress: "none" | "gzip",
 *   ddl:      {include, include_drop, constraints},
 *   objects:  {sequences, views, routines, triggers},
 *   data:     {include, insert_batch_rows} }
 * </pre>
 *
 * <h2>What a backup writes</h2>
 *
 * <p>The scope's {@code TABLE} entries are enumerated in name order and their
 * DDL written through {@link Ddl#script} - the server's own text wherever it
 * can be split from the foreign keys, a reconstruction elsewhere. The kinds
 * {@code objects} switches on (all off when absent, which is a backup of the
 * tables alone) are written beside them, each through the same
 * {@code DESCRIBE kind: "ddl"} the explorer shows, so a backup says of an
 * object what its DDL tab says.
 *
 * <p>The order is the one a replay needs, not the one the catalog lists:
 *
 * <ol>
 *   <li>the product's session settings ({@link Ddl#scriptPrologue}, and
 *       PostgreSQL's {@code check_function_bodies}, which pg_dump also turns
 *       off so a function may name what is created after it);</li>
 *   <li>the {@code DROP}s, when asked for, dependents first;</li>
 *   <li>on PostgreSQL the schema's extensions and its enum and domain types
 *       ({@link PgSchema});</li>
 *   <li>sequences, which a table's default may draw on;</li>
 *   <li>every table;</li>
 *   <li>functions and procedures (and Oracle packages), then views, ordered
 *       so a view comes after the views its text names;</li>
 *   <li>the rows, in foreign-key order, with each product's identity
 *       handling around them;</li>
 *   <li>the foreign keys, after the rows as pg_dump has them, so that two
 *       tables referencing each other load at all (straight after the tables
 *       when there are no rows);</li>
 *   <li>the counters - identity columns and sequences - set to where the
 *       source has them;</li>
 *   <li>materialized views, which are filled when created and so must see the
 *       rows;</li>
 *   <li>triggers last, so that loading the rows fires none of them.</li>
 * </ol>
 *
 * <p>A body with semicolons in it is written the way the product's own client
 * reads a script ({@link Scripts#module}). An object whose DDL cannot be read -
 * no privilege, a kind the server keeps no text for - is left out and reported
 * in the progress {@code errors}, without failing the job: one unreadable
 * procedure is not a reason to lose the backup of everything else.
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

    /** One object in the scope. */
    private static final class Obj {
        final String catalog;
        final String schema;
        final String name;
        /**
         * {@code table}, {@code sequence}, {@code function}, {@code procedure},
         * {@code package}, {@code view}, {@code materialized view} or
         * {@code trigger}.
         */
        final String kind;
        /** The {@code CREATE} text of anything but a table, once read. */
        String ddl;
        /** A table's DDL, once read. */
        Ddl.Script script;
        /** A sequence's last value handed out, as its catalog reports it. */
        String currentValue;
        /** A sequence's increment. */
        String increment;
        /** A sequence's start. */
        String start;
        /**
         * The {@code DROP}s that take the object off, where a plain one by name
         * cannot: a Db2 routine name with overloads, dropped by signature.
         */
        List<String> drops;
        /**
         * The {@code sql_mode} a MySQL or MariaDB routine or trigger was
         * created under, which it keeps and runs under.
         */
        String sqlMode;
        /** A table's own foreign keys by name, for the DROPs to take off first. */
        final List<String> foreignKeys = new ArrayList<>();

        Obj(String catalog, String schema, String name, String kind) {
            this.catalog = catalog;
            this.schema = schema;
            this.name = name;
            this.kind = kind;
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

    /** Everything the scope holds that this backup writes. */
    private static final class Plan {
        final List<Obj> tables = new ArrayList<>();
        final List<Obj> sequences = new ArrayList<>();
        /** Functions, procedures and Oracle packages. */
        final List<Obj> routines = new ArrayList<>();
        /** Views, plain and materialized. */
        final List<Obj> views = new ArrayList<>();
        final List<Obj> triggers = new ArrayList<>();
        /** PostgreSQL's extensions and types; terminated statements. */
        final List<String> preamble = new ArrayList<>();
        /** What drops {@link #preamble}'s types again, dependents first. */
        final List<String> preambleDrops = new ArrayList<>();
        /** What ties PostgreSQL's serial sequences to their columns. */
        final List<String> owned = new ArrayList<>();

        boolean isEmpty() {
            return tables.isEmpty() && sequences.isEmpty() && routines.isEmpty()
                    && views.isEmpty() && triggers.isEmpty() && preamble.isEmpty();
        }
    }

    private final String scopeCatalog;
    private final String scopeSchema;

    private final Path path;
    /** Where the worker writes; set by {@link #run()}, on the worker thread. */
    private OutputFile file;
    private final Charset charset;
    private final String newline;
    private final boolean gzip;

    private final boolean ddlInclude;
    private final boolean includeDrop;
    private final boolean splitForeignKeys;

    private final boolean withSequences;
    private final boolean withViews;
    private final boolean withRoutines;
    private final boolean withTriggers;

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

        // Objects other than tables are DDL; without the DDL there is nothing
        // to write them as.
        JsonObject objects = Json.obj(spec, "objects");
        withSequences = ddlInclude && objects != null && Json.bool(objects, "sequences", false);
        withViews = ddlInclude && objects != null && Json.bool(objects, "views", false);
        withRoutines = ddlInclude && objects != null && Json.bool(objects, "routines", false);
        withTriggers = ddlInclude && objects != null && Json.bool(objects, "triggers", false);

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
        file = OutputFile.create(path);
        ScriptOut out = new ScriptOut(file.open(), charset, newline, gzip);
        boolean closed = false;
        try {
            Plan plan = runLocked(this::plan);
            if (!plan.isEmpty() && !shouldStop()) {
                write(out, plan);
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
                    // The interesting failure is the one already propagating, and
                    // the unfinished file is about to be discarded anyway.
                }
            }
            // After the close, not before: a compressed stream only knows its
            // final size once the trailer is out.
            addBytes(out.unreported());
        }
    }

    @Override
    protected void commit() throws Exception {
        if (shouldStop()) {
            // No cancel was requested, so the session was closed or poisoned
            // underneath the job. That stops the writers at the next row just as
            // a cancel would, and what they left is not the whole script.
            throw new BridgeException("driver",
                    "the session was closed before the backup finished");
        }
        file.commit();
    }

    @Override
    protected void discard() {
        if (file != null) {
            file.discard();
        }
    }

    // ------------------------------------------------------------- the plan

    /** Lists everything the backup writes, under the session lock. */
    private Plan plan() throws SQLException {
        Plan plan = new Plan();
        Connection conn = session().connection();
        DatabaseMetaData dbm = session().metaData();
        Dialect dialect = Dialect.of(dbm);
        Vendor vendor = dialect.vendor();
        Ident id = Ident.of(dbm);
        // An extension's own objects come back with its CREATE EXTENSION;
        // written as the schema's, they would collide with it.
        Set<String> members = vendor.extensionMembers(conn, scopeSchema);

        // A materialized view listed as the table that holds its rows, and
        // what else is listed as a table and comes back with an object of its
        // own: neither is a table to write.
        Set<String> mviews = vendor.materializedViews(conn, scopeSchema);
        Set<String> notTables = vendor.tablesNotWritten(conn, scopeSchema);
        for (Obj o : listTables(dbm)) {
            if (!members.contains(o.name) && !mviews.contains(o.name)
                    && !notTables.contains(o.name)) {
                plan.tables.add(o);
            }
        }
        if (!ddlInclude) {
            return plan;
        }
        for (String[] statement : vendor.backupPreamble(conn, id, scopeCatalog, scopeSchema)) {
            plan.preamble.add(statement[0]);
            if (statement[1] != null) {
                plan.preambleDrops.add(0, statement[1]);
            }
        }
        if (withSequences) {
            for (JsonElement e : Sequences.of(conn, dbm, scopeRequest())) {
                JsonObject s = e.getAsJsonObject();
                String name = Json.str(s, "name");
                if (name == null || members.contains(name)) {
                    continue;
                }
                Obj o = new Obj(scopeCatalog, Json.str(s, "schema"), name, "sequence");
                o.currentValue = Json.str(s, "current_value");
                o.increment = Json.str(s, "increment");
                o.start = Json.str(s, "start_value");
                plan.sequences.add(o);
            }
        }
        if (!plan.sequences.isEmpty()) {
            Set<String> names = new HashSet<>();
            for (Obj s : plan.sequences) {
                names.add(s.name);
            }
            plan.owned.addAll(vendor.ownedSequences(conn, id, scopeSchema, names));
        }
        if (withRoutines) {
            listRoutines(conn, dbm, dialect, members, plan.routines);
        }
        if (withViews) {
            Set<String> indexViews = vendor.indexViews(conn, scopeSchema);
            try (ResultSet rs = dbm.getTables(scopeCatalog, scopeSchema, "%",
                    new String[]{"VIEW", "MATERIALIZED VIEW"})) {
                while (rs.next()) {
                    String name = rs.getString(3);
                    String type = rs.getString(4);
                    if (name != null && !members.contains(name) && !indexViews.contains(name)) {
                        plan.views.add(new Obj(scopeCatalog, rs.getString(2), name,
                                "MATERIALIZED VIEW".equalsIgnoreCase(type)
                                        || mviews.contains(name)
                                        ? "materialized view" : "view"));
                    }
                }
            }
            Set<String> listed = new HashSet<>();
            for (Obj v : plan.views) {
                listed.add(v.name);
            }
            for (String name : mviews) {
                if (!listed.contains(name)) {
                    plan.views.add(new Obj(scopeCatalog, scopeSchema, name, "materialized view"));
                }
            }
        }
        if (withTriggers) {
            for (JsonElement e : Triggers.of(conn, dbm, scopeRequest())) {
                JsonObject t = e.getAsJsonObject();
                String name = Json.str(t, "name");
                if (name != null) {
                    plan.triggers.add(new Obj(scopeCatalog, Json.str(t, "schema"), name,
                            "trigger"));
                }
            }
        }
        // A DDL request answers by name: an overloaded routine's every
        // overload, every PostgreSQL trigger of that name. One entry a name.
        byName(plan.sequences);
        plan.sequences.sort(Comparator.comparing(o -> o.name));
        byName(plan.routines);
        byName(plan.views);
        byName(plan.triggers);
        return plan;
    }

    private JsonObject scopeRequest() {
        JsonObject req = new JsonObject();
        if (scopeCatalog != null) {
            req.addProperty("catalog", scopeCatalog);
        }
        if (scopeSchema != null) {
            req.addProperty("schema", scopeSchema);
        }
        return req;
    }

    /**
     * Functions, then procedures, then - on Oracle - packages. A routine filed
     * under both lists is kept under the first. Oracle's driver lists a
     * package's members as routines whose catalog is the package; the package
     * is written whole instead.
     */
    private void listRoutines(Connection conn, DatabaseMetaData dbm, Dialect dialect,
                              Set<String> members, List<Obj> into) throws SQLException {
        Set<String> seen = new HashSet<>();
        String[] kinds = {"function", "procedure"};
        for (String kind : kinds) {
            Iterable<JsonElement> found;
            try {
                found = "function".equals(kind) ? Routines.functions(dbm, scopeRequest())
                        : Routines.procedures(dbm, scopeRequest());
            } catch (SQLException e) {
                // getFunctions is JDBC 4 and some drivers still refuse it.
                LOG.log(Level.FINE, "no " + kind + " listing", e);
                continue;
            }
            for (JsonElement e : found) {
                JsonObject r = e.getAsJsonObject();
                String name = Json.str(r, "name");
                String catalog = Json.str(r, "catalog");
                if (name == null || members.contains(name)) {
                    continue;
                }
                if (dialect.vendor().packagesRoutines() && catalog != null && !catalog.isEmpty()) {
                    continue;
                }
                if (seen.add(name)) {
                    into.add(new Obj(scopeCatalog, Json.str(r, "schema"), name, kind));
                }
            }
        }
        for (String name : dialect.vendor().packages(conn, scopeSchema)) {
            if (seen.add(name)) {
                into.add(new Obj(scopeCatalog, scopeSchema, name, "package"));
            }
        }
    }

    private static void byName(List<Obj> objects) {
        Set<String> seen = new HashSet<>();
        objects.removeIf(o -> !seen.add(o.name));
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
    private List<Obj> listTables(DatabaseMetaData dbm) throws SQLException {
        List<Obj> tables = new ArrayList<>();
        try (ResultSet rs = dbm.getTables(scopeCatalog, scopeSchema, "%",
                new String[]{"TABLE"})) {
            while (rs.next()) {
                tables.add(new Obj(scopeCatalog, rs.getString(2), rs.getString(3), "table"));
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
                    // Columns 2 and 3 are PKTABLE_SCHEM and PKTABLE_NAME, 12 is
                    // FK_NAME: read in that order, each once.
                    String ref = key(rs.getString(2), rs.getString(3));
                    String fk = rs.getString(12);
                    if (!ref.equals(self) && byKey.containsKey(ref) && !refs.contains(ref)) {
                        refs.add(ref);
                    }
                    if (fk != null && !fk.isEmpty() && !o.foreignKeys.contains(fk)) {
                        o.foreignKeys.add(fk);
                    }
                }
            } catch (SQLException e) {
                // A driver that has no foreign key metadata for a table leaves
                // it unconstrained here; name order is then as good as any.
                LOG.log(Level.FINE, "no imported key metadata for " + self, e);
            }
            references.put(self, refs);
        }
        return ordered(byKey, references);
    }

    /**
     * The objects of {@code byKey} with each one after everything it refers
     * to; a cycle is broken where it closes, leaving the listing's order.
     */
    private static List<Obj> ordered(Map<String, Obj> byKey,
                                     Map<String, List<String>> references) {
        List<Obj> ordered = new ArrayList<>(byKey.size());
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

    /**
     * Orders views so that one comes after every other view its text names.
     *
     * <p>No portable catalog says which view reads which, so the reference is
     * read off the text: another view's name standing as a whole identifier.
     * A name that also appears as a column or a string only moves a view
     * later than it had to go, which costs nothing; what matters is that a view
     * is never created before one it reads.
     */
    private static List<Obj> viewOrder(List<Obj> views) {
        Map<String, Obj> byKey = new LinkedHashMap<>();
        for (Obj o : views) {
            byKey.put(o.name, o);
        }
        Map<String, List<String>> references = new LinkedHashMap<>();
        for (Obj o : views) {
            List<String> refs = new ArrayList<>();
            String text = body(o);
            for (Obj other : views) {
                if (other == o) {
                    continue;
                }
                Pattern word = Pattern.compile("(?<![\\w$#])" + Pattern.quote(other.name)
                        + "(?![\\w$#])", Pattern.CASE_INSENSITIVE | Pattern.UNICODE_CASE);
                if (word.matcher(text).find()) {
                    refs.add(other.name);
                }
            }
            references.put(o.name, refs);
        }
        return ordered(byKey, references);
    }

    /** A view's text after its own name, which would otherwise name itself. */
    private static String body(Obj o) {
        String text = o.ddl == null ? "" : o.ddl;
        int as = text.toUpperCase(Locale.ROOT).indexOf(" AS");
        return as < 0 ? text : text.substring(as);
    }

    // ---------------------------------------------------------------- write

    private void write(ScriptOut out, Plan plan) throws Exception {
        out.line(header());
        phase("ddl");
        List<Obj> dataOrder = runLocked(() -> {
            if (ddlInclude) {
                readDdl(plan);
            }
            return dataInclude || includeDrop ? dependencyOrder(plan.tables) : plan.tables;
        });
        if (shouldStop()) {
            return;
        }
        List<Obj> views = new ArrayList<>();
        List<Obj> materialized = new ArrayList<>();
        for (Obj v : viewOrder(plan.views)) {
            ("view".equals(v.kind) ? views : materialized).add(v);
        }

        // Read under the lock like everything else that touches the driver,
        // cheap as these are.
        DatabaseMetaData dbm = runLocked(() -> session().metaData());
        Dialect dialect = runLocked(() -> Dialect.of(dbm));
        Ident id = runLocked(() -> Ident.of(dbm));
        List<String> afterwards = runLocked(() -> Ddl.scriptEpilogue(dbm));

        Vendor vendor = dialect.vendor();
        boolean routines = ddlInclude && !plan.routines.isEmpty();
        List<String> prologue = new ArrayList<>(runLocked(() -> Ddl.scriptPrologue(dbm)));
        prologue.addAll(vendor.backupPrologue(id, scopeCatalog, scopeSchema, routines));
        lines(out, prologue);

        List<String> alters = new ArrayList<>();
        if (ddlInclude) {
            if (includeDrop) {
                writeDrops(out, plan, views, materialized, dataOrder, dialect, id);
            }
            lines(out, plan.preamble);
            if (vendor.scriptClient() == Vendor.ScriptClient.SQLCMD
                    && !plan.preamble.isEmpty()) {
                // A batch is compiled whole, and a column cannot be declared
                // with a type the same batch creates.
                out.paragraph("GO");
            }
            for (Obj s : plan.sequences) {
                module(out, dialect, s);
            }
            alters = writeTables(out, plan.tables);
            lines(out, plan.owned);
            if (!dataInclude) {
                lines(out, alters);
            }
            for (Obj r : plan.routines) {
                module(out, dialect, r);
            }
            for (Obj v : views) {
                module(out, dialect, v);
            }
            addBytes(out.unreported());
        }

        if (dataInclude && !shouldStop()) {
            List<String> before = runLocked(() -> Ddl.dataPrologue(dbm));
            lines(out, before);
            writeData(out, dataOrder, dialect);
            out.line("");
            List<String> after = runLocked(() -> Ddl.dataEpilogue(dbm));
            lines(out, after);
            // After the rows, as pg_dump adds them: the rows of two tables that
            // reference each other load in no order while the keys are in
            // place, and a key checked once over a loaded table is cheaper
            // than per row.
            lines(out, alters);
        }
        if (shouldStop()) {
            return;
        }

        List<String> counters = runLocked(() -> counters(plan, dataOrder, dialect, id));
        lines(out, counters);
        for (Obj v : materialized) {
            module(out, dialect, v);
        }
        for (Obj t : plan.triggers) {
            module(out, dialect, t);
        }

        List<String> epilogue = new ArrayList<>(afterwards);
        epilogue.addAll(vendor.backupEpilogue(id, scopeCatalog, scopeSchema, routines,
                ddlInclude && (!plan.views.isEmpty() || !plan.routines.isEmpty()
                        || !plan.triggers.isEmpty())));
        lines(out, epilogue);
        addBytes(out.unreported());
    }

    /**
     * Reads every object's DDL. One that cannot be read is dropped from the
     * plan and reported; the backup goes on without it.
     */
    private void readDdl(Plan plan) throws SQLException {
        Connection conn = session().connection();
        DatabaseMetaData dbm = session().metaData();
        for (Obj o : plan.tables) {
            if (shouldStop()) {
                return;
            }
            o.script = Ddl.script(conn, dbm, o.catalog, o.schema, o.name, splitForeignKeys);
            if (dataInclude) {
                o.script = Dialect.of(dbm).vendor().resumeIdentity(conn, Ident.of(dbm), o.catalog,
                        o.schema, o.name, o.script);
            }
        }
        for (List<Obj> kind : List.of(plan.sequences, plan.routines, plan.views, plan.triggers)) {
            kind.removeIf(o -> !shouldStop() && !readObject(conn, dbm, o));
        }
    }

    private boolean readObject(Connection conn, DatabaseMetaData dbm, Obj o) {
        try {
            if ("package".equals(o.kind)) {
                o.ddl = Ddl.oraclePackage(conn, o.schema, o.name);
                if (o.ddl == null) {
                    throw new BridgeException("sql", "the server keeps no readable source");
                }
            } else {
                JsonObject req = new JsonObject();
                req.addProperty("catalog", o.catalog);
                req.addProperty("schema", o.schema);
                req.addProperty("name", o.name);
                req.addProperty("object", "materialized view".equals(o.kind) ? "view" : o.kind);
                o.ddl = Json.str(Ddl.of(conn, dbm, req), "ddl");
            }
            if (o.ddl == null || o.ddl.isBlank()) {
                throw new BridgeException("sql", "the server returned no text");
            }
            Vendor vendor = Dialect.of(dbm).vendor();
            Ident id = Ident.of(dbm);
            o.sqlMode = vendor.sqlMode(conn, o.catalog, o.name, o.kind);
            if ("sequence".equals(o.kind)) {
                o.currentValue = vendor.sequenceValue(conn, id, o.catalog, o.schema, o.name,
                        o.currentValue);
                o.ddl = vendor.resumeSequence(conn, o.schema, o.name, o.ddl);
            } else if ("function".equals(o.kind) || "procedure".equals(o.kind)) {
                // Each overload its own statement, ended by the CLP's
                // terminator, which Scripts.module sets around them.
                List<String> texts = vendor.overloadTexts(conn, o.schema, o.name);
                if (!texts.isEmpty()) {
                    StringBuilder sb = new StringBuilder();
                    for (int k = 0; k < texts.size(); k++) {
                        sb.append(k == texts.size() - 1 ? texts.get(k)
                                : Scripts.db2Terminated(texts.get(k)) + "\n\n");
                    }
                    o.ddl = sb.toString();
                }
                List<String> drops = vendor.routineDrops(conn, id, o.schema, o.name,
                        "procedure".equals(o.kind));
                if (drops != null) {
                    o.drops = drops;
                }
            }
            return true;
        } catch (ScriptFailure e) {
            // A profile's hook that throws fails the backup, as it fails any
            // request: skipping the object would pass a broken script off as
            // an object the server could not give.
            throw e;
        } catch (RuntimeException | SQLException e) {
            LOG.log(Level.FINE, "skipped " + o.kind + " " + o.display(), e);
            addError(new BridgeException("sql",
                    o.kind + " " + o.display() + ": " + e.getMessage(), e));
            return false;
        }
    }

        private void writeDrops(ScriptOut out, Plan plan, List<Obj> views, List<Obj> materialized,
                            List<Obj> dataOrder, Dialect dialect, Ident id) throws IOException {
        // Dependents first: what reads a view, then the views, the routines,
        // the tables from the referencing end, and the sequences they draw on.
        // A trigger goes with its table. An object whose text replaces what is
        // there (CREATE OR REPLACE, CREATE OR ALTER) needs no DROP, and Oracle
        // and PostgreSQL write their routines that way.
        List<Obj> drops = new ArrayList<>();
        for (int i = materialized.size() - 1; i >= 0; i--) {
            drops.add(materialized.get(i));
        }
        for (int i = views.size() - 1; i >= 0; i--) {
            drops.add(views.get(i));
        }
        drops.addAll(plan.routines);
        String catalog = dialect.vendor().createsWithoutCatalog() ? null : scopeCatalog;
        for (Obj o : drops) {
            if (o.drops != null && !o.drops.isEmpty()) {
                for (String drop : o.drops) {
                    out.line(drop);
                }
                continue;
            }
            String keyword = dropKeyword(o);
            if (keyword != null) {
                out.line(Scripts.dropStatement(dialect, keyword,
                        id.qualify(catalog, o.schema, o.name)));
            }
        }
        // Two tables that reference each other drop in no order while the keys
        // stand, so the keys go first, as pg_dump --clean has it. MySQL has
        // its checks off, Oracle drops with CASCADE CONSTRAINTS, and SQLite
        // does not enforce keys unless asked to.
        for (int i = dataOrder.size() - 1; i >= 0; i--) {
            Obj t = dataOrder.get(i);
            for (String fk : t.foreignKeys) {
                String line = Scripts.dropForeignKey(dialect,
                        id.qualify(t.catalog, t.schema, t.name), id.q(fk));
                if (line != null) {
                    out.line(line);
                }
            }
        }
        for (int i = dataOrder.size() - 1; i >= 0; i--) {
            Obj t = dataOrder.get(i);
            out.line(Scripts.dropStatement(dialect, "TABLE",
                    id.qualify(t.catalog, t.schema, t.name)));
        }
        for (Obj o : plan.sequences) {
            out.line(Scripts.dropStatement(dialect, "SEQUENCE",
                    id.qualify(catalog, o.schema, o.name)));
        }
        for (String drop : plan.preambleDrops) {
            out.line(drop);
        }
        out.line("");
    }

    /** What {@code DROP} names the object as, or {@code null} for no drop at all. */
    private static String dropKeyword(Obj o) {
        if ("table".equals(o.kind)) {
            return "TABLE";
        }
        String head = o.ddl == null ? "" : o.ddl.strip().toUpperCase(Locale.ROOT)
                .replaceAll("\\s+", " ");
        if (head.startsWith("CREATE OR REPLACE ") || head.startsWith("CREATE OR ALTER ")) {
            return null;
        }
        if (head.startsWith("CREATE ALIAS ") || head.startsWith("CREATE FORCE ALIAS ")) {
            return "ALIAS";
        }
        switch (o.kind) {
            case "view":              return "VIEW";
            case "materialized view": return "MATERIALIZED VIEW";
            case "sequence":          return "SEQUENCE";
            case "function":          return "FUNCTION";
            case "procedure":         return "PROCEDURE";
            default:                  return null;
        }
    }

    /** Writes every {@code CREATE}, and answers the foreign keys, terminated. */
    private static List<String> writeTables(ScriptOut out, List<Obj> tables)
            throws IOException {
        List<String> alters = new ArrayList<>();
        for (Obj o : tables) {
            if (o.script == null) {
                continue;
            }
            out.paragraph(o.script.creates);
            for (String alter : o.script.alters) {
                alters.add(alter + ";");
            }
        }
        return alters;
    }

    private static void module(ScriptOut out, Dialect dialect, Obj o) throws IOException {
        if (o.ddl == null) {
            return;
        }
        String text = Scripts.module(dialect, o.kind, o.ddl);
        if (o.sqlMode != null) {
            // Created under its own mode, as mysqldump has it: the object keeps
            // the mode it was created under, and its text may only parse there.
            text = "SET @saved_sql_mode = @@SESSION.SQL_MODE;\n"
                    + "SET SESSION SQL_MODE = " + Ident.literal(o.sqlMode) + ";\n"
                    + text + "\nSET SESSION SQL_MODE = @saved_sql_mode;";
        }
        out.paragraph(text);
    }

    /**
     * The first line of every backup: a comment naming the scope it was taken
     * of, {@code -- rudbman backup: {"catalog":"shop","schema":"dbo"}}, the
     * names as the catalog stores them and absent where the request named
     * none.
     *
     * <p>A backup names its objects in full, so a restore into another scope
     * has to know which names to move. This line says it outright, rather
     * than leaving the restore to read it off the statements; to every
     * product's client it is a comment, so the script still replays with
     * {@code psql}, {@code mysql}, {@code sqlcmd} or SQL*Plus as it stands.
     * One line of JSON, so that a name with a quote or a line break in it
     * cannot end the comment early.
     */
    String header() {
        JsonObject scope = new JsonObject();
        if (scopeCatalog != null && !scopeCatalog.isEmpty()) {
            scope.addProperty("catalog", scopeCatalog);
        }
        if (scopeSchema != null && !scopeSchema.isEmpty()) {
            scope.addProperty("schema", scopeSchema);
        }
        return HEADER + new com.google.gson.GsonBuilder().disableHtmlEscaping().create()
                .toJson(scope);
    }

    /** What the first line of a backup starts with; see {@link #header}. */
    static final String HEADER = "-- rudbman backup: ";

    private static void lines(ScriptOut out, List<String> statements) throws IOException {
        for (String s : statements) {
            out.line(s);
        }
        if (!statements.isEmpty()) {
            out.line("");
        }
    }

    /**
     * Sets every counter the rows have moved past: PostgreSQL's identity and
     * serial columns, which an {@code INSERT} with explicit values leaves at
     * their start, then each sequence to the value its source has handed out.
     * MySQL's {@code AUTO_INCREMENT}, H2's and Oracle's identities and SQL
     * Server's follow the rows by themselves, or are carried in the
     * {@code CREATE}.
     */
    private List<String> counters(Plan plan, List<Obj> tables, Dialect dialect, Ident id)
            throws SQLException {
        List<Vendor.Sequence> sequences = new ArrayList<>();
        for (Obj s : plan.sequences) {
            sequences.add(new Vendor.Sequence(s.catalog, s.schema, s.name, s.currentValue,
                    s.increment, s.start));
        }
        List<Vendor.Table> written = new ArrayList<>();
        for (Obj t : tables) {
            written.add(new Vendor.Table(t.catalog, t.schema, t.name));
        }
        return new ArrayList<>(dialect.vendor().counters(session().metaData(), id, sequences,
                written, dataInclude));
    }

    // ------------------------------------------------------------------ data

    private void writeData(ScriptOut out, List<Obj> tables, Dialect dialect) throws Exception {
        for (Obj o : tables) {
            if (shouldStop()) {
                return;
            }
            phase("data:" + o.display());
            runLocked(() -> {
                writeTable(out, o, dialect);
                return null;
            });
            addBytes(out.unreported());
        }
    }

    private void writeTable(ScriptOut out, Obj o, Dialect dialect) throws Exception {
        DatabaseMetaData dbm = session().metaData();
        Ident id = Ident.of(dbm);
        String qualified = id.qualify(o.catalog, o.schema, o.name);

        // A computed column takes no value - every product refuses one - so it
        // is left out wherever the CREATE above recreates it as computed, and
        // kept where it came out a plain column. Rows alone go into a schema
        // that exists already, computed columns and all.
        boolean skipGenerated = o.script == null || o.script.keepsGenerated;
        List<Ddl.DataColumn> columns = Ddl.dataColumns(dbm, o.catalog, o.schema, o.name);
        List<String> selected = new ArrayList<>();
        List<String> identities = new ArrayList<>();
        boolean overriding = false;
        for (Ddl.DataColumn c : columns) {
            if (skipGenerated && c.generated) {
                continue;
            }
            selected.add(c.name);
            if (c.autoIncrement) {
                identities.add(c.name);
            }
            overriding |= c.identityAlways;
        }
        Vendor vendor = dialect.vendor();
        Set<String> asText = vendor.textColumns(session().connection(), o.schema, o.name);
        String select;
        if (selected.isEmpty() || (selected.size() == columns.size() && asText.isEmpty())) {
            select = "*";
        } else {
            StringBuilder sb = new StringBuilder();
            for (String name : selected) {
                if (sb.length() > 0) {
                    sb.append(", ");
                }
                sb.append(asText.contains(name) ? vendor.textColumn(id.q(name)) : id.q(name));
            }
            select = sb.toString();
        }

        // What lets an identity take the source's values, and puts it back.
        Vendor.RowFence fence = vendor.rowFence(session().connection(), id, o.schema, o.name,
                qualified, identities);
        for (String line : fence.before()) {
            out.line(line);
        }

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
            try (ResultSet rs = st.executeQuery("SELECT " + select + " FROM " + qualified)) {
                ResultSetMetaData md = rs.getMetaData();
                int n = md.getColumnCount();
                String[] names = new String[n];
                int[] types = new int[n];
                for (int i = 1; i <= n; i++) {
                    names[i - 1] = md.getColumnLabel(i);
                    types[i - 1] = md.getColumnType(i);
                }
                Scripts.writeInserts(this, out, rs, qualified, names, types, dialect, id,
                        insertBatchRows, overriding);
            } finally {
                inFlight(null);
            }
        }

        for (String line : fence.after()) {
            out.line(line);
        }
    }

}
