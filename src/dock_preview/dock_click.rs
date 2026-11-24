//! Reports a mouse press on the Dock, so the preview closes on the press that starts a switch.
//!
//! Windows closes the taskbar's thumbnail flyout when its button is pressed, before the window
//! comes forward; the preview over the Dock should behave the same way. The click never reaches the
//! preview panel: the panel sits above the tile without covering it, so the press belongs to the
//! Dock process. This module listens to the Dock's own mouse presses with a listen-only per-PID tap
//! (the same mechanism the reveal keeper uses) and reports them to the main thread, which owns the
//! panel.
//!
//! The tap lives for the whole enabled session rather than for one panel: a press often lands
//! inside the hover's open delay, and a tap that only existed while a panel was on screen would
//! miss exactly the click this exists for. It is listen-only, so the press still reaches the Dock
//! and the app still activates; nothing is injected and no other process sees anything.

use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, AtomicPtr, Ordering};
use std::sync::mpsc::{channel, Receiver, RecvTimeoutError, Sender, TryRecvError};
use std::time::Duration;

use crate::event_tap::{
    kCFRunLoopDefaultMode, tap_options, tap_placement, CFMachPortCreateRunLoopSource,
    CFRunLoopAddSource, CGEventGetLocation, CGEventRef, CGEventTapCreateForPid, CGEventTapEnable,
    CGEventTapProxy, CGEventType, TAP_DISABLED_BY_TIMEOUT,
};
use crate::ffi::CFRunLoopRunInMode;
use crate::{log_debug, log_info};

/// kCGEventLeftMouseDown.
const LEFT_MOUSE_DOWN: CGEventType = 1;
/// kCGEventRightMouseDown: the Dock menu replaces the flyout the way a Windows jump list does, so a
/// right press dismisses the preview too.
const RIGHT_MOUSE_DOWN: CGEventType = 3;
/// How long the watcher thread waits for a command while no tap is installed. Blocking is safe
/// there: there are no events to miss.
const IDLE_POLL: Duration = Duration::from_millis(100);
/// How long one run-loop slice may last while the tap is installed. The slice must never park the
/// run loop on the command channel: a press that arrives while the thread is blocked in `recv` is
/// only delivered once the run loop is served again, which would add that wait to every dismissal.
/// The slice is also the worst-case delay for a start/stop command, which only changes where the
/// tap points, never whether a press is seen.
const COMMAND_POLL: Duration = Duration::from_secs(1);
/// `CFRunLoopRunResult` values the watcher has to act on: the run loop was stopped, or it has nothing
/// left to serve (the tap's Mach port died with the Dock). Either way this install can never report
/// another press, and re-entering an empty run loop returns immediately, so continuing would spin.
const RUN_FINISHED: i32 = 1;
const RUN_STOPPED: i32 = 2;

/// The tap's callback state.
struct Context {
    /// The feature session this install belongs to.
    session: u64,
    /// The tap handle the callback needs to re-enable itself after a timeout disable.
    tap: AtomicPtr<c_void>,
    /// Set immediately before this module disables its own tap: the disable pseudo-event that
    /// `CGEventTapEnable(tap, false)` delivers must not be read as the system's terminal disable.
    stopping: AtomicBool,
}

/// Command channel shared with the watcher thread.
enum Command {
    /// Install (or move) the tap for `pid`, reporting presses as belonging to `session`.
    Start {
        pid: i32,
        session: u64,
    },
    Stop,
}

/// Owner handle for the watcher thread.
pub(crate) struct Handle {
    commands: Sender<Command>,
}

impl Handle {
    /// Point the tap at the Dock process. Starting again with the same pid keeps the installed tap;
    /// a new pid (the Dock restarted) moves the tap there. `session` is the feature session that
    /// owns this install: a press reported by a tap from an older session must be rejected, and the
    /// tap can outlive its session by the command latency, so the session travels with the install
    /// rather than being read when a press happens to arrive.
    pub(crate) fn start(&self, pid: i32, session: u64) {
        let _ = self.commands.send(Command::Start { pid, session });
    }

    /// Release the tap. The thread parks until the next `start`.
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
    /// The Dock moved to another process: restart the tap there for the same session.
    Restart(i32, u64),
}

fn run(receiver: Receiver<Command>) {
    let mut pending: Option<(i32, u64)> = None;
    loop {
        let (pid, session) = match pending.take() {
            Some(next) => next,
            None => match receiver.recv_timeout(IDLE_POLL) {
                Ok(Command::Start { pid, session }) => {
                    if !crate::input_monitor::taps_allowed() {
                        log_debug!("[dock-preview] Dock click watcher skipped: taps unavailable");
                        continue;
                    }
                    (pid, session)
                }
                Ok(Command::Stop) => continue,
                Err(RecvTimeoutError::Timeout) => continue,
                Err(RecvTimeoutError::Disconnected) => return,
            },
        };
        match run_active(&receiver, pid, session) {
            ActiveOutcome::Stopped => {}
            ActiveOutcome::Disconnected => return,
            ActiveOutcome::Restart(pid, session) => pending = Some((pid, session)),
        }
    }
}

