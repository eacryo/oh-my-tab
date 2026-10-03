//! Window-switcher-specific event monitoring: CGEventTap intercepts the Cmd/Opt+Tab global shortcut.
//! Common CGEventTap infrastructure (types/FFI/start helper) has been extracted to `event_tap.rs`;
//! this module keeps only the keyboard-shortcut matching logic and the GlobalEvent enum.

use crate::event_tap::{self, tap_location, tap_options, tap_placement};
use crate::{log_debug, log_info};
use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GlobalEvent {
    CmdTabPressed,
    CmdShiftTabPressed,
    CmdReleased,
    ClipboardToggled,
    // Window control: Option+arrow (the direction crosses to the main thread via the bounded
    // input aggregator).
    WindowControl(crate::window_management::Direction),
    // Display move: Option+Shift+arrow keys (the direction crosses to the main thread via the
    // bounded input aggregator).
    WindowDisplayMove(crate::window_management::Direction),
    // Quick actions: Option+I/E/D/L (the action id crosses to the main thread via the bounded
    // input aggregator).
    QuickAction(u8),
    KeystrokeDisplayWake,
}

// keyboard constants used by the window switcher
use crate::event_tap::keyboard::{
    EVENT_FLAGS_CHANGED as K_CG_EVENT_FLAGS_CHANGED, EVENT_KEY_DOWN as K_CG_EVENT_KEY_DOWN,
    FIELD_AUTOREPEAT as K_CG_KEYBOARD_EVENT_AUTOREPEAT,
    FIELD_KEYCODE as K_CG_KEYBOARD_EVENT_KEYCODE, FLAG_COMMAND as K_CG_EVENT_FLAG_MASK_COMMAND,
    FLAG_OPTION as K_CG_EVENT_FLAG_MASK_ALTERNATE, FLAG_SHIFT as K_CG_EVENT_FLAG_MASK_SHIFT,
    VK_TAB as K_VK_TAB, VK_V as K_VK_V,
};

const DEFAULT_CLIPBOARD_SHORTCUT: crate::mouse::shortcut::Shortcut =
    crate::mouse::shortcut::Shortcut {
        keycode: K_VK_V,
        flags: crate::mouse::shortcut::FLAG_ALT,
    };
const DEFAULT_CLIPBOARD_SHORTCUT_PACKED: u64 =
    ((DEFAULT_CLIPBOARD_SHORTCUT.flags as u64) << 16) | DEFAULT_CLIPBOARD_SHORTCUT.keycode as u64;
static CLIPBOARD_SHORTCUT_PACKED: AtomicU64 = AtomicU64::new(DEFAULT_CLIPBOARD_SHORTCUT_PACKED);

/// Replace the event-tap's cached binding atomically so a key event never observes half an update.
pub(crate) fn set_clipboard_shortcut(desc: &str) {
    let shortcut = crate::mouse::shortcut::validate_clipboard_shortcut(desc)
        .unwrap_or(DEFAULT_CLIPBOARD_SHORTCUT);
    let packed = ((shortcut.flags as u64) << 16) | shortcut.keycode as u64;
    CLIPBOARD_SHORTCUT_PACKED.store(packed, Ordering::Release);
}

fn clipboard_shortcut_matches(keycode: u16, flags: u64) -> bool {
    let packed = CLIPBOARD_SHORTCUT_PACKED.load(Ordering::Acquire);
    let shortcut = crate::mouse::shortcut::Shortcut {
        keycode: (packed & 0xffff) as u16,
        flags: (packed >> 16) as u32,
    };
    crate::mouse::shortcut::matches_shortcut(shortcut, keycode, flags)
}

fn switcher_tab_event(flags: crate::event_tap::CGEventFlags) -> GlobalEvent {
    if flags & K_CG_EVENT_FLAG_MASK_SHIFT != 0 {
        GlobalEvent::CmdShiftTabPressed
    } else {
        GlobalEvent::CmdTabPressed
    }
}

