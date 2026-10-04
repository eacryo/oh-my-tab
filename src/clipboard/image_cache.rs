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
pub(super) struct ImageCacheJob {
    pub(super) hash: u64,
    pub(super) data: Option<Arc<Vec<u8>>>,
    pub(super) preview: Arc<Vec<u8>>,
    pub(super) source_path: Option<String>,
    pub(super) warm_detail: bool,
    /// The cache generation this job was queued in; a job whose generation is stale must not
    /// write, see `wipe_cache_for_discard`.
    pub(super) generation: u64,
    /// The hash epoch this job was queued in; a deletion of that record bumps it, see
    /// `retire_image_hash`.
    pub(super) epoch: u64,
}

/// Per-hash job epoch. The global generation cannot express "this record only" -- it would invalidate
/// every other record's queued job too -- so each hash carries its own epoch: a job records the epoch
/// it was queued in, and deleting that record's hash bumps it, which invalidates exactly the jobs
/// queued before the deletion. Re-recording the same image does NOT restore them: a fresh job reads
/// the bumped epoch, so only new work is admitted (an old job must not write a file-reference record's
/// data bytes, or resurrect anything for a deleted entry).
/// Guarded by `CACHE_WRITE_LOCK` wherever it is read or written.
static IMAGE_HASH_EPOCHS: LazyLock<Mutex<HashMap<u64, u64>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// Invalidate the queued work for one deleted record and drop its pending bytes: the files go, and a
/// job that is still queued for the same hash can no longer write them back (nor its preview, nor a
/// detail preview). Only this hash is affected -- other records' jobs keep their generation.
pub(super) fn retire_image_hash(hash: u64) {
    if hash == 0 {
        return;
    }
    // One lock with the writers, so the delete cannot interleave with a write it is meant to cancel.
    let _guard = CACHE_WRITE_LOCK.lock().unwrap();
    *IMAGE_HASH_EPOCHS.lock().unwrap().entry(hash).or_insert(0) += 1;
    PENDING_IMAGE_DATA.lock().unwrap().remove(&hash);
    clear_detail_slot_for_hash(hash);
    cache_delete_image(hash);
}

/// Drop the detail-preview delivery slot when it belongs to `hash` (a deleted record's preview must
/// not be shown later).
fn clear_detail_slot_for_hash(hash: u64) {
    let mut slot = DETAIL_PENDING_HD.lock().unwrap();
    if slot.as_ref().is_some_and(|(h, _, _, _)| *h == hash) {
        *slot = None;
    }
}

/// The epoch `hash` is at now; a job queued in a different epoch was invalidated by a deletion.
pub(super) fn current_hash_epoch(hash: u64) -> u64 {
    if hash == 0 {
        return 0;
    }
    *IMAGE_HASH_EPOCHS.lock().unwrap().get(&hash).unwrap_or(&0)
}

/// The epoch a job about to be queued for `hash` should carry. Recording an image does not reset the
/// epoch: a job queued before a deletion stays invalid, and only this new job is admitted.
pub(super) fn hash_epoch_for_new_job(hash: u64) -> u64 {
    current_hash_epoch(hash)
}

/// Invalidates queued cache work. `wipe_cache_for_discard` bumps it before wiping, so jobs that
/// were queued (or are waiting) for the history that was just deleted cannot recreate its image
/// files afterwards.
static CACHE_GENERATION: AtomicU64 = AtomicU64::new(0);

/// Serializes cache writes against a wipe. A writer holds it across its generation check and its
/// write; the wipe bumps the generation first and then takes it, so a wipe that has returned can
/// never be followed by an older job's write, and a write that won the race is removed by the wipe.
static CACHE_WRITE_LOCK: Mutex<()> = Mutex::new(());

pub(super) fn cache_generation() -> u64 {
    CACHE_GENERATION.load(Ordering::Acquire)
}

/// Run one cache write under the write lock, but only while its generation is still current.
/// Nothing but the write itself belongs here: reading, decoding and encoding happen outside, or a
/// discard on the main thread (switch off / quit) would wait for a large image to be re-encoded.
/// Returns whether the write ran.
pub(super) fn write_while_current(generation: u64, write: impl FnOnce()) -> bool {
    write_while_current_for(0, generation, 0, write)
}

/// As `write_while_current`, but for one hash: the job's generation must still be current AND its
/// hash epoch unchanged, so a record deleted while this job waited (`retire_image_hash`) never gets
/// its files back. `hash == 0` means "no per-hash rule".
fn write_while_current_for(hash: u64, generation: u64, epoch: u64, write: impl FnOnce()) -> bool {
    let _guard = CACHE_WRITE_LOCK.lock().unwrap();
    if generation != cache_generation() || current_hash_epoch(hash) != epoch {
        return false;
    }
    write();
    true
}

