//! Minimal ObjC/Foundation helpers for the extension. The app's ffi module is not available here:
//! the appex is a separate package so its process entry point and link arguments stay separate.

use objc2::runtime::AnyObject;
use objc2::{class, msg_send};
use std::ffi::{c_char, CStr, CString};

/// Build an autoreleased NSString.
pub(crate) unsafe fn nsstring(value: &str) -> *mut AnyObject {
    let text = CString::new(value).unwrap_or_default();
    msg_send![class!(NSString), stringWithUTF8String: text.as_ptr()]
}

/// Copy an NSString into a Rust String.
pub(crate) unsafe fn nsstring_to_rust(value: *mut AnyObject) -> String {
    if value.is_null() {
        return String::new();
    }
    let utf8: *const c_char = msg_send![value, UTF8String];
    if utf8.is_null() {
        return String::new();
    }
    CStr::from_ptr(utf8).to_string_lossy().into_owned()
}

/// Build an autoreleased NSMenu. NSMenu has no +menuWithTitle: convenience initializer.
pub(crate) unsafe fn new_menu(title: &str) -> *mut AnyObject {
    let allocated: *mut AnyObject = msg_send![class!(NSMenu), alloc];
    let menu: *mut AnyObject = msg_send![allocated, initWithTitle: nsstring(title)];
    if !menu.is_null() {
        let _: *mut AnyObject = msg_send![menu, autorelease];
    }
    menu
}

/// Log through NSLog: the extension is sandboxed, so the unified log is the channel a developer can
/// read without Full Disk Access.
pub(crate) fn log(line: &str) {
    unsafe {
        extern "C" {
            fn NSLog(format: *mut AnyObject, ...);
        }
        let format = nsstring("%@");
        let message = nsstring(&format!("[finder-ext] {line}"));
        NSLog(format, message);
    }
}
