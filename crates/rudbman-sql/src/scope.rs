//! Which schema a script's statements name, and moving them to another.
//!
//! A schema backup names every object it creates in full — `"APP"."PARENT"`,
//! `"shop"."dbo"."orders"`, MySQL's `USE shop` and `` `shop`.`orders` `` — and
//! so do `pg_dump`, SSMS and SQL Developer. Run against another schema, such a
//! script still creates its objects where it came from. [`statement_scope`]
//! reads where a statement creates or fills something, so a restore can tell
//! which scope a script belongs to; [`move_scope`] rewrites the names of that
//! scope into another one.
//!
//! # What is moved
//!
//! A name is moved where it names the scope, and only there:
//!
//! * the qualifier of a qualified name: `APP.T`, `"APP"."T"."C"`,
//!   `[shop].[dbo].[t]`, `` `shop`.`t` `` (on MySQL the qualifier is the
//!   database, elsewhere a schema, and a catalog when the name has three
//!   parts or more);
//! * the name after `USE`, `DATABASE`, `SCHEMA` and `CURRENT_SCHEMA =`, and
//!   in a `search_path`;
//! * the strings that hold a name for a function to look up: the first
//!   argument of `nextval`, `setval`, `currval`, `pg_get_serial_sequence`,
//!   `OBJECT_ID`, `DBCC CHECKIDENT` and `DBMS_UTILITY.COMPILE_SCHEMA`, a
//!   string cast to `::regclass`, and SQL Server's `@sequence_name` and
//!   `@level0name` arguments;
//! * all of the above inside a PostgreSQL dollar-quoted body and a MySQL
//!   executable comment, which hold code.
//!
//! Every other string is left alone: a row whose text happens to read
//! `APP.T` is data. Names are compared the way the product compares them —
//! an unquoted name folded to its stored case, a quoted one as written, and
//! without regard to case where the product stores names that way — and a
//! moved name is written in the quoting it had: quoted if it was, bare if the
//! new name can be.
//!
//! ```
//! use rudbman_sql::{Dialect, ScopeMove, move_scope};
//!
//! let moving = ScopeMove::schema("APP", "ARCHIVE");
//! assert_eq!(
//!     move_scope("INSERT INTO \"APP\".\"T\" VALUES ('APP.T')", &Dialect::H2, &moving),
//!     "INSERT INTO \"ARCHIVE\".\"T\" VALUES ('APP.T')"
//! );
//! ```

use std::borrow::Cow;
use std::ops::Range;

use crate::dialect::{Dialect, DialectId};
use crate::lexer::{Lexer, Token, TokenKind};

/// Where a statement puts something: the catalog and the schema its name is
/// qualified with, in the spelling the catalog stores them in.
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash)]
pub struct ScopeName {
    /// The catalog — the database, on MySQL, MariaDB and SQL Server.
    pub catalog: Option<String>,
    /// The schema.
    pub schema: Option<String>,
}

/// Which scope names to rewrite into which.
///
/// Each level holds the name the script uses and the name to write instead;
/// a level whose two names are the same still identifies the script's scope,
/// which is how a three-part name is recognized when only its schema moves.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ScopeMove {
    /// The catalog, from and to.
    pub catalog: Option<(String, String)>,
    /// The schema, from and to.
    pub schema: Option<(String, String)>,
}

impl ScopeMove {
    /// Moves the schema `from` to `to`.
    pub fn schema(from: impl Into<String>, to: impl Into<String>) -> Self {
        Self {
            catalog: None,
            schema: Some((from.into(), to.into())),
        }
    }

    /// Moves the catalog `from` to `to`.
    pub fn catalog(from: impl Into<String>, to: impl Into<String>) -> Self {
        Self {
            catalog: Some((from.into(), to.into())),
            schema: None,
        }
    }

    /// Whether anything changes name under `dialect`'s comparison.
    pub fn moves(&self, dialect: &Dialect) -> bool {
        [&self.catalog, &self.schema]
            .into_iter()
            .flatten()
            .any(|(from, to)| !same_name(from, to, dialect))
    }
}

/// Whether two stored names are the same name to `dialect`.
pub fn same_name(a: &str, b: &str, dialect: &Dialect) -> bool {
    if case_insensitive(dialect) {
        a.eq_ignore_ascii_case(b)
    } else {
        a == b
    }
}

