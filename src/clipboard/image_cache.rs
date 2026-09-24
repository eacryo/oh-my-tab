//! Clipboard subsystem · image_cache: on-disk image bytes and preview cache.

use super::*;
use std::sync::mpsc::{self, SyncSender, TrySendError};

/// The image-byte cache directory: original-format bytes live on disk and memory only keeps
/// the downsampled preview; persistence-off startup wipes it, while persistence-on startup
/// sweeps unreferenced files. Test builds use a dedicated directory, never the real cache.
pub(super) fn clip_image_cache_dir() -> std::path::PathBuf {
    // Smoke mode (--smoke-clipboard) uses a dedicated dir: the smoke runs the REAL binary,
    // so cfg!(test) is off -- without this, injected test entries used to land in the
    // user's real history/cache.
    let name = if SMOKE_MODE.load(Ordering::SeqCst) {
        format!("oh-my-tab-clip-images-smoke-{}", std::process::id())
    } else if cfg!(test) {
        format!(
            "oh-my-tab-clip-images-test-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        )
    } else {
        "oh-my-tab-clip-images".to_string()
    };

    if SMOKE_MODE.load(Ordering::SeqCst) || cfg!(test) {
        // Keep test/smoke data under the system temp directory instead of HOME: the Codex
        // sandbox allows writes in the workspace and temp directories, but may allow reads
        // while denying writes under $HOME/Library/Caches. Otherwise cache tests fail at
        // create_dir_all/rename. Process+thread suffixes preserve parallel-test isolation
        // and avoid reusing a directory from an earlier process.
        return std::env::temp_dir().join(name);
    }

    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string());
    std::path::PathBuf::from(format!("{}/Library/Caches/{}", home, name))
}

/// hash -> cache file path.
pub(super) fn clip_image_path(hash: u64) -> std::path::PathBuf {
    clip_image_cache_dir().join(format!("{hash:016x}"))
}

/// Write bytes into the cache.
pub(super) fn cache_write_image(hash: u64, bytes: &[u8]) -> bool {
    let dir = clip_image_cache_dir();
    if std::fs::create_dir_all(&dir).is_err() {
        return false;
    }
    let path = clip_image_path(hash);
    if path.exists() {
        return true;
    }
    // Write to a temp file then rename, so the paste path never reads a half-written file.
    let tmp = dir.join(format!("{hash:016x}.tmp"));
    let ok = std::fs::write(&tmp, bytes).is_ok() && std::fs::rename(&tmp, &path).is_ok();
    if !ok {
        let _ = std::fs::remove_file(&tmp);
    }
    ok
}

/// Read the original bytes back.
pub(super) fn cache_read_image(hash: u64) -> Option<Vec<u8>> {
    std::fs::read(clip_image_path(hash)).ok()
}

/// Delete a cache file (data + preview).
pub(super) fn cache_delete_image(hash: u64) {
    let _ = std::fs::remove_file(clip_image_path(hash));
    let _ = std::fs::remove_file(clip_image_preview_path(hash));
    let _ = std::fs::remove_file(clip_image_detail_path(hash));
}

/// hash -> the preview file path (the thumbnail is persisted separately so loading the
/// history after a restart needs no re-decoding).
pub(super) fn clip_image_preview_path(hash: u64) -> std::path::PathBuf {
    clip_image_cache_dir().join(format!("{hash:016x}.preview"))
}

/// Write the preview PNG into the cache (idempotent).
pub(super) fn cache_write_preview(hash: u64, preview: &[u8]) -> bool {
    let dir = clip_image_cache_dir();
    if std::fs::create_dir_all(&dir).is_err() {
        return false;
    }
    let path = clip_image_preview_path(hash);
    if path.exists() {
        return true;
    }
    let tmp = dir.join(format!("{hash:016x}.preview.tmp"));
    let ok = std::fs::write(&tmp, preview).is_ok() && std::fs::rename(&tmp, &path).is_ok();
    if !ok {
        let _ = std::fs::remove_file(&tmp);
    }
    ok
}

/// Read the preview back (None when missing; the caller regenerates from the data bytes).
pub(super) fn cache_read_preview(hash: u64) -> Option<Vec<u8>> {
    std::fs::read(clip_image_preview_path(hash)).ok()
}

/// hash -> the detail-preview path (the big image shown by the → detail panel;
/// pregenerated in the background at record time, with a first-open fallback).
pub(super) fn clip_image_detail_path(hash: u64) -> std::path::PathBuf {
    clip_image_cache_dir().join(format!("{hash:016x}.detail"))
}

