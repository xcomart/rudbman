package comart.rudbman.bridge.meta;

import java.util.ArrayList;
import java.util.List;

/**
 * Builds the "insert this row, or update it if the key is already there"
 * statement each product spells differently (architecture.md 6).
 *
 * <p>There is no standard form that runs everywhere, so this is a dialect
 * switch and nothing else:
 *
 * <ul>
 *   <li>PostgreSQL and SQLite: {@code INSERT … ON CONFLICT (key) DO UPDATE};</li>
 *   <li>MySQL and MariaDB: {@code INSERT … ON DUPLICATE KEY UPDATE};</li>
 *   <li>H2: {@code MERGE INTO … KEY (…) VALUES (…)}, H2's own short form -
 *       fewer moving parts than a standard {@code MERGE} and the only one this
 *       bridge's test suite can actually execute;</li>
 *   <li>Oracle, SQL Server and Db2: standard {@code MERGE}, over a one-row
 *       source built from the bind parameters.</li>
 * </ul>
 *
 * <p>{@link Dialect#OTHER} has no portable spelling at all, so
 * {@link #supported} answers {@code false} and the caller rejects the request.
 * Emitting a guess would produce a statement that either fails or, worse,
 * silently inserts duplicates.
 *
 * <p>Every form takes exactly one bind parameter per column, in column order, so
 * the caller binds a transfer's row the same way whatever the product is.
 */
public final class Upsert {

    private Upsert() {
    }

    /**
     * @param dialect the target product
     * @return whether an upsert statement can be built for it
     */
    public static boolean supported(Dialect dialect) {
        return dialect != Dialect.OTHER;
    }

    /**
     * Builds the statement.
     *
     * @param dialect   the target product
     * @param id        the target's quoting rules
     * @param qualified the target table, already qualified and quoted
     * @param columns   every column being written, unquoted, in bind order
     * @param keys      the conflict key columns, unquoted; must be a subset of
     *                  {@code columns}
     * @return SQL with one {@code ?} per column, in {@code columns} order
     * @throws IllegalArgumentException if the dialect has no upsert form
     */
    public static String sql(Dialect dialect, Ident id, String qualified, List<String> columns,
                             List<String> keys) {
        if (!supported(dialect)) {
            throw new IllegalArgumentException("no portable upsert for " + dialect);
        }
        List<String> updates = nonKeyColumns(columns, keys);
        switch (dialect) {
            case POSTGRESQL:
            case SQLITE:
                return onConflict(id, qualified, columns, keys, updates);
            case MYSQL:
            case MARIADB:
                return onDuplicateKey(id, qualified, columns, keys, updates);
            case H2:
                return h2Merge(id, qualified, columns, keys);
            default:
                return merge(dialect, id, qualified, columns, keys, updates);
        }
    }

    /** @return the columns that are not part of the conflict key, in order */
    private static List<String> nonKeyColumns(List<String> columns, List<String> keys) {
        List<String> out = new ArrayList<>();
        for (String c : columns) {
            if (!contains(keys, c)) {
                out.add(c);
            }
        }
        return out;
    }

    /**
     * Case-insensitive membership.
     *
     * <p>The key columns come from {@link java.sql.DatabaseMetaData} in the
     * server's storage case while the written columns come from a result set's
     * labels, so {@code id} and {@code ID} are routinely the same column.
     */
    private static boolean contains(List<String> list, String name) {
        for (String s : list) {
            if (s.equalsIgnoreCase(name)) {
                return true;
            }
        }
        return false;
    }

    private static String columnList(Ident id, List<String> cols) {
        StringBuilder sb = new StringBuilder();
        for (String c : cols) {
            if (sb.length() > 0) {
                sb.append(", ");
            }
            sb.append(id.q(c));
        }
        return sb.toString();
    }

    private static String placeholders(int n) {
        StringBuilder sb = new StringBuilder();
        for (int i = 0; i < n; i++) {
            sb.append(i == 0 ? "?" : ", ?");
        }
        return sb.toString();
    }

    private static String insertHead(Ident id, String qualified, List<String> columns) {
        return "INSERT INTO " + qualified + " (" + columnList(id, columns) + ") VALUES ("
                + placeholders(columns.size()) + ")";
    }

    private static String onConflict(Ident id, String qualified, List<String> columns,
                                     List<String> keys, List<String> updates) {
        StringBuilder sb = new StringBuilder(insertHead(id, qualified, columns));
        sb.append(" ON CONFLICT (").append(columnList(id, keys)).append(')');
        if (updates.isEmpty()) {
            // Every column is part of the key: there is nothing an update could
            // change, and DO NOTHING is the honest spelling of that.
            return sb.append(" DO NOTHING").toString();
        }
        sb.append(" DO UPDATE SET ");
        for (int i = 0; i < updates.size(); i++) {
            String c = id.q(updates.get(i));
            sb.append(i == 0 ? "" : ", ").append(c).append(" = EXCLUDED.").append(c);
        }
        return sb.toString();
    }

