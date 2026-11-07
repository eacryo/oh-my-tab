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

/// Whether a process is an ExtensionKit / appex extension rather than a switchable application.
///
/// An app extension runs out of `....appex/Contents/MacOS/<name>` (system ones under
/// `/System/Library/ExtensionKit/Extensions/`). It can publish ordinary layer-0 windows — the
/// "Screen Saver"/"Displays"/"VPN" panes in System Settings each own one — but its process is not
/// activatable: `_SLPSSetFrontProcessWithOptions` answers `-606` (procNotFound) and
/// `activateWithOptions:` returns false, so the card cannot be opened even though AX raises the
/// window without error.
///
/// The window itself carries no evidence for this: the extension's window is layer 0, alpha 1.0
/// and ordinary-sized, so every layer/shape rule admits it. Only the executable's location tells
/// them apart, which is why this keys on the path rather than on anything about the window.
///
/// Input methods are deliberately not matched: an IME is a `prohibited` process too, and it lives
/// under `/Library/Input Methods/` in an ordinary `.app` bundle.
pub(crate) fn is_extension_process(executable_path: &str) -> bool {
    // EVERY slash-delimited component is tested, not just the first one containing ".appex". In
    // "/Applications/Foo.appexhelper.app/Contents/PlugIns/Widget.appex/Contents/MacOS/Widget" the
    // first hit is ".appexhelper"; stopping there classified a real extension as an application.
    executable_path
        .split('/')
        .any(|component| component.ends_with(".appex"))
}

/// The executable path of a running process, or None when the process cannot be resolved.
pub(crate) unsafe fn executable_path_of(pid: i32) -> Option<String> {
    let app: *mut AnyObject =
        msg_send![class!(NSRunningApplication), runningApplicationWithProcessIdentifier: pid];
    if app.is_null() {
        return None;
    }
    let exec_url: *mut AnyObject = msg_send![app, executableURL];
    if exec_url.is_null() {
        return None;
    }
    let path_obj: *mut AnyObject = msg_send![exec_url, path];
    read_nsstring(path_obj)
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

/// Every application installed in the standard locations, with its bundle identifier and the
/// name the Finder shows.
///
/// The gesture exclusion list is about "this app must not act on my gestures", which includes apps
/// that are not running, so the menu cannot be built from the running-application list alone: it
/// lists what is INSTALLED, and the caller merges the running set into it for the ones started
/// outside these folders (a disk image, a build output).
///
/// The standard folders are the ones macOS itself documents. Nested applications inside a bundle
/// (a browser's helper app, an app's embedded framework) are not offered: they are not things the
/// user launches.
pub(crate) fn installed_application_bundles() -> Vec<(String, String)> {
    let home = std::env::var_os("HOME")
        .map(std::path::PathBuf::from)
        .unwrap_or_default();
    let mut roots = vec![
        std::path::PathBuf::from("/Applications"),
        std::path::PathBuf::from("/Applications/Utilities"),
        std::path::PathBuf::from("/System/Applications"),
        std::path::PathBuf::from("/System/Applications/Utilities"),
    ];
    if !home.as_os_str().is_empty() {
        roots.push(home.join("Applications"));
    }
    let mut found: Vec<(String, String)> = Vec::new();
    for root in roots {
        let Ok(entries) = std::fs::read_dir(&root) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|ext| ext.to_str()) != Some("app") {
                continue;
            }
            let Some(path) = path.to_str() else {
                continue;
            };
            let Some((bundle_id, name)) = (unsafe { bundle_identity_at_path(path) }) else {
                continue;
            };
            if !found.iter().any(|(existing, _)| *existing == bundle_id) {
                found.push((bundle_id, name));
            }
        }
    }
    found.sort_by_key(|(_, name)| name.to_lowercase());
    found
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

