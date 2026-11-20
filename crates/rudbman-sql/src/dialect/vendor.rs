//! The interface a database product implements.
//!
//! Every place this crate answers differently per product is one method here,
//! and each product answers all of them in its own file next to this one. A
//! method without a default is one where no answer is safe to assume: a new
//! product that has not said how it pages, folds names or reads a script does
//! not compile, rather than borrowing another product's habits unnoticed.

use super::{DialectId, Syntax};
use crate::block::Family as Block;
use crate::ddl::{AlterStyle, Unsupported};
use crate::explain::Explain;
use crate::format::ddl::Family as Layout;
use crate::ident::Folding;
use crate::keywords::WordTables;
use crate::script::Client;

/// One database product's answers to everything this crate asks per product.
///
/// Implemented by a unit struct per product and reached through
/// [`Dialect`](super::Dialect), which is a pointer to one of them.
pub(crate) trait Vendor: Send + Sync {
    // Identity.

    /// Which dialect this is.
    fn id(&self) -> DialectId;

    /// The canonical `dialect` string, as `drivers.json` spells it.
    ///
    /// Round-trips through [`Dialect::from_id`](super::Dialect::from_id), and
    /// unique among the registered products, aliases included.
    fn name(&self) -> &'static str;

    /// Other spellings [`Dialect::from_id`](super::Dialect::from_id) accepts
    /// for this product, compared without regard to ASCII case.
    fn aliases(&self) -> &'static [&'static str] {
        &[]
    }

    /// The product's name as a person writes it, for a message about it.
    ///
    /// [`Self::name`] is the lower-case configuration key; a sentence that
    /// named the product as `mssql` would be reading that key aloud.
    fn product(&self) -> &'static str;

    // Lexis.

    /// The lexical rules the lexer branches on.
    fn syntax(&self) -> &'static Syntax;

    /// The reserved words and type names this product adds to the common
    /// tables.
    fn words(&self) -> WordTables;

    /// Frequently used built-in functions SQL completion offers beyond the
    /// common ones, uppercase. A small list, not a catalog.
    fn functions(&self) -> &'static [&'static str];

    // Names.

    /// What the server does to an identifier that reaches it unquoted.
    fn folding(&self) -> Folding;

    /// Whether the product stores names as written and compares them without
    /// regard to case.
    fn case_insensitive_names(&self) -> bool;

    /// Whether the qualifier of a two-part name is the database rather than
    /// the schema.
    fn catalog_first(&self) -> bool;

    /// Whether `USE` changes the database.
    fn uses_catalogs(&self) -> bool;

    // Statements and scripts.

    /// Whose rules a procedural body is read by when statements are split.
    fn block_family(&self) -> Block;

    /// How a routine's body is laid out by the DDL formatter.
    fn routine_layout(&self) -> Layout;

    /// Whose client conventions a script is read by.
    fn client(&self) -> Client;

    // Reading.

    /// The clause that cuts one page out of an ordered `SELECT`, or `None`
    /// where this crate will not write one; see
    /// [`Dialect::page_clause`](super::Dialect::page_clause).
    fn page_clause(&self, limit: u64, offset: u64) -> Option<String>;

    /// Whether a grid with no [`Self::page_clause`] reads its rows through a
    /// server-side cursor rather than the driver's default result set.
    fn reads_by_server_cursor(&self) -> bool;

    /// The statements that read the plan of `sql`, one trimmed statement
    /// without its semicolon, or `None` where the product cannot be asked
    /// without setting something up first.
    fn explain(&self, sql: &str) -> Option<Explain>;

    // Writing.

    /// A row of nothing but defaults inserted into `table`, or `None` where
    /// the product has no such statement.
    ///
    /// `first_column` is the table's first column, already quoted for this
    /// product, for a product that has to name one.
    fn empty_insert(&self, table: &str, first_column: Option<&str>) -> Option<String>;

    /// How one `ALTER TABLE` is spelled, and the two disagreements a
    /// `CREATE TABLE` runs into.
    fn alter_style(&self) -> &'static AlterStyle;

    /// Why the product cannot do `what`, for a structure edit
    /// [`Self::alter_style`] refuses; `None` gives the general reason.
    fn unsupported_reason(&self, _what: Unsupported) -> Option<&'static str> {
        None
    }
}
