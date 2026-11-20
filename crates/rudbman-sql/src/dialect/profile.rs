//! User dialect profiles: a product described in data, on top of a built-in
//! dialect or another profile, its base. `docs/dialect-profiles.md` is the
//! design.
//!
//! This module is what a profile *means*, not how it is spelled: a
//! [`ProfileSpec`] is a parsed profile whatever file it came from, [`resolve`]
//! turns a set of them into [`Dialect`]s, and [`ProfileSpec::of`] writes any
//! dialect out as one. Reading and writing YAML is the configuration layer's
//! job, which keeps this crate free of dependencies.
//!
//! A profile answers what it says and asks its base for the rest: a record
//! (`syntax`, `alter`, `drops`, `blocks`) field by field, a word list by adding
//! to the base's, a template by replacing it. A template's `Some(None)` is "this
//! product has none", which is not the same as saying nothing.

use std::cmp::Ordering;
use std::collections::HashMap;
use std::fmt;
use std::sync::RwLock;

use super::{Dialect, DialectId, Syntax, Vendor};
use crate::block::{BeginRule, Family as Block, Routines};
use crate::ddl::{AlterStyle, AttrStyle, ColumnRename, DropStyle, TableRename, Unsupported};
use crate::explain::Explain;
use crate::format::ddl::Family as Layout;
use crate::ident::Folding;
use crate::keywords::WordTables;
use crate::removal::{DropSpelling, RoutineDrop, SchemaDrop, TableTruncate, TriggerDrop};
use crate::script::Client;

// ------------------------------------------------------------------- the spec

/// One profile, parsed: every key optional except the name and the base.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProfileSpec {
    /// The product's name as a person writes it.
    pub name: String,
    /// The dialect this one starts from: a built-in's name or alias, or
    /// another profile's id.
    pub base: String,
    /// Other spellings a driver definition's `dialect` may use for it.
    pub aliases: Vec<String>,
    /// The lexer's rules, merged field by field over the base's.
    pub syntax: SyntaxSpec,
    /// Reserved words added to the base's.
    pub keywords: Vec<String>,
    /// Type names added to the base's.
    pub types: Vec<String>,
    /// Completion's built-in functions added to the base's.
    pub functions: Vec<String>,
    /// What the server does to an unquoted name.
    pub folding: Option<NameFolding>,
    /// Whether names are stored as written and compared without case.
    pub case_insensitive_names: Option<bool>,
    /// Whether a two-part name's qualifier is the database.
    pub catalog_first: Option<bool>,
    /// Whether `USE` changes the database.
    pub uses_catalogs: Option<bool>,
    /// Whose rules a procedural body is read by.
    pub blocks: BlocksSpec,
    /// How the DDL formatter lays a routine out.
    pub routine_layout: Option<RoutineLayout>,
    /// Whose client conventions a script is read by.
    pub client: Option<ScriptClient>,
    /// The page clause, holes `{limit}` and `{offset}`.
    pub page_clause: Option<Option<String>>,
    /// Whether a grid with no page clause reads through a server cursor.
    pub reads_by_server_cursor: Option<bool>,
    /// The statements that read a plan, hole `{sql}`.
    pub explain: Option<Option<ExplainSpec>>,
    /// A row of nothing but defaults, holes `{table}` and `{first_column}`.
    pub empty_insert: Option<Option<String>>,
    /// The `ALTER TABLE` spellings, merged field by field over the base's.
    pub alter: AlterSpec,
    /// Why a refused structure edit is refused, per kind.
    pub unsupported_reasons: Vec<(Unsupported, String)>,
    /// The explorer's `DROP` and `TRUNCATE`, merged field by field.
    pub drops: DropsSpec,
    /// The statement that enters a scope, holes `{catalog}` and `{schema}`.
    pub enter_scope: Option<Option<String>>,
    /// The driver property a login timeout is set by, in milliseconds.
    pub connect_timeout_property: Option<Option<String>>,
}

/// [`Syntax`], every field optional.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[allow(missing_docs)]
pub struct SyntaxSpec {
    pub hash_line_comment: Option<bool>,
    pub dash_dash_needs_space: Option<bool>,
    pub nested_block_comments: Option<bool>,
    pub backslash_escapes: Option<bool>,
    pub e_strings: Option<bool>,
    pub double_quoted_strings: Option<bool>,
    pub backtick_identifiers: Option<bool>,
    pub bracket_identifiers: Option<bool>,
    pub dollar_quotes: Option<bool>,
    pub hex_literals: Option<bool>,
    pub numbered_parameters: Option<bool>,
    pub colon_parameters: Option<bool>,
    pub at_parameters: Option<bool>,
    pub hash_identifiers: Option<bool>,
}

macro_rules! syntax_fields {
    ($m:ident) => {
        $m!(
            hash_line_comment,
            dash_dash_needs_space,
            nested_block_comments,
            backslash_escapes,
            e_strings,
            double_quoted_strings,
            backtick_identifiers,
            bracket_identifiers,
            dollar_quotes,
            hex_literals,
            numbered_parameters,
            colon_parameters,
            at_parameters,
            hash_identifiers
        )
    };
}

impl SyntaxSpec {
    fn over(&self, base: &Syntax) -> Syntax {
        let mut out = *base;
        macro_rules! apply {
            ($($field:ident),*) => {
                $(if let Some(value) = self.$field {
                    out.$field = value;
                })*
            };
        }
        syntax_fields!(apply);
        out
    }

    fn of(syntax: &Syntax) -> Self {
        macro_rules! copy {
            ($($field:ident),*) => {
                Self { $($field: Some(syntax.$field)),* }
            };
        }
        syntax_fields!(copy)
    }
}