/// The application's user-visible name, for the menu bar's application menu.
///
/// The bundle's display name, so a development build reads "Oh-My-Tab Dev" and the installed build
/// "Oh-My-Tab" for exactly the same code. The fixed name is the fallback for a process started
/// outside a bundle, where the system would otherwise show the executable's name ("oh-my-tab").
pub(crate) fn application_display_name() -> String {
    for key in ["CFBundleDisplayName", "CFBundleName"] {
        let name = unsafe { crate::ffi::bundle_info_string(key) }
            .trim()
            .to_string();
        if !name.is_empty() {
            return name;
        }
    }
    "Oh My Tab".to_string()
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

/// The bundle identifier of the application that is frontmost right now.
///
/// `NSWorkspace.frontmostApplication` is a property read, not a query that waits on another process,
/// so a background tap may ask it: the gesture module uses it to answer "is the excluded app in
/// front" at the moment a press arrives rather than at the last activation notification.
pub(crate) unsafe fn frontmost_bundle_id() -> Option<String> {
    let workspace: *mut AnyObject = msg_send![class!(NSWorkspace), sharedWorkspace];
    if workspace.is_null() {
        return None;
    }
    let app: *mut AnyObject = msg_send![workspace, frontmostApplication];
    if app.is_null() {
        return None;
    }
    let identifier: *mut AnyObject = msg_send![app, bundleIdentifier];
    read_nsstring(identifier).filter(|id| !id.is_empty())
}
/// The bundle identifier of an `NSRunningApplication` object the caller already holds.
///
/// Separate from `bundle_id_for_pid` so a caller that needed the object anyway (to prove the PID is
/// alive) reads the identifier from THAT object: a second `runningApplicationWithProcessIdentifier`
/// can answer differently if the process quits in between.
pub(crate) unsafe fn bundle_id_of(app: *mut AnyObject) -> Option<String> {
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
    identity_of_app(app, pid)
}

/// The identity of an `NSRunningApplication` the caller ALREADY holds.
///
/// Why this exists: a caller that resolved the app for another purpose (the clipboard's exclusion
/// test) must not resolve it a second time to record the source. Two lookups can answer differently
/// when the process quits in between, and the record would then describe another application.
pub(crate) unsafe fn identity_of_app(app: *mut AnyObject, pid: i32) -> AppIdentity {
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

#[cfg(test)]
mod tests {
    use super::is_extension_process;

    /// The exclusion list is about an app that is INSTALLED, not one that happens to be open:
    /// building it from the running set alone was the reported defect (an app in /Applications that
    /// the user had quit simply was not offered).
    ///
    /// Asserted against the real machine only in the shape that is true everywhere: the list is
    /// sorted, deduplicated by bundle identifier, and never offers an entry without an identifier.
    #[test]
    fn installed_applications_are_deduplicated_and_sorted() {
        let apps = super::installed_application_bundles();
        let mut ids: Vec<&str> = apps.iter().map(|(id, _)| id.as_str()).collect();
        let total = ids.len();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), total, "one bundle offered twice");
        assert!(
            apps.iter()
                .all(|(id, name)| !id.is_empty() && !name.is_empty()),
            "every installed entry has an identifier and a name"
        );
        let names: Vec<String> = apps.iter().map(|(_, name)| name.to_lowercase()).collect();
        let mut sorted = names.clone();
        sorted.sort();
        assert_eq!(names, sorted, "the list is presented in name order");
    }

    /// App extensions publish ordinary layer-0 windows and cannot be activated (-606), so the card
    /// never opens. The executable's location is the only evidence, and these are the three the
    /// collector actually met on this machine plus the two shapes a path can take.
    #[test]
    fn a_bundle_under_an_appex_is_an_extension() {
        for path in [
            "/System/Library/ExtensionKit/Extensions/LockScreen.appex/Contents/MacOS/LockScreen",
            "/System/Library/ExtensionKit/Extensions/DisplaysExt.appex/Contents/MacOS/DisplaysExt",
            "/System/Library/ExtensionKit/Extensions/VPN.appex/Contents/MacOS/VPN",
            "/System/Applications/System Settings.app/Contents/PlugIns/Some.appex/Contents/MacOS/Some",
        ] {
            assert!(is_extension_process(path), "{path} is an app extension");
        }
    }

    /// Ordinary applications, and the input method that is `prohibited` for the same reason but
    /// must stay switchable, are not extensions.
    #[test]
    fn an_ordinary_application_is_not_an_extension() {
        for path in [
            "/Applications/Oh My Tab.app/Contents/MacOS/oh-my-tab",
            "/Library/Input Methods/DoubaoIme.app/Contents/MacOS/DoubaoIme",
            "/System/Library/Frameworks/AppKit.framework/Versions/C/XPCServices/com.apple.appkit.xpc.openAndSavePanelService.xpc/Contents/MacOS/com.apple.appkit.xpc.openAndSavePanelService",
            "",
        ] {
            assert!(!is_extension_process(path), "{path} is not an app extension");
        }
    }

    /// A name that merely starts with `.appex` is a different file, not an extension bundle.
    #[test]
    fn an_appex_prefix_without_a_path_boundary_does_not_match() {
        assert!(!is_extension_process(
            "/tmp/Foo.appexhelper/Contents/MacOS/Foo"
        ));
        assert!(!is_extension_process("/tmp/my.appexfile"));
        assert!(is_extension_process("/tmp/Foo.appex"));
    }

    /// A false `.appex` prefix earlier in the path must not mask a real extension later in it.
    /// Testing only the first match read this path as an application.
    #[test]
    fn a_false_appex_prefix_does_not_mask_a_real_extension() {
        assert!(is_extension_process(
            "/Applications/Foo.appexhelper.app/Contents/PlugIns/Widget.appex/Contents/MacOS/Widget"
        ));
        assert!(is_extension_process(
            "/tmp/my.appexfile/Contents/PlugIns/Real.appex/Contents/MacOS/Real"
        ));
    }
}
