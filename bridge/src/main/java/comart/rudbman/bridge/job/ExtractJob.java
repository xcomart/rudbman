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

import java.io.BufferedWriter;
import java.io.Closeable;
import java.io.IOException;
import java.io.OutputStream;
import java.io.OutputStreamWriter;
import java.io.Writer;
import java.math.BigDecimal;
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
import java.sql.Types;
import java.util.ArrayList;
import java.util.HashMap;
import java.util.LinkedHashMap;
import java.util.List;
import java.util.Locale;
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
 * and generated DDL agree. Literals are rendered conservatively: text is single
 * quoted with quotes doubled, numbers go bare, dates go out as plain quoted
 * strings rather than as typed {@code DATE '…'} literals - every product accepts
 * a string there, and SQL Server rejects the typed form. Binary is the one place
 * where a common form does not exist, and the four spellings this bridge knows
 * are listed on {@link #hexLiteral}.
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

    private long reportedBytes;

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
        try (Out out = new Out(Files.newOutputStream(path))) {
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

    private void writeDdl(Out out) throws Exception {
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
                    out.line(dropStatement(dialect, id.qualify(o.catalog, o.schema, o.name)));
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

    /**
     * @return a {@code DROP TABLE} statement, with {@code IF EXISTS} on the
     *         products that have it. Oracle and Db2 do not (Oracle only from
     *         23ai), so there the statement fails on a missing table and the
     *         script has to be run past that error.
     */
    private static String dropStatement(Dialect dialect, String qualified) {
        boolean ifExists;
        switch (dialect) {
            case ORACLE:
            case DB2:
            case OTHER:
                ifExists = false;
                break;
            default:
                ifExists = true;
        }
        return "DROP TABLE " + (ifExists ? "IF EXISTS " : "") + qualified + ";";
    }

    // ------------------------------------------------------------------ data

    private void writeData(Out out) throws Exception {
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

    private void writeTable(Out out, Obj o, TemplateManager template) throws Exception {
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
                        writeInserts(out, rs, qualified, names, types, dialect, id);
                }
            } finally {
                inFlight(null);
            }
        }
    }

    private void writeInserts(Out out, ResultSet rs, String qualified, String[] names,
                              int[] types, Dialect dialect, Ident id) throws Exception {
        StringBuilder cols = new StringBuilder();
        for (String name : names) {
            if (cols.length() > 0) {
                cols.append(", ");
            }
            cols.append(id.q(name));
        }
        String head = "INSERT INTO " + qualified + " (" + cols + ") VALUES";

        // Oracle has no multi-row VALUES clause; asking for one there would
        // produce a script that cannot run, so the request is clamped rather
        // than honoured into a broken file.
        int batchRows = dialect == Dialect.ORACLE ? 1 : insertBatchRows;

        List<String> tuples = new ArrayList<>(batchRows);
        long sinceSync = 0;
        while (rs.next()) {
            if (shouldStop()) {
                break;
            }
            StringBuilder t = new StringBuilder("(");
            for (int i = 1; i <= names.length; i++) {
                if (i > 1) {
                    t.append(", ");
                }
                t.append(literal(rs, i, types[i - 1], dialect));
            }
            tuples.add(t.append(')').toString());
            addRows(1);
            if (tuples.size() >= batchRows) {
                flushTuples(out, head, tuples);
            }
            if (++sinceSync >= BYTE_SYNC_ROWS) {
                sinceSync = 0;
                syncBytes(out);
            }
        }
        if (!tuples.isEmpty()) {
            flushTuples(out, head, tuples);
        }
    }

    private void flushTuples(Out out, String head, List<String> tuples) throws IOException {
        if (tuples.size() == 1) {
            out.line(head + " " + tuples.get(0) + ";");
        } else {
            out.line(head);
            for (int i = 0; i < tuples.size(); i++) {
                out.line(tuples.get(i) + (i == tuples.size() - 1 ? ";" : ","));
            }
        }
        tuples.clear();
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
    private void writeCsv(Out out, ResultSet rs, String[] names, int[] types) throws Exception {
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
                String v = text(rs, i, types[i - 1]);
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
    private void writeTemplate(Out out, ResultSet rs, Obj o, String qualified, String[] names,
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
                String v = text(rs, i, types[i - 1]);
                Map<String, Object> col = new LinkedHashMap<>();
                col.put("name", names[i - 1]);
                col.put("value", v);
                col.put("literal", literal(rs, i, types[i - 1], dialect));
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

    // -------------------------------------------------------------- literals

    /**
     * Renders one column of the current row as a SQL literal.
     *
     * @return the literal text, or {@code NULL}
     * @throws SQLException if the driver fails
     */
    private static String literal(ResultSet rs, int i, int type, Dialect dialect)
            throws SQLException {
        switch (type) {
            case Types.BINARY:
            case Types.VARBINARY:
            case Types.LONGVARBINARY:
            case Types.BLOB: {
                byte[] b = rs.getBytes(i);
                return b == null || rs.wasNull() ? "NULL" : hexLiteral(b, dialect);
            }
            case Types.TINYINT:
            case Types.SMALLINT:
            case Types.INTEGER:
            case Types.BIGINT: {
                // Read as text rather than as a long: an unsigned BIGINT does not
                // fit one, and the driver already knows how to spell its own.
                String s = rs.getString(i);
                return s == null || rs.wasNull() ? "NULL" : s;
            }
            case Types.DECIMAL:
            case Types.NUMERIC: {
                BigDecimal d = rs.getBigDecimal(i);
                // toPlainString, because toString switches to exponent notation
                // at some scales and not every parser accepts 1E+2 as a decimal.
                return d == null || rs.wasNull() ? "NULL" : d.toPlainString();
            }
            case Types.FLOAT:
            case Types.REAL:
            case Types.DOUBLE: {
                double d = rs.getDouble(i);
                if (rs.wasNull()) {
                    return "NULL";
                }
                if (Double.isNaN(d) || Double.isInfinite(d)) {
                    // No portable literal exists; a quoted form at least fails
                    // loudly instead of producing a wrong number silently.
                    return Ident.literal(Double.toString(d));
                }
                return Double.toString(d);
            }
            case Types.BIT:
            case Types.BOOLEAN: {
                boolean b = rs.getBoolean(i);
                if (rs.wasNull()) {
                    return "NULL";
                }
                return booleanLiteral(b, dialect);
            }
            default: {
                String s = rs.getString(i);
                return s == null || rs.wasNull() ? "NULL" : Ident.literal(s);
            }
        }
    }

    /**
     * @return {@code TRUE}/{@code FALSE} where the product has a boolean type,
     *         {@code 1}/{@code 0} where it does not. Oracle before 23ai, SQL
     *         Server and SQLite are in the second group.
     */
    private static String booleanLiteral(boolean b, Dialect dialect) {
        switch (dialect) {
            case ORACLE:
            case SQLSERVER:
            case SQLITE:
            case MYSQL:
            case MARIADB:
                return b ? "1" : "0";
            default:
                return b ? "TRUE" : "FALSE";
        }
    }

    /**
     * Renders binary data.
     *
     * <p>There is no common spelling, so four are known:
     * {@code 0x…} for SQL Server, {@code '\x…'} for PostgreSQL's bytea input,
     * {@code HEXTORAW('…')} for Oracle and the standard {@code X'…'} for
     * everything else, which is what H2, MySQL, SQLite and Db2 accept. A product
     * that reaches {@link Dialect#OTHER} gets the standard form and may not take
     * it.
     */
    private static String hexLiteral(byte[] b, Dialect dialect) {
        String hex = hex(b);
        switch (dialect) {
            case SQLSERVER:  return "0x" + hex;
            case POSTGRESQL: return "'\\x" + hex + "'";
            case ORACLE:     return "HEXTORAW('" + hex + "')";
            default:         return "X'" + hex + "'";
        }
    }

    /**
     * Renders one column of the current row as plain text, for CSV and templates.
     *
     * @return the text, or {@code null} for SQL NULL
     * @throws SQLException if the driver fails
     */
    private static String text(ResultSet rs, int i, int type) throws SQLException {
        switch (type) {
            case Types.BINARY:
            case Types.VARBINARY:
            case Types.LONGVARBINARY:
            case Types.BLOB: {
                byte[] b = rs.getBytes(i);
                return b == null || rs.wasNull() ? null : hex(b);
            }
            default: {
                String s = rs.getString(i);
                return s == null || rs.wasNull() ? null : s;
            }
        }
    }

    /**
     * @param b the bytes
     * @return their upper-case hexadecimal spelling, with no prefix
     */
    private static String hex(byte[] b) {
        StringBuilder sb = new StringBuilder(b.length * 2);
        for (byte x : b) {
            sb.append(Character.forDigit((x >> 4) & 0xf, 16))
                    .append(Character.forDigit(x & 0xf, 16));
        }
        return sb.toString().toUpperCase(Locale.ROOT);
    }

    // ---------------------------------------------------------------- output

    private void syncBytes(Out out) {
        long written = out.written();
        addBytes(written - reportedBytes);
        reportedBytes = written;
    }

    /**
     * The output file: a buffered, charset-encoding writer over a stream that
     * counts the bytes it passes on.
     *
     * <p>The count is what {@code JOB_POLL} reports, and it is read without
     * flushing, so it lags by up to one buffer. That is the right trade for a
     * progress bar; flushing per row to make the number exact would cost far more
     * than the number is worth.
     */
    private final class Out implements Closeable {

        private final Counting counting;
        private final Writer writer;

        Out(OutputStream raw) {
            counting = new Counting(raw);
            writer = new BufferedWriter(new OutputStreamWriter(counting, charset), 1 << 16);
        }

        /** Writes text exactly as given, including any line breaks it holds. */
        void raw(String s) throws IOException {
            writer.write(s);
        }

        /** Writes text and the configured record separator. */
        void line(String s) throws IOException {
            writer.write(s);
            writer.write(newline);
        }

        /**
         * Writes a block of generated SQL, translating its line breaks to the
         * configured separator.
         *
         * <p>Only generated text goes through here. Row data never does: a line
         * break inside a value is data, and rewriting it would corrupt the row.
         */
        void block(String s) throws IOException {
            String normalised = s.replace("\r\n", "\n");
            if ("\n".equals(newline)) {
                writer.write(normalised);
            } else {
                writer.write(normalised.replace("\n", newline));
            }
        }

        void flush() throws IOException {
            writer.flush();
        }

        /** @return bytes handed to the file so far, buffered text excluded */
        long written() {
            return counting.count;
        }

        @Override
        public void close() throws IOException {
            writer.close();
        }
    }

    /** An output stream that counts. */
    private static final class Counting extends OutputStream {

        private final OutputStream out;
        private volatile long count;

        Counting(OutputStream out) {
            this.out = out;
        }

        @Override
        public void write(int b) throws IOException {
            out.write(b);
            count++;
        }

        @Override
        public void write(byte[] b, int off, int len) throws IOException {
            out.write(b, off, len);
            count += len;
        }

        @Override
        public void flush() throws IOException {
            out.flush();
        }

        @Override
        public void close() throws IOException {
            out.close();
        }
    }
}
