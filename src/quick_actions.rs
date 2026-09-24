//! Quick-actions module: Option+I opens Settings, Option+E opens Finder, Option+D shows the
//! desktop, Option+L locks the screen, and double-Control locates the pointer. A dedicated
//! session-level event tap (own thread) intercepts Option+letters and Control transitions; events
//! travel through the existing bridge (GlobalEvent -> performSelectorOnMainThread) and run on
//! the main thread. Same shape as window_management.rs: dedicated thread + RunLoop reference +
//! stop flag.

use objc2::runtime::AnyObject;
use objc2::{class, msg_send};

use crate::event_monitor::GlobalEvent;
use crate::event_tap::{
    self, keyboard, tap_location, tap_options, tap_placement, CFRunLoopGetCurrent,
    CGEventCreateKeyboardEvent, CGEventFlags, CGEventGetFlags, CGEventGetIntegerValueField,
    CGEventMask, CGEventPost, CGEventRef, CGEventSetFlags, CGEventTapProxy, CGEventType,
    K_CG_EVENT_SOURCE_USER_DATA, SYNTHETIC_MARKER,
};
use crate::ffi::{make_nsstring, CFRelease};
use crate::{log_debug, log_info};
use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{LazyLock, Mutex};
use std::thread;
use std::time::{Duration, Instant};

// Keycodes are from Carbon HIToolbox Events.h (kVK_ANSI_I/E/D/L).
const K_VK_I: u16 = 34;
const K_VK_E: u16 = 14;
const K_VK_D: u16 = 2;
const K_VK_L: u16 = 37;
// Modifier masks: exactly Option is required; combos with extra modifiers pass through
// (same rule as Option+arrows).
use crate::event_tap::keyboard::{
    EVENT_FLAGS_CHANGED as K_CG_EVENT_FLAGS_CHANGED, EVENT_KEY_DOWN as K_CG_EVENT_KEY_DOWN,
    EVENT_KEY_UP as K_CG_EVENT_KEY_UP, FIELD_AUTOREPEAT as K_CG_KEYBOARD_EVENT_AUTOREPEAT,
    FIELD_KEYCODE as K_CG_KEYBOARD_EVENT_KEYCODE, FLAG_COMMAND as K_FLAG_COMMAND,
    FLAG_CONTROL as K_FLAG_CONTROL, FLAG_OPTION as K_FLAG_OPTION, FLAG_SHIFT as K_FLAG_SHIFT,
};
const K_DOUBLE_CONTROL_INTERVAL: Duration = Duration::from_millis(350);

static CONTROL_DOWN: AtomicBool = AtomicBool::new(false);
static LAST_CONTROL_PRESS: LazyLock<Mutex<Option<Instant>>> = LazyLock::new(|| Mutex::new(None));

/// Quick actions. The numeric order crosses threads via NSNumber (bridge -> main thread);
/// append-only, never reorder.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum QuickAction {
    OpenSettings = 0,
    OpenFinder = 1,
    ShowDesktop = 2,
    LockScreen = 3,
    LocatePointer = 4,
}

impl QuickAction {
    /// Rebuild an action from the bridge integer (unknown values are dropped).
    pub(crate) fn from_isize(v: isize) -> Option<Self> {
        match v {
            0 => Some(Self::OpenSettings),
            1 => Some(Self::OpenFinder),
            2 => Some(Self::ShowDesktop),
            3 => Some(Self::LockScreen),
            4 => Some(Self::LocatePointer),
            _ => None,
        }
    }

    fn from_keycode(code: u16) -> Option<Self> {
        match code {
            K_VK_I => Some(Self::OpenSettings),
            K_VK_E => Some(Self::OpenFinder),
            K_VK_D => Some(Self::ShowDesktop),
            K_VK_L => Some(Self::LockScreen),
            _ => None,
        }
    }
}

