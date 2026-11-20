use super::*;
use crate::ddl::{ColumnChange, ColumnDef, DdlError, TableAlter, plan_alter};

/// Resolves one profile, which has to load.
fn one(id: &str, spec: ProfileSpec) -> Dialect {
    let mut out = resolve(vec![(id.to_string(), spec)]);
    let (_, result) = out.remove(0);
    result.unwrap_or_else(|error| panic!("{error}"))
}

/// Resolves one profile, which has to fail, and says why.
fn refused(id: &str, spec: ProfileSpec) -> ProfileError {
    let mut out = resolve(vec![(id.to_string(), spec)]);
    out.remove(0).1.expect_err("the profile should not load")
}

fn on(base: &str) -> ProfileSpec {
    ProfileSpec {
        name: "Test".to_string(),
        base: base.to_string(),
        ..ProfileSpec::default()
    }
}

/// Every answer of `a` and `b` is the same, over enough inputs to tell a
/// template from a constant.
fn same_answers(a: Dialect, b: Dialect) {
    let (x, y) = (a.vendor(), b.vendor());
    let n = b.name();
    assert_eq!(x.product(), y.product(), "{n}");
    assert_eq!(x.syntax(), y.syntax(), "{n}");
    let words = |v: &dyn Vendor| {
        let w = v.words();
        (w.keywords.to_vec(), w.types.to_vec())
    };
    assert_eq!(words(x), words(y), "{n}");
    assert_eq!(x.functions(), y.functions(), "{n}");
    assert_eq!(x.folding(), y.folding(), "{n}");
    assert_eq!(
        x.case_insensitive_names(),
        y.case_insensitive_names(),
        "{n}"
    );
    assert_eq!(x.catalog_first(), y.catalog_first(), "{n}");
    assert_eq!(x.uses_catalogs(), y.uses_catalogs(), "{n}");
    assert_eq!(x.block_family(), y.block_family(), "{n}");
    assert!(x.routine_layout() == y.routine_layout(), "{n}");
    assert_eq!(x.client(), y.client(), "{n}");
    for (limit, offset) in [(10, 0), (100, 2_500), (1, 1)] {
        assert_eq!(
            x.page_clause(limit, offset),
            y.page_clause(limit, offset),
            "{n}"
        );
    }
    assert_eq!(
        x.reads_by_server_cursor(),
        y.reads_by_server_cursor(),
        "{n}"
    );
    for sql in ["SELECT 1", "select '{x}' from t where a = '}'"] {
        assert_eq!(x.explain(sql), y.explain(sql), "{n}");
    }
    for (table, column) in [("t", Some("\"c\"")), ("\"T\"", None)] {
        assert_eq!(
            x.empty_insert(table, column),
            y.empty_insert(table, column),
            "{n}"
        );
    }
    assert_eq!(x.alter_style(), y.alter_style(), "{n}");
    for what in Unsupported::ALL {
        assert_eq!(
            x.unsupported_reason(what),
            y.unsupported_reason(what),
            "{n}"
        );
    }
    assert_eq!(x.drop_spelling(), y.drop_spelling(), "{n}");
    for (catalog, schema) in [
        (Some("`c`"), Some("`s`")),
        (Some("c"), None),
        (None, Some("s")),
        (None, None),
    ] {
        assert_eq!(
            x.enter_scope(catalog, schema),
            y.enter_scope(catalog, schema),
            "{n}"
        );
    }
    assert_eq!(
        x.connect_timeout_property(),
        y.connect_timeout_property(),
        "{n}"
    );
}

/// Every built-in, written out as a profile on `generic` and read back,
/// answers as itself: the export says everything the dialect does.
#[test]
fn every_builtin_round_trips_through_a_profile() {
    for &builtin in Dialect::ALL {
        let profile = one("round-trip", ProfileSpec::of(builtin));
        same_answers(profile, builtin);
    }
}

