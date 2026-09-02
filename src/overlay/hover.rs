//! 切换器浮窗 · 悬停机制:命中检测、悬停计时器与延迟滚动悬停。
//! Hover machinery: hit-testing, the hover tick timer, and deferred scroll hover.

use super::*;

/// 按浮窗窗口坐标命中卡片并更新选中(主线程调用)。
/// 鼠标来源有两种:container 的 tracking area(mouseMoved:)与鼠标事件 tap(经
/// performSelectorOnMainThread 跳转)——两者都收敛到这里,坐标已转成浮窗窗口坐标。
/// Select the card under a point in the overlay's window space (main thread).
/// Two mouse sources converge here: the container's tracking area (mouseMoved:) and the
/// mouse event tap (hopped here via performSelectorOnMainThread), both with the point
/// already converted into the overlay's window space.
pub(crate) fn handle_hover_at(loc: NSPoint) {
    if card_close_in_progress() {
        return;
    }
    // 移动本身即"开门"信号,同时按鼠标当前位置补选中。
    // 为什么要补:浮窗打开瞬间鼠标可能已在卡片下,那次 mouseEntered 被门控吞掉且不会
    // 重发(已 inside)——若只靠 mouseEntered,侧键召唤场景 hover 永远不选中(实测)。
    // A move is itself the "gate open" signal; also select the card under the cursor.
    // Why: the overlay may open with the cursor already over a card -- that mouseEntered
    // gets swallowed by the gate and never re-fires (already inside), so side-button
    // summons would never hover-select if we only relied on mouseEntered (verified).
    MOUSE_MOVED.store(true, Ordering::Relaxed);
    unsafe {
        let document = match card_document() {
            Some(document) => document,
            None => return,
        };
        // 交给 AppKit 转换,自动包含 clip bounds 的滚动偏移,不再手工减 container frame。
        // Let AppKit convert the point so clip bounds scrolling is included automatically.
        let document_point: NSPoint = msg_send![
            document,
            convertPoint: loc,
            fromView: std::ptr::null::<AnyObject>()
        ];
        for sv in card_views(document) {
            let frame: NSRect = msg_send![sv, frame];
            let inside = document_point.x >= frame.origin.x
                && document_point.x <= frame.origin.x + frame.size.width
                && document_point.y >= frame.origin.y
                && document_point.y <= frame.origin.y + frame.size.height;
            if !inside {
                continue;
            }
            let Some(idx) = get_card_index(sv) else {
                continue;
            };
            let mut state_opt = TAB_STATE.lock().unwrap();
            if let Some(state) = state_opt.as_mut() {
                if state.selected != idx {
                    log_debug!("[overlay] mm select {} -> {}", state.selected, idx);
                    state.selected = idx;
                    drop(state_opt);
                    reset_thumbnail_nav_anchor();
                    refresh_highlight();
                    update_status_label();
                }
            }
            break;
        }
    }
}

/// 上次上报的鼠标屏幕坐标(hover 轮询 tick 用):位置未变化不重复命中(保持"移动才选中"
/// 的门控语义,浮窗打开瞬间鼠标下的卡片不会被误选)。
/// Last reported cursor screen point (hover poll tick): unchanged positions don't re-run the
/// hit test (keeping the "select only after a move" gate, so the card under the cursor at
/// summon time isn't auto-selected).
pub(super) static HOVER_TICK_POS: Mutex<Option<(f64, f64)>> = Mutex::new(None);

/// hover 轮询定时器(主线程 runloop)。浮窗显示期间每 16ms 读一次 NSEvent.mouseLocation
/// 命中卡片——不依赖任何事件投递(侧键按住期间移动事件是 OtherMouseDragged 且投递给
/// 非浮窗目标,所有 tap/tracking 方案都收不到,实测;轮询直接查全局鼠标位置,与按钮
/// 状态、事件流完全无关)。
/// Hover poll timer (main-thread runloop). While the overlay is shown, NSEvent.mouseLocation
/// is read every 16ms to hit-test the cards -- independent of event delivery (moves while a
/// side button is held are OtherMouseDragged delivered to a non-overlay target; every
/// tap/tracking approach failed to see them, verified; polling reads the global cursor
/// directly, unrelated to button state or event routing).
// Send+Sync 包装(与 device.rs 的 ManagerMutex 同模式):static Mutex 需要 Send+Sync。
// Send+Sync wrapper (same pattern as device.rs's ManagerMutex): statics need Send+Sync.
pub(super) struct TimerMutex(Mutex<Option<event_tap::CFRunLoopTimerRef>>);
unsafe impl Send for TimerMutex {}
unsafe impl Sync for TimerMutex {}

