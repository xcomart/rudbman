//! User dialect profiles as YAML: `<config>/dialects/<id>.yml`.
//!
//! What a profile means lives in `rudbman-sql`
//! ([`ProfileSpec`](rudbman_sql::dialect::profile::ProfileSpec)), which has
//! no dependencies by design; this module is only its spelling, the format
//! `docs/dialect-profiles.md` fixes. Every key is checked: an unknown one is
//! an error with its line, since a typo silently ignored is a setting silently
//! lost.
//!
//! The `bridge` half is read and kept as it stands, for the bridge to take
//! when it learns profiles; nothing here applies it.

use std::fmt;

use rudbman_sql::dialect::profile::{
    AlterSpec, AttrSpec, BlockBegin, BlockLanguage, BlockRoutines, BlocksSpec, ColumnRenameSpec,
    ConstraintDrops, DropsSpec, ExplainSpec, NameFolding, ProfileSpec, RoutineLayout, ScriptClient,
    SyntaxSpec, TableRenameSpec,
};
use rudbman_sql::{RoutineDrop, SchemaDrop, TableTruncate, TriggerDrop, Unsupported};
use serde::{Deserialize, Deserializer, Serialize};

/// The version of the format this build reads and writes.
pub const API: u32 = 1;

/// A profile file, read.
#[derive(Debug, Clone, PartialEq)]
pub struct ProfileFile {
    /// What the profile says rudbman-sql should do.
    pub spec: ProfileSpec,
    /// The `bridge` half, untouched.
    pub bridge: Option<serde_norway::Value>,
}

/// Why a profile file could not be read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseError {
    /// The profile's id.
    pub id: String,
    /// The line the error is on, from 1, where the parser knows it.
    pub line: Option<usize>,
    /// The column, from 1, with the line.
    pub column: Option<usize>,
    /// What is wrong.
    pub message: String,
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.yml", self.id)?;
        if let Some(line) = self.line {
            write!(f, ":{line}")?;
            if let Some(column) = self.column {
                write!(f, ":{column}")?;
            }
        }
        write!(f, ": {}", self.message)
    }
}

impl std::error::Error for ParseError {}

/// Reads the profile `id` from `text`.
///
/// # Errors
///
/// A [`ParseError`] for text that is not YAML, a key the format does not
/// have, a value of the wrong kind, or an `api` this build does not read.
/// Whether the profile *loads* — its base, its templates, its words — is
/// [`resolve`](rudbman_sql::dialect::profile::resolve)'s to say.
pub fn parse(id: &str, text: &str) -> Result<ProfileFile, ParseError> {
    let file: YamlProfile = serde_norway::from_str(text).map_err(|error| {
        let location = error.location();
        ParseError {
            id: id.to_string(),
            line: location.as_ref().map(|at| at.line()),
            column: location.as_ref().map(|at| at.column()),
            message: error.to_string(),
        }
    })?;
    if file.api != API {
        return Err(ParseError {
            id: id.to_string(),
            line: None,
            column: None,
            message: format!(
                "api {} is not a version of the format this build reads, which is {API}",
                file.api
            ),
        });
    }
    let bridge = file.bridge.clone();
    Ok(ProfileFile {
        spec: file.into_spec(),
        bridge,
    })
}

/// Writes `spec` as a profile file, every key it holds and none it does not.
pub fn to_yaml(spec: &ProfileSpec) -> String {
    serde_norway::to_string(&YamlProfile::of(spec)).unwrap_or_else(|error| {
        // Every value here is a string, a bool, a list or a map of those.
        unreachable!("a profile always serializes: {error}")
    })
}

// ---------------------------------------------------------------- the format

