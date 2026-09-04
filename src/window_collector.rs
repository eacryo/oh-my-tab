use objc2::runtime::AnyObject;
use objc2::{class, msg_send, sel};
use std::collections::{HashMap, HashSet};
use std::ffi::{c_char, c_void, CStr};
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant};

use crate::config::CONFIG;
use crate::ffi::{
    kCFBooleanFalse, AXError, AXUIElementCopyAttributeValue, AXUIElementCreateApplication,
    AXUIElementPerformAction, AXUIElementRef, AXUIElementSetAttributeValue,
    AXUIElementSetMessagingTimeout, CFArrayGetCount, CFArrayGetValueAtIndex, CFBooleanGetValue,
    CFDictionaryGetValue, CFNumberGetValue, CFRelease, CFRetain, CFStringCreateWithCString,
    CFStringGetCString, CGWindowListCopyWindowInfo, K_AX_INVALID_UI_ELEMENT, K_AX_SUCCESS,
};
use crate::hash::fnv1a64_hex;
use crate::icon_cache::check_cache_for_identity;
use crate::skylight;
use crate::{log_debug, log_info};

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
/// 仅作为新窗口第一次进入 MRU 表时的启动种子；已有窗口永不随 App 激活整体更新。
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
    windows: Vec<(u32, String, bool)>,
}

// 短 TTL 只用于合并快速连续的召唤/生命周期刷新;过期后仍会重新向 AX 请求权威快照。
// The short TTL only coalesces rapid summon/lifecycle refreshes; expiry still requests a fresh
// authoritative AX snapshot.
const AX_SNAPSHOT_CACHE_TTL: Duration = Duration::from_millis(750);
static AX_SNAPSHOT_CACHE: LazyLock<Mutex<HashMap<i32, CachedAxSnapshot>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

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

// ========== 私有 API（统一经 skylight.rs 的 dlopen/dlsym 加载）==========
// Private APIs (loaded via skylight.rs's shared dlopen/dlsym helpers).
// 用来按 CGWindowID 精确配对 AX/CG 窗口、并在 WindowServer 层只抬起一个窗口。

#[repr(C)]
#[derive(Clone, Copy)]
struct ProcessSerialNumber {
    high_long_of_psn: u32,
    low_long_of_psn: u32,
}

type AxGetWindowFn = unsafe extern "C" fn(AXUIElementRef, *mut u32) -> AXError;
type GetProcessForPIDFn = unsafe extern "C" fn(i32, *mut ProcessSerialNumber) -> i32;
type SlpSetFrontFn = unsafe extern "C" fn(*mut ProcessSerialNumber, u32, i32) -> i32;
type SlpsPostEventRecordFn = unsafe extern "C" fn(*mut ProcessSerialNumber, *mut u8) -> i32;

static AX_GET_WINDOW: std::sync::LazyLock<Option<AxGetWindowFn>> =
    std::sync::LazyLock::new(|| unsafe {
        // _AXUIElementGetWindow 是 HIServices 里的私有符号，dlopen 后 dlsym。
        skylight::load_private_symbol(skylight::HISERVICES_PATH, "_AXUIElementGetWindow")
    });
static GET_PROCESS_FOR_PID: std::sync::LazyLock<Option<GetProcessForPIDFn>> =
    std::sync::LazyLock::new(|| unsafe {
        // GetProcessForPID 也在 HIServices，dlopen 后 dlsym。
        skylight::load_private_symbol(skylight::HISERVICES_PATH, "GetProcessForPID")
    });
static SLP_SET_FRONT: std::sync::LazyLock<Option<SlpSetFrontFn>> =
    std::sync::LazyLock::new(|| unsafe {
        // SkyLight 是私有框架，必须先 dlopen 才能查到符号。
        skylight::load_private_symbol(skylight::SKYLIGHT_PATH, "_SLPSSetFrontProcessWithOptions")
    });
static SLPS_POST_EVENT_RECORD: std::sync::LazyLock<Option<SlpsPostEventRecordFn>> =
    std::sync::LazyLock::new(|| unsafe {
        // make_key_window 的合成鼠标事件也走 SkyLight 的 SLPSPostEventRecordTo。
        // Synthetic mouse events for make_key_window go through SkyLight's
        // SLPSPostEventRecordTo as well.
        skylight::load_private_symbol(skylight::SKYLIGHT_PATH, "SLPSPostEventRecordTo")
    });

/// 取一个 AX 窗口的 CGWindowID（私有 API _AXUIElementGetWindow）。
/// 用它把 AX 窗口和 CG 窗口按 CGWindowID 精确配对，不再靠顺序/标题猜
/// （Edge 等 CG 无窗口名的 App 之前会配错，导致 mru/raise 跟错窗口）。
/// 缩略图模块的 AXObserver 回调也用它解析新创建窗口的 cgwid。
/// Get a CGWindowID for an AX window (private API _AXUIElementGetWindow).
/// Used to pair AX windows with CG windows by CGWindowID instead of guessing
/// by order/title (apps like Edge with no CG window name used to mismatch,
/// corrupting mru/raise targeting). The thumbnail module's AXObserver callback
/// also uses it to resolve newly created windows' cgwids.
pub(crate) unsafe fn ax_window_cgwid(element: AXUIElementRef) -> Option<u32> {
    let f = (*AX_GET_WINDOW)?;
    let mut wid: u32 = 0;
    if f(element, &mut wid) == K_AX_SUCCESS && wid != 0 {
        Some(wid)
    } else {
        None
    }
}

/// 查某 App 当前聚焦窗口的 CGWindowID（kAXFocusedWindow -> _AXUIElementGetWindow）。
/// 比 AX[0] 可靠：AX 窗口数组顺序不一定是最前在前，但 kAXFocusedWindow 是明确聚焦的窗口。
/// Get the CGWindowID of an app's currently focused window (kAXFocusedWindow ->
/// _AXUIElementGetWindow). More reliable than AX[0]: the AX window array order
/// isn't always frontmost-first, but kAXFocusedWindow is the explicitly focused window.
pub(crate) unsafe fn focused_window_cgwid(pid: i32) -> Option<u32> {
    let app = AXUIElementCreateApplication(pid);
    if app.is_null() {
        return None;
    }
    AXUIElementSetMessagingTimeout(app, 0.05); // 50ms 超时，避免卡死在无响应的 App 上。
    let focused_key = cf_string_new("AXFocusedWindow");
    let mut focused: *const c_void = std::ptr::null();
    let err = AXUIElementCopyAttributeValue(app, focused_key, &mut focused);
    CFRelease(focused_key);
    CFRelease(app);
    if err != K_AX_SUCCESS || focused.is_null() {
        return None;
    }
    let wid = ax_window_cgwid(focused);
    CFRelease(focused);
    wid
}

/// 用 SkyLight 私有 API _SLPSSetFrontProcessWithOptions 在 WindowServer 层
/// 只抬起指定 CGWindowID 的那一个窗口（不抬该 App 的所有窗口）。
/// Raise only one window (by CGWindowID) at the WindowServer level via the
/// SkyLight private API _SLPSSetFrontProcessWithOptions -- does NOT raise all
/// of the app's windows the way activate(AllWindows) does.
///
/// mode 用 0x200(kCPSUserGenerated,yabai kCPS* 定义,AltTab 同款):把这次切换标记为
/// 「用户发起」。macOS 14+ 对非用户发起的程序化前台切换会抑制输入焦点转移(目标窗口
/// 变灰红绿灯,需要点击才能获得焦点),0x200 是绕过该抑制的关键。旧代码传的 2 不是
/// 有效标志位,正是灰红绿灯的根因之一。
/// The mode is 0x200 (kCPSUserGenerated, from yabai's kCPS* constants, same as AltTab):
/// it marks this front-switch as user-initiated. macOS 14+ suppresses input-focus transfer
/// for non-user-initiated programmatic front-switches (the target window's traffic lights go
/// grey until a click); 0x200 is what bypasses that suppression. The old code passed 2, which
/// is not a valid flag -- one root cause of the grey traffic lights.
unsafe fn raise_window_slps(pid: i32, wid: u32) -> bool {
    let get_psn = match *GET_PROCESS_FOR_PID {
        Some(f) => f,
        None => {
            log_info!("[raise] SLPS unavailable: GetProcessForPID symbol missing");
            return false;
        }
    };
    let set_front = match *SLP_SET_FRONT {
        Some(f) => f,
        None => {
            log_info!("[raise] SLPS unavailable: _SLPSSetFrontProcessWithOptions symbol missing");
            return false;
        }
    };
    let mut psn = ProcessSerialNumber {
        high_long_of_psn: 0,
        low_long_of_psn: 0,
    };
    let psn_status = get_psn(pid, &mut psn);
    if psn_status != 0 {
        log_info!(
            "[raise] SLPS failed: GetProcessForPID pid={} status={}",
            pid,
            psn_status
        );
        return false;
    }
    let status = set_front(&mut psn, wid, 0x200); // kCPSUserGenerated; CGError success == 0
    if status != 0 {
        log_info!(
            "[raise] SLPS failed: _SLPSSetFrontProcessWithOptions pid={} cgwid={} status={}",
            pid,
            wid,
            status
        );
    }
    status == 0
}

/// 用 SkyLight 私有 API SLPSPostEventRecordTo 向目标窗口投递一次合成鼠标按下事件，
/// 使该窗口成为其 App 的 key window（红绿灯变彩色）。macOS 14+ 把 NSRunningApplication
/// activate 降级为「建议性请求」，跨 App 转移 key 焦点只剩这条可靠路径；0x200 的
/// userGenerated 前台切换只解决「抬到前面」，key 状态必须靠这个合成点击确立。
/// 点击点放到窗口右下方很远处，避免命中任何内容或 resize 区域。事件按 CGWindowID 定向投递，
/// 与坐标无关。
/// 字节布局来自 AltTab 对 CGSInternal/CGSEvent.h 的反向工程；缓冲必须 ≥0x100 并清零，
/// 否则 macOS 14.7.4+ 的 CGSEncodeEventRecord 会越界读取导致 SIGABRT（paneru#123）。
///
/// Make the window `wid` the key window of its app by posting a synthetic left-mouse-down
/// to the WindowServer via the SkyLight private API SLPSPostEventRecordTo.
/// macOS 14+ downgraded NSRunningApplication.activate to an advisory "request"; posting
/// this click is the only reliable way left to move key focus across apps. The 0x200
/// userGenerated front-switch only fronts the window -- the key state needs this click.
/// The click is aimed far beyond the window's bottom-right corner, so it hit-tests to no view
/// or resize edge (nothing is clicked). The event is delivered to the window by CGWindowID,
/// not by the click point.
/// layout is AltTab's reverse-engineering of CGSInternal/CGSEvent.h; the buffer must be
/// at least 0x100 bytes and zeroed, or CGSEncodeEventRecord reads past it on macOS 14.7.4+
/// and SIGABRTs (paneru issue 123).
unsafe fn make_key_window(pid: i32, wid: u32) -> bool {
    let get_psn = match *GET_PROCESS_FOR_PID {
        Some(f) => f,
        None => {
            log_info!("[raise] key click unavailable: GetProcessForPID symbol missing");
            return false;
        }
    };
    let post = match *SLPS_POST_EVENT_RECORD {
        Some(f) => f,
        None => {
            log_info!("[raise] key click unavailable: SLPSPostEventRecordTo symbol missing");
            return false;
        }
    };
    let mut psn = ProcessSerialNumber {
        high_long_of_psn: 0,
        low_long_of_psn: 0,
    };
    let psn_status = get_psn(pid, &mut psn);
    if psn_status != 0 {
        log_info!(
            "[raise] key click failed: GetProcessForPID pid={} status={}",
            pid,
            psn_status
        );
        return false;
    }
    // 0x100 字节清零缓冲:记录本身声明长度 0xf8(offset 0x04),多分配防止越界读崩溃。
    // Zeroed 0x100-byte buffer: the record declares 0xf8 (offset 0x04); the extra space
    // prevents the out-of-bounds read crash.
    let mut bytes = vec![0u8; 0x100];
    bytes[0x04] = 0xf8; // 记录长度 / record length
    bytes[0x3a] = 0x10; // 未公开标志(yabai/Hammerspoon 同款)/ undocumented flag (as yabai/Hammerspoon)
                        // 目标 CGWindowID @ 0x3c(4 字节,小端)/ target CGWindowID @ 0x3c (4 bytes, LE)
    bytes[0x3c..0x40].copy_from_slice(&wid.to_le_bytes());
    // 窗口相对点击点 @ 0x20(16 字节 = CGPoint 两个 f64),远离内容和 resize 区域。
    bytes[0x20..0x28].copy_from_slice(&(300_000.0f64).to_le_bytes());
    bytes[0x28..0x30].copy_from_slice(&(300_000.0f64).to_le_bytes());
    // 0x08 = CGSEventType:一次左键按下即可让目标窗口变 key。
    // 0x08 = CGSEventType: one left-mouse-down makes the target window key.
    bytes[0x08] = 0x01;
    let status = post(&mut psn, bytes.as_mut_ptr());
    if status != 0 {
        log_info!(
            "[raise] key click failed: SLPSPostEventRecordTo pid={} cgwid={} status={}",
            pid,
            wid,
            status
        );
    }
    status == 0
}