/// Whether an action is enabled by config (master switch + the action's own switch).
fn action_enabled(action: QuickAction) -> bool {
    crate::config::CONFIG
        .read()
        .map(|c| {
            c.quick_actions.enabled
                && match action {
                    QuickAction::OpenSettings => c.quick_actions.open_settings,
                    QuickAction::OpenFinder => c.quick_actions.open_finder,
                    QuickAction::ShowDesktop => c.quick_actions.show_desktop,
                    QuickAction::LockScreen => c.quick_actions.lock_screen,
                    QuickAction::LocatePointer => c.quick_actions.locate_pointer,
                }
        })
        .unwrap_or(false)
}

/// The tap callback handles Option+I/E/D/L and double-Control. Enabled Option combos are
/// swallowed and non-autorepeat keyDowns are forwarded to the main thread; Control
/// flagsChanged events always pass through to the system. When our app is frontmost, it passes
/// through only while a settings text field is actively editing.
unsafe extern "C" fn quick_actions_tap_callback(
    _proxy: CGEventTapProxy,
    event_type: CGEventType,
    event: CGEventRef,
    _user_info: *mut c_void,
) -> CGEventRef {
    if crate::input_monitor::handle_disabled_event(event_type, "quick") {
        return event;
    }
    if !crate::input_monitor::taps_allowed() {
        return event;
    }
    if event_type == K_CG_EVENT_FLAGS_CHANGED {
        let flags = CGEventGetFlags(event);
        let control_down = flags & K_FLAG_CONTROL != 0;
        let was_down = CONTROL_DOWN.swap(control_down, Ordering::Relaxed);
        if control_down && !was_down {
            let no_other_modifiers = flags & (K_FLAG_OPTION | K_FLAG_COMMAND | K_FLAG_SHIFT) == 0;
            if no_other_modifiers && action_enabled(QuickAction::LocatePointer) {
                let now = Instant::now();
                let mut last = LAST_CONTROL_PRESS.lock().unwrap();
                let is_double = last.is_some_and(|previous| {
                    now.duration_since(previous) <= K_DOUBLE_CONTROL_INTERVAL
                });
                if is_double {
                    *last = None;
                    log_debug!("[quick] double-Control locate pointer");
                    crate::enqueue_global_event(GlobalEvent::QuickAction(
                        QuickAction::LocatePointer as u8,
                    ));
                } else {
                    *last = Some(now);
                }
            } else {
                *LAST_CONTROL_PRESS.lock().unwrap() = None;
            }
        }
        return event;
    }
    if event_type != K_CG_EVENT_KEY_DOWN && event_type != K_CG_EVENT_KEY_UP {
        return event;
    }
    let keycode = CGEventGetIntegerValueField(event, K_CG_KEYBOARD_EVENT_KEYCODE) as u16;
    let Some(action) = QuickAction::from_keycode(keycode) else {
        return event;
    };
    let flags = CGEventGetFlags(event);
    if flags & K_FLAG_OPTION == 0 || flags & (K_FLAG_COMMAND | K_FLAG_SHIFT | K_FLAG_CONTROL) != 0 {
        return event;
    }
    // Our own synthesized combos (mouse Key Press mappings post at HID level and loop back
    // into session taps) must pass through, or a side button mapped to Option+letter gets
    // hijacked here.
    if CGEventGetIntegerValueField(event, K_CG_EVENT_SOURCE_USER_DATA) == SYNTHETIC_MARKER {
        return event;
    }
    if !action_enabled(action) {
        return event;
    }
    let (_name, pid) = crate::ffi::frontmost_app_info();
    if pid == std::process::id() as i32 && crate::settings::is_text_input_active() {
        return event;
    }
    if event_type == K_CG_EVENT_KEY_DOWN {
        // Ignore system autorepeat: the actions are idempotent one-shots; holding the key
        // should fire once.
        let autorepeat = CGEventGetIntegerValueField(event, K_CG_KEYBOARD_EVENT_AUTOREPEAT);
        if autorepeat == 0 {
            log_debug!("[quick] keyDown Option+{:?}", action);
            crate::enqueue_global_event(GlobalEvent::QuickAction(action as u8));
        }
    }
    // Swallow matching keyDown/keyUp (autorepeat included); apps never see the combo.
    std::ptr::null_mut()
}

