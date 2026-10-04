//! Clipboard subsystem · persist: history persistence (write, prune, load).

use super::*;
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;

/// The history file format version (bump on structural changes; a higher version is
/// ignored on load and the app starts with an empty history).
pub(super) const HISTORY_VERSION: u32 = 1;

/// The history file wrapper (versioned, for future evolution).
#[derive(Debug, Serialize, Deserialize)]
pub(super) struct HistoryFile {
    version: u32,
    entries: Vec<ClipEntry>,
}

/// A borrow-only view for serialization: avoids `entries.to_vec()` deep-copying the entire
/// history (image previews included) -- the previews are `#[serde(skip)]`, so that copy was
/// pure waste.
#[derive(Serialize)]
struct HistoryFileRef<'a> {
    version: u32,
    entries: &'a [ClipEntry],
}

/// The persisted-history path (same dir as config.toml; test builds use a test dir).
pub(super) fn history_file_path() -> std::path::PathBuf {
    if SMOKE_MODE.load(Ordering::SeqCst) || cfg!(test) {
        // Test/smoke history must share the same temp root as the image cache. It is moved
        // out of HOME because the Codex sandbox restricts writes to $HOME/Library/Caches,
        // and persistence tests create this directory directly.
        return clip_image_cache_dir()
            .join("history")
            .join("clipboard-history.toml");
    }

    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string());
    std::path::PathBuf::from(format!("{}/.config/oh-my-tab/clipboard-history.toml", home))
}

/// Whether the history is cleared when the app quits (read from CONFIG).
pub(crate) fn clear_on_quit_enabled() -> bool {
    CONFIG
        .read()
        .map(|c| c.clipboard.clear_on_quit)
        .unwrap_or(false)
}

/// Serialize the history (pure, unit-tested).
pub(super) fn serialize_history(entries: &[ClipEntry]) -> Option<String> {
    let payload = HistoryFileRef {
        version: HISTORY_VERSION,
        entries,
    };
    toml::to_string(&payload).ok()
}

struct PersistJob {
    generation: u64,
    path: std::path::PathBuf,
    entries: Vec<ClipEntry>,
}

/// One serial worker coalesces consecutive snapshots to the newest one, keeping copy events
/// from blocking the main thread on TOML serialization and filesystem I/O.
static PERSIST_SENDER: OnceLock<Sender<PersistJob>> = OnceLock::new();
static PERSIST_GENERATION: AtomicU64 = AtomicU64::new(0);
static PERSIST_IO_LOCK: Mutex<()> = Mutex::new(());
/// The newest generation the worker has processed (coalesced-away jobs count as
/// processed): tests wait on it so the async writeback settles deterministically.
static PERSIST_DONE_GENERATION: AtomicU64 = AtomicU64::new(0);

fn persist_sender() -> &'static Sender<PersistJob> {
    PERSIST_SENDER.get_or_init(|| {
        let (sender, receiver) = mpsc::channel();
        thread::Builder::new()
            .name("oh-my-tab-clipboard-persist".to_string())
            .spawn(|| persist_worker(receiver))
            .expect("failed to start clipboard persistence worker");
        sender
    })
}

fn persist_worker(receiver: Receiver<PersistJob>) {
    while let Ok(mut job) = receiver.recv() {
        // A burst of copies can queue several full snapshots; keep only the newest to reduce
        // serialization and filesystem work.
        while let Ok(newer) = receiver.try_recv() {
            job = newer;
        }
        let done_generation = job.generation;
        write_history_snapshot(job);
        PERSIST_DONE_GENERATION.store(done_generation, Ordering::Release);
    }
}

