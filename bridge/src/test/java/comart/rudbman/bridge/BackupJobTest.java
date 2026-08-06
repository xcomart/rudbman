package comart.rudbman.bridge;

import com.google.gson.JsonObject;
import comart.rudbman.bridge.support.Batch;
import comart.rudbman.bridge.support.H2;
import comart.rudbman.bridge.support.Resp;
import org.junit.jupiter.api.AfterEach;
import org.junit.jupiter.api.BeforeEach;
import org.junit.jupiter.api.Test;
import org.junit.jupiter.api.io.TempDir;

import java.io.ByteArrayOutputStream;
import java.io.InputStream;
import java.nio.charset.StandardCharsets;
import java.nio.file.Files;
import java.nio.file.Path;
import java.util.zip.GZIPInputStream;

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertTrue;

/**
 * {@code JOB_START kind: "backup"} end to end (architecture.md 6).
 *
 * <p>Like the extract's, the assertion that earns its keep is the replay: a
 * backup that produces plausible looking SQL and does not restore is worth
 * nothing. The rest pins the two things a backup adds over an extract - that the
 * scope is enumerated rather than listed, and that {@code bytes} counts the file
 * rather than the text that went into it.
 */
class BackupJobTest {

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

    @Test
    void aScopeBackupReplaysIntoAFreshDatabase() throws Exception {
        H2.exec(session, "create schema bk");
        H2.exec(session, "create table bk.parent ("
                + "id int not null primary key, code varchar(20) not null)");
        H2.exec(session, "create table bk.child ("
                + "id int not null primary key, parent_id int not null, note varchar(200), "
                + "constraint fk_child_parent foreign key (parent_id) references bk.parent(id))");
        H2.exec(session, "insert into bk.parent values (1, 'A'), (2, 'B')");
        H2.exec(session, "insert into bk.child values (10, 1, 'it''s fine'), (11, 2, null)");
        // A table outside the scope, to prove the scope is a filter and not
        // decoration.
        H2.exec(session, "create table public.elsewhere (id int)");
        H2.exec(session, "insert into public.elsewhere values (1)");

        Path out = tmp.resolve("backup.sql");
        JsonObject done = await(start(spec(out, "BK", "none", true, false, true)));
        assertEquals("done", done.get("state").getAsString(), done.toString());
        assertEquals(4, done.get("rows_done").getAsLong(), done.toString());
        // A backup cannot skip a row; the field is there so one progress shape
        // serves every kind of job.
        assertEquals(0, done.get("rows_skipped").getAsLong(), done.toString());
        assertEquals(Files.size(out), done.get("bytes").getAsLong(), done.toString());

        String script = read(out);
        assertTrue(script.contains("CREATE TABLE BK.CHILD"), script);
        assertTrue(!script.contains("ELSEWHERE"), "the scope was ignored:\n" + script);
        // Both CREATEs come before either ALTER; a cycle could not replay
        // otherwise, and CHILD sorts before PARENT.
        assertTrue(script.indexOf("ALTER TABLE") > script.lastIndexOf("CREATE TABLE"), script);
        // The data follows the references even though the enumeration is
        // alphabetical; without that the CHILD rows would be rejected by the
        // key that was just added.
        assertTrue(script.indexOf("INSERT INTO BK.PARENT")
                < script.indexOf("INSERT INTO BK.CHILD"), script);

        long fresh = H2.open(H2.freshUrl());
        try {
            H2.exec(fresh, "create schema bk");
            replay(fresh, out);
            assertEquals("2", one(fresh, "select count(*) from bk.parent"));
            assertEquals("2", one(fresh, "select count(*) from bk.child"));
            assertEquals("it's fine", one(fresh, "select note from bk.child where id = 10"));
            assertEquals("1", one(fresh,
                    "select count(*) from information_schema.table_constraints"
                            + " where table_schema = 'BK' and constraint_type = 'FOREIGN KEY'"));
        } finally {
            H2.close(fresh);
        }
    }

    @Test
    void gzipWritesAValidArchiveAndCountsItsBytes() throws Exception {
        H2.exec(session, "create schema gz");
        H2.exec(session, "create table gz.t (id int, v varchar(80))");
        H2.exec(session, "insert into gz.t select x, 'row number ' || x"
                + " from system_range(1, 2000)");

        Path out = tmp.resolve("backup.sql.gz");
        JsonObject done = await(start(spec(out, "GZ", "gzip", true, false, true)));
        assertEquals("done", done.get("state").getAsString(), done.toString());
        assertEquals(2000, done.get("rows_done").getAsLong(), done.toString());

        // The contract: bytes is what reached the file, compression included.
        long size = Files.size(out);
        assertEquals(size, done.get("bytes").getAsLong(), done.toString());

        String script = gunzip(out);
        assertTrue(script.contains("CREATE TABLE GZ.T"), script.substring(0, 200));
        assertTrue(script.contains("INSERT INTO GZ.T"), script.substring(0, 200));
        assertTrue(size < script.getBytes(StandardCharsets.UTF_8).length,
                "the file should be smaller than the script it holds");
    }

