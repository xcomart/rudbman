package comart.rudbman.bridge.meta;

import java.sql.Connection;
import java.sql.DatabaseMetaData;
import java.util.Collections;
import java.util.Locale;
import java.util.Map;
import java.util.WeakHashMap;

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
 *
 * <p>A session opened with a dialect profile ({@code docs/dialect-profiles.md})
 * answers with the profile's dialect instead: {@link #attach} ties it to the
 * session's connection, and {@link #of} finds it there before it looks at the
 * product name. That is why this is a class of constants rather than an
 * enum - a profile is a dialect too, one per session that names it.
 */
public final class Dialect {

    /** H2, product name {@code "H2"}. */
    public static final Dialect H2 = new Dialect("H2", new H2Vendor());
    /** PostgreSQL and wire-compatible forks that keep the product name. */
    public static final Dialect POSTGRESQL = new Dialect("POSTGRESQL", new PostgresVendor());
    /** MySQL. */
    public static final Dialect MYSQL = new Dialect("MYSQL", new MySqlVendor());
    /** MariaDB. Reported separately from MySQL because only MariaDB has sequences. */
    public static final Dialect MARIADB = new Dialect("MARIADB", new MariaDbVendor());
    /** Oracle Database. */
    public static final Dialect ORACLE = new Dialect("ORACLE", new OracleVendor());
    /** Microsoft SQL Server. */
    public static final Dialect SQLSERVER = new Dialect("SQLSERVER", new SqlServerVendor());
    /** SQLite. */
    public static final Dialect SQLITE = new Dialect("SQLITE", new SqliteVendor());
    /** IBM Db2. */
    public static final Dialect DB2 = new Dialect("DB2", new Db2Vendor());
    /** Anything unrecognized. Only portable, standard-JDBC paths may be used. */
    public static final Dialect OTHER = new Dialect("OTHER", new PortableVendor());

    /** The products the bridge knows, in declaration order. */
    private static final Dialect[] BUILT_IN = {
        H2, POSTGRESQL, MYSQL, MARIADB, ORACLE, SQLSERVER, SQLITE, DB2, OTHER,
    };

    /**
     * The order products are tried in. MariaDB must be tested before MySQL:
     * the MariaDB server reports "MariaDB" but several MySQL-compatible builds
     * put both words in the string, and the sequence support hangs on which
     * one is really there.
     */
    private static final Dialect[] DETECTION = {
        MARIADB, MYSQL, POSTGRESQL, ORACLE, SQLSERVER, SQLITE, H2, DB2,
    };

    /**
     * The profile dialect of each connection opened with one. Weak, so that a
     * connection the bridge lost track of does not keep its profile alive;
     * {@link #detach} is what a closing session calls.
     */
    private static final Map<Connection, Dialect> ATTACHED =
            Collections.synchronizedMap(new WeakHashMap<>());

    private final String name;
    private final Vendor vendor;

    private Dialect(String name, Vendor vendor) {
        this.name = name;
        this.vendor = vendor;
    }

    /**
     * A dialect answered by a profile's vendor.
     *
     * @param name   the profile's id, for messages
     * @param vendor its vendor
     * @return the dialect, to {@link #attach} to a session's connection
     */
    static Dialect profile(String name, Vendor vendor) {
        return new Dialect(name, vendor);
    }

    /** @return the built-in products, {@link #OTHER} last */
    public static Dialect[] values() {
        return BUILT_IN.clone();
    }

    /** @return the constant's name, or a profile's id */
    public String name() {
        return name;
    }

    @Override
    public String toString() {
        return name;
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
     * @return the profile dialect attached to the connection, else the
     *         matching product, else {@link #OTHER}
     */
    public static Dialect of(DatabaseMetaData dbm) {
        if (!ATTACHED.isEmpty()) {
            try {
                Dialect own = ATTACHED.get(dbm.getConnection());
                if (own != null) {
                    return own;
                }
            } catch (Exception | AbstractMethodError e) {
                // No connection to ask after: the product name still answers.
            }
        }
        String productName;
        try {
            productName = dbm.getDatabaseProductName();
        } catch (Exception | AbstractMethodError e) {
            return OTHER;
        }
        return byProductName(productName);
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

    /**
     * The built-in product a profile's base names, by the ids
     * {@code rudbman-sql} gives its dialects.
     *
     * @param id a built-in dialect id
     * @return the product, or {@code null} for an id that names none
     */
    public static Dialect byId(String id) {
        if (id == null) {
            return null;
        }
        switch (id.toLowerCase(Locale.ROOT)) {
            case "generic":
                return OTHER;
            case "h2":
                return H2;
            case "postgres":
                return POSTGRESQL;
            case "mysql":
                return MYSQL;
            case "mariadb":
                return MARIADB;
            case "oracle":
                return ORACLE;
            case "mssql":
                return SQLSERVER;
            case "sqlite":
                return SQLITE;
            case "db2":
                return DB2;
            default:
                return null;
        }
    }

    /**
     * Makes {@code dialect} the one {@link #of} answers for {@code conn}.
     *
     * <p>The connection the driver hands back from its own metadata is
     * registered too, where it is another object: {@link #of} asks after that
     * one.
     *
     * @param conn    a session's connection
     * @param dialect the profile's dialect
     */
    public static void attach(Connection conn, Dialect dialect) {
        ATTACHED.put(conn, dialect);
        try {
            Connection reported = conn.getMetaData().getConnection();
            if (reported != null && reported != conn) {
                ATTACHED.put(reported, dialect);
            }
        } catch (Exception | AbstractMethodError e) {
            // The connection itself is registered, which is what a driver that
            // cannot answer this hands back anyway.
        }
    }

    /**
     * Forgets the profile dialect of a closing session's connection.
     *
     * @param conn the connection
     */
    public static void detach(Connection conn) {
        synchronized (ATTACHED) {
            Dialect own = ATTACHED.remove(conn);
            if (own != null) {
                // The driver's own handle on the connection, if it was another.
                ATTACHED.values().removeIf(d -> d == own);
            }
        }
    }
}
