//! The thumbnail module's AX pre-generation subsystem: a resident thread installs
//! an AXObserver per running app, watches kAXWindowCreatedNotification, and after a
//! 300ms debounce posts backfill jobs to the capture pipeline
//! (super::enqueue_job_for_generation). App launch/termination arrive through
//! app_launched/app_terminated.

use objc2::runtime::AnyObject;
use objc2::{class, msg_send};
use std::collections::HashMap;
use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{LazyLock, Mutex, OnceLock};
use std::time::Duration;

use crate::event_tap::{
    kCFRunLoopDefaultMode, CFRunLoopAddSource, CFRunLoopGetCurrent, CFRunLoopRun,
};
use crate::log_debug;

use super::{
    capture_allowed, enqueue_job_for_generation, CFStringCompare, CapturePriority, RetainedCf,
    BASE_TARGET_PX_H, CACHE, CAPTURE_STATE, STARTUP_PREWARM_MAX,
};
use crate::ffi::{
    make_nsstring, AXObserverAddNotification, AXObserverCreate, AXObserverGetRunLoopSource,
    AXUIElementCreateApplication, AXUIElementGetPid, AxObserverHandle, AxObserverRef, CFRelease,
    CFRunLoopRemoveSource, CFRunLoopSourceContext, CFRunLoopSourceCreate, CFRunLoopSourceSignal,
    CFRunLoopWakeUp, RunLoopHandle, RunLoopSourceHandle,
};

static STARTED: AtomicBool = AtomicBool::new(false);
static STOP_REQUESTED: AtomicBool = AtomicBool::new(false);

/// Cross-thread CFRunLoop handle: only WakeUp is allowed; source management stays on the observer thread.
struct RunLoopSlot(Mutex<Option<RunLoopHandle>>);
static OBSERVER_RL: RunLoopSlot = RunLoopSlot(Mutex::new(None));
/// The command-injection source (stashed once the observer thread creates it; any
/// thread signals it to wake command processing).
struct SourceSlot(Mutex<Option<RunLoopSourceHandle>>);
static CMD_SOURCE: SourceSlot = SourceSlot(Mutex::new(None));

/// Observer-thread commands: install/uninstall a PID's observer (forwarded across
/// threads from NSWorkspace notifications).
enum ObsCmd {
    Install(i32),
    Remove(i32),
}
static CMD_TX: OnceLock<flume::Sender<ObsCmd>> = OnceLock::new();
static CMD_RX: OnceLock<flume::Receiver<ObsCmd>> = OnceLock::new();

/// Installed observers: pid -> (AXObserverRef, runloop source); removed as a pair.
/// Raw pointers again -- needs the Send+Sync wrapper (inserts/removes happen on
/// the observer thread; lookups from any thread).
struct InstalledMap(Mutex<HashMap<i32, (AxObserverHandle, RunLoopSourceHandle)>>);
unsafe impl Send for InstalledMap {}
unsafe impl Sync for InstalledMap {}
static INSTALLED: LazyLock<InstalledMap> =
    LazyLock::new(|| InstalledMap(Mutex::new(HashMap::new())));

// The public-framework AX and CFRunLoop externs now live in ffi.rs (kAXWindowCreated-
// Notification is still resolved at runtime below; CFRunLoopGetCurrent/Run/AddSource are
// reused from event_tap).

/// An equivalent of kAXWindowCreatedNotification. The constant is no longer
/// exported as a dynamic symbol on current macOS (both extern linking and dlsym
/// fail -- verified empirically), but AX notification names compare by STRING
/// VALUE, so the literal "AXWindowCreated" is semantically identical for both
/// registration and callback matching.
static AX_WINDOW_CREATED: LazyLock<RetainedCf<c_void>> = LazyLock::new(|| unsafe {
    // make_nsstring +1 lives for the process lifetime (statically held); cast
    // to *const c_void for the CF APIs.
    let s = make_nsstring("AXWindowCreated");
    RetainedCf::from_retained(std::mem::transmute::<*mut AnyObject, *const c_void>(s))
});