/// Enable quick actions at runtime (shared by the settings hot-switch and the startup path).
/// Idempotent.
pub(crate) fn start() {
    if !crate::input_monitor::taps_allowed() {
        return;
    }
    let mut guard = QA_THREAD.lock().unwrap();
    if guard.as_ref().is_some_and(|h| !h.is_finished()) {
        return;
    }
    if let Some(finished) = guard.take() {
        let _ = finished.join();
    }
    TAP_CONTROL.prepare_start();
    CONTROL_DOWN.store(false, Ordering::Relaxed);
    *LAST_CONTROL_PRESS.lock().unwrap() = None;
    *guard = Some(spawn_tap_thread());
    log_info!("Quick actions enabled.");
}

/// Disable quick actions at runtime (settings hot-switch). Idempotent.
pub(crate) fn stop() {
    TAP_CONTROL.stop();
    let handle = QA_THREAD.lock().unwrap().take();
    if let Some(h) = handle {
        let _ = h.join();
    }
    log_info!("Quick actions disabled.");
}

static TAP_CONTROL: event_tap::TapThreadControl = event_tap::TapThreadControl::new();
static QA_THREAD: Mutex<Option<thread::JoinHandle<()>>> = Mutex::new(None);

fn spawn_tap_thread() -> thread::JoinHandle<()> {
    // Listen mask: keyDown + keyUp + flagsChanged (the double-Control edge).
    let mask: CGEventMask = (1u64 << K_CG_EVENT_KEY_DOWN)
        | (1u64 << K_CG_EVENT_KEY_UP)
        | (1u64 << K_CG_EVENT_FLAGS_CHANGED);
    thread::spawn(move || unsafe {
        crate::performance::set_current_thread_qos(crate::performance::ThreadQos::UserInteractive);
        // Session-level tap: same layer as the switcher, sees real hardware keys; DEFAULT_TAP
        // is required to swallow events.
        let created = event_tap::create_tap_with_retry(
            tap_location::SESSION_EVENT_TAP,
            tap_placement::HEAD_INSERT,
            tap_options::DEFAULT_TAP,
            mask,
            Some(quick_actions_tap_callback),
            std::ptr::null_mut(),
            "quick",
            Some(TAP_CONTROL.cancel_flag()),
        );
        let created = match created {
            Some(created) => created,
            None => return,
        };
        let rl = CFRunLoopGetCurrent();
        TAP_CONTROL.register(created.tap, rl);
        let watchdog = event_tap::start_tap_watchdog(created.tap, TAP_CONTROL.cancel_flag());
        if !TAP_CONTROL.cancel_flag().load(Ordering::SeqCst) && crate::input_monitor::taps_allowed()
        {
            log_debug!("Quick actions event tap started.");
            event_tap::CFRunLoopRun();
        }
        event_tap::stop_tap_watchdog(watchdog);
        event_tap::CGEventTapEnable(created.tap, false);
        TAP_CONTROL.clear(created.tap);
        event_tap::teardown_event_tap(rl, created);
    })
}

/// Main thread: run one quick action (delivered by the bridge).
pub(crate) fn apply_action(action: QuickAction) {
    // The event may land on the main thread after the feature was switched off; re-check.
    if !action_enabled(action) {
        return;
    }
    match action {
        QuickAction::OpenSettings => {
            // Open System Settings (the x-apple.systempreferences: URL scheme is registered by
            // System Settings; openURL: launches or raises it). "Open Settings" refers to the
            // system's settings, not this app's window.
            unsafe { open_system_settings() };
        }
        QuickAction::OpenFinder => {
            unsafe { open_new_finder_window() };
        }
        QuickAction::ShowDesktop => {
            // Same path as the mouse system action: the Dock notification triggers the
            // system's Show Desktop.
            crate::mouse::system_action::fire("com.apple.showdesktop.awake");
        }
        QuickAction::LockScreen => lock_screen(),
        QuickAction::LocatePointer => crate::pointer_locator::show(),
    }
}

