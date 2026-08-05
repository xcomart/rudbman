//! Platform-independent core of rudbman: configuration paths, application
//! settings, saved connection profiles, JDBC driver definitions, OS keychain
//! access, and the trusted host key database.
//!
//! This crate owns everything rudbman persists on disk or in the system
//! credential store. It knows nothing about JNI, SQL, SSH transport, or the
//! GUI, so it can be exercised entirely from tests.
//!
//! Two rules run through all of it. Loading is forgiving — every file here is
//! meant to be hand-editable, so a missing file is a first run, a UTF-8 byte
//! order mark is stripped, missing keys default and out-of-range numbers are
//! clamped. Writing is atomic — the data lands in a temporary sibling file that
//! is renamed over the destination, so a crash mid-save cannot leave a
//! truncated configuration behind.
//!
//! ```no_run
//! use rudbman_core::{ConnectionProfile, ConnectionStore, DriverStore};
//!
//! # fn main() -> anyhow::Result<()> {
//! rudbman_core::init_secrets().ok(); // a missing keychain is not fatal
//!
//! let drivers = DriverStore::load()?; // built-in definitions on a first run
//! let postgres = drivers.get("postgresql").expect("built-in driver");
//!
//! let mut store = ConnectionStore::load()?;
//! store.upsert(ConnectionProfile::new(
//!     "staging",
//!     &postgres.id,
//!     "jdbc:postgresql://db.example.com:5432/app",
//!     "alice",
//! ));
//! store.save()?;
//! # Ok(())
//! # }
//! ```

#![warn(missing_docs)]

pub mod known_hosts;
pub mod paths;
pub mod profile;
pub mod secrets;
pub mod settings;

pub use known_hosts::{HostKeyStatus, KnownHosts};
pub use paths::{
    config_dir, connections_file, drivers_dir, drivers_file, editor_themes_dir, known_hosts_file,
    settings_file, snippets_dir, ui_themes_dir,
};
pub use profile::{
    ConnectionProfile, ConnectionStore, DriverDef, DriverStore, KeepAlive, TunnelAuth, TunnelConfig,
};
pub use secrets::{SecretSlot, SecretStore, init as init_secrets};
pub use settings::{AppSettings, TitlebarStyle, WindowState};
