package comart.rudbman.bridge.job;

import comart.rudbman.bridge.meta.Dialect;
import org.junit.jupiter.api.Test;

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertNull;

/**
 * How a routine, a trigger and a view are written for each product's own
 * command-line client - checked against the real clients (mysql, sqlplus,
 * sqlcmd, psql) once, and pinned here.
 */
class ScriptsTest {

    private static final String MYSQL_BODY = "CREATE PROCEDURE `p`()\nbegin select 1; end;";

    @Test
    void mysqlRoutinesGoBetweenDelimiters() {
        assertEquals("DELIMITER ;;\nCREATE PROCEDURE `p`()\nbegin select 1; end;;\nDELIMITER ;",
                Scripts.module(Dialect.MYSQL, "procedure", MYSQL_BODY));
        assertEquals("CREATE VIEW `v` AS select 1;",
                Scripts.module(Dialect.MARIADB, "view", "CREATE VIEW `v` AS select 1"));
    }

    @Test
    void oraclePlsqlEndsInASlash() {
        assertEquals("CREATE OR REPLACE procedure p is begin null; end;\n/",
                Scripts.module(Dialect.ORACLE, "procedure",
                        "CREATE OR REPLACE procedure p is begin null; end;"));
        // DBMS_METADATA has already written the slashes of a package.
        String pkg = "CREATE OR REPLACE PACKAGE \"S\".\"P\" is end;\n/";
        assertEquals(pkg, Scripts.module(Dialect.ORACLE, "package", pkg));
        assertEquals("CREATE VIEW v AS select 1 from dual;",
                Scripts.module(Dialect.ORACLE, "view", "CREATE VIEW v AS select 1 from dual"));
    }

    @Test
    void sqlServerModulesStandInBatchesOfTheirOwn() {
        assertEquals("GO\ncreate view dbo.v as select 1 x\nGO",
                Scripts.module(Dialect.SQLSERVER, "view", "create view dbo.v as select 1 x"));
        assertEquals("CREATE SEQUENCE dbo.s;",
                Scripts.module(Dialect.SQLSERVER, "sequence", "CREATE SEQUENCE dbo.s"));
    }

    @Test
    void db2RoutinesChangeTheTerminator() {
        assertEquals("--#SET TERMINATOR @\nCREATE PROCEDURE p BEGIN END\n@\n--#SET TERMINATOR ;",
                Scripts.module(Dialect.DB2, "procedure", "CREATE PROCEDURE p BEGIN END;"));
    }

    @Test
    void dollarQuotedBodiesNeedNothing() {
        String fn = "CREATE OR REPLACE FUNCTION f() RETURNS int LANGUAGE sql"
                + " AS $function$ select 1; $function$;";
        assertEquals(fn, Scripts.module(Dialect.POSTGRESQL, "function", fn));
    }

    @Test
    void foreignKeysComeOffWhereTablesWouldOtherwiseBlockEachOther() {
        assertEquals("ALTER TABLE IF EXISTS s.t DROP CONSTRAINT IF EXISTS fk;",
                Scripts.dropForeignKey(Dialect.POSTGRESQL, "s.t", "fk"));
        assertEquals("IF OBJECT_ID('db.s.t', 'U') IS NOT NULL"
                        + " ALTER TABLE db.s.t DROP CONSTRAINT IF EXISTS fk;",
                Scripts.dropForeignKey(Dialect.SQLSERVER, "db.s.t", "fk"));
        // MySQL runs with its checks off; Oracle drops with CASCADE CONSTRAINTS.
        assertNull(Scripts.dropForeignKey(Dialect.MYSQL, "t", "fk"));
        assertNull(Scripts.dropForeignKey(Dialect.ORACLE, "S.T", "FK"));
        assertEquals("DROP TABLE S.T CASCADE CONSTRAINTS;",
                Scripts.dropStatement(Dialect.ORACLE, "S.T"));
    }
}
