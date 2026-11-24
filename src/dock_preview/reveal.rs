//! Keeps an auto-hidden Dock revealed while the preview panel is open.
//!
//! Moving the pointer from a Dock icon onto the preview cards leaves the Dock, so macOS starts
//! hiding it and the panel is left floating over an empty strip. The Dock itself decides to hide
//! from its own mouse-exited event; this module listens to that one event on the Dock process (a
//! listen-only per-PID tap) and answers it with a synthetic `mouseMoved` at the Dock's edge, which
//! the Dock reads as "the pointer is still here".
//!
//! Why an event tap instead of posting mouse moves on a timer: a steady stream of synthetic moves
//! re-runs the Dock's hover/magnification every tick and pops its icon tooltip. Answering only the
//! Dock's own exit keeps the Dock stable: it stops hiding, and nothing else receives an event. The
//! events are posted to the Dock's process only (`CGEventPostToPid`), so the real cursor never
//! moves and no other application sees anything.
//!
//! Releasing the tap posts a `mouseExited` at the same point, so once the panel closes the Dock can
//! hide again instead of staying up until the pointer happens to move.

use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, AtomicPtr, Ordering};
use std::sync::mpsc::{channel, Receiver, RecvTimeoutError, Sender};
use std::sync::Mutex;
use std::time::Duration;

use crate::event_tap::{
    kCFRunLoopDefaultMode, tap_options, tap_placement, CFMachPortCreateRunLoopSource,
    CFRunLoopAddSource, CGEventCreateMouseEvent, CGEventPostToPid, CGEventRef, CGEventSetType,
    CGEventTapCreateForPid, CGEventTapEnable, CGEventTapProxy, CGEventType, CGPoint,
    TAP_DISABLED_BY_TIMEOUT,
};
use crate::ffi::{
    make_nsstring, release_obj, CFBooleanGetTypeID, CFBooleanGetValue, CFGetTypeID,
    CFPreferencesCopyAppValue, CFRelease, CFRunLoopRunInMode,
};
use crate::{log_debug, log_info};

/// kCGEventMouseMoved.
const MOUSE_MOVED: CGEventType = 5;
/// kCGEventMouseExited (the same raw value AppKit's NSEvent.EventType.mouseExited uses).
const MOUSE_EXITED: CGEventType = 9;
/// kCGMouseButtonLeft.
const MOUSE_BUTTON_LEFT: i64 = 0;
/// How long the keeper thread waits for a command while no tap is installed. The tap itself is
/// served by the run loop; this is only the idle/parked poll.
const IDLE_POLL: Duration = Duration::from_millis(100);

/// The tap's callback state: where the Dock should think the pointer is, and the tap handle the
/// callback needs to re-enable itself after a timeout disable.
struct Context {
    pid: i32,
    point: Mutex<(f64, f64)>,
    tap: AtomicPtr<c_void>,
    /// Set immediately before this module disables its own tap: the disable pseudo-event that
    /// `CGEventTapEnable(tap, false)` delivers must not be read as the system's terminal disable.
    stopping: AtomicBool,
}

/// Command channel shared with the keeper thread.
enum Command {
    Start { pid: i32, point: (f64, f64) },
    Update { point: (f64, f64) },
    Stop { post_leave: bool },
}

/// Owner handle for the keeper thread.
pub(crate) struct Handle {
    commands: Sender<Command>,
}

impl Handle {
    /// Install (or move) the reveal point. Called once per shown panel and again when the Dock
    /// icon moves under magnification.
    pub(crate) fn start(&self, pid: i32, point: (f64, f64)) {
        let _ = self.commands.send(Command::Start { pid, point });
    }

    pub(crate) fn update(&self, point: (f64, f64)) {
        let _ = self.commands.send(Command::Update { point });
    }

    /// Release the tap. `post_leave` tells the Dock it may hide again (the pointer is not on the
    /// Dock); when the pointer is back on the Dock the real mouse keeps it visible by itself.
    pub(crate) fn stop(&self, post_leave: bool) {
        let _ = self.commands.send(Command::Stop { post_leave });
    }
}

/// Spawn the single keeper thread. It parks on its command channel until a panel asks for it.
pub(crate) fn spawn() -> Handle {
    let (commands, receiver) = channel();
    let _ = std::thread::Builder::new()
        .name("com.eacryo.oh-my-tab.dock-preview-reveal".to_string())
        .spawn(move || run(receiver));
    Handle { commands }
}

/// Whether the user has the Dock set to hide automatically. A pinned Dock needs no keeper, and
/// posting events to it would only disturb its hover state.
pub(crate) fn dock_auto_hide_enabled() -> bool {
    unsafe {
        let key = make_nsstring("autohide");
        let domain = make_nsstring("com.apple.dock");
        let value = CFPreferencesCopyAppValue(key as *const c_void, domain as *const c_void);
        release_obj(key);
        release_obj(domain);
        if value.is_null() {
            return false;
        }
        let is_boolean = CFGetTypeID(value) == CFBooleanGetTypeID();
        let enabled = is_boolean && CFBooleanGetValue(value);
        CFRelease(value);
        enabled
    }
}