/// Synthesize macOS's Control+Command+Q lock-screen shortcut without blocking the main thread.
fn lock_screen() {
    // macOS 26 removed the legacy CGSession path, so use the public CoreGraphics event API.
    const KEYCODE_Q: u16 = 12;
    const K_FLAG_COMMAND_CONTROL: CGEventFlags = K_FLAG_COMMAND | K_FLAG_CONTROL;
    unsafe {
        let down = CGEventCreateKeyboardEvent(std::ptr::null(), KEYCODE_Q, true);
        let up = CGEventCreateKeyboardEvent(std::ptr::null(), KEYCODE_Q, false);
        if down.is_null() || up.is_null() {
            // Whichever half was created still carries +1 ownership: the failure path must
            // release the non-null one or it leaks.
            if !down.is_null() {
                CFRelease(down as *const c_void);
            }
            if !up.is_null() {
                CFRelease(up as *const c_void);
            }
            log_info!("[quick] lock screen failed: could not create keyboard event");
            return;
        }
        CGEventSetFlags(down, K_FLAG_COMMAND_CONTROL);
        CGEventSetFlags(up, K_FLAG_COMMAND_CONTROL);
        CGEventPost(tap_location::SESSION_EVENT_TAP, down);
        CGEventPost(tap_location::SESSION_EVENT_TAP, up);
        CFRelease(down as *const c_void);
        CFRelease(up as *const c_void);
    }
    log_debug!("[quick] lock screen requested");
}

/// Open System Settings via the x-apple.systempreferences: URL scheme; LaunchServices
/// launches or raises System Settings.
unsafe fn open_system_settings() {
    let url_str = make_nsstring("x-apple.systempreferences:");
    // URLWithString: returns an autoreleased NSURL (+0) we do not own; never CFRelease it.
    let url: *mut AnyObject = msg_send![class!(NSURL), URLWithString: url_str];
    CFRelease(url_str as *const c_void);
    let workspace: *mut AnyObject = msg_send![class!(NSWorkspace), sharedWorkspace];
    // openURL: returns BOOL; objc2 validates the return encoding, so receive it as bool.
    let opened: bool = msg_send![workspace, openURL: url];
    log_debug!("[quick] System Settings opened: {}", opened);
}

/// Open a NEW, maximized Finder window every time (Win+E semantics; openURL: dedupes and
/// only raises an existing window showing the folder). Activate Finder first
/// (IgnoreOtherApps), then post one Cmd+N straight to Finder's process via CGEventPostToPid
/// -- pid-targeted delivery does not depend on activation timing: Finder itself always
/// dequeues it and creates the window. The new window is created asynchronously: remember
/// the focused-window id before Cmd+N, poll until the focused window id changes (the new
/// window has appeared), then maximize it right away (zoom, NOT fullscreen). AX calls need
/// the main thread; this function already runs there (via the event bridge). The app holds
/// the Accessibility permission (its event taps require it), so synthesizing keystrokes and
/// AX writes is legitimate.
unsafe fn open_new_finder_window() {
    let mut finder_app = find_finder_app();
    if finder_app.is_null() {
        // Finder not running: openURL: launches it via LaunchServices with the home folder;
        // wait for the launch, then maximize the window the same way.
        let home = std::env::var("HOME").unwrap_or_else(|_| "/".to_string());
        let path = make_nsstring(&home);
        // fileURLWithPath: returns an autoreleased NSURL (+0) we do not own; never release.
        let url: *mut AnyObject = msg_send![class!(NSURL), fileURLWithPath: path];
        CFRelease(path as *const c_void);
        let workspace: *mut AnyObject = msg_send![class!(NSWorkspace), sharedWorkspace];
        let opened: bool = msg_send![workspace, openURL: url];
        log_debug!("[quick] Finder launched with home folder: {}", opened);
        // Cold start can take seconds: wait up to ~4s.
        for _ in 0..20 {
            std::thread::sleep(std::time::Duration::from_millis(200));
            finder_app = find_finder_app();
            if !finder_app.is_null() {
                break;
            }
        }
        if finder_app.is_null() {
            log_debug!("[quick] Finder did not launch in time");
            return;
        }
        let pid: i32 = msg_send![finder_app, processIdentifier];
        // The launch-opened folder window IS the new window; maximize once it shows in AX.
        for _ in 0..12 {
            std::thread::sleep(std::time::Duration::from_millis(150));
            if crate::window_management::focused_cgwid_of_pid(pid).is_some() {
                let ok = crate::window_management::maximize_focused_window_of_pid(pid);
                log_debug!("[quick] launched Finder window maximized: {}", ok);
                return;
            }
        }
        return;
    }

    let pid: i32 = msg_send![finder_app, processIdentifier];
    // The focused window before Cmd+N: lets the poll tell the new window from the old one.
    let prev_cgwid = crate::window_management::focused_cgwid_of_pid(pid);
    // NSApplicationActivateIgnoringOtherApps = 1 << 1; activateWithOptions: returns BOOL and
    // must be received as bool (objc2 validates return encodings in debug builds).
    let activated: bool = msg_send![finder_app, activateWithOptions: 2isize];
    log_debug!("[quick] Finder activated: {} pid={}", activated, pid);
    post_cmd_n_to_pid(pid);
    for _ in 0..12 {
        std::thread::sleep(std::time::Duration::from_millis(150));
        let cur = crate::window_management::focused_cgwid_of_pid(pid);
        if let Some(cgwid) = cur {
            if Some(cgwid) != prev_cgwid {
                let ok = crate::window_management::maximize_focused_window_of_pid(pid);
                log_debug!("[quick] new Finder window maximized: {}", ok);
                return;
            }
        }
    }
    log_debug!("[quick] new Finder window did not appear in time");
}

