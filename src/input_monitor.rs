//! Runtime Accessibility permission supervision for every global input tap.
//!
//! All global input taps share one runtime permission state. Revocation closes the creation
//! gate before asking each tap to disable itself and exit. Restored permission restarts services
//! from current configuration. UserInput disable is terminal for this process so the watchdog
//! cannot fight the system by repeatedly re-enabling the tap.

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use std::time::Instant;

static ACCESSIBILITY_TRUSTED: AtomicBool = AtomicBool::new(false);
static USER_INPUT_DISABLED: AtomicBool = AtomicBool::new(false);
static STOP_SERVICES_PENDING: AtomicBool = AtomicBool::new(false);
static STARTED: AtomicBool = AtomicBool::new(false);

const POLL_INTERVAL: Duration = Duration::from_millis(200);

pub(crate) fn start() {
    if STARTED.swap(true, Ordering::SeqCst) {
        return;
    }

    ACCESSIBILITY_TRUSTED.store(crate::ffi::has_accessibility_permission(), Ordering::SeqCst);
    std::thread::Builder::new()
        .name("accessibility-permission-monitor".into())
        .spawn(supervise)
        .expect("failed to start Accessibility permission monitor");
}

pub(crate) fn taps_allowed() -> bool {
    ACCESSIBILITY_TRUSTED.load(Ordering::SeqCst) && !USER_INPUT_DISABLED.load(Ordering::SeqCst)
}

/// Whether the process latched the terminal "disabled by user input" state. Exposed so the A2
/// layer (`--e2e-state`) can assert that stopping one service never trips it.
pub(crate) fn user_input_disabled() -> bool {
    USER_INPUT_DISABLED.load(Ordering::SeqCst)
}

pub(crate) fn watchdog_may_enable_tap() -> bool {
    let trusted = crate::ffi::has_accessibility_permission();
    if !trusted {
        if ACCESSIBILITY_TRUSTED.swap(false, Ordering::SeqCst) {
            crate::log_info!("Accessibility permission was revoked; stopping global input taps.");
            STOP_SERVICES_PENDING.store(true, Ordering::SeqCst);
        }
        return false;
    }
    taps_allowed()
}

/// Called for Core Graphics' two tap-disabled pseudo-events. `stop_requested` is the calling
/// tap's own deliberate-stop flag. Returns true when the event should be passed through as a
/// pseudo-event rather than treated as ordinary input.
pub(crate) fn handle_disabled_event(
    event_type: crate::event_tap::CGEventType,
    name: &str,
    stop_requested: bool,
) -> bool {
    // A deliberate stop calls CGEventTapEnable(tap, false), which still delivers the
    // kCGEventTapDisabledByUserInput pseudo-event. Latching that self-inflicted disable as the
    // OS/security case would stop every global tap for the rest of the process, so drop it here.
    // The guard is a top-level check, not a branch on event_type, because it must be reachable for
    // any pseudo-event type (a timeout during a stop is not self-inflicted).
    if is_self_inflicted_disable(event_type, stop_requested) {
        crate::log_debug!(
            "[{}] ignoring the disabled pseudo-event from our own tap stop.",
            name
        );
        return true;
    }
    match event_type {
        crate::event_tap::TAP_DISABLED_BY_TIMEOUT => {
            crate::log_info!(
                "[{}] event tap disabled after timeout; watchdog may recover it.",
                name
            );
            true
        }
        crate::event_tap::TAP_DISABLED_BY_USER_INPUT => {
            if !USER_INPUT_DISABLED.swap(true, Ordering::SeqCst) {
                crate::log_info!(
                    "[{}] event tap disabled by user input; stopping all global input taps until restart.",
                    name
                );
                STOP_SERVICES_PENDING.store(true, Ordering::SeqCst);
            }
            true
        }
        _ => false,
    }
}

