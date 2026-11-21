package comart.rudbman.bridge.codec;

import org.junit.jupiter.api.Test;

import java.sql.Connection;
import java.sql.DriverManager;
import java.sql.ResultSet;
import java.sql.SQLException;
import java.sql.Statement;
import java.sql.Types;

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertFalse;

/** {@link DriverText}: a fraction of a second the driver wrote wrong, mended from its value. */
class DriverTextTest {

    private static final String DAY = "2026-11-21 02:21:39.";

    @Test
    void aFractionMissingItsLeadingZerosIsWrittenFromTheValue() {
        // What MariaDB Connector/J writes, and what the column holds.
        assertEquals(DAY + "064000", DriverText.mended(DAY + "64000", 64_000_000));
        assertEquals(DAY + "006000", DriverText.mended(DAY + "6000", 6_000_000));
        assertEquals(DAY + "000060", DriverText.mended(DAY + "00060", 60_000));
        assertEquals(DAY + "000000123", DriverText.mended(DAY + "123", 123));
    }

    @Test
    void aFractionTheDriverGotRightIsLeftAsItIs() {
        assertEquals(DAY + "123000", DriverText.mended(DAY + "123000", 123_000_000));
        assertEquals(DAY + "000064", DriverText.mended(DAY + "000064", 64_000));
        assertEquals(DAY + "0", DriverText.mended(DAY + "0", 0));
        assertEquals(DAY + "5", DriverText.mended(DAY + "5", 500_000_000));
        assertEquals("2026-11-21 02:21:39", DriverText.mended("2026-11-21 02:21:39", 0));
    }

    @Test
    void aTimesFractionIsMendedTheSameWay() {
        // TIME(3) over the binary protocol, and beyond a day or below zero.
        assertEquals("10:00:00.005000", DriverText.mended("10:00:00.5000", 5_000_000));
        assertEquals("-838:59:59.005000", DriverText.mended("-838:59:59.5000", 5_000_000));
        assertEquals("125:00:00.000060", DriverText.mended("125:00:00.00060", 60_000));
        // The server's own text, over the text protocol, is right.
        assertEquals("10:00:00.005", DriverText.mended("10:00:00.005", 5_000_000));
        assertEquals("-00:00:01.500000", DriverText.mended("-00:00:01.500000", 500_000_000));
    }

    @Test
    void whatFollowsTheFractionStays() {
        assertEquals(DAY + "064000 +09:00", DriverText.mended(DAY + "64000 +09:00", 64_000_000));
    }

    @Test
    void onlyTheMariaDbDriversTimestampsAreMended() throws SQLException {
        try (Connection c = DriverManager.getConnection("jdbc:h2:mem:drivertext");
             Statement st = c.createStatement();
             ResultSet rs = st.executeQuery("SELECT TIMESTAMP '2026-11-21 02:21:39.064'")) {
            rs.next();
            assertFalse(DriverText.mends(rs, Types.TIMESTAMP), "H2's text is its own");
            assertEquals(rs.getString(1), DriverText.of(rs, 1, Types.TIMESTAMP));
        }
    }
}
