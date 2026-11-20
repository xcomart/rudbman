package comart.rudbman.bridge.meta;

import com.google.gson.JsonArray;
import com.google.gson.JsonObject;

import java.sql.Connection;
import java.sql.DatabaseMetaData;
import java.sql.SQLException;
import java.util.Collections;
import java.util.HashSet;
import java.util.List;
import java.util.Map;
import java.util.Set;

/**
 * Everything the bridge does differently per database product.
 *
 * <p>Each product the bridge knows is one subclass in this package, and its
 * {@link Dialect} constant holds an instance. The code that reads catalogs,
 * writes DDL and runs jobs asks {@code dialect.vendor()} what to do rather
 * than testing which product it is, so a product's habits are read in one
 * file.
 *
 * <p>Every method's default is what the bridge does for {@link Dialect#OTHER}:
 * the portable, standard-JDBC path. That is safe by the rule every caller
 * already keeps - a product the bridge does not recognize must stay useful -
 * so a new product starts out as {@code OTHER} and overrides only where it
 * differs. Long product-specific SQL and readers stay beside the code that
 * runs them, package-private, and the product's override is what picks them.
 *
 * <p>Adding a product:
 * <ol>
 *   <li>Write a subclass, starting from the closest existing product, and
 *       override what differs. {@link #names} is the one method every product
 *       answers.</li>
 *   <li>Give it a {@link Dialect} constant and a place in that enum's
 *       detection order.</li>
 *   <li>Run the container suites against a server of that product.</li>
 * </ol>
 *
 * <p>The constructor is package-private: products live here, beside the
 * package-private readers their overrides call.
 */
public abstract class Vendor {

    Vendor() {
    }

    /**
     * @param productName {@link DatabaseMetaData#getDatabaseProductName()},
     *                    lower-cased
     * @return whether it names this product
     */
    abstract boolean names(String productName);

    // ------------------------------------------------------------------ names

    /**
     * Whether the JDBC catalog is the namespace a table sits in, the level
     * {@code INFORMATION_SCHEMA} calls a schema.
     *
     * <p>MySQL and MariaDB, which put the database in {@code TABLE_CAT} and
     * leave {@code TABLE_SCHEM} null.
     *
     * @return {@code false} by default
     */
    public boolean catalogIsSchema() {
        return false;
    }

    // --------------------------------------------------------------- catalogs

    /**
     * Whether a result column's catalog, as the driver reports it, is one its
     * own metadata calls recognize. Where it is not, it is dropped rather than
     * passed on to a lookup that can only answer nothing.
     *
     * @return {@code true} by default
     */
    public boolean resultCatalogsResolve() {
        return true;
    }

    /**
     * Whether a session sees only the catalog it connected to, whatever
     * {@code getCatalogs} lists, so that only that one is answered.
     *
     * @return {@code false} by default
     */
    boolean seesOnlyItsCatalog() {
        return false;
    }

    // --------------------------------------------------------------- routines

    /**
     * Whether a procedure's name carries a group number ({@code name;1}) and
     * no separate function list is to be read.
     *
     * @return {@code false} by default
     */
    boolean numbersProcedures() {
        return false;
    }

    /**
     * Tells the overloads of one routine name apart, in place of the driver's
     * list.
     *
     * @param dbm   the connection metadata
     * @param req   the request, for the routine's name and schema
     * @param items the routines the driver listed
     * @return the list to answer, {@code items} unchanged by default
     */
    JsonArray identifyOverloads(DatabaseMetaData dbm, JsonObject req, JsonArray items) {
        return items;
    }

    /**
     * @param remarks a routine's {@code REMARKS} as the driver reported them
     * @return the comment, without anything the driver writes for every
     *         routine; {@code remarks} unchanged by default
     */
    String routineRemarks(String remarks) {
        return remarks;
    }

