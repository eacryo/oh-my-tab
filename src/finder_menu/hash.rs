//! SHA-256 of a selected file, for the Finder menu's "Copy SHA-256" item.
//!
//! Streamed in fixed chunks: a selected file can be gigabytes, and reading it whole would pin the
//! app's memory for the duration. The caller runs this on a worker thread — a network volume can
//! block for seconds per read — and hands the digest back to the main thread.

use std::fs::File;
use std::io::{self, Read};
use std::path::Path;

use sha2::{Digest, Sha256};

use crate::log_info;

/// Bytes per read. Large enough that the syscall overhead vanishes on a fast disk, small enough
/// that a stalled read does not hold much memory.
const CHUNK: usize = 1024 * 1024;

/// The lowercase hex SHA-256 of the file at `path`.
///
/// Opens with `File::open`, which follows a symlink and fails on a directory, so the caller's
/// regular-file check and this open agree on what is being read. The file is stat-ed after opening
/// and rejected unless it is still a regular file: the selection could have been replaced between
/// the menu being built and the click arriving, and a FIFO here would block the read forever.
pub(crate) fn sha256_file(path: &Path) -> io::Result<String> {
    let mut file = File::open(path)?;
    let metadata = file.metadata()?;
    if !metadata.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "the selection is not a regular file",
        ));
    }
    let mut hasher = Sha256::new();
    let mut buffer = vec![0u8; CHUNK];
    let mut total: u64 = 0;
    loop {
        match file.read(&mut buffer) {
            Ok(0) => break,
            Ok(read) => {
                hasher.update(&buffer[..read]);
                total += read as u64;
            }
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error),
        }
    }
    // The size only, never the path: the log is read by whoever reports a bug from it.
    log_info!("[finder-menu] hashed {} bytes", total);
    Ok(to_hex(&hasher.finalize()))
}

fn to_hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(DIGITS[(byte >> 4) as usize] as char);
        out.push(DIGITS[(byte & 0x0f) as usize] as char);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::{sha256_file, to_hex, CHUNK};
    use std::io::Write;

    /// A temp file that removes itself, so a failing assertion cannot leave the tree dirty.
    struct TempFile(std::path::PathBuf);

    impl TempFile {
        fn new(name: &str, bytes: &[u8]) -> Self {
            let mut path = std::env::temp_dir();
            path.push(format!(
                "oh-my-tab-sha256-{}-{}.bin",
                std::process::id(),
                name
            ));
            let mut file = std::fs::File::create(&path).expect("create temp file");
            file.write_all(bytes).expect("write temp file");
            file.sync_all().expect("flush temp file");
            Self(path)
        }

        fn path(&self) -> &std::path::Path {
            &self.0
        }
    }

    impl Drop for TempFile {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }

    /// The published vector for the empty input, and the standard one-byte one: a hash that is
    /// wrong for these is wrong everywhere and would still look plausible.
    #[test]
    fn known_vectors_match() {
        let empty = TempFile::new("empty", b"");
        assert_eq!(
            sha256_file(empty.path()).expect("hash empty"),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        let abc = TempFile::new("abc", b"abc");
        assert_eq!(
            sha256_file(abc.path()).expect("hash abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    /// A hash that only ever consumed one chunk would pass the vectors above and be wrong for every
    /// real file. This crosses the chunk boundary twice.
    #[test]
    fn content_spanning_several_chunks_hashes_to_one_digest() {
        let data: Vec<u8> = (0..(CHUNK * 2 + 7)).map(|i| (i % 251) as u8).collect();
        let file = TempFile::new("chunked", &data);
        let digest = sha256_file(file.path()).expect("hash chunked");
        assert_eq!(digest.len(), 64);
        // The same bytes hashed by the crate directly, which is the independent answer.
        use sha2::{Digest, Sha256};
        let mut hasher = Sha256::new();
        hasher.update(&data);
        assert_eq!(digest, to_hex(&hasher.finalize()));
    }

    /// A directory must be refused rather than read, and a missing path must report the error
    /// instead of returning an empty digest.
    #[test]
    fn non_regular_and_missing_paths_are_refused() {
        let directory = std::env::temp_dir();
        assert!(sha256_file(&directory).is_err());
        let missing = std::env::temp_dir().join("oh-my-tab-definitely-missing-file");
        assert!(sha256_file(&missing).is_err());
    }

    #[test]
    fn hex_is_lowercase_and_two_digits_per_byte() {
        assert_eq!(to_hex(&[0x00, 0xff, 0x1a]), "00ff1a");
        assert_eq!(to_hex(&[]), "");
    }
}
