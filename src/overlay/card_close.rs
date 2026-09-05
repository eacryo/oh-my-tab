//! 切换器浮窗 · 卡片关闭管线:关闭按钮类、关闭动画/补位重排、异步 AX 关闭与提交。
//! Card-close pipeline: close button class, close animation/reflow, async AX close, and commit.

use super::*;

pub(super) unsafe fn card_views_by_key(
    _windows: &[WindowInfo],
) -> HashMap<WindowKey, *mut AnyObject> {
    let Some(document) = card_document() else {
        return HashMap::new();
    };
    card_views(document)
        .into_iter()
        .filter_map(|card| card_key(card).map(|key| (key, card)))
        .collect()
}

/// 在一个 AppKit 动画事务中让关闭卡片横向收窄,并让其余卡片直接移动到新槽位。
/// In one AppKit animation transaction, collapse the closing card horizontally while moving
/// every surviving card directly into its new slot.
pub(super) unsafe fn animate_card_close_reflow(
    pending: &PendingCardClose,
    views: &HashMap<WindowKey, *mut AnyObject>,
) {
    let _: () = msg_send![class!(NSAnimationContext), beginGrouping];
    let context: *mut AnyObject = msg_send![class!(NSAnimationContext), currentContext];
    let _: () = msg_send![context, setDuration: CARD_CLOSE_ANIMATION_DURATION];
    let timing_name = make_nsstring("easeInEaseOut");
    let timing: *mut AnyObject =
        msg_send![class!(CAMediaTimingFunction), functionWithName: timing_name];
    if !timing.is_null() {
        let _: () = msg_send![context, setTimingFunction: timing];
    }
    CFRelease(timing_name as *const c_void);

    for (&key, &card) in views {
        let Some(original) = pending.original_frames.get(&key) else {
            continue;
        };
        let animator: *mut AnyObject = msg_send![card, animator];
        if key == (pending.pid, pending.cgwid) {
            // 用 frame 宽度收窄,而不是 transform.scale;这样后面的卡片可以无缝填入空出的槽位。
            // Collapse the frame width instead of using transform.scale, so following cards can
            // occupy the released slot without a visual gap.
            let layer: *mut AnyObject = msg_send![card, layer];
            if !layer.is_null() {
                let _: () = msg_send![layer, removeAllAnimations];
                let _: () = msg_send![layer, setMasksToBounds: true];
            }
            let collapsed = NSRect::new(original.origin, NSSize::new(1.0, original.size.height));
            let _: () = msg_send![animator, setFrame: collapsed];
            let _: () = msg_send![animator, setAlphaValue: 0.0f64];
        } else if let Some(final_frame) = pending.final_frames.get(&key) {
            let _: () = msg_send![animator, setFrame: *final_frame];
        }
    }
    if let Some(window) = *OVERLAY_WINDOW.lock().unwrap() {
        let animator: *mut AnyObject = msg_send![window.0, animator];
        let _: () = msg_send![animator, setFrame: pending.final_panel_frame, display: true];
    }
    let _: () = msg_send![class!(NSAnimationContext), endGrouping];
}

/// AX 关闭失败时反向播放同一组 frame 动画,让卡片回到关闭前的位置。
/// If AX rejects the close, reverse the same frame animation to restore every card.
pub(super) unsafe fn restore_card_close_reflow(pending: &PendingCardClose) {
    let views = card_views_by_key(
        &TAB_STATE
            .lock()
            .unwrap()
            .as_ref()
            .map(|state| state.windows.clone())
            .unwrap_or_default(),
    );
    let _: () = msg_send![class!(NSAnimationContext), beginGrouping];
    let context: *mut AnyObject = msg_send![class!(NSAnimationContext), currentContext];
    let _: () = msg_send![context, setDuration: CARD_CLOSE_ANIMATION_DURATION];
    let timing_name = make_nsstring("easeInEaseOut");
    let timing: *mut AnyObject =
        msg_send![class!(CAMediaTimingFunction), functionWithName: timing_name];
    if !timing.is_null() {
        let _: () = msg_send![context, setTimingFunction: timing];
    }
    CFRelease(timing_name as *const c_void);
    for (&key, &card) in &views {
        if let Some(frame) = pending.original_frames.get(&key) {
            let animator: *mut AnyObject = msg_send![card, animator];
            let _: () = msg_send![animator, setFrame: *frame];
            let _: () = msg_send![animator, setAlphaValue: 1.0f64];
            if key == (pending.pid, pending.cgwid) {
                let layer: *mut AnyObject = msg_send![card, layer];
                if !layer.is_null() {
                    let _: () = msg_send![layer, setMasksToBounds: false];
                }
            }
        }
    }
    let _: () = msg_send![class!(NSAnimationContext), endGrouping];
    refresh_highlight();
}