/// What a server does to an unquoted name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(missing_docs)]
pub enum NameFolding {
    Upper,
    Lower,
    Preserve,
}

/// The block rules, every field optional.
///
/// `begin` and `routines` belong to the `psm` language only.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[allow(missing_docs)]
pub struct BlocksSpec {
    pub language: Option<BlockLanguage>,
    pub begin: Option<BlockBegin>,
    pub routines: Option<BlockRoutines>,
}

/// The language a procedural body is written in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlockLanguage {
    /// PL/SQL, as Oracle reads it.
    PlSql,
    /// T-SQL, a routine running to the end of its batch.
    TSql,
    /// SQL/PSM, `BEGIN … END` with `CASE` inside it.
    Psm,
}

/// What a statement starting with `BEGIN` is, in the `psm` language.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(missing_docs)]
pub enum BlockBegin {
    Transaction,
    NotAtomic,
    UnlessTransaction,
}

/// What else a body may be written in, in the `psm` language.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(missing_docs)]
pub enum BlockRoutines {
    Psm,
    PlSql,
    Unknown,
}

/// How the DDL formatter lays a routine's body out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(missing_docs)]
pub enum RoutineLayout {
    MySql,
    Oracle,
    MsSql,
    Db2,
    Other,
}

/// Whose client conventions a script is read by.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(missing_docs)]
pub enum ScriptClient {
    Psql,
    Mysql,
    Sqlcmd,
    SqlPlus,
    Clp,
    Sqlite,
    Plain,
}

/// The statements that read a plan, each a template with the hole `{sql}`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[allow(missing_docs)]
pub struct ExplainSpec {
    pub before: Vec<String>,
    pub plan: String,
    pub after: Vec<String>,
    pub shown: String,
}

/// The `ALTER TABLE` spellings, every field optional.
///
/// `check_drop` is the word a check constraint's `DROP` takes, and belongs to
/// the `per_kind` constraint drops only.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[allow(missing_docs)]
pub struct AlterSpec {
    pub add: Option<String>,
    pub default_first: Option<bool>,
    pub named_reference_columns: Option<bool>,
    pub column_rename: Option<ColumnRenameSpec>,
    pub attrs: Option<AttrSpec>,
    pub constraints: Option<ConstraintDrops>,
    pub check_drop: Option<String>,
    pub table_rename: Option<TableRenameSpec>,
    pub reorg: Option<bool>,
}

/// How a column is renamed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(missing_docs)]
pub enum ColumnRenameSpec {
    RenameColumn,
    AlterRenameTo,
    Change,
    SpRename,
}

/// How a column's attributes are changed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(missing_docs)]
pub enum AttrSpec {
    Clauses,
    Restate,
    ModifyChanged,
    TypeWithNull,
    Rebuild,
}

/// How a constraint is dropped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(missing_docs)]
pub enum ConstraintDrops {
    Named,
    PerKind,
    Rebuild,
}

/// How a table is renamed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(missing_docs)]
pub enum TableRenameSpec {
    RenameTo,
    RenameTable,
    SpRename,
}

/// [`DropSpelling`], every field optional.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[allow(missing_docs)]
pub struct DropsSpec {
    pub schema: Option<SchemaDrop>,
    pub database: Option<bool>,
    pub truncate: Option<TableTruncate>,
    pub routines: Option<RoutineDrop>,
    pub triggers: Option<TriggerDrop>,
    pub in_catalog: Option<bool>,
}

/// Why a profile did not load.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProfileError {
    /// The profile's id.
    pub id: String,
    /// The key the error is about, as a dotted path, where there is one.
    pub key: Option<String>,
    /// What is wrong.
    pub message: String,
}

impl ProfileError {
    fn new(id: &str, key: Option<&str>, message: impl Into<String>) -> Self {
        Self {
            id: id.to_string(),
            key: key.map(str::to_string),
            message: message.into(),
        }
    }
}

impl fmt::Display for ProfileError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.key {
            Some(key) => write!(f, "{}: {key}: {}", self.id, self.message),
            None => write!(f, "{}: {}", self.id, self.message),
        }
    }
}

impl std::error::Error for ProfileError {}

// --------------------------------------------------------------- templates

/// A hole a template may name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Hole {
    Limit,
    Offset,
    Sql,
    Table,
    FirstColumn,
    Catalog,
    Schema,
}

impl Hole {
    const ALL: [Self; 7] = [
        Self::Limit,
        Self::Offset,
        Self::Sql,
        Self::Table,
        Self::FirstColumn,
        Self::Catalog,
        Self::Schema,
    ];

    fn name(self) -> &'static str {
        match self {
            Self::Limit => "limit",
            Self::Offset => "offset",
            Self::Sql => "sql",
            Self::Table => "table",
            Self::FirstColumn => "first_column",
            Self::Catalog => "catalog",
            Self::Schema => "schema",
        }
    }
}

#[derive(Debug, Clone)]
enum Part {
    Text(String),
    Hole(Hole),
}

/// A statement with `{hole}`s; `{{` and `}}` are a literal brace.
#[derive(Debug, Clone)]
struct Template {
    parts: Vec<Part>,
}

