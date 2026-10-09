//! Clipboard subsystem · storage: the session's storage state, its generation, and the purge
//! transaction.
//!
//! Three states decide what may happen to the clipboard's storage (see the plan's §4.5):
//! `Ready` (key available, history loaded: read/write/sweep), `Unavailable` (no key this session:
//! nothing is recorded, written, swept or deleted -- the feature is off until the user authorizes
//! the keychain again) and `Blocked` (data present but unreadable, or a purge is unfinished: never
//! write, never sweep, never delete). Every write path, every sweep entry point and every delete
//! asks this module first — the invariants R1–R6 are only real if there is a single gate.

use super::*;
use std::path::PathBuf;

/// Why storage is blocked. Each reason has its own copy, because they call for different user
/// actions.
#[derive(Clone, PartialEq, Eq, Debug)]
pub(super) enum BlockedReason {
    /// The envelope did not authenticate: damaged, or sealed by a key that no longer exists in
    /// a reproducible form.
    Damaged,
    /// The file was sealed by a different key (key_id mismatch). The data may be perfectly fine
    /// — never replace the key over this.
    ForeignKey,
    /// The history file's format version is newer than this build understands.
    UnsupportedVersion,
    /// The plaintext → encrypted migration could not convert every referenced cache file, so it
    /// did not commit. The plaintext index and every original are kept for the next attempt.
    MigrationIncomplete,
    /// A user-authorized purge could not be completed. Loading or writing now would show the
    /// user records they asked to delete.
    PurgePending,
}

/// What this session may do with the clipboard's storage.
#[derive(Clone, PartialEq, Eq, Debug)]
pub(super) enum StorageState {
    /// The load path has not decided yet. Treated as not-writable so a write can never race
    /// ahead of the decision.
    Uninitialized,
    /// Key available, history loaded or absent.
    Ready,
    /// No key this session (refused / cancelled / locked / simulated). The feature is unavailable:
    /// nothing is recorded, and the panel states why instead of showing entries.
    Unavailable(KeyUnavailable),
    /// Data is present but unreadable, or a purge is unfinished.
    Blocked(BlockedReason),
}

struct Session {
    state: StorageState,
    key: Option<MasterKey>,
    generation: u64,
}

impl Session {
    const fn new() -> Self {
        Self {
            state: StorageState::Uninitialized,
            key: None,
            generation: 0,
        }
    }
}

static SESSION: Mutex<Session> = Mutex::new(Session::new());

/// Whether this process is a test or smoke run: both keep every file under a temporary directory
/// and use the fixed test key, and both may inject entries without going through the load path.
pub(super) fn is_test_process() -> bool {
    cfg!(test) || SMOKE_MODE.load(Ordering::SeqCst)
}

/// The current state (a copy; the state is two small fields).
pub(super) fn state() -> StorageState {
    let session = SESSION.lock().unwrap();
    let state = session.state.clone();
    // Test/smoke affordance: a session that never touched the state machine behaves like a loaded
    // one (writable, test key), so the cache and persistence tests below the load path -- and the
    // smoke runners, which inject entries directly -- do not each have to set it up. A test that
    // cares about the state sets it explicitly.
    if is_test_process() && matches!(state, StorageState::Uninitialized) {
        return StorageState::Ready;
    }
    state
}

/// Test-only: clear the in-process "a purge request could not be recorded" fact, so one test's
/// NotAccepted cannot make a later test run an unexpected purge.
#[cfg(test)]
pub(super) fn clear_purge_not_accepted_for_tests() {
    PURGE_NOT_ACCEPTED.store(false, Ordering::SeqCst);
}

/// Test-only: the state as recorded, without the test/smoke "uninitialized behaves like Ready"
/// affordance. A test asserting what the state machine did (e.g. "a purge leaves it
/// uninitialized") needs this, or the affordance would answer for it.
#[cfg(test)]
pub(super) fn raw_state_for_tests() -> StorageState {
    SESSION.lock().unwrap().state.clone()
}

/// Whether the session may write to disk (invariant: only `Ready`).
pub(super) fn writable() -> bool {
    matches!(state(), StorageState::Ready)
}

/// Whether orphan sweeps may run. Sweeps delete files, so they are gated exactly like writes:
/// a sweep against an unloaded (empty) history would delete every cached image.
pub(super) fn sweep_allowed() -> bool {
    matches!(state(), StorageState::Ready)
}

/// The session's master key (a copy; both copies zeroize on drop).
pub(super) fn key() -> Option<MasterKey> {
    let session = SESSION.lock().unwrap();
    if let Some(key) = &session.key {
        return Some(key.clone());
    }
    // Same test/smoke affordance as `state()`.
    if is_test_process() && matches!(session.state, StorageState::Uninitialized) {
        return Some(keyring::test_key());
    }
    None
}

