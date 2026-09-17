//! 浮窗 · cancel:点击浮窗外部取消切换(本地事件监视 + 失焦收起)。
//! Click-outside cancel (local event monitor + focus-loss dismissal).

use super::*;

// ========== 点击外部取消 / click-outside cancel ==========

/// 注册「点击浮窗外部 → 取消本次切换」:浮窗是 key 面板,点击其他 app 的窗口时
/// WindowServer 把 key 转给新窗口 → 面板收到 NSWindowDidResignKeyNotification →
/// 收起浮窗且不切换(与 Esc 取消同语义)。
/// 点击浮窗内部不会触发(面板保持 key);点击面板自身的空白区/卡片由卡片事件处理。
///
/// 为什么不用全局鼠标监听:resign-key 通知天然区分「点击面板内/外」(事件属于本 app
/// 时不通知),无需 block、无需位置判断;且剪贴板面板已用同一模式,行为一致。
///
/// Register click-outside cancel: the overlay is the key panel, so clicking another app's
/// window hands key to it and the panel fires NSWindowDidResignKeyNotification -> dismiss
/// the overlay without switching (same semantics as Esc).
/// Clicks inside the panel never fire it (the panel keeps key); empty areas of the panel
/// are handled by card events.
///
/// Why not a global mouse monitor: the resign-key notification inherently distinguishes
/// inside/outside clicks (it doesn't fire for our own events), needs no blocks and no
/// hit-testing; the clipboard picker already uses this exact pattern.
pub(crate) fn install_click_to_cancel() {
    unsafe {
        let win = match *OVERLAY_WINDOW.lock().unwrap() {
            Some(w) => w.0,
            None => return,
        };
        let center: *mut AnyObject = msg_send![class!(NSNotificationCenter), defaultCenter];
        let name = make_nsstring("NSWindowDidResignKeyNotification");
        let _: () = msg_send![
            center,
            addObserver: overlay_observer(),
            selector: sel!(overlayWindowResigned:),
            name: name,
            object: win
        ];
        CFRelease(name as *const c_void);
    }
}

/// overlay 专用的通知观察者单例(只承载 resign-key 回调)。
/// Singleton notification observer for the overlay (carries the resign-key callback only).
unsafe fn overlay_observer() -> *mut AnyObject {
    static OBSERVER: OnceLock<CallbackTarget> = OnceLock::new();
    OBSERVER
        .get_or_init(|| {
            let name = CString::new("OhMyTabOverlayObserver").unwrap();
            let superclass = class!(NSObject) as *const _ as *mut AnyObject;
            let cls = objc_allocateClassPair(superclass, name.as_ptr(), 0);
            let types = CString::new("v@:@").unwrap();
            class_addMethod(
                cls,
                sel!(overlayWindowResigned:),
                overlay_window_resigned as *mut c_void,
                types.as_ptr(),
            );
            objc_registerClassPair(cls);
            let inst: *mut AnyObject = msg_send![cls as *const AnyObject, new];
            CallbackTarget::new(inst)
        })
        .0
}

/// 浮窗失去 key → 取消切换。
/// The overlay lost key -> cancel the switch.
extern "C" fn overlay_window_resigned(_self: *mut c_void, _cmd: Sel, _note: *mut c_void) {
    crate::callback_guard::void("overlay_window_resigned", || {
        overlay_window_resigned_inner();
    });
}