/// Start the resident listener thread (idempotent). Duties: install AXObservers
/// for running apps -> pre-generate existing standard windows -> run the runloop
/// serving Install/Remove commands and AX events.
pub(crate) fn start() {
    if STARTED.swap(true, Ordering::SeqCst) {
        return;
    }
    // start() is called from the main-thread runtime configuration path. Capture the initial
    // window keys here, before spawning the observer thread, so the worker never reads TAB_STATE.
    let startup_jobs = crate::with_tab_state(|state_opt| match state_opt.as_ref() {
        Some(state) => {
            let capture_state = CAPTURE_STATE.lock().unwrap();
            state
                .windows
                .iter()
                .filter(|window| {
                    !window.minimized
                        && window.window_id != 0
                        && window.bounds.2 > 0.0
                        && window.bounds.3 > 0.0
                })
                .map(|window| {
                    (
                        window.pid,
                        window.window_id,
                        capture_state.pid_generation(window.pid),
                    )
                })
                .collect::<Vec<_>>()
        }
        None => Vec::new(),
    });
    // Lifecycle commands are low-volume but still bounded; the runloop source drains them
    // promptly, and this capacity absorbs startup bursts without unbounded retention.
    let (tx, rx) = flume::bounded::<ObsCmd>(64);
    let _ = CMD_TX.set(tx);
    let _ = CMD_RX.set(rx);
    STOP_REQUESTED.store(false, Ordering::SeqCst);
    std::thread::Builder::new()
        .name("thumb-observer".into())
        .spawn(move || unsafe {
            let pool: *mut AnyObject = msg_send![class!(NSAutoreleasePool), new];
            // Publish the command source/runloop before AX installation and prewarming.
            // Launch/Terminate commands arriving during startup keep the source signaled
            // and drain immediately once the runloop starts instead of waiting for a later wake.
            let src = CFRunLoopSourceCreate(
                std::ptr::null(),
                0,
                &CFRunLoopSourceContext {
                    version: 0,
                    info: std::ptr::null_mut(),
                    retain: std::ptr::null(),
                    release: std::ptr::null(),
                    copy_description: std::ptr::null(),
                    equal: std::ptr::null(),
                    hash: std::ptr::null(),
                    schedule: std::ptr::null(),
                    cancel: std::ptr::null(),
                    perform: Some(drain_obs_commands),
                },
            );
            let rl = CFRunLoopGetCurrent();
            if !src.is_null() {
                CFRunLoopAddSource(rl, src, kCFRunLoopDefaultMode);
                *CMD_SOURCE.0.lock().unwrap() = Some(RunLoopSourceHandle(src));
            }
            *OBSERVER_RL.0.lock().unwrap() = Some(RunLoopHandle(rl));
            // Commands arriving in the tiny window before source publication could not
            // signal it, so explicitly drain once after publication.
            drain_obs_commands(std::ptr::null_mut());

            // Enumerate running apps and install observers here (AXObserverCreate
            // must run on the thread whose runloop pumps the observer's source).
            let pids = regular_running_pids();
            for pid in &pids {
                install_observer_for_pid(*pid);
            }
            log_debug!(
                "[thumb] observer thread started: {} regular apps observed, capture_allowed={}",
                pids.len(),
                capture_allowed()
            );
            pregen_startup_windows(startup_jobs);
            if !STOP_REQUESTED.load(Ordering::SeqCst) {
                CFRunLoopRun();
            }
            *OBSERVER_RL.0.lock().unwrap() = None;
            let _: () = msg_send![pool, drain];
        })
        .expect("spawn thumb-observer thread");
}

/// The runloop source's perform: drains and executes the command queue.
unsafe extern "C" fn drain_obs_commands(_info: *mut c_void) {
    let Some(rx) = CMD_RX.get() else {
        return;
    };
    while let Ok(cmd) = rx.try_recv() {
        match cmd {
            ObsCmd::Install(pid) => {
                install_observer_for_pid(pid);
                pregen_windows_for_pid(pid);
            }
            ObsCmd::Remove(pid) => {
                if let Some((obs, src)) = INSTALLED.0.lock().unwrap().remove(&pid) {
                    let rl = CFRunLoopGetCurrent();
                    CFRunLoopRemoveSource(rl, src.0, kCFRunLoopDefaultMode);
                    CFRelease(obs.0 as *const c_void);
                }
                // Evict the dead app's cached frames too: they will never be shown
                // again and would only crowd out live windows' frames.
                let evicted = CACHE.lock().unwrap().remove_where(|(k, _)| k.pid == pid);
                for t in evicted {
                    CFRelease(t.img);
                }
            }
        }
    }
}