/// Held-Tab step cadence. macOS delivers its own autorepeat stream for a held Tab: its initial
/// delay, then the keyboard-repeat rate configured in System Settings (measured here at 83ms,
/// ~12/s). Repeating at that rate is faster than the card list, its thumbnail prefetch and its
/// scroll can follow, so accepted steps are spaced by at least this much. A slower configured
/// repeat rate is followed as-is rather than overridden.
const TAB_REPEAT_STEP_INTERVAL: Duration = Duration::from_millis(140);

/// `TAB_REPEAT_LAST_STEP_MS` value meaning "no accepted held-Tab step yet in this hold".
const TAB_REPEAT_DISARMED: u64 = u64::MAX;

/// Millis since `TAB_REPEAT_EPOCH` of the last accepted held-Tab step. A monotonic clock, not wall
/// time: it measures an interval. Read and written only from the tap callback thread, and by tests.
static TAB_REPEAT_LAST_STEP_MS: AtomicU64 = AtomicU64::new(TAB_REPEAT_DISARMED);
static TAB_REPEAT_EPOCH: LazyLock<Instant> = LazyLock::new(Instant::now);

/// What the tap must do with a Tab keyDown.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TabKeyAction {
    /// Not our combo (the modifier is not held): the system gets the event untouched.
    PassThrough,
    /// Our combo, but a held-Tab repeat inside the cadence: swallow it without stepping.
    Swallow,
    /// Our combo and due: step the selection (and still swallow).
    Step,
}

/// Whether a held-Tab repeat is due for a step, given the time since the last accepted step in
/// this hold (`None` = none yet). The first repeat after macOS's own initial delay steps
/// immediately: that delay is already the pause the user sees.
fn repeat_step_due(since_last_step: Option<Duration>) -> bool {
    match since_last_step {
        None => true,
        Some(since) => since >= TAB_REPEAT_STEP_INTERVAL,
    }
}

/// Decide how one Tab keyDown is handled. Throttled repeats must be swallowed rather than passed
/// through, or the system's native Cmd+Tab responds alongside us.
fn classify_tab_keydown(
    modifier_held: bool,
    autorepeat: bool,
    since_last_step: Option<Duration>,
) -> TabKeyAction {
    if !modifier_held {
        return TabKeyAction::PassThrough;
    }
    if autorepeat && !repeat_step_due(since_last_step) {
        return TabKeyAction::Swallow;
    }
    TabKeyAction::Step
}

/// Time since the last accepted held-Tab step, or `None` when this hold has not stepped yet.
fn tab_repeat_elapsed(previous_ms: u64, now_ms: u64) -> Option<Duration> {
    (previous_ms != TAB_REPEAT_DISARMED)
        .then(|| Duration::from_millis(now_ms.saturating_sub(previous_ms)))
}

/// A fresh physical Tab press arms the cadence, so the next repeat steps and later repeats are
/// spaced from it.
fn arm_tab_repeat_cadence() {
    TAB_REPEAT_LAST_STEP_MS.store(TAB_REPEAT_DISARMED, Ordering::Relaxed);
}

/// Record an accepted held-Tab step. Returns whether it was the first step of this hold, which is
/// the only one worth logging: a line per accepted step would flood at the repeat rate.
fn take_tab_repeat_step(now_ms: u64) -> bool {
    let previous_ms = TAB_REPEAT_LAST_STEP_MS.load(Ordering::Relaxed);
    TAB_REPEAT_LAST_STEP_MS.store(now_ms, Ordering::Relaxed);
    previous_ms == TAB_REPEAT_DISARMED
}

// Tracks whether CmdTabPressed was sent, to avoid spurious CmdReleased
static TAB_PRESSED: AtomicBool = AtomicBool::new(false);
static TAP_CONTROL: event_tap::TapThreadControl = event_tap::TapThreadControl::new();
static TAP_THREAD: Mutex<Option<std::thread::JoinHandle<()>>> = Mutex::new(None);
static TAP_ACTIVE: AtomicBool = AtomicBool::new(false);

