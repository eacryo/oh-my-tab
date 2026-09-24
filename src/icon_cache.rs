//! The app icon disk cache under `~/Library/Caches/oh-my-tab-icons/`. Keys come
//! from AppIdentity (bundle id > hashed exec path > pid fallback); a `.meta`
//! sidecar stores the executable mtime as the invalidation fingerprint. The
//! switcher's big icon (128pt) and the clipboard's small one (16pt) share this
//! pipeline and fingerprint.

use objc2::runtime::AnyObject;
use objc2::{class, msg_send};
use std::ffi::c_void;

use crate::app_identity::{resolve_app_identity, AppIdentity};
use crate::ffi::{CFRelease, CFStringCreateWithCString};
use crate::log_debug;

fn icon_cache_dir() -> String {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string());
    // Test builds use a dedicated sibling directory: the smoke tests' clear_icon_cache()
    // only clears the test dir, never the user's real icon cache (clearing the real one
    // used to force a full re-extract on the next summon, stalling ~400ms).
    let name = if cfg!(test) {
        "oh-my-tab-icons-test"
    } else {
        "oh-my-tab-icons"
    };
    format!("{}/Library/Caches/{}", home, name)
}

/// The cached PNG path (suffix-aware: "" = the switcher's big icon, ".small" = the
/// clipboard's small one).
fn cache_path_for_key_suffix(key: &str, suffix: &str) -> String {
    format!("{}/{}{}.png", icon_cache_dir(), key, suffix)
}

fn meta_path_for_key(key: &str) -> String {
    format!("{}/{}.meta", icon_cache_dir(), key)
}

/// Validate the cache: PNG exists, and (when a fingerprint is present) the sidecar
/// matches. An app update changes the mtime -> fingerprint mismatch -> None -> re-extract.
pub(crate) fn check_cache_for_identity(id: &AppIdentity) -> Option<String> {
    check_cache_for_suffix(id, "")
}

/// Same as above, suffix-aware for the small icon; both sizes share one .meta fingerprint.
fn check_cache_for_suffix(id: &AppIdentity, suffix: &str) -> Option<String> {
    let png = cache_path_for_key_suffix(&id.key, suffix);
    if std::fs::metadata(&png).is_err() {
        return None;
    }
    match &id.fingerprint {
        Some(fp) => match std::fs::read_to_string(meta_path_for_key(&id.key)) {
            Ok(stored) if stored.trim() == *fp => Some(png),
            _ => None,
        },
        None => Some(png), // no fingerprint -> file exists = valid
    }
}

pub fn ensure_icon_cache_dir() {
    let _ = std::fs::create_dir_all(icon_cache_dir());
}

/// One-shot migration: remove legacy PID-named cache files (purely-numeric filename stem).
/// New keys are bundle ids (letters/dots) or `exec_`/`pid_`-prefixed, never purely numeric,
/// so nothing legitimate is touched.
pub fn migrate_legacy_cache() {
    let Ok(entries) = std::fs::read_dir(icon_cache_dir()) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        // Only remove .png files whose stem is purely numeric (legacy PID files).
        if path.extension().and_then(|e| e.to_str()) == Some("png") {
            if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
                if !stem.is_empty() && stem.bytes().all(|b| b.is_ascii_digit()) {
                    let _ = std::fs::remove_file(&path);
                }
            }
        }
    }
}

/// Clear the icon cache directory (remove all {key}.png + {key}.meta), then recreate it empty.
/// In-memory WindowInfo.icon_path is NOT invalidated here; the caller must reset it to None
/// and trigger re-extraction.
pub fn clear_icon_cache() {
    let dir = icon_cache_dir();
    // errors if the dir doesn't exist; ignore
    let _ = std::fs::remove_dir_all(&dir);
    ensure_icon_cache_dir();
}

/// The icon cache is keyed by the app's bundle id (falling back to the executable path for
/// non-bundle apps), NOT by PID: PID recycling can never serve another app's stale icon. Each
/// entry has a `.meta` sidecar storing the executable mtime; an app update/reinstall changes
/// the mtime -> verification fails -> re-extract, so no TTL is needed. Apps that change their
/// icon at runtime (Calendar date, dock badge) still freeze until the cache is cleared - an
/// accepted minor limitation.
pub fn check_icon_cache(pid: i32) -> Option<String> {
    let id = unsafe { resolve_app_identity(pid) };
    check_cache_for_identity(&id)
}

