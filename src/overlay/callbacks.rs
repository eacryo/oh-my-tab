//! 浮窗 · callbacks:Cmd+Tab 键盘/释放回调与首帧召唤状态机。
//! Cmd+Tab keyboard/release callbacks and the first-summon state machine.

use super::*;

// ========== ObjC 回调实现 / ObjC callback implementations ==========

/// 首帧一次性显示:用当前(已刷新的)窗口列表做首次选中并弹出浮窗。
/// 由 apply_window_refresh 在消费 pending_first_show 时调用,保证「一次成图」——
/// 显示的就是刷新后的最终排序,不存在「先显示旧快照、再重排」的两段跳变。
/// First-frame single-shot show: pick the initial selection over the (refreshed) window list and
/// pop the overlay. Called by apply_window_refresh when it consumes pending_first_show so the
/// render is single-shot — the shown order is already the final one, no "stale then reorder" jump.
pub(crate) fn show_first_summon(backward: bool) {
    prepare_first_summon_state(backward);
    let t_show = Instant::now();
    show_overlay();
    // TIMING-DEBUG 端到端:tap 回调 → 收集完成 → show_overlay。
    log_debug!("[overlay] summon e2e={}ms", t_show.elapsed().as_millis());
}

/// Prepare the first summon selection without deciding whether the panel should be displayed.
///
/// 首帧选中状态的准备与显示分开,这样在等待快照时收到 CmdReleased 可以直接提交目标,
/// 而不必先短暂显示再隐藏浮窗。
fn prepare_first_summon_state(backward: bool) {
    cancel_scheduled_order_out();
    with_tab_state(|state_opt| {
        let state = state_opt.as_mut().unwrap();
        state.visible = true;
        let (_, frontmost_pid) = frontmost_app_info();
        let frontmost_pid = (frontmost_pid > 0).then_some(frontmost_pid);
        let focus_key = state
            .focus_key
            .filter(|(pid, _)| frontmost_pid == Some(*pid));
        state.selected = prepare_first_summon(
            &mut state.windows,
            &mut state.mru,
            backward,
            frontmost_pid,
            focus_key,
            Instant::now(),
        );
        // 记录召唤瞬间的窗口 key 集合:浮窗打开后的刷新只知道哪些窗口「召唤时就在场」。
        state.summon_keys = Some(state.windows.iter().map(|w| (w.pid, w.window_id)).collect());
        // 首帧默认选中:锁定到「召唤时选中的目标窗口」,刷新不因 MRU 排序变化改选。
        state.user_picked = false;
        state.selected_target_key = state
            .windows
            .get(state.selected)
            .map(|w| (w.pid, w.window_id));
        log_debug!(
            "[overlay] first summon: frontmost_pid={:?} focus_key={:?} selected={} windows={}",
            frontmost_pid,
            focus_key,
            state.selected,
            state.windows.len()
        );
    });
    reset_thumbnail_visible_range();
    reset_thumbnail_scroll();
    reset_thumbnail_nav_anchor();
    MOUSE_MOVED.store(false, Ordering::Relaxed);
    *HOVER_TICK_POS.lock().unwrap() = None;
}

/// Commit a first summon whose Cmd release arrived before the first frame was ready.
/// The target selection is prepared, but the overlay is never ordered on screen.
pub(crate) fn commit_first_summon(backward: bool) {
    prepare_first_summon_state(backward);
    commit_selected_window(false);
}

