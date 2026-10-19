mod app_identity;
mod autostart;
mod callback_guard;
mod clipboard;
mod clipboard_highlight;
mod config;
mod dev_backdrop;
mod dev_flags;
mod e2e_state;
mod event_monitor;
mod event_tap;
mod ffi;
mod glass;
mod hash;
mod i18n;
mod icon_cache;
mod input_monitor;
mod key_bindings;
mod keystroke_display;
mod language_tolerance;
mod logger;
mod mem;
mod menu;
mod mouse;
mod onboarding;
mod overlay;
mod panel_actions;
mod performance;
mod pointer_locator;
mod quick_actions;
mod restart;
mod runtime_config;
mod scroll_indicator_math;
mod scroller;
mod settings;
mod single_instance;
mod skylight;
mod space_groups;
mod space_swipe;
mod space_transition;
mod theme;
mod thumbnail;
mod ui;
mod ui_coordinator;
mod update_notice;
mod updater;
mod window_collector;
mod window_management;
mod window_refresh;
mod window_server;

use config::CONFIG;
use i18n::t;
// FFI primitives (make_nsstring/release_obj/CFRelease/ObjPtr/color+layer helpers) live in ffi.rs
use ffi::*;
// Theme and layout (Colors/colors/card+window size accessors/STATUS_H/H_PADDING) live in theme.rs
use theme::*;
// Switcher overlay & card UI (overlay state/card index/callbacks/rendering/activation) live in overlay.rs
use overlay::*;
// Status bar menu (menu-item state/action callbacks/title refresh) live in menu.rs
use menu::*;
// Settings window (control builders/window build-show-collect/validation alerts/hot config apply) live in settings.rs
use objc2::runtime::{AnyObject, Sel};
use objc2::{class, msg_send, sel};
use objc2_foundation::{NSPoint, NSRect, NSSize};
use settings::*;
use std::cell::RefCell;
use std::collections::{HashSet, VecDeque};
use std::ffi::{c_void, CString};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Mutex, OnceLock};
use std::thread;

use event_monitor::{start as start_event_monitor, GlobalEvent};
use icon_cache::{
    cache_running_app_icons, cache_running_app_icons_small, ensure_icon_cache_dir,
    extract_icon_to_cache, migrate_legacy_cache,
};
use window_collector::{
    app_activation_is_current, clear_ax_window_cache_for_pid, focused_window_cgwid,
    note_app_activated, note_app_terminated, remove_pid_mru, MruMap, WindowInfo,
};

// FFI declarations and ObjC bridging primitives moved to `ffi.rs`

// layout constants moved to `theme.rs`

// Cross-module shared app state (pub(crate) so overlay/menu/settings can access).
// AppState + TAB_STATE stay in main.rs to avoid an overlay<->menu dependency cycle.
pub(crate) struct AppState {
    pub(crate) windows: Vec<WindowInfo>,
    pub(crate) selected: usize,
    pub(crate) visible: bool,
    pub(crate) mru: MruMap,
    pub(crate) focus_key: Option<(i32, u32)>,
    // Window keys the model already held at summon. Refreshes after the overlay shows only treat
    // windows present at the summon as the user's switching targets; a window appearing after the
    // summon is a newcomer and does not participate in the pick.
    // None = not yet recorded (before the first frame).
    pub(crate) summon_keys: Option<HashSet<(i32, u32)>>,
    // false = the selection is still the first-frame default and follows the summon semantics,
    // not the live MRU order; true = the user picked a concrete window and the selection must
    // stay pinned to it across refreshes.
    pub(crate) user_picked: bool,
    // The user's current selection target key. When user_picked=true it is the window the pick
    // must stay pinned to after a refresh; when false it is the first-frame default target and a
    // refresh must not re-pick just because the MRU order shifted.
    pub(crate) selected_target_key: Option<(i32, u32)>,
    // pending_first_show: true once this summon has kicked off a refresh but the overlay has not
    // been shown yet; apply_window_refresh consumes it to show once the first snapshot is ready
    // (single-shot render, avoiding the "show stale snapshot then reorder" jump).
    pub(crate) pending_first_show: bool,
    // backward flag captured while the first frame is pending, so apply_window_refresh can decide
    // the initial pick direction when it finally shows.
    pub(crate) pending_first_backward: bool,
    // Cmd release received while the first snapshot is pending; committed when that snapshot
    // arrives instead of being discarded because the overlay is not visible yet.
    pub(crate) pending_first_release: bool,
}

impl AppState {
    pub(crate) fn new() -> Self {
        // Seed the MRU from the system's front-to-back window order at startup, so the
        // initial ordering after a restart matches the native app-level Cmd+Tab order.
        let mut mru = window_collector::seed_mru_from_system_order();
        let windows = if has_accessibility_permission() {
            let windows = window_collector::collect_windows(&mut mru);
            // The startup collection opens a scope pass like any other; this is the only consumer of
            // it, so it must be consumed or its slot would sit in this (long-lived) thread pinning
            // the oldest destruction epoch, which would stop the forget marks from ever healing.
            if let Some(pass) = crate::thumbnail::take_current_scope_pass() {
                pass.publish();
            }
            // The startup collection's AX evidence is the current truth too; it has no refresh result
            // to travel in, so publish it here as a full pass.
            crate::e2e_state::publish_ax_pid_evidence(
                crate::e2e_state::take_staged_ax_pid_evidence(),
                None,
            );
            windows
        } else {
            Vec::new()
        };
        if !has_accessibility_permission() {
            log_info!("No accessibility permission.");
            log_info!("Go to System Settings → Privacy & Security → Accessibility");
        }
        let win_count = windows.len();
        AppState {
            windows,
            selected: if win_count > 1 { 1 } else { 0 },
            visible: false,
            mru,
            focus_key: None,
            summon_keys: None,
            user_picked: false,
            selected_target_key: None,
            pending_first_show: false,
            pending_first_backward: false,
            pending_first_release: false,
        }
    }
}

// moved to `theme.rs`

// moved to `ffi.rs`

/// Runtime state owned exclusively by the AppKit main thread.
pub(crate) struct AppRuntime {
    pub(crate) switcher: Option<AppState>,
}

thread_local! {
    static APP_RUNTIME: RefCell<AppRuntime> = const { RefCell::new(AppRuntime { switcher: None }) };
}

/// Borrow the main-thread runtime for one short, synchronous state transition.
pub(crate) fn with_tab_state<R>(f: impl FnOnce(&mut Option<AppState>) -> R) -> R {
    debug_assert_main_thread();
    APP_RUNTIME.with(|runtime| f(&mut runtime.borrow_mut().switcher))
}

/// UI runtime must never be accessed from a worker thread.
pub(crate) fn debug_assert_main_thread() {
    #[cfg(debug_assertions)]
    unsafe {
        let is_main: bool = msg_send![class!(NSThread), isMainThread];
        debug_assert!(
            is_main,
            "main-thread runtime accessed off the AppKit main thread"
        );
    }
}

/// Runtime main-thread check (also active in release builds): cross-thread callbacks use it
/// to hop back onto the main thread.
pub(crate) fn is_main_thread() -> bool {
    unsafe { msg_send![class!(NSThread), isMainThread] }
}

/// Neutral app-level entry points used to avoid UI-module dependency cycles.
pub(crate) fn close_settings_for_switcher() {
    settings::close_settings_from_switcher();
}

pub(crate) fn quit_from_settings() {
    menu::quit_application();
}

/// Main-thread-published window-count snapshot. Background diagnostics read this value
/// instead of touching the switcher runtime.
pub(crate) static WINDOW_COUNT: AtomicUsize = AtomicUsize::new(0);
/// Cross-thread callback target used only to schedule work back onto AppKit's main thread.
pub(crate) static CONTROLLER: Mutex<Option<CallbackTarget>> = Mutex::new(None);
static SMOOTH_E2E_RECORD_SCHEDULED: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// Seconds after launch before the settings window is built ahead of its first open. Late enough that the
/// menu bar, the global event tap and the update check have settled, and late enough that a user's first
/// scroll is not stuttered by the build: measured on the A2 smooth-scroll scenario, the stream lost its
/// momentum_ended phase in 2 of 3 runs with the warm-up at 1.0s, in 1 of 3 with the motion gate alone, and
/// in 0 of 6 with the motion gate at 3.0s. Still early enough that the window is ready before a user can
/// reasonably reach the picker's gear or the menu item.
const SETTINGS_PREWARM_DELAY: f64 = 3.0;
/// Retry spacing, and the attempt cap, for a firing that must wait: the first-run guide owns the screen, or a
/// smooth-scroll stream is being emitted. Prewarming under either would be wrong (window the user must not see
/// yet / a stuttered scroll), so the warm-up retries a bounded number of times: 3.0s + 5 x 4.0s ≈ 23s of
/// coverage, after which a launch that never went quiet simply opens cold.
const SETTINGS_PREWARM_RETRY_DELAY: f64 = 4.0;
static SETTINGS_PREWARM_ATTEMPTS: AtomicUsize = AtomicUsize::new(0);
const SETTINGS_PREWARM_MAX_ATTEMPTS: usize = 6;
static SETTINGS_PREWARM_SCHEDULED: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// Shared ObjC target object for menu items and settings buttons.
pub(crate) static MENU_TARGET: MainThreadSlot<Option<ObjPtr>> = MainThreadSlot::new(None);

/// Cross-thread dispatch handle for `MENU_TARGET`. The recording thread wakes the main-thread
/// callback via performSelectorOnMainThread from the background, but MENU_TARGET is a
/// main-thread-only slot -- reading it off-main trips the main-thread assertion in debug builds,
/// and because the caller is an extern "C" event-tap callback the panic cannot unwind, aborting the
/// process (measured: recording a side button always crashed). This holds the same
/// process-lifetime target object, used only to dispatch to the main thread, never to touch UI.
pub(crate) static MENU_TARGET_DISPATCH: Mutex<Option<CallbackTarget>> = Mutex::new(None);
const GLOBAL_INPUT_CAPACITY: usize = 32;

/// Bounded aggregate for global input events. Tab presses keep order, release is latched
/// separately, and low-value control events use a finite queue so a blocked main thread cannot
/// cause the CGEventTap backlog to grow without bound.
struct PendingGlobalInput {
    tab_steps: VecDeque<bool>,
    release_pending: bool,
    clipboard_pending: bool,
    keystroke_display_pending: bool,
    other: VecDeque<GlobalEvent>,
}

impl PendingGlobalInput {
    fn push(&mut self, event: GlobalEvent) {
        match event {
            GlobalEvent::CmdTabPressed | GlobalEvent::CmdShiftTabPressed => {
                let backward = matches!(event, GlobalEvent::CmdShiftTabPressed);
                if self.tab_steps.len() >= GLOBAL_INPUT_CAPACITY {
                    self.tab_steps.pop_front();
                    log_debug!("[kbd] coalesced oldest tab step (input queue full)");
                }
                self.tab_steps.push_back(backward);
            }
            GlobalEvent::CmdReleased => {
                // Release must not be dropped: even when presses are coalesced, the main thread
                // must observe the modifier release.
                self.release_pending = true;
            }
            GlobalEvent::ClipboardToggled => {
                self.clipboard_pending = true;
            }
            GlobalEvent::KeystrokeDisplayWake => {
                self.keystroke_display_pending = true;
            }
            GlobalEvent::BindingAction { .. } => {
                if self.other.len() >= GLOBAL_INPUT_CAPACITY {
                    self.other.pop_front();
                    log_debug!("[kbd] dropped oldest auxiliary input (queue full)");
                }
                self.other.push_back(event);
            }
        }
    }
}

static PENDING_GLOBAL_INPUT: OnceLock<Mutex<PendingGlobalInput>> = OnceLock::new();
static GLOBAL_INPUT_DRAIN_SCHEDULED: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// Non-blockingly aggregate input on the event-tap thread and schedule at most one main-thread
/// drain callback.
pub(crate) fn enqueue_global_event(event: GlobalEvent) {
    let pending = PENDING_GLOBAL_INPUT.get_or_init(|| {
        Mutex::new(PendingGlobalInput {
            tab_steps: VecDeque::new(),
            release_pending: false,
            clipboard_pending: false,
            keystroke_display_pending: false,
            other: VecDeque::new(),
        })
    });
    pending.lock().unwrap().push(event);
    schedule_global_input_drain();
}

fn schedule_global_input_drain() {
    if GLOBAL_INPUT_DRAIN_SCHEDULED.swap(true, std::sync::atomic::Ordering::AcqRel) {
        return;
    }
    let Some(controller) = CONTROLLER.lock().unwrap().map(|ptr| ptr.0) else {
        GLOBAL_INPUT_DRAIN_SCHEDULED.store(false, std::sync::atomic::Ordering::Release);
        return;
    };
    unsafe {
        let _: () = msg_send![
            controller,
            performSelectorOnMainThread: sel!(handleGlobalInputDrain:),
            withObject: std::ptr::null::<AnyObject>(),
            waitUntilDone: false
        ];
    }
}

/// Main-thread consumer for the development text-hide: the panel's text fields are hidden, so a capture taken
/// after this has the same panel, the same layout and the same material as one taken before it, without the
/// glyphs. That is what lets the A2 contrast scenario isolate glyphs from the material's own tonal noise.
extern "C" fn on_dev_blank_text(_self: *mut c_void, _cmd: Sel, _arg: *mut c_void) {
    callback_guard::void("on_dev_blank_text", || {
        crate::clipboard::dev_hide_picker_text();
    });
}

/// Hide the picker's text `delay` after this call, on the main thread. Development only, and deliberately not
/// immediate: a capture taken before the delay is the "with text" frame, so both frames come from this one
/// process (two launches of a translucent material do not re-render identically, which is what broke the
/// earlier two-launch form of that measurement).
/// Open the clipboard picker after a delay (see `show_clipboard_delay`), through the same
/// main-thread controller entry the blank-text switch uses.
pub(crate) fn schedule_dev_show_clipboard(delay: std::time::Duration) {
    std::thread::spawn(move || {
        std::thread::sleep(delay);
        let Some(controller) = CONTROLLER.lock().unwrap().map(|ptr| ptr.0) else {
            return;
        };
        unsafe {
            let _: () = msg_send![
                controller,
                performSelectorOnMainThread: sel!(handleDevShowClipboard:),
                withObject: std::ptr::null::<AnyObject>(),
                waitUntilDone: false
            ];
        }
    });
}

extern "C" fn on_dev_show_clipboard(_this: *mut c_void, _cmd: Sel, _sender: *mut c_void) {
    callback_guard::void("on_dev_show_clipboard", || {
        crate::clipboard::show_picker_for_development();
    });
}

pub(crate) fn schedule_dev_blank_text(delay: std::time::Duration) {
    std::thread::spawn(move || {
        std::thread::sleep(delay);
        let Some(controller) = CONTROLLER.lock().unwrap().map(|ptr| ptr.0) else {
            return;
        };
        unsafe {
            let _: () = msg_send![
                controller,
                performSelectorOnMainThread: sel!(handleDevBlankText:),
                withObject: std::ptr::null::<AnyObject>(),
                waitUntilDone: false
            ];
        }
    });
}

/// Ask the controller to write smooth-scroll e2e counters from the main thread. The tap timer
/// updates only atomics; the snapshot borrows AppState and must remain on AppKit's thread.
pub(crate) fn schedule_smooth_scroll_e2e_record() {
    if !e2e_state::is_enabled()
        || SMOOTH_E2E_RECORD_SCHEDULED.swap(true, std::sync::atomic::Ordering::AcqRel)
    {
        return;
    }
    let Some(controller) = CONTROLLER.lock().unwrap().map(|target| target.0) else {
        SMOOTH_E2E_RECORD_SCHEDULED.store(false, std::sync::atomic::Ordering::Release);
        return;
    };
    unsafe {
        let _: () = msg_send![
            controller,
            performSelectorOnMainThread: sel!(handleSmoothScrollE2eRecord:),
            withObject: std::ptr::null::<AnyObject>(),
            waitUntilDone: false
        ];
    }
}

extern "C" fn on_smooth_scroll_e2e_record(_this: *mut c_void, _cmd: Sel, _sender: *mut c_void) {
    callback_guard::void("on_smooth_scroll_e2e_record", || {
        SMOOTH_E2E_RECORD_SCHEDULED.store(false, std::sync::atomic::Ordering::Release);
        e2e_state::record("smooth_scroll_complete");
    });
}

/// Main-thread consumer for one input batch; release is applied last so rapid Tab presses retain
/// their existing commit semantics.
extern "C" fn on_global_input_drain(_self: *mut c_void, _cmd: Sel, _arg: *mut c_void) {
    callback_guard::void("on_global_input_drain", || {
        on_global_input_drain_inner();
    });
}

fn on_global_input_drain_inner() {
    let Some(pending) = PENDING_GLOBAL_INPUT.get() else {
        return;
    };
    let (tab_steps, clipboard_pending, keystroke_display_pending, other, release_pending) = {
        let mut state = pending.lock().unwrap();
        let tab_steps = std::mem::take(&mut state.tab_steps);
        let clipboard_pending = state.clipboard_pending;
        state.clipboard_pending = false;
        let keystroke_display_pending = state.keystroke_display_pending;
        state.keystroke_display_pending = false;
        let other = std::mem::take(&mut state.other);
        let release_pending = state.release_pending;
        state.release_pending = false;
        GLOBAL_INPUT_DRAIN_SCHEDULED.store(false, std::sync::atomic::Ordering::Release);
        (
            tab_steps,
            clipboard_pending,
            keystroke_display_pending,
            other,
            release_pending,
        )
    };

    for backward in tab_steps {
        if backward {
            on_cmd_shift_tab_pressed(
                std::ptr::null_mut(),
                sel!(handleCmdShiftTabPressed:),
                std::ptr::null_mut(),
            );
        } else {
            on_cmd_tab_pressed(
                std::ptr::null_mut(),
                sel!(handleCmdTabPressed:),
                std::ptr::null_mut(),
            );
        }
    }
    if clipboard_pending {
        clipboard::on_clipboard_toggle(
            std::ptr::null_mut(),
            sel!(onClipboardToggled:),
            std::ptr::null_mut(),
        );
    }
    if keystroke_display_pending {
        keystroke_display::wake();
    }
    for event in other {
        match event {
            // Every binding action -- window control, display moves, quick actions and the system
            // and feature actions -- runs through this one main-thread entry.
            GlobalEvent::BindingAction { action, trigger } => {
                key_bindings::execute_authorized(action, trigger)
            }
            GlobalEvent::CmdTabPressed
            | GlobalEvent::CmdShiftTabPressed
            | GlobalEvent::CmdReleased
            | GlobalEvent::ClipboardToggled
            | GlobalEvent::KeystrokeDisplayWake => {}
        }
    }
    if release_pending {
        on_cmd_released(
            std::ptr::null_mut(),
            sel!(handleCmdReleased:),
            std::ptr::null_mut(),
        );
    }

    let has_pending = {
        let state = pending.lock().unwrap();
        !state.tab_steps.is_empty()
            || state.clipboard_pending
            || state.keystroke_display_pending
            || !state.other.is_empty()
            || state.release_pending
    };
    if !has_pending {
        return;
    }
    schedule_global_input_drain();
}