/// Read the detail preview back (None when missing).
pub(super) fn cache_read_detail_preview(hash: u64) -> Option<Vec<u8>> {
    std::fs::read(clip_image_detail_path(hash)).ok()
}

/// Write the detail preview PNG into the cache (idempotent).
pub(super) fn cache_write_detail_preview(hash: u64, png: &[u8]) -> bool {
    let dir = clip_image_cache_dir();
    if std::fs::create_dir_all(&dir).is_err() {
        return false;
    }
    let path = clip_image_detail_path(hash);
    if path.exists() {
        return true;
    }
    let tmp = dir.join(format!("{hash:016x}.detail.tmp"));
    let ok = std::fs::write(&tmp, png).is_ok() && std::fs::rename(&tmp, &path).is_ok();
    if !ok {
        let _ = std::fs::remove_file(&tmp);
    }
    ok
}

/// One image-cache write job: persist the original bytes (data entries only), persist the
/// preview, then optionally pregenerate the detail image.
struct ImageCacheJob {
    hash: u64,
    data: Option<Arc<Vec<u8>>>,
    preview: Arc<Vec<u8>>,
    source_path: Option<String>,
    warm_detail: bool,
}

static IMAGE_CACHE_SENDER: OnceLock<Option<SyncSender<ImageCacheJob>>> = OnceLock::new();

/// Original bytes recorded but not yet on disk (hash -> PendingImage). If the user pastes /
/// saves before the background write lands, a cache miss falls back to this map, so the
/// async write never breaks a paste. Bounded: on overflow the oldest entry is written
/// synchronously and dropped, so memory cannot grow without limit.
static PENDING_IMAGE_DATA: LazyLock<Mutex<HashMap<u64, PendingImage>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));
/// Monotonic insertion sequence: HashMap order is arbitrary, so "oldest" needs an explicit
/// sequence number.
static PENDING_IMAGE_SEQ: AtomicU64 = AtomicU64::new(0);
const PENDING_IMAGE_LIMIT: usize = 16;

/// One pending byte payload plus its insertion sequence (used to evict in insertion order on
/// overflow).
pub(super) struct PendingImage {
    pub(super) seq: u64,
    pub(super) bytes: Arc<Vec<u8>>,
}

/// Pick the oldest entry (smallest insertion sequence). Pure and unit-tested: HashMap
/// iteration order is unspecified, so `iter().next()` evicts an arbitrary entry, not the
/// oldest one.
pub(super) fn oldest_pending_hash(pending: &HashMap<u64, PendingImage>) -> Option<u64> {
    pending
        .iter()
        .min_by_key(|(_, image)| image.seq)
        .map(|(&hash, _)| hash)
}

fn image_cache_sender() -> Option<&'static SyncSender<ImageCacheJob>> {
    IMAGE_CACHE_SENDER
        .get_or_init(|| {
            let (sender, receiver) = mpsc::sync_channel::<ImageCacheJob>(32);
            std::thread::Builder::new()
                .name("clip-image-cache".into())
                .spawn(move || {
                    while let Ok(job) = receiver.recv() {
                        run_image_cache_job(&job);
                    }
                })
                .ok()
                .map(|_| sender)
        })
        .as_ref()
}

/// The main-thread record path's entry point: hand the image-cache writes (and optional
/// detail pregen) to the background thread. When enqueueing fails (queue full / worker
/// gone), fall back to a synchronous write on the main thread -- bytes are never
/// silently dropped.
pub(super) fn schedule_image_cache_write(
    hash: u64,
    data: Option<Arc<Vec<u8>>>,
    preview: Arc<Vec<u8>>,
    source_path: Option<String>,
    warm_detail: bool,
) {
    if hash == 0 {
        return;
    }
    if let Some(bytes) = &data {
        let mut pending = PENDING_IMAGE_DATA.lock().unwrap();
        if pending.len() >= PENDING_IMAGE_LIMIT {
            // Extreme burst: flush the oldest entry synchronously before inserting, keeping
            // the fallback map bounded.
            if let Some(old_hash) = oldest_pending_hash(&pending) {
                let old_bytes = pending.remove(&old_hash).map(|image| image.bytes);
                drop(pending);
                if let Some(old_bytes) = old_bytes {
                    let _ = cache_write_image(old_hash, &old_bytes);
                }
                pending = PENDING_IMAGE_DATA.lock().unwrap();
            }
        }
        let seq = PENDING_IMAGE_SEQ.fetch_add(1, Ordering::Relaxed);
        pending.insert(
            hash,
            PendingImage {
                seq,
                bytes: bytes.clone(),
            },
        );
    }
    let job = ImageCacheJob {
        hash,
        data,
        preview,
        source_path,
        warm_detail,
    };
    match image_cache_sender() {
        Some(sender) => match sender.try_send(job) {
            Ok(()) => {}
            // try_send hands the job back; run it synchronously.
            Err(TrySendError::Full(job)) | Err(TrySendError::Disconnected(job)) => {
                run_image_cache_job(&job);
            }
        },
        // Worker unavailable (very rare): run on the main thread, never drop bytes.
        None => run_image_cache_job(&job),
    }
}