/// The storage generation: bumped whenever the storage set is discarded or rebuilt. Work
/// started under an older generation must not apply its result or write anything.
pub(super) fn generation() -> u64 {
    SESSION.lock().unwrap().generation
}

/// Invalidate every in-flight task that was started before this point.
pub(super) fn bump_generation() {
    SESSION.lock().unwrap().generation += 1;
}

/// Install the key WITHOUT changing the state: the load path needs it to read the cache while the
/// history is still being merged, and the session must not become writable before that merge (a
/// copy recorded in the window would save an empty snapshot over the file being read).
pub(super) fn hold_key(key: MasterKey) {
    SESSION.lock().unwrap().key = Some(key);
}

/// Ready with a key (the load path's success outcome).
pub(super) fn set_ready(key: MasterKey) {
    let mut session = SESSION.lock().unwrap();
    session.state = StorageState::Ready;
    session.key = Some(key);
}

/// No key this session.
pub(super) fn set_unavailable(reason: KeyUnavailable) {
    let mut session = SESSION.lock().unwrap();
    session.state = StorageState::Unavailable(reason);
    session.key = None;
}

/// Hold a key acquired without a load (the feature is off): the About row reports "granted" from the
/// held key, and holding it deliberately does NOT open the session for writing (same rule as the
/// load's merge window).
pub(super) fn hold_key_only(key: MasterKey) {
    let mut session = SESSION.lock().unwrap();
    session.key = Some(key);
}

/// Data present but unreadable, or a purge unfinished.
pub(super) fn set_blocked(reason: BlockedReason) {
    let mut session = SESSION.lock().unwrap();
    session.state = StorageState::Blocked(reason);
    session.key = None;
}

/// A short state label for `--e2e-state` and logs. Never contains key material.
pub(super) fn state_label() -> &'static str {
    match state() {
        StorageState::Uninitialized => "uninitialized",
        StorageState::Ready => "encrypted",
        StorageState::Unavailable(_) => "unavailable",
        StorageState::Blocked(_) => "blocked",
    }
}

/// A short reason label for `--e2e-state` and logs.
pub(super) fn reason_label() -> &'static str {
    match state() {
        StorageState::Unavailable(reason) => keyring::unavailable_label(&reason),
        StorageState::Blocked(BlockedReason::Damaged) => "damaged",
        StorageState::Blocked(BlockedReason::ForeignKey) => "foreign-key",
        StorageState::Blocked(BlockedReason::UnsupportedVersion) => "version",
        StorageState::Blocked(BlockedReason::MigrationIncomplete) => "migration",
        StorageState::Blocked(BlockedReason::PurgePending) => "purge-pending",
        _ => "",
    }
}

/// The outcome of a purge attempt.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum PurgeOutcome {
    /// Deleted, verified, and the intent was revoked.
    Done,
    /// The intent is recorded but the deletion or its verification did not finish. Retried at
    /// the next opportunity; loading and writing stay blocked until it does.
    Deferred,
    /// Neither intent location could be written, so nothing was deleted (a partial delete would
    /// contradict what we report) and the request is not durable across launches.
    NotAccepted,
}

/// The purge intent marker: next to the history file, so it travels with the storage set.
fn primary_marker_path() -> PathBuf {
    let history = history_file_path();
    match history.parent() {
        Some(dir) => dir.join("clipboard-history.purge-pending"),
        None => PathBuf::from("clipboard-history.purge-pending"),
    }
}

/// The fallback intent marker: a PERSISTENT directory (never `~/Library/Caches`, which the
/// system may clear — a purge intent cannot be regenerated from what is left on disk). In
/// test/smoke runs it stays inside the temporary root, next to the cache directory it must
/// survive.
fn fallback_marker_path() -> PathBuf {
    if SMOKE_MODE.load(Ordering::SeqCst) || cfg!(test) {
        // Test/smoke runs keep the fallback inside the temporary root, in a directory of its own so
        // a test can make BOTH intent locations unusable (the production path below is a persistent
        // directory that no test may touch).
        let cache = clip_image_cache_dir();
        let name = cache
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "oh-my-tab-clip-images".to_string());
        return cache
            .with_file_name(format!("{name}-fallback"))
            .join("purge-pending");
    }
    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string());
    PathBuf::from(format!(
        "{home}/Library/Application Support/oh-my-tab/purge-pending"
    ))
}