// make_nsstring / release_obj / has_accessibility_permission / hex_to_*color / layer_set_* moved to `ffi.rs`

// colors_from_config / system_dark_mode / current_colors / card_* / icon_px / letter_px /
// colors_from_config / system_dark_mode / current_colors / card_* / icon_px / letter_px /
// window_height / window_width moved to `theme.rs`

extern "C" fn on_app_activated(_self: *mut c_void, _cmd: Sel, notification: *mut c_void) {
    callback_guard::void("on_app_activated", || on_app_activated_inner(notification));
}

extern "C" fn on_active_space_changed(_self: *mut c_void, _cmd: Sel, _notification: *mut c_void) {
    callback_guard::void("on_active_space_changed", || {
        window_refresh::note_space_transition(true)
    });
}

/// This app became active. Published for the binding engine's tap thread, which has to decide whether
/// a window action's chord belongs to a focused control here rather than to a window (see
/// `key_bindings::publish_self_frontmost`).
extern "C" fn on_self_became_active(_self: *mut c_void, _cmd: Sel, _notification: *mut c_void) {
    callback_guard::void("on_self_became_active", || {
        key_bindings::publish_self_frontmost(true);
    });
}

/// This app resigned active: the frontmost app is someone else again.
extern "C" fn on_self_resigned_active(_self: *mut c_void, _cmd: Sel, _notification: *mut c_void) {
    callback_guard::void("on_self_resigned_active", || {
        key_bindings::publish_self_frontmost(false);
    });
}

fn on_app_activated_inner(notification: *mut c_void) {
    settings::refresh_permission_ui_if_visible();
    unsafe {
        let user_info: *mut AnyObject = msg_send![notification as *mut AnyObject, userInfo];
        if user_info.is_null() {
            return;
        }
        let key = make_nsstring("NSWorkspaceApplicationKey");
        let app: *mut AnyObject = msg_send![user_info, objectForKey: key];
        CFRelease(key as *const c_void);
        if app.is_null() {
            return;
        }
        // Clipboard target tracking above accepts Accessory apps, which may own input windows.
        // The switcher's window/MRU and icon paths intentionally handle only Regular apps.
        let policy: i64 = msg_send![app, activationPolicy];
        let pid: i32 = msg_send![app, processIdentifier];
        clipboard::note_external_app_activated(pid, policy);
        if policy != 0 {
            return;
        }
        // Record this app activation's token: newly discovered windows use it as their initial
        // MRU, and async queries use it to reject stale results. Existing windows are never
        // updated together from this app-level timestamp.
        let activated_at = note_app_activated(pid);
        let window_ids = window_server::window_ids_for_pid(pid);
        window_server::begin_activation(pid, &window_ids, activated_at);
        // Resolve the focused window's CGWindowID off-main and bump window MRU.
        // External focus switches (system Cmd+Tab, Dock clicks) feed into window
        // ordering through this path. The kAXFocusedWindow AX query can block up
        // to 50ms (target app unresponsive), so it must run off the main thread.
        schedule_activation_focus(pid, activated_at);
    }
}

struct ActivationFocusTask {
    pid: i32,
    activated_at: std::time::Instant,
}

static ACTIVATION_FOCUS_TX: OnceLock<flume::Sender<ActivationFocusTask>> = OnceLock::new();

/// Value-only result delivered back to the main thread after the background AX query; worker
/// threads never mutate window MRU/focus state directly.
#[derive(Clone, Copy)]
struct ActivationFocusResult {
    pid: i32,
    cgwid: u32,
    activated_at: std::time::Instant,
    attempt: usize,
}

static ACTIVATION_FOCUS_RESULTS: OnceLock<Mutex<Vec<ActivationFocusResult>>> = OnceLock::new();
static ACTIVATION_FOCUS_RESULT_SCHEDULED: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// Start a fixed-size AX focus queue so repeated app activations cannot create an
/// unbounded number of short-lived threads.
fn start_activation_focus_scheduler() {
    let (tx, rx) = flume::bounded::<ActivationFocusTask>(32);
    let _ = ACTIVATION_FOCUS_TX.set(tx);
    let _ = ACTIVATION_FOCUS_RESULTS.set(Mutex::new(Vec::new()));
    for worker in 0..2 {
        let rx = rx.clone();
        thread::Builder::new()
            .name(format!("activation-focus-{worker}"))
            .spawn(move || {
                performance::set_current_thread_qos(performance::ThreadQos::Utility);
                while let Ok(task) = rx.recv() {
                    resolve_activation_focus(task);
                }
            })
            .expect("spawn activation-focus worker");
    }
}

/// Schedule one main-thread callback to consume activation-focus results; results from
/// multiple workers are coalesced into one batch.
fn schedule_activation_focus_result(result: ActivationFocusResult) {
    let Some(results) = ACTIVATION_FOCUS_RESULTS.get() else {
        return;
    };
    results.lock().unwrap().push(result);
    if ACTIVATION_FOCUS_RESULT_SCHEDULED.swap(true, std::sync::atomic::Ordering::AcqRel) {
        return;
    }
    let Some(controller) = CONTROLLER.lock().unwrap().map(|ptr| ptr.0) else {
        ACTIVATION_FOCUS_RESULT_SCHEDULED.store(false, std::sync::atomic::Ordering::Release);
        return;
    };
    unsafe {
        let _: () = msg_send![
            controller,
            performSelectorOnMainThread: sel!(handleActivationFocusResult:),
            withObject: std::ptr::null::<AnyObject>(),
            waitUntilDone: false
        ];
    }
}

/// Main-thread consumer for background focus-query results. It updates AppState and starts
/// the activation thumbnail refresh from one serialized state transition.
extern "C" fn on_activation_focus_result(_self: *mut c_void, _cmd: Sel, _arg: *mut c_void) {
    let Some(results) = ACTIVATION_FOCUS_RESULTS.get() else {
        return;
    };
    let batch = {
        let mut pending = results.lock().unwrap();
        let batch = std::mem::take(&mut *pending);
        ACTIVATION_FOCUS_RESULT_SCHEDULED.store(false, std::sync::atomic::Ordering::Release);
        batch
    };
    for result in batch {
        with_tab_state(|state_opt| {
            if let Some(state) = state_opt.as_mut() {
                window_refresh::best_effort_bump_focus_key(state, result.pid, result.cgwid);
                log_debug!(
                    "app-activated bump: pid={} cgwid={} attempt={}",
                    result.pid,
                    result.cgwid,
                    result.attempt
                );
            }
        });
        thumbnail::refresh_after_activation(result.pid, result.cgwid, result.activated_at);
    }
    // A result may arrive just after take; check and schedule another batch so the flag
    // transition cannot strand a notification.
    if results.lock().unwrap().is_empty() {
        return;
    }
    if !ACTIVATION_FOCUS_RESULT_SCHEDULED.swap(true, std::sync::atomic::Ordering::AcqRel) {
        let Some(controller) = CONTROLLER.lock().unwrap().map(|ptr| ptr.0) else {
            ACTIVATION_FOCUS_RESULT_SCHEDULED.store(false, std::sync::atomic::Ordering::Release);
            return;
        };
        unsafe {
            let _: () = msg_send![
                controller,
                performSelectorOnMainThread: sel!(handleActivationFocusResult:),
                withObject: std::ptr::null::<AnyObject>(),
                waitUntilDone: false
            ];
        }
    }
}

fn schedule_activation_focus(pid: i32, activated_at: std::time::Instant) {
    let Some(tx) = ACTIVATION_FOCUS_TX.get() else {
        return;
    };
    if tx
        .try_send(ActivationFocusTask { pid, activated_at })
        .is_err()
    {
        log_debug!("activation focus queue full or stopped: pid={}", pid);
    }
}

fn resolve_activation_focus(task: ActivationFocusTask) {
    unsafe {
        let pool: *mut AnyObject = msg_send![class!(NSAutoreleasePool), new];
        // Log to diagnose MRU bumping: failures are handled with bounded retries only.
        let retry_delays_ms = [0_u64, 50, 150, 300, 700, 1_000];
        let mut bumped = false;
        for (attempt, delay_ms) in retry_delays_ms.into_iter().enumerate() {
            if delay_ms > 0 {
                std::thread::sleep(std::time::Duration::from_millis(delay_ms));
            }
            if !app_activation_is_current(task.pid, task.activated_at) {
                break;
            }
            let workspace: *mut AnyObject = msg_send![class!(NSWorkspace), sharedWorkspace];
            let front_app: *mut AnyObject = msg_send![workspace, frontmostApplication];
            if front_app.is_null() {
                continue;
            }
            let front_pid: i32 = msg_send![front_app, processIdentifier];
            if front_pid != task.pid {
                break;
            }
            if let Some(cgwid) = focused_window_cgwid(task.pid) {
                if !window_server::ax_focus_backstop_allowed(task.pid) {
                    // This branch = the focus slot is already consumed: usually the
                    // echo of our own switcher raise (MRU was committed by the commit
                    // path; the backstop must not bump again), sometimes an external
                    // activation whose 808 arrived first. Both used to silently lose
                    // the post-activation thumbnail refresh, leaving a just-switched-to
                    // window without one; the MRU silencing stays, only the refresh is
                    // let through.
                    thumbnail::refresh_after_activation(task.pid, cgwid, task.activated_at);
                    bumped = true;
                    break;
                }
                schedule_activation_focus_result(ActivationFocusResult {
                    pid: task.pid,
                    cgwid,
                    activated_at: task.activated_at,
                    attempt: attempt + 1,
                });
                bumped = true;
                break;
            }
        }
        if !bumped {
            log_debug!(
                "app-activated bump: pid={} (no focused window / stale activation / AX timeout)",
                task.pid
            );
        }
        let _: () = msg_send![pool, drain];
    }
}

extern "C" fn on_app_launched(_self: *mut c_void, _cmd: Sel, notification: *mut c_void) {
    callback_guard::void("on_app_launched", || on_app_launched_inner(notification));
}

fn on_app_launched_inner(notification: *mut c_void) {
    // Pre-cache the launched app's icon so it's on disk before the user summons
    // the switcher. Run off the main thread (with an autorelease pool) so the
    // launch notification doesn't block the UI; extract_icon_to_cache is
    // defensive (null-safe) and a failure simply leaves the letter-icon fallback.
    let pid: i32 = unsafe {
        let user_info: *mut AnyObject = msg_send![notification as *mut AnyObject, userInfo];
        if user_info.is_null() {
            return;
        }
        let key = make_nsstring("NSWorkspaceApplicationKey");
        let app: *mut AnyObject = msg_send![user_info, objectForKey: key];
        CFRelease(key as *const c_void);
        if app.is_null() {
            return;
        }
        msg_send![app, processIdentifier]
    };
    if pid <= 0 {
        return;
    }
    thread::Builder::new()
        .name("app-launch-icon".into())
        .spawn(move || unsafe {
            performance::set_current_thread_qos(performance::ThreadQos::Utility);
            let pool: *mut AnyObject = msg_send![class!(NSAutoreleasePool), new];
            match extract_icon_to_cache(pid) {
                Some(_) => log_debug!("app-launch icon cached: pid={}", pid),
                None => {
                    // The icon may not be ready the instant the app launches (app.icon nil),
                    // silently failing the extract and leaving the letter placeholder in the
                    // switcher. Retry once after ~1s.
                    log_debug!(
                        "app-launch icon extract failed for pid={}, retrying in 1s",
                        pid
                    );
                    std::thread::sleep(std::time::Duration::from_secs(1));
                    let _ = extract_icon_to_cache(pid);
                }
            }
            let _: () = msg_send![pool, drain];
        })
        .expect("spawn app-launch-icon thread");
    // Thumbnails: install the new app's AXObserver and pre-generate its windows.
    thumbnail::app_launched(pid);
}

/// Forwarding point for NSApplicationWillTerminateNotification (main thread): restore the macOS
/// system pointer values and, when configured, clear the clipboard history before quitting,
/// matching LinearMouse's applicationWillTerminate -- otherwise the linear switch and our tracking
/// speed stay on the device (Cmd+Q, logout and shutdown all land here; the menu-quit path has an
/// equivalent call of its own).
extern "C" fn on_will_terminate(_self: *mut c_void, _cmd: Sel, _notification: *mut c_void) {
    callback_guard::void("on_will_terminate", || {
        crate::mouse::pointer::restore();
        // Cmd+Q, the menu-bar Quit item, logout and shutdown all land here, so one place covers
        // every orderly exit (a crash or SIGKILL posts no notification, by design).
        if crate::clipboard::clear_on_quit_enabled() {
            crate::clipboard::discard_history_on_disk();
        }
    });
}

/// Forwarding point for NSWorkspaceDidTerminateApplicationNotification (main
/// thread): tells the thumbnail module to cancel captures, clear cached frames,
/// and uninstall that app's observer.
extern "C" fn on_app_terminated(_self: *mut c_void, _cmd: Sel, notification: *mut c_void) {
    callback_guard::void("on_app_terminated", || {
        on_app_terminated_inner(notification)
    });
}

fn on_app_terminated_inner(notification: *mut c_void) {
    let pid: i32 = unsafe {
        let user_info: *mut AnyObject = msg_send![notification as *mut AnyObject, userInfo];
        if user_info.is_null() {
            return;
        }
        let key = make_nsstring("NSWorkspaceApplicationKey");
        let app: *mut AnyObject = msg_send![user_info, objectForKey: key];
        CFRelease(key as *const c_void);
        if app.is_null() {
            return;
        }
        msg_send![app, processIdentifier]
    };
    if pid > 0 {
        // Termination clears the activation token and this PID's window MRUs immediately,
        // invalidating in-flight retries and preventing PID/CGWindowID reuse contamination.
        note_app_terminated(pid);
        crate::space_groups::with_tracker_mut(|tracker| tracker.remove_process(pid));
        clear_ax_window_cache_for_pid(pid);
        with_tab_state(|state_opt| {
            if let Some(state) = state_opt.as_mut() {
                let removed = remove_pid_mru(&mut state.mru, pid);
                if state
                    .focus_key
                    .is_some_and(|(focus_pid, _)| focus_pid == pid)
                {
                    state.focus_key = None;
                }
                if removed > 0 {
                    log_debug!(
                        "app-terminated MRU cleanup: pid={} removed={}",
                        pid,
                        removed
                    );
                }
            }
        });
        thumbnail::app_terminated(pid);
    }
}

extern "C" fn on_locale_changed(_self: *mut c_void, _cmd: Sel, _arg: *mut c_void) {
    callback_guard::void("on_locale_changed", || on_locale_changed_inner(_self));
}

/// Scroller-style change callback for `NSScrollerPreferredScrollerStyleDidChangeNotification` (also
/// registered for window-did-become-key / app-did-become-active, which are the ones that actually
/// fire -- see the registration site).
pub(crate) extern "C" fn on_scroller_style_changed(
    _self: *mut AnyObject,
    _cmd: Sel,
    _note: *mut AnyObject,
) {
    unsafe {
        // Notification delivery thread isn't guaranteed to be main, but both re-asserting the style
        // and re-laying out UI must run on main.
        let is_main: bool = msg_send![class!(NSThread), isMainThread];
        // Defer when a settings build is already in progress on this thread: the settings window
        // becoming key (or the app activating) *inside* `with_settings_ui` delivers these
        // notifications synchronously, and the resync below reads the settings UI. Re-entering that
        // borrow panics, and a panic in an `extern "C"` callback aborts the process -- which is how
        // the guide's final-step "Open App Settings" used to crash (2026-09-28, "RefCell already
        // borrowed"). Deferring runs the resync right after the borrow is released.
        if !is_main || crate::settings::settings_ui_borrowed() {
            let _: () = msg_send![_self,
                performSelectorOnMainThread: sel!(handleScrollerStyleChanged:),
                withObject: std::ptr::null::<AnyObject>(),
                waitUntilDone: false
            ];
            return;
        }
    }
    // Diagnostic: log which notification woke us up. A delivery that came through this handler's
    // own performSelectorOnMainThread hop carries no notification object, and messaging nil panics
    // inside an `extern "C"` callback (that aborted the app on 2026-09-28), so the note is
    // optional here.
    if !_note.is_null() {
        let name: *mut AnyObject = unsafe { msg_send![_note, name] };
        crate::log_debug!("[scroller] notification arrived: {}", unsafe {
            crate::ffi::nsstring_to_rust(name)
        });
    }
    // This handler is registered for window-did-become-key too, and that notification is delivered
    // for *any* of our windows. Without this filter the switcher panel becoming key rebuilt the whole
    // settings content synchronously inside the overlay show, so the panel only appeared once that
    // finished. Only the settings window's own activation may drive the resync; an application object
    // (app activation) and the object-less hop above still do.
    let object = if _note.is_null() {
        std::ptr::null_mut::<AnyObject>()
    } else {
        unsafe { msg_send![_note, object] }
    };
    let object_is_window: bool =
        !object.is_null() && unsafe { msg_send![object, isKindOfClass: class!(NSWindow)] };
    if !crate::scroller::resync_applies_to(
        object_is_window,
        object == crate::settings::settings_window_ptr(),
    ) {
        crate::log_debug!("[scroller] resync skipped: window is not the settings window");
        return;
    }
    crate::scroller::note_resync_entered();
    crate::scroller::on_activation_resync();
}

fn on_locale_changed_inner(_self: *mut c_void) {
    unsafe {
        // Notification delivery thread isn't guaranteed to be main, but UI refresh must run on
        // main; when off main, hop to main and re-enter this method.
        let is_main: bool = msg_send![class!(NSThread), isMainThread];
        if !is_main {
            let _: () = msg_send![_self as *mut AnyObject,
                performSelectorOnMainThread: sel!(handleLocaleChanged:),
                withObject: std::ptr::null::<AnyObject>(),
                waitUntilDone: false
            ];
            return;
        }
    }
    // System language changed (NSLocaleCurrentLocaleDidChangeNotification).
    // apply_config_locale only re-resolves when locale is auto (system-derived); it short-circuits
    // for an explicit locale. After re-resolving, refresh menu titles and invalidate the settings
    // window so it rebuilds with the new locale on next open.
    let locale_cfg = CONFIG.read().unwrap().i18n.locale.clone();
    i18n::apply_config_locale(&locale_cfg);
    refresh_menu_titles();
    invalidate_settings_window();
    clipboard::refresh_localized_ui();
}