/// `Some(None)` for an explicit `null`, `None` for a key left out.
fn null_or<'de, D, T>(deserializer: D) -> Result<Option<Option<T>>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<T>::deserialize(deserializer).map(Some)
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct YamlProfile {
    api: u32,
    name: String,
    base: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    aliases: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    sql: Option<YamlSql>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    bridge: Option<serde_norway::Value>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct YamlSql {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    syntax: Option<YamlSyntax>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    words: Option<YamlWords>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    functions: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    folding: Option<YFolding>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    case_insensitive_names: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    catalog_first: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    uses_catalogs: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    blocks: Option<YamlBlocks>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    routine_layout: Option<YLayout>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    client: Option<YClient>,
    #[serde(
        default,
        deserialize_with = "null_or",
        skip_serializing_if = "Option::is_none"
    )]
    page_clause: Option<Option<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    reads_by_server_cursor: Option<bool>,
    #[serde(
        default,
        deserialize_with = "null_or",
        skip_serializing_if = "Option::is_none"
    )]
    explain: Option<Option<YamlExplain>>,
    #[serde(
        default,
        deserialize_with = "null_or",
        skip_serializing_if = "Option::is_none"
    )]
    empty_insert: Option<Option<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    alter: Option<YamlAlter>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    unsupported_reasons: Option<YamlReasons>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    drops: Option<YamlDrops>,
    #[serde(
        default,
        deserialize_with = "null_or",
        skip_serializing_if = "Option::is_none"
    )]
    enter_scope: Option<Option<String>>,
    #[serde(
        default,
        deserialize_with = "null_or",
        skip_serializing_if = "Option::is_none"
    )]
    connect_timeout_property: Option<Option<String>>,
}

macro_rules! yaml_syntax {
    ($($field:ident),* $(,)?) => {
        #[derive(Debug, Default, Serialize, Deserialize)]
        #[serde(deny_unknown_fields)]
        struct YamlSyntax {
            $(
                #[serde(default, skip_serializing_if = "Option::is_none")]
                $field: Option<bool>,
            )*
        }

        impl YamlSyntax {
            fn into_spec(self) -> SyntaxSpec {
                SyntaxSpec { $($field: self.$field),* }
            }

            fn of(spec: &SyntaxSpec) -> Option<Self> {
                let out = Self { $($field: spec.$field),* };
                (out != Self::default()).then_some(out)
            }
        }

        impl PartialEq for YamlSyntax {
            fn eq(&self, other: &Self) -> bool {
                true $(&& self.$field == other.$field)*
            }
        }
    };
}

