//! Completion over the current statement and a caller-owned metadata snapshot.
//! No database access or SQL execution happens here.
use std::ops::Range;

use crate::{Dialect, Token, TokenKind, lex};

mod functions;

/// A table or view in the connected database's metadata snapshot.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub struct Relation {
    /// Exact JDBC catalog name, when supplied by the driver.
    pub catalog: Option<String>,
    /// Exact schema name, when the database has schemas.
    pub schema: Option<String>,
    /// Unquoted table or view name.
    pub name: String,
    /// `None` means that the caller has not fetched this table's columns yet.
    pub columns: Option<Vec<String>>,
}

/// The suggestion's category, also its display priority.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Kind {
    /// Column of a relation referenced in this statement.
    Column,
    /// Table or view.
    Table,
    /// Schema or catalog qualifier.
    Schema,
    /// Built-in aggregate or scalar function of the connection's dialect.
    Function,
    /// Dialect keyword or type name.
    Keyword,
}

/// One completion row and the text to substitute for its prefix.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Candidate {
    /// Unquoted display name.
    pub label: String,
    /// SQL text with dialect-appropriate identifier quoting.
    pub insert: String,
    /// Owning schema or relation, to distinguish duplicate names.
    pub detail: String,
    /// The row's category.
    pub kind: Kind,
}

/// Lexical context at the caret, scoped to its SQL statement.
#[derive(Clone, Debug)]
pub struct Request {
    /// Whole identifier to replace, including any suffix after the caret.
    pub replace: Range<usize>,
    /// Unquoted prefix typed before the caret.
    pub prefix: String,
    qualifier: Vec<String>,
    table_position: bool,
    references: Vec<(Vec<String>, Option<String>)>,
    quoted: bool,
}

fn is_name(token: &Token) -> bool {
    matches!(
        token.kind,
        TokenKind::Identifier | TokenKind::QuotedIdentifier | TokenKind::Function | TokenKind::Type
    )
}

fn unquote(text: &str) -> String {
    match text.chars().next() {
        Some(open @ ('"' | '`' | '[')) => {
            let close = if open == '[' { ']' } else { open };
            let body = &text[open.len_utf8()..];
            body.strip_suffix(close)
                .unwrap_or(body)
                .replace(&format!("{close}{close}"), &close.to_string())
        }
        _ => text.to_owned(),
    }
}

fn name_chain(tokens: &[Token], source: &str, mut i: usize) -> (Vec<String>, usize) {
    let mut parts = Vec::new();
    while let Some(token) = tokens.get(i).filter(|t| is_name(t)) {
        parts.push(unquote(token.text(source)));
        i += 1;
        if tokens.get(i).is_some_and(|t| t.text(source) == ".")
            && tokens.get(i + 1).is_some_and(is_name)
        {
            i += 1;
        } else {
            break;
        }
    }
    (parts, i)
}