impl Template {
    /// Parses `text`, refusing a hole outside `allowed`.
    fn parse(text: &str, allowed: &[Hole]) -> Result<Self, String> {
        let mut parts = Vec::new();
        let mut literal = String::new();
        let mut chars = text.chars().peekable();
        while let Some(ch) = chars.next() {
            match ch {
                '{' if chars.peek() == Some(&'{') => {
                    chars.next();
                    literal.push('{');
                }
                '}' if chars.peek() == Some(&'}') => {
                    chars.next();
                    literal.push('}');
                }
                '{' => {
                    let mut name = String::new();
                    loop {
                        match chars.next() {
                            Some('}') => break,
                            Some(c) => name.push(c),
                            None => return Err(format!("`{{{name}` is never closed")),
                        }
                    }
                    let hole = Hole::ALL
                        .into_iter()
                        .find(|hole| hole.name() == name)
                        .filter(|hole| allowed.contains(hole))
                        .ok_or_else(|| {
                            let names: Vec<String> = allowed
                                .iter()
                                .map(|h| format!("{{{}}}", h.name()))
                                .collect();
                            format!(
                                "unknown hole {{{name}}}; this key takes {}",
                                names.join(", ")
                            )
                        })?;
                    if !literal.is_empty() {
                        parts.push(Part::Text(std::mem::take(&mut literal)));
                    }
                    parts.push(Part::Hole(hole));
                }
                '}' => return Err("a lone `}`; write `}}` for a literal brace".to_string()),
                c => literal.push(c),
            }
        }
        if !literal.is_empty() {
            parts.push(Part::Text(literal));
        }
        Ok(Self { parts })
    }

    /// The statement, or `None` when a hole it uses has no value.
    fn render(&self, value: &dyn Fn(Hole) -> Option<String>) -> Option<String> {
        let mut out = String::new();
        for part in &self.parts {
            match part {
                Part::Text(text) => out.push_str(text),
                Part::Hole(hole) => out.push_str(&value(*hole)?),
            }
        }
        Some(out)
    }
}

/// `text` with its braces doubled, so that it reads back as itself.
fn escaped(text: &str) -> String {
    text.replace('{', "{{").replace('}', "}}")
}

#[derive(Debug, Clone)]
struct ExplainTemplates {
    before: Vec<Template>,
    plan: Template,
    after: Vec<Template>,
    shown: Template,
}

// ---------------------------------------------------------------- the vendor

/// A profile's answers, and its base's for everything else.
struct ProfileVendor {
    base: &'static dyn Vendor,
    name: &'static str,
    aliases: &'static [&'static str],
    product: &'static str,
    syntax: &'static Syntax,
    words: WordTables,
    functions: &'static [&'static str],
    folding: Option<Folding>,
    case_insensitive_names: Option<bool>,
    catalog_first: Option<bool>,
    uses_catalogs: Option<bool>,
    block: Block,
    layout: Option<Layout>,
    client: Option<Client>,
    page_clause: Option<Option<Template>>,
    reads_by_server_cursor: Option<bool>,
    explain: Option<Option<ExplainTemplates>>,
    empty_insert: Option<Option<Template>>,
    alter: &'static AlterStyle,
    unsupported: Vec<(Unsupported, &'static str)>,
    drops: &'static DropSpelling,
    enter_scope: Option<Option<Template>>,
    connect_timeout_property: Option<Option<&'static str>>,
}

impl Vendor for ProfileVendor {
    fn id(&self) -> DialectId {
        DialectId::Profile
    }

    fn name(&self) -> &'static str {
        self.name
    }

    fn aliases(&self) -> &'static [&'static str] {
        self.aliases
    }

    fn product(&self) -> &'static str {
        self.product
    }

    fn syntax(&self) -> &'static Syntax {
        self.syntax
    }

    fn words(&self) -> WordTables {
        self.words
    }

    fn functions(&self) -> &'static [&'static str] {
        self.functions
    }

    fn folding(&self) -> Folding {
        self.folding.unwrap_or_else(|| self.base.folding())
    }

    fn case_insensitive_names(&self) -> bool {
        self.case_insensitive_names
            .unwrap_or_else(|| self.base.case_insensitive_names())
    }

    fn catalog_first(&self) -> bool {
        self.catalog_first
            .unwrap_or_else(|| self.base.catalog_first())
    }

    fn uses_catalogs(&self) -> bool {
        self.uses_catalogs
            .unwrap_or_else(|| self.base.uses_catalogs())
    }

    fn block_family(&self) -> Block {
        self.block
    }

    fn routine_layout(&self) -> Layout {
        self.layout.unwrap_or_else(|| self.base.routine_layout())
    }

    fn client(&self) -> Client {
        self.client.unwrap_or_else(|| self.base.client())
    }

    fn page_clause(&self, limit: u64, offset: u64) -> Option<String> {
        match &self.page_clause {
            None => self.base.page_clause(limit, offset),
            Some(None) => None,
            Some(Some(template)) => template.render(&|hole| match hole {
                Hole::Limit => Some(limit.to_string()),
                Hole::Offset => Some(offset.to_string()),
                _ => None,
            }),
        }
    }

    fn reads_by_server_cursor(&self) -> bool {
        self.reads_by_server_cursor
            .unwrap_or_else(|| self.base.reads_by_server_cursor())
    }

    fn explain(&self, sql: &str) -> Option<Explain> {
        match &self.explain {
            None => self.base.explain(sql),
            Some(None) => None,
            Some(Some(e)) => {
                let fill = |hole: Hole| (hole == Hole::Sql).then(|| sql.to_string());
                let all = |templates: &[Template]| {
                    templates
                        .iter()
                        .map(|template| template.render(&fill))
                        .collect::<Option<Vec<_>>>()
                };
                Some(Explain {
                    before: all(&e.before)?,
                    plan: e.plan.render(&fill)?,
                    after: all(&e.after)?,
                    shown: e.shown.render(&fill)?,
                })
            }
        }
    }

    fn empty_insert(&self, table: &str, first_column: Option<&str>) -> Option<String> {
        match &self.empty_insert {
            None => self.base.empty_insert(table, first_column),
            Some(None) => None,
            Some(Some(template)) => template.render(&|hole| match hole {
                Hole::Table => Some(table.to_string()),
                Hole::FirstColumn => first_column.map(str::to_string),
                _ => None,
            }),
        }
    }

    fn alter_style(&self) -> &'static AlterStyle {
        self.alter
    }

    fn unsupported_reason(&self, what: Unsupported) -> Option<&'static str> {
        self.unsupported
            .iter()
            .find(|(kind, _)| *kind == what)
            .map(|(_, reason)| *reason)
            .or_else(|| self.base.unsupported_reason(what))
    }

    fn drop_spelling(&self) -> &'static DropSpelling {
        self.drops
    }

    fn enter_scope(&self, catalog: Option<&str>, schema: Option<&str>) -> Option<String> {
        match &self.enter_scope {
            None => self.base.enter_scope(catalog, schema),
            Some(None) => None,
            Some(Some(template)) => template.render(&|hole| match hole {
                Hole::Catalog => catalog.map(str::to_string),
                Hole::Schema => schema.map(str::to_string),
                _ => None,
            }),
        }
    }

    fn connect_timeout_property(&self) -> Option<&'static str> {
        match self.connect_timeout_property {
            None => self.base.connect_timeout_property(),
            Some(property) => property,
        }
    }
}