/// 卡片右上角关闭按钮的 action(sender = 关闭按钮):先播放退出动画,再关闭窗口。
/// Action of the card's top-right close button: animate the card first, then close the window.
pub(crate) extern "C" fn on_close_card(_self: *mut c_void, _cmd: Sel, sender: *mut c_void) {
    let card: *mut AnyObject = unsafe { msg_send![sender as *mut AnyObject, superview] };
    if card.is_null() {
        return;
    }
    let Some(idx) = get_card_index(card) else {
        return;
    };
    begin_close_window_at(idx, card);
}

/// 关闭动画是否正在进行;窗口刷新和缩略图回调在此期间必须暂缓结构性更新。
/// Whether a close transition is active; structural refreshes must wait until it commits.
pub(crate) fn card_close_in_progress() -> bool {
    PENDING_CARD_CLOSE.lock().unwrap().is_some()
}

/// 开始卡片收窄与补位动画;真正的 AX 关闭在后台线程执行。
/// Start the slot-collapse/reflow animation; the actual AX close runs on a worker thread.
pub(crate) fn begin_close_window_at(idx: usize, card: *mut AnyObject) {
    if card_close_in_progress() {
        return;
    }
    let (pending, views) = {
        let state_opt = TAB_STATE.lock().unwrap();
        let Some(state) = state_opt.as_ref() else {
            return;
        };
        if !state.visible {
            return;
        }
        let Some(window) = state.windows.get(idx) else {
            return;
        };
        let key = (window.pid, window.window_id);
        let views = unsafe { card_views_by_key(&state.windows) };
        if views.get(&key).copied() != Some(card) {
            return;
        }
        let original_frames: HashMap<WindowKey, NSRect> = views
            .values()
            .map(|&view| {
                let frame: NSRect = unsafe { msg_send![view, frame] };
                let key = state
                    .windows
                    .iter()
                    .find_map(|window| {
                        let candidate = (window.pid, window.window_id);
                        (views.get(&candidate).copied() == Some(view)).then_some(candidate)
                    })
                    .unwrap();
                (key, frame)
            })
            .collect();
        let panel_frame = unsafe {
            OVERLAY_WINDOW
                .lock()
                .unwrap()
                .map(|window| msg_send![window.0, frame])
                .unwrap_or(NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(1.0, 1.0)))
        };
        let panel_w = panel_frame.size.width;
        let overflowed = THUMB_ROW_RANGES
            .lock()
            .unwrap()
            .as_ref()
            .is_some_and(|rows| rows.len() > *THUMB_MAX_ROWS.lock().unwrap());
        let scrollbar_w = if overflowed { THUMB_SCROLLBAR_W } else { 0.0 };
        let card_area_w = (panel_w - scrollbar_w).max(1.0);
        let max_inner = (card_area_w - H_PADDING * 2.0).max(1.0);
        let card_h = original_frames
            .get(&key)
            .map(|frame| frame.size.height)
            .unwrap_or(1.0);
        let gap = if crate::theme::thumbnails_enabled() {
            THUMB_ROW_GAP
        } else {
            ICON_CARD_GAP
        };
        let mut widths = Vec::with_capacity(state.windows.len().saturating_sub(1));
        let mut survivor_keys = Vec::with_capacity(state.windows.len().saturating_sub(1));
        for candidate in &state.windows {
            let candidate_key = (candidate.pid, candidate.window_id);
            if candidate_key == key {
                continue;
            }
            let Some(frame) = original_frames.get(&candidate_key) else {
                return;
            };
            survivor_keys.push(candidate_key);
            widths.push(frame.size.width);
        }
        let document_h = (*THUMB_DOCUMENT_HEIGHT.lock().unwrap()).max(1.0);
        let max_rows = (*THUMB_MAX_ROWS.lock().unwrap()).max(1);
        let (placements, final_row_ranges, final_panel_w, final_overflowed) =
            plan_thumb_close_reflow(
                &widths, card_h, max_inner, gap, document_h, overflowed, max_rows,
            );
        let viewport_h = unsafe {
            CONTAINER
                .lock()
                .unwrap()
                .map(|container| {
                    let bounds: NSRect = msg_send![container.0, bounds];
                    bounds.size.height
                })
                .unwrap_or(1.0)
        };
        let content_h = thumb_document_height_for_rows(final_row_ranges.len(), card_h, gap);
        let final_document_h = content_h.max(viewport_h).max(1.0);
        let final_frames = placements
            .into_iter()
            .filter_map(|placement| {
                let key = *survivor_keys.get(placement.index)?;
                Some((
                    key,
                    NSRect::new(
                        NSPoint::new(placement.x, placement.y),
                        NSSize::new(placement.width, card_h),
                    ),
                ))
            })
            .collect();
        let final_panel_frame = NSRect::new(
            NSPoint::new(
                panel_frame.origin.x + (panel_frame.size.width - final_panel_w) / 2.0,
                panel_frame.origin.y,
            ),
            NSSize::new(final_panel_w, panel_frame.size.height),
        );
        (
            PendingCardClose {
                pid: window.pid,
                cgwid: window.window_id,
                animation_finished: false,
                ax_result: None,
                original_frames,
                final_frames,
                final_row_ranges,
                final_panel_frame,
                final_overflowed,
                original_document_h: document_h,
                final_document_h,
            },
            views,
        )
    };
    let close_key = (pending.pid, pending.cgwid);
    {
        let mut closing = PENDING_CARD_CLOSE.lock().unwrap();
        if closing.is_some() {
            return;
        }
        *closing = Some(pending);
    }

    unsafe {
        let pending_ref = PENDING_CARD_CLOSE.lock().unwrap();
        if let Some(pending) = pending_ref.as_ref() {
            animate_card_close_reflow(pending, &views);
        }
        drop(pending_ref);
        start_async_ax_close(close_key);
        let Some(controller) = *crate::CONTROLLER.lock().unwrap() else {
            *PENDING_CARD_CLOSE.lock().unwrap() = None;
            return;
        };
        let _: () = msg_send![
            controller.0,
            performSelector: sel!(handleCardCloseFinished:),
            withObject: std::ptr::null::<AnyObject>(),
            afterDelay: CARD_CLOSE_ANIMATION_DURATION
        ];
    }
}

