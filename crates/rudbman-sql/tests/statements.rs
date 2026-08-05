//! Statement splitting, and the cursor rules of [`statement_at`].

use rudbman_sql::{Dialect, split_statements, statement_at};

/// The `sql` text of every statement.
fn sqls<'a>(source: &'a str, dialect: &Dialect) -> Vec<&'a str> {
    split_statements(source, dialect)
        .into_iter()
        .map(|s| s.sql(source))
        .collect()
}

/// A semicolon inside a literal is not a boundary. This is the whole reason the
/// splitter runs on the lexer instead of on `str::split(';')`.
#[test]
fn semicolons_inside_literals_are_not_boundaries() {
    assert_eq!(
        sqls("select 'a;b' from t; select 2", &Dialect::GENERIC),
        ["select 'a;b' from t", "select 2"]
    );
    assert_eq!(
        sqls("select \"a;b\" from t;", &Dialect::ORACLE),
        ["select \"a;b\" from t"]
    );
    assert_eq!(
        sqls("select `a;b` from t;", &Dialect::MYSQL),
        ["select `a;b` from t"]
    );
    assert_eq!(
        sqls("select [a;b] from t;", &Dialect::MSSQL),
        ["select [a;b] from t"]
    );
    assert_eq!(
        sqls("select 'it''s; fine';", &Dialect::GENERIC),
        ["select 'it''s; fine'"]
    );
}

/// Nor is one inside a comment, of either shape or however many lines long.
#[test]
fn semicolons_inside_comments_are_not_boundaries() {
    assert_eq!(
        sqls("select 1 -- a ; b\n, 2;", &Dialect::GENERIC),
        ["select 1 -- a ; b\n, 2"]
    );
    assert_eq!(
        sqls("select /* a ; b\n c ; d */ 1;", &Dialect::GENERIC),
        ["select /* a ; b\n c ; d */ 1"]
    );
    assert_eq!(
        sqls("select 1 # a ; b\n;", &Dialect::MYSQL),
        ["select 1 # a ; b"]
    );
    // Nested, in a dialect that nests: the first `*/` does not end it.
    assert_eq!(
        sqls("select /* a /* ; */ ; */ 1;", &Dialect::POSTGRES),
        ["select /* a /* ; */ ; */ 1"]
    );
}

/// A dollar-quoted body is the hard case: it is where the semicolons of a
/// function body live.
#[test]
fn semicolons_inside_dollar_quotes_are_not_boundaries() {
    let script = "create function f() returns int as $body$\n\
                  begin\n\
                  \x20 return 1;\n\
                  end;\n\
                  $body$ language plpgsql;\n\
                  select f();";
    let spans = split_statements(script, &Dialect::POSTGRES);
    assert_eq!(spans.len(), 2, "{:#?}", sqls(script, &Dialect::POSTGRES));
    assert!(spans[0].sql(script).starts_with("create function"));
    assert!(spans[0].sql(script).ends_with("language plpgsql"));
    assert_eq!(spans[1].sql(script), "select f()");
}

/// The last statement of a script usually has no semicolon.
#[test]
fn a_trailing_statement_without_a_semicolon() {
    assert_eq!(sqls("select 1", &Dialect::GENERIC), ["select 1"]);
    assert_eq!(
        sqls("select 1;\nselect 2\n", &Dialect::GENERIC),
        ["select 1", "select 2"]
    );
    // Trailing whitespace and comments after the last semicolon are not a
    // statement of their own.
    assert_eq!(
        sqls("select 1;\n\n-- that is all\n", &Dialect::GENERIC),
        ["select 1"]
    );
}

/// Empty fragments never come back as spans.
#[test]
fn empty_fragments() {
    assert!(sqls("", &Dialect::GENERIC).is_empty());
    assert!(sqls(";", &Dialect::GENERIC).is_empty());
    assert!(sqls(" ;; \n ; ", &Dialect::GENERIC).is_empty());
    assert!(sqls("/* only a comment */", &Dialect::GENERIC).is_empty());
    assert_eq!(
        sqls(";select 1;;select 2;;", &Dialect::GENERIC),
        ["select 1", "select 2"]
    );
}

