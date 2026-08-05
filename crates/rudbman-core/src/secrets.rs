//! Storage of connection secrets in the operating system keychain.
//!
//! Secrets are keyed by the
//! [`ConnectionProfile`](crate::ConnectionProfile) identifier inside the
//! `rudbman` service namespace, so neither `connections.json` nor a profile
//! struct ever contains a password.
//!
//! A profile has two secrets to keep apart — the database password and the SSH
//! tunnel's password or key passphrase — so the account name carries a
//! [`SecretSlot`]: the plain profile id for the connection, `<id>:tunnel` for
//! the tunnel. See the architecture document, §8 and §9.2.
//!
//! The backing store is the platform default provided by `keyring` 4.x: the
//! Windows Credential Manager, the macOS Keychain, or the freedesktop Secret
//! Service. Machines without any of those (a headless Linux box, for instance)
//! are supported in a degraded mode: [`init`] reports the failure and
//! [`SecretStore::get`] then behaves as if no secret had ever been saved.

use std::fmt;
use std::sync::OnceLock;

use anyhow::{Result, anyhow};
use keyring::{Entry, Error as KeyringError};
use uuid::Uuid;

/// Service namespace used for every credential rudbman stores.
const SERVICE: &str = "rudbman";

/// Suffix appended to the profile id for the tunnel's secret.
const TUNNEL_SUFFIX: &str = ":tunnel";

/// Account name used by [`init`] to force the credential store to load.
///
/// Building an entry never creates a credential, so this leaves no trace in the
/// keychain.
const INIT_PROBE_ACCOUNT: &str = "__rudbman_store_probe__";

/// Cached outcome of the first [`init`] call: `None` on success, otherwise the
/// rendered error.
static INIT_OUTCOME: OnceLock<Option<String>> = OnceLock::new();

/// Which of a profile's secrets an operation addresses.
///
/// A connection through a bastion needs two independent credentials, and they
/// are rotated independently, so they get one keychain entry each rather than
/// one entry holding both.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SecretSlot {
    /// The database password, stored under the bare profile id.
    Connection,
    /// The SSH tunnel's password or key passphrase, stored under
    /// `<profile-id>:tunnel`.
    Tunnel,
}

impl SecretSlot {
    /// Keychain account name for this slot of the profile `id`.
    fn account(self, id: Uuid) -> String {
        match self {
            Self::Connection => id.to_string(),
            Self::Tunnel => format!("{id}{TUNNEL_SUFFIX}"),
        }
    }
}

impl fmt::Display for SecretSlot {
    /// Names the slot for log and error messages.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Connection => f.write_str("connection"),
            Self::Tunnel => f.write_str("tunnel"),
        }
    }
}

/// Install the platform credential store.
///
/// Call this once during start-up. Repeated calls are cheap and return the same
/// result as the first one; the store is installed at most once per process.
///
/// # Errors
///
/// Fails when the platform has no usable credential store (a locked or absent
/// Secret Service, for example). Callers may ignore the error and keep running:
/// [`SecretStore::get`] degrades to "no stored secret" in that case, while
/// [`SecretStore::set`] reports the failure.
pub fn init() -> Result<()> {
    // `keyring::Entry::new` installs the platform default store the first time
    // it runs, which is the only way this crate exposes that step.
    let outcome = INIT_OUTCOME.get_or_init(|| match Entry::new(SERVICE, INIT_PROBE_ACCOUNT) {
        Ok(_) => None,
        Err(err) => Some(err.to_string()),
    });
    match outcome {
        None => Ok(()),
        Some(err) => Err(anyhow!("no usable credential store on this system: {err}")),
    }
}

/// Accessor for the OS keychain, keyed by profile identifier and slot.
///
/// The type is a namespace only; there is nothing to construct.
pub struct SecretStore;

impl SecretStore {
    /// Build the keychain entry for `id`/`slot`, or `None` when no store is
    /// installed.
    fn entry(id: Uuid, slot: SecretSlot) -> Result<Option<Entry>> {
        match Entry::new(SERVICE, &slot.account(id)) {
            Ok(entry) => Ok(Some(entry)),
            Err(KeyringError::NoDefaultStore) => Ok(None),
            Err(err) => Err(anyhow!(
                "failed to address the {slot} keychain entry for {id}: {err}"
            )),
        }
    }

    /// Read the secret saved for the profile `id` in `slot`.
    ///
    /// Returns `Ok(None)` when nothing is stored, and also when the platform has
    /// no usable keychain at all, so that the application keeps working without
    /// one — the user is asked for the password instead.
    ///
    /// # Errors
    ///
    /// Fails only when a working keychain refuses the read (locked store,
    /// denied access, non-UTF-8 payload).
    pub fn get(id: Uuid, slot: SecretSlot) -> Result<Option<String>> {
        if let Err(err) = init() {
            log::warn!("treating the {slot} secret for {id} as absent: {err:#}");
            return Ok(None);
        }
        let Some(entry) = Self::entry(id, slot)? else {
            return Ok(None);
        };
        match entry.get_password() {
            Ok(secret) => Ok(Some(secret)),
            Err(KeyringError::NoEntry) => Ok(None),
            Err(err) => Err(anyhow!(
                "failed to read the {slot} keychain entry for {id}: {err}"
            )),
        }
    }

