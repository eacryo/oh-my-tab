//! Key simulator: synthesizes the mapped shortcut as keyboard events posted at the session level.
//! The frontmost app receives ordinary shortcuts; global shortcuts owned by this app dispatch
//! internally, while session taps can observe events posted by other remappers.
//!
//! Modeled on LinearMouse's KeySimulator: modifiers ride on each event as flags; synthetic
//! events carry a userData marker so the tap can recognize its own output.
//!
//! Two-phase down/up semantics are required: the switcher commits on modifier RELEASE -- while
//! the side button is held, a keyDown (with modifiers) is posted and the overlay opens; on
//! release, a keyUp + modifier flagsChanged is posted so our tap sees the modifier go up and
//! commits the switch.

use crate::event_monitor::GlobalEvent;
use crate::mouse::gesture::modifiers::HolderId;

use crate::event_tap::{
    tap_location, CGEventCreateKeyboardEvent, CGEventFlags, CGEventPost, CGEventSetFlags,
    CGEventSetIntegerValueField, CGEventSetType, K_CG_EVENT_SOURCE_USER_DATA, SYNTHETIC_MARKER,
};
use crate::mouse::shortcut::{FLAG_ALT, FLAG_CMD, FLAG_CTRL, FLAG_SHIFT};
use crate::{enqueue_global_event, log_debug};
use std::collections::{HashMap, HashSet};
use std::ffi::c_void;
use std::sync::mpsc::{self, SyncSender, TrySendError};
use std::sync::{Mutex, OnceLock};

/// Side button pressed: post the mapped keyDown (modifiers attached) to the session level.
pub(crate) fn press_down(keycode: u16, flags: u32, desc: &str) {
    // A direct caller (a one-off press, not a button mapping) owns its bits under the gesture slot
    // so it can never collide with a mapped button's identity.
    press_down_for(GESTURE_KEY_SLOT, keycode, flags, desc);
}

/// Press on behalf of \\`holder\\` (a button number, or the gesture slot).
fn press_down_for(holder: u32, keycode: u16, flags: u32, desc: &str) {
    hold_managed_for(HolderId::Button(holder), flags);
    // Binding our own global shortcuts (switcher/clipboard) dispatches internally instead
    // of synthesizing: session-level posts can't loop back to our tap, and HID-level
    // keyboard posts are dropped by the system (no IOHIDEvent, verified).
    if internal_dispatch(keycode, flags, true) {
        return;
    }
    // Full key sequence (same as LinearMouse's postKeyLocked): first the modifiers' real
    // flagsChanged presses (flags accumulating step by step), then the main keyDown.
    // System-level shortcuts (e.g. Ctrl+Up = Mission Control) require the real modifier
    // state transitions; flags attached to a bare keyDown are not recognized.
    let mut acc: u32 = 0;
    for (bit, vk) in MOD_KEYS {
        if flags & bit != 0 {
            acc |= bit;
            unsafe {
                post_modifier_change(vk, acc);
            }
        }
    }
    unsafe {
        post_key(keycode, flags, true);
    }
    log_debug!(
        "[mouse] button -> key down '{}' (keycode={} flags=0x{:x}) via session tap",
        desc,
        keycode,
        flags
    );
}

/// Side button released: post keyUp + a modifier flagsChanged (all modifiers up) to the HID
/// level. It loops back to the switcher tap, whose flagsChanged handler sees the modifier go
/// up and fires CmdReleased to commit the switch.
pub(crate) fn release_up(keycode: u16, flags: u32, desc: &str) {
    release_up_for(GESTURE_KEY_SLOT, keycode, flags, desc);
}

/// Release on behalf of \\`holder\\`.
fn release_up_for(holder: u32, keycode: u16, flags: u32, desc: &str) {
    // Internally dispatched combos: the switcher commits on release, and nothing is synthesized --
    // but the modifiers this holder took at press time are still ours to give back. Returning here
    // used to leave them registered, which surfaced as a modifier stuck down for later input.
    let dispatched_internally = internal_dispatch(keycode, flags, false);
    release_managed_for(HolderId::Button(holder), flags);
    if dispatched_internally {
        return;
    }
    // Main keyUp, then the modifiers this holder no longer needs -- in reverse order, and only the
    // ones no other holder (another button mapping, a gesture, or the user's own fingers) still
    // needs. Walking the flags to zero is what used to lift a key someone else was holding.
    unsafe {
        post_key(keycode, flags, false);
    }
    log_debug!(
        "[mouse] button -> key up '{}' (keycode={} flags=0x{:x}) via session tap",
        desc,
        keycode,
        flags
    );
}

