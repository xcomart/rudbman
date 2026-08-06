//! JVM bootstrap and the one JNI call this crate makes (architecture document,
//! §4.1 and §4.3).
//!
//! # One JVM per process
//!
//! JNI cannot create a second VM after the first, so [`Jvm::start`] is
//! idempotent: the first call builds the VM and every later call — including
//! from another thread — hands back the same one, configuration and all.
//!
//! **`DestroyJavaVM` is never called.** It is unreliable in the presence of
//! attached threads, it can block forever, and process exit does the same job
//! for free.
//!
//! # Why a dedicated thread creates it
//!
//! `JNI_CreateJavaVM` attributes special significance to the thread that calls
//! it and that thread has to stay alive, so the VM is created on a thread of
//! this crate's own that then parks forever. On macOS the alternative would be
//! the main thread, and gpui owns that one.
//!
//! # The options are not negotiable
//!
//! Everything in [`JvmConfig::options`] is fixed except the heap, the locale and
//! the caller's extra arguments. `-Xrs` in particular: without it the JVM
//! installs its own SIGINT/SIGTERM handlers and swallows Ctrl-C and the window
//! close, and the symptom — a window that will not close — points nowhere near
//! the JVM.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::sync::mpsc;

use jni::objects::{JByteArray, JClass, JStaticMethodID};
use jni::refs::Global;
use jni::signature::ReturnType;
use jni::sys::{jint, jvalue};
use jni::{Env, InitArgsBuilder, JNIVersion, JavaVM, jni_sig, jni_str};
use parking_lot::Mutex;
use rudbman_core::AppSettings;

use crate::error::{Error, Result};
use crate::protocol::{Op, parse_json, take_payload};
use crate::response::DriverProbe;
use crate::spec::ProbeRequest;

/// The class holding the single entry point.
const BRIDGE_CLASS: &str = "comart/rudbman/bridge/Bridge";

/// Environment variable that overrides where the Java runtime is looked for.
pub const JAVA_HOME_ENV: &str = "RUDBMAN_JAVA_HOME";

/// Environment variable that overrides where the bridge JAR is looked for.
pub const BRIDGE_JAR_ENV: &str = "RUDBMAN_BRIDGE_JAR";

/// The process-wide VM, once started.
static JVM: OnceLock<Jvm> = OnceLock::new();

/// Serialises start-up so two threads racing into [`Jvm::start`] cannot both
/// try to create a VM. `OnceLock::get_or_init` cannot do this on its own
/// because building the VM is fallible and a failure must not poison the slot.
static STARTING: Mutex<()> = Mutex::new(());

/// How the JVM is started.
///
/// Only the first call to [`Jvm::start`] in a process uses one of these; see
/// that method.
#[derive(Clone, Debug)]
pub struct JvmConfig {
    bridge_jar: PathBuf,
    java_home: Option<PathBuf>,
    heap_mb: u32,
    extra_args: Vec<String>,
    language: Option<String>,
    country: Option<String>,
}

impl JvmConfig {
    /// A configuration with the default heap and no extra arguments.
    pub fn new(bridge_jar: impl Into<PathBuf>) -> Self {
        JvmConfig {
            bridge_jar: bridge_jar.into(),
            java_home: None,
            heap_mb: 1024,
            extra_args: Vec::new(),
            language: None,
            country: None,
        }
    }

    /// The configuration the application settings ask for.
    ///
    /// The heap and the extra arguments come from `settings.json`; the JAR is
    /// resolved by [`default_bridge_jar`]. Both settings only take effect on
    /// the next process start, because a JVM's heap cannot be resized and there
    /// is only ever one JVM.
    pub fn from_settings(settings: &AppSettings) -> Self {
        let mut config = JvmConfig::new(default_bridge_jar());
        config.heap_mb = settings.jvm_heap_mb;
        config.extra_args = settings.jvm_extra_args.clone();
        if let Some(tag) = &settings.language {
            // BCP 47: "ko", "zh-CN", "pt-BR". Java wants the two halves apart.
            let mut parts = tag.split(['-', '_']);
            config.language = parts
                .next()
                .filter(|part| !part.is_empty())
                .map(str::to_string);
            config.country = parts.next().map(str::to_uppercase);
        }
        config
    }

    /// Overrides the Java runtime location, ahead of every other candidate.
    pub fn with_java_home(mut self, java_home: impl Into<PathBuf>) -> Self {
        self.java_home = Some(java_home.into());
        self
    }