// Shortcut mode: true = Command+Tab, false = Option+Tab
pub static SHORTCUT_IS_CMD: AtomicBool = AtomicBool::new(false);

unsafe extern "C" fn event_tap_callback(
    _proxy: crate::event_tap::CGEventTapProxy,
    event_type: crate::event_tap::CGEventType,
    event: crate::event_tap::CGEventRef,
    _user_info: *mut c_void,
) -> crate::event_tap::CGEventRef {
    if crate::input_monitor::handle_disabled_event(event_type, "kbd", TAP_CONTROL.is_stopping()) {
        return event;
    }
    if !crate::input_monitor::taps_allowed() {
        return event;
    }

    match event_type {
        K_CG_EVENT_KEY_DOWN => {
            let keycode =
                crate::event_tap::CGEventGetIntegerValueField(event, K_CG_KEYBOARD_EVENT_KEYCODE)
                    as u16;
            let flags = crate::event_tap::CGEventGetFlags(event);

            // Privacy: debug logs never record the user's keystrokes -- any key other than
            // Tab / Command / Option is logged as plain "Other" (no keycode, no flags), so
            // passwords and typed text never leak into the log. The remaining lines keep the
            // old diagnostic value: any keyDown line proves the tap is alive; a summon line
            // with no reaction means the issue is downstream (bridge/main thread).
            if keycode == K_VK_TAB {
                let is_cmd = SHORTCUT_IS_CMD.load(Ordering::SeqCst);
                let mod_mask = if is_cmd {
                    K_CG_EVENT_FLAG_MASK_COMMAND
                } else {
                    K_CG_EVENT_FLAG_MASK_ALTERNATE
                };
                // Master switch: when off, pass the event through (the native Cmd+Tab
                // takes over) -- no swallow, no event. Same philosophy as the clipboard
                // passthrough: a disabled feature returns the combo to the system.
                if (flags & mod_mask) != 0
                    && !crate::config::CONFIG
                        .read()
                        .map(|c| c.windows.enabled)
                        .unwrap_or(true)
                {
                    log_debug!("[kbd] Tab+Command passthrough (switcher disabled)");
                    return event;
                }
                let autorepeat = crate::event_tap::CGEventGetIntegerValueField(
                    event,
                    K_CG_KEYBOARD_EVENT_AUTOREPEAT,
                ) != 0;
                let now_ms = TAB_REPEAT_EPOCH.elapsed().as_millis() as u64;
                let since_last_step =
                    tab_repeat_elapsed(TAB_REPEAT_LAST_STEP_MS.load(Ordering::Relaxed), now_ms);
                match classify_tab_keydown((flags & mod_mask) != 0, autorepeat, since_last_step) {
                    // A held Tab inside the cadence: swallow it, but do not step. Returning the
                    // event would let the system's native Cmd+Tab react alongside us.
                    TabKeyAction::Swallow => {
                        crate::e2e_state::tab_repeat_throttled();
                        return std::ptr::null_mut();
                    }
                    // Tab without the modifier: not our combo any more, so it belongs to whatever
                    // app is frontmost (holding Tab after letting the modifier go must not keep
                    // switching).
                    TabKeyAction::PassThrough => return event,
                    TabKeyAction::Step => {}
                }
                if autorepeat {
                    crate::e2e_state::tab_repeat_step();
                    if take_tab_repeat_step(now_ms) {
                        log_debug!("[kbd] summon held-Tab repeat: switching continuously");
                    }
                } else {
                    // The summon combo: log only the combo name (not sensitive), never raw
                    // keycode/flags.
                    let combo = if is_cmd { "Tab+Command" } else { "Tab+Option" };
                    log_debug!("[kbd] summon keyDown {}", combo);
                    arm_tab_repeat_cadence();
                }
                TAB_PRESSED.store(true, Ordering::SeqCst);
                crate::enqueue_global_event(switcher_tab_event(flags));
                return std::ptr::null_mut();
            }
            if clipboard_shortcut_matches(keycode, flags) {
                // Side-button mappings that synthesize this chord intentionally loop back through
                // the session tap and can open clipboard history as well.
                if !crate::config::CONFIG.read().unwrap().clipboard.enabled {
                    // Preserve the configured chord for other apps while clipboard history is off.
                    log_debug!("[kbd] clipboard shortcut passthrough (clipboard disabled)");
                } else {
                    log_debug!("[kbd] clipboard shortcut pressed");
                    crate::enqueue_global_event(GlobalEvent::ClipboardToggled);
                    return std::ptr::null_mut();
                }
            }
        }
        K_CG_EVENT_FLAGS_CHANGED => {
            let flags = crate::event_tap::CGEventGetFlags(event);
            let is_cmd = SHORTCUT_IS_CMD.load(Ordering::SeqCst);
            let mod_mask = if is_cmd {
                K_CG_EVENT_FLAG_MASK_COMMAND
            } else {
                K_CG_EVENT_FLAG_MASK_ALTERNATE
            };
            if (flags & mod_mask) == 0 && TAB_PRESSED.swap(false, Ordering::SeqCst) {
                // Record only the modifier category, never ordinary keys. The stable state after
                // the event clears the session tap is sampled later by on_cmd_release_diagnostic;
                // reading system flags here would observe the pre-change value.
                let modifier_keycode = crate::event_tap::CGEventGetIntegerValueField(
                    event,
                    K_CG_KEYBOARD_EVENT_KEYCODE,
                ) as u16;
                let changed_key = modifier_key_name(modifier_keycode);
                log_debug!(
                    "[kbd] switcher modifier release detected: shortcut={} changed_key={}",
                    if is_cmd { "command" } else { "option" },
                    changed_key
                );
                crate::enqueue_global_event(GlobalEvent::CmdReleased);
            }
        }
        _ => {}
    }

    event
}