// ----------------------------------------------------------------- building

fn leak(text: &str) -> &'static str {
    Box::leak(text.to_string().into_boxed_str())
}

fn leak_all(texts: Vec<String>) -> &'static [&'static str] {
    let leaked: Vec<&'static str> = texts.iter().map(|text| leak(text)).collect();
    Box::leak(leaked.into_boxed_slice())
}

/// `base` with `extra` added, upper-cased, sorted and without repeats, the
/// order the word tables' binary search needs.
fn merged_words(base: &[&str], extra: &[String]) -> Vec<String> {
    let mut words: Vec<String> = base
        .iter()
        .map(|word| word.to_string())
        .chain(extra.iter().map(|word| word.to_ascii_uppercase()))
        .collect();
    words.sort_by(|a, b| cmp_upper(a, b));
    words.dedup();
    words
}

fn cmp_upper(a: &str, b: &str) -> Ordering {
    a.bytes()
        .map(|b| b.to_ascii_uppercase())
        .cmp(b.bytes().map(|b| b.to_ascii_uppercase()))
}

/// What a word list may hold: what the lexer can ever look up.
fn check_words(id: &str, key: &str, words: &[String]) -> Result<(), ProfileError> {
    for word in words {
        let ok = !word.is_empty()
            && word.len() <= 24
            && word.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_');
        if !ok {
            return Err(ProfileError::new(
                id,
                Some(key),
                format!(
                    "`{word}` is not a word the lexer can match: ASCII letters, digits and `_`, at most 24"
                ),
            ));
        }
    }
    Ok(())
}

fn template(id: &str, key: &str, text: &str, allowed: &[Hole]) -> Result<Template, ProfileError> {
    Template::parse(text, allowed).map_err(|message| ProfileError::new(id, Some(key), message))
}

fn optional_template(
    id: &str,
    key: &str,
    value: &Option<Option<String>>,
    allowed: &[Hole],
) -> Result<Option<Option<Template>>, ProfileError> {
    match value {
        None => Ok(None),
        Some(None) => Ok(Some(None)),
        Some(Some(text)) => Ok(Some(Some(template(id, key, text, allowed)?))),
    }
}

fn block_over(id: &str, spec: &BlocksSpec, base: Block) -> Result<Block, ProfileError> {
    let psm_only = |what: &str| {
        ProfileError::new(
            id,
            Some(&format!("blocks.{what}")),
            "belongs to the psm language only",
        )
    };
    let language = spec.language.unwrap_or(match base {
        Block::Oracle => BlockLanguage::PlSql,
        Block::MsSql => BlockLanguage::TSql,
        Block::Psm { .. } => BlockLanguage::Psm,
    });
    match language {
        BlockLanguage::PlSql | BlockLanguage::TSql => {
            if spec.begin.is_some() {
                return Err(psm_only("begin"));
            }
            if spec.routines.is_some() {
                return Err(psm_only("routines"));
            }
            Ok(if language == BlockLanguage::PlSql {
                Block::Oracle
            } else {
                Block::MsSql
            })
        }
        BlockLanguage::Psm => {
            let (base_begin, base_routines) = match base {
                Block::Psm { begin, routines } => (Some(begin), Some(routines)),
                _ => (None, None),
            };
            let begin = spec.begin.map(begin_rule).or(base_begin).ok_or_else(|| {
                ProfileError::new(id, Some("blocks.begin"), "is required for psm on this base")
            })?;
            let routines = spec
                .routines
                .map(routines_rule)
                .or(base_routines)
                .ok_or_else(|| {
                    ProfileError::new(
                        id,
                        Some("blocks.routines"),
                        "is required for psm on this base",
                    )
                })?;
            Ok(Block::Psm { begin, routines })
        }
    }
}

fn begin_rule(begin: BlockBegin) -> BeginRule {
    match begin {
        BlockBegin::Transaction => BeginRule::Transaction,
        BlockBegin::NotAtomic => BeginRule::NotAtomic,
        BlockBegin::UnlessTransaction => BeginRule::UnlessTransaction,
    }
}

fn routines_rule(routines: BlockRoutines) -> Routines {
    match routines {
        BlockRoutines::Psm => Routines::Psm,
        BlockRoutines::PlSql => Routines::PlSql,
        BlockRoutines::Unknown => Routines::Unknown,
    }
}

