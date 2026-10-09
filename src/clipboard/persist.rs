//! Clipboard subsystem · persist: history persistence (encrypted write, migration, load, purge).
//!
//! The history file is sealed with the session's master key (`crypto.rs`); the TOML *inside*
//! the envelope is unchanged, so `serialize_history` / `parse_history` still describe the
//! content format. Every write, every sweep and every delete first asks `storage.rs` — that
//! module owns the state machine (R1–R6 in the plan) and the purge transaction.

use super::*;
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;

/// The history CONTENT format version (bump on structural changes; a higher version is
/// ignored on load and the app starts with an empty history).
pub(super) const HISTORY_VERSION: u32 = 1;

/// The logical name of the history file inside its envelope (`object_id` binding).
const HISTORY_LOGICAL_NAME: &str = "history";

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

/// The directory holding the history file (the encrypted file, the legacy plaintext file and
/// the purge markers all live here; test builds use a test dir).
fn history_dir() -> std::path::PathBuf {
    if SMOKE_MODE.load(Ordering::SeqCst) || cfg!(test) {
        // Test/smoke history must share the same temp root as the image cache. It is moved
        // out of HOME because the Codex sandbox restricts writes to $HOME/Library/Caches,
        // and persistence tests create this directory directly.
        return clip_image_cache_dir().join("history");
    }
    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string());
    std::path::PathBuf::from(format!("{home}/.config/oh-my-tab"))
}

/// The live (encrypted) history file.
pub(super) fn history_file_path() -> std::path::PathBuf {
    history_dir().join("clipboard-history.enc")
}

/// The legacy plaintext history file: read once by the migration, then deleted. Never written.
pub(super) fn legacy_history_file_path() -> std::path::PathBuf {
    history_dir().join("clipboard-history.toml")
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

/// How a history text parsed. `Corrupt` and `UnsupportedVersion` must stay apart: the first is
/// damage, the second means a newer build wrote this file — neither may be overwritten.
pub(super) enum ParsedHistory {
    Ok(Vec<ClipEntry>),
    UnsupportedVersion,
    Corrupt,
}

/// Classify the history text (pure, unit-tested).
pub(super) fn parse_history_classified(text: &str) -> ParsedHistory {
    let Ok(file) = toml::from_str::<HistoryFile>(text) else {
        return ParsedHistory::Corrupt;
    };
    if file.version > HISTORY_VERSION {
        return ParsedHistory::UnsupportedVersion;
    }
    ParsedHistory::Ok(file.entries)
}

/// Parse the history text: corruption or a version mismatch -> None. Test-only: the load path
/// needs the classification above, not this collapse.
#[cfg(test)]
pub(super) fn parse_history(text: &str) -> Option<Vec<ClipEntry>> {
    match parse_history_classified(text) {
        ParsedHistory::Ok(entries) => Some(entries),
        ParsedHistory::UnsupportedVersion | ParsedHistory::Corrupt => None,
    }
}

/// The logical cache name of an image hash (`{hash:016x}` and its two preview variants) -- the
/// `object_id` the reader recomputes from the name it asked for.
pub(super) fn image_logical_name(hash: u64) -> String {
    format!("{hash:016x}")
}

/// Seal `plaintext` and write it atomically to `dir/file_name` (temp file, read back and
/// authenticate, then rename). The verification step is what makes a later "it was written" a
/// fact rather than an assumption.
fn write_sealed_file(
    dir: &std::path::Path,
    file_name: &str,
    logical_name: &str,
    kind: ObjectKind,
    key: &MasterKey,
    plaintext: &[u8],
) -> bool {
    let object_id = object_id_for(logical_name);
    let Some(sealed) = seal(key, kind, &object_id, plaintext) else {
        log_info!("Clipboard write failed: sealing failed.");
        return false;
    };
    if std::fs::create_dir_all(dir).is_err() {
        log_info!("Clipboard write failed: cannot create dir.");
        return false;
    }
    let path = dir.join(file_name);
    let tmp = dir.join(format!("{file_name}.tmp{}-0", std::process::id()));
    let ok = std::fs::write(&tmp, &sealed).is_ok();
    if ok {
        // Mode 600: owner-only access.
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600));
    }
    // Read back and authenticate before publishing: a torn or truncated write must never
    // replace the previous file.
    let verified = ok
        && std::fs::read(&tmp)
            .ok()
            .is_some_and(|back| open(key, kind, &object_id, &back).is_ok());
    let ok = verified && std::fs::rename(&tmp, &path).is_ok();
    if !ok {
        let _ = std::fs::remove_file(&tmp);
        log_info!("Clipboard write failed: write/verify error.");
    }
    ok
}

struct PersistJob {
    generation: u64,
    /// The storage generation this snapshot belongs to: a purge or a storage rebuild bumps it,
    /// and a job from before that must not write.
    storage_generation: u64,
    path: std::path::PathBuf,
    entries: Vec<ClipEntry>,
}

/// One serial worker coalesces consecutive snapshots to the newest one, keeping copy events
/// from blocking the main thread on TOML serialization, encryption and filesystem I/O.
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
    // A snapshot from before a purge or a storage rebuild must not land: the storage generation
    // is the authority (a plain save only moves PERSIST_GENERATION).
    if job.storage_generation != storage::generation() {
        log_debug!("[clip] history snapshot dropped: storage generation moved");
        return;
    }
    let Some(key) = storage::key() else {
        log_info!("Clipboard history save skipped: no key in this session.");
        return;
    };
    let Some(text) = serialize_history(&job.entries) else {
        log_info!("Clipboard history save failed: serialize error.");
        return;
    };
    let Some(dir) = job.path.parent() else {
        return;
    };
    let Some(file_name) = job.path.file_name().and_then(|n| n.to_str()) else {
        return;
    };

    // Share the I/O lock with the discard path so clearing the history deletes the file after any
    // in-progress write and prevents an older snapshot from being restored.
    let _io = PERSIST_IO_LOCK.lock().unwrap();
    if PERSIST_GENERATION.load(Ordering::Acquire) != job.generation
        || job.storage_generation != storage::generation()
    {
        return;
    }
    if !write_sealed_file(
        dir,
        file_name,
        HISTORY_LOGICAL_NAME,
        ObjectKind::History,
        &key,
        text.as_bytes(),
    ) {
        return;
    }
    log_debug!(
        "[clip] history saved asynchronously ({} entries, generation={})",
        job.entries.len(),
        job.generation
    );
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

/// Save the current history to disk (sealed, atomic temp+rename, mode 600). Skipped entirely
/// unless the session may write: `Unavailable` / `Blocked` must never write, and a skipped save
/// is logged once per session (invariant R4).
pub(super) fn save_history() {
    if !storage::writable() {
        log_save_skipped_once();
        return;
    }
    if storage::key().is_none() {
        log_save_skipped_once();
        return;
    }
    let mut hist = CLIP_HISTORY.lock().unwrap();
    // Expire before writing: the disk file never keeps expired entries (expiry applies
    // to memory and persistence alike).
    expire_entries(&mut hist, now_secs(), ttl_secs());
    let entries = hist.clone();
    drop(hist);
    let generation = PERSIST_GENERATION.fetch_add(1, Ordering::AcqRel) + 1;
    let job = PersistJob {
        generation,
        storage_generation: storage::generation(),
        path: history_file_path(),
        entries,
    };
    if persist_sender().send(job).is_err() {
        log_info!("Clipboard history save failed: persistence worker stopped.");
    }
}

/// One log line per session when saving is off (a per-copy log would flood).
fn log_save_skipped_once() {
    static LOGGED: AtomicBool = AtomicBool::new(false);
    if !LOGGED.swap(true, Ordering::SeqCst) {
        log_info!("Clipboard history not saved this session (storage is not writable).");
    }
}

/// What the load path decided, computed OFF the main thread (the keychain read can block on a
/// system prompt; nothing here touches AppKit). The main thread applies it.
#[derive(Clone)]
struct LoadOutcome {
    /// The storage generation the decision was taken under. A purge or a storage rebuild bumps it,
    /// and a result from before that must not be applied: it would restore records the user asked
    /// to delete and re-enable writing over the purge's work.
    generation: u64,
    decision: LoadDecision,
}

#[derive(Clone)]
enum LoadDecision {
    /// Key and parsed entries, ready to merge.
    Entries {
        key: MasterKey,
        entries: Vec<ClipEntry>,
    },
    /// Nothing on disk: start empty with this key.
    Empty { key: MasterKey },
    /// No key this session.
    Unavailable(KeyUnavailable),
    /// Data present but unreadable, or a purge unfinished.
    Blocked(BlockedReason),
}

fn outcome(generation: u64, decision: LoadDecision) -> LoadOutcome {
    LoadOutcome {
        generation,
        decision,
    }
}

/// Load the persisted history and MERGE it into the in-memory history. The entry point also
/// finishes a purge an earlier session could not complete, and it is the only place that decides
/// the session's storage state.
///
/// The decision runs on a worker thread and the merge is applied on the main thread: the keychain
/// read may block on a system authorization prompt, and the merge may regenerate previews (AppKit).
/// Test and smoke runs (no run loop to marshal into) run both steps inline.
/// Retry the load after the user asked for keychain access (the settings banner's and the panel's
/// "grant access" button). The read happens on the clipboard worker like the first load, so the
/// system's authorization prompt never blocks the main thread; a retry is refused while another is
/// still in flight, so holding the button down cannot spawn threads.
pub(super) fn retry_load_history() {
    log_info!("[clip] keychain access requested by the user; reloading the history");
    load_history();
}

/// Set while a load is running, by every entry point. Cleared when the outcome is applied, and by
/// `load_history` itself when the worker could not be spawned -- otherwise a failed spawn would leave
/// the button dead for the rest of the session.
/// Whether a load is running and whether one was asked for meanwhile -- behind ONE lock, because
/// "am I busy?" and "record my request" have to be a single handover: as two separate atomics, a
/// request could register after the finishing task had already looked at the queue, leaving no task
/// running and nobody to consume the flag (the storage would stay `Uninitialized`, recording with
/// every save refused and nothing on screen to say so).
#[derive(Default)]
struct LoadGate {
    in_flight: bool,
    requested: bool,
}

static LOAD_GATE: Mutex<LoadGate> = Mutex::new(LoadGate {
    in_flight: false,
    requested: false,
});

/// Take the load slot, or record the request on the attempt that holds it.
fn begin_load() -> Result<(), bool> {
    let mut gate = LOAD_GATE.lock().unwrap();
    if gate.in_flight {
        gate.requested = true;
        return Err(true);
    }
    gate.in_flight = true;
    Ok(())
}

/// Release the load slot and report whether a request was waiting for it. The caller then decides
/// (outside the lock) whether that request still needs running.
fn release_load() -> bool {
    let mut gate = LOAD_GATE.lock().unwrap();
    gate.in_flight = false;
    std::mem::take(&mut gate.requested)
}

/// The storage generation a load outcome has been applied for (`u64::MAX` = none yet). A generation
/// decides for itself: a *stale* outcome must not mark the current one as decided, and a new
/// generation (the feature toggled off and on again) must be able to ask for a load again.
static LOAD_DECIDED_GENERATION: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(u64::MAX);

/// Release the in-flight guard and, when a load was requested meanwhile and the CURRENT storage
/// generation still has no decision, run it now. Called from every path that ends a load attempt.
fn finish_load_attempt() {
    if !release_load() {
        return;
    }
    if LOAD_DECIDED_GENERATION.load(Ordering::SeqCst) != storage::generation()
        && CONFIG.read().unwrap().clipboard.enabled
    {
        log_info!("[clip] running the load that was requested while another was in flight");
        load_history();
    }
}