/// Store a freshly generated detail preview in the delivery slot, but only while its generation is
/// still current: the check and the store share one lock, so a discard cannot slip in between them
/// and leave a deleted entry's preview behind. Returns whether it was stored.
pub(super) fn offer_detail_preview(generation: u64, hash: u64, epoch: u64, png: Vec<u8>) -> bool {
    if generation != cache_generation() {
        return false;
    }
    let _guard = CACHE_WRITE_LOCK.lock().unwrap();
    // Both checks inside the lock: a deletion between the decode and this store must keep the
    // preview of a deleted record out of the slot (the file write is refused for the same reason).
    if generation != cache_generation() || current_hash_epoch(hash) != epoch {
        return false;
    }
    *DETAIL_PENDING_HD.lock().unwrap() = Some((hash, generation, epoch, png));
    true
}

/// Delete every cached file and invalidate the work queued for it (see `CACHE_GENERATION`). The
/// pending maps go too: they hold original bytes and generated previews of entries that no longer
/// exist. Callers reach this only when the whole history is gone -- clearing one scope per hash
/// keeps the remaining entries' jobs valid.
pub(super) fn wipe_cache_for_discard(dir: &std::path::Path) {
    CACHE_GENERATION.fetch_add(1, Ordering::AcqRel);
    // Taking the lock waits for a write in flight, so that write's files are inside the wipe.
    {
        let _guard = CACHE_WRITE_LOCK.lock().unwrap();
        PENDING_IMAGE_DATA.lock().unwrap().clear();
        IMAGE_HASH_EPOCHS.lock().unwrap().clear();
        DETAIL_PENDING_HD.lock().unwrap().take();
        clear_image_cache_dir(dir);
    }
    DETAIL_INFLIGHT.lock().unwrap().clear();
}

static IMAGE_CACHE_SENDER: OnceLock<Option<SyncSender<ImageCacheJob>>> = OnceLock::new();

/// Original bytes recorded but not yet on disk (hash -> PendingImage). If the user pastes /
/// saves before the background write lands, a cache miss falls back to this map, so the
/// async write never breaks a paste. Bounded: on overflow the oldest entry is written
/// synchronously and dropped, so memory cannot grow without limit.
pub(super) static PENDING_IMAGE_DATA: LazyLock<Mutex<HashMap<u64, PendingImage>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));
/// Monotonic insertion sequence: HashMap order is arbitrary, so "oldest" needs an explicit
/// sequence number.
static PENDING_IMAGE_SEQ: AtomicU64 = AtomicU64::new(0);
const PENDING_IMAGE_LIMIT: usize = 16;

/// One pending byte payload plus its insertion sequence (used to evict in insertion order on
/// overflow).
pub(super) struct PendingImage {
    pub(super) seq: u64,
    /// The cache generation and the hash epoch that recorded these bytes. A finished write retires
    /// only the entry its own job queued: after a deletion (epoch bump) and a re-record of the same
    /// image the hash is identical, and dropping the entry by hash -- or by a stale job -- would
    /// delete the new bytes while the file they belong to is still being written.
    pub(super) generation: u64,
    pub(super) epoch: u64,
    pub(super) bytes: Arc<Vec<u8>>,
}