/// Forwarding point for NSWorkspaceDidLaunch (called on main). Installs the new
/// app's observer and pre-generates its existing windows.
pub(crate) fn app_launched(pid: i32) {
    // Restore PID liveness even before the service starts, covering rapid exit/PID
    // reuse during startup.
    CAPTURE_STATE.lock().unwrap().activate_pid(pid);
    if !STARTED.load(Ordering::SeqCst) || pid == std::process::id() as i32 {
        return;
    }
    if let Some(tx) = CMD_TX.get() {
        match tx.try_send(ObsCmd::Install(pid)) {
            Ok(()) => signal_observer_runloop(),
            Err(flume::TrySendError::Full(_)) => {
                log_debug!("[thumb] observer install command dropped (queue full) pid={pid}");
            }
            Err(flume::TrySendError::Disconnected(_)) => {
                log_debug!("[thumb] observer install command dropped (worker stopped) pid={pid}");
            }
        }
    }
}

/// Forwarding point for NSWorkspaceDidTerminate: cancel captures immediately,
/// then remove the observer and cached frames asynchronously.
pub(crate) fn app_terminated(pid: i32) {
    // Invalidate queued/in-flight captures and clear the cache under the same lifecycle
    // lock; the observer thread then only needs to remove the AX source. This lock order
    // prevents a late capture from being inserted after cleanup.
    let mut state = CAPTURE_STATE.lock().unwrap();
    state.cancel_pid(pid);
    let evicted = CACHE
        .lock()
        .unwrap()
        .remove_where(|(key, _)| key.pid == pid);
    drop(state);
    for thumb in evicted {
        unsafe {
            CFRelease(thumb.img);
        }
    }
    // The terminated app's pending blank-retry slots become moot as well.
    super::forget_blank_retries_for_pid(pid);
    super::forget_focused_prewarm_for_pid(pid);
    if let Some(tx) = CMD_TX.get() {
        match tx.try_send(ObsCmd::Remove(pid)) {
            Ok(()) => signal_observer_runloop(),
            Err(flume::TrySendError::Full(_)) => {
                log_debug!("[thumb] observer remove command dropped (queue full) pid={pid}");
            }
            Err(flume::TrySendError::Disconnected(_)) => {
                log_debug!("[thumb] observer remove command dropped (worker stopped) pid={pid}");
            }
        }
    }
}

/// Wake the observer runloop from any thread: Signal marks the source (its perform
/// drains the command queue) and WakeUp makes sure the runloop actually wakes.
fn signal_observer_runloop() {
    let src = CMD_SOURCE.0.lock().unwrap();
    if let Some(src) = *src {
        unsafe {
            CFRunLoopSourceSignal(src.0);
        }
    }
    drop(src);
    let rl = OBSERVER_RL.0.lock().unwrap();
    if let Some(rl) = *rl {
        unsafe {
            CFRunLoopWakeUp(rl.0);
        }
    }
}

/// Running apps with .regular activation policy (excluding ourselves). Menu-bar
/// agents/background processes have no standard windows -- observing them would
/// burn AX round-trips for nothing.
fn regular_running_pids() -> Vec<i32> {
    unsafe {
        let pool: *mut AnyObject = msg_send![class!(NSAutoreleasePool), new];
        let mut out: Vec<i32> = Vec::new();
        let ws: *mut AnyObject = msg_send![class!(NSWorkspace), sharedWorkspace];
        let running: *mut AnyObject = msg_send![ws, runningApplications];
        let count: usize = msg_send![running, count];
        for i in 0..count {
            let app: *mut AnyObject = msg_send![running, objectAtIndex: i];
            let pid: i32 = msg_send![app, processIdentifier];
            // NSApplicationActivationPolicyRegular = 0
            let policy: i64 = msg_send![app, activationPolicy];
            if pid > 0 && policy == 0 && pid != std::process::id() as i32 {
                out.push(pid);
            }
        }
        let _: () = msg_send![pool, drain];
        out
    }
}

/// Install an AXObserver for one PID (MUST run on the observer thread). Failures
/// are logged only -- some apps refuse AX observation, which is normal.
unsafe fn install_observer_for_pid(pid: i32) {
    {
        let installed = INSTALLED.0.lock().unwrap();
        if installed.contains_key(&pid) {
            return;
        }
    }
    let mut obs: AxObserverRef = std::ptr::null_mut();
    if AXObserverCreate(pid, thumb_ax_observer, &mut obs) != 0 || obs.is_null() {
        log_debug!("[thumb] AXObserverCreate failed for pid={}", pid);
        return;
    }
    let app_el = AXUIElementCreateApplication(pid);
    if app_el.is_null() {
        CFRelease(obs as *const c_void);
        return;
    }
    let err = AXObserverAddNotification(obs, app_el, AX_WINDOW_CREATED.ptr, std::ptr::null_mut());
    if err != 0 {
        log_debug!(
            "[thumb] add kAXWindowCreated failed for pid={} err={}",
            pid,
            err
        );
        CFRelease(app_el);
        CFRelease(obs as *const c_void);
        return;
    }
    let src = AXObserverGetRunLoopSource(obs);
    let rl = CFRunLoopGetCurrent();
    CFRunLoopAddSource(rl, src, kCFRunLoopDefaultMode);
    CFRelease(app_el); // the observer holds what it needs
    INSTALLED
        .0
        .lock()
        .unwrap()
        .insert(pid, (AxObserverHandle(obs), RunLoopSourceHandle(src)));
}