pub(super) fn load_history() {
    if begin_load().is_err() {
        log_debug!("[clip] a load is already in flight; queueing this request behind it");
        return;
    }
    if is_test_process() {
        let outcome = decide_load();
        apply_load_outcome(outcome);
        return;
    }
    let spawned = thread::Builder::new()
        .name("oh-my-tab-clipboard-load".to_string())
        .spawn(|| {
            log_debug!("[clip] load worker started");
            let outcome = decide_load();
            // The merge needs the key to read the cache, but the session must NOT become writable
            // before it: a copy recorded in that window would save an empty snapshot over the file
            // the merge is about to read.
            // Only a fresh decision may install a key: a stale one belongs to storage that no
            // longer exists.
            if outcome.generation == storage::generation() {
                if let Some(key) = outcome_key(&outcome) {
                    storage::hold_key(key);
                }
            }
            if !store_pending_load(outcome) {
                return;
            }
            unsafe {
                let target = observer();
                let _: () = msg_send![
                    target,
                    performSelectorOnMainThread: sel!(applyClipboardLoadOnMain:),
                    withObject: std::ptr::null::<AnyObject>(),
                    waitUntilDone: false
                ];
            }
        });
    if spawned.is_err() {
        // Cannot spawn (extremely rare). Running the load here would put the keychain read back on
        // the caller's thread — the very thing this split exists to avoid — so the session stays
        // not-writable and says so: nothing is saved this session.
        log_info!(
            "Clipboard load thread could not start; history stays unloaded and nothing is saved."
        );
        storage::set_unavailable(KeyUnavailable::Other);
        // Release the in-flight guard: a failed spawn must not leave the grant button dead.
        finish_load_attempt();
    }
}

/// Repaint the settings window's permission rows (the About page's keychain state in particular).
/// A keychain grant can be silent and produces no activation event, and a load can land while the
/// window is open, so both paths call this from the main thread.
fn refresh_settings_permission_ui() {
    if crate::is_main_thread() {
        crate::settings::refresh_permission_ui_if_visible();
        return;
    }
    // Same marshalling as the picker refresh: the settings window is main-thread only.
    unsafe {
        let _: () = msg_send![
            observer(),
            performSelectorOnMainThread: sel!(refreshPermissionUI:),
            withObject: std::ptr::null::<AnyObject>(),
            waitUntilDone: false
        ];
    }
}

/// The feature switch is off but the user asked for keychain access (the About row): acquire the key
/// so the system prompt happens and the row can report "granted", WITHOUT loading, migrating, sweeping
/// or saving anything -- the master switch owns that boundary.
pub(super) fn acquire_key_only() {
    if begin_load().is_err() {
        log_debug!("[clip] a load is already in flight; queueing this authorization request");
        return;
    }
    let finish = |result: Result<MasterKey, KeyUnavailable>| {
        match result {
            Ok(key) => {
                storage::hold_key_only(key);
                log_info!("[clip] keychain access granted while the feature is off; key held");
                // A silent grant produces no activation event, so the About row would keep saying
                // "needs authorization" until the page was switched. Reflect it from the main thread.
                refresh_settings_permission_ui();
            }
            Err(reason) => log_info!(
                "[clip] keychain access request failed while the feature is off: {}",
                keyring::unavailable_label(&reason)
            ),
        }
        finish_load_attempt();
    };
    if is_test_process() {
        finish(keyring::acquire(true));
        return;
    }
    let spawned = thread::Builder::new()
        .name("oh-my-tab-clipboard-key".to_string())
        .spawn(move || finish(keyring::acquire(true)));
    if spawned.is_err() {
        finish_load_attempt();
        log_info!("[clip] keychain worker could not start; the feature stays off");
    }
}

/// The outcome waiting to be applied on the main thread (plain data, no pointers).
static PENDING_LOAD: Mutex<Option<LoadOutcome>> = Mutex::new(None);

/// Put a result in the single pending slot, keeping the NEWEST one: an older result arriving later
/// must not displace it, or the marshalled apply would pick up the stale one, discard it, and lose
/// the load that was actually current. Returns whether the result was stored.
fn store_pending_load(outcome: LoadOutcome) -> bool {
    let mut slot = PENDING_LOAD.lock().unwrap();
    let replace = match &*slot {
        Some(existing) => outcome.generation >= existing.generation,
        None => true,
    };
    if replace {
        *slot = Some(outcome);
    } else {
        log_info!("Clipboard load result dropped: a newer load is already waiting.");
    }
    replace
}

fn outcome_key(outcome: &LoadOutcome) -> Option<MasterKey> {
    match &outcome.decision {
        LoadDecision::Entries { key, .. } | LoadDecision::Empty { key } => Some(key.clone()),
        _ => None,
    }
}

/// Main-thread entry for a finished load (see `load_history`).
pub(super) extern "C" fn apply_clipboard_load_on_main(
    _self: *mut c_void,
    _cmd: Sel,
    _note: *mut c_void,
) {
    crate::callback_guard::void("apply_clipboard_load_on_main", || {
        let outcome = PENDING_LOAD.lock().unwrap().take();
        if let Some(outcome) = outcome {
            apply_load_outcome(outcome);
        }
    });
}

/// Apply a finished load: set the session state, merge (AppKit preview regeneration happens here),
/// then let the UI catch up.
fn apply_load_outcome(outcome: LoadOutcome) {
    if outcome.generation != storage::generation() {
        // Stale: it decides nothing about the current generation, so it must not suppress the queued
        // load either (a disable/re-enable during a load ends exactly here).
        log_info!("Clipboard load result dropped: the storage was discarded while it was loading.");
        finish_load_attempt();
        return;
    }
    LOAD_DECIDED_GENERATION.store(outcome.generation, Ordering::SeqCst);
    finish_load_attempt();
    match outcome.decision {
        LoadDecision::Entries { key, entries } => {
            storage::set_ready(key);
            merge_loaded_entries(entries);
            // Same as the empty branch: anything recorded while the load was deciding must be
            // written now (the merge's own save already covers the merged history; this catches
            // the image originals that could not be written earlier).
            resubmit_pending_image_writes();
            save_history();
            // Warm up detail previews for restored image entries (background): after a restart
            // {hash}.detail may not exist yet, so generating ahead keeps the first open sharp.
            for entry in CLIP_HISTORY.lock().unwrap().iter() {
                if let Some(img) = &entry.image {
                    request_detail_preview(img, false);
                }
            }
        }
        LoadDecision::Empty { key } => {
            storage::set_ready(key);
            // Orphans from an earlier version are unreferenced now: the sweep is allowed because
            // the load succeeded (with nothing).
            let removed = sweep_current_clip_image_cache();
            if removed > 0 {
                log_debug!("[clip] swept {} orphan image cache files", removed);
            }
            // Entries recorded while the load was still deciding (the session was not writable
            // then) have to reach the disk now, or they vanish on the next restart.
            resubmit_pending_image_writes();
            save_history();
            log_info!("Clipboard history loaded (0 entries).");
        }
        LoadDecision::Unavailable(reason) => {
            log_info!(
                "Clipboard history not loaded this session (no key: {}).",
                keyring::unavailable_label(&reason)
            );
            storage::set_unavailable(reason);
            // An unavailable session records nothing, so whatever was recorded while the load was
            // still deciding must go too: keeping it would show entries next to a notice that says
            // the feature cannot be used, and its bytes could never reach the disk. Memory only --
            // the stored history is left byte-for-byte alone.
            clear_session_records();
            image_cache::discard_pending_writes();
        }
        LoadDecision::Blocked(reason) => {
            storage::set_blocked(reason);
            // Same rule as `Unavailable`: the panel shows the failure and renders no rows, so
            // session records must not stay behind them (they would still be selectable and
            // pasteable through the keyboard). The disk is not touched either way.
            clear_session_records();
            image_cache::discard_pending_writes();
        }
    }
    // The rows were built before the load landed; refresh them, and give the one-time storage
    // notice a chance if the picker is already up (the state may only be known now). Only the real
    // app has a UI to touch: the inline test/smoke path runs this on a worker thread, where the
    // picker would breach the main-thread runtime.
    if crate::is_main_thread() {
        // The failure must reach the user even if the picker is never opened: a system notification
        // Once per session for the system notification; the settings banner and the panel's own
        // notice are re-derived from the state instead of being announced once.
        notify_storage_failure_once();
        unsafe { crate::settings::refresh_clipboard_unavailable_notice() };
        // The About page's keychain row reads the same state and must follow it too, without waiting
        // for a page switch or an app activation.
        refresh_settings_permission_ui();
        schedule_picker_refresh();
    }
}

/// The off-main decision (see `load_history`). No AppKit, no UI, no writes.
fn decide_load() -> LoadOutcome {
    // A purge the user asked for comes first: loading now would show records they deleted. It runs
    // BEFORE the generation is captured, because a purge that completes here bumps it — this load
    // belongs to the storage as it is after that.
    if storage::process_pending_purge() == storage::PurgeProgress::StillPending {
        return outcome(
            storage::generation(),
            LoadDecision::Blocked(BlockedReason::PurgePending),
        );
    }
    // Everything below belongs to this generation; a later purge bumps it and invalidates the
    // result.
    let generation = storage::generation();
    // An isolation that was interrupted is finished before anything is read or swept: the set has
    // to be moved aside as a whole, or a fresh start would sweep what was not yet isolated.
    if matches!(storage::path_present(&isolation_marker_path()), Some(true)) {
        log_info!("Clipboard storage isolation was interrupted; finishing it.");
        if !resume_isolation(generation) {
            return outcome(generation, LoadDecision::Blocked(BlockedReason::Damaged));
        }
    }
    let encrypted = history_file_path();
    match storage::path_present(&encrypted) {
        Some(true) => return load_encrypted_history(&encrypted, generation),
        // Cannot tell whether an index is there: never read that as "no history" (a fresh start
        // would sweep the cache it references and a later write would replace it).
        None => {
            log_info!("Clipboard history index cannot be inspected; left untouched.");
            return outcome(generation, LoadDecision::Blocked(BlockedReason::Damaged));
        }
        Some(false) => {}
    }
    // No encrypted index: the only other source is the legacy plaintext one, and it is only read
    // when the encrypted file does not exist at all (a damaged `.enc` must never fall back to a
    // stale plaintext file next to it).
    let legacy = legacy_history_file_path();
    match storage::path_present(&legacy) {
        Some(true) => migrate_legacy_history(&legacy, generation),
        None => {
            log_info!("Clipboard legacy history cannot be inspected; left untouched.");
            outcome(generation, LoadDecision::Blocked(BlockedReason::Damaged))
        }
        Some(false) => empty_outcome(generation),
    }
}

/// The session starts with no history on disk (first run, or after an isolated storage set).
fn empty_outcome(generation: u64) -> LoadOutcome {
    match keyring::acquire(true) {
        Ok(key) => outcome(generation, LoadDecision::Empty { key }),
        Err(reason) => outcome(generation, LoadDecision::Unavailable(reason)),
    }
}

