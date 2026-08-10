//! SQL lexing, dialects, and statement splitting: everything rudbman needs to
//! understand the *shape* of a script without understanding the script.
//!
//! Two things are built on this crate. The editor (`rudbman-editor`) colors a
//! buffer with it and finds the statement under the cursor with it; the query
//! runner takes the statements it cuts. A future completion engine will take the
//! same tokens. None of them is a UI concern here — this crate takes `&str` and
//! returns spans, it has no dependencies at all, and it does not know that gpui
//! exists.
//!
//! ```
//! use rudbman_sql::{Dialect, TokenKind, lex, split_statements, statement_at};
//!
//! let dialect = Dialect::from_id("postgres");   // the `dialect` of a DriverDef
//! let script = "-- daily counts\nselect count(*) from orders where day = $1;\nvacuum;";
//!
//! // Highlighting: classified spans, tiling the input.
//! let tokens = lex(script, &dialect);
//! assert_eq!(tokens[0].kind, TokenKind::Comment);
//! assert!(tokens.iter().any(|t| t.kind == TokenKind::Parameter));
//!
//! // Execution: byte ranges, semicolon-separated.
//! assert_eq!(split_statements(script, &dialect).len(), 2);
//! let here = statement_at(script, 30, &dialect).unwrap();
//! assert!(here.sql(script).starts_with("-- daily counts"));
//! ```
//!
//! # A lexer, not a parser
//!
//! There is no AST here and there will not be one. Syntax highlighting needs to
//! know that `SELECT` is a keyword and `'x;y'` is a string, and nothing above
//! that; statement splitting needs the same and nothing more. Building a grammar
//! instead would mean building one *per dialect* — every vendor's SQL diverges
//! above the token level, and none of them diverges much below it — for
//! information neither caller has a use for. The architecture document, §7.4,
//! is where that call is recorded, alongside the decision not to bring in
//! tree-sitter.
//!
//! # Incremental by construction
//!
//! A script can be 100MB, and re-lexing all of it on every keystroke is not
//! something to optimize later. The lexer works a line at a time and hands back
//! a [`LineState`] — what the line ended in the middle of, if anything. An
//! editor re-lexes from the edited line downwards and stops at the first line
//! whose end state is unchanged, which for almost every edit is the first line
//! it tries. See [`lex_line`] and the module documentation of [`mod@lexer`].
//!
//! # Dialects
//!
//! [`Dialect::from_id`] takes the `dialect` string of a driver definition
//! (architecture document §8) — `"h2"`, `"postgres"`, `"mysql"`, `"sqlite"`,
//! `"oracle"`, `"mssql"`, `"generic"` — and anything unrecognized is generic
//! SQL rather than an error. What a dialect changes is comments, quoting,
//! parameters and reserved words; [`Syntax`] is the complete list, in one
//! record, and the reserved-word tables are in `src/keywords.rs`.
//!
//! A dialect also knows how to *write* an identifier back out.
//! [`Dialect::quote_ident`] quotes a name only when leaving it bare would
//! change it — a reserved word, a space, a case the server would fold — and
//! [`Dialect::qualify`] joins the parts of a qualified name that way. That is
//! the crate's one output-side API, and [`mod@ident`] explains why it is not
//! simply "quote everything".
//!
//! # Writing DML
//!
//! The other output-side module is [`mod@dml`], which the result grid's editing
//! mode uses: hand it one table's staged changes and [`plan_edits`] answers with
//! the `DELETE`, `UPDATE` and `INSERT` statements that apply them, in the order
//! a transaction can run them. Every value in those statements is a `?` and
//! travels beside the SQL — a literal cannot be undone, and the one component
//! that knows how to format one is on the far side of the bridge. That module
//! documents the reasoning.
//!
//! # Known limitations
//!
//! Written down rather than half-implemented:
//!
//! * **Oracle PL/SQL blocks and MySQL `DELIMITER`** are not understood by
//!   [`split_statements`]. A `BEGIN ... END;` block splits into pieces, the `/`
//!   terminator means nothing, and `DELIMITER //` is not honoured. See the
//!   [`mod@statement`] module documentation for why a half-working version would
//!   be worse, and when the question gets answered.
//! * **Oracle's `q'[...]'` quote operator** is lexed as the identifier `q`
//!   followed by an ordinary string, so a `'` inside the body ends it early.
//! * **Server modes that change the lexer** are assumed to be at their defaults:
//!   MySQL's `ANSI_QUOTES` and `NO_BACKSLASH_ESCAPES`, SQL Server's
//!   `QUOTED_IDENTIFIER OFF`. Only a live connection could know better, and only
//!   a color would change.
//! * **A word is a function when `(` follows it** on the same line, which calls
//!   `t (x)` in a join a function. Context enough to do better is a parser.
//!
//! # Layout
//!
//! * [`mod@dialect`] — [`Dialect`], [`DialectId`], and the [`Syntax`] record of
//!   what the vendors disagree about.
//! * [`mod@ident`] — [`Dialect::quote_ident`] and [`Dialect::qualify`], for the
//!   callers that write SQL rather than read it.
//! * [`mod@dml`] — [`plan_edits`], [`TableEdits`], [`DmlStatement`],
//!   [`DmlValue`]: the grid's staged edits turned into parameterized SQL.
//! * [`mod@lexer`] — [`Token`], [`TokenKind`], [`LineState`], [`lex_line`],
//!   [`lex`], [`Lexer`].
//! * [`mod@statement`] — [`StatementSpan`], [`split_statements`],
//!   [`statement_at`].
//! * `keywords` — the reserved-word tables, private.

#![warn(missing_docs)]

pub mod dialect;
pub mod dml;
pub mod ident;
pub mod lexer;
pub mod statement;

mod keywords;

pub use dialect::{Dialect, DialectId, Syntax};
pub use dml::{
    DmlError, DmlKind, DmlStatement, DmlValue, InsertCell, RowUpdate, TableEdits, plan_edits,
};
pub use lexer::{Lexer, LineState, Token, TokenKind, lex, lex_line};
pub use statement::{StatementSpan, split_statements, statement_at};