/// Where `sql` creates or fills something, as far as its name says: the
/// qualifier of the name after `CREATE … TABLE`, `VIEW`, `PROCEDURE` and the
/// other kinds, `INSERT INTO`, `ALTER TABLE` and `ALTER SEQUENCE`, the name
/// after `USE` and `CREATE SCHEMA` or `DATABASE`. `None` for a statement that
/// names nothing, or names it unqualified.
pub fn statement_scope(sql: &str, dialect: &Dialect) -> Option<ScopeName> {
    if let Some(opened) = opened_comments(sql, dialect) {
        return statement_scope(&opened, dialect);
    }
    let tokens = code_tokens(sql, dialect);
    let word = |i: usize| -> String {
        tokens
            .get(i)
            .filter(|token| is_word(token.kind))
            .map(|token| token.text(sql).to_ascii_uppercase())
            .unwrap_or_default()
    };
    let catalog_first = catalog_first(dialect);
    let mut at = match word(0).as_str() {
        "USE" if uses_catalogs(dialect) => {
            return single(sql, &tokens, 1, dialect).map(|name| ScopeName {
                catalog: Some(name),
                schema: None,
            });
        }
        "INSERT" if word(1) == "INTO" => 2,
        "ALTER" if matches!(word(1).as_str(), "TABLE" | "SEQUENCE") => {
            let mut at = 2;
            while matches!(word(at).as_str(), "ONLY" | "IF" | "EXISTS") {
                at += 1;
            }
            at
        }
        "CREATE" => {
            let kind = (1..12).find(|&i| {
                matches!(
                    word(i).as_str(),
                    "TABLE"
                        | "VIEW"
                        | "SEQUENCE"
                        | "FUNCTION"
                        | "PROCEDURE"
                        | "PROC"
                        | "TRIGGER"
                        | "PACKAGE"
                        | "TYPE"
                        | "INDEX"
                        | "DOMAIN"
                        | "ALIAS"
                        | "SYNONYM"
                        | "EVENT"
                        | "SCHEMA"
                        | "DATABASE"
                )
            })?;
            let mut at = kind + 1;
            while matches!(word(at).as_str(), "BODY" | "IF" | "NOT" | "EXISTS")
                || tokens
                    .get(at)
                    .is_some_and(|token| token.kind == TokenKind::Comment)
            {
                at += 1;
            }
            if matches!(word(kind).as_str(), "SCHEMA" | "DATABASE") {
                let name = single(sql, &tokens, at, dialect)?;
                return Some(if catalog_first {
                    ScopeName {
                        catalog: Some(name),
                        schema: None,
                    }
                } else {
                    ScopeName {
                        catalog: None,
                        schema: Some(name),
                    }
                });
            }
            at
        }
        _ => return None,
    };
    let chain = chain_at(sql, &tokens, &mut at)?;
    let names: Vec<String> = chain
        .iter()
        .map(|&i| stored(tokens[i].text(sql), tokens[i].kind, dialect))
        .collect();
    match (catalog_first, names.len()) {
        (_, 0 | 1) => None,
        (true, _) => Some(ScopeName {
            catalog: Some(names[0].clone()),
            schema: None,
        }),
        (false, 2) => Some(ScopeName {
            catalog: None,
            schema: Some(names[0].clone()),
        }),
        (false, _) => Some(ScopeName {
            catalog: Some(names[0].clone()),
            schema: Some(names[1].clone()),
        }),
    }
}

/// The scope `sql` creates, when it is `CREATE SCHEMA` or `CREATE DATABASE`:
/// a schema, or on MySQL and MariaDB — where the two are one — and for
/// `CREATE DATABASE` anywhere, a catalog.
pub fn creates_scope(sql: &str, dialect: &Dialect) -> Option<ScopeName> {
    if let Some(opened) = opened_comments(sql, dialect) {
        return creates_scope(&opened, dialect);
    }
    let tokens = code_tokens(sql, dialect);
    let word = |i: usize| -> String {
        tokens
            .get(i)
            .filter(|token| is_word(token.kind))
            .map(|token| token.text(sql).to_ascii_uppercase())
            .unwrap_or_default()
    };
    if word(0) != "CREATE" {
        return None;
    }
    let kind = word(1);
    if kind != "SCHEMA" && kind != "DATABASE" {
        return None;
    }
    let mut at = 2;
    // `IF NOT EXISTS`, which mysqldump writes inside an executable comment.
    while matches!(word(at).as_str(), "IF" | "NOT" | "EXISTS")
        || tokens
            .get(at)
            .is_some_and(|token| token.kind == TokenKind::Comment)
    {
        at += 1;
    }
    let name = single(sql, &tokens, at, dialect)?;
    Some(if kind == "DATABASE" || catalog_first(dialect) {
        ScopeName {
            catalog: Some(name),
            schema: None,
        }
    } else {
        ScopeName {
            catalog: None,
            schema: Some(name),
        }
    })
}

