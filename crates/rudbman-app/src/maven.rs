//! Downloading a JDBC driver JAR from Maven Central.
//!
//! A [`DriverDef`](rudbman_core::DriverDef) carries a `group:artifact:version`
//! coordinate rather than a JAR, because none of the drivers rudbman knows about
//! are redistributable. This module turns that coordinate into the two URLs
//! Maven Central publishes — the artefact and its `.sha1` — fetches both, checks
//! one against the other, and lands the result in
//! [`drivers_dir`](rudbman_core::drivers_dir).
//!
//! # Everything here blocks
//!
//! [`download`] is called from a background thread and reports through the
//! callback it is given. Nothing in this module knows gpui, which is what lets
//! the URL assembly and the checksum be tested without a window.
//!
//! # Why the checksum is not optional
//!
//! A truncated download is a JAR whose class loader fails halfway through
//! resolving a driver, and the error that reaches the user then names a missing
//! class rather than a broken file. Central publishes a `.sha1` beside every
//! artefact; comparing against it turns that into "the download was corrupted",
//! which is a thing the user can act on by pressing the button again.
//!
//! SHA-1 is what Central offers for every artefact ever published, so it is what
//! is checked. It is used here as an integrity check against a truncated or
//! garbled transfer, not as a defence against a forged artefact — TLS to
//! `repo1.maven.org` is what covers that.

use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

/// Base URL of the Maven Central repository.
///
/// Straight out of jdbgen's `MavenREST`: the artefact path under this host is
/// `group/with/slashes/artifact/version/artifact-version.jar`, and the checksum
/// is the same path with `.sha1` appended.
const CENTRAL: &str = "https://repo1.maven.org/maven2";

/// How long to wait for the first byte of a response.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(20);

/// Size of one read from the response body.
///
/// Also the granularity of the progress report and of the cancellation check: a
/// download is abandoned within one chunk of the button being pressed.
const CHUNK: usize = 64 * 1024;

/// Largest artefact this will accept.
///
/// Oracle's `ojdbc11` is about 7 MB and is the biggest driver in the built-in
/// list by a wide margin; the cap is a guard against a coordinate that resolves
/// to something else entirely, not a real limit.
const MAX_BYTES: u64 = 256 * 1024 * 1024;

/// A parsed `group:artifact:version` coordinate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Coordinate {
    /// Group id, e.g. `org.postgresql`.
    pub group: String,
    /// Artefact id, e.g. `postgresql`.
    pub artifact: String,
    /// Version, e.g. `42.7.4`.
    pub version: String,
}

impl Coordinate {
    /// Parses `group:artifact:version`.
    ///
    /// Returns `None` for anything that is not exactly three non-empty parts:
    /// `drivers.json` is hand-editable, and a coordinate with a missing version
    /// would otherwise be assembled into a URL that 404s with no hint as to why.
    pub fn parse(text: &str) -> Option<Self> {
        let mut parts = text.trim().split(':');
        let group = parts.next()?.trim();
        let artifact = parts.next()?.trim();
        let version = parts.next()?.trim();
        if parts.next().is_some()
            || group.is_empty()
            || artifact.is_empty()
            || version.is_empty()
            // A path separator in a coordinate would escape the repository root.
            || [group, artifact, version]
                .iter()
                .any(|part| part.contains('/') || part.contains('\\') || part.contains(".."))
        {
            return None;
        }
        Some(Coordinate {
            group: group.to_string(),
            artifact: artifact.to_string(),
            version: version.to_string(),
        })
    }

    /// The file name the artefact is saved under: `artifact-version.jar`.
    pub fn file_name(&self) -> String {
        format!("{}-{}.jar", self.artifact, self.version)
    }

    /// The artefact's URL on Maven Central.
    pub fn jar_url(&self) -> String {
        format!(
            "{CENTRAL}/{}/{}/{}/{}",
            self.group.replace('.', "/"),
            self.artifact,
            self.version,
            self.file_name()
        )
    }

