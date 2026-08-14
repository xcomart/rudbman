//! Opening and closing a database session, in the order the architecture
//! document §9.3 prescribes.
//!
//! ```text
//! connect     tunnel up → read the bound port → substitute {host}:{port} → OPEN_SESSION
//! disconnect  CLOSE_SESSION → tunnel down
//! ```
//!
//! **The tunnel stands up first and lies down last.** A JDBC session whose
//! tunnel closed underneath it sees nothing but an unexplained socket error, so
//! the ordering is not a preference — it is the difference between a diagnosable
//! failure and a mystery.
//!
//! # Everything in here blocks
//!
//! [`Session`] is synchronous by design (`rudbman-jdbc` is deliberately free of
//! gpui), so [`connect`] is called from `cx.background_spawn` and never from a
//! window callback. What comes back is [`Connected`], which owns the session and
//! the tunnel lease that has to outlive it.
//!
//! # The tunnel is shared, the session is not
//!
//! Two profiles that reach the same database through the same bastion get one
//! tunnel between them, reference counted here (§9.3 — `rudbman-ssh` says in as
//! many words that the counting belongs to the application). The last lease
//! dropped is what closes it.
//!
//! A tunnel that breaks is **never** repaired silently. Every lease carries a
//! [`TunnelLease::watch`] channel that fires once with the reason, and the tab
//! above it goes to a dead state; a reconnect would hide a transaction that was
//! open when the socket went away.
//!
//! # Secrets
//!
//! The password is read from the keychain in [`Credentials::read`], immediately
//! before it is handed to [`ConnectionSpec`], and is stored nowhere else. Every
//! type that touches one — [`ConnectionSpec`], [`TunnelSpec`], [`SshAuth`] —
//! masks it in `Debug`, and nothing in this module renders one by hand.

use std::collections::HashMap;
use std::fmt;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use futures::StreamExt;
use futures::channel::oneshot;
use futures::executor::block_on;
use parking_lot::Mutex;
use rudbman_core::{
    AppSettings, ConnectionProfile, DriverDef, KnownHosts, SecretSlot, SecretStore, TunnelConfig,
};
use rudbman_jdbc::{
    BridgeError, BridgeErrorKind, ConnectionSpec, Error as JdbcError, Jvm, JvmConfig,
    KeepAliveSpec, Session, SessionInfo, default_bridge_jar,
};
use rudbman_ssh::{HostKeyVerifier, PublicKey, SshTunnel, TunnelEvent, TunnelSpec};

/// Placeholders that name the catalogue rather than the server.
///
/// `{database}`, `{service}`, `{sid}` and `{file}` all stand for the same thing
/// on screen — what a product calls the thing you connect *to* — but each driver
/// family spells it its own way, so one field on screen fills whichever of these
/// the template happens to use.
pub const CATALOGUE_PLACEHOLDERS: [&str; 4] = ["database", "service", "sid", "file"];

/// Fills `{placeholder}` holes in a driver's URL template.
///
/// An unknown placeholder is left as it stands rather than blanked: a template
/// from a hand-written `drivers.json` may well use a hole the editor has no
/// field for, and leaving `{warehouse}` visible in the preview says so, whereas
/// silently dropping it produces a URL that fails at the driver with no clue
/// where the gap came from.
pub fn substitute(template: &str, values: &HashMap<String, String>) -> String {
    let mut out = String::with_capacity(template.len());
    let mut rest = template;
    while let Some(open) = rest.find('{') {
        out.push_str(&rest[..open]);
        let Some(close) = rest[open..].find('}').map(|index| open + index) else {
            // An unbalanced brace is part of the URL, not a hole.
            out.push_str(&rest[open..]);
            return out;
        };
        let name = &rest[open + 1..close];
        match values.get(name) {
            Some(value) => out.push_str(value),
            None => out.push_str(&rest[open..=close]),
        }
        rest = &rest[close + 1..];
    }
    out.push_str(rest);
    out
}

/// The placeholders a template actually uses, in the order they appear.
pub fn placeholders_of(template: &str) -> Vec<String> {
    let mut found = Vec::new();
    let mut rest = template;
    while let Some(open) = rest.find('{') {
        let Some(close) = rest[open..].find('}').map(|index| open + index) else {
            break;
        };
        let name = rest[open + 1..close].to_string();
        if !name.is_empty() && !found.contains(&name) {
            found.push(name);
        }
        rest = &rest[close + 1..];
    }
    found
}

/// Rewrites the host and port of an already-assembled JDBC URL.
///
/// What the tunnel needs: the profile's URL names the database as it is called
/// *inside* the remote network, and the driver has to be pointed at the local
/// port instead. Only the authority — the `host:port` between `//` and the next
/// `/`, `;`, `?` or end — is touched, so a URL's grammar survives whatever shape
/// it has. A URL with no `//` (SQLite's `jdbc:sqlite:/path`, H2's
/// `jdbc:h2:mem:x`) is left alone: there is no server in it to redirect.
pub fn redirect_authority(url: &str, host: &str, port: u16) -> String {
    let Some(slashes) = url.find("//") else {
        return url.to_string();
    };
    let start = slashes + 2;
    let end = url[start..]
        .find(['/', ';', '?'])
        .map_or(url.len(), |index| start + index);
    format!("{}{host}:{port}{}", &url[..start], &url[end..])
}

/// A profile's secrets, read from the keychain and held no longer than the
/// connection attempt they belong to.
///
/// Deliberately not `Clone` and deliberately without a `Debug`: the only way to
/// get at these is to move them into a [`ConnectionSpec`] or a [`TunnelSpec`],
/// both of which mask what they hold.
pub struct Credentials {
    /// The database password.
    pub password: Option<String>,
    /// The tunnel's password or key passphrase.
    pub tunnel: Option<String>,
}

impl Credentials {
    /// Reads both slots for `profile`.
    ///
    /// A keychain that is not there is not an error: [`SecretStore::get`]
    /// answers `None`, the driver is handed an empty password, and the database
    /// gets to be the one that says no.
    pub fn read(profile: &ConnectionProfile) -> Self {
        let read = |slot: SecretSlot| {
            SecretStore::get(profile.id, slot).unwrap_or_else(|error| {
                log::warn!("could not read the {slot} secret: {error:#}");
                None
            })
        };
        Credentials {
            password: read(SecretSlot::Connection),
            tunnel: profile.tunnel.as_ref().and(read(SecretSlot::Tunnel)),
        }
    }

