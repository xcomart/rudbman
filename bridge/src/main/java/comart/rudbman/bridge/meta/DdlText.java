package comart.rudbman.bridge.meta;

import java.sql.CallableStatement;
import java.sql.Connection;
import java.sql.PreparedStatement;
import java.sql.ResultSet;
import java.sql.SQLException;
import java.util.ArrayList;
import java.util.List;
import java.util.Locale;
import java.util.regex.Matcher;
import java.util.regex.Pattern;

/**
 * Text helpers and vendor DDL sources shared by {@link Ddl}, {@link RoutineDdl}
 * and {@link ObjectDdl}.
 *
 * <p>Every source here answers {@code null} rather than throwing when it has
 * nothing usable: the callers all have a next layer to try, and a missing
 * privilege is not worth failing a request over.
 */
final class DdlText {

    /**
     * H2's {@code SCRIPT} restricted to one schema still writes every object in
     * it; an object is the row creating it.
     * Source: H2 2.x SQL grammar, "SCRIPT".
     */
    private static final String H2_SCRIPT = "SCRIPT NODATA NOPASSWORDS NOSETTINGS SCHEMA ";

    /**
     * {@code DBMS_METADATA} writes storage, tablespace and segment clauses by
     * default, which are the DBA's business and not what a reader looks for.
     * The transform parameters last for the session, and only change what
     * {@code DBMS_METADATA} itself writes.
     * Source: Oracle Database PL/SQL Packages and Types Reference,
     * "DBMS_METADATA.SET_TRANSFORM_PARAM".
     */
    private static final String ORACLE_TRANSFORMS = "BEGIN"
            + " DBMS_METADATA.SET_TRANSFORM_PARAM(DBMS_METADATA.SESSION_TRANSFORM,"
            + " 'SEGMENT_ATTRIBUTES', FALSE);"
            + " DBMS_METADATA.SET_TRANSFORM_PARAM(DBMS_METADATA.SESSION_TRANSFORM,"
            + " 'SQLTERMINATOR', TRUE);"
            + " END;";

    /** A MySQL account or host part: quoted three ways, or bare. */
    private static final String MYSQL_NAME = "(?:`(?:[^`]|``)*`|'(?:[^']|'')*'|\"(?:[^\"]|\"\")*\""
            + "|[\\w.$%-]+(?:\\(\\))?)";

    /**
     * What MySQL and MariaDB write between {@code CREATE [OR REPLACE]} and the
     * object's keyword: {@code ALGORITHM=...}, {@code DEFINER=...} and a view's
     * {@code SQL SECURITY ...}.
     */
    private static final Pattern MYSQL_PREAMBLE = Pattern.compile(
            "^(CREATE\\s+(?:OR\\s+REPLACE\\s+)?)"
                    + "(?:ALGORITHM\\s*=\\s*\\w+\\s+)?"
                    + "(?:DEFINER\\s*=\\s*" + MYSQL_NAME + "(?:\\s*@\\s*" + MYSQL_NAME + ")?\\s+)?"
                    + "(?:SQL\\s+SECURITY\\s+(?:DEFINER|INVOKER)\\s+)?"
                    + "(?=(?:VIEW|PROCEDURE|FUNCTION|TRIGGER|EVENT|AGGREGATE)\\b)",
            Pattern.CASE_INSENSITIVE);

    private DdlText() {
    }

    /**
     * MySQL's and MariaDB's {@code SHOW CREATE} text without its
     * {@code ALGORITHM}, {@code DEFINER} and view {@code SQL SECURITY} clauses.
     *
     * <p>They name the account that created the object and the plan the server
     * picked, not the object itself, and a statement carrying another account's
     * {@code DEFINER} is one most accounts may not run. What the object is - its
     * columns, its query, its body - is untouched, and so is a routine's own
     * {@code SQL SECURITY} characteristic, which comes after its parameters.
     */
    static String withoutDefiner(String text) {
        if (text == null) {
            return null;
        }
        Matcher m = MYSQL_PREAMBLE.matcher(text);
        return m.find() ? m.group(1) + text.substring(m.end()) : text;
    }

    /** The text with its surrounding blanks gone, or {@code null} when nothing is left. */
    static String stripped(String text) {
        if (text == null) {
            return null;
        }
        String out = text.strip();
        return out.isEmpty() ? null : out;
    }

    /** {@link #stripped}, ending in a {@code ;} as a table's DDL does. */
    static String terminated(String text) {
        String out = stripped(text);
        if (out == null) {
            return null;
        }
        return out.endsWith(";") ? out : out + ";";
    }

    /** Statements one after another, an empty line apart, or {@code null} for none. */
    static String joined(List<String> texts) {
        return texts.isEmpty() ? null : String.join("\n\n", texts);
    }

