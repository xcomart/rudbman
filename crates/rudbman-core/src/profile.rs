//! Saved database connection profiles, JDBC driver definitions, and their
//! JSON-backed stores.
//!
//! Two files live here, both under [`crate::paths::config_dir`]:
//!
//! * `connections.json` — a [`ConnectionStore`] of [`ConnectionProfile`]s: what
//!   to connect to, as which user, through which driver, optionally through an
//!   SSH tunnel.
//! * `drivers.json` — a [`DriverStore`] of [`DriverDef`]s: which JDBC driver
//!   classes exist, which JARs they come from, and how their URLs are shaped.
//!   A machine without the file starts from [`DriverDef::builtins`].
//!
//! **Neither file ever contains a password.** Connection and tunnel secrets go
//! into the OS keychain through [`SecretStore`](crate::SecretStore), keyed by
//! the profile [`Uuid`]. The [`Debug`] implementations in this module are
//! written by hand for the same reason: a profile can still carry
//! user-supplied secrets in [`ConnectionProfile::props`] and in the JDBC URL
//! itself, and a debug print must not leak them into a log file or a panic
//! message.
//!
//! Both stores follow the same discipline as `settings.json`: a missing file is
//! a first run rather than an error, a UTF-8 BOM is tolerated, unknown keys are
//! ignored so a file written by a newer build still opens, missing keys fall
//! back to documented defaults, and every write is atomic.

use std::collections::BTreeMap;
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::de::Error as _;
use serde::{Deserialize, Deserializer, Serialize};
use uuid::Uuid;

use crate::paths::{connections_file, drivers_file, strip_bom, write_atomic};

/// Default SSH port, used when a hand-written tunnel block omits one.
const DEFAULT_SSH_PORT: u16 = 22;

/// Seconds between keep-alive probes when a block omits the interval.
const DEFAULT_KEEP_ALIVE_INTERVAL_S: u32 = 300;

/// Probe statement used when a keep-alive block omits one.
///
/// `select 1` is valid on every dialect rudbman ships a driver for except
/// Oracle, which needs `from dual`; the profile editor is expected to offer the
/// dialect-appropriate default, so this is only the last resort.
const DEFAULT_KEEP_ALIVE_QUERY: &str = "select 1";

/// Placeholder rendered in place of a secret by the manual [`Debug`] impls.
struct Redacted;

impl fmt::Debug for Redacted {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("<redacted>")
    }
}

/// Driver-specific connection properties, rendered with their values masked.
///
/// Property maps are where a JDBC driver takes anything the URL cannot express
/// — including, for several drivers, `password` and key store passphrases. The
/// keys are useful when diagnosing a connection failure and are not secret; the
/// values are assumed to be.
struct MaskedProps<'a>(&'a BTreeMap<String, String>);

impl fmt::Debug for MaskedProps<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_map()
            .entries(self.0.keys().map(|key| (key, &Redacted)))
            .finish()
    }
}

/// Characters that start the parameter section of a JDBC URL.
///
/// `?` is the URI query used by the PostgreSQL and MySQL families; `;` is the
/// property list used by SQL Server and H2. No driver uses both, so whichever
/// comes first ends the base.
const URL_PARAM_START: [char; 2] = ['?', ';'];

/// Characters that separate one URL parameter from the next.
const URL_PARAM_SEPARATORS: [char; 2] = ['&', ';'];

/// A JDBC URL, rendered with everything that could be a credential removed.
///
/// A JDBC URL is the second home of the password — every one of these is a
/// shape a driver accepts:
///
/// ```text
/// jdbc:sqlserver://host:1433;databaseName=db;user=x;password=y
/// jdbc:postgresql://host:5432/db?user=x&password=y
/// jdbc:postgresql://user:pass@host/db
/// jdbc:oracle:thin:user/pass@//host:1521/service
/// ```
///
/// so a profile that reaches a log or a panic message as `{profile:?}` would
/// otherwise undo the entire point of keeping the password in the keychain.
///
/// Two rules, and both err towards hiding too much:
///
/// * **Parameters.** Everything after the first `?` or `;` is split on `&` and
///   `;`, and the value of every `key=value` is replaced while the key stays.
///   The values are *not* filtered by name — the same discipline as
///   [`MaskedProps`]. A hidden `ssl=true` is a mild inconvenience; one missed
///   spelling of `password` (`pwd`, `passwd`, a driver-specific alias, a
///   property the next driver invents) is a leaked credential. A token with no
///   `=` in it carries no value and is left alone.
/// * **User info.** An `@` in the base means credentials precede it. Everything
///   from the *last* `:` before that `@` up to the `@` is replaced. The last
///   colon is what lets one rule cover both spellings: the URI form
///   `//user:pass@host` keeps the user name and drops the password, while
///   Oracle's `thin:user/pass@//host` — where the separator is a slash, not a
///   colon — falls back to the scheme's own colon and hides the whole
///   credential. A URL with an `@` but no password (`//user@host`) is masked
///   too; erring towards a lost user name is the cheap direction. The one
///   exception is an *empty* span, as in Oracle's ordinary
///   `thin:@//host:1521/service`: there is nothing there to hide.
///
/// A URL with neither an `@` nor parameters — which is most of them — is shown
/// verbatim, because a log line has to say *which* connection it is about.
///
/// Only the rendering changes: [`ConnectionProfile::url`] still holds the real
/// URL, which is what gets handed to the driver.
struct MaskedUrl<'a>(&'a str);

impl MaskedUrl<'_> {
    /// Write the base of the URL, with any user info replaced.
    fn write_base(base: &str, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let Some(at) = base.find('@') else {
            return f.write_str(base);
        };
        // Byte indices are safe here: both `@` and `:` are ASCII, so a split at
        // one of them never lands inside a multi-byte character.
        let credentials_start = base[..at].rfind(':').map_or(0, |colon| colon + 1);
        if credentials_start == at {
            // Nothing between the colon and the `@` to hide. Oracle's ordinary
            // `jdbc:oracle:thin:@//host:1521/service` lands here, and printing
            // `<redacted>` for an empty span would only be noise.
            return f.write_str(base);
        }
        f.write_str(&base[..credentials_start])?;
        f.write_str("<redacted>")?;
        f.write_str(&base[at..])
    }

    /// Write the parameter section, with every value replaced.
    fn write_params(params: &str, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut rest = params;
        loop {
            // The separator itself is kept, so the shape of the URL — and with
            // it the driver it belongs to — survives the masking.
            let (token, separator, tail) = match rest.find(URL_PARAM_SEPARATORS) {
                Some(index) => (
                    &rest[..index],
                    Some(&rest[index..index + 1]),
                    &rest[index + 1..],
                ),
                None => (rest, None, ""),
            };
            match token.find('=') {
                Some(equals) => {
                    f.write_str(&token[..=equals])?;
                    f.write_str("<redacted>")?;
                }
                None => f.write_str(token)?,
            }
            let Some(separator) = separator else {
                return Ok(());
            };
            f.write_str(separator)?;
            rest = tail;
        }
    }
}