    @Test
    void includeDropPrependsDropStatements() throws Exception {
        H2.exec(session, "create schema dr");
        H2.exec(session, "create table dr.t (id int)");
        Path out = tmp.resolve("drop.sql");
        assertEquals("done",
                await(start(spec(out, "DR", "none", true, true, false))).get("state").getAsString());
        String script = read(out);
        assertTrue(script.indexOf("DROP TABLE IF EXISTS DR.T;") < script.indexOf("CREATE TABLE"),
                script);
    }

    @Test
    void anEmptyScopeProducesAnEmptyFileRatherThanAFailure() throws Exception {
        Path out = tmp.resolve("empty.sql");
        JsonObject done = await(start(spec(out, "NOSUCHSCHEMA", "none", true, false, true)));
        assertEquals("done", done.get("state").getAsString(), done.toString());
        assertEquals(0, done.get("rows_done").getAsLong(), done.toString());
        assertEquals(0, Files.size(out));
    }

    // ------------------------------------------------------- protocol errors

    @Test
    void aMissingOutputPathIsRejectedBeforeAnyThreadStarts() {
        JsonObject spec = spec(tmp.resolve("no.sql"), "PUBLIC", "none", true, false, false);
        spec.getAsJsonObject("output").remove("path");
        JsonObject err = H2.call(Ops.JOB_START, session, 0, spec).error();
        assertEquals("protocol", err.get("kind").getAsString());
        assertTrue(err.get("message").getAsString().contains("output.path"), err.toString());
    }

    @Test
    void anUnknownCompressionIsRejected() {
        JsonObject spec = spec(tmp.resolve("no.sql"), "PUBLIC", "brotli", true, false, false);
        assertEquals("protocol",
                H2.call(Ops.JOB_START, session, 0, spec).error().get("kind").getAsString());
    }

    @Test
    void aBackupOfNeitherDdlNorDataIsRejected() {
        JsonObject spec = spec(tmp.resolve("no.sql"), "PUBLIC", "none", false, false, false);
        JsonObject err = H2.call(Ops.JOB_START, session, 0, spec).error();
        assertEquals("protocol", err.get("kind").getAsString());
        assertTrue(err.get("message").getAsString().contains("at least one"), err.toString());
    }

    // --------------------------------------------------------------- helpers

    private static JsonObject spec(Path out, String schema, String compress, boolean ddl,
                                   boolean includeDrop, boolean data) {
        JsonObject spec = new JsonObject();
        spec.addProperty("kind", "backup");
        JsonObject scope = new JsonObject();
        scope.addProperty("schema", schema);
        spec.add("scope", scope);
        JsonObject output = new JsonObject();
        output.addProperty("path", out.toString());
        output.addProperty("charset", "UTF-8");
        output.addProperty("newline", "\n");
        spec.add("output", output);
        spec.addProperty("compress", compress);
        JsonObject d = new JsonObject();
        d.addProperty("include", ddl);
        d.addProperty("include_drop", includeDrop);
        d.addProperty("constraints", "alter");
        spec.add("ddl", d);
        JsonObject dt = new JsonObject();
        dt.addProperty("include", data);
        dt.addProperty("insert_batch_rows", 1);
        spec.add("data", dt);
        return spec;
    }

    private long start(JsonObject spec) {
        Resp r = H2.call(Ops.JOB_START, session, 0, spec);
        r.assertOk();
        return r.json().get("job").getAsLong();
    }

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

    private static String read(Path p) throws Exception {
        return new String(Files.readAllBytes(p), StandardCharsets.UTF_8);
    }

    private static String gunzip(Path p) throws Exception {
        ByteArrayOutputStream buf = new ByteArrayOutputStream();
        try (InputStream in = new GZIPInputStream(Files.newInputStream(p))) {
            byte[] chunk = new byte[8192];
            int n;
            while ((n = in.read(chunk)) > 0) {
                buf.write(chunk, 0, n);
            }
        }
        return new String(buf.toByteArray(), StandardCharsets.UTF_8);
    }

    /** Runs a generated script statement by statement, as ExtractJobTest does. */
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

    private static String one(long session, String sql) {
        long cursor = H2.query(session, sql).json().get("cursor").getAsLong();
        try {
            Batch batch = Resp.of(Bridge.call(Ops.FETCH, cursor, 1, null)).batch();
            if (batch.rowCount == 0 || batch.colCount == 0) {
                return null;
            }
            Batch.Column col = batch.columns[0];
            if (!col.valid[0]) {
                return null;
            }
            switch (col.kind) {
                case Batch.I64:  return Long.toString(col.i64[0]);
                case Batch.F64:  return Double.toString(col.f64[0]);
                case Batch.BOOL: return Boolean.toString(col.bools[0]);
                default:         return col.str(0);
            }
        } finally {
            Resp.of(Bridge.call(Ops.CLOSE_CURSOR, cursor, 0, null)).assertOk();
        }
    }
}