extern "C" fn on_keyboard_layout_changed(_self: *mut c_void, _cmd: Sel, _arg: *mut c_void) {
    callback_guard::void("on_keyboard_layout_changed", || {
        unsafe {
            let is_main: bool = msg_send![class!(NSThread), isMainThread];
            if !is_main {
                let _: () = msg_send![_self as *mut AnyObject,
                    performSelectorOnMainThread: sel!(handleKeyboardLayoutChanged:),
                    // (the handler below refreshes the layout cache and republishes the panel key
                    // map, so a layout switch cannot leave the overlay matching stale keys)
                    withObject: std::ptr::null::<AnyObject>(),
                    waitUntilDone: false
                ];
                return;
            }
        }
        keystroke_display::mapping::refresh_layout_cache();
    });
}

/// Callback for effective-appearance changes. Follow the system only when theme is `auto`,
/// and marshal all AppKit/UI work back to the main thread.
extern "C" fn on_appearance_changed(_self: *mut c_void, _cmd: Sel, _arg: *mut c_void) {
    callback_guard::void("on_appearance_changed", || {
        on_appearance_changed_inner(_self)
    });
}

fn on_appearance_changed_inner(_self: *mut c_void) {
    unsafe {
        let is_main: bool = msg_send![class!(NSThread), isMainThread];
        if !is_main {
            let _: () = msg_send![_self as *mut AnyObject,
                performSelectorOnMainThread: sel!(handleAppearanceChanged:),
                withObject: std::ptr::null::<AnyObject>(),
                waitUntilDone: false
            ];
            return;
        }
    }

    let follows_system = CONFIG
        .read()
        .map(|config| config.appearance.theme.eq_ignore_ascii_case("auto"))
        .unwrap_or(true);
    if !follows_system {
        return;
    }

    apply_system_appearance_refresh();
}

/// Refresh every surface that owns custom layer colors after the *system* appearance changed (the
/// "auto" theme path). Split out so the panels' surfaces are covered in one place: the keystroke
/// panel's keycaps re-read the palette on every render while its backdrop is painted once, so a
/// missing entry here leaves light keycaps on a dark shell after a system theme switch.
pub(crate) fn apply_system_appearance_refresh() {
    // Each subsystem owns custom layer colors, so update the native window appearance and
    // rebuild any visible content through its existing theme refresh path.
    settings::refresh_system_appearance();
    unsafe {
        clipboard::apply_theme();
        keystroke_display::apply_glass_properties();
    }
    overlay::apply_theme();
}

/// Callback for screen-parameter changes (NSApplicationDidChangeScreenParametersNotification:
/// external/built-in display plug/unplug, resolution or scaling adjustments). Cached
/// thumbnails carry the old configuration's window aspect and pixel height, and the
/// overlay panel's size follows screen geometry; both go through overlay's debounced
/// entry point.
extern "C" fn on_screen_parameters_changed(_self: *mut c_void, _cmd: Sel, _arg: *mut c_void) {
    callback_guard::void("on_screen_parameters_changed", || {
        on_screen_parameters_changed_inner(_self);
    });
}

fn on_screen_parameters_changed_inner(_self: *mut c_void) {
    unsafe {
        // NSApplication posts this on the main thread; keep the existing defensive
        // hop anyway -- marshal back to main when delivered elsewhere.
        let is_main: bool = msg_send![class!(NSThread), isMainThread];
        if !is_main {
            let _: () = msg_send![_self as *mut AnyObject,
                performSelectorOnMainThread: sel!(handleScreenParametersChanged:),
                withObject: std::ptr::null::<AnyObject>(),
                waitUntilDone: false
            ];
            return;
        }
    }
    log_debug!("[display] screen parameters change notification received");
    overlay::schedule_display_reconfiguration_refresh();
}

/// Re-entry point after the mouse-thread plug/unplug callback hops to the main thread via
/// performSelectorOnMainThread: refresh the settings device popup live (a reconnect shows
/// immediately, no OK/reopen needed).
extern "C" fn on_devices_changed(_self: *mut c_void, _cmd: Sel, _arg: *mut c_void) {
    callback_guard::void("on_devices_changed", || on_devices_changed_inner(_self));
}

fn on_devices_changed_inner(_self: *mut c_void) {
    unsafe {
        let is_main: bool = msg_send![class!(NSThread), isMainThread];
        if !is_main {
            let _: () = msg_send![_self as *mut AnyObject,
                performSelectorOnMainThread: sel!(handleDevicesChanged:),
                withObject: std::ptr::null::<AnyObject>(),
                waitUntilDone: false
            ];
            return;
        }
    }
    crate::settings::refresh_device_popup_if_open();
    // A plug/unplug can change which device is selected, so the mappings on screen are stale too. The popup
    // refresh above marks the page dirty; this fills it (no-op unless the mouse page is the one in use and
    // the page really is stale).
    crate::settings::materialize_mouse_page_if_visible();
}

/// Build the settings window during the first idle moment after launch, so the first real open -- the
/// clipboard picker's settings button, the menu item, or the update banner -- only has to select a page and
/// order the window front. Measured cold open was ~1s, almost all of it building the window and filling the
/// newly added mouse page; the prewarm moves that work off the click path.
///
/// Deferred on purpose: the build allocates ~200 views, and running it inside `run()` would delay the menu
/// bar and the update check. Skipped in smoke processes (their runners assert launch-time state and exit)
/// and while the first-run guide is on screen, where it is retried a bounded number of times.
fn schedule_settings_prewarm() {
    if SETTINGS_PREWARM_SCHEDULED.load(Ordering::Acquire) {
        return;
    }
    // The flag is only set for a request that was really handed to the run loop: a missing controller must
    // leave the warm-up armed for the next attempt, not claim it was scheduled.
    if schedule_settings_prewarm_after(SETTINGS_PREWARM_DELAY) {
        SETTINGS_PREWARM_SCHEDULED.store(true, Ordering::Release);
    }
}

/// Hands one delayed `prewarmSettings:` to the run loop; false when there is no controller to target.
fn schedule_settings_prewarm_after(delay: f64) -> bool {
    let Some(controller) = CONTROLLER.lock().unwrap().map(|target| target.0) else {
        return false;
    };
    unsafe {
        let _: () = msg_send![
            controller,
            performSelector: sel!(prewarmSettings:),
            withObject: std::ptr::null::<AnyObject>(),
            afterDelay: delay
        ];
    }
    true
}

/// What the deferred warm-up does on one firing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SettingsPrewarmStep {
    /// Build the window now.
    Build,
    /// The first-run guide owns the screen: try again later.
    Retry,
    /// The guide is still up after the bounded number of retries: leave the first open cold.
    GiveUp,
}

/// One firing's decision. Pure, so the interactions are a tested contract: warming under the guide would put a
/// window on the user's screen that the guide owns; warming on top of a live smooth-scroll stream blocks the
/// main thread for the build and truncates the stream (measured: the A2 smooth-scroll scenario lost its
/// momentum_ended phase 3 times out of 3 with the warm-up firing at +1s, against 2 of 2 passes without it);
/// and retrying without a cap would keep a timer alive for the whole session.
fn settings_prewarm_step(
    guide_visible: bool,
    motion_active: bool,
    retries: usize,
) -> SettingsPrewarmStep {
    if !guide_visible && !motion_active {
        return SettingsPrewarmStep::Build;
    }
    if retries + 1 < SETTINGS_PREWARM_MAX_ATTEMPTS {
        SettingsPrewarmStep::Retry
    } else {
        SettingsPrewarmStep::GiveUp
    }
}

extern "C" fn on_prewarm_settings(_this: *mut c_void, _cmd: Sel, _sender: *mut c_void) {
    callback_guard::void("on_prewarm_settings", || {
        let retries = SETTINGS_PREWARM_ATTEMPTS.load(Ordering::SeqCst);
        let guide_visible = crate::onboarding::is_visible();
        // Motion is read at firing time: a stream that starts after this check cannot be dodged, but one that
        // is already running must not be stuttered by a 200-view build.
        let motion_active = crate::mouse::smooth::stream_is_active();
        match settings_prewarm_step(guide_visible, motion_active, retries) {
            SettingsPrewarmStep::Build => crate::settings::prewarm_settings_window(),
            SettingsPrewarmStep::Retry => {
                SETTINGS_PREWARM_ATTEMPTS.store(retries + 1, Ordering::SeqCst);
                log_debug!(
                    "[settings-timing] prewarm deferred (guide={guide_visible} motion={motion_active})"
                );
                let _ = schedule_settings_prewarm_after(SETTINGS_PREWARM_RETRY_DELAY);
            }
            SettingsPrewarmStep::GiveUp => {
                log_debug!(
                    "[settings-timing] prewarm given up (guide={guide_visible} motion={motion_active})"
                );
            }
        }
    });
}

/// Thumbnail capture finished (hopped from the worker thread via
/// performSelectorOnMainThread): drains the pending queue and rebuilds the
/// affected cards in place.
extern "C" fn on_thumbnail_ready(_self: *mut c_void, _cmd: Sel, _arg: *mut c_void) {
    callback_guard::void("on_thumbnail_ready", thumbnail::handle_ready_main);
}

/// Screen Recording was granted mid-run (hopped from whichever thread observed the flip): re-run
/// the startup prewarm batch, which had to be skipped while unauthorized.
extern "C" fn on_thumbnail_permission_granted(_self: *mut c_void, _cmd: Sel, _arg: *mut c_void) {
    callback_guard::void("on_thumbnail_permission_granted", || {
        // The guide's resume marker is already written while it is on screen, so it survives even a
        // forced quit & reopen; this only has to warm the frames the grant unlocked.
        thumbnail::backfill_after_permission_grant();
    });
}

/// AX raise mutations are queued by the background raiser and applied here on AppKit's main
/// thread.  Some AX actions synchronously enter AppKit, which traps when called off-main.
extern "C" fn on_ax_raise(_self: *mut c_void, _cmd: Sel, _arg: *mut c_void) {
    callback_guard::void("on_ax_raise", window_collector::handle_ax_raise_main);
}

/// Main-thread entry for deferred cross-display frame verification.
extern "C" fn on_display_move_retry(_self: *mut c_void, _cmd: Sel, arg: *mut c_void) {
    callback_guard::void("on_display_move_retry", || {
        window_management::on_display_move_retry(arg)
    });
}

/// Main-thread entry for the deferred snap settle (write the size after the app's animation).
extern "C" fn on_zoom_observe(_self: *mut c_void, _cmd: Sel, arg: *mut c_void) {
    callback_guard::void("on_zoom_observe", || {
        window_management::on_zoom_observe(arg)
    });
}

extern "C" fn on_restore_verify(_self: *mut c_void, _cmd: Sel, arg: *mut c_void) {
    callback_guard::void("on_restore_verify", || {
        window_management::on_restore_verify(arg)
    });
}

extern "C" fn on_snap_verify(_self: *mut c_void, _cmd: Sel, arg: *mut c_void) {
    callback_guard::void("on_snap_verify", || window_management::on_snap_verify(arg));
}

fn create_overlay_window() -> *mut AnyObject {
    unsafe {
        let screen: *mut AnyObject = msg_send![class!(NSScreen), mainScreen];
        let screen_frame: NSRect = msg_send![screen, frame];
        let h = window_height(6); // initial reasonable default
        let footer_h = footer_band_h();
        let w = window_width(3); // initial three-card baseline; summon recalculates the live width
        let x = (screen_frame.size.width - w) / 2.0 + screen_frame.origin.x;
        let y = (screen_frame.size.height - h) / 2.0 + screen_frame.origin.y;
        let frame = NSRect::new(NSPoint::new(x, y), NSSize::new(w, h));

        // Borderless window (styleMask = 0): no title bar -> the window's visible shape equals
        // the glass's rounded alpha, eliminating the transparent protrusions left at the corners by
        // a rounded glass inside a square window, and keeping all four corners symmetric. A borderless
        // window can't become key by default (no keyboard input), so a custom NSPanel subclass
        // overrides canBecomeKeyWindow -> YES. The earlier titled + transparent-titlebar approach
        // avoided this subclass at the cost of those asymmetric transparent corners (especially
        // visible under the Regular glass style).
        //
        // Key: NSPanel + NSWindowStyleMaskNonactivatingPanel (1<<7) -- the panel becomes key
        // WITHOUT activating the owning app (same as BetterCmdTab's panel). While the app stays
        // inactive during summon, the settings window is never raised above the active app, so
        // the switcher no longer needs the stash/orderBack machinery.
        let style: u64 = 1 << 7; // NSWindowStyleMaskBorderless(0) | NSWindowStyleMaskNonactivatingPanel

        // Register the custom window subclass OhMyTabOverlayWindow : NSPanel (only overrides
        // canBecomeKeyWindow). Inline, like OhMyTabContainerView. register_once makes "registered
        // once" a property of the helper instead of a comment promising it.
        let window_cls = {
            static CLASS: OnceLock<StaticClass> = OnceLock::new();
            crate::ffi::StaticClass::register_once(&CLASS, c"OhMyTabOverlayWindow", || {
                let name = c"OhMyTabOverlayWindow";
                let superclass = class!(NSPanel) as *const _ as *mut AnyObject;
                let cls = objc_allocateClassPair(superclass, name.as_ptr(), 0);
                assert!(
                    !cls.is_null(),
                    "dynamic class {} could not be allocated; is it registered twice?",
                    name.to_string_lossy()
                );
                let types_bool = CString::new("B@:").unwrap();
                class_addMethod(
                    cls,
                    sel!(canBecomeKeyWindow),
                    overlay_window_can_become_key as *mut c_void,
                    types_bool.as_ptr(),
                );
                // Command chords do not arrive at keyDown:. AppKit offers them to performKeyEquivalent:
                // (key window, then the view hierarchy, then the main menu), so the overlay's own
                // Cmd+W/M/H/Q are matched here -- the same layer the settings window uses for its Cmd+Q.
                let types_key_equiv = CString::new("B@:@").unwrap(); // -performKeyEquivalent:(NSEvent*) -> BOOL
                class_addMethod(
                    cls,
                    sel!(performKeyEquivalent:),
                    overlay_window_perform_key_equivalent as *mut c_void,
                    types_key_equiv.as_ptr(),
                );
                objc_registerClassPair(cls);
                StaticClass::defining(cls)
            })
            .0 as *mut AnyObject
        };

        let window: *mut AnyObject = msg_send![window_cls, alloc];
        let window: *mut AnyObject = msg_send![window, initWithContentRect: frame, styleMask: style, backing: 2u64, defer: false];

        // Telegram's fullscreen media viewer uses the popup-menu level (101), so the old
        // NSFloatingWindowLevel (3) was composited underneath it. Use CoreGraphics' public
        // overlay level instead (currently 102): it is the smallest standard level that puts
        // this short-lived switcher above app-owned overlays without resorting to screen-saver
        // level or a Telegram-specific exception.
        let _: () = msg_send![window, setLevel: cg_overlay_window_level()];

        // Keep the switcher in the active app's fullscreen Space and out of normal window
        // cycling/Expose. CanJoinAllApplications is available on the project's macOS 13+
        // minimum and is specifically intended for floating system-style overlays.
        let collection_behavior: usize = (1 << 0)  // NSWindowCollectionBehaviorCanJoinAllSpaces
            | (1 << 3) // NSWindowCollectionBehaviorTransient
            | (1 << 6) // NSWindowCollectionBehaviorIgnoresCycle
            | (1 << 8) // NSWindowCollectionBehaviorFullScreenAuxiliary
            | (1 << 18); // NSWindowCollectionBehaviorCanJoinAllApplications (macOS 13+)
        let _: () = msg_send![window, setCollectionBehavior: collection_behavior];

        // (1) Window must be non-opaque so the compositor allows content
        //     behind the window to show through.
        let _: () = msg_send![window, setOpaque: false];
        // (2) Window background must be clear, otherwise NSThemeFrame draws
        //     a solid color that blocks everything behind it.
        let clear_color: *mut AnyObject = msg_send![class!(NSColor), clearColor];
        let _: () = msg_send![window, setBackgroundColor: clear_color];
        // (3) Disable the window shadow: NSGlassEffectView already provides Liquid Glass depth, so
        //     the window shadow is redundant. Its strength scales with content alpha (Regular's
        //     opaque glass -> a strong shadow ring; Clear's near-transparent glass -> none), which
        //     shows up under Regular as an unwanted dark ring along the glass edge; the drop shadow
        //     is also offset downward, so the bottom corners differ from the top (this is the "ring
        //     still on the edge, bottom corners shaped differently" seen after going borderless).
        //     With it off, only the glass's own depth remains -- clean edges, symmetric corners.
        let _: () = msg_send![window, setHasShadow: false];

        let _: () = msg_send![window, setReleasedWhenClosed: false];
        // Don't let the window hide on deactivate (we manage show/hide)
        let _: () = msg_send![window, setHidesOnDeactivate: false];

        // Shared material backdrop: Liquid Glass on macOS 26+, frost on older systems — or
        // the user's chosen material (frost / opaque) on any system.
        let radius = CONFIG.read().unwrap().appearance.corner_radius;
        let backdrop = crate::glass::install_backdrop(
            window,
            NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(w, h)),
            radius,
            crate::overlay::switcher_backdrop_options(),
        );
        *crate::overlay::OVERLAY_BACKDROP.lock().unwrap() = Some(backdrop);
        let content_parent = backdrop.content_parent;

        // Register OhMyTabContainerView : NSClipView
        let container_cls = {
            static CLASS: OnceLock<StaticClass> = OnceLock::new();
            crate::ffi::StaticClass::register_once(&CLASS, c"OhMyTabContainerView", || {
                let name = c"OhMyTabContainerView";
                let superclass = class!(NSClipView) as *const _ as *mut AnyObject;
                let cls = objc_allocateClassPair(superclass, name.as_ptr(), 0);
                assert!(
                    !cls.is_null(),
                    "dynamic class {} could not be allocated; is it registered twice?",
                    name.to_string_lossy()
                );
                let types_v_obj = CString::new("v@:@").unwrap();
                let types_bool = CString::new("B@:").unwrap();
                class_addMethod(
                    cls,
                    sel!(keyDown:),
                    container_key_down as *mut c_void,
                    types_v_obj.as_ptr(),
                );
                class_addMethod(
                    cls,
                    sel!(acceptsFirstResponder),
                    container_accepts_first_responder as *mut c_void,
                    types_bool.as_ptr(),
                );
                class_addMethod(
                    cls,
                    sel!(mouseMoved:),
                    container_mouse_moved as *mut c_void,
                    types_v_obj.as_ptr(),
                );
                class_addMethod(
                    cls,
                    sel!(mouseEntered:),
                    container_mouse_entered as *mut c_void,
                    types_v_obj.as_ptr(),
                );
                class_addMethod(
                    cls,
                    sel!(mouseExited:),
                    container_mouse_exited as *mut c_void,
                    types_v_obj.as_ptr(),
                );
                class_addMethod(
                    cls,
                    sel!(mouseDragged:),
                    container_mouse_moved as *mut c_void,
                    types_v_obj.as_ptr(),
                );
                class_addMethod(
                    cls,
                    sel!(scrollWheel:),
                    container_scroll_wheel as *mut c_void,
                    types_v_obj.as_ptr(),
                );
                objc_registerClassPair(cls);
                StaticClass::defining(cls)
            })
            .0 as *mut AnyObject
        };

        // The custom scrollbar handles dragging explicitly, avoiding native NSScroller tracking
        // failures inside the nonactivating panel.
        let scroller_cls = {
            static CLASS: OnceLock<StaticClass> = OnceLock::new();
            crate::ffi::StaticClass::register_once(
                &CLASS,
                c"OhMyTabThumbnailScrollIndicator",
                || {
                    let name = c"OhMyTabThumbnailScrollIndicator";
                    let superclass = class!(NSView) as *const _ as *mut AnyObject;
                    let cls = objc_allocateClassPair(superclass, name.as_ptr(), 0);
                    assert!(
                        !cls.is_null(),
                        "dynamic class {} could not be allocated; is it registered twice?",
                        name.to_string_lossy()
                    );
                    let types_rect = CString::new("v@:{CGRect={CGPoint=dd}{CGSize=dd}}").unwrap();
                    class_addMethod(
                        cls,
                        sel!(drawRect:),
                        thumbnail_scroller_draw_rect as *mut c_void,
                        types_rect.as_ptr(),
                    );
                    let types_mouse = CString::new("v@:@").unwrap();
                    class_addMethod(
                        cls,
                        sel!(mouseDown:),
                        thumbnail_scroller_mouse_down as *mut c_void,
                        types_mouse.as_ptr(),
                    );
                    class_addMethod(
                        cls,
                        sel!(mouseDragged:),
                        thumbnail_scroller_mouse_dragged as *mut c_void,
                        types_mouse.as_ptr(),
                    );
                    class_addMethod(
                        cls,
                        sel!(mouseUp:),
                        thumbnail_scroller_mouse_up as *mut c_void,
                        types_mouse.as_ptr(),
                    );
                    class_addMethod(
                        cls,
                        sel!(mouseEntered:),
                        thumbnail_scroller_mouse_entered as *mut c_void,
                        types_mouse.as_ptr(),
                    );
                    class_addMethod(
                        cls,
                        sel!(mouseMoved:),
                        thumbnail_scroller_mouse_moved as *mut c_void,
                        types_mouse.as_ptr(),
                    );
                    class_addMethod(
                        cls,
                        sel!(mouseExited:),
                        thumbnail_scroller_mouse_exited as *mut c_void,
                        types_mouse.as_ptr(),
                    );
                    let types_accepts_first_mouse = CString::new("B@:@").unwrap();
                    class_addMethod(
                        cls,
                        sel!(acceptsFirstMouse:),
                        thumbnail_scroller_accepts_first_mouse as *mut c_void,
                        types_accepts_first_mouse.as_ptr(),
                    );
                    objc_registerClassPair(cls);
                    StaticClass::defining(cls)
                },
            )
            .0 as *mut AnyObject
        };

        let container: *mut AnyObject = msg_send![container_cls, alloc];
        // The card container covers only the area above the status footer; NSClipView clips cards
        // as they move continuously across the viewport edges.
        let container: *mut AnyObject = msg_send![container, initWithFrame: NSRect::new(
            NSPoint::new(0.0, footer_h),
            NSSize::new(w, (h - footer_h).max(1.0))
        )];
        let _: () = msg_send![container, setAutoresizingMask: 18u64];
        let _: () = msg_send![container, setDrawsBackground: false];
        let _: () = msg_send![content_parent, addSubview: container];
        *CONTAINER.lock().unwrap() = Some(ObjPtr::new(container));

        // NSClipView is only the viewport; all cards live in a persistent document view and
        // scrolling moves the bounds instead of rebuilding the card hierarchy.
        let document: *mut AnyObject = msg_send![class!(NSView), alloc];
        let document: *mut AnyObject = msg_send![
            document,
            initWithFrame: NSRect::new(
                NSPoint::new(0.0, 0.0),
                NSSize::new(w, (h - footer_h).max(1.0))
            )
        ];
        let _: () = msg_send![document, setWantsLayer: false];
        let _: () = msg_send![container, setDocumentView: document];
        clear_card_indices();
        *CARD_DOCUMENT.lock().unwrap() = Some(ObjPtr::new(document));
        release_obj(document);

        let status_font: *mut AnyObject = {
            let status_bar_weight = CONFIG.read().unwrap().fonts.status_bar_weight;
            msg_send![class!(NSFont), systemFontOfSize: status_bar_text_size(), weight: status_bar_weight]
        };
        // The status line (and the "no windows to switch" hint it carries) rides on the user's material
        // with vibrant ink, so it needs no band (design-style §3).
        let status_label = make_centered_label_with_class(
            "",
            status_font,
            crate::glass::PanelInk::Muted.color(),
            0.0,
            w,
            footer_h,
            crate::glass::vibrant_label_class(),
        );
        let _: () = msg_send![content_parent, addSubview: status_label];
        *STATUS_LABEL.lock().unwrap() = Some(ObjPtr::new(status_label));

        // The footer's settings gear: the trailing corner of the footer band, same control family as
        // the clipboard picker's corner gear. Its frame is recomputed on every summon (the panel
        // resizes) and on a corner-radius change, so it is never left on a stale geometry.
        let settings_button =
            crate::overlay::make_footer_settings_button(crate::overlay::footer_settings_frame(
                NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(w, h)).size,
            ));
        let _: () = msg_send![content_parent, addSubview: settings_button];
        release_obj(settings_button);
        *crate::overlay::FOOTER_SETTINGS_BUTTON.lock().unwrap() =
            Some(ObjPtr::new(settings_button));

        // Keep the indicator above the card container; rebuilding cards cannot change its z-order
        // or interrupt explicit dragging. Its track starts above the floating settings control when
        // the footer title is off: the control is added to the hierarchy first, so an overlap would
        // give the scrollbar the control's clicks.
        let scroller_bottom = footer_h + crate::theme::footer_control_bottom_reserve();
        let scroller: *mut AnyObject = msg_send![scroller_cls, alloc];
        let scroller: *mut AnyObject = msg_send![
            scroller,
            initWithFrame: NSRect::new(
                NSPoint::new(w - H_PADDING - THUMB_SCROLLBAR_W, scroller_bottom),
                NSSize::new(THUMB_SCROLLBAR_W, (h - scroller_bottom).max(1.0))
            )
        ];
        let _: () = msg_send![scroller, setWantsLayer: false];
        let _: () = msg_send![scroller, setHidden: true];
        let scroller_tracking_opts: u64 = 0x01 | 0x02 | 0x80 | 0x200;
        let scroller_tracking: *mut AnyObject = msg_send![class!(NSTrackingArea), alloc];
        let scroller_tracking: *mut AnyObject = msg_send![
            scroller_tracking,
            initWithRect: NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(THUMB_SCROLLBAR_W, (h - scroller_bottom).max(1.0))),
            options: scroller_tracking_opts,
            owner: scroller,
            userInfo: std::ptr::null::<AnyObject>()
        ];
        let _: () = msg_send![scroller, addTrackingArea: scroller_tracking];
        release_obj(scroller_tracking);
        let _: () = msg_send![content_parent, addSubview: scroller];
        *THUMB_SCROLLER.lock().unwrap() = Some(ObjPtr::new(scroller));

        window
    }
}

