//! Reports a mouse press on the Dock, so the preview closes on the press that starts a switch.
//!
//! The press belongs to the Dock process: the preview panel never covers the tile. This module
//! listens to the Dock's own mouse events with a per-PID tap. When the click-hide child switch is
//! off that tap is listen-only, exactly as before. When it is on the tap is an ACTIVE pass-through
//! tap: almost every event is returned unchanged, but the tap runs before delivery to the Dock. That
//! ordering lets the callback gather pre-click window evidence instead of racing the Dock's own
//! restore/create behavior; the one event it may swallow is an accepted Command+right-click, whose
//! menu would otherwise flash before the quit it was meant to replace.

use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, AtomicPtr, Ordering};
use std::sync::mpsc::{channel, Receiver, RecvTimeoutError, Sender, TryRecvError};
use std::sync::{LazyLock, Mutex, OnceLock};
use std::time::Duration;

use crate::dock_preview::layout::Rect;
use crate::event_tap::{
    kCFRunLoopDefaultMode, tap_options, tap_placement, uptime_ns, CFMachPortCreateRunLoopSource,
    CFRunLoopAddSource, CGEventGetFlags, CGEventGetLocation, CGEventGetTimestamp, CGEventRef,
    CGEventTapCreateForPid, CGEventTapEnable, CGEventTapProxy, CGEventType,
    TAP_DISABLED_BY_TIMEOUT,
};
use crate::ffi::{
    make_nsstring, nsstring_to_rust, release_obj, AXUIElementCopyElementAtPosition,
    AXUIElementCreateApplication, AXUIElementGetPid, AXUIElementGetTypeID,
    AXUIElementSetMessagingTimeout, CFGetTypeID, CFRelease, CFRunLoopRunInMode,
};
use crate::{log_debug, log_info};

/// kCGEventLeftMouseDown.
const LEFT_MOUSE_DOWN: CGEventType = 1;
/// kCGEventLeftMouseUp.
const LEFT_MOUSE_UP: CGEventType = 2;
/// kCGEventRightMouseDown: the Dock menu replaces the flyout the way a Windows jump list does, so a
/// right press dismisses the preview too.
const RIGHT_MOUSE_DOWN: CGEventType = 3;
/// kCGEventLeftMouseDragged.
const LEFT_MOUSE_DRAGGED: CGEventType = 6;
/// How long the watcher thread waits for a command while no tap is installed.
const IDLE_POLL: Duration = Duration::from_millis(100);
/// How long one run-loop slice may last while the tap is installed.
const COMMAND_POLL: Duration = Duration::from_secs(1);
const RUN_FINISHED: i32 = 1;
const RUN_STOPPED: i32 = 2;

/// The cumulative synchronous evidence budget in the active callback. The measured CG window query
/// is sub-millisecond on a busy machine; anything worse is treated as missing evidence, never as a
/// reason to make the user's click wait longer.
const EVIDENCE_BUDGET_NS: u64 = 3_000_000;
const MAX_CLICK_DURATION_NS: u64 = 500_000_000;
const MAX_CLICK_TRAVEL: f64 = 12.0;
const MODIFIER_MASK: u64 = 0x0010_0000 | 0x0008_0000 | 0x0004_0000 | 0x0002_0000;

/// Evidence gathered before the press was delivered to the Dock. It is a value object: nothing in
/// the later click-action path re-asks these pre-click questions.
#[derive(Clone, Debug)]
pub(crate) struct DockClickCandidate {
    pub(crate) session: u64,
    pub(crate) pid: i32,
    pub(crate) bundle: String,
    pub(crate) identity_key: String,
    pub(crate) process_start_time_us: Option<u64>,
    /// One visible ordinary window when the pre-click evidence found one. It remains a strictness
    /// anchor for destructive frontmost actions; an all-minimized app legitimately has none.
    pub(crate) anchor_window: Option<u32>,
    /// Whether the target was frontmost before this click. A non-frontmost click is still evidence
    /// for restore-on-activation, but never for the frontmost hide/minimize action.
    pub(crate) was_frontmost: bool,
    pub(crate) tile_generation: u64,
    pub(crate) settings_generation: u64,
    pub(crate) down_timestamp_ns: u64,
}

#[derive(Clone, Debug)]
struct DownEvidence {
    candidate: DockClickCandidate,
    point: (f64, f64),
    tile: Rect,
}

/// The verified target of a Command+right-click. It carries no window evidence: termination is
/// verified against the application instance itself in the worker.
#[derive(Clone, Debug)]
pub(crate) struct QuickQuitCandidate {
    pub(crate) session: u64,
    pub(crate) pid: i32,
    pub(crate) bundle: String,
    pub(crate) identity_key: String,
    pub(crate) process_start_time_us: Option<u64>,
    pub(crate) settings_generation: u64,
    pub(crate) force: bool,
}

static DOWN: LazyLock<Mutex<Option<DownEvidence>>> = LazyLock::new(|| Mutex::new(None));

/// A visible-window anchor and process identity learned by the Dock preview's normal collection.
/// The active click probe re-verifies the anchor synchronously before using it; this cache exists
/// only so that verification can ask about one window instead of enumerating the whole screen.
#[derive(Clone, Debug)]
struct ClickWindowSeed {
    pid: i32,
    process_start_time_us: Option<u64>,
    anchor_window: u32,
}

