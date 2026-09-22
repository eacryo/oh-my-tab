//! 剪贴板子系统 · observer:全
//! 程粘贴板通知观察者

use super::*;

// ========== 通知观察者 / notification observer ==========

/// 通知观察者单例,承载两个回调:
/// - NSPasteboardDidChangeNotification:剪贴板每次变化即时记录——轮询只在 0.5s 间隔
///   采样一次"当前值",两次采样间的快速连续复制会被跳过(历史只剩最近一条);
///   通知在每次变化时都回调,事件不丢。
/// - NSWindowDidResignKeyNotification:浮窗失去 key(点击了外部)→ 自动隐藏。
///
/// A singleton notification observer carrying two callbacks:
/// - NSPasteboardDidChangeNotification: record on every pasteboard change. Polling samples
///   the current value once per 0.5s interval, so rapid consecutive copies between samples
///   are skipped (history ends up with only the newest entry); the notification fires on
///   every change, so no event is lost.
/// - NSWindowDidResignKeyNotification: the picker loses key (a click outside) -> hide.
pub(super) unsafe fn observer() -> *mut AnyObject {
    static OBSERVER: OnceLock<CallbackTarget> = OnceLock::new();
    OBSERVER
        .get_or_init(|| {
            let name = CString::new("OhMyTabClipboardObserver").unwrap();
            let superclass = class!(NSObject) as *const _ as *mut AnyObject;
            let cls = objc_allocateClassPair(superclass, name.as_ptr(), 0);
            let types = CString::new("v@:@").unwrap();
            class_addMethod(
                cls,
                sel!(clipboardPasteboardChanged:),
                pasteboard_changed as *mut c_void,
                types.as_ptr(),
            );
            class_addMethod(
                cls,
                sel!(pollClipboardOnMain:),
                clip_poll_on_main as *mut c_void,
                types.as_ptr(),
            );
            class_addMethod(
                cls,
                sel!(clipboardWindowResigned:),
                window_did_resign_key as *mut c_void,
                types.as_ptr(),
            );
            class_addMethod(
                cls,
                sel!(clearSystemPasteboardIfOwned:),
                clear_system_pasteboard_if_owned as *mut c_void,
                types.as_ptr(),
            );
            class_addMethod(
                cls,
                sel!(scrollIndicatorBoundsChanged:),
                scroll_indicator_bounds_changed as *mut c_void,
                types.as_ptr(),
            );
            class_addMethod(
                cls,
                sel!(detailScrollIndicatorBoundsChanged:),
                detail_scroll_indicator_bounds_changed as *mut c_void,
                types.as_ptr(),
            );
            class_addMethod(
                cls,
                sel!(toggleDetailSoftWrap:),
                toggle_detail_soft_wrap as *mut c_void,
                types.as_ptr(),
            );
            class_addMethod(
                cls,
                sel!(detailSaveAs:),
                detail_save_as_action as *mut c_void,
                types.as_ptr(),
            );
            // 另存为的主线程重入点:动作在按钮追踪循环内只存槽 + 跳转,真正弹
            // NSSavePanel 在这里(见 PENDING_SAVE_AS 注释)。
            // Main-thread re-entry for save-as: the action only stashes and hops from
            // inside the button tracking loop; the NSSavePanel is presented here (see
            // the PENDING_SAVE_AS comment).
            class_addMethod(
                cls,
                sel!(detailSaveAsDeferred:),
                detail_save_as_deferred as *mut c_void,
                types.as_ptr(),
            );
            // 详情高清预览生成完成(后台线程 → performSelectorOnMainThread):时效
            // 复核后重建详情面板升级为高清图。
            // Detail hi-res preview finished (worker -> performSelectorOnMainThread):
            // re-validate freshness, then rebuild the detail panel to upgrade to hi-res.
            class_addMethod(
                cls,
                sel!(detailPreviewReady:),
                detail_preview_ready as *mut c_void,
                types.as_ptr(),
            );
            class_addMethod(
                cls,
                sel!(clearClipboardHistory:),
                clear_clipboard_history as *mut c_void,
                types.as_ptr(),
            );
            class_addMethod(
                cls,
                sel!(clearClipboardUnpinned:),
                clear_clipboard_unpinned as *mut c_void,
                types.as_ptr(),
            );
            class_addMethod(
                cls,
                sel!(clearClipboardAll:),
                clear_clipboard_all as *mut c_void,
                types.as_ptr(),
            );
            class_addMethod(
                cls,
                sel!(finishClearHistoryCollapse:),
                finish_clear_history_collapse as *mut c_void,
                types.as_ptr(),
            );
            class_addMethod(
                cls,
                sel!(expireClipboardUndo:),
                expire_clipboard_undo as *mut c_void,
                types.as_ptr(),
            );
            class_addMethod(
                cls,
                sel!(refreshPickerRows:),
                picker_refresh_rows as *mut c_void,
                types.as_ptr(),
            );
            class_addMethod(
                cls,
                sel!(refreshPickerSearchRows:),
                picker_refresh_search_rows as *mut c_void,
                types.as_ptr(),
            );
            class_addMethod(
                cls,
                sel!(refreshPickerVisibleRows:),
                picker_refresh_visible_rows as *mut c_void,
                types.as_ptr(),
            );
            class_addMethod(
                cls,
                sel!(searchFieldChanged:),
                search_field_changed as *mut c_void,
                types.as_ptr(),
            );
            class_addMethod(
                cls,
                sel!(filterPillClicked:),
                filter_pill_clicked as *mut c_void,
                types.as_ptr(),
            );
            // 详情文本光标(owner = observer 的 tracking area 投递):进入 = I-beam,
            // 离开 = 箭头。见 detail_tv_cursor_entered 注释。
            // The detail-text cursor (delivered by the tracking area owned by this
            // observer): enter -> I-beam, exit -> arrow. See detail_tv_cursor_entered.
            class_addMethod(
                cls,
                sel!(mouseEntered:),
                detail_tv_cursor_entered as *mut c_void,
                types.as_ptr(),
            );
            class_addMethod(
                cls,
                sel!(mouseExited:),
                detail_tv_cursor_exited as *mut c_void,
                types.as_ptr(),
            );
            class_addMethod(
                cls,
                sel!(searchFocusBegan:),
                search_focus_began as *mut c_void,
                types.as_ptr(),
            );
            class_addMethod(
                cls,
                sel!(searchFocusEnded:),
                search_focus_ended as *mut c_void,
                types.as_ptr(),
            );
            // 搜索框 delegate:拦截字段编辑器翻译出的命令(如 ↓ → moveDown:)。
            // Search-field delegate: intercepts commands the field editor translates
            // (e.g. ↓ -> moveDown:).
            let types_cmd = CString::new("B@:@@:").unwrap();
            class_addMethod(
                cls,
                sel!(control:textView:doCommandBySelector:),
                search_field_do_command as *mut c_void,
                types_cmd.as_ptr(),
            );
            objc_registerClassPair(cls);
            // 实例 alloc(+1):进程级单例,不释放(与静态生命周期一致)。
            // Instance alloc (+1): process-level singleton, never released (matches the
            // static's lifetime).
            let obj: *mut AnyObject = msg_send![cls as *const AnyObject, new];
            CallbackTarget::new(obj)
        })
        .0
}

/// 在主线程维护长驻的列表视图;剪贴板监听线程只排队一次刷新。
/// Keep the long-lived row view tree current on the main thread; the pasteboard observer only
/// queues one coalesced refresh.
extern "C" fn picker_refresh_rows(_self: *mut c_void, _cmd: Sel, _arg: *mut c_void) {
    PICKER_REFRESH_PENDING.store(false, Ordering::SeqCst);
    if PICKER_WINDOW.lock().unwrap().is_none() {
        return;
    }
    // 面板隐藏时只保留模型变化;延迟到下一次显示前统一刷新,避免剪贴板监听占用主线程
    // 并拖慢应用切换浮窗。
    // While hidden, keep only the model change and refresh before the next presentation so the
    // pasteboard observer cannot occupy the main thread and slow the app switcher.
    if !PICKER_VISIBLE.load(Ordering::SeqCst) {
        with_clipboard_ui(|ui| ui.rendered_rows = None);
        return;
    }

    let filter = *CLIP_FILTER.lock().unwrap();
    let show_source = show_source_app();
    let query = with_clipboard_ui(|ui| ui.search_query.clone());
    let key = picker_rows_key(history_revision(), &query, filter, show_source);
    let rows_current = with_clipboard_ui(|ui| {
        ui.rendered_rows
            .as_ref()
            .is_some_and(|current| current == &key)
    });
    if rows_current {
        return;
    }

    unsafe {
        rebuild_rows();
    }
}