fn step_switcher(backward: bool) {
    let (pending, first_show) = with_tab_state(|state_opt| {
        let state = state_opt.as_ref().unwrap();
        (
            state.pending_first_show,
            !state.visible && !state.pending_first_show,
        )
    });

    if pending {
        // 首帧快照仍在后台收集:本次浮窗尚未显示,重复 Tab 无法基于旧快照定位,先忽略,
        // 等 apply_window_refresh 一次性显示后再由用户续按。
        // The first snapshot is still being collected: the overlay isn't shown yet, so another Tab
        // can't be positioned over the stale list — ignore it; let the user continue once the
        // single-shot show lands.
        log_debug!("[overlay] re-Tab during pending first show ignored");
        return;
    }

    if first_show {
        // 首帧:不再先显示旧快照,而是发起后台刷新并标记「待显示」,等 apply_window_refresh
        // 拿到首帧快照后一次性显示(一次成图)。注意:发起刷新必须释放 TAB_STATE 锁,否则
        // request_window_refresh 内部同样要锁 TAB_STATE,造成自死锁(主线程发生阻塞)。
        // First frame: don't show the stale startup snapshot first. Kick off a background refresh
        // and mark pending_first_show; apply_window_refresh consumes it and shows once the first
        // snapshot is ready (single-shot render). NB: the refresh must be kicked off AFTER dropping
        // TAB_STATE, otherwise request_window_refresh re-locks it and deadlocks the main thread.
        crate::performance::begin_switcher_activity();
        request_window_refresh();
        with_tab_state(|state_opt| {
            let state = state_opt.as_mut().unwrap();
            state.visible = false;
            state.pending_first_show = true;
            state.pending_first_backward = backward;
            state.pending_first_release = false;
        });
        schedule_first_summon_timeout();
        // TIMING-DEBUG 端到端:tap 回调 → 收集完成 → show_first_summon。
        log_debug!("[overlay] first summon pending (awaiting snapshot)");
    } else {
        // 用户主动导航(重复按 Tab):选中不再是首帧默认落点,标记 user_picked 并钉住当前目标。
        // User-initiated navigation (repeated Tab): the pick is no longer the first-frame default;
        // mark user_picked and pin to the current target.
        with_tab_state(|state_opt| {
            let state = state_opt.as_mut().unwrap();
            state.selected = horizontal_nav_index(state.selected, state.windows.len(), backward);
            mark_user_picked(state);
        });
        reset_thumbnail_nav_anchor();
        refresh_after_selection_change(true);
    }
}

const FIRST_SUMMON_FALLBACK_DELAY: f64 = 0.12;

/// Give a slow AX refresh a short deadline so the switcher can still respond to a held/released
/// Cmd using the last coherent snapshot. A later refresh result reconciles the visible list.
fn schedule_first_summon_timeout() {
    unsafe {
        let Some(controller) = *crate::CONTROLLER.lock().unwrap() else {
            return;
        };
        let _: () = msg_send![
            controller.0,
            performSelector: sel!(handleFirstSummonTimeout:),
            withObject: std::ptr::null::<AnyObject>(),
            afterDelay: FIRST_SUMMON_FALLBACK_DELAY
        ];
    }
}

/// Main-thread deadline for a first summon. The callback is harmless when the real snapshot has
/// already arrived because apply_window_refresh clears pending_first_show first.
pub(crate) extern "C" fn on_first_summon_timeout(_self: *mut c_void, _cmd: Sel, _arg: *mut c_void) {
    let request = with_tab_state(|state_opt| {
        let state = state_opt.as_mut()?;
        if !state.pending_first_show {
            return None;
        }
        state.pending_first_show = false;
        let backward = state.pending_first_backward;
        let release_pending = state.pending_first_release;
        state.pending_first_release = false;
        Some((backward, release_pending))
    });

    if let Some((backward, release_pending)) = request {
        log_debug!(
            "[overlay] first summon deadline reached (release_pending={})",
            release_pending
        );
        if release_pending {
            commit_first_summon(backward);
        } else {
            show_first_summon(backward);
        }
    }
}

/// 用户主动改变了选中(导航/点击/悬停):标记 user_picked 并钉住当前选中窗口 key。
/// 此后刷新将按该目标窗口恢复选中,再也不随列表重排漂移。调用方须已持有 TAB_STATE。
/// User actively changed the selection (nav/click/hover): mark user_picked and pin to the newly
/// selected window key. Subsequent refreshes restore the pick to that target instead of drifting
/// with a reorder. Caller must already hold TAB_STATE.
pub(super) fn mark_user_picked(state: &mut AppState) {
    state.user_picked = true;
    state.selected_target_key = state
        .windows
        .get(state.selected)
        .map(|w| (w.pid, w.window_id));
}

pub(crate) extern "C" fn on_cmd_tab_pressed(_self: *mut c_void, _cmd: Sel, _arg: *mut c_void) {
    step_switcher(false);
}

pub(crate) extern "C" fn on_cmd_shift_tab_pressed(
    _self: *mut c_void,
    _cmd: Sel,
    _arg: *mut c_void,
) {
    step_switcher(true);
}