    private static String onDuplicateKey(Ident id, String qualified, List<String> columns,
                                         List<String> keys, List<String> updates) {
        StringBuilder sb = new StringBuilder(insertHead(id, qualified, columns));
        sb.append(" ON DUPLICATE KEY UPDATE ");
        // MySQL's clause cannot be empty; assigning a key column to itself is
        // the idiomatic no-op.
        List<String> assign = updates.isEmpty() ? keys.subList(0, 1) : updates;
        for (int i = 0; i < assign.size(); i++) {
            String c = id.q(assign.get(i));
            sb.append(i == 0 ? "" : ", ").append(c).append(" = VALUES(").append(c).append(')');
        }
        return sb.toString();
    }

    /**
     * H2's own {@code MERGE}: the key is named directly and the row is a plain
     * {@code VALUES} list, so no source alias and no match clauses are needed.
     */
    private static String h2Merge(Ident id, String qualified, List<String> columns,
                                  List<String> keys) {
        return "MERGE INTO " + qualified + " (" + columnList(id, columns) + ") KEY ("
                + columnList(id, keys) + ") VALUES (" + placeholders(columns.size()) + ")";
    }

    /**
     * Standard {@code MERGE} for Oracle, SQL Server and Db2.
     *
     * <p>The source row is the bind parameters themselves. Oracle has no
     * {@code VALUES} table constructor, so there it is a {@code SELECT … FROM
     * dual}; SQL Server wants the statement terminated with a semicolon, which
     * is a documented requirement of {@code MERGE} and not a style choice.
     *
     * <p>Key columns are never assigned in {@code WHEN MATCHED}: Oracle rejects
     * updating a column that the {@code ON} clause joins on.
     */
    private static String merge(Dialect dialect, Ident id, String qualified, List<String> columns,
                                List<String> keys, List<String> updates) {
        String src = "s";
        String dst = "d";
        StringBuilder sb = new StringBuilder("MERGE INTO ").append(qualified).append(' ');
        if (dialect != Dialect.ORACLE) {
            sb.append("AS ");
        }
        sb.append(dst).append(" USING ");
        if (dialect == Dialect.ORACLE) {
            sb.append("(SELECT ");
            for (int i = 0; i < columns.size(); i++) {
                sb.append(i == 0 ? "" : ", ").append("? AS ").append(id.q(columns.get(i)));
            }
            sb.append(" FROM dual) ").append(src);
        } else {
            sb.append("(VALUES (").append(placeholders(columns.size())).append(")) AS ")
                    .append(src).append(" (").append(columnList(id, columns)).append(')');
        }
        sb.append(" ON (");
        for (int i = 0; i < keys.size(); i++) {
            String c = id.q(keys.get(i));
            sb.append(i == 0 ? "" : " AND ")
                    .append(dst).append('.').append(c).append(" = ").append(src).append('.')
                    .append(c);
        }
        sb.append(')');
        if (!updates.isEmpty()) {
            sb.append(" WHEN MATCHED THEN UPDATE SET ");
            for (int i = 0; i < updates.size(); i++) {
                String c = id.q(updates.get(i));
                sb.append(i == 0 ? "" : ", ")
                        .append(dst).append('.').append(c).append(" = ").append(src).append('.')
                        .append(c);
            }
        }
        sb.append(" WHEN NOT MATCHED THEN INSERT (").append(columnList(id, columns))
                .append(") VALUES (");
        for (int i = 0; i < columns.size(); i++) {
            sb.append(i == 0 ? "" : ", ").append(src).append('.').append(id.q(columns.get(i)));
        }
        sb.append(')');
        if (dialect == Dialect.SQLSERVER) {
            sb.append(';');
        }
        return sb.toString();
    }

    /**
     * Checks the precondition {@link #sql} cannot enforce itself.
     *
     * <p>A row that does not carry every key column cannot be matched against
     * the table, so the upsert would degenerate into a plain insert - silently,
     * on some products.
     *
     * @param columns the columns being written
     * @param keys    the conflict key columns
     * @return whether every key column is among {@code columns}, compared
     *         without regard to case
     */
    public static boolean covers(List<String> columns, List<String> keys) {
        for (String k : keys) {
            if (!contains(columns, k)) {
                return false;
            }
        }
        return true;
    }
}