fn overlay_window_resigned_inner() {
    // Closing the settings card intentionally hides our settings window and can make the
    // nonactivating overlay resign key as a side effect. The close transition owns that focus
    // change; do not mistake it for a click outside and hide the overlay while it is reflowing.
    //
    // 关闭设置卡片会主动隐藏设置窗口,可能连带让非激活切换面板失去 key。这个焦点变化属于
    // 关闭流程本身,不能在补位动画期间误判为点击外部并收起切换浮窗。
    if card_close_in_progress() {
        return;
    }
    // Read and update the visibility in one short borrow. The release handler clears `visible`
    // before any AppKit call, so this callback can safely be re-entered by `hide_overlay`.
    // 在一次短借用中读取并更新可见状态。释放回调会在调用 AppKit 前先清除 `visible`，因此
    // 即使 `hide_overlay` 同步重入本回调也不会再次借用 runtime。
    let should_hide = with_tab_state(|state_opt| match state_opt.as_mut() {
        Some(state) if state.visible => {
            state.visible = false;
            true
        }
        _ => false,
    });
    if should_hide {
        // resign-key 不等于点击外部:系统面板、前台应用临时窗口或 AppKit 焦点重分配
        // 都可能让 nonactivating panel 失去 key。记录足以区分这些场景的非敏感状态。
        // Resigning key is not synonymous with an outside click: a system panel, a transient
        // foreground-app window, or AppKit focus reassignment can all displace a nonactivating
        // panel. Capture only the non-sensitive state needed to distinguish those cases.
        unsafe {
            // The helper ends the slot borrow before any AppKit query can re-enter us.
            // helper 会在调用 AppKit 查询前结束槽位借用,避免回调重入时再次借用。
            let window = overlay_window_ptr();
            let pointer_inside = window.is_some_and(|window| {
                let frame: NSRect = msg_send![window, frame];
                let mouse: NSPoint = msg_send![class!(NSEvent), mouseLocation];
                mouse.x >= frame.origin.x
                    && mouse.x <= frame.origin.x + frame.size.width
                    && mouse.y >= frame.origin.y
                    && mouse.y <= frame.origin.y + frame.size.height
            });
            let pressed_mouse_buttons: usize = msg_send![class!(NSEvent), pressedMouseButtons];
            let nsapp: *mut AnyObject = msg_send![class!(NSApplication), sharedApplication];
            let app_active: bool = msg_send![nsapp, isActive];
            let current_event: *mut AnyObject = msg_send![nsapp, currentEvent];
            let current_event_type: Option<usize> = if current_event.is_null() {
                None
            } else {
                Some(msg_send![current_event, type])
            };
            let own_key_window: *mut AnyObject = msg_send![nsapp, keyWindow];
            let own_key = match window {
                Some(window) if own_key_window == window => "overlay",
                _ if own_key_window.is_null() => "none",
                _ => "other",
            };
            let live_flags = event_tap::combined_session_flags();
            let command_down = live_flags & NSEVENT_MODIFIER_FLAG_COMMAND != 0;
            let option_down = live_flags & NSEVENT_MODIFIER_FLAG_OPTION != 0;
            let (frontmost_app, frontmost_pid) = frontmost_app_info();
            log_debug!(
                "[overlay] cancelled after key loss: pointer_inside={} mouse_buttons=0x{:x} current_event_type={:?} command_down={} option_down={} app_active={} own_key={} frontmost_pid={} frontmost_app=\"{}\"",
                pointer_inside,
                pressed_mouse_buttons,
                current_event_type,
                command_down,
                option_down,
                app_active,
                own_key,
                frontmost_pid,
                frontmost_app
            );
        }
        hide_overlay();
    }
}

/// 关闭索引 removed_idx 的窗口后调整选中索引(纯函数,单测覆盖):
///
/// - 被关窗口在选中项之前 → 选中前移一格(保持指向同一张窗口);
/// - 被关窗口就是选中项或在其后 → 不动(前者自然指向下一张);
/// - 越界 → 钳到末条;空列表 → 0。
///
/// Adjust the selection after closing the window at `removed_idx` (pure, unit-tested):
///
/// - a closed window BEFORE the selection shifts it back one (same window stays selected);
/// - closing the selection itself or anything after it leaves it (the former naturally
///   points at the next window);
/// - out of range -> the tail; an empty list -> 0.
pub(super) fn remove_window_adjust_selection(
    selected: usize,
    removed_idx: usize,
    new_len: usize,
) -> usize {
    let sel = if removed_idx < selected {
        selected - 1
    } else {
        selected
    };
    if new_len == 0 {
        0
    } else {
        sel.min(new_len - 1)
    }
}

/// 关闭第 idx 张卡片对应的窗口(小叉按钮 / Backspace 共用):AX 关闭成功后
/// 从列表移除并调整选中;没有对应 view 时才使用重建兜底。全部关完 → 收起浮窗。
/// Close the window of card `idx` (shared by the close button and Backspace): on a
/// successful AX close, remove it from the list and adjust selection; rebuild only as a
/// fallback when no card view exists. Closing the last one dismisses the overlay.
pub(crate) fn close_window_at(idx: usize) -> bool {
    let Some((pid, cgwid)) = with_tab_state(|state_opt| {
        let state = state_opt.as_ref()?;
        state.windows.get(idx).map(|w| (w.pid, w.window_id))
    }) else {
        return false;
    };
    // The settings window belongs to this process. Its custom AX close action would re-enter
    // AppKit from a background close worker and can crash; close it directly on the main thread.
    //
    // 本进程的设置窗口不能走后台关闭线程:它的 AX 关闭动作会回调 AppKit,从后台线程重入
    // UI 可能崩溃。直接在主线程走设置窗口的关闭路径。
    if pid == std::process::id() as i32 {
        crate::close_settings_for_switcher();
        return finish_window_close(idx, pid, cgwid);
    }
    if !crate::window_collector::close_ax_window(pid, cgwid) {
        log_info!(
            "close window FAILED (AX close rejected): pid={} cgwid={}",
            pid,
            cgwid
        );
        return false;
    }
    finish_window_close(idx, pid, cgwid)
}