fn write_png_to_cache(png: *mut AnyObject, key: &str, suffix: &str) -> Option<String> {
    unsafe {
        let path = cache_path_for_key_suffix(key, suffix);
        let path_cstr = std::ffi::CString::new(&*path).unwrap();
        let cf_path = CFStringCreateWithCString(std::ptr::null(), path_cstr.as_ptr(), 0x08000100);
        // Atomic write: write to a temp file then rename, so a mid-write crash
        // can't leave a half-written PNG that "file exists = valid" would trust.
        let ok: bool = msg_send![png, writeToFile: cf_path as *mut AnyObject, atomically: true];
        CFRelease(cf_path);
        if ok {
            Some(path)
        } else {
            None
        }
    }
}

/// Negative-cache marker for a failed extraction: `{key}{suffix}.missing`, holding the same
/// fingerprint as `.meta`. An app update (fingerprint change) invalidates it, so it cannot
/// hide a changed icon; while it matches, extraction is skipped -- some processes
/// (loginwindow) never yield a big icon and would otherwise re-run the whole pipeline on
/// every summon and spam the collect-side miss log.
fn miss_path_for_key_suffix(key: &str, suffix: &str) -> String {
    format!("{}/{}{}.missing", icon_cache_dir(), key, suffix)
}

/// Whether this identity (same fingerprint) is already known to fail extraction. The big
/// icon ("") and the clipboard's small one (".small") are tracked independently.
pub(crate) fn extraction_known_missing(id: &AppIdentity, suffix: &str) -> bool {
    let Some(fp) = &id.fingerprint else {
        // No fingerprint -> the marker could never invalidate; keep retrying.
        return false;
    };
    std::fs::read_to_string(miss_path_for_key_suffix(&id.key, suffix))
        .is_ok_and(|stored| stored.trim() == fp)
}

fn mark_extraction_missing(id: &AppIdentity, suffix: &str) {
    if let Some(fp) = &id.fingerprint {
        let _ = std::fs::write(miss_path_for_key_suffix(&id.key, suffix), fp);
    }
}

fn clear_extraction_missing(key: &str, suffix: &str) {
    let _ = std::fs::remove_file(miss_path_for_key_suffix(key, suffix));
}

/// Extraction entry point with negative caching: a same-fingerprint known failure is skipped;
/// a success clears any stale marker.
fn extract_icon_to_cache_sized(pid: i32, pt_size: f64, suffix: &str) -> Option<String> {
    // Identity resolution gets its own autorelease pool: before NSApp run the main thread has
    // no pool and its autoreleased objects (app/URL/path) would leak wholesale.
    let id = unsafe {
        let pool: *mut AnyObject = msg_send![class!(NSAutoreleasePool), new];
        let id = resolve_app_identity(pid);
        let _: () = msg_send![pool, drain];
        id
    };
    extract_icon_to_cache_resolved(pid, &id, pt_size, suffix)
}

/// Extract with an ALREADY-RESOLVED app identity: callers that already have one (e.g. the
/// clipboard record path) avoid resolving the same PID again.
fn extract_icon_to_cache_resolved(
    pid: i32,
    id: &AppIdentity,
    pt_size: f64,
    suffix: &str,
) -> Option<String> {
    if extraction_known_missing(id, suffix) {
        return None;
    }
    let result = extract_icon_render(pid, id, pt_size, suffix);
    match &result {
        Some(_) => clear_extraction_missing(&id.key, suffix),
        None => mark_extraction_missing(id, suffix),
    }
    result
}

