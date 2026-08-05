package comart.rudbman.bridge;

import com.google.gson.JsonArray;
import com.google.gson.JsonObject;
import comart.rudbman.bridge.support.Batch;
import comart.rudbman.bridge.support.H2;
import comart.rudbman.bridge.support.Resp;
import org.junit.jupiter.api.AfterEach;
import org.junit.jupiter.api.BeforeEach;
import org.junit.jupiter.api.Test;
import org.junit.jupiter.api.io.TempDir;

import java.nio.charset.StandardCharsets;
import java.nio.file.Files;
import java.nio.file.Path;
import java.util.ArrayList;
import java.util.List;

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertFalse;
import static org.junit.jupiter.api.Assertions.assertTrue;

/**
 * {@code JOB_START kind: "extract"} end to end (architecture.md 6).
 *
 * <p>The assertions that matter are the replays. A string comparison against a
 * generated script only proves that this build generates what this build
 * generates; running the script into a fresh database and reading the rows back
 * out proves that what was extracted is what was there. Everything else here -
 * the CSV quoting, the template rendering, the progress and cancel behaviour -
 * pins a contract the Rust side codes against.
 */
class ExtractJobTest {

    @TempDir
    Path tmp;

    private long session;

    @BeforeEach
    void setUp() {
        session = H2.open(H2.freshUrl());
    }

    @AfterEach
    void tearDown() {
        H2.close(session);
    }

    // --------------------------------------------------------------- round trip

    @Test
    void ddlAndInsertsReplayIntoAFreshDatabase() throws Exception {
        H2.exec(session, "create schema ex");
        H2.exec(session, "create table ex.parent ("
                + "id int not null primary key, code varchar(20) not null)");
        H2.exec(session, "create table ex.child ("
                + "id int not null primary key, parent_id int not null, "
                + "note varchar(200), amount decimal(12,4), flag boolean, blob_col varbinary(16), "
                + "constraint fk_child_parent foreign key (parent_id) references ex.parent(id))");
        H2.exec(session, "create index idx_child_note on ex.child(note)");
        H2.exec(session, "insert into ex.parent values (1, 'A'), (2, 'B')");
        H2.exec(session, "insert into ex.child values "
                // An embedded apostrophe, which is the literal escape that gets
                // this wrong most often.
                + "(10, 1, 'it''s fine', 1.5, true, X'0A1B'), "
                // Unicode, in a column wide enough that no truncation hides a bug.
                + "(11, 1, '한글 テスト ☃', -2.25, false, null), "
                // Every nullable column null at once.
                + "(12, 2, null, null, null, null)");

        Path out = tmp.resolve("roundtrip.sql");
        long job = start(spec(out, objects(obj("EX", "PARENT"), obj("EX", "CHILD")),
                ddl(true, false, "alter"), data(true, "insert", 1, null, null)));
        JsonObject done = await(job);
        assertEquals("done", done.get("state").getAsString(), done.toString());
        assertEquals(5, done.get("rows_done").getAsLong(), done.toString());
        assertTrue(done.get("bytes").getAsLong() > 0, done.toString());
        assertTrue(done.get("errors").getAsJsonArray().isEmpty(), done.toString());

        long fresh = H2.open(H2.freshUrl());
        try {
            H2.exec(fresh, "create schema ex");
            replay(fresh, out);

            assertEquals("2", one(fresh, "select count(*) from ex.parent"));
            assertEquals("3", one(fresh, "select count(*) from ex.child"));
            assertEquals("it's fine", one(fresh, "select note from ex.child where id = 10"));
            assertEquals("한글 テスト ☃", one(fresh, "select note from ex.child where id = 11"));
            assertEquals("1.5000", one(fresh, "select amount from ex.child where id = 10"));
            assertEquals("true", one(fresh, "select flag from ex.child where id = 10"));
            assertEquals("0a1b", one(fresh, "select blob_col from ex.child where id = 10"));
            assertEquals(null, one(fresh, "select note from ex.child where id = 12"));
            // The foreign key came across as a constraint, not just as a column.
            assertEquals("1", one(fresh, "select count(*) from information_schema.table_constraints"
                    + " where table_name = 'CHILD' and constraint_type = 'FOREIGN KEY'"));
        } finally {
            H2.close(fresh);
        }
    }