/// Returns no request inside comments, literals, parameters or a selection.
/// Automatic completion requires a word prefix or a dot; explicit completion
/// may also start at an empty insertion point.
pub fn request(
    source: &str,
    selection: Range<usize>,
    dialect: Dialect,
    explicit: bool,
) -> Option<Request> {
    if !selection.is_empty() || !source.is_char_boundary(selection.start) {
        return None;
    }
    let caret = selection.start;
    let tokens = lex(source, &dialect);
    let at = tokens.iter().find(|t| t.start < caret && caret <= t.end);
    if let Some(token) = at
        && matches!(
            token.kind,
            TokenKind::Comment | TokenKind::String | TokenKind::Parameter | TokenKind::Number
        )
    {
        return None;
    }
    let word = tokens.iter().find(|t| {
        t.start <= caret && caret <= t.end && (is_name(t) || t.kind == TokenKind::Keyword)
    });
    let mut replace = word.map_or(caret..caret, |t| t.range());
    let quoted = word.is_some_and(|t| t.kind == TokenKind::QuotedIdentifier);
    if let Some(token) = word.filter(|_| quoted) {
        let text = token.text(source);
        let open = text.chars().next()?;
        let close = if open == '[' { ']' } else { open };
        let trailing = text.chars().rev().take_while(|ch| *ch == close).count();
        if text.len() == 1 || trailing.is_multiple_of(2) {
            // An unfinished quote lexes the rest of the statement as a name.
            // Completing its prefix must preserve SQL after the caret.
            replace.end = caret;
        }
    }
    let prefix = unquote(&source[replace.start..caret]);
    // Do not use names or aliases from neighboring statements.
    let start = tokens
        .iter()
        .filter(|t| t.end <= replace.start && t.text(source) == ";")
        .map(|t| t.end)
        .next_back()
        .unwrap_or(0);
    let end = tokens
        .iter()
        .find(|t| t.start >= caret && t.text(source) == ";")
        .map_or(source.len(), |t| t.start);
    let tokens: Vec<_> = tokens
        .into_iter()
        .filter(|t| t.start >= start && t.end <= end && !t.kind.is_trivia())
        .collect();
    let before: Vec<_> = tokens.iter().filter(|t| t.end <= replace.start).collect();
    let mut qualifier = Vec::new();
    let mut i = before.len();
    while i >= 2 && before[i - 1].text(source) == "." && is_name(before[i - 2]) {
        qualifier.push(unquote(before[i - 2].text(source)));
        i -= 2;
    }
    qualifier.reverse();
    if !explicit && prefix.is_empty() && qualifier.is_empty() {
        return None;
    }
    let relation_word = |word: &str| {
        ["FROM", "JOIN", "UPDATE", "INTO", "TABLE"]
            .iter()
            .any(|key| word.eq_ignore_ascii_case(key))
    };
    let table_position = i > 0 && relation_word(before[i - 1].text(source));
    // Parentheses around expressions do not introduce an alias scope. A
    // subquery does, and sibling subqueries need different identities even
    // when their nesting depth is the same.
    let scopes = query_scopes(&tokens, source);
    let caret_scope = tokens
        .iter()
        .zip(&scopes)
        .rev()
        .find(|(token, _)| token.start < caret)
        .map_or(0, |(_, scope)| *scope);
    let mut references = Vec::new();
    let mut from = vec![false; scopes.iter().copied().max().unwrap_or(0) + 1];
    let mut table_position = table_position;
    let mut pos = 0;
    while pos < tokens.len() {
        let text = tokens[pos].text(source);
        let scope = scopes[pos];
        if [
            "WHERE",
            "GROUP",
            "ORDER",
            "HAVING",
            "SET",
            "VALUES",
            "ON",
            "UNION",
            "RETURNING",
        ]
        .iter()
        .any(|key| text.eq_ignore_ascii_case(key))
        {
            from[scope] = false;
        }
        let starts = relation_word(text) || (text == "," && from[scope]);
        if text.eq_ignore_ascii_case("FROM") {
            from[scope] = true;
        }
        if starts && scope == caret_scope {
            if i > 0 && tokens[pos].start == before[i - 1].start {
                table_position = true;
            }
            let (parts, next) = name_chain(&tokens, source, pos + 1);
            if !parts.is_empty() {
                let alias_at = next
                    + usize::from(
                        tokens
                            .get(next)
                            .is_some_and(|t| t.text(source).eq_ignore_ascii_case("AS")),
                    );
                let alias = tokens
                    .get(alias_at)
                    .filter(|t| is_name(t))
                    .map(|t| unquote(t.text(source)));
                references.push((parts, alias));
                pos = next;
                continue;
            }
        }
        pos += 1;
    }
    Some(Request {
        replace,
        prefix,
        qualifier,
        table_position,
        references,
        quoted,
    })
}

fn query_scopes(tokens: &[Token], source: &str) -> Vec<usize> {
    let mut scope = 0;
    let mut next_scope = 1;
    let mut stack = Vec::new();
    tokens
        .iter()
        .enumerate()
        .map(|(i, token)| {
            let text = token.text(source);
            if text == "(" {
                stack.push(scope);
                if tokens.get(i + 1).is_some_and(|next| {
                    ["SELECT", "WITH"]
                        .iter()
                        .any(|word| next.text(source).eq_ignore_ascii_case(word))
                }) {
                    scope = next_scope;
                    next_scope += 1;
                }
            } else if text == ")" {
                scope = stack.pop().unwrap_or(0);
            } else if ["UNION", "INTERSECT", "EXCEPT"]
                .iter()
                .any(|word| text.eq_ignore_ascii_case(word))
            {
                scope = next_scope;
                next_scope += 1;
            }
            scope
        })
        .collect()
}