/// Extract an app icon into the cache at the target point size: the switcher's big icon
/// (128pt) and the clipboard's small one (16pt) share this pipeline. `suffix`: the filename
/// suffix ("" -> {key}.png, ".small" -> {key}.small.png); both sizes share one {key}.meta
/// fingerprint (the same executable mtime).
fn extract_icon_render(pid: i32, id: &AppIdentity, pt_size: f64, suffix: &str) -> Option<String> {
    unsafe {
        use objc2_foundation::{NSPoint, NSRect, NSSize};

        // Wrap in an autorelease pool: app/icon/tiff/rep/png are autoreleased (+0). At startup
        // this runs before NSApp run (no pool yet), so they'd all leak - the ~40MB startup cause.
        let pool: *mut AnyObject = msg_send![class!(NSAutoreleasePool), new];

        // Hit an existing valid cache (mtime-verified) -> skip extraction.
        if let Some(path) = check_cache_for_suffix(id, suffix) {
            let _: () = msg_send![pool, drain];
            return Some(path);
        }

        // Source icon: for our own process use the compile-time-embedded AppIcon.icns --
        // cargo run is a bare exec with no bundle, so NSRunningApplication.icon returns the
        // generic exec icon (the "EXEC" placeholder); this forces our own icon so dev and
        // bundled builds match. Other processes still go through NSRunningApplication.icon.
        let icon: *mut AnyObject = if pid == std::process::id() as i32 {
            let icns_bytes: &[u8] = include_bytes!("../assets/AppIcon.icns");
            let nsdata: *mut AnyObject = msg_send![
                class!(NSData),
                dataWithBytes: icns_bytes.as_ptr() as *const c_void,
                length: icns_bytes.len()
            ];
            // NSImage has no +imageWithData: class method; use alloc + initWithData: (+1) then
            // autorelease so it's pool-managed like app.icon below, with no manual release.
            let img: *mut AnyObject = msg_send![class!(NSImage), alloc];
            let img: *mut AnyObject = msg_send![img, initWithData: nsdata];
            if !img.is_null() {
                let _: *mut AnyObject = msg_send![img, autorelease];
            }
            img
        } else {
            let cls = class!(NSRunningApplication);
            let app: *mut AnyObject = msg_send![cls, runningApplicationWithProcessIdentifier: pid];
            if app.is_null() {
                let _: () = msg_send![pool, drain];
                return None;
            }
            msg_send![app, icon]
        };
        if icon.is_null() {
            let _: () = msg_send![pool, drain];
            return None;
        }

        // Render at Retina resolution: pt_size pt display → 2x (or 1x) pixels.
        let scale: f64 = {
            let screen: *mut AnyObject = msg_send![class!(NSScreen), mainScreen];
            if screen.is_null() {
                2.0
            } else {
                msg_send![screen, backingScaleFactor]
            }
        };
        let px = pt_size * scale;

        let target_img: *mut AnyObject = msg_send![class!(NSImage), alloc];
        let target_img: *mut AnyObject = msg_send![target_img, initWithSize: NSSize::new(px, px)];

        // Draw icon into target with high-quality interpolation (NSImageInterpolationHigh)
        let _: () = msg_send![target_img, lockFocus];
        let dst = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(px, px));
        let src = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(0.0, 0.0));
        let op: usize = 1; // NSCompositingOperationCopy
        let _: () =
            msg_send![icon, drawInRect: dst, fromRect: src, operation: op, fraction: 1.0f64];
        let _: () = msg_send![target_img, unlockFocus];

        // Convert to PNG at target size
        let tiff: *mut AnyObject = msg_send![target_img, TIFFRepresentation];
        let _: () = msg_send![target_img, release]; // target_img is an alloc (+1) the pool cannot own, so release it manually
        if tiff.is_null() {
            let _: () = msg_send![pool, drain];
            return None;
        }

        let rep_cls = class!(NSBitmapImageRep);
        let rep: *mut AnyObject = msg_send![rep_cls, imageRepWithData: tiff];
        if rep.is_null() {
            let _: () = msg_send![pool, drain];
            return None;
        }

        // NSBitmapImageFileTypePNG = 4
        let png: *mut AnyObject = msg_send![rep, representationUsingType: 4u64, properties: std::ptr::null::<AnyObject>()];
        if png.is_null() {
            let _: () = msg_send![pool, drain];
            return None;
        }

        let result = write_png_to_cache(png, &id.key, suffix);
        // Write the mtime sidecar: next hit checks it to detect app updates (mtime change ->
        // re-extract). Only written when the PNG succeeds, so no orphan meta is left behind.
        if result.is_some() {
            if let Some(fp) = &id.fingerprint {
                let _ = std::fs::write(meta_path_for_key(&id.key), fp);
            }
        }
        let _: () = msg_send![pool, drain];
        result
    }
}

