//! Clipboard subsystem · monitor: pasteboard change polling.

use super::*;

/// Poll once: read the text into history when changeCount changed.
pub(super) fn poll_clipboard() {
    // A smoke launch records nothing from the developer's real pasteboard: its history is
    // encrypted with the fixed test key (published in the source) inside a temporary directory, so
    // real clipboard content recorded there is readable by anyone. This guard sits here as well as
    // in `start` because the process-wide observer and the picker's own copy path call this
    // directly; a smoke that needs entries injects them (`--smoke-clipboard`).
    if SMOKE_MODE.load(Ordering::SeqCst) {
        return;
    }
    // The pasteboard notification may in principle be delivered on another thread; the poll
    // touches AppKit and global UI state, so hop back to the main thread first when needed.
    if !crate::is_main_thread() {
        unsafe {
            let target = observer();
            let _: () = msg_send![
                target,
                performSelectorOnMainThread: sel!(pollClipboardOnMain:),
                withObject: std::ptr::null::<AnyObject>(),
                waitUntilDone: false
            ];
        }
        return;
    }
    // Stop recording when the master switch is off (stop() kills the timer, but the
    // process-wide pasteboard notification observer stays registered, so the callback must
    // check the switch itself -- otherwise history keeps accumulating while disabled).
    if !CONFIG.read().unwrap().clipboard.enabled {
        return;
    }
    // The pause gate. It is consulted before anything is read, and a paused poll only advances the
    // changeCount baseline (a stale baseline is exactly what would make the next resume record the
    // copy that happened during the pause). `Baselining` is also closed: a resume must adopt its
    // baseline before a poll can record.
    if collection_paused() {
        if advance_baseline() {
            log_debug!("[clip] change observed while paused: baseline advanced, nothing recorded");
        }
        return;
    }
    // A recovery from an unavailable or blocked storage adopts a fresh baseline: the copy made while
    // nothing could be recorded must not be recorded now (the same promise the pause makes). A failed
    // baseline leaves the request pending and keeps recording off.
    if handle_recovery_baseline() {
        return;
    }
    // And stop while the storage is unavailable (no key, or the stored history cannot be read):
    // the settings page, the picker and the notification all say the feature is unavailable, so
    // it must not quietly keep collecting records that cannot be kept.
    if unavailable_copy().is_some() {
        return;
    }
    let new_cc: Option<i64> = unsafe {
        let pb: *mut AnyObject = msg_send![class!(NSPasteboard), generalPasteboard];
        if pb.is_null() {
            return;
        }
        let cc: i64 = msg_send![pb, changeCount];
        let mut last = LAST_CHANGE_COUNT.lock().unwrap();
        if *last == cc {
            return;
        }
        let prev = *last;
        *last = cc;
        log_debug!("[clip] pasteboard changeCount {} -> {}", prev, cc);
        Some(cc)
    };
    let cc = if let Some(cc) = new_cc {
        cc
    } else {
        return;
    };
    // Paste-and-delete interception: a change hitting the exact armed target is our own
    // burn-after-paste write-back -- skip it regardless of move_used_to_top (the entry is
    // already deleted and must not resurrect via re-recording). If the count is misaligned,
    // the marker fallback still recognizes our write; without either signal the token is
    // stale and a genuine copy is never swallowed.
    let stored = PASTE_DELETE_SUPPRESS_CC.lock().unwrap().take();
    let marker_present = stored.is_some() && unsafe { pasteboard_has_paste_marker() };
    if paste_delete_suppression_hit(stored, cc, marker_present) {
        log_debug!("[clip] change skipped: our own paste-and-delete write-back");
        return;
    }
    // Sensitive-marker interception: content stamped ConcealedType/TransientType by
    // password managers is skipped entirely -- it never enters the history (neither in
    // memory nor on disk). The industry-standard nspasteboard.org convention.
    if unsafe { pasteboard_has_sensitive_marker() } {
        log_debug!("[clip] change skipped: sensitive/transient marker on pasteboard");
        return;
    }
    // Own-paste write-back interception: when "move used entries to top" is off, a
    // changeCount bump carrying our marker (our own paste write-back) is skipped --
    // otherwise the dedup-move would bring the just-used entry to the top, polluting the
    // copy-order with usage. A real copy clears the pasteboard and the marker with it.
    if should_skip_paste_writeback(move_used_to_top(), unsafe { pasteboard_has_paste_marker() }) {
        log_debug!("[clip] change skipped: our own paste write-back (move_used_to_top off)");
        return;
    }
    // The exclusion list runs BEFORE the payload is read: an excluded copy must never have its
    // content read, hashed, cached or icon-extracted. The frontmost app is resolved once here and
    // reused for the recorded source, so the same lookup never happens twice.
    let copy_source = crate::ffi::frontmost_app_info();
    let (verdict, _frontmost, identity) = copy_exclusion(copy_source.1);
    // Built ONCE here, before the payload is read, so the recorded source cannot describe a
    // different application than the one the exclusion test saw.
    let snapshot = SourceSnapshot {
        name: copy_source.0.clone(),
        pid: copy_source.1,
        identity,
    };
    match verdict {
        crate::clipboard::ExclusionVerdict::Record => {}
        crate::clipboard::ExclusionVerdict::Excluded => {
            log_debug!("[clip] change skipped: source application is on the exclusion list");
            return;
        }
        crate::clipboard::ExclusionVerdict::Blocked => {
            // The source could not be identified, so it cannot be shown to be off the list. Dropping
            // the copy is the only answer that cannot leak content the user asked to exclude.
            log_debug!("[clip] change skipped: the copy source could not be identified");
            return;
        }
    }
    let mut history_changed = false;
    match unsafe { read_pasteboard_text() } {
        Some(text) => {
            // A file-url on the pasteboard means a FILE copy: this app only supports a
            // SINGLE image-file copy (recorded as an image entry); every other file copy
            // -- non-image files, multi-file selections -- is skipped entirely, so the
            // filename text never leaks into the history as plain text. Decide the file case
            // FIRST and resolve the source/icon only when something will actually be
            // recorded, avoiding pointless AppKit work for non-image file copies.
            let has_file_url = unsafe { pasteboard_has_file_url() };
            let file_img = if has_file_url {
                unsafe { file_copy_image(&text) }
            } else {
                None
            };
            let (file_img, file_pending) = match file_img {
                Some((entry, pending)) => (Some(entry), Some(pending)),
                None => (None, None),
            };
            // The capture boundary: the payload has been read, so re-check that the pasteboard still
            // holds the change this candidate came from. Anything below has side effects (history,
            // icon extraction, the deferred cache write), and none of them may run for a candidate
            // another process has already replaced.
            if !capture_boundary_holds(cc) {
                log_debug!("[clip] change skipped: pasteboard changed while the copy was read");
                return;
            }
            if let Some(pending) = file_pending {
                pending.commit();
            }
            let skip_file_text = has_file_url && file_img.is_none();
            let (source, source_key) = if skip_file_text {
                (String::new(), String::new())
            } else {
                record_source(&snapshot)
            };
            let mut hist = CLIP_HISTORY.lock().unwrap();
            if let Some(img) = &file_img {
                if record_image(&mut hist, img, &source, &source_key, max_entries()) {
                    history_changed = true;
                    // File copies log only their type and count, never the source path or content.
                    log_debug!(
                        "[clip] recorded file ref (uti={}, total {})",
                        img.uti,
                        hist.len()
                    );
                } else {
                    log_debug!("[clip] change skipped: dup file ref, total {}", hist.len());
                }
            } else if skip_file_text {
                // A file copy that isn't a single image file (non-image / multi-file):
                // skipped, the filename text is never recorded.
                log_debug!(
                    "[clip] change skipped: file copy without a single image file ({} chars)",
                    text.chars().count()
                );
            } else if record_text(&mut hist, &text, &source, &source_key, max_entries()) {
                history_changed = true;
                log_debug!(
                    "[clip] recorded text ({} chars, total {})",
                    text.chars().count(),
                    hist.len()
                );
            } else {
                log_debug!(
                    "[clip] change skipped: dup/empty (text {} chars, total {})",
                    text.chars().count(),
                    hist.len()
                );
            }
            // Expire right after recording (lazy, no extra timer; the picker summon
            // cleans again).
            history_changed |= expire_entries(&mut hist, now_secs(), ttl_secs()) > 0;
        }
        // No text -> try an image (text wins when both are present; a v1 tradeoff).
        None => match unsafe { read_pasteboard_image() } {
            Some((img, pending)) => {
                if !capture_boundary_holds(cc) {
                    log_debug!(
                        "[clip] change skipped: pasteboard changed while the image was read"
                    );
                    return;
                }
                pending.commit();
                let (source, source_key) = record_source(&snapshot);
                let mut hist = CLIP_HISTORY.lock().unwrap();
                if record_image(&mut hist, &img, &source, &source_key, max_entries()) {
                    history_changed = true;
                    log_debug!(
                        "[clip] recorded image (hash={:016x}, uti={}, total {})",
                        img.hash,
                        img.uti,
                        hist.len()
                    );
                    // Detail-preview pregeneration is already scheduled in the background
                    // with the cache-write job (see
                    // image_cache::schedule_image_cache_write); do not enqueue it again here.
                } else {
                    log_debug!("[clip] change skipped: dup image (hash={:016x})", img.hash);
                }
                // Expire right after recording (same as the text branch).
                history_changed |= expire_entries(&mut hist, now_secs(), ttl_secs()) > 0;
            }
            None => log_debug!("[clip] change but no text/image (non-pasteboard content?)"),
        },
    }
    // Serialize and persist only after a real model change; no-op pasteboard changes must
    // not clone, serialize, and atomically rewrite the entire history.
    if history_changed {
        save_history();
        schedule_picker_refresh();
    }
}

