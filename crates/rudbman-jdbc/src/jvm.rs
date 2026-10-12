//! Application settings and protocol adapter for the shared JVM runtime.
use crate::error::{Error, Result};
use crate::protocol::{Op, parse_json, take_payload};
use crate::response::DriverProbe;
use crate::spec::ProbeRequest;
use jni::{Env, JavaVM};
use rudbman_core::AppSettings;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
/// Java runtime environment override.
pub const JAVA_HOME_ENV: &str = "RUDBMAN_JAVA_HOME";
/// Bridge JAR environment override.
pub const BRIDGE_JAR_ENV: &str = "RUDBMAN_BRIDGE_JAR";
const IDENTITY: rudbkit_jdbc::RuntimeIdentity = rudbkit_jdbc::RuntimeIdentity {
    name: "rudbman",
    bridge_class: "comart/rudbman/bridge/Bridge",
    java_home_env: JAVA_HOME_ENV,
};
/// Application configuration for the shared JVM.
#[derive(Clone, Debug)]
pub struct JvmConfig(rudbkit_jdbc::JvmConfig);
impl JvmConfig {
    /// Creates default runtime options for the given bridge.
    pub fn new(jar: impl Into<PathBuf>) -> Self {
        Self(rudbkit_jdbc::JvmConfig::new(jar, IDENTITY))
    }
    /// Applies application heap, extra arguments and locale.
    pub fn from_settings(settings: &AppSettings) -> Self {
        Self(
            rudbkit_jdbc::JvmConfig::new(default_bridge_jar(), IDENTITY)
                .with_heap_mb(settings.jvm_heap_mb)
                .with_extra_args(settings.jvm_extra_args.clone())
                .with_locale(settings.language.as_deref()),
        )
    }
    /// Overrides the Java runtime location.
    pub fn with_java_home(self, path: impl Into<PathBuf>) -> Self {
        Self(self.0.with_java_home(path))
    }
    /// Sets maximum heap size in megabytes.
    pub fn with_heap_mb(self, size: u32) -> Self {
        Self(self.0.with_heap_mb(size))
    }
    /// Appends arguments after the standard options.
    pub fn with_extra_args(self, args: impl IntoIterator<Item = String>) -> Self {
        Self(self.0.with_extra_args(args))
    }
    /// The bridge archive on the class path.
    pub fn bridge_jar(&self) -> &Path {
        self.0.bridge_jar()
    }
    /// The complete JVM option list.
    pub fn options(&self) -> Vec<String> {
        self.0.options()
    }
}
/// Locates this application's bridge archive.
pub fn default_bridge_jar() -> PathBuf {
    rudbkit_jdbc::default_bridge_jar(
        BRIDGE_JAR_ENV,
        "rudbman-bridge.jar",
        Path::new(env!("RUDBMAN_BRIDGE_JAR")),
    )
}
static JVM: OnceLock<Jvm> = OnceLock::new();
/// The process-wide runtime with this application's bridge protocol.
#[derive(Debug)]
pub struct Jvm(&'static rudbkit_jdbc::Jvm);
impl Jvm {
    /// Starts the runtime once; later calls reuse its original configuration.
    pub fn start(config: &JvmConfig) -> Result<&'static Self> {
        let runtime = rudbkit_jdbc::Jvm::start(&config.0)?;
        Ok(JVM.get_or_init(|| Self(runtime)))
    }
    /// The runtime if it has already started.
    pub fn get() -> Option<&'static Self> {
        JVM.get()
    }
    /// Discovers JDBC driver classes without opening a database connection.
    pub fn probe_drivers(&self, jars: &[PathBuf]) -> Result<DriverProbe> {
        let request = serde_json::to_vec(&ProbeRequest::new(jars.to_vec()))?;
        let response = self.call_detached(Op::ProbeDriver, 0, 0, Some(&request))?;
        parse_json(&take_payload(response)?)
    }
    pub(crate) fn vm(&self) -> &JavaVM {
        self.0.vm()
    }
    pub(crate) fn call_attached(
        &self,
        env: &mut Env<'_>,
        op: Op,
        handle: i64,
        arg: i64,
        request: Option<&[u8]>,
    ) -> Result<Vec<u8>> {
        Ok(self
            .0
            .call_attached(env, op.code().into(), handle, arg, request)?)
    }
    pub(crate) fn call_detached(
        &self,
        op: Op,
        handle: i64,
        arg: i64,
        request: Option<&[u8]>,
    ) -> Result<Vec<u8>> {
        Ok(self
            .0
            .call_detached(op.code().into(), handle, arg, request)?)
    }
}
impl From<rudbkit_jdbc::Error> for Error {
    fn from(error: rudbkit_jdbc::Error) -> Self {
        match error {
            rudbkit_jdbc::Error::JvmStart(message) => Self::JvmStart(message),
            rudbkit_jdbc::Error::Jni(message) => Self::Jni(message),
            rudbkit_jdbc::Error::Protocol(message) => Self::Protocol(message),
        }
    }
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
}
