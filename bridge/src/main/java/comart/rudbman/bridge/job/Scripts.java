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
