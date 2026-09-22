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
// 父模块自身不再直接调用 collect(glob 仅测试模块需要)。
// The parent no longer calls collect directly; the glob is test-only.
#[cfg(test)]
use collect::*;
use raise::*;
use raiser::*;
// 对 crate 其他模块暴露的入口(内部子模块实现)。
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

/// 已打印过 `[collect] icon miss` 的 (pid, 图标缓存 key) 集合:同一身份每个进程只打一行
/// (理由见调用点注释)。条目数被「见过的 app 数」限制,不会无限增长。
/// The set of (pid, icon-cache key) pairs whose `[collect] icon miss` line was already
/// printed -- one line per identity per process (rationale at the call site). Bounded by the
/// number of distinct apps seen, so it cannot grow without limit.
static ICON_MISS_LOGGED: LazyLock<Mutex<HashSet<(i32, String)>>> =
    LazyLock::new(|| Mutex::new(HashSet::new()));

/// 首见该 (pid, key) 返回 true(限流放行);取不到身份时不做限流,保留可见性。
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
    pub window_id: u32, // CGWindowID，用于精确 raise（SLPS）/配对
    pub app_name: String,
    pub window_title: String,
    pub icon_path: Option<String>,
    pub is_active: bool,
    pub minimized: bool, // 最小化窗口(show_minimized 打开时才收集)/ minimized (collected only when show_minimized is on)
    // CG 窗口 bounds (x, y, w, h),用于确定激活窗口所在屏幕。全 0 表示未获取到。
    // CG window bounds (x, y, w, h), used to locate the active window's screen. All zeros = unavailable.
    pub bounds: (f64, f64, f64, f64),
}

/// 窗口级 MRU 时间戳，按 (pid, CGWindowID) 索引。
/// 每个窗口独立追踪最后被激活/选中的时间，不与其他窗口共享。
/// 排序时按 elapsed 升序——最近使用的窗口排在前面。
/// Window-level MRU timestamps, keyed by (pid, CGWindowID).
/// Each window is tracked independently — no app-level grouping.
/// Sorted by elapsed ascending — most recently used windows come first.
pub type MruMap = HashMap<(i32, u32), Instant>;

/// PID → 最后一次 App 激活时间（通过 NSWorkspace 通知）。
/// 仅作为新窗口第一次进入 MRU 表时的启动种子；已有窗口不会随 App 激活整体更新。
/// PID → last app activation time (via NSWorkspace notification).
/// Used only to seed a window the first time it enters the MRU map; existing windows
/// are never updated together when their app activates.
static LAST_ACTIVATED: std::sync::LazyLock<std::sync::Mutex<HashMap<i32, Instant>>> =
    std::sync::LazyLock::new(|| std::sync::Mutex::new(HashMap::new()));

/// 通过 NSWorkspaceDidActivateApplicationNotification 发送，由 main.rs 调用。
/// 返回本次激活 token，供异步焦点查询丢弃迟到结果。
/// Called from the NSWorkspaceDidActivateApplicationNotification handler in main.rs.
/// Returns this activation's token so async focus queries can discard stale results.
pub fn note_app_activated(pid: i32) -> Instant {
    let activated_at = Instant::now();
    LAST_ACTIVATED.lock().unwrap().insert(pid, activated_at);
    activated_at
}

/// 异步激活查询是否仍对应 PID 的最新一次激活。
/// Whether an async activation query still belongs to the PID's latest activation.
pub fn app_activation_is_current(pid: i32, activated_at: Instant) -> bool {
    LAST_ACTIVATED.lock().unwrap().get(&pid).copied() == Some(activated_at)
}

/// App 退出时清除激活种子，避免 PID 复用继承旧进程的时间。
/// Clear the activation seed on termination so PID reuse cannot inherit old process state.
pub fn note_app_terminated(pid: i32) {
    LAST_ACTIVATED.lock().unwrap().remove(&pid);
}

/// 将指定窗口的 MRU 时间戳更新为当前时间。
/// 由三个路径调用：
/// 1. oh-my-tab 内选中窗口（on_cmd_released / card_mouse_down / KEY_RETURN）
/// 2. NSWorkspace 激活通知 → on_app_activated 后台线程解析焦点窗口后回主线程调用
///    这是让系统 Cmd+Tab / Dock 点击等外部焦点切换也反映在窗口排序中的关键。
///
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