    /**
     * The rule the whole ordering exists for: two tables that reference each
     * other cannot be created in any order, so the keys have to be added
     * afterwards.
     */
    @Test
    void aForeignKeyCycleStillProducesARunnableScript() throws Exception {
        H2.exec(session, "create schema cyc");
        H2.exec(session, "create table cyc.a (id int not null primary key, b_id int)");
        H2.exec(session, "create table cyc.b (id int not null primary key, a_id int)");
        H2.exec(session, "alter table cyc.a add constraint fk_a_b foreign key (b_id)"
                + " references cyc.b(id)");
        H2.exec(session, "alter table cyc.b add constraint fk_b_a foreign key (a_id)"
                + " references cyc.a(id)");

        Path out = tmp.resolve("cycle.sql");
        long job = start(spec(out, objects(obj("CYC", "A"), obj("CYC", "B")),
                ddl(true, false, "alter"), null));
        assertEquals("done", await(job).get("state").getAsString());

        String script = read(out);
        // Both CREATEs come before either ALTER; that is the whole contract.
        int lastCreate = script.lastIndexOf("CREATE TABLE");
        int firstAlter = script.indexOf("ALTER TABLE");
        assertTrue(firstAlter > lastCreate,
                "foreign keys must follow every CREATE:\n" + script);

        long fresh = H2.open(H2.freshUrl());
        try {
            H2.exec(fresh, "create schema cyc");
            replay(fresh, out);
            assertEquals("2", one(fresh, "select count(*) from information_schema.table_constraints"
                    + " where table_schema = 'CYC' and constraint_type = 'FOREIGN KEY'"));
        } finally {
            H2.close(fresh);
        }
    }

    @Test
    void includeDropPrependsDropStatements() throws Exception {
        H2.exec(session, "create table t (id int)");
        Path out = tmp.resolve("drop.sql");
        long job = start(spec(out, objects(obj("PUBLIC", "T")),
                ddl(true, true, "alter"), null));
        assertEquals("done", await(job).get("state").getAsString());
        String script = read(out);
        assertTrue(script.indexOf("DROP TABLE IF EXISTS PUBLIC.T;")
                        < script.indexOf("CREATE TABLE"), script);
    }

    // -------------------------------------------------------- cancel, progress

    @Test
    void cancellingMidFlightLeavesAPartialFileAndAUsableSession() throws Exception {
        generateBigTable();

        Path out = tmp.resolve("cancelled.sql");
        long job = start(spec(out, objects(obj("PUBLIC", "BIG")), null,
                data(true, "insert", 1, null, null)));

        long seen = awaitRows(job, 1);
        assertTrue(seen > 0);

        JsonObject cancelled = H2.call(Ops.JOB_CANCEL, job, 0, null).json();
        assertTrue(cancelled.get("cancelled").getAsBoolean(), cancelled.toString());

        JsonObject end = await(job);
        assertEquals("cancelled", end.get("state").getAsString(), end.toString());
        assertTrue(end.get("rows_done").getAsLong() < BIG_ROWS,
                "a cancel that only arrived after the last row proves nothing: " + end);

        // The file is there and is short: partial output is kept, not deleted.
        assertTrue(Files.exists(out));
        assertTrue(countLines(out) < BIG_ROWS, "expected a partial file");

        // And the session survived having a statement cancelled underneath it.
        assertEquals(String.valueOf(BIG_ROWS), one(session, "select count(*) from big"));
    }

    @Test
    void rowsDoneRisesMonotonicallyWhileTheJobRuns() throws Exception {
        generateBigTable();

        Path out = tmp.resolve("progress.sql");
        long job = start(spec(out, objects(obj("PUBLIC", "BIG")), null,
                data(true, "insert", 1, null, null)));

        List<Long> samples = new ArrayList<>();
        for (int i = 0; i < 40 && samples.size() < 3; i++) {
            JsonObject p = H2.call(Ops.JOB_POLL, job, 0, null).json();
            if (!"running".equals(p.get("state").getAsString())) {
                // Terminal states unregister the handle, so nothing may poll
                // again; the job outran the sampler.
                break;
            }
            long rows = p.get("rows_done").getAsLong();
            if (samples.isEmpty() || rows > samples.get(samples.size() - 1)) {
                samples.add(rows);
            }
            // rows_total is unknown by design: no COUNT(*) is run up front.
            assertTrue(p.get("rows_total").isJsonNull(), p.toString());
            assertTrue(p.get("eta_s").isJsonNull(), p.toString());
            if (rows > 0) {
                // Before the first row the phase is still "starting"; the poller
                // must not assume a phase name has appeared yet.
                assertEquals("data:PUBLIC.BIG", p.get("phase").getAsString());
            }
            Thread.sleep(5);
        }
        assertTrue(samples.size() >= 2, "expected rising row counts, got " + samples);
        for (int i = 1; i < samples.size(); i++) {
            assertTrue(samples.get(i) > samples.get(i - 1), samples.toString());
        }

        H2.call(Ops.JOB_CANCEL, job, 0, null);
        await(job);
    }

