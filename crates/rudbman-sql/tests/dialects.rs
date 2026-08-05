//! One test per place the vendors disagree.
//!
//! The unit tests inside the crate cover the scanner's shape; these cover the
//! table in `dialect.rs`, dialect by dialect, so that a wrong `bool` in it fails
//! by name.

use rudbman_sql::{Dialect, LineState, TokenKind, lex, lex_line};

/// The `(kind, text)` pairs of `source`, whitespace dropped.
fn kinds(source: &str, dialect: &Dialect) -> Vec<(TokenKind, String)> {
    lex(source, dialect)
        .into_iter()
        .filter(|t| t.kind != TokenKind::Whitespace)
        .map(|t| (t.kind, t.text(source).to_string()))
        .collect()
}

/// The kinds only.
fn only_kinds(source: &str, dialect: &Dialect) -> Vec<TokenKind> {
    kinds(source, dialect).into_iter().map(|(k, _)| k).collect()
}

/// MySQL needs a space after `--`; nobody else does.
///
/// `select 1--2` is `1 - (-2)` in MySQL and a comment everywhere else, and the
/// two readings disagree about where the line ends.
#[test]
fn mysql_dash_dash_needs_a_space() {
    assert_eq!(
        only_kinds("select 1--2", &Dialect::MYSQL),
        [
            TokenKind::Keyword,
            TokenKind::Number,
            TokenKind::Operator, // `-`
            TokenKind::Operator, // `-`
            TokenKind::Number,
        ]
    );
    assert_eq!(
        only_kinds("select 1--2", &Dialect::POSTGRES),
        [TokenKind::Keyword, TokenKind::Number, TokenKind::Comment]
    );
    // With the space, MySQL agrees.
    assert_eq!(
        only_kinds("select 1-- 2", &Dialect::MYSQL),
        [TokenKind::Keyword, TokenKind::Number, TokenKind::Comment]
    );
    // And at the end of a line, where there is nothing to be a space.
    let (tokens, state) = lex_line("select 1--", LineState::START, &Dialect::MYSQL);
    assert_eq!(tokens.last().unwrap().kind, TokenKind::Comment);
    assert!(state.is_start());
}

/// `#` is a comment in MySQL, an operator in PostgreSQL, a name in SQL Server.
#[test]
fn hash_means_three_different_things() {
    assert_eq!(
        only_kinds("select 1 # note", &Dialect::MYSQL),
        [TokenKind::Keyword, TokenKind::Number, TokenKind::Comment]
    );
    assert_eq!(
        kinds("a #> b", &Dialect::POSTGRES)[1],
        (TokenKind::Operator, "#>".into())
    );
    assert_eq!(
        kinds("select * from #tmp", &Dialect::MSSQL).last().unwrap(),
        &(TokenKind::Identifier, "#tmp".into())
    );
}

/// The three identifier-quoting forms, each only where it belongs.
#[test]
fn identifier_quoting() {
    // Backticks: MySQL yes, PostgreSQL no.
    assert_eq!(
        kinds("`order`", &Dialect::MYSQL)[0],
        (TokenKind::QuotedIdentifier, "`order`".into())
    );
    assert_eq!(only_kinds("`x`", &Dialect::POSTGRES)[0], TokenKind::Error);

    // Brackets: SQL Server yes, and `]]` is an escaped `]`.
    assert_eq!(
        kinds("[my table]", &Dialect::MSSQL)[0],
        (TokenKind::QuotedIdentifier, "[my table]".into())
    );
    assert_eq!(
        kinds("[a]]b]", &Dialect::MSSQL)[0],
        (TokenKind::QuotedIdentifier, "[a]]b]".into())
    );
    // Elsewhere a bracket is punctuation — PostgreSQL's array subscript.
    assert_eq!(
        only_kinds("a[1]", &Dialect::POSTGRES),
        [
            TokenKind::Identifier,
            TokenKind::Punctuation,
            TokenKind::Number,
            TokenKind::Punctuation,
        ]
    );

    // Double quotes: an identifier everywhere except MySQL, where they are a
    // string.
    assert_eq!(
        kinds("\"users\"", &Dialect::ORACLE)[0],
        (TokenKind::QuotedIdentifier, "\"users\"".into())
    );
    assert_eq!(
        kinds("\"users\"", &Dialect::MYSQL)[0],
        (TokenKind::String, "\"users\"".into())
    );
}

