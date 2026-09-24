//! Common CGEventTap infrastructure: type aliases, FFI extern declarations, semantic
//! constants, and a generic start helper. Shared by the window switcher (event_monitor)
//! and the mouse enhancement (mouse::event_tap) modules. A leaf module.

use crate::ffi::has_accessibility_permission;
use crate::log_info;
use std::ffi::c_void;
use std::sync::atomic::Ordering;
use std::sync::Mutex;
use std::thread;
use std::time::{Duration, Instant};

pub(crate) type CGEventRef = *mut c_void;
pub(crate) type CGEventTapProxy = *mut c_void;
pub(crate) type CFMachPortRef = *mut c_void;
pub(crate) type CFRunLoopSourceRef = *mut c_void;
pub(crate) type CFRunLoopTimerRef = *mut c_void;
pub(crate) type CFRunLoopRef = *mut c_void;
pub(crate) type CFStringRef = *mut c_void;
pub(crate) type CFAllocatorRef = *mut c_void;
pub(crate) type CGEventType = u32;
pub(crate) type CGEventFlags = u64;
pub(crate) type CGEventMask = u64;

/// Shared keyboard event types, fields, modifier masks, and common keycodes live in this leaf
/// module so the window switcher, quick actions, and window control do not maintain drifting
/// copies. Module-specific action keys remain local.
pub(crate) mod keyboard {
    pub(crate) const EVENT_KEY_DOWN: super::CGEventType = 10;
    pub(crate) const EVENT_KEY_UP: super::CGEventType = 11;
    pub(crate) const EVENT_FLAGS_CHANGED: super::CGEventType = 12;
    pub(crate) const FIELD_AUTOREPEAT: i32 = 8;
    pub(crate) const FIELD_KEYCODE: i32 = 9;

    pub(crate) const FLAG_COMMAND: super::CGEventFlags = 0x0010_0000;
    pub(crate) const FLAG_OPTION: super::CGEventFlags = 0x0008_0000;
    pub(crate) const FLAG_CONTROL: super::CGEventFlags = 0x0004_0000;
    pub(crate) const FLAG_SHIFT: super::CGEventFlags = 0x0002_0000;

    pub(crate) const VK_TAB: u16 = 48;
    pub(crate) const VK_V: u16 = 9;
    pub(crate) const VK_LEFT: u16 = 123;
    pub(crate) const VK_RIGHT: u16 = 124;
    pub(crate) const VK_DOWN: u16 = 125;
    pub(crate) const VK_UP: u16 = 126;
}

/// CGEventTapDisabled pseudo-event values from CoreGraphics' CGEventTypes.h.
pub(crate) const TAP_DISABLED_BY_TIMEOUT: CGEventType = 0xFFFF_FFFE;
pub(crate) const TAP_DISABLED_BY_USER_INPUT: CGEventType = 0xFFFF_FFFF;

/// The Core Foundation objects created by CGEventTapCreate and
/// CFMachPortCreateRunLoopSource. The caller must remove and release them through
/// teardown_event_tap() before its thread exits.
#[derive(Clone, Copy)]
pub(crate) struct CreatedEventTap {
    pub(crate) tap: CFMachPortRef,
    pub(crate) source: CFRunLoopSourceRef,
}

#[derive(Default)]
struct ActiveTap {
    tap: Option<CFMachPortRef>,
    run_loop: Option<CFRunLoopRef>,
}

struct ActiveTapMutex(Mutex<ActiveTap>);
unsafe impl Send for ActiveTapMutex {}
unsafe impl Sync for ActiveTapMutex {}

/// Control handle shared with a tap's owner so shutdown can disable its port immediately,
/// including while the tap thread is waiting inside CFRunLoopRun.
pub(crate) struct TapThreadControl {
    stop_requested: std::sync::atomic::AtomicBool,
    active: ActiveTapMutex,
}