pub fn extract_icon_to_cache(pid: i32) -> Option<String> {
    extract_icon_to_cache_sized(pid, 128.0, "")
}

/// The clipboard header's small icon (16pt, 32px @2x). Called when the source is recorded
/// (the app is guaranteed alive then); the key and .meta fingerprint are shared with the
/// switcher's big icon, while `{key}.small.png` is a separate file.
pub fn extract_small_icon(pid: i32) -> Option<String> {
    extract_icon_to_cache_sized(pid, 16.0, ".small")
}

/// Extract the clipboard small icon using a caller-resolved app identity (resolve first,
/// then extract). Avoids resolving the same PID twice within one recording.
pub(crate) fn extract_small_icon_for_identity(pid: i32, id: &AppIdentity) -> Option<String> {
    extract_icon_to_cache_resolved(pid, id, 16.0, ".small")
}

/// The clipboard small-icon path (for existence checks; key = resolve_app_identity's key).
pub fn small_icon_path_for_key(key: &str) -> String {
    cache_path_for_key_suffix(key, ".small")
}

/// Pre‑cache icons for every currently‑running regular application.
/// Called once at startup so the overlay never shows a missing icon.
///
/// Pre-cache icons for every currently-running REGULAR application. Helper
/// background processes (e.g. PixCake's nested pix-worker/pix-camera-link) share
/// the main app's bundle id, and their NSRunningApplication.icon is the AppKit
/// generic placeholder -- extracting poisons the shared cache key, and since the
/// helpers' binary mtimes equal the main binary's (same install), the .meta check
/// can never detect it. Helpers have no windows; their icons are never needed.
pub(crate) fn cache_running_app_icons() {
    let mut cached: Vec<String> = Vec::new();
    let mut skipped: usize = 0;
    unsafe {
        // This runs before NSApp run, when the main thread has no autorelease pool yet;
        // runningApplications / localizedName are autoreleased, so wrap in a pool to drain them.
        let pool: *mut AnyObject = msg_send![class!(NSAutoreleasePool), new];
        let workspace: *mut AnyObject = msg_send![class!(NSWorkspace), sharedWorkspace];
        let running: *mut AnyObject = msg_send![workspace, runningApplications];
        let count: usize = msg_send![running, count];
        for i in 0..count {
            let app: *mut AnyObject = msg_send![running, objectAtIndex: i];
            // NSApplicationActivationPolicyRegular = 0; skip background/helper apps.
            let policy: i64 = msg_send![app, activationPolicy];
            if policy != 0 {
                skipped += 1;
                continue;
            }
            let pid: i32 = msg_send![app, processIdentifier];
            if check_icon_cache(pid).is_none() {
                let name_str = crate::ffi::ns_running_app_name(app);
                let name_str = if name_str.is_empty() {
                    "?".to_string()
                } else {
                    name_str
                };
                log_debug!("cached icon: {} (pid {})", name_str, pid);
                cached.push(name_str);
                extract_icon_to_cache(pid);
            } else {
                skipped += 1;
            }
        }
        let _: () = msg_send![pool, drain];
    }
    log_debug!(
        "icon cache done: {} cached, {} skipped (already fresh / non-regular)",
        cached.len(),
        skipped,
    );
}