    @Test
    void aTerminalStateIsReportedOnceAndThenTheHandleIsGone() throws Exception {
        H2.exec(session, "create table t (id int)");
        Path out = tmp.resolve("once.sql");
        long job = start(spec(out, objects(obj("PUBLIC", "T")), ddl(true, false, "alter"), null));
        assertEquals("done", await(job).get("state").getAsString());
        // The rule the Rust poller codes against: stop as soon as a terminal
        // state arrives, because the handle died answering it.
        assertEquals("protocol",
                H2.call(Ops.JOB_POLL, job, 0, null).error().get("kind").getAsString());
    }

    // ------------------------------------------------------------------- csv

    @Test
    void csvQuotesOnlyWhatItHasTo() throws Exception {
        H2.exec(session, "create table c (id int, v varchar(50))");
        H2.exec(session, "insert into c values (1, 'plain'), (2, 'a,b'), (3, 'say \"hi\"'), "
                + "(4, 'line1' || char(10) || 'line2'), (5, null), (6, '')");

        Path out = tmp.resolve("out.csv");
        long job = start(spec(out, objects(obj("PUBLIC", "C")), null,
                data(true, "csv", 1, null, null)));
        assertEquals("done", await(job).get("state").getAsString());

        assertEquals("ID,V\n"
                        + "1,plain\n"
                        + "2,\"a,b\"\n"
                        + "3,\"say \"\"hi\"\"\"\n"
                        + "4,\"line1\nline2\"\n"
                        // NULL is an empty field, the empty string is a quoted
                        // empty field. Plain CSV cannot do better than that.
                        + "5,\n"
                        + "6,\"\"\n",
                read(out));
    }

    @Test
    void theNewlineOptionOnlyChangesRecordSeparatorsNotData() throws Exception {
        H2.exec(session, "create table c (id int, v varchar(50))");
        H2.exec(session, "insert into c values (1, 'a' || char(10) || 'b')");

        Path out = tmp.resolve("crlf.csv");
        JsonObject spec = spec(out, objects(obj("PUBLIC", "C")), null,
                data(true, "csv", 1, null, null));
        spec.getAsJsonObject("output").addProperty("newline", "\r\n");
        assertEquals("done", await(start(spec)).get("state").getAsString());

        // The value keeps its bare LF; only the record terminators became CRLF.
        assertEquals("ID,V\r\n1,\"a\nb\"\r\n", read(out));
    }

    // -------------------------------------------------------------- template

    @Test
    void templateModeRunsEachRowThroughTheInheritedEngine() throws Exception {
        H2.exec(session, "create table t (id int, v varchar(20))");
        H2.exec(session, "insert into t values (1, 'x'), (2, 'y')");

        Path template = tmp.resolve("row.tpl");
        Files.write(template, ("${table}#${row_no}: "
                + "${for:item=columns,inStr=\", \"}${name}=${value}${endfor}\n")
                .getBytes(StandardCharsets.UTF_8));

        Path out = tmp.resolve("out.txt");
        long job = start(spec(out, objects(obj("PUBLIC", "T")), null,
                data(true, "template", 1, template.toString(), null)));
        assertEquals("done", await(job).get("state").getAsString());

        assertEquals("T#1: ID=1, V=x\nT#2: ID=2, V=y\n", read(out));
    }

    // ----------------------------------------------------------- insert shapes

    @Test
    void batchedInsertsGroupTheirValues() throws Exception {
        H2.exec(session, "create table t (id int, v varchar(20))");
        H2.exec(session, "insert into t values (1, 'a'), (2, 'b'), (3, 'c')");

        Path out = tmp.resolve("batched.sql");
        long job = start(spec(out, objects(obj("PUBLIC", "T")), null,
                data(true, "insert", 2, null, null)));
        assertEquals("done", await(job).get("state").getAsString());

        assertEquals("INSERT INTO PUBLIC.T (ID, V) VALUES\n"
                        + "(1, 'a'),\n"
                        + "(2, 'b');\n"
                        // The remainder flushes as its own statement.
                        + "INSERT INTO PUBLIC.T (ID, V) VALUES (3, 'c');\n",
                read(out));
    }