/// 没有对应卡片时的同步关闭兜底;正常的卡片关闭走 commit_pending_card_close。
/// Synchronous fallback for a close without a corresponding card; normal card closes use
/// commit_pending_card_close.
fn finish_window_close(idx: usize, pid: i32, cgwid: u32) -> bool {
    let valid = with_tab_state(|state_opt| {
        let Some(state) = state_opt.as_ref() else {
            return false;
        };
        let Some(window) = state.windows.get(idx) else {
            return false;
        };
        window.pid == pid && window.window_id == cgwid
    });
    if !valid {
        return false;
    }
    log_info!("close window: pid={} cgwid={}", pid, cgwid);
    let close_result = with_tab_state(|state_opt| {
        let state = match state_opt.as_mut() {
            Some(s) => s,
            None => return (false, false),
        };
        let was_visible = state.visible;
        let Some(actual_idx) = state
            .windows
            .iter()
            .position(|window| window.pid == pid && window.window_id == cgwid)
        else {
            return (false, false);
        };
        state.windows.remove(actual_idx);
        WINDOW_COUNT.store(state.windows.len(), std::sync::atomic::Ordering::Release);
        state.mru.remove(&(pid, cgwid));
        if state.windows.is_empty() {
            // 全部关完:收起浮窗,不留在空态。
            // All closed: dismiss the overlay, don't linger on an empty state.
            state.visible = false;
            return (true, was_visible);
        }
        state.selected =
            remove_window_adjust_selection(state.selected, actual_idx, state.windows.len());
        if !was_visible {
            return (true, false);
        }
        (true, false)
    });
    if !close_result.0 {
        return false;
    }
    if close_result.1 {
        hide_overlay();
        return true;
    }
    // 兜底路径允许完整重建;卡片关闭按钮本身不会走到这里。
    // The fallback may rebuild the overlay; the card close-button path never reaches it.
    reset_thumbnail_visible_range();
    reset_thumbnail_nav_anchor();
    show_overlay();
    refresh_highlight();
    true
}

/// 视觉隐藏浮窗但**不 orderOut**(窗口保持 ordered)。
/// 切换窗口时不能先 orderOut 再激活目标:面板 orderOut 后 WindowServer 可能把焦点路由到
/// 错误窗口,导致目标窗口的 key-window / first-responder 未被正确确立(光标停止闪烁等)。
/// 对齐 BetterCmdTab 的 vanish() -> activate() -> dismiss() 时序。
///
/// Visually hide the overlay **without orderOut** (the window stays ordered).
/// Ordering out before activating the target lets WindowServer route focus to the wrong window,
/// leaving the target's key-window / first-responder unset (caret stops blinking, etc.).
/// Mirrors BetterCmdTab's vanish() -> activate() -> dismiss() sequence.
pub(crate) fn vanish_overlay() {
    stop_hover_timer();
    clear_thumbnail_scroll_drag();
    set_thumbnail_scroller_hover(false, false);
    // Copy both pointers before any AppKit call; resignKeyWindow can synchronously notify us.
    // 在调用 AppKit 前复制两个指针;resignKeyWindow 可能同步触发通知回调。
    let window = overlay_window_ptr();
    let container = overlay_container_ptr();
    unsafe {
        if let Some(window) = window {
            // alphaValue=0 + contentView hidden:即时视觉消失,但窗口保持 ordered。
            // alphaValue=0 + contentView hidden: instant visual hide, window stays ordered.
            let _: () = msg_send![window, setAlphaValue: 0.0f64];
            if let Some(container) = container {
                let _: () = msg_send![container, setHidden: true];
            }
            // 忽略鼠标事件,防止隐形面板吞点击(直到 delayed orderOut 真正移除它)。
            // Ignore mouse events so the invisible panel doesn't swallow clicks (until the
            // delayed orderOut actually removes it).
            let _: () = msg_send![window, setIgnoresMouseEvents: true];
            // 释放面板的 key window 状态:否则 0.2s 后 orderOut 时 AppKit 会把 key 提升给
            // 我们 app 的下一个可见窗口(设置窗口),重新激活我们,把目标窗口的焦点抢走
            // (目标红绿灯变灰,日志里可见切换后我们 app 的激活通知反复出现)。
            // 先释放 key 再激活目标,目标才能干净地拿到 key 焦点。
            // Resign the panel's key-window state: otherwise, when orderOut fires 0.2s later,
            // AppKit promotes the key to our app's next visible window (the settings window),
            // re-activating us and stealing focus from the target (grey traffic lights; the log
            // shows our app's activation notification repeatedly following switches). Resigning
            // key before activating the target lets the target take key focus cleanly.
            let _: () = msg_send![window, resignKeyWindow];
        }
    }
}

/// 延迟 orderOut 回调:vanish_overlay 之后由 performSelector:withObject:afterDelay: 调用,
/// 在目标窗口激活完成后真正移除浮窗。此时 WindowServer 焦点路由已稳定,orderOut 不会干扰。
///
/// Delayed orderOut callback: called via performSelector:withObject:afterDelay: after
/// vanish_overlay, removing the overlay for real once the target window's activation has
/// settled and WindowServer focus routing is stable.
pub(crate) extern "C" fn on_delayed_order_out(_self: *mut c_void, _cmd: Sel, _arg: *mut c_void) {
    crate::callback_guard::void("on_delayed_order_out", on_delayed_order_out_inner);
}

pub(super) fn delayed_order_out_should_hide(overlay_visible: bool) -> bool {
    !overlay_visible
}

