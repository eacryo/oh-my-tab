//! Reports a mouse press on the Dock, so the preview closes on the press that starts a switch.
//!
//! The press belongs to the Dock process: the preview panel never covers the tile. This module
//! listens to the Dock's own mouse events with a per-PID tap. When the click-hide child switch is
//! off that tap is listen-only, exactly as before. When it is on the tap is an ACTIVE pass-through
//! tap: it still returns every event unchanged, but an active tap runs before the event is delivered
//! to the Dock. That ordering is what lets the callback gather pre-click window evidence instead of
//! racing the Dock's own restore/create behavior.

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
use crate::ffi::CFRunLoopRunInMode;
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
/// the later hide path re-asks these pre-click questions.
#[derive(Clone, Debug)]
pub(crate) struct HideCandidate {
    pub(crate) session: u64,
    pub(crate) pid: i32,
    pub(crate) bundle: String,
    pub(crate) identity_key: String,
    pub(crate) process_start_time_us: Option<u64>,
    pub(crate) anchor_window: u32,
    pub(crate) tile_generation: u64,
    pub(crate) down_timestamp_ns: u64,
}

#[derive(Clone, Debug)]
struct DownEvidence {
    candidate: HideCandidate,
    point: (f64, f64),
    tile: Rect,
}

static DOWN: LazyLock<Mutex<Option<DownEvidence>>> = LazyLock::new(|| Mutex::new(None));

/// A visible-window anchor and process identity learned by the Dock preview's normal collection.
/// The active click probe re-verifies the anchor synchronously before using it; this cache exists
/// only so that verification can ask about one window instead of enumerating the whole screen.
#[derive(Clone, Debug)]
struct ClickWindowSeed {
    pid: i32,
    identity_key: String,
    process_start_time_us: Option<u64>,
    anchor_window: u32,
}

static CLICK_WINDOW_SEED: LazyLock<Mutex<Option<ClickWindowSeed>>> =
    LazyLock::new(|| Mutex::new(None));

pub(crate) fn note_click_window_seed(
    pid: i32,
    identity_key: String,
    process_start_time_us: Option<u64>,
    anchor_window: u32,
) {
    if pid <= 0 || identity_key.is_empty() || anchor_window == 0 {
        return;
    }
    *CLICK_WINDOW_SEED.lock().unwrap() = Some(ClickWindowSeed {
        pid,
        identity_key,
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
    /// Whether this install wanted the active evidence tap. The actual creation can still fall
    /// back to passive observation; `up` events simply never produce a candidate then.
    evidence_enabled: bool,
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
        evidence_enabled,
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
                    task();
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
            let _ = answer_tx.send(probe());
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
        if frontmost != Some(tile.pid) {
            log_debug!(
                "[dock-preview] Dock click down evidence refused: frontmost={:?} target_pid={}",
                frontmost,
                tile.pid
            );
            return None;
        }
        let seed = click_window_seed(tile.pid);
        let (anchor_window, identity_key, process_start_time_us) = if let Some(seed) = seed {
            let anchor_visible =
                crate::window_collector::visible_ordinary_window_now(tile.pid, seed.anchor_window)?;
            if !anchor_visible {
                log_debug!(
                    "[dock-preview] Dock click down evidence refused: seeded anchor {} is not visible pid={}",
                    seed.anchor_window,
                    tile.pid
                );
                return None;
            }
            (
                seed.anchor_window,
                seed.identity_key,
                seed.process_start_time_us?,
            )
        } else {
            let windows = crate::window_collector::visible_ordinary_window_ids_for_pid(tile.pid)?;
            let Some(anchor_window) = windows.first().copied() else {
                log_debug!(
                    "[dock-preview] Dock click down evidence refused: no visible ordinary window pid={}",
                    tile.pid
                );
                return None;
            };
            let process_start_time_us = objc2::rc::autoreleasepool(|_| unsafe {
                crate::app_identity::identity_process_start_time_us(tile.pid)
            })?;
            (anchor_window, tile.bundle.clone(), process_start_time_us)
        };
        if uptime_ns().saturating_sub(probe_started) > EVIDENCE_BUDGET_NS {
            return None;
        }
        Some(DownEvidence {
            candidate: HideCandidate {
                session,
                pid: tile.pid,
                bundle: tile.bundle.clone(),
                identity_key,
                process_start_time_us: Some(process_start_time_us),
                anchor_window,
                tile_generation: tile.generation,
                down_timestamp_ns: timestamp,
            },
            point,
            tile: tile.icon,
        })
    })
}

/// Revalidate the same concrete anchor window before mouse-up is released. This catches a click
/// held long enough for the user to close or minimize that window with the other hand.
fn probe_up(candidate: &HideCandidate, callback_started: u64) -> bool {
    let candidate = candidate.clone();
    bounded_probe("dock-click-up-probe", callback_started, move || {
        crate::window_collector::visible_ordinary_window_now(candidate.pid, candidate.anchor_window)
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

/// The tap callback. In active mode it always returns the original event unchanged; the only
/// difference from passive observation is that this code runs before delivery to the Dock.
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
        match event_type {
            LEFT_MOUSE_DOWN => {
                *DOWN.lock().unwrap() = None;
                if !unmodified(event) {
                    return event;
                }
                let timestamp = CGEventGetTimestamp(event);
                let Some(tile) = crate::dock_preview::observer::click_tile_evidence() else {
                    return event;
                };
                if !tile.icon.contains(point) {
                    return event;
                }
                *DOWN.lock().unwrap() =
                    probe_down(tile, point, timestamp, context.session, callback_started);
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
                let up_anchor_ok = probe_up(&down.candidate, callback_started);
                if unmodified(event) && same_tile && tile_stable && up_anchor_ok {
                    crate::dock_preview::note_dock_hide_candidate(down.candidate);
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
        bounded_probe, click_shape_is_valid, travel, MAX_CLICK_DURATION_NS, MAX_CLICK_TRAVEL,
    };
    use crate::dock_preview::layout::Rect;
    use std::time::{Duration, Instant};

    #[test]
    fn click_travel_boundary_is_conservative() {
        assert!(travel((0.0, 0.0), (3.0, 4.0)) == 5.0);
        assert!(MAX_CLICK_TRAVEL >= 5.0);
        assert!(MAX_CLICK_DURATION_NS <= 1_000_000_000);
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
