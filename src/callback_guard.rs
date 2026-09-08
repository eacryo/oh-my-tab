//! Panic boundaries for Rust callbacks invoked by C and Objective-C runtimes.
//!
//! A panic cannot unwind through an `extern "C"` callback. These small helpers keep the
//! boundary explicit and provide conservative fallbacks for each callback return shape.
//!
//! C/Objective-C runtime 调用 Rust callback 时的 panic 边界。
//!
//! panic 不能穿过 `extern "C"` 回调展开；这些 helper 统一限制边界，并为不同返回值提供保守回退。

use std::panic::{catch_unwind, AssertUnwindSafe};

/// Run a void callback and log instead of allowing panic to cross the ABI boundary.
pub(crate) fn void(label: &'static str, callback: impl FnOnce()) {
    if catch_unwind(AssertUnwindSafe(callback)).is_err() {
        crate::log_info!("[callback] panic contained in {label}");
    }
}

/// Run a query callback and return a conservative fallback when it panics.
pub(crate) fn bool(label: &'static str, fallback: bool, callback: impl FnOnce() -> bool) -> bool {
    match catch_unwind(AssertUnwindSafe(callback)) {
        Ok(value) => value,
        Err(_) => {
            crate::log_info!("[callback] panic contained in {label}; using fallback={fallback}");
            fallback
        }
    }
}

/// Run an event-tap callback and preserve the original event when its Rust body panics.
pub(crate) fn event<T>(label: &'static str, original: T, callback: impl FnOnce() -> T) -> T {
    match catch_unwind(AssertUnwindSafe(callback)) {
        Ok(value) => value,
        Err(_) => {
            crate::log_info!("[callback] panic contained in {label}; passing event through");
            original
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{bool, event, void};

    #[test]
    fn void_callback_contains_panics() {
        void("test_void", || panic!("expected test panic"));
    }

    #[test]
    fn bool_callback_uses_conservative_fallback() {
        assert!(!bool("test_bool", false, || panic!("expected test panic")));
        assert!(bool("test_bool", false, || true));
    }

    #[test]
    fn event_callback_preserves_original_event_on_panic() {
        let original = 42usize;
        assert_eq!(
            event("test_event", original, || panic!("expected test panic")),
            original
        );
        assert_eq!(event("test_event", original, || 7), 7);
    }
}