/// PostgreSQL's dollar quotes, including the several-line case that
/// [`LineState`] exists for.
#[test]
fn postgres_dollar_quotes() {
    let sql = "$$ a ; b $$";
    assert_eq!(
        kinds(sql, &Dialect::POSTGRES)[0],
        (TokenKind::String, sql.into())
    );

    // A tagged quote is closed only by its own tag.
    let sql = "$fn$ select $$ inner $$ ; $fn$";
    assert_eq!(
        kinds(sql, &Dialect::POSTGRES)[0],
        (TokenKind::String, sql.into())
    );

    // A digit cannot start a tag, so `$1` stays a parameter.
    assert_eq!(
        kinds("$1", &Dialect::POSTGRES)[0],
        (TokenKind::Parameter, "$1".into())
    );

    // And without the dialect there are no dollar quotes at all.
    assert_ne!(
        only_kinds("$$ x $$", &Dialect::ORACLE)[0],
        TokenKind::String
    );
}

/// A dollar quote crossing lines, carried by the state and closed by its tag.
#[test]
fn dollar_quote_spans_lines() {
    let pg = &Dialect::POSTGRES;
    let mut state = LineState::START;
    let lines = [
        "create function f() returns int as $body$",
        "  begin",
        "    return 1; -- not a statement boundary",
        "  end",
        "$body$ language plpgsql;",
    ];
    let mut open_lines = 0;
    for line in lines {
        let (tokens, next) = lex_line(line, state, pg);
        if !state.is_start() {
            assert_eq!(tokens[0].kind, TokenKind::String, "on {line:?}");
            open_lines += 1;
        }
        state = next;
    }
    assert_eq!(open_lines, 4);
    assert!(state.is_start(), "the quote closed on the last line");

    // A tag that is not ours does not close it.
    let (_, state) = lex_line("select $a$ x", LineState::START, pg);
    let (_, state) = lex_line("$b$ still inside", state, pg);
    assert!(!state.is_start());
    let (_, state) = lex_line("$a$ out", state, pg);
    assert!(state.is_start());
}

/// Only PostgreSQL and SQL Server nest block comments.
#[test]
fn nested_block_comments() {
    let sql = "/* a /* b */ c */ select";
    assert_eq!(
        only_kinds(sql, &Dialect::POSTGRES),
        [TokenKind::Comment, TokenKind::Keyword]
    );
    assert_eq!(
        only_kinds(sql, &Dialect::POSTGRES),
        only_kinds(sql, &Dialect::MSSQL)
    );
    // Oracle stops at the first `*/`, so `c */` is code.
    assert_eq!(
        only_kinds(sql, &Dialect::ORACLE),
        [
            TokenKind::Comment,
            TokenKind::Identifier, // c
            TokenKind::Operator,   // *
            TokenKind::Operator,   // /
            TokenKind::Keyword,    // select
        ]
    );
}

/// Hex literals everywhere but Oracle.
#[test]
fn hex_literals() {
    for d in [
        Dialect::MYSQL,
        Dialect::H2,
        Dialect::MSSQL,
        Dialect::SQLITE,
        Dialect::POSTGRES,
        Dialect::GENERIC,
    ] {
        assert_eq!(
            kinds("0xdeadBEEF", &d)[0],
            (TokenKind::Number, "0xdeadBEEF".into()),
            "{}",
            d.name()
        );
    }
    assert_eq!(kinds("0xff", &Dialect::ORACLE).len(), 2);
}