/// One background job: persist the original bytes, then the preview, then optionally
/// pregenerate the detail image (idempotent; skipped when already cached).
fn run_image_cache_job(job: &ImageCacheJob) {
    if let Some(data) = &job.data {
        // Remove from the fallback map only on success; on failure it is kept so a paste can
        // still obtain the bytes from memory.
        if cache_write_image(job.hash, data) {
            PENDING_IMAGE_DATA.lock().unwrap().remove(&job.hash);
        }
    }
    if !job.preview.is_empty() {
        let _ = cache_write_preview(job.hash, &job.preview);
    }
    if job.warm_detail && !clip_image_detail_path(job.hash).exists() {
        unsafe {
            let pool: *mut AnyObject = msg_send![class!(NSAutoreleasePool), new];
            let _ = generate_detail_preview_bytes(job.hash, job.source_path.as_deref());
            let _: () = msg_send![pool, drain];
        }
    }
}

/// Fetch an image's original bytes: prefer the on-disk cache, fall back to the in-memory
/// pending bytes when it misses. Shared by paste and save-as.
pub(super) fn image_bytes_for_hash(hash: u64) -> Option<Arc<Vec<u8>>> {
    if let Some(bytes) = cache_read_image(hash) {
        return Some(Arc::new(bytes));
    }
    PENDING_IMAGE_DATA
        .lock()
        .unwrap()
        .get(&hash)
        .map(|image| image.bytes.clone())
}

/// Freshness predicate for background detail-preview jobs (pure; unit-tested): the
/// result is worth swapping into the UI only when the detail panel is visible AND the
/// currently selected entry IS the job's entry. Pregen jobs (enqueued at record time)
/// skip this check -- they only populate the disk cache and never touch the selection.
pub(super) fn detail_result_still_wanted(
    detail_visible: bool,
    current_hash: Option<u64>,
    job_hash: u64,
) -> bool {
    detail_visible && current_hash == Some(job_hash)
}

/// The selected image entry's hash (used for main-thread enqueue snapshots and completion
/// validation; all access goes through Mutexes). None without a selection / for text entries.
pub(super) fn detail_current_hash() -> Option<u64> {
    let sel = picker_selection();
    if sel == NO_SELECTION {
        return None;
    }
    let h_idx = mapped_index(sel)?;
    let hist = CLIP_HISTORY.lock().unwrap();
    hist.get(h_idx)
        .and_then(|e| e.image.as_ref())
        .map(|i| i.hash)
}

/// A background detail-preview job: generate the <=1280px `{hash}.detail` from the data
/// cache / source file and write it atomically. deliver = true means the job originated
/// from a first-open miss (try to refresh the UI afterwards); false = record/load warm-up
/// (cache only, no UI interaction).
pub(super) struct DetailPreviewJob {
    hash: u64,
    source_path: Option<String>,
    deliver: bool,
    // Freshness inputs snapshotted by the main thread at enqueue time; the worker never
    // reads picker/UI statics directly.
    detail_visible: bool,
    selected_hash: Option<u64>,
}

/// The sender side of the detail-preview worker (a lazily started persistent loop).
//  flume recv blocks between jobs; the thread dies with the process (no shutdown
//  protocol needed -- tmp+rename writes are atomic and interruption-safe).
pub(super) fn detail_job_sender() -> flume::Sender<DetailPreviewJob> {
    static SENDER: OnceLock<flume::Sender<DetailPreviewJob>> = OnceLock::new();
    SENDER
        .get_or_init(|| {
            // Detail requests are deduplicated by hash, so a small bounded queue is enough.
            // When full, roll back the in-flight marker and retry on the next detail open
            // instead of allowing clipboard payloads to accumulate without bound.
            let (tx, rx) = flume::bounded::<DetailPreviewJob>(4);
            // The thread name carries the module prefix for logs/debuggers.
            std::thread::Builder::new()
                .name("clip-detail-preview".into())
                .spawn(move || {
                    for job in rx.iter() {
                        unsafe { run_detail_preview_job(&job) };
                    }
                })
                .expect("spawn clip-detail-preview worker");
            tx
        })
        .clone()
}

