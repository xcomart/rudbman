//! Conservative statement access classification for safety gates.

use crate::{Dialect, Token, TokenKind, lex, split_statements};

/// A syntactic access classification used by confirmation and re-run gates.
///
/// This does not replace database permissions: a read-shaped statement may
/// still call a vendor-specific function with side effects.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StatementAccess {
    /// A recognised query shape with no syntactically modifying CTE.
    Read,
    /// A recognised modifying statement.
    Write,
    /// Empty, malformed, or dialect-specific syntax the classifier cannot place.
    Unknown,
}

/// Classifies one SQL statement. Unknown input is deliberately not called a read.
pub fn statement_access(sql: &str, dialect: &Dialect) -> StatementAccess {
    let all = lex(sql, dialect);
    if all.iter().any(|token| {
        token.kind == TokenKind::Comment
            && (token.text(sql).starts_with("/*!") || token.text(sql).starts_with("/*M!"))
    }) {
        return StatementAccess::Unknown;
    }
    if split_statements(sql, dialect).len() != 1 {
        return StatementAccess::Unknown;
    }
    let tokens: Vec<_> = all
        .into_iter()
        .filter(|token| !token.kind.is_trivia())
        .collect();
    classify(sql, &tokens, 0)
}

fn classify(sql: &str, tokens: &[Token], nesting: usize) -> StatementAccess {
    if nesting > 64 {
        return StatementAccess::Unknown;
    }
    let Some(first) = tokens.first() else {
        return StatementAccess::Unknown;
    };
    let word = first.text(sql);
    if word.eq_ignore_ascii_case("SELECT") {
        if !balanced(tokens, sql) || tokens.iter().any(|token| token.kind == TokenKind::Error) {
            return StatementAccess::Unknown;
        }
        return if contains_at_depth(sql, &tokens[1..], "INTO", 0) {
            StatementAccess::Write
        } else {
            StatementAccess::Read
        };
    }
    if word.eq_ignore_ascii_case("SHOW") || word.eq_ignore_ascii_case("DESCRIBE") {
        return StatementAccess::Read;
    }
    if word.eq_ignore_ascii_case("EXPLAIN") {
        let Some(index) = tokens
            .iter()
            .enumerate()
            .skip(1)
            .find_map(|(index, token)| {
                matches!(token.kind, TokenKind::Keyword | TokenKind::Identifier)
                    .then(|| token.text(sql))
                    .filter(|word| is_statement_word(word))
                    .map(|_| index)
            })
        else {
            return StatementAccess::Unknown;
        };
        return classify(sql, &tokens[index..], nesting + 1);
    }
    if word.eq_ignore_ascii_case("WITH") {
        return classify_with(sql, tokens, nesting);
    }
    if is_write_word(word) {
        StatementAccess::Write
    } else {
        StatementAccess::Unknown
    }
}

fn classify_with(sql: &str, tokens: &[Token], nesting: usize) -> StatementAccess {
    let mut i = 1;
    if word_at(sql, tokens, i, "RECURSIVE") {
        i += 1;
    }
    loop {
        if !matches!(
            tokens.get(i).map(|t| t.kind),
            Some(
                TokenKind::Identifier
                    | TokenKind::QuotedIdentifier
                    | TokenKind::Keyword
                    | TokenKind::Function
            )
        ) {
            return StatementAccess::Unknown;
        }
        i += 1;
        if punct_at(sql, tokens, i, "(") {
            let Some(next) = after_balanced(sql, tokens, i) else {
                return StatementAccess::Unknown;
            };
            i = next;
        }
        if !word_at(sql, tokens, i, "AS") {
            return StatementAccess::Unknown;
        }
        i += 1;
        if word_at(sql, tokens, i, "NOT") {
            i += 1;
        }
        if word_at(sql, tokens, i, "MATERIALIZED") {
            i += 1;
        }
        if !punct_at(sql, tokens, i, "(") {
            return StatementAccess::Unknown;
        }
        let Some(end) = matching_paren(sql, tokens, i) else {
            return StatementAccess::Unknown;
        };
        match classify(sql, &tokens[i + 1..end], nesting + 1) {
            StatementAccess::Read => {}
            other => return other,
        }
        i = end + 1;
        if punct_at(sql, tokens, i, ",") {
            i += 1;
            continue;
        }
        return classify(sql, &tokens[i..], nesting + 1);
    }
}

fn is_statement_word(word: &str) -> bool {
    word.eq_ignore_ascii_case("SELECT")
        || word.eq_ignore_ascii_case("WITH")
        || word.eq_ignore_ascii_case("SHOW")
        || word.eq_ignore_ascii_case("DESCRIBE")
        || is_write_word(word)
}

