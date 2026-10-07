//! Background AX phase (dedicated raiser thread) with generation-guarded ordering.

use super::*;

// Generation of the latest raise intent: bumped on every committed switch. Background jobs
// re-check before applying AX mutations and abort once a newer switch supersedes them --
// during rapid consecutive switches, a stale job must never re-raise an old window over the
// newest one (out-of-order flicker).
pub(super) static RAISE_GENERATION: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(0);

pub(super) fn raise_intent_current(generation: u64) -> bool {
    RAISE_GENERATION.load(std::sync::atomic::Ordering::Acquire) == generation
}

struct RaiseJob {
    pid: i32,
    cgwid: u32,
    minimized: bool,
    fast_path_ok: bool,
    // The target lives on another desktop: the commit path fronts its app (or, with the development
    // switch, deliberately does not, so the front-switch rescue is what must move the Space). This
    // job waits for the switch, rescues when it does not arrive, then applies the exact raise.
    on_other_desktop: bool,
    /// Whether the commit path's app activation was accepted (`false` also covers the development
    /// switch that skips it).
    activation: bool,
    generation: u64,
    enqueued_at: Instant,
}

/// AX mutations are delivered to AppKit and must run on the main thread.  Keep the potentially
/// blocking AX lookup on `ax-raiser`, then retain the resolved objects until the main-thread
/// callback applies the final raise/focus actions.
struct MainThreadAxRaise {
    pid: i32,
    cgwid: u32,
    app: AXUIElementRef,
    element: AXUIElementRef,
    focused_key: AXUIElementRef,
    raise_key: AXUIElementRef,
    minimized_key: Option<AXUIElementRef>,
    force_focus: bool,
    generation: u64,
}

unsafe impl Send for MainThreadAxRaise {}

static MAIN_THREAD_AX_RAISES: LazyLock<Mutex<Vec<MainThreadAxRaise>>> =
    LazyLock::new(|| Mutex::new(Vec::new()));

/// Drain AX mutations on the AppKit main thread.  `AXUIElementPerformAction` can synchronously
/// enter AppKit (for example `makeKeyAndOrderFront:`), so invoking it from the background AX
/// worker triggers macOS's "Must only be used from the main thread" trap.
pub(crate) fn handle_ax_raise_main() {
    let jobs = MAIN_THREAD_AX_RAISES
        .lock()
        .unwrap()
        .drain(..)
        .collect::<Vec<_>>();
    for job in jobs {
        unsafe {
            if !raise_intent_current(job.generation) {
                release_main_thread_ax_raise(&job);
                continue;
            }

            let minimized_set_err = job
                .minimized_key
                .map(|key| AXUIElementSetAttributeValue(job.element, key, kCFBooleanFalse));
            if minimized_set_err.is_some() {
                let (slps_ok, click_ok) = raise_window_fast(job.pid, job.cgwid);
                log_debug!(
                    "[raise] precise fast after main-thread unminimize: pid={} cgwid={} slps={} click={}",
                    job.pid,
                    job.cgwid,
                    slps_ok,
                    click_ok
                );
            }
            let raise_started = Instant::now();
            let raise_first_err = AXUIElementPerformAction(job.element, job.raise_key);
            let raise_first_us = raise_started.elapsed().as_micros();
            // Drop the focus backstop and its retry once the target times out (-25204): both calls
            // would only wait out another timeout each -- the log shows first/set_focused/retry all
            // returning -25204 with the main thread blocked for ~4.5s. The SLPS fast raise already
            // ran, so give up the AX backstop and collapse three timeouts into one.
            let timed_out = raise_first_err == K_AX_CANNOT_COMPLETE;
            let (focused_set_err, raise_retry_err) = if timed_out
                || (!job.force_focus
                    && (raise_first_err == K_AX_SUCCESS
                        || raise_first_err == K_AX_INVALID_UI_ELEMENT))
            {
                (None, None)
            } else {
                let focused_set_err =
                    AXUIElementSetAttributeValue(job.app, job.focused_key, job.element);
                let raise_retry_err = AXUIElementPerformAction(job.element, job.raise_key);
                (Some(focused_set_err), Some(raise_retry_err))
            };
            log_debug!(
                "[raise] AXRaise main: pid={} cgwid={} first_err={} first_us={} set_focused={:?} retry={:?} set_minimized={:?}",
                job.pid,
                job.cgwid,
                raise_first_err,
                raise_first_us,
                focused_set_err,
                raise_retry_err,
                minimized_set_err
            );
            release_main_thread_ax_raise(&job);
        }
    }
}

unsafe fn release_main_thread_ax_raise(job: &MainThreadAxRaise) {
    CFRelease(job.app);
    CFRelease(job.element);
    CFRelease(job.focused_key);
    CFRelease(job.raise_key);
    if let Some(key) = job.minimized_key {
        CFRelease(key);
    }
}

#[allow(clippy::too_many_arguments)]
unsafe fn enqueue_main_thread_ax_raise(
    pid: i32,
    cgwid: u32,
    app: AXUIElementRef,
    element: AXUIElementRef,
    focused_key: AXUIElementRef,
    raise_key: AXUIElementRef,
    minimized_key: Option<AXUIElementRef>,
    force_focus: bool,
    generation: u64,
) {
    if !raise_intent_current(generation) {
        return;
    }
    // The caller retains these objects for its own cleanup.  Take an additional retain for the
    // queued main-thread job, including the array-borrowed window element.
    CFRetain(app);
    CFRetain(element);
    CFRetain(focused_key);
    CFRetain(raise_key);
    if let Some(key) = minimized_key {
        CFRetain(key);
    }
    let pending = {
        let mut pending = MAIN_THREAD_AX_RAISES.lock().unwrap();
        let old = std::mem::take(&mut *pending);
        pending.push(MainThreadAxRaise {
            pid,
            cgwid,
            app,
            element,
            focused_key,
            raise_key,
            minimized_key,
            force_focus,
            generation,
        });
        old
    };
    // Keep only the newest generation while the main thread is busy; release retained AX
    // objects from superseded jobs immediately.
    for old in pending {
        release_main_thread_ax_raise(&old);
    }

    if let Some(controller) = crate::CONTROLLER.lock().unwrap().map(|ptr| ptr.0) {
        let _: () = msg_send![controller,
            performSelectorOnMainThread: sel!(handleAxRaise:),
            withObject: std::ptr::null::<AnyObject>(),
            waitUntilDone: false
        ];
    } else {
        // This can only happen during early shutdown/startup, but do not leave retained AX
        // objects behind if the controller has already gone away.
        let pending = MAIN_THREAD_AX_RAISES
            .lock()
            .unwrap()
            .drain(..)
            .collect::<Vec<_>>();
        for job in pending {
            release_main_thread_ax_raise(&job);
        }
    }
}

// A single dedicated raiser thread consumes jobs serially, so AX phases of consecutive
// switches never overlap and completion order equals commit order (combined with the
// supersede check, the final state is always the last switch).
struct RaiseQueue {
    tx: flume::Sender<RaiseJob>,
    rx: flume::Receiver<RaiseJob>,
}

static RAISE_QUEUE: std::sync::LazyLock<RaiseQueue> = std::sync::LazyLock::new(|| {
    let (tx, rx) = flume::bounded::<RaiseJob>(1);
    let worker_rx = rx.clone();
    std::thread::Builder::new()
        .name("ax-raiser".into())
        .spawn(move || {
            for job in worker_rx.iter() {
                run_raise_ax_job(job);
            }
        })
        .expect("spawn ax-raiser thread");
    RaiseQueue { tx, rx }
});

