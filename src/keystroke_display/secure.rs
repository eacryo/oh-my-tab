//! Secure Input polling and a short-lived Accessibility cache.
//!
//! Keystroke content must never appear in logs. AX calls stay on this worker, never the UI thread.

use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

static RUNNING: AtomicBool = AtomicBool::new(false);
static FORCE_SECURE: AtomicBool = AtomicBool::new(false);
static GENERATION: AtomicU64 = AtomicU64::new(0);
static WORKER: OnceLock<Mutex<Option<JoinHandle<()>>>> = OnceLock::new();
static LAST_ACTIVITY: OnceLock<Mutex<Option<Instant>>> = OnceLock::new();
static AX_CACHE: OnceLock<Mutex<Option<(Instant, bool)>>> = OnceLock::new();

const SECURE_POLL_INTERVAL: Duration = Duration::from_millis(50);
const AX_REFRESH_INTERVAL: Duration = Duration::from_millis(250);
const AX_CACHE_MAX_AGE: Duration = Duration::from_millis(750);
const RECENT_ACTIVITY_WINDOW: Duration = Duration::from_secs(3);

#[link(name = "Carbon", kind = "framework")]
extern "C" {
    fn IsSecureEventInputEnabled() -> u8;
}

#[link(name = "ApplicationServices", kind = "framework")]
extern "C" {
    fn AXUIElementCreateSystemWide() -> crate::ffi::AXUIElementRef;
}

pub(super) fn note_key_activity() {
    *LAST_ACTIVITY
        .get_or_init(|| Mutex::new(None))
        .lock()
        .unwrap_or_else(|error| error.into_inner()) = Some(Instant::now());
}

pub(super) fn start(force_secure: bool) {
    FORCE_SECURE.store(force_secure, Ordering::SeqCst);
    if RUNNING.swap(true, Ordering::SeqCst) {
        return;
    }
    let generation = GENERATION.fetch_add(1, Ordering::SeqCst) + 1;
    let worker = thread::Builder::new()
        .name("keystroke-secure-input".into())
        .spawn(move || poll_secure_state(generation))
        .expect("failed to start secure-input monitor");
    let mut slot = WORKER
        .get_or_init(|| Mutex::new(None))
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    if let Some(old) = slot.replace(worker) {
        if old.is_finished() {
            let _ = old.join();
        }
    }
}

pub(super) fn stop() {
    RUNNING.store(false, Ordering::SeqCst);
    GENERATION.fetch_add(1, Ordering::SeqCst);
    FORCE_SECURE.store(false, Ordering::SeqCst);
}

fn poll_secure_state(generation: u64) {
    let mut last_sent = None;
    let mut next_ax_refresh = Instant::now();
    while RUNNING.load(Ordering::SeqCst) && GENERATION.load(Ordering::SeqCst) == generation {
        let now = Instant::now();
        let system_secure = unsafe { IsSecureEventInputEnabled() != 0 };
        let recent = LAST_ACTIVITY
            .get_or_init(|| Mutex::new(None))
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .is_some_and(|last| now.saturating_duration_since(last) <= RECENT_ACTIVITY_WINDOW);

        if recent && now >= next_ax_refresh {
            if let Some(secure) = focused_element_is_secure() {
                *AX_CACHE
                    .get_or_init(|| Mutex::new(None))
                    .lock()
                    .unwrap_or_else(|error| error.into_inner()) = Some((now, secure));
            }
            next_ax_refresh = now + AX_REFRESH_INTERVAL;
        }

        let ax_secure = if recent {
            AX_CACHE
                .get_or_init(|| Mutex::new(None))
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .as_ref()
                .filter(|(updated, _)| now.saturating_duration_since(*updated) <= AX_CACHE_MAX_AGE)
                .is_some_and(|(_, secure)| *secure)
        } else {
            false
        };
        let active = system_secure || ax_secure || FORCE_SECURE.load(Ordering::SeqCst);
        if last_sent != Some(active) {
            super::enqueue(super::Input::SecureActive(active));
            last_sent = Some(active);
        }
        thread::sleep(SECURE_POLL_INTERVAL);
    }
}

/// A successful query with no secure subrole is false; a failed/missing query is None and does
/// not replace the previous cache, which expires open after AX_CACHE_MAX_AGE.
fn focused_element_is_secure() -> Option<bool> {
    unsafe {
        let system_wide = AXUIElementCreateSystemWide();
        if system_wide.is_null() {
            return None;
        }
        let focused_key = crate::window_collector::cf_string_new("AXFocusedUIElement");
        if focused_key.is_null() {
            crate::ffi::CFRelease(system_wide);
            return None;
        }
        let mut focused: *const c_void = std::ptr::null();
        let result =
            crate::ffi::AXUIElementCopyAttributeValue(system_wide, focused_key, &mut focused);
        crate::ffi::CFRelease(focused_key);
        crate::ffi::CFRelease(system_wide);
        if result != crate::ffi::K_AX_SUCCESS || focused.is_null() {
            if !focused.is_null() {
                crate::ffi::CFRelease(focused);
            }
            return (result == crate::ffi::K_AX_SUCCESS).then_some(false);
        }

        let subrole_key = crate::window_collector::cf_string_new("AXSubrole");
        if subrole_key.is_null() {
            crate::ffi::CFRelease(focused);
            return None;
        }
        let mut subrole: *const c_void = std::ptr::null();
        let result = crate::ffi::AXUIElementCopyAttributeValue(focused, subrole_key, &mut subrole);
        crate::ffi::CFRelease(subrole_key);
        crate::ffi::CFRelease(focused);
        if result != crate::ffi::K_AX_SUCCESS || subrole.is_null() {
            if !subrole.is_null() {
                crate::ffi::CFRelease(subrole);
            }
            return (result == crate::ffi::K_AX_SUCCESS).then_some(false);
        }
        let secure_subrole = crate::window_collector::cf_string_new("AXSecureTextField");
        let is_secure = if secure_subrole.is_null() {
            None
        } else {
            Some(crate::ffi::CFStringCompare(subrole, secure_subrole, 0) == 0)
        };
        crate::ffi::CFRelease(subrole);
        if !secure_subrole.is_null() {
            crate::ffi::CFRelease(secure_subrole);
        }
        is_secure
    }
}