    /// The URL of the artefact's SHA-1 checksum.
    pub fn sha1_url(&self) -> String {
        format!("{}.sha1", self.jar_url())
    }
}

/// Why a download did not produce a JAR.
///
/// Split by what the user can do about it: a coordinate they can fix, a
/// repository that does not have it, a network that is not there, a checksum
/// that says the bytes are wrong, and a disk that would not take the file.
#[derive(Debug)]
pub enum DownloadError {
    /// Maven Central answered, but not with the artefact.
    NotFound(String),
    /// The transfer failed, or never started.
    Network(String),
    /// The bytes that arrived do not hash to what Central says they should.
    Checksum {
        /// What Central published.
        expected: String,
        /// What arrived.
        actual: String,
    },
    /// The file could not be written.
    Io(String),
    /// The user pressed cancel.
    Cancelled,
}

/// How far a download has got.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Progress {
    /// Bytes received so far.
    pub received: u64,
    /// Total size, when the server declared one.
    pub total: Option<u64>,
}

impl Progress {
    /// Completion as a fraction, or `None` while the total is unknown.
    pub fn fraction(&self) -> Option<f32> {
        let total = self.total.filter(|total| *total > 0)?;
        Some((self.received as f32 / total as f32).clamp(0., 1.))
    }
}

/// A flag the UI raises to abandon a download in flight.
///
/// Checked once per chunk, so a cancel takes effect within one read rather than
/// at the end of the transfer.
#[derive(Clone, Debug, Default)]
pub struct Cancel(Arc<AtomicBool>);

impl Cancel {
    /// A flag that has not been raised.
    pub fn new() -> Self {
        Self::default()
    }

    /// Raises the flag.
    pub fn cancel(&self) {
        self.0.store(true, Ordering::Relaxed);
    }

    /// Whether the flag has been raised.
    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Relaxed)
    }
}

/// Fetches `coordinate` into `directory` and returns the path it landed at.
///
/// `on_progress` is called once per chunk, on the calling thread.
///
/// The download goes to a temporary sibling first and is renamed over the
/// destination only after the checksum matches, so a failed or cancelled
/// attempt can never leave a half-written JAR where a driver would load it
/// from. An artefact that is already there is not re-fetched: the file name
/// carries the version, so the only way to get a different JAR is to ask for a
/// different coordinate.
pub fn download(
    coordinate: &Coordinate,
    directory: &Path,
    cancel: &Cancel,
    mut on_progress: impl FnMut(Progress),
) -> Result<PathBuf, DownloadError> {
    let destination = directory.join(coordinate.file_name());
    if destination.is_file() {
        return Ok(destination);
    }
    fs::create_dir_all(directory).map_err(|error| DownloadError::Io(error.to_string()))?;

    // The checksum first: it is a few dozen bytes, and a coordinate that does
    // not exist is worth finding out about before megabytes have moved.
    let expected = fetch_sha1(&coordinate.sha1_url())?;
    let bytes = fetch_jar(&coordinate.jar_url(), cancel, &mut on_progress)?;

    let actual = sha1_hex(&bytes);
    if actual != expected {
        return Err(DownloadError::Checksum { expected, actual });
    }

    // Named after the artefact rather than randomly, so a crash mid-download
    // leaves something a user can recognise and delete.
    let temporary = directory.join(format!("{}.part", coordinate.file_name()));
    fs::write(&temporary, &bytes).map_err(|error| DownloadError::Io(error.to_string()))?;
    fs::rename(&temporary, &destination).map_err(|error| {
        let _ = fs::remove_file(&temporary);
        DownloadError::Io(error.to_string())
    })?;
    Ok(destination)
}