/// 选中项越过当前视口时只移动 clip bounds,不重建卡片树;两种布局共用。
/// Move clip bounds when selection leaves the viewport; both layouts share this path and never
/// rebuild the card tree.
fn refresh_after_selection_change(backfill_icons: bool) {
    let selected = with_tab_state(|state| state.as_ref().map(|state| state.selected));
    let needs_relayout = selected.is_some_and(|index| {
        !THUMB_VISIBLE_RANGE
            .lock()
            .unwrap()
            .as_ref()
            .is_some_and(|range| range.contains(&index))
    });
    if needs_relayout {
        if let Some(index) = selected {
            if ensure_thumbnail_selection_visible(index) {
                apply_thumbnail_scroll_offset();
            }
        }
    }
    refresh_highlight();
    update_status_label();
    if backfill_icons {
        extract_uncached_icons();
    }
}

/// 从浮窗容器收集每张卡片的 (index, x, y, width)(按实际 frame,跳过状态栏标签)。
/// Collect (index, x, y, width) for every card from the live container subviews
/// (actual frames; the status-bar labels are skipped).
unsafe fn collect_card_rects() -> Vec<(usize, f64, f64, f64)> {
    let document = match card_document() {
        Some(document) => document,
        None => return Vec::new(),
    };
    let mut out: Vec<(usize, f64, f64, f64)> = Vec::new();
    for sv in card_views(document) {
        let Some(idx) = get_card_index(sv) else {
            continue;
        };
        let f: NSRect = msg_send![sv, frame];
        out.push((idx, f.origin.x, f.origin.y, f.size.width));
    }
    out
}

/// 几何感知的垂直导航(纯函数,可单测):跳到相邻行中水平中心最接近固定锚点的
/// 那一张。锚点在连续上下移动期间不变，因此下再上可以回到原列附近。
/// 返回 None 表示该方向没有相邻行(保持"到边不动"的语义)。
/// 行聚类按 y 值 + 1.0pt 容差(同一行的卡片 y 完全相同,容差只防浮点漂移)。
///
/// Geometry-aware vertical navigation (pure, unit-testable): jump to the card in
/// the adjacent row whose horizontal center is closest to a stable anchor.
/// Flow rows hold different card counts, so a fixed step misaligns or runs off
/// the end. None = no adjacent row in that direction (edge = no-op semantics).
/// Rows cluster by y with a 1pt epsilon (same-row cards share y exactly; the
/// epsilon only guards float drift).
pub(super) fn vertical_nav_index(
    rects: &[(usize, f64, f64, f64)],
    current: usize,
    up: bool,
    anchor_x: f64,
) -> Option<usize> {
    const ROW_EPS: f64 = 1.0;
    let (_, _, cy, _) = rects.iter().find(|(i, ..)| *i == current)?;
    let cur_y = cy;

    // 相邻行:同方向里 y 最接近当前行的那个。
    // The adjacent row: nearest y in the requested direction.
    let mut best_row_y: Option<f64> = None;
    for (_, _, y, _) in rects {
        let dy = y - cur_y;
        let in_direction = if up { dy > ROW_EPS } else { dy < -ROW_EPS };
        if !in_direction {
            continue;
        }
        best_row_y = Some(match best_row_y {
            Some(by) if up => by.min(*y),
            Some(by) => by.max(*y),
            None => *y,
        });
    }
    let target_y = best_row_y?;

    // 目标行内取水平中心最近者(平分取先出现者)。
    // Within the target row, pick the closest horizontal center (ties -> first).
    rects
        .iter()
        .filter(|(_, _, y, _)| (y - target_y).abs() <= ROW_EPS)
        .min_by(|a, b| {
            let da = ((a.1 + a.3 / 2.0) - anchor_x).abs();
            let db = ((b.1 + b.3 / 2.0) - anchor_x).abs();
            da.partial_cmp(&db).unwrap_or(std::cmp::Ordering::Equal)
        })
        .map(|(i, ..)| *i)
}

fn card_center_x(rects: &[(usize, f64, f64, f64)], index: usize) -> Option<f64> {
    rects
        .iter()
        .find(|(i, ..)| *i == index)
        .map(|(_, x, _, width)| x + width / 2.0)
}