/// 将 AX 关闭放到后台线程,不让可见的卡片动画等待 AX 查询和消息超时。
/// Run the AX close off the main thread so visible animation frames never wait on AX queries.
pub(super) fn start_async_ax_close(key: WindowKey) {
    // The settings window belongs to this process. Do not invoke its AX close action from the
    // worker thread: the custom close callback performs AppKit work and must stay on the main
    // thread. The pending card-close animation will consume this successful result normally.
    //
    // 本进程的设置窗口不能在 worker 线程执行 AXPress:自定义关闭回调包含 AppKit 操作,必须
    // 留在主线程。这里直接关闭设置窗口,后续仍由原有动画流程消费成功结果。
    if key.0 == std::process::id() as i32 {
        crate::settings::close_settings_from_switcher();
        let mut closing = PENDING_CARD_CLOSE.lock().unwrap();
        if let Some(current) = closing
            .as_mut()
            .filter(|current| (current.pid, current.cgwid) == key)
        {
            current.ax_result = Some(true);
        }
        return;
    }
    std::thread::spawn(move || {
        let result = crate::window_collector::close_ax_window(key.0, key.1);
        {
            let mut closing = PENDING_CARD_CLOSE.lock().unwrap();
            if let Some(current) = closing
                .as_mut()
                .filter(|current| (current.pid, current.cgwid) == key)
            {
                current.ax_result = Some(result);
            } else {
                return;
            }
        }
        unsafe {
            let Some(controller) = *crate::CONTROLLER.lock().unwrap() else {
                return;
            };
            let _: () = msg_send![
                controller.0,
                performSelectorOnMainThread: sel!(handleCardCloseAXResult:),
                withObject: std::ptr::null::<AnyObject>(),
                waitUntilDone: false
            ];
        }
    });
}