/// Test helper: wait until the worker has drained every snapshot up to the current
/// generation. The save_history at the end of load_history is asynchronous; without
/// draining, an older snapshot can land AFTER a test rewrites the history file and
/// clobber it with stale content (the former flake: left:3, right:1).
#[cfg(test)]
pub(super) fn flush_persist_worker_for_tests() {
    let target = PERSIST_GENERATION.load(Ordering::Acquire);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while PERSIST_DONE_GENERATION.load(Ordering::Acquire) < target {
        if std::time::Instant::now() > deadline {
            panic!("clipboard persist worker did not drain within 10s");
        }
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
}

fn write_history_snapshot(job: PersistJob) {
    let Some(text) = serialize_history(&job.entries) else {
        log_info!("Clipboard history save failed: serialize error.");
        return;
    };
    let Some(dir) = job.path.parent() else {
        return;
    };

    // Share the I/O lock with the discard path so clearing the history deletes the file after any
    // in-progress write and prevents an older snapshot from being restored.
    let _io = PERSIST_IO_LOCK.lock().unwrap();
    if PERSIST_GENERATION.load(Ordering::Acquire) != job.generation {
        return;
    }
    if std::fs::create_dir_all(dir).is_err() {
        log_info!("Clipboard history save failed: cannot create dir.");
        return;
    }
    let tmp = dir.join(format!(
        "clipboard-history.toml.tmp{}-{}",
        std::process::id(),
        job.generation
    ));
    let ok = std::fs::write(&tmp, text.as_bytes()).is_ok();
    if ok {
        // Mode 600: owner-only access.
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600));
    }
    // Check the generation again before rename so a snapshot invalidated while preparing the
    // file is discarded instead of replacing newer history.
    let current = PERSIST_GENERATION.load(Ordering::Acquire) == job.generation;
    let ok = ok && current && std::fs::rename(&tmp, &job.path).is_ok();
    if !ok {
        let _ = std::fs::remove_file(&tmp);
        if current {
            log_info!("Clipboard history save failed: write error.");
        }
        return;
    }
    log_debug!(
        "[clip] history saved asynchronously ({} entries, generation={})",
        job.entries.len(),
        job.generation
    );
}

/// Parse the history text: corruption or a version mismatch -> None (the caller treats it
/// as an empty history).
pub(super) fn parse_history(text: &str) -> Option<Vec<ClipEntry>> {
    let file: HistoryFile = toml::from_str(text).ok()?;
    if file.version > HISTORY_VERSION {
        return None;
    }
    Some(file.entries)
}

/// Restore a loaded entry's runtime fields (data_path/preview_png) on load:
/// - data entries: a missing data file (the cache was swept) -> None (the broken entry is
///   dropped); a missing preview is regenerated from the data bytes and re-persisted
/// - file-copy entries: the preview is read back from `{hash}.preview` (when missing,
///   regenerated from the source file and re-persisted; when the source is also gone, the
///   preview stays empty and the row shows the filename); data_path is always empty
///   (pasting goes through the file-url)
/// - text entries: returned as-is
pub(super) fn restore_loaded_entry(entry: ClipEntry) -> Option<ClipEntry> {
    let Some(img) = &entry.image else {
        return Some(entry); // a text entry
    };
    if img.hash == 0 {
        return Some(entry); // a degenerate file entry
    }
    if let Some(path) = &img.source_path {
        // File-copy entries: the preview comes from the persisted {hash}.preview; when
        // missing it is regenerated from the source file.
        let preview = cache_read_preview(img.hash).unwrap_or_else(|| {
            std::fs::read(path)
                .ok()
                .and_then(|d| unsafe { any_image_to_preview_png(&d) })
                .unwrap_or_default()
        });
        if !preview.is_empty() {
            let _ = cache_write_preview(img.hash, &preview);
        }
        return Some(ClipEntry {
            text: entry.text,
            image: Some(ImageEntry {
                uti: img.uti.clone(),
                hash: img.hash,
                data_path: std::path::PathBuf::new(),
                preview_png: Arc::new(preview),
                source_path: Some(path.clone()),
            }),
            pinned: entry.pinned,
            source_app: entry.source_app,
            source_key: entry.source_key,
            copied_at: entry.copied_at,
        });
    }
    let data = cache_read_image(img.hash)?;
    let preview = cache_read_preview(img.hash)
        .unwrap_or_else(|| unsafe { any_image_to_preview_png(&data) }.unwrap_or_default());
    let _ = cache_write_preview(img.hash, &preview);
    Some(ClipEntry {
        text: entry.text,
        image: Some(ImageEntry {
            uti: img.uti.clone(),
            hash: img.hash,
            data_path: clip_image_path(img.hash),
            preview_png: Arc::new(preview),
            source_path: None,
        }),
        pinned: entry.pinned,
        source_app: entry.source_app,
        source_key: entry.source_key,
        copied_at: entry.copied_at,
    })
}