static CLICK_WINDOW_SEED: LazyLock<Mutex<Option<ClickWindowSeed>>> =
    LazyLock::new(|| Mutex::new(None));

pub(crate) fn note_click_window_seed(
    pid: i32,
    process_start_time_us: Option<u64>,
    anchor_window: u32,
) {
    if pid <= 0 || anchor_window == 0 {
        return;
    }
    *CLICK_WINDOW_SEED.lock().unwrap() = Some(ClickWindowSeed {
        pid,
        process_start_time_us,
        anchor_window,
    });
}

fn click_window_seed(pid: i32) -> Option<ClickWindowSeed> {
    CLICK_WINDOW_SEED
        .lock()
        .unwrap()
        .clone()
        .filter(|seed| seed.pid == pid)
}

/// The tap's callback state.
struct Context {
    /// The feature session this install belongs to.
    session: u64,
    /// The Dock process this per-pid tap observes.
    dock_pid: i32,
    /// Whether this install wanted the active evidence tap. The actual creation can still fall
    /// back to passive observation; `up` events simply never produce a candidate then.
    evidence_enabled: bool,
    /// Whether the installed tap is the active pass-through tap. Quick quit may suppress the
    /// right-click menu only in that mode; a passive fallback must not pretend the menu is gone.
    active_tap: AtomicBool,
    /// The tap handle the callback needs to re-enable itself after a timeout disable.
    tap: AtomicPtr<c_void>,
    /// Set immediately before this module disables its own tap.
    stopping: AtomicBool,
}

/// Command channel shared with the watcher thread.
enum Command {
    Start {
        pid: i32,
        session: u64,
        evidence_enabled: bool,
    },
    Stop,
}

/// Owner handle for the watcher thread.
pub(crate) struct Handle {
    commands: Sender<Command>,
}

impl Handle {
    /// Point the tap at the Dock process. The evidence mode is part of the installation identity:
    /// changing the child switch must rebuild the tap because only the active mode receives events
    /// before delivery and observes mouse-up/dragged.
    pub(crate) fn start(&self, pid: i32, session: u64, evidence_enabled: bool) {
        let _ = self.commands.send(Command::Start {
            pid,
            session,
            evidence_enabled,
        });
    }

    pub(crate) fn stop(&self) {
        let _ = self.commands.send(Command::Stop);
    }
}

/// Spawn the single watcher thread. It parks on its command channel until the feature starts it.
pub(crate) fn spawn() -> Handle {
    let (commands, receiver) = channel();
    let _ = std::thread::Builder::new()
        .name("com.eacryo.oh-my-tab.dock-preview-click".to_string())
        .spawn(move || run(receiver));
    Handle { commands }
}

enum ActiveOutcome {
    Stopped,
    Disconnected,
    Restart(i32, u64, bool),
}

fn run(receiver: Receiver<Command>) {
    let mut pending: Option<(i32, u64, bool)> = None;
    loop {
        let (pid, session, evidence_enabled) = match pending.take() {
            Some(next) => next,
            None => match receiver.recv_timeout(IDLE_POLL) {
                Ok(Command::Start {
                    pid,
                    session,
                    evidence_enabled,
                }) => {
                    if !crate::input_monitor::taps_allowed() {
                        log_debug!("[dock-preview] Dock click watcher skipped: taps unavailable");
                        continue;
                    }
                    (pid, session, evidence_enabled)
                }
                Ok(Command::Stop) => continue,
                Err(RecvTimeoutError::Timeout) => continue,
                Err(RecvTimeoutError::Disconnected) => return,
            },
        };
        match run_active(&receiver, pid, session, evidence_enabled) {
            ActiveOutcome::Stopped => {}
            ActiveOutcome::Disconnected => return,
            ActiveOutcome::Restart(pid, session, evidence_enabled) => {
                pending = Some((pid, session, evidence_enabled));
            }
        }
    }
}

fn mask_for(evidence_enabled: bool) -> u64 {
    let mut mask = (1u64 << LEFT_MOUSE_DOWN) | (1u64 << RIGHT_MOUSE_DOWN);
    if evidence_enabled {
        mask |= (1u64 << LEFT_MOUSE_UP) | (1u64 << LEFT_MOUSE_DRAGGED);
    }
    mask
}