fn matches_name(parts: &[String], table: &Relation) -> bool {
    let Some(name) = parts.last() else {
        return false;
    };
    name.eq_ignore_ascii_case(&table.name)
        && (parts.len() < 2
            || table
                .schema
                .as_ref()
                .or(table.catalog.as_ref())
                .is_some_and(|s| s.eq_ignore_ascii_case(&parts[parts.len() - 2])))
        && (parts.len() < 3
            || table
                .catalog
                .as_ref()
                .is_some_and(|s| s.eq_ignore_ascii_case(&parts[parts.len() - 3])))
}

impl Request {
    /// Tables whose columns can contribute to this completion. The application
    /// can fetch just these, and call `candidates` again after they arrive.
    pub fn column_tables(&self, tables: &[Relation]) -> Vec<usize> {
        if self.table_position {
            return Vec::new();
        }
        tables
            .iter()
            .enumerate()
            .filter_map(|(i, table)| {
                let referenced = self.references.iter().any(|(parts, alias)| {
                    matches_name(parts, table)
                        && (self.qualifier.is_empty()
                            || alias.as_ref().is_some_and(|alias| {
                                self.qualifier.len() == 1
                                    && alias.eq_ignore_ascii_case(&self.qualifier[0])
                            })
                            || matches_name(&self.qualifier, table))
                });
                (referenced || (!self.qualifier.is_empty() && matches_name(&self.qualifier, table)))
                    .then_some(i)
            })
            .collect()
    }

