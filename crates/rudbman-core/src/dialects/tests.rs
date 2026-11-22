use std::fs;

use super::*;
use rudbkit_core::dialects::{ALLOWED_FILE_NAME, BUILT_IN, hash};

/// A profile on `mysql` bringing a driver of its own.
const TIDB: &str = "\
api: 1
name: TiDB
base: mysql
sql:
  page_clause: \"LIMIT {offset}, {limit}\"
bridge:
  script: tidb.groovy
driver:
  class: com.mysql.cj.jdbc.Driver
  url_template: \"jdbc:mysql://{host}:{port}/{database}\"
  default_port: 4000
";

fn write(dir: &Path, name: &str, text: &str) {
    fs::write(dir.join(name), text).expect("write");
}

#[test]
fn a_directory_that_is_not_there_holds_no_profiles() {
    let dir = tempfile::tempdir().expect("tempdir");
    let report = load_from(&dir.path().join("dialects"));
    assert!(report.entries.is_empty());
    assert!(report.problems.is_empty(), "{:?}", report.problems);
}

#[test]
fn profiles_load_with_what_they_bring() {
    let dir = tempfile::tempdir().expect("tempdir");
    write(dir.path(), "tidb.yml", TIDB);
    write(dir.path(), "tidb.groovy", SCRIPT);
    write(dir.path(), "notes.txt", "not a profile");
    let report = load_from(dir.path());
    assert_eq!(report.entries.len(), 1, "only `*.yml` is a profile");

    let entry = &report.entries[0];
    assert_eq!(entry.id, "tidb");
    assert_eq!(entry.label(), "TiDB");
    assert_eq!(entry.base.as_deref(), Some("mysql"));
    assert_eq!(entry.script.as_deref(), Some("tidb.groovy"));
    let dialect = entry
        .dialect()
        .unwrap_or_else(|| panic!("{:?}", entry.state));
    assert_eq!(dialect.name(), "tidb");
    assert_eq!(dialect.product(), "TiDB");
    assert_eq!(
        dialect.page_clause(10, 20),
        Some("LIMIT 20, 10".to_string())
    );

    let drivers = report.drivers();
    assert_eq!(drivers.len(), 1);
    assert_eq!(drivers[0].id, "tidb");
    assert_eq!(drivers[0].dialect, "tidb");
    assert_eq!(drivers[0].default_port, Some(4000));

    assert!(report.entry_for("TiDB").is_some(), "ids ignore case");
    assert!(
        report.entry_for("mysql").is_none(),
        "a built-in is no profile"
    );
}

#[test]
fn a_profile_that_fails_says_why_and_brings_nothing() {
    let dir = tempfile::tempdir().expect("tempdir");
    // An unknown key, on its line.
    write(
        dir.path(),
        "typo.yml",
        "api: 1\nname: Typo\nbase: mysql\nsql:\n  page_clauze: x\n",
    );
    // Reads, but its base names nothing.
    write(
        dir.path(),
        "lost.yml",
        "api: 1\nname: Lost\nbase: nowhere\ndriver:\n  class: a.B\n  url_template: \"jdbc:x:\"\n",
    );
    // On a base that did not read.
    write(dir.path(), "child.yml", "api: 1\nname: Child\nbase: typo\n");
    let report = load_from(dir.path());
    let failure = |id: &str| {
        let entry = report.entries.iter().find(|e| e.id == id).expect(id);
        entry.failure().map(str::to_string).expect("failed")
    };
    let typo = failure("typo");
    assert!(
        typo.contains("page_clauze") && typo.contains("line 5"),
        "{typo}"
    );
    assert!(failure("lost").contains("nowhere"), "{}", failure("lost"));
    assert_eq!(failure("child"), "`typo` did not load");
    assert!(
        report.drivers().is_empty(),
        "a failed profile offers no driver"
    );
    assert!(report.loaded().is_empty());
}