/// 纯窗口级 MRU 排序:按最后激活时间升序(最近使用的在前),无 MRU 记录的窗口
/// 回退到 999 秒(视为极旧,排最后)。纯函数,`now` 由调用方给定,测试可注入固定时间。
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

/// 枚举所有 layer 0 的 CG 窗口，专供 WindowServer 焦点监听使用。
/// 这不是显示列表:AX 仍决定哪些窗口最终展示，监听集合只需要保守覆盖 owner PID。
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

/// 获取当前 Space 中可见的普通窗口及其公开 bounds，供缩略图的 Show Desktop
/// 几何探针使用。这里不走 AX，避免在捕获 worker 中触碰主线程 UI 状态。
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

/// 从当前 CG 快照反查一个未订阅窗口的 owner PID，作为订阅索引失效时的兜底。
/// Resolve an unindexed window's owner PID from a fresh CG snapshot when the subscription index
/// cannot answer it.
pub(crate) fn owner_pid_for_cgwid(window_id: u32) -> Option<i32> {
    window_server_candidates()
        .into_iter()
        .find_map(|(candidate, pid)| (candidate == window_id).then_some(pid))
}

/// 修剪 MRU:删除不在存活窗口集里的条目(防 CGWindowID 复用继承旧时间戳),返回清理数。
/// Prune MRU entries not in the live window set (prevents CGWindowID-reuse inheriting a dead
/// timestamp); returns how many were dropped.
fn prune_mru(mru: &mut MruMap, live_set: &HashSet<(i32, u32)>) -> usize {
    let before = mru.len();
    mru.retain(|k, _| live_set.contains(k));
    before - mru.len()
}

/// 清除指定进程的全部窗口 MRU；由 App 退出通知调用，立即切断 PID/CGWindowID 复用污染。
/// Remove every window MRU for a process; termination calls this to immediately prevent
/// PID/CGWindowID reuse from inheriting stale timestamps.
pub fn remove_pid_mru(mru: &mut MruMap, pid: i32) -> usize {
    let before = mru.len();
    mru.retain(|(entry_pid, _), _| *entry_pid != pid);
    before - mru.len()
}

/// 新窗口只初始化一次：优先使用 App 最近激活时间，否则退回 ancient；顺序偏移只用于
/// 同批首次发现窗口的稳定排序。`or_insert` 是纯窗口 MRU 不搭便车的关键。
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

/// AX 聚焦查询失败时只能在系统报告的前台 App 内回退，不能误提升全局 CG 列表首项。
/// If the AX focused query fails, fallback is restricted to the system-reported frontmost
/// app rather than accidentally bumping the first item in the global CG list.
fn frontmost_fallback(windows: &[WindowInfo], front_pid: Option<i32>) -> Option<(i32, u32)> {
    let pid = front_pid?;
    windows
        .iter()
        .find(|w| w.pid == pid && !w.minimized)
        .map(|w| (w.pid, w.window_id))
}

/// 启动时按窗口前→后顺序预种 MRU(同应用窗口分组,应用级顺序 = CG 前→后序)。
///
/// **重要(load-bearing):这个顺序只是一个启动占位,未必等于原生 Cmd+Tab 的顺序。**
/// macOS 没有公开 API 返回应用切换器的 App MRU,而 CG 的 z 序记录的是"窗口创建/点击
/// 抬升"的历史,与应用激活顺序是两回事——实测应用激活(包括 Cmd+Tab/Dock 切换)不会
/// 重排 z 序(Ghostty/Edge 激活后窗口仍在 z 序深处)。因此这里排出的顺序只是"我们
/// 随便定的一个初始顺序",保证:① 同 App 窗口聚在一起;② 顺序稳定可复现;③ 全部
/// 排在 999s 回退值之前。真正精确的顺序在应用运行起来后由实时激活通知(MRU bump)
/// 逐步修正。如果以后要精确恢复重启前的顺序,需要把 MRU 持久化到磁盘(见
/// clipboard-history 的持久化模式),而不是依赖这个种子。
///
/// 该方法在启动时调用一次(见 AppState::new),返回带种子的 MruMap。
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
        // App 首次出现顺序(前到后)+ 每个 App 内的窗口 CG 顺序。
        // **只用可见(on-screen)窗口推导**:不可见/最小化/其他 Space 的窗口会穿插在
        // z 序里,把应用相对顺序搅乱(实测 Clash Verge 因离屏窗口靠前而虚高)。
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
        // 打印启动时的占位顺序(前→后,应用分组)。注意这只是近似占位,不是原生
        // Cmd+Tab 的应用序(激活不重排 z 序,见函数注释)——仅用于核对与排查。
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