    /// Sets the maximum heap in megabytes (`-Xmx`).
    pub fn with_heap_mb(mut self, heap_mb: u32) -> Self {
        self.heap_mb = heap_mb;
        self
    }

    /// Appends extra JVM arguments, after the ones this crate sets.
    pub fn with_extra_args(mut self, args: impl IntoIterator<Item = String>) -> Self {
        self.extra_args.extend(args);
        self
    }

    /// The bridge JAR this configuration will put on the class path.
    pub fn bridge_jar(&self) -> &Path {
        &self.bridge_jar
    }

    /// The full option list, in the order the JVM will see it.
    ///
    /// Public so that a start-up failure can be logged with the exact arguments
    /// that produced it.
    pub fn options(&self) -> Vec<String> {
        let mut options = vec![
            format!("-Djava.class.path={}", self.bridge_jar.display()),
            // Nothing here ever draws: a driver that reaches for AWT on a
            // headless box would fail with an unrelated-looking error.
            "-Djava.awt.headless=true".to_string(),
            // Keep the JVM's hands off SIGINT/SIGTERM (appendix A).
            "-Xrs".to_string(),
            // Some drivers — Oracle's above all — recurse deeply.
            "-Xss2m".to_string(),
            // A desktop tool's heap is small; parallel GC threads cost more
            // than they save.
            "-XX:+UseSerialGC".to_string(),
            format!("-Xmx{}m", self.heap_mb),
        ];
        // Matching the app locale is what makes a driver's error messages come
        // back in the language the rest of the window is in.
        if let Some(language) = &self.language {
            options.push(format!("-Duser.language={language}"));
        }
        if let Some(country) = &self.country {
            options.push(format!("-Duser.country={country}"));
        }
        options.extend(self.extra_args.iter().cloned());
        options
    }

    /// Picks the Java runtime, in the order of architecture document §4.1.
    ///
    /// `None` means none of the candidates looked like a runtime, in which case
    /// start-up falls back on whatever `java` on `PATH` resolves to.
    fn resolve_java_home(&self) -> Option<PathBuf> {
        // An explicit setting wins: a caller that already resolved a runtime
        // (a test, or an installer) should not be second-guessed.
        let explicit = self.java_home.clone();
        explicit
            .into_iter()
            .chain(bundled_runtime())
            .chain(std::env::var_os(JAVA_HOME_ENV).map(PathBuf::from))
            .chain(std::env::var_os("JAVA_HOME").map(PathBuf::from))
            .find(|candidate| is_java_home(candidate))
    }
}

/// The bundled runtime beside the executable, in the order of architecture
/// document §4.1: `<exe_dir>/runtime` — the flat archive Windows and Linux
/// ship — then `<exe_dir>/../runtime`, which on macOS is
/// `<bundle>/Contents/runtime`.
fn bundled_runtime() -> Vec<PathBuf> {
    std::env::current_exe()
        .ok()
        .and_then(|exe| runtime_candidates(&exe))
        .unwrap_or_default()
}

/// The places a runtime bundled with the executable at `exe` could be.
fn runtime_candidates(exe: &Path) -> Option<Vec<PathBuf>> {
    let dir = exe.parent()?;
    let mut candidates = vec![dir.join("runtime")];
    if let Some(above) = dir.parent() {
        candidates.push(above.join("runtime"));
    }
    Some(candidates)
}

/// Whether a directory holds a JVM shared library where java-locator will look.
fn is_java_home(path: &Path) -> bool {
    const LIBRARIES: [&str; 4] = [
        "lib/server/libjvm.so",
        "lib/server/libjvm.dylib",
        "lib/libjvm.dylib",
        "bin/server/jvm.dll",
    ];
    LIBRARIES.iter().any(|library| path.join(library).is_file())
}

/// Where to look for the bridge JAR when nothing says otherwise.
///
/// In order: the `RUDBMAN_BRIDGE_JAR` environment variable, `lib/` beside the
/// executable, the executable's own directory, and finally the path this crate
/// was built against — which is what makes `cargo test` work from a checkout.
pub fn default_bridge_jar() -> PathBuf {
    const JAR_NAME: &str = "rudbman-bridge.jar";

    if let Some(path) = std::env::var_os(BRIDGE_JAR_ENV) {
        return PathBuf::from(path);
    }
    if let Ok(exe) = std::env::current_exe()
        && let Some(dir) = exe.parent()
    {
        for candidate in [dir.join("lib").join(JAR_NAME), dir.join(JAR_NAME)] {
            if candidate.is_file() {
                return candidate;
            }
        }
    }
    PathBuf::from(env!("RUDBMAN_BRIDGE_JAR"))
}

