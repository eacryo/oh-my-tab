mod autostart;
mod clipboard;
mod clipboard_highlight;
mod config;
mod event_monitor;
mod event_tap;
mod ffi;
mod i18n;
mod logger;
mod mem;
mod menu;
mod mouse;
mod overlay;
mod settings;
mod theme;
mod thumbnail;
mod window_collector;
mod window_server;

use config::CONFIG;
use i18n::t;
// FFI 基础工具(make_nsstring/release_obj/CFRelease/ObjPtr/颜色图层 helper 等)集中在 ffi.rs
// FFI primitives (make_nsstring/release_obj/CFRelease/ObjPtr/color+layer helpers) live in ffi.rs
use ffi::*;
// 主题与布局(Colors/配色/卡片窗口尺寸访问器/STATUS_H/H_PADDING)集中在 theme.rs
// Theme and layout (Colors/colors/card+window size accessors/STATUS_H/H_PADDING) live in theme.rs
use theme::*;
// 切换器浮窗与卡片 UI(浮窗状态/卡片索引/回调/渲染/激活)集中在 overlay.rs
// Switcher overlay & card UI (overlay state/card index/callbacks/rendering/activation) live in overlay.rs
use overlay::*;
// 状态栏菜单(菜单项状态/动作回调/标题刷新)集中在 menu.rs
// Status bar menu (menu-item state/action callbacks/title refresh) live in menu.rs
use menu::*;
// 设置窗口(控件构造/窗口构建显示收集/校验告警/配置热应用)集中在 settings.rs
// Settings window (control builders/window build-show-collect/validation alerts/hot config apply) live in settings.rs
use objc2::runtime::{AnyClass, AnyObject, Sel};
use objc2::{class, msg_send, sel};
use objc2_foundation::{NSPoint, NSRect, NSSize};
use settings::*;
use std::collections::HashSet;
use std::ffi::{c_void, CString};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicU8, Ordering};
use std::sync::{LazyLock, Mutex, OnceLock};
use std::thread;

use event_monitor::{start as start_event_monitor, GlobalEvent};
use window_collector::{
    app_activation_is_current, bump_window_mru, cache_running_app_icons,
    cache_running_app_icons_small, clear_ax_window_cache_for_pid, ensure_icon_cache_dir,
    extract_icon_to_cache, focused_window_cgwid, migrate_legacy_cache, note_app_activated,
    note_app_terminated, remove_pid_mru, MruMap, WindowInfo,
};

// FFI 声明与 ObjC 桥接基础工具已移至 `ffi.rs` / FFI declarations and ObjC bridging primitives moved to `ffi.rs`

// 布局常量 STATUS_H / H_PADDING 已移至 `theme.rs` / layout constants moved to `theme.rs`

// ========== Types ==========

// 跨模块共享的应用状态(overlay/menu/settings 均会访问,故 pub(crate))。
// AppState 与 TAB_STATE 留在 main.rs,避免 overlay↔menu 互相依赖形成环。
// Cross-module shared app state (pub(crate) so overlay/menu/settings can access).
// AppState + TAB_STATE stay in main.rs to avoid an overlay<->menu dependency cycle.
pub(crate) struct AppState {
    pub(crate) windows: Vec<WindowInfo>,
    pub(crate) selected: usize,
    pub(crate) visible: bool,
    pub(crate) mru: MruMap,
    pub(crate) focus_key: Option<(i32, u32)>,
    // 召唤瞬间模型已持有的窗口 key 集合。浮窗打开后的刷新,只应把「召唤时就在场」的窗口
    // 视为用户正在选择的目标;召唤后才出现的新窗口是 newcomer,不参与选中(对齐 alt-tab 的
    // windowIdsAtSummon)。None = 尚未记录(首帧前)。
    // Window keys the model already held at summon. Refreshes after the overlay shows only treat
    // windows present at the summon as the user's switching targets; a window appearing after the
    // summon is a newcomer and does not participate in the pick (mirrors alt-tab's windowIdsAtSummon).
    // None = not yet recorded (before the first frame).
    pub(crate) summon_keys: Option<HashSet<(i32, u32)>>,
    // 用户是否已主动移动过选中(Tab/方向键/点击)。false=选中仍是首帧默认落点,它应跟随
    // 「召唤时语义」而不是当前 MRU 排序;true=用户已选具体窗口,刷新后需钉住该窗口 key。
    // false = the selection is still the first-frame default and follows the summon semantics,
    // not the live MRU order; true = the user picked a concrete window and the selection must
    // stay pinned to it across refreshes (mirrors alt-tab's userPickedSelection).
    pub(crate) user_picked: bool,
    // 用户当前选中的目标窗口 key。user_picked=true 时它是刷新后必须钉住的窗口;
    // user_picked=false 时它是首帧默认目标,刷新不因 MRU 排序漂移而改选它。
    // The user's current selection target key. When user_picked=true it is the window the pick
    // must stay pinned to after a refresh; when false it is the first-frame default target and a
    // refresh must not re-pick just because the MRU order shifted.
    pub(crate) selected_target_key: Option<(i32, u32)>,
    // 首帧「待显示」标记:true 表示本次召唤已发起刷新、但浮窗尚未显示,等待 apply_window_refresh
    // 拿到首帧快照后一次性显示(一次成图,避免「先显示旧快照再重排」的跳变)。
    // pending_first_show: true once this summon has kicked off a refresh but the overlay has not
    // been shown yet; apply_window_refresh consumes it to show once the first snapshot is ready
    // (single-shot render, avoiding the "show stale snapshot then reorder" jump).
    pub(crate) pending_first_show: bool,
    // 首帧待显示时记录用户是按了正向还是反向 Tab,apply_window_refresh 用它在显示时定首选。
    // backward flag captured while the first frame is pending, so apply_window_refresh can decide
    // the initial pick direction when it finally shows.
    pub(crate) pending_first_backward: bool,
}

#[repr(u8)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum WindowRefreshReason {
    Lifecycle = 1,
    Summon = 2,
}

impl WindowRefreshReason {
    fn bumps_frontmost(self) -> bool {
        matches!(self, Self::Summon)
    }

    fn merge(self, other: Self) -> Self {
        if self.bumps_frontmost() || other.bumps_frontmost() {
            Self::Summon
        } else {
            Self::Lifecycle
        }
    }