/// Find Finder in the running applications (returns an NSRunningApplication, a +0 reference
/// we do not own).
unsafe fn find_finder_app() -> *mut AnyObject {
    let workspace: *mut AnyObject = msg_send![class!(NSWorkspace), sharedWorkspace];
    let finder_ns = make_nsstring("com.apple.finder");
    let apps: *mut AnyObject = msg_send![workspace, runningApplications];
    let count: usize = msg_send![apps, count];
    let mut found: *mut AnyObject = std::ptr::null_mut();
    for i in 0..count {
        let app: *mut AnyObject = msg_send![apps, objectAtIndex: i as isize];
        // bundleIdentifier is a copy-property getter returning a +0 reference we do NOT own;
        // never CFRelease it (early release double-frees when the pool drains).
        let bundle: *mut AnyObject = msg_send![app, bundleIdentifier];
        if bundle.is_null() {
            continue;
        }
        let is_finder: bool = msg_send![bundle, isEqualToString: finder_ns];
        if is_finder {
            found = app;
            break;
        }
    }
    CFRelease(finder_ns as *const c_void);
    found
}

/// Post one Cmd+N (down + up) targeted at the given process. The event enters that process's
/// own queue and is handled by it, so focus changes in other apps cannot steal it.
unsafe fn post_cmd_n_to_pid(pid: i32) {
    #[link(name = "CoreGraphics", kind = "framework")]
    extern "C" {
        fn CGEventPostToPid(pid: i32, event: CGEventRef);
    }
    // kVK_ANSI_N = 45 (matches shortcut.rs's "n" -> 0x2D).
    const KEY_N: u16 = 0x2D;
    const K_FLAG_COMMAND: CGEventFlags = keyboard::FLAG_COMMAND;
    let down = CGEventCreateKeyboardEvent(std::ptr::null_mut(), KEY_N, true);
    let up = CGEventCreateKeyboardEvent(std::ptr::null_mut(), KEY_N, false);
    CGEventSetFlags(down, K_FLAG_COMMAND);
    CGEventSetFlags(up, K_FLAG_COMMAND);
    CGEventPostToPid(pid, down);
    std::thread::sleep(std::time::Duration::from_millis(30));
    CGEventPostToPid(pid, up);
    // CGEventCreateKeyboardEvent returns +1; release after use.
    CFRelease(down as *const c_void);
    CFRelease(up as *const c_void);
    log_debug!("[quick] Cmd+N posted to pid {}", pid);
}
