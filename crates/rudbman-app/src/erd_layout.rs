//! Where the boxes of a diagram were left: `erd/<profile-uuid>.json`.
//!
//! One file per connection *profile*, holding one entry per scope — a
//! catalogue and a schema — and under it a table name to a position
//! (architecture document, §8). Keyed by the profile rather than by the open
//! connection because an arrangement is worth more than a tab: closing the tab
//! and opening it again finds the diagram as it was left, and two tabs on one
//! profile are two views of one schema and share the arrangement.
//!
//! Only the *positions* are here. The model itself — the tables, their columns
//! and the foreign keys between them — is rebuilt from the catalogue every time
//! a diagram is opened, because a schema that changed underneath a saved copy
//! is worse than no copy at all. A table the file knows nothing about takes the
//! grid slot [`rudbman_erd::ErdView::set_model`] gave it, and a table the file
//! remembers that the schema has since dropped simply never comes up.
//!
//! Loading is forgiving and writing is atomic, exactly as everything in
//! `rudbman-core` is: a missing file is a first run, a byte order mark is
//! stripped, and the data lands in a temporary sibling that is renamed over the
//! destination.

use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use rudbman_core::erd_layouts_dir;
use rudbman_core::paths::{strip_bom, write_atomic};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::explorer::Scope;

/// Schema version written by this build.
///
/// Informational, like the one on `AppSettings`: every field defaults, so a
/// file carrying another number still loads.
const CURRENT_VERSION: u32 = 1;

/// One table's top-left corner, in the diagram's own coordinates.
///
/// A struct rather than a two-element array so that a hand-edited file names
/// what it is setting.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Position {
    /// Distance from the diagram's origin, rightwards.
    pub x: f32,
    /// Distance from the diagram's origin, downwards.
    pub y: f32,
}

/// Every table position of one scope.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ScopeLayout {
    /// The catalogue, when the scope names one.
    #[serde(default)]
    pub catalog: Option<String>,
    /// The schema, likewise.
    #[serde(default)]
    pub schema: Option<String>,
    /// Where each table's box is, keyed by the table's name.
    ///
    /// Ordered, so that saving an arrangement the user did not change rewrites
    /// the same bytes rather than churning the file between runs.
    #[serde(default)]
    pub tables: BTreeMap<String, Position>,
}

/// One profile's saved diagram layouts.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ErdLayouts {
    /// Schema version of the file; see [`CURRENT_VERSION`].
    #[serde(default)]
    pub version: u32,
    /// One entry per scope a diagram has been opened on.
    #[serde(default)]
    pub scopes: Vec<ScopeLayout>,
}

impl ErdLayouts {
    /// The file one profile's layouts live in.
    ///
    /// # Errors
    ///
    /// Fails when no home directory can be determined for the current user.
    pub fn file_for(profile: Uuid) -> Result<PathBuf> {
        Ok(erd_layouts_dir()?.join(format!("{profile}.json")))
    }

    /// Reads the layouts of one profile.
    ///
    /// # Errors
    ///
    /// Fails for the reasons [`ErdLayouts::load_from`] does, plus a home
    /// directory that cannot be determined.
    pub fn load(profile: Uuid) -> Result<Self> {
        Self::load_from(&Self::file_for(profile)?)
    }

    /// Reads the layouts from an explicit path.
    ///
    /// A missing file is a first run and yields the default. A leading UTF-8
    /// byte order mark is tolerated, and every field defaults.
    ///
    /// # Errors
    ///
    /// Fails when the file cannot be read or does not contain valid JSON.
    pub fn load_from(path: &Path) -> Result<Self> {
        let data = match fs::read(path) {
            Ok(data) => data,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(Self::default()),
            Err(err) => {
                return Err(err).with_context(|| format!("failed to read {}", path.display()));
            }
        };
        serde_json::from_slice(strip_bom(&data))
            .with_context(|| format!("failed to parse ERD layouts from {}", path.display()))
    }

    /// Writes the layouts of one profile.
    ///
    /// # Errors
    ///
    /// Fails for the reasons [`ErdLayouts::save_to`] does, plus a home
    /// directory that cannot be determined.
    pub fn save(&self, profile: Uuid) -> Result<()> {
        self.save_to(&Self::file_for(profile)?)
    }

    /// Writes the layouts to an explicit path, creating parent directories.
    ///
    /// The write is atomic: the data lands in a temporary sibling file that is
    /// renamed over `path`.
    ///
    /// # Errors
    ///
    /// Fails when the parent directory cannot be created, the file cannot be
    /// written, or the value cannot be serialized.
    pub fn save_to(&self, path: &Path) -> Result<()> {
        let json = serde_json::to_vec_pretty(self).context("failed to serialize ERD layouts")?;
        write_atomic(path, &json)
    }