/// Pre-warm the clipboard header's small icons (16pt) at startup. Only called when the
/// clipboard feature is enabled (gated in main.rs); the small cache stays ungenerated when
/// the feature is off -- extracting it for every running app would be wasted work.
pub(crate) fn cache_running_app_icons_small() {
    let mut cached: Vec<String> = Vec::new();
    unsafe {
        // Same as cache_running_app_icons: no autorelease pool before NSApp run.
        let pool: *mut AnyObject = msg_send![class!(NSAutoreleasePool), new];
        let workspace: *mut AnyObject = msg_send![class!(NSWorkspace), sharedWorkspace];
        let running: *mut AnyObject = msg_send![workspace, runningApplications];
        let count: usize = msg_send![running, count];
        for i in 0..count {
            let app: *mut AnyObject = msg_send![running, objectAtIndex: i];
            // Same as cache_running_app_icons: skip background/helper apps (their icons
            // would poison the shared cache key).
            let policy: i64 = msg_send![app, activationPolicy];
            if policy != 0 {
                continue;
            }
            let pid: i32 = msg_send![app, processIdentifier];
            // extract_small_icon verifies {key}.small.png + the mtime fingerprint, hitting
            // the cache skips the work.
            if extract_small_icon(pid).is_some() {
                cached.push(pid.to_string());
            }
        }
        let _: () = msg_send![pool, drain];
    }
    log_debug!(
        "small icon cache done: {} cached/verified (clipboard)",
        cached.len()
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn small_icon_path_uses_dot_small_suffix() {
        // The clipboard small icon = {key}.small.png, same key and dir as the switcher's
        // big icon ({key}.png).
        let key = "com.apple.Safari";
        let path = small_icon_path_for_key(key);
        assert!(path.ends_with(&format!("{}.small.png", key)), "{}", path);
        assert!(!path.contains("..png"));
        // The big/small paths differ only by the suffix.
        let big = cache_path_for_key_suffix(key, "");
        assert!(big.ends_with(&format!("{}.png", key)), "{}", big);
        assert_eq!(path, format!("{}.small.png", &big[..big.len() - 4]));
    }

    #[test]
    fn extraction_miss_marker_is_fingerprint_keyed_and_suffix_scoped() {
        ensure_icon_cache_dir();
        let key = "test.extraction.miss";
        let with_fp = |fp: &str| AppIdentity {
            key: key.to_string(),
            fingerprint: Some(fp.to_string()),
            process_start_time_us: None,
        };
        clear_extraction_missing(key, "");
        assert!(!extraction_known_missing(&with_fp("111"), ""));

        mark_extraction_missing(&with_fp("111"), "");
        assert!(extraction_known_missing(&with_fp("111"), ""));
        // A fingerprint change (app update) invalidates the marker -> retry.
        assert!(!extraction_known_missing(&with_fp("222"), ""));
        // A big-icon failure must not block the small icon (suffix-scoped).
        assert!(!extraction_known_missing(&with_fp("111"), ".small"));

        // No fingerprint (can never invalidate) -> no marker, no negative caching.
        let unverifiable = AppIdentity {
            key: key.to_string(),
            fingerprint: None,
            process_start_time_us: None,
        };
        mark_extraction_missing(&unverifiable, "");
        assert!(!extraction_known_missing(&unverifiable, ""));

        // The clear-on-success path.
        clear_extraction_missing(key, "");
        assert!(!extraction_known_missing(&with_fp("111"), ""));
    }

    #[test]
    #[ignore]
    fn icon_cache_roundtrip_smoke() {
        if !crate::ffi::has_accessibility_permission() {
            eprintln!("[smoke] Accessibility not granted; skipping icon roundtrip");
            return;
        }
        // Use Finder (stable bundle id) for the roundtrip; the test binary itself is a bare
        // exec whose identity resolution is unreliable.
        let pid = unsafe {
            let ns_key = crate::ffi::make_nsstring("com.apple.finder");
            let apps: *mut AnyObject = msg_send![
                class!(NSRunningApplication),
                runningApplicationsWithBundleIdentifier: ns_key
            ];
            CFRelease(ns_key as *const c_void);
            let count: usize = msg_send![apps, count];
            let mut pid: i32 = 0;
            if count > 0 {
                let app: *mut AnyObject = msg_send![apps, objectAtIndex: 0usize];
                pid = msg_send![app, processIdentifier];
            }
            pid
        };
        assert!(pid > 0, "Finder must be running in a GUI session");
        // Start clean by clearing the cache (the smoke test re-extracts; acceptable side effect).
        clear_icon_cache();
        let path = extract_icon_to_cache(pid).expect("Finder icon extraction failed");
        assert!(std::fs::metadata(&path).is_ok(), "extracted PNG must exist");
        // A second query hits the cache; idempotent same path.
        assert_eq!(check_icon_cache(pid).as_deref(), Some(path.as_str()));
        assert_eq!(
            extract_icon_to_cache(pid).as_deref(),
            Some(path.as_str()),
            "re-extract must short-circuit on a valid cache"
        );
        clear_icon_cache();
    }
}