/// 纯逻辑:按显示顺序(应用分组 + 应用内 CG 序)给每个窗口赋一个单调递增的"年龄",
/// 让 sort_windows_by_mru(按年龄升序)排出该顺序;全部 < 1s,仍排在 999s 回退前。
/// 纯函数,单测覆盖排序结果。`now` 由调用方给定,测试注入固定时刻。
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

// 枚举常量:All(0)= 含离屏窗口(orderOut 对话框/最小化/其他 Space),收集恒用它——
// 离屏窗口是否显示由 AX 语义决定(见 collect_windows 的过滤注释)。
// Enumeration constants: All (0) includes off-screen windows (orderOut'd dialogs /
// minimized / other Spaces); collection always uses it -- whether an off-screen window
// shows is decided by AX semantics (see collect_windows' filter comments).
const K_C_G_WINDOW_LIST_OPTION_ALL: u32 = 0;

// AX types

/// 长期保存一个 AX 窗口元素时必须自己持有 CF 引用;裸指针不能直接跨线程放进静态缓存。
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

/// `(process instance, cgwid) -> AXUIElement` 缓存。普通切换直接复用元素,只有 stale
/// element 才重新枚举;不使用 PID 单独作为身份,避免 PID 复用拿到旧 AX 对象。
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
    /// 该条目是否**只**由 kAXFocusedWindow/kAXMainWindow 槽位交回(不在 kAXWindows 里)。
    /// 这两个槽位不做 Space 过滤,所以窗口全在别的 Space(原生全屏 Space 下的后台 App)
    /// 时它们是唯一的线索;但它们同样会交回辅助进程的浮层/子窗口,因此这类条目只在确认
    /// “AX 看不到当前 Space 之外”时才可用,且要过“像真窗口”的尺寸门。
    /// Whether this entry came ONLY from the kAXFocusedWindow/kAXMainWindow slots (absent from
    /// kAXWindows). Those slots are not Space-filtered, so they are the only lead for an app
    /// whose windows all live on another Space (every background app under a native fullscreen
    /// Space) -- but they also hand over helper processes' overlays and child surfaces, so such
    /// entries are usable only once the batch is confirmed to be in that shape, and only when
    /// the window looks like a real one.
    only_via_key_or_main: bool,
}

// 短 TTL 只用于合并快速连续的召唤/生命周期刷新;过期后仍会重新向 AX 请求权威快照。
// The short TTL only coalesces rapid summon/lifecycle refreshes; expiry still requests a fresh
// authoritative AX snapshot.
const AX_SNAPSHOT_CACHE_TTL: Duration = Duration::from_millis(750);
static AX_SNAPSHOT_CACHE: LazyLock<Mutex<HashMap<i32, CachedAxSnapshot>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// 每个 AX 窗口元素的消息超时。`AXUIElementSetMessagingTimeout` 是按元素生效的,而且**不会**
/// 被窗口元素继承:只在 app 元素上设超时时,从 AXWindows 取出的窗口元素查询(AXTitle/AXRole 等)
/// 仍走系统默认值,实测约 1.5s——一个无响应 App 就能让整轮采集卡 1.5s。
/// Per-element AX messaging timeout. `AXUIElementSetMessagingTimeout` applies per element and is
/// NOT inherited: with the timeout set only on the app element, the queries on the window elements
/// taken from AXWindows (AXTitle/AXRole/...) still use the system default of ~1.5s measured -- one
/// unresponsive app was enough to stall a whole collection pass by that much.
const AX_WINDOW_MESSAGING_TIMEOUT: f64 = 0.2;