/// Install the per-PID tap, serve the run loop, and tear down on `Stop`.
fn run_active(
    receiver: &Receiver<Command>,
    pid: i32,
    session: u64,
    evidence_enabled: bool,
) -> ActiveOutcome {
    if !crate::input_monitor::taps_allowed() {
        log_debug!("[dock-preview] Dock click tap skipped: taps unavailable");
        return ActiveOutcome::Stopped;
    }
    if evidence_enabled {
        probe_executor();
    }
    *DOWN.lock().unwrap() = None;
    let context = Box::into_raw(Box::new(Context {
        session,
        dock_pid: pid,
        evidence_enabled,
        active_tap: AtomicBool::new(false),
        tap: AtomicPtr::new(std::ptr::null_mut()),
        stopping: AtomicBool::new(false),
    }));
    let options = if evidence_enabled {
        tap_options::DEFAULT_TAP
    } else {
        tap_options::LISTEN_ONLY
    };
    let mut tap = unsafe {
        CGEventTapCreateForPid(
            pid,
            tap_placement::TAIL_INSERT,
            options,
            mask_for(evidence_enabled),
            Some(press_callback),
            context as *mut c_void,
        )
    };
    let mut passive_fallback = false;
    if tap.is_null() && evidence_enabled {
        // The preview's press dismissal must not depend on the active tap being grantable. Fall
        // back to the old passive tap; clicks then simply keep native behavior because no reliable
        // pre-delivery evidence exists.
        passive_fallback = true;
        tap = unsafe {
            CGEventTapCreateForPid(
                pid,
                tap_placement::TAIL_INSERT,
                tap_options::LISTEN_ONLY,
                mask_for(false),
                Some(press_callback),
                context as *mut c_void,
            )
        };
    }
    if tap.is_null() {
        log_info!("[dock-preview] Dock click tap could not be created");
        unsafe { drop(Box::from_raw(context)) };
        return ActiveOutcome::Stopped;
    }
    let source = unsafe { CFMachPortCreateRunLoopSource(std::ptr::null_mut(), tap, 0) };
    if source.is_null() {
        log_info!("[dock-preview] Dock click tap has no run-loop source");
        unsafe {
            crate::ffi::CFRelease(tap as *const c_void);
            drop(Box::from_raw(context));
        }
        return ActiveOutcome::Stopped;
    }
    let run_loop = unsafe { crate::event_tap::CFRunLoopGetCurrent() };
    unsafe {
        (*context).tap.store(tap, Ordering::SeqCst);
        (*context)
            .active_tap
            .store(evidence_enabled && !passive_fallback, Ordering::SeqCst);
        CFRunLoopAddSource(run_loop, source, kCFRunLoopDefaultMode);
    }
    log_info!(
        "[dock-preview] Dock click watcher active pid={pid} session={session} evidence={} fallback={}",
        evidence_enabled,
        passive_fallback
    );

    let release = || unsafe {
        (*context).stopping.store(true, Ordering::SeqCst);
        *DOWN.lock().unwrap() = None;
        crate::event_tap::teardown_event_tap(
            run_loop,
            crate::event_tap::CreatedEventTap { tap, source },
        );
        drop(Box::from_raw(context));
    };

    loop {
        let result =
            unsafe { CFRunLoopRunInMode(kCFRunLoopDefaultMode, COMMAND_POLL.as_secs_f64(), 1) };
        if result == RUN_FINISHED || result == RUN_STOPPED {
            release();
            log_info!("[dock-preview] Dock click tap lost its run loop; watcher parked");
            return ActiveOutcome::Stopped;
        }
        if !crate::input_monitor::taps_allowed() {
            release();
            log_info!("[dock-preview] Dock click tap released: input taps no longer allowed");
            return ActiveOutcome::Stopped;
        }
        loop {
            match receiver.try_recv() {
                Ok(Command::Start {
                    pid: new_pid,
                    session: new_session,
                    evidence_enabled: new_evidence,
                }) if new_pid != pid
                    || new_session != session
                    || new_evidence != evidence_enabled =>
                {
                    release();
                    log_debug!(
                        "[dock-preview] Dock click watcher moving to pid={new_pid} session={new_session} evidence={new_evidence}"
                    );
                    return ActiveOutcome::Restart(new_pid, new_session, new_evidence);
                }
                Ok(Command::Stop) => {
                    release();
                    log_debug!("[dock-preview] Dock click watcher released");
                    return ActiveOutcome::Stopped;
                }
                Ok(Command::Start { .. }) => {}
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    release();
                    return ActiveOutcome::Disconnected;
                }
            }
        }
    }
}

fn event_point(event: CGEventRef) -> (f64, f64) {
    unsafe {
        let point = CGEventGetLocation(event);
        (point.x, point.y)
    }
}

fn unmodified(event: CGEventRef) -> bool {
    unsafe { CGEventGetFlags(event) & MODIFIER_MASK == 0 }
}

const COMMAND_FLAG: u64 = 0x0010_0000;
const OPTION_FLAG: u64 = 0x0008_0000;
const CONTROL_FLAG: u64 = 0x0004_0000;
const SHIFT_FLAG: u64 = 0x0002_0000;

/// The quick-quit chord is exactly Command+right-click; Option only strengthens it to force-quit.
/// Shift/Control are not accepted as silent extras because they already have Dock/menu meanings.
fn quick_quit_chord_flags(flags: u64) -> Option<bool> {
    if flags & COMMAND_FLAG == 0 || flags & (CONTROL_FLAG | SHIFT_FLAG) != 0 {
        return None;
    }
    Some(flags & OPTION_FLAG != 0)
}

fn quick_quit_chord(event: CGEventRef) -> Option<bool> {
    quick_quit_chord_flags(unsafe { CGEventGetFlags(event) })
}