impl fmt::Debug for MaskedUrl<'_> {
    /// Renders the URL as a quoted string, the way a derived `Debug` would.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("\"")?;
        match self.0.find(URL_PARAM_START) {
            Some(index) => {
                let (base, params) = self.0.split_at(index);
                Self::write_base(base, f)?;
                f.write_str(&params[..1])?;
                Self::write_params(&params[1..], f)?;
            }
            None => Self::write_base(self.0, f)?,
        }
        f.write_str("\"")
    }
}

/// Deserialize an optional block that the documented JSON gates with `enabled`.
///
/// The schema in the architecture document writes `keep_alive` and `tunnel` as
/// objects carrying an `"enabled": true` flag, while the Rust side models
/// "switched off" as [`None`] — one representation of the state instead of two
/// that can disagree. This bridges the two: a missing block, an explicit
/// `null`, and a block with `"enabled": false` all load as [`None`], so a user
/// who flips the flag in a hand-edited file gets what they asked for. The flag
/// is not written back; presence *is* the flag on the way out.
fn deserialize_gated_block<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    let Some(value) = Option::<serde_json::Value>::deserialize(deserializer)? else {
        return Ok(None);
    };
    if value.is_null() {
        return Ok(None);
    }
    let enabled = value
        .get("enabled")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(true);
    if !enabled {
        return Ok(None);
    }
    T::deserialize(value).map(Some).map_err(D::Error::custom)
}

/// Periodic probe that keeps an idle connection from being reaped.
///
/// Firewalls and connection poolers drop idle TCP sessions without telling
/// either end, and the failure only surfaces on the next real statement — long
/// after the user has typed it. A cheap statement on a timer keeps the socket
/// warm and turns a silent death into an error the session can report.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct KeepAlive {
    /// Seconds between probes.
    pub interval_s: u32,
    /// Statement sent as the probe, e.g. `select 1 from dual` on Oracle.
    pub query: String,
}

impl Default for KeepAlive {
    fn default() -> Self {
        Self {
            interval_s: DEFAULT_KEEP_ALIVE_INTERVAL_S,
            query: DEFAULT_KEEP_ALIVE_QUERY.to_string(),
        }
    }
}

/// How the SSH tunnel authenticates against the bastion host.
///
/// Serialized as an internally tagged enum, e.g.
/// `{"kind":"key","path":"/home/me/.ssh/id_ed25519"}`. A bare string —
/// `"agent"`, `"key"`, `"password"` — is also accepted on the way in, because
/// that is how the architecture document spells the field.
///
/// No variant carries a secret: the password and the key passphrase both live
/// in the keychain under
/// [`SecretSlot::Tunnel`](crate::SecretSlot::Tunnel).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Default)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TunnelAuth {
    /// Delegate authentication to a running SSH agent. The default: it is the
    /// only method that needs nothing stored anywhere.
    #[default]
    Agent,
    /// Public key authentication using the private key at `path`.
    Key {
        /// Path of the private key file to offer to the bastion.
        path: PathBuf,
    },
    /// Password authentication, with the password read from the keychain.
    Password,
}

/// On-disk shapes [`TunnelAuth`] accepts.
///
/// The tagged form is what this build writes. The bare string is what the
/// architecture document shows and what a user is most likely to type by hand;
/// with it, the key path comes from the `key_path` sibling on the tunnel block
/// (see [`TunnelConfigRepr`]).
#[derive(Deserialize)]
#[serde(untagged)]
enum TunnelAuthRepr {
    /// `"agent"` / `"key"` / `"password"`.
    Bare(String),
    /// `{"kind": "key", "path": "..."}`.
    Tagged {
        /// Which method; unknown values fall back to the agent.
        kind: String,
        /// Key path, spelled either way round.
        #[serde(default, alias = "key_path")]
        path: Option<PathBuf>,
    },
}

impl<'de> Deserialize<'de> for TunnelAuth {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let (kind, path) = match TunnelAuthRepr::deserialize(deserializer)? {
            TunnelAuthRepr::Bare(kind) => (kind, None),
            TunnelAuthRepr::Tagged { kind, path } => (kind, path),
        };
        Ok(TunnelAuth::from_parts(&kind, path))
    }
}

impl TunnelAuth {
    /// Build a variant from a method name and an optional key path.
    ///
    /// An unrecognized name degrades to [`TunnelAuth::Agent`] rather than
    /// failing the load: one mistyped word must not cost the user the rest of
    /// `connections.json`, and the agent is the method that cannot leak a
    /// secret if it is the wrong guess.
    fn from_parts(kind: &str, path: Option<PathBuf>) -> Self {
        match kind.trim().to_ascii_lowercase().as_str() {
            "key" | "public_key" | "publickey" => Self::Key {
                path: path.unwrap_or_default(),
            },
            "password" => Self::Password,
            "agent" => Self::Agent,
            other => {
                log::warn!("unknown tunnel auth method {other:?}, falling back to the SSH agent");
                Self::Agent
            }
        }
    }
}

/// SSH local port forwarding for one connection profile.
///
/// Production databases usually sit behind a bastion host: rudbman opens a
/// forwarding channel to the bastion, binds a local port, and points JDBC at
/// that port. See the architecture document, §9.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(from = "TunnelConfigRepr")]
pub struct TunnelConfig {
    /// Hostname or IP address of the bastion.
    pub host: String,
    /// TCP port of the bastion's SSH service.
    pub port: u16,
    /// Login user on the bastion.
    pub username: String,
    /// How to authenticate against the bastion.
    pub auth: TunnelAuth,
    /// Host the bastion should connect onwards to — the database, as named
    /// from *inside* the remote network.
    pub remote_host: String,
    /// Port of the database on `remote_host`.
    pub remote_port: u16,
    /// Local port to bind; `0` — the default — lets the OS pick a free one.
    ///
    /// A fixed port is a footgun: two profiles that name the same one cannot be
    /// open at the same time. The actual bound port is what gets substituted
    /// into the JDBC URL, so nothing downstream needs to know which it was.
    pub local_port: u16,
}