impl TapThreadControl {
    pub(crate) const fn new() -> Self {
        Self {
            stop_requested: std::sync::atomic::AtomicBool::new(false),
            active: ActiveTapMutex(Mutex::new(ActiveTap {
                tap: None,
                run_loop: None,
            })),
        }
    }

    pub(crate) fn prepare_start(&self) {
        self.stop_requested.store(false, Ordering::SeqCst);
    }

    pub(crate) fn cancel_flag(&'static self) -> &'static std::sync::atomic::AtomicBool {
        &self.stop_requested
    }

    pub(crate) fn stop(&self) {
        self.stop_requested.store(true, Ordering::SeqCst);
        let active = self.active.0.lock().unwrap();
        unsafe {
            if let Some(tap) = active.tap {
                CGEventTapEnable(tap, false);
            }
            if let Some(run_loop) = active.run_loop {
                CFRunLoopStop(run_loop);
            }
        }
    }

    pub(crate) fn register(&self, tap: CFMachPortRef, run_loop: CFRunLoopRef) {
        let mut active = self.active.0.lock().unwrap();
        active.tap = Some(tap);
        active.run_loop = Some(run_loop);
        if self.stop_requested.load(Ordering::SeqCst) || !crate::input_monitor::taps_allowed() {
            unsafe {
                CGEventTapEnable(tap, false);
                CFRunLoopStop(run_loop);
            }
        }
    }

    pub(crate) fn clear(&self, tap: CFMachPortRef) {
        let mut active = self.active.0.lock().unwrap();
        if active.tap == Some(tap) {
            active.tap = None;
            active.run_loop = None;
        }
    }
}

pub(crate) type CGEventTapCallBack = Option<
    unsafe extern "C" fn(
        proxy: CGEventTapProxy,
        event_type: CGEventType,
        event: CGEventRef,
        user_info: *mut c_void,
    ) -> CGEventRef,
>;

/// CFRunLoopTimer callout: (timer, info).
pub(crate) type CFRunLoopTimerCallBack =
    Option<unsafe extern "C" fn(CFRunLoopTimerRef, *mut c_void)>;

// Semantic enums in place of raw magic numbers, reducing per-caller hardcoding errors.

/// Tap location for CGEventTapCreate.
#[allow(dead_code)]
pub(crate) mod tap_location {
    /// HID level: lowest, sees all hardware events (including session-synthesized ones).
    pub(crate) const HID_EVENT_TAP: i32 = 0;
    /// Session level: sees real hardware events + session-synthesized Cmd+Tab (mouse-remapper injected).
    pub(crate) const SESSION_EVENT_TAP: i32 = 1;
    #[allow(dead_code)]
    pub(crate) const ANNOTATED_SESSION_EVENT_TAP: i32 = 2;
}

/// Placement for CGEventTapCreate.
#[allow(dead_code)]
pub(crate) mod tap_placement {
    /// Head insert: sees events first.
    pub(crate) const HEAD_INSERT: i32 = 0;
    /// Tail insert: sees events last.
    pub(crate) const TAIL_INSERT: i32 = 1;
}

/// Options for CGEventTapCreate. Note the counterintuitive values (see CGEventTypes.h):
/// Default=0 is mutable, ListenOnly=1 is read-only.
#[allow(dead_code)]
pub(crate) mod tap_options {
    /// Default tap: may modify/drop events (requires AX permission). Used when rewriting events.
    pub(crate) const DEFAULT_TAP: u32 = 0;
    /// Listen only: cannot modify events. For observation/logging; debug-safe (callback bugs won't swallow events).
    pub(crate) const LISTEN_ONLY: u32 = 1;
}

#[link(name = "CoreGraphics", kind = "framework")]
extern "C" {
    pub(crate) fn CGEventTapCreate(
        tap: i32,
        place: i32,
        options: u32,
        events_of_interest: CGEventMask,
        callback: CGEventTapCallBack,
        user_info: *mut c_void,
    ) -> CFMachPortRef;

    pub(crate) fn CGEventTapEnable(tap: CFMachPortRef, enable: bool);
    // Query whether the tap is enabled system-side (used by the watchdog).
    pub(crate) fn CGEventTapIsEnabled(tap: CFMachPortRef) -> bool;
    pub(crate) fn CGEventGetIntegerValueField(event: CGEventRef, field: i32) -> i64;
    pub(crate) fn CGEventSetIntegerValueField(event: CGEventRef, field: i32, value: i64);
    #[allow(dead_code)]
    pub(crate) fn CGEventGetDoubleValueField(event: CGEventRef, field: i32) -> f64;
    #[allow(dead_code)]
    pub(crate) fn CGEventSetDoubleValueField(event: CGEventRef, field: i32, value: f64);
    pub(crate) fn CGEventGetFlags(event: CGEventRef) -> CGEventFlags;
    pub(crate) fn CGEventSetFlags(event: CGEventRef, flags: CGEventFlags);
    // Query the combined session's current modifier state so diagnostics can compare an
    // event's flags with the system-wide state.
    pub(crate) fn CGEventSourceFlagsState(state_id: i32) -> CGEventFlags;
    // Change an event's type (e.g. turn a keyboard event into flagsChanged, for synthesizing
    // modifier-key state transitions). No caller today (key synthesis no longer emits
    // flagsChanged); kept for future modifier-state synthesis.
    #[allow(dead_code)]
    pub(crate) fn CGEventSetType(event: CGEventRef, t: CGEventType);
    // Extract the underlying IOHIDEvent from a CGEvent (public API); used for event attribution
    // (matching events to the producing device for per-device config).
    pub(crate) fn CGEventCopyIOHIDEvent(event: CGEventRef) -> *mut c_void;

    // Create a brand-new scroll wheel event.
    // source=null for default source; wheelCount typically 2 (wheel1=vertical, wheel2=horizontal).
    pub(crate) fn CGEventCreateScrollWheelEvent2(
        source: *const c_void,
        units: u32,
        wheel_count: u32,
        wheel1: i32,
        wheel2: i32,
        wheel3: i32,
    ) -> CGEventRef;

    // Post an event to a tap level. kCGSessionEventTap=1 posts to the session level,
    // bypassing HID-level taps and thus the system's natural-scroll override at the HID layer.
    pub(crate) fn CGEventPost(tap: i32, event: CGEventRef);

    // Create a keyboard event (keyDown=1 / keyUp=0), used by the history clipboard to
    // synthesize Cmd+V for pasting.
    pub(crate) fn CGEventCreateKeyboardEvent(
        source: *const c_void,
        keycode: u16,
        key_down: bool,
    ) -> CGEventRef;

    // Query an event's global screen point (bottom-left origin, points). Used by the hover
    // poll to read the current cursor position.
    pub(crate) fn CGEventGetLocation(event: CGEventRef) -> CGPoint;

    // Create an event (null source = default source): a source-less event carries the
    // current mouse location. Used by the hover poll to read the global cursor without
    // depending on the mouseMoved stream (while a side button is held the system emits no
    // mouseMoved, freezing NSEvent.mouseLocation -- verified).
    pub(crate) fn CGEventCreate(source: *const c_void) -> CGEventRef;
}

/// kCGEventSourceStateCombinedSessionState. Keep the raw enum value in one place rather than
/// duplicating it across diagnostic callers.
pub(crate) fn combined_session_flags() -> CGEventFlags {
    unsafe { CGEventSourceFlagsState(0) }
}

/// Rust representation of CGPoint (same layout as CoreGraphics').
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct CGPoint {
    pub x: f64,
    pub y: f64,
}

// IOKit private API: read/write float fields of an IOHIDEvent.
// Unused by the current synthetic-event approach (kept for potential future use).
#[allow(dead_code)]
#[link(name = "IOKit", kind = "framework")]
extern "C" {
    /// Read a float field from an IOHIDEvent.
    pub(crate) fn IOHIDEventGetFloatValue(event: *mut c_void, field: u32) -> f64;
    /// Write a float field to an IOHIDEvent.
    pub(crate) fn IOHIDEventSetFloatValue(event: *mut c_void, field: u32, value: f64);
}

// CFRunLoop functions + timer, linking CoreFoundation.
#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    pub(crate) fn CFMachPortCreateRunLoopSource(
        allocator: CFAllocatorRef,
        port: CFMachPortRef,
        order: i64,
    ) -> CFRunLoopSourceRef;

    pub(crate) fn CFRunLoopAddSource(
        rl: CFRunLoopRef,
        source: CFRunLoopSourceRef,
        mode: CFStringRef,
    );
    pub(crate) fn CFRunLoopGetCurrent() -> CFRunLoopRef;
    pub(crate) fn CFRunLoopGetMain() -> CFRunLoopRef;
    pub(crate) fn CFRunLoopRun();
    pub(crate) fn CFRunLoopStop(rl: CFRunLoopRef);
    pub(crate) fn CFRunLoopTimerInvalidate(timer: CFRunLoopTimerRef);

    // Timer (for the watchdog). fireDate=0 fires on the next runloop pass, interval is the period
    // in seconds. The context argument points to a CFRunLoopTimerContext struct which Create copies;
    // its info field is passed back to the callback -- here info is the tap pointer.
    pub(crate) fn CFRunLoopTimerCreate(
        allocator: CFAllocatorRef,
        fire_date: f64,
        interval: f64,
        flags: u32,
        order: i64,
        callback: CFRunLoopTimerCallBack,
        context: *mut c_void,
    ) -> CFRunLoopTimerRef;
    pub(crate) fn CFRunLoopAddTimer(rl: CFRunLoopRef, timer: CFRunLoopTimerRef, mode: CFStringRef);

    pub(crate) static kCFRunLoopDefaultMode: CFStringRef;
}

