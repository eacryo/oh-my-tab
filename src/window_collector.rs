use objc2::runtime::AnyObject;
use objc2::{class, msg_send, sel};
use std::collections::{HashMap, HashSet};
use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant};

use crate::app_identity::{resolve_app_identity, AppIdentity};
use crate::config::CONFIG;
use crate::ffi::{
    kCFBooleanFalse, AXError, AXUIElementCopyAttributeValue,
    AXUIElementCopyMultipleAttributeValues, AXUIElementCreateApplication, AXUIElementGetTypeID,
    AXUIElementPerformAction, AXUIElementRef, AXUIElementSetAttributeValue,
    AXUIElementSetMessagingTimeout, AXValueGetType, AXValueGetTypeID, CFArrayCreate,
    CFArrayGetCount, CFArrayGetTypeID, CFArrayGetValueAtIndex, CFBooleanGetValue,
    CFDictionaryGetValue, CFGetTypeID, CFNumberGetValue, CFRelease, CFRetain,
    CFStringCreateWithCString, CFStringGetCString, CGWindowListCopyWindowInfo,
    K_AX_CANNOT_COMPLETE, K_AX_INVALID_UI_ELEMENT, K_AX_SUCCESS,
};
#[cfg(test)]
use crate::hash::fnv1a64_hex;
use crate::icon_cache::{check_cache_for_identity, extraction_known_missing};
use crate::skylight;
use crate::{log_debug, log_info};

mod collect;
mod raise;
mod raiser;
// The parent no longer calls collect directly; the glob is test-only.
#[cfg(test)]
use collect::*;
use raise::*;
use raiser::*;
// Entry points exposed to the rest of the crate (implemented in the child modules).
pub(crate) use collect::{
    collect_windows, collect_windows_for_pid, collect_windows_with_frontmost_bump,
    switchable_capture_window_for_pid,
};
pub(crate) use raise::{
    ax_window_cgwid, cf_string_new, clear_ax_window_cache_for_pid, close_ax_window,
    focused_window_cgwid, forget_non_normal_window, raise_window_fast,
};
pub(crate) use raiser::{
    cf_to_rust_string, get_ax_windows_for_pid, handle_ax_raise_main, raise_window_ax_async,
};
static SLPS_GET_PROCESS_MISSING_LOGGED: AtomicBool = AtomicBool::new(false);
static SLPS_SET_FRONT_MISSING_LOGGED: AtomicBool = AtomicBool::new(false);
static SLPS_POST_EVENT_MISSING_LOGGED: AtomicBool = AtomicBool::new(false);

fn log_missing_slps_symbol(logged: &AtomicBool, message: &str) {
    if !logged.swap(true, Ordering::Relaxed) {
        log_info!("{}", message);
    }
}

/// The set of (pid, icon-cache key) pairs whose `[collect] icon miss` line was already
/// printed -- one line per identity per process (rationale at the call site). Bounded by the
/// number of distinct apps seen, so it cannot grow without limit.
static ICON_MISS_LOGGED: LazyLock<Mutex<HashSet<(i32, String)>>> =
    LazyLock::new(|| Mutex::new(HashSet::new()));

/// True on the first sighting of this (pid, key) (rate-limit pass); with no identity
/// available the line is never limited, keeping it visible.
fn icon_miss_unlogged(pid: i32, key: Option<&str>) -> bool {
    let Some(key) = key else {
        return true;
    };
    ICON_MISS_LOGGED
        .lock()
        .unwrap()
        .insert((pid, key.to_string()))
}

#[derive(Debug, Clone, PartialEq)]
pub struct WindowInfo {
    pub pid: i32,
    pub window_id: u32, // CGWindowID, used for exact raise (SLPS) and pairing
    pub app_name: String,
    pub window_title: String,
    pub icon_path: Option<String>,
    pub is_active: bool,
    pub minimized: bool,  // minimized (collected only when show_minimized is on)
    pub app_hidden: bool, // app hidden with Command+H
    // CG window bounds (x, y, w, h), used to locate the active window's screen. All zeros = unavailable.
    pub bounds: (f64, f64, f64, f64),
}

/// Window-level MRU timestamps, keyed by (pid, CGWindowID).
/// Each window is tracked independently — no app-level grouping.
/// Sorted by elapsed ascending — most recently used windows come first.
pub type MruMap = HashMap<(i32, u32), Instant>;

/// PID → last app activation time (via NSWorkspace notification).
/// Used only to seed a window the first time it enters the MRU map; existing windows
/// are never updated together when their app activates.
static LAST_ACTIVATED: std::sync::LazyLock<std::sync::Mutex<HashMap<i32, Instant>>> =
    std::sync::LazyLock::new(|| std::sync::Mutex::new(HashMap::new()));

/// Called from the NSWorkspaceDidActivateApplicationNotification handler in main.rs.
/// Returns this activation's token so async focus queries can discard stale results.
pub fn note_app_activated(pid: i32) -> Instant {
    let activated_at = Instant::now();
    LAST_ACTIVATED.lock().unwrap().insert(pid, activated_at);
    activated_at
}

/// Whether an async activation query still belongs to the PID's latest activation.
pub fn app_activation_is_current(pid: i32, activated_at: Instant) -> bool {
    LAST_ACTIVATED.lock().unwrap().get(&pid).copied() == Some(activated_at)
}