    fn from_raw(value: u8) -> Option<Self> {
        match value {
            value if value == Self::Lifecycle as u8 => Some(Self::Lifecycle),
            value if value == Self::Summon as u8 => Some(Self::Summon),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum WindowRefreshRequest {
    Full(WindowRefreshReason),
    Focused { pid: i32, window_id: u32 },
}

impl AppState {
    pub(crate) fn new() -> Self {
        // 启动时用系统窗口前→后顺序预种 MRU:重启后初始顺序与原生 Cmd+Tab 的
        // 应用级顺序一致(见 window_collector::seed_mru_from_system_order)。
        // Seed the MRU from the system's front-to-back window order at startup, so the
        // initial ordering after a restart matches the native app-level Cmd+Tab order.
        let mut mru = window_collector::seed_mru_from_system_order();
        let windows = if has_accessibility_permission() {
            window_collector::collect_windows(&mut mru)
        } else {
            Vec::new()
        };
        if !has_accessibility_permission() {
            log_info!("No accessibility permission.");
            log_info!("Go to System Settings → Privacy & Security → Accessibility");
        }
        let win_count = windows.len();
        AppState {
            windows,
            selected: if win_count > 1 { 1 } else { 0 },
            visible: false,
            mru,
            focus_key: None,
            summon_keys: None,
            user_picked: false,
            selected_target_key: None,
            pending_first_show: false,
            pending_first_backward: false,
        }
    }
}

struct WindowRefreshResult {
    generation: u64,
    windows: Vec<WindowInfo>,
    mru: MruMap,
    replace_pid: Option<i32>,
    active_key: Option<(i32, u32)>,
}

static WINDOW_REFRESH_IN_FLIGHT: AtomicBool = AtomicBool::new(false);
// 保存待处理刷新中最高优先级的原因:召唤刷新不能被生命周期刷新降级。
// Keep the highest-priority pending reason so a summon refresh cannot be downgraded to a lifecycle refresh.
static WINDOW_REFRESH_PENDING: AtomicU8 = AtomicU8::new(0);
static WINDOW_REFRESH_PENDING_FOCUS: LazyLock<Mutex<Option<(i32, u32)>>> =
    LazyLock::new(|| Mutex::new(None));
static WINDOW_REFRESH_GENERATION: AtomicU64 = AtomicU64::new(0);
static WINDOW_REFRESH_RESULT: LazyLock<Mutex<Option<WindowRefreshResult>>> =
    LazyLock::new(|| Mutex::new(None));

/// 请求一次有界的后台窗口快照；同一时间只允许一个任务，避免快捷键连按制造线程风暴。
/// Request one bounded background window snapshot; only one task may run at a time,
/// preventing rapid shortcut presses from creating a thread storm.
fn request_window_refresh() {
    request_window_refresh_for(WindowRefreshReason::Summon);
}

fn request_lifecycle_window_refresh() {
    request_window_refresh_for(WindowRefreshReason::Lifecycle);
}

fn request_focused_window_refresh(pid: i32, window_id: u32) {
    request_window_refresh_request(WindowRefreshRequest::Focused { pid, window_id });
}

fn merge_pending_refresh_reason(reason: WindowRefreshReason) {
    let mut current = WINDOW_REFRESH_PENDING.load(Ordering::Acquire);
    loop {
        let merged =
            WindowRefreshReason::from_raw(current).map_or(reason, |pending| pending.merge(reason));
        let requested = merged as u8;
        if current == requested {
            return;
        }
        match WINDOW_REFRESH_PENDING.compare_exchange(
            current,
            requested,
            Ordering::AcqRel,
            Ordering::Acquire,
        ) {
            Ok(_) => return,
            Err(next) => current = next,
        }
    }
}

fn take_pending_refresh_reason() -> Option<WindowRefreshReason> {
    WindowRefreshReason::from_raw(WINDOW_REFRESH_PENDING.swap(0, Ordering::AcqRel))
}

fn take_pending_refresh_request() -> Option<WindowRefreshRequest> {
    if let Some(reason) = take_pending_refresh_reason() {
        return Some(WindowRefreshRequest::Full(reason));
    }
    WINDOW_REFRESH_PENDING_FOCUS
        .lock()
        .unwrap()
        .take()
        .map(|(pid, window_id)| WindowRefreshRequest::Focused { pid, window_id })
}

fn queue_refresh_request(request: WindowRefreshRequest) {
    match request {
        WindowRefreshRequest::Full(reason) => merge_pending_refresh_reason(reason),
        WindowRefreshRequest::Focused { pid, window_id } => {
            *WINDOW_REFRESH_PENDING_FOCUS.lock().unwrap() = Some((pid, window_id));
        }
    }
}

fn request_window_refresh_for(reason: WindowRefreshReason) {
    request_window_refresh_request(WindowRefreshRequest::Full(reason));
}

fn request_window_refresh_request(request: WindowRefreshRequest) {
    if WINDOW_REFRESH_IN_FLIGHT.swap(true, Ordering::AcqRel) {
        queue_refresh_request(request);
        return;
    }

    start_window_refresh(request);
}

fn start_window_refresh(request: WindowRefreshRequest) {
    let (generation, mru) = {
        let state_opt = TAB_STATE.lock().unwrap();
        let Some(state) = state_opt.as_ref() else {
            WINDOW_REFRESH_IN_FLIGHT.store(false, Ordering::Release);
            return;
        };
        (
            WINDOW_REFRESH_GENERATION.fetch_add(1, Ordering::AcqRel) + 1,
            state.mru.clone(),
        )
    };

    // 浮窗已显示后,summon-bump 不再把前台窗口写回 MRU:否则每次刷新都会把前台窗口顶到第 0 位、
    // 造成已显示的列表重排(跳变)。首帧待显示(visible=false)仍允许 bump 一次,保证首位是真实前台。
    // Once the overlay is shown, summon-bump must NOT rewrite the frontmost window into MRU:
    // otherwise every refresh hoists it to index 0 and reorders the displayed list (the jump).
    // The not-yet-shown first frame (visible=false) still may bump once so the head is the real
    // frontmost.
    let allow_bump = {
        let state_opt = TAB_STATE.lock().unwrap();
        !state_opt.as_ref().is_some_and(|s| s.visible)
    };

    thread::Builder::new()
        .name("window-refresh".into())
        .spawn(move || {
            let started = std::time::Instant::now();
            let mut mru = mru;
            let (windows, replace_pid, active_key) = match request {
                WindowRefreshRequest::Full(reason) => (
                    window_collector::collect_windows_with_frontmost_bump(
                        &mut mru,
                        reason.bumps_frontmost() && allow_bump,
                    ),
                    None,
                    None,
                ),
                WindowRefreshRequest::Focused { pid, window_id } => {
                    match window_collector::collect_windows_for_pid(&mut mru, pid, window_id) {
                        Some(windows) => {
                            let active_key = windows
                                .iter()
                                .any(|window| window.window_id == window_id)
                                .then_some((pid, window_id));
                            (windows, Some(pid), active_key)
                        }
                        None => {
                            // 定向 AX 查询失败时保留完整快照兜底,避免误删目标 App 的卡片。
                            // Fall back to a full snapshot when the directed AX query fails, so
                            // a transient timeout cannot erase the target app's cards.
                            log_debug!(
                                "[windows] focused refresh fallback: pid={} cgwid={}",
                                pid,
                                window_id
                            );
                            (
                                window_collector::collect_windows_with_frontmost_bump(
                                    &mut mru, false,
                                ),
                                None,
                                None,
                            )
                        }
                    }
                }
            };
            *WINDOW_REFRESH_RESULT.lock().unwrap() = Some(WindowRefreshResult {
                generation,
                windows,
                mru,
                replace_pid,
                active_key,
            });
            log_debug!(
                "[windows] background refresh ready generation={} elapsed={}ms",
                generation,
                started.elapsed().as_millis()
            );

            let controller = CONTROLLER.lock().unwrap().map(|ptr| ptr.0);
            if let Some(controller) = controller {
                unsafe {
                    let _: () = msg_send![controller,
                        performSelectorOnMainThread: sel!(handleWindowRefresh:),
                        withObject: std::ptr::null::<AnyObject>(),
                        waitUntilDone: false
                    ];
                }
            } else {
                WINDOW_REFRESH_RESULT.lock().unwrap().take();
                WINDOW_REFRESH_IN_FLIGHT.store(false, Ordering::Release);
                if let Some(pending_request) = take_pending_refresh_request() {
                    request_window_refresh_request(pending_request);
                }
            }
        })
        .expect("spawn window-refresh thread");
}

fn merge_refreshed_windows(
    existing: &[WindowInfo],
    replace_pid: Option<i32>,
    mut refreshed: Vec<WindowInfo>,
) -> Vec<WindowInfo> {
    // AX 是权威:窗口只在当前快照被 AX 确认时才进入列表,绝不从旧列表复活。
    // AX 对 tab 多表面应用(如 Ghostty)只报当前聚焦窗口,保留旧列表会把未聚焦的 tab 表面
    // 当成独立窗口重新塞进来,导致一张窗口显示成多张卡片。
    // AX is authoritative: a window enters the list only if the current snapshot's AX confirms
    // it; never resurrect from the previous list. For tab-per-surface apps (e.g. Ghostty) AX
    // reports only the focused surface, so retaining the old list would re-inject an unfocused
    // tab surface and show one window as many cards.
    if let Some(pid) = replace_pid {
        let mut merged: Vec<WindowInfo> = existing
            .iter()
            .filter(|window| window.pid != pid)
            .cloned()
            .collect();
        merged.append(&mut refreshed);
        merged
    } else {
        refreshed
    }
}

fn selection_index_after_refresh(
    selected_key: Option<(i32, u32)>,
    previous_index: usize,
    windows: &[WindowInfo],
) -> usize {
    selected_key
        .and_then(|key| {
            windows
                .iter()
                .position(|window| (window.pid, window.window_id) == key)
        })
        .unwrap_or_else(|| previous_index.min(windows.len().saturating_sub(1)))
}

/// 刷新后选中索引:优先跟随「目标窗口 key」,而不是当前列表下标。
///
/// - `user_picked = true`(用户已主动导航):钉住 `target_key`(用户选的具体窗口);它还在列表
///   就恢复其位置,不在才钳制。列表重排(哪怕是同窗口表面切换)不移动选中。
/// - `user_picked = false`(仍是首帧默认落点):锁定首帧选中的 `target_key`,刷新不因 MRU 排序
///   被前台窗口 bump 改写而改选——否则用户按一次 Cmd+Tab,选中会跟着「无窗口增减」的重排漂移。
///
/// Selection index after a refresh: prefer the target window key over the live list index.
/// - user_picked = true: pin to target_key; keep its position if still shown, else clamp.
/// - user_picked = false: lock to the summon-time target so a refresh doesn't re-pick merely
///   because the MRU order was bumped by the frontmost window (a reorder with no add/remove).
fn select_index_after_refresh(
    user_picked: bool,
    target_key: Option<(i32, u32)>,
    live_selected_key: Option<(i32, u32)>,
    previous_index: usize,
    windows: &[WindowInfo],
) -> usize {
    let anchor = if user_picked {
        target_key
    } else {
        target_key.or(live_selected_key)
    };
    selection_index_after_refresh(anchor, previous_index, windows)
}

/// 在主线程应用快照，并合并后台任务开始后产生的窗口级 MRU 更新。
/// Apply a snapshot on the main thread and merge window-level MRU updates made after
/// the background task started.
fn apply_window_refresh() {
    let Some(result) = WINDOW_REFRESH_RESULT.lock().unwrap().take() else {
        WINDOW_REFRESH_IN_FLIGHT.store(false, Ordering::Release);
        if let Some(pending_request) = take_pending_refresh_request() {
            request_window_refresh_request(pending_request);
        }
        return;
    };
    if result.generation != WINDOW_REFRESH_GENERATION.load(Ordering::Acquire) {
        WINDOW_REFRESH_IN_FLIGHT.store(false, Ordering::Release);
        if let Some(pending_request) = take_pending_refresh_request() {
            request_window_refresh_request(pending_request);
        }
        return;
    }

    let subscriptions = window_collector::window_server_candidates();

    let (was_visible, set_changed) = {
        let mut state_opt = TAB_STATE.lock().unwrap();
        let Some(state) = state_opt.as_mut() else {
            WINDOW_REFRESH_IN_FLIGHT.store(false, Ordering::Release);
            return;
        };
        let selected_key = state
            .windows
            .get(state.selected)
            .map(|window| (window.pid, window.window_id));
        let mut mru = state.mru.clone();
        for (key, timestamp) in result.mru {
            if mru.get(&key).is_none_or(|current| *current < timestamp) {
                mru.insert(key, timestamp);
            }
        }
        let replace_pid = result.replace_pid;
        let active_key = result.active_key;
        // 定向刷新只替换目标 PID,其他应用的卡片和窗口顺序记忆保持不变。
        // A directed refresh replaces only the target PID; cards and ordering memory for every
        // other application remain intact.
        let mut windows = merge_refreshed_windows(&state.windows, replace_pid, result.windows);
        window_collector::sort_windows_by_mru(&mut windows, &mru, std::time::Instant::now());
        if replace_pid.is_none() {
            // 全量刷新合并旧窗口后重新建立唯一的前台代表，避免旧快照的 is_active 残留。
            // Re-establish one frontmost representative after a full merge so an old snapshot
            // cannot leave multiple stale is_active flags behind.
            for window in &mut windows {
                window.is_active = false;
            }
            if let Some(first) = windows.first_mut() {
                first.is_active = true;
            }
        } else if let Some(active_key) = active_key {
            for window in &mut windows {
                window.is_active = (window.pid, window.window_id) == active_key;
            }
        }
        // 选中跟随「目标窗口」而非「当前列表下标」:
        // - user_picked=true(用户已主动导航):钉住 selected_target_key,列表重排不动它。
        // - user_picked=false(仍是首帧默认落点):锁定首帧选中的目标窗口,刷新不因 MRU
        //   排序被前台 bump 改写而改选——否则用户按一次 Cmd+Tab,选中会跟着重排漂移。
        // Selection follows the target window, not the live list index:
        // - user_picked=true (user navigated): pin to selected_target_key; reorders cannot move it.
        // - user_picked=false (still the first-frame default): lock to the summon-time target so a
        //   refresh doesn't re-pick just because the MRU order was bumped by the frontmost window.
        let selected = select_index_after_refresh(
            state.user_picked,
            state.selected_target_key,
            selected_key,
            state.selected,
            &windows,
        );
        let changed = state.windows != windows;
        // 集合级变化(窗口增删):只有这个才需要整树重建浮窗。仅排序变化(MRU 被前台 bump 改写、
        // 表面切换)只更新数据,不重建列表——否则用户看到「打开了还在跳」。定向刷新(replace_pid)
        // 替换了目标 PID 的卡片,也视为集合变化。
        // Set-level change (windows added/removed): only this needs a full overlay rebuild. A pure
        // reorder (MRU bumped by the frontmost, surface flip) only updates data without rebuilding
        // the list — otherwise the overlay "keeps jumping after it opened". A directed refresh
        // (replace_pid) swapping a PID's cards also counts as a set change.
        let set_changed = replace_pid.is_some() || {
            let old: HashSet<(i32, u32)> =
                state.windows.iter().map(|w| (w.pid, w.window_id)).collect();
            let new: HashSet<(i32, u32)> = windows.iter().map(|w| (w.pid, w.window_id)).collect();
            old != new
        };
        let was_visible = state.visible;
        // 浮窗显示期间发生排序/集合变化:打印变更前后的完整窗口序与选中位置,便于排查
        // 「窗口排序在浮窗显示后被刷新改动」类问题(如 Ghostty tab 表面切换导致的跳变)。
        // Overlay visible and the ordering/set changed: log the full window order and selection
        // before and after, so a "reorder-after-show" drift (e.g. a Ghostty tab-surface flip) is
        // visible in the log.
        if changed && was_visible {
            crate::log_window_ordering("refresh before", &state.windows, &mru, state.selected);
        }
        state.windows = windows;
        state.selected = selected;
        state.mru = mru;
        if state.windows.is_empty() {
            state.visible = false;
        }
        if changed && was_visible {
            crate::log_window_ordering("refresh after", &state.windows, &state.mru, state.selected);
        }
        (was_visible, set_changed)
    };
    WINDOW_REFRESH_IN_FLIGHT.store(false, Ordering::Release);
    let pending_request = take_pending_refresh_request();

    // 首帧快照就绪:消费 pending_first_show,一次性显示(一次成图)。此时窗口列表已是刷新后的
    // 最终排序,不会再出现「先显示旧快照、再重排」的两段跳变。
    // First snapshot ready: consume pending_first_show and show once (single-shot render). The
    // list is already the final post-refresh order, so the "stale then reorder" jump is gone.
    let first_show_request = {
        let mut state_opt = TAB_STATE.lock().unwrap();
        if let Some(state) = state_opt.as_mut() {
            if state.pending_first_show {
                state.pending_first_show = false;
                Some(state.pending_first_backward)
            } else {
                None
            }
        } else {
            None
        }
    };
    if let Some(backward) = first_show_request {
        crate::overlay::show_first_summon(backward);
        log_debug!(
            "[overlay] summon e2e: first snapshot shown (backward={})",
            backward
        );
    }

    if set_changed && was_visible {
        reset_thumbnail_visible_range();
        reset_thumbnail_scroll();
        reset_thumbnail_nav_anchor();
        show_overlay();
        refresh_highlight();
    }
    // WindowServer 监听所有 CG 候选窗口,而不是只监听 AX 确认并显示的卡片。
    // WindowServer observes every CG candidate instead of only AX-confirmed display cards.
    window_server::update_subscriptions(&subscriptions);
    if let Some(request) = pending_request {
        request_window_refresh_request(request);
    }
}

extern "C" fn on_window_refresh(_self: *mut c_void, _cmd: Sel, _arg: *mut c_void) {
    apply_window_refresh();
}

extern "C" fn on_window_server_event(_self: *mut c_void, _cmd: Sel, _arg: *mut c_void) {
    let events = window_server::drain_main();
    if events.is_empty() {
        return;
    }
    let should_refresh = events.iter().any(|event| match event {
        window_server::WindowServerEvent::Created(window_id) => {
            log_debug!("[windows] WindowServer created cgwid={}", window_id);
            true
        }
        window_server::WindowServerEvent::Destroyed(window_id) => {
            log_debug!("[windows] WindowServer destroyed cgwid={}", window_id);
            true
        }
        window_server::WindowServerEvent::Focused(window_id) => {
            let displayed_pid = TAB_STATE
                .lock()
                .unwrap()
                .as_ref()
                .and_then(|state| {
                    state
                        .windows
                        .iter()
                        .find(|window| window.window_id == *window_id)
                })
                .map(|window| window.pid);
            let pid = displayed_pid
                .or_else(|| window_server::owner_for_window(*window_id))
                .or_else(|| window_collector::owner_pid_for_cgwid(*window_id));
            if let Some(pid) = pid {
                let frontmost_pid = frontmost_app_info().1;
                if frontmost_pid == pid {
                    let activation_token = window_server::activation_token(pid);
                    if window_server::focus_should_bump(pid, *window_id) {
                        if let Some(state) = TAB_STATE.lock().unwrap().as_mut() {
                            // 只把「AX 已确认且显示在列表里」的窗口记为焦点 key;未显示的 CG
                            // surface(Ghostty 单窗口双 tab 的另一个 tab)不该成为焦点锚点。
                            // Anchor the focus key only for an AX-confirmed, shown window; an
                            // undisplayed CG surface (the other Ghostty tab) must not be anchored.
                            best_effort_bump_focus_key(state, pid, *window_id);
                        }
                        if let Some(activated_at) = activation_token {
                            thumbnail::refresh_after_activation(pid, *window_id, activated_at);
                        }
                    }
                    if displayed_pid.is_none() {
                        // 未显示的 CG 窗口也参与焦点追踪,但只有定向 AX 刷新确认后才进入卡片。
                        // Track an undisplayed CG window too, but only let a directed AX refresh
                        // promote it into the card list after confirmation.
                        log_debug!(
                            "[windows] focused unlisted cgwid={} pid={}; directed refresh",
                            window_id,
                            pid
                        );
                        request_focused_window_refresh(pid, *window_id);
                    }
                }
            } else {
                log_debug!(
                    "[windows] focused cgwid={} has no known owner PID",
                    window_id
                );
            }
            false
        }
    });
    if should_refresh {
        request_lifecycle_window_refresh();
    }
}

// 把 (pid, cgwid) 写进 MRU,并且仅当该窗口确实在当前显示列表里时才记为焦点锚点。
// 未显示的 CG surface(Ghostty 单窗口双 tab 的另一个 tab)不锚定,否则首帧匹配失败,
// 造成「切到 tab A 却显示 tab B」的内容跳变。
// Bump (pid, cgwid) into MRU, anchoring the focus key only when the window is actually shown
// in the current display list. An undisplayed CG surface (the other Ghostty tab of a
// single-window multi-tab app) is never anchored, so the first frame cannot mismatch and show
// "switched to tab A but tab B appears". Caller must hold TAB_STATE.
fn best_effort_bump_focus_key(state: &mut AppState, pid: i32, cgwid: u32) {
    bump_window_mru(&mut state.mru, pid, cgwid);
    let shown = state
        .windows
        .iter()
        .any(|window| window.pid == pid && window.window_id == cgwid);
    if shown {
        state.focus_key = Some((pid, cgwid));
    }
}

/// 诊断:打印当前窗口排序 + 选中标记。`>` = 当前选中项,`*` = active(前台代理)项。
/// 排序列与 select 位置同时可见,便于定位「Command+Tab 一次却漂移一格」类问题。
/// Diagnostic: print the window ordering plus the selection marker. `>` = the currently
/// selected entry, `*` = the active (frontmost proxy) entry. Order and selection position are
/// both visible, which makes a one-press-Tab-drifts-a-slot bug easy to pinpoint.
pub(crate) fn log_window_ordering(
    label: &str,
    windows: &[WindowInfo],
    mru: &MruMap,
    selected: usize,
) {
    log_debug!(
        "[order] {}: {} windows (selected={})",
        label,
        windows.len(),
        selected
    );
    for (i, w) in windows.iter().enumerate() {
        let mru_ms = mru
            .get(&(w.pid, w.window_id))
            .map(|t| t.elapsed().as_millis());
        let selected_mark = if i == selected { ">" } else { " " };
        let active_mark = if w.is_active { "*" } else { " " };
        log_debug!(
            "  {}{} pid={} app=\"{}\" cgwid={} title=\"{}\" mru_ms={:?}",
            selected_mark,
            active_mark,
            w.pid,
            w.app_name,
            w.window_id,
            w.window_title,
            mru_ms
        );
    }
}

// Colors 结构已移至 `theme.rs` / moved to `theme.rs`

// ObjPtr / ObjClassPtr 已移至 `ffi.rs` / moved to `ffi.rs`

// ========== Global State ==========

pub(crate) static TAB_STATE: Mutex<Option<AppState>> = Mutex::new(None);
pub(crate) static CONTROLLER: Mutex<Option<ObjPtr>> = Mutex::new(None);

/// 菜单项与设置按钮共用的 ObjC target 对象（OhMyTabMenuTarget2 实例）。
/// Shared ObjC target object for menu items and settings buttons.
pub(crate) static MENU_TARGET: Mutex<Option<ObjPtr>> = Mutex::new(None);
pub(crate) static STATUS_EVENT_TX: std::sync::OnceLock<flume::Sender<GlobalEvent>> =
    std::sync::OnceLock::new();

// ========== Helper Functions ==========
// make_nsstring / release_obj / has_accessibility_permission / hex_to_*color / layer_set_* 已移至 `ffi.rs`
// make_nsstring / release_obj / has_accessibility_permission / hex_to_*color / layer_set_* moved to `ffi.rs`

// colors_from_config / system_dark_mode / current_colors / card_* / icon_px / letter_px /
// window_height / window_width 已移至 `theme.rs`
// colors_from_config / system_dark_mode / current_colors / card_* / icon_px / letter_px /
// window_height / window_width moved to `theme.rs`

// ========== ObjC Method Implementations ==========

// --- Controller ---

extern "C" fn on_app_activated(_self: *mut c_void, _cmd: Sel, notification: *mut c_void) {
    unsafe {
        let user_info: *mut AnyObject = msg_send![notification as *mut AnyObject, userInfo];
        if user_info.is_null() {
            return;
        }
        let key = make_nsstring("NSWorkspaceApplicationKey");
        let app: *mut AnyObject = msg_send![user_info, objectForKey: key];
        CFRelease(key as *const c_void);
        if app.is_null() {
            return;
        }
        // 非常规策略 = 后台/辅助进程(如嵌套 helper):无窗口、图标多为通用占位图,
        // 提取会以相同 bundle id 污染主应用的图标缓存;缩略图观察者也不需要。
        // Non-regular policy = background/helper processes: no windows, icons are
        // usually the generic placeholder, and extracting would poison the main
        // app's shared cache key; the thumbnail observer doesn't need them either.
        let policy: i64 = msg_send![app, activationPolicy];
        if policy != 0 {
            return;
        }
        let pid: i32 = msg_send![app, processIdentifier];
        // 记录本次 App 激活 token：新窗口首次进入 MRU 表时用它作初始时间，异步查询
        // 也用它识别迟到结果；已有窗口不会因这个 App 级时间被整体更新。
        // Record this app activation's token: newly discovered windows use it as their initial
        // MRU, and async queries use it to reject stale results. Existing windows are never
        // updated together from this app-level timestamp.
        let activated_at = note_app_activated(pid);
        let window_ids = window_server::window_ids_for_pid(pid);
        window_server::begin_activation(pid, &window_ids, activated_at);
        // 后台线程解析焦点窗口的 CGWindowID 并 bump 窗口级 MRU。
        // 系统 Cmd+Tab / Dock 点击等外部焦点切换通过此路径反馈到窗口排序中。
        // kAXFocusedWindow 的 AX 查询可能阻塞最高 50ms（目标 App 无响应时），
        // 必须放到后台线程避免卡住主线程 UI。
        // Resolve the focused window's CGWindowID off-main and bump window MRU.
        // External focus switches (system Cmd+Tab, Dock clicks) feed into window
        // ordering through this path. The kAXFocusedWindow AX query can block up
        // to 50ms (target app unresponsive), so it must run off the main thread.
        schedule_activation_focus(pid, activated_at);
    }
}

struct ActivationFocusTask {
    pid: i32,
    activated_at: std::time::Instant,
}

static ACTIVATION_FOCUS_TX: OnceLock<flume::Sender<ActivationFocusTask>> = OnceLock::new();

/// 启动固定大小的 AX 焦点查询队列，避免连续 App 激活创建无限短命线程。
/// Start a fixed-size AX focus queue so repeated app activations cannot create an
/// unbounded number of short-lived threads.
fn start_activation_focus_scheduler() {
    let (tx, rx) = flume::bounded::<ActivationFocusTask>(32);
    let _ = ACTIVATION_FOCUS_TX.set(tx);
    for worker in 0..2 {
        let rx = rx.clone();
        thread::Builder::new()
            .name(format!("activation-focus-{worker}"))
            .spawn(move || {
                while let Ok(task) = rx.recv() {
                    resolve_activation_focus(task);
                }
            })
            .expect("spawn activation-focus worker");
    }
}

fn schedule_activation_focus(pid: i32, activated_at: std::time::Instant) {
    let Some(tx) = ACTIVATION_FOCUS_TX.get() else {
        return;
    };
    if tx
        .try_send(ActivationFocusTask { pid, activated_at })
        .is_err()
    {
        log_debug!("activation focus queue full or stopped: pid={}", pid);
    }
}

fn resolve_activation_focus(task: ActivationFocusTask) {
    unsafe {
        let pool: *mut AnyObject = msg_send![class!(NSAutoreleasePool), new];
        // 日志用于诊断 MRU 是否被正确 bump:成功打印 pid+cgwid;失败只进行有限重试。
        // Log to diagnose MRU bumping: failures are handled with bounded retries only.
        let retry_delays_ms = [0_u64, 50, 150, 300, 700, 1_000];
        let mut bumped = false;
        for (attempt, delay_ms) in retry_delays_ms.into_iter().enumerate() {
            if delay_ms > 0 {
                std::thread::sleep(std::time::Duration::from_millis(delay_ms));
            }
            if !app_activation_is_current(task.pid, task.activated_at) {
                break;
            }
            let workspace: *mut AnyObject = msg_send![class!(NSWorkspace), sharedWorkspace];
            let front_app: *mut AnyObject = msg_send![workspace, frontmostApplication];
            if front_app.is_null() {
                continue;
            }
            let front_pid: i32 = msg_send![front_app, processIdentifier];
            if front_pid != task.pid {
                break;
            }
            if let Some(cgwid) = focused_window_cgwid(task.pid) {
                if !window_server::ax_focus_backstop_allowed(task.pid) {
                    bumped = true;
                    break;
                }
                let mut state_opt = TAB_STATE.lock().unwrap();
                if let Some(ref mut state) = *state_opt {
                    // 同 on_window_server_event:AX 查到的窗口只有确实在显示列表里才记 focus_key,
                    // 避免未确认的 CG surface 污染焦点锚点。
                    // Same guard as on_window_server_event: anchor the focus key only when the
                    // AX-resolved window is actually shown, so an unconfirmed CG surface cannot
                    // poison the focus anchor.
                    best_effort_bump_focus_key(state, task.pid, cgwid);
                    log_debug!(
                        "app-activated bump: pid={} cgwid={} attempt={}",
                        task.pid,
                        cgwid,
                        attempt + 1
                    );
                }
                thumbnail::refresh_after_activation(task.pid, cgwid, task.activated_at);
                bumped = true;
                break;
            }
        }
        if !bumped {
            log_debug!(
                "app-activated bump: pid={} (no focused window / stale activation / AX timeout)",
                task.pid
            );
        }
        let _: () = msg_send![pool, drain];
    }
}

extern "C" fn on_app_launched(_self: *mut c_void, _cmd: Sel, notification: *mut c_void) {
    // Pre-cache the launched app's icon so it's on disk before the user summons
    // the switcher. Run off the main thread (with an autorelease pool) so the
    // launch notification doesn't block the UI; extract_icon_to_cache is
    // defensive (null-safe) and a failure simply leaves the letter-icon fallback.
    let pid: i32 = unsafe {
        let user_info: *mut AnyObject = msg_send![notification as *mut AnyObject, userInfo];
        if user_info.is_null() {
            return;
        }
        let key = make_nsstring("NSWorkspaceApplicationKey");
        let app: *mut AnyObject = msg_send![user_info, objectForKey: key];
        CFRelease(key as *const c_void);
        if app.is_null() {
            return;
        }
        msg_send![app, processIdentifier]
    };
    if pid <= 0 {
        return;
    }
    thread::spawn(move || unsafe {
        let pool: *mut AnyObject = msg_send![class!(NSAutoreleasePool), new];
        match extract_icon_to_cache(pid) {
            Some(_) => log_debug!("app-launch icon cached: pid={}", pid),
            None => {
                // 刚启动瞬间 AppKit 的 icon 可能尚未就绪(app.icon 返回 nil),导致
                // 提取失败且无缓存留下——之后浮窗显示字母占位。延迟 ~1s 重试一次。
                // The icon may not be ready the instant the app launches (app.icon nil),
                // silently failing the extract and leaving the letter placeholder in the
                // switcher. Retry once after ~1s.
                log_debug!(
                    "app-launch icon extract failed for pid={}, retrying in 1s",
                    pid
                );
                std::thread::sleep(std::time::Duration::from_secs(1));
                let _ = extract_icon_to_cache(pid);
            }
        }
        let _: () = msg_send![pool, drain];
    });
    // 缩略图:给新启动的 App 安装 AXObserver 并预生成其既有窗口。
    // Thumbnails: install the new app's AXObserver and pre-generate its windows.
    thumbnail::app_launched(pid);
}

/// NSWorkspaceDidTerminateApplicationNotification 转发点(main 线程):
/// 通知缩略图模块取消捕获、清缓存并卸载该 App 的 observer。
/// Forwarding point for NSWorkspaceDidTerminateApplicationNotification (main
/// thread): tells the thumbnail module to cancel captures, clear cached frames,
/// and uninstall that app's observer.
extern "C" fn on_app_terminated(_self: *mut c_void, _cmd: Sel, notification: *mut c_void) {
    let pid: i32 = unsafe {
        let user_info: *mut AnyObject = msg_send![notification as *mut AnyObject, userInfo];
        if user_info.is_null() {
            return;
        }
        let key = make_nsstring("NSWorkspaceApplicationKey");
        let app: *mut AnyObject = msg_send![user_info, objectForKey: key];
        CFRelease(key as *const c_void);
        if app.is_null() {
            return;
        }
        msg_send![app, processIdentifier]
    };
    if pid > 0 {
        // 退出即清掉激活 token 与该 PID 的窗口 MRU，既会使未结束的重试失效，也避免
        // PID/CGWindowID 复用继承旧进程的时间戳。
        // Termination clears the activation token and this PID's window MRUs immediately,
        // invalidating in-flight retries and preventing PID/CGWindowID reuse contamination.
        note_app_terminated(pid);
        clear_ax_window_cache_for_pid(pid);
        if let Some(ref mut state) = *TAB_STATE.lock().unwrap() {
            let removed = remove_pid_mru(&mut state.mru, pid);
            if state
                .focus_key
                .is_some_and(|(focus_pid, _)| focus_pid == pid)
            {
                state.focus_key = None;
            }
            if removed > 0 {
                log_debug!(
                    "app-terminated MRU cleanup: pid={} removed={}",
                    pid,
                    removed
                );
            }
        }
        thumbnail::app_terminated(pid);
    }
}

extern "C" fn on_locale_changed(_self: *mut c_void, _cmd: Sel, _arg: *mut c_void) {
    unsafe {
        // 通知投递线程不保证是主线程,而刷新 UI 必须在主线程;非主线程时转到主线程重入本方法。
        // Notification delivery thread isn't guaranteed to be main, but UI refresh must run on
        // main; when off main, hop to main and re-enter this method.
        let is_main: bool = msg_send![class!(NSThread), isMainThread];
        if !is_main {
            let _: () = msg_send![_self as *mut AnyObject,
                performSelectorOnMainThread: sel!(handleLocaleChanged:),
                withObject: std::ptr::null::<AnyObject>(),
                waitUntilDone: false
            ];
            return;
        }
    }
    // 系统语言变更(NSLocaleCurrentLocaleDidChangeNotification)。
    // 仅当 locale 为 auto(系统派生)时 apply_config_locale 会真正改变解析结果;显式 locale 下短路。
    // 重新解析后刷新菜单标题、作废设置窗口待下次按新 locale 重建。
    // System language changed (NSLocaleCurrentLocaleDidChangeNotification).
    // apply_config_locale only re-resolves when locale is auto (system-derived); it short-circuits
    // for an explicit locale. After re-resolving, refresh menu titles and invalidate the settings
    // window so it rebuilds with the new locale on next open.
    let locale_cfg = CONFIG.read().unwrap().i18n.locale.clone();
    i18n::apply_config_locale(&locale_cfg);
    refresh_menu_titles();
    invalidate_settings_window();
    clipboard::refresh_localized_ui();
}

/// 鼠标插拔回调(在鼠标线程执行)经 performSelectorOnMainThread 转到主线程后的重入点:
/// 设置窗口开着时即时刷新设备下拉框(重连后立即显示,无需点确定/重开)。
/// Re-entry point after the mouse-thread plug/unplug callback hops to the main thread via
/// performSelectorOnMainThread: refresh the settings device popup live (a reconnect shows
/// immediately, no OK/reopen needed).
extern "C" fn on_devices_changed(_self: *mut c_void, _cmd: Sel, _arg: *mut c_void) {
    unsafe {
        let is_main: bool = msg_send![class!(NSThread), isMainThread];
        if !is_main {
            let _: () = msg_send![_self as *mut AnyObject,
                performSelectorOnMainThread: sel!(handleDevicesChanged:),
                withObject: std::ptr::null::<AnyObject>(),
                waitUntilDone: false
            ];
            return;
        }
    }
    crate::settings::refresh_device_popup_if_open();
}

/// 缩略图捕获完成(worker 线程经 performSelectorOnMainThread 跳来):清空待投递
/// 队列并原位重建受影响卡片。
/// Thumbnail capture finished (hopped from the worker thread via
/// performSelectorOnMainThread): drains the pending queue and rebuilds the
/// affected cards in place.
extern "C" fn on_thumbnail_ready(_self: *mut c_void, _cmd: Sel, _arg: *mut c_void) {
    thumbnail::handle_ready_main();
}

// ========== Class Registration ==========

fn register_classes() {
    unsafe {
        // --- OhMyTabCardView : NSView ---
        let card_cls = {
            let name = CString::new("OhMyTabCardView").unwrap();
            let superclass = class!(NSView) as *const _ as *mut AnyObject;
            let cls = objc_allocateClassPair(superclass, name.as_ptr(), 0);
            let types_v_obj = CString::new("v@:@").unwrap();
            class_addMethod(
                cls,
                sel!(mouseDown:),
                card_mouse_down as *mut c_void,
                types_v_obj.as_ptr(),
            );
            class_addMethod(
                cls,
                sel!(mouseEntered:),
                card_mouse_entered as *mut c_void,
                types_v_obj.as_ptr(),
            );
            objc_registerClassPair(cls);
            cls
        };
        *CARD_CLASS.lock().unwrap() =
            Some(ObjClassPtr(card_cls as *const objc2::runtime::AnyClass));
    }
}

fn create_overlay_window() -> *mut AnyObject {
    unsafe {
        let screen: *mut AnyObject = msg_send![class!(NSScreen), mainScreen];
        let screen_frame: NSRect = msg_send![screen, frame];
        let h = window_height(6); // initial reasonable default
        let w = window_width(3); // initial three-card baseline; summon recalculates the live width
        let x = (screen_frame.size.width - w) / 2.0 + screen_frame.origin.x;
        let y = (screen_frame.size.height - h) / 2.0 + screen_frame.origin.y;
        let frame = NSRect::new(NSPoint::new(x, y), NSSize::new(w, h));

        // Borderless 窗口(styleMask = 0):无标题栏 -> 窗口可见形状 = 玻璃的圆角 alpha,
        // 消除"圆角玻璃 + 方角窗口"在四角留下的透明突出,且四角对称。
        // borderless 默认不能成为 key 窗口(收不到键盘),所以用自定义 NSPanel 子类
        // 重写 canBecomeKeyWindow -> YES。原先用 titled + 透明标题栏绕开此子类,代价就是
        // 四角不对称的透明突出(Regular 下尤为明显)。
        //
        // 关键:NSPanel + NSWindowStyleMaskNonactivatingPanel(1<<7) —— 面板成为 key 窗口时
        // **不激活所属 app**(BetterCmdTab 面板同款)。召唤时 app 保持非激活,设置窗口就不会
        // 被抬到活动 App 前面,切换器因此不再需要 stash/orderBack 机制。
        //
        // Borderless window (styleMask = 0): no title bar -> the window's visible shape equals
        // the glass's rounded alpha, eliminating the transparent protrusions left at the corners by
        // a rounded glass inside a square window, and keeping all four corners symmetric. A borderless
        // window can't become key by default (no keyboard input), so a custom NSPanel subclass
        // overrides canBecomeKeyWindow -> YES. The earlier titled + transparent-titlebar approach
        // avoided this subclass at the cost of those asymmetric transparent corners (especially
        // visible under the Regular glass style).
        //
        // Key: NSPanel + NSWindowStyleMaskNonactivatingPanel (1<<7) -- the panel becomes key
        // WITHOUT activating the owning app (same as BetterCmdTab's panel). While the app stays
        // inactive during summon, the settings window is never raised above the active app, so
        // the switcher no longer needs the stash/orderBack machinery.
        let style: u64 = 1 << 7; // NSWindowStyleMaskBorderless(0) | NSWindowStyleMaskNonactivatingPanel

        // 注册自定义窗口子类 OhMyTabOverlayWindow : NSPanel(仅重写 canBecomeKeyWindow)。
        // 仿 OhMyTabContainerView 的 inline 注册;create_overlay_window 只调用一次,无重复注册风险。
        // Register the custom window subclass OhMyTabOverlayWindow : NSPanel (only overrides
        // canBecomeKeyWindow). Inline, like OhMyTabContainerView; create_overlay_window is called
        // once, so no double-registration.
        let window_cls = {
            let name = CString::new("OhMyTabOverlayWindow").unwrap();
            let superclass = class!(NSPanel) as *const _ as *mut AnyObject;
            let cls = objc_allocateClassPair(superclass, name.as_ptr(), 0);
            let types_bool = CString::new("B@:").unwrap();
            class_addMethod(
                cls,
                sel!(canBecomeKeyWindow),
                overlay_window_can_become_key as *mut c_void,
                types_bool.as_ptr(),
            );
            objc_registerClassPair(cls);
            cls
        };

        let window: *mut AnyObject = msg_send![window_cls, alloc];
        let window: *mut AnyObject = msg_send![window, initWithContentRect: frame, styleMask: style, backing: 2u64, defer: false];

        // NSFloatingWindowLevel = 3 (should be above normal windows during app switch)
        let _: () = msg_send![window, setLevel: 3u64];

        // ========== Window transparency / Liquid Glass settings ==========
        //
        // (1) Window must be non-opaque so the compositor allows content
        //     behind the window to show through.
        let _: () = msg_send![window, setOpaque: false];
        //
        // (2) Window background must be clear, otherwise NSThemeFrame draws
        //     a solid color that blocks everything behind it.
        let clear_color: *mut AnyObject = msg_send![class!(NSColor), clearColor];
        let _: () = msg_send![window, setBackgroundColor: clear_color];
        //
        // (3) 关闭窗口阴影:NSGlassEffectView 自带 Liquid Glass 深度,窗口阴影是多余的。
        //     其强度随内容 alpha 变化(Regular 玻璃不透明 -> 强阴影圈;Clear 近透明 -> 无),
        //     会在 Regular 下沿玻璃边缘形成一圈多余暗环;且投影向下偏移,底角与顶角不一致
        //     (这正是 borderless 后"边上还有一圈、底角形状不同"的来源)。关掉后只留玻璃自身
        //     深度,边缘干净、四角一致。
        // (3) Disable the window shadow: NSGlassEffectView already provides Liquid Glass depth, so
        //     the window shadow is redundant. Its strength scales with content alpha (Regular's
        //     opaque glass -> a strong shadow ring; Clear's near-transparent glass -> none), which
        //     shows up under Regular as an unwanted dark ring along the glass edge; the drop shadow
        //     is also offset downward, so the bottom corners differ from the top (this is the "ring
        //     still on the edge, bottom corners shaped differently" seen after going borderless).
        //     With it off, only the glass's own depth remains -- clean edges, symmetric corners.
        let _: () = msg_send![window, setHasShadow: false];
        // =================================================================

        let _: () = msg_send![window, setReleasedWhenClosed: false];
        // Don't let the window hide on deactivate (we manage show/hide)
        let _: () = msg_send![window, setHidesOnDeactivate: false];

        // --- Liquid Glass ---
        // macOS 26+  → NSGlassEffectView  (new public API, built-in blur)
        // macOS <26 → NSVisualEffectView  (withinWindow + Dark material)
        let is_macos_26 = AnyClass::get(c"NSGlassEffectView").is_some();

        // The view that will contain the card container.
        // On macOS 26 this is the glass view's inner contentView;
        // on older macOS it's the NSVisualEffectView itself.
        let content_parent: *mut AnyObject;

        if is_macos_26 {
            let glass_cls = AnyClass::get(c"NSGlassEffectView").unwrap();
            let glass: *mut AnyObject = msg_send![glass_cls, alloc];
            let glass: *mut AnyObject = msg_send![glass, initWithFrame: NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(w, h))];
            *GLASS_VIEW.lock().unwrap() = Some(ObjPtr(glass)); // 保存指针，供热重载重新应用 / save for hot reload
                                                               // (4) Corner radius — native NSGlassEffectView property, from config.
            let _: () =
                msg_send![glass, setCornerRadius: CONFIG.read().unwrap().appearance.corner_radius];
            // (5) Glass style — "regular" (0) or "clear" (1), from config.
            let style: i64 = match config::effective_glass_style().as_str() {
                "clear" => 1,
                _ => 0, // regular (default)
            };
            let _: () = msg_send![glass, setStyle: style];
            // (6) Tint color — hex RRGGBBAA from config.
            let tint_hex = config::parse_hex8(&config::effective_glass_tint());
            let tint = hex_to_ns_color(tint_hex);
            let _: () = msg_send![glass, setTintColor: tint];
            // (7) Autoresizing so the glass view fills the window on resize.
            let _: () = msg_send![glass, setAutoresizingMask: 18u64];
            let _: () = msg_send![window, setContentView: glass];
            // NSGlassEffectView.contentView may be nil initially - create our own.
            let inner: *mut AnyObject = msg_send![class!(NSView), alloc];
            let inner: *mut AnyObject = msg_send![inner, initWithFrame: NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(w, h))];
            let _: () = msg_send![inner, setAutoresizingMask: 18u64];
            let _: () = msg_send![glass, setContentView: inner];
            // (6.5) 硬裁剪背景模糊:NSGlassEffectView 的 cornerRadius 属性只圆了着色/外观,背景模糊
            //       仍填满方角。给 layer 设 masksToBounds + cornerRadius 把模糊也裁进圆角(对
            //       NSVisualEffectView 是公认有效的做法,NSGlassEffectView 待验证)。
            //       放在 setContentView 之后 + 显式 setWantsLayer,确保 layer 已落实(非 nil),
            //       masksToBounds 真正生效;并打日志确认。
            // (6.5) Hard-clip the backdrop blur: NSGlassEffectView's cornerRadius property only rounds
            //       the tint/appearance, not the backdrop blur. Setting masksToBounds + cornerRadius on
            //       the layer clips the blur into the round (the standard trick for NSVisualEffectView;
            //       unverified for NSGlassEffectView). Done after setContentView + an explicit
            //       setWantsLayer so the layer is realized (non-nil) and masksToBounds actually takes
            //       effect; logged to confirm.
            let radius = CONFIG.read().unwrap().appearance.corner_radius;
            let _: () = msg_send![glass, setWantsLayer: true];
            let glass_layer: *mut AnyObject = msg_send![glass, layer];
            if !glass_layer.is_null() {
                let _: () = msg_send![glass_layer, setCornerRadius: radius];
                let _: () = msg_send![glass_layer, setMasksToBounds: true];
            }
            content_parent = inner;
        } else {
            let content: *mut AnyObject = msg_send![window, contentView];
            let ve: *mut AnyObject = msg_send![class!(NSVisualEffectView), alloc];
            let ve: *mut AnyObject = msg_send![ve, initWithFrame: NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(w, h))];
            // withinWindow blending + Dark material (same as the GPUI version used)
            let _: () = msg_send![ve, setBlendingMode: 1u64]; // WithinWindow
            let _: () = msg_send![ve, setMaterial: 12u64]; // Dark
            let _: () = msg_send![ve, setState: 1u64]; // Active
            let _: () = msg_send![ve, setAutoresizingMask: 18u64];
            let _: () = msg_send![content, addSubview: ve];
            content_parent = ve;
        }

        // --- Container view for cards ---
        // Register OhMyTabContainerView : NSClipView
        let container_cls = {
            let name = CString::new("OhMyTabContainerView").unwrap();
            let superclass = class!(NSClipView) as *const _ as *mut AnyObject;
            let cls = objc_allocateClassPair(superclass, name.as_ptr(), 0);
            let types_v_obj = CString::new("v@:@").unwrap();
            let types_bool = CString::new("B@:").unwrap();
            class_addMethod(
                cls,
                sel!(keyDown:),
                container_key_down as *mut c_void,
                types_v_obj.as_ptr(),
            );
            class_addMethod(
                cls,
                sel!(acceptsFirstResponder),
                container_accepts_first_responder as *mut c_void,
                types_bool.as_ptr(),
            );
            class_addMethod(
                cls,
                sel!(mouseMoved:),
                container_mouse_moved as *mut c_void,
                types_v_obj.as_ptr(),
            );
            class_addMethod(
                cls,
                sel!(mouseEntered:),
                container_mouse_entered as *mut c_void,
                types_v_obj.as_ptr(),
            );
            class_addMethod(
                cls,
                sel!(mouseExited:),
                container_mouse_exited as *mut c_void,
                types_v_obj.as_ptr(),
            );
            class_addMethod(
                cls,
                sel!(mouseDragged:),
                container_mouse_moved as *mut c_void,
                types_v_obj.as_ptr(),
            );
            class_addMethod(
                cls,
                sel!(scrollWheel:),
                container_scroll_wheel as *mut c_void,
                types_v_obj.as_ptr(),
            );
            objc_registerClassPair(cls);
            cls
        };

        // 自定义滚动条由 NSView 显式处理拖拽,避免非激活面板中的 NSScroller 原生 tracking 失效。
        // The custom scrollbar handles dragging explicitly, avoiding native NSScroller tracking
        // failures inside the nonactivating panel.
        let scroller_cls = {
            let name = CString::new("OhMyTabThumbnailScrollIndicator").unwrap();
            let superclass = class!(NSView) as *const _ as *mut AnyObject;
            let cls = objc_allocateClassPair(superclass, name.as_ptr(), 0);
            let types_rect = CString::new("v@:{CGRect={CGPoint=dd}{CGSize=dd}}").unwrap();
            class_addMethod(
                cls,
                sel!(drawRect:),
                thumbnail_scroller_draw_rect as *mut c_void,
                types_rect.as_ptr(),
            );
            let types_mouse = CString::new("v@:@").unwrap();
            class_addMethod(
                cls,
                sel!(mouseDown:),
                thumbnail_scroller_mouse_down as *mut c_void,
                types_mouse.as_ptr(),
            );
            class_addMethod(
                cls,
                sel!(mouseDragged:),
                thumbnail_scroller_mouse_dragged as *mut c_void,
                types_mouse.as_ptr(),
            );
            class_addMethod(
                cls,
                sel!(mouseUp:),
                thumbnail_scroller_mouse_up as *mut c_void,
                types_mouse.as_ptr(),
            );
            class_addMethod(
                cls,
                sel!(mouseEntered:),
                thumbnail_scroller_mouse_entered as *mut c_void,
                types_mouse.as_ptr(),
            );
            class_addMethod(
                cls,
                sel!(mouseMoved:),
                thumbnail_scroller_mouse_moved as *mut c_void,
                types_mouse.as_ptr(),
            );
            class_addMethod(
                cls,
                sel!(mouseExited:),
                thumbnail_scroller_mouse_exited as *mut c_void,
                types_mouse.as_ptr(),
            );
            let types_accepts_first_mouse = CString::new("B@:@").unwrap();
            class_addMethod(
                cls,
                sel!(acceptsFirstMouse:),
                thumbnail_scroller_accepts_first_mouse as *mut c_void,
                types_accepts_first_mouse.as_ptr(),
            );
            objc_registerClassPair(cls);
            cls
        };

        let container: *mut AnyObject = msg_send![container_cls, alloc];
        // 卡片容器只覆盖状态栏以上的区域,NSClipView 会裁掉连续滚动时越过边界的卡片。
        // The card container covers only the area above the status footer; NSClipView clips cards
        // as they move continuously across the viewport edges.
        let container: *mut AnyObject = msg_send![container, initWithFrame: NSRect::new(
            NSPoint::new(0.0, STATUS_H),
            NSSize::new(w, (h - STATUS_H).max(1.0))
        )];
        let _: () = msg_send![container, setAutoresizingMask: 18u64];
        let _: () = msg_send![container, setDrawsBackground: false];
        let _: () = msg_send![content_parent, addSubview: container];
        *CONTAINER.lock().unwrap() = Some(ObjPtr(container));

        // NSClipView 只负责可视窗口;卡片全部挂在持久 document view 上,滚动时只移动 bounds。
        // NSClipView is only the viewport; all cards live in a persistent document view and
        // scrolling moves the bounds instead of rebuilding the card hierarchy.
        let document: *mut AnyObject = msg_send![class!(NSView), alloc];
        let document: *mut AnyObject = msg_send![
            document,
            initWithFrame: NSRect::new(
                NSPoint::new(0.0, 0.0),
                NSSize::new(w, (h - STATUS_H).max(1.0))
            )
        ];
        let _: () = msg_send![document, setWantsLayer: false];
        let _: () = msg_send![container, setDocumentView: document];
        *CARD_DOCUMENT.lock().unwrap() = Some(ObjPtr(document));
        release_obj(document);

        // --- Status label at bottom (standard coords: y=0 is bottom) ---
        let status_font: *mut AnyObject = {
            let cfg = CONFIG.read().unwrap();
            msg_send![class!(NSFont), systemFontOfSize: cfg.fonts.status_bar_size, weight: cfg.fonts.status_bar_weight]
        };
        let status_color = hex_to_ns_color(0x999999ff);
        let status_label = make_centered_label("", status_font, status_color, 0.0, w, STATUS_H);
        let _: () = msg_send![content_parent, addSubview: status_label];
        *STATUS_LABEL.lock().unwrap() = Some(ObjPtr(status_label));

        // 指示器放在卡片容器外层,卡片重建不会改变它的 z-order 或中断显式拖拽。
        // Keep the indicator above the card container; rebuilding cards cannot change its z-order
        // or interrupt explicit dragging.
        let scroller: *mut AnyObject = msg_send![scroller_cls, alloc];
        let scroller: *mut AnyObject = msg_send![
            scroller,
            initWithFrame: NSRect::new(
                NSPoint::new(w - H_PADDING - THUMB_SCROLLBAR_W, STATUS_H),
                NSSize::new(THUMB_SCROLLBAR_W, (h - STATUS_H).max(1.0))
            )
        ];
        let _: () = msg_send![scroller, setWantsLayer: false];
        let _: () = msg_send![scroller, setHidden: true];
        let scroller_tracking_opts: u64 = 0x01 | 0x02 | 0x80 | 0x200;
        let scroller_tracking: *mut AnyObject = msg_send![class!(NSTrackingArea), alloc];
        let scroller_tracking: *mut AnyObject = msg_send![
            scroller_tracking,
            initWithRect: NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(THUMB_SCROLLBAR_W, (h - STATUS_H).max(1.0))),
            options: scroller_tracking_opts,
            owner: scroller,
            userInfo: std::ptr::null::<AnyObject>()
        ];
        let _: () = msg_send![scroller, addTrackingArea: scroller_tracking];
        release_obj(scroller_tracking);
        let _: () = msg_send![content_parent, addSubview: scroller];
        *THUMB_SCROLLER.lock().unwrap() = Some(ObjPtr(scroller));

