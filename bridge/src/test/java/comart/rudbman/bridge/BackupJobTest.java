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
import java.util.List;
import java.util.zip.GZIPInputStream;

import static org.junit.jupiter.api.Assertions.assertArrayEquals;
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
        // The scope it was taken of, for a restore into another one.
        assertTrue(script.startsWith("-- rudbman backup: {\"schema\":\"BK\"}\n"), script);
        // H2's own DDL, not a reconstruction.
        assertTrue(script.contains("CREATE MEMORY TABLE \"BK\".\"CHILD\""), script);
        assertTrue(!script.contains("ELSEWHERE"), "the scope was ignored:\n" + script);
        // Both CREATEs come before the foreign key; a cycle could not replay
        // otherwise, and CHILD sorts before PARENT.
        assertTrue(script.indexOf("FOREIGN KEY") > script.lastIndexOf("CREATE MEMORY TABLE"),
                script);
        // Nothing but tables was asked for.
        assertTrue(!script.contains("CREATE SEQUENCE") && !script.contains("VIEW"), script);
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

    /**
     * A schema with one of everything, backed up whole and replayed: what makes
     * the backup a dump rather than a copy of the tables.
     */
    @Test
    void aFullBackupWritesEveryKindOfObjectAndReplays() throws Exception {
        H2.exec(session, "create schema fb");
        H2.exec(session, "create sequence fb.seq1 start with 5 increment by 2");
        H2.exec(session, "select next value for fb.seq1");
        H2.exec(session, "select next value for fb.seq1");
        H2.exec(session, "create table fb.parent (id int primary key,"
                + " code varchar(20) not null, constraint ck_code check (char_length(code) > 0))");
        H2.exec(session, "create table fb.child ("
                + "id int generated by default as identity primary key,"
                + " parent_id int not null references fb.parent(id),"
                + " twice int generated always as (id * 2))");
        H2.exec(session, "insert into fb.parent values (1, 'a'), (2, 'b')");
        H2.exec(session, "insert into fb.child (parent_id) values (1), (2), (1)");
        // A_DEP sorts before the view it reads.
        H2.exec(session, "create view fb.z_base as select * from fb.parent");
        H2.exec(session, "create view fb.a_dep as select id from fb.z_base");
        H2.exec(session, "create alias fb.half for"
                + " 'comart.rudbman.bridge.support.H2Objects.half'");
        H2.exec(session, "create trigger fb.trg before insert on fb.parent for each row"
                + " call 'comart.rudbman.bridge.support.H2Objects$Noop'");
        // A domain the table is declared with, an identity that refuses a
        // value, and the two types whose text H2 does not read back.
        H2.exec(session, "create domain fb.posint as int check (value > 0)");
        H2.exec(session, "create table fb.kinds (id int generated always as identity primary key,"
                + " p fb.posint, arr int array, js json)");
        H2.exec(session, "insert into fb.kinds (p, arr, js) values (5, array[1, null, 3],"
                + " json '{\"a\":[1,\"x;y\"]}')");

        Path out = tmp.resolve("full.sql");
        JsonObject spec = spec(out, "FB", "none", true, false, true);
        JsonObject objects = new JsonObject();
        for (String kind : new String[]{"sequences", "views", "routines", "triggers"}) {
            objects.addProperty(kind, true);
        }
        spec.add("objects", objects);
        JsonObject done = await(start(spec));
        assertEquals("done", done.get("state").getAsString(), done.toString());
        assertEquals(0, done.getAsJsonArray("errors").size(), done.toString());

        String script = read(out);
        int sequence = script.indexOf("CREATE SEQUENCE \"FB\".\"SEQ1\"");
        int table = script.indexOf("CREATE MEMORY TABLE \"FB\".\"CHILD\"");
        int alias = script.indexOf("ALIAS \"FB\".\"HALF\"");
        int base = script.indexOf("VIEW \"FB\".\"Z_BASE\"");
        int dependent = script.indexOf("VIEW \"FB\".\"A_DEP\"");
        int rows = script.indexOf("INSERT INTO");
        int trigger = script.indexOf("TRIGGER \"FB\".\"TRG\"");
        assertEquals(trigger, script.lastIndexOf("TRIGGER \"FB\".\"TRG\""),
                "the trigger is written once:\n" + script);
        assertTrue(sequence >= 0 && table > sequence, script);
        assertTrue(alias > table && base > alias, script);
        assertTrue(dependent > base, "a view must follow the view it reads:\n" + script);
        assertTrue(rows > dependent && trigger > rows,
                "triggers come after the rows, so loading them fires none:\n" + script);
        // The computed column is recreated as computed, so it takes no value.
        assertTrue(!script.contains("TWICE) VALUES"), script);

        long fresh = H2.open(H2.freshUrl());
        try {
            H2.exec(fresh, "create schema fb");
            replay(fresh, out);
            assertEquals("3", one(fresh, "select count(*) from fb.child"));
            assertEquals("6", one(fresh, "select twice from fb.child where id = 3"));
            assertEquals("2", one(fresh, "select count(*) from fb.a_dep"));
            assertEquals("3", one(fresh, "select fb.half(7)"));
            assertEquals("1", one(fresh, "select count(*) from information_schema.triggers"
                    + " where trigger_schema = 'FB'"));
            assertEquals("1", one(fresh, "select count(*) from information_schema.table_constraints"
                    + " where table_schema = 'FB' and constraint_type = 'CHECK'"));
            // The counters carry on from where the source left them.
            assertEquals("9", one(fresh, "select next value for fb.seq1"));
            H2.exec(fresh, "insert into fb.child (parent_id) values (2)");
            assertEquals("4", one(fresh, "select max(id) from fb.child"));
            assertEquals("5", one(fresh, "select p from fb.kinds where id = 1"));
            assertEquals("3", one(fresh, "select arr[3] from fb.kinds"));
            // A JSON object, not a JSON string holding its text.
            assertEquals("true", one(fresh, "select js is json object from fb.kinds"));
            assertEquals("POSINT", one(fresh, "select domain_name from information_schema.columns"
                    + " where table_name = 'KINDS' and column_name = 'P'"));
        } finally {
            H2.close(fresh);
        }

        // With its DROPs the same backup replays over itself: the keys come off
        // first, so the order the tables drop in does not matter.
        Path again = tmp.resolve("full-drop.sql");
        JsonObject withDrops = spec(again, "FB", "none", true, true, true);
        withDrops.add("objects", objects);
        assertEquals("done", await(start(withDrops)).get("state").getAsString());
        String dropping = read(again);
        assertTrue(dropping.indexOf("DROP CONSTRAINT IF EXISTS")
                < dropping.indexOf("DROP TABLE IF EXISTS"), dropping);
        assertTrue(dropping.contains("DROP ALIAS IF EXISTS FB.HALF;"), dropping);
        long twice = H2.open(H2.freshUrl());
        try {
            H2.exec(twice, "create schema fb");
            replay(twice, again);
            replay(twice, again);
            assertEquals("3", one(twice, "select count(*) from fb.child"));
            assertEquals("2", one(twice, "select count(*) from fb.a_dep"));
        } finally {
            H2.close(twice);
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
        assertTrue(script.contains("CREATE MEMORY TABLE \"GZ\".\"T\""),
                script.substring(0, 200));
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
        assertTrue(script.indexOf("DROP TABLE IF EXISTS DR.T;")
                < script.indexOf("CREATE MEMORY TABLE"), script);
    }

    @Test
    void anEmptyScopeProducesAnEmptyFileRatherThanAFailure() throws Exception {
        Path out = tmp.resolve("empty.sql");
        JsonObject done = await(start(spec(out, "NOSUCHSCHEMA", "none", true, false, true)));
        assertEquals("done", done.get("state").getAsString(), done.toString());
        assertEquals(0, done.get("rows_done").getAsLong(), done.toString());
        assertEquals(0, Files.size(out));
    }

    @Test
    void aCanceledBackupLeavesThePreviousBackupInPlace() throws Exception {
        H2.exec(session, "create schema bg");
        H2.exec(session, "create table bg.big (id int, v varchar(80))");
        H2.exec(session, "insert into bg.big select x, 'row number ' || x"
                + " from system_range(1, 200000)");

        // The case the rule exists for: last night's backup is at the path, and
        // tonight's is canceled half way. Truncating up front would have lost
        // both.
        Path out = tmp.resolve("nightly.sql.gz");
        byte[] previous = "-- last night's backup\n".getBytes(StandardCharsets.UTF_8);
        Files.write(out, previous);
        long job = start(spec(out, "BG", "gzip", true, false, true));
        awaitRows(job);

        H2.call(Ops.JOB_CANCEL, job, 0, null);
        JsonObject end = await(job);
        assertEquals("canceled", end.get("state").getAsString(), end.toString());
        assertTrue(end.get("rows_done").getAsLong() < 200000, end.toString());

        assertArrayEquals(previous, Files.readAllBytes(out));
        assertEquals(List.of(out), ExtractJobTest.listing(tmp));
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

    /** Polls until the job has written at least one row. */
    private void awaitRows(long job) throws Exception {
        for (int i = 0; i < 1200; i++) {
            JsonObject p = H2.call(Ops.JOB_POLL, job, 0, null).json();
            if (p.get("rows_done").getAsLong() > 0) {
                return;
            }
            if (!"running".equals(p.get("state").getAsString())) {
                throw new AssertionError("job finished before producing rows: " + p);
            }
            Thread.sleep(2);
        }
        throw new AssertionError("job " + job + " produced no rows");
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