fn create_controller() -> *mut AnyObject {
    unsafe { msg_send![controller_class().0 as *mut AnyObject, new] }
}

/// Register OhMyTabController : NSObject, the reopen-policy and event-handler target.
///
/// register_once makes "registered once" a property of the helper instead of a comment: a second
/// entry (a smoke runner re-running setup) reuses the class rather than handing
/// objc_allocateClassPair a name the runtime already owns.
fn controller_class() -> StaticClass {
    static CLASS: OnceLock<StaticClass> = OnceLock::new();
    crate::ffi::StaticClass::register_once(&CLASS, c"OhMyTabController", || unsafe {
        let name = c"OhMyTabController";
        let superclass = class!(NSObject) as *const _ as *mut AnyObject;
        let cls = objc_allocateClassPair(superclass, name.as_ptr(), 0);
        assert!(
            !cls.is_null(),
            "dynamic class {} could not be allocated; is it registered twice?",
            name.to_string_lossy()
        );
        let types_v_obj = CString::new("v@:@").unwrap();
        let app_reopen_types = CString::new(APP_REOPEN_METHOD_ENCODING).unwrap();
        class_addMethod(
            cls,
            sel!(applicationShouldHandleReopen:hasVisibleWindows:),
            on_application_should_handle_reopen as *mut c_void,
            app_reopen_types.as_ptr(),
        );
        class_addMethod(
            cls,
            sel!(handleAppReopenOnMainThread:),
            on_app_reopen_on_main_thread as *mut c_void,
            types_v_obj.as_ptr(),
        );
        class_addMethod(
            cls,
            sel!(handleCmdTabPressed:),
            on_cmd_tab_pressed as *mut c_void,
            types_v_obj.as_ptr(),
        );
        class_addMethod(
            cls,
            sel!(handleCmdShiftTabPressed:),
            on_cmd_shift_tab_pressed as *mut c_void,
            types_v_obj.as_ptr(),
        );
        class_addMethod(
            cls,
            sel!(handleCmdReleased:),
            on_cmd_released as *mut c_void,
            types_v_obj.as_ptr(),
        );
        class_addMethod(
            cls,
            sel!(handleCmdReleaseDiagnostic:),
            on_cmd_release_diagnostic as *mut c_void,
            types_v_obj.as_ptr(),
        );

        class_addMethod(
            cls,
            sel!(closeCard:),
            on_close_card as *mut c_void,
            types_v_obj.as_ptr(),
        );
        class_addMethod(
            cls,
            sel!(handleCardCloseFinished:),
            overlay::on_card_close_finished as *mut c_void,
            types_v_obj.as_ptr(),
        );
        class_addMethod(
            cls,
            sel!(handleCardCloseAXResult:),
            overlay::on_card_close_ax_result as *mut c_void,
            types_v_obj.as_ptr(),
        );
        class_addMethod(
            cls,
            sel!(handlePanelActionRefresh:),
            overlay::on_panel_action_refresh as *mut c_void,
            types_v_obj.as_ptr(),
        );
        class_addMethod(
            cls,
            sel!(onClipboardToggled:),
            clipboard::on_clipboard_toggle as *mut c_void,
            types_v_obj.as_ptr(),
        );
        class_addMethod(
            cls,
            sel!(handleDisplayMoveRetry:),
            on_display_move_retry as *mut c_void,
            types_v_obj.as_ptr(),
        );
        class_addMethod(
            cls,
            sel!(handleSnapVerify:),
            on_snap_verify as *mut c_void,
            types_v_obj.as_ptr(),
        );
        class_addMethod(
            cls,
            sel!(handleRestoreVerify:),
            on_restore_verify as *mut c_void,
            types_v_obj.as_ptr(),
        );
        class_addMethod(
            cls,
            sel!(handleZoomObserve:),
            on_zoom_observe as *mut c_void,
            types_v_obj.as_ptr(),
        );
        class_addMethod(
            cls,
            sel!(handleAppActivation:),
            on_app_activated as *mut c_void,
            types_v_obj.as_ptr(),
        );
        class_addMethod(
            cls,
            sel!(handleSelfBecameActive:),
            on_self_became_active as *mut c_void,
            types_v_obj.as_ptr(),
        );
        class_addMethod(
            cls,
            sel!(handleSelfResignedActive:),
            on_self_resigned_active as *mut c_void,
            types_v_obj.as_ptr(),
        );
        class_addMethod(
            cls,
            sel!(handleSpaceTransitionSettle:),
            window_refresh::on_space_transition_settle as *mut c_void,
            types_v_obj.as_ptr(),
        );
        class_addMethod(
            cls,
            sel!(handleActiveSpaceChanged:),
            on_active_space_changed as *mut c_void,
            types_v_obj.as_ptr(),
        );
        class_addMethod(
            cls,
            sel!(handleActivationFocusResult:),
            on_activation_focus_result as *mut c_void,
            types_v_obj.as_ptr(),
        );
        class_addMethod(
            cls,
            sel!(handleGlobalInputDrain:),
            on_global_input_drain as *mut c_void,
            types_v_obj.as_ptr(),
        );
        class_addMethod(
            cls,
            sel!(handleSmoothScrollE2eRecord:),
            on_smooth_scroll_e2e_record as *mut c_void,
            types_v_obj.as_ptr(),
        );
        class_addMethod(
            cls,
            sel!(handleDevBlankText:),
            on_dev_blank_text as *mut c_void,
            types_v_obj.as_ptr(),
        );
        class_addMethod(
            cls,
            sel!(handleDevShowClipboard:),
            on_dev_show_clipboard as *mut c_void,
            types_v_obj.as_ptr(),
        );
        class_addMethod(
            cls,
            sel!(handlePermissionRestartRequired:),
            restart::on_permission_restart_required as *mut c_void,
            types_v_obj.as_ptr(),
        );
        class_addMethod(
            cls,
            sel!(handlePermissionRestartNow:),
            restart::on_permission_restart_now as *mut c_void,
            types_v_obj.as_ptr(),
        );
        class_addMethod(
            cls,
            sel!(handleOnboardingAction:),
            onboarding::on_action as *mut c_void,
            types_v_obj.as_ptr(),
        );
        class_addMethod(
            cls,
            sel!(handleOnboardingTick:),
            onboarding::on_tick as *mut c_void,
            types_v_obj.as_ptr(),
        );
        class_addMethod(
            cls,
            sel!(handleAppLaunch:),
            on_app_launched as *mut c_void,
            types_v_obj.as_ptr(),
        );
        class_addMethod(
            cls,
            sel!(handleLocaleChanged:),
            on_locale_changed as *mut c_void,
            types_v_obj.as_ptr(),
        );
        class_addMethod(
            cls,
            sel!(handleKeyboardLayoutChanged:),
            on_keyboard_layout_changed as *mut c_void,
            types_v_obj.as_ptr(),
        );
        class_addMethod(
            cls,
            sel!(handleScrollerStyleChanged:),
            on_scroller_style_changed as *mut c_void,
            types_v_obj.as_ptr(),
        );
        class_addMethod(
            cls,
            sel!(handleAppearanceChanged:),
            on_appearance_changed as *mut c_void,
            types_v_obj.as_ptr(),
        );
        class_addMethod(
            cls,
            sel!(handleScreenParametersChanged:),
            on_screen_parameters_changed as *mut c_void,
            types_v_obj.as_ptr(),
        );
        class_addMethod(
            cls,
            sel!(handleDisplayReconfiguration:),
            overlay::on_display_reconfiguration as *mut c_void,
            types_v_obj.as_ptr(),
        );
        class_addMethod(
            cls,
            sel!(handleDelayedOrderOut:),
            on_delayed_order_out as *mut c_void,
            types_v_obj.as_ptr(),
        );
        // Deferred switch raise: release/click/Enter commit the vanish to the screen first,
        // then activate+raise on the next runloop turn.
        class_addMethod(
            cls,
            sel!(handleDeferredRaise:),
            on_deferred_raise as *mut c_void,
            types_v_obj.as_ptr(),
        );
        class_addMethod(
            cls,
            sel!(handleDeferredScrollHover:),
            on_deferred_scroll_hover as *mut c_void,
            types_v_obj.as_ptr(),
        );
        class_addMethod(
            cls,
            sel!(handleDevicesChanged:),
            on_devices_changed as *mut c_void,
            types_v_obj.as_ptr(),
        );
        class_addMethod(
            cls,
            sel!(prewarmSettings:),
            on_prewarm_settings as *mut c_void,
            types_v_obj.as_ptr(),
        );
        class_addMethod(
            cls,
            sel!(thumbnailReady:),
            on_thumbnail_ready as *mut c_void,
            types_v_obj.as_ptr(),
        );
        class_addMethod(
            cls,
            sel!(thumbnailPermissionGranted:),
            on_thumbnail_permission_granted as *mut c_void,
            types_v_obj.as_ptr(),
        );
        class_addMethod(
            cls,
            sel!(handleAxRaise:),
            on_ax_raise as *mut c_void,
            types_v_obj.as_ptr(),
        );
        class_addMethod(
            cls,
            sel!(handleAppTerminate:),
            on_app_terminated as *mut c_void,
            types_v_obj.as_ptr(),
        );
        class_addMethod(
            cls,
            sel!(handleWillTerminate:),
            on_will_terminate as *mut c_void,
            types_v_obj.as_ptr(),
        );
        class_addMethod(
            cls,
            sel!(handleWindowRefresh:),
            window_refresh::on_window_refresh as *mut c_void,
            types_v_obj.as_ptr(),
        );
        class_addMethod(
            cls,
            sel!(handleFirstSummonTimeout:),
            overlay::on_first_summon_timeout as *mut c_void,
            types_v_obj.as_ptr(),
        );
        class_addMethod(
            cls,
            sel!(handleWindowServerEvent:),
            window_refresh::on_window_server_event as *mut c_void,
            types_v_obj.as_ptr(),
        );
        class_addMethod(
            cls,
            sel!(handleLifecycleBackstop:),
            window_refresh::on_lifecycle_backstop as *mut c_void,
            types_v_obj.as_ptr(),
        );
        objc_registerClassPair(cls);
        StaticClass::defining(cls)
    })
}

/// Load the canonical app icon directly from the PNG bundled into the binary.
pub(crate) unsafe fn load_embedded_app_icon() -> *mut AnyObject {
    let png_bytes: &[u8] = include_bytes!("../assets/Icon-512x512.png");
    let data: *mut AnyObject = msg_send![
        class!(NSData),
        dataWithBytes: png_bytes.as_ptr() as *const c_void,
        length: png_bytes.len()
    ];
    let image: *mut AnyObject = msg_send![class!(NSImage), alloc];
    msg_send![image, initWithData: data]
}

fn init_app() {
    unsafe {
        let nsapp: *mut AnyObject = msg_send![class!(NSApplication), sharedApplication];
        // NSApplicationActivationPolicyAccessory = 1
        let _: bool = msg_send![nsapp, setActivationPolicy: 1isize];
    }
}

