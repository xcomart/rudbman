package comart.rudbman.bridge.meta;

import java.sql.Connection;
import java.sql.PreparedStatement;
import java.sql.ResultSet;
import java.util.ArrayList;
import java.util.LinkedHashMap;
import java.util.List;
import java.util.Locale;
import java.util.Map;

/**
 * What SQL Server's catalog says about a table that JDBC metadata cannot, read
 * the way SSMS scripts a table: the column types as declared, a default's
 * constraint name, how the keys and indexes are clustered, what an index
 * includes and filters on, and the extended properties.
 *
 * <p>mssql-jdbc reports {@code TYPE_NAME} and sizes that do not make a type
 * back - {@code uniqueidentifier} with a size of 36, {@code money} with a
 * precision - and has no place for an alias type's schema, a collation, an
 * index's {@code INCLUDE} or {@code WHERE}, or a clustered unique index.
 *
 * <p>Every view is read through the table's own database ({@code db.sys.x}),
 * so a session sitting in another database still finds the table. Every query
 * is fenced by {@link Attempt}, a refused one leaving the metadata's answer.
 */
public final class SqlServerSchema {

    private SqlServerSchema() {
    }

    /**
     * The schema's alias types ({@code CREATE TYPE … FROM}), which its columns
     * and parameters may be declared with, to create before anything uses
     * them. Table types are not written.
     *
     * @param conn    the live connection
     * @param id      the quoting rules
     * @param catalog the database, or {@code null} for the session's own
     * @param schema  the schema, which must be named
     * @return each {@code CREATE TYPE}, terminated, with its {@code DROP}
     */
    public static List<String[]> aliasTypes(Connection conn, Ident id, String catalog,
                                            String schema) {
        if (schema == null || schema.isEmpty()) {
            return new ArrayList<>();
        }
        return Attempt.<List<String[]>>on(conn, "sys.types alias", c -> {
            List<String[]> out = new ArrayList<>();
            try (PreparedStatement ps = c.prepareStatement("SELECT t.name, b.name, t.max_length,"
                    + " t.precision, t.scale, t.is_nullable"
                    + " FROM " + sys(id, catalog, "types") + " t"
                    + " JOIN " + sys(id, catalog, "types") + " b ON b.user_type_id = t.system_type_id"
                    + " JOIN " + sys(id, catalog, "schemas") + " s ON s.schema_id = t.schema_id"
                    + " WHERE t.is_user_defined = 1 AND t.is_table_type = 0 AND s.name = ?"
                    + " ORDER BY t.name")) {
                ps.setString(1, schema);
                try (ResultSet rs = ps.executeQuery()) {
                    while (rs.next()) {
                        String name = id.qualify(null, schema, rs.getString(1));
                        out.add(new String[]{"CREATE TYPE " + name + " FROM "
                                + type(rs.getString(2), rs.getInt(3), rs.getInt(4), rs.getInt(5))
                                + (rs.getBoolean(6) ? "" : " NOT NULL") + ";",
                                "DROP TYPE IF EXISTS " + name + ";"});
                    }
                }
            }
            return out;
        }, new ArrayList<>());
    }

    /**
     * Where a table's identity stands in the source, as the statement that
     * puts a restored copy there: rows inserted with their own keys leave the
     * identity at the highest of them, and a key handed out and since deleted
     * would be handed out again.
     *
     * @param conn   the live connection
     * @param id     the quoting rules
     * @param catalog the database, or {@code null} for the session's own
     * @param object the table's name, qualified for {@code OBJECT_ID} and for
     *               the statement
     * @return a {@code DBCC CHECKIDENT}, or {@code null} for a table with no
     *         identity, or one that never handed out a value
     */
    public static String identityReseed(Connection conn, Ident id, String catalog,
                                        String object) {
        return Attempt.<String>on(conn, "sys.identity_columns", c -> {
            try (PreparedStatement ps = c.prepareStatement("SELECT"
                    + " CAST(last_value AS varchar(40)), CAST(increment_value AS varchar(40)),"
                    + " CASE WHEN EXISTS (SELECT 1 FROM " + object + ") THEN 1 ELSE 0 END"
                    + " FROM " + sys(id, catalog, "identity_columns")
                    + " WHERE object_id = OBJECT_ID(?)")) {
                ps.setString(1, object);
                try (ResultSet rs = ps.executeQuery()) {
                    if (!rs.next() || rs.getString(1) == null) {
                        return null;
                    }
                    java.math.BigInteger last = new java.math.BigInteger(rs.getString(1));
                    // A table no row has gone into since it was created takes
                    // the reseed value itself as its next key, any other the
                    // value after it.
                    if (!rs.getBoolean(3)) {
                        last = last.add(new java.math.BigInteger(rs.getString(2)));
                    }
                    return "DBCC CHECKIDENT (" + Ident.literal(object) + ", RESEED, " + last
                            + ") WITH NO_INFOMSGS;";
                }
            }
        }, null);
    }

