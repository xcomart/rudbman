//! Writing an identifier back out: quoting it when the server would otherwise
//! read it as something else.
//!
//! Everything else in this crate reads SQL. This module is the one place that
//! *emits* it, for the callers that assemble a statement out of names they got
//! from a catalog — the query builder (architecture document §7.7), the result
//! grid's sort, a future "generate DDL". Those callers have a table name and a
//! column name and need them to survive the round trip to the server.
//!
//! ```
//! use rudbman_sql::Dialect;
//!
//! let pg = Dialect::from_id("postgres");
//! assert_eq!(pg.quote_ident("orders"), "orders");        // already lower case
//! assert_eq!(pg.quote_ident("Orders"), "\"Orders\"");    // would fold to `orders`
//! assert_eq!(pg.qualify(["app", "order"]), "app.\"order\"");
//!
//! let ora = Dialect::from_id("oracle");
//! assert_eq!(ora.quote_ident("ORDERS"), "ORDERS");       // already upper case
//! assert_eq!(ora.quote_ident("orders"), "\"orders\"");   // would fold to `ORDERS`
//! ```
//!
//! # Why not quote everything
//!
//! Always quoting is correct and unreadable, and on two of the six products it
//! is also *wrong* often enough to matter. `SELECT "ID" FROM "USERS"` against
//! PostgreSQL fails on a table created as `create table users (id int)`, because
//! PostgreSQL folded those names to lower case when it stored them and quoting
//! turns the fold off; Oracle and H2 fold the other way and break the mirror
//! image of that statement. A name a person typed matches what the catalog
//! holds only when it is spelled the way the catalog holds it, and quoting is
//! how a caller says "spelled exactly like this". So the rule here is to quote
//! when leaving the name bare would change it or break the parse, and not
//! otherwise:
//!
//! * the name is empty — an input error, and bare it would produce a syntax
//!   error somewhere far from its cause;
//! * it is not an ASCII identifier: a leading digit, a space, a hyphen, a dot,
//!   or any non-ASCII letter. Unicode identifiers are quoted rather than
//!   analyzed. Every product's rules for them differ, quoting one that did not
//!   need it costs nothing, and the alternative is a table of character classes
//!   this crate has no other use for;
//! * it is a reserved word ([`Dialect::is_keyword`]);
//! * it disagrees with the dialect's unquoted case folding — see below.
//!
//! Type names ([`Dialect::is_type`]) are deliberately *not* quoted. `VALUE`,
//! `TEXT` and `NUMBER` are ordinary column names in the wild, and a grid that
//! renders `t."VALUE"` where the user wrote `t.value` has made the SQL harder
//! to read for a collision that cannot happen: a type name is never a keyword
//! in the position an identifier appears in.
//!
//! # Case folding
//!
//! An unquoted identifier is normalized by the server before it is looked up:
//! Oracle and H2 (and the generic profile, which follows the SQL standard) fold
//! to upper case, PostgreSQL folds to lower case, and MySQL, SQL Server and
//! SQLite preserve what they were given. So a name with a lower-case letter in
//! it is quoted for Oracle, a name with an upper-case letter is quoted for
//! PostgreSQL, and case is never a reason to quote for the other three.
//!
//! The test is ASCII-only, which is enough: a name with a non-ASCII letter has
//! already been quoted by the rule above it.
//!
//! This lives here rather than in [`Syntax`](crate::Syntax) because that record
//! is documented as the rules *the lexer* branches on, and folding is not one —
//! nothing about highlighting or statement splitting changes with it.
//!
//! # Which quote character
//!
//! A double quote everywhere except MySQL, which needs a backtick: `"..."` is a
//! *string literal* there unless the server runs in `ANSI_QUOTES` mode, which
//! no client can see, so a double-quoted column name would silently become a
//! comparison against its own text. A quote character inside the name is
//! doubled — `` ` `` → ``` `` ``` , `"` → `""` — which is how both forms escape,
//! everywhere.
//!
//! SQL Server's `[...]` and SQLite's acceptance of the same are not used. Both
//! products take the standard double quote as well, one form is one thing to
//! test, and `[...]` is the odd one out anyway: it is the only quoting syntax
//! here whose closing character is not its opening one.

use std::borrow::Cow;

use crate::dialect::{Dialect, DialectId};

/// What a server does to an identifier that reaches it unquoted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Folding {
    /// Folded to upper case: Oracle, H2, and the SQL standard.
    Upper,
    /// Folded to lower case: PostgreSQL.
    Lower,
    /// Stored as written: MySQL, SQL Server, SQLite.
    Preserve,
}