fn alter_over(id: &str, spec: &AlterSpec, base: &AlterStyle) -> Result<AlterStyle, ProfileError> {
    let mut out = *base;
    if let Some(add) = &spec.add {
        out.add = leak(add);
    }
    if let Some(value) = spec.default_first {
        out.default_first = value;
    }
    if let Some(value) = spec.named_reference_columns {
        out.named_reference_columns = value;
    }
    if let Some(value) = spec.column_rename {
        out.column_rename = match value {
            ColumnRenameSpec::RenameColumn => ColumnRename::RenameColumn,
            ColumnRenameSpec::AlterRenameTo => ColumnRename::AlterRenameTo,
            ColumnRenameSpec::Change => ColumnRename::Change,
            ColumnRenameSpec::SpRename => ColumnRename::SpRename,
        };
    }
    if let Some(value) = spec.attrs {
        out.attrs = match value {
            AttrSpec::Clauses => AttrStyle::Clauses,
            AttrSpec::Restate => AttrStyle::Restate,
            AttrSpec::ModifyChanged => AttrStyle::ModifyChanged,
            AttrSpec::TypeWithNull => AttrStyle::TypeWithNull,
            AttrSpec::Rebuild => AttrStyle::Rebuild,
        };
    }
    let base_check = match base.constraints {
        DropStyle::PerKind { check } => Some(check),
        _ => None,
    };
    let check = || {
        spec.check_drop
            .as_deref()
            .map(leak)
            .or(base_check)
            .ok_or_else(|| {
                ProfileError::new(id, Some("alter.check_drop"), "is required for per_kind")
            })
    };
    out.constraints = match spec.constraints {
        Some(ConstraintDrops::Named) | Some(ConstraintDrops::Rebuild)
            if spec.check_drop.is_some() =>
        {
            return Err(ProfileError::new(
                id,
                Some("alter.check_drop"),
                "belongs to per_kind constraint drops only",
            ));
        }
        Some(ConstraintDrops::Named) => DropStyle::Named,
        Some(ConstraintDrops::Rebuild) => DropStyle::Rebuild,
        Some(ConstraintDrops::PerKind) => DropStyle::PerKind { check: check()? },
        None if spec.check_drop.is_some() => match base.constraints {
            DropStyle::PerKind { .. } => DropStyle::PerKind { check: check()? },
            _ => {
                return Err(ProfileError::new(
                    id,
                    Some("alter.check_drop"),
                    "belongs to per_kind constraint drops only",
                ));
            }
        },
        None => base.constraints,
    };
    if let Some(value) = spec.table_rename {
        out.table_rename = match value {
            TableRenameSpec::RenameTo => TableRename::RenameTo,
            TableRenameSpec::RenameTable => TableRename::RenameTable,
            TableRenameSpec::SpRename => TableRename::SpRename,
        };
    }
    if let Some(value) = spec.reorg {
        out.reorg = value;
    }
    Ok(out)
}

fn drops_over(spec: &DropsSpec, base: &DropSpelling) -> DropSpelling {
    DropSpelling {
        schema: spec.schema.unwrap_or(base.schema),
        database: spec.database.unwrap_or(base.database),
        truncate: spec.truncate.unwrap_or(base.truncate),
        routines: spec.routines.unwrap_or(base.routines),
        triggers: spec.triggers.unwrap_or(base.triggers),
        in_catalog: spec.in_catalog.unwrap_or(base.in_catalog),
    }
}

fn build(id: &str, spec: &ProfileSpec, base: Dialect) -> Result<Dialect, ProfileError> {
    let base = base.vendor();
    if spec.name.trim().is_empty() {
        return Err(ProfileError::new(id, Some("name"), "is required"));
    }
    check_words(id, "keywords", &spec.keywords)?;
    check_words(id, "types", &spec.types)?;
    check_words(id, "functions", &spec.functions)?;
    let base_words = base.words();
    let words = WordTables {
        keywords: leak_all(merged_words(base_words.keywords, &spec.keywords)),
        types: leak_all(merged_words(base_words.types, &spec.types)),
    };
    let mut functions: Vec<String> = base.functions().iter().map(|f| f.to_string()).collect();
    for function in &spec.functions {
        let function = function.to_ascii_uppercase();
        if !functions.contains(&function) {
            functions.push(function);
        }
    }
    let explain = match &spec.explain {
        None => None,
        Some(None) => Some(None),
        Some(Some(e)) => {
            let one = |key: &str, text: &str| template(id, key, text, &[Hole::Sql]);
            Some(Some(ExplainTemplates {
                before: e
                    .before
                    .iter()
                    .map(|text| one("explain.before", text))
                    .collect::<Result<_, _>>()?,
                plan: one("explain.plan", &e.plan)?,
                after: e
                    .after
                    .iter()
                    .map(|text| one("explain.after", text))
                    .collect::<Result<_, _>>()?,
                shown: one("explain.shown", &e.shown)?,
            }))
        }
    };
    let vendor = ProfileVendor {
        base,
        name: leak(id),
        aliases: leak_all(spec.aliases.clone()),
        product: leak(spec.name.trim()),
        syntax: Box::leak(Box::new(spec.syntax.over(base.syntax()))),
        words,
        functions: leak_all(functions),
        folding: spec.folding.map(|folding| match folding {
            NameFolding::Upper => Folding::Upper,
            NameFolding::Lower => Folding::Lower,
            NameFolding::Preserve => Folding::Preserve,
        }),
        case_insensitive_names: spec.case_insensitive_names,
        catalog_first: spec.catalog_first,
        uses_catalogs: spec.uses_catalogs,
        block: block_over(id, &spec.blocks, base.block_family())?,
        layout: spec.routine_layout.map(|layout| match layout {
            RoutineLayout::MySql => Layout::MySql,
            RoutineLayout::Oracle => Layout::Oracle,
            RoutineLayout::MsSql => Layout::MsSql,
            RoutineLayout::Db2 => Layout::Db2,
            RoutineLayout::Other => Layout::Other,
        }),
        client: spec.client.map(|client| match client {
            ScriptClient::Psql => Client::Psql,
            ScriptClient::Mysql => Client::Mysql,
            ScriptClient::Sqlcmd => Client::Sqlcmd,
            ScriptClient::SqlPlus => Client::SqlPlus,
            ScriptClient::Clp => Client::Clp,
            ScriptClient::Sqlite => Client::Sqlite,
            ScriptClient::Plain => Client::Plain,
        }),
        page_clause: optional_template(
            id,
            "page_clause",
            &spec.page_clause,
            &[Hole::Limit, Hole::Offset],
        )?,
        reads_by_server_cursor: spec.reads_by_server_cursor,
        explain,
        empty_insert: optional_template(
            id,
            "empty_insert",
            &spec.empty_insert,
            &[Hole::Table, Hole::FirstColumn],
        )?,
        alter: Box::leak(Box::new(alter_over(id, &spec.alter, base.alter_style())?)),
        unsupported: spec
            .unsupported_reasons
            .iter()
            .map(|(kind, reason)| (*kind, leak(reason)))
            .collect(),
        drops: Box::leak(Box::new(drops_over(&spec.drops, base.drop_spelling()))),
        enter_scope: optional_template(
            id,
            "enter_scope",
            &spec.enter_scope,
            &[Hole::Catalog, Hole::Schema],
        )?,
        connect_timeout_property: spec
            .connect_timeout_property
            .as_ref()
            .map(|property| property.as_deref().map(leak)),
    };
    Ok(Dialect {
        vendor: Box::leak(Box::new(vendor)),
    })
}

