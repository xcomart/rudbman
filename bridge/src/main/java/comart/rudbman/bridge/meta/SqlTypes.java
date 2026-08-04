package comart.rudbman.bridge.meta;

import java.sql.Types;
import java.util.HashMap;
import java.util.Map;

/**
 * Names for {@link java.sql.Types} constants.
 *
 * <p>Adapted from jdbgen's {@code comart.tools.jdbgen.types.db.SqlTypes} (MIT,
 * Dennis Soungjin Park). Only the JDBC-name half is kept; jdbgen's Java-type
 * mapping belongs to a code generator, not to a database client.
 */
public final class SqlTypes {

    private static final Map<Integer, String> NAMES = new HashMap<>();

    static {
        NAMES.put(Types.ARRAY, "ARRAY");
        NAMES.put(Types.BIGINT, "BIGINT");
        NAMES.put(Types.BINARY, "BINARY");
        NAMES.put(Types.BIT, "BIT");
        NAMES.put(Types.BLOB, "BLOB");
        NAMES.put(Types.BOOLEAN, "BOOLEAN");
        NAMES.put(Types.CHAR, "CHAR");
        NAMES.put(Types.CLOB, "CLOB");
        NAMES.put(Types.DATALINK, "DATALINK");
        NAMES.put(Types.DATE, "DATE");
        NAMES.put(Types.DECIMAL, "DECIMAL");
        NAMES.put(Types.DISTINCT, "DISTINCT");
        NAMES.put(Types.DOUBLE, "DOUBLE");
        NAMES.put(Types.FLOAT, "FLOAT");
        NAMES.put(Types.INTEGER, "INTEGER");
        NAMES.put(Types.JAVA_OBJECT, "JAVA_OBJECT");
        NAMES.put(Types.LONGNVARCHAR, "LONGNVARCHAR");
        NAMES.put(Types.LONGVARBINARY, "LONGVARBINARY");
        NAMES.put(Types.LONGVARCHAR, "LONGVARCHAR");
        NAMES.put(Types.NCHAR, "NCHAR");
        NAMES.put(Types.NCLOB, "NCLOB");
        NAMES.put(Types.NULL, "NULL");
        NAMES.put(Types.NUMERIC, "NUMERIC");
        NAMES.put(Types.NVARCHAR, "NVARCHAR");
        NAMES.put(Types.OTHER, "OTHER");
        NAMES.put(Types.REAL, "REAL");
        NAMES.put(Types.REF, "REF");
        NAMES.put(Types.REF_CURSOR, "REF_CURSOR");
        NAMES.put(Types.ROWID, "ROWID");
        NAMES.put(Types.SMALLINT, "SMALLINT");
        NAMES.put(Types.SQLXML, "SQLXML");
        NAMES.put(Types.STRUCT, "STRUCT");
        NAMES.put(Types.TIME, "TIME");
        NAMES.put(Types.TIME_WITH_TIMEZONE, "TIME_WITH_TIMEZONE");
        NAMES.put(Types.TIMESTAMP, "TIMESTAMP");
        NAMES.put(Types.TIMESTAMP_WITH_TIMEZONE, "TIMESTAMP_WITH_TIMEZONE");
        NAMES.put(Types.TINYINT, "TINYINT");
        NAMES.put(Types.VARBINARY, "VARBINARY");
        NAMES.put(Types.VARCHAR, "VARCHAR");
    }

    private SqlTypes() {
    }

    /**
     * @param type a {@link java.sql.Types} constant
     * @return the JDBC name, or {@code "UNKNOWN(<n>)"} for vendor-specific codes
     */
    public static String name(int type) {
        String n = NAMES.get(type);
        return n != null ? n : "UNKNOWN(" + type + ")";
    }
}