    /**
     * Asks the server for a routine's own text.
     *
     * @param conn     the live connection
     * @param id       the quoting rules
     * @param catalog  the routine's catalog, may be {@code null}
     * @param schema   the routine's schema, may be {@code null}
     * @param name     the routine's name
     * @param function whether it is listed as a function
     * @return the text, or {@code null} where the server has none to give;
     *         {@code null} by default
     */
    String routineText(Connection conn, Ident id, String catalog, String schema, String name,
                       boolean function) {
        return null;
    }

    // -------------------------------------------------------------- sequences

    /**
     * Lists the sequences in a way of the product's own, for a product whose
     * sequences no catalog query of {@link #sequenceQueries} answers.
     *
     * @param conn the live connection
     * @param id   the quoting rules
     * @param req  the request: {@code catalog}, {@code schema}, {@code name}
     * @return the sequences, or {@code null} to run {@link #sequenceQueries};
     *         {@code null} by default
     */
    JsonArray sequences(Connection conn, Ident id, JsonObject req) {
        return null;
    }

    /**
     * @param id      the quoting rules
     * @param catalog the requested catalog, may be {@code null}
     * @return the catalog queries that list sequences, the first one the
     *         server answers being the one used; none by default
     */
    List<String> sequenceQueries(Ident id, String catalog) {
        return List.of();
    }

    // --------------------------------------------------------------- triggers

    /**
     * Lists the triggers in a way of the product's own, for a product with no
     * trigger catalog {@link #triggerQuery} could read.
     *
     * @param conn the live connection
     * @param name the trigger's name, or {@code null} for all
     * @return the triggers, or {@code null} to run {@link #triggerQuery};
     *         {@code null} by default
     */
    JsonArray triggers(Connection conn, String name) {
        return null;
    }

    /**
     * Writes the trigger catalog query, nine columns: catalog, schema, name,
     * table, timing, event, level, enabled, comment.
     *
     * @param sql     the statement, to append to
     * @param args    its bind values, to append to
     * @param id      the quoting rules
     * @param catalog the requested catalog, or {@code null}
     * @param schema  the requested schema, or {@code null}
     * @param name    the requested trigger, or {@code null}
     */
    void triggerQuery(StringBuilder sql, List<String> args, Ident id, String catalog,
                      String schema, String name) {
        sql.append(String.format(Triggers.STANDARD_SQL, "NULL"));
        Triggers.filter(sql, args, "TRIGGER_SCHEMA", schema);
        Triggers.filter(sql, args, "TRIGGER_NAME", name);
        sql.append(" ORDER BY 2, 3");
    }

    /**
     * Rewrites one row of {@link #triggerQuery}'s result, in place, where the
     * product's catalog says something in a shape of its own.
     *
     * @param r the nine columns
     */
    void triggerRow(String[] r) {
    }

    // --------------------------------------------------------------- comments

    /**
     * Whether {@code getTables} reports a view's type in its {@code REMARKS},
     * where no comment was written.
     *
     * @return {@code false} by default
     */
    boolean marksViewsInRemarks() {
        return false;
    }

    /**
     * Whether the driver leaves out comments the server holds, so that they
     * are read from the catalog.
     *
     * @return {@code false} by default
     */
    boolean hidesComments() {
        return false;
    }

    /**
     * @param conn the live connection
     * @param req  the request, for the filters to scope the query with
     * @return table comments, keyed by owner and table name; none by default
     * @throws SQLException if the query fails
     */
    Map<String, String> tableComments(Connection conn, JsonObject req) throws SQLException {
        return Collections.emptyMap();
    }

    /**
     * @param conn the live connection
     * @param req  the request, for the filters to scope the query with
     * @return column comments, keyed by owner, table name and column name;
     *         none by default
     * @throws SQLException if the query fails
     */
    Map<String, String> columnComments(Connection conn, JsonObject req) throws SQLException {
        return Collections.emptyMap();
    }

    // ----------------------------------------------------------------- upsert

    /**
     * Whether {@link #upsert} can write a statement for this product.
     *
     * @return {@code false} by default: there is no portable upsert
     */
    boolean upserts() {
        return false;
    }

