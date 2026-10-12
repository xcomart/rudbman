//! Immutable, file-backed LOB values shared by result grids and bind parameters.
//!
//! A snapshot outlives its JDBC cursor, takes bounded memory, and is removed
//! when its last owner goes away. Character data is always strict UTF-8.

use std::fs::File;
use std::io::{self, Read, Write};
use std::path::Path;
use std::sync::Arc;

use tempfile::NamedTempFile;

const TEXT_PREVIEW_CHARS: usize = 256;
const BINARY_PREVIEW_BYTES: usize = 64;

/// An owned BLOB or UTF-8 CLOB snapshot. Cloning keeps the same file alive.
#[derive(Clone, Debug)]
pub struct LobFile {
    file: Arc<NamedTempFile>,
    binary: bool,
    bytes: u64,
    units: u64,
    preview: Arc<str>,
}

impl PartialEq for LobFile {
    fn eq(&self, other: &Self) -> bool {
        self.file.path() == other.file.path()
    }
}

impl Eq for LobFile {}

impl LobFile {
    /// Copies a stream into a private snapshot, validating CLOB text as UTF-8.
    pub fn from_reader(mut input: impl Read, binary: bool) -> io::Result<Self> {
        let mut file = NamedTempFile::new()?;
        let mut buffer = [0u8; 64 * 1024];
        let mut pending = Vec::with_capacity(buffer.len() + 3);
        let mut bytes = 0u64;
        let mut units = 0u64;
        let preview_limit = if binary {
            BINARY_PREVIEW_BYTES
        } else {
            TEXT_PREVIEW_CHARS * 4
        };
        let mut prefix = Vec::with_capacity(preview_limit);
        loop {
            let read = input.read(&mut buffer)?;
            if read == 0 {
                break;
            }
            file.write_all(&buffer[..read])?;
            bytes += read as u64;
            prefix.extend_from_slice(&buffer[..read.min(preview_limit - prefix.len())]);
            if !binary {
                pending.extend_from_slice(&buffer[..read]);
                let valid = match std::str::from_utf8(&pending) {
                    Ok(text) => text.len(),
                    Err(error) if error.error_len().is_none() => error.valid_up_to(),
                    Err(error) => return Err(io::Error::new(io::ErrorKind::InvalidData, error)),
                };
                // valid is a verified UTF-8 boundary, including when a scalar
                // straddles two I/O buffers. Match JDBC Clob.length's UTF-16 units.
                let text = std::str::from_utf8(&pending[..valid]).map_err(io::Error::other)?;
                units += text.chars().map(|ch| ch.len_utf16() as u64).sum::<u64>();
                pending.drain(..valid);
            }
        }
        if !pending.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "incomplete UTF-8 character",
            ));
        }
        file.flush()?;
        let mut preview = if binary {
            use std::fmt::Write as _;
            let mut hex = String::with_capacity(prefix.len() * 2 + 3);
            for byte in &prefix {
                write!(&mut hex, "{byte:02X}").expect("writing to a String");
            }
            hex
        } else {
            // The complete stream was validated above. Only the bounded
            // prefix may end inside a scalar; never show a replacement glyph.
            let end = match std::str::from_utf8(&prefix) {
                Ok(_) => prefix.len(),
                Err(error) => error.valid_up_to(),
            };
            std::str::from_utf8(&prefix[..end])
                .map_err(io::Error::other)?
                .chars()
                .take(TEXT_PREVIEW_CHARS)
                .collect::<String>()
        };
        let shown_bytes = if binary { prefix.len() } else { preview.len() };
        if bytes > shown_bytes as u64 {
            preview.push('…');
        }
        Ok(Self {
            file: Arc::new(file),
            binary,
            bytes,
            units: if binary { bytes } else { units },
            preview: preview.into(),
        })
    }

    /// Snapshots a selected file, so changing it later cannot change a staged edit.
    pub fn from_path(path: &Path, binary: bool) -> io::Result<Self> {
        Self::from_reader(File::open(path)?, binary)
    }

    /// Snapshots editor contents (raw bytes for BLOB, UTF-8 for CLOB).
    pub fn from_bytes(bytes: &[u8], binary: bool) -> io::Result<Self> {
        Self::from_reader(bytes, binary)
    }

    /// Whether this value is binary rather than character data.
    pub fn is_binary(&self) -> bool {
        self.binary
    }

    /// Length of the snapshot in bytes (UTF-8 bytes for CLOB).
    pub fn byte_len(&self) -> u64 {
        self.bytes
    }

    /// JDBC length: bytes for BLOB, UTF-16 code units for CLOB.
    pub fn len(&self) -> u64 {
        self.units
    }

    /// Whether the value has zero length. An empty LOB is distinct from SQL NULL.
    pub fn is_empty(&self) -> bool {
        self.bytes == 0
    }

    /// A cached grid preview: up to 256 Unicode scalars, or 64 bytes as hex.
    /// A trailing ellipsis marks omitted content. Access never reads the file.
    pub fn preview(&self) -> &str {
        &self.preview
    }

    /// Path used by the in-process JDBC bridge to bind a streaming parameter.
    pub fn path(&self) -> &Path {
        self.file.path()
    }

    /// Reads at most `limit` bytes for a bounded viewer. May end inside a UTF-8 scalar.
    pub fn prefix(&self, limit: usize) -> io::Result<Vec<u8>> {
        let mut bytes = Vec::with_capacity(self.bytes.min(limit as u64) as usize);
        File::open(self.path())?
            .take(limit as u64)
            .read_to_end(&mut bytes)?;
        Ok(bytes)
    }

    /// Saves the complete value atomically, leaving an existing destination
    /// intact if reading or writing fails. CLOB files are UTF-8 without a BOM.
    pub fn export_to(&self, destination: &Path) -> io::Result<()> {
        let parent = destination
            .parent()
            .filter(|path| !path.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        let mut output = NamedTempFile::new_in(parent)?;
        io::copy(&mut File::open(self.path())?, &mut output)?;
        output.flush()?;
        output.as_file().sync_all()?;
        output.persist(destination).map_err(|error| error.error)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_previews_are_bounded_without_splitting_unicode_or_changing_the_body() {
        for text in ["", "short\ntext", &"🙂".repeat(256)] {
            let file = LobFile::from_bytes(text.as_bytes(), false).unwrap();
            assert_eq!(file.preview(), text);
        }
        let text = format!("{}tail", "한🙂".repeat(128));
        let file = LobFile::from_bytes(text.as_bytes(), false).unwrap();
        assert_eq!(file.preview(), format!("{}…", "한🙂".repeat(128)));
        assert_eq!(file.prefix(text.len()).unwrap(), text.as_bytes());
        assert_eq!(file.byte_len(), text.len() as u64);
        assert_eq!(file.clone().preview(), file.preview());
        // The 1024-byte prefix itself ends inside the next emoji here.
        let text = format!("x{}", "🙂".repeat(300));
        let file = LobFile::from_bytes(text.as_bytes(), false).unwrap();
        assert_eq!(file.preview(), format!("x{}…", "🙂".repeat(255)));
    }

    #[test]
    fn previews_handle_utf8_split_across_small_reads() {
        struct SmallReads<'a>(&'a [u8]);
        impl Read for SmallReads<'_> {
            fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
                let length = buffer.len().min(3);
                self.0.read(&mut buffer[..length])
            }
        }
        let text = format!("{}🙂suffix", "x".repeat(255));
        let file = LobFile::from_reader(SmallReads(text.as_bytes()), false).unwrap();
        assert_eq!(file.preview(), format!("{}🙂…", "x".repeat(255)));
    }

    #[test]
    fn binary_previews_show_hex_and_mark_only_truncated_bodies() {
        assert_eq!(LobFile::from_bytes(&[], true).unwrap().preview(), "");
        assert_eq!(
            LobFile::from_bytes(&[0, 128, 255], true).unwrap().preview(),
            "0080FF"
        );
        let bytes = (0..65).collect::<Vec<u8>>();
        let file = LobFile::from_bytes(&bytes, true).unwrap();
        assert_eq!(
            file.preview(),
            concat!(
                "000102030405060708090A0B0C0D0E0F",
                "101112131415161718191A1B1C1D1E1F",
                "202122232425262728292A2B2C2D2E2F",
                "303132333435363738393A3B3C3D3E3F…",
            )
        );
        assert_eq!(file.prefix(65).unwrap(), bytes);
        assert!(
            !LobFile::from_bytes(&bytes[..64], true)
                .unwrap()
                .preview()
                .ends_with('…')
        );
    }

    #[test]
    fn snapshots_preserve_bytes_validate_utf8_and_remove_their_file() {
        let mut text = "a".repeat(65535);
        text.push_str("한🙂\0끝");
        let value = LobFile::from_bytes(text.as_bytes(), false).unwrap();
        assert_eq!(value.len(), text.encode_utf16().count() as u64);
        assert_eq!(value.prefix(text.len()).unwrap(), text.as_bytes());
        let clone = value.clone();
        let path = value.path().to_path_buf();
        drop(value);
        assert!(path.exists());
        drop(clone);
        assert!(!path.exists());
        assert!(LobFile::from_bytes(&[0xff], false).is_err());
        assert!(LobFile::from_bytes(&[0xe2, 0x82], false).is_err());
        assert_eq!(
            LobFile::from_bytes(&[0xff], true)
                .unwrap()
                .prefix(1)
                .unwrap(),
            [0xff]
        );
    }

    #[test]
    fn imports_are_immutable_and_exports_replace_only_the_selected_file() {
        let directory = tempfile::tempdir().unwrap();
        let input = directory.path().join("input.bin");
        let output = directory.path().join("output.bin");
        std::fs::write(&input, [0, 255, 1]).unwrap();
        let value = LobFile::from_path(&input, true).unwrap();
        std::fs::write(&input, "later").unwrap();
        std::fs::write(&output, "old and longer").unwrap();
        value.export_to(&output).unwrap();
        assert_eq!(std::fs::read(output).unwrap(), [0, 255, 1]);
        assert_eq!(std::fs::read(input).unwrap(), b"later");
        assert!(LobFile::from_bytes(&[], false).unwrap().is_empty());
    }
}
