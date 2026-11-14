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

#[test]
fn a_selection_is_cut_into_its_statements_where_it_sits_in_the_editor() {
    use crate::query::statements;
    let selection_statements = |selected: &str, offset: usize, dialect: &Dialect| {
        statements::selection_statements(selected, offset, dialect).unwrap()
    };
    let sql = |cut: &[(String, Option<std::ops::Range<usize>>)]| {
        cut.iter().map(|(sql, _)| sql.clone()).collect::<Vec<_>>()
    };
    let ranges = |cut: &[(String, Option<std::ops::Range<usize>>)]| {
        cut.iter()
            .map(|(_, range)| range.clone())
            .collect::<Vec<_>>()
    };

    // The statements the report was about, which MySQL refused sent together.
    let grants = "CREATE USER IF NOT EXISTS 'aibook'@'%' IDENTIFIED BY 'aibook';\n\
                  GRANT ALL PRIVILEGES ON financedb.* TO 'aibook'@'%';\n\
                  FLUSH PRIVILEGES;";
    let cut = selection_statements(grants, 100, &Dialect::MYSQL);
    assert_eq!(
        sql(&cut),
        [
            "CREATE USER IF NOT EXISTS 'aibook'@'%' IDENTIFIED BY 'aibook'",
            "GRANT ALL PRIVILEGES ON financedb.* TO 'aibook'@'%'",
            "FLUSH PRIVILEGES",
        ]
    );
    // Each range is the statement's place in the editor, not in the selection.
    for (statement, range) in &cut {
        let range = range.clone().unwrap();
        assert_eq!(&grants[range.start - 100..range.end - 100], statement);
    }

    // One statement loses only its semicolon.
    let one = selection_statements("select 1;", 4, &Dialect::H2);
    assert_eq!(sql(&one), ["select 1"]);
    assert_eq!(ranges(&one), [Some(4..12)]);

    // What the splitter would cut into fragments goes whole, as selected.
    let block = "begin\n  update t set a = 1;\n  commit;\nend;";
    let whole = selection_statements(block, 7, &Dialect::ORACLE);
    assert_eq!(sql(&whole), [block]);
    assert_eq!(ranges(&whole), [Some(7..7 + block.len())]);

    // A MySQL executable comment standing alone is not dropped.
    let dump = "/*!40101 SET NAMES utf8mb4 */;\nselect 1;";
    assert_eq!(sql(&selection_statements(dump, 0, &Dialect::MYSQL)), [dump]);
    let only = "/*!40101 SET NAMES utf8mb4 */";
    assert_eq!(sql(&selection_statements(only, 0, &Dialect::MYSQL)), [only]);
    // One that leads a statement travels with it.
    assert_eq!(
        sql(&selection_statements(
            "/*!40101 SET @x = 1 */ select 1; select 2",
            0,
            &Dialect::MYSQL
        )),
        ["/*!40101 SET @x = 1 */ select 1", "select 2"]
    );
}

#[test]
fn run_all_cuts_the_script_the_way_its_client_does() {
    use crate::query::statements::{script_statements, selection_statements};
    let sql = |cut: Vec<(String, Option<std::ops::Range<usize>>)>| {
        cut.into_iter().map(|(sql, _)| sql).collect::<Vec<_>>()
    };

    // A MySQL script with its routine between DELIMITER lines, which the
    // semicolon splitter cut into fragments.
    let mysql = "DELIMITER //\nCREATE PROCEDURE p() BEGIN SELECT 1; SELECT 2; END//\n\
                 DELIMITER ;\nCALL p();";
    assert_eq!(
        sql(script_statements(mysql, 0, &Dialect::MYSQL).unwrap()),
        [
            "CREATE PROCEDURE p() BEGIN SELECT 1; SELECT 2; END",
            "CALL p()"
        ]
    );

    // SQL Server batches, and a routine whose body runs to its GO.
    let mssql = "CREATE PROCEDURE p AS\n  SELECT 1;\n  SELECT 2;\nGO\nEXEC p;\nselect 3";
    assert_eq!(
        sql(script_statements(mssql, 0, &Dialect::MSSQL).unwrap()),
        [
            "CREATE PROCEDURE p AS\n  SELECT 1;\n  SELECT 2;",
            "EXEC p",
            "select 3"
        ]
    );

    // An Oracle unit runs to its slash, and the rest is cut as before.
    let oracle = "create or replace procedure p as\nbegin\n  null;\nend;\n/\nselect 1 from dual;";
    let cut = script_statements(oracle, 0, &Dialect::ORACLE).unwrap();
    assert_eq!(
        sql(cut.clone()),
        [
            "create or replace procedure p as\nbegin\n  null;\nend;",
            "select 1 from dual"
        ]
    );
    let range = cut[1].1.clone().unwrap();
    assert_eq!(&oracle[range], "select 1 from dual");

    // A command the run cannot carry out stops it before anything is sent,
    // pointing at its line.
    let error = script_statements("select 1;\n\\connect other\n", 0, &Dialect::POSTGRES)
        .expect_err("\\connect cannot be run");
    assert_eq!(error.line, 2);

    // A selection with a client line in it is cut the same way, at its place
    // in the editor.
    let cut = selection_statements("select 1\nGO\nselect 2", 10, &Dialect::MSSQL).unwrap();
    assert_eq!(
        cut,
        [
            ("select 1".to_string(), Some(10..18)),
            ("select 2".to_string(), Some(22..30)),
        ]
    );
    // One without keeps the old fallback: a routine typed without DELIMITER
    // lines goes whole.
    let routine = "create procedure p() begin select 1; select 2; end";
    assert_eq!(
        sql(selection_statements(routine, 0, &Dialect::MYSQL).unwrap()),
        [routine]
    );
}