/// A profile exported again is the profile it was, aliases and all.
#[test]
fn a_profile_round_trips_too() {
    let mut spec = on("mysql");
    spec.aliases = vec!["pingcap".to_string()];
    spec.page_clause = Some(Some("LIMIT {offset}, {limit}".to_string()));
    let first = one("tidb", spec);
    let again = ProfileSpec::of(first);
    assert_eq!(again.aliases, ["pingcap"]);
    same_answers(one("tidb", again), first);
}

/// What a profile does not say is its base's answer.
#[test]
fn an_empty_profile_is_its_base() {
    for &builtin in Dialect::ALL {
        let mut spec = on(builtin.name());
        spec.name = builtin.vendor().product().to_string();
        same_answers(one("empty", spec), builtin);
    }
}

/// Records merge field by field over the base's.
#[test]
fn records_merge_field_by_field() {
    let mut spec = on("mysql");
    spec.syntax.hash_line_comment = Some(false);
    spec.alter.reorg = Some(true);
    spec.drops.truncate = Some(TableTruncate::DeleteAll);
    let profile = one("records", spec);
    let (p, mysql) = (profile.vendor(), Dialect::MYSQL.vendor());

    assert!(!p.syntax().hash_line_comment);
    assert_eq!(
        Syntax {
            hash_line_comment: true,
            ..*p.syntax()
        },
        *mysql.syntax()
    );
    assert!(p.alter_style().reorg);
    assert_eq!(
        p.alter_style().column_rename,
        mysql.alter_style().column_rename
    );
    assert_eq!(p.drop_spelling().truncate, TableTruncate::DeleteAll);
    assert_eq!(p.drop_spelling().schema, mysql.drop_spelling().schema);
}

/// Word lists add to the base's, and the lexer finds the additions.
#[test]
fn word_lists_add_to_the_bases() {
    let mut spec = on("mysql");
    spec.keywords = vec!["auto_random".to_string(), "SELECT".to_string()];
    spec.types = vec!["vector".to_string()];
    spec.functions = vec!["tidb_version".to_string(), "CONCAT".to_string()];
    let profile = one("words", spec);

    assert!(profile.is_keyword("auto_random"));
    assert!(profile.is_keyword("straight_join"), "the base's word stays");
    assert!(profile.is_type("VECTOR"));
    assert!(profile.is_type("mediumtext"));
    let functions = profile.vendor().functions();
    assert!(functions.contains(&"TIDB_VERSION"));
    assert_eq!(
        functions.iter().filter(|f| **f == "CONCAT").count(),
        1,
        "a function the base has is not listed twice"
    );
}

/// `null` is "this product has none"; leaving the key out is the base's.
#[test]
fn null_is_not_the_same_as_absent() {
    let mut spec = on("postgres");
    spec.page_clause = Some(None);
    spec.explain = Some(None);
    let none = one("none", spec);
    assert_eq!(none.page_clause(10, 0), None);
    assert_eq!(none.vendor().explain("SELECT 1"), None);

    let absent = one("absent", on("postgres"));
    assert_eq!(
        absent.page_clause(10, 0),
        Some("LIMIT 10 OFFSET 0".to_string())
    );
}

/// A template whose hole has no value gives no statement, the way a
/// built-in answers `None`.
#[test]
fn a_missing_hole_value_gives_no_statement() {
    let mut spec = on("generic");
    spec.enter_scope = Some(Some("USE {catalog}".to_string()));
    spec.empty_insert = Some(Some(
        "INSERT INTO {table} ({first_column}) VALUES (DEFAULT)".to_string(),
    ));
    let profile = one("holes", spec);
    let vendor = profile.vendor();
    assert_eq!(
        vendor.enter_scope(Some("`app`"), None),
        Some("USE `app`".to_string())
    );
    assert_eq!(vendor.enter_scope(None, Some("s")), None);
    assert_eq!(
        vendor.empty_insert("t", Some("c")),
        Some("INSERT INTO t (c) VALUES (DEFAULT)".to_string())
    );
    assert_eq!(vendor.empty_insert("t", None), None);
}

