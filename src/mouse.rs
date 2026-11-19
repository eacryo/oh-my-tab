//! Mouse enhancement module.
//! Provides two scroll modes: Default (passthrough + optional reverse) and Line (fixed line
//! count).

pub(crate) mod device;
pub(crate) mod event_tap;
pub(crate) mod ffi;
pub(crate) mod gesture;
pub(crate) mod keysim;
pub(crate) mod pointer;
pub(crate) mod resolve;
pub(crate) mod scrolling;
pub(crate) mod shortcut;
pub(crate) mod smooth;
pub(crate) mod system_action;

/// Whether the mouse module's OWN behaviour is on: scrolling, pointer acceleration and the
/// side-button mappings. Gestures are deliberately outside this: they are their own input path with
/// their own switch, so turning mouse control off must not take them away.
///
/// The development force-on flag starts the mouse tap so smooth scroll can be exercised without
/// changing the persisted mouse master switch.
pub(crate) fn module_active() -> bool {
    crate::config::CONFIG
        .read()
        .map(|config| module_active_for(&config.mouse))
        .unwrap_or(false)
        || crate::dev_flags::present("smooth-scroll-force-on")
}

/// Serializes the tap's own button handling against the module being switched off.
///
/// The tap thread can read `module_active()` and then be descheduled while the main thread turns the
/// module off and releases what a held button owned; a press registered after that release would
/// never get its Up, because the switch check now skips the mapping branch. The callback holds this
/// across "read the switch and act"; the switch-off path holds it across "release".
static MODULE_GATE: std::sync::Mutex<()> = std::sync::Mutex::new(());

pub(crate) fn module_gate() -> std::sync::MutexGuard<'static, ()> {
    MODULE_GATE
        .lock()
        .unwrap_or_else(|error| error.into_inner())
}

/// Whether the HID tap has to run at all. One tap carries both features, so it is installed while
/// EITHER of them is on; each branch inside the tap asks its own switch.
pub(crate) fn tap_wanted() -> bool {
    crate::config::CONFIG
        .read()
        .map(|config| tap_wanted_for(&config.mouse))
        .unwrap_or(false)
        || crate::dev_flags::present("smooth-scroll-force-on")
}

/// The pure halves, so the gating is testable without a config global or a running tap.
pub(crate) fn module_active_for(mouse: &crate::config::MouseSection) -> bool {
    mouse.enabled
}

pub(crate) fn gestures_on_for(mouse: &crate::config::MouseSection) -> bool {
    mouse.gestures_enabled
}

pub(crate) fn tap_wanted_for(mouse: &crate::config::MouseSection) -> bool {
    module_active_for(mouse) || gestures_on_for(mouse)
}

use crate::log_info;

/// Mouse event-thread lifecycle state. A new thread cannot race ahead while stopping, avoiding a
/// race on the shared cancellation flag.
struct MouseRuntime {
    thread: Option<std::thread::JoinHandle<()>>,
    stopping: bool,
}

static MOUSE_RUNTIME: std::sync::Mutex<MouseRuntime> = std::sync::Mutex::new(MouseRuntime {
    thread: None,
    stopping: false,
});

/// Enable mouse control at runtime (shared by the settings hot-switch and the startup path).
/// Idempotent: no-op when already running (the thread may have exited naturally, e.g. tap
/// creation failed).
pub(crate) fn start() {
    if !crate::input_monitor::taps_allowed() {
        return;
    }
    let finished = {
        let mut runtime = MOUSE_RUNTIME.lock().unwrap();
        // While stopping is being finalized in the background, the reaper decides whether to
        // restart based on the latest CONFIG value.
        if runtime.stopping {
            return;
        }
        // Already running and the thread is alive -> don't start again. Reap a finished handle
        // outside the lock so joining never blocks other lifecycle calls.
        let finished = runtime
            .thread
            .as_ref()
            .filter(|handle| handle.is_finished())
            .is_some();
        if !finished && runtime.thread.is_some() {
            return;
        }
        let finished = if finished {
            runtime.thread.take()
        } else {
            None
        };
        runtime.thread = Some(event_tap::start());
        finished
    };
    if let Some(handle) = finished {
        let _ = handle.join();
    }
    publish_gestures();
    log_info!("Mouse event tap started.");
}

/// Give back everything a pressed mouse button holds when the module's own behaviour is switched
/// off while the tap keeps running for gestures: a mapping's synthetic keys and modifiers, a system
/// action's button state, and the switcher session a mapping opened.
///
/// `stop()` does this for the whole tap; this is the way to do it when only one feature leaves.
pub(crate) fn release_holds() {
    // Same critical section as the callback's "act on a button": a press registered before this
    // runs is released here, and one that arrives after it re-reads the switch under the same lock.
    let _gate = module_gate();
    keysim::release_all_queued();
    keysim::clear_system_button_states();
    event_tap::release_holds();
}