    /**
     * The rows of H2's script of {@code schema} that create the object
     * {@code what "SCHEMA"."NAME"} - {@code ALIAS}, {@code VIEW},
     * {@code SEQUENCE}, {@code TRIGGER}.
     *
     * <p>H2 writes fully quoted, fully qualified names in a script, so the row
     * that creates an object is the one with that reference in it, followed by
     * a space or a bracket so that {@code F} does not also pick {@code F_2}.
     */
    static String h2Script(Connection conn, Ident id, String schema, String what, String name) {
        if (schema == null || schema.isEmpty()) {
            return null;
        }
        String ref = what + " \"" + schema + "\".\"" + name + "\"";
        return Attempt.<String>run(conn, "SCRIPT SCHEMA", st -> {
            List<String> texts = new ArrayList<>();
            try (ResultSet rs = st.executeQuery(H2_SCRIPT + id.q(schema))) {
                while (rs.next()) {
                    String stmt = stripped(rs.getString(1));
                    if (stmt == null || !stmt.toUpperCase(Locale.ROOT).startsWith("CREATE ")) {
                        continue;
                    }
                    int at = stmt.indexOf(ref);
                    if (at >= 0 && at + ref.length() < stmt.length()) {
                        char next = stmt.charAt(at + ref.length());
                        if (next == ' ' || next == '(' || next == '\n') {
                            texts.add(stmt);
                        }
                    }
                }
            }
            return joined(texts);
        }, null);
    }

    /**
     * {@code DBMS_METADATA.GET_DDL} of one object, without its storage
     * clauses, or {@code null} when the account may not read it.
     *
     * @param type the metadata object type: {@code TABLE}, {@code VIEW},
     *             {@code MATERIALIZED_VIEW}, {@code SEQUENCE}, {@code INDEX}
     */
    static String oracleMetadata(Connection conn, String type, String schema, String name) {
        return Attempt.<String>on(conn, "DBMS_METADATA.GET_DDL " + type, c -> {
            try (CallableStatement cs = c.prepareCall(ORACLE_TRANSFORMS)) {
                cs.execute();
            }
            return oracleCall(c, "SELECT DBMS_METADATA.GET_DDL(?, ?, " + owner(schema)
                    + ") FROM DUAL", type, name, schema);
        }, null);
    }

    /**
     * {@code DBMS_METADATA.GET_DEPENDENT_DDL} - an object's comments, its
     * indexes - or {@code null}, which is also how Oracle's "there are none"
     * (ORA-31608) arrives.
     */
    static String oracleDependent(Connection conn, String type, String schema, String name) {
        return Attempt.<String>on(conn, "DBMS_METADATA.GET_DEPENDENT_DDL " + type,
                c -> oracleCall(c, "SELECT DBMS_METADATA.GET_DEPENDENT_DDL(?, ?, " + owner(schema)
                        + ") FROM DUAL", type, name, schema), null);
    }

    /** The owner argument: the schema asked for, or the session's own. */
    static String owner(String schema) {
        return schema == null || schema.isEmpty()
                ? "SYS_CONTEXT('USERENV', 'CURRENT_SCHEMA')" : "?";
    }

    private static String oracleCall(Connection c, String sql, String type, String name,
                                     String schema) throws SQLException {
        try (PreparedStatement ps = c.prepareStatement(sql)) {
            ps.setString(1, type);
            ps.setString(2, name);
            if (schema != null && !schema.isEmpty()) {
                ps.setString(3, schema);
            }
            try (ResultSet rs = ps.executeQuery()) {
                return rs.next() ? stripped(rs.getString(1)) : null;
            }
        }
    }

    /**
     * The text of {@code ALL_SOURCE} for one object, behind
     * {@code CREATE OR REPLACE}: the table holds what followed those words,
     * one row per line with the line break kept.
     */
    static String oracleSource(Connection conn, String schema, String name, String type) {
        return Attempt.<String>on(conn, "ALL_SOURCE " + type, c -> {
            String sql = "SELECT TEXT FROM ALL_SOURCE WHERE OWNER = " + owner(schema)
                    + " AND NAME = ? AND TYPE = ? ORDER BY LINE";
            try (PreparedStatement ps = c.prepareStatement(sql)) {
                int i = 1;
                if (schema != null && !schema.isEmpty()) {
                    ps.setString(i++, schema);
                }
                ps.setString(i++, name);
                ps.setString(i, type);
                StringBuilder sb = new StringBuilder();
                try (ResultSet rs = ps.executeQuery()) {
                    while (rs.next()) {
                        String line = rs.getString(1);
                        if (line != null) {
                            sb.append(line);
                        }
                    }
                }
                String text = stripped(sb.toString());
                return text == null ? null : "CREATE OR REPLACE " + text;
            }
        }, null);
    }

    /** {@code OBJECT_DEFINITION}: the batch a SQL Server module was created with. */
    static String sqlServerDefinition(Connection conn, String qualified) {
        return Attempt.<String>on(conn, "OBJECT_DEFINITION", c -> {
            try (PreparedStatement ps = c.prepareStatement(
                    "SELECT OBJECT_DEFINITION(OBJECT_ID(?))")) {
                ps.setString(1, qualified);
                try (ResultSet rs = ps.executeQuery()) {
                    return rs.next() ? stripped(rs.getString(1)) : null;
                }
            }
        }, null);
    }

    /** {@code sqlite_master.sql}: SQLite keeps every statement as it was typed. */
    static String sqlite(Connection conn, String type, String name) {
        return Attempt.<String>on(conn, "sqlite_master", c -> {
            try (PreparedStatement ps = c.prepareStatement(
                    "SELECT sql FROM sqlite_master WHERE type = ? AND name = ?")) {
                ps.setString(1, type);
                ps.setString(2, name);
                try (ResultSet rs = ps.executeQuery()) {
                    return rs.next() ? terminated(rs.getString(1)) : null;
                }
            }
        }, null);
    }
}
