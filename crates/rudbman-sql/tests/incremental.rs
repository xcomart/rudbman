//! The consistency proof for incremental lexing.
//!
//! The editor never lexes a whole buffer; it lexes the line it changed and walks
//! down until a line's end state stops changing. That is only sound if lexing a
//! source line by line classifies every byte exactly as lexing it in one call
//! does — otherwise the buffer looks different depending on where the last edit
//! happened, which is the class of bug that shows up as "the colors are wrong
//! until I retype the line".
//!
//! The comparison is per byte rather than per token on purpose: a multi-line
//! construct is one token to [`lex`] and one token per line to [`lex_line`], and
//! that difference is by design. What must not differ is what any given byte
//! *is*.

use rudbman_sql::{Dialect, LineState, TokenKind, lex, lex_line};

/// The kind of every byte of `source`, lexed in one call.
fn whole(source: &str, dialect: &Dialect) -> Vec<TokenKind> {
    let mut map = Vec::with_capacity(source.len());
    for token in lex(source, dialect) {
        map.resize(token.end, token.kind);
    }
    assert_eq!(map.len(), source.len(), "tokens must tile the source");
    map
}

/// The kind of every byte of `source`, lexed one line at a time and stitched
/// back together — exactly what an editor holds.
fn by_line(source: &str, dialect: &Dialect) -> (Vec<TokenKind>, Vec<LineState>) {
    let mut map = Vec::with_capacity(source.len());
    let mut states = Vec::new();
    let mut state = LineState::START;
    let mut base = 0;
    for line in source.split_inclusive('\n') {
        // The editor holds lines without their terminator, so the newline is
        // never handed to the lexer at all — which is the strictest form of the
        // question, since its class has to be recovered from the end state
        // alone.
        let (text, newline) = match line.strip_suffix('\n') {
            Some(text) => (text, true),
            None => (line, false),
        };
        let (tokens, next) = lex_line(text, state, dialect);
        for token in tokens {
            map.resize(base + token.end, token.kind);
        }
        assert_eq!(
            map.len(),
            base + text.len(),
            "line tokens must tile the line"
        );
        base += text.len();
        if newline {
            // The newline itself is never handed to `lex_line`, so its class is
            // whatever the line ended inside: part of the block comment or the
            // string that crosses it, and whitespace when nothing is open. This
            // is the same thing an editor concludes from the end state.
            map.push(next.carried_kind(dialect).unwrap_or(TokenKind::Whitespace));
            base += 1;
        }
        state = next;
        states.push(state);
    }
    (map, states)
}

/// Assert the two agree, and say where they do not.
fn assert_agrees(source: &str, dialect: &Dialect) {
    let whole = whole(source, dialect);
    let (lines, _) = by_line(source, dialect);
    assert_eq!(whole.len(), lines.len());
    for (i, (a, b)) in whole.iter().zip(&lines).enumerate() {
        assert_eq!(
            a,
            b,
            "byte {i} ({:?}) in {}: whole-buffer says {a:?}, line-by-line says {b:?}\n{source}",
            &source[i..source.len().min(i + 8)],
            dialect.name()
        );
    }
}

