//! Mouse event tap.
//! Spawns a default-tap CGEventTap (HID level) on a dedicated thread that listens for mouse
//! button and scroll events. Scroll events are processed in one of two modes: Default
//! (passthrough + optional reverse) or Line (fixed line count). Both use the synthetic-event
//! approach: drop the original, post a new event to the session level, bypassing the system
//! natural-scroll override at the HID layer.

use crate::event_tap::{
    self, tap_location, tap_options, tap_placement, CFRunLoopGetCurrent,
    CGEventCreateScrollWheelEvent2, CGEventFlags, CGEventGetFlags, CGEventGetIntegerValueField,
    CGEventMask, CGEventPost, CGEventRef, CGEventSetFlags, CGEventSetIntegerValueField,
    CGEventTapProxy, CGEventType, K_CG_EVENT_SOURCE_USER_DATA, K_CG_SCROLL_EVENT_UNIT_LINE,
    K_CG_SCROLL_WHEEL_EVENT_DELTA_AXIS_1, K_CG_SCROLL_WHEEL_EVENT_DELTA_AXIS_2,
    K_CG_SCROLL_WHEEL_EVENT_IS_CONTINUOUS, K_CG_SESSION_EVENT_TAP, SYNTHETIC_MARKER,
};
use crate::mouse::device;
use crate::mouse::keysim;
use crate::mouse::resolve;
use crate::mouse::scrolling::compute_delta;
use crate::{log_debug, log_info};
use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, SyncSender, TrySendError};
use std::sync::{Mutex, OnceLock};
use std::thread;

// See CGEventType.h.
const K_CG_EVENT_LEFT_MOUSE_DOWN: CGEventType = 1;
const K_CG_EVENT_LEFT_MOUSE_UP: CGEventType = 2;
const K_CG_EVENT_RIGHT_MOUSE_DOWN: CGEventType = 3;
const K_CG_EVENT_RIGHT_MOUSE_UP: CGEventType = 4;
const K_CG_EVENT_OTHER_MOUSE_DOWN: CGEventType = 25;
const K_CG_EVENT_OTHER_MOUSE_UP: CGEventType = 26;
const K_CG_EVENT_SCROLL_WHEEL: CGEventType = 22;

// mouseEventButtonNumber field (field 3, NOT 0 -- 0 is kCGMouseEventNumber, an ever-increasing
// event counter, so reading it yields the counter instead of the real button number, breaking
// recording and matching).
const K_CG_MOUSE_EVENT_BUTTON_NUMBER: i32 = 3;

/// Recording-in-progress flag: set while the settings UI records a button/combo; the tap
/// skips mapping execution so recording never fires a binding (same as LinearMouse's
/// SettingsState.shared.recording).
pub(crate) static RECORDING: AtomicBool = AtomicBool::new(false);

#[derive(Clone, Copy)]
struct ScrollPost {
    dy: i32,
    dx: i32,
    flags: CGEventFlags,
}

static SCROLL_POSTER: OnceLock<Option<SyncSender<ScrollPost>>> = OnceLock::new();
static SCROLL_QUEUE_FULL_LOGGED: AtomicBool = AtomicBool::new(false);

fn scroll_post_sender() -> Option<&'static SyncSender<ScrollPost>> {
    SCROLL_POSTER
        .get_or_init(|| {
            let (sender, receiver) = mpsc::sync_channel::<ScrollPost>(128);
            std::thread::Builder::new()
                .name("mouse-scroll-event-poster".into())
                .spawn(move || {
                    while let Ok(post) = receiver.recv() {
                        unsafe { post_scroll_event(post.dy, post.dx, post.flags) };
                    }
                })
                .ok()
                .map(|_| sender)
        })
        .as_ref()
}

fn ensure_scroll_poster() -> bool {
    scroll_post_sender().is_some()
}

/// Queue scroll synthesis off the HID event callback. Queue saturation is fail-open: the
/// original hardware event passes through instead of being swallowed without a replacement.
fn queue_scroll_post(dy: i32, dx: i32, flags: CGEventFlags) -> bool {
    let Some(sender) = scroll_post_sender() else {
        return false;
    };
    match sender.try_send(ScrollPost { dy, dx, flags }) {
        Ok(()) => true,
        Err(TrySendError::Full(_)) => {
            if !SCROLL_QUEUE_FULL_LOGGED.swap(true, Ordering::Relaxed) {
                log_info!("[mouse] scroll post queue is full; passing original events through.");
            }
            false
        }
        Err(TrySendError::Disconnected(_)) => false,
    }
}