/// Every dialect's bind-parameter spellings, and only its own.
#[test]
fn bind_parameters() {
    // `?` is universal: it is what JDBC speaks.
    for d in [
        Dialect::GENERIC,
        Dialect::H2,
        Dialect::POSTGRES,
        Dialect::MYSQL,
        Dialect::SQLITE,
        Dialect::ORACLE,
        Dialect::MSSQL,
    ] {
        assert_eq!(
            kinds("where a = ?", &d).last().unwrap(),
            &(TokenKind::Parameter, "?".into()),
            "{}",
            d.name()
        );
    }

    assert_eq!(
        kinds(":since", &Dialect::ORACLE)[0],
        (TokenKind::Parameter, ":since".into())
    );
    assert_eq!(
        kinds("@since", &Dialect::MSSQL)[0],
        (TokenKind::Parameter, "@since".into())
    );
    // MySQL user variables are the same spelling.
    assert_eq!(
        kinds("set @x = 1", &Dialect::MYSQL)[1],
        (TokenKind::Parameter, "@x".into())
    );
    // Oracle has no `@name` parameters: `@` there introduces a database link.
    assert_ne!(only_kinds("@x", &Dialect::ORACLE)[0], TokenKind::Parameter);
}

/// The keyword tables are wired to the dialect and do not leak across.
#[test]
fn dialect_specific_keywords() {
    let cases: &[(Dialect, &str)] = &[
        (Dialect::POSTGRES, "ilike"),
        (Dialect::POSTGRES, "lateral"),
        (Dialect::MYSQL, "straight_join"),
        (Dialect::ORACLE, "rownum"),
        (Dialect::ORACLE, "connect"),
        (Dialect::MSSQL, "top"),
        (Dialect::MSSQL, "nolock"),
        (Dialect::SQLITE, "autoincrement"),
        (Dialect::H2, "qualify"),
    ];
    for (dialect, word) in cases {
        assert_eq!(
            only_kinds(word, dialect)[0],
            TokenKind::Keyword,
            "{word} in {}",
            dialect.name()
        );
        assert_eq!(
            only_kinds(word, &Dialect::GENERIC)[0],
            TokenKind::Identifier,
            "{word} is not generic SQL"
        );
    }
}

/// Type names get their own class, whatever the dialect calls them.
#[test]
fn dialect_specific_types() {
    let cases: &[(Dialect, &str)] = &[
        (Dialect::ORACLE, "number"),
        (Dialect::ORACLE, "varchar2"),
        (Dialect::POSTGRES, "jsonb"),
        (Dialect::MYSQL, "mediumtext"),
        (Dialect::MSSQL, "uniqueidentifier"),
        (Dialect::H2, "java_object"),
        (Dialect::GENERIC, "varchar"),
    ];
    for (dialect, word) in cases {
        assert_eq!(
            only_kinds(word, dialect)[0],
            TokenKind::Type,
            "{word} in {}",
            dialect.name()
        );
    }
}

/// A statement that touches most of the forks at once, per dialect, only to
/// check that nothing panics and the tokens still tile the input.
#[test]
fn nothing_panics_on_anything() {
    let soup = "select 'a''b', \"c\", `d`, [e], $$f$$, E'g\\'', 0x1f, ?, $1, :p, @v, a->>'b', \
                #x, -- c\n/* /* n */ */ 1.5e-3, ±";
    for d in [
        Dialect::GENERIC,
        Dialect::H2,
        Dialect::POSTGRES,
        Dialect::MYSQL,
        Dialect::SQLITE,
        Dialect::ORACLE,
        Dialect::MSSQL,
    ] {
        let tokens = lex(soup, &d);
        let mut at = 0;
        for t in &tokens {
            assert_eq!(t.start, at, "{}", d.name());
            assert!(t.end > t.start);
            at = t.end;
        }
        assert_eq!(at, soup.len(), "{}", d.name());
    }
}