/// 搜索输入合并后的主线程刷新:连续按键只在停顿后重建一次可视行。
/// Main-thread refresh after coalescing search input: consecutive keystrokes rebuild the
/// visible rows only once after typing pauses.
extern "C" fn picker_refresh_search_rows(_self: *mut c_void, _cmd: Sel, _arg: *mut c_void) {
    PICKER_SEARCH_REFRESH_PENDING.store(false, Ordering::SeqCst);
    if !PICKER_VISIBLE.load(Ordering::SeqCst)
        || REBUILDING.load(Ordering::SeqCst)
        || PICKER_WINDOW.lock().unwrap().is_none()
    {
        return;
    }
    let filter = *CLIP_FILTER.lock().unwrap();
    let show_source = show_source_app();
    let query = with_clipboard_ui(|ui| ui.search_query.clone());
    let key = picker_rows_key(history_revision(), &query, filter, show_source);
    let rows_current = with_clipboard_ui(|ui| {
        ui.rendered_rows
            .as_ref()
            .is_some_and(|current| current == &key)
    });
    if !rows_current {
        unsafe {
            rebuild_rows();
        }
    }
}

/// 合并连续搜索通知,让文本编辑器保持流畅,同时在短暂停顿后更新列表。
/// Coalesce consecutive search notifications so the editor stays responsive while the list
/// catches up shortly after typing pauses.
unsafe fn schedule_picker_search_refresh() {
    if PICKER_SEARCH_REFRESH_PENDING.swap(true, Ordering::SeqCst) {
        return;
    }
    let target = observer();
    let _: () = msg_send![
        target,
        performSelector: sel!(refreshPickerSearchRows:),
        withObject: std::ptr::null::<AnyObject>(),
        afterDelay: 0.05f64
    ];
}

/// 在滚动事件批次结束后补齐可视行,避免每个 bounds-change 都同步拆建整组控件。
/// Materialize the new viewport after a scroll-event burst instead of tearing down and
/// rebuilding the whole physical row set for every bounds-change callback.
extern "C" fn picker_refresh_visible_rows(_self: *mut c_void, _cmd: Sel, _arg: *mut c_void) {
    PICKER_VISIBLE_ROWS_REFRESH_PENDING.store(false, Ordering::SeqCst);
    if !PICKER_VISIBLE.load(Ordering::SeqCst) || REBUILDING.load(Ordering::SeqCst) {
        return;
    }
    unsafe {
        if picker_materialized_range_changed() {
            sync_visible_rows();
        }
    }
}

/// 合并连续滚动通知,给 AppKit 一个短暂的 run-loop 窗口完成滚动绘制。
/// Coalesce consecutive scroll notifications, giving AppKit a short run-loop window to finish
/// scrolling before the visible row set is rebuilt.
unsafe fn schedule_picker_visible_rows_refresh() {
    if PICKER_VISIBLE_ROWS_REFRESH_PENDING.swap(true, Ordering::SeqCst) {
        return;
    }
    let target = observer();
    let _: () = msg_send![
        target,
        performSelector: sel!(refreshPickerVisibleRows:),
        withObject: std::ptr::null::<AnyObject>(),
        afterDelay: 0.05f64
    ];
}

/// 浮窗可见时把历史变化投递到主线程;隐藏时延迟到下一次呼出前刷新。
/// Deliver history changes to the main thread while the picker is visible; while hidden, defer
/// the refresh until the next summon.
pub(super) fn schedule_picker_refresh() {
    if PICKER_WINDOW.lock().unwrap().is_none()
        || !PICKER_VISIBLE.load(Ordering::SeqCst)
        || PICKER_REFRESH_PENDING.swap(true, Ordering::SeqCst)
    {
        return;
    }
    unsafe {
        let target = observer();
        let _: () = msg_send![
            target,
            performSelectorOnMainThread: sel!(refreshPickerRows:),
            withObject: std::ptr::null::<AnyObject>(),
            waitUntilDone: false
        ];
    }
}

/// 剪贴板变化通知回调(任意线程):即时记录当前文本。
/// Pasteboard-change notification callback (any thread): record the current text immediately.
extern "C" fn pasteboard_changed(_self: *mut c_void, _cmd: Sel, _note: *mut c_void) {
    poll_clipboard();
}

/// 延迟清空一次性粘贴写回的系统剪贴板;若期间 changeCount 或 marker 变化则放弃。
/// Delayed cleanup for a one-shot paste write-back; abort if changeCount or our marker changed.
extern "C" fn clear_system_pasteboard_if_owned(_self: *mut c_void, _cmd: Sel, note: *mut c_void) {
    let Some(note) = (!note.is_null()).then_some(note as *mut AnyObject) else {
        return;
    };
    let scheduled: i64 = unsafe { msg_send![note, longLongValue] };
    // Each delayed selector carries its own changeCount. An older queued callback must not
    // consume the token belonging to a newer one-shot paste.
    // 每个延迟 selector 都携带自己的 changeCount；旧回调不能误消费较新单次粘贴的 token。
    if *PENDING_SYSTEM_PASTEBOARD_CLEAR.lock().unwrap() != Some(scheduled) {
        log_debug!("[clip] system pasteboard clear skipped: stale task");
        return;
    }
    if !clear_system_pasteboard_after_paste() {
        *PENDING_SYSTEM_PASTEBOARD_CLEAR.lock().unwrap() = None;
        log_debug!("[clip] system pasteboard clear skipped: setting disabled");
        return;
    }
    unsafe {
        let pb: *mut AnyObject = msg_send![class!(NSPasteboard), generalPasteboard];
        let current: i64 = if pb.is_null() {
            -1
        } else {
            msg_send![pb, changeCount]
        };
        let marker_present = !pb.is_null() && pasteboard_has_paste_marker();
        if pb.is_null() || !marker_present || current != scheduled {
            *PENDING_SYSTEM_PASTEBOARD_CLEAR.lock().unwrap() = None;
            log_debug!("[clip] system pasteboard clear skipped: ownership changed");
            return;
        }
        let _: isize = msg_send![pb, clearContents];
    }
    *PENDING_SYSTEM_PASTEBOARD_CLEAR.lock().unwrap() = None;
    log_debug!("[clip] system pasteboard cleared after one-shot paste");
}

/// 在主线程安排延迟清空,并记录写回后的 changeCount 作为所有权凭据。
/// Schedule delayed cleanup on the main thread and record the post-write changeCount as the
/// ownership proof.
pub(super) unsafe fn schedule_system_pasteboard_clear() {
    let pb: *mut AnyObject = msg_send![class!(NSPasteboard), generalPasteboard];
    if pb.is_null() {
        return;
    }
    let cc: i64 = msg_send![pb, changeCount];
    *PENDING_SYSTEM_PASTEBOARD_CLEAR.lock().unwrap() = Some(cc);
    let target = observer();
    let token: *mut AnyObject = msg_send![class!(NSNumber), numberWithLongLong: cc];
    let _: () = msg_send![
        target,
        performSelector: sel!(clearSystemPasteboardIfOwned:),
        withObject: token,
        afterDelay: SYSTEM_PASTEBOARD_CLEAR_DELAY
    ];
}

/// 浮窗失去 key 通知回调(主线程):点击外部等场景自动隐藏。
/// Picker resign-key notification callback (main thread): auto-hide on outside clicks, etc.
extern "C" fn window_did_resign_key(_self: *mut c_void, _cmd: Sel, _note: *mut c_void) {
    hide_picker();
}

pub(super) extern "C" fn clipboard_window_send_event(
    _self: *mut c_void,
    _cmd: Sel,
    event: *mut AnyObject,
) {
    unsafe {
        let window = _self as *mut AnyObject;
        type SendEvent = unsafe extern "C" fn(*mut ObjcSuper, Sel, *mut AnyObject);
        let mut sup = ObjcSuper {
            receiver: window as *mut c_void,
            super_class: class!(NSPanel) as *const _ as *mut c_void,
        };
        let send_event: SendEvent = std::mem::transmute(objc_msgSendSuper as *const ());
        send_event(&mut sup, sel!(sendEvent:), event);
    }
}

/// 主列表和详情共用同一个自定义指示器类;只通过目标滚动视图区分状态。
/// The picker and detail share one custom indicator class; only the target scroll view differs.
pub(super) unsafe fn scroll_indicator_class() -> *mut AnyObject {
    static CLASS: OnceLock<usize> = OnceLock::new();
    *CLASS.get_or_init(|| {
        let name = CString::new("OhMyTabClipboardScrollIndicator").unwrap();
        let superclass = class!(NSView) as *const _ as *mut AnyObject;
        let cls = objc_allocateClassPair(superclass, name.as_ptr(), 0);
        let types_mouse = CString::new("v@:@").unwrap();
        class_addMethod(
            cls,
            sel!(mouseDown:),
            scroll_indicator_mouse_down as *mut c_void,
            types_mouse.as_ptr(),
        );
        class_addMethod(
            cls,
            sel!(mouseDragged:),
            scroll_indicator_mouse_dragged as *mut c_void,
            types_mouse.as_ptr(),
        );
        class_addMethod(
            cls,
            sel!(mouseUp:),
            scroll_indicator_mouse_up as *mut c_void,
            types_mouse.as_ptr(),
        );
        let types_accepts_first_mouse = CString::new("B@:@").unwrap();
        class_addMethod(
            cls,
            sel!(acceptsFirstMouse:),
            scroll_indicator_accepts_first_mouse as *mut c_void,
            types_accepts_first_mouse.as_ptr(),
        );
        objc_registerClassPair(cls);
        cls as usize
    }) as *mut AnyObject
}