/// Save the current history to disk (atomic temp+rename, mode 600). Plaintext -- the privacy
/// implications are documented in the README, and `clear_on_quit` is the in-app way out.
pub(super) fn save_history() {
    let mut hist = CLIP_HISTORY.lock().unwrap();
    // Expire before writing: the disk file never keeps expired entries (expiry applies
    // to memory and persistence alike).
    expire_entries(&mut hist, now_secs(), ttl_secs());
    let entries = hist.clone();
    drop(hist);
    let generation = PERSIST_GENERATION.fetch_add(1, Ordering::AcqRel) + 1;
    let job = PersistJob {
        generation,
        path: history_file_path(),
        entries,
    };
    if persist_sender().send(job).is_err() {
        log_info!("Clipboard history save failed: persistence worker stopped.");
    }
}

/// Load the persisted history and MERGE it into the in-memory history (reusing the dedup
/// rules; pinned entries join the pinned block, the rest append in file order (old ->
/// new) at the tail, then trim to max_entries). A missing/corrupt/version-mismatched file
/// is logged and treated as an empty history (config-style resilience).
pub(super) fn load_history() {
    let path = history_file_path();
    {
        // Same lock as the writer: a leftover temp file is only swept when no write is in flight.
        let _io = PERSIST_IO_LOCK.lock().unwrap();
        sweep_history_temp_files(&path);
    }
    let Ok(text) = std::fs::read_to_string(&path) else {
        let removed = sweep_current_clip_image_cache();
        if removed > 0 {
            log_debug!("[clip] swept {} orphan image cache files", removed);
        }
        return; // a missing file = first run
    };
    let Some(entries) = parse_history(&text) else {
        log_info!(
            "Clipboard history load failed (corrupt/version mismatch, starting empty): {}",
            path.display()
        );
        let removed = sweep_current_clip_image_cache();
        if removed > 0 {
            log_debug!("[clip] swept {} orphan image cache files", removed);
        }
        return;
    };
    let mut hist = CLIP_HISTORY.lock().unwrap();
    let max = max_entries();
    let mut history_changed = false;
    // Expired entries are skipped outright: they never reach memory (the disk file is
    // cleaned up afterwards by the save_history rewrite).
    let ttl = ttl_secs();
    let now = now_secs();
    for entry in entries {
        if ttl.is_some_and(|ttl| {
            !entry.pinned
                && entry
                    .copied_at
                    .is_some_and(|t| now.saturating_sub(t) >= ttl)
        }) {
            continue;
        }
        // Data entries: a missing data file (cache was swept) drops the broken entry; a
        // missing preview is regenerated from the data bytes.
        let Some(entry) = restore_loaded_entry(entry) else {
            continue;
        };
        // Dedup follows record_image, **split by entry kind**: data entries (source_path
        // is ALWAYS None) dedup by content hash -- comparing every image by source_path
        // used to make data entries dedup against each other (None==None), dropping all
        // but the first on every load (orphan cache files were the evidence). File entries
        // dedup by content hash; degenerate entries (hash=0) by source path.
        let dup = match &entry.image {
            Some(img) if img.source_path.is_some() => hist.iter().any(|e| {
                e.image.as_ref().is_some_and(|i| {
                    i.source_path.is_some()
                        && if img.hash != 0 {
                            i.hash == img.hash
                        } else {
                            i.source_path.as_deref() == img.source_path.as_deref()
                        }
                })
            }),
            Some(img) => hist.iter().any(|e| {
                e.image
                    .as_ref()
                    .is_some_and(|i| i.source_path.is_none() && i.hash == img.hash)
            }),
            None => hist
                .iter()
                .any(|e| e.image.is_none() && e.text == entry.text),
        };
        if dup {
            continue;
        }
        // Pinned entries join the top of the pinned block (newest first); the rest append
        // at the tail (old -> new).
        if entry.pinned {
            hist.insert(0, entry);
        } else {
            hist.push(entry);
        }
        history_changed = true;
    }
    if hist.len() > max {
        // Dropped entries' cache files go too -- but only when the hash is no longer
        // referenced by a survivor.
        for dropped in &hist[max..] {
            cache_delete_for_removed(&hist[..max], dropped);
        }
        hist.truncate(max);
        history_changed = true;
    }
    if history_changed {
        super::bump_history_revision();
    }
    let swept = sweep_clip_image_cache(&hist);
    let total = hist.len();
    drop(hist);
    if swept > 0 {
        log_debug!("[clip] swept {} orphan image cache files", swept);
    }
    log_info!("Clipboard history loaded ({} entries).", total);
    // Rewrite right after loading so the merge/trim/preview-fill result is on disk,
    // keeping disk and memory in sync.
    save_history();
}

