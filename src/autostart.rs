//! Launch at login via SMAppService (macOS 13+): registers the app as a system login item.
//! TOML's [startup] launch_at_login is the source of truth; sync() applies it on startup / reload /
//! settings OK. Only effective when launched as a .app — SMAppService.mainApp relies on the main
//! bundle, which is absent when running the raw binary via `cargo run` (it logs a warn, no other
//! impact).

use objc2::runtime::{AnyObject, Sel};
use objc2::{msg_send, sel};
use std::ffi::CString;

use crate::ffi::{objc_getClass, objc_msgSend};
use crate::log_info;

#[link(name = "ServiceManagement", kind = "framework")]
extern "C" {}

// SMAppServiceStatus: 1 = SMAppServiceStatusRegistered
const STATUS_REGISTERED: isize = 1;

/// Look up an ObjC class by name via raw objc_getClass, bypassing objc2's `class!` macro
/// verification. Returns nil if not found.
unsafe fn cls_id(name: &str) -> *mut AnyObject {
    let c_name = CString::new(name).unwrap();
    objc_getClass(c_name.as_ptr())
}

/// Send a no-arg message (returning id) via raw objc_msgSend, bypassing objc2's msg_send!
/// verification.
unsafe fn send_id(recv: *mut AnyObject, cmd: Sel) -> *mut AnyObject {
    type F = unsafe extern "C" fn(*mut AnyObject, Sel) -> *mut AnyObject;
    let f: F = std::mem::transmute(objc_msgSend as *const ());
    f(recv, cmd)
}

/// Whether we're running as a .app bundle (main bundle has a bundleIdentifier).
/// Under `cargo run` (raw binary) there's no main bundle and thus no app to register:
/// mainAppService returns a service with status=notFound, and register/unregister would just fail.
/// So we probe first and return nil when there's no bundle, letting the caller take the null branch
/// and log a warn instead of making pointless SMAppService calls.
/// Note: this is NOT to prevent an exception - with the correct selector, mainAppService does not
/// throw; what would abort is sending a wrong selector (e.g. the former +mainApp), independent of bundle.
unsafe fn has_main_bundle() -> bool {
    let cls = cls_id("NSBundle");
    if cls.is_null() {
        return false;
    }
    let bundle = send_id(cls, sel!(mainBundle));
    if bundle.is_null() {
        return false;
    }
    let bid = send_id(bundle, sel!(bundleIdentifier));
    !bid.is_null()
}

/// Get the SMAppService.mainApp instance (the main bundle's login-item service).
/// Returns nil when there's no main bundle or the class can't be found, so is_enabled /
/// set_registered take the null branch and degrade gracefully (log a warn, no impact on other features).
unsafe fn main_app() -> *mut AnyObject {
    if !has_main_bundle() {
        return std::ptr::null_mut();
    }
    let cls = cls_id("SMAppService");
    if cls.is_null() {
        return std::ptr::null_mut();
    }
    // Swift's SMAppService.mainApp bridges to the ObjC class method +mainAppService
    // (not +mainApp - that raises an unrecognized-selector exception Rust can't catch and aborts).
    send_id(cls, sel!(mainAppService))
}

/// Whether the app is currently registered as a login item (status == registered).
pub fn is_enabled() -> bool {
    unsafe {
        let service = main_app();
        if service.is_null() {
            return false;
        }
        let status: isize = msg_send![service, status];
        status == STATUS_REGISTERED
    }
}

/// Register (enabled=true) or unregister (false) the login item. Idempotent. Returns whether it
/// succeeded (the NSError, if any, is released). Uses raw objc_msgSend because the NSError**
/// out-parameter is awkward to encode through objc2's msg_send! — same escape hatch the project
/// already uses for hex_to_cg_color et al.
unsafe fn set_registered(enabled: bool) -> bool {
    let service = main_app();
    if service.is_null() {
        return false;
    }
    // Swift's register()/unregister() bridge to ObjC as -registerAndReturnError: /
    // -unregisterAndReturnError: (NSError** out-param), not registerWithError: / unregisterWithError:
    // (the latter are not real selectors and raise an unrecognized-selector exception that aborts).
    let sel = if enabled {
        sel!(registerAndReturnError:)
    } else {
        sel!(unregisterAndReturnError:)
    };
    type F = unsafe extern "C" fn(*mut AnyObject, Sel, *mut *mut AnyObject) -> bool;
    let f: F = std::mem::transmute(objc_msgSend as *const ());
    let mut err: *mut AnyObject = std::ptr::null_mut();
    let ok = f(service, sel, &mut err);
    // The NSError** out-param is autoreleased by Cocoa convention; the caller does not own it, so
    // never release it - the former release(err) here over-released it, crashing (zombie) when the
    // run loop later drained the autorelease pool. Hit by "Restore Defaults", which calls
    // autostart::sync(false) -> unregisterAndReturnError: returns an autoreleased NSError.
    let _ = err;
    ok
}

/// Sync the system login-item state to match `enabled`, logging the outcome.
pub fn sync(enabled: bool) {
    let ok = unsafe { set_registered(enabled) };
    if ok {
        log_info!(
            "autostart: {} (status={})",
            if enabled {
                "registered"
            } else {
                "unregistered"
            },
            if is_enabled() { "enabled" } else { "disabled" },
        );
    } else {
        log_info!(
            "autostart: {} failed — run as .app? (ad-hoc signed apps may need a one-time approval \
             in System Settings > Login Items)",
            if enabled { "register" } else { "unregister" },
        );
    }
}