        window
    }
}

fn create_controller() -> *mut AnyObject {
    unsafe {
        let name = CString::new("OhMyTabController").unwrap();
        let superclass = class!(NSObject) as *const _ as *mut AnyObject;
        let cls = objc_allocateClassPair(superclass, name.as_ptr(), 0);
        let types_v_obj = CString::new("v@:@").unwrap();
        class_addMethod(
            cls,
            sel!(handleCmdTabPressed:),
            on_cmd_tab_pressed as *mut c_void,
            types_v_obj.as_ptr(),
        );
        class_addMethod(
            cls,
            sel!(handleCmdShiftTabPressed:),
            on_cmd_shift_tab_pressed as *mut c_void,
            types_v_obj.as_ptr(),
        );
        class_addMethod(
            cls,
            sel!(handleCmdReleased:),
            on_cmd_released as *mut c_void,
            types_v_obj.as_ptr(),
        );
        class_addMethod(
            cls,
            sel!(closeCard:),
            on_close_card as *mut c_void,
            types_v_obj.as_ptr(),
        );
        class_addMethod(
            cls,
            sel!(onClipboardToggled:),
            clipboard::on_clipboard_toggle as *mut c_void,
            types_v_obj.as_ptr(),
        );
        class_addMethod(
            cls,
            sel!(handleAppActivation:),
            on_app_activated as *mut c_void,
            types_v_obj.as_ptr(),
        );
        class_addMethod(
            cls,
            sel!(handleAppLaunch:),
            on_app_launched as *mut c_void,
            types_v_obj.as_ptr(),
        );
        class_addMethod(
            cls,
            sel!(handleLocaleChanged:),
            on_locale_changed as *mut c_void,
            types_v_obj.as_ptr(),
        );
        class_addMethod(
            cls,
            sel!(handleDelayedOrderOut:),
            on_delayed_order_out as *mut c_void,
            types_v_obj.as_ptr(),
        );
        // 延迟一拍的切换抬升:释放/点击/回车先让 vanish 提交上屏,下一拍再激活+抬升。
        // Deferred switch raise: release/click/Enter commit the vanish to the screen first,
        // then activate+raise on the next runloop turn.
        class_addMethod(
            cls,
            sel!(handleDeferredRaise:),
            on_deferred_raise as *mut c_void,
            types_v_obj.as_ptr(),
        );
        class_addMethod(
            cls,
            sel!(handleDeferredScrollHover:),
            on_deferred_scroll_hover as *mut c_void,
            types_v_obj.as_ptr(),
        );
        class_addMethod(
            cls,
            sel!(handleDevicesChanged:),
            on_devices_changed as *mut c_void,
            types_v_obj.as_ptr(),
        );
        class_addMethod(
            cls,
            sel!(thumbnailReady:),
            on_thumbnail_ready as *mut c_void,
            types_v_obj.as_ptr(),
        );
        class_addMethod(
            cls,
            sel!(handleAppTerminate:),
            on_app_terminated as *mut c_void,
            types_v_obj.as_ptr(),
        );
        class_addMethod(
            cls,
            sel!(handleWindowRefresh:),
            on_window_refresh as *mut c_void,
            types_v_obj.as_ptr(),
        );
        class_addMethod(
            cls,
            sel!(handleWindowServerEvent:),
            on_window_server_event as *mut c_void,
            types_v_obj.as_ptr(),
        );
        objc_registerClassPair(cls);
        msg_send![cls, new]
    }
}