    /// Secrets typed into a form that has not been saved, for the test button.
    pub fn typed(password: Option<String>, tunnel: Option<String>) -> Self {
        Credentials { password, tunnel }
    }
}

/// Anything that can stop a connection from opening, in the terms the user
/// needs to hear it in.
///
/// The variants are split by *where the fix is*, not by which layer raised the
/// error: a bastion that will not authenticate and a database that will not
/// authenticate are different passwords in different places.
#[derive(Debug)]
pub enum ConnectError {
    /// The driver has no JAR, so there is nothing to load the class from.
    NoDriverJar(String),
    /// The JVM would not start — no Java runtime, or no bridge JAR.
    JvmStart(String),
    /// The tunnel could not be established.
    Tunnel(String),
    /// The bastion presented a host key that is not the one on record.
    HostKeyMismatch {
        /// The bastion, as the profile names it.
        host: String,
        /// The fingerprint that arrived.
        presented: String,
        /// The fingerprint `known_hosts` holds.
        stored: String,
    },
    /// The bastion presented a host key nothing has ever trusted.
    HostKeyUnknown {
        /// The bastion, as the profile names it.
        host: String,
        /// The fingerprint that arrived.
        fingerprint: String,
        /// The key's algorithm.
        algorithm: String,
    },
    /// The driver or the database refused. Carries the bridge's error envelope,
    /// which is what [`ConnectError::hint`] reads the `SQLSTATE` class off.
    Database(BridgeError),
    /// Anything else that came back from the JNI layer: a protocol mismatch, a
    /// worker that died, a JAR that is not a JAR.
    Bridge(String),
}

impl ConnectError {
    /// The `SQLSTATE` class, when the failure came from the database.
    ///
    /// Branch on this and never on the whole code: a missing table is `42S04`
    /// on H2 and `42S02` almost everywhere else, and only the leading two
    /// characters are portable.
    pub fn sql_state_class(&self) -> Option<&str> {
        match self {
            ConnectError::Database(error) => error.sql_state_class(),
            _ => None,
        }
    }

    /// Whether the database said the credentials were wrong.
    ///
    /// `SQLSTATE` class `28` is "invalid authorization specification", which is
    /// the one class every driver family agrees on for this.
    pub fn is_authentication(&self) -> bool {
        self.sql_state_class() == Some("28")
    }

    /// Whether the failure was the driver rather than the database — a class
    /// that is not in the JAR, a URL the driver does not recognise.
    pub fn is_driver(&self) -> bool {
        matches!(
            self,
            ConnectError::NoDriverJar(_)
                | ConnectError::Database(BridgeError {
                    kind: BridgeErrorKind::Driver,
                    ..
                })
        )
    }

    /// The message the dialog shows.
    ///
    /// Never a stack trace and never a masked-off secret: [`BridgeError`]'s own
    /// `Display` renders the kind, the `SQLSTATE` and the message, and its
    /// `Debug` leaves the Java frames out.
    pub fn message(&self) -> String {
        match self {
            ConnectError::NoDriverJar(name) => {
                format!("the driver “{name}” has no JAR; add one in the driver manager")
            }
            ConnectError::JvmStart(message) => message.clone(),
            ConnectError::Tunnel(message) => message.clone(),
            ConnectError::HostKeyMismatch {
                host,
                presented,
                stored,
            } => format!(
                "the host key of {host} has changed: {presented} was presented, {stored} is on record"
            ),
            ConnectError::HostKeyUnknown {
                host,
                fingerprint,
                algorithm,
            } => format!("{host} presented an unknown {algorithm} host key: {fingerprint}"),
            ConnectError::Database(error) => {
                let mut message = error.to_string();
                // Drivers routinely bury the real reason in `getNextException`,
                // which the bridge flattens into the cause chain.
                if let Some(cause) = error.causes.first() {
                    message.push_str(" — ");
                    message.push_str(cause);
                }
                message
            }
            ConnectError::Bridge(message) => message.clone(),
        }
    }
}

impl fmt::Display for ConnectError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message())
    }
}

impl From<JdbcError> for ConnectError {
    fn from(error: JdbcError) -> Self {
        match error {
            JdbcError::JvmStart(message) => ConnectError::JvmStart(message),
            JdbcError::Bridge(bridge) => ConnectError::Database(bridge),
            other => ConnectError::Bridge(other.to_string()),
        }
    }
}

/// A session and the tunnel it runs over, kept together so that neither can
/// outlive the other in the wrong order.
///
/// **The field order is the ordering guarantee.** Rust drops a struct's fields
/// in declaration order, so whoever lets go of the last [`SessionHandle`]
/// closes the session first and releases the tunnel second — §9.3 — whether
/// that is the tab being closed or a metadata query that outlived it.
struct Live {
    /// The JDBC session. Declared first, so it is closed first.
    session: Session,
    /// The tunnel underneath, if any. Declared last, so it lies down last.
    lease: Option<TunnelLease>,
}

/// A shared, `Send` claim on an open session.
///
/// The explorer loads a schema on a background task while the workspace owns
/// the tab, so the session has to be reachable from both; a handle is what they
/// share. Holding one keeps the session — and the tunnel under it — alive, which
/// is what stops a fetch in flight from finding its connection closed out from
/// under it.
#[derive(Clone)]
pub struct SessionHandle(Arc<Live>);

impl SessionHandle {
    /// The session, for the blocking calls that make up a fetch.
    pub fn session(&self) -> &Session {
        &self.0.session
    }
}

impl fmt::Debug for SessionHandle {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("SessionHandle")
            .field(&self.0.session)
            .finish()
    }
}

/// An open session and everything that has to outlive it.
pub struct Connected {
    /// The session and its tunnel; see [`Live`].
    live: Arc<Live>,
    /// What `SESSION_INFO` said: the product, the version, the capability flags.
    pub info: SessionInfo,
}

impl fmt::Debug for Connected {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Connected")
            .field("session", &self.live.session)
            .field("product", &self.info.product_name)
            .field("tunnelled", &self.live.lease.is_some())
            .finish_non_exhaustive()
    }
}