/// 动画与 AX 结果都完成后,按稳定窗口身份提交列表更新和重排。
/// Commit the list update and reflow only after both the animation and AX result are ready.
pub(super) fn finish_pending_card_close() {
    let pending = {
        let mut closing = PENDING_CARD_CLOSE.lock().unwrap();
        let Some(current) = closing.as_ref() else {
            return;
        };
        if !current.animation_finished || current.ax_result.is_none() {
            return;
        }
        closing.take().unwrap()
    };
    if !pending.ax_result.unwrap_or(false) {
        unsafe {
            restore_card_close_reflow(&pending);
        }
        return;
    }
    commit_pending_card_close(pending);
}

/// 退出动画结束回调;AX 可能已完成,也可能仍在后台执行。
/// Exit-animation completion callback; AX may already be done or still be running in the worker.
pub(crate) extern "C" fn on_card_close_finished(_self: *mut c_void, _cmd: Sel, _arg: *mut c_void) {
    if let Some(pending) = PENDING_CARD_CLOSE.lock().unwrap().as_mut() {
        pending.animation_finished = true;
    }
    finish_pending_card_close();
}

/// AX 关闭后台结果回调;与动画回调汇合后再触发 UI 重排。
/// AX worker result callback; joins the animation callback before triggering UI reflow.
pub(crate) extern "C" fn on_card_close_ax_result(_self: *mut c_void, _cmd: Sel, _arg: *mut c_void) {
    finish_pending_card_close();
}