/// Publish the gesture table and switch the gesture runtime on, from the current config.
///
/// Called whenever the mouse module starts and whenever the config is applied. The runtime reads the
/// table snapshot per event, so publishing here is what makes the feature live without restarting
/// the tap thread.
pub(crate) fn publish_gestures() {
    let config = crate::config::CONFIG.read();
    let Ok(config) = config else {
        return;
    };
    let before = gesture::table::current_epoch();
    let errors = gesture::table::publish(&config);
    for (index, error) in errors {
        log_info!("[gesture] row {index} ignored: {error:?}");
    }
    let active = gestures_on_for(&config.mouse);
    gesture::runtime::set_active(active);
    if !active {
        // A recording exists only for this feature. Switching it off must not leave the recorder
        // armed: it would still swallow the next press and then refuse the stroke at commit.
        gesture::recording::cancel();
        crate::settings::gesture_recorder::refresh_recording_row_on_main();
    }
    // Every reload that changes the gesture's own authorization is a revocation of what the old
    // configuration started: deleting a binding, retargeting one, or editing the exclusion list all
    // leave a switcher session the OLD configuration opened with no owner. A session handed to the
    // user is untouched (the claim is only revoked while it is still Active).
    //
    // The epoch is what tells "this reload changed the gesture" from "some other switch moved":
    // mouse control is not part of a gesture's authorization, so flipping it must not dismiss the
    // session a gesture opened.
    if !active || gesture::table::current_epoch() != before {
        gesture::switcher::revoke();
    }
}

/// Disable mouse control at runtime (settings hot-switch).
/// The stop signal is sent immediately and thread reclamation happens in the background so the
/// AppKit main thread never blocks.
pub(crate) fn stop() {
    let handle = {
        let mut runtime = MOUSE_RUNTIME.lock().unwrap();
        // Idempotent: do not spawn duplicate reapers while a stop is already in progress.
        if runtime.stopping {
            return;
        }
        if runtime.thread.is_none() {
            return;
        }
        runtime.stopping = true;
        runtime.thread.take()
    };

    // Signal stop on the current thread, but move the potentially blocking join to a background
    // reaper so AppKit's mouse callback never stalls.
    event_tap::stop();
    // A recording cannot complete either: the tap that feeds it is stopping, so its owner would keep
    // the pointer with nothing to end it.
    gesture::recording::cancel();
    // A gesture in flight cannot complete: its release may never be delivered now.
    let ending = gesture::runtime::with(|rt| {
        let session = rt.session_id();
        rt.invalidate();
        session
    });
    gesture::hud::hide(ending);
    // A switcher this gesture opened is dismissed (the same path Esc takes), and one the user took
    // over is left alone.
    gesture::switcher::revoke();
    gesture::runtime::set_active(false);
    keysim::release_all_queued();
    keysim::clear_system_button_states();
    std::thread::spawn(move || {
        if let Some(handle) = handle {
            let _ = handle.join();
        }

        {
            let mut runtime = MOUSE_RUNTIME.lock().unwrap();
            runtime.stopping = false;
        }
        // If the user re-enabled the feature before stopping finished, restart from the latest
        // config only after the old thread has fully exited.
        let should_restart = tap_wanted();
        if should_restart {
            start();
        }
    });
    log_info!("Mouse event tap stopped.");
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::MouseSection;

    /// A gesture is its own input path: the tap has to run for it, while scrolling, pointer and the
    /// side-button mappings stay off. This is the counter-example that fails on the old gate, where
    /// `tap_wanted` was the mouse master switch.
    #[test]
    fn a_gesture_only_configuration_wants_the_tap_but_not_the_mouse_module() {
        let mouse = MouseSection {
            enabled: false,
            gestures_enabled: true,
            ..MouseSection::default()
        };
        assert!(tap_wanted_for(&mouse));
        assert!(!module_active_for(&mouse));
        assert!(gestures_on_for(&mouse));
    }

    #[test]
    fn both_switches_off_want_no_tap() {
        let mouse = MouseSection::default();
        assert!(!tap_wanted_for(&mouse));
    }

    #[test]
    fn mouse_control_alone_still_wants_the_tap() {
        let mouse = MouseSection {
            enabled: true,
            gestures_enabled: false,
            ..MouseSection::default()
        };
        assert!(tap_wanted_for(&mouse));
        assert!(module_active_for(&mouse));
        assert!(!gestures_on_for(&mouse));
    }
}