/// Resolve the one running instance for `bundle` and verify that it is both the cached target pid
/// and the exact application copy the fresh Dock hit named. A same-bundle copy at another path must
/// fail closed: termination is too destructive to borrow identity from a bundle identifier alone.
unsafe fn sole_running_process_start_for_path(
    bundle: &str,
    expected_pid: i32,
    expected_path: &str,
) -> Option<u64> {
    objc2::rc::autoreleasepool(|_| {
        let ns_bundle = make_nsstring(bundle);
        let apps: *mut objc2::runtime::AnyObject = objc2::msg_send![
            objc2::class!(NSRunningApplication),
            runningApplicationsWithBundleIdentifier: ns_bundle
        ];
        release_obj(ns_bundle);
        if apps.is_null() {
            return None;
        }
        let count: usize = objc2::msg_send![apps, count];
        if count != 1 {
            return None;
        }
        let app: *mut objc2::runtime::AnyObject = objc2::msg_send![apps, objectAtIndex: 0usize];
        if app.is_null() {
            return None;
        }
        let pid: i32 = objc2::msg_send![app, processIdentifier];
        if pid != expected_pid {
            return None;
        }
        let url: *mut objc2::runtime::AnyObject = objc2::msg_send![app, bundleURL];
        if url.is_null() {
            return None;
        }
        let path_obj: *mut objc2::runtime::AnyObject = objc2::msg_send![url, path];
        if path_obj.is_null() || nsstring_to_rust(path_obj) != expected_path {
            return None;
        }
        if crate::app_identity::bundle_id_of(app).as_deref() != Some(bundle) {
            return None;
        }
        crate::app_identity::identity_process_start_time_of_app(app, pid)
    })
}

fn probe_budget_exceeded(started: u64) -> bool {
    uptime_ns().saturating_sub(started) > EVIDENCE_BUDGET_NS
}

/// Owns a CF object across the hit-test walk. AX exceptions are caught by the caller; ownership
/// must not depend on reaching one particular manual release path.
struct OwnedCf(*const c_void);

impl Drop for OwnedCf {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe { CFRelease(self.0) };
        }
    }
}

fn catch_ax_evidence<T: Default>(label: &str, work: impl FnOnce() -> T) -> T {
    match objc2::exception::catch(std::panic::AssertUnwindSafe(work)) {
        Ok(value) => value,
        Err(exception) => {
            log_debug!(
                "[dock-preview] Dock click {} AX exception: {:?}",
                label,
                exception
            );
            T::default()
        }
    }
}

/// Freshly hit-test the press position and return the application path of the Dock item there.
///
/// The cached tile rectangle is only a cheap prefilter: Dock magnification and insertion/removal can
/// move tiles inside the observer's poll interval. Termination is destructive enough that it needs
/// the tile under THIS press, read from the current AX hierarchy, before the menu may be swallowed.
/// The walk is bounded and every element is released on every path.
unsafe fn fresh_dock_tile_path_at(
    point: (f64, f64),
    dock_pid: i32,
    probe_started: u64,
) -> Option<String> {
    // Hit-test inside the Dock application itself. This both narrows the query and prevents an
    // overlapping window owned by another process from being mistaken for the tile under the click.
    let dock = OwnedCf(AXUIElementCreateApplication(dock_pid));
    if dock.0.is_null() {
        return None;
    }
    // A hung Dock must not pin one of the two shared probe threads behind the system default AX
    // timeout. The caller's input budget is much shorter; this bound only decides how soon the
    // worker is reusable after the input itself has already been released.
    AXUIElementSetMessagingTimeout(dock.0 as crate::ffi::AXUIElementRef, 0.02);
    let mut hit: crate::ffi::AXUIElementRef = std::ptr::null_mut();
    let status = AXUIElementCopyElementAtPosition(
        dock.0 as crate::ffi::AXUIElementRef,
        point.0 as f32,
        point.1 as f32,
        &mut hit,
    );
    if status != 0 || hit.is_null() {
        if !hit.is_null() {
            CFRelease(hit);
        }
        return None;
    }
    let mut element = OwnedCf(hit);

    for _ in 0..8 {
        if probe_budget_exceeded(probe_started) {
            return None;
        }
        AXUIElementSetMessagingTimeout(element.0 as crate::ffi::AXUIElementRef, 0.02);
        let mut owner_pid = 0;
        if probe_budget_exceeded(probe_started)
            || AXUIElementGetPid(element.0 as crate::ffi::AXUIElementRef, &mut owner_pid) != 0
            || owner_pid != dock_pid
        {
            return None;
        }
        if probe_budget_exceeded(probe_started) {
            return None;
        }
        if crate::dock_preview::observer::read_string(
            element.0 as crate::ffi::AXUIElementRef,
            "AXSubrole",
        )
        .as_deref()
            == Some("AXApplicationDockItem")
        {
            if probe_budget_exceeded(probe_started) {
                return None;
            }
            return crate::dock_preview::observer::tile_path(
                element.0 as crate::ffi::AXUIElementRef,
            );
        }
        if probe_budget_exceeded(probe_started) {
            return None;
        }
        let parent = crate::dock_preview::observer::copy_attribute(
            element.0 as crate::ffi::AXUIElementRef,
            "AXParent",
        )?;
        let parent = OwnedCf(parent);
        if CFGetTypeID(parent.0) != AXUIElementGetTypeID() {
            return None;
        }
        element = parent;
    }
    None
}

