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
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::mpsc::{self, SyncSender, TrySendError};
use std::sync::{Mutex, OnceLock};
use std::thread;

// See CGEventType.h.
const K_CG_EVENT_LEFT_MOUSE_DOWN: CGEventType = 1;
const K_CG_EVENT_LEFT_MOUSE_UP: CGEventType = 2;
const K_CG_EVENT_RIGHT_MOUSE_DOWN: CGEventType = 3;
const K_CG_EVENT_RIGHT_MOUSE_UP: CGEventType = 4;
const K_CG_EVENT_RIGHT_MOUSE_DRAGGED: CGEventType = 7;
const K_CG_EVENT_OTHER_MOUSE_DOWN: CGEventType = 25;
const K_CG_EVENT_OTHER_MOUSE_UP: CGEventType = 26;
const K_CG_EVENT_OTHER_MOUSE_DRAGGED: CGEventType = 27;
const K_CG_EVENT_SCROLL_WHEEL: CGEventType = 22;
const K_CG_EVENT_KEY_DOWN: CGEventType = 10;
const K_CG_EVENT_KEY_UP: CGEventType = 11;
const K_CG_EVENT_FLAGS_CHANGED: CGEventType = 12;

/// The button that opened the switcher and is therefore being HELD while it is on screen.
///
/// `u32::MAX` means "no side-button summon in progress". Only this button's drags belong to the
/// switcher's hover selection; a right-button stroke drawn while the switcher happens to be up still
/// belongs to the gesture module (the two are different physical buttons).
static SWITCHER_HELD_BUTTON: AtomicU32 = AtomicU32::new(u32::MAX);