/// Switch the app activation policy: .regular while the settings window is open (so it can
/// activate normally and raise itself above the active app when opened), .accessory when
/// closed (pure menu-bar). LSUIElement defaults to .accessory.
pub(crate) fn set_settings_activation_policy(regular: bool) {
    unsafe {
        let nsapp: *mut AnyObject = msg_send![class!(NSApplication), sharedApplication];
        // NSApplicationActivationPolicyRegular = 0, .Accessory = 1
        let policy: isize = if regular { 0 } else { 1 };
        let _: bool = msg_send![nsapp, setActivationPolicy: policy];
    }
}

/// Register OhMyTabMenuTarget2 : NSObject, the target of every menu-item and settings action.
///
/// register_once makes "registered once" a property of the helper instead of a comment: a second
/// entry reuses the class rather than handing objc_allocateClassPair a name the runtime already
/// owns.
fn menu_target_class() -> StaticClass {
    static CLASS: OnceLock<StaticClass> = OnceLock::new();
    crate::ffi::StaticClass::register_once(&CLASS, c"OhMyTabMenuTarget2", || {
        // The class is built from raw objc FFI calls.
        unsafe {
            let name = c"OhMyTabMenuTarget2";
            let superclass = class!(NSObject) as *const _ as *mut AnyObject;
            let cls = objc_allocateClassPair(superclass, name.as_ptr(), 0);
            assert!(
                !cls.is_null(),
                "dynamic class {} could not be allocated; is it registered twice?",
                name.to_string_lossy()
            );
            let types = CString::new("v@:@").unwrap();
            class_addMethod(
                cls,
                sel!(handleQuit:),
                handle_quit as *mut c_void,
                types.as_ptr(),
            );
            class_addMethod(
                cls,
                sel!(handleToggleShortcut:),
                handle_toggle_shortcut as *mut c_void,
                types.as_ptr(),
            );
            class_addMethod(
                cls,
                sel!(handleToggleThumbnail:),
                handle_toggle_thumbnail as *mut c_void,
                types.as_ptr(),
            );
            class_addMethod(
                cls,
                sel!(handleToggleService:),
                handle_toggle_service as *mut c_void,
                types.as_ptr(),
            );
            class_addMethod(
                cls,
                sel!(handlePinClipboard:),
                crate::menu::handle_pin_clipboard as *mut c_void,
                types.as_ptr(),
            );
            class_addMethod(
                cls,
                sel!(handleReloadConfig:),
                handle_reload_config as *mut c_void,
                types.as_ptr(),
            );
            class_addMethod(
                cls,
                sel!(handleClearCaches:),
                handle_clear_caches as *mut c_void,
                types.as_ptr(),
            );
            class_addMethod(
                cls,
                sel!(handleSettings:),
                on_settings_open as *mut c_void,
                types.as_ptr(),
            );
            class_addMethod(
                cls,
                sel!(handleSettingsSidebar:),
                on_sidebar_select as *mut c_void,
                types.as_ptr(),
            );
            class_addMethod(
                cls,
                sel!(handleRestoreDefaults:),
                handle_restore_defaults as *mut c_void,
                types.as_ptr(),
            );
            class_addMethod(
                cls,
                sel!(handleRestoreDefaultsConfirm:),
                handle_restore_defaults_confirm as *mut c_void,
                types.as_ptr(),
            );
            class_addMethod(
                cls,
                sel!(handleRestoreDefaultsCancel:),
                handle_restore_defaults_cancel as *mut c_void,
                types.as_ptr(),
            );
            class_addMethod(
                cls,
                sel!(handleOpenPrivacy:),
                handle_open_privacy as *mut c_void,
                types.as_ptr(),
            );
            // The warning banner's button changes meaning with the warning it carries (permission
            // guidance vs keychain access), so the same selector is registered on both the settings
            // and the clipboard observer classes and both route to this handler.
            class_addMethod(
                cls,
                sel!(retryKeychainAccess:),
                handle_retry_keychain_access as *mut c_void,
                types.as_ptr(),
            );
            // Same two-class arrangement for the storage notice's other action: the settings banner
            // and the picker's empty state both target it.
            class_addMethod(
                cls,
                sel!(clearClipboardStorage:),
                handle_clear_clipboard_storage as *mut c_void,
                types.as_ptr(),
            );
            // The settings window's buttons target THIS action class (MENU_TARGET in
            // settings/window.rs), so "View guide" must be registered here; registering it on the
            // main controller class leaves an unimplemented selector and the click does nothing
            // (measured).
            class_addMethod(
                cls,
                sel!(handleOpenOnboarding:),
                onboarding::on_open_from_settings as *mut c_void,
                types.as_ptr(),
            );
            class_addMethod(
                cls,
                sel!(handleRestartForAccessibility:),
                restart::on_restart_button as *mut c_void,
                types.as_ptr(),
            );
            class_addMethod(
                cls,
                sel!(handleOpenScreenRecordingPrivacy:),
                handle_open_screen_recording_privacy as *mut c_void,
                types.as_ptr(),
            );
            class_addMethod(
                cls,
                sel!(handleCheckForUpdates:),
                handle_check_for_updates as *mut c_void,
                types.as_ptr(),
            );
            class_addMethod(
                cls,
                sel!(handleExportLogs:),
                handle_export_logs as *mut c_void,
                types.as_ptr(),
            );
            class_addMethod(
                cls,
                sel!(handleExportSettings:),
                handle_export_settings as *mut c_void,
                types.as_ptr(),
            );
            class_addMethod(
                cls,
                sel!(handleImportSettings:),
                handle_import_settings as *mut c_void,
                types.as_ptr(),
            );
            class_addMethod(
                cls,
                sel!(handleImportSettingsReady:),
                handle_import_settings_ready as *mut c_void,
                types.as_ptr(),
            );
            class_addMethod(
                cls,
                sel!(handleExportSettingsReady:),
                handle_export_settings_ready as *mut c_void,
                types.as_ptr(),
            );
            class_addMethod(
                cls,
                sel!(handleOpenOfficialWebsite:),
                handle_open_official_website as *mut c_void,
                types.as_ptr(),
            );
            class_addMethod(
                cls,
                sel!(handleEnableMouseToggle:),
                handle_enable_mouse_toggle as *mut c_void,
                types.as_ptr(),
            );
            class_addMethod(
                cls,
                sel!(handleWindowsEnabledToggle:),
                handle_windows_enabled_toggle as *mut c_void,
                types.as_ptr(),
            );
            class_addMethod(
                cls,
                sel!(handleClipboardEnabledToggle:),
                handle_clipboard_enabled_toggle as *mut c_void,
                types.as_ptr(),
            );
            class_addMethod(
                cls,
                sel!(handleClipboardShortcutRecord:),
                settings::handle_clipboard_shortcut_record as *mut c_void,
                types.as_ptr(),
            );
            class_addMethod(
                cls,
                sel!(handleKeyBindingsToggle:),
                handle_key_bindings_toggle as *mut c_void,
                types.as_ptr(),
            );
            class_addMethod(
                cls,
                sel!(handleBindingToggle:),
                bindings::handle_binding_toggle as *mut c_void,
                types.as_ptr(),
            );
            class_addMethod(
                cls,
                sel!(handleBindingEdit:),
                bindings::handle_binding_edit as *mut c_void,
                types.as_ptr(),
            );
            class_addMethod(
                cls,
                sel!(handleBindingDelete:),
                bindings::handle_binding_delete as *mut c_void,
                types.as_ptr(),
            );
            // The footer's single "Add binding" button.
            class_addMethod(
                cls,
                sel!(handleBindingAdd:),
                bindings::handle_binding_add as *mut c_void,
                types.as_ptr(),
            );
            class_addMethod(
                cls,
                sel!(handleBindingPanelAction:),
                bindings::handle_panel_action_changed as *mut c_void,
                types.as_ptr(),
            );
            class_addMethod(
                cls,
                sel!(handleBindingPanelRecord:),
                bindings::handle_panel_record as *mut c_void,
                types.as_ptr(),
            );
            class_addMethod(
                cls,
                sel!(handleBindingPanelConfirm:),
                bindings::handle_panel_confirm as *mut c_void,
                types.as_ptr(),
            );
            class_addMethod(
                cls,
                sel!(handleBindingPanelCancel:),
                bindings::handle_panel_cancel as *mut c_void,
                types.as_ptr(),
            );
            class_addMethod(
                cls,
                sel!(handleBindingConfirm:),
                bindings::handle_binding_confirm as *mut c_void,
                types.as_ptr(),
            );
            class_addMethod(
                cls,
                sel!(handleBindingCancel:),
                bindings::handle_binding_cancel as *mut c_void,
                types.as_ptr(),
            );
            class_addMethod(
                cls,
                sel!(handleControlChanged:),
                on_control_changed as *mut c_void,
                types.as_ptr(),
            );
            class_addMethod(
                cls,
                sel!(handleControlTextDidChange:),
                on_control_text_did_change as *mut c_void,
                types.as_ptr(),
            );
            class_addMethod(
                cls,
                sel!(handleControlTextDidEndEditing:),
                on_control_text_did_end_editing as *mut c_void,
                types.as_ptr(),
            );
            class_addMethod(
                cls,
                sel!(handlePageRestoreDefaults:),
                handle_page_restore_defaults as *mut c_void,
                types.as_ptr(),
            );
            class_addMethod(
                cls,
                sel!(handlePageRestoreDefaultsConfirm:),
                handle_page_restore_defaults_confirm as *mut c_void,
                types.as_ptr(),
            );
            class_addMethod(
                cls,
                sel!(handlePageRestoreDefaultsCancel:),
                handle_page_restore_defaults_cancel as *mut c_void,
                types.as_ptr(),
            );
            class_addMethod(
                cls,
                sel!(handleDeviceChanged:),
                handle_device_changed as *mut c_void,
                types.as_ptr(),
            );
            class_addMethod(
                cls,
                sel!(handleAddMapping:),
                handle_add_mapping as *mut c_void,
                types.as_ptr(),
            );
            class_addMethod(
                cls,
                sel!(handleMappingEnabledChanged:),
                handle_mapping_enabled_changed as *mut c_void,
                types.as_ptr(),
            );
            class_addMethod(
                cls,
                sel!(handleMappingEdit:),
                handle_mapping_edit as *mut c_void,
                types.as_ptr(),
            );
            class_addMethod(
                cls,
                sel!(handleDeleteMapping:),
                handle_delete_mapping as *mut c_void,
                types.as_ptr(),
            );
            class_addMethod(
                cls,
                sel!(handlePanelRecordTrigger:),
                handle_panel_record_trigger as *mut c_void,
                types.as_ptr(),
            );
            class_addMethod(
                cls,
                sel!(handlePanelRecordCombo:),
                handle_panel_record_combo as *mut c_void,
                types.as_ptr(),
            );
            class_addMethod(
                cls,
                sel!(handlePanelActionChanged:),
                handle_panel_action_changed as *mut c_void,
                types.as_ptr(),
            );
            class_addMethod(
                cls,
                sel!(handleMappingConfirm:),
                handle_mapping_confirm as *mut c_void,
                types.as_ptr(),
            );
            class_addMethod(
                cls,
                sel!(handleMappingCancel:),
                handle_mapping_cancel as *mut c_void,
                types.as_ptr(),
            );
            class_addMethod(
                cls,
                sel!(handleRecordingFinished:),
                handle_recording_finished as *mut c_void,
                types.as_ptr(),
            );
            class_addMethod(
                cls,
                sel!(handleRecordingCancelled:),
                handle_recording_cancelled as *mut c_void,
                types.as_ptr(),
            );
            objc_registerClassPair(cls);
            StaticClass::defining(cls)
        }
    })
}

fn setup_status_bar() {
    unsafe {
        let status_bar: *mut AnyObject = msg_send![class!(NSStatusBar), systemStatusBar];
        // NSVariableStatusItemLength = -1.0: the slot auto-sizes to the button's content, so after
        // sizeToFit it hugs the icon with no extra padding. A fixed length (the former 30.0) is not
        // shrunk by sizeToFit, so the slot stays 30pt while the icon is ~17pt, leaving visible gaps
        // around the icon.
        let status_item: *mut AnyObject = msg_send![status_bar, statusItemWithLength: -1.0f64];
        let _: *mut AnyObject = msg_send![status_item, retain];
        // Record the handle for "hide menu bar icon" before the item is configured: the toggle only
        // flips visibility on this same item, so nothing below has to know it can be hidden.
        crate::menu::register_status_item(status_item);

        let button: *mut AnyObject = msg_send![status_item, button];

        // Status bar icon: monochrome template PNG (two overlapped rects, assets/statusbar-icon.png embedded
        // in the binary). setTemplate:YES makes the system render it in the menu bar foreground color
        // (clear on both light/dark menu bars); sizeToFit hugs the image.
        let png_bytes: &[u8] = include_bytes!("../assets/statusbar-icon.png");
        let nsdata: *mut AnyObject = msg_send![
            class!(NSData),
            dataWithBytes: png_bytes.as_ptr() as *const c_void,
            length: png_bytes.len()
        ];
        let image: *mut AnyObject = msg_send![class!(NSImage), alloc];
        let image: *mut AnyObject = msg_send![image, initWithData: nsdata];
        if !image.is_null() {
            // statusbar-icon.png is 162x128 (wider horizontal margins so the icon sits looser
            // from its neighbors). Set the height to the status-bar thickness and scale the width
            // by the PNG aspect ratio, keeping the glyph pixel size while widening the gaps.
            // Forcing a square (icon_size x icon_size) would squash the non-square PNG.
            let icon_size: f64 = msg_send![status_bar, thickness];
            let aspect: f64 = 162.0 / 128.0; // PNG aspect ratio
            let _: () = msg_send![image, setSize: NSSize::new(icon_size * aspect, icon_size)];
            let is_template: bool = true;
            let _: () = msg_send![image, setTemplate: is_template];
            let _: () = msg_send![button, setImage: image];
            // NSImageOnly = 1
            let _: () = msg_send![button, setImagePosition: 1usize];
            let _: () = msg_send![image, release]; // button retained it; drop our alloc +1
        } else {
            let ns_title = make_nsstring("Tab");
            let _: () = msg_send![button, setTitle: ns_title];
            CFRelease(ns_title as *const c_void);
        }

        let _: () = msg_send![button, sizeToFit];
        let _: () = msg_send![button, setNeedsDisplay: true];

        // Build menu
        let menu_title = make_nsstring("");
        let menu: *mut AnyObject = msg_send![class!(NSMenu), alloc];
        let menu: *mut AnyObject = msg_send![menu, initWithTitle: menu_title];
        CFRelease(menu_title as *const c_void);

        let menu_target: *mut AnyObject = msg_send![menu_target_class().0 as *const AnyObject, new];
        *MENU_TARGET.lock().unwrap() = Some(ObjPtr::new(menu_target));
        // Keep a Send-safe handle to the same object for background dispatch (see
        // MENU_TARGET_DISPATCH).
        *MENU_TARGET_DISPATCH.lock().unwrap() = Some(CallbackTarget(menu_target));

        // Settings item comes first and opens the settings window.
        let settings_title = make_nsstring(&t("menu.settings"));
        let settings_key = make_nsstring("");
        let settings_item: *mut AnyObject = msg_send![class!(NSMenuItem), alloc];
        let settings_item: *mut AnyObject = msg_send![settings_item, initWithTitle: settings_title, action: sel!(handleSettings:), keyEquivalent: settings_key];
        CFRelease(settings_title as *const c_void);
        CFRelease(settings_key as *const c_void);
        let _: () = msg_send![settings_item, setTarget: menu_target];
        set_menu_item_title(settings_item, &t("menu.settings"));
        let _: () = msg_send![menu, addItem: settings_item];
        // Separate Settings from the action items below, matching the separator above Quit.
        let settings_separator: *mut AnyObject = msg_send![class!(NSMenuItem), separatorItem];
        let _: () = msg_send![menu, addItem: settings_separator];

        // Five top-level service toggles, between Settings and shortcut-mode switching; icons
        // reuse the semantic symbols from the settings sidebar.
        let service_separator: *mut AnyObject = msg_send![class!(NSMenuItem), separatorItem];
        let _: () = msg_send![menu, addItem: service_separator];
        build_service_menu(menu, menu_target);
        let service_separator: *mut AnyObject = msg_send![class!(NSMenuItem), separatorItem];
        let _: () = msg_send![menu, addItem: service_separator];

        // Shortcut toggle item
        let shortcut_title = make_nsstring(&t("menu.toggle_shortcut.cmd"));
        let shortcut_key = make_nsstring("");
        let shortcut_item: *mut AnyObject = msg_send![class!(NSMenuItem), alloc];
        let shortcut_item: *mut AnyObject = msg_send![shortcut_item, initWithTitle: shortcut_title, action: sel!(handleToggleShortcut:), keyEquivalent: shortcut_key];
        CFRelease(shortcut_title as *const c_void);
        CFRelease(shortcut_key as *const c_void);
        let _: () = msg_send![shortcut_item, setTarget: menu_target];
        set_menu_item_title(shortcut_item, &t("menu.toggle_shortcut.cmd"));
        let _: () = msg_send![menu, addItem: shortcut_item];
        with_menu_ui(|ui| {
            ui.shortcut = Some(ShortcutState {
                item: shortcut_item,
            });
        });

        // Thumbnail/icon-only mode toggle, placed immediately below the shortcut toggle.
        let thumbnail_title_key = if CONFIG.read().unwrap().layout.thumbnails_enabled {
            "menu.toggle_thumbnail_mode.to_icons"
        } else {
            "menu.toggle_thumbnail_mode.to_thumbnails"
        };
        let thumbnail_title = make_nsstring(&t(thumbnail_title_key));
        let thumbnail_key = make_nsstring("");
        let thumbnail_item: *mut AnyObject = msg_send![class!(NSMenuItem), alloc];
        let thumbnail_item: *mut AnyObject = msg_send![thumbnail_item, initWithTitle: thumbnail_title, action: sel!(handleToggleThumbnail:), keyEquivalent: thumbnail_key];
        CFRelease(thumbnail_title as *const c_void);
        CFRelease(thumbnail_key as *const c_void);
        let _: () = msg_send![thumbnail_item, setTarget: menu_target];
        set_menu_item_title(thumbnail_item, &t(thumbnail_title_key));
        let _: () = msg_send![menu, addItem: thumbnail_item];
        with_menu_ui(|ui| {
            ui.thumbnail = Some(ThumbnailState {
                item: thumbnail_item,
            });
        });

        // Clipboard pin item: sits with the service toggles it relates to, but carries its own
        // state (the pin is in-process, not persisted configuration).
        let pin_item = build_clipboard_pin_item(menu, menu_target);
        with_menu_ui(|ui| ui.clipboard_pin = Some(pin_item));

        // Reload Config item
        let reload_title = make_nsstring(&t("menu.reload_config"));
        let reload_key = make_nsstring("");
        let reload_item: *mut AnyObject = msg_send![class!(NSMenuItem), alloc];
        let reload_item: *mut AnyObject = msg_send![reload_item, initWithTitle: reload_title, action: sel!(handleReloadConfig:), keyEquivalent: reload_key];
        CFRelease(reload_title as *const c_void);
        CFRelease(reload_key as *const c_void);
        let _: () = msg_send![reload_item, setTarget: menu_target];
        set_menu_item_title(reload_item, &t("menu.reload_config"));
        let _: () = msg_send![menu, addItem: reload_item];

        // Clear Icon Cache item
        let clear_cache_title = make_nsstring(&t("menu.clear_caches"));
        let clear_cache_key = make_nsstring("");
        let clear_cache_item: *mut AnyObject = msg_send![class!(NSMenuItem), alloc];
        let clear_cache_item: *mut AnyObject = msg_send![clear_cache_item, initWithTitle: clear_cache_title, action: sel!(handleClearCaches:), keyEquivalent: clear_cache_key];
        CFRelease(clear_cache_title as *const c_void);
        CFRelease(clear_cache_key as *const c_void);
        let _: () = msg_send![clear_cache_item, setTarget: menu_target];
        set_menu_item_title(clear_cache_item, &t("menu.clear_caches"));
        let _: () = msg_send![menu, addItem: clear_cache_item];

        // Separator
        let sep_item: *mut AnyObject = msg_send![class!(NSMenuItem), separatorItem];
        let _: () = msg_send![menu, addItem: sep_item];

        // Quit item
        let quit_title = make_nsstring(&t("menu.quit"));
        // Bind Cmd+Q: only effective while this app is frontmost (e.g. settings window open) --
        // when it's a background menu-bar app, Cmd+Q goes to the frontmost app per macOS semantics.
        let quit_key = make_nsstring("q");
        let quit_item: *mut AnyObject = msg_send![class!(NSMenuItem), alloc];
        let quit_item: *mut AnyObject = msg_send![quit_item, initWithTitle: quit_title, action: sel!(handleQuit:), keyEquivalent: quit_key];
        CFRelease(quit_title as *const c_void);
        CFRelease(quit_key as *const c_void);
        let _: () = msg_send![quit_item, setTarget: menu_target];
        set_menu_item_title(quit_item, &t("menu.quit"));
        let _: () = msg_send![menu, addItem: quit_item];

        // register fixed-title items for locale hot-reload
        with_menu_ui(|ui| {
            ui.fixed = Some(FixedMenuItems {
                settings: settings_item,
                reload: reload_item,
                clear_cache: clear_cache_item,
                quit: quit_item,
            });
        });

        let _: () = msg_send![status_item, setMenu: menu];

        // Also set as mainMenu so AppKit's Cmd+Q keyEquivalent dispatch can find the Quit item.
        // An accessory app's mainMenu is NOT shown in the menu bar's app area (that only shows
        // regular apps' menus), but key-equivalent routing still works -- same mechanism as
        // LinearMouse's storyboard mainMenu.
        let nsapp: *mut AnyObject = msg_send![class!(NSApplication), sharedApplication];
        let _: () = msg_send![nsapp, setMainMenu: menu];

        // Pump run loop to let SystemUIServer connect
        for _ in 0..10 {
            CFRunLoopRunInMode(kCFRunLoopDefaultMode, 0.001, 1u8);
        }
    }
}