/// Whether this copy's source is on the user's exclusion list.
///
/// Both signals are consulted and either match excludes: the frontmost application (the app the
/// user was in when the copy happened) and, when present, the copier declared by
/// `org.nspasteboard.source`. The declared identifier is self-reported by the writer, so it is an
/// ADDITIONAL signal only -- it can never override the frontmost verdict, which would re-admit a
/// copy from an excluded app that happened to declare another identifier.
/// Resolve the copy's source ONCE and decide whether it may be recorded.
///
/// The identity is resolved here and the result is what `record_source` is given later, so the
/// exclusion test and the recorded source cannot describe two different applications (they used to be
/// two separate lookups of the same PID).
/// Resolve the copy's source ONCE and decide whether it may be recorded.
///
/// The identity comes from a single `NSRunningApplication` lookup, and the record path is given the
/// SAME snapshot. Two separate lookups (one for the exclusion test, one for the recorded source) can
/// describe two different applications when the process quits in between -- and the earlier shape had
/// exactly that: the outer lookup succeeded, the inner `bundle_id_for_pid` re-queried, returned `None`,
/// and the copy was admitted as an app with no identifier instead of a failed lookup.
fn copy_exclusion(
    front_pid: i32,
) -> (
    crate::clipboard::ExclusionVerdict,
    crate::clipboard::FrontmostSource,
    Option<crate::app_identity::AppIdentity>,
) {
    // Clone the short list rather than hold the CONFIG read guard across Objective-C calls.
    let list = CONFIG
        .read()
        .map(|c| c.clipboard.excluded_apps.clone())
        .unwrap_or_default();
    // ONE lookup. The verdict and the recorded identity are built from the same application
    // object, so they cannot describe different applications when the process quits in between.
    let (source, identity) = resolve_frontmost_source(front_pid);
    let declared = unsafe { declared_source_bundle_id() };
    let verdict = crate::clipboard::exclusion_decision(&list, &source, declared.as_deref());
    (verdict, source, identity)
}

