//! Build script for `rudbman-jdbc`.
//!
//! Its only job is to make sure the Java bridge JAR exists and to hand its
//! location to the crate as the compile-time fallback used by
//! [`default_bridge_jar`](../fn.default_bridge_jar.html).
//!
//! **This script never runs Gradle.** Building the JAR takes a Java toolchain
//! and the better part of a minute, and wiring that into `cargo build` would
//! mean a JVM start-up for every one-line change to a Rust file. The JAR is a
//! separate artefact with its own build command, and the only thing Cargo needs
//! to know is whether it is there.

use std::path::{Path, PathBuf};

fn main() {
    println!("cargo::rerun-if-env-changed=RUDBMAN_BRIDGE_JAR");

    let jar = match std::env::var_os("RUDBMAN_BRIDGE_JAR") {
        Some(path) => PathBuf::from(path),
        None => workspace_jar(),
    };

    // Rebuilding when the JAR changes keeps `RUDBMAN_BRIDGE_JAR` (below) from
    // pointing at a file that has since been deleted.
    println!("cargo::rerun-if-changed={}", jar.display());

    if !jar.is_file() {
        panic!(
            "the Java bridge JAR is missing: {}\n\
             \n\
             Build it with:\n\
             \n    cd bridge && ./gradlew jar\n\
             \n\
             or point RUDBMAN_BRIDGE_JAR at an existing rudbman-bridge.jar.\n\
             This build script deliberately does not invoke Gradle itself.",
            jar.display()
        );
    }

    println!("cargo::rustc-env=RUDBMAN_BRIDGE_JAR={}", jar.display());
}

/// `<workspace>/bridge/build/libs/rudbman-bridge.jar`, where Gradle puts it.
fn workspace_jar() -> PathBuf {
    let manifest = PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").expect("set by cargo"));
    let workspace = manifest
        .parent() // crates/
        .and_then(Path::parent) // workspace root
        .expect("the crate lives two levels below the workspace root");
    workspace.join("bridge/build/libs/rudbman-bridge.jar")
}