/// Clear the activation seed on termination so PID reuse cannot inherit old process state.
pub fn note_app_terminated(pid: i32) {
    LAST_ACTIVATED.lock().unwrap().remove(&pid);
}

/// Bump the MRU timestamp of a specific window to now. Called from three paths:
/// 1. Window selected inside oh-my-tab (on_cmd_released / card_mouse_down / KEY_RETURN)
/// 2. NSWorkspace activation notification → on_app_activated resolves the focused
///    window on a background thread, then calls this on the main thread — this is how
///    external focus switches (system Cmd+Tab, Dock clicks) feed into window ordering.
pub fn bump_window_mru(mru: &mut MruMap, pid: i32, cgwid: u32) {
    if cgwid != 0 {
        mru.insert((pid, cgwid), Instant::now());
    }
}

/// Pure window-level MRU sort: ascending by last-activation time (most recent first);
/// windows without an MRU record fall back to 999s (treated as very old, sorted last).
/// Pure — `now` is supplied by the caller, so tests can inject a fixed clock.
pub(crate) fn sort_windows_by_mru(windows: &mut [WindowInfo], mru: &MruMap, now: Instant) {
    let age = |pid: i32, wid: u32| {
        mru.get(&(pid, wid))
            .map(|t| now.saturating_duration_since(*t))
            .unwrap_or(std::time::Duration::from_secs(999))
    };
    windows.sort_by_key(|a| age(a.pid, a.window_id));
}

/// Enumerate every layer-0 CG window for WindowServer focus observation.
/// This is not the display list: AX still decides what is shown, while observation conservatively
/// covers every owner PID.
pub(crate) fn window_server_candidates() -> Vec<(u32, i32)> {
    unsafe {
        let array = CGWindowListCopyWindowInfo(K_C_G_WINDOW_LIST_OPTION_ALL, 0);
        if array.is_null() {
            return Vec::new();
        }
        let count = CFArrayGetCount(array);
        let mut candidates = Vec::new();
        let mut seen = HashSet::new();
        for i in 0..count {
            let dict = CFArrayGetValueAtIndex(array, i);
            if dict.is_null() || cf_dict_get_i32(dict, "kCGWindowLayer").unwrap_or(999) != 0 {
                continue;
            }
            let pid = cf_dict_get_i32(dict, "kCGWindowOwnerPID").unwrap_or(-1);
            let window_id = cf_dict_get_u32(dict, "kCGWindowNumber").unwrap_or(0);
            if pid > 0 && window_id != 0 && seen.insert((window_id, pid)) {
                candidates.push((window_id, pid));
            }
        }
        CFRelease(array);
        candidates
    }
}