/// The "grant keychain access" action (settings banner, picker empty state, About page): re-read the
/// clipboard key on the clipboard worker, which raises the system's authorization prompt once.
pub(crate) extern "C" fn handle_retry_keychain_access(
    _this: *mut c_void,
    _cmd: Sel,
    _sender: *mut c_void,
) {
    crate::clipboard::retry_keychain_access();
}

/// The storage notice's "clear and restart" action (settings banner and picker empty state): delete
/// the storage set through the purge transaction and start a fresh history. Offered only while the
/// state admits it (see `StorageAction`); the handler re-checks, because the state can recover
/// between the click and the call.
pub(crate) extern "C" fn handle_clear_clipboard_storage(
    _this: *mut c_void,
    _cmd: Sel,
    _sender: *mut c_void,
) {
    crate::clipboard::clear_storage_and_restart();
}

/// Open System Settings -> Privacy & Security -> Accessibility (deep link).
/// Shared by the startup alert and the settings warning banner's button.
pub(crate) fn open_privacy_accessibility() {
    unsafe {
        let url_str = make_nsstring(
            "x-apple.systempreferences:com.apple.preference.security?Privacy_Accessibility",
        );
        let url: *mut AnyObject = msg_send![class!(NSURL), URLWithString: url_str];
        CFRelease(url_str as *const c_void);
        if !url.is_null() {
            let ws: *mut AnyObject = msg_send![class!(NSWorkspace), sharedWorkspace];
            let _: bool = msg_send![ws, openURL: url];
        }
    }
}

/// Open System Settings -> Privacy & Security -> Screen & System Audio Recording.
pub(crate) fn open_privacy_screen_recording() {
    unsafe {
        let url_str = make_nsstring(
            "x-apple.systempreferences:com.apple.preference.security?Privacy_ScreenCapture",
        );
        let url: *mut AnyObject = msg_send![class!(NSURL), URLWithString: url_str];
        CFRelease(url_str as *const c_void);
        if !url.is_null() {
            let ws: *mut AnyObject = msg_send![class!(NSWorkspace), sharedWorkspace];
            let _: bool = msg_send![ws, openURL: url];
        }
    }
}

/// Prompts the user with a custom alert at launch if Accessibility permission is missing.
/// The event-monitor thread is already retrying in the background; once the user grants
/// permission the tap is created automatically - no restart needed.
fn prompt_accessibility_if_needed() {
    if has_accessibility_permission() {
        return;
    }
    // AppState::new already logged "No accessibility permission."; this only shows the alert.
    // Accessory apps have no Dock icon; activate so the alert isn't hidden behind other windows.
    unsafe {
        let nsapp: *mut AnyObject = msg_send![class!(NSApplication), sharedApplication];
        let _: () = msg_send![nsapp, activateIgnoringOtherApps: true];
    }
    let open = confirm_alert(
        &t("alert.accessibility_title"),
        &t("alert.accessibility_msg"),
        &t("alert.accessibility_open"),
        &t("alert.accessibility_later"),
    );
    if open {
        open_privacy_accessibility();
    }
}

/// Resolves the development switch `--open-settings[=<name|index>]`, forwarded by
/// `scripts/dev-restart.sh` (argv is the only channel; see the dev_flags module). Returns a sidebar
/// page index, or None when the switch is absent or names no page, so a normal launch is unaffected.
///
/// The name and the index both come from `settings::SETTINGS_PAGES`, so this switch cannot drift from
/// the sidebar it addresses; a bad value is refused rather than clamped to a different page, and the
/// refusal is logged because a typo would otherwise look exactly like a switch that did nothing.
fn open_settings_page_request() -> Option<usize> {
    let spec = crate::dev_flags::value("open-settings")?;
    match crate::settings::parse_settings_page_spec(&spec) {
        Some(page) => Some(page),
        None => {
            log_info!(
                "[dev] --open-settings={spec} names no page; the settings window stays closed"
            );
            None
        }
    }
}

/// Whether the development switch `--show-clipboard` was forwarded by `scripts/dev-restart.sh`.
/// The clipboard picker otherwise only appears through a global hotkey, which a script cannot
/// press without stealing focus, so this is the only way to verify its laid-out UI (the footer's
/// shortcut legends, the row grid) from a script or cua.
fn show_clipboard_request() -> bool {
    crate::dev_flags::present("show-clipboard")
}

/// Whether the development switch `--show-overlay` was forwarded by `scripts/dev-restart.sh`.
///
/// The switcher only appears through the global hotkey, which a script cannot press without stealing
/// focus, so this is the only way to reach its laid-out UI (the footer's settings gear) from a real
/// pointer gesture. Unlike `--smoke-overlay`, which drives its assertions and exits, this leaves the
/// panel standing so an A2 scenario can click it.
fn show_overlay_request() -> bool {
    crate::dev_flags::present("show-overlay")
}

/// `--show-clipboard=after:N` opens the picker N seconds after launch instead of immediately. The
/// storage load runs off the main thread and can take seconds (a keychain read may block), so this
/// is how the "load finished first, picker opened later" ordering is reached without a hotkey.
fn show_clipboard_delay() -> Option<f64> {
    crate::dev_flags::value("show-clipboard")
        .and_then(|value| value.strip_prefix("after:").map(str::to_string))
        .and_then(|rest| rest.parse::<f64>().ok())
}

#[cfg(target_arch = "aarch64")]
type AppKitBool = bool;
#[cfg(target_arch = "x86_64")]
type AppKitBool = i8;

// BOOL is `_Bool` on arm64 and signed char on Intel macOS; class_addMethod needs the matching
// runtime encoding as well as the matching callback ABI.
#[cfg(target_arch = "aarch64")]
const APP_REOPEN_METHOD_ENCODING: &str = "B@:@B";
#[cfg(target_arch = "x86_64")]
const APP_REOPEN_METHOD_ENCODING: &str = "c@:@c";

#[cfg(target_arch = "aarch64")]
fn appkit_bool(value: bool) -> AppKitBool {
    value
}

#[cfg(target_arch = "x86_64")]
fn appkit_bool(value: bool) -> AppKitBool {
    value as i8
}

#[cfg(target_arch = "aarch64")]
fn appkit_bool_value(value: AppKitBool) -> bool {
    value
}

#[cfg(target_arch = "x86_64")]
fn appkit_bool_value(value: AppKitBool) -> bool {
    value != 0
}

extern "C" fn on_application_should_handle_reopen(
    this: *mut c_void,
    _cmd: Sel,
    _application: *mut c_void,
    _has_visible_windows: AppKitBool,
) -> AppKitBool {
    callback_guard::void("application_should_handle_reopen", || {
        if is_main_thread() {
            open_settings_for_reopen();
        } else {
            unsafe {
                let _: () = msg_send![
                    this as *mut AnyObject,
                    performSelectorOnMainThread: sel!(handleAppReopenOnMainThread:),
                    withObject: std::ptr::null::<AnyObject>(),
                    waitUntilDone: false
                ];
            }
        }
    });
    // The callback performs the custom reopen action; NO suppresses AppKit's generic behavior.
    appkit_bool(false)
}

extern "C" fn on_app_reopen_on_main_thread(_this: *mut c_void, _cmd: Sel, _sender: *mut c_void) {
    callback_guard::void("app_reopen_on_main_thread", open_settings_for_reopen);
}

fn open_settings_for_reopen() {
    debug_assert_main_thread();
    if onboarding::is_visible() {
        onboarding::finish_for_settings();
    }
    settings::reopen_for_app_activation(updater::wait_for_quit_active());
}

fn application_reopen_smoke() -> bool {
    unsafe {
        let app: *mut AnyObject = msg_send![class!(NSApplication), sharedApplication];
        let delegate: *mut AnyObject = msg_send![app, delegate];
        if delegate.is_null() {
            return false;
        }
        let responds: bool = msg_send![
            delegate,
            respondsToSelector: sel!(applicationShouldHandleReopen:hasVisibleWindows:)
        ];
        if !responds {
            return false;
        }
        let handled: AppKitBool = msg_send![
            delegate,
            applicationShouldHandleReopen: app,
            hasVisibleWindows: appkit_bool(false)
        ];
        if appkit_bool_value(handled) || !settings::settings_window_is_visible() {
            return false;
        }

        settings::show_settings_page(settings::SETTINGS_ABOUT_PAGE_INDEX);
        let handled: AppKitBool = msg_send![
            delegate,
            applicationShouldHandleReopen: app,
            hasVisibleWindows: appkit_bool(true)
        ];
        if appkit_bool_value(handled) {
            return false;
        }
        // The reopen's own result is asserted first and kept: the window must still be up and still on
        // the page the reopen left it on. Adding the clipboard navigation below must not replace this --
        // it did once, and then a reopen that wrongly reset the page could still pass.
        if !settings::settings_window_is_visible()
            || settings::e2e_selected_sidebar() != settings::SETTINGS_ABOUT_PAGE_INDEX
        {
            return false;
        }
        // The clipboard picker's header button calls exactly this: from a DIFFERENT page, because a
        // sidebar already sitting on the target page cannot tell navigation from a button whose action
        // did nothing. The settings window is live here (this process ran the lifecycle), so both the
        // page and the window are real facts.
        settings::show_settings_page(0);
        if settings::e2e_selected_sidebar() != 0 {
            return false;
        }
        // The mouse page's controls are filled only when that page is the one in use: opening on General
        // and then on Clipboard must leave them unfilled, and asking for the Mouse page must fill them
        // (before this change every open resolved a device and rendered its mappings).
        if settings::e2e_mouse_page_materialized() {
            return false;
        }
        settings::show_settings_page(settings::SETTINGS_MOUSE_PAGE_INDEX);
        if !settings::e2e_mouse_page_materialized()
            || settings::e2e_mouse_line_count_text().is_empty()
        {
            return false;
        }
        // The startup prewarm must be a no-op on a window that already exists: a prewarmed window is left
        // on the page the user chose, with the page's controls still filled. Re-entering the build/load
        // path here would park the window back on General and hand the user a different page.
        let mouse_line_count = settings::e2e_mouse_line_count_text();
        settings::prewarm_settings_window();
        if !settings::settings_window_is_visible()
            || settings::e2e_selected_sidebar() != settings::SETTINGS_MOUSE_PAGE_INDEX
            || !settings::e2e_mouse_page_materialized()
            || settings::e2e_mouse_line_count_text() != mouse_line_count
        {
            return false;
        }
        // A config reload must make the filled, clean mouse page stale even while ANOTHER page is on screen.
        // The page is expensive to fill, so it is filled on demand; the failure this guards is the one where
        // the reload only refilled a mouse page that was already showing and marked the page clean anyway,
        // leaving the user looking at the values the reload replaced.
        settings::show_settings_page(settings::SETTINGS_CLIPBOARD_PAGE_INDEX);
        // The master switch, not the resolved line count: it is read straight from the config section, so
        // the assertion cannot be confounded by which profile the resolver picks for this machine.
        let previous_enabled = CONFIG.read().unwrap().mouse.enabled;
        let reloaded_enabled = !previous_enabled;
        CONFIG.write().unwrap().mouse.enabled = reloaded_enabled;
        settings::refresh_switcher_keystroke_and_mouse_controls_from_config();
        settings::show_settings_page(settings::SETTINGS_MOUSE_PAGE_INDEX);
        let after_reload = settings::e2e_mouse_enabled_state();
        if after_reload != isize::from(reloaded_enabled) {
            eprintln!(
                "[smoke-app-reopen] the mouse page was not refilled after a config reload: switch={after_reload}, expected {reloaded_enabled}"
            );
            return false;
        }
        // The plug/unplug entry (what `on_devices_changed_inner` calls) must stale the page the same way.
        CONFIG.write().unwrap().mouse.enabled = previous_enabled;
        settings::show_settings_page(settings::SETTINGS_CLIPBOARD_PAGE_INDEX);
        settings::refresh_device_popup_if_open();
        settings::show_settings_page(settings::SETTINGS_MOUSE_PAGE_INDEX);
        let after_device_change = settings::e2e_mouse_enabled_state();
        if after_device_change != isize::from(previous_enabled) {
            eprintln!(
                "[smoke-app-reopen] a device change did not stale the mouse page: switch={after_device_change}, expected {previous_enabled}"
            );
            return false;
        }
        // Back to Clipboard for the navigation assertions below.
        settings::show_clipboard_page();
        settings::settings_window_is_visible()
            // ...and it must have APPEARED on the clipboard page: the step above left the window presented
            // on General, and a present-then-select path would store General here while the final selection
            // still read 3.
            && settings::e2e_presented_page() == Some(settings::SETTINGS_CLIPBOARD_PAGE_INDEX)
            && settings::e2e_selected_sidebar() == settings::SETTINGS_CLIPBOARD_PAGE_INDEX
    }
}

/// Emit the one line that names the running process and the build it reports to itself.
///
/// Read by `scripts/dev-restart.sh` to prove a launch reached the build that was just made: the
/// plist on disk can describe a build this process never loaded. Every field comes from the process
/// itself — its own pid, its own executable path, its own main bundle — so a record can only ever
/// describe the process that wrote it. This is an identity claim, not a readiness one.
fn log_launch_identity() {
    let executable = std::env::current_exe()
        .map(|path| path.to_string_lossy().into_owned())
        .unwrap_or_default();
    // NSBundle is main-thread only, which is where `run` executes.
    let (version, identifier) = unsafe {
        (
            crate::ffi::bundle_info_string("CFBundleVersion"),
            crate::ffi::bundle_info_string("CFBundleIdentifier"),
        )
    };
    log_info!(
        "{}",
        logger::launch_identity_line(std::process::id(), &executable, &version, &identifier)
    );
}