yaml_syntax!(
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
    hash_identifiers,
);

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct YamlWords {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    keywords: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    types: Vec<String>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct YamlBlocks {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    language: Option<YLanguage>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    begin: Option<YBegin>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    routines: Option<YRoutines>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct YamlExplain {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    before: Vec<String>,
    plan: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    after: Vec<String>,
    shown: String,
}

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct YamlAlter {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    add: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    default_first: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    named_reference_columns: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    column_rename: Option<YColumnRename>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    attrs: Option<YAttrs>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    constraints: Option<YConstraints>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    check_drop: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    table_rename: Option<YTableRename>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    reorg: Option<bool>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct YamlReasons {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    column_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    nullability: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    default: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    constraint_drop: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    reference_without_columns: Option<String>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct YamlDrops {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    schema: Option<YSchemaDrop>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    database: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    truncate: Option<YTruncate>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    routines: Option<YRoutineDrop>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    triggers: Option<YTriggerDrop>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    in_catalog: Option<bool>,
}

/// A YAML spelling for each variant of one of `rudbman-sql`'s enums, and the
/// conversions both ways; the matches are exhaustive, so a variant added
/// there fails to compile here until it has a spelling.
macro_rules! spelled {
    ($yaml:ident <=> $spec:ident { $($variant:ident = $name:literal),* $(,)? }) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
        enum $yaml {
            $(#[serde(rename = $name)] $variant),*
        }

        impl From<$yaml> for $spec {
            fn from(value: $yaml) -> Self {
                match value {
                    $($yaml::$variant => $spec::$variant),*
                }
            }
        }

        impl From<$spec> for $yaml {
            fn from(value: $spec) -> Self {
                match value {
                    $($spec::$variant => $yaml::$variant),*
                }
            }
        }
    };
}

spelled!(YFolding <=> NameFolding { Upper = "upper", Lower = "lower", Preserve = "preserve" });
spelled!(YLanguage <=> BlockLanguage { PlSql = "plsql", TSql = "tsql", Psm = "psm" });
spelled!(YBegin <=> BlockBegin {
    Transaction = "transaction",
    NotAtomic = "not_atomic",
    UnlessTransaction = "unless_transaction",
});
spelled!(YRoutines <=> BlockRoutines { Psm = "psm", PlSql = "plsql", Unknown = "unknown" });
spelled!(YLayout <=> RoutineLayout {
    MySql = "mysql",
    Oracle = "oracle",
    MsSql = "mssql",
    Db2 = "db2",
    Other = "other",
});
spelled!(YClient <=> ScriptClient {
    Psql = "psql",
    Mysql = "mysql",
    Sqlcmd = "sqlcmd",
    SqlPlus = "sqlplus",
    Clp = "clp",
    Sqlite = "sqlite",
    Plain = "plain",
});
spelled!(YColumnRename <=> ColumnRenameSpec {
    RenameColumn = "rename_column",
    AlterRenameTo = "alter_rename_to",
    Change = "change",
    SpRename = "sp_rename",
});
spelled!(YAttrs <=> AttrSpec {
    Clauses = "clauses",
    Restate = "restate",
    ModifyChanged = "modify_changed",
    TypeWithNull = "type_with_null",
    Rebuild = "rebuild",
});
spelled!(YConstraints <=> ConstraintDrops {
    Named = "named",
    PerKind = "per_kind",
    Rebuild = "rebuild",
});
spelled!(YTableRename <=> TableRenameSpec {
    RenameTo = "rename_to",
    RenameTable = "rename_table",
    SpRename = "sp_rename",
});
spelled!(YSchemaDrop <=> SchemaDrop {
    None = "none",
    Schema = "schema",
    SchemaRestrict = "schema_restrict",
    User = "user",
    Database = "database",
});
spelled!(YTruncate <=> TableTruncate {
    Truncate = "truncate",
    Immediate = "immediate",
    DeleteAll = "delete_all",
});
spelled!(YRoutineDrop <=> RoutineDrop {
    None = "none",
    Named = "named",
    Alias = "alias",
    Overloaded = "overloaded",
    Specific = "specific",
    Packaged = "packaged",
});
spelled!(YTriggerDrop <=> TriggerDrop { Named = "named", OnTable = "on_table" });

// ------------------------------------------------------------ conversions

impl YamlProfile {
    fn into_spec(self) -> ProfileSpec {
        let sql = self.sql.unwrap_or_default();
        let words = sql.words.unwrap_or_default();
        let blocks = sql.blocks.unwrap_or_default();
        let alter = sql.alter.unwrap_or_default();
        let reasons = sql.unsupported_reasons.unwrap_or_default();
        let drops = sql.drops.unwrap_or_default();
        ProfileSpec {
            name: self.name,
            base: self.base,
            aliases: self.aliases,
            syntax: sql.syntax.map(YamlSyntax::into_spec).unwrap_or_default(),
            keywords: words.keywords,
            types: words.types,
            functions: sql.functions,
            folding: sql.folding.map(Into::into),
            case_insensitive_names: sql.case_insensitive_names,
            catalog_first: sql.catalog_first,
            uses_catalogs: sql.uses_catalogs,
            blocks: BlocksSpec {
                language: blocks.language.map(Into::into),
                begin: blocks.begin.map(Into::into),
                routines: blocks.routines.map(Into::into),
            },
            routine_layout: sql.routine_layout.map(Into::into),
            client: sql.client.map(Into::into),
            page_clause: sql.page_clause,
            reads_by_server_cursor: sql.reads_by_server_cursor,
            explain: sql.explain.map(|explain| {
                explain.map(|e| ExplainSpec {
                    before: e.before,
                    plan: e.plan,
                    after: e.after,
                    shown: e.shown,
                })
            }),
            empty_insert: sql.empty_insert,
            alter: AlterSpec {
                add: alter.add,
                default_first: alter.default_first,
                named_reference_columns: alter.named_reference_columns,
                column_rename: alter.column_rename.map(Into::into),
                attrs: alter.attrs.map(Into::into),
                constraints: alter.constraints.map(Into::into),
                check_drop: alter.check_drop,
                table_rename: alter.table_rename.map(Into::into),
                reorg: alter.reorg,
            },
            unsupported_reasons: [
                (Unsupported::ColumnType, reasons.column_type),
                (Unsupported::Nullability, reasons.nullability),
                (Unsupported::Default, reasons.default),
                (Unsupported::ConstraintDrop, reasons.constraint_drop),
                (
                    Unsupported::ReferenceWithoutColumns,
                    reasons.reference_without_columns,
                ),
            ]
            .into_iter()
            .filter_map(|(what, reason)| reason.map(|reason| (what, reason)))
            .collect(),
            drops: DropsSpec {
                schema: drops.schema.map(Into::into),
                database: drops.database,
                truncate: drops.truncate.map(Into::into),
                routines: drops.routines.map(Into::into),
                triggers: drops.triggers.map(Into::into),
                in_catalog: drops.in_catalog,
            },
            enter_scope: sql.enter_scope,
            connect_timeout_property: sql.connect_timeout_property,
        }
    }

    fn of(spec: &ProfileSpec) -> Self {
        let reason = |what: Unsupported| {
            spec.unsupported_reasons
                .iter()
                .find(|(kind, _)| *kind == what)
                .map(|(_, reason)| reason.clone())
        };
        let reasons = YamlReasons {
            column_type: reason(Unsupported::ColumnType),
            nullability: reason(Unsupported::Nullability),
            default: reason(Unsupported::Default),
            constraint_drop: reason(Unsupported::ConstraintDrop),
            reference_without_columns: reason(Unsupported::ReferenceWithoutColumns),
        };
        let words = YamlWords {
            keywords: spec.keywords.clone(),
            types: spec.types.clone(),
        };
        let blocks = YamlBlocks {
            language: spec.blocks.language.map(Into::into),
            begin: spec.blocks.begin.map(Into::into),
            routines: spec.blocks.routines.map(Into::into),
        };
        let a = &spec.alter;
        let alter = YamlAlter {
            add: a.add.clone(),
            default_first: a.default_first,
            named_reference_columns: a.named_reference_columns,
            column_rename: a.column_rename.map(Into::into),
            attrs: a.attrs.map(Into::into),
            constraints: a.constraints.map(Into::into),
            check_drop: a.check_drop.clone(),
            table_rename: a.table_rename.map(Into::into),
            reorg: a.reorg,
        };
        let d = &spec.drops;
        let drops = YamlDrops {
            schema: d.schema.map(Into::into),
            database: d.database,
            truncate: d.truncate.map(Into::into),
            routines: d.routines.map(Into::into),
            triggers: d.triggers.map(Into::into),
            in_catalog: d.in_catalog,
        };
        let sql = YamlSql {
            syntax: YamlSyntax::of(&spec.syntax),
            words: (!words.keywords.is_empty() || !words.types.is_empty()).then_some(words),
            functions: spec.functions.clone(),
            folding: spec.folding.map(Into::into),
            case_insensitive_names: spec.case_insensitive_names,
            catalog_first: spec.catalog_first,
            uses_catalogs: spec.uses_catalogs,
            blocks: (spec.blocks != BlocksSpec::default()).then_some(blocks),
            routine_layout: spec.routine_layout.map(Into::into),
            client: spec.client.map(Into::into),
            page_clause: spec.page_clause.clone(),
            reads_by_server_cursor: spec.reads_by_server_cursor,
            explain: spec.explain.clone().map(|explain| {
                explain.map(|e| YamlExplain {
                    before: e.before,
                    plan: e.plan,
                    after: e.after,
                    shown: e.shown,
                })
            }),
            empty_insert: spec.empty_insert.clone(),
            alter: (spec.alter != AlterSpec::default()).then_some(alter),
            unsupported_reasons: (!spec.unsupported_reasons.is_empty()).then_some(reasons),
            drops: (spec.drops != DropsSpec::default()).then_some(drops),
            enter_scope: spec.enter_scope.clone(),
            connect_timeout_property: spec.connect_timeout_property.clone(),
        };
        let empty = serde_norway::to_value(&sql)
            .map(|value| value.as_mapping().is_some_and(|map| map.is_empty()))
            .unwrap_or(false);
        Self {
            api: API,
            name: spec.name.clone(),
            base: spec.base.clone(),
            aliases: spec.aliases.clone(),
            sql: (!empty).then_some(sql),
            bridge: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rudbman_sql::Dialect;
    use rudbman_sql::dialect::profile::resolve;

    /// Every built-in, exported, written as YAML and read back, is the same
    /// profile: the format holds everything the export says.
    #[test]
    fn every_builtin_export_round_trips_through_yaml() {
        for &builtin in Dialect::ALL {
            let spec = ProfileSpec::of(builtin);
            let text = to_yaml(&spec);
            let read = parse("round-trip", &text).unwrap_or_else(|error| panic!("{error}\n{text}"));
            assert_eq!(read.spec, spec, "{}\n{text}", builtin.name());
        }
    }

    const TIDB: &str = "\
api: 1
name: TiDB
base: mysql
aliases: [pingcap]
sql:
  words:
    keywords: [AUTO_RANDOM]
  page_clause: \"LIMIT {offset}, {limit}\"
  explain: null
  alter:
    reorg: false
  unsupported_reasons:
    default: TiDB keeps no default here
bridge:
  script: tidb.groovy
";

    /// The format of the design document, read and loaded.
    #[test]
    fn a_profile_reads_and_loads() {
        let file = parse("tidb", TIDB).expect("reads");
        assert_eq!(file.spec.name, "TiDB");
        assert_eq!(file.spec.aliases, ["pingcap"]);
        assert_eq!(file.spec.explain, Some(None), "null is kept as null");
        assert_eq!(file.spec.empty_insert, None, "a key left out stays out");
        assert!(file.bridge.is_some(), "the bridge half is kept");

        let (_, loaded) = resolve(vec![("tidb".to_string(), file.spec)]).remove(0);
        let tidb = loaded.expect("loads");
        assert_eq!(tidb.page_clause(10, 20), Some("LIMIT 20, 10".to_string()));
        assert!(tidb.is_keyword("auto_random"));
        assert!(tidb.is_keyword("straight_join"));
    }

    /// A key the format does not have is an error with its line.
    #[test]
    fn an_unknown_key_says_where() {
        let text = "api: 1\nname: X\nbase: mysql\nsql:\n  page_clauze: \"LIMIT {limit}\"\n";
        let error = parse("typo", text).expect_err("refused");
        assert_eq!(error.line, Some(5), "{error}");
        assert!(error.message.contains("page_clauze"), "{error}");
        assert!(error.to_string().starts_with("typo.yml:5:"), "{error}");
    }

    /// A value outside its list is an error with its line too.
    #[test]
    fn an_unknown_value_says_where() {
        let text = "api: 1\nname: X\nbase: mysql\nsql:\n  folding: sideways\n";
        let error = parse("value", text).expect_err("refused");
        assert_eq!(error.line, Some(5), "{error}");
    }

    /// The version is required and has to be this build's.
    #[test]
    fn the_api_version_is_checked() {
        assert!(parse("none", "name: X\nbase: mysql\n").is_err());
        let error = parse("later", "api: 2\nname: X\nbase: mysql\n").expect_err("refused");
        assert!(error.message.contains("api 2"), "{error}");
    }

    /// A profile that says nothing about rudbman-sql writes no `sql` half.
    #[test]
    fn an_empty_half_is_not_written() {
        let spec = ProfileSpec {
            name: "Bare".to_string(),
            base: "postgres".to_string(),
            ..ProfileSpec::default()
        };
        assert_eq!(to_yaml(&spec), "api: 1\nname: Bare\nbase: postgres\n");
    }
}