// --------------------------------------------------------------- resolving

/// Whether `id` may name a profile: lower-case ASCII letters, digits, `-` and
/// `_`, and no built-in's name or alias.
fn check_id(id: &str, key: Option<&str>, owner: &str) -> Result<(), ProfileError> {
    let well_formed = !id.is_empty()
        && id
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'_');
    if !well_formed {
        return Err(ProfileError::new(
            owner,
            key,
            format!("`{id}` is not an id: lower-case ASCII letters, digits, `-` and `_`"),
        ));
    }
    if builtin(id).is_some() {
        return Err(ProfileError::new(
            owner,
            key,
            format!(
                "`{id}` is a built-in dialect's; give the profile an id of its own and `{id}` as its base"
            ),
        ));
    }
    Ok(())
}

/// The built-in dialect a name or alias spells, ignoring ASCII case.
fn builtin(name: &str) -> Option<Dialect> {
    Dialect::ALL.iter().copied().find(|dialect| {
        dialect.name().eq_ignore_ascii_case(name)
            || dialect
                .vendor()
                .aliases()
                .iter()
                .any(|alias| alias.eq_ignore_ascii_case(name))
    })
}

/// Turns a set of profiles into dialects, each on its base: a built-in or a
/// profile in the same set.
///
/// The answers come back in the order the profiles were given. A profile
/// fails on its own errors and on its base's, and a cycle of bases fails every
/// profile in it. Nothing is registered: see [`install`].
pub fn resolve(
    profiles: Vec<(String, ProfileSpec)>,
) -> Vec<(String, Result<Dialect, ProfileError>)> {
    let mut index: HashMap<String, usize> = HashMap::new();
    let mut spelled: HashMap<String, String> = HashMap::new();
    let mut early: Vec<Option<ProfileError>> = vec![None; profiles.len()];
    for (at, (id, spec)) in profiles.iter().enumerate() {
        if let Err(error) = check_id(id, None, id) {
            early[at] = Some(error);
            continue;
        }
        if index.contains_key(id) || spelled.contains_key(id) {
            early[at] = Some(ProfileError::new(id, None, "is defined twice"));
            continue;
        }
        let mut alias_error = None;
        for alias in &spec.aliases {
            if let Err(error) = check_id(alias, Some("aliases"), id) {
                alias_error = Some(error);
                break;
            }
            if let Some(other) = spelled
                .get(alias)
                .cloned()
                .or_else(|| index.contains_key(alias).then(|| alias.clone()))
            {
                alias_error = Some(ProfileError::new(
                    id,
                    Some("aliases"),
                    format!("`{alias}` already names `{other}`"),
                ));
                break;
            }
        }
        if let Some(error) = alias_error {
            early[at] = Some(error);
            continue;
        }
        index.insert(id.clone(), at);
        for alias in &spec.aliases {
            spelled.insert(alias.clone(), id.clone());
        }
    }

    struct Walk<'a> {
        profiles: &'a [(String, ProfileSpec)],
        index: &'a HashMap<String, usize>,
        failed_early: &'a [bool],
        done: Vec<Option<Result<Dialect, ProfileError>>>,
        stack: Vec<usize>,
        in_cycle: Vec<bool>,
    }

    impl Walk<'_> {
        fn visit(&mut self, at: usize) -> Result<Dialect, ProfileError> {
            if let Some(result) = &self.done[at] {
                return result.clone();
            }
            if let Some(from) = self.stack.iter().position(|&on| on == at) {
                for &member in &self.stack[from..] {
                    self.in_cycle[member] = true;
                }
                return Err(self.cycle(at));
            }
            let (id, spec) = &self.profiles[at];
            self.stack.push(at);
            let base = if spec.base.trim().is_empty() {
                Err(ProfileError::new(id, Some("base"), "is required"))
            } else if let Some(dialect) = builtin(&spec.base) {
                Ok(dialect)
            } else if let Some(&other) = self.index.get(&spec.base) {
                self.visit(other).map_err(|error| {
                    if self.in_cycle[at] {
                        self.cycle(at)
                    } else {
                        ProfileError::new(
                            id,
                            Some("base"),
                            format!("`{}` did not load: {error}", spec.base),
                        )
                    }
                })
            } else if self
                .profiles
                .iter()
                .zip(self.failed_early)
                .any(|((other, _), &failed)| failed && *other == spec.base)
            {
                Err(ProfileError::new(
                    id,
                    Some("base"),
                    format!("`{}` did not load", spec.base),
                ))
            } else {
                Err(ProfileError::new(
                    id,
                    Some("base"),
                    format!("`{}` names no dialect", spec.base),
                ))
            };
            self.stack.pop();
            let result = if self.in_cycle[at] {
                Err(self.cycle(at))
            } else {
                base.and_then(|base| build(id, spec, base))
            };
            self.done[at] = Some(result.clone());
            result
        }

        fn cycle(&self, at: usize) -> ProfileError {
            ProfileError::new(
                &self.profiles[at].0,
                Some("base"),
                "is part of a cycle of bases",
            )
        }
    }

    let failed_early: Vec<bool> = early.iter().map(Option::is_some).collect();
    let mut walk = Walk {
        profiles: &profiles,
        index: &index,
        failed_early: &failed_early,
        done: vec![None; profiles.len()],
        stack: Vec::new(),
        in_cycle: vec![false; profiles.len()],
    };
    let mut out = Vec::with_capacity(profiles.len());
    for at in 0..profiles.len() {
        let result = match early[at].take() {
            Some(error) => Err(error),
            None => walk.visit(at),
        };
        out.push((profiles[at].0.clone(), result));
    }
    out
}