/// `sql` with the names of `moving`'s scope rewritten into the new one, as
/// the module documentation describes. Borrowed when nothing moves.
pub fn move_scope<'a>(sql: &'a str, dialect: &Dialect, moving: &ScopeMove) -> Cow<'a, str> {
    if !moving.moves(dialect) {
        return Cow::Borrowed(sql);
    }
    let edits = edits(sql, dialect, moving);
    if edits.is_empty() {
        return Cow::Borrowed(sql);
    }
    let mut out = String::with_capacity(sql.len() + 16);
    let mut at = 0;
    for (range, replacement) in edits {
        out.push_str(&sql[at..range.start]);
        out.push_str(&replacement);
        at = range.end;
    }
    out.push_str(&sql[at..]);
    Cow::Owned(out)
}

/// The rewrites `move_scope` makes, in order and not overlapping.
fn edits(sql: &str, dialect: &Dialect, moving: &ScopeMove) -> Vec<(Range<usize>, String)> {
    let tokens = code_tokens(sql, dialect);
    let mut out: Vec<(Range<usize>, String)> = Vec::new();
    let punct = |i: usize, c: &str| {
        tokens.get(i).is_some_and(|token| {
            matches!(token.kind, TokenKind::Punctuation | TokenKind::Operator)
                && token.text(sql) == c
        })
    };

    let mut i = 0;
    while i < tokens.len() {
        let token = tokens[i];
        match token.kind {
            // Code inside a string: a dollar-quoted body.
            TokenKind::String if token.text(sql).starts_with('$') => {
                if let Some(edit) = dollar_body(sql, token, dialect, moving) {
                    out.push(edit);
                }
                i += 1;
            }
            // Code inside a comment: a MySQL executable comment.
            TokenKind::Comment => {
                if let Some(edit) = executable_comment(sql, token, dialect, moving) {
                    out.push(edit);
                }
                i += 1;
            }
            TokenKind::String => {
                let bare = if holds_a_name(sql, &tokens, i, dialect) {
                    Some(false)
                } else {
                    holds_a_schema(sql, &tokens, i).then_some(true)
                };
                if let Some(bare) = bare
                    && let Some(edit) = name_string(sql, token, dialect, moving, bare)
                {
                    out.push(edit);
                }
                i += 1;
            }
            kind if is_name(kind) => {
                let preceded_by_dot = i > 0 && punct(i - 1, ".");
                // A name after a keyword that takes a scope's name.
                if !preceded_by_dot && !punct(i + 1, ".") {
                    if let Some(level) = scope_keyword(sql, &tokens, i, dialect)
                        && let Some(edit) = rename(sql, token, level, dialect, moving)
                    {
                        out.push(edit);
                    }
                    i += 1;
                    continue;
                }
                if preceded_by_dot {
                    i += 1;
                    continue;
                }
                let mut at = i;
                let chain = chain_at(sql, &tokens, &mut at).unwrap_or_else(|| vec![i]);
                out.extend(chain_edits(sql, &tokens, &chain, dialect, moving));
                i = at.max(i + 1);
            }
            _ => i += 1,
        }
    }
    out
}

/// Which level of a scope the bare name at `i` names, from the keyword ahead
/// of it: `USE x`, `CREATE DATABASE x`, `SCHEMA [IF [NOT] EXISTS] x`,
/// `CURRENT_SCHEMA = x`, `search_path TO x, y`.
fn scope_keyword(sql: &str, tokens: &[Token], i: usize, dialect: &Dialect) -> Option<Level> {
    let upper = |k: usize| tokens[k].text(sql).to_ascii_uppercase();
    let mut k = i;
    // The earlier names of a list: `search_path TO a, b`.
    while k >= 2 && upper(k - 1) == "," && is_name(tokens[k - 2].kind) {
        k -= 2;
    }
    while k >= 1 && matches!(upper(k - 1).as_str(), "IF" | "NOT" | "EXISTS" | "=" | "TO") {
        k -= 1;
    }
    if k == 0 {
        return None;
    }
    match upper(k - 1).as_str() {
        "USE" if uses_catalogs(dialect) => Some(Level::Catalog),
        "DATABASE" | "SCHEMA" if catalog_first(dialect) => Some(Level::Catalog),
        "SCHEMA" | "CURRENT_SCHEMA" | "SEARCH_PATH" => Some(Level::Schema),
        _ => None,
    }
}

/// A level of a scope.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Level {
    Catalog,
    Schema,
}

/// The rewrites of one qualified name, its tokens at `chain`.
fn chain_edits(
    sql: &str,
    tokens: &[Token],
    chain: &[usize],
    dialect: &Dialect,
    moving: &ScopeMove,
) -> Vec<(Range<usize>, String)> {
    let mut out = Vec::new();
    if chain.len() < 2 {
        return out;
    }
    let name = |n: usize| stored(tokens[chain[n]].text(sql), tokens[chain[n]].kind, dialect);
    let is = |n: usize, level: &Option<(String, String)>| {
        level
            .as_ref()
            .is_some_and(|(from, _)| same_name(&name(n), from, dialect))
    };
    let mut push = |n: usize, level: Level| {
        if let Some(edit) = rename(sql, tokens[chain[n]], level, dialect, moving) {
            out.push(edit);
        }
    };
    if catalog_first(dialect) {
        if is(0, &moving.catalog) {
            push(0, Level::Catalog);
        }
    } else if chain.len() >= 3 && is(0, &moving.catalog) {
        push(0, Level::Catalog);
        if is(1, &moving.schema) {
            push(1, Level::Schema);
        }
    } else if is(0, &moving.schema) {
        push(0, Level::Schema);
    }
    out
}