    /** {@code [db].sys.name}, or {@code sys.name} with no database named. */
    static String sys(Ident id, String catalog, String name) {
        return (catalog != null && !catalog.isEmpty() ? id.q(catalog) + "." : "") + "sys." + name;
    }

    /**
     * Each column's type as it is declared, with its collation where that is
     * not the database's own; computed columns are left out.
     *
     * @param object the table's name, qualified for {@code OBJECT_ID}
     */
    static Map<String, String> columnTypes(Connection conn, Ident id, String catalog,
                                           String object) {
        return Attempt.<Map<String, String>>on(conn, "sys.columns", c -> {
            Map<String, String> out = new LinkedHashMap<>();
            try (PreparedStatement ps = c.prepareStatement("SELECT c.name, t.name,"
                    + " SCHEMA_NAME(t.schema_id), t.is_user_defined, c.max_length, c.precision,"
                    + " c.scale, c.collation_name,"
                    + " CAST(DATABASEPROPERTYEX(COALESCE(?, DB_NAME()), 'Collation')"
                    + " AS sysname), c.is_computed"
                    + " FROM " + sys(id, catalog, "columns") + " c"
                    + " JOIN " + sys(id, catalog, "types") + " t ON t.user_type_id = c.user_type_id"
                    + " WHERE c.object_id = OBJECT_ID(?)")) {
                ps.setString(1, catalog == null || catalog.isEmpty() ? null : catalog);
                ps.setString(2, object);
                try (ResultSet rs = ps.executeQuery()) {
                    while (rs.next()) {
                        String name = rs.getString(1);
                        String type = rs.getString(2);
                        String typeSchema = rs.getString(3);
                        boolean userDefined = rs.getBoolean(4);
                        int maxLength = rs.getInt(5);
                        int precision = rs.getInt(6);
                        int scale = rs.getInt(7);
                        String collation = rs.getString(8);
                        String dbCollation = rs.getString(9);
                        if (rs.getBoolean(10)) {
                            continue;
                        }
                        String text = userDefined ? id.qualify(null, typeSchema, type)
                                : type(type, maxLength, precision, scale);
                        if (!userDefined && collation != null && dbCollation != null
                                && !collation.equalsIgnoreCase(dbCollation)) {
                            text += " COLLATE " + collation;
                        }
                        out.put(name, text);
                    }
                }
            }
            return out;
        }, new LinkedHashMap<>());
    }

    /** A system type with the parameters it is declared with, as SSMS writes it. */
    static String type(String type, int maxLength, int precision, int scale) {
        switch (type.toLowerCase(Locale.ROOT)) {
            case "varchar":
            case "char":
            case "varbinary":
            case "binary":
                return type + "(" + (maxLength == -1 ? "max" : String.valueOf(maxLength)) + ")";
            case "nvarchar":
            case "nchar":
                return type + "(" + (maxLength == -1 ? "max" : String.valueOf(maxLength / 2)) + ")";
            case "decimal":
            case "numeric":
                return type + "(" + precision + ", " + scale + ")";
            case "datetime2":
            case "time":
            case "datetimeoffset":
                return type + "(" + scale + ")";
            case "float":
                return precision == 53 ? type : type + "(" + precision + ")";
            default:
                return type;
        }
    }

    /** Each defaulted column's default constraint name. */
    static Map<String, String> defaultNames(Connection conn, Ident id, String catalog,
                                            String object) {
        return Attempt.<Map<String, String>>on(conn, "sys.default_constraints", c -> {
            Map<String, String> out = new LinkedHashMap<>();
            try (PreparedStatement ps = c.prepareStatement("SELECT c.name, d.name"
                    + " FROM " + sys(id, catalog, "default_constraints") + " d"
                    + " JOIN " + sys(id, catalog, "columns") + " c"
                    + " ON c.object_id = d.parent_object_id AND c.column_id = d.parent_column_id"
                    + " WHERE d.parent_object_id = OBJECT_ID(?)")) {
                ps.setString(1, object);
                try (ResultSet rs = ps.executeQuery()) {
                    while (rs.next()) {
                        out.put(rs.getString(1), rs.getString(2));
                    }
                }
            }
            return out;
        }, new LinkedHashMap<>());
    }

