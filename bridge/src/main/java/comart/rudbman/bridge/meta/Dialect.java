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
 * therefore need a vendor catalog query, and a vendor catalog query needs to
 * know the vendor.
 *
 * <p>The discriminator is {@link DatabaseMetaData#getDatabaseProductName()},
 * the same string {@code SESSION_INFO} reports, matched case-insensitively on a
 * substring. Product names are marketing text and do drift between versions, so
 * an unrecognized name is not an error: it maps to {@link #OTHER}, which means
 * "portable paths only". Every caller must stay useful under {@link #OTHER}.
 */
public enum Dialect {

    /** H2, product name {@code "H2"}. */
    H2(new H2Vendor()),
    /** PostgreSQL and wire-compatible forks that keep the product name. */
    POSTGRESQL(new PostgresVendor()),
    /** MySQL. */
    MYSQL(new MySqlVendor()),
    /** MariaDB. Reported separately from MySQL because only MariaDB has sequences. */
    MARIADB(new MariaDbVendor()),
    /** Oracle Database. */
    ORACLE(new OracleVendor()),
    /** Microsoft SQL Server. */
    SQLSERVER(new SqlServerVendor()),
    /** SQLite. */
    SQLITE(new SqliteVendor()),
    /** IBM Db2. */
    DB2(new Db2Vendor()),
    /** Anything unrecognized. Only portable, standard-JDBC paths may be used. */
    OTHER(new PortableVendor());

    /**
     * The order products are tried in. MariaDB must be tested before MySQL:
     * the MariaDB server reports "MariaDB" but several MySQL-compatible builds
     * put both words in the string, and the sequence support hangs on which
     * one is really there.
     */
    private static final Dialect[] DETECTION = {
        MARIADB, MYSQL, POSTGRESQL, ORACLE, SQLSERVER, SQLITE, H2, DB2,
    };

    private final Vendor vendor;

    Dialect(Vendor vendor) {
        this.vendor = vendor;
    }

    /** @return everything the bridge does differently for this product */
    public Vendor vendor() {
        return vendor;
    }

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
        for (Dialect d : DETECTION) {
            if (d.vendor.names(n)) {
                return d;
            }
        }
        return OTHER;
    }
}