fn load_encrypted_history(path: &std::path::Path, generation: u64) -> LoadOutcome {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        // Vanished between the existence check and the read: treat it as a first run.
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return empty_outcome(generation)
        }
        // Unreadable (permissions, I/O): NOT "no history". Starting empty would allow the sweep to
        // delete the images this index still references and the rewrite to replace it.
        Err(_) => {
            log_info!("Clipboard history file is unreadable; left untouched.");
            return outcome(generation, LoadDecision::Blocked(BlockedReason::Damaged));
        }
    };
    // `allow_create = false`: encrypted data exists, so invariant R1 forbids creating a key
    // here — a new key would make the existing file permanently unreadable.
    match keyring::acquire(false) {
        Ok(key) => {
            // The cache verification below reads through `storage::key()`, so the key has to be
            // installed first. `hold_key` deliberately does NOT make the session writable: the
            // merge still happens on the main thread, after this returns.
            storage::hold_key(key.clone());
            let object_id = object_id_for(HISTORY_LOGICAL_NAME);
            match open(&key, ObjectKind::History, &object_id, &bytes) {
                Ok(plaintext) => {
                    let Ok(text) = String::from_utf8(plaintext) else {
                        return outcome(generation, LoadDecision::Blocked(BlockedReason::Damaged));
                    };
                    let entries = match parse_history_classified(&text) {
                        ParsedHistory::Ok(entries) => entries,
                        ParsedHistory::UnsupportedVersion => {
                            log_info!("Clipboard history load failed (version newer than this build); left untouched.");
                            return outcome(
                                generation,
                                LoadDecision::Blocked(BlockedReason::UnsupportedVersion),
                            );
                        }
                        ParsedHistory::Corrupt => {
                            log_info!("Clipboard history load failed (corrupt); left untouched.");
                            return outcome(
                                generation,
                                LoadDecision::Blocked(BlockedReason::Damaged),
                            );
                        }
                    };
                    if !referenced_cache_files_ok(&entries) {
                        log_info!("Clipboard history left untouched: referenced cache files did not authenticate.");
                        return outcome(generation, LoadDecision::Blocked(BlockedReason::Damaged));
                    }
                    // A legacy plaintext index next to a healthy encrypted one is superseded: its
                    // content is not read, and leaving plaintext on disk is exactly what this
                    // feature removes. The removal is VERIFIED: while it fails, the store is not
                    // reported as clean, and the next load retries.
                    let legacy = legacy_history_file_path();
                    if plaintext_cleanup_incomplete(&legacy) {
                        log_info!(
                            "Clipboard history is encrypted, but the plaintext cleanup is incomplete; it stays resumable."
                        );
                        return outcome(
                            generation,
                            LoadDecision::Blocked(BlockedReason::MigrationIncomplete),
                        );
                    }
                    outcome(generation, LoadDecision::Entries { key, entries })
                }
                Err(OpenError::ForeignKey { key_id }) => {
                    log_info!(
                        "Clipboard history belongs to another key (key_id {:02x}{:02x}..); left untouched.",
                        key_id[0],
                        key_id[1]
                    );
                    outcome(generation, LoadDecision::Blocked(BlockedReason::ForeignKey))
                }
                Err(_) => {
                    log_info!("Clipboard history failed to authenticate; left untouched.");
                    outcome(generation, LoadDecision::Blocked(BlockedReason::Damaged))
                }
            }
        }
        Err(KeyUnavailable::Missing) => {
            // The key is gone for good while encrypted data exists: unrecoverable by design.
            // Isolate the WHOLE storage set (file and cache together) so the fresh start's sweep
            // cannot delete it, then start over with a new key.
            log_info!(
                "Clipboard history key is missing; isolating the storage set and starting fresh."
            );
            if !isolate_storage_set(generation) {
                log_info!("Clipboard storage could not be isolated; leaving it untouched.");
                return outcome(generation, LoadDecision::Blocked(BlockedReason::Damaged));
            }
            empty_outcome(generation)
        }
        Err(reason) => outcome(generation, LoadDecision::Unavailable(reason)),
    }
}

/// Remove a superseded plaintext index and any leftover temp file, then VERIFY: `true` when
/// something survived. Both load paths share this, so a plaintext leftover always keeps the state
/// honest instead of being reported as a healthy encrypted store.
fn plaintext_cleanup_incomplete(legacy: &std::path::Path) -> bool {
    if matches!(storage::path_present(legacy), Some(true)) {
        remove_legacy_index(legacy);
    }
    let leftovers = sweep_history_temp_files(legacy);
    matches!(storage::path_present(legacy), Some(true))
        || legacy_index_leftover()
        || !leftovers.is_empty()
}

/// Every referenced cache file must either open or be absent. A file that is PRESENT but does not
/// authenticate must never become a dropped entry: the sweep would then delete a file that may
/// still be recoverable, and the rewrite would drop its last reference.
fn referenced_cache_files_ok(entries: &[ClipEntry]) -> bool {
    let mut ok = true;
    for entry in entries {
        let Some(img) = &entry.image else {
            continue;
        };
        if img.hash == 0 {
            continue;
        }
        if img.source_path.is_none() && cache_image_state(img.hash) == CacheFileState::Failed {
            log_info!(
                "Clipboard image cache file is present but unreadable ({})",
                image_logical_name(img.hash)
            );
            ok = false;
        }
        if cache_preview_state(img.hash) == CacheFileState::Failed {
            log_info!(
                "Clipboard image preview is present but unreadable ({}.preview)",
                image_logical_name(img.hash)
            );
            ok = false;
        }
    }
    ok
}

/// Set when a superseded plaintext index could not be removed: the promise "no plaintext on disk"
/// is then not met, and the state has to say so instead of reporting a clean encrypted store.
static LEGACY_INDEX_LEFTOVER: AtomicBool = AtomicBool::new(false);

/// Whether a superseded plaintext index survived this session (reported in `--e2e-state`).
pub(super) fn legacy_index_leftover() -> bool {
    LEGACY_INDEX_LEFTOVER.load(Ordering::SeqCst)
}

/// Remove a superseded plaintext index, VERIFYING the result: a plaintext copy that stays behind is
/// reported (and retried on the next load) instead of being silently ignored.
fn remove_legacy_index(path: &std::path::Path) {
    match std::fs::remove_file(path) {
        Ok(()) => LEGACY_INDEX_LEFTOVER.store(false, Ordering::SeqCst),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            LEGACY_INDEX_LEFTOVER.store(false, Ordering::SeqCst)
        }
        Err(_) => {
            log_info!(
                "Clipboard plaintext history index could not be removed; it stays on disk and is retried."
            );
            LEGACY_INDEX_LEFTOVER.store(true, Ordering::SeqCst);
        }
    }
}

/// Durable record that the storage set is being moved aside: the move takes two steps (the cache
/// directory, then the index files), and a crash between them must not make the next launch start
/// empty next to a cache it would then sweep.
fn isolation_marker_path() -> std::path::PathBuf {
    history_dir().join("clipboard-history.isolating")
}

/// Move the history file(s) and the image cache aside so a fresh start cannot reach them. The
/// names never overwrite an existing file, and nothing is deleted (R3). The intent is recorded
/// first so an interrupted isolation is recognizable, and a partial move is rolled back.
fn isolate_storage_set(generation: u64) -> bool {
    // The isolation moves files aside, so it takes the same locks the purge and the migration take,
    // and it re-checks the generation inside them: a result that became stale while it waited must
    // not touch the storage a purge or a newer load is responsible for.
    let _io = PERSIST_IO_LOCK.lock().unwrap();
    let _cache = image_cache::cache_write_lock();
    if generation != storage::generation() {
        log_info!("Clipboard storage isolation abandoned: the storage generation moved.");
        return false;
    }
    let marker = isolation_marker_path();
    if let Some(dir) = marker.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if std::fs::write(&marker, b"isolating\n").is_err() {
        // Without the record, a crash mid-move would be indistinguishable from "no history".
        log_info!("Clipboard storage cannot be isolated: the marker is not writable.");
        return false;
    }
    finish_isolation()
}

/// Finish an isolation a crash interrupted. It moves files, so it goes through the SAME guards as a
/// fresh isolation: the shared locks (purge / migration order) and the generation check. An old load
/// that finds the marker must not rename the storage a newer generation owns.
fn resume_isolation(generation: u64) -> bool {
    let _io = PERSIST_IO_LOCK.lock().unwrap();
    let _cache = image_cache::cache_write_lock();
    if generation != storage::generation() {
        log_info!("Clipboard isolation resume abandoned: the storage generation moved.");
        return false;
    }
    finish_isolation()
}

/// The move itself (see `isolate_storage_set`).
fn finish_isolation() -> bool {
    let cache = clip_image_cache_dir();
    let mut moved_cache: Option<std::path::PathBuf> = None;
    if matches!(storage::path_present(&cache), Some(true)) {
        match rename_aside(&cache) {
            Some(path) => moved_cache = Some(path),
            None => return false, // nothing else has been touched
        }
    }
    for path in [history_file_path(), legacy_history_file_path()] {
        if !matches!(storage::path_present(&path), Some(true)) {
            continue;
        }
        if rename_aside(&path).is_none() {
            // Roll the cache back, so the next launch sees the state this one saw instead of
            // "no index" next to a cache it would sweep.
            if let Some(moved) = &moved_cache {
                let _ = std::fs::rename(moved, &cache);
            }
            return false;
        }
    }
    // Only a verified revocation counts: a marker left behind would make the next launch isolate the
    // history written from now on.
    match std::fs::remove_file(isolation_marker_path()) {
        Ok(()) => true,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => true,
        Err(_) => {
            log_info!("Clipboard isolation marker could not be removed; staying blocked.");
            false
        }
    }
}

/// Rename `path` to `<path>.failed-<unix seconds>`, adding a numeric suffix when that name is
/// taken. Returns the new path.
fn rename_aside(path: &std::path::Path) -> Option<std::path::PathBuf> {
    let stamp = now_secs();
    let base = format!("{}.failed-{stamp}", path.display());
    for attempt in 0..100 {
        let candidate = if attempt == 0 {
            std::path::PathBuf::from(&base)
        } else {
            std::path::PathBuf::from(format!("{base}-{attempt}"))
        };
        if candidate.exists() {
            continue;
        }
        if std::fs::rename(path, &candidate).is_ok() {
            log_info!("Clipboard storage kept aside at {}", candidate.display());
            return Some(candidate);
        }
        return None;
    }
    None
}

/// Merge loaded entries into the in-memory history (dedup by kind, pinned first, trim to
/// max_entries), then sweep orphans and rewrite so disk and memory agree.
fn merge_loaded_entries(entries: Vec<ClipEntry>) {
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
    let swept = if storage::sweep_allowed() {
        sweep_clip_image_cache(&hist)
    } else {
        0
    };
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

/// Migrate the legacy plaintext history: re-seal every referenced cache file, then commit the
/// encrypted index, and only then delete the plaintext one. Idempotent and resumable: a crash
/// anywhere re-runs from the top, and files that already authenticate are skipped. Any file
/// that cannot be converted blocks the commit — the normal sweep keeps previews of surviving
/// entries, so a leftover plaintext preview would stay in the active cache while the UI
/// claimed the history was encrypted.
fn migrate_legacy_history(legacy: &std::path::Path, generation: u64) -> LoadOutcome {
    let text = match std::fs::read_to_string(legacy) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return empty_outcome(generation)
        }
        // Unreadable, or not valid UTF-8: never treat that as "no history" (the sweep would delete
        // the images it references).
        Err(_) => {
            log_info!("Clipboard legacy history is unreadable; left untouched.");
            return outcome(generation, LoadDecision::Blocked(BlockedReason::Damaged));
        }
    };
    let entries = match parse_history_classified(&text) {
        ParsedHistory::Ok(entries) => entries,
        ParsedHistory::UnsupportedVersion => {
            log_info!("Clipboard legacy history is newer than this build; left untouched.");
            return outcome(
                generation,
                LoadDecision::Blocked(BlockedReason::UnsupportedVersion),
            );
        }
        ParsedHistory::Corrupt => {
            log_info!("Clipboard legacy history is corrupt; left untouched.");
            return outcome(generation, LoadDecision::Blocked(BlockedReason::Damaged));
        }
    };
    // Encrypted cache files may exist from an interrupted earlier migration: creating a new key
    // would orphan them, so creation is only allowed when nothing is sealed yet.
    let allow_create = !any_sealed_cache_file();
    match keyring::acquire(allow_create) {
        Ok(key) => {
            // The conversion and the commit share the purge's I/O lock (and the cache lock, taken in
            // the same order the purge takes them): a purge that ran in between would delete the
            // files this migration is about to re-seal, and the migration would put them back.
            let _io = PERSIST_IO_LOCK.lock().unwrap();
            let _cache = image_cache::cache_write_lock();
            let mut failures = 0usize;
            for entry in &entries {
                let Some(img) = &entry.image else {
                    continue;
                };
                if img.hash == 0 {
                    continue;
                }
                let name = image_logical_name(img.hash);
                if img.source_path.is_none() {
                    failures += usize::from(matches!(
                        convert_cache_file(
                            &clip_image_path(img.hash),
                            ObjectKind::ImageData,
                            &name,
                            &key
                        ),
                        Convert::Failed
                    ));
                }
                failures += usize::from(matches!(
                    convert_cache_file(
                        &clip_image_preview_path(img.hash),
                        ObjectKind::Thumbnail,
                        &format!("{name}.preview"),
                        &key
                    ),
                    Convert::Failed
                ));
                failures += usize::from(matches!(
                    convert_cache_file(
                        &clip_image_detail_path(img.hash),
                        ObjectKind::Detail,
                        &format!("{name}.detail"),
                        &key
                    ),
                    Convert::Failed
                ));
            }
            if failures > 0 {
                log_info!(
                    "Clipboard migration incomplete ({} cache files could not be converted); nothing was committed.",
                    failures
                );
                return outcome(
                    generation,
                    LoadDecision::Blocked(BlockedReason::MigrationIncomplete),
                );
            }
            // Re-verified INSIDE the lock: a purge either ran before it (its generation is older,
            // so this aborts) or is waiting for it (and will delete what was just committed).
            if generation != storage::generation() {
                log_info!("Clipboard migration abandoned: the storage was discarded while it ran.");
                return outcome(
                    generation,
                    LoadDecision::Blocked(BlockedReason::PurgePending),
                );
            }
            // Commit: publish the encrypted index first (verified), then remove the plaintext.
            let Some(serialized) = serialize_history(&entries) else {
                return outcome(
                    generation,
                    LoadDecision::Blocked(BlockedReason::MigrationIncomplete),
                );
            };
            let dir = history_dir();
            if !write_sealed_file(
                &dir,
                "clipboard-history.enc",
                HISTORY_LOGICAL_NAME,
                ObjectKind::History,
                &key,
                serialized.as_bytes(),
            ) {
                log_info!("Clipboard migration could not publish the encrypted index; nothing was deleted.");
                return outcome(
                    generation,
                    LoadDecision::Blocked(BlockedReason::MigrationIncomplete),
                );
            }
            // The plaintext cleanup is part of the promise: while a plaintext index or a leftover
            // temp file survives, the migration is NOT complete and must not report as such.
            if plaintext_cleanup_incomplete(legacy) {
                log_info!(
                    "Clipboard migration committed, but the plaintext cleanup is incomplete; it stays resumable."
                );
                return outcome(
                    generation,
                    LoadDecision::Blocked(BlockedReason::MigrationIncomplete),
                );
            }
            log_info!("Clipboard history migrated to the encrypted format.");
            outcome(generation, LoadDecision::Entries { key, entries })
        }
        Err(KeyUnavailable::Missing) => {
            // Partially migrated data without a key: unrecoverable by design. Isolate the whole
            // set (file + cache) and start fresh, never deleting anything.
            log_info!("Clipboard migration found no key with sealed data present; isolating and starting fresh.");
            if !isolate_storage_set(generation) {
                return outcome(generation, LoadDecision::Blocked(BlockedReason::Damaged));
            }
            empty_outcome(generation)
        }
        Err(reason) => outcome(generation, LoadDecision::Unavailable(reason)),
    }
}