impl Dialect {
    /// Quotes `name` if this dialect would not read it back unchanged.
    ///
    /// Returns the input borrowed when no quoting is needed, which is the
    /// common case — a catalog name in the dialect's own case, assembled into a
    /// statement, allocates nothing. See the [module
    /// documentation](self) for the full rule and the reasoning behind it.
    ///
    /// ```
    /// use rudbman_sql::Dialect;
    ///
    /// let d = Dialect::from_id("mysql");
    /// assert_eq!(d.quote_ident("Orders"), "Orders");      // MySQL preserves case
    /// assert_eq!(d.quote_ident("order"), "`order`");      // reserved word
    /// assert_eq!(d.quote_ident("a`b"), "`a``b`");         // doubled
    /// ```
    pub fn quote_ident<'a>(&self, name: &'a str) -> Cow<'a, str> {
        if self.needs_quoting(name) {
            Cow::Owned(self.quote_always(name))
        } else {
            Cow::Borrowed(name)
        }
    }

    /// Joins `parts` into a qualified name, quoting each part on its own.
    ///
    /// The separator is a dot, and nothing else is inserted: `qualify(["app",
    /// "order details"])` is `app."order details"`. Empty and absent parts —
    /// the schema of a database that has none, a catalog the driver left null —
    /// are the caller's to filter out, because only the caller knows whether a
    /// missing part means "omit it" or "the name is broken"; an empty string
    /// reaching here is quoted like any other unusable name.
    ///
    /// ```
    /// use rudbman_sql::Dialect;
    ///
    /// let d = Dialect::from_id("oracle");
    /// assert_eq!(d.qualify(["APP", "ORDERS"]), "APP.ORDERS");
    /// let schema: Option<&str> = None;
    /// assert_eq!(d.qualify(schema.into_iter().chain(["ORDERS"])), "ORDERS");
    /// ```
    pub fn qualify<'a>(&self, parts: impl IntoIterator<Item = &'a str>) -> String {
        let mut out = String::new();
        for part in parts {
            if !out.is_empty() {
                out.push('.');
            }
            out.push_str(&self.quote_ident(part));
        }
        out
    }

    /// Whether leaving `name` bare would change it or break the parse.
    fn needs_quoting(&self, name: &str) -> bool {
        let mut chars = name.chars();
        let Some(first) = chars.next() else {
            return true;
        };
        if !first.is_ascii_alphabetic() && first != '_' {
            return true;
        }
        if chars.any(|c| !c.is_ascii_alphanumeric() && c != '_') {
            return true;
        }
        if self.is_keyword(name) {
            return true;
        }
        // From here the name is pure ASCII, so the case test is a byte test.
        match self.folding() {
            Folding::Upper => name.bytes().any(|b| b.is_ascii_lowercase()),
            Folding::Lower => name.bytes().any(|b| b.is_ascii_uppercase()),
            Folding::Preserve => false,
        }
    }

    /// Quotes unconditionally, doubling any quote character in the name.
    fn quote_always(&self, name: &str) -> String {
        // `double_quoted_strings` is MySQL and only MySQL: it says a double
        // quote opens a string here, which is exactly the reason to reach for
        // the backtick. H2 and SQLite accept backticks too, but for them the
        // double quote is the native form and the one their catalogs speak.
        let quote = if self.syntax().double_quoted_strings {
            '`'
        } else {
            '"'
        };
        let mut out = String::with_capacity(name.len() + 2);
        out.push(quote);
        for c in name.chars() {
            if c == quote {
                out.push(quote);
            }
            out.push(c);
        }
        out.push(quote);
        out
    }

    /// What this dialect does to an unquoted identifier.
    fn folding(&self) -> Folding {
        match self.id() {
            DialectId::Generic | DialectId::H2 | DialectId::Oracle => Folding::Upper,
            DialectId::Postgres => Folding::Lower,
            DialectId::MySql | DialectId::MsSql | DialectId::Sqlite => Folding::Preserve,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The dialects that fold up, the one that folds down, and the three that
    /// leave a name alone — the only reason an ordinary name is ever quoted.
    #[test]
    fn case_folding_decides_for_ordinary_names() {
        for d in [Dialect::ORACLE, Dialect::H2, Dialect::GENERIC] {
            assert_eq!(d.quote_ident("ORDERS"), "ORDERS", "{}", d.name());
            assert_eq!(d.quote_ident("orders"), "\"orders\"", "{}", d.name());
            assert_eq!(d.quote_ident("Orders"), "\"Orders\"", "{}", d.name());
        }

        assert_eq!(Dialect::POSTGRES.quote_ident("orders"), "orders");
        assert_eq!(Dialect::POSTGRES.quote_ident("ORDERS"), "\"ORDERS\"");
        assert_eq!(Dialect::POSTGRES.quote_ident("Orders"), "\"Orders\"");

        for d in [Dialect::MYSQL, Dialect::MSSQL, Dialect::SQLITE] {
            for name in ["orders", "ORDERS", "Orders", "orderS"] {
                assert_eq!(d.quote_ident(name), name, "{} {name}", d.name());
            }
        }
    }

    /// A name that is already fine is handed back untouched, not copied.
    #[test]
    fn unquoted_names_are_borrowed() {
        assert!(matches!(
            Dialect::POSTGRES.quote_ident("orders"),
            Cow::Borrowed(_)
        ));
        assert!(matches!(
            Dialect::POSTGRES.quote_ident("Orders"),
            Cow::Owned(_)
        ));
    }

    /// A reserved word is quoted whatever its case, in the dialect that
    /// reserves it.
    #[test]
    fn keywords_are_quoted() {
        assert_eq!(Dialect::POSTGRES.quote_ident("order"), "\"order\"");
        assert_eq!(Dialect::POSTGRES.quote_ident("select"), "\"select\"");
        assert_eq!(Dialect::MYSQL.quote_ident("order"), "`order`");
        assert_eq!(Dialect::MYSQL.quote_ident("ORDER"), "`ORDER`");
        assert_eq!(Dialect::MSSQL.quote_ident("select"), "\"select\"");
        assert_eq!(Dialect::ORACLE.quote_ident("SELECT"), "\"SELECT\"");
        assert_eq!(Dialect::SQLITE.quote_ident("Select"), "\"Select\"");

        // Only where it *is* reserved: MySQL's `straight_join` is nobody
        // else's, and PostgreSQL's `ilike` is not MySQL's.
        assert_eq!(
            Dialect::MYSQL.quote_ident("straight_join"),
            "`straight_join`"
        );
        assert_eq!(
            Dialect::SQLITE.quote_ident("straight_join"),
            "straight_join"
        );
        assert_eq!(Dialect::POSTGRES.quote_ident("ilike"), "\"ilike\"");
        assert_eq!(Dialect::MYSQL.quote_ident("ilike"), "ilike");
    }

    /// Type names are not keywords, and a column called `int` stays bare.
    #[test]
    fn type_names_are_not_quoted() {
        for name in ["int", "text", "varchar"] {
            assert!(Dialect::POSTGRES.is_type(name), "{name}");
            assert!(!Dialect::POSTGRES.is_keyword(name), "{name}");
            assert_eq!(Dialect::POSTGRES.quote_ident(name), name);
        }
        assert_eq!(Dialect::MYSQL.quote_ident("value"), "value");
        assert_eq!(Dialect::ORACLE.quote_ident("NUMBER"), "NUMBER");
    }

    /// Anything that is not an ASCII identifier is quoted, whatever the
    /// dialect's case rule says.
    #[test]
    fn non_identifier_shapes_are_quoted() {
        let d = Dialect::MYSQL; // preserves case, so nothing else can be the cause
        assert_eq!(d.quote_ident(""), "``");
        assert_eq!(d.quote_ident("order details"), "`order details`");
        assert_eq!(d.quote_ident("order-details"), "`order-details`");
        assert_eq!(d.quote_ident("2fast"), "`2fast`");
        assert_eq!(d.quote_ident("a.b"), "`a.b`");
        assert_eq!(d.quote_ident("count(*)"), "`count(*)`");
        assert_eq!(d.quote_ident("naïve"), "`naïve`");
        assert_eq!(d.quote_ident("주문"), "`주문`");

        // The shapes that are still identifiers.
        assert_eq!(d.quote_ident("_private"), "_private");
        assert_eq!(d.quote_ident("t1_2"), "t1_2");
    }

    /// The quote character is doubled inside the name, in both forms.
    #[test]
    fn embedded_quotes_are_doubled() {
        assert_eq!(Dialect::MYSQL.quote_ident("a`b"), "`a``b`");
        assert_eq!(Dialect::MYSQL.quote_ident("``"), "``````");
        // A double quote is not the quote character there, so it passes
        // through — and a backtick is not the quote character elsewhere.
        assert_eq!(Dialect::MYSQL.quote_ident("a\"b"), "`a\"b`");
        assert_eq!(Dialect::POSTGRES.quote_ident("a\"b"), "\"a\"\"b\"");
        assert_eq!(Dialect::POSTGRES.quote_ident("a`b"), "\"a`b\"");
        assert_eq!(Dialect::MSSQL.quote_ident("a]b"), "\"a]b\"");
    }

    /// Qualified names quote each part on its own.
    #[test]
    fn qualify_quotes_part_by_part() {
        assert_eq!(
            Dialect::ORACLE.qualify(["APP", "ORDER DETAILS"]),
            "APP.\"ORDER DETAILS\""
        );
        assert_eq!(
            Dialect::POSTGRES.qualify(["public", "order", "id"]),
            "public.\"order\".id"
        );
        assert_eq!(Dialect::MYSQL.qualify(["app", "order"]), "app.`order`");
        assert_eq!(Dialect::MYSQL.qualify(["orders"]), "orders");
        assert_eq!(Dialect::MYSQL.qualify(Vec::<&str>::new()), "");

        // A part the caller failed to filter is quoted rather than dropped: the
        // resulting `.` is visible, which is the point.
        assert_eq!(Dialect::MYSQL.qualify(["", "orders"]), "``.orders");
    }
}
