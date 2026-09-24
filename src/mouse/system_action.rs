//! System actions: trigger Mission Control / Launchpad / Show Desktop / App Expose through
//! the Dock's private API. `CoreDockSendNotification` is a private ApplicationServices
//! symbol (same declaration as LinearMouse's DockKitC) -- it sends a notification string
//! straight to the Dock, bypassing keyboard-event synthesis, so it is not subject to the
//! "synthetic events can't trigger system-level shortcuts" limitation.

use crate::ffi::make_nsstring;
use crate::log_debug;
use std::ffi::c_void;

#[link(name = "ApplicationServices", kind = "framework")]
extern "C" {
    // Private SPI: send a system-function notification to the Dock (2nd arg is always 0).
    fn CoreDockSendNotification(notification: *mut c_void, unknown: i32) -> i32;
}

/// Fire a system action (called once on button press; Dock notifications are toggles, no
/// paired release needed).
pub(crate) fn fire(notification: &'static str) {
    unsafe {
        let ns = make_nsstring(notification);
        let _ = CoreDockSendNotification(ns as *mut c_void, 0);
        crate::ffi::CFRelease(ns as *const c_void);
    }
    log_debug!("[mouse] system action: {}", notification);
}