/// Whether any cache file is already sealed (used to forbid key creation mid-migration).
fn any_sealed_cache_file() -> bool {
    let dir = clip_image_cache_dir();
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return false;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        if let Ok(bytes) = std::fs::read(&path) {
            if looks_like_envelope(&bytes) {
                return true;
            }
        }
    }
    false
}

enum Convert {
    Converted,
    Absent,
    Failed,
}

/// Convert one cache file in place: a plaintext file is sealed and atomically replaced (temp
/// file, read back and authenticate, rename); a sealed file is only accepted when it
/// authenticates with the expected kind and object id — the magic alone is not proof.
fn convert_cache_file(
    path: &std::path::Path,
    kind: ObjectKind,
    logical_name: &str,
    key: &MasterKey,
) -> Convert {
    let Ok(bytes) = std::fs::read(path) else {
        return if path.exists() {
            Convert::Failed
        } else {
            Convert::Absent
        };
    };
    let object_id = object_id_for(logical_name);
    if looks_like_envelope(&bytes) {
        return if open(key, kind, &object_id, &bytes).is_ok() {
            Convert::Converted
        } else {
            Convert::Failed
        };
    }
    let Some(dir) = path.parent() else {
        return Convert::Failed;
    };
    let Some(file_name) = path.file_name().and_then(|n| n.to_str()) else {
        return Convert::Failed;
    };
    if write_sealed_file(dir, file_name, logical_name, kind, key, &bytes) {
        Convert::Converted
    } else {
        Convert::Failed
    }
}

/// The atomic-write temp file name pattern: `<history file name>.tmp<pid>-<generation>`. A
/// process that dies between the temp write and the rename leaves one behind -- with the full
/// history in it -- so the cleanup paths have to recognize and remove them.
/// Pure, unit-tested: strict, so an unrelated file in the same directory is never touched.
pub(super) fn is_history_temp_file_name(name: &str) -> bool {
    for base in ["clipboard-history.enc", "clipboard-history.toml"] {
        let Some(rest) = name.strip_prefix(base) else {
            continue;
        };
        let Some(rest) = rest.strip_prefix(".tmp") else {
            continue;
        };
        let Some((pid, generation)) = rest.split_once('-') else {
            return false;
        };
        return !pid.is_empty()
            && !generation.is_empty()
            && pid.bytes().all(|b| b.is_ascii_digit())
            && generation.bytes().all(|b| b.is_ascii_digit());
    }
    false
}

/// Remove leftover atomic-write temp files from the directory holding the history file. They
/// hold the same history, so a crash must not leave them for the next session to ignore.
fn sweep_history_temp_files(history_path: &std::path::Path) -> Vec<std::path::PathBuf> {
    let mut remaining = Vec::new();
    let Some(dir) = history_path.parent() else {
        return remaining;
    };
    let Ok(entries) = std::fs::read_dir(dir) else {
        // Cannot even look: report the directory as unverified so a purge does not claim success.
        remaining.push(dir.to_path_buf());
        return remaining;
    };
    let mut removed = 0usize;
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        if !is_history_temp_file_name(name) {
            continue;
        }
        let path = entry.path();
        match std::fs::remove_file(&path) {
            Ok(()) => removed += 1,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => remaining.push(path),
        }
    }
    if removed > 0 {
        log_info!("Removed {removed} leftover clipboard history temp file(s).");
    }
    remaining
}

/// Delete the history file and the image cache, VERIFYING the result: the paths that are still
/// there come back to the caller, which must report the failure instead of claiming success
/// (the old implementation ignored every error). Invariant R6.
pub(super) fn discard_history_verified() -> Vec<std::path::PathBuf> {
    let generation = PERSIST_GENERATION.fetch_add(1, Ordering::AcqRel) + 1;
    // Same lock as the writer: the delete waits out a write in flight, and a snapshot prepared
    // before it cannot land afterwards.
    let _io = PERSIST_IO_LOCK.lock().unwrap();
    let mut remaining = discard_history_in(&history_file_path(), &clip_image_cache_dir());
    remaining.extend(discard_history_in(
        &legacy_history_file_path(),
        &clip_image_cache_dir(),
    ));
    remaining.sort();
    remaining.dedup();
    // A discard invalidates the queued snapshots instead of running them, so the worker's
    // "drained" watermark moves with the generation: nothing is pending for it.
    PERSIST_DONE_GENERATION.fetch_max(generation, Ordering::AcqRel);
    remaining
}