/// One `NSRunningApplication` lookup, turned into the enum the decision table needs.
///
/// The bundle identifier is read from the SAME application object that proved the PID is alive, so
/// "no identifier" (a real state) and "the lookup failed" (the app is gone) cannot be confused.
fn resolve_frontmost_source(
    pid: i32,
) -> (
    crate::clipboard::FrontmostSource,
    Option<crate::app_identity::AppIdentity>,
) {
    use objc2::msg_send;
    use objc2::runtime::AnyObject;
    if pid <= 0 {
        return (crate::clipboard::FrontmostSource::LookupFailed, None);
    }
    unsafe {
        let app: *mut AnyObject = msg_send![
            class!(NSRunningApplication),
            runningApplicationWithProcessIdentifier: pid
        ];
        if app.is_null() {
            // The process is gone: a stale PID is a failed lookup, not an app without an id.
            return (crate::clipboard::FrontmostSource::LookupFailed, None);
        }
        (
            crate::clipboard::FrontmostSource::Resolved {
                bundle_id: crate::app_identity::bundle_id_of(app),
            },
            // From the SAME object that answered the exclusion question.
            Some(crate::app_identity::identity_of_app(app, pid)),
        )
    }
}

/// Resolve the frontmost app at copy time: returns (app name, icon-cache key) and extracts
/// the 16pt small icon. The app is alive now (the most reliable moment; failure only means
/// no icon). Reuses the resolved identity so icon extraction never resolves the same PID
/// twice (a duplicate NSRunningApplication lookup + executable stat).
/// Whether the pasteboard still holds the change a candidate was read from. A different count
/// means another process replaced the content while the payload was being read, so the candidate is
/// dropped rather than recorded (and its deferred cache write never happens).
fn capture_boundary_holds(expected: i64) -> bool {
    super::current_change_count() == Some(expected)
}