    /**
     * @return {@code NONCLUSTERED} for a primary key whose index is not the
     *         table's clustered one, which a {@code PRIMARY KEY} is unless
     *         told; {@code null} otherwise
     */
    static String primaryKeyKind(Connection conn, Ident id, String catalog, String object) {
        return Attempt.<String>on(conn, "sys.key_constraints", c -> {
            try (PreparedStatement ps = c.prepareStatement("SELECT i.type_desc"
                    + " FROM " + sys(id, catalog, "key_constraints") + " k"
                    + " JOIN " + sys(id, catalog, "indexes") + " i"
                    + " ON i.object_id = k.parent_object_id AND i.index_id = k.unique_index_id"
                    + " WHERE k.type = 'PK' AND k.parent_object_id = OBJECT_ID(?)")) {
                ps.setString(1, object);
                try (ResultSet rs = ps.executeQuery()) {
                    return rs.next() && "NONCLUSTERED".equals(rs.getString(1)) ? "NONCLUSTERED"
                            : null;
                }
            }
        }, null);
    }

    /** The table's {@code UNIQUE} constraints, as lines of its body. */
    static List<String> uniqueConstraints(Connection conn, Ident id, String catalog,
                                          String object) {
        return Attempt.<List<String>>on(conn, "sys.key_constraints unique", c -> {
            List<String> out = new ArrayList<>();
            try (PreparedStatement ps = c.prepareStatement("SELECT k.name, i.type_desc,"
                    + " i.object_id, i.index_id"
                    + " FROM " + sys(id, catalog, "key_constraints") + " k"
                    + " JOIN " + sys(id, catalog, "indexes") + " i"
                    + " ON i.object_id = k.parent_object_id AND i.index_id = k.unique_index_id"
                    + " WHERE k.type = 'UQ' AND k.parent_object_id = OBJECT_ID(?)"
                    + " ORDER BY k.name")) {
                ps.setString(1, object);
                List<Object[]> keys = new ArrayList<>();
                try (ResultSet rs = ps.executeQuery()) {
                    while (rs.next()) {
                        keys.add(new Object[]{rs.getString(1), rs.getString(2), rs.getInt(3),
                                rs.getInt(4)});
                    }
                }
                for (Object[] k : keys) {
                    String[] cols = indexColumns(c, id, catalog, (Integer) k[2], (Integer) k[3]);
                    out.add("CONSTRAINT " + id.q((String) k[0]) + " UNIQUE"
                            + ("CLUSTERED".equals(k[1]) ? " CLUSTERED" : "") + " (" + cols[0] + ")");
                }
            }
            return out;
        }, new ArrayList<>());
    }

    /**
     * The key columns, each with {@code DESC} where it is descending, and the
     * included ones, of one index, as two column lists.
     */
    private static String[] indexColumns(Connection c, Ident id, String catalog, int objectId,
                                         int indexId) throws java.sql.SQLException {
        StringBuilder keys = new StringBuilder();
        StringBuilder included = new StringBuilder();
        try (PreparedStatement ps = c.prepareStatement("SELECT c.name, ic.is_descending_key,"
                + " ic.is_included_column"
                + " FROM " + sys(id, catalog, "index_columns") + " ic"
                + " JOIN " + sys(id, catalog, "columns") + " c"
                + " ON c.object_id = ic.object_id AND c.column_id = ic.column_id"
                + " WHERE ic.object_id = ? AND ic.index_id = ?"
                + " ORDER BY ic.is_included_column, ic.key_ordinal, ic.index_column_id")) {
            ps.setInt(1, objectId);
            ps.setInt(2, indexId);
            try (ResultSet rs = ps.executeQuery()) {
                while (rs.next()) {
                    StringBuilder sb = rs.getBoolean(3) ? included : keys;
                    if (sb.length() > 0) {
                        sb.append(", ");
                    }
                    sb.append(id.q(rs.getString(1)));
                    if (!rs.getBoolean(3) && rs.getBoolean(2)) {
                        sb.append(" DESC");
                    }
                }
            }
        }
        return new String[]{keys.toString(), included.toString()};
    }