/// Destructive quick-quit evidence is accepted only when the fresh hit names the same application
/// copy as the cached target. The running application's own bundleURL is checked separately before
/// a process start time is accepted; a stale cached rectangle alone can never select a victim.
fn quick_quit_hit_matches_target(hit_path: Option<&str>, expected_path: &str) -> bool {
    hit_path == Some(expected_path)
}

fn travel(a: (f64, f64), b: (f64, f64)) -> f64 {
    let dx = a.0 - b.0;
    let dy = a.1 - b.1;
    (dx * dx + dy * dy).sqrt()
}

type ProbeTask = Box<dyn FnOnce() + Send + 'static>;

/// A tiny persistent executor for input-evidence probes. Thread creation alone can consume the
/// click budget on a busy machine, so probes are queued to already-running workers instead. Two
/// workers keep one stuck operating-system query from disabling every later probe; a task that
/// answers after its caller timed out is simply dropped with the response channel.
fn probe_executor() -> &'static flume::Sender<ProbeTask> {
    static EXECUTOR: OnceLock<flume::Sender<ProbeTask>> = OnceLock::new();
    EXECUTOR.get_or_init(|| {
        let (tx, rx) = flume::bounded::<ProbeTask>(16);
        for index in 0..2 {
            let worker_rx = rx.clone();
            let name = format!("com.eacryo.oh-my-tab.dock-click-probe-{index}");
            let _ = std::thread::Builder::new().name(name).spawn(move || {
                while let Ok(task) = worker_rx.recv() {
                    // Several probes resolve NSRunningApplication/NSURL objects. The probe threads
                    // live forever, so every task needs its own autorelease boundary instead of
                    // relying on each query to remember one.
                    objc2::rc::autoreleasepool(|_| task());
                }
            });
        }
        tx
    })
}

/// Run one evidence query on a worker while the active callback waits only up to the bounded budget.
/// The event is still inside the pass-through callback during that wait, so a result that arrives in
/// time is genuinely pre-delivery evidence; a late result is dropped when the receiver goes away.
fn bounded_probe<T: Send + 'static>(
    label: &'static str,
    callback_started: u64,
    probe: impl FnOnce() -> Option<T> + Send + 'static,
) -> Option<T> {
    let started = callback_started;
    let (answer_tx, answer_rx) = channel();
    if probe_executor()
        .try_send(Box::new(move || {
            // A task can sit behind another slow AX query longer than the input callback is allowed
            // to wait. Once its caller's budget is gone there is no consumer left, so do not start
            // the IPC at all.
            if uptime_ns().saturating_sub(started) <= EVIDENCE_BUDGET_NS {
                let _ = answer_tx.send(probe());
            }
        }))
        .is_err()
    {
        return None;
    }
    let spent = uptime_ns().saturating_sub(started);
    let remaining = EVIDENCE_BUDGET_NS.saturating_sub(spent);
    if remaining == 0 {
        return None;
    }
    let received = answer_rx.recv_timeout(Duration::from_nanos(remaining));
    let elapsed = uptime_ns().saturating_sub(started);
    if received.is_err() || elapsed > EVIDENCE_BUDGET_NS {
        log_debug!(
            "[dock-preview] Dock click {} evidence timed out: {}us",
            label,
            elapsed / 1000
        );
        return None;
    }
    received.ok().flatten()
}

/// Gather the mouse-down evidence inside the bounded active-callback path. `None` is always
/// conservative: the event still passes through unchanged and the click merely keeps native behavior.
fn probe_down(
    tile: crate::dock_preview::observer::ClickTileEvidence,
    point: (f64, f64),
    timestamp: u64,
    session: u64,
    settings_generation: u64,
    callback_started: u64,
) -> Option<DownEvidence> {
    if tile.established_completed_uptime_ns > timestamp || tile.bundle == "com.apple.finder" {
        log_debug!(
            "[dock-preview] Dock click down evidence refused: bundle={} established={} latest_sample={}..{} event_ts={}",
            tile.bundle,
            tile.established_completed_uptime_ns,
            tile.sample_started_uptime_ns,
            tile.sample_completed_uptime_ns,
            timestamp
        );
        return None;
    }
    bounded_probe("dock-click-down-probe", callback_started, move || {
        let probe_started = uptime_ns();
        let frontmost =
            objc2::rc::autoreleasepool(|_| unsafe { crate::app_identity::frontmost_pid() });
        let was_frontmost = frontmost == Some(tile.pid);
        // Establish the instance at this pid BEFORE trusting the seed. The cache is keyed by pid
        // only, and an excluded application may never collect another preview that refreshes it;
        // after pid reuse the old start time would otherwise poison every later click action.
        let (current_bundle, current_start_time_us) = objc2::rc::autoreleasepool(|_| unsafe {
            crate::app_identity::bundle_id_and_process_start_time_us(tile.pid)
        });
        let seed = click_window_seed(tile.pid);
        let plan = seed_evidence_plan(
            current_bundle.as_deref(),
            &tile.bundle,
            current_start_time_us,
            seed.as_ref(),
        );
        if plan == SeedEvidencePlan::Refuse {
            return None;
        }
        let anchor_window = match plan {
            SeedEvidencePlan::UseSeed => {
                let seed = seed?;
                let anchor_visible = crate::window_collector::visible_ordinary_window_now(
                    tile.pid,
                    seed.anchor_window,
                )?;
                // The preview seed is only a fast path. Once that concrete window is confirmed gone
                // or no longer visible, another visible window of the same instance is still evidence
                // for the click action; this matters for applications whose previews are excluded and
                // whose seed therefore cannot be refreshed by another collection.
                let fresh_windows = if anchor_visible {
                    Vec::new()
                } else {
                    crate::window_collector::visible_ordinary_window_ids_for_pid(tile.pid)?
                };
                anchor_from_seed_or_fresh(seed.anchor_window, anchor_visible, &fresh_windows)
            }
            SeedEvidencePlan::Refresh => {
                crate::window_collector::visible_ordinary_window_ids_for_pid(tile.pid)?
                    .first()
                    .copied()
            }
            SeedEvidencePlan::Refuse => unreachable!("the refusal returned above"),
        };
        // `identity_of_app` prefers a non-empty bundle identifier as its key, and the plan above
        // proved that this process publishes the tile's non-empty identifier.
        let identity_key = tile.bundle.clone();
        let process_start_time_us = current_start_time_us?;
        if probe_budget_exceeded(probe_started) {
            return None;
        }
        Some(DownEvidence {
            candidate: DockClickCandidate {
                session,
                pid: tile.pid,
                bundle: tile.bundle.clone(),
                identity_key,
                process_start_time_us: Some(process_start_time_us),
                anchor_window,
                was_frontmost,
                tile_generation: tile.generation,
                settings_generation,
                down_timestamp_ns: timestamp,
            },
            point,
            tile: tile.icon,
        })
    })
}