impl Default for TunnelConfig {
    fn default() -> Self {
        Self {
            host: String::new(),
            port: DEFAULT_SSH_PORT,
            username: String::new(),
            auth: TunnelAuth::default(),
            remote_host: String::new(),
            remote_port: 0,
            local_port: 0,
        }
    }
}

/// On-disk shape of [`TunnelConfig`], with the fields the document puts beside
/// the auth method rather than inside it.
///
/// Only the `key_path` sibling makes this necessary: the document writes
/// `"auth": "key", "key_path": "~/.ssh/id_ed25519"`, and the path has to be
/// folded into [`TunnelAuth::Key`] before the public struct sees it.
#[derive(Deserialize)]
#[serde(default)]
struct TunnelConfigRepr {
    host: String,
    port: u16,
    username: String,
    auth: TunnelAuth,
    key_path: Option<PathBuf>,
    remote_host: String,
    remote_port: u16,
    local_port: u16,
}

impl Default for TunnelConfigRepr {
    fn default() -> Self {
        let base = TunnelConfig::default();
        Self {
            host: base.host,
            port: base.port,
            username: base.username,
            auth: base.auth,
            key_path: None,
            remote_host: base.remote_host,
            remote_port: base.remote_port,
            local_port: base.local_port,
        }
    }
}

impl From<TunnelConfigRepr> for TunnelConfig {
    fn from(repr: TunnelConfigRepr) -> Self {
        let auth = match (repr.auth, repr.key_path) {
            // A sibling `key_path` fills in a key whose own path is empty; an
            // inline path wins, since it is the more specific spelling.
            (TunnelAuth::Key { path }, Some(sibling)) if path.as_os_str().is_empty() => {
                TunnelAuth::Key { path: sibling }
            }
            (auth, _) => auth,
        };
        Self {
            host: repr.host,
            port: repr.port,
            username: repr.username,
            auth,
            remote_host: repr.remote_host,
            remote_port: repr.remote_port,
            local_port: repr.local_port,
        }
    }
}

impl fmt::Debug for TunnelConfig {
    /// Renders the tunnel without disclosing anything secret.
    ///
    /// Nothing here holds a secret today — [`TunnelAuth`] deliberately carries
    /// none — but the impl is written by hand anyway so that adding a field
    /// that does is a decision someone has to make in this function rather than
    /// something a `derive` does silently.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TunnelConfig")
            .field("host", &self.host)
            .field("port", &self.port)
            .field("username", &self.username)
            .field("auth", &self.auth)
            .field("remote_host", &self.remote_host)
            .field("remote_port", &self.remote_port)
            .field("local_port", &self.local_port)
            .finish()
    }
}

/// A single saved database connection.
///
/// Everything needed to open a JDBC session except the password, which lives in
/// the OS keychain under [`ConnectionProfile::id`].
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ConnectionProfile {
    /// Stable identifier; also the account key used by
    /// [`SecretStore`](crate::SecretStore).
    pub id: Uuid,
    /// Human-readable name shown in the UI.
    pub name: String,
    /// Group this profile appears under in the connection tree.
    pub folder: Option<String>,
    /// Tab color tag, `"#RRGGBB"`.
    ///
    /// Not validated here: the UI owns color parsing, and a profile with a
    /// typo in it must still open.
    pub color: Option<String>,
    /// Id of the [`DriverDef`] this profile connects through.
    pub driver_id: String,
    /// JDBC URL, already substituted — `jdbc:postgresql://db:5432/app`.
    pub url: String,
    /// Login user on the database.
    pub username: String,
    /// Driver-specific connection properties.
    ///
    /// Sorted, so that a saved file does not churn between runs. Values may be
    /// secret and are masked by the [`Debug`] impl.
    pub props: BTreeMap<String, String>,
    /// Idle keep-alive probe; `None` disables it.
    #[serde(deserialize_with = "deserialize_gated_block")]
    pub keep_alive: Option<KeepAlive>,
    /// Open the session with `Connection.setReadOnly(true)` and refuse DDL/DML.
    pub read_only: bool,
    /// Whether statements commit as they run.
    pub auto_commit: bool,
    /// Ask before executing a statement that modifies data.
    pub confirm_writes: bool,
    /// SSH tunnel to open before connecting; `None` connects directly.
    #[serde(deserialize_with = "deserialize_gated_block")]
    pub tunnel: Option<TunnelConfig>,
}

impl Default for ConnectionProfile {
    /// A blank profile with a fresh id and the safe answer to every switch.
    ///
    /// `auto_commit` and `confirm_writes` both start on: the first is what
    /// every JDBC driver does by default, and the second is the cheap half of
    /// the production-accident guard described in the architecture document,
    /// §8. `read_only` starts off — a profile the user cannot write through is
    /// a deliberate choice, not a default.
    fn default() -> Self {
        Self {
            id: Uuid::new_v4(),
            name: String::new(),
            folder: None,
            color: None,
            driver_id: String::new(),
            url: String::new(),
            username: String::new(),
            props: BTreeMap::new(),
            keep_alive: None,
            read_only: false,
            auto_commit: true,
            confirm_writes: true,
            tunnel: None,
        }
    }
}

impl ConnectionProfile {
    /// Create a profile with a freshly generated identifier.
    ///
    /// Everything not passed here — properties, keep-alive, tunnel — starts at
    /// the [`Default`] value and is filled in by the profile editor.
    pub fn new(
        name: impl Into<String>,
        driver_id: impl Into<String>,
        url: impl Into<String>,
        username: impl Into<String>,
    ) -> Self {
        Self {
            name: name.into(),
            driver_id: driver_id.into(),
            url: url.into(),
            username: username.into(),
            ..Self::default()
        }
    }

    /// Connection target in `user@url` form for the tab title and tooltips.
    pub fn label(&self) -> String {
        if self.username.is_empty() {
            self.url.clone()
        } else {
            format!("{}@{}", self.username, self.url)
        }
    }
}

impl fmt::Debug for ConnectionProfile {
    /// Renders the profile with every possibly-secret value masked.
    ///
    /// The profile has no password *field*, but it has two places a password
    /// can still be sitting, and both are masked here: the free-form
    /// [`ConnectionProfile::props`] map that several JDBC drivers expect a
    /// `password` in (see [`MaskedProps`]), and [`ConnectionProfile::url`],
    /// which every driver family accepts credentials in one way or another
    /// (see [`MaskedUrl`]).
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ConnectionProfile")
            .field("id", &self.id)
            .field("name", &self.name)
            .field("folder", &self.folder)
            .field("color", &self.color)
            .field("driver_id", &self.driver_id)
            .field("url", &MaskedUrl(&self.url))
            .field("username", &self.username)
            .field("props", &MaskedProps(&self.props))
            .field("keep_alive", &self.keep_alive)
            .field("read_only", &self.read_only)
            .field("auto_commit", &self.auto_commit)
            .field("confirm_writes", &self.confirm_writes)
            .field("tunnel", &self.tunnel)
            .finish()
    }
}