/// The two ranges of a span, and what each is for.
#[test]
fn ranges_and_text() {
    let script = "  select 1 ;  select 2";
    let spans = split_statements(script, &Dialect::GENERIC);
    assert_eq!(spans[0].text(script), "select 1 ;");
    assert_eq!(spans[0].sql(script), "select 1");
    assert_eq!(spans[0].start, 2);
    assert_eq!(spans[0].end, 12);
    assert_eq!(spans[0].sql_end, 10);
    assert_eq!(spans[1].text(script), "select 2");
    assert_eq!(spans[1].sql_end, spans[1].end);
}

/// Where the cursor is, statement by statement.
#[test]
fn statement_at_boundaries() {
    let script = "select 1;\n\nselect 2;\n";
    //            0123456789 10        20
    let at = |offset| {
        statement_at(script, offset, &Dialect::GENERIC)
            .map(|s| s.sql(script))
            .unwrap()
    };

    assert_eq!(at(0), "select 1", "on the first byte");
    assert_eq!(at(4), "select 1", "inside");
    assert_eq!(at(8), "select 1", "on the semicolon");
    assert_eq!(at(9), "select 1", "just after the semicolon");
    assert_eq!(at(10), "select 1", "on the blank line after it");
    assert_eq!(at(11), "select 2", "on the first byte of the next");
    assert_eq!(at(19), "select 2", "on its semicolon");
    assert_eq!(at(21), "select 2", "past the end of the script");
    assert_eq!(at(9999), "select 2", "past the end of everything");
}

/// The cursor before the first statement, and in a script with none.
#[test]
fn statement_at_degenerate_cases() {
    let script = "\n\n  -- a note\n\nselect 1;";
    let found = statement_at(script, 0, &Dialect::GENERIC).unwrap();
    assert_eq!(found.sql(script), "-- a note\n\nselect 1");

    // A cursor in the leading comment gets the statement the comment belongs to.
    assert_eq!(
        statement_at(script, 6, &Dialect::GENERIC).unwrap(),
        found,
        "a comment belongs to the statement under it"
    );

    assert!(statement_at("", 0, &Dialect::GENERIC).is_none());
    assert!(statement_at("  \n-- nothing\n", 5, &Dialect::GENERIC).is_none());
}

/// The cursor rules hold when the semicolons are hidden inside literals.
#[test]
fn statement_at_ignores_hidden_semicolons() {
    let script = "select 'a;b';select 2;";
    let d = &Dialect::GENERIC;
    assert_eq!(
        statement_at(script, 9, d).unwrap().sql(script),
        "select 'a;b'",
        "the cursor is inside the string, not in the second statement"
    );
    assert_eq!(statement_at(script, 14, d).unwrap().sql(script), "select 2");
}

/// [`split_statements`] and [`statement_at`] must not disagree.
#[test]
fn statement_at_agrees_with_the_split() {
    let script = "-- one\nselect 'a;b' from t;\n\n/* two */ update t set x = 1 where y = ';';\n\
                  select $$ ; $$;\nvacuum";
    let d = &Dialect::POSTGRES;
    let spans = split_statements(script, d);
    assert_eq!(spans.len(), 4);
    for span in &spans {
        for offset in [span.start, (span.start + span.end) / 2, span.end] {
            assert_eq!(
                statement_at(script, offset, d).as_ref(),
                Some(span),
                "offset {offset} of {span:?}"
            );
        }
    }
}

/// The limitations are limitations, and this is what they look like — so that
/// the day someone fixes them, the test says so rather than passing quietly.
#[test]
fn known_limitations_are_what_the_documentation_says() {
    // A PL/SQL block splits into pieces. It should be one statement.
    let block = "begin\n  insert into t values (1);\n  commit;\nend;\n/";
    assert_eq!(
        split_statements(block, &Dialect::ORACLE).len(),
        4,
        "PL/SQL blocks are not supported; see the `statement` module docs"
    );

    // `DELIMITER` is not honoured, so the trigger body is cut at the `;` inside
    // it and the `//` that really ends it is read as two operators.
    let delimited = "delimiter //\ncreate trigger t before insert on x for each row\n\
                     begin\n  set @a = 1;\nend//\ndelimiter ;";
    let spans = split_statements(delimited, &Dialect::MYSQL);
    assert_eq!(
        spans.len(),
        2,
        "DELIMITER is not honoured; see the `statement` module docs"
    );
    assert!(
        spans[0].sql(delimited).ends_with("set @a = 1"),
        "the body was cut in half: {:?}",
        spans[0].sql(delimited)
    );
}