/// Whether `path` is there, distinguishing "definitely absent" from "cannot tell". A check that
/// cannot be made must never be reported as absence: the purge would then revoke its intent and
/// re-enable writing over records it never verified.
pub(super) fn path_present(path: &std::path::Path) -> Option<bool> {
    match std::fs::symlink_metadata(path) {
        Ok(_) => Some(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Some(false),
        Err(_) => None,
    }
}

/// The state of the purge intent, as far as this process can tell.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum IntentState {
    Absent,
    Present,
    /// The marker could not be checked: treated as "pending, unverified" — never as absent.
    Unknown,
}

/// Whether a purge is waiting to be finished (either marker).
pub(super) fn purge_intent_state() -> IntentState {
    let mut unknown = false;
    for path in [primary_marker_path(), fallback_marker_path()] {
        match path_present(&path) {
            Some(true) => return IntentState::Present,
            Some(false) => {}
            None => unknown = true,
        }
    }
    if unknown {
        IntentState::Unknown
    } else {
        IntentState::Absent
    }
}

/// Whether a purge is waiting to be finished (either marker). Test-only: production asks for the
/// three-valued `purge_intent_state()`, because "cannot check" must not read as "absent".
#[cfg(test)]
pub(super) fn purge_intent_present() -> bool {
    purge_intent_state() == IntentState::Present
}

/// Set when a purge was requested but no intent could be recorded. It has to survive the next
/// config apply: with no marker on disk, that path would otherwise assume the records are gone and
/// re-enable writing over history the user asked to delete. Only a verified purge clears it.
static PURGE_NOT_ACCEPTED: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// Whether a purge request could not even be recorded this session.
pub(super) fn purge_not_accepted() -> bool {
    PURGE_NOT_ACCEPTED.load(Ordering::SeqCst)
}

/// Test-only: the fallback intent path, so a test can make both locations unusable.
#[cfg(test)]
pub(super) fn fallback_marker_path_for_tests() -> PathBuf {
    fallback_marker_path()
}

/// Record the purge intent. Tries the primary location first, then the persistent fallback.
fn write_purge_intent() -> bool {
    let primary = primary_marker_path();
    if let Some(dir) = primary.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if std::fs::write(&primary, b"purge\n").is_ok() {
        return true;
    }
    let fallback = fallback_marker_path();
    if let Some(dir) = fallback.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    std::fs::write(&fallback, b"purge\n").is_ok()
}

/// Revoke the purge intent. Only `true` when NEITHER marker remains: a stale marker left behind
/// would make the next launch delete freshly written history.
fn clear_purge_intent() -> bool {
    let mut ok = true;
    for path in [primary_marker_path(), fallback_marker_path()] {
        match std::fs::remove_file(&path) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => {
                log_info!(
                    "Clipboard purge intent could not be removed: {}",
                    path.display()
                );
                ok = false;
            }
        }
    }
    ok
}

/// Run the purge transaction (invariant R6):
/// 1. persist the intent FIRST (so a crash mid-delete is recoverable),
/// 2. stop in-flight work (generation bump) so nothing writes after this point,
/// 3. delete, 4. verify, 5. revoke the intent.
///
/// Only a verified deletion plus a verified revocation returns `Done`.
pub(super) fn run_full_purge() -> PurgeOutcome {
    // Any attempt invalidates loads that started before it, whatever its outcome: a request to stop
    // keeping records must not be bypassed by a result computed under the old generation.
    bump_generation();
    match purge_intent_state() {
        IntentState::Present => {}
        IntentState::Absent => {
            if !write_purge_intent() {
                log_info!(
                    "Clipboard purge not accepted: neither intent location is writable; nothing was deleted."
                );
                PURGE_NOT_ACCEPTED.store(true, Ordering::SeqCst);
                return PurgeOutcome::NotAccepted;
            }
        }
        IntentState::Unknown => {
            // A marker that cannot be checked might be there: deleting now could remove history
            // whose purge nobody authorized. Nothing is deleted, and the session stays blocked.
            log_info!("Clipboard purge deferred: the intent state cannot be verified.");
            return PurgeOutcome::Deferred;
        }
    }
    bump_generation();
    let remaining = discard_history_verified();
    if !remaining.is_empty() {
        for path in &remaining {
            log_info!(
                "Clipboard purge incomplete, still present: {}",
                path.display()
            );
        }
        return PurgeOutcome::Deferred;
    }
    if !clear_purge_intent() {
        return PurgeOutcome::Deferred;
    }
    // The intent must be verifiably gone: a marker that cannot be checked counts as still there.
    if purge_intent_state() != IntentState::Absent {
        log_info!("Clipboard purge could not be verified as complete; it stays pending.");
        return PurgeOutcome::Deferred;
    }
    PURGE_NOT_ACCEPTED.store(false, Ordering::SeqCst);
    log_info!("Clipboard purge complete (history file, image cache and intent removed).");
    PurgeOutcome::Done
}