/// The running JVM and the one method handle this crate caches.
///
/// Cheap to pass around: [`Jvm::start`] hands out a `&'static Jvm`, since the
/// VM outlives everything anyway.
pub struct Jvm {
    vm: JavaVM,
    /// A global reference, not a local one: a method ID is only valid while its
    /// class stays loaded, and this reference is what keeps it loaded.
    bridge_class: Global<JClass<'static>>,
    /// `Bridge.call`. **The only cached method ID in the crate** — having a
    /// single entry point is precisely what makes that possible.
    call_method: JStaticMethodID,
}

impl std::fmt::Debug for Jvm {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Jvm").finish_non_exhaustive()
    }
}

impl Jvm {
    /// Starts the process-wide JVM, or returns the one already running.
    ///
    /// **The configuration only applies to the first call.** JNI allows exactly
    /// one VM per process and offers no way to reconfigure it, so a later call
    /// with a different [`JvmConfig`] returns the running VM and ignores the
    /// difference. Callers that need different options must change them and
    /// restart the process, which is what the settings dialog tells the user.
    pub fn start(config: &JvmConfig) -> Result<&'static Jvm> {
        if let Some(jvm) = JVM.get() {
            return Ok(jvm);
        }
        let _guard = STARTING.lock();
        // Someone may have won the race between the check above and the lock.
        if let Some(jvm) = JVM.get() {
            return Ok(jvm);
        }
        let jvm = boot(config)?;
        Ok(JVM.get_or_init(|| jvm))
    }

    /// The running JVM, if one has been started.
    pub fn get() -> Option<&'static Jvm> {
        JVM.get()
    }

    /// Lists the JDBC drivers a set of JARs offers, without loading any of them.
    ///
    /// This is the one operation that belongs on the JVM rather than on a
    /// [`Session`](crate::Session): it is what the driver manager asks *before*
    /// any connection exists — the user has just picked a file and wants to be
    /// told the class name instead of having to find it. It runs on the calling
    /// thread (attach, call, detach), which is safe because the bridge answers
    /// it from a throwaway class loader that it closes again, holding no state
    /// and pinning no file. Expect it to block for as long as it takes to read
    /// the archives.
    ///
    /// # What comes back
    ///
    /// | Input | Result |
    /// |---|---|
    /// | A driver JAR | [`DriverProbe`] with the classes found and, usually, the `META-INF/services` declaration |
    /// | A JAR with no `java.sql.Driver` in it | `Ok` with **both lists empty** — not an error. See [`DriverProbe::is_empty`] |
    /// | A file that is not an archive at all | `Ok` with both lists empty: nothing that reads as a zip entry is found, and there is nothing to report |
    /// | A path that does not exist | `Err` with [`BridgeErrorKind::Driver`](crate::BridgeErrorKind::Driver) and the absolute path in the message |
    /// | A damaged archive | `Err` with [`BridgeErrorKind::Io`](crate::BridgeErrorKind::Io) when the entry stream fails part way through |
    /// | An empty `jars` slice | `Err` with [`BridgeErrorKind::Protocol`](crate::BridgeErrorKind::Protocol). Passed through to the bridge rather than short-circuited here, so there is one answer to this question and not two |
    ///
    /// A class that fails to resolve — half the classes in a driver JAR
    /// reference optional dependencies that were not shipped — is skipped
    /// silently. That is the normal case, not a failure.
    pub fn probe_drivers(&self, jars: &[PathBuf]) -> Result<DriverProbe> {
        let request = serde_json::to_vec(&ProbeRequest::new(jars.to_vec()))?;
        let response = self.call_detached(Op::ProbeDriver, 0, 0, Some(&request))?;
        parse_json(&take_payload(response)?)
    }

    /// The underlying VM handle, for attaching threads.
    pub(crate) fn vm(&self) -> &JavaVM {
        &self.vm
    }

    /// Invokes `Bridge.call` on a thread that is already attached.
    ///
    /// Returns the raw response envelope; see
    /// [`take_payload`](crate::protocol::take_payload).
    ///
    /// No `ExceptionCheck` on the normal path: the bridge catches every
    /// `Throwable` and answers with an ERROR envelope, so an exception crossing
    /// this boundary would be a bridge bug. `jni-rs` still checks after the
    /// call, which covers the fatal cases (an `OutOfMemoryError` while building
    /// the response, say).
    pub(crate) fn call_attached(
        &self,
        env: &mut Env<'_>,
        op: Op,
        handle: i64,
        arg: i64,
        request: Option<&[u8]>,
    ) -> Result<Vec<u8>> {
        // A frame per call: the worker thread stays attached for the life of
        // the session, and without this the request and response arrays would
        // pile up in the attachment's frame until the session closed.
        env.with_local_frame(4, |env| {
            let request = match request {
                Some(bytes) => env.byte_array_from_slice(bytes)?,
                None => JByteArray::null(),
            };

            // `jvalue` is a raw union; writing one is safe, reading it is what
            // the JNI call below does under the `unsafe`.
            let args = [
                jvalue {
                    i: op.code() as jint,
                },
                jvalue { j: handle },
                jvalue { j: arg },
                jvalue {
                    l: request.as_raw(),
                },
            ];

            // SAFETY: `call_method` was looked up on `bridge_class` with the
            // signature `(IJJ[B)[B`, and the four arguments above are int,
            // long, long and a byte array in that order.
            let returned = unsafe {
                env.call_static_method_unchecked(
                    &self.bridge_class,
                    self.call_method,
                    ReturnType::Array,
                    &args,
                )?
            };

            let object = returned.l()?;
            if object.is_null() {
                return Err(Error::Protocol(
                    "Bridge.call returned null, which it documents it never does".into(),
                ));
            }
            // SAFETY: the method's return type is `[B`, and `into_raw` gives up
            // the only owning wrapper for that reference.
            let array = unsafe { JByteArray::from_raw(env, object.into_raw()) };
            Ok(env.convert_byte_array(&array)?)
        })
    }

    /// Invokes `Bridge.call` from a thread that may not be attached, attaching
    /// and detaching around it.
    ///
    /// Only `CANCEL` takes this path. It has to run while the session worker is
    /// blocked inside the very statement it is cancelling, so it cannot be
    /// queued behind it, and it is rare enough that the cost of an attach does
    /// not matter (architecture document, §4.2).
    pub(crate) fn call_detached(
        &self,
        op: Op,
        handle: i64,
        arg: i64,
        request: Option<&[u8]>,
    ) -> Result<Vec<u8>> {
        self.vm.attach_current_thread_for_scope(|env| {
            self.call_attached(env, op, handle, arg, request)
        })
    }
}