    /// Up to 200 matching suggestions, with columns before names and keywords.
    pub fn candidates(&self, dialect: Dialect, tables: &[Relation]) -> Vec<Candidate> {
        let mut result = Vec::new();
        let prefix = self.prefix.to_lowercase();
        let mut add = |label: String, insert: String, detail: String, kind: Kind| {
            if label.to_lowercase().starts_with(&prefix) {
                result.push(Candidate {
                    label,
                    insert,
                    detail,
                    kind,
                });
            }
        };
        for i in self.column_tables(tables) {
            let table = &tables[i];
            for name in table.columns.iter().flatten() {
                add(
                    name.clone(),
                    dialect.quote_ident(name).into_owned(),
                    table
                        .schema
                        .iter()
                        .chain(std::iter::once(&table.name))
                        .cloned()
                        .collect::<Vec<_>>()
                        .join("."),
                    Kind::Column,
                );
            }
        }
        for table in tables {
            let qualified = self.qualifier.len() <= 2
                && self.qualifier.last().is_some_and(|q| {
                    table
                        .schema
                        .as_ref()
                        .or(table.catalog.as_ref())
                        .is_some_and(|s| q.eq_ignore_ascii_case(s))
                })
                && (self.qualifier.len() < 2
                    || table
                        .catalog
                        .as_ref()
                        .is_some_and(|catalog| catalog.eq_ignore_ascii_case(&self.qualifier[0])));
            if self.qualifier.is_empty() || qualified {
                // Without an explicit schema, insert it when present so a name
                // from another schema cannot resolve to the wrong relation.
                let parts: Vec<_> = if qualified {
                    vec![table.name.as_str()]
                } else {
                    table
                        .schema
                        .as_deref()
                        .or(table.catalog.as_deref())
                        .into_iter()
                        .chain(std::iter::once(table.name.as_str()))
                        .collect()
                };
                add(
                    table.name.clone(),
                    dialect.qualify(parts),
                    table
                        .schema
                        .clone()
                        .or(table.catalog.clone())
                        .unwrap_or_default(),
                    Kind::Table,
                );
            }
            if self.qualifier.is_empty()
                && let Some(schema) = table.schema.as_ref().or(table.catalog.as_ref())
            {
                add(
                    schema.clone(),
                    dialect.quote_ident(schema).into_owned(),
                    String::new(),
                    Kind::Schema,
                );
            }
        }
        if self.qualifier.is_empty() && !self.quoted {
            let functions: Vec<_> = if self.table_position {
                Vec::new()
            } else {
                functions::for_dialect(dialect).collect()
            };
            for function in &functions {
                add(
                    (*function).to_owned(),
                    (*function).to_owned(),
                    String::new(),
                    Kind::Function,
                );
            }
            for word in dialect.completion_words() {
                // COALESCE and similar names also occur in the lexer tables.
                // Offer a single function row rather than a duplicate keyword.
                if functions.contains(&word) {
                    continue;
                }
                add(
                    word.to_owned(),
                    word.to_owned(),
                    String::new(),
                    Kind::Keyword,
                );
            }
        }
        result.sort_by(|a, b| {
            (a.kind, a.label.to_lowercase(), &a.detail).cmp(&(
                b.kind,
                b.label.to_lowercase(),
                &b.detail,
            ))
        });
        result.dedup_by(|a, b| a.kind == b.kind && a.insert == b.insert && a.detail == b.detail);
        result.truncate(200);
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn tables() -> Vec<Relation> {
        vec![
            Relation {
                schema: Some("public".into()),
                name: "users".into(),
                columns: Some(vec!["name".into(), "select".into(), "user id".into()]),
                ..Default::default()
            },
            Relation {
                schema: Some("sales".into()),
                name: "orders".into(),
                columns: Some(vec!["total".into()]),
                ..Default::default()
            },
        ]
    }
    fn complete(sql: &str) -> (Request, Vec<Candidate>) {
        let caret = sql.find('|').unwrap();
        let sql = sql.replacen('|', "", 1);
        let request = request(&sql, caret..caret, Dialect::POSTGRES, true).unwrap();
        let candidates = request.candidates(Dialect::POSTGRES, &tables());
        (request, candidates)
    }
    #[test]
    fn keywords_follow_the_dialect_and_replace_the_whole_word() {
        let (request, candidates) = complete("sel|ect");
        assert_eq!(request.replace, 0..6);
        assert!(candidates.iter().any(|c| c.insert == "SELECT"));
        let (request, candidates) = complete("ili|");
        assert!(candidates.iter().any(|c| c.insert == "ILIKE"));
        assert!(
            !request
                .candidates(Dialect::MYSQL, &[])
                .iter()
                .any(|c| c.insert == "ILIKE")
        );
    }
    #[test]
    fn aliases_qualified_names_and_joins_resolve_columns() {
        for sql in [
            "SELECT u.na| FROM public.users AS u",
            "SELECT users.na| FROM users",
            "SELECT public.users.na|",
            "SELECT lower(u.na|) FROM users u",
            "SELECT (SELECT u.na| FROM users u), (SELECT * FROM orders u)",
            "SELECT na| FROM users u JOIN orders o ON u.name = o.total",
        ] {
            let (_, candidates) = complete(sql);
            assert_eq!(candidates[0].insert, "name", "{sql}");
        }
        let (_, candidates) = complete("SELECT o.| FROM users u JOIN sales.orders o ON true");
        assert_eq!(
            candidates
                .iter()
                .map(|c| c.insert.as_str())
                .collect::<Vec<_>>(),
            vec!["total"]
        );
    }
    #[test]
    fn table_and_schema_completion_and_quoting() {
        let (_, candidates) = complete("SELECT * FROM sales.or|");
        assert_eq!(candidates[0].insert, "orders");
        let (_, candidates) = complete("SELECT * FROM us|");
        assert!(candidates.iter().any(|c| c.insert == "public.users"));
        let (_, candidates) = complete("SELECT u.\"user | FROM users u");
        // The unterminated quoted name swallows FROM; use a closed name below.
        assert!(candidates.is_empty());
        let (_, candidates) = complete("SELECT u.\"user |\" FROM users u");
        assert_eq!(candidates[0].insert, "\"user id\"");
        let sql = "SELECT * FROM users u WHERE u.\"na| AND true";
        let (request, candidates) = complete(sql);
        assert_eq!(request.replace.end, sql.find('|').unwrap());
        assert_eq!(candidates[0].insert, "name");
    }
    #[test]
    fn comments_literals_parameters_and_selections_do_not_complete() {
        for sql in [
            "-- sel",
            "/* sel",
            "SELECT 'sel",
            "SELECT $$sel",
            "SELECT $1",
            "SELECT 12",
        ] {
            assert!(
                request(sql, sql.len()..sql.len(), Dialect::POSTGRES, true).is_none(),
                "{sql}"
            );
        }
        assert!(request("select", 0..3, Dialect::POSTGRES, true).is_none());
        assert!(request("SELECT ", 7..7, Dialect::POSTGRES, false).is_none());
    }
    #[test]
    fn statements_and_nested_queries_do_not_leak_aliases() {
        let (_, candidates) = complete("SELECT u.na|; SELECT * FROM users u");
        assert!(candidates.is_empty());
        let (_, candidates) = complete("SELECT u.na| UNION SELECT * FROM users u");
        assert!(candidates.is_empty());
        let (_, candidates) = complete("SELECT u.na| FROM (SELECT * FROM users u) nested");
        assert!(candidates.is_empty());
    }
    #[test]
    fn unicode_prefixes_and_comma_joins_work() {
        let (_, candidates) = complete("SELECT o.to| FROM users u, orders o");
        assert_eq!(candidates[0].insert, "total");
        let sql = "SELECT 고객";
        let request = request(sql, sql.len()..sql.len(), Dialect::POSTGRES, true).unwrap();
        assert_eq!(request.prefix, "고객");
    }

    #[test]
    fn aggregate_functions_complete_on_every_dialect_without_replacing_arguments() {
        for dialect in [
            Dialect::GENERIC,
            Dialect::H2,
            Dialect::POSTGRES,
            Dialect::MYSQL,
            Dialect::MARIADB,
            Dialect::SQLITE,
            Dialect::ORACLE,
            Dialect::MSSQL,
            Dialect::DB2,
        ] {
            for name in ["COUNT", "SUM", "AVG", "MIN", "MAX"] {
                let source = format!("SELECT {}(*)", name.to_lowercase());
                let caret = 7 + name.len() - 1;
                let request = request(&source, caret..caret, dialect, false).unwrap();
                let candidates = request.candidates(dialect, &[]);
                let candidate = candidates
                    .iter()
                    .find(|c| c.label == name && c.kind == Kind::Function)
                    .unwrap_or_else(|| panic!("{} must suggest {name}", dialect.name()));
                let mut completed = source.clone();
                completed.replace_range(request.replace, &candidate.insert);
                assert_eq!(completed, format!("SELECT {name}(*)"));
            }
        }
    }

    #[test]
    fn vendor_functions_and_keyword_duplicates_have_the_right_category() {
        for (dialect, supported, absent) in [
            (Dialect::POSTGRES, "STRING_AGG", "NVL"),
            (Dialect::MYSQL, "GROUP_CONCAT", "STRING_AGG"),
            (Dialect::MARIADB, "GROUP_CONCAT", "STRING_AGG"),
            (Dialect::SQLITE, "IFNULL", "NVL"),
            (Dialect::ORACLE, "NVL", "IFNULL"),
            (Dialect::MSSQL, "ISNULL", "IFNULL"),
            (Dialect::DB2, "POSSTR", "ISNULL"),
            (Dialect::H2, "FORMATDATETIME", "DATE_FORMAT"),
        ] {
            let request = request("SELECT ", 7..7, dialect, true).unwrap();
            let candidates = request.candidates(dialect, &[]);
            assert!(
                candidates
                    .iter()
                    .any(|c| c.label == supported && c.kind == Kind::Function),
                "{}: {supported}",
                dialect.name()
            );
            assert!(
                !candidates
                    .iter()
                    .any(|c| c.label == absent && c.kind == Kind::Function),
                "{}: {absent}",
                dialect.name()
            );
            let coalesce: Vec<_> = candidates
                .iter()
                .filter(|c| c.label == "COALESCE")
                .collect();
            assert_eq!(coalesce.len(), 1);
            assert_eq!(coalesce[0].kind, Kind::Function);
        }
    }

    #[test]
    fn functions_do_not_intrude_on_relation_or_quoted_name_completion() {
        for source in ["SELECT * FROM co", "SELECT users.co", "SELECT \"co"] {
            let request =
                request(source, source.len()..source.len(), Dialect::POSTGRES, true).unwrap();
            assert!(
                !request
                    .candidates(Dialect::POSTGRES, &tables())
                    .iter()
                    .any(|c| c.kind == Kind::Function),
                "{source}"
            );
        }
    }
}
