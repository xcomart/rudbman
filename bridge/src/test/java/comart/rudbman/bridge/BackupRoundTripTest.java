package comart.rudbman.bridge;

import com.google.gson.JsonObject;
import comart.rudbman.bridge.support.H2;
import comart.rudbman.bridge.support.SchemaSnapshot;
import org.junit.jupiter.api.Test;
import org.junit.jupiter.api.Timeout;
import org.junit.jupiter.api.io.TempDir;

import java.io.IOException;
import java.nio.charset.StandardCharsets;
import java.nio.file.Files;
import java.nio.file.Path;
import java.sql.Connection;
import java.sql.DriverManager;
import java.sql.SQLException;
import java.sql.Statement;
import java.util.ArrayList;
import java.util.LinkedHashSet;
import java.util.List;
import java.util.Set;
import java.util.concurrent.TimeUnit;
import java.util.regex.Matcher;
import java.util.regex.Pattern;

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertTrue;
import static org.junit.jupiter.api.Assertions.fail;
import static org.junit.jupiter.api.Assumptions.assumeTrue;

/**
 * A schema backup, replayed with each product's own command-line client, gives
 * back the schema it was taken of: the same catalog, the same rows, the same
 * next value from every sequence and identity.
 *
 * <p>{@link BackupJobTest} proves the job on H2 through JDBC. This proves the
 * script against the servers and the clients people restore with - psql,
 * mysql, mariadb, sqlcmd, SQL*Plus, the Db2 CLP - which is where a script is
 * judged: every one of them reads a script its own way, and the catalogs hold
 * what JDBC metadata cannot see. Each product is checked three ways:
 *
 * <ol>
 *   <li>the backup restored into an emptied schema matches the source;</li>
 *   <li>the backup with its {@code DROP}s, replayed twice, matches it too;</li>
 *   <li>a backup of the restored schema is the backup it was restored from.</li>
 * </ol>
 *
 * <p>A snapshot ({@link SchemaSnapshot}) is the JDBC metadata, the product's
 * own catalog rows ({@code roundtrip/<product>-catalog.sql}), every row, and
 * probes of the next values. The fixtures ({@code roundtrip/<product>-setup.sql})
 * hold every kind of object the backup writes and the values that broke it
 * before. Their sequences and identities do not cache, because a cached one can
 * only resume at its cache boundary, which is by design and not a mismatch.
 *
 * <p>Opt-in like {@code crates/rudbman-jdbc/tests/containers.rs}, with the
 * same {@code RUDBMAN_TEST_*_URL} variables: a product with no URL is skipped.
 * The restore runs inside the server's container, named by
 * {@code RUDBMAN_TEST_*_CONTAINER} (by default the one {@code docker/compose.yml}
 * starts). H2 and SQLite need no server and always run; SQLite is replayed by
 * the {@code sqlite3} shell, or Python's {@code sqlite3} module without one.
 *
 * <pre>
 * docker compose -f docker/compose.yml up -d
 * export RUDBMAN_TEST_PG_URL=jdbc:postgresql://127.0.0.1:55432/rudbman   # and the others
 * cd bridge &amp;&amp; ./gradlew backupRoundTrip
 * </pre>
 *
 * <p>The test drops and recreates what it works in: the schema {@code bk} on
 * PostgreSQL, the database {@code bk} on MySQL and MariaDB, the database
 * {@code bkdb} on SQL Server, the user {@code BK} on Oracle and the schema
 * {@code BK} on Db2.
 */
@Timeout(value = 15, unit = TimeUnit.MINUTES)
class BackupRoundTripTest {

    private static final String FILE = "/tmp/rudbman-roundtrip.sql";

    @TempDir
    Path tmp;

    // ------------------------------------------------------------- products

    @Test
    void postgresqlBackupRestoresTheSameSchema() throws Exception {
        String url = url("PG");
        String user = env("RUDBMAN_TEST_PG_USER", "rudbman");
        String password = env("RUDBMAN_TEST_PG_PASSWORD", "rudbman");
        String container = env("RUDBMAN_TEST_PG_CONTAINER", "rudbman-test-postgres");
        String database = env("RUDBMAN_TEST_PG_DATABASE", "rudbman");
        Location at = new Location(url, "org.postgresql.Driver", user, password, null, "bk");
        Product p = new Product("pg", at) {
            @Override
            void reset() throws SQLException {
                sql(at, "drop schema if exists bk cascade", "create schema bk");
            }

            @Override
            void restore(Path script, Location into, boolean firstDropRun) throws Exception {
                copyInto(container, script);
                expectSuccess(run(null, "docker", "exec", container, "psql", "-X", "-q",
                        "-U", user, "-d", database, "-v", "ON_ERROR_STOP=1", "-f", FILE));
            }
        };
        roundTrip(p);
    }