// Without Accessibility permission, no tap is created; the permission supervisor restarts services
// from current configuration after trust is restored. A transient creation failure while permission
// remains valid is retried every RETRY_INTERVAL, up to RETRY_MAX times.
const RETRY_INTERVAL: Duration = Duration::from_secs(3);
const RETRY_CANCEL_POLL_INTERVAL: Duration = Duration::from_millis(20);
const RETRY_MAX: u32 = 40;

fn wait_for_retry_or_cancel(cancel: Option<&'static std::sync::atomic::AtomicBool>) -> bool {
    let deadline = Instant::now() + RETRY_INTERVAL;
    loop {
        if cancel.is_some_and(|flag| flag.load(std::sync::atomic::Ordering::Relaxed)) {
            return true;
        }
        let now = Instant::now();
        if now >= deadline {
            return false;
        }
        std::thread::sleep((deadline - now).min(RETRY_CANCEL_POLL_INTERVAL));
    }
}

/// Create an event tap and add it to the current thread's CFRunLoop. Retries on failure
/// per RETRY_INTERVAL/RETRY_MAX. Returns the created tap (or None if retries exhausted).
///
/// # Safety
/// Caller must invoke on a dedicated thread (CFRunLoopRun will block it afterwards).
///
/// `cancel` is an optional cancellation flag: when set, the retry loop bails out early
/// (used when stopping the mouse tap at runtime so join() doesn't block the caller during
/// the retry window). None means never cancel (e.g. the keyboard tap, resident for the app's
/// lifetime).
#[allow(clippy::too_many_arguments)]
pub(crate) unsafe fn create_tap_with_retry(
    location: i32,
    placement: i32,
    options: u32,
    mask: CGEventMask,
    callback: CGEventTapCallBack,
    user_info: *mut c_void,
    log_name: &str,
    cancel: Option<&'static std::sync::atomic::AtomicBool>,
) -> Option<CreatedEventTap> {
    if !crate::input_monitor::watchdog_may_enable_tap() {
        log_info!(
            "[{}] Event tap not created because Accessibility input is disabled.",
            log_name
        );
        return None;
    }
    let mut tap = CGEventTapCreate(location, placement, options, mask, callback, user_info);

    // First creation failed (usually missing Accessibility): retry a bounded number of times
    // to give the user time to grant permission in System Settings.
    if tap.is_null() {
        log_info!(
            "[{}] No Accessibility permission yet; event tap will retry every {:?} up to {} times (~{}s).",
            log_name,
            RETRY_INTERVAL,
            RETRY_MAX,
            RETRY_INTERVAL.as_secs() * RETRY_MAX as u64
        );
        let mut granted = false;
        for _ in 0..RETRY_MAX {
            // Wait in short slices while polling cancellation so runtime disable is not held up
            // by the full three-second retry interval.
            if wait_for_retry_or_cancel(cancel) || !crate::input_monitor::watchdog_may_enable_tap()
            {
                log_info!("[{}] Event tap cancelled by stop request.", log_name);
                return None;
            }
            if has_accessibility_permission() {
                tap = CGEventTapCreate(location, placement, options, mask, callback, user_info);
                if !tap.is_null() {
                    granted = true;
                    break;
                }
            }
        }
        if granted {
            log_info!(
                "[{}] Accessibility permission granted; event tap created.",
                log_name
            );
        } else {
            log_info!(
                "[{}] Event tap retry exhausted ({}x); permission supervisor will retry while the feature is enabled.",
                log_name,
                RETRY_MAX
            );
            return None;
        }
    }

    let source = CFMachPortCreateRunLoopSource(std::ptr::null_mut(), tap, 0);
    if source.is_null() {
        crate::ffi::CFRelease(tap as *const c_void);
        log_info!("[{}] Failed to create event tap run-loop source.", log_name);
        return None;
    }
    CFRunLoopAddSource(CFRunLoopGetCurrent(), source, kCFRunLoopDefaultMode);
    if cancel.is_some_and(|flag| flag.load(Ordering::SeqCst))
        || !crate::input_monitor::watchdog_may_enable_tap()
    {
        teardown_event_tap(CFRunLoopGetCurrent(), CreatedEventTap { tap, source });
        return None;
    }
    CGEventTapEnable(tap, true);
    Some(CreatedEventTap { tap, source })
}