#[test]
fn a_pending_update_is_noticed_and_not_loaded() {
    let dir = tempfile::tempdir().expect("tempdir");
    write(dir.path(), "tidb.yml", TIDB);
    write(dir.path(), "tidb.yml.new", TIDB);
    write(dir.path(), "tidb.groovy.new", "// newer");
    let report = load_from(dir.path());
    assert_eq!(report.entries.len(), 1, "a `.new` is no profile");
    let pending: Vec<String> = report.entries[0]
        .pending
        .iter()
        .map(|path| path.file_name().unwrap().to_string_lossy().into_owned())
        .collect();
    assert_eq!(pending, ["tidb.yml.new", "tidb.groovy.new"]);
}

// --------------------------------------------------------------- shipping

/// Every profile the application ships loads, among the others it ships, and
/// every script it ships belongs to one.
#[test]
fn every_shipped_profile_loads() {
    let dir = tempfile::tempdir().expect("tempdir");
    sync_shipped(dir.path(), SHIPPED).expect("sync");
    let report = load_from(dir.path());
    for entry in &report.entries {
        assert!(entry.dialect().is_some(), "{}: {:?}", entry.id, entry.state);
    }
    for file in SHIPPED {
        assert!(
            !file.name.contains(['/', '\\']),
            "{} is a name, not a path",
            file.name
        );
        if let Some(stem) = file.name.strip_suffix(".groovy") {
            assert!(
                report.entries.iter().any(|entry| {
                    entry.script.as_deref() == Some(file.name) || entry.id == stem
                }),
                "{} belongs to no profile",
                file.name
            );
        }
    }
}

/// An export reads back as the same profile, and loads.
#[test]
fn an_export_loads_as_a_profile() {
    let dir = tempfile::tempdir().expect("tempdir");
    write(
        dir.path(),
        "copy.yml",
        &export(Dialect::from_id("postgres")),
    );
    let report = load_from(dir.path());
    assert!(
        report.entries[0].dialect().is_some(),
        "{:?}",
        report.entries[0].state
    );
}

/// A session opened with a profile hands the bridge its `bridge` half merged
/// along its chain of bases, and the built-in the chain ends in.
#[test]
fn a_session_profile_merges_its_chain_of_bases() {
    let dir = tempfile::tempdir().expect("tempdir");
    write(
        dir.path(),
        "parent.yml",
        "api: 1\nname: Parent\nbase: PostgreSQL\nbridge:\n  hex_literal: \"0x{hex}\"\n  \
         comments: { hides_comments: true, escape: none }\n",
    );
    write(
        dir.path(),
        "child.yml",
        "api: 1\nname: Child\nbase: parent\nbridge:\n  detect: [child]\n  \
         comments: { hides_comments: false }\n",
    );
    write(
        dir.path(),
        "plain.yml",
        "api: 1\nname: Plain\nbase: generic\n",
    );
    let report = load_from(dir.path());

    let child = report.session_profile("child").expect("loaded");
    assert_eq!(child["id"], "child");
    assert_eq!(child["base"], "postgres", "the built-in, by its own name");
    assert_eq!(child["bridge"]["hex_literal"], "0x{hex}", "the parent's");
    assert_eq!(
        child["bridge"]["comments"]["hides_comments"], false,
        "its own"
    );
    assert_eq!(
        child["bridge"]["comments"]["escape"], "none",
        "merged by key"
    );

    let plain = report.session_profile("plain").expect("loaded");
    assert_eq!(
        plain["bridge"],
        serde_json::json!({}),
        "no bridge half: its base's"
    );
    assert_eq!(
        report.session_profile("mysql"),
        None,
        "a built-in is no profile"
    );

    let detected: Vec<String> = report
        .detect_profiles()
        .iter()
        .map(|profile| profile["id"].as_str().unwrap_or_default().to_string())
        .collect();
    assert_eq!(detected, ["child"], "only a profile that detects");
}