/// Detect whether the target combo is one of our own global shortcuts; if so, send the
/// GlobalEvent through the internal channel. Returns true when dispatched internally (the
/// caller must not synthesize).
fn internal_dispatch(keycode: u16, flags: u32, down: bool) -> bool {
    // The switcher's current combo: Tab + the active mode's modifier (a binding like
    // "cmd+tab" / "alt+tab", without extra modifiers).
    let sw_mod = match crate::event_monitor::switcher_modifier_name() {
        "control" => FLAG_CTRL,
        "command" => FLAG_CMD,
        _ => FLAG_ALT,
    };
    if keycode == 48 && flags == sw_mod {
        log_debug!(
            "[mouse] switcher key mapping internally dispatched: phase={}",
            if down { "down" } else { "up" }
        );
        if down {
            enqueue_global_event(GlobalEvent::CmdTabPressed);
        } else {
            // The switcher commits on release (same semantics as releasing the modifier).
            enqueue_global_event(GlobalEvent::CmdReleased);
        }
        return true;
    }
    // Option+V remains an internal fast path for the default binding. Other configured clipboard
    // chords are posted normally and the session-level event monitor catches the loop-back.
    if keycode == 9 && flags == FLAG_ALT && down {
        let enabled = crate::config::CONFIG
            .read()
            .map(|c| c.clipboard.enabled)
            .unwrap_or(false);
        if enabled {
            enqueue_global_event(GlobalEvent::ClipboardToggled);
            return true;
        }
    }
    false
}

/// Modifier (bit, keycode) pairs, fixed order: cmd → alt → ctrl → shift.
const MOD_KEYS: [(u32, u16); 4] = [
    (FLAG_CMD, 55),
    (FLAG_ALT, 58),
    (FLAG_CTRL, 59),
    (FLAG_SHIFT, 56),
];

enum KeyJob {
    Down {
        keycode: u16,
        flags: u32,
        desc: String,
        /// The button this press belongs to; its release gives back only this button's bits.
        holder: u32,
    },
    Up {
        keycode: u16,
        flags: u32,
        desc: String,
        holder: u32,
    },
    SystemAction(&'static str),
    /// A Dock-backed action asked for by a gesture; the worker re-checks the epoch first.
    GestureSystemAction {
        notification: &'static str,
        epoch: u64,
        generation: u64,
    },
    /// A complete gesture chord: pressed, released and its modifiers given back by the worker.
    GestureChord {
        keycode: u16,
        flags: u32,
        holder: u64,
        epoch: u64,
        generation: u64,
    },
}

static KEY_JOBS: OnceLock<Option<SyncSender<KeyJob>>> = OnceLock::new();
static ACTIVE_BUTTON_BINDINGS: OnceLock<Mutex<HashMap<u32, (u16, u32)>>> = OnceLock::new();
static ACTIVE_SYSTEM_BUTTONS: OnceLock<Mutex<HashSet<u32>>> = OnceLock::new();
static RELEASE_BACKLOG: OnceLock<Mutex<HashMap<u32, (u16, u32)>>> = OnceLock::new();

fn active_bindings() -> &'static Mutex<HashMap<u32, (u16, u32)>> {
    ACTIVE_BUTTON_BINDINGS.get_or_init(|| Mutex::new(HashMap::new()))
}

fn release_backlog() -> &'static Mutex<HashMap<u32, (u16, u32)>> {
    RELEASE_BACKLOG.get_or_init(|| Mutex::new(HashMap::new()))
}

fn active_system_buttons() -> &'static Mutex<HashSet<u32>> {
    ACTIVE_SYSTEM_BUTTONS.get_or_init(|| Mutex::new(HashSet::new()))
}

fn drain_release_backlog() {
    // The button identity travels with the release: giving the bits back under a different holder
    // would leave the real one registered forever once the queue had been full at release time.
    let releases: Vec<(u32, (u16, u32))> = release_backlog().lock().unwrap().drain().collect();
    for (button, (keycode, flags)) in releases {
        release_up_for(button, keycode, flags, "queued release");
    }
}