/// App entry: the full startup/smoke/run loop (called by the thin bin wrapper; behavior
/// is unchanged by the lib/bin split).
pub fn run() {
    let is_smoke_process = crate::dev_flags::any_prefix("--smoke-");
    // Every smoke launch runs the isolated clipboard session, so no runner has to remember a
    // switch: the developer's own config normally has the clipboard enabled, and startup would
    // otherwise load their real (encrypted) history and read the master key from the login
    // keychain -- which prompts, because the cargo binary a smoke spawns is not the build the
    // key item's ACL admits. Smoke mode keeps the history and the image cache in a process-scoped
    // temporary directory and uses the fixed test key instead (see `clipboard::smoke`).
    if is_smoke_process {
        clipboard::set_smoke_mode();
    }
    // GUI smoke entry points are test subprocesses and may run alongside the development app;
    // every normal launch channel shares one lock.
    let is_gui_smoke_process = std::env::args().any(|arg| {
        arg == "--smoke-clipboard"
            || arg == "--smoke-overlay"
            || arg == "--smoke-keystroke-display-panel"
            || arg == "--smoke-settings-layout"
            || arg == "--smoke-settings-state-sync"
            || arg == "--smoke-settings-collapsible-row"
            || arg == "--smoke-settings-mouse-profile-callback"
            || arg == "--smoke-app-reopen"
            || arg == "--smoke-onboarding-live-apply"
            || arg == "--smoke-update-prompts"
            || arg == "--smoke-smooth-scroll-event"
            || arg == "--smoke-card-badges"
            || arg == "--smoke-selection-ring"
            || arg == "--smoke-space-state-matrix"
            || arg == "--smoke-panel-keys"
    });
    let _instance_guard = if is_gui_smoke_process {
        None
    } else {
        match single_instance::acquire() {
            Ok(guard) => Some(guard),
            Err(error) => {
                eprintln!("[single-instance] startup refused: {error}");
                if is_smoke_process {
                    std::process::exit(1);
                }
                return;
            }
        }
    };

    // Smoke-test entry (--smoke-clipboard): show the clipboard picker twice on the real main
    // thread inside a real NSApplication (exercising rebuild_rows' row cleanup), exit(0) on
    // success. Invoked as a subprocess by the #[ignore] test in clipboard.rs -- the test
    // harness's worker threads trip AppKit's main-thread guard.
    if std::env::args().any(|a| a == "--smoke-clipboard") {
        unsafe {
            let pool: *mut AnyObject = msg_send![class!(NSAutoreleasePool), new];
            init_app();
            // The isolated clipboard session is already on: every `--smoke-*` launch enables it
            // (see the top of `run`), so injected entries can never touch the user's real data.
            drop(CONFIG.read().unwrap()); // force CONFIG initialization, matching a normal launch
            let ok = clipboard::smoke_runner();
            let _: () = msg_send![pool, drain];
            if !ok {
                eprintln!("[smoke-clipboard] picker smoke failed");
                std::process::exit(1);
            }
            std::process::exit(0);
        }
    }

    // 1. Init NSApplication as accessory (no dock icon)
    init_app();

    //     Init logger: before everything else, read log level from CONFIG, auto-detect dev/prod.
    {
        let cfg = CONFIG.read().unwrap(); // triggers LazyLock init + config load
        let level = match cfg.logging.level.as_str() {
            "debug" => logger::LogLevel::Debug,
            _ => logger::LogLevel::Info,
        };
        let is_dev = std::env::current_exe()
            .map(|p| p.to_string_lossy().contains("target/"))
            .unwrap_or(false);
        logger::init(
            &logger::LogConfig {
                level,
                file_path: cfg.logging.file_path.clone(),
            },
            is_dev,
        );
    }
    log_launch_identity();

    // 2. Register custom ObjC classes
    overlay::register_card_class();

    // Force CONFIG init (also applies i18n locale) so the menu is built with the configured
    // locale. CONFIG's LazyLock init calls i18n::apply_config_locale; no cycle (see i18n.rs).
    drop(CONFIG.read().unwrap());

    // 3. Setup status bar menu
    setup_status_bar();
    // Apply the saved icon visibility once the item exists. Later changes arrive through
    // runtime_config's menu_bar_icon flag, which calls the same function.
    crate::menu::set_status_item_visible(!CONFIG.read().unwrap().startup.hide_menu_bar_icon);

    // 4. Initialize state
    ensure_icon_cache_dir();
    // One-shot cleanup of legacy PID-named cache files (purely-numeric-stem .png); useless to
    // the new version and just take up space.
    migrate_legacy_cache();
    cache_running_app_icons(); // pre-warm icon cache for all running apps
                               // Pre-warm the clipboard header's small icons: only when the clipboard feature is enabled,
                               // otherwise skip (no point extracting for a disabled feature).
    if CONFIG.read().unwrap().clipboard.enabled {
        cache_running_app_icons_small();
    }

    // 4b. Force CONFIG to initialise and report any validation errors
    {
        let cfg = CONFIG.read().unwrap();
        // First load already happened via LazyLock; re-run validate to report problems
        let errs = cfg.validate();
        if !errs.is_empty() {
            log_info!(
                "Config errors in ~/.config/oh-my-tab/config.toml ({} issue(s)):",
                errs.len()
            );
            for e in &errs {
                log_info!("  • {}", e);
            }
            log_info!("Using defaults for invalid fields.");
        }
    }

    let initial_state = AppState::new();
    WINDOW_COUNT.store(initial_state.windows.len(), Ordering::Release);
    with_tab_state(|state| *state = Some(initial_state));

    // 5. Create overlay window (hidden initially)
    let window = create_overlay_window();
    *OVERLAY_WINDOW.lock().unwrap() = Some(ObjPtr::new(window));
    // Hide initially
    hide_overlay();
    // A click outside the overlay cancels the switch (dismissed when the panel loses key).
    install_click_to_cancel();

    // 6. Create controller object
    let controller = create_controller();
    *CONTROLLER.lock().unwrap() = Some(CallbackTarget::new(controller));
    // This process-lifetime controller already owns input and notification callbacks; install the
    // same object as NSApplication's delegate before finishLaunching and the main event loop.
    unsafe {
        let nsapp: *mut AnyObject = msg_send![class!(NSApplication), sharedApplication];
        let _: () = msg_send![nsapp, setDelegate: controller];
    }

    // Sparkle is loaded dynamically so a source checkout can still run without the native
    // framework. Release/dev bundles that contain Contents/Frameworks/Sparkle.framework get the
    // standard updater UI and automatic checks; the About page reports a clear setup hint when
    // the framework is not present yet.
    updater::initialize(CONFIG.read().unwrap().updates.automatically_check);

    // One-shot instrument for the synthetic Dock swipe (`--space-swipe=left|right`): it posts the
    // gesture after the app is up and logs the Space before and after, so the mechanism can be
    // verified without a trackpad. Nothing in the switch path calls it.
    if let Some(direction) = crate::dev_flags::value("space-swipe") {
        std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(2000));
            crate::space_swipe::probe(&direction);
        });
    }

    // Window-state matrix instrument. Record mode walks the app's own probe window through every
    // state the decode distinguishes and prints the raw WindowServer fields (the source command for
    // the bit semantics documented in window_collector::window_state); the smoke mode asserts the
    // pinned matrix. Both need a GUI session and the main thread, and both restore the window.
    if crate::dev_flags::present("space-state-record")
        || crate::dev_flags::present("smoke-space-state-matrix")
    {
        unsafe {
            let nsapp: *mut AnyObject = msg_send![class!(NSApplication), sharedApplication];
            let _: () = msg_send![nsapp, finishLaunching];
        }
        let recording = crate::dev_flags::present("space-state-record");
        let mode = if recording {
            window_collector::ProbeMode::Record
        } else {
            window_collector::ProbeMode::Matrix
        };
        let ok = window_collector::run_space_state_probe(mode);
        if !ok {
            eprintln!("[space-state] the state matrix did not hold");
            std::process::exit(1);
        }
        // The record mode makes no judgement, so it must not claim the matrix held.
        if recording {
            log_info!("[space-state] recording complete (no assertions were made)");
        } else {
            log_info!("[space-state] the state matrix held");
        }
        std::process::exit(0);
    }

    // Smoke-test entry (--smoke-card-badges): build real cards from controlled window input and
    // assert the preview-corner badges match the card's state. Needs a GUI session (AppKit view
    // tree) and no window collection at all, so it can cover the icon-fallback branch that a card
    // of another desktop takes.
    if crate::dev_flags::present("smoke-card-badges") {
        unsafe {
            let nsapp: *mut AnyObject = msg_send![class!(NSApplication), sharedApplication];
            let _: () = msg_send![nsapp, finishLaunching];
        }
        if !overlay::smoke_card_badges() {
            eprintln!("[smoke-card-badges] the preview badges do not match the card state");
            std::process::exit(1);
        }
        log_info!("[smoke-card-badges] preview badges match the card state");
        std::process::exit(0);
    }

    // Smoke-test entry (--smoke-selection-ring): build real cards from controlled window input in
    // both layouts and assert the selected-state ring exists with the documented concentric
    // geometry. Needs a GUI session (AppKit view tree) and no window collection at all.
    if crate::dev_flags::present("smoke-selection-ring") {
        unsafe {
            let nsapp: *mut AnyObject = msg_send![class!(NSApplication), sharedApplication];
            let _: () = msg_send![nsapp, finishLaunching];
        }
        if !overlay::smoke_selection_ring() {
            eprintln!(
                "[smoke-selection-ring] a card layout is missing the documented selection ring"
            );
            std::process::exit(1);
        }
        log_info!("[smoke-selection-ring] both card layouts build the documented selection ring");
        std::process::exit(0);
    }

    if crate::dev_flags::present("smoke-update-prompts") {
        unsafe {
            let nsapp: *mut AnyObject = msg_send![class!(NSApplication), sharedApplication];
            let _: () = msg_send![nsapp, finishLaunching];
        }
        if !updater::smoke_update_prompt_layouts() {
            eprintln!("[smoke-update-prompts] one or more localized prompt buttons do not fit");
            std::process::exit(1);
        }
        log_info!("[smoke-update-prompts] all update prompt stages fit in all locales");
        std::process::exit(0);
    }

    // Start bounded workers for post-activation AX focus queries so notification bursts do not
    // create large numbers of threads.
    start_activation_focus_scheduler();
    window_server::start();
    let initial_subscriptions = window_collector::window_server_candidates();
    window_server::update_subscriptions(&initial_subscriptions);
    // Register the overlay presentation hooks: window_refresh keeps only the one-way
    // overlay -> window_refresh trigger direction; the reverse show/rebuild actions are
    // injected here at startup (see window_refresh::OverlayPresenter).
    window_refresh::register_overlay_presenter(window_refresh::OverlayPresenter {
        card_close_in_progress: overlay::card_close_in_progress,
        commit_first_summon: overlay::commit_first_summon,
        show_first_summon: overlay::show_first_summon,
        take_display_relayout_pending: overlay::take_display_relayout_pending,
        reset_thumbnail_state: overlay::reset_thumbnail_state,
        show_overlay: overlay::show_overlay,
        refresh_highlight: overlay::refresh_highlight,
    });
    window_refresh::start_lifecycle_backstop();

    // 6b. Listen for system app activation so MRU stays in sync
    // when the user switches apps via Dock, Cmd+Tab, etc.
    unsafe {
        let ws: *mut AnyObject = msg_send![class!(NSWorkspace), sharedWorkspace];
        let nc: *mut AnyObject = msg_send![ws, notificationCenter];
        let name = make_nsstring("NSWorkspaceDidActivateApplicationNotification");
        let _: () = msg_send![nc,
            addObserver: controller,
            selector: sel!(handleAppActivation:),
            name: name,
            object: std::ptr::null::<AnyObject>(),
        ];
        CFRelease(name as *const c_void);

        // Pre-cache icons for apps launched after startup so they're ready
        // before the user summons the switcher (fixes missing icons for apps
        // opened while oh-my-tab is already running).
        let launch_name = make_nsstring("NSWorkspaceDidLaunchApplicationNotification");
        let _: () = msg_send![nc,
            addObserver: controller,
            selector: sel!(handleAppLaunch:),
            name: launch_name,
            object: std::ptr::null::<AnyObject>(),
        ];
        CFRelease(launch_name as *const c_void);

        // App-terminate notice: the thumbnail module cancels pending captures and
        // removes the app's observer/cache entries.
        let term_name = make_nsstring("NSWorkspaceDidTerminateApplicationNotification");
        let _: () = msg_send![nc,
            addObserver: controller,
            selector: sel!(handleAppTerminate:),
            name: term_name,
            object: std::ptr::null::<AnyObject>(),
        ];
        CFRelease(term_name as *const c_void);

        // Space-change notifications use NSWorkspace's notification center. Collection adds a
        // fullscreen-scoped onscreen-window flip fallback for setups where fullscreen enters omit
        // this notification (see space_transition::SpaceFlipDetector).
        let space_name = make_nsstring("NSWorkspaceActiveSpaceDidChangeNotification");
        let _: () = msg_send![nc,
            addObserver: controller,
            selector: sel!(handleActiveSpaceChanged:),
            name: space_name,
            object: ws,
        ];
        CFRelease(space_name as *const c_void);

        // Listen for system language changes to live-follow when locale is auto.
        // NSLocaleCurrentLocaleDidChangeNotification is posted to the default notification center
        // (not the workspace center), so use NSNotificationCenter defaultCenter.
        let default_nc: *mut AnyObject = msg_send![class!(NSNotificationCenter), defaultCenter];
        let locale_name = make_nsstring("NSLocaleCurrentLocaleDidChangeNotification");
        let _: () = msg_send![default_nc,
            addObserver: controller,
            selector: sel!(handleLocaleChanged:),
            name: locale_name,
            object: std::ptr::null::<AnyObject>(),
        ];
        CFRelease(locale_name as *const c_void);

        // Observe scroller-style changes: a runtime preference/input-device switch makes AppKit
        // re-tile existing scroll views as space-taking legacy, clipping the right column. Re-assert
        // overlay here and re-lay out when needed (see the scroller module).
        let scroller_name = make_nsstring("NSScrollerPreferredScrollerStyleDidChangeNotification");
        let _: () = msg_send![default_nc,
            addObserver: controller,
            selector: sel!(handleScrollerStyleChanged:),
            name: scroller_name,
            object: std::ptr::null::<AnyObject>(),
        ];
        CFRelease(scroller_name as *const c_void);
        // Note: `NSScrollerPreferredScrollerStyleDidChangeNotification` is never delivered (verified
        // with logging on 2026-09-22), while window-did-become-key and app-did-become-active are, and
        // those cover the real scenario (the user plugs in a mouse or changes the preference and then
        // returns to the settings window). The sync therefore hangs off these two: every activation
        // measures the scroller footprint and re-lays out the content when it changed (see the
        // scroller module).
        for probe_name in [
            "NSWindowDidBecomeKeyNotification",
            "NSApplicationDidBecomeActiveNotification",
        ] {
            let name_ns = make_nsstring(probe_name);
            let _: () = msg_send![default_nc,
                addObserver: controller,
                selector: sel!(handleScrollerStyleChanged:),
                name: name_ns,
                object: std::ptr::null::<AnyObject>(),
            ];
            CFRelease(name_ns as *const c_void);
        }
        // The binding engine's tap thread has to know whether this app is frontmost BEFORE it swallows
        // a press, so that a window action's chord reaches the focused control while our own window is
        // up. Published from these notifications, which are the authoritative transition.
        for (probe_name, selector) in [
            (
                "NSApplicationDidBecomeActiveNotification",
                sel!(handleSelfBecameActive:),
            ),
            (
                "NSApplicationDidResignActiveNotification",
                sel!(handleSelfResignedActive:),
            ),
        ] {
            let name_ns = make_nsstring(probe_name);
            let _: () = msg_send![default_nc,
                addObserver: controller,
                selector: selector,
                name: name_ns,
                object: std::ptr::null::<AnyObject>(),
            ];
            CFRelease(name_ns as *const c_void);
        }

        // Observe effective appearance changes so theme=auto follows light/dark mode without
        // requiring a restart.
        let appearance_name =
            make_nsstring("NSApplicationDidChangeEffectiveAppearanceNotification");
        let _: () = msg_send![default_nc,
            addObserver: controller,
            selector: sel!(handleAppearanceChanged:),
            name: appearance_name,
            object: std::ptr::null::<AnyObject>(),
        ];
        CFRelease(appearance_name as *const c_void);

        // Once a window explicitly sets Aqua/DarkAqua, the app's effectiveAppearance may stay
        // unchanged. macOS broadcasts the system-level theme switch through the distributed
        // notification center, so observe that notification as well.
        let distributed_nc: *mut AnyObject =
            msg_send![class!(NSDistributedNotificationCenter), defaultCenter];
        let layout_changed = keystroke_display::mapping::layout_changed_notification();
        let _: () = msg_send![distributed_nc,
            addObserver: controller,
            selector: sel!(handleKeyboardLayoutChanged:),
            name: layout_changed as *mut AnyObject,
            object: std::ptr::null::<AnyObject>(),
        ];
        keystroke_display::mapping::refresh_layout_cache();
        let distributed_theme_name = make_nsstring("AppleInterfaceThemeChangedNotification");
        let _: () = msg_send![distributed_nc,
            addObserver: controller,
            selector: sel!(handleAppearanceChanged:),
            name: distributed_theme_name,
            object: std::ptr::null::<AnyObject>(),
        ];
        CFRelease(distributed_theme_name as *const c_void);

        // Observe screen-parameter changes (display plug/unplug, resolution changes):
        // both the thumbnail cache and the overlay layout depend on screen geometry;
        // a change forces one debounced refresh through overlay's entry point.
        let screen_params_name =
            make_nsstring("NSApplicationDidChangeScreenParametersNotification");
        let _: () = msg_send![default_nc,
            addObserver: controller,
            selector: sel!(handleScreenParametersChanged:),
            name: screen_params_name,
            object: std::ptr::null::<AnyObject>(),
        ];
        CFRelease(screen_params_name as *const c_void);

        // This process is about to quit: restore the system pointer values (same as LinearMouse's
        // applicationWillTerminate). Cmd+Q / logout / shutdown post this; a crash (SIGABRT) does
        // not -- that case self-heals on the next launch through the "unset writes the system
        // value back" logic.
        let will_terminate_name = make_nsstring("NSApplicationWillTerminateNotification");
        let _: () = msg_send![default_nc,
            addObserver: controller,
            selector: sel!(handleWillTerminate:),
            name: will_terminate_name,
            object: std::ptr::null::<AnyObject>(),
        ];
        CFRelease(will_terminate_name as *const c_void);
    }

    // 7. Start runtime permission supervision before any global input tap.
    input_monitor::start();
    // Input is coalesced into one main-thread drain callback.
    start_event_monitor();

    // The hover poll timer is started/stopped by the overlay itself (start_hover_timer
    // from show_overlay): the main-thread runloop reads the global cursor position every
    // 16ms and hit-tests the cards, independent of event delivery (moves while a side
    // button is held can't be obtained via any tap/tracking, verified).

    // Apply startup configuration and every optional runtime service through one entry point.
    let startup_cfg = CONFIG.read().unwrap().clone();
    runtime_config::apply_config_change(
        &config::Config::default(),
        &startup_cfg,
        runtime_config::ConfigChangeSource::Startup,
    );

    // Start memory sampling after optional modules so the 60s baseline represents the
    // complete feature profile.
    mem::start();

    // Global input is drained by handleGlobalInputDrain:; no bridge thread is needed.

    // Smoke-test entry (--smoke-overlay): after full init, drive a summon directly, then traverse,
    // wrap, and step backward once to cover continuous scrolling and bidirectional navigation. The
    // show is asynchronous (the window snapshot arrives on a worker and the presses in between are
    // ignored), so the runloop is pumped before the traversal. Ends with the selection on the last
    // window: that is the card the viewport must scroll to, and the one whose ring sits closest to the
    // status footer. Pump the main runloop for 2s so async thumbnail deliveries (thumbnailReady) land,
    // and exit(0) on survival. Synthetic keystrokes never reach the CGEventTap, so this path must be
    // driven internally.
    if std::env::args().any(|a| a == "--smoke-overlay") {
        unsafe {
            let nsapp: *mut AnyObject = msg_send![class!(NSApplication), sharedApplication];
            let _: () = msg_send![nsapp, finishLaunching];
            log_info!("[smoke-overlay] summoning");
            on_cmd_tab_pressed(
                std::ptr::null_mut(),
                sel!(handleCmdTabPressed:),
                std::ptr::null_mut(),
            );
            if let Some(scroller) = thumbnail_scroller() {
                let hidden: bool = msg_send![scroller.0, isHidden];
                let bounds: NSRect = msg_send![scroller.0, bounds];
                let geometry = thumbnail_scroller_geometry(
                    bounds.size.height,
                    thumbnail_scroller_max_offset(),
                    0.0,
                );
                log_info!(
                    "[smoke-overlay] scroller hidden={} frame={:.1}x{:.1} knob_h={:.1}",
                    hidden,
                    bounds.size.width,
                    bounds.size.height,
                    geometry.map_or(0.0, |g| g.knob_h)
                );
            }
            // Drive the same absolute-offset path to cover fractional positions and bottom
            // clamping; real dragging and wheel scrolling use this path too.
            for fraction in [0.35f64, 1.0f64] {
                thumbnail_scroller_set_fraction_for_smoke(fraction);
            }
            let rl: *mut AnyObject = msg_send![class!(NSRunLoop), currentRunLoop];
            let date: *mut AnyObject =
                msg_send![class!(NSDate), dateWithTimeIntervalSinceNow: 2.0f64];
            let _: () = msg_send![rl, runUntilDate: date];

            // Now that the overlay is up, the presses actually navigate: traverse (wrapping) and step
            // backward once, then settle on the last window.
            let window_count =
                with_tab_state(|state| state.as_ref().map_or(0, |state| state.windows.len()));
            for _ in 0..window_count.saturating_add(1) {
                on_cmd_tab_pressed(
                    std::ptr::null_mut(),
                    sel!(handleCmdTabPressed:),
                    std::ptr::null_mut(),
                );
            }
            on_cmd_shift_tab_pressed(
                std::ptr::null_mut(),
                sel!(handleCmdShiftTabPressed:),
                std::ptr::null_mut(),
            );
            let mut guard = 0usize;
            while guard <= window_count {
                let selected = with_tab_state(|state| {
                    state.as_ref().map_or(usize::MAX, |state| state.selected)
                });
                if selected + 1 >= window_count {
                    break;
                }
                on_cmd_tab_pressed(
                    std::ptr::null_mut(),
                    sel!(handleCmdTabPressed:),
                    std::ptr::null_mut(),
                );
                guard += 1;
            }
            // Let the navigation's scroll land before measuring the laid-out frames.
            let settle: *mut AnyObject =
                msg_send![class!(NSDate), dateWithTimeIntervalSinceNow: 0.3f64];
            let _: () = msg_send![rl, runUntilDate: settle];

            // Laid-out frames, not pixels: the bottom-most card must not be laid out below the card
            // viewport, or the status footer covers the row's own content.
            match crate::overlay::smoke_bottom_card_clears_the_selection_ring() {
                Ok(Some(clearance)) => log_info!(
                    "[smoke-overlay] bottom row keeps {:.1}pt clear of the card viewport",
                    clearance
                ),
                Ok(None) => log_info!(
                    "[smoke-overlay] no windows laid out; the selection-ring clearance not measured"
                ),
                Err(reason) => {
                    eprintln!("[smoke-overlay] selection ring clearance failed: {reason}");
                    std::process::exit(1);
                }
            }
            // The footer's settings gear must sit inside the band and clear of the status text. A
            // title long enough to reach the gear, or a corner inset that pushed the gear out of the
            // band, is exactly the layout defect this asserts on the real view tree.
            match crate::overlay::smoke_footer_settings_button_fits() {
                Ok(()) => {
                    log_info!("[smoke-overlay] the footer settings gear fits inside the band")
                }
                Err(reason) => {
                    eprintln!("[smoke-overlay] footer settings gear layout failed: {reason}");
                    std::process::exit(1);
                }
            }
            // The click path: action/target wiring, then a real mouse-down. The second probe is what
            // catches a mouseDown: override that forgot to call super -- the defect that shipped a
            // gear which looked right and did nothing.
            match crate::overlay::smoke_footer_settings_click_wiring() {
                Ok(()) => {
                    log_info!("[smoke-overlay] the footer settings gear is wired to its action")
                }
                Err(reason) => {
                    eprintln!("[smoke-overlay] footer settings gear click wiring failed: {reason}");
                    std::process::exit(1);
                }
            }
            match crate::overlay::smoke_footer_settings_mouse_down_arms() {
                Ok(()) => log_info!("[smoke-overlay] the footer settings gear receives mouse-down"),
                Err(reason) => {
                    eprintln!("[smoke-overlay] footer settings gear click failed: {reason}");
                    std::process::exit(1);
                }
            }
            // The selection must have been scrolled fully into the viewport: the rendered range
            // includes the teaser row below the viewport's bottom edge.
            match crate::overlay::smoke_selected_card_is_fully_visible() {
                Ok(clearance) => log_info!(
                    "[smoke-overlay] selected card is fully visible, {clearance:.1}pt above the viewport's bottom edge"
                ),
                Err(reason) => {
                    eprintln!("[smoke-overlay] selected card visibility failed: {reason}");
                    std::process::exit(1);
                }
            }
            // In-overlay action keys: the panel is now summoned and key, so drive the registered
            // performKeyEquivalent: with real NSEvents. This covers method registration, the
            // eligibility decision and the consumption rule; the system's own dispatch is the A2
            // scenario's job (this runner says so rather than implying otherwise).
            if std::env::args().any(|a| a == "--smoke-panel-keys")
                && !crate::overlay::smoke_panel_key_equivalent()
            {
                eprintln!("[smoke-panel-keys] the panel key handler contract failed");
                std::process::exit(1);
            }
            log_info!("[smoke-overlay] summon survived");
            std::process::exit(0);
        }
    }

    // Smoke-test entry (--smoke-panel-keys) on its own: the handler needs a real overlay window,
    // so it is exercised through the summon path above when both flags are given. Run alone, it
    // reports that it needs the window rather than silently passing.
    if std::env::args().any(|a| a == "--smoke-panel-keys") {
        eprintln!("[smoke-panel-keys] needs the overlay window: run with --smoke-overlay --smoke-panel-keys");
        std::process::exit(1);
    }

    // Settings layout smoke entry: open and traverse all eight pages on the real NSApplication
    // main thread so the runtime layout validator checks the final AppKit view tree. Requires a
    // GUI session; a panic/non-zero exit reports a failure.
    if std::env::args().any(|a| a == "--smoke-settings-layout") {
        unsafe {
            // The layout validator recognizes `--smoke-settings-layout` on its own; nothing is
            // injected into the environment any more.
            let nsapp: *mut AnyObject = msg_send![class!(NSApplication), sharedApplication];
            let _: () = msg_send![nsapp, finishLaunching];
            let original_locale = CONFIG.read().unwrap().i18n.locale.clone();
            for locale in ["en", "zh-Hans", "zh-Hant"] {
                i18n::apply_config_locale(locale);
                settings::invalidate_settings_window();
                log_info!("[smoke-settings-layout] validating locale={locale}");
                if !settings::settings_layout_smoke_runner() {
                    i18n::apply_config_locale(&original_locale);
                    settings::invalidate_settings_window();
                    eprintln!("[smoke-settings-layout] layout failed for locale={locale}");
                    std::process::exit(1);
                }
            }
            i18n::apply_config_locale(&original_locale);
            settings::invalidate_settings_window();
            log_info!("[smoke-settings-layout] all pages survived in all locales");
            std::process::exit(0);
        }
    }

    if std::env::args().any(|a| a == "--smoke-smooth-scroll-event") {
        unsafe {
            if !mouse::smooth::transformer::smoke_event_fields() {
                eprintln!("[smoke-smooth-scroll-event] synthetic event fields did not round-trip");
                std::process::exit(1);
            }
            log_info!("[smoke-smooth-scroll-event] synthetic event fields passed");
            std::process::exit(0);
        }
    }

    // Invoke the registered application delegate selector on AppKit's main thread and assert
    // that custom handling returns NO after presenting the Settings window.
    if std::env::args().any(|a| a == "--smoke-app-reopen") {
        unsafe {
            let nsapp: *mut AnyObject = msg_send![class!(NSApplication), sharedApplication];
            let _: () = msg_send![nsapp, finishLaunching];
            if !application_reopen_smoke() {
                eprintln!("[smoke-app-reopen] delegate handling or Settings visibility failed");
                std::process::exit(1);
            }
            log_info!("[smoke-app-reopen] delegate returned NO and Settings is visible");
            std::process::exit(0);
        }
    }

    if std::env::args().any(|a| a == "--smoke-keystroke-display-panel") {
        unsafe {
            let nsapp: *mut AnyObject = msg_send![class!(NSApplication), sharedApplication];
            let _: () = msg_send![nsapp, finishLaunching];
            let ok = keystroke_display::smoke_panel_runner();
            if !ok {
                eprintln!(
                    "[smoke-keystroke-display-panel] panel glass, sizing, or grip behavior failed"
                );
                std::process::exit(1);
            }
            log_info!("[smoke-keystroke-display-panel] glass, sizing, and grip behavior passed");
            std::process::exit(0);
        }
    }

    // Settings state-sync smoke entry: rebuild settings content once on the real AppKit main
    // thread and verify that the configured values survive the rebuild.
    if std::env::args().any(|a| a == "--smoke-settings-state-sync") {
        unsafe {
            let nsapp: *mut AnyObject = msg_send![class!(NSApplication), sharedApplication];
            let _: () = msg_send![nsapp, finishLaunching];
            let ok = settings::settings_state_sync_smoke_runner();
            if !ok {
                eprintln!("[smoke-settings-state-sync] settings state was not preserved");
                std::process::exit(1);
            }
            log_info!("[smoke-settings-state-sync] settings state survived rebuild");
            std::process::exit(0);
        }
    }

    // Conditional-row smoke entry: verify adjacent layout after collapsing a clipboard row in
    // the real AppKit settings page.
    if std::env::args().any(|a| a == "--smoke-settings-collapsible-row") {
        unsafe {
            let nsapp: *mut AnyObject = msg_send![class!(NSApplication), sharedApplication];
            let _: () = msg_send![nsapp, finishLaunching];
            let ok = settings::settings_collapsible_row_smoke_runner();
            if !ok {
                eprintln!("[smoke-settings-collapsible-row] conditional row geometry failed");
                std::process::exit(1);
            }
            log_info!("[smoke-settings-collapsible-row] conditional row geometry is stable");
            std::process::exit(0);
        }
    }

    // Mouse-profile callback smoke: drive real AppKit target/action controls through the
    // extern callback and verify config plus dependent-control refresh without persisting.
    if std::env::args().any(|a| a == "--smoke-settings-mouse-profile-callback") {
        unsafe {
            let nsapp: *mut AnyObject = msg_send![class!(NSApplication), sharedApplication];
            let _: () = msg_send![nsapp, finishLaunching];
            let ok = settings::settings_mouse_profile_callback_smoke_runner();
            if !ok {
                eprintln!("[smoke-settings-mouse-profile-callback] callback state did not apply");
                std::process::exit(1);
            }
            log_info!("[smoke-settings-mouse-profile-callback] mouse controls applied safely");
            std::process::exit(0);
        }
    }

    // Onboarding live-apply smoke entry: drive the guide's real display-mode segment and
    // clipboard switch, and verify both choices reach the live config without pressing Next.
    if std::env::args().any(|a| a == "--smoke-onboarding-live-apply") {
        unsafe {
            let nsapp: *mut AnyObject = msg_send![class!(NSApplication), sharedApplication];
            let _: () = msg_send![nsapp, finishLaunching];
            let ok = onboarding::live_apply_smoke_runner();
            if !ok {
                eprintln!(
                    "[smoke-onboarding-live-apply] a guide choice did not apply on selection"
                );
                std::process::exit(1);
            }
            log_info!("[smoke-onboarding-live-apply] guide choices applied without Next");
            std::process::exit(0);
        }
    }

    // 8. Run the main event loop (blocks until [NSApp terminate:])
    unsafe {
        let nsapp: *mut AnyObject = msg_send![class!(NSApplication), sharedApplication];
        let _: () = msg_send![nsapp, finishLaunching];
        // The first-run guide (auto-shown once): it covers Accessibility itself, so the alert is
        // skipped while it is on screen -- two dialogs at once help nobody. The alert still covers
        // a missing grant once the guide has already been seen.
        let onboarding_shown = onboarding::maybe_show_on_launch();
        // Prompt for Accessibility if missing (the event-monitor thread is already retrying in the background).
        if !onboarding_shown {
            prompt_accessibility_if_needed();
        }
        // Development switch: park the settings window on a page at launch so the UI can be
        // verified from a script / cua.
        if let Some(page) = open_settings_page_request() {
            settings::show_settings_page(page);
        }
        // Development switch: paint a controlled backdrop behind the panels, so a translucent material's
        // contrast can be measured against a known surface instead of whatever the desktop happens to show
        // (the A2 scenario `scripts/e2e/panel-contrast.sh` needs that to be repeatable). `texture` is the
        // patterned form: a flat tone cannot tell a live blur from an opaque fill that covers the texture.
        if let Some(shade) = crate::dev_flags::value("panel-backdrop") {
            if shade.trim().eq_ignore_ascii_case("texture") {
                dev_backdrop::show_texture();
            } else {
                show_dev_backdrop(&shade);
            }
        }
        // Development switch: open the clipboard picker so its laid-out UI is reachable without
        // driving the global hotkey (which would steal focus).
        if show_clipboard_request() {
            match show_clipboard_delay() {
                Some(seconds) if seconds > 0.0 => {
                    schedule_dev_show_clipboard(std::time::Duration::from_secs_f64(seconds));
                }
                _ => clipboard::show_picker_for_development(),
            }
            // Development only: `--clipboard-blank-text=after:N` hides the picker's text N seconds from now, so
            // the A2 contrast scenario captures the same panel in the same launch with and without its text.
            // Two launches would not do: a translucent material does not re-render identically across them.
            if let Some(seconds) =
                crate::dev_flags::value("clipboard-blank-text").and_then(|value| {
                    value
                        .strip_prefix("after:")
                        .and_then(|rest| rest.parse::<f64>().ok())
                })
            {
                schedule_dev_blank_text(std::time::Duration::from_secs_f64(seconds));
            }
        }
        // Development switch: summon the switcher and leave it standing, so an A2 scenario can
        // deliver a real pointer gesture to the footer (which the global hotkey path cannot reach
        // unattended). The panel is nonactivating, so this does not steal focus.
        if show_overlay_request() {
            crate::overlay::show_overlay_for_development();
        }
        // Development switch: seed a visible key panel so an A2 scenario can click its trailing
        // settings gear (the panel otherwise only appears from a real keystroke).
        if crate::dev_flags::present("show-keystroke-display") {
            crate::keystroke_display::show_for_development();
        }
        // Warm the settings window before the run loop takes over (see `schedule_settings_prewarm`).
        // Smoke processes assert launch-time state and exit through their runner, so they skip it.
        if !is_smoke_process {
            schedule_settings_prewarm();
        }
        e2e_state::record("launch");
        let _: () = msg_send![nsapp, run];
    }
}

