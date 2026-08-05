package comart.rudbman.bridge.meta;

import org.junit.jupiter.api.Test;

import static org.junit.jupiter.api.Assertions.assertEquals;

/**
 * Product-name matching, which is the one part of the vendor-specific paths a
 * test against H2 alone cannot otherwise reach.
 */
class DialectTest {

    @Test
    void recognisesTheProductsWithVendorPaths() {
        assertEquals(Dialect.H2, Dialect.byProductName("H2"));
        assertEquals(Dialect.POSTGRESQL, Dialect.byProductName("PostgreSQL"));
        assertEquals(Dialect.ORACLE, Dialect.byProductName("Oracle"));
        assertEquals(Dialect.MYSQL, Dialect.byProductName("MySQL"));
        assertEquals(Dialect.SQLSERVER, Dialect.byProductName("Microsoft SQL Server"));
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
