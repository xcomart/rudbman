package comart.rudbman.bridge.meta;

import java.sql.Connection;
import java.sql.PreparedStatement;
import java.sql.ResultSet;
import java.util.ArrayList;
import java.util.HashSet;
import java.util.List;
import java.util.Set;

/**
 * What a PostgreSQL schema holds besides its tables, views, routines,
 * sequences and triggers, as far as a backup of it needs: the extensions
 * installed into it, and the enum and domain types its columns are declared
 * with.
 *
 * <p>Without these a script does not replay - a column of an enum type is an
 * error until the type exists - and an extension's own functions, tables and
 * types are not the schema's to write: they come back with
 * {@code CREATE EXTENSION}, as pg_dump has them, and writing them as plain
 * objects would collide with the extension the moment it is installed.
 *
 * <p>Every query is fenced by {@link Attempt}: an account that may not read a
 * catalog gets a script without that part, not a failed backup.
 */
public final class PgSchema {

    /**
     * The objects of {@code schema} that belong to an extension, by name:
     * relations (tables, views, sequences), routines and types.
     */
    private static final String MEMBERS_SQL = "SELECT c.relname"
            + " FROM pg_catalog.pg_depend d"
            + " JOIN pg_catalog.pg_class c ON d.classid = 'pg_catalog.pg_class'::regclass"
            + " AND d.objid = c.oid"
            + " JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace"
            + " WHERE d.deptype = 'e' AND n.nspname = ?"
            + " UNION SELECT p.proname"
            + " FROM pg_catalog.pg_depend d"
            + " JOIN pg_catalog.pg_proc p ON d.classid = 'pg_catalog.pg_proc'::regclass"
            + " AND d.objid = p.oid"
            + " JOIN pg_catalog.pg_namespace n ON n.oid = p.pronamespace"
            + " WHERE d.deptype = 'e' AND n.nspname = ?"
            + " UNION SELECT t.typname"
            + " FROM pg_catalog.pg_depend d"
            + " JOIN pg_catalog.pg_type t ON d.classid = 'pg_catalog.pg_type'::regclass"
            + " AND d.objid = t.oid"
            + " JOIN pg_catalog.pg_namespace n ON n.oid = t.typnamespace"
            + " WHERE d.deptype = 'e' AND n.nspname = ?";

    /** An extension's own type, which its {@code CREATE EXTENSION} brings back. */
    private static final String NOT_A_MEMBER = " AND NOT EXISTS (SELECT 1 FROM pg_catalog.pg_depend d"
            + " WHERE d.classid = 'pg_catalog.pg_type'::regclass AND d.objid = t.oid"
            + " AND d.deptype = 'e')";

    private static final String EXTENSIONS_SQL = "SELECT e.extname"
            + " FROM pg_catalog.pg_extension e"
            + " JOIN pg_catalog.pg_namespace n ON n.oid = e.extnamespace"
            + " WHERE n.nspname = ? ORDER BY e.extname";

    private static final String ENUMS_SQL = "SELECT t.typname,"
            + " string_agg(quote_literal(e.enumlabel), ', ' ORDER BY e.enumsortorder)"
            + " FROM pg_catalog.pg_type t"
            + " JOIN pg_catalog.pg_enum e ON e.enumtypid = t.oid"
            + " JOIN pg_catalog.pg_namespace n ON n.oid = t.typnamespace"
            + " WHERE n.nspname = ?" + NOT_A_MEMBER
            + " GROUP BY t.oid, t.typname ORDER BY t.typname";

    private static final String DOMAINS_SQL = "SELECT t.typname,"
            + " pg_catalog.format_type(t.typbasetype, t.typtypmod), t.typnotnull, t.typdefault,"
            + " (SELECT string_agg('CONSTRAINT ' || quote_ident(k.conname) || ' '"
            + " || pg_catalog.pg_get_constraintdef(k.oid, true), ' ' ORDER BY k.conname)"
            + " FROM pg_catalog.pg_constraint k WHERE k.contypid = t.oid)"
            + " FROM pg_catalog.pg_type t"
            + " JOIN pg_catalog.pg_namespace n ON n.oid = t.typnamespace"
            + " WHERE t.typtype = 'd' AND n.nspname = ?" + NOT_A_MEMBER
            + " ORDER BY t.typname";

    /**
     * The schema's sequences a column owns - a serial's - with the column:
     * what {@code OWNED BY} records, and {@code pg_get_serial_sequence} and
     * dropping the table go by.
     */
    private static final String OWNED_SQL = "SELECT s.relname, t.relname, a.attname"
            + " FROM pg_catalog.pg_depend d"
            + " JOIN pg_catalog.pg_class s ON s.oid = d.objid AND s.relkind = 'S'"
            + " JOIN pg_catalog.pg_class t ON t.oid = d.refobjid"
            + " JOIN pg_catalog.pg_attribute a ON a.attrelid = t.oid AND a.attnum = d.refobjsubid"
            + " JOIN pg_catalog.pg_namespace n ON n.oid = s.relnamespace"
            + " WHERE d.classid = 'pg_catalog.pg_class'::regclass"
            + " AND d.refclassid = 'pg_catalog.pg_class'::regclass"
            + " AND d.deptype = 'a' AND n.nspname = ? ORDER BY s.relname";

    private PgSchema() {
    }