/// What the click probe may do with the preview's cached window seed after establishing the
/// process instance that currently owns the pid.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SeedEvidencePlan {
    /// The current instance could not be established, or it is not the application the tile named.
    Refuse,
    /// The seed belongs to this same process instance, so its one window can be revalidated
    /// without enumerating the process first.
    UseSeed,
    /// No seed belongs to this instance (absent, or left behind by a reused pid): build evidence
    /// from the current process's visible windows.
    Refresh,
}

fn seed_evidence_plan(
    current_bundle: Option<&str>,
    tile_bundle: &str,
    current_start_time_us: Option<u64>,
    seed: Option<&ClickWindowSeed>,
) -> SeedEvidencePlan {
    if current_bundle != Some(tile_bundle)
        || tile_bundle.is_empty()
        || current_start_time_us.is_none()
    {
        return SeedEvidencePlan::Refuse;
    }
    match seed {
        Some(seed) if seed.process_start_time_us == current_start_time_us => {
            SeedEvidencePlan::UseSeed
        }
        _ => SeedEvidencePlan::Refresh,
    }
}

/// Choose the click anchor after validating the preview seed: a still-visible seed wins without an
/// enumeration; a confirmed-stale seed falls back to the process's current visible windows.
fn anchor_from_seed_or_fresh(
    seed_window: u32,
    seed_visible: bool,
    fresh_windows: &[u32],
) -> Option<u32> {
    if seed_visible {
        Some(seed_window)
    } else {
        fresh_windows.first().copied()
    }
}

/// Revalidate the same concrete anchor window before mouse-up is released. This catches a click
/// held long enough for the user to close or minimize that window with the other hand.
fn probe_up(candidate: &DockClickCandidate, callback_started: u64) -> bool {
    let Some(anchor_window) = candidate.anchor_window else {
        // An all-minimized app has no visible anchor by definition. Identity, click shape and tile
        // stability still gate it; the later worker only restores windows for such a candidate.
        return true;
    };
    let pid = candidate.pid;
    bounded_probe("dock-click-up-probe", callback_started, move || {
        crate::window_collector::visible_ordinary_window_now(pid, anchor_window)
    })
    .unwrap_or(false)
}

fn click_shape_is_valid(
    down_point: (f64, f64),
    up_point: (f64, f64),
    tile: Rect,
    down_timestamp_ns: u64,
    up_timestamp_ns: u64,
) -> bool {
    let Some(duration) = up_timestamp_ns.checked_sub(down_timestamp_ns) else {
        return false;
    };
    tile.contains(up_point)
        && travel(down_point, up_point) <= MAX_CLICK_TRAVEL
        && duration <= MAX_CLICK_DURATION_NS
}