/// Creates the VM on a thread of our own and waits for the result.
fn boot(config: &JvmConfig) -> Result<Jvm> {
    if !config.bridge_jar.is_file() {
        return Err(Error::JvmStart(format!(
            "the bridge JAR is missing: {} (build it with `cd bridge && ./gradlew jar`)",
            config.bridge_jar.display()
        )));
    }

    let options = config.options();
    let java_home = config.resolve_java_home();
    let jar = config.bridge_jar.clone();
    log::debug!(
        "starting the JVM with JAVA_HOME={:?} and options {:?}",
        java_home,
        options
    );

    let (tx, rx) = mpsc::channel();
    std::thread::Builder::new()
        .name("rudbman-jvm".to_string())
        .spawn(move || {
            let result = create(options, java_home, &jar);
            let started = result.is_ok();
            if tx.send(result).is_err() {
                return;
            }
            // The creating thread is special to the JVM and has to outlive it,
            // and the JVM outlives everything. Parking costs one idle thread.
            if started {
                loop {
                    std::thread::park();
                }
            }
        })
        .map_err(|source| Error::JvmStart(format!("cannot spawn the JVM thread: {source}")))?;

    rx.recv()
        .map_err(|_| Error::JvmStart("the JVM bootstrap thread died silently".into()))?
}