/// 提交关闭结果时只移除一张 view,其余 view 保持不变并重新绑定新索引。
/// Commit a successful close by removing one view only; surviving views are reused and rebound
/// to their new indices.
pub(super) fn commit_pending_card_close(pending: PendingCardClose) {
    let key = (pending.pid, pending.cgwid);
    let (old_windows, new_windows, selected, was_visible, became_empty) = {
        let mut state_opt = TAB_STATE.lock().unwrap();
        let Some(state) = state_opt.as_mut() else {
            return;
        };
        let Some(actual_idx) = state
            .windows
            .iter()
            .position(|window| (window.pid, window.window_id) == key)
        else {
            return;
        };
        let old_windows = state.windows.clone();
        let was_visible = state.visible;
        state.windows.remove(actual_idx);
        state.mru.remove(&key);
        state.selected =
            remove_window_adjust_selection(state.selected, actual_idx, state.windows.len());
        state.selected_target_key = state
            .windows
            .get(state.selected)
            .map(|window| (window.pid, window.window_id));
        let became_empty = state.windows.is_empty();
        if became_empty {
            state.visible = false;
        }
        (
            old_windows,
            state.windows.clone(),
            state.selected,
            was_visible,
            became_empty,
        )
    };

    let views = unsafe { card_views_by_key(&old_windows) };
    // 提交时把卡片与 document 一起平移;二者使用同一个 delta,所以用户看到的内容不会跳变。
    // Rebase cards and the document together at commit; sharing one delta keeps visible content
    // stationary instead of making the page jump while the scrollbar stays at its old position.
    let old_offset = *THUMB_SCROLL_OFFSET.lock().unwrap();
    let viewport_h = unsafe {
        CONTAINER
            .lock()
            .unwrap()
            .map(|container| {
                let bounds: NSRect = msg_send![container.0, bounds];
                bounds.size.height
            })
            .unwrap_or(1.0)
    };
    let (document_h, max_offset, rebased_offset, document_delta) =
        rebase_thumb_scroll_after_document_resize(
            pending.original_document_h,
            pending.final_document_h,
            viewport_h,
            old_offset,
        );

    unsafe {
        if let Some(window) = *OVERLAY_WINDOW.lock().unwrap() {
            let _: () = msg_send![window.0, setFrame: pending.final_panel_frame, display: false];
        }
        if let Some(container) = *CONTAINER.lock().unwrap() {
            let frame: NSRect = msg_send![container.0, frame];
            let _: () = msg_send![
                container.0,
                setFrame: NSRect::new(
                    frame.origin,
                    NSSize::new(pending.final_panel_frame.size.width, frame.size.height)
                )
            ];
        }
        if let Some(closing_card) = views.get(&key).copied() {
            remove_card_index(closing_card);
            let _: () = msg_send![closing_card, removeFromSuperview];
        }
        for (index, window) in new_windows.iter().enumerate() {
            let survivor_key = (window.pid, window.window_id);
            if let Some(card) = views.get(&survivor_key).copied() {
                set_card_index(card, index);
                if let Some(frame) = pending.final_frames.get(&survivor_key) {
                    let rebased_frame = NSRect::new(
                        NSPoint::new(frame.origin.x, frame.origin.y + document_delta),
                        frame.size,
                    );
                    let _: () = msg_send![card, setFrame: rebased_frame];
                }
            }
        }

        if let Some(document) = card_document() {
            let frame: NSRect = msg_send![document, frame];
            let _: () = msg_send![
                document,
                setFrame: NSRect::new(
                    frame.origin,
                    NSSize::new(pending.final_panel_frame.size.width, document_h)
                )
            ];
        }
        *THUMB_DOCUMENT_HEIGHT.lock().unwrap() = document_h;
    }

    if became_empty {
        if was_visible {
            hide_overlay();
        }
        reset_thumbnail_visible_range();
        reset_thumbnail_scroll();
        reset_thumbnail_nav_anchor();
        return;
    }
    if !was_visible {
        return;
    }

    // 关闭动画期间同步缩放面板;提交时原子同步 document 和滚动元数据,避免跳变。
    // Animate the panel resize together with the close transition; atomically sync the document
    // and scroll metadata at commit so the content cannot jump independently of the scrollbar.
    let max_rows = (*THUMB_MAX_ROWS.lock().unwrap()).max(1);
    let row_count = pending.final_row_ranges.len();
    *THUMB_ROW_RANGES.lock().unwrap() = Some(pending.final_row_ranges);
    *THUMB_SCROLL_MAX_OFFSET.lock().unwrap() = max_offset;
    let offset = {
        let mut offset = THUMB_SCROLL_OFFSET.lock().unwrap();
        *offset = rebased_offset;
        *offset
    };
    update_thumbnail_scroll_state(offset);
    unsafe {
        apply_thumbnail_clip_offset();
        if let Some(window) = *OVERLAY_WINDOW.lock().unwrap() {
            let frame: NSRect = msg_send![window.0, frame];
            update_thumbnail_scroller(
                frame.size.width,
                frame.size.height,
                pending.final_overflowed,
                row_count,
                max_rows,
            );
        }
    }
    refresh_highlight();
    update_status_label();
    log_debug!(
        "[overlay] close reflow committed: pid={} cgwid={} remaining={} selected={}",
        pending.pid,
        pending.cgwid,
        new_windows.len(),
        selected
    );
}

pub(crate) extern "C" fn on_cmd_released(_self: *mut c_void, _cmd: Sel, _arg: *mut c_void) {
    if card_close_in_progress() {
        return;
    }
    {
        let mut state_opt = TAB_STATE.lock().unwrap();
        let Some(state) = state_opt.as_mut() else {
            return;
        };
        if !state.visible {
            if state.pending_first_show {
                // The bridge delivered CmdReleased before the AX-backed first frame was ready.
                // Latch it so apply_window_refresh can commit the eventual default target.
                state.pending_first_release = true;
                log_debug!("[overlay] CmdReleased latched while first snapshot is pending");
            }
            return;
        }
    }

    commit_selected_window(true);
}