/// Synthesize a scroll event and post it to the session level.
/// Line mode posts in "line" units; Default mode passes the raw delta through.
unsafe fn post_scroll_event(dy: i32, dx: i32, flags: CGEventFlags) {
    let synthetic =
        CGEventCreateScrollWheelEvent2(std::ptr::null(), K_CG_SCROLL_EVENT_UNIT_LINE, 2, dy, dx, 0);

    if synthetic.is_null() {
        log_info!("[mouse] failed to synthesize scroll event");
        return;
    }

    CGEventSetFlags(synthetic, flags);
    CGEventSetIntegerValueField(synthetic, K_CG_EVENT_SOURCE_USER_DATA, SYNTHETIC_MARKER);

    CGEventPost(K_CG_SESSION_EVENT_TAP, synthetic);
    crate::ffi::CFRelease(synthetic as *const c_void);
}

unsafe extern "C" fn mouse_event_tap_callback(
    proxy: CGEventTapProxy,
    event_type: CGEventType,
    event: CGEventRef,
    user_info: *mut c_void,
) -> CGEventRef {
    crate::callback_guard::event("mouse_event_tap_callback", event, || unsafe {
        mouse_event_tap_callback_inner(proxy, event_type, event, user_info)
    })
}

unsafe fn mouse_event_tap_callback_inner(
    _proxy: CGEventTapProxy,
    event_type: CGEventType,
    event: CGEventRef,
    _user_info: *mut c_void,
) -> CGEventRef {
    if crate::input_monitor::handle_disabled_event(event_type, "mouse") {
        return event;
    }
    if STOP_REQUESTED.load(Ordering::SeqCst) || !crate::input_monitor::taps_allowed() {
        return event;
    }
    let flags: CGEventFlags = CGEventGetFlags(event);

    if event_type == K_CG_EVENT_SCROLL_WHEEL {
        // Skip our own synthetic scroll events (defensive; session-posted events shouldn't reach HID tap).
        let user_data = CGEventGetIntegerValueField(event, K_CG_EVENT_SOURCE_USER_DATA);
        if user_data == SYNTHETIC_MARKER {
            return event;
        }

        // continuous=1 means a continuous (pixel-level) scroll event from a trackpad / Magic Mouse.
        // Both modes handle only discrete mouse wheel events; trackpad is skipped.
        let continuous = CGEventGetIntegerValueField(event, K_CG_SCROLL_WHEEL_EVENT_IS_CONTINUOUS);
        if continuous != 0 {
            return event;
        }

        // deltaAxis1 = vertical delta, deltaAxis2 = horizontal delta.
        let dy = CGEventGetIntegerValueField(event, K_CG_SCROLL_WHEEL_EVENT_DELTA_AXIS_1);
        let dx = CGEventGetIntegerValueField(event, K_CG_SCROLL_WHEEL_EVENT_DELTA_AXIS_2);

        // Attribution: CGEvent -> producing device -> (VID, PID). None on failure (falls back to
        // the "All Mice" profile).
        let dev_key = device::device_from_cgevent(event);
        // Resolve the effective config for this device (merging "All Mice" + per-device profiles).
        let resolved = resolve::resolve(dev_key);

        // Default / Line: compute delta (passthrough or line-count normalization + reverse) ->
        // post synthetic event -> drop the original.
        let (ndy, ndx) = compute_delta(dy, dx, &resolved);
        if queue_scroll_post(ndy, ndx, flags) {
            std::ptr::null_mut()
        } else {
            event
        }
    } else {
        let button = CGEventGetIntegerValueField(event, K_CG_MOUSE_EVENT_BUTTON_NUMBER);
        // Button mappings: only middle/side buttons (>= 2) take part; left (0)/right (1)
        // are never bound so the user can't lock themselves out of clicking. Skipped while
        // recording.
        if button >= 2 && !RECORDING.load(Ordering::Relaxed) {
            let dev_key = device::device_from_cgevent(event);
            let resolved = resolve::resolve(dev_key);
            // The device profile's mappings master switch: when off, behave as unbound
            // (events pass through).
            if !resolved.button_mappings_enabled {
                return event;
            }
            if let Some(desc) = resolved.button_mappings.get(&button.to_string()) {
                // Dispatch by binding type: shortcut -> synthesized keys; system action ->
                // Dock private notification; none -> swallow without action. Both directions
                // swallow the original event (the app never sees the raw side-button click).
                match crate::mouse::shortcut::parse_binding(desc) {
                    Ok(crate::mouse::shortcut::Binding::Key(sc)) => match event_type {
                        K_CG_EVENT_OTHER_MOUSE_DOWN => {
                            if keysim::queue_button_mapping(
                                button as u32,
                                true,
                                sc.keycode,
                                sc.flags,
                                desc,
                            ) {
                                if STOP_REQUESTED.load(Ordering::SeqCst) {
                                    keysim::release_all_queued();
                                }
                                return std::ptr::null_mut();
                            }
                            return event;
                        }
                        K_CG_EVENT_OTHER_MOUSE_UP => {
                            if keysim::queue_button_mapping(
                                button as u32,
                                false,
                                sc.keycode,
                                sc.flags,
                                desc,
                            ) {
                                if STOP_REQUESTED.load(Ordering::SeqCst) {
                                    keysim::release_all_queued();
                                }
                                return std::ptr::null_mut();
                            }
                            return event;
                        }
                        _ => {}
                    },
                    Ok(crate::mouse::shortcut::Binding::System(notif)) => {
                        // System actions fire once on press (Dock notifications toggle);
                        // the release is only swallowed.
                        if keysim::queue_system_action(
                            button as u32,
                            event_type == K_CG_EVENT_OTHER_MOUSE_DOWN,
                            notif,
                        ) {
                            if STOP_REQUESTED.load(Ordering::SeqCst) {
                                keysim::clear_system_button_states();
                            }
                            return std::ptr::null_mut();
                        }
                        return event;
                    }
                    Ok(crate::mouse::shortcut::Binding::Switcher) => {
                        // Open the switcher (two-phase): press opens the overlay, release
                        // commits -- same semantics as holding Cmd+Tab (selection while
                        // held, commit on release).
                        if event_type == K_CG_EVENT_OTHER_MOUSE_DOWN {
                            log_debug!(
                                "[mouse] switcher source event: button={} phase=down",
                                button
                            );
                            crate::enqueue_global_event(
                                crate::event_monitor::GlobalEvent::CmdTabPressed,
                            );
                        } else if event_type == K_CG_EVENT_OTHER_MOUSE_UP {
                            log_debug!("[mouse] switcher source event: button={} phase=up", button);
                            crate::enqueue_global_event(
                                crate::event_monitor::GlobalEvent::CmdReleased,
                            );
                        }
                        return std::ptr::null_mut();
                    }
                    Ok(crate::mouse::shortcut::Binding::None) => {
                        // Explicit none: swallow, no action.
                        return std::ptr::null_mut();
                    }
                    Err(_) => {
                        // Mapping exists but failed to parse (hand-edited config): note it.
                        log_info!("[mouse] button {}: unparseable binding {:?}", button, desc);
                    }
                }
            }
        }
        event
    }
}