    @Test
    void mysqlBackupRestoresTheSameSchema() throws Exception {
        roundTrip(mysqlFamily("MYSQL", "my", "mysql", "rudbman-test-mysql",
                "com.mysql.cj.jdbc.Driver", "utf8mb4_0900_ai_ci"));
    }

    @Test
    void mariadbBackupRestoresTheSameSchema() throws Exception {
        roundTrip(mysqlFamily("MARIADB", "ma", "mariadb", "rudbman-test-mariadb",
                "org.mariadb.jdbc.Driver", "utf8mb4_uca1400_ai_ci"));
    }

    private Product mysqlFamily(String key, String name, String client, String defaultContainer,
                                String driver, String collation) {
        String url = url(key);
        // Root: the fixture creates a database, and routines and triggers.
        String password = env("RUDBMAN_TEST_" + key + "_ROOT_PASSWORD", "rudbman");
        String container = env("RUDBMAN_TEST_" + key + "_CONTAINER", defaultContainer);
        Location at = new Location(url, driver, "root", password, "bk", null);
        return new Product(name, at) {
            @Override
            void reset() throws SQLException {
                sql(at, "drop database if exists bk",
                        "create database bk character set utf8mb4 collate " + collation);
            }

            @Override
            void restore(Path script, Location into, boolean firstDropRun) throws Exception {
                copyInto(container, script);
                expectSuccess(run(null, "docker", "exec", "-e", "MYSQL_PWD=" + password,
                        container, "sh", "-c", client + " -uroot --default-character-set=utf8mb4 < "
                                + FILE));
            }
        };
    }

    @Test
    void sqlServerBackupRestoresTheSameSchema() throws Exception {
        String url = url("MSSQL");
        String user = env("RUDBMAN_TEST_MSSQL_USER", "sa");
        String password = env("RUDBMAN_TEST_MSSQL_PASSWORD", "Rudbman!Passw0rd");
        String container = env("RUDBMAN_TEST_MSSQL_CONTAINER", "rudbman-test-mssql");
        Location master = new Location(url, "com.microsoft.sqlserver.jdbc.SQLServerDriver", user,
                password, null, null);
        Location at = new Location(url + ";databaseName=bkdb", master.driver, user, password,
                "bkdb", "bk");
        Product p = new Product("ms", at) {
            @Override
            void reset() throws SQLException {
                sql(master, "if db_id('bkdb') is not null begin"
                        + " alter database bkdb set single_user with rollback immediate;"
                        + " drop database bkdb; end", "create database bkdb");
                sql(at, "create schema bk");
            }

            @Override
            void restore(Path script, Location into, boolean firstDropRun) throws Exception {
                copyInto(container, script);
                // -I: sqlcmd starts with QUOTED_IDENTIFIER off; -f 65001: the
                // file is UTF-8.
                expectSuccess(run(null, "docker", "exec", container,
                        "/opt/mssql-tools18/bin/sqlcmd", "-C", "-S", "localhost", "-U", user,
                        "-P", password, "-d", "bkdb", "-b", "-I", "-f", "65001", "-i", FILE));
            }
        };
        roundTrip(p);
    }