pub(super) fn commit_selected_window(overlay_was_visible: bool) {
    let mut state_opt = TAB_STATE.lock().unwrap();
    let state = state_opt.as_mut().unwrap();
    if !state.visible {
        return;
    }
    if let Some(w) = state.windows.get(state.selected) {
        let pid = w.pid;
        let cgwid = w.window_id;
        let minimized = w.minimized;
        let release_started = Instant::now();
        log_debug!(
            "Switching to '{}' (pid={} cgwid={})",
            w.app_name,
            pid,
            cgwid
        );
        // 先视觉隐藏(不 orderOut),再激活目标窗口,最后延迟 orderOut。
        // 先 orderOut 会干扰 WindowServer 焦点路由,导致目标窗口的 first-responder 未确立
        // (光标停止闪烁等)。对齐 BetterCmdTab 的 vanish() -> activate() -> dismiss() 时序。
        // Vanish first (no orderOut), then activate the target, then delay orderOut.
        // Ordering out first disrupts WindowServer focus routing, leaving the target's
        // first-responder unset (caret stops blinking, etc.). Mirrors BetterCmdTab's
        // vanish() -> activate() -> dismiss() sequence.
        if overlay_was_visible {
            vanish_overlay();
        }
        // 设置窗口无需特殊处理:浮窗是 nonactivating 面板,召唤时 app 未激活,设置窗口
        // 从未被抬升(从别的 App 召唤时被其窗口盖住;从设置召唤时透过玻璃可见),切走后
        // 留在原位。与 BetterCmdTab 行为一致,无 stash/restore 机制。
        // No settings-window handling needed: the overlay is a nonactivating panel, so the app
        // stays inactive during summon and the settings window is never raised (covered by the
        // active app's windows when summoning from elsewhere; visible through the glass when
        // summoning from settings). It stays put after the switch. Matches BetterCmdTab --
        // no stash/restore machinery.
        // 快速抬升只包含毫秒级的 WindowServer 调用,在当前释放回调中立即执行,避免原生
        // Cmd+Tab 不需要的额外 RunLoop turn;耗时不稳定的 AX 兜底仍由 activate_and_raise
        // 异步提交。vanish 已经把面板设为透明且 resignKey,不会再阻塞目标窗口拿焦点。
        // The fast raise only contains millisecond-scale WindowServer calls, so run it in this
        // release callback instead of paying for an extra RunLoop turn that native Cmd+Tab does
        // not need; activate_and_raise still submits the variable-latency AX backstop async.
        // vanish has already made the panel transparent and resigned key, so it cannot block the
        // target from taking focus.
        activate_and_raise(pid, cgwid, minimized);
        log_debug!(
            "[raise] release path complete: pid={} cgwid={} elapsed={}ms",
            pid,
            cgwid,
            release_started.elapsed().as_millis()
        );
        if overlay_was_visible {
            schedule_delayed_order_out();
        }
        state.focus_key = Some((pid, cgwid));
        bump_window_mru(&mut state.mru, pid, cgwid);
        log_debug!(
            "commit: pid={} app=\"{}\" cgwid={} selected={}",
            pid,
            w.app_name,
            cgwid,
            state.selected
        );
    } else {
        // 空窗口/选中越界:没有可切换的目标,直接收起浮窗(否则会停留在桌面上)。
        // Empty list / out-of-range selection: no switchable target, dismiss the overlay
        // (otherwise it would stay stuck on the desktop).
        log_info!(
            "CmdReleased: selected index {} out of bounds (windows={})",
            state.selected,
            state.windows.len()
        );
        if overlay_was_visible {
            hide_overlay();
        }
    }
    state.visible = false;
    crate::performance::end_switcher_activity();
}

// --- Card View ---

/// 设置关闭按钮的基础/悬停颜色与背景。
/// Apply the close button's base or hover tint and background.
pub(super) unsafe fn set_close_button_hover_style(button: *mut AnyObject, hovered: bool) {
    let tint = if hovered {
        // HTML .close:hover: rgba(195, 40, 35, .86)
        hex_to_ns_color(0xC32823DB)
    } else {
        // HTML .close: rgba(0, 0, 0, .30)
        hex_to_ns_color(0x0000004D)
    };
    let _: () = msg_send![button, setContentTintColor: tint];

    let layer: *mut AnyObject = msg_send![button, layer];
    if hovered {
        // HTML .close:hover background: rgba(195, 40, 35, .07)
        layer_set_background(layer, hex_to_cg_color(0xC3282312));
    } else {
        layer_set_background(layer, std::ptr::null_mut());
    }
}