    /// Where `scope`'s boxes were left, in the shape [`rudbman_erd::ErdView`]
    /// takes them.
    ///
    /// An empty map for a scope the file says nothing about, which is what
    /// makes a first open fall back to the grid layout for every table.
    pub fn positions(&self, scope: &Scope) -> HashMap<String, (f32, f32)> {
        self.scopes
            .iter()
            .find(|entry| entry.catalog == scope.catalog && entry.schema == scope.schema)
            .map(|entry| {
                entry
                    .tables
                    .iter()
                    .map(|(name, at)| (name.clone(), (at.x, at.y)))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Replaces `scope`'s boxes with `positions`, adding the scope if it is new.
    ///
    /// A replacement rather than a merge: the positions handed over are every
    /// table of the diagram that produced them, so anything left behind would
    /// be a table the schema no longer has.
    pub fn set_positions(&mut self, scope: &Scope, positions: HashMap<String, (f32, f32)>) {
        self.version = CURRENT_VERSION;
        let tables: BTreeMap<String, Position> = positions
            .into_iter()
            .map(|(name, (x, y))| (name, Position { x, y }))
            .collect();
        match self
            .scopes
            .iter_mut()
            .find(|entry| entry.catalog == scope.catalog && entry.schema == scope.schema)
        {
            Some(entry) => entry.tables = tables,
            None => self.scopes.push(ScopeLayout {
                catalog: scope.catalog.clone(),
                schema: scope.schema.clone(),
                tables,
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scope(schema: &str) -> Scope {
        Scope {
            catalog: None,
            schema: Some(schema.to_string()),
        }
    }

    #[test]
    fn a_file_that_is_not_there_yet_is_a_first_run() {
        let dir = tempfile::tempdir().expect("a temporary directory");
        let layouts =
            ErdLayouts::load_from(&dir.path().join("nobody.json")).expect("a missing file loads");
        assert_eq!(layouts, ErdLayouts::default());
        assert!(layouts.positions(&scope("PUBLIC")).is_empty());
    }

    #[test]
    fn positions_survive_the_round_trip_scope_by_scope() {
        let dir = tempfile::tempdir().expect("a temporary directory");
        let path = dir.path().join("profile.json");

        let mut layouts = ErdLayouts::default();
        layouts.set_positions(
            &scope("PUBLIC"),
            HashMap::from([
                ("ORDERS".to_string(), (40., 40.)),
                ("CUSTOMERS".to_string(), (320., 40.)),
            ]),
        );
        // A second scope of the same profile is a second entry, not a merge:
        // two schemas routinely hold a table of the same name.
        layouts.set_positions(&scope("APP"), HashMap::from([("ORDERS".into(), (7., 9.))]));
        layouts.save_to(&path).expect("the file is written");

        let read = ErdLayouts::load_from(&path).expect("the file is read back");
        assert_eq!(read.version, CURRENT_VERSION);
        assert_eq!(read.scopes.len(), 2);
        let public = read.positions(&scope("PUBLIC"));
        assert_eq!(public.get("ORDERS"), Some(&(40., 40.)));
        assert_eq!(public.get("CUSTOMERS"), Some(&(320., 40.)));
        assert_eq!(read.positions(&scope("APP")).get("ORDERS"), Some(&(7., 9.)));
        // And a scope nobody has arranged still answers with nothing.
        assert!(read.positions(&scope("OTHER")).is_empty());
    }

    #[test]
    fn saving_a_scope_again_replaces_what_it_held() {
        let mut layouts = ErdLayouts::default();
        layouts.set_positions(
            &scope("PUBLIC"),
            HashMap::from([
                ("ORDERS".to_string(), (10., 10.)),
                ("DROPPED".to_string(), (20., 20.)),
            ]),
        );
        layouts.set_positions(
            &scope("PUBLIC"),
            HashMap::from([("ORDERS".to_string(), (99., 99.))]),
        );

        assert_eq!(layouts.scopes.len(), 1, "the scope was duplicated");
        let positions = layouts.positions(&scope("PUBLIC"));
        assert_eq!(positions.get("ORDERS"), Some(&(99., 99.)));
        assert!(
            !positions.contains_key("DROPPED"),
            "a table the diagram no longer has was kept: {positions:?}"
        );
    }

    #[test]
    fn a_byte_order_mark_is_tolerated() {
        let dir = tempfile::tempdir().expect("a temporary directory");
        let path = dir.path().join("bom.json");
        let mut bytes = b"\xEF\xBB\xBF".to_vec();
        bytes.extend_from_slice(
            br#"{"version":1,"scopes":[{"schema":"PUBLIC","tables":{"T":{"x":1.0,"y":2.0}}}]}"#,
        );
        fs::write(&path, bytes).expect("the fixture is written");

        let layouts = ErdLayouts::load_from(&path).expect("a BOM is not a parse error");
        assert_eq!(
            layouts.positions(&scope("PUBLIC")).get("T"),
            Some(&(1., 2.))
        );
    }
}