/// The source identity captured ONCE at copy time, before the payload is read.
///
/// Why a value rather than a PID: `record_source` used to re-resolve the PID after the payload was
/// read. If the app quit in between, the recorded source dropped to a `pid_*` cache key; if the PID was
/// reused, the record could carry ANOTHER application's identity and icon. The exclusion verdict and the
/// recorded source now come from one lookup.
pub(super) struct SourceSnapshot {
    /// Display name of the frontmost app.
    pub(super) name: String,
    /// Its PID, for icon extraction only.
    pub(super) pid: i32,
    /// The cache identity resolved at copy time, when the PID was known to be alive.
    pub(super) identity: Option<crate::app_identity::AppIdentity>,
}

/// Record a copy's source from the snapshot taken before the payload was read.
pub(super) fn record_source(snapshot: &SourceSnapshot) -> (String, String) {
    let Some(identity) = &snapshot.identity else {
        // The lookup failed at copy time, so there is no identity to attach; the copy is still
        // recorded under the name only.
        return (snapshot.name.clone(), String::new());
    };
    let key = identity.key.clone();
    if snapshot.pid > 0 {
        let _ = crate::icon_cache::extract_small_icon_for_identity(snapshot.pid, identity);
    }
    (snapshot.name.clone(), key)
}

/// Timer tick callback (main thread): keep polling.
pub(super) extern "C" fn clip_poll_tick(_self: *mut c_void, _cmd: Sel, _timer: *mut c_void) {
    crate::callback_guard::void("clip_poll_tick", poll_clipboard);
}

/// Main-thread re-entry for a pasteboard notification delivered off the main thread (see the
/// top of poll_clipboard).
pub(super) extern "C" fn clip_poll_on_main(_self: *mut c_void, _cmd: Sel, _note: *mut c_void) {
    crate::callback_guard::void("clip_poll_on_main", poll_clipboard);
}