/// Enqueue the serialized AX backstop. Normal windows only perform cached AXRaise; known
/// minimized windows are restored first.
///
/// AX enumeration can block tens to hundreds of milliseconds on an unresponsive app; it must
/// stay off the main thread.
pub(crate) fn raise_window_ax_async(
    pid: i32,
    cgwid: u32,
    minimized: bool,
    fast_path_ok: bool,
    on_other_desktop: bool,
    activation: bool,
) -> u64 {
    if cgwid == 0 {
        return 0;
    }
    let generation = RAISE_GENERATION.fetch_add(1, std::sync::atomic::Ordering::AcqRel) + 1;
    let job = RaiseJob {
        pid,
        cgwid,
        minimized,
        fast_path_ok,
        on_other_desktop,
        activation,
        generation,
        enqueued_at: Instant::now(),
    };
    match RAISE_QUEUE.tx.try_send(job) {
        Ok(()) => {}
        Err(flume::TrySendError::Full(job)) => {
            // A full slot means the old job has not started; replace it with the latest
            // generation instead of letting stale raises accumulate.
            let _ = RAISE_QUEUE.rx.try_recv();
            if RAISE_QUEUE.tx.try_send(job).is_err() {
                log_debug!("[raise] latest job could not replace queued job");
            }
        }
        Err(flume::TrySendError::Disconnected(_)) => {
            log_debug!("[raise] ax-raiser queue disconnected");
        }
    }
    generation
}

fn run_raise_ax_job(job: RaiseJob) {
    if !raise_intent_current(job.generation) {
        log_debug!(
            "[raise] ax superseded before start: pid={} cgwid={} gen={}",
            job.pid,
            job.cgwid,
            job.generation
        );
        return;
    }
    let started = Instant::now();
    log_debug!(
        "[raise] ax job start: pid={} cgwid={} gen={} queue_ms={}",
        job.pid,
        job.cgwid,
        job.generation,
        job.enqueued_at.elapsed().as_millis()
    );
    // Always wrap background work in an autorelease pool: this path only touches CF objects
    // today; the pool guards against leaks if ObjC calls are ever added.
    unsafe {
        let pool: *mut AnyObject = msg_send![class!(NSAutoreleasePool), new];
        if job.on_other_desktop {
            // Nothing AX-shaped to do: the window has no AX element on this side of the switch,
            // and fronting the app again could raise a sibling window that is already visible.
            // The commit path has already fronted the app; this waits for the desktop to follow
            // and lands the exact window.
            raise_other_desktop_window(&job, started, job.fast_path_ok, job.activation);
        } else {
            let force_ax_focus = if job.minimized || job.fast_path_ok {
                !job.fast_path_ok
            } else {
                !retry_failed_fast_path(&job)
            };
            raise_window_ax_job(&job, started, force_ax_focus);
        }
        let _: () = msg_send![pool, drain];
    }
}

/// How long the cross-desktop raise gives the Space to become active, when to apply the
/// exact-window front-switch as the rescue, and the polling step.
///
/// The budget is generous because the transition is animated: a switch that lands at all can flip
/// `isOnscreen` well after the call returns, and a short budget abandons a switch already in flight
/// (observed: a recorded `onscreen=false` followed by the Space changing afterwards). If nothing
/// lands, the AX phase still runs and the record says `onscreen=false`.
const OTHER_DESKTOP_SETTLE_BUDGET: Duration = Duration::from_millis(3000);
const OTHER_DESKTOP_ACTIVATION_BUDGET: Duration = Duration::from_millis(400);
const OTHER_DESKTOP_SETTLE_STEP: Duration = Duration::from_millis(20);
/// Extra window for a switch that only lands while the AX phase is already running.
const OTHER_DESKTOP_LATE_SETTLE_BUDGET: Duration = Duration::from_millis(1500);