    /**
     * The indexes of a table or an indexed view that no key or {@code UNIQUE}
     * constraint owns: rowstore ones with their clustering, {@code INCLUDE}
     * and {@code WHERE}, and columnstore ones. The clustered index comes first,
     * as it has to on a view. XML and spatial indexes are not written.
     *
     * @param object    the table's or view's name, qualified for {@code OBJECT_ID}
     * @param qualified the name to write
     * @return the statements, unterminated, or {@code null} when the catalog
     *         could not be read
     */
    static List<String> indexes(Connection conn, Ident id, String catalog, String object,
                                String qualified) {
        return Attempt.<List<String>>on(conn, "sys.indexes", c -> {
            List<Object[]> found = new ArrayList<>();
            try (PreparedStatement ps = c.prepareStatement("SELECT i.name, i.type, i.is_unique,"
                    + " i.has_filter, i.filter_definition, i.object_id, i.index_id"
                    + " FROM " + sys(id, catalog, "indexes") + " i"
                    + " WHERE i.object_id = OBJECT_ID(?) AND i.type IN (1, 2, 5, 6)"
                    + " AND i.is_primary_key = 0 AND i.is_unique_constraint = 0"
                    + " AND i.is_hypothetical = 0"
                    + " ORDER BY CASE WHEN i.type IN (1, 5) THEN 0 ELSE 1 END, i.name")) {
                ps.setString(1, object);
                try (ResultSet rs = ps.executeQuery()) {
                    while (rs.next()) {
                        found.add(new Object[]{rs.getString(1), rs.getInt(2), rs.getBoolean(3),
                                rs.getBoolean(4), rs.getString(5), rs.getInt(6), rs.getInt(7)});
                    }
                }
            }
            List<String> out = new ArrayList<>();
            for (Object[] f : found) {
                int type = (Integer) f[1];
                String[] cols = indexColumns(c, id, catalog, (Integer) f[5], (Integer) f[6]);
                StringBuilder sb = new StringBuilder("CREATE ");
                if ((Boolean) f[2]) {
                    sb.append("UNIQUE ");
                }
                sb.append(type == 1 || type == 5 ? "CLUSTERED " : "NONCLUSTERED ");
                if (type == 5 || type == 6) {
                    sb.append("COLUMNSTORE ");
                }
                sb.append("INDEX ").append(id.q((String) f[0])).append(" ON ").append(qualified);
                if (type == 6) {
                    sb.append(" (").append(cols[1].isEmpty() ? cols[0] : cols[1]).append(')');
                } else if (type != 5) {
                    sb.append(" (").append(cols[0]).append(')');
                    if (!cols[1].isEmpty()) {
                        sb.append(" INCLUDE (").append(cols[1]).append(')');
                    }
                }
                if ((Boolean) f[3] && f[4] != null) {
                    sb.append(" WHERE ").append(f[4]);
                }
                out.add(sb.toString());
            }
            return out;
        }, null);
    }

    /**
     * The table's and its columns' extended properties - {@code MS_Description},
     * which SSMS shows as the description, and any other - as the procedure
     * calls that set them.
     */
    static List<String> extendedProperties(Connection conn, Ident id, String catalog,
                                           String object, String schema, String table) {
        return Attempt.<List<String>>on(conn, "sys.extended_properties", c -> {
            List<String> out = new ArrayList<>();
            try (PreparedStatement ps = c.prepareStatement("SELECT p.name,"
                    + " CAST(p.value AS nvarchar(4000)), c.name"
                    + " FROM " + sys(id, catalog, "extended_properties") + " p"
                    + " LEFT JOIN " + sys(id, catalog, "columns") + " c"
                    + " ON c.object_id = p.major_id AND c.column_id = p.minor_id"
                    + " WHERE p.class = 1 AND p.major_id = OBJECT_ID(?)"
                    + " ORDER BY p.minor_id, p.name")) {
                ps.setString(1, object);
                try (ResultSet rs = ps.executeQuery()) {
                    while (rs.next()) {
                        String value = rs.getString(2);
                        String column = rs.getString(3);
                        StringBuilder sb = new StringBuilder("EXEC sys.sp_addextendedproperty")
                                .append(" @name = N").append(Ident.literal(rs.getString(1)))
                                .append(", @value = ")
                                .append(value == null ? "NULL" : "N" + Ident.literal(value))
                                .append(", @level0type = N'SCHEMA', @level0name = N")
                                .append(Ident.literal(schema))
                                .append(", @level1type = N'TABLE', @level1name = N")
                                .append(Ident.literal(table));
                        if (column != null) {
                            sb.append(", @level2type = N'COLUMN', @level2name = N")
                                    .append(Ident.literal(column));
                        }
                        out.add(sb.append(';').toString());
                    }
                }
            }
            return out;
        }, new ArrayList<>());
    }
}