/// A user-authorized purge (turning the switch off, or `clear_on_quit`). The key is kept
/// (decision 3): it reveals nothing on its own, and deleting it would add an irreversible
/// failure path without a real gain.
pub(super) fn request_full_purge() -> PurgeOutcome {
    let outcome = run_full_purge();
    if outcome != PurgeOutcome::Done {
        set_blocked(BlockedReason::PurgePending);
    }
    outcome
}

/// What a pending-purge attempt did.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum PurgeProgress {
    /// No purge was waiting.
    NothingPending,
    /// A purge was waiting and is now complete and verified.
    Completed,
    /// A purge is still pending (deferred, or not even recordable): the session stays blocked.
    StillPending,
}

/// Finish a purge an earlier session could not complete. Called at every launch and on every config
/// change, BEFORE anything loads or writes, and regardless of whether the clipboard switch is
/// currently on (`clear_on_quit` failures happen with the switch on).
pub(super) fn process_pending_purge() -> PurgeProgress {
    if !purge_not_accepted() && purge_intent_state() == IntentState::Absent {
        // Nothing pending. A session that was blocked on the purge can now be rebuilt: the records
        // it was protecting are gone, so a fresh key is allowed (invariant R1).
        if matches!(state(), StorageState::Blocked(BlockedReason::PurgePending)) {
            rebuild_after_purge();
            return PurgeProgress::Completed;
        }
        return PurgeProgress::NothingPending;
    }
    // Either a marker is there, or an earlier attempt could not even record one. Retry the
    // transaction: an unrecorded request must keep the session blocked until a VERIFIED purge
    // completes, and a later apply must not read "no marker" as "the records are gone".
    match run_full_purge() {
        PurgeOutcome::Done => {
            rebuild_after_purge();
            PurgeProgress::Completed
        }
        PurgeOutcome::Deferred | PurgeOutcome::NotAccepted => {
            log_info!("Clipboard history stays blocked: a purge is still pending.");
            set_blocked(BlockedReason::PurgePending);
            PurgeProgress::StillPending
        }
    }
}

/// After a completed purge the storage set is empty; the session must not become writable until a
/// fresh load decides (see the body).
fn rebuild_after_purge() {
    // Deliberately NOT acquiring the key here: this runs on the caller's thread (a config apply on
    // the main thread), and a keychain read may block on a system prompt. The session stays
    // uninitialized and not writable until the next load decides — off the main thread.
    let mut session = SESSION.lock().unwrap();
    session.state = StorageState::Uninitialized;
    session.key = None;
}

/// Test-only: the single lock every test that touches the storage state OR the shared image
/// cache takes. The state is process-global, so a test that parks it in `Unavailable` would
/// otherwise make a parallel cache test's write fail.
#[cfg(test)]
pub(super) fn test_lock() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: Mutex<()> = Mutex::new(());
    LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_ready_is_writable_or_sweepable() {
        let _guard = test_lock();
        let saved = state();
        set_unavailable(KeyUnavailable::Missing);
        assert!(!writable() && !sweep_allowed());
        set_blocked(BlockedReason::Damaged);
        assert!(!writable() && !sweep_allowed());
        set_ready(keyring::test_key());
        assert!(writable() && sweep_allowed());
        // Restore whatever the surrounding tests were using.
        match saved {
            StorageState::Unavailable(reason) => set_unavailable(reason),
            StorageState::Blocked(reason) => set_blocked(reason),
            StorageState::Ready => set_ready(keyring::test_key()),
            StorageState::Uninitialized => {
                let mut session = SESSION.lock().unwrap();
                session.state = StorageState::Uninitialized;
            }
        }
    }

    #[test]
    fn state_labels_never_leak_key_material() {
        let _guard = test_lock();
        assert!(!state_label().is_empty());
        let labels = [state_label(), reason_label()];
        for label in labels {
            assert!(
                !label.contains("not-a-secret"),
                "labels must not carry key bytes"
            );
        }
    }

    #[test]
    fn the_generation_only_moves_forward() {
        let _guard = test_lock();
        let before = generation();
        bump_generation();
        assert!(generation() > before);
    }

    #[test]
    fn the_purge_intent_lives_outside_the_cache_directory() {
        let _guard = test_lock();
        // The fallback marker must survive the purge, so it may not sit inside the cache dir
        // that the purge wipes.
        let fallback = fallback_marker_path();
        let cache = clip_image_cache_dir();
        assert!(
            !fallback.starts_with(&cache),
            "fallback {} must not live inside the cache dir {}",
            fallback.display(),
            cache.display()
        );
    }
}
