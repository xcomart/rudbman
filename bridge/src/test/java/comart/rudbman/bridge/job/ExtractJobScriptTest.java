package comart.rudbman.bridge.job;

import org.junit.jupiter.api.Test;

import static org.junit.jupiter.api.Assertions.assertFalse;
import static org.junit.jupiter.api.Assertions.assertTrue;

/**
 * Which extractions are SQL scripts, and so carry the product's session
 * settings - {@code SET FOREIGN_KEY_CHECKS} on MySQL and MariaDB,
 * {@code SET QUOTED_IDENTIFIER} on SQL Server - around them.
 *
 * <p>The H2 the job tests run against has no such settings, so the rule is
 * held here on its own.
 */
class ExtractJobScriptTest {

    @Test
    void definitionsOrInsertsMakeAScript() {
        assertTrue(ExtractJob.isScript(true, false, "insert"));
        assertTrue(ExtractJob.isScript(false, true, "insert"));
        assertTrue(ExtractJob.isScript(true, true, "insert"));
        // The DDL half is SQL whatever follows it.
        assertTrue(ExtractJob.isScript(true, true, "csv"));
        assertTrue(ExtractJob.isScript(true, true, "template"));
    }

    @Test
    void rowsAloneInCsvOrATemplateAreNotAScript() {
        assertFalse(ExtractJob.isScript(false, true, "csv"));
        assertFalse(ExtractJob.isScript(false, true, "template"));
    }
}