/// 抬窗路径的 AX 在主线程上执行,超时必须比采集路径更紧:超时只损失焦点兜底,已有的 SLPS
/// 快速抬窗不受影响,而主线程被占住会让整个界面(含下一次 Cmd+Tab)都停摆。
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

// CG / CF / AX 的公开框架 extern 已统一到 ffi.rs(本模块只保留经 skylight.rs 的私有 API)。
// The public-framework CG/CF/AX externs now live in ffi.rs (this module keeps only the
// private APIs loaded via skylight.rs).

#[cfg(test)]
mod tests {
    use super::*;

    /// 冒烟(GUI/ObjC 运行时):确认 objc2 的异常边界真的能接住 ObjC 异常。
    /// 这条机制是 AX 采集路径的崩溃兜底:没有它,异常会以 `__rust_foreign_exception` 终止进程
    /// (见 2026-09-17 22:20 的崩溃报告)。需要图形会话,故标 ignore;
    /// 跑法:`cargo test -- --ignored objc_exception_guard_contains_a_raise`。
    ///
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
            // Debug 里带 NSException 的 name,便于日志定位(采集路径同样打印它)。
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
        // 同一 (pid, key) 只放行一次;不同身份互不影响;取不到身份时不做限流。
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
        // 提交一次切换 = bump 出一个新代号;更早的代号立即失效,后台任务据此中止,
        // 保证快速连续切换时旧任务不会把旧窗口抬回来。
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
        // 标准窗口:任意标题。/ Standard windows: any title.
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
        // AXDialog(JetBrains 主窗口):必须带非空标题。
        // AXDialog (JetBrains main windows): must be titled.
        assert!(ax_subrole_kept(Some("AXDialog"), Some("AXWindow"), true));
        assert!(!ax_subrole_kept(Some("AXDialog"), Some("AXWindow"), false));
        // Xcode reports ordinary windows as AXUnknown, but only with AXWindow role and title.
        assert!(ax_subrole_kept(Some("AXUnknown"), Some("AXWindow"), true));
        assert!(!ax_subrole_kept(Some("AXUnknown"), Some("AXWindow"), false));
        assert!(!ax_subrole_kept(Some("AXUnknown"), Some("AXButton"), true));
        // 弹窗/面板/隐形窗口:一律过滤。
        // Popups/panels/invisible windows: always filtered.
        assert!(!ax_subrole_kept(Some("AXSheet"), Some("AXWindow"), true));
        assert!(!ax_subrole_kept(Some("AXDrawer"), Some("AXWindow"), true));
        // 无 subrole(部分 App 不设置)→ 视为标准窗口。
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
    fn custom_roots_use_alt_tab_substantial_boundary() {
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
            bounds: (0.0, 0.0, 0.0, 0.0),
        }
    }

    #[test]
    fn candidate_window_elements_keep_published_order_and_append_key_and_main() {
        // 三个属性槽位合成候选列表:kAXWindows 的顺序在前,后面补 focused / main;
        // 同一个窗口在后面的槽位里再出现时只保留首次出现(前面的顺序优先),并且不被
        // 标成“只来自 key/main 槽位”。
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
        // 空数组不是“没有窗口”:窗口全在别的 Space 时 kAXWindows 返空,而 focused/main
        // 仍会交回那个窗口——在这里提前返回,恰好会丢掉唯一能救回它的信息。恢复出来的
        // 窗口带上标记,好让调用方只在确认退化时才用它们。
        // An empty array is not "no windows": kAXWindows answers empty when every window lives
        // on another Space while focused/main still hand the window over, so returning early on
        // empty would drop exactly the evidence that recovers it. Recovered entries carry a mark
        // so callers can use them only once degradation is confirmed.
        assert_eq!(
            candidate_window_elements(&[], Some((7, 'k')), None),
            vec![(7, 'k', true)]
        );
        // focused/main 通常指向同一个窗口,不能因此变成两张。
        assert_eq!(
            candidate_window_elements(&[], Some((7, 'k')), Some((7, 'm'))),
            vec![(7, 'k', true)]
        );
    }

    #[test]
    fn candidate_window_elements_dedupe_by_window_id_then_by_element() {
        // 同一个窗口在槽位间是不同对象:按窗口 id 去重,避免重复的逐元素属性查询。
        let published = [(10u32, 'a'), (10, 'A'), (11, 'b')];
        assert_eq!(
            candidate_window_elements(&published, Some((10, 'x')), Some((11, 'y'))),
            vec![(10, 'a', false), (11, 'b', false)]
        );
        // 取不到窗口 id 的元素按元素本身去重。
        assert_eq!(
            candidate_window_elements(&[(0u32, 'z')], Some((0, 'z')), None),
            vec![(0, 'z', false)]
        );
        // 三个槽位都没有内容才是真的空。
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
            }
        }
        // 全部无标题 → 豁免(自绘标题栏的 App)。
        assert!(windows_are_all_untitled(&[info(""), info("")]));
        assert!(!windows_are_all_untitled(&[info(""), info("title")]));
        // 没看到任何窗口 ≠ 无标题窗口:空集合不能拿豁免。
        assert!(!windows_are_all_untitled(&[]));
    }

    #[test]
    fn ax_degradation_ignores_helper_processes_without_real_windows() {
        // 只有“CG 里确实有像真窗口”的 App 才算退化证据:辅助进程(光标浮层/菜单条)本来
        // 就没有 AX 窗口,不能因为它们把退化判定撑爆。
        // Only apps that DO have a real-looking CG window count as degradation evidence: helper
        // processes (cursor overlays, bar surfaces) never had AX windows, and must not trip it.
        let empty: HashSet<i32> = [1, 2, 3, 4].into_iter().collect();
        let windows = [
            (1, 0, (0.0, 0.0, 900.0, 600.0)),   // 真窗口 → 计入 / real window
            (2, 0, (0.0, 0.0, 64.0, 64.0)),     // 光标浮层 → 不计 / cursor overlay
            (3, 0, (0.0, 0.0, 1470.0, 33.0)),   // 菜单条 → 不计 / bar surface
            (4, 101, (0.0, 0.0, 900.0, 600.0)), // 非 0 层 → 不计 / non-zero layer
            (9, 0, (0.0, 0.0, 900.0, 600.0)),   // AX 非空 → 不计 / app that answered
        ];
        assert_eq!(pids_with_real_window(windows, &empty), HashSet::from([1]));
    }

    #[test]
    fn ax_degradation_needs_several_empty_apps_without_a_windowed_majority() {
        // 日常:个别 App 返空(隐形锚点窗口那种)不算退化,仍维持“整 App 跳过”。
        assert!(!ax_batch_looks_degraded(1, 20));
        assert!(!ax_batch_looks_degraded(2, 2));
        // 原生全屏 Space:有 CG 窗口的 App 成批返空。
        assert!(ax_batch_looks_degraded(20, 0));
        assert!(ax_batch_looks_degraded(3, 3));
        // 返空的是少数 → 不是退化。
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
        // 同一输入恒定;输出为 16 位十六进制,不含 `/`(文件名安全)。
        // Same input -> same output; 16 hex chars, no '/' (filename-safe).
        let a = fnv1a64_hex("/Applications/Safari.app/Contents/MacOS/Safari");
        let b = fnv1a64_hex("/Applications/Safari.app/Contents/MacOS/Safari");
        assert_eq!(a, b);
        assert_eq!(a.len(), 16);
        assert!(a.chars().all(|c| c.is_ascii_hexdigit()));
        // 不同路径应产生不同键(极低碰撞概率)。
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
        // 窗口 (1,100) 5 秒前激活,(2,200) 1 秒前激活 -> 后者在前。
        mru.insert((1, 100), now - std::time::Duration::from_secs(5));
        mru.insert((2, 200), now - std::time::Duration::from_secs(1));
        let mut ws = vec![window(1, 100), window(2, 200)];
        sort_windows_by_mru(&mut ws, &mru, now);
        assert_eq!((ws[0].pid, ws[0].window_id), (2, 200));
        assert_eq!((ws[1].pid, ws[1].window_id), (1, 100));
    }

    #[test]
    fn sort_windows_by_mru_no_record_sorted_last() {
        // 无 MRU 记录的窗口回退 999 秒,排在所有有记录的窗口之后。
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
        // Edge 启动时先使用新标签页，随后恢复并聚焦 ChatGPT：恢复窗口的显式焦点 bump
        // 最新，新标签页保留 Edge 激活种子，之前使用的其他 App 依次排后。
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
        // `or_insert` 不覆盖已有窗口：激活浏览器窗口 A 时，旧窗口 B 保留自己的旧 MRU。
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
        // 启动种子:同 App 窗口分组、App 顺序 = 前到后首次出现、应用内 = CG 序,
        // 且全部排在无记录(999s 回退)之前。这是"重启后顺序与原生应用序一致"的核心。
        // Startup seed: same-app windows group together, apps follow the front-to-back
        // first-appearance order, per-app windows keep the CG z-order, and everything
        // sorts ahead of the 999s no-record fallback -- the core of "order matches the
        // native app order after restart".
        let now = Instant::now();
        // 呈现为乱序的 CG 前→后窗口流,经 app_order/app_windows 分组后应为:
        // App1(100,200) / App2(300) / App3(400,500) —— 应用内窗口按 CG 出现序。
        // A jumbled CG window stream exposes the grouping: after app_order/app_windows,
        // the display order must be App1(100,200) / App2(300) / App3(400,500), with each
        // app's windows in CG appearance order.
        let app_order = vec![1, 2, 3];
        let app_windows: HashMap<i32, Vec<u32>> = [
            (1, vec![200, 100]), // CG 序:200 在前(更靠前)
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
            window(9, 999), // 无记录回退 / no-record fallback
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
        mru.insert((2, 200), now); // 死条目 / dead entry
        mru.insert((3, 300), now); // 死条目 / dead entry
        let live: HashSet<(i32, u32)> = [(1, 100), (4, 400)].into_iter().collect();
        let pruned = prune_mru(&mut mru, &live);
        assert_eq!(pruned, 2);
        assert!(mru.contains_key(&(1, 100)));
        assert_eq!(mru.len(), 1);
        // 无死条目时返回 0 且不动 map。
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
        // cgwid == 0(未配对成功)不写入 MRU。
        // cgwid == 0 (pairing failed) is not recorded.
        let mut mru = MruMap::new();
        bump_window_mru(&mut mru, 1, 0);
        assert!(mru.is_empty());
        bump_window_mru(&mut mru, 1, 42);
        assert!(mru.contains_key(&(1, 42)));
    }

    // ========== 冒烟测试(需要真实 GUI 会话 + 辅助功能权限,手动运行)==========
    // ========== Smoke tests (need a real GUI session + Accessibility grant; run manually) ==========
    // 运行:cargo test -- --ignored
    // 这些测试真实调用 CG/AX 栈,CI 上无 GUI 会话,默认跳过。

    #[test]
    #[ignore]
    fn collect_windows_smoke() {
        // 无辅助功能权限时直接跳过(不是失败)。
        // Skip (not fail) when Accessibility is not granted.
        if !crate::ffi::has_accessibility_permission() {
            eprintln!("[smoke] Accessibility not granted; skipping collect_windows");
            return;
        }
        let mut mru = MruMap::new();
        let wins = collect_windows(&mut mru);
        // 有 GUI 会话时至少应能看到若干窗口(通常 >2)。
        // With a GUI session we should see at least a few windows (usually >2).
        assert!(wins.len() >= 2, "expected >=2 windows, got {}", wins.len());
        // 不变式 1:(pid, window_id) 全局唯一。
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
        // 不变式 2:第一个窗口被标记为激活。
        // Invariant 2: the first window is marked active.
        assert!(wins[0].is_active);
        // 不变式 3:排序依赖 MRU —— 显示列表里每个窗口都应有 MRU 条目。
        // 反向(每个 MRU 条目都在显示列表)不成立:frontmost 聚焦窗口是单独经系统 API
        // bump 的,可能被 AX 配对过滤而不在显示列表,反向断言会偶发误报。
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