// ------------------------------------------------------------------ trust

/// A script for [`TIDB`] to name.
const SCRIPT: &str = "String hexLiteral(String hex) { \"0x${hex}\" }\n";

fn hooks(report: &Report, id: &str) -> Script {
    let entry = report.entry_for(id).expect(id);
    assert!(entry.dialect().is_some(), "{:?}", entry.state);
    entry.hooks.clone().expect("a script")
}

fn runs(report: &Report, id: &str) -> bool {
    runs_script(&report.session_profile(id).expect("loaded"))
}

#[test]
fn a_script_runs_only_once_it_is_allowed_and_as_it_was_allowed() {
    let dir = tempfile::tempdir().expect("tempdir");
    write(dir.path(), "tidb.yml", TIDB);
    write(dir.path(), "tidb.groovy", SCRIPT);
    let report = load_from(dir.path());
    let script = hooks(&report, "tidb");
    assert_eq!(script.name, "tidb.groovy");
    assert_eq!(script.path, dir.path().join("tidb.groovy"));
    assert_eq!(script.sha256, hash(SCRIPT.as_bytes()));
    assert!(!script.allowed, "a new script is not allowed");
    let session = report.session_profile("tidb").expect("loaded");
    assert!(
        !runs_script(&session),
        "refused, the profile loads without it"
    );
    assert_eq!(
        session["bridge"]["script"], "tidb.groovy",
        "its data still applies"
    );

    allow_in(dir.path(), &script).expect("allow");
    let report = load_from(dir.path());
    assert!(hooks(&report, "tidb").allowed);
    let session = report.session_profile("tidb").expect("loaded");
    assert!(runs_script(&session));
    assert_eq!(session["script"]["text"], SCRIPT);
    assert_eq!(
        session["script"]["path"],
        dir.path().join("tidb.groovy").display().to_string()
    );

    write(dir.path(), "tidb.groovy", "// changed\n");
    let report = load_from(dir.path());
    assert!(!hooks(&report, "tidb").allowed, "changed, it asks again");
    assert!(!runs(&report, "tidb"));

    write(dir.path(), "tidb.groovy", SCRIPT);
    assert!(
        runs(&load_from(dir.path()), "tidb"),
        "the version allowed runs again"
    );
    disallow_in(dir.path(), "tidb.groovy").expect("disallow");
    assert!(!runs(&load_from(dir.path()), "tidb"), "turned off");
}

#[test]
fn allowing_a_version_that_has_since_changed_allows_nothing() {
    let dir = tempfile::tempdir().expect("tempdir");
    write(dir.path(), "tidb.yml", TIDB);
    write(dir.path(), "tidb.groovy", SCRIPT);
    let asked = hooks(&load_from(dir.path()), "tidb");
    // Edited while the question was showing.
    write(dir.path(), "tidb.groovy", "// edited meanwhile\n");
    allow_in(dir.path(), &asked).expect("allow");
    assert!(!runs(&load_from(dir.path()), "tidb"));
}

#[test]
fn a_script_that_cannot_be_read_fails_its_profile() {
    let dir = tempfile::tempdir().expect("tempdir");
    write(dir.path(), "tidb.yml", TIDB);
    let report = load_from(dir.path());
    let entry = report.entry_for("tidb").expect("tidb");
    let failure = entry.failure().expect("failed");
    assert!(
        failure.starts_with("bridge.script: tidb.groovy: "),
        "{failure}"
    );
    assert!(report.loaded().is_empty(), "its drivers get generic SQL");
    assert!(report.session_profile("tidb").is_none());
}

#[test]
fn a_script_outside_the_profiles_folder_fails_its_profile() {
    let dir = tempfile::tempdir().expect("tempdir");
    write(
        dir.path(),
        "escape.yml",
        "api: 1\nname: Escape\nbase: mysql\nbridge:\n  script: ../outside.groovy\n",
    );
    let report = load_from(dir.path());
    let failure = report.entries[0].failure().expect("failed").to_string();
    assert!(failure.contains("bridge.script"), "{failure}");
}

