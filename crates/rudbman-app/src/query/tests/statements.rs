use super::*;

#[test]
fn a_comment_before_a_write_does_not_hide_it() {
    let h2 = Dialect::H2;
    // The whole reason the judgement goes through the lexer: a script's
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