#[cfg(test)]
pub(super) fn edge_row_nav_index(
    rects: &[(usize, f64, f64, f64)],
    top: bool,
    anchor_x: f64,
) -> Option<usize> {
    const ROW_EPS: f64 = 1.0;
    let target_y =
        rects
            .iter()
            .map(|(_, _, y, _)| *y)
            .reduce(|a, b| if top { a.max(b) } else { a.min(b) })?;
    rects
        .iter()
        .filter(|(_, _, y, _)| (y - target_y).abs() <= ROW_EPS)
        .min_by(|a, b| {
            let da = ((a.1 + a.3 / 2.0) - anchor_x).abs();
            let db = ((b.1 + b.3 / 2.0) - anchor_x).abs();
            da.partial_cmp(&db).unwrap_or(std::cmp::Ordering::Equal)
        })
        .map(|(index, ..)| *index)
}

/// 两种布局的上下导航:完整 document 中按固定水平锚点移动,越过视口时只移动 clip bounds。
/// Vertical navigation for both layouts uses the complete document and a stable horizontal
/// anchor; crossing the viewport only moves clip bounds.
unsafe fn navigate_thumbnail_vertical(rects: &[(usize, f64, f64, f64)], up: bool) {
    let selected = with_tab_state(|state_opt| {
        let state = state_opt.as_mut()?;
        if !state.visible || state.windows.is_empty() {
            return None;
        }
        let current_center = card_center_x(rects, state.selected)?;
        let anchor_x = {
            let mut anchor = THUMB_NAV_ANCHOR_X.lock().unwrap();
            *anchor.get_or_insert(current_center)
        };
        let index = vertical_nav_index(rects, state.selected, up, anchor_x)?;
        state.selected = index;
        mark_user_picked(state);
        Some(index)
    });
    if let Some(index) = selected {
        if ensure_thumbnail_selection_visible(index) {
            apply_thumbnail_scroll_offset();
        }
        refresh_highlight();
        update_status_label();
    }
}

// layer_set_shadow_color 的本地副本已删除:与 ffi::layer_set_shadow_color 逐行等价,
// 拆分后 cancel.rs 的调用经 use super::* → ffi 的 pub(crate) 版本解析。
// The local layer_set_shadow_color copy is removed: it is line-for-line identical to
// ffi::layer_set_shadow_color, which cancel.rs now resolves to via use super::* -> ffi.

/// 用 CALayer 的 KVC 子键设置二维平移，避免把 CATransform3D 结构体传进 objc2
/// `msg_send!` 的运行时编码校验。父层变换会携带背景、描边、阴影与全部子视图，
/// 同时不改 NSView frame，因此导航几何和原位卡片重建仍使用稳定基准。
/// Set 2D translation through CALayer's KVC sub-key, avoiding CATransform3D in objc2's
/// runtime-checked `msg_send!`. Transforming the parent carries its background, border,
/// shadow, and all subviews without changing the NSView frame, so navigation geometry and
/// in-place card rebuilding retain a stable baseline.
pub(super) unsafe fn layer_set_translation_y(layer: *mut AnyObject, y: f64) {
    let value: *mut AnyObject = msg_send![class!(NSNumber), numberWithDouble: y];
    let key = make_nsstring("transform.translation.y");
    let _: () = msg_send![layer, setValue: value, forKeyPath: key];
    CFRelease(key as *const c_void);
}