/// flagsChanged carries modifier keys only; category names make diagnostics useful without
/// widening keystroke logging to raw key codes.
fn modifier_key_name(keycode: u16) -> &'static str {
    match keycode {
        54 | 55 => "command",
        56 | 60 => "shift",
        57 => "caps_lock",
        58 | 61 => "option",
        59 | 62 => "control",
        63 => "function",
        _ => "other_modifier",
    }
}

pub fn start() {
    if !crate::input_monitor::taps_allowed() {
        return;
    }
    let mask: crate::event_tap::CGEventMask =
        (1u64 << K_CG_EVENT_KEY_DOWN) | (1u64 << K_CG_EVENT_FLAGS_CHANGED);

    let mut thread = TAP_THREAD.lock().unwrap();
    if thread.as_ref().is_some_and(|handle| !handle.is_finished()) {
        return;
    }
    if let Some(finished) = thread.take() {
        let _ = finished.join();
    }

    // The switcher tap sits at the session level: sees real hardware events AND session-synthesized
    // Cmd+Tab from mouse-remapper software (a HID-level tap can't see session-posted synthetic events,
    // so a side-button-mapped Cmd+Tab would slip past). options = DEFAULT_TAP: must be able to swallow
    // the Cmd+Tab event (return null), so a mutable tap is required.
    *thread = Some(event_tap::start_event_tap_thread(
        tap_location::SESSION_EVENT_TAP,
        tap_placement::HEAD_INSERT,
        tap_options::DEFAULT_TAP,
        mask,
        Some(event_tap_callback),
        0,
        "kbd",
        &TAP_CONTROL,
        || {
            TAP_ACTIVE.store(true, Ordering::SeqCst);
            // The shortcut can be toggled via menu/settings; print the actual combo from SHORTCUT_IS_CMD.
            let shortcut = if SHORTCUT_IS_CMD.load(Ordering::SeqCst) {
                "Command+Tab"
            } else {
                "Option+Tab"
            };
            log_info!(
                "Event monitor started. Listening for {} globally.",
                shortcut
            );
            // Starting after this session tap and using HEAD_INSERT keeps the display tap first
            // in the same-level chain, before the switcher can swallow Cmd+Tab.
            crate::keystroke_display::switcher_tap_started();
        },
    ));
}