/// Reads the `.sha1` sidecar.
///
/// Central writes it as the hex digest, sometimes followed by the file name the
/// way `sha1sum` does; only the first token is the digest.
fn fetch_sha1(url: &str) -> Result<String, DownloadError> {
    let mut response = request(url)?;
    let text = response
        .body_mut()
        .read_to_string()
        .map_err(|error| DownloadError::Network(error.to_string()))?;
    let digest = text.split_whitespace().next().unwrap_or_default();
    if digest.len() != 40 || !digest.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err(DownloadError::Network(format!(
            "the checksum published at {url} is not a SHA-1 digest"
        )));
    }
    Ok(digest.to_ascii_lowercase())
}

/// Streams the artefact, reporting progress and honouring the cancel flag.
fn fetch_jar(
    url: &str,
    cancel: &Cancel,
    on_progress: &mut impl FnMut(Progress),
) -> Result<Vec<u8>, DownloadError> {
    let mut response = request(url)?;
    let total = response
        .headers()
        .get("content-length")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|total| *total <= MAX_BYTES);

    let mut body = response.body_mut().as_reader();
    let mut bytes: Vec<u8> = Vec::with_capacity(total.unwrap_or(0) as usize);
    let mut chunk = vec![0u8; CHUNK];
    on_progress(Progress { received: 0, total });

    loop {
        if cancel.is_cancelled() {
            return Err(DownloadError::Cancelled);
        }
        let read = body
            .read(&mut chunk)
            .map_err(|error| DownloadError::Network(error.to_string()))?;
        if read == 0 {
            break;
        }
        bytes.extend_from_slice(&chunk[..read]);
        if bytes.len() as u64 > MAX_BYTES {
            return Err(DownloadError::Network(format!(
                "{url} is larger than the {} MB rudbman will download",
                MAX_BYTES / (1024 * 1024)
            )));
        }
        on_progress(Progress {
            received: bytes.len() as u64,
            total,
        });
    }
    Ok(bytes)
}

/// One GET, with the status codes that matter told apart.
fn request(url: &str) -> Result<ureq::http::Response<ureq::Body>, DownloadError> {
    let agent = ureq::Agent::config_builder()
        .timeout_connect(Some(CONNECT_TIMEOUT))
        // Central redirects `repo1` requests within its own CDN.
        .max_redirects(5)
        .build()
        .new_agent();

    match agent.get(url).call() {
        Ok(response) => Ok(response),
        Err(ureq::Error::StatusCode(code)) if code == 404 || code == 410 => {
            Err(DownloadError::NotFound(url.to_string()))
        }
        Err(ureq::Error::StatusCode(code)) => Err(DownloadError::Network(format!(
            "{url} answered with HTTP {code}"
        ))),
        Err(error) => Err(DownloadError::Network(error.to_string())),
    }
}

