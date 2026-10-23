//! App identity resolution, shared by window_collector and icon_cache.
//! it for cache keys, forming a module cycle. Both sides now depend one-way on
//! this module instead.

use objc2::runtime::AnyObject;
use objc2::{class, msg_send};
use std::ffi::{c_char, CStr};

use crate::ffi::{make_nsstring, CFRelease};
use crate::hash::fnv1a64_hex;
use std::ffi::c_void;

/// A running app's cache identity: `key` is the cache filename, `fingerprint` detects updates.
pub(crate) struct AppIdentity {
    pub(crate) key: String,
    /// Executable mtime (seconds since UNIX epoch). None means unverified -> "file exists = valid".
    pub(crate) fingerprint: Option<String>,
    /// Process start time in microseconds since UNIX epoch, used to distinguish PID reuse.
    pub(crate) process_start_time_us: Option<u64>,
}

/// Read an NSString into a Rust String (nil -> None). The object is autoreleased; caller must be in a pool.
unsafe fn read_nsstring(obj: *mut AnyObject) -> Option<String> {
    if obj.is_null() {
        return None;
    }
    let utf8: *const c_char = msg_send![obj, UTF8String];
    if utf8.is_null() {
        return None;
    }
    Some(CStr::from_ptr(utf8).to_string_lossy().into_owned())
}

/// The bundle identifier and display name of an application bundle on disk.
///
/// Used by the clipboard exclusion list's "choose an app" control, which must also reach an app that
/// is not running. `NSBundle` answers the identifier without parsing Info.plist by hand; a path that
/// is not a bundle (or has no identifier) yields `None` so the caller can refuse the entry.
pub(crate) unsafe fn bundle_identity_at_path(path: &str) -> Option<(String, String)> {
    let path_ns = make_nsstring(path);
    let bundle: *mut AnyObject = msg_send![class!(NSBundle), bundleWithPath: path_ns];
    CFRelease(path_ns as *const c_void);
    if bundle.is_null() {
        return None;
    }
    let id_obj: *mut AnyObject = msg_send![bundle, bundleIdentifier];
    let bundle_id = read_nsstring(id_obj).filter(|b| !b.is_empty())?;
    // The display name is the bundle's own localized name; fall back to the file name without the
    // extension so an entry is never presented as a bare identifier when a name exists.
    let name = std::path::Path::new(path)
        .file_stem()
        .map(|stem| stem.to_string_lossy().into_owned())
        .unwrap_or_else(|| bundle_id.clone());
    Some((bundle_id, name))
}

/// Every running regular (Dock-visible) application that has a bundle identifier, sorted by
/// display name, with this process excluded. Backs the clipboard exclusion list's "add a running
/// app" control: only apps that can actually own a copy are offered.
pub(crate) unsafe fn running_apps_with_bundle_ids() -> Vec<(String, String)> {
    let own_pid = std::process::id() as i32;
    let workspace: *mut AnyObject = msg_send![class!(NSWorkspace), sharedWorkspace];
    let apps: *mut AnyObject = msg_send![workspace, runningApplications];
    if apps.is_null() {
        return Vec::new();
    }
    let count: usize = msg_send![apps, count];
    let mut result: Vec<(String, String)> = Vec::new();
    for i in 0..count {
        let app: *mut AnyObject = msg_send![apps, objectAtIndex: i as isize];
        if app.is_null() {
            continue;
        }
        let policy: isize = msg_send![app, activationPolicy];
        if policy != 0 {
            continue;
        }
        let pid: i32 = msg_send![app, processIdentifier];
        if pid <= 0 || pid == own_pid {
            continue;
        }
        let bid_obj: *mut AnyObject = msg_send![app, bundleIdentifier];
        let Some(bundle_id) = read_nsstring(bid_obj).filter(|b| !b.is_empty()) else {
            continue;
        };
        let name_obj: *mut AnyObject = msg_send![app, localizedName];
        let name = read_nsstring(name_obj).unwrap_or_else(|| bundle_id.clone());
        if !result.iter().any(|(existing, _)| *existing == bundle_id) {
            result.push((bundle_id, name));
        }
    }
    result.sort_by_key(|(_, name)| name.to_lowercase());
    result
}

/// Just the bundle identifier of a running PID (None when the app has none or the pid is stale).
/// Deliberately cheaper than `resolve_app_identity`: the clipboard's exclusion check runs before
/// the payload is read, so it must not pay for an executable stat and fingerprint.
pub(crate) unsafe fn bundle_id_for_pid(pid: i32) -> Option<String> {
    if pid <= 0 {
        return None;
    }
    let app: *mut AnyObject =
        msg_send![class!(NSRunningApplication), runningApplicationWithProcessIdentifier: pid];
    if app.is_null() {
        return None;
    }
    let bid_obj: *mut AnyObject = msg_send![app, bundleIdentifier];
    read_nsstring(bid_obj).filter(|b| !b.is_empty())
}

/// Resolve a PID's cache identity. Key priority: bundleIdentifier (reverse-DNS,
/// filename-safe) > hashed executable path > `pid_{pid}` fallback. Fingerprint is the
/// executable mtime; an app update gets a new mtime -> forces re-extract. The clipboard
/// reuses this identity when recording a source (the same key/fallback chain as the switcher).
pub(crate) unsafe fn resolve_app_identity(pid: i32) -> AppIdentity {
    let app: *mut AnyObject =
        msg_send![class!(NSRunningApplication), runningApplicationWithProcessIdentifier: pid];
    if app.is_null() {
        // PID stale (app just quit) -> fall back to pid key, no fingerprint (can't verify).
        return AppIdentity {
            key: format!("pid_{}", pid),
            fingerprint: None,
            process_start_time_us: None,
        };
    }

    let process_start_time_us = {
        let launch_date: *mut AnyObject = msg_send![app, launchDate];
        if launch_date.is_null() {
            None
        } else {
            let seconds: f64 = msg_send![launch_date, timeIntervalSince1970];
            (seconds.is_finite() && seconds >= 0.0).then_some((seconds * 1_000_000.0) as u64)
        }
    };

    let bid_obj: *mut AnyObject = msg_send![app, bundleIdentifier];
    let bundle_id = read_nsstring(bid_obj);
    // An empty identifier is not an identifier: treat it as absent so a caller never matches "".
    let bundle_id = bundle_id.filter(|b| !b.is_empty());

    let exec_url: *mut AnyObject = msg_send![app, executableURL];
    let exec_path = if exec_url.is_null() {
        None
    } else {
        let path_obj: *mut AnyObject = msg_send![exec_url, path];
        read_nsstring(path_obj)
    };

    // Fingerprint = exec mtime (seconds). Unavailable (empty path / stat fail) -> None, no verification.
    let fingerprint = exec_path.as_ref().and_then(|p| {
        std::fs::metadata(p)
            .ok()
            .and_then(|m| m.modified().ok())
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs().to_string())
    });

    let key = if let Some(bid) = bundle_id {
        bid
    } else if let Some(p) = exec_path {
        format!("exec_{}", fnv1a64_hex(&p))
    } else {
        format!("pid_{}", pid)
    };

    AppIdentity {
        key,
        fingerprint,
        process_start_time_us,
    }
}