/// Development helper (`--panel-backdrop=black|white|gray|#RRGGBB`): a plain window at the normal level,
/// which is *below* every panel (they sit at `normal + 3`), so the material samples it instead of the
/// desktop. Its content view is a solid layer fill, so the backdrop is exactly the requested colour.
unsafe fn show_dev_backdrop(shade: &str) {
    let value: f64 = match shade.trim().to_ascii_lowercase().as_str() {
        "black" => 0.0,
        "white" => 1.0,
        "gray" | "grey" => 0.5,
        other => {
            let hex = other.trim_start_matches('#');
            if hex.len() != 6 {
                log_info!("[dev-backdrop] ignored: {shade} is not black/white/gray/#RRGGBB");
                return;
            }
            let channel = u32::from_str_radix(&hex[0..2], 16).unwrap_or(0);
            channel as f64 / 255.0
        }
    };
    let screen: *mut AnyObject = msg_send![class!(NSScreen), mainScreen];
    let frame: NSRect = msg_send![screen, frame];
    let window: *mut AnyObject = msg_send![class!(NSWindow), alloc];
    let window: *mut AnyObject =
        msg_send![window, initWithContentRect: frame, styleMask: 0u64, backing: 2u64, defer: false];
    let _: () = msg_send![window, setOpaque: true];
    let _: () = msg_send![window, setReleasedWhenClosed: false];
    let content: *mut AnyObject = msg_send![window, contentView];
    let _: () = msg_send![content, setWantsLayer: true];
    let layer: *mut AnyObject = msg_send![content, layer];
    if !layer.is_null() {
        let colour: *mut AnyObject =
            msg_send![class!(NSColor), colorWithCalibratedWhite: value, alpha: 1.0f64];
        crate::ffi::layer_set_background(layer, crate::ffi::ns_color_to_cg(colour));
    }
    let _: () = msg_send![window, orderFrontRegardless];
    log_info!("[dev-backdrop] controlled backdrop up (shade={shade})");
}

pub fn run_relaunch_helper_if_requested(args: &[String]) -> bool {
    restart::run_relaunch_helper_if_requested(args)
}

#[cfg(test)]
mod tests {
    use super::{settings_prewarm_step, SettingsPrewarmStep, SETTINGS_PREWARM_MAX_ATTEMPTS};

    #[test]
    fn the_settings_warm_up_builds_only_at_a_quiet_moment_and_gives_up_after_the_cap() {
        assert_eq!(
            settings_prewarm_step(false, false, 0),
            SettingsPrewarmStep::Build,
            "an idle launch must warm the settings window"
        );
        assert_eq!(
            settings_prewarm_step(false, false, SETTINGS_PREWARM_MAX_ATTEMPTS),
            SettingsPrewarmStep::Build,
            "the retry cap must not stop a warm-up nothing blocks any more"
        );
        for retries in 0..SETTINGS_PREWARM_MAX_ATTEMPTS - 1 {
            for (guide, motion) in [(true, false), (false, true), (true, true)] {
                assert_eq!(
                    settings_prewarm_step(guide, motion, retries),
                    SettingsPrewarmStep::Retry,
                    "retry {retries} must reschedule (guide={guide} motion={motion})"
                );
            }
        }
        for (guide, motion) in [(true, false), (false, true), (true, true)] {
            assert_eq!(
                settings_prewarm_step(guide, motion, SETTINGS_PREWARM_MAX_ATTEMPTS - 1),
                SettingsPrewarmStep::GiveUp,
                "the last retry must give up instead of keeping a timer alive (guide={guide} motion={motion})"
            );
        }
        assert_eq!(
            settings_prewarm_step(false, true, SETTINGS_PREWARM_MAX_ATTEMPTS + 5),
            SettingsPrewarmStep::GiveUp,
            "a retry count past the cap must still give up"
        );
    }
}