enum ActiveOutcome {
    Stopped,
    Disconnected,
    /// The Dock (or the panel) moved to another process: restart the tap there.
    Restart(i32, (f64, f64)),
}

fn run(receiver: Receiver<Command>) {
    let mut pending: Option<(i32, (f64, f64))> = None;
    loop {
        let (pid, point) = match pending.take() {
            Some(next) => next,
            None => match receiver.recv_timeout(IDLE_POLL) {
                // Parked: no tap, no CPU. A new panel's `Start` wakes this up.
                Ok(Command::Start { pid, point }) => {
                    if !crate::input_monitor::taps_allowed() {
                        log_debug!(
                            "[dock-preview] Dock reveal keeper skipped: event taps unavailable"
                        );
                        continue;
                    }
                    (pid, point)
                }
                Ok(Command::Update { .. }) | Ok(Command::Stop { .. }) => continue,
                Err(RecvTimeoutError::Timeout) => continue,
                Err(RecvTimeoutError::Disconnected) => return,
            },
        };
        match run_active(&receiver, pid, point) {
            ActiveOutcome::Stopped => {}
            ActiveOutcome::Disconnected => return,
            ActiveOutcome::Restart(pid, point) => pending = Some((pid, point)),
        }
    }
}

/// Install the per-PID tap, serve the run loop, and tear down on `Stop`.
fn run_active(receiver: &Receiver<Command>, pid: i32, point: (f64, f64)) -> ActiveOutcome {
    // A terminal disable or a revoked permission may have happened since the keeper last started:
    // check before installing, including a restart after the Dock changed process.
    if !crate::input_monitor::taps_allowed() {
        log_debug!("[dock-preview] Dock reveal keeper skipped: taps unavailable");
        return ActiveOutcome::Stopped;
    }
    let context = Box::into_raw(Box::new(Context {
        pid,
        point: Mutex::new(point),
        tap: AtomicPtr::new(std::ptr::null_mut()),
        stopping: AtomicBool::new(false),
    }));
    let mask: u64 = 1 << MOUSE_EXITED;
    let tap = unsafe {
        CGEventTapCreateForPid(
            pid,
            tap_placement::TAIL_INSERT,
            tap_options::LISTEN_ONLY,
            mask,
            Some(dock_exit_callback),
            context as *mut c_void,
        )
    };
    if tap.is_null() {
        log_info!("[dock-preview] Dock reveal tap could not be created");
        unsafe { drop(Box::from_raw(context)) };
        return ActiveOutcome::Stopped;
    }
    let source = unsafe { CFMachPortCreateRunLoopSource(std::ptr::null_mut(), tap, 0) };
    if source.is_null() {
        // CFRelease on a null source would crash, so this branch releases only what was created.
        log_info!("[dock-preview] Dock reveal tap has no run-loop source");
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
    // Dock may already have started hiding while the panel opened: put the pointer back on its
    // edge once up front so the first exit is answered even if it happened before the tap existed.
    unsafe { post_mouse(pid, point, MOUSE_MOVED) };
    log_debug!(
        "[dock-preview] Dock reveal keeper active pid={pid} point=({:.0},{:.0})",
        point.0,
        point.1
    );

    loop {
        unsafe {
            CFRunLoopRunInMode(kCFRunLoopDefaultMode, 0.05, 1);
        }
        if !crate::input_monitor::taps_allowed() {
            // A terminal disable or a revoked permission: stop serving and never post again.
            unsafe {
                (*context).stopping.store(true, Ordering::SeqCst);
                crate::event_tap::teardown_event_tap(
                    run_loop,
                    crate::event_tap::CreatedEventTap { tap, source },
                );
                drop(Box::from_raw(context));
            }
            log_info!("[dock-preview] Dock reveal keeper released: input taps no longer allowed");
            return ActiveOutcome::Stopped;
        }
        match receiver.recv_timeout(IDLE_POLL) {
            Ok(Command::Start {
                pid: new_pid,
                point: new_point,
            }) if new_pid != pid => {
                unsafe {
                    (*context).stopping.store(true, Ordering::SeqCst);
                    crate::event_tap::teardown_event_tap(
                        run_loop,
                        crate::event_tap::CreatedEventTap { tap, source },
                    );
                    drop(Box::from_raw(context));
                }
                log_debug!("[dock-preview] Dock reveal keeper moving to pid={new_pid}");
                return ActiveOutcome::Restart(new_pid, new_point);
            }
            Ok(Command::Start { point, .. }) | Ok(Command::Update { point }) => {
                if let Ok(mut current) = unsafe { &*context }.point.lock() {
                    *current = point;
                }
            }
            Ok(Command::Stop { post_leave }) => {
                let last = unsafe { &*context }
                    .point
                    .lock()
                    .map(|point| *point)
                    .unwrap_or(point);
                // The farewell move exists to reveal nothing; with input taps no longer allowed it
                // must not be injected either.
                if post_leave && crate::input_monitor::taps_allowed() {
                    unsafe { post_mouse(pid, last, MOUSE_EXITED) };
                }
                unsafe {
                    (*context).stopping.store(true, Ordering::SeqCst);
                    crate::event_tap::teardown_event_tap(
                        run_loop,
                        crate::event_tap::CreatedEventTap { tap, source },
                    );
                    drop(Box::from_raw(context));
                }
                log_debug!("[dock-preview] Dock reveal keeper released");
                return ActiveOutcome::Stopped;
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => {
                // Process is going away: remove the tap without a farewell event.
                unsafe {
                    (*context).stopping.store(true, Ordering::SeqCst);
                    crate::event_tap::teardown_event_tap(
                        run_loop,
                        crate::event_tap::CreatedEventTap { tap, source },
                    );
                    drop(Box::from_raw(context));
                }
                return ActiveOutcome::Disconnected;
            }
        }
    }
}

/// The tap callback. Only mouse-exited events and the tap's own disable pseudo-events arrive.
unsafe extern "C" fn dock_exit_callback(
    _proxy: CGEventTapProxy,
    event_type: CGEventType,
    event: CGEventRef,
    user_info: *mut c_void,
) -> CGEventRef {
    crate::callback_guard::value("dock_preview_reveal_tap", event, || {
        if user_info.is_null() {
            return event;
        }
        let context = &*(user_info as *const Context);
        // The shared policy separates this module's own stop from the system's terminal disable. A
        // timeout is recoverable while taps are still allowed; a UserInput disable is terminal for
        // the process, so nothing here re-enables it.
        let stopping = context.stopping.load(Ordering::SeqCst);
        if crate::input_monitor::handle_disabled_event(event_type, "dock-preview-reveal", stopping)
        {
            if event_type == TAP_DISABLED_BY_TIMEOUT && crate::input_monitor::taps_allowed() {
                let tap = context.tap.load(Ordering::SeqCst);
                if !tap.is_null() {
                    CGEventTapEnable(tap, true);
                }
            }
            return event;
        }
        // A global terminal state (another tap's UserInput disable, or a revoked permission) stops
        // this tap from acting even if its own disable event has not arrived yet.
        if !crate::input_monitor::taps_allowed() {
            return event;
        }
        if event_type == MOUSE_EXITED {
            let point = *context.point.lock().unwrap();
            post_mouse(context.pid, point, MOUSE_MOVED);
        }
        event
    })
}

/// Post a mouse event to one process only. Never moves the real cursor.
unsafe fn post_mouse(pid: i32, point: (f64, f64), event_type: CGEventType) {
    let event = CGEventCreateMouseEvent(
        std::ptr::null(),
        MOUSE_MOVED,
        CGPoint {
            x: point.0,
            y: point.1,
        },
        MOUSE_BUTTON_LEFT,
    );
    if event.is_null() {
        return;
    }
    if event_type != MOUSE_MOVED {
        CGEventSetType(event, event_type);
    }
    CGEventPostToPid(pid, event);
    CFRelease(event);
}

/// The point on the Dock's screen edge the Dock should keep seeing the pointer at.
pub(crate) fn reveal_point(
    edge: crate::dock_preview::layout::DockEdge,
    anchor: crate::dock_preview::layout::Rect,
    screen: crate::dock_preview::layout::Rect,
) -> (f64, f64) {
    let center_x = anchor.x + anchor.w / 2.0;
    let center_y = anchor.y + anchor.h / 2.0;
    match edge {
        crate::dock_preview::layout::DockEdge::Bottom => (center_x, screen.max_y() - 1.0),
        crate::dock_preview::layout::DockEdge::Top => (center_x, screen.y + 1.0),
        crate::dock_preview::layout::DockEdge::Left => (screen.x + 1.0, center_y),
        crate::dock_preview::layout::DockEdge::Right => (screen.max_x() - 1.0, center_y),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The points the keeper posts must stay on the Dock's edge: a point inside the screen would
    /// make the Dock treat it as a normal hover and start magnifying a tile.
    fn reveal_point_sits_on_the_edge(edge: crate::dock_preview::layout::DockEdge) -> bool {
        let screen = crate::dock_preview::layout::Rect::new(0.0, 0.0, 1470.0, 956.0);
        let anchor = crate::dock_preview::layout::Rect::new(268.0, 894.0, 44.0, 56.0);
        let point = reveal_point(edge, anchor, screen);
        match edge {
            crate::dock_preview::layout::DockEdge::Bottom => point.1 >= screen.max_y() - 1.5,
            crate::dock_preview::layout::DockEdge::Top => point.1 <= screen.y + 1.5,
            crate::dock_preview::layout::DockEdge::Left => point.0 <= screen.x + 1.5,
            crate::dock_preview::layout::DockEdge::Right => point.0 >= screen.max_x() - 1.5,
        }
    }

    #[test]
    fn the_reveal_point_hugs_the_dock_edge() {
        use crate::dock_preview::layout::DockEdge;
        for edge in [
            DockEdge::Bottom,
            DockEdge::Top,
            DockEdge::Left,
            DockEdge::Right,
        ] {
            assert!(reveal_point_sits_on_the_edge(edge));
        }
    }
}