/// The atomic-write temp file name pattern: `clipboard-history.toml.tmp<pid>-<generation>`. A process
/// that dies between the temp write and the rename leaves one behind -- with the full plaintext
/// history -- so the cleanup paths have to recognize and remove them.
/// Pure, unit-tested: strict, so an unrelated file in the same directory is never touched.
pub(super) fn is_history_temp_file_name(name: &str) -> bool {
    let Some(rest) = name.strip_prefix("clipboard-history.toml.tmp") else {
        return false;
    };
    let Some((pid, generation)) = rest.split_once('-') else {
        return false;
    };
    !pid.is_empty()
        && !generation.is_empty()
        && pid.bytes().all(|b| b.is_ascii_digit())
        && generation.bytes().all(|b| b.is_ascii_digit())
}

/// Remove leftover atomic-write temp files from the directory holding the history file. They are
/// plaintext history, so a crash must not leave them for the next session to ignore.
fn sweep_history_temp_files(history_path: &std::path::Path) {
    let Some(dir) = history_path.parent() else {
        return;
    };
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut removed = 0usize;
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        if is_history_temp_file_name(name) && std::fs::remove_file(entry.path()).is_ok() {
            removed += 1;
        }
    }
    if removed > 0 {
        log_info!("Removed {removed} leftover clipboard history temp file(s).");
    }
}

/// Delete every trace of the history: the file and the image cache. Used when the clipboard
/// switch is turned off and when `clear_on_quit` fires, so both paths share one implementation.
/// The generation bump drops persist jobs that are already queued, or a snapshot prepared before
/// the delete would land on disk afterwards.
pub(crate) fn discard_history_on_disk() {
    PERSIST_GENERATION.fetch_add(1, Ordering::AcqRel);
    let _io = PERSIST_IO_LOCK.lock().unwrap();
    let path = history_file_path();
    let cache_dir = clip_image_cache_dir();
    discard_history_in(&path, &cache_dir);
    log_info!("Clipboard history file and image cache removed.");
}

/// Delete a history file and wipe a cache directory. Parameterized for the test, which must not
/// disturb the shared directories the rest of the suite writes to.
pub(super) fn discard_history_in(history_path: &std::path::Path, cache_dir: &std::path::Path) {
    let _ = std::fs::remove_file(history_path);
    // A crash between the temp write and the rename leaves the whole history in a temp file.
    sweep_history_temp_files(history_path);
    // Invalidates the queued image work and waits out a write in flight, so once this returns no
    // job for the discarded history can put its files back (`wipe_cache_for_discard`).
    wipe_cache_for_discard(cache_dir);
}
