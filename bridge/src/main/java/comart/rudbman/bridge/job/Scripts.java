package comart.rudbman.bridge.job;

import comart.rudbman.bridge.meta.Dialect;
import comart.rudbman.bridge.meta.Ident;

import java.io.IOException;
import java.sql.ResultSet;
import java.util.ArrayList;
import java.util.List;

/**
 * The statement shapes a generated script is made of, shared by
 * {@link ExtractJob} and {@link BackupJob}.
 *
 * <p>A backup is an extract without the object list, so it has to produce the
 * same bytes for the same rows. Keeping the {@code DROP} spelling and the
 * {@code INSERT} batching here is what guarantees that.
 */
final class Scripts {

    /** How often the byte counter is pulled out of the output stream. */
    private static final int BYTE_SYNC_ROWS = 256;

    private Scripts() {
    }

    /**
     * @param dialect   the target dialect
     * @param qualified the already-quoted table name
     * @return a {@code DROP TABLE} statement, with {@code IF EXISTS} on the
     *         products that have it. Oracle and Db2 do not (Oracle only from
     *         23ai), so there the statement fails on a missing table and the
     *         script has to be run past that error.
     */
    static String dropStatement(Dialect dialect, String qualified) {
        return dropStatement(dialect, "TABLE", qualified);
    }

    /**
     * @param dialect   the target dialect
     * @param keyword   what is dropped: {@code TABLE}, {@code VIEW},
     *                  {@code MATERIALIZED VIEW}, {@code SEQUENCE},
     *                  {@code FUNCTION}, {@code PROCEDURE}, {@code ALIAS}
     * @param qualified the already-quoted name
     * @return the {@code DROP}, with {@code IF EXISTS} where the product has it
     */
    static String dropStatement(Dialect dialect, String keyword, String qualified) {
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
        // Oracle's own way past the foreign keys that point at a table.
        String cascade = dialect == Dialect.ORACLE && "TABLE".equals(keyword)
                ? " CASCADE CONSTRAINTS" : "";
        return "DROP " + keyword + " " + (ifExists ? "IF EXISTS " : "") + qualified + cascade
                + ";";
    }

    /**
     * Takes one foreign key off a table that may not exist yet, so that the
     * tables can be dropped in any order; {@code null} where the product needs
     * no such step (see the caller).
     *
     * @param dialect   the target dialect
     * @param qualified the already-quoted table name
     * @param key       the already-quoted constraint name
     * @return the statement, terminated, or {@code null}
     */
    static String dropForeignKey(Dialect dialect, String qualified, String key) {
        switch (dialect) {
            case MYSQL:
            case MARIADB:
            case ORACLE:
            case SQLITE:
                return null;
            case SQLSERVER:
                return "IF OBJECT_ID(" + Ident.literal(qualified) + ", 'U') IS NOT NULL"
                        + " ALTER TABLE " + qualified + " DROP CONSTRAINT IF EXISTS " + key + ";";
            case POSTGRESQL:
            case H2:
                return "ALTER TABLE IF EXISTS " + qualified + " DROP CONSTRAINT IF EXISTS "
                        + key + ";";
            default:
                return "ALTER TABLE " + qualified + " DROP CONSTRAINT " + key + ";";
        }
    }

    /**
     * One routine, trigger or view as the product's own command-line client
     * runs it from a script file.
     *
     * <p>A body holds semicolons of its own, and every client has its own way
     * of being told where such a statement ends - none of them part of SQL, so
     * a script either carries the right one or does not replay:
     *
     * <ul>
     *   <li><b>MySQL, MariaDB</b>: {@code DELIMITER ;;} around the statement,
     *       as mysqldump writes it.</li>
     *   <li><b>Oracle</b>: a {@code /} on its own line after a PL/SQL unit,
     *       for SQL*Plus and SQLcl.</li>
     *   <li><b>SQL Server</b>: {@code GO} on both sides, because a view, a
     *       routine and a trigger must each be the only statement of their
     *       batch.</li>
     *   <li><b>Db2</b>: the CLP's {@code --#SET TERMINATOR} directive.</li>
     * </ul>
     *
     * <p>PostgreSQL, H2 and SQLite need nothing: a dollar-quoted body is one
     * token there, and SQLite's shell reads a trigger's {@code BEGIN … END}.
     *
     * @param dialect the source dialect
     * @param kind    {@code view}, {@code materialized view},
     *                {@code function}, {@code procedure}, {@code package},
     *                {@code trigger} or {@code sequence}
     * @param text    the object's DDL, as {@code DESCRIBE kind: "ddl"} returns it
     * @return the text to write, terminated
     */
    static String module(Dialect dialect, String kind, String text) {
        String body = text.strip();
        boolean procedural = "function".equals(kind) || "procedure".equals(kind)
                || "trigger".equals(kind);
        switch (dialect) {
            case MYSQL:
            case MARIADB:
                if (procedural) {
                    return "DELIMITER ;;\n" + withoutTerminator(body) + ";;\nDELIMITER ;";
                }
                return terminated(body);
            case ORACLE:
                if ("package".equals(kind)) {
                    // DBMS_METADATA already ends every unit in a slash.
                    return body;
                }
                if (procedural) {
                    return terminated(body) + "\n/";
                }
                return terminated(body);
            case SQLSERVER:
                if (!"sequence".equals(kind)) {
                    return "GO\n" + body + "\nGO";
                }
                return terminated(body);
            case DB2:
                if (procedural) {
                    return "--#SET TERMINATOR @\n" + withoutTerminator(body)
                            + "\n@\n--#SET TERMINATOR ;";
                }
                return terminated(body);
            default:
                return terminated(body);
        }
    }