fn key_job_sender() -> Option<&'static SyncSender<KeyJob>> {
    KEY_JOBS
        .get_or_init(|| {
            let (sender, receiver) = mpsc::sync_channel::<KeyJob>(256);
            std::thread::Builder::new()
                .name("mouse-key-event-poster".into())
                .spawn(move || {
                    while let Ok(job) = receiver.recv() {
                        match job {
                            KeyJob::Down {
                                keycode,
                                flags,
                                desc,
                                holder,
                            } => {
                                if !crate::mouse::event_tap::mouse_tap_stopping()
                                    && crate::input_monitor::taps_allowed()
                                {
                                    press_down_for(holder, keycode, flags, &desc);
                                }
                            }
                            KeyJob::Up {
                                keycode,
                                flags,
                                desc,
                                holder,
                            } => {
                                release_up_for(holder, keycode, flags, &desc);
                            }
                            KeyJob::GestureChord {
                                keycode,
                                flags,
                                holder,
                                epoch,
                                generation,
                            } => run_gesture_chord(keycode, flags, holder, epoch, generation),
                            KeyJob::GestureSystemAction {
                                notification,
                                epoch,
                                generation,
                            } => {
                                if !crate::mouse::event_tap::mouse_tap_stopping()
                                    && crate::input_monitor::taps_allowed()
                                    && crate::mouse::gesture::runtime::still_authorized(
                                        epoch, generation,
                                    )
                                {
                                    crate::mouse::system_action::fire(notification);
                                }
                            }
                            KeyJob::SystemAction(notification) => {
                                if !crate::mouse::event_tap::mouse_tap_stopping()
                                    && crate::input_monitor::taps_allowed()
                                {
                                    crate::mouse::system_action::fire(notification);
                                }
                            }
                        }
                        drain_release_backlog();
                    }
                })
                .ok()
                .map(|_| sender)
        })
        .as_ref()
}

pub(crate) fn ensure_key_poster() -> bool {
    key_job_sender().is_some()
}

/// Run Dock-backed actions off the HID callback and swallow their matching button-up only when
/// the down action was accepted into the worker queue.
pub(crate) fn queue_system_action(button: u32, down: bool, notification: &'static str) -> bool {
    let Some(sender) = key_job_sender() else {
        return false;
    };
    let mut active = active_system_buttons().lock().unwrap();
    if down {
        if active.contains(&button) {
            return true;
        }
        match sender.try_send(KeyJob::SystemAction(notification)) {
            Ok(()) => {
                active.insert(button);
                true
            }
            Err(TrySendError::Full(_)) | Err(TrySendError::Disconnected(_)) => false,
        }
    } else {
        active.remove(&button)
    }
}

pub(crate) fn clear_system_button_states() {
    active_system_buttons().lock().unwrap().clear();
}

/// Queue a mapped key action away from the HID callback. The bounded queue preserves memory
/// limits; when a release cannot fit, a coalesced emergency release is drained by the worker.
pub(crate) fn queue_button_mapping(
    button: u32,
    down: bool,
    keycode: u16,
    flags: u32,
    desc: &str,
) -> bool {
    let Some(sender) = key_job_sender() else {
        return false;
    };
    let mut active = active_bindings().lock().unwrap();
    if down {
        if release_backlog().lock().unwrap().contains_key(&button) {
            return false;
        }
        let job = KeyJob::Down {
            keycode,
            flags,
            desc: desc.to_owned(),
            holder: button,
        };
        match sender.try_send(job) {
            Ok(()) => {
                active.insert(button, (keycode, flags));
                true
            }
            Err(TrySendError::Full(_)) | Err(TrySendError::Disconnected(_)) => false,
        }
    } else {
        let Some((active_keycode, active_flags)) = active.get(&button).copied() else {
            return false;
        };
        let job = KeyJob::Up {
            keycode: active_keycode,
            flags: active_flags,
            desc: desc.to_owned(),
            holder: button,
        };
        match sender.try_send(job) {
            Ok(()) => {
                active.remove(&button);
                true
            }
            Err(TrySendError::Full(_)) => {
                active.remove(&button);
                release_backlog()
                    .lock()
                    .unwrap()
                    .insert(button, (active_keycode, active_flags));
                true
            }
            Err(TrySendError::Disconnected(_)) => false,
        }
    }
}

/// Queue releases for every mapped shortcut still held when the mouse tap stops.
pub(crate) fn release_all_queued() {
    let mut active = active_bindings().lock().unwrap();
    if active.is_empty() {
        return;
    }
    let sender = key_job_sender();
    for (button, (keycode, flags)) in active.drain() {
        if let Some(sender) = sender {
            match sender.try_send(KeyJob::Up {
                keycode,
                flags,
                desc: "tap stopped".into(),
                holder: button,
            }) {
                Ok(()) => {}
                Err(TrySendError::Full(_)) => {
                    release_backlog()
                        .lock()
                        .unwrap()
                        .insert(button, (keycode, flags));
                }
                Err(TrySendError::Disconnected(_)) => {}
            }
        }
    }
}

/// The flags a synthetic event must carry: this binding's intent merged with every other holder's,
/// so pressing Option while another button holds Command does not show the application a state with
/// Command missing.
fn arbitrated(flags: u32) -> u32 {
    crate::mouse::gesture::runtime::forward_flags(flags)
}