    @Test
    void whereNarrowsTheSingleObjectItIsAllowedOn() throws Exception {
        H2.exec(session, "create table t (id int)");
        H2.exec(session, "insert into t values (1), (2), (3)");

        Path out = tmp.resolve("where.sql");
        long job = start(spec(out, objects(obj("PUBLIC", "T")), null,
                data(true, "insert", 1, null, "id > 1")));
        JsonObject done = await(job);
        assertEquals("done", done.get("state").getAsString());
        assertEquals(2, done.get("rows_done").getAsLong());
    }

    // ------------------------------------------------------- protocol errors

    @Test
    void whereWithSeveralObjectsIsRejectedBeforeAnyThreadStarts() {
        H2.exec(session, "create table t (id int)");
        H2.exec(session, "create table u (id int)");
        JsonObject spec = spec(tmp.resolve("no.sql"),
                objects(obj("PUBLIC", "T"), obj("PUBLIC", "U")), null,
                data(true, "insert", 1, null, "id > 1"));
        JsonObject err = H2.call(Ops.JOB_START, session, 0, spec).error();
        assertEquals("protocol", err.get("kind").getAsString());
        assertTrue(err.get("message").getAsString().contains("exactly one"), err.toString());
    }

    @Test
    void anUnknownJobKindIsAProtocolError() {
        JsonObject spec = new JsonObject();
        spec.addProperty("kind", "teleport");
        assertEquals("protocol",
                H2.call(Ops.JOB_START, session, 0, spec).error().get("kind").getAsString());
    }

    @Test
    void anUnknownModeIsAProtocolError() {
        JsonObject spec = spec(tmp.resolve("no.sql"), objects(obj("PUBLIC", "T")), null,
                data(true, "yaml", 1, null, null));
        assertEquals("protocol",
                H2.call(Ops.JOB_START, session, 0, spec).error().get("kind").getAsString());
    }

    @Test
    void anUnwritablePathFailsTheJobRatherThanTheCall() throws Exception {
        H2.exec(session, "create table t (id int)");
        // A path whose parent is an existing regular file: the directory cannot
        // be created, and the failure only shows up on the worker thread.
        Path file = tmp.resolve("a-file");
        Files.write(file, new byte[]{1});
        JsonObject spec = spec(file.resolve("nested/out.sql"), objects(obj("PUBLIC", "T")),
                ddl(true, false, "alter"), null);
        JsonObject end = await(start(spec));
        assertEquals("failed", end.get("state").getAsString(), end.toString());
        assertEquals(1, end.get("errors").getAsJsonArray().size(), end.toString());
        assertEquals("io", end.get("errors").getAsJsonArray().get(0).getAsJsonObject()
                .get("kind").getAsString(), end.toString());
    }

    @Test
    void closingTheSessionCancelsItsJobs() throws Exception {
        long own = H2.open(H2.freshUrl());
        H2.exec(own, "create table t (id int)");
        Path out = tmp.resolve("closed.sql");
        JsonObject spec = spec(out, objects(obj("PUBLIC", "T")), ddl(true, false, "alter"), null);
        Resp started = H2.call(Ops.JOB_START, own, 0, spec);
        long job = started.json().get("job").getAsLong();
        H2.close(own);
        // The handle is gone with the session, which is what keeps an abandoned
        // job from outliving the connection it was reading through.
        assertFalse(H2.call(Ops.JOB_POLL, job, 0, null).ok);
    }

    // --------------------------------------------------------------- helpers

    private static final int BIG_ROWS = 200_000;

    private void generateBigTable() {
        H2.exec(session, "create table big (id int, v varchar(80))");
        H2.exec(session, "insert into big select x, 'row number ' || x"
                + " from system_range(1, " + BIG_ROWS + ")");
    }

    private long start(JsonObject spec) {
        return H2.call(Ops.JOB_START, session, 0, spec).json().get("job").getAsLong();
    }

    /** Polls until the job leaves {@code running} and returns that one answer. */
    private JsonObject await(long job) throws Exception {
        for (int i = 0; i < 1200; i++) {
            JsonObject p = H2.call(Ops.JOB_POLL, job, 0, null).json();
            if (!"running".equals(p.get("state").getAsString())) {
                return p;
            }
            Thread.sleep(25);
        }
        throw new AssertionError("job " + job + " never finished");
    }

