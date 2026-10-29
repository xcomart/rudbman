use super::*;

#[test]
fn a_comment_before_a_write_does_not_hide_it() {
    let h2 = Dialect::H2;
    // The whole reason the judgment goes through the lexer: a script's
    // explanation sits above the statement it explains.
    assert!(is_write_statement(
        "-- nightly clean-up\nUPDATE orders SET state = 'x'",
        &h2
    ));
    assert!(is_write_statement(
        "/* two\n   lines */ delete from orders",
        &h2
    ));
    assert!(is_write_statement("  \n\tinsert into t values (1)", &h2));
    assert!(is_write_statement("drop table t", &h2));
    assert!(is_write_statement("call do_something()", &h2));

    // And a `WITH` that only selects is a read, comment and all.
    assert!(!is_write_statement(
        "-- who ordered what\nWITH recent AS (SELECT * FROM orders) SELECT * FROM recent",
        &h2
    ));
    assert!(!is_write_statement("select 1", &h2));
    assert!(!is_write_statement("EXPLAIN select 1", &h2));
    assert!(!is_write_statement("show tables", &h2));

    assert!(is_write_statement(
        "WITH removed AS (DELETE FROM t RETURNING id) SELECT * FROM removed",
        &h2
    ));
    assert!(is_write_statement("EXPLAIN ANALYZE UPDATE t SET v=1", &h2));
    assert!(is_write_statement("SELECT * INTO backup FROM t", &h2));
    assert!(is_write_statement("/*! DELETE FROM t */", &Dialect::MYSQL));

    // Nothing to send is neither: no statement, so no question.
    assert!(!is_write_statement("", &h2));
    assert!(!is_write_statement("   \n  ", &h2));
    assert!(!is_write_statement("-- only a comment", &h2));
}

/// Which statements are read a page at a time, and what the page clause is
/// appended to.
#[test]
fn a_plain_read_is_paged_and_anything_that_cuts_its_own_rows_is_not() {
    use crate::query::statements::pageable_body;

    for dialect in [
        Dialect::H2,
        Dialect::POSTGRES,
        Dialect::MYSQL,
        Dialect::MARIADB,
        Dialect::SQLITE,
        Dialect::DB2,
    ] {
        let name = dialect.name();
        for (sql, body) in [
            ("select * from t", "select * from t"),
            // The trailing trivia goes, or a `-- note` would swallow the
            // clause appended after it.
            (
                "  select * from t order by a desc; -- newest first\n",
                "select * from t order by a desc",
            ),
            (
                "with r as (select * from t limit 5) select * from r",
                "with r as (select * from t limit 5) select * from r",
            ),
            // The words that refuse a page count at the top level only: a
            // subquery may cut its own rows, and a function may be called
            // anything.
            (
                "select * from t where id in (select id from u order by id limit 3)",
                "select * from t where id in (select id from u order by id limit 3)",
            ),
            (
                "select a, b from t join u on t.id = u.id",
                "select a, b from t join u on t.id = u.id",
            ),
            (
                "select * from (select 1 as a) x union all select 2",
                "select * from (select 1 as a) x union all select 2",
            ),
            // The sort round trip's wrapper is a plain read like any other.
            (
                "SELECT * FROM (select id from t limit 10) rudbman_sort ORDER BY id DESC",
                "SELECT * FROM (select id from t limit 10) rudbman_sort ORDER BY id DESC",
            ),
        ] {
            assert_eq!(pageable_body(sql, &dialect), Some(body), "{name}: {sql}");
        }

        for sql in [
            // Already cut: its own page is the user's, not ours to cut again.
            "select * from t limit 10",
            "select * from t order by a limit 10 offset 20",
            "select * from t offset 5 rows fetch next 10 rows only",
            // Clauses that must stay last, or a page clause cannot follow.
            "select * from t for update",
            "select * from t where a = 1 lock in share mode",
            "select * from t for system_time all",
            // Db2's isolation clause, and MySQL's rollup beside it.
            "select * from t with ur",
            "select a, count(*) from t group by a with rollup",
            // Reads, but not ones a page clause belongs on.
            "show tables",
            "explain select * from t",
            "(select 1) union (select 2)",
            // Not reads at all.
            "update t set a = 1",
            "select * into u from t",
            "call p()",
            "",
            "-- nothing",
            "select 1; select 2",
        ] {
            assert_eq!(pageable_body(sql, &dialect), None, "{name}: {sql}");
        }
    }

    // A dialect with no page clause keeps the cursor for everything.
    for dialect in [Dialect::ORACLE, Dialect::MSSQL, Dialect::GENERIC] {
        assert_eq!(
            pageable_body("select * from t", &dialect),
            None,
            "{}",
            dialect.name()
        );
    }
}

/// Which statements SQL Server reads through a server cursor: plain reads,
/// including the ones it would not page, but nothing with a top-level `FOR`,
/// which SQL Server refuses in a cursor.
#[test]
fn sql_server_reads_plain_selects_through_a_server_cursor() {
    use crate::query::statements::server_cursor_read;

    let mssql = Dialect::MSSQL;
    for sql in [
        "select * from t",
        "SELECT TOP 10 * FROM t ORDER BY id",
        "select id from t order by id offset 0 rows fetch next 5 rows only",
        "with r as (select id from t) select * from r",
        "select a, (select b from u for xml path('')) as x from t",
        "  select * from t; -- trailing note\n",
    ] {
        assert!(server_cursor_read(sql, &mssql), "{sql}");
    }
    for sql in [
        "select id from t for json path",
        "SELECT id FROM t FOR XML AUTO",
        "select * from t for system_time all",
        "update t set a = 1",
        "select * into u from t",
        "exec sp_who",
        "select 1; select 2",
        "",
    ] {
        assert!(!server_cursor_read(sql, &mssql), "{sql}");
    }

    // Every other dialect keeps the driver's default result set.
    for dialect in [
        Dialect::GENERIC,
        Dialect::H2,
        Dialect::POSTGRES,
        Dialect::MYSQL,
        Dialect::MARIADB,
        Dialect::SQLITE,
        Dialect::ORACLE,
    ] {
        assert!(
            !server_cursor_read("select * from t", &dialect),
            "{}",
            dialect.name()
        );
    }
}