/// Whether a tap-disabled pseudo-event is the one our own `CGEventTapEnable(tap, false)` delivers
/// while a deliberate stop is in flight, rather than an OS/security disable. Stopping a tap sets
/// its flag before disabling it, so a self-inflicted `kCGEventTapDisabledByUserInput` is always
/// observed with `stop_requested` set.
fn is_self_inflicted_disable(
    event_type: crate::event_tap::CGEventType,
    stop_requested: bool,
) -> bool {
    stop_requested && event_type == crate::event_tap::TAP_DISABLED_BY_USER_INPUT
}

fn supervise() {
    let mut next_reconcile = Instant::now() + Duration::from_secs(2);
    loop {
        let trusted = crate::ffi::has_accessibility_permission();
        let previous = ACCESSIBILITY_TRUSTED.swap(trusted, Ordering::SeqCst);

        if previous && !trusted {
            crate::log_info!("Accessibility permission was revoked; stopping global input taps.");
            STOP_SERVICES_PENDING.store(true, Ordering::SeqCst);
        }

        if STOP_SERVICES_PENDING.swap(false, Ordering::SeqCst) {
            stop_services();
        }

        if !previous && trusted {
            if USER_INPUT_DISABLED.load(Ordering::SeqCst) {
                // Never re-enable the disabled tap. A fresh process clears this terminal latch
                // and creates new taps only after the OS reports Accessibility as trusted.
                crate::restart::accessibility_restored_after_terminal_tap();
            } else {
                crate::log_info!(
                    "Accessibility permission was restored; restarting configured input taps."
                );
                start_configured_services();
                next_reconcile = Instant::now() + Duration::from_secs(2);
            }
        } else if trusted
            && !USER_INPUT_DISABLED.load(Ordering::SeqCst)
            && Instant::now() >= next_reconcile
        {
            // Periodic reconciliation retries services whose old tap thread was still unwinding
            // or whose tap creation failed transiently.
            start_configured_services();
            next_reconcile = Instant::now() + Duration::from_secs(2);
        }

        std::thread::sleep(POLL_INTERVAL);
    }
}

fn stop_services() {
    // Store the global gate before this function is called, so callbacks already in flight
    // immediately become pass-through while each module disables its Mach port.
    crate::keystroke_display::stop();
    crate::event_monitor::stop();
    crate::mouse::stop();
    crate::window_management::stop();
    crate::quick_actions::stop();
    crate::settings::cancel_recording_from_main();
}

fn start_configured_services() {
    crate::event_monitor::start();
    crate::keystroke_display::start();
    let config = match crate::config::CONFIG.read() {
        Ok(config) => config.clone(),
        Err(_) => return,
    };
    if config.mouse.enabled || crate::dev_flags::present("smooth-scroll-force-on") {
        crate::mouse::start();
    }
    if config.window_control.enabled {
        crate::window_management::start();
    }
    if config.quick_actions.enabled {
        crate::quick_actions::start();
    }
}

#[cfg(test)]
mod tests {
    use super::is_self_inflicted_disable;
    use crate::event_tap::{TAP_DISABLED_BY_TIMEOUT, TAP_DISABLED_BY_USER_INPUT};

    /// Stopping a tap ourselves delivers the UserInput pseudo-event while the stop flag is set.
    /// Classifying it as self-inflicted keeps it from latching the terminal state; toggling one
    /// feature off must not stop the switcher for the rest of the process.
    #[test]
    fn own_stop_is_self_inflicted_not_terminal() {
        assert!(is_self_inflicted_disable(TAP_DISABLED_BY_USER_INPUT, true));
        // A genuine OS/security disable arrives with no stop in flight and must still latch.
        assert!(!is_self_inflicted_disable(
            TAP_DISABLED_BY_USER_INPUT,
            false
        ));
        // A timeout during a stop is recoverable by the watchdog, not the self-inflicted case.
        assert!(!is_self_inflicted_disable(TAP_DISABLED_BY_TIMEOUT, true));
    }
}