fn scroll_target_for_indicator(indicator: *mut AnyObject) -> Option<ScrollTarget> {
    if DETAIL_SCROLL_INDICATOR
        .lock()
        .unwrap()
        .is_some_and(|detail| detail.0 == indicator)
    {
        return Some(ScrollTarget::Detail);
    }
    if DETAIL_HORIZONTAL_SCROLL_INDICATOR
        .lock()
        .unwrap()
        .is_some_and(|detail| detail.0 == indicator)
    {
        return Some(ScrollTarget::DetailHorizontal);
    }
    if SCROLL_INDICATOR
        .lock()
        .unwrap()
        .is_some_and(|picker| picker.0 == indicator)
    {
        Some(ScrollTarget::Picker)
    } else {
        None
    }
}

unsafe fn scroll_for_target(target: ScrollTarget) -> Option<*mut AnyObject> {
    match target {
        ScrollTarget::Picker => SCROLL_VIEW.lock().unwrap().map(|scroll| scroll.0),
        ScrollTarget::Detail | ScrollTarget::DetailHorizontal => {
            DETAIL_SCROLL_VIEW.lock().unwrap().map(|scroll| scroll.0)
        }
    }
}

/// 计算指示器的 y/高度;拖拽和绘制必须使用同一套映射,否则拖到轨道底部时会跳动。
/// Compute the indicator's y/height; dragging and drawing must share this mapping or the
/// thumb jumps when it reaches the end of the track.
pub(super) fn scroll_indicator_geometry(
    visible: f64,
    document: f64,
    offset: f64,
    corner_reserve: f64,
) -> Option<(f64, f64)> {
    let track_start = SCROLL_INDICATOR_EDGE;
    let track_end = visible - SCROLL_INDICATOR_EDGE - corner_reserve;
    if track_end <= track_start || document <= visible {
        return None;
    }
    // 轨道末端统一避开右下角安全区;绘制和拖拽必须使用同一轨道映射。
    // Keep the track end outside the lower-right safe corner; drawing and dragging must use
    // this same track mapping.
    let track_len = track_end - track_start;
    let knob_len = (visible * visible / document)
        .max(SCROLL_INDICATOR_MIN_LEN)
        .min(track_len);
    let max_offset = document - visible;
    let travel = track_len - knob_len;
    let progress = if max_offset > 0.0 {
        (offset / max_offset).clamp(0.0, 1.0)
    } else {
        0.0
    };
    Some((track_start + progress * travel, knob_len))
}

/// 在 10pt 透明命中视图中绘制居中的 6pt 可见胶囊;父视图仍负责接收拖拽事件。
/// Draw a centered 6pt visible capsule inside the 10pt transparent hit view; the parent view
/// remains responsible for receiving drag events.
pub(super) unsafe fn update_scroll_indicator_visual(
    indicator: *mut AnyObject,
    length: f64,
    horizontal: bool,
) {
    let _: () = msg_send![indicator, setWantsLayer: true];
    let parent_layer: *mut AnyObject = msg_send![indicator, layer];
    let sublayers: *mut AnyObject = msg_send![parent_layer, sublayers];
    let count: usize = if sublayers.is_null() {
        0
    } else {
        msg_send![sublayers, count]
    };
    let visual_layer: *mut AnyObject = if count == 0 {
        let visual: *mut AnyObject = msg_send![class!(CALayer), layer];
        let ind_bg: *mut AnyObject =
            msg_send![class!(NSColor), colorWithWhite: 0.0f64, alpha: 0.35f64];
        crate::ffi::layer_set_background(visual, crate::ffi::ns_color_to_cg(ind_bg));
        let _: () = msg_send![visual, setCornerRadius: SCROLL_INDICATOR_R];
        let _: () = msg_send![parent_layer, addSublayer: visual];
        visual
    } else {
        msg_send![sublayers, objectAtIndex: 0usize]
    };
    let frame = if horizontal {
        NSRect::new(
            NSPoint::new(0.0, (SCROLL_INDICATOR_HIT_W - SCROLL_INDICATOR_W) / 2.0),
            NSSize::new(length, SCROLL_INDICATOR_W),
        )
    } else {
        NSRect::new(
            NSPoint::new((SCROLL_INDICATOR_HIT_W - SCROLL_INDICATOR_W) / 2.0, 0.0),
            NSSize::new(SCROLL_INDICATOR_W, length),
        )
    };
    let _: () = msg_send![visual_layer, setFrame: frame];
}

/// 更新滚动指示器的位置/长度:内容溢出时显示(恒显示,不淡出),否则隐藏。
/// 由 clipView 的 bounds 变化通知回调与 show_picker(首次呼出即显示)调用。
/// Update the scroll indicator's position/length: shown while the content overflows
/// (always visible, no fade-out), hidden otherwise. Called by the clip-view bounds-change
/// notification callback AND by show_picker (visible on the first summon).
pub(super) unsafe fn update_scroll_indicator_for(target: ScrollTarget) {
    let scroll = match scroll_for_target(target) {
        Some(scroll) => scroll,
        None => return,
    };
    let (indicator, horizontal) = match target {
        ScrollTarget::Picker => (
            match *SCROLL_INDICATOR.lock().unwrap() {
                Some(indicator) => indicator.0,
                None => return,
            },
            false,
        ),
        ScrollTarget::Detail => (
            match *DETAIL_SCROLL_INDICATOR.lock().unwrap() {
                Some(indicator) => indicator.0,
                None => return,
            },
            false,
        ),
        ScrollTarget::DetailHorizontal => (
            match *DETAIL_HORIZONTAL_SCROLL_INDICATOR.lock().unwrap() {
                Some(indicator) => indicator.0,
                None => return,
            },
            true,
        ),
    };
    let clip: *mut AnyObject = msg_send![scroll, contentView];
    let clip_bounds: NSRect = msg_send![clip, bounds];
    let visible = if horizontal {
        clip_bounds.size.width
    } else {
        clip_bounds.size.height
    };
    let offset = if horizontal {
        clip_bounds.origin.x
    } else {
        clip_bounds.origin.y
    };
    let doc: *mut AnyObject = msg_send![scroll, documentView];
    let document = if doc.is_null() {
        0.0
    } else {
        let df: NSRect = msg_send![doc, frame];
        if horizontal {
            df.size.width
        } else {
            df.size.height
        }
    };

    let corner_reserve = match target {
        ScrollTarget::Picker => 0.0,
        ScrollTarget::Detail | ScrollTarget::DetailHorizontal => SCROLL_INDICATOR_CORNER_RESERVE,
    };
    let Some((knob_pos, knob_len)) =
        scroll_indicator_geometry(visible, document, offset, corner_reserve)
    else {
        let _: () = msg_send![indicator, setHidden: true];
        return;
    };
    let frame = if horizontal {
        NSRect::new(
            NSPoint::new(
                knob_pos,
                clip_bounds.size.height - SCROLL_INDICATOR_HIT_W - 3.0,
            ),
            NSSize::new(knob_len, SCROLL_INDICATOR_HIT_W),
        )
    } else {
        NSRect::new(
            NSPoint::new(
                clip_bounds.size.width - SCROLL_INDICATOR_HIT_W - 3.0,
                knob_pos,
            ),
            NSSize::new(SCROLL_INDICATOR_HIT_W, knob_len),
        )
    };
    let _: () = msg_send![indicator, setFrame: frame];
    update_scroll_indicator_visual(indicator, knob_len, horizontal);
    let _: () = msg_send![indicator, setHidden: false];
}

pub(super) fn update_scroll_indicator() {
    unsafe { update_scroll_indicator_for(ScrollTarget::Picker) }
}

/// 允许自定义指示器接收第一次鼠标点击;非激活面板也要能直接开始拖拽。
/// Accept the first mouse click so the nonactivating panel can start dragging immediately.
extern "C" fn scroll_indicator_accepts_first_mouse(
    _self: *mut c_void,
    _cmd: Sel,
    _event: *mut c_void,
) -> bool {
    true
}

/// 按指示器拖动距离换算文档滚动偏移。系统滚动条已关闭,NSView 不会自动提供这套行为。
/// Convert thumb movement into document offset. The system scroller is disabled, so NSView
/// does not provide this behavior automatically.
extern "C" fn scroll_indicator_mouse_down(_self: *mut c_void, _cmd: Sel, event: *mut c_void) {
    unsafe {
        let Some(target) = scroll_target_for_indicator(_self as *mut AnyObject) else {
            return;
        };
        let scroll = match scroll_for_target(target) {
            Some(scroll) => scroll,
            None => return,
        };
        let clip: *mut AnyObject = msg_send![scroll, contentView];
        let clip_bounds: NSRect = msg_send![clip, bounds];
        let doc: *mut AnyObject = msg_send![scroll, documentView];
        if doc.is_null() {
            return;
        }
        let doc_frame: NSRect = msg_send![doc, frame];
        let horizontal = matches!(target, ScrollTarget::DetailHorizontal);
        let visible = if horizontal {
            clip_bounds.size.width
        } else {
            clip_bounds.size.height
        };
        let document = if horizontal {
            doc_frame.size.width
        } else {
            doc_frame.size.height
        };
        let offset = if horizontal {
            clip_bounds.origin.x
        } else {
            clip_bounds.origin.y
        };
        let corner_reserve = match target {
            ScrollTarget::Picker => 0.0,
            ScrollTarget::Detail | ScrollTarget::DetailHorizontal => {
                SCROLL_INDICATOR_CORNER_RESERVE
            }
        };
        let Some((_, knob_len)) =
            scroll_indicator_geometry(visible, document, offset, corner_reserve)
        else {
            return;
        };
        let track_len = visible - (SCROLL_INDICATOR_EDGE * 2.0) - corner_reserve;
        let thumb_travel = track_len - knob_len;
        if thumb_travel <= 0.0 {
            return;
        }
        let location: NSPoint = msg_send![event as *mut AnyObject, locationInWindow];
        let point: NSPoint = msg_send![
            scroll,
            convertPoint: location,
            fromView: std::ptr::null::<AnyObject>()
        ];
        let max_offset = (document - visible).max(0.0);
        *SCROLL_DRAG.lock().unwrap() = Some(ScrollDragState {
            target,
            start_axis: if horizontal { point.x } else { point.y },
            start_offset: offset.clamp(0.0, max_offset),
            max_offset,
            thumb_travel,
        });
    }
}