/// The mouse thread's active tap and RunLoop, allowing stop() to disable the tap synchronously
/// before stopping its RunLoop. Stored before CFRunLoopRun and cleared before the thread exits.
/// Wrapped Mutex with Send+Sync (same pattern as device.rs's ManagerMutex; statics need it).
#[derive(Default)]
struct TapControl {
    tap: Option<event_tap::CFMachPortRef>,
    run_loop: Option<event_tap::CFRunLoopRef>,
}

struct TapControlMutex(Mutex<TapControl>);
unsafe impl Send for TapControlMutex {}
unsafe impl Sync for TapControlMutex {}

static TAP_CONTROL: OnceLock<TapControlMutex> = OnceLock::new();

fn tap_control() -> &'static Mutex<TapControl> {
    &TAP_CONTROL
        .get_or_init(|| TapControlMutex(Mutex::new(TapControl::default())))
        .0
}

/// Runtime stop request flag: once set by stop(), the retry loop (blocked in thread::sleep)
/// bails out on wake-up, so stop()'s join() never blocks the caller during the missing-
/// permission retry window.
static STOP_REQUESTED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

pub(crate) fn mouse_tap_stopping() -> bool {
    STOP_REQUESTED.load(std::sync::atomic::Ordering::SeqCst)
}

/// Stop the mouse event thread at runtime: set the cancel flag + CFRunLoopStop, the thread
/// exits naturally. Idempotent: no-op when not running. Called by settings.rs when the
/// "Enable mouse control" switch is turned off.
pub(crate) fn stop() {
    // Set the flag first, then stop the RunLoop: during the retry window the thread sees the
    // flag on wake-up (no RunLoop involved yet).
    STOP_REQUESTED.store(true, std::sync::atomic::Ordering::SeqCst);
    let control = tap_control().lock().unwrap();
    unsafe {
        // Disable the HID tap synchronously first, ensuring the next physical click cannot enter
        // a RunLoop that is already stopping.
        if let Some(tap) = control.tap {
            event_tap::CGEventTapEnable(tap, false);
        }
        if let Some(rl) = control.run_loop {
            event_tap::CFRunLoopStop(rl);
        }
    }
}

