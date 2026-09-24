//! Clipboard subsystem · monitor: pasteboard change polling.

use super::*;

/// Poll once: read the text into history when changeCount changed.
pub(super) fn poll_clipboard() {
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
            let skip_file_text = has_file_url && file_img.is_none();
            let (source, source_key) = if skip_file_text {
                (String::new(), String::new())
            } else {
                record_source()
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
            Some(img) => {
                let (source, source_key) = record_source();
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

/// Resolve the frontmost app at copy time: returns (app name, icon-cache key) and extracts
/// the 16pt small icon. The app is alive now (the most reliable moment; failure only means
/// no icon). Reuses the resolved identity so icon extraction never resolves the same PID
/// twice (a duplicate NSRunningApplication lookup + executable stat).
fn record_source() -> (String, String) {
    let (source, pid) = crate::ffi::frontmost_app_info();
    if pid <= 0 {
        return (source, String::new());
    }
    let id = unsafe { crate::app_identity::resolve_app_identity(pid) };
    let key = id.key.clone();
    let _ = crate::icon_cache::extract_small_icon_for_identity(pid, &id);
    (source, key)
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
        // With persist ON: the cache is kept (the previous session's image bytes/previews
        // are still referenced) and the history is loaded BEFORE recording the current
        // pasteboard. With persist OFF (default): the cache is wiped -- the history is not
        // persisted, so leftovers are orphans; sweeping first keeps the just-written cache
        // from being deleted.
        if persist_enabled() {
            load_history();
        } else {
            clear_clip_image_cache();
        }
        // Expire at startup (load_history only covers the persist-on case; the in-memory
        // history needs expiry with persist off too).
        {
            let mut hist = CLIP_HISTORY.lock().unwrap();
            expire_entries(&mut hist, now_secs(), ttl_secs());
        }
        if persist_enabled() {
            let removed = sweep_current_clip_image_cache();
            if removed > 0 {
                log_debug!("[clip] swept {} orphan image cache files", removed);
            }
            // Warm up detail previews for restored image entries (background): after a
            // restart {hash}.detail may not exist yet (the last session never opened that
            // detail), so generating ahead keeps the first open instantly sharp.
            for entry in CLIP_HISTORY.lock().unwrap().iter() {
                if let Some(img) = &entry.image {
                    request_detail_preview(img, false);
                }
            }
        }
        // Then record the current pasteboard, or the first summon would show an empty list.
        poll_clipboard();
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

/// Stop polling (idempotent).
pub(crate) fn stop() {
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
