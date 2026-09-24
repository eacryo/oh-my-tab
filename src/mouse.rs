//! Mouse enhancement module.
//! Provides two scroll modes: Default (passthrough + optional reverse) and Line (fixed line
//! count).

pub(crate) mod device;
pub(crate) mod event_tap;
pub(crate) mod ffi;
pub(crate) mod keysim;
pub(crate) mod pointer;
pub(crate) mod resolve;
pub(crate) mod scrolling;
pub(crate) mod shortcut;
pub(crate) mod system_action;

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
    log_info!("Mouse control enabled.");
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
        let should_restart = crate::config::CONFIG
            .read()
            .map(|cfg| cfg.mouse.enabled)
            .unwrap_or(false);
        if should_restart {
            start();
        }
    });
    log_info!("Mouse control disabled.");
}