/// `{{` and `}}` are literal braces; a hole the key does not take, a lone
/// `}` and an unclosed `{` are refused with the key they are in.
#[test]
fn templates_are_checked_when_they_load() {
    let mut spec = on("generic");
    spec.page_clause = Some(Some("FETCH {{FIRST}} {limit}".to_string()));
    assert_eq!(
        one("braces", spec).page_clause(5, 0),
        Some("FETCH {FIRST} 5".to_string())
    );

    for (text, says) in [
        ("LIMIT {rows}", "unknown hole {rows}"),
        ("LIMIT {table}", "unknown hole {table}"),
        ("LIMIT } {limit}", "lone `}`"),
        ("LIMIT {limit", "never closed"),
    ] {
        let mut spec = on("generic");
        spec.page_clause = Some(Some(text.to_string()));
        let error = refused("bad", spec);
        assert_eq!(error.key.as_deref(), Some("page_clause"), "{text}");
        assert!(error.message.contains(says), "{text}: {}", error.message);
    }
}

/// A profile on a profile sees what the one below it says.
#[test]
fn bases_chain() {
    let mut lower = on("mysql");
    lower.page_clause = Some(Some("LIMIT {offset}, {limit}".to_string()));
    let mut upper = on("lower");
    upper.keywords = vec!["UPPER_ONLY".to_string()];
    let out = resolve(vec![
        ("upper".to_string(), upper),
        ("lower".to_string(), lower),
    ]);
    let upper = out[0].1.clone().expect("upper loads");
    assert_eq!(upper.page_clause(10, 20), Some("LIMIT 20, 10".to_string()));
    assert!(upper.is_keyword("upper_only"));
    assert!(upper.is_keyword("straight_join"));
}

/// The block rules merge over the base's, and refuse what the language
/// cannot carry.
#[test]
fn block_rules_merge_and_refuse_what_does_not_fit() {
    let mut spec = on("mysql");
    spec.blocks.routines = Some(BlockRoutines::PlSql);
    assert_eq!(
        one("psm", spec).vendor().block_family(),
        Block::Psm {
            begin: BeginRule::NotAtomic,
            routines: Routines::PlSql
        }
    );

    let mut spec = on("oracle");
    spec.blocks.begin = Some(BlockBegin::Transaction);
    assert_eq!(refused("bad", spec).key.as_deref(), Some("blocks.begin"));

    let mut spec = on("oracle");
    spec.blocks.language = Some(BlockLanguage::Psm);
    assert_eq!(refused("bad", spec).key.as_deref(), Some("blocks.begin"));
}

/// The check constraint's drop word belongs to per-kind drops alone.
#[test]
fn check_drop_belongs_to_per_kind_drops() {
    let mut spec = on("mysql");
    spec.alter.check_drop = Some("CONSTRAINT".to_string());
    assert_eq!(
        one("mariadb-like", spec).vendor().alter_style().constraints,
        DropStyle::PerKind {
            check: "CONSTRAINT"
        }
    );

    let mut spec = on("postgres");
    spec.alter.check_drop = Some("CHECK".to_string());
    assert_eq!(
        refused("bad", spec).key.as_deref(),
        Some("alter.check_drop")
    );

    let mut spec = on("postgres");
    spec.alter.constraints = Some(ConstraintDrops::PerKind);
    assert_eq!(
        refused("bad", spec).key.as_deref(),
        Some("alter.check_drop")
    );
}

/// The id, the name, the words and the base are checked before anything is
/// built.
#[test]
fn what_cannot_load_says_why() {
    for id in ["postgres", "pg", "sqlserver", "db2"] {
        assert!(
            refused(id, on("generic")).message.contains("built-in"),
            "{id}"
        );
    }
    for id in ["TiDB", "ti db", "", "tidb.yml"] {
        assert!(
            refused(id, on("generic")).message.contains("not an id"),
            "{id:?}"
        );
    }
    let mut spec = on("generic");
    spec.name = " ".to_string();
    assert_eq!(refused("nameless", spec).key.as_deref(), Some("name"));

    let mut spec = on("generic");
    spec.keywords = vec!["A_KEYWORD_FAR_LONGER_THAN_ANY".to_string()];
    assert_eq!(refused("long", spec).key.as_deref(), Some("keywords"));

    let error = refused("orphan", on("nowhere"));
    assert_eq!(error.key.as_deref(), Some("base"));
    assert!(error.message.contains("names no dialect"));
    assert_eq!(refused("baseless", on("")).key.as_deref(), Some("base"));
}