/// Start polling (idempotent): create a main-thread NSTimer and record the current
/// pasteboard once immediately.
pub(crate) fn start() {
    unsafe {
        let timer_holder =
            POLL_TIMER.get_or_init(|| MainThreadSlot::new(ObjPtr::new(std::ptr::null_mut())));
        let mut guard = timer_holder.lock().unwrap();
        if !guard.0.is_null() {
            return; // already running
        }
        // The image cache is kept across launches (the history references it) and the history is
        // loaded BEFORE recording the current pasteboard, so a restart continues where it left
        // off. The load runs off the main thread (the keychain read may block on a system prompt)
        // and applies its result here when it finishes: the merge, the orphan sweep and the detail
        // warm-up all happen in that apply step, because the sweep must only run after a
        // successful load (invariant R2) and the merge may regenerate previews (AppKit).
        load_history();
        // Expire at startup: the loaded history can hold entries that aged out while the app
        // was not running. Entries recorded before the load lands expire on the next pass.
        {
            let mut hist = CLIP_HISTORY.lock().unwrap();
            expire_entries(&mut hist, now_secs(), ttl_secs());
        }
        // Then record the current pasteboard, or the first summon would show an empty list.
        // A smoke launch does neither that nor the monitoring below: it must not read the real
        // pasteboard at all (see `poll_clipboard`), so it installs no observer and no timer -- the
        // tier-A smoke asserts that state. The load, the expiry pass and the picker warm-up still
        // run, so the storage state and the rows the smokes exercise are unchanged.
        let smoke = SMOKE_MODE.load(Ordering::SeqCst);
        if !smoke {
            // Seed the effective state from the persisted intent BEFORE the first poll. A paused
            // start adopts a baseline and records nothing; an unpaused start leaves the baseline
            // unset on purpose, so the immediate poll records the current pasteboard (the existing
            // "the first summon is not empty" behaviour).
            init_collection_gate();
            poll_clipboard();
        }
        // Following Maccy's model, warm the long-lived panel and initial row tree while hidden;
        // the shortcut only presents content that is already ready.
        PICKER_REFRESH_PENDING.store(false, Ordering::SeqCst);
        ensure_picker_window();
        rebuild_rows();
        // The panel remains hidden during startup; draw it once into its backing store so the
        // first summon does not make orderFrontRegardless pay for the entire list and glass.
        if let Some(window) = *PICKER_WINDOW.lock().unwrap() {
            let _: () = msg_send![window.0, displayIfNeeded];
        }
        if smoke {
            return;
        }
        // Register the pasteboard-change notification: instant recording on every change, so
        // rapid consecutive copies between polling samples are not lost.
        register_pasteboard_observer();
        let timer: *mut AnyObject = msg_send![
            class!(NSTimer),
            scheduledTimerWithTimeInterval: POLL_INTERVAL,
            target: timer_target(),
            selector: sel!(clipPollTick:),
            userInfo: std::ptr::null::<AnyObject>(),
            repeats: true
        ];
        *guard = ObjPtr::new(timer);
        log_debug!(
            "Clipboard history polling started (every {}s).",
            POLL_INTERVAL
        );
    }
}

/// Whether this process is monitoring the system pasteboard (the polling timer is installed). A
/// smoke launch leaves it uninstalled -- see `start` -- which is the state the tier-A smoke asserts.
pub(super) fn polling_active() -> bool {
    crate::debug_assert_main_thread();
    POLL_TIMER
        .get()
        .is_some_and(|holder| !holder.lock().unwrap().0.is_null())
}

/// Stop polling (idempotent).
pub(crate) fn stop() {
    // A hand-back armed before the feature was switched off must not send a keystroke afterwards: the
    // panel is going away and the tick's enabled check would already refuse it, but cancelling here
    // also releases the burn suppression token promptly instead of leaving it for a timer.
    super::paste_handback::cancel();
    unsafe {
        let timer_holder =
            POLL_TIMER.get_or_init(|| MainThreadSlot::new(ObjPtr::new(std::ptr::null_mut())));
        let mut guard = timer_holder.lock().unwrap();
        if !guard.0.is_null() {
            let _: () = msg_send![guard.0, invalidate];
            // scheduledTimerWithTimeInterval: returns +0 (owned by the run loop); it must
            // NOT be released (over-release crashes); invalidate lets the run loop release it.
            *guard = ObjPtr::new(std::ptr::null_mut());
            log_debug!("Clipboard history polling stopped.");
        }
    }
}