fn init_app() {
    unsafe {
        let nsapp: *mut AnyObject = msg_send![class!(NSApplication), sharedApplication];
        // NSApplicationActivationPolicyAccessory = 1
        let _: bool = msg_send![nsapp, setActivationPolicy: 1isize];
    }
}

/// 切换应用激活策略。设置窗口打开时切 .regular(进 Dock / 系统 Cmd+Tab / 调度中心图标),
/// 关闭时切回 .accessory(纯菜单栏)。LSUIElement 默认 .accessory,设置窗口需要 .regular
/// 才能正常激活抬升(打开设置时从别的 App 顶部弹出来)。
/// Switch the app activation policy: .regular while the settings window is open (so it can
/// activate normally and raise itself above the active app when opened), .accessory when
/// closed (pure menu-bar). LSUIElement defaults to .accessory.
pub(crate) fn set_settings_activation_policy(regular: bool) {
    unsafe {
        let nsapp: *mut AnyObject = msg_send![class!(NSApplication), sharedApplication];
        // NSApplicationActivationPolicyRegular = 0, .Accessory = 1
        let policy: isize = if regular { 0 } else { 1 };
        let _: bool = msg_send![nsapp, setActivationPolicy: policy];
    }
}

fn setup_status_bar() {
    unsafe {
        let status_bar: *mut AnyObject = msg_send![class!(NSStatusBar), systemStatusBar];
        // NSVariableStatusItemLength = -1.0:槽位按 button 内容自适应,sizeToFit 后贴合图标,
        // 不留多余边距。固定长度(曾经的 30.0)不会被 sizeToFit 缩小,槽位恒为 30pt,而图标只有
        // ~17pt,居中/靠左后两侧留空,看着和邻居图标之间有很大间距。
        // NSVariableStatusItemLength = -1.0: the slot auto-sizes to the button's content, so after
        // sizeToFit it hugs the icon with no extra padding. A fixed length (the former 30.0) is not
        // shrunk by sizeToFit, so the slot stays 30pt while the icon is ~17pt, leaving visible gaps
        // around the icon.
        let status_item: *mut AnyObject = msg_send![status_bar, statusItemWithLength: -1.0f64];
        let _: *mut AnyObject = msg_send![status_item, retain];

        let button: *mut AnyObject = msg_send![status_item, button];

        // Status bar icon: 单色 template PNG(两个矩形叠放,assets/statusbar-icon.png 嵌入二进制)。
        // setTemplate:YES 让系统按菜单栏前景色渲染(浅/深色 menu bar 都清晰);sizeToFit 让 button 贴合 image。
        // Status bar icon: monochrome template PNG (two overlapped rects, assets/statusbar-icon.png embedded
        // in the binary). setTemplate:YES makes the system render it in the menu bar foreground color
        // (clear on both light/dark menu bars); sizeToFit hugs the image.
        let png_bytes: &[u8] = include_bytes!("../assets/statusbar-icon.png");
        let nsdata: *mut AnyObject = msg_send![
            class!(NSData),
            dataWithBytes: png_bytes.as_ptr() as *const c_void,
            length: png_bytes.len()
        ];
        let image: *mut AnyObject = msg_send![class!(NSImage), alloc];
        let image: *mut AnyObject = msg_send![image, initWithData: nsdata];
        if !image.is_null() {
            // statusbar-icon.png 是 162x128(横向留白更宽,让图标与邻居间距更舒展)。
            // 设高度为 status bar 厚度、宽度按 PNG 纵横比等比缩放,保持图形像素大小不变、
            // 只放大左右空隙。强制方形(icon_size x icon_size)会压扁非方形 PNG。
            // statusbar-icon.png is 162x128 (wider horizontal margins so the icon sits looser
            // from its neighbors). Set the height to the status-bar thickness and scale the width
            // by the PNG aspect ratio, keeping the glyph pixel size while widening the gaps.
            // Forcing a square (icon_size x icon_size) would squash the non-square PNG.
            let icon_size: f64 = msg_send![status_bar, thickness];
            let aspect: f64 = 162.0 / 128.0; // statusbar-icon.png 纵横比 / PNG aspect ratio
            let _: () = msg_send![image, setSize: NSSize::new(icon_size * aspect, icon_size)];
            let is_template: bool = true;
            let _: () = msg_send![image, setTemplate: is_template];
            let _: () = msg_send![button, setImage: image];
            // NSImageOnly = 1
            let _: () = msg_send![button, setImagePosition: 1usize];
            let _: () = msg_send![image, release]; // button retained it; drop our alloc +1
        } else {
            let ns_title = make_nsstring("Tab");
            let _: () = msg_send![button, setTitle: ns_title];
            CFRelease(ns_title as *const c_void);
        }

        let _: () = msg_send![button, sizeToFit];
        let _: () = msg_send![button, setNeedsDisplay: true];

        // Build menu
        let menu_title = make_nsstring("");
        let menu: *mut AnyObject = msg_send![class!(NSMenu), alloc];
        let menu: *mut AnyObject = msg_send![menu, initWithTitle: menu_title];
        CFRelease(menu_title as *const c_void);

        // Menu action target class
        let action_cls = {
            let name = CString::new("OhMyTabMenuTarget2").unwrap();
            let superclass: *const objc2::runtime::AnyClass = class!(NSObject);
            let cls = objc_allocateClassPair(superclass as *mut AnyObject, name.as_ptr(), 0);
            if cls.is_null() {
                log_info!("Failed to allocate ObjC class for menu target.");
                return;
            }
            let types = CString::new("v@:@").unwrap();
            class_addMethod(
                cls,
                sel!(handleQuit:),
                handle_quit as *mut c_void,
                types.as_ptr(),
            );
            class_addMethod(
                cls,
                sel!(handleToggleShortcut:),
                handle_toggle_shortcut as *mut c_void,
                types.as_ptr(),
            );
            class_addMethod(
                cls,
                sel!(handleToggleThumbnail:),
                handle_toggle_thumbnail as *mut c_void,
                types.as_ptr(),
            );
            class_addMethod(
                cls,
                sel!(handleReloadConfig:),
                handle_reload_config as *mut c_void,
                types.as_ptr(),
            );
            class_addMethod(
                cls,
                sel!(handleClearIconCache:),
                handle_clear_icon_cache as *mut c_void,
                types.as_ptr(),
            );
            class_addMethod(
                cls,
                sel!(handleSettings:),
                on_settings_open as *mut c_void,
                types.as_ptr(),
            );
            class_addMethod(
                cls,
                sel!(handleSettingsOk:),
                on_settings_ok as *mut c_void,
                types.as_ptr(),
            );
            class_addMethod(
                cls,
                sel!(handleSettingsCancel:),
                on_settings_cancel as *mut c_void,
                types.as_ptr(),
            );
            class_addMethod(
                cls,
                sel!(handleGlassTintChanged:),
                on_glass_tint_changed as *mut c_void,
                types.as_ptr(),
            );
            class_addMethod(
                cls,
                sel!(handleGlassTintPanelChanged:),
                on_glass_tint_panel_changed as *mut c_void,
                types.as_ptr(),
            );
            class_addMethod(
                cls,
                sel!(handleGlassTintPanelWillClose:),
                on_glass_tint_panel_will_close as *mut c_void,
                types.as_ptr(),
            );
            class_addMethod(
                cls,
                sel!(handleGlassTintReset:),
                on_glass_tint_reset as *mut c_void,
                types.as_ptr(),
            );
            class_addMethod(
                cls,
                sel!(handleGlassStyleChanged:),
                on_glass_style_changed as *mut c_void,
                types.as_ptr(),
            );
            class_addMethod(
                cls,
                sel!(handleSettingsSidebar:),
                on_sidebar_select as *mut c_void,
                types.as_ptr(),
            );
            class_addMethod(
                cls,
                sel!(handleRestoreDefaults:),
                handle_restore_defaults as *mut c_void,
                types.as_ptr(),
            );
            class_addMethod(
                cls,
                sel!(handleOpenPrivacy:),
                handle_open_privacy as *mut c_void,
                types.as_ptr(),
            );
            class_addMethod(
                cls,
                sel!(handleEnableMouseToggle:),
                handle_enable_mouse_toggle as *mut c_void,
                types.as_ptr(),
            );
            class_addMethod(
                cls,
                sel!(handleDeviceChanged:),
                handle_device_changed as *mut c_void,
                types.as_ptr(),
            );
            class_addMethod(
                cls,
                sel!(handleScrollModeChanged:),
                handle_scroll_mode_changed as *mut c_void,
                types.as_ptr(),
            );
            class_addMethod(
                cls,
                sel!(handleLineCountChanged:),
                handle_line_count_changed as *mut c_void,
                types.as_ptr(),
            );
            class_addMethod(
                cls,
                sel!(handleAddMapping:),
                handle_add_mapping as *mut c_void,
                types.as_ptr(),
            );
            class_addMethod(
                cls,
                sel!(handleMappingEnabledChanged:),
                handle_mapping_enabled_changed as *mut c_void,
                types.as_ptr(),
            );
            class_addMethod(
                cls,
                sel!(handleMappingEdit:),
                handle_mapping_edit as *mut c_void,
                types.as_ptr(),
            );
            class_addMethod(
                cls,
                sel!(handleDeleteMapping:),
                handle_delete_mapping as *mut c_void,
                types.as_ptr(),
            );
            class_addMethod(
                cls,
                sel!(handlePanelRecordTrigger:),
                handle_panel_record_trigger as *mut c_void,
                types.as_ptr(),
            );
            class_addMethod(
                cls,
                sel!(handlePanelRecordCombo:),
                handle_panel_record_combo as *mut c_void,
                types.as_ptr(),
            );
            class_addMethod(
                cls,
                sel!(handlePanelActionChanged:),
                handle_panel_action_changed as *mut c_void,
                types.as_ptr(),
            );
            class_addMethod(
                cls,
                sel!(handleMappingConfirm:),
                handle_mapping_confirm as *mut c_void,
                types.as_ptr(),
            );
            class_addMethod(
                cls,
                sel!(handleMappingCancel:),
                handle_mapping_cancel as *mut c_void,
                types.as_ptr(),
            );
            class_addMethod(
                cls,
                sel!(handleRecordingFinished:),
                handle_recording_finished as *mut c_void,
                types.as_ptr(),
            );
            class_addMethod(
                cls,
                sel!(handleRecordingCancelled:),
                handle_recording_cancelled as *mut c_void,
                types.as_ptr(),
            );
            objc_registerClassPair(cls);
            cls
        };
        let menu_target: *mut AnyObject = msg_send![action_cls as *const AnyObject, new];
        *MENU_TARGET.lock().unwrap() = Some(ObjPtr(menu_target));

        // 设置 item 放在菜单第一项,用于打开设置窗口。
        // Settings item comes first and opens the settings window.
        let settings_title = make_nsstring(&t("menu.settings"));
        let settings_key = make_nsstring("");
        let settings_item: *mut AnyObject = msg_send![class!(NSMenuItem), alloc];
        let settings_item: *mut AnyObject = msg_send![settings_item, initWithTitle: settings_title, action: sel!(handleSettings:), keyEquivalent: settings_key];
        CFRelease(settings_title as *const c_void);
        CFRelease(settings_key as *const c_void);
        let _: () = msg_send![settings_item, setTarget: menu_target];
        let _: () = msg_send![menu, addItem: settings_item];
        // 设置与下方操作项之间的分隔线,样式与退出项上方一致。
        // Separate Settings from the action items below, matching the separator above Quit.
        let settings_separator: *mut AnyObject = msg_send![class!(NSMenuItem), separatorItem];
        let _: () = msg_send![menu, addItem: settings_separator];

        // Shortcut toggle item
        let shortcut_title = make_nsstring(&t("menu.toggle_shortcut.cmd"));
        let shortcut_key = make_nsstring("");
        let shortcut_item: *mut AnyObject = msg_send![class!(NSMenuItem), alloc];
        let shortcut_item: *mut AnyObject = msg_send![shortcut_item, initWithTitle: shortcut_title, action: sel!(handleToggleShortcut:), keyEquivalent: shortcut_key];
        CFRelease(shortcut_title as *const c_void);
        CFRelease(shortcut_key as *const c_void);
        let _: () = msg_send![shortcut_item, setTarget: menu_target];
        let _: () = msg_send![menu, addItem: shortcut_item];
        *SHORTCUT_ITEM.lock().unwrap() = Some(ShortcutState {
            item: shortcut_item,
        });

        // 缩略图/纯图标模式切换项,紧跟快捷键模式切换项。
        // Thumbnail/icon-only mode toggle, placed immediately below the shortcut toggle.
        let thumbnail_title_key = if CONFIG.read().unwrap().layout.thumbnails_enabled {
            "menu.toggle_thumbnail_mode.to_icons"
        } else {
            "menu.toggle_thumbnail_mode.to_thumbnails"
        };
        let thumbnail_title = make_nsstring(&t(thumbnail_title_key));
        let thumbnail_key = make_nsstring("");
        let thumbnail_item: *mut AnyObject = msg_send![class!(NSMenuItem), alloc];
        let thumbnail_item: *mut AnyObject = msg_send![thumbnail_item, initWithTitle: thumbnail_title, action: sel!(handleToggleThumbnail:), keyEquivalent: thumbnail_key];
        CFRelease(thumbnail_title as *const c_void);
        CFRelease(thumbnail_key as *const c_void);
        let _: () = msg_send![thumbnail_item, setTarget: menu_target];
        let _: () = msg_send![menu, addItem: thumbnail_item];
        *THUMBNAIL_ITEM.lock().unwrap() = Some(ThumbnailState {
            item: thumbnail_item,
        });

        // Reload Config item
        let reload_title = make_nsstring(&t("menu.reload_config"));
        let reload_key = make_nsstring("");
        let reload_item: *mut AnyObject = msg_send![class!(NSMenuItem), alloc];
        let reload_item: *mut AnyObject = msg_send![reload_item, initWithTitle: reload_title, action: sel!(handleReloadConfig:), keyEquivalent: reload_key];
        CFRelease(reload_title as *const c_void);
        CFRelease(reload_key as *const c_void);
        let _: () = msg_send![reload_item, setTarget: menu_target];
        let _: () = msg_send![menu, addItem: reload_item];

        // Clear Icon Cache item
        let clear_cache_title = make_nsstring(&t("menu.clear_icon_cache"));
        let clear_cache_key = make_nsstring("");
        let clear_cache_item: *mut AnyObject = msg_send![class!(NSMenuItem), alloc];
        let clear_cache_item: *mut AnyObject = msg_send![clear_cache_item, initWithTitle: clear_cache_title, action: sel!(handleClearIconCache:), keyEquivalent: clear_cache_key];
        CFRelease(clear_cache_title as *const c_void);
        CFRelease(clear_cache_key as *const c_void);
        let _: () = msg_send![clear_cache_item, setTarget: menu_target];
        let _: () = msg_send![menu, addItem: clear_cache_item];

        // Separator
        let sep_item: *mut AnyObject = msg_send![class!(NSMenuItem), separatorItem];
        let _: () = msg_send![menu, addItem: sep_item];

        // Quit item
        let quit_title = make_nsstring(&t("menu.quit"));
        // 绑定 Cmd+Q:仅在本 app 为前台(如设置窗口打开)时生效——菜单栏常驻 app
        // 在后台时 Cmd+Q 由当前前台 app 处理,这是 macOS 语义。
        // Bind Cmd+Q: only effective while this app is frontmost (e.g. settings window open) --
        // when it's a background menu-bar app, Cmd+Q goes to the frontmost app per macOS semantics.
        let quit_key = make_nsstring("q");
        let quit_item: *mut AnyObject = msg_send![class!(NSMenuItem), alloc];
        let quit_item: *mut AnyObject = msg_send![quit_item, initWithTitle: quit_title, action: sel!(handleQuit:), keyEquivalent: quit_key];
        CFRelease(quit_title as *const c_void);
        CFRelease(quit_key as *const c_void);
        let _: () = msg_send![quit_item, setTarget: menu_target];
        let _: () = msg_send![menu, addItem: quit_item];

        // 登记固定标题项,供热重载 locale 时批量重设标题 / register fixed-title items for locale hot-reload
        *FIXED_MENU_ITEMS.lock().unwrap() = Some(FixedMenuItems {
            settings: settings_item,
            reload: reload_item,
            clear_cache: clear_cache_item,
            quit: quit_item,
        });

        let _: () = msg_send![status_item, setMenu: menu];

        // 同时设为 mainMenu:让 AppKit 的 Cmd+Q keyEquivalent 分发能找到 Quit 项
        // (accessory app 的 mainMenu 不会显示在菜单栏最左侧——那里只显示 regular app
        // 的菜单,但快捷键路由仍生效,同 LinearMouse 的 storyboard mainMenu 机制)。
        // Also set as mainMenu so AppKit's Cmd+Q keyEquivalent dispatch can find the Quit item.
        // An accessory app's mainMenu is NOT shown in the menu bar's app area (that only shows
        // regular apps' menus), but key-equivalent routing still works -- same mechanism as
        // LinearMouse's storyboard mainMenu.
        let nsapp: *mut AnyObject = msg_send![class!(NSApplication), sharedApplication];
        let _: () = msg_send![nsapp, setMainMenu: menu];

        // Pump run loop to let SystemUIServer connect
        for _ in 0..10 {
            CFRunLoopRunInMode(kCFRunLoopDefaultMode, 0.001, 1u8);
        }
    }
}