    @Test
    void oracleBackupRestoresTheSameSchema() throws Exception {
        String url = url("ORACLE");
        String system = env("RUDBMAN_TEST_ORACLE_SYSTEM_PASSWORD", "rudbman");
        String container = env("RUDBMAN_TEST_ORACLE_CONTAINER", "rudbman-test-oracle");
        String service = env("RUDBMAN_TEST_ORACLE_SERVICE", "FREEPDB1");
        Location admin = new Location(url, "oracle.jdbc.OracleDriver", "system", system, null,
                null);
        Location at = new Location(url, admin.driver, "bk", "bk", null, "BK");
        Product p = new Product("ora", at) {
            @Override
            void reset() throws SQLException {
                sql(admin, "begin execute immediate 'drop user bk cascade';"
                                + " exception when others then if sqlcode != -1918 then raise;"
                                + " end if; end;",
                        "create user bk identified by bk quota unlimited on users",
                        "grant create session, create table, create view, create procedure,"
                                + " create trigger, create sequence, create materialized view"
                                + " to bk");
            }

            @Override
            void restore(Path script, Location into, boolean firstDropRun) throws Exception {
                copyInto(container, script);
                // Oracle has no DROP ... IF EXISTS: onto an empty schema the
                // DROPs fail, and only for want of the object.
                String onError = firstDropRun ? "continue" : "exit failure";
                Result r = run("whenever sqlerror " + onError + "\nwhenever oserror exit failure\n@"
                                + FILE + "\nexit\n", "docker", "exec", "-i",
                        "-e", "NLS_LANG=AMERICAN_AMERICA.AL32UTF8", container,
                        "sqlplus", "-s", "-L", "bk/bk@//localhost/" + service);
                // A PL/SQL unit that does not compile is a warning to SQL*Plus.
                assertTrue(!r.output.contains("compilation errors"), r.output);
                if (firstDropRun) {
                    onlyMissing(r.output, Pattern.compile("ORA-\\d{5}"),
                            Set.of("ORA-00942", "ORA-02289", "ORA-04043", "ORA-12003"));
                } else {
                    expectSuccess(r);
                }
            }

            @Override
            String normalizeSnapshot(String line) {
                // System-generated names, and the old tables the DROPs leave in
                // the recycle bin.
                return line.contains("BIN$") ? null : line
                        .replaceAll("ISEQ\\$\\$_\\d+", "ISEQ\\$\\$_N")
                        .replaceAll("SYS_C\\d+", "SYS_C");
            }

            @Override
            String normalizeScript(String line) {
                // A restored identity starts where its source left off, and
                // DBMS_METADATA says so.
                return line.replaceAll("START WITH \\d+", "START WITH n");
            }
        };
        roundTrip(p);
    }

    @Test
    void db2BackupRestoresTheSameSchema() throws Exception {
        String url = url("DB2");
        String user = env("RUDBMAN_TEST_DB2_USER", "rudbman");
        String password = env("RUDBMAN_TEST_DB2_PASSWORD", "rudbman");
        String container = env("RUDBMAN_TEST_DB2_CONTAINER", "rudbman-test-db2");
        String database = env("RUDBMAN_TEST_DB2_DATABASE", "RUDBMAN");
        Location at = new Location(url, "com.ibm.db2.jcc.DB2Driver", user, password, null, "BK");
        Product p = new Product("db2", at) {
            @Override
            void reset() throws SQLException {
                // By hand rather than with SYSPROC.ADMIN_DROP_SCHEMA, which
                // needs a SYSTOOLSPACE a new database does not have.
                try (Connection c = at.connect(); Statement st = c.createStatement()) {
                    List<String> drops = new ArrayList<>();
                    try (java.sql.ResultSet rs = st.executeQuery("select 'drop trigger BK.\"'"
                            + " || trigname || '\"' from syscat.triggers where trigschema = 'BK'"
                            + " union all select 'drop view BK.\"' || viewname || '\"'"
                            + " from syscat.views v where viewschema = 'BK' and not exists"
                            + " (select 1 from syscat.indexes i where i.viewschema = v.viewschema"
                            + " and i.viewname = v.viewname)"
                            + " union all select 'drop specific ' || case routinetype when 'P'"
                            + " then 'procedure' else 'function' end || ' BK.\"' || specificname"
                            + " || '\"' from syscat.routines where routineschema = 'BK'"
                            + " union all select 'drop table BK.\"' || tabname || '\"'"
                            + " from syscat.tables where tabschema = 'BK' and type = 'T'"
                            + " union all select 'drop sequence BK.\"' || seqname || '\"'"
                            + " from syscat.sequences where seqschema = 'BK' and seqtype = 'S'")) {
                        while (rs.next()) {
                            drops.add(rs.getString(1));
                        }
                    }
                    // In passes: what a later object depends on goes once its
                    // dependents have.
                    for (int pass = 0; pass < 5 && !drops.isEmpty(); pass++) {
                        drops.removeIf(d -> {
                            try {
                                st.execute(d);
                                return true;
                            } catch (SQLException e) {
                                return false;
                            }
                        });
                    }
                    assertTrue(drops.isEmpty(), "could not empty the schema BK: " + drops);
                    try (java.sql.ResultSet rs = st.executeQuery("select 1 from syscat.schemata"
                            + " where schemaname = 'BK'")) {
                        if (!rs.next()) {
                            st.execute("create schema BK");
                        }
                    }
                }
            }

            @Override
            void restore(Path script, Location into, boolean firstDropRun) throws Exception {
                copyInto(container, script);
                expectSuccess(run(null, "docker", "exec", container, "chmod", "644", FILE));
                // -s stops at the first error; the DROPs of a first run onto an
                // empty schema are expected to fail, so that run goes on.
                Result r = run(null, "docker", "exec", container, "su", "-", user, "-c",
                        "export DB2CODEPAGE=1208; db2 connect to " + database + " >/dev/null;"
                                + " db2 -tv" + (firstDropRun ? "" : "s") + "f " + FILE
                                + "; rc=$?; db2 terminate >/dev/null; exit $rc");
                if (firstDropRun) {
                    onlyMissing(r.output, Pattern.compile("SQL\\d{4,5}N"),
                            Set.of("SQL0204N", "SQL0458N"));
                } else {
                    // 1 and 2 are warnings, 4 and up errors.
                    assertTrue(r.exit < 4, "exit " + r.exit + "\n" + tail(r.output));
                }
            }

            @Override
            String normalizeSnapshot(String line) {
                return line.replaceAll("SQL\\d{15}", "SQLnnn");
            }
        };
        roundTrip(p);
    }