/// The profiles [`Dialect::from_id`] finds after the built-ins.
static PROFILES: RwLock<Vec<Dialect>> = RwLock::new(Vec::new());

/// Makes `dialects` the registered profiles, in place of any registered
/// before.
///
/// A dialect a caller still holds from the earlier set keeps working: its
/// answers were leaked to live as long as the process, which is the price of
/// handing out `Copy` dialects and is a few kilobytes a reload.
pub fn install(dialects: Vec<Dialect>) {
    let mut profiles = PROFILES
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    *profiles = dialects;
}

/// The registered profiles, in the order they were installed.
pub fn installed() -> Vec<Dialect> {
    PROFILES
        .read()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clone()
}

/// The registered profile a name or alias spells, ignoring ASCII case.
pub(crate) fn find(name: &str) -> Option<Dialect> {
    PROFILES
        .read()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .iter()
        .copied()
        .find(|dialect| {
            dialect.name().eq_ignore_ascii_case(name)
                || dialect
                    .vendor()
                    .aliases()
                    .iter()
                    .any(|alias| alias.eq_ignore_ascii_case(name))
        })
}

// ---------------------------------------------------------------- exporting

// Values a probe hands a vendor so that what it writes can be read back as a
// template: numbers and strings no real statement contains.
const LIMIT_PROBE: u64 = 7_100_000_000_000_017;
const OFFSET_PROBE: u64 = 8_200_000_000_000_028;
const SQL_PROBE: &str = "\u{E000}sql\u{E001}";
const TABLE_PROBE: &str = "\u{E000}table\u{E001}";
const COLUMN_PROBE: &str = "\u{E000}column\u{E001}";
const CATALOG_PROBE: &str = "\u{E000}catalog\u{E001}";
const SCHEMA_PROBE: &str = "\u{E000}schema\u{E001}";

/// `text` as a template: its braces escaped and each probe a hole.
fn holes(text: &str, probes: &[(&str, Hole)]) -> String {
    let mut out = escaped(text);
    for (probe, hole) in probes {
        out = out.replace(probe, &format!("{{{}}}", hole.name()));
    }
    out
}