    private static String terminated(String text) {
        return text.endsWith(";") ? text : text + ";";
    }

    private static String withoutTerminator(String text) {
        String out = text;
        while (out.endsWith(";")) {
            out = out.substring(0, out.length() - 1).stripTrailing();
        }
        return out;
    }

    /**
     * Streams a result set out as {@code INSERT} statements.
     *
     * @param job             the job, for progress and the cancellation flag
     * @param out             the output file
     * @param rs              the rows, positioned before the first
     * @param qualified       the already-quoted target table name
     * @param names           the column names, unquoted
     * @param types           the columns' {@link java.sql.Types} codes
     * @param dialect         the target dialect
     * @param id              the target's quoting rules
     * @param insertBatchRows how many rows share one {@code VALUES} clause
     * @throws Exception if the driver or the file fails
     */
    static void writeInserts(Jobs.Job job, ScriptOut out, ResultSet rs, String qualified,
                             String[] names, int[] types, Dialect dialect, Ident id,
                             int insertBatchRows) throws Exception {
        writeInserts(job, out, rs, qualified, names, types, dialect, id, insertBatchRows, false);
    }

    /**
     * {@link #writeInserts(Jobs.Job, ScriptOut, ResultSet, String, String[], int[], Dialect,
     * Ident, int)}, saying {@code OVERRIDING SYSTEM VALUE} when asked: what a
     * PostgreSQL table with a {@code GENERATED ALWAYS} identity needs to take
     * the values it had.
     */
    static void writeInserts(Jobs.Job job, ScriptOut out, ResultSet rs, String qualified,
                             String[] names, int[] types, Dialect dialect, Ident id,
                             int insertBatchRows, boolean overridingSystemValue)
            throws Exception {
        StringBuilder cols = new StringBuilder();
        for (String name : names) {
            if (cols.length() > 0) {
                cols.append(", ");
            }
            cols.append(id.q(name));
        }
        String head = "INSERT INTO " + qualified + " (" + cols + ")"
                + (overridingSystemValue ? " OVERRIDING SYSTEM VALUE" : "") + " VALUES";

        // Oracle has no multi-row VALUES clause; asking for one there would
        // produce a script that cannot run, so the request is clamped rather
        // than honored into a broken file.
        int batchRows = dialect == Dialect.ORACLE ? 1 : insertBatchRows;

        List<String> tuples = new ArrayList<>(batchRows);
        long sinceSync = 0;
        while (rs.next()) {
            if (job.shouldStop()) {
                break;
            }
            StringBuilder t = new StringBuilder("(");
            for (int i = 1; i <= names.length; i++) {
                if (i > 1) {
                    t.append(", ");
                }
                t.append(Literals.literal(rs, i, types[i - 1], dialect));
            }
            tuples.add(t.append(')').toString());
            job.addRows(1);
            if (tuples.size() >= batchRows) {
                flushTuples(out, head, tuples);
            }
            if (++sinceSync >= BYTE_SYNC_ROWS) {
                sinceSync = 0;
                job.addBytes(out.unreported());
            }
        }
        if (!tuples.isEmpty()) {
            flushTuples(out, head, tuples);
        }
    }

    private static void flushTuples(ScriptOut out, String head, List<String> tuples)
            throws IOException {
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
}