    @Test
    void h2BackupRestoresTheSameSchema() throws Exception {
        Location src = h2(tmp.resolve("src"));
        Product p = new Product("h2", src) {
            @Override
            void reset() {
            }

            @Override
            Location target(int n) {
                return h2(tmp.resolve("dst" + n));
            }

            @Override
            void restore(Path script, Location into, boolean firstDropRun) throws Exception {
                // The schema exists before a restore, as on every product.
                sql(into, "create schema if not exists BK");
                org.h2.tools.RunScript.execute(into.url, into.user, into.password,
                        script.toString(), StandardCharsets.UTF_8, false);
            }

            @Override
            String normalizeSnapshot(String line) {
                // The indexes H2 names after the constraint that made them.
                return line.replaceAll("PRIMARY_KEY_[0-9A-F]+", "PRIMARY_KEY_N")
                        .replaceAll("CONSTRAINT(_[0-9A-F]+)?_INDEX_[0-9A-F]+",
                                "CONSTRAINT_INDEX_N");
            }
        };
        roundTrip(p);
    }

    private static Location h2(Path file) {
        return new Location("jdbc:h2:file:" + file, "org.h2.Driver", "sa", "", null, "BK");
    }

    @Test
    void sqliteBackupRestoresTheSameSchema() throws Exception {
        List<String> shell = sqliteShell();
        assumeTrue(shell != null, "skipped: neither sqlite3 nor python3 is on the PATH");
        Location src = sqlite(tmp.resolve("src.db"));
        Product p = new Product("sl", src) {
            @Override
            void reset() {
            }

            @Override
            Location target(int n) {
                return sqlite(tmp.resolve("dst" + n + ".db"));
            }

            @Override
            void restore(Path script, Location into, boolean firstDropRun) throws Exception {
                List<String> cmd = new ArrayList<>(shell);
                String file = into.url.substring("jdbc:sqlite:".length());
                if ("sqlite3".equals(cmd.get(0))) {
                    cmd.add(file);
                    expectSuccess(run(Files.readString(script), cmd.toArray(new String[0])));
                } else {
                    cmd.add(file);
                    cmd.add(script.toString());
                    expectSuccess(run(null, cmd.toArray(new String[0])));
                }
            }
        };
        roundTrip(p);
    }

    private static Location sqlite(Path file) {
        return new Location("jdbc:sqlite:" + file, "org.sqlite.JDBC", "", "", null, null);
    }

    /** The {@code sqlite3} shell, or Python's module, which splits a script the same way. */
    private static List<String> sqliteShell() {
        try {
            if (run(".quit\n", "sqlite3", ":memory:").exit == 0) {
                return List.of("sqlite3", "-bail");
            }
        } catch (IOException | InterruptedException ignored) {
            // Not installed.
        }
        try {
            if (run(null, "python3", "-c", "import sqlite3").exit == 0) {
                return List.of("python3", "-I", "-c", "import sqlite3, sys;"
                        + " c = sqlite3.connect(sys.argv[1]);"
                        + " c.executescript(open(sys.argv[2], encoding='utf-8').read());"
                        + " c.close()");
            }
        } catch (IOException | InterruptedException ignored) {
            // Not installed either.
        }
        return null;
    }

    // ------------------------------------------------------------ the check

    /** Where a schema is: how to connect, and the scope a backup names. */
    static final class Location {
        final String url;
        final String driver;
        final String user;
        final String password;
        final String catalog;
        final String schema;