// ========== Main ==========

/// 打开「系统设置 -> 隐私与安全性 -> 辅助功能」面板(深链)。
/// 供启动告警框与设置里的警告条按钮共用。
/// Open System Settings -> Privacy & Security -> Accessibility (deep link).
/// Shared by the startup alert and the settings warning banner's button.
pub(crate) fn open_privacy_accessibility() {
    unsafe {
        let url_str = make_nsstring(
            "x-apple.systempreferences:com.apple.preference.security?Privacy_Accessibility",
        );
        let url: *mut AnyObject = msg_send![class!(NSURL), URLWithString: url_str];
        CFRelease(url_str as *const c_void);
        if !url.is_null() {
            let ws: *mut AnyObject = msg_send![class!(NSWorkspace), sharedWorkspace];
            let _: bool = msg_send![ws, openURL: url];
        }
    }
}

/// 启动时若缺 Accessibility 权限,弹自定义告警框引导用户去授权。
/// 事件监听线程已在后台有限次重试,用户授权后 tap 会自动建成,无需重启。
/// Prompts the user with a custom alert at launch if Accessibility permission is missing.
/// The event-monitor thread is already retrying in the background; once the user grants
/// permission the tap is created automatically - no restart needed.
fn prompt_accessibility_if_needed() {
    if has_accessibility_permission() {
        return;
    }
    // AppState::new 已记过 "No accessibility permission." 日志,这里只负责弹框,不重复记。
    // AppState::new already logged "No accessibility permission."; this only shows the alert.
    // 辅助应用无 Dock 图标,主动激活以免告警框被其它窗口遮挡。
    // Accessory apps have no Dock icon; activate so the alert isn't hidden behind other windows.
    unsafe {
        let nsapp: *mut AnyObject = msg_send![class!(NSApplication), sharedApplication];
        let _: () = msg_send![nsapp, activateIgnoringOtherApps: true];
    }
    let open = confirm_alert(
        &t("alert.accessibility_title"),
        &t("alert.accessibility_msg"),
        &t("alert.accessibility_open"),
        &t("alert.accessibility_later"),
    );
    if open {
        open_privacy_accessibility();
    }
}