fn is_write_word(word: &str) -> bool {
    [
        "INSERT", "UPDATE", "DELETE", "MERGE", "CREATE", "ALTER", "DROP", "TRUNCATE", "CALL",
        "EXEC", "EXECUTE", "GRANT", "REVOKE", "REPLACE", "UPSERT", "COPY",
    ]
    .iter()
    .any(|candidate| word.eq_ignore_ascii_case(candidate))
}

fn contains_at_depth(sql: &str, tokens: &[Token], needle: &str, wanted: usize) -> bool {
    let mut depth: usize = 0;
    for token in tokens {
        match token.text(sql) {
            "(" => depth += 1,
            ")" => depth = depth.saturating_sub(1),
            text if depth == wanted && text.eq_ignore_ascii_case(needle) => return true,
            _ => {}
        }
    }
    false
}

fn matching_paren(sql: &str, tokens: &[Token], start: usize) -> Option<usize> {
    let mut depth = 0;
    for (i, token) in tokens.iter().enumerate().skip(start) {
        match token.text(sql) {
            "(" => depth += 1,
            ")" => {
                depth -= 1;
                if depth == 0 {
                    return Some(i);
                }
            }
            _ => {}
        }
    }
    None
}

fn balanced(tokens: &[Token], sql: &str) -> bool {
    let mut depth = 0usize;
    for token in tokens {
        match token.text(sql) {
            "(" => depth += 1,
            ")" if depth == 0 => return false,
            ")" => depth -= 1,
            _ => {}
        }
    }
    depth == 0
}

fn after_balanced(sql: &str, tokens: &[Token], start: usize) -> Option<usize> {
    matching_paren(sql, tokens, start).map(|end| end + 1)
}

fn word_at(sql: &str, tokens: &[Token], i: usize, word: &str) -> bool {
    tokens
        .get(i)
        .is_some_and(|token| token.text(sql).eq_ignore_ascii_case(word))
}

fn punct_at(sql: &str, tokens: &[Token], i: usize, punct: &str) -> bool {
    tokens.get(i).is_some_and(|token| token.text(sql) == punct)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn protects_structural_write_bypasses() {
        let pg = Dialect::POSTGRES;
        for sql in [
            "WITH x AS (DELETE FROM t RETURNING id) SELECT * FROM x",
            "WITH x AS (WITH y AS (UPDATE t SET v=1 RETURNING *) SELECT * FROM y) SELECT * FROM x",
            "WITH RECURSIVE x AS MATERIALIZED (SELECT 1) DELETE FROM t",
            "EXPLAIN ANALYZE DELETE FROM t",
            "SELECT * INTO archive FROM live",
        ] {
            assert_eq!(statement_access(sql, &pg), StatementAccess::Write, "{sql}");
        }
    }

    #[test]
    fn preserves_harmless_reads_and_ignores_literals() {
        for dialect in [
            Dialect::POSTGRES,
            Dialect::MYSQL,
            Dialect::MSSQL,
            Dialect::H2,
        ] {
            for sql in [
                "SELECT 'into delete' AS note",
                "WITH x AS (SELECT 'update' AS v), y AS NOT MATERIALIZED (SELECT * FROM x) SELECT * FROM y",
                "WITH x(id) AS (SELECT 1) SELECT * FROM x",
                "EXPLAIN SELECT * FROM t",
                "SHOW TABLES",
                "DESCRIBE t",
            ] {
                assert_eq!(
                    statement_access(sql, &dialect),
                    StatementAccess::Read,
                    "{sql}"
                );
            }
        }
    }

    #[test]
    fn malformed_and_unrecognised_are_unknown() {
        let h2 = Dialect::H2;
        for sql in [
            "",
            "-- comment",
            "WITH x AS (SELECT 1 SELECT * FROM x",
            "EXPLAIN",
            "BEGIN",
            "SELECT (1",
            "SELECT 1; DELETE FROM t",
            "/*! DELETE FROM t */",
            "/*M! DELETE FROM t */",
        ] {
            assert_eq!(
                statement_access(sql, &h2),
                StatementAccess::Unknown,
                "{sql}"
            );
        }
    }

    #[test]
    fn excessive_cte_nesting_is_unknown() {
        let mut sql = "SELECT 1".to_string();
        for _ in 0..70 {
            sql = format!("WITH x AS ({sql}) SELECT * FROM x");
        }
        assert_eq!(
            statement_access(&sql, &Dialect::POSTGRES),
            StatementAccess::Unknown
        );
    }
}