impl Connected {
    /// The session, for a call on the calling thread.
    pub fn session(&self) -> &Session {
        &self.live.session
    }

    /// A handle a background task can carry.
    pub fn handle(&self) -> SessionHandle {
        SessionHandle(Arc::clone(&self.live))
    }

    /// The tunnel this session runs over, if any.
    pub fn lease(&self) -> Option<&TunnelLease> {
        self.live.lease.as_ref()
    }

    /// A one-line description of the server for the status bar.
    ///
    /// The product name and its version, which is what a user checks first and
    /// the only thing `SESSION_INFO` guarantees is worth showing. A driver that
    /// answered neither leaves this `None` rather than showing an empty cell.
    pub fn product(&self) -> Option<String> {
        let name = self.info.product_name.as_deref()?.trim();
        if name.is_empty() {
            return None;
        }
        match self.info.product_version.as_deref().map(str::trim) {
            Some(version) if !version.is_empty() => Some(format!("{name} {version}")),
            _ => Some(name.to_string()),
        }
    }

    /// Closes the session and then the tunnel, reporting the failure.
    ///
    /// A [`SessionHandle`] still out — a metadata fetch that has not come back
    /// yet — takes the close with it: this returns without waiting, and the
    /// last handle to be dropped runs the same two steps in the same order
    /// through [`Live`]'s own drop. Waiting here instead would block the UI
    /// thread's task on a query nobody is going to read.
    pub fn close(self) -> Result<(), JdbcError> {
        match Arc::try_unwrap(self.live) {
            Ok(Live { session, lease }) => {
                let result = session.close();
                drop(lease);
                result
            }
            Err(shared) => {
                log::debug!(
                    "session {} is still in use by a background task; it closes with that",
                    shared.session.handle()
                );
                drop(shared);
                Ok(())
            }
        }
    }
}

/// Builds the connection specification a profile asks for, without connecting.
///
/// Split out from [`connect`] so that the URL a form is about to use can be
/// shown before anything is opened, and so that the substitution can be tested
/// without a JVM.
pub fn build_spec(
    profile: &ConnectionProfile,
    driver: &DriverDef,
    credentials: &Credentials,
    url: &str,
) -> ConnectionSpec {
    let mut spec = ConnectionSpec::new(url, &driver.class);
    spec.jars = driver.jars.clone();
    spec.username = (!profile.username.is_empty()).then(|| profile.username.clone());
    spec.password = credentials.password.clone();
    spec.props = profile.props.clone();
    spec.read_only = profile.read_only;
    spec.auto_commit = profile.auto_commit;
    spec.keep_alive = profile.keep_alive.as_ref().map(|keep| KeepAliveSpec {
        enabled: true,
        interval_s: keep.interval_s,
        query: keep.query.clone(),
    });
    spec
}

/// How long [`connect`] waits before its one-shot retry on "no route to
/// host".
///
/// Long enough for macOS to finish re-evaluating the local-network
/// permission it re-checks after every boot; short enough that a database
/// that is genuinely unreachable does not keep the connect dialog hanging.
const NO_ROUTE_RETRY_DELAY: std::time::Duration = std::time::Duration::from_millis(1500);

/// Opens a session for `profile`, tunnel and all, retrying once if the first
/// attempt is rejected with "no route to host".
///
/// **Blocks.** Call it from `cx.background_spawn`.
///
/// Fresh after a reboot, macOS has to re-decide whether this app may touch
/// the local network, and the socket connect a tunnel or a direct JDBC URL
/// opens while that decision is still pending comes back `EHOSTUNREACH` —
/// "No route to host" — even though the exact same address is reachable a
/// moment later. [`is_no_route`] recognises that failure regardless of which
/// layer raised it, and this wrapper waits [`NO_ROUTE_RETRY_DELAY`] and asks
/// [`connect_once`] to try again, exactly once — a host that is actually
/// unreachable fails the retry the same way and is reported as such.
pub fn connect(
    profile: &ConnectionProfile,
    driver: &DriverDef,
    credentials: &Credentials,
    settings: &AppSettings,
) -> Result<Connected, ConnectError> {
    match connect_once(profile, driver, credentials, settings) {
        Err(error) if is_no_route(&error) => {
            log::warn!(
                "connect: \"no route to host\" on the first attempt (likely macOS's \
                 local-network permission still settling after boot); retrying once \
                 after {NO_ROUTE_RETRY_DELAY:?}: {error}"
            );
            std::thread::sleep(NO_ROUTE_RETRY_DELAY);
            connect_once(profile, driver, credentials, settings)
        }
        result => result,
    }
}

/// Whether `error` is the "no route to host" (`EHOSTUNREACH`) failure that
/// [`connect`] retries once.
///
/// Judged on [`ConnectError::message`] rather than the variant, because the
/// same OS-level failure surfaces through two different variants depending
/// on the path taken: an SSH tunnel reports it as [`ConnectError::Tunnel`]
/// with the transport's "os error 65" text, while a direct JDBC connection
/// reports it as [`ConnectError::Database`] with the driver's
/// `NoRouteToHostException` message. [`ConnectError::NoDriverJar`] and
/// [`ConnectError::JvmStart`] fail before any socket is touched, so they are
/// rejected up front rather than pattern-matched against network wording.
fn is_no_route(error: &ConnectError) -> bool {
    match error {
        ConnectError::NoDriverJar(_) | ConnectError::JvmStart(_) => false,
        _ => error.message().to_lowercase().contains("no route to host"),
    }
}

