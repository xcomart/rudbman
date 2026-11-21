use super::*;

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

fn read(dir: &Path, name: &str) -> Option<String> {
    fs::read_to_string(dir.join(name)).ok()
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

const V1: Shipped = Shipped {
    name: "tidb.yml",
    contents: b"version 1",
};
const V2: Shipped = Shipped {
    name: "tidb.yml",
    contents: b"version 2",
};

#[test]
fn a_file_never_offered_is_written_and_recorded() {
    let dir = tempfile::tempdir().expect("tempdir");
    let dir = dir.path().join("dialects");
    let synced = sync_shipped(&dir, &[V1]).expect("sync");
    assert_eq!(synced.written, ["tidb.yml"]);
    assert_eq!(read(&dir, "tidb.yml").as_deref(), Some("version 1"));
    assert!(read(&dir, RECORD_FILE_NAME).is_some());

    let again = sync_shipped(&dir, &[V1]).expect("sync");
    assert_eq!(again, Synced::default(), "nothing more to do");
}

#[test]
fn nothing_shipped_writes_nothing() {
    let dir = tempfile::tempdir().expect("tempdir");
    let dir = dir.path().join("dialects");
    sync_shipped(&dir, &[]).expect("sync");
    assert!(!dir.exists(), "no directory made for nothing");
}

#[test]
fn a_deleted_file_stays_deleted() {
    let dir = tempfile::tempdir().expect("tempdir");
    sync_shipped(dir.path(), &[V1]).expect("sync");
    fs::remove_file(dir.path().join("tidb.yml")).expect("delete");
    let synced = sync_shipped(dir.path(), &[V2]).expect("sync");
    assert_eq!(synced, Synced::default());
    assert_eq!(read(dir.path(), "tidb.yml"), None, "a newer version too");
    assert_eq!(read(dir.path(), "tidb.yml.new"), None);
}

#[test]
fn an_unchanged_file_takes_the_newer_version() {
    let dir = tempfile::tempdir().expect("tempdir");
    sync_shipped(dir.path(), &[V1]).expect("sync");
    let synced = sync_shipped(dir.path(), &[V2]).expect("sync");
    assert_eq!(synced.replaced, ["tidb.yml"]);
    assert_eq!(read(dir.path(), "tidb.yml").as_deref(), Some("version 2"));
}

#[test]
fn a_changed_file_is_kept_and_the_newer_version_waits_beside_it() {
    let dir = tempfile::tempdir().expect("tempdir");
    sync_shipped(dir.path(), &[V1]).expect("sync");
    write(dir.path(), "tidb.yml", "mine");
    let synced = sync_shipped(dir.path(), &[V2]).expect("sync");
    assert_eq!(synced.beside, ["tidb.yml"]);
    assert_eq!(read(dir.path(), "tidb.yml").as_deref(), Some("mine"));
    assert_eq!(
        read(dir.path(), "tidb.yml.new").as_deref(),
        Some("version 2")
    );

    // Offered once: the next start leaves both as they are.
    assert_eq!(
        sync_shipped(dir.path(), &[V2]).expect("sync"),
        Synced::default()
    );
    assert_eq!(
        read(dir.path(), "tidb.yml.new").as_deref(),
        Some("version 2")
    );

    // Taken: the `.new` has done its work.
    write(dir.path(), "tidb.yml", "version 2");
    sync_shipped(dir.path(), &[V2]).expect("sync");
    assert_eq!(read(dir.path(), "tidb.yml.new"), None);

    // And the file, now as offered, follows the next version again.
    let v3 = Shipped {
        name: "tidb.yml",
        contents: b"version 3",
    };
    assert_eq!(
        sync_shipped(dir.path(), &[v3]).expect("sync").replaced,
        ["tidb.yml"]
    );
}

#[test]
fn a_users_own_file_of_the_same_name_is_not_overwritten() {
    let dir = tempfile::tempdir().expect("tempdir");
    write(dir.path(), "tidb.yml", "mine from before");
    let synced = sync_shipped(dir.path(), &[V1]).expect("sync");
    assert_eq!(synced.beside, ["tidb.yml"]);
    assert_eq!(
        read(dir.path(), "tidb.yml").as_deref(),
        Some("mine from before")
    );
    assert_eq!(
        read(dir.path(), "tidb.yml.new").as_deref(),
        Some("version 1")
    );
}

#[test]
fn an_unreadable_record_decides_nothing() {
    let dir = tempfile::tempdir().expect("tempdir");
    write(dir.path(), RECORD_FILE_NAME, "{ not json");
    assert!(sync_shipped(dir.path(), &[V1]).is_err());
    assert_eq!(read(dir.path(), "tidb.yml"), None);
}

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

const SCRIPT_V1: Shipped = Shipped {
    name: "tidb.groovy",
    contents: b"// version 1",
};
const SCRIPT_V2: Shipped = Shipped {
    name: "tidb.groovy",
    contents: b"// version 2",
};
const SCRIPT_V3: Shipped = Shipped {
    name: "tidb.groovy",
    contents: b"// version 3",
};

fn allowed_hash(dir: &Path, name: &str) -> Option<String> {
    read_allowed(dir)
        .expect("allowed.json")
        .scripts
        .get(name)
        .cloned()
}

#[test]
fn a_shipped_script_is_allowed_as_the_application_writes_it() {
    let dir = tempfile::tempdir().expect("tempdir");
    sync_shipped(dir.path(), &[V1, SCRIPT_V1]).expect("sync");
    assert_eq!(
        allowed_hash(dir.path(), "tidb.groovy"),
        Some(hash(SCRIPT_V1.contents))
    );
    assert_eq!(
        allowed_hash(dir.path(), "tidb.yml"),
        None,
        "a profile is no script"
    );

    // A newer version replaces one the user left alone, and stays allowed.
    sync_shipped(dir.path(), &[V1, SCRIPT_V2]).expect("sync");
    assert_eq!(
        allowed_hash(dir.path(), "tidb.groovy"),
        Some(hash(SCRIPT_V2.contents))
    );

    // Edited, it is the user's: the update waits beside it, and the edit
    // needs allowing.
    write(dir.path(), "tidb.groovy", "// the user's");
    sync_shipped(dir.path(), &[V1, SCRIPT_V3]).expect("sync");
    assert_eq!(
        read(dir.path(), "tidb.groovy").as_deref(),
        Some("// the user's")
    );
    assert_eq!(
        allowed_hash(dir.path(), "tidb.groovy"),
        Some(hash(SCRIPT_V2.contents)),
        "what was allowed is not what is there"
    );
}

#[test]
fn a_shipped_script_the_user_turned_off_stays_off_across_an_update() {
    let dir = tempfile::tempdir().expect("tempdir");
    sync_shipped(dir.path(), &[SCRIPT_V1]).expect("sync");
    disallow_in(dir.path(), "tidb.groovy").expect("disallow");
    let synced = sync_shipped(dir.path(), &[SCRIPT_V2]).expect("sync");
    assert_eq!(synced.replaced, ["tidb.groovy"]);
    assert_eq!(allowed_hash(dir.path(), "tidb.groovy"), None);
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