extern "C" fn scroll_indicator_mouse_dragged(_self: *mut c_void, _cmd: Sel, event: *mut c_void) {
    unsafe {
        let drag = match *SCROLL_DRAG.lock().unwrap() {
            Some(drag) => drag,
            None => return,
        };
        let scroll = match scroll_for_target(drag.target) {
            Some(scroll) => scroll,
            None => return,
        };
        let location: NSPoint = msg_send![event as *mut AnyObject, locationInWindow];
        let point: NSPoint = msg_send![
            scroll,
            convertPoint: location,
            fromView: std::ptr::null::<AnyObject>()
        ];
        let horizontal = matches!(drag.target, ScrollTarget::DetailHorizontal);
        let axis = if horizontal { point.x } else { point.y };
        let offset = (drag.start_offset
            + (axis - drag.start_axis) * drag.max_offset / drag.thumb_travel)
            .clamp(0.0, drag.max_offset);
        let clip: *mut AnyObject = msg_send![scroll, contentView];
        let bounds: NSRect = msg_send![clip, bounds];
        let origin = if horizontal {
            NSPoint::new(offset, bounds.origin.y)
        } else {
            NSPoint::new(bounds.origin.x, offset)
        };
        let _: () = msg_send![clip, setBoundsOrigin: origin];
        let _: () = msg_send![scroll, reflectScrolledClipView: clip];
    }
}

extern "C" fn scroll_indicator_mouse_up(_self: *mut c_void, _cmd: Sel, _event: *mut c_void) {
    *SCROLL_DRAG.lock().unwrap() = None;
}

/// clipView bounds 变化通知回调(滚动发生)→ 更新指示器;详情打开时同步移动详情,
/// 让它跟着选中行走(否则滚动后详情与行错位)。
/// Clip-view bounds-change notification callback (scrolling) -> update the indicator; with
/// the detail open, move it along so it keeps following the selected row (otherwise a
/// scroll would leave the detail misaligned with its row).
/// C 回调的 panic 边界:panic 穿不过 extern "C" 帧(会 abort 整个进程),这里统一接住。
/// Panic boundary for the C callback: a panic cannot unwind through an `extern "C"` frame (it
/// aborts the process), so it is contained here.
extern "C" fn scroll_indicator_bounds_changed(_self: *mut c_void, _cmd: Sel, _note: *mut c_void) {
    crate::callback_guard::void("scroll_indicator_bounds_changed", || unsafe {
        scroll_indicator_bounds_changed_inner(_self, _cmd, _note)
    });
}

unsafe fn scroll_indicator_bounds_changed_inner(_self: *mut c_void, _cmd: Sel, _note: *mut c_void) {
    update_scroll_indicator();
    if !REBUILDING.load(Ordering::SeqCst) && PICKER_VISIBLE.load(Ordering::SeqCst) {
        unsafe {
            if picker_materialized_range_changed() {
                schedule_picker_visible_rows_refresh();
            }
        }
    }
    reposition_detail();
}

/// 判断一行是否与可视区(含 overscan)相交。
/// Check whether a row intersects the viewport, including overscan.
pub(super) fn picker_row_is_drawable(row: NSRect, viewport: NSRect, overscan: f64) -> bool {
    row.origin.y + row.size.height >= viewport.origin.y - overscan
        && row.origin.y <= viewport.origin.y + viewport.size.height + overscan
}

/// 根据完整行高计算需要物化的显示索引范围,保留少量上下缓冲以避免滚动边界闪烁。
/// Compute the materialized display-index range from all row heights, retaining a small
/// overscan on both sides to avoid flashing at scroll boundaries.
pub(super) fn picker_visible_range(
    pitches: &[f64],
    viewport: NSRect,
    overscan: f64,
) -> (usize, usize) {
    if pitches.is_empty() || viewport.size.height <= 0.0 {
        return (0, 0);
    }
    // 前缀和一次性算出各行顶边:循环里逐行调 `row_top` 会重新累加前面的行高,
    // 整段退化成 O(n²)(与 rebuild_rows 同一个坑,见 model::row_offsets)。
    // One-pass prefix sums give every row top: calling `row_top` per row re-sums the
    // preceding pitches, degrading the scan to O(n^2) (the same trap rebuild_rows fixed;
    // see model::row_offsets).
    let offsets = row_offsets(pitches);
    let mut start = None;
    let mut end = 0;
    for (index, &height) in pitches.iter().enumerate() {
        let row = NSRect::new(
            NSPoint::new(0.0, offsets[index]),
            NSSize::new(PICKER_W, height),
        );
        if picker_row_is_drawable(row, viewport, overscan) {
            start.get_or_insert(index);
            end = index + 1;
        }
    }
    match start {
        Some(start) => (start, end),
        None => (0, pitches.len().min(1)),
    }
}

/// 取当前列表视口;布局尚未完成时只预热顶部少量行,避免首开一次性创建整表。
/// Read the current list viewport; before layout completes, warm only a small top slice so
/// the first presentation never creates the entire list synchronously.
pub(super) unsafe fn picker_visible_row_range(pitches: &[f64], row_count: usize) -> (usize, usize) {
    if row_count == 0 {
        return (0, 0);
    }
    let fallback_end = row_count.min(12);
    let Some(container) = picker_container_ptr() else {
        return (0, fallback_end);
    };
    let Some(scroll) = *SCROLL_VIEW.lock().unwrap() else {
        return (0, fallback_end);
    };
    let clip: *mut AnyObject = msg_send![scroll.0, contentView];
    if clip.is_null() {
        return (0, fallback_end);
    }
    let clip_bounds: NSRect = msg_send![clip, bounds];
    let visible_rect: NSRect = msg_send![
        container,
        convertRect: clip_bounds,
        fromView: clip
    ];
    if visible_rect.size.width <= 0.0 || visible_rect.size.height <= 0.0 {
        return (0, fallback_end);
    }
    picker_visible_range(pitches, visible_rect, ROW_H * 1.5)
}

/// 判断当前物理行槽位是否覆盖视口所需范围。
/// Check whether the current physical row slots cover the range needed by the viewport.
pub(super) unsafe fn picker_materialized_range_changed() -> bool {
    let pitches = ROW_PITCHES.lock().unwrap().clone();
    let filtered_len = with_clipboard_ui(|ui| ui.filtered.len());
    let (start, end) = picker_visible_row_range(&pitches, filtered_len);
    let indices = ROW_VIEW_INDICES.lock().unwrap();
    !indices.iter().copied().eq(start..end)
}

pub(super) fn row_view_for_display_index(index: usize) -> Option<RowHoverViews> {
    let indices = ROW_VIEW_INDICES.lock().unwrap();
    let slot = indices
        .iter()
        .position(|&display_index| display_index == index)?;
    ROW_HOVER_VIEWS.lock().unwrap().get(slot).copied()
}

/// 返回详情 NSClipView 的实时合法纵向范围。NSTextView 的 textContainerInset 会让
/// 顶部/底部不一定等于 `0..documentHeight-visibleHeight`,必须交给 AppKit 约束。
/// Return the detail NSClipView's live legal vertical range. NSTextView's text-container inset
/// means the endpoints are not necessarily `0..documentHeight-visibleHeight`; AppKit must
/// constrain them.
pub(super) unsafe fn detail_scroll_range(scroll: *mut AnyObject) -> Option<(f64, f64)> {
    if scroll.is_null() {
        return None;
    }
    let clip: *mut AnyObject = msg_send![scroll, contentView];
    if clip.is_null() {
        return None;
    }
    let bounds: NSRect = msg_send![clip, bounds];
    let top_request = NSRect::new(NSPoint::new(bounds.origin.x, -1_000_000_000.0), bounds.size);
    let bottom_request = NSRect::new(NSPoint::new(bounds.origin.x, 1_000_000_000.0), bounds.size);
    let top: NSRect = msg_send![clip, constrainBoundsRect: top_request];
    let bottom: NSRect = msg_send![clip, constrainBoundsRect: bottom_request];
    Some((
        top.origin.y.min(bottom.origin.y),
        top.origin.y.max(bottom.origin.y),
    ))
}