/// SHA-1 of `bytes`, as lower-case hex.
///
/// Hand-rolled rather than pulled in: this is the only hash in the workspace,
/// FIPS 180-4 spells it out in fifty lines, and the reference vectors below are
/// what keep it honest. The same trade `rudbman_jdbc::spec` makes for base64.
pub fn sha1_hex(bytes: &[u8]) -> String {
    let mut state: [u32; 5] = [
        0x6745_2301,
        0xefcd_ab89,
        0x98ba_dcfe,
        0x1032_5476,
        0xc3d2_e1f0,
    ];

    // The padded message: the bytes, a single 1 bit, zeroes, and the original
    // length in bits as a big-endian u64.
    let mut padded = bytes.to_vec();
    let bit_length = (bytes.len() as u64).wrapping_mul(8);
    padded.push(0x80);
    while padded.len() % 64 != 56 {
        padded.push(0);
    }
    padded.extend_from_slice(&bit_length.to_be_bytes());

    for block in padded.chunks_exact(64) {
        let mut words = [0u32; 80];
        for (index, word) in block.chunks_exact(4).enumerate() {
            words[index] = u32::from_be_bytes([word[0], word[1], word[2], word[3]]);
        }
        for index in 16..80 {
            words[index] =
                (words[index - 3] ^ words[index - 8] ^ words[index - 14] ^ words[index - 16])
                    .rotate_left(1);
        }

        let [mut a, mut b, mut c, mut d, mut e] = state;
        for (index, word) in words.iter().enumerate() {
            let (f, k) = match index {
                0..=19 => ((b & c) | ((!b) & d), 0x5a82_7999),
                20..=39 => (b ^ c ^ d, 0x6ed9_eba1),
                40..=59 => ((b & c) | (b & d) | (c & d), 0x8f1b_bcdc),
                _ => (b ^ c ^ d, 0xca62_c1d6),
            };
            let temp = a
                .rotate_left(5)
                .wrapping_add(f)
                .wrapping_add(e)
                .wrapping_add(k)
                .wrapping_add(*word);
            e = d;
            d = c;
            c = b.rotate_left(30);
            b = a;
            a = temp;
        }
        for (slot, value) in state.iter_mut().zip([a, b, c, d, e]) {
            *slot = slot.wrapping_add(value);
        }
    }

    state.iter().map(|word| format!("{word:08x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_coordinate_becomes_the_two_urls_central_publishes() {
        let coordinate = Coordinate::parse("org.postgresql:postgresql:42.7.4").expect("parses");
        assert_eq!(coordinate.file_name(), "postgresql-42.7.4.jar");
        assert_eq!(
            coordinate.jar_url(),
            "https://repo1.maven.org/maven2/org/postgresql/postgresql/42.7.4/postgresql-42.7.4.jar"
        );
        assert_eq!(
            coordinate.sha1_url(),
            format!("{}.sha1", coordinate.jar_url())
        );

        // Every dot of the group id becomes a directory, however many there are.
        let oracle =
            Coordinate::parse("com.oracle.database.jdbc:ojdbc11:23.6.0.24.10").expect("parses");
        assert_eq!(
            oracle.jar_url(),
            "https://repo1.maven.org/maven2/com/oracle/database/jdbc/ojdbc11/\
             23.6.0.24.10/ojdbc11-23.6.0.24.10.jar",
        );
    }

    /// The whole path, against the real Maven Central.
    ///
    /// Ignored by default because it needs the network and moves four megabytes;
    /// run it with `cargo test -p rudbman-app -- --ignored` when the downloader
    /// itself is what changed. The checksum is what makes it worth having: it
    /// proves the streamed bytes and the published digest agree, which no
    /// offline test can.
    #[test]
    #[ignore = "reaches Maven Central over the network"]
    fn a_real_artefact_downloads_and_matches_its_published_checksum() {
        let directory = tempfile::tempdir().expect("tempdir");
        let coordinate = Coordinate::parse("com.h2database:h2:2.3.232").expect("parses");
        let cancel = Cancel::new();
        let mut seen = Vec::new();

        let path = download(&coordinate, directory.path(), &cancel, |progress| {
            seen.push(progress);
        })
        .expect("H2 is on Maven Central");

        assert_eq!(path.file_name().unwrap(), "h2-2.3.232.jar");
        let bytes = std::fs::read(&path).expect("read back");
        assert!(bytes.len() > 1_000_000, "{} bytes", bytes.len());
        // A JAR is a zip.
        assert_eq!(&bytes[..2], b"PK");
        // Progress was reported, and monotonically.
        assert!(seen.len() > 1);
        assert!(
            seen.windows(2)
                .all(|pair| pair[0].received <= pair[1].received)
        );
        assert_eq!(
            seen.last().map(|last| last.received),
            Some(bytes.len() as u64)
        );
        // A second call finds the file already there and does not refetch.
        assert_eq!(
            download(&coordinate, directory.path(), &cancel, |_| {
                panic!("an artefact already on disk must not be fetched again")
            })
            .expect("already there"),
            path
        );
        // No `.part` file survives a successful download.
        assert!(!directory.path().join("h2-2.3.232.jar.part").exists());
    }

    /// A coordinate that resolves to nothing is a 404, and it has to be said in
    /// those terms rather than as a generic network failure.
    #[test]
    #[ignore = "reaches Maven Central over the network"]
    fn a_coordinate_central_does_not_have_is_reported_as_not_found() {
        let directory = tempfile::tempdir().expect("tempdir");
        let coordinate =
            Coordinate::parse("com.h2database:h2:0.0.0-does-not-exist").expect("parses");
        let error = download(&coordinate, directory.path(), &Cancel::new(), |_| {})
            .expect_err("nothing is published there");
        assert!(matches!(error, DownloadError::NotFound(_)), "{error:?}");
    }

    #[test]
    fn a_coordinate_that_is_not_three_parts_is_refused() {
        for text in [
            "",
            "org.postgresql",
            "org.postgresql:postgresql",
            "org.postgresql:postgresql:42.7.4:jar",
            "org.postgresql::42.7.4",
            " : : ",
        ] {
            assert!(Coordinate::parse(text).is_none(), "{text:?}");
        }
        // Whitespace around the parts is a hand-edited file, not an error.
        assert_eq!(
            Coordinate::parse(" com.h2database : h2 : 2.3.232 "),
            Coordinate::parse("com.h2database:h2:2.3.232")
        );
    }

    #[test]
    fn a_coordinate_cannot_escape_the_repository_root() {
        // `drivers.json` is hand-editable and the coordinate is pasted straight
        // into a URL *and* into a file name.
        for text in [
            "../../etc:passwd:1",
            "com.example:../evil:1",
            "com.example:artifact:../1",
            "com/example:artifact:1",
        ] {
            assert!(Coordinate::parse(text).is_none(), "{text:?}");
        }
    }

    #[test]
    fn sha1_matches_the_reference_vectors() {
        // FIPS 180-2 appendix A, plus the empty string and a message long
        // enough to need a second block — which is where a padding mistake
        // hides.
        assert_eq!(sha1_hex(b""), "da39a3ee5e6b4b0d3255bfef95601890afd80709");
        assert_eq!(sha1_hex(b"abc"), "a9993e364706816aba3e25717850c26c9cd0d89d");
        assert_eq!(
            sha1_hex(b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq"),
            "84983e441c3bd26ebaae4aa1f95129e5e54670f1"
        );
        assert_eq!(
            sha1_hex(&b"a".repeat(1_000_000)),
            "34aa973cd4c4daa4f61eeb2bdbad27316534016f"
        );
        // 55, 56 and 64 bytes: the lengths either side of the boundary where
        // the length field no longer fits beside the message, and one exactly
        // one block long. A padding mistake shows up here and nowhere else.
        assert_eq!(
            sha1_hex(&b"x".repeat(55)),
            "cef734ba81a024479e09eb5a75b6ddae62e6abf1"
        );
        assert_eq!(
            sha1_hex(&b"x".repeat(56)),
            "901305367c259952f4e7af8323f480d59f81335b"
        );
        assert_eq!(
            sha1_hex(&b"x".repeat(64)),
            "bb2fa3ee7afb9f54c6dfb5d021f14b1ffe40c163"
        );
    }

    #[test]
    fn progress_is_a_fraction_only_once_the_total_is_known() {
        assert_eq!(
            Progress {
                received: 50,
                total: Some(200)
            }
            .fraction(),
            Some(0.25)
        );
        assert_eq!(
            Progress {
                received: 50,
                total: None
            }
            .fraction(),
            None
        );
        // A server that declares zero would otherwise divide by it.
        assert_eq!(
            Progress {
                received: 0,
                total: Some(0)
            }
            .fraction(),
            None
        );
    }

    #[test]
    fn a_raised_cancel_flag_is_seen_through_every_clone() {
        let cancel = Cancel::new();
        let copy = cancel.clone();
        assert!(!copy.is_cancelled());
        cancel.cancel();
        assert!(copy.is_cancelled());
    }
}