/// Runs on the bootstrap thread: sets `JAVA_HOME`, creates the VM, resolves
/// `Bridge.call`.
fn create(options: Vec<String>, java_home: Option<PathBuf>, jar: &Path) -> Result<Jvm> {
    if let Some(home) = &java_home {
        // SAFETY: `set_var` is unsound only when another thread reads or writes
        // the environment at the same time. This runs once, from the single
        // thread inside the `STARTING` lock, before any session thread of this
        // crate exists — and it has to happen here rather than in the caller,
        // because java-locator reads `JAVA_HOME` inside `JavaVM::new` below.
        unsafe { std::env::set_var("JAVA_HOME", home) };
    }

    let mut builder = InitArgsBuilder::new().version(JNIVersion::V1_8);
    for option in options {
        builder = builder.option(option);
    }
    let args = builder
        .build()
        .map_err(|source| Error::JvmStart(format!("invalid JVM options: {source}")))?;

    let vm = JavaVM::new(args).map_err(|source| {
        Error::JvmStart(format!(
            "{source} (JAVA_HOME={}; set {JAVA_HOME_ENV} to point at a Java 17 runtime)",
            java_home
                .as_deref()
                .map(|home| home.display().to_string())
                .unwrap_or_else(|| "<unset>".to_string()),
        ))
    })?;

    vm.attach_current_thread(|env| {
        // The literal is repeated because `jni_str!` transcodes at compile time
        // and only takes a literal; `BRIDGE_CLASS` above is the same name for
        // the error messages.
        let class = env
            .find_class(jni_str!("comart/rudbman/bridge/Bridge"))
            .map_err(|source| {
                Error::JvmStart(format!(
                    "the bridge class {BRIDGE_CLASS} is not in {}: {source}",
                    jar.display()
                ))
            })?;
        let bridge_class = env.new_global_ref(class)?;
        let call_method = env
            .get_static_method_id(&bridge_class, jni_str!("call"), jni_sig!("(IJJ[B)[B"))
            .map_err(|source| {
                Error::JvmStart(format!(
                    "{BRIDGE_CLASS}.call(int,long,long,byte[]): {source}"
                ))
            })?;
        Ok(Jvm {
            vm: vm.clone(),
            bridge_class,
            call_method,
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_fixed_options_are_all_there() {
        let options = JvmConfig::new("/tmp/rudbman-bridge.jar")
            .with_heap_mb(2048)
            .options();
        assert_eq!(options[0], "-Djava.class.path=/tmp/rudbman-bridge.jar");
        assert!(options.contains(&"-Djava.awt.headless=true".to_string()));
        assert!(
            options.contains(&"-Xrs".to_string()),
            "without -Xrs the JVM eats SIGINT and the window will not close"
        );
        assert!(options.contains(&"-Xss2m".to_string()));
        assert!(options.contains(&"-XX:+UseSerialGC".to_string()));
        assert!(options.contains(&"-Xmx2048m".to_string()));
    }

    #[test]
    fn extra_arguments_come_last_so_they_can_override() {
        let options = JvmConfig::new("/tmp/x.jar")
            .with_extra_args(["-Doracle.jdbc.timezoneAsRegion=false".to_string()])
            .options();
        assert_eq!(
            options.last().map(String::as_str),
            Some("-Doracle.jdbc.timezoneAsRegion=false")
        );
    }

    #[test]
    fn settings_supply_the_heap_the_extra_args_and_the_locale() {
        let settings = AppSettings {
            jvm_heap_mb: 512,
            jvm_extra_args: vec!["-Dfoo=bar".to_string()],
            language: Some("zh-CN".to_string()),
            ..AppSettings::default()
        };
        let options = JvmConfig::from_settings(&settings).options();
        assert!(options.contains(&"-Xmx512m".to_string()));
        assert!(options.contains(&"-Dfoo=bar".to_string()));
        assert!(options.contains(&"-Duser.language=zh".to_string()));
        assert!(options.contains(&"-Duser.country=CN".to_string()));
    }

    #[test]
    fn a_directory_without_a_jvm_library_is_not_a_java_home() {
        assert!(!is_java_home(Path::new("/definitely/not/a/runtime")));
    }

    #[test]
    fn the_bundled_runtime_is_looked_for_beside_the_executable_then_above_it() {
        let candidates = runtime_candidates(Path::new("/opt/rudbman/bin/rudbman")).unwrap();
        assert_eq!(
            candidates,
            vec![
                PathBuf::from("/opt/rudbman/bin/runtime"),
                PathBuf::from("/opt/rudbman/runtime"),
            ],
            "the flat archive's spelling must win over the macOS bundle's"
        );
    }
}