/// 无条件滚到 AppKit 计算出的真实顶部,而不是假设顶部 y=0。
/// Scroll unconditionally to AppKit's actual constrained top instead of assuming y=0.
pub(super) unsafe fn scroll_detail_to_top(scroll: *mut AnyObject) {
    let Some((min_y, _)) = detail_scroll_range(scroll) else {
        return;
    };
    let clip: *mut AnyObject = msg_send![scroll, contentView];
    let bounds: NSRect = msg_send![clip, bounds];
    let _: () = msg_send![
        clip,
        setBoundsOrigin: NSPoint::new(bounds.origin.x, min_y)
    ];
    let _: () = msg_send![scroll, reflectScrolledClipView: clip];
}

/// 详情原生滚动视图的 bounds 变化 → 只更新自定义滚动条胶囊。端点越界(橡皮筋)
/// 属于原生 elasticity 的职责,不应在这里改写 clipView bounds——手势进行中的
/// 同步硬钳会污染 NSScrollView 的动量累加基准,让后续惯性事件从脏基准重新施加
/// delta,两端反复拉锯直到惯性耗尽,屏幕上就是滚动条"抽搐一下"。橡皮筋期间
/// 指示器几何把进度 clamp 到 0..1,滑块自然钉在端点,与系统滚动条表现一致。
/// Bounds changes from the detail's native scroll view -> update the custom capsule
/// indicators only. Endpoint overscroll (rubber banding) is native elasticity's job and
/// must never be answered by rewriting the clip-view bounds here -- a synchronous hard
/// clamp mid-gesture poisons NSScrollView's momentum base, so each following momentum
/// event reapplies its delta from the stale base and the two systems fight until the decay
/// ends (the on-screen scrollbar twitch). During rubber banding the indicator geometry
/// clamps progress to 0..1, so the thumb pins at the endpoint just like a system scroller.
pub(super) extern "C" fn detail_scroll_indicator_bounds_changed(
    _self: *mut c_void,
    _cmd: Sel,
    _note: *mut c_void,
) {
    // update_scroll_indicator_for 内部自取 DETAIL_SCROLL_VIEW,视图不存在时会静默返回。
    // update_scroll_indicator_for reads DETAIL_SCROLL_VIEW itself and returns silently
    // when the view is gone.
    unsafe {
        update_scroll_indicator_for(ScrollTarget::Detail);
        update_scroll_indicator_for(ScrollTarget::DetailHorizontal);
    }
}
fn cancel_clipboard_undo_timer() {
    unsafe {
        let target = observer();
        let _: () = msg_send![
            class!(NSObject),
            cancelPreviousPerformRequestsWithTarget: target,
            selector: sel!(expireClipboardUndo:),
            object: std::ptr::null::<AnyObject>()
        ];
    }
}

fn discard_deleted_clipboard_entry() {
    let pending = DELETED_CLIPBOARD_ENTRY.lock().unwrap().take();
    let Some(pending) = pending else {
        return;
    };
    let history = CLIP_HISTORY.lock().unwrap();
    cache_delete_for_removed(&history, &pending.entry);
}

fn schedule_clipboard_undo_expiry() {
    cancel_clipboard_undo_timer();
    unsafe {
        let target = observer();
        let _: () = msg_send![
            target,
            performSelector: sel!(expireClipboardUndo:),
            withObject: std::ptr::null::<AnyObject>(),
            afterDelay: CLIPBOARD_UNDO_WINDOW.as_secs_f64()
        ];
    }
}

pub(super) fn remember_deleted_clipboard_entry(entry: ClipEntry, original_index: usize) {
    discard_deleted_clipboard_entry();
    let generation = DELETED_CLIPBOARD_GENERATION.fetch_add(1, Ordering::SeqCst) + 1;
    *DELETED_CLIPBOARD_ENTRY.lock().unwrap() = Some(DeletedClipboardEntry {
        entry,
        original_index,
        expires_at: Instant::now() + CLIPBOARD_UNDO_WINDOW,
        generation,
    });
    schedule_clipboard_undo_expiry();
}

fn expire_deleted_clipboard_entry() {
    let generation = DELETED_CLIPBOARD_GENERATION.load(Ordering::Acquire);
    let expired = DELETED_CLIPBOARD_ENTRY
        .lock()
        .unwrap()
        .as_ref()
        .is_some_and(|pending| {
            pending.generation == generation
                && clipboard_undo_expired(pending.expires_at, Instant::now())
        });
    if expired {
        discard_deleted_clipboard_entry();
        DELETED_CLIPBOARD_GENERATION.fetch_add(1, Ordering::SeqCst);
    }
}

extern "C" fn expire_clipboard_undo(_self: *mut c_void, _cmd: Sel, _arg: *mut c_void) {
    expire_deleted_clipboard_entry();
}

pub(super) fn undo_deleted_clipboard_entry() -> Option<ClipEntry> {
    let pending = DELETED_CLIPBOARD_ENTRY.lock().unwrap().take()?;
    cancel_clipboard_undo_timer();
    if clipboard_undo_expired(pending.expires_at, Instant::now()) {
        let history = CLIP_HISTORY.lock().unwrap();
        cache_delete_for_removed(&history, &pending.entry);
        return None;
    }
    let restored = pending.entry.clone();
    let mut history = CLIP_HISTORY.lock().unwrap();
    let (_, inserted) = restore_entry_at(&mut history, pending.entry, pending.original_index);
    let max = max_entries();
    if history.len() > max {
        let dropped: Vec<ClipEntry> = history.drain(max..).collect();
        for entry in &dropped {
            cache_delete_for_removed(&history, entry);
        }
    }
    let restored_h_idx = history
        .iter()
        .position(|entry| same_clip_entry_identity(entry, &restored));
    let present = restored_h_idx.is_some();
    if let Some(h_idx) = restored_h_idx {
        let query = with_clipboard_ui(|ui| ui.search_query.clone());
        let filter = *CLIP_FILTER.lock().unwrap();
        let filtered = filtered_indices(&history, &query, filter);
        if let Some(display_idx) = filtered.iter().position(|&idx| idx == h_idx) {
            set_picker_selection(display_idx);
        }
    }
    drop(history);
    DELETED_CLIPBOARD_GENERATION.fetch_add(1, Ordering::SeqCst);
    if inserted || present {
        save_history();
    }
    present.then_some(restored)
}

pub(super) fn picker_filters_y() -> f64 {
    TOP_PAD_Y + SEARCH_H + SEARCH_GAP_Y
}

pub(super) fn clear_history_confirmation_layout(anchor: NSRect) -> (NSRect, [NSRect; 2]) {
    let labels = [
        t("clipboard.clear_confirm_unpinned"),
        t("clipboard.clear_confirm_all"),
    ];
    let button_widths = labels.map(|label| {
        localized_string_width(&label, CLEAR_CONFIRM_BUTTON_FONT_SIZE)
            + CLEAR_CONFIRM_BUTTON_PAD_X * 2.0
    });
    let card_width =
        CLEAR_CONFIRM_CARD_PAD_X * 2.0 + button_widths.iter().sum::<f64>() + CLEAR_CONFIRM_GAP;
    let surface = NSRect::new(
        NSPoint::new(
            anchor.origin.x + anchor.size.width - card_width,
            anchor.origin.y,
        ),
        NSSize::new(card_width, CLEAR_CONFIRM_CARD_H),
    );
    let buttons = std::array::from_fn(|index| {
        let x = CLEAR_CONFIRM_CARD_PAD_X
            + button_widths[..index].iter().sum::<f64>()
            + index as f64 * CLEAR_CONFIRM_GAP;
        NSRect::new(
            NSPoint::new(x, CLEAR_CONFIRM_CARD_PAD_Y),
            NSSize::new(button_widths[index], CLEAR_CONFIRM_BUTTON_H),
        )
    });
    (surface, buttons)
}

unsafe fn clear_confirmation_reduce_motion() -> bool {
    let workspace: *mut AnyObject = msg_send![class!(NSWorkspace), sharedWorkspace];
    !workspace.is_null()
        && msg_send![workspace, respondsToSelector: sel!(accessibilityDisplayShouldReduceMotion)]
        && msg_send![workspace, accessibilityDisplayShouldReduceMotion]
}

#[allow(clippy::too_many_arguments)]
unsafe fn clear_confirmation_spring_value(
    layer: *mut AnyObject,
    key_path: &str,
    from: *mut AnyObject,
    to: *mut AnyObject,
    duration: f64,
    stiffness: f64,
    damping: f64,
    animation_key: &str,
) {
    let path = make_nsstring(key_path);
    let animation: *mut AnyObject =
        msg_send![class!(CASpringAnimation), animationWithKeyPath: path];
    CFRelease(path as *const c_void);
    let _: () = msg_send![animation, setFromValue: from];
    let _: () = msg_send![animation, setToValue: to];
    let _: () = msg_send![animation, setMass: 1.0f64];
    let _: () = msg_send![animation, setStiffness: stiffness];
    let _: () = msg_send![animation, setDamping: damping];
    let _: () = msg_send![animation, setInitialVelocity: 0.0f64];
    let _: () = msg_send![animation, setDuration: duration];
    let key = make_nsstring(animation_key);
    let _: () = msg_send![layer, addAnimation: animation, forKey: key];
    CFRelease(key as *const c_void);
}