pub(crate) extern "C" fn container_key_down(_self: *mut c_void, _cmd: Sel, event: *mut c_void) {
    unsafe {
        let key_code: u16 = msg_send![event as *mut AnyObject, keyCode];
        let modifier_flags: u64 = msg_send![event as *mut AnyObject, modifierFlags];
        let shift_pressed = modifier_flags & NSEVENT_MODIFIER_FLAG_SHIFT != 0;
        // Collect navigation frames before borrowing runtime; reentrant AppKit calls happen
        // only after the state borrow has been released.
        // 几何导航的 frame 收集先于借用 runtime；可能同步重入 AppKit 的调用均在释放借用后执行。
        let nav_rects = collect_card_rects();
        enum KeyAction {
            None,
            RefreshSelection,
            Vertical(bool),
            Close(usize),
            Activate {
                pid: i32,
                cgwid: u32,
                minimized: bool,
            },
            Hide,
        }
        let action = with_tab_state(|state_opt| {
            let state = state_opt.as_mut().unwrap();
            if !state.visible {
                return KeyAction::None;
            }
            match key_code {
                KEY_TAB | KEY_RIGHT | KEY_LEFT if !state.windows.is_empty() => {
                    let backward = key_code == KEY_LEFT || (key_code == KEY_TAB && shift_pressed);
                    state.selected =
                        horizontal_nav_index(state.selected, state.windows.len(), backward);
                    mark_user_picked(state);
                    KeyAction::RefreshSelection
                }
                KEY_UP if !state.windows.is_empty() => KeyAction::Vertical(true),
                KEY_DOWN if !state.windows.is_empty() => KeyAction::Vertical(false),
                KEY_DELETE if !state.windows.is_empty() => KeyAction::Close(state.selected),
                KEY_RETURN => {
                    if let Some(w) = state.windows.get(state.selected) {
                        let action = KeyAction::Activate {
                            pid: w.pid,
                            cgwid: w.window_id,
                            minimized: w.minimized,
                        };
                        state.focus_key = Some((w.pid, w.window_id));
                        bump_window_mru(&mut state.mru, w.pid, w.window_id);
                        state.visible = false;
                        action
                    } else {
                        state.visible = false;
                        KeyAction::Hide
                    }
                }
                KEY_ESCAPE => {
                    state.visible = false;
                    KeyAction::Hide
                }
                _ => KeyAction::None,
            }
        });
        match action {
            KeyAction::RefreshSelection => {
                reset_thumbnail_nav_anchor();
                refresh_after_selection_change(false);
            }
            KeyAction::Vertical(up) => navigate_thumbnail_vertical(&nav_rects, up),
            KeyAction::Close(idx) => {
                // Backspace:关闭选中卡片对应的窗口,浮窗保持打开。
                // Backspace: close the selected card's window; the overlay stays open.
                let card = card_document().and_then(|document| {
                    card_views(document)
                        .into_iter()
                        .find(|card| get_card_index(*card) == Some(idx))
                });
                if let Some(card) = card {
                    begin_close_window_at(idx, card);
                } else {
                    close_window_at(idx);
                }
            }
            KeyAction::Activate {
                pid,
                cgwid,
                minimized,
            } => {
                vanish_overlay();
                // 同 on_cmd_released:设置窗口无需特殊处理(见该处注释);抬升延迟一拍执行。
                // Same as on_cmd_released: no settings-window handling needed (see comment
                // there); the raise is deferred by one runloop turn so the vanish commits first.
                schedule_deferred_raise(pid, cgwid, minimized);
                schedule_delayed_order_out();
            }
            KeyAction::Hide => hide_overlay(),
            KeyAction::None => {}
        }
    }
}

pub(crate) extern "C" fn container_accepts_first_responder(_self: *mut c_void, _cmd: Sel) -> bool {
    crate::callback_guard::bool("container_accepts_first_responder", false, || true)
}

/// 两种布局都接收鼠标滚轮和触控板滚动,保留 point 级增量而不是量化为整行。
/// Both layouts handle mouse-wheel and trackpad scrolling, preserving point-level deltas instead
/// of quantizing them to whole rows.
pub(crate) extern "C" fn container_scroll_wheel(_self: *mut c_void, _cmd: Sel, event: *mut c_void) {
    unsafe {
        let delta_y: f64 = msg_send![event as *mut AnyObject, scrollingDeltaY];
        if delta_y.abs() < f64::EPSILON {
            return;
        }
        let precise: bool = msg_send![event as *mut AnyObject, hasPreciseScrollingDeltas];
        if precise {
            scroll_thumbnail_by_offset(-delta_y);
        } else {
            // 离散鼠标滚轮仍按一个小的 point 步长前进,而不是直接跳到下一行。
            // Discrete mouse wheels still advance by a small point step instead of jumping to the
            // next row immediately.
            const DISCRETE_SCROLL_STEP: f64 = 40.0;
            scroll_thumbnail_by_offset(-delta_y.signum() * DISCRETE_SCROLL_STEP);
        }
    }
}

pub(crate) extern "C" fn container_mouse_entered(
    _self: *mut c_void,
    _cmd: Sel,
    event: *mut c_void,
) {
    unsafe {
        let point: NSPoint = msg_send![event as *mut AnyObject, locationInWindow];
        update_thumbnail_pointer_state(point);
    }
}

pub(crate) extern "C" fn container_mouse_exited(_self: *mut c_void, _cmd: Sel, event: *mut c_void) {
    unsafe {
        let point: NSPoint = msg_send![event as *mut AnyObject, locationInWindow];
        update_thumbnail_pointer_state(point);
    }
}

