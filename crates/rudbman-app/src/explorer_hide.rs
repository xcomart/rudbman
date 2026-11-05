//! What a profile's hide lists take out of the explorer tree.
//!
//! The lists themselves are saved with the profile
//! ([`ExplorerHide`](rudbman_core::ExplorerHide)) as text; this is where they
//! become something that can be asked about a name. [`HideRules`] is compiled
//! once per connection and consulted while a level is read
//! ([`load_children`](crate::explorer::load_children)), before the
//! layer-skipping rule counts what the level holds — which is the whole reason
//! it is applied there rather than in the tree's own filter, which only ever
//! narrows what was already read.
//!
//! A pattern is a regular expression matched against the *whole* name, case
//! ignored: `pg_temp_.*` hides `pg_temp_3` and not `my_pg_temp_3`, and
//! `information_schema` hides that schema and nothing that merely contains the
//! word. The filter box over the tree matches anywhere instead, because there
//! a stray match costs a glance; here it costs a schema nobody can see, and
//! nothing on screen to say why.

use regex::{Regex, RegexBuilder};
use rudbman_core::{ExplorerHide, HideList};

use crate::explorer::Folder;
use crate::explorer_filter::FolderSet;

/// The patterns a list's text holds: one per line, trimmed, blank lines
/// dropped.
pub fn patterns_of(text: &str) -> Vec<String> {
    text.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(str::to_owned)
        .collect()
}

/// Compiles one pattern the way the explorer applies it: the whole name, any
/// case.
pub fn compile(pattern: &str) -> Result<Regex, regex::Error> {
    RegexBuilder::new(&format!("^(?:{pattern})$"))
        .case_insensitive(true)
        .build()
}

/// The first line of `text` whose pattern does not compile, as its line number
/// (from 1, blank lines counted, as an editor counts them) and the reason, for
/// the dialog to refuse it by.
pub fn first_invalid_line(text: &str) -> Option<(usize, String)> {
    text.lines()
        .enumerate()
        .map(|(index, line)| (index, line.trim()))
        .filter(|(_, line)| !line.is_empty())
        .find_map(|(index, line)| {
            compile(line)
                .err()
                .map(|error| (index + 1, first_line(&error)))
        })
}

/// The first line of a regex error: the rest is a drawing of the pattern with
/// a caret under the fault, which a one-line status cannot show.
fn first_line(error: &regex::Error) -> String {
    let text = error.to_string();
    text.lines()
        .rev()
        .find(|line| line.starts_with("error:"))
        .map(|line| line.trim_start_matches("error:").trim().to_owned())
        .unwrap_or(text)
}

/// The name a folder is saved under in
/// [`ExplorerHide::object_folders`](rudbman_core::ExplorerHide::object_folders).
pub fn folder_key(folder: Folder) -> String {
    match serde_json::to_value(folder) {
        Ok(serde_json::Value::String(key)) => key,
        _ => format!("{folder:?}"),
    }
}

/// The folder saved under `key`, if any is.
fn folder_of(key: &str) -> Option<Folder> {
    serde_json::from_value(serde_json::Value::String(key.to_owned())).ok()
}

/// The folders the object list applies to.
pub fn object_folders(hide: &ExplorerHide) -> FolderSet {
    match &hide.object_folders {
        None => FolderSet::ALL,
        Some(keys) => {
            keys.iter()
                .filter_map(|key| folder_of(key))
                .fold(FolderSet::NONE, |set, folder| {
                    if set.contains(folder) {
                        set
                    } else {
                        set.toggled(folder)
                    }
                })
        }
    }
}

/// How `folders` is saved: `None` for every folder, which is what a profile
/// that never chose says.
pub fn object_folders_value(folders: FolderSet) -> Option<Vec<String>> {
    (!folders.is_all()).then(|| folders.iter().map(folder_key).collect())
}

/// One compiled list.
#[derive(Debug, Default)]
struct NameRule {
    patterns: Vec<Regex>,
    show_only: bool,
}

impl NameRule {
    /// Compiles `list`, passing over a pattern that does not compile: the
    /// dialog never saves one, so it can only have been typed into the file
    /// by hand, and a tree missing one rule is better than no tree.
    fn new(list: &HideList, what: &str) -> Self {
        let patterns = list
            .patterns
            .iter()
            .filter_map(|pattern| match compile(pattern) {
                Ok(regex) => Some(regex),
                Err(error) => {
                    log::warn!("ignoring the {what} hide pattern {pattern:?}: {error}");
                    None
                }
            })
            .collect();
        Self {
            patterns,
            show_only: list.show_only,
        }
    }

    /// Whether `name` is left out. An empty list leaves nothing out, either
    /// way round.
    fn hides(&self, name: &str) -> bool {
        if self.patterns.is_empty() {
            return false;
        }
        let matched = self.patterns.iter().any(|pattern| pattern.is_match(name));
        matched != self.show_only
    }
}

/// A profile's hide lists, compiled.
#[derive(Debug)]
pub struct HideRules {
    catalogs: NameRule,
    schemas: NameRule,
    objects: NameRule,
    object_folders: FolderSet,
}