/// Opens a session for `profile`, tunnel and all — the work [`connect`]
/// wraps with its one-shot "no route to host" retry.
///
/// **Blocks.** Called only from [`connect`], which is itself only ever
/// called from `cx.background_spawn`.
///
/// The order is architecture document §9.3 and is not negotiable: the tunnel
/// first, then the port it actually bound, then the URL, then `OPEN_SESSION`.
/// A failure anywhere after the tunnel came up releases the lease on the way
/// out — an aborted connection that leaves a bastion channel open is a leak the
/// user has no way to see, let alone close. That clean unwind is also what
/// makes the retry in [`connect`] safe: a failed attempt leaves nothing
/// behind for the next one to trip over.
///
/// The JVM is started here rather than at application start-up: a user who
/// never opens a connection never pays for a Java runtime, and a machine with
/// no runtime at all only finds out in a dialog that is asking to connect.
fn connect_once(
    profile: &ConnectionProfile,
    driver: &DriverDef,
    credentials: &Credentials,
    settings: &AppSettings,
) -> Result<Connected, ConnectError> {
    if driver.jars.is_empty() {
        return Err(ConnectError::NoDriverJar(driver.name.clone()));
    }

    // 1. The tunnel, and 2. the port it bound. Held in a local so that every
    //    `?` below releases it.
    let lease = match profile.tunnel.as_ref() {
        Some(config) => Some(open_tunnel(config, credentials.tunnel.clone())?),
        None => None,
    };

    // 3. The URL, pointed at the local end of the tunnel when there is one.
    let url = match &lease {
        Some(lease) => redirect_authority(&profile.url, "127.0.0.1", lease.local_port()),
        None => profile.url.clone(),
    };

    // 4. The JVM, then OPEN_SESSION.
    let jvm = start_jvm(settings)?;
    let spec = build_spec(profile, driver, credentials, &url);
    let session = Session::open(jvm, &spec)?;
    let info = session.info().map_err(ConnectError::from)?;

    Ok(Connected {
        live: Arc::new(Live { session, lease }),
        info,
    })
}

/// Starts the process-wide JVM, or hands back the one already running.
///
/// The heap and the extra arguments only take effect on the first call of a
/// process — JNI permits one VM and offers no way to reconfigure it — which is
/// what the settings dialog's "takes effect on the next start" hint is about.
///
/// Shared with the driver manager, which needs a VM for
/// [`Jvm::probe_drivers`](rudbman_jdbc::Jvm::probe_drivers) and must reach it
/// through the same settings — a probe that ran under a different heap than the
/// connection that follows would be a difference nobody could see.
///
/// **Blocks** on the first call of a process, for as long as
/// `JNI_CreateJavaVM` takes.
pub(crate) fn start_jvm(settings: &AppSettings) -> Result<&'static Jvm, ConnectError> {
    let mut config = JvmConfig::from_settings(settings);
    if config.bridge_jar().as_os_str().is_empty() {
        config = JvmConfig::new(default_bridge_jar());
    }
    Jvm::start(&config).map_err(ConnectError::from)
}

/// Identity of a tunnel, for the purpose of sharing one.
///
/// The bastion and the target, exactly as §9.3 puts it. The authentication
/// method is deliberately *not* part of it: two profiles reaching one database
/// through one bastion are the same tunnel even when one of them authenticates
/// with a key and the other with the agent — whichever got there first is the
/// one that is up. The requested local port is, though: a profile that pins a
/// port is asking for that port and must not be handed somebody else's.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct TunnelKey {
    host: String,
    port: u16,
    username: String,
    remote_host: String,
    remote_port: u16,
    local_port: u16,
}

impl TunnelKey {
    /// The key of one profile's tunnel block.
    fn of(config: &TunnelConfig) -> Self {
        TunnelKey {
            host: config.host.clone(),
            port: config.port,
            username: config.username.clone(),
            remote_host: config.remote_host.clone(),
            remote_port: config.remote_port,
            local_port: config.local_port,
        }
    }
}

/// One shared tunnel and everything hanging off it.
struct Shared {
    /// The transport. Closed by the last lease to be dropped.
    tunnel: SshTunnel,
    /// The port the OS actually bound, which is what goes into the URL.
    local_port: u16,
    /// How many sessions are using it.
    leases: usize,
    /// Set once the tunnel has ended, so a lease taken in the same instant is
    /// not handed a corpse.
    dead: Arc<AtomicBool>,
    /// One sender per lease, fired with the reason when the tunnel ends.
    watchers: Vec<oneshot::Sender<String>>,
}

/// The tunnels currently up, keyed by bastion and target.
///
/// A process-wide map rather than something the workspace owns, because it has
/// to be reachable from the background threads that do the connecting, and
/// those hold no gpui context.
static TUNNELS: Mutex<Option<HashMap<TunnelKey, Arc<Mutex<Shared>>>>> = Mutex::new(None);

/// One session's claim on a shared tunnel.
///
/// Dropping it releases the claim, and the last one out closes the tunnel.
pub struct TunnelLease {
    key: TunnelKey,
    shared: Arc<Mutex<Shared>>,
    local_port: u16,
}

impl fmt::Debug for TunnelLease {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TunnelLease")
            .field(
                "bastion",
                &format_args!("{}:{}", self.key.host, self.key.port),
            )
            .field(
                "target",
                &format_args!("{}:{}", self.key.remote_host, self.key.remote_port),
            )
            .field("local_port", &self.local_port)
            .finish()
    }
}

impl TunnelLease {
    /// The local port the tunnel is listening on.
    pub fn local_port(&self) -> u16 {
        self.local_port
    }

    /// A channel that fires once, with the reason, when the tunnel ends.
    ///
    /// The session above it is then dead and stays dead: rudbman does not
    /// reconnect silently, because a transaction may have been open and the
    /// user has to be the one who decides what happens next (§9.3).
    ///
    /// A tunnel that is *already* down fires immediately, so a caller cannot
    /// miss the news by having asked a moment too late.
    pub fn watch(&self) -> oneshot::Receiver<String> {
        let (sender, receiver) = oneshot::channel();
        let mut shared = self.shared.lock();
        if shared.dead.load(Ordering::Relaxed) {
            let _ = sender.send("the tunnel is down".to_string());
        } else {
            shared.watchers.push(sender);
        }
        receiver
    }
}

impl Drop for TunnelLease {
    fn drop(&mut self) {
        let mut registry = TUNNELS.lock();
        let Some(map) = registry.as_mut() else {
            return;
        };
        let Some(shared) = map.get(&self.key).cloned() else {
            return;
        };
        let last = {
            let mut shared = shared.lock();
            shared.leases = shared.leases.saturating_sub(1);
            shared.leases == 0
        };
        if last {
            map.remove(&self.key);
            // Dropped after the map entry is gone so that a connect racing this
            // one starts a fresh tunnel rather than joining a closing one.
            drop(registry);
            shared.lock().tunnel.close();
        }
    }
}