pub(crate) extern "C" fn thumbnail_scroller_mouse_entered(
    _self: *mut c_void,
    _cmd: Sel,
    event: *mut c_void,
) {
    unsafe {
        let point: NSPoint = msg_send![event as *mut AnyObject, locationInWindow];
        update_thumbnail_pointer_state(point);
    }
}

pub(crate) extern "C" fn thumbnail_scroller_mouse_moved(
    _self: *mut c_void,
    _cmd: Sel,
    event: *mut c_void,
) {
    unsafe {
        let point: NSPoint = msg_send![event as *mut AnyObject, locationInWindow];
        update_thumbnail_pointer_state(point);
    }
}

pub(crate) extern "C" fn thumbnail_scroller_mouse_exited(
    _self: *mut c_void,
    _cmd: Sel,
    event: *mut c_void,
) {
    unsafe {
        let point: NSPoint = msg_send![event as *mut AnyObject, locationInWindow];
        update_thumbnail_pointer_state(point);
    }
}

/// HTML 参考稿的可见滑块宽度;命中区域仍由外层 14pt 视图提供。
/// Visible thumb width from the HTML reference; the outer 14pt view remains the hit area.
const THUMB_SCROLLBAR_VISIBLE_W: f64 = 5.0;
/// HTML 参考稿的上下留白在原生浮窗中放大到 6pt,避免胶囊视觉上贴住边缘。
/// Increase the HTML reference's edge inset to 6pt in the native panel so the capsule never looks flush with the viewport.
const THUMB_SCROLLBAR_EDGE: f64 = 22.0;
const THUMB_SCROLLBAR_MIN_KNOB_H: f64 = 24.0;

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct ThumbnailScrollerGeometry {
    pub(crate) knob_y: f64,
    pub(crate) knob_h: f64,
    pub(crate) thumb_travel: f64,
}

/// 用完整滚动范围计算胶囊位置;绘制和拖拽必须共享这套几何。
/// Compute the capsule from the complete scroll range; drawing and dragging must share it.
pub(crate) fn thumbnail_scroller_geometry(
    track_h: f64,
    max_offset: f64,
    offset: f64,
) -> Option<ThumbnailScrollerGeometry> {
    if !track_h.is_finite() || !max_offset.is_finite() || max_offset <= f64::EPSILON {
        return None;
    }
    let track_h = track_h - THUMB_SCROLLBAR_EDGE * 2.0;
    if track_h <= 0.0 {
        return None;
    }
    let knob_h =
        (track_h * track_h / (track_h + max_offset)).clamp(THUMB_SCROLLBAR_MIN_KNOB_H, track_h);
    let thumb_travel = (track_h - knob_h).max(0.0);
    let progress = (offset / max_offset).clamp(0.0, 1.0);
    Some(ThumbnailScrollerGeometry {
        // AppKit coordinates grow upward: offset 0 is the visual top of the content.
        // AppKit 坐标向上增长:offset 0 对应内容视觉上的顶部。
        knob_y: THUMB_SCROLLBAR_EDGE + (1.0 - progress) * thumb_travel,
        knob_h,
        thumb_travel,
    })
}

pub(super) unsafe fn update_thumbnail_pointer_state(window_point: NSPoint) {
    let Some(container) = (*CONTAINER.lock().unwrap()).map(|container| container.0) else {
        set_thumbnail_scroller_hover(false, false);
        return;
    };
    let container_point: NSPoint = msg_send![
        container,
        convertPoint: window_point,
        fromView: std::ptr::null::<AnyObject>()
    ];
    let container_bounds: NSRect = msg_send![container, bounds];
    let in_viewport = container_point.x >= container_bounds.origin.x
        && container_point.x <= container_bounds.origin.x + container_bounds.size.width
        && container_point.y >= container_bounds.origin.y
        && container_point.y <= container_bounds.origin.y + container_bounds.size.height;

    let Some(scroller) = thumbnail_scroller() else {
        set_thumbnail_scroller_hover(in_viewport, false);
        return;
    };
    let scroller_point: NSPoint = msg_send![
        scroller.0,
        convertPoint: window_point,
        fromView: std::ptr::null::<AnyObject>()
    ];
    let scroller_bounds: NSRect = msg_send![scroller.0, bounds];
    let in_scroller = scroller_point.x >= scroller_bounds.origin.x
        && scroller_point.x <= scroller_bounds.origin.x + scroller_bounds.size.width
        && scroller_point.y >= scroller_bounds.origin.y
        && scroller_point.y <= scroller_bounds.origin.y + scroller_bounds.size.height;
    let max_offset = *THUMB_SCROLL_MAX_OFFSET.lock().unwrap();
    let offset = *THUMB_SCROLL_OFFSET.lock().unwrap();
    let knob = thumbnail_scroller_geometry(scroller_bounds.size.height, max_offset, offset)
        .is_some_and(|geometry| {
            in_scroller && thumbnail_scroller_knob_contains(scroller_point.y, geometry)
        });
    set_thumbnail_scroller_hover(in_viewport || in_scroller, knob);
}