    /** @return the first {@code rows_done} at or above {@code atLeast} */
    private long awaitRows(long job, long atLeast) throws Exception {
        for (int i = 0; i < 1200; i++) {
            JsonObject p = H2.call(Ops.JOB_POLL, job, 0, null).json();
            long rows = p.get("rows_done").getAsLong();
            if (rows >= atLeast) {
                return rows;
            }
            if (!"running".equals(p.get("state").getAsString())) {
                throw new AssertionError("job finished before producing rows: " + p);
            }
            Thread.sleep(2);
        }
        throw new AssertionError("job " + job + " produced no rows");
    }

    private static JsonObject obj(String schema, String name) {
        JsonObject o = new JsonObject();
        o.addProperty("schema", schema);
        o.addProperty("name", name);
        return o;
    }

    private static JsonArray objects(JsonObject... objs) {
        JsonArray arr = new JsonArray();
        for (JsonObject o : objs) {
            arr.add(o);
        }
        return arr;
    }

    private static JsonObject ddl(boolean include, boolean drop, String constraints) {
        JsonObject o = new JsonObject();
        o.addProperty("include", include);
        o.addProperty("include_drop", drop);
        o.addProperty("constraints", constraints);
        return o;
    }

    private static JsonObject data(boolean include, String mode, int batch, String templatePath,
                                   String where) {
        JsonObject o = new JsonObject();
        o.addProperty("include", include);
        o.addProperty("mode", mode);
        o.addProperty("insert_batch_rows", batch);
        if (templatePath != null) {
            o.addProperty("template_path", templatePath);
        }
        if (where != null) {
            o.addProperty("where", where);
        }
        return o;
    }

    private static JsonObject spec(Path out, JsonArray objects, JsonObject ddl, JsonObject data) {
        JsonObject spec = new JsonObject();
        spec.addProperty("kind", "extract");
        spec.add("objects", objects);
        JsonObject output = new JsonObject();
        output.addProperty("path", out.toString());
        output.addProperty("charset", "UTF-8");
        output.addProperty("newline", "\n");
        spec.add("output", output);
        if (ddl != null) {
            spec.add("ddl", ddl);
        }
        if (data != null) {
            spec.add("data", data);
        }
        return spec;
    }

    private static String read(Path p) throws Exception {
        return new String(Files.readAllBytes(p), StandardCharsets.UTF_8);
    }

    private static long countLines(Path p) throws Exception {
        try (var lines = Files.lines(p, StandardCharsets.UTF_8)) {
            return lines.count();
        }
    }

    /**
     * Runs a generated script statement by statement.
     *
     * <p>Statements are split on a {@code ;} that ends a line, which is safe only
     * because this test owns the fixture and no value in it holds one. Real
     * splitting is {@code rudbman-sql}'s job on the Rust side.
     */
    private static void replay(long target, Path script) throws Exception {
        StringBuilder sb = new StringBuilder();
        for (String line : read(script).split("\n", -1)) {
            sb.append(line).append('\n');
            if (line.trim().endsWith(";")) {
                String stmt = sb.toString().trim();
                sb.setLength(0);
                stmt = stmt.substring(0, stmt.length() - 1).trim();
                if (!stmt.isEmpty()) {
                    H2.exec(target, stmt);
                }
            }
        }
    }

    /**
     * @return the first column of the first row rendered as text, or {@code null}
     *         when it is NULL or the query returned nothing
     */
    private static String one(long session, String sql) {
        long cursor = H2.query(session, sql).json().get("cursor").getAsLong();
        try {
            Batch batch = Resp.of(Bridge.call(Ops.FETCH, cursor, 1, null)).batch();
            if (batch.rowCount == 0 || batch.colCount == 0) {
                return null;
            }
            return cell(batch.columns[0]);
        } finally {
            Resp.of(Bridge.call(Ops.CLOSE_CURSOR, cursor, 0, null)).assertOk();
        }
    }

    private static String cell(Batch.Column col) {
        if (!col.valid[0]) {
            return null;
        }
        switch (col.kind) {
            case Batch.I64:  return Long.toString(col.i64[0]);
            case Batch.F64:  return Double.toString(col.f64[0]);
            case Batch.BOOL: return Boolean.toString(col.bools[0]);
            case Batch.BIN:  return hex(col.bin(0));
            default:         return col.str(0);
        }
    }

    private static String hex(byte[] b) {
        StringBuilder sb = new StringBuilder();
        for (byte x : b) {
            sb.append(Character.forDigit((x >> 4) & 0xf, 16))
                    .append(Character.forDigit(x & 0xf, 16));
        }
        return sb.toString();
    }
}