    /**
     * Writes the upsert statement; see {@link Upsert#sql}.
     *
     * @param id        the quoting rules
     * @param qualified the target table, qualified and quoted
     * @param columns   every column written, unquoted, in bind order
     * @param keys      the conflict key columns, unquoted
     * @param updates   the columns that are not part of the key, in order
     * @return SQL with one {@code ?} per column, or {@code null} where
     *         {@link #upserts} is false
     */
    String upsert(Ident id, String qualified, List<String> columns, List<String> keys,
                  List<String> updates) {
        return null;
    }

    // -------------------------------------------------------------- table DDL

    /**
     * @return what a data script runs before its rows, terminated; see
     *         {@link Ddl#dataPrologue}. None by default.
     */
    List<String> dataPrologue() {
        return List.of();
    }

    /** @return what undoes {@link #dataPrologue}, terminated; none by default */
    List<String> dataEpilogue() {
        return List.of();
    }

    /**
     * @return what a DDL script starts with so that its {@code CREATE}s replay
     *         in any order, terminated; see {@link Ddl#scriptPrologue}. None by
     *         default.
     */
    List<String> scriptPrologue() {
        return List.of();
    }

    /** @return what undoes {@link #scriptPrologue}, terminated; none by default */
    List<String> scriptEpilogue() {
        return List.of();
    }

    /**
     * Asks the server for a table's own DDL.
     *
     * @return the text, or {@code null} where the product has none or the
     *         attempt fails; {@code null} by default
     */
    String tableText(Connection conn, Ident id, String catalog, String schema, String table) {
        return null;
    }

    /**
     * The server's own text with the foreign keys out of the creation order's
     * way.
     *
     * @return the script, or {@code null} for a product whose text cannot be
     *         split that way; {@code null} by default
     */
    Ddl.Script nativeSplit(Connection conn, Ident id, String catalog, String schema,
                           String table) {
        return null;
    }

    /**
     * Whether the reconstruction writes a generated column as one: only where
     * {@link #catalogExtras} recovers the expression.
     *
     * @return {@code false} by default
     */
    boolean reconstructsGenerated() {
        return false;
    }

    /**
     * What the product's own catalog knows and JDBC metadata does not: the
     * {@code CHECK} constraints, returned as table-body lines, and what it
     * sets on the columns - native types, generated expressions, identities,
     * default names.
     *
     * @return the table-body lines; none by default, which leaves the
     *         reconstruction what JDBC alone gives
     */
    List<String> catalogExtras(Connection conn, Ident id, String catalog, String schema,
                               String table, List<Ddl.Col> cols) {
        return List.of();
    }

    /**
     * Corrects the driver's {@code IS_GENERATEDCOLUMN}, in place, where it
     * cannot be taken at its word.
     */
    void confirmGenerated(Connection conn, String catalog, String table, List<Ddl.Col> cols) {
    }

    /**
     * @return the names of the table's {@code GENERATED ALWAYS} identity
     *         columns; none by default
     */
    Set<String> alwaysIdentities(Connection conn, String schema, String table) {
        return new HashSet<>();
    }

    /**
     * Whether a statement's object name includes its catalog. Not where a
     * session sees one database and a catalog-qualified name reads as a
     * cross-database reference.
     *
     * @return {@code true} by default
     */
    boolean qualifiesByCatalog() {
        return true;
    }

    /**
     * @param object the table's name, qualified and quoted
     * @return the word written between {@code PRIMARY KEY} and its columns,
     *         such as {@code CLUSTERED}, or {@code null}; {@code null} by default
     */
    String primaryKeyKind(Connection conn, Ident id, String catalog, String object) {
        return null;
    }

    /**
     * Whether a foreign key can be {@code DEFERRABLE}, so that the reconstruction
     * writes what the driver reports of it.
     *
     * @return {@code false} by default
     */
    boolean defersKeys() {
        return false;
    }

