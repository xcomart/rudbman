package comart.rudbman.bridge.meta;

import org.junit.jupiter.api.Test;

import static org.junit.jupiter.api.Assertions.assertEquals;

/** {@link DdlText#withoutDefiner}, over what MySQL and MariaDB quote back. */
class DdlTextTest {

    @Test
    void aViewLosesItsAlgorithmDefinerAndSecurity() {
        assertEquals("CREATE VIEW `v` AS select 1 AS `a`",
                DdlText.withoutDefiner("CREATE ALGORITHM=UNDEFINED DEFINER=`root`@`%` "
                        + "SQL SECURITY DEFINER VIEW `v` AS select 1 AS `a`"));
        assertEquals("CREATE OR REPLACE VIEW v AS select 1",
                DdlText.withoutDefiner("CREATE OR REPLACE ALGORITHM = MERGE "
                        + "DEFINER = 'app'@'10.0.%' SQL SECURITY INVOKER VIEW v AS select 1"));
    }

    @Test
    void aRoutineAndATriggerLoseTheirDefinerOnly() {
        assertEquals("CREATE PROCEDURE `p`(IN a INT)\n    SQL SECURITY INVOKER\nBEGIN END",
                DdlText.withoutDefiner("CREATE DEFINER=`root`@`localhost` PROCEDURE `p`(IN a INT)"
                        + "\n    SQL SECURITY INVOKER\nBEGIN END"));
        assertEquals("CREATE trigger t before insert on x for each row set new.a = 1",
                DdlText.withoutDefiner("CREATE DEFINER=CURRENT_USER trigger t before insert "
                        + "on x for each row set new.a = 1"));
        assertEquals("CREATE AGGREGATE FUNCTION f() RETURNS INT BEGIN END",
                DdlText.withoutDefiner("CREATE DEFINER=`a``b`@`%` AGGREGATE FUNCTION f() "
                        + "RETURNS INT BEGIN END"));
    }

    @Test
    void textWithoutAPreambleIsLeftAlone() {
        String table = "CREATE TABLE `t` (`definer` int)";
        assertEquals(table, DdlText.withoutDefiner(table));
        String view = "CREATE VIEW v AS select 'DEFINER=x' AS d";
        assertEquals(view, DdlText.withoutDefiner(view));
    }
}