pub(super) fn thumbnail_scroll_offset_for_drag(
    start_offset: f64,
    start_y: f64,
    current_y: f64,
    max_offset: f64,
    thumb_travel: f64,
) -> f64 {
    if max_offset <= 0.0 || thumb_travel <= 0.0 {
        return start_offset.clamp(0.0, max_offset.max(0.0));
    }
    (start_offset + (start_y - current_y) * max_offset / thumb_travel).clamp(0.0, max_offset)
}

/// 只绘制滚动条胶囊;透明的整个指示器视图负责命中和显式拖拽。
/// Draw only the scrollbar capsule; the transparent indicator view owns hit testing and explicit dragging.
pub(crate) extern "C" fn thumbnail_scroller_draw_rect(
    scroller: *mut c_void,
    _cmd: Sel,
    _dirty_rect: NSRect,
) {
    unsafe {
        let scroller = scroller as *mut AnyObject;
        let bounds: NSRect = msg_send![scroller, bounds];
        let offset = *THUMB_SCROLL_OFFSET.lock().unwrap();
        let max_offset = *THUMB_SCROLL_MAX_OFFSET.lock().unwrap();
        let Some(geometry) = thumbnail_scroller_geometry(bounds.size.height, max_offset, offset)
        else {
            return;
        };
        let inset_x = ((bounds.size.width - THUMB_SCROLLBAR_VISIBLE_W) / 2.0).max(0.0);
        let capsule = NSRect::new(
            NSPoint::new(inset_x, geometry.knob_y),
            NSSize::new(
                THUMB_SCROLLBAR_VISIBLE_W.min(bounds.size.width),
                geometry.knob_h,
            ),
        );
        let dark = match CONFIG.read().unwrap().appearance.theme.as_str() {
            "light" => false,
            "dark" => true,
            _ => system_dark_mode(),
        };
        let hover = *THUMB_SCROLLER_HOVER.lock().unwrap();
        let dragging = THUMB_SCROLL_DRAG.lock().unwrap().is_some();
        let alpha = thumbnail_scroller_alpha(hover.viewport, hover.knob, dragging);
        let color: *mut AnyObject = if dark {
            msg_send![class!(NSColor), colorWithWhite: 1.0f64, alpha: alpha]
        } else {
            let color = 0x464E5C00 | u32::from((alpha * 255.0).round() as u8);
            hex_to_ns_color(color)
        };
        let _: () = msg_send![color, set];
        let path: *mut AnyObject = msg_send![
            class!(NSBezierPath),
            bezierPathWithRoundedRect: capsule,
            xRadius: THUMB_SCROLLBAR_VISIBLE_W / 2.0,
            yRadius: THUMB_SCROLLBAR_VISIBLE_W / 2.0
        ];
        let _: () = msg_send![path, fill];
    }
}

/// 非激活浮窗第一次点击也必须交给滚动条,否则按住 Command 时首个拖拽按下会被窗口层丢弃。
/// A nonactivating panel must deliver the first click to the scroller, otherwise the initial
/// drag press is discarded while Command is held.
pub(crate) extern "C" fn thumbnail_scroller_accepts_first_mouse(
    _self: *mut c_void,
    _cmd: Sel,
    _event: *mut c_void,
) -> bool {
    true
}

