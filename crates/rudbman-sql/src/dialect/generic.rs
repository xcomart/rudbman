//! Plain SQL: the common word tables and nothing vendor-specific.
//!
//! Also what an unrecognized `dialect` string resolves to, so every answer
//! here is the standard one, or a refusal where no form is accepted by every
//! product.

use super::{DialectId, Syntax, Vendor};
use crate::block::{BeginRule, Family as Block, Routines};
use crate::ddl::AlterStyle;
use crate::explain::Explain;
use crate::format::ddl::Family as Layout;
use crate::ident::Folding;
use crate::keywords::WordTables;
use crate::removal::DropSpelling;
use crate::script::Client;

/// Standard SQL.
pub(crate) struct Generic;

impl Vendor for Generic {
    fn id(&self) -> DialectId {
        DialectId::Generic
    }

    fn name(&self) -> &'static str {
        "generic"
    }

    fn aliases(&self) -> &'static [&'static str] {
        &["sql", "ansi"]
    }

    fn product(&self) -> &'static str {
        "generic SQL"
    }

    fn syntax(&self) -> &'static Syntax {
        &Syntax::BASE
    }

    fn words(&self) -> WordTables {
        WordTables::NONE
    }

    fn functions(&self) -> &'static [&'static str] {
        &[]
    }

    fn folding(&self) -> Folding {
        Folding::Upper
    }

    fn case_insensitive_names(&self) -> bool {
        false
    }

    fn catalog_first(&self) -> bool {
        false
    }

    fn uses_catalogs(&self) -> bool {
        false
    }

    /// The guess for a product nobody named: `BEGIN` opens a block unless a
    /// transaction word follows, and a body may be written in anything.
    fn block_family(&self) -> Block {
        Block::Psm {
            begin: BeginRule::UnlessTransaction,
            routines: Routines::Unknown,
        }
    }

    fn routine_layout(&self) -> Layout {
        Layout::Other
    }

    fn client(&self) -> Client {
        Client::Plain
    }

    /// Plain SQL has no page clause every product accepts.
    fn page_clause(&self, _limit: u64, _offset: u64) -> Option<String> {
        None
    }

    fn reads_by_server_cursor(&self) -> bool {
        false
    }

    /// `EXPLAIN <sql>`, the most common spelling, and the database's own
    /// refusal when it is not this one's.
    fn explain(&self, sql: &str) -> Option<Explain> {
        Some(Explain::prefixed("EXPLAIN", sql))
    }

    /// The standard form.
    fn empty_insert(&self, table: &str, _first_column: Option<&str>) -> Option<String> {
        Some(format!("INSERT INTO {table} DEFAULT VALUES"))
    }

    fn alter_style(&self) -> &'static AlterStyle {
        &AlterStyle::STANDARD
    }

    fn drop_spelling(&self) -> &'static DropSpelling {
        &DropSpelling::STANDARD
    }

    /// Plain SQL has no statement for it.
    fn enter_scope(&self, _catalog: Option<&str>, _schema: Option<&str>) -> Option<String> {
        None
    }

    fn connect_timeout_property(&self) -> Option<&'static str> {
        None
    }
}