/// One worker iteration: idempotent skip (cache exists) -> dequeue freshness check for
/// on-demand jobs -> generate inside an autoreleasepool + atomic cache write -> deliver
/// jobs stash the bytes and hop to the main thread.
pub(super) unsafe fn run_detail_preview_job(job: &DetailPreviewJob) {
    // Idempotent: when pregen and on-demand requests race, whoever lands first writes
    // the file and the other skips.
    if clip_image_detail_path(job.hash).exists() {
        DETAIL_INFLIGHT.lock().unwrap().remove(&job.hash);
        return;
    }
    // Enqueue-time freshness snapshot: the worker uses only values supplied by the main
    // thread for this cheap discard check, never picker/UI statics. The final guard remains
    // in the main-thread callback because the user can navigate away during generation.
    if job.deliver && !detail_result_still_wanted(job.detail_visible, job.selected_hash, job.hash) {
        DETAIL_INFLIGHT.lock().unwrap().remove(&job.hash);
        return;
    }
    // AppKit temporaries (NSImage/TIFF/PNG encodes) drain with the pool -- same
    // precedent as icon extraction's "background thread + autoreleasepool"; if this ever
    // proves unstable, switch to pure CoreGraphics (CGImageSourceCreateThumbnailAtIndex,
    // unconditionally thread-safe).
    let pool: *mut AnyObject = msg_send![class!(NSAutoreleasePool), new];
    let png = generate_detail_preview_bytes(job.hash, job.source_path.as_deref());
    let _: () = msg_send![pool, drain];
    if let Some(png) = png {
        if job.deliver {
            // Stash before hopping to the main thread: the handler / show_detail_for_sel
            // sees complete bytes when consuming the slot.
            *DETAIL_PENDING_HD.lock().unwrap() = Some((job.hash, png));
            let target = observer();
            let _: () = msg_send![
                target,
                performSelectorOnMainThread: sel!(detailPreviewReady:),
                withObject: std::ptr::null_mut::<AnyObject>(),
                waitUntilDone: false
            ];
        }
    }
    DETAIL_INFLIGHT.lock().unwrap().remove(&job.hash);
}

/// Generate the detail-preview bytes from the data cache / source file and cache them
/// (the generation half of the old ensure_detail_preview; pure IO + decode/encode with no
/// UI statics touched -- safe on any thread). Degenerate hash=0 skips the cache write to
/// avoid orphan files.
pub(super) unsafe fn generate_detail_preview_bytes(
    hash: u64,
    source_path: Option<&str>,
) -> Option<Vec<u8>> {
    let bytes = match source_path {
        None => cache_read_image(hash),
        Some(p) => std::fs::read(p).ok(),
    };
    let bytes = bytes?;
    let png = any_image_to_scaled_png(&bytes, DETAIL_PREVIEW_MAX_DIM)?;
    if hash != 0 {
        cache_write_detail_preview(hash, &png);
    }
    Some(png)
}

/// Enqueue a detail-preview generation job (degenerate hash=0 entries are never queued).
//  Deduplicated via the in-flight set; an enqueue failure (dead worker) rolls the marker
//  back.
pub(super) fn request_detail_preview(img: &ImageEntry, deliver: bool) {
    if img.hash == 0 {
        return;
    }
    {
        let mut inflight = DETAIL_INFLIGHT.lock().unwrap();
        if !inflight.insert(img.hash) {
            return;
        }
    }
    let (detail_visible, selected_hash) = if deliver {
        // ensure_detail_preview is called only after the current detail content is known to
        // be this image. The visible flag may not be set until show_detail_for_sel finishes,
        // so do not discard the first-open job based on that not-yet-updated flag.
        (true, Some(img.hash))
    } else {
        (false, None)
    };
    if matches!(
        detail_job_sender().try_send(DetailPreviewJob {
            hash: img.hash,
            source_path: img.source_path.clone(),
            deliver,
            detail_visible,
            selected_hash,
        }),
        Err(flume::TrySendError::Disconnected(_)) | Err(flume::TrySendError::Full(_))
    ) {
        DETAIL_INFLIGHT.lock().unwrap().remove(&img.hash);
    }
}