unsafe fn clear_confirmation_spring_frame(view: *mut AnyObject, target: NSRect) {
    let layer: *mut AnyObject = msg_send![view, layer];
    if layer.is_null() {
        let _: () = msg_send![view, setFrame: target];
        return;
    }
    let presentation: *mut AnyObject = msg_send![layer, presentationLayer];
    let current = if presentation.is_null() {
        layer
    } else {
        presentation
    };
    let from_bounds: NSRect = msg_send![current, bounds];
    let from_position: NSPoint = msg_send![current, position];
    let _: () = msg_send![class!(CATransaction), begin];
    let _: () = msg_send![class!(CATransaction), setDisableActions: true];
    let _: () = msg_send![view, setFrame: target];
    let _: () = msg_send![class!(CATransaction), commit];
    let to_bounds: NSRect = msg_send![layer, bounds];
    let to_position: NSPoint = msg_send![layer, position];
    for (path, from, to, key) in [
        (
            "bounds",
            msg_send![class!(NSValue), valueWithRect: from_bounds],
            msg_send![class!(NSValue), valueWithRect: to_bounds],
            "clipboard-clear-shell-bounds",
        ),
        (
            "position",
            msg_send![class!(NSValue), valueWithPoint: from_position],
            msg_send![class!(NSValue), valueWithPoint: to_position],
            "clipboard-clear-shell-position",
        ),
    ] {
        clear_confirmation_spring_value(
            layer,
            path,
            from,
            to,
            CLEAR_CONFIRM_SHELL_DURATION,
            160.0,
            24.0,
            key,
        );
    }
}

unsafe fn clear_confirmation_animate_opacity(view: *mut AnyObject, visible: bool) {
    let layer: *mut AnyObject = msg_send![view, layer];
    let target = if visible { 1.0 } else { 0.0 };
    if layer.is_null() {
        let _: () = msg_send![view, setAlphaValue: target];
        return;
    }
    let presentation: *mut AnyObject = msg_send![layer, presentationLayer];
    let from: f64 = if presentation.is_null() {
        msg_send![view, alphaValue]
    } else {
        // CALayer opacity is a CGFloat-compatible Objective-C `float`, not an `f64` return.
        // CALayer 的 opacity 返回类型是 Objective-C `float`，不能按 `f64` 接收。
        let opacity: f32 = msg_send![presentation, opacity];
        opacity as f64
    };
    let _: () = msg_send![class!(CATransaction), begin];
    let _: () = msg_send![class!(CATransaction), setDisableActions: true];
    let _: () = msg_send![view, setAlphaValue: target];
    let _: () = msg_send![class!(CATransaction), commit];
    let from_value: *mut AnyObject = msg_send![class!(NSNumber), numberWithDouble: from];
    let to_value: *mut AnyObject = msg_send![class!(NSNumber), numberWithDouble: target];
    clear_confirmation_spring_value(
        layer,
        "opacity",
        from_value,
        to_value,
        if visible {
            CLEAR_CONFIRM_CONTENT_DURATION
        } else {
            0.16
        },
        if visible { 266.0 } else { 350.0 },
        if visible { 30.0 } else { 36.0 },
        "clipboard-clear-opacity",
    );
}

unsafe fn clear_confirmation_animate_content_open(button: *mut AnyObject) {
    clear_confirmation_animate_opacity(button, true);
    let layer: *mut AnyObject = msg_send![button, layer];
    if layer.is_null() {
        return;
    }
    // 与设置页一致,内容从轻微上移和缩小的状态弹入展开的外壳。
    // Match the settings control's content entrance: a small upward offset and scale
    // settle into the expanding shell.
    for (path, from, to, key) in [
        (
            "transform.translation.y",
            8.0,
            0.0,
            "clipboard-clear-content-y",
        ),
        (
            "transform.scale",
            0.98,
            1.0,
            "clipboard-clear-content-scale",
        ),
    ] {
        let key_path = make_nsstring(path);
        let from_value: *mut AnyObject = msg_send![class!(NSNumber), numberWithDouble: from];
        let to_value: *mut AnyObject = msg_send![class!(NSNumber), numberWithDouble: to];
        let _: () = msg_send![class!(CATransaction), begin];
        let _: () = msg_send![class!(CATransaction), setDisableActions: true];
        let _: () = msg_send![layer, setValue: to_value, forKeyPath: key_path];
        let _: () = msg_send![class!(CATransaction), commit];
        CFRelease(key_path as *const c_void);
        clear_confirmation_spring_value(
            layer,
            path,
            from_value,
            to_value,
            CLEAR_CONFIRM_CONTENT_DURATION,
            266.0,
            30.0,
            key,
        );
    }
}

/// 设置清空确认卡片的展开状态;状态切换只操作已缓存的视图指针,不触发历史变更。
/// Toggle the clear-history confirmation card; this only changes cached views and never
/// mutates clipboard history.
pub(super) fn set_clear_history_confirmation_expanded(expanded: bool) {
    if CLEAR_HISTORY_CONFIRMATION_EXPANDED.swap(expanded, Ordering::SeqCst) == expanded {
        return;
    }
    let views = *CLEAR_HISTORY_CONFIRMATION.lock().unwrap();
    let clear_button = *CLEAR_HISTORY_BUTTON.lock().unwrap();
    let Some(views) = views else {
        return;
    };

    unsafe {
        let parent: *mut AnyObject = msg_send![views.surface.0, superview];
        let Some(clear) = clear_button else {
            return;
        };
        let header: *mut AnyObject = msg_send![clear.0, superview];
        let anchor: NSRect = msg_send![clear.0, frame];
        let (expanded_in_header, _) = clear_history_confirmation_layout(anchor);
        let expanded_frame: NSRect =
            msg_send![header, convertRect: expanded_in_header, toView: parent];
        let collapsed_frame: NSRect = msg_send![header, convertRect: anchor, toView: parent];
        let animated = !clear_confirmation_reduce_motion();
        let target = observer();
        let _: () = msg_send![
            class!(NSObject),
            cancelPreviousPerformRequestsWithTarget: target,
            selector: sel!(finishClearHistoryCollapse:),
            object: std::ptr::null::<AnyObject>()
        ];
        if expanded {
            // 卡片与 header 同级,超出 header 的下两行仍能被 AppKit 命中并收到 hover。
            // Keep the card alongside the header so its lower rows remain hit-testable beyond
            // the header's bounds. Bring the card above the scroll view when it opens.
            let _: () = msg_send![
                parent,
                addSubview: views.surface.0,
                positioned: 1isize,
                relativeTo: std::ptr::null::<AnyObject>()
            ];
            let hidden: bool = msg_send![views.surface.0, isHidden];
            if hidden {
                let layer: *mut AnyObject = msg_send![views.surface.0, layer];
                if !layer.is_null() {
                    let _: () = msg_send![layer, removeAllAnimations];
                }
                let _: () = msg_send![class!(CATransaction), begin];
                let _: () = msg_send![class!(CATransaction), setDisableActions: true];
                let _: () = msg_send![views.surface.0, setFrame: collapsed_frame];
                let _: () = msg_send![class!(CATransaction), commit];
            }
            let _: () = msg_send![views.surface.0, setHidden: false];
            let _: () = msg_send![clear.0, setHidden: true];
        } else {
            // 收起时让入口和筛选 tab 位于正在缩小的卡片上层,避免透明层吞掉点击。
            // Keep the trigger and filters above the collapsing card so its fading shell
            // cannot intercept the next click.
            let _: () = msg_send![clear.0, setHidden: false];
            let _: () = msg_send![
                parent,
                addSubview: header,
                positioned: 1isize,
                relativeTo: std::ptr::null::<AnyObject>()
            ];
        }
        for button in [views.unpinned.0, views.all.0] {
            let _: () = msg_send![button, setHidden: false];
            if !animated {
                let layer: *mut AnyObject = msg_send![button, layer];
                if !layer.is_null() {
                    let _: () = msg_send![layer, removeAllAnimations];
                }
                let _: () = msg_send![button, setAlphaValue: if expanded { 1.0 } else { 0.0 }];
            } else if expanded {
                clear_confirmation_animate_content_open(button);
            } else {
                clear_confirmation_animate_opacity(button, false);
            }
        }
        if animated {
            clear_confirmation_spring_frame(
                views.surface.0,
                if expanded {
                    expanded_frame
                } else {
                    collapsed_frame
                },
            );
        } else {
            let _: () = msg_send![views.surface.0, setFrame: if expanded { expanded_frame } else { collapsed_frame }];
        }
        if expanded {
            let _: () = msg_send![views.surface.0, setAlphaValue: 1.0f64];
        } else if animated {
            let _: () = msg_send![
                target,
                performSelector: sel!(finishClearHistoryCollapse:),
                withObject: std::ptr::null::<AnyObject>(),
                afterDelay: CLEAR_CONFIRM_SHELL_DURATION
            ];
        } else {
            let _: () = msg_send![views.surface.0, setHidden: true];
        }
    }
}

pub(super) fn clear_history_confirmation_expanded() -> bool {
    CLEAR_HISTORY_CONFIRMATION_EXPANDED.load(Ordering::SeqCst)
}