/// Disable and release an event tap and its RunLoop source synchronously.
///
/// # Safety
/// `created` must still belong to `run_loop` and may be torn down only once.
pub(crate) unsafe fn teardown_event_tap(run_loop: CFRunLoopRef, created: CreatedEventTap) {
    CGEventTapEnable(created.tap, false);
    crate::ffi::CFRunLoopRemoveSource(
        run_loop,
        created.source,
        kCFRunLoopDefaultMode as *const c_void,
    );
    crate::ffi::CFRelease(created.source as *const c_void);
    crate::ffi::CFRelease(created.tap as *const c_void);
}

/// Context struct for CFRunLoopTimerCreate (version=0; info is passed back to the callback).
#[repr(C)]
pub(crate) struct CFRunLoopTimerContext {
    pub(crate) version: isize,
    pub(crate) info: *mut c_void,
    pub(crate) retain: Option<unsafe extern "C" fn(*const c_void) -> *const c_void>,
    pub(crate) release: Option<unsafe extern "C" fn(*const c_void)>,
    pub(crate) copy_description: Option<unsafe extern "C" fn(*const c_void) -> CFStringRef>,
}

struct TapWatchdogContext {
    tap: CFMachPortRef,
    stop_requested: &'static std::sync::atomic::AtomicBool,
}