pub(super) static HOVER_TIMER: TimerMutex = TimerMutex(Mutex::new(None));

/// hover 轮询 tick(主线程):读全局鼠标位置,位置变化时按卡片选中。
/// Hover poll tick (main thread): reads the global cursor; on movement, selects the card.
pub(super) unsafe extern "C" fn hover_tick_callback(
    _timer: event_tap::CFRunLoopTimerRef,
    _info: *mut c_void,
) {
    // 用 CGEventCreate 读当前鼠标位置:不依赖 mouseMoved 事件流(侧键按住期间系统
    // 不产生 mouseMoved,NSEvent.mouseLocation 冻结;CGEventCreate 直接查系统状态)。
    // Read the cursor via CGEventCreate: independent of the mouseMoved stream (while a side
    // button is held the system emits no mouseMoved, freezing NSEvent.mouseLocation;
    // CGEventCreate queries the system state directly).
    let ev = event_tap::CGEventCreate(std::ptr::null_mut());
    if ev.is_null() {
        return;
    }
    let pos = event_tap::CGEventGetLocation(ev);
    CFRelease(ev as *const c_void);
    // CGEventGetLocation 是 CG 坐标系(主屏左上原点),浮窗 frame 是 AppKit 坐标系
    // (主屏左下原点)——y 轴必须翻转,否则鼠标在上半屏时命中的是下半屏对称位置的
    // 卡片(实测错位)。
    // CGEventGetLocation uses the CG coordinate space (main-display top-left origin),
    // while window frames use the AppKit space (main-display bottom-left origin) -- the
    // y axis must be flipped, or the cursor in the upper half hits the mirrored card in
    // the lower half (verified misalignment).
    let main_h: f64 = {
        let main: *mut AnyObject = msg_send![class!(NSScreen), mainScreen];
        let mf: NSRect = msg_send![main, frame];
        mf.size.height
    };
    let pos = NSPoint::new(pos.x, main_h - pos.y);
    let container = match *CONTAINER.lock().unwrap() {
        Some(c) => c.0,
        None => return,
    };
    let win: *mut AnyObject = msg_send![container, window];
    let win_frame: NSRect = msg_send![win, frame];
    let loc = NSPoint::new(pos.x - win_frame.origin.x, pos.y - win_frame.origin.y);
    update_thumbnail_pointer_state(loc);
    let mut last = HOVER_TICK_POS.lock().unwrap();
    match *last {
        // 首次 tick:last 还没基准,只记录位置不选中(保持"移动后才选中"的门控
        // 语义,浮窗打开瞬间鼠标下的卡片不被误选)。原实现用 map_or(pos) 计算 dx,
        // None 时 dx=dy=0 永远被 <4.0 挡掉,last 永不更新、选中永不触发(实测)。
        // First tick: no baseline yet -- record the position without selecting (keeping
        // the "select only after a move" gate; the card under the cursor at summon time
        // isn't auto-selected). The old code computed dx via map_or(pos), which made
        // dx=dy=0 when None, forever tripping the <4.0 check -- the baseline was never
        // stored and selection never fired (verified).
        None => {
            *last = Some((pos.x, pos.y));
            return;
        }
        Some((px, py)) => {
            let dx = pos.x - px;
            let dy = pos.y - py;
            if dx * dx + dy * dy < 4.0 {
                return;
            }
            *last = Some((pos.x, pos.y));
        }
    }
    drop(last);
    handle_hover_at(loc);
}

