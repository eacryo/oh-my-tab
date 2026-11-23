//! SHA-256 of the selected files, for the Finder menu's digest items.
//!
//! Streamed in fixed chunks: a selected file can be gigabytes, and reading it whole would pin the
//! app's memory for the duration. The caller runs this on a worker thread — a network volume can
//! block for seconds per read — and hands the digests back to the main thread.

use std::fs::OpenOptions;
use std::io::{self, Read};
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;

use sha2::{Digest, Sha256};

use crate::log_info;

/// Bytes per read. Large enough that the syscall overhead vanishes on a fast disk, small enough
/// that a stalled read does not hold much memory.
const CHUNK: usize = 1024 * 1024;

/// `O_NONBLOCK` from the platform's `<fcntl.h>` (macOS and the BSDs share the value). Opening a
/// FIFO without it waits for a writer, which is forever when the "writer" is a menu click; with it
/// the open returns at once and the `is_file` check below refuses the entry. A regular file ignores
/// the flag.
const O_NONBLOCK: i32 = 0x0004;

/// The lowercase hex SHA-256 of the file at `path`.
///
/// Opens non-blocking, then rejects anything that is not a regular file *after* opening: the
/// selection could have been replaced between the menu being built and the click arriving, and a
/// FIFO swapped into the path would otherwise block the hashing thread forever. Following a
/// symlink is unchanged (`open` resolves it, and the opened descriptor is what is checked).
pub(crate) fn sha256_file(path: &Path) -> io::Result<String> {
    let mut file = OpenOptions::new()
        .read(true)
        .custom_flags(O_NONBLOCK)
        .open(path)?;
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

/// The digests of `paths`, in the order given, plus how many entries were not hashed.
///
/// An entry that cannot be read — a folder, a special file, a volume that went away — is skipped
/// and counted; the rest still produce their digests. A batch that failed as a whole would leave
/// the user with nothing to verify for the files that were fine.
pub(crate) struct Batched {
    /// `(path, digest)` in the order the items were selected.
    pub(crate) entries: Vec<(String, String)>,
    /// Entries that were skipped: not a regular file, or not readable.
    pub(crate) skipped: usize,
}

pub(crate) fn sha256_paths(paths: &[String]) -> Batched {
    let mut entries = Vec::with_capacity(paths.len());
    let mut skipped = 0;
    for path in paths {
        match sha256_file(Path::new(path)) {
            Ok(digest) => entries.push((path.clone(), digest)),
            Err(error) => {
                skipped += 1;
                // The error, never the path: the log is read by whoever reports a bug from it.
                log_info!("[finder-menu] skipped an entry: {error}");
            }
        }
    }
    Batched { entries, skipped }
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

    /// A batch keeps the selection order and counts what it could not hash, so the user's
    /// `digest  name` lines still line up with the files that produced them.
    #[test]
    fn a_batch_keeps_its_order_and_counts_the_skipped_entries() {
        let first = TempFile::new("batch-first", b"one");
        let second = TempFile::new("batch-second", b"two");
        let missing = std::env::temp_dir().join("oh-my-tab-definitely-missing-batch-file");
        let paths = vec![
            first.path().to_string_lossy().into_owned(),
            std::env::temp_dir().to_string_lossy().into_owned(),
            second.path().to_string_lossy().into_owned(),
            missing.to_string_lossy().into_owned(),
        ];
        let batched = super::sha256_paths(&paths);
        assert_eq!(batched.skipped, 2, "a folder and a missing path");
        assert_eq!(batched.entries.len(), 2);
        assert_eq!(batched.entries[0].0, paths[0]);
        assert_eq!(batched.entries[1].0, paths[2]);
        assert_ne!(batched.entries[0].1, batched.entries[1].1);
    }

    /// The open used to block on a FIFO — `open(2)` waits for a writer — which would have pinned
    /// the hashing thread forever on a selection that was replaced after the menu was built. The
    /// result must come back without a writer ever appearing, which is what the bounded receive
    /// asserts: a regression hangs this test rather than passing it.
    #[test]
    fn a_fifo_is_refused_without_waiting_for_a_writer() {
        let path = std::env::temp_dir().join(format!(
            "oh-my-tab-sha256-fifo-{}-{}",
            std::process::id(),
            "pipe"
        ));
        let _ = std::fs::remove_file(&path);
        let status = std::process::Command::new("mkfifo")
            .arg(&path)
            .status()
            .expect("mkfifo is available");
        assert!(status.success(), "could not create the FIFO");

        let (sender, receiver) = std::sync::mpsc::channel();
        let moved = path.clone();
        std::thread::spawn(move || {
            let _ = sender.send(sha256_file(&moved).is_err());
        });
        let refused = receiver
            .recv_timeout(std::time::Duration::from_secs(5))
            .expect("opening a FIFO must not wait for a writer");
        assert!(refused, "a FIFO is not a regular file");
        let _ = std::fs::remove_file(&path);
    }

    /// Following a symlink stayed the behavior: the check is on the opened descriptor, so a link to
    /// a regular file hashes to the target's digest.
    #[test]
    fn a_symlink_to_a_regular_file_is_still_hashed() {
        let target = TempFile::new("symlink-target", b"linked");
        let link = std::env::temp_dir().join(format!(
            "oh-my-tab-sha256-link-{}-{}",
            std::process::id(),
            "symlink"
        ));
        let _ = std::fs::remove_file(&link);
        std::os::unix::fs::symlink(target.path(), &link).expect("create the symlink");
        assert_eq!(
            sha256_file(&link).expect("hash through the link"),
            sha256_file(target.path()).expect("hash the target")
        );
        let _ = std::fs::remove_file(&link);
    }

    #[test]
    fn hex_is_lowercase_and_two_digits_per_byte() {
        assert_eq!(to_hex(&[0x00, 0xff, 0x1a]), "00ff1a");
        assert_eq!(to_hex(&[]), "");
    }
}