/// Install the per-PID tap, serve the run loop, and tear down on `Stop`.
fn run_active(receiver: &Receiver<Command>, pid: i32, session: u64) -> ActiveOutcome {
    // A terminal disable or a revoked permission may have happened since this thread last asked:
    // re-check before every install, including a rebuild after the Dock restarted.
    if !crate::input_monitor::taps_allowed() {
        log_debug!("[dock-preview] Dock click tap skipped: taps unavailable");
        return ActiveOutcome::Stopped;
    }
    let context = Box::into_raw(Box::new(Context {
        session,
        tap: AtomicPtr::new(std::ptr::null_mut()),
        stopping: AtomicBool::new(false),
    }));
    let mask: u64 = (1u64 << LEFT_MOUSE_DOWN) | (1u64 << RIGHT_MOUSE_DOWN);
    let tap = unsafe {
        CGEventTapCreateForPid(
            pid,
            tap_placement::TAIL_INSERT,
            tap_options::LISTEN_ONLY,
            mask,
            Some(press_callback),
            context as *mut c_void,
        )
    };
    if tap.is_null() {
        log_info!("[dock-preview] Dock click tap could not be created");
        unsafe { drop(Box::from_raw(context)) };
        return ActiveOutcome::Stopped;
    }
    let source = unsafe { CFMachPortCreateRunLoopSource(std::ptr::null_mut(), tap, 0) };
    if source.is_null() {
        // CFRelease on a null source would crash, so this branch releases only what was created.
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
    // Info, not debug: whether this tap could be installed is the first thing a live check needs,
    // and it happens once per enabled session (or Dock restart), not per press.
    log_info!("[dock-preview] Dock click watcher active pid={pid} session={session}");

    // Release the tap exactly once, from whichever branch ends this install.
    let release = || unsafe {
        (*context).stopping.store(true, Ordering::SeqCst);
        crate::event_tap::teardown_event_tap(
            run_loop,
            crate::event_tap::CreatedEventTap { tap, source },
        );
        drop(Box::from_raw(context));
    };

    loop {
        let result = unsafe {
            // Serve the tap as soon as its port is ready. `returnAfterSourceHandled` hands control
            // back after one batch, so a press is reported on this iteration, and the command
            // channel is only polled in between -- never blocked on while presses can arrive.
            CFRunLoopRunInMode(kCFRunLoopDefaultMode, COMMAND_POLL.as_secs_f64(), 1)
        };
        if result == RUN_FINISHED || result == RUN_STOPPED {
            // The port died (the Dock restarted) or the loop was stopped: release and park. The
            // next `Start` installs for the new Dock.
            release();
            log_info!("[dock-preview] Dock click tap lost its run loop; watcher parked");
            return ActiveOutcome::Stopped;
        }
        if !crate::input_monitor::taps_allowed() {
            // A terminal disable or a revoked permission: this tap must not act again, and nothing
            // will send it a command.
            release();
            log_info!("[dock-preview] Dock click tap released: input taps no longer allowed");
            return ActiveOutcome::Stopped;
        }
        // Apply every queued command in one loop: the hover path sends a `Start` each time it
        // schedules an open, so a backlog of duplicate starts must not delay a stop or a pid change
        // behind them, and no command may be read and dropped between two calls.
        loop {
            match receiver.try_recv() {
                Ok(Command::Start {
                    pid: new_pid,
                    session: new_session,
                }) if new_pid != pid || new_session != session => {
                    release();
                    log_debug!(
                        "[dock-preview] Dock click watcher moving to pid={new_pid} session={new_session}"
                    );
                    return ActiveOutcome::Restart(new_pid, new_session);
                }
                Ok(Command::Stop) => {
                    release();
                    log_debug!("[dock-preview] Dock click watcher released");
                    return ActiveOutcome::Stopped;
                }
                // A `Start` for the pid and session already installed keeps the tap as it is.
                Ok(Command::Start { .. }) => {}
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    // Process is going away: remove the tap without a farewell event.
                    release();
                    return ActiveOutcome::Disconnected;
                }
            }
        }
    }
}

/// The tap callback. Only mouse presses and the tap's own disable pseudo-events arrive; it runs on
/// the watcher thread, so it does no work beyond reporting the press to the main thread.
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
        // The shared policy separates this module's own stop from the system's terminal disable. A
        // timeout is recoverable while taps are still allowed; a UserInput disable is terminal for
        // the process, so nothing here re-enables it.
        let stopping = context.stopping.load(Ordering::SeqCst);
        if crate::input_monitor::handle_disabled_event(event_type, "dock-preview-click", stopping) {
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
        if event_type == LEFT_MOUSE_DOWN || event_type == RIGHT_MOUSE_DOWN {
            let point = CGEventGetLocation(event);
            crate::dock_preview::note_dock_click((point.x, point.y), context.session);
        }
        event
    })
}