#[test]
fn a_profile_runs_the_script_of_its_base_profile() {
    let dir = tempfile::tempdir().expect("tempdir");
    write(dir.path(), "tidb.yml", TIDB);
    write(dir.path(), "tidb.groovy", SCRIPT);
    write(
        dir.path(),
        "tidb-cloud.yml",
        "api: 1\nname: TiDB Cloud\nbase: tidb\n",
    );
    let report = load_from(dir.path());
    assert_eq!(hooks(&report, "tidb-cloud").name, "tidb.groovy");
    allow_in(dir.path(), &hooks(&report, "tidb")).expect("allow");
    let report = load_from(dir.path());
    assert!(runs(&report, "tidb") && runs(&report, "tidb-cloud"));
}

#[test]
fn an_allow_list_that_cannot_be_read_allows_nothing() {
    let dir = tempfile::tempdir().expect("tempdir");
    write(dir.path(), "tidb.yml", TIDB);
    write(dir.path(), "tidb.groovy", SCRIPT);
    write(dir.path(), ALLOWED_FILE_NAME, "{ not json");
    let report = load_from(dir.path());
    assert!(!runs(&report, "tidb"));
    assert!(
        report
            .problems
            .iter()
            .any(|p| p.contains(ALLOWED_FILE_NAME)),
        "{:?}",
        report.problems
    );
}

#[test]
fn the_groovy_runtime_is_the_one_the_bridge_is_tested_on() {
    let gradle = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../bridge/build.gradle");
    let gradle = fs::read_to_string(&gradle).expect("bridge/build.gradle");
    assert!(
        gradle.contains(&format!("groovyRuntime '{GROOVY_RUNTIME}'")),
        "bridge/build.gradle tests on another Groovy than {GROOVY_RUNTIME}"
    );
}

// ------------------------------------------------------- shipped products

/// The shipped profiles of rudbkit's `dialects/`, as the
/// application loads them: synced into a folder of their own, then read.
fn shipped_report() -> (tempfile::TempDir, Report) {
    let dir = tempfile::tempdir().expect("tempdir");
    sync_shipped(dir.path(), SHIPPED).expect("sync");
    let report = load_from(dir.path());
    (dir, report)
}

fn shipped_dialect(report: &Report, id: &str) -> Dialect {
    report
        .entry_for(id)
        .and_then(Entry::dialect)
        .unwrap_or_else(|| panic!("{id} loads"))
}

#[test]
fn firebird_alters_with_its_own_spellings() {
    use rudbman_sql::{ColumnChange, ColumnDef, TableAlter, plan_alter};
    let (_dir, report) = shipped_report();
    let firebird = shipped_dialect(&report, "firebird");
    let mut alter = TableAlter::new(["ORDERS"]);
    alter.adds.push(
        ColumnDef::new("NOTE", "varchar(20)")
            .with_default("'x'")
            .with_not_null(true),
    );
    alter.changes.push(ColumnChange {
        from: ColumnDef::new("QTY", "integer"),
        to: ColumnDef::new("QTY", "bigint"),
    });
    alter.changes.push(ColumnChange {
        from: ColumnDef::new("OLD", "integer"),
        to: ColumnDef::new("NEW", "integer"),
    });
    alter.drops.push("GONE".to_string());
    assert_eq!(
        plan_alter(&alter, &firebird).expect("plans"),
        [
            "ALTER TABLE ORDERS ADD NOTE varchar(20) DEFAULT 'x' NOT NULL",
            "ALTER TABLE ORDERS ALTER COLUMN QTY TYPE bigint",
            "ALTER TABLE ORDERS ALTER COLUMN OLD TO NEW",
            "ALTER TABLE ORDERS DROP GONE",
        ]
    );

    let mut rename = TableAlter::new(["ORDERS"]);
    rename.rename_to = Some("ORDER_LINES".to_string());
    let error = plan_alter(&rename, &firebird).expect_err("Firebird renames no table");
    assert!(
        error
            .to_string()
            .starts_with("Firebird cannot rename a table: Firebird has no statement"),
        "{error}"
    );
}