/// Synthesize a modifier flagsChanged event (flags = the accumulated modifier state at that
/// moment), posted to the session level.
unsafe fn post_modifier_change(vk: u16, flags: u32) {
    let ev = CGEventCreateKeyboardEvent(std::ptr::null(), vk, false);
    if ev.is_null() {
        return;
    }
    CGEventSetType(ev, 12); // kCGEventFlagsChanged
                            // The arbitrated state, never this holder's view of it: a synthetic event that omitted another
                            // holder's modifier would tell the application the key went up.
    CGEventSetFlags(ev, arbitrated(flags) as CGEventFlags);
    CGEventSetIntegerValueField(ev, K_CG_EVENT_SOURCE_USER_DATA, SYNTHETIC_MARKER);
    CGEventPost(tap_location::SESSION_EVENT_TAP, ev);
    // CFRelease, not CGEventRelease: the latter is an inline wrapper in the macOS headers and is
    // not an exported symbol.
    crate::event_tap::CFRelease(ev as *const _);
}

/// Synthesize a single keyboard event (keyDown/true or keyUp/false), tagged with userData and
/// posted to the session level. HID-level keyboard posts are dropped by the system.
unsafe fn post_key(keycode: u16, flags: u32, key_down: bool) {
    let flags = arbitrated(flags);
    let ev = CGEventCreateKeyboardEvent(std::ptr::null(), keycode, key_down);
    if ev.is_null() {
        return;
    }
    CGEventSetFlags(ev, flags as CGEventFlags);
    CGEventSetIntegerValueField(ev, K_CG_EVENT_SOURCE_USER_DATA, SYNTHETIC_MARKER);
    CGEventPost(tap_location::SESSION_EVENT_TAP, ev);
    crate::ffi::CFRelease(ev as *const c_void);
}

/// Press the managed modifier bits `flags` names, owned by `holder`.
fn hold_managed_for(holder: HolderId, flags: u32) {
    if let Some(next) = crate::mouse::gesture::runtime::acquire_modifiers(holder, flags) {
        match next {
            0 => {}
            value => post_merged_modifier_change(value),
        }
    }
}

/// Release what `holder` held; a bit another holder still needs stays down.
fn release_managed_for(holder: HolderId, flags: u32) {
    if let Some(next) = crate::mouse::gesture::runtime::release_modifiers(holder, flags) {
        post_merged_modifier_change(next);
    }
}

/// Publish a merged modifier state as a flagsChanged, using the first modifier keycode as the carrier
/// (the application keys on the flags value, not on which side produced it).
fn post_merged_modifier_change(flags: u32) {
    let keycode = MOD_KEYS
        .iter()
        .find(|(bit, _)| flags & bit != 0)
        .map(|(_, vk)| *vk)
        .unwrap_or(55);
    unsafe {
        post_modifier_change(keycode, flags);
    }
}

/// A completed gesture's shortcut. One job carries the whole transaction: the worker takes the
/// modifier bits, presses, releases and gives the bits back. Taking them here would leave them owned
/// by a holder no release path knows about when the queue refuses the press.
pub(crate) fn queue_gesture_keys(keycode: u16, flags: u32, epoch: u64, generation: u64) {
    let Some(sender) = key_job_sender() else {
        return;
    };
    let holder = GESTURE_HOLDER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let _ = sender.try_send(KeyJob::GestureChord {
        keycode,
        flags,
        holder,
        epoch,
        generation,
    });
}

/// Run one complete gesture chord on the poster thread. Every exit gives the modifier bits back.
fn run_gesture_chord(keycode: u16, flags: u32, holder: u64, epoch: u64, generation: u64) {
    if crate::mouse::event_tap::mouse_tap_stopping() || !crate::input_monitor::taps_allowed() {
        return;
    }
    // Typing into a recorder, or a configuration that withdrew the gesture while this job waited:
    // the consumer re-checks rather than trusting the decision made at release.
    if !crate::mouse::gesture::runtime::still_authorized(epoch, generation) {
        return;
    }
    let id = HolderId::Gesture(holder);
    hold_managed_for(id, flags);
    press_down(keycode, flags, "gesture");
    release_up(keycode, flags, "gesture");
    if let Some(next) = crate::mouse::gesture::runtime::release_modifiers(id, flags) {
        post_merged_modifier_change(next);
    }
}

/// Slot number the release backlog uses for a gesture chord. Button numbers start at 2 in this
/// table, so 0 is free and cannot collide with a mapped button.
const GESTURE_KEY_SLOT: u32 = 0;

/// Distinguishes one gesture transaction from the next in the modifier arbiter.
static GESTURE_HOLDER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

/// Fire a Dock-backed system action on behalf of a gesture.
pub(crate) fn queue_system_action_for_gesture(
    notification: &'static str,
    epoch: u64,
    generation: u64,
) {
    let Some(sender) = key_job_sender() else {
        return;
    };
    // Routed through the same re-check shape as a chord: the worker refuses once the gesture that
    // asked for it has been withdrawn or a recorder took over.
    let _ = sender.try_send(KeyJob::GestureSystemAction {
        notification,
        epoch,
        generation,
    });
}