pub(crate) fn tap_is_active() -> bool {
    TAP_ACTIVE.load(Ordering::SeqCst)
}

pub(crate) fn stop() {
    TAP_ACTIVE.store(false, Ordering::SeqCst);
    TAP_CONTROL.stop();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shift_tab_emits_the_backward_switcher_event() {
        assert_eq!(switcher_tab_event(0), GlobalEvent::CmdTabPressed);
        assert_eq!(
            switcher_tab_event(K_CG_EVENT_FLAG_MASK_SHIFT),
            GlobalEvent::CmdShiftTabPressed
        );
        assert_eq!(
            switcher_tab_event(K_CG_EVENT_FLAG_MASK_SHIFT | K_CG_EVENT_FLAG_MASK_COMMAND),
            GlobalEvent::CmdShiftTabPressed
        );
    }

    #[test]
    fn a_fresh_tab_press_with_the_modifier_steps() {
        assert_eq!(classify_tab_keydown(true, false, None), TabKeyAction::Step);
    }

    #[test]
    fn tab_without_the_modifier_passes_through_to_the_system() {
        // Holding Tab after letting the modifier go must not keep switching, and the event is not
        // ours any more, so it belongs to whatever app is frontmost.
        assert_eq!(
            classify_tab_keydown(false, false, None),
            TabKeyAction::PassThrough
        );
        assert_eq!(
            classify_tab_keydown(false, true, Some(TAB_REPEAT_STEP_INTERVAL)),
            TabKeyAction::PassThrough
        );
    }

    #[test]
    fn the_first_repeat_of_a_hold_steps_immediately() {
        // macOS already applied its own initial repeat delay, so that pause is the one the user
        // sees; waiting again here would add a second one.
        assert!(repeat_step_due(None));
        assert_eq!(classify_tab_keydown(true, true, None), TabKeyAction::Step);
    }

    #[test]
    fn repeats_inside_the_cadence_are_swallowed_without_stepping() {
        let just_inside = TAB_REPEAT_STEP_INTERVAL - Duration::from_millis(1);
        assert!(!repeat_step_due(Some(just_inside)));
        assert_eq!(
            classify_tab_keydown(true, true, Some(just_inside)),
            TabKeyAction::Swallow
        );
        assert!(repeat_step_due(Some(TAB_REPEAT_STEP_INTERVAL)));
        assert_eq!(
            classify_tab_keydown(true, true, Some(TAB_REPEAT_STEP_INTERVAL)),
            TabKeyAction::Step
        );
    }

    #[test]
    fn a_disarmed_cadence_reports_no_elapsed_step() {
        assert_eq!(tab_repeat_elapsed(TAB_REPEAT_DISARMED, 10_000), None);
        assert_eq!(
            tab_repeat_elapsed(1_000, 1_500),
            Some(Duration::from_millis(500))
        );
        // A clock that appears to go backwards must not panic or report a huge interval.
        assert_eq!(tab_repeat_elapsed(2_000, 1_000), Some(Duration::ZERO));
    }

    #[test]
    fn modifier_key_names_do_not_expose_raw_keycodes() {
        assert_eq!(modifier_key_name(54), "command");
        assert_eq!(modifier_key_name(61), "option");
        assert_eq!(modifier_key_name(999), "other_modifier");
    }
}