fn on_delayed_order_out_inner() {
    let overlay_visible =
        with_tab_state(|state_opt| state_opt.as_ref().is_some_and(|state| state.visible));
    if !delayed_order_out_should_hide(overlay_visible) {
        // A new summon can happen before an older delayed callback fires. Do not order out the
        // new overlay; only restore the visual state left by the previous vanish.
        // 旧回调可能在新一轮召唤后才触发。不能收起新的浮窗,这里只恢复上一次 vanish 留下的显示状态。
        log_debug!("[overlay] skipped stale delayed orderOut while overlay is visible");
    } else {
        hide_overlay();
    }
    // 恢复浮窗的 alphaValue / contentView 可见性 / 鼠标事件,下次 show_overlay 时正常显示。
    // Restore the overlay's alphaValue / contentView visibility / mouse events for the next
    // show_overlay call.
    let window = overlay_window_ptr();
    let container = overlay_container_ptr();
    unsafe {
        if let Some(window) = window {
            let _: () = msg_send![window, setAlphaValue: 1.0f64];
            let _: () = msg_send![window, setIgnoresMouseEvents: false];
        }
        if let Some(container) = container {
            let _: () = msg_send![container, setHidden: false];
        }
    }
}

/// 延迟一拍的切换抬升槽 + 调度。
/// 释放/点击/回车的处理函数先 vanish_overlay 并结束当前 runloop turn,让渲染事务提交
/// (vanish 真正上屏、浮窗立即消失),下一个 runloop 周期才执行激活+抬升链。
/// 之前 vanish 与激活+AX 链挤在同一次主线程 turn 里:AX 枚举阻塞主线程期间 vanish 无法
/// 提交,表现为「窗口已经切过去,浮窗冻结在上面顿一下才消失」。
///
/// Deferred-raise slot + scheduling. The release/click/Enter handlers vanish the overlay and
/// END their runloop turn so the render transaction commits (the vanish reaches the screen,
/// the overlay disappears instantly); the activate+raise chain runs on the NEXT runloop cycle.
/// Previously the vanish and the activate+AX chain shared one main-thread turn: while the AX
/// enumeration blocked it, the vanish could not commit -- the overlay lingered, frozen, over
/// the already-switched window.
struct DeferredRaise {
    pid: i32,
    cgwid: u32,
    minimized: bool,
    scheduled_at: Instant,
}

static DEFERRED_RAISE: LazyLock<Mutex<Option<DeferredRaise>>> = LazyLock::new(|| Mutex::new(None));

pub(super) fn schedule_deferred_raise(pid: i32, cgwid: u32, minimized: bool) {
    *DEFERRED_RAISE.lock().unwrap() = Some(DeferredRaise {
        pid,
        cgwid,
        minimized,
        scheduled_at: Instant::now(),
    });
    unsafe {
        let ctrl = crate::CONTROLLER.lock().unwrap().unwrap().0;
        // afterDelay:0 = 当前 turn 结束后尽快执行——先提交 vanish,再抬升。
        // afterDelay:0 = run as soon as the current turn ends: commit the vanish first, then raise.
        let _: () = msg_send![
            ctrl,
            performSelector: sel!(handleDeferredRaise:),
            withObject: std::ptr::null::<AnyObject>(),
            afterDelay: 0.0f64
        ];
    }
}

pub(crate) extern "C" fn on_deferred_raise(_self: *mut c_void, _cmd: Sel, _arg: *mut c_void) {
    let Some(job) = DEFERRED_RAISE.lock().unwrap().take() else {
        return;
    };
    // 释放→本回调的间隔 = 「先提交 vanish」付出的额外延迟,正常应为几毫秒。
    // Release-to-callback gap = the extra delay paid for committing the vanish first;
    // normally a few milliseconds.
    log_debug!(
        "[raise] deferred fire: pid={} cgwid={} +{}ms after release",
        job.pid,
        job.cgwid,
        job.scheduled_at.elapsed().as_millis()
    );
    activate_and_raise(job.pid, job.cgwid, job.minimized);
}

pub(super) fn cancel_scheduled_order_out() {
    unsafe {
        let Some(ctrl) = crate::CONTROLLER
            .lock()
            .unwrap()
            .map(|controller| controller.0)
        else {
            return;
        };
        let _: () = msg_send![
            class!(NSObject),
            cancelPreviousPerformRequestsWithTarget: ctrl,
            selector: sel!(handleDelayedOrderOut:),
            object: std::ptr::null::<AnyObject>()
        ];
    }
}