/// Delete a history file and wipe a cache directory, returning whatever survived.
/// Parameterized for the test, which must not disturb the shared directories the rest of the
/// suite writes to.
pub(super) fn discard_history_in(
    history_path: &std::path::Path,
    cache_dir: &std::path::Path,
) -> Vec<std::path::PathBuf> {
    let mut remaining = Vec::new();
    match std::fs::remove_file(history_path) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(_) => remaining.push(history_path.to_path_buf()),
    }
    // A crash between the temp write and the rename leaves the whole history in a temp file.
    remaining.extend(sweep_history_temp_files(history_path));
    // Invalidates the queued image work and waits out a write in flight, so once this returns no
    // job for the discarded history can put its files back (`wipe_cache_for_discard`).
    wipe_cache_for_discard(cache_dir);
    match std::fs::read_dir(cache_dir) {
        Ok(entries) => {
            for entry in entries.flatten() {
                if entry.path().is_file() {
                    remaining.push(entry.path());
                }
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(_) => {
            // Cannot enumerate the cache: the deletion cannot be verified, so it must not be
            // reported as complete.
            remaining.push(cache_dir.to_path_buf());
        }
    }
    remaining
}

/// Delete every trace of the history: the file and the image cache. Used when the clipboard
/// switch is turned off and when `clear_on_quit` fires, so both paths share one implementation.
/// The key is KEPT (decision 3), and the deletion runs as a purge transaction so a failure is
/// recorded, retried and reported instead of being swallowed (invariant R6).
pub(crate) fn discard_history_on_disk() {
    match storage::request_full_purge() {
        PurgeOutcome::Done => log_info!("Clipboard history file and image cache removed."),
        PurgeOutcome::Deferred => log_info!(
            "Clipboard purge deferred: some files could not be removed; it stays pending and will be retried."
        ),
        PurgeOutcome::NotAccepted => log_info!(
            "Clipboard purge not accepted: no intent could be recorded, so nothing was deleted."
        ),
    }
}

/// Test-only: build and apply a load result that belongs to an older storage generation (a purge
/// or a storage rebuild bumped it in between). It must be dropped.
#[cfg(test)]
pub(super) fn apply_stale_load_for_tests() {
    let generation = storage::generation();
    let decision = LoadDecision::Empty {
        key: keyring::test_key(),
    };
    // In isolation: a queued load request would legitimately re-run a load after the stale outcome is
    // dropped, and that is a different scenario (see
    // `a_queued_load_survives_a_stale_outcome_and_a_new_generation`).
    release_load();
    storage::bump_generation();
    storage::set_unavailable(KeyUnavailable::Missing);
    apply_load_outcome(LoadOutcome {
        generation,
        decision,
    });
}

#[cfg(test)]
pub(super) mod test_support {
    use super::*;

    /// Write a sealed history file for tests (the test key is the one the session uses under
    /// `cfg(test)`).
    pub(in crate::clipboard) fn write_sealed_history(entries: &[ClipEntry]) -> bool {
        let key = keyring::test_key();
        let Some(text) = serialize_history(entries) else {
            return false;
        };
        let dir = history_dir();
        write_sealed_file(
            &dir,
            "clipboard-history.enc",
            HISTORY_LOGICAL_NAME,
            ObjectKind::History,
            &key,
            text.as_bytes(),
        )
    }

    /// Read the raw bytes of the live history file (for "no plaintext on disk" assertions).
    pub(in crate::clipboard) fn read_history_bytes() -> Option<Vec<u8>> {
        std::fs::read(history_file_path()).ok()
    }
}

#[cfg(test)]
mod encryption_tests {
    use super::*;

    /// The tests here touch the shared cache/history directories and the process-global storage
    /// state, so they take the same lock as the rest of the clipboard suite.
    fn guard() -> std::sync::MutexGuard<'static, ()> {
        let guard = storage::test_lock();
        storage::set_ready(keyring::test_key());
        guard
    }

    fn clean_disk() {
        let _ = std::fs::remove_file(history_file_path());
        let _ = std::fs::remove_file(legacy_history_file_path());
        let dir = history_dir();
        if let Ok(entries) = std::fs::read_dir(&dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_file() {
                    let _ = std::fs::remove_file(&path);
                } else {
                    let _ = std::fs::remove_dir_all(&path);
                }
            }
        }
        let cache = clip_image_cache_dir();
        if let Ok(entries) = std::fs::read_dir(&cache) {
            for entry in entries.flatten() {
                let _ = std::fs::remove_file(entry.path());
            }
        }
        image_cache::clear_pending_state_for_tests();
        storage::clear_purge_not_accepted_for_tests();
        // Markers live outside the two directories above (the fallback is its own directory), and a
        // leftover one would make the next test's load run a purge first.
        let _ = std::fs::remove_file(storage::fallback_marker_path_for_tests());
        if let Some(dir) = storage::fallback_marker_path_for_tests().parent() {
            let _ = std::fs::remove_dir_all(dir);
        }
        let _ = std::fs::remove_file(isolation_marker_path());
        CLIP_HISTORY.lock().unwrap().clear();
    }

    fn text_entry(text: &str) -> ClipEntry {
        ClipEntry {
            text: text.to_string(),
            image: None,
            pinned: false,
            source_app: String::new(),
            source_key: String::new(),
            copied_at: Some(now_secs()),
        }
    }

    fn image_entry(hash: u64) -> ClipEntry {
        ClipEntry {
            text: String::new(),
            image: Some(ImageEntry {
                uti: "public.png".to_string(),
                hash,
                data_path: clip_image_path(hash),
                preview_png: Arc::new(Vec::new()),
                source_path: None,
            }),
            pinned: false,
            source_app: String::new(),
            source_key: String::new(),
            copied_at: Some(now_secs()),
        }
    }

    /// The core promise: the disk never holds the history in the clear.
    #[test]
    fn saving_never_puts_the_plaintext_history_on_disk() {
        let _guard = guard();
        clean_disk();
        let secret = "correct-horse-battery-staple";
        CLIP_HISTORY.lock().unwrap().push(text_entry(secret));
        save_history();
        flush_persist_worker_for_tests();
        let bytes = test_support::read_history_bytes().expect("history file");
        assert!(looks_like_envelope(&bytes), "the file must be sealed");
        assert!(
            !bytes.windows(secret.len()).any(|w| w == secret.as_bytes()),
            "the plaintext must not be recoverable from the file"
        );
        // ... and it round-trips through the reader.
        let key = keyring::test_key();
        let plain = open(
            &key,
            ObjectKind::History,
            &object_id_for(HISTORY_LOGICAL_NAME),
            &bytes,
        )
        .expect("open");
        assert!(String::from_utf8(plain).unwrap().contains(secret));
        clean_disk();
    }

    #[test]
    fn the_image_cache_files_are_sealed_and_object_bound() {
        let _guard = guard();
        clean_disk();
        let data = b"\x89PNG\r\n\x1a\n-original-bytes";
        let hash = fnv1a64(data);
        assert!(cache_write_image(hash, data));
        assert!(cache_write_preview(hash, b"preview-bytes"));
        assert!(cache_write_detail_preview(hash, b"detail-bytes"));
        for path in [
            clip_image_path(hash),
            clip_image_preview_path(hash),
            clip_image_detail_path(hash),
        ] {
            let bytes = std::fs::read(&path).expect("cache file");
            assert!(
                looks_like_envelope(&bytes),
                "{} must be sealed",
                path.display()
            );
            assert!(
                !bytes.windows(4).any(|w| w == b"\x89PNG"),
                "no PNG signature may survive in {}",
                path.display()
            );
        }
        // The three kinds are not interchangeable, and the reader recomputes the object id from
        // the name it asked for: a complete file moved to another hash's slot must not open.
        let other = fnv1a64(b"another-image");
        std::fs::write(
            clip_image_path(other),
            std::fs::read(clip_image_path(hash)).unwrap(),
        )
        .unwrap();
        assert_eq!(cache_read_image(other), None);
        assert_eq!(cache_read_image(hash).as_deref(), Some(&data[..]));
        assert_eq!(
            cache_read_preview(hash).as_deref(),
            Some(&b"preview-bytes"[..])
        );
        assert_eq!(
            cache_read_detail_preview(hash).as_deref(),
            Some(&b"detail-bytes"[..])
        );
        clean_disk();
    }

    /// Migration: plaintext history + plaintext cache -> sealed, and the plaintext index goes.
    #[test]
    fn migrating_a_legacy_history_seals_the_index_and_the_cache() {
        let _guard = guard();
        clean_disk();
        let data = b"\x89PNG\r\n\x1a\nlegacy-image";
        let hash = fnv1a64(data);
        // The legacy world: plaintext cache files written directly.
        std::fs::create_dir_all(clip_image_cache_dir()).unwrap();
        std::fs::write(clip_image_path(hash), data).unwrap();
        std::fs::write(clip_image_preview_path(hash), b"legacy-preview").unwrap();
        std::fs::write(clip_image_detail_path(hash), b"legacy-detail").unwrap();
        let legacy = legacy_history_file_path();
        std::fs::create_dir_all(legacy.parent().unwrap()).unwrap();
        std::fs::write(
            &legacy,
            serialize_history(&[image_entry(hash), text_entry("legacy text")]).unwrap(),
        )
        .unwrap();

        load_history();

        assert!(
            !legacy.exists(),
            "the plaintext index must be gone once the encrypted one is committed"
        );
        let bytes = test_support::read_history_bytes().expect("encrypted index");
        assert!(looks_like_envelope(&bytes));
        assert!(
            !bytes.windows(11).any(|w| w == b"legacy text"),
            "the migrated index must be sealed"
        );
        for (path, expected) in [
            (clip_image_path(hash), &data[..]),
            (clip_image_preview_path(hash), &b"legacy-preview"[..]),
            (clip_image_detail_path(hash), &b"legacy-detail"[..]),
        ] {
            let sealed = std::fs::read(&path).expect("cache file");
            assert!(
                looks_like_envelope(&sealed),
                "{} must be sealed",
                path.display()
            );
            assert!(!sealed.windows(4).any(|w| w == b"\x89PNG"));
            let _ = expected;
        }
        // The migrated entries are usable in this session.
        assert!(cache_read_image(hash).is_some());
        assert_eq!(CLIP_HISTORY.lock().unwrap().len(), 2);
        assert!(matches!(storage::state(), StorageState::Ready));
        clean_disk();
    }

    /// A damaged encrypted index blocks everything and must not fall back to a plaintext file
    /// lying next to it.
    #[test]
    fn a_damaged_index_blocks_and_leaves_both_files_untouched() {
        let _guard = guard();
        clean_disk();
        let encrypted = history_file_path();
        std::fs::create_dir_all(encrypted.parent().unwrap()).unwrap();
        let damaged = b"OMTCLIP\x01not-really-a-sealed-file".to_vec();
        std::fs::write(&encrypted, &damaged).unwrap();
        let legacy = legacy_history_file_path();
        let stale = serialize_history(&[text_entry("stale plaintext")]).unwrap();
        std::fs::write(&legacy, &stale).unwrap();

        load_history();

        assert!(matches!(
            storage::state(),
            StorageState::Blocked(BlockedReason::Damaged)
        ));
        assert_eq!(
            std::fs::read(&encrypted).unwrap(),
            damaged,
            "the damaged file stays"
        );
        assert_eq!(
            std::fs::read_to_string(&legacy).unwrap(),
            stale,
            "the plaintext file must never be used to overwrite anything"
        );
        assert!(CLIP_HISTORY.lock().unwrap().is_empty());
        // And a save attempt in this state writes nothing at all.
        CLIP_HISTORY
            .lock()
            .unwrap()
            .push(text_entry("must not be written"));
        save_history();
        flush_persist_worker_for_tests();
        assert_eq!(std::fs::read(&encrypted).unwrap(), damaged);
        clean_disk();
    }

    #[test]
    fn a_foreign_key_blocks_instead_of_replacing_the_key() {
        let _guard = guard();
        clean_disk();
        let foreign = MasterKey::generate().expect("key");
        let Some(text) = serialize_history(&[text_entry("foreign")]) else {
            panic!("serialize")
        };
        let sealed = seal(
            &foreign,
            ObjectKind::History,
            &object_id_for(HISTORY_LOGICAL_NAME),
            text.as_bytes(),
        )
        .expect("seal");
        let encrypted = history_file_path();
        std::fs::create_dir_all(encrypted.parent().unwrap()).unwrap();
        std::fs::write(&encrypted, &sealed).unwrap();

        load_history();

        assert!(matches!(
            storage::state(),
            StorageState::Blocked(BlockedReason::ForeignKey)
        ));
        assert_eq!(std::fs::read(&encrypted).unwrap(), sealed, "the file stays");
        clean_disk();
    }

    #[test]
    fn an_unsupported_version_blocks_without_rewriting() {
        let _guard = guard();
        clean_disk();
        let future = format!("version = {}\nentries = []\n", HISTORY_VERSION + 1);
        let key = keyring::test_key();
        let sealed = seal(
            &key,
            ObjectKind::History,
            &object_id_for(HISTORY_LOGICAL_NAME),
            future.as_bytes(),
        )
        .expect("seal");
        let encrypted = history_file_path();
        std::fs::create_dir_all(encrypted.parent().unwrap()).unwrap();
        std::fs::write(&encrypted, &sealed).unwrap();

        load_history();

        assert!(matches!(
            storage::state(),
            StorageState::Blocked(BlockedReason::UnsupportedVersion)
        ));
        assert_eq!(std::fs::read(&encrypted).unwrap(), sealed, "no rewrite");
        clean_disk();
    }

    /// A file that cannot be converted stops the migration before the commit: the plaintext
    /// index and every original stay, and the state says why.
    #[test]
    fn an_unconvertible_cache_file_blocks_the_migration_before_the_commit() {
        use std::os::unix::fs::PermissionsExt;
        let _guard = guard();
        clean_disk();
        let data = b"\x89PNG\r\n\x1a\nunreadable";
        let hash = fnv1a64(data);
        std::fs::create_dir_all(clip_image_cache_dir()).unwrap();
        std::fs::write(clip_image_path(hash), data).unwrap();
        std::fs::set_permissions(
            clip_image_path(hash),
            std::fs::Permissions::from_mode(0o000),
        )
        .unwrap();
        let legacy = legacy_history_file_path();
        std::fs::create_dir_all(legacy.parent().unwrap()).unwrap();
        let text = serialize_history(&[image_entry(hash)]).unwrap();
        std::fs::write(&legacy, &text).unwrap();

        load_history();

        assert!(matches!(
            storage::state(),
            StorageState::Blocked(BlockedReason::MigrationIncomplete)
        ));
        assert_eq!(
            std::fs::read_to_string(&legacy).unwrap(),
            text,
            "the index stays"
        );
        assert!(!history_file_path().exists(), "nothing may be committed");
        let _ = std::fs::set_permissions(
            clip_image_path(hash),
            std::fs::Permissions::from_mode(0o600),
        );
        clean_disk();
    }

    #[test]
    fn a_healthy_encrypted_index_removes_a_leftover_plaintext_one() {
        let _guard = guard();
        clean_disk();
        assert!(test_support::write_sealed_history(&[text_entry("live")]));
        let legacy = legacy_history_file_path();
        std::fs::create_dir_all(legacy.parent().unwrap()).unwrap();
        std::fs::write(&legacy, serialize_history(&[text_entry("stale")]).unwrap()).unwrap();

        load_history();

        assert!(
            !legacy.exists(),
            "the superseded plaintext index must be removed"
        );
        assert_eq!(CLIP_HISTORY.lock().unwrap().len(), 1);
        clean_disk();
    }

    /// The purge transaction: intent first, delete, verify, revoke — and only then writes again.
    #[test]
    fn a_purge_records_its_intent_and_revokes_it_after_the_deletion() {
        let _guard = guard();
        clean_disk();
        assert!(test_support::write_sealed_history(&[text_entry(
            "to be purged"
        )]));
        let data = b"image-to-purge";
        let hash = fnv1a64(data);
        assert!(cache_write_image(hash, data));

        let outcome = storage::request_full_purge();

        assert_eq!(outcome, PurgeOutcome::Done);
        assert!(!history_file_path().exists());
        assert_eq!(cache_read_image(hash), None);
        assert!(
            !storage::purge_intent_present(),
            "the intent must be revoked"
        );
        assert!(
            storage::writable(),
            "a completed purge leaves the session usable"
        );
        clean_disk();
    }

    /// The deletion is verified: when the files cannot go, the intent stays, writes stay off and
    /// the next launch retries.
    #[test]
    fn a_failed_purge_keeps_the_intent_and_blocks_writing() {
        use std::os::unix::fs::PermissionsExt;
        let _guard = guard();
        clean_disk();
        let dir = history_dir();
        std::fs::create_dir_all(&dir).unwrap();
        assert!(test_support::write_sealed_history(&[text_entry(
            "undeletable"
        )]));
        // Make the history file itself impossible to remove by making its directory read-only.
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o500)).unwrap();

        let outcome = storage::request_full_purge();

        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).unwrap();
        assert_eq!(outcome, PurgeOutcome::Deferred);
        assert!(
            storage::purge_intent_present(),
            "the intent must survive for the retry"
        );
        assert!(!storage::writable(), "a pending purge keeps writes off");
        // A save attempt now must not create or modify anything.
        CLIP_HISTORY.lock().unwrap().push(text_entry("new data"));
        save_history();
        flush_persist_worker_for_tests();
        let bytes = test_support::read_history_bytes().expect("file");
        let key = keyring::test_key();
        let plain = open(
            &key,
            ObjectKind::History,
            &object_id_for(HISTORY_LOGICAL_NAME),
            &bytes,
        )
        .expect("open");
        assert!(
            !String::from_utf8(plain).unwrap().contains("new data"),
            "nothing may be written while the purge is pending"
        );
        // The next opportunity finishes the purge, and the session becomes writable again.
        assert_eq!(
            storage::process_pending_purge(),
            storage::PurgeProgress::Completed
        );
        assert!(!storage::purge_intent_present());
        assert!(!history_file_path().exists());
        assert!(
            matches!(storage::raw_state_for_tests(), StorageState::Uninitialized),
            "state was {:?}",
            storage::raw_state_for_tests()
        );
        clean_disk();
    }

    /// A marker that cannot be revoked keeps writes off: otherwise the next launch would treat it
    /// as pending and delete freshly written history.
    #[test]
    fn a_stale_marker_keeps_writes_off_until_it_is_gone() {
        let _guard = guard();
        clean_disk();
        assert!(test_support::write_sealed_history(&[text_entry("old")]));
        // A directory at the marker path: `remove_file` fails, so the revocation cannot succeed.
        let marker = history_dir().join("clipboard-history.purge-pending");
        std::fs::create_dir_all(&marker).unwrap();

        let outcome = storage::request_full_purge();

        assert_eq!(outcome, PurgeOutcome::Deferred);
        assert!(!storage::writable());
        CLIP_HISTORY.lock().unwrap().push(text_entry("fresh"));
        save_history();
        flush_persist_worker_for_tests();
        assert!(
            !history_file_path().exists(),
            "no new history may be written while the stale marker is there"
        );
        std::fs::remove_dir_all(&marker).unwrap();
        assert_eq!(
            storage::process_pending_purge(),
            storage::PurgeProgress::Completed
        );
        assert!(
            matches!(storage::raw_state_for_tests(), StorageState::Uninitialized),
            "state was {:?}",
            storage::raw_state_for_tests()
        );
        clean_disk();
    }

    /// A pending purge is processed before anything loads, and loading stays blocked while it
    /// cannot complete.
    #[test]
    fn a_pending_purge_is_processed_before_loading() {
        use std::os::unix::fs::PermissionsExt;
        let _guard = guard();
        clean_disk();
        let dir = history_dir();
        std::fs::create_dir_all(&dir).unwrap();
        assert!(test_support::write_sealed_history(&[text_entry(
            "pending purge"
        )]));
        // Record the intent the way a crashed purge would leave it, then make the deletion fail.
        std::fs::write(dir.join("clipboard-history.purge-pending"), b"purge\n").unwrap();
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o500)).unwrap();

        load_history();

        assert!(matches!(
            storage::state(),
            StorageState::Blocked(BlockedReason::PurgePending)
        ));
        assert!(
            CLIP_HISTORY.lock().unwrap().is_empty(),
            "records the user asked to delete must not be loaded"
        );
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).unwrap();
        // The retry finishes it and the session recovers.
        assert_eq!(
            storage::process_pending_purge(),
            storage::PurgeProgress::Completed
        );
        assert!(!history_file_path().exists());
        clean_disk();
    }

    #[test]
    fn isolation_renames_the_whole_storage_set_and_never_deletes() {
        let _guard = guard();
        clean_disk();
        assert!(test_support::write_sealed_history(&[text_entry(
            "unrecoverable"
        )]));
        let data = b"image-unrecoverable";
        let hash = fnv1a64(data);
        assert!(cache_write_image(hash, data));
        let cache_dir = clip_image_cache_dir();

        assert!(isolate_storage_set(storage::generation()));

        assert!(
            !history_file_path().exists(),
            "the original name must be free for a fresh start"
        );
        assert!(
            !cache_dir.exists(),
            "the cache directory is moved aside with the history"
        );
        // The whole set still exists aside: nothing was deleted. In a test build the history
        // directory lives INSIDE the cache directory, so moving the cache aside carries the
        // history with it (production moves the index file and the cache separately).
        let parent = cache_dir.parent().unwrap();
        let aside_cache = std::fs::read_dir(parent)
            .unwrap()
            .flatten()
            .map(|e| e.path())
            .find(|p| {
                p.file_name()
                    .map(|n| n.to_string_lossy().contains(".failed-"))
                    .unwrap_or(false)
            })
            .expect("the cache directory must be kept aside");
        fn holds_history(dir: &std::path::Path) -> bool {
            let Ok(entries) = std::fs::read_dir(dir) else {
                return false;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                let name = path.file_name().map(|n| n.to_string_lossy().into_owned());
                if name
                    .as_deref()
                    .is_some_and(|n| n.starts_with("clipboard-history.enc"))
                {
                    return true;
                }
                if path.is_dir() && holds_history(&path) {
                    return true;
                }
            }
            false
        }
        let kept_history = holds_history(&aside_cache);
        assert!(kept_history, "the history must be kept aside, not deleted");
        // The isolation marker is gone once the whole set has moved (in the test layout it may
        // travel inside the aside cache directory, so only the live location matters).
        assert!(
            !matches!(storage::path_present(&isolation_marker_path()), Some(true)),
            "a completed isolation must not leave its marker behind"
        );
        let _ = std::fs::remove_dir_all(&aside_cache);
        clean_disk();
    }

    /// The counter-example for a defect found in review: an unavailable session must leave the stored
    /// history file and the image cache byte-for-byte alone. `clear_session_records` (which the load
    /// path calls) drops this session's records only -- the full switch-off clear, which also purges
    /// the disk, must never be reachable from a failed keychain read.
    #[test]
    fn an_unavailable_session_leaves_the_stored_files_untouched() {
        let _guard = guard();
        clean_disk();
        // What a previous session left behind: one encrypted history and one cached image.
        storage::set_ready(keyring::test_key());
        {
            let mut hist = CLIP_HISTORY.lock().unwrap();
            hist.push(text_entry("stored before the failure"));
        }
        save_history();
        flush_persist_worker_for_tests();
        let cached_hash = fnv1a64(b"cached image bytes");
        assert!(cache_write_image(cached_hash, b"cached image bytes"));
        let history_before = std::fs::read(history_file_path()).expect("history written");
        assert!(clip_image_path(cached_hash).exists());

        // The load decides the session cannot read the key: this is the branch that used to clear the
        // disk as a side effect.
        apply_load_outcome(LoadOutcome {
            generation: storage::generation(),
            decision: LoadDecision::Unavailable(KeyUnavailable::AccessDenied),
        });

        assert_eq!(storage::state_label(), "unavailable");
        assert_eq!(
            std::fs::read(history_file_path()).expect("history still there"),
            history_before,
            "a failed keychain read must not touch the stored history"
        );
        assert!(
            clip_image_path(cached_hash).exists(),
            "a failed keychain read must not touch the image cache"
        );
        assert!(
            CLIP_HISTORY.lock().unwrap().is_empty(),
            "the session's own records are dropped either way"
        );
        storage::set_ready(keyring::test_key());
        clean_disk();
    }

    /// The About row's data source, and the key-only authorization path that feeds it: with the
    /// feature off, asking for keychain access acquires and holds the key (one prompt in the real app)
    /// without loading, migrating, sweeping or saving anything, and the row's
    /// `storage_key_available()` flips to true so the refresh it triggers has something to show.
    #[test]
    fn a_key_only_grant_holds_the_key_without_loading() {
        let _guard = guard();
        clean_disk();
        {
            let mut cfg = CONFIG.read().unwrap().clone();
            cfg.clipboard.enabled = false;
            *CONFIG.write().unwrap() = cfg;
        }
        storage::set_unavailable(KeyUnavailable::AccessDenied);
        assert!(!crate::clipboard::storage_key_available());
        let decided_before = LOAD_DECIDED_GENERATION.load(Ordering::SeqCst);
        crate::clipboard::retry_keychain_access();
        assert!(
            crate::clipboard::storage_key_available(),
            "the grant must hold a key, which is what the About row reports"
        );
        assert_eq!(
            LOAD_DECIDED_GENERATION.load(Ordering::SeqCst),
            decided_before,
            "the key-only path must not load the storage"
        );
        assert!(
            !history_file_path().exists(),
            "the key-only path must not write anything"
        );
        {
            let mut cfg = CONFIG.read().unwrap().clone();
            cfg.clipboard.enabled = true;
            *CONFIG.write().unwrap() = cfg;
        }
        storage::set_ready(keyring::test_key());
        clean_disk();
    }

    /// The counter-examples for the "dropped load" finding, driven through the real
    /// `apply_load_outcome` (not by poking flags):
    ///
    /// (a) a STALE outcome must not decide the current generation, so the request queued behind it
    ///     still runs -- the disable/re-enable-during-a-load ordering that used to leave the storage
    ///     undecided forever;
    /// (b) a new storage generation (the feature toggled off and on) must be able to ask for a load
    ///     again, even though an earlier generation was decided.
    #[test]
    fn a_queued_load_survives_a_stale_outcome_and_a_new_generation() {
        let _guard = guard();
        clean_disk();
        storage::set_ready(keyring::test_key());
        {
            let mut cfg = CONFIG.read().unwrap().clone();
            cfg.clipboard.enabled = true;
            *CONFIG.write().unwrap() = cfg;
        }
        // (a) Stale: the outcome belongs to a generation the storage has already left. A second
        // attempt queues itself on the one the test holds, which is the state the real interleaving
        // produces (a load in flight, a request waiting).
        assert!(begin_load().is_ok(), "the test holds the slot");
        assert!(begin_load().is_err(), "the test queues a request on it");
        apply_load_outcome(LoadOutcome {
            generation: storage::generation().wrapping_sub(1),
            decision: LoadDecision::Unavailable(KeyUnavailable::AccessDenied),
        });
        assert_eq!(
            LOAD_DECIDED_GENERATION.load(Ordering::SeqCst),
            storage::generation(),
            "the queued load must have run for the CURRENT generation after the stale outcome"
        );
        assert!(
            crate::clipboard::storage_key_available(),
            "the current generation's load ran and holds a key"
        );

        // (b) A new generation must be able to ask again.
        assert!(begin_load().is_ok(), "the test holds the slot");
        assert!(begin_load().is_err(), "and queues a request");
        storage::bump_generation();
        finish_load_attempt();
        assert_eq!(
            LOAD_DECIDED_GENERATION.load(Ordering::SeqCst),
            storage::generation(),
            "a new storage generation must not be blocked by an earlier generation's decision"
        );

        // With the feature off the queued request is dropped, not run: the switch owns that boundary.
        let decided_before = LOAD_DECIDED_GENERATION.load(Ordering::SeqCst);
        {
            let mut cfg = CONFIG.read().unwrap().clone();
            cfg.clipboard.enabled = false;
            *CONFIG.write().unwrap() = cfg;
        }
        assert!(begin_load().is_ok(), "the test drives the gate directly");
        storage::bump_generation();
        finish_load_attempt();
        assert_eq!(
            LOAD_DECIDED_GENERATION.load(Ordering::SeqCst),
            decided_before,
            "a disabled feature must not load the storage"
        );
        {
            let mut cfg = CONFIG.read().unwrap().clone();
            cfg.clipboard.enabled = true;
            *CONFIG.write().unwrap() = cfg;
        }
        storage::set_ready(keyring::test_key());
        clean_disk();
    }

    /// The ordered handover the review asked to pin down: a requester that observes the slot busy
    /// must have its request consumed by whoever finishes. As two separate atomics this could lose
    /// the request (it could land after the finisher had already looked at the queue), which would
    /// leave the storage undecided with recording still on; one lock makes the handover a single
    /// critical section, and these assertions pin the ordering down.
    #[test]
    fn the_load_handover_cannot_lose_a_request() {
        let _guard = guard();
        release_load();
        assert!(begin_load().is_ok(), "the first attempt takes the slot");
        assert!(
            begin_load().is_err(),
            "a second attempt can only record its request on the running one"
        );
        assert!(
            release_load(),
            "the finisher must see the request recorded before it released the slot"
        );
        assert!(
            !release_load(),
            "and only once: a consumed request must not run twice"
        );
        assert!(
            begin_load().is_ok(),
            "a request arriving after the release starts a fresh attempt"
        );
        release_load();
        clean_disk();
    }

    /// While another load holds the slot, the grant button's retry must queue itself on it instead of
    /// running as a second attempt, and must not touch the storage state.
    #[test]
    fn a_retry_while_a_load_is_in_flight_is_queued() {
        let _guard = guard();
        clean_disk();
        storage::set_unavailable(KeyUnavailable::AccessDenied);
        assert!(begin_load().is_ok(), "the test holds the slot");
        retry_load_history();
        assert_eq!(
            storage::state_label(),
            "unavailable",
            "a queued retry must not change the storage state"
        );
        assert!(
            release_load(),
            "the retry must have registered itself on the running attempt"
        );
        storage::set_ready(keyring::test_key());
        clean_disk();
    }

    /// Without a key the feature is unavailable: nothing is written, and the state says so.
    #[test]
    fn a_session_without_a_key_writes_nothing() {
        let _guard = guard();
        clean_disk();
        storage::set_unavailable(KeyUnavailable::AccessDenied);

        assert!(!cache_write_image(fnv1a64(b"x"), b"x"));
        CLIP_HISTORY.lock().unwrap().push(text_entry("no key"));
        save_history();
        flush_persist_worker_for_tests();

        assert!(
            !history_file_path().exists(),
            "a session without a key must not create the file"
        );
        assert!(!clip_image_path(fnv1a64(b"x")).exists());
        assert_eq!(storage::state_label(), "unavailable");
        assert_eq!(storage::reason_label(), "access-denied");
        storage::set_ready(keyring::test_key());
        clean_disk();
    }

    /// Holding the key must not make the session writable: between the off-main decision and the
    /// main-thread merge, a save would snapshot an empty list over the file being read.
    #[test]
    fn holding_a_key_does_not_open_the_session_for_writing() {
        let _guard = guard();
        storage::set_unavailable(KeyUnavailable::Missing);
        storage::hold_key(keyring::test_key());
        assert!(!storage::writable(), "the merge has not happened yet");
        assert!(!storage::sweep_allowed());
        assert!(
            storage::key().is_some(),
            "the merge needs the key to read the cache"
        );
        storage::set_ready(keyring::test_key());
        clean_disk();
    }

    /// An index that exists but cannot be read is NOT "no history": starting empty would sweep the
    /// images it references and overwrite it.
    #[test]
    fn an_unreadable_index_blocks_instead_of_starting_empty() {
        use std::os::unix::fs::PermissionsExt;
        let _guard = guard();
        clean_disk();
        assert!(test_support::write_sealed_history(&[text_entry(
            "unreadable"
        )]));
        let data = b"image-kept-alive";
        let hash = fnv1a64(data);
        assert!(cache_write_image(hash, data));
        let encrypted = history_file_path();
        std::fs::set_permissions(&encrypted, std::fs::Permissions::from_mode(0o000)).unwrap();

        load_history();

        std::fs::set_permissions(&encrypted, std::fs::Permissions::from_mode(0o600)).unwrap();
        assert!(matches!(
            storage::state(),
            StorageState::Blocked(BlockedReason::Damaged)
        ));
        // The blocked state drops the key by design, so presence is asserted on the file itself.
        assert!(
            clip_image_path(hash).exists(),
            "the image must not be swept"
        );
        let sealed_now = std::fs::read(clip_image_path(hash)).unwrap();
        assert!(
            !sealed_now.is_empty(),
            "the file must still hold its content"
        );
        assert!(
            looks_like_envelope(&sealed_now),
            "the file must stay sealed"
        );
        assert!(encrypted.exists(), "the index must stay");
        clean_disk();
    }

    /// A legacy index that is not valid UTF-8 is damage, not "no history".
    #[test]
    fn an_invalid_utf8_legacy_index_blocks_instead_of_starting_empty() {
        let _guard = guard();
        clean_disk();
        let data = b"legacy-image-kept";
        let hash = fnv1a64(data);
        std::fs::create_dir_all(clip_image_cache_dir()).unwrap();
        std::fs::write(clip_image_path(hash), data).unwrap();
        let legacy = legacy_history_file_path();
        std::fs::create_dir_all(legacy.parent().unwrap()).unwrap();
        std::fs::write(&legacy, [0xff, 0xfe, 0x00, 0x01]).unwrap();

        load_history();

        assert!(matches!(
            storage::state(),
            StorageState::Blocked(BlockedReason::Damaged)
        ));
        assert!(legacy.exists(), "the legacy index must stay");
        assert!(
            clip_image_path(hash).exists(),
            "the image must not be swept"
        );
        assert!(!history_file_path().exists(), "nothing may be committed");
        clean_disk();
    }

    /// A cache file that is present but does not authenticate (here: a complete envelope for a
    /// different object) blocks the load instead of dropping the entry and sweeping the file.
    #[test]
    fn an_unauthenticated_cache_file_blocks_the_whole_load() {
        let _guard = guard();
        clean_disk();
        let data = b"real-image-bytes";
        let hash = fnv1a64(data);
        // Seal the wrong object into this hash's slot: the file is a valid envelope, but not this
        // object's.
        let other = object_id_for("some-other-object");
        let sealed = seal(&keyring::test_key(), ObjectKind::ImageData, &other, data).expect("seal");
        std::fs::create_dir_all(clip_image_cache_dir()).unwrap();
        std::fs::write(clip_image_path(hash), &sealed).unwrap();
        assert!(test_support::write_sealed_history(&[image_entry(hash)]));
        let index_before = test_support::read_history_bytes().expect("index");

        load_history();

        assert!(matches!(
            storage::state(),
            StorageState::Blocked(BlockedReason::Damaged)
        ));
        assert_eq!(
            std::fs::read(clip_image_path(hash)).unwrap(),
            sealed,
            "the file must not be deleted"
        );
        assert_eq!(
            test_support::read_history_bytes().expect("index"),
            index_before,
            "the index must not be rewritten"
        );
        clean_disk();
    }

    /// An isolation that was interrupted (marker left behind) is finished before anything else: the
    /// set moves aside as a whole, so a fresh start cannot sweep what was not yet isolated.
    #[test]
    fn an_interrupted_isolation_is_finished_before_the_next_load() {
        let _guard = guard();
        clean_disk();
        assert!(test_support::write_sealed_history(&[text_entry(
            "half isolated"
        )]));
        let data = b"image-half-isolated";
        let hash = fnv1a64(data);
        assert!(cache_write_image(hash, data));
        // What a crash mid-isolation leaves: the marker, with everything still in place.
        let marker = isolation_marker_path();
        std::fs::create_dir_all(marker.parent().unwrap()).unwrap();
        std::fs::write(&marker, b"isolating\n").unwrap();

        load_history();

        assert!(
            !history_file_path().exists(),
            "the set must have moved aside"
        );
        assert!(!clip_image_cache_dir().exists());
        assert!(
            !matches!(storage::path_present(&marker), Some(true)),
            "the marker must be gone once the isolation completed"
        );
        assert!(matches!(storage::state(), StorageState::Ready));
        clean_disk();
    }

    /// A temp file that cannot be removed keeps the purge pending: an unverified deletion must
    /// never be reported as complete.
    #[test]
    fn an_undeletable_temp_file_keeps_the_purge_pending() {
        use std::process::Command;
        let _guard = guard();
        clean_disk();
        assert!(test_support::write_sealed_history(&[text_entry(
            "purge me"
        )]));
        let dir = history_dir();
        let temp = dir.join(format!("clipboard-history.enc.tmp{}-7", std::process::id()));
        std::fs::write(&temp, b"leftover").unwrap();
        // The immutable flag makes unlink fail even though the directory is writable.
        let locked = Command::new("/usr/bin/chflags")
            .arg("uchg")
            .arg(&temp)
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        if !locked {
            clean_disk();
            return; // chflags unavailable: the case cannot be constructed here
        }

        let outcome = storage::request_full_purge();

        let _ = Command::new("/usr/bin/chflags")
            .arg("nouchg")
            .arg(&temp)
            .status();
        assert_eq!(
            outcome,
            PurgeOutcome::Deferred,
            "a temp file that survived must keep the purge pending"
        );
        assert!(storage::purge_intent_present());
        assert!(!storage::writable());
        assert_eq!(
            storage::process_pending_purge(),
            storage::PurgeProgress::Completed,
            "the retry completes it"
        );
        assert!(storage::writable());
        clean_disk();
    }

    /// A purge that could not even be recorded keeps the session blocked across config applies,
    /// and a retry that can record it finishes the job.
    #[test]
    fn an_unrecorded_purge_keeps_the_session_blocked() {
        use std::os::unix::fs::PermissionsExt;
        let _guard = guard();
        clean_disk();
        let dir = history_dir();
        std::fs::create_dir_all(&dir).unwrap();
        assert!(test_support::write_sealed_history(&[text_entry(
            "unrecordable"
        )]));
        // Neither intent location may be writable: the primary lives in a read-only directory, the
        // fallback's own directory is read-only too.
        let fallback = storage::fallback_marker_path_for_tests();
        let fallback_dir = fallback.parent().unwrap().to_path_buf();
        std::fs::create_dir_all(&fallback_dir).unwrap();
        std::fs::set_permissions(&fallback_dir, std::fs::Permissions::from_mode(0o500)).unwrap();
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o500)).unwrap();

        let outcome = storage::request_full_purge();

        assert_eq!(outcome, PurgeOutcome::NotAccepted);
        assert!(storage::purge_not_accepted());
        assert!(!storage::writable());
        // The config-apply path sees no marker; it must not conclude the records are gone.
        assert_eq!(
            storage::process_pending_purge(),
            storage::PurgeProgress::StillPending
        );
        assert!(
            !storage::writable(),
            "an unrecorded purge request keeps the session blocked"
        );
        // Once the obstacles are gone the retry records the intent, deletes and clears the block.
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).unwrap();
        std::fs::set_permissions(&fallback_dir, std::fs::Permissions::from_mode(0o700)).unwrap();
        assert_eq!(
            storage::process_pending_purge(),
            storage::PurgeProgress::Completed
        );
        assert!(!history_file_path().exists());
        assert!(
            matches!(storage::raw_state_for_tests(), StorageState::Uninitialized),
            "state was {:?}",
            storage::raw_state_for_tests()
        );
        clean_disk();
    }

    /// In a memory-only session an existing cache file must NOT report a successful write: the
    /// caller would retire the pending bytes and lose the only readable copy of the new image.
    #[test]
    fn an_existing_cache_file_is_not_a_successful_write_without_a_key() {
        let _guard = guard();
        clean_disk();
        let data = b"cached-earlier";
        let hash = fnv1a64(data);
        assert!(
            cache_write_image(hash, data),
            "the earlier session wrote it"
        );
        storage::set_unavailable(KeyUnavailable::Missing);

        assert!(
            !cache_write_image(hash, data),
            "a memory-only session may not claim the write"
        );
        // The retirement path must not delete the file either.
        image_cache::retire_image_hash(hash);
        assert!(
            clip_image_path(hash).exists(),
            "never delete in a memory-only session"
        );
        storage::set_ready(keyring::test_key());
        clean_disk();
    }

    /// A load result that belongs to storage discarded in the meantime must never be applied: it
    /// would restore records the user asked to delete and re-enable writing.
    #[test]
    fn a_stale_load_result_is_dropped() {
        let _guard = guard();
        clean_disk();
        apply_stale_load_for_tests();
        assert!(
            matches!(storage::raw_state_for_tests(), StorageState::Unavailable(_)),
            "the stale result must not touch the state"
        );
        assert!(storage::key().is_none(), "and must not install a key");
        clean_disk();
    }

    /// Content copied while the load is still deciding is not writable yet; when the load lands it
    /// must be persisted (the history and the image originals held in memory).
    #[test]
    fn content_recorded_before_the_load_lands_reaches_the_disk() {
        let _guard = guard();
        clean_disk();
        storage::set_unavailable(KeyUnavailable::Missing);
        let data = b"image-during-load";
        let hash = fnv1a64(data);
        CLIP_HISTORY
            .lock()
            .unwrap()
            .push(text_entry("copied during the load"));
        CLIP_HISTORY.lock().unwrap().push(image_entry(hash));
        image_cache::insert_pending_for_tests(hash, Arc::new(data.to_vec()));
        assert!(!storage::writable(), "nothing may be written yet");

        // The load lands with an empty decision (nothing on disk).
        apply_load_outcome(outcome(
            storage::generation(),
            LoadDecision::Empty {
                key: keyring::test_key(),
            },
        ));
        flush_persist_worker_for_tests();

        let bytes = test_support::read_history_bytes().expect("history file");
        let key = keyring::test_key();
        let plain = open(
            &key,
            ObjectKind::History,
            &object_id_for(HISTORY_LOGICAL_NAME),
            &bytes,
        )
        .expect("open");
        assert!(
            String::from_utf8(plain)
                .unwrap()
                .contains("copied during the load"),
            "the text copied during the load must reach the disk"
        );
        assert!(
            cache_read_image(hash).is_some(),
            "the pending image original must reach the cache"
        );
        assert_eq!(image_cache::pending_count_for_tests(), 0);
        clean_disk();
    }

    /// A marker that cannot be checked must never authorize a deletion: the purge defers and the
    /// session stays blocked, with the history untouched.
    #[test]
    fn an_unverifiable_marker_defers_the_purge() {
        use std::os::unix::fs::PermissionsExt;
        let _guard = guard();
        clean_disk();
        assert!(test_support::write_sealed_history(&[text_entry(
            "must survive"
        )]));
        // The fallback marker's own directory becomes unreadable: its state cannot be determined.
        let fallback_dir = storage::fallback_marker_path_for_tests()
            .parent()
            .unwrap()
            .to_path_buf();
        std::fs::create_dir_all(&fallback_dir).unwrap();
        std::fs::set_permissions(&fallback_dir, std::fs::Permissions::from_mode(0o000)).unwrap();

        load_history();

        std::fs::set_permissions(&fallback_dir, std::fs::Permissions::from_mode(0o700)).unwrap();
        assert!(matches!(
            storage::state(),
            StorageState::Blocked(BlockedReason::PurgePending)
        ));
        assert!(
            history_file_path().exists(),
            "an unverifiable marker must not authorize deleting the history"
        );
        clean_disk();
    }

    /// An isolation whose marker cannot be revoked must not report success: the next launch would
    /// isolate the history written from then on.
    #[test]
    fn an_unrevocable_isolation_marker_keeps_the_isolation_incomplete() {
        use std::process::Command;
        let _guard = guard();
        clean_disk();
        assert!(test_support::write_sealed_history(&[text_entry(
            "isolate me"
        )]));
        let marker = isolation_marker_path();
        std::fs::create_dir_all(marker.parent().unwrap()).unwrap();
        std::fs::write(&marker, b"isolating\n").unwrap();
        let locked = Command::new("/usr/bin/chflags")
            .arg("uchg")
            .arg(&marker)
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        if !locked {
            clean_disk();
            return; // chflags unavailable: the case cannot be constructed here
        }

        let isolated = isolate_storage_set(storage::generation());

        let _ = Command::new("/usr/bin/chflags")
            .arg("nouchg")
            .arg(&marker)
            .status();
        assert!(
            !isolated,
            "a marker that survives must keep the isolation incomplete"
        );
        clean_disk();
    }

    /// A migration that committed but could not finish the plaintext cleanup is NOT complete: the
    /// plaintext stays and the state must say so, while the encrypted index stays usable.
    #[test]
    fn an_unfinished_plaintext_cleanup_keeps_the_migration_incomplete() {
        use std::process::Command;
        let _guard = guard();
        clean_disk();
        let legacy = legacy_history_file_path();
        std::fs::create_dir_all(legacy.parent().unwrap()).unwrap();
        std::fs::write(&legacy, serialize_history(&[text_entry("legacy")]).unwrap()).unwrap();
        let locked = Command::new("/usr/bin/chflags")
            .arg("uchg")
            .arg(&legacy)
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        if !locked {
            clean_disk();
            return;
        }

        load_history();

        let _ = Command::new("/usr/bin/chflags")
            .arg("nouchg")
            .arg(&legacy)
            .status();
        assert!(matches!(
            storage::state(),
            StorageState::Blocked(BlockedReason::MigrationIncomplete)
        ));
        assert!(
            history_file_path().exists(),
            "the encrypted index is committed and stays"
        );
        assert!(legacy.exists(), "the plaintext index stays for the retry");
        // The retry (a later load) finishes it.
        load_history();
        assert!(matches!(storage::state(), StorageState::Ready));
        assert!(!legacy.exists());
        clean_disk();
    }

    /// A purge request that could not even be recorded still invalidates loads that started before
    /// it: otherwise an old result would re-enable writing over the records the user asked to drop.
    #[test]
    fn a_blocked_purge_request_invalidates_an_earlier_load() {
        use std::os::unix::fs::PermissionsExt;
        let _guard = guard();
        clean_disk();
        assert!(test_support::write_sealed_history(&[text_entry("old")]));
        let generation_before = storage::generation();
        // Both intent locations unusable -> the purge is not accepted at all.
        let dir = history_dir();
        let fallback_dir = storage::fallback_marker_path_for_tests()
            .parent()
            .unwrap()
            .to_path_buf();
        std::fs::create_dir_all(&fallback_dir).unwrap();
        std::fs::set_permissions(&fallback_dir, std::fs::Permissions::from_mode(0o500)).unwrap();
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o500)).unwrap();

        let outcome = storage::request_full_purge();

        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).unwrap();
        std::fs::set_permissions(&fallback_dir, std::fs::Permissions::from_mode(0o700)).unwrap();
        assert_eq!(outcome, PurgeOutcome::NotAccepted);
        assert!(
            storage::generation() > generation_before,
            "a purge request invalidates loads that started before it"
        );
        // A result captured before the request is dropped, not applied.
        apply_load_outcome(LoadOutcome {
            generation: generation_before,
            decision: LoadDecision::Empty {
                key: keyring::test_key(),
            },
        });
        assert!(matches!(
            storage::raw_state_for_tests(),
            StorageState::Blocked(BlockedReason::PurgePending)
        ));
        assert!(history_file_path().exists(), "nothing was deleted");
        clean_disk();
    }

    /// A plaintext leftover keeps the state honest on EVERY load, not only on the first migration
    /// attempt: while the fault persists, repeated loads stay blocked and resumable.
    #[test]
    fn a_persistent_plaintext_leftover_keeps_blocking_every_load() {
        use std::process::Command;
        let _guard = guard();
        clean_disk();
        let legacy = legacy_history_file_path();
        std::fs::create_dir_all(legacy.parent().unwrap()).unwrap();
        std::fs::write(&legacy, serialize_history(&[text_entry("legacy")]).unwrap()).unwrap();
        let locked = Command::new("/usr/bin/chflags")
            .arg("uchg")
            .arg(&legacy)
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        if !locked {
            clean_disk();
            return;
        }

        for round in 0..2 {
            load_history();
            assert!(
                matches!(
                    storage::state(),
                    StorageState::Blocked(BlockedReason::MigrationIncomplete)
                ),
                "load {round} must stay blocked while the plaintext index survives, state was {:?}",
                storage::state()
            );
        }
        let _ = Command::new("/usr/bin/chflags")
            .arg("nouchg")
            .arg(&legacy)
            .status();
        // Once the fault is gone the next load finishes the cleanup and reports ready.
        load_history();
        assert!(matches!(storage::state(), StorageState::Ready));
        assert!(!legacy.exists());
        clean_disk();
    }

    /// A load result that became stale must not isolate the storage a newer generation owns: the
    /// isolation carries its generation and takes the same locks as the purge and the migration.
    #[test]
    fn a_stale_isolation_request_touches_nothing() {
        let _guard = guard();
        clean_disk();
        assert!(test_support::write_sealed_history(&[text_entry(
            "new generation"
        )]));
        let data = b"new-generation-image";
        let hash = fnv1a64(data);
        assert!(cache_write_image(hash, data));
        let stale_generation = storage::generation();
        storage::bump_generation(); // a purge or a newer load moved on

        let isolated = isolate_storage_set(stale_generation);

        assert!(!isolated, "a stale isolation must refuse to run");
        assert!(history_file_path().exists(), "the current history stays");
        assert!(clip_image_path(hash).exists(), "the current cache stays");
        clean_disk();
    }

    /// The pending slot keeps the NEWEST result: an older one arriving later must not displace it,
    /// or the valid load would be lost and the session stay unwritable.
    #[test]
    fn an_older_load_result_never_displaces_a_newer_one() {
        let _guard = guard();
        clean_disk();
        let generation = storage::generation();
        let newer = LoadOutcome {
            generation: generation + 1,
            decision: LoadDecision::Empty {
                key: keyring::test_key(),
            },
        };
        let older = LoadOutcome {
            generation,
            decision: LoadDecision::Blocked(BlockedReason::Damaged),
        };
        assert!(store_pending_load(newer.clone()));
        assert!(
            !store_pending_load(older),
            "an older result must be dropped"
        );
        let stored = PENDING_LOAD.lock().unwrap().take().expect("slot");
        assert_eq!(stored.generation, generation + 1);
        clean_disk();
    }

    /// An interrupted isolation resumed by a STALE load must not rename the storage a newer
    /// generation owns: the resume goes through the same locks and generation check.
    #[test]
    fn a_stale_isolation_resume_touches_nothing() {
        let _guard = guard();
        clean_disk();
        assert!(test_support::write_sealed_history(&[text_entry(
            "new generation"
        )]));
        let data = b"new-generation-image";
        let hash = fnv1a64(data);
        assert!(cache_write_image(hash, data));
        let marker = isolation_marker_path();
        std::fs::create_dir_all(marker.parent().unwrap()).unwrap();
        std::fs::write(&marker, b"isolating\n").unwrap();
        let index_before = std::fs::read(history_file_path()).unwrap();
        let cache_before = std::fs::read(clip_image_path(hash)).unwrap();
        let stale_generation = storage::generation();
        storage::bump_generation(); // a purge or a newer load moved on

        let resumed = resume_isolation(stale_generation);

        assert!(!resumed, "a stale resume must refuse to run");
        assert_eq!(
            std::fs::read(history_file_path()).unwrap(),
            index_before,
            "the current index must be byte-identical"
        );
        assert_eq!(
            std::fs::read(clip_image_path(hash)).unwrap(),
            cache_before,
            "the current cache file must be byte-identical"
        );
        assert!(
            matches!(storage::path_present(&marker), Some(true)),
            "the marker stays for a current load to finish"
        );
        // The current generation may finish it.
        assert!(resume_isolation(storage::generation()));
        assert!(!history_file_path().exists());
        clean_disk();
    }

    #[test]
    fn the_temp_file_predicate_covers_both_history_names() {
        assert!(is_history_temp_file_name("clipboard-history.enc.tmp1234-7"));
        assert!(is_history_temp_file_name(
            "clipboard-history.toml.tmp1234-7"
        ));
        for lookalike in [
            "clipboard-history.enc",
            "clipboard-history.enc.tmp",
            "clipboard-history.enc.tmp1234",
            "clipboard-history.enc.tmp-7",
            "clipboard-history.enc.tmp1234-",
            "clipboard-history.enc.tmp1234-7-8",
            "clipboard-history.enc.tmpa-7",
            "clipboard-history.enc.tmp1234-x",
            "clipboard-history.enc.failed-1234",
            "clipboard-history.bak",
            "clipboard-history.enc.tmp1234-7.tmp",
        ] {
            assert!(
                !is_history_temp_file_name(lookalike),
                "{lookalike} must not be treated as a temp file"
            );
        }
    }
}