/// Start the mouse event listener thread. Called by main.rs / settings.rs when mouse control
/// is enabled.
pub(crate) fn start() -> thread::JoinHandle<()> {
    // Start the post workers before installing the HID callback, so its first event never
    // needs to create a thread.
    let scroll_poster_ready = ensure_scroll_poster();
    let key_poster_ready = keysim::ensure_key_poster();
    if !scroll_poster_ready || !key_poster_ready {
        log_info!("[mouse] failed to start one or more asynchronous event-post workers.");
    }

    // Listen mask: left/right/other button down/up + scroll wheel. Excludes mouseMoved.
    let mask: CGEventMask = (1u64 << K_CG_EVENT_LEFT_MOUSE_DOWN)
        | (1u64 << K_CG_EVENT_LEFT_MOUSE_UP)
        | (1u64 << K_CG_EVENT_RIGHT_MOUSE_DOWN)
        | (1u64 << K_CG_EVENT_RIGHT_MOUSE_UP)
        | (1u64 << K_CG_EVENT_OTHER_MOUSE_DOWN)
        | (1u64 << K_CG_EVENT_OTHER_MOUSE_UP)
        | (1u64 << K_CG_EVENT_SCROLL_WHEEL);

    // Clear the stale stop flag before spawning; a subsequent stop() can no longer be overwritten
    // by the new thread starting late.
    STOP_REQUESTED.store(false, std::sync::atomic::Ordering::SeqCst);

    thread::spawn(move || unsafe {
        crate::performance::set_current_thread_qos(crate::performance::ThreadQos::UserInteractive);
        // Enumerate connected devices once at startup (also lazily re-done on attribution failure).
        device::ensure_enumerated();
        // Create event tap (HID level, mutable). Pass the cancel flag: bails out early on a
        // stop request (even during the missing-permission retry window).
        let created = event_tap::create_tap_with_retry(
            tap_location::HID_EVENT_TAP,
            tap_placement::HEAD_INSERT,
            tap_options::DEFAULT_TAP,
            mask,
            Some(mouse_event_tap_callback),
            std::ptr::null_mut(),
            "mouse",
            Some(&STOP_REQUESTED),
        );

        let created = match created {
            Some(created) => created,
            None => return,
        };

        let rl = CFRunLoopGetCurrent();

        // Device plug/unplug monitor: event-driven registry rebuild on Bluetooth disconnect/
        // reconnect (avoids the stale-client attribution failure that breaks scroll direction
        // / profile matching). Callbacks run on this same thread, safe.
        device::start_plug_monitor(rl);

        // Store the RunLoop for stop(); re-check the stop flag after storing to close the race
        // where the flag is set between the store and the run (stop() either reads Some and
        // CFRunLoopStop works, or reads None and the thread's check catches it).
        {
            let mut control = tap_control().lock().unwrap();
            control.tap = Some(created.tap);
            control.run_loop = Some(rl);
        }
        let watchdog = event_tap::start_tap_watchdog(created.tap, &STOP_REQUESTED);
        if !STOP_REQUESTED.load(std::sync::atomic::Ordering::SeqCst)
            && crate::input_monitor::taps_allowed()
        {
            log_debug!("Mouse event tap started.");
            // Block on the RunLoop until stop() fires CFRunLoopStop or the thread is killed.
            event_tap::CFRunLoopRun();
        }
        event_tap::stop_tap_watchdog(watchdog);
        // Once the RunLoop returns, disable the tap before removing it from shared state and
        // releasing its Core Foundation objects.
        event_tap::CGEventTapEnable(created.tap, false);
        {
            let mut control = tap_control().lock().unwrap();
            if control.tap == Some(created.tap) {
                control.tap = None;
                control.run_loop = None;
            }
        }
        event_tap::teardown_event_tap(rl, created);
    })
}