/// 在主线程上延迟 0.2s 执行 orderOut(通过 controller 的 handleDelayedOrderOut:)。
/// vanish_overlay() 之后调用此函数:目标窗口的激活会在 0.2s 内完成,之后才真正移除浮窗,
/// 避免 orderOut 干扰 WindowServer 焦点路由。
///
/// Schedule a delayed orderOut on the main thread (via the controller's handleDelayedOrderOut:).
/// Called after vanish_overlay(): the target window's activation completes within 0.2s, after
/// which the overlay is removed for real, avoiding orderOut interfering with WindowServer focus.
pub(super) fn schedule_delayed_order_out() {
    unsafe {
        let ctrl = crate::CONTROLLER.lock().unwrap().unwrap().0;
        // performSelector:withObject:afterDelay: 在主线程 RunLoop 上延迟调度。
        // performSelector:withObject:afterDelay: schedules on the main thread's RunLoop.
        let _: () = msg_send![
            ctrl,
            performSelector: sel!(handleDelayedOrderOut:),
            withObject: std::ptr::null::<AnyObject>(),
            afterDelay: 0.2f64
        ];
    }
}

pub(crate) fn refresh_highlight() {
    unsafe {
        let document = match card_document() {
            Some(document) => document,
            None => return,
        };
        let Some(selected) = with_tab_state(|state_opt| {
            let state = state_opt.as_ref()?;
            state.visible.then_some(state.selected)
        }) else {
            return;
        };
        let colors = current_colors();
        // 选中态采用 HTML 参考中的轻量背景和 1.5px 内描边,不再使用厚重的蓝色边框。
        // Match the HTML reference with a subtle background and 1.5px inset-style border instead of
        // the previous heavy blue outline.
        let sel_bg_color = hex_to_cg_color(colors.card_bg_sel);
        let sel_border_color = hex_to_cg_color(colors.card_border_sel);

        for sv in card_views(document) {
            let layer: *mut AnyObject = msg_send![sv, layer];
            let Some(tag) = get_card_index(sv) else {
                continue;
            };
            // 读卡片标题 label 文本,验证内容与索引对应(排查"显示 Picview 却打开 Ghostty")。
            // Read the card's title-label text to verify content matches the index (investigating
            // "shows Picview but opens Ghostty").
            let is_selected = tag == selected;
            let preview: *mut AnyObject = msg_send![sv, viewWithTag: THUMB_PREVIEW_TAG];
            if !preview.is_null() {
                // HTML 把 translateY(-1px) 施加在 `.item.selected` 根元素，而不是
                // `.preview`；在 AppKit 坐标中以 +1pt 平移卡片根层，标题行与预览区
                // 才会作为一个整体上浮。
                // The HTML applies translateY(-1px) to the `.item.selected` root rather
                // than `.preview`; +1pt in AppKit coordinates lifts the caption row and
                // preview together as one card.
                layer_set_translation_y(layer, thumbnail_card_lift_y(is_selected));
            }
            if is_selected {
                // 设计稿 .item.selected:1.5px 清晰 accent 描边 rgba(75,123,236,.78)。
                // 白底上柔色圈不可见,轮廓线必须用实色 accent 才能显形。
                // The mockup's .item.selected: a crisp 1.5px accent border
                // rgba(75,123,236,.78). A soft ring is invisible on the white
                // surface -- the outline needs the solid accent to show.
                let _: () = msg_send![layer, setBorderWidth: 1.5f64];
                layer_set_border(layer, sel_border_color);
                layer_set_background(layer, sel_bg_color);
                // 投影:0 10px 24px rgba(42,62,102,.12)(设计稿同款)。CSS blur 24 ≈
                // CALayer shadowRadius 12;CALayer shadowOffset y 正值向上,向下投影取 -10。
                // Drop shadow: 0 10px 24px rgba(42,62,102,.12), straight from the mockup.
                // CSS blur 24 ≈ CALayer shadowRadius 12; CALayer's shadowOffset y is up-positive,
                // so a downward shadow takes -10.
                let shadow_color = hex_to_cg_color(0x2A3E66FF);
                // CGColorRef 不能进 objc2 的 msg_send!('@' vs '^{CGColor=}' 运行时
                // 拒绝,实测召唤即崩),照 layer_set_background 惯例走裸 objc_msgSend。
                // A CGColorRef cannot go through objc2's msg_send! ('@' vs
                // '^{CGColor=}' rejected at runtime -- crashed the summon, verified);
                // use raw objc_msgSend per the layer_set_background convention.
                layer_set_shadow_color(layer, shadow_color);
                let _: () = msg_send![layer, setShadowOpacity: 0.12f32];
                let _: () = msg_send![layer, setShadowRadius: 12.0f64];
                let _: () = msg_send![layer, setShadowOffset: NSSize::new(0.0, -10.0)];
            } else {
                let _: () = msg_send![layer, setBorderWidth: 0.0f64];
                layer_set_border(layer, std::ptr::null_mut());
                layer_set_background(layer, std::ptr::null_mut());
                let _: () = msg_send![layer, setShadowOpacity: 0.0f32];
            }

            // CSS 的第一层 box-shadow 是卡片外侧 2px、零模糊的 accent-soft 圈,
            // 不能与下面的深色模糊投影共用 CALayer.shadow。独立 ring 视图保留
            // RGB 并把 alpha 提升到适合 Liquid Glass 的 38%,再叠一层零偏移蓝色柔光；
            // 只在缩略图模式选中时显示。
            // The first CSS box-shadow is a zero-blur 2px accent-soft ring outside the
            // card; it cannot share CALayer.shadow with the dark blurred drop shadow.
            // A dedicated ring view preserves the RGB, raises alpha to 38% for Liquid
            // Glass, adds a zero-offset blue glow, and appears only on the selected card.
            let ring: *mut AnyObject = msg_send![sv, viewWithTag: THUMB_SELECTION_RING_TAG];
            if !ring.is_null() {
                let ring_layer: *mut AnyObject = msg_send![ring, layer];
                layer_set_border(
                    ring_layer,
                    hex_to_cg_color(color_with_alpha(
                        colors.card_border_sel,
                        SELECTION_RING_ALPHA,
                    )),
                );
                layer_set_shadow_color(
                    ring_layer,
                    hex_to_cg_color(color_with_alpha(colors.card_border_sel, 0xFF)),
                );
                let glow_opacity = if is_selected {
                    SELECTION_GLOW_OPACITY
                } else {
                    0.0
                };
                let _: () = msg_send![ring_layer, setShadowOpacity: glow_opacity];
                let _: () = msg_send![ring_layer, setShadowRadius: SELECTION_GLOW_RADIUS];
                let _: () = msg_send![ring_layer, setShadowOffset: NSSize::new(0.0, 0.0)];
                let _: () = msg_send![ring, setHidden: !is_selected];
            }

            // 图标在选中态向上轻移 2pt;每次都从基准 y 重算,避免反复
            // 切换时累计位移。
            // Nudge the icon up by 2pt when selected; recompute from the baseline on every
            // refresh so repeated selection changes never accumulate the offset.
            let icon: *mut AnyObject = msg_send![sv, viewWithTag: ICON_VIEW_TAG];
            if !icon.is_null() {
                let icon_frame: NSRect = msg_send![icon, frame];
                let icon_px_now = icon_frame.size.height;
                let icon_bottom = card_h() - 8.0 - icon_px();
                let base_y = if (icon_px_now - icon_px()).abs() < 0.5 {
                    icon_bottom
                } else {
                    icon_bottom + (icon_px() - icon_px_now) / 2.0
                };
                let icon_y = base_y
                    + if is_selected {
                        SELECTED_CONTENT_NUDGE
                    } else {
                        0.0
                    };
                let _: () = msg_send![
                    icon,
                    setFrameOrigin: NSPoint::new(icon_frame.origin.x, icon_y)
                ];
            }

            // 缩略图模式:预览区自身不再单独位移，整卡根层已携带标题与预览共同上浮；
            // 预览容器保持透明且无固定描边,选中态仅由卡片外圈表达。
            // Thumbnail mode: the preview no longer moves independently because the card
            // root now lifts the caption and preview together; the transparent, borderless
            // preview is represented by the card's outer selection ring only.

            // ⌫ 关闭按钮随选中态显隐:选中卡片显示、其余隐藏(选中即出现,
            // 不限于鼠标悬停——键盘导航选中同样可见)。
            // The ⌫ close button follows the selection: the selected card shows it, the
            // rest hide it (visible whenever the card is selected, keyboard navigation
            // included -- not only while the mouse hovers).
            let btn: *mut AnyObject = msg_send![sv, viewWithTag: CLOSE_BTN_TAG];
            if !btn.is_null() {
                let _: () = msg_send![btn, setHidden: tag != selected];
            }
        }
    }
}