/// The rewrite of the name `token` when it is `level`'s old name.
fn rename(
    sql: &str,
    token: Token,
    level: Level,
    dialect: &Dialect,
    moving: &ScopeMove,
) -> Option<(Range<usize>, String)> {
    let (from, to) = match level {
        Level::Catalog => moving.catalog.as_ref()?,
        Level::Schema => moving.schema.as_ref()?,
    };
    let text = token.text(sql);
    if !same_name(&stored(text, token.kind, dialect), from, dialect) || same_name(from, to, dialect)
    {
        return None;
    }
    Some((token.range(), spelled(to, text, token.kind, dialect)))
}

/// Whether the string at `i` holds a qualified name a function looks up.
fn holds_a_name(sql: &str, tokens: &[Token], i: usize, dialect: &Dialect) -> bool {
    let text = |k: usize| tokens.get(k).map(|token| token.text(sql)).unwrap_or("");
    // `'app.seq'::regclass`
    if text(i + 1) == "::" && text(i + 2).eq_ignore_ascii_case("regclass") {
        return true;
    }
    // The first argument of a function that looks a name up.
    if i >= 2 && text(i - 1) == "(" {
        let function = text(i - 2).to_ascii_lowercase();
        if matches!(
            function.as_str(),
            "nextval"
                | "setval"
                | "currval"
                | "pg_get_serial_sequence"
                | "object_id"
                | "checkident"
        ) {
            return true;
        }
    }
    // SQL Server's `@sequence_name = N'…'`.
    dialect.id() == DialectId::MsSql
        && i >= 2
        && text(i - 1) == "="
        && text(i - 2).eq_ignore_ascii_case("@sequence_name")
}

/// Whether the string at `i` holds a bare schema name: Oracle's
/// `COMPILE_SCHEMA('APP', …)`, SQL Server's `@level0name = N'dbo'`.
fn holds_a_schema(sql: &str, tokens: &[Token], i: usize) -> bool {
    let text = |k: usize| tokens.get(k).map(|token| token.text(sql)).unwrap_or("");
    i >= 2
        && ((text(i - 1) == "(" && text(i - 2).eq_ignore_ascii_case("compile_schema"))
            || (text(i - 1) == "=" && text(i - 2).eq_ignore_ascii_case("@level0name")))
}

/// The rewrite of a string literal holding a name: a qualified name, or a
/// bare schema when `bare`.
fn name_string(
    sql: &str,
    token: Token,
    dialect: &Dialect,
    moving: &ScopeMove,
    bare: bool,
) -> Option<(Range<usize>, String)> {
    let text = token.text(sql);
    let open = text.find('\'')?;
    if !text.ends_with('\'') || text.len() < open + 2 {
        return None;
    }
    let content = text[open + 1..text.len() - 1].replace("''", "'");
    let moved = if bare {
        let (from, to) = moving.schema.as_ref()?;
        if !same_name(&content, from, dialect) || same_name(from, to, dialect) {
            return None;
        }
        to.clone()
    } else {
        match move_names_only(&content, dialect, moving) {
            Cow::Borrowed(_) => return None,
            Cow::Owned(moved) => moved,
        }
    };
    Some((
        token.range(),
        format!("{}'{}'", &text[..open], moved.replace('\'', "''")),
    ))
}

/// [`move_scope`] over text that is nothing but a name, as a string holds
/// one.
fn move_names_only<'a>(text: &'a str, dialect: &Dialect, moving: &ScopeMove) -> Cow<'a, str> {
    let tokens = code_tokens(text, dialect);
    if tokens.is_empty()
        || !tokens
            .iter()
            .all(|token| is_name(token.kind) || token.text(text) == ".")
    {
        return Cow::Borrowed(text);
    }
    let mut at = 0;
    let Some(chain) = chain_at(text, &tokens, &mut at) else {
        return Cow::Borrowed(text);
    };
    let edits = chain_edits(text, &tokens, &chain, dialect, moving);
    if edits.is_empty() {
        return Cow::Borrowed(text);
    }
    let mut out = String::new();
    let mut last = 0;
    for (range, replacement) in edits {
        out.push_str(&text[last..range.start]);
        out.push_str(&replacement);
        last = range.end;
    }
    out.push_str(&text[last..]);
    Cow::Owned(out)
}