/// Get visible ordinary windows and their public bounds in the current Space for the
/// thumbnail Show Desktop geometry probe. This avoids AX and main-thread UI state.
pub(crate) fn ordinary_onscreen_window_bounds() -> Vec<(u32, (f64, f64, f64, f64))> {
    const ON_SCREEN_ONLY: u32 = 1 << 0;
    const EXCLUDE_DESKTOP_ELEMENTS: u32 = 1 << 4;

    unsafe {
        let array = CGWindowListCopyWindowInfo(ON_SCREEN_ONLY | EXCLUDE_DESKTOP_ELEMENTS, 0);
        if array.is_null() {
            return Vec::new();
        }
        let own_pid = std::process::id() as i32;
        let count = CFArrayGetCount(array);
        let mut windows = Vec::new();
        for i in 0..count {
            let dict = CFArrayGetValueAtIndex(array, i);
            if dict.is_null()
                || cf_dict_get_i32(dict, "kCGWindowLayer").unwrap_or(999) != 0
                || cf_dict_get_i32(dict, "kCGWindowOwnerPID").unwrap_or(-1) == own_pid
                || cf_dict_get_f64(dict, "kCGWindowAlpha").unwrap_or(0.0) <= 0.0
                || !cf_dict_get_bool(dict, "kCGWindowIsOnscreen").unwrap_or(false)
            {
                continue;
            }
            let window_id = cf_dict_get_u32(dict, "kCGWindowNumber").unwrap_or(0);
            let bounds = cf_dict_get_bounds(dict, "kCGWindowBounds").unwrap_or_default();
            if window_id == 0 || bounds.2 < 160.0 || bounds.3 < 120.0 {
                continue;
            }
            windows.push((window_id, bounds));
        }
        CFRelease(array);
        windows.sort_by(|(_, a), (_, b)| {
            (b.2 * b.3)
                .partial_cmp(&(a.2 * a.3))
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        windows.truncate(3);
        windows
    }
}

/// Resolve an unindexed window's owner PID from a fresh CG snapshot when the subscription index
/// cannot answer it.
pub(crate) fn owner_pid_for_cgwid(window_id: u32) -> Option<i32> {
    window_server_candidates()
        .into_iter()
        .find_map(|(candidate, pid)| (candidate == window_id).then_some(pid))
}

/// Prune MRU entries not in the live window set (prevents CGWindowID-reuse inheriting a dead
/// timestamp); returns how many were dropped.
fn prune_mru(mru: &mut MruMap, live_set: &HashSet<(i32, u32)>) -> usize {
    let before = mru.len();
    mru.retain(|k, _| live_set.contains(k));
    before - mru.len()
}

/// Remove every window MRU for a process; termination calls this to immediately prevent
/// PID/CGWindowID reuse from inheriting stale timestamps.
pub fn remove_pid_mru(mru: &mut MruMap, pid: i32) -> usize {
    let before = mru.len();
    mru.retain(|(entry_pid, _), _| *entry_pid != pid);
    before - mru.len()
}

/// Initialize a new window once: prefer the app's latest activation time, otherwise use
/// ancient; the order offset only stabilizes windows first seen in the same batch. `or_insert`
/// is what prevents existing sibling windows from riding an app activation.
fn initialize_window_mru(
    mru: &mut MruMap,
    pid: i32,
    cgwid: u32,
    app_activated_at: Option<Instant>,
    ancient_base: Instant,
    insertion_order: u32,
) {
    let base = app_activated_at.unwrap_or(ancient_base);
    let ordered_ts = base
        .checked_sub(Duration::from_millis(insertion_order as u64))
        .unwrap_or(base);
    mru.entry((pid, cgwid)).or_insert(ordered_ts);
}

/// If the AX focused query fails, fallback is restricted to the system-reported frontmost
/// app rather than accidentally bumping the first item in the global CG list.
fn frontmost_fallback(windows: &[WindowInfo], front_pid: Option<i32>) -> Option<(i32, u32)> {
    let pid = front_pid?;
    windows
        .iter()
        .find(|w| w.pid == pid && !w.minimized)
        .map(|w| (w.pid, w.window_id))
}

/// Seed the MRU from the front-to-back window order at startup (same-app windows grouped,
/// app-level order = the CG front-to-back order).
///
/// **Important (load-bearing): this order is only a startup placeholder and is NOT
/// guaranteed to match the native Cmd+Tab order.** macOS exposes no public API for the
/// switcher's app MRU, and the CG z-order reflects "window created / clicked-raised"
/// history rather than app-activation order -- verified: activating an app (Cmd+Tab / Dock)
/// does NOT reorder the z-order (Ghostty/Edge stayed deep in the z-order after activation).
/// So this is just a plausible initial order we impose, ensuring: 1) same-app windows are
/// grouped; 2) the order is stable and reproducible; 3) everything sorts ahead of the 999s
/// fallback. The precise order is corrected live by the activation notifications (MRU bumps)
/// once the app is running. To exactly restore the pre-restart order, the MRU would need to
/// be persisted to disk (see the clipboard-history persistence pattern), not seeded.
pub fn seed_mru_from_system_order() -> MruMap {
    unsafe {
        let array = CGWindowListCopyWindowInfo(K_C_G_WINDOW_LIST_OPTION_ALL, 0);
        if array.is_null() {
            return MruMap::new();
        }
        let count = CFArrayGetCount(array);
        let now = Instant::now();
        // App first-appearance order (front-to-back) + each app's windows in CG order.
        // ONLY on-screen windows rank: invisible/minimized/other-Space windows interleave
        // in the z-order and skew the app ranking (Clash Verge used to rank high thanks
        // to its off-screen windows).
        let mut app_order: Vec<i32> = Vec::new();
        let mut app_windows: HashMap<i32, Vec<u32>> = HashMap::new();
        let mut app_names: HashMap<i32, String> = HashMap::new();
        for i in 0..count {
            let dict = CFArrayGetValueAtIndex(array, i);
            if dict.is_null() {
                continue;
            }
            let layer = cf_dict_get_i32(dict, "kCGWindowLayer").unwrap_or(999);
            if layer != 0 {
                continue;
            }
            let onscreen = cf_dict_get_bool(dict, "kCGWindowIsOnscreen").unwrap_or(false);
            if !onscreen {
                continue;
            }
            let pid = cf_dict_get_i32(dict, "kCGWindowOwnerPID").unwrap_or(-1);
            if pid <= 0 {
                continue;
            }
            let owner_name = cf_dict_get_string(dict, "kCGWindowOwnerName").unwrap_or_default();
            if owner_name.is_empty() || owner_name == "Dock" {
                continue;
            }
            if !app_order.contains(&pid) {
                app_order.push(pid);
                app_names.insert(pid, owner_name);
            }
            let cgwid = cf_dict_get_u32(dict, "kCGWindowNumber").unwrap_or(0);
            let list = app_windows.entry(pid).or_default();
            if cgwid != 0 && !list.contains(&cgwid) {
                list.push(cgwid);
            }
        }
        // Print the startup PLACEHOLDER order (front-to-back, app-grouped). This is only an
        // approximation, NOT the native Cmd+Tab order (activation does not reorder the
        // z-order; see the fn doc) -- printed for cross-checking and debugging.
        log_debug!("[seed] startup placeholder window order (front-to-back, on-screen only):");
        for pid in &app_order {
            let wins = app_windows.get(pid).map(|v| v.as_slice()).unwrap_or(&[]);
            let names: Vec<String> = wins.iter().map(|w| w.to_string()).collect();
            log_debug!(
                "  pid={} app=\"{}\" windows=[{}]",
                pid,
                app_names.get(pid).map(|s| s.as_str()).unwrap_or("?"),
                names.join(", ")
            );
        }
        seed_timestamps(&app_order, &app_windows, now)
    }
}

/// Pure logic: assign each window a monotonically increasing "age" in display order
/// (app-grouped + per-app CG order), so sort_windows_by_mru (ascending age) emits that
/// order; all < 1s, still ahead of the 999s fallback. Pure -- `now` is injected by tests.
fn seed_timestamps(
    app_order: &[i32],
    app_windows: &HashMap<i32, Vec<u32>>,
    now: Instant,
) -> MruMap {
    let mut mru = MruMap::new();
    let mut step: u64 = 0;
    for pid in app_order {
        if let Some(wins) = app_windows.get(pid) {
            for &cgwid in wins {
                let t = now - std::time::Duration::from_millis(step);
                mru.insert((*pid, cgwid), t);
                step += 1;
            }
        }
    }
    mru
}

// Enumeration constants: All (0) includes off-screen windows (orderOut'd dialogs /
// minimized / other Spaces); collection always uses it -- whether an off-screen window
// shows is decided by AX semantics (see collect_windows' filter comments).
const K_C_G_WINDOW_LIST_OPTION_ALL: u32 = 0;

// AX types

/// A cached AX window element needs its own CF retain; wrap the raw pointer before sharing it
/// across the background collector and the serialized raiser thread.
#[derive(Clone, Copy)]
struct CachedAxElement(AXUIElementRef);
unsafe impl Send for CachedAxElement {}
unsafe impl Sync for CachedAxElement {}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
struct AxWindowCacheKey {
    pid: i32,
    process_start_time_us: u64,
    cgwid: u32,
}

/// `(process instance, cgwid) -> AXUIElement` cache. Normal raises reuse the element; only
/// stale elements trigger a fresh AXWindows enumeration. PID alone is not an identity because
/// macOS can recycle it.
static AX_WINDOW_CACHE: LazyLock<Mutex<HashMap<AxWindowCacheKey, CachedAxElement>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

#[derive(Clone)]
struct CachedAxSnapshot {
    process_start_time_us: Option<u64>,
    refreshed_at: Instant,
    windows: Vec<AxWindowInfo>,
}

/// AX semantic facts needed after pairing with the WindowServer snapshot.
/// A native tab bar: every tab of a window group is its own window, but only the SELECTED one
/// exposes the tab bar and the full tab-title list; background tab windows do not. That
/// asymmetry is what identifies "these windows are one tab group" (see collect::fold_tab_group).
#[derive(Clone, Debug)]
struct TabGroupInfo {
    /// Tab titles, in tab-bar order.
    titles: Vec<String>,
}

/// The AX role/subrole answers what a surface is; WindowServer layer and bounds answer where
/// it is and whether a custom root is substantial enough to be a switch destination.
#[derive(Clone, Debug)]
struct AxWindowInfo {
    cgwid: u32,
    title: String,
    minimized: bool,
    is_main: bool,
    is_fullscreen: bool,
    is_custom_root: bool,
    /// The native tab bar this window exposes (only the selected tab's window has one), used by
    /// collect::fold_tab_group to identify the group's windows.
    tab_group: Option<TabGroupInfo>,
    /// Whether this entry came ONLY from the kAXFocusedWindow/kAXMainWindow slots (absent from
    /// kAXWindows). Those slots are not Space-filtered, so they are the only lead for an app
    /// whose windows all live on another Space (every background app under a native fullscreen
    /// Space) -- but they also hand over helper processes' overlays and child surfaces, so such
    /// entries are usable only once the batch is confirmed to be in that shape, and only when
    /// the window looks like a real one.
    only_via_key_or_main: bool,
}

// The short TTL only coalesces rapid summon/lifecycle refreshes; expiry still requests a fresh
// authoritative AX snapshot.
const AX_SNAPSHOT_CACHE_TTL: Duration = Duration::from_millis(750);
static AX_SNAPSHOT_CACHE: LazyLock<Mutex<HashMap<i32, CachedAxSnapshot>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// Per-element AX messaging timeout. `AXUIElementSetMessagingTimeout` applies per element and is
/// NOT inherited: with the timeout set only on the app element, the queries on the window elements
/// taken from AXWindows (AXTitle/AXRole/...) still use the system default of ~1.5s measured -- one
/// unresponsive app was enough to stall a whole collection pass by that much.
const AX_WINDOW_MESSAGING_TIMEOUT: f64 = 0.2;

/// The raise path runs its AX on the main thread, so its timeout is tighter than the collection
/// path: a timeout only loses the focus backstop (the SLPS fast raise already ran), whereas a
/// blocked main thread freezes the whole UI, including the next Cmd+Tab.
const AX_RAISE_MESSAGING_TIMEOUT: f64 = 0.25;

/// A window that was observed at a non-normal CG layer must keep that classification while the
/// same process instance and CGWindowID are alive. Some apps (notably Pixelmator-style helpers)
/// briefly report a floating form at layer 8 and later report the same window at layer 0.
///
/// PID alone is deliberately not part of the identity: macOS can recycle it. The process start
/// timestamp makes a new process incarnation unable to inherit the old window classification.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
struct WindowInstanceKey {
    pid: i32,
    process_start_time_us: u64,
    cgwid: u32,
}

static KNOWN_NON_NORMAL_WINDOWS: LazyLock<Mutex<HashSet<WindowInstanceKey>>> =
    LazyLock::new(|| Mutex::new(HashSet::new()));

// The public-framework CG/CF/AX externs now live in ffi.rs (this module keeps only the
// private APIs loaded via skylight.rs).

#[cfg(test)]
mod tests {
    use super::*;

    /// Smoke (GUI/ObjC runtime): verifies that objc2's exception boundary really contains an
    /// Objective-C exception. This is the crash backstop of the AX collection path: without it the
    /// exception terminates the process via `__rust_foreign_exception` (see the 2026-09-17 22:20
    /// crash report). Requires a GUI session, hence #[ignore]; run with
    /// `cargo test -- --ignored objc_exception_guard_contains_a_raise`.
    #[test]
    #[ignore]
    fn objc_exception_guard_contains_a_raise() {
        use objc2::runtime::AnyObject;
        use objc2::{class, msg_send};
        unsafe {
            let name = crate::ffi::make_nsstring("OhMyTabSmokeException");
            let reason = crate::ffi::make_nsstring("raised by the smoke test");
            let exception: *mut AnyObject = msg_send![
                class!(NSException),
                exceptionWithName: name,
                reason: reason,
                userInfo: std::ptr::null_mut::<AnyObject>()
            ];
            crate::ffi::CFRelease(name as *const c_void);
            crate::ffi::CFRelease(reason as *const c_void);
            assert!(!exception.is_null(), "NSException must be constructible");

            let caught = objc2::exception::catch(std::panic::AssertUnwindSafe(|| {
                let _: () = msg_send![exception, raise];
                "unreachable"
            }));
            let err =
                caught.expect_err("the raised exception must be caught, not abort the process");
            // The Debug output carries the NSException's name for log-based diagnosis (the
            // collection path logs the same way).
            let text = format!("{err:?}");
            assert!(
                text.contains("OhMyTabSmokeException"),
                "exception name must surface in the log text: {text}"
            );
        }
    }

    #[test]
    fn icon_miss_log_is_rate_limited_per_identity() {
        // One pass per (pid, key); distinct identities are independent; no identity -> never
        // limited.
        assert!(icon_miss_unlogged(910001, Some("test.icon.miss.a")));
        assert!(!icon_miss_unlogged(910001, Some("test.icon.miss.a")));
        assert!(icon_miss_unlogged(910002, Some("test.icon.miss.a")));
        assert!(icon_miss_unlogged(910001, Some("test.icon.miss.b")));
        assert!(icon_miss_unlogged(910003, None));
        assert!(icon_miss_unlogged(910003, None));
    }

    #[test]
    fn raise_generation_supersedes_older_jobs() {
        // Each committed switch bumps a new generation; older ones immediately go stale and
        // background jobs abort on that check, so a stale job can never re-raise an old window.
        let first = RAISE_GENERATION.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
        assert!(raise_intent_current(first));
        let second = RAISE_GENERATION.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
        assert!(!raise_intent_current(first));
        assert!(raise_intent_current(second));
    }

    #[test]
    fn ax_subrole_keep_rule_accepts_standard_and_titled_dialog() {
        use super::ax_subrole_kept;
        // Standard windows: any title.
        assert!(ax_subrole_kept(
            Some("AXStandardWindow"),
            Some("AXWindow"),
            false
        ));
        assert!(ax_subrole_kept(
            Some("AXStandardWindow"),
            Some("AXWindow"),
            true
        ));
        // AXDialog (JetBrains main windows): must be titled.
        assert!(ax_subrole_kept(Some("AXDialog"), Some("AXWindow"), true));
        assert!(!ax_subrole_kept(Some("AXDialog"), Some("AXWindow"), false));
        // Xcode reports ordinary windows as AXUnknown, but only with AXWindow role and title.
        assert!(ax_subrole_kept(Some("AXUnknown"), Some("AXWindow"), true));
        assert!(!ax_subrole_kept(Some("AXUnknown"), Some("AXWindow"), false));
        assert!(!ax_subrole_kept(Some("AXUnknown"), Some("AXButton"), true));
        // Popups/panels/invisible windows: always filtered.
        assert!(!ax_subrole_kept(Some("AXSheet"), Some("AXWindow"), true));
        assert!(!ax_subrole_kept(Some("AXDrawer"), Some("AXWindow"), true));
        // Missing subrole (some apps don't set it) -> standard.
        assert!(ax_subrole_kept(None, None, false));
    }

    #[test]
    fn ax_backfill_keeps_ordered_out_windows_but_rejects_overlay_layers() {
        use super::should_backfill_ax_window;

        // A CG entry missing means orderOut/off-screen; AX may legitimately restore it.
        assert!(should_backfill_ax_window(None));
        // Layer 0 is a normal window, even if it was skipped for another display filter.
        assert!(should_backfill_ax_window(Some(0)));
        // Non-zero layers are app-owned overlays/menus, not switcher targets.
        assert!(!should_backfill_ax_window(Some(101)));
    }

    #[test]
    fn non_normal_windows_need_main_or_fullscreen_semantics() {
        assert!(admissible_window_placement(0, false, false));
        assert!(admissible_window_placement(3, true, false));
        assert!(admissible_window_placement(101, false, true));
        assert!(!admissible_window_placement(3, false, false));
    }

    #[test]
    fn custom_roots_use_the_substantial_size_boundary() {
        assert!(custom_window_is_substantial((0.0, 0.0, 100.0, 50.0)));
        assert!(!custom_window_is_substantial((0.0, 0.0, 99.0, 50.0)));
        assert!(!custom_window_is_substantial((0.0, 0.0, 100.0, 49.0)));
    }

    #[test]
    fn attached_surfaces_are_not_independent_destinations() {
        assert!(!is_attached_surface(None));
        assert!(!is_attached_surface(Some(0)));
        assert!(is_attached_surface(Some(94)));
    }

    fn window(pid: i32, wid: u32) -> WindowInfo {
        WindowInfo {
            pid,
            window_id: wid,
            app_name: String::new(),
            window_title: String::new(),
            icon_path: None,
            is_active: false,
            minimized: false,
            app_hidden: false,
            bounds: (0.0, 0.0, 0.0, 0.0),
        }
    }

    #[test]
    fn candidate_window_elements_keep_published_order_and_append_key_and_main() {
        // The three slots fold into one candidate list: kAXWindows order first, then the focused
        // and main slots; a window repeated in a later slot keeps its first occurrence and is not
        // marked as coming only from the key/main slots.
        let published = [(10u32, 'a'), (11, 'b')];
        assert_eq!(
            candidate_window_elements(&published, Some((11, 'B')), Some((12, 'c'))),
            vec![(10, 'a', false), (11, 'b', false), (12, 'c', true)]
        );
    }

    #[test]
    fn candidate_window_elements_survive_an_empty_published_list() {
        // An empty array is not "no windows": kAXWindows answers empty when every window lives
        // on another Space while focused/main still hand the window over, so returning early on
        // empty would drop exactly the evidence that recovers it. Recovered entries carry a mark
        // so callers can use them only once degradation is confirmed.
        assert_eq!(
            candidate_window_elements(&[], Some((7, 'k')), None),
            vec![(7, 'k', true)]
        );
        // focused and main usually point at the same window, which must not become two cards.
        assert_eq!(
            candidate_window_elements(&[], Some((7, 'k')), Some((7, 'm'))),
            vec![(7, 'k', true)]
        );
    }

    #[test]
    fn candidate_window_elements_dedupe_by_window_id_then_by_element() {
        // The same window is a different object per slot: dedupe by window id so per-element attribute
        // queries are not repeated.
        let published = [(10u32, 'a'), (10, 'A'), (11, 'b')];
        assert_eq!(
            candidate_window_elements(&published, Some((10, 'x')), Some((11, 'y'))),
            vec![(10, 'a', false), (11, 'b', false)]
        );
        // Elements with no window id are deduped by the element itself.
        assert_eq!(
            candidate_window_elements(&[(0u32, 'z')], Some((0, 'z')), None),
            vec![(0, 'z', false)]
        );
        // The window is only empty when all three slots are empty.
        assert!(candidate_window_elements::<char>(&[], None, None).is_empty());
    }

    #[test]
    fn untitled_exemption_needs_windows_and_every_title_empty() {
        fn info(title: &str) -> AxWindowInfo {
            AxWindowInfo {
                cgwid: 1,
                title: title.to_string(),
                minimized: false,
                is_main: false,
                is_fullscreen: false,
                is_custom_root: false,
                only_via_key_or_main: false,
                tab_group: None,
            }
        }
        // All untitled -> exempt (apps that draw their own title bar).
        assert!(windows_are_all_untitled(&[info(""), info("")]));
        assert!(!windows_are_all_untitled(&[info(""), info("title")]));
        // Seeing no windows is not the same as untitled windows: an empty set gets no exemption.
        assert!(!windows_are_all_untitled(&[]));
    }

    #[test]
    fn ax_degradation_ignores_helper_processes_without_real_windows() {
        // Only apps that DO have a real-looking CG window count as degradation evidence: helper
        // processes (cursor overlays, bar surfaces) never had AX windows, and must not trip it.
        let empty: HashSet<i32> = [1, 2, 3, 4].into_iter().collect();
        let windows = [
            (1, 0, (0.0, 0.0, 900.0, 600.0)),   // real window
            (2, 0, (0.0, 0.0, 64.0, 64.0)),     // cursor overlay
            (3, 0, (0.0, 0.0, 1470.0, 33.0)),   // bar surface
            (4, 101, (0.0, 0.0, 900.0, 600.0)), // non-zero layer
            (9, 0, (0.0, 0.0, 900.0, 600.0)),   // app that answered
        ];
        assert_eq!(pids_with_real_window(windows, &empty), HashSet::from([1]));
    }

    #[test]
    fn ax_degradation_needs_several_empty_apps_without_a_windowed_majority() {
        // Everyday case: a few apps returning nothing (invisible anchor windows) is not degradation, so
        // "skip the whole app" still stands.
        assert!(!ax_batch_looks_degraded(1, 20));
        assert!(!ax_batch_looks_degraded(2, 2));
        // Native fullscreen Space: apps that do have CG windows return nothing in bulk.
        assert!(ax_batch_looks_degraded(20, 0));
        assert!(ax_batch_looks_degraded(3, 3));
        // Only a minority returned nothing -> not degradation.
        assert!(!ax_batch_looks_degraded(3, 10));
        assert!(!ax_batch_looks_degraded(0, 0));
    }

    #[test]
    fn choose_switchable_capture_window_prefers_focus_and_rejects_thin_windows() {
        let mut focused = window(10, 101);
        focused.bounds = (0.0, 0.0, 900.0, 600.0);
        let mut larger = window(10, 102);
        larger.bounds = (0.0, 0.0, 1200.0, 800.0);
        let mut tiny = window(10, 103);
        tiny.bounds = (0.0, 0.0, 80.0, 80.0);
        let mut minimized = window(10, 104);
        minimized.bounds = (0.0, 0.0, 1200.0, 800.0);
        minimized.minimized = true;
        let windows = vec![focused.clone(), larger, tiny.clone(), minimized];

        assert_eq!(
            choose_switchable_capture_window(&windows, Some(101), 999)
                .map(|window| window.window_id),
            Some(101)
        );
        assert_eq!(
            choose_switchable_capture_window(&windows, Some(999), 999)
                .map(|window| window.window_id),
            Some(102)
        );
        assert_eq!(choose_switchable_capture_window(&[tiny], None, 0), None);
        assert_eq!(
            choose_switchable_capture_window(&[focused], None, 101).map(|window| window.window_id),
            Some(101)
        );
    }

    #[test]
    fn fnv1a_hex_is_deterministic_and_stable() {
        // Same input -> same output; 16 hex chars, no '/' (filename-safe).
        let a = fnv1a64_hex("/Applications/Safari.app/Contents/MacOS/Safari");
        let b = fnv1a64_hex("/Applications/Safari.app/Contents/MacOS/Safari");
        assert_eq!(a, b);
        assert_eq!(a.len(), 16);
        assert!(a.chars().all(|c| c.is_ascii_hexdigit()));
        // Different paths should yield different keys (vanishingly low collision chance).
        assert_ne!(
            fnv1a64_hex("/Applications/Safari.app"),
            fnv1a64_hex("/Applications/Firefox.app")
        );
        assert_ne!(fnv1a64_hex(""), fnv1a64_hex("x"));
    }

    #[test]
    fn sort_windows_by_mru_orders_most_recent_first() {
        let now = Instant::now();
        let mut mru = MruMap::new();
        // Window (1,100) was active 5s ago and (2,200) 1s ago -> the latter comes first.
        mru.insert((1, 100), now - std::time::Duration::from_secs(5));
        mru.insert((2, 200), now - std::time::Duration::from_secs(1));
        let mut ws = vec![window(1, 100), window(2, 200)];
        sort_windows_by_mru(&mut ws, &mru, now);
        assert_eq!((ws[0].pid, ws[0].window_id), (2, 200));
        assert_eq!((ws[1].pid, ws[1].window_id), (1, 100));
    }

    #[test]
    fn sort_windows_by_mru_no_record_sorted_last() {
        // Windows without an MRU record fall back to 999s — after all recorded ones.
        let now = Instant::now();
        let mut mru = MruMap::new();
        mru.insert((1, 100), now - std::time::Duration::from_secs(60));
        let mut ws = vec![window(9, 999), window(1, 100)];
        sort_windows_by_mru(&mut ws, &mru, now);
        assert_eq!((ws[0].pid, ws[0].window_id), (1, 100));
        assert_eq!((ws[1].pid, ws[1].window_id), (9, 999));
    }

    #[test]
    fn newly_discovered_windows_preserve_activation_timeline() {
        // Edge starts on a new tab, then restores and focuses ChatGPT: the restored window's
        // explicit focus bump is newest, the new tab keeps Edge's activation seed, and apps
        // used earlier follow in order.
        let now = Instant::now();
        let ancient = now - Duration::from_secs(86_400);
        let edge_activated = now - Duration::from_secs(2);
        let mut mru = MruMap::new();
        mru.insert((20, 200), now - Duration::from_secs(10)); // RustRover
        mru.insert((30, 300), now - Duration::from_secs(20)); // Ghostty
        initialize_window_mru(&mut mru, 10, 101, Some(edge_activated), ancient, 0); // new tab
        initialize_window_mru(&mut mru, 10, 102, Some(edge_activated), ancient, 1); // ChatGPT
        mru.insert((10, 102), now); // restored focused window

        let mut ws = vec![
            window(30, 300),
            window(10, 101),
            window(20, 200),
            window(10, 102),
        ];
        sort_windows_by_mru(&mut ws, &mru, now);
        let order: Vec<(i32, u32)> = ws.iter().map(|w| (w.pid, w.window_id)).collect();
        assert_eq!(order, vec![(10, 102), (10, 101), (20, 200), (30, 300)]);
    }

    #[test]
    fn app_activation_does_not_bump_existing_sibling_window() {
        // `or_insert` does not overwrite an existing window: activating browser window A
        // leaves old sibling B at its own older MRU.
        let now = Instant::now();
        let old_sibling_mru = now - Duration::from_secs(60);
        let mut mru = MruMap::from([((10, 100), old_sibling_mru)]);
        initialize_window_mru(
            &mut mru,
            10,
            100,
            Some(now - Duration::from_secs(1)),
            now - Duration::from_secs(86_400),
            0,
        );
        assert_eq!(mru.get(&(10, 100)), Some(&old_sibling_mru));
    }

    #[test]
    fn new_window_without_activation_uses_ancient_fallback() {
        let now = Instant::now();
        let ancient = now - Duration::from_secs(86_400);
        let mut mru = MruMap::new();
        initialize_window_mru(&mut mru, 10, 100, None, ancient, 3);
        assert_eq!(
            mru.get(&(10, 100)),
            Some(&(ancient - Duration::from_millis(3)))
        );
    }

    #[test]
    fn frontmost_fallback_never_selects_another_app() {
        let mut own_front = window(10, 100);
        own_front.minimized = true;
        let ws = vec![window(20, 200), own_front];
        assert_eq!(frontmost_fallback(&ws, Some(10)), None);
        assert_eq!(frontmost_fallback(&ws, Some(20)), Some((20, 200)));
        assert_eq!(frontmost_fallback(&ws, None), None);
    }

    #[test]
    fn seed_groups_windows_by_app_in_system_front_to_back_order() {
        // Startup seed: same-app windows group together, apps follow the front-to-back
        // first-appearance order, per-app windows keep the CG z-order, and everything
        // sorts ahead of the 999s no-record fallback -- the core of "order matches the
        // native app order after restart".
        let now = Instant::now();
        // A jumbled CG window stream exposes the grouping: after app_order/app_windows,
        // the display order must be App1(100,200) / App2(300) / App3(400,500), with each
        // app's windows in CG appearance order.
        let app_order = vec![1, 2, 3];
        let app_windows: HashMap<i32, Vec<u32>> = [
            (1, vec![200, 100]), // CG order: 200 comes first (further forward)
            (2, vec![300]),
            (3, vec![400, 500]),
        ]
        .into_iter()
        .collect();
        let mru = seed_timestamps(&app_order, &app_windows, now);

        let mut ws = vec![
            window(3, 500),
            window(1, 100),
            window(2, 300),
            window(3, 400),
            window(1, 200),
            window(9, 999), // no-record fallback
        ];
        sort_windows_by_mru(&mut ws, &mru, now);
        let order: Vec<(i32, u32)> = ws.iter().map(|w| (w.pid, w.window_id)).collect();
        assert_eq!(
            order,
            vec![(1, 200), (1, 100), (2, 300), (3, 400), (3, 500), (9, 999)],
            "seeded order must group apps and keep the per-app CG order"
        );
    }

    #[test]
    fn prune_mru_drops_only_dead_entries() {
        let now = Instant::now();
        let mut mru = MruMap::new();
        mru.insert((1, 100), now);
        mru.insert((2, 200), now); // dead entry
        mru.insert((3, 300), now); // dead entry
        let live: HashSet<(i32, u32)> = [(1, 100), (4, 400)].into_iter().collect();
        let pruned = prune_mru(&mut mru, &live);
        assert_eq!(pruned, 2);
        assert!(mru.contains_key(&(1, 100)));
        assert_eq!(mru.len(), 1);
        // Nothing to drop -> 0 and the map is untouched.
        assert_eq!(prune_mru(&mut mru, &live), 0);
        assert_eq!(mru.len(), 1);
    }

    #[test]
    fn remove_pid_mru_drops_only_terminated_process() {
        let now = Instant::now();
        let mut mru = MruMap::from([((1, 100), now), ((1, 101), now), ((2, 200), now)]);
        assert_eq!(remove_pid_mru(&mut mru, 1), 2);
        assert_eq!(mru, MruMap::from([((2, 200), now)]));
        assert_eq!(remove_pid_mru(&mut mru, 1), 0);
    }

    #[test]
    fn bump_window_mru_ignores_zero_cgwid() {
        // cgwid == 0 (pairing failed) is not recorded.
        let mut mru = MruMap::new();
        bump_window_mru(&mut mru, 1, 0);
        assert!(mru.is_empty());
        bump_window_mru(&mut mru, 1, 42);
        assert!(mru.contains_key(&(1, 42)));
    }

    #[test]
    #[ignore]
    fn collect_windows_smoke() {
        // Skip (not fail) when Accessibility is not granted.
        if !crate::ffi::has_accessibility_permission() {
            eprintln!("[smoke] Accessibility not granted; skipping collect_windows");
            return;
        }
        let mut mru = MruMap::new();
        let wins = collect_windows(&mut mru);
        // With a GUI session we should see at least a few windows (usually >2).
        assert!(wins.len() >= 2, "expected >=2 windows, got {}", wins.len());
        // Invariant 1: (pid, window_id) globally unique.
        let mut seen: HashSet<(i32, u32)> = HashSet::new();
        for w in &wins {
            assert!(w.window_id != 0, "window_id must be nonzero");
            assert!(
                seen.insert((w.pid, w.window_id)),
                "duplicate window: pid={} wid={}",
                w.pid,
                w.window_id
            );
            assert!(!w.app_name.is_empty(), "app_name must not be empty");
        }
        // Invariant 2: the first window is marked active.
        assert!(wins[0].is_active);
        // Invariant 3: sorting relies on MRU — every display-list window must have an entry.
        // The converse (every MRU entry in the display list) does NOT hold: the frontmost
        // focused window is bumped via a separate system-API path and may be filtered out of
        // the display list by AX pairing, so a reverse assertion would flake spuriously.
        let all_have_mru = wins.iter().all(|w| mru.contains_key(&(w.pid, w.window_id)));
        assert!(
            all_have_mru,
            "some display windows lack MRU entries (sorting would fall back)"
        );
    }
}