/// The tap callback. In active mode it runs before delivery to the Dock. Everything passes through
/// unchanged except an accepted quick-quit right press, which is swallowed so the Dock menu does not
/// open for a gesture whose meaning is quit instead.
unsafe extern "C" fn press_callback(
    _proxy: CGEventTapProxy,
    event_type: CGEventType,
    event: CGEventRef,
    user_info: *mut c_void,
) -> CGEventRef {
    crate::callback_guard::value("dock_preview_click_tap", event, || {
        if user_info.is_null() {
            return event;
        }
        let context = &*(user_info as *const Context);
        let callback_started = uptime_ns();
        let stopping = context.stopping.load(Ordering::SeqCst);
        if crate::input_monitor::handle_disabled_event(event_type, "dock-preview-click", stopping) {
            // A timeout disable can lose dragged/up events. The incomplete gesture is cancelled
            // before re-enable so its late up cannot pair with a later press.
            *DOWN.lock().unwrap() = None;
            if event_type == TAP_DISABLED_BY_TIMEOUT && crate::input_monitor::taps_allowed() {
                let tap = context.tap.load(Ordering::SeqCst);
                if !tap.is_null() {
                    CGEventTapEnable(tap, true);
                }
            }
            return event;
        }
        if stopping {
            return event;
        }
        if !crate::input_monitor::taps_allowed() {
            *DOWN.lock().unwrap() = None;
            return event;
        }
        let point = event_point(event);
        if event_type == LEFT_MOUSE_DOWN || event_type == RIGHT_MOUSE_DOWN {
            // Every Dock press keeps the old press-dismissal behavior regardless of evidence mode.
            crate::dock_preview::note_dock_click(point, context.session);
        }
        if !context.evidence_enabled {
            return event;
        }
        if !context.active_tap.load(Ordering::SeqCst) {
            // The passive fallback exists only to preserve press-dismissal. Its callbacks run after
            // the Dock has already handled the press, so "pre-click" evidence collected there is not
            // evidence at all and must never authorize a hide/minimize/quit action.
            return event;
        }
        match event_type {
            RIGHT_MOUSE_DOWN => {
                let Some(force) = quick_quit_chord(event) else {
                    return event;
                };
                if !crate::dock_preview::quick_quit_enabled()
                    || !crate::dock_preview::click_session_active(context.session)
                {
                    return event;
                }
                let Some(tile) = crate::dock_preview::observer::click_tile_evidence() else {
                    return event;
                };
                if !tile.icon.contains(point) || tile.bundle == "com.apple.finder" {
                    return event;
                }
                let settings_generation = crate::dock_preview::dock_click_settings_generation();
                let pid = tile.pid;
                let expected_path = tile.path.clone();
                let bundle = tile.bundle.clone();
                let dock_pid = context.dock_pid;
                let process_start_time_us =
                    bounded_probe("dock-quick-quit-probe", callback_started, move || unsafe {
                        // One bundle can exceptionally have two running copies. The Dock tile does
                        // not carry a public pid attribute, so ambiguity is a menu, not a guess.
                        let hit_path = catch_ax_evidence("quick-quit hit-test", || {
                            fresh_dock_tile_path_at(point, dock_pid, callback_started)
                        });
                        if !quick_quit_hit_matches_target(hit_path.as_deref(), &expected_path) {
                            return None;
                        }
                        sole_running_process_start_for_path(&bundle, pid, &expected_path)
                    });
                if process_start_time_us.is_none() {
                    return event;
                }
                let accepted = crate::dock_preview::note_dock_quick_quit_candidate(
                    QuickQuitCandidate {
                        session: context.session,
                        pid: tile.pid,
                        bundle: tile.bundle.clone(),
                        identity_key: tile.bundle.clone(),
                        process_start_time_us,
                        settings_generation,
                        force,
                    },
                    callback_started.saturating_add(EVIDENCE_BUDGET_NS),
                );
                if !accepted {
                    return event;
                }
                // The active tap runs before the Dock menu. Once the gesture is accepted the menu
                // is suppressed so quit/force-quit is the whole interaction rather than a menu flicker.
                return std::ptr::null_mut();
            }
            LEFT_MOUSE_DOWN => {
                *DOWN.lock().unwrap() = None;
                if !crate::dock_preview::left_click_actions_enabled_now() || !unmodified(event) {
                    return event;
                }
                let timestamp = CGEventGetTimestamp(event);
                let settings_generation = crate::dock_preview::dock_click_settings_generation();
                let Some(tile) = crate::dock_preview::observer::click_tile_evidence() else {
                    return event;
                };
                if !tile.icon.contains(point) {
                    return event;
                }
                *DOWN.lock().unwrap() = probe_down(
                    tile,
                    point,
                    timestamp,
                    context.session,
                    settings_generation,
                    callback_started,
                );
            }
            LEFT_MOUSE_DRAGGED => {
                let mut down = DOWN.lock().unwrap();
                if let Some(existing) = down.as_ref() {
                    if travel(existing.point, point) > MAX_CLICK_TRAVEL
                        || !existing.tile.contains(point)
                    {
                        *down = None;
                    }
                }
            }
            LEFT_MOUSE_UP => {
                let Some(down) = DOWN.lock().unwrap().take() else {
                    return event;
                };
                let timestamp = CGEventGetTimestamp(event);
                let same_tile = click_shape_is_valid(
                    down.point,
                    point,
                    down.tile,
                    down.candidate.down_timestamp_ns,
                    timestamp,
                );
                let current_tile = crate::dock_preview::observer::click_tile_evidence();
                let tile_stable = current_tile
                    .as_ref()
                    .is_some_and(|tile| tile.generation == down.candidate.tile_generation);
                let settings_stable = down.candidate.settings_generation
                    == crate::dock_preview::dock_click_settings_generation();
                let up_anchor_ok = settings_stable && probe_up(&down.candidate, callback_started);
                if unmodified(event) && same_tile && tile_stable && up_anchor_ok {
                    crate::dock_preview::note_dock_click_candidate(down.candidate);
                }
            }
            _ => {}
        }
        event
    })
}