pub(crate) struct TapWatchdog {
    timer: CFRunLoopTimerRef,
    context: *mut TapWatchdogContext,
}

/// Timeout disables are recoverable; explicit stop, permission loss, and UserInput disables are
/// terminal for the affected tap and must never be undone by this watchdog.
unsafe extern "C" fn tap_watchdog_callback(_timer: CFRunLoopTimerRef, info: *mut c_void) {
    if info.is_null() {
        return;
    }
    let context = &*(info as *const TapWatchdogContext);
    let tap = context.tap;
    if tap.is_null()
        || context.stop_requested.load(Ordering::SeqCst)
        || !crate::input_monitor::watchdog_may_enable_tap()
    {
        return;
    }
    if !CGEventTapIsEnabled(tap) {
        CGEventTapEnable(tap, true);
        log_info!("[tap] event tap was disabled by the system; re-enabled.");
    }
}

/// Attach a 3s watchdog to a tap. The context stays alive until stop_tap_watchdog.
pub(crate) unsafe fn start_tap_watchdog(
    tap: CFMachPortRef,
    stop_requested: &'static std::sync::atomic::AtomicBool,
) -> TapWatchdog {
    let context = Box::into_raw(Box::new(TapWatchdogContext {
        tap,
        stop_requested,
    }));
    let ctx = CFRunLoopTimerContext {
        version: 0,
        info: context as *mut c_void,
        retain: None,
        release: None,
        copy_description: None,
    };
    let timer = CFRunLoopTimerCreate(
        std::ptr::null_mut(),
        0.0, // fire on the next runloop pass
        3.0, // then every 3s
        0,
        0,
        Some(tap_watchdog_callback),
        &ctx as *const CFRunLoopTimerContext as *mut c_void,
    );
    if !timer.is_null() {
        CFRunLoopAddTimer(CFRunLoopGetCurrent(), timer, kCFRunLoopDefaultMode);
    }
    TapWatchdog { timer, context }
}