/// Fetch the detail display bytes for an image entry (synchronous three states, NEVER
/// blocking): 1) a hit in the freshly-generated slot -> consume and clear it; 2) the
/// `{hash}.detail` disk cache -> a millisecond-scale read; 3) return the in-memory 480px
/// preview right away while enqueueing background generation (deliver=true); completion
/// triggers a rebuild through detail_preview_ready to upgrade to hi-res. None (no bytes
//  at all) lets the caller fall back to the filename text.
pub(super) fn ensure_detail_preview(img: &ImageEntry) -> Option<Arc<Vec<u8>>> {
    {
        let mut slot = DETAIL_PENDING_HD.lock().unwrap();
        if let Some((h, _)) = slot.as_ref() {
            if *h == img.hash {
                return slot.take().map(|(_, p)| Arc::new(p));
            }
        }
    }
    if let Some(png) = cache_read_detail_preview(img.hash) {
        return Some(Arc::new(png));
    }
    if !img.preview_png.is_empty() {
        request_detail_preview(img, true);
        // The in-memory preview is already an Arc: clone the refcount, no deep PNG copy.
        return Some(img.preview_png.clone());
    }
    None
}

/// Whether `hash` is still referenced by the surviving entries. A file entry and a data
/// entry with identical content (same hash) coexist and SHARE the disk cache (`{hash}`
/// data bytes + `{hash}.preview`), so deletion MUST check references first -- an unguarded
/// delete would wipe a survivor's files (a data entry would lose its paste bytes forever).
pub(super) fn hash_referenced_by<'a>(
    mut survivors: impl Iterator<Item = &'a ClipEntry>,
    hash: u64,
) -> bool {
    survivors.any(|e| e.image.as_ref().is_some_and(|i| i.hash == hash))
}

/// Remove a removed image's cache by hash, after confirming no survivor shares the file.
pub(super) fn cache_delete_for_hash(history: &[ClipEntry], hash: u64) {
    if hash != 0 && !hash_referenced_by(history.iter(), hash) {
        cache_delete_image(hash);
    }
}

/// Delete a removed entry's cache files (data bytes + preview together), but ONLY when
/// the hash is no longer referenced by any surviving entry; a degenerate entry (hash=0)
/// has no files.
pub(super) fn cache_delete_for_removed(history: &[ClipEntry], removed: &ClipEntry) {
    let Some(img) = &removed.image else {
        return;
    };
    cache_delete_for_hash(history, img.hash);
}

/// Wipe the whole image cache dir (called at startup: the history is not persisted, so
/// any leftover file is an orphan).
pub(super) fn clear_clip_image_cache() {
    let dir = clip_image_cache_dir();
    if let Ok(entries) = std::fs::read_dir(&dir) {
        for e in entries.flatten() {
            let _ = std::fs::remove_file(e.path());
        }
    }
}

/// Sweep orphan image-cache files against the current history. DATA entries may keep the
/// extensionless original plus preview/detail; FILE references may keep preview/detail only.
pub(super) fn sweep_clip_image_cache(history: &[ClipEntry]) -> usize {
    let mut all_hashes = HashSet::new();
    let mut data_hashes = HashSet::new();
    for entry in history {
        let Some(img) = &entry.image else {
            continue;
        };
        if img.hash == 0 {
            continue;
        }
        all_hashes.insert(img.hash);
        if img.source_path.is_none() {
            data_hashes.insert(img.hash);
        }
    }

    let dir = clip_image_cache_dir();
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return 0;
    };
    let mut removed = 0;
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        let (stem, is_raw) = if let Some(stem) = name.strip_suffix(".preview") {
            (stem, false)
        } else if let Some(stem) = name.strip_suffix(".detail") {
            (stem, false)
        } else {
            (name, true)
        };
        let keep = u64::from_str_radix(stem, 16).ok().is_some_and(|hash| {
            all_hashes.contains(&hash) && (!is_raw || data_hashes.contains(&hash))
        });
        if !keep && std::fs::remove_file(&path).is_ok() {
            removed += 1;
        }
    }
    removed
}

/// Sweep against the in-memory history for startup and persistence-toggle paths.
pub(super) fn sweep_current_clip_image_cache() -> usize {
    let history = CLIP_HISTORY.lock().unwrap();
    sweep_clip_image_cache(&history)
}