/// Ask AppKit to activate an application without raising all of its windows.
/// This is used only after the precise WindowServer path reports a transient failure.
pub(crate) fn activate_pid(pid: i32) -> bool {
    unsafe {
        let app: *mut AnyObject =
            msg_send![class!(NSRunningApplication), runningApplicationWithProcessIdentifier: pid];
        if app.is_null() {
            log_info!("[raise] activate fallback: no running app for pid={}", pid);
            return false;
        }
        let activated: bool = msg_send![app, activateWithOptions: 0usize];
        if !activated {
            log_info!(
                "[raise] activate fallback failed: pid={} activateWithOptions=false",
                pid
            );
        }
        activated
    }
}

/// 由 &str 构造 CFString(+1 引用,调用方 CFRelease)。窗口控制模块复用。
/// Build a CFString from &str (+1 reference; caller CFReleases). Reused by window control.
pub(crate) fn cf_string_new(s: &str) -> *const c_void {
    let c_str = std::ffi::CString::new(s).unwrap();
    unsafe { CFStringCreateWithCString(std::ptr::null(), c_str.as_ptr(), 0x08000100) }
}

unsafe fn cache_ax_window_element(
    pid: i32,
    process_start_time_us: Option<u64>,
    cgwid: u32,
    element: AXUIElementRef,
) {
    let Some(process_start_time_us) = process_start_time_us else {
        return;
    };
    if cgwid == 0 || element.is_null() {
        return;
    }
    let key = AxWindowCacheKey {
        pid,
        process_start_time_us,
        cgwid,
    };
    CFRetain(element);
    let old = AX_WINDOW_CACHE
        .lock()
        .unwrap()
        .insert(key, CachedAxElement(element));
    if let Some(old) = old {
        CFRelease(old.0);
    }
}

unsafe fn cached_ax_window_element(
    pid: i32,
    process_start_time_us: Option<u64>,
    cgwid: u32,
) -> Option<AXUIElementRef> {
    let key = AxWindowCacheKey {
        pid,
        process_start_time_us: process_start_time_us?,
        cgwid,
    };
    let cached = AX_WINDOW_CACHE.lock().unwrap().get(&key).copied()?;
    // The caller owns this temporary retain and must CFRelease it after the AX operations.
    CFRetain(cached.0);
    Some(cached.0)
}

unsafe fn invalidate_cached_ax_window_element(
    pid: i32,
    process_start_time_us: Option<u64>,
    cgwid: u32,
    element: AXUIElementRef,
) {
    let Some(process_start_time_us) = process_start_time_us else {
        return;
    };
    let key = AxWindowCacheKey {
        pid,
        process_start_time_us,
        cgwid,
    };
    let removed = {
        let mut cache = AX_WINDOW_CACHE.lock().unwrap();
        if cache.get(&key).is_some_and(|cached| cached.0 == element) {
            cache.remove(&key)
        } else {
            None
        }
    };
    if let Some(removed) = removed {
        CFRelease(removed.0);
    }
}

pub(crate) fn clear_ax_window_cache_for_pid(pid: i32) {
    let keys: Vec<_> = AX_WINDOW_CACHE
        .lock()
        .unwrap()
        .keys()
        .filter(|key| key.pid == pid)
        .copied()
        .collect();
    let removed: Vec<_> = keys
        .into_iter()
        .filter_map(|key| AX_WINDOW_CACHE.lock().unwrap().remove(&key))
        .collect();
    for element in removed {
        unsafe { CFRelease(element.0) };
    }
    AX_SNAPSHOT_CACHE.lock().unwrap().remove(&pid);
}

fn cached_ax_snapshot(
    pid: i32,
    process_start_time_us: Option<u64>,
) -> Option<Vec<(u32, String, bool)>> {
    let start = process_start_time_us?;
    let cache = AX_SNAPSHOT_CACHE.lock().unwrap();
    let snapshot = cache.get(&pid)?;
    if snapshot.process_start_time_us != Some(start)
        || snapshot.refreshed_at.elapsed() > AX_SNAPSHOT_CACHE_TTL
    {
        return None;
    }
    Some(snapshot.windows.clone())
}

fn cache_ax_snapshot(
    pid: i32,
    process_start_time_us: Option<u64>,
    windows: &[(u32, String, bool)],
) {
    if process_start_time_us.is_none() {
        return;
    }
    AX_SNAPSHOT_CACHE.lock().unwrap().insert(
        pid,
        CachedAxSnapshot {
            process_start_time_us,
            refreshed_at: Instant::now(),
            windows: windows.to_vec(),
        },
    );
}

fn cf_dict_get_string(dict: *const c_void, key: &str) -> Option<String> {
    let cf_key = cf_string_new(key);
    let value = unsafe { CFDictionaryGetValue(dict, cf_key) };
    unsafe { CFRelease(cf_key) };
    if value.is_null() {
        return None;
    }
    cf_to_rust_string(value)
}

fn cf_dict_get_i32(dict: *const c_void, key: &str) -> Option<i32> {
    let cf_key = cf_string_new(key);
    let value = unsafe { CFDictionaryGetValue(dict, cf_key) };
    unsafe { CFRelease(cf_key) };
    if value.is_null() {
        return None;
    }
    let mut num: i32 = 0;
    let ok = unsafe { CFNumberGetValue(value, 3, &mut num as *mut i32 as *mut c_void) };
    if ok {
        Some(num)
    } else {
        None
    }
}

fn cf_dict_get_u32(dict: *const c_void, key: &str) -> Option<u32> {
    let cf_key = cf_string_new(key);
    let value = unsafe { CFDictionaryGetValue(dict, cf_key) };
    unsafe { CFRelease(cf_key) };
    if value.is_null() {
        return None;
    }
    let mut num: i32 = 0;
    let ok = unsafe { CFNumberGetValue(value, 3, &mut num as *mut i32 as *mut c_void) };
    if ok {
        Some(num as u32)
    } else {
        None
    }
}

/// 读 CG dict 里的 CFNumber double(如 kCGWindowAlpha)。CFNumberGetValue type 13 =
/// kCFNumberDoubleType。
/// Read a CFNumber double from a CG dict (e.g. kCGWindowAlpha). CFNumberGetValue type 13 =
/// kCFNumberDoubleType.
fn cf_dict_get_f64(dict: *const c_void, key: &str) -> Option<f64> {
    let cf_key = cf_string_new(key);
    let value = unsafe { CFDictionaryGetValue(dict, cf_key) };
    unsafe { CFRelease(cf_key) };
    if value.is_null() {
        return None;
    }
    let mut num: f64 = 0.0;
    let ok = unsafe { CFNumberGetValue(value, 13, &mut num as *mut f64 as *mut c_void) };
    if ok {
        Some(num)
    } else {
        None
    }
}

// 读 CG dict 里的 CFBoolean(如 kCGWindowIsOnscreen)。/ Read a CFBoolean from a CG dict (e.g. kCGWindowIsOnscreen).
fn cf_dict_get_bool(dict: *const c_void, key: &str) -> Option<bool> {
    let cf_key = cf_string_new(key);
    let value = unsafe { CFDictionaryGetValue(dict, cf_key) };
    unsafe { CFRelease(cf_key) };
    if value.is_null() {
        return None;
    }
    Some(unsafe { CFBooleanGetValue(value) })
}

/// 读 CG dict 里的 kCGWindowBounds(嵌套 dict:X/Y/Width/Height),返回 (x, y, w, h)。
/// 用于确定激活窗口所在屏幕(overlay 的"跟随激活窗口"定位)。
///
/// Read kCGWindowBounds (a nested dict: X/Y/Width/Height) from a CG dict, returning (x, y, w, h).
/// Used to determine the active window's screen (the overlay's "follow active window" placement).
fn cf_dict_get_bounds(dict: *const c_void, key: &str) -> Option<(f64, f64, f64, f64)> {
    let cf_key = cf_string_new(key);
    let value = unsafe { CFDictionaryGetValue(dict, cf_key) };
    unsafe { CFRelease(cf_key) };
    if value.is_null() {
        return None;
    }
    let x = cf_dict_get_f64(value, "X")?;
    let y = cf_dict_get_f64(value, "Y")?;
    let w = cf_dict_get_f64(value, "Width")?;
    let h = cf_dict_get_f64(value, "Height")?;
    Some((x, y, w, h))
}

/// 一个运行中 App 的缓存身份:`key` 用作缓存文件名,`fingerprint` 用于检测 App 更新。
/// A running app's cache identity: `key` is the cache filename, `fingerprint` detects updates.
pub(crate) struct AppIdentity {
    pub(crate) key: String,
    /// 可执行文件 mtime(自 UNIX epoch 的秒数)。None 表示无法校验,退化为「文件存在即有效」。
    /// Executable mtime (seconds since UNIX epoch). None means unverified -> "file exists = valid".
    pub(crate) fingerprint: Option<String>,
    /// 进程启动时间(自 UNIX epoch 起的微秒),用于区分 PID 复用后的不同进程实例。
    /// Process start time in microseconds since UNIX epoch, used to distinguish PID reuse.
    pub(crate) process_start_time_us: Option<u64>,
}

/// 读一个 NSString 到 Rust String(nil -> None)。对象是 autoreleased,调用方需在池内。
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

