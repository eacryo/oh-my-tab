//! Panic boundary for callbacks entered from Objective-C.
//!
//! A panic cannot unwind across an `extern "C"` boundary: without this, a malformed message would
//! abort the extension process inside Finder instead of failing one callback.

use std::panic::{catch_unwind, AssertUnwindSafe};

pub(crate) fn void(label: &'static str, callback: impl FnOnce()) {
    if catch_unwind(AssertUnwindSafe(callback)).is_err() {
        crate::ffi::log(&format!("panic contained in {label}"));
    }
}

pub(crate) fn query<T>(label: &'static str, fallback: T, callback: impl FnOnce() -> T) -> T {
    match catch_unwind(AssertUnwindSafe(callback)) {
        Ok(value) => value,
        Err(_) => {
            crate::ffi::log(&format!("panic contained in {label}"));
            fallback
        }
    }
}