/// 在非激活面板中显式开始拖拽,不依赖 NSScroller 的原生 tracking。
/// Start dragging explicitly inside the nonactivating panel instead of relying on NSScroller tracking.
pub(crate) extern "C" fn thumbnail_scroller_mouse_down(
    _self: *mut c_void,
    _cmd: Sel,
    event: *mut c_void,
) {
    unsafe {
        let scroller = _self as *mut AnyObject;
        let location: NSPoint = msg_send![event as *mut AnyObject, locationInWindow];
        update_thumbnail_pointer_state(location);
        let point: NSPoint = msg_send![
            scroller,
            convertPoint: location,
            fromView: std::ptr::null::<AnyObject>()
        ];
        let bounds: NSRect = msg_send![scroller, bounds];
        let max_offset = *THUMB_SCROLL_MAX_OFFSET.lock().unwrap();
        let current_offset = *THUMB_SCROLL_OFFSET.lock().unwrap();
        let Some(geometry) =
            thumbnail_scroller_geometry(bounds.size.height, max_offset, current_offset)
        else {
            return;
        };
        let inside_knob = thumbnail_scroller_knob_contains(point.y, geometry);
        let start_offset = if inside_knob || geometry.thumb_travel <= 0.0 {
            current_offset
        } else {
            let target_progress =
                ((geometry.knob_y + geometry.thumb_travel + geometry.knob_h / 2.0 - point.y)
                    / geometry.thumb_travel)
                    .clamp(0.0, 1.0);
            target_progress * max_offset
        };
        if !inside_knob {
            set_thumbnail_scroll_offset(start_offset, max_offset);
        }
        *THUMB_SCROLL_DRAG.lock().unwrap() = Some(ThumbnailScrollDrag {
            start_y: point.y,
            start_offset,
            max_offset,
            thumb_travel: geometry.thumb_travel,
        });
        invalidate_thumbnail_scroller();
    }
}

pub(crate) extern "C" fn thumbnail_scroller_mouse_dragged(
    _self: *mut c_void,
    _cmd: Sel,
    event: *mut c_void,
) {
    unsafe {
        let Some(drag) = *THUMB_SCROLL_DRAG.lock().unwrap() else {
            return;
        };
        let scroller = _self as *mut AnyObject;
        let location: NSPoint = msg_send![event as *mut AnyObject, locationInWindow];
        let point: NSPoint = msg_send![
            scroller,
            convertPoint: location,
            fromView: std::ptr::null::<AnyObject>()
        ];
        let next = thumbnail_scroll_offset_for_drag(
            drag.start_offset,
            drag.start_y,
            point.y,
            drag.max_offset,
            drag.thumb_travel,
        );
        set_thumbnail_scroll_offset(next, drag.max_offset);
    }
}

pub(crate) extern "C" fn thumbnail_scroller_mouse_up(
    _self: *mut c_void,
    _cmd: Sel,
    event: *mut c_void,
) {
    *THUMB_SCROLL_DRAG.lock().unwrap() = None;
    unsafe {
        let point: NSPoint = msg_send![event as *mut AnyObject, locationInWindow];
        update_thumbnail_pointer_state(point);
    }
    invalidate_thumbnail_scroller();
}

/// 更新滚动条的轨道、滑块比例和当前位置;无溢出时完全隐藏。
/// Update the scroller track, knob proportion, and position; hide it when there is no overflow.
pub(super) unsafe fn update_thumbnail_scroller(
    panel_w: f64,
    panel_h: f64,
    overflowed: bool,
    row_count: usize,
    max_rows: usize,
) {
    let Some(scroller) = thumbnail_scroller() else {
        return;
    };
    if !overflowed || row_count <= max_rows || max_rows == 0 {
        let _: () = msg_send![scroller.0, setHidden: true];
        return;
    }
    let footer_h = status_h();
    let frame = NSRect::new(
        NSPoint::new(
            panel_w - H_PADDING - THUMB_SCROLLBAR_W + THUMB_SCROLLBAR_W / 2.0,
            footer_h,
        ),
        NSSize::new(THUMB_SCROLLBAR_W, (panel_h - footer_h).max(1.0)),
    );
    // 拖拽期间保持命中视图的 frame 不变;卡片重建只刷新胶囊绘制。
    // Keep the hit view's frame stable during dragging; card rebuilds only refresh the capsule.
    if THUMB_SCROLL_DRAG.lock().unwrap().is_none() {
        let _: () = msg_send![scroller.0, setFrame: frame];
    }
    let _: () = msg_send![scroller.0, setHidden: false];
    let _: () = msg_send![scroller.0, setNeedsDisplay: true];
}

/// borderless 浮窗重写:允许成为 key 窗口(否则收不到键盘事件)。
/// Override for the borderless overlay window: allow it to become key (otherwise it
/// receives no keyboard events).
pub(crate) extern "C" fn overlay_window_can_become_key(_self: *mut c_void, _cmd: Sel) -> bool {
    true
}