pub(crate) fn extract_uncached_icons() {
    let uncached: Vec<i32> = with_tab_state(|state_opt| {
        if let Some(state) = state_opt.as_ref() {
            state
                .windows
                .iter()
                .filter(|w| w.icon_path.is_none())
                .map(|w| w.pid)
                .collect::<HashSet<_>>()
                .into_iter()
                .collect()
        } else {
            Vec::new()
        }
    });

    // Record which window indices got a freshly cached icon so we can re-render
    // just those cards in place (otherwise the on-screen letter icons wouldn't
    // update until the next summon).
    let mut updated_indices: Vec<usize> = Vec::new();
    // TIMING-DEBUG 逐 PID 提取计时:定位是哪个 app 的图标提取拖慢 summon。
    let mut icons_total_ms: u128 = 0; // TIMING-DEBUG
    for pid in uncached {
        let t_icon = Instant::now(); // TIMING-DEBUG
        if let Some(ref path) = extract_icon_to_cache(pid) {
            let path = path.clone();
            with_tab_state(|state_opt| {
                if let Some(state) = state_opt.as_mut() {
                    for (i, w) in state.windows.iter_mut().enumerate() {
                        if w.pid == pid && w.icon_path.is_none() {
                            w.icon_path = Some(path.clone());
                            updated_indices.push(i);
                        }
                    }
                }
            });
        }
        let icon_ms = t_icon.elapsed().as_millis(); // TIMING-DEBUG
        icons_total_ms += icon_ms;
        // TIMING-DEBUG 标记慢提取(≥20ms)。
        if icon_ms >= 20 {
            log_debug!("[overlay] icons: extract pid={} {}ms", pid, icon_ms);
        }
    }

    if !updated_indices.is_empty() {
        let t_rebuild = Instant::now(); // TIMING-DEBUG
        rebuild_cards(&updated_indices);
        // TIMING-DEBUG 汇总:提取总耗时 + 卡片就地重建耗时。
        log_debug!(
            "[overlay] icons: extract_total={}ms rebuild_cards x={} {}ms",
            icons_total_ms,
            updated_indices.len(),
            t_rebuild.elapsed().as_millis()
        );
    }
}