/// 解析一个 PID 对应 App 的缓存身份。
/// 键优先级:bundleIdentifier(reverse-DNS,文件名安全)> 可执行文件路径哈希 > `pid_{pid}` 兜底。
/// 指纹取可执行文件 mtime;App 更新会换新 mtime -> 触发重提。
/// 剪贴板记录来源时也复用此身份(与切换器同一套键/回退)。
///
/// Resolve a PID's cache identity. Key priority: bundleIdentifier (reverse-DNS,
/// filename-safe) > hashed executable path > `pid_{pid}` fallback. Fingerprint is the
/// executable mtime; an app update gets a new mtime -> forces re-extract. The clipboard
/// reuses this identity when recording a source (the same key/fallback chain as the switcher).
pub(crate) unsafe fn resolve_app_identity(pid: i32) -> AppIdentity {
    let app: *mut AnyObject =
        msg_send![class!(NSRunningApplication), runningApplicationWithProcessIdentifier: pid];
    if app.is_null() {
        // PID 已失效(App 刚退出)-> 回退到 pid 键,无指纹(无法校验)。
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

    let exec_url: *mut AnyObject = msg_send![app, executableURL];
    let exec_path = if exec_url.is_null() {
        None
    } else {
        let path_obj: *mut AnyObject = msg_send![exec_url, path];
        read_nsstring(path_obj)
    };

    // 指纹 = 可执行文件 mtime(秒)。取不到(路径为空 / stat 失败)-> None,退化为不校验。
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

fn window_instance_key(
    pid: i32,
    process_start_time_us: Option<u64>,
    cgwid: u32,
) -> Option<WindowInstanceKey> {
    (pid > 0 && cgwid != 0).then_some(WindowInstanceKey {
        pid,
        process_start_time_us: process_start_time_us?,
        cgwid,
    })
}

fn remember_non_normal_window(pid: i32, process_start_time_us: Option<u64>, cgwid: u32) {
    let Some(key) = window_instance_key(pid, process_start_time_us, cgwid) else {
        return;
    };
    let mut known = KNOWN_NON_NORMAL_WINDOWS.lock().unwrap();
    // A newly observed process incarnation with the same PID supersedes any stale entries from
    // the old incarnation. This also bounds the cache when a WindowServer destroy notification
    // is delayed or lost.
    known.retain(|entry| {
        entry.pid != key.pid || entry.process_start_time_us == key.process_start_time_us
    });
    known.insert(key);
}

fn is_known_non_normal_window(pid: i32, process_start_time_us: Option<u64>, cgwid: u32) -> bool {
    window_instance_key(pid, process_start_time_us, cgwid)
        .is_some_and(|key| KNOWN_NON_NORMAL_WINDOWS.lock().unwrap().contains(&key))
}

/// Forget a destroyed window's sticky layer classification. CGWindowID is globally unique while
/// alive, so removing all entries with this ID is safe and prevents stale state on ID reuse.
pub(crate) fn forget_non_normal_window(cgwid: u32) {
    if cgwid == 0 {
        return;
    }
    KNOWN_NON_NORMAL_WINDOWS
        .lock()
        .unwrap()
        .retain(|entry| entry.cgwid != cgwid);
}

/// AX 关闭窗口(等效点击窗口的关闭按钮):遍历 pid 的 AXWindows 匹配 cgwid,
/// 主方案 = 取窗口的 AXCloseButton 元素并执行 AXPress;兜底 = 窗口级 AXClose
/// action(macOS 26 上多数 app 不再暴露,故以按钮方案为主)。最小化窗口同样适用。
/// 返回是否找到并成功发起关闭;失败 → false。
/// Close a window via AX (equivalent to clicking its close button): scan the pid's
/// AXWindows for cgwid; the PRIMARY path grabs the window's AXCloseButton and presses it;
/// the fallback is the window-level AXClose action (most apps no longer expose it on
/// macOS 26, hence the button-first approach). Works for minimized windows too. Returns
/// whether the close was initiated.
pub(crate) fn close_ax_window(pid: i32, cgwid: u32) -> bool {
    unsafe {
        let app = AXUIElementCreateApplication(pid);
        if app.is_null() {
            return false;
        }
        AXUIElementSetMessagingTimeout(app, 0.5);
        let windows_key = cf_string_new("AXWindows");
        let mut windows_array: *const c_void = std::ptr::null();
        let err = AXUIElementCopyAttributeValue(app, windows_key, &mut windows_array);
        CFRelease(windows_key);
        CFRelease(app);
        if err != K_AX_SUCCESS || windows_array.is_null() {
            return false;
        }
        let count = CFArrayGetCount(windows_array);
        let close_btn_key = cf_string_new("AXCloseButton");
        let press_key = cf_string_new("AXPress");
        let close_key = cf_string_new("AXClose");
        let mut ok = false;
        for i in 0..count {
            let element = CFArrayGetValueAtIndex(windows_array, i);
            if element.is_null() {
                continue;
            }
            if ax_window_cgwid(element) != Some(cgwid) {
                continue;
            }
            // 主方案:关闭按钮 + AXPress(标准窗口通用,系统设置/Chrome 实测可用)。
            // Primary: the close button + AXPress (works on any standard window; verified
            // on System Settings and Chrome).
            let mut close_btn: *const c_void = std::ptr::null();
            if AXUIElementCopyAttributeValue(element, close_btn_key, &mut close_btn) == K_AX_SUCCESS
                && !close_btn.is_null()
            {
                ok = AXUIElementPerformAction(close_btn, press_key) == K_AX_SUCCESS;
                CFRelease(close_btn);
                break;
            }
            // 兜底:窗口级 AXClose。
            // Fallback: the window-level AXClose action.
            ok = AXUIElementPerformAction(element, close_key) == K_AX_SUCCESS;
            break;
        }
        CFRelease(close_btn_key);
        CFRelease(press_key);
        CFRelease(close_key);
        CFRelease(windows_array);
        ok
    }
}

/// 抬升前半段:WindowServer 层抬窗(SLPS)+ 合成点击确立 key window。
/// 普通窗口在提交时调用;最小化窗口由后台 raiser 在解除最小化后调用。
///
/// First half of the raise: WindowServer-level raise (SLPS) plus the synthetic click that
/// establishes the key window. Normal windows call it at commit time; minimized windows call it
/// from the background raiser after being restored.
pub(crate) fn raise_window_fast(pid: i32, cgwid: u32) -> (bool, bool) {
    if cgwid == 0 {
        return (false, false);
    }
    unsafe {
        // 1. WindowServer 层只抬这一个窗口（SkyLight 私有 API _SLPSSetFrontProcessWithOptions，
        //    mode=0x200 userGenerated），避免 activate(AllWindows) 把该 App 的所有窗口都抬到前面。
        //    Raise only this one window at the WindowServer level (SkyLight private API,
        //    mode=0x200 userGenerated), avoiding activate(AllWindows) raising every window.
        let fast_started = Instant::now();
        let slps_started = Instant::now();
        let slps_ok = raise_window_slps(pid, cgwid);
        let slps_elapsed = slps_started.elapsed().as_micros();

        // 1.5 合成鼠标点击确立 key window：SLPS 只负责「抬到前面 + 设为前台进程」，key 状态
        //    必须由这个合成点击授予（macOS 14+ 无公开 API 可跨 App 转移 key 焦点）。
        //    The synthetic click establishes the key window: SLPS only fronts the window and
        //    process; the key state is granted by this click (macOS 14+ has no public API to
        //    move key focus across apps).
        let click_started = Instant::now();
        let click_ok = make_key_window(pid, cgwid);
        log_debug!(
            "[raise] fast stages: pid={} cgwid={} slps={} slps_us={} click={} click_us={} total_us={}",
            pid,
            cgwid,
            slps_ok,
            slps_elapsed,
            click_ok,
            click_started.elapsed().as_micros(),
            fast_started.elapsed().as_micros()
        );
        (slps_ok, click_ok)
    }
}

// ========== AX 阶段后台化(专职 raiser 线程) / background AX phase (dedicated raiser) ==========

// 最新抬升意图代号:每次提交切换自增;后台任务在应用 AX 变更前重查,已被更新的切换
// 取代就中止——快速连续切换时,旧任务绝不能把旧窗口又抬回新窗口上面(乱序回跳)。
// Generation of the latest raise intent: bumped on every committed switch. Background jobs
// re-check before applying AX mutations and abort once a newer switch supersedes them --
// during rapid consecutive switches, a stale job must never re-raise an old window over the
// newest one (out-of-order flicker).
static RAISE_GENERATION: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

fn raise_intent_current(generation: u64) -> bool {
    RAISE_GENERATION.load(std::sync::atomic::Ordering::Acquire) == generation
}

struct RaiseJob {
    pid: i32,
    cgwid: u32,
    minimized: bool,
    fast_path_ok: bool,
    generation: u64,
    enqueued_at: Instant,
}

/// AX mutations are delivered to AppKit and must run on the main thread.  Keep the potentially
/// blocking AX lookup on `ax-raiser`, then retain the resolved objects until the main-thread
/// callback applies the final raise/focus actions.
struct MainThreadAxRaise {
    pid: i32,
    cgwid: u32,
    app: AXUIElementRef,
    element: AXUIElementRef,
    focused_key: AXUIElementRef,
    raise_key: AXUIElementRef,
    minimized_key: Option<AXUIElementRef>,
    force_focus: bool,
    generation: u64,
}

unsafe impl Send for MainThreadAxRaise {}

static MAIN_THREAD_AX_RAISES: LazyLock<Mutex<Vec<MainThreadAxRaise>>> =
    LazyLock::new(|| Mutex::new(Vec::new()));

/// Drain AX mutations on the AppKit main thread.  `AXUIElementPerformAction` can synchronously
/// enter AppKit (for example `makeKeyAndOrderFront:`), so invoking it from the background AX
/// worker triggers macOS's "Must only be used from the main thread" trap.
pub(crate) fn handle_ax_raise_main() {
    let jobs = MAIN_THREAD_AX_RAISES
        .lock()
        .unwrap()
        .drain(..)
        .collect::<Vec<_>>();
    for job in jobs {
        unsafe {
            if !raise_intent_current(job.generation) {
                release_main_thread_ax_raise(&job);
                continue;
            }

            let minimized_set_err = job
                .minimized_key
                .map(|key| AXUIElementSetAttributeValue(job.element, key, kCFBooleanFalse));
            if minimized_set_err.is_some() {
                let (slps_ok, click_ok) = raise_window_fast(job.pid, job.cgwid);
                log_debug!(
                    "[raise] precise fast after main-thread unminimize: pid={} cgwid={} slps={} click={}",
                    job.pid,
                    job.cgwid,
                    slps_ok,
                    click_ok
                );
            }
            let raise_started = Instant::now();
            let raise_first_err = AXUIElementPerformAction(job.element, job.raise_key);
            let raise_first_us = raise_started.elapsed().as_micros();
            let (focused_set_err, raise_retry_err) = if !job.force_focus
                && (raise_first_err == K_AX_SUCCESS || raise_first_err == K_AX_INVALID_UI_ELEMENT)
            {
                (None, None)
            } else {
                let focused_set_err =
                    AXUIElementSetAttributeValue(job.app, job.focused_key, job.element);
                let raise_retry_err = AXUIElementPerformAction(job.element, job.raise_key);
                (Some(focused_set_err), Some(raise_retry_err))
            };
            log_debug!(
                "[raise] AXRaise main: pid={} cgwid={} first_err={} first_us={} set_focused={:?} retry={:?} set_minimized={:?}",
                job.pid,
                job.cgwid,
                raise_first_err,
                raise_first_us,
                focused_set_err,
                raise_retry_err,
                minimized_set_err
            );
            release_main_thread_ax_raise(&job);
        }
    }
}

unsafe fn release_main_thread_ax_raise(job: &MainThreadAxRaise) {
    CFRelease(job.app);
    CFRelease(job.element);
    CFRelease(job.focused_key);
    CFRelease(job.raise_key);
    if let Some(key) = job.minimized_key {
        CFRelease(key);
    }
}

#[allow(clippy::too_many_arguments)]
unsafe fn enqueue_main_thread_ax_raise(
    pid: i32,
    cgwid: u32,
    app: AXUIElementRef,
    element: AXUIElementRef,
    focused_key: AXUIElementRef,
    raise_key: AXUIElementRef,
    minimized_key: Option<AXUIElementRef>,
    force_focus: bool,
    generation: u64,
) {
    if !raise_intent_current(generation) {
        return;
    }
    // The caller retains these objects for its own cleanup.  Take an additional retain for the
    // queued main-thread job, including the array-borrowed window element.
    CFRetain(app);
    CFRetain(element);
    CFRetain(focused_key);
    CFRetain(raise_key);
    if let Some(key) = minimized_key {
        CFRetain(key);
    }
    MAIN_THREAD_AX_RAISES
        .lock()
        .unwrap()
        .push(MainThreadAxRaise {
            pid,
            cgwid,
            app,
            element,
            focused_key,
            raise_key,
            minimized_key,
            force_focus,
            generation,
        });

    if let Some(controller) = crate::CONTROLLER.lock().unwrap().map(|ptr| ptr.0) {
        let _: () = msg_send![controller,
            performSelectorOnMainThread: sel!(handleAxRaise:),
            withObject: std::ptr::null::<AnyObject>(),
            waitUntilDone: false
        ];
    } else {
        // This can only happen during early shutdown/startup, but do not leave retained AX
        // objects behind if the controller has already gone away.
        let pending = MAIN_THREAD_AX_RAISES
            .lock()
            .unwrap()
            .drain(..)
            .collect::<Vec<_>>();
        for job in pending {
            release_main_thread_ax_raise(&job);
        }
    }
}

// 单一专职 raiser 线程:串行 FIFO 消费任务,保证两次切换的 AX 阶段不并发、
// 完成顺序与提交顺序一致(配合 supersede 检查,最终状态永远是最后一次切换)。
// A single dedicated raiser thread consumes jobs serially, so AX phases of consecutive
// switches never overlap and completion order equals commit order (combined with the
// supersede check, the final state is always the last switch).
static RAISE_TX: std::sync::LazyLock<flume::Sender<RaiseJob>> = std::sync::LazyLock::new(|| {
    let (tx, rx) = flume::unbounded::<RaiseJob>();
    std::thread::Builder::new()
        .name("ax-raiser".into())
        .spawn(move || {
            for job in rx.iter() {
                run_raise_ax_job(job);
            }
        })
        .expect("spawn ax-raiser thread");
    tx
});

/// 提交后台 AX 精确抬升任务。普通窗口只执行 cached AXRaise;已知最小化窗口先还原。
/// Enqueue the serialized AX backstop. Normal windows only perform cached AXRaise; known
/// minimized windows are restored first.
/// AX 枚举对无响应 App 可能阻塞几十至上百毫秒,绝不能放主线程。
///
/// AX enumeration can block tens to hundreds of milliseconds on an unresponsive app; it must
/// stay off the main thread.
pub(crate) fn raise_window_ax_async(
    pid: i32,
    cgwid: u32,
    minimized: bool,
    fast_path_ok: bool,
) -> u64 {
    if cgwid == 0 {
        return 0;
    }
    let generation = RAISE_GENERATION.fetch_add(1, std::sync::atomic::Ordering::AcqRel) + 1;
    let _ = RAISE_TX.send(RaiseJob {
        pid,
        cgwid,
        minimized,
        fast_path_ok,
        generation,
        enqueued_at: Instant::now(),
    });
    generation
}

fn run_raise_ax_job(job: RaiseJob) {
    if !raise_intent_current(job.generation) {
        log_debug!(
            "[raise] ax superseded before start: pid={} cgwid={} gen={}",
            job.pid,
            job.cgwid,
            job.generation
        );
        return;
    }
    let started = Instant::now();
    log_debug!(
        "[raise] ax job start: pid={} cgwid={} gen={} queue_ms={}",
        job.pid,
        job.cgwid,
        job.generation,
        job.enqueued_at.elapsed().as_millis()
    );
    // 后台线程一律包 autorelease pool:当前只用 CF 对象,包一层防将来引入 ObjC 调用后泄漏。
    // Always wrap background work in an autorelease pool: this path only touches CF objects
    // today; the pool guards against leaks if ObjC calls are ever added.
    unsafe {
        let pool: *mut AnyObject = msg_send![class!(NSAutoreleasePool), new];
        let force_ax_focus = if job.minimized || job.fast_path_ok {
            !job.fast_path_ok
        } else {
            !retry_failed_fast_path(&job)
        };
        raise_window_ax_job(&job, started, force_ax_focus);
        let _: () = msg_send![pool, drain];
    }
}

/// Recover a failed synchronous raise without blocking the main thread.
/// The first attempt already ran at commit time; these retries cover the short-lived
/// process-table/WindowServer race seen when switching away from another application.
unsafe fn retry_failed_fast_path(job: &RaiseJob) -> bool {
    const RETRY_DELAYS_MS: [u64; 2] = [8, 20];
    let started = Instant::now();
    let activate_ok = activate_pid(job.pid);
    let mut last = (false, false);
    let mut attempts = 0;
    for delay_ms in RETRY_DELAYS_MS {
        if !raise_intent_current(job.generation) {
            log_debug!(
                "[raise] fast recovery superseded: pid={} cgwid={} gen={}",
                job.pid,
                job.cgwid,
                job.generation
            );
            return false;
        }
        std::thread::sleep(Duration::from_millis(delay_ms));
        attempts += 1;
        last = raise_window_fast(job.pid, job.cgwid);
        if last.0 && last.1 {
            log_debug!(
                "[raise] fast recovery succeeded: pid={} cgwid={} activate={} attempts={} elapsed={}ms",
                job.pid,
                job.cgwid,
                activate_ok,
                attempts,
                started.elapsed().as_millis()
            );
            return true;
        }
    }
    log_info!(
        "[raise] fast recovery exhausted: pid={} cgwid={} activate={} slps={} click={} attempts={} elapsed={}ms",
        job.pid,
        job.cgwid,
        activate_ok,
        last.0,
        last.1,
        attempts,
        started.elapsed().as_millis()
    );
    false
}

/// AX 阶段本体:缓存命中时普通窗口只执行 AXRaise;已知最小化窗口先还原并补跑快速路径。
/// 缓存失效才枚举 AXWindows,按 CGWindowID 重新配对并刷新缓存。
/// AX phase: on a cache hit, normal windows only perform AXRaise; known minimized windows are
/// restored first and then run the fast path. Only a stale/missing cache enumerates AXWindows,
/// pairs by CGWindowID, and refreshes the cache.
unsafe fn raise_window_ax_job(job: &RaiseJob, started: Instant, force_ax_focus: bool) {
    let app_started = Instant::now();
    let app = AXUIElementCreateApplication(job.pid);
    if app.is_null() {
        log_info!("[raise] ax skipped: no AX app for pid={}", job.pid);
        return;
    }
    let process_start_time_us = resolve_app_identity(job.pid).process_start_time_us;
    let app_create_us = app_started.elapsed().as_micros();
    // 1s 消息超时:无响应 App 不能把 raiser 线程卡太久(旧同步实现无超时,最坏按默认
    // 超时阻塞主线程);超时只损失焦点兜底,快速路径的 SLPS 抬窗不受影响。
    // 1s messaging timeout: an unresponsive app must not stall the raiser thread for long
    // (the old sync path had no timeout and could block the main thread for the default
    // timeout). A timeout only loses the focus backstop; the fast path's SLPS raise stands.
    AXUIElementSetMessagingTimeout(app, 1.0);

    let raise_key = cf_string_new("AXRaise");
    let focused_key = cf_string_new("AXFocusedWindow");
    let minimized_key = job.minimized.then(|| cf_string_new("AXMinimized"));

    // collect_windows 已经为当前窗口保留了 AX 元素。正常切换直接复用它，避免每次都做
    // 一次 AXWindows IPC；只有元素失效时才回退到下面的实时枚举。
    // collect_windows retains the AX element for the current window. Reuse it for normal
    // switches to avoid an AXWindows IPC round trip; only stale elements use the live scan below.
    if let Some(element) = cached_ax_window_element(job.pid, process_start_time_us, job.cgwid) {
        if !raise_intent_current(job.generation) {
            CFRelease(element);
            CFRelease(raise_key);
            CFRelease(focused_key);
            if let Some(minimized_key) = minimized_key {
                CFRelease(minimized_key);
            }
            CFRelease(app);
            return;
        }
        // Unminimize is an AX mutation and is performed by the main-thread queue below.
        let minimized_set_err: Option<AXError> = None;
        let (raise_first_err, focused_set_err, raise_retry_err) = raise_ax_element(
            job.pid,
            job.cgwid,
            app,
            element,
            focused_key,
            raise_key,
            minimized_key,
            force_ax_focus,
            job.generation,
        );
        let effective_raise_err = raise_retry_err.unwrap_or(raise_first_err);
        if effective_raise_err != K_AX_INVALID_UI_ELEMENT {
            log_debug!(
                "[raise] ax raised cached: pid={} cgwid={} known_minimized={} set_minimized={:?} raise_first={} set_focused={:?} raise_retry={:?} app_create_us={} waited={}ms total={}ms",
                job.pid,
                job.cgwid,
                job.minimized,
                minimized_set_err,
                raise_first_err,
                focused_set_err,
                raise_retry_err,
                app_create_us,
                job.enqueued_at.elapsed().as_millis(),
                started.elapsed().as_millis()
            );
            CFRelease(element);
            CFRelease(raise_key);
            CFRelease(focused_key);
            if let Some(minimized_key) = minimized_key {
                CFRelease(minimized_key);
            }
            CFRelease(app);
            return;
        }
        log_debug!(
            "[raise] cached AX element stale: pid={} cgwid={} raise={} — refreshing",
            job.pid,
            job.cgwid,
            effective_raise_err
        );
        invalidate_cached_ax_window_element(job.pid, process_start_time_us, job.cgwid, element);
        CFRelease(element);
    }

    let windows_key = cf_string_new("AXWindows");
    let mut windows_array: *const c_void = std::ptr::null();
    let err = AXUIElementCopyAttributeValue(app, windows_key, &mut windows_array);
    CFRelease(windows_key);
    if err != K_AX_SUCCESS || windows_array.is_null() {
        CFRelease(raise_key);
        CFRelease(focused_key);
        if let Some(minimized_key) = minimized_key {
            CFRelease(minimized_key);
        }
        CFRelease(app);
        log_info!(
            "[raise] ax NO MATCH: pid={} cgwid={} ax_query_err={} waited={}ms",
            job.pid,
            job.cgwid,
            err,
            job.enqueued_at.elapsed().as_millis()
        );
        return;
    }
    let count = CFArrayGetCount(windows_array);
    let mut matched = false;
    let mut superseded = false;
    for i in 0..count {
        let element = CFArrayGetValueAtIndex(windows_array, i);
        if element.is_null() {
            continue;
        }
        if ax_window_cgwid(element) == Some(job.cgwid) {
            // 应用变更前的最后一道 supersede 闸:枚举期间若来了更新的切换,本任务整体
            // 放弃(枚举结果作废),由新任务重新执行。
            // Final supersede gate right before applying: if a newer switch arrived during
            // enumeration, drop this job entirely (stale match) and let the new one run.
            if !raise_intent_current(job.generation) {
                superseded = true;
                log_debug!(
                    "[raise] ax superseded before apply: pid={} cgwid={} gen={}",
                    job.pid,
                    job.cgwid,
                    job.generation
                );
                break;
            }
            // 找到实时元素后更新缓存,下一次切换就不必重新枚举。
            // Refresh the cache with the live element so the next switch skips enumeration.
            cache_ax_window_element(job.pid, process_start_time_us, job.cgwid, element);
            // Unminimize is an AX mutation and is performed by the main-thread queue below.
            let minimized_set_err: Option<AXError> = None;
            let (raise_first_err, focused_set_err, raise_retry_err) = raise_ax_element(
                job.pid,
                job.cgwid,
                app,
                element,
                focused_key,
                raise_key,
                minimized_key,
                force_ax_focus,
                job.generation,
            );
            matched = true;
            log_debug!(
                "[raise] ax raised refreshed: pid={} cgwid={} ax_windows={} known_minimized={} set_minimized={:?} raise_first={} set_focused={:?} raise_retry={:?} app_create_us={} waited={}ms total={}ms",
                job.pid,
                job.cgwid,
                count,
                job.minimized,
                minimized_set_err,
                raise_first_err,
                focused_set_err,
                raise_retry_err,
                app_create_us,
                job.enqueued_at.elapsed().as_millis(),
                started.elapsed().as_millis()
            );
            break;
        }
    }
    if !matched && !superseded {
        log_info!(
            "[raise] ax NO MATCH: pid={} cgwid={} ax_windows={} waited={}ms total={}ms",
            job.pid,
            job.cgwid,
            count,
            job.enqueued_at.elapsed().as_millis(),
            started.elapsed().as_millis()
        );
    }
    CFRelease(raise_key);
    CFRelease(focused_key);
    if let Some(minimized_key) = minimized_key {
        CFRelease(minimized_key);
    }
    CFRelease(windows_array);
    CFRelease(app);
}

/// 普通路径只执行一次 AXRaise。仅当它明确失败且不是 stale element 时,才设置
/// AXFocusedWindow 并重试;成功路径不再支付额外 AX IPC。
/// The normal path performs one AXRaise. Only an explicit non-stale failure sets
/// AXFocusedWindow and retries; a successful raise pays no extra AX IPC.
#[allow(clippy::too_many_arguments)]
unsafe fn raise_ax_element(
    pid: i32,
    cgwid: u32,
    app: AXUIElementRef,
    element: AXUIElementRef,
    focused_key: AXUIElementRef,
    raise_key: AXUIElementRef,
    minimized_key: Option<AXUIElementRef>,
    force_focus: bool,
    generation: u64,
) -> (AXError, Option<AXError>, Option<AXError>) {
    log_debug!(
        "[raise] AXRaise queued for main thread: pid={} cgwid={} force_focus={}",
        pid,
        cgwid,
        force_focus
    );
    enqueue_main_thread_ax_raise(
        pid,
        cgwid,
        app,
        element,
        focused_key,
        raise_key,
        minimized_key,
        force_focus,
        generation,
    );
    (K_AX_SUCCESS, None, None)
}

/// 查一个 PID 的 AX 标准窗口列表。
/// 返回 None = AX 查询失败(该 App 无 AX 数据,可走 CG 回退);
/// Some(vec) = 查询成功,subrole 过滤后可能为空(无标准窗口 = 调度中心不显示它,直接跳过)。
///
/// Query an app's AX standard-window list.
/// None = AX query failed (app has no AX data; CG fallback allowed);
/// Some(vec) = query succeeded, possibly empty after subrole filtering (no standard windows =
/// Mission Control won't show it; skip entirely).
/// AX 窗口角色白名单:标准窗口(AXStandardWindow)任意;对话框(AXDialog——
/// JetBrains 系 IDE 的主窗口角色)必须有非空标题;其余角色(弹窗/面板/隐形窗口)
/// 一律过滤;无 subrole 视为标准窗口(部分 App 不设置此属性)。空标题的 AXDialog
/// 仍被过滤——BetterDisplay 隐形窗口防线的一部分(隐形窗口无标题,无论 subrole
/// 是什么都不该进入列表)。纯函数,单测覆盖。
/// AX-window subrole keep-rule: standard windows always pass; AXDialog (the subrole
/// JetBrains IDEs use for their MAIN windows) only when titled; everything else
/// (popups/panels/invisible windows) is filtered; a missing subrole counts as standard
/// (some apps don't set it). Untitled AXDialog stays filtered -- part of the
/// BetterDisplay invisible-window defense (invisible windows are untitled, so whatever
/// their subrole, they must not enter the list). Pure function, unit-tested.
fn ax_subrole_kept(subrole: Option<&str>, titled: bool) -> bool {
    match subrole {
        Some("AXStandardWindow") => true,
        Some("AXDialog") => titled,
        Some(_) => false,
        // 无 subrole → 视为标准窗口(部分 App 不设置此属性)。
        // A missing subrole counts as standard (some apps don't set it).
        None => true,
    }
}

/// Decide whether an AX-only window may be backfilled into the switcher.
/// A missing CG entry means an orderOut'd window, which AX can legitimately recover; a known
/// non-zero CG layer means an app-owned overlay/menu and must stay out of the window switcher.
///
/// 判断 AX-only 窗口是否可以补回切换器。CG 中完全没有对应项表示 orderOut 的窗口,AX
/// 仍可能合法地补回;但如果 CG 已知该窗口处于非 0 层,它就是应用浮层/菜单,不能进入切换器。
fn should_backfill_ax_window(cg_layer: Option<i32>) -> bool {
    match cg_layer {
        Some(layer) => layer == 0,
        None => true,
    }
}

fn should_backfill_ax_window_for_process(
    pid: i32,
    cgwid: u32,
    process_start_time_us: Option<u64>,
    cg_layer: Option<i32>,
) -> bool {
    should_backfill_ax_window(cg_layer)
        && !is_known_non_normal_window(pid, process_start_time_us, cgwid)
}

fn remember_non_normal_cg_windows(
    cg_window_layers: &HashMap<(i32, u32), i32>,
    identities: &HashMap<i32, AppIdentity>,
) {
    for (&(pid, cgwid), &layer) in cg_window_layers {
        if layer != 0 {
            let process_start_time_us = identities
                .get(&pid)
                .and_then(|identity| identity.process_start_time_us);
            remember_non_normal_window(pid, process_start_time_us, cgwid);
        }
    }
}

fn remember_non_normal_cg_windows_for_process(
    pid: i32,
    process_start_time_us: Option<u64>,
    cg_window_layers: &HashMap<u32, i32>,
) {
    for (&cgwid, &layer) in cg_window_layers {
        if layer != 0 {
            remember_non_normal_window(pid, process_start_time_us, cgwid);
        }
    }
}

/// 查某 PID 的全部标准 AX 窗口:(cgwid, 标题, 是否最小化)。collect_windows 与
/// 缩略图模块的启动预生成共用。
/// All standard AX windows for a PID: (cgwid, title, minimized). Shared between
/// collect_windows and the thumbnail module's startup pre-generation.
pub(crate) fn get_ax_windows_for_pid(pid: i32) -> Option<Vec<(u32, String, bool)>> {
    let process_start_time_us = unsafe { resolve_app_identity(pid).process_start_time_us };
    get_ax_windows_for_pid_with_identity(pid, process_start_time_us)
}

fn get_ax_windows_for_pid_with_identity(
    pid: i32,
    process_start_time_us: Option<u64>,
) -> Option<Vec<(u32, String, bool)>> {
    if let Some(windows) = cached_ax_snapshot(pid, process_start_time_us) {
        log_debug!(
            "[collect] ax cache hit: pid={} windows={}",
            pid,
            windows.len()
        );
        return Some(windows);
    }

    unsafe {
        let app = AXUIElementCreateApplication(pid);
        if app.is_null() {
            return None;
        }

        // 设 50ms 消息超时:慢/无响应 App 的 AX 查询会快速失败,而不是卡默认 10s 超时
        // (后者会让该 App 整体走 CG 回退,混入隐形窗口)。
        // Set a 50ms messaging timeout: AX queries on slow/unresponsive apps fail fast instead
        // of hitting the default 10s timeout (which would push the app to the CG fallback path
        // and let invisible windows through).
        AXUIElementSetMessagingTimeout(app, 0.05);

        let windows_key = cf_string_new("AXWindows");
        let mut windows_array: *const c_void = std::ptr::null();
        let err = AXUIElementCopyAttributeValue(app, windows_key, &mut windows_array);
        CFRelease(windows_key);
        CFRelease(app);
        if err != K_AX_SUCCESS || windows_array.is_null() {
            return None;
        }

        let count = CFArrayGetCount(windows_array);
        let title_key = cf_string_new("AXTitle");
        let subrole_key = cf_string_new("AXSubrole");
        let minimized_key = cf_string_new("AXMinimized");
        let mut results = Vec::with_capacity(count as usize);

        for i in 0..count {
            let element = CFArrayGetValueAtIndex(windows_array, i);
            if element.is_null() {
                continue;
            }

            // 只保留标准窗口(AXStandardWindow)+ 有标题的对话框(AXDialog),过滤弹出
            // 面板/下拉菜单等非标准窗口。AXDialog 是 JetBrains 系 IDE 主窗口的角色
            // (WebStorm 实测),必须放行;空标题的 AXDialog 仍按弹出面板过滤(见
            // ax_subrole_kept 注释:BetterDisplay 隐形窗口防线)。
            // Keep AXStandardWindow + TITLED AXDialog; filter popups/panels/dropdowns.
            // AXDialog is the subrole JetBrains IDEs use for their main windows (measured
            // on WebStorm) and must pass; an UNTITLED AXDialog stays filtered (see
            // ax_subrole_kept: the BetterDisplay invisible-window defense).
            let mut subrole_value: *const c_void = std::ptr::null();
            let kept = if AXUIElementCopyAttributeValue(element, subrole_key, &mut subrole_value)
                == K_AX_SUCCESS
                && !subrole_value.is_null()
            {
                let s = cf_to_rust_string(subrole_value);
                CFRelease(subrole_value);
                // AXDialog 需额外判断标题(见 ax_subrole_kept:JetBrains 主窗口有标题,
                // 空标题对话框按弹出面板过滤)。
                // AXDialog needs the extra title check (see ax_subrole_kept: JetBrains
                // main windows are titled; untitled dialogs stay filtered as popups).
                let titled = if s.as_deref() == Some("AXDialog") {
                    let mut title_value: *const c_void = std::ptr::null();
                    if AXUIElementCopyAttributeValue(element, title_key, &mut title_value)
                        == K_AX_SUCCESS
                        && !title_value.is_null()
                    {
                        let t = cf_to_rust_string(title_value);
                        CFRelease(title_value);
                        t.is_some_and(|t| !t.is_empty())
                    } else {
                        false
                    }
                } else {
                    false
                };
                ax_subrole_kept(s.as_deref(), titled)
            } else {
                // 无 subrole → 视为标准窗口(部分 App 不设置此属性)。
                // No subrole means standard window for apps that don't set it.
                true
            };
            if !kept {
                continue;
            }

            let mut title_value: *const c_void = std::ptr::null();
            let title = if AXUIElementCopyAttributeValue(element, title_key, &mut title_value)
                == K_AX_SUCCESS
                && !title_value.is_null()
            {
                let t = cf_to_rust_string(title_value);
                CFRelease(title_value);
                t.unwrap_or_default()
            } else {
                String::new()
            };
            // AXMinimized:窗口是否最小化。无此属性(部分 App)按 false 处理。
            // AXMinimized: whether the window is minimized. Absent attribute (some apps) -> false.
            let minimized = {
                let mut min_value: *const c_void = std::ptr::null();
                if AXUIElementCopyAttributeValue(element, minimized_key, &mut min_value)
                    == K_AX_SUCCESS
                    && !min_value.is_null()
                {
                    let m = CFBooleanGetValue(min_value);
                    CFRelease(min_value);
                    m
                } else {
                    false
                }
            };
            // 取该 AX 窗口的 CGWindowID（私有 API），用于和 CG 窗口精确配对。
            let cgwid = ax_window_cgwid(element).unwrap_or(0);
            // 保留精确元素供激活路径复用,这样正常切换不必再次读取 AXWindows。
            // Retain the exact element for the activation path so normal raises do not need
            // another AXWindows round trip.
            cache_ax_window_element(pid, process_start_time_us, cgwid, element);
            results.push((cgwid, title, minimized));
        }
        CFRelease(title_key);
        CFRelease(subrole_key);
        CFRelease(minimized_key);
        CFRelease(windows_array);
        cache_ax_snapshot(pid, process_start_time_us, &results);
        Some(results)
    }
}

/// CFString -> Rust String(None = 转换失败)。窗口控制模块复用。
/// CFString -> Rust String (None = conversion failed). Reused by window control.
pub(crate) fn cf_to_rust_string(cf_string: *const c_void) -> Option<String> {
    let mut buf = vec![0u8; 1024];
    let ok = unsafe {
        CFStringGetCString(
            cf_string,
            buf.as_mut_ptr() as *mut i8,
            buf.len() as isize,
            0x08000100,
        )
    };
    if ok {
        let end = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
        Some(String::from_utf8_lossy(&buf[..end]).to_string())
    } else {
        None
    }
}

// ========== per-PID AX 收集(可并行)/ per-PID AX collection (parallelizable) ==========

/// 一个工作线程处理一段 PID 后的部分结果集,线程间按 key 合并——合并后与串行版
/// 逐字段一致(卡片顺序由第二遍 CG 数组遍历决定,与收集顺序无关)。
/// One worker thread's partial result set for its PID chunk; merged by key afterwards --
/// the merge equals the serial version field-for-field (card order is decided by the
/// second pass over the CG array and never depends on collection order).
struct AxPartial {
    icon_ids: HashMap<i32, AppIdentity>,
    ax_queried_pids: HashSet<i32>,
    ax_wid_to_info: HashMap<i32, HashMap<u32, (String, bool)>>,
    titleless_pids: HashSet<i32>,
    /// 本段所有 PID 的 AX 查询工作耗时之和(诊断用;墙钟由调用方测)。
    /// Sum of AX query work time for this chunk (diagnostics; wall clock is measured by the caller).
    ax_work_ms: u128,
}

/// 处理一段 PID:逐个解析应用身份 + 查询该应用的 AX 窗口列表。AX 远程查询支持
/// 多线程(messaging timeout 按 element 隔离);resolve_app_identity 只读
/// NSRunningApplication 属性 + stat。整段包 autoreleasepool 回收 ObjC 临时对象。
/// Process a chunk of PIDs: resolve each app identity + query its AX window list. AX
/// remote messaging works from any thread (messaging timeouts are per-element);
/// resolve_app_identity only reads NSRunningApplication properties + stat. The whole
/// chunk runs inside an autoreleasepool to drain ObjC temporaries.
///
/// # Safety
/// 调用方需保证无并发的 AX/ObjC 环境冲突(与主线程的 AX 使用互不共享元素)。
/// The caller must ensure no conflicting concurrent use of shared AX/ObjC elements with
/// the main thread.
unsafe fn ax_collect_chunk(chunk: &[i32], pid_names: &HashMap<i32, String>) -> AxPartial {
    let mut partial = AxPartial {
        icon_ids: HashMap::new(),
        ax_queried_pids: HashSet::new(),
        ax_wid_to_info: HashMap::new(),
        titleless_pids: HashSet::new(),
        ax_work_ms: 0,
    };
    // AppKit 临时对象(NSRunningApplication 等)随池回收;图标提取后台线程同款先例。
    // AppKit temporaries (NSRunningApplication et al) drain with the pool; same precedent
    // as the background icon-extraction thread.
    let pool: *mut AnyObject = msg_send![class!(NSAutoreleasePool), new];
    for &pid in chunk {
        let t_pid = Instant::now();
        let identity = unsafe { resolve_app_identity(pid) };
        let process_start_time_us = identity.process_start_time_us;
        partial.icon_ids.insert(pid, identity);
        let ax_wins = get_ax_windows_for_pid_with_identity(pid, process_start_time_us);
        let pid_ms = t_pid.elapsed().as_millis();
        partial.ax_work_ms += pid_ms;
        // AX 查询结果汇总:定位“一会 1 个一会 2 个”——设置窗口抖动时看 windows 数
        // 是否变化(缺窗口/失败/超时)。多线程下日志行可能交错,但每行自带 pid/app
        // 标识,logger 的 flume 通道保证行内原子输出。
        // AX query summary: pin down the 1-vs-2 flicker -- watch whether the window count
        // changes (missing windows / failure / timeout). Lines from parallel workers may
        // interleave, but each carries its own pid/app tag, and the logger's flume channel
        // keeps every line atomic.
        match &ax_wins {
            Some(w) => log_debug!(
                "[collect] ax pid={} app=\"{}\" windows={} ({}ms)",
                pid,
                pid_names.get(&pid).map(String::as_str).unwrap_or("?"),
                w.len(),
                pid_ms
            ),
            None => log_debug!(
                "[collect] ax pid={} app=\"{}\" FAILED (CG fallback, {}ms)",
                pid,
                pid_names.get(&pid).map(String::as_str).unwrap_or("?"),
                pid_ms
            ),
        }
        // TIMING-DEBUG 慢 AX 查询(≥20ms)单独标记:定位卡顿来自哪个应用(如 Ghostty)。
        // TIMING-DEBUG Flag slow AX queries (>=20ms) individually: pin down which app stalls
        // the summon.
        if pid_ms >= 20 {
            log_debug!(
                "[collect] ax slow: pid={} app=\"{}\" {}ms",
                pid,
                pid_names.get(&pid).map(String::as_str).unwrap_or("?"),
                pid_ms
            );
        }
        match ax_wins {
            Some(wins) if !wins.is_empty() => {
                partial.ax_queried_pids.insert(pid);
                if wins.iter().all(|(_, t, _)| t.is_empty()) {
                    partial.titleless_pids.insert(pid);
                }
                let mut wid_map: HashMap<u32, (String, bool)> = HashMap::new();
                for (cgwid, title, minimized) in &wins {
                    if *cgwid != 0 {
                        wid_map.insert(*cgwid, (title.clone(), *minimized));
                    }
                }
                partial.ax_wid_to_info.insert(pid, wid_map);
            }
            // AX 查询成功但无标准窗口:该 App 没有调度中心可见的窗口,后续直接跳过。
            // AX query succeeded but no standard windows: the app has no Mission-Control-visible
            // windows; skip all its CG windows in the second pass.
            Some(_) => {
                partial.ax_queried_pids.insert(pid);
            }
            // AX 查询失败(无 AX 数据):保留 CG 回退路径。
            // AX query failed (no AX data): keep the CG fallback path.
            None => {}
        }
    }
    let _: () = msg_send![pool, drain];
    partial
}

/// 只重新发现一个 PID 的窗口，用于 WindowServer 发现“未显示焦点窗口”后的定向刷新。
/// AX 仍是显示权威，CG 只负责提供当前窗口几何信息和候选集合。
/// Rediscover one PID's windows after WindowServer reports an undisplayed focused window.
/// AX remains authoritative for display; CG only supplies current geometry and candidates.
pub(crate) fn collect_windows_for_pid(
    mru: &mut MruMap,
    pid: i32,
    focused_cgwid: u32,
) -> Option<Vec<WindowInfo>> {
    unsafe {
        let pool: *mut AnyObject = msg_send![class!(NSAutoreleasePool), new];
        let result = collect_windows_for_pid_inner(mru, pid, focused_cgwid);
        let _: () = msg_send![pool, drain];
        result
    }
}

unsafe fn collect_windows_for_pid_inner(
    mru: &mut MruMap,
    pid: i32,
    focused_cgwid: u32,
) -> Option<Vec<WindowInfo>> {
    let show_minimized = CONFIG.read().unwrap().windows.show_minimized;
    let array = CGWindowListCopyWindowInfo(K_C_G_WINDOW_LIST_OPTION_ALL, 0);
    if array.is_null() {
        return None;
    }

    let Some(ax_wins) = get_ax_windows_for_pid(pid) else {
        CFRelease(array);
        return None;
    };
    let ax_wid_to_info: HashMap<u32, (String, bool)> = ax_wins
        .iter()
        .filter_map(|(window_id, title, minimized)| {
            (*window_id != 0).then_some((*window_id, (title.clone(), *minimized)))
        })
        .collect();
    let titleless = !ax_wins.is_empty() && ax_wins.iter().all(|(_, title, _)| title.is_empty());
    let identity = resolve_app_identity(pid);
    let icon_path = check_cache_for_identity(&identity);
    let last_activated = LAST_ACTIVATED.lock().unwrap().get(&pid).copied();
    let now = Instant::now();
    let ancient_base = now.checked_sub(Duration::from_secs(86_400)).unwrap_or(now);
    let mut insertion_order = 0;
    let mut app_name = String::new();
    let mut shown = HashSet::new();
    let mut current_cg_ids = HashSet::new();
    let mut cg_window_layers: HashMap<u32, i32> = HashMap::new();
    let mut windows = Vec::new();
    let count = CFArrayGetCount(array);

    for i in 0..count {
        let dict = CFArrayGetValueAtIndex(array, i);
        if dict.is_null() {
            continue;
        }
        let layer = cf_dict_get_i32(dict, "kCGWindowLayer").unwrap_or(999);
        let owner_pid = cf_dict_get_i32(dict, "kCGWindowOwnerPID").unwrap_or(-1);
        if owner_pid != pid {
            continue;
        }
        let cgwid = cf_dict_get_u32(dict, "kCGWindowNumber").unwrap_or(0);
        if cgwid != 0 {
            cg_window_layers.insert(cgwid, layer);
        }
        if layer != 0 {
            continue;
        }
        if is_known_non_normal_window(pid, identity.process_start_time_us, cgwid) {
            continue;
        }
        if cf_dict_get_f64(dict, "kCGWindowAlpha").unwrap_or(1.0) <= 0.0 {
            continue;
        }
        let owner_name = cf_dict_get_string(dict, "kCGWindowOwnerName").unwrap_or_default();
        if owner_name.is_empty() || owner_name == "Dock" {
            continue;
        }
        if cgwid != 0 {
            current_cg_ids.insert(cgwid);
        }
        let bounds = cf_dict_get_bounds(dict, "kCGWindowBounds").unwrap_or((0.0, 0.0, 0.0, 0.0));
        let Some((window_title, minimized)) = ax_wid_to_info.get(&cgwid) else {
            continue;
        };
        if pid == std::process::id() as i32
            && !cf_dict_get_bool(dict, "kCGWindowIsOnscreen").unwrap_or(false)
        {
            continue;
        }
        if !show_minimized && *minimized {
            continue;
        }
        if window_title.is_empty() && !titleless {
            continue;
        }

        initialize_window_mru(
            mru,
            pid,
            cgwid,
            last_activated,
            ancient_base,
            insertion_order,
        );
        insertion_order += 1;
        app_name = owner_name;
        windows.push(WindowInfo {
            pid,
            window_id: cgwid,
            app_name: app_name.clone(),
            window_title: window_title.clone(),
            icon_path: icon_path.clone(),
            is_active: false,
            minimized: *minimized,
            bounds,
        });
        shown.insert(cgwid);
    }
    remember_non_normal_cg_windows_for_process(
        pid,
        identity.process_start_time_us,
        &cg_window_layers,
    );
    CFRelease(array);

    // CGWindowList 里没有的 AX 窗口仍然是合法窗口,例如 orderOut 的设置对话框。
    // AX-only windows remain valid, for example orderOut'd settings dialogs absent from CG.
    if pid != std::process::id() as i32 {
        for (&cgwid, (window_title, minimized)) in &ax_wid_to_info {
            if shown.contains(&cgwid) || (!show_minimized && *minimized) {
                continue;
            }
            if !should_backfill_ax_window_for_process(
                pid,
                cgwid,
                identity.process_start_time_us,
                cg_window_layers.get(&cgwid).copied(),
            ) {
                continue;
            }
            if window_title.is_empty() && !titleless {
                continue;
            }
            initialize_window_mru(
                mru,
                pid,
                cgwid,
                last_activated,
                ancient_base,
                insertion_order,
            );
            insertion_order += 1;
            windows.push(WindowInfo {
                pid,
                window_id: cgwid,
                app_name: app_name.clone(),
                window_title: window_title.clone(),
                icon_path: icon_path.clone(),
                is_active: false,
                minimized: *minimized,
                bounds: (0.0, 0.0, 0.0, 0.0),
            });
        }
    }

    if app_name.is_empty() {
        let app: *mut AnyObject = msg_send![
            class!(NSRunningApplication),
            runningApplicationWithProcessIdentifier: pid
        ];
        app_name = crate::ffi::ns_running_app_name(app);
        if app_name.is_empty() {
            app_name = format!("PID {pid}");
        }
        for window in &mut windows {
            window.app_name = app_name.clone();
        }
    }

    // 定向收集只清理目标 PID 的死亡窗口,不能修剪其他 PID 的 MRU。
    // Directed collection prunes dead windows only for the target PID; it must not prune other PIDs.
    // AX 暂时漏报时，当前 CG 仍存活的已知窗口不能丢掉 MRU 时间；否则它重新出现会像新窗口。
    // Preserve MRU for known windows still alive in CG when AX transiently omits them; otherwise
    // they return as "new" windows and lose their ordering history.
    let live_ids: HashSet<u32> = current_cg_ids
        .into_iter()
        .chain(ax_wid_to_info.keys().copied().filter(|cgwid| {
            should_backfill_ax_window_for_process(
                pid,
                *cgwid,
                identity.process_start_time_us,
                cg_window_layers.get(cgwid).copied(),
            ) || is_known_non_normal_window(pid, identity.process_start_time_us, *cgwid)
        }))
        .collect();
    mru.retain(|(entry_pid, window_id), _| *entry_pid != pid || live_ids.contains(window_id));
    sort_windows_by_mru(&mut windows, mru, now);
    for window in &mut windows {
        window.is_active = window.window_id == focused_cgwid;
    }
    Some(windows)
}

pub fn collect_windows(mru: &mut MruMap) -> Vec<WindowInfo> {
    collect_windows_with_frontmost_bump(mru, true)
}

/// 收集窗口快照,可选择是否把当前前台窗口写入 MRU。
/// 生命周期事件触发的刷新只负责更新窗口集合,不能把自身误当成 summon。
/// Collect a window snapshot, optionally recording the current frontmost window in MRU.
/// Lifecycle-triggered refreshes only update the window set and must not act like a summon.
pub(crate) fn collect_windows_with_frontmost_bump(
    mru: &mut MruMap,
    bump_frontmost: bool,
) -> Vec<WindowInfo> {
    let show_minimized = CONFIG.read().unwrap().windows.show_minimized;
    // 始终用 All 枚举(含离屏窗口)。原因:部分应用(如 JetBrains 系 IDE)在"主窗口被
    // 激活"时会把设置对话框 orderOut(隐藏但保留窗口对象)——它 isOnscreen=false,
    // OnScreenOnly 枚举不到,切换器就会"切到主窗口后设置窗口消失"(BetterCmdTab 用
    // All 枚举,无此问题)。离屏窗口是否显示改由 AX 语义决定(见下文的过滤逻辑):
    // AX 仍报的窗口是合法可切换窗口(调度中心/系统 Cmd+Tab 也认),AX 不报的
    // 隐藏辅助窗口会被 subrole/空标题过滤拦掉。
    // Always enumerate with All (off-screen windows included). Some apps (JetBrains IDEs)
    // orderOut their settings dialog when the main window is activated -- it then has
    // isOnscreen=false, invisible to OnScreenOnly, so the switcher would lose it after
    // switching to the main window (BetterCmdTab uses All; no such issue). Whether an
    // off-screen window shows is now decided by AX semantics (see the filter below): a
    // window AX still reports is a legitimate switchable window (Mission Control and the
    // system Cmd+Tab agree); hidden helper surfaces AX never reports are dropped by the
    // subrole/empty-title filters.
    let cg_option = K_C_G_WINDOW_LIST_OPTION_ALL;
    let array = unsafe { CGWindowListCopyWindowInfo(cg_option, 0) };
    if array.is_null() {
        return vec![];
    }

    // 不再按 PID 排除本应用(own-PID)窗口:设置窗口也是 own-PID,排除它会导致设置
    // 开着时切不到它。浮窗自己不需要靠 PID 排除--它使用非 0 的 overlay 层级,
    // kCGWindowLayer != 0,已被下面的 layer 过滤挡掉(与枚举模式无关)。设置窗口
    // 关着时 orderOut 离屏,由下文的 own-PID isOnscreen 过滤排除,故
    // "开->显示为卡片、关->不显示"仍然成立。
    //
    // Own-PID windows are no longer excluded by PID: the settings window is own-PID too, and
    // excluding it would make it unswitchable while open. The overlay itself needs no PID
    // exclusion -- it uses a non-zero overlay level and is dropped by the layer check below
    // (independent of the enumeration mode). The settings window, when
    // closed, is orderOut'd (off-screen) and excluded by the own-PID isOnscreen filter
    // below, so "open -> shown as a card, closed -> hidden" still holds.
    let mut windows: Vec<WindowInfo> = Vec::new();
    // CG 循环里已显示的窗口集合:AX 补漏时跳过(避免重复卡片)。
    // Windows already shown by the CG loop: skipped by the AX backfill (no duplicate rows).
    let mut shown: HashSet<(i32, u32)> = HashSet::new();
    // TIMING-DEBUG 阶段计时(debug 档):定位 summon 卡顿——CG 枚举 / 每 PID AX 查询 / frontmost。
    // TIMING-DEBUG Phase timings (debug tier): locate summon stalls -- CG enumeration /
    // per-PID AX queries / the frontmost lookup. Remove together with the [collect] logs.
    let t0 = Instant::now();
    let count = unsafe { CFArrayGetCount(array) };
    let now = Instant::now();
    let ancient_base = now.checked_sub(Duration::from_secs(86_400)).unwrap_or(now);
    // 一次收集使用同一激活快照，避免同批窗口因通知并发到达而得到两套时间基准。
    // One collection uses one activation snapshot so concurrent notifications cannot give
    // windows in the same batch two different time bases.
    let last_activated = LAST_ACTIVATED.lock().unwrap().clone();
    let t_cg_ms = t0.elapsed().as_millis();
    let mut insertion_order: u32 = 0;

    // 第一遍遍历：收集所有 PID，用于批量查询 AX 窗口
    // First pass: collect all PIDs to batch query AX windows
    let mut pids: HashSet<i32> = HashSet::new();
    // Snapshot every CG window's layer so AX backfill can distinguish orderOut'd windows from
    // app-owned overlays that were intentionally filtered by the normal layer-0 pass.
    // 记录所有 CG 窗口的层级,让 AX 补漏区分合法 orderOut 窗口和被 layer-0 遍历主动过滤的应用浮层。
    let mut cg_window_layers: HashMap<(i32, u32), i32> = HashMap::new();
    // TIMING-DEBUG pid -> 应用名(慢 AX 日志用)/ pid -> app name (for the slow-AX log).
    let mut pid_names: HashMap<i32, String> = HashMap::new();
    for i in 0..count {
        let dict = unsafe { CFArrayGetValueAtIndex(array, i) };
        if dict.is_null() {
            continue;
        }
        let owner_pid = cf_dict_get_i32(dict, "kCGWindowOwnerPID").unwrap_or(-1);
        if owner_pid <= 0 {
            continue;
        }
        let layer = cf_dict_get_i32(dict, "kCGWindowLayer").unwrap_or(999);
        let cgwid = cf_dict_get_u32(dict, "kCGWindowNumber").unwrap_or(0);
        if cgwid != 0 {
            cg_window_layers.insert((owner_pid, cgwid), layer);
        }
        if layer != 0 {
            continue;
        }
        let owner_name = cf_dict_get_string(dict, "kCGWindowOwnerName").unwrap_or_default();
        if owner_name.is_empty() || owner_name == "Dock" {
            continue;
        }
        pid_names.insert(owner_pid, owner_name);
        pids.insert(owner_pid);
    }

    // 以 AX 窗口列表为主数据源（macOS App Switcher 的做法）
    // Use AX window list as primary source (same as macOS App Switcher)
    // pid -> 缓存身份(bundle id + mtime)。AX 循环里按 pid 解析一次,供第二遍按窗口查缓存,
    // 避免每个窗口都做一次 NSRunningApplication 查找(一个 App 多窗口时尤其浪费)。
    // pid -> cache identity (bundle id + mtime). Resolved once per pid in the AX phase so the
    // second pass can look up the cache per-window without an NSRunningApplication call each time
    // (wasteful when one app has many windows).
    let mut icon_ids: HashMap<i32, AppIdentity> = HashMap::new();
    // AX 收集阶段(并行):把 PID 分成 K 组(K = min(逻辑核数, PID 数),运行时自适应
    // 任意 M 系列芯片),各组在工作线程同时查询——AX 远程消息支持多线程,每组结果
    // 按 key 合并后与串行版逐字段一致。墙钟时间从"所有 PID 之和"降为"最慢单 PID"
    // (微信这类对 AX 提问反复磨蹭的应用曾是串行总耗时的主导项,实测单 PID 可达 1.5s)。
    // The AX collection phase (parallel): split PIDs into K chunks (K = min(logical cores,
    // PID count), runtime-adaptive to any Apple Silicon variant) queried simultaneously on
    // worker threads -- AX remote messaging is multi-thread-capable and each chunk merges
    // by key into a result identical to the serial version. Wall clock drops from "sum of
    // all PIDs" to "slowest single PID" (apps like WeChat that stall on every AX question
    // used to dominate serial totals; up to 1.5s for one PID in logs).
    let mut ax_wid_to_info: HashMap<i32, HashMap<u32, (String, bool)>> = HashMap::new();
    // AX 查询「成功」的 pid 集合:成功但无标准窗口的 App 应整体跳过(调度中心不显示它),
    // 只有查询失败(None)才允许走 CG 回退。这是 BetterDisplay 隐形窗口 bug 的根因修复:
    // AX 成功但 subrole 过滤后为空,不能等同于「无 AX 数据」。
    // Pids whose AX query SUCCEEDED: an app with a successful query but no standard windows
    // must be skipped entirely (Mission Control doesn't show it); only a failed query (None)
    // allows the CG fallback. This fixes the BetterDisplay invisible-window bug: an AX query
    // that succeeds but yields no standard windows must not be treated as "no AX data".
    let mut ax_queried_pids: HashSet<i32> = HashSet::new();
    // AX 窗口「全部」为空标题的 App（如 Microsoft To Do：自绘标题栏 -> AXTitle 为空）。
    // 这类 App 的空标题窗口是真实主窗口，不能被当作弹出面板丢弃。
    // Apps whose AX windows are ALL untitled (e.g. Microsoft To Do, which has a
    // custom title bar and an empty AXTitle). Their titleless windows are real
    // main windows and must not be dropped as popups.
    let mut titleless_pids: HashSet<i32> = HashSet::new();
    let t_ax = Instant::now();
    {
        let pid_list: Vec<i32> = pids.iter().copied().collect();
        // AX is an IPC service implemented by the target apps, not CPU-bound work. Keep a
        // small fixed cap so a large PID set does not overload WindowServer/AX servers and turn
        // the first frame into a synchronized timeout storm.
        const MAX_AX_WORKERS: usize = 4;
        let partials: Vec<AxPartial> = if pid_list.is_empty() {
            Vec::new()
        } else {
            let workers = std::thread::available_parallelism()
                .map(|n| n.get())
                .unwrap_or(4)
                .min(pid_list.len())
                .clamp(1, MAX_AX_WORKERS);
            let chunk_size = pid_list.len().div_ceil(workers);
            std::thread::scope(|scope| {
                let handles: Vec<_> = pid_list
                    .chunks(chunk_size)
                    .map(|chunk| scope.spawn(|| unsafe { ax_collect_chunk(chunk, &pid_names) }))
                    .collect();
                handles
                    .into_iter()
                    .map(|h| h.join().expect("ax collect worker panicked"))
                    .collect()
            })
        };
        // 按 key 合并各分段;ax_wid_to_info 的键互不相交(每 PID 只属于一段),
        // 合并顺序不影响最终内容。
        // Merge chunks by key; ax_wid_to_info keys are disjoint (each PID lives in exactly
        // one chunk), so merge order cannot affect the outcome.
        for p in partials {
            icon_ids.extend(p.icon_ids);
            ax_queried_pids.extend(p.ax_queried_pids);
            ax_wid_to_info.extend(p.ax_wid_to_info);
            titleless_pids.extend(p.titleless_pids);
        }
    }
    // TIMING-DEBUG AX 阶段墙钟(并行后 ≠ 各 PID 工作耗时之和;单 PID 工作耗时见
    // 各 "ax pid=" 行的 ms 值)。
    // TIMING-DEBUG Wall clock of the parallel AX phase (NOT the sum of per-PID work
    // times anymore; per-PID work shows in each "ax pid=" line's ms value).
    let ax_total_ms = t_ax.elapsed().as_millis();
    remember_non_normal_cg_windows(&cg_window_layers, &icon_ids);

    for i in 0..count {
        let dict = unsafe { CFArrayGetValueAtIndex(array, i) };
        if dict.is_null() {
            continue;
        }

        let layer = cf_dict_get_i32(dict, "kCGWindowLayer").unwrap_or(999);
        if layer != 0 {
            continue;
        }

        // 全透明窗口(alpha=0)不可见,调度中心不显示,跳过。
        // Fully transparent windows (alpha=0) are invisible; Mission Control doesn't show them.
        let alpha = cf_dict_get_f64(dict, "kCGWindowAlpha").unwrap_or(1.0);
        if alpha <= 0.0 {
            continue;
        }

        let owner_pid = cf_dict_get_i32(dict, "kCGWindowOwnerPID").unwrap_or(-1);
        if owner_pid <= 0 {
            continue;
        }

        let owner_name = cf_dict_get_string(dict, "kCGWindowOwnerName").unwrap_or_default();
        if owner_name.is_empty() || owner_name == "Dock" {
            continue;
        }

        let cg_title = cf_dict_get_string(dict, "kCGWindowName").unwrap_or_default();
        let cgwid = cf_dict_get_u32(dict, "kCGWindowNumber").unwrap_or(0);
        if is_known_non_normal_window(
            owner_pid,
            icon_ids
                .get(&owner_pid)
                .and_then(|identity| identity.process_start_time_us),
            cgwid,
        ) {
            log_debug!(
                "[collect] sticky non-normal layer: pid={} app=\"{}\" cgwid={} -> dropped",
                owner_pid,
                owner_name,
                cgwid
            );
            continue;
        }
        // bounds (x, y, w, h);解析失败时全 0,调用方会回退到主屏幕。
        // bounds (x, y, w, h); all zeros on parse failure, caller falls back to the main screen.
        let bounds = cf_dict_get_bounds(dict, "kCGWindowBounds").unwrap_or((0.0, 0.0, 0.0, 0.0));

        // 按 CGWindowID 精确配对 AX 标题（不再按顺序/字符串猜）。
        // AX 为权威数据源：CG 窗口必须能在 AX 里按 CGWindowID 找到才保留。
        // Pair the AX title by CGWindowID (no more order/string guessing).
        // AX is authoritative: a CG window is kept only if AX has a window with
        // the same CGWindowID.
        let (window_title, minimized) = if let Some(wid_map) = ax_wid_to_info.get(&owner_pid) {
            match wid_map.get(&cgwid) {
                Some((t, m)) => (t.clone(), *m),
                None => {
                    // 配对失败:CG 窗口在 AX 列表里没有(弹出面板 或 AX 漏报)。
                    // 记日志定位设置窗口“时有时无”:抖动时这里应出现它的 cgwid。
                    // Pair miss: the CG window has no AX counterpart (a popup, or AX
                    // failed to report it). Logged to pin the Settings-window flicker.
                    log_debug!(
                        "[collect] ax pair-miss: pid={} app=\"{}\" cgwid={} {:.0}x{:.0} -> dropped",
                        owner_pid,
                        owner_name,
                        cgwid,
                        bounds.2,
                        bounds.3
                    );
                    continue; // CG 窗口在 AX 里没有 -> 弹出面板，跳过 / popup, skip
                }
            }
        } else if ax_queried_pids.contains(&owner_pid) {
            // AX 查询成功但该 App 无标准窗口(如 BetterDisplay 的隐形锚点窗口):
            // 调度中心不显示它,这里也整体跳过,不回退到 CG。
            // AX query succeeded but the app has no standard windows (e.g. BetterDisplay's
            // invisible anchor window): Mission Control doesn't show it, skip entirely —
            // no CG fallback.
            continue;
        } else {
            // 该 App 无 AX 数据 -> 退回 CG 标题,最小化状态未知按 false。
            // No AX data -> fall back to CG title; minimized status unknown, assume false.
            (cg_title, false)
        };

        // 枚举已改为 All(见 cg_option 注释),离屏窗口(orderOut 的对话框/最小化/其他
        // Space)全部在列表里,显示与否由以下规则决定:
        // - 本应用自己的窗口(设置窗口):orderOut = 关闭,按 isOnscreen 过滤——
        //   开->显示、关->不显示,维持原行为(其他应用的 orderOut 窗口不能用此规则,
        //   它们可能是 JetBrains 那种"隐藏但合法"的设置对话框)。
        // - 其他应用的窗口:AX 配对成功的都显示——orderOut 但 AX 仍报的窗口
        //   (JetBrains 激活主窗口时隐藏的设置对话框)是合法可切换窗口必须显示;
        //   最小化窗口由"显示最小化窗口"开关控制(AX minimized 标记,与 isOnscreen
        //   无关——最小化窗口 isOnscreen=false 但 minimized=true)。
        // The enumeration is now All (see the cg_option comment): off-screen windows
        // (orderOut'd dialogs / minimized / other Spaces) are all listed, and whether they
        // show is decided here:
        // - own windows (the settings window): orderOut = closed, filtered by isOnscreen --
        //   open -> shown, closed -> hidden, preserving the original behavior (other apps'
        //   orderOut'd windows can't use this rule; they may be JetBrains-style hidden-but-
        //   legitimate settings dialogs).
        // - other apps' windows: every AX-paired window shows -- an orderOut'd window AX
        //   still reports (JetBrains hiding its settings dialog on main-window activation)
        //   is a legitimate switchable window and must show; minimized windows are gated
        //   by the "show minimized windows" switch (the AX minimized flag -- unrelated to
        //   isOnscreen: a minimized window is isOnscreen=false but minimized=true).
        if owner_pid == std::process::id() as i32 {
            let is_onscreen = cf_dict_get_bool(dict, "kCGWindowIsOnscreen").unwrap_or(false);
            if !is_onscreen {
                continue;
            }
        } else if !show_minimized && minimized {
            continue;
        }

        // 空标题窗口仅对「AX 确认过全部窗口无标题」的 App(titleless_pids)保留;
        // AX 查询失败回退的 App 不再享受空标题豁免(无法确认其窗口身份)。
        // Titleless windows are kept only for apps AX confirmed as all-untitled
        // (titleless_pids); the AX-failed fallback no longer exempts empty titles
        // (window identity can't be verified there).
        if window_title.is_empty() && !titleless_pids.contains(&owner_pid) {
            continue;
        }

        // mru 按 (pid, CGWindowID) 索引——CGWindowID 在窗口生命周期内稳定不变，
        // 比 title 更可靠（title 会随浏览器标签页切换而变）。新窗口优先以 App 最近
        // 激活时间初始化；已有条目绝不覆盖，因此旧同 App 窗口不会搭便车。
        // Key mru by (pid, CGWindowID) — CGWindowID is stable for the window's
        // lifetime, more reliable than title (which changes with browser tabs). New windows
        // prefer the app's latest activation as their seed; existing entries are never
        // overwritten, so old same-app windows cannot ride along.
        initialize_window_mru(
            mru,
            owner_pid,
            cgwid,
            last_activated.get(&owner_pid).copied(),
            ancient_base,
            insertion_order,
        );
        insertion_order += 1;
        let icon_path = icon_ids.get(&owner_pid).and_then(check_cache_for_identity);
        // TIMING-DEBUG 图标缓存 miss 标记:排查 summon 卡顿——哪些 app 会触发同步提取。
        // TIMING-DEBUG Flag icon-cache misses: which apps trigger the synchronous extract.
        if icon_path.is_none() {
            log_debug!(
                "[collect] icon miss: pid={} app=\"{}\"",
                owner_pid,
                owner_name
            );
        }
        windows.push(WindowInfo {
            pid: owner_pid,
            window_id: cgwid,
            app_name: owner_name,
            window_title,
            icon_path,
            is_active: false,
            minimized,
            bounds,
        });
        shown.insert((owner_pid, cgwid));
    }

    // AX 补漏:AX 窗口列表报、但 CG 枚举没有的窗口。例:JetBrains 系 IDE 在“主窗口
    // 被激活”时把设置对话框 orderOut(隐藏但保留窗口对象)——orderOut 的窗口不在
    // CGWindowList 里(optionAll 也只含屏上窗口),按 CG 遍历永远看不到它;但 AX 仍
    // 报它,且它是合法可切换窗口(BetterCmdTab 以 AX 列表为主数据源,故稳定显示)。
    // 这里用 AX 的标题/minimized 补出条目;bounds 未知(离屏),调用方回退主屏幕。
    // AX backfill: windows AX reports but the CG enumeration lacks. E.g. JetBrains IDEs
    // orderOut their settings dialog when the main window is activated -- an orderOut'd
    // window is NOT in CGWindowList (optionAll only covers on-screen windows), so a
    // CG-driven loop can never see it; AX still reports it and it is a legitimate
    // switchable window (BetterCmdTab uses the AX list as its primary source and shows
    // it stably). Entries are built from AX title/minimized; bounds are unknown
    // (off-screen), callers fall back to the main screen.
    for (&pid, wid_map) in ax_wid_to_info.iter() {
        // 本应用窗口(设置窗口)走 CG 路径的 isOnscreen 过滤,这里不补:
        // 关闭(orderOut)时不应出现在切换器里。
        // Own windows (the settings window) go through the CG isOnscreen filter; not
        // backfilled here: when closed (orderOut) they must not show.
        if pid == std::process::id() as i32 {
            continue;
        }
        let mut entries: Vec<(u32, &String, bool)> =
            wid_map.iter().map(|(w, (t, m))| (*w, t, *m)).collect();
        entries.sort_by_key(|(w, _, _)| *w);
        for (cgwid, title, minimized) in entries {
            if shown.contains(&(pid, cgwid)) {
                continue;
            }
            let process_start_time_us = icon_ids
                .get(&pid)
                .and_then(|identity| identity.process_start_time_us);
            if !should_backfill_ax_window_for_process(
                pid,
                cgwid,
                process_start_time_us,
                cg_window_layers.get(&(pid, cgwid)).copied(),
            ) {
                let layer = cg_window_layers.get(&(pid, cgwid)).copied();
                log_debug!(
                    "[collect] ax-only skipped non-normal layer: pid={} app=\"{}\" cgwid={} layer={:?}",
                    pid,
                    pid_names.get(&pid).map(String::as_str).unwrap_or("?"),
                    cgwid,
                    layer
                );
                continue;
            }
            // 与 CG 路径同款过滤:最小化由开关控制;空标题(非 titleless)无意义。
            // Same filters as the CG path: minimized gated by the switch; empty titles
            // (not titleless) are meaningless.
            if !show_minimized && minimized {
                continue;
            }
            if title.is_empty() && !titleless_pids.contains(&pid) {
                continue;
            }
            initialize_window_mru(
                mru,
                pid,
                cgwid,
                last_activated.get(&pid).copied(),
                ancient_base,
                insertion_order,
            );
            insertion_order += 1;
            let icon_path = icon_ids.get(&pid).and_then(check_cache_for_identity);
            log_debug!(
                "[collect] ax-only window restored: pid={} app=\"{}\" cgwid={}",
                pid,
                pid_names.get(&pid).map(String::as_str).unwrap_or("?"),
                cgwid
            );
            windows.push(WindowInfo {
                pid,
                window_id: cgwid,
                app_name: pid_names.get(&pid).cloned().unwrap_or_default(),
                window_title: title.clone(),
                icon_path,
                is_active: false,
                minimized,
                bounds: (0.0, 0.0, 0.0, 0.0),
            });
        }
    }

    // 修剪 MRU:只保留存活窗口的条目。存活集用 All 模式枚举(含最小化/离屏窗口),
    // 不用显示列表——OnScreenOnly 看不到最小化窗口,按显示列表修剪会清掉它们的
    // 排序记忆。已关闭窗口的残留条目不清的话,系统复用 CGWindowID 时新窗口会
    // or_insert 命中旧时间戳、按旧窗口的时间排序(继承污染)。存活集只做最保守
    // 过滤(layer 0 + 有效 pid),宁全勿缺;显示层的过滤(AX 配对/Dock/alpha)不适用。
    // 代价:show_minimized 关闭时每次 summon 补一次 All 枚举(亚毫秒级)。
    //
    // Prune MRU to the live window set. The live set is enumerated in All mode (includes
    // minimized/off-screen windows) rather than the display list -- OnScreenOnly can't see
    // minimized windows, so pruning against the display list would wipe their ordering
    // memory. Without pruning, a recycled CGWindowID would make a new window or_insert
    // the dead window's timestamp (inheritance pollution). The live set uses only the most
    // conservative filters (layer 0 + valid pid) -- better to keep than to drop; display
    // filters (AX pairing / Dock / alpha) don't apply here. Cost: one extra All-mode
    // enumeration per summon when show_minimized is off (sub-millisecond).
    let mut live_set: HashSet<(i32, u32)> = {
        let src = if show_minimized {
            array // 主查询已是 All 模式,直接复用 / main query is already All mode
        } else {
            unsafe { CGWindowListCopyWindowInfo(K_C_G_WINDOW_LIST_OPTION_ALL, 0) }
        };
        let mut s: HashSet<(i32, u32)> = HashSet::new();
        if !src.is_null() {
            let n = unsafe { CFArrayGetCount(src) };
            for i in 0..n {
                let dict = unsafe { CFArrayGetValueAtIndex(src, i) };
                if dict.is_null() {
                    continue;
                }
                if cf_dict_get_i32(dict, "kCGWindowLayer").unwrap_or(999) != 0 {
                    continue;
                }
                let pid = cf_dict_get_i32(dict, "kCGWindowOwnerPID").unwrap_or(-1);
                if pid <= 0 {
                    continue;
                }
                let cgwid = cf_dict_get_u32(dict, "kCGWindowNumber").unwrap_or(0);
                s.insert((pid, cgwid));
            }
        }
        if !show_minimized && !src.is_null() {
            unsafe { CFRelease(src) };
        }
        s
    };
    // AX 仍报但 CG 枚举不到的窗口(orderOut 的合法窗口)也是存活窗口:并入存活集,
    // 否则 MRU 修剪会清掉它们的排序记忆(下次 summon 又按新窗口排到末尾)。
    // AX-reported windows missing from the CG enumeration (orderOut'd but legitimate)
    // count as alive too: merge them in, or the MRU prune would wipe their ordering
    // memory (they would re-sort to the tail as "new" windows next summon).
    for (&pid, wid_map) in ax_wid_to_info.iter() {
        let process_start_time_us = icon_ids
            .get(&pid)
            .and_then(|identity| identity.process_start_time_us);
        for &cgwid in wid_map.keys() {
            if should_backfill_ax_window_for_process(
                pid,
                cgwid,
                process_start_time_us,
                cg_window_layers.get(&(pid, cgwid)).copied(),
            ) || is_known_non_normal_window(pid, process_start_time_us, cgwid)
            {
                live_set.insert((pid, cgwid));
            }
        }
    }
    let pruned = prune_mru(mru, &live_set);
    // 修剪数量可观测(debug 档):有残留才打,平时每次 summon 无噪音。
    // Pruning is observable (debug tier): logged only when entries were dropped, so a
    // normal summon stays silent.
    if pruned > 0 {
        log_debug!("[windows] pruned {} stale MRU entries", pruned);
    }

    unsafe { CFRelease(array) };

    let (front_pid, frontmost, fm_ms): (Option<i32>, Option<(i32, u32)>, u128) = if bump_frontmost {
        // 只有召唤刷新才读取并 bump 当前前台窗口;生命周期刷新不能因临时窗口创建而改写 MRU。
        // Only summon refreshes read and bump the frontmost window; a transient lifecycle
        // window must not rewrite MRU just because it caused a refresh.
        let t_fm = Instant::now();
        let result = unsafe {
            let workspace: *mut AnyObject = msg_send![class!(NSWorkspace), sharedWorkspace];
            let front_app: *mut AnyObject = msg_send![workspace, frontmostApplication];
            if !front_app.is_null() {
                let front_pid: i32 = msg_send![front_app, processIdentifier];
                (
                    Some(front_pid),
                    focused_window_cgwid(front_pid).map(|cgwid| (front_pid, cgwid)),
                )
            } else {
                (None, None)
            }
        };
        (result.0, result.1, t_fm.elapsed().as_millis())
    } else {
        (None, None, 0)
    };
    if bump_frontmost {
        // 前台 app 的 AX 聚焦窗口查询也单独计时(从慢响应 app 切出时这里是第二个等待点)。
        // The frontmost app's AX focused-window query is timed too (a second wait when leaving a
        // slow-responding app).
        if let Some((pid, _)) = frontmost {
            log_debug!(
                "[collect] frontmost pid={} app=\"{}\" {}ms",
                pid,
                pid_names.get(&pid).map(String::as_str).unwrap_or("?"),
                fm_ms
            );
        }
        if let Some((pid, cgwid)) = frontmost {
            mru.insert((pid, cgwid), now);
        } else if let Some((pid, cgwid)) = frontmost_fallback(&windows, front_pid) {
            // 回退严格限制在系统前台 App 内,避免 AX 失败时把其他 App 的 CG 首项误刷为最新。
            // Restrict fallback to the system frontmost app so an AX failure cannot bump another
            // app's first CG item as most recent.
            mru.insert((pid, cgwid), now);
        }
    }

    // 纯窗口级 MRU 排序：每个窗口独立按最后被激活的时间排序。
    // LAST_ACTIVATED 只为新窗口生成一次初始窗口时间；已有窗口不更新，避免从 App C
    // 切到浏览器窗口 A 时，浏览器的旧窗口 B 也搭便车排到 C 前面。
    // Pure window-level MRU sort: each window is sorted independently by
    // when it was last activated. LAST_ACTIVATED only seeds a newly discovered window once;
    // existing windows are not updated, preventing browser window B from riding A's coattails
    // when switching from app C to browser window A.
    sort_windows_by_mru(&mut windows, mru, now);

    if let Some(first) = windows.first_mut() {
        first.is_active = true;
    }
    // TIMING-DEBUG 汇总:总耗时 + 各阶段(排查 summon 卡顿用)。
    // TIMING-DEBUG Summary: total + per-phase timings (for summon-stall diagnosis).
    let total_ms = t0.elapsed().as_millis();
    log_debug!(
        "[collect] total={}ms cg={}ms ax={}ms frontmost={}ms",
        total_ms,
        t_cg_ms,
        ax_total_ms,
        fm_ms
    );
    windows
}

#[cfg(test)]
mod tests {
    use super::*;

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
        assert!(ax_subrole_kept(Some("AXStandardWindow"), false));
        assert!(ax_subrole_kept(Some("AXStandardWindow"), true));
        // AXDialog(JetBrains 主窗口):必须带非空标题。
        // AXDialog (JetBrains main windows): must be titled.
        assert!(ax_subrole_kept(Some("AXDialog"), true));
        assert!(!ax_subrole_kept(Some("AXDialog"), false));
        // 弹窗/面板/隐形窗口:一律过滤。
        // Popups/panels/invisible windows: always filtered.
        assert!(!ax_subrole_kept(Some("AXUnknown"), true));
        assert!(!ax_subrole_kept(Some("AXSheet"), true));
        assert!(!ax_subrole_kept(Some("AXDrawer"), true));
        // 无 subrole(部分 App 不设置)→ 视为标准窗口。
        // Missing subrole (some apps don't set it) -> standard.
        assert!(ax_subrole_kept(None, false));
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