/// 浮窗显示期间启动 hover 轮询定时器(先清旧的)。由 show_overlay 调用。
/// Start the hover poll timer while the overlay is shown (invalidating any stale one first).
/// Called from show_overlay.
pub(crate) fn start_hover_timer() {
    unsafe {
        // 先停掉上次可能残留的定时器,避免重复 tick。
        // Invalidate any stale timer from a previous summon first.
        let old = HOVER_TIMER.0.lock().unwrap().take();
        if let Some(t) = old {
            event_tap::CFRunLoopTimerInvalidate(t);
        }
        let ctx = crate::event_tap::CFRunLoopTimerContext {
            version: 0,
            info: std::ptr::null_mut(),
            retain: None,
            release: None,
            copy_description: None,
        };
        let timer = event_tap::CFRunLoopTimerCreate(
            std::ptr::null_mut(),
            0.0,   // 立即触发一次(浮窗打开时若有移动立即选中)/ fire immediately
            0.032, // 之后每 ~32ms(≈30fps,足够 hover 且不给主线程加负载)/
            // then every ~32ms (~30fps; enough for hover without main-thread load)
            0,
            0,
            Some(hover_tick_callback),
            &ctx as *const crate::event_tap::CFRunLoopTimerContext as *mut c_void,
        );
        if !timer.is_null() {
            event_tap::CFRunLoopAddTimer(
                event_tap::CFRunLoopGetMain(),
                timer,
                event_tap::kCFRunLoopDefaultMode,
            );
            *HOVER_TIMER.0.lock().unwrap() = Some(timer);
        }
    }
}

/// 浮窗消失时停止 hover 轮询。由 vanish_overlay / hide_overlay 调用。
/// Stop the hover poll when the overlay disappears. Called from vanish_overlay / hide_overlay.
pub(crate) fn stop_hover_timer() {
    unsafe {
        let old = HOVER_TIMER.0.lock().unwrap().take();
        if let Some(t) = old {
            event_tap::CFRunLoopTimerInvalidate(t);
        }
    }
}

pub(crate) extern "C" fn container_mouse_moved(_self: *mut c_void, _cmd: Sel, _event: *mut c_void) {
    // locationInWindow 相对事件源窗口 —— nonactivating 面板下 mouseMoved 事件可能
    // 挂在下方 app 的窗口上,坐标不可靠(实测 loc 不随鼠标移动变化)。改用屏幕坐标
    // (NSEvent.mouseLocation,左下原点)转浮窗窗口坐标,与卡片 frame 同基准。
    // locationInWindow is relative to the event's source window -- with the
    // nonactivating panel the mouseMoved event may attach to the app window below, so
    // the coordinate is unreliable (verified: loc never changed while moving). Use the
    // screen coordinate (NSEvent.mouseLocation, bottom-left origin) converted into the
    // overlay's window space, the same base as the card frames.
    unsafe {
        let mouse_screen: NSPoint = msg_send![class!(NSEvent), mouseLocation];
        let container = match *CONTAINER.lock().unwrap() {
            Some(c) => c.0,
            None => return,
        };
        let win: *mut AnyObject = msg_send![container, window];
        let win_frame: NSRect = msg_send![win, frame];
        let loc = NSPoint::new(
            mouse_screen.x - win_frame.origin.x,
            mouse_screen.y - win_frame.origin.y,
        );
        update_thumbnail_pointer_state(loc);
        handle_hover_at(loc);
    }
}

pub(super) fn schedule_deferred_scroll_hover() {
    unsafe {
        let Some(controller) = *crate::CONTROLLER.lock().unwrap() else {
            return;
        };
        let selector = sel!(handleDeferredScrollHover:);
        let _: () = msg_send![
            class!(NSObject),
            cancelPreviousPerformRequestsWithTarget: controller.0,
            selector: selector,
            object: std::ptr::null::<AnyObject>()
        ];
        let _: () = msg_send![
            controller.0,
            performSelector: selector,
            withObject: std::ptr::null::<AnyObject>(),
            afterDelay: 0.05f64
        ];
    }
}

pub(crate) extern "C" fn on_deferred_scroll_hover(
    _self: *mut c_void,
    _cmd: Sel,
    _arg: *mut c_void,
) {
    let visible = TAB_STATE
        .lock()
        .unwrap()
        .as_ref()
        .is_some_and(|state| state.visible);
    if visible {
        container_mouse_moved(
            std::ptr::null_mut(),
            sel!(mouseMoved:),
            std::ptr::null_mut(),
        );
    }
}