/// Rebuild the card views for the given window indices in place, so newly
/// extracted icons appear immediately without re-summoning. Each affected card
/// is replaced by a fresh one built from the updated `WindowInfo` (which now has
/// an icon_path), preserving its frame and card index.
pub(crate) fn rebuild_cards(indices: &[usize]) {
    if indices.is_empty() || card_close_in_progress() {
        return;
    }
    let affected: HashSet<usize> = indices.iter().copied().collect();
    let to_rebuild: HashMap<usize, WindowInfo> = with_tab_state(|state_opt| {
        let Some(state) = state_opt.as_ref() else {
            return HashMap::new();
        };
        if !state.visible {
            return HashMap::new();
        }
        affected
            .iter()
            .filter_map(|&i| state.windows.get(i).map(|w| (i, w.clone())))
            .collect()
    });
    if to_rebuild.is_empty() {
        return;
    }

    unsafe {
        let thumbnail_capture_allowed =
            crate::theme::thumbnails_enabled() && crate::thumbnail::capture_allowed();
        let document = match card_document() {
            Some(document) => document,
            None => return,
        };

        // Collect affected card views + their frames first; don't mutate the
        // subview array while iterating it.
        let mut replacements: Vec<(*mut AnyObject, NSRect, usize)> = Vec::new();
        for sv in card_views(document) {
            let Some(idx) = get_card_index(sv) else {
                continue;
            };
            if to_rebuild.contains_key(&idx) {
                let frame: NSRect = msg_send![sv, frame];
                replacements.push((sv, frame, idx));
            }
        }

        for (old_view, frame, idx) in replacements {
            if let Some(w) = to_rebuild.get(&idx) {
                remove_card_index(old_view);
                // 沿用旧卡 frame 的宽高(原位替换:流式布局缩卡后高宽都是逐卡值)。
                // Reuse the old card frame's width AND height (in-place replacement:
                // after flow shrink both are per-card values).
                let new_card = create_card_view(
                    w,
                    idx,
                    frame.size.width,
                    frame.size.height,
                    thumbnail_capture_allowed,
                );
                let _: () = msg_send![new_card, setFrame: frame];
                let _: () = msg_send![old_view, removeFromSuperview];
                let _: () = msg_send![document, addSubview: new_card];
                release_obj(new_card); // container owns the card; drop create_card_view's alloc +1
            }
        }

        // New card views have no selection border; re-apply the highlight.
        refresh_highlight();
    }
}

/// 缩略图捕获完成后的轻量更新：只刷新受影响卡片的预览容器，不重建标题、按钮、
/// tracking area 或选中图层。一次 ready 批次只扫描一次容器子视图。
/// Lightweight post-capture update: refresh only affected cards' preview containers,
/// without rebuilding captions, buttons, tracking areas, or selection layers. One
/// ready batch scans the container's subviews once.
pub(crate) fn refresh_thumbnail_previews(keys: &[(i32, u32)]) {
    if keys.is_empty() || card_close_in_progress() {
        return;
    }
    let affected: HashSet<WindowKey> = keys.iter().copied().collect();
    let windows: HashMap<WindowKey, WindowInfo> = with_tab_state(|state_opt| {
        let Some(state) = state_opt.as_ref() else {
            return HashMap::new();
        };
        if !state.visible {
            return HashMap::new();
        }
        state
            .windows
            .iter()
            .filter_map(|window| {
                let key = (window.pid, window.window_id);
                affected.contains(&key).then(|| (key, window.clone()))
            })
            .collect()
    });
    if windows.is_empty() {
        return;
    }

    unsafe {
        let started = Instant::now();
        let capture_allowed =
            crate::theme::thumbnails_enabled() && crate::thumbnail::capture_allowed();
        let colors = current_colors();
        let document = match card_document() {
            Some(document) => document,
            None => return,
        };
        let mut updated = 0usize;
        for card in card_views(document) {
            let Some(key) = card_key(card) else {
                continue;
            };
            let Some(window) = windows.get(&key) else {
                continue;
            };
            let preview: *mut AnyObject = msg_send![card, viewWithTag: THUMB_PREVIEW_TAG];
            if preview.is_null() {
                continue;
            }
            // 填充前记录帧版本,填充后同步进签名;竞态下签名至多偏旧一版,
            // 下一次召唤 Replace 自愈。
            // Snapshot the frame version before populating and sync it into the
            // signature afterwards; under a race the signature lags at most one
            // version and the next summon self-heals via Replace.
            let epoch = crate::thumbnail::frame_epoch(key.0, key.1);
            populate_thumbnail_preview(preview, window, &colors, capture_allowed);
            sync_card_signature_epoch(card, epoch);
            updated += 1;
        }
        log_debug!(
            "[thumb] preview refresh: requested={} updated={} ms={}",
            windows.len(),
            updated,
            started.elapsed().as_millis()
        );
    }
}

