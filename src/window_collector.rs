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
    AXUIElementSetMessagingTimeout, AXValueGetType, AXValueGetTypeID, AXValueGetValue,
    CFArrayCreate, CFArrayGetCount, CFArrayGetTypeID, CFArrayGetValueAtIndex, CFBooleanGetValue,
    CFDictionaryGetValue, CFGetTypeID, CFNumberGetValue, CFRelease, CFRetain,
    CFStringCreateWithCString, CFStringGetCString, CGDisplayBounds, CGGetActiveDisplayList,
    CGWindowListCopyWindowInfo, K_AX_CANNOT_COMPLETE, K_AX_INVALID_UI_ELEMENT, K_AX_SUCCESS,
};
#[cfg(test)]
use crate::hash::fnv1a64_hex;
use crate::icon_cache::{check_cache_for_identity, extraction_known_missing};
use crate::skylight;
use crate::{log_debug, log_info};

mod collect;
mod raise;
mod raiser;
#[path = "window_collector/skylight.rs"]
mod space_membership;
mod space_state_probe;
mod window_state;
// The parent no longer calls collect directly; the glob is test-only.
#[cfg(test)]
use collect::*;
use raise::*;
use raiser::*;
pub(crate) use space_state_probe::{run as run_space_state_probe, ProbeMode};
pub(crate) use window_state::{ax_app_hidden_tag, StateSource, WindowStateEvidence};
// Entry points exposed to the rest of the crate (implemented in the child modules).
pub(crate) use collect::{
    collect_windows, collect_windows_for_pid, collect_windows_with_frontmost_bump,
    switchable_capture_window_for_pid,
};
pub(crate) use raise::{
    activate_pid, ax_window_cgwid, cf_string_new, clear_ax_window_cache_for_pid,
    clear_ax_window_cache_for_window, close_ax_window, focused_window_cgwid,
    forget_non_normal_window, raise_window_fast,
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
    // Native macOS fullscreen (AXFullScreen flag, a fullscreen-type Space, or display-filling
    // bounds). Presentation-only: drives the thumbnail's corner badge; raise and activation logic
    // never read it.
    pub fullscreen: bool,
    // The window lives on another macOS desktop (Space), admitted by the "show other desktops"
    // switch. Such a window usually has no AX element (the AX list is filtered by the current
    // Space), so its title is the CG window name; its physical state still comes from the
    // WindowServer row, which is why `state` travels with the card. Presentation-only, with two
    // consequences the raise and thumbnail paths do read: a capture is impossible off the current
    // desktop, and activation needs the Space switch that fronting the app performs.
    pub on_other_desktop: bool,
    // CG window bounds (x, y, w, h), used to locate the active window's screen. All zeros = unavailable.
    pub bounds: (f64, f64, f64, f64),
    // Per-field evidence for the three state flags above, published by `--e2e-state` so a scenario
    // can prove which plane decided a value. It carries the raw WindowServer row the card was
    // decoded from, so it is accepted and discarded with the collection result that produced it.
    pub(crate) state: WindowStateEvidence,
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
/// Ask about one specific window rather than listing every window.
const K_C_G_WINDOW_LIST_OPTION_INCLUDING_WINDOW: u32 = 1 << 3;

/// The WindowServer's current CGWindowLayer for one window, or `None` when it does not report that
/// window as on screen.
///
/// A collection pass reads the whole CG list *before* its AX phase, so a window created in between
/// is AX-visible and missing from that pass's list for exactly that pass. Asking about the window
/// itself answers the question that matters -- how the WindowServer classifies it -- instead of
/// reading "absent from the earlier list" as "orderOut'd window", which is how a floating
/// media-viewer window (layer 101) got admitted as a switcher card.
fn window_layer_now(cgwid: u32) -> Option<i32> {
    if cgwid == 0 {
        return None;
    }
    let array =
        unsafe { CGWindowListCopyWindowInfo(K_C_G_WINDOW_LIST_OPTION_INCLUDING_WINDOW, cgwid) };
    if array.is_null() {
        return None;
    }
    let layer = unsafe {
        if CFArrayGetCount(array) <= 0 {
            None
        } else {
            let dict = CFArrayGetValueAtIndex(array, 0);
            if dict.is_null() {
                None
            } else {
                cf_dict_get_i32(dict, "kCGWindowLayer")
            }
        }
    };
    unsafe { CFRelease(array) };
    layer
}

/// Whether WindowServer currently reports this window as on screen, i.e. it is a window of the
/// active desktop.
///
/// The cross-desktop raise uses this as its settle signal: a window on another desktop reports
/// false, and the flag flips exactly when fronting its app switches the Space (measured on
/// macOS 26). Reading it per window keeps the wait tied to the fact being waited for rather than
/// to a fixed sleep.
pub(crate) fn window_is_onscreen_now(cgwid: u32) -> bool {
    if cgwid == 0 {
        return false;
    }
    let array =
        unsafe { CGWindowListCopyWindowInfo(K_C_G_WINDOW_LIST_OPTION_INCLUDING_WINDOW, cgwid) };
    if array.is_null() {
        return false;
    }
    let onscreen = unsafe {
        if CFArrayGetCount(array) <= 0 {
            false
        } else {
            let dict = CFArrayGetValueAtIndex(array, 0);
            !dict.is_null() && cf_dict_get_bool(dict, "kCGWindowIsOnscreen") == Some(true)
        }
    };
    unsafe { CFRelease(array) };
    onscreen
}

/// The layer that classifies an AX-only window: this pass's snapshot entry when it has one, else a
/// fresh query for that window alone.
///
/// The snapshot entry is preferred because it costs nothing; the query runs only for the windows
/// the snapshot lacks, which is exactly the case its absence cannot decide.
fn ax_only_layer_for_backfill(
    snapshot_layer: Option<i32>,
    query: impl FnOnce() -> Option<i32>,
) -> Option<i32> {
    snapshot_layer.or_else(query)
}

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
    /// Positive AXSubrole/AXFullScreen evidence; false or unavailable still permits the CG bounds fallback.
    is_fullscreen: Option<bool>,
    is_custom_root: bool,
    /// AXSubrole == AXFloatingWindow. The subrole follows the presentation state (Telegram's media
    /// viewer is AXDialog while large and AXFloatingWindow while windowed), so it cannot decide on
    /// its own: `floating_window_is_admissible` demands WindowServer evidence as well.
    is_floating_window: bool,
    /// The native tab bar this window exposes (only the selected tab's window has one), used by
    /// collect::fold_tab_group to identify the group's windows.
    tab_group: Option<TabGroupInfo>,
    /// Whether this entry came ONLY from the kAXFocusedWindow/kAXMainWindow slots (absent from
    /// kAXWindows). Those slots can discover an app window even when its window list is empty;
    /// normal identity, size, activatable-process, and Space-membership filters decide whether it
    /// is published because the slots can also hand over helper overlays and child surfaces.
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

/// Windows AX has identified as one of an app's windows at least once (published by `kAXWindows`,
/// or recovered from the key/main slots).
///
/// The cross-desktop admission needs one piece of history: when an app's `kAXWindows` is
/// Space-filtered but its key/main slots still name a window, a CG window named by neither is a
/// secondary surface AX excludes -- 微信 keeps a 280x380 off-screen window titled 微信 beside its
/// real window, and admitting it produced a second, dead card. A window AX has *ever* identified is
/// not that case, so a real second window of the app (created on another desktop before this process
/// saw the app there) is still admitted once its desktop has been visited.
/// The AX-identity memory and its lifecycle, in one lock so a check, an insert and a cleanup are
/// atomic with respect to each other.
///
/// Validity is a lifecycle question, not a clock one: a destruction bumps `epoch`, every collection
/// records the epoch it began at, and an observation of a window or process incarnation destroyed
/// after that epoch is refused. A collection that began after the destruction -- including one that
/// sees a recycled CGWindowID or a new incarnation of a pid -- is unaffected. Marks are never
/// expired by time (they are a few bytes per destroyed window, bounded by one session), so a slow
/// collection can never outlive its veto.
#[derive(Default)]
pub(crate) struct AxIdentityMemory {
    known: HashSet<WindowInstanceKey>,
    /// CGWindowIDs destroyed, with the epoch at which each became invalid.
    destroyed_cgwids: HashMap<u32, u64>,
    /// Processes that ended: pid -> (incarnation that ended when known, epoch of the end). The veto
    /// is the epoch, not the incarnation: a termination callback can run before this pid's first AX
    /// query cached anything, and the cache holds the latest snapshot rather than the one that ended,
    /// so "the pid ended before this pass began" is the fact to act on.
    forgotten_processes: HashMap<i32, (Option<u64>, u64)>,
    epoch: u64,
    /// Identity passes currently open, counted per epoch: two passes can begin at the same epoch
    /// (no destruction between them), and each guard must unregister only its own. A destruction mark
    /// is only needed while a pass that began at or before it is still running, so closing the last
    /// such pass reclaims it.
    open_passes: std::collections::BTreeMap<u64, usize>,
}

static AX_IDENTITY: LazyLock<Mutex<AxIdentityMemory>> =
    LazyLock::new(|| Mutex::new(AxIdentityMemory::default()));

thread_local! {
    /// The epoch of the collection pass this thread is running (see `begin_ax_identity_pass`).
    static AX_IDENTITY_PASS_EPOCH: std::cell::Cell<Option<u64>> =
        const { std::cell::Cell::new(None) };
}

/// Reclaim destruction marks no open pass can still need: a mark only matters to a pass that began
/// at or before the destruction, so once every open pass is newer the marks can go.
fn prune_identity_marks(memory: &mut AxIdentityMemory) {
    match memory.open_passes.keys().next().copied() {
        Some(oldest_open) => {
            memory
                .destroyed_cgwids
                .retain(|_, epoch| *epoch > oldest_open);
            memory
                .forgotten_processes
                .retain(|_, (_, epoch)| *epoch > oldest_open);
        }
        None => {
            memory.destroyed_cgwids.clear();
            memory.forgotten_processes.clear();
        }
    }
}

/// Owns one identity pass: closing it (or dropping it) reclaims the marks that pass was the last
/// possible user of.
pub(crate) struct AxIdentityPassGuard(u64);

impl Drop for AxIdentityPassGuard {
    fn drop(&mut self) {
        AX_IDENTITY_PASS_EPOCH.with(|slot| slot.set(None));
        let mut memory = AX_IDENTITY.lock().unwrap();
        if let Some(count) = memory.open_passes.get_mut(&self.0) {
            *count -= 1;
            if *count == 0 {
                memory.open_passes.remove(&self.0);
            }
        }
        prune_identity_marks(&mut memory);
    }
}

/// Open an identity pass for this thread (collector entry points call this next to the scope pass).
pub(crate) fn begin_ax_identity_pass() -> AxIdentityPassGuard {
    let mut memory = AX_IDENTITY.lock().unwrap();
    let epoch = memory.epoch;
    *memory.open_passes.entry(epoch).or_insert(0) += 1;
    AX_IDENTITY_PASS_EPOCH.with(|slot| slot.set(Some(epoch)));
    AxIdentityPassGuard(epoch)
}

/// The identity pass the calling thread is in, or `None` when it never opened one.
fn ax_identity_pass_epoch() -> Option<u64> {
    AX_IDENTITY_PASS_EPOCH.with(|slot| slot.get())
}

/// Record that AX identified this window as one of the app's windows. Returns whether the record was
/// taken; a window or process incarnation invalidated since this pass began is refused inside the
/// same critical section that would insert it.
pub(crate) fn remember_ax_identity(
    pid: i32,
    process_start_time_us: Option<u64>,
    cgwid: u32,
) -> bool {
    if pid <= 0 || cgwid == 0 {
        return false;
    }
    let (Some(process_start_time_us), Some(pass_epoch)) =
        (process_start_time_us, ax_identity_pass_epoch())
    else {
        return false;
    };
    let key = WindowInstanceKey {
        pid,
        process_start_time_us,
        cgwid,
    };
    let mut memory = AX_IDENTITY.lock().unwrap();
    if memory
        .destroyed_cgwids
        .get(&cgwid)
        .is_some_and(|destroyed| *destroyed > pass_epoch)
    {
        return false;
    }
    if memory
        .forgotten_processes
        .get(&pid)
        .is_some_and(|(_, epoch)| *epoch > pass_epoch)
    {
        return false;
    }
    memory.known.retain(|entry| {
        entry.pid != key.pid || entry.process_start_time_us == key.process_start_time_us
    });
    memory.known.insert(key);
    true
}

pub(crate) fn is_ax_identified_window(
    pid: i32,
    process_start_time_us: Option<u64>,
    cgwid: u32,
) -> bool {
    if pid <= 0 || cgwid == 0 {
        return false;
    }
    let Some(process_start_time_us) = process_start_time_us else {
        return false;
    };
    let key = WindowInstanceKey {
        pid,
        process_start_time_us,
        cgwid,
    };
    AX_IDENTITY.lock().unwrap().known.contains(&key)
}

/// Whether AX has ever identified this window, ignoring the process incarnation (diagnostics only).
pub(crate) fn is_ax_identified_window_for_pid(pid: i32, cgwid: u32) -> bool {
    cgwid != 0
        && AX_IDENTITY
            .lock()
            .unwrap()
            .known
            .iter()
            .any(|entry| entry.pid == pid && entry.cgwid == cgwid)
}

pub(crate) fn forget_ax_identified_window(cgwid: u32) {
    if cgwid == 0 {
        return;
    }
    let mut memory = AX_IDENTITY.lock().unwrap();
    memory.epoch += 1;
    let epoch = memory.epoch;
    memory.destroyed_cgwids.insert(cgwid, epoch);
    memory.known.retain(|entry| entry.cgwid != cgwid);
    prune_identity_marks(&mut memory);
}

/// Forget every record of a process that ended. `process_start_time_us` is the incarnation that
/// ended: it must come from the identity cached while the process was alive, because re-resolving
/// the pid after termination can answer for a recycled pid instead.
pub(crate) fn forget_ax_identified_process(pid: i32, process_start_time_us: Option<u64>) {
    let mut memory = AX_IDENTITY.lock().unwrap();
    memory.epoch += 1;
    let epoch = memory.epoch;
    memory.known.retain(|entry| entry.pid != pid);
    memory
        .forgotten_processes
        .insert(pid, (process_start_time_us, epoch));
    // Reclaim inside the same critical section: with no older pass open the mark has no consumer.
    prune_identity_marks(&mut memory);
}

// The public-framework CG/CF/AX externs now live in ffi.rs (this module keeps only the
// private APIs loaded via skylight.rs).

#[cfg(test)]
mod ax_identity_tests {
    use super::*;
    use std::sync::Mutex as StdMutex;

    /// These tests share one process-wide memory (and its open-pass set), so they serialize.
    static TEST_LOCK: StdMutex<()> = StdMutex::new(());

    /// The identity protocol is what keeps a window AX refuses to name out of the cross-desktop
    /// candidate list; its orderings are asserted here because a single-window A2 cannot reach them.
    #[test]
    fn a_destroyed_window_is_refused_to_an_older_pass_and_allowed_to_a_newer_one() {
        let _guard = TEST_LOCK.lock().unwrap();
        let pid = 95_001;
        let start = Some(1_700_000_000_000_000u64);
        let cgwid = 95_101;

        let pass = begin_ax_identity_pass();
        assert!(remember_ax_identity(pid, start, cgwid));
        drop(pass);

        // The window dies while no pass is open.
        forget_ax_identified_window(cgwid);

        // A pass that began before the destruction (its epoch predates it) is refused...
        let stale_pass = begin_ax_identity_pass();
        // ...simulated by taking the epoch captured at open time and then destroying afterwards:
        let stale_epoch = AX_IDENTITY_PASS_EPOCH.with(|slot| slot.get());
        assert!(stale_epoch.is_some());
        forget_ax_identified_window(cgwid);
        assert!(
            !remember_ax_identity(pid, start, cgwid),
            "an observation from a pass that began before the destruction must be refused"
        );
        drop(stale_pass);

        // A pass that begins after it may record the recycled ID immediately.
        let fresh_pass = begin_ax_identity_pass();
        assert!(remember_ax_identity(pid, start, cgwid));
        drop(fresh_pass);
    }

    #[test]
    fn a_process_end_vetoes_observations_of_that_pid_even_without_a_cached_incarnation() {
        let _guard = TEST_LOCK.lock().unwrap();
        // The termination callback can run before this pid's first AX query cached anything, and the
        // cache holds the latest snapshot rather than the one that ended -- so the veto is the pid
        // plus the epoch, and the incarnation is only diagnostic.
        let pid = 95_002;
        let cgwid = 95_102;
        // A pass is already running when the process ends: its later observation must be refused.
        let stale = begin_ax_identity_pass();
        forget_ax_identified_process(pid, None);
        assert!(!remember_ax_identity(pid, Some(42), cgwid));
        drop(stale);

        // A pass that begins after the termination (a new incarnation) is unaffected.
        let fresh = begin_ax_identity_pass();
        assert!(remember_ax_identity(pid, Some(43), cgwid));
        drop(fresh);
    }

    #[test]
    fn destruction_marks_are_reclaimed_once_no_older_pass_is_open() {
        let _guard = TEST_LOCK.lock().unwrap();
        let pid = 95_003;
        let cgwid = 95_103;

        // A pass is running when the window and the process are invalidated: both marks must stay,
        // because that pass may still publish an observation taken before the invalidation.
        let older = begin_ax_identity_pass();
        forget_ax_identified_window(cgwid);
        forget_ax_identified_process(pid, None);
        {
            let marks = AX_IDENTITY.lock().unwrap();
            assert!(marks.destroyed_cgwids.contains_key(&cgwid));
            assert!(marks.forgotten_processes.contains_key(&pid));
        }

        // Closing the last pass that old reclaims them in the same step.
        drop(older);
        {
            let marks = AX_IDENTITY.lock().unwrap();
            assert!(marks.destroyed_cgwids.is_empty());
            assert!(marks.forgotten_processes.is_empty());
        }

        // With no pass open at all, a new invalidation is reclaimed immediately.
        forget_ax_identified_window(cgwid);
        forget_ax_identified_process(pid, None);
        let marks = AX_IDENTITY.lock().unwrap();
        assert!(marks.destroyed_cgwids.is_empty());
        assert!(marks.forgotten_processes.is_empty());
    }

    #[test]
    fn two_passes_at_the_same_epoch_each_hold_their_own_mark() {
        // No destruction between the two opens means both passes share an epoch: closing one must
        // not reclaim a mark the other still needs (a `BTreeSet` keyed by epoch would have collapsed
        // them into one entry). The veto itself is asserted by
        // `a_destroyed_window_is_refused_to_an_older_pass_and_allowed_to_a_newer_one`, which keeps
        // the pass epoch installed while it checks the refusal.
        let _guard = TEST_LOCK.lock().unwrap();
        let pid = 95_004;
        let cgwid = 95_104;
        let first = begin_ax_identity_pass();
        let second = begin_ax_identity_pass();
        forget_ax_identified_window(cgwid);
        forget_ax_identified_process(pid, None);

        drop(first);
        {
            let marks = AX_IDENTITY.lock().unwrap();
            assert!(
                marks.destroyed_cgwids.contains_key(&cgwid)
                    && marks.forgotten_processes.contains_key(&pid),
                "the second pass still needs both marks"
            );
        }
        drop(second);
        let marks = AX_IDENTITY.lock().unwrap();
        assert!(marks.destroyed_cgwids.is_empty());
        assert!(marks.forgotten_processes.is_empty());
    }
}

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
    fn ax_only_backfill_asks_the_window_only_when_the_snapshot_lacks_it() {
        // A snapshot entry is authoritative and free, so the query must not run.
        let mut queried = false;
        assert_eq!(
            ax_only_layer_for_backfill(Some(101), || {
                queried = true;
                Some(0)
            }),
            Some(101)
        );
        assert!(
            !queried,
            "a layer the pass snapshot already has must not cost a WindowServer query"
        );

        // The pass snapshot is read before the AX phase, so its absence is not evidence of
        // anything: the fresh query decides. A floating window (a media viewer at layer 101) that
        // the snapshot missed stays out of the switcher.
        assert_eq!(ax_only_layer_for_backfill(None, || Some(101)), Some(101));
        assert!(!should_backfill_ax_window(ax_only_layer_for_backfill(
            None,
            || Some(101)
        )));

        // Nothing reported for this window: the AX backfill keeps recovering orderOut'd windows.
        assert_eq!(ax_only_layer_for_backfill(None, || None), None);
        assert!(should_backfill_ax_window(ax_only_layer_for_backfill(
            None,
            || None
        )));
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
            Some("AXFullScreen"),
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
        // AXFloatingWindow: Telegram reports its windowed media viewer this way, and the subrole
        // flips with the presentation state, so the walk lets a titled one through and the pairing
        // stage demands WindowServer evidence instead.
        assert!(ax_subrole_kept(
            Some("AXFloatingWindow"),
            Some("AXWindow"),
            true
        ));
        assert!(!ax_subrole_kept(
            Some("AXFloatingWindow"),
            Some("AXWindow"),
            false
        ));
        assert!(!ax_subrole_kept(
            Some("AXFloatingWindow"),
            Some("AXButton"),
            true
        ));
        // Popups/panels/invisible windows: always filtered.
        assert!(!ax_subrole_kept(Some("AXSheet"), Some("AXWindow"), true));
        assert!(!ax_subrole_kept(Some("AXDrawer"), Some("AXWindow"), true));
        // Missing subrole (some apps don't set it) -> standard.
        assert!(ax_subrole_kept(None, None, false));
    }

    #[test]
    fn a_titled_floating_window_needs_the_ordinary_window_shape() {
        use super::floating_window_is_admissible;
        // Telegram's windowed media viewer: layer 0, 880x660 -- Mission Control lists it, so must we.
        assert!(floating_window_is_admissible(
            0,
            (369.0, 190.0, 880.0, 660.0)
        ));
        // A floating window above layer 0 (menu, HUD, palette) stays out.
        assert!(!floating_window_is_admissible(
            4,
            (369.0, 190.0, 880.0, 660.0)
        ));
        assert!(!floating_window_is_admissible(
            101,
            (0.0, 0.0, 1470.0, 956.0)
        ));
        // Too small to be a switch destination at layer 0 (a floating utility strip).
        assert!(!floating_window_is_admissible(0, (0.0, 0.0, 64.0, 33.0)));
        assert!(floating_window_is_admissible(0, (0.0, 0.0, 100.0, 50.0)));
        // The whole admission rule, as both pairing paths call it: the viewer again, then the same
        // shape as a floating palette (above layer 0) and as a tiny strip -- both rejected.
        use super::window_admission;
        assert!(window_admission(
            0,
            false,
            Some(false),
            true,
            (369.0, 190.0, 880.0, 660.0)
        ));
        assert!(!window_admission(
            4,
            false,
            Some(false),
            true,
            (369.0, 190.0, 880.0, 660.0)
        ));
        assert!(!window_admission(
            0,
            false,
            Some(false),
            true,
            (0.0, 0.0, 64.0, 33.0)
        ));
        // A non-floating window keeps the old layer rule: a non-zero layer needs main or fullscreen.
        assert!(window_admission(
            101,
            true,
            Some(false),
            false,
            (0.0, 0.0, 10.0, 10.0)
        ));
        assert!(!window_admission(
            101,
            false,
            Some(false),
            false,
            (0.0, 0.0, 10.0, 10.0)
        ));
    }

    #[test]
    fn the_floating_size_rule_survives_the_ax_only_backfill() {
        use super::{backfill_admission, window_admission};
        // A titled AXFloatingWindow whose WindowServer shape is a small floating strip: the pairing
        // stage rejects it...
        assert!(!window_admission(
            0,
            false,
            Some(false),
            true,
            (0.0, 0.0, 64.0, 33.0)
        ));
        // ...and the AX-only backfill, which publishes a window from AX alone with zero bounds and
        // used to check the layer only, must not restore it: same pass, same shape, same rejection.
        // (It is a regression guard: the backfill's `should_backfill_ax_window(Some(0))` is true,
        // which is what let a rejected floating window come back as a zero-bounds card.)
        assert!(!backfill_admission(
            Some(0),
            Some((0.0, 0.0, 64.0, 33.0)),
            false,
            false,
            false,
            true
        ));
        // A substantial floating window is restored, because this pass's snapshot described it.
        assert!(backfill_admission(
            Some(0),
            Some((369.0, 190.0, 880.0, 660.0)),
            false,
            false,
            false,
            true
        ));
        // No CG entry at all -> no shape evidence -> a floating window stays out.
        assert!(!backfill_admission(None, None, false, false, false, true));
        // Non-floating windows keep the backfill they had: a missing CG entry is still an
        // orderOut'd window AX may legitimately restore.
        assert!(backfill_admission(None, None, false, false, false, false));
        // The other backfill rules still hold: sticky drops, a non-main custom root drops, an
        // attached-surface-free substantial ordinary window at layer 0 comes back.
        assert!(!backfill_admission(
            Some(0),
            Some((0.0, 0.0, 880.0, 660.0)),
            true,
            false,
            false,
            false
        ));
        assert!(!backfill_admission(
            Some(0),
            Some((0.0, 0.0, 880.0, 660.0)),
            false,
            true,
            false,
            false
        ));
        assert!(backfill_admission(
            Some(0),
            Some((0.0, 0.0, 880.0, 660.0)),
            false,
            false,
            false,
            false
        ));
    }

    #[test]
    fn ax_fullscreen_detection_preserves_unknown_state_for_bounds_fallback() {
        use super::ax_fullscreen_from_attributes;
        assert_eq!(
            ax_fullscreen_from_attributes(Some("AXFullScreen"), None),
            Some(true)
        );
        assert_eq!(
            ax_fullscreen_from_attributes(Some("AXStandardWindow"), None),
            Some(false)
        );
        assert_eq!(ax_fullscreen_from_attributes(None, None), None);
        assert_eq!(
            ax_fullscreen_from_attributes(Some("AXStandardWindow"), Some(true)),
            Some(true)
        );
        assert_eq!(
            ax_fullscreen_from_attributes(Some("AXFullScreen"), Some(false)),
            Some(true)
        );
        assert_eq!(
            ax_fullscreen_from_attributes(Some("AXStandardWindow"), Some(false)),
            Some(false)
        );
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
            fullscreen: false,
            on_other_desktop: false,
            bounds: (0.0, 0.0, 0.0, 0.0),
            state: Default::default(),
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
        // empty would drop exactly the evidence that discovers it. Recovered entries carry a mark
        // so callers can apply the normal discovery and Space-membership filters before publish.
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
                is_fullscreen: Some(false),
                is_custom_root: false,
                is_floating_window: false,
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

        // A window on another desktop cannot be captured, so the prewarm target must skip it even
        // when it is the app's frontmost window -- otherwise the switch would spend capture work on
        // a frame the WindowServer cannot produce.
        let mut cross_desktop = window(10, 105);
        cross_desktop.bounds = (0.0, 0.0, 1200.0, 800.0);
        cross_desktop.on_other_desktop = true;
        assert_eq!(
            choose_switchable_capture_window(&[cross_desktop.clone()], Some(105), 105),
            None
        );
        let mut local = window(10, 106);
        local.bounds = (0.0, 0.0, 1200.0, 800.0);
        assert_eq!(
            choose_switchable_capture_window(&[cross_desktop, local], Some(105), 105)
                .map(|window| window.window_id),
            Some(106)
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