#[cfg(test)]
mod tests {
    use super::{
        anchor_from_seed_or_fresh, bounded_probe, click_shape_is_valid, quick_quit_chord_flags,
        quick_quit_hit_matches_target, seed_evidence_plan, travel, ClickWindowSeed,
        SeedEvidencePlan, COMMAND_FLAG, CONTROL_FLAG, MAX_CLICK_DURATION_NS, OPTION_FLAG,
        SHIFT_FLAG,
    };
    use crate::dock_preview::layout::Rect;
    use std::time::{Duration, Instant};

    #[test]
    fn click_travel_boundary_is_conservative() {
        assert!(travel((0.0, 0.0), (3.0, 4.0)) == 5.0);
    }

    #[test]
    fn click_shape_accepts_only_a_short_quiet_click_on_the_same_tile() {
        let tile = Rect::new(100.0, 100.0, 40.0, 40.0);
        assert!(click_shape_is_valid(
            (110.0, 110.0),
            (112.0, 111.0),
            tile,
            10,
            20
        ));
        assert!(!click_shape_is_valid(
            (110.0, 110.0),
            (130.0, 110.0),
            tile,
            10,
            20
        ));
        assert!(!click_shape_is_valid(
            (110.0, 110.0),
            (141.0, 110.0),
            tile,
            10,
            20
        ));
        assert!(!click_shape_is_valid(
            (110.0, 110.0),
            (111.0, 111.0),
            tile,
            10,
            10 + MAX_CLICK_DURATION_NS + 1
        ));
        // A backwards timestamp pair is not evidence of a click at all.
        assert!(!click_shape_is_valid(
            (110.0, 110.0),
            (111.0, 111.0),
            tile,
            20,
            10
        ));
    }

    #[test]
    fn quick_quit_accepts_command_and_option_but_no_extra_modifier() {
        assert_eq!(quick_quit_chord_flags(COMMAND_FLAG), Some(false));
        assert_eq!(
            quick_quit_chord_flags(COMMAND_FLAG | OPTION_FLAG),
            Some(true)
        );
        assert_eq!(quick_quit_chord_flags(OPTION_FLAG), None);
        assert_eq!(quick_quit_chord_flags(COMMAND_FLAG | SHIFT_FLAG), None);
        assert_eq!(quick_quit_chord_flags(COMMAND_FLAG | CONTROL_FLAG), None);
    }

    #[test]
    fn a_stale_quick_quit_rectangle_without_matching_hit_evidence_is_rejected() {
        assert!(!quick_quit_hit_matches_target(
            None,
            "/Applications/Safari.app"
        ));
        assert!(!quick_quit_hit_matches_target(
            Some("/Applications/Safari-copy.app"),
            "/Applications/Safari.app"
        ));
        assert!(quick_quit_hit_matches_target(
            Some("/Applications/Safari.app"),
            "/Applications/Safari.app"
        ));
    }

    #[test]
    fn a_seed_is_used_only_after_the_current_process_instance_matches() {
        let seed = |process_start_time_us| ClickWindowSeed {
            pid: 42,
            process_start_time_us,
            anchor_window: 7,
        };
        assert_eq!(
            seed_evidence_plan(
                Some("com.example.App"),
                "com.example.App",
                Some(200),
                Some(&seed(Some(100))),
            ),
            SeedEvidencePlan::Refresh,
            "the same pid with another start time is another instance: its seed is evidence for nothing"
        );
        assert_eq!(
            seed_evidence_plan(
                Some("com.example.App"),
                "com.example.App",
                Some(200),
                Some(&seed(Some(200))),
            ),
            SeedEvidencePlan::UseSeed
        );
        assert_eq!(
            seed_evidence_plan(Some("com.example.App"), "com.example.App", Some(200), None),
            SeedEvidencePlan::Refresh
        );
        assert_eq!(
            seed_evidence_plan(Some("com.example.Copy"), "com.example.App", Some(200), None),
            SeedEvidencePlan::Refuse
        );
        assert_eq!(
            seed_evidence_plan(Some("com.example.App"), "com.example.App", None, None),
            SeedEvidencePlan::Refuse
        );
    }

    #[test]
    fn a_confirmed_stale_seed_anchor_falls_back_to_a_current_visible_window() {
        assert_eq!(anchor_from_seed_or_fresh(7, true, &[9, 10]), Some(7));
        assert_eq!(anchor_from_seed_or_fresh(7, false, &[9, 10]), Some(9));
        assert_eq!(anchor_from_seed_or_fresh(7, false, &[]), None);
    }

    #[test]
    fn bounded_probe_drops_a_late_answer_inside_the_budget() {
        let started = Instant::now();
        assert_eq!(
            bounded_probe("test-probe-timeout", crate::event_tap::uptime_ns(), || {
                std::thread::sleep(Duration::from_millis(20));
                Some(7)
            }),
            None
        );
        assert!(
            started.elapsed() < Duration::from_millis(15),
            "the bounded probe must return near its 3ms budget, not after the worker: {:?}",
            started.elapsed()
        );
        assert_eq!(
            bounded_probe("test-probe-fast", crate::event_tap::uptime_ns(), || Some(9)),
            Some(9)
        );
    }
}