    /**
     * Whether comments are part of the {@code CREATE}: a table's as
     * {@code COMMENT=}, a column's as {@code COMMENT} in its definition.
     *
     * @return {@code false} by default
     */
    boolean commentsInline() {
        return false;
    }

    /**
     * Whether comments are written as {@code COMMENT ON} after the
     * {@code CREATE}.
     *
     * @return {@code true} by default
     */
    boolean commentsOn() {
        return true;
    }

    /**
     * The table's indexes as the server writes them, in place of the ones
     * rebuilt from {@code getIndexInfo}.
     *
     * @param qualified the table's name as the {@code CREATE} writes it
     * @return the statements, unterminated, or {@code null} to rebuild them;
     *         {@code null} by default
     */
    List<String> nativeIndexes(Connection conn, Ident id, String catalog, String schema,
                               String table, String qualified) {
        return null;
    }

    /**
     * @return statements, terminated, that set what the product keeps beside
     *         the DDL and comments; none by default
     */
    List<String> extendedProperties(Connection conn, Ident id, String catalog, String schema,
                                    String table) {
        return List.of();
    }

    /**
     * Whether a computed column is written without a type of its own.
     *
     * @return {@code false} by default
     */
    boolean computedHasNoType() {
        return false;
    }

    /**
     * Whether an auto-increment column whose default draws on a sequence is a
     * serial pseudo-type, written as its integer and that default.
     *
     * @return {@code false} by default
     */
    boolean serialColumns() {
        return false;
    }

    /** @return the clause an auto-increment column is written with */
    String identityClause() {
        return "GENERATED BY DEFAULT AS IDENTITY";
    }

    /**
     * Whether a column definition puts {@code NOT NULL} ahead of
     * {@code DEFAULT} and the identity clause, as MySQL's grammar fixes it.
     *
     * @return {@code false} by default: identity or {@code DEFAULT}, then
     *         {@code NOT NULL}
     */
    boolean notNullFirst() {
        return false;
    }

    /**
     * @param name the type's name
     * @param size {@code COLUMN_SIZE}
     * @return the type written with the product's own spelling of an
     *         over-long length, or {@code null}; {@code null} by default
     */
    String maxType(String name, Integer size) {
        return null;
    }

    /**
     * Whether the server makes an index for every foreign key that has none
     * and lists it like any other, so that one backing a foreign key is not
     * written out to be made twice.
     *
     * @return {@code true} by default, the safe answer for an unknown product
     */
    boolean indexesForeignKeys() {
        return true;
    }

    // ------------------------------------------------------------- object DDL

    /** @return a view's own text, or {@code null}; {@code null} by default */
    String viewText(Connection conn, ObjectDdl.Target t) {
        return null;
    }

    /** @return a sequence's own text, or {@code null}; {@code null} by default */
    String sequenceText(Connection conn, ObjectDdl.Target t) {
        return null;
    }

    /** @return a trigger's own text, or {@code null}; {@code null} by default */
    String triggerText(Connection conn, ObjectDdl.Target t) {
        return null;
    }

    /**
     * Whether a {@code CREATE} names its object without the database, which
     * the product refuses there where a lookup accepts it.
     *
     * @return {@code false} by default
     */
    boolean createsWithoutCatalog() {
        return false;
    }

    /**
     * Whether {@code CREATE SEQUENCE} takes an {@code AS} type.
     *
     * @return {@code true} by default
     */
    boolean typesSequences() {
        return true;
    }

    /**
     * Whether a sequence cache of one is written {@code NO CACHE}, the product
     * keeping the one as the other and refusing {@code CACHE 1}.
     *
     * @return {@code false} by default
     */
    boolean cacheOfOneIsNoCache() {
        return false;
    }

    // ---------------------------------------------------------------- extract

    /**
     * Whether a table can carry a {@code GENERATED ALWAYS} identity whose
     * values an {@code INSERT} only takes with {@code OVERRIDING SYSTEM VALUE}.
     *
     * @return {@code false} by default
     */
    public boolean overridesSystemValue() {
        return false;
    }
}
