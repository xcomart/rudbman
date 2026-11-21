// MariaDB, described as a dialect profile on `mysql` rather than by the
// bridge's own MariaDbVendor: what MariaDB adds to MySQL, as hooks of API 1
// (docs/dialect-profiles.md, §5.2). The container suites run against the
// MariaDB server through this profile as they run through the built-in
// (crates/rudbman-jdbc/tests/containers.rs, §9). A sequence's own text is the
// profile's data, `texts.sequence`; the rest is here.

/** MariaDB's sequences, which MySQL has none of: listed as tables of their own type. */
List<Map<String, String>> sequences(Connection conn, Ident id, String catalog, String schema,
        String name) {
    // The database is the catalog; a request that names it as its schema is
    // answered too.
    String database = catalog ?: schema
    String sql = "SELECT TABLE_SCHEMA, TABLE_NAME, TABLE_COMMENT FROM information_schema.TABLES" +
            " WHERE TABLE_TYPE = 'SEQUENCE'" +
            (database ? ' AND TABLE_SCHEMA = ?' : '') +
            (name ? ' AND TABLE_NAME = ?' : '') +
            ' ORDER BY TABLE_SCHEMA, TABLE_NAME'
    List<Map<String, String>> found = []
    conn.prepareStatement(sql).withCloseable { ps ->
        int at = 1
        if (database) {
            ps.setString(at++, database)
        }
        if (name) {
            ps.setString(at, name)
        }
        ps.executeQuery().withCloseable { rs ->
            while (rs.next()) {
                Map<String, String> row = [SEQUENCE_CATALOG: rs.getString(1),
                                           SEQUENCE_NAME   : rs.getString(2)]
                if (rs.getString(3)) {
                    row.REMARKS = rs.getString(3)
                }
                found << row
            }
        }
    }
    Map<String, String> types = typesOf(conn)
    found.each { row ->
        row.DATA_TYPE = types[row.SEQUENCE_CATALOG + '.' + row.SEQUENCE_NAME] ?: 'bigint'
        if (name || found.size() <= 100) {
            options(conn, id.qualify(row.SEQUENCE_CATALOG, row.SEQUENCE_NAME), row)
        }
    }
    found
}

/** Each sequence's type: `AS <type>` arrived with the SEQUENCES view, in 11.5. */
private static Map<String, String> typesOf(Connection conn) {
    Map<String, String> out = [:]
    try {
        conn.createStatement().withCloseable { st ->
            st.executeQuery('SELECT SEQUENCE_SCHEMA, SEQUENCE_NAME, DATA_TYPE' +
                    ' FROM information_schema.SEQUENCES').withCloseable { rs ->
                while (rs.next()) {
                    out[rs.getString(1) + '.' + rs.getString(2)] = rs.getString(3)
                }
            }
        }
    } catch (SQLException ignored) {
        // Before it every sequence is a BIGINT.
    }
    out
}

/** A sequence's options, read off its own row; one that cannot be read is listed without. */
private static void options(Connection conn, String qualified, Map<String, String> row) {
    try {
        conn.createStatement().withCloseable { st ->
            st.executeQuery('SELECT * FROM ' + qualified).withCloseable { rs ->
                if (rs.next()) {
                    for (String label : ['MINIMUM_VALUE', 'MAXIMUM_VALUE', 'START_VALUE',
                                         'INCREMENT', 'CACHE_SIZE', 'CYCLE_OPTION']) {
                        row[label] = rs.getString(label.toLowerCase())
                    }
                }
            }
        }
    } catch (SQLException ignored) {
        // A privilege on the listing but not on the sequence.
    }
}

/**
 * Where a restored sequence continues from: the next value no cache has
 * taken. SHOW CREATE SEQUENCE writes the original start instead.
 */
String sequenceValue(Connection conn, Ident id, String catalog, String schema, String name,
        String listed) {
    try {
        conn.createStatement().withCloseable { st ->
            st.executeQuery('SELECT next_not_cached_value FROM ' + id.qualify(catalog, name))
                    .withCloseable { rs -> rs.next() ? rs.getString(1) : null }
        }
    } catch (SQLException ignored) {
        null
    }
}

/** The sequences put back where the source's continue; MariaDB keeps no other counter. */
List<String> counters(DatabaseMetaData dbm, Ident id, List<Sequence> sequences,
        List<Table> tables, boolean data) {
    sequences.findAll { it.value() ==~ /-?\d+/ }.collect {
        "SELECT SETVAL(${id.qualify(it.catalog(), it.name())}, ${it.value()}, 0);"
    }
}