extern "C" fn finish_clear_history_collapse(_self: *mut c_void, _cmd: Sel, _sender: *mut c_void) {
    if clear_history_confirmation_expanded() {
        return;
    }
    let views = *CLEAR_HISTORY_CONFIRMATION.lock().unwrap();
    let Some(views) = views else {
        return;
    };
    unsafe {
        let _: () = msg_send![views.surface.0, setHidden: true];
        for button in [views.unpinned.0, views.all.0] {
            let _: () = msg_send![button, setHidden: true];
        }
    }
}

fn clear_clipboard_history_scope(clear_all: bool) {
    discard_deleted_clipboard_entry();
    cancel_clipboard_undo_timer();
    DELETED_CLIPBOARD_GENERATION.fetch_add(1, Ordering::SeqCst);
    let mut history = CLIP_HISTORY.lock().unwrap();
    let removed_entries = remove_history_scope(&mut history, clear_all);
    let removed_hashes: HashSet<u64> = removed_entries
        .iter()
        .filter_map(|entry| entry.image.as_ref().map(|image| image.hash))
        .filter(|hash| *hash != 0)
        .collect();
    for hash in removed_hashes {
        cache_delete_for_hash(&history, hash);
    }
    let kept_count = history.len();
    drop(history);
    save_history();
    clear_search();
    hide_detail();
    unsafe { rebuild_rows() };
    log_info!(
        "Clipboard history cleared by user (clear_all={}, kept_entries={})",
        clear_all,
        kept_count
    );
}

/// 点击清空入口只展开确认卡片,不改变历史。
/// Clicking the clear entry point only expands the confirmation card; history is untouched.
extern "C" fn clear_clipboard_history(_self: *mut c_void, _cmd: Sel, _sender: *mut c_void) {
    set_clear_history_confirmation_expanded(true);
}

extern "C" fn clear_clipboard_unpinned(_self: *mut c_void, _cmd: Sel, _sender: *mut c_void) {
    set_clear_history_confirmation_expanded(false);
    clear_clipboard_history_scope(false);
}

extern "C" fn clear_clipboard_all(_self: *mut c_void, _cmd: Sel, _sender: *mut c_void) {
    set_clear_history_confirmation_expanded(false);
    clear_clipboard_history_scope(true);
}

/// 清空搜索词 + 搜索框文本(不重建;调用方按需 rebuild)。
/// Clear the search query and the search field's text (no rebuild; callers rebuild as needed).
unsafe fn set_search_clear_button_visible(visible: bool) {
    if let Some(button) = *SEARCH_CLEAR_BUTTON.lock().unwrap() {
        let _: () = msg_send![button.0, setHidden: !visible];
    }
}

pub(super) fn clear_search() {
    with_clipboard_ui(|ui| ui.search_query.clear());
    SEARCH_CLEAR_HOVERED.store(false, Ordering::SeqCst);
    unsafe { set_search_clear_button_visible(false) };
    if let Some(f) = *SEARCH_FIELD.lock().unwrap() {
        unsafe {
            let empty_ns = make_nsstring("");
            let _: () = msg_send![f.0, setStringValue: empty_ns];
            let _: () = msg_send![f.0, setNeedsDisplay: true];
            CFRelease(empty_ns as *const c_void);
        }
    }
}

/// 搜索框文本变化通知回调:更新搜索词并重建过滤列表。
/// Search-field text-change notification callback: update the query and rebuild the filter.
extern "C" fn search_field_changed(_self: *mut c_void, _cmd: Sel, note: *mut c_void) {
    let field: *mut AnyObject = unsafe { msg_send![note as *mut AnyObject, object] };
    if field.is_null() {
        return;
    }
    let s: *mut AnyObject = unsafe { msg_send![field, stringValue] };
    let q = unsafe { nsstring_to_rust(s) };
    let has_query = !q.is_empty();
    with_clipboard_ui(|ui| ui.search_query = q);
    if !has_query {
        SEARCH_CLEAR_HOVERED.store(false, Ordering::SeqCst);
    }
    unsafe { set_search_clear_button_visible(has_query) };
    // ⌘F 键帽和右侧 × 都由自绘 cell 根据查询状态定位,文本变化时强制重绘。
    // The hand-drawn ⌘F keycap and right × are positioned from query state, so force a redraw
    // whenever text changes.
    unsafe {
        let _: () = msg_send![field, setNeedsDisplay: true];
    }
    // 不重置选中:编辑期间(焦点在搜索框)保持无选中;回列表时(↓)由
    // search_field_do_command 重置为首条。
    // Do NOT reset the selection: while editing (focus in the search field) it stays
    // "no selection"; returning to the list (↓) resets it to the first entry in
    // search_field_do_command.
    unsafe { schedule_picker_search_refresh() };
}

/// NSSearchField 的 Esc(cancelOperation:):有搜索词 → 清空并恢复全列表(方案 A 第一级);
/// 无搜索词 → 关闭浮窗(第二级)。
/// NSSearchField's Esc (cancelOperation:): a query gets cleared and the full list restored
/// (scheme A, level one); with no query the picker closes (level two).
pub(super) extern "C" fn search_field_cancel(_self: *mut c_void, _cmd: Sel) {
    let has_query = with_clipboard_ui(|ui| !ui.search_query.is_empty());
    if has_query {
        clear_search();
        // 焦点仍在搜索框,保持无选中(高光不恢复)。
        // Focus stays in the search field: keep "no selection" (no highlight returns).
        unsafe { rebuild_rows() };
    } else {
        hide_picker();
    }
}

/// 搜索框右侧清除按钮:不走响应链的 cancelOperation:(它可能被字段编辑器截获),直接
/// 清空字段和过滤条件。与 Esc 不同,空字段点击不会关闭浮窗。
/// The search field's right clear button: do not route through responder-chain cancelOperation:
/// (which the field editor may intercept); clear the field and filter directly. Unlike Esc,
/// clicking an already-empty field never closes the picker.
pub(super) extern "C" fn search_clear_button(_self: *mut c_void, _cmd: Sel, _sender: *mut c_void) {
    clear_search();
    unsafe { rebuild_rows() };
}

/// 搜索框的自绘 × 命中测试:只在有查询时拦截右侧 18pt,其余鼠标事件照常交给父类。
/// Hit-tests the custom search ×: intercept only the rightmost 18pt while queried and forward
/// every other mouse event to the superclass normally.
/// 鼠标位置是否落在搜索框右侧自绘 × 的命中区域。
/// Whether a mouse location falls inside the search field's custom right-side × hit area.
unsafe fn search_clear_contains_event(field: *mut AnyObject, event: *mut c_void) -> bool {
    if !search_has_query() {
        return false;
    }
    let location: NSPoint = msg_send![event as *mut AnyObject, locationInWindow];
    let point: NSPoint =
        msg_send![field, convertPoint: location, fromView: std::ptr::null::<AnyObject>()];
    let bounds: NSRect = msg_send![field, bounds];
    point.x >= bounds.size.width - SEARCH_PAD_IN - SEARCH_CLEAR_W
        && point.x <= bounds.size.width - SEARCH_PAD_IN
        && point.y >= 0.0
        && point.y <= bounds.size.height
}

/// 刷新自绘 × 的悬停状态;状态变化时仅重绘搜索框,不触发过滤或列表重建。
/// Refreshes the custom × hover state; redraws only the search field on changes, never filters
/// or rebuilds the list.
unsafe fn update_search_clear_hover(field: *mut AnyObject, event: *mut c_void) {
    let hovered = search_clear_contains_event(field, event);
    if SEARCH_CLEAR_HOVERED.swap(hovered, Ordering::SeqCst) != hovered {
        let _: () = msg_send![field, setNeedsDisplay: true];
    }
}

pub(super) extern "C" fn search_field_mouse_moved(
    _self: *mut c_void,
    _cmd: Sel,
    event: *mut c_void,
) {
    unsafe { update_search_clear_hover(_self as *mut AnyObject, event) }
}

pub(super) extern "C" fn search_field_mouse_entered(
    _self: *mut c_void,
    _cmd: Sel,
    event: *mut c_void,
) {
    unsafe { update_search_clear_hover(_self as *mut AnyObject, event) }
}

pub(super) extern "C" fn search_field_mouse_exited(
    _self: *mut c_void,
    _cmd: Sel,
    _event: *mut c_void,
) {
    if SEARCH_CLEAR_HOVERED.swap(false, Ordering::SeqCst) {
        unsafe {
            let field = _self as *mut AnyObject;
            let _: () = msg_send![field, setNeedsDisplay: true];
        }
    }
}

pub(super) extern "C" fn search_field_mouse_down(
    _self: *mut c_void,
    _cmd: Sel,
    event: *mut c_void,
) {
    unsafe {
        let field = _self as *mut AnyObject;
        if search_clear_contains_event(field, event) {
            search_clear_button(_self, sel!(clearSearch:), event);
            return;
        }
        type F = unsafe extern "C" fn(*mut ObjcSuper, Sel, *mut c_void) -> ();
        let super_class = class!(NSSearchField) as *const _ as *mut c_void;
        let mut sup = ObjcSuper {
            receiver: _self,
            super_class,
        };
        let f: F = std::mem::transmute(objc_msgSendSuper as *const ());
        f(&mut sup, sel!(mouseDown:), event);
    }
}