/// Test-only: place a pending fallback entry exactly as the record path does, but without queueing
/// the write job, so an interleaving can be constructed deterministically.
#[cfg(test)]
pub(super) fn insert_pending_for_tests(hash: u64, bytes: Arc<Vec<u8>>) {
    let generation = cache_generation();
    let seq = PENDING_IMAGE_SEQ.fetch_add(1, Ordering::Relaxed);
    PENDING_IMAGE_DATA.lock().unwrap().insert(
        hash,
        PendingImage {
            seq,
            generation,
            epoch: current_hash_epoch(hash),
            bytes,
        },
    );
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
    let generation = cache_generation();
    // The epoch a NEW job for this image carries. Deliberately not reset: a job queued before a
    // deletion stays invalid, so re-recording admits only this new work.
    let epoch = hash_epoch_for_new_job(hash);
    if let Some(bytes) = &data {
        let mut pending = PENDING_IMAGE_DATA.lock().unwrap();
        if pending.len() >= PENDING_IMAGE_LIMIT {
            // Extreme burst: flush the oldest entry synchronously before inserting, keeping
            // the fallback map bounded.
            if let Some(old_hash) = oldest_pending_hash(&pending) {
                let old_bytes = pending.remove(&old_hash).map(|image| image.bytes);
                drop(pending);
                if let Some(old_bytes) = old_bytes {
                    write_while_current(generation, || {
                        let _ = cache_write_image(old_hash, &old_bytes);
                    });
                }
                pending = PENDING_IMAGE_DATA.lock().unwrap();
            }
        }
        let seq = PENDING_IMAGE_SEQ.fetch_add(1, Ordering::Relaxed);
        pending.insert(
            hash,
            PendingImage {
                seq,
                generation,
                epoch,
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
        generation,
        epoch,
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

/// Write one job's original bytes and retire its pending fallback entry: both steps under the same
/// generation guard, and the retirement is keyed to the JOB's generation (not just the hash), so a
/// job that resumes after a discard cannot drop a re-recorded entry's bytes.
/// The fallback map is what a paste or save-as reads while the file is not on disk yet, so dropping
/// the wrong entry there loses data that is still being written. A failed write keeps the entry.
pub(super) fn finish_image_write(hash: u64, generation: u64, epoch: u64, data: &[u8]) {
    write_while_current_for(hash, generation, epoch, || {
        if cache_write_image(hash, data) {
            retire_pending_after_write(hash, generation, epoch);
        }
    });
}

/// Retire the pending fallback entry a finished write replaced. Two things matter, and the failure
/// they prevent only shows up under a specific interleaving: a job writes (its generation is still
/// current), the history is discarded, the user copies the SAME image again (same hash, new pending
/// entry, generation bumped, file not written yet), and only then does the old job get here.
/// It runs inside the writer's generation guard (`finish_image_write`) AND matches the entry's
/// generation against the job's rather than the hash alone, so the re-recorded entry keeps the bytes
/// a paste or save-as needs while the file is missing.
pub(super) fn retire_pending_after_write(hash: u64, generation: u64, epoch: u64) {
    let mut pending = PENDING_IMAGE_DATA.lock().unwrap();
    if pending
        .get(&hash)
        .is_some_and(|image| image.generation == generation && image.epoch == epoch)
    {
        pending.remove(&hash);
    }
}

/// One background job: persist the original bytes, then the preview, then optionally/// One background job: persist the original bytes, then the preview, then optionally
/// pregenerate the detail image (idempotent; skipped when already cached).
pub(super) fn run_image_cache_job(job: &ImageCacheJob) {
    if let Some(data) = &job.data {
        finish_image_write(job.hash, job.generation, job.epoch, data);
    }
    if !job.preview.is_empty() {
        write_while_current_for(job.hash, job.generation, job.epoch, || {
            let _ = cache_write_preview(job.hash, &job.preview);
        });
    }
    // The optional detail pregen is the expensive part (read + decode + encode): it runs outside the
    // lock, and only the resulting write is generation-checked.
    if job.warm_detail && !clip_image_detail_path(job.hash).exists() {
        let generated = unsafe {
            let pool: *mut AnyObject = msg_send![class!(NSAutoreleasePool), new];
            let png = generate_detail_preview_png(job.hash, job.source_path.as_deref());
            let _: () = msg_send![pool, drain];
            png
        };
        if let Some(png) = generated {
            write_while_current_for(job.hash, job.generation, job.epoch, || {
                let _ = cache_write_detail_preview(job.hash, &png);
            });
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

/// Whether a finished detail preview may be delivered. Every condition is required: its cache
/// generation is still current (a discard means the entry it describes is gone), its hash epoch is
/// still current (that record was deleted meanwhile, even if the history itself was not discarded --
/// clearing one filter scope is enough), and it is still the one the detail panel wants.
/// Pure, unit-tested.
pub(super) fn detail_slot_deliverable(
    slot_generation: u64,
    current_generation: u64,
    slot_epoch: u64,
    current_epoch: u64,
    still_wanted: bool,
) -> bool {
    slot_generation == current_generation && slot_epoch == current_epoch && still_wanted
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
    pub(super) hash: u64,
    pub(super) source_path: Option<String>,
    pub(super) deliver: bool,
    // Freshness inputs snapshotted by the main thread at enqueue time; the worker never
    // reads picker/UI statics directly.
    pub(super) detail_visible: bool,
    pub(super) selected_hash: Option<u64>,
    /// The cache generation this job was queued in (see `wipe_cache_for_discard`).
    pub(super) generation: u64,
    /// The hash epoch this job was queued in (see `retire_image_hash`).
    pub(super) epoch: u64,
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
    // A wipe that happened while this job waited makes the whole job pointless: the entry it
    // describes is gone, so neither its file nor its delivered preview may come back.
    if job.generation != cache_generation() || current_hash_epoch(job.hash) != job.epoch {
        finish_detail_job(job);
        return;
    }
    // Idempotent: when pregen and on-demand requests race, whoever lands first writes
    // the file and the other skips.
    if clip_image_detail_path(job.hash).exists() {
        finish_detail_job(job);
        return;
    }
    // Enqueue-time freshness snapshot: the worker uses only values supplied by the main
    // thread for this cheap discard check, never picker/UI statics. The final guard remains
    // in the main-thread callback because the user can navigate away during generation.
    if job.deliver && !detail_result_still_wanted(job.detail_visible, job.selected_hash, job.hash) {
        finish_detail_job(job);
        return;
    }
    // AppKit temporaries (NSImage/TIFF/PNG encodes) drain with the pool -- same
    // precedent as icon extraction's "background thread + autoreleasepool"; if this ever
    // proves unstable, switch to pure CoreGraphics (CGImageSourceCreateThumbnailAtIndex,
    // unconditionally thread-safe).
    // Reading, decoding and encoding run without the write lock: a discard on the main thread must
    // not wait for a large image to be re-encoded.
    let pool: *mut AnyObject = msg_send![class!(NSAutoreleasePool), new];
    let png = generate_detail_preview_png(job.hash, job.source_path.as_deref());
    let _: () = msg_send![pool, drain];
    if let Some(png) = png {
        write_while_current_for(job.hash, job.generation, job.epoch, || {
            let _ = cache_write_detail_preview(job.hash, &png);
        });
        if job.deliver {
            // Stash before hopping to the main thread: the handler / show_detail_for_sel sees
            // complete bytes when consuming the slot. The store is generation-checked and shares
            // one lock with the check, so a discard that happens now cannot leave this preview for
            // a deleted entry behind.
            if offer_detail_preview(job.generation, job.hash, job.epoch, png) {
                let target = observer();
                let _: () = msg_send![
                    target,
                    performSelectorOnMainThread: sel!(detailPreviewReady:),
                    withObject: std::ptr::null_mut::<AnyObject>(),
                    waitUntilDone: false
                ];
            }
        }
    }
    finish_detail_job(job);
}

/// Release this job's in-flight marker. Keyed by the job's own identity, so a request queued after a
/// deletion (a new generation or epoch for the same hash) keeps its marker and still runs.
fn finish_detail_job(job: &DetailPreviewJob) {
    DETAIL_INFLIGHT
        .lock()
        .unwrap()
        .remove(&(job.hash, job.generation, job.epoch));
}

/// Generate the detail-preview bytes from the data cache / source file (pure IO + decode/encode
/// with no UI statics touched -- safe on any thread, and deliberately without the cache write
/// lock: it can take as long as the image is big). Degenerate hash=0 entries generate the same way
/// but must not be cached.
pub(super) unsafe fn generate_detail_preview_png(
    hash: u64,
    source_path: Option<&str>,
) -> Option<Vec<u8>> {
    let bytes = match source_path {
        None => cache_read_image(hash),
        Some(p) => std::fs::read(p).ok(),
    };
    let bytes = bytes?;
    any_image_to_scaled_png(&bytes, DETAIL_PREVIEW_MAX_DIM)
}

/// Enqueue a detail-preview generation job (degenerate hash=0 entries are never queued).
//  Deduplicated via the in-flight set; an enqueue failure (dead worker) rolls the marker
//  back.
pub(super) fn request_detail_preview(img: &ImageEntry, deliver: bool) {
    if img.hash == 0 {
        return;
    }
    let generation = cache_generation();
    let epoch = hash_epoch_for_new_job(img.hash);
    let key = (img.hash, generation, epoch);
    {
        let mut inflight = DETAIL_INFLIGHT.lock().unwrap();
        // Same hash in a NEW generation/epoch is a different job: the one in flight belongs to a
        // record that was discarded (its result will be refused), so this request must be queued.
        if !inflight.insert(key) {
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
            generation,
            epoch,
        }),
        Err(flume::TrySendError::Disconnected(_)) | Err(flume::TrySendError::Full(_))
    ) {
        DETAIL_INFLIGHT.lock().unwrap().remove(&key);
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
        if let Some((h, generation, epoch, _)) = slot.as_ref() {
            // Same rule as the delivery path: bytes generated for a discarded history, or for a
            // record that was deleted since, are dropped instead of shown.
            if *h == img.hash
                && detail_slot_deliverable(
                    *generation,
                    cache_generation(),
                    *epoch,
                    current_hash_epoch(*h),
                    true,
                )
            {
                return slot.take().map(|(_, _, _, p)| Arc::new(p));
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
        // Deleting the files is not enough: a cache job queued for this record is still allowed to
        // write (its generation stays valid), so retirement is part of "this hash is gone".
        retire_image_hash(hash);
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
/// Wipe one cache directory (the dir itself stays). Parameterized so the discard path can be
/// tested against a directory of its own instead of the shared one every other test uses; the
/// production caller passes `clip_image_cache_dir()`.
pub(super) fn clear_image_cache_dir(dir: &std::path::Path) {
    if let Ok(entries) = std::fs::read_dir(dir) {
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