        Location(String url, String driver, String user, String password, String catalog,
                 String schema) {
            this.url = url;
            this.driver = driver;
            this.user = user;
            this.password = password;
            this.catalog = catalog;
            this.schema = schema;
        }

        Connection connect() throws SQLException {
            return DriverManager.getConnection(url, user, password);
        }
    }

    /** One product: how to empty its schema and how its client replays a script. */
    abstract static class Product {
        final String name;
        final Location source;

        Product(String name, Location source) {
            this.name = name;
            this.source = source;
        }

        /** Empties the schema the fixture goes into. */
        abstract void reset() throws Exception;

        /** Replays a script with the product's client; fails the test if it fails. */
        abstract void restore(Path script, Location into, boolean firstDropRun) throws Exception;

        /**
         * The schema the {@code n}th restore goes into, emptied: the source's
         * own place on a server, a file of its own for H2 and SQLite.
         */
        Location target(int n) throws Exception {
            reset();
            return source;
        }

        /** A snapshot line with what differs by nature taken out, or {@code null} to drop it. */
        String normalizeSnapshot(String line) {
            return line;
        }

        /** A script line likewise, for the comparison of two backups. */
        String normalizeScript(String line) {
            return line;
        }
    }

    private void roundTrip(Product p) throws Exception {
        p.reset();
        String catalog = SchemaSnapshot.resource("/roundtrip/" + p.name + "-catalog.sql");
        try (Connection c = p.source.connect(); Statement st = c.createStatement()) {
            for (String stmt : SchemaSnapshot.resource("/roundtrip/" + p.name + "-setup.sql")
                    .split("\n--;;\n")) {
                if (!stmt.isBlank()) {
                    try {
                        st.execute(stmt.strip());
                    } catch (SQLException e) {
                        fail("the fixture failed at: " + stmt.strip() + "\n" + e.getMessage());
                    }
                }
            }
        }
        Path backup = backup(p.source, false, "backup.sql");
        Path withDrops = backup(p.source, true, "backup-drop.sql");
        List<String> source = snapshot(p, p.source, catalog);

        Location first = p.target(1);
        p.restore(backup, first, false);
        Path again = backup(first, false, "backup-again.sql");
        List<String> restored = snapshot(p, first, catalog);
        same(source, restored, p.name + ": the restore differs from the source");

        Location second = p.target(2);
        p.restore(withDrops, second, true);
        p.restore(withDrops, second, false);
        List<String> replayed = snapshot(p, second, catalog);
        same(source, replayed, p.name + ": the backup with its DROPs, run twice, differs");

        same(script(p, backup), script(p, again),
                p.name + ": a backup of the restore differs from the backup restored");
    }

    /** Runs the backup job over a location, every kind of object and the rows. */
    private Path backup(Location at, boolean drops, String file) throws Exception {
        JsonObject open = new JsonObject();
        open.addProperty("url", at.url);
        open.addProperty("driver_class", at.driver);
        open.addProperty("username", at.user);
        open.addProperty("password", at.password);
        long session = H2.call(Ops.OPEN_SESSION, 0, 0, open).num("session");
        try {
            Path out = tmp.resolve(file);
            JsonObject spec = new JsonObject();
            spec.addProperty("kind", "backup");
            JsonObject scope = new JsonObject();
            if (at.catalog != null) {
                scope.addProperty("catalog", at.catalog);
            }
            if (at.schema != null) {
                scope.addProperty("schema", at.schema);
            }
            spec.add("scope", scope);
            JsonObject output = new JsonObject();
            output.addProperty("path", out.toString());
            output.addProperty("charset", "UTF-8");
            output.addProperty("newline", "\n");
            spec.add("output", output);
            spec.addProperty("compress", "none");
            JsonObject ddl = new JsonObject();
            ddl.addProperty("include", true);
            ddl.addProperty("include_drop", drops);
            ddl.addProperty("constraints", "alter");
            spec.add("ddl", ddl);
            JsonObject objects = new JsonObject();
            for (String kind : new String[]{"sequences", "views", "routines", "triggers"}) {
                objects.addProperty(kind, true);
            }
            spec.add("objects", objects);
            JsonObject data = new JsonObject();
            data.addProperty("include", true);
            data.addProperty("insert_batch_rows", 1);
            spec.add("data", data);
            long job = H2.call(Ops.JOB_START, session, 0, spec).num("job");
            JsonObject done = null;
            for (int i = 0; i < 12000 && done == null; i++) {
                JsonObject poll = H2.call(Ops.JOB_POLL, job, 0, null).json();
                if (!"running".equals(poll.get("state").getAsString())) {
                    done = poll;
                } else {
                    Thread.sleep(25);
                }
            }
            assertTrue(done != null, "the backup never finished");
            assertEquals("done", done.get("state").getAsString(), done.toString());
            assertEquals(0, done.getAsJsonArray("errors").size(),
                    "the backup skipped objects: " + done);
            return out;
        } finally {
            H2.call(Ops.CLOSE_SESSION, session, 0, null);
        }
    }