/// Collection of saved [`ConnectionProfile`]s, persisted as `connections.json`.
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct ConnectionStore {
    /// Profiles in user-visible order.
    #[serde(default)]
    connections: Vec<ConnectionProfile>,
}

impl ConnectionStore {
    /// Load the store from the default configuration file.
    ///
    /// A missing file is not an error: it yields an empty store, which is what a
    /// first run looks like.
    ///
    /// # Errors
    ///
    /// Fails when the configuration directory cannot be determined, the file
    /// cannot be read, or its contents are not valid JSON.
    pub fn load() -> Result<Self> {
        Self::load_from(&connections_file()?)
    }

    /// Load the store from an explicit path.
    ///
    /// A missing file yields an empty store.
    ///
    /// # Errors
    ///
    /// Fails when the file cannot be read or does not contain valid JSON.
    pub fn load_from(path: &Path) -> Result<Self> {
        let Some(data) = read_optional(path)? else {
            return Ok(Self::default());
        };
        serde_json::from_slice(strip_bom(&data))
            .with_context(|| format!("failed to parse connections from {}", path.display()))
    }

    /// Write the store to the default configuration file.
    ///
    /// # Errors
    ///
    /// Fails when the configuration directory cannot be determined or created,
    /// or when the file cannot be written.
    pub fn save(&self) -> Result<()> {
        self.save_to(&connections_file()?)
    }

    /// Write the store to an explicit path, creating parent directories.
    ///
    /// The write is atomic: the data lands in a temporary sibling file that is
    /// then renamed over `path`.
    ///
    /// # Errors
    ///
    /// Fails when the parent directory cannot be created or the file cannot be
    /// written.
    pub fn save_to(&self, path: &Path) -> Result<()> {
        let json = serde_json::to_vec_pretty(self).context("failed to serialize connections")?;
        write_atomic(path, &json)
    }

    /// All profiles, in insertion order.
    pub fn connections(&self) -> &[ConnectionProfile] {
        &self.connections
    }

    /// Look up a profile by identifier.
    pub fn get(&self, id: Uuid) -> Option<&ConnectionProfile> {
        self.connections.iter().find(|p| p.id == id)
    }

    /// Insert `profile`, replacing an existing entry with the same identifier.
    ///
    /// Replacement keeps the original position in the list.
    pub fn upsert(&mut self, profile: ConnectionProfile) {
        match self.connections.iter_mut().find(|p| p.id == profile.id) {
            Some(slot) => *slot = profile,
            None => self.connections.push(profile),
        }
    }

    /// Remove the profile with the given identifier and return it.
    pub fn remove(&mut self, id: Uuid) -> Option<ConnectionProfile> {
        let index = self.connections.iter().position(|p| p.id == id)?;
        Some(self.connections.remove(index))
    }

    /// Number of stored profiles.
    pub fn len(&self) -> usize {
        self.connections.len()
    }

    /// Whether the store holds no profiles.
    pub fn is_empty(&self) -> bool {
        self.connections.is_empty()
    }
}

/// A JDBC driver rudbman can load: its class, its JARs, and its URL shape.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct DriverDef {
    /// Stable id referenced by [`ConnectionProfile::driver_id`].
    pub id: String,
    /// Human-readable name shown in the driver picker.
    pub name: String,
    /// Icon name, matching an asset stem such as `"oracle"`.
    pub icon: Option<String>,
    /// Fully qualified driver class, e.g. `oracle.jdbc.OracleDriver`.
    pub class: String,
    /// JARs to put on the driver's isolated class loader.
    ///
    /// Empty for every built-in definition: the JARs are not redistributable,
    /// so the user either downloads them from [`DriverDef::maven`] or points at
    /// copies they already have.
    pub jars: Vec<PathBuf>,
    /// Maven coordinate to download the driver from, `group:artifact:version`.
    pub maven: Option<String>,
    /// URL skeleton with `{placeholder}` holes the connection editor fills.
    pub url_template: String,
    /// Port pre-filled by the connection editor; `None` for file-backed
    /// databases that have no port at all.
    pub default_port: Option<u16>,
    /// SQL dialect id.
    ///
    /// Picks the keyword set, identifier quoting rules, DDL generator, and
    /// paging syntax used by the SQL layer, so an unknown value degrades to
    /// generic behaviour rather than failing.
    pub dialect: String,
}

impl Default for DriverDef {
    fn default() -> Self {
        Self {
            id: String::new(),
            name: String::new(),
            icon: None,
            class: String::new(),
            jars: Vec::new(),
            maven: None,
            url_template: String::new(),
            default_port: None,
            dialect: DIALECT_GENERIC.to_string(),
        }
    }
}

/// Dialect id for a database the SQL layer has no special knowledge of.
const DIALECT_GENERIC: &str = "generic";

/// One built-in driver definition, in the shape the table below is written in.
///
/// A struct rather than a tuple so that eight string columns cannot be
/// transposed by accident.
struct BuiltinDriver {
    /// Value of [`DriverDef::id`].
    id: &'static str,
    /// Value of [`DriverDef::name`].
    name: &'static str,
    /// Value of [`DriverDef::icon`]; every built-in has one.
    icon: &'static str,
    /// Value of [`DriverDef::class`].
    class: &'static str,
    /// Value of [`DriverDef::maven`]; every built-in is downloadable.
    maven: &'static str,
    /// Value of [`DriverDef::url_template`].
    url_template: &'static str,
    /// Value of [`DriverDef::default_port`].
    default_port: Option<u16>,
    /// Value of [`DriverDef::dialect`].
    dialect: &'static str,
}