    /**
     * {@code ALTER SEQUENCE … OWNED BY} for every sequence of {@code schema}
     * a column owns, to run once both exist.
     *
     * @param conn      the live connection
     * @param id        the quoting rules
     * @param schema    the schema, which must be named
     * @param sequences the names of the sequences the script creates
     * @return the statements, each terminated
     */
    public static List<String> ownedSequences(Connection conn, Ident id, String schema,
                                              Set<String> sequences) {
        if (schema == null || schema.isEmpty() || sequences.isEmpty()) {
            return new ArrayList<>();
        }
        return Attempt.<List<String>>on(conn, "pg_depend owned", c -> {
            List<String> out = new ArrayList<>();
            try (PreparedStatement ps = c.prepareStatement(OWNED_SQL)) {
                ps.setString(1, schema);
                try (ResultSet rs = ps.executeQuery()) {
                    while (rs.next()) {
                        String sequence = rs.getString(1);
                        if (sequences.contains(sequence)) {
                            out.add("ALTER SEQUENCE " + id.qualify(null, schema, sequence)
                                    + " OWNED BY " + id.qualify(null, schema, rs.getString(2))
                                    + "." + id.q(rs.getString(3)) + ";");
                        }
                    }
                }
            }
            return out;
        }, new ArrayList<>());
    }

    /**
     * @param conn   the live connection
     * @param schema the schema, which must be named
     * @return the names of the schema's objects that belong to an extension
     */
    public static Set<String> extensionMembers(Connection conn, String schema) {
        if (schema == null || schema.isEmpty()) {
            return new HashSet<>();
        }
        return Attempt.<Set<String>>on(conn, "pg_depend", c -> {
            Set<String> out = new HashSet<>();
            try (PreparedStatement ps = c.prepareStatement(MEMBERS_SQL)) {
                ps.setString(1, schema);
                ps.setString(2, schema);
                ps.setString(3, schema);
                try (ResultSet rs = ps.executeQuery()) {
                    while (rs.next()) {
                        out.add(rs.getString(1));
                    }
                }
            }
            return out;
        }, new HashSet<>());
    }

    /**
     * The statements that recreate what the schema's tables are declared with,
     * in the order they can run: the extensions, then the enums, then the
     * domains, which may be over an enum.
     *
     * @param conn   the live connection
     * @param id     the quoting rules
     * @param schema the schema, which must be named
     * @return each statement, terminated, with the {@code DROP} that removes
     *         what it creates again, or {@code null} beside an extension, whose
     *         {@code CREATE} asks {@code IF NOT EXISTS}
     */
    public static List<String[]> preamble(Connection conn, Ident id, String schema) {
        List<String[]> out = new ArrayList<>();
        if (schema == null || schema.isEmpty()) {
            return out;
        }
        out.addAll(Attempt.<List<String[]>>on(conn, "pg_extension", c -> {
            List<String[]> list = new ArrayList<>();
            try (PreparedStatement ps = c.prepareStatement(EXTENSIONS_SQL)) {
                ps.setString(1, schema);
                try (ResultSet rs = ps.executeQuery()) {
                    while (rs.next()) {
                        list.add(new String[]{"CREATE EXTENSION IF NOT EXISTS "
                                + id.q(rs.getString(1)) + " WITH SCHEMA " + id.q(schema) + ";",
                                null});
                    }
                }
            }
            return list;
        }, new ArrayList<>()));
        out.addAll(Attempt.<List<String[]>>on(conn, "pg_enum", c -> {
            List<String[]> list = new ArrayList<>();
            try (PreparedStatement ps = c.prepareStatement(ENUMS_SQL)) {
                ps.setString(1, schema);
                try (ResultSet rs = ps.executeQuery()) {
                    while (rs.next()) {
                        String name = id.qualify(null, schema, rs.getString(1));
                        list.add(new String[]{"CREATE TYPE " + name + " AS ENUM ("
                                + rs.getString(2) + ");", "DROP TYPE IF EXISTS " + name + ";"});
                    }
                }
            }
            return list;
        }, new ArrayList<>()));
        out.addAll(Attempt.<List<String[]>>on(conn, "pg_type domains", c -> {
            List<String[]> list = new ArrayList<>();
            try (PreparedStatement ps = c.prepareStatement(DOMAINS_SQL)) {
                ps.setString(1, schema);
                try (ResultSet rs = ps.executeQuery()) {
                    while (rs.next()) {
                        String name = rs.getString(1);
                        String base = rs.getString(2);
                        boolean notNull = rs.getBoolean(3);
                        String def = rs.getString(4);
                        String checks = rs.getString(5);
                        StringBuilder sb = new StringBuilder("CREATE DOMAIN ")
                                .append(id.qualify(null, schema, name)).append(" AS ").append(base);
                        if (def != null && !def.isEmpty()) {
                            sb.append(" DEFAULT ").append(def);
                        }
                        if (notNull) {
                            sb.append(" NOT NULL");
                        }
                        if (checks != null && !checks.isEmpty()) {
                            sb.append(' ').append(checks);
                        }
                        list.add(new String[]{sb.append(';').toString(),
                                "DROP DOMAIN IF EXISTS " + id.qualify(null, schema, name) + ";"});
                    }
                }
            }
            return list;
        }, new ArrayList<>()));
        return out;
    }
}
