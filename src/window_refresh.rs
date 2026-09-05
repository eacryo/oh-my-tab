//! 窗口快照刷新管线:请求合并(summon 优先)、后台收集线程、代数防串台、
//! 主线程应用(合并 MRU、选中锚定、首帧一次成图)以及 WindowServer 事件消费。
//! 从 main.rs 拆出;AppState/TAB_STATE/CONTROLLER 仍归 main.rs 所有,本模块只经
//! crate:: 路径访问。
//!
//! Window-snapshot refresh pipeline: request merging (summon wins), the background
//! collector thread, generation guarding, main-thread application (MRU merge,
//! selection anchoring, single-shot first frame), and WindowServer event consumption.
//! Split out of main.rs; AppState/TAB_STATE/CONTROLLER remain owned by main.rs and
//! are accessed here through crate:: paths.

use objc2::runtime::{AnyObject, Sel};
use objc2::{msg_send, sel};
use std::collections::{HashMap, HashSet};
use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicU8, Ordering};
use std::sync::{LazyLock, Mutex};
use std::thread;

use crate::ffi::frontmost_app_info;
use crate::overlay;
use crate::performance;
use crate::thumbnail;
use crate::window_collector::{
    bump_window_mru, collect_windows_for_pid, collect_windows_with_frontmost_bump,
    forget_non_normal_window, owner_pid_for_cgwid, sort_windows_by_mru, window_server_candidates,
    MruMap, WindowInfo,
};
use crate::window_server;
use crate::{log_debug, AppState, CONTROLLER, TAB_STATE};

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

