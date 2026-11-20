package comart.rudbman.bridge.meta;

import com.google.gson.JsonArray;
import com.google.gson.JsonObject;

import java.sql.Connection;
import java.sql.DatabaseMetaData;
import java.sql.SQLException;
import java.util.Collections;
import java.util.List;
import java.util.Map;

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
