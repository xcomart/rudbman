package comart.rudbman.bridge.meta;

import java.sql.DatabaseMetaData;
import java.util.Locale;

/**
 * The database product behind a session, to the resolution the bridge actually
 * needs.
 *
 * <p>Two of the {@code DESCRIBE} kinds cannot be answered through JDBC alone.
 * Sequences have no {@link DatabaseMetaData} accessor at all, and a table's own
 * {@code CREATE} text is something only the server can quote verbatim. Both
 * therefore need a vendor catalogue query, and a vendor catalogue query needs to
 * know the vendor.
 *
 * <p>The discriminator is {@link DatabaseMetaData#getDatabaseProductName()},
 * the same string {@code SESSION_INFO} reports, matched case-insensitively on a
 * substring. Product names are marketing text and do drift between versions, so
 * an unrecognised name is not an error: it maps to {@link #OTHER}, which means
 * "portable paths only". Every caller must stay useful under {@link #OTHER}.
 */
public enum Dialect {

    /** H2, product name {@code "H2"}. */
    H2,
    /** PostgreSQL and wire-compatible forks that keep the product name. */
    POSTGRESQL,
    /** MySQL. */
    MYSQL,
    /** MariaDB. Reported separately from MySQL because only MariaDB has sequences. */
    MARIADB,
    /** Oracle Database. */
    ORACLE,
    /** Microsoft SQL Server. */
    SQLSERVER,
    /** SQLite. */
    SQLITE,
    /** IBM Db2. */
    DB2,
    /** Anything unrecognised. Only portable, standard-JDBC paths may be used. */
    OTHER;

    /**
     * Identifies the product behind a connection.
     *
     * <p>Never throws: a driver that fails to answer what it is still deserves
     * the portable paths rather than a failed request.
     *
     * @param dbm the connection metadata
     * @return the matching dialect, or {@link #OTHER}
     */
    public static Dialect of(DatabaseMetaData dbm) {
        String name;
        try {
            name = dbm.getDatabaseProductName();
        } catch (Exception | AbstractMethodError e) {
            return OTHER;
        }
        return byProductName(name);
    }

    /**
     * Identifies the product from its name.
     *
     * @param productName the value of {@code getDatabaseProductName()}, may be
     *                    {@code null}
     * @return the matching dialect, or {@link #OTHER}
     */
    public static Dialect byProductName(String productName) {
        if (productName == null) {
            return OTHER;
        }
        String n = productName.toLowerCase(Locale.ROOT);
        // MariaDB must be tested before MySQL: the MariaDB server reports
        // "MariaDB" but several MySQL-compatible builds put both words in the
        // string, and the sequence support hangs on which one is really there.
        if (n.contains("mariadb")) {
            return MARIADB;
        }
        if (n.contains("mysql")) {
            return MYSQL;
        }
        if (n.contains("postgresql") || n.contains("postgres")) {
            return POSTGRESQL;
        }
        if (n.contains("oracle")) {
            return ORACLE;
        }
        if (n.contains("microsoft sql server") || n.contains("sql server")) {
            return SQLSERVER;
        }
        if (n.contains("sqlite")) {
            return SQLITE;
        }
        if (n.startsWith("h2")) {
            return H2;
        }
        if (n.contains("db2")) {
            return DB2;
        }
        return OTHER;
    }

    /** @return whether this product is MySQL or MariaDB. */
    public boolean isMySqlFamily() {
        return this == MYSQL || this == MARIADB;
    }
}
