//! macOS scheduling helpers for latency-sensitive parts of the switcher.
//!
//! The switcher is an accessory app, so its event-tap and bridge threads can otherwise
//! inherit a relatively ordinary QoS class while the app is inactive.  Keep the policy
//! centralized here: only short, user-visible paths are promoted and background work is
//! deliberately left at Utility/Background QoS.

use crate::ffi::{make_nsstring, release_obj, CFRelease, ObjPtr};
use crate::{class, log_debug, msg_send};
use objc2::runtime::AnyObject;
use std::ffi::c_void;
use std::sync::{LazyLock, Mutex};

/// Darwin QoS classes from `<sys/qos.h>`.
///
/// These values are part of the public macOS ABI.  Keeping them here avoids spreading raw
/// constants through the event-tap and worker implementations.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ThreadQos {
    UserInteractive,
    UserInitiated,
    Utility,
    Background,
}

impl ThreadQos {
    fn raw(self) -> u32 {
        match self {
            Self::UserInteractive => 0x21,
            Self::UserInitiated => 0x19,
            Self::Utility => 0x11,
            Self::Background => 0x09,
        }
    }
}

#[link(name = "System", kind = "dylib")]
extern "C" {
    fn pthread_set_qos_class_self_np(qos_class: u32, relative_priority: i32) -> i32;
}

/// Set the QoS of the calling thread.  A failure is diagnostic only: scheduling policy must
/// never prevent the event tap or the app from starting.
pub(crate) fn set_current_thread_qos(qos: ThreadQos) {
    let result = unsafe { pthread_set_qos_class_self_np(qos.raw(), 0) };
    if result != 0 {
        log_debug!(
            "[perf] pthread_set_qos_class_self_np failed: qos={:?} errno={}",
            qos,
            result
        );
    }
}

/// `NSActivityUserInitiatedAllowingIdleSystemSleep` from NSProcessInfo.h.  Build it from the
/// documented public flags instead of baking the combined hexadecimal value into call sites.
const NS_ACTIVITY_IDLE_SYSTEM_SLEEP_DISABLED: u64 = 1 << 20;
const NS_ACTIVITY_USER_INITIATED: u64 = 0x00FF_FFFF | NS_ACTIVITY_IDLE_SYSTEM_SLEEP_DISABLED;
const NS_ACTIVITY_USER_INITIATED_ALLOWING_IDLE_SYSTEM_SLEEP: u64 =
    NS_ACTIVITY_USER_INITIATED & !NS_ACTIVITY_IDLE_SYSTEM_SLEEP_DISABLED;

/// The activity token is owned by this process and only touched on the main thread.  The extra
/// retain is intentional because `beginActivityWithOptions:reason:` returns an autoreleased
/// token when called through the raw MRC bridge used by this project.
static SWITCHER_ACTIVITY: LazyLock<Mutex<Option<ObjPtr>>> = LazyLock::new(|| Mutex::new(None));

/// Tell App Nap/sudden-termination policy that a user-visible switcher operation is in flight.
/// Idempotent so multiple key-down paths cannot leak nested activity tokens.
pub(crate) fn begin_switcher_activity() {
    let mut slot = SWITCHER_ACTIVITY.lock().unwrap();
    if slot.is_some() {
        return;
    }

    unsafe {
        let process_info: *mut AnyObject = msg_send![class!(NSProcessInfo), processInfo];
        if process_info.is_null() {
            log_debug!("[perf] NSProcessInfo processInfo returned null");
            return;
        }
        let reason = make_nsstring("Oh My Tab switcher interaction");
        let token: *mut AnyObject = msg_send![
            process_info,
            beginActivityWithOptions: NS_ACTIVITY_USER_INITIATED_ALLOWING_IDLE_SYSTEM_SLEEP,
            reason: reason
        ];
        CFRelease(reason as *const c_void);
        if token.is_null() {
            log_debug!("[perf] beginActivityWithOptions returned null");
            return;
        }
        let retained: *mut AnyObject = msg_send![token, retain];
        if retained.is_null() {
            log_debug!("[perf] retaining switcher activity token failed");
            return;
        }
        *slot = Some(ObjPtr(retained));
    }
}

/// End the current switcher activity.  Safe to call from every dismissal/cancel path.
pub(crate) fn end_switcher_activity() {
    let token = SWITCHER_ACTIVITY.lock().unwrap().take();
    let Some(token) = token else {
        return;
    };

    unsafe {
        let process_info: *mut AnyObject = msg_send![class!(NSProcessInfo), processInfo];
        if !process_info.is_null() {
            let _: () = msg_send![process_info, endActivity: token.0];
        }
        release_obj(token.0);
    }
}

#[cfg(test)]
mod tests {
    use super::ThreadQos;

    #[test]
    fn qos_values_match_darwin_public_constants() {
        assert_eq!(ThreadQos::UserInteractive.raw(), 0x21);
        assert_eq!(ThreadQos::UserInitiated.raw(), 0x19);
        assert_eq!(ThreadQos::Utility.raw(), 0x11);
        assert_eq!(ThreadQos::Background.raw(), 0x09);
    }
}