/// Escape's virtual keycode.
pub(crate) const VK_ESCAPE: u16 = 53;
/// kCGKeyboardEventKeycode.
const K_CG_KEYBOARD_EVENT_KEYCODE: i32 = 9;
/// kCGKeyboardEventAutorepeat: non-zero when the key press is a repeat of a held key.
const K_CG_KEYBOARD_EVENT_AUTOREPEAT: i32 = 8;

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
    user_info: *mut c_void,
) -> CGEventRef {
    if crate::input_monitor::handle_disabled_event(event_type, "mouse", mouse_tap_stopping()) {
        // The tap was disabled under us. Whatever gesture was in flight lost its event stream: its
        // release may never arrive, so invalidate rather than wait, and never replay a click whose
        // press the application may or may not have seen.
        if event_type == event_tap::TAP_DISABLED_BY_TIMEOUT
            || event_type == event_tap::TAP_DISABLED_BY_USER_INPUT
        {
            let ending = crate::mouse::gesture::runtime::with(|rt| {
                let session = rt.session_id();
                rt.invalidate();
                session
            });
            crate::mouse::gesture::hud::hide(ending);
        }
        return event;
    }
    if mouse_tap_stopping() || !crate::input_monitor::taps_allowed() {
        return event;
    }
    let flags: CGEventFlags = CGEventGetFlags(event);

    // Gestures first: the module owns its buttons end to end, so a press it took must never fall
    // through to the button-mapping branch below (that branch would send a shortcut press at down
    // and then never see the release, leaving the key stuck down).
    if let Some(handled) = gesture_dispatch(event_type, event, flags) {
        return handled;
    }

    // Modifier arbitration for everything else. A physical key event is rewritten so the
    // application never sees a modifier released while a synthetic holder still needs it; with no
    // synthetic holder the rewrite is the identity, which is why ordinary typing is unaffected.
    // Maintained whenever the arbitration is in use, which is whenever button mappings can run --
    // NOT gated on the gesture switch. A physical state that stopped updating when gestures were
    // turned off would let a button mapping synthesise a modifier the user released minutes ago.
    if matches!(
        event_type,
        K_CG_EVENT_KEY_DOWN | K_CG_EVENT_KEY_UP | K_CG_EVENT_FLAGS_CHANGED
    ) {
        let raw = flags as u32;
        crate::mouse::gesture::runtime::observe_physical(raw);
        let merged = crate::mouse::gesture::runtime::forward_flags(raw);
        if merged != raw {
            CGEventSetFlags(event, merged as CGEventFlags);
        }
    }

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

        // Smooth mode consumes discrete wheel lines and emits its own timed pixel stream. It
        // takes precedence over Line mode; the configured reverse direction is applied before
        // feeding the pure engine.
        if resolved.scroll_mode == crate::mouse::scrolling::ScrollMode::Smooth {
            if !user_info.is_null() {
                let sign = if resolved.reverse_scroll { -1.0 } else { 1.0 };
                let fed = (&mut *(user_info
                    as *mut crate::mouse::smooth::transformer::SmoothTransformer))
                    .feed(
                        resolved.smooth_scrolling,
                        dx as f64 * sign,
                        dy as f64 * sign,
                        flags,
                    );
                if fed {
                    return std::ptr::null_mut();
                }
            }
            return event;
        }

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
                            // Remember WHICH button is being held: its drags feed the switcher's
                            // hover selection, and no other button's drags do.
                            SWITCHER_HELD_BUTTON.store(button as u32, Ordering::SeqCst);
                            crate::enqueue_global_event(
                                crate::event_monitor::GlobalEvent::CmdTabPressed,
                            );
                        } else if event_type == K_CG_EVENT_OTHER_MOUSE_UP {
                            log_debug!("[mouse] switcher source event: button={} phase=up", button);
                            SWITCHER_HELD_BUTTON.store(u32::MAX, Ordering::SeqCst);
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

/// Whether the tap is installed and still feeding events.
///
/// A recording and a gesture only exist while it does: its lifecycle stops a recording (see
/// [[crate::mouse::stop]]), so the settings window has to be able to say "not ready" instead of
/// arming a capture that nothing will ever feed. Reads the same state [[stop]] acts on, so it
/// answers for the installed port rather than for a thread handle someone still holds.
pub(crate) fn tap_running() -> bool {
    if mouse_tap_stopping() {
        return false;
    }
    let control = tap_control().lock().unwrap();
    match control.tap {
        Some(tap) => unsafe { event_tap::CGEventTapIsEnabled(tap) },
        None => false,
    }
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

    // Listen mask: left/right/other button down/up, the right/other drags a gesture trail is drawn
    // from, scroll wheel, and the keyboard events the gesture module needs (Esc, and the modifier
    // flags it rewrites so a physical release cannot lift a modifier a synthetic holder still needs).
    let mask: CGEventMask = (1u64 << K_CG_EVENT_LEFT_MOUSE_DOWN)
        | (1u64 << K_CG_EVENT_LEFT_MOUSE_UP)
        | (1u64 << K_CG_EVENT_RIGHT_MOUSE_DOWN)
        | (1u64 << K_CG_EVENT_RIGHT_MOUSE_UP)
        | (1u64 << K_CG_EVENT_RIGHT_MOUSE_DRAGGED)
        | (1u64 << K_CG_EVENT_OTHER_MOUSE_DOWN)
        | (1u64 << K_CG_EVENT_OTHER_MOUSE_UP)
        | (1u64 << K_CG_EVENT_OTHER_MOUSE_DRAGGED)
        | (1u64 << K_CG_EVENT_SCROLL_WHEEL)
        | (1u64 << K_CG_EVENT_KEY_DOWN)
        | (1u64 << K_CG_EVENT_KEY_UP)
        | (1u64 << K_CG_EVENT_FLAGS_CHANGED);

    // Clear the stale stop flag before spawning; a subsequent stop() can no longer be overwritten
    // by the new thread starting late.
    STOP_REQUESTED.store(false, std::sync::atomic::Ordering::SeqCst);

    thread::spawn(move || unsafe {
        crate::performance::set_current_thread_qos(crate::performance::ThreadQos::UserInteractive);
        let mut smooth_transformer =
            Box::new(crate::mouse::smooth::transformer::SmoothTransformer::new());
        let smooth_transformer_ptr = (&mut *smooth_transformer
            as *mut crate::mouse::smooth::transformer::SmoothTransformer)
            as *mut c_void;
        // Enumerate connected devices once at startup (also lazily re-done on attribution failure).
        device::ensure_enumerated();
        // Modifier baseline: whatever the user is holding now is the starting physical state. Only
        // the seed uses it; every later event carries its own flags.
        crate::mouse::gesture::runtime::seed_modifiers(event_tap::CGEventSourceFlagsState(0) as u32);
        // Create event tap (HID level, mutable). Pass the cancel flag: bails out early on a
        // stop request (even during the missing-permission retry window).
        let created = event_tap::create_tap_with_retry(
            tap_location::HID_EVENT_TAP,
            tap_placement::HEAD_INSERT,
            tap_options::DEFAULT_TAP,
            mask,
            Some(mouse_event_tap_callback),
            smooth_transformer_ptr,
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
        smooth_transformer.shutdown();
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

/// Whether a drag of `button` belongs to the switcher's hover selection rather than to a gesture.
///
/// Only the button that opened the switcher qualifies. The distinction is the whole point: a
/// side-button summon leaves that button held, so its drags must select cards, while a
/// right-button stroke drawn at the same moment is a gesture in progress and has to reach the
/// engine (swallowing it here is what stopped the trail from being drawn).
fn feeds_switcher_hover(held_button: u32, button: u32, switcher_visible: bool) -> bool {
    switcher_visible && held_button != u32::MAX && held_button == button
}

/// Route one event through the gesture module. Returns `Some` with the event to hand back (null to
/// swallow, the original to pass), or `None` when the module has nothing to say and the caller
/// continues to the scroll / button-mapping branches.
unsafe fn gesture_dispatch(
    event_type: CGEventType,
    event: CGEventRef,
    flags: CGEventFlags,
) -> Option<CGEventRef> {
    use crate::mouse::gesture::engine::Decision;
    use crate::mouse::gesture::runtime;

    // A recording owns the pointer while it runs: it takes the press, the samples and the release
    // before the gesture engine or the button mappings see any of them.
    let button = || CGEventGetIntegerValueField(event, K_CG_MOUSE_EVENT_BUTTON_NUMBER) as u32;
    // A press that is being LEARNED as a trigger button is passed through end to end. RECORDING is
    // cleared the moment the button is captured, but the user is still holding it: letting the
    // release (or a drag) reach the gesture path would act on a half press the application never
    // saw the down of.
    let learn_device =
        crate::mouse::gesture::runtime::device_key(device::device_from_cgevent(event));
    if crate::settings::learn_press_owned(button(), learn_device) {
        // The right and the side buttons have their own release types; the left button's release is
        // a leftMouseUp, which this path never sees (the gesture module is not its owner).
        if matches!(
            event_type,
            K_CG_EVENT_RIGHT_MOUSE_UP | K_CG_EVENT_OTHER_MOUSE_UP
        ) {
            // A refused learning swallowed its down, so its tail is swallowed too: no bare release,
            // and no mapping firing after the refusal.
            let refused = crate::settings::learn_press_refused(button(), learn_device);
            crate::settings::learn_press_released(button(), learn_device);
            return if refused {
                Some(std::ptr::null_mut())
            } else {
                None
            };
        }
        if crate::settings::learn_press_refused(button(), learn_device) {
            return Some(std::ptr::null_mut());
        }
        return None;
    }
    // A recording that was cancelled still owns the Up of the press it swallowed: the tail is
    // consumed by the recorder whatever the stage, so the application never sees a bare release.
    // Only a STOPPED recorder needs this: while a recording is active its own release handler
    // finishes the capture, and treating that release as a tail would swallow the result.
    if !crate::mouse::gesture::recording::is_recording()
        && matches!(
            event_type,
            K_CG_EVENT_RIGHT_MOUSE_UP | K_CG_EVENT_OTHER_MOUSE_UP
        )
        && crate::mouse::gesture::recording::owns_tail(number_of(event))
        && crate::mouse::gesture::recording::tap_release(number_of(event))
    {
        return Some(std::ptr::null_mut());
    }
    if crate::mouse::gesture::recording::is_recording() {
        match event_type {
            K_CG_EVENT_RIGHT_MOUSE_DOWN | K_CG_EVENT_OTHER_MOUSE_DOWN => {
                let number = button();
                if number != 0
                    && crate::mouse::gesture::recording::tap_press(number, cg_point(event))
                {
                    return Some(std::ptr::null_mut());
                }
            }
            K_CG_EVENT_RIGHT_MOUSE_DRAGGED | K_CG_EVENT_OTHER_MOUSE_DRAGGED => {
                let number = button();
                if crate::mouse::gesture::recording::tap_drag(number, cg_point(event)) {
                    // The preview is coalesced: the tap only publishes, and one scheduled main-thread
                    // task redraws the row with whatever the latest sample produced.
                    crate::mouse::gesture::recording::request_preview();
                    return Some(std::ptr::null_mut());
                }
            }
            K_CG_EVENT_RIGHT_MOUSE_UP | K_CG_EVENT_OTHER_MOUSE_UP => {
                let number = button();
                if crate::mouse::gesture::recording::tap_release(number) {
                    return Some(std::ptr::null_mut());
                }
            }
            _ => {}
        }
    }

    // Our own replay must never be taken for a fresh press: it would swallow its own click.
    if CGEventGetIntegerValueField(event, K_CG_EVENT_SOURCE_USER_DATA)
        == crate::mouse::gesture::replay::REPLAY_MARKER
    {
        return Some(event);
    }

    let button = || CGEventGetIntegerValueField(event, K_CG_MOUSE_EVENT_BUTTON_NUMBER) as u32;

    // The tail of a press this module swallowed stays ours even after the module stops: letting
    // the release fall through would send a mapping-up for a key that was never pressed.
    if matches!(
        event_type,
        K_CG_EVENT_RIGHT_MOUSE_UP | K_CG_EVENT_OTHER_MOUSE_UP
    ) {
        let number = button();
        let device = runtime::device_key(device::device_from_cgevent(event));
        let owned = runtime::with(|rt| rt.owns_tail(number, device));
        if owned {
            runtime::with(|rt| {
                rt.clear_tail(number, device);
                rt.release_tail(number, device, cg_point(event), now_ms())
            });
            return Some(std::ptr::null_mut());
        }
    }
    if event_type == K_CG_EVENT_KEY_UP {
        let keycode = CGEventGetIntegerValueField(event, K_CG_KEYBOARD_EVENT_KEYCODE) as u16;
        if keycode == VK_ESCAPE && runtime::with(|rt| rt.release_escape()) {
            return Some(std::ptr::null_mut());
        }
    }

    // An Esc this module already took keeps its repeats and its release EVEN AFTER the module is
    // switched off: the application would otherwise see a key-up whose key-down it never got.
    if event_type == K_CG_EVENT_KEY_DOWN {
        let keycode = CGEventGetIntegerValueField(event, K_CG_KEYBOARD_EVENT_KEYCODE) as u16;
        if keycode == VK_ESCAPE
            && CGEventGetIntegerValueField(event, K_CG_KEYBOARD_EVENT_AUTOREPEAT) != 0
            && runtime::with(|rt| rt.escape_owned())
        {
            return Some(std::ptr::null_mut());
        }
    }

    match event_type {
        K_CG_EVENT_RIGHT_MOUSE_DOWN | K_CG_EVENT_OTHER_MOUSE_DOWN => {
            let number = button();
            // The left button is not a gesture trigger; the right and the side buttons are.
            if number == 0 {
                return None;
            }
            let device = runtime::device_key(device::device_from_cgevent(event));
            let point = cg_point(event);
            let now = now_ms();
            // WHICH application this press lands in has to be read before the decision: the
            // exclusion list is about the app being clicked, not about whichever application macOS
            // calls active (see `excluded_bundle`).
            let bundle = runtime::excluded_bundle_for_event(window_under_pointer(event));
            if runtime::with(|rt| {
                rt.press(number, device, point, now, flags as u32, bundle.as_deref())
            }) {
                Some(std::ptr::null_mut())
            } else {
                None
            }
        }
        K_CG_EVENT_RIGHT_MOUSE_DRAGGED | K_CG_EVENT_OTHER_MOUSE_DRAGGED => {
            // The button that OPENED the switcher feeds its hover selection from the event's own
            // position: that button stays held for the whole summon, and a cursor query is not
            // reliable while it is (see `event_tap::cursor_location`). The event position is exact,
            // so selection does not depend on what any query reports.
            //
            // No OTHER button's drag is touched. A right-button stroke drawn while the switcher
            // happened to be on screen still belongs to the gesture module, and swallowing it here
            // is what stopped the trail from being drawn.
            if feeds_switcher_hover(
                SWITCHER_HELD_BUTTON.load(Ordering::SeqCst),
                button(),
                crate::mouse::gesture::session_state::switcher_shown(),
            ) {
                let location = event_tap::CGEventGetLocation(event);
                crate::schedule_overlay_hover_at(location.x, location.y);
                return Some(event);
            }
            let number = button();
            let device = runtime::device_key(device::device_from_cgevent(event));
            let point = cg_point(event);
            let now = now_ms();
            let (swallowed, trail) = runtime::with(|rt| rt.dragged(number, device, point, now));
            if swallowed {
                if let Some(trail) = trail {
                    crate::mouse::gesture::hud::publish(trail);
                }
                Some(std::ptr::null_mut())
            } else {
                None
            }
        }
        K_CG_EVENT_RIGHT_MOUSE_UP | K_CG_EVENT_OTHER_MOUSE_UP => {
            let number = button();
            let device = runtime::device_key(device::device_from_cgevent(event));
            let point = cg_point(event);
            let now = now_ms();
            let bundle = runtime::excluded_bundle_for_event(window_under_pointer(event));
            let (decision, target, ending_session, auth) = runtime::with(|rt| {
                let session = rt.session_id();
                let epoch = rt.epoch();
                let (decision, target) = rt.release(number, device, point, now, bundle.as_deref());
                (
                    decision,
                    target,
                    session,
                    crate::mouse::gesture::executor::Authorization {
                        epoch,
                        session,
                        generation: crate::mouse::gesture::runtime::service_generation(),
                    },
                )
            });
            match decision {
                Decision::Execute => {
                    runtime::with(|rt| rt.note_matched(number));
                    crate::mouse::gesture::hud::hide(ending_session);
                    if let Some(row) = target {
                        crate::mouse::gesture::executor::run(row, auth);
                    }
                    Some(std::ptr::null_mut())
                }
                Decision::Replay | Decision::Swallow => {
                    crate::mouse::gesture::hud::hide(ending_session);
                    Some(std::ptr::null_mut())
                }
                Decision::Pass => None,
            }
        }
        K_CG_EVENT_KEY_DOWN => {
            let keycode = CGEventGetIntegerValueField(event, K_CG_KEYBOARD_EVENT_KEYCODE) as u16;
            if keycode != VK_ESCAPE {
                return None;
            }
            // A repeat of an Esc this module already took is still ours and stays swallowed. Only an
            // unowned repeat passes: its key-down went to the application before the gesture started,
            // so taking the repeat (and the up after it) would leave a stuck key there.
            if CGEventGetIntegerValueField(event, K_CG_KEYBOARD_EVENT_AUTOREPEAT) != 0 {
                return if runtime::with(|rt| rt.escape_owned()) {
                    Some(std::ptr::null_mut())
                } else {
                    None
                };
            }
            // An Esc the module took keeps its repeats and its release: the application must never
            // see the tail of a press it never got.
            if runtime::with(|rt| rt.escape()) {
                Some(std::ptr::null_mut())
            } else {
                None
            }
        }
        _ => None,
    }
}

/// The window the event belongs to: `kCGMouseEventWindowUnderMousePointer`, which the WindowServer
/// fills in for every mouse event with the number of the window under the pointer. 0 when the field
/// is absent (a synthetic event), which the owner lookup reads as "unknown application".
fn window_under_pointer(event: CGEventRef) -> i64 {
    const K_CG_MOUSE_EVENT_WINDOW_UNDER_MOUSE_POINTER: i32 = 91;
    unsafe { CGEventGetIntegerValueField(event, K_CG_MOUSE_EVENT_WINDOW_UNDER_MOUSE_POINTER) }
}

/// The event's global point, in the coordinate space the recognizer uses.
fn number_of(event: CGEventRef) -> u32 {
    unsafe { CGEventGetIntegerValueField(event, K_CG_MOUSE_EVENT_BUTTON_NUMBER) as u32 }
}

unsafe fn cg_point(event: CGEventRef) -> crate::mouse::gesture::recognizer::Point {
    let location = event_tap::CGEventGetLocation(event);
    crate::mouse::gesture::recognizer::Point::new(location.x, location.y)
}

/// Milliseconds on a monotonic clock, for the recognizer's sample timing.
fn now_ms() -> f64 {
    static START: OnceLock<std::time::Instant> = OnceLock::new();
    let start = START.get_or_init(std::time::Instant::now);
    start.elapsed().as_secs_f64() * 1000.0
}

#[cfg(test)]
mod switcher_hover_tests {
    use super::feeds_switcher_hover;

    #[test]
    fn only_the_summoning_button_feeds_the_switcher_hover() {
        // Side button 3 opened the switcher and is still held: its drags select cards.
        assert!(feeds_switcher_hover(3, 3, true));
        // The right button drawn while the switcher is up is a GESTURE, not hover input. Swallowing
        // it was the regression that stopped the trail from being drawn.
        assert!(!feeds_switcher_hover(3, 1, true));
        // No summon in progress: nothing feeds hover, whatever is visible.
        assert!(!feeds_switcher_hover(u32::MAX, 3, true));
        // Switcher gone: the held record is stale until its up arrives, and it must not swallow.
        assert!(!feeds_switcher_hover(3, 3, false));
    }
}