impl ProfileSpec {
    /// `dialect` written out with every key filled in, on `generic`: the
    /// starting point for a profile of a product like it.
    ///
    /// The templates are read off what the dialect writes for values no real
    /// statement contains. A built-in's aliases are left out, since they are
    /// reserved; a profile's are kept.
    pub fn of(dialect: Dialect) -> Self {
        let vendor = dialect.vendor();
        let numbers = [
            (LIMIT_PROBE.to_string(), Hole::Limit),
            (OFFSET_PROBE.to_string(), Hole::Offset),
        ];
        let numbers: Vec<(&str, Hole)> = numbers.iter().map(|(p, h)| (p.as_str(), *h)).collect();
        let words = vendor.words();
        Self {
            name: vendor.product().to_string(),
            base: Dialect::GENERIC.name().to_string(),
            aliases: if dialect.id() == DialectId::Profile {
                vendor.aliases().iter().map(|a| a.to_string()).collect()
            } else {
                Vec::new()
            },
            syntax: SyntaxSpec::of(vendor.syntax()),
            keywords: words.keywords.iter().map(|w| w.to_string()).collect(),
            types: words.types.iter().map(|w| w.to_string()).collect(),
            functions: vendor.functions().iter().map(|f| f.to_string()).collect(),
            folding: Some(match vendor.folding() {
                Folding::Upper => NameFolding::Upper,
                Folding::Lower => NameFolding::Lower,
                Folding::Preserve => NameFolding::Preserve,
            }),
            case_insensitive_names: Some(vendor.case_insensitive_names()),
            catalog_first: Some(vendor.catalog_first()),
            uses_catalogs: Some(vendor.uses_catalogs()),
            blocks: match vendor.block_family() {
                Block::Oracle => BlocksSpec {
                    language: Some(BlockLanguage::PlSql),
                    ..BlocksSpec::default()
                },
                Block::MsSql => BlocksSpec {
                    language: Some(BlockLanguage::TSql),
                    ..BlocksSpec::default()
                },
                Block::Psm { begin, routines } => BlocksSpec {
                    language: Some(BlockLanguage::Psm),
                    begin: Some(match begin {
                        BeginRule::Transaction => BlockBegin::Transaction,
                        BeginRule::NotAtomic => BlockBegin::NotAtomic,
                        BeginRule::UnlessTransaction => BlockBegin::UnlessTransaction,
                    }),
                    routines: Some(match routines {
                        Routines::Psm => BlockRoutines::Psm,
                        Routines::PlSql => BlockRoutines::PlSql,
                        Routines::Unknown => BlockRoutines::Unknown,
                    }),
                },
            },
            routine_layout: Some(match vendor.routine_layout() {
                Layout::MySql => RoutineLayout::MySql,
                Layout::Oracle => RoutineLayout::Oracle,
                Layout::MsSql => RoutineLayout::MsSql,
                Layout::Db2 => RoutineLayout::Db2,
                Layout::Other => RoutineLayout::Other,
            }),
            client: Some(match vendor.client() {
                Client::Psql => ScriptClient::Psql,
                Client::Mysql => ScriptClient::Mysql,
                Client::Sqlcmd => ScriptClient::Sqlcmd,
                Client::SqlPlus => ScriptClient::SqlPlus,
                Client::Clp => ScriptClient::Clp,
                Client::Sqlite => ScriptClient::Sqlite,
                Client::Plain => ScriptClient::Plain,
            }),
            page_clause: Some(
                vendor
                    .page_clause(LIMIT_PROBE, OFFSET_PROBE)
                    .map(|text| holes(&text, &numbers)),
            ),
            reads_by_server_cursor: Some(vendor.reads_by_server_cursor()),
            explain: Some(vendor.explain(SQL_PROBE).map(|e| {
                let probes = [(SQL_PROBE, Hole::Sql)];
                ExplainSpec {
                    before: e.before.iter().map(|s| holes(s, &probes)).collect(),
                    plan: holes(&e.plan, &probes),
                    after: e.after.iter().map(|s| holes(s, &probes)).collect(),
                    shown: holes(&e.shown, &probes),
                }
            })),
            empty_insert: Some(
                vendor
                    .empty_insert(TABLE_PROBE, Some(COLUMN_PROBE))
                    .or_else(|| vendor.empty_insert(TABLE_PROBE, None))
                    .map(|text| {
                        holes(
                            &text,
                            &[
                                (TABLE_PROBE, Hole::Table),
                                (COLUMN_PROBE, Hole::FirstColumn),
                            ],
                        )
                    }),
            ),
            alter: {
                let alter = vendor.alter_style();
                AlterSpec {
                    add: Some(alter.add.to_string()),
                    default_first: Some(alter.default_first),
                    named_reference_columns: Some(alter.named_reference_columns),
                    column_rename: Some(match alter.column_rename {
                        ColumnRename::RenameColumn => ColumnRenameSpec::RenameColumn,
                        ColumnRename::AlterRenameTo => ColumnRenameSpec::AlterRenameTo,
                        ColumnRename::Change => ColumnRenameSpec::Change,
                        ColumnRename::SpRename => ColumnRenameSpec::SpRename,
                    }),
                    attrs: Some(match alter.attrs {
                        AttrStyle::Clauses => AttrSpec::Clauses,
                        AttrStyle::Restate => AttrSpec::Restate,
                        AttrStyle::ModifyChanged => AttrSpec::ModifyChanged,
                        AttrStyle::TypeWithNull => AttrSpec::TypeWithNull,
                        AttrStyle::Rebuild => AttrSpec::Rebuild,
                    }),
                    constraints: Some(match alter.constraints {
                        DropStyle::Named => ConstraintDrops::Named,
                        DropStyle::PerKind { .. } => ConstraintDrops::PerKind,
                        DropStyle::Rebuild => ConstraintDrops::Rebuild,
                    }),
                    check_drop: match alter.constraints {
                        DropStyle::PerKind { check } => Some(check.to_string()),
                        _ => None,
                    },
                    table_rename: Some(match alter.table_rename {
                        TableRename::RenameTo => TableRenameSpec::RenameTo,
                        TableRename::RenameTable => TableRenameSpec::RenameTable,
                        TableRename::SpRename => TableRenameSpec::SpRename,
                    }),
                    reorg: Some(alter.reorg),
                }
            },
            unsupported_reasons: Unsupported::ALL
                .into_iter()
                .filter_map(|what| {
                    vendor
                        .unsupported_reason(what)
                        .map(|r| (what, r.to_string()))
                })
                .collect(),
            drops: {
                let drops = vendor.drop_spelling();
                DropsSpec {
                    schema: Some(drops.schema),
                    database: Some(drops.database),
                    truncate: Some(drops.truncate),
                    routines: Some(drops.routines),
                    triggers: Some(drops.triggers),
                    in_catalog: Some(drops.in_catalog),
                }
            },
            enter_scope: Some(
                vendor
                    .enter_scope(Some(CATALOG_PROBE), Some(SCHEMA_PROBE))
                    .map(|text| {
                        holes(
                            &text,
                            &[(CATALOG_PROBE, Hole::Catalog), (SCHEMA_PROBE, Hole::Schema)],
                        )
                    }),
            ),
            connect_timeout_property: Some(vendor.connect_timeout_property().map(str::to_string)),
        }
    }
}

#[cfg(test)]
mod tests;