fn main() {
    // 冒烟测试入口(--smoke-clipboard):在真实主线程 + NSApplication 环境里两次显示
    // 剪贴板浮窗(覆盖 rebuild_rows 行清理路径),成功 exit(0)。由 clipboard 模块的
    // #[ignore] 测试以子进程方式调用——测试 harness 的工作线程会被 AppKit 主线程限制拦下。
    // Smoke-test entry (--smoke-clipboard): show the clipboard picker twice on the real main
    // thread inside a real NSApplication (exercising rebuild_rows' row cleanup), exit(0) on
    // success. Invoked as a subprocess by the #[ignore] test in clipboard.rs -- the test
    // harness's worker threads trip AppKit's main-thread guard.
    if std::env::args().any(|a| a == "--smoke-clipboard") {
        unsafe {
            let pool: *mut AnyObject = msg_send![class!(NSAutoreleasePool), new];
            init_app();
            // 冒烟模式:历史/缓存隔离到专用目录,绝不能写入用户的真实数据。
            // Smoke mode: isolate the history/cache into a dedicated directory so the
            // injected entries can never touch the user's real data.
            clipboard::set_smoke_mode();
            drop(CONFIG.read().unwrap()); // 触发 CONFIG 初始化,与正常启动一致
            let ok = clipboard::smoke_runner();
            let _: () = msg_send![pool, drain];
            if !ok {
                eprintln!("[smoke-clipboard] picker smoke failed");
                std::process::exit(1);
            }
            std::process::exit(0);
        }
    }

    // 1. Init NSApplication as accessory (no dock icon)
    init_app();

    // 1b. 初始化 logger:早于一切,从 CONFIG 读日志级别,根据 cargo run / .app 决定输出目标。
    //     Init logger: before everything else, read log level from CONFIG, auto-detect dev/prod.
    {
        let cfg = CONFIG.read().unwrap(); // 触发 LazyLock 初始化和 config 加载 / triggers LazyLock init + config load
        let level = match cfg.logging.level.as_str() {
            "debug" => logger::LogLevel::Debug,
            _ => logger::LogLevel::Info,
        };
        let is_dev = std::env::current_exe()
            .map(|p| p.to_string_lossy().contains("target/"))
            .unwrap_or(false);
        logger::init(
            &logger::LogConfig {
                level,
                file_path: cfg.logging.file_path.clone(),
            },
            is_dev,
        );
    }

    // 2. Register custom ObjC classes
    register_classes();

    // 2b. 强制 CONFIG 初始化(顺带应用 i18n locale),保证菜单按配置 locale 构建。
    //     CONFIG 的 LazyLock 初始化会调用 i18n::apply_config_locale,无循环依赖。
    // Force CONFIG init (also applies i18n locale) so the menu is built with the configured
    // locale. CONFIG's LazyLock init calls i18n::apply_config_locale; no cycle (see i18n.rs).
    drop(CONFIG.read().unwrap());

    // 3. Setup status bar menu
    setup_status_bar();

    // 3a. 按配置初始化快捷键模式:SHORTCUT_IS_CMD 默认 false(Option),启动时必须从 config 读回,
    //     否则设置里改的 modifier 重启后会丢失。放在 refresh_menu_titles 之前,让菜单标题刷新时
    //     读到的已是正确值。
    // Initialize shortcut mode from config: SHORTCUT_IS_CMD defaults to false (Option) and must be
    // read back at startup, otherwise the modifier chosen in Settings is lost on restart. Runs
    // before refresh_menu_titles so the label refresh sees the correct value.
    set_shortcut_mode(CONFIG.read().unwrap().keyboard.modifier == "command");

    // 同步开机自启(SMAppService):TOML [startup] launch_at_login 为唯一事实源。
    // 仅以 .app 方式启动时生效(cargo run 裸二进制下 mainApp 不可用,会记 warn,不影响其它功能)。
    // Sync launch-at-login (SMAppService): TOML's [startup] launch_at_login is the source of truth.
    // Only effective when launched as a .app (raw `cargo run` has no main bundle -> logs a warn, no other impact).
    autostart::sync(CONFIG.read().unwrap().startup.launch_at_login);

    // 3b. 按实际主题修正初始菜单标签。setup_status_bar 用占位标题(is_dark=false +
    //     "切换深色");若 config 主题为 dark/auto,这里修正为正确的 toggle 标签。
    //     locale 已在 2b 应用,菜单文本本身已正确,此处只修正 toggle 方向。
    // Fix initial menu labels against the real theme. setup_status_bar used a placeholder
    // (is_dark=false + "switch to dark"); if the config theme is dark/auto this corrects the
    // toggle direction. Locale was applied in 2b so text is already correct; this only fixes
    // the toggle direction.
    refresh_menu_titles();

    // 4. Initialize state
    ensure_icon_cache_dir();
    // 一次性清理旧版按 PID 命名的缓存文件(纯数字 stem 的 .png),它们对新版无用、只会占地方。
    // One-shot cleanup of legacy PID-named cache files (purely-numeric-stem .png); useless to
    // the new version and just take up space.
    migrate_legacy_cache();
    cache_running_app_icons(); // pre-warm icon cache for all running apps
                               // 剪贴板标题栏小图标预热:仅当剪贴板功能开启时才生成,否则跳过(避免无谓的提取)。
                               // Pre-warm the clipboard header's small icons: only when the clipboard feature is enabled,
                               // otherwise skip (no point extracting for a disabled feature).
    if CONFIG.read().unwrap().clipboard.enabled {
        cache_running_app_icons_small();
    }

    // 4b. Force CONFIG to initialise and report any validation errors
    {
        let cfg = CONFIG.read().unwrap();
        // First load already happened via LazyLock; re-run validate to report problems
        let errs = cfg.validate();
        if !errs.is_empty() {
            log_info!(
                "Config errors in ~/.config/oh-my-tab/config.toml ({} issue(s)):",
                errs.len()
            );
            for e in &errs {
                log_info!("  • {}", e);
            }
            log_info!("Using defaults for invalid fields.");
        }
    }

    *TAB_STATE.lock().unwrap() = Some(AppState::new());

    // 5. Create overlay window (hidden initially)
    let window = create_overlay_window();
    *OVERLAY_WINDOW.lock().unwrap() = Some(ObjPtr(window));
    // Hide initially
    hide_overlay();
    // 点击浮窗外部 → 取消切换(面板失去 key 时收起,同 Esc 语义)。
    // A click outside the overlay cancels the switch (dismissed when the panel loses key).
    install_click_to_cancel();

    // 6. Create controller object
    let controller = create_controller();
    *CONTROLLER.lock().unwrap() = Some(ObjPtr(controller));

    // 固定 worker 承接 App 激活后的 AX 聚焦查询，避免通知风暴时创建大量线程。
    // Start bounded workers for post-activation AX focus queries so notification bursts do not
    // create large numbers of threads.
    start_activation_focus_scheduler();
    window_server::start();
    let initial_subscriptions = window_collector::window_server_candidates();
    window_server::update_subscriptions(&initial_subscriptions);

    // 6b. Listen for system app activation so MRU stays in sync
    // when the user switches apps via Dock, Cmd+Tab, etc.
    unsafe {
        let ws: *mut AnyObject = msg_send![class!(NSWorkspace), sharedWorkspace];
        let nc: *mut AnyObject = msg_send![ws, notificationCenter];
        let name = make_nsstring("NSWorkspaceDidActivateApplicationNotification");
        let _: () = msg_send![nc,
            addObserver: controller,
            selector: sel!(handleAppActivation:),
            name: name,
            object: std::ptr::null::<AnyObject>(),
        ];
        CFRelease(name as *const c_void);

        // Pre-cache icons for apps launched after startup so they're ready
        // before the user summons the switcher (fixes missing icons for apps
        // opened while oh-my-tab is already running).
        let launch_name = make_nsstring("NSWorkspaceDidLaunchApplicationNotification");
        let _: () = msg_send![nc,
            addObserver: controller,
            selector: sel!(handleAppLaunch:),
            name: launch_name,
            object: std::ptr::null::<AnyObject>(),
        ];
        CFRelease(launch_name as *const c_void);

        // App 退出通知:缩略图模块据此取消待处理捕获并清理 observer/缓存。
        // App-terminate notice: the thumbnail module cancels pending captures and
        // removes the app's observer/cache entries.
        let term_name = make_nsstring("NSWorkspaceDidTerminateApplicationNotification");
        let _: () = msg_send![nc,
            addObserver: controller,
            selector: sel!(handleAppTerminate:),
            name: term_name,
            object: std::ptr::null::<AnyObject>(),
        ];
        CFRelease(term_name as *const c_void);

        // 监听系统语言变更,locale 为 auto 时实时跟随。NSLocaleCurrentLocaleDidChangeNotification
        // 投递在默认通知中心(不是 workspace 中心),故用 NSNotificationCenter defaultCenter。
        // Listen for system language changes to live-follow when locale is auto.
        // NSLocaleCurrentLocaleDidChangeNotification is posted to the default notification center
        // (not the workspace center), so use NSNotificationCenter defaultCenter.
        let default_nc: *mut AnyObject = msg_send![class!(NSNotificationCenter), defaultCenter];
        let locale_name = make_nsstring("NSLocaleCurrentLocaleDidChangeNotification");
        let _: () = msg_send![default_nc,
            addObserver: controller,
            selector: sel!(handleLocaleChanged:),
            name: locale_name,
            object: std::ptr::null::<AnyObject>(),
        ];
        CFRelease(locale_name as *const c_void);
    }

    // 7. Start event monitor + bridge thread
    let (event_tx, event_rx) = flume::unbounded();
    let _monitor = start_event_monitor(event_tx.clone());
    STATUS_EVENT_TX.set(event_tx).ok();

    // 7b. Start the mouse event tap only if enabled in config.
    // 鼠标事件 tap:仅在配置启用时启动(start 幂等,日志在 mouse::start 内)。
    // Mouse event tap: start only if enabled (start is idempotent; logging lives inside).
    let mouse_enabled = CONFIG.read().map(|c| c.mouse.enabled).unwrap_or(false);
    if mouse_enabled {
        mouse::start();
    }

    // 7b2. hover 轮询定时器在浮窗显示/隐藏时由 overlay 自行启停(show_overlay 调用
    // start_hover_timer),无需在此启动:主线程 runloop 每 16ms 读全局鼠标位置命中
    // 卡片,不依赖事件投递(侧键按住期间移动事件无法通过任何 tap/tracking 获取,实测)。
    // The hover poll timer is started/stopped by the overlay itself (start_hover_timer
    // from show_overlay): the main-thread runloop reads the global cursor position every
    // 16ms and hit-tests the cards, independent of event delivery (moves while a side
    // button is held can't be obtained via any tap/tracking, verified).

    // 7c. Apply pointer settings (disable system acceleration if configured).
    // 指针设置(配置了禁用系统加速时立即生效)。
    mouse::pointer::apply();

    // 7d. Start the clipboard-history poller only if enabled in config.
    // 剪贴板历史轮询:仅在配置启用时启动(幂等)。
    // Clipboard-history polling: start only if enabled (idempotent).
    let clip_enabled = CONFIG.read().map(|c| c.clipboard.enabled).unwrap_or(false);
    if clip_enabled {
        clipboard::start();
    }

    // 7e. 窗口缩略图服务:有界启动预热 + 常驻 AXObserver + 内存 LRU。无屏幕录制
    // 权限时启动预热跳过、worker 每任务前 preflight,浮窗保持纯图标渲染。
    // Window thumbnails: bounded startup prewarming + resident AXObserver + memory
    // LRU. Without Screen Recording permission startup prewarming is skipped and
    // the worker preflights each job, while the overlay keeps icon-only rendering.
    let thumbs_enabled = CONFIG
        .read()
        .map(|c| c.layout.thumbnails_enabled)
        .unwrap_or(false);
    if thumbs_enabled {
        thumbnail::start();
    }

    // 内存采样线程放在可选模块启动之后,这样 60s 基线对应完整的功能画像。
    // Start memory sampling after optional modules so the 60s baseline represents the
    // complete feature profile.
    mem::start();

    // Bridge thread: flume events → main thread via performSelectorOnMainThread
    thread::spawn(move || {
        while let Ok(event) = event_rx.recv() {
            let action = match event {
                GlobalEvent::CmdTabPressed => sel!(handleCmdTabPressed:),
                GlobalEvent::CmdShiftTabPressed => sel!(handleCmdShiftTabPressed:),
                GlobalEvent::CmdReleased => sel!(handleCmdReleased:),
                GlobalEvent::ClipboardToggled => sel!(onClipboardToggled:),
            };
            // Read controller pointer from static (only written once, safe to read)
            let ctrl = CONTROLLER.lock().unwrap().unwrap().0;
            unsafe {
                let _: () = msg_send![ctrl,
                    performSelectorOnMainThread: action,
                    withObject: std::ptr::null::<AnyObject>(),
                    waitUntilDone: false
                ];
            }
        }
        log_info!("Bridge thread exiting.");
    });

    // 冒烟测试入口(--smoke-overlay):完整初始化后直接驱动召唤路径，再遍历并循环
    // 一次窗口列表并反向一步，覆盖超量布局的连续滚动/双向回绕；随后泵 2 秒主 runloop 让异步
    // 缩略图投递(thumbnailReady)落地，无崩溃 exit(0)。用于无头验证 Cmd+Tab
    // 链路(合成按键到不了 CGEventTap，无法从外部触发)。
    // Smoke-test entry (--smoke-overlay): after full init, drive a summon directly,
    // then traverse, wrap, and step backward once to cover continuous scrolling and bidirectional navigation. Pump
    // the main runloop for 2s so async thumbnail deliveries (thumbnailReady) land,
    // and exit(0) on survival. Synthetic keystrokes never reach the CGEventTap, so
    // this path must be driven internally.
    if std::env::args().any(|a| a == "--smoke-overlay") {
        unsafe {
            let nsapp: *mut AnyObject = msg_send![class!(NSApplication), sharedApplication];
            let _: () = msg_send![nsapp, finishLaunching];
            log_info!("[smoke-overlay] summoning");
            on_cmd_tab_pressed(
                std::ptr::null_mut(),
                sel!(handleCmdTabPressed:),
                std::ptr::null_mut(),
            );
            if let Some(scroller) = thumbnail_scroller() {
                let hidden: bool = msg_send![scroller.0, isHidden];
                let bounds: NSRect = msg_send![scroller.0, bounds];
                let geometry = thumbnail_scroller_geometry(
                    bounds.size.height,
                    thumbnail_scroller_max_offset(),
                    0.0,
                );
                log_info!(
                    "[smoke-overlay] scroller hidden={} frame={:.1}x{:.1} knob_h={:.1}",
                    hidden,
                    bounds.size.width,
                    bounds.size.height,
                    geometry.map_or(0.0, |g| g.knob_h)
                );
            }
            let window_count = TAB_STATE
                .lock()
                .unwrap()
                .as_ref()
                .map_or(0, |state| state.windows.len());
            for _ in 0..window_count.saturating_add(1) {
                on_cmd_tab_pressed(
                    std::ptr::null_mut(),
                    sel!(handleCmdTabPressed:),
                    std::ptr::null_mut(),
                );
            }
            on_cmd_shift_tab_pressed(
                std::ptr::null_mut(),
                sel!(handleCmdShiftTabPressed:),
                std::ptr::null_mut(),
            );
            // 直接驱动同一套绝对偏移换算,覆盖小数位置与底部钳制;真实拖拽和滚轮也复用该路径。
            // Drive the same absolute-offset path to cover fractional positions and bottom
            // clamping; real dragging and wheel scrolling use this path too.
            for fraction in [0.35f64, 1.0f64] {
                thumbnail_scroller_set_fraction_for_smoke(fraction);
            }
            let rl: *mut AnyObject = msg_send![class!(NSRunLoop), currentRunLoop];
            let date: *mut AnyObject =
                msg_send![class!(NSDate), dateWithTimeIntervalSinceNow: 2.0f64];
            let _: () = msg_send![rl, runUntilDate: date];
            log_info!("[smoke-overlay] summon survived");
            std::process::exit(0);
        }
    }

    // 8. Run the main event loop (blocks until [NSApp terminate:])
    unsafe {
        let nsapp: *mut AnyObject = msg_send![class!(NSApplication), sharedApplication];
        let _: () = msg_send![nsapp, finishLaunching];
        // 启动后若缺 Accessibility 权限,弹告警框引导授权(事件监听线程已在后台有限次重试)。
        // Prompt for Accessibility if missing (the event-monitor thread is already retrying in the background).
        prompt_accessibility_if_needed();
        let _: () = msg_send![nsapp, run];
    }
}