    /// Save `secret` for the profile `id` in `slot`, replacing any previous
    /// value.
    ///
    /// # Errors
    ///
    /// Fails when no credential store is available or when the store rejects
    /// the write. Unlike [`SecretStore::get`] this never fails silently: a
    /// secret the user asked to save must not vanish unnoticed.
    pub fn set(id: Uuid, slot: SecretSlot, secret: &str) -> Result<()> {
        init()?;
        let entry = Self::entry(id, slot)?.ok_or_else(|| {
            anyhow!("no credential store available to save the {slot} secret for {id}")
        })?;
        entry
            .set_password(secret)
            .map_err(|err| anyhow!("failed to save the {slot} keychain entry for {id}: {err}"))
    }

    /// Delete the secret saved for the profile `id` in `slot`.
    ///
    /// Deleting a secret that does not exist succeeds, as does deleting on a
    /// machine without a credential store: in both cases nothing is left behind.
    ///
    /// # Errors
    ///
    /// Fails when a working keychain refuses the deletion.
    pub fn delete(id: Uuid, slot: SecretSlot) -> Result<()> {
        if let Err(err) = init() {
            log::warn!("nothing to delete for the {slot} secret of {id}: {err:#}");
            return Ok(());
        }
        let Some(entry) = Self::entry(id, slot)? else {
            return Ok(());
        };
        match entry.delete_credential() {
            Ok(()) | Err(KeyringError::NoEntry) => Ok(()),
            Err(err) => Err(anyhow!(
                "failed to delete the {slot} keychain entry for {id}: {err}"
            )),
        }
    }

    /// Delete every secret belonging to the profile `id`.
    ///
    /// Called when a profile is removed: leaving a password behind for an id
    /// nothing references any more is a leak the user cannot see, let alone
    /// clean up.
    ///
    /// # Errors
    ///
    /// Fails when a working keychain refuses one of the deletions.
    pub fn delete_all(id: Uuid) -> Result<()> {
        Self::delete(id, SecretSlot::Connection)?;
        Self::delete(id, SecretSlot::Tunnel)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_service_namespace_is_the_app_name() {
        // The architecture document spells the keychain key `rudbman:<id>`.
        assert_eq!(SERVICE, "rudbman");
    }

    #[test]
    fn the_two_slots_of_a_profile_get_distinct_accounts() {
        let id = Uuid::new_v4();
        let connection = SecretSlot::Connection.account(id);
        let tunnel = SecretSlot::Tunnel.account(id);

        assert_eq!(connection, id.to_string());
        assert_eq!(tunnel, format!("{id}:tunnel"));
        assert_ne!(connection, tunnel);
    }

    #[test]
    fn accounts_of_different_profiles_never_collide() {
        let a = Uuid::new_v4();
        let b = Uuid::new_v4();
        assert_ne!(
            SecretSlot::Connection.account(a),
            SecretSlot::Connection.account(b)
        );
        // A profile's tunnel account must not be another profile's connection
        // account, which the `:tunnel` suffix guarantees since a UUID cannot
        // contain a colon.
        assert!(!SecretSlot::Connection.account(a).contains(':'));
    }

    #[test]
    fn slots_name_themselves_for_error_messages() {
        assert_eq!(SecretSlot::Connection.to_string(), "connection");
        assert_eq!(SecretSlot::Tunnel.to_string(), "tunnel");
    }

    #[test]
    #[ignore = "touches the real OS keychain"]
    fn init_is_idempotent() {
        // Installing the store may legitimately fail (headless CI), but the
        // answer must be stable across calls and must never panic.
        let first = init().is_ok();
        let second = init().is_ok();
        assert_eq!(first, second);
    }

    #[test]
    #[ignore = "touches the real OS keychain"]
    fn get_of_unknown_id_is_none() {
        // On a machine with no keychain this exercises the degraded path; with
        // one, it exercises the `NoEntry` path. Either way: `Ok(None)`.
        let id = Uuid::new_v4();
        assert_eq!(
            SecretStore::get(id, SecretSlot::Connection).expect("get must not fail"),
            None
        );
        assert_eq!(
            SecretStore::get(id, SecretSlot::Tunnel).expect("get must not fail"),
            None
        );
    }

    #[test]
    #[ignore = "touches the real OS keychain"]
    fn set_get_delete_round_trip() {
        init().expect("credential store");
        let id = Uuid::new_v4();

        SecretStore::set(id, SecretSlot::Connection, "hunter2").expect("set");
        SecretStore::set(id, SecretSlot::Tunnel, "passphrase").expect("set tunnel");

        assert_eq!(
            SecretStore::get(id, SecretSlot::Connection).expect("get"),
            Some("hunter2".into())
        );
        assert_eq!(
            SecretStore::get(id, SecretSlot::Tunnel).expect("get tunnel"),
            Some("passphrase".into()),
            "the two slots must not overwrite each other"
        );

        SecretStore::set(id, SecretSlot::Connection, "hunter3").expect("overwrite");
        assert_eq!(
            SecretStore::get(id, SecretSlot::Connection).expect("get"),
            Some("hunter3".into())
        );

        SecretStore::delete_all(id).expect("delete all");
        assert_eq!(
            SecretStore::get(id, SecretSlot::Connection).expect("get deleted"),
            None
        );
        assert_eq!(
            SecretStore::get(id, SecretSlot::Tunnel).expect("get deleted"),
            None
        );
    }

    #[test]
    #[ignore = "touches the real OS keychain"]
    fn delete_missing_entry_is_ok() {
        init().expect("credential store");
        SecretStore::delete_all(Uuid::new_v4()).expect("delete of missing entries must succeed");
    }
}