/// A newline inside a construct is the case the whole design is for.
#[test]
fn multi_line_constructs_agree() {
    let cases: &[(Dialect, &str)] = &[
        (
            Dialect::POSTGRES,
            "select 1;\n/* a comment\n   that runs ; over\n   three lines */\nselect 2;\n",
        ),
        (
            Dialect::POSTGRES,
            "/* outer /* inner\n still inner */ still outer */ select 1;\n",
        ),
        (
            Dialect::POSTGRES,
            "create function f() returns int as $body$\nbegin\n  return 1;\nend\n$body$;\n",
        ),
        (
            Dialect::POSTGRES,
            "select $$ a $$, $tag$ b\nc $tag$, $$ d\ne $$;\n",
        ),
        (
            Dialect::GENERIC,
            "insert into t values ('a line\nand another ; line\nand a third');\n",
        ),
        (
            Dialect::ORACLE,
            "select \"a very\nlong quoted\nname\" from dual;\n",
        ),
        (Dialect::MYSQL, "select `a\nb`, \"c\nd\", 'e\\\nf';\n"),
        (Dialect::MSSQL, "select [a\nb] from [c\nd];\n"),
        (Dialect::MYSQL, "select 1 # comment\n, 2 -- comment\n, 3;\n"),
        (
            Dialect::POSTGRES,
            "select 1 -- trailing comment, no newline",
        ),
        (Dialect::POSTGRES, "/* unterminated\nblock comment"),
        (Dialect::POSTGRES, "select $unclosed$ body\nmore body"),
        (Dialect::GENERIC, "select 'unclosed\nstring"),
        (Dialect::GENERIC, ""),
        (Dialect::GENERIC, "\n\n\n"),
    ];
    for (dialect, source) in cases {
        assert_agrees(source, dialect);
    }
}

/// The same script through every dialect: a rule that crosses a line in one of
/// them must not desynchronize the other.
#[test]
fn every_dialect_agrees_with_itself() {
    let script = "-- header\nselect a, 'b;c', \"d\", `e`, [f], $g$h\ni$g$, 0x1f, ?, :p, @v\n\
                  from t /* note\n   note */ where x -- tail\n;\n# hash\nselect 2";
    for dialect in [
        Dialect::GENERIC,
        Dialect::H2,
        Dialect::POSTGRES,
        Dialect::MYSQL,
        Dialect::SQLITE,
        Dialect::ORACLE,
        Dialect::MSSQL,
    ] {
        assert_agrees(script, &dialect);
    }
}

/// The re-lex loop an editor actually runs: change a line, walk down, stop when
/// the end state matches what was cached.
#[test]
fn a_stable_line_state_stops_the_relex() {
    let pg = &Dialect::POSTGRES;
    let before = "select 1;\nselect 2;\nselect 3;\nselect 4;\n";
    let (_, states) = by_line(before, pg);
    assert!(states.iter().all(|s| s.is_start()));

    // An edit that opens nothing: the very first line re-lexed already has the
    // cached end state, so the walk stops there.
    let after = "select 11;\nselect 2;\nselect 3;\nselect 4;\n";
    let (_, states_after) = by_line(after, pg);
    assert_eq!(states[0], states_after[0]);

    // An edit that opens a block comment: every line below changes state until
    // something closes it, which here is nothing.
    let opened = "select 1; /*\nselect 2;\nselect 3;\nselect 4;\n";
    let (_, states_opened) = by_line(opened, pg);
    assert!(states_opened.iter().all(|s| !s.is_start()));
    assert_ne!(states[0], states_opened[0]);
}

/// Two different dollar tags must not be confused for one another, since that
/// is what the hashed [`LineState`] representation is trusted not to do.
#[test]
fn dollar_tags_are_distinguished_by_state() {
    let pg = &Dialect::POSTGRES;
    let (_, a) = lex_line("$a$ x", LineState::START, pg);
    let (_, b) = lex_line("$b$ x", LineState::START, pg);
    let (_, long) = lex_line("$a_very_long_tag_indeed$ x", LineState::START, pg);
    assert_ne!(a, b);
    assert_ne!(a, long);
    assert_ne!(b, long);
    // Same tag, same state: this is what lets the walk terminate.
    let (_, a2) = lex_line("$a$ y", LineState::START, pg);
    assert_eq!(a, a2);
}

/// Block comment nesting depth is part of the state, not just "in a comment".
#[test]
fn nesting_depth_is_part_of_the_state() {
    let pg = &Dialect::POSTGRES;
    let (_, one) = lex_line("/* a", LineState::START, pg);
    let (_, two) = lex_line("/* a /* b", LineState::START, pg);
    assert_ne!(one, two);
    let (_, back) = lex_line("b */", two, pg);
    assert_eq!(
        back, one,
        "closing one level returns to the shallower state"
    );
}