/// Takes a lease on the tunnel `config` describes, opening one if none is up.
fn open_tunnel(config: &TunnelConfig, secret: Option<String>) -> Result<TunnelLease, ConnectError> {
    let key = TunnelKey::of(config);

    // An existing tunnel: take a lease and go, without touching the network.
    {
        let mut registry = TUNNELS.lock();
        let map = registry.get_or_insert_with(HashMap::new);
        if let Some(shared) = map.get(&key).cloned() {
            let mut guard = shared.lock();
            if !guard.dead.load(Ordering::Relaxed) {
                guard.leases += 1;
                let local_port = guard.local_port;
                drop(guard);
                return Ok(TunnelLease {
                    key,
                    shared,
                    local_port,
                });
            }
            // A tunnel that has died but whose last lease has not been dropped
            // yet must not be handed out again.
            map.remove(&key);
        }
    }

    let spec = TunnelSpec::from_config(config, secret);
    let verifier = Arc::new(StoredHostKeys::new(config.host.clone(), config.port));
    let (tunnel, mut events) = SshTunnel::open(spec, verifier.clone());

    // `open` returns before the transport has done anything; the port is only
    // real once `Ready` says so. Blocking here is what makes `connect` a
    // sequence rather than a callback maze — and this whole call is already on
    // a background thread.
    let local_port = loop {
        match block_on(events.next()) {
            Some(TunnelEvent::Ready { local_port }) => break local_port,
            Some(TunnelEvent::Error(kind, message)) => {
                tunnel.close();
                return Err(verifier.explain(kind, message));
            }
            Some(TunnelEvent::Disconnected { reason }) => {
                tunnel.close();
                return Err(ConnectError::Tunnel(reason));
            }
            Some(_) => continue,
            None => {
                tunnel.close();
                return Err(ConnectError::Tunnel(
                    "the tunnel ended without ever reporting a bound port".to_string(),
                ));
            }
        }
    };

    let dead = Arc::new(AtomicBool::new(false));
    let shared = Arc::new(Mutex::new(Shared {
        tunnel,
        local_port,
        leases: 1,
        dead: Arc::clone(&dead),
        watchers: Vec::new(),
    }));

    // The rest of the event stream is drained on a thread of its own: a tunnel
    // that dies has to reach the sessions above it, and nothing else is
    // listening once `connect` has returned.
    let watched = Arc::clone(&shared);
    std::thread::Builder::new()
        .name("rudbman-tunnel-watch".to_string())
        .spawn(move || {
            let reason = block_on(async move {
                while let Some(event) = events.next().await {
                    match event {
                        TunnelEvent::Error(kind, message) => return format!("{kind}: {message}"),
                        TunnelEvent::Disconnected { reason } => return reason,
                        // A single forwarded socket dying is not the tunnel
                        // dying; the transport is still healthy.
                        other => log::debug!("tunnel event: {other:?}"),
                    }
                }
                "the tunnel ended".to_string()
            });
            dead.store(true, Ordering::Relaxed);
            let watchers = std::mem::take(&mut watched.lock().watchers);
            for watcher in watchers {
                let _ = watcher.send(reason.clone());
            }
        })
        .map_err(|error| ConnectError::Tunnel(error.to_string()))?;

    TUNNELS
        .lock()
        .get_or_insert_with(HashMap::new)
        .insert(key.clone(), Arc::clone(&shared));

    Ok(TunnelLease {
        key,
        shared,
        local_port,
    })
}

/// How many tunnels are currently up.
///
/// An assertion helper: the failure paths of [`connect`] have to leave the
/// registry as they found it, and that is not observable any other way.
#[cfg(test)]
pub fn open_tunnel_count() -> usize {
    TUNNELS.lock().as_ref().map_or(0, HashMap::len)
}

/// Host key policy: `known_hosts` decides, and an unknown key is refused.
///
/// Refusing rather than asking is the conservative half of §9.3's "show the
/// fingerprint and ask": the verifier runs inside the key exchange on the
/// transport thread and must not block, so it cannot put a dialog up and wait.
/// What it does instead is record what it saw, so the connection error can name
/// the fingerprint and the user can accept it from a dialog that is allowed to
/// take its time.
struct StoredHostKeys {
    host: String,
    port: u16,
    /// What the bastion presented, recorded for the error message.
    seen: Mutex<Option<(String, String, bool)>>,
}

impl StoredHostKeys {
    /// A verifier for one bastion.
    fn new(host: String, port: u16) -> Self {
        StoredHostKeys {
            host,
            port,
            seen: Mutex::new(None),
        }
    }

    /// Turns a tunnel failure into the error the dialog shows.
    ///
    /// A host key rejection is re-read from what the verifier recorded, so the
    /// user is told which fingerprint arrived and whether it merely was not
    /// known or actively contradicts one that is.
    fn explain(&self, kind: rudbman_ssh::SshErrorKind, message: String) -> ConnectError {
        if kind != rudbman_ssh::SshErrorKind::HostKeyRejected {
            return ConnectError::Tunnel(format!("{kind}: {message}"));
        }
        let seen = self.seen.lock().clone();
        let Some((algorithm, fingerprint, _)) = seen else {
            return ConnectError::Tunnel(format!("{kind}: {message}"));
        };
        match KnownHosts::load()
            .map(|hosts| hosts.status(&self.host, self.port, &algorithm, &fingerprint))
        {
            Ok(rudbman_core::HostKeyStatus::Mismatch { stored_fingerprint }) => {
                ConnectError::HostKeyMismatch {
                    host: self.host.clone(),
                    presented: fingerprint,
                    stored: stored_fingerprint,
                }
            }
            _ => ConnectError::HostKeyUnknown {
                host: self.host.clone(),
                fingerprint,
                algorithm,
            },
        }
    }
}

#[async_trait::async_trait]
impl HostKeyVerifier for StoredHostKeys {
    async fn verify(&self, host: &str, port: u16, key: &PublicKey) -> bool {
        let algorithm = rudbman_ssh::algorithm_name(key);
        let fingerprint = rudbman_ssh::fingerprint(key);
        let trusted = matches!(
            KnownHosts::load().map(|hosts| hosts.status(host, port, &algorithm, &fingerprint)),
            Ok(rudbman_core::HostKeyStatus::Trusted)
        );
        *self.seen.lock() = Some((algorithm, fingerprint, trusted));
        trusted
    }
}