/// Built-in driver definitions, in the order the picker shows them.
///
/// The Maven coordinates pin a version that was current when this list was
/// written; they are a starting point for the downloader, not a requirement.
/// Users edit `drivers.json` to move to another version or to add a driver that
/// is not here.
const BUILTIN_DRIVERS: &[BuiltinDriver] = &[
    BuiltinDriver {
        id: "h2",
        name: "H2",
        icon: "h2",
        class: "org.h2.Driver",
        maven: "com.h2database:h2:2.3.232",
        // The server URL: an embedded H2 file is opened by editing this into
        // `jdbc:h2:file:/path/to/db`, which the connection editor allows.
        url_template: "jdbc:h2:tcp://{host}:{port}/{database}",
        default_port: Some(9092),
        dialect: "h2",
    },
    BuiltinDriver {
        id: "postgresql",
        name: "PostgreSQL",
        icon: "postgresql",
        class: "org.postgresql.Driver",
        maven: "org.postgresql:postgresql:42.7.4",
        url_template: "jdbc:postgresql://{host}:{port}/{database}",
        default_port: Some(5432),
        dialect: "postgres",
    },
    BuiltinDriver {
        id: "mysql",
        name: "MySQL",
        icon: "mysql",
        class: "com.mysql.cj.jdbc.Driver",
        maven: "com.mysql:mysql-connector-j:9.1.0",
        url_template: "jdbc:mysql://{host}:{port}/{database}",
        default_port: Some(3306),
        dialect: "mysql",
    },
    BuiltinDriver {
        id: "mariadb",
        name: "MariaDB",
        icon: "mariadb",
        class: "org.mariadb.jdbc.Driver",
        maven: "org.mariadb.jdbc:mariadb-java-client:3.5.1",
        url_template: "jdbc:mariadb://{host}:{port}/{database}",
        default_port: Some(3306),
        // MariaDB's SQL surface is MySQL's, so it shares the dialect rather
        // than duplicating one.
        dialect: "mysql",
    },
    BuiltinDriver {
        id: "sqlite",
        name: "SQLite",
        icon: "sqlite",
        class: "org.sqlite.JDBC",
        maven: "org.xerial:sqlite-jdbc:3.47.1.0",
        // SQLite is a file, not a service: no host, no port.
        url_template: "jdbc:sqlite:{file}",
        default_port: None,
        dialect: "sqlite",
    },
    BuiltinDriver {
        id: "oracle-thin",
        name: "Oracle Thin",
        icon: "oracle",
        class: "oracle.jdbc.OracleDriver",
        maven: "com.oracle.database.jdbc:ojdbc11:23.6.0.24.10",
        // The service-name form; the older SID form is `@//{host}:{port}:{sid}`.
        url_template: "jdbc:oracle:thin:@//{host}:{port}/{service}",
        default_port: Some(1521),
        dialect: "oracle",
    },
    BuiltinDriver {
        id: "mssql",
        name: "Microsoft SQL Server",
        icon: "mssql",
        class: "com.microsoft.sqlserver.jdbc.SQLServerDriver",
        maven: "com.microsoft.sqlserver:mssql-jdbc:12.8.1.jre11",
        // Semicolon-separated properties, not a path: that is this driver's URL
        // grammar, not a typo.
        url_template: "jdbc:sqlserver://{host}:{port};databaseName={database}",
        default_port: Some(1433),
        dialect: "mssql",
    },
];

impl DriverDef {
    /// The driver definitions rudbman knows without being told.
    ///
    /// Used verbatim when `drivers.json` does not exist yet. Every entry has an
    /// empty [`DriverDef::jars`]: the definition says what a driver *is*, the
    /// user still has to supply the JAR.
    pub fn builtins() -> Vec<Self> {
        BUILTIN_DRIVERS
            .iter()
            .map(|builtin| Self {
                id: builtin.id.to_string(),
                name: builtin.name.to_string(),
                icon: Some(builtin.icon.to_string()),
                class: builtin.class.to_string(),
                jars: Vec::new(),
                maven: Some(builtin.maven.to_string()),
                url_template: builtin.url_template.to_string(),
                default_port: builtin.default_port,
                dialect: builtin.dialect.to_string(),
            })
            .collect()
    }
}

/// Collection of [`DriverDef`]s, persisted as `drivers.json`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DriverStore {
    /// Definitions in picker order.
    #[serde(default)]
    drivers: Vec<DriverDef>,
}

impl Default for DriverStore {
    /// The built-in definitions, which is what a machine without a
    /// `drivers.json` starts from.
    fn default() -> Self {
        Self {
            drivers: DriverDef::builtins(),
        }
    }
}

impl DriverStore {
    /// Load the store from the default configuration file.
    ///
    /// A missing file yields [`DriverStore::default`] — the built-in
    /// definitions. An *existing* file is taken at its word, empty or not: a
    /// user who deleted a driver they never use must not find it back.
    ///
    /// # Errors
    ///
    /// Fails when the configuration directory cannot be determined, the file
    /// cannot be read, or its contents are not valid JSON.
    pub fn load() -> Result<Self> {
        Self::load_from(&drivers_file()?)
    }

    /// Load the store from an explicit path.
    ///
    /// A missing file yields the built-in definitions.
    ///
    /// # Errors
    ///
    /// Fails when the file cannot be read or does not contain valid JSON.
    pub fn load_from(path: &Path) -> Result<Self> {
        let Some(data) = read_optional(path)? else {
            return Ok(Self::default());
        };
        serde_json::from_slice(strip_bom(&data))
            .with_context(|| format!("failed to parse drivers from {}", path.display()))
    }

    /// Write the store to the default configuration file.
    ///
    /// # Errors
    ///
    /// Fails when the configuration directory cannot be determined or created,
    /// or when the file cannot be written.
    pub fn save(&self) -> Result<()> {
        self.save_to(&drivers_file()?)
    }

    /// Write the store to an explicit path, creating parent directories.
    ///
    /// The write is atomic: the data lands in a temporary sibling file that is
    /// then renamed over `path`.
    ///
    /// # Errors
    ///
    /// Fails when the parent directory cannot be created or the file cannot be
    /// written.
    pub fn save_to(&self, path: &Path) -> Result<()> {
        let json = serde_json::to_vec_pretty(self).context("failed to serialize drivers")?;
        write_atomic(path, &json)
    }

    /// All definitions, in picker order.
    pub fn drivers(&self) -> &[DriverDef] {
        &self.drivers
    }

    /// Look up a definition by id.
    pub fn get(&self, id: &str) -> Option<&DriverDef> {
        self.drivers.iter().find(|d| d.id == id)
    }

    /// Insert `driver`, replacing an existing definition with the same id.
    ///
    /// Replacement keeps the original position in the list.
    pub fn upsert(&mut self, driver: DriverDef) {
        match self.drivers.iter_mut().find(|d| d.id == driver.id) {
            Some(slot) => *slot = driver,
            None => self.drivers.push(driver),
        }
    }

    /// Remove the definition with the given id and return it.
    pub fn remove(&mut self, id: &str) -> Option<DriverDef> {
        let index = self.drivers.iter().position(|d| d.id == id)?;
        Some(self.drivers.remove(index))
    }

    /// Number of stored definitions.
    pub fn len(&self) -> usize {
        self.drivers.len()
    }