pub(crate) unsafe fn stop_tap_watchdog(watchdog: TapWatchdog) {
    if !watchdog.timer.is_null() {
        CFRunLoopTimerInvalidate(watchdog.timer);
        crate::ffi::CFRelease(watchdog.timer as *const c_void);
    }
    if !watchdog.context.is_null() {
        drop(Box::from_raw(watchdog.context));
    }
}

/// Start a CGEventTap + CFRunLoop on a dedicated thread.
/// Wraps the common "spawn thread -> create tap (with retry) -> add runloop source -> block" flow.
#[allow(clippy::too_many_arguments)]
pub(crate) fn start_event_tap_thread(
    location: i32,
    placement: i32,
    options: u32,
    mask: CGEventMask,
    callback: CGEventTapCallBack,
    user_info: usize,
    log_name: &'static str,
    control: &'static TapThreadControl,
    on_started: impl FnOnce() + Send + 'static,
) -> thread::JoinHandle<()> {
    control.prepare_start();
    thread::spawn(move || unsafe {
        crate::performance::set_current_thread_qos(crate::performance::ThreadQos::UserInteractive);
        let created = create_tap_with_retry(
            location,
            placement,
            options,
            mask,
            callback,
            user_info as *mut c_void,
            log_name,
            Some(control.cancel_flag()),
        );

        let Some(created) = created else {
            return;
        };

        let run_loop = CFRunLoopGetCurrent();
        control.register(created.tap, run_loop);

        // Watchdog: the system may disable the tap during busy startup or under a debugger;
        // attach a periodic check that self-heals it.
        let watchdog = start_tap_watchdog(created.tap, control.cancel_flag());
        on_started();
        if !control.stop_requested.load(Ordering::SeqCst) && crate::input_monitor::taps_allowed() {
            CFRunLoopRun();
        }
        stop_tap_watchdog(watchdog);
        CGEventTapEnable(created.tap, false);
        control.clear(created.tap);
        teardown_event_tap(run_loop, created);
    })
}