#[cfg(test)]
mod tests {
    use super::{merge_refreshed_windows, selection_index_after_refresh, WindowRefreshReason};
    use crate::window_collector::WindowInfo;

    fn window(pid: i32, window_id: u32) -> WindowInfo {
        WindowInfo {
            pid,
            window_id,
            app_name: format!("App {pid}"),
            window_title: format!("Window {window_id}"),
            icon_path: None,
            is_active: false,
            minimized: false,
            bounds: (0.0, 0.0, 100.0, 100.0),
        }
    }

    #[test]
    fn lifecycle_refresh_does_not_bump_frontmost() {
        assert!(!WindowRefreshReason::Lifecycle.bumps_frontmost());
        assert!(WindowRefreshReason::Summon.bumps_frontmost());
    }

    #[test]
    fn pending_refresh_keeps_summon_priority() {
        assert_eq!(
            WindowRefreshReason::Lifecycle.merge(WindowRefreshReason::Lifecycle),
            WindowRefreshReason::Lifecycle
        );
        assert_eq!(
            WindowRefreshReason::Lifecycle.merge(WindowRefreshReason::Summon),
            WindowRefreshReason::Summon
        );
        assert_eq!(
            WindowRefreshReason::Summon.merge(WindowRefreshReason::Lifecycle),
            WindowRefreshReason::Summon
        );
    }