/// The rewrite of a dollar-quoted body.
fn dollar_body(
    sql: &str,
    token: Token,
    dialect: &Dialect,
    moving: &ScopeMove,
) -> Option<(Range<usize>, String)> {
    let text = token.text(sql);
    let tag_end = text[1..].find('$')? + 2;
    let tag = &text[..tag_end];
    if text.len() < tag.len() * 2 || !text.ends_with(tag) {
        return None;
    }
    let body = &text[tag.len()..text.len() - tag.len()];
    match move_scope(body, dialect, moving) {
        Cow::Borrowed(_) => None,
        Cow::Owned(moved) => Some((token.range(), format!("{tag}{moved}{tag}"))),
    }
}

/// The rewrite of a MySQL executable comment: `/*!50001 … */`.
fn executable_comment(
    sql: &str,
    token: Token,
    dialect: &Dialect,
    moving: &ScopeMove,
) -> Option<(Range<usize>, String)> {
    if !catalog_first(dialect) {
        return None;
    }
    let text = token.text(sql);
    let head = if text.starts_with("/*!") {
        3
    } else if text.starts_with("/*M!") {
        4
    } else {
        return None;
    };
    if !text.ends_with("*/") || text.len() < head + 2 {
        return None;
    }
    let digits = text[head..].bytes().take_while(u8::is_ascii_digit).count();
    let body = &text[head + digits..text.len() - 2];
    match move_scope(body, dialect, moving) {
        Cow::Borrowed(_) => None,
        Cow::Owned(moved) => Some((
            token.range(),
            format!("{}{moved}*/", &text[..head + digits]),
        )),
    }
}

/// `sql` with each MySQL executable comment replaced by the code in it, as
/// the server reads it: `mysqldump` and `mariadb-dump` write a trigger, a
/// view and a routine entirely inside them. `None` when there is none.
fn opened_comments(sql: &str, dialect: &Dialect) -> Option<String> {
    if !catalog_first(dialect) || !(sql.contains("/*!") || sql.contains("/*M!")) {
        return None;
    }
    let mut out = String::with_capacity(sql.len());
    for token in Lexer::new(sql, dialect) {
        let text = token.text(sql);
        let head = if text.starts_with("/*!") {
            3
        } else if text.starts_with("/*M!") {
            4
        } else {
            out.push_str(text);
            continue;
        };
        if token.kind != TokenKind::Comment || !text.ends_with("*/") || text.len() < head + 2 {
            out.push_str(text);
            continue;
        }
        let digits = text[head..].bytes().take_while(u8::is_ascii_digit).count();
        out.push(' ');
        out.push_str(&text[head + digits..text.len() - 2]);
        out.push(' ');
    }
    Some(out)
}

/// The tokens of `sql` that are not whitespace, and not comments either,
/// except a MySQL executable comment, which is code.
fn code_tokens(sql: &str, dialect: &Dialect) -> Vec<Token> {
    Lexer::new(sql, dialect)
        .filter(|token| match token.kind {
            TokenKind::Whitespace => false,
            TokenKind::Comment => {
                let text = token.text(sql);
                catalog_first(dialect) && (text.starts_with("/*!") || text.starts_with("/*M!"))
            }
            _ => true,
        })
        .collect()
}

/// The token indices of the qualified name starting at `*at`, which is left
/// after it.
fn chain_at(sql: &str, tokens: &[Token], at: &mut usize) -> Option<Vec<usize>> {
    let first = tokens.get(*at)?;
    if !is_name(first.kind) {
        return None;
    }
    let mut chain = vec![*at];
    let mut i = *at + 1;
    while tokens.get(i).is_some_and(|token| token.text(sql) == ".")
        && tokens.get(i + 1).is_some_and(|token| is_name(token.kind))
    {
        chain.push(i + 1);
        i += 2;
    }
    *at = i;
    Some(chain)
}

/// The stored spelling of the single name at `at`, when it is not the start
/// of a qualified one — nor `AUTHORIZATION`, which Db2 and SQL Server allow
/// in place of a schema's name.
fn single(sql: &str, tokens: &[Token], at: usize, dialect: &Dialect) -> Option<String> {
    let token = tokens.get(at)?;
    if !is_name(token.kind)
        || tokens.get(at + 1).is_some_and(|next| next.text(sql) == ".")
        || token.text(sql).eq_ignore_ascii_case("AUTHORIZATION")
    {
        return None;
    }
    Some(stored(token.text(sql), token.kind, dialect))
}

fn is_word(kind: TokenKind) -> bool {
    matches!(
        kind,
        TokenKind::Keyword | TokenKind::Identifier | TokenKind::Function | TokenKind::Type
    )
}

