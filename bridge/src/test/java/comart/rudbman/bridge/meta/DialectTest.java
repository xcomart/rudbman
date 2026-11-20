package comart.rudbman.bridge.meta;

import org.junit.jupiter.api.Test;

import java.util.Map;

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertNotNull;

/**
 * Product-name matching, which is the one part of the vendor-specific paths a
 * test against H2 alone cannot otherwise reach.
 */
class DialectTest {

    /**
     * What each product's driver reports as its name, one for every product
     * the bridge has a vendor for. A product added to {@link Dialect} without
     * one here fails {@link #recognizesEveryProductItHasAVendorFor}.
     */
    private static final Map<Dialect, String> REPORTED = Map.of(
            Dialect.H2, "H2",
            Dialect.POSTGRESQL, "PostgreSQL",
            Dialect.MYSQL, "MySQL",
            Dialect.MARIADB, "MariaDB",
            Dialect.ORACLE, "Oracle",
            Dialect.SQLSERVER, "Microsoft SQL Server",
            Dialect.SQLITE, "SQLite",
            Dialect.DB2, "DB2/LINUXX8664");

    @Test
    void recognizesEveryProductItHasAVendorFor() {
        for (Dialect d : Dialect.values()) {
            if (d == Dialect.OTHER) {
                continue;
            }
            String name = REPORTED.get(d);
            assertNotNull(name, d + " has no reported product name to test");
            assertEquals(d, Dialect.byProductName(name), name);
        }
    }

    @Test
    void mariaDbIsNotMysql() {
        // Only MariaDB has sequences, and its product string mentions MySQL in
        // several builds, so the MariaDB test has to come first.
        assertEquals(Dialect.MARIADB, Dialect.byProductName("MariaDB"));
        assertEquals(Dialect.MARIADB, Dialect.byProductName("MySQL (MariaDB Server 10.11.6)"));
    }

    @Test
    void anythingElseGetsThePortablePathsOnly() {
        assertEquals(Dialect.OTHER, Dialect.byProductName("Snowflake"));
        assertEquals(Dialect.OTHER, Dialect.byProductName(null));
        assertEquals(Dialect.OTHER, Dialect.byProductName(""));
    }
}