/// Records `fingerprint` as trusted for `host`, so the next attempt gets through.
///
/// What the "trust this key" button in the connection dialog calls. Writing the
/// file is the whole of it; the tunnel is opened again from scratch afterwards.
pub fn trust_host_key(
    host: &str,
    port: u16,
    algorithm: &str,
    fingerprint: &str,
) -> anyhow::Result<()> {
    let mut hosts = KnownHosts::load()?;
    hosts.trust(host, port, algorithm, fingerprint);
    hosts.save()
}

/// Locating the H2 driver JAR the tests connect through.
///
/// H2 opens an in-memory database with no server behind it, which makes it the
/// one driver a test can take all the way to `OPEN_SESSION` on any machine. The
/// JAR is looked up rather than shipped — it is not redistributable — and a
/// missing one **fails** the test rather than skipping it: a test that passes
/// because it could not find the thing it tests is worse than no test.
#[cfg(test)]
pub(crate) mod h2 {
    use std::path::PathBuf;

    /// The H2 JAR, from `RUDBMAN_TEST_H2_JAR` or the Gradle cache the bridge's
    /// own suite fills.
    pub fn jar() -> PathBuf {
        if let Some(path) = std::env::var_os("RUDBMAN_TEST_H2_JAR") {
            let path = PathBuf::from(path);
            assert!(
                path.is_file(),
                "RUDBMAN_TEST_H2_JAR points at {}, which is not a file",
                path.display()
            );
            return path;
        }
        in_gradle_cache().unwrap_or_else(|| {
            panic!(
                "the H2 driver JAR was not found.\n\
                 \n\
                 These tests open a real connection through a real driver.\n\
                 Fetch it into the Gradle cache by running the bridge's suite:\n\
                 \n    cd bridge && ./gradlew test\n\
                 \n\
                 or point RUDBMAN_TEST_H2_JAR at an h2-*.jar you already have."
            )
        })
    }