/// 把 CONFIG 里的玻璃属性(style/tint/cornerRadius)重新应用到已存在的 NSGlassEffectView,
/// 用于设置热重载。仅 macOS 26+ 且玻璃视图已创建时生效;否则空操作。
/// Re-apply glass properties (style/tint/cornerRadius) from CONFIG to the existing
/// NSGlassEffectView, for hot reload. Only effective on macOS 26+ once the glass view
/// exists; otherwise a no-op.
pub(crate) unsafe fn apply_glass_properties() {
    let glass = match *GLASS_VIEW.lock().unwrap() {
        Some(g) => g.0,
        None => return,
    };
    if glass.is_null() {
        return;
    }
    let radius = CONFIG.read().unwrap().appearance.corner_radius;
    let style_name = config::effective_glass_style();
    let tint_hex = config::parse_hex8(&config::effective_glass_tint());
    let _: () = msg_send![glass, setCornerRadius: radius];
    // 同步 layer 的硬裁剪:cornerRadius 只圆着色不圆模糊,需 masksToBounds 把模糊也裁进圆角
    // (见 create_overlay_window 的 (6.5) 注释)。
    // Mirror the layer hard-clip: cornerRadius rounds the tint but not the blur, so masksToBounds
    // is needed to clip the blur into the rounded shape (see (6.5) in create_overlay_window).
    let glass_layer: *mut AnyObject = msg_send![glass, layer];
    if !glass_layer.is_null() {
        let _: () = msg_send![glass_layer, setCornerRadius: radius];
        let _: () = msg_send![glass_layer, setMasksToBounds: true];
    }
    let style: i64 = match style_name.as_str() {
        "clear" => 1,
        _ => 0, // regular
    };
    let _: () = msg_send![glass, setStyle: style];
    let tint = hex_to_ns_color(tint_hex);
    let _: () = msg_send![glass, setTintColor: tint];
}

pub(crate) fn apply_theme() {
    // Rebuild visible cards as well as updating the window material. Card labels and preview
    // layers use concrete colors chosen at creation time, so changing only NSAppearance leaves
    // existing cards with the previous palette until the overlay is summoned again.
    // 主题变化除了更新窗口材质,还要重建当前可见卡片。卡片文字和预览图层在创建时写入具体颜色,
    // 只设置 NSAppearance 会让已存在的卡片继续使用旧调色板,直到下次重新召唤。
    let visible_indices = with_tab_state(|state_opt| {
        state_opt
            .as_ref()
            .filter(|state| state.visible)
            .map(|state| (0..state.windows.len()).collect::<Vec<_>>())
            .unwrap_or_default()
    });

    let is_dark = crate::theme::resolved_is_dark();
    let theme_changed = {
        let mut last = LAST_APPLIED_THEME_DARK.lock().unwrap();
        let changed = *last != Some(is_dark);
        *last = Some(is_dark);
        changed
    };

    unsafe {
        // 主题来源于 config 的解析结果;显式主题由设置页保存,auto 主题由系统外观通知触发刷新。
        // The theme comes from the resolved config; explicit themes are saved by Settings, while
        // auto themes are refreshed from the system appearance notification.
        // Update window appearance for blur material tint
        if let Some(window) = *OVERLAY_WINDOW.lock().unwrap() {
            let appearance_name = if is_dark {
                make_nsstring("NSAppearanceNameDarkAqua")
            } else {
                make_nsstring("NSAppearanceNameAqua")
            };
            let appearance: *mut AnyObject =
                msg_send![class!(NSAppearance), appearanceNamed: appearance_name];
            CFRelease(appearance_name as *const c_void);
            if !appearance.is_null() {
                let _: () = msg_send![window.0, setAppearance: appearance];
            }
        }

        apply_glass_properties();
    }

    if visible_indices.is_empty() {
        refresh_highlight();
    } else {
        rebuild_cards(&visible_indices);
    }
    // Theme changes also alter the pixels inside captured application windows. Rebuild the card
    // tree immediately and force a fresh capture for every known window so the next preview does
    // not keep a frame from the previous light/dark appearance.
    // 主题变化也会改变应用窗口截图中的像素。先立即重建卡片树,再强制重拍所有已知窗口,
    // 避免预览继续显示切换前的明暗主题。
    if theme_changed {
        let target_px_h = *THUMB_CAPTURE_TARGET_PX_H.lock().unwrap();
        crate::thumbnail::refresh_for_theme(target_px_h);
    }
    update_status_label();
}
