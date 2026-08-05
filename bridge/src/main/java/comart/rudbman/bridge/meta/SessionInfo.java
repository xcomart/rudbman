package comart.rudbman.bridge.meta;

import com.google.gson.JsonObject;
import comart.rudbman.bridge.Session;

import java.sql.Connection;
import java.sql.DatabaseMetaData;
import java.sql.SQLException;

/**
 * {@code SESSION_INFO} (op {@code 0x04}): product, driver and capability facts
 * about a live session.
 *
 * <p>Every accessor is wrapped. {@link DatabaseMetaData} is where drivers are at
 * their least reliable - several throw from methods the spec says they must
 * implement - and losing one capability flag must not lose the whole answer.
 */
public final class SessionInfo {

    private SessionInfo() {
    }

    /**
     * Collects the session description.
     *
     * @param session the session
     * @return the response body
     * @throws SQLException if the connection itself is unusable
     */
    public static JsonObject of(Session session) throws SQLException {
        session.lock();
        try {
            Connection c = session.connection();
            DatabaseMetaData md = session.metaData();
            JsonObject o = new JsonObject();

            o.addProperty("url", session.url());
            o.addProperty("driver_class", session.driverClass());
            o.addProperty("product_name", str(md::getDatabaseProductName));
            o.addProperty("product_version", str(md::getDatabaseProductVersion));
            o.addProperty("database_major", num(md::getDatabaseMajorVersion));
            o.addProperty("database_minor", num(md::getDatabaseMinorVersion));
            o.addProperty("driver_name", str(md::getDriverName));
            o.addProperty("driver_version", str(md::getDriverVersion));
            o.addProperty("jdbc_major", num(md::getJDBCMajorVersion));
            o.addProperty("jdbc_minor", num(md::getJDBCMinorVersion));
            o.addProperty("user_name", str(md::getUserName));

            o.addProperty("catalog", str(c::getCatalog));
            o.addProperty("schema", str(c::getSchema));
            o.addProperty("read_only", flag(c::isReadOnly));
            o.addProperty("auto_commit", flag(c::getAutoCommit));
            o.addProperty("transaction_isolation", num(c::getTransactionIsolation));

            o.addProperty("identifier_quote", str(md::getIdentifierQuoteString));
            o.addProperty("catalog_separator", str(md::getCatalogSeparator));
            o.addProperty("catalog_term", str(md::getCatalogTerm));
            o.addProperty("schema_term", str(md::getSchemaTerm));
            o.addProperty("procedure_term", str(md::getProcedureTerm));
            o.addProperty("search_string_escape", str(md::getSearchStringEscape));
            o.addProperty("extra_name_characters", str(md::getExtraNameCharacters));
            o.addProperty("sql_keywords", str(md::getSQLKeywords));

            o.addProperty("stores_upper_case_identifiers", flag(md::storesUpperCaseIdentifiers));
            o.addProperty("stores_lower_case_identifiers", flag(md::storesLowerCaseIdentifiers));
            o.addProperty("stores_mixed_case_identifiers", flag(md::storesMixedCaseIdentifiers));
            o.addProperty("supports_mixed_case_quoted_identifiers",
                    flag(md::supportsMixedCaseQuotedIdentifiers));
            o.addProperty("supports_transactions", flag(md::supportsTransactions));
            o.addProperty("supports_savepoints", flag(md::supportsSavepoints));
            o.addProperty("supports_batch_updates", flag(md::supportsBatchUpdates));
            o.addProperty("supports_schemas_in_table_definitions",
                    flag(md::supportsSchemasInTableDefinitions));
            o.addProperty("supports_catalogs_in_table_definitions",
                    flag(md::supportsCatalogsInTableDefinitions));
            o.addProperty("supports_stored_procedures", flag(md::supportsStoredProcedures));
            o.addProperty("supports_get_generated_keys", flag(md::supportsGetGeneratedKeys));
            o.addProperty("supports_multiple_result_sets", flag(md::supportsMultipleResultSets));
            o.addProperty("default_transaction_isolation",
                    num(md::getDefaultTransactionIsolation));
            o.addProperty("max_statement_length", num(md::getMaxStatementLength));
            return o;
        } finally {
            session.unlock();
        }
    }

    private interface StrCall {
        String call() throws SQLException;
    }

    private interface NumCall {
        int call() throws SQLException;
    }

    private interface FlagCall {
        boolean call() throws SQLException;
    }

    private static String str(StrCall c) {
        try {
            return c.call();
        } catch (SQLException | RuntimeException | AbstractMethodError e) {
            return null;
        }
    }

    private static Integer num(NumCall c) {
        try {
            return c.call();
        } catch (SQLException | RuntimeException | AbstractMethodError e) {
            return null;
        }
    }

    private static Boolean flag(FlagCall c) {
        try {
            return c.call();
        } catch (SQLException | RuntimeException | AbstractMethodError e) {
            return null;
        }
    }
}