struct WindowRefreshResult {
    generation: u64,
    windows: Vec<WindowInfo>,
    mru: MruMap,
    replace_pid: Option<i32>,
    active_key: Option<(i32, u32)>,
    // Exact frontmost window resolved during a first-summon refresh.  This must be carried
    // alongside the snapshot so the first frame does not reuse a stale same-PID focus_key.
    summon_focus_key: Option<(i32, u32)>,
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
pub(crate) fn request_window_refresh() {
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

/// Prefer the exact focus key from the current summon snapshot over a cached key.  Both keys
/// can belong to the same PID when an app opens or focuses another window (for example Edge's
/// normal and InPrivate windows), so comparing only the PID is insufficient.
fn select_summon_focus_key(
    fresh: Option<(i32, u32)>,
    cached: Option<(i32, u32)>,
    windows: &[WindowInfo],
) -> Option<(i32, u32)> {
    let is_present = |key: (i32, u32)| {
        windows
            .iter()
            .any(|window| (window.pid, window.window_id) == key)
    };
    fresh
        .filter(|key| is_present(*key))
        .or_else(|| cached.filter(|key| is_present(*key)))
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
            performance::set_current_thread_qos(match request {
                WindowRefreshRequest::Full(WindowRefreshReason::Summon) => {
                    performance::ThreadQos::UserInitiated
                }
                WindowRefreshRequest::Full(WindowRefreshReason::Lifecycle)
                | WindowRefreshRequest::Focused { .. } => performance::ThreadQos::Utility,
            });
            let mut mru = mru;
            let (windows, replace_pid, active_key, summon_focus_key) = match request {
                WindowRefreshRequest::Full(reason) => {
                    let bump_frontmost = reason.bumps_frontmost() && allow_bump;
                    let windows = collect_windows_with_frontmost_bump(&mut mru, bump_frontmost);
                    // collect_windows_with_frontmost_bump marks the exact frontmost window as
                    // the first active item after sorting. Preserve that key for first-summon
                    // selection; lifecycle refreshes intentionally do not provide this value.
                    let summon_focus_key = if bump_frontmost {
                        windows
                            .iter()
                            .find(|window| window.is_active)
                            .map(|window| (window.pid, window.window_id))
                    } else {
                        None
                    };
                    (windows, None, None, summon_focus_key)
                }
                WindowRefreshRequest::Focused { pid, window_id } => {
                    match collect_windows_for_pid(&mut mru, pid, window_id) {
                        Some(windows) => {
                            let active_key = windows
                                .iter()
                                .any(|window| window.window_id == window_id)
                                .then_some((pid, window_id));
                            (windows, Some(pid), active_key, None)
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
                                collect_windows_with_frontmost_bump(&mut mru, false),
                                None,
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
                summon_focus_key,
            });
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

/// Keep the currently visible card order while applying refreshed window data.
///
/// MRU sorting is useful when the overlay is summoned, but replacing the order of
/// `TAB_STATE.windows` while the existing card tree remains in place breaks the
/// card-index invariant. Rebase the refreshed records onto the current order so
/// pure refreshes update metadata without moving visible cards.
fn preserve_existing_window_order(
    existing: &[WindowInfo],
    refreshed: Vec<WindowInfo>,
) -> Vec<WindowInfo> {
    let refresh_order: Vec<(i32, u32)> = refreshed
        .iter()
        .map(|window| (window.pid, window.window_id))
        .collect();
    let mut by_key: HashMap<(i32, u32), WindowInfo> = refreshed
        .into_iter()
        .map(|window| ((window.pid, window.window_id), window))
        .collect();
    let mut ordered = Vec::with_capacity(existing.len() + by_key.len());
    for current in existing {
        let key = (current.pid, current.window_id);
        if let Some(window) = by_key.remove(&key) {
            ordered.push(window);
        }
    }
    // New windows are not part of a pure reorder, but appending them makes this
    // helper safe for callers that use it across a concurrent add/remove. Keep
    // the refreshed order deterministic instead of iterating the hash map.
    for key in refresh_order {
        if let Some(window) = by_key.remove(&key) {
            ordered.push(window);
        }
    }
    ordered
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
    if overlay::card_close_in_progress() {
        // 关闭卡片正在收窄补位时保留快照,避免刷新重建 view 树打断过渡动画。
        // Keep the snapshot pending while close reflow runs, so a refresh cannot rebuild the view tree.
        unsafe {
            if let Some(controller) = *CONTROLLER.lock().unwrap() {
                let _: () = msg_send![
                    controller.0,
                    performSelector: sel!(handleWindowRefresh:),
                    withObject: std::ptr::null::<AnyObject>(),
                    afterDelay: 0.08f64
                ];
            }
        }
        return;
    }
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

    let summon_focus_key = result.summon_focus_key;
    let subscriptions = window_server_candidates();

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
        let mut sorted_windows =
            merge_refreshed_windows(&state.windows, replace_pid, result.windows);
        sort_windows_by_mru(&mut sorted_windows, &mru, std::time::Instant::now());
        if replace_pid.is_none() {
            // 全量刷新合并旧窗口后重新建立唯一的前台代表，避免旧快照的 is_active 残留。
            // Re-establish one frontmost representative after a full merge so an old snapshot
            // cannot leave multiple stale is_active flags behind.
            for window in &mut sorted_windows {
                window.is_active = false;
            }
            if let Some(first) = sorted_windows.first_mut() {
                first.is_active = true;
            }
        } else if let Some(active_key) = active_key {
            for window in &mut sorted_windows {
                window.is_active = (window.pid, window.window_id) == active_key;
            }
        }
        let set_changed = replace_pid.is_some() || {
            let old: HashSet<(i32, u32)> =
                state.windows.iter().map(|w| (w.pid, w.window_id)).collect();
            let new: HashSet<(i32, u32)> = sorted_windows
                .iter()
                .map(|w| (w.pid, w.window_id))
                .collect();
            old != new
        };
        let was_visible = state.visible;
        let windows = if was_visible && !set_changed {
            preserve_existing_window_order(&state.windows, sorted_windows)
        } else {
            sorted_windows
        };
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
        // 集合级变化(窗口增删):只有这个才需要整树重建浮窗。仅排序变化(MRU 被前台 bump 改写、
        // 表面切换)只更新数据,不重建列表——否则用户看到「打开了还在跳」。定向刷新(replace_pid)
        // 替换了目标 PID 的卡片,也视为集合变化。
        // Set-level change (windows added/removed): only this needs a full overlay rebuild. A pure
        // reorder (MRU bumped by the frontmost, surface flip) only updates data without rebuilding
        // the list — otherwise the overlay "keeps jumping after it opened". A directed refresh
        // (replace_pid) swapping a PID's cards also counts as a set change.
        state.windows = windows;
        state.selected = selected;
        state.mru = mru;
        if state.windows.is_empty() {
            state.visible = false;
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
                let backward = state.pending_first_backward;
                let release_pending = state.pending_first_release;
                state.pending_first_release = false;
                Some((backward, release_pending))
            } else {
                None
            }
        } else {
            None
        }
    };
    if let Some((backward, release_pending)) = first_show_request {
        // The snapshot already resolved the current window exactly. Update the persistent focus
        // key before prepare_first_summon_state runs; otherwise a stale sibling from the same
        // app (for example Edge's previous normal window) wins over the new focused window.
        if let Some(state) = TAB_STATE.lock().unwrap().as_mut() {
            if let Some(focus_key) =
                select_summon_focus_key(summon_focus_key, state.focus_key, &state.windows)
            {
                state.focus_key = Some(focus_key);
            }
        }
        if release_pending {
            // Cmd was released while the first snapshot was pending. Commit the selected target
            // without ever displaying the panel; on_cmd_released cannot do this itself because
            // the panel is intentionally still invisible until the snapshot is ready.
            overlay::commit_first_summon(backward);
            log_debug!(
                "[overlay] summon e2e: first snapshot committed after pending release (backward={})",
                backward
            );
        } else {
            overlay::show_first_summon(backward);
            log_debug!(
                "[overlay] summon e2e: first snapshot shown (backward={})",
                backward
            );
        }
    }

    if set_changed && was_visible {
        overlay::reset_thumbnail_visible_range();
        overlay::reset_thumbnail_scroll();
        overlay::reset_thumbnail_nav_anchor();
        overlay::show_overlay();
        overlay::refresh_highlight();
    }
    // WindowServer 监听所有 CG 候选窗口,而不是只监听 AX 确认并显示的卡片。
    // WindowServer observes every CG candidate instead of only AX-confirmed display cards.
    window_server::update_subscriptions(&subscriptions);
    if let Some(request) = pending_request {
        request_window_refresh_request(request);
    }
}

pub(crate) extern "C" fn on_window_refresh(_self: *mut c_void, _cmd: Sel, _arg: *mut c_void) {
    apply_window_refresh();
}

pub(crate) extern "C" fn on_window_server_event(_self: *mut c_void, _cmd: Sel, _arg: *mut c_void) {
    let events = window_server::drain_main();
    if events.is_empty() {
        return;
    }
    let should_refresh = events.iter().any(|event| match event {
        window_server::WindowServerEvent::Created => true,
        window_server::WindowServerEvent::Destroyed(window_id) => {
            forget_non_normal_window(*window_id);
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
                .or_else(|| owner_pid_for_cgwid(*window_id));
            if let Some(pid) = pid {
                let frontmost_pid = frontmost_app_info().1;
                log_debug!(
                    "[windows] focused event: cgwid={} pid={} displayed={} frontmost={}",
                    window_id,
                    pid,
                    displayed_pid.is_some(),
                    frontmost_pid == pid
                );
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
pub(crate) fn best_effort_bump_focus_key(state: &mut AppState, pid: i32, cgwid: u32) {
    bump_window_mru(&mut state.mru, pid, cgwid);
    let shown = state
        .windows
        .iter()
        .any(|window| window.pid == pid && window.window_id == cgwid);
    if shown {
        state.focus_key = Some((pid, cgwid));
    }
}

#[cfg(test)]
mod tests {
    use super::{
        merge_refreshed_windows, preserve_existing_window_order, selection_index_after_refresh,
        WindowRefreshReason,
    };
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
    fn visible_refresh_preserves_existing_order_but_uses_refreshed_records() {
        let existing = vec![window(1, 10), window(2, 20), window(3, 30), window(4, 40)];
        let mut refreshed = vec![window(4, 40), window(1, 10), window(2, 20), window(3, 30)];
        refreshed[0].window_title = "updated Edge".into();

        let ordered = preserve_existing_window_order(&existing, refreshed);
        let keys: Vec<(i32, u32)> = ordered
            .iter()
            .map(|window| (window.pid, window.window_id))
            .collect();

        assert_eq!(keys, vec![(1, 10), (2, 20), (3, 30), (4, 40)]);
        assert_eq!(ordered[3].window_title, "updated Edge");
    }

    #[test]
    fn visible_order_helper_drops_removed_windows_and_appends_new_windows() {
        let existing = vec![window(1, 10), window(2, 20), window(3, 30)];
        let refreshed = vec![window(4, 40), window(3, 30), window(1, 10)];

        let ordered = preserve_existing_window_order(&existing, refreshed);
        let keys: Vec<(i32, u32)> = ordered
            .iter()
            .map(|window| (window.pid, window.window_id))
            .collect();

        assert_eq!(keys, vec![(1, 10), (3, 30), (4, 40)]);
    }

    #[test]
    fn fresh_summon_focus_key_overrides_stale_same_pid_key() {
        let windows = vec![window(23199, 2837), window(23199, 13512)];

        let selected =
            super::select_summon_focus_key(Some((23199, 13512)), Some((23199, 2837)), &windows);

        assert_eq!(selected, Some((23199, 13512)));
    }

    #[test]
    fn summon_focus_key_falls_back_to_cached_present_key() {
        let windows = vec![window(23199, 2837), window(23199, 13512)];

        let selected = super::select_summon_focus_key(None, Some((23199, 2837)), &windows);

        assert_eq!(selected, Some((23199, 2837)));
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