    /// Whether the store holds no definitions.
    pub fn is_empty(&self) -> bool {
        self.drivers.is_empty()
    }
}

/// Read a file, mapping "not found" to [`None`] rather than an error.
///
/// Both stores treat a missing file as a first run, and both want every other
/// I/O failure reported: a config directory that cannot be read is worth saying
/// out loud instead of quietly starting from scratch and overwriting it later.
fn read_optional(path: &Path) -> Result<Option<Vec<u8>>> {
    match fs::read(path) {
        Ok(data) => Ok(Some(data)),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(err) => Err(err).with_context(|| format!("failed to read {}", path.display())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(name: &str) -> ConnectionProfile {
        ConnectionProfile::new(
            name,
            "postgresql",
            "jdbc:postgresql://db.example.com:5432/app",
            "alice",
        )
    }

    fn sample_tunnel() -> TunnelConfig {
        TunnelConfig {
            host: "bastion.example.com".to_string(),
            port: 22,
            username: "ops".to_string(),
            auth: TunnelAuth::Key {
                path: PathBuf::from("/home/ops/.ssh/id_ed25519"),
            },
            remote_host: "db.internal".to_string(),
            remote_port: 5432,
            local_port: 0,
        }
    }

    #[test]
    fn new_assigns_unique_ids_and_safe_defaults() {
        let a = sample("a");
        let b = sample("b");
        assert_ne!(a.id, b.id);
        assert!(!a.read_only);
        assert!(a.auto_commit);
        assert!(a.confirm_writes);
        assert_eq!(a.keep_alive, None);
        assert_eq!(a.tunnel, None);
        assert!(a.props.is_empty());
    }

    #[test]
    fn label_falls_back_to_the_bare_url() {
        let mut profile = sample("prod");
        assert_eq!(
            profile.label(),
            "alice@jdbc:postgresql://db.example.com:5432/app"
        );
        profile.username.clear();
        assert_eq!(profile.label(), "jdbc:postgresql://db.example.com:5432/app");
    }

    #[test]
    fn debug_masks_property_values_but_keeps_keys() {
        let mut profile = sample("prod");
        profile
            .props
            .insert("password".to_string(), "hunter2".to_string());
        profile
            .props
            .insert("ssl".to_string(), "require".to_string());

        let rendered = format!("{profile:?}");
        assert!(rendered.contains("\"password\""), "got {rendered}");
        assert!(rendered.contains("\"ssl\""), "got {rendered}");
        assert!(rendered.contains("<redacted>"), "got {rendered}");
        assert!(!rendered.contains("hunter2"), "secret leaked: {rendered}");
        // A non-secret value is masked too: core cannot tell which is which.
        assert!(!rendered.contains("require"), "value leaked: {rendered}");
    }

    #[test]
    fn debug_masks_credentials_in_every_url_shape_a_driver_accepts() {
        // Left: what a user may have typed. Right: what a log may contain.
        let cases = [
            (
                "jdbc:sqlserver://host:1433;databaseName=db;user=x;password=hunter2",
                "jdbc:sqlserver://host:1433;databaseName=<redacted>;user=<redacted>;\
                 password=<redacted>",
            ),
            (
                "jdbc:postgresql://host:5432/db?user=x&password=hunter2",
                "jdbc:postgresql://host:5432/db?user=<redacted>&password=<redacted>",
            ),
            (
                "jdbc:mysql://host/db?password=hunter2&useSSL=true",
                "jdbc:mysql://host/db?password=<redacted>&useSSL=<redacted>",
            ),
            (
                "jdbc:postgresql://user:hunter2@host/db",
                "jdbc:postgresql://user:<redacted>@host/db",
            ),
            (
                "jdbc:oracle:thin:user/hunter2@//host:1521/service",
                "jdbc:oracle:thin:<redacted>@//host:1521/service",
            ),
            (
                "jdbc:h2:./data;PASSWORD=hunter2",
                "jdbc:h2:./data;PASSWORD=<redacted>",
            ),
            // No password at all, but an `@` still hides the user name: the
            // error goes towards saying too little.
            ("jdbc:mysql://user@host/db", "jdbc:mysql:<redacted>@host/db"),
        ];

        for (url, expected) in cases {
            let profile = ConnectionProfile::new("prod", "generic", url, "alice");
            let rendered = format!("{profile:?}");
            assert!(
                rendered.contains(expected),
                "masking {url}\n  expected: {expected}\n  rendered: {rendered}"
            );
            // The point of the whole exercise.
            assert!(
                !rendered.contains("hunter2"),
                "password leaked from {url}: {rendered}"
            );
        }
    }

    #[test]
    fn debug_shows_a_url_that_cannot_hold_a_credential_in_full() {
        // Guards against over-masking: a log line has to say which connection
        // it is about, and this is the shape most URLs have.
        for url in [
            "jdbc:postgresql://db.example.com:5432/app",
            "jdbc:oracle:thin:@//db.example.com:1521/ORCLPDB",
            "jdbc:sqlite:/var/lib/rudbman/local.db",
        ] {
            let profile = ConnectionProfile::new("prod", "generic", url, "alice");
            let rendered = format!("{profile:?}");
            assert!(
                rendered.contains(url),
                "url masked for no reason: {rendered}"
            );
        }
    }

    #[test]
    fn debug_of_a_tunnel_shows_the_key_path_but_no_secret() {
        let mut profile = sample("prod");
        profile.tunnel = Some(sample_tunnel());
        let rendered = format!("{profile:?}");
        assert!(rendered.contains("id_ed25519"), "got {rendered}");
        assert!(rendered.contains("bastion.example.com"), "got {rendered}");

        // The password variant carries nothing at all to leak.
        let auth = format!("{:?}", TunnelAuth::Password);
        assert_eq!(auth, "Password");
    }

    #[test]
    fn tunnel_auth_serializes_as_an_internally_tagged_enum() {
        assert_eq!(
            serde_json::to_value(TunnelAuth::Agent).unwrap(),
            serde_json::json!({ "kind": "agent" })
        );
        let value = serde_json::to_value(TunnelAuth::Key {
            path: PathBuf::from("key"),
        })
        .unwrap();
        assert_eq!(value["kind"], "key");
        assert_eq!(value["path"], "key");
    }

    #[test]
    fn tunnel_auth_round_trips() {
        let cases = [
            TunnelAuth::Agent,
            TunnelAuth::Password,
            TunnelAuth::Key {
                path: PathBuf::from("/home/alice/.ssh/id_rsa"),
            },
        ];
        for auth in cases {
            let json = serde_json::to_string(&auth).expect("serialize");
            let back: TunnelAuth = serde_json::from_str(&json).expect("deserialize");
            assert_eq!(auth, back, "round trip failed for {json}");
        }
    }

    #[test]
    fn tunnel_auth_accepts_the_documented_bare_strings() {
        for (text, expected) in [
            ("\"agent\"", TunnelAuth::Agent),
            ("\"password\"", TunnelAuth::Password),
            (
                "\"KEY\"",
                TunnelAuth::Key {
                    path: PathBuf::new(),
                },
            ),
        ] {
            let auth: TunnelAuth = serde_json::from_str(text).expect(text);
            assert_eq!(auth, expected, "parsing {text}");
        }
    }

    #[test]
    fn an_unknown_tunnel_auth_method_degrades_to_the_agent() {
        let auth: TunnelAuth = serde_json::from_str("\"biometrics\"").expect("parse");
        assert_eq!(auth, TunnelAuth::Agent);
    }

    #[test]
    fn a_sibling_key_path_is_folded_into_the_key_variant() {
        // Exactly the shape the architecture document, §9.2, writes.
        let json = r#"{
            "enabled": true,
            "host": "bastion.example.com", "port": 22,
            "username": "ops",
            "auth": "key",
            "key_path": "/home/ops/.ssh/id_ed25519",
            "remote_host": "db.internal", "remote_port": 5432,
            "local_port": 0
        }"#;
        let tunnel: TunnelConfig = serde_json::from_str(json).expect("parse");
        assert_eq!(
            tunnel.auth,
            TunnelAuth::Key {
                path: PathBuf::from("/home/ops/.ssh/id_ed25519")
            }
        );
        assert_eq!(tunnel.remote_port, 5432);
        assert_eq!(tunnel.local_port, 0);
    }

    #[test]
    fn a_tunnel_with_missing_fields_still_loads() {
        let tunnel: TunnelConfig =
            serde_json::from_str(r#"{"host":"bastion","remote_host":"db"}"#).expect("parse");
        assert_eq!(tunnel.port, 22, "the SSH port must default");
        assert_eq!(tunnel.auth, TunnelAuth::Agent);
        assert_eq!(tunnel.local_port, 0, "0 lets the OS pick the port");
        assert!(tunnel.username.is_empty());
    }

    #[test]
    fn a_disabled_block_loads_as_none() {
        let json = r#"{
            "connections": [{
                "name": "prod",
                "keep_alive": { "enabled": false, "interval_s": 300 },
                "tunnel": { "enabled": false, "host": "bastion" }
            }]
        }"#;
        let store: ConnectionStore = serde_json::from_str(json).expect("parse");
        let profile = &store.connections()[0];
        assert_eq!(profile.keep_alive, None);
        assert_eq!(profile.tunnel, None);
    }

    #[test]
    fn an_enabled_block_loads_with_its_values() {
        let json = r#"{
            "connections": [{
                "name": "prod",
                "keep_alive": { "enabled": true, "interval_s": 60, "query": "select 1 from dual" },
                "tunnel": { "enabled": true, "host": "bastion", "remote_host": "db",
                            "remote_port": 1521 }
            }]
        }"#;
        let store: ConnectionStore = serde_json::from_str(json).expect("parse");
        let profile = &store.connections()[0];
        assert_eq!(
            profile.keep_alive,
            Some(KeepAlive {
                interval_s: 60,
                query: "select 1 from dual".to_string(),
            })
        );
        assert_eq!(profile.tunnel.as_ref().map(|t| t.remote_port), Some(1521));
    }

    #[test]
    fn a_keep_alive_block_without_an_interval_defaults_it() {
        let json = r#"{"connections":[{"name":"p","keep_alive":{}}]}"#;
        let store: ConnectionStore = serde_json::from_str(json).expect("parse");
        assert_eq!(
            store.connections()[0].keep_alive,
            Some(KeepAlive::default()),
            "an empty block must not lose the documented defaults"
        );
        assert_eq!(KeepAlive::default().interval_s, 300);
    }

    #[test]
    fn a_hand_written_profile_with_almost_nothing_in_it_loads() {
        // Every field but the name is missing; nothing may fail the load.
        let json = r#"{"connections":[{"name":"scratch"}]}"#;
        let store: ConnectionStore = serde_json::from_str(json).expect("parse");
        let profile = &store.connections()[0];
        assert_eq!(profile.name, "scratch");
        assert!(profile.auto_commit);
        assert!(profile.confirm_writes);
        assert!(!profile.read_only);
        assert!(profile.driver_id.is_empty());
        assert_ne!(profile.id, Uuid::nil(), "a missing id gets a fresh one");
    }

    #[test]
    fn unknown_profile_fields_are_ignored() {
        // A file written by a future version must not break an older build.
        let json = r#"{"connections":[{"name":"p","future":{"nested":[1,2]}}],"future":true}"#;
        let store: ConnectionStore = serde_json::from_str(json).expect("parse");
        assert_eq!(store.len(), 1);
    }

    #[test]
    fn save_to_load_from_round_trip() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("cfg").join("connections.json");

        let mut first = sample("first");
        first.folder = Some("production".to_string());
        first.color = Some("#e06c75".to_string());
        first
            .props
            .insert("ApplicationName".to_string(), "rudbman".to_string());
        first.keep_alive = Some(KeepAlive {
            interval_s: 120,
            query: "select 1".to_string(),
        });
        first.tunnel = Some(sample_tunnel());
        first.read_only = true;

        let second =
            ConnectionProfile::new("second", "oracle-thin", "jdbc:oracle:thin:@//h/S", "s");

        let mut store = ConnectionStore::default();
        store.upsert(first.clone());
        store.upsert(second.clone());
        store.save_to(&path).expect("save");

        let loaded = ConnectionStore::load_from(&path).expect("load");
        assert_eq!(loaded.len(), 2);
        assert_eq!(loaded.connections(), &[first, second]);

        // Saving over an existing file must work too.
        loaded.save_to(&path).expect("overwrite");
        assert_eq!(ConnectionStore::load_from(&path).expect("reload").len(), 2);
    }

    #[test]
    fn a_saved_profile_never_contains_a_password_key_of_its_own() {
        // The keychain owns passwords; the file must have nowhere to put one.
        let json = serde_json::to_string(&sample("p")).expect("serialize");
        let value: serde_json::Value = serde_json::from_str(&json).expect("parse");
        assert!(value.get("password").is_none(), "got {json}");
        assert!(value.get("secret").is_none(), "got {json}");
    }

    #[test]
    fn upsert_replaces_same_id_in_place() {
        let mut store = ConnectionStore::default();
        let keep = sample("keep");
        let mut original = sample("original");
        store.upsert(keep.clone());
        store.upsert(original.clone());

        original.name = "renamed".to_string();
        store.upsert(original.clone());

        assert_eq!(store.len(), 2);
        assert_eq!(store.connections()[0].id, keep.id);
        assert_eq!(
            store.get(original.id).map(|p| p.name.as_str()),
            Some("renamed")
        );
    }

    #[test]
    fn remove_returns_the_profile() {
        let mut store = ConnectionStore::default();
        let profile = sample("victim");
        store.upsert(profile.clone());

        assert!(!store.is_empty());
        assert_eq!(store.remove(profile.id), Some(profile.clone()));
        assert!(store.is_empty());
        assert_eq!(store.remove(profile.id), None);
        assert_eq!(store.get(profile.id), None);
    }

    #[test]
    fn load_from_missing_file_is_empty() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = ConnectionStore::load_from(&dir.path().join("nope.json")).expect("load");
        assert!(store.is_empty());
    }

    #[test]
    fn load_from_tolerates_a_utf8_bom() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("connections.json");

        let mut store = ConnectionStore::default();
        store.upsert(sample("bom"));
        store.save_to(&path).expect("save");

        // Rewrite the file the way a Windows editor would.
        let saved = fs::read(&path).expect("read");
        let mut with_bom = b"\xEF\xBB\xBF".to_vec();
        with_bom.extend_from_slice(&saved);
        fs::write(&path, with_bom).expect("write");

        let loaded = ConnectionStore::load_from(&path).expect("load");
        assert_eq!(loaded.connections()[0].name, "bom");
    }

    #[test]
    fn load_from_invalid_json_fails_without_panicking() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("connections.json");
        fs::write(&path, b"{ not json at all").expect("write");

        let err = ConnectionStore::load_from(&path).expect_err("must be an error");
        assert!(
            err.to_string().contains("failed to parse connections"),
            "unhelpful error: {err:#}"
        );
        // The corrupt file is left alone for the user to fix.
        assert!(path.exists());
    }

    #[test]
    fn drivers_default_to_the_builtins() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = DriverStore::load_from(&dir.path().join("absent.json")).expect("load");
        assert_eq!(store.drivers(), DriverDef::builtins());
        assert!(store.get("postgresql").is_some());
    }

    #[test]
    fn an_existing_driver_file_is_taken_at_its_word() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("drivers.json");
        fs::write(&path, br#"{"drivers":[]}"#).expect("write");

        let store = DriverStore::load_from(&path).expect("load");
        assert!(
            store.is_empty(),
            "a user who removed every driver must not get them back"
        );
    }

    #[test]
    fn builtin_drivers_are_complete_and_unique() {
        let builtins = DriverDef::builtins();
        assert_eq!(builtins.len(), 7);

        let mut ids: Vec<&str> = builtins.iter().map(|d| d.id.as_str()).collect();
        ids.sort_unstable();
        let unique = ids.len();
        ids.dedup();
        assert_eq!(ids.len(), unique, "duplicate driver id");

        for driver in &builtins {
            assert!(!driver.name.is_empty(), "{} has no name", driver.id);
            assert!(driver.class.contains('.'), "{} has no class", driver.id);
            assert!(
                driver.url_template.starts_with("jdbc:"),
                "{} has a suspicious URL template: {}",
                driver.id,
                driver.url_template
            );
            assert!(
                driver.jars.is_empty(),
                "{} ships JARs it has no right to",
                driver.id
            );
            assert!(driver.icon.is_some(), "{} has no icon", driver.id);
            assert!(!driver.dialect.is_empty(), "{} has no dialect", driver.id);
            let maven = driver.maven.as_deref().expect("maven coordinate");
            assert_eq!(
                maven.split(':').count(),
                3,
                "{} has a malformed coordinate {maven}",
                driver.id
            );
        }
    }

    #[test]
    fn builtin_ports_match_the_well_known_ones() {
        let builtins = DriverDef::builtins();
        let port = |id: &str| {
            builtins
                .iter()
                .find(|d| d.id == id)
                .and_then(|d| d.default_port)
        };
        assert_eq!(port("postgresql"), Some(5432));
        assert_eq!(port("mysql"), Some(3306));
        assert_eq!(port("mariadb"), Some(3306));
        assert_eq!(port("oracle-thin"), Some(1521));
        assert_eq!(port("mssql"), Some(1433));
        assert_eq!(port("h2"), Some(9092));
        // SQLite is a file, not a service.
        assert_eq!(port("sqlite"), None);
    }

    #[test]
    fn driver_store_round_trips_and_edits() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("cfg").join("drivers.json");

        let mut store = DriverStore::default();
        let before = store.len();
        store.upsert(DriverDef {
            id: "cubrid".to_string(),
            name: "CUBRID".to_string(),
            icon: Some("cubrid".to_string()),
            class: "cubrid.jdbc.driver.CUBRIDDriver".to_string(),
            url_template: "jdbc:cubrid:{host}:{port}:{database}:::".to_string(),
            default_port: Some(33000),
            dialect: "generic".to_string(),
            ..DriverDef::default()
        });
        assert_eq!(store.len(), before + 1);

        store.save_to(&path).expect("save");
        let loaded = DriverStore::load_from(&path).expect("load");
        assert_eq!(loaded.drivers(), store.drivers());
        assert_eq!(
            loaded.get("cubrid").map(|d| d.name.as_str()),
            Some("CUBRID")
        );

        let mut loaded = loaded;
        assert!(loaded.remove("cubrid").is_some());
        assert!(loaded.remove("cubrid").is_none());
        assert_eq!(loaded.len(), before);
    }

    #[test]
    fn a_hand_written_driver_with_missing_fields_loads() {
        let json = r#"{"drivers":[{"id":"custom","class":"com.example.Driver"}]}"#;
        let store: DriverStore = serde_json::from_str(json).expect("parse");
        let driver = store.get("custom").expect("custom driver");
        assert_eq!(driver.dialect, "generic", "an omitted dialect is generic");
        assert_eq!(driver.default_port, None);
        assert!(driver.jars.is_empty());
        assert!(driver.maven.is_none());
    }

    #[test]
    fn driver_load_from_invalid_json_fails() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("drivers.json");
        fs::write(&path, br#"{"drivers": "not a list"}"#).expect("write");

        let err = DriverStore::load_from(&path).expect_err("must be an error");
        assert!(
            err.to_string().contains("failed to parse drivers"),
            "unhelpful error: {err:#}"
        );
    }
}