    /// Walks `<gradle home>/caches/modules-2/files-2.1/com.h2database/h2/*/*/h2-*.jar`.
    ///
    /// Hand-rolled rather than a glob dependency, and the same walk
    /// `rudbman-jdbc`'s own integration test does — the shape is fixed and this
    /// is the only place in the crate that needs it.
    fn in_gradle_cache() -> Option<PathBuf> {
        let gradle_home = std::env::var_os("GRADLE_USER_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".gradle")))?;
        let root = gradle_home.join("caches/modules-2/files-2.1/com.h2database/h2");

        let mut newest: Option<(String, PathBuf)> = None;
        for version in std::fs::read_dir(&root).ok()?.flatten() {
            let number = version.file_name().to_string_lossy().into_owned();
            // Only the binary artefact: the same directory holds `-javadoc` and
            // `-sources`, and picking one of those gets a class loader with no
            // driver in it.
            let wanted = format!("h2-{number}.jar");
            for hash in std::fs::read_dir(version.path()).ok()?.flatten() {
                for file in std::fs::read_dir(hash.path()).ok()?.flatten() {
                    if file.file_name().to_string_lossy() == wanted
                        && newest
                            .as_ref()
                            .is_none_or(|(best, _)| best.as_str() < number.as_str())
                    {
                        newest = Some((number.clone(), file.path()));
                    }
                }
            }
        }
        newest.map(|(_, path)| path)
    }

    /// The driver definition the tests connect with.
    pub fn driver() -> rudbman_core::DriverDef {
        rudbman_core::DriverDef {
            id: "h2".to_string(),
            name: "H2".to_string(),
            class: "org.h2.Driver".to_string(),
            jars: vec![jar()],
            url_template: "jdbc:h2:tcp://{host}:{port}/{database}".to_string(),
            dialect: "h2".to_string(),
            ..rudbman_core::DriverDef::default()
        }
    }

    /// A profile for an in-memory database no other test shares.
    pub fn profile(name: &str) -> rudbman_core::ConnectionProfile {
        use std::sync::atomic::{AtomicU32, Ordering};
        static SEQ: AtomicU32 = AtomicU32::new(0);
        rudbman_core::ConnectionProfile::new(
            name,
            "h2",
            format!(
                "jdbc:h2:mem:rudbman-app-{}",
                SEQ.fetch_add(1, Ordering::Relaxed)
            ),
            "sa",
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn values(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(key, value)| ((*key).to_string(), (*value).to_string()))
            .collect()
    }

    #[test]
    fn a_template_is_filled_hole_by_hole() {
        let filled = substitute(
            "jdbc:postgresql://{host}:{port}/{database}",
            &values(&[
                ("host", "db.example.com"),
                ("port", "5432"),
                ("database", "app"),
            ]),
        );
        assert_eq!(filled, "jdbc:postgresql://db.example.com:5432/app");

        // SQL Server's semicolon grammar and Oracle's `@//` both go through the
        // same substitution untouched.
        assert_eq!(
            substitute(
                "jdbc:sqlserver://{host}:{port};databaseName={database}",
                &values(&[("host", "sql"), ("port", "1433"), ("database", "app")]),
            ),
            "jdbc:sqlserver://sql:1433;databaseName=app"
        );
        assert_eq!(
            substitute(
                "jdbc:oracle:thin:@//{host}:{port}/{service}",
                &values(&[("host", "ora"), ("port", "1521"), ("service", "ORCLPDB")]),
            ),
            "jdbc:oracle:thin:@//ora:1521/ORCLPDB"
        );
    }

    #[test]
    fn a_hole_nothing_fills_stays_visible() {
        // A hand-written `drivers.json` may use a placeholder the editor has no
        // field for. Leaving it in the preview says so; blanking it would
        // produce a URL that fails at the driver with nothing to point at.
        assert_eq!(
            substitute("jdbc:x://{host}/{warehouse}", &values(&[("host", "h")])),
            "jdbc:x://h/{warehouse}"
        );
        // An unbalanced brace is part of the URL.
        assert_eq!(substitute("jdbc:x:{oops", &values(&[])), "jdbc:x:{oops");
    }

    #[test]
    fn the_holes_a_template_uses_are_listed_in_order_and_once_each() {
        assert_eq!(
            placeholders_of("jdbc:postgresql://{host}:{port}/{database}"),
            vec!["host", "port", "database"]
        );
        assert_eq!(placeholders_of("jdbc:sqlite:{file}"), vec!["file"]);
        assert_eq!(
            placeholders_of("jdbc:x://{host}:{port}/?failover={host}"),
            vec!["host", "port"]
        );
        assert!(placeholders_of("jdbc:h2:mem:test").is_empty());
    }

    #[test]
    fn a_tunnel_moves_only_the_authority_of_a_url() {
        assert_eq!(
            redirect_authority("jdbc:postgresql://db.internal:5432/app", "127.0.0.1", 41234),
            "jdbc:postgresql://127.0.0.1:41234/app"
        );
        // The semicolon grammar: the authority ends at the first `;`.
        assert_eq!(
            redirect_authority(
                "jdbc:sqlserver://db.internal:1433;databaseName=app",
                "127.0.0.1",
                41234
            ),
            "jdbc:sqlserver://127.0.0.1:41234;databaseName=app"
        );
        // A query string.
        assert_eq!(
            redirect_authority(
                "jdbc:mysql://db.internal:3306/app?useSSL=false",
                "127.0.0.1",
                1
            ),
            "jdbc:mysql://127.0.0.1:1/app?useSSL=false"
        );
        // A URL with no authority at all has no server to redirect.
        assert_eq!(
            redirect_authority("jdbc:h2:mem:test", "127.0.0.1", 41234),
            "jdbc:h2:mem:test"
        );
        assert_eq!(
            redirect_authority("jdbc:sqlite:/var/db/app.sqlite", "127.0.0.1", 1),
            "jdbc:sqlite:/var/db/app.sqlite"
        );
    }

    #[test]
    fn the_spec_carries_the_profile_and_nothing_it_was_not_given() {
        let mut profile = ConnectionProfile::new("staging", "postgresql", "jdbc:x", "alice");
        profile.read_only = true;
        profile.auto_commit = false;
        profile.props.insert("ssl".into(), "true".into());
        profile.keep_alive = Some(rudbman_core::KeepAlive {
            interval_s: 60,
            query: "select 1".into(),
        });

        let driver = DriverDef {
            id: "postgresql".into(),
            class: "org.postgresql.Driver".into(),
            jars: vec!["/tmp/pg.jar".into()],
            ..DriverDef::default()
        };
        let credentials = Credentials::typed(Some("hunter2".into()), None);
        let spec = build_spec(&profile, &driver, &credentials, "jdbc:postgresql://h:1/d");

        assert_eq!(spec.url, "jdbc:postgresql://h:1/d");
        assert_eq!(spec.driver_class, "org.postgresql.Driver");
        assert_eq!(spec.jars, vec![std::path::PathBuf::from("/tmp/pg.jar")]);
        assert_eq!(spec.username.as_deref(), Some("alice"));
        assert!(spec.read_only);
        assert!(!spec.auto_commit);
        assert_eq!(spec.props.get("ssl").map(String::as_str), Some("true"));
        assert_eq!(
            spec.keep_alive.as_ref().map(|keep| keep.interval_s),
            Some(60)
        );

        // The password is in the spec and nowhere in its rendering.
        assert_eq!(spec.password.as_deref(), Some("hunter2"));
        assert!(!format!("{spec:?}").contains("hunter2"), "{spec:?}");
    }

    #[test]
    fn a_profile_with_no_user_name_sends_none_rather_than_an_empty_one() {
        // An empty `user` property is not the same as no property: several
        // drivers take the empty string as a login attempt and fail with a
        // message about a user called "".
        let profile = ConnectionProfile::new("x", "h2", "jdbc:h2:mem:t", "");
        let driver = DriverDef {
            jars: vec!["/tmp/h2.jar".into()],
            ..DriverDef::default()
        };
        let spec = build_spec(
            &profile,
            &driver,
            &Credentials::typed(None, None),
            "jdbc:h2:mem:t",
        );
        assert_eq!(spec.username, None);
    }

    #[test]
    fn a_driver_with_no_jar_is_refused_before_anything_is_opened() {
        // No JVM is started and no tunnel goes up: the check is the first thing
        // `connect` does, which is what makes this test cheap enough to run.
        let profile = ConnectionProfile::new("x", "h2", "jdbc:h2:mem:t", "sa");
        let driver = DriverDef {
            name: "H2".into(),
            ..DriverDef::default()
        };
        let error = connect(
            &profile,
            &driver,
            &Credentials::typed(None, None),
            &AppSettings::default(),
        )
        .expect_err("a driver without a JAR cannot connect");
        assert!(matches!(error, ConnectError::NoDriverJar(_)), "{error:?}");
        assert!(error.is_driver());
        assert_eq!(open_tunnel_count(), 0, "nothing may have been opened");
    }

    #[test]
    fn two_profiles_through_one_bastion_share_a_key() {
        let base = TunnelConfig {
            host: "bastion".into(),
            port: 22,
            username: "ops".into(),
            remote_host: "db.internal".into(),
            remote_port: 5432,
            ..TunnelConfig::default()
        };
        // Same bastion, same target, different auth: one tunnel.
        let with_key = TunnelConfig {
            auth: rudbman_core::TunnelAuth::Key {
                path: "/home/me/.ssh/id".into(),
            },
            ..base.clone()
        };
        assert_eq!(TunnelKey::of(&base), TunnelKey::of(&with_key));

        // A different target is a different tunnel, and so is a pinned local
        // port: a profile that asks for one is asking for that one.
        let other_target = TunnelConfig {
            remote_port: 5433,
            ..base.clone()
        };
        assert_ne!(TunnelKey::of(&base), TunnelKey::of(&other_target));
        let pinned = TunnelConfig {
            local_port: 15432,
            ..base.clone()
        };
        assert_ne!(TunnelKey::of(&base), TunnelKey::of(&pinned));
    }

    #[test]
    fn an_authentication_failure_is_told_apart_by_its_sqlstate_class() {
        let bridge = |state: &str| {
            ConnectError::Database(
                serde_json::from_str::<BridgeError>(&format!(
                    r#"{{"kind":"sql","sql_state":"{state}","vendor_code":28000,
                        "message":"invalid credentials","causes":[],"stack":null}}"#
                ))
                .expect("envelope"),
            )
        };
        assert!(bridge("28000").is_authentication());
        assert_eq!(bridge("28P01").sql_state_class(), Some("28"));
        assert!(!bridge("08001").is_authentication());
        // A failure that never reached the database has no SQLSTATE at all.
        assert_eq!(ConnectError::Tunnel("nope".into()).sql_state_class(), None);
    }