#[test]
fn run_statement_sends_the_unit_the_editor_highlighted() {
    use crate::query::statements::editor_statement;
    use crate::sql_highlight::DialectHighlighter;
    use rugpui_editor::{Buffer, SyntaxCache, syntax};

    // What the editor highlights at `needle`, and what "Run statement" sends
    // for it.
    let run = |script: &str, needle: &str, dialect: Dialect| {
        let buffer = Buffer::new(script);
        let cache = SyntaxCache::new(&buffer, Some(DialectHighlighter::shared(dialect)));
        let at = script.find(needle).expect("the needle is in the script");
        let span = syntax::statement_at(&buffer, &cache, at).expect("a statement");
        editor_statement(script, span, &dialect)
    };

    // A body is sent whole, and without the terminator a driver would refuse.
    let mysql = "CREATE PROCEDURE p()\nBEGIN\n  SELECT 1;\n  SELECT 2;\nEND;\nCALL p();";
    let (sql, range) = run(mysql, "SELECT 2", Dialect::MYSQL).unwrap();
    assert_eq!(
        sql,
        "CREATE PROCEDURE p()\nBEGIN\n  SELECT 1;\n  SELECT 2;\nEND"
    );
    assert_eq!(&mysql[range.unwrap()], sql);
    let delimited = "DELIMITER //\nCREATE PROCEDURE p() BEGIN SELECT 1; END//\nDELIMITER ;";
    assert_eq!(
        run(delimited, "SELECT 1", Dialect::MYSQL).unwrap().0,
        "CREATE PROCEDURE p() BEGIN SELECT 1; END"
    );
    // PL/SQL keeps its `END;`, and SQL*Plus's `EXEC` is the block it stands for.
    let oracle = "begin\n  null;\nend;\n/\nexec dbms_lock.sleep(1)\n";
    assert_eq!(
        run(oracle, "null", Dialect::ORACLE).unwrap().0,
        "begin\n  null;\nend;"
    );
    assert_eq!(
        run(oracle, "exec", Dialect::ORACLE).unwrap().0,
        "BEGIN dbms_lock.sleep(1); END;"
    );
    // A T-SQL routine runs to its GO.
    let mssql = "CREATE PROCEDURE p AS\n  SELECT 1;\n  SELECT 2;\nGO\nEXEC p";
    assert_eq!(
        run(mssql, "SELECT 2", Dialect::MSSQL).unwrap().0,
        "CREATE PROCEDURE p AS\n  SELECT 1;\n  SELECT 2;"
    );
    // A line the client would refuse is refused here too, with its line.
    let error = run("select 1;\n\\connect other\n", "connect", Dialect::POSTGRES)
        .expect_err("\\connect cannot be run");
    assert_eq!(error.line, 2);
    // Plain statements go as they always went.
    let plain = "select 1;\nselect 2;";
    assert_eq!(
        run(plain, "2", Dialect::POSTGRES).unwrap(),
        ("select 2".to_string(), Some(10..18))
    );
}
