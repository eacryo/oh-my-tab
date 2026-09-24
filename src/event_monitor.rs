//! Window-switcher-specific event monitoring: CGEventTap intercepts the Cmd/Opt+Tab global shortcut.
//! Common CGEventTap infrastructure (types/FFI/start helper) has been extracted to `event_tap.rs`;
//! this module keeps only the keyboard-shortcut matching logic and the GlobalEvent enum.

use crate::event_tap::{self, tap_location, tap_options, tap_placement};
use crate::{log_debug, log_info};
use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

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
}

// keyboard constants used by the window switcher
use crate::event_tap::keyboard::{
    EVENT_FLAGS_CHANGED as K_CG_EVENT_FLAGS_CHANGED, EVENT_KEY_DOWN as K_CG_EVENT_KEY_DOWN,
    FIELD_AUTOREPEAT as K_CG_KEYBOARD_EVENT_AUTOREPEAT,
    FIELD_KEYCODE as K_CG_KEYBOARD_EVENT_KEYCODE, FLAG_COMMAND as K_CG_EVENT_FLAG_MASK_COMMAND,
    FLAG_CONTROL as K_CG_EVENT_FLAG_MASK_CONTROL, FLAG_OPTION as K_CG_EVENT_FLAG_MASK_ALTERNATE,
    FLAG_SHIFT as K_CG_EVENT_FLAG_MASK_SHIFT, VK_TAB as K_VK_TAB, VK_V as K_VK_V,
};

fn switcher_tab_event(flags: crate::event_tap::CGEventFlags) -> GlobalEvent {
    if flags & K_CG_EVENT_FLAG_MASK_SHIFT != 0 {
        GlobalEvent::CmdShiftTabPressed
    } else {
        GlobalEvent::CmdTabPressed
    }
}

/// Ignore only the system-generated repeat for one held Tab; separate physical presses still navigate continuously.
fn should_ignore_tab_autorepeat(autorepeat: i64) -> bool {
    autorepeat != 0
}

// Tracks whether CmdTabPressed was sent, to avoid spurious CmdReleased
static TAB_PRESSED: AtomicBool = AtomicBool::new(false);
static TAP_CONTROL: event_tap::TapThreadControl = event_tap::TapThreadControl::new();
static TAP_THREAD: Mutex<Option<std::thread::JoinHandle<()>>> = Mutex::new(None);

// Shortcut mode: true = Command+Tab, false = Option+Tab
pub static SHORTCUT_IS_CMD: AtomicBool = AtomicBool::new(false);

unsafe extern "C" fn event_tap_callback(
    _proxy: crate::event_tap::CGEventTapProxy,
    event_type: crate::event_tap::CGEventType,
    event: crate::event_tap::CGEventRef,
    _user_info: *mut c_void,
) -> crate::event_tap::CGEventRef {
    if crate::input_monitor::handle_disabled_event(event_type, "kbd") {
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
                if (flags & mod_mask) != 0 {
                    // Master switch: when off, pass the event through (the native Cmd+Tab
                    // takes over) -- no swallow, no event. Same philosophy as the Option+V
                    // passthrough: a disabled feature returns the combo to the system.
                    if !crate::config::CONFIG
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
                    );
                    if should_ignore_tab_autorepeat(autorepeat) {
                        // Repeat events must be swallowed rather than passed through, or the system's native Cmd+Tab responds alongside us.
                        log_debug!("[kbd] summon autorepeat ignored");
                        return std::ptr::null_mut();
                    }
                    // The summon combo: log only the combo name (not sensitive), never raw keycode/flags.
                    let combo = if is_cmd { "Tab+Command" } else { "Tab+Option" };
                    log_debug!("[kbd] summon keyDown {}", combo);
                    TAB_PRESSED.store(true, Ordering::SeqCst);
                    crate::enqueue_global_event(switcher_tab_event(flags));
                    return std::ptr::null_mut();
                }
            } else if keycode == K_VK_V && (flags & K_CG_EVENT_FLAG_MASK_ALTERNATE) != 0 {
                // History-clipboard summon: Option+V (always Option, independent of the
                // shortcut mode). The event is swallowed, mirroring Win+V. Only the combo
                // name is logged (privacy convention).
                //
                // Precise match is required: flags is the bitmask of ALL currently held
                // modifiers, so a bare "contains Option" check would swallow combos like
                // Cmd+Option+V (paste-and-match-style) and break the system shortcut.
                // Combos carrying any other modifier pass through untouched.
                let other_mods = flags
                    & (K_CG_EVENT_FLAG_MASK_COMMAND
                        | K_CG_EVENT_FLAG_MASK_SHIFT
                        | K_CG_EVENT_FLAG_MASK_CONTROL);
                if other_mods != 0 {
                    // Combos with extra modifiers (e.g. Cmd+Option+V) pass through.
                    log_debug!("[kbd] Option+V passthrough (extra modifiers)");
                } else if !crate::config::CONFIG.read().unwrap().clipboard.enabled {
                    // When the feature is disabled, do NOT swallow Option+V -- other apps may
                    // need the combo, so it passes through untouched.
                    log_debug!("[kbd] Option+V passthrough (clipboard disabled)");
                } else {
                    log_debug!("[kbd] summon keyDown V+Option (clipboard)");
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
        },
    ));
}

pub(crate) fn stop() {
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
    fn only_autorepeat_tab_events_are_ignored() {
        assert!(!should_ignore_tab_autorepeat(0));
        assert!(should_ignore_tab_autorepeat(1));
    }

    #[test]
    fn modifier_key_names_do_not_expose_raw_keycodes() {
        assert_eq!(modifier_key_name(54), "command");
        assert_eq!(modifier_key_name(61), "option");
        assert_eq!(modifier_key_name(999), "other_modifier");
    }
}