    #[test]
    fn a_driver_failure_is_told_apart_from_a_database_one() {
        let driver: BridgeError = serde_json::from_str(
            r#"{"kind":"driver","sql_state":null,"vendor_code":0,
                "message":"no suitable driver","causes":[],"stack":null}"#,
        )
        .expect("envelope");
        assert!(ConnectError::Database(driver).is_driver());

        let sql: BridgeError = serde_json::from_str(
            r#"{"kind":"sql","sql_state":"28000","vendor_code":0,
                "message":"nope","causes":[],"stack":null}"#,
        )
        .expect("envelope");
        assert!(!ConnectError::Database(sql).is_driver());
    }

    /// The whole of what this module exists for, against a real database.
    ///
    /// A real JVM, the real bridge JAR, the real H2 driver: the substituted URL
    /// reaches `OPEN_SESSION`, `SESSION_INFO` comes back naming the product, and
    /// the session closes. Everything above this — the tab, the status bar — is
    /// a rendering of what `SESSION_INFO` said, so if this passes and the bar is
    /// wrong the bug is in the view.
    #[test]
    fn a_real_h2_session_opens_reports_its_product_and_closes() {
        let profile = h2::profile("integration");
        let connected = connect(
            &profile,
            &h2::driver(),
            &Credentials::typed(Some(String::new()), None),
            &AppSettings::default(),
        )
        .expect("H2 opens an in-memory database without a server");

        let product = connected.product().expect("H2 names itself");
        assert!(product.starts_with("H2 "), "{product}");
        // The version is the JAR's, so the assertion is that one is there at
        // all rather than which one.
        assert!(
            product
                .split_whitespace()
                .nth(1)
                .is_some_and(|version| { version.split('.').count() >= 2 }),
            "no version in {product}"
        );
        assert!(connected.session().ping().expect("ping").ok);
        assert!(connected.lease().is_none(), "no tunnel was asked for");
        connected.close().expect("the session closes");
        assert_eq!(open_tunnel_count(), 0);
    }

    /// The failure a user sees most: the right database, the wrong password.
    ///
    /// H2 answers `28000` for it, which is the `SQLSTATE` class the dialog turns
    /// into "check the user name and password". The message has to be the
    /// driver's own and must not carry the password that was refused.
    #[test]
    fn a_wrong_password_comes_back_as_an_authentication_failure() {
        // The database has to exist before a wrong password can be refused by
        // it rather than by its absence, and `DB_CLOSE_DELAY=-1` is what keeps
        // it alive between the two connections.
        let mut profile = h2::profile("wrong-password");
        profile.url = format!("{};DB_CLOSE_DELAY=-1", profile.url);
        let created = connect(
            &profile,
            &h2::driver(),
            &Credentials::typed(Some("hunter2".into()), None),
            &AppSettings::default(),
        )
        .expect("the first connection creates the database");

        let error = connect(
            &profile,
            &h2::driver(),
            &Credentials::typed(Some("not-hunter2".into()), None),
            &AppSettings::default(),
        )
        .expect_err("a wrong password must be refused");

        assert!(
            error.is_authentication(),
            "expected SQLSTATE class 28, got {:?}: {error}",
            error.sql_state_class()
        );
        let message = error.message();
        assert!(!message.is_empty());
        assert!(
            !message.contains("not-hunter2") && !message.contains("hunter2"),
            "the refused password reached the message: {message}"
        );
        created.close().expect("close");
    }

    /// A driver class that is not in the JAR is a *driver* failure, not a
    /// database one — a different message, pointing at a different fix.
    #[test]
    fn a_driver_class_that_is_not_in_the_jar_is_a_driver_failure() {
        let driver = DriverDef {
            class: "org.h2.NoSuchDriver".to_string(),
            ..h2::driver()
        };
        let error = connect(
            &h2::profile("bad-class"),
            &driver,
            &Credentials::typed(Some(String::new()), None),
            &AppSettings::default(),
        )
        .expect_err("a missing driver class cannot connect");
        assert!(error.is_driver(), "{error:?}");
        assert!(!error.is_authentication());
    }

    #[test]
    fn the_cause_chain_is_appended_to_the_message() {
        // Drivers hide the reason in `getNextException` often enough that a
        // message without it reads as "connection failed" and nothing more.
        let error: BridgeError = serde_json::from_str(
            r#"{"kind":"sql","sql_state":"08001","vendor_code":0,
                "message":"connection refused",
                "causes":["java.net.ConnectException: Connection refused"],
                "stack":"at ..."}"#,
        )
        .expect("envelope");
        let message = ConnectError::Database(error).message();
        assert!(message.contains("connection refused"), "{message}");
        assert!(message.contains("Connection refused"), "{message}");
        // Never the stack.
        assert!(!message.contains("at ..."), "{message}");
    }

    #[test]
    fn no_route_to_host_is_recognised_from_either_layer() {
        // The SSH tunnel path: `rudbman-ssh` surfaces the OS error as text.
        assert!(is_no_route(&ConnectError::Tunnel(
            "connect: No route to host (os error 65)".to_string()
        )));
        // A differently worded tunnel failure is not the same thing.
        assert!(!is_no_route(&ConnectError::Tunnel(
            "connect: Connection refused (os error 61)".to_string()
        )));

        // The direct JDBC path: the bridge's `NoRouteToHostException` message
        // comes back inside a `BridgeError`.
        let bridge: BridgeError = serde_json::from_str(
            r#"{"kind":"io","sql_state":null,"vendor_code":0,
                "message":"java.net.NoRouteToHostException: No route to host",
                "causes":[],"stack":"at ..."}"#,
        )
        .expect("envelope");
        assert!(is_no_route(&ConnectError::Database(bridge)));

        // Variants that fail before any socket is touched are never it, even
        // if their text happened to mention hosts or routes.
        assert!(!is_no_route(&ConnectError::NoDriverJar(
            "no route to host driver".to_string()
        )));
        assert!(!is_no_route(&ConnectError::JvmStart(
            "no route to host".to_string()
        )));
    }
}