/// Startup prewarming reuses AppState's already AX-paired MRU snapshot instead of
/// repeating one AX query per PID. It takes only the first STARTUP_PREWARM_MAX
/// non-minimized windows with usable bounds.
unsafe fn pregen_startup_windows(startup_jobs: Vec<(i32, u32, u64)>) {
    if !crate::theme::thumbnails_enabled() || !capture_allowed() {
        log_debug!("[thumb] startup prewarm skipped (disabled or unauthorized)");
        return;
    }
    let eligible = startup_jobs.len();
    let jobs = startup_jobs
        .into_iter()
        .take(STARTUP_PREWARM_MAX)
        .collect::<Vec<_>>();
    let mut queued = 0;
    for (pid, wid, pid_generation) in &jobs {
        queued += usize::from(enqueue_job_for_generation(
            *pid,
            *wid,
            BASE_TARGET_PX_H,
            CapturePriority::Startup,
            *pid_generation,
        ));
    }
    log_debug!(
        "[thumb] startup prewarm: eligible={} bounded={} queued={}",
        eligible,
        jobs.len(),
        queued
    );
}

/// Pre-generate existing standard windows for a newly launched app; the initial
/// startup batch uses pregen_startup_windows instead.
unsafe fn pregen_windows_for_pid(pid: i32) {
    if !crate::theme::thumbnails_enabled() || !capture_allowed() {
        return;
    }
    let pid_generation = CAPTURE_STATE.lock().unwrap().pid_generation(pid);
    let Some(windows) = crate::window_collector::get_ax_windows_for_pid(pid) else {
        log_debug!("[thumb] pregen pid={}: AX query failed", pid);
        return;
    };
    let mut queued = 0;
    for (wid, _title, minimized) in windows {
        // wid=0 = degenerate entries whose _AXUIElementGetWindow failed; capturing
        // them always fails.
        if minimized || wid == 0 {
            continue; // no backing store while minimized
        }
        queued += usize::from(enqueue_job_for_generation(
            pid,
            wid,
            BASE_TARGET_PX_H,
            CapturePriority::NewWindow,
            pid_generation,
        ));
    }
    log_debug!("[thumb] pregen pid={}: {} windows queued", pid, queued);
}

/// The AXObserver callback: kAXWindowCreated -> resolve the new window's cgwid ->
/// pre-generate after a 300ms debounce (on a throwaway thread so the observer
/// runloop never blocks).
unsafe extern "C" fn thumb_ax_observer(
    _observer: AxObserverRef,
    element: *const c_void,
    notification: *const c_void,
    _info: *mut c_void,
) {
    let pool: *mut AnyObject = msg_send![class!(NSAutoreleasePool), new];
    // Notification names compare by string value (the literal equals the system
    // constant; see AX_WINDOW_CREATED).
    if !notification.is_null()
        && unsafe { CFStringCompare(notification, AX_WINDOW_CREATED.ptr, 0) } == 0
    {
        let mut wid: u32 = 0;
        if crate::window_collector::ax_window_cgwid(element).is_some_and(|resolved| {
            wid = resolved;
            wid != 0
        }) {
            let mut pid: i32 = 0;
            AXUIElementGetPid(element, &mut pid);
            if pid > 0 && crate::theme::thumbnails_enabled() {
                let pid_generation = CAPTURE_STATE.lock().unwrap().pid_generation(pid);
                // Debounce 300ms: brand-new windows may still be laying out / blank.
                std::thread::spawn(move || {
                    std::thread::sleep(Duration::from_millis(300));
                    if crate::theme::thumbnails_enabled() && capture_allowed() {
                        enqueue_job_for_generation(
                            pid,
                            wid,
                            BASE_TARGET_PX_H,
                            CapturePriority::NewWindow,
                            pid_generation,
                        );
                    }
                });
            }
        }
    }
    let _: () = msg_send![pool, drain];
}