/// A cycle of bases fails every profile in it, and only those: one that
/// merely stands on the cycle did not load because its base did not.
#[test]
fn a_cycle_of_bases_fails_its_members() {
    let out = resolve(vec![
        ("a".to_string(), on("b")),
        ("b".to_string(), on("a")),
        ("c".to_string(), on("a")),
    ]);
    for (id, result) in &out[..2] {
        let error = result.clone().expect_err(id);
        assert!(error.message.contains("cycle"), "{id}: {error}");
    }
    let error = out[2].1.clone().expect_err("c");
    assert!(error.message.contains("did not load"), "{error}");
}

/// Two profiles cannot share an id or a spelling.
#[test]
fn ids_and_aliases_are_unique() {
    let mut aliased = on("generic");
    aliased.aliases = vec!["first".to_string()];
    let out = resolve(vec![
        ("first".to_string(), on("generic")),
        ("first".to_string(), on("generic")),
        ("second".to_string(), aliased),
    ]);
    assert!(out[0].1.is_ok());
    assert!(out[1].1.clone().unwrap_err().message.contains("twice"));
    assert_eq!(
        out[2].1.clone().unwrap_err().key.as_deref(),
        Some("aliases")
    );

    let mut builtin_alias = on("generic");
    builtin_alias.aliases = vec!["tsql".to_string()];
    assert_eq!(
        refused("third", builtin_alias).key.as_deref(),
        Some("aliases")
    );
}

/// A base that failed early, before any building, is named as one that did
/// not load rather than as one that is not there.
#[test]
fn a_base_that_failed_early_did_not_load() {
    let mut bad = on("generic");
    bad.aliases = vec!["Bad Alias".to_string()];
    let out = resolve(vec![
        ("broken".to_string(), bad),
        ("above".to_string(), on("broken")),
    ]);
    assert!(out[0].1.is_err());
    let error = out[1].1.clone().unwrap_err();
    assert!(error.message.contains("did not load"), "{error}");
}

/// A profile is its own dialect: equal only to itself, shown by its name,
/// and found by `from_id` once installed, by name and alias.
#[test]
fn installed_profiles_are_found_by_name_and_alias() {
    let mut spec = on("mysql");
    spec.aliases = vec!["install-alias".to_string()];
    let profile = one("install-test", spec);
    assert_ne!(profile, Dialect::MYSQL);
    assert_eq!(profile.id(), DialectId::Profile);
    assert_eq!(format!("{profile:?}"), "Dialect(\"install-test\")");

    install(vec![profile]);
    assert_eq!(Dialect::from_id("install-test"), profile);
    assert_eq!(Dialect::from_id(" Install-Alias "), profile);
    assert_eq!(installed(), [profile]);
    // A built-in still wins its own spelling.
    assert_eq!(Dialect::from_id("mysql"), Dialect::MYSQL);
    install(Vec::new());
    assert_eq!(Dialect::from_id("install-test"), Dialect::GENERIC);
}

/// A refused structure edit names the profile's product, with the reason
/// its base gives.
#[test]
fn errors_name_the_profiles_product() {
    let mut spec = on("sqlite");
    spec.name = "Turso".to_string();
    let profile = one("turso", spec);
    let mut alter = TableAlter::new(["t"]);
    alter.changes.push(ColumnChange {
        from: ColumnDef::new("a", "integer"),
        to: ColumnDef::new("a", "text"),
    });
    let error = plan_alter(&alter, &profile).expect_err("SQLite cannot retype");
    assert!(matches!(&error, DdlError::Unsupported { dialect, .. } if *dialect == profile));
    let message = error.to_string();
    assert!(
        message.starts_with("Turso cannot change the type"),
        "{message}"
    );
    assert!(
        message.contains("table would have to be rebuilt"),
        "{message}"
    );
}