impl Default for HideRules {
    /// Hides nothing.
    fn default() -> Self {
        Self {
            catalogs: NameRule::default(),
            schemas: NameRule::default(),
            objects: NameRule::default(),
            object_folders: FolderSet::ALL,
        }
    }
}

impl HideRules {
    /// Compiles a profile's lists.
    pub fn new(hide: &ExplorerHide) -> Self {
        Self {
            catalogs: NameRule::new(&hide.catalogs, "catalog"),
            schemas: NameRule::new(&hide.schemas, "schema"),
            objects: NameRule::new(&hide.objects, "object"),
            object_folders: object_folders(hide),
        }
    }

    /// Whether the catalog `name` is left out.
    pub fn hides_catalog(&self, name: &str) -> bool {
        self.catalogs.hides(name)
    }

    /// Whether the schema `name` is left out.
    pub fn hides_schema(&self, name: &str) -> bool {
        self.schemas.hides(name)
    }

    /// Whether the object `name` in `folder` is left out.
    pub fn hides_object(&self, folder: Folder, name: &str) -> bool {
        self.object_folders.contains(folder) && self.objects.hides(name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn list(patterns: &[&str], show_only: bool) -> HideList {
        HideList {
            patterns: patterns.iter().map(|p| (*p).to_owned()).collect(),
            show_only,
        }
    }

    #[test]
    fn a_pattern_matches_the_whole_name_in_any_case() {
        let rules = HideRules::new(&ExplorerHide {
            schemas: list(&["pg_temp_\\d+", "information_schema"], false),
            ..ExplorerHide::default()
        });
        assert!(rules.hides_schema("pg_temp_3"));
        assert!(rules.hides_schema("INFORMATION_SCHEMA"));
        assert!(
            !rules.hides_schema("my_pg_temp_3"),
            "the whole name, not a part"
        );
        assert!(!rules.hides_schema("information_schema_copy"));
        assert!(!rules.hides_schema("public"));
        // Anchors the author wrote anyway change nothing.
        let anchored = HideRules::new(&ExplorerHide {
            schemas: list(&["^pg_.*$"], false),
            ..ExplorerHide::default()
        });
        assert!(anchored.hides_schema("pg_catalog"));
        // Nothing else was listed, so nothing else is hidden.
        assert!(!rules.hides_catalog("pg_temp_3"));
        assert!(!rules.hides_object(Folder::Tables, "pg_temp_3"));
    }

    #[test]
    fn show_only_turns_a_list_around_and_an_empty_list_hides_nothing() {
        let rules = HideRules::new(&ExplorerHide {
            catalogs: list(&["app_.*"], true),
            schemas: list(&[], true),
            ..ExplorerHide::default()
        });
        assert!(!rules.hides_catalog("app_main"));
        assert!(rules.hides_catalog("mysql"));
        assert!(
            !rules.hides_schema("anything"),
            "an empty list, either way round"
        );
    }

    #[test]
    fn the_object_list_applies_to_the_folders_it_names() {
        let hide = ExplorerHide {
            objects: list(&["BIN\\$.*"], false),
            object_folders: Some(vec![folder_key(Folder::Tables), "Nonsense".to_owned()]),
            ..ExplorerHide::default()
        };
        let rules = HideRules::new(&hide);
        assert!(rules.hides_object(Folder::Tables, "BIN$abc"));
        assert!(!rules.hides_object(Folder::Views, "BIN$abc"));
        assert!(!rules.hides_object(Folder::Tables, "ORDERS"));

        // Every folder is saved as no choice at all, and read back as every
        // folder.
        assert_eq!(object_folders_value(FolderSet::ALL), None);
        assert_eq!(object_folders(&ExplorerHide::default()), FolderSet::ALL);
        let tables = FolderSet::NONE.toggled(Folder::Tables);
        assert_eq!(
            object_folders_value(tables),
            Some(vec!["Tables".to_owned()])
        );
        assert_eq!(object_folders(&hide), tables);
    }

    #[test]
    fn a_pattern_that_does_not_compile_is_named_and_otherwise_passed_over() {
        let text = "  pg_.*  \n\n(unclosed\nok";
        let patterns = patterns_of(text);
        assert_eq!(patterns, vec!["pg_.*", "(unclosed", "ok"]);
        // The line as the field shows it, the blank one counted.
        let (line, reason) = first_invalid_line(text).expect("one is broken");
        assert_eq!(line, 3);
        assert!(!reason.is_empty() && !reason.contains('\n'), "{reason:?}");
        assert_eq!(first_invalid_line("a\n\nb.*"), None);

        let rules = HideRules::new(&ExplorerHide {
            schemas: HideList {
                patterns,
                show_only: false,
            },
            ..ExplorerHide::default()
        });
        assert!(rules.hides_schema("pg_catalog"));
        assert!(rules.hides_schema("OK"));
        assert!(!rules.hides_schema("(unclosed"));
    }
}