/// 关闭按钮的动态 ObjC 子类,用于实现 HTML 参考中的悬停红色反馈。
/// Dynamic ObjC subclass for the close button, providing the HTML reference's red hover feedback.
pub(super) fn close_button_class() -> *mut AnyObject {
    static CLOSE_BUTTON_CLASS: OnceLock<ObjClassPtr> = OnceLock::new();
    CLOSE_BUTTON_CLASS
        .get_or_init(|| unsafe {
            let name = CString::new("OhMyTabCloseButton").unwrap();
            let superclass = class!(NSButton) as *const _ as *mut AnyObject;
            let cls = objc_allocateClassPair(superclass, name.as_ptr(), 0);
            let types_v_obj = CString::new("v@:@").unwrap();
            class_addMethod(
                cls,
                sel!(mouseEntered:),
                close_button_mouse_entered as *mut c_void,
                types_v_obj.as_ptr(),
            );
            class_addMethod(
                cls,
                sel!(mouseExited:),
                close_button_mouse_exited as *mut c_void,
                types_v_obj.as_ptr(),
            );
            objc_registerClassPair(cls);
            ObjClassPtr(cls as *const objc2::runtime::AnyClass)
        })
        .0 as *mut AnyObject
}

pub(super) extern "C" fn close_button_mouse_entered(
    _self: *mut c_void,
    _cmd: Sel,
    _event: *mut c_void,
) {
    unsafe {
        set_close_button_hover_style(_self as *mut AnyObject, true);
    }
}

pub(super) extern "C" fn close_button_mouse_exited(
    _self: *mut c_void,
    _cmd: Sel,
    _event: *mut c_void,
) {
    unsafe {
        set_close_button_hover_style(_self as *mut AnyObject, false);
    }
}

pub(crate) extern "C" fn card_mouse_down(_self: *mut c_void, _cmd: Sel, _event: *mut c_void) {
    let Some(idx) = get_card_index(_self as *mut AnyObject) else {
        return;
    };
    let mut state_opt = TAB_STATE.lock().unwrap();
    let state = state_opt.as_mut().unwrap();
    if let Some(w) = state.windows.get(idx) {
        let pid = w.pid;
        let cgwid = w.window_id;
        let minimized = w.minimized;
        vanish_overlay();
        // 同 on_cmd_released:设置窗口无需特殊处理(见该处注释);抬升延迟一拍执行。
        // Same as on_cmd_released: no settings-window handling needed (see comment there);
        // the raise is deferred by one runloop turn so the vanish commits first.
        schedule_deferred_raise(pid, cgwid, minimized);
        schedule_delayed_order_out();
        state.focus_key = Some((pid, cgwid));
        bump_window_mru(&mut state.mru, pid, cgwid);
        state.visible = false;
    } else {
        // 空窗口时无卡片可点,理论上不可达;防御性收起浮窗(与 on_cmd_released 一致)。
        // Unreachable in practice (no cards when the list is empty); defensive dismiss,
        // same as on_cmd_released.
        hide_overlay();
        state.visible = false;
    }
}

pub(crate) extern "C" fn card_mouse_entered(_self: *mut c_void, _cmd: Sel, _event: *mut c_void) {
    // Ignore hover until the user has moved the mouse at least once.
    // Prevents selecting the card under the cursor when the window first opens.
    let Some(idx) = get_card_index(_self as *mut AnyObject) else {
        return;
    };
    if !MOUSE_MOVED.load(Ordering::Relaxed) {
        log_debug!(
            "[overlay] card {} mouseEntered (gated, mouse not moved yet)",
            idx
        );
        return;
    }
    let mut state_opt = TAB_STATE.lock().unwrap();
    let state = state_opt.as_mut().unwrap();
    if state.selected != idx {
        state.selected = idx;
        mark_user_picked(state);
        drop(state_opt);
        reset_thumbnail_nav_anchor();
        refresh_highlight();
        update_status_label();
    } else {
        drop(state_opt);
    }
}