    private static List<String> snapshot(Product p, Location at, String catalog)
            throws SQLException {
        try (Connection c = at.connect()) {
            List<String> out = new ArrayList<>();
            // Oracle's materialized view log and refresh tables are not tables
            // the schema owner made.
            for (String line : SchemaSnapshot.of(c, at.catalog, at.schema, catalog,
                    Pattern.compile("^(MLOG\\$|RUPD\\$)"))) {
                String kept = p.normalizeSnapshot(line);
                if (kept != null) {
                    out.add(kept);
                }
            }
            return out;
        }
    }

    private static List<String> script(Product p, Path file) throws IOException {
        List<String> out = new ArrayList<>();
        for (String line : Files.readAllLines(file, StandardCharsets.UTF_8)) {
            out.add(p.normalizeScript(line));
        }
        return out;
    }

    /** Equal lists, or a failure that shows the lines each side alone has. */
    private static void same(List<String> expected, List<String> actual, String what) {
        if (expected.equals(actual)) {
            return;
        }
        Set<String> only = new LinkedHashSet<>(expected);
        actual.forEach(only::remove);
        Set<String> extra = new LinkedHashSet<>(actual);
        expected.forEach(extra::remove);
        StringBuilder sb = new StringBuilder(what).append('\n');
        only.stream().limit(40).forEach(l -> sb.append("- ").append(l).append('\n'));
        extra.stream().limit(40).forEach(l -> sb.append("+ ").append(l).append('\n'));
        if (only.isEmpty() && extra.isEmpty()) {
            sb.append("(the same lines in another order or count)");
        }
        fail(sb.toString());
    }

    // -------------------------------------------------------------- helpers

    /** The product's URL, or the test is skipped. */
    private static String url(String key) {
        String url = System.getenv("RUDBMAN_TEST_" + key + "_URL");
        assumeTrue(url != null && !url.isEmpty(), "skipped: RUDBMAN_TEST_" + key + "_URL is unset;"
                + " start the server with `docker compose -f docker/compose.yml up -d`");
        return url;
    }

    private static String env(String name, String fallback) {
        String v = System.getenv(name);
        return v == null || v.isEmpty() ? fallback : v;
    }

    private static void sql(Location at, String... statements) throws SQLException {
        try (Connection c = at.connect(); Statement st = c.createStatement()) {
            for (String s : statements) {
                st.execute(s);
            }
        }
    }

    private static void copyInto(String container, Path script) throws Exception {
        expectSuccess(run(null, "docker", "cp", script.toString(), container + ":" + FILE));
    }

    static final class Result {
        final int exit;
        final String output;

        Result(int exit, String output) {
            this.exit = exit;
            this.output = output;
        }
    }

    private static Result run(String input, String... command)
            throws IOException, InterruptedException {
        ProcessBuilder pb = new ProcessBuilder(command).redirectErrorStream(true);
        Process proc = pb.start();
        if (input != null) {
            proc.getOutputStream().write(input.getBytes(StandardCharsets.UTF_8));
        }
        proc.getOutputStream().close();
        byte[] out = proc.getInputStream().readAllBytes();
        if (!proc.waitFor(10, TimeUnit.MINUTES)) {
            proc.destroyForcibly();
            fail(String.join(" ", command) + " did not finish");
        }
        return new Result(proc.exitValue(), new String(out, StandardCharsets.UTF_8));
    }

    private static void expectSuccess(Result r) {
        assertEquals(0, r.exit, tail(r.output));
    }

    /** Every error code in a client's output is one of a missing object's. */
    private static void onlyMissing(String output, Pattern code, Set<String> allowed) {
        Matcher m = code.matcher(output);
        while (m.find()) {
            assertTrue(allowed.contains(m.group()), "unexpected " + m.group() + " in the first"
                    + " run of the DROP variant\n" + tail(output));
        }
    }

    private static String tail(String output) {
        return output.length() <= 6000 ? output : "..." + output.substring(output.length() - 6000);
    }
}