    #[test]
    fn focused_refresh_replaces_only_the_target_pid() {
        let existing = vec![window(10, 100), window(20, 200), window(10, 101)];
        let refreshed = vec![window(10, 102), window(10, 103)];

        let merged = merge_refreshed_windows(&existing, Some(10), refreshed);
        let keys: Vec<(i32, u32)> = merged
            .iter()
            .map(|window| (window.pid, window.window_id))
            .collect();

        assert_eq!(keys, vec![(20, 200), (10, 102), (10, 103)]);
    }

    #[test]
    fn full_refresh_is_ax_authoritative() {
        let existing = vec![window(10, 100), window(20, 200)];
        let refreshed = vec![window(20, 200)];

        let merged = merge_refreshed_windows(&existing, None, refreshed);
        let keys: Vec<(i32, u32)> = merged
            .iter()
            .map(|window| (window.pid, window.window_id))
            .collect();

        assert_eq!(keys, vec![(20, 200)]);
    }

    #[test]
    fn known_window_is_removed_when_its_cg_window_is_gone() {
        let existing = vec![window(10, 100), window(20, 200)];
        let refreshed = vec![window(20, 200)];

        let merged = merge_refreshed_windows(&existing, None, refreshed);
        let keys: Vec<(i32, u32)> = merged
            .iter()
            .map(|window| (window.pid, window.window_id))
            .collect();

        assert_eq!(keys, vec![(20, 200)]);
    }

    #[test]
    fn minimized_known_window_is_not_retained_when_hidden_windows_are_disabled() {
        let mut minimized = window(10, 100);
        minimized.minimized = true;
        let existing = vec![minimized];

        let merged = merge_refreshed_windows(&existing, None, Vec::new());

        assert!(merged.is_empty());
    }

    #[test]
    fn focused_refresh_drops_omitted_sibling() {
        let existing = vec![window(10, 100), window(10, 101), window(20, 200)];
        let refreshed = vec![window(10, 102)];

        let merged = merge_refreshed_windows(&existing, Some(10), refreshed);
        let keys: Vec<(i32, u32)> = merged
            .iter()
            .map(|window| (window.pid, window.window_id))
            .collect();

        assert_eq!(keys, vec![(20, 200), (10, 102)]);
    }

    #[test]
    fn refresh_selection_follows_the_exact_window_key_after_reordering() {
        let windows = vec![window(20, 200), window(10, 100), window(10, 101)];

        assert_eq!(
            selection_index_after_refresh(Some((10, 101)), 0, &windows),
            2
        );
    }

    #[test]
    fn refresh_selection_keeps_the_previous_slot_when_the_key_is_gone() {
        let windows = vec![window(20, 200), window(10, 100)];

        assert_eq!(
            selection_index_after_refresh(Some((10, 999)), 3, &windows),
            1
        );
    }
}