// See CGEventTypes.h. Scroll reversal flips 4 field groups to cover all consumer types.

/// Vertical scroll delta (integer, line-level). field 11.
pub(crate) const K_CG_SCROLL_WHEEL_EVENT_DELTA_AXIS_1: i32 = 11;
/// Horizontal scroll delta (integer, line-level). field 12.
#[allow(dead_code)]
pub(crate) const K_CG_SCROLL_WHEEL_EVENT_DELTA_AXIS_2: i32 = 12;

/// Vertical scroll delta (fixed-point, 16.16 format). field 93.
#[allow(dead_code)]
pub(crate) const K_CG_SCROLL_WHEEL_EVENT_FIXED_PT_DELTA_AXIS_1: i32 = 93;
/// Horizontal scroll delta (fixed-point, 16.16 format). field 94.
#[allow(dead_code)]
pub(crate) const K_CG_SCROLL_WHEEL_EVENT_FIXED_PT_DELTA_AXIS_2: i32 = 94;

/// Vertical scroll delta (pixel-level). field 96.
#[allow(dead_code)]
pub(crate) const K_CG_SCROLL_WHEEL_EVENT_POINT_DELTA_AXIS_1: i32 = 96;
/// Horizontal scroll delta (pixel-level). field 97.
#[allow(dead_code)]
pub(crate) const K_CG_SCROLL_WHEEL_EVENT_POINT_DELTA_AXIS_2: i32 = 97;

/// Whether the event is continuous (pixel-level) scroll. field 88. 0=discrete (line), 1=continuous (trackpad).
pub(crate) const K_CG_SCROLL_WHEEL_EVENT_IS_CONTINUOUS: i32 = 88;

// IOHIDEvent-level scroll fields (private API). kIOHIDEventTypeScroll=6, field = (type<<16)|offset.
/// IOHIDEvent vertical scroll field.
#[allow(dead_code)]
pub(crate) const K_IOHID_EVENT_FIELD_SCROLL_X: u32 = 6 << 16;
/// IOHIDEvent horizontal scroll field.
#[allow(dead_code)]
pub(crate) const K_IOHID_EVENT_FIELD_SCROLL_Y: u32 = (6 << 16) | 1;

/// CGEventPost tap location: kCGSessionEventTap=1.
/// Synthetic events posted at session level bypass HID-level taps, avoiding the system's
/// natural-scroll override at the HID layer.
pub(crate) const K_CG_SESSION_EVENT_TAP: i32 = 1;

/// CGEventCreateScrollWheelEvent2 units: kCGScrollEventUnitLine=1 (line-level, discrete scroll).
pub(crate) const K_CG_SCROLL_EVENT_UNIT_LINE: u32 = 1;

/// eventSourceUserData field (field 42). Used to tag synthetic events so our own tap can
/// recognize and skip them, preventing infinite loops.
pub(crate) const K_CG_EVENT_SOURCE_USER_DATA: i32 = 42;

/// eventSourceUnixProcessID field (field 41). Non-zero = the event was injected via CGEventPost by
/// that process; 0 = hardware. It tells a software-KVM virtual pointer (e.g. Deskflow) apart from a
/// real device: injected events carry no IOHIDEvent sender (hardware button events often don't
/// either), so this field is the only reliable signal.
/// Measured: injected events carry the injecting process's pid and sourceStateID = 0 (private);
/// hardware events carry pid = 0 and state = 1.
pub(crate) const K_CG_EVENT_SOURCE_UNIX_PROCESS_ID: i32 = 41;

/// Synthetic-event marker magic (ASCII "OMTSCRL"). Written to eventSourceUserData so our tap
/// can recognize and skip our own synthetic events.
#[allow(clippy::unusual_byte_groupings)]
pub(crate) const SYNTHETIC_MARKER: i64 = 0x4F4D_5453_4352_4C;