/// Wait (bounded, generation-checked) for the target window to join the active desktop.
unsafe fn wait_for_target_onscreen(job: &RaiseJob, budget: Duration, waited_ms: &mut u128) -> bool {
    let deadline = Instant::now() + budget;
    loop {
        if !raise_intent_current(job.generation) {
            return false;
        }
        if crate::window_collector::window_is_onscreen_now(job.cgwid) {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(OTHER_DESKTOP_SETTLE_STEP);
        *waited_ms += OTHER_DESKTOP_SETTLE_STEP.as_millis();
    }
}

/// Land the exact window after its desktop has become active.
///
/// Two attempts can move the Space, in this order:
///
/// 1. app activation (`NSRunningApplication.activateWithOptions:`), which the commit path already
///    performed;
/// 2. the exact-window front-switch (SLPS with the window id + the targeted click), applied only
///    when the window has not joined the active desktop within `OTHER_DESKTOP_ACTIVATION_BUDGET`.
///    macOS refuses attempt 1 in some states (`activateWithOptions=false` with the target never
///    becoming frontmost -- observed in the user's log), and both reference implementations use
///    attempt 2 for a cross-Space target, so it is the rescue rather than an unused belt.
///
/// Which attempt moved the Space, and the rescue's own result, are published with the target
/// identity so the branch is assertable (`scripts/e2e/space-desktops.sh` runs one pass with
/// activation suppressed so the rescue must do the work alone).
///
/// The wait is tied to the fact being waited for (`window_is_onscreen_now`) and the generation is
/// re-checked every step, so a newer switch cancels this raise. Once the window is part of the
/// active desktop it is in `kAXWindows` again, so the exact window is reachable through the same AX
/// path every other card uses -- which is also what makes a known-minimized window recoverable here.
unsafe fn raise_other_desktop_window(
    job: &RaiseJob,
    started: Instant,
    fast_path_ok: bool,
    activation: bool,
) {
    let mut waited_ms = 0u128;

    // Attempt 1 was the app activation the commit path already performed; give it a short window of
    // its own before adding the rescue.
    let mut onscreen =
        wait_for_target_onscreen(job, OTHER_DESKTOP_ACTIVATION_BUDGET, &mut waited_ms);
    let mut rescue_attempted = false;
    let mut rescue = false;
    if !onscreen && raise_intent_current(job.generation) {
        // Attempt 2: the exact-window front-switch (SLPS with the window id + the targeted click).
        // Both reference implementations use it for a cross-Space target; here it is the rescue for
        // the states where macOS refuses to activate the app.
        rescue_attempted = true;
        let (slps_ok, click_ok) = raise_window_fast(job.pid, job.cgwid);
        rescue = slps_ok && click_ok;
        log_debug!(
            "[raise] other-desktop front-switch rescue: pid={} cgwid={} commit_fast_path_ok={} slps={} click={} waited={}ms",
            job.pid,
            job.cgwid,
            fast_path_ok,
            slps_ok,
            click_ok,
            waited_ms
        );
        let remaining = OTHER_DESKTOP_SETTLE_BUDGET.saturating_sub(Duration::from_millis(
            waited_ms.min(u128::from(u64::MAX)) as u64,
        ));
        onscreen = wait_for_target_onscreen(job, remaining, &mut waited_ms);
    }
    if !raise_intent_current(job.generation) {
        return;
    }

    let mut ax_matched = raise_window_ax_job(job, started, false);
    // The transition is animated, so the window can join the active desktop while the AX phase is
    // already running -- and that phase then matched an element that was not on the active desktop,
    // where an AXRaise does nothing. Re-check once and re-apply, so a late arrival still gets the
    // exact raise instead of only the Space switch.
    if !onscreen
        && raise_intent_current(job.generation)
        && wait_for_target_onscreen(job, OTHER_DESKTOP_LATE_SETTLE_BUDGET, &mut waited_ms)
    {
        onscreen = true;
        let late_matched = raise_window_ax_job(job, started, false);
        ax_matched = late_matched || ax_matched;
        log_debug!(
            "[raise] other-desktop late arrival: pid={} cgwid={} late_matched={} waited={}ms",
            job.pid,
            job.cgwid,
            late_matched,
            waited_ms
        );
    }
    log_debug!(
        "[raise] other-desktop raise: pid={} cgwid={} onscreen={} ax_matched={} activation={} rescue_attempted={} rescue={} waited={}ms total={}ms",
        job.pid,
        job.cgwid,
        onscreen,
        ax_matched,
        activation,
        rescue_attempted,
        rescue,
        waited_ms,
        started.elapsed().as_millis()
    );
    crate::e2e_state::record_other_desktop_raise(
        job.pid,
        job.cgwid,
        job.generation,
        onscreen,
        ax_matched,
        activation,
        rescue_attempted,
        rescue,
    );
}

/// Recover a failed synchronous raise without blocking the main thread.
/// The first attempt already ran at commit time; these retries cover the short-lived
/// process-table/WindowServer race seen when switching away from another application.
unsafe fn retry_failed_fast_path(job: &RaiseJob) -> bool {
    const RETRY_DELAYS_MS: [u64; 2] = [8, 20];
    let started = Instant::now();
    let activate_ok = activate_pid(job.pid);
    let mut last = (false, false);
    let mut attempts = 0;
    for delay_ms in RETRY_DELAYS_MS {
        if !raise_intent_current(job.generation) {
            log_debug!(
                "[raise] fast recovery superseded: pid={} cgwid={} gen={}",
                job.pid,
                job.cgwid,
                job.generation
            );
            return false;
        }
        std::thread::sleep(Duration::from_millis(delay_ms));
        attempts += 1;
        last = raise_window_fast(job.pid, job.cgwid);
        if last.0 && last.1 {
            log_debug!(
                "[raise] fast recovery succeeded: pid={} cgwid={} activate={} attempts={} elapsed={}ms",
                job.pid,
                job.cgwid,
                activate_ok,
                attempts,
                started.elapsed().as_millis()
            );
            return true;
        }
    }
    log_debug!(
        "[raise] fast recovery exhausted: pid={} cgwid={} activate={} slps={} click={} attempts={} elapsed={}ms",
        job.pid,
        job.cgwid,
        activate_ok,
        last.0,
        last.1,
        attempts,
        started.elapsed().as_millis()
    );
    false
}

/// AX phase: on a cache hit, normal windows only perform AXRaise; known minimized windows are
/// restored first and then run the fast path. Only a stale/missing cache enumerates AXWindows,
/// pairs by CGWindowID, and refreshes the cache.
///
/// Returns whether the exact window was matched and its raise applied. `false` covers both "no
/// element for this CGWindowID" and "could not talk to the app" -- the caller that ignores the
/// value is the normal path, where the SLPS fast raise is the evidence; the other-desktop raise
/// records it, because there the fast raise is not evidence of anything.
unsafe fn raise_window_ax_job(job: &RaiseJob, started: Instant, force_ax_focus: bool) -> bool {
    let app_started = Instant::now();
    let app = AXUIElementCreateApplication(job.pid);
    if app.is_null() {
        log_info!("[raise] ax skipped: no AX app for pid={}", job.pid);
        return false;
    }
    let process_start_time_us = resolve_app_identity(job.pid).process_start_time_us;
    let app_create_us = app_started.elapsed().as_micros();
    // Every AX call on the raise path is bounded by AX_RAISE_MESSAGING_TIMEOUT: the window element
    // is set inside raise_ax_element, the app element here (it is not inherited by children).
    AXUIElementSetMessagingTimeout(app, AX_RAISE_MESSAGING_TIMEOUT);

    let raise_key = cf_string_new("AXRaise");
    let focused_key = cf_string_new("AXFocusedWindow");
    let minimized_key = job.minimized.then(|| cf_string_new("AXMinimized"));

    // collect_windows retains the AX element for the current window. Reuse it for normal
    // switches to avoid an AXWindows IPC round trip; only stale elements use the live scan below.
    if let Some(element) = cached_ax_window_element(job.pid, process_start_time_us, job.cgwid) {
        if !raise_intent_current(job.generation) {
            CFRelease(element);
            CFRelease(raise_key);
            CFRelease(focused_key);
            if let Some(minimized_key) = minimized_key {
                CFRelease(minimized_key);
            }
            CFRelease(app);
            // Superseded before applying: this job must not report a match it never made.
            return false;
        }
        // Unminimize is an AX mutation and is performed by the main-thread queue below.
        let minimized_set_err: Option<AXError> = None;
        let (raise_first_err, focused_set_err, raise_retry_err) = raise_ax_element(
            job.pid,
            job.cgwid,
            app,
            element,
            focused_key,
            raise_key,
            minimized_key,
            force_ax_focus,
            job.generation,
        );
        let effective_raise_err = raise_retry_err.unwrap_or(raise_first_err);
        if effective_raise_err != K_AX_INVALID_UI_ELEMENT {
            log_debug!(
                "[raise] ax raised cached: pid={} cgwid={} known_minimized={} set_minimized={:?} raise_first={} set_focused={:?} raise_retry={:?} app_create_us={} waited={}ms total={}ms",
                job.pid,
                job.cgwid,
                job.minimized,
                minimized_set_err,
                raise_first_err,
                focused_set_err,
                raise_retry_err,
                app_create_us,
                job.enqueued_at.elapsed().as_millis(),
                started.elapsed().as_millis()
            );
            CFRelease(element);
            CFRelease(raise_key);
            CFRelease(focused_key);
            if let Some(minimized_key) = minimized_key {
                CFRelease(minimized_key);
            }
            CFRelease(app);
            // The cached element is the exact window and its raise was applied.
            return true;
        }
        log_debug!(
            "[raise] cached AX element stale: pid={} cgwid={} raise={} — refreshing",
            job.pid,
            job.cgwid,
            effective_raise_err
        );
        invalidate_cached_ax_window_element(job.pid, process_start_time_us, job.cgwid, element);
        CFRelease(element);
    }

    // Read the same three app-level slots as collection: AXWindows is Space-filtered, while
    // AXFocusedWindow and AXMainWindow can still identify a window on another Space.
    let lookup_keys = [
        cf_string_new("AXWindows"),
        cf_string_new("AXFocusedWindow"),
        cf_string_new("AXMainWindow"),
    ];
    let lookup_keys_array = CFArrayCreate(
        std::ptr::null(),
        lookup_keys.as_ptr(),
        lookup_keys.len() as isize,
        std::ptr::null(),
    );
    let mut lookup_slots: *const c_void = std::ptr::null();
    let ax_query_err = if lookup_keys_array.is_null() {
        -1
    } else {
        let result =
            AXUIElementCopyMultipleAttributeValues(app, lookup_keys_array, 0, &mut lookup_slots);
        CFRelease(lookup_keys_array);
        result
    };
    for key in lookup_keys {
        if !key.is_null() {
            CFRelease(key);
        }
    }

    let windows_array = if ax_query_err == K_AX_SUCCESS {
        ax_slot_value(lookup_slots, 0).filter(|value| CFGetTypeID(*value) == CFArrayGetTypeID())
    } else {
        None
    };
    let ax_window_count = if ax_query_err == K_AX_SUCCESS {
        windows_array.map_or(0, |array| CFArrayGetCount(array))
    } else {
        -1
    };
    let mut published = Vec::new();
    if let Some(array) = windows_array {
        for index in 0..CFArrayGetCount(array) {
            let element = CFArrayGetValueAtIndex(array, index);
            if !element.is_null() {
                published.push((ax_window_cgwid(element).unwrap_or(0), element));
            }
        }
    }
    let focused_window = ax_slot_value(lookup_slots, 1)
        .filter(|element| CFGetTypeID(*element) == AXUIElementGetTypeID())
        .map(|element| (ax_window_cgwid(element).unwrap_or(0), element));
    let main_window = ax_slot_value(lookup_slots, 2)
        .filter(|element| CFGetTypeID(*element) == AXUIElementGetTypeID())
        .map(|element| (ax_window_cgwid(element).unwrap_or(0), element));
    let candidates = candidate_window_elements(&published, focused_window, main_window);
    let ax_window_ids = candidates
        .iter()
        .filter_map(|(cgwid, _, only_from_slots)| (!only_from_slots).then_some(*cgwid))
        .collect::<Vec<_>>();
    let key_main_ids = candidates
        .iter()
        .filter_map(|(cgwid, _, only_from_slots)| (*only_from_slots).then_some(*cgwid))
        .collect::<Vec<_>>();

    let mut fullscreen_candidates = Vec::new();
    let mut fullscreen_elements = Vec::new();
    let mut selected_source = raise_match_source(
        job.cgwid,
        &ax_window_ids,
        &key_main_ids,
        &fullscreen_candidates,
    );
    let mut selected_element = selected_source.and_then(|source| {
        candidates
            .iter()
            .find(|(cgwid, _, only_from_slots)| {
                *cgwid == job.cgwid
                    && match source {
                        RaiseMatchSource::AxWindows => !only_from_slots,
                        RaiseMatchSource::KeyMainSlot => *only_from_slots,
                        RaiseMatchSource::FullscreenSubrole => false,
                    }
            })
            .map(|(_, element, _)| *element)
    });

    // If neither app list nor key/main names the target, inspect app children for fullscreen
    // window candidates before reporting that no AX element can be matched.
    let mut children_array: *const c_void = std::ptr::null();
    let mut children_key = std::ptr::null();
    if selected_element.is_none() {
        children_key = cf_string_new("AXChildren");
        if !children_key.is_null()
            && AXUIElementCopyAttributeValue(app, children_key, &mut children_array) == K_AX_SUCCESS
            && !children_array.is_null()
            && CFGetTypeID(children_array) == CFArrayGetTypeID()
        {
            let subrole_key = cf_string_new("AXSubrole");
            if !subrole_key.is_null() {
                for index in 0..CFArrayGetCount(children_array) {
                    let element = CFArrayGetValueAtIndex(children_array, index);
                    if element.is_null() || CFGetTypeID(element) != AXUIElementGetTypeID() {
                        continue;
                    }
                    AXUIElementSetMessagingTimeout(element, AX_RAISE_MESSAGING_TIMEOUT);
                    let subrole = read_ax_subrole(element, subrole_key);
                    if ax_fullscreen_from_attributes(subrole.as_deref(), None) != Some(true) {
                        continue;
                    }
                    if let Some(cgwid) = ax_window_cgwid(element) {
                        fullscreen_candidates.push(cgwid);
                        fullscreen_elements.push((cgwid, element));
                    }
                }
                CFRelease(subrole_key);
            }
            selected_source = raise_match_source(
                job.cgwid,
                &ax_window_ids,
                &key_main_ids,
                &fullscreen_candidates,
            );
            if selected_source == Some(RaiseMatchSource::FullscreenSubrole) {
                selected_element = fullscreen_elements
                    .iter()
                    .find_map(|(cgwid, element)| (*cgwid == job.cgwid).then_some(*element));
            }
        }
    }
    if !children_key.is_null() {
        CFRelease(children_key);
    }

    let mut superseded = false;
    if let (Some(source), Some(element)) = (selected_source, selected_element) {
        // Final supersede gate right before applying: if a newer switch arrived during
        // enumeration, drop this job entirely (stale match) and let the new one run.
        if !raise_intent_current(job.generation) {
            superseded = true;
            log_debug!(
                "[raise] ax superseded before apply: pid={} cgwid={} gen={}",
                job.pid,
                job.cgwid,
                job.generation
            );
        } else {
            let matched_subrole_key = cf_string_new("AXSubrole");
            let matched_is_fullscreen = if matched_subrole_key.is_null() {
                false
            } else {
                AXUIElementSetMessagingTimeout(element, AX_RAISE_MESSAGING_TIMEOUT);
                let subrole = read_ax_subrole(element, matched_subrole_key);
                CFRelease(matched_subrole_key);
                ax_fullscreen_from_attributes(subrole.as_deref(), None) == Some(true)
            };
            match source {
                RaiseMatchSource::AxWindows => {}
                RaiseMatchSource::KeyMainSlot => log_debug!(
                    "[raise] ax matched via key/main slot: pid={} cgwid={}",
                    job.pid,
                    job.cgwid
                ),
                RaiseMatchSource::FullscreenSubrole => log_debug!(
                    "[raise] ax matched via fullscreen subrole: pid={} cgwid={}",
                    job.pid,
                    job.cgwid
                ),
            }
            cache_ax_window_element(job.pid, process_start_time_us, job.cgwid, element);
            let (raise_first_err, focused_set_err, raise_retry_err) = raise_ax_element(
                job.pid,
                job.cgwid,
                app,
                element,
                focused_key,
                raise_key,
                minimized_key,
                force_ax_focus,
                job.generation,
            );
            log_debug!(
                "[raise] ax raised refreshed: pid={} cgwid={} source={:?} fullscreen_subrole={} ax_windows={} known_minimized={} raise_first={} set_focused={:?} raise_retry={:?} app_create_us={} waited={}ms total={}ms",
                job.pid,
                job.cgwid,
                source,
                matched_is_fullscreen,
                ax_window_count,
                job.minimized,
                raise_first_err,
                focused_set_err,
                raise_retry_err,
                app_create_us,
                job.enqueued_at.elapsed().as_millis(),
                started.elapsed().as_millis()
            );
        }
    }
    if selected_element.is_none() && !superseded {
        log_info!(
            "[raise] ax NO MATCH: pid={} cgwid={} ax_windows={} ax_query_err={} waited={}ms total={}ms",
            job.pid,
            job.cgwid,
            ax_window_count,
            ax_query_err,
            job.enqueued_at.elapsed().as_millis(),
            started.elapsed().as_millis()
        );
    }
    if !lookup_slots.is_null() {
        CFRelease(lookup_slots);
    }
    // The matched AXUIElement may be borrowed from this array; keep it alive until the cache
    // and queued main-thread raise have taken their own retains.
    if !children_array.is_null() {
        CFRelease(children_array);
    }
    CFRelease(raise_key);
    CFRelease(focused_key);
    if let Some(minimized_key) = minimized_key {
        CFRelease(minimized_key);
    }
    CFRelease(app);
    // Matched means the exact window element was found and the raise applied (or queued for the
    // main thread). A superseded job reports no match even when it found one.
    selected_element.is_some() && !superseded
}

/// The normal path performs one AXRaise. Only an explicit non-stale failure sets
/// AXFocusedWindow and retries; a successful raise pays no extra AX IPC.
#[allow(clippy::too_many_arguments)]
unsafe fn raise_ax_element(
    pid: i32,
    cgwid: u32,
    app: AXUIElementRef,
    element: AXUIElementRef,
    focused_key: AXUIElementRef,
    raise_key: AXUIElementRef,
    minimized_key: Option<AXUIElementRef>,
    force_focus: bool,
    generation: u64,
) -> (AXError, Option<AXError>, Option<AXError>) {
    log_debug!(
        "[raise] AXRaise queued for main thread: pid={} cgwid={} force_focus={}",
        pid,
        cgwid,
        force_focus
    );
    // AXRaise targets the window element, which does not inherit the app element's timeout
    // (see the AX_WINDOW_MESSAGING_TIMEOUT note). Every AX call below runs on the main thread, so
    // a missing timeout freezes the UI for the system default (~1.5s).
    AXUIElementSetMessagingTimeout(element, AX_RAISE_MESSAGING_TIMEOUT);
    AXUIElementSetMessagingTimeout(app, AX_RAISE_MESSAGING_TIMEOUT);
    enqueue_main_thread_ax_raise(
        pid,
        cgwid,
        app,
        element,
        focused_key,
        raise_key,
        minimized_key,
        force_focus,
        generation,
    );
    (K_AX_SUCCESS, None, None)
}

/// Query an app's AX standard-window list.
/// None = AX query failed (app has no AX data; CG fallback allowed);
/// Some(vec) = query succeeded, possibly empty after subrole filtering (no standard windows =
/// Mission Control won't show it; skip entirely).
/// AX-window subrole keep-rule: standard windows always pass; AXDialog (the subrole
/// JetBrains IDEs use for their MAIN windows) only when titled; some apps (such as Xcode)
/// report ordinary windows as AXUnknown, so allow that only with AXWindow role and a non-empty
/// title. AXFloatingWindow (what Telegram reports for its windowed media viewer) passes the same
/// way, because the subrole follows the presentation state; `floating_window_is_admissible`
/// decides at pairing time whether WindowServer sees an ordinary window. Everything else
/// (popups/panels/invisible windows) is filtered; a missing subrole counts as standard.
/// Pure function, unit-tested.
pub(super) fn ax_subrole_kept(subrole: Option<&str>, role: Option<&str>, titled: bool) -> bool {
    match subrole {
        Some("AXStandardWindow") => true,
        Some("AXFullScreen") => true,
        Some("AXDialog") => titled,
        Some("AXFloatingWindow") => role == Some("AXWindow") && titled,
        Some("AXUnknown") => role == Some("AXWindow") && titled,
        Some(_) => false,
        // A missing subrole counts as standard (some apps don't set it).
        None => true,
    }
}

/// Either AX signal may be affirmative; retain unknown only when both signals are unavailable.
pub(super) fn ax_fullscreen_from_attributes(
    subrole: Option<&str>,
    fullscreen_attribute: Option<bool>,
) -> Option<bool> {
    if subrole == Some("AXFullScreen") || fullscreen_attribute == Some(true) {
        Some(true)
    } else if subrole.is_some() || fullscreen_attribute.is_some() {
        Some(false)
    } else {
        None
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RaiseMatchSource {
    AxWindows,
    KeyMainSlot,
    FullscreenSubrole,
}

fn raise_match_source(
    target_cgwid: u32,
    ax_windows: &[u32],
    key_main_slots: &[u32],
    fullscreen_candidates: &[u32],
) -> Option<RaiseMatchSource> {
    if ax_windows.contains(&target_cgwid) {
        Some(RaiseMatchSource::AxWindows)
    } else if key_main_slots.contains(&target_cgwid) {
        Some(RaiseMatchSource::KeyMainSlot)
    } else if fullscreen_candidates.contains(&target_cgwid) {
        Some(RaiseMatchSource::FullscreenSubrole)
    } else {
        None
    }
}

unsafe fn read_ax_subrole(element: AXUIElementRef, subrole_key: *const c_void) -> Option<String> {
    let mut value: *const c_void = std::ptr::null();
    if AXUIElementCopyAttributeValue(element, subrole_key, &mut value) == K_AX_SUCCESS
        && !value.is_null()
    {
        let subrole = cf_to_rust_string(value);
        CFRelease(value);
        subrole
    } else {
        if !value.is_null() {
            CFRelease(value);
        }
        None
    }
}

unsafe fn ax_boolean_attribute(element: AXUIElementRef, key: *const c_void) -> Option<bool> {
    let mut value: *const c_void = std::ptr::null();
    let result = AXUIElementCopyAttributeValue(element, key, &mut value);
    if value.is_null() {
        return None;
    }
    let boolean = (result == K_AX_SUCCESS).then(|| CFBooleanGetValue(value));
    CFRelease(value);
    boolean
}

// The substantial custom-window boundary. Standard windows and titled dialogs do not use this
// size gate; it only prevents an untitled/unknown custom root from becoming a switch
// destination when it is merely a tiny auxiliary surface.
const CUSTOM_WINDOW_MIN_WIDTH: f64 = 100.0;
const CUSTOM_WINDOW_MIN_HEIGHT: f64 = 50.0;

pub(super) fn admissible_window_placement(layer: i32, is_main: bool, is_fullscreen: bool) -> bool {
    layer == 0 || is_main || is_fullscreen
}

pub(super) fn custom_window_is_substantial(bounds: (f64, f64, f64, f64)) -> bool {
    bounds.2 >= CUSTOM_WINDOW_MIN_WIDTH && bounds.3 >= CUSTOM_WINDOW_MIN_HEIGHT
}

/// The WindowServer shape a titled AXFloatingWindow needs to be a switch destination.
/// Mission Control lists such a window (Telegram's windowed media viewer is 880x660 at layer 0),
/// while the floating surfaces that must stay out -- inspectors, palettes, popovers -- are either
/// attached to an owner (dropped earlier by `is_attached_surface`) or sit above layer 0.
/// Pure function, unit-tested.
pub(super) fn floating_window_is_admissible(layer: i32, bounds: (f64, f64, f64, f64)) -> bool {
    layer == 0 && custom_window_is_substantial(bounds)
}

/// Every admission rule the CG pairing stage applies to one window, in the order it applies them,
/// so the call sites cannot drift from what the tests pin.
/// Pure function, unit-tested.
pub(super) fn window_admission(
    layer: i32,
    is_main: bool,
    ax_fullscreen: Option<bool>,
    is_floating_window: bool,
    bounds: (f64, f64, f64, f64),
) -> bool {
    admissible_window_placement(
        layer,
        is_main,
        crate::window_collector::collect::ax_reports_fullscreen(ax_fullscreen),
    ) && (!is_floating_window || floating_window_is_admissible(layer, bounds))
}

/// The conservative validity rule for a candidate that has no AX element at all: a window on
/// another desktop, which `kAXWindows` cannot publish because it is filtered by the current Space.
///
/// AX cannot vouch for such a window, so the shape a custom/restored window needs to be switchable
/// is required unconditionally: an ordinary layer-0 window of ordinary size. The titled-floating and
/// custom-root exemptions deliberately do not apply here -- they are AX judgments about a window the
/// app told us about, and without that judgment a small layer-0 helper surface must not become a
/// card. Pure function, unit-tested.
pub(super) fn cg_only_window_admissible(layer: i32, bounds: (f64, f64, f64, f64)) -> bool {
    layer == 0 && custom_window_is_substantial(bounds)
}

pub(super) fn is_attached_surface(parent_id: Option<u32>) -> bool {
    parent_id.is_some_and(|parent| parent != 0)
}

/// Decide whether an AX-only window may be backfilled into the switcher.
/// A missing CG entry means an orderOut'd window, which AX can legitimately recover; a known
/// non-zero CG layer means an app-owned overlay/menu and must stay out of the window switcher.
pub(super) fn should_backfill_ax_window(cg_layer: Option<i32>) -> bool {
    match cg_layer {
        Some(layer) => layer == 0,
        None => true,
    }
}

/// The AXFloatingWindow half of the backfill rule. The subrole cannot decide on its own (see
/// `floating_window_is_admissible`) and the backfill publishes windows with zero bounds, so a
/// floating window is restored only when this pass's CG snapshot already described it as an
/// ordinary window: without that, the backfill would undo the size gate the pairing stage just
/// applied -- a titled 64x33 floating window at layer 0 came back as a card that way.
/// Pure function, unit-tested.
pub(super) fn floating_window_backfill_admissible(
    cg_layer: Option<i32>,
    cg_bounds: Option<(f64, f64, f64, f64)>,
) -> bool {
    cg_layer == Some(0) && cg_bounds.is_some_and(custom_window_is_substantial)
}

/// Every rule the AX-only backfill applies to one window, in the order it applies them, so the call
/// sites cannot drift from what the tests pin. The title and Space checks stay at the call site
/// because they need state this function does not take.
/// Pure function, unit-tested.
pub(super) fn backfill_admission(
    cg_layer: Option<i32>,
    cg_bounds: Option<(f64, f64, f64, f64)>,
    sticky: bool,
    is_custom_root: bool,
    is_main: bool,
    is_floating_window: bool,
) -> bool {
    !sticky
        && should_backfill_ax_window(cg_layer)
        && (!is_custom_root || is_main)
        && (!is_floating_window || floating_window_backfill_admissible(cg_layer, cg_bounds))
}

pub(super) fn should_backfill_ax_window_for_process(
    pid: i32,
    cgwid: u32,
    process_start_time_us: Option<u64>,
    cg_layer: Option<i32>,
) -> bool {
    should_backfill_ax_window(cg_layer)
        && !is_known_non_normal_window(pid, process_start_time_us, cgwid)
}

pub(super) fn remember_non_normal_cg_windows(
    cg_window_layers: &HashMap<(i32, u32), i32>,
    identities: &HashMap<i32, AppIdentity>,
    ax_windows: &HashMap<i32, HashMap<u32, AxWindowInfo>>,
    native_fullscreen_windows: &HashSet<(i32, u32)>,
    parent_ids: &HashMap<u32, u32>,
) {
    for (&(pid, cgwid), &layer) in cg_window_layers {
        if is_attached_surface(parent_ids.get(&cgwid).copied()) {
            continue;
        }
        let is_main_or_fullscreen = ax_windows
            .get(&pid)
            .and_then(|windows| windows.get(&cgwid))
            .is_some_and(|window| window.is_main || window.is_fullscreen == Some(true))
            || native_fullscreen_windows.contains(&(pid, cgwid));
        if layer != 0 && !is_main_or_fullscreen {
            let process_start_time_us = identities
                .get(&pid)
                .and_then(|identity| identity.process_start_time_us);
            remember_non_normal_window(pid, process_start_time_us, cgwid);
        }
    }
}

pub(super) fn remember_non_normal_cg_windows_for_process(
    pid: i32,
    process_start_time_us: Option<u64>,
    cg_window_layers: &HashMap<u32, i32>,
    ax_windows: &HashMap<u32, AxWindowInfo>,
    native_fullscreen_windows: &HashSet<u32>,
    parent_ids: &HashMap<u32, u32>,
) {
    for (&cgwid, &layer) in cg_window_layers {
        if is_attached_surface(parent_ids.get(&cgwid).copied()) {
            continue;
        }
        let is_main_or_fullscreen = ax_windows
            .get(&cgwid)
            .is_some_and(|window| window.is_main || window.is_fullscreen == Some(true))
            || native_fullscreen_windows.contains(&cgwid);
        if layer != 0 && !is_main_or_fullscreen {
            remember_non_normal_window(pid, process_start_time_us, cgwid);
        }
    }
}

/// All standard AX windows for a PID: (cgwid, title, minimized). Shared between
/// collect_windows and the thumbnail module's startup pre-generation.
pub(crate) fn get_ax_windows_for_pid(pid: i32) -> Option<Vec<(u32, String, bool)>> {
    let process_start_time_us = unsafe { resolve_app_identity(pid).process_start_time_us };
    get_ax_windows_for_pid_with_identity(pid, process_start_time_us).map(|windows| {
        windows
            .into_iter()
            .map(|window| (window.cgwid, window.title, window.minimized))
            .collect()
    })
}

/// kAXValueAXErrorType: the placeholder type a batch read uses for an attribute the app did
/// not answer.
const K_AX_VALUE_AX_ERROR_TYPE: i32 = 5;

/// What a batch-read slot said. "The app answered that it has no value" and "the read failed" are
/// different facts: the first means the app has no such window, the second means nothing is known
/// about it at all -- and only the second may keep the CG fallback (see `cg_only_pairing`).
pub(super) enum AxSlotOutcome {
    Value(AXUIElementRef),
    /// The app answered, but not with a value: `kAXErrorNoValue` / an unsupported attribute.
    NoValue,
    /// The read itself failed (communication, a dead element, or an unrecognised code).
    Failed,
}

pub(super) const K_AX_ERROR_FAILURE: i32 = -25200;
pub(super) const K_AX_ERROR_ILLEGAL_ARGUMENT: i32 = -25201;
pub(super) const K_AX_ERROR_INVALID_UI_ELEMENT: i32 = -25202;
pub(super) const K_AX_ERROR_CANNOT_COMPLETE: i32 = -25204;
pub(super) const K_AX_ERROR_ATTRIBUTE_UNSUPPORTED: i32 = -25205;
pub(super) const K_AX_ERROR_ACTION_UNSUPPORTED: i32 = -25206;
pub(super) const K_AX_ERROR_NOTIFICATION_UNSUPPORTED: i32 = -25207;
pub(super) const K_AX_ERROR_NOT_IMPLEMENTED: i32 = -25208;
pub(super) const K_AX_ERROR_NO_VALUE: i32 = -25212;
pub(super) const K_AX_ERROR_PARAMETERIZED_ATTRIBUTE_UNSUPPORTED: i32 = -25213;

/// Whether an attribute-level AXError means "the app answered, and has no value for this attribute"
/// as opposed to a failed read. A batch read without stopOnError reports both the same way -- as an
/// error placeholder in the slot -- so the code inside the placeholder is the only thing that
/// separates them. Pure function, unit-tested.
pub(super) fn ax_error_means_no_answer(code: i32) -> bool {
    match code {
        K_AX_ERROR_NO_VALUE
        | K_AX_ERROR_ATTRIBUTE_UNSUPPORTED
        | K_AX_ERROR_ILLEGAL_ARGUMENT
        | K_AX_ERROR_NOT_IMPLEMENTED
        | K_AX_ERROR_PARAMETERIZED_ATTRIBUTE_UNSUPPORTED
        | K_AX_ERROR_ACTION_UNSUPPORTED
        | K_AX_ERROR_NOTIFICATION_UNSUPPORTED => true,
        // A read that could not complete (a dead element, a communication failure) is a failed
        // read, never "the app has no value" -- and so is any code this build does not know.
        K_AX_ERROR_CANNOT_COMPLETE | K_AX_ERROR_INVALID_UI_ELEMENT | K_AX_ERROR_FAILURE => false,
        _ => false,
    }
}

/// Whether a per-app AX read learned nothing usable *and* failed to complete: no window element came
/// back and at least one of the three attribute reads failed.
///
/// This is not the same as an empty answer. Stats answers `AXWindows` with an empty array and
/// reports `NoValue` for both key/main slots -- it answered, and it has no window (see
/// `cg_only_pairing`). But when a read *failed* while nothing came back, the answer is incomplete:
/// the window list is Space-filtered, so a real window of another desktop may have been reachable
/// only through the key/main slots that just failed, and treating that as "no windows" would drop it.
/// Pure function, unit-tested.
pub(super) fn ax_read_is_incomplete(
    window_elements: usize,
    has_focused: bool,
    has_main: bool,
    any_slot_failed: bool,
) -> bool {
    window_elements == 0 && !has_focused && !has_main && any_slot_failed
}

/// Read one slot of a batch-read result, keeping the distinction the caller needs.
pub(super) unsafe fn ax_slot_outcome(slots: *const c_void, index: isize) -> AxSlotOutcome {
    if slots.is_null() || index >= CFArrayGetCount(slots) {
        return AxSlotOutcome::Failed;
    }
    let value = CFArrayGetValueAtIndex(slots, index);
    if value.is_null() {
        return AxSlotOutcome::Failed;
    }
    // A batch read without stopOnError puts an kAXValueAXErrorType AXValue in a slot the app could
    // not answer; not recognising it would read "did not answer" as "answered an object".
    if CFGetTypeID(value) == AXValueGetTypeID() && AXValueGetType(value) == K_AX_VALUE_AX_ERROR_TYPE
    {
        let mut code: i32 = K_AX_ERROR_FAILURE;
        let read = AXValueGetValue(
            value,
            K_AX_VALUE_AX_ERROR_TYPE,
            (&mut code as *mut i32).cast(),
        );
        if read && ax_error_means_no_answer(code) {
            return AxSlotOutcome::NoValue;
        }
        return AxSlotOutcome::Failed;
    }
    AxSlotOutcome::Value(value)
}

/// Read one slot of a batch-read result: an out-of-range index, null or an error placeholder all
/// mean "did not answer" (None).
unsafe fn ax_slot_value(slots: *const c_void, index: isize) -> Option<AXUIElementRef> {
    match ax_slot_outcome(slots, index) {
        AxSlotOutcome::Value(value) => Some(value),
        AxSlotOutcome::NoValue | AxSlotOutcome::Failed => None,
    }
}

/// Merges the window elements of the three attribute slots: `kAXWindows` order first, then the
/// focused and main slots; a window (identified by its CGWindowID) keeps its first occurrence,
/// while elements without an id are deduplicated by element. The three must be read together, and
/// **an empty array is not "no windows"**: AppKit's `kAXWindows` is filtered by the CURRENT
/// Space (it answers with an empty array when every window lives on another Space), whereas
/// `kAXFocusedWindow`/`kAXMainWindow` read NSApplication's `_keyWindow`/`_mainWindow` weak refs
/// with no Space filter and no active-state guard, so such an app still hands its key/main window
/// back. Returning early on an empty `windows` here would throw away exactly the evidence that
/// recovers those windows.
pub(super) fn candidate_window_elements<T>(
    published: &[(u32, T)],
    focused: Option<(u32, T)>,
    main: Option<(u32, T)>,
) -> Vec<(u32, T, bool)>
where
    T: Copy + Eq + std::hash::Hash,
{
    let mut result = Vec::with_capacity(published.len() + 2);
    let mut seen_wids = HashSet::new();
    let mut seen_elements = HashSet::new();
    // The last component marks "only from the key/main slots" (see
    // AxWindowInfo::only_via_key_or_main).
    let candidates = published
        .iter()
        .copied()
        .map(|entry| (entry, false))
        .chain(focused.map(|entry| (entry, true)))
        .chain(main.map(|entry| (entry, true)));
    for ((wid, element), only_via_key_or_main) in candidates {
        // The slots repeat the same window as different objects; deduplicating by id saves the
        // duplicate per-element attribute reads.
        let duplicate = if wid != 0 {
            !seen_wids.insert(wid)
        } else {
            !seen_elements.insert(element)
        };
        if duplicate {
            continue;
        }
        result.push((wid, element, only_via_key_or_main));
    }
    result
}

/// Reads the native tab bar (an AXTabGroup among the window's direct children) and returns its
/// tab titles. Only the selected tab's window exposes one, so a background tab reads as None --
/// which is how callers identify a tab group. None when the structure is unrecognised, there is
/// no tab bar, or fewer than two tabs (fail-open).
unsafe fn read_tab_group_info(element: AXUIElementRef) -> Option<TabGroupInfo> {
    let children_key = cf_string_new("AXChildren");
    let role_key = cf_string_new("AXRole");
    let title_key = cf_string_new("AXTitle");
    let mut info = None;

    let mut children_value: *const c_void = std::ptr::null();
    if AXUIElementCopyAttributeValue(element, children_key, &mut children_value) == K_AX_SUCCESS
        && !children_value.is_null()
        && CFGetTypeID(children_value) == CFArrayGetTypeID()
    {
        let count = CFArrayGetCount(children_value);
        // Some apps hang the tab buttons directly off the window with no AXTabGroup wrapper, so
        // both shapes are collected while walking the direct children.
        let mut group_titles: Option<Vec<String>> = None;
        let mut bare_titles: Vec<String> = Vec::new();
        for index in 0..count {
            let child = CFArrayGetValueAtIndex(children_value, index);
            if child.is_null() {
                continue;
            }
            let mut role_value: *const c_void = std::ptr::null();
            let role = if AXUIElementCopyAttributeValue(child, role_key, &mut role_value)
                == K_AX_SUCCESS
                && !role_value.is_null()
            {
                let role = cf_to_rust_string(role_value);
                CFRelease(role_value);
                role
            } else {
                None
            };
            match role.as_deref() {
                Some("AXTabGroup") => {
                    if group_titles.is_none() {
                        group_titles = read_tab_titles(child, children_key, title_key);
                    }
                }
                // Barely-attached tab buttons count only when they really form a group, so a
                // lone radio button is never mistaken for a tab bar.
                Some("AXRadioButton") => {
                    if let Some(title) = read_ax_title(child, title_key) {
                        bare_titles.push(title);
                    }
                }
                _ => {}
            }
        }
        info = group_titles
            .or_else(|| (bare_titles.len() >= 2).then_some(bare_titles))
            .map(|titles| TabGroupInfo { titles });
    }
    if !children_value.is_null() {
        CFRelease(children_value);
    }
    CFRelease(children_key);
    CFRelease(role_key);
    CFRelease(title_key);
    info
}

/// Reads the titles of an AXTabGroup's direct children (the tab buttons); None under two.
unsafe fn read_tab_titles(
    group: AXUIElementRef,
    children_key: *const c_void,
    title_key: *const c_void,
) -> Option<Vec<String>> {
    let mut tabs_value: *const c_void = std::ptr::null();
    if AXUIElementCopyAttributeValue(group, children_key, &mut tabs_value) != K_AX_SUCCESS
        || tabs_value.is_null()
        || CFGetTypeID(tabs_value) != CFArrayGetTypeID()
    {
        if !tabs_value.is_null() {
            CFRelease(tabs_value);
        }
        return None;
    }
    let count = CFArrayGetCount(tabs_value);
    let mut titles = Vec::new();
    for index in 0..count {
        let tab = CFArrayGetValueAtIndex(tabs_value, index);
        if tab.is_null() {
            continue;
        }
        if let Some(title) = read_ax_title(tab, title_key) {
            titles.push(title);
        }
    }
    CFRelease(tabs_value);
    (titles.len() >= 2).then_some(titles)
}

/// Reads an element's AXTitle (unreadable or empty -> None).
unsafe fn read_ax_title(element: AXUIElementRef, title_key: *const c_void) -> Option<String> {
    let mut value: *const c_void = std::ptr::null();
    if AXUIElementCopyAttributeValue(element, title_key, &mut value) == K_AX_SUCCESS
        && !value.is_null()
    {
        let title = cf_to_rust_string(value);
        CFRelease(value);
        title.filter(|title| !title.is_empty())
    } else {
        None
    }
}

pub(super) fn get_ax_windows_for_pid_with_identity(
    pid: i32,
    process_start_time_us: Option<u64>,
) -> Option<Vec<AxWindowInfo>> {
    if let Some(windows) = cached_ax_snapshot(pid, process_start_time_us) {
        return Some(windows);
    }

    unsafe {
        let app = AXUIElementCreateApplication(pid);
        if app.is_null() {
            return None;
        }

        // Set a 50ms messaging timeout: AX queries on slow/unresponsive apps fail fast instead
        // of hitting the default 10s timeout (which would push the app to the CG fallback path
        // and let invisible windows through).
        AXUIElementSetMessagingTimeout(app, 0.05);

        // One batched read of the three attributes (rationale and merge rules in
        // candidate_window_elements).
        let windows_key = cf_string_new("AXWindows");
        let focused_window_key = cf_string_new("AXFocusedWindow");
        let main_window_key = cf_string_new("AXMainWindow");
        let keys = [windows_key, focused_window_key, main_window_key];
        // callbacks = null: the array is only a synchronous input to this call; `keys` keeps the
        // strings alive for its duration.
        let keys_array = CFArrayCreate(
            std::ptr::null(),
            keys.as_ptr(),
            keys.len() as isize,
            std::ptr::null(),
        );
        if keys_array.is_null() {
            for key in keys {
                CFRelease(key);
            }
            CFRelease(app);
            return None;
        }
        let mut slots: *const c_void = std::ptr::null();
        let err = AXUIElementCopyMultipleAttributeValues(app, keys_array, 0, &mut slots);
        CFRelease(keys_array);
        for key in keys {
            CFRelease(key);
        }
        CFRelease(app);
        if err != K_AX_SUCCESS || slots.is_null() {
            return None;
        }
        // Caller-owned: the guard releases it on every exit path below, including the early
        // "nothing learned about this app" return.
        let _slots = super::space_membership::OwnedCf(slots);

        // Slot 0 = kAXWindows (a CFArray), 1 = kAXFocusedWindow, 2 = kAXMainWindow. The outcomes
        // keep "the app answered with no value" apart from "the read failed": only the latter is a
        // failed query, and only a failed query keeps the CG fallback.
        let windows_outcome = ax_slot_outcome(slots, 0);
        let focused_outcome = ax_slot_outcome(slots, 1);
        let main_outcome = ax_slot_outcome(slots, 2);
        let windows_array = match windows_outcome {
            AxSlotOutcome::Value(value) if CFGetTypeID(value) == CFArrayGetTypeID() => Some(value),
            _ => None,
        };
        let focused_window = match focused_outcome {
            AxSlotOutcome::Value(value) if CFGetTypeID(value) == AXUIElementGetTypeID() => {
                Some(value)
            }
            _ => None,
        };
        let main_window = match main_outcome {
            AxSlotOutcome::Value(value) if CFGetTypeID(value) == AXUIElementGetTypeID() => {
                Some(value)
            }
            _ => None,
        };
        // Nothing usable came back and at least one of the three reads failed: the query is
        // incomplete, so this must not be cached as "the app answered that it has no windows".
        let any_slot_failed = matches!(windows_outcome, AxSlotOutcome::Failed)
            || matches!(focused_outcome, AxSlotOutcome::Failed)
            || matches!(main_outcome, AxSlotOutcome::Failed);
        let window_elements =
            windows_array.map_or(0, |array| CFArrayGetCount(array).max(0) as usize);
        if ax_read_is_incomplete(
            window_elements,
            focused_window.is_some(),
            main_window.is_some(),
            any_slot_failed,
        ) {
            return None;
        }

        let mut published: Vec<(u32, AXUIElementRef)> = Vec::new();
        if let Some(array) = windows_array {
            let count = CFArrayGetCount(array);
            published.reserve(count as usize);
            for i in 0..count {
                let element = CFArrayGetValueAtIndex(array, i);
                if !element.is_null() {
                    published.push((ax_window_cgwid(element).unwrap_or(0), element));
                }
            }
        }
        let candidates = candidate_window_elements(
            &published,
            focused_window.map(|element| (ax_window_cgwid(element).unwrap_or(0), element)),
            main_window.map(|element| (ax_window_cgwid(element).unwrap_or(0), element)),
        );

        let candidate_count = candidates.len();
        let title_key = cf_string_new("AXTitle");
        let role_key = cf_string_new("AXRole");
        let subrole_key = cf_string_new("AXSubrole");
        let minimized_key = cf_string_new("AXMinimized");
        let fullscreen_key = cf_string_new("AXFullScreen");
        let main_key = cf_string_new("AXMain");
        let mut results = Vec::with_capacity(candidates.len());

        for (cgwid, element, only_via_key_or_main) in candidates {
            // The 50ms on the app element does not carry over to the window elements; set it per
            // element, or an unresponsive app's window queries fall back to the system default
            // (~1.5s) and stall the whole collection pass.
            AXUIElementSetMessagingTimeout(element, AX_WINDOW_MESSAGING_TIMEOUT);

            // Keep standard windows, titled dialogs, and titled AXUnknown elements whose
            // role is AXWindow (ordinary windows in apps such as Xcode); filter popups,
            // panels, and other non-standard elements.
            let mut subrole_value: *const c_void = std::ptr::null();
            let subrole = if AXUIElementCopyAttributeValue(element, subrole_key, &mut subrole_value)
                == K_AX_SUCCESS
                && !subrole_value.is_null()
            {
                let s = cf_to_rust_string(subrole_value);
                CFRelease(subrole_value);
                s
            } else {
                None
            };
            let kept = if subrole.is_some() {
                let mut role_value: *const c_void = std::ptr::null();
                let role = if AXUIElementCopyAttributeValue(element, role_key, &mut role_value)
                    == K_AX_SUCCESS
                    && !role_value.is_null()
                {
                    let role = cf_to_rust_string(role_value);
                    CFRelease(role_value);
                    role
                } else {
                    None
                };
                // AXDialog/AXUnknown/AXFloatingWindow require a non-empty title; untitled elements
                // stay filtered as popups/invisible windows.
                let titled = if matches!(
                    subrole.as_deref(),
                    Some("AXDialog") | Some("AXUnknown") | Some("AXFloatingWindow")
                ) {
                    let mut title_value: *const c_void = std::ptr::null();
                    if AXUIElementCopyAttributeValue(element, title_key, &mut title_value)
                        == K_AX_SUCCESS
                        && !title_value.is_null()
                    {
                        let t = cf_to_rust_string(title_value);
                        CFRelease(title_value);
                        t.is_some_and(|t| !t.is_empty())
                    } else {
                        false
                    }
                } else {
                    false
                };
                ax_subrole_kept(subrole.as_deref(), role.as_deref(), titled)
            } else {
                // No subrole means standard window for apps that don't set it.
                true
            };
            if !kept {
                continue;
            }

            let mut title_value: *const c_void = std::ptr::null();
            let title = if AXUIElementCopyAttributeValue(element, title_key, &mut title_value)
                == K_AX_SUCCESS
                && !title_value.is_null()
            {
                let t = cf_to_rust_string(title_value);
                CFRelease(title_value);
                t.unwrap_or_default()
            } else {
                String::new()
            };
            // AXMinimized: whether the window is minimized. Absent attribute (some apps) -> false.
            let minimized = {
                let mut min_value: *const c_void = std::ptr::null();
                if AXUIElementCopyAttributeValue(element, minimized_key, &mut min_value)
                    == K_AX_SUCCESS
                    && !min_value.is_null()
                {
                    let m = CFBooleanGetValue(min_value);
                    CFRelease(min_value);
                    m
                } else {
                    false
                }
            };
            // The native tab bar is read only for a minimized window of a multi-window app:
            // background tabs only reach the AX list while minimized (the visible/hidden states
            // hand over the selected tab alone), which is the only state that needs folding;
            // reading it otherwise is pure cost. A failed read means "no tab group" (fail-open).
            let tab_group = if candidate_count >= 2 && minimized {
                read_tab_group_info(element)
            } else {
                None
            };
            let is_main = {
                let mut main_value: *const c_void = std::ptr::null();
                if AXUIElementCopyAttributeValue(element, main_key, &mut main_value) == K_AX_SUCCESS
                    && !main_value.is_null()
                {
                    let value = CFBooleanGetValue(main_value);
                    CFRelease(main_value);
                    value
                } else {
                    false
                }
            };
            let fullscreen_attribute = ax_boolean_attribute(element, fullscreen_key);
            let is_fullscreen =
                ax_fullscreen_from_attributes(subrole.as_deref(), fullscreen_attribute);
            // cgwid was resolved while merging the candidates (private API, used to pair with
            // the CG window).
            // Retain the exact element for the activation path so normal raises do not need
            // another AXWindows round trip.
            cache_ax_window_element(pid, process_start_time_us, cgwid, element);
            results.push(AxWindowInfo {
                cgwid,
                title,
                minimized,
                is_main,
                is_fullscreen,
                is_custom_root: subrole.as_deref() == Some("AXUnknown"),
                is_floating_window: subrole.as_deref() == Some("AXFloatingWindow"),
                only_via_key_or_main,
                tab_group,
            });
        }
        CFRelease(title_key);
        CFRelease(subrole_key);
        CFRelease(minimized_key);
        CFRelease(fullscreen_key);
        CFRelease(main_key);

        cache_ax_snapshot(pid, process_start_time_us, &results);
        Some(results)
    }
}

/// Whether the process behind `pid` cannot be activated
/// (NSApplicationActivationPolicyProhibited = 2). Such a process has no switchable windows by
/// definition (settings-pane hosts, cursor-overlay services).
pub(super) unsafe fn process_cannot_be_activated(pid: i32) -> bool {
    let app: *mut AnyObject =
        msg_send![class!(NSRunningApplication), runningApplicationWithProcessIdentifier: pid];
    if app.is_null() {
        return false;
    }
    let policy: i64 = msg_send![app, activationPolicy];
    policy == 2
}

/// CFString -> Rust String (None = conversion failed). Reused by window control.
pub(crate) fn cf_to_rust_string(cf_string: *const c_void) -> Option<String> {
    let mut buf = vec![0u8; 1024];
    let ok = unsafe {
        CFStringGetCString(
            cf_string,
            buf.as_mut_ptr() as *mut i8,
            buf.len() as isize,
            0x08000100,
        )
    };
    if ok {
        let end = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
        Some(String::from_utf8_lossy(&buf[..end]).to_string())
    } else {
        None
    }
}

#[cfg(test)]
mod raise_match_tests {
    use super::{raise_match_source, RaiseMatchSource};

    #[test]
    fn target_in_ax_windows_uses_the_normal_match() {
        assert_eq!(
            raise_match_source(6865, &[61, 6865], &[6865], &[6865]),
            Some(RaiseMatchSource::AxWindows)
        );
    }

    #[test]
    fn target_missing_from_ax_windows_uses_key_or_main_slot() {
        assert_eq!(
            raise_match_source(6865, &[61], &[6865], &[]),
            Some(RaiseMatchSource::KeyMainSlot)
        );
    }

    #[test]
    fn target_missing_from_lists_uses_fullscreen_subrole_candidate() {
        assert_eq!(
            raise_match_source(6865, &[61], &[], &[6865]),
            Some(RaiseMatchSource::FullscreenSubrole)
        );
    }

    #[test]
    fn target_missing_from_every_candidate_set_has_no_match() {
        assert_eq!(raise_match_source(6865, &[61], &[], &[62]), None);
    }
}