/// 搜索框底/描边样式助手(层背景走 raw FFI)。聚焦只加强内描边,保持稳定的磨砂底色。
/// The search field's fill/ring helper (raw FFI for the layer background). Focus strengthens
/// only the inner ring and keeps the frosted fill stable.
pub(super) unsafe fn style_search_field(field: *mut AnyObject, focused: bool) {
    let layer: *mut AnyObject = msg_send![field, layer];
    let palette = clipboard_palette();
    let background = crate::ffi::hex_to_cg_color(palette.field_bg);
    crate::ffi::layer_set_background(layer, background);
    let ring = if focused {
        palette.accent
    } else {
        palette.card_border
    };
    crate::ffi::layer_set_border(layer, crate::ffi::hex_to_cg_color(ring));
}

/// 编辑开始:保持默认 4.5% 磨砂底,仅使用 10% 内描边指示焦点,避免输入时突变白色。
/// Editing begins: keep the default 4.5% frosted fill and use only a 10% inner ring for focus,
/// avoiding a disruptive white transition while typing.
extern "C" fn search_focus_began(_self: *mut c_void, _cmd: Sel, note: *mut c_void) {
    unsafe {
        let field: *mut AnyObject = msg_send![note as *mut AnyObject, object];
        if !field.is_null() {
            style_search_field(field, true);
            // 聚焦态切换必须显式重绘 cell:占位提示由 cell 自绘,聚焦即隐(IME 组合
            // 期间 stringValue 仍为空,若不重绘会与拼音预编辑串叠加)。图层底色变化
            // 不会触发 cell 重绘。
            // Focus transitions must explicitly redraw the cell: the placeholder is
            // cell-drawn and hides on focus (during IME composition stringValue stays
            // empty, so a stale placeholder would sit under the pre-edit pinyin).
            // Layer background changes do not trigger cell redraws.
            let _: () = msg_send![field, setNeedsDisplay: true];
        }
    }
}

/// 编辑结束:还原默认内描边。 / Editing ends: restore the default inner ring.
extern "C" fn search_focus_ended(_self: *mut c_void, _cmd: Sel, note: *mut c_void) {
    unsafe {
        let field: *mut AnyObject = msg_send![note as *mut AnyObject, object];
        if !field.is_null() {
            style_search_field(field, false);
            // 与 search_focus_began 同理:失焦后恢复占位提示需要立即重绘。
            // Same as search_focus_began: restoring the placeholder on blur needs an
            // immediate redraw.
            let _: () = msg_send![field, setNeedsDisplay: true];
        }
    }
}

/// 当前行视图是否已对应当前历史/查询/筛选(与刷新回调同一判定口径)。
/// Whether the materialized rows already match the current history/query/filter (the same
/// predicate used by the refresh callbacks).
fn picker_rows_are_current() -> bool {
    let filter = *CLIP_FILTER.lock().unwrap();
    let show_source = show_source_app();
    let query = with_clipboard_ui(|ui| ui.search_query.clone());
    let key = picker_rows_key(history_revision(), &query, filter, show_source);
    with_clipboard_ui(|ui| {
        ui.rendered_rows
            .as_ref()
            .is_some_and(|current| current == &key)
    })
}

/// 搜索框 delegate 的命令拦截:↓(moveDown:) → 焦点切到列表并选中过滤结果第一条,返回
/// YES 吞掉该命令;其余命令返回 NO 交给字段编辑器正常处理(光标移动/输入等)。
/// Search-field delegate command interception: ↓ (moveDown:) moves focus into the list and
/// selects the first filtered entry, returning YES (consumed); any other command returns NO
/// so the field editor handles it (cursor movement / text input).
///
/// 为什么必须走这里:搜索框开始编辑后第一响应者是窗口的字段编辑器(NSTextView),键盘事件
/// 根本不经过搜索框的 keyDown:;编辑器把 ↓ 翻译成 moveDown: 命令后通过
/// control:textView:doCommandBySelector: 转发给搜索框的 delegate——这是文本控件拦截按键
/// 的官方机制。
/// Why this is necessary: once the search field edits, the FIRST RESPONDER is the window's
/// field editor (an NSTextView) -- key events never reach the search field's keyDown:. The
/// editor translates ↓ into a moveDown: command and forwards it to the field's delegate via
/// control:textView:doCommandBySelector: -- the official way to intercept keys on text controls.
pub(super) extern "C" fn search_field_do_command(
    _self: *mut c_void,
    _cmd: Sel,
    _control: *mut c_void,
    _text_view: *mut c_void,
    command_selector: Sel,
) -> bool {
    if command_selector != sel!(moveDown:) && command_selector != sel!(moveUp:) {
        return false;
    }
    unsafe {
        // 搜索词/过滤结果保留,仅把焦点与选中交给列表。↓ = 最新一条(首行);
        // ↑ = 最久远的一条(显示列表末行,随后滚动到可见)。
        // The query/filter stays; only focus and the selection move to the list.
        // ↓ = the newest entry (first row); ↑ = the oldest (the display list's tail,
        // scrolled into view afterwards).
        let display_len = with_clipboard_ui(|ui| ui.filtered.len());
        let sel = if command_selector == sel!(moveUp:) {
            // 空列表:0(无行可选中,无高光;saturating_sub 防下溢)。
            // Empty list: 0 (no row to select, no highlight; saturating_sub guards).
            display_len.saturating_sub(1)
        } else {
            0
        };
        let previous = picker_selection();
        set_picker_selection(sel);
        // 列表已对应当前查询时只刷新前后两行高光;查询刚变、去抖刷新未落定时仍需一次
        // 完整重建,否则会把新列表的索引套到旧行树上。
        // When the rows already match the query, update only the two rows' highlights; right
        // after a query change (the debounced refresh has not landed yet) a full rebuild is
        // still required, otherwise the new list index is applied to stale rows.
        if picker_rows_are_current() {
            refresh_selection(previous, sel);
        } else {
            rebuild_rows();
        }
        // 详情打开时,其详情按钮的实心图标要跟随新的选中行。
        // With the detail open, its filled action icon must follow the new selection.
        if detail_visible() {
            refresh_detail_action_visuals();
        }
        // ↑ 选中末行时视口还停在顶部:用确定性的偏移计算滚动到选中行可见。
        // With ↑ the tail is selected while the viewport is still at the top: use the
        // deterministic offset calculation to bring the selected row into view.
        if let Some(container) = picker_container_ptr() {
            scroll_selection_into_view(container, sel);
        }
        if let Some(container) = picker_container_ptr() {
            let window = match *PICKER_WINDOW.lock().unwrap() {
                Some(w) => w.0,
                None => return true,
            };
            // makeFirstResponder: 返回 BOOL('B')。
            // makeFirstResponder: returns BOOL ('B').
            let _: bool = msg_send![window, makeFirstResponder: container];
        }
    }
    true
}

/// 是否已注册剪贴板变化通知(幂等,防止 start/stop 反复注册导致重复回调)。
/// Whether the pasteboard-change notification has been registered (idempotent; start/stop
/// cycles must not double-register and duplicate callbacks).
static NOTIFICATION_REGISTERED: AtomicBool = AtomicBool::new(false);

/// 注册剪贴板变化通知(仅一次)。/ Register the pasteboard-change notification (once).
pub(super) unsafe fn register_pasteboard_observer() {
    if NOTIFICATION_REGISTERED.swap(true, Ordering::SeqCst) {
        return;
    }
    let center: *mut AnyObject = msg_send![class!(NSNotificationCenter), defaultCenter];
    let name = make_nsstring("NSPasteboardDidChangeNotification");
    let _: () = msg_send![
        center,
        addObserver: observer(),
        selector: sel!(clipboardPasteboardChanged:),
        name: name,
        object: std::ptr::null::<AnyObject>()
    ];
    CFRelease(name as *const c_void);
    log_debug!("Pasteboard change observer registered.");
}

/// NSTimer 的 target:NSTimer 会向它发 clipPollTick:。动态注册一个轻量类,方法转发到
/// clip_poll_tick。类只注册一次,实例每次 start 新建(+1,随 timer 持有)。
/// The NSTimer target: NSTimer sends clipPollTick: to it. A tiny dynamic class forwards the
/// method to clip_poll_tick; the class is registered once, and an instance is created per start.
pub(super) unsafe fn timer_target() -> *mut AnyObject {
    static TIMER_CLS: OnceLock<StaticClass> = OnceLock::new();
    let cls = *TIMER_CLS.get_or_init(|| {
        let name = CString::new("OhMyTabClipTimerTarget").unwrap();
        let superclass = class!(NSObject) as *const _ as *mut AnyObject;
        let cls = objc_allocateClassPair(superclass, name.as_ptr(), 0);
        let types = CString::new("v@:@").unwrap();
        class_addMethod(
            cls,
            sel!(clipPollTick:),
            clip_poll_tick as *mut c_void,
            types.as_ptr(),
        );
        objc_registerClassPair(cls);
        StaticClass(cls as *const objc2::runtime::AnyClass)
    });
    let obj: *mut AnyObject = msg_send![cls.0 as *const AnyObject, new];
    obj
}