fn is_name(kind: TokenKind) -> bool {
    is_word(kind) || kind == TokenKind::QuotedIdentifier
}

/// The products whose qualifier, in a two-part name, is the database.
const fn catalog_first(dialect: &Dialect) -> bool {
    matches!(dialect.id(), DialectId::MySql | DialectId::MariaDb)
}

/// The products with a `USE` that changes the database.
const fn uses_catalogs(dialect: &Dialect) -> bool {
    matches!(
        dialect.id(),
        DialectId::MySql | DialectId::MariaDb | DialectId::MsSql
    )
}

/// The products that store names as written and compare them without regard
/// to case.
const fn case_insensitive(dialect: &Dialect) -> bool {
    matches!(
        dialect.id(),
        DialectId::MySql | DialectId::MariaDb | DialectId::MsSql | DialectId::Sqlite
    )
}

/// A name as the catalog stores it: unquoted and unescaped when it was
/// quoted, folded the way the product folds a bare name when it was not.
fn stored(text: &str, kind: TokenKind, dialect: &Dialect) -> String {
    if kind == TokenKind::QuotedIdentifier && text.len() >= 2 {
        let (open, close) = (text.as_bytes()[0], text.as_bytes()[text.len() - 1]);
        let inner = &text[1..text.len() - 1];
        return match (open, close) {
            (b'[', b']') => inner.replace("]]", "]"),
            (b'`', b'`') => inner.replace("``", "`"),
            _ => inner.replace("\"\"", "\""),
        };
    }
    match dialect.id() {
        DialectId::Postgres => text.to_lowercase(),
        DialectId::Generic | DialectId::H2 | DialectId::Oracle | DialectId::Db2 => {
            text.to_uppercase()
        }
        _ => text.to_string(),
    }
}

