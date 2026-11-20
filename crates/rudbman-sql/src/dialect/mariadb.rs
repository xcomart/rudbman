//! MariaDB: MySQL's lexis, and its own `DROP CONSTRAINT`.
//!
//! The fork kept MySQL's lexical rules, words and most of its grammar, and
//! every answer it shares is read from [`MySql`] rather than copied, so the two
//! cannot drift apart by accident. What is written here is where they part.

use super::mysql::{self, MySql};
use super::{DialectId, Syntax, Vendor};
use crate::block::{BeginRule, Family as Block, Routines};
use crate::ddl::{AlterStyle, DropStyle, Unsupported};
use crate::explain::Explain;
use crate::format::ddl::Family as Layout;
use crate::ident::Folding;
use crate::keywords::WordTables;
use crate::removal::DropSpelling;
use crate::script::Client;

/// MariaDB.
pub(crate) struct MariaDb;

/// MariaDB: MySQL's row with one word changed.
///
/// The fork kept the grammar down to `CHANGE COLUMN` and the reference it
/// will not resolve, and parted company at the check constraint alone —
/// `ALTER TABLE t DROP CHECK c` is SQLSTATE 42000 on a MariaDB 11, which
/// spells it `DROP CONSTRAINT c` and has since 10.2.22. Both spellings are
/// exclusive to their product, so this is a row rather than a widened
/// MySQL one: MySQL 8.0.16 through 8.0.18 have no `DROP CONSTRAINT` to
/// share.
const ALTER: AlterStyle = AlterStyle {
    constraints: DropStyle::PerKind {
        check: "CONSTRAINT",
    },
    ..mysql::ALTER
};

impl Vendor for MariaDb {
    fn id(&self) -> DialectId {
        DialectId::MariaDb
    }

    fn name(&self) -> &'static str {
        "mariadb"
    }

    fn product(&self) -> &'static str {
        "MariaDB"
    }

    fn syntax(&self) -> &'static Syntax {
        MySql.syntax()
    }

    fn words(&self) -> WordTables {
        MySql.words()
    }

    fn functions(&self) -> &'static [&'static str] {
        MySql.functions()
    }

    fn folding(&self) -> Folding {
        MySql.folding()
    }

    fn case_insensitive_names(&self) -> bool {
        MySql.case_insensitive_names()
    }

    fn catalog_first(&self) -> bool {
        MySql.catalog_first()
    }

    fn uses_catalogs(&self) -> bool {
        MySql.uses_catalogs()
    }

    /// MySQL's `BEGIN NOT ATOMIC`, and PL/SQL bodies under
    /// `sql_mode=ORACLE`.
    fn block_family(&self) -> Block {
        Block::Psm {
            begin: BeginRule::NotAtomic,
            routines: Routines::PlSql,
        }
    }

    fn routine_layout(&self) -> Layout {
        MySql.routine_layout()
    }

    fn client(&self) -> Client {
        MySql.client()
    }

    fn page_clause(&self, limit: u64, offset: u64) -> Option<String> {
        MySql.page_clause(limit, offset)
    }

    fn reads_by_server_cursor(&self) -> bool {
        MySql.reads_by_server_cursor()
    }

    fn explain(&self, sql: &str) -> Option<Explain> {
        MySql.explain(sql)
    }

    /// MariaDB inherited MySQL's refusal of `DEFAULT VALUES` along with the
    /// grammar, so it takes MySQL's two empty lists rather than the standard
    /// form neither accepts.
    fn empty_insert(&self, table: &str, first_column: Option<&str>) -> Option<String> {
        MySql.empty_insert(table, first_column)
    }

    fn alter_style(&self) -> &'static AlterStyle {
        &ALTER
    }

    fn unsupported_reason(&self, what: Unsupported) -> Option<&'static str> {
        MySql.unsupported_reason(what)
    }

    fn drop_spelling(&self) -> &'static DropSpelling {
        MySql.drop_spelling()
    }

    fn enter_scope(&self, catalog: Option<&str>, schema: Option<&str>) -> Option<String> {
        MySql.enter_scope(catalog, schema)
    }

    /// MariaDB Connector/J reads Connector/J's property.
    fn connect_timeout_property(&self) -> Option<&'static str> {
        MySql.connect_timeout_property()
    }
}