#[test]
fn cockroachdb_drops_a_unique_constraint_as_its_index() {
    use rudbman_sql::{ConstraintDrop, ConstraintKind, TableAlter, plan_alter};
    let (_dir, report) = shipped_report();
    let cockroach = shipped_dialect(&report, "cockroachdb");
    let mut alter = TableAlter::new(["app", "orders"]);
    for (kind, name) in [
        (ConstraintKind::Unique, "orders_code_uq"),
        (ConstraintKind::Check, "orders_ck"),
    ] {
        alter.drop_constraints.push(ConstraintDrop {
            kind,
            name: name.to_string(),
        });
    }
    assert_eq!(
        plan_alter(&alter, &cockroach).expect("plans"),
        [
            "DROP INDEX app.orders@orders_code_uq CASCADE",
            "ALTER TABLE app.orders DROP CONSTRAINT orders_ck",
        ]
    );
}

#[test]
fn a_firebird_routine_runs_as_one_statement() {
    let (_dir, report) = shipped_report();
    let firebird = shipped_dialect(&report, "firebird");
    let script = "create procedure p (who varchar(20)) returns (msg varchar(100)) as\n\
                  declare x int = 0;\n\
                  begin\n  msg = 'hi ' || who;\n  suspend;\nend;\n\
                  select * from p('a');\n";
    let statements = rudbman_sql::split_script(script, &firebird).expect("splits");
    assert_eq!(statements.len(), 2, "{statements:#?}");
    // Whole, its own `end;` kept as a PL/SQL unit keeps it: Firebird takes
    // the body over JDBC with or without it.
    assert!(
        statements[0].sql.starts_with("create procedure p"),
        "{:?}",
        statements[0].sql
    );
    assert!(
        statements[0].sql.trim_end().ends_with("end;"),
        "{:?}",
        statements[0].sql
    );
    assert_eq!(statements[1].sql.trim(), "select * from p('a')");
}

#[test]
fn the_shipped_products_bring_their_drivers_and_firebird_its_script() {
    let (_dir, report) = shipped_report();
    let classes: Vec<(String, String)> = report
        .drivers()
        .into_iter()
        .map(|driver| (driver.id, driver.class))
        .collect();
    for (id, class) in [
        ("cockroachdb", "org.postgresql.Driver"),
        ("firebird", "org.firebirdsql.jdbc.FBDriver"),
        ("tidb", "com.mysql.cj.jdbc.Driver"),
    ] {
        assert!(
            classes.contains(&(id.to_string(), class.to_string())),
            "{id}: {classes:?}"
        );
    }
    for id in ["firebird", "tidb"] {
        let script = report
            .entry_for(id)
            .and_then(|e| e.hooks.clone())
            .expect("a script");
        assert!(
            script.allowed,
            "{id}: a shipped script is allowed as written"
        );
        assert!(runs_script(&report.session_profile(id).expect("loaded")));
    }
}

/// rudbkit's list of the built-ins a chain of bases ends in, which rudbgen
/// reads by, is this build's: every built-in by its name and aliases.
#[test]
fn rudbkits_built_ins_are_these() {
    let ours: Vec<(&str, Vec<&str>)> = Dialect::ALL
        .iter()
        .map(|dialect| (dialect.name(), dialect.aliases().to_vec()))
        .collect();
    let theirs: Vec<(&str, Vec<&str>)> = BUILT_IN
        .iter()
        .map(|(name, aliases)| (*name, aliases.to_vec()))
        .collect();
    assert_eq!(theirs, ours);
}