/// `name` written the way `original` was: in the same quotes, or bare where
/// the product would read it back unchanged.
fn spelled(name: &str, original: &str, kind: TokenKind, dialect: &Dialect) -> String {
    if kind != TokenKind::QuotedIdentifier {
        return dialect.quote_ident(name).into_owned();
    }
    match original.as_bytes()[0] {
        b'[' => format!("[{}]", name.replace(']', "]]")),
        b'`' => format!("`{}`", name.replace('`', "``")),
        _ => format!("\"{}\"", name.replace('"', "\"\"")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scope(catalog: Option<&str>, schema: Option<&str>) -> Option<ScopeName> {
        Some(ScopeName {
            catalog: catalog.map(String::from),
            schema: schema.map(String::from),
        })
    }

    #[test]
    fn a_statement_says_where_it_puts_things() {
        let h2 = Dialect::H2;
        assert_eq!(
            statement_scope("CREATE MEMORY TABLE \"APP\".\"T\" (ID INT)", &h2),
            scope(None, Some("APP"))
        );
        assert_eq!(
            statement_scope("-- rows\nINSERT INTO app.t VALUES (1)", &h2),
            scope(None, Some("APP"))
        );
        assert_eq!(statement_scope("CREATE TABLE t (id int)", &h2), None);
        assert_eq!(statement_scope("SELECT * FROM app.t", &h2), None);
        assert_eq!(
            statement_scope(
                "create or replace editionable procedure \"BK\".\"P\" as begin null; end;",
                &Dialect::ORACLE
            ),
            scope(None, Some("BK"))
        );
        assert_eq!(
            statement_scope(
                "CREATE TABLE \"shop\".\"dbo\".\"t\" (id int)",
                &Dialect::MSSQL
            ),
            scope(Some("shop"), Some("dbo"))
        );
        assert_eq!(
            statement_scope("USE [shop]", &Dialect::MSSQL),
            scope(Some("shop"), None)
        );
        assert_eq!(
            statement_scope("USE `shop`", &Dialect::MYSQL),
            scope(Some("shop"), None)
        );
        assert_eq!(
            statement_scope("INSERT INTO `shop`.`t` VALUES (1)", &Dialect::MYSQL),
            scope(Some("shop"), None)
        );
        assert_eq!(
            statement_scope("CREATE DATABASE IF NOT EXISTS shop", &Dialect::MARIADB),
            scope(Some("shop"), None)
        );
        // mariadb-dump keeps a trigger's text as it was written, inside an
        // executable comment.
        assert_eq!(
            statement_scope(
                "/*!50003 CREATE*/ /*!50017 DEFINER=`root`@`%`*/ /*!50003 trigger bk.trg before \
                 insert on bk.child for each row set new.qty = 1 */",
                &Dialect::MARIADB
            ),
            scope(Some("bk"), None)
        );
        assert_eq!(
            statement_scope("CREATE SCHEMA bk", &Dialect::POSTGRES),
            scope(None, Some("bk"))
        );
        assert_eq!(
            statement_scope(
                "ALTER TABLE ONLY \"Bk\".parent ADD CONSTRAINT p PRIMARY KEY (id)",
                &Dialect::POSTGRES
            ),
            scope(None, Some("Bk"))
        );
    }

    #[test]
    fn qualified_names_move_and_data_stays() {
        let moving = ScopeMove::schema("bk", "bk2");
        let pg = Dialect::POSTGRES;
        assert_eq!(
            move_scope(
                "INSERT INTO bk.parent (note) VALUES ('bk.parent')",
                &pg,
                &moving
            ),
            "INSERT INTO bk2.parent (note) VALUES ('bk.parent')"
        );
        assert_eq!(
            move_scope("ALTER SEQUENCE bk.s OWNED BY bk.t.id", &pg, &moving),
            "ALTER SEQUENCE bk2.s OWNED BY bk2.t.id"
        );
        // A column of a table named like the schema is not the schema.
        assert_eq!(
            move_scope("SELECT t.bk FROM bk.t", &pg, &moving),
            "SELECT t.bk FROM bk2.t"
        );
        // Another schema stays where it is.
        assert_eq!(
            move_scope(
                "CREATE TABLE bk.t (h text DEFAULT public.gen_random_uuid())",
                &pg,
                &moving
            ),
            "CREATE TABLE bk2.t (h text DEFAULT public.gen_random_uuid())"
        );
        // A name that needs quotes gets them.
        let moving_up = ScopeMove::schema("bk", "Archive");
        assert_eq!(
            move_scope("CREATE SCHEMA bk", &pg, &moving_up),
            "CREATE SCHEMA \"Archive\""
        );
        assert_eq!(
            move_scope("CREATE TABLE \"bk\".t ()", &pg, &moving_up),
            "CREATE TABLE \"Archive\".t ()"
        );
    }

    #[test]
    fn names_in_strings_that_look_one_up_move() {
        let moving = ScopeMove::schema("bk", "bk2");
        let pg = Dialect::POSTGRES;
        assert_eq!(
            move_scope(
                "CREATE TABLE bk.t (id integer DEFAULT nextval('bk.t_id_seq'::regclass))",
                &pg,
                &moving
            ),
            "CREATE TABLE bk2.t (id integer DEFAULT nextval('bk2.t_id_seq'::regclass))"
        );
        assert_eq!(
            move_scope(
                "SELECT pg_catalog.setval(pg_catalog.pg_get_serial_sequence('bk.\"T\"', 'id'), 7, true)",
                &pg,
                &moving
            ),
            "SELECT pg_catalog.setval(pg_catalog.pg_get_serial_sequence('bk2.\"T\"', 'id'), 7, true)"
        );
        let oracle = ScopeMove::schema("BK", "BK2");
        assert_eq!(
            move_scope(
                "BEGIN DBMS_UTILITY.COMPILE_SCHEMA('BK', FALSE); END;",
                &Dialect::ORACLE,
                &oracle
            ),
            "BEGIN DBMS_UTILITY.COMPILE_SCHEMA('BK2', FALSE); END;"
        );
        let ms = ScopeMove {
            catalog: Some(("bkdb".into(), "bkdb".into())),
            schema: Some(("bk".into(), "sales".into())),
        };
        let mssql = Dialect::MSSQL;
        assert_eq!(
            move_scope(
                "DBCC CHECKIDENT ('\"bkdb\".\"bk\".\"t\"', RESEED, 4)",
                &mssql,
                &ms
            ),
            "DBCC CHECKIDENT ('\"bkdb\".\"sales\".\"t\"', RESEED, 4)"
        );
        assert_eq!(
            move_scope(
                "EXEC sys.sp_addextendedproperty @name = N'MS_Description', @value = N'bk', \
                 @level0type = N'SCHEMA', @level0name = N'bk', @level1type = N'TABLE', @level1name = N't'",
                &mssql,
                &ms
            ),
            "EXEC sys.sp_addextendedproperty @name = N'MS_Description', @value = N'bk', \
             @level0type = N'SCHEMA', @level0name = N'sales', @level1type = N'TABLE', @level1name = N't'"
        );
        assert_eq!(
            move_scope(
                "INSERT INTO \"bkdb\".\"bk\".\"t\" VALUES (N'bk')",
                &mssql,
                &ms
            ),
            "INSERT INTO \"bkdb\".\"sales\".\"t\" VALUES (N'bk')"
        );
    }

    #[test]
    fn scope_keywords_and_bodies_move() {
        let pg = Dialect::POSTGRES;
        let moving = ScopeMove::schema("bk", "bk2");
        assert_eq!(
            move_scope(
                "CREATE EXTENSION IF NOT EXISTS pgcrypto WITH SCHEMA bk",
                &pg,
                &moving
            ),
            "CREATE EXTENSION IF NOT EXISTS pgcrypto WITH SCHEMA bk2"
        );
        assert_eq!(
            move_scope("ALTER SCHEMA bk OWNER TO rudbman", &pg, &moving),
            "ALTER SCHEMA bk2 OWNER TO rudbman"
        );
        assert_eq!(
            move_scope("SET search_path TO bk, public", &pg, &moving),
            "SET search_path TO bk2, public"
        );
        assert_eq!(
            move_scope(
                "CREATE FUNCTION bk.f() RETURNS int AS $body$ SELECT count(*) FROM bk.t WHERE x = 'bk.t' $body$ LANGUAGE sql",
                &pg,
                &moving
            ),
            "CREATE FUNCTION bk2.f() RETURNS int AS $body$ SELECT count(*) FROM bk2.t WHERE x = 'bk.t' $body$ LANGUAGE sql"
        );

        let my = ScopeMove::catalog("shop", "shop_copy");
        let mysql = Dialect::MYSQL;
        assert_eq!(move_scope("USE `shop`", &mysql, &my), "USE `shop_copy`");
        assert_eq!(
            move_scope("CREATE DATABASE IF NOT EXISTS shop", &mysql, &my),
            "CREATE DATABASE IF NOT EXISTS shop_copy"
        );
        assert_eq!(
            move_scope("INSERT INTO `shop`.`t` VALUES ('shop.t')", &mysql, &my),
            "INSERT INTO `shop_copy`.`t` VALUES ('shop.t')"
        );
        assert_eq!(
            move_scope(
                "/*!50001 VIEW `v` AS select `shop`.`t`.`a` AS `a` from `shop`.`t` */",
                &mysql,
                &my
            ),
            "/*!50001 VIEW `v` AS select `shop_copy`.`t`.`a` AS `a` from `shop_copy`.`t` */"
        );
        assert_eq!(
            move_scope(
                "ALTER SESSION SET CURRENT_SCHEMA = BK",
                &Dialect::ORACLE,
                &ScopeMove::schema("BK", "BK2")
            ),
            "ALTER SESSION SET CURRENT_SCHEMA = BK2"
        );
        assert_eq!(
            move_scope(
                "USE [bkdb]",
                &Dialect::MSSQL,
                &ScopeMove::catalog("bkdb", "other")
            ),
            "USE [other]"
        );
    }

    #[test]
    fn the_scope_a_statement_creates() {
        assert_eq!(
            creates_scope("CREATE SCHEMA bk", &Dialect::POSTGRES),
            scope(None, Some("bk"))
        );
        assert_eq!(
            creates_scope(
                "CREATE SCHEMA IF NOT EXISTS \"APP\" AUTHORIZATION \"SA\"",
                &Dialect::H2
            ),
            scope(None, Some("APP"))
        );
        assert_eq!(
            creates_scope(
                "CREATE DATABASE /*!32312 IF NOT EXISTS*/ `shop`",
                &Dialect::MYSQL
            ),
            scope(Some("shop"), None)
        );
        assert_eq!(
            creates_scope("CREATE SCHEMA shop", &Dialect::MARIADB),
            scope(Some("shop"), None)
        );
        assert_eq!(
            creates_scope("CREATE DATABASE bkdb", &Dialect::MSSQL),
            scope(Some("bkdb"), None)
        );
        assert_eq!(
            creates_scope("CREATE TABLE bk.t (id int)", &Dialect::POSTGRES),
            None
        );
        assert_eq!(
            creates_scope("CREATE SCHEMA AUTHORIZATION u", &Dialect::DB2),
            None
        );
    }

    #[test]
    fn names_compare_the_way_the_product_compares_them() {
        // Oracle folds a bare name up: `bk.t` is in `BK`, `"bk".t` is not.
        let oracle = ScopeMove::schema("BK", "BK2");
        assert_eq!(
            move_scope("INSERT INTO bk.t VALUES (1)", &Dialect::ORACLE, &oracle),
            "INSERT INTO BK2.t VALUES (1)"
        );
        assert_eq!(
            move_scope("INSERT INTO \"bk\".t VALUES (1)", &Dialect::ORACLE, &oracle),
            "INSERT INTO \"bk\".t VALUES (1)"
        );
        // SQL Server does not care about case.
        let ms = ScopeMove::schema("dbo", "sales");
        assert_eq!(
            move_scope("INSERT INTO [DBO].[t] VALUES (1)", &Dialect::MSSQL, &ms),
            "INSERT INTO [sales].[t] VALUES (1)"
        );
        // Nothing moves when the names are the same name.
        assert!(!ScopeMove::schema("dbo", "DBO").moves(&Dialect::MSSQL));
        assert!(ScopeMove::schema("bk", "BK").moves(&Dialect::POSTGRES));
    }
}
